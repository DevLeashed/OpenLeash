//! Release-gate tests for the MCP boundary: server names, pre-approval, and the
//! `mcp__<server>__<tool>` delimiter.
//!
//! Three things here are security properties rather than features:
//!
//! * a per-server `auto_approve` list may only ever *narrow* what the gate would
//!   otherwise ask about — it must never pre-approve a tool the user did not name;
//! * a server name that the `mcp__<server>__<tool>` delimiter cannot carry must be
//!   refused, because a name that mis-parses can make a rule written for one
//!   server cover another's tool;
//! * these all reach `permissions::check` as ordinary allow rules, so the tests
//!   assert through that same function — a pre-approval that does not show up
//!   there is not a pre-approval at all.
//!
//! The functions under test are pure: no filesystem, no network, no clock, no
//! env, no global. Everything runs in parallel.

#![cfg(test)]

use super::mcp::{auto_approve_rules, server_name_error, split_tool, sync_auto_approve, wire_tool};
use super::permissions::{check, Ctx, Decision};
use super::store::{AllowRule, McpServerCfg};
use serde_json::json;

// ───────────────────────── helpers ─────────────────────────

fn cfg(name: &str, auto_approve: &[&str]) -> McpServerCfg {
    McpServerCfg {
        name: name.into(),
        auto_approve: auto_approve.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn patterns(rules: &[AllowRule]) -> Vec<String> {
    rules.iter().map(|r| r.pattern.clone()).collect()
}

/// A gate context whose only allow rules are the ones under test. `cwd`/`project`
/// are unused by the `mcp__` arm, so any platform-safe value will do.
fn gate_ctx<'a>(allow: &'a [AllowRule]) -> Ctx<'a> {
    Ctx {
        perm: "ask",
        plan: false,
        cwd: "",
        project: "",
        allow,
    }
}

/// Whether a call to `tool` reaches `Allow` — i.e. is pre-approved outright.
fn is_allowed(tool: &str, allow: &[AllowRule]) -> bool {
    matches!(
        check(tool, &json!({}), None, &gate_ctx(allow)),
        Decision::Allow
    )
}

// ───────────────────────── per-tool auto-approval ─────────────────────────

/// One entry becomes exactly one rule, and it is the same rule the approval
/// prompt offers when you press "always allow" — so the config path and the
/// click path produce byte-identical grants.
#[test]
fn one_entry_expands_to_the_single_rule_the_prompt_offers() {
    let rules = auto_approve_rules(&cfg("gh", &["read_thing"])).unwrap();
    assert_eq!(patterns(&rules), vec!["mcp__gh__read_thing *"]);
    assert_eq!(rules[0].project, "", "a pre-approval is not project-scoped");
}

/// The security property: naming one tool pre-approves *that tool* and nothing
/// else on the server, and nothing on a neighbouring server.
#[test]
fn a_pre_approved_tool_never_covers_another() {
    let allow = auto_approve_rules(&cfg("gh", &["read_thing"])).unwrap();
    assert!(is_allowed("mcp__gh__read_thing", &allow));
    // A sibling tool on the same server still asks.
    assert!(!is_allowed("mcp__gh__write_thing", &allow));
    // So does a tool whose name merely starts with the approved one.
    assert!(!is_allowed("mcp__gh__read_thing_else", &allow));
    // And a different server: the rule is scoped to `gh`, not to the tool name.
    assert!(!is_allowed("mcp__gtihub__read_thing", &allow));
    assert!(!is_allowed("mcp__ghx__read_thing", &allow));
}

/// Pre-approving a whole server has to be spelled out one tool at a time. A
/// wildcard is refused rather than honoured, because a `*` that can span the
/// `__` separators is exactly the widening this must never do.
#[test]
fn a_wildcard_entry_is_refused_not_widened() {
    for bad in ["*", "read*", "mcp__gh__*", "*_thing", "read_thing*"] {
        assert!(
            auto_approve_rules(&cfg("gh", &[bad])).is_err(),
            "a wildcard entry {bad:?} must be rejected, not expanded"
        );
    }
    // The one safe spelling of "the whole server" is every tool, listed.
    let rules = auto_approve_rules(&cfg("gh", &["read", "write"])).unwrap();
    assert_eq!(patterns(&rules).len(), 2);
    assert!(is_allowed("mcp__gh__read", &rules) && is_allowed("mcp__gh__write", &rules));
    assert!(!is_allowed("mcp__gh__delete", &rules));
}

