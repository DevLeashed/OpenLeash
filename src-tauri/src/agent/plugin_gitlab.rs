//! The GitLab plugin: one `gitlab` tool with an `action` enum.
//!
//! Wired into the harness through `plugin_services.rs` (schema, permissions,
//! dispatch, settings). Off by default; reads run freely, writes ask.
//!
//! Talks to REST v4 at `<base>/api/v4` — gitlab.com or a self-hosted instance,
//! including one served under a sub-path (`https://example.com/gitlab`). Every
//! request, built-in or raw, is joined onto that base through
//! `plugin_services::pin_to_base`, so the token can only ever go to the
//! configured host. Request building and response formatting are pure
//! functions (tested below without a network); `Gl` only sends.

use super::git;
use super::plugin_services::{self, Identity, Reply};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GitlabCfg {
    /// Off unless the user switches it on (Settings → Connectors).
    pub enabled: bool,
    /// Instance URL. Empty = GITLAB_URL, else https://gitlab.com.
    pub base_url: String,
    /// Personal access token (glpat-…, scope `api` or `read_api`). Empty = GITLAB_TOKEN.
    pub token: String,
}

/// Hand-written so a token never reaches a log line or a test failure message.
impl std::fmt::Debug for GitlabCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitlabCfg")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
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
    "list_projects",
    "list_issues",
    "get_issue",
    "list_mrs",
    "get_mr",
    "mr_diff",
    "list_pipelines",
    "get_pipeline",
    "job_log",
    "get_file",
    "search",
];

/// Actions that change something on GitLab (need approval).
/// `api` is classified separately in `is_write`.
pub const WRITES: &[&str] = &[
    "create_issue",
    "update_issue",
    "create_mr",
    "update_mr",
    "comment",
    "retry_pipeline",
];

/// The raw passthrough action (classified by method and path).
const RAW: &str = "api";

const DEFAULT_BASE: &str = "https://gitlab.com";
const SERVICE: &str = "GitLab";
/// Whole-result ceiling, the same as GitHub's diff clip.
const MAX_OUT: usize = 80_000;
/// GitLab's own maximum page size.
const PAGE_MAX: u64 = 100;

const NO_TOKEN: &str = "GitLab isn't connected: add a personal access token (scope `api`, or `read_api` for read-only use) in Settings → Connectors → GitLab, or set the GITLAB_TOKEN environment variable (and GITLAB_URL for a self-hosted instance).";

pub fn schema() -> Value {
    let mut actions: Vec<&str> = READS.iter().chain(WRITES.iter()).copied().collect();
    actions.push(RAW);
    json!({
        "name": "gitlab",
        "description": "Work with GitLab (gitlab.com or a self-hosted instance) through its REST API v4, authenticated with the user's personal access token. `project` is `group/sub/name` or a numeric id; leave it out to use the working directory's git `origin` remote (only when that remote is on the configured GitLab host). Issues and merge requests are addressed by `iid`, the per-project number shown as #12 / !34. Actions:\n\
    - whoami · list_projects (search, membership, limit, page)\n\
    - list_issues (state opened|closed|all, labels, assignee, search, limit, page) · get_issue (iid: description + notes; include_system to see system notes) · create_issue (title, description, labels, assignee_ids) · update_issue (iid, title/description/state_event close|reopen/labels/add_labels/remove_labels/assignee_ids)\n\
    - list_mrs (state opened|closed|merged|locked|all, search, labels, limit, page) · get_mr (iid: description, pipeline, merge status, changed files, notes) · mr_diff (iid: unified diff) · create_mr (title, description, source_branch = current branch, target_branch = project default branch, draft, labels, assignee_ids, remove_source_branch) · update_mr (iid, title/description/state_event close|reopen/target_branch/labels/draft)\n\
    - comment (kind issue|mr, iid, body)\n\
    - list_pipelines (ref, status, limit, page) · get_pipeline (id: status + jobs by stage, failed ones first) · job_log (id = job id, lines = how many trailing lines, default 200) · retry_pipeline (id: retries its failed jobs)\n\
    - get_file (path, ref) · search (query, scope projects|issues|merge_requests|blobs; inside `project` when given, otherwise instance-wide — except blobs, which default to the working directory's project)\n\
    - api (method, path, body): any other REST v4 endpoint, path relative to /api/v4, e.g. `GET /projects/group%2Fname/releases`. It can only reach the configured GitLab host.\n\
    Output: one line per item (#iid issues, !iid MRs, pipeline/job ids, web URLs); long text is clipped. Lists return `limit` items (default 20, max 100) from `page`; when a full page comes back more may be available — ask for page + 1. Project-scoped results start with `[project: … · how it was chosen]` — name that project in your answer, and pass `project` explicitly if the user means a different one. Push your branch with bash (`git push -u origin HEAD`) before create_mr. Changes (create/update/comment/retry, non-GET api) ask the user first unless allowed. A `read_api` token can only read; writes need scope `api` and a project role that allows them.",
        "input_schema": {"type":"object","properties":{
            "action":{"type":"string","enum": actions},
            "project":{"type":"string","description":"group/sub/name or numeric project id (default: the working directory's origin remote, if it is on this GitLab host)"},
            "iid":{"type":"integer","description":"Issue or merge request number within the project (#12 → 12, !34 → 34)"},
            "id":{"type":"integer","description":"get_pipeline / retry_pipeline: pipeline id. job_log: job id"},
            "title":{"type":"string","description":"Issue or MR title"},
            "description":{"type":"string","description":"Markdown description of an issue or MR"},
            "body":{"description":"comment: Markdown text of the note. api: JSON request body (object or JSON string)"},
            "kind":{"type":"string","enum":["issue","mr"],"description":"comment: whether `iid` is an issue or a merge request"},
            "state":{"type":"string","description":"list_issues: opened | closed | all (default opened). list_mrs: opened | closed | merged | locked | all (default opened)"},
            "state_event":{"type":"string","enum":["close","reopen"],"description":"update_issue / update_mr: close or reopen"},
            "labels":{"type":"array","items":{"type":"string"},"description":"Label names (filter for lists; replaces the labels on create/update)"},
            "add_labels":{"type":"array","items":{"type":"string"},"description":"update_issue / update_mr: labels to add"},
            "remove_labels":{"type":"array","items":{"type":"string"},"description":"update_issue / update_mr: labels to remove"},
            "assignee":{"type":"string","description":"list_issues: username, numeric user id, `none` or `any`"},
            "assignee_ids":{"type":"array","items":{"type":"integer"},"description":"create_issue / update_issue / create_mr: user ids to assign"},
            "search":{"type":"string","description":"list_projects / list_issues / list_mrs: text filter"},
            "membership":{"type":"boolean","description":"list_projects: only projects you are a member of (default true)"},
            "include_system":{"type":"boolean","description":"get_issue / get_mr: also show system notes (label changes, pushes…; default false)"},
            "source_branch":{"type":"string","description":"create_mr: branch with the changes (default: current branch)"},
            "target_branch":{"type":"string","description":"create_mr / update_mr: branch to merge into (default: project default branch)"},
            "draft":{"type":"boolean","description":"create_mr / update_mr: mark as draft (adds or removes the `Draft:` title prefix)"},
            "remove_source_branch":{"type":"boolean","description":"create_mr: delete the source branch when merged"},
            "ref":{"type":"string","description":"list_pipelines: branch or tag filter. get_file: branch, tag or commit (default: HEAD of the default branch)"},
            "status":{"type":"string","description":"list_pipelines: created | waiting_for_resource | preparing | pending | running | success | failed | canceled | skipped | manual | scheduled"},
            "lines":{"type":"integer","description":"job_log: how many trailing lines of the log (default 200, max 5000)"},
            "path":{"type":"string","description":"get_file: file path in the repo. api: endpoint path relative to /api/v4, e.g. /projects/group%2Fname/releases"},
            "query":{"type":"string","description":"search: the search text"},
            "scope":{"type":"string","enum":["projects","issues","merge_requests","blobs"],"description":"search: what to search (default issues)"},
            "method":{"type":"string","description":"api: GET | POST | PUT | DELETE (default GET)"},
            "limit":{"type":"integer","description":"Max results (default 20, max 100)"},
            "page":{"type":"integer","description":"Page of results, starting at 1"}},
            "required":["action"]}
    })
}

/// Whether this call changes something on GitLab. Fails closed.
pub fn is_write(input: &Value) -> bool {
    let a = input["action"].as_str().unwrap_or("");
    if a == "api" {
        return !(plugin_services::method_is_read(&input["method"])
            && plugin_services::raw_path_is_relative(input["path"].as_str().unwrap_or("")));
    }
    !READS.contains(&a)
}

// ───────────────────────────── credentials & base ─────────────────────────────

/// Resolved connection details. Debug is hand-written so the token never
/// shows up in a test failure.
struct Creds {
    /// Instance root without `/api/v4`, e.g. `https://example.com/gitlab`.
    base: String,
    /// `<base>/api/v4` — every request is pinned to this.
    api: String,
    token: String,
    /// `settings` or the env var the token came from.
    source: &'static str,
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("base", &self.base)
            .field("api", &self.api)
            .field("token", &"[redacted]")
            .field("source", &self.source)
            .finish()
    }
}

/// Credentials from settings, then the environment. `env` is injected so tests
/// never touch the process environment.
fn creds(cfg: &GitlabCfg, env: &dyn Fn(&str) -> Option<String>) -> Result<Creds, String> {
    let (raw_base, base_src) = plugin_services::setting_or_env(&cfg.base_url, &["GITLAB_URL"], env)
        .unwrap_or_else(|| (DEFAULT_BASE.to_string(), "default"));
    let base = normalize_base(&raw_base).map_err(|e| {
        format!(
            "GitLab: {e} Fix the instance URL {}.",
            match base_src {
                "settings" => "in Settings → Connectors → GitLab",
                _ => "in the GITLAB_URL environment variable (or set one in Settings → Connectors → GitLab)",
            }
        )
    })?;
    let (token, source) = plugin_services::setting_or_env(&cfg.token, &["GITLAB_TOKEN"], env)
        .ok_or_else(|| NO_TOKEN.to_string())?;
    Ok(Creds {
        api: format!("{base}/api/v4"),
        base,
        token,
        source,
    })
}

/// `https://gitlab.example.com/gitlab/` or `…/api/v4` → the instance root
/// `https://gitlab.example.com/gitlab`. A bare host gets `https://`. The raw
/// value is never echoed back: a pasted URL can carry credentials.
fn normalize_base(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    let full = if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    let u = reqwest::Url::parse(&full)
        .map_err(|_| "the instance URL is not a valid URL.".to_string())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none_or(str::is_empty) {
        return Err("the instance URL must be an http(s) URL with a host.".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(
            "the instance URL must not contain a user name or password — put the token in the token field."
                .into(),
        );
    }
    let mut path = u.path().trim_end_matches('/').to_string();
    if path.to_ascii_lowercase().ends_with("/api/v4") {
        path.truncate(path.len() - "/api/v4".len());
        path = path.trim_end_matches('/').to_string();
    }
    Ok(format!("{}{path}", u.origin().ascii_serialization()))
}

/// `gitlab.example.com/gitlab` — the instance as people say it.
fn instance(base: &str) -> String {
    base.split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(base)
        .to_string()
}

// ───────────────────────────── project resolution ─────────────────────────────

/// `group/sub/name` from a git remote URL, but only when the remote is on the
/// configured GitLab host — a GitHub remote, or another GitLab instance, must
/// not be mistaken for a project here (the call would hit the wrong project or
/// 404 confusingly). Supports `https://[user@]host[/sub-path]/group/proj(.git)`,
/// `ssh://git@host:2222/group/proj.git` and scp-like `git@host:group/proj.git`.
/// For http(s) remotes of an instance under a sub-path the sub-path is
/// stripped; ssh remotes never carry it.
pub fn project_from_remote(remote_url: &str, base_url: &str) -> Option<String> {
    let base = reqwest::Url::parse(&normalize_base(base_url).ok()?).ok()?;
    let base_host = base.host_str()?.to_ascii_lowercase();
    let prefix = base.path().trim_matches('/').to_ascii_lowercase();
    let r = remote_url.trim();
    let (host, path, is_http) = if r.contains("://") {
        let u = reqwest::Url::parse(r).ok()?;
        let http = matches!(u.scheme(), "http" | "https");
        if !http && !matches!(u.scheme(), "ssh" | "git+ssh" | "ssh+git") {
            return None;
        }
        (
            u.host_str()?.to_ascii_lowercase(),
            u.path().to_string(),
            http,
        )
    } else {
        let (left, path) = r.split_once(':')?;
        let host = left.rsplit('@').next()?;
        if host.is_empty() || host.contains(['/', '\\']) {
            return None;
        }
        (host.to_ascii_lowercase(), path.to_string(), false)
    };
    if host != base_host {
        return None;
    }
    let mut p = path.trim_matches('/');
    if is_http && !prefix.is_empty() {
        let head = p.get(..prefix.len())?;
        let rest = p.get(prefix.len()..)?;
        if !head.eq_ignore_ascii_case(&prefix) || !rest.starts_with('/') {
            return None;
        }
        p = rest.trim_start_matches('/');
    }
    let p = p.strip_suffix(".git").unwrap_or(p).trim_end_matches('/');
    let segs: Vec<&str> = p.split('/').collect();
    if segs.len() < 2 || segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return None;
    }
    Some(p.to_string())
}

