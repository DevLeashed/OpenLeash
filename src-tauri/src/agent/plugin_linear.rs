//! The Linear plugin: one `linear` tool with an `action` enum.
//!
//! Wired into the harness through `plugin_services.rs` (schema, permissions,
//! dispatch, settings). Off by default; reads run freely, writes ask.
//!
//! Linear only has a GraphQL API (`POST https://api.linear.app/graphql`). Every
//! user- or model-supplied value travels in the request's `variables`, never
//! spliced into the query text, so a title like `"}) { issueDelete` is just a
//! string to the server. Request building (`read_request`, `issue_input`, the
//! `*_lookup` builders) and response formatting are pure functions so they are
//! tested without a network.

use super::plugin_services::{self, Identity};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LinearCfg {
    /// Off unless the user switches it on (Settings → Connectors).
    pub enabled: bool,
    /// Personal API key (lin_api_…). Empty = LINEAR_API_KEY.
    pub token: String,
}

/// Hand-written so a token never reaches a log line or a test failure message.
impl std::fmt::Debug for LinearCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearCfg")
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
    "list_issues",
    "search_issues",
    "get_issue",
    "list_teams",
    "list_states",
    "list_projects",
    "list_labels",
    "list_users",
];

/// Actions that change something on Linear (need approval).
/// `graphql` is classified separately in `is_write`.
pub const WRITES: &[&str] = &["create_issue", "update_issue", "comment"];

/// The raw passthrough action; a read or a write depending on its document.
const RAW: &str = "graphql";

/// The one and only endpoint. The raw action takes a document, not a URL, so
/// there is no path for the model to point the token somewhere else.
const ENDPOINT: &str = "https://api.linear.app/graphql";

const ENV_KEYS: &[&str] = &["LINEAR_API_KEY"];

/// Body / comment / whole-result clip sizes, matching the GitHub plugin.
const BODY_CLIP: usize = 20_000;
const COMMENT_CLIP: usize = 4_000;
const RESULT_CLIP: usize = 80_000;
const RAW_CLIP: usize = 60_000;

pub fn schema() -> Value {
    json!({
        "name": "linear",
        "description": "Work with Linear (issues, teams, projects) through its GraphQL API, authenticated as the user's API key. Actions:\n\
    - whoami: the key's user and workspace\n\
    - list_issues (team, state, assignee, label, project, limit, after): most recently updated first. team = key (ENG) or name; state = name (\"In Progress\") or type (backlog|unstarted|started|completed|canceled|triage); assignee = email, name, \"me\" or \"none\"\n\
    - search_issues (query, plus the same optional filters, limit, after): full-text search over titles/descriptions\n\
    - get_issue (issue = identifier like ENG-123 or UUID): details, description and comments\n\
    - list_teams (limit) · list_states (team optional) · list_projects (team, query, limit) · list_labels (team optional) · list_users (query, include_inactive, limit)\n\
    - create_issue (team, title, description, priority, assignee, state, labels, project)\n\
    - update_issue (issue + any of title, description, state, assignee, priority, labels, add_labels, remove_labels, project). `labels` replaces the whole label set; add_labels/remove_labels edit it. assignee/project \"none\" clears them\n\
    - comment (issue, body: markdown)\n\
    - graphql (query, variables, operation_name): any other GraphQL document against https://api.linear.app/graphql. Pass values in `variables`, not inside the query text.\n\
    Names are resolved to IDs for you (team key/name, state name, user email/name, label names, project name); IDs work too. Priority: 0 none, 1 urgent, 2 high, 3 medium, 4 low (numbers or those words). Issues are listed as `ENG-123 Title [State] · P2 · @assignee · {labels} · url`; lists say when more results are available and give the `after` cursor. Changes (create_issue, update_issue, comment, and any graphql document containing a mutation/subscription or that cannot be parsed confidently) ask the user first unless allowed. A key with read-only permission cannot write.",
        "input_schema": {"type":"object","properties":{
            "action":{"type":"string","enum":["whoami","list_issues","search_issues","get_issue","list_teams","list_states","list_projects","list_labels","list_users","create_issue","update_issue","comment","graphql"]},
            "issue":{"type":"string","description":"Issue identifier (ENG-123) or UUID — get_issue, update_issue, comment"},
            "team":{"type":"string","description":"Team key (ENG), name or UUID. Required for create_issue; a filter elsewhere"},
            "title":{"type":"string"},
            "description":{"type":"string","description":"Issue description (markdown)"},
            "body":{"description":"comment: the comment text (markdown)"},
            "state":{"type":"string","description":"Workflow state name (\"In Progress\"); for list/search filters also a state type (started, completed, …)"},
            "assignee":{"type":"string","description":"Email, name/display name, \"me\", or \"none\" (unassigned / clear)"},
            "priority":{"description":"0 none, 1 urgent, 2 high, 3 medium, 4 low (or those words)"},
            "label":{"type":"string","description":"list/search filter: issues having this label"},
            "labels":{"type":"array","items":{"type":"string"},"description":"Label names. create_issue: labels to set. update_issue: replaces all labels"},
            "add_labels":{"type":"array","items":{"type":"string"},"description":"update_issue: label names to add"},
            "remove_labels":{"type":"array","items":{"type":"string"},"description":"update_issue: label names to remove"},
            "project":{"type":"string","description":"Project name or UUID (\"none\" clears it on update)"},
            "query":{"type":"string","description":"search_issues: search text. list_projects/list_users: name filter. graphql: the GraphQL document"},
            "variables":{"type":"object","description":"graphql: the variables object"},
            "operation_name":{"type":"string","description":"graphql: which operation to run when the document has several"},
            "include_inactive":{"type":"boolean","description":"list_users: also list deactivated users"},
            "limit":{"type":"integer","description":"Max results (default 20, max 100)"},
            "after":{"type":"string","description":"Pagination cursor from a previous result's \"more available\" line"}},
            "required":["action"]}
    })
}

/// Whether this call changes something on Linear. Fails closed.
pub fn is_write(input: &Value) -> bool {
    let a = input["action"].as_str().unwrap_or("");
    if a == RAW {
        // A raw document is a read only when we can see it holds nothing but
        // queries (and fragments). A non-string query is not something we can
        // reason about, so it is a write.
        return match input["query"].as_str() {
            Some(q) => graphql_is_mutation(q),
            None => true,
        };
    }
    !READS.contains(&a)
}

