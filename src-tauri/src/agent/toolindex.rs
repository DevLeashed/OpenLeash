//! Deferred tool catalog: the long tail of tools a chat does not carry in
//! every request, and the index behind `tool_search`.
//!
//! Why this exists, with numbers: a few dozen tool definitions is 10-20K
//! tokens, and tool-choice accuracy degrades past roughly that many (GitHub
//! Copilot's tool-search notes, which is also where the ~30 threshold comes
//! from). This harness passes it comfortably once MCP servers are on: ~29
//! built-ins, plus a dozen per connected server. So above the threshold the
//! built-ins stay declared and the long tail — every `mcp__<server>__<tool>`,
//! and the plugin tools when the user asks for them to be included — is held
//! back, indexed by name and description, and reached through `tool_search`.
//!
//! # The prompt-cache question
//!
//! `runner.rs` freezes the system prompt and tool list per task so the
//! provider's prompt cache keeps hitting, and `prefix_plugins` serves a frozen
//! plugin snapshot so the *core* tool set can never disagree with the prose in
//! the system prompt that describes it (`prompt::screen_section` names the
//! plugin tools it writes about). A tool list that grows mid-task looks like it
//! breaks that. It does not, and the reason is what the growth is made of:
//!
//!   * **MCP tools are never named in the system prompt.** There is no prose to
//!     disagree with, so appending one cannot violate the invariant
//!     `prefix_plugins` exists to protect. That is why MCP tools are what this
//!     defers by default.
//!   * **Plugin tools are named in it** (`screen_section` writes "`browser`
//!     renders and drives a throwaway browser…"). Deferring those by default
//!     would put a name in the prompt and not in the list — exactly the
//!     mismatch the frozen snapshot prevents. So plugin deferral is an explicit
//!     opt-in (`Settings::defer_plugins`) that is only coherent because
//!     `tool_search`'s description, built in the same frozen prefix, names the
//!     plugins it is holding back. It is off by default until `prompt.rs` (owned
//!     elsewhere) can tell the model to search for the tool it names.
//!   * What the growth costs is one prompt-cache write the first time a batch is
//!     loaded, because the tools block of the prefix changed; after that the
//!     bytes are stable, since the loaded set only ever grows and is appended in
//!     a deterministic order. Bounded and one-off: a chat pays it once per
//!     search, which is the same trade the harness already makes at compaction.
//!   * The growth lands at a turn boundary by construction: `tool_defs` is
//!     rebuilt at the top of every loop iteration, and a search result is only
//!     persisted after the turn that produced it has ended. No request in flight
//!     ever sees the list change under it, and history stays append-only.
//!
//! The rejected alternative — return the schemas as tool *results* and never
//! touch the list — respects the cache to the last byte but loses the point of
//! the feature: the schema then lives in message history, where it is re-sent
//! every turn anyway, and a tool the model was never declared is one some
//! gateways reject and every model is likelier to forget at the moment it
//! should reach for it. So a search result does both: it carries the schema text
//! so the model can act in the same turn, and the persistence below means the
//! tools are declared from the next turn on.
//!
//! Deferral is a *presentation* decision over the frozen tool snapshot, never a
//! second source of truth: the catalog is built from exactly the `Vec<Value>`
//! the frozen prefix would have served.

use super::Runtime;
use serde_json::Value;
use std::collections::HashSet;

/// Below this many tools in total a chat loads everything and the mechanism
/// does not exist for it. Copilot's rule, adopted unchanged: under a few dozen
/// tools the search is pure overhead, and the cost it saves is not there yet.
pub const SEARCH_THRESHOLD: usize = 30;

/// Hits a search returns when the model does not say.
pub const DEFAULT_RESULTS: usize = 5;
/// Ceiling on hits, so one search cannot paste the whole catalog into context.
pub const MAX_RESULTS: usize = 10;

/// Plugin tool names. Must match `plugins::schemas`. Anything not listed here
/// classifies as [`Source::Other`] and is therefore never deferred, which is
/// the safe direction for a tool nobody has taught this file about.
const PLUGIN_IDS: [&str; 4] = ["github", "computer", "render", "browser"];

/// Where a deferred tool came from. Carried so a search result can say whose
/// tool it is, and so a description can name what is being held back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Mcp { server: String },
    Plugin { id: String },
    Other,
}

