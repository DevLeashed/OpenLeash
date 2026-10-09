//! Release-gate tests for the file-restoring checkpoint store.
//!
//! `checkpoint` is the only code in the tree that *writes over the user's
//! files*, and it does so on the strength of a comparison against a shadow
//! repository the user has never seen. Two things about it can only be checked
//! against a real disk and a real `git`:
//!
//! - **Untracked files.** The whole reason this store exists instead of the
//!   task's own worktree is that a file the agent *created* is invisible to
//!   `git diff` against the task branch. A test that only asked "is the name in
//!   a list" would pass on a store that committed nothing, so every capture
//!   test below deletes the file and asserts the *bytes come back*.
//! - **The shadow store staying out of the user's repository.** A bug here is
//!   not a wrong diff, it is somebody's unmerged work. The test named in the
//!   module doc builds a real repository and asserts its `.git` is untouched.
//!
//! These tests touch disk, so each claims its own `OPENLEASH_HOME` through
//! `store::test_home` and holds the guard for its whole run: the env var is
//! process-wide and the tests in a binary run in parallel.

#![cfg(test)]

use super::checkpoint::{self, FileStat, Kind};
use super::store;
use std::path::{Path, PathBuf};

/// A private home for one test, plus the lock on `OPENLEASH_HOME`. Hold the
/// guard until the test ends or a parallel test will read our folder.
fn scratch(name: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
    let dir = std::env::temp_dir().join(format!("openleash-cp-{}-{}", name, std::process::id()));
    let guard = store::test_home(&dir);
    (dir, guard)
}

/// A fake "project" — deliberately *not* a git repository for most tests, so
/// every file in it is genuinely untracked by anything.
fn project_dir(home: &Path, name: &str) -> String {
    let p = home.join(name);
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().to_string()
}

fn write(dir: &str, rel: &str, body: &str) {
    let p = Path::new(dir).join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, body).unwrap();
}

fn read(dir: &str, rel: &str) -> String {
    std::fs::read_to_string(Path::new(dir).join(rel)).unwrap()
}

/// Commit a change and hand back the sha. Panics if the snapshot found nothing
/// to commit, which in these tests is always a set-up mistake worth failing on.
fn snap(chat: &str, worktree: &str, count: usize, kind: Kind) -> String {
    checkpoint::snapshot(chat, worktree, count, kind)
        .expect("snapshot should not fail")
        .expect("the fixture changed something, so a snapshot should have committed")
}

/// Only a file whose current content matches the explicit `record_agent_write`
/// ledger is eligible for overwrite. The final untracked file was changed by an
/// external writer, never attributed to the agent; even though the shadow repo
/// captured it, restore must leave it alone. This distinguishes file history
/// from restore authority.
fn agent_write(chat: &str, worktree: &str, rel: &str, item: usize, msg: usize) {
    checkpoint::record_agent_write(chat, worktree, item, msg, &Path::new(worktree).join(rel))
        .expect("edited file should be inside the task worktree");
}

fn has(files: &[FileStat], path: &str, status: &str) -> bool {
    files.iter().any(|f| f.path == path && f.status == status)
}

