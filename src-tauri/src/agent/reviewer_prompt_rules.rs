//! Prompt rules that must reach *every* agent kind.
//!
//! The frozen prefix picks one of two rule blocks — `CORE` for the main agent,
//! `SUB_CORE` for a sub-agent — so any working rule that matters to both lives
//! twice, in two blocks nothing keeps in sync. The rule that every command
//! already starts in the agent's working directory is the one that bit: it was
//! only in `CORE`, so sub-agents (which is where long-running background
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
        "no network, external libraries/resources",
        "never put secrets or sensitive data",
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
fn every_agent_kind_is_told_commands_start_in_their_working_directory() {
    for (who, sys) in [
        ("main", prompt::system(&env(), None, &[])),
        ("sub-agent", prompt::system(&env(), Some(&sub()), &[])),
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
