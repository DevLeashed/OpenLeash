//! The Slack plugin: one `slack` tool with an `action` enum.
//!
//! Wired into the harness through `plugin_services.rs` (schema, permissions,
//! dispatch, settings). Off by default; reads run freely, writes ask.
//!
//! This is the *agent tool* (the model reads and posts in Slack on the user's
//! behalf), not the Slack chat connector under `connectors/`.
//!
//! Slack's Web API quirks this module deals with:
//! - every method is `https://slack.com/api/<method>`; reads take query
//!   parameters (built with `Url::query_pairs_mut`, never string concat), writes
//!   take a JSON body with an explicit `charset=utf-8` (without it Slack adds a
//!   `missing_charset` warning and may mis-decode non-ASCII text);
//! - failures come back as HTTP 200 with `{"ok":false,"error":"…"}`, so the
//!   `ok` flag — not the status — decides success (`check_http` + `check_ok`);
//! - `search.messages` only works with a *user* token (`xoxp-…`, `search:read`);
//!   bot tokens get `not_allowed_token_type`, so the user token is used for
//!   `search` and nothing else.

use super::plugin_services::{self, Identity, Reply};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SlackCfg {
    /// Off unless the user switches it on (Settings → Connectors).
    pub enabled: bool,
    /// Bot token (xoxb-…). Empty = SLACK_BOT_TOKEN.
    pub token: String,
    /// Optional user token (xoxp-…) with search:read, only for `search`. Empty = SLACK_USER_TOKEN.
    pub user_token: String,
}

/// Hand-written so a token never reaches a log line or a test failure message.
impl std::fmt::Debug for SlackCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackCfg")
            .field("enabled", &self.enabled)
            .field(
                "token",
                &if self.token.is_empty() {
                    ""
                } else {
                    "[redacted]"
                },
            )
            .field(
                "user_token",
                &if self.user_token.is_empty() {
                    ""
                } else {
                    "[redacted]"
                },
            )
            .finish()
    }
}

/// Actions that only read. Anything not listed here (including an action
/// nobody has heard of) is a write — see `is_write`.
pub const READS: &[&str] = &[
    "whoami",
    "list_channels",
    "history",
    "replies",
    "search",
    "user_info",
    "list_users",
];

/// Actions that change something on Slack (need approval).
pub const WRITES: &[&str] = &["post_message", "reply", "react"];

const API: &str = "https://slack.com/api/";
const UA: &str = "OpenLeash/0.1";
/// The whole tool result, like the GitHub plugin's cap.
const MAX_OUT: usize = 80_000;
/// One message's text inside a list (Slack allows ~40k per message; a list of
/// those would drown the context).
const MSG_CLIP: usize = 4_000;
/// `users.info` lookups per call when turning `U…` ids into names. Cheap
/// (tier 4), but a busy channel can mention hundreds of people; past this cap
/// the raw id is shown, which is still usable.
const USER_LOOKUPS: usize = 40;
/// Pages walked when filling `limit` from a filtered list, or when resolving
/// `#name` to an id.
const MAX_PAGES: usize = 10;
const RESOLVE_PAGES: usize = 20;

const ENV_BOT: &str = "SLACK_BOT_TOKEN";
const ENV_USER: &str = "SLACK_USER_TOKEN";

pub fn schema() -> Value {
    json!({
        "name": "slack",
        "description": "Read and post in the user's Slack workspace through the Slack Web API (as the configured Slack app's bot). Actions:\n\
    - whoami: which bot/user and workspace the tokens belong to (and whether search is available)\n\
    - list_channels (limit, cursor): `#name (id) · members · topic`, marking channels the bot is a member of\n\
    - history (channel, limit, oldest, latest, cursor): recent messages, oldest first; threads show their reply count\n\
    - replies (channel, thread_ts, limit, cursor): a whole thread, parent first\n\
    - search (query, count, page, sort): search.messages with Slack search syntax (`in:#general from:@ada after:2024-05-01 \"exact phrase\"`). Needs a USER token (xoxp-… with search:read) — bot tokens cannot search; without one, use history instead\n\
    - user_info (user = U… id, or email): profile details · list_users (limit, cursor)\n\
    - post_message (channel, text, blocks?) · reply (channel, thread_ts, text, reply_broadcast?) · react (channel, timestamp, emoji)\n\
    `channel` is an id (C…/G…/D…, or a U… user id to DM through the app) or a `#name`. Messages are shown as `ts · YYYY-MM-DD HH:MM UTC · @user: text` — pass that `ts` as thread_ts/timestamp. `oldest`/`latest` take a Slack ts, an RFC 3339 time or a YYYY-MM-DD date. Text uses Slack mrkdwn (*bold*, _italic_, `code`, <https://url|label>, <@U123> mentions), not Markdown. The bot can only read channels it was invited to (`/invite @app` in Slack); a `missing_scope` error names the scope the Slack app needs. Writes (post_message, reply, react) ask the user first unless allowed; never post unless the user asked you to.",
        "input_schema": {"type":"object","properties":{
            "action":{"type":"string","enum":["whoami","list_channels","history","replies","search","user_info","list_users","post_message","reply","react"]},
            "channel":{"type":"string","description":"Channel id (C…/G…/D…), a #name, or a U… user id (post_message: DM from the app)"},
            "thread_ts":{"type":"string","description":"replies/reply: the ts of the thread's parent message (as shown in history)"},
            "timestamp":{"type":"string","description":"react: the ts of the message to react to"},
            "emoji":{"type":"string","description":"react: emoji name, with or without colons (e.g. thumbsup, :white_check_mark:)"},
            "text":{"type":"string","description":"post_message/reply: message text in Slack mrkdwn (also the notification fallback when blocks are given)"},
            "blocks":{"description":"post_message/reply: optional Block Kit blocks (an array, or a JSON string of one), passed through as-is"},
            "reply_broadcast":{"type":"boolean","description":"reply: also post the reply to the channel"},
            "query":{"type":"string","description":"search: Slack search query (supports in:, from:, before:, after:, has:)"},
            "count":{"type":"integer","description":"search: results per page (default 20, max 100)"},
            "page":{"type":"integer","description":"search: result page (default 1)"},
            "sort":{"type":"string","enum":["score","timestamp"],"description":"search: relevance (default) or newest first"},
            "user":{"type":"string","description":"user_info: user id (U…)"},
            "email":{"type":"string","description":"user_info: look the user up by email instead (needs users:read.email)"},
            "oldest":{"type":"string","description":"history: only messages after this (Slack ts, RFC 3339 or YYYY-MM-DD, UTC)"},
            "latest":{"type":"string","description":"history: only messages before this (same formats)"},
            "limit":{"type":"integer","description":"Max results (default 20, max 100)"},
            "cursor":{"type":"string","description":"Continue a list where the previous call said more was available"}},
            "required":["action"]}
    })
}

/// Whether this call changes something on Slack. Fails closed.
pub fn is_write(input: &Value) -> bool {
    let a = input["action"].as_str().unwrap_or("");
    !READS.contains(&a)
}

// ───────────────────────────── credentials ─────────────────────────────

/// The resolved tokens and where each came from (`settings` or the env var).
pub struct Creds {
    pub bot: Option<(String, &'static str)>,
    pub user: Option<(String, &'static str)>,
}

/// Hand-written for the same reason as `SlackCfg`'s.
impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("bot", &self.bot.as_ref().map(|(_, s)| *s))
            .field("user", &self.user.as_ref().map(|(_, s)| *s))
            .finish()
    }
}

const NOT_CONFIGURED: &str = "Slack is not configured: add a bot token (xoxb-…) in Settings → Connectors → Slack, or set the SLACK_BOT_TOKEN environment variable (optionally a user token xoxp-… with search:read in the same card or SLACK_USER_TOKEN, for search).";

/// Tokens from settings first, then the environment. `env` is injected so
/// tests never read (or race on) the real process environment.
pub fn creds(cfg: &SlackCfg, env: &dyn Fn(&str) -> Option<String>) -> Result<Creds, String> {
    let bot = plugin_services::setting_or_env(&cfg.token, &[ENV_BOT], env);
    let user = plugin_services::setting_or_env(&cfg.user_token, &[ENV_USER], env);
    if bot.is_none() && user.is_none() {
        return Err(NOT_CONFIGURED.into());
    }
    Ok(Creds { bot, user })
}

impl Creds {
    fn secrets(&self) -> Vec<&str> {
        self.bot
            .iter()
            .chain(self.user.iter())
            .map(|(t, _)| t.as_str())
            .collect()
    }

    /// The token an action uses: the user token for `search` (bots cannot
    /// search), the bot token for everything else. The user token is never a
    /// fallback for other actions — it acts as the person, not the app, and
    /// the user only granted it for search.
    fn token_for(&self, action: &str) -> Result<(&str, bool), String> {
        if action == "search" {
            return self
                .user
                .as_ref()
                .map(|(t, _)| (t.as_str(), true))
                .ok_or_else(search_needs_user_token);
        }
        self.bot
            .as_ref()
            .map(|(t, _)| (t.as_str(), false))
            .ok_or_else(|| {
                format!("Slack `{action}` needs a bot token (xoxb-…), but only a user token is configured (it is used for search only). Add the bot token in Settings → Connectors → Slack, or set SLACK_BOT_TOKEN.")
            })
    }

