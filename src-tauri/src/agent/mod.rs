//! OpenLeash agent harness core.
//!
//! - `mod.rs`         shared types + the `Harness` (task registry, event bus)
//! - `providers.rs`   model backends (Anthropic native, OpenAI-compatible)
//! - `runner.rs`      the agent loop (stream → tools → loop), subagents, compaction
//! - `tools.rs`       tool schemas + implementations
//! - `permissions.rs` permission modes, allowlist rules, read-only command detection
//! - `shell.rs`       foreground/background shell execution
//! - `prompt.rs`      system prompt + project memory
//! - `git.rs`         worktrees, review diff, commit
//! - `mcp.rs`         stdio MCP client
//! - `memory.rs`      per-project agent memory (index + topic files)
//! - `projects.rs`    opening projects from WSL distros and cloned git URLs
//! - `store.rs`       settings + task persistence

pub mod accounts;
pub mod attachments;
pub mod bisect;
pub mod browser;
pub mod browser_annotate;
pub mod browser_panel;
pub mod browser_policy;
pub mod browser_proxy;
pub mod browser_snapshot;
pub mod burner;
pub mod burner_policy;
pub mod chats;
pub mod checkpoint;
pub mod checks;
pub mod commitmsg;
pub mod docker;
pub mod git;
pub mod http_tool;
pub mod imagefit;
pub mod lsp;
pub mod mcp;
pub mod mcp_oauth;
pub mod memory;
pub mod pcguard;
pub mod permissions;
pub mod plugin_gitlab;
pub mod plugin_jira;
pub mod plugin_linear;
pub mod plugin_notion;
pub mod plugin_services;
pub mod plugin_slack;
pub mod plugins;
pub mod projects;
pub mod prompt;
pub mod proof;
pub mod providers;
pub mod routerules;
pub mod race;
mod race_safety;
pub mod review;
#[cfg(test)]
mod reviewer_accounting;
#[cfg(test)]
mod reviewer_bg_jobs;
#[cfg(test)]
mod reviewer_browser_core;
#[cfg(test)]
mod reviewer_bisect;
#[cfg(test)]
mod reviewer_burner;
#[cfg(test)]
mod reviewer_checkpoint;
#[cfg(test)]
mod reviewer_commit;
#[cfg(test)]
mod reviewer_context_parts;
#[cfg(test)]
mod reviewer_cross_chat;
#[cfg(test)]
mod reviewer_docs;
#[cfg(test)]
mod reviewer_edits;
#[cfg(test)]
mod reviewer_hooks;
#[cfg(test)]
mod reviewer_http_request;
#[cfg(test)]
mod reviewer_mcp;
#[cfg(test)]
mod reviewer_memory_scope;
#[cfg(test)]
mod reviewer_review;
// #[cfg(test)]
// mod reviewer_modes;
#[cfg(test)]
mod reviewer_notices;
#[cfg(test)]
mod reviewer_permissions;
#[cfg(test)]
mod reviewer_plugins_scope;
#[cfg(test)]
mod reviewer_privacy;
#[cfg(test)]
mod reviewer_prompt_rules;
#[cfg(test)]
mod reviewer_router_rules;
#[cfg(test)]
mod reviewer_run_timeline;
#[cfg(test)]
mod reviewer_runtime;
#[cfg(test)]
mod reviewer_runtime_wake;
#[cfg(test)]
mod reviewer_sandbox;
#[cfg(test)]
mod reviewer_service_plugins;
#[cfg(test)]
mod reviewer_ssh;
#[cfg(test)]
mod reviewer_staleness;
#[cfg(test)]
mod reviewer_startup;
#[cfg(test)]
mod reviewer_streams;
#[cfg(test)]
mod reviewer_summary_sync;
#[cfg(test)]
mod reviewer_task_creation;
#[cfg(test)]
mod reviewer_task_persistence;
#[cfg(test)]
mod reviewer_toolsearch;
#[cfg(test)]
mod reviewer_trust;
#[cfg(test)]
mod reviewer_unread;
#[cfg(test)]
mod reviewer_webfetch;
pub mod router;
pub mod runner;
pub mod schedule;
pub mod shell;
pub mod skillgen;
pub mod spans;
// Remote-SSH projects: addressing/quoting (`host`) and live SFTP + command
// sessions (`ssh_session`). Phase 2 wires them into tools/runner/git; until
// then most of their API has no caller.
#[allow(dead_code)]
pub mod host;
#[allow(dead_code)]
pub mod ssh_session;
pub mod stats;
pub mod store;
#[cfg(test)]
pub(crate) mod tests;
pub mod toolindex;
pub mod tools;
pub mod tools_remote;
pub mod transfer;
pub mod trust;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub fn new_id() -> String {
    Uuid::new_v4().simple().to_string()[..12].to_string()
}

// ───────── ULTRATHREAD X ─────────
/// Layers below the orchestrator that a ladder may configure. Six is the
/// ceiling on nesting depth for any ultrathread.
pub const X_MAX_LAYERS: usize = 6;
/// Most subagents one agent may launch at the same time, per layer.
pub const X_MAX_FANOUT: u8 = 8;
/// Subagents running at once across a whole X tree (a plain ultrathread runs 16).
pub const X_DEFAULT_RUNNING: usize = 32;
/// Subagents spawned for one X task, ever. The real bound on spend: a running
/// cap alone lets a long task churn through thousands of agent-lives.
pub const X_DEFAULT_TOTAL: usize = 400;
/// Ceiling on how many subagents may run at once, whatever the ladder says.
pub const MAX_ULTRA_RUNNING: usize = 64;

/// One model-facing message, stored in Anthropic content-block shape
/// (the richest format). OpenAI-compatible providers convert on the way out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<Value>,
    /// Which model produced an assistant turn (thinking signatures only replay to the same model).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: vec![json!({"type":"text","text":text.into()})],
            model: String::new(),
        }
    }
    pub fn user(content: Vec<Value>) -> Self {
        Self {
            role: "user".into(),
            content,
            model: String::new(),
        }
    }
}

/// Why a task is paused. Paused agents are frozen mid-flight; nothing is said to the model.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Pause {
    pub reason: String,
    /// manual | exhausted | error | closed
    pub kind: String,
    pub since: DateTime<Utc>,
}

/// One entry in the UI timeline. `data` carries kind-specific fields so the
/// IPC shape stays stable while item kinds evolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// user | text | thinking | tool | approval | question | sub | notice | artifict
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub data: Value,
    pub ts: DateTime<Utc>,
}