/// The `project` the caller named, if any: a path, a numeric id, or a pasted
/// project URL on this instance.
fn explicit_project(input: &Value, base: &str) -> Result<Option<String>, String> {
    match &input["project"] {
        Value::Number(n) => Ok(Some(n.to_string())),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            if t.contains("://") || t.contains('@') {
                return project_from_remote(t, base).map(Some).ok_or_else(|| {
                    format!(
                        "`project` looks like a URL that is not a project on {}. Pass `group/name` or a numeric id.",
                        instance(base)
                    )
                });
            }
            let t = t.trim_matches('/');
            let t = t.strip_suffix(".git").unwrap_or(t);
            if t.is_empty() || dots_only(t) {
                return Err("`project` must be `group/name` or a numeric id.".into());
            }
            Ok(Some(t.to_string()))
        }
        Value::Null => Ok(None),
        _ => Err("`project` must be `group/name` or a numeric id.".into()),
    }
}

/// The project for a scoped call and how it was chosen. `remote` is called
/// only when no project was named (it runs git).
fn pick_project(
    input: &Value,
    remote: &dyn Fn() -> Option<String>,
    base: &str,
) -> Result<(String, &'static str), String> {
    if let Some(p) = explicit_project(input, base)? {
        return Ok((p, "as requested"));
    }
    // The remote itself is never echoed: an https remote can embed a token.
    match remote() {
        Some(r) => project_from_remote(&r, base)
            .map(|p| (p, "from the working directory's origin remote"))
            .ok_or_else(|| {
                format!(
                    "No `project` given, and the working directory's `origin` remote is not a project on {}. Pass project: \"group/name\" (or its numeric id).",
                    instance(base)
                )
            }),
        None => Err("No `project` given and the working directory has no git `origin` remote. Pass project: \"group/name\" (or its numeric id).".into()),
    }
}

// ───────────────────────────── request building ─────────────────────────────

/// One API request, relative to `<base>/api/v4`.
#[derive(Debug, Clone, PartialEq)]
struct Req {
    method: &'static str,
    path: String,
    body: Option<Value>,
}

fn get(path: String) -> Req {
    Req {
        method: "GET",
        path,
        body: None,
    }
}

fn post(path: String, body: Option<Value>) -> Req {
    Req {
        method: "POST",
        path,
        body,
    }
}

fn put(path: String, body: Value) -> Req {
    Req {
        method: "PUT",
        path,
        body: Some(body),
    }
}

/// One URL path segment. GitLab wants a project path (and a file path) as a
/// single segment with `/` → `%2F`.
fn seg(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// A value that would become a `.`/`..` segment. The URL parser resolves
/// those even percent-encoded (`%2E%2E`), so they are refused up front
/// rather than encoded.
fn dots_only(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '.')
}

fn proj(project: &str) -> String {
    format!("projects/{}", seg(project))
}

/// `path?k=v&…` with every value percent-encoded; empty/absent values dropped.
fn with_query(path: &str, q: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = q
        .iter()
        .filter_map(|(k, v)| {
            v.as_deref()
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k}={}", urlencoding::encode(v)))
        })
        .collect();
    if parts.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parts.join("&"))
    }
}

/// A trimmed, non-empty string parameter.
fn s(input: &Value, k: &str) -> Option<String> {
    input[k]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// Free text (descriptions, note bodies): kept as written, but blank = absent.
fn text(input: &Value, k: &str) -> Option<String> {
    input[k]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .map(String::from)
}

/// An integer given as a number, or as a string like `12`, `#12` or `!12`.
fn int(input: &Value, k: &str) -> Option<u64> {
    match &input[k] {
        Value::Number(n) => n.as_u64(),
        Value::String(t) => t.trim().trim_start_matches(['#', '!']).parse().ok(),
        _ => None,
    }
}

fn need_int(input: &Value, k: &str, action: &str) -> Result<u64, String> {
    int(input, k).ok_or_else(|| format!("{action} needs `{k}` (a number)."))
}

fn limit(input: &Value) -> u64 {
    int(input, "limit").unwrap_or(20).clamp(1, PAGE_MAX)
}

fn page(input: &Value) -> u64 {
    int(input, "page").unwrap_or(1).max(1)
}

/// Labels as GitLab's comma-separated string, from an array or a string.
fn labels(input: &Value, k: &str) -> Option<String> {
    let joined = match &input[k] {
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(","),
        Value::String(t) => t.trim().to_string(),
        _ => return None,
    };
    (!joined.is_empty()).then_some(joined)
}

fn ids(input: &Value, k: &str) -> Option<Vec<u64>> {
    match &input[k] {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()))
                .collect(),
        ),
        Value::Number(n) => n.as_u64().map(|n| vec![n]),
        _ => None,
    }
}

fn list_state(v: Option<String>, mr: bool) -> Result<Option<String>, String> {
    let st = v.unwrap_or_else(|| "opened".into()).to_ascii_lowercase();
    let st = match st.as_str() {
        "open" => "opened".to_string(),
        "close" => "closed".to_string(),
        _ => st,
    };
    let ok: &[&str] = if mr {
        &["opened", "closed", "merged", "locked", "all"]
    } else {
        &["opened", "closed", "all"]
    };
    if !ok.contains(&st.as_str()) {
        return Err(format!("`state` must be one of {}.", ok.join(" | ")));
    }
    // GitLab returns every state when `state` is left out.
    Ok((st != "all").then_some(st))
}

fn state_event(input: &Value) -> Result<Option<String>, String> {
    let Some(v) = s(input, "state_event").or_else(|| s(input, "state")) else {
        return Ok(None);
    };
    match v.to_ascii_lowercase().as_str() {
        "close" | "closed" => Ok(Some("close".into())),
        "reopen" | "reopened" | "open" | "opened" => Ok(Some("reopen".into())),
        _ => Err("`state_event` must be close or reopen.".into()),
    }
}

fn req_user() -> Req {
    get("user".into())
}

fn req_project(project: &str) -> Req {
    get(proj(project))
}

fn req_list_projects(input: &Value) -> Req {
    let membership = input["membership"].as_bool().unwrap_or(true);
    get(with_query(
        "projects",
        &[
            ("membership", Some(membership.to_string())),
            ("simple", Some("true".into())),
            ("order_by", Some("last_activity_at".into())),
            ("search", s(input, "search")),
            ("per_page", Some(limit(input).to_string())),
            ("page", Some(page(input).to_string())),
        ],
    ))
}

fn req_list_issues(project: &str, input: &Value) -> Result<Req, String> {
    let state = list_state(s(input, "state"), false)?;
    let (mut a_id, mut a_user) = (None, None);
    if let Some(a) = s(input, "assignee") {
        if a.eq_ignore_ascii_case("none") {
            a_id = Some("None".to_string());
        } else if a.eq_ignore_ascii_case("any") {
            a_id = Some("Any".to_string());
        } else if a.chars().all(|c| c.is_ascii_digit()) {
            a_id = Some(a);
        } else {
            a_user = Some(a.trim_start_matches('@').to_string());
        }
    }
    Ok(get(with_query(
        &format!("{}/issues", proj(project)),
        &[
            ("state", state),
            ("labels", labels(input, "labels")),
            ("assignee_id", a_id),
            ("assignee_username", a_user),
            ("search", s(input, "search")),
            ("per_page", Some(limit(input).to_string())),
            ("page", Some(page(input).to_string())),
        ],
    )))
}

fn req_issue(project: &str, iid: u64) -> Req {
    get(format!("{}/issues/{iid}", proj(project)))
}

/// Notes oldest first, as a conversation reads. `kind` is `issues` or
/// `merge_requests`.
fn req_notes(project: &str, kind: &str, iid: u64) -> Req {
    get(with_query(
        &format!("{}/{kind}/{iid}/notes", proj(project)),
        &[
            ("sort", Some("asc".into())),
            ("order_by", Some("created_at".into())),
            ("per_page", Some(PAGE_MAX.to_string())),
        ],
    ))
}

fn req_list_mrs(project: &str, input: &Value) -> Result<Req, String> {
    let state = list_state(s(input, "state"), true)?;
    Ok(get(with_query(
        &format!("{}/merge_requests", proj(project)),
        &[
            ("state", state),
            ("labels", labels(input, "labels")),
            ("search", s(input, "search")),
            ("per_page", Some(limit(input).to_string())),
            ("page", Some(page(input).to_string())),
        ],
    )))
}

fn req_mr(project: &str, iid: u64) -> Req {
    get(format!("{}/merge_requests/{iid}", proj(project)))
}

/// `/diffs` (GitLab 15.7+), paginated by file.
fn req_mr_diffs(project: &str, iid: u64, page: u64) -> Req {
    get(with_query(
        &format!("{}/merge_requests/{iid}/diffs", proj(project)),
        &[
            ("per_page", Some(PAGE_MAX.to_string())),
            ("page", Some(page.to_string())),
        ],
    ))
}

/// The older `/changes` endpoint, for instances that predate `/diffs`.
fn req_mr_changes(project: &str, iid: u64) -> Req {
    get(format!("{}/merge_requests/{iid}/changes", proj(project)))
}

const PIPELINE_STATUSES: &[&str] = &[
    "created",
    "waiting_for_resource",
    "preparing",
    "pending",
    "running",
    "success",
    "failed",
    "canceled",
    "skipped",
    "manual",
    "scheduled",
];

fn req_list_pipelines(project: &str, input: &Value) -> Result<Req, String> {
    let status = s(input, "status").map(|v| v.to_ascii_lowercase());
    if let Some(st) = &status {
        if !PIPELINE_STATUSES.contains(&st.as_str()) {
            return Err(format!(
                "`status` must be one of {}.",
                PIPELINE_STATUSES.join(" | ")
            ));
        }
    }
    Ok(get(with_query(
        &format!("{}/pipelines", proj(project)),
        &[
            ("ref", s(input, "ref")),
            ("status", status),
            ("per_page", Some(limit(input).to_string())),
            ("page", Some(page(input).to_string())),
        ],
    )))
}

fn req_pipeline(project: &str, id: u64) -> Req {
    get(format!("{}/pipelines/{id}", proj(project)))
}

fn req_pipeline_jobs(project: &str, id: u64) -> Req {
    get(with_query(
        &format!("{}/pipelines/{id}/jobs", proj(project)),
        &[("per_page", Some(PAGE_MAX.to_string()))],
    ))
}

fn req_job_trace(project: &str, id: u64) -> Req {
    get(format!("{}/jobs/{id}/trace", proj(project)))
}

fn req_file(project: &str, path: &str, rf: &str) -> Result<Req, String> {
    let p = path.trim().trim_start_matches('/');
    if p.is_empty() || dots_only(p) {
        return Err("get_file needs `path` (a file relative to the repo root).".into());
    }
    Ok(get(with_query(
        &format!("{}/repository/files/{}/raw", proj(project), seg(p)),
        &[("ref", Some(rf.to_string()))],
    )))
}

const SEARCH_SCOPES: &[&str] = &["projects", "issues", "merge_requests", "blobs"];

fn search_scope(input: &Value) -> Result<String, String> {
    let sc = s(input, "scope")
        .unwrap_or_else(|| "issues".into())
        .to_ascii_lowercase();
    let sc = match sc.as_str() {
        "mrs" | "mr" | "merge_request" => "merge_requests".to_string(),
        "code" | "blob" | "files" => "blobs".to_string(),
        _ => sc,
    };
    if !SEARCH_SCOPES.contains(&sc.as_str()) {
        return Err(format!(
            "`scope` must be one of {}.",
            SEARCH_SCOPES.join(" | ")
        ));
    }
    Ok(sc)
}

fn req_search(project: Option<&str>, scope: &str, input: &Value) -> Result<Req, String> {
    let q = s(input, "query").ok_or("search needs `query`.")?;
    let path = match project {
        Some(p) if scope != "projects" => format!("{}/search", proj(p)),
        _ => "search".to_string(),
    };
    Ok(get(with_query(
        &path,
        &[
            ("scope", Some(scope.to_string())),
            ("search", Some(q)),
            ("per_page", Some(limit(input).to_string())),
            ("page", Some(page(input).to_string())),
        ],
    )))
}

fn req_create_issue(project: &str, input: &Value) -> Result<Req, String> {
    let title = s(input, "title").ok_or("create_issue needs `title`.")?;
    let mut b = json!({ "title": title });
    if let Some(d) = text(input, "description") {
        b["description"] = json!(d);
    }
    if let Some(l) = labels(input, "labels") {
        b["labels"] = json!(l);
    }
    if let Some(a) = ids(input, "assignee_ids") {
        b["assignee_ids"] = json!(a);
    }
    Ok(post(format!("{}/issues", proj(project)), Some(b)))
}

fn req_update_issue(project: &str, input: &Value) -> Result<Req, String> {
    let iid = need_int(input, "iid", "update_issue")?;
    let mut b = json!({});
    if let Some(t) = s(input, "title") {
        b["title"] = json!(t);
    }
    if let Some(d) = text(input, "description") {
        b["description"] = json!(d);
    }
    if let Some(e) = state_event(input)? {
        b["state_event"] = json!(e);
    }
    for k in ["labels", "add_labels", "remove_labels"] {
        if let Some(l) = labels(input, k) {
            b[k] = json!(l);
        }
    }
    if let Some(a) = ids(input, "assignee_ids") {
        b["assignee_ids"] = json!(a);
    }
    if b.as_object().is_none_or(|o| o.is_empty()) {
        return Err("Nothing to update: pass title, description, state_event, labels, add_labels, remove_labels or assignee_ids.".into());
    }
    Ok(put(format!("{}/issues/{iid}", proj(project)), b))
}

