//! Slack: a Slack app the user owns, talking to their OpenLeash agent.
//!
//! The app connects over Socket Mode: `apps.connections.open` (with the
//! app-level `xapp-…` token) hands out a one-time WebSocket URL, events arrive
//! on it, and replies go out through the Web API with the bot `xoxb-…` token.
//! Socket Mode needs no public URL, which is the point: the agent runs on a
//! desktop behind NAT. No Slack SDK: the protocol is "ack every envelope by
//! id, reconnect when told to", and `tokio-tungstenite` and `reqwest` are
//! already in the tree.
//!
//! The security boundary is [`gate`], the same shape as Discord's: a message
//! reaches the agent only if its author is on the user's list of Slack member
//! ids, and an empty list lets nobody in (the bot doesn't even connect).
//! Workspaces, channels and DMs narrow that further; bots (including this one)
//! are never answered, so two agents can't loop each other.
//!
//! Every Slack thread is one conversation. In a channel, a top-level mention
//! starts a thread and the reply goes into it; later replies in that thread
//! continue the same chat. That keeps one channel from collapsing every
//! request into a single chat, and keeps the agent's replies out of the
//! channel's main timeline.
//!
//! Both tokens live in `connectors.json`, not `settings.json` (see
//! `super::Vault`), never cross IPC back to the UI, and are scrubbed from every
//! error message ([`redact`]) before it reaches the status line or a log.

use super::bridge::{self, Bridge, ChatDefaults, Incoming, Outgoing};
use super::discord;
use crate::agent::Harness;
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const API: &str = "https://slack.com/api";
/// Slack accepts up to 40,000 characters but recommends 4,000 and folds long
/// messages behind "Show more"; leave room for a reopened code fence.
pub const MESSAGE_LIMIT: usize = 3_900;
/// Message subtypes that are still a person talking: a message with a file
/// attached, and a thread reply also sent to the channel. Every other subtype
/// (edits, deletions, joins, topic changes, bot messages, `/me`) is not
/// something anyone said to the agent, and an edit must not re-run a request.
const PERSON_SUBTYPES: &[&str] = &["file_share", "thread_broadcast"];

/// Everything the user configures. The two tokens are the only secrets.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// App-level token (`xapp-…`, connections:write) for Socket Mode.
    pub app_token: String,
    /// Bot token (`xoxb-…`) for the Web API.
    pub bot_token: String,
    /// Slack member ids (`U…`/`W…`) allowed to talk to the agent. Required.
    pub allowed_users: Vec<String>,
    /// Workspace (team) ids (`T…`). Empty: any workspace the app is in.
    pub allowed_teams: Vec<String>,
    /// Channel ids (`C…`/`G…`). Empty: any channel the app is in.
    pub allowed_channels: Vec<String>,
    pub allow_dms: bool,
    /// In a channel, answer only when @mentioned (or in a thread it is in).
    pub require_mention: bool,
    /// Folder new Slack chats work in. Empty: the app's current project.
    pub project: String,
    /// Model for new Slack chats. Empty: the most recently used model.
    pub model: String,
    /// Permission mode for new Slack chats.
    pub perm: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            app_token: String::new(),
            bot_token: String::new(),
            allowed_users: vec![],
            allowed_teams: vec![],
            allowed_channels: vec![],
            allow_dms: true,
            require_mention: true,
            project: String::new(),
            model: String::new(),
            // Edits inside the project run; commands ask, and the question is
            // relayed to Slack. Turbo would let a chat message run anything.
            perm: "auto".into(),
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let r = |t: &str| if t.is_empty() { "" } else { "<redacted>" };
        f.debug_struct("Config")
            .field("enabled", &self.enabled)
            .field("app_token", &r(&self.app_token))
            .field("bot_token", &r(&self.bot_token))
            .field("allowed_users", &self.allowed_users)
            .field("allowed_teams", &self.allowed_teams)
            .field("allowed_channels", &self.allowed_channels)
            .field("allow_dms", &self.allow_dms)
            .field("require_mention", &self.require_mention)
            .field("project", &self.project)
            .field("model", &self.model)
            .field("perm", &self.perm)
            .finish()
    }
}

/// What the UI may change: everything but the tokens, which have their own
/// command so that a settings form can never echo them back.
#[derive(Debug, Clone, Deserialize)]
pub struct Patch {
    pub enabled: bool,
    pub allowed_users: Vec<String>,
    pub allowed_teams: Vec<String>,
    pub allowed_channels: Vec<String>,
    pub allow_dms: bool,
    pub require_mention: bool,
    pub project: String,
    pub model: String,
    pub perm: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum IdKind {
    User,
    Team,
    Channel,
}

/// A Slack id: an uppercase type letter, then uppercase letters and digits.
/// Users are `U…` (or `W…` on Enterprise Grid), workspaces `T…`, channels
/// `C…`, private channels `G…` (older ones) and DMs `D…`.
fn slack_id(kind: IdKind, s: &str) -> bool {
    let (first, min_len): (&[u8], usize) = match kind {
        IdKind::User => (&b"UW"[..], 3),
        IdKind::Team => (&b"T"[..], 2),
        IdKind::Channel => (&b"CGD"[..], 2),
    };
    let b = s.as_bytes();
    (min_len..=24).contains(&b.len())
        && first.contains(&b[0])
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn ids(kind: IdKind, list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for raw in list {
        // Accept a mention or channel link pasted as-is: `<@U123>`,
        // `<@U123|alice>`, `<#C123|general>`.
        let t = raw.trim();
        let t = t
            .strip_prefix("<@")
            .or_else(|| t.strip_prefix("<#"))
            .unwrap_or(t);
        let t = t.trim_end_matches('>');
        let id = t.split('|').next().unwrap_or("").trim();
        if id.is_empty() {
            continue;
        }
        if !slack_id(kind, id) {
            return Err(match kind {
                IdKind::User => format!("\"{}\" isn't a Slack member id. In Slack open the person's profile → ⋮ (More) → Copy member ID. It starts with U (or W).", raw.trim()),
                IdKind::Team => format!("\"{}\" isn't a Slack workspace id. It starts with T: open Slack in a browser and copy it from the address (app.slack.com/client/T…), or leave this empty.", raw.trim()),
                IdKind::Channel => format!("\"{}\" isn't a Slack channel id. Open the channel, click its name at the top, and copy the Channel ID at the bottom of the About tab. It starts with C (or G).", raw.trim()),
            });
        }
        if !out.iter().any(|x| x == id) {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

impl Config {
    pub fn apply(&mut self, p: Patch) -> Result<(), String> {
        let perm = p.perm.trim();
        if !discord::PERMS.contains(&perm) {
            return Err(format!("Unknown permission mode `{perm}`."));
        }
        self.allowed_users = ids(IdKind::User, &p.allowed_users)?;
        self.allowed_teams = ids(IdKind::Team, &p.allowed_teams)?;
        self.allowed_channels = ids(IdKind::Channel, &p.allowed_channels)?;
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
        if self.app_token.is_empty() {
            Some("Add the app-level token (xapp-…).")
        } else if self.bot_token.is_empty() {
            Some("Add the bot token (xoxb-…).")
        } else if self.allowed_users.is_empty() {
            Some("Add at least one Slack user id. Nobody can talk to the agent until you do.")
        } else {
            None
        }
    }
}

/// Which of the two tokens a `slack_set_token` call sets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenKind {
    App,
    Bot,
}

impl TokenKind {
    pub fn parse(s: &str) -> Result<TokenKind, String> {
        match s {
            "app" => Ok(TokenKind::App),
            "bot" => Ok(TokenKind::Bot),
            _ => Err(format!("Unknown Slack token kind `{s}`.")),
        }
    }

    fn label(self) -> &'static str {
        match self {
            TokenKind::App => "app-level token",
            TokenKind::Bot => "bot token",
        }
    }
}

/// Clean up a pasted token of the given kind; empty means "remove". The two
/// boxes sit next to each other, so the likeliest mistake is pasting one
/// token into the other's box, or the user token (`xoxp-`, which acts as the
/// person rather than the bot) from the same OAuth page. Each gets its own
/// explanation. Errors never repeat the pasted value.
pub fn clean_token(kind: TokenKind, raw: &str) -> Result<String, String> {
    let t = raw.trim();
    let t = t.strip_prefix("Bearer ").unwrap_or(t).trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let (want, other) = match kind {
        TokenKind::App => ("xapp-", "xoxb-"),
        TokenKind::Bot => ("xoxb-", "xapp-"),
    };
    if !t.starts_with(want) {
        return Err(if t.starts_with(other) {
            match kind {
                TokenKind::App => "That's the bot token (xoxb-…). This box takes the app-level token (xapp-…): Basic Information → App-Level Tokens.".into(),
                TokenKind::Bot => "That's the app-level token (xapp-…). This box takes the bot token (xoxb-…): OAuth & Permissions → Bot User OAuth Token.".into(),
            }
        } else if t.starts_with("xoxp-") {
            "That's a user token (xoxp-…), which acts as you rather than the bot. Use the Bot User OAuth Token (xoxb-…) from OAuth & Permissions.".into()
        } else if t.starts_with("xoxe") {
            "That's a refreshable token from token rotation, which this connector doesn't support. Turn token rotation off in your app's OAuth & Permissions page and reinstall the app.".into()
        } else {
            match kind {
                TokenKind::App => "That doesn't look like an app-level token. In your Slack app open Basic Information → App-Level Tokens, generate one with the connections:write scope, and copy the value that starts with xapp-.".into(),
                TokenKind::Bot => "That doesn't look like a bot token. In your Slack app open OAuth & Permissions and copy the Bot User OAuth Token that starts with xoxb-.".into(),
            }
        });
    }
    let ok =
        (24..=300).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !ok {
        return Err(format!(
            "That {} looks cut short or has stray characters in it. Copy it again with the copy button next to it in your app's settings.",
            kind.label()
        ));
    }
    Ok(t.to_string())
}

static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(xox[a-z]|xapp)-[A-Za-z0-9-]*").expect("regex"));
static WSS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"wss?://\S+").expect("regex"));
static TICKET_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ticket=[^&\s]+").expect("regex"));

/// Scrub secrets from a message headed for the status line or a log. Tokens
/// travel only in headers, but an error type can echo a request; and the
/// Socket Mode URL carries a ticket that is a short-lived credential of its
/// own, so a WebSocket URL is never shown at all.
pub fn redact(s: &str) -> String {
    let s = WSS_RE.replace_all(s, "<socket url>");
    let s = TICKET_RE.replace_all(&s, "ticket=<redacted>");
    TOKEN_RE.replace_all(&s, "$1-<redacted>").into_owned()
}

/// One Socket Mode frame, as far as the connection cares.
#[derive(Debug, Clone, PartialEq)]
pub enum Envelope {
    Hello,
    /// Slack is about to close this connection: `warning`,
    /// `refresh_requested` or `link_disabled`.
    Disconnect(String),
    /// An Events API delivery: the id to ack, and the event callback.
    Event {
        id: String,
        payload: Value,
    },
    /// Slash commands, interactivity and anything newer: acked so Slack stops
    /// retrying, otherwise ignored. The manifest subscribes to none of them.
    Other {
        id: String,
    },
    Unknown,
}

pub fn envelope(text: &str) -> Envelope {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let id = v["envelope_id"].as_str().map(String::from);
    match (v["type"].as_str().unwrap_or(""), id) {
        ("hello", _) => Envelope::Hello,
        ("disconnect", _) => Envelope::Disconnect(v["reason"].as_str().unwrap_or("").to_string()),
        ("events_api", Some(id)) => Envelope::Event {
            id,
            payload: v["payload"].clone(),
        },
        (_, Some(id)) => Envelope::Other { id },
        _ => Envelope::Unknown,
    }
}

/// The acknowledgement Slack wants within three seconds of every envelope, or
/// it delivers the event again (and eventually to another connection).
pub fn ack(id: &str) -> String {
    json!({ "envelope_id": id }).to_string()
}

/// One message event, the fields the gate and the bridge need.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Msg {
    /// The workspace the event was delivered for (`team_id` on the callback).
    pub team: String,
    /// The author's own workspace, when Slack says (it differs in channels
    /// shared with another organisation).
    pub user_team: String,
    pub channel: String,
    /// A one-to-one DM with the bot (`channel_type` `im`).
    pub dm: bool,
    /// A group DM (`mpim`). Treated as a channel: several people read it.
    pub group_dm: bool,
    pub user: String,
    pub bot: bool,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub text: String,
    pub mentions_bot: bool,
    pub files: Vec<String>,
}

