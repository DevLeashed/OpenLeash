//! Checkpoint bisect: "which agent edit broke it?"
//!
//! Every successful agent file edit is recorded as a commit of its own in the
//! chat's shadow store (`checkpoint::record_edit`). This module turns that log
//! into a numbered timeline of *steps* and binary-searches it with a test
//! command that exits 0 when the tree is good.
//!
//! # Isolation
//!
//! A state is never checked out over the user's files. Each probe writes the
//! tree into a fresh scratch directory under the system temp folder
//! (`checkpoint::materialize_scratch`, which reads only the shadow store), runs
//! the command there, and removes the directory again — on success, failure,
//! timeout, cancellation, and if the future is simply dropped ([`Scratch`] does
//! it in `Drop`). The user's checkout, its `.git` and the store's own index and
//! `HEAD` are not written to by a bisect.
//!
//! The snapshot holds only what `git add -A` would: ignored folders such as
//! `node_modules` are not in it. A caller can name such folders in `share`;
//! they are *linked* (symlink, or a junction on Windows) into the scratch copy,
//! read-through. That is a deliberate, opt-in hole in the isolation — a
//! command can write through the link — and the scratch cleanup unlinks them
//! before deleting anything, so cleanup never recurses into the user's folder.
//!
//! # Permissions
//!
//! The command is a shell command and is judged exactly as `bash` would judge
//! it: the runner's `bisect_edits` arm applies the same subagent limits and
//! calls the same `gate("bash", {"command": …})`, and the user action
//! (`task_bisect_run`) applies `permissions::check("bash", …)` and refuses on
//! any `Deny` (deny rules, plan mode, the Disabled rung). An `Ask` there is
//! answered by the user's own confirmation in the Review dialog, which shows the
//! exact command. See `reviewer_bisect` for the parity tests.
//!
//! # Steps
//!
//! A step is either one agent edit, or — when the files changed between two
//! recorded edits by something else (a shell command, the user, a rewind) — an
//! `external` step covering exactly that difference, so a break introduced by
//! `npm install` is not blamed on the next `edit_file`.

use super::checkpoint::{self, EditRecord};
use super::permissions::{self, Decision};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// The tool name of a step that is not an agent edit.
pub const EXTERNAL: &str = "external";

/// One step of the bisect timeline. `index` is 1-based; state `k` is the tree
/// *after* step `k`, and state `k - 1` the tree before it.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub index: usize,
    pub item_id: String,
    pub sub_id: Option<String>,
    pub msg_index: usize,
    pub tool: String,
    pub path: String,
    pub add: usize,
    pub del: usize,
    pub lines: Vec<Value>,
    #[serde(skip)]
    pub parent: String,
    #[serde(skip)]
    pub commit: String,
}

impl Step {
    fn edit(index: usize, e: &EditRecord) -> Self {
        Step {
            index,
            item_id: e.item_id.clone(),
            sub_id: e.sub_id.clone(),
            msg_index: e.msg_index,
            tool: e.tool.clone(),
            path: e.path.clone(),
            add: 0,
            del: 0,
            lines: vec![],
            parent: e.parent.clone(),
            commit: e.commit.clone(),
        }
    }
}

/// Summarise a list of paths for an `external` step's `path`.
fn summary(paths: &[String]) -> String {
    let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
    let more = paths.len().saturating_sub(shown.len());
    format!(
        "{} file{}: {}{}",
        paths.len(),
        if paths.len() == 1 { "" } else { "s" },
        shown.join(", "),
        if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        }
    )
}

