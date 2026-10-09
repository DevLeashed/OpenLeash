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
//! - `store.rs`       settings + task persistence

pub mod accounts;
pub mod browser;
pub mod checkpoint;
pub mod checks;
pub mod commitmsg;
pub mod git;
pub mod mcp;
pub mod mcp_oauth;
pub mod memory;
pub mod pcguard;
pub mod permissions;
pub mod plugins;
pub mod prompt;
pub mod providers;
#[cfg(test)]
mod reviewer_accounting;
#[cfg(test)]
mod reviewer_checkpoint;
#[cfg(test)]
mod reviewer_commit;
#[cfg(test)]
mod reviewer_edits;
#[cfg(test)]
mod reviewer_hooks;
#[cfg(test)]
mod reviewer_mcp;
#[cfg(test)]
mod reviewer_memory_scope;
#[cfg(test)]
mod reviewer_modes;
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
mod reviewer_runtime;
#[cfg(test)]
mod reviewer_runtime_wake;
#[cfg(test)]
mod reviewer_sandbox;
#[cfg(test)]
mod reviewer_staleness;
#[cfg(test)]
mod reviewer_startup;
#[cfg(test)]
mod reviewer_streams;
#[cfg(test)]
mod reviewer_task_creation;
#[cfg(test)]
mod reviewer_toolsearch;
#[cfg(test)]
mod reviewer_trust;
#[cfg(test)]
mod reviewer_webfetch;
pub mod router;
pub mod runner;
pub mod shell;
pub mod stats;
pub mod store;
#[cfg(test)]
mod tests;
pub mod toolindex;
pub mod tools;
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
/// Most sub-agents one agent may launch at the same time, per layer.
pub const X_MAX_FANOUT: u8 = 8;
/// Sub-agents running at once across a whole X tree (a plain ultrathread runs 16).
pub const X_DEFAULT_RUNNING: usize = 32;
/// Sub-agents spawned for one X task, ever. The real bound on spend: a running
/// cap alone lets a long task churn through thousands of agent-lives.
pub const X_DEFAULT_TOTAL: usize = 400;
/// Ceiling on how many sub-agents may run at once, whatever the ladder says.
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
    /// user | text | thinking | tool | approval | question | sub | notice | artifact
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
    /// to Sonnet mid-run left every sub-agent on Opus for the rest of the task,
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
    /// Sub-agent that launched this one (ultrathread nesting); empty = the main agent.
    #[serde(default)]
    pub parent: String,
    /// 1 = launched by main, 2 = launched by a sub-agent, …
    #[serde(default)]
    pub depth: u8,
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
    pub model: String,
    pub effort: usize,
    #[serde(
        default = "crate::agent::permissions::default_perm",
        deserialize_with = "crate::agent::permissions::deserialize_perm"
    )]
    pub perm: String,
    pub plan: bool,
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
    /// Sub-agent ids this task may spawn.
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
    /// Each sub-agent's own conversation, so a stopped one can be continued.
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
    /// Background commands alive at the last save ("bg_id: cmd"). Non-empty on load = the app died under them.
    #[serde(default)]
    pub bg_live: Vec<String>,
    /// Commands (foreground + background) still running. While paused and > 0, the pause is "in progress".
    #[serde(skip)]
    pub busy: usize,
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
    /// The chat this one was forked from, and how. "full" copied the whole
    /// conversation; "compact" starts from a summary of it instead.
    #[serde(default)]
    pub forked_from: Option<String>,
    #[serde(default)]
    pub pinned: bool,
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
    #[serde(default)]
    pub checkpoints: Vec<Checkpoint>,
    #[serde(default)]
    pub touched: HashMap<String, Option<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the user last *did* something here: sent a message, answered a
    /// question, paused, stopped, resumed, or messaged a sub-agent.
    /// `updated_at` moves on every agent step, so it cannot order a list — a
    /// background chat working through a hundred tool calls would shove a chat
    /// the user just opened to the bottom. This is what a chat list sorts on.
    #[serde(default = "epoch")]
    pub touched_at: DateTime<Utc>,
    /// False while `sub_items`/`sub_msgs` are still on disk rather than in memory.
    ///
    /// These two are ~97% of a task file's bytes (sub-agent timelines and their
    /// model conversations) and nothing at boot needs them: the sidebar reads
    /// `summary()`, which skips both. So `load_tasks` parses a header and leaves
    /// the body behind, and `Harness::hydrate` reads the file again the first
    /// time a chat is actually opened. `load_tasks` sets this; it is never
    /// written to disk, and never persisted — so a task created in this session
    /// is hydrated by construction.
    ///
    /// It is load-bearing for correctness, not just speed. The saver serializes
    /// whatever is in memory and rewrites the whole file, so saving a task that
    /// was never hydrated would replace real sub-agent history with two empty
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
    pub archived: bool,
    pub hidden: bool,
    pub forked_from: Option<String>,
    pub pinned: bool,
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
}