    /// Loose prefix checks, appended to auth-type errors only: a token in the
    /// wrong slot is the most common setup mistake, but it is not blocked (Slack
    /// has other token kinds, e.g. xoxe- rotating tokens). Never echoes a token.
    fn note(&self) -> String {
        let mut n = String::new();
        if let Some((t, _)) = &self.bot {
            if t.starts_with("xoxp-") {
                n.push_str(" Note: the bot-token slot holds a user token (xoxp-…); bot tokens start with xoxb-, and the user token belongs in the user-token field.");
            } else if !t.starts_with("xox") {
                n.push_str(" Note: the bot token does not look like a Slack token (they start with xoxb-).");
            }
        }
        if let Some((t, _)) = &self.user {
            if t.starts_with("xoxb-") {
                n.push_str(" Note: the user-token slot holds a bot token (xoxb-…); search needs a user token (xoxp-…).");
            } else if !t.starts_with("xox") {
                n.push_str(" Note: the user token does not look like a Slack token (they start with xoxp-).");
            }
        }
        n
    }
}

fn search_needs_user_token() -> String {
    "Slack search needs a user token: search.messages only accepts a user token (xoxp-…) with the search:read scope — bot tokens cannot search. Add one in Settings → Connectors → Slack → user token, or set SLACK_USER_TOKEN. Meanwhile, read a channel with `history` (and `replies` for threads) instead.".into()
}

// ───────────────────────────── requests ─────────────────────────────

/// One Web API call: GET with query parameters when `body` is `None`,
/// otherwise POST with a JSON body. Pure, so URL/body building is testable.
#[derive(Debug, Clone, PartialEq)]
pub struct Req {
    pub api: &'static str,
    pub query: Vec<(&'static str, String)>,
    pub body: Option<Value>,
}

impl Req {
    fn get(api: &'static str, query: Vec<(&'static str, String)>) -> Self {
        Req {
            api,
            query,
            body: None,
        }
    }

    fn post(api: &'static str, body: Value) -> Self {
        Req {
            api,
            query: vec![],
            body: Some(body),
        }
    }

    /// `https://slack.com/api/<method>?k=v…`, every value percent-encoded by
    /// the URL library (a `#`, `&` or space in a query cannot change the call).
    pub fn url(&self) -> Result<reqwest::Url, String> {
        let mut u = reqwest::Url::parse(API)
            .and_then(|b| b.join(self.api))
            .map_err(|e| format!("Slack: bad API URL for {}: {e}", self.api))?;
        if !self.query.is_empty() {
            let mut q = u.query_pairs_mut();
            for (k, v) in &self.query {
                q.append_pair(k, v);
            }
        }
        Ok(u)
    }
}

fn opt(q: &mut Vec<(&'static str, String)>, k: &'static str, v: Option<&str>) {
    if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
        q.push((k, v.to_string()));
    }
}

pub fn req_auth_test() -> Req {
    Req::get("auth.test", vec![])
}

pub fn req_list_channels(limit: usize, cursor: Option<&str>, exclude_archived: bool) -> Req {
    let mut q = vec![
        ("types", "public_channel,private_channel".to_string()),
        ("exclude_archived", exclude_archived.to_string()),
        ("limit", limit.to_string()),
    ];
    opt(&mut q, "cursor", cursor);
    Req::get("conversations.list", q)
}

pub fn req_history(
    channel: &str,
    limit: usize,
    oldest: Option<&str>,
    latest: Option<&str>,
    cursor: Option<&str>,
) -> Req {
    let mut q = vec![
        ("channel", channel.to_string()),
        ("limit", limit.to_string()),
    ];
    opt(&mut q, "oldest", oldest);
    opt(&mut q, "latest", latest);
    opt(&mut q, "cursor", cursor);
    Req::get("conversations.history", q)
}

pub fn req_replies(channel: &str, ts: &str, limit: usize, cursor: Option<&str>) -> Req {
    let mut q = vec![
        ("channel", channel.to_string()),
        ("ts", ts.to_string()),
        ("limit", limit.to_string()),
    ];
    opt(&mut q, "cursor", cursor);
    Req::get("conversations.replies", q)
}

pub fn req_search(query: &str, count: usize, page: u64, sort: Option<&str>) -> Req {
    let mut q = vec![
        ("query", query.to_string()),
        ("count", count.to_string()),
        ("page", page.to_string()),
        ("highlight", "false".to_string()),
    ];
    if let Some(s) = sort.filter(|s| matches!(*s, "score" | "timestamp")) {
        q.push(("sort", s.to_string()));
        q.push(("sort_dir", "desc".to_string()));
    }
    Req::get("search.messages", q)
}

pub fn req_user_info(user: &str) -> Req {
    Req::get("users.info", vec![("user", user.to_string())])
}

pub fn req_lookup_email(email: &str) -> Req {
    Req::get("users.lookupByEmail", vec![("email", email.to_string())])
}

pub fn req_users_list(limit: usize, cursor: Option<&str>) -> Req {
    let mut q = vec![("limit", limit.to_string())];
    opt(&mut q, "cursor", cursor);
    Req::get("users.list", q)
}

/// chat.postMessage, both for a new message and a thread reply. The text is a
/// JSON value, so quotes/newlines in it can never alter the request.
pub fn req_post(
    channel: &str,
    text: Option<&str>,
    blocks: Option<Value>,
    thread_ts: Option<&str>,
    broadcast: bool,
) -> Req {
    let mut b = json!({ "channel": channel });
    if let Some(t) = text {
        b["text"] = json!(t);
    }
    if let Some(bl) = blocks {
        b["blocks"] = bl;
    }
    if let Some(ts) = thread_ts {
        b["thread_ts"] = json!(ts);
        if broadcast {
            b["reply_broadcast"] = json!(true);
        }
    }
    Req::post("chat.postMessage", b)
}

pub fn req_react(channel: &str, ts: &str, emoji: &str) -> Req {
    Req::post(
        "reactions.add",
        json!({ "channel": channel, "timestamp": ts, "name": emoji_name(emoji) }),
    )
}

pub fn req_permalink(channel: &str, ts: &str) -> Req {
    Req::get(
        "chat.getPermalink",
        vec![
            ("channel", channel.to_string()),
            ("message_ts", ts.to_string()),
        ],
    )
}

/// `:thumbsup:` → `thumbsup`; `:+1::skin-tone-2:` → `+1::skin-tone-2`.
pub fn emoji_name(e: &str) -> String {
    e.trim().trim_matches(':').to_string()
}

/// Block Kit blocks: an array as-is, or a JSON string holding one (models
/// often pass JSON as a string).
pub fn parse_blocks(v: &Value) -> Result<Option<Value>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Array(a) if a.is_empty() => Ok(None),
        Value::Array(_) => Ok(Some(v.clone())),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(a @ Value::Array(_)) => Ok(Some(a)),
            Ok(Value::Object(o)) if o.get("blocks").is_some_and(Value::is_array) => {
                Ok(o.get("blocks").cloned())
            }
            _ => Err("`blocks` must be a JSON array of Block Kit blocks.".into()),
        },
        Value::Object(o) if o.get("blocks").is_some_and(Value::is_array) => {
            Ok(o.get("blocks").cloned())
        }
        _ => Err("`blocks` must be a JSON array of Block Kit blocks.".into()),
    }
}

// ───────────────────────────── input helpers ─────────────────────────────

fn s<'a>(input: &'a Value, k: &str) -> Option<&'a str> {
    input[k].as_str().map(str::trim).filter(|v| !v.is_empty())
}

fn num(input: &Value, k: &str) -> Option<u64> {
    match &input[k] {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
}

/// `limit` (or another key), default 20, clamped to 1..=100.
fn limit_of(input: &Value, keys: &[&str]) -> usize {
    keys.iter()
        .find_map(|k| num(input, k))
        .unwrap_or(20)
        .clamp(1, 100) as usize
}

fn need<'a>(input: &'a Value, k: &str, action: &str) -> Result<&'a str, String> {
    s(input, k).ok_or_else(|| format!("Slack `{action}` needs `{k}`."))
}

/// How a `channel` argument names a conversation.
#[derive(Debug, PartialEq)]
pub enum ChannelRef {
    Id(String),
    Name(String),
}

fn looks_like_id(s: &str) -> bool {
    s.len() >= 9
        && s.len() <= 15
        && s.starts_with(['C', 'G', 'D', 'U', 'W'])
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `C0123ABCD`, `#general`, `general`, `<#C0123ABCD|general>` or an
/// `…slack.com/archives/C0123ABCD/p…` link.
pub fn channel_ref(raw: &str) -> Result<ChannelRef, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("`channel` is empty.".into());
    }
    if let Some(inner) = t.strip_prefix("<#").and_then(|r| r.strip_suffix('>')) {
        let id = inner.split('|').next().unwrap_or("");
        if looks_like_id(id) {
            return Ok(ChannelRef::Id(id.to_string()));
        }
    }
    if let Some(rest) = t.split("/archives/").nth(1) {
        let id = rest.split(['/', '?', '#']).next().unwrap_or("");
        if looks_like_id(id) {
            return Ok(ChannelRef::Id(id.to_string()));
        }
    }
    if let Some(name) = t.strip_prefix('#') {
        return Ok(ChannelRef::Name(name.trim().to_lowercase()));
    }
    if looks_like_id(t) {
        return Ok(ChannelRef::Id(t.to_string()));
    }
    Ok(ChannelRef::Name(t.to_lowercase()))
}

/// A message ts as Slack wants it. Accepts `1712345678.123456`, the
/// `p1712345678123456` id from a permalink, or a whole permalink.
pub fn norm_ts(raw: &str) -> String {
    let t = raw.trim();
    let tail = t.rsplit('/').next().unwrap_or(t);
    let tail = tail.split(['?', '#']).next().unwrap_or(tail);
    if let Some(d) = tail.strip_prefix('p') {
        if d.len() > 6 && d.chars().all(|c| c.is_ascii_digit()) {
            let (a, b) = d.split_at(d.len() - 6);
            return format!("{a}.{b}");
        }
    }
    t.to_string()
}