impl Msg {
    /// A reply inside a thread (not the message that started it).
    pub fn in_thread(&self) -> bool {
        self.thread_ts.as_deref().is_some_and(|t| t != self.ts)
    }
}

/// An `event_callback` carrying `message` or `app_mention`; `None` for
/// everything else, including system and edited messages (see
/// [`PERSON_SUBTYPES`]).
pub fn parse(payload: &Value, bot_user: &str) -> Option<Msg> {
    if payload["type"].as_str() != Some("event_callback") {
        return None;
    }
    let e = &payload["event"];
    let kind = e["type"].as_str()?;
    if kind != "message" && kind != "app_mention" {
        return None;
    }
    if let Some(sub) = e["subtype"].as_str() {
        if !PERSON_SUBTYPES.contains(&sub) {
            return None;
        }
    }
    if e["hidden"].as_bool() == Some(true) {
        return None;
    }
    let channel = e["channel"].as_str()?.to_string();
    // `app_mention` carries no channel_type; a DM's id starts with D.
    let ctype = e["channel_type"]
        .as_str()
        .unwrap_or(if channel.starts_with('D') {
            "im"
        } else {
            "channel"
        });
    let text = e["text"].as_str().unwrap_or("").to_string();
    let mentions_bot = kind == "app_mention"
        || (!bot_user.is_empty()
            && (text.contains(&format!("<@{bot_user}>"))
                || text.contains(&format!("<@{bot_user}|"))));
    Some(Msg {
        team: payload["team_id"].as_str().unwrap_or("").to_string(),
        user_team: e["user_team"]
            .as_str()
            .or(e["team"].as_str())
            .unwrap_or("")
            .to_string(),
        dm: ctype == "im",
        group_dm: ctype == "mpim",
        user: e["user"].as_str()?.to_string(),
        bot: e["bot_id"].is_string() || e["bot_profile"].is_object(),
        ts: e["ts"].as_str()?.to_string(),
        thread_ts: e["thread_ts"].as_str().map(String::from),
        files: e["files"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|f| f["name"].as_str().or(f["title"].as_str()))
                    // The file stays in Slack: downloading needs the bot token,
                    // and a private URL would be useless to the agent anyway.
                    .map(|n| {
                        format!(
                            "{} (a file in Slack, not downloaded)",
                            bridge::sanitize(n, 120)
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        channel,
        mentions_bot,
        text,
    })
}

/// The bridge conversation a message belongs to. A thread is one
/// conversation (`channel:thread_ts`); in a channel a top-level message starts
/// its own thread (`channel:ts`), so the reply goes under it; a plain DM is
/// one long conversation (`channel`).
pub fn conv_of(m: &Msg) -> String {
    match (&m.thread_ts, m.dm) {
        (Some(t), _) => format!("{}:{t}", m.channel),
        (None, true) => m.channel.clone(),
        (None, false) => format!("{}:{}", m.channel, m.ts),
    }
}

/// Where a conversation's replies go: the channel, and the thread if any.
pub fn target(conv: &str) -> (&str, Option<&str>) {
    match conv.split_once(':') {
        Some((c, t)) => (c, Some(t)),
        None => (conv, None),
    }
}

/// Why a message was not handed to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignore {
    Bot,
    NoUsers,
    User,
    Team,
    Dms,
    Channel,
    Mention,
}

impl Ignore {
    pub fn says(self) -> &'static str {
        match self {
            Ignore::Bot => "from a bot",
            Ignore::NoUsers => "no allowed users are set",
            Ignore::User => "the author isn't in your allowed users",
            Ignore::Team => "that workspace isn't in your allowed workspaces",
            Ignore::Dms => "direct messages are turned off",
            Ignore::Channel => "that channel isn't in your allowed channels",
            Ignore::Mention => "the bot wasn't mentioned",
        }
    }
}

/// Who may talk to the agent. `known_thread`: the message's thread is one the
/// bot is already talking in, which stands in for a mention (nobody wants to
/// @-mention the bot on every reply of a conversation they started with it).
///
/// The order matters: identity first, so a stranger is refused for being a
/// stranger wherever they wrote; then place; then the mention rule, which is
/// politeness rather than security and so comes last.
pub fn gate(cfg: &Config, m: &Msg, bot_user: &str, known_thread: bool) -> Result<(), Ignore> {
    if m.bot || (!bot_user.is_empty() && m.user == bot_user) {
        return Err(Ignore::Bot);
    }
    if cfg.allowed_users.is_empty() {
        return Err(Ignore::NoUsers);
    }
    if !cfg.allowed_users.iter().any(|u| u == &m.user) {
        return Err(Ignore::User);
    }
    // Both the workspace the event came through and the author's home
    // workspace must be listed: in a channel shared with another organisation
    // they differ, and listing one must not admit the other.
    if !cfg.allowed_teams.is_empty()
        && [&m.team, &m.user_team]
            .iter()
            .any(|t| !t.is_empty() && !cfg.allowed_teams.iter().any(|a| a == *t))
    {
        return Err(Ignore::Team);
    }
    if !cfg.allowed_teams.is_empty() && m.team.is_empty() && m.user_team.is_empty() {
        return Err(Ignore::Team);
    }
    if m.dm {
        return if cfg.allow_dms {
            Ok(())
        } else {
            Err(Ignore::Dms)
        };
    }
    if !cfg.allowed_channels.is_empty() && !cfg.allowed_channels.iter().any(|c| c == &m.channel) {
        return Err(Ignore::Channel);
    }
    if cfg.require_mention && !m.mentions_bot && !(known_thread && m.in_thread()) {
        return Err(Ignore::Mention);
    }
    Ok(())
}

/// Messages already handled, by `channel:ts`. A channel mention arrives twice
/// (as `message` and as `app_mention`), and Slack redelivers an event whose
/// ack was late; either copy may come first, so the first one wins.
pub struct Seen {
    order: VecDeque<String>,
    set: HashSet<String>,
    cap: usize,
}

impl Seen {
    pub fn new(cap: usize) -> Seen {
        Seen {
            order: VecDeque::new(),
            set: HashSet::new(),
            cap,
        }
    }

    /// True the first time `key` is seen (within the last `cap` keys).
    pub fn first(&mut self, key: &str) -> bool {
        if self.set.contains(key) {
            return false;
        }
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        self.order.push_back(key.to_string());
        self.set.insert(key.to_string());
        true
    }
}

static ANGLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>]*)>").expect("regex"));