impl Item {
    pub fn new(kind: &str, text: impl Into<String>, data: Value) -> Self {
        Self {
            id: new_id(),
            kind: kind.into(),
            text: text.into(),
            data,
            ts: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Todo {
    pub content: String,
    pub status: String,
    #[serde(default, rename = "activeForm")]
    pub active_form: String,
}

/// Goal mode: the agent keeps working until it proves the goal is met.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Goal {
    pub text: String,
    /// active | achieved | blocked | gave_up
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub nudges: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubInfo {
    pub id: String,
    /// Reasoning effort override for this subagent (0 = Max … 4 = Low); None = its agent type's / the chat's.
    #[serde(default)]
    pub effort: Option<usize>,
    /// Ultrathread worktrees: this agent's own checkout + branch (empty = the task's).
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub branch: String,
    pub role: String,
    pub task: String,
    pub status: String,
    pub meta: String,
    /// The model this agent runs on. Empty = none of its own: follow whatever the
    /// chat says right now (an X ladder for this depth, else the chat's model).
    ///
    /// This is an *override*, not a record of what it was launched on. Stamping
    /// the resolved model in at spawn froze the answer: a chat swapped from Opus
    /// to Sonnet mid-run left every subagent on Opus for the rest of the task,
    /// and the swap dialog — which rewrites `sub.model` — had nothing to rewrite.
    /// Resolution is re-done per request instead, so only a model somebody
    /// actually chose is stored here.
    #[serde(default)]
    pub model: String,
    /// Account/provider serving its last request.
    #[serde(default)]
    pub serving: String,
    #[serde(default)]
    pub started: Option<DateTime<Utc>>,
    #[serde(default)]
    pub report: String,
    /// The main agent's `task` tool call that launched it (to hand back a resumed report).
    #[serde(default)]
    pub call_id: String,
    /// Its pill in the main timeline.
    #[serde(default)]
    pub item_id: String,
    /// Runs detached; its report reaches the main agent as a note.
    #[serde(default)]
    pub background: bool,
    /// Subagent that launched this one (ultrathread nesting); empty = the main agent.
    #[serde(default)]
    pub parent: String,
    /// 1 = launched by main, 2 = launched by a subagent, …
    #[serde(default)]
    pub depth: u8,
}

/// How the most recent prompt split by category, in tokens.
///
/// An estimate, and labelled as one on purpose: a provider reports only a prompt
/// *total*, and the serialized request is built and dropped inside
/// `providers::call`, so nothing downstream can see the individual pieces. So the
/// split is measured from the request we assembled, then scaled by
/// [`ContextParts::scaled_to`] so the parts sum to the provider's own count. The
/// ring keeps provider truth for the total and shows this shape for the mix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextParts {
    /// Core harness prompt, environment and the wrapper guidance.
    pub system: u64,
    /// The project instruction files injected as `# Project instructions`.
    pub instructions: u64,
    /// The skills list, plus the memory/research sections appended after it.
    pub skills: u64,
    /// The tool schemas actually sent, including MCP, plugin and held-back ones.
    pub tools: u64,
    /// Attachment notes and image blocks carried on the user's turn.
    pub attachments: u64,
    /// Conversation history, excluding a compaction summary.
    pub history: u64,
    /// The compaction summary that replaced earlier history, when one exists.
    pub summary: u64,
}

impl ContextParts {
    pub fn total(&self) -> u64 {
        self.system
            + self.instructions
            + self.skills
            + self.tools
            + self.attachments
            + self.history
            + self.summary
    }

    /// Nothing measured yet. Drives `skip_serializing_if`, so a task that has
    /// never run a turn (and every task saved before this existed) carries no
    /// empty object on the wire.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Rescale so the parts sum to exactly `total`, the provider-reported prompt
    /// size. Without this the bars would add up to a number that disagrees with
    /// the ring beside them: the split comes from a character heuristic and the
    /// provider is the truth. The rounding remainder lands on the largest bucket
    /// so the sum matches to the token.
    ///
    /// `total` must be the **prompt** side (`input + cache_read + cache_write`),
    /// not `TurnUsage::context()` — that also counts the reply this very request
    /// produced, which was not in the prompt that was measured.
    pub fn scaled_to(&self, total: u64) -> ContextParts {
        let mine = self.total();
        if mine == 0 {
            return ContextParts::default();
        }
        let scale = |v: u64| (u128::from(v) * u128::from(total) / u128::from(mine)) as u64;
        let mut out = ContextParts {
            system: scale(self.system),
            instructions: scale(self.instructions),
            skills: scale(self.skills),
            tools: scale(self.tools),
            attachments: scale(self.attachments),
            history: scale(self.history),
            summary: scale(self.summary),
        };
        let drift = i64::try_from(total.saturating_sub(out.total())).unwrap_or(0);
        if drift != 0 {
            let biggest = out.largest_mut();
            *biggest = (*biggest as i64 + drift).max(0) as u64;
        }
        out
    }

    fn largest_mut(&mut self) -> &mut u64 {
        let all = [
            self.system,
            self.instructions,
            self.skills,
            self.tools,
            self.attachments,
            self.history,
            self.summary,
        ];
        let idx = all
            .iter()
            .enumerate()
            .max_by_key(|(_, v)| **v)
            .map(|(i, _)| i)
            .unwrap_or(0);
        match idx {
            0 => &mut self.system,
            1 => &mut self.instructions,
            2 => &mut self.skills,
            3 => &mut self.tools,
            4 => &mut self.attachments,
            5 => &mut self.history,
            _ => &mut self.summary,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
    /// Prompt size of the most recent request — drives the context meter + compaction.
    pub last_context: u64,
    /// That same request, split by category. Deliberately *not* written by
    /// keepalives or side requests, for the same reason `last_context` is not:
    /// a ping must not rescale the meter. Absent until a turn has run, and
    /// `#[serde(default)]` is what keeps every already-saved task loading.
    #[serde(default, skip_serializing_if = "ContextParts::is_empty")]
    pub parts: ContextParts,
}

/// Marks where a user turn starts, so rewinding a conversation can drop
/// everything from that turn onwards.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Checkpoint {
    pub item_index: usize,
    pub msg_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    /// The user named this chat themselves, so the agent must never rename it
    /// again — their word for the chat outranks a summary of it. Cleared from
    /// the sidebar's context menu when they want the agent back on it.
    #[serde(default)]
    pub titled: bool,
    /// idle | running | waiting | done | failed | stopped
    pub status: String,
    /// approval | question | null — what "waiting" is waiting on
    #[serde(default)]
    pub waiting_kind: Option<String>,
    pub step: String,
    pub project: String,
    pub cwd: String,
    pub branch: String,
    #[serde(default)]
    pub base_branch: String,
    #[serde(default)]
    pub base_commit: Option<String>,
    pub worktree: bool,
    /// The container this chat runs its commands in (docker workspace mode).
    /// `cwd` is then the task's private copy of the project on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<docker::DockerEnv>,
    pub model: String,
    /// Backend-installed restriction, persisted before startup is dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burner: Option<burner_policy::BurnPolicy>,
    pub effort: usize,
    #[serde(
        default = "crate::agent::permissions::default_perm",
        deserialize_with = "crate::agent::permissions::deserialize_perm"
    )]
    pub perm: String,
    pub plan: bool,
    /// The artifict this chat's plan was recorded as, so refining the plan
    /// appends a revision to it rather than filing a second artifict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_artifict: Option<String>,
    /// Ultrathread: long-haul orchestration with nested subagents.
    #[serde(default)]
    pub ultra: bool,
    #[serde(default = "yes")]
    pub subagents: bool,
    #[serde(default)]
    pub goal: Option<Goal>,
    /// guide | default | necessary
    #[serde(default = "assist_default")]
    pub assist: String,
    /// Subagent ids this task may spawn.
    #[serde(default = "agents_default")]
    pub agents: Vec<String>,
    /// Changes the user made mid-run that apply with the next user message (model, assist, agents).
    #[serde(default)]
    pub pending: serde_json::Map<String, Value>,
    #[serde(default)]
    pub paused: Option<Pause>,
    /// Account/provider that served the last main request.
    #[serde(default)]
    pub serving: String,
    /// Frozen system prompt + MCP tools: byte-stable for the prompt cache until compaction.
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub mcp_tools: Vec<Value>,
    /// The plugin config the frozen prefix was built from. `tools::schemas` needs
    /// it (a core tool, `screenshot`, follows the computer plugin), and reading
    /// live settings at request time would let the tool list disagree with the
    /// prompt that describes it -- e.g. a plugin flipped on mid-chat would add a
    /// tool the frozen system prompt says does not exist. Cleared alongside
    /// `system`/`mcp_tools` wherever those are dropped to force a rebuild.
    #[serde(default)]
    pub plugins: crate::agent::plugins::PluginsCfg,
    /// What the model was last told (assist mode, agents), so changes get announced, never edited in.
    #[serde(default)]
    pub told: serde_json::Map<String, Value>,
    #[serde(default)]
    pub sub_items: HashMap<String, Vec<Item>>,
    /// Each subagent's own conversation, so a stopped one can be continued.
    #[serde(default)]
    pub sub_msgs: HashMap<String, Vec<Message>>,
    /// The user messaged this task during a global pause, so it runs anyway.
    #[serde(default)]
    pub unpaused: bool,
    /// Set when the user stops (not pauses) a run: told to the agent once, on its next request.
    #[serde(default)]
    pub stop_note: Option<String>,
    /// Ultrathread worktrees: each top-level subagent works in its own git worktree,
    /// and a `fuze` agent merges their branches back.
    #[serde(default)]
    pub ultra_wt: bool,
    /// ULTRATHREAD X: the per-layer ladder (model / effort / fanout) this task fans
    /// out with. `None` = plain ultrathread. A 1-layer ladder is ultrathread with
    /// a model for the workers, so X needs >= 2 layers to be a distinct mode.
    #[serde(default)]
    pub ultra_x: Option<store::UltraX>,
    /// Resumed after a stop with "wrap up": don't restart cut-off subagents, just summarise.
    #[serde(default)]
    pub wrap_up: bool,
    /// Resumed after a stop with "summarise and continue": say where things stand,
    /// then carry on. Mutually exclusive with `wrap_up` — that one is the resume
    /// that stops, this one is the resume that reports on its way back into the
    /// work, so it must leave cut-off subagents resuming normally.
    #[serde(default)]
    pub summarise_first: bool,
    /// Background commands alive at the last save ("bg_id: cmd"). Non-empty on load = the app died under them.
    #[serde(default)]
    pub bg_live: Vec<String>,
    /// Commands (foreground + background) still running. While paused and > 0, the pause is "in progress".
    #[serde(skip)]
    pub busy: usize,
    /// The background commands alive *right now*, as a summary publishes them.
    ///
    /// Deliberately not `bg_live`, and deliberately not saved. `bg_live` is the
    /// record that goes to disk and is read back as "the app died under these",
    /// so after a reload it names jobs the OS has already killed; a summary
    /// built from it would list dead servers as live. This one is a live mirror
    /// written by `refresh_busy`, which every background start and exit already
    /// calls, and dropped by `#[serde(skip)]` so nothing can revive it later.
    #[serde(skip)]
    pub bg_now: Vec<shell::BgInfo>,
    /// Model swaps made in this chat (old -> new), applied to subagents it spawns later.
    #[serde(default)]
    pub model_map: HashMap<String, String>,
    /// Fallback route id ("" = none).
    #[serde(default)]
    pub route: String,
    /// Hidden from the sidebar; kept until permanently deleted.
    #[serde(default)]
    pub archived: bool,
    /// Out of the sidebar too, but with a difference: a hidden chat is still a
    /// normal chat you can open, message and archive. Set on forks, which are
    /// reached from the chat they came from rather than found on their own.
    #[serde(default)]
    pub hidden: bool,
    /// Something here is waiting to be looked at: a run finished or failed, a
    /// question or an approval is pending, a notice was left, or the chat paused
    /// itself — and nobody has opened it since.
    ///
    /// Session state, not saved state (`#[serde(skip)]`): it is the record behind
    /// the sidebar's blue dot, and a dot is drawn from a summary, so the *backend*
    /// keeps it rather than the window that draws it. A remote browser is a second
    /// client of this same app: `remote/shim.js` reloads the page on every
    /// reconnect, and a phone that was asleep when the event fired holds no flag
    /// of its own — it used to paint a finished chat as if it had already been
    /// read. `Harness::attention` sets this, `Task::mark_seen` clears it.
    #[serde(skip)]
    pub unread: bool,
    /// The chat this one was forked from, and how. "full" copied the whole
    /// conversation; "compact" starts from a summary of it instead.
    #[serde(default)]
    pub forked_from: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    /// Whether this chat was left sitting on its Artificts surface. Saved with
    /// the chat, like its name and its pin: switching to another chat brings
    /// that chat's own answer with it, so the record surface is a property of
    /// the chat rather than a mode of the window. Absent means false, so a chat
    /// saved before this existed still loads.
    #[serde(default)]
    pub artifict_open: bool,
    /// Manual sidebar position (drag to reorder). 0 = sort by recency instead:
    /// either never dragged, or released by acting in the chat — `Harness::touch`
    /// clears it, which is what makes "you did something, so it goes to the top"
    /// true for a chat the user had once dragged.
    #[serde(default)]
    pub order: f64,
    pub items: Vec<Item>,
    pub messages: Vec<Message>,
    pub todos: Vec<Todo>,
    pub subs: Vec<SubInfo>,
    pub usage: Usage,
    #[serde(default)]
    pub read_files: HashMap<String, u64>,
    /// Content hash (`tools::content_hash`) of each file in `read_files`, as
    /// it was read. Remote (SSH) files are checked against it on edit: SFTP
    /// mtimes are whole seconds, so a change within the same second as the
    /// read would otherwise go unnoticed. Missing in older task files.
    #[serde(default)]
    pub read_hashes: HashMap<String, String>,
    /// Timed record of the run (model requests, tool calls, waits) behind the
    /// run timeline. See `spans.rs`. Absent in older task files, and not
    /// written while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<spans::Span>,
    #[serde(default)]
    pub checkpoints: Vec<Checkpoint>,
    pub touched: HashMap<String, Option<String>>,
    /// Router decisions persisted for activity history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_log: Vec<router::RouteEvent>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the user last *did* something here: sent a message, answered a
    /// question, paused, stopped, resumed, or messaged a subagent.
    /// `updated_at` moves on every agent step, so it cannot order a list — a
    /// background chat working through a hundred tool calls would shove a chat
    /// the user just opened to the bottom. This is what a chat list sorts on.
    #[serde(default = "epoch")]
    pub touched_at: DateTime<Utc>,
    /// False while `sub_items`/`sub_msgs` are still on disk rather than in memory.
    ///
    /// These two are ~97% of a task file's bytes (subagent timelines and their
    /// model conversations) and nothing at boot needs them: the sidebar reads
    /// `summary()`, which skips both. So `load_tasks` parses a header and leaves
    /// the body behind, and `Harness::hydrate` reads the file again the first
    /// time a chat is actually opened. `load_tasks` sets this; it is never
    /// written to disk, and never persisted — so a task created in this session
    /// is hydrated by construction.
    ///
    /// It is load-bearing for correctness, not just speed. The saver serializes
    /// whatever is in memory and rewrites the whole file, so saving a task that
    /// was never hydrated would replace real subagent history with two empty
    /// maps — silent, permanent data loss. `Harness::task()` hydrates before
    /// handing out a `TaskRef`, which closes that for every save path, and
    /// `store::save_task` refuses a stub outright as a backstop.
    #[serde(skip)]
    pub hydrated: bool,
}

/// Tasks saved before this field existed fall back to their last real change, so
/// the first run after an upgrade doesn't float every chat to the top at once.
fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default()
}

fn yes() -> bool {
    true
}

fn assist_default() -> String {
    "default".into()
}

fn agents_default() -> Vec<String> {
    vec!["explore".into(), "general".into()]
}

/// Order marker stamped on every summary this process builds, so the UI can tell
/// a *stale* one from a newer one instead of walking a chat's state backwards.
///
/// A summary reaches the UI by two routes: pushed as a live event, or read back
/// as a snapshot — `app_boot`'s task list, `task_open` when a chat is opened.
/// Both are built from the same in-memory task, but they reach the frontend out
/// of order: a snapshot is built at one instant and applied after whatever
/// events the backend published while the request was still in flight. Applying
/// that snapshot then put the chat back where it was before those events, and
/// nothing republished it — the chat sat at "running" after it had finished
/// (a spinner that never stops), or at "waiting" after the answer had been
/// given, until the next unrelated event for that chat. A reload re-ran the
/// same race, which is why it came back "sometimes" and sometimes outlived it.
///
/// Process-local and monotonic, and stamped as each summary is built, so a
/// higher `rev` means "built later, from state at least as new" — exactly the
/// comparison `src/store.ts` makes. Deliberately not a clock: two updates in the
/// same millisecond still order, and a system clock stepping backwards cannot
/// make fresh state look stale.
static SUMMARY_REV: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Everything about a task except the heavy timeline/history — what lists render.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    /// Named by hand: the agent's `set_title` is refused on this chat.
    pub titled: bool,
    pub status: String,
    pub waiting_kind: Option<String>,
    pub step: String,
    pub project: String,
    pub cwd: String,
    pub branch: String,
    pub base_branch: String,
    pub worktree: bool,
    /// Present when this chat runs in a docker workspace.
    pub docker: Option<docker::DockerEnv>,
    /// The container's live state as the harness last saw it.
    pub container: Option<docker::State>,
    pub model: String,
    pub effort: usize,
    pub perm: String,
    pub plan: bool,
    pub ultra: bool,
    pub ultra_wt: bool,
    /// The ULTRATHREAD X ladder, when this task runs one.
    pub ultra_x: Option<store::UltraX>,
    pub subagents: bool,
    pub goal: Option<Goal>,
    pub assist: String,
    pub agents: Vec<String>,
    pub pending: serde_json::Map<String, Value>,
    pub paused: Option<Pause>,
    pub serving: String,
    pub route: String,
    pub unpaused: bool,
    pub busy: usize,
    /// The background commands still alive, so the UI can name what the pause
    /// does *not* stop. See [`Task::bg_now`].
    pub bg_now: Vec<shell::BgInfo>,
    pub archived: bool,
    pub hidden: bool,
    /// There is something in this chat nobody has looked at yet: the sidebar's
    /// blue dot. The backend owns it (`Harness::attention` sets it, opening the
    /// chat clears it) so every client draws the same dot, including a remote
    /// browser that has only just loaded the page.
    pub unread: bool,
    pub forked_from: Option<String>,
    pub pinned: bool,
    /// This chat was left on its Artificts surface. Per chat, and saved with it.
    pub artifict_open: bool,
    pub order: f64,
    pub todos: Vec<Todo>,
    pub subs: Vec<SubInfo>,
    pub usage: Usage,
    pub context_window: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the user last did something in this chat. The chat list sorts on
    /// this, not on `updated_at` (see `Task::touched_at`).
    pub touched_at: DateTime<Utc>,
    /// Which build of this summary this is; see `SUMMARY_REV`. The UI keeps it
    /// so a snapshot built before a live event cannot overwrite that event.
    pub rev: u64,
}

