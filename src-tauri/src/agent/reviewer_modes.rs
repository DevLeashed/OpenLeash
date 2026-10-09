//! Release-gate tests for the two axes a skill is invoked on
//! (`user-invocable` / `disable-model-invocation`) and the per-agent tool
//! restrictions (`groups` + `fileRegex`) and iteration ceiling (`steps`).
//!
//! The property that matters is negative on purpose: a skill marked
//! `disable-model-invocation: true` must be *absent* from what the model is
//! told, not merely deprioritised. `parse_skill_md` returning the flag is not
//! enough — the system prompt is the thing the model reads, so a skill that
//! reaches it has been auto-invocable all along. That is why one test here
//! assembles the prompt and asserts the skill's name and description do not
//! appear in it, and why the frontmatter-absent case asserts the *opposite*
//! (today's behaviour: visible to the model).
//!
//! These tests deliberately do not assert anything about `permissions.rs`,
//! `runner.rs` or `prompt.rs` internals: those files are owned elsewhere and the
//! enforcement is wired at integration. What is fixed here is the data model and
//! its validation, which is what the enforcement and the UI both read.

use super::store::{
    parse_agent_md, parse_skill_md, resolve_invocation, skill_model_visible, validate_agent_groups,
    AgentDef, SkillDef, SkillMeta, ToolGroup, TOOL_GROUPS,
};

fn skill(name: &str, extra: &str) -> String {
    format!("---\nname: {name}\ndescription: does {name} things\n{extra}---\n# body\n")
}

/// Build the `SkillDef` the loading path would, so prompt-level assertions run
/// against the same shape the runner hands `prompt::system`.
fn to_def(m: SkillMeta) -> SkillDef {
    let (disable, palette) = resolve_invocation(&m);
    SkillDef {
        name: m.name,
        description: m.description,
        source: "user".into(),
        path: "/skills/x/SKILL.md".into(),
        enabled: true,
        files: 0,
        disable_model_invocation: disable,
        user_invocable: palette,
    }
}

// ─────────────────── 1. user-invocable / disable-model-invocation ───────────────

/// The whole point: a `disable-model-invocation` skill leaves the model's list.
/// Asserted against the assembled prompt, not just the flag, because the flag is
/// only useful if the prompt filter obeys it.
#[test]
fn disable_model_invocation_skill_is_not_in_the_models_list_but_stays_user_invocable() {
    let m = parse_skill_md(
        &skill("deploy-to-production", "disable-model-invocation: true\n"),
        "x",
    )
    .unwrap();
    assert!(m.disable_model_invocation);
    let d = to_def(m);
    assert!(
        !skill_model_visible(&d),
        "the model-facing predicate must be false"
    );
    assert!(
        d.user_invocable,
        "a model-disabled skill is still the user's to call — otherwise nobody can"
    );

    let listed: Vec<SkillDef> = vec![d.clone()]
        .into_iter()
        .filter(skill_model_visible)
        .collect();
    let sec = super::prompt::skills_section(&listed);
    assert!(
        !sec.contains("deploy-to-production"),
        "the skill must not reach the system prompt at all.\n---\n{sec}\n---"
    );
    assert!(
        sec.is_empty(),
        "a model-disabled skill costs zero context, so the section is empty"
    );
}

/// With the frontmatter absent, behaviour must be exactly what it was before the
/// field existed: visible to the model, no palette entry. A default that flipped
/// either of these would silently change every installed skill.
#[test]
fn no_frontmatter_keeps_todays_behaviour() {
    let m = parse_skill_md(&skill("plain-skill", ""), "x").unwrap();
    assert!(!m.disable_model_invocation);
    assert!(!m.user_invocable);
    let d = to_def(m);
    assert!(skill_model_visible(&d), "unflagged skills stay visible");
    assert!(
        !d.user_invocable,
        "and do not appear in a palette by default"
    );
    let sec = super::prompt::skills_section(std::slice::from_ref(&d));
    assert!(
        sec.contains("plain-skill"),
        "the ordinary skill must still be listed.\n---\n{sec}\n---"
    );
}

/// `user-invocable: true` alone: still model-visible (it did not ask to be
/// hidden) but now in the palette.
#[test]
fn user_invocable_alone_adds_a_palette_entry_without_hiding_from_the_model() {
    let m = parse_skill_md(&skill("release", "user-invocable: true\n"), "x").unwrap();
    assert!(m.user_invocable && !m.disable_model_invocation);
    let d = to_def(m);
    assert!(d.user_invocable);
    assert!(skill_model_visible(&d));
}

