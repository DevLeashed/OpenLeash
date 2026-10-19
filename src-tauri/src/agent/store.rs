//! Settings + task persistence (plain JSON under ~/.openleash).

use super::{
    new_id, Item, Message, Task, MAX_ULTRA_RUNNING, X_DEFAULT_RUNNING, X_DEFAULT_TOTAL,
    X_MAX_FANOUT, X_MAX_LAYERS,
};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Tests that point OPENLEASH_HOME at their own folder hold this for their whole run:
/// the env var is process-wide and tests run on parallel threads.
#[cfg(test)]
pub static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Claim the data folder for one test (fresh dir, env var set, lock held until the guard drops).
#[cfg(test)]
pub fn test_home(dir: &std::path::Path) -> std::sync::MutexGuard<'static, ()> {
    let g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_dir_all(dir);
    std::env::set_var("OPENLEASH_HOME", dir);
    g
}

fn default_docker_idle() -> u64 {
    super::docker::DEFAULT_IDLE_MINUTES
}

pub fn data_dir() -> PathBuf {
    let d = match std::env::var("OPENLEASH_HOME") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".openleash"),
    };
    let _ = std::fs::create_dir_all(d.join("tasks"));
    d
}

/// `Debug` is hand-written because the derive would print `api_key` and
/// `api_keys` in cleartext. Nothing formats a ProviderCfg today, but the
/// release profile is `panic = "abort"` and one `dbg!` away is a key in a log
/// file. Serialize is untouched — this only changes how the type looks when
/// debug-printed, never what lands on disk.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ProviderCfg {
    #[serde(default)]
    pub api_key: String,
    /// Pool of API keys used for rotation when `key_pool` is on.
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Rotate across `api_keys` when one is rate-limited or rejected.
    #[serde(default)]
    pub key_pool: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "t")]
    pub enabled: bool,
}

impl std::fmt::Debug for ProviderCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCfg")
            .field("base_url", &self.base_url)
            .field("enabled", &self.enabled)
            .field("key_pool", &self.key_pool)
            .field("api_key", &secret(&self.api_key))
            .field(
                "api_keys",
                &format_args!("[{} redacted]", self.api_keys.len()),
            )
            .finish()
    }
}

/// What a secret field shows in a debug print: empty stays empty, anything else
/// is a fixed marker. Deliberately not the real length — that is a slow, cheap
/// oracle for an attacker reading a log. Takes the value rather than a bool so
/// the "is it set" test cannot be inverted at a call site.
fn secret(v: &str) -> &'static str {
    if v.is_empty() {
        ""
    } else {
        "<redacted>"
    }
}

/// A user-added provider: any OpenAI- or Anthropic-compatible endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// openai | anthropic
    #[serde(default = "openai_kind")]
    pub kind: String,
    /// Broken/flaky endpoint: retry far harder before giving up on it.
    #[serde(default)]
    pub insist: bool,
}

/// A ceiling the user put on one of an account's usage windows, so the harness
/// stops using that account before the window is spent.
///
/// Deliberately stored on the account rather than as a cooldown: a cooldown is
/// live state that a resume or a refresh failure is allowed to clear, and a cap
/// the user asked for has to survive both. It is also why nothing about it is
/// derived from the current usage reading — the cap is a standing instruction,
/// and the usage read it acts on arrives later.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct UsageCap {
    /// Window label exactly as the provider reports it ("5h", "Week", "Month").
    /// Matching on the label rather than an index is what keeps a cap attached
    /// to the window across a provider adding or reordering its own list.
    pub window: String,
    /// Percent of the window used at which the account stops being used.
    pub used: f64,
    /// Unix seconds the cap lifts on its own; 0 = until the user removes it.
    pub until: i64,
}

/// A subscription login (ChatGPT/Codex or Claude Pro/Max) used via OAuth.
/// `Debug` is hand-written so the OAuth tokens never reach a log — see the
/// note on `ProviderCfg`.
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Account {
    pub id: String,
    /// codex | claude
    pub kind: String,
    pub label: String,
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds; 0 = unknown / long-lived.
    pub expires_at: i64,
    /// ChatGPT account id (codex only).
    pub account_id: String,
    /// Higher goes first. Equal priority prefers whichever is already warm.
    pub priority: i32,
    pub enabled: bool,
    /// File the login came from; rotated tokens are written back so the owning CLI keeps working.
    pub source: String,
    pub disabled_reason: String,
    /// Caps the user set on this account's usage windows; empty is the norm.
    pub caps: Vec<UsageCap>,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("email", &self.email)
            .field("access_token", &secret(&self.access_token))
            .field("refresh_token", &secret(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("priority", &self.priority)
            .field("enabled", &self.enabled)
            .field("source", &self.source)
            .field("disabled_reason", &self.disabled_reason)
            .finish()
    }
}

/// Fallback chain: try each model in order (a pool model tries every account), then pause.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Route {
    pub id: String,
    pub name: String,
    /// Models this route is the default for.
    pub heads: Vec<String>,
    /// Default for every model that has no dedicated route.
    pub all: bool,
    /// Fallbacks, tried in order after the chosen model.
    pub steps: Vec<String>,
    /// pause | fail
    pub on_exhausted: String,
    /// Optional label shown in the router editor.
    #[serde(default)]
    pub description: String,
    /// Models ordered weakest to strongest.
    #[serde(default)]
    pub ladder: Vec<String>,
    /// Ordered routing rules.
    #[serde(default)]
    pub rules: Vec<super::routerules::Rule>,
}

/// A subagent definition (built-in or user-made).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub prompt: String,
    /// Empty = inherit the task's model.
    pub model: String,
    /// all | read_only | no_shell
    pub tools: String,
    pub color: String,
    pub builtin: bool,
    /// project = loaded from .openleash/agents in the project
    pub source: String,
    /// Give this subagent the project's AGENTS.md / OPENLEASH.md / CLAUDE.md.
    #[serde(default = "t")]
    pub inject_instructions: bool,
    /// Reasoning effort (0 = Max … 4 = Low). None = same as the chat (Explore: at most Medium).
    #[serde(default)]
    pub effort: Option<usize>,
    /// Tool access, Roo-Code style: plain group names (`read`, `edit`, `command`,
    /// `mcp`) for unrestricted access, or a two-element `[name, {fileRegex, …}]`
    /// tuple to restrict a group to matching paths. Empty = fall back to
    /// `tools` above, which is what every agent saved before this field did.
    #[serde(default)]
    pub groups: Vec<ToolGroup>,
}

/// One entry of an `AgentDef`'s `groups`: Roo Code's two shapes, exactly —
/// a bare group name, or a `[name, {fileRegex, description}]` tuple. An untagged
/// enum over a newtype tuple so serde both parses and emits the array form the
/// brief and Roo use, without a hand-written `Deserialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolGroup {
    Plain(String),
    Restricted((String, GroupOpts)),
}

/// The options object of a restricted group. `fileRegex` is Roo's spelling on
/// the wire; the snake-case variant is accepted on read so an agent file that
/// used our internal naming still loads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupOpts {
    #[serde(rename = "fileRegex", alias = "file_regex")]
    pub file_regex: String,
    pub description: String,
}

impl ToolGroup {
    /// The group name, whichever shape this is.
    pub fn name(&self) -> &str {
        match self {
            ToolGroup::Plain(n) => n,
            ToolGroup::Restricted((n, _)) => n,
        }
    }

    fn opts(&self) -> Option<&GroupOpts> {
        match self {
            ToolGroup::Plain(_) => None,
            ToolGroup::Restricted((_, o)) => Some(o),
        }
    }

    /// The path pattern this group is limited to, if any.
    pub fn file_regex(&self) -> Option<&str> {
        self.opts()
            .map(|o| o.file_regex.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn description(&self) -> &str {
        self.opts().map(|o| o.description.as_str()).unwrap_or("")
    }

    fn restricted(name: String, file_regex: String, description: String) -> ToolGroup {
        ToolGroup::Restricted((
            name,
            GroupOpts {
                file_regex,
                description,
            },
        ))
    }
}

/// The `edit`, `read`, `command` and `mcp` group names Roo uses, mapped to the
/// tools they gate. `read`/`command`/`mcp` are accepted so a `.roomodes`-shaped
/// file loads without complaint; only `edit` carries a `fileRegex` today (the
/// same restriction Roo documents), and validation rejects a regex on the rest
/// rather than pretending to honour it.
pub const TOOL_GROUPS: [&str; 4] = ["read", "edit", "command", "mcp"];

impl AgentDef {
    /// Compile the edit group's `fileRegex`, if it declares one. `Err` carries a
    /// message naming the agent and the pattern.
    pub fn edit_restriction(&self) -> Result<Option<regex::Regex>, String> {
        for g in &self.groups {
            if g.name().eq_ignore_ascii_case("edit") {
                if let Some(p) = g.file_regex() {
                    return regex::Regex::new(p).map(Some).map_err(|e| {
                        format!(
                            "Agent `{}`: `fileRegex: {}` isn't a valid regular expression ({e}).",
                            self.id, p
                        )
                    });
                }
            }
        }
        Ok(None)
    }
}

/// Validate an agent's `groups` (and the `fileRegex` inside them). Used by the
/// settings write path so a broken pattern is refused at save time rather than
/// silently disarming the restriction at run time. `Err` is user-facing text.
pub fn validate_agent_groups(d: &AgentDef) -> Result<(), String> {
    let mut seen_edit_regex = false;
    for g in &d.groups {
        let name = g.name().to_lowercase();
        if !TOOL_GROUPS.contains(&name.as_str()) {
            return Err(format!(
                "Agent `{}`: unknown tool group `{}`. Use one of: {}.",
                d.id,
                g.name(),
                TOOL_GROUPS.join(", ")
            ));
        }
        if let Some(p) = g.file_regex() {
            // Only `edit` (and, to read files of a given type, `read`) can be a
            // path policy; a regex on `command`/`mcp` would look enforced in the
            // UI and not be, so refuse it loudly.
            if name != "edit" {
                return Err(format!(
                    "Agent `{}`: `fileRegex` only applies to the `edit` group, not `{}`.",
                    d.id,
                    g.name()
                ));
            }
            if seen_edit_regex {
                return Err(format!(
                    "Agent `{}`: the `edit` group can only appear once — merge the patterns into one `fileRegex`.",
                    d.id
                ));
            }
            seen_edit_regex = true;
            regex::Regex::new(p).map_err(|e| {
                format!(
                    "Agent `{}`: `fileRegex: {}` isn't a valid regular expression ({e}).",
                    d.id, p
                )
            })?;
        }
    }
    Ok(())
}

/// A skill definition: a `SKILL.md` file (Agent Skills / Claude Code format)
/// with `name` + `description` frontmatter, plus optional bundled resources
/// in the same directory. The agent sees the name/description list and reads
/// the full `SKILL.md` with `read_file` when the job calls for it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    /// user = `~/.openleash/skills/<name>`, project = the project's skills dir
    pub source: String,
    /// Absolute path to the `SKILL.md` file.
    pub path: String,
    pub enabled: bool,
    /// Bundled files beside `SKILL.md` (references, scripts, templates).
    pub files: usize,
    /// `disable-model-invocation: true` in the frontmatter — absent from the
    /// model's skill list, so it costs zero context until the user invokes it.
    #[serde(default)]
    pub disable_model_invocation: bool,
    /// `user-invocable: true` in the frontmatter — offered in the user's
    /// command palette. A skill with `disable_model_invocation` is invocable
    /// too, or it would be reachable by nobody.
    #[serde(default)]
    pub user_invocable: bool,
}

fn openai_kind() -> String {
    "openai".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowRule {
    /// `Bash(pnpm test *)`-style: a command prefix pattern where `*` matches anything.
    pub pattern: String,
    /// Empty = all projects, else a project path.
    #[serde(default)]
    pub project: String,
}

/// A per-project opt-out of some of the project's own instruction files.
///
/// The whole reason it exists: a repository can carry a `CLAUDE.md` (or
/// `AGENTS.md`, …) that a particular project should not have injected into every
/// agent's prompt — the common case being a `CLAUDE.md` inherited from a
/// different tool. The user picks which files, per project, and the loader skips
/// them exactly as if they were absent.
///
/// Kept separate from folder trust on purpose. Trust is a security decision
/// about whether *anything* from the folder may be loaded, fails closed, and is
/// remembered per folder (and parent). This is a convenience selection that
/// fails *open* — no rule means every file is loaded — so recording one must
/// never be mistaken for, or weaken, a trust decision. An untrusted folder
/// injects nothing regardless of what is listed here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProjectDocRule {
    /// The project folder, stored normalized like a trust decision's path so a
    /// path picked one way and compared another still matches.
    pub path: String,
    /// File names from `prompt::MEMORY_FILES` the user switched off. Project
    /// files only; the user's own `~/.` globals are not governed by this.
    pub disabled: Vec<String>,
}

/// `Debug` is hand-written: `env` is exactly where people put `GITHUB_TOKEN`
/// and friends, so a derived Debug would print it. Keys are kept (they are
/// diagnostic) — only the values are masked.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct McpServerCfg {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "t")]
    pub enabled: bool,
    /// `stdio` (default) runs the command as a child process. `http` speaks
    /// the streamable-HTTP transport to a remote endpoint, which is how nearly
    /// every hosted MCP server is published. Absent means stdio, so every
    /// existing settings.json keeps working untouched.
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub url: String,
    /// Extra headers to send with every request (auth on a hosted server).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Servers that require OAuth rather than a bearer token. The URL is used
    /// as the resource indicator for discovery.
    #[serde(default)]
    pub oauth: bool,
    /// Tool names on this server that skip the approval prompt outright, e.g.
    /// `["read_thing"]` pre-approves `mcp__<server>__read_thing` and nothing
    /// else. Each entry expands (via `mcp::auto_approve_rules`) to the exact
    /// `mcp__<server>__<tool> *` allow rule the approval prompt already offers,
    /// so this field only ever narrows — it can never widen past the one tool it
    /// names. Empty = ask as before.
    #[serde(default)]
    pub auto_approve: Vec<String>,
    /// Always declare this server's tools, instead of holding them back for
    /// `tool_search` (see `toolindex`). On by default, and `= "t"` for the same
    /// reason as `enabled`: a settings.json written before this field existed
    /// must keep the new behaviour, not silently start deferring a server the
    /// user has never made a decision about. An explicit `false` is the opt-out
    /// — for a server whose tools the agent reaches for constantly, where a
    /// search per call is the expense and the token saving is the rounding error.
    #[serde(default = "t")]
    pub defer: bool,
}

impl McpServerCfg {
    /// True when this server is a remote HTTP endpoint rather than a command.
    pub fn is_http(&self) -> bool {
        self.transport.eq_ignore_ascii_case("http")
    }

    /// The one-line description Settings shows under the server name.
    pub fn describe(&self) -> String {
        if self.is_http() {
            self.url.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
                .trim()
                .to_string()
        }
    }
}

impl std::fmt::Debug for McpServerCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: HashMap<&str, &str> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), secret(v)))
            .collect();
        let headers: HashMap<&str, &str> = self
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), secret(v)))
            .collect();
        f.debug_struct("McpServerCfg")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &env)
            .field("transport", &self.transport)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("oauth", &self.oauth)
            .field("enabled", &self.enabled)
            .field("auto_approve", &self.auto_approve)
            .finish()
    }
}

/// One layer of an ULTRATHREAD X ladder: which model its subagents run on,
/// how hard they think, and how many of them a coordinator may launch at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UltraXLayer {
    /// Empty = the orchestrator's model.
    pub model: String,
    /// Reasoning effort (0 = Max … 4 = Low). None = that subagent type's / the chat's.
    pub effort: Option<usize>,
    /// How many subagents one agent on this layer may launch at the same time.
    /// 0 = no per-agent cap (only the whole-tree running cap applies).
    pub fanout: u8,
}

fn x_layer() -> UltraXLayer {
    UltraXLayer {
        model: String::new(),
        effort: None,
        fanout: 0,
    }
}

impl Default for UltraXLayer {
    fn default() -> Self {
        x_layer()
    }
}

/// ULTRATHREAD X: a configured ladder of subagent layers, each with its own
/// model / effort / fanout. A shallow ladder is just ultrathread with per-layer
/// models, so this is a superset rather than a separate mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UltraX {
    /// Layers below the orchestrator (1 = launched by the main agent). 1..=X_MAX_LAYERS.
    pub layers: Vec<UltraXLayer>,
    /// Subagents running at once across the whole tree (0 = X_DEFAULT_RUNNING).
    pub max_running: usize,
    /// Subagents spawned for the whole task, ever (0 = X_DEFAULT_TOTAL).
    pub max_total: usize,
    /// Run the nested tree in ULTRATHREAD worktrees.
    pub wt: bool,
}

impl Default for UltraX {
    fn default() -> Self {
        UltraX {
            layers: vec![x_layer(); X_MAX_LAYERS],
            max_running: X_DEFAULT_RUNNING,
            max_total: X_DEFAULT_TOTAL,
            wt: false,
        }
    }
}

impl UltraX {
    /// Deepest layer a subagent may launch another subagent on, 0..=layers.len().
    pub fn max_depth(&self) -> u8 {
        self.layers.len().min(X_MAX_LAYERS) as u8
    }

    /// A task is in ULTRATHREAD X when it has a ladder configured with >= 2 layers.
    pub fn on(&self) -> bool {
        self.layers.len() > 1
    }

    /// Cap on simultaneously running subagents for this tree.
    pub fn running_cap(&self) -> usize {
        if self.max_running == 0 {
            X_DEFAULT_RUNNING
        } else {
            self.max_running.min(MAX_ULTRA_RUNNING)
        }
    }