/// Whether a GraphQL document may change something: true for any `mutation`
/// or `subscription` operation, and — failing closed — whenever the document
/// cannot be read with confidence (empty, unterminated string or block string,
/// unbalanced brackets, a top-level definition that is not `query`,
/// `fragment` or a `{ … }` shorthand query).
///
/// A small lexer rather than a substring search: `mutation` inside a string
/// argument, a `#` comment or a field alias at depth > 0 is not an operation,
/// while `query A { … } mutation B { … }` is a write even though it starts
/// with `query`. Keywords are case-sensitive in GraphQL, so `Mutation` at the
/// start of a definition is not a query and therefore counts as a write too.
pub fn graphql_is_mutation(query: &str) -> bool {
    let c: Vec<char> = query.chars().collect();
    let mut i = 0;
    // Expected closers for every open `{`, `(` and `[`. Only names seen with
    // this empty are operation keywords / names; everything nested is fields,
    // arguments, variable definitions or default values.
    let mut stack: Vec<char> = Vec::new();
    // At the start of the document or right after a top-level selection set
    // closes: the next token begins a new definition.
    let mut at_def_start = true;
    let mut definitions = 0usize;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '#' => {
                while i < c.len() && c[i] != '\n' && c[i] != '\r' {
                    i += 1;
                }
                continue;
            }
            '"' => {
                if c.get(i + 1) == Some(&'"') && c.get(i + 2) == Some(&'"') {
                    // Block string: ends at the first unescaped `"""`.
                    i += 3;
                    loop {
                        if i >= c.len() {
                            return true;
                        }
                        if c[i] == '\\' && c.get(i + 1..i + 4) == Some(&['"', '"', '"'][..]) {
                            i += 4;
                            continue;
                        }
                        if c[i] == '"' && c.get(i + 1) == Some(&'"') && c.get(i + 2) == Some(&'"') {
                            i += 3;
                            break;
                        }
                        i += 1;
                    }
                } else {
                    // Ordinary string: escapes skip a character; a raw line
                    // break is invalid GraphQL, so we stop trusting the lexer.
                    i += 1;
                    loop {
                        match c.get(i) {
                            None | Some('\n') | Some('\r') => return true,
                            Some('\\') => i += 2,
                            Some('"') => {
                                i += 1;
                                break;
                            }
                            Some(_) => i += 1,
                        }
                    }
                }
                continue;
            }
            '{' | '(' | '[' => {
                if stack.is_empty() && ch == '{' {
                    // `{ … }` with no keyword is the query shorthand.
                    at_def_start = false;
                }
                stack.push(match ch {
                    '{' => '}',
                    '(' => ')',
                    _ => ']',
                });
            }
            '}' | ')' | ']' => {
                if stack.pop() != Some(ch) {
                    return true;
                }
                if stack.is_empty() && ch == '}' {
                    at_def_start = true;
                    definitions += 1;
                }
            }
            _ if ch.is_ascii_alphabetic() || ch == '_' => {
                let start = i;
                while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                if stack.is_empty() {
                    let name: String = c[start..i].iter().collect();
                    // Anywhere at the top level, even as an operation name:
                    // not worth being clever about.
                    if name == "mutation" || name == "subscription" {
                        return true;
                    }
                    if at_def_start {
                        if name != "query" && name != "fragment" {
                            return true;
                        }
                        at_def_start = false;
                    }
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    // Unclosed brackets, or nothing executable at all (empty, comments only).
    !stack.is_empty() || definitions == 0
}

// ───────────────────────────── credentials ─────────────────────────────

/// The key, the exact `Authorization` header value, and where it came from.
struct Creds {
    token: String,
    auth: String,
    source: &'static str,
}

const NO_KEY: &str = "No Linear API key is configured. Add a personal API key (Linear → Settings → Security & access → Personal API keys) in Settings → Connectors → Linear, or set the LINEAR_API_KEY environment variable.";

fn creds(cfg: &LinearCfg, env: &dyn Fn(&str) -> Option<String>) -> Result<Creds, String> {
    let (raw, source) = plugin_services::setting_or_env(&cfg.token, ENV_KEYS, env).ok_or(NO_KEY)?;
    let (token, auth) = auth_header(&raw);
    if token.is_empty() {
        return Err(NO_KEY.into());
    }
    Ok(Creds {
        token,
        auth,
        source,
    })
}

/// `(bare token, Authorization header value)`. Linear wants personal API keys
/// (`lin_api_…`) bare — `Bearer lin_api_…` is rejected — but OAuth access
/// tokens with `Bearer`. Users paste either form, so a pasted `Bearer ` is
/// stripped from a personal key and kept (or added) for anything else.
fn auth_header(raw: &str) -> (String, String) {
    let t = raw.trim();
    // `Bearer` followed by whitespace or nothing (a lone `Bearer` is no key).
    let (bare, had_bearer) = match (t.get(..6), t.get(6..)) {
        (Some(p), Some(rest))
            if p.eq_ignore_ascii_case("bearer")
                && (rest.is_empty() || rest.starts_with(char::is_whitespace)) =>
        {
            (rest.trim(), true)
        }
        _ => (t, false),
    };
    let header = if bare.starts_with("lin_api_") || !(had_bearer || bare.starts_with("lin_oauth")) {
        bare.to_string()
    } else {
        format!("Bearer {bare}")
    };
    (bare.to_string(), header)
}

// ───────────────────────────── requests ─────────────────────────────

/// One GraphQL request: the document is a constant per action; everything the
/// caller supplied is in `variables`.
#[derive(Debug, Clone, PartialEq)]
struct Gql {
    query: String,
    variables: Value,
    operation_name: Option<String>,
}

impl Gql {
    fn new(query: &str, variables: Value) -> Self {
        Gql {
            query: query.to_string(),
            variables,
            operation_name: None,
        }
    }

    fn body(&self) -> Value {
        let mut b = json!({"query": self.query, "variables": self.variables});
        if let Some(op) = &self.operation_name {
            b["operationName"] = json!(op);
        }
        b
    }
}

const ROW: &str = "identifier title url priority state { name } assignee { name displayName } labels { nodes { name } }";
const PAGE: &str = "pageInfo { hasNextPage endCursor }";

const WHOAMI_Q: &str =
    "query Whoami { viewer { id name displayName email } organization { name urlKey } }";

fn list_issues_q() -> String {
    format!("query ListIssues($filter: IssueFilter, $first: Int, $after: String) {{ issues(filter: $filter, first: $first, after: $after, orderBy: updatedAt) {{ nodes {{ {ROW} }} {PAGE} }} }}")
}

/// `searchIssues` is Linear's current full-text search (the older
/// `issueSearch` is deprecated).
fn search_issues_q() -> String {
    format!("query SearchIssues($term: String!, $filter: IssueFilter, $first: Int, $after: String) {{ searchIssues(term: $term, filter: $filter, first: $first, after: $after) {{ nodes {{ {ROW} }} {PAGE} }} }}")
}

const GET_ISSUE_Q: &str = "query GetIssue($id: String!) { issue(id: $id) { id identifier title url description priority priorityLabel createdAt updatedAt dueDate estimate state { name type } assignee { name displayName email } creator { name displayName } team { key name } labels { nodes { name } } project { name } cycle { number name } parent { identifier title } comments(first: 50) { nodes { body createdAt user { name displayName } } pageInfo { hasNextPage } } } }";

const TEAMS_Q: &str = "query ListTeams($first: Int, $after: String) { teams(first: $first, after: $after) { nodes { id key name description } pageInfo { hasNextPage endCursor } } }";

const STATES_Q: &str = "query ListStates($filter: WorkflowStateFilter, $first: Int, $after: String) { workflowStates(filter: $filter, first: $first, after: $after) { nodes { id name type position team { key } } pageInfo { hasNextPage endCursor } } }";

const PROJECTS_Q: &str = "query ListProjects($filter: ProjectFilter, $first: Int, $after: String) { projects(filter: $filter, first: $first, after: $after, orderBy: updatedAt) { nodes { id name url progress targetDate status { name } lead { name displayName } teams { nodes { key } } } pageInfo { hasNextPage endCursor } } }";

const LABELS_Q: &str = "query ListLabels($filter: IssueLabelFilter, $first: Int, $after: String) { issueLabels(filter: $filter, first: $first, after: $after) { nodes { id name team { id key } parent { name } } pageInfo { hasNextPage endCursor } } }";

const USERS_Q: &str = "query ListUsers($filter: UserFilter, $first: Int, $after: String, $includeDisabled: Boolean) { users(filter: $filter, first: $first, after: $after, includeDisabled: $includeDisabled) { nodes { id name displayName email active admin } pageInfo { hasNextPage endCursor } } }";

const ISSUE_REF_Q: &str = "query IssueRef($id: String!) { issue(id: $id) { id identifier team { id key } labels { nodes { id name } } } }";

const TEAM_LOOKUP_Q: &str =
    "query TeamLookup($filter: TeamFilter) { teams(filter: $filter, first: 10) { nodes { id key name } } }";

const USER_LOOKUP_Q: &str = "query UserLookup($filter: UserFilter) { users(filter: $filter, first: 10) { nodes { id name displayName email } } }";

const STATE_LOOKUP_Q: &str = "query StateLookup($filter: WorkflowStateFilter) { workflowStates(filter: $filter, first: 10) { nodes { id name type } } }";

const LABEL_LOOKUP_Q: &str = "query LabelLookup($filter: IssueLabelFilter) { issueLabels(filter: $filter, first: 100) { nodes { id name team { id } } } }";

const PROJECT_LOOKUP_Q: &str = "query ProjectLookup($filter: ProjectFilter) { projects(filter: $filter, first: 10) { nodes { id name } } }";

fn create_issue_q() -> String {
    format!("mutation CreateIssue($input: IssueCreateInput!) {{ issueCreate(input: $input) {{ success issue {{ {ROW} }} }} }}")
}

fn update_issue_q() -> String {
    format!("mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) {{ issueUpdate(id: $id, input: $input) {{ success issue {{ {ROW} }} }} }}")
}

const COMMENT_Q: &str = "mutation Comment($input: CommentCreateInput!) { commentCreate(input: $input) { success comment { id url createdAt } } }";

/// A trimmed, non-empty string parameter.
fn s<'a>(input: &'a Value, k: &str) -> Option<&'a str> {
    input[k].as_str().map(str::trim).filter(|v| !v.is_empty())
}

fn limit(input: &Value) -> i64 {
    let n = input["limit"]
        .as_i64()
        .or_else(|| input["limit"].as_str().and_then(|v| v.trim().parse().ok()))
        .unwrap_or(20);
    n.clamp(1, 100)
}

fn page_vars(input: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("first".into(), json!(limit(input)));
    if let Some(a) = s(input, "after") {
        m.insert("after".into(), json!(a));
    }
    m
}

fn is_uuid(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `eng-123` → `ENG-123` (identifiers are upper-case); UUIDs untouched.
fn issue_ref(v: &str) -> String {
    let v = v.trim();
    match v.split_once('-') {
        Some((k, n))
            if !is_uuid(v)
                && !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric())
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit()) =>
        {
            v.to_ascii_uppercase()
        }
        _ => v.to_string(),
    }
}

fn is_none_word(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "none" | "unassigned" | "nobody" | "null"
    )
}

/// A `TeamFilter`: by id, or by key or name (case-insensitive).
fn team_filter(t: &str) -> Value {
    if is_uuid(t) {
        json!({"id": {"eq": t}})
    } else {
        json!({"or": [{"key": {"eqIgnoreCase": t}}, {"name": {"eqIgnoreCase": t}}]})
    }
}

/// A `UserFilter` for `me`, an email, an id or a (display) name.
fn user_filter(u: &str) -> Value {
    if u.eq_ignore_ascii_case("me") {
        json!({"isMe": {"eq": true}})
    } else if is_uuid(u) {
        json!({"id": {"eq": u}})
    } else if u.contains('@') {
        json!({"email": {"eqIgnoreCase": u}})
    } else {
        json!({"or": [{"name": {"eqIgnoreCase": u}}, {"displayName": {"eqIgnoreCase": u}}]})
    }
}

const STATE_TYPES: [&str; 6] = [
    "backlog",
    "unstarted",
    "started",
    "completed",
    "canceled",
    "triage",
];

/// An `IssueFilter` from the list/search filter params (`None` = no filter).
fn issue_filter(input: &Value) -> Option<Value> {
    let mut f = Map::new();
    if let Some(t) = s(input, "team") {
        f.insert("team".into(), team_filter(t));
    }
    if let Some(st) = s(input, "state") {
        let low = st.to_ascii_lowercase();
        let v = if STATE_TYPES.contains(&low.as_str()) {
            json!({"or": [{"name": {"eqIgnoreCase": st}}, {"type": {"eq": low}}]})
        } else {
            json!({"name": {"eqIgnoreCase": st}})
        };
        f.insert("state".into(), v);
    }
    if let Some(a) = s(input, "assignee") {
        let v = if is_none_word(a) {
            json!({"null": true})
        } else {
            user_filter(a)
        };
        f.insert("assignee".into(), v);
    }
    if let Some(l) = s(input, "label") {
        f.insert(
            "labels".into(),
            json!({"some": {"name": {"eqIgnoreCase": l}}}),
        );
    }
    if let Some(p) = s(input, "project") {
        let v = if is_uuid(p) {
            json!({"id": {"eq": p}})
        } else {
            json!({"name": {"eqIgnoreCase": p}})
        };
        f.insert("project".into(), v);
    }
    (!f.is_empty()).then_some(Value::Object(f))
}

/// The request for a read action or the raw `graphql` action.
fn read_request(action: &str, input: &Value) -> Result<Gql, String> {
    let mut vars = page_vars(input);
    let q = match action {
        "whoami" => return Ok(Gql::new(WHOAMI_Q, json!({}))),
        "list_issues" => {
            if let Some(f) = issue_filter(input) {
                vars.insert("filter".into(), f);
            }
            list_issues_q()
        }
        "search_issues" => {
            let term = s(input, "query").ok_or("search_issues needs `query` (the search text).")?;
            vars.insert("term".into(), json!(term));
            if let Some(f) = issue_filter(input) {
                vars.insert("filter".into(), f);
            }
            search_issues_q()
        }
        "get_issue" => {
            let id = s(input, "issue")
                .or_else(|| s(input, "id"))
                .ok_or("get_issue needs `issue` (an identifier like ENG-123, or a UUID).")?;
            return Ok(Gql::new(GET_ISSUE_Q, json!({"id": issue_ref(id)})));
        }
        "list_teams" => TEAMS_Q.to_string(),
        "list_states" => {
            if let Some(t) = s(input, "team") {
                vars.insert("filter".into(), json!({"team": team_filter(t)}));
            }
            vars.insert("first".into(), json!(100));
            STATES_Q.to_string()
        }
        "list_projects" => {
            let mut f = Vec::new();
            if let Some(t) = s(input, "team") {
                f.push(json!({"accessibleTeams": {"some": team_filter(t)}}));
            }
            if let Some(n) = s(input, "query") {
                f.push(json!({"name": {"containsIgnoreCase": n}}));
            }
            if !f.is_empty() {
                vars.insert("filter".into(), json!({"and": f}));
            }
            PROJECTS_Q.to_string()
        }
        "list_labels" => {
            if let Some(t) = s(input, "team") {
                // A team's issues can use its own labels and workspace ones.
                vars.insert(
                    "filter".into(),
                    json!({"or": [{"team": team_filter(t)}, {"team": {"null": true}}]}),
                );
            }
            if input["limit"].is_null() {
                vars.insert("first".into(), json!(100));
            }
            LABELS_Q.to_string()
        }
        "list_users" => {
            let mut f = Vec::new();
            if let Some(n) = s(input, "query") {
                f.push(json!({"or": [
                    {"name": {"containsIgnoreCase": n}},
                    {"displayName": {"containsIgnoreCase": n}},
                    {"email": {"containsIgnoreCase": n}}
                ]}));
            }
            if !f.is_empty() {
                vars.insert("filter".into(), json!({"and": f}));
            }
            vars.insert(
                "includeDisabled".into(),
                json!(input["include_inactive"].as_bool().unwrap_or(false)),
            );
            USERS_Q.to_string()
        }
        RAW => return raw_request(input),
        other => return Err(format!("Unknown linear action `{other}`.")),
    };
    Ok(Gql::new(&q, Value::Object(vars)))
}