/// `oldest`/`latest`: a Slack ts as-is, or an RFC 3339 time / `YYYY-MM-DD`
/// (UTC) / `YYYY-MM-DD HH:MM` turned into epoch seconds.
pub fn to_slack_ts(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if !t.is_empty()
        && t.chars().all(|c| c.is_ascii_digit() || c == '.')
        && t.parse::<f64>().is_ok()
    {
        return Ok(t.to_string());
    }
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(t) {
        return Ok(d.timestamp().to_string());
    }
    for f in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(t, f) {
            return Ok(d.and_utc().timestamp().to_string());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            return Ok(dt.and_utc().timestamp().to_string());
        }
    }
    Err(format!(
        "`{t}` is not a time: use a Slack ts (1712345678.123456), an RFC 3339 time or YYYY-MM-DD."
    ))
}

// ───────────────────────────── responses ─────────────────────────────

/// Non-2xx statuses and non-JSON bodies. Slack reports most failures as
/// HTTP 200 + `ok:false`, which `check_ok` handles.
pub fn check_http(api: &str, rep: &Reply, secrets: &[&str], note: &str) -> Result<Value, String> {
    if !(200..300).contains(&rep.status) {
        let msg = serde_json::from_str::<Value>(&rep.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(String::from))
            .unwrap_or_else(|| plugin_services::clip(rep.body.trim(), 300));
        let hint = match rep.status {
            401 | 403 => format!("The Slack token was rejected — check Settings → Connectors → Slack (or SLACK_BOT_TOKEN / SLACK_USER_TOKEN).{note}"),
            404 => "Unknown Slack API method.".to_string(),
            500..=599 => "Slack had a server error; retry in a moment.".to_string(),
            _ => String::new(),
        };
        return Err(plugin_services::http_error(
            "Slack",
            rep.status,
            api,
            &msg,
            hint.trim(),
            secrets,
        ));
    }
    serde_json::from_str(&rep.body).map_err(|_| {
        plugin_services::redact(
            &format!(
                "Slack sent a non-JSON reply to {api}: {}",
                plugin_services::clip(rep.body.trim(), 300)
            ),
            secrets,
        )
    })
}

/// `{"ok":false,"error":"…"}` → a clear error with a fix.
pub fn check_ok(api: &str, v: Value, secrets: &[&str], note: &str) -> Result<Value, String> {
    if v["ok"].as_bool() == Some(true) {
        return Ok(v);
    }
    Err(slack_error(api, &v, secrets, note))
}

pub fn slack_error(api: &str, v: &Value, secrets: &[&str], note: &str) -> String {
    let code = v["error"].as_str().unwrap_or("unknown_error");
    let hint = match code {
        "not_in_channel" => "The bot is not a member of this channel. Invite it in Slack with `/invite @<your app>` in the channel (or channel details → Integrations → Add apps), then retry.".to_string(),
        "missing_scope" => {
            let needed = v["needed"].as_str().unwrap_or("?");
            let provided = v["provided"].as_str().unwrap_or("?");
            format!("The token lacks a scope this call needs — needed: {needed}; the token has: {provided}. Add the scope under OAuth & Permissions in the Slack app's settings, reinstall the app to the workspace, and update the token in Settings → Connectors → Slack.")
        }
        "channel_not_found" => "No such channel visible to this token. Use list_channels for ids; private channels and DMs are only visible once the bot is a member.".to_string(),
        "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" | "account_inactive" => format!("The Slack token is missing, invalid or revoked — set a valid one in Settings → Connectors → Slack (or SLACK_BOT_TOKEN / SLACK_USER_TOKEN).{note}"),
        "not_allowed_token_type" => format!("This method does not accept this kind of token (search needs a user token xoxp-…; most others a bot token xoxb-…).{note}"),
        "ratelimited" | "rate_limited" => "Slack rate limit hit — wait a minute before calling again, and narrow the request.".to_string(),
        "user_not_found" | "users_not_found" => "No such user. Check the id (U…) or email; list_users shows ids. Looking up by email needs the users:read.email scope.".to_string(),
        "message_not_found" | "thread_not_found" => "No message with that ts in this channel — copy the ts exactly as history shows it.".to_string(),
        "invalid_name" => "Unknown emoji name — use the name without colons, e.g. thumbsup or white_check_mark.".to_string(),
        "is_archived" => "That channel is archived; it can no longer be posted to.".to_string(),
        "no_text" => "The message needs `text` (or `blocks`).".to_string(),
        "msg_too_long" => "The message is too long for Slack; shorten it or split it.".to_string(),
        "invalid_blocks" | "invalid_blocks_format" => "The Block Kit blocks are invalid; check them in Slack's Block Kit Builder, or send plain `text`.".to_string(),
        "restricted_action" | "cant_reply_in_thread" | "not_allowed" => "The workspace settings do not allow the app to do this here.".to_string(),
        "invalid_cursor" => "The cursor is invalid or expired; start the list again without `cursor`.".to_string(),
        "invalid_ts_latest" | "invalid_ts_oldest" => "`oldest`/`latest` must be a Slack ts or a date.".to_string(),
        _ => String::new(),
    };
    let extra = v["response_metadata"]["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter_map(Value::as_str)
                .take(5)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|m| !m.is_empty())
        .map(|m| format!(" ({})", plugin_services::clip(&m, 600)))
        .unwrap_or_default();
    plugin_services::redact(
        &format!(
            "Slack {api} failed: {code}{extra}.{}{hint}",
            if hint.is_empty() { "" } else { " " }
        ),
        secrets,
    )
}

// ───────────────────────────── formatting ─────────────────────────────

/// `1712345678.123456` → `2024-04-05 19:34 UTC`.
pub fn ts_human(ts: &str) -> String {
    ts.split('.')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "?".into())
}

fn one_line(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        flat.chars().take(n).collect::<String>() + "…"
    }
}

fn mention_re() -> Option<regex::Regex> {
    regex::Regex::new(r"<([^<>\s][^<>]*)>").ok()
}

/// Slack's wire format → readable text: `<@U1>` → `@name` (from `users`),
/// `<#C1|general>` → `#general`, `<!here>` → `@here`, `<url|label>` →
/// `label (url)`, and `&lt; &gt; &amp;` unescaped.
pub fn render_text(text: &str, users: &HashMap<String, String>) -> String {
    let out = match mention_re() {
        Some(re) => re
            .replace_all(text, |c: &regex::Captures| {
                let inner = &c[1];
                let (target, label) = match inner.split_once('|') {
                    Some((t, l)) => (t, Some(l).filter(|l| !l.is_empty())),
                    None => (inner, None),
                };
                if let Some(id) = target.strip_prefix('@') {
                    let name = users.get(id).map(String::as_str).or(label).unwrap_or(id);
                    format!("@{}", name.trim_start_matches('@'))
                } else if let Some(id) = target.strip_prefix('#') {
                    format!("#{}", label.unwrap_or(id))
                } else if let Some(sp) = target.strip_prefix('!') {
                    if let Some(l) = label {
                        l.to_string()
                    } else if sp.starts_with("subteam^") {
                        "@group".into()
                    } else {
                        format!("@{sp}")
                    }
                } else {
                    let t = target.strip_prefix("mailto:").unwrap_or(target);
                    match label {
                        Some(l) if l != t && l != target => format!("{l} ({t})"),
                        _ => t.to_string(),
                    }
                }
            })
            .into_owned(),
        None => text.to_string(),
    };
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The display name Slack shows: display name, else real name, else handle.
pub fn user_name(u: &Value) -> String {
    [
        &u["profile"]["display_name"],
        &u["profile"]["real_name"],
        &u["real_name"],
        &u["name"],
    ]
    .iter()
    .find_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()))
    .unwrap_or("?")
    .to_string()
}

/// User ids a batch of messages refers to (authors and `<@U…>` mentions),
/// in first-seen order, skipping ones already in `known`.
pub fn user_ids(msgs: &[Value], known: &HashMap<String, String>) -> Vec<String> {
    let re = regex::Regex::new(r"<@([UW][A-Z0-9]+)").ok();
    let mut out: Vec<String> = vec![];
    let mut push = |id: &str| {
        if !id.is_empty() && !known.contains_key(id) && !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    };
    for m in msgs {
        if let Some(u) = m["user"].as_str() {
            push(u);
        }
        if let (Some(re), Some(t)) = (&re, m["text"].as_str()) {
            for c in re.captures_iter(t) {
                push(&c[1]);
            }
        }
    }
    out
}