    /// Cap on subagents spawned for the whole task, ever.
    pub fn total_cap(&self) -> usize {
        if self.max_total == 0 {
            X_DEFAULT_TOTAL
        } else {
            self.max_total
        }
    }

    /// The layer `depth` agents run on (1 = the orchestrator's own subagents).
    pub fn layer(&self, depth: u8) -> Option<&UltraXLayer> {
        if depth == 0 {
            return None;
        }
        self.layers.get(depth as usize - 1)
    }

    /// Keep the ladder valid: trim to the layer cap, pad out to it, clamp the numbers.
    pub fn normalize(&mut self) {
        self.layers.truncate(X_MAX_LAYERS);
        while self.layers.len() < X_MAX_LAYERS {
            self.layers.push(x_layer());
        }
        for l in &mut self.layers {
            l.fanout = l.fanout.min(X_MAX_FANOUT);
            if let Some(e) = l.effort {
                l.effort = Some(e.min(4));
            }
        }
        self.max_running = self.running_cap();
        self.max_total = self.total_cap();
    }
}

fn t() -> bool {
    true
}

/// A user-defined slash command that expands to a prompt (never executable code).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub prompt: String,
}

/// A prompt parked for later from the new-task screen, with the composer
/// options it was written under. Loading one restores all of it at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPrompt {
    pub id: String,
    pub text: String,
    /// Project folder it was written for; "" = wherever you are now.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub route: String,
    #[serde(default)]
    pub assist: String,
    #[serde(
        default = "super::permissions::default_perm",
        deserialize_with = "super::permissions::deserialize_perm"
    )]
    pub perm: String,
    /// None = use the default effort.
    #[serde(default)]
    pub effort: Option<usize>,
    #[serde(default)]
    pub plan: bool,
    #[serde(default)]
    pub ultra: bool,
    #[serde(default)]
    pub ultra_wt: bool,
    #[serde(default)]
    pub worktree: bool,
    #[serde(default)]
    pub branch: String,
    /// Subagent ids it may use; empty = the current set.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Pasted images, as data URLs.
    #[serde(default)]
    pub images: Vec<String>,
    /// Files attached by path. The agent opens these itself.
    #[serde(default)]
    pub files: Vec<SavedFile>,
    pub created_at: DateTime<Utc>,
}

/// One entry of a saved prompt's attached files. Just enough to restore the
/// chip in the composer; the path is the whole point, so nothing is read here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedFile {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub image: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub model: String,
    pub effort: usize,
    // What the settings file records is copied onto a task when it is created.
    // This also normalizes the empty string old builds wrote before a choice.
    #[serde(
        default = "super::permissions::default_perm",
        deserialize_with = "super::permissions::deserialize_perm"
    )]
    pub perm: String,
    pub worktree: bool,
    pub subagents: bool,
    pub ui_size: String,
    pub send_with: String,
    /// Let Enter send the message in a phone browser, with Shift Enter the
    /// newline. Off by default.
    ///
    /// A phone's on-screen keyboard is the reason this is separate from
    /// `send_with` and off rather than on: its Return key cannot be relied on
    /// to say whether Shift is held, so Enter-sends there would also send every
    /// message the user meant to put a line break in, with no modifier left to
    /// ask for the newline. Off, Enter is always a newline and only the send
    /// button sends — the one arrangement that cannot lose a draft to a
    /// misread key. The desktop window is unaffected and follows `send_with`.
    #[serde(default)]
    pub phone_enter_sends: bool,
    /// Color theme: "raycast" (cyan) or "lavender".
    pub theme: String,
    /// Light or dark, or follow the OS. "light" / "dark" / "system".
    pub mode: String,
    pub projects: Vec<String>,
    pub project: String,
    pub providers: HashMap<String, ProviderCfg>,
    pub recent_models: Vec<String>,
    /// Starred in the model picker.
    pub favorite_models: Vec<String>,
    pub custom_models: Vec<String>,
    pub custom_providers: Vec<CustomProvider>,
    pub model_configs: Vec<super::providers::ModelInfo>,
    /// Model ids toggled off in Settings → Models (hidden from pickers, dimmed in settings).
    #[serde(default)]
    pub disabled_models: Vec<String>,
    /// Built-in / pool model ids deleted in Settings → Models (hidden everywhere; re-adding revives).
    #[serde(default)]
    pub removed_models: Vec<String>,
    pub allow: Vec<AllowRule>,
    pub mcp: Vec<McpServerCfg>,
    pub budget: f64,
    /// yyyy-mm-dd -> dollars
    pub spend: HashMap<String, f64>,
    pub tokens_month: u64,
    /// Interface zoom in percent. Defaults to 100 on Linux and 110 on Windows/macOS.
    pub ui_zoom: u32,
    pub accounts: Vec<Account>,
    pub routes: Vec<Route>,
    pub agents: Vec<AgentDef>,
    /// Subagent ids new chats may use.
    pub default_agents: Vec<String>,
    /// Skill names toggled off (hidden from agents, dimmed in settings).
    #[serde(default)]
    pub disabled_skills: Vec<String>,
    /// Prompts parked for later from the new-task screen.
    #[serde(default)]
    pub saved_prompts: Vec<SavedPrompt>,
    /// User-authored prompt templates; expanded as ordinary user messages, never executed.
    #[serde(default)]
    pub custom_commands: Vec<CustomCommand>,
    /// guide | default | necessary
    pub assist: String,
    /// Every agent in every task is paused.
    pub paused_all: bool,
    pub paused_reason: String,
    /// Closing the window hides it to the tray; agents keep running.
    pub close_to_tray: bool,
    /// Workspace for new tasks: "local" | "worktree" | "docker". Empty means
    /// "follow `worktree`", which is all older settings files have.
    #[serde(default)]
    pub workspace: String,
    /// Minutes a docker workspace may sit idle before its container is
    /// stopped (0 = never). A project's `openleash.json` can override it.
    #[serde(default = "default_docker_idle")]
    pub docker_idle_minutes: u64,
    /// Rebuild Docker images automatically if the build context changed. The first build still occurs.
    #[serde(default = "t")]
    pub docker_auto_rebuild: bool,
    /// Desktop notifications when a chat finishes, fails, needs you or auto-pauses (only while the window isn't focused).
    #[serde(default = "t")]
    pub notify: bool,
    /// Show the model's internal reasoning blocks in the transcript. Hidden by default.
    #[serde(default)]
    pub show_thinking: bool,
    /// Show the estimated per-category split of the context window (system
    /// prompt, project instructions, skills and memory, tool schemas,
    /// attachments, conversation, compaction summary) under the context meter.
    ///
    /// Off by default, and off is the honest state: no provider reports that
    /// split, so it is measured from the request the harness assembled and
    /// rescaled to the provider's own prompt count. The ring's used/limit pair
    /// is the measured part; this is the estimate, and it is drawn only for the
    /// user who asks for it.
    #[serde(default)]
    pub ctx_breakdown: bool,
    /// Subscription kinds whose usage shows on the home screen.
    pub home_usage: Vec<String>,
    /// Settings page the sidebar footer's shortcut button — the one beside the
    /// usage bar — opens, as a tab id from the frontend's settings-tab list.
    ///
    /// Only the frontend reads it, and an unknown id resolves back to
    /// [`sidebar_button_tab_default`] there, so a page this build does not have
    /// cannot leave the button opening a blank settings screen. "accounts" is
    /// the page that button has always opened, so an install upgrading into this
    /// field keeps the button it already had.
    #[serde(default = "sidebar_button_tab_default")]
    pub sidebar_button_tab: String,
    /// Cross-chat inbox for pending approvals and questions. Hidden by default.
    #[serde(default)]
    pub needs_you: bool,
    /// Show the optional GitHub pull-request screen next to Saved prompts.
    #[serde(default)]
    pub show_pull_requests: bool,
    /// Show the Schedules screen in the sidebar. Off by default; enabled
    /// schedules run either way.
    #[serde(default)]
    pub show_schedules: bool,
    /// Sidebar shows chats from every project, not just the current one.
    #[serde(default)]
    pub all_projects: bool,
    /// Order of the Saved prompts list. Empty keeps the stored order (prompts are
    /// appended as they are parked, so it reads oldest first); `newest` is most
    /// recently parked first, `folder` groups the current folder's prompts above
    /// the rest, `az` sorts by prompt text. The Saved screen offers these only
    /// while `all_projects` is on, but the order is honoured whenever it is set.
    #[serde(default)]
    pub saved_sort: String,
    /// Accent per model id ("#rrggbb"), e.g. the working spinner. Unset = the family default.
    #[serde(default)]
    pub model_colors: HashMap<String, String>,
    /// Built-in plugins (Settings → Plugins).
    #[serde(default)]
    pub plugins: super::plugins::PluginsCfg,
    /// Typecheck/compile touched projects after edits and feed errors back.
    pub diagnostics: bool,
    /// Nudge an agent that edited code but never built or tested it.
    pub verify_nudge: bool,
    /// User commands run before/after tools and at the run's edges.
    ///
    /// The command bodies live here; whether each may *run* does not. That is
    /// `trusted_hooks` below — see `checks::TrustedHook` for why the approval is
    /// a separate list rather than a `trusted: true` in this one.
    pub hooks: Vec<super::checks::Hook>,
    /// Hooks the user has reviewed and approved, keyed by `(id, origin)` and the
    /// hash of the body that was approved.
    ///
    /// Deliberately its own list, and deliberately never written by a hook's own
    /// file: `.openleash/hooks.json` in a repository contributes hooks, but only
    /// this list — which lives in the user's `settings.json` — can approve one.
    /// That is the entire reason a prompt-injected repo cannot run a hook: it can
    /// write the hook, and it cannot write the approval.
    #[serde(default)]
    pub trusted_hooks: Vec<super::checks::TrustedHook>,
    /// The one-time grandfathering in `checks::migrate_hooks` has run.
    ///
    /// Load-bearing, not bookkeeping: it is what stops the *next* hook from
    /// being auto-approved on the next launch. An approval that re-issues itself
    /// is not a review, so the migration is allowed to run once and never again;
    /// after that a new hook — the user's or a repository's — is untrusted until
    /// somebody presses Trust.
    #[serde(default)]
    pub hooks_trust_migrated: bool,
    /// Also inject the user's global `~/.claude/CLAUDE.md` into every agent. Off by default.
    #[serde(default)]
    pub inject_global_claude: bool,
    /// Per-project opt-out of individual project instruction files. Empty = every
    /// project loads everything it has; an entry names a folder and the files the
    /// user switched off for it. Fails open and is only a selection, never a
    /// trust decision — see [`ProjectDocRule`].
    #[serde(default)]
    pub project_docs: Vec<ProjectDocRule>,
    /// Let the agent see the user's other chats — and their live state — and
    /// send one of them a message (`list_chats`, `message_chat`). On by
    /// default; the user can switch it off. It is the one tool pair that
    /// reaches *outside* the chat the user opened, so an agent can read
    /// conversations it was never given and start a turn in one of them. The
    /// main agent only — a chat's main agent is the one with the wider view,
    /// and a fan-out of subagents each messaging other chats is not something
    /// a brief asks for.
    ///
    /// The tool list does not change with the switch: a chat that already holds
    /// the frozen prefix keeps its tools and is told the rules changed (see
    /// `runner::announce_changes`), so toggling mid-chat cannot leave the cached
    /// prefix disagreeing with what the model may do.
    /// `= "t"` rather than `#[serde(default)]`: a field-level default means
    /// *false*, so a settings.json written before this flag existed would load
    /// with it off. An explicit `false` from the user still wins.
    #[serde(default = "t")]
    pub cross_chat: bool,
    /// Let the agent keep this chat's name up to date as the work moves on
    /// (the `set_title` tool). It rides along on turns the agent is already
    /// making, so it costs nothing extra, and an unnamed chat in the sidebar
    /// is the worse default — a chat you name yourself is never renamed, and
    /// the right-click menu hands a name back to the agent.
    ///
    /// `= "t"` rather than `#[serde(default)]` on purpose: a field-level
    /// `default` means *false*, so a settings.json written before this flag
    /// existed would load with naming off, and the one user who never chose
    /// anything would be the one who kept the old behaviour. An explicit
    /// `false` from the user still wins.
    #[serde(default = "t")]
    pub agent_titles: bool,
    /// Keep the sidebar sorted by attention and recent activity, including chats
    /// the user has manually dragged. Off preserves the manual order.
    #[serde(default = "t")]
    pub automatic_chat_reorder: bool,
    /// Resume chats paused because provider accounts/keys were exhausted once one frees up.
    /// Existing installs keep the established automatic-resume behavior.
    #[serde(default = "t")]
    pub automatic_resume: bool,
    /// The welcome tour has been shown (and closed) once. The frontend opens it
    /// on its own only while this is false *and* there are no chats, so an
    /// existing install that predates the flag — which loads it as false — is
    /// not ambushed by a first-run tour. It stays reachable from the palette.
    #[serde(default)]
    pub welcome_seen: bool,
    /// Let the agent keep long-term memory (the `memory` tool): notes about the
    /// user shared by every project, and notes per project, loaded at the start
    /// of each new chat. On by default. The agent only ever writes its own
    /// notes, as plain markdown under `~/.openleash/memory/` that the user can
    /// read, edit or delete by hand (or in the project, see `memory_in_project`).
    #[serde(default = "t")]
    pub memory: bool,
    /// Whether the on-by-default switch for `memory` has been applied to this
    /// settings file. Builds before it wrote `"memory": false` into every file
    /// whether or not anyone chose it, so the serde default alone would leave
    /// every existing install off. `load_settings` turns memory on once for a
    /// file without this marker and sets it, so a later "off" sticks.
    #[serde(default)]
    pub memory_default_applied: bool,
    /// Projects whose own notes are kept in `<project>/.openleash/memory/` so
    /// they can be committed, instead of under `~/.openleash/memory/`. Per
    /// project, compared as normalized folder paths like `project_docs`; empty
    /// means every project keeps them privately, which is what an upgrade from
    /// a build without this setting should do. A project not listed still reads
    /// notes already committed there. See `memory::Dirs`.
    #[serde(default)]
    pub memory_in_project: Vec<String>,
    /// Whether the on-by-default switch for the browser plugin has been
    /// applied to this settings file. The same story as
    /// `memory_default_applied`: builds before it wrote
    /// `"browser": {"enabled": false, …}` into every file because that was
    /// the default, not because anyone chose it. `load_settings` turns the
    /// browser on once for a file without this marker and sets it, so an off
    /// chosen afterwards (saved together with the marker) sticks.
    #[serde(default)]
    pub browser_default_applied: bool,
    #[serde(default = "t")]
    pub deny_env_files: bool,
    /// Folder trust decisions. A folder with no decision here is untrusted —
    /// fail closed — so an upgrade loads with an empty list and every existing
    /// project is asked about the next time it is opened. See `agent::trust`.
    #[serde(default)]
    pub trust: Vec<crate::agent::trust::TrustDecision>,
    #[serde(default)]
    pub git_attribution: String,
    #[serde(default)]
    pub git_commit_verify: bool,
    #[serde(default)]
    pub cache_keepalive: bool,
    #[serde(default = "keepalive_pings_default")]
    pub cache_keepalive_pings: u32,
    /// Keep a chat's row in the stats after the chat is deleted. On by default:
    /// the spend happened either way, and the row is what explains a total no
    /// live chat accounts for. Off drops the row when the chat is deleted.
    #[serde(default = "t")]
    pub keep_chat_stats: bool,
    #[serde(default = "t")]
    pub tool_search: bool,
    #[serde(default)]
    pub defer_plugins: bool,
    #[serde(default = "t")]
    pub checkpoints: bool,
    /// Opt-in, local OTLP/HTTP trace export: the URL of a collector the *user*
    /// runs. One span per model request and one per tool call go there, as
    /// GenAI-semconv spans (see `agent::trace`).
    ///
    /// Empty — the default, and what every `settings.json` written before this
    /// field existed loads as — means the exporter is not started at all: no
    /// client, no thread, and nothing added to the request or tool path. That
    /// is the whole safety model: OpenLeash still advertises no telemetry, and
    /// spans only leave the machine when the user types an address for them.
    ///
    /// There is deliberately no vendor default and no API key field, and the
    /// frontend's placeholder suggestion (`SUGGESTED_ENDPOINT` in
    /// `agent::trace`) is a suggestion, never a stored value — storing one
    /// would make "off by default" untrue.
    #[serde(default)]
    pub trace_endpoint: String,
    /// "Spend quota before it resets": the backlog queue and the thresholds
    /// the watcher in `agent::burner` acts on. Off by default; a file written
    /// before this field existed loads with it off and an empty queue.
    #[serde(default)]
    pub burner: super::burner::BurnerCfg,
    /// Every key of `settings.json` this build has no field for.
    ///
    /// A catch-all, not a rejection: `deny_unknown_fields` is deliberately off
    /// (see `research_tools_cannot_be_switched_off`), because a file written by
    /// a newer build has to keep loading here. The missing half was what
    /// happened next — those keys were dropped, so opening and re-saving your
    /// settings on an older version quietly deleted what the newer one wrote,
    /// and a mistyped key was reported nowhere at all. Keeping the keys makes
    /// that round trip lossless, and [`unknown_settings_warning`] gives "why
    /// didn't my setting do anything?" an answer. Values are held only so they
    /// can be written back; nothing reads them.
    ///
    /// The names come from serde's own field dispatch, never from diffing the
    /// keys in the file against the keys this build writes: a field with
    /// `skip_serializing_if` — this one included — is absent from that output
    /// while it holds no value, so a diff would report a real setting as unknown
    /// the first time the user set it.
    ///
    /// `skip_serializing_if` here is what keeps the empty case byte-for-byte
    /// what this build wrote before the field existed.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Four pings: about twenty minutes of a five-minute provider window, which
