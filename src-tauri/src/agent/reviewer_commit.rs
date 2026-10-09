//! Release-gate tests for agent-made commits: message shaping, attribution, and
//! the `--no-verify` default.
//!
//! Two halves. The shaping half is pure text in / text out — every case in the
//! brief lives there (multi-line, a fenced block, a trailing explanation, an
//! over-long subject, an empty response, a `#` comment line git would strip).
//!
//! The git half uses a scratch repository in a temp directory (the same
//! `Sandbox` the other reviewer modules use), never this repository, and proves
//! the security property behaviourally: a `pre-commit` hook that fails must stop
//! a commit that runs hooks and must be *untouched* by the `--no-verify` path.

#![cfg(test)]

use super::commitmsg::{self, Attribution, Draft};
use super::git;
use super::reviewer_sandbox::Sandbox;
use super::store;

// ───────────────────────────── shaping ─────────────────────────────

fn files() -> Vec<git::FileDiff> {
    vec![
        git::FileDiff {
            path: "src/a.rs".into(),
            status: "M".into(),
            add: 3,
            del: 1,
            lines: vec![],
        },
        git::FileDiff {
            path: "src/b.rs".into(),
            status: "A".into(),
            add: 10,
            del: 0,
            lines: vec![],
        },
    ]
}

#[test]
fn a_plain_multi_line_answer_survives_intact() {
    let raw = "feat(review): add a commit affordance\n\nThe review screen gains a Generate button and an attribution picker.\nBody lines are preserved in order.";
    assert_eq!(
        commitmsg::shape(raw),
        "feat(review): add a commit affordance\n\nThe review screen gains a Generate button and an attribution picker.\nBody lines are preserved in order."
    );
}

#[test]
fn a_wrapping_code_fence_is_stripped() {
    // Models fence the message constantly, sometimes with a language tag.
    assert_eq!(
        commitmsg::shape("```\nfix: drop the trailing newline\n```"),
        "fix: drop the trailing newline"
    );
    assert_eq!(
        commitmsg::shape("```text\nfix: drop the trailing newline\n\nwhy it mattered\n```"),
        "fix: drop the trailing newline\n\nwhy it mattered"
    );
    // Text around the fence is the model talking to us, not the message.
    assert_eq!(
        commitmsg::shape(
            "Sure! Here's your commit message:\n\n```\nchore: bump deps\n```\n\nI hope this helps!"
        ),
        "chore: bump deps"
    );
}

#[test]
fn an_empty_fence_falls_back_instead_of_committing_a_fence() {
    assert_eq!(commitmsg::shape("```\n```"), "");
    assert_eq!(commitmsg::shape("```"), "");
}

#[test]
fn a_here_is_your_preamble_is_dropped() {
    assert_eq!(
        commitmsg::shape("Here is your commit message:\n\nfeat: teach the agent to commit"),
        "feat: teach the agent to commit"
    );
    assert_eq!(
        commitmsg::shape("Sure, here's a commit message:\n\nfix: handle a nil diff"),
        "fix: handle a nil diff"
    );
    // A subject that merely mentions the phrase is not preamble.
    assert_eq!(
        commitmsg::shape("feat: rework the commit message formatter"),
        "feat: rework the commit message formatter"
    );
}

#[test]
fn a_trailing_explanation_is_cut_off() {
    assert_eq!(
        commitmsg::shape("feat: add trailers\n\nAdds Co-authored-by support.\n\nI hope this helps! Let me know if you want a different wording."),
        "feat: add trailers\n\nAdds Co-authored-by support."
    );
    assert_eq!(
        commitmsg::shape("fix: off-by-one\n\nPlease let me know if this needs a test."),
        "fix: off-by-one"
    );
    // The body keeps a line that coincidentally starts with "hope" if it is not
    // the last paragraph — only chatter at the end goes.
    assert_eq!(
        commitmsg::shape("feat: x\n\nhope this helps future us\nMore explanation."),
        "feat: x\n\nhope this helps future us\nMore explanation."
    );
}