/// The chat's bisect timeline, oldest first. `count` is the task's current
/// `checkpoints.len()`: edits recorded under a higher count belong to a branch
/// a rewind abandoned (`checkpoint::truncate` drops them too; this is the
/// belt to its braces). Cheap: no diffs are computed (see [`describe`]).
pub fn timeline(chat: &str, count: usize) -> Vec<Step> {
    let edits: Vec<EditRecord> = checkpoint::read_edits(chat)
        .into_iter()
        .filter(|e| e.count <= count)
        .collect();
    let mut steps: Vec<Step> = vec![];
    let mut prev: Option<String> = None;
    for e in &edits {
        if let Some(pc) = prev.as_deref() {
            if pc != e.parent && !checkpoint::same_tree(chat, pc, &e.parent) {
                let files: Vec<String> = checkpoint::changed(chat, pc, &e.parent)
                    .into_iter()
                    .map(|f| f.path)
                    .collect();
                if !files.is_empty() {
                    steps.push(Step {
                        index: steps.len() + 1,
                        item_id: String::new(),
                        sub_id: None,
                        msg_index: e.msg_index,
                        tool: EXTERNAL.into(),
                        path: summary(&files),
                        add: 0,
                        del: 0,
                        lines: vec![],
                        parent: pc.to_string(),
                        commit: e.parent.clone(),
                    });
                }
            }
        }
        steps.push(Step::edit(steps.len() + 1, e));
        prev = Some(e.commit.clone());
    }
    steps
}

/// Fill in a step's diff: the file's lines for an edit, the changed paths for
/// an external step.
pub fn describe(chat: &str, step: &Step) -> Step {
    let mut s = step.clone();
    if s.tool == EXTERNAL {
        let files = checkpoint::changed(chat, &s.parent, &s.commit);
        s.add = files.iter().map(|f| f.add).sum();
        s.del = files.iter().map(|f| f.del).sum();
        s.lines = files
            .iter()
            .take(200)
            .map(|f| json!({"k": "c", "t": format!("{} {}", f.status, f.path)}))
            .collect();
        return s;
    }
    let text = |c: &str| {
        checkpoint::file_at(chat, c, &s.path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    let (lines, add, del) = super::tools::diff_lines(&text(&s.parent), &text(&s.commit));
    s.lines = lines;
    s.add = add;
    s.del = del;
    s
}

// ───────────────────────────── the search ─────────────────────────────

/// What one probe of one state concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Exit 0 on every run.
    Good,
    /// Non-zero on every run.
    Bad,
    /// Killed at the timeout: says nothing about the state.
    Timeout,
    /// Passed on some runs and failed on others.
    Flaky,
    /// The state could not be tested at all (materialise failed, cancelled).
    Error,
}

/// Where the search ended. States are numbered as in [`Step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// State `k` is bad and state `k - 1` good: step `k` broke it.
    FirstBad(usize),
    /// The last state in range is good.
    AllGood,
    /// The state before the range is already bad.
    AlreadyBad,
    /// `good` passed and `bad` failed, but every state between could not be
    /// classified (timeout/flaky), so any step in `good+1..=bad` may be it.
    Inconclusive { good: usize, bad: usize },
    /// An endpoint of the range could not be classified.
    Undecided { state: usize, verdict: Verdict },
}

/// The bisect as a pure state machine: [`Bisector::next`] says which state to
/// test, [`Bisector::record`] takes the verdict. No I/O, so the algorithm is
/// tested without a shell (see the tests below).
///
/// The end of the range is probed first — "is it even broken?" is the cheap
/// question that most often ends a bisect — then the start, then binary
/// search. A timeout or flaky result marks a state as skipped (as in
/// `git bisect skip`) and the search picks the untested state nearest the
/// middle instead; if only skipped states remain between the last good and the
/// first bad, the answer is the whole remaining span, not a guess.
#[derive(Debug, Clone)]
pub struct Bisector {
    lo: usize,
    hi: usize,
    hi_seen: bool,
    lo_seen: bool,
    good: usize,
    bad: usize,
    skipped: BTreeSet<usize>,
    done: Option<Found>,
}

/// What to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Probe(usize),
    Done(Found),
}

impl Bisector {
    /// Search states `lo..=hi` (`lo < hi`); `lo` is the tree before the first
    /// step in range.
    pub fn new(lo: usize, hi: usize) -> Self {
        Bisector {
            lo,
            hi,
            hi_seen: false,
            lo_seen: false,
            good: lo,
            bad: hi,
            skipped: BTreeSet::new(),
            done: None,
        }
    }