/// One tool that may be held back from a request.
#[derive(Debug, Clone)]
pub struct DeferredTool {
    pub name: String,
    pub description: String,
    /// The definition exactly as it would have been served.
    pub schema: Value,
    pub source: Source,
    /// `false` = always declared (the server or plugin opted out of deferral).
    pub defer: bool,
}

impl DeferredTool {
    /// A short human label for where the tool lives, for the search result.
    pub fn origin(&self) -> String {
        match &self.source {
            Source::Mcp { server } => format!("MCP server `{server}`"),
            Source::Plugin { id } => format!("plugin `{id}`"),
            Source::Other => "this harness".to_string(),
        }
    }
}

/// The deferrable tail, in the frozen order it was built from.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub tools: Vec<DeferredTool>,
}

impl Catalog {
    /// Build from the frozen MCP + plugin schema list the prefix would serve.
    ///
    /// `off_servers` names MCP servers whose tools are never deferred (matched
    /// after the same sanitising `mcp::tool_schemas` applies to their names).
    /// `defer_plugins` is the global switch from `Settings`; see the module
    /// comment for why it is off unless asked for.
    pub fn build(frozen: &[Value], off_servers: &[String], defer_plugins: bool) -> Catalog {
        let off_s: HashSet<String> = off_servers.iter().map(|s| server_key(s)).collect();
        let mut tools = Vec::new();
        for t in frozen {
            let Some(name) = t["name"].as_str().filter(|n| !n.is_empty()) else {
                continue;
            };
            let (source, defer) = classify(name, &off_s, defer_plugins);
            tools.push(DeferredTool {
                name: name.to_string(),
                description: t["description"].as_str().unwrap_or("").to_string(),
                schema: t.clone(),
                source,
                defer,
            });
        }
        Catalog { tools }
    }

    /// Tools held back at this moment.
    pub fn deferred(&self) -> impl Iterator<Item = &DeferredTool> {
        self.tools.iter().filter(|t| t.defer)
    }

    /// Tools that opted out, and so are declared exactly as before.
    pub fn always(&self) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|t| !t.defer)
            .map(|t| t.schema.clone())
            .collect()
    }

    /// Every schema in the catalog, in frozen order. Served wholesale when the
    /// chat is below the threshold.
    pub fn schemas(&self) -> Vec<Value> {
        self.tools.iter().map(|t| t.schema.clone()).collect()
    }

    /// Whether the search mechanism should run at all for a request whose core
    /// list is `core` tools long.
    pub fn should_defer(&self, core: usize) -> bool {
        core + self.tools.len() > SEARCH_THRESHOLD && self.tools.iter().any(|t| t.defer)
    }

    /// Names of the deferred *plugin* tools, so the search tool's description
    /// can say which prompt-named tools moved behind it.
    pub fn deferred_plugins(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .deferred()
            .filter(|t| matches!(t.source, Source::Plugin { .. }))
            .map(|t| t.name.clone())
            .collect();
        out.sort();
        out
    }

    /// Schemas for the deferred tools named in `loaded`, in catalog order.
    /// Names that are not (or are no longer) in the catalog resolve to nothing,
    /// which is what makes a loaded set safe to carry across a reload.
    pub fn resolve(&self, loaded: &[String]) -> Vec<Value> {
        if loaded.is_empty() {
            return vec![];
        }
        let want: HashSet<&str> = loaded.iter().map(|s| s.as_str()).collect();
        self.tools
            .iter()
            .filter(|t| t.defer && want.contains(t.name.as_str()))
            .map(|t| t.schema.clone())
            .collect()
    }

    /// The servers and plugins the tail draws from, for a no-match answer that
    /// tells the model what is actually there.
    pub fn origins(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in self.deferred() {
            let o = t.origin();
            if !out.contains(&o) {
                out.push(o);
            }
        }
        out
    }

    /// Ranked search over the deferred tools.
    ///
    /// Ranking, in one paragraph: the query is lowercased and split into
    /// alphanumeric tokens, and each tool is scored against them. A token that
    /// equals a whole segment of the tool's name (`post_message` → `post`,
    /// `message`, plus the server and `mcp`) is the strongest signal, a token
    /// that merely prefixes a segment is weaker, a token anywhere else in the
    /// name weaker still, and a token in the description weakest — with a bonus
    /// when it appears early, since that is where a tool says what it is for.
    /// The whole query appearing in the name outranks any token score. Ties
    /// break on the name, so the same query always returns the same order
    /// (which the prompt cache needs). `select:name` skips ranking entirely and
    /// picks tools by exact name.
    pub fn search(&self, query: &str, k: usize) -> Vec<&DeferredTool> {
        let k = k.clamp(1, MAX_RESULTS);
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        if let Some(rest) = q.strip_prefix("select:") {
            let want: Vec<String> = rest
                .split([',', ';', ' '])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let mut out: Vec<&DeferredTool> = self
                .deferred()
                .filter(|t| want.contains(&t.name.to_lowercase()))
                .collect();
            out.sort_by(|a, b| a.name.cmp(&b.name));
            out.truncate(k);
            return out;
        }
        let toks = tokenize(&q);
        if toks.is_empty() {
            return vec![];
        }
        let mut scored: Vec<(i64, &DeferredTool)> = self
            .deferred()
            .map(|t| (score(t, &q, &toks), t))
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored.into_iter().take(k).map(|(_, t)| t).collect()
    }
}