impl Task {
    /// The user is looking at this chat now, so there is nothing here left to
    /// look at. Returns whether that changed anything, so a caller only publishes
    /// a summary when the dot actually goes out.
    ///
    /// A blocking wait is the exception, and it is load-bearing: a question or an
    /// approval stays marked until the *backend* ends the wait (`wait_for_user`),
    /// so opening a chat to read the request, then leaving without answering,
    /// does not quietly mark the request as handled.
    pub fn mark_seen(&mut self) -> bool {
        let blocking = matches!(
            self.waiting_kind.as_deref(),
            Some("question") | Some("approval")
        );
        if self.unread && !blocking {
            self.unread = false;
            return true;
        }
        false
    }

    /// Claim a subagent for a resume: check it's free (and ours, when we're a
    /// subagent), mark it running and hand back its conversation with the new
    /// turn appended — all under one lock, so nothing can start the same
    /// subagent twice and end up driving one transcript from two places.
    /// `None` = already running; no conversation = nothing to resume.
    pub fn try_resume_sub(
        &mut self,
        id: &str,
        by: Option<&str>,
        note: &str,
    ) -> Option<Vec<Message>> {
        let sub = self.subs.iter_mut().find(|s| s.id == id)?;
        if sub.status == "running" || by.is_some_and(|me| sub.parent != me) {
            return None;
        }
        sub.status = "running".into();
        sub.meta = "resuming".into();
        let mut msgs = self.sub_msgs.get(id)?.clone();
        if msgs.is_empty() {
            return None;
        }
        runner::push_user_blocks(&mut msgs, vec![json!({"type": "text", "text": note})]);
        self.sub_msgs.insert(id.to_string(), msgs.clone());
        Some(msgs)
    }
    /// Working directory + branch an agent works in: its own worktree, or the task's.
    pub fn cwd_for(&self, sub: Option<&str>) -> (String, String) {
        match sub
            .and_then(|id| self.subs.iter().find(|s| s.id == id))
            .filter(|s| !s.cwd.is_empty())
        {
            Some(s) => (s.cwd.clone(), s.branch.clone()),
            None => (self.cwd.clone(), self.branch.clone()),
        }
    }

    pub fn summary(&self) -> TaskSummary {
        self.summary_with(self.subs.clone())
    }

    /// The same summary, with every subagent's report left out of the clone.
    ///
    /// `emit_summary` cleared the reports *after* `summary()` had already cloned
    /// them, so the allocation happened anyway and was then thrown away. Each
    /// report can run to 20k characters, and this fires on every status change of
    /// every agent — so a 20-agent swarm built and dropped several hundred
    /// kilobytes of text per event to publish a step line. The field is stripped
    /// on the wire either way (the UI fetches a report by id when it needs one),
    /// so building it empty is the same summary for less work.
    pub fn summary_without_reports(&self) -> TaskSummary {
        let subs = self
            .subs
            .iter()
            .map(|s| SubInfo {
                id: s.id.clone(),
                effort: s.effort,
                cwd: s.cwd.clone(),
                branch: s.branch.clone(),
                role: s.role.clone(),
                task: s.task.clone(),
                status: s.status.clone(),
                meta: s.meta.clone(),
                model: s.model.clone(),
                serving: s.serving.clone(),
                started: s.started,
                // Never copied: this is the whole point of the variant. The
                // struct literal is exhaustive, so a field added to `SubInfo`
                // fails to compile here rather than silently going missing.
                report: String::new(),
                call_id: s.call_id.clone(),
                item_id: s.item_id.clone(),
                background: s.background,
                parent: s.parent.clone(),
                depth: s.depth,
            })
            .collect();
        self.summary_with(subs)
    }

    fn summary_with(&self, subs: Vec<SubInfo>) -> TaskSummary {
        TaskSummary {
            // Every summary this process hands out — live event or snapshot —
            // is stamped here, so two of them can be ordered. See `SUMMARY_REV`.
            rev: SUMMARY_REV.fetch_add(1, Ordering::SeqCst),
            id: self.id.clone(),
            title: self.title.clone(),
            titled: self.titled,
            status: self.status.clone(),
            waiting_kind: self.waiting_kind.clone(),
            step: self.step.clone(),
            project: self.project.clone(),
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
            base_branch: self.base_branch.clone(),
            worktree: self.worktree,
            docker: self.docker.clone(),
            container: self
                .docker
                .as_ref()
                .map(|_| docker::live(&self.id).map(|l| l.state).unwrap_or_default()),
            model: self.model.clone(),
            effort: self.effort,
            perm: self.perm.clone(),
            plan: self.plan,
            ultra: self.ultra,
            ultra_wt: self.ultra_wt,
            ultra_x: self.ultra_x.clone(),
            subagents: self.subagents,
            goal: self.goal.clone(),
            assist: self.assist.clone(),
            agents: self.agents.clone(),
            pending: self.pending.clone(),
            paused: self.paused.clone(),
            serving: self.serving.clone(),
            route: self.route.clone(),
            unpaused: self.unpaused,
            busy: self.busy,
            bg_now: self.bg_now.clone(),
            archived: self.archived,
            hidden: self.hidden,
            unread: self.unread,
            forked_from: self.forked_from.clone(),
            pinned: self.pinned,
            artifict_open: self.artifict_open,
            order: self.order,
            todos: self.todos.clone(),
            subs,
            usage: self.usage.clone(),
            context_window: router::context_window(&self.model, &self.route),
            created_at: self.created_at,
            updated_at: self.updated_at,
            touched_at: self.touched_at,
        }
    }
}

/// The model one agent runs on, right now.
///
/// Resolved per request rather than stamped on the agent at spawn: a chat
/// swapped mid-run has to reach the subagents it has *already* launched, not
/// only the ones it launches next. Precedence, biggest decision first:
///
/// 1. a model somebody explicitly chose for this agent — `SubInfo.model`, set
///    by its own panel, by a swap, or at launch from its agent type's model,
/// 2. its ULTRATHREAD X layer, read live so editing the ladder reaches the
///    agents already running on it,
/// 3. the chat's model — so an agent nobody has said otherwise follows the chat.
///
/// Deliberately a pure function of the task, and deliberately not going through
/// `model_map`. That map records "this chat swapped *away from* these models",
/// and it belongs to the one place a launch starts from an agent type's model
/// (see `spawn_sub`). Applying it here as well meant a swap in this chat quietly
/// undid a later, deliberate pick of that same model — the picker took the click
/// and the agent carried on with the model the user had just replaced.
///
/// `sub` is `None` for the main agent, which has no layer and so always lands on
/// the chat's model. `fallback` is what the caller already had in hand, used only
/// for a task whose model is somehow empty.
///
/// The main agent (`sub` is `None`) reads the chat's model and nothing else: a
/// swap made mid-turn waits for the user's next message, so letting the main
/// agent see the pending value would apply it a turn early — the one thing the
/// `*` badge promises it does not. A subagent has no such turn boundary to
/// respect, so it follows a queued swap and the whole chat moves together.
pub fn resolve_model(t: &Task, sub: Option<&SubInfo>, depth: u8, fallback: &str) -> String {
    if let Some(m) = sub.map(|s| s.model.as_str()).filter(|m| !m.is_empty()) {
        return m.to_string();
    }
    if let Some(m) = t
        .ultra_x
        .as_ref()
        .filter(|x| x.on())
        .and_then(|x| x.layer(depth))
        .map(|l| l.model.as_str())
        .filter(|m| !m.is_empty())
    {
        return m.to_string();
    }
    let chat = match sub {
        // A subagent following the chat, including a swap already queued for the
        // next message — the user is moving the whole chat, not just its lead.
        Some(_) => chat_model_of(t),
        None => None,
    };
    chat.or_else(|| (!t.model.is_empty()).then(|| t.model.clone()))
        .unwrap_or_else(|| fallback.to_string())
}

/// A chat's model for anything that is not the main agent's own turn: a swap
/// queued for its next message counts, so a subagent following the chat moves
/// with it instead of staying on a model the user has already replaced.
pub fn chat_model_of(t: &Task) -> Option<String> {
    t.pending
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|m| !m.is_empty())
        .map(String::from)
}

/// The reasoning effort one agent runs at right now (0 = Max … 4 = Low), for
/// anything that is not a request being built: the swap dialog, and a subagent
/// that inherits rather than picks.
///
/// Precedence mirrors the turn loop exactly, or the dialog would report a level
/// nothing actually sends: the agent's own override, else its agent type's, else
/// the chat's — with a read-only type held to at most Medium, which is the one
/// case where the answer is not simply "the chat's".
pub fn resolve_effort(
    t: &Task,
    sub: Option<&SubInfo>,
    def_effort: Option<usize>,
    read_only: bool,
) -> usize {
    match sub {
        None => t.effort,
        Some(s) => match s.effort.or(def_effort) {
            Some(e) => e.min(4),
            None if read_only => t.effort.max(3),
            None => t.effort,
        },
    }
}

/// Record the spend of a request that is not an agent turn — a drafted commit
/// message, a permission review — in the same monthly ledger agent turns use.
///
/// The router has already recorded the detailed sample through `Stats::record`;
/// this is the number the budget cap reads, so a side request that skipped it
/// would quietly under-count what the user has spent this month.
pub(crate) async fn record_side_spend(h: &Harness, cost: f64, tokens: u64) {
    let month = {
        let mut settings = h.settings.write().await;
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        *settings.spend.entry(day).or_insert(0.0) += cost;
        settings.tokens_month += tokens;
        settings.month_spend()
    };
    h.save_settings_debounced().await;
    let tokens = h.settings.read().await.tokens_month;
    (h.bus)(
        "ol://usage",
        serde_json::json!({ "month": month, "tokens": tokens }),
    );
}

/// A broadcast the user sent, and which agents have acted on it since. The user
/// gets a running list of who took it on, so a "undo the lighting" lands where
/// it should and the agents it never concerned stay quiet.
#[derive(Clone, Default, Serialize)]
pub struct Broadcast {
    pub text: String,
    /// Who it was sent to ("main" plus every working subagent's id).
    pub who: Vec<String>,
    /// Short ids of the agents that acted on it, in the order they did.
    pub replied: Vec<String>,
}

impl Broadcast {
    pub fn took(&self, id: &str) -> bool {
        self.replied.iter().any(|x| x == id)
    }
}

/// One message the user pressed Alt-Enter on while the agent was still working:
/// it waits for the end of the turn instead of steering into it. The queue is a
/// single ordered list so the UI can reorder it, and every entry names the
/// timeline item that stands for it, so edit and remove act on the exact message
/// the row shows.
#[derive(Debug, Clone)]
pub struct QueuedMsg {
    /// The `user` item this message shows up as, queued or not.
    pub item_id: String,
    /// Send order. Also the order the UI sorts by, and what a move rewrites.
    pub seq: u64,
    /// Content blocks in Anthropic shape: text, or text with images, so a photo
    /// queues the same way a message does.
    pub blocks: Vec<Value>,
}

/// The ceiling on how many images are pinned for one agent on top of whatever
/// the user actually sent. See `Harness::note_images`, which enforces it.
pub const INBOX_IMAGES: usize = 8;

/// A live park and the watcher generation that owns it. Deliberately runtime-only:
/// this does not claim that a trigger survives an application restart.
///
/// `pub` (not `pub(crate)`) because `Runtime` is publicly reachable now that
/// `agent` is a public module — the headless binary needs the harness — and a
/// `pub` field cannot expose a more-private type without a lint warning.
#[derive(Default)]
pub struct WakeState {
    pub(crate) parked: bool,
    pub(crate) generation: u64,
    pub(crate) deadline_ms: u64,
    pub(crate) cancel: Option<CancellationToken>,
}

