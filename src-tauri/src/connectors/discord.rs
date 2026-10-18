//! Discord: a bot account the user owns, talking to their OpenLeash agent.
//!
//! The bot connects to Discord's Gateway (a WebSocket) for incoming messages
//! and uses the REST API to reply. No Discord library: the Gateway protocol the
//! bot needs is five opcodes, and `tokio-tungstenite` and `reqwest` are already
//! in the tree.
//!
//! The security boundary is [`gate`]. A message reaches the agent only if its
//! author is on the user's list of Discord user ids, and an empty list lets
//! nobody in: this bot drives an agent that edits files and runs commands on
//! the user's machine, so "anyone who can see the bot" must never be the
//! default. Servers, channels and DMs narrow that further; bots (including this
//! one) are never answered, so two agents can't loop each other.
//!
//! The token lives in `connectors.json`, not `settings.json` (see
//! `super::Vault`), and never crosses IPC back to the UI.

use super::bridge::{self, Bridge, ChatDefaults, Incoming, Outgoing};
use crate::agent::Harness;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const API: &str = "https://discord.com/api/v10";
const UA: &str = "DiscordBot (https://github.com/openleash, 1)";
/// GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT. The last is
/// privileged: the bot owner must switch it on in the Developer Portal, and
/// without it every message arrives with empty content.
pub const INTENTS: u64 = (1 << 0) | (1 << 9) | (1 << 12) | (1 << 15);
/// View Channels, Send Messages, Send Messages in Threads, Read Message
/// History: what the invite link asks for, and nothing more.
pub const INVITE_PERMISSIONS: u64 = (1 << 10) | (1 << 11) | (1 << 38) | (1 << 16);
/// Discord's limit is 2,000 characters; leave room for a reopened code fence.
pub const MESSAGE_LIMIT: usize = 1_900;

/// Everything the user configures. The token is the only secret.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub token: String,
    /// Discord user ids allowed to talk to the agent. Required.
    pub allowed_users: Vec<String>,
    /// Server (guild) ids the bot answers in. Empty: any server it is in.
    pub allowed_guilds: Vec<String>,
    /// Channel ids the bot answers in (a thread counts as its parent channel).
    /// Empty: any channel in an allowed server.
    pub allowed_channels: Vec<String>,
    pub allow_dms: bool,
    /// In a server, answer only when @mentioned or replied to.
    pub require_mention: bool,
    /// Folder new Discord chats work in. Empty: the app's current project.
    pub project: String,
    /// Model for new Discord chats. Empty: the most recently used model.
    pub model: String,
    /// Permission mode for new Discord chats.
    pub perm: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            token: String::new(),
            allowed_users: vec![],
            allowed_guilds: vec![],
            allowed_channels: vec![],
            allow_dms: true,
            require_mention: true,
            project: String::new(),
            model: String::new(),
            // Edits inside the project run; commands ask, and the question is
            // relayed to Discord. Turbo would let a chat message run anything.
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
            .field("allowed_guilds", &self.allowed_guilds)
            .field("allowed_channels", &self.allowed_channels)
            .field("allow_dms", &self.allow_dms)
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
    pub allowed_guilds: Vec<String>,
    pub allowed_channels: Vec<String>,
    pub allow_dms: bool,
    pub require_mention: bool,
    pub project: String,
    pub model: String,
    pub perm: String,
}