/// A message's text as the person meant it: Slack's `<@U1|name>`, `<#C1|x>`
/// and `<https://…|label>` markup turned back into words, the bot's own
/// mention dropped, and `&amp;` `&lt;` `&gt;` unescaped (Slack escapes those
/// three in everything it sends).
pub fn inbound_text(text: &str, bot_user: &str) -> String {
    let out = ANGLE_RE.replace_all(text, |c: &regex::Captures| {
        let inner = &c[1];
        let (id, label) = match inner.split_once('|') {
            Some((a, b)) => (a, Some(b)),
            None => (inner, None),
        };
        if let Some(u) = id.strip_prefix('@') {
            if !bot_user.is_empty() && u == bot_user {
                return String::new();
            }
            return format!("@{}", label.unwrap_or(u));
        }
        if let Some(ch) = id.strip_prefix('#') {
            return format!("#{}", label.unwrap_or(ch));
        }
        if let Some(special) = id.strip_prefix('!') {
            return match label {
                Some(l) => l.to_string(),
                None => format!("@{}", special.split('^').next().unwrap_or(special)),
            };
        }
        match label {
            Some(l) if l != id && !id.ends_with(l) => format!("{l} ({id})"),
            Some(l) => l.to_string(),
            None => id.to_string(),
        }
    });
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// Slack's three control characters, escaped. With `<` gone the agent can't
/// write `<!channel>`, `<!here>` or `<@U…>`: nothing it says pages anyone.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

static BOLD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*(\S(?:.*?\S)?)\*\*").expect("regex"));
static STRIKE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~~(\S(?:.*?\S)?)~~").expect("regex"));
static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{1,6}\s+(.+?)\s*#*$").expect("regex"));
// Only http(s) targets become links, and `|` can't appear in one: a link is
// the one piece of `<…>` markup this writes, so it must not be bendable into
// `<!channel>` or a user mention.
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\[\]]+)\]\((https?://[^\s()|]+)\)").expect("regex"));

/// The agent's Markdown as Slack mrkdwn: escaped first (see [`escape`]), then
/// `**bold**` → `*bold*`, `~~x~~` → `~x~`, headings → bold lines and
/// `[label](https://…)` → `<https://…|label>`. Code blocks are only escaped,
/// and lose their language tag, which Slack would print as the first line.
pub fn to_mrkdwn(md: &str) -> String {
    let mut out: Vec<String> = vec![];
    let mut in_code = false;
    for line in md.split('\n') {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("```") {
            in_code = !in_code;
            let indent = &line[..line.len() - t.len()];
            // A fence with text after it on the same line (```code```) is
            // passed through escaped.
            if in_code && rest.contains("```") {
                in_code = false;
                out.push(escape(line));
            } else {
                out.push(format!("{indent}```"));
            }
            continue;
        }
        if in_code {
            out.push(escape(line));
            continue;
        }
        let mut l = escape(line);
        if let Some(c) = HEADING_RE.captures(&l) {
            l = format!("*{}*", c[1].trim_matches('*'));
        }
        l = BOLD_RE.replace_all(&l, "*$1*").into_owned();
        l = STRIKE_RE.replace_all(&l, "~$1~").into_owned();
        l = LINK_RE.replace_all(&l, "<$2|$1>").into_owned();
        out.push(l);
    }
    out.join("\n")
}

/// A `chat.postMessage` body. `parse: none` and `link_names: false` so Slack
/// never turns a plain "@channel" into a ping either.
pub fn post_body(channel: &str, thread: Option<&str>, text: &str) -> Value {
    let mut b = json!({
        "channel": channel,
        "text": text,
        "mrkdwn": true,
        "parse": "none",
        "link_names": false,
        "unfurl_links": false,
        "unfurl_media": false,
    });
    if let Some(t) = thread {
        b["thread_ts"] = json!(t);
    }
    b
}

/// The app id inside a Socket Mode URL (`…?ticket=…&app_id=A…`).
pub fn app_id_of(url: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("app_id="))
        .filter(|a| !a.is_empty())
        .map(String::from)
}

/// What a failed Web API call means for the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiErr {
    /// The configuration is wrong: retrying would only get the app limited.
    Fatal(String),
    /// Rate-limited: wait this long first.
    Wait(Duration),
    /// Probably transient.
    Retry(String),
}

impl ApiErr {
    pub fn text(&self) -> String {
        match self {
            ApiErr::Fatal(m) | ApiErr::Retry(m) => m.clone(),
            ApiErr::Wait(d) => format!(
                "Slack is rate-limiting the app; try again in {}s.",
                d.as_secs().max(1)
            ),
        }
    }
}

/// A Slack error code, safe to show: it is chosen by the server, so keep only
/// what an error code can contain.
fn code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':' || *c == ',')
        .take(80)
        .collect()
}

/// Read one Web API response. Slack answers most failures with HTTP 200 and
/// `{"ok": false, "error": "…"}`; rate limits come as HTTP 429 with a
/// `Retry-After` header (seconds).
pub fn classify(
    which: TokenKind,
    status: u16,
    retry_after: Option<&str>,
    body: &Value,
) -> Result<(), ApiErr> {
    let wait = || {
        let s = retry_after
            .and_then(|r| r.trim().parse::<u64>().ok())
            .unwrap_or(1);
        ApiErr::Wait(Duration::from_secs(s.clamp(1, 300)))
    };
    if status == 429 {
        return Err(wait());
    }
    if body["ok"].as_bool() == Some(true) {
        return Ok(());
    }
    if status >= 500 || !body.is_object() {
        return Err(ApiErr::Retry(format!("Slack answered {status}.")));
    }
    let err = code(body["error"].as_str().unwrap_or("unknown_error"));
    let fix = match which {
        TokenKind::Bot => "Paste a fresh Bot User OAuth Token (xoxb-…) from your app's OAuth & Permissions page. Reinstalling the app replaces the old one.",
        TokenKind::App => "Generate a new app-level token under Basic Information → App-Level Tokens, with the connections:write scope.",
    };
    match err.as_str() {
        "invalid_auth" | "not_authed" | "account_inactive" | "token_revoked" | "token_expired" => Err(ApiErr::Fatal(format!(
            "Slack rejected the {} ({err}). {fix}",
            which.label()
        ))),
        "not_allowed_token_type" => Err(ApiErr::Fatal(format!(
            "Slack says that {} is the wrong kind of token. {fix}",
            which.label()
        ))),
        "missing_scope" => Err(ApiErr::Fatal(format!(
            "The {} is missing the `{}` scope. Add it in your app's settings (or recreate the app from the manifest) and reinstall it to the workspace.",
            which.label(),
            code(body["needed"].as_str().unwrap_or("?"))
        ))),
        "ratelimited" => Err(wait()),
        _ => Err(ApiErr::Retry(format!("Slack said `{err}`."))),
    }
}

