//! Release-gate tests for checkpoint bisect (`bisect.rs`, the per-edit log in
//! `checkpoint.rs`, and the user action `bisect_run` in `lib.rs`).
//!
//! What only a real disk, a real `git` and a real shell can show:
//!
//! - **The edit log.** One commit per agent edit, each holding the tree right
//!   after that edit, built without moving the store's `HEAD` or index.
//! - **The search, end to end.** A real command over real scratch copies
//!   finds the edit that broke it, says "all good" / "already bad", treats a
//!   timeout as unclassifiable, and blames shell/user changes between edits on
//!   an `external` step rather than on the next edit.
//! - **Isolation.** The user's files and their `.git` are byte-for-byte the
//!   same after a bisect, the command's own writes land only in scratch, and
//!   every scratch copy is gone afterwards — including a shared folder's link,
//!   whose target must survive the cleanup.
//! - **Permission parity for the user action.** The same `permissions::check`
//!   verdict as `bash`: a deny rule, plan mode and the Disabled rung (with
//!   chained commands and substitutions) all refuse, and nothing runs.
//!   The agent tool's parity is in `runner_bisect_tests.rs`.
//!
//! Every test claims its own `OPENLEASH_HOME` via `store::test_home` and holds
//! the guard for its whole run.

#![cfg(test)]
#![allow(clippy::await_holding_lock)]

use super::bisect::{self, Request, EXTERNAL};
use super::checkpoint::{self, EditMeta, Kind};
use super::store;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// The test command: exits 0 while `check.txt` still matches `good.txt`.
/// `git diff --no-index` behaves the same under bash and PowerShell, which is
/// what `shell::run` picks on a Windows machine without Git Bash.
pub(super) const CHECK: &str = "git diff --no-index --quiet good.txt check.txt";

pub(super) fn home(name: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
    let dir = std::env::temp_dir().join(format!(
        "openleash-bisect-{}-{}",
        name,
        uuid::Uuid::new_v4().simple()
    ));
    let guard = store::test_home(&dir);
    (dir, guard)
}

fn write(dir: &str, rel: &str, body: &str) {
    let p = Path::new(dir).join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, body).unwrap();
}

/// What the runner does after a successful edit: write, then log.
pub(super) fn agent_edit(chat: &str, wt: &str, i: usize, rel: &str, body: &str) {
    let item = format!("item-{i}");
    checkpoint::write_edit(
        chat,
        wt,
        EditMeta {
            count: 1,
            item_id: &item,
            sub_id: None,
            msg_index: i,
            tool: "write_file",
        },
        i,
        &Path::new(wt).join(rel),
        body.as_bytes(),
    )
    .expect("edit should be logged");
}

/// A project with a base snapshot and `n` logged edits; edit `bad_at` breaks
/// `check.txt` (0 = none does), every other edit adds an unrelated file.
pub(super) fn history(home: &Path, chat: &str, n: usize, bad_at: usize) -> String {
    let wt = home.join("proj");
    std::fs::create_dir_all(&wt).unwrap();
    let wt = wt.to_string_lossy().to_string();
    write(&wt, "good.txt", "fine\n");
    write(&wt, "check.txt", "fine\n");
    write(&wt, ".gitignore", "node_modules/\n");
    checkpoint::snapshot(chat, &wt, 0, Kind::User).unwrap();
    for i in 1..=n {
        if i == bad_at {
            agent_edit(chat, &wt, i, "check.txt", "broken\n");
        } else {
            agent_edit(chat, &wt, i, &format!("f{i}.txt"), &format!("{i}\n"));
        }
    }
    wt
}

fn req(cmd: &str) -> Request {
    Request::new(cmd.to_string(), None, None, Some(60_000), None, None)
}

fn noop(_: serde_json::Value) {}