/// The whole mechanism for one request: the catalog, and whether it is on.
#[derive(Debug, Clone)]
pub struct Deferral {
    pub catalog: Catalog,
    on: bool,
}

impl Deferral {
    pub fn from_catalog(catalog: Catalog, on: bool) -> Deferral {
        Deferral { catalog, on }
    }

    /// `core_count` is the length of the built-in list this request would serve.
    /// `enabled` is the global switch; a chat below the threshold is off
    /// regardless, because small setups must not get worse.
    pub fn new(
        core_count: usize,
        frozen_tail: &[Value],
        off_servers: &[String],
        defer_plugins: bool,
        enabled: bool,
    ) -> Deferral {
        let catalog = Catalog::build(frozen_tail, off_servers, defer_plugins);
        let on = enabled && catalog.should_defer(core_count);
        Deferral { catalog, on }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    /// The tool list one request should carry: `core`, plus the opted-out tail,
    /// plus `tool_search` itself, plus anything the model has already loaded.
    ///
    /// Off: the frozen tail goes back on verbatim — the old behaviour exactly,
    /// with no search tool to waste a slot on.
    pub fn tool_list(&self, mut core: Vec<Value>, loaded: &[String]) -> Vec<Value> {
        if !self.on {
            core.extend(self.catalog.schemas());
            return core;
        }
        core.extend(self.catalog.always());
        core.push(super::tools::tool_search_schema(
            &self.catalog.deferred_plugins(),
        ));
        core.extend(self.catalog.resolve(loaded));
        core
    }

    /// Names the search should report as newly loaded, given what was already
    /// loaded. Used to keep the timeline/notes honest about what a search did.
    pub fn newly_loaded(&self, hits: &[&DeferredTool], before: &[String]) -> Vec<String> {
        hits.iter()
            .map(|t| t.name.clone())
            .filter(|n| !before.iter().any(|x| x == n))
            .collect()
    }
}

/// `sanitize` as `mcp.rs` applies it to a server name when it builds a tool
/// name. Mirrored here because the opt-out list arrives as configured names and
/// the catalog sees only the sanitised ones; the two must agree or a server
/// could opt out and be ignored.
pub fn server_key(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn classify(name: &str, off_s: &HashSet<String>, defer_plugins: bool) -> (Source, bool) {
    if let Some(rest) = name.strip_prefix("mcp__") {
        // The split is the one `McpManager::call` makes when it routes the call,
        // and it is unambiguous because `sanitize` turns every non-word
        // character into `_`.
        if let Some((server, _)) = rest.split_once("__") {
            return (
                Source::Mcp {
                    server: server.to_string(),
                },
                !off_s.contains(server),
            );
        }
    }
    if let Some(id) = PLUGIN_IDS.iter().find(|p| **p == name) {
        return (
            Source::Plugin {
                id: (*id).to_string(),
            },
            defer_plugins,
        );
    }
    // Anything unrecognised loads exactly as it always did. Deferral is for the
    // tail we know about; a tool must never silently vanish because nobody
    // added it to a list here.
    (Source::Other, false)
}

fn tokenize(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in q.split(|c: char| !c.is_alphanumeric()) {
        if !t.is_empty() && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Name segments: split on the separators tool names use, and on a lower→upper
/// boundary so a camelCase tool is still found by its words.
fn name_parts(name: &str) -> Vec<String> {
    let mut spaced = String::with_capacity(name.len() + 8);
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() && prev_lower {
            spaced.push('_');
        }
        spaced.push(c);
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
    }
    spaced
        .split(|c| c == '_' || c == '-')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

fn score(t: &DeferredTool, q: &str, toks: &[String]) -> i64 {
    let name = t.name.to_lowercase();
    let desc = t.description.to_lowercase();
    let parts = name_parts(&t.name);
    let mut s = 0i64;
    if name == q {
        s += 1000;
    } else if name.contains(q) {
        s += 200;
    }
    for tok in toks {
        if parts.iter().any(|p| p == tok) {
            s += 40;
        } else if parts.iter().any(|p| p.starts_with(tok.as_str())) {
            s += 12;
        } else if name.contains(tok.as_str()) {
            s += 8;
        }
        match desc.find(tok.as_str()) {
            // Early in the description is where a tool says what it is for.
            Some(0..=119) => s += 6,
            Some(_) => s += 2,
            None => {}
        }
    }
    s
}

/// Deferred tools this agent has already loaded, oldest first.
pub async fn loaded(rt: &Runtime, key: &str) -> Vec<String> {
    rt.loaded_tools
        .lock()
        .await
        .get(key)
        .cloned()
        .unwrap_or_default()
}

/// Remember the tools a search found for the rest of the conversation.
/// Idempotent: loading the same tool twice does not duplicate it, so the tool
/// list stays byte-stable across turns and the prompt cache keeps hitting.
pub async fn remember(rt: &Runtime, key: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let mut g = rt.loaded_tools.lock().await;
    let list = g.entry(key.to_string()).or_default();
    for n in names {
        if !list.iter().any(|x| x == n) {
            list.push(n.clone());
        }
    }
}

/// Drop an agent's loaded set. Called wherever the frozen prefix is dropped —
/// compaction and reload — because the two describe the same request.
#[cfg(test)]
pub async fn forget(rt: &Runtime, key: &str) {
    rt.loaded_tools.lock().await.remove(key);
}

/// The tool-result text for a search. Returns `Ok(text)`; a search that matches
/// nothing is a normal answer, not an error, and says what is actually indexed
/// so the model can retry rather than give up.
pub fn search_result(cat: &Catalog, query: &str, limit: usize) -> String {
    let hits = cat.search(query, limit);
    if hits.is_empty() {
        let origins = cat.origins();
        let where_ = if origins.is_empty() {
            "No deferred tools are configured in this chat.".to_string()
        } else {
            format!("Deferred tools exist for: {}.", origins.join(", "))
        };
        return format!(
            "No tool matched `{query}`. {where_} Try a shorter query with the words you'd expect in the tool's name (e.g. `pull request`, `issue`, `browser`), or `select:server__tool` if you know the exact name."
        );
    }
    let mut out = String::from("<system-reminder>These tools are now available to call; they stay available for the rest of this conversation.</system-reminder>\n");
    for t in &hits {
        out.push_str(&format!(
            "\n- `{}` ({}): {}\n",
            t.name,
            t.origin(),
            t.description.trim()
        ));
    }
    out.push_str(&format!(
        "\nCall them by name. {} more match(es) may exist; search again with a different query if none of these is right.",
        if hits.len() >= limit { "More" } else { "No" }
    ));
    out
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn server_keys_match_the_naming_rule() {
        // The same rule `mcp::sanitize` applies when it builds `mcp__{server}__{tool}`.
        assert_eq!(server_key("my server"), "my_server");
        assert_eq!(server_key("ctx7"), "ctx7");
        assert_eq!(server_key("a/b.c"), "a_b_c");
        assert_eq!(server_key("keep-dash"), "keep-dash");
    }

    #[test]
    fn name_parts_splits_separators_and_camel_case() {
        assert_eq!(
            name_parts("mcp__postMessage"),
            vec!["mcp", "post", "message"]
        );
        assert_eq!(name_parts("pull-request"), vec!["pull", "request"]);
    }

    #[test]
    fn tokenize_dedupes_and_drops_punctuation() {
        assert_eq!(
            tokenize("post the post message!"),
            vec!["post", "the", "message"]
        );
    }
}