fn author(m: &Value, users: &HashMap<String, String>) -> String {
    if let Some(u) = m["user"].as_str().filter(|u| !u.is_empty()) {
        let name = users.get(u).cloned().or_else(|| {
            m["username"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(String::from)
        });
        return match name {
            Some(n) => format!("@{n}"),
            None => format!("@{u}"),
        };
    }
    let bot = m["username"]
        .as_str()
        .or_else(|| m["bot_profile"]["name"].as_str())
        .filter(|n| !n.is_empty());
    match bot {
        Some(n) => format!("{n} (bot)"),
        None if m["bot_id"].is_string() => "(bot)".into(),
        None => "(unknown)".into(),
    }
}

/// One message: `ts · time · @who: text` plus files, reactions and thread
/// info. Continuation lines are indented so messages stay visually separate.
pub fn fmt_message(m: &Value, users: &HashMap<String, String>) -> String {
    let ts = m["ts"].as_str().unwrap_or("?");
    let mut text = m["text"].as_str().unwrap_or("").to_string();
    if text.trim().is_empty() {
        // Attachment-only messages (integrations) carry their text there.
        text = m["attachments"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find_map(|x| x["fallback"].as_str().or(x["text"].as_str()))
            })
            .unwrap_or("")
            .to_string();
    }
    let body = render_text(&plugin_services::clip(&text, MSG_CLIP), users);
    let mut line = format!(
        "{ts} · {} · {}: {}",
        ts_human(ts),
        author(m, users),
        body.trim().replace('\n', "\n    ")
    );
    if let Some(st) = m["subtype"]
        .as_str()
        .filter(|s| !matches!(*s, "thread_broadcast" | "bot_message"))
    {
        line.push_str(&format!(" [{st}]"));
    }
    if m["edited"].is_object() {
        line.push_str(" [edited]");
    }
    if let Some(files) = m["files"].as_array().filter(|f| !f.is_empty()) {
        let names: Vec<String> = files
            .iter()
            .map(|f| {
                f["name"]
                    .as_str()
                    .or(f["title"].as_str())
                    .unwrap_or("file")
                    .to_string()
            })
            .collect();
        line.push_str(&format!(" [files: {}]", names.join(", ")));
    }
    if let Some(rs) = m["reactions"].as_array().filter(|r| !r.is_empty()) {
        let r: Vec<String> = rs
            .iter()
            .map(|r| {
                format!(
                    ":{}: {}",
                    r["name"].as_str().unwrap_or("?"),
                    r["count"].as_u64().unwrap_or(0)
                )
            })
            .collect();
        line.push_str(&format!(" [{}]", r.join(" ")));
    }
    let replies = m["reply_count"].as_u64().unwrap_or(0);
    if replies > 0 {
        let latest = m["latest_reply"]
            .as_str()
            .map(|l| format!(", latest {}", ts_human(l)))
            .unwrap_or_default();
        line.push_str(&format!(
            " [thread: {replies} {}{latest} — replies thread_ts={}]",
            if replies == 1 { "reply" } else { "replies" },
            m["thread_ts"].as_str().unwrap_or(ts)
        ));
    }
    line
}

/// One channel: `#name (id) · N members · private · member · topic`.
pub fn fmt_channel(c: &Value) -> String {
    let mut parts = vec![format!(
        "#{} ({})",
        c["name"].as_str().unwrap_or("?"),
        c["id"].as_str().unwrap_or("?")
    )];
    if let Some(n) = c["num_members"].as_u64() {
        parts.push(format!("{n} member{}", if n == 1 { "" } else { "s" }));
    }
    if c["is_private"].as_bool() == Some(true) {
        parts.push("private".into());
    }
    if c["is_archived"].as_bool() == Some(true) {
        parts.push("archived".into());
    }
    parts.push(
        if c["is_member"].as_bool() == Some(true) {
            "bot is a member"
        } else {
            "bot not a member"
        }
        .into(),
    );
    let topic = c["topic"]["value"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            c["purpose"]["value"]
                .as_str()
                .filter(|t| !t.trim().is_empty())
        });
    if let Some(t) = topic {
        parts.push(one_line(&render_text(t, &HashMap::new()), 150));
    }
    parts.join(" · ")
}

pub fn fmt_channels(chs: &[Value], next: Option<&str>) -> String {
    if chs.is_empty() {
        return "No channels visible to the bot (it sees public channels, and private ones it was invited to).".into();
    }
    let members = chs
        .iter()
        .filter(|c| c["is_member"].as_bool() == Some(true))
        .count();
    let mut out = format!(
        "{} channel{} ({members} with the bot as a member — it can only read those):\n",
        chs.len(),
        if chs.len() == 1 { "" } else { "s" }
    );
    for c in chs {
        out.push_str(&fmt_channel(c));
        out.push('\n');
    }
    out.push_str(&more(next));
    out.trim_end().to_string()
}

fn more(next: Option<&str>) -> String {
    match next.filter(|n| !n.is_empty()) {
        Some(n) => format!("More available: pass cursor=\"{n}\"."),
        None => String::new(),
    }
}

/// A row for list_users.
pub fn fmt_user_row(u: &Value) -> String {
    let mut parts = vec![format!(
        "@{} ({})",
        u["name"].as_str().unwrap_or("?"),
        u["id"].as_str().unwrap_or("?")
    )];
    let real = u["profile"]["real_name"]
        .as_str()
        .or(u["real_name"].as_str())
        .filter(|s| !s.trim().is_empty());
    if let Some(r) = real {
        parts.push(r.to_string());
    }
    if let Some(t) = u["profile"]["title"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    {
        parts.push(one_line(t, 80));
    }
    if let Some(e) = u["profile"]["email"].as_str().filter(|s| !s.is_empty()) {
        parts.push(e.to_string());
    }
    for (k, tag) in [
        ("is_bot", "bot"),
        ("is_admin", "admin"),
        ("deleted", "deactivated"),
        ("is_restricted", "guest"),
    ] {
        if u[k].as_bool() == Some(true) {
            parts.push(tag.into());
        }
    }
    parts.join(" · ")
}

pub fn fmt_users(us: &[Value], next: Option<&str>) -> String {
    if us.is_empty() {
        return "No users.".into();
    }
    let mut out = format!(
        "{} user{}:\n",
        us.len(),
        if us.len() == 1 { "" } else { "s" }
    );
    for u in us {
        out.push_str(&fmt_user_row(u));
        out.push('\n');
    }
    out.push_str(&more(next));
    out.trim_end().to_string()
}

pub fn fmt_user_detail(u: &Value) -> String {
    let p = &u["profile"];
    let mut out = format!("{}\n", fmt_user_row(u));
    for (label, v) in [
        ("Display name", &p["display_name"]),
        ("Real name", &p["real_name"]),
        ("Title", &p["title"]),
        ("Email", &p["email"]),
        ("Phone", &p["phone"]),
        ("Time zone", &u["tz"]),
    ] {
        if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
            out.push_str(&format!("{label}: {}\n", one_line(s, 200)));
        }
    }
    let status = [
        p["status_emoji"].as_str().unwrap_or(""),
        p["status_text"].as_str().unwrap_or(""),
    ]
    .join(" ");
    if !status.trim().is_empty() {
        out.push_str(&format!("Status: {}\n", one_line(&status, 200)));
    }
    if u["is_owner"].as_bool() == Some(true) {
        out.push_str("Workspace owner\n");
    }
    out.trim_end().to_string()
}

pub fn fmt_search(v: &Value, query: &str, users: &HashMap<String, String>) -> String {
    let m = &v["messages"];
    let matches = m["matches"].as_array().cloned().unwrap_or_default();
    let total = m["total"]
        .as_u64()
        .or(m["paging"]["total"].as_u64())
        .unwrap_or(matches.len() as u64);
    if matches.is_empty() {
        return format!("No messages match {query:?}.");
    }
    let page = m["paging"]["page"].as_u64().unwrap_or(1);
    let pages = m["paging"]["pages"].as_u64().unwrap_or(1);
    let mut out = format!(
        "{} of {total} matches for {query:?} (page {page}/{pages}):\n",
        matches.len()
    );
    for x in &matches {
        let ch = &x["channel"];
        let where_ = if ch["is_im"].as_bool() == Some(true) {
            format!("DM ({})", ch["id"].as_str().unwrap_or("?"))
        } else if ch["is_mpim"].as_bool() == Some(true) {
            format!("group DM ({})", ch["id"].as_str().unwrap_or("?"))
        } else {
            format!(
                "#{} ({})",
                ch["name"].as_str().unwrap_or("?"),
                ch["id"].as_str().unwrap_or("?")
            )
        };
        out.push_str(&format!("{where_} · {}", fmt_message(x, users)));
        if let Some(l) = x["permalink"].as_str() {
            out.push_str(&format!("\n    {l}"));
        }
        out.push('\n');
    }
    if page < pages {
        out.push_str(&format!("More available: pass page={}.", page + 1));
    }
    out.trim_end().to_string()
}

/// `@bot (U…) · Team (https://team.slack.com/)`.
pub fn fmt_auth(v: &Value) -> String {
    let who = v["user"].as_str().unwrap_or("?");
    let id = v["user_id"].as_str().unwrap_or("?");
    let team = v["team"].as_str().unwrap_or("?");
    let url = v["url"].as_str().unwrap_or("");
    let mut s = format!(
        "@{who} ({id}{}) · {team}",
        if v["bot_id"].is_string() { ", bot" } else { "" }
    );
    if !url.is_empty() {
        s.push_str(&format!(" ({url})"));
    }
    s
}

// ───────────────────────────── network ─────────────────────────────

struct Slack<'a> {
    http: &'a reqwest::Client,
    c: &'a Creds,
    /// id → display name, filled once per tool call.
    users: HashMap<String, String>,
}

