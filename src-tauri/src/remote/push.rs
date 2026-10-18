//! Phone push approvals: ntfy configuration, bounded one-use grants, and messages.
//!
//! Route integration deliberately lives in `remote::mod`; callers must hold the
//! Remote lock while checking and consuming a grant, and re-check after any await.
use super::auth::{self, PushCfg, Vault};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const TOKEN_TTL: Duration = Duration::from_secs(15 * 60);
const TOMBSTONE: Duration = Duration::from_secs(30 * 60);
const MAX_GRANTS: usize = 4096;
pub const DEFAULT_SERVER: &str = "https://ntfy.sh";

#[derive(Debug, Clone)]
struct Grant {
    session: String,
    r#gen: String,
    task: String,
    item: String,
    expires: Instant,
    used: bool,
}

#[derive(Default)]
pub struct Grants {
    map: HashMap<String, Grant>,
}

impl Grants {
    pub fn mint(
        &mut self,
        session: &str,
        generation: &str,
        task: &str,
        item: &str,
        now: Instant,
    ) -> String {
        self.prune(now);
        let token = auth::secret();
        self.map.insert(
            auth::hash(&token),
            Grant {
                session: session.into(),
                r#gen: generation.into(),
                task: task.into(),
                item: item.into(),
                expires: now + TOKEN_TTL,
                used: false,
            },
        );
        token
    }

    fn prune(&mut self, now: Instant) {
        self.map.retain(|_, g| now < g.expires + TOMBSTONE);
        if self.map.len() >= MAX_GRANTS {
            let mut by_age: Vec<(String, Instant)> = self
                .map
                .iter()
                .map(|(k, g)| (k.clone(), g.expires))
                .collect();
            by_age.sort_by_key(|(_, e)| *e);
            let excess = self.map.len() + 1 - MAX_GRANTS;
            for (k, _) in by_age.into_iter().take(excess) {
                self.map.remove(&k);
            }
        }
    }

    /// Remove a session's grants; `None` clears the entire table.
    pub fn forget_session(&mut self, session: Option<&str>) {
        self.map
            .retain(|_, g| session.is_some_and(|s| g.session != s));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Session(String),
    Token,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    BadToken,
    Revoked,
    Expired,
    Answered(Option<String>),
    ConfirmRequired,
    BadDecision,
}

/// Validate token binding, live session generation and TTL without spending it.
pub fn check(
    v: &Vault,
    grants: &Grants,
    caller: &Caller,
    task: &str,
    item: &str,
    token: &str,
    now: Instant,
) -> Result<PushCfg, Refusal> {
    if token.is_empty() {
        return Err(Refusal::BadToken);
    }
    let g = grants
        .map
        .get(&auth::hash(token))
        .ok_or(Refusal::BadToken)?;
    if g.task != task || g.item != item {
        return Err(Refusal::BadToken);
    }
    if let Caller::Session(s) = caller {
        if *s != g.session {
            return Err(Refusal::BadToken);
        }
    }
    let cfg = v
        .sessions
        .iter()
        .find(|s| s.id == g.session)
        .and_then(|s| s.push.clone())
        .filter(|c| c.r#gen == g.r#gen)
        .ok_or(Refusal::Revoked)?;
    if g.used {
        return Err(Refusal::Answered(None));
    }
    if now >= g.expires {
        return Err(Refusal::Expired);
    }
    Ok(cfg)
}

/// Revalidate and atomically consume while caller holds the vault/grants lock.
/// A successful call records the tombstone so subsequent calls say Answered.
pub fn consume(
    v: &Vault,
    grants: &mut Grants,
    caller: &Caller,
    task: &str,
    item: &str,
    token: &str,
    now: Instant,
) -> Result<PushCfg, Refusal> {
    let cfg = check(v, grants, caller, task, item, token, now)?;
    let key = auth::hash(token);
    match grants.map.get_mut(&key) {
        Some(g) if !g.used => {
            g.used = true;
            Ok(cfg)
        }
        _ => Err(Refusal::Answered(None)),
    }
}

/// Whether a persistent "Always" decision is valid for this approval item.
pub fn always_label(data: &Value) -> Option<String> {
    if data["repeat"].as_bool().unwrap_or(false) {
        return None;
    }
    if data["kind"].as_str() == Some("plan") {
        return Some("Approve + auto-edit".into());
    }
    if let Some(rule) = data["rule"].as_str().filter(|r| !r.is_empty()) {
        return Some(format!("Always allow {rule}"));
    }
    (data["kind"].as_str() == Some("edit")).then(|| "Allow all edits".into())
}

fn clip(s: &str, n: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= n {
        one_line
    } else {
        format!("{}…", one_line.chars().take(n).collect::<String>())
    }
}

/// Validate a server base URL. Empty selects ntfy.sh; preserve optional path prefixes.
pub fn clean_server(raw: &str) -> Result<String, String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return Ok(String::new());
    }
    let url = reqwest::Url::parse(raw)
        .map_err(|_| "Enter the ntfy server's URL, e.g. https://ntfy.sh.".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Enter the ntfy server's URL, e.g. https://ntfy.sh.".into());
    }
    Ok(raw.to_string())
}

