//! Regression coverage for the compact Git metadata lookup used on task creation.

#![cfg(test)]

use super::git;
use super::reviewer_sandbox::Sandbox;
use std::process::Command;

fn run(cwd: &str, args: &[&str]) {
    let out = Command::new("git")
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
}

#[test]
fn task_creation_metadata_matches_the_existing_git_lookups() {
    let sandbox = Sandbox::new("task-git-metadata");
    let cwd = sandbox.root().to_string_lossy().to_string();
    run(&cwd, &["init", "-q"]);
    run(&cwd, &["config", "user.name", "OpenLeash Test"]);
    run(&cwd, &["config", "user.email", "test@openleash.dev"]);
    sandbox.write("file.txt", "task metadata\n");
    run(&cwd, &["add", "file.txt"]);
    run(&cwd, &["commit", "-q", "-m", "initial"]);

    let metadata = git::metadata(&cwd).expect("a committed repository is detected");
    assert_eq!(metadata.branch, git::current_branch(&cwd));
    assert_eq!(metadata.head, git::head(&cwd));
    assert!(metadata.head.is_some());
}

#[test]
fn unborn_git_repositories_have_no_branch_or_head() {
    let sandbox = Sandbox::new("task-git-unborn");
    let cwd = sandbox.root().to_string_lossy().to_string();
    run(&cwd, &["init", "-q"]);

    let metadata = git::metadata(&cwd).expect("an initialized repository is detected");
    assert_eq!(metadata.branch, "");
    assert_eq!(metadata.head, None);
}

#[test]
fn a_non_repository_has_no_git_metadata() {
    let sandbox = Sandbox::new("task-git-nonrepo");
    let cwd = sandbox.root().to_string_lossy().to_string();
    assert_eq!(git::metadata(&cwd), None);
}