#[test]
fn an_over_long_subject_is_cut_at_a_word() {
    let long = format!("feat: {}", "really long description ".repeat(6).trim());
    let out = commitmsg::shape(&long);
    let subject = out.lines().next().unwrap();
    assert!(
        subject.chars().count() <= commitmsg::MAX_SUBJECT,
        "{subject:?} is {} chars",
        subject.chars().count()
    );
    assert!(
        subject.starts_with("feat: really long description"),
        "{subject:?}"
    );
    assert!(
        !subject.ends_with(' '),
        "no trailing space after the cut: {subject:?}"
    );
    assert_eq!(subject, subject.trim_end());
}

#[test]
fn a_single_unbreakable_subject_is_still_bounded() {
    let long = format!("feat: {}", "x".repeat(200));
    let out = commitmsg::shape(&long);
    assert!(
        out.chars().count() <= commitmsg::MAX_SUBJECT,
        "{} chars",
        out.chars().count()
    );
}

#[test]
fn an_empty_response_shapes_to_nothing() {
    assert_eq!(commitmsg::shape(""), "");
    assert_eq!(commitmsg::shape("   \n\n  "), "");
    assert_eq!(commitmsg::shape("# only a comment\n"), "");
}

#[test]
fn a_hash_comment_line_git_would_strip_is_dropped() {
    // git removes `#` lines when it cleans up a commit message, so a model that
    // writes them is annotating for us. Leaving one in the subject would commit
    // an empty message; leaving one mid-body silently loses it anyway.
    assert_eq!(
        commitmsg::shape("feat: add hooks\n\n# TODO: mention the setting\nExplains the default."),
        "feat: add hooks\n\nExplains the default."
    );
    assert_eq!(
        commitmsg::shape("# a leading note\nfeat: add hooks"),
        "feat: add hooks"
    );
}

#[test]
fn the_shaped_message_is_conventional_or_at_least_text() {
    assert!(commitmsg::is_conventional("feat: x"));
    assert!(commitmsg::is_conventional("fix(scope): x"));
    assert!(commitmsg::is_conventional("chore!: x"));
    assert!(!commitmsg::is_conventional("Feature: x"));
    assert!(!commitmsg::is_conventional("no colon here"));
    assert!(!commitmsg::is_conventional(""));
}

#[test]
fn an_empty_or_junk_reply_falls_back_to_a_deterministic_message() {
    let f = files();
    let d: Draft = commitmsg::draft_from_raw("", &f);
    assert_eq!(d.source, "fallback");
    assert_eq!(d.message, "chore: update 2 files");
    assert_eq!(commitmsg::draft_from_raw("```\n```", &f).source, "fallback");

    let one = commitmsg::draft_from_raw(
        "",
        &[git::FileDiff {
            path: "src/a.rs".into(),
            status: "M".into(),
            add: 3,
            del: 1,
            lines: vec![],
        }],
    );
    assert_eq!(one.message, "chore: update src/a.rs");
    assert_eq!(
        commitmsg::fallback_message(&[]),
        "chore: no tracked changes"
    );

    let good = commitmsg::draft_from_raw("feat: real message", &f);
    assert_eq!(good.source, "model");
    assert_eq!(good.message, "feat: real message");
}

#[test]
fn an_enormous_reply_is_bounded() {
    let raw = "feat: bounded\n\n".to_string() + &"word ".repeat(20_000);
    let out = commitmsg::shape(&raw);
    assert!(
        out.chars().count() < 20_000,
        "a runaway reply must not become a {} char commit message",
        out.chars().count()
    );
}

// ───────────────────────────── attribution ─────────────────────────────

#[test]
fn every_attribution_style_has_the_trailer_it_promises() {
    assert_eq!(Attribution::None.trailer(), "");
    assert_eq!(
        Attribution::CoAuthoredBy.trailer(),
        "Co-authored-by: OpenLeash <agent@openleash.dev>"
    );
    assert_eq!(
        Attribution::AssistedBy.trailer(),
        "Assisted-by: OpenLeash <agent@openleash.dev>"
    );
    assert_eq!(
        Attribution::GeneratedWith.trailer(),
        "Generated with OpenLeash"
    );
}

