//! Folder trust: whether the project a chat runs in may inject anything into
//! the agent's context.
//!
//! `docs/security-model.md` used to describe instruction injection as a
//! disclosure — "cloning an untrusted repository is enough to plant
//! instructions in the agent you then point at it" — with plan mode as the only
//! mitigation. This module turns that disclosure into a control: every folder
//! is untrusted until the user says otherwise, and an untrusted folder
//! contributes nothing to the prompt — no instructions, no skills, no project
//! agents, no project hooks and no project MCP config.
//!
//! The decision is per folder and re-decidable. "Trust parent folder" is
//! recorded as its own *kind* of decision so a whole tree (`~/code/…`) can be
//! trusted at once and a single checkout inside it can still be untrusted
//! afterwards: resolution walks up from the folder and the nearest decision
//! wins, with an exact `Folder` decision outranking a `Parent` one recorded on
//! the same path.
//!
//! Fail closed is the whole point. There is deliberately no "grandfather the
//! projects already in `settings.projects`" migration: the attack this guards
//! against is "clone a repo, open it", and a user who had already opened a
//! hostile repo before upgrading is exactly the user a silent migration would
//! fail. Existing projects therefore start untrusted, and the UI asks on open.

use super::checks::Hook;
use super::prompt::{self, MEMORY_FILES};
use super::store;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// `folder` = this exact directory only. `parent` = this directory and every
/// directory beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustKind {
    Folder,
    Parent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustState {
    Trusted,
    Untrusted,
}

/// One recorded decision. `path` is stored normalized (see [`norm_path`]) so a
/// path typed one way and picked another still compares equal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustDecision {
    pub path: String,
    pub kind: TrustKind,
    pub decision: TrustState,
    pub decided_at: DateTime<Utc>,
}

impl Default for TrustDecision {
    fn default() -> Self {
        Self {
            path: String::new(),
            kind: TrustKind::Folder,
            decision: TrustState::Untrusted,
            decided_at: Utc::now(),
        }
    }
}

/// Normalize a folder path for comparison: forward slashes, no doubled or
/// trailing separators, and case-folded on Windows (where `C:\Foo` and
/// `c:/foo` are the same directory). Deliberately lexical rather than
/// `canonicalize`d: the folder may not exist yet when a decision is recorded,
/// and following a symlink would silently move a decision onto its target.
pub fn norm_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut prev = false;
    for c in p.trim().chars() {
        let c = if c == '\\' { '/' } else { c };
        if c == '/' {
            if !prev {
                out.push('/');
            }
            prev = true;
        } else {
            out.push(c);
            prev = false;
        }
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    if cfg!(windows) {
        out = out.to_lowercase();
    }
    out
}

/// The next directory up, or `None` at the top. `c:/a/b` -> `c:/a` -> `c:`.
fn parent_of(p: &str) -> Option<String> {
    let i = p.rfind('/')?;
    if i == 0 {
        // A leading `/` is the filesystem root, which has no parent.
        return None;
    }
    Some(p[..i].to_string())
}

/// The parent of a folder, normalized, for the "trust the parent folder" choice
/// — which records a decision on the *parent*, so a later "don't trust" on the
/// folder itself is still possible. `None` for a path with no parent (a drive
/// root), where the dialog must fall back to trusting the folder alone.
pub fn parent_dir(path: &str) -> Option<String> {
    let p = norm_path(path);
    let up = parent_of(&p)?;
    // A bare drive (`c:`) or filesystem root is not a folder to trust.
    if up.ends_with(':') || up.is_empty() || up == "/" {
        return None;
    }
    Some(up)
}

/// The decision that governs `path`: the nearest one on the walk up the tree.
/// An exact `Folder` decision is checked only against `path` itself — `Folder`
/// means "this directory, not its children" — while a `Parent` decision covers
/// the folder and everything under it. `None` = never decided, which callers
/// must read as untrusted.
pub fn resolve<'a>(decisions: &'a [TrustDecision], path: &str) -> Option<&'a TrustDecision> {
    let want = norm_path(path);
    let mut cur = want.clone();
    // Bounded so a malformed path cannot spin this loop; real trees are far
    // shallower than this.
    for _ in 0..256 {
        if cur == want {
            if let Some(d) = decisions
                .iter()
                .find(|d| d.kind == TrustKind::Folder && norm_path(&d.path) == cur)
            {
                return Some(d);
            }
        }
        if let Some(d) = decisions
            .iter()
            .find(|d| d.kind == TrustKind::Parent && norm_path(&d.path) == cur)
        {
            return Some(d);
        }
        let next = parent_of(&cur)?;
        if next == cur {
            return None;
        }
        cur = next;
    }
    None
}