    pub fn next(&self) -> Next {
        if let Some(d) = &self.done {
            return Next::Done(d.clone());
        }
        if !self.hi_seen {
            return Next::Probe(self.hi);
        }
        if !self.lo_seen {
            return Next::Probe(self.lo);
        }
        let mid = (self.good + self.bad) / 2;
        let pick = (self.good + 1..self.bad)
            .filter(|s| !self.skipped.contains(s))
            .min_by_key(|s| (s.abs_diff(mid), *s));
        match pick {
            Some(s) => Next::Probe(s),
            None if self.bad == self.good + 1 => Next::Done(Found::FirstBad(self.bad)),
            None => Next::Done(Found::Inconclusive {
                good: self.good,
                bad: self.bad,
            }),
        }
    }

    pub fn record(&mut self, state: usize, verdict: Verdict) {
        if self.done.is_some() {
            return;
        }
        if !self.hi_seen && state == self.hi {
            self.hi_seen = true;
            match verdict {
                Verdict::Good => self.done = Some(Found::AllGood),
                Verdict::Bad => {}
                v => self.done = Some(Found::Undecided { state, verdict: v }),
            }
            return;
        }
        if !self.lo_seen && state == self.lo {
            self.lo_seen = true;
            match verdict {
                Verdict::Good => {}
                Verdict::Bad => self.done = Some(Found::AlreadyBad),
                v => self.done = Some(Found::Undecided { state, verdict: v }),
            }
            return;
        }
        if state <= self.good || state >= self.bad {
            return;
        }
        match verdict {
            Verdict::Good => self.good = state,
            Verdict::Bad => self.bad = state,
            _ => {
                self.skipped.insert(state);
            }
        }
    }
}

// ───────────────────────────── probing ─────────────────────────────

/// One probe's record, as reported and streamed.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub state: usize,
    pub verdict: Verdict,
    pub code: Option<i32>,
    pub ms: u64,
    pub output: String,
}

/// The scratch root every bisect writes under.
pub fn scratch_root() -> PathBuf {
    std::env::temp_dir().join("openleash-bisect")
}

/// A scratch copy that removes itself. Links are removed first and on their
/// own, so the recursive delete never walks into a shared folder of the
/// user's; if a link cannot be removed the directory is left behind rather
/// than risk it.
pub struct Scratch {
    pub dir: PathBuf,
    links: Vec<PathBuf>,
}

impl Scratch {
    fn create() -> Result<Self, String> {
        let dir = scratch_root().join(super::new_id());
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create a scratch folder: {e}"))?;
        Ok(Scratch { dir, links: vec![] })
    }

    /// Link `rel` (a folder of the user's worktree) into the scratch copy.
    fn share(&mut self, worktree: &str, rel: &str) -> Result<(), String> {
        let rel = rel.trim().trim_matches(['/', '\\']).replace('\\', "/");
        let bad = rel.is_empty()
            || rel.contains(':')
            || rel
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..");
        if bad {
            return Err(format!("`{rel}` is not a folder inside the project."));
        }
        let root = Path::new(worktree)
            .canonicalize()
            .map_err(|e| format!("cannot resolve the project folder: {e}"))?;
        let src = root
            .join(&rel)
            .canonicalize()
            .map_err(|_| format!("shared folder `{rel}` does not exist"))?;
        if !src.starts_with(&root) || src == root || !src.is_dir() {
            return Err(format!("`{rel}` is not a folder inside the project."));
        }
        let dst = self.dir.join(&rel);
        if dst.symlink_metadata().is_ok() {
            return Err(format!(
                "`{rel}` is part of the snapshot itself; only ignored folders can be shared."
            ));
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        link_dir(&src, &dst).map_err(|e| format!("cannot link `{rel}`: {e}"))?;
        self.links.push(dst);
        Ok(())
    }
}

#[cfg(unix)]
fn link_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(src, dst).map_err(|e| e.to_string())
}

