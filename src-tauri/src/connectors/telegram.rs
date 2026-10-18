//! Telegram: a bot the user owns, talking to their OpenLeash agent.
//!
//! The bot long-polls the Bot API (`getUpdates`) over HTTPS and replies with
//! `sendMessage`. No webhook: a webhook needs a public URL pointing at this
//! machine, and long polling needs nothing but outbound HTTPS, which is what a
//! desktop app behind a home router has. No Telegram library: the bot needs
//! four methods, and `reqwest` is already in the tree.
//!
//! The security boundary is [`gate`]. A message reaches the agent only if its
//! sender's numeric user id is on the user's list, and an empty list lets
//! nobody in: anyone can find a bot by its @username, and this one drives an
//! agent that edits files and runs commands on the user's machine. Groups are
//! off by default and narrowed further by chat id; bots (including this one)
//! are never answered, so two agents can't loop each other.
//!
//! The token lives in `connectors.json`, not `settings.json` (see
//! `super::Vault`), and never crosses IPC back to the UI. Unlike Discord's,
//! Telegram's token is part of every request *URL*
//! (`https://api.telegram.org/bot<token>/getMe`), and reqwest puts the URL in
//! its error messages; so every error that can reach the UI or the log goes
//! through [`redact`] first.

use super::bridge::{self, Bridge, ChatDefaults, Incoming, Outgoing};
use super::discord::PERMS;
use crate::agent::Harness;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const API: &str = "https://api.telegram.org";
/// How long one `getUpdates` waits for a message before returning empty.
/// Telegram allows up to 50 seconds; longer polls mean fewer requests.
pub const POLL_SECS: u64 = 50;
/// Telegram's limit is 4,096 characters; leave room for a reopened code fence.
pub const MESSAGE_LIMIT: usize = 4_000;
/// A message older than the poller by more than this was sent while OpenLeash
/// wasn't running. The slack absorbs clock drift between this machine and
/// Telegram's servers, and a message sent while the poller was restarting
/// after a settings change.
pub const STALE_GRACE_SECS: i64 = 120;
/// How long to wait after a 409 (someone else is polling this bot).
const CONFLICT_WAIT: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Everything the user configures. The token is the only secret.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub token: String,
    /// Telegram numeric user ids allowed to talk to the agent. Required.
    pub allowed_users: Vec<String>,
    /// Group chat ids the bot answers in. Empty: any group it is in (when
    /// groups are allowed at all).
    pub allowed_chats: Vec<String>,
    pub allow_dms: bool,
    /// Answer in groups at all. Off by default: a group is a room other
    /// people can read, and adding the bot to one should be a decision.
    pub allow_groups: bool,
    /// In a group, answer only when @mentioned or replied to.
    pub require_mention: bool,
    /// Folder new Telegram chats work in. Empty: the app's current project.
    pub project: String,
    /// Model for new Telegram chats. Empty: the most recently used model.
    pub model: String,
    /// Permission mode for new Telegram chats.
    pub perm: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            token: String::new(),
            allowed_users: vec![],
            allowed_chats: vec![],
            allow_dms: true,
            allow_groups: false,
            require_mention: true,
            project: String::new(),
            model: String::new(),
            // Edits inside the project run; commands ask, and the question is
            // relayed to Telegram. Turbo would let a chat message run anything.
            perm: "auto".into(),
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("enabled", &self.enabled)
            .field(
                "token",
                &if self.token.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field("allowed_users", &self.allowed_users)
            .field("allowed_chats", &self.allowed_chats)
            .field("allow_dms", &self.allow_dms)
            .field("allow_groups", &self.allow_groups)
            .field("require_mention", &self.require_mention)
            .field("project", &self.project)
            .field("model", &self.model)
            .field("perm", &self.perm)
            .finish()
    }
}

/// What the UI may change. Everything but the token, which has its own
/// command so that a settings form can never echo it back.
#[derive(Debug, Clone, Deserialize)]
pub struct Patch {
    pub enabled: bool,
    pub allowed_users: Vec<String>,
    pub allowed_chats: Vec<String>,
    pub allow_dms: bool,
    pub allow_groups: bool,
    pub require_mention: bool,
    pub project: String,
    pub model: String,
    pub perm: String,
}

/// Telegram ids are plain integers: users are positive, groups negative
/// (supergroups look like -1001234567890). They are stored in canonical form
/// (`007` → `7`) so the gate can compare strings.
fn ids(list: &[String], groups: bool) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for raw in list {
        let id = raw.trim();
        if id.is_empty() {
            continue;
        }
        let how = if groups {
            "Group chat ids are numbers like -1001234567890: send a message in the group with the bot in it, and the id shows up below as the last ignored message."
        } else {
            "User ids are numbers like 123456789: message @userinfobot from that Telegram account to see it."
        };
        if id.starts_with('@') {
            return Err(format!(
                "\"{id}\" is a username, and usernames can change hands. {how}"
            ));
        }
        let n = id
            .parse::<i64>()
            .ok()
            .filter(|n| if groups { *n != 0 } else { *n > 0 })
            .ok_or_else(|| {
                let what = if groups { "group chat" } else { "user" };
                format!("\"{id}\" isn't a Telegram {what} id. {how}")
            })?;
        let id = n.to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

impl Config {
    pub fn apply(&mut self, p: Patch) -> Result<(), String> {
        let perm = p.perm.trim();
        if !PERMS.contains(&perm) {
            return Err(format!("Unknown permission mode `{perm}`."));
        }
        self.allowed_users = ids(&p.allowed_users, false)?;
        self.allowed_chats = ids(&p.allowed_chats, true)?;
        self.enabled = p.enabled;
        self.allow_dms = p.allow_dms;
        self.allow_groups = p.allow_groups;
        self.require_mention = p.require_mention;
        self.project = p.project.trim().to_string();
        self.model = p.model.trim().to_string();
        self.perm = perm.to_string();
        Ok(())
    }

    /// Why the bot can't run, if it can't.
    pub fn missing(&self) -> Option<&'static str> {
        if self.token.is_empty() {
            Some("Add the bot token.")
        } else if self.allowed_users.is_empty() {
            Some("Add at least one Telegram user id. Nobody can talk to the agent until you do.")
        } else {
            None
        }
    }
}

/// Clean up a pasted token. People paste it with a trailing newline, or
/// copy `bot<token>` out of an API URL; the shape check (`<bot id>:<secret>`)
/// catches a username or some other value pasted into the wrong box before
/// Telegram does.
pub fn clean_token(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    let t = t.strip_prefix("bot").unwrap_or(t).trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let ok = match t.split_once(':') {
        Some((id, secret)) => {
            (1..=20).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_digit())
                && (30..=100).contains(&secret.len())
                && secret
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }
        None => false,
    };
    if !ok {
        return Err("That doesn't look like a bot token. In Telegram, message @BotFather, send /mybots → your bot → API Token, and copy the whole value (a number, a colon, then about 35 letters and digits).".into());
    }
    Ok(t.to_string())
}

