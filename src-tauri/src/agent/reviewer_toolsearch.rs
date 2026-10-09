//! Tests for the deferred tool catalog — the piece of context that is *not*
//! sent with every request, and the `tool_search` tool that reaches it.
//!
//! The property the whole feature turns on: the tool list a request carries and
//! the frozen system prompt that describes it must not disagree. MCP tools are
//! never named in the prompt, so deferring them is safe; a plugin tool is, so it
//! is only deferred when the user asks. Both halves are pinned here, along with
//! the threshold (small setups load everything), the ranking, the persistence
//! that makes a loaded tool stay loaded, and the no-match answer.

use super::toolindex::{self, Catalog, Deferral, Source, DEFAULT_RESULTS, SEARCH_THRESHOLD};
use super::Runtime;
use serde_json::{json, Value};

fn mcp(server: &str, tool: &str, desc: &str) -> Value {
    json!({
        "name": format!("mcp__{server}__{tool}"),
        "description": desc,
        "input_schema": {"type": "object", "properties": {}}
    })
}

fn plugin(id: &str, desc: &str) -> Value {
    json!({"name": id, "description": desc, "input_schema": {"type": "object"}})
}

/// A tail much like a real chat's: two MCP servers plus a plugin, about the same
/// size as the built-in list, so the total crosses the threshold.
fn tail() -> Vec<Value> {
    let mut v = vec![
        mcp(
            "github",
            "create_pull_request",
            "Create a pull request on GitHub from a branch.",
        ),
        mcp(
            "github",
            "merge_pull_request",
            "Merge a pull request once its checks pass.",
        ),
        mcp(
            "github",
            "create_issue",
            "Open a new issue in a repository.",
        ),
        mcp(
            "ctx7",
            "resolve-library-id",
            "Resolve a package name to a Context7 library id for documentation lookup.",
        ),
        mcp(
            "ctx7",
            "get-library-docs",
            "Fetch the documentation for a Context7 library.",
        ),
        plugin(
            "browser",
            "Drive a headless browser: navigate, click, type.",
        ),
        plugin("render", "Screenshot a page in a headless browser."),
    ];
    // A dozen more, so `core + len` clears the threshold the way a real chat does.
    for i in 0..12 {
        v.push(mcp(
            "jira",
            &format!("issue_op{i}"),
            "Work with a Jira issue.",
        ));
    }
    v
}

fn names(v: &[Value]) -> Vec<String> {
    v.iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect()
}

fn core(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| json!({"name": format!("core_{i}"), "description": "a built-in"}))
        .collect()
}

#[test]
fn below_the_threshold_everything_loads_and_there_is_no_search() {
    // The point of the threshold: a setup that is not paying the cost must not
    // pay the mechanism either. `tool_search` itself costs a slot and a
    // description, so it must not be present when nothing is deferred.
    let small = Deferral::new(5, &tail(), &[], false, true);
    assert!(!small.on(), "5 built-ins + a tail is under the threshold");
    let list = small.tool_list(core(5), &[]);
    assert_eq!(
        list.len(),
        5 + tail().len(),
        "everything is declared, exactly as before"
    );
    assert!(
        !names(&list).contains(&"tool_search".to_string()),
        "no search tool when nothing is held back"
    );
}

#[test]
fn above_the_threshold_the_tail_is_deferred_and_search_is_offered() {
    let d = Deferral::new(29, &tail(), &[], false, true);
    assert!(d.on(), "29 built-ins + the tail is past the threshold");
    let list = d.tool_list(core(29), &[]);
    let n = names(&list);
    assert!(
        n.contains(&"tool_search".to_string()),
        "the model is told it can search"
    );
    assert!(
        !n.iter().any(|x| x.starts_with("mcp__")),
        "no MCP tool is declared: that is the whole saving"
    );
    // Plugin tools are named in `prompt::screen_section`, so by default they
    // stay declared — see the plugin test below.
    assert!(n.contains(&"browser".to_string()));
    assert_eq!(list.len(), 29 + 1 + 2, "core + search + both plugins");
}