/// Whether `path` may inject into the prompt. Fail closed: no decision, or a
/// decision that is not `Trusted`, means no.
pub fn trusted(decisions: &[TrustDecision], path: &str) -> bool {
    resolve(decisions, path).is_some_and(|d| d.decision == TrustState::Trusted)
}

/// Whether a decision exists at all (either state), for the UI to tell "asked
/// and refused" from "never asked".
pub fn decided(decisions: &[TrustDecision], path: &str) -> bool {
    resolve(decisions, path).is_some()
}

/// Record a decision, replacing any earlier one for the same folder and kind.
/// A `Folder` and a `Parent` decision for one path coexist on purpose: a later
/// "don't trust this one checkout" is a folder decision under a parent trust.
pub fn upsert(decisions: &mut Vec<TrustDecision>, path: &str, kind: TrustKind, state: TrustState) {
    let n = norm_path(path);
    decisions.retain(|d| !(d.kind == kind && norm_path(&d.path) == n));
    decisions.push(TrustDecision {
        path: n,
        kind,
        decision: state,
        decided_at: Utc::now(),
    });
}

/// Forget every decision recorded for a folder (both kinds).
pub fn clear(decisions: &mut Vec<TrustDecision>, path: &str) {
    let n = norm_path(path);
    decisions.retain(|d| norm_path(&d.path) != n);
}

// ───────────────────── process-global view ─────────────────────
//
// `prompt::system` builds a sub-agent's prompt from a `prompt::Env` that carries
// no settings, and `runner.rs` (which owns that call) is not ours to change, so
// the trust decision the *prompt* needs cannot be threaded through as an
// argument. It is kept here as one process-global table instead, written
// wherever settings are (app boot, `settings_update`, and the trust commands)
// and read by `prompt` when it decides whether to inject project instructions.
//
// This mirrors `router::ROUTES`, which is global for the same structural reason:
// the frozen-prefix builder has the model string but not the settings. As there,
// it is a cache of a value that lives on disk, not a second source of truth —
// `Settings::trust` is the record, this is the view the prompt path can reach.
static TRUST: std::sync::RwLock<Vec<TrustDecision>> = std::sync::RwLock::new(Vec::new());

/// Tests that drive the global take turns, from any module — same reason as
/// `router::ROUTES_LOCK`.
#[cfg(test)]
pub static TRUST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Publish the settings' decisions to the prompt path. Called on boot and on
/// every settings write that can change trust.
pub fn set_trust(decisions: &[TrustDecision]) {
    if let Ok(mut g) = TRUST.write() {
        *g = decisions.to_vec();
    }
}

/// A snapshot of the published decisions.
pub fn current() -> Vec<TrustDecision> {
    TRUST.read().map(|g| g.clone()).unwrap_or_default()
}

/// Whether `path` is trusted according to the published decisions. Fail closed:
/// an unpublished or empty table means nothing is trusted yet.
pub fn trusted_now(path: &str) -> bool {
    TRUST.read().map(|g| trusted(&g, path)).unwrap_or(false)
}

// ───────────────────────────── manifest ─────────────────────────────