/// Take the token out of anything that might be shown or logged. Telegram's
/// own answers don't echo it, but a reqwest error carries the request URL,
/// and the URL carries the token.
pub fn redact(s: &str, token: &str) -> String {
    if token.is_empty() {
        return s.to_string();
    }
    let mut out = s.replace(token, "<token>");
    // The secret half alone is as good as the whole thing to an attacker
    // who can guess the bot id from its profile.
    if let Some((_, secret)) = token.split_once(':') {
        if !secret.is_empty() {
            out = out.replace(secret, "<token>");
        }
    }
    out
}

/// What to do about a Bot API call that didn't succeed.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The config is wrong: stop. Retrying a bad token only gets the bot
    /// rate-limited.
    Fatal(String),
    /// Wait exactly this long, then try again.
    Wait(Duration, String),
    /// Try again after the usual (growing) backoff: Telegram or the network
    /// is having a moment.
    Backoff(String),
    /// Telegram refused this particular request (a chat the bot was removed
    /// from, a deleted topic). Sending the same thing again won't help; the
    /// poller still retries, slowly, since its request never changes.
    Reject(String),
}

impl Verdict {
    pub fn message(self) -> String {
        match self {
            Verdict::Fatal(e) | Verdict::Wait(_, e) | Verdict::Backoff(e) | Verdict::Reject(e) => e,
        }
    }

    fn redacted(self, token: &str) -> Verdict {
        match self {
            Verdict::Fatal(e) => Verdict::Fatal(redact(&e, token)),
            Verdict::Wait(d, e) => Verdict::Wait(d, redact(&e, token)),
            Verdict::Backoff(e) => Verdict::Backoff(redact(&e, token)),
            Verdict::Reject(e) => Verdict::Reject(redact(&e, token)),
        }
    }
}

/// Decide from an HTTP status and Telegram's JSON answer
/// (`{"ok":false,"error_code":…,"description":…,"parameters":{…}}`).
pub fn classify(status: u16, body: &Value) -> Verdict {
    let code = body["error_code"]
        .as_u64()
        .map(|c| c as u16)
        .unwrap_or(status);
    let desc = bridge::sanitize(body["description"].as_str().unwrap_or(""), 300);
    match code {
        // A revoked or mistyped token: 401, or 404 because `bot<token>` is
        // part of the path and an unknown one is an unknown page.
        401 | 404 => Verdict::Fatal("Telegram rejected the bot token. Paste a fresh one (@BotFather → /mybots → your bot → API Token).".into()),
        // Only one `getUpdates` may run per bot, and none while a webhook is
        // set. Not fatal: right after this app restarts its own poller,
        // Telegram can still see the old request for a moment, and another
        // copy of OpenLeash (or another bot program) may simply be closed
        // later. Retrying every half minute costs nothing; stopping would
        // leave the bot off until the user noticed.
        409 => {
            let why = if desc.to_lowercase().contains("webhook") {
                "This bot has a webhook set, so Telegram won't hand messages to OpenLeash. Remove it (open https://api.telegram.org/bot<token>/deleteWebhook in a browser, with your token), or use a different bot. Retrying…"
            } else {
                "Another program is polling this bot (another OpenLeash, or a different bot program using the same token). Close it, or give OpenLeash its own bot. Retrying…"
            };
            Verdict::Wait(CONFLICT_WAIT, why.into())
        }
        429 => {
            let secs = body["parameters"]["retry_after"].as_u64().unwrap_or(5);
            Verdict::Wait(
                Duration::from_secs(secs.clamp(1, 3600)),
                format!("Telegram asked the bot to slow down for {secs}s."),
            )
        }
        _ => {
            let msg = if desc.is_empty() {
                format!("Telegram answered {code}.")
            } else {
                format!("Telegram answered {code}: {desc}")
            };
            if (400..500).contains(&code) {
                Verdict::Reject(msg)
            } else {
                Verdict::Backoff(msg)
            }
        }
    }
}

/// The next wait after a failure: doubling, capped at a minute.
pub fn next_backoff(prev: Duration) -> Duration {
    (prev * 2).clamp(Duration::from_secs(1), MAX_BACKOFF)
}

/// One incoming `message`, the fields the gate and the bridge need.
#[derive(Debug, Clone, Default)]
pub struct Msg {
    pub id: i64,
    pub chat_id: i64,
    /// private | group | supergroup | channel
    pub chat_type: String,
    pub chat_title: String,
    /// The forum topic, when the group has topics turned on.
    pub thread: Option<i64>,
    pub from_id: i64,
    pub from_bot: bool,
    pub author_name: String,
    /// The text, or a photo's or file's caption.
    pub text: String,
    pub mentions_bot: bool,
    pub reply_to_bot: bool,
    /// Unix seconds, from Telegram.
    pub date: i64,
    pub attachments: Vec<String>,
}

/// The text an entity covers. Telegram counts entity offsets in UTF-16 code
/// units, so an emoji before a mention would shift a char- or byte-based cut.
fn entity_text(text: &str, e: &Value) -> Option<String> {
    let off = e["offset"].as_u64()? as usize;
    let len = e["length"].as_u64()? as usize;
    let units: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16(units.get(off..off.checked_add(len)?)?).ok()
}

