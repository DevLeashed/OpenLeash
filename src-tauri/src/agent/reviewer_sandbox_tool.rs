//! Permission and scoping tests for the `sandbox` tool.
//!
//! A sandbox is the agent's own container, so using one needs no approval, but
//! it is still an action: plan mode and the Disabled rung must refuse it, a
//! deny rule must beat Turbo, read-only subagents must not be offered it, and
//! `create` (a ~1 GB image build plus a container) asks first.

#![cfg(test)]

use super::permissions::{check, Ctx, Decision};
use super::plugins;
use super::store::AllowRule;
use serde_json::json;

fn rule(p: &str) -> AllowRule {
    AllowRule {
        pattern: p.into(),
        project: String::new(),
    }
}

fn run(perm: &str, plan: bool, allow: &[AllowRule], input: serde_json::Value) -> Decision {
    check(
        "sandbox",
        &input,
        None,
        &Ctx {
            perm,
            plan,
            cwd: ".",
            project: ".",
            allow,
        },
    )
}

fn is_allow(d: &Decision) -> bool {
    matches!(d, Decision::Allow)
}
fn is_deny(d: &Decision) -> bool {
    matches!(d, Decision::Deny(_))
}
fn is_ask(d: &Decision) -> bool {
    matches!(d, Decision::Ask { .. })
}

#[test]
fn plan_mode_refuses_every_sandbox_action_but_the_list() {
    for op in ["create", "use", "start", "stop", "release", "delete", "exec", "publish"] {
        let d = run("ask", true, &[], json!({"op": op, "name": "x"}));
        assert!(is_deny(&d), "{op} must be refused in plan mode");
    }
    // Typing at the sandbox's screen is an action; looking at it is not gated
    // by the runner, but a batch that clicks is `screen` and must be refused.
    let click = json!({"op": "screen", "actions": [{"type": "click", "x": 1, "y": 1}]});
    assert!(is_deny(&run("turbo", true, &[], click)));
    assert!(is_allow(&run("ask", true, &[], json!({"op": "list"}))));
}

#[test]
fn the_disabled_rung_refuses_sandbox_actions() {
    assert!(is_deny(&run("disabled", false, &[], json!({"op": "exec", "command": "ls"}))));
    assert!(is_allow(&run("disabled", false, &[], json!({"op": "list"}))));
}

#[test]
fn a_deny_rule_beats_turbo_and_can_name_one_operation() {
    let all = [rule("!sandbox *")];
    for op in ["list", "exec", "create", "screen"] {
        assert!(
            is_deny(&run("turbo", false, &all, json!({"op": op}))),
            "!sandbox * must deny {op}"
        );
    }
    let exec_only = [rule("!sandbox:exec")];
    assert!(is_deny(&run("turbo", false, &exec_only, json!({"op": "exec", "command": "x"}))));
    assert!(is_allow(&run("turbo", false, &exec_only, json!({"op": "screen"}))));
}

#[test]
fn create_asks_but_working_inside_a_sandbox_does_not() {
    assert!(is_ask(&run("ask", false, &[], json!({"op": "create", "name": "Box"}))));
    assert!(is_allow(&run("turbo", false, &[], json!({"op": "create", "name": "Box"}))));
    assert!(is_allow(&run(
        "ask",
        false,
        &[rule("sandbox:create")],
        json!({"op": "create"})
    )));
    for op in ["use", "start", "stop", "release", "exec", "screen", "publish"] {
        assert!(
            is_allow(&run("ask", false, &[], json!({"op": op}))),
            "{op} needs no approval: it only touches the sandbox"
        );
    }
}

#[test]
fn delete_asks_unless_allowed_and_a_deny_rule_wins() {
    let del = json!({"op": "delete", "sandbox": "Test desktop"});
    assert!(is_ask(&run("ask", false, &[], del.clone())));
    assert!(is_ask(&run("auto", false, &[], del.clone())));
    assert!(is_allow(&run("turbo", false, &[], del.clone())));
    assert!(is_allow(&run("ask", false, &[rule("sandbox:delete")], del.clone())));
    assert!(is_deny(&run("turbo", false, &[rule("!sandbox:delete")], del.clone())));
    assert!(is_deny(&run("disabled", false, &[], del)));
}

#[test]
fn read_only_subagents_are_not_offered_the_tool() {
    assert!(!plugins::offered_to_sub("read_only", "sandbox"));
    assert!(plugins::offered_to_sub("all", "sandbox"));
}

#[test]
fn sandboxes_are_built_in_and_not_a_plugin() {
    // Always offered, whatever the plugin switches say, and old settings files
    // (which know nothing of sandboxes) still load.
    let c = plugins::PluginsCfg::default();
    assert!(plugins::schemas(&c).iter().any(|s| s["name"] == "sandbox"));
    let old: plugins::PluginsCfg = serde_json::from_str(r#"{"computer":{}}"#).unwrap();
    assert!(plugins::schemas(&old).iter().any(|s| s["name"] == "sandbox"));
}

#[test]
fn the_host_computer_toggle_does_not_gate_the_sandbox_screen() {
    // Whatever the host toggle says, the sandbox tool carries its own screen actions.
    for computer in [false, true] {
        let mut c = plugins::PluginsCfg::default();
        c.computer.enabled = computer;
        let tools = plugins::schemas(&c);
        let sb = tools.iter().find(|s| s["name"] == "sandbox").unwrap();
        assert!(sb["input_schema"]["properties"]["actions"]["items"].is_object());
        assert_eq!(tools.iter().any(|s| s["name"] == "computer"), computer);
    }
}