/// Both flags together, and the spellings people actually type.
#[test]
fn flag_spellings_and_an_explicit_false_are_honoured() {
    for key in ["disable-model-invocation", "disable_model_invocation"] {
        let m = parse_skill_md(&skill("a", &format!("{key}: true\n")), "x").unwrap();
        assert!(m.disable_model_invocation, "{key} must be read");
    }
    // An explicit false is false, whatever the spelling.
    for v in ["false", "no", "off"] {
        let m = parse_skill_md(
            &skill("a", &format!("disable-model-invocation: {v}\n")),
            "x",
        )
        .unwrap();
        assert!(!m.disable_model_invocation, "{v} must mean no");
        assert!(!m.user_invocable);
    }
    let m = parse_skill_md(&skill("a", "user-invocable: false\n"), "x").unwrap();
    assert!(!m.user_invocable);
}

/// The unpublished-by-default rule: a skill that asked for neither flag keeps
/// both axes off, and only `disable-model-invocation` implies the palette.
#[test]
fn an_unflagged_skill_is_neither_palette_nor_hidden() {
    let (disable, palette) = resolve_invocation(&SkillMeta {
        name: "s".into(),
        description: "d".into(),
        ..Default::default()
    });
    assert!(!disable && !palette);
}

// ───────────────────────── 2. groups + fileRegex ───────────────────────────────

fn agent(groups: &str) -> AgentDef {
    parse_agent_md(
        &format!("---\nname: docs\ndescription: writes docs\n{groups}---\nbody\n"),
        "docs",
    )
    .unwrap()
}

/// The Roo shape, both forms in one block: a plain group is a name, a restricted
/// one is the tuple.
#[test]
fn groups_parse_both_the_plain_and_the_tuple_form() {
    let d = agent("groups:\n  - read\n  - edit\n  - [edit, {fileRegex: \"\\\\.(md|mdx)$\", description: docs only}]\n");
    assert!(matches!(d.groups[0], ToolGroup::Plain(ref n) if n == "read"));
    // The middle `edit` is unrestricted, the last is not — a parser that
    // collapsed them would make the restriction look present but not apply.
    assert_eq!(d.groups.iter().filter(|g| g.name() == "edit").count(), 2);
    let r = d
        .groups
        .iter()
        .find(|g| g.file_regex().is_some())
        .expect("the restricted tuple survived");
    assert_eq!(r.name(), "edit");
    assert_eq!(r.file_regex(), Some("\\.(md|mdx)$"));
    assert_eq!(r.description(), "docs only");
    // The unrestricted `edit` reports no pattern.
    assert!(d.groups[1].file_regex().is_none());
}

/// The block-sequence spelling of the same tuple must not be silently dropped.
#[test]
fn groups_parse_the_block_sequence_spelling() {
    let d = agent(
        "groups:\n  - read\n  - edit\n    fileRegex: \"\\\\.md$\"\n    description: markdown\n",
    );
    let r = d.groups.iter().find(|g| g.file_regex().is_some()).unwrap();
    assert_eq!((r.name(), r.file_regex()), ("edit", Some("\\.md$")));
    assert_eq!(r.description(), "markdown");
    // The plain `read` above is untouched.
    assert!(d
        .groups
        .iter()
        .any(|g| g.name() == "read" && g.file_regex().is_none()));
}

/// The flow form on one line, which a hand-edited file may well use.
#[test]
fn groups_parse_the_inline_flow_form() {
    let d = agent("groups: [read, [edit, {fileRegex: \"^src/.*\", description: source}]]\n");
    assert!(d.groups.iter().any(|g| g.name() == "read"));
    let r = d.groups.iter().find(|g| g.file_regex().is_some()).unwrap();
    assert_eq!((r.name(), r.file_regex()), ("edit", Some("^src/.*")));
}

/// An invalid `fileRegex` is an error, not a silently-ignored restriction. Both
/// the validator and the compiled accessor must refuse it.
#[test]
fn an_invalid_file_regex_is_rejected_with_a_clear_error() {
    // An unmatched `(` is invalid to `regex`; note `\(` would be *valid* (a
    // literal paren), which is exactly the trap a test on the wrong string
    // falls into.
    let d =
        agent("groups:\n  - read\n  - [edit, {fileRegex: \"(unclosed\", description: broken}]\n");
    let err = validate_agent_groups(&d).expect_err("a bad pattern must be refused");
    assert!(
        err.contains("docs") && err.contains("fileRegex") && err.contains("regular expression"),
        "the error must name the agent, the field and the problem: {err}"
    );
    // The accessor the gate will use refuses to hand back a broken pattern.
    assert!(d.edit_restriction().is_err());
    // And a good one compiles.
    let ok = agent("groups:\n  - [edit, {fileRegex: \"\\\\.md$\"}]\n");
    validate_agent_groups(&ok).expect("a valid pattern passes");
    assert!(ok.edit_restriction().unwrap().is_some());
}

