//! The per-project instruction selection: the user can switch off individual
//! project doc files (e.g. a `CLAUDE.md` a particular repo carries) without
//! refusing the whole folder.
//!
//! Two properties are load-bearing and each has a test here:
//!
//! 1. A disabled file reaches *no* agent's prompt — not even as a name. The
//!    point of switching `CLAUDE.md` off is that the model never sees it, so a
//!    partial fix that merely dropped its body would still be a leak.
//! 2. The selection is per project, exact-folder, and fails open. A rule for one
//!    project must not change another's prompt (including a sibling under a
//!    shared parent), and a project with no rule gets everything.
//!
//! The selection is *not* a trust mechanism, so a third test pins that a folder
//! that is still untrusted injects nothing whether or not a rule exists.

use super::prompt::{self, Env};
use super::store::{self, ProjectDocRule, Settings};
use super::trust::{self, TrustDecision, TrustKind, TrustState};

fn project(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ol-docs-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn env_for<'a>(project: &'a str, disabled: &'a [String]) -> Env<'a> {
    Env {
        cwd: ".",
        project: Box::leak(project.to_string().into_boxed_str()),
        branch: "",
        is_git: false,
        worktree: false,
        date: "2026-01-01".into(),
        agent_id: "ol-test",
        inject_global_claude: false,
        disabled_docs: disabled,
    }
}

/// Trust a folder and hold the process-global lock for the test's duration.
fn trust(project: &str) -> std::sync::MutexGuard<'static, ()> {
    let g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    trust::set_trust(&[TrustDecision {
        path: trust::norm_path(project),
        kind: TrustKind::Folder,
        decision: TrustState::Trusted,
        decided_at: chrono::Utc::now(),
    }]);
    g
}

