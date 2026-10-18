//! Release-gate tests for the service plugins' permission boundary: Linear,
//! Jira, Slack, Notion and GitLab.
//!
//! Each of these tools acts on a third-party service *as the user*, with the
//! user's token. The boundary mirrors GitHub's: reads run, writes ask with a
//! per-action `"<tool>:<action>"` rule, plan mode refuses writes, deny rules
//! win over everything, and anything that cannot be classified is a write.
//!
//! Like `reviewer_permissions.rs`, these only call the pure `check` function
//! and the hub's pure helpers: no network, no env, no filesystem writes.

#![cfg(test)]

use super::permissions::{check, Ctx, Decision};
use super::plugin_services::{self, SERVICES};
use super::store::AllowRule;
use serde_json::{json, Value};

const PROJECT: &str = "D:/openleash";

fn ctx<'a>(perm: &'a str, plan: bool, allow: &'a [AllowRule]) -> Ctx<'a> {
    Ctx {
        perm,
        plan,
        cwd: PROJECT,
        project: PROJECT,
        allow,
    }
}

fn rule(pattern: &str, project: &str) -> AllowRule {
    AllowRule {
        pattern: pattern.into(),
        project: project.into(),
    }
}

/// One representative read and one write per service.
fn cases() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "linear",
            json!({"action": "list_issues", "team": "ENG"}),
            json!({"action": "create_issue", "team": "ENG", "title": "Bug"}),
        ),
        (
            "jira",
            json!({"action": "search", "jql": "project = X"}),
            json!({"action": "transition", "key": "X-1", "transition": "Done"}),
        ),
        (
            "slack",
            json!({"action": "history", "channel": "C123"}),
            json!({"action": "post_message", "channel": "C123", "text": "hi"}),
        ),
        (
            "notion",
            json!({"action": "search", "query": "roadmap"}),
            json!({"action": "update_page", "page_id": "abc", "properties": {}}),
        ),
        (
            "gitlab",
            json!({"action": "list_mrs", "project": "g/p"}),
            json!({"action": "retry_pipeline", "project": "g/p", "id": 7}),
        ),
    ]
}

/// `Decision` has no `Debug`; this is what a failing assertion prints.
fn show(d: &Decision) -> String {
    match d {
        Decision::Allow => "Allow".into(),
        Decision::Deny(r) => format!("Deny({r:?})"),
        Decision::Ask {
            title,
            detail,
            reason,
            rule,
        } => format!("Ask(title={title:?}, detail={detail:?}, reason={reason:?}, rule={rule:?})"),
    }
}

fn is_ask(d: &Decision) -> bool {
    matches!(d, Decision::Ask { .. })
}

#[test]
fn reads_run_without_asking_on_every_rung() {
    for (tool, read, _) in cases() {
        for perm in ["ask", "auto", "reviewer", "disabled", "full"] {
            let d = check(tool, &read, None, &ctx(perm, false, &[]));
            assert!(
                matches!(d, Decision::Allow),
                "{tool} read on {perm}: {}",
                show(&d)
            );
        }
        // Plan mode is about not *doing* things, not about not looking.
        let d = check(tool, &read, None, &ctx("ask", true, &[]));
        assert!(
            matches!(d, Decision::Allow),
            "{tool} read in plan mode: {}",
            show(&d)
        );
    }
}

#[test]
fn writes_ask_with_a_named_per_action_rule() {
    for (tool, _, write) in cases() {
        let action = write["action"].as_str().unwrap();
        let d = check(tool, &write, None, &ctx("ask", false, &[]));
        let Decision::Ask {
            title,
            detail,
            reason,
            rule,
        } = &d
        else {
            panic!("{tool} {action} must ask, got {}", show(&d));
        };
        let label = plugin_services::label(tool);
        assert_eq!(*title, format!("{label} · {}", action.replace('_', " ")));
        assert_eq!(rule.as_deref(), Some(format!("{tool}:{action}").as_str()));
        assert!(reason.contains(label), "{reason}");
        // The detail shows what will be sent, without repeating the action.
        assert!(!detail.contains("\"action\""), "{detail}");
        assert!(detail.len() > 2, "{detail}");
        // Auto-edit does not cover a third-party write either.
        assert!(is_ask(&check(tool, &write, None, &ctx("auto", false, &[]))));
    }
}

#[test]
fn plan_mode_refuses_writes_even_with_full_access_or_an_allow_rule() {
    for (tool, _, write) in cases() {
        let action = write["action"].as_str().unwrap();
        let allow = [rule(&format!("{tool}:{action}"), "")];
        for c in [ctx("full", true, &[]), ctx("ask", true, &allow)] {
            let d = check(tool, &write, None, &c);
            assert!(
                matches!(d, Decision::Deny(_)),
                "{tool} {action} in plan: {}",
                show(&d)
            );
        }
    }
}

#[test]
fn full_access_runs_writes_and_disabled_refuses_them() {
    for (tool, _, write) in cases() {
        assert!(matches!(
            check(tool, &write, None, &ctx("full", false, &[])),
            Decision::Allow
        ));
        assert!(matches!(
            check(tool, &write, None, &ctx("disabled", false, &[])),
            Decision::Deny(_)
        ));
    }
}