#[test]
fn the_trailer_is_appended_once_and_never_twice() {
    let msg = "feat: commit quality";
    let once = commitmsg::compose(msg, Attribution::CoAuthoredBy);
    assert_eq!(
        once,
        "feat: commit quality\n\nCo-authored-by: OpenLeash <agent@openleash.dev>"
    );
    // The Generate button fills the composer with the final message and the
    // commit path runs this again, so a second pass must be a no-op — otherwise
    // every agent commit gains a second identical trailer.
    assert_eq!(commitmsg::compose(&once, Attribution::CoAuthoredBy), once);
    // The same message under a *different* style gets that style, unremoved.
    let assisted = commitmsg::compose(msg, Attribution::AssistedBy);
    assert!(assisted.ends_with("Assisted-by: OpenLeash <agent@openleash.dev>"));
}

#[test]
fn a_user_written_trailer_is_respected() {
    let msg = "feat: x\n\nCo-authored-by: A Human <human@example.com>";
    assert_eq!(commitmsg::compose(msg, Attribution::CoAuthoredBy), msg);
    // `none` adds nothing at all.
    assert_eq!(commitmsg::compose("feat: x", Attribution::None), "feat: x");
    assert_eq!(commitmsg::compose("", Attribution::None), "");
}

#[test]
fn attribution_parses_every_stored_spelling() {
    assert_eq!(Attribution::parse("none"), Attribution::None);
    assert_eq!(Attribution::parse("assisted-by"), Attribution::AssistedBy);
    assert_eq!(
        Attribution::parse("generated-with"),
        Attribution::GeneratedWith
    );
    assert_eq!(
        Attribution::parse("co-authored-by"),
        Attribution::CoAuthoredBy
    );
    // Round-trip, so the UI and the file agree.
    for a in [
        Attribution::None,
        Attribution::CoAuthoredBy,
        Attribution::AssistedBy,
        Attribution::GeneratedWith,
    ] {
        assert_eq!(Attribution::parse(a.as_str()), a);
    }
    // The whole point of the default: a settings.json written before this field
    // existed has `""` here, and an unset field must not silently turn
    // attribution *off* — a reader of `git log` should be able to tell an agent
    // wrote it. Only an explicit "none" does that.
    assert_eq!(Attribution::parse(""), Attribution::CoAuthoredBy);
    assert_eq!(
        Attribution::parse("something-else"),
        Attribution::CoAuthoredBy
    );
}

#[test]
fn the_default_settings_are_honest_but_not_noisy() {
    let s = store::Settings::default();
    // Attribution on by default, in the trailer git already renders as credit.
    assert_eq!(
        Attribution::parse(&s.git_attribution),
        Attribution::CoAuthoredBy
    );
    // And hooks skipped by default — see `Settings::git_commit_verify`.
    assert!(!s.git_commit_verify, "--no-verify must be the default");
    // The prompt is overridable, so the built-in is only a fallback.
    assert!(commitmsg::PROMPT.contains("Conventional Commits"));
}

// ───────────────────────────── git plumbing ─────────────────────────────

/// A scratch repository with a failing `pre-commit` hook.
fn repo_with_failing_hook(tag: &str) -> Sandbox {
    let s = Sandbox::new(tag);
    let cwd = s.root().to_string_lossy().to_string();
    s.write("a.txt", "hello\n");
    run(&cwd, &["init", "-q", "."]);
    // Configure locally so a machine's global config (gpg signing, a global
    // core.hooksPath, a commit template) cannot change what this test proves.
    run(&cwd, &["config", "user.name", "OpenLeash Test"]);
    run(&cwd, &["config", "user.email", "test@openleash.dev"]);
    run(&cwd, &["config", "commit.gpgsign", "false"]);
    let hooks = s.root().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    run(
        &cwd,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
    // Relative, because git runs a hook from the top of the working tree.
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho ran > hook-marker\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&hook).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&hook, p).unwrap();
    }
    run(&cwd, &["add", "-A"]);
    s
}

