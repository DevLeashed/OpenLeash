//! The Jira plugin: one `jira` tool with an `action` enum.
//!
//! Wired into the harness through `plugin_services.rs` (schema, permissions,
//! dispatch, settings). Off by default; reads run freely, writes ask.
//!
//! Two deployments, two auth modes, two REST versions — picked by whether an
//! account email is configured:
//! - **Cloud** (`*.atlassian.net`): basic auth `email:api-token` and REST
//!   **v3**. v3 is where Atlassian ships new endpoints (the enhanced
//!   `search/jql` that replaces the retired `/search`) and it speaks Atlassian
//!   Document Format (ADF) for descriptions and comments.
//! - **Server / Data Center**: a bearer personal access token and REST **v2**
//!   (v3 does not exist there); rich text is a plain wiki-markup string.
//!
//! Choosing by auth mode rather than by sniffing the host keeps proxied or
//! custom-domain Cloud sites working: whoever configured an email is on Cloud.

use super::plugin_services::{self, Identity};
use base64::Engine as _;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JiraCfg {
    /// Off unless the user switches it on (Settings → Connectors).
    pub enabled: bool,
    /// Site URL, e.g. https://acme.atlassian.net or a Server/DC URL. Empty = JIRA_BASE_URL.
    pub base_url: String,
    /// Atlassian account email for Cloud basic auth. Empty = JIRA_EMAIL; empty everywhere = bearer PAT (Server/DC).
    pub email: String,
    /// API token (Cloud) or personal access token (Server/DC). Empty = JIRA_API_TOKEN.
    pub token: String,
}

/// Hand-written so a token never reaches a log line or a test failure message.
impl std::fmt::Debug for JiraCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JiraCfg")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("email", &self.email)
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
    "get_issue",
    "list_transitions",
    "list_projects",
];

/// Actions that change something on Jira (need approval).
/// `api` is classified separately in `is_write`.
pub const WRITES: &[&str] = &["create_issue", "update_issue", "transition", "comment"];

/// The raw passthrough, classified per request in `is_write`.
const RAW: &str = "api";

const UA: &str = "OpenLeash/0.1";
/// Descriptions in `get_issue`.
const BODY_CLIP: usize = 20_000;
/// Each comment in `get_issue`.
const COMMENT_CLIP: usize = 4_000;
/// The newest comments shown by `get_issue`; older ones are counted, not shown.
const MAX_COMMENTS: usize = 50;
/// Raw `api` replies.
const RAW_CLIP: usize = 60_000;
/// Any whole result.
const OUT_CLIP: usize = 80_000;
/// Pages a single `search` follows before handing back a cursor. With
/// `limit` ≤ 100 and Jira's 100-per-page cap this is normally one page; the
/// bound only matters if a server returns short pages.
const MAX_PAGES: usize = 5;

/// What `search` always asks for: exactly what `issue_row` prints.
const ROW_FIELDS: &[&str] = &["summary", "status", "issuetype", "priority", "assignee"];

/// What `get_issue` asks for (rather than every field, which on a big site is
/// hundreds of custom fields the model can't use).
const ISSUE_FIELDS: &str = "summary,status,issuetype,priority,assignee,reporter,labels,components,fixVersions,parent,description,comment,created,updated,resolution,project,subtasks,issuelinks,duedate";

pub fn schema() -> Value {
    let mut actions: Vec<&str> = READS.iter().chain(WRITES.iter()).copied().collect();
    actions.push(RAW);
    json!({
        "name": "jira",
        "description": "Work with Jira (Cloud or Server/Data Center) as the configured account. Issues are named by key, e.g. `PROJ-123`. Actions:\n\
    - whoami: the account and site the credentials reach.\n\
    - search (jql, fields, limit, cursor): issues matching a JQL query, e.g. `project = PROJ AND statusCategory != Done ORDER BY updated DESC` or `assignee = currentUser() AND resolution = Unresolved`. `fields` adds extra field ids/names to each row (e.g. [\"duedate\",\"customfield_10016\"]).\n\
    - get_issue (key): summary, status, type, priority, assignee, reporter, labels, components, fix versions, parent/epic, subtasks, links, description and comments.\n\
    - list_transitions (key): the workflow moves available from the issue's current status.\n\
    - list_projects (query, limit, cursor).\n\
    - create_issue (project, issue_type default \"Task\", summary, description, labels, priority, assignee, parent, fields).\n\
    - update_issue (key + any of summary, description, labels, add_labels, remove_labels, priority, assignee, parent, fields).\n\
    - transition (key, transition = name or id from list_transitions or the target status name, optional comment, resolution).\n\
    - comment (key, body).\n\
    - api (method, path, body): any other REST endpoint on the configured site, e.g. `GET /rest/api/3/issue/PROJ-1/changelog` (Cloud) or `/rest/api/2/...` (Server/DC). The path is always joined onto the configured site; other hosts are refused.\n\
    Output: one line per issue `KEY Summary [Status] · Type · Priority · @assignee · url`; long text is clipped. Lists that stop early say so and give a `cursor` for the next page. Descriptions and comments are plain text (Markdown-ish: blank lines between paragraphs, ``` code fences); they are converted to Jira's rich-text format for you. `assignee` takes an accountId (Cloud) or username (Server/DC), \"me\" for the configured account, or \"none\" to unassign. Changes (create_issue, update_issue, transition, comment, and any non-GET api call) ask the user first unless allowed. Results only include what this account may see.",
        "input_schema": {"type":"object","properties":{
            "action":{"type":"string","enum": actions},
            "jql":{"type":"string","description":"search: the JQL query"},
            "fields":{"description":"search: array of extra field ids/names to show per row. create_issue/update_issue: an object of raw Jira fields merged into the request (e.g. {\"customfield_10016\": 3, \"duedate\": \"2026-01-31\"}); named params win on conflict"},
            "limit":{"type":"integer","description":"Max results (default 20, max 100)"},
            "cursor":{"type":"string","description":"Continue a search or list_projects from where the previous call stopped (the value it printed)"},
            "key":{"type":"string","description":"Issue key like PROJ-123 (or numeric issue id)"},
            "project":{"type":"string","description":"create_issue: project key, e.g. PROJ"},
            "issue_type":{"type":"string","description":"create_issue: issue type name, e.g. Task, Bug, Story, Epic, Sub-task (default Task)"},
            "summary":{"type":"string"},
            "description":{"type":"string","description":"Plain text; blank lines separate paragraphs, ``` fences make code blocks"},
            "labels":{"type":"array","items":{"type":"string"},"description":"create/update: the full label set (no spaces in a label)"},
            "add_labels":{"type":"array","items":{"type":"string"},"description":"update_issue: labels to add"},
            "remove_labels":{"type":"array","items":{"type":"string"},"description":"update_issue: labels to remove"},
            "priority":{"type":"string","description":"Priority name, e.g. High"},
            "assignee":{"type":"string","description":"accountId (Cloud) or username (Server/DC); \"me\"; \"none\" to unassign"},
            "parent":{"type":"string","description":"Parent issue key (epic, or the parent of a sub-task)"},
            "transition":{"type":"string","description":"transition: transition name or id, or the target status name"},
            "comment":{"type":"string","description":"transition: optional comment added with the move"},
            "resolution":{"type":"string","description":"transition: optional resolution name, e.g. Done, Won't Do (only if the transition screen has it)"},
            "body":{"description":"comment: the comment text. api: the JSON request body (object or JSON string)"},
            "query":{"type":"string","description":"list_projects: filter by name or key"},
            "method":{"type":"string","description":"api: GET (default) | POST | PUT | PATCH | DELETE"},
            "path":{"type":"string","description":"api: endpoint path on the site, e.g. /rest/api/3/issue/PROJ-1/changelog"}},
            "required":["action"]}
    })
}

/// Whether this call changes something on Jira. Fails closed.
pub fn is_write(input: &Value) -> bool {
    let a = input["action"].as_str().unwrap_or("");
    if a == RAW {
        return !(plugin_services::method_is_read(&input["method"])
            && plugin_services::raw_path_is_relative(input["path"].as_str().unwrap_or("")));
    }
    !READS.contains(&a)
}

pub async fn run(http: &reqwest::Client, cfg: &JiraCfg, input: &Value) -> Result<String, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let secrets = c.secrets();
    let out = Jira { http, c: &c }.run(input).await?;
    // Redacting the output as well costs nothing and covers a server that
    // echoes a request header back into a field.
    Ok(plugin_services::clip(
        &plugin_services::redact(&out, &secrets),
        OUT_CLIP,
    ))
}

pub async fn whoami(http: &reqwest::Client, cfg: &JiraCfg) -> Result<Identity, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let v = Jira { http, c: &c }.call(&myself_req(c.cloud())).await?;
    Ok(Identity {
        identity: identity_line(&v, &c),
        source: c.source.clone(),
    })
}

// ───────────────────────────── credentials ─────────────────────────────

/// Resolved connection details. Not `Debug`-derived: the token and the basic
/// credential must never reach a log line or a test failure message.
pub(crate) struct Creds {
    /// Site URL without a trailing slash, e.g. `https://acme.atlassian.net`.
    base: String,
    /// `Some` = Cloud (basic auth, REST v3); `None` = Server/DC (bearer, REST v2).
    email: Option<String>,
    token: String,
    /// base64(`email:token`). The email is not secret but this is: it decodes
    /// straight back to the token, so it is redacted like the token itself.
    basic: Option<String>,
    /// Where the credentials came from: `settings`, an env var name, or both.
    source: String,
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("base", &self.base)
            .field("email", &self.email)
            .field("token", &"[redacted]")
            .field("source", &self.source)
            .finish()
    }
}

impl Creds {
    fn cloud(&self) -> bool {
        self.email.is_some()
    }

    fn secrets(&self) -> Vec<&str> {
        let mut v = vec![self.token.as_str()];
        if let Some(b) = &self.basic {
            v.push(b.as_str());
        }
        v
    }

    fn auth_header(&self) -> Result<reqwest::header::HeaderValue, String> {
        let raw = match &self.basic {
            Some(b) => format!("Basic {b}"),
            None => format!("Bearer {}", self.token),
        };
        let mut h = reqwest::header::HeaderValue::from_str(&raw).map_err(|_| {
            "The Jira token contains characters that can't go in an HTTP header — re-paste it in Settings → Connectors → Jira.".to_string()
        })?;
        h.set_sensitive(true);
        Ok(h)
    }

    fn mode(&self) -> &'static str {
        if self.cloud() {
            "Jira Cloud (REST v3, email + API token)"
        } else {
            "Jira Server/Data Center (REST v2, personal access token)"
        }
    }
}

/// The REST prefix for the auth mode (see the module comment for why).
fn api(cloud: bool) -> &'static str {
    if cloud {
        "/rest/api/3"
    } else {
        "/rest/api/2"
    }
}

fn is_cloud_host(u: &reqwest::Url) -> bool {
    u.host_str()
        .map(str::to_ascii_lowercase)
        .is_some_and(|h| h.ends_with(".atlassian.net"))
}

/// Validate and normalise the site URL: http(s), a host, no credentials, no
/// query or fragment, no trailing slash. A Cloud site has no context path, so
/// a pasted board/issue URL is cut back to the site; a Server/DC URL keeps its
/// path (`https://host/jira`).
fn normalize_base(raw: &str) -> Result<String, String> {
    let mut u = reqwest::Url::parse(raw.trim()).map_err(|_| {
        format!("The Jira base URL `{}` is not a valid URL — use the site address, e.g. https://acme.atlassian.net.", raw.trim())
    })?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(format!(
            "The Jira base URL `{}` must be an http(s) URL with a host, e.g. https://acme.atlassian.net.",
            raw.trim()
        ));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("The Jira base URL must not contain a username or password — put the email and token in Settings → Connectors → Jira.".into());
    }
    u.set_query(None);
    u.set_fragment(None);
    if is_cloud_host(&u) {
        u.set_path("");
    }
    Ok(u.as_str().trim_end_matches('/').to_string())
}