/// Ordinary messages only. `edited_message` is not a new request (and
/// answering edits would run a task twice); `channel_post` has no person
/// behind it to check against the allowlist. Neither is even asked for
/// (`allowed_updates`), but a stray one is still dropped here.
pub fn parse(update: &Value, bot_id: i64, bot_username: &str) -> Option<Msg> {
    let m = update.get("message")?;
    let from = m.get("from")?;
    let chat = &m["chat"];
    let (text, entities) = match m["text"].as_str() {
        Some(t) => (t.to_string(), &m["entities"]),
        None => (
            m["caption"].as_str().unwrap_or("").to_string(),
            &m["caption_entities"],
        ),
    };
    let handle = format!("@{}", bot_username.to_lowercase());
    let mentions_bot = entities.as_array().is_some_and(|es| {
        es.iter().any(|e| match e["type"].as_str() {
            Some("mention") => {
                !bot_username.is_empty()
                    && entity_text(&text, e).is_some_and(|s| s.to_lowercase() == handle)
            }
            // A mention of someone without a username carries the user.
            Some("text_mention") => bot_id != 0 && e["user"]["id"].as_i64() == Some(bot_id),
            // `/new@thisbot` is addressed to this bot by name.
            Some("bot_command") => {
                !bot_username.is_empty()
                    && entity_text(&text, e).is_some_and(|s| s.to_lowercase().ends_with(&handle))
            }
            _ => false,
        })
    });
    let thread = if m["is_topic_message"].as_bool() == Some(true) {
        m["message_thread_id"].as_i64()
    } else {
        None
    };
    let reply = &m["reply_to_message"];
    // In a forum topic every message "replies" to the topic's opening
    // message; that is not a reply to the bot even if the bot opened it.
    let topic_root = thread.is_some() && reply["message_id"].as_i64() == thread;
    let reply_to_bot = bot_id != 0 && !topic_root && reply["from"]["id"].as_i64() == Some(bot_id);
    let mut attachments = vec![];
    // Names only, never a link: a Telegram file URL contains the bot token.
    if m.get("photo").is_some() {
        attachments.push("a photo (not downloaded; open Telegram to see it)".to_string());
    }
    for kind in ["document", "video", "audio", "voice"] {
        if let Some(f) = m.get(kind) {
            let name = f["file_name"].as_str().unwrap_or(kind);
            attachments.push(format!(
                "{} (not downloaded; open Telegram to see it)",
                bridge::sanitize(name, 120)
            ));
        }
    }
    if text.trim().is_empty() && attachments.is_empty() {
        // Joins, pins, stickers and other things nobody typed.
        return None;
    }
    let username = from["username"].as_str().filter(|u| !u.is_empty());
    let first = from["first_name"].as_str().unwrap_or("");
    Some(Msg {
        id: m["message_id"].as_i64()?,
        chat_id: chat["id"].as_i64()?,
        chat_type: chat["type"].as_str().unwrap_or("").to_string(),
        chat_title: chat["title"].as_str().unwrap_or("").to_string(),
        thread,
        from_id: from["id"].as_i64()?,
        from_bot: from["is_bot"].as_bool().unwrap_or(false),
        author_name: match username {
            Some(u) => format!("@{u}"),
            None if !first.is_empty() => first.to_string(),
            None => "someone".into(),
        },
        text,
        mentions_bot,
        reply_to_bot,
        date: m["date"].as_i64().unwrap_or(0),
        attachments,
    })
}

/// Why a message was not handed to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignore {
    Bot,
    NoUsers,
    User,
    Dms,
    Groups,
    Chat,
    Mention,
    Channel,
    Stale,
}

impl Ignore {
    pub fn says(self) -> &'static str {
        match self {
            Ignore::Bot => "from a bot",
            Ignore::NoUsers => "no allowed users are set",
            Ignore::User => "the sender isn't in your allowed users",
            Ignore::Dms => "direct messages are turned off",
            Ignore::Groups => "groups are turned off",
            Ignore::Chat => "that group isn't in your allowed group chats",
            Ignore::Mention => "the bot wasn't mentioned",
            Ignore::Channel => "channels aren't supported",
            Ignore::Stale => "it was sent while OpenLeash wasn't running",
        }
    }
}

/// Who may talk to the agent.
///
/// The order matters: identity first, so a stranger is refused for being a
/// stranger wherever they wrote; then place; then the mention rule, which is
/// politeness rather than security and so comes last.
pub fn gate(cfg: &Config, m: &Msg, bot_id: i64) -> Result<(), Ignore> {
    if m.from_bot || m.from_id == bot_id {
        return Err(Ignore::Bot);
    }
    if cfg.allowed_users.is_empty() {
        return Err(Ignore::NoUsers);
    }
    let from = m.from_id.to_string();
    if !cfg.allowed_users.iter().any(|u| u == &from) {
        return Err(Ignore::User);
    }
    match m.chat_type.as_str() {
        "private" => {
            if cfg.allow_dms {
                Ok(())
            } else {
                Err(Ignore::Dms)
            }
        }
        "group" | "supergroup" => {
            if !cfg.allow_groups {
                return Err(Ignore::Groups);
            }
            let chat = m.chat_id.to_string();
            if !cfg.allowed_chats.is_empty() && !cfg.allowed_chats.iter().any(|c| c == &chat) {
                return Err(Ignore::Chat);
            }
            if cfg.require_mention && !m.mentions_bot && !m.reply_to_bot {
                return Err(Ignore::Mention);
            }
            Ok(())
        }
        _ => Err(Ignore::Channel),
    }
}

/// Sent before the poller started (see [`STALE_GRACE_SECS`]). Those messages
/// are acknowledged and dropped rather than run: Telegram keeps a bot's
/// updates for a day, and "deploy it" typed this morning shouldn't run when
/// the laptop opens tonight. This is also what Discord does, whose gateway
/// doesn't replay messages from while the bot was offline.
pub fn stale(date: i64, started: i64) -> bool {
    date > 0 && date + STALE_GRACE_SECS < started
}

/// The message without the bot's own @mention. `/new@thisbot` becomes
/// `/new`, so the bridge's commands work as typed in a group.
pub fn strip_mention(text: &str, bot_username: &str) -> String {
    if bot_username.is_empty() {
        return text.trim().to_string();
    }
    let needle = format!("@{}", bot_username.to_ascii_lowercase());
    // ASCII lowercasing keeps byte offsets, so positions found in `lower`
    // are valid in `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(i) = lower[at..].find(&needle) {
        let start = at + i;
        let end = start + needle.len();
        // `@thisbot_fan` is somebody else.
        let whole = !lower[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        out.push_str(&text[at..start]);
        if !whole {
            out.push_str(&text[start..end]);
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out.trim().to_string()
}

/// The bridge's conversation id. A forum topic is its own conversation (and
/// so its own chat), like a Discord thread: topics are how a group keeps
/// separate subjects apart.
pub fn conv_id(chat_id: i64, thread: Option<i64>) -> String {
    match thread {
        Some(t) => format!("{chat_id}:{t}"),
        None => chat_id.to_string(),
    }
}

/// The chat and topic to post a conversation's messages to.
pub fn parse_conv(conv: &str) -> Option<(i64, Option<i64>)> {
    match conv.split_once(':') {
        Some((c, t)) => Some((c.parse().ok()?, Some(t.parse().ok()?))),
        None => Some((conv.parse().ok()?, None)),
    }
}

/// `sendMessage` bodies for one reply: plain text (no `parse_mode`, so
/// nothing the agent writes can fail to parse as markup), split to
/// Telegram's size, the first part answering the message it replies to.
pub fn send_bodies(conv: &str, text: &str, reply_to: Option<&str>) -> Vec<Value> {
    let Some((chat, thread)) = parse_conv(conv) else {
        return vec![];
    };
    super::discord::split(text, MESSAGE_LIMIT)
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut b = json!({
                "chat_id": chat,
                "text": chunk,
                // A link in an agent reply shouldn't unfurl into a preview
                // Telegram's servers fetched.
                "link_preview_options": {"is_disabled": true},
            });
            if let Some(t) = thread {
                b["message_thread_id"] = json!(t);
            }
            if let (0, Some(id)) = (i, reply_to.and_then(|r| r.parse::<i64>().ok())) {
                // The person may have deleted their message meanwhile; the
                // reply should still arrive.
                b["reply_parameters"] =
                    json!({"message_id": id, "allow_sending_without_reply": true});
            }
            b
        })
        .collect()
}