fn run(cwd: &str, args: &[&str]) -> std::process::Output {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} could not run: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn the_no_verify_path_never_runs_a_repo_hook() {
    // The security property, proved by side effect rather than by argv: the hook
    // would write `hook-marker` and fail. With `--no-verify` the commit lands and
    // the marker is absent; without it the commit is refused and the marker is
    // there. A cloned repository controls that hook, so "the agent committed"
    // must not mean "the agent ran this repository's code".
    let s = repo_with_failing_hook("commit-nv-skip");
    let cwd = s.root().to_string_lossy().to_string();
    let marker = s.root().join("hook-marker");

    let sha = git::commit_with(&cwd, "feat: agent commit", true).expect("commit with --no-verify");
    assert!(!sha.is_empty(), "a short sha comes back");
    assert!(
        !marker.exists(),
        "the hook must not have run under --no-verify"
    );
    let log = run(&cwd, &["log", "--format=%s"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("feat: agent commit"),
        "the commit landed"
    );
}

#[test]
fn running_hooks_is_opt_in_and_a_bad_hook_can_refuse_the_commit() {
    // The other half of the same property: when the setting is flipped, the hook
    // really does run (so the opt-in is not a lie) and its failure is reported
    // rather than swallowed.
    let s = repo_with_failing_hook("commit-verify-runs");
    let cwd = s.root().to_string_lossy().to_string();
    let marker = s.root().join("hook-marker");

    let err = git::commit_with(&cwd, "feat: should be refused", false)
        .expect_err("a failing pre-commit hook must refuse the commit");
    // Not just "an error": git 2.52 on Windows reports a refused hook with an
    // empty stderr, so `commit_with` names the two possible causes itself. A
    // bare empty string here would reach the UI as a blank toast.
    assert!(
        err.contains("hook"),
        "the message has to name the hook: {err:?}"
    );
    assert!(marker.exists(), "the hook ran when verification was on");
    // Nothing was committed, so there is no HEAD to log: a plain `git log`
    // fails here, and that failure is itself the assertion.
    assert!(
        git::head(&cwd).is_none(),
        "a refused commit must leave no commit behind"
    );
}

#[test]
fn commit_with_still_commits_when_there_is_no_hook_at_all() {
    // The common case must not depend on the hook machinery above: a plain repo
    // commits either way, with the message it was given.
    let s = Sandbox::new("commit-plain");
    let cwd = s.root().to_string_lossy().to_string();
    s.write("a.txt", "hello\n");
    run(&cwd, &["init", "-q", "."]);
    run(&cwd, &["config", "user.name", "OpenLeash Test"]);
    run(&cwd, &["config", "user.email", "test@openleash.dev"]);

    let sha = git::commit_with(&cwd, "chore: plain", true).expect("plain commit");
    assert!(!sha.is_empty());
    // The legacy entry point still works, unchanged, alongside the new one.
    s.write("b.txt", "second\n");
    let sha2 = git::commit(&cwd, "chore: second").expect("legacy commit");
    assert!(!sha2.is_empty());
    assert_ne!(sha, sha2);
}

#[test]
fn a_trailer_composed_onto_the_message_is_what_lands_in_the_log() {
    // The end-to-end shape of the feature, minus the model: compose the message
    // the commit path is handed, commit it, and read it back out of `git log`.
    let s = Sandbox::new("commit-trailer");
    let cwd = s.root().to_string_lossy().to_string();
    s.write("a.txt", "hello\n");
    run(&cwd, &["init", "-q", "."]);
    run(&cwd, &["config", "user.name", "OpenLeash Test"]);
    run(&cwd, &["config", "user.email", "test@openleash.dev"]);

    let message = commitmsg::compose("feat: attributed", Attribution::CoAuthoredBy);
    git::commit_with(&cwd, &message, true).expect("attributed commit");
    let log = run(&cwd, &["log", "-1", "--format=%B"]);
    let body = String::from_utf8_lossy(&log.stdout);
    assert!(body.contains("feat: attributed"), "{body:?}");
    assert!(
        body.contains("Co-authored-by: OpenLeash <agent@openleash.dev>"),
        "the trailer is in the commit body: {body:?}"
    );
}
