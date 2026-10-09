//! Release-gate tests for what a *disabled* plugin leaves in the agent's context.
//!
//! The toggles were never broken in the way they look broken: `plugins::schemas`
//! filters every plugin schema on `enabled`, and each `exec_*` handler re-checks
//! and refuses. A disabled plugin genuinely cannot be called.
//!
//! What it did leave behind was *prose*. The screen section was a fixed block in
//! `prompt::CORE`, so every agent was told to "prefer the headless equivalent"
//! and handed the names `browser`, `render` and `computer` whether or not those
//! plugins existed. `screenshot` was worse: a *core* tool, so it sat in the tool
//! list unconditionally while `exec_screenshot` refuses when the computer plugin
//! is off. The prompt was steering the model into a tool whose only possible
//! result was an error, and telling users who had switched computer use off that
//! they had it.
//!
//! These tests are about the *agent-facing* context — the system prompt and the
//! tool list — not about the handlers. The gating was never the bug; the
//! *advertising* was.
//!
//! Note what is deliberately NOT asserted: that the strings `github` or `browser`
//! appear nowhere in the assembled context. They never could. The harness's own
//! instructions enumerate every tool by name for the model, and a user's
//! `AGENTS.md` can say anything. The property that actually matters is narrower:
//! the system prompt must not steer the model towards a plugin that is off, and a
//! disabled plugin must not be offered as a callable tool.

use super::plugins::{BrowserCfg, ComputerCfg, GithubCfg, PluginsCfg};
use super::{prompt, tools};

fn cfg(github: bool, computer: bool, browser: bool) -> PluginsCfg {
    PluginsCfg {
        github: GithubCfg {
            enabled: github,
            ..Default::default()
        },
        computer: ComputerCfg {
            enabled: computer,
            ..Default::default()
        },
        browser: BrowserCfg {
            enabled: browser,
            ..Default::default()
        },
    }
}