/// Credentials from settings, then the environment (`env` is injected so tests
/// never touch the process environment).
fn creds(cfg: &JiraCfg, env: &dyn Fn(&str) -> Option<String>) -> Result<Creds, String> {
    let (raw_base, base_src) = plugin_services::setting_or_env(&cfg.base_url, &["JIRA_BASE_URL"], env)
        .ok_or("Jira isn't set up: no site URL. Set it in Settings → Connectors → Jira (Base URL, e.g. https://acme.atlassian.net) or the JIRA_BASE_URL environment variable.")?;
    let base = normalize_base(&raw_base)?;
    let email = plugin_services::setting_or_env(&cfg.email, &["JIRA_EMAIL"], env);
    let (token, tok_src) = plugin_services::setting_or_env(&cfg.token, &["JIRA_API_TOKEN"], env)
        .ok_or("Jira isn't set up: no API token. Paste one in Settings → Connectors → Jira, or set JIRA_API_TOKEN (Cloud: an Atlassian API token plus your account email in JIRA_EMAIL; Server/Data Center: a personal access token, no email).")?;
    if email.is_none() {
        if let Ok(u) = reqwest::Url::parse(&base) {
            if is_cloud_host(&u) {
                // A Cloud API token is not a bearer token: without the email
                // every call would fail with an opaque 401.
                return Err(format!(
                    "`{}` is Jira Cloud, which needs your Atlassian account email alongside the API token. Add it in Settings → Connectors → Jira (Email) or set JIRA_EMAIL.",
                    u.host_str().unwrap_or_default()
                ));
            }
        }
    }
    let basic = email
        .as_ref()
        .map(|(e, _)| base64::engine::general_purpose::STANDARD.encode(format!("{e}:{token}")));
    let mut sources: Vec<&str> = vec![tok_src];
    for s in [email.as_ref().map(|(_, s)| *s), Some(base_src)]
        .into_iter()
        .flatten()
    {
        if !sources.contains(&s) {
            sources.push(s);
        }
    }
    Ok(Creds {
        base,
        email: email.map(|(e, _)| e),
        token,
        basic,
        source: sources.join(" + "),
    })
}

// ───────────────────────────── requests (pure) ─────────────────────────────

/// One request, built without touching the network so tests can inspect it.
/// `path` is relative to the site and always goes through `pin_to_base`.
#[derive(Debug, Clone, PartialEq)]
struct Req {
    method: Method,
    path: String,
    query: Vec<(String, String)>,
    body: Option<Value>,
}

impl Req {
    fn new(method: Method, path: String) -> Self {
        Req {
            method,
            path,
            query: vec![],
            body: None,
        }
    }

    fn q(mut self, k: &str, v: impl Into<String>) -> Self {
        self.query.push((k.to_string(), v.into()));
        self
    }

    fn body(mut self, b: Value) -> Self {
        self.body = Some(b);
        self
    }

    fn what(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

/// The full URL: pinned to the site (so nothing — not even a raw `api` path —
/// can carry the credentials to another host), with the query encoded by the
/// URL library rather than by hand.
fn url_for(base: &str, req: &Req) -> Result<reqwest::Url, String> {
    let mut u = plugin_services::pin_to_base(base, &req.path)?;
    if !req.query.is_empty() {
        u.query_pairs_mut().extend_pairs(req.query.iter());
    }
    Ok(u)
}

fn s(input: &Value, k: &str) -> Option<String> {
    input[k]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn limit(input: &Value) -> usize {
    input["limit"]
        .as_u64()
        .or_else(|| input["limit"].as_str().and_then(|v| v.trim().parse().ok()))
        .unwrap_or(20)
        .clamp(1, 100) as usize
}

/// A list of strings from an array or a comma-separated string.
fn str_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        Value::String(t) => t
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => vec![],
    }
}

/// `PROJ-123` or a numeric id. Checked rather than merely encoded: the key is a
/// path segment, and `..` would silently re-target a write at another endpoint.
fn valid_key(k: &str) -> bool {
    if !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let Some((p, n)) = k.rsplit_once('-') else {
        return false;
    };
    p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
}

fn issue_key(input: &Value) -> Result<String, String> {
    let k = s(input, "key").ok_or("`key` is required: an issue key like PROJ-123.")?;
    if !valid_key(&k) {
        return Err(format!(
            "`{}` is not an issue key — use the form PROJ-123.",
            plugin_services::clip(&k, 80)
        ));
    }
    Ok(k.to_ascii_uppercase())
}

/// Rich text in the shape the API version wants: ADF on Cloud v3, a plain
/// (wiki markup) string on Server v2.
fn rich(cloud: bool, text: &str) -> Value {
    if cloud {
        text_to_adf(text)
    } else {
        json!(text)
    }
}

fn myself_req(cloud: bool) -> Req {
    Req::new(Method::GET, format!("{}/myself", api(cloud)))
}

fn search_fields(extra: &[String]) -> Vec<String> {
    let mut f: Vec<String> = ROW_FIELDS.iter().map(|x| x.to_string()).collect();
    for e in extra {
        if !f.contains(e) {
            f.push(e.clone());
        }
    }
    f
}

/// JQL travels as a JSON string in a POST body, never spliced into a URL.
/// Cloud uses the enhanced `search/jql` endpoint (token paging); the old
/// `/rest/api/3/search` is being removed. Server/DC v2 pages by `startAt`.
fn search_req(
    cloud: bool,
    jql: &str,
    extra: &[String],
    max: usize,
    cursor: Option<&str>,
) -> Result<Req, String> {
    let mut body = json!({"jql": jql, "maxResults": max, "fields": search_fields(extra)});
    if cloud {
        if let Some(c) = cursor {
            body["nextPageToken"] = json!(c);
        }
        Ok(Req::new(Method::POST, "/rest/api/3/search/jql".into()).body(body))
    } else {
        let start = match cursor {
            Some(c) => c.trim().parse::<u64>().map_err(|_| {
                format!("`cursor` must be the number printed by the previous search, not `{c}`.")
            })?,
            None => 0,
        };
        body["startAt"] = json!(start);
        Ok(Req::new(Method::POST, "/rest/api/2/search".into()).body(body))
    }
}

/// The cursor for the next page, if there is one.
fn next_cursor(cloud: bool, page: &Value, got: usize) -> Option<String> {
    if cloud {
        if page["isLast"].as_bool() == Some(true) {
            return None;
        }
        page["nextPageToken"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(String::from)
    } else {
        let start = page["startAt"].as_u64().unwrap_or(0);
        let total = page["total"].as_u64()?;
        let next = start + got as u64;
        (got > 0 && next < total).then(|| next.to_string())
    }
}

fn get_issue_req(cloud: bool, key: &str) -> Req {
    Req::new(
        Method::GET,
        format!("{}/issue/{}", api(cloud), urlencoding::encode(key)),
    )
    .q("fields", ISSUE_FIELDS)
}

fn transitions_req(cloud: bool, key: &str) -> Req {
    Req::new(
        Method::GET,
        format!(
            "{}/issue/{}/transitions",
            api(cloud),
            urlencoding::encode(key)
        ),
    )
}

/// Cloud pages projects server-side; Server/DC returns them all in one array
/// (filtered and paged here, see `run`).
fn projects_req(cloud: bool, max: usize, start: usize, query: Option<&str>) -> Req {
    if cloud {
        let mut r = Req::new(Method::GET, "/rest/api/3/project/search".into())
            .q("maxResults", max.to_string())
            .q("startAt", start.to_string())
            .q("orderBy", "key")
            .q("expand", "lead");
        if let Some(q) = query {
            r = r.q("query", q);
        }
        r
    } else {
        Req::new(Method::GET, "/rest/api/2/project".into()).q("expand", "lead")
    }
}

fn parse_start(cursor: Option<&str>) -> Result<usize, String> {
    match cursor {
        Some(c) => c.trim().parse::<usize>().map_err(|_| {
            format!("`cursor` must be the number printed by the previous list_projects, not `{c}`.")
        }),
        None => Ok(0),
    }
}

fn assignee_value(cloud: bool, who: &str) -> Value {
    if matches!(
        who.to_ascii_lowercase().as_str(),
        "none" | "unassigned" | "-" | "null"
    ) {
        Value::Null
    } else if cloud {
        json!({"accountId": who})
    } else {
        json!({"name": who})
    }
}

/// The raw `fields` passthrough, with a string description on Cloud turned
/// into ADF (v3 rejects a bare string there).
fn passthrough(cloud: bool, input: &Value) -> Result<Map<String, Value>, String> {
    match &input["fields"] {
        Value::Null => Ok(Map::new()),
        Value::Object(m) => {
            let mut m = m.clone();
            if cloud {
                if let Some(Value::String(d)) = m.get("description").cloned() {
                    m.insert("description".into(), text_to_adf(&d));
                }
            }
            Ok(m)
        }
        _ => Err("`fields` must be an object of Jira field ids to values, e.g. {\"duedate\": \"2026-01-31\"}.".into()),
    }
}

/// The named params shared by create and update, applied over the passthrough
/// (named params win). Returns which fields were set, for the reply.
fn named_fields(cloud: bool, input: &Value, f: &mut Map<String, Value>) -> Vec<String> {
    let mut set = vec![];
    if let Some(v) = s(input, "summary") {
        f.insert("summary".into(), json!(v));
        set.push("summary".into());
    }
    if let Some(v) = input["description"].as_str() {
        f.insert("description".into(), rich(cloud, v));
        set.push("description".into());
    }
    if !input["labels"].is_null() {
        f.insert("labels".into(), json!(str_list(&input["labels"])));
        set.push("labels".into());
    }
    if let Some(v) = s(input, "priority") {
        f.insert("priority".into(), json!({"name": v}));
        set.push("priority".into());
    }
    if let Some(v) = s(input, "assignee") {
        f.insert("assignee".into(), assignee_value(cloud, &v));
        set.push("assignee".into());
    }
    if let Some(v) = s(input, "parent") {
        f.insert("parent".into(), json!({"key": v.to_ascii_uppercase()}));
        set.push("parent".into());
    }
    set
}

fn create_req(cloud: bool, input: &Value) -> Result<Req, String> {
    let project = s(input, "project")
        .ok_or("create_issue needs `project` (a project key like PROJ — see list_projects).")?;
    let summary = s(input, "summary").ok_or("create_issue needs `summary`.")?;
    let issue_type = s(input, "issue_type").unwrap_or_else(|| "Task".into());
    let mut f = passthrough(cloud, input)?;
    named_fields(cloud, input, &mut f);
    f.insert(
        "project".into(),
        json!({"key": project.to_ascii_uppercase()}),
    );
    f.insert("issuetype".into(), json!({"name": issue_type}));
    f.insert("summary".into(), json!(summary));
    Ok(Req::new(Method::POST, format!("{}/issue", api(cloud))).body(json!({"fields": f})))
}

fn update_req(cloud: bool, key: &str, input: &Value) -> Result<(Req, Vec<String>), String> {
    let mut f = passthrough(cloud, input)?;
    let mut changed: Vec<String> = f.keys().cloned().collect();
    for n in named_fields(cloud, input, &mut f) {
        if !changed.contains(&n) {
            changed.push(n);
        }
    }
    let mut body = json!({});
    let ops: Vec<Value> = str_list(&input["add_labels"])
        .into_iter()
        .map(|l| json!({"add": l}))
        .chain(
            str_list(&input["remove_labels"])
                .into_iter()
                .map(|l| json!({"remove": l})),
        )
        .collect();
    if !ops.is_empty() {
        body["update"] = json!({"labels": ops});
        changed.push("labels (add/remove)".into());
    }
    if changed.is_empty() {
        return Err("update_issue needs something to change: summary, description, labels, add_labels, remove_labels, priority, assignee, parent or fields.".into());
    }
    if !f.is_empty() {
        body["fields"] = Value::Object(f);
    }
    Ok((
        Req::new(
            Method::PUT,
            format!("{}/issue/{}", api(cloud), urlencoding::encode(key)),
        )
        .body(body),
        changed,
    ))
}

fn transition_req(
    cloud: bool,
    key: &str,
    id: &str,
    comment: Option<&str>,
    resolution: Option<&str>,
) -> Req {
    let mut body = json!({"transition": {"id": id}});
    if let Some(r) = resolution {
        body["fields"] = json!({"resolution": {"name": r}});
    }
    if let Some(c) = comment {
        body["update"] = json!({"comment": [{"add": {"body": rich(cloud, c)}}]});
    }
    Req::new(
        Method::POST,
        format!(
            "{}/issue/{}/transitions",
            api(cloud),
            urlencoding::encode(key)
        ),
    )
    .body(body)
}

fn comment_req(cloud: bool, key: &str, body: &str) -> Req {
    Req::new(
        Method::POST,
        format!("{}/issue/{}/comment", api(cloud), urlencoding::encode(key)),
    )
    .body(json!({"body": rich(cloud, body)}))
}

/// The raw passthrough. The path is kept as given and joined by `pin_to_base`
/// in `url_for` — never concatenated here — so it cannot leave the site.
fn raw_req(input: &Value) -> Result<Req, String> {
    let path =
        s(input, "path").ok_or("api needs `path`, e.g. /rest/api/3/issue/PROJ-1/changelog.")?;
    let m = match &input["method"] {
        Value::Null => "GET".to_string(),
        Value::String(m) => m.trim().to_ascii_uppercase(),
        _ => return Err("`method` must be a string: GET, POST, PUT, PATCH or DELETE.".into()),
    };
    let method = match m.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        other => {
            return Err(format!(
                "Unsupported method `{}` — use GET, POST, PUT, PATCH or DELETE.",
                plugin_services::clip(other, 20)
            ))
        }
    };
    let body = match &input["body"] {
        Value::Null => None,
        Value::String(t) if t.trim().is_empty() => None,
        Value::String(t) => {
            Some(serde_json::from_str(t).map_err(|e| format!("`body` isn't valid JSON: {e}"))?)
        }
        v => Some(v.clone()),
    };
    Ok(Req {
        method,
        path,
        query: vec![],
        body,
    })
}

// ───────────────────────────── replies (pure) ─────────────────────────────

/// Jira's error shape: `{"errorMessages": [...], "errors": {"field": "msg"}}`,
/// sometimes `{"message": ...}`; an HTML page from a proxy or wrong URL falls
/// back to its (clipped) text.
fn server_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let mut parts: Vec<String> = v["errorMessages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m.as_str())
            .map(String::from)
            .collect();
        if let Some(m) = v["errors"].as_object() {
            parts.extend(m.iter().map(|(k, e)| {
                format!(
                    "{k}: {}",
                    e.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| e.to_string())
                )
            }));
        }
        if let Some(m) = v["message"].as_str() {
            parts.push(m.to_string());
        }
        if !parts.is_empty() {
            return parts.join("; ");
        }
    }
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(400).collect()
}