/// How the connection ended, and so what happens next.
#[derive(Debug, Clone, PartialEq)]
enum End {
    Cancelled,
    /// Something broke: try again after the backoff.
    Retry(String),
    /// Slack asked for a new connection (a routine refresh): reconnect now.
    Refresh,
    /// Rate-limited: try again after this long.
    Wait(Duration, String),
    /// The configuration is wrong: stop until the user changes it.
    Fatal(String),
}

impl From<ApiErr> for End {
    fn from(e: ApiErr) -> End {
        match e {
            ApiErr::Fatal(m) => End::Fatal(m),
            ApiErr::Retry(m) => End::Retry(m),
            ApiErr::Wait(d) => End::Wait(d, ApiErr::Wait(d).text()),
        }
    }
}

/// A `disconnect` envelope. Slack refreshes every connection every few hours
/// (`warning` comes a few seconds before `refresh_requested`); reconnecting on
/// either loses nothing, because events not acked here are redelivered.
/// `link_disabled` means Socket Mode was switched off for the app, which no
/// amount of reconnecting fixes.
fn on_disconnect(reason: &str) -> End {
    match reason {
        "link_disabled" => End::Fatal("Slack turned Socket Mode off for this app (link_disabled). Switch it back on in your app's settings → Socket Mode, then switch the bot off and on here.".into()),
        _ => End::Refresh,
    }
}

const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// After a connection ends: how long to sleep before the next one, and the
/// backoff after that; `None` stops the loop.
fn plan(end: &End, backoff: Duration) -> Option<(Duration, Duration)> {
    match end {
        End::Cancelled | End::Fatal(_) => None,
        End::Refresh => Some((Duration::ZERO, backoff)),
        End::Wait(d, _) => Some((*d, backoff)),
        End::Retry(_) => Some((backoff, (backoff * 2).min(MAX_BACKOFF))),
    }
}

/// What the Settings page shows. No tokens: only whether each is set.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub has_app_token: bool,
    pub has_bot_token: bool,
    pub allowed_users: Vec<String>,
    pub allowed_teams: Vec<String>,
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
    /// The bot's member id (`U…`): what a mention of it looks like.
    pub bot_id: String,
    pub team_name: String,
    pub chats: usize,
    /// The last message the gate turned away, and why: the quickest way to see
    /// that an id was copied wrong. Ids only, never message text.
    pub last_ignored: String,
}

#[derive(Default, Clone)]
struct Live {
    state: String,
    error: String,
    bot_user: String,
    bot_name: String,
    team_name: String,
    last_ignored: String,
}

pub struct Slack {
    http: reqwest::Client,
    cfg: Mutex<Config>,
    live: Mutex<Live>,
    seen: Mutex<Seen>,
    /// Conversations the bot has talked in (from the links file at start, plus
    /// every one handed to the bridge since): a reply in one of these threads
    /// needs no mention. Over-remembering is harmless; the gate still checks
    /// who wrote it.
    threads: Mutex<HashSet<String>>,
    users: Mutex<HashMap<String, String>>,
    channels: Mutex<HashMap<String, String>>,
    run: Mutex<Option<CancellationToken>>,
    pub bridge: Arc<Bridge>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Slack {
    pub fn new(h: Arc<Harness>, cfg: Config) -> Arc<Slack> {
        let (tx, rx) = mpsc::unbounded_channel();
        let links = crate::agent::store::data_dir().join("slack-chats.json");
        let threads: HashSet<String> = std::fs::read_to_string(&links)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
            .map(|m| m.into_keys().collect())
            .unwrap_or_default();
        let bridge = Bridge::new(h, tx, links);
        let me = Arc::new(Slack {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
            cfg: Mutex::new(cfg),
            live: Mutex::new(Live::default()),
            seen: Mutex::new(Seen::new(512)),
            threads: Mutex::new(threads),
            users: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
            run: Mutex::new(None),
            bridge,
        });
        let sender = me.clone();
        tauri::async_runtime::spawn(async move { sender.deliver(rx).await });
        me
    }

    pub fn config(&self) -> Config {
        lock(&self.cfg).clone()
    }

    pub fn set_config(&self, cfg: Config) {
        *lock(&self.cfg) = cfg;
    }

    fn set_live(&self, f: impl FnOnce(&mut Live)) {
        f(&mut lock(&self.live));
    }

    pub async fn status(&self) -> Status {
        let c = self.config();
        let l = lock(&self.live).clone();
        Status {
            enabled: c.enabled,
            has_app_token: !c.app_token.is_empty(),
            has_bot_token: !c.bot_token.is_empty(),
            allowed_users: c.allowed_users,
            allowed_teams: c.allowed_teams,
            allowed_channels: c.allowed_channels,
            allow_dms: c.allow_dms,
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
            bot_id: l.bot_user,
            team_name: l.team_name,
            chats: self.bridge.chat_count().await,
            last_ignored: l.last_ignored,
        }
    }

    /// (Re)start or stop the bot to match the config. Cheap, and safe to call
    /// after every change: the old connection is cancelled first.
    pub fn apply(self: &Arc<Self>) {
        if let Some(c) = lock(&self.run).take() {
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
        *lock(&self.run) = Some(token.clone());
        self.set_live(|l| {
            l.state = "connecting".into();
            l.error.clear();
        });
        let me = self.clone();
        tauri::async_runtime::spawn(async move { me.connect(token).await });
    }

    pub fn shutdown(&self) {
        if let Some(c) = lock(&self.run).take() {
            c.cancel();
        }
    }

    /// Check the saved tokens against Slack (`auth.test` with the bot token,
    /// `apps.connections.open` with the app token) without starting the bot.
    /// Ok: a short human line. Errors never contain a token.
    pub async fn test(&self) -> Result<String, String> {
        let c = self.config();
        if c.bot_token.is_empty() {
            return Err("Add the bot token (xoxb-…) first.".into());
        }
        if c.app_token.is_empty() {
            return Err("Add the app-level token (xapp-…) first.".into());
        }
        let who = self
            .call(TokenKind::Bot, &c.bot_token, "auth.test", &[])
            .await
            .map_err(|e| e.text())?;
        // The URL is a one-time ticket; it is never connected to or shown.
        let url = self.socket_url(&c.app_token).await.map_err(|e| e.text())?;
        // Two tokens from two different apps both "work" but the bot would
        // never hear anything: catch that here rather than in silence later.
        if let (Some(app), Some(bot)) = (app_id_of(&url), who["bot_id"].as_str()) {
            if let Ok(info) = self
                .call(TokenKind::Bot, &c.bot_token, "bots.info", &[("bot", bot)])
                .await
            {
                if let Some(other) = info["bot"]["app_id"].as_str() {
                    if other != app {
                        return Err("The two tokens belong to different Slack apps. Take both from the same app: the bot token from OAuth & Permissions, the app-level token from Basic Information.".into());
                    }
                }
            }
        }
        Ok(format!(
            "Tokens work: @{} in {}.",
            bridge::sanitize(who["user"].as_str().unwrap_or("bot"), 64),
            bridge::sanitize(who["team"].as_str().unwrap_or("your workspace"), 80)
        ))
    }

    /// Reconnect forever (with backoff) until cancelled or told the config is
    /// wrong. A config error stops the loop: retrying a bad token only gets the
    /// app rate-limited.
    async fn connect(self: Arc<Self>, cancel: CancellationToken) {
        let mut backoff = Duration::from_secs(1);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let mut ready = false;
            let end = self.session(&cancel, &mut ready).await;
            if ready {
                backoff = Duration::from_secs(1);
            }
            match &end {
                End::Cancelled => return,
                End::Fatal(e) => {
                    let e = redact(e);
                    eprintln!("[openleash] slack: {e}");
                    self.set_live(|l| {
                        l.state = "error".into();
                        l.error = e;
                    });
                }
                End::Refresh => self.set_live(|l| {
                    l.state = "reconnecting".into();
                    l.error.clear();
                }),
                End::Retry(e) | End::Wait(_, e) => {
                    let e = redact(e);
                    self.set_live(|l| {
                        l.state = "reconnecting".into();
                        l.error = e;
                    });
                }
            }
            let Some((sleep, next)) = plan(&end, backoff) else {
                return;
            };
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(sleep) => {}
            }
            backoff = next;
        }
    }

    /// One Web API call with a form body (the read methods don't take JSON),
    /// waiting out short rate limits.
    async fn call(
        &self,
        which: TokenKind,
        token: &str,
        method: &str,
        form: &[(&str, &str)],
    ) -> Result<Value, ApiErr> {
        self.request(which, token, method, |r| r.form(form)).await
    }