fn rule(path: &str, disabled: &[&str]) -> ProjectDocRule {
    ProjectDocRule {
        path: trust::norm_path(path),
        disabled: disabled.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn a_disabled_file_is_absent_from_the_prompt_by_name_and_by_content() {
    let d = project("disabled-basic");
    std::fs::write(d.join("AGENTS.md"), "KEEP-ME-RULE").unwrap();
    std::fs::write(d.join("CLAUDE.md"), "DROP-ME-RULE").unwrap();
    let p = d.to_string_lossy().into_owned();
    let _g = trust(&p);

    // Before the rule, both are in.
    let before = prompt::system(&env_for(&p, &[]), None, &[]);
    assert!(before.contains("DROP-ME-RULE"), "precondition: {before}");

    let disabled = vec!["CLAUDE.md".to_string()];
    let after = prompt::system(&env_for(&p, &disabled), None, &[]);

    assert!(
        !after.contains("DROP-ME-RULE"),
        "a disabled file's body must not reach the prompt:\n{after}"
    );
    assert!(
        !after.contains("CLAUDE.md"),
        "a disabled file must not even be named:\n{after}"
    );
    assert!(
        after.contains("KEEP-ME-RULE"),
        "the files not switched off must still be injected:\n{after}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_rule_for_one_project_does_not_affect_another() {
    // Two unrelated checkouts. Switching CLAUDE.md off in one must leave the
    // other's prompt untouched — the whole feature is "per project".
    let mine = project("scope-mine");
    let other = project("scope-other");
    std::fs::write(mine.join("CLAUDE.md"), "MINE-CLAUDE").unwrap();
    std::fs::write(other.join("CLAUDE.md"), "OTHER-CLAUDE").unwrap();
    let mp = mine.to_string_lossy().into_owned();
    let op = other.to_string_lossy().into_owned();
    let _g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    trust::set_trust(&[
        TrustDecision {
            path: trust::norm_path(&mp),
            kind: TrustKind::Folder,
            decision: TrustState::Trusted,
            decided_at: chrono::Utc::now(),
        },
        TrustDecision {
            path: trust::norm_path(&op),
            kind: TrustKind::Folder,
            decision: TrustState::Trusted,
            decided_at: chrono::Utc::now(),
        },
    ]);

    let rules = vec![rule(&mp, &["CLAUDE.md"])];
    let disabled = prompt::disabled_docs(&rules, &mp);
    assert_eq!(disabled, vec!["CLAUDE.md".to_string()]);
    assert!(
        prompt::disabled_docs(&rules, &op).is_empty(),
        "another project must not inherit the rule"
    );

    let mine_prompt = prompt::system(&env_for(&mp, &disabled), None, &[]);
    assert!(!mine_prompt.contains("MINE-CLAUDE"));
    let other_prompt = prompt::system(&env_for(&op, &[]), None, &[]);
    assert!(
        other_prompt.contains("OTHER-CLAUDE"),
        "the other project keeps its file:\n{other_prompt}"
    );
    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn a_rule_on_a_parent_folder_does_not_govern_a_nested_project() {
    // Trust inherits down a tree; this selection deliberately does not. A repo
    // opened inside a folder with a rule is its own project and gets everything.
    let parent = project("parent-scope");
    let child = parent.join("packages/app");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(child.join("CLAUDE.md"), "CHILD-CLAUDE").unwrap();
    let pp = parent.to_string_lossy().into_owned();
    let cp = child.to_string_lossy().into_owned();

    let rules = vec![rule(&pp, &["CLAUDE.md"])];
    assert!(
        prompt::disabled_docs(&rules, &cp).is_empty(),
        "a rule on the parent must not reach the child project"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn a_project_with_no_rule_fails_open() {
    let p = project("no-rule");
    assert!(
        prompt::disabled_docs(&[], &p.to_string_lossy()).is_empty(),
        "no rule means every file is loaded"
    );
    let _ = std::fs::remove_dir_all(&p);
}

#[test]
fn rules_compare_paths_the_way_trust_does() {
    // A path stored with backslashes / a trailing separator / different case on
    // Windows still matches the project the task actually runs in, and the
    // normalized form is what is persisted so a later read compares equal.
    let d = project("norm");
    let p = d.to_string_lossy().into_owned();
    let windowsish = p.replace('/', "\\") + "\\";
    let r = vec![rule(&windowsish, &["CLAUDE.md"])];
    assert_eq!(
        prompt::disabled_docs(&r, &p),
        vec!["CLAUDE.md".to_string()],
        "a differently-spelled path must still match"
    );
    assert_eq!(r[0].path, trust::norm_path(&p), "stored normalized");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_untrusted_folder_injects_nothing_even_with_a_rule() {
    // The selection is a convenience, not a trust decision: recording one must
    // not be mistaken for, or weaken, folder trust.
    let d = project("untrusted");
    std::fs::write(d.join("AGENTS.md"), "PLANTED-RULE").unwrap();
    std::fs::write(d.join("CLAUDE.md"), "MORE-PLANTED").unwrap();
    let p = d.to_string_lossy().into_owned();
    let _g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    trust::set_trust(&[]);

    let disabled = vec!["CLAUDE.md".to_string()];
    let sys = prompt::system(&env_for(&p, &disabled), None, &[]);
    assert!(
        !sys.contains("PLANTED-RULE") && !sys.contains("MORE-PLANTED"),
        "nothing project-local is injected while untrusted:\n{sys}"
    );
    assert!(
        sys.contains("NOT trusted"),
        "and the model is told why:\n{sys}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_selection_only_touches_project_files_not_the_users_globals() {
    // `inject_global_claude` and the user's `~/.openleash/OPENLEASH.md` are the
    // user's own files, so a project rule naming them must have no effect.
    let d = project("globals-untouched");
    let p = d.to_string_lossy().into_owned();
    let _g = trust(&p);
    let disabled = vec!["~/.claude/CLAUDE.md (user-global)".to_string()];
    // The global names are not in `MEMORY_FILES`, so the loader ignores them.
    assert!(prompt::project_files(&p, &disabled).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_manifest_shows_a_disabled_file_marked_as_disabled() {
    // The doc selection lives next to the trust dialog's discovery view, so a
    // file the user switched off must still be listed — flagged, so the UI can
    // offer the toggle back — and must not count as "injecting".
    let root = std::env::temp_dir().join(format!("ol-docs-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _home = store::test_home(&root.join("home"));
    let d = root.join("proj");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("AGENTS.md"), "keep").unwrap();
    std::fs::write(d.join("CLAUDE.md"), "drop").unwrap();
    let p = d.to_string_lossy().into_owned();

    let mut s = Settings::default();
    trust::upsert(&mut s.trust, &p, TrustKind::Folder, TrustState::Trusted);
    s.project_docs = vec![rule(&p, &["CLAUDE.md"])];

    let m = trust::manifest(&s, &p);
    let claude = m
        .instructions
        .iter()
        .find(|f| f.name == "CLAUDE.md")
        .expect("a switched-off file is still listed so it can be switched back on");
    assert!(claude.disabled, "it must be flagged as disabled");
    let agents = m
        .instructions
        .iter()
        .find(|f| f.name == "AGENTS.md")
        .expect("AGENTS.md is listed");
    assert!(!agents.disabled);

    // Trusted *and* still carrying a loaded project item, because AGENTS.md is
    // not switched off. Disabling the only project file would make it not inject.
    assert!(m.injecting, "AGENTS.md still injects");
    let mut only_disabled = s.clone();
    only_disabled.project_docs = vec![rule(&p, &["AGENTS.md", "CLAUDE.md"])];
    let m2 = trust::manifest(&only_disabled, &p);
    assert!(
        !m2.has_project_items(),
        "nothing project-local is left to inject"
    );
    assert!(!m2.injecting);
    let _ = std::fs::remove_dir_all(&root);
}

/// `.openleash/DESIGN.md` is the project's design system: an instruction file
/// in a subdirectory, so this drives the nested `Path::join` on every platform
/// and pins that it is gated by trust and switchable like the flat files.
#[test]
fn the_design_system_file_is_injected_only_for_a_trusted_project() {
    let d = project("design-system");
    std::fs::create_dir_all(d.join(".openleash")).unwrap();
    std::fs::write(
        d.join(".openleash").join("DESIGN.md"),
        "DESIGN-TOKENS: --accent #ff5500",
    )
    .unwrap();
    let p = d.to_string_lossy().into_owned();
    assert_eq!(
        prompt::MEMORY_FILES.last(),
        Some(&".openleash/DESIGN.md"),
        "the design system is read last, after the project's own rules"
    );
    let files = prompt::project_files(&p, &[]);
    assert_eq!(
        files,
        vec![(
            ".openleash/DESIGN.md".to_string(),
            "DESIGN-TOKENS: --accent #ff5500".to_string()
        )]
    );

    {
        let _g = trust(&p);
        let sys = prompt::system(&env_for(&p, &[]), None, &[]);
        assert!(
            sys.contains(
                "<file name=\".openleash/DESIGN.md\">\nDESIGN-TOKENS: --accent #ff5500\n</file>"
            ),
            "a trusted project's design system is injected:\n{sys}"
        );
        let disabled = vec![".openleash/DESIGN.md".to_string()];
        let off = prompt::system(&env_for(&p, &disabled), None, &[]);
        assert!(
            !off.contains("DESIGN-TOKENS"),
            "it can be switched off per project"
        );
    }
    {
        let _g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        trust::set_trust(&[]);
        let sys = prompt::system(&env_for(&p, &[]), None, &[]);
        assert!(
            !sys.contains("DESIGN-TOKENS"),
            "an untrusted folder's design system is not injected:\n{sys}"
        );
        assert!(sys.contains("NOT trusted"));
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_manifest_lists_the_design_system_file() {
    let root = std::env::temp_dir().join(format!("ol-docs-design-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _home = store::test_home(&root.join("home"));
    let d = root.join("proj");
    std::fs::create_dir_all(d.join(".openleash")).unwrap();
    std::fs::write(d.join(".openleash").join("DESIGN.md"), "tokens").unwrap();
    let p = d.to_string_lossy().into_owned();
    let mut s = Settings::default();
    trust::upsert(&mut s.trust, &p, TrustKind::Folder, TrustState::Trusted);
    let m = trust::manifest(&s, &p);
    let design = m
        .instructions
        .iter()
        .find(|f| f.name == ".openleash/DESIGN.md")
        .expect("the design system is listed in the trust manifest");
    assert!(!design.disabled);
    assert_eq!(design.bytes, 6);
    assert!(m.injecting);
    let _ = std::fs::remove_dir_all(&root);
}