impl Task {
    /// Claim a sub-agent for a resume: check it's free (and ours, when we're a
    /// sub-agent), mark it running and hand back its conversation with the new
    /// turn appended — all under one lock, so nothing can start the same
    /// sub-agent twice and end up driving one transcript from two places.
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
        TaskSummary {
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
            archived: self.archived,
            hidden: self.hidden,
            forked_from: self.forked_from.clone(),
            pinned: self.pinned,
            order: self.order,
            todos: self.todos.clone(),
            subs: self.subs.clone(),
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
/// swapped mid-run has to reach the sub-agents it has *already* launched, not
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
/// `*` badge promises it does not. A sub-agent has no such turn boundary to
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
        // A sub-agent following the chat, including a swap already queued for the
        // next message — the user is moving the whole chat, not just its lead.
        Some(_) => chat_model_of(t),
        None => None,
    };
    chat.or_else(|| (!t.model.is_empty()).then(|| t.model.clone()))
        .unwrap_or_else(|| fallback.to_string())
}

/// A chat's model for anything that is not the main agent's own turn: a swap
/// queued for its next message counts, so a sub-agent following the chat moves
/// with it instead of staying on a model the user has already replaced.
pub fn chat_model_of(t: &Task) -> Option<String> {
    t.pending
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|m| !m.is_empty())
        .map(String::from)
}

/// The reasoning effort one agent runs at right now (0 = Max … 4 = Low), for
/// anything that is not a request being built: the swap dialog, and a sub-agent
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

/// A broadcast the user sent, and which agents have acted on it since. The user
/// gets a running list of who took it on, so a "undo the lighting" lands where
/// it should and the agents it never concerned stay quiet.
#[derive(Clone, Default, Serialize)]
pub struct Broadcast {
    pub text: String,
    /// Who it was sent to ("main" plus every working sub-agent's id).
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
#[derive(Default)]
pub(crate) struct WakeState {
    pub(crate) parked: bool,
    pub(crate) generation: u64,
    pub(crate) deadline_ms: u64,
    pub(crate) cancel: Option<CancellationToken>,
}

/// Live, non-persisted state for a task.
#[derive(Default)]
pub struct Runtime {
    pub cancel: std::sync::Mutex<Option<CancellationToken>>,
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
    pub running: std::sync::atomic::AtomicBool,
    /// Parent token for background sub-agents (outlives a single main run; Esc replaces it).
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
    /// half-loaded task silently overwriting its own sub-agent history — see
    /// `Task::hydrated`.
    pub async fn task(&self, id: &str) -> Result<TaskRef, String> {
        let r = self
            .tasks
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no task {id}"))?;
        self.ensure_hydrated(&r).await?;
        Ok(r)
    }

    /// Splice a task's sub-agent maps back in, once, if they are still on disk.
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
                // the (already missing) sub-agent panels are empty.
                eprintln!("[openleash] could not load sub-agent history for {id}: {e}");
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
        loop {
            let bell = self.pause_bell.notified();
            if !self.is_paused(task_id).await {
                return true;
            }
            tokio::select! {
                _ = bell => {},
                _ = cancel.cancelled() => return false,
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {},
            }
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
        self.update_task(task_id, |t| {
            if t.paused.is_none() {
                t.paused = Some(Pause {
                    reason,
                    kind,
                    since: Utc::now(),
                });
            }
        })
        .await;
        self.save_task(task_id).await;
        // Pausing by hand is the user acting on this chat, so it comes to the
        // top. A pause the app raised on its own (a route ran out of models) is
        // not: that is the agent's news, and it must not reorder anything.
        if kind_s == "manual" {
            self.touch(task_id).await;
        }
        // Recount here, not only when a command starts or ends: pausing while a
        // command is already in flight is exactly the case the "Pausing... waiting
        // on N commands" banner is about, and `busy` isn't persisted.
        self.refresh_busy(task_id).await;
        self.pause_bell.notify_waiters();
        (self.bus)(
            "ol://attention",
            json!({"task_id": task_id, "kind": "paused", "pause": kind_s, "reason": reason_s}),
        );
    }