    async fn request(
        &self,
        which: TokenKind,
        token: &str,
        method: &str,
        body: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<Value, ApiErr> {
        if token.is_empty() {
            return Err(ApiErr::Fatal(format!("Add the {} first.", which.label())));
        }
        let mut last = ApiErr::Retry("Slack kept rate-limiting the request.".into());
        for _ in 0..3 {
            let req = self.http.post(format!("{API}/{method}")).bearer_auth(token);
            let r = body(req).send().await.map_err(|e| {
                ApiErr::Retry(redact(&format!("Can't reach Slack: {}", e.without_url())))
            })?;
            let status = r.status().as_u16();
            let retry_after = r
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let v: Value = r.json().await.unwrap_or(Value::Null);
            match classify(which, status, retry_after.as_deref(), &v) {
                Ok(()) => return Ok(v),
                Err(ApiErr::Wait(d)) if d <= Duration::from_secs(30) => {
                    last = ApiErr::Wait(d);
                    tokio::time::sleep(d).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// A fresh Socket Mode URL. Each is good for one connection.
    async fn socket_url(&self, app_token: &str) -> Result<String, ApiErr> {
        let v = self
            .call(TokenKind::App, app_token, "apps.connections.open", &[])
            .await?;
        match v["url"].as_str() {
            Some(u) if u.starts_with("wss://") => Ok(u.to_string()),
            _ => Err(ApiErr::Retry(
                "Slack didn't return a Socket Mode address.".into(),
            )),
        }
    }

    async fn session(self: &Arc<Self>, cancel: &CancellationToken, ready: &mut bool) -> End {
        let cfg = self.config();
        let who = tokio::select! {
            _ = cancel.cancelled() => return End::Cancelled,
            r = self.call(TokenKind::Bot, &cfg.bot_token, "auth.test", &[]) => r,
        };
        let who = match who {
            Ok(v) => v,
            Err(e) => return e.into(),
        };
        self.set_live(|l| {
            l.bot_user = who["user_id"].as_str().unwrap_or("").to_string();
            l.bot_name = who["user"].as_str().unwrap_or("").to_string();
            l.team_name = who["team"].as_str().unwrap_or("").to_string();
        });
        let url = tokio::select! {
            _ = cancel.cancelled() => return End::Cancelled,
            r = self.socket_url(&cfg.app_token) => r,
        };
        let url = match url {
            Ok(u) => u,
            Err(e) => return e.into(),
        };
        let conn = tokio::select! {
            _ = cancel.cancelled() => return End::Cancelled,
            c = tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(url.as_str())) => c,
        };
        let mut ws = match conn {
            Ok(Ok((ws, _))) => ws,
            // The error can quote the URL, and the URL holds the ticket.
            Ok(Err(e)) => return End::Retry(redact(&format!("Can't connect to Slack: {e}"))),
            Err(_) => return End::Retry("Connecting to Slack timed out.".into()),
        };
        drop(url);
        // `hello` says the connection is live.
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return End::Cancelled,
                m = tokio::time::timeout(Duration::from_secs(20), ws.next()) => m,
            };
            match next {
                Ok(Some(Ok(Message::Text(t)))) => match envelope(t.as_str()) {
                    Envelope::Hello => break,
                    Envelope::Disconnect(r) => return on_disconnect(&r),
                    _ => {}
                },
                Ok(Some(Ok(_))) => {}
                _ => return End::Retry("Slack closed the connection before saying hello.".into()),
            }
        }
        *ready = true;
        self.set_live(|l| {
            l.state = "connected".into();
            l.error.clear();
        });
        // Slack pings us, but a half-open TCP connection goes quiet rather
        // than erroring, so ping it too and give up when nothing comes back.
        let mut beat = tokio::time::interval(Duration::from_secs(30));
        beat.tick().await;
        let mut last_rx = Instant::now();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = ws.close(None).await;
                    return End::Cancelled;
                }
                _ = beat.tick() => {
                    if last_rx.elapsed() > Duration::from_secs(90) {
                        let _ = ws.close(None).await;
                        return End::Retry("Slack stopped answering.".into());
                    }
                    if ws.send(Message::Ping(Default::default())).await.is_err() {
                        return End::Retry("The Slack connection dropped.".into());
                    }
                }
                m = ws.next() => {
                    last_rx = Instant::now();
                    let text = match m {
                        Some(Ok(Message::Text(t))) => t,
                        // tungstenite queues the pong itself; flushing sends it.
                        Some(Ok(Message::Ping(_))) => { let _ = ws.flush().await; continue; }
                        Some(Ok(Message::Close(frame))) => {
                            let code = frame.map(|f| u16::from(f.code)).unwrap_or(1000);
                            return End::Retry(format!("Slack closed the connection (code {code})."));
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => return End::Retry(redact(&format!("Slack connection error: {e}"))),
                        None => return End::Retry("Slack closed the connection.".into()),
                    };
                    match envelope(text.as_str()) {
                        Envelope::Hello | Envelope::Unknown => {}
                        Envelope::Disconnect(r) => {
                            let _ = ws.close(None).await;
                            return on_disconnect(&r);
                        }
                        // Ack first, work after: the ack is due in three
                        // seconds, and the work is handed off anyway.
                        Envelope::Event { id, payload } => {
                            if ws.send(Message::Text(ack(&id).into())).await.is_err() {
                                return End::Retry("The Slack connection dropped.".into());
                            }
                            self.inbound(payload);
                        }
                        Envelope::Other { id } => {
                            if ws.send(Message::Text(ack(&id).into())).await.is_err() {
                                return End::Retry("The Slack connection dropped.".into());
                            }
                        }
                    }
                }
            }
        }
    }