/// A junction: unlike a directory symlink it needs neither admin rights nor
/// developer mode. `raw_arg` because `cmd /C` does its own quote parsing, and
/// a Windows path cannot contain `"`.
#[cfg(windows)]
fn link_dir(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let (s, d) = (src.to_string_lossy(), dst.to_string_lossy());
    if s.contains('"') || d.contains('"') {
        return Err("path contains a quote".into());
    }
    let out = std::process::Command::new("cmd")
        .raw_arg(format!("/C mklink /J \"{d}\" \"{s}\""))
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Remove a link without touching its target.
fn unlink(p: &Path) -> bool {
    #[cfg(windows)]
    let r = std::fs::remove_dir(p).or_else(|_| std::fs::remove_file(p));
    #[cfg(not(windows))]
    let r = std::fs::remove_file(p);
    r.is_ok() || p.symlink_metadata().is_err()
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let all_unlinked = self.links.iter().all(|l| unlink(l));
        if all_unlinked {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// What a bisect was asked to do.
#[derive(Debug, Clone)]
pub struct Request {
    pub command: String,
    pub from: Option<usize>,
    pub to: Option<usize>,
    pub timeout_ms: u64,
    pub runs: u32,
    pub share: Vec<String>,
}

impl Request {
    pub fn new(
        command: String,
        from: Option<usize>,
        to: Option<usize>,
        timeout_ms: Option<u64>,
        runs: Option<u32>,
        share: Option<Vec<String>>,
    ) -> Self {
        Request {
            command,
            from,
            to,
            timeout_ms: timeout_ms.unwrap_or(120_000).clamp(1_000, 600_000),
            runs: runs.unwrap_or(1).clamp(1, 3),
            share: share.unwrap_or_default(),
        }
    }
}

fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let t: String = s.chars().skip(n - max).collect();
    format!("…{t}")
}

/// Materialise `commit` into a fresh scratch copy and run the command there,
/// `runs` times. The scratch copy is gone when this returns.
pub async fn probe(
    chat: &str,
    worktree: &str,
    commit: &str,
    state: usize,
    req: &Request,
    cancel: &CancellationToken,
) -> Probe {
    let started = std::time::Instant::now();
    let fail = |msg: String, started: std::time::Instant| Probe {
        state,
        verdict: Verdict::Error,
        code: None,
        ms: started.elapsed().as_millis() as u64,
        output: msg,
    };
    let scratch = {
        let (c, cm, w, share) = (
            chat.to_string(),
            commit.to_string(),
            worktree.to_string(),
            req.share.clone(),
        );
        let made = tokio::task::spawn_blocking(move || -> Result<Scratch, String> {
            let mut s = Scratch::create()?;
            checkpoint::materialize_scratch(&c, &cm, &s.dir.to_string_lossy())?;
            for rel in &share {
                s.share(&w, rel)?;
            }
            Ok(s)
        })
        .await;
        match made {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return fail(format!("Could not prepare this state: {e}"), started),
            Err(e) => return fail(e.to_string(), started),
        }
    };
    let dir = scratch.dir.to_string_lossy().to_string();
    let (mut passes, mut fails) = (0u32, 0u32);
    let mut last_code = None;
    let mut output = String::new();
    for _ in 0..req.runs {
        if cancel.is_cancelled() {
            return fail("Cancelled.".into(), started);
        }
        let r = match super::shell::run(&req.command, &dir, req.timeout_ms, cancel).await {
            Ok(r) => r,
            Err(e) => return fail(e, started),
        };
        output = r.output;
        last_code = r.code;
        if r.interrupted {
            return fail("Cancelled.".into(), started);
        }
        if r.timed_out {
            return Probe {
                state,
                verdict: Verdict::Timeout,
                code: None,
                ms: started.elapsed().as_millis() as u64,
                output: tail(&output, 1500),
            };
        }
        if r.code == Some(0) {
            passes += 1;
        } else {
            fails += 1;
        }
    }
    drop(scratch);
    Probe {
        state,
        verdict: match (passes, fails) {
            (_, 0) => Verdict::Good,
            (0, _) => Verdict::Bad,
            _ => Verdict::Flaky,
        },
        code: last_code,
        ms: started.elapsed().as_millis() as u64,
        output: tail(&output, 1500),
    }
}

/// The finished bisect.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `found` | `all_good` | `already_bad` | `inconclusive` | `cancelled`.
    pub outcome: String,
    pub culprit: Option<Step>,
    pub candidates: Vec<Step>,
    pub probes: Vec<Probe>,
    pub from: usize,
    pub to: usize,
    pub total: usize,
    pub message: String,
}

/// Run a whole bisect over `steps`, emitting progress through `emit`. The
/// caller has already checked the command's permission.
pub async fn run(
    chat: &str,
    worktree: &str,
    steps: &[Step],
    req: &Request,
    cancel: &CancellationToken,
    emit: &(dyn Fn(Value) + Send + Sync),
) -> Result<Report, String> {
    let total = steps.len();
    if total == 0 {
        return Err("This chat has no recorded file edits to bisect.".into());
    }
    let from = req.from.unwrap_or(1);
    let to = req.to.unwrap_or(total);
    if from == 0 || from > to || to > total {
        return Err(format!(
            "Invalid range {from}..{to}: steps are numbered 1 to {total}."
        ));
    }
    if req.command.trim().is_empty() {
        return Err("A test command is required.".into());
    }
    emit(json!({"phase": "start", "from": from, "to": to, "total": total}));
    let commit_of = |state: usize| -> &str {
        if state == from - 1 {
            &steps[from - 1].parent
        } else {
            &steps[state - 1].commit
        }
    };
    let mut b = Bisector::new(from - 1, to);
    let mut probes: Vec<Probe> = vec![];
    let mut cache: HashMap<usize, Verdict> = HashMap::new();
    let found = loop {
        if cancel.is_cancelled() {
            break None;
        }
        match b.next() {
            Next::Done(f) => break Some(f),
            Next::Probe(state) => {
                if let Some(v) = cache.get(&state) {
                    b.record(state, *v);
                    continue;
                }
                emit(json!({"phase": "probe", "state": state, "step": probes.len() + 1}));
                let p = probe(chat, worktree, commit_of(state), state, req, cancel).await;
                if cancel.is_cancelled() {
                    break None;
                }
                emit(json!({"phase": "probed", "probe": p}));
                cache.insert(state, p.verdict);
                b.record(state, p.verdict);
                probes.push(p);
            }
        }
    };
    let mut report = Report {
        outcome: String::new(),
        culprit: None,
        candidates: vec![],
        probes,
        from,
        to,
        total,
        message: String::new(),
    };
    let label = |s: &Step| {
        if s.tool == EXTERNAL {
            format!(
                "step {} (changes outside the agent's edits: {})",
                s.index, s.path
            )
        } else {
            format!("step {} ({} {})", s.index, s.tool, s.path)
        }
    };
    match found {
        None => {
            report.outcome = "cancelled".into();
            report.message = "The bisect was cancelled.".into();
        }
        Some(Found::FirstBad(k)) => {
            let s = describe(chat, &steps[k - 1]);
            report.outcome = "found".into();
            report.message = format!(
                "The command passes before {} and fails after it: that is the first bad step.",
                label(&s)
            );
            report.culprit = Some(s);
        }
        Some(Found::AllGood) => {
            report.outcome = "all_good".into();
            report.message =
                format!("The command passes after step {to}, so no step in {from}..{to} broke it.");
        }
        Some(Found::AlreadyBad) => {
            report.outcome = "already_bad".into();
            report.message = format!(
                "The command already fails before step {from}. Scratch copies hold only files the snapshot tracks — ignored folders such as node_modules are missing unless shared."
            );
        }
        Some(Found::Inconclusive { good, bad }) => {
            report.outcome = "inconclusive".into();
            report.candidates = steps[good..bad].iter().map(|s| describe(chat, s)).collect();
            report.message = format!(
                "Passes after step {good} and fails after step {bad}, but the states between timed out or were flaky; the culprit is one of steps {}..{bad}.",
                good + 1
            );
        }
        Some(Found::Undecided { state, verdict }) => {
            report.outcome = "inconclusive".into();
            report.message = format!(
                "Could not classify the state {} step {}: the command {}.",
                if state == from - 1 { "before" } else { "after" },
                if state == from - 1 { from } else { state },
                match verdict {
                    Verdict::Timeout => "timed out",
                    Verdict::Flaky => "was flaky",
                    _ => "could not run",
                }
            );
        }
    }
    emit(json!({"phase": "done", "report": report}));
    Ok(report)
}

// ───────────────────────────── shared policy ─────────────────────────────

/// Why a chat cannot be bisected at all, or `None` if it can. Checkpoints must
/// be on and local; a docker workspace runs `bash` in its container, and
/// running the same command on the host instead would be a different
/// security boundary than the one `bash` enforces, so it is refused.
pub fn unavailable(checkpoints_on: bool, cwd: &str, docker: bool) -> Option<&'static str> {
    if super::host::is_ssh(cwd) {
        Some("Bisect needs local checkpoints; this chat runs over SSH.")
    } else if docker {
        Some("Bisect runs commands on this machine, but this chat's commands run in a docker container.")
    } else if !checkpoints_on {
        Some("Checkpoints are off (Settings), so no edit history was recorded.")
    } else {
        None
    }
}