fn hint(status: u16) -> &'static str {
    match status {
        401 => "Jira rejected the credentials — check them in Settings → Connectors → Jira (or JIRA_EMAIL / JIRA_API_TOKEN): Jira Cloud needs the account email plus an Atlassian API token; Server/Data Center needs a personal access token with the email left empty.",
        403 => "(403: this account lacks permission for that project, issue or action — or the base URL points at the wrong site.)",
        404 => "(404: the issue/project doesn't exist, this account can't see it, or the base URL points at the wrong Jira site.)",
        400 => "(Field names and values must match the project's create/edit screens; list_transitions shows valid moves.)",
        _ => "",
    }
}

/// A reply read to the end → JSON, or a clear, token-free error.
fn interpret(status: u16, body: &str, what: &str, secrets: &[&str]) -> Result<Value, String> {
    if !(200..300).contains(&status) {
        return Err(plugin_services::http_error(
            "Jira",
            status,
            what,
            &server_message(body),
            hint(status),
            secrets,
        ));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(body).map_err(|_| {
        plugin_services::redact(
            &format!(
                "Jira answered {what} with something that isn't JSON — check that the base URL is the Jira site itself (got: {}).",
                plugin_services::clip(&body.split_whitespace().collect::<Vec<_>>().join(" "), 200)
            ),
            secrets,
        )
    })
}

fn browse_url(base: &str, key: &str) -> String {
    format!("{base}/browse/{key}")
}

/// A person, status, priority, component… whatever Jira calls its name.
fn name_of(v: &Value) -> Option<String> {
    ["displayName", "name", "value", "key"]
        .iter()
        .find_map(|k| v[*k].as_str())
        .map(String::from)
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(String::from).or_else(|| name_of(x)))
        .collect()
}

/// `2024-05-02T10:31:00.000+0000` → `2024-05-02 10:31`.
fn when(v: &Value) -> String {
    v.as_str()
        .map(|t| t.chars().take(16).collect::<String>().replace('T', " "))
        .unwrap_or_default()
}

/// Description/comment body: ADF on Cloud, a string on Server.
fn body_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(t) => t.trim().to_string(),
        other => adf_to_text(other),
    }
}

/// One line per issue: `KEY-1 Summary [Status] · Type · Priority · @assignee · url`.
fn issue_row(base: &str, i: &Value) -> String {
    let f = &i["fields"];
    let key = i["key"].as_str().unwrap_or("?");
    let mut parts = vec![format!(
        "{key} {} [{}]",
        f["summary"].as_str().unwrap_or("(no summary)"),
        f["status"]["name"].as_str().unwrap_or("?")
    )];
    if let Some(t) = f["issuetype"]["name"].as_str() {
        parts.push(t.to_string());
    }
    if let Some(p) = f["priority"]["name"].as_str() {
        parts.push(p.to_string());
    }
    parts.push(match name_of(&f["assignee"]) {
        Some(a) => format!("@{a}"),
        None => "unassigned".into(),
    });
    parts.push(browse_url(base, key));
    parts.join(" · ")
}

/// A compact rendering of an arbitrary field value, for extra search fields.
fn compact(v: &Value) -> Option<String> {
    let out = match v {
        Value::Null => return None,
        Value::String(t) => t.clone(),
        Value::Number(_) | Value::Bool(_) => v.to_string(),
        Value::Array(a) if a.is_empty() => return None,
        Value::Array(a) => a.iter().filter_map(compact).collect::<Vec<_>>().join(", "),
        Value::Object(_) if v["type"] == "doc" => adf_to_text(v).replace('\n', " "),
        Value::Object(_) => name_of(v).unwrap_or_else(|| v.to_string()),
    };
    Some(plugin_services::clip(&out, 300))
}

fn format_search(
    base: &str,
    jql: &str,
    issues: &[Value],
    extra: &[String],
    more: Option<&str>,
) -> String {
    if issues.is_empty() {
        return format!("No issues match `{jql}` on {base}.");
    }
    let mut out = vec![format!("{} issue(s) for `{jql}` on {base}:", issues.len())];
    for i in issues {
        out.push(issue_row(base, i));
        for e in extra {
            if ROW_FIELDS.contains(&e.as_str()) {
                continue;
            }
            if let Some(v) = compact(&i["fields"][e.as_str()]) {
                out.push(format!("    {e}: {v}"));
            }
        }
    }
    if let Some(c) = more {
        out.push(format!("More available — call again with cursor: \"{c}\"."));
    }
    out.join("\n")
}

fn linked_row(base: &str, i: &Value) -> String {
    let key = i["key"].as_str().unwrap_or("?");
    format!(
        "{key} {} [{}] · {}",
        i["fields"]["summary"].as_str().unwrap_or(""),
        i["fields"]["status"]["name"].as_str().unwrap_or("?"),
        browse_url(base, key)
    )
}

fn format_issue(base: &str, i: &Value) -> String {
    let f = &i["fields"];
    let mut out = vec![issue_row(base, i)];
    let mut meta = vec![];
    if let Some(p) = f["project"]["key"].as_str() {
        meta.push(format!(
            "Project: {} ({p})",
            f["project"]["name"].as_str().unwrap_or(p)
        ));
    }
    if let Some(r) = name_of(&f["reporter"]) {
        meta.push(format!("Reporter: @{r}"));
    }
    for (label, k) in [
        ("Created", "created"),
        ("Updated", "updated"),
        ("Due", "duedate"),
    ] {
        let t = when(&f[k]);
        if !t.is_empty() {
            meta.push(format!("{label}: {t}"));
        }
    }
    if let Some(r) = name_of(&f["resolution"]) {
        meta.push(format!("Resolution: {r}"));
    }
    if !meta.is_empty() {
        out.push(meta.join(" · "));
    }
    let mut tags = vec![];
    for (label, k) in [
        ("Labels", "labels"),
        ("Components", "components"),
        ("Fix versions", "fixVersions"),
    ] {
        let n = names(&f[k]);
        if !n.is_empty() {
            tags.push(format!("{label}: {}", n.join(", ")));
        }
    }
    if !tags.is_empty() {
        out.push(tags.join(" · "));
    }
    if f["parent"]["key"].is_string() {
        let kind = f["parent"]["fields"]["issuetype"]["name"]
            .as_str()
            .unwrap_or("Parent");
        out.push(format!(
            "Parent ({kind}): {}",
            linked_row(base, &f["parent"])
        ));
    }
    let subs: Vec<String> = f["subtasks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| format!("  {}", linked_row(base, s)))
        .collect();
    if !subs.is_empty() {
        out.push("Subtasks:".into());
        out.extend(subs);
    }
    let links: Vec<String> = f["issuelinks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let (rel, other) = if l["outwardIssue"].is_object() {
                (l["type"]["outward"].as_str(), &l["outwardIssue"])
            } else if l["inwardIssue"].is_object() {
                (l["type"]["inward"].as_str(), &l["inwardIssue"])
            } else {
                return None;
            };
            Some(format!(
                "  {} {}",
                rel.unwrap_or("relates to"),
                linked_row(base, other)
            ))
        })
        .collect();
    if !links.is_empty() {
        out.push("Links:".into());
        out.extend(links);
    }
    let desc = body_text(&f["description"]);
    out.push(String::new());
    out.push("Description:".into());
    out.push(if desc.is_empty() {
        "(none)".into()
    } else {
        plugin_services::clip(&desc, BODY_CLIP)
    });
    let comments = f["comment"]["comments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total = f["comment"]["total"]
        .as_u64()
        .map(|t| t as usize)
        .unwrap_or(comments.len())
        .max(comments.len());
    out.push(String::new());
    if comments.is_empty() {
        out.push(if total > 0 {
            format!("Comments: {total} (not included in this reply)")
        } else {
            "Comments: none".into()
        });
    } else {
        let shown = &comments[comments.len().saturating_sub(MAX_COMMENTS)..];
        out.push(if shown.len() < total {
            format!("Comments (the last {} of {total}):", shown.len())
        } else {
            format!("Comments ({total}):")
        });
        for c in shown {
            out.push(format!(
                "--- @{} · {}",
                name_of(&c["author"]).unwrap_or_else(|| "?".into()),
                when(&c["created"])
            ));
            out.push(plugin_services::clip(&body_text(&c["body"]), COMMENT_CLIP));
        }
    }
    out.join("\n")
}