/// covers the pause length that actually shows up in a long task.
fn keepalive_pings_default() -> u32 {
    4
}

/// The page the sidebar footer's shortcut button opens on a fresh install, and
/// for any stored id the frontend has no page for. Accounts, beside a usage bar
/// that is itself about accounts, is the page that button has always opened;
/// matches `SIDEBAR_SETTINGS_TAB_DEFAULT` in `src/ui/settingsTabs.ts`.
fn sidebar_button_tab_default() -> String {
    "accounts".into()
}

fn default_ui_zoom() -> u32 {
    if cfg!(target_os = "linux") { 100 } else { 110 }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "anthropic/claude-opus-5".into(),
            effort: 2,
            perm: super::permissions::default_perm(),
            worktree: false,
            subagents: true,
            // "v2" is the current layout marker, so a fresh install doesn't walk
            // the migration below and get rewritten to 90%.
            ui_size: "v2".into(),
            send_with: "enter".into(),
            // A phone browser's Enter is a newline until the user asks for
            // Enter-to-send: see the field's comment. Off is the default for a
            // fresh install and for every settings.json written before this
            // field existed, which is the whole point of it.
            phone_enter_sends: false,
            theme: "raycast".into(),
            mode: "dark".into(),
            projects: vec![],
            project: String::new(),
            providers: HashMap::new(),
            recent_models: vec![],
            favorite_models: vec![],
            custom_models: vec![],
            custom_providers: vec![],
            model_configs: vec![],
            disabled_models: vec![],
            removed_models: vec![],
            allow: vec![
                AllowRule {
                    pattern: "git status*".into(),
                    project: String::new(),
                },
                AllowRule {
                    pattern: "git diff*".into(),
                    project: String::new(),
                },
                AllowRule {
                    pattern: "git log*".into(),
                    project: String::new(),
                },
            ],
            mcp: vec![],
            budget: 50.0,
            spend: HashMap::new(),
            tokens_month: 0,
            ui_zoom: default_ui_zoom(),
            accounts: vec![],
            routes: vec![],
            agents: vec![],
            default_agents: vec!["explore".into(), "general".into()],
            disabled_skills: vec![],
            saved_prompts: vec![],
            custom_commands: vec![],
            assist: "default".into(),
            paused_all: false,
            paused_reason: String::new(),
            close_to_tray: true,
            workspace: String::new(),
            docker_idle_minutes: default_docker_idle(),
            docker_auto_rebuild: true,
            notify: true,
            show_thinking: false,
            ctx_breakdown: false,
            model_colors: HashMap::new(),
            home_usage: super::providers::PROVIDERS
                .iter()
                .filter(|p| p.account.is_some())
                .map(|p| p.id.into())
                .collect(),
            sidebar_button_tab: sidebar_button_tab_default(),
            needs_you: false,
            show_pull_requests: false,
            show_schedules: false,
            all_projects: false,
            saved_sort: String::new(),
            plugins: Default::default(),
            diagnostics: true,
            verify_nudge: true,
            hooks: vec![],
            trusted_hooks: vec![],
            hooks_trust_migrated: false,
            inject_global_claude: false,
            project_docs: vec![],
            agent_titles: true,
            automatic_chat_reorder: true,
            automatic_resume: true,
            welcome_seen: false,
            memory: true,
            memory_default_applied: true,
            memory_in_project: vec![],
            browser_default_applied: true,
            cross_chat: true,
            deny_env_files: true,
            trust: vec![],
            git_attribution: "co-authored-by".into(),
            git_commit_verify: false,
            cache_keepalive: false,
            cache_keepalive_pings: keepalive_pings_default(),
            keep_chat_stats: true,
            tool_search: true,
            defer_plugins: false,
            checkpoints: true,
            // Exporting traces requires the user to name a collector. A fresh
            // install has none, so this starts off and stays off until they do.
            trace_endpoint: String::new(),
            burner: Default::default(),
            // A fresh install has no file, so nothing can be unrecognised. The
            // catch-all is filled only by `load_settings`, from a file this
            // build did not write.
            unknown: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn month_spend(&self) -> f64 {
        let prefix = chrono::Local::now().format("%Y-%m").to_string();
        self.spend
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .sum()
    }
}

/// The `settings.json` keys this build does not recognise, sorted. Empty when
/// the file is one this build understands — the common case.
///
/// Read from [`Settings::unknown`], which `load_settings` fills in; nothing is
/// re-read from disk here. Names only, never values: a key name is what the user
/// mistyped or what a newer build added, and a value can be anything at all,
/// including something pasted from a credential.
pub fn unknown_settings_keys(s: &Settings) -> Vec<String> {
    s.unknown.keys().cloned().collect()
}

/// A one-line warning naming the keys in [`unknown_settings_keys`] and how many
/// there are, or `None` when there are none — so a caller can show nothing at
/// all rather than an empty notice.
///
/// This is the "why didn't my setting work?" answer: the keys were kept (see
/// [`Settings::unknown`]) but nothing in this build reads them, so the settings
/// screen cannot show them and the only other signal would be silence.
pub fn unknown_settings_warning(s: &Settings) -> Option<String> {
    let keys = unknown_settings_keys(s);
    if keys.is_empty() {
        return None;
    }
    Some(format!(
        "settings.json has {} setting{} this build does not recognise: {}. \
         They were kept, but nothing here reads them — check the spelling, or the \
         version you are running.",
        keys.len(),
        if keys.len() == 1 { "" } else { "s" },
        keys.join(", ")
    ))
}

pub fn load_settings() -> Settings {
    let raw = std::fs::read_to_string(data_dir().join("settings.json")).unwrap_or_default();
    let mut s: Settings = serde_json::from_str(&raw).unwrap_or_default();
    // A mistyped or newer-build key is not an error — the file has to keep
    // loading — but silence is not an answer either: say which keys this build
    // ignored, and in `Settings::unknown` keep them so the next save cannot
    // delete them. Names only; a value may be a secret the user pasted.
    if let Some(warning) = unknown_settings_warning(&s) {
        eprintln!("[openleash] {warning}");
    }
    if s.ui_zoom < 50 || s.ui_zoom > 200 {
        // Old named sizes: what used to be "Large" is the new 100%.
        s.ui_zoom = match s.ui_size.as_str() {
            "Small" => 80,
            "Default" | "" => 90,
            _ => 100,
        };
        s.ui_size = "custom".into();
    }
    // v2 layout: migrate the legacy default to the platform's current default zoom.
    if s.ui_size != "v2" {
        if s.ui_zoom == 100 || s.ui_zoom == 0 {
            s.ui_zoom = default_ui_zoom();
        }
        s.ui_size = "v2".into();
    }
    if !matches!(s.assist.as_str(), "guide" | "default" | "necessary") {
        s.assist = "default".into();
    }
    // Canonicalize the attribution style. `Attribution::parse` deliberately maps
    // anything unrecognised — including the empty string a settings.json written
    // before this field existed produces — to the default, and only an explicit
    // "none" to off. Writing the parsed value back means the file agrees with
    // what commits will carry, instead of the UI showing a value the commit path
    // silently reinterprets.
    s.git_attribution = super::commitmsg::Attribution::parse(&s.git_attribution)
        .as_str()
        .to_string();
    // `explore` + `general` are always on: repair old settings that toggled them off.
    ensure_required_agents(&mut s.default_agents);
    // The `render` plugin was folded into `browser`; an install that had render
    // on keeps the capability under the surviving name. Needs the raw text,
    // because serde has already dropped the now-unknown `render` key.
    s.plugins.migrate(&raw);
    // Fold each MCP server's `auto_approve` list back into `allow`. The rules
    // are not stored twice: `allow` is the only place a rule lives, and this is
    // what writes them on boot so a hand-edit of settings.json and a live UI
    // edit converge on the same state.
    super::mcp::sync_auto_approve(&mut s.allow, &s.mcp);
    super::checks::migrate_hooks(&mut s);
    apply_memory_default(&mut s);
    // After `plugins.migrate`: the render fold-in only copies render's
    // viewport while the browser is still off, so switching it on first
    // would drop the numbers an old render user had.
    apply_browser_default(&mut s);
    s
}

/// See `Settings::memory_default_applied`. Once per settings file.
fn apply_memory_default(s: &mut Settings) {
    if !s.memory_default_applied {
        s.memory = true;
        s.memory_default_applied = true;
    }
}

/// See `Settings::browser_default_applied`. Once per settings file. Nothing
/// is written here — like memory's, the marker reaches disk with the next
/// save, and until then every load reaches the same answer (on). Switching
/// the plugin on launches nothing: Chromium starts lazily, on the first
/// `browser` call or panel open.
fn apply_browser_default(s: &mut Settings) {
    if !s.browser_default_applied {
        s.plugins.browser.enabled = true;
        s.browser_default_applied = true;
    }
}

pub fn save_settings(s: &Settings) {
    if let Ok(j) = serde_json::to_string_pretty(s) {
        write_atomic(&data_dir().join("settings.json"), &j);
    }
}

pub fn save_task(t: &Task) {
    // A task whose subagent maps are still on disk cannot be written back: the
    // in-memory copy is a header, and saving it would replace real subagent
    // history with two empty maps. Nothing should reach here — `Harness::task`
    // hydrates first, and every save path goes through it — but a silent
    // rewrite of the user's history is the worst thing this program can do, so
    // the backstop is a hard refusal rather than a best-effort merge.
    if !t.hydrated {
        eprintln!(
            "[openleash] refused to save {}: its subagent history has not been loaded. \
             This is a bug — the write was skipped rather than truncate the file.",
            t.id
        );
        return;
    }
    if let Ok(j) = serde_json::to_string(t) {
        save_json(&t.id, &j);
    }
}

/// Fallible task persistence for operations that must not acknowledge success
/// until the atomic replacement lands. Existing best-effort save callers keep
/// their behavior; notices can instead roll back their mutation on an error.
pub fn try_save_task(t: &Task) -> Result<(), String> {
    if !t.hydrated {
        return Err(format!(
            "refused to save {}: subagent history is not loaded",
            t.id
        ));
    }
    let json = serde_json::to_string(t).map_err(|e| format!("serializing task {}: {e}", t.id))?;
    try_write_atomic(&task_path(&t.id), &json).map_err(|e| e.to_string())
}

/// Write a task's JSON. Split out from `save_task` so the debounced writer can
/// do the serialize itself, off the async runtime and holding the task lock
/// only that long, and hand the result here to write.
pub fn save_json(id: &str, json: &str) {
    write_atomic(&task_path(id), json);
}

pub fn delete_task(id: &str) {
    let _ = std::fs::remove_file(task_path(id));
}

/// Where a task's file lives. One place, so the loader and the hydrator cannot
/// disagree about it.
pub fn task_path(id: &str) -> PathBuf {
    data_dir().join("tasks").join(format!("{id}.json"))
}

/// Delete the temp files an interrupted `try_write_atomic` left in the tasks
/// folder.
///
/// A task file can be hundreds of megabytes, so the window between
/// `File::create(&tmp)` and `rename` is seconds long, and this is a desktop app
/// that gets killed, crashes and runs out of disk. The temp is removed on the
/// error path, but a process that dies inside that window leaves it behind
/// forever: nothing ever renames or rereads a dotfile, so those bytes are pure
/// loss — the real dataset this was measured on carried 886 MB of them.
///
/// Runs from `load_tasks`, which the app calls once at startup *before* any
/// writer task is registered, so there is no in-flight write to mistake for an
/// orphan. The name pattern is matched exactly rather than deleting every
/// dotfile, so a `.gitkeep` or the user's own hidden file survives.
pub fn sweep_temp_files() {
    let dir = data_dir().join("tasks");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in rd.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if is_temp_name(&name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether a file name is one `try_write_atomic` writes: `.<name>.<id>.tmp`.
///
/// Both parts are hex, so the shape cannot collide with a real task file (those
/// are `<id>.json`) or with an unrelated hidden file.
fn is_temp_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(".tmp") else {
        return false;
    };
    match rest.rsplit_once('.') {
        Some((_, id)) => !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

/// Read one task file with the heavy fields skipped. See `Task::hydrated`.
pub fn read_task_header(path: &Path) -> Result<Task, String> {
    let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_task(&s, false)
}

/// Read one task file in full, including `sub_items`/`sub_msgs`.
pub fn read_task_full(path: &Path) -> Result<Task, String> {
    let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_task(&s, true)
}

/// Deserialize a task file, optionally leaving the two subagent fields behind.
///
/// The header pass keeps `items`, `messages`, `touched`, `read_files`,
/// `checkpoints` and `subs` fully populated, so the review view, rewind,
/// checkpointing and `task_get` need no hydration at all — only the two
/// subagent maps do.
fn parse_task(s: &str, full: bool) -> Result<Task, String> {
    // `HEAVY_MODE` is thread-local and these files are parsed on a thread pool,
    // so it must be set on the thread that runs serde — not around the call.
    let mut t: Task = HEAVY_MODE.with(|m| {
        m.set(full);
        let out = if full {
            serde_json::from_str::<Task>(s).map_err(|e| e.to_string())
        } else {
            serde_json::from_str::<RawTask>(s)
                .map(RawTask::into_task)
                .map_err(|e| e.to_string())
        };
        m.set(true);
        out
    })?;
    t.hydrated = full;
    Ok(t)
}

/// `Task` with the two subagent fields intercepted.
///
/// The deserializers dispatch on `HEAVY_MODE`, a thread-local set by
/// `parse_task`, rather than `Task` carrying a `deserialize_with` that would
/// force *every* read — including hydration itself — through the slow path.
/// One struct definition, two parse modes, and a new field cannot be added to
/// `Task` without being added here too: `RawTask` flattens `Task`, so serde
/// rejects an unknown field rather than dropping it.
#[derive(serde::Deserialize)]
struct RawTask {
    #[serde(default, deserialize_with = "heavy_or_skip")]
    sub_items: HashMap<String, Vec<Item>>,
    #[serde(default, deserialize_with = "heavy_or_skip")]
    sub_msgs: HashMap<String, Vec<Message>>,
    #[serde(flatten)]
    rest: Task,
}

thread_local! {
    /// True = materialise the subagent maps, false = skip their bytes.
    static HEAVY_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

fn heavy_or_skip<'de, D, T>(d: D) -> Result<HashMap<String, T>, D::Error>
where
    D: serde::de::Deserializer<'de>,
    T: serde::de::Deserialize<'de>,
{
    if HEAVY_MODE.with(|m| m.get()) {
        HashMap::deserialize(d)
    } else {
        serde::de::IgnoredAny::deserialize(d)?;
        Ok(HashMap::default())
    }
}

impl RawTask {
    fn into_task(self) -> Task {
        let RawTask {
            sub_items,
            sub_msgs,
            mut rest,
        } = self;
        rest.sub_items = sub_items;
        rest.sub_msgs = sub_msgs;
        rest
    }
}

/// Load every task's header. Cheap enough to run before the first window paints.
///
/// Parsing runs on a thread pool: this is the app's single largest startup cost
/// (2.5 GB of JSON over 190 files, of which the header pass touches ~6%), and it
/// is pure CPU, so it scales with cores until the disk saturates. Serial, the
/// same work is 14 seconds on a real dataset; skipping alone gets it to ~3,
/// skipping plus the pool lands well under one.
pub fn load_tasks() -> Vec<Task> {
    sweep_temp_files();
    let paths: Vec<PathBuf> = std::fs::read_dir(data_dir().join("tasks"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return vec![];
    }
    let pool = pool();
    let mut out: Vec<Task> = pool.install(|| {
        paths
            .par_iter()
            .filter_map(|p| read_task_header(p).ok())
            .map(repair_loaded)
            .collect()
    });
    out.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    out
}

/// A thread pool sized to the machine, created once.
///
/// Capped at 8: beyond that the files are big enough that the pool is waiting on
/// the disk rather than the CPU, and 8 concurrent readers already saturate it.
/// Oversubscribing a spinning disk makes the 14 seconds worse, not better.
/// The shared worker pool: `load_tasks` fans out across it at boot, and a
/// single chat's body is parsed on it when that chat is opened.
pub fn pool() -> &'static rayon::ThreadPool {
    use std::sync::OnceLock;
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(
                std::thread::available_parallelism()
                    .map(|n| n.get().min(8))
                    .unwrap_or(4),
            )
            .build()
            .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap())
    })
}

/// Everything `load_tasks` does to a freshly-parsed task, apart from the
/// subagent transcript repair that needs a hydrated body.
///
/// Split out of the reader so it runs per-file on the pool.
fn repair_loaded(mut t: Task) -> Task {
    // The project instructions (`AGENTS.md` and friends) are part of
    // the frozen system prompt, so a prompt saved before the user
    // edited those files is stale. Drop it here and it is rebuilt
    // from the files as they are on the next request. A chat created
    // fresh has nothing to drop and is unaffected.
    t.system.clear();
    t.mcp_tools.clear();
    t.plugins = Default::default();
    // `explore` + `general` are always on: repair tasks saved before that.
    ensure_required_agents(&mut t.agents);
    if !t.agents.is_empty() {
        t.subagents = true;
    }
    // Anything mid-flight when the app closed is no longer running.
    let crashed = t.status == "running" || t.status == "waiting";
    // `t.subs` is in the header, so this repair still runs at boot; the matching
    // repair of each subagent's *conversation* is not, because `sub_msgs` is
    // still on disk. `hydrate` does it instead — see `store::hydrate_task`.
    for sub in t.subs.iter_mut().filter(|x| x.status == "running") {
        sub.status = "stopped".into();
    }
    if crashed {
        super::runner::fix_dangling_with(&mut t.messages, super::runner::CRASHED);
        // Tool pills still spinning in the UI: mark them cut off.
        for i in t
            .items
            .iter_mut()
            .filter(|i| i.kind == "tool" && i.data["status"] == "running")
        {
            i.data["status"] = serde_json::json!("error");
            i.data["meta"] = serde_json::json!("cut off by crash");
        }
    }
    // Background commands died with the app: tell the agent to restart them.
    if !t.bg_live.is_empty() {
        let list = t.bg_live.join(", ");
        let note = format!("The app closed or crashed while these background commands were running; they were killed and did NOT complete. Start them again if you still need them: {list}");
        t.stop_note = Some(match t.stop_note.take() {
            Some(n) => format!(
                "{n}
<system-reminder>{note}</system-reminder>"
            ),
            None => format!("<system-reminder>{note}</system-reminder>"),
        });
        t.items.push(Item::new(
            "notice",
            format!(
                "App closed mid-run · {} background command{} stopped, the agent will rerun {}",
                t.bg_live.len(),
                if t.bg_live.len() == 1 { "" } else { "s" },
                if t.bg_live.len() == 1 { "it" } else { "them" }
            ),
            serde_json::json!({"level": "stopped"}),
        ));
        t.bg_live.clear();
    }
    if t.status == "running" || t.status == "waiting" {
        // Closed or crashed mid-run: treat it as paused so Resume picks it up.
        t.status = "stopped".into();
        t.waiting_kind = None;
        t.step = "Paused when the app closed".into();
        // Any pause becomes a "closed" one: after a restart only the user resumes it
        // (an `exhausted` pause would be auto-woken by the account watcher on its first tick).
        let reason = match &t.paused {
            Some(p) if p.kind != "closed" => {
                format!("The app closed while this was paused ({})", p.reason)
            }
            Some(p) => p.reason.clone(),
            None => "The app closed while this was running".into(),
        };
        t.paused = Some(super::Pause {
            reason,
            kind: "closed".into(),
            since: chrono::Utc::now(),
        });
    }
    if let Some(p) = t.paused.as_mut().filter(|p| p.kind == "exhausted") {
        p.reason = format!("The app closed while this was paused ({})", p.reason);
        p.kind = "closed".into();
    }
    // Chats saved before `touched_at` existed have no user-activity
    // time. Fall back to when they were last changed, so the list
    // opens in roughly the order it had, instead of every chat
    // jumping to the top the first time this runs.
    if t.touched_at.timestamp() <= 0 {
        t.touched_at = t.updated_at;
    }
    t
}

/// Repair a subagent conversation truncated by a crash, once its body is loaded.
///
/// The boot pass used to do this for every task. It cannot any more: `sub_msgs`
/// is skipped at boot, so there is nothing to repair yet. It runs here instead,
/// before the conversation is spliced in, which is the last point at which the
/// file on disk is the only copy — a chat that is never opened never needs the
/// repair, and a chat that is opened gets it before the model ever sees the
/// history.
///
/// The repair is idempotent and only ever *adds* the missing `tool_result`
/// blocks, so a task that was hydrated by an earlier launch (and saved) is
/// unaffected on the next one.
pub fn hydrate_task(t: &mut Task) {
    if t.hydrated {
        return;
    }
    for sub in t.subs.iter().filter(|x| x.status == "stopped") {
        if let Some(m) = t.sub_msgs.get_mut(&sub.id) {
            super::runner::fix_dangling_with(m, super::runner::CRASHED);
        }
    }
    t.hydrated = true;
}

/// Write a file so a crash mid-write cannot corrupt it: serialize to a
/// uniquely-named temp file in the *same directory* (rename is only atomic
/// within a filesystem), flush it to the platter, then rename over the target.
///
/// Three things this deliberately does that the obvious `fs::write` + `fs::rename`
/// does not:
///   * `sync_all` on the temp before the rename, and on the directory after.
///     Without it a crash can leave a zero-length `settings.json` that
///     `load_settings` would read as "no settings", silently dropping every API
///     key, OAuth token and preference the user had.
///   * The rename result is reported instead of discarded. It used to be
///     `let _ = ...`, which turned a failed save into silent data loss.
///   * The temp name is unique. `with_extension("tmp")` turns `tasks/abc.json`
///     into `tasks/abc.tmp`, so two writers on neighbouring ids could clobber
///     each other's half-written file.
///
/// Best effort: a caller that cannot take a `Result` still gets a warning.
pub fn write_atomic(path: &PathBuf, content: &str) {
    if let Err(e) = try_write_atomic(path, content) {
        eprintln!("[openleash] could not save {}: {e}", path.display());
    }
}

pub fn try_write_atomic(path: &PathBuf, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "out".into());
    // Same-directory temp so the rename stays atomic, and unique so concurrent
    // writers cannot collide on it.
    let tmp = dir.join(format!(".{name}.{}.tmp", new_id()));

    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, content.as_bytes())?;
        // Get the bytes on disk before the rename makes them reachable under
        // the real name.
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();

    if let Err(e) = &result {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::new(
            e.kind(),
            format!("writing {}: {e}", path.display()),
        ));
    }
    // Best effort: the rename already happened. Not every platform lets you
    // open a directory for fsync, and a failure here means a durability
    // downgrade, not a lost write.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