#[test]
fn an_always_rule_covers_its_action_and_project_only() {
    for (tool, _, write) in cases() {
        let action = write["action"].as_str().unwrap();
        let own = [rule(&format!("{tool}:{action}"), PROJECT)];
        assert!(matches!(
            check(tool, &write, None, &ctx("ask", false, &own)),
            Decision::Allow
        ));
        // The same action in another project is a separate grant.
        let elsewhere = [rule(&format!("{tool}:{action}"), "D:/other")];
        assert!(is_ask(&check(
            tool,
            &write,
            None,
            &ctx("ask", false, &elsewhere)
        )));
        // A grant for one action is not a grant for another.
        let mut other = write.clone();
        other["action"] = json!(match tool {
            "linear" => "comment",
            "jira" => "comment",
            "slack" => "react",
            "notion" => "append_blocks",
            _ => "comment",
        });
        assert!(is_ask(&check(tool, &other, None, &ctx("ask", false, &own))));
        // `tool *` covers every write of that tool.
        let all = [rule(&format!("{tool} *"), "")];
        assert!(matches!(
            check(tool, &other, None, &ctx("ask", false, &all)),
            Decision::Allow
        ));
        // And one service's rule never covers another's.
        let wrong = [rule("github *", ""), rule(&format!("github:{action}"), "")];
        assert!(is_ask(&check(
            tool,
            &write,
            None,
            &ctx("ask", false, &wrong)
        )));
    }
}

#[test]
fn deny_rules_win_over_full_access_for_reads_and_writes() {
    for (tool, read, write) in cases() {
        let action = write["action"].as_str().unwrap();
        let one = [rule(&format!("!{tool}:{action}"), "")];
        assert!(matches!(
            check(tool, &write, None, &ctx("full", false, &one)),
            Decision::Deny(_)
        ));
        // A deny on one action leaves the reads alone.
        assert!(matches!(
            check(tool, &read, None, &ctx("full", false, &one)),
            Decision::Allow
        ));
        // `!tool *` denies the whole tool, reads included.
        let whole = [rule(&format!("!{tool} *"), "")];
        for input in [&read, &write] {
            assert!(matches!(
                check(tool, input, None, &ctx("full", false, &whole)),
                Decision::Deny(_)
            ));
        }
        // A deny on a read action is honoured too.
        let read_action = read["action"].as_str().unwrap();
        let deny_read = [rule(&format!("!{tool}:{read_action}"), "")];
        assert!(matches!(
            check(tool, &read, None, &ctx("full", false, &deny_read)),
            Decision::Deny(_)
        ));
    }
}

/// Raw passthroughs are classified from the request itself, and anything that
/// is not plainly a GET to a relative path on the configured host is a write.
#[test]
fn raw_api_calls_fail_closed() {
    let c = ctx("ask", false, &[]);
    for tool in ["jira", "gitlab"] {
        let read = json!({"action": "api", "path": "/rest/api/3/myself"});
        assert!(
            matches!(check(tool, &read, None, &c), Decision::Allow),
            "{tool}"
        );
        let explicit_get = json!({"action": "api", "method": "get", "path": "projects"});
        assert!(matches!(
            check(tool, &explicit_get, None, &c),
            Decision::Allow
        ));
        for input in [
            json!({"action": "api", "method": "POST", "path": "/x"}),
            json!({"action": "api", "method": "DELETE", "path": "/x"}),
            json!({"action": "api", "method": "BREW", "path": "/x"}),
            json!({"action": "api", "method": 7, "path": "/x"}),
            json!({"action": "api", "path": "https://evil.com/x"}),
            json!({"action": "api", "path": "//evil.com/x"}),
            json!({"action": "api", "path": "\\\\evil.com\\x"}),
            json!({"action": "api", "path": "%2f%2fevil.com"}),
            json!({"action": "api"}),
        ] {
            assert!(is_ask(&check(tool, &input, None, &c)), "{tool} {input}");
        }
        // And plan mode refuses them, like any other write.
        assert!(matches!(
            check(
                tool,
                &json!({"action": "api", "method": "PUT", "path": "/x"}),
                None,
                &ctx("ask", true, &[])
            ),
            Decision::Deny(_)
        ));
    }
    // Linear's raw passthrough is GraphQL: a mutation is a write, a query a read.
    let m =
        json!({"action": "graphql", "query": "mutation { issueDelete(id: \"x\") { success } }"});
    assert!(is_ask(&check("linear", &m, None, &c)));
    let q = json!({"action": "graphql", "query": "query { viewer { name } } # mutation"});
    assert!(matches!(check("linear", &q, None, &c), Decision::Allow));
    let hidden = json!({"action": "graphql", "query": "query A { viewer { id } } mutation B { issueDelete(id: \"x\") { success } }"});
    assert!(is_ask(&check("linear", &hidden, None, &c)));
}

/// An action nobody classified must never be treated as a read.
#[test]
fn unknown_actions_ask() {
    for tool in SERVICES {
        for input in [
            json!({"action": "nuke_everything"}),
            json!({}),
            json!({"action": 5}),
        ] {
            assert!(
                is_ask(&check(tool, &input, None, &ctx("ask", false, &[]))),
                "{tool} {input}"
            );
            assert!(matches!(
                check(tool, &input, None, &ctx("ask", true, &[])),
                Decision::Deny(_)
            ));
        }
    }
}

/// The plugin snapshot a task keeps (and writes to its task file) carries no
/// credentials, GitHub's included.
#[test]
fn the_task_snapshot_carries_no_tokens() {
    let mut c = super::plugins::PluginsCfg::default();
    c.github.token = "ghp_secret_value".into();
    for (p, f) in plugin_services::SECRET_FIELDS {
        plugin_services::set_secret(&mut c, p, f, &format!("tok-{p}-{f}-secret")).unwrap();
    }
    let snap = serde_json::to_string(&c.without_secrets()).unwrap();
    assert!(!snap.contains("secret"), "{snap}");
}
