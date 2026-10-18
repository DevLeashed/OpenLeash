//! File-restoring checkpoints: a per-chat shadow git repository.
//!
//! The task's working directory is snapshotted into
//! `~/.openleash/checkpoints/<chat_id>/shadow.git` — a repository of our own,
//! with its own object store, index and history, whose *work tree* is the
//! user's checkout. Nothing here ever touches the user's `.git`: every git
//! invocation names our git dir explicitly, points `GIT_CONFIG_GLOBAL` at an
//! empty file and sets `GIT_CONFIG_NOSYSTEM`, so git cannot discover, read or
//! write theirs. That separation is the whole point — a rewind exists to
//! overwrite files, and a bug that reached the user's real index or history
//! would destroy work that has no other copy. See
//! `reviewer_checkpoint::the_shadow_store_never_touches_the_users_repository`.
//!
//! Why a shadow repo rather than the task's own worktree: `git diff` against
//! the task's branch shows committed work only, so a file the agent *created*
//! is invisible to it, and there is nothing to `checkout` back for a path that
//! was never in the index. This store runs `add -A`, so untracked files are in
//! it — including files a `bash` command created, moved or removed, which is
//! the one class of change an edit-only tracker misses.
//!
//! # Checkpoints, snapshots and the two kinds of snapshot
//!
//! A snapshot is a commit of the whole work tree, appended to `turns` as
//! `<count> <kind> <sha>`, where `count` is `Task::checkpoints.len()` when it
//! was taken. There are two kinds:
//!
//! - [`Kind::User`] — taken between turns, when someone acts in the chat. The
//!   files have not moved since the last turn ended, so anything this commits
//!   is the *user's* own work.
//! - [`Kind::Agent`] — taken when the runner captures a completed turn. This
//!   stores the full tree, but does not itself grant restore authority to paths.
//!
//! Checkpoint `k` wants the file state *before* prompt `k` ran: the newest
//! snapshot with `count <= k`. The count climbs by one per submitted prompt and
//! never falls, so that is exactly the last moment at which the task had not
//! yet moved past prompt `k` — whether the snapshot is the user one taken as
//! that prompt was submitted or the agent one that ended the previous turn.
//!
//! # What may be overwritten: explicit file-write provenance
//!
//! The `mine` ledger is written only by [`record_agent_write`], called by the
//! executor immediately after a successful `edit_file`, `multi_edit`, or
//! `write_file`. A whole-worktree diff is deliberately not enough: a file that
//! changed during a turn could have been written by `bash`, a subagent or the
//! user, and guessing ownership from it would let rewind erase work it does not
//! own. On restore, a file is eligible only while its current blob matches this
//! explicit post-write blob; anything else is left alone and reported back.
//!
//! Capturing a turn and recording a file write are separate: a snapshot stores
//! the whole tree for comparison and fork, while [`record_agent_write`] alone
//! gives a file overwrite authority.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One file in a checkpoint's diff.
#[derive(Debug, Clone, Serialize)]
pub struct FileStat {
    pub path: String,
    /// A (added) | M (modified) | D (deleted) | T (type changed).
    pub status: String,
    pub add: usize,
    pub del: usize,
}

/// What a file restore did, including what it refused to touch.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Restored {
    pub restored: usize,
    /// Files left alone because their content is not what the agent last wrote.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    User,
    Agent,
}

impl Kind {
    fn code(self) -> char {
        match self {
            Kind::User => 'u',
            Kind::Agent => 'a',
        }
    }
}

/// The "no such path in this tree" marker in the ledger. A real blob id is hex,
/// so `-` can never collide with one.
const ABSENT: &str = "-";

/// Everything a snapshot needs, lifted off a task while its lock is held so the
/// git work itself can run in a `spawn_blocking` with no lock in hand. Taken by
/// `Harness`'s save paths; [`Snapshot::take`] is the part that touches the disk.
#[derive(Debug, Clone)]
pub struct Snapshot {
    chat: String,
    cwd: String,
    count: usize,
    item_index: usize,
    msg_index: usize,
    kind: Kind,
}

impl Snapshot {
    /// A snapshot of whatever `t` had at this moment. Defaults to
    /// [`Kind::Agent`], because the paths that call this without saying are the
    /// turn loop and the end of a run — see `Harness::save_task_now`.
    ///
    /// `enabled` is `Settings::checkpoints`: off means no shadow repo is ever
    /// created, which is the user asking for the old behaviour (and for not
    /// carrying a second copy of their folder around).
    pub fn of(t: &super::Task, enabled: bool) -> Option<Self> {
        // Checkpoints shadow a *local* folder; a remote (SSH) project has none.
        let enabled = enabled && !super::host::is_ssh(&t.cwd);
        enabled.then(|| Snapshot {
            chat: t.id.clone(),
            cwd: t.cwd.clone(),
            count: t.checkpoints.len(),
            item_index: t.items.len(),
            msg_index: t.messages.len(),
            kind: Kind::Agent,
        })
    }

    /// Mark this as a user's action rather than the agent's own turn, so
    /// whatever it commits is *not* folded into the overwrite ledger.
    pub fn user(mut self) -> Self {
        self.kind = Kind::User;
        self
    }