/// The user action's permission check: the same `permissions::check` the
/// agent's `bash` goes through, with the same input shape. Any `Deny` —
/// a deny rule, plan mode, the Disabled rung, the env-file guard — refuses.
/// An `Ask` is satisfied by the user's own confirmation of this exact command.
pub fn check_user_command(cmd: &str, ctx: &permissions::Ctx) -> Result<(), String> {
    match permissions::check("bash", &json!({"command": cmd}), None, ctx) {
        Decision::Deny(reason) => Err(reason),
        Decision::Allow | Decision::Ask { .. } => Ok(()),
    }
}

/// One bisect per chat at a time, and the handle the Cancel button uses.
static ACTIVE: Mutex<Option<HashMap<String, CancellationToken>>> = Mutex::new(None);

/// Holds a chat's slot in the registry; releases it on drop.
pub struct Active {
    task_id: String,
    pub token: CancellationToken,
}

impl Drop for Active {
    fn drop(&mut self) {
        let mut g = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_mut() {
            m.remove(&self.task_id);
        }
    }
}

/// Claim the chat's bisect slot. `token` should be a child of whatever already
/// cancels the caller (the run's token for the agent tool).
pub fn begin(task_id: &str, token: CancellationToken) -> Result<Active, String> {
    let mut g = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.contains_key(task_id) {
        return Err("A bisect is already running in this chat.".into());
    }
    m.insert(task_id.to_string(), token.clone());
    Ok(Active {
        task_id: task_id.to_string(),
        token,
    })
}