/// GitLab marks drafts by title prefix; these are the prefixes it recognises.
fn strip_draft(title: &str) -> &str {
    let mut t = title.trim_start();
    loop {
        let low = t.to_ascii_lowercase();
        let cut = ["draft:", "[draft]", "(draft)", "wip:", "[wip]"]
            .iter()
            .find(|p| low.starts_with(*p))
            .map(|p| p.len());
        match cut {
            Some(n) => t = t[n..].trim_start(),
            None => return t,
        }
    }
}

fn draft_title(title: &str, draft: bool) -> String {
    let bare = strip_draft(title);
    if draft {
        format!("Draft: {bare}")
    } else {
        bare.to_string()
    }
}

fn req_create_mr(project: &str, input: &Value, source: &str, target: &str) -> Result<Req, String> {
    let title = s(input, "title").ok_or("create_mr needs `title`.")?;
    let title = match input["draft"].as_bool() {
        Some(d) => draft_title(&title, d),
        None => title,
    };
    let mut b = json!({
        "source_branch": source,
        "target_branch": target,
        "title": title,
    });
    if let Some(d) = text(input, "description") {
        b["description"] = json!(d);
    }
    if let Some(l) = labels(input, "labels") {
        b["labels"] = json!(l);
    }
    if let Some(a) = ids(input, "assignee_ids") {
        b["assignee_ids"] = json!(a);
    }
    if let Some(r) = input["remove_source_branch"].as_bool() {
        b["remove_source_branch"] = json!(r);
    }
    Ok(post(format!("{}/merge_requests", proj(project)), Some(b)))
}

/// `current_title` is the MR's title as it is now — needed only to toggle
/// `draft` without a new `title` (the run fetches it in that case).
fn req_update_mr(project: &str, input: &Value, current_title: Option<&str>) -> Result<Req, String> {
    let iid = need_int(input, "iid", "update_mr")?;
    let mut b = json!({});
    let title = s(input, "title");
    match (input["draft"].as_bool(), title) {
        (Some(d), Some(t)) => b["title"] = json!(draft_title(&t, d)),
        (Some(d), None) => {
            let cur = current_title.ok_or("update_mr: pass `title` together with `draft`.")?;
            b["title"] = json!(draft_title(cur, d));
        }
        (None, Some(t)) => b["title"] = json!(t),
        (None, None) => {}
    }
    if let Some(d) = text(input, "description") {
        b["description"] = json!(d);
    }
    if let Some(e) = state_event(input)? {
        b["state_event"] = json!(e);
    }
    if let Some(t) = s(input, "target_branch") {
        b["target_branch"] = json!(t);
    }
    for k in ["labels", "add_labels", "remove_labels"] {
        if let Some(l) = labels(input, k) {
            b[k] = json!(l);
        }
    }
    if b.as_object().is_none_or(|o| o.is_empty()) {
        return Err("Nothing to update: pass title, description, state_event, target_branch, labels, add_labels, remove_labels or draft.".into());
    }
    Ok(put(format!("{}/merge_requests/{iid}", proj(project)), b))
}

/// `issues` or `merge_requests` for `comment`'s `kind`.
fn note_kind(input: &Value) -> Result<&'static str, String> {
    match s(input, "kind")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "issue" | "issues" => Ok("issues"),
        "mr" | "mrs" | "merge_request" | "merge_requests" => Ok("merge_requests"),
        _ => Err("comment needs `kind`: issue or mr.".into()),
    }
}

fn req_comment(project: &str, input: &Value) -> Result<Req, String> {
    let kind = note_kind(input)?;
    let iid = need_int(input, "iid", "comment")?;
    let body = text(input, "body").ok_or("comment needs `body`.")?;
    Ok(post(
        format!("{}/{kind}/{iid}/notes", proj(project)),
        Some(json!({ "body": body })),
    ))
}

fn req_retry(project: &str, id: u64) -> Req {
    post(format!("{}/pipelines/{id}/retry", proj(project)), None)
}

/// The URL for a raw `api` call: always through `pin_to_base`, so the path
/// can never leave the configured host. A leading `/api/v4/` that the model
/// copied from the docs is dropped rather than doubled.
fn raw_url(api: &str, path: &str) -> Result<reqwest::Url, String> {
    let p = path.trim();
    if p.contains("://") || p.starts_with("//") {
        return plugin_services::pin_to_base(api, p);
    }
    let rel = p.trim_start_matches('/');
    let rel = match rel.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("api/v4/") => &rel[7..],
        _ => rel,
    };
    if rel.is_empty() || rel.eq_ignore_ascii_case("api/v4") {
        return Err("api needs `path`, e.g. /projects/group%2Fname/releases.".into());
    }
    plugin_services::pin_to_base(api, rel)
}

// ───────────────────────────── errors ─────────────────────────────

/// GitLab's error text: `message` may be a string, an array or a
/// field → messages object; OAuth-style errors use `error_description`.
fn message_text(m: &Value) -> String {
    match m {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(message_text).collect::<Vec<_>>().join("; "),
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| format!("{k}: {}", message_text(v)))
            .collect::<Vec<_>>()
            .join("; "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn server_message(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) => {
            for k in ["message", "error_description", "error"] {
                let m = message_text(&v[k]);
                if !m.trim().is_empty() {
                    return m;
                }
            }
            plugin_services::clip(body.trim(), 300)
        }
        Err(_) => plugin_services::clip(body.trim(), 300),
    }
}

fn hint(status: u16) -> &'static str {
    match status {
        401 => "The token is missing, expired or revoked — create a personal access token with scope `api` (or `read_api` for reads only) and save it in Settings → Connectors → GitLab (or GITLAB_TOKEN).",
        403 => "The token's role or scopes don't allow this — writes need scope `api` (a `read_api` token can only read) and a project role that permits the change.",
        404 => "The project path or id may be wrong, or the token has no access to it (GitLab answers 404 for projects you can't see). Pass `project` as `group/sub/name` or its numeric id.",
        _ => "",
    }
}

/// A token-free error for a non-2xx reply. `what` uses the URL path only:
/// query strings carry the user's search text and add nothing.
fn error_for(status: u16, method: &str, url_path: &str, body: &str, token: &str) -> String {
    plugin_services::http_error(
        SERVICE,
        status,
        &format!("{method} {url_path}"),
        &server_message(body),
        hint(status),
        &[token],
    )
}

// ───────────────────────────── formatting ─────────────────────────────

fn st<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn num(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => "?".into(),
    }
}

/// `2024-05-01T10:22:33.123Z` → `2024-05-01 10:22`.
fn when(v: &Value) -> String {
    let s = v.as_str().unwrap_or("");
    s.chars().take(16).collect::<String>().replacen('T', " ", 1)
}

fn user(v: &Value) -> String {
    format!("@{}", v["username"].as_str().unwrap_or("?"))
}

fn users(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().map(user).collect()
}

fn label_names(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l.as_str().or_else(|| l["name"].as_str()))
        .map(String::from)
        .collect()
}

/// When a full page came back there may be more.
fn more(count: usize, limit: u64, page: u64) -> String {
    if count as u64 >= limit {
        format!(
            "\n(more may be available — pass page: {} for the next {limit})",
            page + 1
        )
    } else {
        String::new()
    }
}

fn list_out(v: &Value, input: &Value, row: fn(&Value) -> String, empty: &str) -> String {
    let items = v.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return if page(input) > 1 {
            format!("{empty} (page {})", page(input))
        } else {
            empty.to_string()
        };
    }
    let rows: Vec<String> = items.iter().map(row).collect();
    format!(
        "{}{}",
        rows.join("\n"),
        more(items.len(), limit(input), page(input))
    )
}

fn whoami_line(u: &Value, inst: &str) -> String {
    let name = st(u, "name");
    format!(
        "Signed in to {inst} as {}{}{}",
        user(u),
        if name.is_empty() {
            String::new()
        } else {
            format!(" ({name})")
        },
        if u["is_admin"] == true {
            " · instance admin"
        } else {
            ""
        }
    )
}

fn identity_line(u: &Value, inst: &str) -> String {
    let name = st(u, "name");
    if name.is_empty() {
        format!("{} · {inst}", user(u))
    } else {
        format!("{name} {} · {inst}", user(u))
    }
}

fn project_row(p: &Value) -> String {
    let desc = st(p, "description").lines().next().unwrap_or("").trim();
    format!(
        "{} · id {}{}{}{} · {}{}",
        p["path_with_namespace"]
            .as_str()
            .or(p["name_with_namespace"].as_str())
            .unwrap_or("?"),
        num(&p["id"]),
        p["visibility"]
            .as_str()
            .map(|v| format!(" · {v}"))
            .unwrap_or_default(),
        p["default_branch"]
            .as_str()
            .map(|b| format!(" · default {b}"))
            .unwrap_or_default(),
        if p["archived"] == true {
            " · ARCHIVED"
        } else {
            ""
        },
        st(p, "web_url"),
        if desc.is_empty() {
            String::new()
        } else {
            format!(" — {}", plugin_services::clip(desc, 120))
        }
    )
}

fn issue_row(i: &Value) -> String {
    let l = label_names(&i["labels"]);
    let a = users(&i["assignees"]);
    format!(
        "#{} {} [{}]{} · {}{} · {} comments · {}",
        num(&i["iid"]),
        st(i, "title"),
        st(i, "state"),
        if l.is_empty() {
            String::new()
        } else {
            format!(" {{{}}}", l.join(", "))
        },
        user(&i["author"]),
        if a.is_empty() {
            String::new()
        } else {
            format!(" → {}", a.join(", "))
        },
        i["user_notes_count"].as_u64().unwrap_or(0),
        st(i, "web_url")
    )
}

fn is_draft(m: &Value) -> bool {
    m["draft"] == true || m["work_in_progress"] == true
}

fn mr_row(m: &Value) -> String {
    format!(
        "!{} {} [{}{}] {} → {} · {} · {}",
        num(&m["iid"]),
        st(m, "title"),
        st(m, "state"),
        if is_draft(m) { ", draft" } else { "" },
        m["source_branch"].as_str().unwrap_or("?"),
        m["target_branch"].as_str().unwrap_or("?"),
        user(&m["author"]),
        st(m, "web_url")
    )
}

fn sha8(v: &Value) -> String {
    v.as_str().unwrap_or("").chars().take(8).collect()
}

fn pipeline_row(p: &Value) -> String {
    format!(
        "#{} [{}] {} · {} · {} · {} · {}",
        num(&p["id"]),
        st(p, "status"),
        p["ref"].as_str().unwrap_or("?"),
        sha8(&p["sha"]),
        st(p, "source"),
        when(&p["created_at"]),
        st(p, "web_url")
    )
}

/// Seconds (GitLab sends a float or null) → `42.1s` / `3m 05s`.
fn fmt_dur(v: &Value) -> String {
    let Some(secs) = v.as_f64() else {
        return "-".into();
    };
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let t = secs.round() as u64;
        format!("{}m {:02}s", t / 60, t % 60)
    }
}

fn fmt_notes(notes: &Value, include_system: bool) -> String {
    let all = notes.as_array().cloned().unwrap_or_default();
    let shown: Vec<&Value> = all
        .iter()
        .filter(|n| include_system || n["system"] != true)
        .collect();
    let hidden = all.len() - shown.len();
    let mut out = String::new();
    if !shown.is_empty() {
        out.push_str(&format!("\nNotes ({}):\n", shown.len()));
        for n in &shown {
            let pos = &n["position"];
            let at = match pos["new_path"].as_str().or(pos["old_path"].as_str()) {
                Some(path) => format!(
                    " on {path}:{}",
                    pos["new_line"]
                        .as_u64()
                        .or(pos["old_line"].as_u64())
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "?".into())
                ),
                None => String::new(),
            };
            out.push_str(&format!(
                "── {} · {}{at}{}{}\n{}\n",
                user(&n["author"]),
                when(&n["created_at"]),
                if n["system"] == true {
                    " · system"
                } else {
                    ""
                },
                if n["resolved"] == true {
                    " · resolved"
                } else {
                    ""
                },
                plugin_services::clip(st(n, "body"), 4000)
            ));
        }
    }
    if hidden > 0 {
        out.push_str(&format!(
            "({hidden} system note{} hidden — pass include_system: true to see them.)\n",
            if hidden == 1 { "" } else { "s" }
        ));
    }
    if all.len() as u64 >= PAGE_MAX {
        out.push_str("(Only the first 100 notes are shown.)\n");
    }
    out
}

fn fmt_issue(i: &Value, notes: &Value, include_system: bool) -> String {
    let mut meta = vec![
        format!("created {}", when(&i["created_at"])),
        format!("updated {}", when(&i["updated_at"])),
    ];
    if let Some(m) = i["milestone"]["title"].as_str() {
        meta.push(format!("milestone {m}"));
    }
    if let Some(d) = i["due_date"].as_str() {
        meta.push(format!("due {d}"));
    }
    if i["confidential"] == true {
        meta.push("confidential".into());
    }
    let desc = st(i, "description");
    format!(
        "{}\n{}\n\n{}\n{}",
        issue_row(i),
        meta.join(" · "),
        if desc.trim().is_empty() {
            "(no description)".to_string()
        } else {
            plugin_services::clip(desc, 20_000)
        },
        fmt_notes(notes, include_system)
    )
}

