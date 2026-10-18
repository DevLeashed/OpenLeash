//! Prompt rules that must reach *every* agent kind.
//!
//! The frozen prefix picks one of two rule blocks — `CORE` for the main agent,
//! `SUB_CORE` for a subagent — so any working rule that matters to both lives
//! twice, in two blocks nothing keeps in sync. The rule that every command
//! already starts in the agent's working directory is the one that bit: it was
//! only in `CORE`, so subagents (which is where long-running background
//! commands actually get written) were never told, and prefixed every command
//! with `cd "<the directory the prompt had just shown them>"`.

use super::prompt::{self, Env};
use super::store::AgentDef;

fn env() -> Env<'static> {
    Env {
        cwd: "D:/openleash",
        project: "D:/openleash",
        branch: "main",
        is_git: true,
        worktree: false,
        date: "2026-01-01".into(),
        agent_id: "ol-test",
        inject_global_claude: false,
        disabled_docs: &[],
    }
}

fn sub() -> AgentDef {
    AgentDef {
        id: "general".into(),
        name: "General".into(),
        description: "d".into(),
        tools: "all".into(),
        ..Default::default()
    }
}

/// The no-`cd` rule has to be in both rule blocks, and say the same thing,
/// because `prompt::system` picks exactly one per agent. Assert the rule by its
/// substance rather than its wording, so rewording either block doesn't fail
/// this; what must not come back is an agent kind that never hears it.
#[test]
fn main_prompt_explains_inline_visualization_behavior_and_safety() {
    let sys = prompt::system(&env(), None, &[]);
    for rule in [
        "purposeful interactive chart",
        "labeled exactly `openleash-viz`",
        "runs automatically once the complete fence is received",
        "Working on the interactive preview",
        "Source button is only for users who choose to inspect the code",
        "ordinary code fences are welcome",
        "Only a closed, exact `openleash-viz` fence in an assistant reply executes",
        // A diagram the model writes without the fence is still drawn, so the
        // rule has to name that case: it is the one difference from "raw HTML is
        // inert", and an agent that does not know it will distrust the display.
        "which is read as a diagram",
        "block in your reply is read as that same inert diagram",
        "no network, external libraries/resources",
        "never put secrets or sensitive data",
        // Charts: the declarative fence, the host-only hint key, and the one
        // library path an `openleash-viz` document may load.
        "closed top-level `openleash-chart` fence",
        "Apache ECharts 6 option",
        "\"openleash\": {\"height\"",
        "<script src=\"lib/echarts.js\"></script>",
        "the only external script the frame allows",
        "the one exception is the bundled chart engine",
    ] {
        assert!(
            sys.contains(rule),
            "main prompt is missing visualization rule {rule:?}.\n---\n{sys}\n---"
        );
    }
    assert!(
        !sys.contains("explicitly clicks Preview"),
        "main prompt still tells the agent to make users click Preview"
    );
}

#[test]
fn handoffs_preserve_verification_receipts_and_unresolved_failures() {
    let handoff = prompt::compact_prompt();
    for rule in [
        "Verification receipts",
        "exact commands",
        "filters/scope",
        "exit codes",
        "evidence paths",
        "owner-accepted",
        "unresolved failure",
        "isolated rerun",
        "unverified",
        "IDs",
        "authorization",
    ] {
        assert!(handoff.contains(rule), "missing handoff rule: {rule}");
    }
    let sys = prompt::system(&env(), None, &[]);
    assert!(sys.contains("compare completion claims against observed tool results"));
    assert!(sys.contains("label partial results explicitly"));
}

#[test]
fn every_agent_kind_gets_guidance_to_recover_interrupted_subagents_without_duplicates() {
    for (who, sys) in [
        ("main", prompt::system(&env(), None, &[])),
        ("subagent", prompt::system(&env(), Some(&sub()), &[])),
    ] {
        for rule in [
            "does not necessarily lose delegated progress",
            "old processes are gone",
            "check current subagent status and existing reports",
            "wait for workers that are still running",
            "do not survive a full app restart",
        ] {
            assert!(
                sys.contains(rule),
                "the {who} prompt is missing interrupted-work guidance {rule:?}.\n---\n{sys}\n---"
            );
        }
    }
}

#[test]
fn every_agent_kind_is_told_commands_start_in_their_working_directory() {
    for (who, sys) in [
        ("main", prompt::system(&env(), None, &[])),
        ("subagent", prompt::system(&env(), Some(&sub()), &[])),
    ] {
        assert!(
            sys.contains("already starts in"),
            "the {who} prompt never says commands start in the working directory.\n---\n{sys}\n---"
        );
        assert!(
            sys.contains("never begin with `cd`"),
            "the {who} prompt does not forbid the leading `cd`.\n---\n{sys}\n---"
        );
    }
}