/// Every path under `root`, relative and sorted — for comparing a `.git`
/// directory before and after without caring what any one file contains.
fn tree_of(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            out.push(
                p.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            if p.is_dir() {
                walk(&p, base, out);
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Run git *in the user's own repository*, the way the app never does.
fn ugit(project: &str, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["-c", "safe.directory=*"])
        .args(args)
        .output()
        .expect("git should be installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The blob id `checkpoint`'s own hashing would compute for a file, using the
/// same config-free environment, so a hand-written ledger entry matches.
fn blob_id(cwd: &Path, file: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "core.autocrlf=false", "hash-object", "--"])
        .arg(file)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", cwd.join("gitconfig.global"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git should be installed");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ───────────────── a snapshot must keep what the agent created ─────────────────

/// The headline advantage over the task worktree: a file that is in no index
/// anywhere is still captured, and its *contents* are recoverable. Asserting a
/// name appears in a diff would pass on a store that committed nothing; the
/// delete-then-restore at the end is what proves the bytes are really held.
///
/// The deletion has to be snapshotted before it can be restored, and that is
/// the ledger working as designed: a path is only overwritten when its content
/// on disk still matches what the agent last left there, so a file that simply
/// vanished between snapshots reads as the user's and is left alone.
#[test]
fn a_checkpoint_captures_an_untracked_file() {
    let (home, _home_lock) = scratch("untracked");
    let work = project_dir(&home, "proj");
    let chat = "untracked-chat";

    write(&work, "base.txt", "base\n");
    let c0 = snap(chat, &work, 0, Kind::User);

    // Simulate a successful write_file call. Bash changes remain untracked by
    // provenance, even though the full-tree shadow snapshot captures the bytes.
    write(&work, "notes.txt", "an untracked payload\n");
    agent_write(chat, &work, "notes.txt", 1, 1);
    let c1 = snap(chat, &work, 0, Kind::Agent);

    let files = checkpoint::changed(chat, &c0, &c1);
    assert!(
        has(&files, "notes.txt", "A"),
        "an untracked file must be in the snapshot: {files:?}"
    );
    let now = checkpoint::changed_now(chat, &work, &c0);
    assert!(
        has(&now, "notes.txt", "A"),
        "and visible against the live folder too: {now:?}"
    );

    // A `bash`/external write is captured by the shadow tree, but is not in the
    // positive file-tool provenance ledger. Restore must not claim it as agent
    // work just because it differs from the target snapshot.
    write(&work, "notes.txt", "external content\n");
    let skipped = checkpoint::restore(chat, &work, &c0, &["notes.txt".to_string()]);
    assert_eq!(
        (skipped.restored, skipped.skipped.len()),
        (0, 1),
        "{skipped:?}"
    );
    assert_eq!(read(&work, "notes.txt"), "external content\n");

    // A later successful file write updates provenance. Rewinding to the prior
    // agent snapshot restores the bytes it contained, not the later version.
    write(&work, "notes.txt", "later agent content\n");
    agent_write(chat, &work, "notes.txt", 2, 2);
    let c2 = snap(chat, &work, 1, Kind::Agent);
    assert!(has(&checkpoint::changed(chat, &c1, &c2), "notes.txt", "M"));
    let r = checkpoint::restore(chat, &work, &c1, &["notes.txt".to_string()]);
    assert_eq!(r.restored, 1, "the file must be put back: {r:?}");
    assert!(r.skipped.is_empty(), "{r:?}");
    assert_eq!(
        read(&work, "notes.txt"),
        "an untracked payload\n",
        "the stored bytes, not just the name, must come back"
    );
}

// ───────────────── a restore puts the tree back ─────────────────

/// A restore puts positively attributed file writes back. The shadow tree still
/// records a shell deletion, but a delete has no successful edit-file provenance
/// to license recreating the file; that limit is disclosed in the picker.
#[test]
fn restoring_files_puts_content_back() {
    let (home, _home_lock) = scratch("restore");
    let work = project_dir(&home, "proj");
    let chat = "restore-chat";

    write(&work, "file.txt", "A\n");
    write(&work, "del.txt", "D\n");
    let a = snap(chat, &work, 0, Kind::User);

    write(&work, "file.txt", "B\n");
    write(&work, "new.txt", "N\n");
    agent_write(chat, &work, "file.txt", 1, 1);
    agent_write(chat, &work, "new.txt", 1, 1);
    std::fs::remove_file(Path::new(&work).join("del.txt")).unwrap();
    let b = snap(chat, &work, 1, Kind::Agent);

    let changed = checkpoint::changed(chat, &a, &b);
    assert!(has(&changed, "file.txt", "M"), "{changed:?}");
    assert!(has(&changed, "new.txt", "A"), "{changed:?}");
    assert!(has(&changed, "del.txt", "D"), "{changed:?}");

    let r = checkpoint::restore(
        chat,
        &work,
        &a,
        &["file.txt".to_string(), "new.txt".to_string()],
    );
    assert_eq!(r.restored, 2, "both known agent writes are undone: {r:?}");
    assert!(r.skipped.is_empty(), "{r:?}");
    assert_eq!(
        read(&work, "file.txt"),
        "A\n",
        "the modified file is A again"
    );
    assert!(
        !Path::new(&work).join("new.txt").exists(),
        "a file that did not exist at A is removed"
    );
    assert!(
        !Path::new(&work).join("del.txt").exists(),
        "shell deletions have no file-tool provenance"
    );
}

// ───────────────── the ledger: the agent's work, never the user's ─────────────────

/// The security property. A file the user edited by hand after the agent wrote
/// it must survive a rewind, and be named back in `skipped`; a file the user
/// never touched must still be restored. Both halves at once, because a rule
/// that skips everything would pass a test that only checked the first.
#[test]
fn the_user_edited_file_is_skipped_and_the_rest_restored() {
    let (home, _home_lock) = scratch("skip");
    let work = project_dir(&home, "proj");
    let chat = "skip-chat";

    write(&work, "file.txt", "A\n");
    write(&work, "keep.txt", "K0\n");
    let a = snap(chat, &work, 0, Kind::User);

    // The agent's turn: both files, so both are in the ledger.
    write(&work, "file.txt", "X\n");
    write(&work, "keep.txt", "K1\n");
    agent_write(chat, &work, "file.txt", 1, 1);
    agent_write(chat, &work, "keep.txt", 1, 1);
    let _b = snap(chat, &work, 1, Kind::Agent);

    // Then the user edits one file by hand after the agent write snapshot.
    write(&work, "file.txt", "Y\n");

    let r = checkpoint::restore(
        chat,
        &work,
        &a,
        &["file.txt".to_string(), "keep.txt".to_string()],
    );
    assert_eq!(
        r.restored, 1,
        "only the file the user never touched may be overwritten: {r:?}"
    );
    assert_eq!(
        r.skipped,
        vec!["file.txt".to_string()],
        "the user's edit is named back, not silently dropped: {r:?}"
    );
    assert_eq!(
        read(&work, "file.txt"),
        "Y\n",
        "a rewind must never undo the user's own work"
    );
    assert_eq!(
        read(&work, "keep.txt"),
        "K0\n",
        "and must still undo the agent's own work in the same pass"
    );
}

/// The module doc promises a second rewind keeps working because a restore
/// updates the ledger with what it wrote. Restoring to the *same* commit twice
/// would not test that — the second call sees `target == now` and short-circuits
/// before the ledger is consulted — so this walks the timeline backwards: C to
/// B, then B to A. If the ledger still held C, the second call would see a
/// mismatch for a file we ourselves had just placed and skip it.
#[test]
fn a_second_rewind_is_not_fooled_by_its_own_write() {
    let (home, _home_lock) = scratch("twice");
    let work = project_dir(&home, "proj");
    let chat = "twice-chat";

    write(&work, "f.txt", "A\n");
    let a = snap(chat, &work, 0, Kind::User);
    write(&work, "f.txt", "B\n");
    agent_write(chat, &work, "f.txt", 1, 1);
    let b = snap(chat, &work, 1, Kind::Agent);
    write(&work, "f.txt", "C\n");
    agent_write(chat, &work, "f.txt", 2, 2);
    let _c = snap(chat, &work, 2, Kind::Agent);

    let r1 = checkpoint::restore(chat, &work, &b, &["f.txt".to_string()]);
    assert_eq!((r1.restored, r1.skipped.len()), (1, 0), "{r1:?}");
    assert_eq!(read(&work, "f.txt"), "B\n");

    let r2 = checkpoint::restore(chat, &work, &a, &["f.txt".to_string()]);
    assert_eq!(
        (r2.restored, r2.skipped.len()),
        (1, 0),
        "a second rewind must not skip a file the first one placed: {r2:?}"
    );
    assert_eq!(read(&work, "f.txt"), "A\n");
}

// ───────────────── the shadow store must never touch the user's repo ─────────────────

/// The test the module doc names. A bug in the shadow store is not a wrong
/// diff — it is somebody's unmerged work, because a rewind exists to overwrite
/// files. So this builds a real repository, runs a full
/// ensure / snapshot / restore / target cycle against it, and then checks the
/// user's repository is exactly as it was.
///
/// Each assertion catches a different failure:
/// - unchanged `HEAD` / `rev-list --all`: a stray `commit`, `checkout` or
///   `reset` reaching the user's history.
/// - unchanged branch list: a `worktree add` / `branch -f` on their refs.
/// - unchanged `.git/index` bytes and only the expected unstaged file difference:
///   the shadow store must not stage the rewind in the user's repository.
/// - no new path under `.git`: an `index`, `objects/` write or reflog from a
///   git invocation that meant to be about our store.
/// - the store living under `OPENLEASH_HOME/checkpoints`: the structural fact
///   all of the above depends on.
#[test]
fn the_shadow_store_never_touches_the_users_repository() {
    let (home, _home_lock) = scratch("shadow");
    let work = project_dir(&home, "repo");
    let chat = "shadow-chat";

    // A real repository, exactly as a user would hand it to the agent.
    ugit(&work, &["init", "-q"]);
    write(&work, "tracked.txt", "base\n");
    ugit(&work, &["add", "-A"]);
    ugit(
        &work,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let head0 = ugit(&work, &["rev-parse", "HEAD"]);
    let branches0 = ugit(&work, &["branch", "--format=%(refname:short)"]);
    let index0 = std::fs::read(Path::new(&work).join(".git/index")).unwrap();
    let status0 = ugit(&work, &["status", "--porcelain"]);
    let history0 = ugit(&work, &["rev-list", "--all"]);
    let git_tree0 = tree_of(&Path::new(&work).join(".git"));
    assert_eq!(status0, "", "the fixture starts clean");

    // The pre-prompt state is a clean, committed checkout. The shadow store
    // creates an empty baseline commit for this no-change snapshot.
    let c0 = snap(chat, &work, 0, Kind::User);
    write(&work, "tracked.txt", "agent\n");
    agent_write(chat, &work, "tracked.txt", 1, 1);
    let c1 = snap(chat, &work, 1, Kind::Agent);
    assert_eq!(
        checkpoint::target(chat, 0).as_deref(),
        Some(c0.as_str()),
        "target(0) is the first snapshot"
    );
    assert_eq!(
        checkpoint::target(chat, 1).as_deref(),
        Some(c1.as_str()),
        "target(1) is the second"
    );
    assert_eq!(
        checkpoint::head(chat).as_deref(),
        Some(c1.as_str()),
        "the store's HEAD is its own newest commit"
    );

    let paths = checkpoint::candidates(chat, &work, &c0);
    let r = checkpoint::restore(chat, &work, &c0, &paths);
    assert_eq!(
        r.restored, 1,
        "the attributed file write round-trips: {r:?}"
    );
    assert!(r.skipped.is_empty(), "{r:?}");

    assert_eq!(read(&work, "tracked.txt"), "base\n");

    // 1. The user's history and refs are untouched.
    assert_eq!(
        ugit(&work, &["rev-parse", "HEAD"]),
        head0,
        "the shadow store must not move the user's HEAD"
    );
    assert_eq!(
        ugit(&work, &["branch", "--format=%(refname:short)"]),
        branches0,
        "nor add or move a branch of theirs"
    );
    assert_eq!(
        ugit(&work, &["rev-list", "--all"]),
        history0,
        "nor write a commit reachable from any of their refs"
    );

    // 2. Rewind writes files directly, without staging their restoration in
    //    the user's repository. The index remains byte-for-byte untouched.
    assert_eq!(
        std::fs::read(Path::new(&work).join(".git/index")).unwrap(),
        index0,
        "the user's index bytes must not change"
    );
    // The restored blob matches the user's committed index entry exactly, so
    // status is clean; the byte-for-byte index check above proves we never
    // touched that index while restoring from the shadow store.
    assert_eq!(ugit(&work, &["status", "--porcelain"]), status0);

    // 3. No new files appeared under `.git` — the strongest form of "nothing
    //    of ours landed in their repository".
    let git_tree1 = tree_of(&Path::new(&work).join(".git"));
    let new_files: Vec<&String> = git_tree1
        .iter()
        .filter(|p| !git_tree0.contains(p))
        .collect();
    assert!(
        new_files.is_empty(),
        "the shadow store added files under the user's .git: {new_files:?}"
    );

    // 4. The store is structurally somewhere else entirely.
    let store_dir = checkpoint::store(chat);
    assert!(
        store_dir.starts_with(checkpoint::root()),
        "the store must live under `checkpoints`, not {}",
        store_dir.display()
    );
    assert!(
        !store_dir.starts_with(Path::new(&work)),
        "the store must never live inside the user's project"
    );
    assert!(
        store_dir.join("shadow.git").join("HEAD").is_file(),
        "and it is a real repository of its own"
    );
    // The user's own `.git` was never a path the store committed: `add -A` on a
    // tree holding another checkout would otherwise stage a gitlink for it.
    for f in checkpoint::changed(chat, &c0, &c1) {
        assert!(
            !f.path.starts_with(".git"),
            "the store staged the user's .git: {}",
            f.path
        );
    }
}

// ───────────────── target: which turn does a checkpoint point at? ─────────────────

/// `target` is the indirection the whole rewind rests on: checkpoint `k` wants
/// the newest snapshot with `count <= k`. Snapshot here with `count` 0 (user),
/// 0 (agent) and 1 (user) — the shape a real turn loop produces — and pin both
/// sides of the boundary. An off-by-one (`<` instead of `<=`) restores the
/// wrong turn's files, with no error.
#[test]
fn target_maps_a_checkpoint_to_the_right_commit() {
    let (home, _home_lock) = scratch("target");
    let work = project_dir(&home, "proj");
    let chat = "target-chat";

    write(&work, "a.txt", "one\n");
    let c0 = snap(chat, &work, 0, Kind::User);
    write(&work, "a.txt", "two\n");
    let s1 = snap(chat, &work, 0, Kind::Agent);
    write(&work, "b.txt", "three\n");
    let s2 = snap(chat, &work, 1, Kind::User);
    assert_ne!(c0, s1);
    assert_ne!(s1, s2);

    assert_eq!(
        checkpoint::target(chat, 0).as_deref(),
        Some(s1.as_str()),
        "the newest snapshot with count <= 0 is the agent one taken at the same count"
    );
    assert_eq!(
        checkpoint::target(chat, 1).as_deref(),
        Some(s2.as_str()),
        "count 1 reaches past the count-0 pair"
    );
    assert_eq!(
        checkpoint::target(chat, 2).as_deref(),
        Some(s2.as_str()),
        "a count past the newest still resolves to the newest, never to nothing"
    );
}

// ───────────────── safe_rel: this is the function that writes files ─────────────────

/// `restore` is the only writer, and `safe_rel` is its one guard against a path
/// that escapes the worktree. Paths reaching it come from git's own output, so
/// anything with a drive letter, a leading separator or a `..` segment is not
/// ours and must be refused rather than joined.
///
/// The refusal has to be tested where it would actually hurt. A hostile path
/// with *nothing planted outside* would pass even with the guard removed,
/// because git cannot resolve `commit:../x` and the ledger has no entry — so
/// this plants real files outside the worktree and hand-writes ledger entries
/// for them. With the guard gone, `restore` would match the ledger and *delete*
/// those files (their target state is absent), which is what the assertions on
/// their contents catch. The legitimate path proves the call really restores
/// something, so the test cannot pass vacuously.
///
/// Hand-writing the `mine` sidecar couples this to its on-disk format; it is
/// the only way to give the guard something it would otherwise refuse, and the
/// alternative — a `#[cfg(test)]` window into `safe_rel` — would test the regex
/// instead of the path that writes.
#[test]
fn safe_rel_refuses_escapes_but_allows_relative_paths() {
    use std::io::Write;

    let (home, _home_lock) = scratch("saferel");
    let work = project_dir(&home, "proj");
    let home_s = home.to_string_lossy().to_string();
    let chat = "saferel-chat";

    // Legitimate path, updated through a successful agent file tool.
    write(&work, "a b/c.txt", "original\n");
    std::fs::create_dir_all(Path::new(&work).join("a")).unwrap();
    let c0 = snap(chat, &work, 0, Kind::User);
    write(&work, "a b/c.txt", "agent version\n");
    agent_write(chat, &work, "a b/c.txt", 1, 1);
    let _c1 = snap(chat, &work, 1, Kind::Agent);

    // Files *outside* the worktree that a `..`-bearing path would reach.
    write(&home_s, "probe.txt", "outside probe\n");
    write(&home_s, "b", "outside b\n");

    // Give the ledger entries for the hostile paths, as if a snapshot had once
    // recorded them. Nothing in production can reach this — which is the point:
    // the guard is what stops these paths being acted on.
    let mine_path = checkpoint::store(chat).join("mine");
    let mut ledger = std::fs::read_to_string(&mine_path).unwrap_or_default();
    if !ledger.is_empty() && !ledger.ends_with('\n') {
        ledger.push('\n');
    }
    for (rel, file) in [
        ("../probe.txt", home.join("probe.txt")),
        ("a/../../b", home.join("b")),
    ] {
        ledger.push_str(&format!("{}\t0\t0\t{}\n", blob_id(&home, &file), rel));
    }
    let mut f = std::fs::File::create(&mine_path).unwrap();
    f.write_all(ledger.as_bytes()).unwrap();
    drop(f);

    let r = checkpoint::restore(
        chat,
        &work,
        &c0,
        &[
            "../../etc/passwd".to_string(),
            "/abs/path".to_string(),
            "C:\\Windows\\x".to_string(),
            "a/../../b".to_string(),
            "../probe.txt".to_string(),
            "a b/c.txt".to_string(),
        ],
    );

    // A hostile path is refused before it is even considered: it is neither
    // restored nor reported as a skip.
    assert_eq!(
        r.restored, 1,
        "only the legitimate path may be written: {r:?}"
    );
    assert!(r.skipped.is_empty(), "refused paths are not skips: {r:?}");

    // Nothing outside the worktree was touched.
    assert!(
        home.join("probe.txt").is_file(),
        "'..' escaped the worktree"
    );
    assert_eq!(read(&home_s, "probe.txt"), "outside probe\n");
    assert!(
        home.join("b").is_file(),
        "a mid-path '..' escaped the worktree"
    );
    assert_eq!(read(&home_s, "b"), "outside b\n");

    // And the legitimate, space-bearing relative path was restored to its
    // pre-agent contents, proving it still went through the real writer.
    assert_eq!(read(&work, "a b/c.txt"), "original\n");
}