/// `(+added, −removed)` lines in one file's hunk text. GitLab's `diff` field
/// starts at the first `@@`, so there are no `---`/`+++` headers to skip.
fn diff_counts(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(a, d), l| {
        if l.starts_with('+') {
            (a + 1, d)
        } else if l.starts_with('-') {
            (a, d + 1)
        } else {
            (a, d)
        }
    })
}

fn change_line(d: &Value) -> String {
    let (old, new) = (st(d, "old_path"), st(d, "new_path"));
    let (a, r) = diff_counts(st(d, "diff"));
    let (mark, name) = if d["new_file"] == true {
        ("A", new.to_string())
    } else if d["deleted_file"] == true {
        ("D", old.to_string())
    } else if d["renamed_file"] == true {
        ("R", format!("{old} → {new}"))
    } else {
        ("M", new.to_string())
    };
    let big = if st(d, "diff").is_empty() && (d["too_large"] == true || d["collapsed"] == true) {
        " (diff too large to show)"
    } else {
        ""
    };
    format!("  {mark} {name} (+{a} −{r}){big}")
}

fn fmt_mr(
    m: &Value,
    diffs: &[Value],
    complete: bool,
    notes: &Value,
    include_system: bool,
) -> String {
    let pipe = &m["head_pipeline"];
    let pipe = if pipe.is_object() {
        pipe
    } else {
        &m["pipeline"]
    };
    let pipeline = if pipe.is_object() {
        format!("{} (#{})", st(pipe, "status"), num(&pipe["id"]))
    } else {
        "none".to_string()
    };
    let merge = m["detailed_merge_status"]
        .as_str()
        .or(m["merge_status"].as_str())
        .unwrap_or("unknown");
    let mut out =
        format!(
        "{}\npipeline: {pipeline} · merge status: {merge}{} · {} files changed · created {}{}\n",
        mr_row(m),
        if m["has_conflicts"] == true {
            " · HAS CONFLICTS"
        } else {
            ""
        },
        m["changes_count"].as_str().map(String::from).unwrap_or_else(|| diffs.len().to_string()),
        when(&m["created_at"]),
        m["merged_at"]
            .as_str()
            .map(|_| format!(" · merged {}", when(&m["merged_at"])))
            .unwrap_or_default()
    );
    let mut people = vec![];
    let a = users(&m["assignees"]);
    if !a.is_empty() {
        people.push(format!("assignees: {}", a.join(", ")));
    }
    let r = users(&m["reviewers"]);
    if !r.is_empty() {
        people.push(format!("reviewers: {}", r.join(", ")));
    }
    let l = label_names(&m["labels"]);
    if !l.is_empty() {
        people.push(format!("labels: {}", l.join(", ")));
    }
    if !people.is_empty() {
        out.push_str(&people.join(" · "));
        out.push('\n');
    }
    let desc = st(m, "description");
    out.push('\n');
    out.push_str(&if desc.trim().is_empty() {
        "(no description)".to_string()
    } else {
        plugin_services::clip(desc, 15_000)
    });
    out.push_str(&format!(
        "\n\nChanges ({} files{}):\n",
        diffs.len(),
        if complete { "" } else { ", first page only" }
    ));
    for d in diffs {
        out.push_str(&change_line(d));
        out.push('\n');
    }
    out.push_str(&fmt_notes(notes, include_system));
    out.push_str("\nUse mr_diff for the full diff.");
    out
}

/// A unified diff from GitLab's per-file diff entries, in the shape
/// `git diff` prints, so it reads (and applies) like one.
fn assemble_diff(diffs: &[Value]) -> String {
    let mut out = String::new();
    for d in diffs {
        let (old, new) = (st(d, "old_path"), st(d, "new_path"));
        out.push_str(&format!("diff --git a/{old} b/{new}\n"));
        if d["new_file"] == true {
            out.push_str(&format!("new file mode {}\n", st(d, "b_mode")));
        } else if d["deleted_file"] == true {
            out.push_str(&format!("deleted file mode {}\n", st(d, "a_mode")));
        } else if d["renamed_file"] == true {
            out.push_str(&format!("rename from {old}\nrename to {new}\n"));
        }
        let body = st(d, "diff");
        if body.is_empty() {
            if d["too_large"] == true || d["collapsed"] == true {
                out.push_str("(diff too large to show — use get_file for this path)\n");
            }
            continue;
        }
        out.push_str(&format!(
            "--- {}\n+++ {}\n",
            if d["new_file"] == true {
                "/dev/null".to_string()
            } else {
                format!("a/{old}")
            },
            if d["deleted_file"] == true {
                "/dev/null".to_string()
            } else {
                format!("b/{new}")
            }
        ));
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn fmt_pipeline(p: &Value, jobs: &Value) -> String {
    let mut out = format!(
        "Pipeline #{} [{}] {} · {} · {} · {} · {}\n",
        num(&p["id"]),
        st(p, "status"),
        p["ref"].as_str().unwrap_or("?"),
        sha8(&p["sha"]),
        st(p, "source"),
        fmt_dur(&p["duration"]),
        st(p, "web_url")
    );
    let mut list = jobs.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        out.push_str("No jobs.");
        return out;
    }
    // The API lists newest first; by id ascending is roughly stage order.
    list.sort_by_key(|j| j["id"].as_u64().unwrap_or(0));
    let failed: Vec<&Value> = list.iter().filter(|j| j["status"] == "failed").collect();
    if !failed.is_empty() {
        out.push_str(&format!("\nFAILED jobs ({}):\n", failed.len()));
        for j in &failed {
            out.push_str(&format!(
                "  ✗ {} {} [{}] {}{} · {} · {}\n",
                num(&j["id"]),
                st(j, "name"),
                st(j, "stage"),
                j["failure_reason"].as_str().unwrap_or("failed"),
                if j["allow_failure"] == true {
                    " (allowed to fail)"
                } else {
                    ""
                },
                fmt_dur(&j["duration"]),
                st(j, "web_url")
            ));
        }
    }
    let mut stages: Vec<String> = vec![];
    for j in &list {
        let sname = st(j, "stage").to_string();
        if !stages.contains(&sname) {
            stages.push(sname);
        }
    }
    out.push_str("\nJobs by stage:\n");
    for sname in &stages {
        out.push_str(&format!("  {sname}:\n"));
        for j in list.iter().filter(|j| st(j, "stage") == sname) {
            let status = st(j, "status");
            let mark = match status {
                "failed" => "✗",
                "success" => "✓",
                _ => "·",
            };
            out.push_str(&format!(
                "    {mark} {} {} [{}] · {}{}\n",
                num(&j["id"]),
                st(j, "name"),
                if status == "failed" { "FAILED" } else { status },
                fmt_dur(&j["duration"]),
                if j["allow_failure"] == true {
                    " (allowed to fail)"
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(f) = failed.first() {
        out.push_str(&format!(
            "\nUse job_log with id: {} for the failed job's log.",
            num(&f["id"])
        ));
    }
    out
}

/// A CI log as text: ANSI colours and GitLab's collapsible-section markers
/// removed, and `\r`-overwritten progress lines reduced to what the terminal
/// would finally show. Then the last `lines` lines.
fn clean_trace(raw: &str, lines: usize) -> (String, usize) {
    let mut t = raw.to_string();
    if let Ok(re) = regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]") {
        t = re.replace_all(&t, "").into_owned();
    }
    if let Ok(re) =
        regex::Regex::new(r"section_(?:start|end):\d+:[A-Za-z0-9_.\-]+(?:\[[^\]\r\n]*\])?\r?")
    {
        t = re.replace_all(&t, "").into_owned();
    }
    let all: Vec<&str> = t
        .lines()
        .map(|l| l.split('\r').rfind(|s| !s.is_empty()).unwrap_or(""))
        .collect();
    let total = all.len();
    let tail = plugin_services::tail(&all.join("\n"), lines);
    (tail, total)
}

fn search_row(scope: &str, i: &Value) -> String {
    match scope {
        "projects" => project_row(i),
        "merge_requests" => mr_row(i),
        "blobs" => {
            let data: String = st(i, "data")
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{}:{} @{} (project {})\n{}",
                i["path"].as_str().or(i["filename"].as_str()).unwrap_or("?"),
                num(&i["startline"]),
                st(i, "ref"),
                num(&i["project_id"]),
                plugin_services::clip(&data, 800)
            )
        }
        _ => issue_row(i),
    }
}

fn fmt_search(scope: &str, v: &Value, input: &Value) -> String {
    let items = v.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return format!("No {} found.", scope.replace('_', " "));
    }
    let rows: Vec<String> = items.iter().map(|i| search_row(scope, i)).collect();
    format!(
        "{} {} (page {}):\n{}{}",
        items.len(),
        scope.replace('_', " "),
        page(input),
        rows.join("\n"),
        more(items.len(), limit(input), page(input))
    )
}

/// The web URL of a note, when the project is a path (a numeric id has no
/// stable web route).
fn note_url(base: &str, project: &str, kind: &str, iid: u64, note_id: &Value) -> String {
    if project.chars().all(|c| c.is_ascii_digit()) {
        return String::new();
    }
    format!(" {base}/{project}/-/{kind}/{iid}#note_{}", num(note_id))
}

fn looks_binary(s: &str) -> bool {
    s.chars().take(8000).any(|c| c == '\0')
}

// ───────────────────────────── network ─────────────────────────────

struct Gl<'a> {
    http: &'a reqwest::Client,
    c: &'a Creds,
}

impl Gl<'_> {
    async fn send_url(
        &self,
        method: &str,
        url: reqwest::Url,
        body: Option<&Value>,
    ) -> Result<(Reply, String), String> {
        let m = reqwest::Method::from_bytes(method.trim().to_ascii_uppercase().as_bytes())
            .map_err(|_| format!("`{method}` is not an HTTP method."))?;
        let what_path = url.path().to_string();
        let mut rq = self
            .http
            .request(m, url)
            .header("PRIVATE-TOKEN", &self.c.token)
            .header("user-agent", "OpenLeash/0.1")
            .header("accept", "application/json");
        if let Some(b) = body {
            rq = rq.json(b);
        }
        let mut r = plugin_services::send(SERVICE, rq, &[&self.c.token]).await?;
        // Text endpoints (files, traces and raw API) bypass JSON parsing. Remove
        // echoes before any cleaner, tail or clip can split the credential.
        r.body = plugin_services::redact(&r.body, &[&self.c.token]);
        Ok((r, what_path))
    }

    async fn send(&self, rq: &Req) -> Result<(Reply, String), String> {
        let url = plugin_services::pin_to_base(&self.c.api, &rq.path)?;
        self.send_url(rq.method, url, rq.body.as_ref()).await
    }

    /// The body of a 2xx reply, else a token-free error with a hint.
    async fn ok(&self, rq: &Req) -> Result<String, String> {
        let (r, path) = self.send(rq).await?;
        if !(200..300).contains(&r.status) {
            return Err(error_for(
                r.status,
                rq.method,
                &path,
                &r.body,
                &self.c.token,
            ));
        }
        Ok(r.body)
    }

    async fn json(&self, rq: &Req) -> Result<Value, String> {
        let body = self.ok(rq).await?;
        parse_json(&body, &self.c.token)
    }

    /// Diff entries for an MR, up to `max_pages` pages of 100 files, and
    /// whether that was all of them. Falls back to `/changes` on instances
    /// older than 15.7, which answer 404 for `/diffs`.
    async fn mr_diffs(
        &self,
        project: &str,
        iid: u64,
        max_pages: u64,
    ) -> Result<(Vec<Value>, bool), String> {
        let mut all = vec![];
        for pg in 1..=max_pages {
            let rq = req_mr_diffs(project, iid, pg);
            let (r, path) = self.send(&rq).await?;
            if r.status == 404 && pg == 1 {
                let v = self.json(&req_mr_changes(project, iid)).await?;
                return Ok((
                    v["changes"].as_array().cloned().unwrap_or_default(),
                    v["overflow"] != true,
                ));
            }
            if !(200..300).contains(&r.status) {
                return Err(error_for(r.status, "GET", &path, &r.body, &self.c.token));
            }
            let batch = parse_json(&r.body, &self.c.token)?
                .as_array()
                .cloned()
                .unwrap_or_default();
            let n = batch.len() as u64;
            all.extend(batch);
            if n < PAGE_MAX {
                return Ok((all, true));
            }
        }
        Ok((all, false))
    }

    async fn act(
        &self,
        action: &str,
        input: &Value,
        project: Option<&str>,
        cwd: &str,
    ) -> Result<String, String> {
        let pr = || project.ok_or_else(|| format!("{action} needs a `project`."));
        let include_system = input["include_system"].as_bool().unwrap_or(false);
        match action {
            "whoami" => {
                let u = self.json(&req_user()).await?;
                Ok(whoami_line(&u, &instance(&self.c.base)))
            }
            "list_projects" => {
                let v = self.json(&req_list_projects(input)).await?;
                Ok(list_out(&v, input, project_row, "No projects."))
            }
            "list_issues" => {
                let v = self.json(&req_list_issues(pr()?, input)?).await?;
                Ok(list_out(&v, input, issue_row, "No issues."))
            }
            "get_issue" => {
                let p = pr()?;
                let iid = need_int(input, "iid", action)?;
                let i = self.json(&req_issue(p, iid)).await?;
                let n = self.json(&req_notes(p, "issues", iid)).await?;
                Ok(fmt_issue(&i, &n, include_system))
            }
            "list_mrs" => {
                let v = self.json(&req_list_mrs(pr()?, input)?).await?;
                Ok(list_out(&v, input, mr_row, "No merge requests."))
            }
            "get_mr" => {
                let p = pr()?;
                let iid = need_int(input, "iid", action)?;
                let m = self.json(&req_mr(p, iid)).await?;
                let (diffs, complete) = self.mr_diffs(p, iid, 1).await?;
                let n = self.json(&req_notes(p, "merge_requests", iid)).await?;
                Ok(fmt_mr(&m, &diffs, complete, &n, include_system))
            }
            "mr_diff" => {
                let p = pr()?;
                let iid = need_int(input, "iid", action)?;
                let (diffs, complete) = self.mr_diffs(p, iid, 10).await?;
                if diffs.is_empty() {
                    return Ok(format!("!{iid} has no changes."));
                }
                Ok(format!(
                    "!{iid} · {} files{}\n{}",
                    diffs.len(),
                    if complete {
                        ""
                    } else {
                        " (more files exist; only the first 1000 are shown)"
                    },
                    plugin_services::clip(&assemble_diff(&diffs), MAX_OUT)
                ))
            }
            "list_pipelines" => {
                let v = self.json(&req_list_pipelines(pr()?, input)?).await?;
                Ok(list_out(&v, input, pipeline_row, "No pipelines."))
            }
            "get_pipeline" => {
                let p = pr()?;
                let id = need_int(input, "id", action)?;
                let pl = self.json(&req_pipeline(p, id)).await?;
                let jobs = self.json(&req_pipeline_jobs(p, id)).await?;
                Ok(fmt_pipeline(&pl, &jobs))
            }
            "job_log" => {
                let p = pr()?;
                let id = int(input, "id")
                    .or_else(|| int(input, "job_id"))
                    .ok_or("job_log needs `id` (the job id — see get_pipeline).")?;
                let lines = int(input, "lines").unwrap_or(200).clamp(1, 5000) as usize;
                let raw = self.ok(&req_job_trace(p, id)).await?;
                let (log, total) = clean_trace(&raw, lines);
                if log.trim().is_empty() {
                    return Ok(format!("Job {id} has no log yet."));
                }
                Ok(format!(
                    "── job {id} · last {} of {total} lines ──\n{}",
                    lines.min(total),
                    plugin_services::clip(&log, MAX_OUT)
                ))
            }
            "get_file" => {
                let p = pr()?;
                let path = s(input, "path").ok_or("get_file needs `path`.")?;
                let rf = s(input, "ref").unwrap_or_else(|| "HEAD".into());
                let body = self.ok(&req_file(p, &path, &rf)?).await.map_err(|e| {
                    if e.contains(" 404 ") {
                        format!("{e} (Or `path`/`ref` doesn't exist — paths are relative to the repo root.)")
                    } else {
                        e
                    }
                })?;
                if looks_binary(&body) {
                    return Ok(format!("{path} @ {rf} is a binary file."));
                }
                Ok(format!(
                    "{path} @ {rf}\n{}",
                    plugin_services::clip(&body, MAX_OUT)
                ))
            }
            "search" => {
                let scope = search_scope(input)?;
                let v = self.json(&req_search(project, &scope, input)?).await?;
                Ok(fmt_search(&scope, &v, input))
            }
            "create_issue" => {
                let v = self.json(&req_create_issue(pr()?, input)?).await?;
                Ok(format!(
                    "Created issue #{} {}",
                    num(&v["iid"]),
                    st(&v, "web_url")
                ))
            }
            "update_issue" => {
                let v = self.json(&req_update_issue(pr()?, input)?).await?;
                Ok(format!("Updated {}", issue_row(&v)))
            }
            "create_mr" => {
                let p = pr()?;
                let source = match s(input, "source_branch") {
                    Some(b) => b,
                    None => git::current_branch(cwd),
                };
                if source.is_empty() || source == "HEAD" {
                    return Err("Couldn't tell the current branch; pass `source_branch`.".into());
                }
                let target = match s(input, "target_branch") {
                    Some(t) => t,
                    None => self.json(&req_project(p)).await?["default_branch"]
                        .as_str()
                        .unwrap_or("main")
                        .to_string(),
                };
                let v = self
                    .json(&req_create_mr(p, input, &source, &target)?)
                    .await
                    .map_err(|e| {
                        if e.contains(" 409 ") {
                            format!("{e} (An open MR for `{source}` already exists — use list_mrs / update_mr.)")
                        } else if e.contains("branch") || e.contains(" 400 ") || e.contains(" 422 ") {
                            format!("{e}\nIs the branch pushed? Run `git push -u origin HEAD` first.")
                        } else {
                            e
                        }
                    })?;
                Ok(format!(
                    "Opened MR !{} ({source} → {target}) {}",
                    num(&v["iid"]),
                    st(&v, "web_url")
                ))
            }
            "update_mr" => {
                let p = pr()?;
                let current = if input["draft"].is_boolean() && s(input, "title").is_none() {
                    let iid = need_int(input, "iid", action)?;
                    let m = self.json(&req_mr(p, iid)).await?;
                    Some(st(&m, "title").to_string())
                } else {
                    None
                };
                let v = self
                    .json(&req_update_mr(p, input, current.as_deref())?)
                    .await?;
                Ok(format!("Updated {}", mr_row(&v)))
            }
            "comment" => {
                let p = pr()?;
                let rq = req_comment(p, input)?;
                let kind = note_kind(input)?;
                let iid = need_int(input, "iid", action)?;
                let v = self.json(&rq).await?;
                Ok(format!(
                    "Commented on {}{iid} (note {}).{}",
                    if kind == "issues" { "#" } else { "!" },
                    num(&v["id"]),
                    note_url(&self.c.base, p, kind, iid, &v["id"])
                ))
            }
            "retry_pipeline" => {
                let id = need_int(input, "id", action)?;
                let v = self.json(&req_retry(pr()?, id)).await?;
                Ok(format!(
                    "Retrying the failed jobs of pipeline #{}: now [{}] {}",
                    num(&v["id"]),
                    st(&v, "status"),
                    st(&v, "web_url")
                ))
            }
            "api" => {
                let path = s(input, "path")
                    .ok_or("api needs `path`, e.g. /projects/group%2Fname/releases.")?;
                let method = input["method"]
                    .as_str()
                    .unwrap_or("GET")
                    .trim()
                    .to_ascii_uppercase();
                let body = match &input["body"] {
                    Value::Null => None,
                    Value::String(t) if t.trim().is_empty() => None,
                    Value::String(t) => Some(
                        serde_json::from_str::<Value>(t)
                            .map_err(|e| format!("`body` isn't valid JSON: {e}"))?,
                    ),
                    v => Some(v.clone()),
                };
                let url = raw_url(&self.c.api, &path)?;
                let (r, p) = self.send_url(&method, url, body.as_ref()).await?;
                if !(200..300).contains(&r.status) {
                    return Err(error_for(r.status, &method, &p, &r.body, &self.c.token));
                }
                let shown = match parse_json(&r.body, &self.c.token) {
                    Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_default(),
                    Err(_) => r.body.clone(),
                };
                Ok(format!(
                    "HTTP {}\n{}",
                    r.status,
                    plugin_services::clip(&shown, 60_000)
                ))
            }
            other => Err(format!("Unknown gitlab action `{other}`.")),
        }
    }
}