#[test]
fn the_global_switch_turns_the_mechanism_off_entirely() {
    let d = Deferral::new(29, &tail(), &[], false, false);
    assert!(!d.on());
    let list = d.tool_list(core(29), &[]);
    assert_eq!(list.len(), 29 + tail().len());
    assert!(!names(&list).contains(&"tool_search".to_string()));
}

#[test]
fn search_ranks_a_name_segment_match_above_a_description_mention() {
    let cat = Catalog::build(&tail(), &[], false);
    let hits = cat.search("pull request", DEFAULT_RESULTS);
    assert!(!hits.is_empty(), "a real query finds something");
    // `mcp__github__create_pull_request` and `merge_pull_request` both carry the
    // words in their *names*; nothing else should outrank them.
    assert!(
        hits[0].name.contains("pull_request"),
        "the strongest name match wins, got {}",
        hits[0].name
    );
    let all = cat.search("pull request", DEFAULT_RESULTS);
    assert!(
        all.iter().all(|h| h.name.contains("pull_request")),
        "a Jira issue only mentions 'issue', not 'pull request'"
    );
}

#[test]
fn search_prefers_a_name_match_over_a_description_only_match() {
    let cat = Catalog::build(&tail(), &[], false);
    let hits = cat.search("library", DEFAULT_RESULTS);
    // Both ctx7 tools carry `library` in the name; `resolve-library-id` also
    // camel-splits. Whatever comes back must be one of them, not a Jira issue.
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| h.name.contains("library")),
        "description-only mentions must not beat name matches: {:?}",
        hits.iter().map(|h| &h.name).collect::<Vec<_>>()
    );
}

#[test]
fn search_is_deterministic_so_the_tool_list_stays_byte_stable() {
    // The prompt cache needs the same query to produce the same list every time,
    // including the order the loaded schemas are appended in.
    let cat = Catalog::build(&tail(), &[], false);
    let a: Vec<String> = cat
        .search("issue", DEFAULT_RESULTS)
        .iter()
        .map(|t| t.name.clone())
        .collect();
    for _ in 0..5 {
        let b: Vec<String> = cat
            .search("issue", DEFAULT_RESULTS)
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(a, b, "same query, same order, every time");
    }
}

#[test]
fn select_loads_a_tool_by_exact_name() {
    let cat = Catalog::build(&tail(), &[], false);
    let hits = cat.search("select:mcp__ctx7__get-library-docs", 5);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "mcp__ctx7__get-library-docs");
}

#[test]
fn a_search_that_matches_nothing_degrades_gracefully() {
    // Not an error, and not silence: the model is told what *is* indexed, so a
    // miss is a retry rather than a dead end.
    let cat = Catalog::build(&tail(), &[], false);
    assert!(cat
        .search("kubernetes helm chart", DEFAULT_RESULTS)
        .is_empty());
    let text = toolindex::search_result(&cat, "kubernetes helm chart", DEFAULT_RESULTS);
    assert!(
        text.contains("No tool matched"),
        "a miss says so plainly: {text}"
    );
    assert!(
        text.contains("MCP server `github`") || text.contains("MCP server `ctx7`"),
        "and says what is actually there: {text}"
    );
    // An empty query is the other degenerate case: also a plain answer, no panic.
    let empty = toolindex::search_result(&cat, "   ", DEFAULT_RESULTS);
    assert!(empty.contains("No tool matched"));
}

