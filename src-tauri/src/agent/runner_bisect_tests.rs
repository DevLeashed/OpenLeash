//! Permission parity for the `bisect_edits` agent tool, through the real
//! `Agent::exec_inner` dispatch.
//!
//! The tool runs a shell command, so it must be refused exactly where `bash`
//! with the same command is refused — the same message, from the same checks:
//! deny rules, plan mode, the Disabled rung with chained commands, pipes,
//! redirects and command substitution, a subagent's read-only / no-shell /
//! fileRegex limits. Every refused command here would write a marker file if
//! it ran, and the chat has a real edit history, so a bypassed gate would run
//! it rather than fail for some unrelated reason.

#![allow(clippy::await_holding_lock)]

use super::*;
use crate::agent::reviewer_bisect::{history, home, refused_cases, CHECK};
use crate::agent::tests::{harness, task};

fn agent_for(
    wt: &str,
    chat: &str,
    perm: &str,
    plan: bool,
    allow: &[&str],
    sub: Option<AgentDef>,
) -> Agent {
    let mut t = task(chat, "unused/model");
    t.cwd = wt.to_string();
    t.project = wt.to_string();
    t.perm = perm.into();
    t.plan = plan;
    t.checkpoints = vec![super::super::Checkpoint {
        item_index: 0,
        msg_index: 0,
    }];
    let s = store::Settings {
        allow: allow
            .iter()
            .map(|p| AllowRule {
                pattern: p.to_string(),
                project: String::new(),
            })
            .collect(),
        ..Default::default()
    };
    let (h, _) = harness(s, vec![t]);
    Agent {
        h,
        task_id: chat.into(),
        sub: sub.map(|def| SubCtx {
            id: "worker".into(),
            def,
        }),
        cancel: CancellationToken::new(),
    }
}

async fn both(agent: &Agent, cmd: &str) -> (Result<String, String>, Result<String, String>) {
    let input = json!({"command": cmd});
    let bash = agent.exec_inner("bash", &input, None, "bash-call").await;
    let bisect = agent
        .exec_inner("bisect_edits", &input, None, "bisect-call")
        .await;
    (bash, bisect)
}

#[tokio::test]
async fn bisect_edits_is_denied_exactly_where_bash_is() {
    let (home, _g) = home("agentperm");
    let chat = "agentperm";
    let wt = history(&home, chat, 3, 2);
    let marker = home.join("ran.txt").to_string_lossy().replace('\\', "/");
    for (perm, plan, allow, cmd) in refused_cases(&marker) {
        let agent = agent_for(&wt, chat, perm, plan, &allow, None);
        let (bash, bisect) = both(&agent, &cmd).await;
        let bash = bash.expect_err("fixture: bash must refuse this");
        assert_eq!(
            bisect.err().as_deref(),
            Some(bash.as_str()),
            "`{cmd}` ({perm}, plan={plan}): bisect_edits must give bash's refusal"
        );
        assert!(!std::path::Path::new(&marker).exists(), "`{cmd}` ran");
    }
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn bisect_edits_obeys_subagent_shell_limits() {
    let (home, _g) = home("subperm");
    let chat = "subperm";
    let wt = history(&home, chat, 3, 2);
    let marker = home.join("ran.txt").to_string_lossy().replace('\\', "/");
    let cmd = format!("echo pwned > {marker}");

    // Read-only worker: only read-only commands, same message as bash.
    let ro = AgentDef {
        id: "explore".into(),
        tools: "read_only".into(),
        ..Default::default()
    };
    let agent = agent_for(&wt, chat, "full", false, &[], Some(ro));
    let (bash, bisect) = both(&agent, &cmd).await;
    assert_eq!(bisect.err(), bash.err());
    assert!(!std::path::Path::new(&marker).exists());

    // No-shell worker: refused outright.
    let ns = AgentDef {
        id: "writer".into(),
        tools: "no_shell".into(),
        ..Default::default()
    };
    let agent = agent_for(&wt, chat, "full", false, &[], Some(ns));
    let (bash, bisect) = both(&agent, &cmd).await;
    assert!(bash.unwrap_err().contains("can't use bash"));
    assert!(bisect.unwrap_err().contains("can't use bisect_edits"));

    // A fileRegex worker may not use bash, so it may not bisect either.
    let fr = AgentDef {
        id: "docs".into(),
        name: "Docs".into(),
        groups: vec![store::ToolGroup::Restricted((
            "edit".into(),
            store::GroupOpts {
                file_regex: r"^docs/.*\.md$".into(),
                description: "docs only".into(),
            },
        ))],
        ..Default::default()
    };
    let agent = agent_for(&wt, chat, "full", false, &[], Some(fr));
    let (bash, bisect) = both(&agent, &cmd).await;
    let bash = bash.unwrap_err();
    assert!(bash.contains("FileRestrictionError"));
    assert_eq!(bisect.err(), Some(bash));
    assert!(!std::path::Path::new(&marker).exists());
    let _ = std::fs::remove_dir_all(home);
}

/// The allowed path works through dispatch too, and a read-only command is
/// allowed even on the Disabled rung — as it is for bash.
#[tokio::test]
async fn bisect_edits_runs_through_dispatch_when_bash_would() {
    let (home, _g) = home("agentrun");
    let chat = "agentrun";
    let wt = history(&home, chat, 5, 4);
    let agent = agent_for(&wt, chat, "full", false, &[], None);
    let denied = agent_for(&wt, chat, "full", false, &["!bisect_edits"], None);
    let error = denied
        .exec_inner(
            "bisect_edits",
            &json!({"command": "git --version"}),
            None,
            "named-deny",
        )
        .await
        .unwrap_err();
    assert!(error.contains("!bisect_edits"), "{error}");
    let out = agent
        .exec_inner("bisect_edits", &json!({"command": CHECK}), None, "call")
        .await
        .expect("allowed bisect should run");
    assert!(out.contains("Culprit: step 4"), "{out}");
    assert!(out.contains("check.txt"));
    let agent = agent_for(&wt, chat, "disabled", true, &[], None);
    let out = agent
        .exec_inner("bisect_edits", &json!({"command": "ls"}), None, "call")
        .await
        .expect("a read-only command runs on the Disabled rung and in plan mode");
    assert!(out.contains("no step in 1..5 broke it"), "{out}");
    let _ = std::fs::remove_dir_all(home);
}
