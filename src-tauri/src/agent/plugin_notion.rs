//! The Notion plugin: one `notion` tool with an `action` enum.
//!
//! Wired into the harness through `plugin_services.rs` (schema, permissions,
//! dispatch, settings). Off by default; reads run freely, writes ask.
//!
//! Layout: pure request builders (`*_request` / `*_body`), pure formatters
//! (`format_*`, `blocks_to_text`, `property_to_text`, `text_to_blocks`) and a
//! thin network layer behind the private `Source` trait, so every action can be
//! exercised in tests against fixture JSON without a network.

use super::plugin_services::{self, clip, Identity};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NotionCfg {
    /// Off unless the user switches it on (Settings → Connectors).
    pub enabled: bool,
    /// Internal integration secret (ntn_… / secret_…). Empty = NOTION_TOKEN.
    pub token: String,
}

/// Hand-written so a token never reaches a log line or a test failure message.
impl std::fmt::Debug for NotionCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotionCfg")
            .field("enabled", &self.enabled)
            .field(
                "token",
                &if self.token.is_empty() {
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
    "search",
    "get_page",
    "get_database",
    "query_database",
    "get_blocks",
];

/// Actions that change something on Notion (need approval).
pub const WRITES: &[&str] = &["create_page", "append_blocks", "update_page"];

const API: &str = "https://api.notion.com/v1";

/// Pinned on purpose. `2025-09-03` splits databases into "data sources"
/// (queries move to `/data_sources/{id}/query`, page parents gain
/// `data_source_id`), which would change every database action here and the
/// ids users paste. `2022-06-28` is still served, stable, and what nearly all
/// docs and examples describe. Bump it only together with those code paths.
const NOTION_VERSION: &str = "2022-06-28";

const ENV_KEYS: &[&str] = &["NOTION_TOKEN"];
const LABEL: &str = "Notion";
/// Whole-result ceiling, like the GitHub plugin's.
const MAX_OUT: usize = 80_000;
/// Nested levels below the top-level blocks that `get_page` loads.
const MAX_NEST: usize = 3;
/// Total blocks / requests one `get_page` may spend; a huge page would
/// otherwise cost dozens of round-trips and flood the context.
const BLOCK_BUDGET: usize = 400;
const REQUEST_BUDGET: usize = 25;
/// Notion accepts at most 100 children per append/create request.
const CHILDREN_PER_REQUEST: usize = 100;
/// Notion's per-rich-text-object content limit (counted like JS string length,
/// i.e. UTF-16 code units), and its per-array limit.
const RICH_MAX: usize = 2000;
const RICH_ITEMS_MAX: usize = 100;

const SHARE_HINT: &str = "An integration only sees pages and databases shared with it: in Notion open the page or database → ••• → Connections → add this integration (children inherit access). Also check the id.";

pub fn schema() -> Value {
    json!({
        "name": "notion",
        "description": "Read and edit the user's Notion workspace through the Notion API (as the user's integration). Ids may be given as the 32-hex id, a dashed UUID, or a full Notion URL. Actions:\n\
    - whoami: the integration and workspace the token belongs to\n\
    - search (query, filter page|database, limit, cursor): pages/databases by title → `title · type · id · url · edited <time>`\n\
    - get_page (page_id): properties + content as markdown-ish text (headings, lists, to-dos `- [ ]`, toggles `▸`, quotes, callouts, code, tables as `|` rows, files as links, child pages/databases with ids). Nested blocks load up to 3 levels and ~400 blocks; the rest is marked `not loaded` with how to fetch it\n\
    - get_blocks (block_id, cursor): the children of any block or page, rendered the same way (for content get_page marked as not loaded / more)\n\
    - get_database (database_id): schema — property names, types, select/status options\n\
    - query_database (database_id, filter, sorts, limit, cursor): rows `title · id · url · Prop: value …`; `filter`/`sorts` are Notion's JSON, passed through (use the property names from get_database)\n\
    - create_page (parent_page_id OR database_id, title, properties, content or children, icon): `properties` (Notion property-value JSON) only for database parents; `content` is markdown-ish text → blocks\n\
    - append_blocks (block_id or page_id, content or children, after): adds blocks at the end (or after block `after`); long content is sent in chunks of 100 blocks\n\
    - update_page (page_id, properties, title, icon): change properties/title/icon. Archiving or trashing is not supported.\n\
    `content` syntax: `# `/`## `/`### ` headings, `- `/`* ` bullets, `1. ` numbered, `- [ ] `/`- [x] ` to-dos, `> ` quotes, ``` fenced code (with language), `---` dividers; other lines become paragraphs. Inline formatting is sent as plain text. Lists return `[more available — pass cursor: …]` when truncated (default limit 20, max 100). If something is not found, it is usually not shared with the integration (page ••• → Connections). Writes (create_page, append_blocks, update_page) ask the user first unless allowed.",
        "input_schema": {"type":"object","properties":{
            "action":{"type":"string","enum":["whoami","search","get_page","get_blocks","get_database","query_database","create_page","append_blocks","update_page"]},
            "query":{"type":"string","description":"search: text to match in titles (empty = recently edited)"},
            "filter":{"description":"search: \"page\" or \"database\". query_database: a Notion filter object, e.g. {\"property\":\"Status\",\"status\":{\"equals\":\"Done\"}}"},
            "sorts":{"type":"array","description":"query_database: Notion sorts, e.g. [{\"property\":\"Due\",\"direction\":\"ascending\"}]"},
            "limit":{"type":"integer","description":"Max results (default 20, max 100)"},
            "cursor":{"type":"string","description":"Continue a listing from the cursor a previous call returned"},
            "page_id":{"type":"string","description":"Page id or URL (get_page, update_page; append_blocks target)"},
            "database_id":{"type":"string","description":"Database id or URL (get_database, query_database; create_page parent)"},
            "block_id":{"type":"string","description":"Block or page id/URL (get_blocks, append_blocks)"},
            "parent_page_id":{"type":"string","description":"create_page: the parent page id or URL"},
            "title":{"type":"string","description":"create_page / update_page: page title"},
            "content":{"type":"string","description":"create_page / append_blocks: markdown-ish text converted to Notion blocks"},
            "children":{"type":"array","description":"create_page / append_blocks: raw Notion block objects, passed through instead of `content`"},
            "properties":{"type":"object","description":"create_page (database parent) / update_page: Notion property values keyed by property name, passed through, e.g. {\"Status\":{\"status\":{\"name\":\"Done\"}}}"},
            "icon":{"type":"string","description":"create_page / update_page: an emoji, or an https image URL"},
            "after":{"type":"string","description":"append_blocks: insert after this block id instead of at the end (max 100 blocks)"}},
            "required":["action"]}
    })
}

/// Whether this call changes something on Notion. Fails closed.
pub fn is_write(input: &Value) -> bool {
    let a = input["action"].as_str().unwrap_or("");
    !READS.contains(&a)
}

// ───────────────────────────── credentials ─────────────────────────────

/// The token and where it came from. No `Debug`: nothing may print it.
struct Creds {
    token: String,
    source: &'static str,
}

fn creds(cfg: &NotionCfg, env: &dyn Fn(&str) -> Option<String>) -> Result<Creds, String> {
    plugin_services::setting_or_env(&cfg.token, ENV_KEYS, env)
        .map(|(token, source)| Creds { token, source })
        .ok_or_else(|| {
            "Notion is not connected: paste an internal integration secret in Settings → Connectors → Notion, or set the NOTION_TOKEN environment variable. Create one at https://www.notion.so/profile/integrations, then share pages with it (page ••• → Connections).".to_string()
        })
}

// ───────────────────────────── ids ─────────────────────────────

/// A Notion id from a raw 32-hex id, a dashed UUID or a Notion URL, as the
/// dashed lowercase UUID the API documents. In a URL the id is the trailing 32
/// hex of the last path segment (`…/My-Page-<id>`); a `p=<id>` query (a page
/// peeked inside a database view) wins, while `v=` (a view id) and `#<block>`
/// are ignored.
pub fn parse_id(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let bad = || {
        format!(
            "`{}` is not a Notion id: pass the 32-character id (with or without dashes) or the page/database URL.",
            clip(s, 120)
        )
    };
    let (before_frag, _) = s.split_once('#').unwrap_or((s, ""));
    let (path, query) = before_frag.split_once('?').unwrap_or((before_frag, ""));
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("p=") {
            if let Some(id) = id_from_segment(v) {
                return Ok(id);
            }
        }
    }
    let seg = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    id_from_segment(seg).ok_or_else(bad)
}

fn id_from_segment(seg: &str) -> Option<String> {
    let b = seg.as_bytes();
    let hex = |x: &[u8]| x.iter().all(u8::is_ascii_hexdigit);
    let compact = if b.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| b[i] == b'-')
        && seg.bytes().filter(|c| *c == b'-').count() == 4
        && hex(&seg.bytes().filter(|c| *c != b'-').collect::<Vec<_>>())
    {
        seg.replace('-', "")
    } else if b.len() >= 32 && hex(&b[b.len() - 32..]) && (b.len() == 32 || b[b.len() - 33] == b'-')
    {
        // The last 32 bytes are ASCII, so this slice is on a char boundary.
        seg[seg.len() - 32..].to_string()
    } else {
        return None;
    };
    let t = compact.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &t[0..8],
        &t[8..12],
        &t[12..16],
        &t[16..20],
        &t[20..32]
    ))
}

/// The id from the first of `keys` (then `id`) that is set.
fn id_param(input: &Value, keys: &[&str]) -> Result<String, String> {
    keys.iter()
        .chain(std::iter::once(&"id"))
        .find_map(|k| input[*k].as_str().map(str::trim).filter(|v| !v.is_empty()))
        .map(parse_id)
        .unwrap_or_else(|| {
            Err(format!(
                "`{}` is required (a Notion id or URL).",
                keys.first().copied().unwrap_or("id")
            ))
        })
}

fn opt_id(input: &Value, key: &str) -> Result<Option<String>, String> {
    match input[key].as_str().map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => parse_id(v).map(Some),
        None => Ok(None),
    }
}

/// A JSON parameter given either as JSON or as a string holding JSON (models
/// do both).
fn json_param(input: &Value, key: &str) -> Result<Option<Value>, String> {
    match &input[key] {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => serde_json::from_str(s)
            .map(Some)
            .map_err(|e| format!("`{key}` must be JSON: {e}")),
        v => Ok(Some(v.clone())),
    }
}

fn limit(input: &Value) -> u64 {
    input["limit"].as_u64().unwrap_or(20).clamp(1, 100)
}

fn cursor(input: &Value) -> Option<&str> {
    input["cursor"]
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty())
}

// ───────────────────────────── requests ─────────────────────────────

/// One API request, built purely so tests can check it. `path` is relative to
/// `API` and only ever contains ids that went through `parse_id`.
#[derive(Debug, Clone, PartialEq)]
struct Req {
    method: &'static str,
    path: String,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
}