/// A full `mcp__…` wire name is refused too: entries are scoped to their server
/// already, so accepting one would let a stray name target a rule at a server
/// the user was not editing.
#[test]
fn a_full_wire_name_is_refused() {
    assert!(auto_approve_rules(&cfg("gh", &["mcp__gh__read_thing"])).is_err());
    assert!(
        auto_approve_rules(&cfg("gh", &["read thing"])).is_err(),
        "spaces are not a tool name"
    );
    assert!(
        auto_approve_rules(&cfg("gh", &[""])).is_err(),
        "an empty entry is a mistake, not a no-op"
    );
}

/// Duplicates collapse, so a re-saved list cannot grow two identical rules.
#[test]
fn duplicate_entries_collapse_to_one_rule() {
    let rules = auto_approve_rules(&cfg("gh", &["read", "read", " write "])).unwrap();
    assert_eq!(
        patterns(&rules),
        vec!["mcp__gh__read *", "mcp__gh__write *"]
    );
}

// ───────────────────────── the settings boundary ─────────────────────────

/// The boundary expansion is what actually puts rules in front of the gate.
#[test]
fn the_boundary_expansion_lands_rules_the_gate_honours() {
    let mut allow: Vec<AllowRule> = vec![];
    sync_auto_approve(&mut allow, &[cfg("gh", &["read_thing"])]);
    assert!(is_allowed("mcp__gh__read_thing", &allow));
    assert!(
        !is_allowed("mcp__gh__write_thing", &allow),
        "only the named tool is approved"
    );
}

/// It runs at boot and on every settings write, so a second run must change
/// nothing — no duplicates, and above all no rule taken away.
#[test]
fn running_the_boundary_expansion_twice_changes_nothing() {
    let servers = [cfg("gh", &["read"]), cfg("ctx7", &["get_docs"])];
    let mut allow: Vec<AllowRule> = vec![];
    sync_auto_approve(&mut allow, &servers);
    let once = patterns(&allow);
    sync_auto_approve(&mut allow, &servers);
    assert_eq!(patterns(&allow), once, "reconciling again must be a no-op");
}

/// A pre-existing hand-typed rule survives the reconcile. Deleting rules that
/// are merely absent from `auto_approve` would silently revoke a permission the
/// user wrote themselves, so the boundary step only ever adds.
#[test]
fn reconciling_never_revokes_a_rule_it_did_not_add() {
    let hand = AllowRule {
        pattern: "mcp__gh__read_thing *".into(),
        project: String::new(),
    };
    let mut allow = vec![hand.clone()];
    sync_auto_approve(&mut allow, &[cfg("gh", &["write_thing"])]);
    assert!(
        allow.iter().any(|r| r.pattern == hand.pattern),
        "an unrelated config must not drop a rule the user wrote"
    );
    assert!(
        is_allowed("mcp__gh__write_thing", &allow),
        "the new entry is added"
    );
}

/// Retraction is the UI's job and it works: drop the entry, drop the rule, and
/// the gate asks again. (The boundary step is add-only, so it must not put the
/// rule back either.)
#[test]
fn removing_an_entry_and_its_rule_actually_revokes() {
    let mut allow: Vec<AllowRule> = vec![];
    sync_auto_approve(&mut allow, &[cfg("gh", &["read_thing"])]);
    assert!(is_allowed("mcp__gh__read_thing", &allow));

    // What the MCP tab sends on a chip removal: the rule filtered out, and the
    // entry gone from the config. Then the boundary step runs on that payload.
    allow.retain(|r| r.pattern != "mcp__gh__read_thing *");
    sync_auto_approve(&mut allow, &[cfg("gh", &[])]);
    assert!(
        !is_allowed("mcp__gh__read_thing", &allow),
        "a revoked pre-approval must ask again"
    );
}