    /// Recount in-flight commands so the UI can show "Pausing…" until they drain.
    pub async fn refresh_busy(&self, task_id: &str) {
        let fg = self
            .runtime(task_id)
            .fg
            .load(std::sync::atomic::Ordering::SeqCst);
        let live: Vec<String> = self
            .bg
            .list(task_id)
            .into_iter()
            .filter(|b| b.running)
            .map(|b| format!("{}: {}", b.id, b.cmd))
            .collect();
        let mut changed = false;
        let counted = self
            .update_task_quiet(task_id, |t| {
                // Background jobs only count while a quit is draining them. A dev
                // server or watcher is meant to outlive a pause and never exits on
                // its own, so counting it made "Pausing… waiting on N commands to
                // finish" a banner that could never clear. Foreground commands are
                // bounded by their timeout, which is what makes waiting on them sane.
                let quitting = t.paused.as_ref().is_some_and(|p| p.kind == "quit");
                t.busy = fg + if quitting { live.len() } else { 0 };
                changed = t.bg_live != live;
                t.bg_live = live;
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
            // Sub-agents get the same closing as the main one, or a swarm would be
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
    pub fn emit_summary(&self, t: &Task) {
        let mut s = t.summary();
        for sub in s.subs.iter_mut() {
            sub.report.clear();
        }
        self.emit(
            &t.id,
            "task",
            serde_json::to_value(s).unwrap_or(Value::Null),
        );
    }

    /// Pending informational notices are already in task headers: do not hydrate
    /// every sub-agent transcript just to restore the app's notification cards.
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

    /// Timeline item in a sub-agent's own transcript.
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
            if !self.runtime(task_id).running.load(Ordering::SeqCst) {
                let enabled = self.settings.read().await.checkpoints;
                let snap = checkpoint::Snapshot::of(&*t.lock().await, enabled);
                if let Some(snap) = snap {
                    let _ = tokio::task::spawn_blocking(move || snap.user().take()).await;
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
            f(&mut t);
            t.updated_at = Utc::now();
            self.emit_summary(&t);
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
        f(&mut t);
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
    /// so every sub-agent `step()`, item patch and summary queued behind it
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
            let mut t = t.lock().await;
            // Trimming images here rather than only in the debounced writer: a
            // caller of this path is about to read the file back or the run is
            // over, so this is the last chance to keep a multi-megabyte payload
            // of screenshots out of what lands on disk.
            runner::prune_task_images(&mut t);
            store::save_task(&t);
        }
        self.dirty.lock().unwrap().remove(task_id);
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
                    for id in ids {
                        let rt = h.runtime(&id);
                        let _save = rt.task_save.lock().await;
                        // Take the id out before writing, so a save requested
                        // while this is in flight re-marks it dirty for the next
                        // pass instead of being swallowed by a stale write.
                        h.dirty.lock().unwrap().remove(&id);
                        // `task()` hydrates, which is what makes writing safe: a
                        // header-only task would overwrite its own sub-agent
                        // history. This is the one save path that did not go
                        // through the accessor by accident, so it is worth saying.
                        let Ok(t) = h.task(&id).await else { continue };
                        // Serialize and write on a blocking thread, so neither
                        // the CPU nor the disk is on the async runtime, and the
                        // lock is only held to produce the JSON.
                        let json = tokio::task::spawn_blocking(move || {
                            let mut g = t.blocking_lock();
                            // The sweep is part of the write, not a separate pass:
                            // it only has to keep up with the timeline, and doing
                            // it here means the bytes written are already the
                            // trimmed ones, so a chat reopened tomorrow cannot
                            // parse a gigabyte of screenshots back in.
                            runner::prune_task_images(&mut g);
                            serde_json::to_string(&*g)
                        })
                        .await;
                        if let Ok(Ok(j)) = json {
                            store::save_json(&id, &j);
                        }
                    }
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
        rt.pending.lock().await.insert(item_id.to_string(), tx);
        self.update_task(task_id, |t| {
            t.status = "waiting".into();
            t.waiting_kind = Some(kind.into());
        })
        .await;
        (self.bus)("ol://attention", json!({"task_id": task_id, "kind": kind}));
        let res = tokio::select! {
            r = rx => r.ok(),
            _ = cancel.cancelled() => None,
        };
        rt.pending.lock().await.remove(item_id);
        self.update_task(task_id, |t| {
            if t.status == "waiting" {
                t.status = "running".into();
            }
            t.waiting_kind = None;
        })
        .await;
        res
    }
}