impl Req {
    fn get(path: impl Into<String>) -> Self {
        Req {
            method: "GET",
            path: path.into(),
            query: vec![],
            body: None,
        }
    }
    fn post(path: impl Into<String>, body: Value) -> Self {
        Req {
            method: "POST",
            path: path.into(),
            query: vec![],
            body: Some(body),
        }
    }
    fn patch(path: impl Into<String>, body: Value) -> Self {
        Req {
            method: "PATCH",
            path: path.into(),
            query: vec![],
            body: Some(body),
        }
    }
    fn what(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

fn search_request(input: &Value) -> Result<Req, String> {
    let mut body = json!({"page_size": limit(input)});
    let q = input["query"].as_str().map(str::trim).unwrap_or("");
    if !q.is_empty() {
        body["query"] = json!(q);
    } else {
        // Without a query Notion's order is arbitrary; newest first is what
        // "what's in my workspace" wants. With one, keep its relevance order.
        body["sort"] = json!({"direction": "descending", "timestamp": "last_edited_time"});
    }
    match &input["filter"] {
        Value::Null => {}
        Value::String(f) if f.trim().is_empty() => {}
        Value::String(f) => {
            let f = f.trim().to_ascii_lowercase();
            if f != "page" && f != "database" {
                return Err(format!(
                    "search `filter` must be \"page\" or \"database\", not `{f}`."
                ));
            }
            body["filter"] = json!({"property": "object", "value": f});
        }
        Value::Object(_) => body["filter"] = input["filter"].clone(),
        _ => return Err("search `filter` must be \"page\" or \"database\".".into()),
    }
    if let Some(c) = cursor(input) {
        body["start_cursor"] = json!(c);
    }
    Ok(Req::post("/search", body))
}

fn query_database_request(input: &Value) -> Result<Req, String> {
    let id = id_param(input, &["database_id"])?;
    let mut body = json!({"page_size": limit(input)});
    if let Some(f) = json_param(input, "filter")? {
        if !f.is_object() {
            return Err("query_database `filter` must be a Notion filter object.".into());
        }
        body["filter"] = f;
    }
    if let Some(s) = json_param(input, "sorts")? {
        let s = if s.is_object() { json!([s]) } else { s };
        if !s.is_array() {
            return Err("query_database `sorts` must be an array of sort objects.".into());
        }
        body["sorts"] = s;
    }
    if let Some(c) = cursor(input) {
        body["start_cursor"] = json!(c);
    }
    Ok(Req::post(format!("/databases/{id}/query"), body))
}

fn children_request(block_id: &str, cursor: Option<&str>, page_size: usize) -> Req {
    let mut r = Req::get(format!("/blocks/{block_id}/children"));
    r.query
        .push(("page_size", page_size.clamp(1, 100).to_string()));
    if let Some(c) = cursor {
        r.query.push(("start_cursor", c.to_string()));
    }
    r
}

enum Parent {
    Page(String),
    Database(String),
}

/// Whether passthrough properties already set the title property.
fn has_title_value(props: &Option<Value>) -> bool {
    props
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|m| m.values().any(|v| v.get("title").is_some()))
}

fn create_page_body(
    parent: &Parent,
    title: Option<&str>,
    props: Option<Value>,
    title_prop: Option<&str>,
    children: &[Value],
    icon: Option<&str>,
) -> Result<Value, String> {
    let mut body = json!({});
    match parent {
        Parent::Page(id) => {
            if props
                .as_ref()
                .and_then(Value::as_object)
                .is_some_and(|m| !m.is_empty())
            {
                return Err("A page under a page only has a title — `properties` is for pages in a database (pass `database_id`). Use `title` instead.".into());
            }
            let title = title.ok_or("create_page under a page needs a `title`.")?;
            body["parent"] = json!({"page_id": id});
            body["properties"] = json!({"title": {"title": title_rich(title)}});
        }
        Parent::Database(id) => {
            body["parent"] = json!({"database_id": id});
            let mut p = match props {
                Some(Value::Object(m)) => Value::Object(m),
                None => json!({}),
                Some(_) => {
                    return Err("`properties` must be an object keyed by property name.".into())
                }
            };
            if let (Some(t), Some(name)) = (title, title_prop) {
                p[name] = json!({"title": title_rich(t)});
            }
            body["properties"] = p;
        }
    }
    if !children.is_empty() {
        body["children"] = json!(children);
    }
    if let Some(i) = icon {
        body["icon"] = icon_value(i);
    }
    Ok(body)
}

/// The append requests for `blocks`, at most 100 per request as Notion
/// requires. `after` positions the blocks; with more than one chunk the later
/// chunks would need the id of the previous chunk's last block, so that case is
/// refused instead of risking out-of-order content.
fn append_requests(id: &str, blocks: &[Value], after: Option<&str>) -> Result<Vec<Req>, String> {
    if blocks.is_empty() {
        return Err("Nothing to append: pass `content` text or `children` blocks.".into());
    }
    if after.is_some() && blocks.len() > CHILDREN_PER_REQUEST {
        return Err(format!(
            "`after` works with up to {CHILDREN_PER_REQUEST} blocks per call; this is {}. Split the content, or leave out `after` to append at the end.",
            blocks.len()
        ));
    }
    Ok(blocks
        .chunks(CHILDREN_PER_REQUEST)
        .map(|c| {
            let mut body = json!({"children": c});
            if let Some(a) = after {
                body["after"] = json!(a);
            }
            Req::patch(format!("/blocks/{id}/children"), body)
        })
        .collect())
}

/// Keys that would archive/trash a page. Refused outright: deleting content
/// is out of scope for this tool, and silently ignoring them would let the
/// model believe it had done so.
const REFUSED_UPDATE_KEYS: &[&str] = &["archived", "in_trash"];

fn refuse_trash(input: &Value) -> Result<(), String> {
    if let Some(k) = REFUSED_UPDATE_KEYS.iter().find(|k| !input[**k].is_null()) {
        return Err(format!("update_page does not support `{k}`: archiving or trashing pages is not available in this tool. Ask the user to do it in Notion."));
    }
    Ok(())
}

fn update_page_body(input: &Value, title_prop: Option<&str>) -> Result<Value, String> {
    refuse_trash(input)?;
    let mut props = match json_param(input, "properties")? {
        None => json!({}),
        Some(Value::Object(m)) => Value::Object(m),
        Some(_) => return Err("`properties` must be an object keyed by property name.".into()),
    };
    if let Some(t) = input["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let name = title_prop.ok_or("Could not find the page's title property.")?;
        props[name] = json!({"title": title_rich(t)});
    }
    let mut body = json!({});
    if props.as_object().is_some_and(|m| !m.is_empty()) {
        body["properties"] = props;
    }
    if let Some(i) = input["icon"]
        .as_str()
        .map(str::trim)
        .filter(|i| !i.is_empty())
    {
        body["icon"] = icon_value(i);
    }
    if body.as_object().is_some_and(|m| m.is_empty()) {
        return Err("update_page needs `properties`, `title` or `icon`.".into());
    }
    Ok(body)
}

fn icon_value(icon: &str) -> Value {
    let i = icon.trim();
    if i.starts_with("https://") {
        json!({"type": "external", "external": {"url": i}})
    } else {
        json!({"type": "emoji", "emoji": i})
    }
}

// ───────────────────────────── network ─────────────────────────────

/// Where requests go: the real API, or a fixture in tests.
trait Source {
    async fn call(&self, rq: &Req) -> Result<Value, String>;
}

struct Api<'a> {
    http: &'a reqwest::Client,
    token: &'a str,
}

impl Source for Api<'_> {
    async fn call(&self, rq: &Req) -> Result<Value, String> {
        let method = match rq.method {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PATCH" => reqwest::Method::PATCH,
            m => return Err(format!("Unsupported method {m}.")),
        };
        let mut b = self
            .http
            .request(method, format!("{API}{}", rq.path))
            .bearer_auth(self.token)
            .header("Notion-Version", NOTION_VERSION)
            .header("user-agent", "OpenLeash/0.1");
        if !rq.query.is_empty() {
            b = b.query(&rq.query);
        }
        if let Some(body) = &rq.body {
            b = b.json(body);
        }
        let r = plugin_services::send(LABEL, b, &[self.token]).await?;
        parse_reply(r.status, &r.body, &rq.what(), self.token)
    }
}

fn parse_reply(status: u16, body: &str, what: &str, token: &str) -> Result<Value, String> {
    if !(200..300).contains(&status) {
        return Err(api_error(status, body, what, &[token]));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(body).map_err(|e| format!("Notion sent bad JSON: {e}"))
}

/// Notion errors are `{object:"error", status, code, message}`; the code picks
/// the hint.
fn api_error(status: u16, body: &str, what: &str, secrets: &[&str]) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v["code"].as_str().unwrap_or("");
    let msg = v["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| body.chars().take(400).collect());
    let hint = match (code, status) {
        ("unauthorized", _) | (_, 401) => "The token is missing or invalid — check Settings → Connectors → Notion (or NOTION_TOKEN).",
        ("restricted_resource" | "object_not_found", _) | (_, 403 | 404) => SHARE_HINT,
        ("rate_limited", _) => "Notion's rate limit was hit — wait a little before calling again.",
        ("validation_error", _) | (_, 400) => "Notion rejected the request as invalid — check property names and types against get_database, and that page ids are pages and database ids are databases.",
        ("conflict_error", _) | (_, 409) => "The page changed while saving — fetch it again and retry.",
        (_, 500..=599) => "Notion is having trouble right now — try again shortly.",
        _ => "",
    };
    let shown = if code.is_empty() {
        msg
    } else {
        format!("{} ({code})", msg.trim())
    };
    plugin_services::http_error(LABEL, status, what, &shown, hint, secrets)
}

// ───────────────────────────── text helpers ─────────────────────────────

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Rich text as plain text (Notion's own `plain_text`, which already renders
/// mentions and equations).
pub fn rich_text_to_plain(rt: &Value) -> String {
    rt.as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            t["plain_text"]
                .as_str()
                .or_else(|| t["text"]["content"].as_str())
                .unwrap_or("")
        })
        .collect()
}

/// Rich text with links kept as `[text](url)`, for block content.
fn rich_text_md(rt: &Value) -> String {
    rt.as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let text = t["plain_text"]
                .as_str()
                .or_else(|| t["text"]["content"].as_str())
                .unwrap_or("");
            match t["href"].as_str().filter(|h| !h.is_empty() && *h != text) {
                Some(h) => format!("[{text}]({h})"),
                None => text.to_string(),
            }
        })
        .collect()
}

fn one_line(t: &str, n: usize) -> String {
    let t = t.replace(['\n', '\r'], " ");
    if t.chars().count() <= n {
        t
    } else {
        t.chars().take(n).collect::<String>() + "…"
    }
}

fn num(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn date(v: &Value) -> String {
    let start = s(&v["start"]);
    match v["end"].as_str() {
        Some(e) if !e.is_empty() => format!("{start} → {e}"),
        _ => start.to_string(),
    }
}

fn user(u: &Value) -> String {
    match u["name"].as_str().filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => format!("user {}", s(&u["id"])),
    }
}

fn names(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["name"].as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One property value as text: `Done`, `a, b`, `2024-01-01 → 2024-01-05`, …
/// Empty when the value is empty or of a type with no text form.
pub fn property_to_text(p: &Value) -> String {
    let ty = s(&p["type"]);
    let v = &p[ty];
    match ty {
        "title" | "rich_text" => rich_text_to_plain(v),
        "number" => num(v),
        "select" | "status" => s(&v["name"]).to_string(),
        "multi_select" => names(v),
        "date" => date(v),
        "people" => v
            .as_array()
            .into_iter()
            .flatten()
            .map(user)
            .collect::<Vec<_>>()
            .join(", "),
        "checkbox" => if v.as_bool() == Some(true) {
            "yes"
        } else {
            "no"
        }
        .into(),
        "url" | "email" | "phone_number" | "created_time" | "last_edited_time" => s(v).to_string(),
        "formula" => match s(&v["type"]) {
            "string" => s(&v["string"]).to_string(),
            "number" => num(&v["number"]),
            "boolean" => match v["boolean"].as_bool() {
                Some(true) => "yes".into(),
                Some(false) => "no".into(),
                None => String::new(),
            },
            "date" => date(&v["date"]),
            _ => String::new(),
        },
        "relation" => {
            let mut ids: Vec<&str> = v
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["id"].as_str())
                .collect();
            if p["has_more"] == true {
                ids.push("…");
            }
            ids.join(", ")
        }
        "rollup" => match s(&v["type"]) {
            "number" => num(&v["number"]),
            "date" => date(&v["date"]),
            "array" => v["array"]
                .as_array()
                .into_iter()
                .flatten()
                .map(property_to_text)
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        },
        "created_by" | "last_edited_by" => user(v),
        "unique_id" => match (
            v["prefix"].as_str().filter(|p| !p.is_empty()),
            num(&v["number"]),
        ) {
            (_, n) if n.is_empty() => String::new(),
            (Some(pre), n) => format!("{pre}-{n}"),
            (None, n) => n,
        },
        "files" => v
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let url = file_url(f);
                let name = s(&f["name"]);
                if url.is_empty() {
                    name.to_string()
                } else {
                    format!("[{name}]({url})")
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        "verification" => s(&v["state"]).to_string(),
        _ => String::new(),
    }
}

/// A file object's URL (`external.url`, or Notion-hosted `file.url`, which
/// expires after an hour).
fn file_url(f: &Value) -> &str {
    f["external"]["url"]
        .as_str()
        .or_else(|| f["file"]["url"].as_str())
        .unwrap_or("")
}

/// A page's or database's title.
fn title_of(o: &Value) -> String {
    let t = if o["title"].is_array() {
        rich_text_to_plain(&o["title"])
    } else {
        o["properties"]
            .as_object()
            .and_then(|m| m.values().find(|p| p["type"] == "title"))
            .map(|p| rich_text_to_plain(&p["title"]))
            .unwrap_or_default()
    };
    let t = one_line(t.trim(), 200);
    if t.is_empty() {
        "(untitled)".into()
    } else {
        t
    }
}

/// The name of the title property (of a page or a database schema).
fn title_prop_name(o: &Value) -> Option<String> {
    o["properties"]
        .as_object()?
        .iter()
        .find(|(_, p)| p["type"] == "title")
        .map(|(k, _)| k.clone())
}

fn gone(o: &Value) -> &'static str {
    if o["in_trash"] == true || o["archived"] == true {
        " [archived]"
    } else {
        ""
    }
}