fn raw_request(input: &Value) -> Result<Gql, String> {
    let q = input["query"]
        .as_str()
        .filter(|q| !q.trim().is_empty())
        .ok_or("graphql needs `query` (a GraphQL document).")?;
    let variables = match &input["variables"] {
        Value::Null => json!({}),
        v @ Value::Object(_) => v.clone(),
        // Models sometimes send the variables JSON-encoded.
        Value::String(t) if t.trim().is_empty() => json!({}),
        Value::String(t) => match serde_json::from_str::<Value>(t) {
            Ok(v @ Value::Object(_)) => v,
            _ => return Err("`variables` must be a JSON object.".into()),
        },
        _ => return Err("`variables` must be a JSON object.".into()),
    };
    let mut g = Gql::new(q, variables);
    g.operation_name = s(input, "operation_name").map(String::from);
    Ok(g)
}

// Lookups used by the write actions to turn names into IDs.

fn team_lookup(t: &str) -> Gql {
    Gql::new(TEAM_LOOKUP_Q, json!({"filter": team_filter(t)}))
}

fn user_lookup(u: &str) -> Gql {
    Gql::new(USER_LOOKUP_Q, json!({"filter": user_filter(u)}))
}

fn state_lookup(team_id: &str, name: &str) -> Gql {
    Gql::new(
        STATE_LOOKUP_Q,
        json!({"filter": {"team": {"id": {"eq": team_id}}, "name": {"eqIgnoreCase": name}}}),
    )
}

fn label_lookup(names: &[String]) -> Gql {
    let any: Vec<Value> = names
        .iter()
        .map(|n| json!({"name": {"eqIgnoreCase": n}}))
        .collect();
    Gql::new(LABEL_LOOKUP_Q, json!({"filter": {"or": any}}))
}

fn project_lookup(p: &str) -> Gql {
    Gql::new(
        PROJECT_LOOKUP_Q,
        json!({"filter": {"name": {"eqIgnoreCase": p}}}),
    )
}

/// The single matching node's `id`, or an error that says what to do.
/// `exact` picks one out of several case-insensitive matches (e.g. the team
/// whose key is exactly what was typed).
fn pick_one(
    nodes: &Value,
    kind: &str,
    wanted: &str,
    hint: &str,
    exact: &dyn Fn(&Value) -> bool,
) -> Result<String, String> {
    let list = nodes.as_array().cloned().unwrap_or_default();
    let id = |n: &Value| n["id"].as_str().map(String::from);
    match list.len() {
        0 => Err(format!("No Linear {kind} matches `{wanted}`. {hint}")),
        1 => id(&list[0]).ok_or_else(|| format!("Linear returned a {kind} without an id.")),
        _ => {
            let exacts: Vec<&Value> = list.iter().filter(|n| exact(n)).collect();
            if exacts.len() == 1 {
                return id(exacts[0])
                    .ok_or_else(|| format!("Linear returned a {kind} without an id."));
            }
            let names: Vec<String> = list
                .iter()
                .map(|n| {
                    let label = n["email"]
                        .as_str()
                        .or_else(|| n["key"].as_str())
                        .or_else(|| n["name"].as_str())
                        .unwrap_or("?");
                    format!("{label} ({})", n["id"].as_str().unwrap_or("?"))
                })
                .collect();
            Err(format!(
                "`{wanted}` matches several Linear {kind}s: {}. Pass a more specific value or the id.",
                names.join(", ")
            ))
        }
    }
}