// ───────────────────────────── subagent definitions ─────────────────────────────

/// Built-in subagents that are always available: they can't be deleted and
/// can't be toggled off (per-chat or as new-chat defaults).
pub const REQUIRED_AGENTS: &[&str] = &["explore", "general"];

/// Add any missing required agent id, preserving existing order.
pub fn ensure_required_agents(ids: &mut Vec<String>) {
    for r in REQUIRED_AGENTS {
        if !ids.iter().any(|x| x == r) {
            ids.push(r.to_string());
        }
    }
}

/// Same as `ensure_required_agents`, but takes ownership.
pub fn with_required_agents(mut ids: Vec<String>) -> Vec<String> {
    ensure_required_agents(&mut ids);
    ids
}

pub fn builtin_agents() -> Vec<AgentDef> {
    vec![
        AgentDef {
            id: "explore".into(),
            name: "Explore".into(),
            description: "Fast read-only search: find where things live, trace a flow across files, answer questions about the code.".into(),
            prompt: "You have read-only tools. Search efficiently: start broad with glob/grep, then read only what matters, and batch independent calls in one response. Report exact file paths with line numbers.".into(),
            model: String::new(),
            tools: "read_only".into(),
            color: "#33d6ff".into(),
            builtin: true,
            source: String::new(),
            inject_instructions: true,
            effort: None,
            groups: vec![],
        },
        AgentDef {
            id: "general".into(),
            name: "General".into(),
            description: "Does a self-contained piece of work end to end: edits files, runs commands, verifies.".into(),
            prompt: "Complete the job fully and verify it (build/tests) before reporting.".into(),
            model: String::new(),
            tools: "all".into(),
            color: "#a78bfa".into(),
            builtin: true,
            source: String::new(),
            inject_instructions: true,
            effort: None,
            groups: vec![],
        },
    ]
}

/// Ultrathread worktrees only: merges the workers' branches back into the main checkout.
pub fn fuze_agent() -> AgentDef {
    AgentDef {
        id: "fuze".into(),
        name: "Fuze".into(),
        description: "Ultrathread worktrees: merges finished worker branches into the main checkout, resolves conflicts, and verifies the combined build.".into(),
        prompt: "You are the FUZE agent. You work in the main checkout (the task's working directory). Your brief lists worker branches. For each: `git merge --no-ff <branch>` (commit or stash anything uncommitted in the main checkout first, and say so). Resolve conflicts by reading both sides and keeping the intent of both — never blindly take one side, never drop a worker's change without saying why. After merging, build and run the tests; fix integration breakage yourself. Once a branch is merged cleanly, remove its worktree with `git worktree remove --force <path>` — `--force` is required, because a worktree carries ignored files copied into it at setup (`.env`, `node_modules`) and plain `git worktree remove` refuses over untracked files. The branch is kept either way. A brief that marks a branch COULD NOT COMMIT means its changes are still uncommitted in that worktree: commit them there first (`git -C <path> add -A && git -C <path> commit -m ...`), or merge the worktree's files by hand. Report: merged branches, conflicts and how you resolved them, build/test results, anything you couldn't merge.".into(),
        model: String::new(),
        tools: "all".into(),
        color: "#f472b6".into(),
        builtin: true,
        source: String::new(),
        inject_instructions: true,
        effort: None,
        groups: vec![],
    }
}

/// Parse the `groups:` block of an agent file into `ToolGroup`s.
///
/// `groups` is a heterogeneous sequence — plain names where the group is
/// unrestricted, `[name, {fileRegex, description}]` where it is not:
///
/// ```text
/// groups:
///   - read
///   - edit
///   - [edit, {fileRegex: "\.(md|mdx)$", description: "docs only"}]
/// ```
///
/// The house frontmatter parser is line-based and one-level, so this is a tiny
/// dedicated reader over the same lines rather than a YAML dependency. It tracks
/// the `groups:` header, then each `- ` item, accepting both `- read` and
/// `- [read, …]` for the plain form (people write both, and Roo's docs show
/// both). A malformed line is skipped, not fatal: an agent file that fails to
/// parse at all would be worse than one missing a group.
fn parse_agent_groups(front: &str) -> Vec<ToolGroup> {
    let mut groups: Vec<ToolGroup> = vec![];
    let mut in_groups = false;
    // Lines collected as (the `- …` item, its indented continuation lines).
    let mut pending: Option<(String, Vec<String>)> = None;
    for line in front.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        // The `groups:` header itself (any indent) opens the block.
        if let Some(rest) = t.strip_prefix("groups:") {
            if let Some((item, cont)) = pending.take() {
                if let Some(g) = finish_group_item(&item, &cont) {
                    groups.push(g);
                }
            }
            let inline = rest.trim();
            // `groups: [read, edit]` — accept the flow form too, one line.
            if inline.starts_with('[') {
                for item in split_flow_items(inline) {
                    if let Some(g) = parse_group_item(&item) {
                        groups.push(g);
                    }
                }
            }
            in_groups = true;
            continue;
        }
        if !in_groups {
            continue;
        }
        // A top-level key (indent 0, not a list item) ends the block.
        if indent == 0 && !t.starts_with('-') {
            if let Some((item, cont)) = pending.take() {
                if let Some(g) = finish_group_item(&item, &cont) {
                    groups.push(g);
                }
            }
            in_groups = false;
            continue;
        }
        if let Some(rest) = t.strip_prefix("- ") {
            if let Some((item, cont)) = pending.take() {
                if let Some(g) = finish_group_item(&item, &cont) {
                    groups.push(g);
                }
            }
            pending = Some((rest.trim().to_string(), vec![]));
        } else if let Some((_, cont)) = pending.as_mut() {
            // An indented `key: value` under the last `- ` item.
            cont.push(t.to_string());
        }
    }
    if let Some((item, cont)) = pending.take() {
        if let Some(g) = finish_group_item(&item, &cont) {
            groups.push(g);
        }
    }
    groups
}

/// Turn a collected `- …` item plus its continuation lines into a `ToolGroup`.
/// A bare name with an indented `fileRegex:` under it is the block-sequence
/// spelling of Roo's `[edit, {fileRegex: …}]` tuple.
fn finish_group_item(item: &str, cont: &[String]) -> Option<ToolGroup> {
    if cont.is_empty() {
        return parse_group_item(item);
    }
    let mut file_regex = String::new();
    let mut description = String::new();
    for l in cont {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = unquote_yaml(v);
        match k.trim().to_lowercase().as_str() {
            "fileregex" | "file_regex" | "file-regex" => file_regex = v,
            "description" => description = v,
            _ => {}
        }
    }
    if file_regex.is_empty() && description.is_empty() {
        return parse_group_item(item);
    }
    Some(ToolGroup::restricted(
        item.trim()
            .trim_matches('[')
            .trim_end_matches(']')
            .trim()
            .to_string(),
        file_regex,
        description,
    ))
}

/// Split a flow sequence body (`[a, b, [c, {d: e}]]`) on top-level commas.
fn split_flow_items(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = vec![];
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// One `groups:` item → `ToolGroup`. Handles `read`, `[edit, {…}]`, and the
/// block-sequence spelling some YAML writers emit for the same tuple shape:
/// ```text
///   - edit
///     fileRegex: \.md$
/// ```
/// The caller flattens block tuples before this is called.
fn parse_group_item(item: &str) -> Option<ToolGroup> {
    let item = item.trim();
    if item.is_empty() {
        return None;
    }
    if let Some(inner) = item.strip_prefix('[') {
        let inner = inner.trim_end_matches(']');
        let mut it = split_flow_items(inner).into_iter();
        let name = it.next()?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let opts = it.next().unwrap_or_default();
        let (file_regex, description) = parse_group_opts(&opts);
        // `[edit]` with no options is just an unrestricted group.
        if file_regex.is_empty() && description.is_empty() {
            return Some(ToolGroup::Plain(name));
        }
        return Some(ToolGroup::restricted(name, file_regex, description));
    }
    // A YAML key line that leaked in as its own item is not a group name.
    if item.contains(':') {
        return None;
    }
    Some(ToolGroup::Plain(item.to_string()))
}

/// Pull `fileRegex` / `description` out of an options object, tolerating no
/// braces and quoted values.
fn parse_group_opts(opts: &str) -> (String, String) {
    let mut file_regex = String::new();
    let mut description = String::new();
    let body = opts.trim().trim_start_matches('{').trim_end_matches('}');
    for part in split_flow_items(body) {
        let Some((k, v)) = part.split_once(':') else {
            continue;
        };
        let v = unquote_yaml(v);
        match k.trim().to_lowercase().as_str() {
            "fileregex" | "file_regex" | "file-regex" => file_regex = v,
            "description" => description = v,
            _ => {}
        }
    }
    (file_regex, description)
}

/// Unquote a YAML scalar: strip one layer of matched quotes and resolve the
/// escapes that matter for a regex. `"\.(md)$"` and `'\.(md)$'` both have to come
/// back as `\.(md)$`, and a double-quoted `"\\.(md|mdx)$"` (which is what the
/// brief and Roo's docs write) has to become `\.(md|mdx)$` — leaving the doubled
/// backslash in would hand `regex` a pattern that matches a literal backslash,
/// i.e. a restriction that silently never fires.
fn unquote_yaml(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                out.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '0' => '\0',
                    // `\\` and `\"` (and any other escaped char) collapse to the
                    // char itself; that is what makes `\\.` one backslash.
                    other => other,
                });
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

/// `---\nkey: value\n---\nbody` agent files (Claude Code / opencode style).
pub(crate) fn parse_agent_md(text: &str, file_stem: &str) -> Option<AgentDef> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---")?;
    let (front, body) = rest.split_once("\n---")?;
    let mut d = AgentDef {
        id: file_stem.to_lowercase().replace(' ', "-"),
        tools: "all".into(),
        source: "project".into(),
        color: "#fbbf24".into(),
        inject_instructions: true,
        ..Default::default()
    };
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = unquote_yaml(v);
        match k.trim().to_lowercase().as_str() {
            "name" => {
                d.id = v.to_lowercase().replace(' ', "-");
                d.name = v;
            }
            "description" => d.description = v,
            "model" if v != "inherit" => d.model = v,
            "tools" => {
                let l = v.to_lowercase();
                d.tools = if l.contains("read")
                    && !l.contains("write")
                    && !l.contains("edit")
                    && !l.contains("bash")
                {
                    "read_only".into()
                } else if !l.contains("bash") && (l.contains("edit") || l.contains("write")) {
                    "no_shell".into()
                } else {
                    "all".into()
                };
            }
            "color" => d.color = v,
            // Claude Code style names, or our 0-4 scale.
            "effort" | "reasoning" | "reasoning_effort" => {
                d.effort = match v.to_lowercase().as_str() {
                    "max" | "maximum" => Some(0),
                    "xhigh" | "extra high" | "extra-high" => Some(1),
                    "high" => Some(2),
                    "medium" => Some(3),
                    "low" | "minimal" => Some(4),
                    n => n.parse::<usize>().ok().map(|n| n.min(4)),
                }
            }
            "inject_instructions" | "instructions" => {
                d.inject_instructions = !matches!(v.to_lowercase().as_str(), "false" | "no" | "off")
            }
            _ => {}
        }
    }
    d.groups = parse_agent_groups(front);
    d.prompt = body.trim_start_matches('-').trim().to_string();
    if d.name.is_empty() {
        d.name = d.id.clone();
    }
    Some(d)
}

/// Built-ins (overridable), user agents from settings, then — only when the
/// project folder is trusted — the project's `.openleash/agents/*.md`.
///
/// The trust gate lives here rather than at the call site on purpose: this is
/// the one function every agent-list consumer goes through, so a new caller
/// cannot accidentally reintroduce project agents from an untrusted folder.
pub fn all_agents(s: &Settings, project: &str) -> Vec<AgentDef> {
    let mut out = builtin_agents();
    for a in &s.agents {
        match out.iter_mut().find(|x| x.id == a.id) {
            Some(x) => {
                let builtin = x.builtin;
                *x = a.clone();
                x.builtin = builtin;
            }
            None => out.push(a.clone()),
        }
    }
    if !project.is_empty() && crate::agent::trust::trusted(&s.trust, project) {
        for (d, _) in read_project_agents(project) {
            if !out.iter().any(|x| x.id == d.id) {
                out.push(d);
            }
        }
    }
    out
}