fn more_note(v: &Value) -> String {
    match (v["has_more"] == true, v["next_cursor"].as_str()) {
        (true, Some(c)) => format!("\n[more available — pass cursor: \"{c}\"]"),
        _ => String::new(),
    }
}

// ───────────────────────────── formatting ─────────────────────────────

fn identity_line(me: &Value) -> String {
    let name = me["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or("(unnamed integration)");
    let mut out = if me["type"] == "bot" {
        format!("{name} (integration)")
    } else {
        let email = s(&me["person"]["email"]);
        if email.is_empty() {
            name.to_string()
        } else {
            format!("{name} <{email}>")
        }
    };
    if let Some(ws) = me["bot"]["workspace_name"]
        .as_str()
        .filter(|w| !w.is_empty())
    {
        out.push_str(&format!(" · {ws}"));
    }
    out
}

fn search_row(o: &Value) -> String {
    format!(
        "{}{} · {} · {} · {} · edited {}",
        title_of(o),
        gone(o),
        s(&o["object"]),
        s(&o["id"]),
        s(&o["url"]),
        s(&o["last_edited_time"])
    )
}

fn format_search(v: &Value) -> String {
    let rows: Vec<String> = v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(search_row)
        .collect();
    if rows.is_empty() {
        return "No pages or databases matched. (Search only covers what is shared with the integration — page ••• → Connections.)".into();
    }
    rows.join("\n") + &more_note(v)
}

fn query_row(p: &Value) -> String {
    let mut row = format!(
        "{}{} · {} · {}",
        title_of(p),
        gone(p),
        s(&p["id"]),
        s(&p["url"])
    );
    let mut shown = 0;
    for (k, prop) in p["properties"].as_object().into_iter().flatten() {
        if prop["type"] == "title" {
            continue;
        }
        let t = property_to_text(prop);
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if shown == 12 {
            row.push_str(" · …");
            break;
        }
        row.push_str(&format!(" · {k}: {}", one_line(t, 120)));
        shown += 1;
    }
    row
}

fn format_query(v: &Value) -> String {
    let rows: Vec<String> = v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(query_row)
        .collect();
    if rows.is_empty() {
        return "No rows matched.".into();
    }
    rows.join("\n") + &more_note(v)
}

fn format_database(db: &Value) -> String {
    let mut out = format!(
        "[database] {}{} · {} · {}\n",
        title_of(db),
        gone(db),
        s(&db["id"]),
        s(&db["url"])
    );
    let desc = rich_text_to_plain(&db["description"]);
    if !desc.trim().is_empty() {
        out.push_str(&format!("Description: {}\n", clip(desc.trim(), 2000)));
    }
    let mut props: Vec<(&String, &Value)> =
        db["properties"].as_object().into_iter().flatten().collect();
    // Title first, then by name: the title is what create_page's `title` sets.
    props.sort_by_key(|(k, p)| (p["type"] != "title", k.to_lowercase()));
    out.push_str(&format!("Properties ({}):\n", props.len()));
    for (name, p) in props {
        let ty = s(&p["type"]);
        let d = &p[ty];
        let detail = match ty {
            "select" | "multi_select" | "status" => {
                let opts: Vec<&str> = d["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o["name"].as_str())
                    .collect();
                if opts.is_empty() {
                    String::new()
                } else {
                    let more = if opts.len() > 50 { " | …" } else { "" };
                    format!(": {}{more}", opts[..opts.len().min(50)].join(" | "))
                }
            }
            "relation" => format!(" → database {}", s(&d["database_id"])),
            "formula" => format!(": {}", one_line(s(&d["expression"]), 200)),
            "number" => match s(&d["format"]) {
                "" | "number" => String::new(),
                f => format!(", format {f}"),
            },
            "rollup" => format!(
                ": {} of {}.{}",
                s(&d["function"]),
                s(&d["relation_property_name"]),
                s(&d["rollup_property_name"])
            ),
            "unique_id" => match d["prefix"].as_str() {
                Some(p) if !p.is_empty() => format!(", prefix {p}"),
                _ => String::new(),
            },
            _ => String::new(),
        };
        out.push_str(&format!("- {name} ({ty}){detail}\n"));
    }
    out
}

fn format_page_header(page: &Value) -> String {
    let parent = &page["parent"];
    let parent = match s(&parent["type"]) {
        "database_id" => format!("database {}", s(&parent["database_id"])),
        "page_id" => format!("page {}", s(&parent["page_id"])),
        "block_id" => format!("block {}", s(&parent["block_id"])),
        "workspace" => "workspace".into(),
        other => other.to_string(),
    };
    let mut out = format!(
        "[page] {}{}\nid: {} · url: {} · parent: {} · last edited: {}\n",
        title_of(page),
        gone(page),
        s(&page["id"]),
        s(&page["url"]),
        parent,
        s(&page["last_edited_time"])
    );
    let mut props: Vec<String> = vec![];
    for (k, p) in page["properties"].as_object().into_iter().flatten() {
        if p["type"] == "title" {
            continue;
        }
        let t = property_to_text(p);
        if !t.trim().is_empty() {
            props.push(format!("  {k}: {}", clip(t.trim(), 2000)));
        }
    }
    if !props.is_empty() {
        out.push_str("Properties:\n");
        out.push_str(&props.join("\n"));
        out.push('\n');
    }
    out
}

/// Block children fetched so far, keyed by parent id, plus parents whose list
/// was cut (with the cursor to continue) or could not be loaded.
#[derive(Default)]
struct Tree {
    kids: HashMap<String, Vec<Value>>,
    more: HashMap<String, String>,
    errors: HashMap<String, String>,
}

/// Blocks as markdown-ish text. Nested children are not part of a block
/// object, so blocks with `has_children` show a "not loaded" marker here;
/// `get_page` renders the fetched tree through the same code.
pub fn blocks_to_text(blocks: &[Value]) -> String {
    let mut out = String::new();
    render(blocks, &Tree::default(), 0, &mut out);
    out
}

fn render(blocks: &[Value], tree: &Tree, depth: usize, out: &mut String) {
    let ind = "  ".repeat(depth);
    let mut n = 0;
    for b in blocks {
        let ty = s(&b["type"]);
        n = if ty == "numbered_list_item" { n + 1 } else { 0 };
        let id = s(&b["id"]);
        let line = block_line(b, ty, n, tree);
        if let Some(l) = &line {
            for x in l.split('\n') {
                out.push_str(&ind);
                out.push_str(x);
                out.push('\n');
            }
        }
        // Child pages/databases are separate documents (fetch them by id);
        // a table's rows were rendered by `block_line`.
        if b["has_children"] != true || matches!(ty, "child_page" | "child_database" | "table") {
            continue;
        }
        // Containers without a line of their own (columns, synced blocks)
        // don't indent their content.
        let cd = if line.is_some() { depth + 1 } else { depth };
        let cind = "  ".repeat(cd);
        if let Some(e) = tree.errors.get(id) {
            out.push_str(&format!(
                "{cind}[nested blocks could not be loaded: {}]\n",
                one_line(e, 300)
            ));
            continue;
        }
        match tree.kids.get(id) {
            Some(k) => render(k, tree, cd, out),
            None => out.push_str(&format!(
                "{cind}[… nested blocks not loaded — get_blocks with block_id {id}]\n"
            )),
        }
        if let Some(c) = tree.more.get(id) {
            out.push_str(&format!(
                "{cind}[… more nested blocks — get_blocks with block_id {id}, cursor \"{c}\"]\n"
            ));
        }
    }
}

fn row_text(cells: &Value) -> String {
    let cells: Vec<String> = cells
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| one_line(&rich_text_md(c), 500).replace('|', "\\|"))
        .collect();
    format!("| {} |", cells.join(" | "))
}

fn table_text(b: &Value, tree: &Tree) -> String {
    let id = s(&b["id"]);
    let Some(rows) = tree.kids.get(id) else {
        return format!("[table — rows not loaded; get_blocks with block_id {id}]");
    };
    let width = b["table"]["table_width"].as_u64().unwrap_or(1).max(1) as usize;
    let mut out: Vec<String> = vec![];
    for (i, r) in rows.iter().enumerate() {
        out.push(row_text(&r["table_row"]["cells"]));
        if i == 0 && b["table"]["has_column_header"] == true {
            out.push(format!("|{}", " --- |".repeat(width)));
        }
    }
    if let Some(c) = tree.more.get(id) {
        out.push(format!(
            "[… more rows — get_blocks with block_id {id}, cursor \"{c}\"]"
        ));
    }
    out.join("\n")
}

/// The text of one block, or `None` for pure containers.
fn block_line(b: &Value, ty: &str, n: usize, tree: &Tree) -> Option<String> {
    let d = &b[ty];
    let txt = || rich_text_md(&d["rich_text"]);
    let caption = || {
        let c = rich_text_to_plain(&d["caption"]);
        if c.trim().is_empty() {
            String::new()
        } else {
            format!(" — {}", c.trim())
        }
    };
    Some(match ty {
        "paragraph" | "template" => txt(),
        "heading_1" => format!("# {}", txt()),
        "heading_2" => format!("## {}", txt()),
        "heading_3" => format!("### {}", txt()),
        "bulleted_list_item" => format!("- {}", txt()),
        "numbered_list_item" => format!("{n}. {}", txt()),
        "to_do" => format!(
            "- [{}] {}",
            if d["checked"] == true { "x" } else { " " },
            txt()
        ),
        "toggle" => format!("▸ {}", txt()),
        "quote" => format!("> {}", txt()),
        "callout" => {
            let icon = d["icon"]["emoji"].as_str().unwrap_or("💡");
            format!("> {icon} {}", txt())
        }
        "code" => {
            let lang = match s(&d["language"]) {
                "plain text" => "",
                l => l,
            };
            format!("```{lang}\n{}\n```", rich_text_to_plain(&d["rich_text"]))
        }
        "divider" => "---".into(),
        "image" | "video" | "audio" | "file" | "pdf" => {
            let cap = rich_text_to_plain(&d["caption"]);
            let label = [cap.trim(), s(&d["name"])]
                .into_iter()
                .find(|x| !x.is_empty())
                .unwrap_or(ty)
                .to_string();
            format!("[{ty}: {}]({})", one_line(&label, 200), file_url(d))
        }
        "bookmark" | "embed" | "link_preview" => {
            format!("[{ty}] {}{}", s(&d["url"]), caption())
        }
        "child_page" => format!("[child page] {} (id {})", s(&d["title"]), s(&b["id"])),
        "child_database" => format!(
            "[child database] {} (id {} — get_database / query_database)",
            s(&d["title"]),
            s(&b["id"])
        ),
        "equation" => format!("$${}$$", s(&d["expression"])),
        "link_to_page" => {
            let t = s(&d["type"]);
            format!("[link to {}] {}", t.trim_end_matches("_id"), s(&d[t]))
        }
        "table_of_contents" => "[table of contents]".into(),
        "table" => table_text(b, tree),
        "table_row" => row_text(&d["cells"]),
        "breadcrumb" | "column_list" | "column" | "synced_block" => return None,
        other => format!("[{other} block]"),
    })
}

// ───────────────────────────── text → blocks ─────────────────────────────

/// Split into pieces of at most `max` UTF-16 code units, the unit Notion's
/// 2000-character limit counts in (an emoji outside the BMP counts twice).
fn chunk_utf16(text: &str, max: usize) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut len = 0;
    for c in text.chars() {
        let l = c.len_utf16();
        if len + l > max {
            out.push(std::mem::take(&mut cur));
            len = 0;
        }
        cur.push(c);
        len += l;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn rich_item(t: &str) -> Value {
    json!({"type": "text", "text": {"content": t}})
}

/// A title's rich text (at most 100 chunks of 2000).
fn title_rich(t: &str) -> Vec<Value> {
    chunk_utf16(t, RICH_MAX)
        .iter()
        .take(RICH_ITEMS_MAX)
        .map(|c| rich_item(c))
        .collect()
}

/// Block(s) of type `ty` holding `text`; text beyond one block's 100 rich-text
/// items continues in further blocks of the same type.
fn text_blocks(ty: &str, text: &str, extra: Value) -> Vec<Value> {
    let chunks = chunk_utf16(text, RICH_MAX);
    let groups: Vec<&[String]> = if chunks.is_empty() {
        vec![&[]]
    } else {
        chunks.chunks(RICH_ITEMS_MAX).collect()
    };
    groups
        .into_iter()
        .map(|g| {
            let mut inner =
                json!({"rich_text": g.iter().map(|c| rich_item(c)).collect::<Vec<_>>()});
            for (k, v) in extra.as_object().into_iter().flatten() {
                inner[k] = v.clone();
            }
            let mut b = json!({"object": "block", "type": ty});
            b[ty] = inner;
            b
        })
        .collect()
}

/// Notion only accepts its own language names for code blocks (anything else
/// is a validation error), so common fence tags are mapped and the rest
/// become "plain text".
fn notion_language(tag: &str) -> &'static str {
    const NATIVE: &[&str] = &[
        "abap",
        "arduino",
        "bash",
        "basic",
        "c",
        "clojure",
        "coffeescript",
        "c++",
        "c#",
        "css",
        "dart",
        "diff",
        "docker",
        "elixir",
        "elm",
        "erlang",
        "flow",
        "fortran",
        "f#",
        "gherkin",
        "glsl",
        "go",
        "graphql",
        "groovy",
        "haskell",
        "html",
        "java",
        "javascript",
        "json",
        "julia",
        "kotlin",
        "latex",
        "less",
        "lisp",
        "livescript",
        "lua",
        "makefile",
        "markdown",
        "markup",
        "matlab",
        "mermaid",
        "nix",
        "objective-c",
        "ocaml",
        "pascal",
        "perl",
        "php",
        "plain text",
        "powershell",
        "prolog",
        "protobuf",
        "python",
        "r",
        "reason",
        "ruby",
        "rust",
        "sass",
        "scala",
        "scheme",
        "scss",
        "shell",
        "sql",
        "swift",
        "typescript",
        "vb.net",
        "verilog",
        "vhdl",
        "visual basic",
        "webassembly",
        "xml",
        "yaml",
    ];
    let t = tag.trim().to_ascii_lowercase();
    let t = t.split_whitespace().next().unwrap_or("");
    let alias = match t {
        "js" | "jsx" | "mjs" | "cjs" | "node" => "javascript",
        "ts" | "tsx" => "typescript",
        "py" | "python3" => "python",
        "rs" => "rust",
        "sh" | "zsh" | "console" => "shell",
        "ps" | "ps1" | "pwsh" => "powershell",
        "cpp" | "cc" | "cxx" | "hpp" => "c++",
        "cs" | "csharp" => "c#",
        "fs" | "fsharp" => "f#",
        "yml" => "yaml",
        "md" => "markdown",
        "golang" => "go",
        "rb" => "ruby",
        "kt" | "kts" => "kotlin",
        "dockerfile" => "docker",
        "objc" => "objective-c",
        "tex" => "latex",
        "proto" => "protobuf",
        "make" => "makefile",
        "wasm" => "webassembly",
        "htm" | "svg" => "html",
        "gql" => "graphql",
        "patch" => "diff",
        other => other,
    };
    NATIVE
        .iter()
        .find(|n| **n == alias)
        .copied()
        .unwrap_or("plain text")
}

fn heading(t: &str) -> Option<(usize, &str)> {
    let hashes = t.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) {
        if let Some(rest) = t[hashes..].strip_prefix(' ') {
            return Some((hashes.min(3), rest.trim()));
        }
    }
    None
}