#[tokio::test]
async fn a_searched_tool_stays_loaded_for_the_rest_of_the_conversation() {
    // Copilot's rule, and the one the whole persistence model exists for: search
    // once, use the tool from then on. Modelled end to end through the real
    // helpers: the search, what gets remembered, and the tool list the *next*
    // request would carry.
    let rt = Runtime::default();
    let d = Deferral::new(29, &tail(), &[], false, true);
    assert!(d.on());

    let hits = d.catalog.search("pull request", DEFAULT_RESULTS);
    let found = hits[0].name.clone();
    let loaded = toolindex::loaded(&rt, "main").await;
    assert!(loaded.is_empty(), "nothing loaded before the search");

    let new = d.newly_loaded(&hits, &loaded);
    assert!(new.contains(&found));
    toolindex::remember(&rt, "main", &new).await;

    // The turn the search happened on still had the old list; that is fine —
    // the result text carried the schema. From the next turn on, the tool is
    // declared.
    let next = d.tool_list(core(29), &toolindex::loaded(&rt, "main").await);
    let n = names(&next);
    assert!(
        n.contains(&found),
        "{found} must be declared from the next request on"
    );
    // Everything MCP that is now declared is something this search loaded; a
    // server nobody searched for stays deferred.
    let mcp_now: Vec<&String> = n.iter().filter(|x| x.starts_with("mcp__")).collect();
    assert!(
        mcp_now.iter().all(|x| new.contains(x)),
        "only what the search loaded is declared, got {mcp_now:?}"
    );

    // "pull request" matches both GitHub tools, which is fine — the point is
    // that a second search neither drops nor duplicates what is already loaded,
    // so the appended list is stable turn to turn.
    let count = toolindex::loaded(&rt, "main").await.len();
    assert!(count >= 1);
    toolindex::remember(&rt, "main", &new).await;
    assert_eq!(
        toolindex::loaded(&rt, "main").await.len(),
        count,
        "re-loading the same tools must not grow the list"
    );

    // The per-agent key keeps a sub-agent's loaded set off the main agent's.
    assert!(toolindex::loaded(&rt, "sub-1").await.is_empty());

    // Compaction drops the prefix; the loaded set goes with it.
    toolindex::forget(&rt, "main").await;
    assert!(toolindex::loaded(&rt, "main").await.is_empty());
    let after = d.tool_list(core(29), &toolindex::loaded(&rt, "main").await);
    assert!(
        !names(&after).contains(&found),
        "a rebuilt prefix does not keep the old set's schemas"
    );
}

#[tokio::test]
async fn loaded_names_that_are_no_longer_in_the_catalog_resolve_to_nothing() {
    // A server can disconnect, or a plugin be turned off, between a search and
    // the next request. The loaded set is just names, so it must be harmless.
    let d = Deferral::new(29, &tail(), &[], false, true);
    let resolved = d.catalog.resolve(&["mcp__ghost__gone".to_string()]);
    assert!(resolved.is_empty(), "an unknown name adds no schema");
    let mixed = d.catalog.resolve(&[
        "mcp__ghost__gone".into(),
        "mcp__github__create_issue".into(),
    ]);
    assert_eq!(mixed.len(), 1);
    assert_eq!(mixed[0]["name"], "mcp__github__create_issue");
}

#[test]
fn a_server_can_opt_out_and_its_tools_stay_declared() {
    // The reason to opt out: a server the agent reaches for constantly is a
    // search per call, and the search costs more than the token saving.
    let cat = Catalog::build(&tail(), &["github".to_string()], false);
    for t in cat
        .tools
        .iter()
        .filter(|t| t.name.starts_with("mcp__github__"))
    {
        assert!(!t.defer, "{} opted out", t.name);
        assert!(matches!(t.source, Source::Mcp { .. }));
    }
    let kept: Vec<String> = names(&cat.always());
    assert!(
        kept.iter().any(|n| n == "mcp__github__create_issue"),
        "the opted-out server is kept: {kept:?}"
    );
    assert!(
        !kept
            .iter()
            .any(|n| n.starts_with("mcp__ctx7__") || n.starts_with("mcp__jira__")),
        "every *other* MCP server is still deferred: {kept:?}"
    );
    let d = Deferral::new(29, &tail(), &["github".to_string()], false, true);
    let list = names(&d.tool_list(core(29), &[]));
    assert!(list.contains(&"mcp__github__create_issue".to_string()));
    assert!(
        !list.contains(&"mcp__ctx7__get-library-docs".to_string()),
        "the server that did not opt out is still deferred"
    );
}