/// Label ids for `names`: the issue's team labels first, then workspace
/// labels (no team). A label from another team cannot go on this issue.
fn pick_labels(nodes: &Value, team_id: &str, names: &[String]) -> Result<Vec<String>, String> {
    let list = nodes.as_array().cloned().unwrap_or_default();
    let mut ids = Vec::new();
    let mut missing = Vec::new();
    for n in names {
        let matching = |team_ok: &dyn Fn(&Value) -> bool| {
            list.iter()
                .find(|l| {
                    l["name"]
                        .as_str()
                        .is_some_and(|x| x.eq_ignore_ascii_case(n))
                        && team_ok(&l["team"])
                })
                .and_then(|l| l["id"].as_str().map(String::from))
        };
        let found = matching(&|t: &Value| t["id"].as_str() == Some(team_id))
            .or_else(|| matching(&|t: &Value| t.is_null()));
        match found {
            Some(id) if !ids.contains(&id) => ids.push(id),
            Some(_) => {}
            None => missing.push(n.clone()),
        }
    }
    if missing.is_empty() {
        Ok(ids)
    } else {
        Err(format!(
            "No Linear label named {} for this team (or workspace-wide). Use list_labels to see the options.",
            missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

/// A list of strings from an array or a comma-separated string.
fn str_list(v: &Value) -> Option<Vec<String>> {
    let out: Vec<String> = match v {
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
        _ => return None,
    };
    Some(out)
}

/// `priority` as Linear's 0..=4 (`None` = not given).
fn priority(v: &Value) -> Result<Option<i64>, String> {
    const BAD: &str = "`priority` must be 0 (none), 1 (urgent), 2 (high), 3 (medium) or 4 (low).";
    let n = match v {
        Value::Null => return Ok(None),
        Value::Number(n) => n.as_i64().ok_or(BAD)?,
        Value::String(t) => match t.trim().to_ascii_lowercase().as_str() {
            "none" | "no priority" => 0,
            "urgent" => 1,
            "high" => 2,
            "medium" | "normal" => 3,
            "low" => 4,
            other => other.trim_start_matches('p').parse().map_err(|_| BAD)?,
        },
        _ => return Err(BAD.into()),
    };
    if (0..=4).contains(&n) {
        Ok(Some(n))
    } else {
        Err(BAD.into())
    }
}

/// IDs already resolved for an issue create/update. `Some(None)` clears a
/// field (unassign, remove from project).
#[derive(Debug, Default)]
struct Resolved {
    team_id: Option<String>,
    state_id: Option<String>,
    assignee_id: Option<Option<String>>,
    label_ids: Option<Vec<String>>,
    project_id: Option<Option<String>>,
}

/// The `IssueCreateInput` / `IssueUpdateInput` object. Only fields the caller
/// gave are present, so an update never blanks something it did not mention.
fn issue_input(input: &Value, r: &Resolved) -> Result<Value, String> {
    let mut m = Map::new();
    if let Some(t) = &r.team_id {
        m.insert("teamId".into(), json!(t));
    }
    if let Some(t) = s(input, "title") {
        m.insert("title".into(), json!(t));
    }
    if let Some(d) = input["description"].as_str() {
        m.insert("description".into(), json!(d));
    }
    if let Some(p) = priority(&input["priority"])? {
        m.insert("priority".into(), json!(p));
    }
    if let Some(st) = &r.state_id {
        m.insert("stateId".into(), json!(st));
    }
    if let Some(a) = &r.assignee_id {
        m.insert("assigneeId".into(), json!(a));
    }
    if let Some(l) = &r.label_ids {
        m.insert("labelIds".into(), json!(l));
    }
    if let Some(p) = &r.project_id {
        m.insert("projectId".into(), json!(p));
    }
    Ok(Value::Object(m))
}

fn comment_request(issue_id: &str, body: &str) -> Gql {
    Gql::new(
        COMMENT_Q,
        json!({"input": {"issueId": issue_id, "body": body}}),
    )
}

// ───────────────────────────── responses ─────────────────────────────

const AUTH_HINT: &str = "The API key is missing, invalid or revoked — check Settings → Connectors → Linear (or LINEAR_API_KEY). Personal keys look like lin_api_….";
const ACCESS_HINT: &str = "The key's user may not have access to this team or issue, or the key's permissions do not allow this (a read-only key cannot write).";
const NOT_FOUND_HINT: &str =
    "Check the identifier or ID — list_issues, search_issues, list_teams etc. show valid ones.";

/// Redact decoded strings and object keys before formatting or clipping can
/// split a credential. Replacing serialized JSON alone misses escaped tokens.
fn redact_value(value: Value, secrets: &[&str]) -> Value {
    match value {
        Value::String(s) => Value::String(plugin_services::redact(&s, secrets)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| redact_value(v, secrets))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(k, v)| {
                    (
                        plugin_services::redact(&k, secrets),
                        redact_value(v, secrets),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

/// Turn a reply into the `data` object, or a clear, token-free error. Linear
/// reports most failures as HTTP 200 (or 400) with an `errors` array, so a 2xx
/// status alone means nothing.
fn interpret(status: u16, body: &str, what: &str, secrets: &[&str]) -> Result<Value, String> {
    let v = redact_value(serde_json::from_str(body).unwrap_or(Value::Null), secrets);
    let body = plugin_services::redact(body, secrets);
    let errors = v["errors"].as_array().filter(|e| !e.is_empty());
    let code_of = |e: &Value| e["extensions"]["code"].as_str().unwrap_or("").to_string();
    let hint_for = |codes: &[String], msgs: &str| -> &'static str {
        let low = msgs.to_ascii_lowercase();
        if status == 401 || codes.iter().any(|c| c == "AUTHENTICATION_ERROR") {
            AUTH_HINT
        } else if status == 403 || codes.iter().any(|c| c == "FORBIDDEN") {
            ACCESS_HINT
        } else if status == 404 || low.contains("not found") {
            NOT_FOUND_HINT
        } else {
            ""
        }
    };
    if let Some(errs) = errors {
        let codes: Vec<String> = errs.iter().map(code_of).collect();
        if codes.iter().any(|c| c == "RATELIMITED") {
            return Err(plugin_services::rate_limited("Linear", None));
        }
        let msgs = errs
            .iter()
            .map(|e| {
                let m = e["message"].as_str().unwrap_or("unknown error");
                match e["extensions"]["userPresentableMessage"].as_str() {
                    Some(u) if u != m => format!("{m} ({u})"),
                    _ => m.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        let hint = hint_for(&codes, &msgs);
        if (200..300).contains(&status) {
            return Err(plugin_services::redact(
                &format!(
                    "Linear GraphQL error on {what}: {}{}{hint}",
                    plugin_services::clip(&msgs, 600),
                    if hint.is_empty() { "" } else { " " }
                ),
                secrets,
            ));
        }
        return Err(plugin_services::http_error(
            "Linear", status, what, &msgs, hint, secrets,
        ));
    }
    if !(200..300).contains(&status) {
        let hint = hint_for(&[], "");
        return Err(plugin_services::http_error(
            "Linear",
            status,
            what,
            &plugin_services::clip(&body, 400),
            hint,
            secrets,
        ));
    }
    match v.get("data") {
        Some(d) if !d.is_null() => Ok(d.clone()),
        _ => Err(plugin_services::redact(
            &format!(
                "Linear sent no data for {what}: {}",
                plugin_services::clip(body.trim(), 400)
            ),
            secrets,
        )),
    }
}

fn str_or<'a>(v: &'a Value, d: &'a str) -> &'a str {
    v.as_str().filter(|x| !x.is_empty()).unwrap_or(d)
}

fn person(u: &Value) -> Option<String> {
    u["displayName"]
        .as_str()
        .or_else(|| u["name"].as_str())
        .filter(|x| !x.is_empty())
        .map(|x| format!("@{x}"))
}

fn label_names(i: &Value) -> Vec<&str> {
    i["labels"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
        .collect()
}

/// `ENG-123 Title [State] · P2 · @assignee · {labels} · url`
fn issue_row(i: &Value) -> String {
    let labels = label_names(i);
    let title = str_or(&i["title"], "");
    let mut out = format!(
        "{}{}{title} [{}] · P{} · {}",
        str_or(&i["identifier"], "?"),
        if title.is_empty() { "" } else { " " },
        str_or(&i["state"]["name"], "?"),
        i["priority"].as_i64().unwrap_or(0),
        person(&i["assignee"]).unwrap_or_else(|| "unassigned".into()),
    );
    if !labels.is_empty() {
        out.push_str(&format!(" · {{{}}}", labels.join(", ")));
    }
    if let Some(u) = i["url"].as_str() {
        out.push_str(&format!(" · {u}"));
    }
    out
}

/// `nodes` one per line, plus a "more available" line with the cursor.
fn list(conn: &Value, noun: &str, empty: &str, row: &dyn Fn(&Value) -> String) -> String {
    let nodes = conn["nodes"].as_array().cloned().unwrap_or_default();
    if nodes.is_empty() {
        return empty.to_string();
    }
    let mut out = format!("{} {noun}:\n", nodes.len());
    for n in &nodes {
        out.push_str(&row(n));
        out.push('\n');
    }
    if conn["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
        match conn["pageInfo"]["endCursor"].as_str() {
            Some(c) => out.push_str(&format!("… more available — pass after: \"{c}\"\n")),
            None => out.push_str("… more available\n"),
        }
    }
    out.trim_end().to_string()
}

fn identity_of(d: &Value) -> String {
    let v = &d["viewer"];
    let name = v["name"]
        .as_str()
        .or_else(|| v["displayName"].as_str())
        .unwrap_or("?");
    let mut out = name.to_string();
    if let Some(e) = v["email"].as_str().filter(|e| !e.is_empty()) {
        out.push_str(&format!(" <{e}>"));
    }
    if let Some(o) = d["organization"]["name"].as_str() {
        out.push_str(&format!(" · {o}"));
    }
    out
}

fn date(v: &Value) -> String {
    v.as_str().unwrap_or("").chars().take(16).collect()
}

fn issue_detail(i: &Value) -> String {
    if i.is_null() {
        return "No such issue.".into();
    }
    let mut out = format!(
        "{} {}\n{}\nState: {} · Priority: {} ({}) · Assignee: {} · Team: {}",
        str_or(&i["identifier"], "?"),
        str_or(&i["title"], ""),
        str_or(&i["url"], ""),
        str_or(&i["state"]["name"], "?"),
        i["priority"].as_i64().unwrap_or(0),
        str_or(&i["priorityLabel"], "?"),
        person(&i["assignee"]).unwrap_or_else(|| "unassigned".into()),
        str_or(&i["team"]["key"], "?"),
    );
    let labels = label_names(i);
    if !labels.is_empty() {
        out.push_str(&format!("\nLabels: {}", labels.join(", ")));
    }
    if let Some(p) = i["project"]["name"].as_str() {
        out.push_str(&format!("\nProject: {p}"));
    }
    if !i["cycle"].is_null() {
        out.push_str(&format!(
            "\nCycle: {}{}",
            i["cycle"]["number"],
            i["cycle"]["name"]
                .as_str()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default()
        ));
    }
    if let Some(p) = i["parent"]["identifier"].as_str() {
        out.push_str(&format!(
            "\nParent: {p} {}",
            str_or(&i["parent"]["title"], "")
        ));
    }
    if let Some(d) = i["dueDate"].as_str() {
        out.push_str(&format!("\nDue: {d}"));
    }
    if let Some(e) = i["estimate"].as_f64() {
        out.push_str(&format!("\nEstimate: {e}"));
    }
    out.push_str(&format!(
        "\nCreated {} by {} · updated {}",
        date(&i["createdAt"]),
        person(&i["creator"]).unwrap_or_else(|| "?".into()),
        date(&i["updatedAt"])
    ));
    let desc = i["description"].as_str().unwrap_or("").trim();
    out.push_str("\n\n");
    out.push_str(&if desc.is_empty() {
        "(no description)".to_string()
    } else {
        plugin_services::clip(desc, BODY_CLIP)
    });
    let mut comments = i["comments"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    // Oldest first, like a conversation; the API's order is not guaranteed.
    comments.sort_by(|a, b| {
        a["createdAt"]
            .as_str()
            .unwrap_or("")
            .cmp(b["createdAt"].as_str().unwrap_or(""))
    });
    if !comments.is_empty() {
        out.push_str(&format!("\n\nComments ({}):\n", comments.len()));
        for c in &comments {
            out.push_str(&format!(
                "── {} · {}\n{}\n",
                person(&c["user"]).unwrap_or_else(|| "?".into()),
                date(&c["createdAt"]),
                plugin_services::clip(c["body"].as_str().unwrap_or("").trim(), COMMENT_CLIP)
            ));
        }
        if i["comments"]["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
            out.push_str("… older/newer comments not shown (first 50 only)\n");
        }
    }
    out.trim_end().to_string()
}

fn team_row(t: &Value) -> String {
    let mut out = format!(
        "{} · {} · id {}",
        str_or(&t["key"], "?"),
        str_or(&t["name"], ""),
        str_or(&t["id"], "?")
    );
    if let Some(d) = t["description"].as_str().filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!(" · {}", plugin_services::clip(d.trim(), 200)));
    }
    out
}

fn state_row(s: &Value) -> String {
    format!(
        "{} · {} ({}) · id {}",
        str_or(&s["team"]["key"], "?"),
        str_or(&s["name"], "?"),
        str_or(&s["type"], "?"),
        str_or(&s["id"], "?")
    )
}

fn project_row(p: &Value) -> String {
    let mut out = format!(
        "{} [{}]",
        str_or(&p["name"], "?"),
        str_or(&p["status"]["name"], "?")
    );
    if let Some(pr) = p["progress"].as_f64() {
        out.push_str(&format!(" · {:.0}%", pr * 100.0));
    }
    if let Some(l) = person(&p["lead"]) {
        out.push_str(&format!(" · lead {l}"));
    }
    if let Some(t) = p["targetDate"].as_str() {
        out.push_str(&format!(" · target {t}"));
    }
    let teams: Vec<&str> = p["teams"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["key"].as_str())
        .collect();
    if !teams.is_empty() {
        out.push_str(&format!(" · teams {}", teams.join(", ")));
    }
    if let Some(u) = p["url"].as_str() {
        out.push_str(&format!(" · {u}"));
    }
    out.push_str(&format!(" · id {}", str_or(&p["id"], "?")));
    out
}

fn label_row(l: &Value) -> String {
    let name = match l["parent"]["name"].as_str() {
        Some(g) => format!("{g}/{}", str_or(&l["name"], "?")),
        None => str_or(&l["name"], "?").to_string(),
    };
    format!(
        "{name} ({}) · id {}",
        l["team"]["key"].as_str().unwrap_or("workspace"),
        str_or(&l["id"], "?")
    )
}

fn user_row(u: &Value) -> String {
    let mut out = str_or(&u["name"], "?").to_string();
    if let Some(d) = u["displayName"].as_str().filter(|d| !d.is_empty()) {
        out.push_str(&format!(" @{d}"));
    }
    if let Some(e) = u["email"].as_str().filter(|e| !e.is_empty()) {
        out.push_str(&format!(" <{e}>"));
    }
    if u["admin"].as_bool() == Some(true) {
        out.push_str(" · admin");
    }
    if u["active"].as_bool() == Some(false) {
        out.push_str(" · deactivated");
    }
    out.push_str(&format!(" · id {}", str_or(&u["id"], "?")));
    out
}

/// Plain text for a read action's `data`.
fn format_read(action: &str, d: &Value) -> String {
    match action {
        "whoami" => format!(
            "{}\nuser id {} · workspace {}",
            identity_of(d),
            str_or(&d["viewer"]["id"], "?"),
            str_or(&d["organization"]["urlKey"], "?")
        ),
        "list_issues" => list(
            &d["issues"],
            "issues (most recently updated first)",
            "No issues match.",
            &issue_row,
        ),
        "search_issues" => list(
            &d["searchIssues"],
            "issues",
            "No issues match the search.",
            &issue_row,
        ),
        "get_issue" => issue_detail(&d["issue"]),
        "list_teams" => list(&d["teams"], "teams", "No teams.", &team_row),
        "list_states" => {
            let mut conn = d["workflowStates"].clone();
            if let Some(nodes) = conn["nodes"].as_array_mut() {
                nodes.sort_by(|a, b| {
                    let k = |x: &Value| {
                        (
                            x["team"]["key"].as_str().unwrap_or("").to_string(),
                            x["position"].as_f64().unwrap_or(0.0),
                        )
                    };
                    let (ta, pa) = k(a);
                    let (tb, pb) = k(b);
                    ta.cmp(&tb).then(pa.total_cmp(&pb))
                });
            }
            list(&conn, "workflow states", "No workflow states.", &state_row)
        }
        "list_projects" => list(
            &d["projects"],
            "projects",
            "No projects match.",
            &project_row,
        ),
        "list_labels" => list(&d["issueLabels"], "labels", "No labels.", &label_row),
        "list_users" => list(&d["users"], "users", "No users match.", &user_row),
        _ => plugin_services::clip(
            &serde_json::to_string_pretty(d).unwrap_or_default(),
            RAW_CLIP,
        ),
    }
}

/// `Created ENG-124 …` / `Updated ENG-124 …` from a mutation payload.
fn format_issue_write(verb: &str, payload: &Value) -> Result<String, String> {
    if payload["success"].as_bool() != Some(true) {
        return Err(format!(
            "Linear did not confirm the {verb} (success = false)."
        ));
    }
    Ok(format!("{verb} {}", issue_row(&payload["issue"])))
}

// ───────────────────────────── network ─────────────────────────────

struct Lin<'a> {
    http: &'a reqwest::Client,
    c: Creds,
}

impl Lin<'_> {
    async fn gql(&self, what: &str, q: &Gql) -> Result<Value, String> {
        let secrets = [self.c.token.as_str()];
        let mut auth = reqwest::header::HeaderValue::from_str(&self.c.auth).map_err(|_| {
            "The Linear API key contains characters that cannot be sent in a header — re-paste it in Settings → Connectors → Linear.".to_string()
        })?;
        // Keeps the key out of reqwest's own Debug output.
        auth.set_sensitive(true);
        let rq = self
            .http
            .post(ENDPOINT)
            .header(reqwest::header::AUTHORIZATION, auth)
            .header("user-agent", "OpenLeash/0.1")
            .json(&q.body());
        let r = plugin_services::send("Linear", rq, &secrets).await?;
        interpret(r.status, &r.body, what, &secrets)
    }

    async fn team_id(&self, t: &str) -> Result<String, String> {
        let d = self.gql("team lookup", &team_lookup(t)).await?;
        pick_one(
            &d["teams"]["nodes"],
            "team",
            t,
            "Use list_teams to see the teams.",
            &|n| n["key"].as_str().is_some_and(|k| k.eq_ignore_ascii_case(t)),
        )
    }

    /// `Some(None)` for "none" (unassign).
    async fn assignee_id(&self, u: Option<&str>) -> Result<Option<Option<String>>, String> {
        let Some(u) = u else { return Ok(None) };
        if is_none_word(u) {
            return Ok(Some(None));
        }
        let d = self.gql("user lookup", &user_lookup(u)).await?;
        pick_one(
            &d["users"]["nodes"],
            "user",
            u,
            "Use list_users to see who is in the workspace.",
            &|n| {
                n["displayName"]
                    .as_str()
                    .is_some_and(|x| x.eq_ignore_ascii_case(u))
            },
        )
        .map(|id| Some(Some(id)))
    }

    async fn state_id(&self, team_id: &str, st: &str) -> Result<String, String> {
        let d = self.gql("state lookup", &state_lookup(team_id, st)).await?;
        pick_one(
            &d["workflowStates"]["nodes"],
            "workflow state",
            st,
            "Use list_states with this team to see its states.",
            &|n| n["name"].as_str() == Some(st),
        )
    }

    async fn label_ids(&self, team_id: &str, names: &[String]) -> Result<Vec<String>, String> {
        if names.is_empty() {
            return Ok(vec![]);
        }
        let d = self.gql("label lookup", &label_lookup(names)).await?;
        pick_labels(&d["issueLabels"]["nodes"], team_id, names)
    }

    async fn project_id(&self, p: Option<&str>) -> Result<Option<Option<String>>, String> {
        let Some(p) = p else { return Ok(None) };
        if is_none_word(p) {
            return Ok(Some(None));
        }
        if is_uuid(p) {
            return Ok(Some(Some(p.to_string())));
        }
        let d = self.gql("project lookup", &project_lookup(p)).await?;
        pick_one(
            &d["projects"]["nodes"],
            "project",
            p,
            "Use list_projects to see the projects.",
            &|n| n["name"].as_str() == Some(p),
        )
        .map(|id| Some(Some(id)))
    }

    /// `(uuid, identifier, team id, current label ids)` of an issue.
    async fn issue_ref(&self, id: &str) -> Result<(String, String, String, Vec<String>), String> {
        let d = self
            .gql(
                "issue lookup",
                &Gql::new(ISSUE_REF_Q, json!({"id": issue_ref(id)})),
            )
            .await?;
        let i = &d["issue"];
        let uuid = i["id"]
            .as_str()
            .ok_or_else(|| format!("No Linear issue `{id}`. {NOT_FOUND_HINT}"))?;
        let labels = i["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["id"].as_str().map(String::from))
            .collect();
        Ok((
            uuid.to_string(),
            str_or(&i["identifier"], id).to_string(),
            str_or(&i["team"]["id"], "").to_string(),
            labels,
        ))
    }

    async fn run(&self, input: &Value) -> Result<String, String> {
        let action = input["action"].as_str().ok_or("`action` is required.")?;
        match action {
            "create_issue" => {
                let team = s(input, "team")
                    .ok_or("create_issue needs `team` (key like ENG, or name — see list_teams).")?;
                if s(input, "title").is_none() {
                    return Err("create_issue needs `title`.".into());
                }
                priority(&input["priority"])?;
                let team_id = self.team_id(team).await?;
                let mut r = Resolved {
                    assignee_id: self.assignee_id(s(input, "assignee")).await?,
                    project_id: self.project_id(s(input, "project")).await?,
                    ..Default::default()
                };
                if let Some(st) = s(input, "state") {
                    r.state_id = Some(self.state_id(&team_id, st).await?);
                }
                if let Some(names) = str_list(&input["labels"]).filter(|l| !l.is_empty()) {
                    r.label_ids = Some(self.label_ids(&team_id, &names).await?);
                }
                r.team_id = Some(team_id);
                let mut i = input.clone();
                if i["description"].is_null() {
                    // `body` is the GitHub plugin's name for the same thing.
                    i["description"] = input["body"].clone();
                }
                let vars = json!({"input": issue_input(&i, &r)?});
                let d = self
                    .gql("issueCreate", &Gql::new(&create_issue_q(), vars))
                    .await?;
                format_issue_write("Created", &d["issueCreate"])
            }
            "update_issue" => {
                let id = s(input, "issue")
                    .or_else(|| s(input, "id"))
                    .ok_or("update_issue needs `issue` (ENG-123 or a UUID).")?;
                priority(&input["priority"])?;
                let labels = str_list(&input["labels"]);
                let add = str_list(&input["add_labels"]).unwrap_or_default();
                let remove = str_list(&input["remove_labels"]).unwrap_or_default();
                let touches_labels = labels.is_some() || !add.is_empty() || !remove.is_empty();
                let empty = issue_input(input, &Resolved::default())?
                    .as_object()
                    .is_some_and(|m| m.is_empty());
                if empty
                    && s(input, "state").is_none()
                    && s(input, "assignee").is_none()
                    && s(input, "project").is_none()
                    && !touches_labels
                {
                    return Err("update_issue needs at least one of title, description, state, assignee, priority, labels, add_labels, remove_labels, project.".into());
                }
                let (uuid, _ident, team_id, current) = self.issue_ref(id).await?;
                let mut r = Resolved {
                    assignee_id: self.assignee_id(s(input, "assignee")).await?,
                    project_id: self.project_id(s(input, "project")).await?,
                    ..Default::default()
                };
                if let Some(st) = s(input, "state") {
                    r.state_id = Some(self.state_id(&team_id, st).await?);
                }
                if touches_labels {
                    let mut ids = match &labels {
                        Some(l) => self.label_ids(&team_id, l).await?,
                        None => current,
                    };
                    for a in self.label_ids(&team_id, &add).await? {
                        if !ids.contains(&a) {
                            ids.push(a);
                        }
                    }
                    let gone = self.label_ids(&team_id, &remove).await?;
                    ids.retain(|x| !gone.contains(x));
                    r.label_ids = Some(ids);
                }
                let vars = json!({"id": uuid, "input": issue_input(input, &r)?});
                let d = self
                    .gql("issueUpdate", &Gql::new(&update_issue_q(), vars))
                    .await?;
                format_issue_write("Updated", &d["issueUpdate"])
            }
            "comment" => {
                let id = s(input, "issue")
                    .or_else(|| s(input, "id"))
                    .ok_or("comment needs `issue` (ENG-123 or a UUID).")?;
                let body = input["body"]
                    .as_str()
                    .filter(|b| !b.trim().is_empty())
                    .ok_or("comment needs `body` (markdown).")?;
                let (uuid, ident, _, _) = self.issue_ref(id).await?;
                let d = self
                    .gql("commentCreate", &comment_request(&uuid, body))
                    .await?;
                let p = &d["commentCreate"];
                if p["success"].as_bool() != Some(true) {
                    return Err("Linear did not confirm the comment (success = false).".into());
                }
                Ok(format!(
                    "Commented on {ident} · {}",
                    str_or(&p["comment"]["url"], "")
                ))
            }
            _ => {
                let q = read_request(action, input)?;
                let what = if action == RAW {
                    "graphql".to_string()
                } else {
                    action.to_string()
                };
                let d = self.gql(&what, &q).await?;
                Ok(format_read(action, &d))
            }
        }
    }
}

pub async fn run(http: &reqwest::Client, cfg: &LinearCfg, input: &Value) -> Result<String, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let lin = Lin { http, c };
    let secrets = [lin.c.token.as_str()];
    lin.run(input)
        .await
        .map(|out| plugin_services::clip(&plugin_services::redact(&out, &secrets), RESULT_CLIP))
        .map_err(|e| plugin_services::redact(&e, &secrets))
}

pub async fn whoami(http: &reqwest::Client, cfg: &LinearCfg) -> Result<Identity, String> {
    let c = creds(cfg, &plugin_services::real_env)?;
    let source = c.source.to_string();
    let lin = Lin { http, c };
    let secrets = [lin.c.token.as_str()];
    let d = lin
        .gql("whoami", &Gql::new(WHOAMI_Q, json!({})))
        .await
        .map_err(|e| plugin_services::redact(&e, &secrets))?;
    Ok(Identity {
        identity: plugin_services::redact(&identity_of(&d), &secrets),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "lin_api_DUMMYdummy0123456789";

    // ── classification ──

    #[test]
    fn every_action_is_classified() {
        for a in READS {
            assert!(!is_write(&json!({"action": a})), "{a} is a read");
        }
        for a in WRITES {
            assert!(is_write(&json!({"action": a})), "{a} is a write");
        }
        assert!(is_write(&json!({"action": "delete_issue"})));
        assert!(is_write(&json!({})));
        assert!(is_write(&json!({"action": 3})));
        assert!(!is_write(
            &json!({"action": "graphql", "query": "{ viewer { id } }"})
        ));
        assert!(is_write(
            &json!({"action": "graphql", "query": "mutation { issueDelete(id: \"x\") { success } }"})
        ));
        assert!(is_write(&json!({"action": "graphql"})), "missing query");
        assert!(is_write(&json!({"action": "graphql", "query": 1})));
        assert!(is_write(
            &json!({"action": "graphql", "query": ["{ viewer { id } }"]})
        ));
        // operationName cannot turn a mutation document into a read.
        assert!(is_write(
            &json!({"action": "graphql", "operation_name": "A",
            "query": "query A { viewer { id } } mutation B { x }"})
        ));
    }

    #[test]
    fn schema_enum_matches_reads_and_writes() {
        let s = schema();
        assert_eq!(s["name"], "linear");
        assert_eq!(s["input_schema"]["required"], json!(["action"]));
        let e: Vec<&str> = s["input_schema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for a in &e {
            assert!(
                READS.contains(a) || WRITES.contains(a) || *a == RAW,
                "{a} is not classified"
            );
        }
        for a in READS.iter().chain(WRITES).chain([RAW].iter()) {
            assert!(e.contains(a), "{a} missing from the enum");
        }
        let desc = s["description"].as_str().unwrap();
        for a in &e {
            assert!(desc.contains(a), "description does not mention {a}");
        }
        // Every param the code reads is documented.
        let props = s["input_schema"]["properties"].as_object().unwrap();
        for p in [
            "issue",
            "team",
            "title",
            "description",
            "body",
            "state",
            "assignee",
            "priority",
            "label",
            "labels",
            "add_labels",
            "remove_labels",
            "project",
            "query",
            "variables",
            "operation_name",
            "include_inactive",
            "limit",
            "after",
        ] {
            assert!(props.contains_key(p), "{p} undocumented");
        }
    }

    #[test]
    fn graphql_reads() {
        for q in [
            "{ viewer { id } }",
            "query { viewer { id } }",
            "query Me { viewer { id name } }",
            "  query Me($n: Int = 5) { issues(first: $n) { nodes { id } } }",
            // `mutation` only inside a string argument.
            r#"query { issues(filter:{title:{eq:"mutation"}}) { nodes { id } } }"#,
            // Comment-only mention.
            "# mutation { issueDelete }\nquery { viewer { id } }",
            "query { viewer { id } } # mutation B { x }",
            // At depth > 0: aliases, fields, arguments.
            "{ x: mutation }",
            "{ mutation: viewer { id } subscription { id } }",
            "query { issues(filter: {mutation: true}) { nodes { id } } }",
            // Escaped quotes inside strings.
            r#"query { issues(filter:{title:{eq:"say \"}\" mutation {"}}) { nodes { id } } }"#,
            r#"query { a(s: "back\\") { b } }"#,
            // Block strings containing braces and keywords.
            "query { a(s: \"\"\"} mutation { issueDelete \"\"\") { b } }",
            "query { a(s: \"\"\"escaped \\\"\"\" still } mutation {\"\"\") { b } }",
            // Several queries and fragments.
            "query A { viewer { ...F } } query B { teams { nodes { id } } } fragment F on User { id }",
            "fragment F on Mutation { x } query { viewer { id } }",
            // Default object values in variable definitions.
            "query Q($f: IssueFilter = {title: {eq: \"}\"}}) { issues(filter: $f) { nodes { id } } }",
            "query @dir(a: \"mutation\") { viewer { id } }",
            "query\r\n{\tviewer { id }\r\n}",
            "\u{feff}{ viewer { id } }",
        ] {
            assert!(!graphql_is_mutation(q), "should be a read: {q}");
        }
    }

    #[test]
    fn graphql_writes_and_fail_closed() {
        for q in [
            "mutation { issueDelete(id: \"x\") { success } }",
            "  mutation  M { x }",
            "subscription { issueUpdates { id } }",
            "query A { viewer { id } } mutation B { issueDelete(id: \"x\") { success } }",
            "query A { viewer { id } }\nmutation{x}",
            "{ viewer { id } } mutation { x }",
            // Case matters: `Mutation`/`MUTATION`/`Query` are not query keywords.
            "Mutation { x }",
            "MUTATION { x }",
            "Query { viewer { id } }",
            // Type-system definitions are not executable reads.
            "type X { a: Int }",
            "schema { query: Q mutation: M }",
            "extend type Query { a: Int }",
            // Named `mutation` — not worth the nuance.
            "query mutation { viewer { id } }",
            // Unparseable.
            "",
            "   ",
            "# only a comment",
            "query { viewer { id }",
            "query { viewer { id } } }",
            "query { a(x: 1] }",
            r#"query { a(s: "unterminated) { b } }"#,
            "query { a(s: \"\"\"never closed) { b } }",
            "query { a(s: \"line\nbreak\") { b } }",
            r#"query { a(s: "ends with backslash \"#,
            "query",
            "query A",
        ] {
            assert!(graphql_is_mutation(q), "should be a write: {q:?}");
        }
        // The comment that hides a mutation keyword ends at a line break.
        assert!(graphql_is_mutation(
            "query { viewer { id } } # x\rmutation { y }"
        ));
    }

    // ── credentials ──

    #[test]
    fn creds_settings_beat_env_and_missing_is_helpful() {
        let env = |k: &str| (k == "LINEAR_API_KEY").then(|| "lin_api_fromenv123".to_string());
        let cfg = LinearCfg {
            enabled: true,
            token: format!("  {KEY} "),
        };
        let c = creds(&cfg, &env).unwrap();
        assert_eq!((c.token.as_str(), c.source), (KEY, "settings"));
        assert_eq!(c.auth, KEY, "personal keys are sent bare");

        let c = creds(&LinearCfg::default(), &env).unwrap();
        assert_eq!(
            (c.token.as_str(), c.source),
            ("lin_api_fromenv123", "LINEAR_API_KEY")
        );

        let blank = |_: &str| Some("   ".to_string());
        for e in [
            creds(&LinearCfg::default(), &|_: &str| None).err().unwrap(),
            creds(&LinearCfg::default(), &blank).err().unwrap(),
            creds(
                &LinearCfg {
                    enabled: true,
                    token: "Bearer   ".into(),
                },
                &|_: &str| None,
            )
            .err()
            .unwrap(),
        ] {
            assert!(e.contains("Settings → Connectors → Linear"), "{e}");
            assert!(e.contains("LINEAR_API_KEY"), "{e}");
        }
    }

    #[test]
    fn auth_header_handles_bearer_and_oauth() {
        assert_eq!(auth_header(KEY), (KEY.into(), KEY.into()));
        // A pasted "Bearer" is wrong for a personal key: strip it.
        assert_eq!(
            auth_header(&format!("Bearer {KEY}")),
            (KEY.into(), KEY.into())
        );
        assert_eq!(
            auth_header(&format!("bearer   {KEY}")),
            (KEY.into(), KEY.into())
        );
        assert_eq!(
            auth_header("lin_oauth_abc123def"),
            (
                "lin_oauth_abc123def".into(),
                "Bearer lin_oauth_abc123def".into()
            )
        );
        assert_eq!(
            auth_header("Bearer someoauthtoken99"),
            ("someoauthtoken99".into(), "Bearer someoauthtoken99".into())
        );
        assert_eq!(
            auth_header("plainkey12345"),
            ("plainkey12345".into(), "plainkey12345".into())
        );
        // Non-ASCII near the prefix must not panic on slicing.
        assert_eq!(auth_header("béarer x").0, "béarer x");
        assert_eq!(auth_header("bearerish_token").0, "bearerish_token");
        assert_eq!(auth_header("Bearer").0, "");
    }

    #[test]
    fn debug_never_prints_the_key() {
        let cfg = LinearCfg {
            enabled: true,
            token: KEY.into(),
        };
        assert!(!format!("{cfg:?}").contains(KEY));
    }

    // ── request building ──

    #[test]
    fn list_issues_filters_go_in_variables() {
        let evil = "ENG\") { nodes { id } } mutation { x";
        let g = read_request(
            "list_issues",
            &json!({"action": "list_issues", "team": evil, "state": "started",
                "assignee": "me", "label": "Bug", "project": "Q3 \"Launch\"", "limit": 500,
                "after": "cursor-1"}),
        )
        .unwrap();
        assert!(!g.query.contains(evil) && !g.query.contains("Launch"));
        assert!(g.query.contains("orderBy: updatedAt"));
        assert!(!graphql_is_mutation(&g.query));
        let v = &g.variables;
        assert_eq!(v["first"], 100, "limit is clamped");
        assert_eq!(v["after"], "cursor-1");
        assert_eq!(v["filter"]["team"]["or"][0]["key"]["eqIgnoreCase"], evil);
        assert_eq!(v["filter"]["state"]["or"][1]["type"]["eq"], "started");
        assert_eq!(v["filter"]["assignee"], json!({"isMe": {"eq": true}}));
        assert_eq!(
            v["filter"]["labels"],
            json!({"some": {"name": {"eqIgnoreCase": "Bug"}}})
        );
        assert_eq!(
            v["filter"]["project"]["name"]["eqIgnoreCase"],
            "Q3 \"Launch\""
        );
        // The body is real JSON with the value intact.
        let body = serde_json::to_string(&g.body()).unwrap();
        let back: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            back["variables"]["filter"]["team"]["or"][0]["key"]["eqIgnoreCase"],
            evil
        );

        let g = read_request("list_issues", &json!({"limit": 0})).unwrap();
        assert_eq!(g.variables, json!({"first": 1}));
        let g = read_request("list_issues", &json!({})).unwrap();
        assert_eq!(g.variables, json!({"first": 20}));
        let g = read_request(
            "list_issues",
            &json!({"assignee": "ada@example.com", "state": "In Review"}),
        )
        .unwrap();
        assert_eq!(
            g.variables["filter"]["assignee"],
            json!({"email": {"eqIgnoreCase": "ada@example.com"}})
        );
        assert_eq!(
            g.variables["filter"]["state"],
            json!({"name": {"eqIgnoreCase": "In Review"}})
        );
        let g = read_request("list_issues", &json!({"assignee": "none"})).unwrap();
        assert_eq!(g.variables["filter"]["assignee"], json!({"null": true}));
        let g = read_request("list_issues", &json!({"assignee": "Ada"})).unwrap();
        assert_eq!(
            g.variables["filter"]["assignee"]["or"][1]["displayName"]["eqIgnoreCase"],
            "Ada"
        );
    }

    #[test]
    fn other_read_requests() {
        let g = read_request(
            "search_issues",
            &json!({"query": "crash \"on\" save", "team": "ENG"}),
        )
        .unwrap();
        assert!(g.query.contains("searchIssues(term: $term"));
        assert_eq!(g.variables["term"], "crash \"on\" save");
        assert!(g.variables["filter"]["team"].is_object());
        assert!(read_request("search_issues", &json!({"query": "  "})).is_err());

        let g = read_request("get_issue", &json!({"issue": "eng-123"})).unwrap();
        assert_eq!(g.variables, json!({"id": "ENG-123"}));
        let uuid = "8f3c1a2b-0d4e-4f5a-9b6c-7d8e9f0a1b2c";
        let g = read_request("get_issue", &json!({"issue": uuid})).unwrap();
        assert_eq!(g.variables, json!({"id": uuid}));
        assert!(read_request("get_issue", &json!({})).is_err());

        let g = read_request("list_states", &json!({"team": uuid})).unwrap();
        assert_eq!(g.variables["filter"], json!({"team": {"id": {"eq": uuid}}}));
        let g = read_request("list_projects", &json!({"team": "ENG", "query": "launch"})).unwrap();
        assert_eq!(
            g.variables["filter"]["and"][1],
            json!({"name": {"containsIgnoreCase": "launch"}})
        );
        let g = read_request("list_labels", &json!({"team": "ENG"})).unwrap();
        assert_eq!(
            g.variables["filter"]["or"][1],
            json!({"team": {"null": true}})
        );
        assert_eq!(g.variables["first"], 100);
        let g = read_request(
            "list_users",
            &json!({"query": "ada", "include_inactive": true}),
        )
        .unwrap();
        assert_eq!(g.variables["includeDisabled"], true);
        assert!(g.variables["filter"]["and"][0]["or"].is_array());
        let g = read_request("list_teams", &json!({"limit": "5"})).unwrap();
        assert_eq!(g.variables["first"], 5);
        let g = read_request("whoami", &json!({})).unwrap();
        assert!(g.query.contains("viewer") && g.query.contains("organization"));
        assert!(read_request("nope", &json!({})).is_err());
        // Every built-in read document is classified as a read.
        for a in READS {
            let g = read_request(a, &json!({"query": "x", "issue": "ENG-1"})).unwrap();
            assert!(!graphql_is_mutation(&g.query), "{a}: {}", g.query);
        }
        for q in [create_issue_q(), update_issue_q(), COMMENT_Q.to_string()] {
            assert!(graphql_is_mutation(&q));
        }
    }

    #[test]
    fn raw_graphql_request() {
        let g = read_request(
            "graphql",
            &json!({"query": "query($id: String!) { issue(id: $id) { title } }",
                "variables": {"id": "ENG-1"}, "operation_name": "Q"}),
        )
        .unwrap();
        assert_eq!(
            g.body(),
            json!({"query": "query($id: String!) { issue(id: $id) { title } }",
                "variables": {"id": "ENG-1"}, "operationName": "Q"})
        );
        let g = read_request(
            "graphql",
            &json!({"query": "{ viewer { id } }", "variables": "{\"a\":1}"}),
        )
        .unwrap();
        assert_eq!(g.variables, json!({"a": 1}));
        assert!(g.body().get("operationName").is_none());
        assert!(read_request("graphql", &json!({"query": "{ a }", "variables": [1]})).is_err());
        assert!(read_request("graphql", &json!({"query": "{ a }", "variables": "[1]"})).is_err());
        assert!(read_request("graphql", &json!({"query": ""})).is_err());
        assert!(read_request("graphql", &json!({})).is_err());
    }

    #[test]
    fn lookups_and_write_inputs() {
        let g = team_lookup("Engineering");
        assert_eq!(
            g.variables["filter"]["or"][1]["name"]["eqIgnoreCase"],
            "Engineering"
        );
        assert_eq!(
            user_lookup("me").variables["filter"],
            json!({"isMe": {"eq": true}})
        );
        let g = state_lookup("team-1", "In Progress");
        assert_eq!(
            g.variables["filter"],
            json!({"team": {"id": {"eq": "team-1"}}, "name": {"eqIgnoreCase": "In Progress"}})
        );
        let g = label_lookup(&["Bug".into(), "P\"1".into()]);
        assert_eq!(
            g.variables["filter"]["or"][1]["name"]["eqIgnoreCase"],
            "P\"1"
        );
        assert_eq!(
            project_lookup("Q3").variables["filter"],
            json!({"name": {"eqIgnoreCase": "Q3"}})
        );

        let r = Resolved {
            team_id: Some("t1".into()),
            state_id: Some("s1".into()),
            assignee_id: Some(Some("u1".into())),
            label_ids: Some(vec!["l1".into(), "l2".into()]),
            project_id: Some(None),
        };
        let v = issue_input(
            &json!({"title": " Crash \"}\" ", "description": "Steps:\n1. `x`", "priority": "high"}),
            &r,
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"teamId": "t1", "title": "Crash \"}\"", "description": "Steps:\n1. `x`",
                "priority": 2, "stateId": "s1", "assigneeId": "u1",
                "labelIds": ["l1", "l2"], "projectId": null})
        );
        // An update with only a title touches nothing else.
        let v = issue_input(&json!({"title": "New"}), &Resolved::default()).unwrap();
        assert_eq!(v, json!({"title": "New"}));
        let r = Resolved {
            assignee_id: Some(None),
            ..Default::default()
        };
        assert_eq!(
            issue_input(&json!({}), &r).unwrap(),
            json!({"assigneeId": null})
        );
        assert!(issue_input(&json!({"priority": 7}), &Resolved::default()).is_err());

        let c = comment_request("uuid-1", "Looks good — `ship it` \"}\"");
        assert_eq!(
            c.variables,
            json!({"input": {"issueId": "uuid-1", "body": "Looks good — `ship it` \"}\""}})
        );
        assert!(!c.query.contains("ship it"));
    }

    #[test]
    fn priority_parsing() {
        assert_eq!(priority(&Value::Null), Ok(None));
        assert_eq!(priority(&json!(0)), Ok(Some(0)));
        assert_eq!(priority(&json!(4)), Ok(Some(4)));
        assert_eq!(priority(&json!("Urgent")), Ok(Some(1)));
        assert_eq!(priority(&json!("normal")), Ok(Some(3)));
        assert_eq!(priority(&json!("P2")), Ok(Some(2)));
        assert_eq!(priority(&json!("3")), Ok(Some(3)));
        for bad in [json!(5), json!(-1), json!(2.5), json!("asap"), json!(true)] {
            assert!(priority(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn helpers() {
        assert!(is_uuid("8f3c1a2b-0d4e-4f5a-9b6c-7d8e9f0a1b2c"));
        assert!(!is_uuid("8f3c1a2b-0d4e-4f5a-9b6c-7d8e9f0a1b2"));
        assert!(!is_uuid("ENG-123"));
        assert_eq!(issue_ref(" eng-42 "), "ENG-42");
        assert_eq!(issue_ref("some-thing"), "some-thing");
        assert_eq!(
            str_list(&json!("Bug, ,Feature")),
            Some(vec!["Bug".to_string(), "Feature".to_string()])
        );
        assert_eq!(
            str_list(&json!(["a", 1, " b "])),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(str_list(&Value::Null), None);
    }

    #[test]
    fn picking_ids_from_lookups() {
        let teams = json!([{"id": "t1", "key": "ENG", "name": "Engineering"},
                            {"id": "t2", "key": "ENGX", "name": "eng"}]);
        let by_key = |n: &Value| {
            n["key"]
                .as_str()
                .is_some_and(|k| k.eq_ignore_ascii_case("eng"))
        };
        assert_eq!(pick_one(&teams, "team", "eng", "", &by_key).unwrap(), "t1");
        let e = pick_one(&teams, "team", "x", "", &|_: &Value| false).unwrap_err();
        assert!(e.contains("several") && e.contains("ENG (t1)"), "{e}");
        let e = pick_one(&json!([]), "team", "OPS", "Use list_teams.", &by_key).unwrap_err();
        assert!(
            e.contains("No Linear team matches `OPS`. Use list_teams."),
            "{e}"
        );
        assert_eq!(
            pick_one(&json!([{"id": "u1"}]), "user", "ada", "", &by_key).unwrap(),
            "u1"
        );

        let labels = json!([
            {"id": "other", "name": "Bug", "team": {"id": "t2"}},
            {"id": "ws", "name": "bug", "team": null},
            {"id": "mine", "name": "BUG", "team": {"id": "t1"}},
            {"id": "ws-feat", "name": "Feature", "team": null},
            {"id": "t2-only", "name": "Ops", "team": {"id": "t2"}}
        ]);
        assert_eq!(
            pick_labels(
                &labels,
                "t1",
                &["bug".into(), "feature".into(), "Bug".into()]
            )
            .unwrap(),
            vec!["mine", "ws-feat"]
        );
        assert_eq!(
            pick_labels(&labels, "t3", &["Bug".into()]).unwrap(),
            vec!["ws"]
        );
        let e = pick_labels(&labels, "t1", &["Ops".into(), "Nope".into()]).unwrap_err();
        assert!(e.contains("`Ops`, `Nope`"), "{e}");
    }

    // ── responses ──

    fn issues_fixture() -> Value {
        json!({"issues": {"nodes": [
            {"identifier": "ENG-123", "title": "Crash on save", "url": "https://linear.app/acme/issue/ENG-123/crash-on-save",
             "priority": 2, "state": {"name": "In Progress"},
             "assignee": {"name": "Ada Lovelace", "displayName": "ada"},
             "labels": {"nodes": [{"name": "Bug"}, {"name": "Frontend"}]}},
            {"identifier": "ENG-7", "title": "Docs", "url": "https://linear.app/acme/issue/ENG-7/docs",
             "priority": 0, "state": {"name": "Backlog"}, "assignee": null, "labels": {"nodes": []}}
        ], "pageInfo": {"hasNextPage": true, "endCursor": "abc123"}}})
    }

    #[test]
    fn issue_rows_and_lists() {
        let out = format_read("list_issues", &issues_fixture());
        assert!(out.starts_with("2 issues"), "{out}");
        assert!(out.contains("ENG-123 Crash on save [In Progress] · P2 · @ada · {Bug, Frontend} · https://linear.app/acme/issue/ENG-123/crash-on-save"), "{out}");
        assert!(
            out.contains(
                "ENG-7 Docs [Backlog] · P0 · unassigned · https://linear.app/acme/issue/ENG-7/docs"
            ),
            "{out}"
        );
        assert!(
            out.contains("more available — pass after: \"abc123\""),
            "{out}"
        );

        let empty = json!({"issues": {"nodes": [], "pageInfo": {"hasNextPage": false}}});
        assert_eq!(format_read("list_issues", &empty), "No issues match.");
        assert_eq!(
            format_read("search_issues", &json!({})),
            "No issues match the search."
        );
        // Missing fields don't panic.
        assert_eq!(issue_row(&json!({})), "? [?] · P0 · unassigned");
        let s = json!({"searchIssues": {"nodes": [{"identifier": "ENG-1", "title": "x"}]}});
        let out = format_read("search_issues", &s);
        assert!(
            out.contains("ENG-1 x [?]") && !out.contains("more"),
            "{out}"
        );
    }

    #[test]
    fn issue_details_with_comments_and_clipping() {
        let d = json!({"issue": {
            "id": "8f3c1a2b-0d4e-4f5a-9b6c-7d8e9f0a1b2c", "identifier": "ENG-123", "title": "Crash on save",
            "url": "https://linear.app/acme/issue/ENG-123", "description": "x".repeat(25_000),
            "priority": 1, "priorityLabel": "Urgent", "createdAt": "2024-05-01T10:22:33.000Z",
            "updatedAt": "2024-05-02T08:00:00.000Z", "dueDate": "2024-06-01", "estimate": 3,
            "state": {"name": "Todo", "type": "unstarted"},
            "assignee": {"name": "Ada Lovelace", "displayName": "ada", "email": "ada@example.com"},
            "creator": {"name": "Grace Hopper", "displayName": "grace"},
            "team": {"key": "ENG", "name": "Engineering"},
            "labels": {"nodes": [{"name": "Bug"}]}, "project": {"name": "Q3 Launch"},
            "cycle": {"number": 12, "name": null}, "parent": {"identifier": "ENG-100", "title": "Epic"},
            "comments": {"nodes": [
                {"body": "second", "createdAt": "2024-05-03T00:00:00Z", "user": {"name": "Grace Hopper", "displayName": "grace"}},
                {"body": "y".repeat(5_000), "createdAt": "2024-05-02T00:00:00Z", "user": null}
            ], "pageInfo": {"hasNextPage": false}}}});
        let out = format_read("get_issue", &d);
        assert!(out.starts_with("ENG-123 Crash on save\nhttps://linear.app/acme/issue/ENG-123\nState: Todo · Priority: 1 (Urgent) · Assignee: @ada · Team: ENG"), "{out}");
        for want in [
            "Labels: Bug",
            "Project: Q3 Launch",
            "Cycle: 12",
            "Parent: ENG-100 Epic",
            "Due: 2024-06-01",
            "Estimate: 3",
            "Created 2024-05-01T10:22 by @grace",
            "Comments (2):",
            "── ? · 2024-05-02T00:00",
            "[… truncated]",
        ] {
            assert!(out.contains(want), "missing {want}");
        }
        assert!(
            out.find("── ?").unwrap() < out.find("── @grace").unwrap(),
            "oldest first"
        );
        assert!(out.chars().count() < 20_000 + 4_000 + 1_000);

        let bare = format_read(
            "get_issue",
            &json!({"issue": {"identifier": "ENG-9", "title": "t"}}),
        );
        assert!(
            bare.contains("(no description)") && !bare.contains("Comments"),
            "{bare}"
        );
        assert_eq!(
            format_read("get_issue", &json!({"issue": null})),
            "No such issue."
        );
    }

    #[test]
    fn other_lists() {
        let t = json!({"teams": {"nodes": [{"id": "t1", "key": "ENG", "name": "Engineering", "description": null}], "pageInfo": {"hasNextPage": false}}});
        assert_eq!(
            format_read("list_teams", &t),
            "1 teams:\nENG · Engineering · id t1"
        );
        let s = json!({"workflowStates": {"nodes": [
            {"id": "s2", "name": "Done", "type": "completed", "position": 3, "team": {"key": "ENG"}},
            {"id": "s1", "name": "Todo", "type": "unstarted", "position": 1, "team": {"key": "ENG"}},
            {"id": "s0", "name": "Backlog", "type": "backlog", "position": 0, "team": {"key": "DES"}}]}});
        let out = format_read("list_states", &s);
        assert_eq!(
            out,
            "3 workflow states:\nDES · Backlog (backlog) · id s0\nENG · Todo (unstarted) · id s1\nENG · Done (completed) · id s2"
        );
        let p = json!({"projects": {"nodes": [{"id": "p1", "name": "Q3 Launch", "url": "https://linear.app/acme/project/q3",
            "progress": 0.456, "targetDate": "2024-09-30", "status": {"name": "In Progress"},
            "lead": {"name": "Ada", "displayName": "ada"}, "teams": {"nodes": [{"key": "ENG"}, {"key": "DES"}]}}]}});
        assert_eq!(
            format_read("list_projects", &p),
            "1 projects:\nQ3 Launch [In Progress] · 46% · lead @ada · target 2024-09-30 · teams ENG, DES · https://linear.app/acme/project/q3 · id p1"
        );
        let l = json!({"issueLabels": {"nodes": [
            {"id": "l1", "name": "Bug", "team": null, "parent": null},
            {"id": "l2", "name": "frontend", "team": {"id": "t1", "key": "ENG"}, "parent": {"name": "Area"}}]}});
        assert_eq!(
            format_read("list_labels", &l),
            "2 labels:\nBug (workspace) · id l1\nArea/frontend (ENG) · id l2"
        );
        let u = json!({"users": {"nodes": [
            {"id": "u1", "name": "Ada Lovelace", "displayName": "ada", "email": "ada@example.com", "active": true, "admin": true},
            {"id": "u2", "name": "Old Timer", "displayName": "", "email": null, "active": false, "admin": false}]}});
        assert_eq!(
            format_read("list_users", &u),
            "2 users:\nAda Lovelace @ada <ada@example.com> · admin · id u1\nOld Timer · deactivated · id u2"
        );
        assert_eq!(
            format_read("list_users", &json!({"users": {"nodes": []}})),
            "No users match."
        );
        let raw = format_read("graphql", &json!({"viewer": {"id": "u1"}}));
        assert!(raw.contains("\"viewer\""), "{raw}");
    }

    #[test]
    fn whoami_identity() {
        let d = json!({"viewer": {"id": "u1", "name": "Ada Lovelace", "displayName": "ada", "email": "ada@example.com"},
                       "organization": {"name": "Acme", "urlKey": "acme"}});
        assert_eq!(identity_of(&d), "Ada Lovelace <ada@example.com> · Acme");
        assert_eq!(
            format_read("whoami", &d),
            "Ada Lovelace <ada@example.com> · Acme\nuser id u1 · workspace acme"
        );
        assert_eq!(identity_of(&json!({})), "?");
    }

    #[test]
    fn write_payloads() {
        let p = json!({"success": true, "issue": {"identifier": "ENG-124", "title": "New", "priority": 3,
            "state": {"name": "Todo"}, "assignee": null, "labels": {"nodes": []}, "url": "https://linear.app/acme/issue/ENG-124"}});
        assert_eq!(
            format_issue_write("Created", &p).unwrap(),
            "Created ENG-124 New [Todo] · P3 · unassigned · https://linear.app/acme/issue/ENG-124"
        );
        assert!(format_issue_write("Updated", &json!({"success": false})).is_err());
        assert!(format_issue_write("Updated", &Value::Null).is_err());
    }

    #[test]
    fn successful_data_redacts_echoed_credentials_before_formatting() {
        for token in [
            KEY,
            "lin_oauth_DUMMYdummy0123456789",
            "dummy-quote\"slash\\token",
        ] {
            let body = json!({"data": {
                "viewer": {"id": "u1", "name": format!("Ada {token}"), "email": "ada@example.com"},
                "issues": {"nodes": [{"identifier": "ENG-1", "title": token}],
                    "pageInfo": {"hasNextPage": true, "endCursor": token}},
                "issueCreate": {"success": true, "issue": {"identifier": "ENG-2", "title": token}},
                "nested": [{token: [token, 42, true, null]}]
            }})
            .to_string();
            let data = interpret(200, &body, "graphql", &[token]).unwrap();
            let expected = json!({"[redacted]": ["[redacted]", 42, true, null]});
            assert_eq!(data["nested"][0], expected);
            for out in [
                identity_of(&data),
                format_read("whoami", &data),
                format_read("list_issues", &data),
                format_read("graphql", &data),
                format_issue_write("Created", &data["issueCreate"]).unwrap(),
            ] {
                assert!(
                    !out.contains(token),
                    "successful output leaked the credential"
                );
                assert!(out.contains("[redacted]"));
            }
            assert_eq!(data["viewer"]["id"], "u1");
        }
    }

    #[tokio::test]
    async fn validation_errors_redact_echoed_credentials() {
        let http = reqwest::Client::new();
        let cfg = LinearCfg {
            enabled: true,
            token: KEY.into(),
        };
        let error = run(&http, &cfg, &json!({"action": KEY})).await.unwrap_err();
        assert!(
            !error.contains(KEY),
            "validation error leaked the credential"
        );
        assert!(error.contains("[redacted]"));
    }

    #[test]
    fn graphql_errors_are_surfaced_and_token_free() {
        // HTTP 200 with an errors array is still an error.
        let body = json!({"data": null, "errors": [{"message": format!("Entity not found: Issue {KEY}"),
            "extensions": {"code": "INVALID_INPUT", "userPresentableMessage": "Could not find referenced Issue."}}]}).to_string();
        let e = interpret(200, &body, "get_issue", &[KEY]).unwrap_err();
        assert!(
            e.starts_with("Linear GraphQL error on get_issue: Entity not found"),
            "{e}"
        );
        assert!(e.contains("Could not find referenced Issue."), "{e}");
        assert!(e.contains(NOT_FOUND_HINT), "{e}");
        assert!(!e.contains(KEY), "{e}");

        // 400 with an auth error.
        let body = json!({"errors": [{"message": "Authentication required, not authenticated",
            "extensions": {"code": "AUTHENTICATION_ERROR", "type": "authentication error"}}]})
        .to_string();
        let e = interpret(400, &body, "whoami", &[KEY]).unwrap_err();
        assert!(
            e.starts_with("Linear 400 on whoami: Authentication required"),
            "{e}"
        );
        assert!(e.contains("Settings → Connectors → Linear"), "{e}");

        // RATELIMITED maps to the shared rate-limit message.
        let body = json!({"errors": [{"message": "Rate limit exceeded", "extensions": {"code": "RATELIMITED"}}]}).to_string();
        for st in [200, 400] {
            assert_eq!(
                interpret(st, &body, "list_issues", &[KEY]).unwrap_err(),
                plugin_services::rate_limited("Linear", None)
            );
        }

        let body =
            json!({"errors": [{"message": "Forbidden", "extensions": {"code": "FORBIDDEN"}}]})
                .to_string();
        assert!(interpret(200, &body, "issueCreate", &[KEY])
            .unwrap_err()
            .contains("read-only key cannot write"));

        // Non-JSON error bodies.
        let e = interpret(401, &format!("bad key {KEY}"), "whoami", &[KEY]).unwrap_err();
        assert!(
            e.starts_with("Linear 401 on whoami:") && e.contains(AUTH_HINT),
            "{e}"
        );
        assert!(!e.contains(KEY), "{e}");
        let e = interpret(502, "<html>Bad gateway</html>", "list_teams", &[KEY]).unwrap_err();
        assert!(e.contains("502") && e.contains("Bad gateway"), "{e}");
        // 200 without data.
        assert!(interpret(200, "{}", "x", &[KEY]).is_err());
        assert!(interpret(200, "not json", "x", &[KEY]).is_err());
        // The happy path.
        assert_eq!(
            interpret(200, r#"{"data":{"viewer":{"id":"u1"}}}"#, "whoami", &[KEY]).unwrap(),
            json!({"viewer": {"id": "u1"}})
        );
    }
}