/// What the Settings page shows. No token: only whether one is set.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub has_token: bool,
    pub allowed_users: Vec<String>,
    pub allowed_chats: Vec<String>,
    pub allow_dms: bool,
    pub allow_groups: bool,
    pub require_mention: bool,
    pub project: String,
    pub model: String,
    pub perm: String,
    /// off | incomplete | connecting | connected | reconnecting | error
    pub state: String,
    pub error: String,
    pub bot_name: String,
    pub bot_id: String,
    pub chats: usize,
    /// The last message the gate turned away, and why: the quickest way to see
    /// that an id was copied wrong. Ids only, never message text.
    pub last_ignored: String,
}

#[derive(Default, Clone)]
struct Live {
    state: String,
    error: String,
    bot_id: i64,
    bot_name: String,
    last_ignored: String,
}

pub struct Telegram {
    http: reqwest::Client,
    cfg: Mutex<Config>,
    live: Mutex<Live>,
    run: Mutex<Option<CancellationToken>>,
    /// The same queue the bridge posts to, for the adapter's own notices.
    out: mpsc::UnboundedSender<Outgoing>,
    pub bridge: Arc<Bridge>,
}

enum End {
    Cancelled,
    Retry(Option<Duration>, String),
    Fatal(String),
}

impl From<Verdict> for End {
    fn from(v: Verdict) -> End {
        match v {
            Verdict::Fatal(e) => End::Fatal(e),
            Verdict::Wait(d, e) => End::Retry(Some(d), e),
            Verdict::Backoff(e) | Verdict::Reject(e) => End::Retry(None, e),
        }
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Telegram {
    pub fn new(h: Arc<Harness>, cfg: Config) -> Arc<Telegram> {
        let (tx, rx) = mpsc::unbounded_channel();
        let bridge = Bridge::new(
            h,
            tx.clone(),
            crate::agent::store::data_dir().join("telegram-chats.json"),
        );
        let me = Arc::new(Telegram {
            // `getUpdates` sets its own, longer timeout: the default has to
            // stay short so a hung `sendMessage` doesn't hold up replies.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
            cfg: Mutex::new(cfg),
            live: Mutex::new(Live::default()),
            run: Mutex::new(None),
            out: tx,
            bridge,
        });
        let sender = me.clone();
        tauri::async_runtime::spawn(async move { sender.deliver(rx).await });
        me
    }

    pub fn config(&self) -> Config {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_config(&self, cfg: Config) {
        *self.cfg.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
    }

    fn set_live(&self, f: impl FnOnce(&mut Live)) {
        f(&mut self.live.lock().unwrap_or_else(|e| e.into_inner()));
    }

    pub async fn status(&self) -> Status {
        let c = self.config();
        let l = self.live.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Status {
            enabled: c.enabled,
            has_token: !c.token.is_empty(),
            allowed_users: c.allowed_users,
            allowed_chats: c.allowed_chats,
            allow_dms: c.allow_dms,
            allow_groups: c.allow_groups,
            require_mention: c.require_mention,
            project: c.project,
            model: c.model,
            perm: c.perm,
            state: if l.state.is_empty() {
                "off".into()
            } else {
                l.state
            },
            error: l.error,
            bot_name: l.bot_name,
            bot_id: if l.bot_id == 0 {
                String::new()
            } else {
                l.bot_id.to_string()
            },
            chats: self.bridge.chat_count().await,
            last_ignored: l.last_ignored,
        }
    }

    /// (Re)start or stop the bot to match the config. Cheap, and safe to call
    /// after every change: the old poller is cancelled first.
    pub fn apply(self: &Arc<Self>) {
        if let Some(c) = self.run.lock().unwrap_or_else(|e| e.into_inner()).take() {
            c.cancel();
        }
        let cfg = self.config();
        if !cfg.enabled {
            self.set_live(|l| {
                l.state = "off".into();
                l.error.clear();
            });
            return;
        }
        if let Some(why) = cfg.missing() {
            self.set_live(|l| {
                l.state = "incomplete".into();
                l.error = why.into();
            });
            return;
        }
        let token = CancellationToken::new();
        *self.run.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
        self.set_live(|l| {
            l.state = "connecting".into();
            l.error.clear();
        });
        let me = self.clone();
        tauri::async_runtime::spawn(async move { me.connect(token).await });
    }

    pub fn shutdown(&self) {
        if let Some(c) = self.run.lock().unwrap_or_else(|e| e.into_inner()).take() {
            c.cancel();
        }
    }

    /// Check the saved token against Telegram (`getMe`) without starting the
    /// bot. Ok: a short human line ("Token works: @name"). Errors never
    /// contain the token.
    pub async fn test(&self) -> Result<String, String> {
        let token = self.config().token;
        if token.is_empty() {
            return Err("Save a bot token first.".into());
        }
        let me = self
            .call(&token, "getMe", json!({}), None)
            .await
            .map_err(Verdict::message)?;
        let name = me["username"].as_str().unwrap_or("bot");
        let mut s = format!("Token works: @{name}.");
        if me["can_read_all_group_messages"].as_bool() == Some(false) {
            s.push_str(" Privacy mode is on, so in groups it only sees @mentions, replies to it and commands.");
        }
        Ok(s)
    }

    /// One Bot API call. Errors come back already redacted and classified.
    async fn call(
        &self,
        token: &str,
        method: &str,
        body: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, Verdict> {
        let mut req = self
            .http
            .post(format!("{API}/bot{token}/{method}"))
            .json(&body);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let r = req.send().await.map_err(|e| {
            Verdict::Backoff(redact(
                &format!("Can't reach Telegram: {}", e.without_url()),
                token,
            ))
        })?;
        let status = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if v["ok"].as_bool() == Some(true) {
            return Ok(v["result"].clone());
        }
        Err(classify(status, &v).redacted(token))
    }

    /// A call that waits out rate limits and brief outages, for replies.
    async fn send(&self, method: &str, body: Value) -> Result<(), String> {
        let token = self.config().token;
        if token.is_empty() {
            return Err("no token".into());
        }
        let mut wait = Duration::from_secs(1);
        for _ in 0..4 {
            match self.call(&token, method, body.clone(), None).await {
                Ok(_) => return Ok(()),
                Err(Verdict::Fatal(e) | Verdict::Reject(e)) => return Err(e),
                // A group's limit is about 20 messages a minute; a long reply
                // can hit it, and the wait Telegram names is the one to honor
                // (within reason: the queue behind it is waiting too).
                Err(Verdict::Wait(d, _)) => {
                    tokio::time::sleep(d.min(Duration::from_secs(300))).await
                }
                Err(Verdict::Backoff(_)) => {
                    tokio::time::sleep(wait).await;
                    wait = next_backoff(wait);
                }
            }
        }
        Err("Telegram kept refusing the message.".into())
    }

    /// Poll forever (with backoff) until cancelled or told the config is
    /// wrong.
    async fn connect(self: Arc<Self>, cancel: CancellationToken) {
        let started = now_unix();
        let mut backoff = Duration::from_secs(1);
        // Kept across reconnects so nothing is handled twice.
        let mut offset: i64 = 0;
        let mut told: HashSet<String> = HashSet::new();
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let mut polled = false;
            let wait = match self
                .session(&cancel, started, &mut offset, &mut told, &mut polled)
                .await
            {
                End::Cancelled => return,
                End::Fatal(e) => {
                    eprintln!("[openleash] telegram: {e}");
                    self.set_live(|l| {
                        l.state = "error".into();
                        l.error = e;
                    });
                    return;
                }
                End::Retry(wait, e) => {
                    if polled {
                        backoff = Duration::from_secs(1);
                    }
                    self.set_live(|l| {
                        l.state = "reconnecting".into();
                        l.error = e;
                    });
                    match wait {
                        Some(d) => d,
                        None => {
                            let d = backoff;
                            backoff = next_backoff(backoff);
                            d
                        }
                    }
                }
            };
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    async fn session(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        started: i64,
        offset: &mut i64,
        told: &mut HashSet<String>,
        polled: &mut bool,
    ) -> End {
        let token = self.config().token;
        let me = tokio::select! {
            _ = cancel.cancelled() => return End::Cancelled,
            r = self.call(&token, "getMe", json!({}), None) => r,
        };
        let me = match me {
            Ok(v) => v,
            Err(v) => return v.into(),
        };
        let bot_id = me["id"].as_i64().unwrap_or(0);
        let bot_name = me["username"].as_str().unwrap_or("").to_string();
        self.set_live(|l| {
            l.bot_id = bot_id;
            l.bot_name = bot_name;
        });
        loop {
            let mut body = json!({
                "timeout": POLL_SECS,
                "allowed_updates": ["message"],
            });
            if *offset > 0 {
                // Asking from `offset` confirms everything before it, so
                // Telegram stops resending what was already handled.
                body["offset"] = json!(*offset);
            }
            let r = tokio::select! {
                _ = cancel.cancelled() => return End::Cancelled,
                r = self.call(&token, "getUpdates", body, Some(Duration::from_secs(POLL_SECS + 15))) => r,
            };
            let updates = match r {
                Ok(v) => v,
                Err(v) => return v.into(),
            };
            if !*polled {
                *polled = true;
                self.set_live(|l| {
                    l.state = "connected".into();
                    l.error.clear();
                });
            }
            for u in updates.as_array().into_iter().flatten() {
                if let Some(id) = u["update_id"].as_i64() {
                    *offset = (*offset).max(id + 1);
                }
                self.inbound(u, started, told);
            }
        }
    }

    /// An update arrived. Gate it here (cheap, synchronous) and hand the work
    /// to its own task, so a slow chat creation never holds up polling.
    fn inbound(self: &Arc<Self>, u: &Value, started: i64, told: &mut HashSet<String>) {
        let (bot_id, bot_name) = {
            let l = self.live.lock().unwrap_or_else(|e| e.into_inner());
            (l.bot_id, l.bot_name.clone())
        };
        let cfg = self.config();
        let Some(m) = parse(u, bot_id, &bot_name) else {
            return;
        };
        let verdict = gate(&cfg, &m, bot_id).and_then(|_| {
            if stale(m.date, started) {
                Err(Ignore::Stale)
            } else {
                Ok(())
            }
        });
        let conv = conv_id(m.chat_id, m.thread);
        if let Err(why) = verdict {
            if why != Ignore::Bot && why != Ignore::Mention {
                self.set_live(|l| {
                    l.last_ignored = format!(
                        "Ignored a message from user {} in chat {}: {}.",
                        m.from_id,
                        m.chat_id,
                        why.says()
                    );
                });
            }
            // Only an allowed user hears about it, once per conversation, so
            // a pile of old messages gets one answer instead of a pile.
            if why == Ignore::Stale && told.insert(conv.clone()) {
                let _ = self.out.send(Outgoing::Post {
                    conv,
                    text: "⏸ OpenLeash wasn't running when you sent that, so I skipped it (and anything else from before it started). Send it again if you still want it.".into(),
                    reply_to: Some(m.id.to_string()),
                });
            }
            return;
        }
        let place = match m.chat_type.as_str() {
            "private" => "a private chat".to_string(),
            _ if m.chat_title.trim().is_empty() => "a group chat".to_string(),
            _ => format!("the group \"{}\"", bridge::sanitize(&m.chat_title, 80)),
        };
        let defaults = ChatDefaults {
            platform: "Telegram",
            project: cfg.project.clone(),
            model: cfg.model.clone(),
            perm: cfg.perm.clone(),
        };
        let incoming = Incoming {
            conv,
            message_id: m.id.to_string(),
            author: bridge::sanitize(&m.author_name, 64),
            author_id: m.from_id.to_string(),
            place,
            text: strip_mention(&m.text, &bot_name),
            attachments: m.attachments.clone(),
        };
        let me = self.clone();
        tauri::async_runtime::spawn(async move { me.bridge.handle(incoming, defaults).await });
    }

    /// Drain the bridge's queue onto Telegram, in order.
    async fn deliver(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Outgoing>) {
        while let Some(o) = rx.recv().await {
            match o {
                // Telegram shows "typing…" for about five seconds and the
                // bridge renews it every eight, so it flickers off briefly
                // during a long turn. Harmless, and not worth a timer here.
                Outgoing::Typing { conv } => {
                    let Some((chat, thread)) = parse_conv(&conv) else {
                        continue;
                    };
                    let mut body = json!({"chat_id": chat, "action": "typing"});
                    if let Some(t) = thread {
                        body["message_thread_id"] = json!(t);
                    }
                    let _ = self.send("sendChatAction", body).await;
                }
                Outgoing::Post {
                    conv,
                    text,
                    reply_to,
                } => {
                    for body in send_bodies(&conv, &text, reply_to.as_deref()) {
                        if let Err(e) = self.send("sendMessage", body).await {
                            eprintln!("[openleash] telegram: couldn't post to chat {conv}: {e}");
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: i64 = 7_000_000_001;
    const BOT_NAME: &str = "LeashBot";
    const ME: i64 = 123_456_789;
    const TOKEN: &str = "7000000001:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsawQ";

    fn cfg() -> Config {
        Config {
            allowed_users: vec![ME.to_string()],
            ..Config::default()
        }
    }

    fn dm(from: i64) -> Msg {
        Msg {
            id: 1,
            chat_id: from,
            chat_type: "private".into(),
            from_id: from,
            text: "hi".into(),
            ..Msg::default()
        }
    }

    fn in_group(from: i64, chat: i64, mentioned: bool) -> Msg {
        Msg {
            chat_id: chat,
            chat_type: "supergroup".into(),
            chat_title: "Team".into(),
            mentions_bot: mentioned,
            ..dm(from)
        }
    }

    fn groups_on() -> Config {
        Config {
            allow_groups: true,
            ..cfg()
        }
    }

    fn update(message: Value) -> Value {
        json!({"update_id": 10, "message": message})
    }

    #[test]
    fn defaults_are_safe() {
        let c = Config::default();
        assert!(!c.enabled && c.token.is_empty() && c.allowed_users.is_empty());
        assert!(c.allow_dms && !c.allow_groups && c.require_mention);
        assert_eq!(c.perm, "auto");
        assert!(c.missing().is_some());
        let partial: Config = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(partial.enabled && !partial.allow_groups && partial.require_mention);
    }

    #[test]
    fn nobody_gets_in_without_an_allowed_user_list() {
        let open = Config {
            token: TOKEN.into(),
            allow_groups: true,
            ..Config::default()
        };
        assert_eq!(gate(&open, &dm(ME), BOT), Err(Ignore::NoUsers));
        assert_eq!(
            gate(&open, &in_group(ME, -100, true), BOT),
            Err(Ignore::NoUsers)
        );
        assert!(open.missing().is_some(), "the bot doesn't even start");
        assert!(cfg().missing().is_some(), "nor without a token");
        let ready = Config {
            token: TOKEN.into(),
            ..cfg()
        };
        assert!(ready.missing().is_none());
    }

    #[test]
    fn only_listed_users_reach_the_agent() {
        let c = cfg();
        assert_eq!(gate(&c, &dm(ME), BOT), Ok(()));
        assert_eq!(gate(&c, &dm(987_654_321), BOT), Err(Ignore::User));
        // A stranger is refused for being a stranger, even where a listed
        // user would also have been refused for another reason.
        let no_dm = Config {
            allow_dms: false,
            ..cfg()
        };
        assert_eq!(gate(&no_dm, &dm(987_654_321), BOT), Err(Ignore::User));
        assert_eq!(gate(&no_dm, &dm(ME), BOT), Err(Ignore::Dms));
        assert_eq!(
            gate(&c, &in_group(987_654_321, -100, true), BOT),
            Err(Ignore::User)
        );
    }

    #[test]
    fn bots_are_never_answered_including_this_one() {
        let mut c = cfg();
        c.allowed_users.push(BOT.to_string());
        let mut m = dm(ME);
        m.from_bot = true;
        assert_eq!(gate(&c, &m, BOT), Err(Ignore::Bot));
        assert_eq!(
            gate(&c, &dm(BOT), BOT),
            Err(Ignore::Bot),
            "even if its own id was listed"
        );
    }

    #[test]
    fn groups_are_off_by_default_and_narrowed_by_chat_id() {
        let c = cfg();
        assert_eq!(
            gate(&c, &in_group(ME, -1001234567890, true), BOT),
            Err(Ignore::Groups)
        );
        let mut c = groups_on();
        assert_eq!(gate(&c, &in_group(ME, -1001234567890, true), BOT), Ok(()));
        c.allowed_chats = vec!["-1001234567890".into()];
        assert_eq!(gate(&c, &in_group(ME, -1001234567890, true), BOT), Ok(()));
        assert_eq!(
            gate(&c, &in_group(ME, -1009999999999, true), BOT),
            Err(Ignore::Chat)
        );
        // A plain (non-super) group too.
        let mut basic = in_group(ME, -4000001, true);
        basic.chat_type = "group".into();
        assert_eq!(gate(&c, &basic, BOT), Err(Ignore::Chat));
        // Chat limits never apply to private chats; `allow_dms` governs those.
        assert_eq!(gate(&c, &dm(ME), BOT), Ok(()));
        // A channel has no sender to check, so it is never answered.
        let mut chan = in_group(ME, -1001234567890, true);
        chan.chat_type = "channel".into();
        assert_eq!(gate(&c, &chan, BOT), Err(Ignore::Channel));
    }

    #[test]
    fn a_mention_or_a_reply_is_needed_in_groups_by_default() {
        let c = groups_on();
        assert_eq!(
            gate(&c, &in_group(ME, -100, false), BOT),
            Err(Ignore::Mention)
        );
        assert_eq!(gate(&c, &in_group(ME, -100, true), BOT), Ok(()));
        let mut reply = in_group(ME, -100, false);
        reply.reply_to_bot = true;
        assert_eq!(gate(&c, &reply, BOT), Ok(()));
        let relaxed = Config {
            require_mention: false,
            ..groups_on()
        };
        assert_eq!(gate(&relaxed, &in_group(ME, -100, false), BOT), Ok(()));
        assert_eq!(
            gate(&c, &dm(ME), BOT),
            Ok(()),
            "private chats never need one"
        );
    }

    #[test]
    fn private_messages_parse() {
        let u = update(json!({
            "message_id": 5, "date": 1_700_000_000,
            "from": {"id": ME, "is_bot": false, "first_name": "Alice", "username": "alice"},
            "chat": {"id": ME, "type": "private", "first_name": "Alice"},
            "text": "fix the build"
        }));
        let m = parse(&u, BOT, BOT_NAME).unwrap();
        assert_eq!((m.id, m.chat_id, m.from_id), (5, ME, ME));
        assert_eq!(m.chat_type, "private");
        assert_eq!(m.author_name, "@alice");
        assert_eq!(m.date, 1_700_000_000);
        assert!(!m.from_bot && !m.mentions_bot && !m.reply_to_bot);
        assert_eq!(m.thread, None);
        assert_eq!(gate(&cfg(), &m, BOT), Ok(()));
        // No username: the first name.
        let u = update(json!({
            "message_id": 6, "from": {"id": ME, "first_name": "Alice"},
            "chat": {"id": ME, "type": "private"}, "text": "hi"
        }));
        assert_eq!(parse(&u, BOT, BOT_NAME).unwrap().author_name, "Alice");
    }

    #[test]
    fn group_mentions_count_by_entity_case_insensitively_after_emoji() {
        // "🙂 " is three UTF-16 units: the entity offset is 3, not 2 chars or
        // 5 bytes.
        let text = "🙂 @leashbot run the tests";
        let u = update(json!({
            "message_id": 7,
            "from": {"id": ME, "first_name": "Alice"},
            "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Team"},
            "text": text,
            "entities": [{"type": "mention", "offset": 3, "length": 9}]
        }));
        let m = parse(&u, BOT, BOT_NAME).unwrap();
        assert!(m.mentions_bot);
        assert_eq!(m.chat_title, "Team");
        assert_eq!(strip_mention(&m.text, BOT_NAME), "🙂  run the tests");
        assert_eq!(gate(&groups_on(), &m, BOT), Ok(()));
        // Somebody else's mention is not this bot's.
        let other = update(json!({
            "message_id": 8, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "group"},
            "text": "@leashbot_fan hi",
            "entities": [{"type": "mention", "offset": 0, "length": 13}]
        }));
        let m = parse(&other, BOT, BOT_NAME).unwrap();
        assert!(!m.mentions_bot);
        assert_eq!(gate(&groups_on(), &m, BOT), Err(Ignore::Mention));
        // A text mention carries the user instead of a username.
        let tm = update(json!({
            "message_id": 9, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "group"},
            "text": "Leash do it",
            "entities": [{"type": "text_mention", "offset": 0, "length": 5, "user": {"id": BOT}}]
        }));
        assert!(parse(&tm, BOT, BOT_NAME).unwrap().mentions_bot);
        // `/status@LeashBot` is addressed to this bot and works as `/status`.
        let cmd = update(json!({
            "message_id": 10, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "group"},
            "text": "/status@LeashBot",
            "entities": [{"type": "bot_command", "offset": 0, "length": 16}]
        }));
        let m = parse(&cmd, BOT, BOT_NAME).unwrap();
        assert!(m.mentions_bot);
        assert_eq!(strip_mention(&m.text, BOT_NAME), "/status");
    }

    #[test]
    fn replies_to_the_bot_count_but_a_topic_root_does_not() {
        let u = update(json!({
            "message_id": 11, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "supergroup", "title": "T"},
            "text": "and the docs too",
            "reply_to_message": {"message_id": 3, "from": {"id": BOT, "is_bot": true}}
        }));
        assert!(parse(&u, BOT, BOT_NAME).unwrap().reply_to_bot);
        let topic = update(json!({
            "message_id": 12, "message_thread_id": 40, "is_topic_message": true,
            "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "supergroup", "title": "T", "is_forum": true},
            "text": "hello",
            "reply_to_message": {"message_id": 40, "from": {"id": BOT, "is_bot": true}}
        }));
        let m = parse(&topic, BOT, BOT_NAME).unwrap();
        assert!(!m.reply_to_bot, "the topic's opening message isn't a reply");
        assert_eq!(m.thread, Some(40));
        assert_eq!(conv_id(m.chat_id, m.thread), "-100:40");
        // `message_thread_id` alone (a reply thread outside forums) is not a topic.
        let plain = update(json!({
            "message_id": 13, "message_thread_id": 2,
            "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "supergroup"}, "text": "x"
        }));
        assert_eq!(parse(&plain, BOT, BOT_NAME).unwrap().thread, None);
    }

    #[test]
    fn bots_channels_edits_and_service_messages_are_not_requests() {
        let from_bot = update(json!({
            "message_id": 1, "from": {"id": 55, "is_bot": true, "first_name": "Other"},
            "chat": {"id": 55, "type": "private"}, "text": "hi"
        }));
        let m = parse(&from_bot, BOT, BOT_NAME).unwrap();
        assert!(m.from_bot);
        assert_eq!(gate(&cfg(), &m, BOT), Err(Ignore::Bot));
        let chan = json!({"update_id": 2, "channel_post": {
            "message_id": 1, "chat": {"id": -100, "type": "channel"}, "text": "news"
        }});
        assert!(parse(&chan, BOT, BOT_NAME).is_none());
        let edit = json!({"update_id": 3, "edited_message": {
            "message_id": 1, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": ME, "type": "private"}, "text": "hi again"
        }});
        assert!(parse(&edit, BOT, BOT_NAME).is_none());
        let join = update(json!({
            "message_id": 1, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": -100, "type": "group"}, "new_chat_members": [{"id": BOT}]
        }));
        assert!(parse(&join, BOT, BOT_NAME).is_none());
    }

    #[test]
    fn attachments_are_named_without_a_link_carrying_the_token() {
        let u = update(json!({
            "message_id": 1, "from": {"id": ME, "first_name": "A"},
            "chat": {"id": ME, "type": "private"},
            "caption": "what's in this log?",
            "document": {"file_id": "BQAC", "file_name": "build<1>.log"},
            "photo": [{"file_id": "AgAC", "width": 90, "height": 90}]
        }));
        let m = parse(&u, BOT, BOT_NAME).unwrap();
        assert_eq!(m.text, "what's in this log?");
        assert_eq!(m.attachments.len(), 2);
        assert!(m.attachments.iter().any(|a| a.starts_with("build1.log")));
        assert!(m.attachments.iter().all(|a| !a.contains("http")));
    }

    #[test]
    fn stale_messages_are_those_sent_well_before_the_poller_started() {
        let start = 1_700_000_000;
        assert!(stale(start - 3600, start));
        assert!(!stale(start - 30, start), "a restart's worth of slack");
        assert!(!stale(start + 5, start));
        assert!(!stale(0, start), "no date: don't guess");
    }

    #[test]
    fn ids_are_validated_and_canonical() {
        let mut c = Config::default();
        let p = |users: Vec<&str>, chats: Vec<&str>| Patch {
            enabled: true,
            allowed_users: users.into_iter().map(String::from).collect(),
            allowed_chats: chats.into_iter().map(String::from).collect(),
            allow_dms: true,
            allow_groups: true,
            require_mention: true,
            project: " D:\\work ".into(),
            model: String::new(),
            perm: "auto".into(),
        };
        c.apply(p(
            vec![" 123456789 ", "123456789", "", "0042"],
            vec!["-1001234567890", " -4000001"],
        ))
        .unwrap();
        assert_eq!(c.allowed_users, vec!["123456789".to_string(), "42".into()]);
        assert_eq!(
            c.allowed_chats,
            vec!["-1001234567890".to_string(), "-4000001".into()]
        );
        assert_eq!(c.project, "D:\\work");
        let e = c.apply(p(vec!["@alice"], vec![])).unwrap_err();
        assert!(e.contains("username") && e.contains("@userinfobot"), "{e}");
        assert!(
            c.apply(p(vec!["-123"], vec![])).is_err(),
            "users are positive"
        );
        assert!(c.apply(p(vec!["0"], vec![])).is_err());
        assert!(c.apply(p(vec!["12ab"], vec![])).is_err());
        assert!(c.apply(p(vec![ME_STR], vec!["0"])).is_err());
        assert!(c.apply(p(vec![ME_STR], vec!["team"])).is_err());
        let mut bad = p(vec![ME_STR], vec![]);
        bad.perm = "root".into();
        assert!(
            c.apply(bad).is_err(),
            "an unknown permission mode is refused, not stored"
        );
        assert_eq!(c.perm, "auto");
    }

    const ME_STR: &str = "123456789";

    #[test]
    fn tokens_are_cleaned_and_shape_checked() {
        assert_eq!(clean_token(&format!("  {TOKEN}\n")).unwrap(), TOKEN);
        assert_eq!(clean_token(&format!("bot{TOKEN}")).unwrap(), TOKEN);
        assert_eq!(clean_token("").unwrap(), "");
        assert_eq!(clean_token("  \n").unwrap(), "");
        assert!(
            clean_token("123456789").is_err(),
            "a user id is not a token"
        );
        assert!(clean_token("@LeashBot").is_err());
        assert!(clean_token("7000000001:short").is_err());
        assert!(clean_token("abc:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsawQ").is_err());
        assert!(clean_token("7000000001:AAHdqTcvCH1vGWJxfSeofSAs0K5PAL sawQ").is_err());
        let e = clean_token("nope").unwrap_err();
        assert!(e.contains("@BotFather"), "{e}");
    }

    #[test]
    fn the_token_never_reaches_debug_output_or_errors() {
        let c = Config {
            token: TOKEN.into(),
            ..Config::default()
        };
        let s = format!("{c:?}");
        assert!(!s.contains(TOKEN) && s.contains("<redacted>"), "{s}");
        let secret = TOKEN.split_once(':').unwrap().1;
        assert!(!s.contains(secret));
        // What a reqwest error looks like when it keeps the URL.
        let err = format!(
            "error sending request for url (https://api.telegram.org/bot{TOKEN}/getUpdates)"
        );
        let r = redact(&err, TOKEN);
        assert!(
            !r.contains(secret) && r.contains("bot<token>/getUpdates"),
            "{r}"
        );
        assert_eq!(redact(&format!("x {secret} y"), TOKEN), "x <token> y");
        assert_eq!(redact("nothing here", ""), "nothing here");
    }

    #[test]
    fn api_failures_map_to_stop_wait_or_back_off() {
        let bad = json!({"ok": false, "error_code": 401, "description": "Unauthorized"});
        assert!(matches!(classify(401, &bad), Verdict::Fatal(_)));
        let gone = json!({"ok": false, "error_code": 404, "description": "Not Found"});
        assert!(matches!(classify(404, &gone), Verdict::Fatal(_)));
        let conflict = json!({"ok": false, "error_code": 409, "description": "Conflict: terminated by other getUpdates request; make sure that only one bot instance is running"});
        match classify(409, &conflict) {
            Verdict::Wait(d, e) => {
                assert_eq!(d, CONFLICT_WAIT);
                assert!(e.starts_with("Another program is polling this bot"), "{e}");
            }
            v => panic!("{v:?}"),
        }
        let hook = json!({"ok": false, "error_code": 409, "description": "Conflict: can't use getUpdates method while webhook is active; use deleteWebhook to delete the webhook first"});
        match classify(409, &hook) {
            Verdict::Wait(_, e) => assert!(e.contains("webhook"), "{e}"),
            v => panic!("{v:?}"),
        }
        let slow = json!({"ok": false, "error_code": 429, "description": "Too Many Requests: retry after 17", "parameters": {"retry_after": 17}});
        assert!(
            matches!(classify(429, &slow), Verdict::Wait(d, _) if d == Duration::from_secs(17))
        );
        let slow_no_hint = json!({"ok": false, "error_code": 429});
        assert!(
            matches!(classify(429, &slow_no_hint), Verdict::Wait(d, _) if d == Duration::from_secs(5))
        );
        assert!(matches!(classify(502, &Value::Null), Verdict::Backoff(e) if e.contains("502")));
        let bad_req =
            json!({"ok": false, "error_code": 400, "description": "Bad Request: chat not found"});
        assert!(
            matches!(classify(400, &bad_req), Verdict::Reject(e) if e.contains("chat not found"))
        );
        assert!(matches!(
            classify(403, &json!({"error_code": 403})),
            Verdict::Reject(_)
        ));
        // Backoff doubles and is capped.
        let mut d = Duration::from_secs(1);
        let mut seen = vec![];
        for _ in 0..8 {
            d = next_backoff(d);
            seen.push(d.as_secs());
        }
        assert_eq!(seen, vec![2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn conversations_are_chats_or_forum_topics() {
        assert_eq!(conv_id(ME, None), "123456789");
        assert_eq!(conv_id(-1001234567890, Some(7)), "-1001234567890:7");
        assert_eq!(
            parse_conv("-1001234567890:7"),
            Some((-1001234567890, Some(7)))
        );
        assert_eq!(parse_conv("123456789"), Some((ME, None)));
        assert_eq!(parse_conv("nope"), None);
        assert_eq!(parse_conv("-1:x"), None);
    }

    #[test]
    fn replies_are_plain_text_split_and_threaded() {
        let mut text = String::from("Intro line\n```rust\n");
        for i in 0..800 {
            text.push_str(&format!("let x{i} = {i};\n"));
        }
        text.push_str("```\nOutro");
        let bodies = send_bodies("-100:7", &text, Some("42"));
        assert!(bodies.len() > 1);
        for (i, b) in bodies.iter().enumerate() {
            let t = b["text"].as_str().unwrap();
            assert!(t.chars().count() <= 4096, "{} chars", t.chars().count());
            assert_eq!(t.matches("```").count() % 2, 0, "balanced fences:\n{t}");
            assert_eq!(b["chat_id"], json!(-100));
            assert_eq!(b["message_thread_id"], json!(7));
            assert!(b.get("parse_mode").is_none());
            assert_eq!(b["link_preview_options"]["is_disabled"], json!(true));
            assert_eq!(b.get("reply_parameters").is_some(), i == 0);
        }
        assert_eq!(
            bodies[0]["reply_parameters"],
            json!({"message_id": 42, "allow_sending_without_reply": true})
        );
        assert!(bodies[1]["text"].as_str().unwrap().starts_with("```rust"));
        let joined: Vec<&str> = bodies.iter().map(|b| b["text"].as_str().unwrap()).collect();
        let joined = joined.join("\n");
        for i in [0, 399, 799] {
            assert!(joined.contains(&format!("let x{i} = {i};")));
        }
        let one = send_bodies("123456789", "short", None);
        assert_eq!(one.len(), 1);
        assert!(one[0].get("message_thread_id").is_none());
        assert!(one[0].get("reply_parameters").is_none());
        assert!(send_bodies("garbage", "x", None).is_empty());
    }

    #[test]
    fn mentions_are_stripped_only_when_they_are_this_bot() {
        assert_eq!(strip_mention("@LeashBot fix it", BOT_NAME), "fix it");
        assert_eq!(
            strip_mention("hey @leashbot, fix it", BOT_NAME),
            "hey , fix it"
        );
        assert_eq!(strip_mention("/new@LEASHBOT", BOT_NAME), "/new");
        assert_eq!(
            strip_mention("cc @LeashBot_fan", BOT_NAME),
            "cc @LeashBot_fan"
        );
        assert_eq!(strip_mention("  plain  ", ""), "plain");
    }
}