/// Every file under `root` with its bytes, sorted — the user's tree, compared
/// whole before and after.
fn contents(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                out.push((rel, std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Scratch folders that hold a file named `marker`.
fn scratch_with(marker: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(bisect::scratch_root()) else {
        return vec![];
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("openleash-bisect-")
        })
        .map(|e| e.path())
        .filter(|p| p.join(marker).exists())
        .collect()
}

fn git(dir: &str, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn the_edit_log_keeps_one_tree_per_edit_without_moving_head() {
    let (home, _g) = home("log");
    let chat = "log";
    let wt = history(&home, chat, 0, 0);
    let head = checkpoint::head(chat).unwrap();
    agent_edit(chat, &wt, 1, "a.txt", "one\n");
    agent_edit(chat, &wt, 2, "a.txt", "two\n");
    agent_edit(chat, &wt, 3, "sub/b.txt", "three\n");
    assert_eq!(
        checkpoint::head(chat).as_deref(),
        Some(head.as_str()),
        "logging edits must not move the snapshot HEAD"
    );
    let edits = checkpoint::read_edits(chat);
    assert_eq!(edits.len(), 3);
    assert!(checkpoint::same_tree(chat, &edits[0].parent, &head));
    assert!(checkpoint::same_tree(
        chat,
        &edits[1].parent,
        &edits[0].commit
    ));
    assert!(checkpoint::same_tree(
        chat,
        &edits[2].parent,
        &edits[1].commit
    ));
    assert_eq!(edits[1].item_id, "item-2");
    assert_eq!(edits[2].path, "sub/b.txt");
    let at =
        |c: &str, p: &str| checkpoint::file_at(chat, c, p).map(|b| String::from_utf8(b).unwrap());
    assert_eq!(at(&edits[0].commit, "a.txt").as_deref(), Some("one\n"));
    assert_eq!(at(&edits[1].commit, "a.txt").as_deref(), Some("two\n"));
    assert_eq!(at(&edits[0].parent, "a.txt"), None);
    // The intermediate state survives even though a.txt was overwritten
    // before any snapshot could see "one".
    assert_eq!(at(&edits[2].commit, "a.txt").as_deref(), Some("two\n"));
    assert_eq!(
        at(&edits[2].commit, "sub/b.txt").as_deref(),
        Some("three\n")
    );
    // The store's own index is still in step with HEAD: nothing staged.
    assert!(checkpoint::snapshot(chat, &wt, 1, Kind::Agent)
        .unwrap()
        .is_some());
    let steps = bisect::timeline(chat, 1);
    assert_eq!(steps.len(), 3);
    assert!(steps.iter().all(|s| s.tool == "write_file"));
    // A later snapshot rebases the next edit on HEAD.
    agent_edit(chat, &wt, 4, "a.txt", "four\n");
    let edits = checkpoint::read_edits(chat);
    assert!(checkpoint::same_tree(
        chat,
        &edits[3].parent,
        &checkpoint::head(chat).unwrap()
    ));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rewinding_drops_the_edits_of_the_abandoned_turns() {
    let (home, _g) = home("trunc");
    let chat = "trunc";
    let wt = history(&home, chat, 2, 0);
    // A second turn's edit.
    write(&wt, "late.txt", "x\n");
    checkpoint::write_edit(
        chat,
        &wt,
        EditMeta {
            count: 2,
            item_id: "late",
            sub_id: Some("sub-1"),
            msg_index: 9,
            tool: "edit_file",
        },
        9,
        &Path::new(&wt).join("late.txt"),
        b"x
",
    )
    .unwrap();
    assert_eq!(bisect::timeline(chat, 2).len(), 3);
    assert_eq!(
        checkpoint::read_edits(chat)[2].sub_id.as_deref(),
        Some("sub-1")
    );
    assert_eq!(bisect::timeline(chat, 1).len(), 2, "a lower count hides it");
    checkpoint::truncate(chat, 1);
    assert_eq!(checkpoint::read_edits(chat).len(), 2);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn finds_the_breaking_edit_and_leaves_the_users_tree_alone() {
    let (home, _g) = home("found");
    let chat = "found";
    let wt = history(&home, chat, 9, 6);
    // Make the user's folder a real repository, so its `.git` is part of
    // what must not change.
    git(&wt, &["init", "-q"]);
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "user"]);
    let before = contents(Path::new(&wt));
    let status = git(&wt, &["status", "--porcelain"]);
    let user_head = git(&wt, &["rev-parse", "HEAD"]);
    let store_head = checkpoint::head(chat);
    let marker = format!("marker-{}.txt", uuid::Uuid::new_v4().simple());
    // The command writes into its working directory: that must be scratch.
    let cmd = format!("echo ran > {marker}; {CHECK}");
    let steps = bisect::timeline(chat, 1);
    let events = std::sync::Mutex::new(vec![]);
    let emit = |v: serde_json::Value| events.lock().unwrap().push(v);
    let r = bisect::run(
        chat,
        &wt,
        &steps,
        &req(&cmd),
        &CancellationToken::new(),
        &emit,
    )
    .await
    .unwrap();
    assert_eq!(r.outcome, "found", "{}", r.message);
    let c = r.culprit.as_ref().unwrap();
    assert_eq!(
        (c.index, c.path.as_str(), c.item_id.as_str()),
        (6, "check.txt", "item-6")
    );
    assert_eq!((c.add, c.del), (1, 1));
    assert!(c.lines.iter().any(|l| l["k"] == "a" && l["t"] == "broken"));
    assert!(
        r.probes.len() <= 2 + 4,
        "binary search, not a scan: {}",
        r.probes.len()
    );
    let ev = events.lock().unwrap();
    assert_eq!(ev.first().unwrap()["phase"], "start");
    assert_eq!(ev.last().unwrap()["phase"], "done");
    assert!(ev.iter().any(|e| e["phase"] == "probed"));
    drop(ev);

    assert_eq!(contents(Path::new(&wt)), before, "user files changed");
    assert_eq!(git(&wt, &["status", "--porcelain"]), status);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), user_head);
    assert_eq!(checkpoint::head(chat), store_head);
    assert!(!Path::new(&wt).join(&marker).exists());
    assert!(
        scratch_with(&marker).is_empty(),
        "scratch copies were left behind"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn all_good_already_bad_and_subranges() {
    let (home, _g) = home("ends");
    let chat = "ends";
    let wt = history(&home, chat, 5, 3);
    let steps = bisect::timeline(chat, 1);
    let cancel = CancellationToken::new();
    let r = bisect::run(chat, &wt, &steps, &req("git --version"), &cancel, &noop)
        .await
        .unwrap();
    assert_eq!(r.outcome, "all_good");
    assert_eq!(r.probes.len(), 1);
    let r = bisect::run(
        chat,
        &wt,
        &steps,
        &req("git diff --no-index --quiet good.txt missing.txt"),
        &cancel,
        &noop,
    )
    .await
    .unwrap();
    assert_eq!(r.outcome, "already_bad");
    // Range 4..5 starts after the break.
    let mut q = req(CHECK);
    q.from = Some(4);
    let r = bisect::run(chat, &wt, &steps, &q, &cancel, &noop)
        .await
        .unwrap();
    assert_eq!(r.outcome, "already_bad");
    // Range 2..3 contains it.
    let mut q = req(CHECK);
    (q.from, q.to) = (Some(2), Some(3));
    let r = bisect::run(chat, &wt, &steps, &q, &cancel, &noop)
        .await
        .unwrap();
    assert_eq!(r.culprit.map(|c| c.index), Some(3));
    let mut q = req(CHECK);
    (q.from, q.to) = (Some(4), Some(9));
    assert!(bisect::run(chat, &wt, &steps, &q, &cancel, &noop)
        .await
        .is_err());
    assert!(bisect::run(chat, &wt, &[], &req(CHECK), &cancel, &noop)
        .await
        .is_err());
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn a_timeout_is_unclassifiable_and_still_cleans_up() {
    let (home, _g) = home("timeout");
    let chat = "timeout";
    let wt = history(&home, chat, 3, 2);
    let steps = bisect::timeline(chat, 1);
    let marker = format!("marker-{}.txt", uuid::Uuid::new_v4().simple());
    let mut q = req(&format!("echo x > {marker}; sleep 8"));
    q.timeout_ms = 1_000;
    let r = bisect::run(chat, &wt, &steps, &q, &CancellationToken::new(), &noop)
        .await
        .unwrap();
    assert_eq!(r.outcome, "inconclusive", "{}", r.message);
    assert_eq!(r.probes[0].verdict, bisect::Verdict::Timeout);
    assert!(r.message.contains("timed out"));
    // A killed process can hold its folder for a moment on Windows.
    for _ in 0..50 {
        if scratch_with(&marker).is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(scratch_with(&marker).is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn flaky_runs_are_detected() {
    let (home, _g) = home("flaky");
    let chat = "flaky";
    let wt = history(&home, chat, 2, 0);
    let steps = bisect::timeline(chat, 1);
    // Fails on the first run in each scratch copy, passes on the second: the
    // copy is fresh per state, the counter file per copy.
    let mut q = req("if [ -f seen ]; then exit 0; else touch seen; exit 1; fi");
    q.runs = 2;
    if !super::shell::shell().name.contains("bash") {
        return; // the flaky command is bash syntax
    }
    let r = bisect::run(chat, &wt, &steps, &q, &CancellationToken::new(), &noop)
        .await
        .unwrap();
    assert_eq!(r.probes[0].verdict, bisect::Verdict::Flaky);
    assert_eq!(r.outcome, "inconclusive");
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn a_cancelled_bisect_reports_cancelled() {
    let (home, _g) = home("cancel");
    let chat = "cancel";
    let wt = history(&home, chat, 3, 2);
    let steps = bisect::timeline(chat, 1);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = bisect::run(chat, &wt, &steps, &req(CHECK), &cancel, &noop)
        .await
        .unwrap();
    assert_eq!(r.outcome, "cancelled");
    assert!(r.probes.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn ignored_files_are_not_in_edit_history_and_executable_mode_survives() {
    let (home, _g) = home("ignored-mode");
    let chat = "ignored-mode";
    let wt = history(&home, chat, 0, 0);
    write(&wt, ".gitignore", "node_modules/\n.env\n");
    write(&wt, ".env", "DO_NOT_STORE=this\n");
    let result = checkpoint::write_edit(
        chat,
        &wt,
        EditMeta {
            count: 1,
            item_id: "secret",
            sub_id: None,
            msg_index: 1,
            tool: "write_file",
        },
        1,
        &Path::new(&wt).join(".env"),
        b"DO_NOT_STORE=this\n",
    );
    assert!(result.is_ok());
    assert!(
        checkpoint::read_edits(chat).is_empty(),
        "ignored secret file must not enter history"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = Path::new(&wt).join("run.sh");
        write(&wt, "run.sh", "#!/bin/sh\nexit 0\n");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        checkpoint::write_edit(
            chat,
            &wt,
            EditMeta {
                count: 1,
                item_id: "exec",
                sub_id: None,
                msg_index: 2,
                tool: "write_file",
            },
            2,
            &script,
            b"#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        let rec = checkpoint::read_edits(chat).last().unwrap().clone();
        assert_eq!(
            checkpoint::mode_at(chat, &rec.commit, "run.sh").as_deref(),
            Some("100755")
        );
    }
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn changes_between_edits_are_their_own_external_step() {
    let (home, _g) = home("external");
    let chat = "external";
    let wt = history(&home, chat, 2, 0);
    // A user/shell write breaks another already-tracked file between two
    // tools in the SAME turn. No intervening turn snapshot is available.
    write(&wt, "check.txt", "broken by a command\n");
    agent_edit(chat, &wt, 3, "f3.txt", "3\n");
    let steps = bisect::timeline(chat, 1);
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[2].tool, EXTERNAL);
    assert!(steps[2].path.contains("check.txt"), "{}", steps[2].path);
    let r = bisect::run(
        chat,
        &wt,
        &steps,
        &req(CHECK),
        &CancellationToken::new(),
        &noop,
    )
    .await
    .unwrap();
    let c = r.culprit.unwrap();
    assert_eq!(
        (c.index, c.tool.as_str()),
        (3, EXTERNAL),
        "the edit after it is not to blame"
    );
    assert!(c
        .lines
        .iter()
        .any(|l| l["t"].as_str().unwrap_or("").contains("check.txt")));
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn a_shared_folder_is_linked_in_and_survives_cleanup() {
    let (home, _g) = home("share");
    let chat = "share";
    let wt = history(&home, chat, 3, 2);
    // Ignored, so not in any snapshot.
    write(&wt, "node_modules/dep/good.txt", "fine\n");
    let steps = bisect::timeline(chat, 1);
    let cmd = "git diff --no-index --quiet node_modules/dep/good.txt check.txt";
    let r = bisect::run(
        chat,
        &wt,
        &steps,
        &req(cmd),
        &CancellationToken::new(),
        &noop,
    )
    .await
    .unwrap();
    assert_eq!(
        r.outcome, "already_bad",
        "without the share the dep is missing"
    );
    let mut q = req(cmd);
    q.share = vec!["node_modules".into()];
    let r = bisect::run(chat, &wt, &steps, &q, &CancellationToken::new(), &noop)
        .await
        .unwrap();
    assert_eq!(r.culprit.map(|c| c.index), Some(2), "{}", r.message);
    assert_eq!(
        std::fs::read_to_string(Path::new(&wt).join("node_modules/dep/good.txt")).unwrap(),
        "fine\n",
        "cleaning up the link must not delete the user's folder"
    );
    // Anything that is not an ignored folder inside the project is refused.
    for bad in ["..", "../x", "check.txt", "/", "C:/Windows", "missing"] {
        let mut q = req(cmd);
        q.share = vec![bad.into()];
        let r = bisect::run(chat, &wt, &steps, &q, &CancellationToken::new(), &noop)
            .await
            .unwrap();
        assert!(
            r.probes.iter().all(|p| p.verdict == bisect::Verdict::Error),
            "share {bad} should not have been usable"
        );
    }
    let _ = std::fs::remove_dir_all(home);
}

// ─────────────────────── the user action's permissions ───────────────────────

fn harness_for(
    wt: &str,
    chat: &str,
    perm: &str,
    plan: bool,
    allow: &[&str],
) -> std::sync::Arc<super::Harness> {
    let mut t = super::tests::task(chat, "unused/model");
    t.cwd = wt.to_string();
    t.project = wt.to_string();
    t.perm = perm.into();
    t.plan = plan;
    t.checkpoints = vec![super::Checkpoint {
        item_index: 0,
        msg_index: 0,
    }];
    let s = store::Settings {
        allow: allow
            .iter()
            .map(|p| store::AllowRule {
                pattern: p.to_string(),
                project: String::new(),
            })
            .collect(),
        ..Default::default()
    };
    super::tests::harness(s, vec![t]).0
}

/// The commands that must be refused, and the settings that refuse them. Each
/// would leave `marker` behind if it ran.
pub(super) fn refused_cases(marker: &str) -> Vec<(&'static str, bool, Vec<&'static str>, String)> {
    vec![
        (
            "full",
            false,
            vec!["!echo *"],
            format!("echo pwned > {marker}"),
        ),
        ("full", true, vec![], format!("echo pwned > {marker}")),
        (
            "disabled",
            false,
            vec![],
            format!("git status && echo pwned > {marker}"),
        ),
        (
            "disabled",
            false,
            vec![],
            format!("git status; echo pwned > {marker}"),
        ),
        (
            "disabled",
            false,
            vec![],
            format!("git status | tee {marker}"),
        ),
        (
            "disabled",
            false,
            vec![],
            format!("echo $(echo pwned > {marker})"),
        ),
        (
            "disabled",
            false,
            vec![],
            format!("echo `echo pwned > {marker}`"),
        ),
        (
            "disabled",
            false,
            vec!["git status*"],
            format!("git status && echo pwned > {marker}"),
        ),
        ("disabled", false, vec![], format!("git status > {marker}")),
    ]
}

#[tokio::test]
async fn the_user_action_is_refused_wherever_bash_is_denied() {
    let (home, _g) = home("userperm");
    let chat = "userperm";
    let wt = history(&home, chat, 3, 2);
    let marker = home.join("ran.txt").to_string_lossy().replace('\\', "/");
    for (perm, plan, allow, cmd) in refused_cases(&marker) {
        let h = harness_for(&wt, chat, perm, plan, &allow);
        let rules: Vec<store::AllowRule> = h.settings.read().await.allow.clone();
        let ctx = super::permissions::Ctx {
            perm,
            plan,
            cwd: &wt,
            project: &wt,
            allow: &rules,
        };
        let want = match super::permissions::check(
            "bash",
            &serde_json::json!({"command": cmd}),
            None,
            &ctx,
        ) {
            super::permissions::Decision::Deny(r) => r,
            _ => panic!("fixture: bash should deny `{cmd}` ({perm}, plan={plan})"),
        };
        let got = crate::bisect_run(&h, chat, req(&cmd)).await;
        assert_eq!(
            got.err().as_deref(),
            Some(want.as_str()),
            "`{cmd}` ({perm}, plan={plan})"
        );
        assert!(!Path::new(&marker).exists(), "`{cmd}` ran");
    }
    // And with nothing in the way it runs, streaming `bisect` events.
    let h = harness_for(&wt, chat, "full", false, &[]);
    let r = crate::bisect_run(&h, chat, req(CHECK)).await.unwrap();
    assert_eq!(r.culprit.map(|c| c.index), Some(2));
    // A tool-name deny must also apply even though execution is checked as bash.
    let h = harness_for(&wt, chat, "full", false, &["!bisect_edits"]);
    assert!(crate::bisect_run(&h, chat, req(CHECK))
        .await
        .unwrap_err()
        .contains("!bisect_edits"));
    // Off checkpoints, docker and SSH refuse before anything runs.
    let h = harness_for(&wt, chat, "full", false, &[]);
    h.settings.write().await.checkpoints = false;
    assert!(crate::bisect_run(&h, chat, req(CHECK))
        .await
        .unwrap_err()
        .contains("Checkpoints are off"));
    let _ = std::fs::remove_dir_all(home);
}

// The Review action must participate in the same foreground lifecycle as tools,
// even when there is no main run (and therefore no Runtime::cancel token).
async fn user_bisect_lifecycle(force: bool) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let (home, _g) = home(if force { "userforce" } else { "userstop" });
    let chat = if force { "userforce" } else { "userstop" };
    let wt = history(&home, chat, 2, 1);
    let h = harness_for(&wt, chat, "full", false, &[]);
    let marker = format!("marker-{}.txt", uuid::Uuid::new_v4().simple());
    let command = format!("echo running > {marker}; sleep 30");
    let run = {
        let h = h.clone();
        tokio::spawn(async move { crate::bisect_run(&h, chat, req(&command)).await })
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while h.runtime(chat).fg.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("UI bisect was not registered as foreground work");
    let fg = h.runtime(chat).fg.load(Ordering::SeqCst);
    let busy = h.task(chat).await.unwrap().lock().await.busy;
    if force {
        h.force_pause(chat).await;
    } else {
        super::runner::interrupt(&h, chat).await;
    }
    let result = tokio::time::timeout(Duration::from_secs(5), run).await;
    // Always clean up the old implementation's detached operation before failing.
    if result.is_err() {
        bisect::cancel(chat);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(fg, 1, "UI bisect must count as foreground work");
    assert_eq!(busy, 1, "UI bisect must publish its busy count");
    let report = result
        .expect("Stop/force pause did not cancel UI bisect")
        .unwrap()
        .unwrap();
    assert_eq!(report.outcome, "cancelled", "{}", report.message);
    assert_eq!(h.runtime(chat).fg.load(Ordering::SeqCst), 0);
    assert_eq!(h.task(chat).await.unwrap().lock().await.busy, 0);
    assert!(!bisect::cancel(chat), "bisect registry entry leaked");
    for _ in 0..50 {
        if scratch_with(&marker).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(scratch_with(&marker).is_empty(), "scratch directory leaked");
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn user_bisect_foreground_stop_cancels_and_cleans_up() {
    user_bisect_lifecycle(false).await;
}

#[tokio::test]
async fn user_bisect_foreground_force_cancels_and_cleans_up() {
    user_bisect_lifecycle(true).await;
}

#[tokio::test]
async fn user_bisect_foreground_dropped_while_paused_releases_everything() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let (home, _g) = home("userdrop");
    let chat = "userdrop";
    let wt = history(&home, chat, 2, 1);
    let h = harness_for(&wt, chat, "full", false, &[]);
    h.pause_task(chat, "manual", "test").await;
    let run = {
        let h = h.clone();
        tokio::spawn(async move { crate::bisect_run(&h, chat, req("sleep 30")).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while h.runtime(chat).fg.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("UI bisect was not registered while paused");
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    assert_eq!(h.runtime(chat).fg.load(Ordering::SeqCst), 0);
    assert!(
        !bisect::cancel(chat),
        "dropped future leaked its registry slot"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.task(chat).await.unwrap().lock().await.busy != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("drop failed to publish foreground cleanup");
    let _ = std::fs::remove_dir_all(home);
}