fn format_transitions(key: &str, v: &Value) -> String {
    let rows: Vec<String> = v["transitions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let mut r = format!(
                "{} · {} → {}",
                t["id"].as_str().unwrap_or("?"),
                t["name"].as_str().unwrap_or("?"),
                t["to"]["name"].as_str().unwrap_or("?")
            );
            if t["hasScreen"].as_bool() == Some(true) {
                r.push_str(" (has a screen: may need fields)");
            }
            r
        })
        .collect();
    if rows.is_empty() {
        return format!("No transitions are available for {key} to this account.");
    }
    format!(
        "Transitions for {key} (id · name → target status; pass the name or id to `transition`):\n{}",
        rows.join("\n")
    )
}

/// The transition to run: by id, by name, or by target status name (if that
/// is unambiguous). Returns `(id, name, target status)`.
fn resolve_transition(
    key: &str,
    v: &Value,
    want: &str,
) -> Result<(String, String, String), String> {
    let all: Vec<&Value> = v["transitions"].as_array().into_iter().flatten().collect();
    let pick = |t: &Value| {
        (
            t["id"].as_str().unwrap_or("").to_string(),
            t["name"].as_str().unwrap_or("").to_string(),
            t["to"]["name"].as_str().unwrap_or("").to_string(),
        )
    };
    let w = want.trim();
    if let Some(t) = all.iter().find(|t| t["id"].as_str() == Some(w)) {
        return Ok(pick(t));
    }
    if let Some(t) = all.iter().find(|t| {
        t["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(w))
    }) {
        return Ok(pick(t));
    }
    let by_target: Vec<&&Value> = all
        .iter()
        .filter(|t| {
            t["to"]["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(w))
        })
        .collect();
    if by_target.len() == 1 {
        return Ok(pick(by_target[0]));
    }
    let avail: Vec<String> = all
        .iter()
        .map(|t| {
            let (id, name, to) = pick(t);
            format!("{name} (id {id} → {to})")
        })
        .collect();
    Err(if avail.is_empty() {
        format!("{key} has no transitions available to this account.")
    } else {
        format!(
            "No single transition `{w}` for {key}. Available: {}.",
            avail.join(", ")
        )
    })
}

fn project_row(base: &str, p: &Value) -> String {
    let key = p["key"].as_str().unwrap_or("?");
    let mut parts = vec![format!("{key} {}", p["name"].as_str().unwrap_or(""))];
    if let Some(t) = p["projectTypeKey"].as_str() {
        parts.push(t.to_string());
    }
    if let Some(l) = name_of(&p["lead"]) {
        parts.push(format!("lead @{l}"));
    }
    parts.push(browse_url(base, key));
    parts.join(" · ")
}

fn format_projects(base: &str, list: &[Value], more: Option<&str>) -> String {
    if list.is_empty() {
        return format!("No projects visible to this account on {base}.");
    }
    let mut out = vec![format!("{} project(s) on {base}:", list.len())];
    out.extend(list.iter().map(|p| project_row(base, p)));
    if let Some(c) = more {
        out.push(format!("More available — call again with cursor: \"{c}\"."));
    }
    out.join("\n")
}

/// Server/DC returns every project at once: filter by `query`, then page.
fn page_projects(
    all: &[Value],
    query: Option<&str>,
    start: usize,
    max: usize,
) -> (Vec<Value>, Option<String>) {
    let q = query.map(str::to_lowercase);
    let hits: Vec<&Value> = all
        .iter()
        .filter(|p| match &q {
            None => true,
            Some(q) => ["key", "name"]
                .iter()
                .any(|k| p[*k].as_str().is_some_and(|v| v.to_lowercase().contains(q))),
        })
        .collect();
    let page: Vec<Value> = hits
        .iter()
        .skip(start)
        .take(max)
        .map(|p| (*p).clone())
        .collect();
    let next = start + page.len();
    let more = (next < hits.len()).then(|| next.to_string());
    (page, more)
}

fn host_of(base: &str) -> String {
    reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_else(|| base.to_string())
}

/// `Ada Lovelace <ada@example.com> · acme.atlassian.net`.
fn identity_line(v: &Value, c: &Creds) -> String {
    let name = v["displayName"]
        .as_str()
        .or(v["name"].as_str())
        .unwrap_or("(unknown user)");
    let email = v["emailAddress"].as_str().filter(|e| !e.is_empty());
    format!(
        "{name}{} · {}",
        email.map(|e| format!(" <{e}>")).unwrap_or_default(),
        host_of(&c.base)
    )
}

fn format_myself(v: &Value, c: &Creds) -> String {
    let id = if c.cloud() {
        v["accountId"].as_str().map(|a| format!("accountId {a}"))
    } else {
        v["name"].as_str().map(|n| format!("username {n}"))
    };
    let mut out = identity_line(v, c);
    if let Some(id) = id {
        out.push_str(&format!(" · {id}"));
    }
    out.push_str(&format!("\nSite: {} — {}", c.base, c.mode()));
    out
}

// ───────────────────────────── ADF ─────────────────────────────

fn kids(n: &Value) -> &[Value] {
    n["content"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn is_inline(t: &str) -> bool {
    matches!(
        t,
        "text"
            | "hardBreak"
            | "mention"
            | "emoji"
            | "inlineCard"
            | "status"
            | "date"
            | "placeholder"
            | "inlineExtension"
            | "mediaInline"
    )
}

/// Children as blocks, separated by a blank line.
fn blocks(nodes: &[Value]) -> String {
    nodes
        .iter()
        .map(node_text)
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn inlines(nodes: &[Value]) -> String {
    nodes.iter().map(node_text).collect()
}

fn prefix_lines(text: &str, first: &str, rest: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| {
            let p = if i == 0 { first } else { rest };
            if l.is_empty() {
                p.trim_end().to_string()
            } else {
                format!("{p}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn list_text(items: &[Value], ordered: Option<u64>, tasks: bool) -> String {
    let mut n = ordered.unwrap_or(1);
    items
        .iter()
        .map(|item| {
            let marker = if tasks {
                if item["attrs"]["state"] == "DONE" {
                    "- [x] ".to_string()
                } else {
                    "- [ ] ".to_string()
                }
            } else if ordered.is_some() {
                let m = format!("{n}. ");
                n += 1;
                m
            } else {
                "- ".to_string()
            };
            // Tight lists: an item's paragraph and its nested list share lines.
            let c = kids(item);
            let body = if c
                .iter()
                .all(|k| is_inline(k["type"].as_str().unwrap_or("")))
            {
                inlines(c)
            } else {
                c.iter()
                    .map(node_text)
                    .filter(|t| !t.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            prefix_lines(&body, &marker, &" ".repeat(marker.chars().count()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn table_text(rows: &[Value]) -> String {
    let mut out = vec![];
    for (i, row) in rows.iter().enumerate() {
        let cells: Vec<String> = kids(row)
            .iter()
            .map(|c| {
                blocks(kids(c))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .replace('|', "\\|")
            })
            .collect();
        out.push(format!("| {} |", cells.join(" | ")));
        if i == 0 && kids(row).iter().all(|c| c["type"] == "tableHeader") && !cells.is_empty() {
            out.push(format!("|{}", "---|".repeat(cells.len())));
        }
    }
    out.join("\n")
}

fn text_with_marks(n: &Value) -> String {
    let t = n["text"].as_str().unwrap_or("").to_string();
    let marks = n["marks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if marks.iter().any(|m| m["type"] == "code") {
        return format!("`{t}`");
    }
    if let Some(href) = marks
        .iter()
        .find(|m| m["type"] == "link")
        .and_then(|m| m["attrs"]["href"].as_str())
    {
        if href != t {
            return format!("{t} ({href})");
        }
    }
    t
}

fn node_text(n: &Value) -> String {
    let attrs = &n["attrs"];
    match n["type"].as_str().unwrap_or("") {
        "doc" => blocks(kids(n)),
        "paragraph" => inlines(kids(n)),
        "heading" => {
            let level = attrs["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize;
            format!("{} {}", "#".repeat(level), inlines(kids(n)))
        }
        "text" => text_with_marks(n),
        "hardBreak" => "\n".into(),
        "bulletList" => list_text(kids(n), None, false),
        "orderedList" => list_text(kids(n), Some(attrs["order"].as_u64().unwrap_or(1)), false),
        "taskList" => list_text(kids(n), None, true),
        "listItem" | "taskItem" => blocks(kids(n)),
        "codeBlock" => format!(
            "```{}\n{}\n```",
            attrs["language"].as_str().unwrap_or(""),
            kids(n)
                .iter()
                .filter_map(|t| t["text"].as_str())
                .collect::<String>()
        ),
        "blockquote" => prefix_lines(&blocks(kids(n)), "> ", "> "),
        "panel" => {
            let kind = attrs["panelType"].as_str().unwrap_or("info");
            prefix_lines(&blocks(kids(n)), &format!("> [{kind}] "), "> ")
        }
        "rule" => "---".into(),
        "mention" => {
            let name = attrs["text"]
                .as_str()
                .or(attrs["id"].as_str())
                .unwrap_or("someone");
            format!("@{}", name.trim_start_matches('@'))
        }
        "emoji" => attrs["shortName"]
            .as_str()
            .or(attrs["text"].as_str())
            .unwrap_or("")
            .to_string(),
        "inlineCard" | "blockCard" | "embedCard" => attrs["url"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| inlines(kids(n))),
        "status" => format!("[{}]", attrs["text"].as_str().unwrap_or("")),
        "date" => attrs["timestamp"]
            .as_str()
            .and_then(|t| t.parse::<i64>().ok())
            .or(attrs["timestamp"].as_i64())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        "table" => table_text(kids(n)),
        "media" | "mediaInline" => format!(
            "[attachment{}]",
            attrs["alt"]
                .as_str()
                .filter(|a| !a.is_empty())
                .map(|a| format!(": {a}"))
                .unwrap_or_default()
        ),
        "expand" | "nestedExpand" => {
            let body = blocks(kids(n));
            match attrs["title"].as_str().filter(|t| !t.is_empty()) {
                Some(t) => format!("▸ {t}\n{body}"),
                None => body,
            }
        }
        // Unknown or container nodes (mediaSingle, layoutSection, extensions…):
        // their text lives in their children.
        _ => {
            let c = kids(n);
            if c.is_empty() {
                n["text"].as_str().unwrap_or("").to_string()
            } else if c
                .iter()
                .all(|k| is_inline(k["type"].as_str().unwrap_or("")))
            {
                inlines(c)
            } else {
                blocks(c)
            }
        }
    }
}

/// Atlassian Document Format → readable plain text (Markdown-ish).
pub fn adf_to_text(v: &Value) -> String {
    let raw = match v {
        Value::Null => return String::new(),
        Value::String(t) => return t.clone(),
        Value::Array(a) => blocks(a),
        _ => node_text(v),
    };
    // Empty paragraphs in the source would otherwise pile up blank lines.
    let mut out = String::new();
    let mut blank = 0;
    for line in raw.lines() {
        let l = line.trim_end();
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.trim().to_string()
}

fn paragraph(lines: &[&str]) -> Value {
    let mut content = vec![];
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            content.push(json!({"type": "hardBreak"}));
        }
        if !l.is_empty() {
            content.push(json!({"type": "text", "text": l}));
        }
    }
    json!({"type": "paragraph", "content": content})
}

fn code_block(lang: &str, lines: &[&str]) -> Value {
    let text = lines.join("\n");
    let mut b = json!({"type": "codeBlock", "content": []});
    if !text.is_empty() {
        b["content"] = json!([{"type": "text", "text": text}]);
    }
    if !lang.is_empty() {
        b["attrs"] = json!({"language": lang});
    }
    b
}

/// Plain text → minimal ADF for Cloud writes: blank lines split paragraphs,
/// single newlines become hard breaks, ``` fences become code blocks. Text is
/// carried in `text` nodes verbatim, so nothing the model writes is
/// interpreted as markup structure.
pub fn text_to_adf(text: &str) -> Value {
    let text = text.replace("\r\n", "\n");
    let mut blocks_out: Vec<Value> = vec![];
    let mut para: Vec<&str> = vec![];
    let mut code: Option<(String, Vec<&str>)> = None;
    for line in text.split('\n') {
        let fence = line.trim_start().strip_prefix("```");
        if code.is_some() {
            if fence.is_some() {
                if let Some((lang, buf)) = code.take() {
                    blocks_out.push(code_block(&lang, &buf));
                }
            } else if let Some((_, buf)) = code.as_mut() {
                buf.push(line);
            }
            continue;
        }
        if let Some(lang) = fence {
            if !para.is_empty() {
                blocks_out.push(paragraph(&para));
                para.clear();
            }
            code = Some((lang.trim().to_string(), vec![]));
        } else if line.trim().is_empty() {
            if !para.is_empty() {
                blocks_out.push(paragraph(&para));
                para.clear();
            }
        } else {
            para.push(line);
        }
    }
    if let Some((lang, buf)) = code {
        blocks_out.push(code_block(&lang, &buf));
    }
    if !para.is_empty() {
        blocks_out.push(paragraph(&para));
    }
    json!({"type": "doc", "version": 1, "content": blocks_out})
}

// ───────────────────────────── network ─────────────────────────────

struct Jira<'a> {
    http: &'a reqwest::Client,
    c: &'a Creds,
}

impl Jira<'_> {
    async fn call(&self, req: &Req) -> Result<Value, String> {
        let secrets = self.c.secrets();
        let url = url_for(&self.c.base, req)?;
        let mut rq = self
            .http
            .request(req.method.clone(), url)
            .header(reqwest::header::AUTHORIZATION, self.c.auth_header()?)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, UA);
        if let Some(b) = &req.body {
            rq = rq.json(b);
        }
        let r = plugin_services::send("Jira", rq, &secrets).await?;
        interpret(r.status, &r.body, &req.what(), &secrets)
    }

    /// `assignee: "me"` → this account's accountId (Cloud) or username (Server).
    async fn resolve_me(&self, input: &Value) -> Result<Value, String> {
        if !s(input, "assignee").is_some_and(|a| a.eq_ignore_ascii_case("me")) {
            return Ok(input.clone());
        }
        let me = self.call(&myself_req(self.c.cloud())).await?;
        let id = if self.c.cloud() {
            me["accountId"].as_str()
        } else {
            me["name"].as_str()
        }
        .ok_or("Jira didn't say who this account is, so `assignee: \"me\"` can't be resolved — pass the accountId/username.")?;
        let mut out = input.clone();
        out["assignee"] = json!(id);
        Ok(out)
    }

    async fn run(&self, input: &Value) -> Result<String, String> {
        let cloud = self.c.cloud();
        let base = self.c.base.as_str();
        match input["action"].as_str().unwrap_or("") {
            "whoami" => Ok(format_myself(&self.call(&myself_req(cloud)).await?, self.c)),
            "search" => {
                let jql = s(input, "jql").ok_or("search needs `jql`, e.g. `project = PROJ AND statusCategory != Done ORDER BY updated DESC`.")?;
                let extra = str_list(&input["fields"]);
                let want = limit(input);
                let mut cursor = s(input, "cursor");
                let mut issues: Vec<Value> = vec![];
                let mut more = None;
                for page_no in 0..MAX_PAGES {
                    let req =
                        search_req(cloud, &jql, &extra, want - issues.len(), cursor.as_deref())?;
                    let page = self.call(&req).await?;
                    let got = page["issues"].as_array().cloned().unwrap_or_default();
                    let n = got.len();
                    issues.extend(got);
                    match next_cursor(cloud, &page, n) {
                        None => break,
                        Some(next)
                            if issues.len() >= want || n == 0 || page_no + 1 == MAX_PAGES =>
                        {
                            more = Some(next);
                            break;
                        }
                        Some(next) => cursor = Some(next),
                    }
                }
                issues.truncate(want);
                Ok(format_search(base, &jql, &issues, &extra, more.as_deref()))
            }
            "get_issue" => {
                let key = issue_key(input)?;
                Ok(format_issue(
                    base,
                    &self.call(&get_issue_req(cloud, &key)).await?,
                ))
            }
            "list_transitions" => {
                let key = issue_key(input)?;
                Ok(format_transitions(
                    &key,
                    &self.call(&transitions_req(cloud, &key)).await?,
                ))
            }
            "list_projects" => {
                let want = limit(input);
                let start = parse_start(s(input, "cursor").as_deref())?;
                let query = s(input, "query");
                let v = self
                    .call(&projects_req(cloud, want, start, query.as_deref()))
                    .await?;
                let (list, more) = if cloud {
                    let list = v["values"].as_array().cloned().unwrap_or_default();
                    let next =
                        v["startAt"].as_u64().map(|s| s as usize).unwrap_or(start) + list.len();
                    let more = (v["isLast"].as_bool() == Some(false) && !list.is_empty())
                        .then(|| next.to_string());
                    (list, more)
                } else {
                    page_projects(
                        v.as_array().map(Vec::as_slice).unwrap_or(&[]),
                        query.as_deref(),
                        start,
                        want,
                    )
                };
                Ok(format_projects(base, &list, more.as_deref()))
            }
            "create_issue" => {
                let input = self.resolve_me(input).await?;
                let v = self.call(&create_req(cloud, &input)?).await?;
                let key = v["key"].as_str().unwrap_or("?");
                Ok(format!("Created {key} · {}", browse_url(base, key)))
            }
            "update_issue" => {
                let key = issue_key(input)?;
                let input = self.resolve_me(input).await?;
                let (req, changed) = update_req(cloud, &key, &input)?;
                self.call(&req).await?;
                Ok(format!(
                    "Updated {key} ({}) · {}",
                    changed.join(", "),
                    browse_url(base, &key)
                ))
            }
            "transition" => {
                let key = issue_key(input)?;
                let want = s(input, "transition").ok_or("transition needs `transition`: a transition name or id (see list_transitions) or the target status name.")?;
                let list = self.call(&transitions_req(cloud, &key)).await?;
                let (id, name, to) = resolve_transition(&key, &list, &want)?;
                let req = transition_req(
                    cloud,
                    &key,
                    &id,
                    s(input, "comment").as_deref(),
                    s(input, "resolution").as_deref(),
                );
                self.call(&req).await?;
                Ok(format!("{key}: {name} → {to} · {}", browse_url(base, &key)))
            }
            "comment" => {
                let key = issue_key(input)?;
                let body = s(input, "body").ok_or("comment needs `body` (the comment text).")?;
                let v = self.call(&comment_req(cloud, &key, &body)).await?;
                let url = match v["id"].as_str() {
                    Some(id) => format!("{}?focusedCommentId={id}", browse_url(base, &key)),
                    None => browse_url(base, &key),
                };
                Ok(format!("Commented on {key} · {url}"))
            }
            RAW => {
                let v = self.call(&raw_req(input)?).await?;
                if v.is_null() {
                    return Ok("OK (no content).".into());
                }
                Ok(plugin_services::clip(
                    &serde_json::to_string_pretty(&v).unwrap_or_default(),
                    RAW_CLIP,
                ))
            }
            other => Err(format!(
                "Unknown jira action `{}`. Use one of: {}, {}, {RAW}.",
                plugin_services::clip(other, 60),
                READS.join(", "),
                WRITES.join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ATATT3xFfGF0-dummy-token-123456";
    const BASE: &str = "https://acme.atlassian.net";
    const SERVER: &str = "https://jira.example.com/jira";

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn cfg(base: &str, email: &str, token: &str) -> JiraCfg {
        JiraCfg {
            enabled: true,
            base_url: base.into(),
            email: email.into(),
            token: token.into(),
        }
    }

    fn cloud() -> Creds {
        creds(&cfg(BASE, "ada@example.com", TOKEN), &no_env).unwrap()
    }

    fn server() -> Creds {
        creds(&cfg(SERVER, "", TOKEN), &no_env).unwrap()
    }

    // ── classification and schema ──

    #[test]
    fn every_action_is_classified() {
        for a in READS {
            assert!(!is_write(&json!({"action": a})), "{a} is a read");
        }
        for a in WRITES {
            assert!(is_write(&json!({"action": a})), "{a} is a write");
        }
        for bad in [
            json!({"action": "delete_issue"}),
            json!({}),
            json!({"action": 3}),
        ] {
            assert!(is_write(&bad), "{bad}");
        }
    }

    #[test]
    fn schema_enum_matches_reads_writes_and_raw() {
        let sc = schema();
        assert_eq!(sc["name"], "jira");
        assert_eq!(sc["input_schema"]["required"], json!(["action"]));
        let e: Vec<&str> = sc["input_schema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for a in &e {
            assert!(READS.contains(a) || WRITES.contains(a) || *a == RAW, "{a}");
        }
        for a in READS.iter().chain(WRITES) {
            assert!(e.contains(a), "{a} missing from enum");
        }
        assert!(e.contains(&RAW));
        let desc = sc["description"].as_str().unwrap();
        for a in &e {
            assert!(desc.contains(a), "description must mention {a}");
        }
        assert!(desc.contains("ask the user first"));
    }

    #[test]
    fn raw_api_classification_fails_closed() {
        let read = |m: Value, p: &str| !is_write(&json!({"action": "api", "method": m, "path": p}));
        assert!(read(Value::Null, "/rest/api/3/myself"));
        assert!(read(json!("get"), "rest/api/2/issue/X-1"));
        for p in [
            "//evil.com/x",
            "https://evil.com/x",
            "https://acme.atlassian.net@evil.com/x",
            "https://acme.atlassian.net.evil.com/x",
            "\\\\evil.com\\x",
            "/\\evil.com",
            "%2f%2fevil.com",
            "/x\r\nHost: evil.com",
            "",
        ] {
            assert!(!read(json!("GET"), p), "{p:?} must be a write");
        }
        for m in [
            json!("POST"),
            json!("PUT"),
            json!("DELETE"),
            json!("PATCH"),
            json!("BREW"),
            json!(1),
        ] {
            assert!(!read(m.clone(), "/rest/api/3/myself"), "{m}");
        }
        assert!(is_write(&json!({"action": "api"})));
    }

    #[test]
    fn raw_api_requests_stay_on_the_site() {
        for bad in [
            "//evil.com/x",
            "https://evil.com/rest/api/3/myself",
            "https://acme.atlassian.net@evil.com/x",
            "https://acme.atlassian.net.evil.com/x",
            "http://acme.atlassian.net/rest/api/3/myself",
            "\\\\evil.com\\x",
            "/\\evil.com",
            "%2f%2fevil.com/x",
            "/%2F%2Fevil.com",
            "/x\r\nHost: evil.com",
        ] {
            let r = raw_req(&json!({"action": "api", "path": bad})).and_then(|r| url_for(BASE, &r));
            assert!(r.is_err(), "{bad:?} must be refused, got {r:?}");
        }
        let r = raw_req(
            &json!({"action": "api", "path": "/rest/api/3/issue/X-1/changelog?maxResults=5"}),
        )
        .unwrap();
        assert_eq!(r.method, Method::GET);
        assert_eq!(
            url_for(BASE, &r).unwrap().as_str(),
            "https://acme.atlassian.net/rest/api/3/issue/X-1/changelog?maxResults=5"
        );
        // A Server context path is kept.
        assert_eq!(
            url_for(
                SERVER,
                &raw_req(&json!({"path": "rest/api/2/myself"})).unwrap()
            )
            .unwrap()
            .as_str(),
            "https://jira.example.com/jira/rest/api/2/myself"
        );
        let post = raw_req(
            &json!({"method": "post", "path": "/rest/api/3/issueLink", "body": "{\"a\":1}"}),
        )
        .unwrap();
        assert_eq!(post.method, Method::POST);
        assert_eq!(post.body, Some(json!({"a": 1})));
        assert!(raw_req(&json!({"method": "BREW", "path": "/x"})).is_err());
        assert!(raw_req(&json!({"method": 7, "path": "/x"})).is_err());
        assert!(raw_req(&json!({"path": "/x", "body": "{nope"})).is_err());
        assert!(raw_req(&json!({})).is_err());
    }

    // ── credentials ──

    #[test]
    fn creds_settings_beat_env_and_pick_the_auth_mode() {
        let env = |k: &str| match k {
            "JIRA_BASE_URL" => Some("https://env.atlassian.net".to_string()),
            "JIRA_EMAIL" => Some("env@example.com".to_string()),
            "JIRA_API_TOKEN" => Some("env-token-abcdef".to_string()),
            _ => None,
        };
        let c = creds(
            &cfg("https://acme.atlassian.net/", "ada@example.com", TOKEN),
            &env,
        )
        .unwrap();
        assert_eq!(c.base, BASE);
        assert_eq!(c.email.as_deref(), Some("ada@example.com"));
        assert_eq!(c.token, TOKEN);
        assert_eq!(c.source, "settings");
        assert!(c.cloud());
        let b = c.basic.clone().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&b)
            .unwrap();
        assert_eq!(decoded, format!("ada@example.com:{TOKEN}").into_bytes());

        let e = creds(&JiraCfg::default(), &env).unwrap();
        assert_eq!(e.base, "https://env.atlassian.net");
        assert_eq!(e.token, "env-token-abcdef");
        assert_eq!(e.email.as_deref(), Some("env@example.com"));
        assert_eq!(e.source, "JIRA_API_TOKEN + JIRA_EMAIL + JIRA_BASE_URL");

        // Mixed: token in settings, the rest from env.
        let m = creds(&cfg("", "", TOKEN), &env).unwrap();
        assert_eq!(m.source, "settings + JIRA_EMAIL + JIRA_BASE_URL");

        // No email → Server/DC bearer + v2.
        let s = server();
        assert!(!s.cloud());
        assert!(s.basic.is_none());
        assert_eq!(s.base, SERVER);
        assert_eq!(api(s.cloud()), "/rest/api/2");
        assert_eq!(api(true), "/rest/api/3");
    }

    #[test]
    fn creds_missing_or_bad_say_where_to_fix_and_leak_nothing() {
        let e = creds(&cfg("", "", TOKEN), &no_env).unwrap_err();
        assert!(
            e.contains("Settings → Connectors → Jira") && e.contains("JIRA_BASE_URL"),
            "{e}"
        );
        assert!(!e.contains(TOKEN));
        let e = creds(&cfg(BASE, "ada@example.com", ""), &no_env).unwrap_err();
        assert!(
            e.contains("Settings → Connectors → Jira") && e.contains("JIRA_API_TOKEN"),
            "{e}"
        );
        let e = creds(&cfg(BASE, "", TOKEN), &no_env).unwrap_err();
        assert!(e.contains("JIRA_EMAIL") && !e.contains(TOKEN), "{e}");
        for bad in [
            "acme.atlassian.net",
            "ftp://x.com",
            "file:///etc",
            "https://u:p@jira.x.com",
            "not a url",
        ] {
            let e = creds(&cfg(bad, "a@b.c", TOKEN), &no_env).unwrap_err();
            assert!(!e.contains(TOKEN), "{e}");
        }
        // A pasted board URL on Cloud is cut back to the site.
        let c = creds(
            &cfg(
                "https://acme.atlassian.net/jira/software/projects/X/boards/1?x=1#y",
                "a@b.c",
                TOKEN,
            ),
            &no_env,
        )
        .unwrap();
        assert_eq!(c.base, BASE);
        let c = creds(&cfg("http://jira.local:8080/", "", TOKEN), &no_env).unwrap();
        assert_eq!(c.base, "http://jira.local:8080");
        let basic = cloud().basic.unwrap();
        let dbg = format!("{:?}{:?}", cloud(), cfg(BASE, "a", TOKEN));
        assert!(!dbg.contains(TOKEN) && !dbg.contains(&basic), "{dbg}");
    }

    #[test]
    fn errors_never_contain_the_token_or_basic_credential() {
        let c = cloud();
        let basic = c.basic.clone().unwrap();
        let echo = format!("{{\"errorMessages\":[\"bad header Basic {basic} / {TOKEN}\"]}}");
        for status in [400u16, 401, 403, 404, 500] {
            let e = interpret(status, &echo, "GET /rest/api/3/myself", &c.secrets()).unwrap_err();
            assert!(!e.contains(TOKEN) && !e.contains(&basic), "{e}");
            assert!(
                e.starts_with(&format!("Jira {status} on GET /rest/api/3/myself")),
                "{e}"
            );
        }
        let e = interpret(
            200,
            &format!("<html>{basic}</html>"),
            "GET /x",
            &c.secrets(),
        )
        .unwrap_err();
        assert!(!e.contains(&basic) && e.contains("isn't JSON"), "{e}");
    }

    #[test]
    fn http_errors_carry_hints_and_jira_messages() {
        let s = server();
        let body = r#"{"errorMessages":["Issue type is required."],"errors":{"summary":"You must specify a summary of the issue.","customfield_10016":"Number value expected"}}"#;
        let e = interpret(400, body, "POST /rest/api/2/issue", &s.secrets()).unwrap_err();
        assert!(e.contains("Issue type is required."), "{e}");
        assert!(e.contains("summary: You must specify a summary"), "{e}");
        assert!(
            e.contains("customfield_10016: Number value expected"),
            "{e}"
        );
        let e = interpret(401, "", "GET /rest/api/2/myself", &s.secrets()).unwrap_err();
        assert!(e.contains("Settings → Connectors → Jira"), "{e}");
        let e = interpret(403, r#"{"message":"forbidden"}"#, "GET /x", &s.secrets()).unwrap_err();
        assert!(e.contains("forbidden") && e.contains("permission"), "{e}");
        let e = interpret(
            404,
            r#"{"errorMessages":["Issue does not exist or you do not have permission to see it."],"errors":{}}"#,
            "GET /x",
            &s.secrets(),
        )
        .unwrap_err();
        assert!(
            e.contains("Issue does not exist") && e.contains("wrong Jira site"),
            "{e}"
        );
        assert_eq!(interpret(204, "", "PUT /x", &[]).unwrap(), Value::Null);
        assert_eq!(
            interpret(200, r#"{"a":1}"#, "GET /x", &[]).unwrap(),
            json!({"a": 1})
        );
        assert!(
            server_message("<html>\n  <body>Login</body></html>").contains("<body>Login</body>")
        );
    }

    #[test]
    fn auth_header_is_basic_on_cloud_and_bearer_on_server() {
        let c = cloud();
        let h = c.auth_header().unwrap();
        assert!(h.is_sensitive());
        assert_eq!(
            h.to_str().unwrap(),
            format!("Basic {}", c.basic.clone().unwrap())
        );
        assert_eq!(
            server().auth_header().unwrap().to_str().unwrap(),
            format!("Bearer {TOKEN}")
        );
        let bad = creds(&cfg(SERVER, "", "tok\u{7f}en-123456"), &no_env).unwrap();
        let e = bad.auth_header().unwrap_err();
        assert!(!e.contains("en-123456"), "{e}");
    }

    // ── request building ──

    #[test]
    fn search_uses_the_enhanced_endpoint_on_cloud_and_v2_on_server() {
        let jql = "project = \"A&B\" AND summary ~ \"x?y=1#z\"";
        let r = search_req(true, jql, &["duedate".into(), "summary".into()], 20, None).unwrap();
        assert_eq!(r.method, Method::POST);
        assert_eq!(r.path, "/rest/api/3/search/jql");
        let b = r.body.clone().unwrap();
        assert_eq!(
            b["jql"], jql,
            "JQL travels verbatim as JSON, not in the URL"
        );
        assert_eq!(b["maxResults"], 20);
        assert_eq!(
            b["fields"],
            json!([
                "summary",
                "status",
                "issuetype",
                "priority",
                "assignee",
                "duedate"
            ])
        );
        assert!(b.get("nextPageToken").is_none());
        assert_eq!(
            url_for(BASE, &r).unwrap().as_str(),
            "https://acme.atlassian.net/rest/api/3/search/jql"
        );
        let r = search_req(true, jql, &[], 5, Some("tok==")).unwrap();
        assert_eq!(r.body.unwrap()["nextPageToken"], "tok==");

        let r = search_req(false, jql, &[], 7, Some("40")).unwrap();
        assert_eq!(r.path, "/rest/api/2/search");
        let b = r.body.unwrap();
        assert_eq!(
            (b["startAt"].clone(), b["maxResults"].clone()),
            (json!(40), json!(7))
        );
        assert!(search_req(false, jql, &[], 7, Some("abc")).is_err());
    }

    #[test]
    fn cursors_follow_each_paging_style() {
        let c = |cloud: bool, v: Value, n: usize| next_cursor(cloud, &v, n);
        assert_eq!(
            c(true, json!({"nextPageToken": "n1", "isLast": false}), 20),
            Some("n1".into())
        );
        assert_eq!(
            c(true, json!({"nextPageToken": "n1", "isLast": true}), 20),
            None
        );
        assert_eq!(c(true, json!({"isLast": true}), 3), None);
        assert_eq!(
            c(false, json!({"startAt": 20, "total": 45}), 20),
            Some("40".into())
        );
        assert_eq!(c(false, json!({"startAt": 40, "total": 45}), 5), None);
        assert_eq!(c(false, json!({"startAt": 0, "total": 45}), 0), None);
    }

    #[test]
    fn limit_defaults_and_clamps() {
        assert_eq!(limit(&json!({})), 20);
        assert_eq!(limit(&json!({"limit": 0})), 1);
        assert_eq!(limit(&json!({"limit": 1000})), 100);
        assert_eq!(limit(&json!({"limit": "5"})), 5);
    }

    #[test]
    fn issue_keys_are_validated_before_reaching_a_path() {
        assert_eq!(issue_key(&json!({"key": " proj-12 "})).unwrap(), "PROJ-12");
        assert_eq!(issue_key(&json!({"key": "10042"})).unwrap(), "10042");
        assert_eq!(issue_key(&json!({"key": "AB_2-7"})).unwrap(), "AB_2-7");
        for bad in [
            "",
            "..",
            "X-1/../../myself",
            "X-1?x=1",
            "X-",
            "-1",
            "1X-2",
            "X 1",
            "X-1#",
            "X-%2e",
        ] {
            assert!(issue_key(&json!({"key": bad})).is_err(), "{bad:?}");
        }
        assert!(issue_key(&json!({})).unwrap_err().contains("PROJ-123"));
    }

    #[test]
    fn read_requests_have_the_right_paths_and_queries() {
        let r = get_issue_req(true, "PROJ-1");
        assert_eq!(
            (r.method.clone(), r.path.as_str()),
            (Method::GET, "/rest/api/3/issue/PROJ-1")
        );
        let u = url_for(BASE, &r).unwrap();
        assert!(
            u.as_str().starts_with(
                "https://acme.atlassian.net/rest/api/3/issue/PROJ-1?fields=summary%2Cstatus"
            ),
            "{u}"
        );
        assert_eq!(
            get_issue_req(false, "PROJ-1").path,
            "/rest/api/2/issue/PROJ-1"
        );
        assert_eq!(
            transitions_req(true, "PROJ-1").path,
            "/rest/api/3/issue/PROJ-1/transitions"
        );
        assert_eq!(myself_req(false).path, "/rest/api/2/myself");
        let p = projects_req(true, 10, 20, Some("a&b c"));
        assert_eq!(
            url_for(BASE, &p).unwrap().as_str(),
            "https://acme.atlassian.net/rest/api/3/project/search?maxResults=10&startAt=20&orderBy=key&expand=lead&query=a%26b+c"
        );
        assert_eq!(
            url_for(SERVER, &projects_req(false, 10, 0, None))
                .unwrap()
                .as_str(),
            "https://jira.example.com/jira/rest/api/2/project?expand=lead"
        );
        assert!(parse_start(Some("x")).is_err());
        assert_eq!(parse_start(None).unwrap(), 0);
    }

    #[test]
    fn create_issue_builds_fields_for_each_api_version() {
        let input = json!({
            "project": "proj", "issue_type": "Bug", "summary": "Crash on \"save\"",
            "description": "Steps:\n1. open\n\nBoom", "labels": ["ui", "crash"],
            "priority": "High", "assignee": "5b10ac8d82e05b22cc7d4ef5", "parent": "proj-1",
            "fields": {"customfield_10016": 3, "summary": "overridden?", "duedate": "2026-01-31"}
        });
        let r = create_req(true, &input).unwrap();
        assert_eq!(
            (r.method.clone(), r.path.as_str()),
            (Method::POST, "/rest/api/3/issue")
        );
        let f = &r.body.as_ref().unwrap()["fields"];
        assert_eq!(f["project"], json!({"key": "PROJ"}));
        assert_eq!(f["issuetype"], json!({"name": "Bug"}));
        assert_eq!(
            f["summary"], "Crash on \"save\"",
            "named params win over the passthrough"
        );
        assert_eq!(f["description"]["type"], "doc");
        assert_eq!(f["labels"], json!(["ui", "crash"]));
        assert_eq!(f["priority"], json!({"name": "High"}));
        assert_eq!(
            f["assignee"],
            json!({"accountId": "5b10ac8d82e05b22cc7d4ef5"})
        );
        assert_eq!(f["parent"], json!({"key": "PROJ-1"}));
        assert_eq!(f["customfield_10016"], 3);
        assert_eq!(f["duedate"], "2026-01-31");

        let mut srv = input.clone();
        srv["assignee"] = json!("ada");
        let r = create_req(false, &srv).unwrap();
        assert_eq!(r.path, "/rest/api/2/issue");
        let f = &r.body.as_ref().unwrap()["fields"];
        assert_eq!(f["description"], "Steps:\n1. open\n\nBoom");
        assert_eq!(f["assignee"], json!({"name": "ada"}));

        let min = create_req(true, &json!({"project": "P", "summary": "s"})).unwrap();
        assert_eq!(min.body.unwrap()["fields"]["issuetype"]["name"], "Task");
        assert!(create_req(true, &json!({"summary": "s"}))
            .unwrap_err()
            .contains("project"));
        assert!(create_req(true, &json!({"project": "P"}))
            .unwrap_err()
            .contains("summary"));
        assert!(create_req(
            true,
            &json!({"project": "P", "summary": "s", "fields": [1]})
        )
        .is_err());
        // A passthrough string description is converted on Cloud only.
        let pt = create_req(
            true,
            &json!({"project": "P", "summary": "s", "fields": {"description": "hi"}}),
        )
        .unwrap();
        assert_eq!(pt.body.unwrap()["fields"]["description"]["type"], "doc");
    }

    #[test]
    fn update_issue_sets_fields_and_label_ops() {
        let (r, changed) = update_req(
            true,
            "PROJ-2",
            &json!({"summary": "New", "assignee": "none", "add_labels": ["a"], "remove_labels": "b, c", "fields": {"duedate": null}}),
        )
        .unwrap();
        assert_eq!(
            (r.method.clone(), r.path.as_str()),
            (Method::PUT, "/rest/api/3/issue/PROJ-2")
        );
        let b = r.body.unwrap();
        assert_eq!(b["fields"]["summary"], "New");
        assert!(
            b["fields"]["assignee"].is_null() && b["fields"].get("assignee").is_some(),
            "none unassigns"
        );
        assert!(b["fields"].get("duedate").is_some());
        assert_eq!(
            b["update"]["labels"],
            json!([{"add": "a"}, {"remove": "b"}, {"remove": "c"}])
        );
        assert!(
            changed.contains(&"summary".to_string()) && changed.contains(&"duedate".to_string())
        );
        let (r, _) = update_req(false, "PROJ-2", &json!({"description": "plain"})).unwrap();
        assert_eq!(r.body.unwrap()["fields"]["description"], "plain");
        assert!(update_req(true, "PROJ-2", &json!({"key": "PROJ-2"}))
            .unwrap_err()
            .contains("something to change"));
    }

    #[test]
    fn transition_and_comment_requests() {
        let r = transition_req(true, "PROJ-3", "31", Some("Shipped in 1.2"), Some("Done"));
        assert_eq!(
            (r.method.clone(), r.path.as_str()),
            (Method::POST, "/rest/api/3/issue/PROJ-3/transitions")
        );
        let b = r.body.unwrap();
        assert_eq!(b["transition"], json!({"id": "31"}));
        assert_eq!(b["fields"]["resolution"], json!({"name": "Done"}));
        assert_eq!(b["update"]["comment"][0]["add"]["body"]["type"], "doc");
        let r = transition_req(false, "PROJ-3", "31", None, None);
        assert_eq!(r.path, "/rest/api/2/issue/PROJ-3/transitions");
        let b = r.body.unwrap();
        assert!(b.get("fields").is_none() && b.get("update").is_none());

        let c = comment_req(true, "PROJ-3", "Hello <b>&</b>");
        assert_eq!(c.path, "/rest/api/3/issue/PROJ-3/comment");
        assert_eq!(
            c.body.unwrap()["body"]["content"][0]["content"][0]["text"],
            "Hello <b>&</b>"
        );
        assert_eq!(
            comment_req(false, "PROJ-3", "hi").body.unwrap(),
            json!({"body": "hi"})
        );
    }

    #[test]
    fn transitions_resolve_by_id_name_or_target_status() {
        let v = json!({"transitions": [
            {"id": "11", "name": "To Do", "to": {"name": "To Do"}},
            {"id": "21", "name": "Start progress", "to": {"name": "In Progress"}, "hasScreen": false},
            {"id": "31", "name": "Resolve", "to": {"name": "Done"}, "hasScreen": true},
            {"id": "41", "name": "Close", "to": {"name": "Done"}}
        ]});
        assert_eq!(resolve_transition("P-1", &v, "21").unwrap().0, "21");
        assert_eq!(
            resolve_transition("P-1", &v, "start PROGRESS").unwrap().0,
            "21"
        );
        assert_eq!(
            resolve_transition("P-1", &v, "in progress").unwrap().0,
            "21"
        );
        let e = resolve_transition("P-1", &v, "Done").unwrap_err();
        assert!(
            e.contains("Resolve (id 31 → Done)") && e.contains("Close (id 41"),
            "ambiguous target: {e}"
        );
        assert!(resolve_transition("P-1", &v, "Ship").is_err());
        assert!(resolve_transition("P-1", &json!({"transitions": []}), "x")
            .unwrap_err()
            .contains("no transitions"));
        let t = format_transitions("P-1", &v);
        assert!(t.contains("31 · Resolve → Done (has a screen"), "{t}");
        assert!(format_transitions("P-1", &json!({})).contains("No transitions"));
    }

    // ── response formatting ──

    fn search_fixture() -> Value {
        json!({
            "issues": [
                {"id": "10001", "key": "PROJ-1", "fields": {
                    "summary": "Login fails on Safari",
                    "status": {"name": "In Progress", "statusCategory": {"key": "indeterminate"}},
                    "issuetype": {"name": "Bug"}, "priority": {"name": "High"},
                    "assignee": {"accountId": "5b10ac8d82e05b22cc7d4ef5", "displayName": "Ada Lovelace"},
                    "duedate": "2026-02-01",
                    "customfield_10020": [{"id": 3, "name": "Sprint 7"}]}},
                {"id": "10002", "key": "PROJ-2", "fields": {
                    "summary": "Docs", "status": {"name": "To Do"}, "issuetype": {"name": "Task"},
                    "priority": null, "assignee": null}},
                {"id": "10003", "key": "PROJ-3", "fields": {}}
            ],
            "nextPageToken": "CAEaAggD", "isLast": false
        })
    }

    #[test]
    fn search_rows_from_fixture() {
        let v = search_fixture();
        let issues = v["issues"].as_array().unwrap();
        let extra = [
            "duedate".to_string(),
            "customfield_10020".into(),
            "status".into(),
        ];
        let out = format_search(BASE, "project = PROJ", issues, &extra, Some("CAEaAggD"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "3 issue(s) for `project = PROJ` on https://acme.atlassian.net:"
        );
        assert_eq!(lines[1], "PROJ-1 Login fails on Safari [In Progress] · Bug · High · @Ada Lovelace · https://acme.atlassian.net/browse/PROJ-1");
        assert_eq!(lines[2], "    duedate: 2026-02-01");
        assert_eq!(lines[3], "    customfield_10020: Sprint 7");
        assert_eq!(
            lines[4],
            "PROJ-2 Docs [To Do] · Task · unassigned · https://acme.atlassian.net/browse/PROJ-2"
        );
        assert_eq!(
            lines[5],
            "PROJ-3 (no summary) [?] · unassigned · https://acme.atlassian.net/browse/PROJ-3"
        );
        assert!(out.ends_with("cursor: \"CAEaAggD\"."), "{out}");
        assert!(!out.contains("status:"), "default fields are not repeated");
        assert_eq!(
            format_search(BASE, "x", &[], &[], None),
            "No issues match `x` on https://acme.atlassian.net."
        );
    }

    /// A trimmed Cloud `GET /issue/KEY` reply. Parsed from text: it nests
    /// deeper than `json!` can expand.
    fn issue_fixture() -> Value {
        serde_json::from_str(r#"{
            "id": "10001", "key": "PROJ-1",
            "fields": {
                "summary": "Login fails on Safari",
                "status": {"name": "In Progress"}, "issuetype": {"name": "Bug"}, "priority": {"name": "High"},
                "assignee": {"displayName": "Ada Lovelace"}, "reporter": {"displayName": "Grace Hopper"},
                "project": {"key": "PROJ", "name": "Project X"},
                "labels": ["safari", "auth"], "components": [{"id": "1", "name": "Web"}],
                "fixVersions": [{"id": "2", "name": "1.2.0"}],
                "created": "2026-01-02T10:31:00.000+0000", "updated": "2026-01-05T08:00:00.000+0000",
                "resolution": null,
                "parent": {"key": "PROJ-0", "fields": {"summary": "Auth epic", "status": {"name": "To Do"}, "issuetype": {"name": "Epic"}}},
                "subtasks": [{"key": "PROJ-5", "fields": {"summary": "Repro", "status": {"name": "Done"}}}],
                "issuelinks": [{"type": {"inward": "is blocked by", "outward": "blocks"},
                    "outwardIssue": {"key": "PROJ-9", "fields": {"summary": "Release", "status": {"name": "To Do"}}}}],
                "description": {"type": "doc", "version": 1, "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "Safari 17 shows a blank page."}]}]},
                "comment": {"total": 2, "comments": [
                    {"author": {"displayName": "Grace Hopper"}, "created": "2026-01-03T09:00:00.000+0000",
                     "body": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [
                        {"type": "mention", "attrs": {"id": "x", "text": "@Ada Lovelace"}},
                        {"type": "text", "text": " can you look?"}]}]}},
                    {"author": {"displayName": "Ada Lovelace"}, "created": "2026-01-04T09:00:00.000+0000",
                     "body": "Server-style plain body"}
                ]}
            }
        }"#)
        .expect("fixture is valid JSON")
    }

    #[test]
    fn issue_details_from_fixture() {
        let out = format_issue(BASE, &issue_fixture());
        for want in [
            "PROJ-1 Login fails on Safari [In Progress] · Bug · High · @Ada Lovelace · https://acme.atlassian.net/browse/PROJ-1",
            "Project: Project X (PROJ) · Reporter: @Grace Hopper · Created: 2026-01-02 10:31 · Updated: 2026-01-05 08:00",
            "Labels: safari, auth · Components: Web · Fix versions: 1.2.0",
            "Parent (Epic): PROJ-0 Auth epic [To Do] · https://acme.atlassian.net/browse/PROJ-0",
            "  PROJ-5 Repro [Done]",
            "  blocks PROJ-9 Release [To Do]",
            "Description:\nSafari 17 shows a blank page.",
            "Comments (2):",
            "--- @Grace Hopper · 2026-01-03 09:00\n@Ada Lovelace can you look?",
            "Server-style plain body",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        assert!(!out.contains("Resolution"));
    }

    #[test]
    fn issue_details_handle_missing_fields_and_clip() {
        let out = format_issue(BASE, &json!({"key": "X-1", "fields": {}}));
        assert!(out.contains("X-1 (no summary) [?] · unassigned"), "{out}");
        assert!(
            out.contains("Description:\n(none)") && out.contains("Comments: none"),
            "{out}"
        );
        let long = "x".repeat(BODY_CLIP + 500);
        let many: Vec<Value> = (0..60)
            .map(|i| json!({"author": {"displayName": format!("u{i}")}, "body": "y".repeat(COMMENT_CLIP + 10)}))
            .collect();
        let out = format_issue(
            BASE,
            &json!({"key": "X-1", "fields": {"description": long, "comment": {"total": 75, "comments": many}}}),
        );
        assert!(out.contains("[… truncated]"));
        assert!(out.contains("Comments (the last 50 of 75):"), "{out}");
        assert!(!out.contains("@u9 ") && out.contains("@u10 ") && out.contains("@u59 "));
    }

    #[test]
    fn projects_from_fixtures() {
        let cloud_page = json!({"startAt": 0, "maxResults": 2, "total": 3, "isLast": false, "values": [
            {"key": "PROJ", "name": "Project X", "projectTypeKey": "software", "lead": {"displayName": "Ada Lovelace"}},
            {"key": "OPS", "name": "Operations", "projectTypeKey": "service_desk"}
        ]});
        let list = cloud_page["values"].as_array().unwrap();
        let out = format_projects(BASE, list, Some("2"));
        assert!(
            out.contains("PROJ Project X · software · lead @Ada Lovelace · https://acme.atlassian.net/browse/PROJ"),
            "{out}"
        );
        assert!(
            out.contains("OPS Operations · service_desk · https://acme.atlassian.net/browse/OPS")
        );
        assert!(out.ends_with("cursor: \"2\"."));
        assert!(format_projects(BASE, &[], None).contains("No projects"));

        let server_list = vec![
            json!({"key": "AA", "name": "Alpha"}),
            json!({"key": "BB", "name": "Beta"}),
            json!({"key": "AB", "name": "Gamma"}),
        ];
        let (p, more) = page_projects(&server_list, Some("a"), 0, 2);
        assert_eq!(p.len(), 2);
        assert_eq!(more.as_deref(), Some("2"));
        let (p, more) = page_projects(&server_list, Some("a"), 2, 2);
        assert_eq!((p.len(), more), (1, None));
        let (p, _) = page_projects(&server_list, Some("beta"), 0, 10);
        assert_eq!(p[0]["key"], "BB");
    }

    #[test]
    fn whoami_from_fixtures() {
        let c = cloud();
        let me = json!({"accountId": "5b10ac8d82e05b22cc7d4ef5", "emailAddress": "ada@example.com", "displayName": "Ada Lovelace", "active": true});
        assert_eq!(
            identity_line(&me, &c),
            "Ada Lovelace <ada@example.com> · acme.atlassian.net"
        );
        let out = format_myself(&me, &c);
        assert!(
            out.contains("accountId 5b10ac8d82e05b22cc7d4ef5") && out.contains("REST v3"),
            "{out}"
        );
        let s = server();
        let me = json!({"name": "ada", "key": "JIRAUSER10000", "displayName": "Ada L", "emailAddress": ""});
        let out = format_myself(&me, &s);
        assert!(
            out.starts_with("Ada L · jira.example.com · username ada"),
            "{out}"
        );
        assert!(out.contains("REST v2") && !out.contains(TOKEN));
        assert_eq!(
            identity_line(&json!({}), &s),
            "(unknown user) · jira.example.com"
        );
    }

    // ── ADF ──

    #[test]
    fn adf_renders_common_nodes() {
        let p = |t: &str| json!({"type": "paragraph", "content": [{"type": "text", "text": t}]});
        let li = |t: &str| json!({"type": "listItem", "content": [p(t)]});
        let doc = json!({"type": "doc", "version": 1, "content": [
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Steps"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "See "},
                {"type": "text", "text": "the docs", "marks": [{"type": "link", "attrs": {"href": "https://ex.com/d"}}]},
                {"type": "text", "text": " and "},
                {"type": "text", "text": "cfg.yml", "marks": [{"type": "code"}]},
                {"type": "hardBreak"},
                {"type": "text", "text": "ping "},
                {"type": "mention", "attrs": {"id": "abc", "text": "Grace"}},
                {"type": "text", "text": " "},
                {"type": "emoji", "attrs": {"shortName": ":tada:", "text": "🎉"}},
                {"type": "text", "text": " "},
                {"type": "inlineCard", "attrs": {"url": "https://acme.atlassian.net/browse/PROJ-2"}},
                {"type": "text", "text": " "},
                {"type": "status", "attrs": {"text": "BLOCKED", "color": "red"}}
            ]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [p("one"), {"type": "bulletList", "content": [li("nested")]}]},
                li("two")
            ]},
            {"type": "orderedList", "attrs": {"order": 3}, "content": [li("third"), li("fourth")]},
            {"type": "codeBlock", "attrs": {"language": "rust"}, "content": [{"type": "text", "text": "fn main() {}\nlet x = 1;"}]},
            {"type": "blockquote", "content": [p("quoted")]},
            {"type": "rule"},
            {"type": "paragraph", "content": []},
            {"type": "paragraph", "content": []},
            {"type": "table", "content": [
                {"type": "tableRow", "content": [
                    {"type": "tableHeader", "content": [p("Browser")]},
                    {"type": "tableHeader", "content": [p("Result")]}]},
                {"type": "tableRow", "content": [
                    {"type": "tableCell", "content": [p("Safari")]},
                    {"type": "tableCell", "content": [p("a|b")]}]}
            ]},
            {"type": "mediaSingle", "content": [{"type": "media", "attrs": {"id": "m1", "type": "file", "alt": "shot.png"}}]},
            {"type": "someFutureNode", "content": [p("still readable")]}
        ]});
        let want = "## Steps\n\n\
See the docs (https://ex.com/d) and `cfg.yml`\nping @Grace :tada: https://acme.atlassian.net/browse/PROJ-2 [BLOCKED]\n\n\
- one\n  - nested\n- two\n\n\
3. third\n4. fourth\n\n\
```rust\nfn main() {}\nlet x = 1;\n```\n\n\
> quoted\n\n\
---\n\n\
| Browser | Result |\n|---|---|\n| Safari | a\\|b |\n\n\
[attachment: shot.png]\n\n\
still readable";
        assert_eq!(adf_to_text(&doc), want);
    }

    #[test]
    fn adf_edge_cases() {
        assert_eq!(adf_to_text(&Value::Null), "");
        assert_eq!(adf_to_text(&json!("plain")), "plain");
        assert_eq!(adf_to_text(&json!({"type": "doc", "content": []})), "");
        assert_eq!(adf_to_text(&json!({"type": "doc"})), "");
        // Mentions that already carry `@` don't get a second one.
        let m = json!({"type": "paragraph", "content": [{"type": "mention", "attrs": {"text": "@Ada"}}]});
        assert_eq!(adf_to_text(&m), "@Ada");
        let task = json!({"type": "taskList", "content": [
            {"type": "taskItem", "attrs": {"state": "DONE"}, "content": [{"type": "text", "text": "done"}]},
            {"type": "taskItem", "attrs": {"state": "TODO"}, "content": [{"type": "text", "text": "todo"}]}]});
        assert_eq!(adf_to_text(&task), "- [x] done\n- [ ] todo");
        let date = json!({"type": "paragraph", "content": [{"type": "date", "attrs": {"timestamp": "1767225600000"}}]});
        assert_eq!(adf_to_text(&date), "2026-01-01");
        let panel = json!({"type": "panel", "attrs": {"panelType": "warning"}, "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "careful"}]}]});
        assert_eq!(adf_to_text(&panel), "> [warning] careful");
        // A link whose text is the URL is printed once.
        let l = json!({"type": "text", "text": "https://x.y", "marks": [{"type": "link", "attrs": {"href": "https://x.y"}}]});
        assert_eq!(adf_to_text(&l), "https://x.y");
    }

    #[test]
    fn text_becomes_minimal_adf() {
        let d = text_to_adf(
            "First line\r\nsecond line\n\n\nNew para\n```sh\nnpm ci\n\necho \"<x>\"\n```\nafter",
        );
        assert_eq!(d["type"], "doc");
        assert_eq!(d["version"], 1);
        let c = d["content"].as_array().unwrap();
        assert_eq!(c.len(), 4, "{d}");
        assert_eq!(
            c[0],
            json!({"type": "paragraph", "content": [
                {"type": "text", "text": "First line"}, {"type": "hardBreak"}, {"type": "text", "text": "second line"}]})
        );
        assert_eq!(c[1]["content"][0]["text"], "New para");
        assert_eq!(
            c[2],
            json!({"type": "codeBlock", "attrs": {"language": "sh"}, "content": [{"type": "text", "text": "npm ci\n\necho \"<x>\""}]})
        );
        assert_eq!(c[3]["content"][0]["text"], "after");
        assert_eq!(text_to_adf("")["content"], json!([]));
        // An unclosed fence still keeps the code.
        let u = text_to_adf("```\nx");
        assert_eq!(u["content"][0]["type"], "codeBlock");
        assert!(u["content"][0].get("attrs").is_none());
        // Round trip.
        assert_eq!(adf_to_text(&text_to_adf("a\nb\n\nc")), "a\nb\n\nc");
    }

    /// Validation happens before any request: these never reach the network.
    #[test]
    fn bad_input_is_refused_before_any_request() {
        let c = server();
        let http = reqwest::Client::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let run = |input: Value| {
            rt.block_on(Jira { http: &http, c: &c }.run(&input))
                .unwrap_err()
        };
        let e = run(json!({"action": "nuke"}));
        assert!(
            e.contains("Unknown jira action") && e.contains("create_issue"),
            "{e}"
        );
        assert!(run(json!({"action": "get_issue", "key": "../x"})).contains("not an issue key"));
        assert!(run(json!({"action": "search"})).contains("jql"));
        assert!(run(json!({"action": "comment", "key": "X-1"})).contains("body"));
        assert!(
            run(json!({"action": "update_issue", "key": "X-1"})).contains("something to change")
        );
        assert!(run(json!({"action": "api", "path": "https://evil.com/x"})).contains("Refusing"));
        assert!(run(json!({"action": "list_projects", "cursor": "zz"})).contains("cursor"));
    }
}