/// Unknown group names and `fileRegex` on a group that cannot honour it are
/// refused, so a config never *looks* restricted without being it.
#[test]
fn unknown_groups_and_misplaced_file_regex_are_refused() {
    let bad = agent("groups:\n  - write\n");
    assert!(validate_agent_groups(&bad).is_err());
    let misplaced = agent("groups:\n  - [command, {fileRegex: \"\\\\.sh$\"}]\n");
    let e = validate_agent_groups(&misplaced).unwrap_err();
    assert!(
        e.contains("edit"),
        "the error points at the group that works: {e}"
    );
    // Every name Roo documents is accepted.
    for name in TOOL_GROUPS {
        let d = agent(&format!("groups:\n  - {name}\n"));
        validate_agent_groups(&d).unwrap_or_else(|e| panic!("{name} should be valid: {e}"));
    }
}

/// A JSON-written agent (the settings path) round-trips the tuple form, which is
/// how the UI will hand it back. This is the seam the frontend depends on.
#[test]
fn groups_round_trip_through_json_as_roos_tuple_shape() {
    let d = agent("groups:\n  - read\n  - [edit, {fileRegex: \"\\\\.md$\", description: docs}]\n");
    let j = serde_json::to_string(&d.groups).unwrap();
    assert!(
        j.contains(r#"["edit","#) || j.contains(r#"["edit",{"#),
        "the restricted group must serialize as a two-element tuple: {j}"
    );
    let back: Vec<ToolGroup> = serde_json::from_str(&j).unwrap();
    assert_eq!(back[1].file_regex(), Some("\\.md$"));
    // And Roo's exact JSON, hand-written, parses.
    let hand: Vec<ToolGroup> = serde_json::from_str(
        r#"["read", ["edit", {"fileRegex": "\\.(md|mdx)$", "description": "docs"}]]"#,
    )
    .unwrap();
    assert_eq!(hand[0].name(), "read");
    assert_eq!(hand[1].file_regex(), Some("\\.(md|mdx)$"));
}

/// An agent saved before the field existed must load with no groups and no
/// ceiling — i.e. unrestricted, exactly as it behaved.
#[test]
fn an_agent_from_before_the_fields_loads_unrestricted() {
    let old: AgentDef = serde_json::from_str(r#"{"id":"a","tools":"all"}"#).unwrap();
    assert!(old.groups.is_empty());
    assert_eq!(old.steps, 0, "no ceiling unless one was asked for");
    assert!(old.edit_restriction().unwrap().is_none());
    validate_agent_groups(&old).unwrap();
}

/// A regex that stops an agent from editing anything it was meant to touch is
/// the failure a bad pattern buys; assert the documented patterns match what
/// they claim, so the example in the docs is not itself wrong.
#[test]
fn documented_file_regex_patterns_match_what_they_say() {
    let docs = agent("groups:\n  - [edit, {fileRegex: \"\\\\.(md|mdx)$\"}]\n");
    let re = docs.edit_restriction().unwrap().unwrap();
    assert!(re.is_match("README.md"));
    assert!(re.is_match("docs/page.mdx"));
    assert!(!re.is_match("src/lib.rs"));
    assert!(!re.is_match("md")); // an unanchored pattern would wrongly match
}

// ───────────────────────── 3. steps ceiling ────────────────────────────────────

/// The default is *no* ceiling, so every agent a user already saved keeps
/// running as long as before; only the built-ins carry a finite one.
#[test]
fn steps_defaults_to_no_ceiling() {
    let old: AgentDef = serde_json::from_str(r#"{"id":"a"}"#).unwrap();
    assert_eq!(old.steps, 0);

    let builtins = super::store::builtin_agents();
    for b in &builtins {
        assert!(
            b.steps > 0,
            "built-in `{}` should ship a ceiling so a runaway explorer is bounded",
            b.id
        );
    }
    let explore = builtins.iter().find(|a| a.id == "explore").unwrap();
    let general = builtins.iter().find(|a| a.id == "general").unwrap();
    assert!(
        explore.steps < general.steps,
        "a read-only explorer is capped tighter than a general worker"
    );
    assert!(super::store::fuze_agent().steps > 0);
}

/// `steps` is read from an agent file (opencode's key, plus the spellings people
/// reach for), and a missing one stays at zero.
#[test]
fn steps_parses_from_an_agent_file() {
    for key in ["steps", "max_steps", "maxTurns"] {
        let d = agent(&format!("{key}: 7\n"));
        assert_eq!(d.steps, 7, "{key} must set the ceiling");
    }
    let d = agent("");
    assert_eq!(d.steps, 0, "absent means no ceiling");
}

/// A ceiling round-trips through settings JSON, so the UI's edit is the value
/// the runner will read.
#[test]
fn steps_round_trips_through_settings_json() {
    let mut d = agent("");
    d.steps = 12;
    let j = serde_json::to_string(&d).unwrap();
    let back: AgentDef = serde_json::from_str(&j).unwrap();
    assert_eq!(back.steps, 12);
}