#[test]
fn an_opt_out_matches_the_sanitised_server_name() {
    // `mcp::tool_schemas` builds `mcp__{sanitize(server)}__{tool}`, so the
    // catalog sees `my_server` while settings hold `my server`. An opt-out that
    // compared the two raw would silently miss and keep spending the tokens.
    let t = vec![json!({
        "name": "mcp__my_server__do_thing",
        "description": "does a thing",
        "input_schema": {"type": "object"}
    })];
    let cat = Catalog::build(&t, &["my server".to_string()], false);
    assert!(!cat.tools[0].defer, "the opt-out must survive sanitising");
    assert_eq!(
        cat.tools[0].source,
        Source::Mcp {
            server: "my_server".into()
        }
    );
}

#[test]
fn plugin_tools_are_only_deferred_when_asked_for() {
    // The invariant half of the design. `prompt::screen_section` names `browser`
    // and `render` in the frozen system prompt and `prefix_plugins` exists so the
    // list can never disagree with it, so by default they are declared. Turning
    // plugin deferral on is coherent only because the search tool's own
    // description then names them — asserted below.
    let off = Deferral::new(29, &tail(), &[], false, true);
    let n = names(&off.tool_list(core(29), &[]));
    assert!(n.contains(&"browser".to_string()));
    assert!(n.contains(&"render".to_string()));

    let on = Deferral::new(29, &tail(), &[], true, true);
    let n = names(&on.tool_list(core(29), &[]));
    assert!(!n.contains(&"browser".to_string()), "deferred on request");
    let search = on
        .tool_list(core(29), &[])
        .into_iter()
        .find(|t| t["name"] == "tool_search")
        .expect("search tool present");
    let desc = search["description"].as_str().unwrap();
    assert!(
        desc.contains("`browser`") && desc.contains("`render`"),
        "with plugins deferred, the prompt-named tools must be named by the \
         search tool instead, or the prompt describes a list that is not there: {desc}"
    );
}

#[test]
fn a_search_result_reports_the_tools_it_found() {
    let cat = Catalog::build(&tail(), &[], false);
    let text = toolindex::search_result(&cat, "pull request", DEFAULT_RESULTS);
    assert!(text.contains("mcp__github__create_pull_request"), "{text}");
    assert!(
        text.contains("stay available"),
        "the persistence promise is stated"
    );
}

#[test]
fn a_deferred_tool_carries_its_origin_so_the_model_knows_what_it_reached() {
    let cat = Catalog::build(&tail(), &[], false);
    let t = cat
        .tools
        .iter()
        .find(|t| t.name == "mcp__ctx7__get-library-docs")
        .unwrap();
    assert_eq!(t.origin(), "MCP server `ctx7`");
    let p = cat.tools.iter().find(|t| t.name == "browser").unwrap();
    assert_eq!(p.origin(), "plugin `browser`");
}

#[test]
fn an_unrecognised_tool_is_never_deferred() {
    // The safe direction: a future tool nobody classified must keep loading as
    // it always did, rather than vanish into a catalog nothing searches.
    let cat = Catalog::build(
        &[json!({"name": "brand_new_thing", "description": "who knows"})],
        &[],
        true,
    );
    assert!(!cat.tools[0].defer);
    assert_eq!(cat.tools[0].source, Source::Other);
    assert!(cat.deferred().count() == 0);
    assert!(
        !cat.should_defer(100),
        "nothing to defer means nothing to search"
    );
}

#[test]
fn the_threshold_counts_the_core_plus_the_tail() {
    // Exactly at the threshold is not past it: "below ~30, skip it".
    let cat = Catalog::build(&tail(), &[], false);
    let total = cat.tools.len();
    assert!(
        cat.should_defer(SEARCH_THRESHOLD + 1 - total),
        "over: defer"
    );
    assert!(
        !cat.should_defer(SEARCH_THRESHOLD - total),
        "at/under: load everything"
    );
}