/// Live, non-persisted state for a task.
#[derive(Default)]
pub struct Runtime {
    pub cancel: std::sync::Mutex<Option<CancellationToken>>,
    /// Serializes permission reviews. A turn that issues ten risky calls would
    /// otherwise fire ten reviewer requests at once — a cost spike and exactly
    /// the shape of burst providers rate-limit, which would make the reviewer
    /// fail closed on requests that were perfectly answerable. Nothing else is
    /// held under this lock, and Esc still cancels a review in flight.
    pub review: Mutex<()>,
    pub pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    /// The Alt-Enter messages, oldest first — the list above the composer is a
    /// view of exactly this. A plain message typed mid-turn steers straight away
    /// and never lands here.
    pub queue: Mutex<Vec<QueuedMsg>>,
    /// Source of `QueuedMsg::seq`.
    pub send_seq: std::sync::atomic::AtomicU64,
    /// Set when the user messages a *paused* chat. The resume that follows is a
    /// continuation, not a fresh turn, so its model call replays the request the
    /// chat froze on — built before the new text existed. The router checks this
    /// once the chat thaws and abandons that attempt, so the replacement request
    /// is built from history that includes the message. Plain mid-run steering
    /// doesn't set it: there the message rides along as a reminder.
    pub steered: std::sync::atomic::AtomicBool,
    /// Harness notes per agent ("main" or a sub id), delivered as <system-reminder> on its next request.
    pub inbox: Mutex<HashMap<String, Vec<String>>>,
    /// A broadcast the user sent, and which agents have acted on it since
    /// ("main" or a sub id). Armed by `broadcast`, reported by `who_replied`.
    pub broadcast: Mutex<Option<Broadcast>>,
    /// Images the user pasted into an agent's own panel, waiting for that
    /// agent's next request. Kept apart from `inbox` (text) so a picture
    /// arrives as a real image block instead of a caption.
    pub inbox_images: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Deferred tools this agent has loaded with `tool_search` ("main" or a sub
    /// id), oldest first. Copilot's rule and the right one: a loaded tool stays
    /// declared for the rest of the conversation, so the model searches once
    /// instead of paying for a search per use.
    ///
    /// Live state, not `Task` state, and deliberately: it is a *presentation*
    /// decision over the frozen tool snapshot, and the snapshot itself is
    /// already re-derived from the task's frozen `mcp_tools` + `plugins` every
    /// request. So this is a cache of that derivation, not a second source of
    /// truth — a restart drops it, the next search rebuilds it, and until then
    /// the history is still valid (a `tool_use` for a tool that was declared
    /// when it was recorded replays fine whether or not it is declared now).
    /// Keeping it here also keeps it out of the task file, which is what makes
    /// this merge cleanly with every other agent adding `Task` fields.
    pub loaded_tools: Mutex<HashMap<String, Vec<String>>>,
    pub repeats: Mutex<HashMap<String, u32>>,
    /// Router rule counters and active holds by requesting agent.
    pub router: std::sync::Mutex<HashMap<String, router::AgentRoute>>,
    /// Startup/setup work for a newly-created chat, independent of background agent jobs.
    pub startup_cancel: std::sync::Mutex<Option<CancellationToken>>,
    /// Prevents follow-up sends from starting a turn before workspace setup and
    /// the initial prompt dispatch complete.
    pub startup_pending: std::sync::atomic::AtomicBool,
    pub startup_ready: tokio::sync::Notify,
    /// Synchronous deletion waits for startup cleanup before inspecting worktree state.
    pub startup_done: std::sync::Condvar,
    pub running: std::sync::atomic::AtomicBool,
    /// Parent token for background subagents (outlives a single main run; Esc replaces it).
    pub bg_cancel: std::sync::Mutex<CancellationToken>,
    /// Foreground commands in flight.
    pub fg: std::sync::atomic::AtomicUsize,
    /// Pulled by a force pause: kills in-flight commands without stopping the agents.
    pub force: std::sync::Mutex<CancellationToken>,
    /// Nudges the task writer. One per harness (keyed on the empty id), not one
    /// per task: the writer drains every dirty task in one pass.
    pub save_wake: tokio::sync::Notify,
    /// Serialize task snapshots and their writes together, always before taking
    /// the task lock. Otherwise a delayed debounced snapshot can overwrite a
    /// notice or dismissal that has already acknowledged durable success.
    pub task_save: Mutex<()>,
    /// Live wake state. Deliberately runtime-only: a park does not claim to
    /// survive application restart. `generation` fences off stale watchers.
    pub wake_state: std::sync::Mutex<WakeState>,
    /// Keepalive pings sent for this task so far, capped per task by the
    /// settings. Kept here rather than on the `Task` because it is pure runtime
    /// bookkeeping — a restart has no reason to remember how many keepalives the
    /// previous session paid for.
    pub keepalive_pings: std::sync::atomic::AtomicU32,
    /// Cancels the keepalive loop. Held here, not in the spawned task, because
    /// an `interrupt` has to reach it through `stop_tokens` — a keepalive that
    /// outlived a stop would keep spending the user's money on a chat they
    /// deliberately ended.
    pub keepalive_cancel: std::sync::Mutex<Option<CancellationToken>>,
    /// Each running subagent's own cancel token, by sub id. Stopping one agent
    /// cancels only its token (and whatever it spawned, which hangs off it), so
    /// the rest of the chat keeps going. Esc still reaches all of them, through
    /// the parent token each one was created from.
    pub sub_cancel: std::sync::Mutex<HashMap<String, CancellationToken>>,
    /// The newest queued shadow-checkpoint job for this task (`None` = none
    /// ever queued); it settles to `true` once that job and every job queued
    /// before it are done. See [`Runtime::queue_checkpoint`].
    pub checkpoint_chain: std::sync::Mutex<Option<tokio::sync::watch::Receiver<bool>>>,
    /// Deletion fences out late end-of-run captures before draining the chain.
    pub checkpoints_closed: std::sync::atomic::AtomicBool,
}

impl Runtime {
    /// Run a blocking shadow-checkpoint job after every job queued before it,
    /// without making the caller wait, and return a handle that settles when
    /// it is done.
    ///
    /// The send path's user snapshot is the reason this exists. On a new chat
    /// it is `init`, config, `status`, `add -A` of the whole checkout and a
    /// commit — seconds on a large repository — and it used to be awaited
    /// before the run even started, so the first model request sat behind it.
    /// It now runs here, and everything whose correctness depends on it having
    /// landed waits on [`Runtime::checkpoints_settled`] instead: every tool
    /// (a command or edit that ran first would be committed as the *user's*
    /// work, and rewind would then refuse to put it back), the end-of-run
    /// capture, rewind, fork and deletion.
    ///
    /// Chained rather than merely serialised: `checkpoint`'s write lock keeps
    /// two git processes off one index, but it is not FIFO, and the `turns`
    /// ledger is read last-wins — a later touch's snapshot overtaking an
    /// earlier one would record the timeline out of order.
    pub fn queue_checkpoint(
        &self,
        job: impl FnOnce() + Send + 'static,
    ) -> tokio::sync::watch::Receiver<bool> {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let previous = {
            let mut chain = self
                .checkpoint_chain
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.checkpoints_closed.load(Ordering::SeqCst) {
                let _ = tx.send(true);
                return rx;
            }
            chain.replace(rx.clone())
        };
        tokio::spawn(async move {
            if let Some(mut previous) = previous {
                // `Err` = that job's sender is gone (it panicked or the runtime is
                // shutting down): nothing more is coming from it either way.
                let _ = previous.wait_for(|done| *done).await;
            }
            let _ = tokio::task::spawn_blocking(job).await;
            let _ = tx.send(true);
        });
        rx
    }

    /// Prevent late jobs from recreating a deleted task's checkpoint store.
    /// Taken under the queue lock so a concurrent queue is either included in
    /// the drain or rejected, never registered after the drain observed it.
    pub fn close_checkpoints(&self) {
        let _chain = self
            .checkpoint_chain
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.checkpoints_closed.store(true, Ordering::SeqCst);
    }

    /// Wait until every checkpoint job queued so far has finished. Returns at
    /// once when none is pending, which is the steady state: only the first
    /// tool after a send ever actually waits.
    pub async fn checkpoints_settled(&self) {
        let pending = self
            .checkpoint_chain
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(mut rx) = pending {
            let _ = rx.wait_for(|done| *done).await;
        }
    }