/// Cancel the chat's running bisect. `false` when there is none.
pub fn cancel(task_id: &str) -> bool {
    let g = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref().and_then(|m| m.get(task_id)) {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
    }
}

/// A plain-text report for the agent tool.
pub fn report_text(r: &Report) -> String {
    let mut s = format!("{}\n", r.message);
    if let Some(c) = &r.culprit {
        s.push_str(&format!(
            "\nCulprit: step {} — {} {} (+{} −{}){}\n",
            c.index,
            c.tool,
            c.path,
            c.add,
            c.del,
            match &c.sub_id {
                Some(sub) => format!(", made by subagent {sub}"),
                None => String::new(),
            }
        ));
        for l in c.lines.iter().take(120) {
            let k = l["k"].as_str().unwrap_or("c");
            let t = l["t"].as_str().unwrap_or("");
            let sign = match k {
                "a" => "+",
                "d" => "-",
                "h" => "",
                _ => " ",
            };
            s.push_str(&format!("{sign}{t}\n"));
        }
    }
    if !r.candidates.is_empty() {
        s.push_str("\nCandidates:\n");
        for c in &r.candidates {
            s.push_str(&format!("- step {}: {} {}\n", c.index, c.tool, c.path));
        }
    }
    s.push_str(&format!("\nProbes ({}):\n", r.probes.len()));
    for p in &r.probes {
        s.push_str(&format!(
            "- after step {}: {:?} (exit {}, {} ms)\n",
            p.state,
            p.verdict,
            p.code.map(|c| c.to_string()).unwrap_or("-".into()),
            p.ms
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the state machine with a fixed truth table: `bad_from` is the
    /// first bad state, `skip` the states that cannot be classified.
    fn drive(lo: usize, hi: usize, verdict: impl Fn(usize) -> Verdict) -> (Found, Vec<usize>) {
        let mut b = Bisector::new(lo, hi);
        let mut probed = vec![];
        loop {
            match b.next() {
                Next::Done(f) => return (f, probed),
                Next::Probe(s) => {
                    assert!(probed.len() < 100, "runaway search");
                    probed.push(s);
                    b.record(s, verdict(s));
                }
            }
        }
    }

    fn line(bad_from: usize) -> impl Fn(usize) -> Verdict {
        move |s| {
            if s >= bad_from {
                Verdict::Bad
            } else {
                Verdict::Good
            }
        }
    }

    #[test]
    fn finds_the_first_bad_step_for_every_position() {
        for n in 1..=40usize {
            for bad_from in 1..=n {
                let (found, probed) = drive(0, n, line(bad_from));
                assert_eq!(
                    found,
                    Found::FirstBad(bad_from),
                    "n={n} bad_from={bad_from}"
                );
                // two endpoints + ceil(log2(n)) at most
                let bound = 2 + (usize::BITS - n.leading_zeros()) as usize;
                assert!(probed.len() <= bound, "n={n}: {} probes", probed.len());
            }
        }
    }

    #[test]
    fn a_subrange_is_searched_within_its_bounds() {
        let (found, probed) = drive(4, 9, line(7));
        assert_eq!(found, Found::FirstBad(7));
        assert!(probed.iter().all(|s| (4..=9).contains(s)));
    }

    #[test]
    fn all_good_stops_after_one_probe() {
        let (found, probed) = drive(0, 10, |_| Verdict::Good);
        assert_eq!(found, Found::AllGood);
        assert_eq!(probed, vec![10]);
    }

    #[test]
    fn all_bad_reports_already_broken() {
        let (found, probed) = drive(0, 10, |_| Verdict::Bad);
        assert_eq!(found, Found::AlreadyBad);
        assert_eq!(probed, vec![10, 0]);
    }

    #[test]
    fn timeouts_and_flaky_states_are_skipped_not_guessed() {
        // bad from 6; state 5 times out, so 5 and 6 cannot be told apart.
        let (found, _) = drive(0, 10, |s| match s {
            5 => Verdict::Timeout,
            s if s >= 6 => Verdict::Bad,
            _ => Verdict::Good,
        });
        assert_eq!(found, Found::Inconclusive { good: 4, bad: 6 });
        // A skipped state that does not sit on the boundary costs nothing.
        let (found, _) = drive(0, 10, |s| match s {
            5 => Verdict::Flaky,
            s if s >= 8 => Verdict::Bad,
            _ => Verdict::Good,
        });
        assert_eq!(found, Found::FirstBad(8));
        // Everything in between unclassifiable: the whole span is the answer.
        let (found, _) = drive(0, 4, |s| match s {
            0 => Verdict::Good,
            4 => Verdict::Bad,
            _ => Verdict::Error,
        });
        assert_eq!(found, Found::Inconclusive { good: 0, bad: 4 });
    }

    #[test]
    fn an_unclassifiable_endpoint_ends_the_search() {
        let (found, probed) = drive(0, 10, |s| {
            if s == 10 {
                Verdict::Timeout
            } else {
                Verdict::Good
            }
        });
        assert_eq!(
            found,
            Found::Undecided {
                state: 10,
                verdict: Verdict::Timeout
            }
        );
        assert_eq!(probed, vec![10]);
        let (found, _) = drive(
            0,
            10,
            |s| if s == 0 { Verdict::Flaky } else { Verdict::Bad },
        );
        assert_eq!(
            found,
            Found::Undecided {
                state: 0,
                verdict: Verdict::Flaky
            }
        );
    }

    #[test]
    fn request_clamps_its_knobs() {
        let r = Request::new("x".into(), None, None, Some(1), Some(99), None);
        assert_eq!(r.timeout_ms, 1_000);
        assert_eq!(r.runs, 3);
        let r = Request::new("x".into(), None, None, None, Some(0), None);
        assert_eq!((r.timeout_ms, r.runs), (120_000, 1));
    }

    #[test]
    fn one_bisect_per_chat_and_cancel_reaches_it() {
        let id = format!("bisect-slot-{}", super::super::new_id());
        let a = begin(&id, CancellationToken::new()).unwrap();
        assert!(begin(&id, CancellationToken::new()).is_err());
        assert!(cancel(&id));
        assert!(a.token.is_cancelled());
        drop(a);
        assert!(!cancel(&id));
        assert!(begin(&id, CancellationToken::new()).is_ok());
    }
}