/// The project's own agent definitions, each with the file it was parsed from.
/// Untrusted callers must go through `all_agents`, which applies the gate.
pub fn read_project_agents(project: &str) -> Vec<(AgentDef, std::path::PathBuf)> {
    let mut out = vec![];
    // A remote (SSH) project's agent files are on its host and are not
    // loaded (phase 2A); the URI must never be joined into a local path.
    if project.is_empty() || super::host::is_ssh(project) {
        return out;
    }
    for dir in [".openleash/agents", ".claude/agents", ".opencode/agent"] {
        if let Ok(rd) = std::fs::read_dir(std::path::Path::new(project).join(dir)) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let stem = p
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or("agent")
                    .to_string();
                if let Some(d) = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|t| parse_agent_md(&t, &stem))
                {
                    if !out.iter().any(|(x, _)| x.id == d.id) {
                        out.push((d, p));
                    }
                }
            }
        }
    }
    out
}

/// Project skill directories (checked in order). `.openleash/skills` is
/// native; `.claude/skills` is read for Claude Code compatibility.
fn project_skill_dirs(project: &str) -> Vec<std::path::PathBuf> {
    // Remote (SSH) project skills are not loaded; see `read_project_agents`.
    if project.is_empty() || super::host::is_ssh(project) {
        return vec![];
    }
    let root = std::path::Path::new(project);
    [".openleash/skills", ".claude/skills", ".agents/skills"]
        .iter()
        .map(|d| root.join(d))
        .collect()
}

// ───────────────────────────── skills ─────────────────────────────

/// Where imported skills live: `~/.openleash/skills/<name>/SKILL.md`.
pub fn skills_dir() -> PathBuf {
    let d = data_dir().join("skills");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn valid_skill_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Normalize a skill name: lowercase, spaces/underscores to dashes,
/// everything else dropped. Returns "" when nothing usable remains.
pub fn normalize_skill_name(n: &str) -> String {
    let mut out = String::new();
    for c in n.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            out.push(c);
        } else if c == ' ' || c == '_' {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

/// Frontmatter of a `SKILL.md`, plus whether an explicit name was given.
///
/// The two invocation booleans are the point: Claude Code and Crush separate
/// "the model may reach for this" from "the user can call it", so a
/// `deploy-to-production` skill can exist without the model ever reaching for
/// it unprompted — and, with `disable-model-invocation`, without costing a line
/// of context until it *is* invoked.
#[derive(Debug, Clone, Default)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    /// `disable-model-invocation: true` — hidden from the model's skill list
    /// entirely, so it never enters the system prompt. Absent = false, which is
    /// what every `SKILL.md` written before this field did.
    pub disable_model_invocation: bool,
    /// `user-invocable: true` — shows in the user's command palette. Absent =
    /// false: today no skill is in a palette, and this changes nothing until a
    /// skill opts in (or sets `disable-model-invocation`, see `resolve_invocation`).
    pub user_invocable: bool,
}

/// Frontmatter booleans accept the spellings people actually write. An absent
/// value never reaches here; an explicit one is true unless it is clearly off,
/// matching how `inject_instructions` is read elsewhere in this file.
fn frontmatter_bool(v: &str) -> bool {
    !matches!(
        v.trim().to_lowercase().as_str(),
        "false" | "no" | "off" | "0"
    )
}

/// The two invocation axes after defaults are applied.
///
/// `disable-model-invocation: true` implies user-invocable: the flag exists to
/// keep a skill out of the *model's* reach while the user can still call it, so
/// such a skill belongs in the palette rather than being unreachable by both.
/// With neither flag written, both come back false — i.e. exactly today's
/// behaviour: visible to the model, no palette entry.
pub fn resolve_invocation(m: &SkillMeta) -> (bool, bool) {
    let disable = m.disable_model_invocation;
    let palette = m.user_invocable || disable;
    (disable, palette)
}

/// Whether the model may be told about this skill at all. This is the single
/// predicate the system-prompt filter must use (see `prompt.rs`).
pub fn skill_model_visible(d: &SkillDef) -> bool {
    !d.disable_model_invocation
}

/// Parse a `SKILL.md` file: `---\nname: x\ndescription: y\n---\nbody`.
/// A missing `name` falls back to the directory/file stem; a missing/empty
/// `description` is an error since the agent uses it to decide when the skill
/// applies. `None` for anything that isn't a usable skill.
pub fn parse_skill_md(text: &str, fallback_name: &str) -> Option<SkillMeta> {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let rest = text.strip_prefix("---")?;
    // Frontmatter ends at a line that is exactly `---` (trailing content after is the body).
    let mut end = None;
    let mut pos = 0;
    for line in rest.lines() {
        // +1 for the stripped newline.
        pos += line.len() + 1;
        if line.trim() == "---" {
            end = Some(pos);
            break;
        }
    }
    let end = end?;
    let front = &rest[..rest.len().min(end)];
    // Re-derive front without the closing line: everything before the last `---` line.
    let front = front.trim_end().strip_suffix("---").unwrap_or(front).trim();
    let mut name = String::new();
    let mut description = String::new();
    let mut disable_model_invocation = false;
    let mut user_invocable = false;
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            // Explicit names are strict (validated as-is); only the fallback
            // directory/file stem gets normalized below.
            "name" => name = v.trim().to_string(),
            "description" => description = v,
            // Claude Code's spelling first, then the snake/camel variants people
            // reach for out of habit.
            "disable-model-invocation" | "disable_model_invocation" | "disableModelInvocation" => {
                disable_model_invocation = frontmatter_bool(&v)
            }
            "user-invocable" | "user_invocable" | "userInvocable" => {
                user_invocable = frontmatter_bool(&v)
            }
            _ => {}
        }
    }
    if name.is_empty() {
        name = normalize_skill_name(fallback_name);
    }
    if !valid_skill_name(&name) || description.trim().is_empty() {
        return None;
    }
    Some(SkillMeta {
        name,
        description: description.trim().to_string(),
        disable_model_invocation,
        user_invocable,
    })
}

/// The most directory entries [`count_skill_files`] looks at, and how deep it
/// goes. The count is a label in the settings list; a skill folder that
/// carries a `node_modules` (or links back to an ancestor, which `is_dir`
/// follows) would otherwise make listing skills walk an unbounded tree.
const SKILL_COUNT_ENTRIES: usize = 2_000;
const SKILL_COUNT_DEPTH: usize = 6;

fn count_skill_files(dir: &std::path::Path) -> usize {
    fn walk(dir: &std::path::Path, depth: usize, budget: &mut usize, n: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let p = e.path();
            if p.is_dir() {
                if depth < SKILL_COUNT_DEPTH {
                    walk(&p, depth + 1, budget, n);
                }
            } else if p.file_name().and_then(|x| x.to_str()) != Some("SKILL.md") {
                *n += 1;
            }
        }
    }
    let (mut budget, mut n) = (SKILL_COUNT_ENTRIES, 0);
    walk(dir, 0, &mut budget, &mut n);
    n
}

fn read_skill_dir(dir: &std::path::Path, source: &str, count_files: bool) -> Vec<SkillDef> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let d = e.path();
        if !d.is_dir() {
            continue;
        }
        let md = d.join("SKILL.md");
        if !md.is_file() {
            continue;
        }
        let stem = d
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("skill")
            .to_string();
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        if let Some(m) = parse_skill_md(&text, &stem) {
            let (disable, palette) = resolve_invocation(&m);
            out.push(SkillDef {
                name: m.name,
                description: m.description,
                source: source.into(),
                path: md.to_string_lossy().to_string(),
                enabled: true,
                files: if count_files {
                    count_skill_files(&d)
                } else {
                    0
                },
                disable_model_invocation: disable,
                user_invocable: palette,
            });
        }
    }
    out
}

/// User skills plus the project's skills — but the project's only when the
/// folder is trusted. A project skill with the same name wins over the user one.
/// Disabled skills stay listed with `enabled = false` so the UI can show them
/// dimmed.
///
/// The gate is here, not at the call sites, for the same reason as `all_agents`:
/// this is the single door into the skills list, and a project skill's
/// description is injected into the system prompt.
pub fn all_skills(s: &Settings, project: &str) -> Vec<SkillDef> {
    skills_in(&SkillScope::of(s, project), project, true)
}

/// The settings [`all_skills`] depends on, lifted out so the disk work can run
/// after the settings lock is released (and off the async runtime): every
/// model request reads settings, and a skills folder can be large.
#[derive(Debug, Clone)]
pub struct SkillScope {
    trusted: bool,
    disabled: Vec<String>,
}

impl SkillScope {
    pub fn of(s: &Settings, project: &str) -> Self {
        SkillScope {
            trusted: crate::agent::trust::trusted(&s.trust, project),
            disabled: s.disabled_skills.clone(),
        }
    }
}