/// A Discord id ("snowflake"): 17-20 digits today; a little slack either side.
pub fn snowflake(s: &str) -> bool {
    (15..=21).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn ids(field: &str, list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for raw in list {
        // Accept a mention or channel link pasted as-is: `<@123>`, `<#123>`.
        let id = raw
            .trim()
            .trim_start_matches("<@!")
            .trim_start_matches("<@")
            .trim_start_matches("<#")
            .trim_end_matches('>');
        if id.is_empty() {
            continue;
        }
        if !snowflake(id) {
            return Err(format!("\"{}\" isn't a Discord {field} id. Turn on Developer Mode in Discord (Settings → Advanced), then right-click → Copy ID.", raw.trim()));
        }
        if !out.iter().any(|x| x == id) {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

pub const PERMS: &[&str] = &["disabled", "allowlist", "reviewer", "auto", "turbo"];

impl Config {
    pub fn apply(&mut self, p: Patch) -> Result<(), String> {
        let perm = p.perm.trim();
        if !PERMS.contains(&perm) {
            return Err(format!("Unknown permission mode `{perm}`."));
        }
        self.allowed_users = ids("user", &p.allowed_users)?;
        self.allowed_guilds = ids("server", &p.allowed_guilds)?;
        self.allowed_channels = ids("channel", &p.allowed_channels)?;
        self.enabled = p.enabled;
        self.allow_dms = p.allow_dms;
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
            Some("Add at least one Discord user id. Nobody can talk to the agent until you do.")
        } else {
            None
        }
    }
}

/// Clean up a pasted token. People paste `Bot xyz`, or the token with a
/// trailing newline; the shape check catches a client secret or an app id
/// pasted into the wrong box before Discord does.
pub fn clean_token(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    let t = t.strip_prefix("Bot ").unwrap_or(t).trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let parts: Vec<&str> = t.split('.').collect();
    let ok = parts.len() == 3
        && t.len() >= 50
        && t.len() <= 200
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    if !ok {
        return Err("That doesn't look like a bot token. In the Developer Portal open your app → Bot → Reset Token, and copy the whole value (three parts separated by dots).".into());
    }
    Ok(t.to_string())
}

/// One MESSAGE_CREATE, the fields the gate and the bridge need.
#[derive(Debug, Clone, Default)]
pub struct Msg {
    pub id: String,
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub author_id: String,
    pub author_name: String,
    pub author_bot: bool,
    pub content: String,
    pub mentions_bot: bool,
    pub reply_to_bot: bool,
    pub attachments: Vec<String>,
}

/// Ordinary messages only: 0 is DEFAULT and 19 is REPLY. Joins, pins, boosts
/// and other system messages are not something anyone said to the agent.
pub fn parse(d: &Value, bot_id: &str) -> Option<Msg> {
    let kind = d["type"].as_u64().unwrap_or(0);
    if kind != 0 && kind != 19 {
        return None;
    }
    let a = &d["author"];
    let content = d["content"].as_str().unwrap_or("").to_string();
    let mentions_bot = !bot_id.is_empty()
        && (d["mentions"]
            .as_array()
            .is_some_and(|m| m.iter().any(|u| u["id"].as_str() == Some(bot_id)))
            || content.contains(&format!("<@{bot_id}>"))
            || content.contains(&format!("<@!{bot_id}>")));
    Some(Msg {
        id: d["id"].as_str()?.to_string(),
        channel_id: d["channel_id"].as_str()?.to_string(),
        guild_id: d["guild_id"].as_str().map(String::from),
        author_id: a["id"].as_str()?.to_string(),
        author_name: a["global_name"]
            .as_str()
            .or(a["username"].as_str())
            .unwrap_or("someone")
            .to_string(),
        author_bot: a["bot"].as_bool().unwrap_or(false) || a["system"].as_bool().unwrap_or(false),
        mentions_bot,
        reply_to_bot: !bot_id.is_empty()
            && d["referenced_message"]["author"]["id"].as_str() == Some(bot_id),
        attachments: d["attachments"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| {
                        Some(format!(
                            "{} — {}",
                            bridge::sanitize(x["filename"].as_str()?, 120),
                            x["url"].as_str()?
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        content,
    })
}

/// Why a message was not handed to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignore {
    Bot,
    NoUsers,
    User,
    Dms,
    Guild,
    Channel,
    Mention,
}

impl Ignore {
    pub fn says(self) -> &'static str {
        match self {
            Ignore::Bot => "from a bot",
            Ignore::NoUsers => "no allowed users are set",
            Ignore::User => "the author isn't in your allowed users",
            Ignore::Dms => "direct messages are turned off",
            Ignore::Guild => "that server isn't in your allowed servers",
            Ignore::Channel => "that channel isn't in your allowed channels",
            Ignore::Mention => "the bot wasn't mentioned",
        }
    }
}

/// Who may talk to the agent. `parent` is the parent channel when the message
/// is in a thread (looked up only when the channel list needs it).
///
/// The order matters: identity first, so a stranger is refused for being a
/// stranger wherever they wrote; then place; then the mention rule, which is
/// politeness rather than security and so comes last.
pub fn gate(cfg: &Config, m: &Msg, bot_id: &str, parent: Option<&str>) -> Result<(), Ignore> {
    if m.author_bot || m.author_id == bot_id {
        return Err(Ignore::Bot);
    }
    if cfg.allowed_users.is_empty() {
        return Err(Ignore::NoUsers);
    }
    if !cfg.allowed_users.iter().any(|u| u == &m.author_id) {
        return Err(Ignore::User);
    }
    let Some(guild) = &m.guild_id else {
        return if cfg.allow_dms {
            Ok(())
        } else {
            Err(Ignore::Dms)
        };
    };
    if !cfg.allowed_guilds.is_empty() && !cfg.allowed_guilds.iter().any(|g| g == guild) {
        return Err(Ignore::Guild);
    }
    if !cfg.allowed_channels.is_empty()
        && !cfg
            .allowed_channels
            .iter()
            .any(|c| c == &m.channel_id || Some(c.as_str()) == parent)
    {
        return Err(Ignore::Channel);
    }
    if cfg.require_mention && !m.mentions_bot && !m.reply_to_bot {
        return Err(Ignore::Mention);
    }
    Ok(())
}

/// The message without the bot's own mention.
pub fn strip_mention(content: &str, bot_id: &str) -> String {
    content
        .replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "")
        .trim()
        .to_string()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Split a reply into Discord-sized messages, on line boundaries where it
/// can. A code block cut in two is closed at the end of one message and
/// reopened (same language) at the start of the next, so neither half
/// renders as broken Markdown.
pub fn split(text: &str, limit: usize) -> Vec<String> {
    let mut chunks: Vec<String> = vec![];
    let mut cur: Vec<String> = vec![];
    let mut len = 0usize;
    let mut fence: Option<String> = None;
    let flush = |cur: &mut Vec<String>,
                 len: &mut usize,
                 chunks: &mut Vec<String>,
                 fence: &Option<String>| {
        if cur.is_empty() {
            return;
        }
        if fence.is_some() {
            cur.push("```".into());
        }
        let s = cur.join("\n");
        if !s.trim().is_empty() {
            chunks.push(s);
        }
        cur.clear();
        *len = 0;
        if let Some(f) = fence {
            cur.push(f.clone());
            *len = chars(f) + 1;
        }
    };
    for line in text.split('\n') {
        // A single line longer than a message is cut by characters.
        let pieces: Vec<String> = if chars(line) > limit - 16 {
            line.chars()
                .collect::<Vec<_>>()
                .chunks(limit - 16)
                .map(|c| c.iter().collect())
                .collect()
        } else {
            vec![line.to_string()]
        };
        for piece in pieces {
            let reserve = if fence.is_some() { 4 } else { 0 };
            if !cur.is_empty() && len + chars(&piece) + 1 + reserve > limit {
                flush(&mut cur, &mut len, &mut chunks, &fence);
            }
            len += chars(&piece) + 1;
            cur.push(piece);
        }
        if line.trim_start().starts_with("```") {
            fence = if fence.is_some() {
                None
            } else {
                Some(line.trim().to_string())
            };
        }
    }
    // The text's own fence, if left open, stays open: closing it here would be
    // editing what the agent wrote.
    flush(&mut cur, &mut len, &mut chunks, &None);
    chunks
}

/// What the Settings page shows. No token: only whether one is set.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub has_token: bool,
    pub allowed_users: Vec<String>,
    pub allowed_guilds: Vec<String>,
    pub allowed_channels: Vec<String>,
    pub allow_dms: bool,
    pub require_mention: bool,
    pub project: String,
    pub model: String,
    pub perm: String,
    /// off | incomplete | connecting | connected | reconnecting | error
    pub state: String,
    pub error: String,
    pub bot_name: String,
    pub bot_id: String,
    pub invite_url: String,
    pub servers: usize,
    pub chats: usize,
    /// The last message the gate turned away, and why: the quickest way to see
    /// that an id was copied wrong. Ids only, never message text.
    pub last_ignored: String,
}

#[derive(Default, Clone)]
struct Live {
    state: String,
    error: String,
    bot_id: String,
    bot_name: String,
    app_id: String,
    guilds: HashMap<String, String>,
    last_ignored: String,
}

#[derive(Clone)]
struct Channel {
    name: String,
    parent: Option<String>,
}

pub struct Discord {
    http: reqwest::Client,
    cfg: Mutex<Config>,
    live: Mutex<Live>,
    channels: Mutex<HashMap<String, Channel>>,
    run: Mutex<Option<CancellationToken>>,
    pub bridge: Arc<Bridge>,
}

enum End {
    Cancelled,
    Retry(String),
    Fatal(String),
}

struct Resume {
    session: String,
    url: String,
    seq: Option<u64>,
}

impl Discord {
    pub fn new(h: Arc<Harness>, cfg: Config) -> Arc<Discord> {
        let (tx, rx) = mpsc::unbounded_channel();
        let bridge = Bridge::new(
            h,
            tx,
            crate::agent::store::data_dir().join("discord-chats.json"),
        );
        let me = Arc::new(Discord {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
            cfg: Mutex::new(cfg),
            live: Mutex::new(Live::default()),
            channels: Mutex::new(HashMap::new()),
            run: Mutex::new(None),
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
        let app = if l.app_id.is_empty() {
            &l.bot_id
        } else {
            &l.app_id
        };
        Status {
            enabled: c.enabled,
            has_token: !c.token.is_empty(),
            allowed_users: c.allowed_users.clone(),
            allowed_guilds: c.allowed_guilds.clone(),
            allowed_channels: c.allowed_channels.clone(),
            allow_dms: c.allow_dms,
            require_mention: c.require_mention,
            project: c.project.clone(),
            model: c.model.clone(),
            perm: c.perm.clone(),
            state: if l.state.is_empty() {
                "off".into()
            } else {
                l.state.clone()
            },
            error: l.error.clone(),
            bot_name: l.bot_name.clone(),
            bot_id: l.bot_id.clone(),
            invite_url: if app.is_empty() {
                String::new()
            } else {
                format!("https://discord.com/oauth2/authorize?client_id={app}&scope=bot&permissions={INVITE_PERMISSIONS}")
            },
            servers: l.guilds.len(),
            chats: self.bridge.chat_count().await,
            last_ignored: l.last_ignored.clone(),
        }
    }

    /// (Re)start or stop the bot to match the config. Cheap, and safe to call
    /// after every change: the old connection is cancelled first.
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

    /// Reconnect forever (with backoff) until cancelled or told the config is
    /// wrong. A config error stops the loop: retrying a bad token only gets the
    /// bot rate-limited.
    async fn connect(self: Arc<Self>, cancel: CancellationToken) {
        let mut backoff = Duration::from_secs(1);
        let mut resume: Option<Resume> = None;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let mut ready = false;
            match self.session(&cancel, &mut resume, &mut ready).await {
                End::Cancelled => return,
                End::Fatal(e) => {
                    eprintln!("[openleash] discord: {e}");
                    self.set_live(|l| {
                        l.state = "error".into();
                        l.error = e;
                    });
                    return;
                }
                End::Retry(e) => {
                    if ready {
                        backoff = Duration::from_secs(1);
                    }
                    self.set_live(|l| {
                        l.state = "reconnecting".into();
                        l.error = e;
                    });
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    async fn gateway_url(&self, token: &str) -> Result<String, End> {
        let r = self
            .http
            .get(format!("{API}/gateway/bot"))
            .header("Authorization", format!("Bot {token}"))
            .header("User-Agent", UA)
            .send()
            .await
            .map_err(|e| End::Retry(format!("Can't reach Discord: {e}")))?;
        match r.status().as_u16() {
            200 => {}
            401 => return Err(End::Fatal("Discord rejected the bot token. Paste a fresh one (Developer Portal → your app → Bot → Reset Token).".into())),
            s => return Err(End::Retry(format!("Discord answered {s} to the gateway request."))),
        }
        let v: Value = r.json().await.map_err(|e| End::Retry(e.to_string()))?;
        Ok(v["url"]
            .as_str()
            .unwrap_or("wss://gateway.discord.gg")
            .to_string())
    }

    async fn session(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        resume: &mut Option<Resume>,
        ready: &mut bool,
    ) -> End {
        let token = self.config().token;
        let base = match resume {
            Some(r) => r.url.clone(),
            None => match self.gateway_url(&token).await {
                Ok(u) => u,
                Err(e) => return e,
            },
        };
        let url = format!("{}/?v=10&encoding=json", base.trim_end_matches('/'));
        let (mut ws, _) = match tokio_tungstenite::connect_async(&url).await {
            Ok(x) => x,
            Err(e) => return End::Retry(format!("Can't connect to the Discord gateway: {e}")),
        };
        // HELLO carries the heartbeat interval.
        let interval = loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return End::Cancelled,
                m = tokio::time::timeout(Duration::from_secs(20), ws.next()) => m,
            };
            match next {
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                    if v["op"] == 10 {
                        break v["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250);
                    }
                }
                Ok(Some(Ok(_))) => {}
                _ => return End::Retry("The gateway closed before saying hello.".into()),
            }
        };
        let hello = match resume {
            Some(r) => {
                json!({"op": 6, "d": {"token": token, "session_id": r.session, "seq": r.seq}})
            }
            None => json!({"op": 2, "d": {
                "token": token,
                "intents": INTENTS,
                "properties": {"os": std::env::consts::OS, "browser": "openleash", "device": "openleash"}
            }}),
        };
        if ws
            .send(Message::Text(hello.to_string().into()))
            .await
            .is_err()
        {
            return End::Retry("The gateway dropped the connection.".into());
        }
        // The first beat is jittered so a fleet reconnecting at once spreads
        // out, which Discord asks for. uuid's randomness is enough for that.
        let jitter = (uuid::Uuid::new_v4().as_u128() % 1000) as u64;
        let mut beat = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(interval * jitter / 1000),
            Duration::from_millis(interval),
        );
        let mut acked = true;
        let mut seq: Option<u64> = resume.as_ref().and_then(|r| r.seq);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = ws.close(None).await;
                    return End::Cancelled;
                }
                _ = beat.tick() => {
                    // No ACK since the last beat: the connection is a zombie.
                    if !acked {
                        let _ = ws.close(None).await;
                        return End::Retry("Discord stopped answering heartbeats.".into());
                    }
                    acked = false;
                    if ws.send(Message::Text(json!({"op": 1, "d": seq}).to_string().into())).await.is_err() {
                        return End::Retry("The gateway dropped the connection.".into());
                    }
                }
                m = ws.next() => {
                    let text = match m {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(frame))) => {
                            let code = frame.map(|f| u16::from(f.code)).unwrap_or(1000);
                            return self.closed(code, resume);
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => return End::Retry(format!("Gateway error: {e}")),
                        None => return End::Retry("The gateway closed the connection.".into()),
                    };
                    let v: Value = serde_json::from_str(text.as_str()).unwrap_or(Value::Null);
                    if let Some(s) = v["s"].as_u64() {
                        seq = Some(s);
                        if let Some(r) = resume.as_mut() {
                            r.seq = Some(s);
                        }
                    }
                    match v["op"].as_u64() {
                        Some(0) => {
                            let t = v["t"].as_str().unwrap_or("");
                            let d = v["d"].clone();
                            match t {
                                "READY" => {
                                    *ready = true;
                                    *resume = Some(Resume {
                                        session: d["session_id"].as_str().unwrap_or("").to_string(),
                                        url: d["resume_gateway_url"].as_str().unwrap_or(&base).to_string(),
                                        seq,
                                    });
                                    self.set_live(|l| {
                                        l.state = "connected".into();
                                        l.error.clear();
                                        l.bot_id = d["user"]["id"].as_str().unwrap_or("").to_string();
                                        l.bot_name = d["user"]["username"].as_str().unwrap_or("").to_string();
                                        l.app_id = d["application"]["id"].as_str().unwrap_or("").to_string();
                                        l.guilds = d["guilds"].as_array().map(|g| g.iter().filter_map(|x| Some((x["id"].as_str()?.to_string(), String::new()))).collect()).unwrap_or_default();
                                    });
                                }
                                "RESUMED" => {
                                    *ready = true;
                                    self.set_live(|l| {
                                        l.state = "connected".into();
                                        l.error.clear();
                                    });
                                }
                                "GUILD_CREATE" | "GUILD_UPDATE" => {
                                    if let (Some(id), Some(name)) = (d["id"].as_str(), d["name"].as_str()) {
                                        let (id, name) = (id.to_string(), name.to_string());
                                        self.set_live(|l| { l.guilds.insert(id, name); });
                                    }
                                }
                                "GUILD_DELETE" => {
                                    if let Some(id) = d["id"].as_str() {
                                        let id = id.to_string();
                                        self.set_live(|l| { l.guilds.remove(&id); });
                                    }
                                }
                                "MESSAGE_CREATE" => self.inbound(d),
                                _ => {}
                            }
                        }
                        Some(1) => {
                            let _ = ws.send(Message::Text(json!({"op": 1, "d": seq}).to_string().into())).await;
                        }
                        Some(7) => {
                            let _ = ws.close(None).await;
                            return End::Retry("Discord asked the bot to reconnect.".into());
                        }
                        Some(9) => {
                            if v["d"].as_bool() != Some(true) {
                                *resume = None;
                            }
                            // Discord asks for a 1-5 second wait before identifying again.
                            tokio::time::sleep(Duration::from_millis(1000 + (uuid::Uuid::new_v4().as_u128() % 4000) as u64)).await;
                            return End::Retry("Discord ended the session; starting a new one.".into());
                        }
                        Some(11) => acked = true,
                        _ => {}
                    }
                }
            }
        }
    }

    fn closed(&self, code: u16, resume: &mut Option<Resume>) -> End {
        match code {
            4004 => End::Fatal("Discord rejected the bot token. Paste a fresh one (Developer Portal → your app → Bot → Reset Token).".into()),
            4014 => End::Fatal("Turn on Message Content Intent for this bot: Developer Portal → your app → Bot → Privileged Gateway Intents. Without it the bot can't read messages.".into()),
            4010..=4013 => End::Fatal(format!("Discord refused the connection (code {code}).")),
            4007 | 4009 => {
                *resume = None;
                End::Retry("The Discord session expired; starting a new one.".into())
            }
            _ => End::Retry(format!("The gateway closed the connection (code {code}).")),
        }
    }

    /// A message arrived. Gate it here (cheap, synchronous) and hand the work
    /// to its own task, so a slow chat creation never stalls the heartbeat.
    fn inbound(self: &Arc<Self>, d: Value) {
        let (bot_id, cfg) = {
            let l = self.live.lock().unwrap_or_else(|e| e.into_inner());
            (l.bot_id.clone(), self.config())
        };
        let Some(m) = parse(&d, &bot_id) else { return };
        let verdict = gate(&cfg, &m, &bot_id, None);
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let verdict = match verdict {
                // A thread is allowed when its parent channel is: look the
                // parent up only when that is the one thing standing in the way.
                Err(Ignore::Channel) if m.guild_id.is_some() => {
                    let parent = me.channel(&m.channel_id).await.and_then(|c| c.parent);
                    gate(&cfg, &m, &bot_id, parent.as_deref())
                }
                v => v,
            };
            if let Err(why) = verdict {
                if why != Ignore::Bot && why != Ignore::Mention {
                    me.set_live(|l| {
                        l.last_ignored = format!(
                            "Ignored a message from user {} in channel {}: {}.",
                            m.author_id,
                            m.channel_id,
                            why.says()
                        );
                    });
                }
                return;
            }
            let place = match &m.guild_id {
                None => "a direct message".to_string(),
                Some(g) => {
                    let chan = me
                        .channel(&m.channel_id)
                        .await
                        .map(|c| c.name)
                        .unwrap_or_default();
                    let server = me
                        .live
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .guilds
                        .get(g)
                        .cloned()
                        .unwrap_or_default();
                    let chan = if chan.is_empty() {
                        format!("channel {}", m.channel_id)
                    } else {
                        format!("#{}", bridge::sanitize(&chan, 80))
                    };
                    if server.is_empty() {
                        chan
                    } else {
                        format!("{chan} in {}", bridge::sanitize(&server, 80))
                    }
                }
            };
            let defaults = ChatDefaults {
                platform: "Discord",
                project: cfg.project.clone(),
                model: cfg.model.clone(),
                perm: cfg.perm.clone(),
            };
            let incoming = Incoming {
                conv: m.channel_id.clone(),
                message_id: m.id.clone(),
                author: bridge::sanitize(&m.author_name, 64),
                author_id: m.author_id.clone(),
                place,
                text: strip_mention(&m.content, &bot_id),
                attachments: m.attachments.clone(),
            };
            me.bridge.handle(incoming, defaults).await;
        });
    }

    async fn channel(&self, id: &str) -> Option<Channel> {
        if let Some(c) = self
            .channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            return Some(c.clone());
        }
        let v = self
            .rest(reqwest::Method::GET, &format!("/channels/{id}"), None)
            .await
            .ok()?;
        let c = Channel {
            name: v["name"].as_str().unwrap_or("").to_string(),
            // Only a thread's parent counts: a text channel's `parent_id` is
            // its category, and listing a category must not admit every
            // channel under it.
            parent: matches!(v["type"].as_u64(), Some(10..=12))
                .then(|| v["parent_id"].as_str().map(String::from))
                .flatten(),
        };
        let mut cache = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 2000 {
            cache.clear();
        }
        cache.insert(id.to_string(), c.clone());
        Some(c)
    }

    /// One REST call, waiting out Discord's rate limits (a 429 says how long).
    async fn rest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let token = self.config().token;
        if token.is_empty() {
            return Err("no token".into());
        }
        for _ in 0..4 {
            let mut req = self
                .http
                .request(method.clone(), format!("{API}{path}"))
                .header("Authorization", format!("Bot {token}"))
                .header("User-Agent", UA);
            if let Some(b) = &body {
                req = req.json(b);
            }
            let r = req.send().await.map_err(|e| e.to_string())?;
            let status = r.status().as_u16();
            if status == 429 {
                let v: Value = r.json().await.unwrap_or(Value::Null);
                let wait = v["retry_after"].as_f64().unwrap_or(1.0).clamp(0.05, 30.0);
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                continue;
            }
            if status == 204 {
                return Ok(Value::Null);
            }
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                return Ok(v);
            }
            return Err(format!(
                "Discord answered {status}: {}",
                v["message"].as_str().unwrap_or("")
            ));
        }
        Err("Discord kept rate-limiting the request.".into())
    }

    /// Drain the bridge's queue onto Discord, in order.
    async fn deliver(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Outgoing>) {
        while let Some(o) = rx.recv().await {
            match o {
                Outgoing::Typing { conv } => {
                    let _ = self
                        .rest(
                            reqwest::Method::POST,
                            &format!("/channels/{conv}/typing"),
                            None,
                        )
                        .await;
                }
                Outgoing::Post {
                    conv,
                    text,
                    reply_to,
                } => {
                    for (i, chunk) in split(&text, MESSAGE_LIMIT).into_iter().enumerate() {
                        let mut body = json!({
                            "content": chunk,
                            // Never ping anyone: an agent reply that says
                            // "@everyone" must not page a whole server.
                            "allowed_mentions": {"parse": []},
                        });
                        if let (0, Some(id)) = (i, &reply_to) {
                            body["message_reference"] =
                                json!({"message_id": id, "fail_if_not_exists": false});
                        }
                        if let Err(e) = self
                            .rest(
                                reqwest::Method::POST,
                                &format!("/channels/{conv}/messages"),
                                Some(body),
                            )
                            .await
                        {
                            eprintln!("[openleash] discord: couldn't post to channel {conv}: {e}");
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

    const BOT: &str = "900000000000000001";
    const ME: &str = "100000000000000001";

    fn cfg() -> Config {
        Config {
            allowed_users: vec![ME.into()],
            ..Config::default()
        }
    }

    fn dm(author: &str) -> Msg {
        Msg {
            id: "1".into(),
            channel_id: "300000000000000001".into(),
            author_id: author.into(),
            content: "hi".into(),
            ..Msg::default()
        }
    }

    fn in_guild(author: &str, channel: &str, mentioned: bool) -> Msg {
        Msg {
            guild_id: Some("200000000000000001".into()),
            channel_id: channel.into(),
            mentions_bot: mentioned,
            ..dm(author)
        }
    }

    #[test]
    fn nobody_gets_in_without_an_allowed_user_list() {
        let open = Config::default();
        assert_eq!(gate(&open, &dm(ME), BOT, None), Err(Ignore::NoUsers));
        assert_eq!(
            gate(&open, &in_guild(ME, "1", true), BOT, None),
            Err(Ignore::NoUsers)
        );
        assert!(open.missing().is_some(), "the bot doesn't even connect");
    }

    #[test]
    fn only_listed_users_reach_the_agent() {
        let c = cfg();
        assert_eq!(gate(&c, &dm(ME), BOT, None), Ok(()));
        assert_eq!(
            gate(&c, &dm("100000000000000002"), BOT, None),
            Err(Ignore::User)
        );
        // A stranger is refused for being a stranger, even where a listed
        // user would also have been refused for another reason.
        let mut no_dm = cfg();
        no_dm.allow_dms = false;
        assert_eq!(
            gate(&no_dm, &dm("100000000000000002"), BOT, None),
            Err(Ignore::User)
        );
        assert_eq!(gate(&no_dm, &dm(ME), BOT, None), Err(Ignore::Dms));
    }

    #[test]
    fn bots_are_never_answered_including_this_one() {
        let mut c = cfg();
        c.allowed_users.push(BOT.into());
        let mut m = dm(ME);
        m.author_bot = true;
        assert_eq!(gate(&c, &m, BOT, None), Err(Ignore::Bot));
        assert_eq!(
            gate(&c, &dm(BOT), BOT, None),
            Err(Ignore::Bot),
            "even if its own id was listed"
        );
    }

    #[test]
    fn servers_and_channels_narrow_and_threads_follow_their_parent() {
        let mut c = cfg();
        c.require_mention = false;
        assert_eq!(
            gate(&c, &in_guild(ME, "400000000000000001", false), BOT, None),
            Ok(())
        );
        c.allowed_guilds = vec!["200000000000000009".into()];
        assert_eq!(
            gate(&c, &in_guild(ME, "400000000000000001", false), BOT, None),
            Err(Ignore::Guild)
        );
        c.allowed_guilds = vec!["200000000000000001".into()];
        c.allowed_channels = vec!["400000000000000001".into()];
        assert_eq!(
            gate(&c, &in_guild(ME, "400000000000000001", false), BOT, None),
            Ok(())
        );
        let thread = in_guild(ME, "500000000000000001", false);
        assert_eq!(gate(&c, &thread, BOT, None), Err(Ignore::Channel));
        assert_eq!(gate(&c, &thread, BOT, Some("400000000000000001")), Ok(()));
        assert_eq!(
            gate(&c, &thread, BOT, Some("400000000000000002")),
            Err(Ignore::Channel)
        );
        // Channel limits never apply to DMs; `allow_dms` governs those.
        assert_eq!(gate(&c, &dm(ME), BOT, None), Ok(()));
    }

    #[test]
    fn a_mention_or_a_reply_is_needed_in_servers_by_default() {
        let c = cfg();
        assert_eq!(
            gate(&c, &in_guild(ME, "1", false), BOT, None),
            Err(Ignore::Mention)
        );
        assert_eq!(gate(&c, &in_guild(ME, "1", true), BOT, None), Ok(()));
        let mut reply = in_guild(ME, "1", false);
        reply.reply_to_bot = true;
        assert_eq!(gate(&c, &reply, BOT, None), Ok(()));
        assert_eq!(gate(&c, &dm(ME), BOT, None), Ok(()), "DMs never need one");
    }

    #[test]
    fn messages_parse_from_gateway_payloads() {
        let d = json!({
            "id": "11", "channel_id": "22", "guild_id": "33", "type": 19,
            "author": {"id": ME, "username": "alice", "global_name": "Alice"},
            "content": format!("<@{BOT}> fix the build"),
            "mentions": [{"id": BOT}],
            "referenced_message": {"author": {"id": BOT}},
            "attachments": [{"filename": "log.txt", "url": "https://cdn.discordapp.com/x/log.txt"}]
        });
        let m = parse(&d, BOT).unwrap();
        assert!(m.mentions_bot && m.reply_to_bot && !m.author_bot);
        assert_eq!(m.author_name, "Alice");
        assert_eq!(strip_mention(&m.content, BOT), "fix the build");
        assert_eq!(
            m.attachments,
            vec!["log.txt — https://cdn.discordapp.com/x/log.txt".to_string()]
        );
        // System messages (joins, pins) are not someone talking to the agent.
        assert!(parse(
            &json!({"id": "1", "channel_id": "2", "type": 7, "author": {"id": ME}}),
            BOT
        )
        .is_none());
    }

    #[test]
    fn ids_are_validated_and_pasted_mentions_accepted() {
        let mut c = Config::default();
        let p = |users: Vec<&str>| Patch {
            enabled: true,
            allowed_users: users.into_iter().map(String::from).collect(),
            allowed_guilds: vec![],
            allowed_channels: vec![],
            allow_dms: true,
            require_mention: true,
            project: String::new(),
            model: String::new(),
            perm: "auto".into(),
        };
        c.apply(p(vec![
            &format!("<@{ME}>"),
            ME,
            " ",
            "<@!100000000000000002>",
        ]))
        .unwrap();
        assert_eq!(
            c.allowed_users,
            vec![ME.to_string(), "100000000000000002".into()]
        );
        assert!(c.apply(p(vec!["alice"])).is_err());
        assert!(c.apply(p(vec!["12345"])).is_err());
        let mut bad = p(vec![ME]);
        bad.perm = "root".into();
        assert!(
            c.apply(bad).is_err(),
            "an unknown permission mode is refused, not stored"
        );
    }

    #[test]
    fn tokens_are_cleaned_and_shape_checked() {
        let t = format!("{}.{}.{}", "M".repeat(26), "G".repeat(6), "x".repeat(38));
        assert_eq!(clean_token(&format!("  Bot {t}\n")).unwrap(), t);
        assert_eq!(clean_token("").unwrap(), "");
        assert!(
            clean_token("123456789012345678").is_err(),
            "an application id is not a token"
        );
        assert!(clean_token("not a token at all, really not").is_err());
    }

    #[test]
    fn the_token_never_reaches_debug_output() {
        let c = Config {
            token: "secret.token.value".into(),
            ..Config::default()
        };
        let s = format!("{c:?}");
        assert!(
            !s.contains("secret.token.value") && s.contains("<redacted>"),
            "{s}"
        );
    }

    #[test]
    fn long_replies_split_on_lines_and_keep_code_blocks_whole() {
        let mut text = String::from("Intro line\n```rust\n");
        for i in 0..400 {
            text.push_str(&format!("let x{i} = {i};\n"));
        }
        text.push_str("```\nOutro");
        let parts = split(&text, MESSAGE_LIMIT);
        assert!(parts.len() > 1);
        for p in &parts {
            assert!(
                p.chars().count() <= MESSAGE_LIMIT,
                "{} chars",
                p.chars().count()
            );
            assert_eq!(
                p.matches("```").count() % 2,
                0,
                "every chunk has balanced fences:\n{p}"
            );
        }
        assert!(
            parts[1].starts_with("```rust"),
            "the next chunk reopens with the language"
        );
        let joined = parts.join("\n");
        for i in [0, 199, 399] {
            assert!(joined.contains(&format!("let x{i} = {i};")));
        }
        assert!(joined.ends_with("Outro"));
        // One enormous line is cut by characters, on char boundaries.
        let long: String = "é".repeat(5000);
        let parts = split(&long, MESSAGE_LIMIT);
        assert!(parts.iter().all(|p| p.chars().count() <= MESSAGE_LIMIT));
        assert_eq!(parts.concat().chars().count(), 5000);
        assert!(split("   \n  ", MESSAGE_LIMIT).is_empty());
    }
}