    /// [`Runtime::checkpoints_settled`], abandoned if `cancel` fires first.
    /// `false` = cancelled while still waiting, so the caller must not go on to
    /// do whatever it was waiting to do. A snapshot is bounded, but Esc has to
    /// work during one on a huge checkout too.
    pub async fn checkpoints_settled_or(&self, cancel: &CancellationToken) -> bool {
        tokio::select! {
            biased;
            _ = self.checkpoints_settled() => true,
            _ = cancel.cancelled() => false,
        }
    }
}

impl Runtime {
    /// Throw away everything that was addressed to an agent but never made it
    /// into history: the Alt-Enter queue, the mid-turn notes, the photos pasted
    /// into a running chat, and the flag that tells a frozen router to rebuild
    /// its request.
    ///
    /// These live outside `messages` by design — a note rides the agent's *next*
    /// request rather than becoming a turn of its own — which is exactly why
    /// truncating the transcript does not reach them. A rewind (edit a message
    /// and press Send) throws the whole future away, so leaving them behind
    /// splices a conversation that no longer exists onto the branch you just
    /// rewound to: the agent is told about a message you deleted, and a photo
    /// you re-decided about reappears in the rebuilt context.
    ///
    /// `steered` goes too, and it is the one that also guards a *paused* chat:
    /// a frozen router holds a request built from the history that existed when
    /// it froze, and answering that after a rewind means replying to a turn the
    /// user has already replaced. Clearing it lets the next request be built
    /// from the rewound history.
    ///
    /// `broadcast` is left alone: it is the "who has taken this on" tracker for a
    /// notice in `items`, not a channel into the model, and a broadcast sent
    /// before the rewind point is still on screen. (A reply recorded against a
    /// notice the rewind discarded is harmless — `mark_replied` patches by
    /// finding the notice, and skips it when there is nothing left to patch.)
    pub async fn drop_undelivered(&self) {
        self.queue.lock().await.clear();
        self.inbox.lock().await.clear();
        self.inbox_images.lock().await.clear();
        // Tools loaded by a `tool_search` the user just rewound past go with the
        // rest: the request that carried their schema is no longer in history,
        // and keeping them declared would leave a tool in the list whose only
        // explanation was deleted.
        self.loaded_tools.lock().await.clear();
        self.steered
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Whether an attention payload is something the user has not seen yet — the
/// rule behind the blue dot.
///
/// The same set `describe()` in `src/notify.ts` words into a toast, and
/// deliberately the same two exclusions: a `stopped` run is the user's own
/// keystroke, and a parked chat (`waiting`) is awaiting something that will
/// announce itself when it fires, so neither is news. A `paused` event is news
/// only when the app raised the pause on its own; a manual one is what the user
/// was looking at when they pressed it. Exported for tests.
pub fn attention_is_news(payload: &Value) -> bool {
    match payload["kind"].as_str().unwrap_or("") {
        "done" | "failed" | "question" | "approval" | "nonblocking" | "notice" => true,
        "paused" => payload["pause"].as_str() != Some("manual"),
        _ => false,
    }
}

pub type TaskRef = Arc<Mutex<Task>>;

/// Where UI events go (the Tauri window in the app, a recorder in tests).
pub type Bus = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct Harness {
    pub bus: Bus,
    pub tasks: RwLock<HashMap<String, TaskRef>>,
    pub runtimes: std::sync::Mutex<HashMap<String, Arc<Runtime>>>,
    pub settings: RwLock<store::Settings>,
    pub bg: shell::BgManager,
    pub mcp: mcp::McpManager,
    pub http: reqwest::Client,
    /// Wakes anything waiting on a pause flag.
    pub pause_bell: tokio::sync::Notify,
    pub accts: accounts::AccountRt,
    /// API key cooldown state (key pools).
    pub keys: accounts::KeyRt,
    pub stats: stats::Stats,
    /// Tasks with an unwritten change. One writer task drains this, so the many
    /// saves a long run makes cost a map insert rather than a full rewrite of
    /// the transcript each time.
    pub dirty: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The writer task itself, started on the first `save_task`.
    pub saver: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Settings have an unwritten change. Same writer, same reason as `dirty`:
    /// a full rewrite of `settings.json` (two fsyncs, every account token in
    /// it) per model turn is not a cost worth paying for a spend figure.
    pub settings_dirty: std::sync::atomic::AtomicBool,
    /// This harness behind an `Arc`, set by the app at startup so the writer has
    /// something to call back into. Tests build a `Harness` directly and leave
    /// it `None`: there `save_task` has no writer, so callers save inline.
    pub me: std::sync::OnceLock<Arc<Harness>>,
}

/// How long a save waits for more saves before it actually writes. A long run
/// asks for one after every tool round, and those are seconds apart; the window
/// is short enough that a crash loses almost nothing.
pub const SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(1_500);

impl Harness {
    pub fn runtime(&self, id: &str) -> Arc<Runtime> {
        self.runtimes
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    /// A chat's live state *only if it already has one*, without creating it.
    ///
    /// `runtime()` inserts, which is right for anything about to work on a chat
    /// and wrong for anything that merely reports on one: `list_chats` walks
    /// every chat the user has, and through `runtime()` that would leave a
    /// `Runtime` — with its inbox, its queue and its pending approval map —
    /// behind for every one of them, for a read the model may make on every turn
    /// of every run. A chat with nothing live this session simply reads `None`,
    /// which callers treat as "nothing running".
    pub fn runtime_if(&self, id: &str) -> Option<Arc<Runtime>> {
        self.runtimes.lock().unwrap().get(id).cloned()
    }

    /// Forget a deleted chat's live state, so the map does not keep one
    /// `Runtime` per task id for the life of the process.
    ///
    /// `runtime()` inserts on demand and is called from everywhere — every
    /// step, every item patch, every background command — so a chat the user
    /// opened once and then deleted still had its `Runtime` parked here
    /// afterwards, holding whatever the Alt-Enter queue, the inbox and the
    /// pasted-image buffer were left holding. Nothing ever removed them: the map
    /// had exactly one accessor, `runtime()`, and no `remove`, so deleting chats
    /// was the one action that made it grow.
    ///
    /// Deliberately *not* called for a chat that is merely idle. A `Runtime`
    /// holds the cancellation tokens of in-flight work and the `pending` map that
    /// approvals and questions are answered through, so dropping one while a run
    /// still holds it would leave those prompts with nobody to answer them.
    /// `purge_task` has already stopped the tokens and killed the background
    /// commands before this runs, which is what makes it safe there and nowhere
    /// else.
    ///
    /// The empty id is the harness's own runtime (the shared task writer) and is
    /// never dropped: it has no chat to be deleted with.
    pub fn forget_runtime(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.runtimes.lock().unwrap().remove(id);
    }

    /// The one accessor every task read goes through, and the single place a
    /// task body can be brought into memory.
    ///
    /// Hydration lives here rather than in the commands because this is the
    /// choke point that the saver also passes through: `save_task_now` and the
    /// debounced writer both call `self.task()`, so a task cannot be written to
    /// disk before it has been read. That is the whole defence against a
    /// half-loaded task silently overwriting its own subagent history — see
    /// `Task::hydrated`.
    pub async fn task_ref(&self, id: &str) -> Result<TaskRef, String> {
        self.tasks
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no task {id}"))
    }

    pub async fn task(&self, id: &str) -> Result<TaskRef, String> {
        let r = self.task_ref(id).await?;
        self.ensure_hydrated(&r).await?;
        Ok(r)
    }

    /// One subagent's own transcript. `task_open` only sends the main timeline,
    /// so the panel asks for this when it opens. Hydrates first: subagent bodies
    /// stay on disk until something reads them.
    pub async fn sub_transcript(&self, task_id: &str, sub: &str) -> Result<Vec<Item>, String> {
        let r = self.task(task_id).await?;
        let t = r.lock().await;
        Ok(t.sub_items.get(sub).cloned().unwrap_or_default())
    }

    /// Splice a task's subagent maps back in, once, if they are still on disk.
    ///
    /// Idempotent and safe to race: the flag is checked under the task lock and
    /// the read happens under it too, so a second caller waits rather than
    /// parsing the same 700 MB twice. A file that has gone missing or turned to
    /// garbage is *not* fatal — the header is still perfectly usable, and the
    /// task would otherwise be unreachable for the rest of the session because
    /// a failed hydration would be retried on every single access.
    pub async fn ensure_hydrated(&self, r: &TaskRef) -> Result<(), String> {
        let already = { r.lock().await.hydrated };
        if already {
            return Ok(());
        }
        let id = { r.lock().await.id.clone() };
        let path = store::task_path(&id);
        // Parsed on the blocking pool, never inline: one chat can be 700 MB and
        // that is ~1.4s of CPU. On the async runtime it would stall every other
        // task's streaming deltas and every IPC call behind it; on the pool the
        // UI keeps running and the tokens keep arriving while it happens. The
        // `await` below is the point — `task_get` waits for it, so the chat
        // still opens complete, just without freezing everything else on the way.
        let loaded = tokio::task::spawn_blocking(move || {
            store::pool().install(|| store::read_task_full(&path))
        })
        .await
        .map_err(|e| format!("hydrating {id} failed: {e}"))?;
        let mut g = r.lock().await;
        if g.hydrated {
            // Someone else won the race while this thread was reading.
            return Ok(());
        }
        match loaded {
            Ok(fresh) => {
                // Take the body off the freshly-parsed copy, not the stub: the
                // in-memory header may have been mutated since boot (a pause, a
                // rename, a model swap), and that has to survive hydration.
                g.sub_items = fresh.sub_items;
                g.sub_msgs = fresh.sub_msgs;
                store::hydrate_task(&mut g);
                Ok(())
            }
            Err(e) => {
                // Leave the stub in place but stop retrying: a chat whose file
                // vanished keeps its sidebar entry and its transcript, and only
                // the (already missing) subagent panels are empty.
                eprintln!("[openleash] could not load subagent history for {id}: {e}");
                g.hydrated = true;
                Ok(())
            }
        }
    }

    pub fn emit(&self, task_id: &str, kind: &str, payload: Value) {
        (self.bus)(
            "ol://event",
            json!({"task_id": task_id, "kind": kind, "payload": payload}),
        );
    }

    /// Publish that an artifict changed so any open workspace reloads it live.
    /// The payload is only ids: the workspace re-reads through `artifict_get`,
    /// so a stale preview is never shown from an event alone. Called from both
    /// the user-facing IPC commands and the agent's own artifict tools, which is
    /// what makes a revision made mid-turn appear without a manual Refresh.
    pub fn emit_artifict(&self, task_id: &str, artifict: &crate::artificts::Artifict) {
        self.emit(
            task_id,
            "artifict",
            json!({
                "artifict_id": artifict.id,
                "version_id": artifict.current_version_id,
            }),
        );
    }

    /// Keep the plan an agent is proposing with the project it is about.
    ///
    /// Plan mode writes nothing to the checkout, so the plan itself is the
    /// deliverable — and a plan that lives only in the transcript that produced
    /// it is lost to every other chat about this project and to a rewound
    /// timeline. This records it as a markdown artifict like any other, which
    /// also gives it revisions: a plan the user sends back for changes grows a
    /// version instead of a second artifict.
    ///
    /// Best-effort on purpose. A task with no project folder, a remote task, or
    /// a full artifict store must not cost the user the approval card they are
    /// waiting on, so failures here return `None` and the flow continues.
    pub async fn save_plan_artifict(
        &self,
        task_id: &str,
        plan: &str,
    ) -> Option<crate::artificts::Artifict> {
        let (project, title, existing) = {
            let task = self.task(task_id).await.ok()?;
            let t = task.lock().await;
            (t.project.clone(), t.title.clone(), t.plan_artifict.clone())
        };
        let fallback = if title.trim().is_empty() {
            "Plan".to_string()
        } else {
            format!("Plan · {}", title.trim())
        };
        let artifict =
            crate::artificts::save_plan(&project, existing.as_deref(), &fallback, plan).ok()?;
        self.update_task(task_id, |t| t.plan_artifict = Some(artifict.id.clone()))
            .await;
        self.emit_artifict(task_id, &artifict);
        Some(artifict)
    }

    /// Whether the *global* pause is what is holding this task — a task that
    /// opted out of it, or that is not working, is not held by it.
    ///
    /// This is the one place that decides it, and the UI mirrors it in
    /// `isPaused` (`ui/Paused.tsx`). It is a function of the task and the flag
    /// alone so callers can read it off a `TaskSummary` they already hold, without
    /// a second pass over the tasks.
    pub fn held_by_global(summary: &TaskSummary, paused_all: bool) -> bool {
        paused_all
            && !summary.unpaused
            && (summary.status == "running" || summary.status == "waiting")
    }

    /// Global or per-task pause in effect.
    ///
    /// The global pause only holds chats that were *working* when it landed, which
    /// is the same rule the paused list and the session banner use (`isPaused` in
    /// `ui/Paused.tsx`). Without the status half here the two disagreed: a chat at
    /// `idle` is frozen by this predicate but looks idle everywhere in the UI, so a
    /// brand-new chat created under "Pause all" sat there with no reply and nothing
    /// on screen saying it was waiting. It is frozen the moment it starts running,
    /// because a run only ever begins from a message the user has just sent.
    pub async fn is_paused(&self, task_id: &str) -> bool {
        let all = self.settings.read().await.paused_all;
        match self.task(task_id).await {
            Ok(t) => {
                let t = t.lock().await;
                t.paused.is_some() || Self::held_by_global(&t.summary(), all)
            }
            Err(_) => all,
        }
    }

    /// Drop the global pause once it is holding nothing, and say whether it did.
    ///
    /// The flag is sticky: it lives in `settings.json`, and a quit sets it, so a
    /// clean shutdown leaves the freeze on. But what it *holds* is narrow — only
    /// chats that are working, per [`Harness::held_by_global`]. A chat that
    /// finished, was stopped, or was closed out by the next launch drops out of
    /// the freeze while the flag stays on, and a flag holding nothing is still
    /// read as a global pause by everything except the paused list, which asks.
    /// That disagreement is the bug: the banner said "Everything is paused" over
    /// a list reading "Nothing is frozen right now", which is what a user with
    /// nothing paused sees after reopening the app.
    ///
    /// So the flag is cleared as soon as there is nothing left for it to hold.
    /// `tasks_resume` already made this test after a partial lift; doing it here
    /// as well means every other route to a stranded flag — a per-chat resume
    /// that was the last one holding it, and a restart — reconciles too.
    ///
    /// Every task is read through a blocking lock rather than `try_lock`, because
    /// guessing wrong here releases chats the user left frozen: a chat that was
    /// momentarily locked has to count as held, not be skipped.
    pub async fn clear_stranded_global_pause(&self) -> bool {
        if !self.settings.read().await.paused_all {
            return false;
        }
        let refs: Vec<_> = {
            let tasks = self.tasks.read().await;
            tasks.values().cloned().collect()
        };
        for t in &refs {
            // Already known on, so the flag is passed through rather than read
            // again under the lock: the predicate is the one place that says
            // what the flag holds, and both callers have to agree with it.
            if Self::held_by_global(&t.lock().await.summary(), true) {
                return false;
            }
        }
        let mut s = self.settings.write().await;
        if !s.paused_all {
            return false;
        }
        s.paused_all = false;
        s.paused_reason.clear();
        store::save_settings(&s);
        let out = serde_json::to_value(crate::public_settings(&s)).unwrap_or_default();
        drop(s);
        (self.bus)("ol://settings", out);
        true
    }

    /// Block while paused. Returns false if cancelled.
    pub async fn wait_unpaused(&self, task_id: &str, cancel: &CancellationToken) -> bool {
        let mut waited: Option<DateTime<Utc>> = None;
        loop {
            let bell = self.pause_bell.notified();
            if !self.is_paused(task_id).await {
                if let Some(from) = waited {
                    self.note_unpaused(task_id, from).await;
                }
                return true;
            }
            waited.get_or_insert_with(Utc::now);
            tokio::select! {
                _ = bell => {},
                _ = cancel.cancelled() => return false,
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {},
            }
        }
    }

    /// The run-timeline side of a pause lifting, seen by a waiter that was held
    /// since `from`. Closes the chat's own pause; a global "Pause all" never
    /// opened one, so the first waiter to notice records it — later waiters
    /// find that span overlapping their wait and add nothing.
    async fn note_unpaused(&self, task_id: &str, from: DateTime<Utc>) {
        let Ok(t) = self.task(task_id).await else {
            return;
        };
        let settings = self.settings.read().await;
        let mut t = t.lock().await;
        // The pause may have been raised again after the waiter's first read.
        // Never let a stale waiter close the new pause (and never close parks).
        if t.paused.is_some() || Self::held_by_global(&t.summary(), settings.paused_all) {
            return;
        }
        let now = Utc::now();
        spans::close_waits(&mut t.spans, "pause", now);
        let covered = t.spans.iter().any(|s| {
            s.kind == "wait"
                && s.name == "pause"
                && s.start <= now
                && s.end.is_none_or(|e| e >= from)
        });
        if !covered {
            let mut span = spans::Span::new("wait", None, "pause").detail("global");
            span.start = from;
            span.end = Some(now);
            spans::push(&mut t.spans, span);
        }
    }

    pub async fn pause_task(&self, task_id: &str, kind: &str, reason: &str) {
        let (kind, reason) = (kind.to_string(), reason.to_string());
        let (kind_s, reason_s) = (kind.clone(), reason.clone());
        // A pause is when the cache keepalive *should* run — the chat freezes and
        // its cached prefix would otherwise go cold — so it is deliberately not
        // cancelled here. `stop_tokens` (a real stop) and `unpark` (a real wake)
        // are what end it, and the loop's own global-pause check keeps it from
        // pinging under "Pause all", where nothing is coming back for the cache.
        let newly_paused = self
            .update_task_quiet(task_id, |t| {
                if t.paused.is_none() {
                    // Only an actual pause-lifting transition closes this;
                    // other agents and background commands may still drain.
                    let span = spans::Span::new("wait", None, "pause").detail(&kind);
                    spans::push(&mut t.spans, span);
                    t.paused = Some(Pause {
                        reason,
                        kind,
                        since: Utc::now(),
                    });
                }
            })
            .await;
        // Only the transition into the paused state is news. A route can have
        // several requests watching the same task; once one parks it, the rest
        // must not announce the same pause again.
        if newly_paused {
            // Publish the state and wake any in-flight request before doing work
            // that can wait on filesystem snapshots or command bookkeeping. In
            // particular, `touch` may capture a checkpoint, which should not hold
            // up the pause announcement after the task summary already says it is
            // frozen.
            self.pause_bell.notify_waiters();
            // The same call decides whether this is the user's news or their own
            // keystroke — see `attention_is_news`.
            self.attention(
                task_id,
                json!({"task_id": task_id, "kind": "paused", "pause": kind_s, "reason": reason_s}),
            )
            .await;
        }
        // Recount on *every* pause, not only the first: this is the number the
        // banner is made of, so it is read when the banner appears rather than
        // trusted from whatever recounted last. A pause can land between one
        // recount and the next — a command that was already past the pause gate,
        // work starting as the chat freezes — and a chat frozen once already (by
        // the global pause, by an earlier pause raising a different reason) used to
        // return here before looking at the count at all. It can wait for another
        // task update, which is why it stays below the announcement above.
        self.refresh_busy(task_id).await;
        if !newly_paused {
            return;
        }
        self.save_task(task_id).await;
        // Pausing by hand is the user acting on this chat, so it comes to the
        // top. A pause the app raised on its own (a route ran out of models) is
        // not: that is the agent's news, and it must not reorder anything.
        if kind_s == "manual" {
            self.touch(task_id).await;
        }
    }

    /// Recount in-flight commands so the UI can show "Pausing…" until they drain.
    pub async fn refresh_busy(&self, task_id: &str) {
        let rt = self.runtime(task_id);
        let mut changed = false;
        let counted = self
            .update_task_quiet(task_id, |t| {
                // Read the live state *here*, under the task lock, rather than
                // before it. Every command start and end recounts, so recounts run
                // concurrently with the counts they publish — a swarm finishing
                // together queues several of them on this lock — and the value a
                // recount writes is whatever it read when it *started*. The one
                // that read first can be the one that queues behind the others and
                // writes last, and then the banner names a command that exited
                // minutes ago and nothing recounts it away, because nothing is
                // running any more. Read under the lock, the last write is the true
                // count by construction.
                let fg = rt.fg.load(std::sync::atomic::Ordering::SeqCst);
                let live: Vec<shell::BgInfo> = self
                    .bg
                    .list(task_id)
                    .into_iter()
                    .filter(|b| b.running)
                    .collect();
                let named: Vec<String> = live
                    .iter()
                    .map(|b| format!("{}: {}", b.id, b.cmd))
                    .collect();
                // Background jobs only count while a quit is draining them. A dev
                // server or watcher is meant to outlive a pause and never exits on
                // its own, so counting it made "Pausing… waiting on N commands to
                // finish" a banner that could never clear. Foreground commands are
                // bounded by their timeout, which is what makes waiting on them sane.
                let quitting = t.paused.as_ref().is_some_and(|p| p.kind == "quit");
                t.busy = fg + if quitting { live.len() } else { 0 };
                changed = t.bg_live != named;
                t.bg_live = named;
                // Published as it stands, so a banner that says everything is
                // paused can name the servers that are still up. This is also the
                // only reason a background job starting or exiting reaches the UI:
                // `emit_key` reads it, so the recount publishes when the list
                // moves even though `busy` (which the same recount keeps current)
                // deliberately does not count these while a pause is on.
                t.bg_now = live;
            })
            .await;
        if counted && changed {
            self.save_task(task_id).await;
        }
    }

    /// Pause, and don't wait for commands to finish: kill them all. Their results tell
    /// the agent they were cut off, so it reruns them after resuming.
    pub async fn force_pause(&self, task_id: &str) {
        let rt = self.runtime(task_id);
        // Nothing in flight is the common case — a chat frozen by the global pause
        // that wasn't running a command, or a swarm of twenty where only two are
        // building. Cancelling an already-uncancelled token is harmless, so that
        // part is always done, but settling is not: the wait below is a sleep, and
        // the tray Quit runs this over every chat while the process is on its way
        // out, so a chat with nothing to kill must not cost 300ms.
        let fg = rt.fg.load(std::sync::atomic::Ordering::SeqCst);
        if fg == 0 && !self.bg.list(task_id).iter().any(|b| b.running) {
            return;
        }
        {
            let mut f = rt.force.lock().unwrap();
            f.cancel();
            *f = CancellationToken::new();
        }
        let killed: Vec<String> = self
            .bg
            .list(task_id)
            .into_iter()
            .filter(|b| b.running)
            .map(|b| format!("`{}` ({})", b.cmd, b.id))
            .collect();
        self.bg.kill_task_with(
            task_id,
            "stopped by force pause before it finished — rerun it",
        );
        if !killed.is_empty() {
            self.note(task_id, "main", format!("<system-reminder>The user force-paused this task. These background commands were killed before they finished and need to be started again: {}</system-reminder>", killed.join(", "))).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        self.emit(task_id, "bg", json!(self.bg.list(task_id)));
        self.refresh_busy(task_id).await;
    }

    /// Force-pause every chat at once: freeze them all, cut every command in flight
    /// and tell each agent what it lost — then leave the app free to exit.
    ///
    /// What makes a force pause feel like a stop is what is *not* here: no run is
    /// ever cancelled (`interrupt` would end it, and the chat would show as
    /// stopped). Each chat is frozen where it stands instead, with the tool calls
    /// that were in flight closed off as errors and a note saying they were cut
    /// short. So the chat keeps reading as paused, and the agent gets that note on
    /// its very next request — whenever the user thaws it. It is a `stop_note`,
    /// which is what a stop leaves behind and what survives the app closing, so a
    /// chat quit this way is still told once it comes back.
    ///
    /// Returns how many chats had work cut off, for the log line the caller writes.
    /// A chat with nothing in flight is not one of them: the global pause below is
    /// what holds it, and the pause banner only lists chats that were working.
    pub async fn force_pause_all(&self) -> usize {
        let ids: Vec<String> = {
            let tasks = self.tasks.read().await;
            // No filter on what is working: `busy` is not persisted and reads 0 for
            // every task until something recounts it, so "was it busy?" cannot be
            // answered from a summary here. A chat with nothing in flight costs one
            // `bg.list` and an early return, whereas guessing wrong means a running
            // build survives a Quit. Archived chats are the one exclusion: they were
            // already put to bed, and freezing one only adds a dead row to the paused
            // list. A task being momentarily locked is left out too, which is better
            // than waiting on a run that is in the middle of a turn.
            tasks
                .values()
                .filter_map(|t| {
                    t.try_lock()
                        .ok()
                        .map(|t| (!t.archived).then(|| t.id.clone()))
                })
                .flatten()
                .collect()
        };
        {
            let mut s = self.settings.write().await;
            s.paused_all = true;
            s.paused_reason = "Everything stopped for the app to close".into();
            store::save_settings(&s);
            (self.bus)(
                "ol://settings",
                serde_json::to_value(crate::public_settings(&s)).unwrap_or_default(),
            );
        }
        for t in self.tasks.read().await.values() {
            t.lock().await.unpaused = false;
        }
        self.pause_bell.notify_waiters();
        let mut cut = 0;
        for id in &ids {
            if self.force_pause_sweep(id).await {
                cut += 1;
            }
        }
        cut
    }

    /// One chat's share of `force_pause_all`, done without holding the settings lock.
    ///
    /// It cannot be `force_pause`: that has to `await` (it sleeps for the killed
    /// processes to record their exits, and takes the task lock via `note` and
    /// `refresh_busy`) while holding the task lock itself (`note` takes it again
    /// through `self.task`), so calling it from inside a lock is a deadlock. It also
    /// would not fit in the global-pause block, which is why the settings write is
    /// released before the sweep starts. The same work is written out flat here, in
    /// the order it has to happen, with nothing to wait on.
    ///
    /// Returns whether this chat had work cut off.
    async fn force_pause_sweep(&self, task_id: &str) -> bool {
        let rt = self.runtime(task_id);
        // Read the in-flight count before the task lock: a command that is just
        // ending is released by `fg` being dropped, and taking the lock first could
        // wait on the very call that drops it.
        let fg = rt.fg.load(std::sync::atomic::Ordering::SeqCst);
        let live_bg = self.bg.kill_task_with_reasons(
            task_id,
            "killed when the app closed before it finished — rerun it",
        );
        if fg == 0 && live_bg.is_empty() {
            return false;
        }
        // What the kills left behind: the jobs are asked to die, but on Windows a
        // process tree takes a moment to actually go, and the next session has to
        // hear about them either way.
        let live: Vec<String> = live_bg
            .iter()
            .map(|b| format!("{}: {}", b.id, b.cmd))
            .collect();
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            // `paused` is only set when empty, so a chat frozen by an earlier pause
            // keeps its own reason. The kind is what marks this one as the app's
            // doing rather than the user's, which keeps it out of `touch` (the chat
            // list's "when did I last act here" clock) and off the exhausted-accounts
            // auto-resume list.
            if t.paused.is_none() {
                t.paused = Some(Pause {
                    reason: "Stopped for the app to close".into(),
                    kind: "quit".into(),
                    since: Utc::now(),
                });
            }
            // Subagents get the same closing as the main one, or a swarm would be
            // told its work was cut while its workers carry on as if nothing had
            // happened. Their ids are collected first: a sub and its transcript live
            // in two maps of the same task, so both can't be borrowed at once.
            let running: Vec<String> = t
                .subs
                .iter()
                .filter(|s| s.status == "running")
                .map(|s| s.id.clone())
                .collect();
            for id in &running {
                if let Some(s) = t.subs.iter_mut().find(|s| &s.id == id) {
                    s.status = "stopped".into();
                }
                if let Some(m) = t.sub_msgs.get_mut(id) {
                    crate::runner::fix_dangling_with(m, crate::runner::CRASHED);
                }
            }
            // A tool call that was mid-flight when the app was told to stop never got
            // its result back. Close the holes with a "run it again" error, the way
            // a stop does, so the next request can't be rejected for a tool_use with
            // no tool_result after it. The ids are read *before* that: repairing the
            // history appends the tool_result turn, so afterwards the last message is
            // a user one and the cut calls are not where you would look for them.
            let cut: Vec<String> = t
                .messages
                .last()
                .filter(|m| m.role == "assistant")
                .map(|m| {
                    m.content
                        .iter()
                        .filter(|b| b["type"] == "tool_use" && b["name"] != "task")
                        .filter_map(|b| b["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            crate::runner::fix_dangling_with(&mut t.messages, crate::runner::CRASHED);
            // Stands in for the `stop_note` a real stop leaves, so the same "these
            // calls were cut, retry them" line rides the next request. Appended, not
            // replaced: a note from an earlier stop is still owed.
            let note = crate::runner::stop_note(&t.messages);
            t.stop_note = Some(match t.stop_note.take() {
                Some(n) => format!("{n}\n{note}"),
                None => note,
            });
            // A cut-off call cannot record its own death, so the tool pills of the
            // turn that was in flight are closed by hand — otherwise the transcript
            // keeps spinning spinners and shows nothing about the cut until load's
            // crash repair runs, days later.
            for id in &cut {
                if let Some(slot) = t
                    .items
                    .iter_mut()
                    .filter(|i| i.kind == "tool" && i.data["status"] == "running")
                    .find(|i| i.data["tool_use_id"] == id.as_str())
                {
                    slot.data["status"] = json!("error");
                    slot.data["meta"] = json!("cut off by quit");
                }
            }
            // A background command that is still running gets here: it is not in
            // `bg_live` yet, and it is not one of the `fg` ones (a command that has
            // only been alive for a moment can finish between the count above and
            // this lock, having released nothing for `fg`). The recount below reads
            // live processes, so it cannot see one that is already on its way out —
            // without this the pause banner loses count of it and the app can close
            // on top of a command that was still meant to be running.
            if !live.is_empty() && fg == 0 {
                t.busy += live.len();
            }
        }
        self.refresh_busy(task_id).await;
        // `bg_live` is the list load reads to tell a later session what died with the
        // app, and the background jobs have no idea the app is quitting — the OS is
        // about to kill them. So it is written *after* the recount, which is the one
        // other thing that writes it: the recount would otherwise see the kills land
        // and clear the record again, and the commands would die unreported, which is
        // exactly what this sweep exists to prevent.
        if !live.is_empty() {
            self.update_task_quiet(task_id, |t| t.bg_live.extend(live.clone()))
                .await;
        }
        // The point of this sweep is what survives the quit, so it cannot go through
        // the debounced writer: that holds a task for up to `SAVE_DEBOUNCE` and the
        // process is on its way out. This is the one save that has to be on disk
        // before the next `app.exit` — the freeze, the cut note and `bg_live` are
        // what the next session reads to work out what it lost.
        self.save_task_now(task_id).await;
        true
    }

    /// Queue a harness note for one agent ("main" or a sub id).
    pub async fn note(&self, task_id: &str, agent: &str, text: String) {
        self.runtime(task_id)
            .inbox
            .lock()
            .await
            .entry(agent.to_string())
            .or_default()
            .push(text);
    }

    /// Queue pasted images for one agent. (media_type, base64) pairs.
    ///
    /// Capped per agent, newest kept. These are base64, so a photo is ~4/3 its
    /// size, and they are held here rather than in a message: a picture pasted
    /// into a *paused* chat, or into one whose agent never gets another request
    /// because the task stopped, is never drained by the read in the turn loop.
    /// The composer's own 8-image limit only bounds one send — pasting again
    /// into a chat that is already busy adds to the same buffer, every time,
    /// until a run finally picks it up. Uncapped, that is unbounded base64 for
    /// an agent that may never read any of it.
    pub async fn note_images(&self, task_id: &str, agent: &str, images: Vec<(String, String)>) {
        if images.is_empty() {
            return;
        }
        let rt = self.runtime(task_id);
        let mut q = rt.inbox_images.lock().await;
        let list = q.entry(agent.to_string()).or_default();
        list.extend(images);
        // The oldest go first: what a user pasting again means is that the
        // pictures they just sent are the ones the next request should carry.
        if list.len() > INBOX_IMAGES {
            let drop_n = list.len() - INBOX_IMAGES;
            list.drain(..drop_n);
        }
    }

    /// Push a task summary to the UI.
    ///
    /// Subagent reports are left out on purpose: they run to tens of kilobytes
    /// each, and this fires on every status change of every agent — so a swarm
    /// of 20 meant serializing 400 kB of text to show a spinner. The reports are
    /// only ever read when a subagent's panel is opened, which fetches the task
    /// whole via `task_get`.
    /// Re-send a docker task's summary after its container changed state, so
    /// the chat header shows "building", "running" or "stopped" live.
    pub async fn emit_container(&self, task_id: &str) {
        if let Ok(t) = self.task(task_id).await {
            let t = t.lock().await;
            self.emit_summary(&t);
        }
    }

    pub fn emit_summary(&self, t: &Task) {
        let s = t.summary_without_reports();
        self.emit(
            &t.id,
            "task",
            serde_json::to_value(s).unwrap_or(Value::Null),
        );
    }

    /// Publish an attention event and remember it in the chat.
    ///
    /// Whether an event is also *news* is one rule, kept here so the mark and the
    /// event can never disagree about it — see `attention_is_news`. The mark is
    /// what a client draws as the sidebar's blue dot, and it lives on the task
    /// rather than in the window that drew it because there is more than one
    /// client (see `Task::unread`).
    pub async fn attention(&self, task_id: &str, payload: Value) {
        if attention_is_news(&payload) {
            self.set_unread(task_id, true).await;
        }
        (self.bus)("ol://attention", payload);
    }

    /// Change the "nothing here has been looked at yet" mark and publish it, so
    /// every client's dot follows. Deliberately not `update_task`: reading a chat
    /// is not work *in* the chat, and `updated_at` is what a settled chat sorts
    /// on (see `Task::touched_at`).
    async fn set_unread(&self, task_id: &str, unread: bool) {
        let Ok(task) = self.task(task_id).await else {
            return;
        };
        let mut t = task.lock().await;
        if t.unread == unread {
            return;
        }
        t.unread = unread;
        self.emit_summary(&t);
    }

    /// Someone is opening this chat, so the mark that says nobody has looked at
    /// it goes out — and it goes out for *everyone*, because the mark is the
    /// backend's and a second client on the same chat has to agree about it.
    /// A blocking question or approval is the exception; see `Task::mark_seen`.
    pub async fn seen(&self, task_id: &str) {
        let Ok(task) = self.task(task_id).await else {
            return;
        };
        let mut t = task.lock().await;
        if t.mark_seen() {
            self.emit_summary(&t);
        }
    }

    /// Pending informational notices are already in task headers: do not hydrate
    /// every subagent transcript just to restore the app's notification cards.
    pub async fn user_notices(&self) -> HashMap<String, Vec<Item>> {
        let tasks: Vec<TaskRef> = self.tasks.read().await.values().cloned().collect();
        let mut notices = HashMap::new();
        for task in tasks {
            let task = task.lock().await;
            if task.hidden || task.archived {
                continue;
            }
            let items: Vec<Item> = task
                .items
                .iter()
                .filter(|item| {
                    item.kind == "user_notice" && !item.data["dismissed"].as_bool().unwrap_or(false)
                })
                .cloned()
                .collect();
            if !items.is_empty() {
                notices.insert(task.id.clone(), items);
            }
        }
        notices
    }

    /// Publish a notice only after persistence, holding the task lock through
    /// mutation and rollback so neither readers nor the saver see a failed card.
    pub async fn deliver_user_notice(&self, task_id: &str, item: Item) -> Result<(), String> {
        let rt = self.runtime(task_id);
        let _save = rt.task_save.lock().await;
        let task = self.task(task_id).await?;
        let mut task = task.lock().await;
        task.items.push(item.clone());
        if let Err(error) = store::try_save_task(&task) {
            task.items.pop();
            return Err(format!("Could not persist informational notice: {error}"));
        }
        self.emit(task_id, "item", json!(item));
        Ok(())
    }

    /// Dismiss only this task's informational card. In particular this must not
    /// use `respond` or the agent inbox: dismissal is not an answer or permission.
    pub async fn dismiss_user_notice(&self, task_id: &str, item_id: &str) -> Result<(), String> {
        let rt = self.runtime(task_id);
        let _save = rt.task_save.lock().await;
        let task = self.task(task_id).await?;
        let mut task = task.lock().await;
        let index = task
            .items
            .iter()
            .position(|item| item.id == item_id && item.kind == "user_notice")
            .ok_or("Informational notice not found in this task.")?;
        let original = task.items[index].data.clone();
        task.items[index].data["dismissed"] = json!(true);
        if let Err(error) = store::try_save_task(&task) {
            task.items[index].data = original;
            return Err(format!("Could not persist notice dismissal: {error}"));
        }
        self.emit(task_id, "item", json!(task.items[index]));
        Ok(())
    }

    /// Append (or replace, by id) a timeline item and push it to the UI.
    pub async fn upsert_item(&self, task_id: &str, item: Item) {
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            if let Some(slot) = t.items.iter_mut().find(|i| i.id == item.id) {
                *slot = item.clone();
            } else {
                t.items.push(item.clone());
            }
        }
        self.emit(
            task_id,
            "item",
            serde_json::to_value(&item).unwrap_or(Value::Null),
        );
    }

    /// One timeline item from a transient state that has finished (e.g. a
    /// startup/loading notice). Removes and publishes only when it still exists.
    pub async fn remove_item(&self, task_id: &str, item_id: &str) {
        let mut removed = false;
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            let before = t.items.len();
            t.items.retain(|item| item.id != item_id);
            removed = t.items.len() != before;
        }
        if removed {
            self.emit(task_id, "itemgone", json!({"id": item_id}));
            self.save_task(task_id).await;
        }
    }

    /// Timeline item in a subagent's own transcript.
    pub async fn upsert_sub_item(&self, task_id: &str, sub: &str, item: Item) {
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            let list = t.sub_items.entry(sub.to_string()).or_default();
            if let Some(slot) = list.iter_mut().find(|i| i.id == item.id) {
                *slot = item.clone();
            } else {
                list.push(item.clone());
            }
        }
        self.emit(task_id, "sitem", json!({"sub_id": sub, "item": item}));
    }

    pub async fn upsert_in(&self, task_id: &str, sub: Option<&str>, item: Item) {
        match sub {
            Some(s) => self.upsert_sub_item(task_id, s, item).await,
            None => self.upsert_item(task_id, item).await,
        }
    }

    pub async fn patch_in(
        &self,
        task_id: &str,
        sub: Option<&str>,
        item_id: &str,
        f: impl FnOnce(&mut Item),
    ) {
        let Some(sub) = sub else {
            return self.patch_item(task_id, item_id, f).await;
        };
        let mut out = None;
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            if let Some(slot) = t
                .sub_items
                .get_mut(sub)
                .and_then(|l| l.iter_mut().find(|i| i.id == item_id))
            {
                f(slot);
                out = Some(slot.clone());
            }
        }
        if let Some(item) = out {
            self.emit(task_id, "sitem", json!({"sub_id": sub, "item": item}));
        }
    }

    pub async fn patch_item(&self, task_id: &str, item_id: &str, f: impl FnOnce(&mut Item)) {
        let mut out = None;
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            if let Some(slot) = t.items.iter_mut().find(|i| i.id == item_id) {
                f(slot);
                out = Some(slot.clone());
            }
        }
        if let Some(item) = out {
            self.emit(
                task_id,
                "item",
                serde_json::to_value(&item).unwrap_or(Value::Null),
            );
        }
    }

    /// The user did something in this chat: mark it as the most recently used
    /// one, so the chat list puts it at the top. Deliberately separate from
    /// `updated_at`, which every agent step moves.
    ///
    /// This also *releases* a manual position. The sidebar's sort key is
    /// `order || touched_at` (`sortKey`, `ui/Chrome.tsx`): a dragged chat carries
    /// an `order` and wins outright, so `touched_at` alone was invisible for it
    /// and every caller of this said "put it at the top" while the row stayed put.
    /// `order` had no way back to 0 either, so one drag made the chat permanently
    /// un-sortable. Touching is the user saying "I want this one back in the
    /// flow", which is the only event that should undo their own layout.
    ///
    /// Saved, because `order` is persisted and a release that isn't written down
    /// comes back on the next launch — the chat would stay stuck until the user
    /// happened to act in it again. The dirty-set save, not an inline write: this
    /// is not on the agent's hot path, and the flag is a few bytes.
    pub async fn touch(&self, task_id: &str) {
        if let Ok(t) = self.task(task_id).await {
            // Only when nothing is running. A `touch` mid-run is the user
            // steering — a message into a turn in flight — and the files the
            // agent is partway through changing are the *agent's*, not the
            // user's. Recording them here would take them out of the overwrite
            // ledger and make a later rewind refuse files it should be putting
            // back; skipped, they are recorded by the agent snapshot the turn
            // itself asks for.
            //
            // This is the snapshot that keeps the two kinds apart. It runs
            // between turns, so whatever it commits since the last one is the
            // user's own work — see `checkpoint`'s module note. Done before the
            // task lock is taken for the bookkeeping below, because it is a disk
            // operation and that lock gates every other reader of this chat.
            //
            // The marker (`Snapshot::of`: counts and indices) is fixed here, as
            // the message is sent; the git work is queued, not awaited, because
            // the send path would otherwise hold the first model request behind
            // a full `add -A` of the checkout. Every tool, the end-of-run
            // capture, rewind and fork wait for it instead
            // (`Runtime::checkpoints_settled`).
            let rt = self.runtime(task_id);
            if !rt.running.load(Ordering::SeqCst) {
                let enabled = self.settings.read().await.checkpoints;
                let snap = checkpoint::Snapshot::of(&*t.lock().await, enabled);
                if let Some(snap) = snap {
                    rt.queue_checkpoint(move || {
                        snap.user().take();
                    });
                }
            }
            let mut t = t.lock().await;
            t.touched_at = Utc::now();
            t.order = 0.0;
            self.emit_summary(&t);
            drop(t);
            self.save_task(task_id).await;
        }
    }

    pub async fn update_task(&self, task_id: &str, f: impl FnOnce(&mut Task)) {
        if let Ok(t) = self.task(task_id).await {
            let mut t = t.lock().await;
            let was_paused = t.paused.is_some();
            let was_parked = t.waiting_kind.as_deref() == Some("wake");
            f(&mut t);
            Self::close_timeline_wait_transitions(&mut t, was_paused, was_parked);
            t.updated_at = Utc::now();
            self.emit_summary(&t);
        }
    }

    /// Close only the wait whose state was actually lifted. Used by both task
    /// mutation paths so send, wake and resume share the same timing boundary.
    fn close_timeline_wait_transitions(t: &mut Task, was_paused: bool, was_parked: bool) {
        let now = Utc::now();
        if was_paused && t.paused.is_none() {
            spans::close_waits(&mut t.spans, "pause", now);
        }
        if was_parked && t.waiting_kind.as_deref() != Some("wake") {
            spans::close_waits(&mut t.spans, "park", now);
        }
    }

    /// Like `update_task`, but the summary is only pushed when something the UI
    /// can see actually changed.
    ///
    /// `update_task` emits a whole cloned `TaskSummary` on every call, and the
    /// `subs` inside it carry each agent's full report — so a quiet chat (one
    /// waiting on a subagent, say) republished hundreds of kilobytes of JSON
    /// once a second for no visible change, and the frontend re-rendered the
    /// transcript and sidebar from it every time. Callers that only refresh
    /// transient state — step text, subagent meta, busy counts — use this.
    pub async fn update_task_quiet(&self, task_id: &str, f: impl FnOnce(&mut Task)) -> bool {
        let Ok(t) = self.task(task_id).await else {
            return false;
        };
        let mut t = t.lock().await;
        let before = self.emit_key(&t);
        let was_paused = t.paused.is_some();
        let was_parked = t.waiting_kind.as_deref() == Some("wake");
        f(&mut t);
        Self::close_timeline_wait_transitions(&mut t, was_paused, was_parked);
        if self.emit_key(&t) == before {
            return false;
        }
        t.updated_at = Utc::now();
        self.emit_summary(&t);
        true
    }

    /// What the UI renders from a summary, as one comparable string.
    fn emit_key(&self, t: &Task) -> String {
        serde_json::json!([
            t.status,
            t.waiting_kind,
            t.step,
            t.paused,
            t.busy,
            // The live background commands, by identity. Starting or exiting is
            // news; a new last output line is not, and including it would make
            // this key move continuously for a list nobody re-reads it from.
            t.bg_now
                .iter()
                .map(|b| json!([&b.id, &b.cmd]))
                .collect::<Vec<_>>(),
            t.pinned,
            t.archived,
            t.hidden,
            t.unpaused,
            t.serving,
            t.todos,
            // The transcript's extent, not its content. A turn that streamed a
            // word but finished in the same status leaves every field above
            // unchanged, so without this a chat that was doing real work
            // published nothing and the sidebar and Home kept showing it as
            // finished — the cost of the key is that it moves on nearly every
            // turn, and a summary per turn is exactly what this method exists
            // to make cheap. `updated_at` alone would have done that too, but it
            // also moves for bookkeeping, which is why this list is explicit.
            (
                t.items.len(),
                t.items.iter().map(|i| i.text.len()).sum::<usize>()
            ),
            // Status and progress only: an agent's report can be 20k characters
            // and it is read once, when its panel is opened.
            t.subs
                .iter()
                .map(|s| json!([
                    &s.id,
                    &s.status,
                    &s.meta,
                    &s.model,
                    s.started,
                    s.report.len()
                ]))
                .collect::<Vec<_>>(),
        ])
        .to_string()
    }

    /// Persist a task, without blocking the agent on it.
    ///
    /// The runner saves after every round of tool calls, and each save
    /// serializes the *whole* transcript — megabytes by the end of a long task.
    /// Done inline that holds the task's lock for the length of a full rewrite,
    /// so every subagent `step()`, item patch and summary queued behind it
    /// waits for several megabytes to hit the disk, and a burst of tool calls
    /// repeated the same work. Instead a save marks the task dirty and a shared
    /// writer flushes at most one task per `SAVE_DEBOUNCE`, off the lock, with
    /// everything that piled up in between coalesced into it. Use
    /// `save_task_now` where the write has to have landed before we go on.
    pub async fn save_task(&self, task_id: &str) {
        self.dirty.lock().unwrap().insert(task_id.to_string());
        if let Some(h) = self.me.get().cloned() {
            h.wake_saver();
        }
    }

    /// Persist a task and wait for it to be on disk — for the points where the
    /// run is over, or a command is about to read the file back, so a crash
    /// can't lose the tail of a long task.
    pub async fn save_task_now(&self, task_id: &str) {
        let rt = self.runtime(task_id);
        let _save = rt.task_save.lock().await;
        if let Ok(t) = self.task(task_id).await {
            Self::write_task_json(t).await;
        }
        self.dirty.lock().unwrap().remove(task_id);
    }

    /// Both save paths hold `task_save` until this finishes, so an older
    /// snapshot cannot land after a newer one. Only pruning and serialization
    /// need the task lock; the atomic write (including fsync) must not hold it
    /// or run on an async worker.
    async fn write_task_json(t: TaskRef) {
        tokio::task::spawn_blocking(move || {
            let (id, json) = {
                let mut g = t.blocking_lock();
                // Prune before serializing so reopened chats do not bring old
                // screenshot payloads back into memory.
                runner::prune_task_images(&mut g);
                (g.id.clone(), serde_json::to_string(&*g))
            };
            if let Ok(json) = json {
                store::save_json(&id, &json);
            }
        })
        .await
        .ok();
    }

    /// Persist settings, without making the caller wait for the disk.
    ///
    /// Spend accounting used to do this inline on every model turn, and it is
    /// the most expensive write in the app: `Settings` holds every account
    /// (access *and* refresh tokens), every provider config, the model list,
    /// routes, agents and hooks, so each call cloned the lot and then rewrote
    /// the whole file with two fsyncs. At one write per turn that is two fsyncs
    /// of the entire settings file on the async runtime, for a figure that moves
    /// by cents — and under ultrathread every agent's turn queued behind the
    /// shared settings lock to do it.
    ///
    /// So it goes through the writer that already coalesces task saves: mark
    /// dirty, let the shared pass pick it up, and everything that piled up in
    /// between becomes one write. What the user sees is still exact — the usage
    /// event is built from memory, not from the file.
    ///
    /// A harness with no writer (the tests build one as a plain struct, so `me`
    /// is empty) writes inline, as it always did, rather than dropping the save.
    pub async fn save_settings_debounced(&self) {
        if self.me.get().is_none() {
            let s = self.settings.read().await.clone();
            store::save_settings(&s);
            return;
        }
        self.settings_dirty.store(true, Ordering::Relaxed);
        self.wake_saver();
    }

    /// Write settings now and wait for it, clearing the pending dirty flag so
    /// the writer doesn't repeat the work. For quitting, where the last few
    /// cents have to be on disk.
    pub async fn save_settings_now(&self) {
        let s = self.settings.read().await.clone();
        tokio::task::spawn_blocking(move || store::save_settings(&s))
            .await
            .ok();
        self.settings_dirty.store(false, Ordering::Relaxed);
    }

    /// Whether the writer still owes the settings file a write.
    fn take_settings_dirty(&self) -> bool {
        self.settings_dirty.swap(false, Ordering::Relaxed)
    }

    /// Start the writer task, if it isn't already running, and tell it there's
    /// work. A harness built as a plain struct (the tests) has no `Arc` to give
    /// the writer, so `me` is empty and there is nothing to start.
    fn wake_saver(&self) {
        {
            let slot = self.saver.lock().unwrap();
            if slot.is_some() {
                drop(slot);
                self.runtime("").save_wake.notify_one();
                return;
            }
        }
        let h = self
            .me
            .get()
            .cloned()
            .expect("harness not registered with Harness::register");
        let rt = self.runtime("");
        let handle = tokio::spawn(async move {
            loop {
                rt.save_wake.notified().await;
                // Coalesce first: a burst of tool calls asks for a save after
                // every round, and those are close enough together to be one
                // write of the whole transcript.
                tokio::time::sleep(SAVE_DEBOUNCE).await;
                loop {
                    // Settings go first and on their own: they are one file, and
                    // a long run rewrites the whole thing (every account token
                    // in it) for spend that moves by cents. If a pass finds
                    // nothing else to do, `ids` is empty and the loop ends —
                    // so the dirty flag has to be cleared or checked before it,
                    // or this would spin forever re-writing settings.
                    if h.take_settings_dirty() {
                        h.save_settings_now().await;
                    }
                    let ids: Vec<String> = h.dirty.lock().unwrap().iter().cloned().collect();
                    if ids.is_empty() {
                        break;
                    }
                    // Write the dirty tasks concurrently rather than one at a
                    // time. The work is per-file and each task has its own
                    // `task_save` mutex, so the ordering below is unchanged: the
                    // only thing that was serial was *waiting* on the previous
                    // task's fsyncs while holding none of its locks. With chats
                    // that run to hundreds of megabytes that wait was seconds
                    // each, and ten dirty chats meant every save in the app
                    // landed tens of seconds late — including `save_task_now`
                    // for a chat that was never dirty, which queued behind the
                    // same stuck writer.
                    //
                    // Bounded, not unbounded: the serialize half of a write runs
                    // on `store::pool()`, which is capped at 8 threads, so more
                    // concurrent writers than that only oversubscribe it (and the
                    // disk) while making the flush order less predictable.
                    let bound = store::pool().current_num_threads().max(1);
                    let jobs: Vec<_> = ids
                        .into_iter()
                        .take(bound.saturating_mul(2))
                        .map(|id| {
                            let h = h.clone();
                            async move {
                                let rt = h.runtime(&id);
                                let _save = rt.task_save.lock().await;
                                // Take the id out before writing, so a save requested
                                // while this is in flight re-marks it dirty for the
                                // next pass instead of being swallowed by a stale write.
                                h.dirty.lock().unwrap().remove(&id);
                                // `task()` hydrates, which is what makes writing safe: a
                                // header-only task would overwrite its own subagent
                                // history. This is the one save path that did not go
                                // through the accessor by accident, so it is worth saying.
                                let Ok(t) = h.task(&id).await else { return };
                                Self::write_task_json(t).await;
                            }
                        })
                        .collect();
                    // Any id beyond the bound stays dirty and the outer loop picks
                    // it up on the next pass, so a backlog drains in bounded
                    // batches instead of being dropped.
                    let _ = futures_util::future::join_all(jobs).await;
                }
            }
        });
        let mut slot = self.saver.lock().unwrap();
        if slot.is_some() {
            handle.abort();
        } else {
            *slot = Some(handle);
        }
    }

    /// Hand the writer task something to call back into. Called once, where the
    /// app builds its `Arc<Harness>`.
    pub fn register(me: &Arc<Harness>) {
        let _ = me.me.set(me.clone());
    }

    /// Block until the UI answers an approval/question item (or the task is cancelled).
    pub async fn wait_for_user(
        &self,
        task_id: &str,
        item_id: &str,
        kind: &str,
        cancel: &CancellationToken,
    ) -> Option<Value> {
        let rt = self.runtime(task_id);
        let (tx, rx) = oneshot::channel();
        let mut span = spans::Span::new("wait", None, kind);
        span.item = item_id.to_string();
        rt.pending.lock().await.insert(item_id.to_string(), tx);
        self.update_task(task_id, |t| {
            t.status = "waiting".into();
            t.waiting_kind = Some(kind.into());
        })
        .await;
        self.attention(
            task_id,
            json!({"task_id": task_id, "item_id": item_id, "kind": kind}),
        )
        .await;
        let res = tokio::select! {
            r = rx => r.ok(),
            _ = cancel.cancelled() => None,
        };
        rt.pending.lock().await.remove(item_id);
        let answered = res.is_some();
        self.update_task(task_id, |t| {
            // On the lane of whichever agent asked: the item says which.
            span.agent = spans::agent_of_item(t, item_id);
            span.end = Some(Utc::now());
            span.status = if answered { "answered" } else { "cancelled" }.into();
            spans::push(&mut t.spans, span);
            if t.status == "waiting" {
                t.status = "running".into();
            }
            t.waiting_kind = None;
            // The request has been dealt with (answered, or the run was
            // cancelled), so the mark the announcement above set goes with it: a
            // blocking wait is the one thing `mark_seen` deliberately leaves
            // alone, and nothing else would ever clear this one.
            t.unread = false;
        })
        .await;
        res
    }

    /// Answer a pending approval/question through the single channel shared by
    /// desktop and remote notification actions.
    pub async fn respond_pending(
        &self,
        task_id: &str,
        item_id: &str,
        response: Value,
    ) -> Result<(), String> {
        let tx = self.runtime(task_id).pending.lock().await.remove(item_id);
        match tx {
            Some(tx) => {
                self.touch(task_id).await;
                let _ = tx.send(response);
                Ok(())
            }
            None => Err("That request is no longer pending.".into()),
        }
    }
}