/// Work on decoded strings/keys so JSON escapes cannot hide an echoed token.
fn redact_value(value: Value, token: &str) -> Value {
    match value {
        Value::String(s) => Value::String(plugin_services::redact(&s, &[token])),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| redact_value(v, token)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(k, v)| {
                    (
                        plugin_services::redact(&k, &[token]),
                        redact_value(v, token),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

fn parse_json(body: &str, token: &str) -> Result<Value, String> {
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(body)
        .map(|v| redact_value(v, token))
        .map_err(|e| {
            plugin_services::redact(
                &format!(
                    "GitLab sent a response that isn't JSON ({e}): {}",
                    plugin_services::clip(&plugin_services::redact(body.trim(), &[token]), 200)
                ),
                &[token],
            )
        })
}

/// Which project, if any, an action is about. `Ok(None)` = instance-wide.
fn project_for(
    action: &str,
    input: &Value,
    remote: &dyn Fn() -> Option<String>,
    base: &str,
) -> Result<Option<(String, &'static str)>, String> {
    match action {
        "whoami" | "list_projects" | "api" => Ok(None),
        "search" => {
            let scope = search_scope(input)?;
            if scope == "projects" {
                return Ok(None);
            }
            if let Some(p) = explicit_project(input, base)? {
                return Ok(Some((p, "as requested")));
            }
            // Code search instance-wide needs advanced search (and is rarely
            // what "search the code" means), so blobs default to this repo.
            if scope == "blobs" {
                return Ok(pick_project(input, remote, base).ok());
            }
            Ok(None)
        }
        _ => pick_project(input, remote, base).map(Some),
    }
}

pub async fn run(
    http: &reqwest::Client,
    cfg: &GitlabCfg,
    input: &Value,
    cwd: &str,
) -> Result<String, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let result = async {
        let action = input["action"].as_str().unwrap_or("").trim();
        if !READS.contains(&action) && !WRITES.contains(&action) && action != RAW {
            return Err(format!(
                "Unknown gitlab action `{action}`. Use one of: {}, {RAW}.",
                READS
                    .iter()
                    .chain(WRITES.iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let remote = || git::remote_url(cwd);
        let project = project_for(action, input, &remote, &c.base)?;
        let gl = Gl { http, c: &c };
        let out = gl
            .act(
                action,
                input,
                project.as_ref().map(|(p, _)| p.as_str()),
                cwd,
            )
            .await?;
        let out = match &project {
            Some((p, how)) => format!("[project: {p} · {how}]\n{out}"),
            None => out,
        };
        Ok(out)
    }
    .await;
    result
        .map(|out| plugin_services::clip(&plugin_services::redact(&out, &[&c.token]), MAX_OUT))
        .map_err(|e| plugin_services::redact(&e, &[&c.token]))
}

pub async fn whoami(http: &reqwest::Client, cfg: &GitlabCfg) -> Result<Identity, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let gl = Gl { http, c: &c };
    let u = gl
        .json(&req_user())
        .await
        .map_err(|e| plugin_services::redact(&e, &[&c.token]))?;
    Ok(Identity {
        identity: plugin_services::redact(&identity_line(&u, &instance(&c.base)), &[&c.token]),
        source: c.source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "dummy-gitlab-token-0123456789";
    const BASE: &str = "https://gitlab.example.com";

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn cfg(base: &str, token: &str) -> GitlabCfg {
        GitlabCfg {
            enabled: true,
            base_url: base.into(),
            token: token.into(),
        }
    }

    // ── classification & schema ──

    #[test]
    fn every_action_is_classified() {
        for a in READS {
            assert!(!is_write(&json!({"action": a})), "{a} is a read");
        }
        for a in WRITES {
            assert!(is_write(&json!({"action": a})), "{a} is a write");
        }
        assert!(is_write(&json!({"action": "delete_project"})));
        assert!(is_write(&json!({"action": ""})));
        assert!(is_write(&json!({})));
        assert!(is_write(&json!({"action": 3})));
    }

    #[test]
    fn raw_api_is_a_read_only_for_relative_gets() {
        assert!(!is_write(&json!({"action": "api", "path": "/projects"})));
        assert!(!is_write(
            &json!({"action": "api", "method": "get", "path": "projects/1/releases"})
        ));
        for m in [
            "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "BREW", "",
        ] {
            assert!(
                is_write(&json!({"action": "api", "method": m, "path": "/projects"})),
                "{m}"
            );
        }
        assert!(is_write(
            &json!({"action": "api", "method": 1, "path": "/x"})
        ));
        for p in [
            "//evil.com/x",
            "https://evil.com/api/v4/user",
            "https://gitlab.example.com@evil.com/x",
            "https://gitlab.example.com.evil.com/x",
            "\\\\evil.com\\x",
            "/\\evil.com",
            "%2f%2fevil.com",
            "/x\r\nHost: evil.com",
            "",
        ] {
            assert!(
                is_write(&json!({"action": "api", "path": p})),
                "{p:?} must be a write"
            );
        }
        assert!(is_write(&json!({"action": "api"})));
    }

    #[test]
    fn schema_enum_matches_reads_and_writes() {
        let sc = schema();
        assert_eq!(sc["name"], "gitlab");
        let en: Vec<String> = sc["input_schema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        for a in &en {
            assert!(
                READS.contains(&a.as_str()) || WRITES.contains(&a.as_str()) || a == RAW,
                "{a} unclassified"
            );
        }
        for a in READS.iter().chain(WRITES.iter()) {
            assert!(en.iter().any(|e| e == a), "{a} missing from enum");
        }
        assert!(en.iter().any(|e| e == RAW));
        assert_eq!(sc["input_schema"]["required"], json!(["action"]));
        let desc = sc["description"].as_str().unwrap();
        for a in &en {
            assert!(desc.contains(a.as_str()), "description must mention {a}");
        }
        for (k, v) in sc["input_schema"]["properties"].as_object().unwrap() {
            if k != "action" {
                assert!(v["description"].is_string(), "{k} undocumented");
            }
        }
    }

    // ── credentials ──

    #[test]
    fn settings_beat_env_and_defaults_apply() {
        let env = |k: &str| match k {
            "GITLAB_TOKEN" => Some("glpat-fromenv-000000".to_string()),
            "GITLAB_URL" => Some("https://env.example.org/".to_string()),
            _ => None,
        };
        let c = creds(&cfg("https://set.example.com", TOKEN), &env).unwrap();
        assert_eq!(c.base, "https://set.example.com");
        assert_eq!(c.api, "https://set.example.com/api/v4");
        assert_eq!(c.token, TOKEN);
        assert_eq!(c.source, "settings");
        let c = creds(&cfg("", ""), &env).unwrap();
        assert_eq!(c.base, "https://env.example.org");
        assert_eq!(c.token, "glpat-fromenv-000000");
        assert_eq!(c.source, "GITLAB_TOKEN");
        let only_token = |k: &str| (k == "GITLAB_TOKEN").then(|| TOKEN.to_string());
        let c = creds(&cfg("  ", ""), &only_token).unwrap();
        assert_eq!(c.base, "https://gitlab.com");
        assert_eq!(c.api, "https://gitlab.com/api/v4");
    }

    #[test]
    fn missing_token_says_where_to_set_it() {
        let e = creds(&cfg(BASE, ""), &no_env).unwrap_err();
        assert!(e.contains("Settings → Connectors → GitLab"), "{e}");
        assert!(e.contains("GITLAB_TOKEN"), "{e}");
        assert!(e.contains("read_api"), "{e}");
    }

    #[test]
    fn bad_base_is_refused_without_echoing_it() {
        for b in [
            "ftp://gitlab.example.com",
            "https://",
            "https://oauth2:glpat-secretsecret@gitlab.example.com",
            "http://[::1",
        ] {
            let e = creds(&cfg(b, TOKEN), &no_env).unwrap_err();
            assert!(e.contains("Settings → Connectors → GitLab"), "{e}");
            assert!(
                !e.contains("glpat-secretsecret") && !e.contains(TOKEN),
                "{e}"
            );
        }
        let env = |k: &str| match k {
            "GITLAB_URL" => Some("ftp://x".to_string()),
            _ => Some(TOKEN.to_string()),
        };
        let e = creds(&cfg("", ""), &env).unwrap_err();
        assert!(e.contains("GITLAB_URL"), "{e}");
    }

    #[test]
    fn base_urls_are_normalized() {
        for (raw, want) in [
            ("https://gitlab.example.com", "https://gitlab.example.com"),
            ("https://gitlab.example.com/", "https://gitlab.example.com"),
            (
                "https://gitlab.example.com/api/v4",
                "https://gitlab.example.com",
            ),
            (
                "https://gitlab.example.com/api/v4/",
                "https://gitlab.example.com",
            ),
            (
                "https://GitLab.Example.com/API/V4",
                "https://gitlab.example.com",
            ),
            ("https://example.com/gitlab", "https://example.com/gitlab"),
            (
                "https://example.com/gitlab/api/v4",
                "https://example.com/gitlab",
            ),
            ("http://10.0.0.5:8080/", "http://10.0.0.5:8080"),
            (
                "https://gitlab.example.com:443",
                "https://gitlab.example.com",
            ),
            ("gitlab.example.com", "https://gitlab.example.com"),
            (
                "  https://gitlab.example.com?x=1#y ",
                "https://gitlab.example.com",
            ),
        ] {
            assert_eq!(normalize_base(raw).as_deref(), Ok(want), "{raw}");
        }
        assert_eq!(instance("https://example.com/gitlab"), "example.com/gitlab");
    }

    #[test]
    fn creds_debug_hides_the_token() {
        let c = creds(&cfg(BASE, TOKEN), &no_env).unwrap();
        let d = format!("{c:?}{:?}", cfg(BASE, TOKEN));
        assert!(!d.contains(TOKEN), "{d}");
    }

    // ── project resolution ──

    #[test]
    fn projects_come_from_remotes_on_the_same_host_only() {
        let cases = [
            (
                "https://gitlab.example.com/group/proj.git",
                BASE,
                Some("group/proj"),
            ),
            (
                "https://gitlab.example.com/group/sub/proj",
                BASE,
                Some("group/sub/proj"),
            ),
            (
                "https://gitlab.example.com/group/proj/",
                BASE,
                Some("group/proj"),
            ),
            (
                "https://oauth2:glpat-x@gitlab.example.com/group/proj.git",
                BASE,
                Some("group/proj"),
            ),
            (
                "git@gitlab.example.com:group/sub/proj.git",
                BASE,
                Some("group/sub/proj"),
            ),
            ("gitlab.example.com:group/proj", BASE, Some("group/proj")),
            (
                "ssh://git@gitlab.example.com:2222/group/proj.git",
                BASE,
                Some("group/proj"),
            ),
            (
                "ssh://git@GITLAB.example.com/group/proj.git",
                BASE,
                Some("group/proj"),
            ),
            (
                "https://gitlab.example.com/group/proj.git",
                "https://gitlab.example.com/api/v4/",
                Some("group/proj"),
            ),
            // An instance under a sub-path: https remotes carry it, ssh ones don't.
            (
                "https://example.com/gitlab/group/proj.git",
                "https://example.com/gitlab",
                Some("group/proj"),
            ),
            (
                "git@example.com:group/proj.git",
                "https://example.com/gitlab",
                Some("group/proj"),
            ),
            (
                "https://example.com/other/group/proj.git",
                "https://example.com/gitlab",
                None,
            ),
            (
                "https://example.com/gitlabx/group/proj.git",
                "https://example.com/gitlab",
                None,
            ),
            // Other hosts never match.
            ("https://github.com/owner/repo.git", BASE, None),
            ("git@github.com:owner/repo.git", BASE, None),
            (
                "https://gitlab.example.com.evil.com/group/proj.git",
                BASE,
                None,
            ),
            ("https://gitlab.com/group/proj.git", BASE, None),
            (
                "git@gitlab.com:group/proj.git",
                "https://gitlab.com",
                Some("group/proj"),
            ),
            // Not a project path.
            ("https://gitlab.example.com/proj.git", BASE, None),
            ("https://gitlab.example.com/", BASE, None),
            ("https://gitlab.example.com/group/../proj", BASE, None),
            ("file:///srv/git/group/proj.git", BASE, None),
            ("C:\\repos\\proj", BASE, None),
            ("", BASE, None),
        ];
        for (remote, base, want) in cases {
            assert_eq!(
                project_from_remote(remote, base).as_deref(),
                want,
                "{remote} on {base}"
            );
        }
    }

    #[test]
    fn explicit_projects_beat_the_remote() {
        let remote = || Some("git@gitlab.example.com:group/proj.git".to_string());
        let called = std::cell::Cell::new(false);
        let tracking = || {
            called.set(true);
            Some("git@gitlab.example.com:group/proj.git".to_string())
        };
        assert_eq!(
            pick_project(&json!({"project": " other/thing/ "}), &tracking, BASE).unwrap(),
            ("other/thing".to_string(), "as requested")
        );
        assert!(
            !called.get(),
            "git is not consulted when a project is named"
        );
        assert_eq!(
            pick_project(&json!({"project": 42}), &remote, BASE).unwrap(),
            ("42".to_string(), "as requested")
        );
        assert_eq!(
            pick_project(
                &json!({"project": "https://gitlab.example.com/a/b"}),
                &remote,
                BASE
            )
            .unwrap()
            .0,
            "a/b"
        );
        assert!(pick_project(&json!({"project": "https://evil.com/a/b"}), &remote, BASE).is_err());
        assert_eq!(
            pick_project(&json!({}), &remote, BASE).unwrap(),
            (
                "group/proj".to_string(),
                "from the working directory's origin remote"
            )
        );
        let gh = || Some("https://x:ghp_secrettoken123@github.com/o/r.git".to_string());
        let e = pick_project(&json!({}), &gh, BASE).unwrap_err();
        assert!(
            e.contains("gitlab.example.com") && e.contains("project"),
            "{e}"
        );
        assert!(
            !e.contains("ghp_secrettoken123"),
            "remote must not be echoed: {e}"
        );
        let none = || None;
        assert!(pick_project(&json!({}), &none, BASE)
            .unwrap_err()
            .contains("no git `origin` remote"));
    }

    #[test]
    fn instance_wide_actions_need_no_project() {
        let none = || None;
        for a in ["whoami", "list_projects", "api"] {
            assert_eq!(project_for(a, &json!({}), &none, BASE).unwrap(), None);
        }
        assert_eq!(
            project_for("search", &json!({"query": "x"}), &none, BASE).unwrap(),
            None
        );
        assert_eq!(
            project_for(
                "search",
                &json!({"query": "x", "scope": "blobs"}),
                &none,
                BASE
            )
            .unwrap(),
            None
        );
        let remote = || Some("git@gitlab.example.com:g/p.git".to_string());
        assert_eq!(
            project_for(
                "search",
                &json!({"query": "x", "scope": "blobs"}),
                &remote,
                BASE
            )
            .unwrap()
            .unwrap()
            .0,
            "g/p"
        );
        assert_eq!(
            project_for("search", &json!({"query": "x"}), &remote, BASE).unwrap(),
            None,
            "issue search stays instance-wide unless a project is named"
        );
        assert!(project_for("list_issues", &json!({}), &none, BASE).is_err());
    }

    // ── request building ──

    #[test]
    fn project_paths_are_one_encoded_segment() {
        assert_eq!(proj("group/sub/name"), "projects/group%2Fsub%2Fname");
        assert_eq!(proj("42"), "projects/42");
        assert_eq!(proj("a b?#"), "projects/a%20b%3F%23");
        // Survives URL parsing on the real base, sub-path included.
        let u = plugin_services::pin_to_base(
            "https://example.com/gitlab/api/v4",
            &req_issue("g/s/p", 3).path,
        )
        .unwrap();
        assert_eq!(
            u.as_str(),
            "https://example.com/gitlab/api/v4/projects/g%2Fs%2Fp/issues/3"
        );
        // `..` inside a path is part of the one segment, not a dot segment.
        let u = plugin_services::pin_to_base("https://x.com/api/v4", &req_issue("g/..", 3).path)
            .unwrap();
        assert_eq!(u.path(), "/api/v4/projects/g%2F../issues/3");
        // A bare `.`/`..` would be resolved by the URL parser even encoded,
        // so it is refused as input.
        let none = || None;
        for bad in ["..", ".", "/../"] {
            assert!(
                pick_project(&json!({ "project": bad }), &none, BASE).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn list_requests_encode_filters() {
        let r = req_list_projects(&json!({"search": "my app&x=1", "limit": 500}));
        assert_eq!(
            r.path,
            "projects?membership=true&simple=true&order_by=last_activity_at&search=my%20app%26x%3D1&per_page=100&page=1"
        );
        assert_eq!(r.method, "GET");
        let r = req_list_projects(&json!({"membership": false, "limit": 0, "page": 3}));
        assert!(r.path.contains("membership=false") && r.path.contains("per_page=1&page=3"));

        let r = req_list_issues(
            "g/p",
            &json!({"state": "all", "labels": ["bug", "P 1"], "assignee": "@ada", "search": "crash"}),
        )
        .unwrap();
        assert_eq!(
            r.path,
            "projects/g%2Fp/issues?labels=bug%2CP%201&assignee_username=ada&search=crash&per_page=20&page=1"
        );
        let r = req_list_issues("g/p", &json!({"state": "open", "assignee": "none"})).unwrap();
        assert!(r.path.contains("state=opened") && r.path.contains("assignee_id=None"));
        let r = req_list_issues("g/p", &json!({"assignee": "17"})).unwrap();
        assert!(r.path.contains("state=opened&assignee_id=17"));
        assert!(req_list_issues("g/p", &json!({"state": "merged"})).is_err());

        let r = req_list_mrs("g/p", &json!({"state": "merged", "limit": 5})).unwrap();
        assert_eq!(
            r.path,
            "projects/g%2Fp/merge_requests?state=merged&per_page=5&page=1"
        );
        let r = req_list_mrs("g/p", &json!({"state": "all"})).unwrap();
        assert!(!r.path.contains("state="));
        assert!(req_list_mrs("g/p", &json!({"state": "bogus"})).is_err());

        let r =
            req_list_pipelines("g/p", &json!({"ref": "feature/x", "status": "FAILED"})).unwrap();
        assert_eq!(
            r.path,
            "projects/g%2Fp/pipelines?ref=feature%2Fx&status=failed&per_page=20&page=1"
        );
        assert!(req_list_pipelines("g/p", &json!({"status": "broken"})).is_err());
    }

    #[test]
    fn detail_requests() {
        assert_eq!(req_user(), get("user".into()));
        assert_eq!(req_project("g/p").path, "projects/g%2Fp");
        assert_eq!(
            req_notes("g/p", "issues", 5).path,
            "projects/g%2Fp/issues/5/notes?sort=asc&order_by=created_at&per_page=100"
        );
        assert_eq!(req_mr("g/p", 7).path, "projects/g%2Fp/merge_requests/7");
        assert_eq!(
            req_mr_diffs("g/p", 7, 2).path,
            "projects/g%2Fp/merge_requests/7/diffs?per_page=100&page=2"
        );
        assert_eq!(
            req_mr_changes("g/p", 7).path,
            "projects/g%2Fp/merge_requests/7/changes"
        );
        assert_eq!(req_pipeline("1", 9).path, "projects/1/pipelines/9");
        assert_eq!(
            req_pipeline_jobs("1", 9).path,
            "projects/1/pipelines/9/jobs?per_page=100"
        );
        assert_eq!(req_job_trace("1", 33).path, "projects/1/jobs/33/trace");
        assert_eq!(
            req_file("g/p", "/src/main file.rs", "feat/x").unwrap().path,
            "projects/g%2Fp/repository/files/src%2Fmain%20file.rs/raw?ref=feat%2Fx"
        );
        assert!(req_file("g/p", "..", "HEAD").is_err());
        assert_eq!(
            req_file("g/p", "../x", "HEAD").unwrap().path,
            "projects/g%2Fp/repository/files/..%2Fx/raw?ref=HEAD"
        );
        assert!(req_file("g/p", " / ", "HEAD").is_err());
        let r = req_retry("g/p", 9);
        assert_eq!(
            (r.method, r.path.as_str(), r.body),
            ("POST", "projects/g%2Fp/pipelines/9/retry", None)
        );
    }

    #[test]
    fn search_requests() {
        let i = json!({"query": "fn main()", "scope": "blobs"});
        assert_eq!(
            req_search(Some("g/p"), "blobs", &i).unwrap().path,
            "projects/g%2Fp/search?scope=blobs&search=fn%20main%28%29&per_page=20&page=1"
        );
        assert_eq!(
            req_search(None, "issues", &json!({"query": "a&b"}))
                .unwrap()
                .path,
            "search?scope=issues&search=a%26b&per_page=20&page=1"
        );
        assert!(req_search(Some("g/p"), "projects", &json!({"query": "x"}))
            .unwrap()
            .path
            .starts_with("search?scope=projects"));
        assert!(req_search(None, "issues", &json!({})).is_err());
        assert_eq!(search_scope(&json!({})).unwrap(), "issues");
        assert_eq!(
            search_scope(&json!({"scope": "MRs"})).unwrap(),
            "merge_requests"
        );
        assert_eq!(search_scope(&json!({"scope": "code"})).unwrap(), "blobs");
        assert!(search_scope(&json!({"scope": "users"})).is_err());
    }

    #[test]
    fn issue_writes() {
        let r = req_create_issue(
            "g/p",
            &json!({"title": " Crash on \"start\" ", "description": "  steps\n1. run", "labels": ["bug", "ui"], "assignee_ids": [3, "4"]}),
        )
        .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "projects/g%2Fp/issues");
        assert_eq!(
            r.body.unwrap(),
            json!({"title": "Crash on \"start\"", "description": "  steps\n1. run", "labels": "bug,ui", "assignee_ids": [3, 4]})
        );
        assert!(req_create_issue("g/p", &json!({"description": "x"})).is_err());

        let r = req_update_issue(
            "g/p",
            &json!({"iid": "#12", "state_event": "close", "add_labels": "done", "remove_labels": ["doing"]}),
        )
        .unwrap();
        assert_eq!(
            (r.method, r.path.as_str()),
            ("PUT", "projects/g%2Fp/issues/12")
        );
        assert_eq!(
            r.body.unwrap(),
            json!({"state_event": "close", "add_labels": "done", "remove_labels": "doing"})
        );
        let r = req_update_issue("g/p", &json!({"iid": 1, "state": "opened"})).unwrap();
        assert_eq!(r.body.unwrap(), json!({"state_event": "reopen"}));
        assert!(req_update_issue("g/p", &json!({"iid": 1})).is_err());
        assert!(req_update_issue("g/p", &json!({"title": "x"})).is_err());
        assert!(req_update_issue("g/p", &json!({"iid": 1, "state_event": "delete"})).is_err());
    }

    #[test]
    fn mr_writes() {
        let r = req_create_mr(
            "g/p",
            &json!({"title": "Add login", "description": "Closes #3", "draft": true, "remove_source_branch": true}),
            "feat/login",
            "main",
        )
        .unwrap();
        assert_eq!(
            (r.method, r.path.as_str()),
            ("POST", "projects/g%2Fp/merge_requests")
        );
        assert_eq!(
            r.body.unwrap(),
            json!({"source_branch": "feat/login", "target_branch": "main", "title": "Draft: Add login", "description": "Closes #3", "remove_source_branch": true})
        );
        let r =
            req_create_mr("g/p", &json!({"title": "WIP: x", "draft": false}), "a", "b").unwrap();
        assert_eq!(r.body.unwrap()["title"], "x");
        let r = req_create_mr(
            "g/p",
            &json!({"title": "Draft: x", "draft": true}),
            "a",
            "b",
        )
        .unwrap();
        assert_eq!(r.body.unwrap()["title"], "Draft: x", "no double prefix");
        assert!(req_create_mr("g/p", &json!({}), "a", "b").is_err());

        let r = req_update_mr(
            "g/p",
            &json!({"iid": 4, "target_branch": "release", "state_event": "reopen"}),
            None,
        )
        .unwrap();
        assert_eq!(
            (r.method, r.path.as_str()),
            ("PUT", "projects/g%2Fp/merge_requests/4")
        );
        assert_eq!(
            r.body.unwrap(),
            json!({"state_event": "reopen", "target_branch": "release"})
        );
        let r = req_update_mr(
            "g/p",
            &json!({"iid": 4, "draft": false}),
            Some("[Draft] Ship it"),
        )
        .unwrap();
        assert_eq!(r.body.unwrap(), json!({"title": "Ship it"}));
        let r = req_update_mr("g/p", &json!({"iid": 4, "draft": true}), Some("Ship it")).unwrap();
        assert_eq!(r.body.unwrap(), json!({"title": "Draft: Ship it"}));
        assert!(req_update_mr("g/p", &json!({"iid": 4, "draft": true}), None).is_err());
        assert!(req_update_mr("g/p", &json!({"iid": 4}), None).is_err());
    }

    #[test]
    fn comments_go_to_notes() {
        let r = req_comment("g/p", &json!({"kind": "mr", "iid": "!8", "body": "LGTM"})).unwrap();
        assert_eq!(r.path, "projects/g%2Fp/merge_requests/8/notes");
        assert_eq!(r.body.unwrap(), json!({"body": "LGTM"}));
        let r = req_comment("g/p", &json!({"kind": "issue", "iid": 2, "body": "x"})).unwrap();
        assert_eq!(r.path, "projects/g%2Fp/issues/2/notes");
        assert!(req_comment("g/p", &json!({"iid": 2, "body": "x"})).is_err());
        assert!(req_comment("g/p", &json!({"kind": "mr", "iid": 2, "body": "  "})).is_err());
        assert!(req_comment("g/p", &json!({"kind": "mr", "body": "x"})).is_err());
        assert_eq!(
            note_url(BASE, "g/p", "issues", 2, &json!(99)),
            " https://gitlab.example.com/g/p/-/issues/2#note_99"
        );
        assert_eq!(note_url(BASE, "42", "issues", 2, &json!(99)), "");
    }

    #[test]
    fn raw_api_stays_on_the_configured_host() {
        let api = "https://example.com/gitlab/api/v4";
        assert_eq!(
            raw_url(api, "/projects?per_page=5").unwrap().as_str(),
            "https://example.com/gitlab/api/v4/projects?per_page=5"
        );
        assert_eq!(
            raw_url(api, "/api/v4/projects/1").unwrap().as_str(),
            "https://example.com/gitlab/api/v4/projects/1"
        );
        assert!(raw_url(api, "https://example.com/gitlab/api/v4/user").is_ok());
        for bad in [
            "//evil.com/x",
            "https://evil.com/api/v4/user",
            "https://example.com@evil.com/x",
            "https://example.com.evil.com/x",
            "http://example.com/gitlab/api/v4/user",
            "\\\\evil.com\\x",
            "/\\evil.com",
            "%2f%2fevil.com",
            "/%2F%2Fevil.com",
            "/api/v4/%2f%2fevil.com",
            "/x\r\nHost: evil.com",
            "/api/v4",
            "",
        ] {
            assert!(raw_url(api, bad).is_err(), "{bad:?} must be refused");
        }
        // Whatever slips past classification still lands on the base host.
        for odd in ["/api/v4//evil.com", "../../../x", "/@evil.com"] {
            assert_eq!(
                raw_url(api, odd).unwrap().host_str(),
                Some("example.com"),
                "{odd}"
            );
        }
    }

    // ── errors ──

    #[test]
    fn errors_carry_hints_and_never_the_token() {
        let e = error_for(
            401,
            "GET",
            "/api/v4/user",
            &format!(r#"{{"message":"401 Unauthorized {TOKEN}"}}"#),
            TOKEN,
        );
        assert!(
            e.starts_with("GitLab 401 on GET /api/v4/user: 401 Unauthorized"),
            "{e}"
        );
        assert!(
            e.contains("read_api") && e.contains("Settings → Connectors → GitLab"),
            "{e}"
        );
        assert!(!e.contains(TOKEN), "{e}");
        let e = error_for(403, "POST", "/p", r#"{"message":"403 Forbidden"}"#, TOKEN);
        assert!(e.contains("role") && e.contains("scope"), "{e}");
        let e = error_for(
            404,
            "GET",
            "/p",
            r#"{"message":"404 Project Not Found"}"#,
            TOKEN,
        );
        assert!(
            e.contains("404 Project Not Found") && e.contains("group/sub/name"),
            "{e}"
        );
        let e = error_for(
            400,
            "POST",
            "/p",
            r#"{"message":{"title":["can't be blank"],"labels":["is invalid"]}}"#,
            TOKEN,
        );
        assert!(
            e.contains("title: can't be blank") && e.contains("labels: is invalid"),
            "{e}"
        );
        let e = error_for(
            401,
            "GET",
            "/u",
            r#"{"error":"invalid_token","error_description":"Token was revoked"}"#,
            TOKEN,
        );
        assert!(e.contains("Token was revoked"), "{e}");
        let e = error_for(502, "GET", "/u", "<html>Bad gateway</html>", TOKEN);
        assert!(e.contains("Bad gateway") && !e.ends_with(' '), "{e}");
        let e = parse_json(&format!("not json {TOKEN}"), TOKEN).unwrap_err();
        assert!(!e.contains(TOKEN), "{e}");
        assert_eq!(parse_json("  ", TOKEN).unwrap(), Value::Null);
    }

    // ── formatting ──

    fn issue_fixture() -> Value {
        json!({
            "id": 900, "iid": 12, "project_id": 5, "title": "Login button does nothing",
            "description": "Steps:\n1. click", "state": "opened",
            "created_at": "2024-05-01T10:22:33.123Z", "updated_at": "2024-05-02T08:00:00.000Z",
            "labels": ["bug", "frontend"], "milestone": {"title": "v1.2"},
            "author": {"id": 1, "username": "ada", "name": "Ada"},
            "assignees": [{"id": 2, "username": "bob"}],
            "user_notes_count": 2, "due_date": null, "confidential": false,
            "web_url": "https://gitlab.example.com/g/p/-/issues/12"
        })
    }

    #[test]
    fn issue_rows_and_details() {
        assert_eq!(
            issue_row(&issue_fixture()),
            "#12 Login button does nothing [opened] {bug, frontend} · @ada → @bob · 2 comments · https://gitlab.example.com/g/p/-/issues/12"
        );
        assert_eq!(issue_row(&json!({})), "#?  [] · @? · 0 comments · ");
        let notes = json!([
            {"id": 1, "body": "added ~bug label", "system": true, "author": {"username": "ada"}, "created_at": "2024-05-01T10:23:00Z"},
            {"id": 2, "body": "Repro'd on Firefox", "system": false, "author": {"username": "bob"}, "created_at": "2024-05-01T11:00:00Z"}
        ]);
        let out = fmt_issue(&issue_fixture(), &notes, false);
        assert!(
            out.contains("milestone v1.2") && out.contains("created 2024-05-01 10:22"),
            "{out}"
        );
        assert!(out.contains("Steps:\n1. click"));
        assert!(
            out.contains("Notes (1):\n── @bob · 2024-05-01 11:00\nRepro'd on Firefox"),
            "{out}"
        );
        assert!(!out.contains("added ~bug label"));
        assert!(out.contains("1 system note hidden"));
        let out = fmt_issue(&issue_fixture(), &notes, true);
        assert!(out.contains("added ~bug label") && out.contains("· system"));
        let mut long = issue_fixture();
        long["description"] = json!("x".repeat(25_000));
        let out = fmt_issue(&long, &json!([]), false);
        assert!(out.contains("[… truncated]") && out.len() < 21_000);
        let out = fmt_issue(&json!({"iid": 1}), &Value::Null, false);
        assert!(out.contains("(no description)"));
    }

    #[test]
    fn list_output_and_pagination_note() {
        let v = json!([issue_fixture(), issue_fixture()]);
        let out = list_out(&v, &json!({"limit": 2}), issue_row, "No issues.");
        assert_eq!(out.lines().count(), 3, "two rows and the more-note: {out}");
        assert!(out.contains("pass page: 2"), "{out}");
        let out = list_out(&v, &json!({"limit": 5}), issue_row, "No issues.");
        assert!(!out.contains("more may be available"));
        assert_eq!(
            list_out(&json!([]), &json!({}), issue_row, "No issues."),
            "No issues."
        );
        assert_eq!(
            list_out(&json!([]), &json!({"page": 3}), issue_row, "No issues."),
            "No issues. (page 3)"
        );
        assert_eq!(
            list_out(
                &json!({"message": "x"}),
                &json!({}),
                mr_row,
                "No merge requests."
            ),
            "No merge requests."
        );
    }

    #[test]
    fn whoami_and_project_rows() {
        let u = json!({"id": 1, "username": "ada", "name": "Ada Lovelace", "email": "ada@example.com", "is_admin": false});
        assert_eq!(
            whoami_line(&u, "gitlab.example.com"),
            "Signed in to gitlab.example.com as @ada (Ada Lovelace)"
        );
        assert_eq!(
            identity_line(&u, "example.com/gitlab"),
            "Ada Lovelace @ada · example.com/gitlab"
        );
        assert_eq!(identity_line(&json!({"username": "bot"}), "h"), "@bot · h");
        let p = json!({"id": 5, "path_with_namespace": "g/p", "default_branch": "main", "visibility": "private",
            "web_url": "https://gitlab.example.com/g/p", "description": "The app\nmore", "archived": false});
        assert_eq!(
            project_row(&p),
            "g/p · id 5 · private · default main · https://gitlab.example.com/g/p — The app"
        );
        assert_eq!(project_row(&json!({"id": 1})), "? · id 1 · ");
    }

    fn mr_fixture() -> Value {
        json!({
            "id": 77, "iid": 34, "title": "Draft: Add login", "state": "opened", "draft": true,
            "source_branch": "feat/login", "target_branch": "main",
            "author": {"username": "ada"}, "assignees": [{"username": "bob"}], "reviewers": [{"username": "cy"}],
            "labels": ["auth"], "description": "Adds login.\n\nCloses #12",
            "detailed_merge_status": "ci_still_running", "has_conflicts": false, "changes_count": "2",
            "created_at": "2024-05-03T09:00:00Z", "merged_at": null,
            "head_pipeline": {"id": 501, "status": "running", "web_url": "https://gitlab.example.com/g/p/-/pipelines/501"},
            "web_url": "https://gitlab.example.com/g/p/-/merge_requests/34"
        })
    }

    fn diffs_fixture() -> Vec<Value> {
        vec![
            json!({"old_path": "src/login.rs", "new_path": "src/login.rs", "a_mode": "100644", "b_mode": "100644",
                "new_file": true, "renamed_file": false, "deleted_file": false,
                "diff": "@@ -0,0 +1,2 @@\n+fn login() {}\n+// done\n"}),
            json!({"old_path": "old.txt", "new_path": "new.txt", "a_mode": "100644", "b_mode": "100644",
                "new_file": false, "renamed_file": true, "deleted_file": false,
                "diff": "@@ -1 +1 @@\n-a\n+b"}),
            json!({"old_path": "gone.md", "new_path": "gone.md", "a_mode": "100644", "b_mode": "0",
                "new_file": false, "renamed_file": false, "deleted_file": true,
                "diff": "@@ -1 +0,0 @@\n-bye\n"}),
            json!({"old_path": "big.json", "new_path": "big.json", "new_file": false, "renamed_file": false,
                "deleted_file": false, "diff": "", "too_large": true}),
        ]
    }

    #[test]
    fn mr_rows_and_details() {
        assert_eq!(
            mr_row(&mr_fixture()),
            "!34 Draft: Add login [opened, draft] feat/login → main · @ada · https://gitlab.example.com/g/p/-/merge_requests/34"
        );
        assert_eq!(
            mr_row(&json!({"work_in_progress": true})),
            "!?  [, draft] ? → ? · @? · "
        );
        let notes = json!([{"id": 3, "body": "nit: rename", "author": {"username": "cy"}, "created_at": "2024-05-03T10:00:00Z",
            "type": "DiffNote", "resolved": true, "position": {"new_path": "src/login.rs", "new_line": 1}}]);
        let out = fmt_mr(&mr_fixture(), &diffs_fixture(), true, &notes, false);
        assert!(
            out.contains(
                "pipeline: running (#501) · merge status: ci_still_running · 2 files changed"
            ),
            "{out}"
        );
        assert!(
            out.contains("assignees: @bob · reviewers: @cy · labels: auth"),
            "{out}"
        );
        assert!(out.contains("Changes (4 files):\n  A src/login.rs (+2 −0)\n  R old.txt → new.txt (+1 −1)\n  D gone.md (+0 −1)\n  M big.json (+0 −0) (diff too large to show)"), "{out}");
        assert!(
            out.contains("── @cy · 2024-05-03 10:00 on src/login.rs:1 · resolved\nnit: rename"),
            "{out}"
        );
        assert!(out.ends_with("Use mr_diff for the full diff."));
        let out = fmt_mr(&json!({"iid": 1}), &[], false, &json!([]), false);
        assert!(
            out.contains("pipeline: none")
                && out.contains("first page only")
                && out.contains("(no description)"),
            "{out}"
        );
    }

    #[test]
    fn diffs_assemble_like_git() {
        let d = assemble_diff(&diffs_fixture());
        assert!(d.starts_with("diff --git a/src/login.rs b/src/login.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/login.rs\n@@ -0,0 +1,2 @@\n+fn login() {}\n"), "{d}");
        assert!(d.contains("diff --git a/old.txt b/new.txt\nrename from old.txt\nrename to new.txt\n--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-a\n+b\n"), "{d}");
        assert!(
            d.contains("deleted file mode 100644\n--- a/gone.md\n+++ /dev/null\n"),
            "{d}"
        );
        assert!(
            d.contains("diff --git a/big.json b/big.json\n(diff too large to show"),
            "{d}"
        );
        assert_eq!(assemble_diff(&[]), "");
        assert_eq!(diff_counts("@@ -1 +1 @@\n-a\n+b\n+c\n d"), (2, 1));
    }

    #[test]
    fn pipelines_highlight_failures() {
        let p = json!({"id": 501, "status": "failed", "ref": "main", "sha": "0123456789abcdef", "source": "push",
            "duration": 312.0, "web_url": "https://gitlab.example.com/g/p/-/pipelines/501", "created_at": "2024-05-03T09:00:00Z"});
        assert_eq!(
            pipeline_row(&p),
            "#501 [failed] main · 01234567 · push · 2024-05-03 09:00 · https://gitlab.example.com/g/p/-/pipelines/501"
        );
        let jobs = json!([
            {"id": 12, "name": "deploy", "stage": "deploy", "status": "skipped", "duration": null},
            {"id": 11, "name": "rspec 1/2", "stage": "test", "status": "failed", "duration": 45.25,
             "failure_reason": "script_failure", "allow_failure": false, "web_url": "https://gitlab.example.com/g/p/-/jobs/11"},
            {"id": 10, "name": "build", "stage": "build", "status": "success", "duration": 125.6}
        ]);
        let out = fmt_pipeline(&p, &jobs);
        assert!(
            out.starts_with("Pipeline #501 [failed] main · 01234567 · push · 5m 12s"),
            "{out}"
        );
        assert!(out.contains("FAILED jobs (1):\n  ✗ 11 rspec 1/2 [test] script_failure · 45.2s · https://gitlab.example.com/g/p/-/jobs/11"), "{out}");
        let stages = out.find("  build:").unwrap() < out.find("  test:").unwrap()
            && out.find("  test:").unwrap() < out.find("  deploy:").unwrap();
        assert!(stages, "stages in order: {out}");
        assert!(out.contains("    ✓ 10 build [success] · 2m 06s"), "{out}");
        assert!(out.contains("    ✗ 11 rspec 1/2 [FAILED] · 45.2s"), "{out}");
        assert!(out.contains("    · 12 deploy [skipped] · -"), "{out}");
        assert!(out.ends_with("Use job_log with id: 11 for the failed job's log."));
        let out = fmt_pipeline(&json!({"id": 1}), &json!([]));
        assert!(out.ends_with("No jobs."));
    }

    #[test]
    fn traces_are_cleaned_and_tailed() {
        let raw = "\x1b[0KRunning with gitlab-runner 16.0\n\
            \x1b[0Ksection_start:1700000000:step_script\r\x1b[0K\x1b[32;1m$ cargo test\x1b[0;m\n\
            Downloading 10%\rDownloading 100%\n\
            test a ... ok\r\n\
            \x1b[31;1mERROR: Job failed: exit code 101\x1b[0;m\n\
            section_end:1700000099:step_script\r\x1b[0K";
        let (all, total) = clean_trace(raw, 200);
        assert_eq!(
            all,
            "Running with gitlab-runner 16.0\n$ cargo test\nDownloading 100%\ntest a ... ok\nERROR: Job failed: exit code 101"
        );
        assert_eq!(total, 5);
        let (t, _) = clean_trace(raw, 2);
        assert_eq!(t, "test a ... ok\nERROR: Job failed: exit code 101");
        let (t, n) = clean_trace("", 10);
        assert_eq!((t.as_str(), n), ("", 0));
    }

    #[test]
    fn search_results() {
        let blobs = json!([{"basename": "main", "data": "fn main() {\n    run();\n}", "path": "src/main.rs",
            "filename": "src/main.rs", "ref": "main", "startline": 1, "project_id": 5}]);
        let out = fmt_search("blobs", &blobs, &json!({}));
        assert_eq!(
            out,
            "1 blobs (page 1):\nsrc/main.rs:1 @main (project 5)\n    fn main() {\n        run();\n    }"
        );
        let out = fmt_search("issues", &json!([issue_fixture()]), &json!({"limit": 1}));
        assert!(
            out.contains("#12 Login button") && out.contains("pass page: 2"),
            "{out}"
        );
        assert!(fmt_search("merge_requests", &json!([mr_fixture()]), &json!({})).contains("!34"));
        assert!(fmt_search(
            "projects",
            &json!([{"id": 5, "path_with_namespace": "g/p"}]),
            &json!({})
        )
        .contains("g/p · id 5"));
        assert_eq!(
            fmt_search("merge_requests", &json!([]), &json!({})),
            "No merge requests found."
        );
    }

    #[test]
    fn successful_data_redacts_echoed_credentials_before_formatting() {
        for token in [TOKEN, "dummy-quote\"slash\\token"] {
            let body = json!({
                "iid": 12, "title": token, "name": format!("Ada {token}"), "username": "ada",
                "nested": [{token: [token, 42, true, null]}]
            })
            .to_string();
            let data = parse_json(&body, token).unwrap();
            assert_eq!(
                data["nested"][0],
                json!({"[redacted]": ["[redacted]", 42, true, null]})
            );
            for out in [
                issue_row(&data),
                identity_line(&data, "example.com"),
                whoami_line(&data, "example.com"),
            ] {
                assert!(
                    !out.contains(token),
                    "successful output leaked the credential"
                );
                assert!(out.contains("[redacted]"));
            }
            assert_eq!(data["iid"], 12);
        }
    }

    // One local response per call; no real account, repository or network service.
    async fn echo_fixture(body: String) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (base, server)
    }

    #[tokio::test]
    async fn successful_outputs_redact_echoed_credentials() {
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for (input, body) in [
            (
                json!({"action": "api", "path": "user"}),
                json!({"echo": TOKEN}).to_string(),
            ),
            (
                json!({"action": "api", "path": "user"}),
                format!("plain echo {TOKEN}"),
            ),
            (
                json!({"action": "get_file", "project": "g/p", "path": "test.txt"}),
                format!("file echo {TOKEN}"),
            ),
            (
                json!({"action": "job_log", "project": "g/p", "id": 1}),
                format!("log echo {TOKEN}"),
            ),
            (
                json!({"action": "whoami"}),
                json!({"name": TOKEN, "username": "ada"}).to_string(),
            ),
        ] {
            let (base, server) = echo_fixture(body).await;
            let out = run(&http, &cfg(&base, TOKEN), &input, "unused-fixture-cwd")
                .await
                .unwrap();
            server.await.unwrap();
            assert!(
                !out.contains(TOKEN),
                "successful output leaked the credential"
            );
            assert!(out.contains("[redacted]"));
        }
        let (base, server) =
            echo_fixture(json!({"name": TOKEN, "username": "ada"}).to_string()).await;
        let identity = whoami(&http, &cfg(&base, TOKEN)).await.unwrap();
        server.await.unwrap();
        assert!(
            !identity.identity.contains(TOKEN),
            "connection identity leaked the credential"
        );
        assert!(identity.identity.contains("[redacted]"));
    }

    #[test]
    fn small_helpers() {
        assert_eq!(fmt_dur(&json!(null)), "-");
        assert_eq!(fmt_dur(&json!(59.94)), "59.9s");
        assert_eq!(fmt_dur(&json!(3600)), "60m 00s");
        assert_eq!(when(&json!("2024-05-01T10:22:33Z")), "2024-05-01 10:22");
        assert_eq!(when(&Value::Null), "");
        assert_eq!(int(&json!({"iid": "!34"}), "iid"), Some(34));
        assert_eq!(int(&json!({"iid": -1}), "iid"), None);
        assert_eq!(limit(&json!({"limit": 1000})), 100);
        assert_eq!(limit(&json!({})), 20);
        assert_eq!(page(&json!({"page": 0})), 1);
        assert_eq!(
            labels(&json!({"l": " a , b "}), "l").as_deref(),
            Some("a , b")
        );
        assert_eq!(labels(&json!({"l": []}), "l"), None);
        assert_eq!(strip_draft("Draft: WIP: [Draft] x"), "x");
        assert!(looks_binary("PNG\0\0"));
        assert!(!looks_binary("text"));
        assert_eq!(
            with_query("p", &[("a", None), ("b", Some(String::new()))]),
            "p"
        );
    }
}