pub fn server_of(v: &Vault) -> String {
    if v.ntfy.server.is_empty() {
        DEFAULT_SERVER.into()
    } else {
        v.ntfy.server.clone()
    }
}

/// Store a validated server and optional write-only bearer token in the vault.
pub fn configure_ntfy(v: &mut Vault, server: &str, token: Option<&str>) -> Result<(), String> {
    let server = clean_server(server)?;
    let changed_server = v.ntfy.server != server;
    v.ntfy.server = server;
    if let Some(token) = token {
        let token = token.trim();
        if token.chars().any(char::is_whitespace) {
            return Err("The ntfy access token cannot contain whitespace.".into());
        }
        v.ntfy.token = token.to_string();
    } else if changed_server {
        v.ntfy.token.clear();
    }
    Ok(())
}

pub fn new_topic() -> String {
    format!("ol-{}", &auth::hash(&auth::secret())[..32])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub details: bool,
    pub confirm_all: bool,
    pub questions: bool,
    pub done: bool,
}
impl Options {
    pub fn from_json(v: &Value) -> Self {
        let b = |k: &str, d: bool| v[k].as_bool().unwrap_or(d);
        Self {
            details: b("details", false),
            confirm_all: b("confirm_all", false),
            questions: b("questions", true),
            done: b("done", true),
        }
    }
    fn apply(self, c: &mut PushCfg) {
        c.details = self.details;
        c.confirm_all = self.confirm_all;
        c.questions = self.questions;
        c.done = self.done;
    }
}

/// Enable/update this session. Every change rotates the generation and invalidates outstanding grants.
pub fn enable(
    v: &mut Vault,
    session: &str,
    origin: &str,
    options: Options,
    now: i64,
    grants: Option<&mut Grants>,
) -> bool {
    let Some(s) = v.sessions.iter_mut().find(|s| s.id == session) else {
        return false;
    };
    if let Some(c) = &mut s.push {
        options.apply(c);
        c.base_url = origin.to_string();
        c.r#gen = auth::secret()[..16].to_string();
    } else {
        let mut c = PushCfg {
            topic: new_topic(),
            details: false,
            confirm_all: false,
            questions: true,
            done: true,
            base_url: origin.to_string(),
            r#gen: auth::secret()[..16].to_string(),
            since: now,
        };
        options.apply(&mut c);
        s.push = Some(c);
    }
    if let Some(g) = grants {
        g.forget_session(Some(session));
    }
    true
}

pub fn set_options(v: &mut Vault, session: &str, options: Options, grants: &mut Grants) -> bool {
    let Some(c) = v
        .sessions
        .iter_mut()
        .find(|s| s.id == session)
        .and_then(|s| s.push.as_mut())
    else {
        return false;
    };
    options.apply(c);
    c.r#gen = auth::secret()[..16].to_string();
    grants.forget_session(Some(session));
    true
}