fn numbered(t: &str) -> Option<&str> {
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    t[digits..]
        .strip_prefix(". ")
        .or_else(|| t[digits..].strip_prefix(") "))
}

fn todo(t: &str) -> Option<(bool, &str)> {
    for bullet in ["- ", "* "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            for (mark, checked) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
                if let Some(r) = rest.strip_prefix(mark) {
                    if r.is_empty() || r.starts_with(' ') {
                        return Some((checked, r.trim_start()));
                    }
                }
            }
        }
    }
    None
}

/// Markdown-ish text → Notion blocks. Consecutive plain lines form one
/// paragraph (kept as line breaks), blank lines separate paragraphs, and an
/// unclosed ``` fence runs to the end of the text.
pub fn text_to_blocks(text: &str) -> Vec<Value> {
    fn flush(para: &mut Vec<&str>, out: &mut Vec<Value>) {
        if !para.is_empty() {
            out.extend(text_blocks("paragraph", &para.join("\n"), json!({})));
            para.clear();
        }
    }
    let mut out: Vec<Value> = vec![];
    let mut para: Vec<&str> = vec![];
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("```") {
            flush(&mut para, &mut out);
            let lang = notion_language(rest);
            let mut code: Vec<&str> = vec![];
            for l in lines.by_ref() {
                let lt = l.trim();
                if lt.starts_with("```") && lt.trim_start_matches('`').trim().is_empty() {
                    break;
                }
                code.push(l);
            }
            out.extend(text_blocks(
                "code",
                &code.join("\n"),
                json!({"language": lang}),
            ));
            continue;
        }
        if t.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        let block = if let Some((level, rest)) = heading(t) {
            text_blocks(&format!("heading_{level}"), rest, json!({}))
        } else if let Some((checked, rest)) = todo(t) {
            text_blocks("to_do", rest, json!({"checked": checked}))
        } else if let Some(rest) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        {
            text_blocks("bulleted_list_item", rest.trim(), json!({}))
        } else if let Some(rest) = numbered(t) {
            text_blocks("numbered_list_item", rest.trim(), json!({}))
        } else if t == ">" || t.starts_with("> ") {
            text_blocks("quote", t[1..].trim(), json!({}))
        } else if t == "---" || t == "***" || t == "___" {
            vec![json!({"object": "block", "type": "divider", "divider": {}})]
        } else {
            para.push(t);
            continue;
        };
        flush(&mut para, &mut out);
        out.extend(block);
    }
    flush(&mut para, &mut out);
    out
}

/// Blocks to write: raw `children` passthrough, else `content` text.
fn blocks_param(input: &Value) -> Result<Vec<Value>, String> {
    if let Some(ch) = json_param(input, "children")? {
        return match ch {
            Value::Array(a) => Ok(a),
            _ => Err("`children` must be an array of Notion block objects.".into()),
        };
    }
    Ok(text_to_blocks(input["content"].as_str().unwrap_or("")))
}

// ───────────────────────────── actions ─────────────────────────────

struct Fetched {
    tree: Tree,
    blocks: usize,
    truncated: bool,
}

/// Breadth-first, so when the budget runs out it is the deepest content that
/// is left unloaded rather than the end of the page.
async fn fetch_tree<S: Source>(
    src: &S,
    root: &str,
    start: Option<String>,
) -> Result<Fetched, String> {
    let mut tree = Tree::default();
    let mut queue = VecDeque::from([(root.to_string(), 0usize, start)]);
    let (mut requests, mut total, mut truncated) = (0usize, 0usize, false);
    while let Some((id, depth, mut cur)) = queue.pop_front() {
        if requests >= REQUEST_BUDGET || total >= BLOCK_BUDGET {
            truncated = true;
            continue;
        }
        let mut list = vec![];
        loop {
            let rq = children_request(&id, cur.as_deref(), (BLOCK_BUDGET - total).min(100));
            requests += 1;
            let v = match src.call(&rq).await {
                Ok(v) => v,
                Err(e) if id == root => return Err(e),
                Err(e) => {
                    // One unshared synced block shouldn't sink the whole page.
                    tree.errors.insert(id.clone(), e);
                    cur = None;
                    break;
                }
            };
            let res = v["results"].as_array().cloned().unwrap_or_default();
            total += res.len();
            for b in &res {
                let nested = b["has_children"] == true
                    && depth < MAX_NEST
                    && !matches!(s(&b["type"]), "child_page" | "child_database");
                if let Some(bid) = b["id"].as_str().filter(|_| nested) {
                    queue.push_back((bid.to_string(), depth + 1, None));
                }
            }
            list.extend(res);
            cur = if v["has_more"] == true {
                v["next_cursor"].as_str().map(String::from)
            } else {
                None
            };
            if cur.is_none() {
                break;
            }
            if requests >= REQUEST_BUDGET || total >= BLOCK_BUDGET {
                truncated = true;
                break;
            }
        }
        if let Some(c) = cur {
            tree.more.insert(id.clone(), c);
        }
        if !tree.errors.contains_key(&id) {
            tree.kids.insert(id, list);
        }
    }
    Ok(Fetched {
        tree,
        blocks: total,
        truncated,
    })
}

fn format_content(root: &str, f: &Fetched) -> String {
    let mut out = String::new();
    let top = f.tree.kids.get(root).map(Vec::as_slice).unwrap_or(&[]);
    if top.is_empty() {
        out.push_str("(no content)\n");
    }
    render(top, &f.tree, 0, &mut out);
    if let Some(c) = f.tree.more.get(root) {
        out.push_str(&format!(
            "[more blocks — get_blocks with block_id {root}, cursor \"{c}\"]\n"
        ));
    }
    if f.truncated {
        out.push_str(&format!(
            "[stopped after {} blocks to keep this short; content marked \"not loaded\"/\"more\" can be fetched with get_blocks]\n",
            f.blocks
        ));
    }
    out
}

fn children_count(rq: &Req) -> usize {
    rq.body
        .as_ref()
        .and_then(|b| b["children"].as_array())
        .map_or(0, Vec::len)
}