/// [`all_skills`] from a lifted [`SkillScope`]. `count_files = false` leaves
/// `files` at 0: the system prompt never shows the count, so building one does
/// not walk every skill's folder for it.
pub fn skills_in(scope: &SkillScope, project: &str, count_files: bool) -> Vec<SkillDef> {
    let mut out = read_skill_dir(&skills_dir(), "user", count_files);
    if scope.trusted {
        for dir in project_skill_dirs(project) {
            for d in read_skill_dir(&dir, "project", count_files) {
                match out.iter_mut().find(|x| x.name == d.name) {
                    Some(x) => *x = d,
                    None => out.push(d),
                }
            }
        }
    }
    for x in out.iter_mut() {
        x.enabled = !scope.disabled.contains(&x.name);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The project's own skills, ignoring the trust gate. Only the trust manifest
/// and the settings preview call this: everything agent-facing goes through
/// `all_skills` so an untrusted folder's skills never reach a prompt.
pub fn read_project_skills(project: &str) -> Vec<SkillDef> {
    let mut out: Vec<SkillDef> = vec![];
    for dir in project_skill_dirs(project) {
        for d in read_skill_dir(&dir, "project", true) {
            match out.iter_mut().find(|x| x.name == d.name) {
                Some(x) => *x = d,
                None => out.push(d),
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read a skill's full `SKILL.md` (truncated) plus the relative paths of
/// its bundled files, for the settings preview.
pub fn read_skill(
    s: &Settings,
    project: &str,
    name: &str,
) -> Option<(SkillDef, String, Vec<String>)> {
    let def = all_skills(s, project)
        .into_iter()
        .find(|x| x.name == name)?;
    let text: String = std::fs::read_to_string(&def.path)
        .ok()?
        .chars()
        .take(20_000)
        .collect();
    let mut files = vec![];
    if let Some(dir) = std::path::Path::new(&def.path).parent() {
        collect_skill_files(dir, dir, &mut files);
        files.sort();
        files.truncate(50);
    }
    Some((def, text, files))
}

fn collect_skill_files(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_skill_files(base, &p, out);
            } else if out.len() < 200 {
                out.push(
                    p.strip_prefix(base)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if s.is_dir() {
            copy_dir_all(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

fn find_skill_md(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for n in ["SKILL.md", "skill.md", "Skill.md"] {
        let p = dir.join(n);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Import a skill from a file (`SKILL.md` / any `.md`) or a directory
/// containing `SKILL.md`. Directories are copied whole (bundled resources
/// included); single files become `<name>/SKILL.md`. Overwrites a user
/// skill with the same name.
pub fn import_skill(src: &str) -> Result<SkillDef, String> {
    let src_p = std::path::Path::new(src.trim());
    if !src_p.exists() {
        return Err("That path doesn't exist.".into());
    }
    let (md_path, from_dir) = if src_p.is_dir() {
        (
            find_skill_md(src_p).ok_or_else(|| "That folder has no SKILL.md in it.".to_string())?,
            true,
        )
    } else {
        if src_p
            .extension()
            .and_then(|x| x.to_str())
            .map(|e| e.to_ascii_lowercase())
            != Some("md".into())
        {
            return Err("Pick a SKILL.md file or a folder containing one.".into());
        }
        (src_p.to_path_buf(), false)
    };
    let text = std::fs::read_to_string(&md_path)
        .map_err(|e| format!("Couldn't read {}: {e}", md_path.display()))?;
    let fallback = if from_dir {
        src_p
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("skill")
            .to_string()
    } else {
        // A file named SKILL.md takes the parent folder's name; any other
        // `.md` file takes the file stem.
        if md_path
            .file_name()
            .and_then(|x| x.to_str())
            .map(|n| n.to_ascii_lowercase())
            == Some("skill.md".into())
        {
            md_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|x| x.to_str())
                .unwrap_or("skill")
                .to_string()
        } else {
            md_path
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or("skill")
                .to_string()
        }
    };
    let Some(m) = parse_skill_md(&text, &fallback) else {
        return Err("That file isn't a skill: it needs frontmatter with a name (lowercase letters, numbers, dashes) and a non-empty description, e.g.\n---\nname: my-skill\ndescription: Does X, use when Y\n---".into());
    };
    let name = m.name.clone();
    let (disable, palette) = resolve_invocation(&m);
    let dest = skills_dir().join(&name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .map_err(|e| format!("Couldn't replace the existing '{name}' skill: {e}"))?;
    }
    if from_dir {
        copy_dir_all(src_p, &dest).map_err(|e| format!("Couldn't copy the skill folder: {e}"))?;
        // Normalize the entry file to SKILL.md so discovery always finds it.
        let got = find_skill_md(&dest);
        if got.as_deref() != Some(&dest.join("SKILL.md")) {
            if let Some(g) = got {
                let _ = std::fs::rename(g, dest.join("SKILL.md"));
            }
        }
    } else {
        std::fs::create_dir_all(&dest)
            .map_err(|e| format!("Couldn't create {}: {e}", dest.display()))?;
        std::fs::write(dest.join("SKILL.md"), &text)
            .map_err(|e| format!("Couldn't save the skill: {e}"))?;
    }
    let md = dest.join("SKILL.md");
    Ok(SkillDef {
        name,
        description: m.description.clone(),
        source: "user".into(),
        path: md.to_string_lossy().to_string(),
        enabled: true,
        files: count_skill_files(&dest),
        disable_model_invocation: disable,
        user_invocable: palette,
    })
}

/// Delete a user skill by name. Project skills live in the repo — remove
/// them there instead.
pub fn remove_skill(name: &str) -> Result<(), String> {
    if !valid_skill_name(name) {
        return Err("Unknown skill.".into());
    }
    let dir = skills_dir().join(name);
    if !dir.is_dir() {
        return Err(
            "That skill isn't installed (project skills live in the repo — delete them there)."
                .into(),
        );
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("Couldn't delete the skill: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod task_header_tests {
    use super::*;

    #[test]
    fn new_pull_request_page_is_off_by_default_and_for_old_settings() {
        assert!(!Settings::default().show_pull_requests);
        let old: Settings = serde_json::from_str(
            r#"{"model":"m","effort":1,"perm":"turbo","worktree":false,"subagents":true}"#,
        )
        .unwrap();
        assert!(!old.show_pull_requests);
    }

    #[test]
    fn schedules_page_is_off_by_default_and_for_old_settings() {
        assert!(!Settings::default().show_schedules);
        let old: Settings = serde_json::from_str(
            r#"{"model":"m","effort":1,"perm":"turbo","worktree":false,"subagents":true}"#,
        )
        .unwrap();
        assert!(!old.show_schedules);
        let on: Settings = serde_json::from_str(r#"{"show_schedules":true}"#).unwrap();
        assert!(on.show_schedules);
    }

    #[test]
    fn a_task_without_a_permission_mode_defaults_to_full_access() {
        let mut value =
            serde_json::to_value(crate::agent::tests::task("unset-perm", "test-model")).unwrap();
        value.as_object_mut().unwrap().remove("perm");
        let raw = serde_json::to_string(&value).unwrap();

        for task in [
            parse_task(&raw, false).unwrap(),
            parse_task(&raw, true).unwrap(),
        ] {
            assert_eq!(task.perm, "turbo");
        }
    }

    #[test]
    fn header_matches_string_parser_with_large_skipped_fields() {
        let dir = std::env::temp_dir().join(format!("ol-stream-header-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("task.json");
        let mut value =
            serde_json::to_value(crate::agent::tests::task("header", "test-model")).unwrap();
        // Cross many buffer boundaries, including escaped and multibyte text.
        let text = "transcript \" quoted Ω 😀\n".repeat(100_000);
        value["sub_items"] = serde_json::json!({"sub": [{"text": text}]});
        value["sub_msgs"] = value["sub_items"].clone();
        let raw = serde_json::to_string(&value).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let expected = parse_task(&raw, false).unwrap();
        let actual = read_task_header(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert!(!actual.hydrated);
        assert!(actual.sub_items.is_empty() && actual.sub_msgs.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn header_preserves_normal_and_corrupt_file_behavior() {
        let dir = std::env::temp_dir().join(format!("ol-stream-corrupt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("task.json");
        let raw =
            serde_json::to_string(&crate::agent::tests::task("normal", "test-model")).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let actual = read_task_header(&path).unwrap();
        assert_eq!(actual.id, "normal");
        assert!(!actual.hydrated);
        for corrupt in [
            raw[..raw.len() - 1].to_owned(),
            format!("{raw} true"),
            "{}".into(),
            raw.replace("\"id\":\"normal\"", "\"id\":42"),
        ] {
            std::fs::write(&path, &corrupt).unwrap();
            assert!(parse_task(&corrupt, false).is_err());
            assert!(read_task_header(&path).is_err());
            assert!(
                HEAVY_MODE.with(|m| m.get()),
                "parse errors restore full mode"
            );
        }
        assert!(read_task_header(&dir.join("missing.json")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn header_rejects_invalid_utf8_in_skipped_transcripts() {
        let dir = std::env::temp_dir().join(format!("ol-stream-utf8-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("task.json");
        let mut value =
            serde_json::to_value(crate::agent::tests::task("utf8", "test-model")).unwrap();
        value["sub_msgs"] = serde_json::json!({"sub": "INVALID_BYTE"});
        let mut raw = serde_json::to_vec(&value).unwrap();
        let index = raw.windows(12).position(|w| w == b"INVALID_BYTE").unwrap();
        raw[index] = 0xff;
        std::fs::write(&path, raw).unwrap();
        assert!(std::fs::read_to_string(&path).is_err());
        assert!(read_task_header(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_before_a_field_load_with_the_default() {
        // Older settings files keep the optional cross-chat inbox hidden, while
        // an unset permission mode takes the new Full Access default.
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert_eq!(old.perm, "turbo");
        assert!(!old.needs_you);
        assert!(!old.all_projects);
        // An unset saved-prompt order is the stored one: a settings file written
        // before the field existed must not come back as some other order.
        assert!(old.saved_sort.is_empty());
        let on: Settings = serde_json::from_str(
            r#"{"ui_size":"v2","ui_zoom":110,"needs_you":true,"all_projects":true,"saved_sort":"folder"}"#,
        )
        .unwrap();
        assert!(on.needs_you);
        assert!(on.all_projects);
        assert_eq!(on.saved_sort, "folder");
    }

    #[test]
    fn accounts_saved_before_caps_existed_load_without_any() {
        // A cap is an instruction the user gave by hand, so the only honest
        // reading of a settings file that predates the field is "no caps". The
        // old JSON therefore has to carry no `caps` key for this to prove
        // anything, and the new one has to survive round-tripping with the
        // window label it was set on.
        let old: Settings = serde_json::from_str(
            r#"{"accounts":[{"id":"a1","kind":"codex","label":"ChatGPT 1","priority":10,"enabled":true}]}"#,
        )
        .unwrap();
        assert_eq!(old.accounts.len(), 1);
        assert!(old.accounts[0].caps.is_empty());
        let capped: Settings = serde_json::from_str(
            r#"{"accounts":[{"id":"a1","caps":[{"window":"5h","used":50,"until":0}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            capped.accounts[0].caps,
            vec![UsageCap {
                window: "5h".into(),
                used: 50.0,
                until: 0
            }]
        );
    }

    #[test]
    fn full_access_is_the_default_when_permission_is_unset() {
        assert_eq!(Settings::default().perm, "turbo");

        let older: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert_eq!(older.perm, "turbo");

        let chosen: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"perm":"auto"}"#).unwrap();
        assert_eq!(chosen.perm, "auto", "explicit permission choices survive");

        let blank: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"perm":""}"#).unwrap();
        assert_eq!(blank.perm, "turbo", "blank legacy values are unset");
    }

    #[test]
    fn reasoning_stays_out_of_the_transcript_until_the_setting_says_otherwise() {
        // `show_thinking` is a bare `#[serde(default)]`, so the off state has to
        // be asserted from both directions: a fresh install and a settings.json
        // written before the field existed. The first user to "fix" this by
        // moving it to `default = "t"` would silently put a page of reasoning
        // in everyone's transcript, and nothing else here would catch it.
        assert!(!Settings::default().show_thinking);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(!old.show_thinking);
        // An explicit `true` from the user is still honoured.
        let on: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"show_thinking":true}"#).unwrap();
        assert!(on.show_thinking);
    }

    #[test]
    fn the_context_breakdown_split_is_opt_in() {
        // `ctx_breakdown` is a bare `#[serde(default)]`, so the off state has to
        // be asserted from both directions: a fresh install and a settings.json
        // written before the field existed. The split is an estimate scaled to
        // the provider's count, not a number any provider reports, so it is the
        // ring and its token pair that everyone sees by default — moving this
        // to `default = "t"` would put the estimate in every details panel.
        assert!(!Settings::default().ctx_breakdown);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(!old.ctx_breakdown);
        // An explicit `true` from the user is still honoured, and stored.
        let on: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"ctx_breakdown":true}"#).unwrap();
        assert!(on.ctx_breakdown);
        assert!(serde_json::to_value(&on).unwrap()["ctx_breakdown"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn a_phone_browser_starts_with_enter_as_a_newline() {
        // `phone_enter_sends` is a bare `#[serde(default)]` and the off state is
        // the safety property, not a preference: a phone's on-screen Return key
        // cannot be trusted to report Shift, so an install that upgraded into
        // Enter-sends would send the messages its owner was breaking into
        // paragraphs. Asserted from both directions so moving this to
        // `default = "t"` fails here rather than in someone's draft.
        assert!(!Settings::default().phone_enter_sends);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(!old.phone_enter_sends);
        // Turning it on in Settings is still honoured, and stored.
        let on: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"phone_enter_sends":true}"#)
                .unwrap();
        assert!(on.phone_enter_sends);
        assert!(serde_json::to_value(&on).unwrap()["phone_enter_sends"]
            .as_bool()
            .unwrap());
        // The desktop switch it deliberately does not follow.
        let ctrl: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"send_with":"ctrl-enter"}"#)
                .unwrap();
        assert!(!ctrl.phone_enter_sends);
        assert_eq!(ctrl.send_with, "ctrl-enter");
    }

    #[test]
    fn automatic_resume_is_on_for_new_and_existing_settings() {
        assert!(Settings::default().automatic_resume);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(old.automatic_resume);
        let off: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"automatic_resume":false}"#)
                .unwrap();
        assert!(!off.automatic_resume);
    }

    #[test]
    fn automatic_docker_rebuild_defaults_on_for_old_settings_and_respects_false() {
        assert!(Settings::default().docker_auto_rebuild);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(old.docker_auto_rebuild);
        let off: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"docker_auto_rebuild":false}"#)
                .unwrap();
        assert!(!off.docker_auto_rebuild);
    }

    #[test]
    fn automatic_chat_reorder_is_on_for_new_and_existing_settings() {
        assert!(Settings::default().automatic_chat_reorder);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(old.automatic_chat_reorder);
        let off: Settings = serde_json::from_str(
            r#"{"ui_size":"v2","ui_zoom":110,"automatic_chat_reorder":false}"#,
        )
        .unwrap();
        assert!(!off.automatic_chat_reorder);
    }

    #[test]
    fn welcome_seen_defaults_off_and_round_trips() {
        assert!(!Settings::default().welcome_seen);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2"}"#).unwrap();
        assert!(!old.welcome_seen);
        let seen: Settings = serde_json::from_str(r#"{"welcome_seen":true}"#).unwrap();
        assert!(seen.welcome_seen);
        let json = serde_json::to_string(&seen).unwrap();
        assert!(json.contains("\"welcome_seen\":true"));
    }

    #[test]
    fn auto_naming_is_on_unless_asked_otherwise() {
        // Naming a chat is what the tool is for, and it only fires once the job
        // is clear enough to have a name -- an unnamed chat in the sidebar is
        // the failure mode. A settings.json from before the flag existed must
        // land on the same default as a fresh install, not on a quiet `false`
        // -- that is what a bare `#[serde(default)]` on the field would have
        // given, and the user who never touched this setting would have been
        // the only one left with it off.
        assert!(Settings::default().agent_titles);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(
            old.agent_titles,
            "an existing settings.json must pick up the new default"
        );
        // An explicit `false` from the user still wins.
        let off: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"agent_titles":false}"#).unwrap();
        assert!(!off.agent_titles);
    }

    #[test]
    fn research_tools_cannot_be_switched_off() {
        // The deep-research tools lost their setting, so the tools and the rules
        // that govern them are unconditional. A settings.json written while the
        // toggle still existed can carry `"research": false`; deserialising has to
        // keep working (no `deny_unknown_fields`), and that stale false must not
        // survive a save-and-reload, or it would look like the switch still works.
        //
        // The save-and-reload here is the deliberate one: a hand-built struct
        // that never carried the key. A real load/save pair *does* write it back
        // now — as an unrecognised key, held in `Settings::unknown` so an older
        // build cannot delete a newer build's settings — and no code reads it, so
        // the switch is still gone. See `unknown_settings_keys_are_reported_and_kept`.
        let old: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"research":false}"#).unwrap();
        let round = Settings {
            ui_size: old.ui_size,
            ui_zoom: old.ui_zoom,
            ..Settings::default()
        };
        let json = serde_json::to_string(&round).unwrap();
        assert!(
            !json.contains("research"),
            "the removed flag must not come back on the next write"
        );
    }

    #[test]
    fn unknown_settings_keys_are_reported_and_kept() {
        // A key this build has no field for — a typo, or a setting a newer build
        // added — must not be an error (the file still has to load) and must not
        // be silently deleted on the next save. It is reported by name instead,
        // which is the only answer to "why didn't my setting do anything?".
        let root = std::env::temp_dir().join(format!("ol-unknown-keys-{}", std::process::id()));
        let _home = test_home(&root);
        // The claim on the folder is fresh, so it does not exist yet: the hand
        // written file below is the only thing in it.
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("settings.json");
        std::fs::write(
            &file,
            r#"{"ui_size":"v2","ui_zoom":110,"theme":"lavender","frm_the_future":true,"another":7}"#,
        )
        .unwrap();

        let s = load_settings();
        assert_eq!(s.theme, "lavender", "the known settings still load");
        assert_eq!(
            unknown_settings_keys(&s),
            vec!["another".to_string(), "frm_the_future".to_string()],
            "both unrecognised keys are named, sorted, and nothing else is"
        );
        let warning =
            unknown_settings_warning(&s).expect("an unknown key must be reported, not silent");
        assert!(
            warning.contains("frm_the_future") && warning.contains("another"),
            "the warning names the keys: {warning}"
        );
        assert!(warning.contains('2'), "the warning counts them: {warning}");
        assert!(
            !warning.contains("true") && !warning.contains('7'),
            "values are never repeated — one of them may be a pasted credential: {warning}"
        );

        // Save → load: the keys are still there, values intact, and the file on
        // disk is what a restart reads back.
        save_settings(&s);
        let reloaded = load_settings();
        assert_eq!(unknown_settings_keys(&reloaded), unknown_settings_keys(&s));
        assert_eq!(
            reloaded.unknown.get("frm_the_future"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(reloaded.unknown.get("another"), Some(&serde_json::json!(7)));
        let raw = std::fs::read_to_string(&file).unwrap();
        let on_disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(on_disk["frm_the_future"], serde_json::json!(true));
        assert_eq!(on_disk["another"], serde_json::json!(7));
        // And the known settings did not shift underneath them.
        assert_eq!(reloaded.theme, "lavender");
        assert_eq!(reloaded.ui_zoom, 110);
        // The other way settings travel: the whole struct through a JSON value,
        // which is the path the UI's settings patch takes. The keys have to
        // survive that too, or a theme change would drop them.
        let via_value: Settings =
            serde_json::from_value(serde_json::to_value(&reloaded).unwrap()).unwrap();
        assert_eq!(
            unknown_settings_keys(&via_value),
            unknown_settings_keys(&reloaded)
        );
        assert_eq!(
            via_value.unknown.get("frm_the_future"),
            Some(&serde_json::json!(true))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The trace-endpoint switch is a new field on an old struct, so both
    /// halves of that matter: a file written before it existed has to load with
    /// export off, and the field has to be a *known* one — a settings file that
    /// carries it must not have it reported as unrecognised and round-tripped
    /// as a mystery value nothing reads.
    #[test]
    fn the_trace_endpoint_setting_is_known_and_defaults_to_off() {
        let root = std::env::temp_dir().join(format!("ol-trace-endpoint-{}", std::process::id()));
        let _home = test_home(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"ui_size":"v2","ui_zoom":110}"#,
        )
        .unwrap();
        let s = load_settings();
        assert_eq!(
            s.trace_endpoint, "",
            "export is off until the user names a collector"
        );
        assert!(
            unknown_settings_keys(&s).is_empty(),
            "the new field must be recognised, not reported: {:?}",
            unknown_settings_keys(&s)
        );

        // Round trip: the user's URL survives a save and reload as a real value.
        let mut s = s;
        s.trace_endpoint = "http://127.0.0.1:4318/v1/traces".into();
        save_settings(&s);
        assert_eq!(
            load_settings().trace_endpoint,
            "http://127.0.0.1:4318/v1/traces"
        );

        // And a file that already carries the key loads it, rather than treating
        // it as one of `unknown`'s mystery keys.
        let parsed: Settings = serde_json::from_str(
            r#"{"ui_size":"v2","ui_zoom":110,"trace_endpoint":"http://localhost:4318/v1/traces"}"#,
        )
        .unwrap();
        assert_eq!(parsed.trace_endpoint, "http://localhost:4318/v1/traces");
        assert!(unknown_settings_keys(&parsed).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_settings_file_this_build_understands_reports_nothing() {
        // The feature's failure mode is noise: a report that fires on a file with
        // nothing wrong with it is worse than no report, because the one time it
        // matters it reads like every other time. A file containing only fields
        // this build has must stay silent, and must survive load + save byte for
        // byte — a file it understands is not a file it rewrites.
        let root = std::env::temp_dir().join(format!("ol-known-keys-{}", std::process::id()));
        let _home = test_home(&root);
        let file = root.join("settings.json");
        let mut s = load_settings();
        s.theme = "lavender".into();
        save_settings(&s);
        let written = std::fs::read_to_string(&file).unwrap();

        let loaded = load_settings();
        assert!(
            unknown_settings_keys(&loaded).is_empty(),
            "a file this build wrote reports nothing: {:?}",
            unknown_settings_keys(&loaded)
        );
        assert!(unknown_settings_warning(&loaded).is_none());
        assert!(loaded.unknown.is_empty());
        save_settings(&loaded);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            written,
            "load + save must not touch a file it fully understands"
        );

        // A hand-written file with only known keys — partial, older — is the same
        // case, and every field it leaves out is a default rather than a key this
        // build does not know.
        std::fs::write(
            &file,
            r#"{"ui_size":"v2","ui_zoom":110,"theme":"lavender","send_with":"ctrl-enter"}"#,
        )
        .unwrap();
        let partial = load_settings();
        assert!(unknown_settings_keys(&partial).is_empty());
        assert_eq!(partial.send_with, "ctrl-enter");
        assert!(partial.close_to_tray, "unmentioned fields default");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unknown_key_cannot_shadow_a_real_setting() {
        // The report is what a user is meant to act on, so it has to be about the
        // keys *this build has no field for* — not about names that merely look
        // like something. A near-miss must be listed as unknown and leave the real
        // setting exactly as the file set it.
        let s: Settings = serde_json::from_str(
            r#"{"ui_size":"v2","ui_zoom":110,"model":"anthropic/claude-opus-5",
                "uiZoom":7,"ui_zooms":["x"],"models":1,"Model":"typo"}"#,
        )
        .unwrap();
        assert_eq!(s.ui_zoom, 110, "the real setting keeps the file's value");
        assert_eq!(s.model, "anthropic/claude-opus-5");
        assert_eq!(
            unknown_settings_keys(&s),
            vec![
                "Model".to_string(),
                "models".to_string(),
                "uiZoom".to_string(),
                "ui_zooms".to_string()
            ]
        );
        // Neither half overwrote the other: the struct holds the real value under
        // the real name, and the look-alike under its own.
        let written = serde_json::to_value(&s).unwrap();
        assert_eq!(written["ui_zoom"], serde_json::json!(110));
        assert_eq!(
            written["model"],
            serde_json::json!("anthropic/claude-opus-5")
        );
        assert_eq!(written["uiZoom"], serde_json::json!(7));
        assert_eq!(written["models"], serde_json::json!(1));
    }

    #[test]
    fn a_setting_that_serialises_conditionally_is_not_reported_as_unknown() {
        // The tempting implementation is a diff — "keys in the file, minus keys
        // this build writes back" — and it is wrong for every field marked
        // `skip_serializing_if`: such a field is absent from this build's own
        // output while it holds no value, so the diff would call a *real* setting
        // unknown the first time the user set it. `unknown` is that field here,
        // which is why the names come from which fields the deserializer filled
        // in instead.
        let written = serde_json::to_value(Settings::default()).unwrap();
        assert!(
            written.get("unknown").is_none(),
            "the field is skipped in this build's own output, so a diff would not \
             know the name at all"
        );
        // A file whose only extra key is that field's own name: the catch-all
        // holds the rest of the file, so its name is not a setting — it is
        // reported like any other key this build has no field for, rather than
        // being swallowed or hidden.
        let s: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"unknown":{"typo":1}}"#).unwrap();
        assert_eq!(
            unknown_settings_keys(&s),
            vec!["unknown".to_string()],
            "the catch-all's own name is not a field name the file can fill"
        );
        assert_eq!(
            s.unknown.get("unknown"),
            Some(&serde_json::json!({"typo": 1}))
        );
    }

    #[test]
    fn memory_is_on_by_default_and_an_explicit_off_survives_the_migration() {
        assert!(Settings::default().memory);
        // A file written before the field had a serde default of its own.
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(old.memory);
        // Old builds serialized `"memory": false` whether or not anyone chose
        // it, so a pre-marker file is turned on once...
        let mut pre: Settings = serde_json::from_str(r#"{"memory":false}"#).unwrap();
        assert!(!pre.memory && !pre.memory_default_applied);
        apply_memory_default(&mut pre);
        assert!(pre.memory && pre.memory_default_applied);
        // ...and an off chosen after that, which saves with the marker, sticks.
        let mut chosen: Settings =
            serde_json::from_str(r#"{"memory":false,"memory_default_applied":true}"#).unwrap();
        apply_memory_default(&mut chosen);
        assert!(!chosen.memory);
    }

    #[test]
    fn browser_is_on_by_default_and_an_explicit_off_survives_the_migration() {
        // A fresh install: on, with the marker already set.
        let fresh = Settings::default();
        assert!(fresh.plugins.browser.enabled && fresh.browser_default_applied);
        // An old file that only ever had the old default written into it is
        // turned on once...
        let mut pre: Settings = serde_json::from_str(
            r#"{"plugins":{"browser":{"enabled":false,"width":1024,"height":700}}}"#,
        )
        .unwrap();
        assert!(!pre.plugins.browser.enabled && !pre.browser_default_applied);
        apply_browser_default(&mut pre);
        assert!(pre.plugins.browser.enabled && pre.browser_default_applied);
        assert_eq!(
            (pre.plugins.browser.width, pre.plugins.browser.height),
            (1024, 700),
            "only the switch moves"
        );
        // ...and the marker round-trips, so a later off sticks...
        let mut saved: Settings =
            serde_json::from_str(&serde_json::to_string(&pre).unwrap()).unwrap();
        assert!(saved.browser_default_applied);
        saved.plugins.browser.enabled = false;
        let mut chosen: Settings =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        apply_browser_default(&mut chosen);
        assert!(!chosen.plugins.browser.enabled, "an explicit off survives");
        // ...and an explicit on with the marker stays on.
        let mut on: Settings = serde_json::from_str(
            r#"{"browser_default_applied":true,"plugins":{"browser":{"enabled":true}}}"#,
        )
        .unwrap();
        apply_browser_default(&mut on);
        assert!(on.plugins.browser.enabled);
    }

    /// End to end through `load_settings`, including the order against the
    /// `render` fold-in: an old render user keeps render's viewport.
    #[test]
    fn load_settings_switches_the_browser_on_once_after_the_render_fold_in() {
        let root = std::env::temp_dir().join(format!(
            "ol-browser-default-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _home = test_home(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("settings.json");

        std::fs::write(
            &file,
            r#"{"plugins":{"browser":{"enabled":false},"render":{"enabled":true,"width":900,"height":600}}}"#,
        )
        .unwrap();
        let s = load_settings();
        assert!(s.plugins.browser.enabled && s.browser_default_applied);
        assert_eq!(
            (s.plugins.browser.width, s.plugins.browser.height),
            (900, 600)
        );

        std::fs::write(&file, r#"{"plugins":{"browser":{"enabled":false}}}"#).unwrap();
        let mut s = load_settings();
        assert!(
            s.plugins.browser.enabled,
            "pre-marker off is the old default"
        );
        // The user turns it off; the save carries the marker; it stays off.
        s.plugins.browser.enabled = false;
        save_settings(&s);
        let back = load_settings();
        assert!(!back.plugins.browser.enabled && back.browser_default_applied);
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(raw["browser_default_applied"], serde_json::json!(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_chat_is_on_by_default_and_respects_an_explicit_off() {
        // Listing and messaging other chats is on for new installs, and a
        // settings.json written before the flag existed must load with it on
        // too (`= "t"`, not a field-level `false` default). An explicit `false`
        // from the user still wins.
        assert!(Settings::default().cross_chat);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(old.cross_chat);
        let off: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"cross_chat":false}"#).unwrap();
        assert!(!off.cross_chat);
        assert!(serde_json::to_string(&Settings::default())
            .unwrap()
            .contains("\"cross_chat\":true"));
    }

    #[test]
    fn fresh_install_uses_the_platform_zoom_default() {
        // The frontend's DEFAULT_ZOOM has to agree with this, or the app boots
        // at one size and Ctrl 0 / Reset jumps to another.
        let s = Settings::default();
        assert_eq!(s.ui_zoom, default_ui_zoom());
        // And the default must not walk the old named-size migration on first load.
        assert_eq!(s.ui_size, "v2");
    }

    #[test]
    fn the_env_read_deny_is_on_by_default_and_survives_an_old_settings_file() {
        // A field-level `#[serde(default)]` would read as *false*, switching the
        // built-in credential guard off for exactly the user who never chose
        // anything. It has to default true, and an older settings.json has to
        // load into the true default too.
        assert!(Settings::default().deny_env_files);
        let old: Settings = serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110}"#).unwrap();
        assert!(
            old.deny_env_files,
            "a file written before the field existed keeps the deny on"
        );
        // An explicit `false` still wins.
        let off: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"deny_env_files":false}"#)
                .unwrap();
        assert!(!off.deny_env_files);
    }

    #[test]
    fn a_ladder_of_one_layer_is_not_x_mode() {
        // X needs >= 2 layers; a single layer is just ultrathread with a worker model.
        let mut x = UltraX {
            layers: vec![x_layer()],
            ..Default::default()
        };
        assert!(!x.on());
        x.layers.push(x_layer());
        assert!(x.on(), "two layers makes it X mode");
    }

    #[test]
    fn depth_comes_from_the_ladder_and_stops_at_six() {
        assert_eq!(UltraX::default().max_depth(), X_MAX_LAYERS as u8);
        let shallow = UltraX {
            layers: vec![x_layer(); 3],
            ..Default::default()
        };
        assert_eq!(shallow.max_depth(), 3, "a 3-layer ladder nests 3 deep");
        // Over-long ladders are clamped rather than trusted.
        let huge = UltraX {
            layers: vec![x_layer(); 40],
            ..Default::default()
        };
        assert_eq!(huge.max_depth(), X_MAX_LAYERS as u8);
    }

    #[test]
    fn layers_are_addressed_by_depth() {
        let x = UltraX {
            layers: vec![
                UltraXLayer {
                    model: "a".into(),
                    effort: Some(1),
                    fanout: 2,
                },
                UltraXLayer {
                    model: "b".into(),
                    effort: None,
                    fanout: 0,
                },
            ],
            ..Default::default()
        };
        assert_eq!(x.layer(1).map(|l| l.model.as_str()), Some("a"));
        assert_eq!(x.layer(2).map(|l| l.model.as_str()), Some("b"));
        assert!(x.layer(0).is_none(), "the orchestrator is not a layer");
        assert!(x.layer(3).is_none(), "past the configured height");
    }

    #[test]
    fn normalize_clamps_everything_the_ui_could_send_loose() {
        let mut x = UltraX {
            layers: vec![
                UltraXLayer {
                    model: "a".into(),
                    effort: Some(99),
                    fanout: 200,
                },
                UltraXLayer {
                    model: "b".into(),
                    effort: None,
                    fanout: 0,
                },
            ],
            max_running: 9999,
            max_total: 0,
            wt: true,
        };
        x.normalize();
        assert_eq!(x.layers[0].effort, Some(4), "effort tops out at 4");
        assert_eq!(
            x.layers[0].fanout, X_MAX_FANOUT,
            "fanout tops out at X_MAX_FANOUT"
        );
        assert_eq!(
            x.max_running, MAX_ULTRA_RUNNING,
            "concurrency is capped by the ceiling"
        );
        assert_eq!(
            x.max_total, X_DEFAULT_TOTAL,
            "0 means the default, not no limit"
        );
        // Short ladders are padded out to the full six so every layer is reachable.
        assert_eq!(x.layers.len(), X_MAX_LAYERS);
        assert!(x.wt, "worktrees survive normalization");
    }

    #[test]
    fn a_ladder_survives_a_json_round_trip() {
        let x = UltraX {
            layers: vec![UltraXLayer {
                model: "cheap".into(),
                effort: Some(4),
                fanout: 8,
            }],
            max_running: 12,
            max_total: 50,
            wt: true,
        };
        let back: UltraX = serde_json::from_str(&serde_json::to_string(&x).unwrap()).unwrap();
        assert_eq!(back.layers, x.layers);
        assert_eq!(back.max_running, 12);
        assert_eq!(back.max_total, 50);
        assert!(back.wt);
        // Missing fields fall back to the defaults rather than failing to load.
        let bare: UltraX = serde_json::from_str("{}").unwrap();
        assert_eq!(bare.layers.len(), X_MAX_LAYERS);
        assert!(!bare.wt);
    }

    #[test]
    fn required_agents_stay_on() {
        // Empty, partial and custom lists all end up with explore + general.
        assert_eq!(
            with_required_agents(vec![]),
            vec!["explore".to_string(), "general".to_string()]
        );
        assert_eq!(
            with_required_agents(vec!["custom".into(), "explore".into()]),
            vec![
                "custom".to_string(),
                "explore".to_string(),
                "general".to_string()
            ]
        );
        // Already whole: untouched, order preserved.
        let full = vec![
            "explore".to_string(),
            "general".to_string(),
            "custom".to_string(),
        ];
        assert_eq!(with_required_agents(full.clone()), full);
        let mut ids = vec!["custom".to_string()];
        ensure_required_agents(&mut ids);
        assert!(ids.contains(&"explore".to_string()) && ids.contains(&"general".to_string()));
    }

    #[test]
    fn parses_agent_files() {
        let d = parse_agent_md("---\nname: Code Reviewer\ndescription: Reviews diffs\ntools: Read, Grep, Glob\nmodel: inherit\n---\nYou review code.", "x").unwrap();
        assert_eq!(
            (d.id.as_str(), d.tools.as_str(), d.model.as_str()),
            ("code-reviewer", "read_only", "")
        );
        assert_eq!(d.prompt, "You review code.");
        assert!(d.inject_instructions);
        let bare = parse_agent_md(
            "---
name: x
inject_instructions: false
---
hi",
            "x",
        )
        .unwrap();
        assert!(!bare.inject_instructions);
        let old: AgentDef = serde_json::from_str(r#"{"id":"a"}"#).unwrap();
        assert!(old.inject_instructions, "defaults on for saved agents");
    }

    #[test]
    fn parses_skill_files() {
        let m = parse_skill_md("---\nname: pdf-forms\ndescription: Fill PDF forms, use when handling PDFs\n---\n# body", "x").unwrap();
        assert_eq!(
            (m.name.as_str(), m.description.as_str()),
            ("pdf-forms", "Fill PDF forms, use when handling PDFs")
        );
        assert!(
            !m.disable_model_invocation && !m.user_invocable,
            "frontmatter-free defaults hold"
        );
        // Name falls back to the folder, normalized.
        let m =
            parse_skill_md("---\ndescription: Does things\n---\nbody", "My Skill_Name").unwrap();
        assert_eq!(m.name, "my-skill-name");
        // Quoted values work.
        let m = parse_skill_md("---\nname: \"a-b\"\ndescription: 'Does x'\n---\n", "f").unwrap();
        assert_eq!((m.name.as_str(), m.description.as_str()), ("a-b", "Does x"));
        // Missing description / unusable name / no frontmatter all fail.
        assert!(parse_skill_md("---\nname: ok-name\n---\nbody", "f").is_none());
        assert!(parse_skill_md("---\nname: !!!\ndescription: x\n---\n", "???").is_none());
        assert!(parse_skill_md("# no frontmatter", "f").is_none());
        assert!(valid_skill_name("pdf-forms"));
        assert!(!valid_skill_name("PDF"));
        assert!(!valid_skill_name("has space"));
    }

    #[test]
    fn the_prompt_skill_list_skips_the_file_count_and_the_ui_count_is_bounded() {
        let root = std::env::temp_dir().join(format!("ol-skills-{}-count", std::process::id()));
        let _home = test_home(&root);
        let skill = skills_dir().join("heavy");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: heavy\ndescription: A skill with a huge folder\n---\n",
        )
        .unwrap();
        std::fs::write(skill.join("ref.md"), "x").unwrap();
        // Deeper than the walk goes, and wider than its entry budget.
        let mut deep = skill.join("node_modules");
        for i in 0..(SKILL_COUNT_DEPTH + 3) {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("too-deep.js"), "x").unwrap();
        let wide = skill.join("wide");
        std::fs::create_dir_all(&wide).unwrap();
        for i in 0..(SKILL_COUNT_ENTRIES + 50) {
            std::fs::write(wide.join(format!("f{i}")), "").unwrap();
        }

        let s = Settings::default();
        let ui = all_skills(&s, "");
        let heavy = ui.iter().find(|x| x.name == "heavy").unwrap();
        assert!(
            heavy.files >= 1 && heavy.files <= SKILL_COUNT_ENTRIES,
            "{}",
            heavy.files
        );

        let prompt = skills_in(&SkillScope::of(&s, ""), "", false);
        let heavy = prompt.iter().find(|x| x.name == "heavy").unwrap();
        assert_eq!(
            heavy.files, 0,
            "the prompt path does not walk skill folders"
        );
        assert_eq!(heavy.description, "A skill with a huge folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skills_import_list_and_remove() {
        let root = std::env::temp_dir().join(format!("ol-skills-{}-import", std::process::id()));
        let _home = test_home(&root);
        // A skill folder with a bundled resource imports whole.
        let src = root.join("src").join("pdf-wizard");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "---\nname: pdf-wizard\ndescription: Fill PDF forms, use when handling PDFs\n---\n# Do it\n").unwrap();
        std::fs::write(src.join("fields.md"), "field docs").unwrap();
        let def = import_skill(src.to_str().unwrap()).unwrap();
        assert_eq!(def.name, "pdf-wizard");
        assert_eq!(def.files, 1);
        assert_eq!(def.source, "user");
        // A lone `.md` file takes the file stem as its name.
        let single = root.join("single.md");
        std::fs::write(&single, "---\ndescription: Solo skill\n---\nbody").unwrap();
        assert_eq!(
            import_skill(single.to_str().unwrap()).unwrap().name,
            "single"
        );
        // Listed for agents, with disable support.
        let mut s = Settings::default();
        assert!(all_skills(&s, "")
            .iter()
            .any(|x| x.name == "pdf-wizard" && x.enabled && x.source == "user"));
        s.disabled_skills.push("pdf-wizard".into());
        assert_eq!(
            all_skills(&s, "")
                .iter()
                .find(|x| x.name == "pdf-wizard")
                .map(|x| x.enabled),
            Some(false)
        );
        // Preview: full text plus bundled files.
        let (def3, content, files) = read_skill(&Settings::default(), "", "pdf-wizard").unwrap();
        assert_eq!(def3.name, "pdf-wizard");
        assert!(content.contains("# Do it"));
        assert!(
            files.contains(&"SKILL.md".to_string()) && files.contains(&"fields.md".to_string())
        );
        // A project skill with the same name wins over the user one — but only
        // when the folder is trusted. An untrusted (or undecided) project must
        // not be able to put its own skill in front of the user's.
        let proj = root.join("proj");
        let pdir = proj.join(".openleash").join("skills").join("pdf-wizard");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(
            pdir.join("SKILL.md"),
            "---\nname: pdf-wizard\ndescription: Project version\n---\n",
        )
        .unwrap();
        let untrusted = all_skills(&Settings::default(), proj.to_str().unwrap())
            .into_iter()
            .find(|x| x.name == "pdf-wizard")
            .unwrap();
        assert_eq!(
            (untrusted.source.as_str(), untrusted.description.as_str()),
            ("user", "Fill PDF forms, use when handling PDFs"),
            "an undecided folder is untrusted, so its skill must not win"
        );
        let mut trusted_settings = Settings::default();
        crate::agent::trust::upsert(
            &mut trusted_settings.trust,
            proj.to_str().unwrap(),
            crate::agent::trust::TrustKind::Folder,
            crate::agent::trust::TrustState::Trusted,
        );
        let won = all_skills(&trusted_settings, proj.to_str().unwrap())
            .into_iter()
            .find(|x| x.name == "pdf-wizard")
            .unwrap();
        assert_eq!(
            (won.source.as_str(), won.description.as_str()),
            ("project", "Project version"),
            "a trusted folder's skill takes precedence"
        );
        // Removing the user skill leaves the project one alone.
        remove_skill("pdf-wizard").unwrap();
        assert!(remove_skill("pdf-wizard").is_err(), "already gone");
        assert!(!skills_dir().join("pdf-wizard").exists());
        // Junk is rejected with a helpful error.
        assert!(import_skill(root.join("missing").to_str().unwrap()).is_err());
        std::fs::write(root.join("bad.md"), "no frontmatter").unwrap();
        assert!(import_skill(root.join("bad.md").to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}

// ───────────────────────────── saved prompts ─────────────────────────────

#[cfg(test)]
mod saved_prompt_tests {
    use super::*;

    fn sp(id: &str, text: &str) -> SavedPrompt {
        SavedPrompt {
            id: id.into(),
            text: text.into(),
            project: "D:/work/app".into(),
            model: "anthropic/claude-opus-5".into(),
            route: "subs".into(),
            assist: "necessary".into(),
            perm: "full".into(),
            effort: Some(1),
            plan: true,
            ultra: true,
            ultra_wt: false,
            worktree: true,
            branch: "feat".into(),
            agents: vec!["explore".into(), "general".into()],
            images: vec![],
            files: vec![],
            created_at: Utc::now(),
        }
    }

    /// The `settings_update` path: splice the key into the stored JSON, read it back
    /// as `Settings`, write it out again. Anything `Settings` doesn't know about is
    /// dropped here, silently — which is how a field can look saved in the UI and be
    /// gone after a restart. (This is what happens when the app is built from a
    /// commit older than the field, so it doesn't compile if the field goes missing.)
    #[test]
    fn saved_prompts_survive_a_settings_update_round_trip() {
        let root = std::env::temp_dir().join(format!("ol-saved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path, serde_json::to_string(&Settings::default()).unwrap()).unwrap();

        // What the frontend sends when you press the bookmark button.
        let patch = serde_json::json!({ "saved_prompts": [sp("s1", "ship the thing"), sp("s2", "and this")] });
        let mut cur: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (k, v) in patch.as_object().unwrap() {
            cur.as_object_mut().unwrap().insert(k.clone(), v.clone());
        }
        let next: Settings = serde_json::from_value(cur).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&next).unwrap()).unwrap();

        let back: Settings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.saved_prompts.len(), 2, "saved prompts must reach disk");
        assert_eq!(back.saved_prompts[0].text, "ship the thing");
        assert_eq!(back.saved_prompts[0].model, "anthropic/claude-opus-5");
        assert_eq!(back.saved_prompts[0].route, "subs");
        assert_eq!(back.saved_prompts[0].effort, Some(1));
        assert!(
            back.saved_prompts[0].plan
                && back.saved_prompts[0].ultra
                && back.saved_prompts[0].worktree
        );
        assert_eq!(back.saved_prompts[0].branch, "feat");
        // Options the save path may leave empty still have to come back.
        let bare: Settings = serde_json::from_str(
            r#"{"saved_prompts":[{"id":"s3","text":"bare","created_at":"2026-09-26T00:00:00Z"}]}"#,
        )
        .unwrap();
        assert!(bare.saved_prompts[0].agents.is_empty());
        assert_eq!(bare.saved_prompts[0].effort, None);
        assert_eq!(bare.saved_prompts[0].perm, "turbo");
        let blank: Settings = serde_json::from_str(
            r#"{"saved_prompts":[{"id":"s4","text":"blank","perm":"","created_at":"2026-09-26T00:00:00Z"}]}"#,
        )
        .unwrap();
        assert_eq!(blank.saved_prompts[0].perm, "turbo");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn custom_commands_default_and_persist() {
        let root = std::env::temp_dir().join(format!("ol-custom-commands-{}", std::process::id()));
        let _home = test_home(&root);
        let mut s = load_settings();
        assert!(s.custom_commands.is_empty());
        s.custom_commands.push(CustomCommand {
            name: "review".into(),
            description: "Review edits".into(),
            prompt: "Review {{args}}".into(),
        });
        save_settings(&s);
        let reloaded = load_settings();
        assert_eq!(reloaded.custom_commands.len(), 1);
        assert_eq!(reloaded.custom_commands[0].prompt, "Review {{args}}");
    }

    /// The same round trip through the real load/save pair the app uses, so the
    /// file on disk is what a restart would read back.
    #[test]
    fn saved_prompts_reload_after_a_restart() {
        let root = std::env::temp_dir().join(format!("ol-saved-restart-{}", std::process::id()));
        let _home = test_home(&root);
        let mut s = load_settings();
        s.saved_prompts
            .push(sp("s1", "run the migration once staging is green"));
        s.saved_prompts.push(sp("s2", "bump the changelog"));
        save_settings(&s);

        let reloaded = load_settings();
        assert_eq!(
            reloaded.saved_prompts.len(),
            2,
            "a restart must not lose saved prompts"
        );
        assert_eq!(
            reloaded.saved_prompts[0].text,
            "run the migration once staging is green"
        );
        assert_eq!(reloaded.saved_prompts[0].perm, "full");
        assert_eq!(reloaded.saved_prompts[0].assist, "necessary");
        assert_eq!(reloaded.saved_prompts[1].text, "bump the changelog");
        // Loading one off the list is the other direction of the same write.
        let mut after: Settings = reloaded;
        after.saved_prompts.retain(|p| p.id != "s1");
        save_settings(&after);
        assert_eq!(load_settings().saved_prompts.len(), 1);
    }
}

// ───────────────────────────── composer drafts ─────────────────────────────

/// `drafts/<task id>.txt`, or `drafts/new-chat.txt` for the new-chat box.
fn draft_path_in(root: &std::path::Path, key: &str) -> PathBuf {
    let name: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let d = root.join("drafts");
    let _ = std::fs::create_dir_all(&d);
    d.join(format!(
        "{}.txt",
        if name.is_empty() { "new-chat" } else { &name }
    ))
}

fn load_draft_in(root: &std::path::Path, key: &str) -> String {
    std::fs::read_to_string(draft_path_in(root, key)).unwrap_or_default()
}

/// Empty text deletes the draft file.
fn save_draft_in(root: &std::path::Path, key: &str, text: &str) {
    let p = draft_path_in(root, key);
    if text.trim().is_empty() {
        let _ = std::fs::remove_file(p);
    } else {
        write_atomic(&p, text);
    }
}

pub fn load_draft(key: &str) -> String {
    load_draft_in(&data_dir(), key)
}

pub fn save_draft(key: &str, text: &str) {
    save_draft_in(&data_dir(), key, text)
}

#[cfg(test)]
mod secret_debug_tests {
    use super::*;

    #[test]
    fn no_secret_survives_a_debug_print() {
        // The failure this guards is quiet: someone adds a `dbg!` or a `tracing`
        // field on an error path, and every API key and OAuth token in the
        // settings file lands in a log. The types are the choke point.
        let k = "sk-live-DO-NOT-LOG-abcdef123456";
        let tok = "oauth-DO-NOT-LOG-zzz";

        let cfg = ProviderCfg {
            api_key: k.into(),
            api_keys: vec![k.into()],
            key_pool: true,
            base_url: "https://x".into(),
            enabled: true,
        };
        let d = format!("{cfg:?}");
        assert!(!d.contains(k), "ProviderCfg leaked an api key: {d}");
        assert!(
            !d.contains("abcdef123456"),
            "ProviderCfg leaked key material: {d}"
        );
        assert!(
            d.contains(r#"api_key: "<redacted>""#),
            "should say it held something: {d}"
        );

        let a = Account {
            id: "a".into(),
            kind: "claude".into(),
            label: "L".into(),
            email: "e".into(),
            access_token: tok.into(),
            refresh_token: tok.into(),
            ..Default::default()
        };
        let d = format!("{a:?}");
        assert!(!d.contains(tok), "Account leaked an OAuth token: {d}");
        assert!(
            d.contains("claude"),
            "non-secret fields should still be readable: {d}"
        );
        let mcp = McpServerCfg {
            name: "gh".into(),
            command: "npx".into(),
            args: vec!["-y".into()],
            env: HashMap::from([("GITHUB_TOKEN".to_string(), tok.to_string())]),
            enabled: true,
            ..Default::default()
        };
        let d = format!("{mcp:?}");
        assert!(!d.contains(tok), "McpServerCfg leaked an env token: {d}");
        assert!(
            d.contains("GITHUB_TOKEN"),
            "the env *name* is diagnostic, keep it: {d}"
        );

        // A hosted server authenticates with a header token, which must be
        // masked on exactly the same footing as a stdio env token.
        let mcp = McpServerCfg {
            name: "ctx7".into(),
            transport: "http".into(),
            url: "https://mcp.example.com/mcp".into(),
            headers: HashMap::from([("Authorization".to_string(), tok.to_string())]),
            enabled: true,
            ..Default::default()
        };
        let d = format!("{mcp:?}");
        assert!(!d.contains(tok), "McpServerCfg leaked a header token: {d}");
        assert!(
            d.contains("Authorization"),
            "the header *name* is diagnostic, keep it: {d}"
        );
    }

    #[test]
    fn the_interrupted_write_temp_is_recognised_and_removed() {
        // An interrupted `try_write_atomic` leaves `.{name}.{id}.tmp` behind, and
        // nothing ever renames or rereads it — the real dataset carried 886 MB
        // of these. Before the sweep existed, a crash during a 700 MB write cost
        // half a gigabyte of disk per event, forever.
        let root = std::env::temp_dir().join(format!("ol-sweep-{}", std::process::id()));
        let _home = test_home(&root);
        let tasks = data_dir().join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();

        let orphan = tasks.join(".b3c99f60e9a4.json.75fa3393765d.tmp");
        std::fs::write(&orphan, "half a task").unwrap();
        let keep = tasks.join("b3c99f60e9a4.json");
        std::fs::write(&keep, "{}").unwrap();
        // Hidden files that are not ours must survive: the pattern is matched,
        // not every dotfile.
        let other = tasks.join(".gitkeep");
        std::fs::write(&other, "").unwrap();
        // A wrong extension is not our temp either.
        std::fs::write(tasks.join(".b3c99f60e9a4.json.75fa3393765d.tmp.bak"), "x").unwrap();

        assert!(is_temp_name(".b3c99f60e9a4.json.75fa3393765d.tmp"));
        assert!(!is_temp_name("b3c99f60e9a4.json"), "a real task file");
        assert!(!is_temp_name(".gitkeep"));
        assert!(!is_temp_name("..tmp"));
        assert!(!is_temp_name("no-id-here.tmp"));

        sweep_temp_files();
        assert!(!orphan.exists(), "the orphaned write temp is gone");
        assert!(keep.exists(), "the real task file survives");
        assert!(other.exists(), "an unrelated hidden file survives");
        assert!(tasks
            .join(".b3c99f60e9a4.json.75fa3393765d.tmp.bak")
            .exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_empty_secret_still_reads_as_empty_not_redacted() {
        // Otherwise "no key configured" and "a key is configured but hidden"
        // look identical in a debug log, which is the one thing you check first.
        let cfg = ProviderCfg::default();
        let d = format!("{cfg:?}");
        assert!(d.contains(r#"api_key: """#), "should read as empty: {d}");
        // (The empty *pool* still prints its count; only the scalar key above
        // reads as empty.)
    }

    #[test]
    fn serialising_still_writes_the_real_key_to_disk() {
        // The Debug impls must not have leaked into Serialize: a settings file
        // with a redacted key would silently break every provider.
        let k = "sk-real-key-123";
        let s = Settings {
            providers: HashMap::from([(
                "anthropic".to_string(),
                ProviderCfg {
                    api_key: k.into(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(
            j.contains(k),
            "the on-disk format must still carry the real key"
        );
        assert!(!format!("{s:?}").contains(k), "but a debug print must not");
    }
}

#[cfg(test)]
mod atomic_write_tests {
    use super::*;

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temp_behind() {
        let root = std::env::temp_dir().join(format!("ol-atomic-{}", new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let p = root.join("settings.json");

        try_write_atomic(&p, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":1}");
        try_write_atomic(&p, "{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":2}");

        // The temp file is uniquely named per write, so it must not be left
        // sitting next to the real file after a successful save.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "settings.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_writers_never_collide_on_the_same_temp_name() {
        // The old `path.with_extension("tmp")` turned `tasks/abc.json` into
        // `tasks/abc.tmp`, so neighbouring task ids clobbered each other.
        let root = std::env::temp_dir().join(format!("ol-atomic-collide-{}", new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("abc.json");
        let b = root.join("abd.json");
        // Same extension, different stem: the old scheme mapped both to `.tmp`.
        assert_ne!(
            a.with_extension("tmp"),
            b.with_extension("tmp"),
            "precondition: these used to collide"
        );

        try_write_atomic(&a, "\"a\"").unwrap();
        try_write_atomic(&b, "\"b\"").unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "\"a\"");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "\"b\"");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_write_reports_instead_of_disappearing() {
        // The old version discarded the rename result, so a failed save was
        // indistinguishable from a successful one and silently lost settings.
        let root = std::env::temp_dir().join(format!("ol-atomic-fail-{}", new_id()));
        let missing = root.join("no-such-dir").join("settings.json");
        assert!(
            try_write_atomic(&missing, "{}").is_err(),
            "writing into a missing directory must surface an error"
        );
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    #[test]
    fn drafts_round_trip_and_clear() {
        let root = std::env::temp_dir().join(format!("ol-drafts-{}", std::process::id()));
        save_draft_in(&root, "abc123", "half-written thought");
        assert_eq!(load_draft_in(&root, "abc123"), "half-written thought");
        save_draft_in(&root, "new-chat", "new chat idea");
        assert_eq!(load_draft_in(&root, "new-chat"), "new chat idea");
        assert_eq!(
            load_draft_in(&root, "abc123"),
            "half-written thought",
            "separate files"
        );
        save_draft_in(&root, "abc123", "  ");
        assert_eq!(load_draft_in(&root, "abc123"), "");
        assert!(!draft_path_in(&root, "abc123").exists());
        assert!(
            draft_path_in(&root, "../../evil").ends_with("evil.txt"),
            "no path tricks"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// A chat saved before the user rewrote `AGENTS.md` must not keep running on
    /// the old copy: the frozen prompt is dropped on load, so the next request
    /// rebuilds it from whatever the files say now.
    #[test]
    fn a_reloaded_chat_drops_its_stale_project_instructions() {
        // Two directories: one is the app's data dir, the other a project folder
        // holding the instruction file. Keeping them apart stops the harness's
        // own bookkeeping from touching the file under test.
        let root = std::env::temp_dir().join(format!("ol-memload-{}", std::process::id()));
        let _home = test_home(&root.join("home"));
        // The trust table is process-global; hold it so this test's decision
        // cannot leak into another module running in parallel.
        let _trust = crate::agent::trust::TRUST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project = project.to_string_lossy().into_owned();
        std::fs::write(Path::new(&project).join("AGENTS.md"), "the old rules").unwrap();
        // Trusted: this test is about the frozen prompt being rebuilt from the
        // file on disk, not about the trust gate (which has its own tests).
        crate::agent::trust::set_trust(&[crate::agent::trust::TrustDecision {
            path: crate::agent::trust::norm_path(&project),
            kind: crate::agent::trust::TrustKind::Folder,
            decision: crate::agent::trust::TrustState::Trusted,
            decided_at: chrono::Utc::now(),
        }]);

        let t = Task {
            id: "m1".into(),
            title: "t".into(),
            route_log: vec![],
            titled: false,
            status: "idle".into(),
            waiting_kind: None,
            step: String::new(),
            project: project.clone(),
            cwd: ".".into(),
            branch: String::new(),
            base_branch: String::new(),
            base_commit: None,
            worktree: false,
            model: "m".into(),
            burner: None,
            effort: 2,
            perm: "auto".into(),
            plan: false,
            plan_artifict: None,
            ultra: false,
            ultra_wt: false,
            ultra_x: None,
            subagents: true,
            goal: None,
            assist: "default".into(),
            agents: vec!["explore".into()],
            pending: Default::default(),
            paused: None,
            serving: String::new(),
            system: "# Project instructions\n<file name=\"AGENTS.md\">\nthe old rules\n</file>"
                .into(),
            mcp_tools: vec![json!({"name": "kept_tool"})],
            plugins: Default::default(),
            told: Default::default(),
            sub_items: Default::default(),
            sub_msgs: Default::default(),
            route: String::new(),
            unpaused: false,
            stop_note: None,
            bg_live: vec![],
            busy: 0,
            bg_now: vec![],
            wrap_up: false,
            summarise_first: false,
            model_map: Default::default(),
            archived: false,
            pinned: false,
            artifict_open: false,
            order: 0.0,
            hidden: false,
            // `Task::unread` is the sidebar's blue dot; this fixture is a saved
            // chat, and nothing here is waiting to be looked at.
            unread: false,
            forked_from: None,
            items: vec![],
            messages: vec![],
            todos: vec![],
            subs: vec![],
            usage: Default::default(),
            read_files: Default::default(),
            read_hashes: Default::default(),
            spans: vec![],
            checkpoints: vec![],
            touched: Default::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            touched_at: chrono::Utc::now(),
            hydrated: true,
            docker: None,
            sandbox: None,
        };
        save_task(&t);
        // What is on disk still carries the old prompt; that is the whole point --
        // a chat saved by the previous run is stale by definition.
        let on_disk = std::fs::read_to_string(data_dir().join("tasks").join("m1.json")).unwrap();
        assert!(
            on_disk.contains("the old rules"),
            "the saved prompt is the old one"
        );
        assert!(
            !load_tasks()
                .iter()
                .any(|x| x.system.contains("the old rules")),
            "loading drops it"
        );

        // The user rewrites the instructions, then the app restarts.
        std::fs::write(Path::new(&project).join("AGENTS.md"), "the new rules").unwrap();
        let back = load_tasks();
        let t2 = back
            .iter()
            .find(|x| x.id == "m1")
            .expect("the chat came back");
        assert!(
            t2.system.is_empty(),
            "the frozen prompt is dropped so the next request rebuilds it"
        );
        assert!(
            t2.mcp_tools.is_empty(),
            "the cached tool list goes with it, so it is rebuilt too"
        );
        assert_eq!(
            t2.title, "t",
            "the chat itself is untouched -- only the prompt went"
        );

        // And what it rebuilds from is the file as it is now, not the old copy.
        let rebuilt = super::super::prompt::system(
            &super::super::prompt::Env {
                cwd: ".",
                project: &project,
                branch: "",
                is_git: false,
                worktree: false,
                date: "2026-09-28".into(),
                agent_id: "ol-m1",
                inject_global_claude: false,
                disabled_docs: &[],
            },
            None,
            &[],
        );
        assert!(
            rebuilt.contains("the new rules"),
            "the new rules are what it picks up: {rebuilt}"
        );
        assert!(!rebuilt.contains("the old rules"), "the old rules are gone");
        let _ = std::fs::remove_dir_all(root);
    }
}