pub fn disable(v: &mut Vault, session: &str, grants: &mut Grants) -> bool {
    let removed = v
        .sessions
        .iter_mut()
        .find(|s| s.id == session)
        .and_then(|s| s.push.take())
        .is_some();
    if removed {
        grants.forget_session(Some(session));
    }
    removed
}

pub fn state_json(v: &Vault, session: &str) -> Value {
    let server = server_of(v);
    let cfg = v
        .sessions
        .iter()
        .find(|s| s.id == session)
        .and_then(|s| s.push.clone());
    let host = server.split_once("://").map(|(_, r)| r).unwrap_or(&server);
    let public = server == DEFAULT_SERVER;
    match cfg {
        Some(c) => {
            json!({"enabled":true,"topic":c.topic,"server":server,"subscribe_url":format!("{server}/{}",c.topic),"app_url":format!("ntfy://{host}/{}",c.topic),"details":c.details,"confirm_all":c.confirm_all,"questions":c.questions,"done":c.done,"base_url":c.base_url,"server_public":public})
        }
        None => {
            json!({"enabled":false,"topic":"","server":server,"subscribe_url":"","app_url":"","details":false,"confirm_all":false,"questions":true,"done":true,"base_url":"","server_public":public})
        }
    }
}

/// Build a per-device ntfy message; without `details`, task content stays private.
pub fn message(
    cfg: &PushCfg,
    task: &str,
    item: &str,
    kind: &str,
    chat: &str,
    it: Option<&crate::agent::Item>,
    token: Option<&str>,
) -> Value {
    let base = cfg.base_url.trim_end_matches('/');
    let data = it.map(|i| i.data.clone()).unwrap_or(Value::Null);
    let text = it.map(|i| i.text.as_str()).unwrap_or("");
    let chat = clip(chat, 60);
    let generic = |s: &str| ("OpenLeash".to_string(), s.to_string());
    let (title, body) = match (kind, cfg.details) {
        ("approval", false) => generic("A chat needs your approval."),
        ("approval", true) => (
            format!("“{chat}” needs approval"),
            clip(data["title"].as_str().unwrap_or(text), 200),
        ),
        ("done", false) => generic("A chat finished."),
        ("done", true) => (
            format!("“{chat}” finished"),
            "Open OpenLeash to see the reply.".into(),
        ),
        ("failed", false) => generic("A chat stopped with an error."),
        ("failed", true) => (
            format!("“{chat}” stopped with an error"),
            "Open OpenLeash for the details.".into(),
        ),
        (_, false) => generic("A chat has a question for you."),
        (_, true) => (
            format!("“{chat}” has a question"),
            clip(
                data["questions"][0]["question"]
                    .as_str()
                    .unwrap_or("Open OpenLeash to answer."),
                200,
            ),
        ),
    };
    let tag = match kind {
        "approval" => "lock",
        "done" => "white_check_mark",
        "failed" => "warning",
        _ => "question",
    };
    let mut m = json!({"topic":cfg.topic,"title":title,"message":body,"priority":if kind=="done" {3}else{4},"tags":[tag],"click":format!("{base}/")});
    if let Some(token) = token {
        let confirm = format!(
            "{base}/__openleash/push/approve#task={}&item={}&token={}",
            urlencoding::encode(task),
            urlencoding::encode(item),
            urlencoding::encode(token)
        );
        let act = |label: &str, decision: &str| json!({"action":"http","label":label,"url":format!("{base}/__openleash/push/act"),"method":"POST","headers":{"Content-Type":"application/json"},"body":json!({"task":task,"item":item,"token":token,"decision":decision}).to_string(),"clear":true});
        let actions = if cfg.confirm_all {
            vec![json!({"action":"view","label":"Review…","url":confirm,"clear":true})]
        } else {
            let mut a = vec![act("Approve once", "once")];
            if always_label(&data).is_some() {
                a.push(json!({"action":"view","label":"Always…","url":confirm,"clear":true}));
            }
            a.push(act("Deny", "deny"));
            a
        };
        m["actions"] = json!(actions);
    }
    m
}