async fn run_with<S: Source>(src: &S, input: &Value, source: &str) -> Result<String, String> {
    let action = input["action"].as_str().unwrap_or("");
    match action {
        "whoami" => {
            let me = src.call(&Req::get("/users/me")).await?;
            Ok(format!(
                "Signed in to Notion as {} · bot id {} · credentials from {source}",
                identity_line(&me),
                s(&me["id"])
            ))
        }
        "search" => Ok(format_search(&src.call(&search_request(input)?).await?)),
        "get_page" => {
            let id = id_param(input, &["page_id"])?;
            let page = src.call(&Req::get(format!("/pages/{id}"))).await?;
            let f = fetch_tree(src, &id, None).await?;
            Ok(format!(
                "{}\nContent:\n{}",
                format_page_header(&page),
                format_content(&id, &f)
            ))
        }
        "get_blocks" => {
            let id = id_param(input, &["block_id", "page_id"])?;
            let f = fetch_tree(src, &id, cursor(input).map(String::from)).await?;
            Ok(format!("[blocks of {id}]\n{}", format_content(&id, &f)))
        }
        "get_database" => {
            let id = id_param(input, &["database_id"])?;
            Ok(format_database(
                &src.call(&Req::get(format!("/databases/{id}"))).await?,
            ))
        }
        "query_database" => Ok(format_query(
            &src.call(&query_database_request(input)?).await?,
        )),
        "create_page" => {
            let page_parent = opt_id(input, "parent_page_id")?;
            let db_parent = opt_id(input, "database_id")?;
            let parent = match (page_parent, db_parent) {
                (Some(p), None) => Parent::Page(p),
                (None, Some(d)) => Parent::Database(d),
                (Some(_), Some(_)) => {
                    return Err("Pass either `parent_page_id` or `database_id`, not both.".into())
                }
                (None, None) => return Err(
                    "create_page needs `parent_page_id` (a page) or `database_id` (a database)."
                        .into(),
                ),
            };
            let title = input["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let props = json_param(input, "properties")?;
            let blocks = blocks_param(input)?;
            let icon = input["icon"]
                .as_str()
                .map(str::trim)
                .filter(|i| !i.is_empty());
            let (first, rest) = blocks.split_at(blocks.len().min(CHILDREN_PER_REQUEST));
            // Validate before any request (e.g. properties under a page parent).
            create_page_body(&parent, title, props.clone(), Some("title"), first, icon)?;
            // A database's title property can have any name ("Name", "Task"…);
            // look it up rather than guess, unless `properties` already sets it.
            let title_prop = match &parent {
                Parent::Database(d) if title.is_some() && !has_title_value(&props) => {
                    let db = src.call(&Req::get(format!("/databases/{d}"))).await?;
                    Some(title_prop_name(&db).ok_or("That database has no title property.")?)
                }
                _ => None,
            };
            let body = create_page_body(&parent, title, props, title_prop.as_deref(), first, icon)?;
            let page = src.call(&Req::post("/pages", body)).await?;
            let new_id = s(&page["id"]).to_string();
            let mut out = format!(
                "Created page {} · {} · {}",
                title_of(&page),
                new_id,
                s(&page["url"])
            );
            if !rest.is_empty() {
                let mut done = first.len();
                for rq in append_requests(&new_id, rest, None)? {
                    if let Err(e) = src.call(&rq).await {
                        return Err(format!(
                            "{out}, but only {done} of {} blocks were added before an error: {e}",
                            blocks.len()
                        ));
                    }
                    done += children_count(&rq);
                }
            }
            if !blocks.is_empty() {
                out.push_str(&format!(" · {} blocks", blocks.len()));
            }
            Ok(out)
        }
        "append_blocks" => {
            let id = id_param(input, &["block_id", "page_id"])?;
            let after = opt_id(input, "after")?;
            let blocks = blocks_param(input)?;
            let reqs = append_requests(&id, &blocks, after.as_deref())?;
            let mut done = 0;
            for rq in &reqs {
                if let Err(e) = src.call(rq).await {
                    return Err(if done == 0 {
                        e
                    } else {
                        format!("Appended {done} of {} blocks, then: {e}", blocks.len())
                    });
                }
                done += children_count(rq);
            }
            Ok(format!(
                "Appended {done} block{} to {id} in {} request{}.",
                if done == 1 { "" } else { "s" },
                reqs.len(),
                if reqs.len() == 1 { "" } else { "s" }
            ))
        }
        "update_page" => {
            refuse_trash(input)?;
            let id = id_param(input, &["page_id"])?;
            // Validate the rest before spending a request on the title lookup.
            update_page_body(input, Some("title"))?;
            let title_prop = if input["title"]
                .as_str()
                .is_some_and(|t| !t.trim().is_empty())
            {
                let page = src.call(&Req::get(format!("/pages/{id}"))).await?;
                title_prop_name(&page)
            } else {
                None
            };
            let body = update_page_body(input, title_prop.as_deref())?;
            let page = src.call(&Req::patch(format!("/pages/{id}"), body)).await?;
            Ok(format!(
                "Updated page {} · {} · {}",
                title_of(&page),
                s(&page["id"]),
                s(&page["url"])
            ))
        }
        other => Err(format!(
            "Unknown notion action `{other}`. Use one of: {}.",
            READS
                .iter()
                .chain(WRITES)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub async fn run(http: &reqwest::Client, cfg: &NotionCfg, input: &Value) -> Result<String, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let api = Api {
        http,
        token: &c.token,
    };
    let secrets = [c.token.as_str()];
    match run_with(&api, input, c.source).await {
        Ok(out) => Ok(plugin_services::redact(&clip(&out, MAX_OUT), &secrets)),
        Err(e) => Err(plugin_services::redact(&e, &secrets)),
    }
}

pub async fn whoami(http: &reqwest::Client, cfg: &NotionCfg) -> Result<Identity, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let api = Api {
        http,
        token: &c.token,
    };
    let me = api.call(&Req::get("/users/me")).await?;
    Ok(Identity {
        identity: identity_line(&me),
        source: c.source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const PAGE: &str = "59833787-2cf9-4fdf-8782-e53db20768a5";
    const PAGE_HEX: &str = "598337872cf94fdf8782e53db20768a5";
    const DB: &str = "d9824bdc-8445-4327-be8b-5b47500af6ce";
    const TOKEN: &str = "ntn_dummy_token_1234567890";

    type Reply = Box<dyn Fn(&Req) -> Result<Value, String> + Send + Sync>;

    /// A fixture API: answers from `f`, records every request.
    struct Fake {
        f: Reply,
        log: Mutex<Vec<Req>>,
    }

    impl Source for Fake {
        async fn call(&self, rq: &Req) -> Result<Value, String> {
            self.log.lock().unwrap().push(rq.clone());
            (self.f)(rq)
        }
    }

    fn fake(f: impl Fn(&Req) -> Result<Value, String> + Send + Sync + 'static) -> Fake {
        Fake {
            f: Box::new(f),
            log: Mutex::new(vec![]),
        }
    }

    impl Fake {
        fn reqs(&self) -> Vec<Req> {
            self.log.lock().unwrap().clone()
        }
    }

    fn rt(t: &str) -> Value {
        json!([{"type": "text", "text": {"content": t, "link": null}, "plain_text": t, "href": null}])
    }

    fn block(id: &str, ty: &str, inner: Value, has_children: bool) -> Value {
        let mut b = json!({"object": "block", "id": id, "type": ty, "has_children": has_children});
        b[ty] = inner;
        b
    }

    fn para(id: &str, t: &str, kids: bool) -> Value {
        block(
            id,
            "paragraph",
            json!({"rich_text": rt(t), "color": "default"}),
            kids,
        )
    }

    fn list(results: Vec<Value>, next: Option<&str>) -> Value {
        json!({"object": "list", "results": results, "has_more": next.is_some(), "next_cursor": next})
    }

    fn page_fixture() -> Value {
        json!({
            "object": "page", "id": PAGE,
            "created_time": "2024-03-01T10:00:00.000Z", "last_edited_time": "2024-03-02T09:30:00.000Z",
            "archived": false, "in_trash": false,
            "url": "https://www.notion.so/Roadmap-598337872cf94fdf8782e53db20768a5",
            "parent": {"type": "database_id", "database_id": DB},
            "properties": {
                "Name": {"id": "title", "type": "title", "title": rt("Roadmap Q3")},
                "Status": {"id": "s", "type": "status", "status": {"id": "1", "name": "In progress", "color": "blue"}},
                "Tags": {"id": "t", "type": "multi_select", "multi_select": [{"name": "infra"}, {"name": "ui"}]},
                "Due": {"id": "d", "type": "date", "date": {"start": "2024-07-01", "end": null, "time_zone": null}},
                "Empty": {"id": "e", "type": "rich_text", "rich_text": []}
            }
        })
    }

    fn db_fixture() -> Value {
        json!({
            "object": "database", "id": DB, "url": "https://www.notion.so/d9824bdc84454327be8b5b47500af6ce",
            "title": rt("Tasks"), "description": rt("All the tasks"), "archived": false,
            "properties": {
                "Status": {"id": "a", "name": "Status", "type": "status", "status": {"options": [{"name": "Not started"}, {"name": "Done"}], "groups": []}},
                "Task": {"id": "title", "name": "Task", "type": "title", "title": {}},
                "Tags": {"id": "b", "name": "Tags", "type": "multi_select", "multi_select": {"options": [{"name": "infra"}, {"name": "ui"}]}},
                "Project": {"id": "c", "name": "Project", "type": "relation", "relation": {"database_id": "668d797c-76fa-4934-9b05-ad288df2d136", "type": "single_property", "single_property": {}}},
                "Budget": {"id": "d", "name": "Budget", "type": "number", "number": {"format": "dollar"}},
                "Score": {"id": "e", "name": "Score", "type": "formula", "formula": {"expression": "prop(\"Budget\") * 2"}}
            }
        })
    }

    // ───── classification & schema ─────

    #[test]
    fn schema_enum_matches_reads_and_writes() {
        let sc = schema();
        assert_eq!(sc["name"], "notion");
        let en: Vec<&str> = sc["input_schema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for a in &en {
            assert!(READS.contains(a) || WRITES.contains(a), "{a} unclassified");
        }
        for a in READS.iter().chain(WRITES) {
            assert!(en.contains(a), "{a} missing from the enum");
            assert!(
                sc["description"].as_str().unwrap().contains(a),
                "{a} undocumented"
            );
        }
        assert_eq!(en.len(), READS.len() + WRITES.len());
        assert_eq!(sc["input_schema"]["required"], json!(["action"]));
        assert!(READS.iter().all(|r| !WRITES.contains(r)));
    }

    #[test]
    fn is_write_classifies_every_action_and_fails_closed() {
        for a in READS {
            assert!(!is_write(&json!({"action": a})), "{a} is a read");
        }
        for a in WRITES {
            assert!(is_write(&json!({"action": a})), "{a} is a write");
        }
        for bad in [
            json!({}),
            json!({"action": ""}),
            json!({"action": "delete_page"}),
            json!({"action": "SEARCH"}),
            json!({"action": 1}),
            json!({"action": ["search"]}),
        ] {
            assert!(is_write(&bad), "{bad} must be a write");
        }
    }

    // ───── credentials ─────

    #[test]
    fn creds_prefer_settings_then_env() {
        let env = |k: &str| (k == "NOTION_TOKEN").then(|| " ntn_from_env_123 ".to_string());
        let cfg = NotionCfg {
            enabled: true,
            token: " ntn_from_settings_123 ".into(),
        };
        let c = creds(&cfg, &env).unwrap();
        assert_eq!(
            (c.token.as_str(), c.source),
            ("ntn_from_settings_123", "settings")
        );
        let c = creds(&NotionCfg::default(), &env).unwrap();
        assert_eq!(
            (c.token.as_str(), c.source),
            ("ntn_from_env_123", "NOTION_TOKEN")
        );
        let blank = |_: &str| Some("   ".to_string());
        let e = creds(&NotionCfg::default(), &blank).err().unwrap();
        assert!(e.contains("Settings → Connectors → Notion"), "{e}");
        assert!(e.contains("NOTION_TOKEN"), "{e}");
        let none = |_: &str| None;
        assert!(creds(&NotionCfg::default(), &none).is_err());
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("ntn_from_settings"), "{dbg}");
    }

    // ───── ids ─────

    #[test]
    fn parse_id_accepts_hex_uuid_and_urls() {
        for ok in [
            PAGE_HEX.to_string(),
            PAGE.to_string(),
            PAGE_HEX.to_ascii_uppercase(),
            format!("  {PAGE}  "),
            format!("https://www.notion.so/Roadmap-Q3-{PAGE_HEX}"),
            format!("https://www.notion.so/acme/Roadmap-Q3-{PAGE_HEX}?pvs=4"),
            format!("https://www.notion.so/acme/{PAGE_HEX}/"),
            format!("https://acme.notion.site/Roadmap-{PAGE_HEX}#abcdef"),
            format!("notion.so/acme/{PAGE}"),
            format!("https://www.notion.so/acme/d9824bdc84454327be8b5b47500af6ce?v=0123456789abcdef0123456789abcdef&p={PAGE_HEX}&pm=s"),
            format!("https://www.notion.so/Über-uns-{PAGE_HEX}"),
        ] {
            assert_eq!(parse_id(&ok).as_deref(), Ok(PAGE), "{ok}");
        }
        // A database view URL: the path id, not the `v=` view id.
        assert_eq!(
            parse_id("https://www.notion.so/acme/d9824bdc84454327be8b5b47500af6ce?v=0123456789abcdef0123456789abcdef").as_deref(),
            Ok(DB)
        );
        for bad in [
            "",
            "abc",
            "598337872cf94fdf8782e53db20768a",   // 31
            "598337872cf94fdf8782e53db20768az",  // not hex
            "x598337872cf94fdf8782e53db20768a5", // glued to a slug without a dash
            "https://www.notion.so/acme/Roadmap",
            "59833787-2cf9-4fdf-8782-e53db20768a5-",
            "59833787-2cf9-4fdf-87-82e53db20768a5",
            "ünïcode-598337872cf94fdf8782e53db20768a",
            "../users/me",
        ] {
            assert!(parse_id(bad).is_err(), "{bad:?} must be refused");
        }
    }

    // ───── request building ─────

    #[test]
    fn search_request_encodes_query_filter_cursor_limit() {
        let q = "roadmap \"Q3\" & <things>\n";
        let r = search_request(
            &json!({"query": q, "filter": "Database", "limit": 500, "cursor": "abc"}),
        )
        .unwrap();
        assert_eq!((r.method, r.path.as_str()), ("POST", "/search"));
        let b = r.body.unwrap();
        assert_eq!(
            b["query"],
            q.trim(),
            "user text goes in the JSON body as is"
        );
        assert_eq!(
            b["filter"],
            json!({"property": "object", "value": "database"})
        );
        assert_eq!(b["page_size"], 100);
        assert_eq!(b["start_cursor"], "abc");
        assert!(b.get("sort").is_none(), "relevance order for a query");
        let b = search_request(&json!({"limit": 0})).unwrap().body.unwrap();
        assert_eq!(b["page_size"], 1);
        assert!(b.get("query").is_none() && b.get("filter").is_none());
        assert_eq!(b["sort"]["timestamp"], "last_edited_time");
        assert_eq!(
            search_request(&json!({})).unwrap().body.unwrap()["page_size"],
            20
        );
        assert!(search_request(&json!({"filter": "user"})).is_err());
        assert!(search_request(&json!({"filter": 3})).is_err());
    }

    #[test]
    fn query_database_request_passes_filter_and_sorts_through() {
        let filter = json!({"and": [{"property": "Status", "status": {"equals": "Done"}}]});
        let r = query_database_request(&json!({
            "database_id": format!("https://www.notion.so/{}", DB.replace('-', "")),
            "filter": filter, "sorts": [{"property": "Due", "direction": "ascending"}], "limit": 5
        }))
        .unwrap();
        assert_eq!(r.path, format!("/databases/{DB}/query"));
        let b = r.body.unwrap();
        assert_eq!(b["filter"], filter);
        assert_eq!(b["sorts"][0]["property"], "Due");
        assert_eq!(b["page_size"], 5);
        // JSON given as strings, and a lone sort object.
        let b = query_database_request(&json!({"database_id": DB, "filter": filter.to_string(), "sorts": "{\"timestamp\":\"created_time\",\"direction\":\"descending\"}"}))
            .unwrap()
            .body
            .unwrap();
        assert_eq!(b["filter"], filter);
        assert_eq!(b["sorts"][0]["timestamp"], "created_time");
        assert!(query_database_request(&json!({"database_id": DB, "filter": "[1]"})).is_err());
        assert!(query_database_request(&json!({"database_id": DB, "filter": "{nope"})).is_err());
        assert!(query_database_request(&json!({"database_id": DB, "sorts": 3})).is_err());
        assert!(query_database_request(&json!({})).is_err());
        assert!(query_database_request(&json!({"database_id": "../users"})).is_err());
    }

    #[test]
    fn children_request_paginates() {
        let r = children_request(PAGE, Some("cur 1"), 500);
        assert_eq!(r.path, format!("/blocks/{PAGE}/children"));
        assert_eq!(
            r.query,
            vec![
                ("page_size", "100".to_string()),
                ("start_cursor", "cur 1".to_string())
            ]
        );
        assert_eq!(
            children_request(PAGE, None, 0).query,
            vec![("page_size", "1".to_string())]
        );
    }

    #[test]
    fn create_page_body_for_page_and_database_parents() {
        let kids = text_to_blocks("hello");
        let b = create_page_body(
            &Parent::Page(PAGE.into()),
            Some("Notes"),
            None,
            None,
            &kids,
            Some("📝"),
        )
        .unwrap();
        assert_eq!(b["parent"], json!({"page_id": PAGE}));
        assert_eq!(
            b["properties"]["title"]["title"][0]["text"]["content"],
            "Notes"
        );
        assert_eq!(b["children"][0]["type"], "paragraph");
        assert_eq!(b["icon"], json!({"type": "emoji", "emoji": "📝"}));
        // Properties make no sense under a page; a title is required.
        assert!(create_page_body(
            &Parent::Page(PAGE.into()),
            Some("x"),
            Some(json!({"Status": {}})),
            None,
            &[],
            None
        )
        .is_err());
        assert!(create_page_body(&Parent::Page(PAGE.into()), None, None, None, &[], None).is_err());

        let props = json!({"Status": {"status": {"name": "Done"}}});
        let b = create_page_body(
            &Parent::Database(DB.into()),
            Some("Ship it"),
            Some(props),
            Some("Task"),
            &[],
            Some("https://x.test/i.png"),
        )
        .unwrap();
        assert_eq!(b["parent"], json!({"database_id": DB}));
        assert_eq!(b["properties"]["Status"]["status"]["name"], "Done");
        assert_eq!(
            b["properties"]["Task"]["title"][0]["text"]["content"],
            "Ship it"
        );
        assert!(b.get("children").is_none());
        assert_eq!(b["icon"]["external"]["url"], "https://x.test/i.png");
        assert!(create_page_body(
            &Parent::Database(DB.into()),
            None,
            Some(json!([1])),
            None,
            &[],
            None
        )
        .is_err());
        assert!(has_title_value(&Some(json!({"Name": {"title": []}}))));
        assert!(!has_title_value(&Some(json!({"Status": {"status": {}}}))));
    }

    #[test]
    fn append_requests_chunk_by_100() {
        let blocks: Vec<Value> = (0..250).map(|i| para("", &i.to_string(), false)).collect();
        let reqs = append_requests(PAGE, &blocks, None).unwrap();
        let sizes: Vec<usize> = reqs.iter().map(children_count).collect();
        assert_eq!(sizes, vec![100, 100, 50]);
        assert!(reqs
            .iter()
            .all(|r| r.method == "PATCH" && r.path == format!("/blocks/{PAGE}/children")));
        assert_eq!(
            reqs[2].body.as_ref().unwrap()["children"][0]["paragraph"]["rich_text"][0]["text"]
                ["content"],
            "200"
        );
        let one = append_requests(PAGE, &blocks[..3], Some(DB)).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].body.as_ref().unwrap()["after"], DB);
        assert!(
            append_requests(PAGE, &blocks, Some(DB)).is_err(),
            "after + chunking would reorder"
        );
        assert!(append_requests(PAGE, &[], None).is_err());
    }

    #[test]
    fn update_page_refuses_archiving_and_builds_body() {
        for k in ["archived", "in_trash"] {
            for v in [json!(true), json!(false), json!("yes")] {
                let mut input = json!({"action": "update_page", "page_id": PAGE, "properties": {}});
                input[k] = v;
                let e = update_page_body(&input, None).unwrap_err();
                assert!(e.contains(k) && e.contains("not available"), "{e}");
            }
        }
        assert!(update_page_body(&json!({"page_id": PAGE}), None).is_err());
        assert!(update_page_body(&json!({"properties": {}}), None).is_err());
        assert!(update_page_body(&json!({"properties": [1]}), None).is_err());
        let b = update_page_body(
            &json!({"properties": {"Status": {"status": {"name": "Done"}}}, "title": "New", "icon": "🚀"}),
            Some("Name"),
        )
        .unwrap();
        assert_eq!(b["properties"]["Status"]["status"]["name"], "Done");
        assert_eq!(
            b["properties"]["Name"]["title"][0]["text"]["content"],
            "New"
        );
        assert_eq!(b["icon"]["emoji"], "🚀");
        assert!(b.get("archived").is_none() && b.get("in_trash").is_none());
        assert!(update_page_body(&json!({"title": "New"}), None).is_err());
    }

    // ───── text → blocks ─────

    fn content(b: &Value) -> String {
        let ty = b["type"].as_str().unwrap();
        b[ty]["rich_text"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"]["content"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn text_to_blocks_maps_markdown_ish_lines() {
        let src = "# Title\n## Sub\n### Third\n#### Deep\n- one\n* two\n- [ ] todo\n- [x] done\n1. first\n12) twelfth\n> quoted\n---\nplain a\nplain b\n\nnext para\n```ts\nconst a = 1;\n  indented\n```\n#nospace\n";
        let b = text_to_blocks(src);
        let types: Vec<&str> = b.iter().map(|x| x["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec![
                "heading_1",
                "heading_2",
                "heading_3",
                "heading_3",
                "bulleted_list_item",
                "bulleted_list_item",
                "to_do",
                "to_do",
                "numbered_list_item",
                "numbered_list_item",
                "quote",
                "divider",
                "paragraph",
                "paragraph",
                "code",
                "paragraph"
            ]
        );
        assert_eq!(content(&b[0]), "Title");
        assert_eq!(content(&b[3]), "Deep");
        assert_eq!(b[6]["to_do"]["checked"], false);
        assert_eq!(content(&b[6]), "todo");
        assert_eq!(b[7]["to_do"]["checked"], true);
        assert_eq!(content(&b[9]), "twelfth");
        assert_eq!(content(&b[10]), "quoted");
        assert_eq!(content(&b[12]), "plain a\nplain b");
        assert_eq!(content(&b[14]), "const a = 1;\n  indented");
        assert_eq!(b[14]["code"]["language"], "typescript");
        assert_eq!(content(&b[15]), "#nospace");
        assert!(b.iter().all(|x| x["object"] == "block"));
        assert!(text_to_blocks("").is_empty());
        assert!(text_to_blocks("\n\n  \n").is_empty());
    }

    #[test]
    fn code_fences_languages_and_unclosed() {
        let b = text_to_blocks(
            "```\nx\n```\n```Rust\ny\n```\n```toml\nz\n```\n```python\nunclosed\nstill code",
        );
        let langs: Vec<&str> = b
            .iter()
            .map(|x| x["code"]["language"].as_str().unwrap())
            .collect();
        assert_eq!(langs, vec!["plain text", "rust", "plain text", "python"]);
        assert_eq!(content(&b[3]), "unclosed\nstill code");
        assert_eq!(notion_language("c++"), "c++");
        assert_eq!(notion_language("sh"), "shell");
        assert_eq!(notion_language("js {.class}"), "javascript");
        // CRLF input.
        let b = text_to_blocks("# A\r\n- b\r\n");
        assert_eq!(content(&b[0]), "A");
        assert_eq!(content(&b[1]), "b");
    }

    #[test]
    fn rich_text_is_chunked_at_2000_utf16_units() {
        let long = "a".repeat(4500);
        let b = text_to_blocks(&long);
        let rt = b[0]["paragraph"]["rich_text"].as_array().unwrap();
        let lens: Vec<usize> = rt
            .iter()
            .map(|r| r["text"]["content"].as_str().unwrap().len())
            .collect();
        assert_eq!(lens, vec![2000, 2000, 500]);
        // An emoji is 2 UTF-16 units: 1500 of them is 3000 units → 2 chunks.
        let emoji = "😀".repeat(1500);
        let chunks = chunk_utf16(&emoji, RICH_MAX);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.encode_utf16().count() <= RICH_MAX));
        assert_eq!(chunks.concat(), emoji);
        // More than 100 chunks continue in a second block.
        let huge = "b".repeat(RICH_MAX * 101);
        let b = text_to_blocks(&huge);
        assert_eq!(b.len(), 2);
        assert_eq!(
            b[0]["paragraph"]["rich_text"].as_array().unwrap().len(),
            100
        );
        assert_eq!(b[1]["paragraph"]["rich_text"].as_array().unwrap().len(), 1);
        // An empty block still has a rich_text array.
        assert_eq!(
            text_blocks("heading_1", "", json!({}))[0]["heading_1"]["rich_text"],
            json!([])
        );
        assert_eq!(text_to_blocks(">")[0]["quote"]["rich_text"], json!([]));
    }

    // ───── rich text & properties ─────

    #[test]
    fn rich_text_plain_and_links() {
        let r = json!([
            {"type": "text", "plain_text": "See ", "href": null},
            {"type": "text", "plain_text": "docs", "href": "https://example.com/docs"},
            {"type": "mention", "plain_text": "@Ada", "href": null},
            {"type": "text", "text": {"content": "raw"}}
        ]);
        assert_eq!(rich_text_to_plain(&r), "See docs@Adaraw");
        assert_eq!(
            rich_text_md(&r),
            "See [docs](https://example.com/docs)@Adaraw"
        );
        assert_eq!(rich_text_to_plain(&Value::Null), "");
        assert_eq!(rich_text_to_plain(&json!([{}])), "");
    }

    #[test]
    fn property_to_text_covers_every_type() {
        let cases = [
            (json!({"type": "title", "title": rt("T")}), "T"),
            (json!({"type": "rich_text", "rich_text": rt("R")}), "R"),
            (json!({"type": "number", "number": 42}), "42"),
            (json!({"type": "number", "number": 1.5}), "1.5"),
            (json!({"type": "number", "number": null}), ""),
            (
                json!({"type": "select", "select": {"name": "High"}}),
                "High",
            ),
            (json!({"type": "select", "select": null}), ""),
            (
                json!({"type": "status", "status": {"name": "Done"}}),
                "Done",
            ),
            (
                json!({"type": "multi_select", "multi_select": [{"name": "a"}, {"name": "b"}]}),
                "a, b",
            ),
            (
                json!({"type": "date", "date": {"start": "2024-01-01", "end": "2024-01-05"}}),
                "2024-01-01 → 2024-01-05",
            ),
            (json!({"type": "date", "date": null}), ""),
            (
                json!({"type": "people", "people": [{"object": "user", "id": "u1", "name": "Ada"}, {"object": "user", "id": "u2"}]}),
                "Ada, user u2",
            ),
            (json!({"type": "checkbox", "checkbox": true}), "yes"),
            (json!({"type": "checkbox", "checkbox": false}), "no"),
            (
                json!({"type": "url", "url": "https://x.test"}),
                "https://x.test",
            ),
            (json!({"type": "email", "email": "a@x.test"}), "a@x.test"),
            (
                json!({"type": "phone_number", "phone_number": "+1 555"}),
                "+1 555",
            ),
            (
                json!({"type": "formula", "formula": {"type": "string", "string": "s"}}),
                "s",
            ),
            (
                json!({"type": "formula", "formula": {"type": "number", "number": 7}}),
                "7",
            ),
            (
                json!({"type": "formula", "formula": {"type": "boolean", "boolean": false}}),
                "no",
            ),
            (
                json!({"type": "formula", "formula": {"type": "date", "date": {"start": "2024-02-02"}}}),
                "2024-02-02",
            ),
            (
                json!({"type": "relation", "relation": [{"id": "r1"}, {"id": "r2"}], "has_more": true}),
                "r1, r2, …",
            ),
            (
                json!({"type": "rollup", "rollup": {"type": "number", "number": 3, "function": "count"}}),
                "3",
            ),
            (
                json!({"type": "rollup", "rollup": {"type": "array", "array": [{"type": "title", "title": rt("x")}, {"type": "number", "number": 2}]}}),
                "x, 2",
            ),
            (
                json!({"type": "rollup", "rollup": {"type": "date", "date": {"start": "2024-03-03"}}}),
                "2024-03-03",
            ),
            (
                json!({"type": "created_time", "created_time": "2024-01-01T00:00:00.000Z"}),
                "2024-01-01T00:00:00.000Z",
            ),
            (
                json!({"type": "last_edited_time", "last_edited_time": "2024-01-02T00:00:00.000Z"}),
                "2024-01-02T00:00:00.000Z",
            ),
            (
                json!({"type": "created_by", "created_by": {"object": "user", "id": "u1", "name": "Ada"}}),
                "Ada",
            ),
            (
                json!({"type": "last_edited_by", "last_edited_by": {"object": "user", "id": "u9"}}),
                "user u9",
            ),
            (
                json!({"type": "unique_id", "unique_id": {"prefix": "ENG", "number": 12}}),
                "ENG-12",
            ),
            (
                json!({"type": "unique_id", "unique_id": {"prefix": null, "number": 5}}),
                "5",
            ),
            (
                json!({"type": "files", "files": [{"name": "spec.pdf", "type": "file", "file": {"url": "https://s3.test/spec.pdf"}}]}),
                "[spec.pdf](https://s3.test/spec.pdf)",
            ),
            (json!({"type": "button", "button": {}}), ""),
            (json!({}), ""),
        ];
        for (p, want) in cases {
            assert_eq!(property_to_text(&p), want, "{p}");
        }
    }

    // ───── formatting ─────

    #[test]
    fn search_rows_empty_and_more() {
        let v = json!({"object": "list", "results": [
            page_fixture(),
            {"object": "database", "id": DB, "title": rt("Tasks"), "url": "https://www.notion.so/db", "last_edited_time": "2024-01-01T00:00:00.000Z", "archived": true},
            {"object": "page", "id": "x", "properties": {}}
        ], "has_more": true, "next_cursor": "c-2"});
        let out = format_search(&v);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], format!("Roadmap Q3 · page · {PAGE} · https://www.notion.so/Roadmap-598337872cf94fdf8782e53db20768a5 · edited 2024-03-02T09:30:00.000Z"));
        assert!(
            lines[1].starts_with("Tasks [archived] · database · "),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("(untitled) · page · x"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "[more available — pass cursor: \"c-2\"]");
        let empty = format_search(&list(vec![], None));
        assert!(empty.contains("No pages") && empty.contains("Connections"));
    }

    #[test]
    fn database_schema_lists_types_and_options() {
        let out = format_database(&db_fixture());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            format!(
                "[database] Tasks · {DB} · https://www.notion.so/d9824bdc84454327be8b5b47500af6ce"
            )
        );
        assert_eq!(lines[1], "Description: All the tasks");
        assert_eq!(lines[2], "Properties (6):");
        assert_eq!(lines[3], "- Task (title)", "title first");
        assert!(out.contains("- Status (status): Not started | Done"));
        assert!(out.contains("- Tags (multi_select): infra | ui"));
        assert!(
            out.contains("- Project (relation) → database 668d797c-76fa-4934-9b05-ad288df2d136")
        );
        assert!(out.contains("- Budget (number), format dollar"));
        assert!(out.contains("- Score (formula): prop(\"Budget\") * 2"));
        assert!(format_database(&json!({})).contains("Properties (0)"));
    }

    #[test]
    fn query_rows_show_title_and_key_properties() {
        let out = format_query(&list(vec![page_fixture()], Some("next")));
        let first = out.lines().next().unwrap();
        assert_eq!(first, format!("Roadmap Q3 · {PAGE} · https://www.notion.so/Roadmap-598337872cf94fdf8782e53db20768a5 · Due: 2024-07-01 · Status: In progress · Tags: infra, ui"));
        assert!(!first.contains("Empty"), "empty values are skipped");
        assert!(out.ends_with("[more available — pass cursor: \"next\"]"));
        assert_eq!(format_query(&list(vec![], None)), "No rows matched.");
    }

    #[test]
    fn page_header_summarises_properties() {
        let out = format_page_header(&page_fixture());
        assert!(out.starts_with("[page] Roadmap Q3\n"), "{out}");
        assert!(out.contains(&format!("parent: database {DB}")));
        assert!(out.contains("  Status: In progress"));
        assert!(!out.contains("Empty:"));
        let bare = format_page_header(
            &json!({"object": "page", "parent": {"type": "workspace", "workspace": true}, "in_trash": true}),
        );
        assert!(bare.starts_with("[page] (untitled) [archived]"), "{bare}");
        assert!(bare.contains("parent: workspace"));
    }

    #[test]
    fn blocks_to_text_renders_every_block_type() {
        let blocks = vec![
            block(
                "h1",
                "heading_1",
                json!({"rich_text": rt("Top"), "is_toggleable": false}),
                false,
            ),
            block("h2", "heading_2", json!({"rich_text": rt("Mid")}), false),
            block("h3", "heading_3", json!({"rich_text": rt("Low")}), false),
            para("p", "Hello", false),
            block(
                "b",
                "bulleted_list_item",
                json!({"rich_text": rt("bullet")}),
                false,
            ),
            block(
                "n1",
                "numbered_list_item",
                json!({"rich_text": rt("one")}),
                false,
            ),
            block(
                "n2",
                "numbered_list_item",
                json!({"rich_text": rt("two")}),
                false,
            ),
            block(
                "t1",
                "to_do",
                json!({"rich_text": rt("open"), "checked": false}),
                false,
            ),
            block(
                "t2",
                "to_do",
                json!({"rich_text": rt("closed"), "checked": true}),
                false,
            ),
            block(
                "n3",
                "numbered_list_item",
                json!({"rich_text": rt("restart")}),
                false,
            ),
            block("tg", "toggle", json!({"rich_text": rt("More")}), true),
            block("q", "quote", json!({"rich_text": rt("wise")}), false),
            block(
                "c",
                "callout",
                json!({"rich_text": rt("note"), "icon": {"type": "emoji", "emoji": "⚠️"}}),
                false,
            ),
            block(
                "cd",
                "code",
                json!({"rich_text": rt("fn main() {}\nmore"), "language": "rust", "caption": []}),
                false,
            ),
            block("d", "divider", json!({}), false),
            block(
                "i",
                "image",
                json!({"type": "external", "external": {"url": "https://img.test/a.png"}, "caption": rt("Diagram")}),
                false,
            ),
            block(
                "f",
                "file",
                json!({"type": "file", "file": {"url": "https://s3.test/f.zip", "expiry_time": "x"}, "caption": [], "name": "f.zip"}),
                false,
            ),
            block(
                "bm",
                "bookmark",
                json!({"url": "https://example.com", "caption": []}),
                false,
            ),
            block("em", "embed", json!({"url": "https://embed.test"}), false),
            block("cp", "child_page", json!({"title": "Sub page"}), true),
            block("cdb", "child_database", json!({"title": "Sub db"}), false),
            block("eq", "equation", json!({"expression": "e=mc^2"}), false),
            block(
                "ltp",
                "link_to_page",
                json!({"type": "page_id", "page_id": PAGE}),
                false,
            ),
            block("u", "ai_block", json!({}), false),
        ];
        let out = blocks_to_text(&blocks);
        for want in [
            "# Top\n## Mid\n### Low\nHello\n- bullet\n1. one\n2. two\n- [ ] open\n- [x] closed\n1. restart\n▸ More\n",
            "  [… nested blocks not loaded — get_blocks with block_id tg]\n",
            "> wise\n> ⚠️ note\n```rust\nfn main() {}\nmore\n```\n---\n",
            "[image: Diagram](https://img.test/a.png)\n",
            "[file: f.zip](https://s3.test/f.zip)\n",
            "[bookmark] https://example.com\n[embed] https://embed.test\n",
            "[child page] Sub page (id cp)\n[child database] Sub db (id cdb — get_database / query_database)\n",
            "$$e=mc^2$$\n",
            &format!("[link to page] {PAGE}\n"),
            "[ai_block block]\n",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
        assert!(!out.contains("block_id cp"), "child pages are not expanded");
        assert_eq!(blocks_to_text(&[]), "");
    }

    #[test]
    fn nested_tree_tables_columns_and_cursors_render() {
        let mut tree = Tree::default();
        tree.kids
            .insert("tg".into(), vec![para("p1", "inside", true)]);
        tree.kids.insert(
            "p1".into(),
            vec![block(
                "b1",
                "bulleted_list_item",
                json!({"rich_text": rt("deeper")}),
                false,
            )],
        );
        tree.more.insert("tg".into(), "cur-x".into());
        tree.kids.insert(
            "tbl".into(),
            vec![
                block(
                    "r1",
                    "table_row",
                    json!({"cells": [rt("H1"), rt("H|2")]}),
                    false,
                ),
                block("r2", "table_row", json!({"cells": [rt("a"), []]}), false),
            ],
        );
        tree.kids
            .insert("cl".into(), vec![block("col", "column", json!({}), true)]);
        tree.kids
            .insert("col".into(), vec![para("pc", "in column", false)]);
        tree.errors.insert(
            "sy".into(),
            "Notion 404 on GET /blocks/sy/children: nope".into(),
        );
        let blocks = vec![
            block("tg", "toggle", json!({"rich_text": rt("Open me")}), true),
            block(
                "tbl",
                "table",
                json!({"table_width": 2, "has_column_header": true, "has_row_header": false}),
                true,
            ),
            block("cl", "column_list", json!({}), true),
            block(
                "sy",
                "synced_block",
                json!({"synced_from": {"block_id": "z"}}),
                true,
            ),
        ];
        let mut out = String::new();
        render(&blocks, &tree, 0, &mut out);
        assert_eq!(
            out,
            "▸ Open me\n  inside\n    - deeper\n  [… more nested blocks — get_blocks with block_id tg, cursor \"cur-x\"]\n\
             | H1 | H\\|2 |\n| --- | --- |\n| a |  |\n\
             in column\n\
             [nested blocks could not be loaded: Notion 404 on GET /blocks/sy/children: nope]\n"
        );
        // A table whose rows were not fetched says so.
        let t = blocks_to_text(&blocks[1..2]);
        assert!(
            t.contains("rows not loaded; get_blocks with block_id tbl"),
            "{t}"
        );
    }

    #[test]
    fn identity_lines() {
        let me = json!({"object": "user", "id": "b1", "type": "bot", "name": "OpenLeash", "bot": {"owner": {"type": "workspace", "workspace": true}, "workspace_name": "Acme"}});
        assert_eq!(identity_line(&me), "OpenLeash (integration) · Acme");
        assert_eq!(
            identity_line(&json!({"type": "bot", "bot": {}})),
            "(unnamed integration) (integration)"
        );
        assert_eq!(
            identity_line(
                &json!({"type": "person", "name": "Ada", "person": {"email": "ada@x.test"}})
            ),
            "Ada <ada@x.test>"
        );
    }

    // ───── errors ─────

    #[test]
    fn api_errors_map_codes_to_hints_without_the_token() {
        let err = |status: u16, code: &str| {
            let body = json!({"object": "error", "status": status, "code": code, "message": format!("boom with {TOKEN}"), "request_id": "r"}).to_string();
            api_error(status, &body, "GET /pages/x", &[TOKEN])
        };
        let e = err(401, "unauthorized");
        assert!(e.starts_with("Notion 401 on GET /pages/x: "), "{e}");
        assert!(
            e.contains("(unauthorized)") && e.contains("Settings → Connectors → Notion"),
            "{e}"
        );
        for (st, code) in [(404, "object_not_found"), (403, "restricted_resource")] {
            let e = err(st, code);
            assert!(e.contains("Connections") && e.contains(code), "{e}");
        }
        let e = err(400, "validation_error");
        assert!(e.contains("get_database"), "{e}");
        assert!(err(409, "conflict_error").contains("retry"));
        assert!(err(503, "service_unavailable").contains("try again"));
        assert!(err(429, "rate_limited").contains("rate limit"));
        for (st, code) in [
            (401, "unauthorized"),
            (404, "object_not_found"),
            (400, "validation_error"),
            (500, "internal_server_error"),
        ] {
            assert!(!err(st, code).contains(TOKEN), "token leaked for {code}");
        }
        // A non-JSON body (proxy page) is clipped and still redacted.
        let e = api_error(
            502,
            &format!("<html>{TOKEN}{}</html>", "x".repeat(2000)),
            "POST /search",
            &[TOKEN],
        );
        assert!(!e.contains(TOKEN) && e.len() < 1200, "{e}");
        assert!(parse_reply(200, "", "x", TOKEN).unwrap().is_null());
        assert!(parse_reply(200, "{bad", "x", TOKEN).is_err());
        assert_eq!(parse_reply(200, "{\"a\":1}", "x", TOKEN).unwrap()["a"], 1);
        let e = parse_reply(
            404,
            "{\"code\":\"object_not_found\",\"message\":\"no\"}",
            "GET /pages/y",
            TOKEN,
        )
        .unwrap_err();
        assert!(e.contains("Notion 404"), "{e}");
    }

    // ───── actions end to end against fixtures ─────

    #[tokio::test]
    async fn whoami_and_search_actions() {
        let f = fake(|rq| match rq.path.as_str() {
            "/users/me" => Ok(
                json!({"object": "user", "id": "b1", "type": "bot", "name": "OL", "bot": {"workspace_name": "Acme"}}),
            ),
            "/search" => Ok(list(vec![page_fixture()], None)),
            p => Err(format!("unexpected {p}")),
        });
        let out = run_with(&f, &json!({"action": "whoami"}), "NOTION_TOKEN")
            .await
            .unwrap();
        assert_eq!(out, "Signed in to Notion as OL (integration) · Acme · bot id b1 · credentials from NOTION_TOKEN");
        let out = run_with(
            &f,
            &json!({"action": "search", "query": "road", "filter": "page"}),
            "settings",
        )
        .await
        .unwrap();
        assert!(out.starts_with("Roadmap Q3 · page"), "{out}");
        let reqs = f.reqs();
        assert_eq!(reqs[0], Req::get("/users/me"));
        assert_eq!(reqs[1].body.as_ref().unwrap()["filter"]["value"], "page");
        let e = run_with(&f, &json!({"action": "nuke"}), "settings")
            .await
            .unwrap_err();
        assert!(
            e.contains("Unknown notion action `nuke`") && e.contains("get_page"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn get_page_recurses_paginates_and_respects_depth() {
        let f = fake(|rq| {
            let cur = rq
                .query
                .iter()
                .find(|(k, _)| *k == "start_cursor")
                .map(|(_, v)| v.as_str());
            let children = format!("/blocks/{PAGE}/children");
            match (rq.path.as_str(), cur) {
                (p, _) if p == format!("/pages/{PAGE}") => Ok(page_fixture()),
                (p, None) if p == children => Ok(list(
                    vec![
                        para("a", "first", false),
                        block("l1", "toggle", json!({"rich_text": rt("L1")}), true),
                    ],
                    Some("c2"),
                )),
                (p, Some("c2")) if p == children => Ok(list(
                    vec![
                        para("z", "last", false),
                        block("cp", "child_page", json!({"title": "Kid"}), true),
                    ],
                    None,
                )),
                ("/blocks/l1/children", None) => Ok(list(vec![para("l2", "L2", true)], None)),
                ("/blocks/l2/children", None) => Ok(list(vec![para("l3", "L3", true)], None)),
                ("/blocks/l3/children", None) => Ok(list(vec![para("l4", "L4", true)], None)),
                (p, _) => Err(format!("unexpected {p}")),
            }
        });
        let out = run_with(&f, &json!({"action": "get_page", "page_id": format!("https://www.notion.so/x-{PAGE_HEX}")}), "settings")
            .await
            .unwrap();
        assert!(out.starts_with("[page] Roadmap Q3\n"), "{out}");
        assert!(out.contains("Content:\nfirst\n▸ L1\n  L2\n    L3\n      L4\n        [… nested blocks not loaded — get_blocks with block_id l4]\nlast\n[child page] Kid (id cp)\n"), "{out}");
        let paths: Vec<String> = f.reqs().iter().map(|r| r.path.clone()).collect();
        assert!(
            !paths
                .iter()
                .any(|p| p.contains("/blocks/cp/") || p.contains("/blocks/l4/")),
            "{paths:?}"
        );
        assert_eq!(paths.len(), 6, "{paths:?}");
    }

    #[tokio::test]
    async fn get_page_stops_at_the_block_budget() {
        // An endless page: every request returns as many blocks as it asks for.
        let f = fake(|rq| {
            if rq.path.starts_with("/pages/") {
                return Ok(page_fixture());
            }
            let n: usize = rq.query[0].1.parse().unwrap();
            Ok(list(
                (0..n).map(|i| para(&format!("b{i}"), "x", false)).collect(),
                Some("more"),
            ))
        });
        let out = run_with(
            &f,
            &json!({"action": "get_page", "page_id": PAGE}),
            "settings",
        )
        .await
        .unwrap();
        assert!(
            out.contains(&format!(
                "[more blocks — get_blocks with block_id {PAGE}, cursor \"more\"]"
            )),
            "{out}"
        );
        assert!(out.contains("[stopped after 400 blocks"), "{out}");
        assert_eq!(f.reqs().len(), 1 + BLOCK_BUDGET / 100);
        // get_blocks continues from a cursor.
        let f2 = fake(|rq| {
            assert_eq!(rq.query[1], ("start_cursor", "more".to_string()));
            Ok(list(vec![para("q", "continued", false)], None))
        });
        let out = run_with(
            &f2,
            &json!({"action": "get_blocks", "block_id": PAGE, "cursor": "more"}),
            "settings",
        )
        .await
        .unwrap();
        assert_eq!(out, format!("[blocks of {PAGE}]\ncontinued\n"));
    }

    #[tokio::test]
    async fn get_page_errors_propagate_and_nested_errors_do_not() {
        let f = fake(|rq| match rq.path.as_str() {
            p if p.starts_with("/pages/") => Ok(page_fixture()),
            p if p == format!("/blocks/{PAGE}/children") => Ok(list(
                vec![block("s", "synced_block", json!({}), true)],
                None,
            )),
            _ => Err("Notion 404 on GET /blocks/s/children: gone".into()),
        });
        let out = run_with(
            &f,
            &json!({"action": "get_page", "page_id": PAGE}),
            "settings",
        )
        .await
        .unwrap();
        assert!(
            out.contains("[nested blocks could not be loaded: Notion 404"),
            "{out}"
        );
        let f = fake(|_| Err("Notion 404 on GET /pages/x: not shared".into()));
        assert!(run_with(
            &f,
            &json!({"action": "get_page", "page_id": PAGE}),
            "settings"
        )
        .await
        .is_err());
        assert!(run_with(&f, &json!({"action": "get_page"}), "settings")
            .await
            .unwrap_err()
            .contains("page_id"));
        assert_eq!(f.reqs().len(), 1, "a missing id makes no request");
    }

    #[tokio::test]
    async fn create_page_in_database_looks_up_the_title_property_and_chunks() {
        let f = fake(|rq| match (rq.method, rq.path.as_str()) {
            ("GET", p) if p == format!("/databases/{DB}") => Ok(db_fixture()),
            ("POST", "/pages") => Ok(
                json!({"object": "page", "id": PAGE, "url": "https://www.notion.so/new", "properties": {"Task": {"type": "title", "title": rt("Ship")}}}),
            ),
            ("PATCH", p) if p == format!("/blocks/{PAGE}/children") => Ok(list(vec![], None)),
            (m, p) => Err(format!("unexpected {m} {p}")),
        });
        let content: String = (0..150).map(|i| format!("- item {i}\n")).collect();
        let out = run_with(&f, &json!({"action": "create_page", "database_id": DB, "title": "Ship", "properties": {"Status": {"status": {"name": "Done"}}}, "content": content}), "settings")
            .await
            .unwrap();
        assert_eq!(
            out,
            format!("Created page Ship · {PAGE} · https://www.notion.so/new · 150 blocks")
        );
        let reqs = f.reqs();
        assert_eq!(reqs.len(), 3);
        let body = reqs[1].body.as_ref().unwrap();
        assert_eq!(
            body["properties"]["Task"]["title"][0]["text"]["content"],
            "Ship"
        );
        assert_eq!(body["properties"]["Status"]["status"]["name"], "Done");
        assert_eq!(children_count(&reqs[1]), 100);
        assert_eq!(children_count(&reqs[2]), 50);

        // Title already in `properties`: no lookup.
        let f = fake(|rq| match rq.path.as_str() {
            "/pages" => Ok(json!({"id": PAGE, "url": "u", "properties": {}})),
            p => Err(format!("unexpected {p}")),
        });
        run_with(&f, &json!({"action": "create_page", "database_id": DB, "title": "x", "properties": {"Name": {"title": [{"text": {"content": "x"}}]}}}), "settings").await.unwrap();
        assert_eq!(f.reqs().len(), 1);

        // Bad combos make no request at all.
        let f = fake(|_| Err("should not be called".into()));
        for bad in [
            json!({"action": "create_page", "title": "x"}),
            json!({"action": "create_page", "parent_page_id": PAGE, "database_id": DB, "title": "x"}),
            json!({"action": "create_page", "parent_page_id": PAGE, "title": "x", "properties": {"Status": {}}}),
            json!({"action": "create_page", "parent_page_id": "nope", "title": "x"}),
        ] {
            assert!(run_with(&f, &bad, "settings").await.is_err(), "{bad}");
        }
        assert!(f.reqs().is_empty());
    }

    #[tokio::test]
    async fn append_and_update_actions() {
        let f = fake(|rq| match (rq.method, rq.path.as_str()) {
            ("PATCH", p) if p.ends_with("/children") => Ok(list(vec![], None)),
            ("GET", p) if p.starts_with("/pages/") => Ok(page_fixture()),
            ("PATCH", p) if p.starts_with("/pages/") => Ok(page_fixture()),
            (m, p) => Err(format!("unexpected {m} {p}")),
        });
        let out = run_with(
            &f,
            &json!({"action": "append_blocks", "page_id": PAGE, "content": "# Hi\nthere"}),
            "settings",
        )
        .await
        .unwrap();
        assert_eq!(out, format!("Appended 2 blocks to {PAGE} in 1 request."));
        let raw = json!([{"object": "block", "type": "divider", "divider": {}}]);
        let out = run_with(
            &f,
            &json!({"action": "append_blocks", "block_id": PAGE, "children": raw}),
            "settings",
        )
        .await
        .unwrap();
        assert!(out.starts_with("Appended 1 block to"), "{out}");
        assert_eq!(f.reqs()[1].body.as_ref().unwrap()["children"], raw);
        assert!(run_with(
            &f,
            &json!({"action": "append_blocks", "page_id": PAGE}),
            "settings"
        )
        .await
        .is_err());
        assert!(run_with(
            &f,
            &json!({"action": "append_blocks", "page_id": PAGE, "children": {"a": 1}}),
            "settings"
        )
        .await
        .is_err());

        let out = run_with(
            &f,
            &json!({"action": "update_page", "page_id": PAGE, "title": "Renamed"}),
            "settings",
        )
        .await
        .unwrap();
        assert!(out.starts_with("Updated page Roadmap Q3"), "{out}");
        let reqs = f.reqs();
        let last = reqs.last().unwrap();
        assert_eq!(last.method, "PATCH");
        assert_eq!(
            last.body.as_ref().unwrap()["properties"]["Name"]["title"][0]["text"]["content"],
            "Renamed"
        );

        let n = f.reqs().len();
        let e = run_with(
            &f,
            &json!({"action": "update_page", "page_id": PAGE, "archived": true}),
            "settings",
        )
        .await
        .unwrap_err();
        assert!(e.contains("archived"), "{e}");
        let e = run_with(&f, &json!({"action": "update_page", "page_id": PAGE, "in_trash": true, "properties": {"S": {}}}), "settings").await.unwrap_err();
        assert!(e.contains("in_trash"), "{e}");
        assert!(run_with(
            &f,
            &json!({"action": "update_page", "page_id": PAGE}),
            "settings"
        )
        .await
        .is_err());
        assert_eq!(f.reqs().len(), n, "refusals make no request");
    }

    #[tokio::test]
    async fn partial_append_failure_reports_progress() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let f = fake(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(list(vec![], None))
            } else {
                Err("Notion 400 on PATCH: bad".into())
            }
        });
        let content: String = (0..150).map(|i| format!("{i}. x\n")).collect();
        let e = run_with(
            &f,
            &json!({"action": "append_blocks", "page_id": PAGE, "content": content}),
            "settings",
        )
        .await
        .unwrap_err();
        assert!(e.starts_with("Appended 100 of 150 blocks, then:"), "{e}");
    }
}