/// One file that would be injected into the system prompt.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ManifestFile {
    /// As the prompt names it (`AGENTS.md`, or the `~/.openleash/…` marker).
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub chars: usize,
    /// Over the 40,000-char cap the loader applies, so only a prefix is read.
    pub truncated: bool,
    /// A `CLAUDE.md` that only points at `AGENTS.md`; it is skipped anyway.
    pub pointer_only: bool,
    /// True for the user's own `~/.…` files, which are loaded even when the
    /// project folder is untrusted.
    pub global: bool,
    /// First few hundred characters, so the dialog can show the actual text.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestSkill {
    pub name: String,
    pub description: String,
    pub path: String,
    pub files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestAgent {
    pub id: String,
    pub name: String,
    pub description: String,
    /// all | read_only | no_shell
    pub tools: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestHook {
    pub event: String,
    pub matcher: String,
    pub command: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestMcp {
    pub name: String,
    pub command: String,
    pub transport: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestOverride {
    pub file: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Warning {
    /// danger (runs code / auto-approves) | warn | info
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Matched {
    pub kind: TrustKind,
    pub path: String,
    pub decision: TrustState,
}

/// Everything a folder would add, computed whether or not it is trusted so the
/// user can see what they are deciding about.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Manifest {
    pub path: String,
    pub trusted: bool,
    pub decided: bool,
    pub matched: Option<Matched>,
    /// The folder one level up, for the "trust the parent folder" choice. Empty
    /// when there is none (a drive root).
    pub parent_dir: String,
    /// Trusted *and* carrying something project-local that will be loaded.
    pub injecting: bool,
    pub instructions: Vec<ManifestFile>,
    pub skills: Vec<ManifestSkill>,
    pub agents: Vec<ManifestAgent>,
    pub hooks: Vec<ManifestHook>,
    pub mcp: Vec<ManifestMcp>,
    pub overrides: Vec<ManifestOverride>,
    pub warnings: Vec<Warning>,
}

impl Manifest {
    /// Anything project-local at all, globals excluded.
    pub fn has_project_items(&self) -> bool {
        self.instructions.iter().any(|f| !f.global)
            || !self.skills.is_empty()
            || !self.agents.is_empty()
            || !self.hooks.is_empty()
            || !self.mcp.is_empty()
            || !self.overrides.is_empty()
    }
}

const PREVIEW_CHARS: usize = 700;
const MANIFEST_CAP: usize = 200;

fn push_file(out: &mut Vec<ManifestFile>, path: &Path, name: &str, global: bool, raw: &str) {
    let chars = raw.chars().count();
    let preview: String = raw.chars().take(PREVIEW_CHARS).collect();
    out.push(ManifestFile {
        name: name.to_string(),
        path: path.to_string_lossy().to_string(),
        bytes: std::fs::metadata(path)
            .map(|m| m.len())
            .unwrap_or(chars as u64),
        chars,
        truncated: chars > 40_000,
        pointer_only: name == "CLAUDE.md" && prompt::is_agents_pointer_only(raw),
        global,
        preview,
    });
}

/// Read a project JSON file, returning `None` when it is missing or malformed
/// (a malformed one is still worth a warning, which the callers add).
fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The hooks a project declares in `.openleash/hooks.json`. OpenLeash does not
/// execute these today — `runner.rs` reads the global `Settings::hooks` only —
/// so this is what the trust manifest shows, and the one function any future
/// project-hook wiring must go through so the trust gate is never skipped.
pub fn project_hooks(root: &str) -> Vec<Hook> {
    let Some(v) = read_json(&Path::new(root).join(".openleash").join("hooks.json")) else {
        return vec![];
    };
    serde_json::from_value::<Vec<Hook>>(v)
        .unwrap_or_default()
        .into_iter()
        .filter(|h| !h.command.trim().is_empty())
        .collect()
}

/// The project's hooks if and only if the folder is trusted. The single door
/// for project hooks, whatever calls it.
#[cfg(test)]
pub fn hooks_for(s: &store::Settings, root: &str) -> Vec<Hook> {
    if trusted(&s.trust, root) {
        project_hooks(root)
    } else {
        vec![]
    }
}

fn scan_hooks(root: &str, m: &mut Manifest) {
    for h in project_hooks(root) {
        m.hooks.push(ManifestHook {
            event: h.event,
            matcher: h.matcher,
            command: h.command,
            source: ".openleash/hooks.json".into(),
        });
    }
    // Claude Code keeps hooks nested inside its settings file; we do not run
    // them, but a user deciding about a repo deserves to see that one is there.
    let p = Path::new(root).join(".claude").join("settings.json");
    if let Some(v) = read_json(&p) {
        if let Some(obj) = v.get("hooks").and_then(|h| h.as_object()) {
            for (event, groups) in obj {
                let n = groups.as_array().map(|a| a.len()).unwrap_or(0);
                for _ in 0..n {
                    m.hooks.push(ManifestHook {
                        event: event.clone(),
                        matcher: String::new(),
                        command: "(declared in .claude/settings.json)".into(),
                        source: ".claude/settings.json".into(),
                    });
                }
            }
        }
    }
}

fn scan_mcp(root: &str, m: &mut Manifest) {
    for file in [".mcp.json", ".openleash/mcp.json"] {
        let Some(v) = read_json(&Path::new(root).join(file)) else {
            continue;
        };
        let servers = v.get("mcpServers").cloned().unwrap_or(v);
        if let Some(obj) = servers.as_object() {
            for (name, cfg) in obj {
                let cmd = cfg.get("command").and_then(|x| x.as_str()).unwrap_or("");
                let url = cfg.get("url").and_then(|x| x.as_str()).unwrap_or("");
                let http = !url.is_empty()
                    || cfg.get("transport").and_then(|x| x.as_str()) == Some("http");
                m.mcp.push(ManifestMcp {
                    name: name.clone(),
                    command: if !cmd.is_empty() {
                        cmd.into()
                    } else {
                        url.into()
                    },
                    transport: if http { "http".into() } else { "stdio".into() },
                    source: file.into(),
                });
            }
        } else if let Some(arr) = servers.as_array() {
            for cfg in arr {
                let name = cfg.get("name").and_then(|x| x.as_str()).unwrap_or("server");
                let cmd = cfg.get("command").and_then(|x| x.as_str()).unwrap_or("");
                let url = cfg.get("url").and_then(|x| x.as_str()).unwrap_or("");
                m.mcp.push(ManifestMcp {
                    name: name.into(),
                    command: if !cmd.is_empty() {
                        cmd.into()
                    } else {
                        url.into()
                    },
                    transport: if !url.is_empty() {
                        "http".into()
                    } else {
                        "stdio".into()
                    },
                    source: file.into(),
                });
            }
        }
    }
}

/// Setting keys that would loosen the permission gate, or otherwise change what
/// the harness does, if this project's config were honoured.
const DANGEROUS_KEYS: &[&str] = &[
    "permissions",
    "allowedTools",
    "autoApprove",
    "defaultMode",
    "enableAllProjectMcpServers",
    "bypassPermissions",
];

fn scan_overrides(root: &str, m: &mut Manifest) {
    for file in [".claude/settings.json", ".openleash/settings.json"] {
        let Some(v) = read_json(&Path::new(root).join(file)) else {
            continue;
        };
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if !DANGEROUS_KEYS.contains(&k.as_str()) {
                    continue;
                }
                let text = serde_json::to_string(val).unwrap_or_default();
                m.overrides.push(ManifestOverride {
                    file: file.into(),
                    key: k.clone(),
                    value: text.chars().take(120).collect(),
                });
                m.warnings.push(Warning {
                    level: "danger".into(),
                    text: format!(
                        "`{k}` in {file} would change how the agent is allowed to act in this folder."
                    ),
                });
            }
        }
    }
}

/// Build the manifest for one folder. `s` supplies the trust decisions and the
/// `inject_global_claude` toggle; everything else is read from disk.
pub fn manifest(s: &store::Settings, root: &str) -> Manifest {
    let matched = resolve(&s.trust, root).cloned();
    let trusted = matched
        .as_ref()
        .is_some_and(|d| d.decision == TrustState::Trusted);
    let mut m = Manifest {
        path: norm_path(root),
        trusted,
        decided: matched.is_some(),
        matched: matched.map(|d| Matched {
            kind: d.kind,
            path: d.path,
            decision: d.decision,
        }),
        parent_dir: parent_dir(root).unwrap_or_default(),
        ..Default::default()
    };

    for f in MEMORY_FILES {
        let p = Path::new(root).join(f);
        if let Ok(text) = std::fs::read_to_string(&p) {
            push_file(&mut m.instructions, &p, f, false, &text);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let g = home.join(".openleash").join("OPENLEASH.md");
        if let Ok(text) = std::fs::read_to_string(&g) {
            push_file(
                &mut m.instructions,
                &g,
                "~/.openleash/OPENLEASH.md (user-global)",
                true,
                &text,
            );
        }
        if s.inject_global_claude {
            let c = home.join(".claude").join("CLAUDE.md");
            if let Ok(text) = std::fs::read_to_string(&c) {
                if !text.trim().is_empty() {
                    push_file(
                        &mut m.instructions,
                        &c,
                        "~/.claude/CLAUDE.md (user-global)",
                        true,
                        &text,
                    );
                }
            }
        }
    }

    if !root.is_empty() && Path::new(root).is_dir() {
        for sk in store::read_project_skills(root)
            .into_iter()
            .take(MANIFEST_CAP)
        {
            m.skills.push(ManifestSkill {
                name: sk.name,
                description: sk.description,
                path: sk.path,
                files: sk.files,
            });
        }
        for (a, p) in store::read_project_agents(root)
            .into_iter()
            .take(MANIFEST_CAP)
        {
            m.agents.push(ManifestAgent {
                id: a.id,
                name: a.name,
                description: a.description,
                tools: a.tools,
                path: p.to_string_lossy().to_string(),
            });
        }
        scan_hooks(root, &mut m);
        scan_mcp(root, &mut m);
        scan_overrides(root, &mut m);
    }

    m.instructions.truncate(MANIFEST_CAP);
    for f in &m.instructions {
        if f.truncated {
            m.warnings.push(Warning {
                level: "warn".into(),
                text: format!(
                    "{} is {} characters; only the first 40,000 are read into the prompt.",
                    f.name, f.chars
                ),
            });
        }
    }
    if !m.hooks.is_empty() {
        m.warnings.push(Warning {
            level: "danger".into(),
            text: "This folder declares a hook — a shell command of its choosing that runs around tool calls. OpenLeash runs only the hooks you set in Settings → Checks & hooks; nothing here runs this one. It is shown because it is what the repository is asking for.".into(),
        });
    }
    if !m.mcp.is_empty() {
        m.warnings.push(Warning {
            level: "danger".into(),
            text: "This folder declares MCP servers — programs it wants started. Nothing here starts them today (OpenLeash runs the servers you configure), but a declaration is a program the project gets to name.".into(),
        });
    }
    if !trusted && m.has_project_items() {
        m.warnings.push(Warning {
            level: "info".into(),
            text: "Not trusted: none of the project's instructions, skills, agents, hooks or MCP config is loaded.".into(),
        });
    }
    m.injecting = trusted && m.has_project_items();
    m
}

#[cfg(test)]
mod unit {
    use super::*;

    fn d(path: &str, kind: TrustKind, state: TrustState) -> TrustDecision {
        TrustDecision {
            path: norm_path(path),
            kind,
            decision: state,
            decided_at: Utc::now(),
        }
    }

    #[test]
    fn no_decision_is_untrusted() {
        assert!(!trusted(&[], "C:/work/app"));
        assert!(resolve(&[], "C:/work/app").is_none());
    }

    #[test]
    fn a_folder_decision_covers_only_that_folder() {
        let v = [d("C:/work/app", TrustKind::Folder, TrustState::Trusted)];
        assert!(trusted(&v, "C:/work/app"));
        assert!(!trusted(&v, "C:/work/app/nested"));
        assert!(!trusted(&v, "C:/work/other"));
    }

    #[test]
    fn a_parent_decision_covers_children() {
        let v = [d("C:/work", TrustKind::Parent, TrustState::Trusted)];
        assert!(trusted(&v, "C:/work"));
        assert!(trusted(&v, "C:/work/app"));
        assert!(trusted(&v, "C:/work/app/deep/nested"));
        assert!(!trusted(&v, "C:/elsewhere"));
    }

    #[test]
    fn the_nearest_decision_wins() {
        let v = [
            d("C:/work", TrustKind::Parent, TrustState::Trusted),
            d("C:/work/sketchy", TrustKind::Folder, TrustState::Untrusted),
        ];
        assert!(trusted(&v, "C:/work/app"));
        assert!(!trusted(&v, "C:/work/sketchy"));
        // A folder untrusted on the parent still trusts everything else below it.
        assert!(trusted(&v, "C:/work/other"));
    }

    #[test]
    fn a_folder_decision_outranks_a_parent_one_on_the_same_path() {
        let v = [
            d("C:/work", TrustKind::Parent, TrustState::Untrusted),
            d("C:/work", TrustKind::Folder, TrustState::Trusted),
        ];
        assert!(trusted(&v, "C:/work"));
        // …but the parent record still governs the children.
        assert!(!trusted(&v, "C:/work/app"));
    }

    #[test]
    fn paths_are_normalized_before_comparison() {
        let v = [d("C:\\Work\\App\\", TrustKind::Folder, TrustState::Trusted)];
        assert!(trusted(&v, "c:/work/app"));
        assert!(trusted(&v, "C:/work/app/"));
        assert!(trusted(&v, "C:\\Work\\App"));
    }
}