    /// Commit, and return the sha. `None` = nothing changed, or the folder is
    /// gone: neither is worth an error on a save path.
    pub fn take(self) -> Option<String> {
        snapshot_at(
            &self.chat,
            &self.cwd,
            self.count,
            self.item_index,
            self.msg_index,
            self.kind,
        )
        .ok()
        .flatten()
    }
}

/// How many changed entries one snapshot may have before we give up. A checkout
/// with tens of thousands of untracked files is not a workspace anyone rewinds
/// by hand, and committing it after every tool round would be a performance bug
/// dressed up as a feature.
const MAX_CHANGES: usize = 5_000;

/// Serialises every shadow-git write, process-wide.
///
/// Two snapshots of the same chat can be asked for at once — a user action and
/// the debounced writer both land here around the same moment — and `git add`
/// takes an `index.lock`. Left to race, one call fails with a lock error and is
/// treated as "no snapshot", which is safe but loses checkpoints for no reason.
/// Snapshotting is not a hot path, so one lock for the whole module is the
/// right trade.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_writes() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hold every shadow-git write, so a test can stand in for a snapshot that
/// takes seconds on a large checkout.
#[cfg(test)]
pub fn hold_writes_for_test() -> std::sync::MutexGuard<'static, ()> {
    lock_writes()
}

/// `~/.openleash/checkpoints`.
pub fn root() -> PathBuf {
    super::store::data_dir().join("checkpoints")
}

/// One chat's store. Task ids are hex, so this is always a safe path component;
/// anything else is sanitised rather than trusted, because an id that escaped
/// the folder would put our writes somewhere else.
pub fn store(chat: &str) -> PathBuf {
    let safe: String = chat
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    root().join(if safe.is_empty() {
        "unknown".to_string()
    } else {
        safe
    })
}

fn git_dir(chat: &str) -> PathBuf {
    store(chat).join("shadow.git")
}

fn turns_path(chat: &str) -> PathBuf {
    store(chat).join("turns")
}

fn mine_path(chat: &str) -> PathBuf {
    store(chat).join("mine")
}

pub fn exists(chat: &str) -> bool {
    git_dir(chat).join("HEAD").is_file()
}

/// Run git against the shadow store.
///
/// `worktree` is passed explicitly wherever there is one. The store's own
/// `core.worktree` is the fallback for calls that take none, but naming it per
/// call keeps "which checkout is this about?" local to the call, so a task
/// whose folder moved cannot silently snapshot the wrong tree.
fn git(chat: &str, worktree: Option<&str>, args: &[&str]) -> Result<Vec<u8>, String> {
    git_env(chat, worktree, args, None)
}