    /// An event arrived. Gate it here (cheap, synchronous) and hand the work
    /// to its own task, so a slow chat creation never stalls the socket.
    fn inbound(self: &Arc<Self>, payload: Value) {
        let bot_user = lock(&self.live).bot_user.clone();
        let cfg = self.config();
        let Some(m) = parse(&payload, &bot_user) else {
            return;
        };
        if !lock(&self.seen).first(&format!("{}:{}", m.channel, m.ts)) {
            return;
        }
        let conv = conv_of(&m);
        let known = lock(&self.threads).contains(&conv);
        if let Err(why) = gate(&cfg, &m, &bot_user, known) {
            if why != Ignore::Bot && why != Ignore::Mention {
                self.set_live(|l| {
                    l.last_ignored = format!(
                        "Ignored a message from user {} in channel {}: {}.",
                        code(&m.user),
                        code(&m.channel),
                        why.says()
                    );
                });
            }
            return;
        }
        lock(&self.threads).insert(conv.clone());
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let place = if m.dm {
                "a direct message".to_string()
            } else if m.group_dm {
                "a group direct message".to_string()
            } else {
                let name = me.channel_name(&cfg.bot_token, &m.channel).await;
                let chan = match name {
                    Some(n) => format!("#{}", bridge::sanitize(&n, 80)),
                    None => format!("channel {}", code(&m.channel)),
                };
                let team = lock(&me.live).team_name.clone();
                if team.is_empty() {
                    chan
                } else {
                    format!("{chan} in {}", bridge::sanitize(&team, 80))
                }
            };
            let author = me
                .user_name(&cfg.bot_token, &m.user)
                .await
                .unwrap_or_else(|| m.user.clone());
            let defaults = ChatDefaults {
                platform: "Slack",
                project: cfg.project.clone(),
                model: cfg.model.clone(),
                perm: cfg.perm.clone(),
            };
            let incoming = Incoming {
                conv,
                message_id: m.ts.clone(),
                author: bridge::sanitize(&author, 64),
                author_id: m.user.clone(),
                place,
                text: inbound_text(&m.text, &bot_user),
                attachments: m.files.clone(),
            };
            me.bridge.handle(incoming, defaults).await;
        });
    }

    async fn channel_name(&self, token: &str, id: &str) -> Option<String> {
        if let Some(n) = lock(&self.channels).get(id) {
            return Some(n.clone());
        }
        let v = self
            .call(
                TokenKind::Bot,
                token,
                "conversations.info",
                &[("channel", id)],
            )
            .await
            .ok()?;
        let n = v["channel"]["name"].as_str()?.to_string();
        let mut cache = lock(&self.channels);
        if cache.len() > 2000 {
            cache.clear();
        }
        cache.insert(id.to_string(), n.clone());
        Some(n)
    }

    async fn user_name(&self, token: &str, id: &str) -> Option<String> {
        if let Some(n) = lock(&self.users).get(id) {
            return Some(n.clone());
        }
        let v = self
            .call(TokenKind::Bot, token, "users.info", &[("user", id)])
            .await
            .ok()?;
        let u = &v["user"];
        let n = [
            &u["profile"]["display_name"],
            &u["profile"]["real_name"],
            &u["real_name"],
            &u["name"],
        ]
        .iter()
        .find_map(|x| x.as_str().filter(|s| !s.trim().is_empty()))?
        .to_string();
        let mut cache = lock(&self.users);
        if cache.len() > 2000 {
            cache.clear();
        }
        cache.insert(id.to_string(), n.clone());
        Some(n)
    }

    /// Drain the bridge's queue onto Slack, in order.
    async fn deliver(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Outgoing>) {
        while let Some(o) = rx.recv().await {
            match o {
                // Bots can't show "typing…" over Socket Mode (that was an RTM
                // feature), so there is nothing to send.
                Outgoing::Typing { .. } => {}
                // `reply_to` is not needed: the conversation is the thread,
                // and posting into it is how Slack replies.
                Outgoing::Post { conv, text, .. } => {
                    let token = self.config().bot_token;
                    let (channel, thread) = target(&conv);
                    for chunk in discord::split(&to_mrkdwn(&text), MESSAGE_LIMIT) {
                        let body = post_body(channel, thread, &chunk);
                        if let Err(e) = self
                            .request(TokenKind::Bot, &token, "chat.postMessage", |r| {
                                r.json(&body)
                            })
                            .await
                        {
                            eprintln!(
                                "[openleash] slack: couldn't post to channel {}: {}",
                                code(channel),
                                redact(&e.text())
                            );
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

    const BOT: &str = "U0BOT0001";
    const ME: &str = "U0ME00001";
    const TEAM: &str = "T0TEAM001";

    fn cfg() -> Config {
        Config {
            allowed_users: vec![ME.into()],
            ..Config::default()
        }
    }

    fn dm(user: &str) -> Msg {
        Msg {
            team: TEAM.into(),
            channel: "D0DM00001".into(),
            dm: true,
            user: user.into(),
            ts: "1700000000.000100".into(),
            text: "hi".into(),
            ..Msg::default()
        }
    }

    fn in_channel(user: &str, channel: &str, mentioned: bool) -> Msg {
        Msg {
            channel: channel.into(),
            dm: false,
            mentions_bot: mentioned,
            ..dm(user)
        }
    }

    fn callback(event: Value) -> Value {
        json!({"type": "event_callback", "team_id": TEAM, "event_id": "Ev1", "event": event})
    }

    #[test]
    fn nobody_gets_in_without_an_allowed_user_list() {
        let open = Config::default();
        assert_eq!(gate(&open, &dm(ME), BOT, false), Err(Ignore::NoUsers));
        assert_eq!(
            gate(&open, &in_channel(ME, "C1", true), BOT, true),
            Err(Ignore::NoUsers)
        );
        assert!(open.missing().is_some(), "the bot doesn't even connect");
        let tokens_only = Config {
            app_token: "xapp-x".into(),
            bot_token: "xoxb-x".into(),
            ..Config::default()
        };
        assert!(tokens_only.missing().unwrap().contains("user id"));
    }

    #[test]
    fn only_listed_users_reach_the_agent() {
        let c = cfg();
        assert_eq!(gate(&c, &dm(ME), BOT, false), Ok(()));
        assert_eq!(gate(&c, &dm("U0OTHER01"), BOT, false), Err(Ignore::User));
        // A stranger is refused for being a stranger, even where a listed
        // user would also have been refused for another reason.
        let mut no_dm = cfg();
        no_dm.allow_dms = false;
        assert_eq!(
            gate(&no_dm, &dm("U0OTHER01"), BOT, false),
            Err(Ignore::User)
        );
        assert_eq!(gate(&no_dm, &dm(ME), BOT, false), Err(Ignore::Dms));
    }

    #[test]
    fn bots_are_never_answered_including_this_one() {
        let mut c = cfg();
        c.allowed_users.push(BOT.into());
        let mut m = dm(ME);
        m.bot = true;
        assert_eq!(gate(&c, &m, BOT, false), Err(Ignore::Bot));
        assert_eq!(
            gate(&c, &dm(BOT), BOT, false),
            Err(Ignore::Bot),
            "even if its own id was listed"
        );
    }

    #[test]
    fn workspaces_and_channels_narrow_access() {
        let mut c = cfg();
        c.require_mention = false;
        assert_eq!(
            gate(&c, &in_channel(ME, "C0GEN0001", false), BOT, false),
            Ok(())
        );
        c.allowed_teams = vec!["T0OTHER01".into()];
        assert_eq!(
            gate(&c, &in_channel(ME, "C0GEN0001", false), BOT, false),
            Err(Ignore::Team)
        );
        assert_eq!(gate(&c, &dm(ME), BOT, false), Err(Ignore::Team), "DMs too");
        c.allowed_teams = vec![TEAM.into()];
        // A shared channel: the author's home workspace must be listed too.
        let mut foreign = in_channel(ME, "C0GEN0001", false);
        foreign.user_team = "T0FOREIGN".into();
        assert_eq!(gate(&c, &foreign, BOT, false), Err(Ignore::Team));
        let mut unknown = in_channel(ME, "C0GEN0001", false);
        unknown.team.clear();
        assert_eq!(
            gate(&c, &unknown, BOT, false),
            Err(Ignore::Team),
            "a listed workspace can't be confirmed from an event that names none"
        );
        c.allowed_channels = vec!["C0GEN0001".into()];
        assert_eq!(
            gate(&c, &in_channel(ME, "C0GEN0001", false), BOT, false),
            Ok(())
        );
        assert_eq!(
            gate(&c, &in_channel(ME, "C0RANDOM1", false), BOT, false),
            Err(Ignore::Channel)
        );
        // Channel limits never apply to DMs; `allow_dms` governs those.
        assert_eq!(gate(&c, &dm(ME), BOT, false), Ok(()));
        // A group DM is a channel for these rules.
        let mut group = in_channel(ME, "G0GROUP01", false);
        group.group_dm = true;
        assert_eq!(gate(&c, &group, BOT, false), Err(Ignore::Channel));
    }

    #[test]
    fn a_mention_is_needed_in_channels_except_in_the_bots_own_threads() {
        let c = cfg();
        assert_eq!(
            gate(&c, &in_channel(ME, "C1", false), BOT, false),
            Err(Ignore::Mention)
        );
        assert_eq!(gate(&c, &in_channel(ME, "C1", true), BOT, false), Ok(()));
        assert_eq!(gate(&c, &dm(ME), BOT, false), Ok(()), "DMs never need one");
        let mut reply = in_channel(ME, "C1", false);
        reply.thread_ts = Some("1600000000.000001".into());
        assert_eq!(
            gate(&c, &reply, BOT, true),
            Ok(()),
            "a reply in a thread the bot is in"
        );
        assert_eq!(
            gate(&c, &reply, BOT, false),
            Err(Ignore::Mention),
            "someone else's thread still needs a mention"
        );
        let mut top = in_channel(ME, "C1", false);
        top.thread_ts = Some(top.ts.clone());
        assert_eq!(
            gate(&c, &top, BOT, true),
            Err(Ignore::Mention),
            "a thread's first message isn't a reply in it"
        );
        let mut stranger = reply.clone();
        stranger.user = "U0OTHER01".into();
        assert_eq!(gate(&c, &stranger, BOT, true), Err(Ignore::User));
    }

    #[test]
    fn events_parse_and_system_messages_are_dropped() {
        let msg = callback(json!({
            "type": "message", "channel": "C0GEN0001", "channel_type": "channel",
            "user": ME, "text": format!("<@{BOT}> fix the build"), "ts": "1.2",
            "team": TEAM,
            "files": [{"name": "log.txt", "url_private": "https://files.slack.com/x"}]
        }));
        let m = parse(&msg, BOT).unwrap();
        assert!(m.mentions_bot && !m.dm && !m.bot && !m.group_dm);
        assert_eq!((m.team.as_str(), m.user.as_str()), (TEAM, ME));
        assert_eq!(m.files.len(), 1);
        assert!(m.files[0].starts_with("log.txt") && !m.files[0].contains("https"));
        assert_eq!(inbound_text(&m.text, BOT), "fix the build");
        let mention = callback(json!({
            "type": "app_mention", "channel": "C0GEN0001", "user": ME, "text": "hey", "ts": "1.3"
        }));
        let m = parse(&mention, BOT).unwrap();
        assert!(
            m.mentions_bot && !m.dm,
            "app_mention counts even without the text form"
        );
        let im = callback(
            json!({"type": "message", "channel": "D0DM00001", "channel_type": "im", "user": ME, "text": "hi", "ts": "1.4"}),
        );
        assert!(parse(&im, BOT).unwrap().dm);
        let group = callback(
            json!({"type": "message", "channel": "G0GRP0001", "channel_type": "mpim", "user": ME, "text": "hi", "ts": "1.4"}),
        );
        let g = parse(&group, BOT).unwrap();
        assert!(g.group_dm && !g.dm);
        let bot = callback(
            json!({"type": "message", "channel": "D1", "channel_type": "im", "user": "U0X", "bot_id": "B1", "text": "x", "ts": "1.5"}),
        );
        assert!(parse(&bot, BOT).unwrap().bot);
        for sub in [
            "message_changed",
            "message_deleted",
            "bot_message",
            "channel_join",
            "me_message",
        ] {
            let e = callback(
                json!({"type": "message", "subtype": sub, "channel": "C1", "user": ME, "text": "x", "ts": "1.6"}),
            );
            assert!(parse(&e, BOT).is_none(), "{sub}");
        }
        let share = callback(
            json!({"type": "message", "subtype": "file_share", "channel": "D1", "channel_type": "im", "user": ME, "text": "", "ts": "1.7", "files": [{"name": "a.png"}]}),
        );
        assert!(
            parse(&share, BOT).is_some(),
            "a file someone shared is a message"
        );
        let hidden = callback(
            json!({"type": "message", "hidden": true, "channel": "C1", "user": ME, "ts": "1.8"}),
        );
        assert!(parse(&hidden, BOT).is_none());
        assert!(parse(&json!({"type": "url_verification"}), BOT).is_none());
        assert!(parse(
            &callback(json!({"type": "reaction_added", "user": ME})),
            BOT
        )
        .is_none());
    }

    #[test]
    fn envelopes_parse_and_every_one_with_an_id_is_acked() {
        assert_eq!(
            envelope(r#"{"type":"hello","num_connections":1,"connection_info":{"app_id":"A1"}}"#),
            Envelope::Hello
        );
        assert_eq!(
            envelope(r#"{"type":"disconnect","reason":"refresh_requested"}"#),
            Envelope::Disconnect("refresh_requested".into())
        );
        let e = envelope(
            r#"{"envelope_id":"e-1","type":"events_api","payload":{"type":"event_callback","event":{"type":"message"}},"accepts_response_payload":false}"#,
        );
        match e {
            Envelope::Event { id, payload } => {
                assert_eq!(id, "e-1");
                assert_eq!(payload["event"]["type"], "message");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            envelope(r#"{"envelope_id":"e-2","type":"slash_commands","payload":{}}"#),
            Envelope::Other { id: "e-2".into() }
        );
        assert_eq!(
            envelope(r#"{"envelope_id":"e-3","type":"interactive","payload":{}}"#),
            Envelope::Other { id: "e-3".into() }
        );
        assert_eq!(envelope("not json"), Envelope::Unknown);
        let a: Value = serde_json::from_str(&ack("e-1")).unwrap();
        assert_eq!(a, json!({"envelope_id": "e-1"}));
    }

    #[test]
    fn disconnects_and_errors_decide_what_happens_next() {
        assert_eq!(on_disconnect("refresh_requested"), End::Refresh);
        assert_eq!(on_disconnect("warning"), End::Refresh);
        assert!(
            matches!(on_disconnect("link_disabled"), End::Fatal(m) if m.contains("Socket Mode"))
        );
        let s = Duration::from_secs;
        assert_eq!(plan(&End::Refresh, s(8)), Some((Duration::ZERO, s(8))));
        assert_eq!(plan(&End::Retry("x".into()), s(8)), Some((s(8), s(16))));
        assert_eq!(
            plan(&End::Retry("x".into()), s(60)),
            Some((s(60), s(60))),
            "capped"
        );
        assert_eq!(
            plan(&End::Wait(s(30), "x".into()), s(1)),
            Some((s(30), s(1)))
        );
        assert_eq!(plan(&End::Fatal("x".into()), s(1)), None);
        assert_eq!(plan(&End::Cancelled, s(1)), None);

        let ok = json!({"ok": true});
        assert_eq!(classify(TokenKind::Bot, 200, None, &ok), Ok(()));
        for code in [
            "invalid_auth",
            "not_authed",
            "account_inactive",
            "token_revoked",
        ] {
            let r = classify(
                TokenKind::Bot,
                200,
                None,
                &json!({"ok": false, "error": code}),
            );
            assert!(
                matches!(&r, Err(ApiErr::Fatal(m)) if m.contains("bot token") && m.contains(code)),
                "{r:?}"
            );
        }
        let r = classify(
            TokenKind::App,
            200,
            None,
            &json!({"ok": false, "error": "invalid_auth"}),
        );
        assert!(
            matches!(&r, Err(ApiErr::Fatal(m)) if m.contains("connections:write")),
            "{r:?}"
        );
        let r = classify(
            TokenKind::Bot,
            200,
            None,
            &json!({"ok": false, "error": "missing_scope", "needed": "chat:write"}),
        );
        assert!(
            matches!(&r, Err(ApiErr::Fatal(m)) if m.contains("chat:write")),
            "{r:?}"
        );
        assert_eq!(
            classify(TokenKind::Bot, 429, Some("17"), &Value::Null),
            Err(ApiErr::Wait(s(17)))
        );
        assert_eq!(
            classify(TokenKind::Bot, 429, None, &Value::Null),
            Err(ApiErr::Wait(s(1)))
        );
        assert_eq!(
            classify(
                TokenKind::Bot,
                200,
                Some("5"),
                &json!({"ok": false, "error": "ratelimited"})
            ),
            Err(ApiErr::Wait(s(5)))
        );
        assert!(matches!(
            classify(TokenKind::Bot, 503, None, &Value::Null),
            Err(ApiErr::Retry(_))
        ));
        assert!(matches!(
            classify(TokenKind::Bot, 200, None, &json!({"ok": false, "error": "channel_not_found"})),
            Err(ApiErr::Retry(m)) if m.contains("channel_not_found")
        ));
        // A server-chosen error code can't smuggle markup into the status line.
        let r = classify(
            TokenKind::Bot,
            200,
            None,
            &json!({"ok": false, "error": "<b>x</b> y"}),
        );
        assert_eq!(r, Err(ApiErr::Retry("Slack said `bxby`.".into())));
        assert!(matches!(
            End::from(ApiErr::Fatal("f".into())),
            End::Fatal(_)
        ));
    }

    #[test]
    fn duplicate_deliveries_are_handled_once() {
        let mut seen = Seen::new(3);
        assert!(seen.first("C1:1.1"));
        assert!(!seen.first("C1:1.1"), "the app_mention copy of a message");
        assert!(seen.first("C1:1.2"));
        assert!(seen.first("C1:1.3"));
        assert!(seen.first("C1:1.4"));
        assert!(seen.first("C1:1.1"), "the oldest key was evicted");
        assert_eq!(seen.order.len(), 3);
        assert_eq!(seen.set.len(), 3);
    }

    #[test]
    fn threads_are_conversations() {
        let top = in_channel(ME, "C0GEN0001", true);
        assert_eq!(
            conv_of(&top),
            "C0GEN0001:1700000000.000100",
            "a mention starts a thread"
        );
        let mut reply = top.clone();
        reply.ts = "1700000001.000200".into();
        reply.thread_ts = Some("1700000000.000100".into());
        assert_eq!(
            conv_of(&reply),
            conv_of(&top),
            "its replies continue the same chat"
        );
        assert_eq!(conv_of(&dm(ME)), "D0DM00001", "a DM is one conversation");
        let mut dm_thread = dm(ME);
        dm_thread.thread_ts = Some("1.0".into());
        assert_eq!(conv_of(&dm_thread), "D0DM00001:1.0");
        assert_eq!(target("C1:1.5"), ("C1", Some("1.5")));
        assert_eq!(target("D1"), ("D1", None));
        let b = post_body("C1", Some("1.5"), "hello");
        assert_eq!(b["thread_ts"], "1.5");
        assert_eq!(b["parse"], "none");
        assert_eq!(b["link_names"], false);
        assert!(post_body("D1", None, "x").get("thread_ts").is_none());
    }

    #[test]
    fn ids_are_validated_and_pasted_mentions_accepted() {
        let mut c = Config::default();
        let p = |users: Vec<&str>| Patch {
            enabled: true,
            allowed_users: users.into_iter().map(String::from).collect(),
            allowed_teams: vec![],
            allowed_channels: vec![],
            allow_dms: true,
            require_mention: true,
            project: String::new(),
            model: String::new(),
            perm: "auto".into(),
        };
        c.apply(p(vec![&format!("<@{ME}>"), ME, " ", "<@W0GRID001|alice>"]))
            .unwrap();
        assert_eq!(c.allowed_users, vec![ME.to_string(), "W0GRID001".into()]);
        for bad in ["alice", "u0lower01", "C0GEN0001", "U", "U0ME-0001"] {
            let r = c.apply(p(vec![bad]));
            assert!(
                r.as_ref().is_err_and(|e| e.contains("Copy member ID")),
                "{bad}: {r:?}"
            );
        }
        let mut both = p(vec![ME]);
        both.allowed_teams = vec![TEAM.into()];
        both.allowed_channels = vec!["<#C0GEN0001|general>".into(), "G0PRIV001".into()];
        c.apply(both).unwrap();
        assert_eq!(c.allowed_teams, vec![TEAM.to_string()]);
        assert_eq!(
            c.allowed_channels,
            vec!["C0GEN0001".to_string(), "G0PRIV001".into()]
        );
        let mut bad_team = p(vec![ME]);
        bad_team.allowed_teams = vec!["acme".into()];
        assert!(c.apply(bad_team).is_err());
        let mut bad_chan = p(vec![ME]);
        bad_chan.allowed_channels = vec!["#general".into()];
        assert!(c.apply(bad_chan).unwrap_err().contains("Channel ID"));
        let mut bad = p(vec![ME]);
        bad.perm = "root".into();
        assert!(
            c.apply(bad).is_err(),
            "an unknown permission mode is refused, not stored"
        );
        assert_eq!(c.perm, "auto");
    }

    #[test]
    fn tokens_are_cleaned_and_shape_checked() {
        let app = format!("xapp-1-A0APP0001-{}-{}", "1".repeat(13), "a".repeat(64));
        let bot = format!(
            "xoxb-{}-{}-{}",
            "1".repeat(12),
            "2".repeat(13),
            "B".repeat(24)
        );
        assert_eq!(
            clean_token(TokenKind::App, &format!("  {app}\n")).unwrap(),
            app
        );
        assert_eq!(
            clean_token(TokenKind::Bot, &format!("Bearer {bot}")).unwrap(),
            bot
        );
        assert_eq!(
            clean_token(TokenKind::Bot, "  ").unwrap(),
            "",
            "empty removes"
        );
        let swapped = clean_token(TokenKind::App, &bot).unwrap_err();
        assert!(swapped.contains("That's the bot token"), "{swapped}");
        let swapped = clean_token(TokenKind::Bot, &app).unwrap_err();
        assert!(swapped.contains("That's the app-level token"), "{swapped}");
        let user = clean_token(TokenKind::Bot, &bot.replace("xoxb-", "xoxp-")).unwrap_err();
        assert!(user.contains("user token"), "{user}");
        assert!(clean_token(TokenKind::Bot, "xoxe.xoxb-1-abc")
            .unwrap_err()
            .contains("rotation"));
        assert!(clean_token(TokenKind::Bot, "xoxb-short").is_err());
        assert!(clean_token(TokenKind::Bot, &format!("{bot} extra")).is_err());
        assert!(clean_token(TokenKind::App, "hello").is_err());
        // No error repeats the pasted secret.
        for (k, t) in [
            (TokenKind::App, bot.clone()),
            (TokenKind::Bot, app.clone()),
            (TokenKind::Bot, format!("{bot}!")),
        ] {
            let e = clean_token(k, &t).unwrap_err();
            assert!(!e.contains(&t[5..20]), "{e}");
        }
        assert_eq!(TokenKind::parse("app"), Ok(TokenKind::App));
        assert_eq!(TokenKind::parse("bot"), Ok(TokenKind::Bot));
        assert!(TokenKind::parse("user").is_err());
    }

    #[test]
    fn the_tokens_never_reach_debug_output_or_errors() {
        let c = Config {
            app_token: "xapp-secret-app-value".into(),
            bot_token: "xoxb-secret-bot-value".into(),
            allowed_users: vec![ME.into()],
            ..Config::default()
        };
        let s = format!("{c:?}");
        assert!(
            !s.contains("secret") && s.contains("<redacted>") && s.contains(ME),
            "{s}"
        );
        let e = redact("error sending request for url (wss://wss-primary.slack.com/link/?ticket=abc-123&app_id=A1): Bearer xoxb-123-456-abc and xapp-1-A1-2-xyz");
        assert!(
            !e.contains("abc-123") && !e.contains("xoxb-123") && !e.contains("xyz"),
            "{e}"
        );
        assert!(
            e.contains("<socket url>") && e.contains("xoxb-<redacted>"),
            "{e}"
        );
        assert_eq!(
            redact("https://slack.com/api/x?ticket=t1&a=b"),
            "https://slack.com/api/x?ticket=<redacted>&a=b"
        );
        assert_eq!(
            redact("Slack said `invalid_auth`."),
            "Slack said `invalid_auth`."
        );
        assert_eq!(
            app_id_of("wss://wss-primary.slack.com/link/?ticket=t&app_id=A0APP"),
            Some("A0APP".into())
        );
        assert_eq!(app_id_of("wss://x/link"), None);
    }

    #[test]
    fn replies_are_escaped_so_the_agent_cannot_ping_anyone() {
        let out = to_mrkdwn("Done <!channel> and <!here> cc <@U0ME00001> & <#C1>");
        assert!(!out.contains('<') && !out.contains('>'), "{out}");
        assert!(
            out.contains("&lt;!channel&gt;") && out.contains("&amp;"),
            "{out}"
        );
        // Markdown becomes mrkdwn; a link is the one markup written, and only
        // for http(s) targets.
        assert_eq!(to_mrkdwn("**bold** and ~~gone~~"), "*bold* and ~gone~");
        assert_eq!(to_mrkdwn("## Summary"), "*Summary*");
        assert_eq!(
            to_mrkdwn("see [the docs](https://x.dev/a?b=1&c=2)"),
            "see <https://x.dev/a?b=1&amp;c=2|the docs>"
        );
        let sneaky = to_mrkdwn("[x](!channel) [y](javascript:alert(1)) [z](https://a|<!here>)");
        assert!(!sneaky.contains("<!") && !sneaky.contains("|<"), "{sneaky}");
        // Code is only escaped, and the language tag goes (Slack would print it).
        let code = to_mrkdwn("```rust\nlet a = b**c**d < e;\n```\nafter");
        assert_eq!(code, "```\nlet a = b**c**d &lt; e;\n```\nafter");
        assert_eq!(escape("a<b>&"), "a&lt;b&gt;&amp;");
        // A one-line ```code``` doesn't leave the rest of the reply in a block.
        assert_eq!(to_mrkdwn("```x<y```\n**b**"), "```x&lt;y```\n*b*");
    }

    #[test]
    fn inbound_markup_reads_as_the_person_wrote_it() {
        assert_eq!(
            inbound_text(&format!("<@{BOT}> ask <@U0OTHER01|bob> in <#C0GEN0001|general> about <https://x.dev|the site> &amp; &lt;tags&gt;"), BOT),
            "ask @bob in #general about the site (https://x.dev) & <tags>"
        );
        assert_eq!(
            inbound_text("<!here> look <https://x.dev> <mailto:a@b.c|a@b.c>", BOT),
            "@here look https://x.dev a@b.c"
        );
        assert_eq!(inbound_text(&format!("<@{BOT}|openleash>   "), BOT), "");
    }

    #[test]
    fn long_replies_split_under_the_limit_and_keep_code_blocks_whole() {
        let mut text = String::from("Intro <b>\n```rust\n");
        for i in 0..900 {
            text.push_str(&format!("let x{i} = a < b;\n"));
        }
        text.push_str("```\nOutro");
        let parts = discord::split(&to_mrkdwn(&text), MESSAGE_LIMIT);
        assert!(parts.len() > 1);
        for p in &parts {
            assert!(
                p.chars().count() <= MESSAGE_LIMIT,
                "{} chars",
                p.chars().count()
            );
            assert_eq!(p.matches("```").count() % 2, 0, "balanced fences:\n{p}");
            assert!(!p.contains('<'));
        }
        assert!(
            parts[1].starts_with("```\n"),
            "the next chunk reopens the block"
        );
        assert!(parts.join("\n").ends_with("Outro"));
    }

    #[test]
    fn defaults_are_safe() {
        let c = Config::default();
        assert!(!c.enabled && c.require_mention && c.allow_dms);
        assert_eq!(c.perm, "auto");
        assert!(c.allowed_users.is_empty() && c.missing().is_some());
        let back: Config = serde_json::from_str("{}").unwrap();
        assert!(back.require_mention && back.perm == "auto");
    }
}