impl Slack<'_> {
    /// Send and decode, checking HTTP status but not Slack's `ok` flag.
    async fn raw(&self, req: &Req, user: bool) -> Result<Value, String> {
        let tok = if user {
            self.c.user.as_ref()
        } else {
            self.c.bot.as_ref()
        }
        .map(|(t, _)| t.as_str())
        .ok_or_else(|| NOT_CONFIGURED.to_string())?;
        let secrets = self.c.secrets();
        let url = req.url()?;
        let rq = match &req.body {
            None => self.http.get(url),
            Some(b) => self
                .http
                .post(url)
                .header("content-type", "application/json; charset=utf-8")
                .body(b.to_string()),
        }
        .bearer_auth(tok)
        .header("user-agent", UA);
        let rep = plugin_services::send("Slack", rq, &secrets).await?;
        check_http(req.api, &rep, &secrets, &self.c.note())
    }

    async fn call(&self, req: &Req, user: bool) -> Result<Value, String> {
        let v = self.raw(req, user).await?;
        check_ok(req.api, v, &self.c.secrets(), &self.c.note())
    }

    /// Bot-token call (everything but search).
    async fn bot(&self, req: &Req) -> Result<Value, String> {
        self.call(req, false).await
    }

    /// Fill the name cache for these ids. Best effort: a failed lookup just
    /// leaves the id visible.
    async fn learn_users(&mut self, ids: Vec<String>, user_token: bool) {
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|i| !self.users.contains_key(i))
            .take(USER_LOOKUPS)
            .collect();
        if ids.is_empty() {
            return;
        }
        let me = &*self;
        let found = futures_util::future::join_all(ids.iter().map(|id| async move {
            let r = me.call(&req_user_info(id), user_token).await.ok()?;
            Some((id.clone(), user_name(&r["user"])))
        }))
        .await;
        for (id, name) in found.into_iter().flatten() {
            self.users.insert(id, name);
        }
    }

    /// Channel argument → `(id, label for headers)`. Names are resolved
    /// through conversations.list (archived included: history still works).
    async fn channel(&self, input: &Value, action: &str) -> Result<(String, String), String> {
        let raw = need(input, "channel", action)?;
        match channel_ref(raw)? {
            ChannelRef::Id(id) => Ok((id.clone(), id)),
            ChannelRef::Name(name) => {
                let mut cursor: Option<String> = None;
                for _ in 0..RESOLVE_PAGES {
                    let v = self
                        .bot(&req_list_channels(1000, cursor.as_deref(), false))
                        .await?;
                    if let Some(c) = v["channels"].as_array().and_then(|a| {
                        a.iter().find(|c| {
                            c["name"].as_str() == Some(name.as_str())
                                || c["name_normalized"].as_str() == Some(name.as_str())
                        })
                    }) {
                        let id = c["id"].as_str().unwrap_or("").to_string();
                        return Ok((id.clone(), format!("#{name} ({id})")));
                    }
                    cursor = next_cursor(&v);
                    if cursor.is_none() {
                        break;
                    }
                }
                Err(format!("No channel named #{name} is visible to the bot (private channels only appear once the bot is invited). Use list_channels, or pass the channel id (C…)."))
            }
        }
    }

    async fn run(&mut self, input: &Value) -> Result<String, String> {
        let action = input["action"].as_str().unwrap_or("");
        // whoami reports on whichever tokens exist, so it works with only a
        // user token too; every other action needs its own kind of token.
        let use_user = if action == "whoami" {
            false
        } else {
            self.c.token_for(action)?.1
        };
        match action {
            "whoami" => self.whoami_text().await,
            "list_channels" => {
                let limit = limit_of(input, &["limit"]);
                let mut cursor = s(input, "cursor").map(String::from);
                let mut out: Vec<Value> = vec![];
                for _ in 0..MAX_PAGES {
                    let v = self
                        .bot(&req_list_channels(
                            limit - out.len(),
                            cursor.as_deref(),
                            true,
                        ))
                        .await?;
                    out.extend(v["channels"].as_array().cloned().unwrap_or_default());
                    cursor = next_cursor(&v);
                    if out.len() >= limit || cursor.is_none() {
                        break;
                    }
                }
                out.truncate(limit);
                Ok(fmt_channels(&out, cursor.as_deref()))
            }
            "history" => {
                let (id, label) = self.channel(input, action).await?;
                let oldest = s(input, "oldest").map(to_slack_ts).transpose()?;
                let latest = s(input, "latest").map(to_slack_ts).transpose()?;
                let v = self
                    .bot(&req_history(
                        &id,
                        limit_of(input, &["limit"]),
                        oldest.as_deref(),
                        latest.as_deref(),
                        s(input, "cursor"),
                    ))
                    .await?;
                let mut msgs = v["messages"].as_array().cloned().unwrap_or_default();
                // Slack sends newest first; a conversation reads oldest first.
                msgs.reverse();
                let ids = user_ids(&msgs, &self.users);
                self.learn_users(ids, false).await;
                let mut out = format!(
                    "{label} · {} message{}, oldest first:\n",
                    msgs.len(),
                    if msgs.len() == 1 { "" } else { "s" }
                );
                if msgs.is_empty() {
                    out = format!("{label}: no messages in that range.\n");
                }
                for m in &msgs {
                    out.push_str(&fmt_message(m, &self.users));
                    out.push('\n');
                }
                if v["has_more"].as_bool() == Some(true) {
                    out.push_str(&match next_cursor(&v) {
                        Some(c) => format!("Older messages available: pass cursor=\"{c}\"."),
                        None => "Older messages available: pass latest=<oldest ts shown>.".into(),
                    });
                }
                Ok(out.trim_end().to_string())
            }
            "replies" => {
                let (id, label) = self.channel(input, action).await?;
                let ts = s(input, "thread_ts")
                    .or_else(|| s(input, "ts"))
                    .or_else(|| s(input, "timestamp"))
                    .map(norm_ts)
                    .ok_or("Slack `replies` needs `thread_ts` (the parent message's ts).")?;
                let v = self
                    .bot(&req_replies(
                        &id,
                        &ts,
                        limit_of(input, &["limit"]),
                        s(input, "cursor"),
                    ))
                    .await?;
                let msgs = v["messages"].as_array().cloned().unwrap_or_default();
                let ids = user_ids(&msgs, &self.users);
                self.learn_users(ids, false).await;
                let mut out = format!(
                    "Thread {ts} in {label} · {} message{} (parent first):\n",
                    msgs.len(),
                    if msgs.len() == 1 { "" } else { "s" }
                );
                for m in &msgs {
                    out.push_str(&fmt_message(m, &self.users));
                    out.push('\n');
                }
                if v["has_more"].as_bool() == Some(true) {
                    out.push_str(&more(next_cursor(&v).as_deref()));
                }
                Ok(out.trim_end().to_string())
            }
            "search" => {
                let q = need(input, "query", action)?;
                let v = self
                    .call(
                        &req_search(
                            q,
                            limit_of(input, &["count", "limit"]),
                            num(input, "page").unwrap_or(1).max(1),
                            s(input, "sort"),
                        ),
                        use_user,
                    )
                    .await?;
                let matches = v["messages"]["matches"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let ids = user_ids(&matches, &self.users);
                self.learn_users(ids, true).await;
                Ok(fmt_search(&v, q, &self.users))
            }
            "user_info" => {
                let v = if let Some(e) = s(input, "email") {
                    self.bot(&req_lookup_email(e)).await?
                } else {
                    let u = s(input, "user").ok_or(
                        "Slack `user_info` needs `user` (a U… id) or `email`; list_users shows ids.",
                    )?;
                    let u = u
                        .trim_start_matches("<@")
                        .trim_end_matches('>')
                        .split('|')
                        .next()
                        .unwrap_or(u);
                    self.bot(&req_user_info(u)).await?
                };
                Ok(fmt_user_detail(&v["user"]))
            }
            "list_users" => {
                let limit = limit_of(input, &["limit"]);
                let mut cursor = s(input, "cursor").map(String::from);
                let mut out: Vec<Value> = vec![];
                for _ in 0..MAX_PAGES {
                    let v = self
                        .bot(&req_users_list(limit - out.len(), cursor.as_deref()))
                        .await?;
                    out.extend(v["members"].as_array().cloned().unwrap_or_default());
                    cursor = next_cursor(&v);
                    if out.len() >= limit || cursor.is_none() {
                        break;
                    }
                }
                out.truncate(limit);
                Ok(fmt_users(&out, cursor.as_deref()))
            }
            "post_message" | "reply" => {
                let (id, label) = self.channel(input, action).await?;
                let text = s(input, "text");
                let blocks = parse_blocks(&input["blocks"])?;
                if text.is_none() && blocks.is_none() {
                    return Err(format!("Slack `{action}` needs `text` (or `blocks`)."));
                }
                let thread = if action == "reply" {
                    Some(
                        s(input, "thread_ts")
                            .or_else(|| s(input, "ts"))
                            .map(norm_ts)
                            .ok_or("Slack `reply` needs `thread_ts` (the parent message's ts).")?,
                    )
                } else {
                    None
                };
                let broadcast = input["reply_broadcast"].as_bool() == Some(true);
                let v = self
                    .bot(&req_post(&id, text, blocks, thread.as_deref(), broadcast))
                    .await?;
                let ch = v["channel"].as_str().unwrap_or(&id).to_string();
                let ts = v["ts"].as_str().unwrap_or("?").to_string();
                let link = self
                    .bot(&req_permalink(&ch, &ts))
                    .await
                    .ok()
                    .and_then(|p| p["permalink"].as_str().map(String::from));
                let mut out = match &thread {
                    Some(t) => format!(
                        "Replied in thread {t} in {label}{} · ts={ts} · {}",
                        if broadcast {
                            " (also sent to the channel)"
                        } else {
                            ""
                        },
                        ts_human(&ts)
                    ),
                    None => format!("Posted to {label} · ts={ts} · {}", ts_human(&ts)),
                };
                if let Some(l) = link {
                    out.push_str(&format!("\n{l}"));
                }
                Ok(out)
            }
            "react" => {
                let (id, label) = self.channel(input, action).await?;
                let ts = s(input, "timestamp")
                    .or_else(|| s(input, "ts"))
                    .or_else(|| s(input, "thread_ts"))
                    .map(norm_ts)
                    .ok_or("Slack `react` needs `timestamp` (the message's ts).")?;
                let emoji = s(input, "emoji")
                    .or_else(|| s(input, "name"))
                    .map(emoji_name)
                    .filter(|e| !e.is_empty())
                    .ok_or("Slack `react` needs `emoji` (e.g. thumbsup).")?;
                let req = req_react(&id, &ts, &emoji);
                let v = self.raw(&req, false).await?;
                if v["error"].as_str() == Some("already_reacted") {
                    return Ok(format!("Already reacted :{emoji}: to {ts} in {label}."));
                }
                check_ok(req.api, v, &self.c.secrets(), &self.c.note())?;
                Ok(format!("Reacted :{emoji}: to {ts} in {label}."))
            }
            other => Err(format!(
                "Unknown Slack action `{other}`. Use one of: {}.",
                READS
                    .iter()
                    .chain(WRITES.iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    async fn whoami_text(&self) -> Result<String, String> {
        let mut out = String::new();
        if let Some((_, src)) = &self.c.bot {
            let v = self.call(&req_auth_test(), false).await?;
            out.push_str(&format!("Bot: {} · token from {src}\n", fmt_auth(&v)));
        } else {
            out.push_str("Bot: no bot token (xoxb-…) — only search works. Add one in Settings → Connectors → Slack, or set SLACK_BOT_TOKEN.\n");
        }
        match &self.c.user {
            Some((_, src)) => match self.call(&req_auth_test(), true).await {
                Ok(v) => out.push_str(&format!(
                    "Search: user token for {} · from {src}\n",
                    fmt_auth(&v)
                )),
                Err(e) => out.push_str(&format!("Search: the user token failed — {e}\n")),
            },
            None => out.push_str("Search: unavailable — no user token (xoxp-… with search:read). Add one in Settings → Connectors → Slack → user token, or set SLACK_USER_TOKEN.\n"),
        }
        out.push_str(&self.c.note());
        Ok(out.trim().to_string())
    }
}

fn next_cursor(v: &Value) -> Option<String> {
    v["response_metadata"]["next_cursor"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(String::from)
}

pub async fn run(http: &reqwest::Client, cfg: &SlackCfg, input: &Value) -> Result<String, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let secrets = c.secrets();
    let mut sl = Slack {
        http,
        c: &c,
        users: HashMap::new(),
    };
    match sl.run(input).await {
        Ok(out) => Ok(plugin_services::redact(
            &plugin_services::clip(&out, MAX_OUT),
            &secrets,
        )),
        Err(e) => Err(plugin_services::redact(&e, &secrets)),
    }
}

pub async fn whoami(http: &reqwest::Client, cfg: &SlackCfg) -> Result<Identity, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let sl = Slack {
        http,
        c: &c,
        users: HashMap::new(),
    };
    let secrets = c.secrets();
    let r = async {
        if let Some((_, src)) = &c.bot {
            let v = sl.call(&req_auth_test(), false).await?;
            let mut identity = fmt_auth(&v);
            match &c.user {
                Some(_) => match sl.call(&req_auth_test(), true).await {
                    Ok(u) => identity.push_str(&format!(
                        " · search as @{}",
                        u["user"].as_str().unwrap_or("?")
                    )),
                    Err(_) => identity.push_str(" · user token rejected (search unavailable)"),
                },
                None => identity.push_str(" · no user token (search unavailable)"),
            }
            Ok(Identity {
                identity,
                source: (*src).to_string(),
            })
        } else {
            // Only a user token: report it, but say the bot actions need more.
            let src = c.user.as_ref().map(|(_, s)| *s).unwrap_or("settings");
            let v = sl.call(&req_auth_test(), true).await?;
            Ok(Identity {
                identity: format!(
                    "{} · user token only (search works; other actions need a bot token xoxb-…)",
                    fmt_auth(&v)
                ),
                source: src.to_string(),
            })
        }
    }
    .await;
    r.map_err(|e: String| plugin_services::redact(&e, &secrets))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: &str = "xoxb-1111-2222-dummytokenvalue";
    const USER: &str = "xoxp-3333-4444-dummyusertoken";

    fn env_none(_: &str) -> Option<String> {
        None
    }

    fn both() -> Creds {
        Creds {
            bot: Some((BOT.into(), "settings")),
            user: Some((USER.into(), ENV_USER)),
        }
    }

    fn no_users() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn every_action_is_classified() {
        for a in READS {
            assert!(!is_write(&json!({"action": a})), "{a} is a read");
        }
        for a in WRITES {
            assert!(is_write(&json!({"action": a})), "{a} is a write");
        }
        for a in [
            "post_message",
            "reply",
            "react",
            "",
            "delete",
            "api",
            "WHOAMI",
        ] {
            assert!(is_write(&json!({"action": a})), "{a:?} must be a write");
        }
        assert!(is_write(&json!({})));
        assert!(is_write(&json!({"action": 1})));
    }

    #[test]
    fn schema_enum_matches_reads_and_writes() {
        let s = schema();
        assert_eq!(s["name"], "slack");
        assert_eq!(s["input_schema"]["required"], json!(["action"]));
        let e: Vec<&str> = s["input_schema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for a in &e {
            assert!(READS.contains(a) || WRITES.contains(a), "{a} unclassified");
        }
        for a in READS.iter().chain(WRITES.iter()) {
            assert!(e.contains(a), "{a} missing from enum");
            assert!(
                s["description"].as_str().unwrap().contains(a),
                "{a} undocumented"
            );
        }
        assert_eq!(e.len(), READS.len() + WRITES.len());
    }

    #[test]
    fn creds_settings_beat_env() {
        let env = |k: &str| match k {
            ENV_BOT => Some("xoxb-from-env-123".to_string()),
            ENV_USER => Some("xoxp-from-env-456".to_string()),
            _ => None,
        };
        let cfg = SlackCfg {
            enabled: true,
            token: format!(" {BOT} "),
            user_token: String::new(),
        };
        let c = creds(&cfg, &env).unwrap();
        assert_eq!(c.bot, Some((BOT.to_string(), "settings")));
        assert_eq!(c.user, Some(("xoxp-from-env-456".to_string(), ENV_USER)));
        let c = creds(&SlackCfg::default(), &env).unwrap();
        assert_eq!(c.bot.as_ref().unwrap().1, ENV_BOT);
        let dbg = format!("{c:?}");
        assert!(
            !dbg.contains("xoxb-from-env") && !dbg.contains("xoxp-from-env"),
            "{dbg}"
        );
    }

    #[test]
    fn missing_creds_say_where_to_set_them() {
        let e = creds(&SlackCfg::default(), &env_none).unwrap_err();
        assert!(e.contains("Settings → Connectors → Slack"), "{e}");
        assert!(
            e.contains("SLACK_BOT_TOKEN") && e.contains("SLACK_USER_TOKEN"),
            "{e}"
        );
        // Whitespace-only settings and env values count as missing.
        let blank = |_: &str| Some("  ".to_string());
        let cfg = SlackCfg {
            enabled: true,
            token: " ".into(),
            user_token: "".into(),
        };
        assert!(creds(&cfg, &blank).is_err());
    }

    #[test]
    fn search_uses_only_the_user_token() {
        let c = both();
        assert_eq!(c.token_for("search").unwrap(), (USER, true));
        assert_eq!(c.token_for("history").unwrap(), (BOT, false));
        let bot_only = Creds {
            bot: Some((BOT.into(), "settings")),
            user: None,
        };
        let e = bot_only.token_for("search").unwrap_err();
        assert!(e.contains("user token") && e.contains("search:read"), "{e}");
        assert!(
            e.contains("Settings → Connectors → Slack → user token"),
            "{e}"
        );
        assert!(
            e.contains("SLACK_USER_TOKEN") && e.contains("history"),
            "{e}"
        );
        assert!(!e.contains(BOT));
        // The user token is never a fallback for bot actions.
        let user_only = Creds {
            bot: None,
            user: Some((USER.into(), "settings")),
        };
        let e = user_only.token_for("post_message").unwrap_err();
        assert!(e.contains("bot token") && !e.contains(USER), "{e}");
        assert!(user_only.token_for("search").is_ok());
    }

    #[test]
    fn swapped_tokens_get_a_note_but_are_not_blocked() {
        let c = Creds {
            bot: Some((USER.into(), "settings")),
            user: Some((BOT.into(), "settings")),
        };
        let n = c.note();
        assert!(n.contains("bot-token slot holds a user token"), "{n}");
        assert!(n.contains("user-token slot holds a bot token"), "{n}");
        assert!(!n.contains(USER) && !n.contains(BOT));
        assert!(c.token_for("history").is_ok());
        assert!(both().note().is_empty());
        let odd = Creds {
            bot: Some(("not-a-slack-token".into(), "settings")),
            user: None,
        };
        assert!(odd.note().contains("does not look like"));
    }

    #[test]
    fn read_requests_are_gets_with_encoded_queries() {
        let r = req_auth_test();
        assert!(r.body.is_none());
        assert_eq!(r.url().unwrap().as_str(), "https://slack.com/api/auth.test");

        let u = req_list_channels(20, Some("dXNlcjpVMDYx"), true)
            .url()
            .unwrap();
        assert_eq!(
            u.as_str(),
            "https://slack.com/api/conversations.list?types=public_channel%2Cprivate_channel&exclude_archived=true&limit=20&cursor=dXNlcjpVMDYx"
        );
        let u = req_list_channels(5, Some("  "), false).url().unwrap();
        assert!(!u.as_str().contains("cursor"), "{u}");

        let u = req_history("C0123ABCD", 7, Some("1700000000"), None, None)
            .url()
            .unwrap();
        assert_eq!(
            u.as_str(),
            "https://slack.com/api/conversations.history?channel=C0123ABCD&limit=7&oldest=1700000000"
        );
        let u = req_replies("C0123ABCD", "1712345678.123456", 20, None)
            .url()
            .unwrap();
        assert!(u
            .as_str()
            .ends_with("?channel=C0123ABCD&ts=1712345678.123456&limit=20"));

        // User text cannot add parameters or a fragment.
        let r = req_search(
            "in:#general deploy & rollback \"x\" ?token=1#frag",
            10,
            2,
            Some("timestamp"),
        );
        let u = r.url().unwrap();
        assert_eq!(u.host_str(), Some("slack.com"));
        assert_eq!(u.path(), "/api/search.messages");
        assert!(u.fragment().is_none());
        let q: Vec<(String, String)> = u.query_pairs().into_owned().collect();
        assert_eq!(
            q[0],
            (
                "query".into(),
                "in:#general deploy & rollback \"x\" ?token=1#frag".into()
            )
        );
        assert!(q.contains(&("count".into(), "10".into())));
        assert!(q.contains(&("page".into(), "2".into())));
        assert!(q.contains(&("sort".into(), "timestamp".into())));
        assert!(!q.iter().any(|(k, _)| k == "token"));
        // An unknown sort is dropped rather than sent.
        assert!(!req_search("x", 1, 1, Some("bogus"))
            .url()
            .unwrap()
            .as_str()
            .contains("sort"));

        assert!(req_lookup_email("a+b@example.com")
            .url()
            .unwrap()
            .as_str()
            .ends_with("users.lookupByEmail?email=a%2Bb%40example.com"));
        assert!(req_user_info("U012AB3CD")
            .url()
            .unwrap()
            .as_str()
            .ends_with("users.info?user=U012AB3CD"));
        assert!(req_users_list(3, Some("c1"))
            .url()
            .unwrap()
            .as_str()
            .ends_with("users.list?limit=3&cursor=c1"));
        assert!(req_permalink("C1", "1.2")
            .url()
            .unwrap()
            .as_str()
            .ends_with("chat.getPermalink?channel=C1&message_ts=1.2"));
    }

    #[test]
    fn write_requests_are_json_posts() {
        let r = req_post("C0123ABCD", Some("hi \"there\"\n<@U1>"), None, None, true);
        assert_eq!(r.api, "chat.postMessage");
        assert!(r.query.is_empty());
        assert_eq!(
            r.body,
            Some(json!({"channel":"C0123ABCD","text":"hi \"there\"\n<@U1>"})),
            "reply_broadcast only applies to thread replies"
        );
        assert_eq!(
            r.url().unwrap().as_str(),
            "https://slack.com/api/chat.postMessage"
        );

        let blocks = json!([{"type":"section","text":{"type":"mrkdwn","text":"*hi*"}}]);
        let r = req_post(
            "C1",
            Some("hi"),
            Some(blocks.clone()),
            Some("1712345678.123456"),
            true,
        );
        let b = r.body.unwrap();
        assert_eq!(b["blocks"], blocks);
        assert_eq!(b["thread_ts"], "1712345678.123456");
        assert_eq!(b["reply_broadcast"], true);
        let b = req_post("C1", Some("x"), None, Some("1.2"), false)
            .body
            .unwrap();
        assert!(b.get("reply_broadcast").is_none());

        let r = req_react("C1", "1712345678.123456", ":white_check_mark:");
        assert_eq!(r.api, "reactions.add");
        assert_eq!(
            r.body,
            Some(json!({"channel":"C1","timestamp":"1712345678.123456","name":"white_check_mark"}))
        );
        assert_eq!(emoji_name(" :+1::skin-tone-2: "), "+1::skin-tone-2");
    }

    #[test]
    fn blocks_accept_arrays_or_json_strings() {
        let a = json!([{"type":"divider"}]);
        assert_eq!(parse_blocks(&a).unwrap(), Some(a.clone()));
        assert_eq!(
            parse_blocks(&json!(a.to_string())).unwrap(),
            Some(a.clone())
        );
        assert_eq!(
            parse_blocks(&json!({"blocks": a})).unwrap(),
            Some(a.clone())
        );
        assert_eq!(parse_blocks(&Value::Null).unwrap(), None);
        assert_eq!(parse_blocks(&json!([])).unwrap(), None);
        assert!(parse_blocks(&json!("not json")).is_err());
        assert!(parse_blocks(&json!(5)).is_err());
    }

    #[test]
    fn channel_refs_and_timestamps() {
        assert_eq!(
            channel_ref("C0123ABCD").unwrap(),
            ChannelRef::Id("C0123ABCD".into())
        );
        assert_eq!(
            channel_ref("U012AB3CD").unwrap(),
            ChannelRef::Id("U012AB3CD".into())
        );
        assert_eq!(
            channel_ref("#General").unwrap(),
            ChannelRef::Name("general".into())
        );
        assert_eq!(
            channel_ref("dev-ops").unwrap(),
            ChannelRef::Name("dev-ops".into())
        );
        assert_eq!(
            channel_ref("<#C0123ABCD|general>").unwrap(),
            ChannelRef::Id("C0123ABCD".into())
        );
        assert_eq!(
            channel_ref("https://acme.slack.com/archives/C0123ABCD/p1712345678123456").unwrap(),
            ChannelRef::Id("C0123ABCD".into())
        );
        assert!(channel_ref("  ").is_err());

        assert_eq!(norm_ts("1712345678.123456"), "1712345678.123456");
        assert_eq!(norm_ts("p1712345678123456"), "1712345678.123456");
        assert_eq!(
            norm_ts("https://acme.slack.com/archives/C0123ABCD/p1712345678123456?thread_ts=1"),
            "1712345678.123456"
        );

        assert_eq!(ts_human("1712345678.123456"), "2024-04-05 19:34 UTC");
        assert_eq!(ts_human("garbage"), "?");
        assert_eq!(
            to_slack_ts("1712345678.123456").unwrap(),
            "1712345678.123456"
        );
        assert_eq!(to_slack_ts("2024-04-05").unwrap(), "1712275200");
        assert_eq!(to_slack_ts("2024-04-05T19:34:38Z").unwrap(), "1712345678");
        assert_eq!(to_slack_ts("2024-04-05 00:01").unwrap(), "1712275260");
        assert!(to_slack_ts("yesterday").is_err());
    }

    #[test]
    fn limits_default_and_clamp() {
        assert_eq!(limit_of(&json!({}), &["limit"]), 20);
        assert_eq!(limit_of(&json!({"limit": 0}), &["limit"]), 1);
        assert_eq!(limit_of(&json!({"limit": 5000}), &["limit"]), 100);
        assert_eq!(limit_of(&json!({"limit": "7"}), &["limit"]), 7);
        assert_eq!(
            limit_of(&json!({"limit": 3, "count": 9}), &["count", "limit"]),
            9
        );
    }

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            body: body.into(),
        }
    }

    #[test]
    fn ok_false_errors_carry_hints() {
        let c = both();
        let sec = c.secrets();
        let e = check_ok(
            "conversations.history",
            json!({"ok":false,"error":"not_in_channel"}),
            &sec,
            "",
        )
        .unwrap_err();
        assert!(e.contains("not_in_channel") && e.contains("/invite"), "{e}");

        let e = check_ok(
            "conversations.history",
            json!({"ok":false,"error":"missing_scope","needed":"channels:history","provided":"chat:write,channels:read"}),
            &sec,
            "",
        )
        .unwrap_err();
        assert!(e.contains("needed: channels:history"), "{e}");
        assert!(e.contains("chat:write,channels:read"), "{e}");

        let e = check_ok(
            "x",
            json!({"ok":false,"error":"channel_not_found"}),
            &sec,
            "",
        )
        .unwrap_err();
        assert!(e.contains("list_channels"), "{e}");
        for code in ["invalid_auth", "not_authed", "token_revoked"] {
            let e =
                check_ok("auth.test", json!({"ok":false,"error":code}), &sec, " NOTE").unwrap_err();
            assert!(
                e.contains("Settings → Connectors → Slack") && e.ends_with("NOTE"),
                "{e}"
            );
        }
        let e = check_ok("x", json!({"ok":false,"error":"ratelimited"}), &sec, "").unwrap_err();
        assert!(e.contains("rate limit"), "{e}");
        let e = check_ok(
            "chat.postMessage",
            json!({"ok":false,"error":"invalid_blocks","response_metadata":{"messages":["[ERROR] missing required field: text"]}}),
            &sec,
            "",
        )
        .unwrap_err();
        assert!(e.contains("missing required field"), "{e}");
        // An unknown code still comes through.
        let e = check_ok("x", json!({"ok":false,"error":"weird_thing"}), &sec, "").unwrap_err();
        assert!(e.contains("weird_thing"));
        let e = check_ok("x", json!({"nothing":1}), &sec, "").unwrap_err();
        assert!(e.contains("unknown_error"));
        assert!(check_ok("x", json!({"ok":true,"a":1}), &sec, "").is_ok());
    }

    #[test]
    fn errors_never_contain_a_token() {
        let c = both();
        let sec = c.secrets();
        // A server (or proxy) that echoes the token back.
        let echo = format!("bad token {BOT} / {USER}");
        let e = check_http("auth.test", &reply(401, &echo), &sec, "").unwrap_err();
        assert!(
            e.contains("401") && e.contains("Settings → Connectors → Slack"),
            "{e}"
        );
        assert!(!e.contains(BOT) && !e.contains(USER), "{e}");
        let e = check_http("auth.test", &reply(200, &echo), &sec, "").unwrap_err();
        assert!(
            e.contains("non-JSON") && !e.contains(BOT) && !e.contains(USER),
            "{e}"
        );
        let e = check_ok(
            "x",
            json!({"ok":false,"error":BOT,"response_metadata":{"messages":[USER]}}),
            &sec,
            "",
        )
        .unwrap_err();
        assert!(!e.contains(BOT) && !e.contains(USER), "{e}");
        let e = check_http("x", &reply(503, "<html>down</html>"), &sec, "").unwrap_err();
        assert!(e.contains("503") && e.contains("retry"), "{e}");
        assert!(check_http("x", &reply(200, r#"{"ok":false,"error":"x"}"#), &sec, "").is_ok());
    }

    #[test]
    fn mentions_links_and_escapes_render() {
        let mut users = HashMap::new();
        users.insert("U01ADA".to_string(), "ada".to_string());
        let t = "<@U01ADA> and <@U02BOB|bob> and <@U03ZED> in <#C0123ABCD|general> / <#C9>: \
                 <!here> <!subteam^S1|@oncall> <!subteam^S2> <https://x.dev/a?b=1&amp;c=2|docs> \
                 <https://y.dev> <mailto:a@b.com|a@b.com> 1 &lt; 2 &amp;&amp; 3 &gt; 2";
        assert_eq!(
            render_text(t, &users),
            "@ada and @bob and @U03ZED in #general / #C9: @here @oncall @group docs (https://x.dev/a?b=1&c=2) https://y.dev a@b.com 1 < 2 && 3 > 2"
        );
        assert_eq!(render_text("", &users), "");
    }

    const HISTORY: &str = r#"{"ok":true,"messages":[
        {"type":"message","user":"U01ADA","text":"Deploy done <@U02BOB>","ts":"1712345678.123456","thread_ts":"1712345678.123456","reply_count":3,"latest_reply":"1712349999.000100","reactions":[{"name":"tada","count":2,"users":["U02BOB","U03"]}]},
        {"type":"message","subtype":"bot_message","bot_id":"B01","username":"CI","text":"","attachments":[{"fallback":"Build #42 passed"}],"ts":"1712340000.000200"},
        {"type":"message","subtype":"channel_join","user":"U02BOB","text":"<@U02BOB> has joined the channel","ts":"1712330000.000300"},
        {"type":"message","user":"U02BOB","text":"see file\nline two","ts":"1712320000.000400","files":[{"id":"F1","name":"log.txt"}],"edited":{"user":"U02BOB","ts":"1712320001.0"}}
    ],"has_more":true,"response_metadata":{"next_cursor":"bmV4dF90czoxNzEy"}}"#;

    #[test]
    fn history_messages_format() {
        let v: Value = serde_json::from_str(HISTORY).unwrap();
        let msgs = v["messages"].as_array().unwrap().clone();
        let mut users = HashMap::new();
        assert_eq!(user_ids(&msgs, &users), vec!["U01ADA", "U02BOB"]);
        users.insert("U01ADA".to_string(), "Ada".to_string());
        assert_eq!(user_ids(&msgs, &users), vec!["U02BOB"]);
        users.insert("U02BOB".to_string(), "Bob".to_string());

        let m0 = fmt_message(&msgs[0], &users);
        assert_eq!(
            m0,
            "1712345678.123456 · 2024-04-05 19:34 UTC · @Ada: Deploy done @Bob [:tada: 2] [thread: 3 replies, latest 2024-04-05 20:46 UTC — replies thread_ts=1712345678.123456]"
        );
        let m1 = fmt_message(&msgs[1], &users);
        assert!(m1.contains("CI (bot): Build #42 passed"), "{m1}");
        assert!(!m1.contains("[bot_message]"), "{m1}");
        let m2 = fmt_message(&msgs[2], &users);
        assert!(
            m2.contains("@Bob: @Bob has joined the channel [channel_join]"),
            "{m2}"
        );
        let m3 = fmt_message(&msgs[3], &users);
        assert!(
            m3.contains("see file\n    line two [edited] [files: log.txt]"),
            "{m3}"
        );
        // Unresolved authors keep their id.
        assert!(fmt_message(&msgs[0], &no_users()).contains("@U01ADA: Deploy done @U02BOB"));
        assert!(next_cursor(&v).as_deref() == Some("bmV4dF90czoxNzEy"));
        // A message with nothing in it still formats.
        assert_eq!(fmt_message(&json!({}), &no_users()), "? · ? · (unknown): ");
    }

    #[test]
    fn long_messages_are_clipped() {
        let m = json!({"user":"U1","text":"x".repeat(MSG_CLIP + 500),"ts":"1.0"});
        let out = fmt_message(&m, &no_users());
        assert!(out.contains("[… truncated]"));
        assert!(out.len() < MSG_CLIP + 200);
    }

    const CHANNELS: &str = r#"{"ok":true,"channels":[
        {"id":"C0123ABCD","name":"general","is_channel":true,"is_private":false,"is_archived":false,"is_member":true,"num_members":42,"topic":{"value":"Company-wide\nannouncements"},"purpose":{"value":"x"}},
        {"id":"G0456EFGH","name":"secret-proj","is_private":true,"is_member":false,"num_members":1,"topic":{"value":""},"purpose":{"value":"Skunkworks <#C0123ABCD|general>"}},
        {"id":"C0789IJKL","name":"random"}
    ],"response_metadata":{"next_cursor":""}}"#;

    #[test]
    fn channels_format() {
        let v: Value = serde_json::from_str(CHANNELS).unwrap();
        let chs = v["channels"].as_array().unwrap();
        let out = fmt_channels(chs, next_cursor(&v).as_deref());
        assert_eq!(
            out,
            "3 channels (1 with the bot as a member — it can only read those):\n\
             #general (C0123ABCD) · 42 members · bot is a member · Company-wide announcements\n\
             #secret-proj (G0456EFGH) · 1 member · private · bot not a member · Skunkworks #general\n\
             #random (C0789IJKL) · bot not a member"
        );
        let more = fmt_channels(&chs[..1], Some("abc"));
        assert!(
            more.ends_with("More available: pass cursor=\"abc\"."),
            "{more}"
        );
        assert!(fmt_channels(&[], None).starts_with("No channels"));
    }

    const USERS: &str = r#"{"ok":true,"members":[
        {"id":"U01ADA","name":"ada","real_name":"Ada Lovelace","tz":"Europe/London","is_admin":true,"profile":{"display_name":"ada","real_name":"Ada Lovelace","title":"Engineer","email":"ada@example.com","status_text":"Heads down","status_emoji":":headphones:"}},
        {"id":"U02BOT","name":"ci-bot","is_bot":true,"profile":{"real_name":"CI"}},
        {"id":"U03OLD","name":"gone","deleted":true,"profile":{}}
    ],"response_metadata":{"next_cursor":"dXNlcjpVMDM="}}"#;

    #[test]
    fn users_format() {
        let v: Value = serde_json::from_str(USERS).unwrap();
        let us = v["members"].as_array().unwrap();
        let out = fmt_users(us, next_cursor(&v).as_deref());
        assert_eq!(
            out,
            "3 users:\n\
             @ada (U01ADA) · Ada Lovelace · Engineer · ada@example.com · admin\n\
             @ci-bot (U02BOT) · CI · bot\n\
             @gone (U03OLD) · deactivated\n\
             More available: pass cursor=\"dXNlcjpVMDM=\"."
        );
        let d = fmt_user_detail(&us[0]);
        assert!(d.contains("Time zone: Europe/London"), "{d}");
        assert!(d.contains("Status: :headphones: Heads down"), "{d}");
        assert_eq!(user_name(&us[0]), "ada");
        assert_eq!(user_name(&us[1]), "CI");
        assert_eq!(user_name(&us[2]), "gone");
        assert_eq!(user_name(&json!({})), "?");
        assert_eq!(fmt_user_detail(&json!({})), "@? (?)");
        assert_eq!(fmt_users(&[], None), "No users.");
    }

    const SEARCH: &str = r#"{"ok":true,"query":"deploy","messages":{"total":57,
        "matches":[
          {"type":"message","user":"U01ADA","username":"ada","text":"deploy at 5 <@U02BOB>","ts":"1712345678.123456","channel":{"id":"C0123ABCD","name":"general","is_im":false},"permalink":"https://acme.slack.com/archives/C0123ABCD/p1712345678123456"},
          {"type":"message","user":"U02BOB","username":"bob","text":"ok","ts":"1712345679.000001","channel":{"id":"D0123ABCD","is_im":true}}
        ],
        "paging":{"count":2,"total":57,"page":1,"pages":29}}}"#;

    #[test]
    fn search_format() {
        let v: Value = serde_json::from_str(SEARCH).unwrap();
        let out = fmt_search(&v, "deploy", &no_users());
        assert!(
            out.starts_with("2 of 57 matches for \"deploy\" (page 1/29):\n"),
            "{out}"
        );
        assert!(out.contains("#general (C0123ABCD) · 1712345678.123456 · 2024-04-05 19:34 UTC · @ada: deploy at 5 @U02BOB\n    https://acme.slack.com/archives/C0123ABCD/p1712345678123456"), "{out}");
        assert!(out.contains("DM (D0123ABCD) · 1712345679.000001"), "{out}");
        assert!(out.ends_with("More available: pass page=2."), "{out}");
        let empty = json!({"ok":true,"messages":{"total":0,"matches":[]}});
        assert_eq!(
            fmt_search(&empty, "nope", &no_users()),
            "No messages match \"nope\"."
        );
    }

    #[test]
    fn auth_test_formats_identity() {
        let v = json!({"ok":true,"url":"https://acme.slack.com/","team":"Acme","user":"openleash","team_id":"T1","user_id":"U0BOT","bot_id":"B1"});
        assert_eq!(
            fmt_auth(&v),
            "@openleash (U0BOT, bot) · Acme (https://acme.slack.com/)"
        );
        let u = json!({"ok":true,"team":"Acme","user":"ada","user_id":"U01ADA"});
        assert_eq!(fmt_auth(&u), "@ada (U01ADA) · Acme");
    }

    #[tokio::test]
    async fn run_fails_cleanly_before_any_network() {
        // No credentials at all is reported before any request is built. The
        // env is the real one here, so only assert when it has no Slack token.
        if std::env::var(ENV_BOT).is_err() && std::env::var(ENV_USER).is_err() {
            let http = reqwest::Client::new();
            let e = run(&http, &SlackCfg::default(), &json!({"action":"whoami"}))
                .await
                .unwrap_err();
            assert!(e.contains("Settings → Connectors → Slack"), "{e}");
        }
        // Search with only a bot token is refused locally, without a request.
        let http = reqwest::Client::new();
        let cfg = SlackCfg {
            enabled: true,
            token: BOT.into(),
            user_token: String::new(),
        };
        if std::env::var(ENV_USER).is_err() {
            let e = run(&http, &cfg, &json!({"action":"search","query":"x"}))
                .await
                .unwrap_err();
            assert!(e.contains("user token") && !e.contains(BOT), "{e}");
        }
    }
}