/// [`git`] with an optional private index file (`GIT_INDEX_FILE`), for the
/// plumbing that builds per-edit commits without touching the store's own
/// index — which the next snapshot's `status` depends on.
fn git_env(
    chat: &str,
    worktree: Option<&str>,
    args: &[&str],
    index: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let dir = store(chat);
    let gd = git_dir(chat);
    let mut c = Command::new("git");
    c.arg("-c")
        .arg("safe.directory=*")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("-c")
        .arg("core.autocrlf=false")
        .arg("-c")
        .arg("gc.auto=0")
        .arg("--git-dir")
        .arg(&gd);
    if let Some(w) = worktree {
        c.arg("--work-tree").arg(w);
    }
    c.args(args)
        .current_dir(&dir)
        // No global or system config: the user's `core.hooksPath`, aliases and
        // commit-signing settings must not reach a repository they have never
        // heard of, and a missing config file is an empty config to git.
        .env("GIT_CONFIG_GLOBAL", dir.join("gitconfig.global"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(index) = index {
        c.env("GIT_INDEX_FILE", index);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().map_err(|e| format!("git not available: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn git_str(chat: &str, worktree: Option<&str>, args: &[&str]) -> Result<String, String> {
    git(chat, worktree, args).map(|b| String::from_utf8_lossy(&b).to_string())
}

/// Write a small file next to the store, whole. Temp-then-rename so a crash
/// cannot leave a truncated ledger that would licence restoring files the user
/// has since edited.
fn write_sidecar(path: &Path, body: &str) {
    let tmp = path.with_extension("new");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn config(chat: &str, key: &str, value: &str) {
    let _ = git_str(chat, None, &["config", "--local", key, value]);
}

/// `value` as a git config value: always quoted, so a path holding `#`, `;`
/// or leading spaces cannot be cut short as a comment or trimmed.
fn config_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The last `core.worktree` in a config file's text, or `None` if it has none
/// or it is written in a form this reader does not handle. Only ever used to
/// *skip* a `git config` call when the value already matches, so anything it
/// gets wrong costs one process spawn, never a wrong setting.
fn config_worktree(text: &str) -> Option<String> {
    let mut in_core = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_core = line.eq_ignore_ascii_case("[core]");
            continue;
        }
        if !in_core {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("worktree") {
            continue;
        }
        found = config_unquote(raw.trim());
    }
    found
}

fn config_unquote(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut quoted = false;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => match chars.next()? {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                _ => return None,
            },
            '#' | ';' if !quoted => break,
            c => out.push(c),
        }
    }
    (!quoted).then(|| out.trim_end().to_string())
}

/// Create the store if it isn't there, and pin it to `worktree`.
///
/// `init --bare` followed by `core.bare=false`: the store has no checkout of
/// its own and must never grow one, so keeping `.git` *as* the directory makes
/// "which tree is this about" an explicit parameter of every call rather than
/// something git infers from the folder it happens to sit in.
///
/// This runs before every snapshot, so it spawns as little as it can: the
/// settings are appended to the config `init` wrote (whose platform probes —
/// `filemode`, `symlinks`, `ignorecase` — are kept), instead of one `git
/// config` process each, and `core.worktree` is only rewritten when the file
/// does not already say it. A later section overrides an earlier one for
/// single-valued keys, which is what makes appending a second `[core]` safe.
pub fn ensure(chat: &str, worktree: &str) -> Result<(), String> {
    super::host::refuse(worktree, "Checkpoints are")?;
    let dir = store(chat);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create the checkpoint folder: {e}"))?;
    let gd = git_dir(chat);
    let want = worktree.replace('\\', "/");
    let cfg = gd.join("config");
    if !gd.join("HEAD").is_file() {
        let gd_s = gd.to_string_lossy().to_string();
        git_str(chat, None, &["init", "--bare", "--quiet", &gd_s])?;
        let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        // `autocrlf`, `gpgsign` and `gc.auto` are belt and braces: `git()`'s
        // flags cover them, but a store carrying its own copy of them cannot be
        // talked out of them by a later `-c`.
        text.push_str(&format!(
            "[core]\n\tbare = false\n\tautocrlf = false\n\tworktree = {}\n\
             [user]\n\tname = OpenLeash\n\temail = checkpoints@openleash.local\n\
             [commit]\n\tgpgsign = false\n\
             [gc]\n\tauto = 0\n",
            config_quote(&want)
        ));
        if let Err(e) = write_config(&cfg, &text) {
            // Never leave a half-configured store behind: without `bare =
            // false` every later call would fail on "this operation must be
            // run in a work tree", and `HEAD` existing would stop this branch
            // from ever running again.
            let _ = std::fs::remove_dir_all(&gd);
            return Err(format!("cannot configure the checkpoint store: {e}"));
        }
        // `add -A` on a tree that contains another checkout would otherwise
        // stage a gitlink for it and then fail on the objects behind it.
        let info = gd.join("info");
        let _ = std::fs::create_dir_all(&info);
        let _ = std::fs::write(info.join("exclude"), ".git/\n");
        return Ok(());
    }
    let current = std::fs::read_to_string(&cfg)
        .ok()
        .and_then(|t| config_worktree(&t));
    if current.as_deref() != Some(want.as_str()) {
        config(chat, "core.worktree", &want);
    }
    Ok(())
}

/// Temp-then-rename, like the ledgers: git reading a truncated config would
/// fail every call against the store.
fn write_config(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// Record the exact post-write content of a successful agent file edit.
///
/// `absolute_path` is the resolved path the runner just wrote. It must resolve
/// inside `worktree`; a symlink that points outside is refused too. No
/// worktree-wide diff is used to infer ownership — this is the only way a file
/// enters the "the agent last wrote this" ledger that permits rewind to
/// overwrite it.
pub fn record_agent_write(
    task_id: &str,
    worktree: &str,
    item_index: usize,
    msg_index: usize,
    absolute_path: &Path,
) -> Result<(), String> {
    super::host::refuse(worktree, "Checkpoints are")?;
    let rel = rel_in_worktree(worktree, absolute_path)?;
    let _guard = lock_writes();
    ensure(task_id, worktree)?;
    let blob = blob_now(task_id, worktree, &rel);
    if blob == ABSENT {
        return Err("the edited file no longer exists".into());
    }
    let mut mine = read_mine(task_id);
    mine.insert(
        rel,
        AgentWrite {
            blob,
            item_index,
            msg_index,
        },
    );
    write_mine(task_id, &mine);
    Ok(())
}

/// `absolute_path` as a store-relative, `/`-separated path inside `worktree`.
/// Both sides are canonicalised, so a symlink that points out of the worktree
/// is refused rather than recorded under an inside-looking name.
fn rel_in_worktree(worktree: &str, absolute_path: &Path) -> Result<String, String> {
    super::tools::local_guard(absolute_path)?;
    let root = Path::new(worktree)
        .canonicalize()
        .map_err(|e| format!("cannot resolve worktree: {e}"))?;
    let path = absolute_path
        .canonicalize()
        .map_err(|e| format!("cannot resolve edited file: {e}"))?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| "edited file is outside the task worktree".to_string())?;
    let rel = rel
        .to_str()
        .ok_or_else(|| "edited file path is not valid UTF-8".to_string())?
        .replace('\\', "/");
    if !safe_rel(&rel) {
        return Err("edited file path is unsafe".into());
    }
    Ok(rel)
}

// ─────────────────────────── per-edit history ───────────────────────────
//
// Snapshots are per turn (and per tool round); an agent can make many edits in
// between. Bisecting "which edit broke it" needs the tree *after each edit*,
// so every successful file edit also becomes a commit object of its own in the
// shadow store, built with plumbing against a private index: the parent is the
// previous edit's commit while no snapshot has happened since, otherwise the
// newest snapshot (which then also carries whatever shell commands or the user
// changed in between). Nothing here moves `HEAD` or touches the store's index,
// so the snapshot/rewind machinery above sees exactly what it saw before.
// The commits are only reachable through the `edits` log; `gc.auto=0` keeps
// them alive for as long as the store exists.

/// Who made an edit, for the log. Borrowed so the runner need not allocate.
#[derive(Debug, Clone, Copy)]
pub struct EditMeta<'a> {
    /// `Task::checkpoints.len()` when the edit ran: the turn it belongs to, so
    /// a rewind's `truncate` can drop the edits of an abandoned branch.
    pub count: usize,
    /// The transcript row of the tool call (`Item::id`).
    pub item_id: &'a str,
    /// The subagent that made it, if not the main agent.
    pub sub_id: Option<&'a str>,
    /// `Task::messages.len()` when the edit ran.
    pub msg_index: usize,
    /// `edit_file` / `multi_edit` / `write_file`.
    pub tool: &'a str,
}

/// One line of the `edits` log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditRecord {
    pub count: usize,
    pub item_id: String,
    pub sub_id: Option<String>,
    pub msg_index: usize,
    pub tool: String,
    /// The store's `HEAD` when the edit was recorded.
    pub head: String,
    /// The tree before this edit.
    pub parent: String,
    /// The tree after this edit.
    pub commit: String,
    pub path: String,
}

fn edits_path(chat: &str) -> PathBuf {
    store(chat).join("edits")
}

/// Log fields are tab-separated with the path last; anything that could break
/// the line format is replaced rather than trusted.
fn log_field(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.is_empty() {
        "-".into()
    } else {
        s
    }
}

/// The edit log, oldest first. Lines that do not parse are skipped.
pub fn read_edits(chat: &str) -> Vec<EditRecord> {
    std::fs::read_to_string(edits_path(chat))
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.splitn(9, '\t').collect();
                    if f.len() != 9 || f[8].is_empty() || !safe_rel(f[8]) {
                        return None;
                    }
                    let opt = |v: &str| (v != "-").then(|| v.to_string());
                    Some(EditRecord {
                        count: f[0].parse().ok()?,
                        item_id: opt(f[1]).unwrap_or_default(),
                        sub_id: opt(f[2]),
                        msg_index: f[3].parse().ok()?,
                        tool: f[4].to_string(),
                        head: f[5].to_string(),
                        parent: f[6].to_string(),
                        commit: f[7].to_string(),
                        path: f[8].to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Write an agent edit and capture its before/after states under the same
/// checkpoint writer lock. Other agent edits cannot slip between the write
/// and its capture. External filesystem writers are not locked by OpenLeash:
/// their changes observed before this write belong to the parent tree, not
/// to this tool call.
///
/// Snapshot failures must not turn a successful edit into a reported failure;
/// only creating/writing the actual file propagates an error. The private
/// index uses ordinary `git add`, never forced cache insertion, so ignored
/// secrets and ignored build/dependency files stay out of the history.
pub fn write_edit(
    chat: &str,
    worktree: &str,
    meta: EditMeta<'_>,
    item_index: usize,
    absolute_path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    let _guard = lock_writes();
    if let Some(dir) = absolute_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create directory: {e}"))?;
    }
    let before = prepare_edit(chat, worktree, absolute_path);
    std::fs::write(absolute_path, bytes).map_err(|e| format!("write failed: {e}"))?;
    // Provenance and bisect history are separate; neither grants the other
    // restore authority. Both are captured before releasing the writer lock.
    if let Ok(rel) = rel_in_worktree(worktree, absolute_path) {
        let blob = blob_now(chat, worktree, &rel);
        if blob != ABSENT {
            let mut mine = read_mine(chat);
            mine.insert(
                rel,
                AgentWrite {
                    blob,
                    item_index,
                    msg_index: meta.msg_index,
                },
            );
            write_mine(chat, &mine);
        }
    }
    if let Ok((rel, head, parent, index)) = before {
        let _ = finish_edit(chat, worktree, meta, rel, head, parent, &index);
        let _ = std::fs::remove_file(index);
    }
    Ok(())
}

/// Private index representing the actual current tracked/non-ignored tree,
/// rather than the previous tool's synthetic tree. This records user/shell
/// changes between file tools even when no turn snapshot happened between.
fn prepare_edit(
    chat: &str,
    worktree: &str,
    path: &Path,
) -> Result<(String, String, String, PathBuf), String> {
    super::host::refuse(worktree, "Checkpoints are")?;
    let root = Path::new(worktree)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let parent_dir = path
        .parent()
        .ok_or("edited file has no parent")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let full = parent_dir.join(path.file_name().ok_or("edited file has no name")?);
    let rel = full
        .strip_prefix(&root)
        .map_err(|_| "edited file is outside the task worktree")?
        .to_str()
        .ok_or("edited path is not UTF-8")?
        .replace('\\', "/");
    if !safe_rel(&rel) || rel.contains(['\t', '\n', '\r']) {
        return Err("edited file path cannot be logged".into());
    }
    ensure(chat, worktree)?;
    let head = head(chat).ok_or("no snapshot to build the edit history on yet")?;
    let index = store(chat).join("edit-index");
    let _ = std::fs::remove_file(&index);
    let result = (|| {
        git_env(chat, None, &["read-tree", &head], Some(&index))?;
        git_env(
            chat,
            Some(worktree),
            &["add", "-A", "--", "."],
            Some(&index),
        )?;
        let tree = String::from_utf8_lossy(&git_env(chat, None, &["write-tree"], Some(&index))?)
            .trim()
            .to_string();
        let parent = git_str(
            chat,
            None,
            &["commit-tree", &tree, "-p", &head, "-m", "before agent edit"],
        )?
        .trim()
        .to_string();
        Ok((rel, head, parent, index.clone()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(index);
    }
    result
}

fn finish_edit(
    chat: &str,
    worktree: &str,
    meta: EditMeta<'_>,
    rel: String,
    head: String,
    parent: String,
    index: &Path,
) -> Result<EditRecord, String> {
    // Ordinary git add respects excludes and captures the platform's executable
    // bit. Unlike update-index --cacheinfo, it cannot force an ignored secret or
    // dependency file into the history.
    git_env(chat, Some(worktree), &["add", "--", &rel], Some(index))?;
    let tree = String::from_utf8_lossy(&git_env(chat, None, &["write-tree"], Some(index))?)
        .trim()
        .to_string();
    let commit = git_str(
        chat,
        None,
        &[
            "commit-tree",
            &tree,
            "-p",
            &parent,
            "-m",
            &format!("edit {rel}"),
        ],
    )?
    .trim()
    .to_string();
    let rec = EditRecord {
        count: meta.count,
        item_id: meta.item_id.to_string(),
        sub_id: meta.sub_id.map(String::from),
        msg_index: meta.msg_index,
        tool: meta.tool.to_string(),
        head,
        parent,
        commit,
        path: rel,
    };
    use std::io::Write;
    let line = format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        rec.count,
        log_field(&rec.item_id),
        log_field(rec.sub_id.as_deref().unwrap_or("")),
        rec.msg_index,
        log_field(&rec.tool),
        rec.head,
        rec.parent,
        rec.commit,
        rec.path
    );
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(edits_path(chat))
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| format!("cannot write the edit log: {e}"))?;
    Ok(rec)
}

/// The bytes of `path` in `commit`, or `None` when that tree has no such file.
pub fn file_at(chat: &str, commit: &str, path: &str) -> Option<Vec<u8>> {
    if !safe_rel(path) {
        return None;
    }
    git(
        chat,
        None,
        &["cat-file", "blob", &format!("{commit}:{path}")],
    )
    .ok()
}

/// The git mode for a path in a commit, or `None` if absent.
pub fn mode_at(chat: &str, commit: &str, path: &str) -> Option<String> {
    if !safe_rel(path) {
        return None;
    }
    git_str(chat, None, &["ls-tree", commit, "--", path])
        .ok()?
        .split_whitespace()
        .next()
        .map(String::from)
}

/// Whether two commits hold the same tree.
pub fn same_tree(chat: &str, a: &str, b: &str) -> bool {
    let tree = |c: &str| {
        git_str(
            chat,
            None,
            &["rev-parse", "--verify", "--quiet", &format!("{c}^{{tree}}")],
        )
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    };
    match (tree(a), tree(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Capture the most recently submitted user turn without the caller needing to
/// reconstruct the snapshot ledger's internal marker tuple.
pub fn capture_latest_turn(task_id: &str, worktree: &str) -> Result<(), String> {
    super::host::refuse(worktree, "Checkpoints are")?;
    let (count, item, msg) = read_turns(task_id)
        .into_iter()
        .rev()
        .find(|(_, _, _, kind, _)| *kind == Kind::User.code())
        .map(|(count, item, msg, _, _)| (count, item, msg))
        .ok_or_else(|| "no user checkpoint was recorded for this task".to_string())?;
    snapshot_at(task_id, worktree, count + 1, item, msg, Kind::Agent)?;
    Ok(())
}

/// Convenience for fixtures that already know the checkpoint count. Runtime
/// code uses [`Snapshot::take`] for a user snapshot and `capture_latest_turn`
/// for the corresponding agent snapshot.
#[cfg(test)]
pub fn snapshot(
    chat: &str,
    worktree: &str,
    count: usize,
    kind: Kind,
) -> Result<Option<String>, String> {
    snapshot_at(chat, worktree, count, count, count, kind)
}

/// Record one point in the checkpoint timeline. Snapshots never infer which
/// files belong to the agent: only `record_agent_write` populates that ledger.
fn snapshot_at(
    chat: &str,
    worktree: &str,
    count: usize,
    item_index: usize,
    msg_index: usize,
    kind: Kind,
) -> Result<Option<String>, String> {
    super::host::refuse(worktree, "Checkpoints are")?;
    if worktree.is_empty() || !Path::new(worktree).is_dir() {
        return Err("That chat's folder is not there any more.".into());
    }
    let _guard = lock_writes();
    ensure(chat, worktree)?;
    let status = git_str(chat, Some(worktree), &["status", "--porcelain"])?;
    if status.lines().count() > MAX_CHANGES {
        return Err(format!(
            "Too much changed at once ({MAX_CHANGES}+ entries) to checkpoint this folder."
        ));
    }
    // A brand-new shadow repo has no baseline. Even a clean user worktree must
    // be added in full here; an empty baseline would make rewinding the first
    // turn remove every tracked file. The index is ours (`--git-dir` above), so
    // this reads the working tree without staging in the user's repository.
    let first_snapshot = head(chat).is_none();
    if first_snapshot || !status.trim().is_empty() {
        git_str(chat, Some(worktree), &["add", "-A"])?;
        git_str(
            chat,
            Some(worktree),
            &[
                "commit",
                "--quiet",
                "--no-verify",
                "--allow-empty",
                "-m",
                "checkpoint",
            ],
        )?;
    }
    let sha = git_str(chat, Some(worktree), &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if sha.is_empty() {
        return Ok(None);
    }
    // No-change turns still need a timeline marker. Repeating the same sha is
    // intentional: the checkpoint existed, the files simply did not move.
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(turns_path(chat))
    {
        let _ = writeln!(f, "{count} {item_index} {msg_index} {} {sha}", kind.code());
    }
    Ok(Some(sha))
}

/// Record one point in the checkpoint timeline. Snapshots never infer which
/// files belong to the agent: only `record_agent_write` populates that ledger.

#[derive(Debug, Clone)]
struct AgentWrite {
    blob: String,
    item_index: usize,
    msg_index: usize,
}

fn write_mine(chat: &str, mine: &HashMap<String, AgentWrite>) {
    let mut body = String::with_capacity(mine.len() * 80);
    for (path, record) in mine {
        body.push_str(&record.blob);
        body.push('\t');
        body.push_str(&record.item_index.to_string());
        body.push('\t');
        body.push_str(&record.msg_index.to_string());
        body.push('\t');
        body.push_str(path);
        body.push('\n');
    }
    write_sidecar(&mine_path(chat), &body);
}

fn read_mine(chat: &str) -> HashMap<String, AgentWrite> {
    std::fs::read_to_string(mine_path(chat))
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let mut fields = l.splitn(4, '\t');
                    let blob = fields.next()?.to_string();
                    let item_index = fields.next()?.parse().ok()?;
                    let msg_index = fields.next()?.parse().ok()?;
                    let path = fields.next()?;
                    (!path.is_empty()).then(|| {
                        (
                            path.to_string(),
                            AgentWrite {
                                blob,
                                item_index,
                                msg_index,
                            },
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The commit holding the file state *before* checkpoint `count` ran.
pub fn target(chat: &str, count: usize) -> Option<String> {
    read_turns(chat)
        .into_iter()
        .rfind(|(n, _, _, _, _)| *n <= count)
        .map(|(_, _, _, _, sha)| sha)
}

fn read_turns(chat: &str) -> Vec<(usize, usize, usize, char, String)> {
    std::fs::read_to_string(turns_path(chat))
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let mut it = l.splitn(5, ' ');
                    let count: usize = it.next()?.trim().parse().ok()?;
                    let item: usize = it.next()?.trim().parse().ok()?;
                    let msg: usize = it.next()?.trim().parse().ok()?;
                    let kind = it.next()?.trim().chars().next().unwrap_or('u');
                    let sha = it.next()?.trim().to_string();
                    (!sha.is_empty()).then_some((count, item, msg, kind, sha))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The newest snapshot.
pub fn head(chat: &str) -> Option<String> {
    git_str(chat, None, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The blob of `path` in `commit`, or [`ABSENT`] when that commit has no such
/// path.
fn blob_at(chat: &str, commit: &str, path: &str) -> String {
    git_str(
        chat,
        None,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{commit}:{path}"),
        ],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| ABSENT.to_string())
}

/// The blob of `path` as it is on disk now ([`ABSENT`] = deleted).
///
/// Hashed *by the shadow repo* (`--git-dir` and `--work-tree` both explicit) so
/// the bytes are converted exactly as `add -A` converted them when it wrote the
/// commit we compare against. A bare `git hash-object` in the worktree would
/// discover the user's own repository and apply the user's attributes instead,
/// and a mismatch there makes the skip rule refuse every file — safe, but it
/// would quietly turn the feature off.
fn blob_now(chat: &str, worktree: &str, path: &str) -> String {
    let abs = Path::new(worktree).join(path);
    if !abs.is_file() {
        return ABSENT.to_string();
    }
    let dir = store(chat);
    let mut c = Command::new("git");
    c.arg("-c")
        .arg("core.autocrlf=false")
        .arg("--git-dir")
        .arg(git_dir(chat))
        .arg("--work-tree")
        .arg(worktree)
        .arg("hash-object")
        .arg("--")
        .arg(&abs)
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("gitconfig.global"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ABSENT.to_string())
}

/// Every path in a snapshot's tree.
fn names_in(chat: &str, commit: &str) -> Vec<String> {
    git_str(chat, None, &["ls-tree", "-r", "--name-only", "-z", commit])
        .map(|s| {
            s.split('\0')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Files that differ between two snapshots, with their line counts.
pub fn changed(chat: &str, from: &str, to: &str) -> Vec<FileStat> {
    let mut out: Vec<FileStat> = vec![];
    let Ok(raw) = git_str(
        chat,
        None,
        &["diff", "--no-renames", "--raw", "-z", from, to, "--"],
    ) else {
        return out;
    };
    // `:mode mode oldsha newsha STATUS\0path\0`, one pair per file.
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut i = 0;
    while i + 1 < fields.len() {
        let meta = fields[i];
        let path = fields[i + 1];
        i += 2;
        if path.is_empty() {
            continue;
        }
        let status = meta
            .split(' ')
            .next_back()
            .and_then(|s| s.chars().next())
            .map(String::from)
            .unwrap_or_else(|| "M".into());
        out.push(FileStat {
            path: path.to_string(),
            status,
            add: 0,
            del: 0,
        });
    }
    if let Ok(ns) = git_str(
        chat,
        None,
        &["diff", "--no-renames", "--numstat", "-z", from, to, "--"],
    ) {
        let nums: Vec<&str> = ns.split('\0').collect();
        let mut i = 0;
        while i + 1 < nums.len() {
            let head = nums[i];
            let path = nums[i + 1];
            i += 2;
            let mut it = head.split('\t');
            let add = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let del = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            if let Some(f) = out.iter_mut().find(|f| f.path == path) {
                f.add = add;
                f.del = del;
            }
        }
    }
    out
}

/// Files that differ between a snapshot and the working tree right now —
/// including files the store has never committed, which for the turn in
/// progress is most of them.
pub fn changed_now(chat: &str, worktree: &str, from: &str) -> Vec<FileStat> {
    if !exists(chat) || super::host::is_ssh(worktree) {
        return vec![];
    }
    let mut out = changed(chat, from, "HEAD");
    if let Ok(untracked) = git_str(
        chat,
        Some(worktree),
        &["ls-files", "-o", "-z", "--exclude-standard"],
    ) {
        for p in untracked.split('\0').filter(|p| !p.is_empty()) {
            if out.iter().any(|f| f.path == p) {
                continue;
            }
            let content = std::fs::read_to_string(Path::new(worktree).join(p)).unwrap_or_default();
            out.push(FileStat {
                path: p.to_string(),
                status: "A".into(),
                add: content.lines().count(),
                del: 0,
            });
        }
    }
    out
}

/// Reject a path that could write outside the worktree. Paths here come from
/// git's own output and are always relative to the work-tree root, so anything
/// with a drive letter, a leading separator or a `..` segment is not one of
/// ours — and is refused rather than joined, because this is the one function
/// in the module that writes to the user's files.
fn safe_rel(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains(':')
        && !path.split(['/', '\\']).any(|s| s == "..")
}

/// Put files back, refusing to clobber anything the user has touched since.
///
/// The rule, and it is the difference between an undo and a data-loss bug: a
/// file is overwritten only when its content on disk still matches what *the
/// agent last wrote* (the `mine` ledger). If the user edited it by hand
/// afterwards, or wrote a file the agent never did, the content differs, so it
/// is left exactly as the user left it and reported in [`Restored::skipped`].
/// A rewind undoes the agent's work and never the user's.
///
/// The write is a blob straight to the file — no `checkout`, no index. That
/// keeps the user's index untouched and keeps ours consistent, which the next
/// snapshot's `status` depends on.
pub fn restore(chat: &str, worktree: &str, commit: &str, paths: &[String]) -> Restored {
    // Fail closed: nothing restored on a remote (SSH) folder.
    if super::host::is_ssh(worktree) {
        return Restored::default();
    }
    let _guard = lock_writes();
    let mut out = Restored::default();
    let mut mine = read_mine(chat);
    let mut dirty_ledger = false;
    for path in paths.iter().filter(|p| safe_rel(p)) {
        let target = blob_at(chat, commit, path);
        let now = blob_now(chat, worktree, path);
        // Already the state we would write: nothing to do, and not a skip.
        if target == now {
            continue;
        }
        if mine.get(path).map(|r| r.blob.as_str()) != Some(now.as_str()) {
            out.skipped.push(path.clone());
            continue;
        }
        if !write_blob(chat, worktree, &target, path) {
            continue;
        }
        // The disk now holds a version from the agent's timeline, so the ledger
        // follows it: leaving the old blob behind would make the *next* rewind
        // see a mismatch and refuse a file we ourselves just placed.
        let previous = mine.get(path);
        mine.insert(
            path.clone(),
            AgentWrite {
                blob: target,
                item_index: previous.map_or(0, |r| r.item_index),
                msg_index: previous.map_or(0, |r| r.msg_index),
            },
        );
        dirty_ledger = true;
        out.restored += 1;
    }
    if dirty_ledger {
        write_mine(chat, &mine);
    }
    out
}

/// Write one blob from the shadow object store into the work tree, or remove
/// the file when the target state is absent. Returns whether anything changed.
fn write_blob(chat: &str, worktree: &str, blob: &str, path: &str) -> bool {
    let abs = Path::new(worktree).join(path);
    if blob == ABSENT {
        return match std::fs::remove_file(&abs) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => false,
        };
    }
    let Ok(bytes) = git(chat, None, &["cat-file", "blob", blob]) else {
        return false;
    };
    if let Some(dir) = abs.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&abs, bytes).is_ok()
}

/// Every path a restore from this snapshot would look at: what differs between
/// it and the working tree as it is now.
pub fn candidates(chat: &str, worktree: &str, commit: &str) -> Vec<String> {
    changed_now(chat, worktree, commit)
        .into_iter()
        .map(|f| f.path)
        .collect()
}

/// Write a snapshot's whole tree into `dest`, a fresh checkout.
///
/// Files are copied, never deleted: `dest` is a new branch point and a path that
/// exists there but not in the snapshot belongs to the branch being forked
/// from.
///
/// The temporary index is load-bearing. `checkout` writes an index as it goes,
/// and letting it write the *store's* index would leave our view of the tree
/// pointing at a directory that is not the store's work tree — the next
/// `git status` would then lie about what had changed, and the skip rule is
/// built on that answer.
pub fn materialize(chat: &str, commit: &str, dest: &str) -> Result<usize, String> {
    materialize_with(chat, commit, dest, "fork-index")
}

/// [`materialize`] for a bisect scratch copy. Its own index file, so a bisect
/// running next to a fork of the same chat cannot delete the fork's index
/// out from under it.
pub fn materialize_scratch(chat: &str, commit: &str, dest: &str) -> Result<usize, String> {
    materialize_with(
        chat,
        commit,
        dest,
        &format!("bisect-index-{}", super::new_id()),
    )
}

fn materialize_with(chat: &str, commit: &str, dest: &str, index: &str) -> Result<usize, String> {
    super::host::refuse(dest, "Checkpoints are")?;
    if !Path::new(dest).is_dir() {
        return Err("The new worktree is not there.".into());
    }
    let tmp = store(chat).join(index);
    let _ = std::fs::remove_file(&tmp);
    let dir = store(chat);
    let mut c = Command::new("git");
    c.arg("-c")
        .arg("safe.directory=*")
        .arg("-c")
        .arg("core.autocrlf=false")
        .arg("--git-dir")
        .arg(git_dir(chat))
        .arg("--work-tree")
        .arg(dest)
        .args(["checkout", commit, "--", "."])
        .current_dir(dest)
        .env("GIT_INDEX_FILE", &tmp)
        .env("GIT_CONFIG_GLOBAL", dir.join("gitconfig.global"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().map_err(|e| format!("git not available: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // Count from the commit, not the directory: the directory also holds
    // everything the branch already had.
    Ok(names_in(chat, commit).len())
}

/// A self-describing label for a checkpoint, in the style Gemini CLI uses: when
/// it happened, and which file it was about. Pure formatting, so it can be
/// tested without a repository.
pub fn label(ts: &str, files: &[FileStat]) -> String {
    let when: String = ts.chars().take(19).collect();
    if files.is_empty() {
        return format!("{when} · no file changes");
    }
    // The longest path, so a top-level file does not name a turn that was
    // mostly about something three directories down.
    let name = files
        .iter()
        .map(|f| f.path.as_str())
        .max_by_key(|p| p.len())
        .and_then(|p| p.rsplit(['/', '\\']).next())
        .unwrap_or("turn");
    let n = files.len();
    format!(
        "{when} · {name}{}",
        if n > 1 {
            format!(" (+{} more)", n - 1)
        } else {
            String::new()
        }
    )
}

/// Drop the snapshots past `count`, so a rewind's shorter conversation cannot be
/// served by a restore point from the branch it just abandoned.
///
/// The commits stay (unreferenced objects, and unreachable from any ref we hand
/// out), but the index of them must not: `target` reads this file last-wins, so
/// leaving a stale `<higher count>` line in place would let the *next*
/// checkpoint restore to a tree that included work the user rewound away.
///
/// The `mine` ledger is deliberately left alone. It only ever licenses
/// overwriting content the agent itself produced, so a leftover entry can still
/// only cause a safe restore — and rewinding twice has to keep working, which
/// needs those entries.
pub fn truncate(chat: &str, count: usize) {
    let _guard = lock_writes();
    let kept: Vec<String> = read_turns(chat)
        .into_iter()
        .filter(|(n, _, _, _, _)| *n <= count)
        .map(|(n, item, msg, kind, sha)| format!("{n} {item} {msg} {kind} {sha}"))
        .collect();
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    write_sidecar(&turns_path(chat), &body);
    // The per-edit history follows the same cut: an edit made in a turn that
    // no longer exists must not be offered as a bisect step.
    let edits = read_edits(chat);
    if edits.iter().any(|e| e.count > count) {
        let body: String = std::fs::read_to_string(edits_path(chat))
            .unwrap_or_default()
            .lines()
            .filter(|l| {
                l.split('\t')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|n| n <= count)
            })
            .map(|l| format!("{l}\n"))
            .collect();
        write_sidecar(&edits_path(chat), &body);
    }
}

/// Drop a chat's snapshots. Called when the chat itself is deleted: the store
/// holds a full copy of the user's files, so leaving it behind would keep a
/// deleted chat's contents on disk indefinitely.
pub fn purge(chat: &str) {
    let _ = std::fs::remove_dir_all(store(chat));
}

#[cfg(test)]
mod config_tests {
    use super::{config_quote, config_worktree};

    /// The reader only decides whether `ensure` may skip a `git config`
    /// spawn, so it must agree with both forms the file can hold: ours
    /// (always quoted) and git's own (bare, as older stores were written).
    #[test]
    fn the_worktree_reader_agrees_with_both_writers() {
        let ours = format!(
            "[core]\n\tbare = true\n[core]\n\tworktree = {}\n",
            config_quote("D:/a b/#x;y \"q\" \\z")
        );
        assert_eq!(
            config_worktree(&ours).as_deref(),
            Some("D:/a b/#x;y \"q\" \\z")
        );
        let gits = "[core]\n\tworktree = D:/work/proj ; trailing comment\n";
        assert_eq!(config_worktree(gits).as_deref(), Some("D:/work/proj"));
        // Last one wins, as it does for git; another section's key is not it.
        let two = "[core]\n\tworktree = /old\n[remote \"o\"]\n\tworktree = /no\n[core]\n\tworktree = /new\n";
        assert_eq!(config_worktree(two).as_deref(), Some("/new"));
        assert_eq!(config_worktree("[core]\n\tbare = false\n"), None);
        // Unreadable means "spawn git config", never a guessed value.
        assert_eq!(config_worktree("[core]\n\tworktree = \"/open\n"), None);
    }
}