/// A shared environment for the prompt-only checks, matching what the prefix
/// builders hand `prompt::system`.
fn env() -> prompt::Env<'static> {
    prompt::Env {
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

/// The system prompt alone, as the frozen prefix builds it: core plus the
/// plugin-built sections.
fn system_prompt(c: &PluginsCfg) -> String {
    prompt::system(&env(), None, &[]) + &prompt::screen_section(c)
}

/// Every tool name the model is offered: the core list, then the plugin
/// schemas appended to it, which is the order the frozen prefix serves them in.
fn offered(c: &PluginsCfg, sub: Option<&str>) -> Vec<String> {
    let mut v = tools::schemas(sub, c);
    v.extend(super::plugins::schemas(c));
    v.iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect()
}

/// A plugin that is off must not be offered as a callable tool, and the system
/// prompt must not tell the model to reach for one. Both halves matter: the tool
/// list is what the model picks from, and the prompt is what it is steered by.
#[test]
fn a_disabled_plugin_is_neither_offered_nor_recommended() {
    for c in [
        cfg(false, false, false),
        cfg(true, false, false),
        cfg(false, true, false),
        cfg(false, false, false),
        cfg(false, false, true),
        cfg(true, true, true),
    ] {
        let tools_offered = offered(&c, None);
        for (name, on) in [
            ("github", c.github.enabled),
            ("computer", c.computer.enabled),
            ("browser", c.browser.enabled),
        ] {
            if on {
                assert!(
                    tools_offered.iter().any(|n| n == name),
                    "{name} is on but missing from the tool list: {tools_offered:?}"
                );
                continue;
            }
            assert!(
                !tools_offered.iter().any(|n| n == name),
                "{name} is off but still in the tool list: {tools_offered:?}"
            );
        }

        let sys = system_prompt(&c);
        // `github` has no section prose, so there is nothing to steer with.
        for (name, on) in [
            ("browser", c.browser.enabled),
            ("computer", c.computer.enabled),
        ] {
            if on {
                continue;
            }
            assert!(
                !sys.contains(&format!("`{name}`")),
                "{name} is off, but the system prompt names it as a tool.\n---\n{sys}\n---"
            );
        }
    }
}

/// The screen section is about the headless path first and the desktop second,
/// so with neither available there is nothing to say. It must go entirely rather
/// than leaving a heading or a rule behind.
#[test]
fn no_screen_prose_when_no_screen_plugin_is_on() {
    assert_eq!(prompt::screen_section(&cfg(false, false, false)), "");
    assert_eq!(
        prompt::screen_section(&cfg(true, true, false)),
        "",
        "computer use is not a headless tool, so with no browser on there is nothing to steer toward"
    );
    for c in [cfg(false, false, true), cfg(false, true, true)] {
        assert!(
            !prompt::screen_section(&c).is_empty(),
            "{c:?} has a headless or desktop tool and should say something"
        );
    }
}

/// The section names only what is on: `browser` on its own must not advertise
/// `computer`, and vice versa. Getting this wrong is exactly what makes the model
/// reach for a tool it does not have.
#[test]
fn the_screen_section_names_only_the_live_plugins() {
    let browser_only = prompt::screen_section(&cfg(false, false, true));
    assert!(browser_only.contains("`browser`"));
    assert!(!browser_only.contains("`computer`"));

    let computer_only = prompt::screen_section(&cfg(false, true, false));
    assert!(!computer_only.contains("`browser`"));
}

/// With computer use off the model must be told it cannot see the screen, not
/// left to discover it by calling something that refuses.
#[test]
fn computer_off_says_the_screen_is_unavailable() {
    let off = prompt::screen_section(&cfg(false, false, true));
    assert!(
        off.contains("cannot see or drive the user's screen"),
        "expected an explicit statement that the screen is unavailable, got:\n{off}"
    );
    let on = prompt::screen_section(&cfg(false, true, true));
    assert!(on.contains("Drive the user's screen"));
}

/// The rule about faking input from a shell outlives any combination of the
/// toggles — it is the reason a shell is not an escape hatch when the screen
/// tools are off. It must survive in every configuration that has any screen
/// prose at all.
#[test]
fn the_no_fake_input_rule_survives_every_configuration() {
    for c in [cfg(false, false, true), cfg(false, true, true)] {
        let s = prompt::screen_section(&c);
        assert!(
            s.contains("Never synthesise input to the desktop from a shell"),
            "the shell-escape rule went missing for {c:?}"
        );
    }
}

/// `screenshot` is the core tool that captures the user's whole screen, and
/// `exec_screenshot` refuses it when the computer plugin is off. It must
/// therefore be *absent* from the tool list, not merely gated at run time —
/// offering a tool whose only outcome is an error wastes a turn and reads to
/// the model as a broken harness. Checked for every sub-agent policy, since the
/// gate is on the schema list and not on the agent kind.
#[test]
fn screenshot_is_in_the_tool_list_only_with_the_computer_plugin() {
    for (computer, want) in [(false, false), (true, true)] {
        for sub in [None, Some("all"), Some("read_only"), Some("no_shell")] {
            let names = offered(&cfg(false, computer, false), sub);
            assert_eq!(
                names.iter().any(|n| n == "screenshot"),
                want,
                "computer={computer} sub={sub:?} should {} have `screenshot`",
                if want { "" } else { "not " }
            );
        }
    }
}

/// Plan mode used to name the disabled tools by hand ("the github / computer /
/// browser-write / MCP tools"), so a user with every plugin off was still told
/// about them. The refusal list has to describe the *kind* of tool instead.
#[test]
fn plan_mode_does_not_name_plugins() {
    let r = prompt::plan_reminder();
    for name in ["github", "computer", "browser"] {
        assert!(
            !r.contains(&format!("`{name}`")) && !r.contains(&format!(" {name} ")),
            "plan_reminder names `{name}`, which may not exist"
        );
    }
    assert!(
        r.contains("writes to the outside world"),
        "the refusal list lost its description of the external-write tools"
    );
}

/// The section must stay byte-stable for the life of a task, or the prompt cache
/// misses on every turn. Same config in, same bytes out — twice, to catch an
/// accidental dependency on time or iteration order.
#[test]
fn the_screen_section_is_byte_stable_for_a_fixed_config() {
    let c = cfg(true, true, true);
    assert_eq!(prompt::screen_section(&c), prompt::screen_section(&c));
    let off = cfg(false, false, false);
    assert_eq!(prompt::screen_section(&off), prompt::screen_section(&off));
}

/// A sub-agent builds its tool list from the parent's *frozen* plugin snapshot,
/// not from live settings. `prefix_plugins` is the seam: the hot prefix wins, and
/// a cold task falls back to settings. Both paths have to name the live tools,
/// because the sub-agent shares the parent's prompt world.
#[test]
fn a_sub_agent_tool_list_follows_the_parents_frozen_prefix() {
    let with_computer = cfg(false, true, false);
    let without = cfg(false, false, false);

    // The frozen path: whatever the snapshot says is what a sub-agent gets,
    // even where live settings would say otherwise. Asserted through the schema
    // builder because `prefix_plugins` needs a Harness, and the builder is the
    // only thing that consumes its result.
    assert!(offered(&with_computer, Some("all"))
        .iter()
        .any(|n| n == "screenshot"));
    assert!(!offered(&without, Some("all"))
        .iter()
        .any(|n| n == "screenshot"));

    // A `Default` snapshot is what a cold task carries in `Task.plugins`, which is
    // exactly why `prefix_plugins` falls back to live settings instead of
    // trusting it: read literally it means "every plugin off", and a first
    // sub-agent would inherit that and conclude computer use was switched off.
    let cold = PluginsCfg::default();
    assert!(
        !cold.computer.enabled && !cold.browser.enabled,
        "the cold-task default is all-off, which is why it must not be used as-is"
    );
    assert!(!super::tools::schemas(Some("all"), &cold)
        .iter()
        .any(|t| t["name"] == "screenshot"));
}