fn client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    })
}

/// Publish JSON to ntfy's root endpoint, optionally using bearer auth.
pub async fn publish(server: &str, token: &str, message: &Value) -> Result<(), String> {
    let mut req = client()
        .post(format!("{}/", server.trim_end_matches('/')))
        .json(message);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let r = req
        .send()
        .await
        .map_err(|e| format!("Couldn't reach the ntfy server: {e}"))?;
    if r.status().is_success() {
        Ok(())
    } else {
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Err(format!(
            "The ntfy server said {status}: {}",
            clip(&text, 200)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::auth::Ntfy;
    #[test]
    fn grants_are_hashed_bound_single_use_and_revalidated() {
        let mut v = Vault::default();
        let (sid, _) = v.create_session("phone", 1);
        assert!(enable(
            &mut v,
            &sid,
            "http://host",
            Options {
                details: false,
                confirm_all: false,
                questions: true,
                done: true
            },
            1,
            None
        ));
        let cfg = v.sessions[0].push.clone().unwrap();
        let mut grants = Grants::default();
        let token = grants.mint(&sid, &cfg.r#gen, "task", "item", Instant::now());
        assert!(!grants.map.contains_key(&token));
        assert!(check(
            &v,
            &grants,
            &Caller::Token,
            "task",
            "item",
            &token,
            Instant::now()
        )
        .is_ok());
        assert!(consume(
            &v,
            &mut grants,
            &Caller::Token,
            "task",
            "item",
            &token,
            Instant::now()
        )
        .is_ok());
        assert_eq!(
            check(
                &v,
                &grants,
                &Caller::Token,
                "task",
                "item",
                &token,
                Instant::now()
            ),
            Err(Refusal::Answered(None))
        );
        assert_eq!(
            check(
                &v,
                &grants,
                &Caller::Token,
                "other",
                "item",
                &token,
                Instant::now()
            ),
            Err(Refusal::BadToken)
        );
    }
    #[test]
    fn action_semantics_and_options_are_safe_by_default() {
        let defaults = Options::from_json(&json!({}));
        assert!(!defaults.details && !defaults.confirm_all && defaults.questions && defaults.done);
        assert_eq!(
            always_label(&json!({"kind":"command","rule":"git push *"})).as_deref(),
            Some("Always allow git push *")
        );
        assert_eq!(
            always_label(&json!({"kind":"command","rule":"x","repeat":true})),
            None
        );
        let mut v = Vault::default();
        let (sid, _) = v.create_session("phone", 1);
        enable(
            &mut v,
            &sid,
            "http://host",
            Options {
                confirm_all: true,
                ..defaults
            },
            1,
            None,
        );
        let cfg = v.sessions[0].push.as_ref().unwrap();
        let m = message(
            cfg,
            "task",
            "item",
            "approval",
            "chat",
            None,
            Some("secret"),
        );
        let actions = m["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["action"], "view");
    }
    #[test]
    fn server_normalization_rejects_unsafe_urls() {
        assert_eq!(
            clean_server(" https://ntfy.example/ ").unwrap(),
            "https://ntfy.example"
        );
        for raw in [
            "ftp://host",
            "https://u:p@host",
            "https://host/?q=1",
            "javascript:alert(1)",
        ] {
            assert!(clean_server(raw).is_err(), "{raw}");
        }
        let mut v = Vault::default();
        v.ntfy = Ntfy {
            server: "https://ntfy.example".into(),
            token: "secret".into(),
        };
        configure_ntfy(&mut v, "", None).unwrap();
        assert!(v.ntfy.token.is_empty());
    }
}