/// An invalid list pre-approves nothing at all for that server — a typo fails
/// closed, and the visible error is the server's own `status`.
#[test]
fn an_invalid_entry_pre_approves_nothing_for_that_server() {
    let mut allow: Vec<AllowRule> = vec![];
    sync_auto_approve(&mut allow, &[cfg("gh", &["read", "*"])]);
    assert!(
        !is_allowed("mcp__gh__read", &allow),
        "a list that does not parse grants nothing, not the entries that did"
    );
}

/// Pre-approved is not un-gated: plan mode denies an MCP tool before the allow
/// list is even consulted, so a pre-approval written in another mode cannot make
/// a plan-mode run reach out.
#[test]
fn a_pre_approved_tool_is_still_denied_in_plan_mode() {
    let allow = auto_approve_rules(&cfg("gh", &["read_thing"])).unwrap();
    let plan = Ctx {
        plan: true,
        ..gate_ctx(&allow)
    };
    assert!(
        matches!(
            check("mcp__gh__read_thing", &json!({}), None, &plan),
            Decision::Deny(_)
        ),
        "plan mode must refuse a pre-approved MCP tool"
    );
}

// ───────────────────────── server names ─────────────────────────

/// The delimiter cannot carry a name with an underscore: `split_tool` cuts at
/// the first `__`, so these names mis-parse. This is the proof the rejection
/// below is needed, stated as the mis-parse it prevents.
#[test]
fn an_underscored_name_would_be_mis_parsed_by_the_delimiter() {
    // A name containing `__` splits a character early: server `add__x`, tool
    // `t` reads back as server `add`, tool `x__t` — the call would run on the
    // wrong server while the rule checked was for `add__x`.
    assert_eq!(split_tool(&wire_tool("add__x", "t")), Some(("add", "x__t")));
    assert_ne!(split_tool(&wire_tool("add__x", "t")), Some(("add__x", "t")));
    // A name ending in `_` mis-splits the same way, one character early.
    assert_eq!(split_tool(&wire_tool("add_", "t")), Some(("add", "_t")));
    // A clean name is exact, and a tool's own `__` is preserved.
    assert_eq!(
        split_tool(&wire_tool("gh", "read_thing")),
        Some(("gh", "read_thing"))
    );
    assert_eq!(split_tool(&wire_tool("gh", "a__b")), Some(("gh", "a__b")));
    // Non-MCP names are rejected outright rather than guessed at.
    assert_eq!(split_tool("read_file"), None);
    assert_eq!(
        split_tool("mcp__gh"),
        None,
        "a name with no tool is not a tool"
    );
}

/// So an underscored name — or any character the wire name cannot carry — is
/// refused with a readable error rather than accepted and mis-parsed.
#[test]
fn a_name_the_delimiter_cannot_carry_is_refused() {
    for bad in ["my_server", "add__x", "add_", "_gh", "a.b", "a b", "gh!"] {
        assert!(
            server_name_error(bad).is_some(),
            "server name {bad:?} must be refused"
        );
    }
    // And the refusal propagates: a bad name cannot produce an expansion either.
    assert!(auto_approve_rules(&cfg("my_server", &["read"])).is_err());
    // An empty name is refused too.
    assert!(server_name_error("").is_some());
    assert!(server_name_error("   ").is_some());
}

/// Letters, digits and dashes are exactly what the wire name carries, so they
/// are accepted — the restriction is not gratuitously wider than the delimiter.
#[test]
fn a_clean_name_is_accepted() {
    for good in ["gh", "ctx7", "my-server", "A1", "github"] {
        assert!(
            server_name_error(good).is_none(),
            "server name {good:?} should be fine"
        );
    }
}

/// Once a clean name is through, the wire name is exactly
/// `mcp__<name>__<tool>` with the first `__` the separator: what the model sees,
/// what the rule is written against, and what routes the call all agree.
#[test]
fn a_clean_name_round_trips_through_the_wire_format() {
    for (server, tool) in [("gh", "read_thing"), ("my-server", "a__b"), ("ctx7", "get")] {
        assert_eq!(split_tool(&wire_tool(server, tool)), Some((server, tool)));
    }
}
