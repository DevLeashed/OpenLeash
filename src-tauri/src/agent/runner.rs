//! The agent loop.
//!
//!   user turn → [compact?] → route a model request → tool calls → permission gate →
//!   execute (read-only calls and sub-agents in parallel) → tool results (+ notes) → loop
//!
//! Prompt-cache discipline: the system prompt and tool list are frozen per task
//! (until compaction) and history is append-only. Anything that changes
//! mid-task — assist mode, the sub-agent list, plan mode, steering — is
//! *announced* as a <system-reminder> in the next user turn, never edited
//! into earlier messages.
//!
//! Sub-agents run the same loop with a fresh history, their own model and a
//! narrower tool set. Their transcripts stream into `task.sub_items`, their
//! approvals surface in the main timeline, and their questions go to the main
//! agent first, then (depending on assist mode) to the user.

fn remember_computer_objection(seen: &std::sync::atomic::AtomicBool, objection: bool) {
    if objection {
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod computer_objection_tests {
    #[test]
    fn consumed_objection_survives_watcher_shutdown() {
        let seen = std::sync::atomic::AtomicBool::new(false);
        super::remember_computer_objection(&seen, true);
        super::remember_computer_objection(&seen, false);
        assert!(seen.load(std::sync::atomic::Ordering::SeqCst));
    }
}

use super::permissions::{self, Decision};
use super::providers::{self, ChatRequest, StreamEvent};
use super::router::{self, RouteErr, Who};
use super::store::{self, AgentDef, AllowRule, UltraX};
use super::{
    checkpoint, checks, git, prompt, resolve_model, toolindex, tools, Broadcast, Checkpoint,
    Harness, Item, Message, QueuedMsg, Runtime, SubInfo, Task, Todo,
};
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A url, trimmed to something that fits on one line of a transcript.
fn short(s: impl AsRef<str>) -> String {
    s.as_ref().chars().take(80).collect()
}

/// Turn a `navigate` target into something a browser can load: a url as given,
/// or a local file resolved against the working directory and made a `file://`
/// url. Checked before a browser is pointed at it, because the path comes from
/// a model and a browser will happily open anything it is given.
fn browser_target(raw: &str, cwd: &str) -> Result<String, String> {
    let l = raw.to_ascii_lowercase();
    if l.starts_with("http://") || l.starts_with("https://") || l.starts_with("file://") {
        return Ok(raw.to_string());
    }
    if l.contains("://") {
        return Err(format!("`{raw}` isn't a url this browser will load. Use http://, https://, file://, or a path to a local .html file."));
    }
    let full = tools::resolve(cwd, raw);
    if !full.is_file() {
        return Err(format!(
            "{} isn't a file. Navigate to a url, or to a local .html file that exists.",
            full.display()
        ));
    }
    Ok(format!(
        "file:///{}",
        full.display().to_string().replace('\\', "/")
    ))
}

/// Turn a `Runtime.evaluate` result into tool text. `returnByValue` means a
/// string result arrives JSON-quoted, so a plain object is formatted rather
/// than printed as `{"a":1}`.
fn text_result(v: &Value, empty: &str) -> Value {
    let s = v
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string());
    let s = s.trim();
    if s.is_empty() || s == "null" {
        return json!(empty);
    }
    // A JSON object or array we produced ourselves: pretty-print it, since
    // these are answers meant to be read.
    if let Ok(pretty) = serde_json::from_str::<Value>(s) {
        if pretty.is_object() || pretty.is_array() {
            return json!(serde_json::to_string_pretty(&pretty).unwrap_or_else(|_| s.to_string()));
        }
    }
    json!(s)
}

fn artifact_input(input: &Value) -> Result<crate::artifacts::ArtifactInput, String> {
    serde_json::from_value(input.clone())
        .map_err(|error| format!("Invalid artifact input: {error}"))
}

#[derive(Clone)]
struct SubCtx {
    id: String,
    def: AgentDef,
}

/// The runtime policy for a subagent's fileRegex. Invalid patterns are a hard
/// denial too: project agent files are loaded without the settings-save
/// validator, so enforcement must never depend on that validator having run.
fn subagent_file_restriction_denial(
    def: &AgentDef,
    tool: &str,
    path: Option<&str>,
    cwd: &str,
) -> Option<String> {
    if !matches!(tool, "edit_file" | "multi_edit" | "write_file" | "bash") {
        return None;
    }
    let pattern = match def.edit_restriction() {
        Ok(Some(pattern)) => pattern,
        Ok(None) => return None,
        Err(error) => {
            return Some(format!(
                "FileRestrictionError: {error} File writes and bash are denied until the agent's fileRegex is corrected."
            ));
        }
    };
    if tool == "bash" {
        return Some(format!(
            "FileRestrictionError: the `{}` agent cannot use `bash` because shell commands and redirections could bypass its fileRegex restriction.",
            def.name
        ));
    }
    let description = def
        .groups
        .iter()
        .find(|group| group.name().eq_ignore_ascii_case("edit"))
        .map(|group| group.description())
        .filter(|description| !description.is_empty())
        .unwrap_or("");
    permissions::file_restriction_denial(
        &def.name,
        pattern.as_str(),
        description,
        tool,
        path.unwrap_or(""),
        cwd,
    )
}

#[derive(Clone)]
pub struct Agent {
    h: Arc<Harness>,
    task_id: String,
    sub: Option<SubCtx>,
    cancel: CancellationToken,
}

const INTERRUPTED: &str = "interrupted";
const MAX_GOAL_NUDGES: u32 = 25;
/// Plan mode: how many times to send an agent that ended its turn still holding
/// an unapproved plan back to the plan step. Low on purpose — the plan is one
/// decision, so a run that can't produce one in a couple of rounds should end
/// and leave the chat visibly still in plan mode rather than burn the context.
const MAX_PLAN_NUDGES: u32 = 3;
/// Ultrathread: how deep sub-agents may nest below the main agent.
const ULTRA_MAX_DEPTH: u8 = 2;
/// Ultrathread: sub-agents running at once across the whole tree. Well above the
/// depth cap's plausible fan-out: at depth 2 with no per-layer fanout, the tree
/// only *needs* this many running agents to look wide, and a cap lower than that
/// just makes the orchestrator poll `task_status` instead of working.
const ULTRA_MAX_RUNNING: usize = 40;
const MAX_ULTRA_NUDGES: u32 = 30;

/// The floor between keepalive pings, in milliseconds of wall clock. Providers
/// evict a cached prefix in roughly five minutes of inactivity, so pinging much
/// faster buys nothing and much slower lets the prefix go cold between pings.
/// Overridable in tests via `OPENLEASH_KEEPALIVE_MS` so no test has to sleep for
/// five real minutes to prove the loop works.
fn keepalive_interval() -> std::time::Duration {
    std::env::var("OPENLEASH_KEEPALIVE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(300))
}

/// How often a GitHub-backed wake re-checks the repo. A CI run takes minutes, so
/// a minute is plenty and keeps the poll inside GitHub's rate limit even for a
/// handful of parked chats. Overridable in tests via `OPENLEASH_GH_POLL_MS`.
fn gh_poll_interval() -> std::time::Duration {
    std::env::var("OPENLEASH_GH_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(60))
}

/// The `wait_for_event` tool schema.
///
/// Defined here rather than in `tools::schemas` because that file is another
/// agent's while this lands: the schema is a constant, so building it in the
/// runner keeps the feature self-contained *and* leaves the frozen tool list
/// byte-stable across requests (the one property the prompt cache depends on).
/// A later move into `tools::schemas` is pure relocation, no behaviour change.
fn wait_for_event_schema() -> Value {
    json!({
        "name": "wait_for_event",
        "description": "Park this task on a local trigger and end the turn. Use it when the honest next step is to wait for something outside the conversation rather than to poll: \"wait for CI on this branch\", \"wait for a review on PR 42\", or \"wait half an hour, then continue\". The task becomes `waiting`, comes off your hands, and a follow-up turn is started automatically when the trigger fires — you do NOT need to poll `task_status`. Only the main agent may park a task. Prefer a concrete, checkable source: `timer` for a delay, `ci` or `pr` for GitHub.",
        "input_schema": {"type":"object","properties":{
            "reason":{"type":"string","description":"Short note shown in the chat and fed back to you on the wake."},
            "source":{"type":"string","enum":["timer","ci","pr"],
                "description":"timer = wake after delay_s. ci = wake when the latest workflow run for branch finishes. pr = wake when PR `pr` gets a new comment/review."},
            "delay_s":{"type":"integer","description":"timer only: seconds to wait (30–86400)."},
            "branch":{"type":"string","description":"ci only: branch to watch (default: the task's branch)."},
            "pr":{"type":"integer","description":"pr only: pull request number."},
            "repo":{"type":"string","description":"owner/name (default: the working directory's origin remote)."}},
            "required":["source","reason"]}
    })
}

/// The main agent's tool list: core schemas plus the frozen MCP/plugin tail,
/// `wait_for_event`, and any deferred tools this runtime has loaded. Every
/// main-agent request must use this helper so side requests and real turns agree.
async fn main_tool_list(
    h: &Arc<Harness>,
    task_id: &str,
    plugins: &super::plugins::PluginsCfg,
    mcp: Vec<Value>,
) -> Vec<Value> {
    let mut core = tools::schemas(None, plugins);
    core.push(wait_for_event_schema());
    let settings = h.settings.read().await.clone();
    let off: Vec<String> = settings
        .mcp
        .iter()
        .filter(|m| !m.defer)
        .map(|m| m.name.clone())
        .collect();
    let deferral = toolindex::Deferral::new(
        core.len(),
        &mcp,
        &off,
        settings.defer_plugins,
        settings.tool_search,
    );
    let loaded = toolindex::loaded(&h.runtime(task_id), "main").await;
    deferral.tool_list(core, &loaded)
}

/// Which external thing a parked task is waiting on.
///
/// A closed set, and deliberately so: the model names `timer`/`ci`/`pr`, never a
/// URL or a command, so parking a task can only ever make the harness watch a
/// GitHub surface it already has tools for or count down a clock. There is no
/// shell, no arbitrary fetch, and so no reason for the permission gate to
/// interrupt the run to ask — which is why `wait_for_event` is not gated.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct WakeSource {
    pub(crate) source: String,
    pub(crate) reason: String,
    #[serde(default)]
    pub(crate) delay_s: u64,
    #[serde(default)]
    pub(crate) branch: String,
    #[serde(default)]
    pub(crate) pr: u64,
    #[serde(default)]
    pub(crate) repo: String,
    /// The GitHub state seen at park time, so a poll can tell *new* from "still
    /// the same run/comment that was already there" and not wake on the status
    /// quo. Only meaningful for `ci`/`pr`.
    #[serde(default)]
    pub(crate) baseline: String,
}

impl WakeSource {
    fn label(&self) -> String {
        match self.source.as_str() {
            "timer" => format!("{} (in {})", self.reason, human_delay(self.delay_s)),
            "ci" => format!(
                "{} (CI on {})",
                self.reason,
                if self.branch.is_empty() {
                    "this branch"
                } else {
                    &self.branch
                }
            ),
            "pr" => format!("{} (PR #{})", self.reason, self.pr),
            _ => self.reason.clone(),
        }
    }
    fn is_github(&self) -> bool {
        matches!(self.source.as_str(), "ci" | "pr")
    }
}

fn human_delay(s: u64) -> String {
    if s < 90 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", (s + 30) / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// Does this freshly fetched GitHub snapshot mean it is time to wake?
///
/// Split out as a pure function on purpose: this is the part with a rule in it
/// ("a *conclusion* on CI, a new comment or review on a PR — not the same one we
/// already saw"), and a rule is worth testing without a network. `since` is the
/// baseline captured at park time; `None` means "fire".
pub(crate) fn wake_reason(src: &WakeSource, snapshot: &Value) -> Option<String> {
    match src.source.as_str() {
        "ci" => {
            // Runs are newest-first. Fire once the newest run has a conclusion
            // (success/failure) *and* it is not the run we were already looking
            // at when we parked -- otherwise a task parked just after a finished
            // run would wake immediately on that same run.
            let run = snapshot["workflow_runs"].as_array()?.first()?;
            let conclusion = run["conclusion"].as_str().unwrap_or("");
            if conclusion.is_empty() {
                return None;
            }
            let id = run["id"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_default();
            if id == src.baseline {
                return None;
            }
            Some(format!(
                "CI run {id} finished: {conclusion} ({} · {})",
                run["name"].as_str().unwrap_or("a workflow"),
                run["head_branch"].as_str().unwrap_or("?")
            ))
        }
        "pr" => {
            // Any comment or review newer than the mark captured at park time.
            match latest_pr_mark(snapshot) {
                Some(id) if id != src.baseline => {
                    Some(format!("PR #{} has a new comment or review", src.pr))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Plain-ultrathread nesting cap for a tree without an X ladder.
fn ultra_max_depth(x: Option<&UltraX>) -> u8 {
    match x {
        Some(x) => x.max_depth(),
        None => ULTRA_MAX_DEPTH,
    }
}

/// How many sub-agents may run at once in this task's tree. An X ladder can
/// raise it; the ceiling is MAX_ULTRA_RUNNING.
fn ultra_running_cap(x: Option<&UltraX>) -> usize {
    match x {
        Some(x) => x.running_cap(),
        None => ULTRA_MAX_RUNNING,
    }
}

/// An agent's own ULTRATHREAD X ladder, when this task runs one.
fn task_x(t: &Task) -> Option<&UltraX> {
    t.ultra_x.as_ref().filter(|x| x.on())
}

/// Cap on how many sub-agents one agent may launch at the same time. Plain
/// ultrathread leans on the global cap and the "several at once" prompt instead;
/// an X ladder sets it per layer.
fn layer_fanout(x: Option<&UltraX>, depth: u8) -> Option<u8> {
    x?.layer(depth).map(|l| l.fanout).filter(|f| *f > 0)
}

fn parse_todos(input: &Value) -> Result<Vec<Todo>, String> {
    let mut todos: Vec<Todo> = serde_json::from_value(input["todos"].clone())
        .map_err(|e| format!("invalid todos: {e}"))?;
    if todos.len() > 20 {
        return Err("A todo list can have at most 20 items.".into());
    }
    if todos
        .iter()
        .filter(|todo| todo.status == "in_progress")
        .count()
        > 1
    {
        return Err("Only one todo can be in progress at a time.".into());
    }
    for todo in &mut todos {
        todo.content = todo.content.trim().to_string();
        todo.active_form = todo.active_form.trim().to_string();
        if todo.content.is_empty() || todo.content.chars().count() > 200 {
            return Err("Each todo needs a concise description (1-200 characters).".into());
        }
        if todo.active_form.chars().count() > 200 {
            return Err("Each activeForm must be at most 200 characters.".into());
        }
        if !matches!(
            todo.status.as_str(),
            "pending" | "in_progress" | "completed"
        ) {
            return Err(format!("Unknown todo status: {}", todo.status));
        }
    }
    Ok(todos)
}

// ───────────────────────────── entry points ─────────────────────────────

/// A user message. Starts a run, or — if one is in flight — queues it as
/// steering that gets folded into the very next model call.
pub async fn send(h: Arc<Harness>, task_id: String, text: String) -> Result<(), String> {
    send_as(h, task_id, text.clone(), text).await
}

/// Changes the user made mid-run take effect with their next message.
pub fn apply_pending(t: &mut Task) {
    let p = std::mem::take(&mut t.pending);
    if let Some(m) = p.get("model").and_then(|v| v.as_str()) {
        t.model = m.into();
    }
    if let Some(a) = p.get("assist").and_then(|v| v.as_str()) {
        t.assist = a.into();
    }
    if let Some(a) = p.get("agents").and_then(|v| v.as_array()) {
        t.agents = a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        // `explore` + `general` are always on.
        store::ensure_required_agents(&mut t.agents);
        t.subagents = !t.agents.is_empty();
    }
}

/// A plain text message, as the one content block the agent will read.
fn text_blocks(text: &str) -> Vec<Value> {
    vec![json!({"type": "text", "text": text})]
}

/// Put a message the user sent mid-turn into the queue, and show it as a queued
/// timeline item. The item carries the queue position, so the list above the
/// composer and the queue itself can never disagree about the order.
pub async fn enqueue(h: &Harness, task_id: &str, item: &mut Item, blocks: Vec<Value>) {
    let seq = h.runtime(task_id).send_seq.fetch_add(1, Ordering::SeqCst);
    h.runtime(task_id).queue.lock().await.push(QueuedMsg {
        item_id: item.id.clone(),
        seq,
        blocks,
    });
    // Merge, never replace. The item is the only durable trace of a queued
    // message — the queue itself is runtime state and does not survive a restart —
    // and it is also what the row above the composer is drawn from. Overwriting
    // `data` dropped whatever the message arrived with, so an Alt-Enter photo
    // showed as a queued row with no picture: the attachment the user had just
    // added was gone from the only place it was ever shown.
    if let Some(d) = item.data.as_object_mut() {
        d.insert("queued".into(), json!(true));
        d.insert("seq".into(), json!(seq));
    } else {
        item.data = json!({"queued": true, "seq": seq});
    }
    h.upsert_item(task_id, item.clone()).await;
}

/// Like `send`, but the model sees `model_text` while the timeline shows `display`.
pub async fn send_as(
    h: Arc<Harness>,
    task_id: String,
    display: String,
    text: String,
) -> Result<(), String> {
    send_opts(h, task_id, display, text, false).await
}

/// `later`: mid-run, hold the message until the current run ends (instead of steering now).
pub async fn send_opts(
    h: Arc<Harness>,
    task_id: String,
    display: String,
    text: String,
    later: bool,
) -> Result<(), String> {
    let rt = h.runtime(&task_id);
    let t = h.task(&task_id).await?;
    let (project, cwd) = {
        let guard = t.lock().await;
        (guard.project.clone(), guard.cwd.clone())
    };
    let hooks = {
        let settings = h.settings.read().await;
        checks::hooks_for(&settings, &project)
    };
    let cancel = rt
        .cancel
        .lock()
        .unwrap()
        .as_ref()
        .cloned()
        .unwrap_or_default();
    if let Some(reason) =
        checks::user_prompt_submit(&hooks, &text, &cwd, &project, &task_id, &cancel).await
    {
        let msg = format!("A user_prompt_submit hook refused the message:\n{reason}");
        h.upsert_item(
            &task_id,
            Item::new("notice", msg.clone(), json!({"level":"warning"})),
        )
        .await;
        return Err(msg);
    }
    let item = Item::new("user", display, json!({}));
    // The user sent this, so the chat is the one they are working in: put it at
    // the top of the list, whatever else is running.
    h.touch(&task_id).await;
    // Sending into a *parked* chat wakes it: a message is a person saying "not
    // now, this", which outranks any trigger the model was waiting on. Both the
    // deadline and the watcher go, or the background loop would later resume a
    // chat that has already moved on. A woken-early task is a normal turn, so
    // nothing below changes.
    if rt.wake_state.lock().unwrap().parked {
        unpark(h.as_ref(), &task_id);
        h.update_task(&task_id, |t| {
            if t.waiting_kind.as_deref() == Some("wake") {
                t.waiting_kind = None;
            }
        })
        .await;
    }
    // Messaging a paused task resumes it (just this one, even under a global pause).
    //
    // The raw flag, deliberately, not `is_paused`. `is_paused` only holds chats that
    // are *working*, but a run marks itself `running` before the router's pause
    // gate ever looks at it — so a chat sent to while idle would take the plain
    // path, start a run, and then park itself the moment it looked frozen. The
    // message below is the user picking this chat up, which is what `unpaused`
    // means, so under a global pause it always opts this one chat out.
    let was_paused = t.lock().await.paused.is_some() || h.settings.read().await.paused_all;
    if was_paused {
        // The text joins history *before* the pause lifts, and the router is told
        // to rebuild its frozen request. Thawing first would let the agent replay
        // the request it froze on — a turn this message wasn't part of — and
        // answer that instead.
        {
            let mut t = t.lock().await;
            apply_pending(&mut t);
            let cp = Checkpoint {
                item_index: t.items.len(),
                msg_index: t.messages.len(),
            };
            t.checkpoints.push(cp);
            t.messages.push(Message::user_text(text.clone()));
        }
        h.upsert_item(&task_id, item).await;
        // The router is always told, so it abandons the request it froze on and
        // rebuilds from history, which now holds this text. A run that starts
        // below clears the flag again as it goes round.
        rt.steered.store(true, Ordering::SeqCst);
        h.update_task(&task_id, |t| {
            t.paused = None;
            t.unpaused = true;
        })
        .await;
        h.pause_bell.notify_waiters();
        // Thawing a chat that is *also* stopped — nothing running to read the
        // text — would otherwise leave the message in history with nobody to
        // answer it: the transcript shows it, the chat stays "stopped", and the
        // agent never wakes. That is what sending into a stopped chat, or
        // editing a message and pressing Send after a stop, used to do.
        // `spawn_run` is a no-op if a run started in the meantime, and the
        // message is in history either way, so that race is harmless.
        if !rt.running.load(Ordering::SeqCst) {
            spawn_run(h, task_id);
        }
        return Ok(());
    }
    if rt.running.load(Ordering::SeqCst) {
        if !later {
            // A plain message steers: the agent picks it up on its very next
            // model call, the way it always has. Only Alt-Enter waits.
            steer_now(&h, &task_id, &text).await;
            h.upsert_item(&task_id, item).await;
            return Ok(());
        }
        let mut item = item;
        enqueue(&h, &task_id, &mut item, text_blocks(&text)).await;
        return Ok(());
    }
    {
        let mut t = t.lock().await;
        apply_pending(&mut t);
        let cp = Checkpoint {
            item_index: t.items.len(),
            msg_index: t.messages.len(),
        };
        t.checkpoints.push(cp);
        t.messages.push(Message::user_text(text));
    }
    h.upsert_item(&task_id, item).await;
    spawn_run(h, task_id);
    Ok(())
}

/// `data:<mime>;base64,<data>` → (media_type, base64). The shape the UI's
/// FileReader produces and the IPC `images` arrays carry.
pub fn parse_data_url(url: &str) -> Result<(String, String), String> {
    let (head, data) = url.split_once(',').ok_or("That image couldn't be read.")?;
    let mt = head
        .strip_prefix("data:")
        .and_then(|h| h.strip_suffix(";base64"))
        .ok_or("That image couldn't be read.")?;
    if mt.is_empty() || data.is_empty() {
        return Err("That image couldn't be read.".into());
    }
    Ok((mt.to_string(), data.to_string()))
}

/// `data:<mime>;base64,<data>` URLs → Anthropic image content blocks.
fn image_blocks(images: &[String]) -> Result<Vec<Value>, String> {
    let mut blocks = vec![];
    for url in images {
        let (mt, data) = parse_data_url(url)?;
        blocks.push(
            json!({"type": "image", "source": {"type": "base64", "media_type": mt, "data": data}}),
        );
    }
    Ok(blocks)
}

/// A user message with pasted images (`data:<mime>;base64,<data>` URLs).
///
/// Mid-run a plain message steers: the photos go to the agent on its next model
/// call as real image blocks. Alt-Enter waits for the end of the turn instead,
/// which is what puts it in the queue list above the composer.
pub async fn send_images(
    h: Arc<Harness>,
    task_id: String,
    text: String,
    images: Vec<String>,
    later: bool,
) -> Result<(), String> {
    let rt = h.runtime(&task_id);
    let mut blocks = image_blocks(&images)?;
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text.clone()}));
    }
    let t = h.task(&task_id).await?;
    // Same as `send_opts`: the user sent this, so the chat goes to the top.
    h.touch(&task_id).await;
    // Images are user input too: clear an active park before delivering them.
    if rt.wake_state.lock().unwrap().parked {
        unpark(h.as_ref(), &task_id);
        h.update_task(&task_id, |t| {
            if t.waiting_kind.as_deref() == Some("wake") {
                t.waiting_kind = None;
            }
        })
        .await;
    }
    // Same as `send_opts`, and for the same reason: the raw flag, not the
    // predicate, so a photo into an idle chat under a global pause can't leave a
    // run that freezes itself on its way in.
    let was_paused = t.lock().await.paused.is_some() || h.settings.read().await.paused_all;
    if was_paused {
        // Same hand-off as `send_opts`: history first, then the thaw, so the
        // frozen request is rebuilt with these blocks already in it.
        {
            let mut t = t.lock().await;
            apply_pending(&mut t);
            let cp = Checkpoint {
                item_index: t.items.len(),
                msg_index: t.messages.len(),
            };
            t.checkpoints.push(cp);
            t.messages.push(Message::user(blocks.clone()));
        }
        let item = Item::new("user", text, json!({"images": images}));
        h.upsert_item(&task_id, item).await;
        // Same hand-off as `send_opts`: the router is told to rebuild, and a
        // chat with nothing running gets a run so these photos are looked at.
        rt.steered.store(true, Ordering::SeqCst);
        h.update_task(&task_id, |t| {
            t.paused = None;
            t.unpaused = true;
        })
        .await;
        h.pause_bell.notify_waiters();
        if !rt.running.load(Ordering::SeqCst) {
            spawn_run(h, task_id);
        }
        return Ok(());
    }
    if rt.running.load(Ordering::SeqCst) {
        if !later {
            // Same rule as text: a plain message steers, only Alt-Enter waits.
            // A photo sent on its own still gets the reminder, because the agent
            // has to be told to look at the picture rather than find it unexplained
            // at the end of a turn.
            let note = if text.is_empty() {
                "The user sent this image while you were working, and said nothing with it. Look at it now.".to_string()
            } else {
                text.clone()
            };
            steer_now(&h, &task_id, &note).await;
            // The photos ride as real image blocks on the agent's next request,
            // not as a caption naming them.
            let imgs: Vec<(String, String)> = blocks
                .iter()
                .filter(|b| b["type"] == "image")
                .filter_map(|b| {
                    Some((
                        b["source"]["media_type"].as_str()?.to_string(),
                        b["source"]["data"].as_str()?.to_string(),
                    ))
                })
                .collect();
            h.note_images(&task_id, "main", imgs).await;
            h.upsert_item(&task_id, Item::new("user", text, json!({"images": images})))
                .await;
            return Ok(());
        }
        let mut item = Item::new("user", text, json!({"images": images}));
        enqueue(&h, &task_id, &mut item, blocks).await;
        return Ok(());
    }
    let item = Item::new("user", text, json!({"images": images}));
    {
        let mut t = t.lock().await;
        apply_pending(&mut t);
        let cp = Checkpoint {
            item_index: t.items.len(),
            msg_index: t.messages.len(),
        };
        t.checkpoints.push(cp);
        t.messages.push(Message::user(blocks));
    }
    h.upsert_item(&task_id, item).await;
    spawn_run(h, task_id);
    Ok(())
}

pub fn spawn_run(h: Arc<Harness>, task_id: String) {
    tauri::async_runtime::spawn(async move {
        run_main(h, task_id).await;
    });
}

/// Cancel the agent and every sub-agent, without touching background commands.
/// `interrupt` is this plus the kill; `purge_task` is this plus its own, because
/// it runs on a blocking thread that can't await the async half.
pub fn stop_tokens(h: &Harness, task_id: &str) {
    let rt = h.runtime(task_id);
    if let Some(c) = rt.cancel.lock().unwrap().as_ref() {
        c.cancel();
    }
    {
        let mut bg = rt.bg_cancel.lock().unwrap();
        bg.cancel();
        *bg = CancellationToken::new();
    }
    // And the keepalive. It is a parked background loop that spends money on a
    // timer, so leaving it running past a stop would have the chat keep pinging
    // a provider for a run the user deliberately ended. `stop_tokens` is the one
    // place every stop path (interrupt, force pause, purge) already goes through.
    let keepalive = rt.keepalive_cancel.lock().unwrap().take();
    if let Some(c) = keepalive {
        c.cancel();
    }
}

/// Esc: stops the main agent and every sub-agent, background ones included.
///
/// Everything the chat had running dies with it — the foreground commands die
/// with `cancel` (each `bash` waits on a child token), and the background ones
/// are killed here. A background command outlives the run that started it (a
/// dev server is meant to), so nothing else would ever stop it: before this,
/// a stop left `npm run dev` and every watcher running with the chat showing
/// "stopped". Whichever jobs went down are named in the note the agent reads
/// on its next request, so it reruns the ones it still needs instead of
/// assuming a server is up.
pub async fn interrupt(h: &Harness, task_id: &str) {
    stop_tokens(h, task_id);
    // A stop also drops any park: the chat is no longer *waiting for X*, it is
    // stopped, and the watcher must not later wake a task the user ended.
    if h.runtime(task_id).wake_state.lock().unwrap().parked {
        unpark(h, task_id);
        h.update_task(task_id, |t| {
            if t.waiting_kind.as_deref() == Some("wake") {
                t.waiting_kind = None;
            }
        })
        .await;
    }
    let killed = h.bg.kill_task_with_reasons(
        task_id,
        "the user stopped this chat before it finished — rerun it",
    );
    if !killed.is_empty() {
        let list: Vec<String> = killed
            .iter()
            .map(|b| format!("`{}` ({})", b.cmd, b.id))
            .collect();
        h.note(task_id, "main", format!("<system-reminder>The user STOPPED this task. These background commands were killed before they finished, so whatever they were serving or watching is gone and has to be started again: {}</system-reminder>", list.join(", "))).await;
    }
    h.emit(task_id, "bg", json!(h.bg.list(task_id)));
    h.refresh_busy(task_id).await;
}

/// Counts one foreground command in `Runtime::fg` for exactly as long as it lives.
struct FgGuard(Arc<super::Runtime>);
impl FgGuard {
    fn new(rt: Arc<super::Runtime>) -> Self {
        rt.fg.fetch_add(1, Ordering::SeqCst);
        FgGuard(rt)
    }
}
impl Drop for FgGuard {
    fn drop(&mut self) {
        self.0.fg.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Only a message from a peer agent is news. The user reaching all agents at
/// once is for the orchestrator to sort out: the rest pick it up on their own
/// next step, and waking each one for a message it should ignore just burns a turn.
const MAIL_KINDS: &str = "agent-msg";

/// A harness note, tagged with who it is from so a receiver can tell an
/// ordinary note apart from one it should drop everything to read.
/// Wrap the text in `Kinded::user` for anything the user said or asked every agent.
struct Kinded(String);

impl Kinded {
    fn user(text: &str) -> Self {
        Self(format!("<kind:user-msg>\n{text}"))
    }

    /// The message an agent addresses to others, including a user's broadcast:
    /// the main agent can relay it with a plain `note`.
    fn agent(text: &str) -> Self {
        Self(format!("<kind:agent-msg>\n{text}"))
    }
}

/// Steer the main agent with a message the user typed mid-turn and has now asked
/// to send at once: the same delivery a queued message gets when a run is in
/// flight, without waiting for the run to end.
pub async fn steer_now(h: &Harness, task_id: &str, text: &str) {
    let note = format!("<system-reminder>The user sent this message while you were working. Take it into account right away:</system-reminder>\n{text}");
    h.note(task_id, "main", Kinded::user(&note).0).await;
}

/// The user answered a question the agent asked without waiting for it.
///
/// The answer rides the same path a mid-turn message does: a note in the
/// agent's inbox, read on its next model call. So it never interrupts a tool
/// call or a running step — which is the whole point of asking this way — and
/// it can still land mid-turn if the user got to it while the agent was working.
/// If the run has already finished there is nobody to read it, so the answer
/// goes to the chat as a new user turn instead of vanishing.
pub async fn answer_nonblocking(
    h: &Arc<Harness>,
    task_id: &str,
    item_id: &str,
    response: &Value,
) -> Result<(), String> {
    let qs: Vec<Value> = {
        let Ok(t) = h.task(task_id).await else {
            return Err("That chat is gone.".into());
        };
        let t = t.lock().await;
        let Some(it) = t
            .items
            .iter()
            .find(|i| i.id == item_id && i.kind == "asklater")
        else {
            return Err("That question is no longer on screen.".into());
        };
        it.data["questions"].as_array().cloned().unwrap_or_default()
    };
    if qs.is_empty() {
        return Err("That question has nothing to answer.".into());
    }
    let mut body = String::new();
    let skips = render_answers(&mut body, &qs, response);
    let note = format!("<system-reminder>The user answered the question you asked earlier with ask_nonblocking, while you were still working. They have not seen anything since they answered it, so fit the answer into what you are doing now — it is a preference or a detail, not a new task. If you had already decided without them and this says otherwise, change it and say so. If you have already finished, don't start new work over it: mention what you'd have done differently and stop.</system-reminder>\n\n{body}");
    // Record the answer on the item before handing it over. Without this the
    // card above the composer still reads as unanswered, so it would sit there
    // forever next to a question that has in fact been dealt with.
    h.patch_item(task_id, item_id, |i| {
        if response["dismissed"].as_bool().unwrap_or(false) {
            i.data["dismissed"] = json!(true);
        } else {
            i.data["answers"] = response["answers"].clone();
            i.data["notes"] = response["notes"].clone();
            i.data["skipped"] = json!(skips);
        }
    })
    .await;
    h.note(task_id, "main", format!("<kind:user-msg>\n{note}"))
        .await;
    // Nothing is running to read the inbox, so hand it to the chat as a turn.
    if !h.runtime(task_id).running.load(Ordering::SeqCst) {
        h.touch(task_id).await;
        spawn_run(h.clone(), task_id.to_string());
    }
    Ok(())
}

/// Hand a finished background sub-agent's report to the main agent: as a note
/// if it's working, otherwise as a new turn so it can react right away.
async fn deliver_to_main(h: &Arc<Harness>, task_id: &str, text: String) {
    h.note(task_id, "main", Kinded::agent(&text).0).await;
    if !h.runtime(task_id).running.load(Ordering::SeqCst) {
        spawn_run(h.clone(), task_id.to_string());
    }
}

/// The user sent one message to every agent working in the task at once. It is
/// addressed to all of them, so most of them must not act on it: it only lands
/// on the agents whose own work is actually about what the message names.
pub const BROADCAST_RULE: &str = "\
This is a GLOBAL message from the user, sent to every agent working in this task at once. \
Act on it ONLY if it applies to the work you are actually doing: if you have edited, \
created or own that area, or your work depends on it. Otherwise ignore it completely and \
carry on with your task — do not re-read your files because of it, do not touch code you \
have not been working in, and do not send a reply just to acknowledge it. \
If it does apply, fold it into what you are doing right away, undoing your own earlier work \
if that's what it asks for. Don't forward it to other agents.";

/// Wrap a broadcast in the framing every recipient gets.
fn broadcast_note(text: &str) -> String {
    format!("<system-reminder>{BROADCAST_RULE}</system-reminder>\n{text}")
}

/// The user broadcasts a message to every working agent in the task: the main
/// agent plus every sub-agent, nested ultrathread workers included. Returns who
/// it reached.
pub async fn broadcast(h: &Arc<Harness>, task_id: &str, text: &str) -> Result<Vec<String>, String> {
    let msg = text.trim();
    if msg.is_empty() {
        return Err("Write the message to broadcast first.".into());
    }
    let t = h.task(task_id).await?;
    // A broadcast reaches only the agents in this one chat, so it is a user
    // action on that chat alone — it must not reorder anything else.
    h.touch(task_id).await;
    let note = broadcast_note(msg);
    // Every sub-agent is in `subs`, however deep, so this one loop covers them all.
    let subs: Vec<SubInfo> = t
        .lock()
        .await
        .subs
        .iter()
        .filter(|s| s.status == "running")
        .cloned()
        .collect();
    let mut sent: Vec<String> = vec![];
    for s in &subs {
        h.note(task_id, &s.id, Kinded::user(&note).0).await;
        h.upsert_sub_item(
            task_id,
            &s.id,
            Item::new(
                "notice",
                format!("Broadcast from the user: {msg}"),
                json!({"level": "msg", "global": true}),
            ),
        )
        .await;
        sent.push(s.id.clone());
    }
    // The main agent: as steering if it's already working, as its own turn otherwise.
    if h.runtime(task_id).running.load(Ordering::SeqCst) {
        h.note(task_id, "main", Kinded::user(&note).0).await;
    } else {
        send_as(h.clone(), task_id.to_string(), msg.to_string(), note).await?;
    }
    sent.insert(0, "main".into());
    // Remember who it went to, so the user can watch who takes it on.
    *h.runtime(task_id).broadcast.lock().await = Some(Broadcast {
        text: msg.to_string(),
        who: sent.clone(),
        replied: vec![],
    });
    h.upsert_item(
        task_id,
        Item::new(
            "notice",
            format!("You broadcast to all agents: {msg}"),
            json!({"level": "msg", "global": true, "replied": []}),
        ),
    )
    .await;
    Ok(sent)
}

/// Note that an agent acted on the open broadcast. Called from the run loop once
/// an agent that was sent one starts a real turn on it: it took the message on,
/// which is the signal the user is waiting for.
async fn mark_replied(h: &Harness, task_id: &str, agent: &str) {
    let replied = {
        let rt = h.runtime(task_id);
        let mut slot = rt.broadcast.lock().await;
        let Some(b) = slot.as_mut() else { return };
        if b.took(agent) || !b.who.iter().any(|x| x == agent) {
            return;
        }
        b.replied.push(agent.to_string());
        b.replied.clone()
    };
    // The broadcast line grows a "→ n agents on it" tail as they answer.
    let Ok(t) = h.task(task_id).await else { return };
    let id = t
        .lock()
        .await
        .items
        .iter()
        .rev()
        .find(|i| i.kind == "notice" && i.data["global"] == true)
        .map(|i| i.id.clone());
    if let Some(id) = id {
        h.patch_item(task_id, &id, |it| it.data["replied"] = json!(replied))
            .await;
    }
}

/// The user typed into a sub-agent's panel. A running sub-agent gets it on its
/// next request; a finished one continues with it, and its follow-up report
/// goes to the main agent.
/// The user typed into a sub-agent's panel. A running sub-agent gets it on its
/// next request; a finished one continues with it, and its follow-up report
/// goes to the main agent. Pasted images ride along as real image blocks.
pub async fn sub_message(
    h: &Arc<Harness>,
    task_id: &str,
    sub_id: &str,
    text: String,
    images: Vec<String>,
) -> Result<(), String> {
    let t = h.task(task_id).await?;
    // Typing into a sub-agent's panel is the user working in this chat, so it
    // counts the same as a message to the main agent.
    h.touch(task_id).await;
    let (info, saved, project, assist) = {
        let t = t.lock().await;
        let info = t
            .subs
            .iter()
            .find(|s| s.id == sub_id)
            .cloned()
            .ok_or("That subagent is gone.")?;
        (
            info,
            t.sub_msgs.get(sub_id).cloned(),
            t.project.clone(),
            t.assist.clone(),
        )
    };
    let img_blocks = image_blocks(&images)?;
    let mut data = json!({"direct": true});
    if !images.is_empty() {
        data["images"] = json!(images);
    }
    h.upsert_sub_item(task_id, sub_id, Item::new("user", text.clone(), data))
        .await;
    if info.status == "running" {
        h.note(task_id, sub_id, Kinded::user(&format!("<system-reminder>The user sent you this message directly. Take it into account right away:</system-reminder>\n{text}")).0).await;
        h.note_images(
            task_id,
            sub_id,
            img_blocks
                .iter()
                .filter_map(|b| {
                    Some((
                        b["source"]["media_type"].as_str()?.to_string(),
                        b["source"]["data"].as_str()?.to_string(),
                    ))
                })
                .collect(),
        )
        .await;
        return Ok(());
    }
    let mut init = saved
        .ok_or("This subagent's conversation wasn't saved (it ran before this feature existed).")?;
    let mut blocks = img_blocks;
    blocks.push(json!({"type": "text", "text": format!("<system-reminder>The user sent you this message directly.</system-reminder>\n{text}")}));
    push_user_blocks(&mut init, blocks);
    let defs = store::all_agents(&*h.settings.read().await, &project);
    let def = defs
        .iter()
        .find(|d| d.id == info.role)
        .cloned()
        .unwrap_or_else(|| fallback_def(&info.role));
    let _ = assist;
    let agent = Agent {
        h: h.clone(),
        task_id: task_id.to_string(),
        sub: None,
        cancel: h.runtime(task_id).bg_cancel.lock().unwrap().clone(),
    };
    let (h2, tid, sid) = (h.clone(), task_id.to_string(), sub_id.to_string());
    tauri::async_runtime::spawn(async move {
        let res = agent
            .drive_sub(
                sid.clone(),
                def.clone(),
                info.item_id.clone(),
                init,
                (!info.parent.is_empty()).then(|| info.parent.clone()),
                agent.cancel.clone(),
            )
            .await;
        if let Ok(r) = res {
            let note = format!("<system-reminder>The user messaged your `{}` subagent (\"{}\") directly, and it followed up. Its new report:</system-reminder>\n{}", def.id, info.task, r);
            deliver_to_main(&h2, &tid, note).await;
        }
    });
    Ok(())
}

/// "What is it doing?" — a side request over a copy of the chat plus live state. The real
/// run isn't interrupted and nothing is added to its history; works while paused too.
pub async fn status_summary(h: &Arc<Harness>, task_id: &str) -> Result<Value, String> {
    // One pass under the lock, copying out only what the status report reads.
    // This cloned the entire task and then cloned the message list out of the
    // clone again, so asking "what is it doing?" on a long chat made two full
    // copies of a multi-megabyte transcript.
    let (mut messages, status, step, paused, todos, live, model) = {
        let tr = h.task(task_id).await?;
        let g = tr.lock().await;
        if g.messages.is_empty() {
            return Ok(json!({"text": "Nothing has happened in this chat yet.", "model": ""}));
        }
        let live: Vec<SubInfo> = g
            .subs
            .iter()
            .filter(|s| s.status == "running")
            .cloned()
            .collect();
        (
            g.messages.clone(),
            g.status.clone(),
            g.step.clone(),
            g.paused.is_some(),
            g.todos.clone(),
            live,
            g.model.clone(),
        )
    };
    let running = status == "running" || status == "waiting";
    fix_dangling(&mut messages);
    if running {
        if let Some(last) = messages.last_mut().filter(|m| m.role == "user") {
            for b in last.content.iter_mut().filter(|b| {
                b["type"] == "tool_result"
                    && tools::tool_text(&b["content"]).starts_with("This tool call was interrupted")
            }) {
                b["content"] = json!("(still running)");
                b.as_object_mut().map(|o| o.remove("is_error"));
            }
        }
    }
    prune_images(&mut messages);
    // Live state the conversation doesn't show: todos, subagents' latest steps, background jobs.
    let mut state = format!(
        "Chat status: {}{} · step: {}\n",
        status,
        if paused { " (paused)" } else { "" },
        step
    );
    if !todos.is_empty() {
        state.push_str("\nTodos:\n");
        for x in &todos {
            state.push_str(&format!("- [{}] {}\n", x.status, x.content));
        }
    }
    if !live.is_empty() {
        state.push_str("\nRunning subagents:\n");
        // Each agent's last few timeline items, read in the same short pass.
        let recent: HashMap<String, Vec<String>> = {
            let tr = h.task(task_id).await?;
            let g = tr.lock().await;
            live.iter()
                .filter_map(|s| {
                    g.sub_items.get(&s.id).map(|l| {
                        (
                            s.id.clone(),
                            l.iter()
                                .rev()
                                .filter(|i| i.kind == "tool" || i.kind == "text")
                                .take(4)
                                .map(|i| {
                                    let what = if i.kind == "tool" {
                                        format!(
                                            "{} {}",
                                            i.data["name"].as_str().unwrap_or("tool"),
                                            i.data["input"]
                                                .to_string()
                                                .chars()
                                                .take(120)
                                                .collect::<String>()
                                        )
                                    } else {
                                        i.text.chars().take(160).collect()
                                    };
                                    format!("    · {what}")
                                })
                                .collect(),
                        )
                    })
                })
                .collect()
        };
        for s in &live {
            state.push_str(&format!(
                "- `{}` ({}) on: {} · now: {}\n",
                s.id, s.role, s.task, s.meta
            ));
            for r in recent.get(&s.id).into_iter().flatten().rev() {
                state.push_str(r);
                state.push('\n');
            }
        }
    }
    let bg: Vec<_> =
        h.bg.list(task_id)
            .into_iter()
            .filter(|b| b.running)
            .collect();
    if !bg.is_empty() {
        state.push_str("\nBackground commands:\n");
        for b in bg {
            state.push_str(&format!(
                "- {} `{}` · last line: {}\n",
                b.id, b.cmd, b.last_line
            ));
        }
    }
    let ask = format!("<system-reminder>STATUS CHECK from the user. This is a side request: your real run is NOT interrupted and this answer is not added to your history. Using the conversation and the live state below, write a short status report in markdown with these parts, skipping any that are empty: **Now** (what is happening at this moment, 1-2 lines), **Done so far**, **In flight** (each running subagent / background command and what it's doing), **Next**, **Blockers / risks**. Be concrete (files, commands, numbers). At most ~200 words. Do not call tools.</system-reminder>\n\nLive state:\n{state}");
    push_user_blocks(&mut messages, vec![json!({"type": "text", "text": ask})]);
    // Same frozen prefix as the main agent, so it's mostly a cache read.
    let (system, mcp, plugins) = frozen_prefix(h, task_id).await?;
    let tl = main_tool_list(h, task_id, &plugins, mcp).await;
    let req = ChatRequest {
        system,
        messages,
        tools: tl,
        effort: 1,
        max_tokens: 2_000,
        cache_key: format!("ol-{task_id}"),
    };
    let (text, usage, target) = match router::oneshot(
        h,
        Who {
            task_id,
            sub: None,
            side: true,
            turn: false,
            usage_id: "side",
        },
        &model,
        &req,
        &CancellationToken::new(),
    )
    .await
    {
        Ok(x) => x,
        Err(RouteErr::Fatal(e)) | Err(RouteErr::TooLong(e)) => return Err(e),
        // Nothing to rebuild here: a one-shot is a single shot by definition.
        Err(RouteErr::Cancelled) | Err(RouteErr::Refresh) => return Err("Cancelled".into()),
    };
    let cost = usage.cost(&providers::model_info(&target.model_id));
    record_spend(h, cost, usage.input + usage.output).await;
    h.update_task(task_id, |t| t.usage.cost += cost).await;
    Ok(json!({"text": text.trim(), "model": target.model_id, "via": target.label()}))
}

/// `/btw <question>` — a quick side question answered from the chat so far.
///
/// A side request over a *copy* of the conversation, in the shape of
/// `status_summary`: the run is not interrupted, nothing is added to the
/// history, and it works while the chat is paused. The caller puts the answer in
/// the timeline as an `aside` item, which is UI-only — model context is
/// `messages`, so an aside the user can re-read never becomes something the
/// agent itself sees.
///
/// Two things are deliberately *not* inherited from the main turn:
///
///  - **No tools, at the wire level.** The empty `tools` list (the same one
///    `who_ask` already sends) is the real guard here, not the prompt: an agent
///    that answered "let me check" and then ran `grep` would spend a whole tool
///    round to answer a question asked *about* the work rather than in it. A
///    model can still emit a `tool_use` regardless, so one is never executed and
///    a turn that produced nothing else is reported as the failure it is.
///  - **Its own cache key.** Sharing `ol-{task}` would let this reply become the
///    cached tail the main turn reads next, which is exactly the pollution the
///    command exists to avoid.
///
/// Spent at the chat's own effort rather than a floored one: a question is not
/// work to be done, so it gets the level the user set for work they meant.
pub async fn btw(
    h: &Arc<Harness>,
    task_id: &str,
    question: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let (mut messages, model, effort) = {
        let t = h.task(task_id).await?;
        let g = t.lock().await;
        (g.messages.clone(), g.model.clone(), g.effort)
    };
    if messages.is_empty() {
        return Err("This chat has nothing to go on yet — send it something first.".into());
    }
    // The same repairs the main loop makes before it re-sends history: a
    // tool_use with no result behind it is a malformed request, and images are
    // pruned because a side question cannot look at them usefully.
    fix_dangling(&mut messages);
    prune_images(&mut messages);
    push_user_blocks(
        &mut messages,
        vec![json!({"type": "text", "text": prompt::aside_prompt(question)})],
    );
    let t = h.task(task_id).await?;
    // `prefix_for`, not `frozen_prefix`: this must not write the rebuilt prefix
    // back onto the chat, which is not its to change.
    let (system, _mcp, _plugins) = {
        let g = t.lock().await;
        prefix_for(h, &g).await
    };
    let req = ChatRequest {
        system,
        messages,
        tools: vec![],
        effort,
        max_tokens: 4_000.min(router::max_output(&model, "")),
        cache_key: format!("ol-btw-{task_id}"),
    };
    // `request` rather than `oneshot` so a tool_use that comes back anyway can be
    // dropped below, instead of being executed by a one-shot that ignores it.
    let (turn, target) = match router::request(
        h,
        Who {
            task_id,
            sub: None,
            side: true,
            turn: false,
            usage_id: "side",
        },
        &model,
        &req,
        &mut |_e: StreamEvent| {},
        cancel,
    )
    .await
    {
        Ok(x) => x,
        Err(RouteErr::Fatal(e)) | Err(RouteErr::TooLong(e)) => return Err(e),
        Err(RouteErr::Cancelled) => return Err("Cancelled".into()),
        // Nothing to rebuild: one shot, so a refresh just means the model changed
        // under us. Answer with what we got rather than silently dropping it.
        Err(RouteErr::Refresh) => return Err("The model changed mid-answer. Ask again.".into()),
    };
    let text = turn
        .content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    // The empty `tools` list is the guard; this is the backstop for a model that
    // calls one anyway. Nothing is executed either way — `router::request` only
    // returns the turn — so all that is left is the case where the model spent the
    // whole answer on a tool call and said nothing, which would otherwise render as
    // a blank aside and read as a bug.
    if text.is_empty() && turn.content.iter().any(|b| b["type"] == "tool_use") {
        return Err("The model reached for a tool to answer that. Side questions can't use tools — try asking it as a normal message.".into());
    }
    let usage = turn.usage;
    let cost = usage.cost(&providers::model_info(&target.model_id));
    record_spend(h, cost, usage.input + usage.output).await;
    h.update_task(task_id, |t| t.usage.cost += cost).await;
    Ok(text)
}

/// Park a finished task on an external trigger and start the watcher.
///
/// The model calls this as a tool when the honest next step is "wait for X"
/// rather than "poll X". It ends the turn (`waiting_kind = "wake"`) and
/// `spawn_wake` runs the trigger in the background; when it fires it calls
/// `resume`, which is the *same* path the user's Resume button uses — so a woken
/// task is an ordinary followed-up turn, not a second turn loop. See `resume` for
/// how the deadline is enforced.
pub(crate) async fn park_waiting(
    h: &Arc<Harness>,
    task_id: &str,
    src: WakeSource,
) -> Result<(), String> {
    let rt = h.runtime(task_id);
    // A keepalive has nothing to keep warm once the answer is "wait": the whole
    // point of parking is that no request is coming for a while.
    if let Some(c) = rt.keepalive_cancel.lock().unwrap().take() {
        c.cancel();
    }
    let deadline = if src.source == "timer" {
        (chrono::Utc::now().timestamp_millis().max(0) as u64)
            .saturating_add(src.delay_s.saturating_mul(1000))
    } else {
        0
    };
    let (generation, cancel, previous) = {
        let mut wake = rt.wake_state.lock().unwrap();
        wake.generation = wake.generation.wrapping_add(1).max(1);
        wake.parked = true;
        wake.deadline_ms = deadline;
        let cancel = CancellationToken::new();
        let previous = wake.cancel.replace(cancel.clone());
        (wake.generation, cancel, previous)
    };
    if let Some(previous) = previous {
        previous.cancel();
    }
    let label = src.label();
    h.update_task(task_id, |t| {
        t.status = "waiting".into();
        t.waiting_kind = Some("wake".into());
        t.step = format!("Waiting for {label}");
    })
    .await;
    h.upsert_item(
        task_id,
        Item::new(
            "notice",
            format!("Waiting for {label}"),
            json!({"level": "event", "sum": format!("waiting · {label}")}),
        ),
    )
    .await;
    h.save_task_now(task_id).await;
    spawn_wake(h.clone(), task_id.to_string(), src, generation, cancel);
    Ok(())
}

/// Clear a parked task's trigger: the model going back to work, the user
/// sending or resuming, or a stop. Idempotent — most tasks were never parked.
fn unpark(h: &Harness, task_id: &str) {
    let rt = h.runtime(task_id);
    let cancel = {
        let mut wake = rt.wake_state.lock().unwrap();
        wake.generation = wake.generation.wrapping_add(1).max(1);
        wake.parked = false;
        wake.deadline_ms = 0;
        wake.cancel.take()
    };
    if let Some(cancel) = cancel {
        cancel.cancel();
    }
    let keepalive = rt.keepalive_cancel.lock().unwrap().take();
    if let Some(c) = keepalive {
        c.cancel();
    }
}

/// Pure: has a `timer` wake's delay elapsed?
///
/// Split from the loop on purpose: "the timer logic is testable without real time
/// passing" is the requirement, and `elapsed = now >= start + delay` is a
/// decision about three integers. The loop supplies the numbers; this decides.
pub(crate) fn timer_elapsed(started_ms: u64, delay_s: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(started_ms) >= delay_s.saturating_mul(1000)
}

/// How long the watcher naps before its next look. A timer naps its own
/// remaining time (clamped, so a sub-second remainder still fires promptly and a
/// long delay does not sleep past a stop); a GitHub wake naps the shared poll
/// interval, since it is rate-limited by GitHub, not by its own clock.
pub(crate) fn wake_nap_ms(source: &str, remaining_ms: u64, poll_ms: u64) -> u64 {
    if source == "timer" {
        remaining_ms.clamp(50, 5_000)
    } else {
        poll_ms
    }
}

/// One spawned task owns both the countdown and the GitHub poll. A watcher
/// exits when its trigger fires or its generation is cleared/superseded.
fn spawn_wake(
    h: Arc<Harness>,
    task_id: String,
    src: WakeSource,
    generation: u64,
    cancel: CancellationToken,
) {
    tauri::async_runtime::spawn(async move {
        let started = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let reason = loop {
            if !wake_generation_is_current(&h, &task_id, generation) || cancel.is_cancelled() {
                return;
            }
            let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
            let elapsed = now.saturating_sub(started);
            let remaining = src.delay_s.saturating_mul(1000).saturating_sub(elapsed);
            if src.source == "timer"
                && timer_elapsed(
                    started,
                    src.delay_s,
                    chrono::Utc::now().timestamp_millis() as u64,
                )
            {
                break src.reason.clone();
            }
            if src.is_github() {
                if let Some(reason) = wake_github_snapshot(&h, &task_id, &src).await {
                    break reason;
                }
            }
            // A timer polls its own remaining time in short hops so a shorter
            // `delay_s` than the poll interval still fires on time; a GitHub wake
            // polls on the shared interval.
            let nap = std::time::Duration::from_millis(wake_nap_ms(
                &src.source,
                remaining,
                gh_poll_interval().as_millis() as u64,
            ));
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(nap) => {}
            }
        };
        wake_now_generation(&h, &task_id, generation, reason).await;
    });
}

pub(crate) fn wake_generation_is_current(h: &Harness, task_id: &str, generation: u64) -> bool {
    let rt = h.runtime(task_id);
    let wake = rt.wake_state.lock().unwrap();
    wake.parked && wake.generation == generation
}

/// The moment a trigger fires: clear its own generation and start the follow-up turn.
/// A stale watcher cannot clear a newer park.
#[cfg(test)]
pub(crate) async fn wake_now(h: &Arc<Harness>, task_id: &str, note: String) {
    let rt = h.runtime(task_id);
    let generation = rt.wake_state.lock().unwrap().generation;
    wake_now_generation(h, task_id, generation, note).await;
}

async fn wake_now_generation(h: &Arc<Harness>, task_id: &str, generation: u64, note: String) {
    let cancel = {
        let rt = h.runtime(task_id);
        let mut wake = rt.wake_state.lock().unwrap();
        if !wake.parked || wake.generation != generation {
            return;
        }
        wake.parked = false;
        wake.deadline_ms = 0;
        wake.generation = wake.generation.wrapping_add(1).max(1);
        wake.cancel.take()
    };
    if let Some(cancel) = cancel {
        cancel.cancel();
    }
    h.note(
        task_id,
        "main",
        format!("<system-reminder>Woken: {note}. Continue the task.</system-reminder>"),
    )
    .await;
    h.update_task(task_id, |t| t.waiting_kind = None).await;
    let _ = resume(h, task_id, None).await;
}

/// The GitHub poll half of `park_waiting`: fetch the watched state, apply the
/// pure `wake_reason` rule, and report a reason if it is time.
async fn wake_github_snapshot(h: &Arc<Harness>, task_id: &str, src: &WakeSource) -> Option<String> {
    // The plugin being off is a transient miss, not a wake: the user may be
    // about to turn it on, and a task parked on CI is not wrong to wait.
    let snap = gh_snapshot(h, task_id, src).await?;
    wake_reason(src, &snap)
}

/// Fetch the one GitHub thing a parked trigger watches, as raw JSON.
///
/// Built inline rather than through the `github` tool's `api` action, and that is
/// the point: the watcher only ever issues the two GETs below, so it needs no
/// permission prompt and can point at nothing the model chose beyond the repo it
/// named. `None` on any miss (no token, plugin off, no origin remote, a failed
/// request) — the caller treats every one as "not yet".
async fn gh_snapshot(h: &Arc<Harness>, task_id: &str, src: &WakeSource) -> Option<Value> {
    let cfg = h.settings.read().await.plugins.github.clone();
    if !cfg.enabled {
        return None;
    }
    let token = tokio::task::spawn_blocking(move || super::plugins::github_token(&cfg))
        .await
        .ok()
        .flatten()
        .map(|(t, _)| t)?;
    let (cwd, branch) = {
        let t = h.task(task_id).await.ok()?;
        let g = t.lock().await;
        (g.cwd.clone(), g.branch.clone())
    };
    let repo = if !src.repo.is_empty() {
        src.repo.clone()
    } else {
        super::plugins::parse_repo(&git::remote_url(&cwd)?).unwrap_or_default()
    };
    if repo.is_empty() {
        return None;
    }
    let gh = super::plugins::Gh {
        http: &h.http,
        token,
        cwd: &cwd,
    };
    let snap = match src.source.as_str() {
        "ci" => {
            let b = if src.branch.is_empty() {
                branch
            } else {
                src.branch.clone()
            };
            let path = format!(
                "/repos/{repo}/actions/runs?branch={}&per_page=1",
                urlencoding::encode(&b)
            );
            gh.run(&json!({"action": "api", "method": "GET", "path": path}))
                .await
                .ok()?
        }
        "pr" => {
            if src.pr == 0 {
                return None;
            }
            let path = format!("/repos/{repo}/pulls/{}/comments", src.pr);
            gh.run(&json!({"action": "api", "method": "GET", "path": path}))
                .await
                .ok()?
        }
        _ => return None,
    };
    // `gh.run`'s `api` arm pretty-prints the JSON; parse it back for the rule.
    serde_json::from_str(&snap).ok()
}

/// The "already seen" mark a fresh park records, so its first poll does not wake
/// on the state of the world as it was *before* the park. The inverse of the
/// value `wake_reason` compares against.
pub(crate) fn baseline_of(src: &WakeSource, snapshot: &Value) -> String {
    match src.source.as_str() {
        "ci" => snapshot["workflow_runs"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r["id"].as_u64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        "pr" => latest_pr_mark(snapshot).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The newest comment/review id (or timestamp) on a PR, as `wake_reason` and
/// `baseline_of` both need it. GitHub lists these oldest-first, so the max is the
/// most recent arrival.
fn latest_pr_mark(snapshot: &Value) -> Option<String> {
    snapshot
        .as_array()?
        .iter()
        .filter_map(|c| {
            c["id"]
                .as_u64()
                .map(|n| n.to_string())
                .or_else(|| c["created_at"].as_str().map(String::from))
        })
        .max()
}

/// Resume a stopped chat the way the user chose: `continue` (retry what was cut off and
/// carry on) or `wrap` (don't continue; summarise where things stand).
pub async fn resume_after_stop(h: &Arc<Harness>, task_id: &str, mode: &str) -> Result<(), String> {
    let wrap = mode == "wrap";
    let extra = if wrap {
        "<system-reminder>The user does NOT want you to continue this work. Don't retry the cancelled calls and don't start anything new (reading files or `git status`/`git diff` to check the state is fine). Reply with a summary for the user: what got done, what was cut off or is left half-finished (and whether anything is now broken), and what would be left to do. Subagents that were stopped stay stopped; mention what they were doing.</system-reminder>"
    } else {
        "<system-reminder>The user wants you to CONTINUE: check the current state, retry whatever was cut off, and carry on with the task.</system-reminder>"
    };
    h.update_task(task_id, |t| {
        t.stop_note = Some(format!("{}{extra}", t.stop_note.take().unwrap_or_default()));
        t.wrap_up = wrap;
    })
    .await;
    resume(h, task_id, None).await
}

/// Unpause one task. `message` (optional) goes to the main agent and every live sub-agent.
/// A task that isn't running (e.g. after the app was closed) is restarted where it stopped.
pub async fn resume(
    h: &Arc<Harness>,
    task_id: &str,
    message: Option<String>,
) -> Result<(), String> {
    let t = h.task(task_id).await?;
    // A timer park gates ordinary resume until its deadline; GitHub parks have
    // no deadline and remain user-resumable. This is runtime-only state.
    let rt = h.runtime(task_id);
    let (parked, deadline) = {
        let wake = rt.wake_state.lock().unwrap();
        (wake.parked, wake.deadline_ms)
    };
    if parked
        && deadline != 0
        && message.is_none()
        && (chrono::Utc::now().timestamp_millis().max(0) as u64) < deadline
    {
        return Err("This chat is waiting on a trigger; it will wake on its own.".into());
    }
    if parked {
        unpark(h, task_id);
    }
    // Resuming is the user picking the chat back up, so the keepalive budget
    // starts over: the cap is there to stop a chat parked overnight from pinging
    // forever, not to leave a chat that was *actually* resumed unable to keep its
    // cache warm for the work it is now doing.
    h.runtime(task_id)
        .keepalive_pings
        .store(0, std::sync::atomic::Ordering::SeqCst);
    // `unpaused` as well as `paused`, or this is a no-op under "Pause all": the
    // chat's own pause goes, the global flag stays, and the chat stays frozen
    // where it was — which is how resuming one chat looked like it had paused
    // everything. The flag is this chat's own opt-out, so setting it here lifts
    // only this one; clearing the global pause is `resume_all`'s job alone.
    h.update_task(task_id, |t| {
        t.paused = None;
        t.unpaused = true;
    })
    .await;
    // One more try at whatever the pause benched, before this chat is woken.
    //
    // This is the "Resume does nothing, press it again" bug. A chat frozen on an
    // out-of-usage chain wakes up and walks its chain again — and the chain is
    // built by skipping whatever is benched, which may be all of it, because
    // nothing re-tests it while the chat is frozen. `router` clears a bench as it
    // re-walks, but that code is behind the pause we just lifted, so it never
    // runs: the chain comes back empty, not one request is even sent, the pause is
    // raised again and the agent is parked once more. Every press repeats it
    // against the same bench, which is why the count felt arbitrary — and a swarm
    // makes it worse, since each parked sub-agent walks the same empty chain and
    // re-raises the pause behind the user.
    //
    // A key that is really out answers 429 and benches itself again, so a resume
    // costs one request and re-parks with a fresh reason: no worse than before,
    // and only ever on the press that had nothing better to try. Accounts keep
    // their quota bench — that one is the user waiting for a window to roll over,
    // not a stale record — and lose only the bench a failed token refresh left,
    // which says nothing about whether the key in hand still works.
    h.keys.clear_benches();
    h.accts.expire_refresh_benches();
    h.pause_bell.notify_waiters();
    h.save_task(task_id).await;
    let rt = h.runtime(task_id);
    let msg = message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    if rt.running.load(Ordering::SeqCst) {
        if let Some(m) = &msg {
            // A Resume message steers, like any other message sent mid-turn. The
            // note to the live sub-agents goes out with it.
            let item = Item::new("user", m.clone(), json!({}));
            steer_now(h, task_id, m).await;
            for s in t.lock().await.subs.iter().filter(|s| s.status == "running") {
                h.note(
                    task_id,
                    &s.id,
                    Kinded::user(&format!(
                        "<system-reminder>Message from the user to all agents:</system-reminder>
{m}"
                    ))
                    .0,
                )
                .await;
            }
            h.upsert_item(task_id, item).await;
        }
        return Ok(());
    }
    // Not running: continue the conversation from where it stopped.
    let has_history = !t.lock().await.messages.is_empty();
    match msg {
        Some(m) => send(h.clone(), task_id.to_string(), m).await,
        None if has_history => {
            {
                let mut t = t.lock().await;
                fix_dangling(&mut t.messages);
                if t.messages.last().is_some_and(|m| m.role == "assistant") {
                    let next = if t.wrap_up {
                        "Don't continue the task: summarise where things stand."
                    } else {
                        "Continue the task from where you stopped."
                    };
                    t.messages.push(Message::user_text(format!(
                        "<system-reminder>{next}</system-reminder>"
                    )));
                }
            }
            spawn_run(h.clone(), task_id.to_string());
            Ok(())
        }
        None => Ok(()),
    }
}

async fn run_main(h: Arc<Harness>, task_id: String) {
    let rt = h.runtime(&task_id);
    if rt.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let cancel = CancellationToken::new();
    *rt.cancel.lock().unwrap() = Some(cancel.clone());
    h.update_task(&task_id, |t| {
        t.status = "running".into();
        t.waiting_kind = None;
        t.step = "Thinking".into();
    })
    .await;

    let (project, cwd) = if let Ok(task) = h.task(&task_id).await {
        let task = task.lock().await;
        (task.project.clone(), task.cwd.clone())
    } else {
        (String::new(), String::new())
    };
    let hooks = {
        let settings = h.settings.read().await;
        checks::hooks_for(&settings, &project)
    };
    let _ = checks::session_start(&hooks, &cwd, &project, &task_id, &cancel).await;
    let agent = Agent {
        h: h.clone(),
        task_id: task_id.clone(),
        sub: None,
        cancel: cancel.clone(),
    };
    let res = agent.run_loop(None).await;

    if let Err(error) = &res {
        let _ = checks::error(&hooks, error, &cwd, &project, &task_id, &cancel).await;
    }
    let summary = res
        .as_ref()
        .map(String::as_str)
        .unwrap_or_else(|e| e.as_str());
    let _ = checks::session_end(&hooks, summary, &cwd, &project, &task_id, &cancel).await;
    rt.running.store(false, Ordering::SeqCst);
    *rt.cancel.lock().unwrap() = None;

    let (status, step) = match &res {
        Ok(_) => ("done", None),
        Err(e) if e == INTERRUPTED => ("stopped", Some("Interrupted".to_string())),
        Err(e) => ("failed", Some(e.clone())),
    };
    // A run that ended by *parking* (`wait_for_event`) is not done. The tool set
    // `waiting` + `waiting_kind = "wake"` and started a watcher; the ending below
    // would relabel the chat "Done" and clear the kind out from under it, which
    // is the park vanishing the instant the turn loop returns.
    let parked = rt.wake_state.lock().unwrap().parked;
    if let Err(e) = &res {
        let text = if e == INTERRUPTED {
            "Interrupted · tell the agent what to do instead".to_string()
        } else {
            e.clone()
        };
        let level = if e == INTERRUPTED { "stopped" } else { "error" };
        h.upsert_item(&task_id, Item::new("notice", text, json!({"level": level})))
            .await;
    }
    h.update_task(&task_id, |t| {
        if parked {
            // Leave `waiting`/`waiting_kind`/`step` exactly as the park set them.
            for s in t.subs.iter_mut().filter(|s| s.status == "running") {
                s.status = "stopped".into();
            }
            return;
        }
        if status == "stopped" {
            // A stop (unlike a pause) cancels work: the agent hears about it on its next request.
            fix_dangling(&mut t.messages);
            t.stop_note = Some(stop_note(&t.messages));
        }
        t.status = status.into();
        t.waiting_kind = None;
        for s in t.subs.iter_mut().filter(|s| s.status == "running") {
            s.status = "stopped".into();
        }
        match step {
            Some(s) => t.step = s.chars().take(200).collect(),
            None => {
                let done = t.todos.iter().filter(|x| x.status == "completed").count();
                t.step = if t.todos.is_empty() {
                    "Done".into()
                } else {
                    format!("Done · {done}/{} tasks", t.todos.len())
                };
            }
        }
    })
    .await;
    // The run is over: flush rather than leave the last turn of a long task
    // sitting in the debounced writer.
    h.save_task_now(&task_id).await;
    let checkpoint_enabled = h.settings.read().await.checkpoints;
    if checkpoint_enabled {
        if let Ok(task) = h.task(&task_id).await {
            let cwd = task.lock().await.cwd.clone();
            let tid = task_id.clone();
            let _ =
                tokio::task::spawn_blocking(move || checkpoint::capture_latest_turn(&tid, &cwd))
                    .await;
        }
    }
    // The agent's browser belongs to this run. A finished run is not coming back
    // to the page it was on, and a headless Chrome left running holds a profile
    // dir and a chunk of memory for nothing.
    super::browser::close_task(&task_id);
    (h.bus)(
        "ol://attention",
        json!({"task_id": task_id, "kind": if parked { "waiting" } else { status }}),
    );

    // A parked run is *waiting*, not finished, so it gets neither the queued
    // turn nor a keepalive: the watcher owns what happens next, and a keepalive
    // would keep paying to warm a prefix the model is not coming back to read
    // until the trigger fires. `park_waiting` cancelled any keepalive already.
    if parked {
        return;
    }
    // A stopped run is the user saying "enough" — so the queue is left alone.
    // Sending it anyway would have the agent carry straight on with the next
    // thing they were told to stop, which is the opposite of what Stop means.
    // The messages stay queued and editable, and go out when the user says so.
    if status == "stopped" {
        return;
    }
    // An ordinary finish: the queue may hold a follow-up turn, which restarts
    // the run below and serves its own idle. Only if there is none is this the
    // end of the run, and so the moment the cache-keepalive loop becomes useful.
    let drained = drain_queue(&rt).await;
    if drained.is_empty() {
        if status == "done" {
            start_keepalive(h.clone(), task_id);
        }
        return;
    }
    {
        let pending: Vec<Value> = drained
            .iter()
            .flat_map(|q| q.blocks.iter().cloned())
            .collect();
        if let Ok(t) = h.task(&task_id).await {
            let mut t = t.lock().await;
            apply_pending(&mut t);
            fix_dangling(&mut t.messages);
            // One user turn, not several in a row.
            push_user_blocks(&mut t.messages, pending);
        }
        mark_delivered(&h, &task_id, &drained).await;
        Box::pin(run_main(h, task_id)).await;
    }
}

/// Take everything the user sent while this run was in flight, oldest first —
/// the list above the composer, in the order the user left it.
pub async fn drain_queue(rt: &Arc<Runtime>) -> Vec<QueuedMsg> {
    let mut all = std::mem::take(&mut *rt.queue.lock().await);
    all.sort_by_key(|q| q.seq);
    all
}

/// The agent has now read these queued messages: drop their "queued" state so
/// they read as ordinary sent messages, editable and rewindable like any other.
/// Named by id rather than swept by flag — a message the user edited, reordered
/// or removed while the run was finishing must not be marked delivered.
pub async fn mark_delivered(h: &Arc<Harness>, task_id: &str, sent: &[QueuedMsg]) {
    for q in sent {
        h.patch_item(task_id, &q.item_id, |i| {
            if let Some(o) = i.data.as_object_mut() {
                o.remove("queued");
                o.remove("seq");
            }
        })
        .await;
    }
}

/// What the agent is told after the user stopped a run: which tool calls in its
/// last turn were cancelled before finishing. Sub-agent calls are left out —
/// `resume_subs` continues those on its own.
pub fn stop_note(msgs: &[Message]) -> String {
    let mut cut: Vec<String> = vec![];
    if let Some(i) = msgs.iter().rposition(|m| m.role == "assistant") {
        let results = msgs
            .get(i + 1)
            .filter(|m| m.role == "user")
            .map(|m| m.content.as_slice())
            .unwrap_or(&[]);
        for b in msgs[i]
            .content
            .iter()
            .filter(|b| b["type"] == "tool_use" && b["name"] != "task")
        {
            let id = b["id"].as_str().unwrap_or("");
            let stopped = results
                .iter()
                .find(|r| r["type"] == "tool_result" && r["tool_use_id"] == id)
                .is_none_or(|r| {
                    let c = tools::tool_text(&r["content"]);
                    r["is_error"] == true
                        && (c.contains("Interrupted by user")
                            || c.starts_with("This tool call was interrupted"))
                });
            if stopped {
                cut.push(describe_call(b));
            }
        }
    }
    let mut s = String::from("<system-reminder>The user STOPPED your previous run (this was a stop, not a pause): everything in flight was cancelled.");
    if cut.is_empty() {
        s.push_str(" No tool calls were cut off; your last response may have been cut short.");
    } else {
        s.push_str(" These tool calls were cancelled before they finished, so their effects may be missing or partial:\n");
        for c in &cut {
            s.push_str(&format!("- {c}\n"));
        }
        s.push_str("Check the current state, then retry any of them that are still needed — unless the user's latest message says otherwise (they may have stopped you on purpose).");
    }
    s.push_str("</system-reminder>");
    s
}

fn describe_call(b: &Value) -> String {
    let name = b["name"].as_str().unwrap_or("?");
    let i = &b["input"];
    let arg = ["command", "path", "url", "query", "pattern", "action"]
        .iter()
        .find_map(|k| i[*k].as_str())
        .unwrap_or("");
    let arg: String = arg.chars().take(160).collect();
    if arg.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {arg}")
    }
}

/// Screenshots are big (computer use sends one per step). Once history holds
/// more than `PRUNE_AT` images from tool results, all but the newest `KEEP_IMAGES`
/// become a text stub. Done in bulk, not every turn, so the prompt cache only
/// resets occasionally. Images the user attached are never touched.
const PRUNE_AT: usize = 6;
const KEEP_IMAGES: usize = 3;

pub fn prune_images(msgs: &mut [Message]) -> bool {
    let mut spots: Vec<(usize, usize, usize)> = vec![];
    for (mi, m) in msgs.iter().enumerate().filter(|(_, m)| m.role == "user") {
        for (bi, b) in m
            .content
            .iter()
            .enumerate()
            .filter(|(_, b)| b["type"] == "tool_result")
        {
            if let Some(a) = b["content"].as_array() {
                for (ci, c) in a.iter().enumerate() {
                    if c["type"] == "image" {
                        spots.push((mi, bi, ci));
                    }
                }
            }
        }
    }
    if spots.len() <= PRUNE_AT {
        return false;
    }
    for &(mi, bi, ci) in &spots[..spots.len() - KEEP_IMAGES] {
        msgs[mi].content[bi]["content"][ci] =
            json!({"type": "text", "text": "[older screenshot removed to save context]"});
    }
    true
}

/// How many timeline items keep their images.
///
/// Separate from `KEEP_IMAGES`, and deliberately much smaller. That one trims
/// what goes to the *model*: it runs once per turn on request-sized data, and
/// is allowed to be generous. This one trims what the *process* keeps.
/// `data.images` on a timeline item is base64 inside a `serde_json::Value` in
/// `Task.items`, held for as long as the chat is open — and, because nothing
/// ever trimmed `items`, for as long as the app runs.
///
/// Nothing reads these back for the model: the pixels a tool returned reach the
/// provider from the tool-result content block in `messages`, which
/// `prune_images` already handles. A timeline item's `data.images` is only what
/// the UI draws as a thumbnail. So the newest few are worth keeping — the user
/// is usually looking at the screenshots from a moment ago — and older ones drop
/// to a count the row can still show. Computer use sends one per step, so an
/// hour of it is thousands of images at up to 12 MB each.
pub const KEEP_ITEM_IMAGES: usize = 3;

/// Blank the images on all but the newest [`KEEP_ITEM_IMAGES`] timeline items
/// that carry any, and report whether anything changed.
///
/// Idempotent, and cheap enough to call on every save: it walks the timeline
/// once, and once the sweep has done its work the items it touched are no
/// longer candidates, so a later call is one O(items) scan that finds nothing.
pub fn prune_item_images(items: &mut [Item]) -> bool {
    let spots: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| has_item_images(i))
        .map(|(n, _)| n)
        .collect();
    if spots.len() <= KEEP_ITEM_IMAGES {
        return false;
    }
    for &n in &spots[..spots.len() - KEEP_ITEM_IMAGES] {
        drop_item_images(&mut items[n]);
    }
    true
}

/// Both shapes the timeline carries images in: a tool result's thumbnails
/// (`data.images` of a screenshot / view_image / computer call) and a user's own
/// pasted picture.
fn has_item_images(i: &Item) -> bool {
    i.data
        .get("images")
        .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
}

/// Replace an item's images with a count of what went, so a row can still say
/// it had screenshots rather than looking like it never took one.
fn drop_item_images(i: &mut Item) {
    let Some(prev) = i.data.get("images").and_then(|v| v.as_array()) else {
        return;
    };
    let n = prev.len();
    i.data["images"] = json!([]);
    i.data["images_dropped"] = json!(n);
}

/// Sweep the main transcript and every sub-agent's own.
///
/// Done here rather than in the turn loop because this is bookkeeping about
/// what the process holds, and the saver already rewrites the whole task: it
/// belongs with that write, not on the model's critical path where it would
/// walk the timeline on every request. Dropping from the struct also drops it
/// from the file on the next save, which is the point — a chat reopened
/// tomorrow should not parse a gigabyte of screenshots back in.
pub fn prune_task_images(t: &mut Task) -> bool {
    let mut changed = prune_item_images(&mut t.items);
    for list in t.sub_items.values_mut() {
        changed |= prune_item_images(list);
    }
    changed
}

/// Every tool_use needs a tool_result right after it. Repairs histories cut
/// short by a crash, app close, or interrupted sub-agents.
pub fn fix_dangling(msgs: &mut Vec<Message>) {
    fix_dangling_with(
        msgs,
        "This tool call was interrupted before it finished (the run was stopped).",
    );
}

/// Crash / app-close flavour: the command may have half-run, so say to rerun it.
pub const CRASHED: &str = "This tool call was interrupted before it finished: the app closed or crashed while it was running, so it did NOT complete. Run it again.";

pub fn fix_dangling_with(msgs: &mut Vec<Message>, text: &str) {
    let mut i = 0;
    while i < msgs.len() {
        if msgs[i].role == "assistant" {
            let ids: Vec<String> = msgs[i]
                .content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| b["id"].as_str().map(String::from))
                .collect();
            if !ids.is_empty() {
                let has_next_user = msgs.get(i + 1).is_some_and(|m| m.role == "user");
                if !has_next_user {
                    msgs.insert(i + 1, Message::user(vec![]));
                }
                let next = &mut msgs[i + 1];
                let missing: Vec<Value> = ids
                    .iter()
                    .filter(|id| !next.content.iter().any(|b| b["type"] == "tool_result" && b["tool_use_id"].as_str() == Some(id)))
                    .map(|id| json!({"type": "tool_result", "tool_use_id": id, "content": text, "is_error": true}))
                    .collect();
                if !missing.is_empty() {
                    let rest = std::mem::take(&mut next.content);
                    next.content = missing.into_iter().chain(rest).collect();
                }
            }
        }
        i += 1;
    }
}

/// Extra prompt sections for the features that change how an agent works.
///
/// Read once, when the frozen prefix is built, so the tools and the rules that
/// govern them land in the same cached block. The deep-research tools are part of
/// the kit now and carry no switch, so their section is unconditional; only
/// memory is a choice, and sub-agents never get it — memory is the
/// conversation's own knowledge, and a fan-out of sub-agents each writing to the
/// same index would fight over it.
fn feature_sections(settings: &store::Settings) -> String {
    let mut s = String::new();
    if settings.memory {
        s.push_str(prompt::memory_section());
    }
    s.push_str(prompt::research_section());
    // Plugin-built, like the tool list it has to agree with: `screen_section`
    // names only the plugins this task actually has, so a disabled one leaves
    // no trace in the prompt.
    s.push_str(&prompt::screen_section(&settings.plugins));
    s
}

/// The plugin config a task's tool list should be built from.
///
/// Prefers the snapshot frozen with the prefix, because that is the config the
/// agent's system prompt was written under — serving live settings here could
/// pair a tool list with a prompt describing different switches. A task whose
/// prefix is not built yet has nothing to be consistent with, so it reads live
/// settings; otherwise the very first sub-agent of a cold task would find
/// `screenshot` missing and think the user had turned computer use off.
async fn prefix_plugins(
    h: &Arc<Harness>,
    t: &tokio::sync::MutexGuard<'_, Task>,
) -> super::plugins::PluginsCfg {
    if t.system.is_empty() {
        return h.settings.read().await.plugins.clone();
    }
    t.plugins.clone()
}

/// Main-agent system prompt + MCP tools, frozen on first use so every request
/// shares a cached prefix.
///
/// Returns the plugin snapshot the prefix was built from alongside it. Callers
/// need it for `tools::schemas`: the frozen `screenshot` tool follows the
/// computer plugin, so serving a live settings read here could pair a tool list
/// with a prompt that was written under different switches.
async fn frozen_prefix(
    h: &Arc<Harness>,
    task_id: &str,
) -> Result<(String, Vec<Value>, super::plugins::PluginsCfg), String> {
    let t = h.task(task_id).await?;
    let (sys, mcp, cwd, project, branch, worktree, plugins) = {
        let t = t.lock().await;
        (
            t.system.clone(),
            t.mcp_tools.clone(),
            t.cwd.clone(),
            t.project.clone(),
            t.branch.clone(),
            t.worktree,
            t.plugins.clone(),
        )
    };
    if !sys.is_empty() {
        return Ok((sys, mcp, plugins));
    }
    let agent_id = format!("ol-{task_id}");
    let (skills, inject_global_claude, extra, plugins) = {
        let s = h.settings.read().await;
        let out = (
            store::all_skills(&s, &project),
            s.inject_global_claude,
            feature_sections(&s),
            s.plugins.clone(),
        );
        drop(s);
        out
    };
    let system = prompt::system(
        &prompt::Env {
            cwd: &cwd,
            project: &project,
            branch: &branch,
            is_git: git::is_repo(&cwd),
            worktree,
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            agent_id: &agent_id,
            inject_global_claude,
        },
        None,
        &skills,
    ) + &extra;
    let mut mcp = h.mcp.tool_schemas().await;
    mcp.extend(super::plugins::schemas(&plugins));
    let settings = h.settings.read().await.clone();
    let core = tools::schemas(None, &plugins);
    let off: Vec<String> = settings
        .mcp
        .iter()
        .filter(|m| !m.defer)
        .map(|m| m.name.clone())
        .collect();
    let deferral = toolindex::Deferral::new(
        core.len(),
        &mcp,
        &off,
        settings.defer_plugins,
        settings.tool_search,
    );
    if deferral.on() {
        mcp.push(tools::tool_search_schema(
            &deferral.catalog.deferred_plugins(),
        ));
    }
    let mut t = t.lock().await;
    t.system = system.clone();
    t.mcp_tools = mcp.clone();
    // Snapshot what the prompt above was built from, so the tool list served
    // with it stays consistent for the life of the prefix.
    t.plugins = plugins.clone();
    Ok((system, mcp, plugins))
}

/// The frozen system prompt and tool list for a task, without touching it.
///
/// `frozen_prefix` writes the pair back onto the task it reads, which is right for
/// a run about to use them and wrong for anything that only wants to *ask* a
/// question against them: a fork summarises a chat it is not allowed to alter.
/// So this reads the cache when it is warm and rebuilds it in memory when it is
/// not, leaving the task exactly as it found it.
async fn prefix_for(
    h: &Arc<Harness>,
    t: &Task,
) -> (String, Vec<Value>, super::plugins::PluginsCfg) {
    if !t.system.is_empty() {
        return (t.system.clone(), t.mcp_tools.clone(), t.plugins.clone());
    }
    let (skills, inject_global_claude, extra, plugins) = {
        let s = h.settings.read().await;
        let out = (
            store::all_skills(&s, &t.project),
            s.inject_global_claude,
            feature_sections(&s),
            s.plugins.clone(),
        );
        drop(s);
        out
    };
    let system = prompt::system(
        &prompt::Env {
            cwd: &t.cwd,
            project: &t.project,
            branch: &t.branch,
            is_git: git::is_repo(&t.cwd),
            worktree: t.worktree,
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            agent_id: &format!("ol-{}", t.id),
            inject_global_claude,
        },
        None,
        &skills,
    ) + &extra;
    let mut mcp = h.mcp.tool_schemas().await;
    mcp.extend(super::plugins::schemas(&plugins));
    let settings = h.settings.read().await.clone();
    let core = tools::schemas(None, &plugins);
    let off: Vec<String> = settings
        .mcp
        .iter()
        .filter(|m| !m.defer)
        .map(|m| m.name.clone())
        .collect();
    let deferral = toolindex::Deferral::new(
        core.len(),
        &mcp,
        &off,
        settings.defer_plugins,
        settings.tool_search,
    );
    if deferral.on() {
        mcp.push(tools::tool_search_schema(
            &deferral.catalog.deferred_plugins(),
        ));
    }
    (system, mcp, plugins)
}

/// Whether the agent may rename this chat to `want`, and why not if it may not.
///
/// The user's two rules live here, not in the prompt: naming is on unless the
/// user turned it off, and a chat the user named themselves is off limits. A
/// module-level function so the lock is testable without a model in the loop.
pub async fn rename_check(h: &Arc<Harness>, task_id: &str, want: &str) -> Result<(), String> {
    if !h.settings.read().await.agent_titles {
        return Err("Chat naming is turned off in Settings → Chats.".into());
    }
    let (current, locked) = {
        let t = h.task(task_id).await?;
        let g = t.lock().await;
        (g.title.clone(), g.titled)
    };
    if locked {
        return Err("The user named this chat themselves, so leave the title alone.".into());
    }
    if want == current {
        return Err(format!("The title is already \"{current}\" and still describes the work. Only change it if the task has moved on."));
    }
    Ok(())
}

/// Summarise a conversation without changing it: the handoff text a compacted
/// fork starts from, produced from a snapshot the caller already holds.
///
/// `compact` cannot be used for this — it rewrites the task it is handed, so
/// asking it for a fork's summary would compact the *original* chat out from
/// under the user. It also leaves a "Compacting conversation…" item in the
/// timeline, which would be a lie in a chat that is being copied, not compacted.
pub async fn summarise(
    h: &Arc<Harness>,
    t: &Task,
    cancel: &CancellationToken,
) -> Result<String, String> {
    if t.messages.is_empty() {
        return Ok(String::new());
    }
    let mut messages = t.messages.clone();
    let (system, mcp, plugins) = prefix_for(h, t).await;
    let tools_v = main_tool_list(h, &t.id, &plugins, mcp).await;
    let mut ask = prompt::compact_prompt().to_string();
    ask.push_str("\n\nRespond with text only; do not call any tools.");
    fix_dangling(&mut messages);
    match messages.last_mut() {
        Some(m) if m.role == "user" => m.content.push(json!({"type": "text", "text": ask})),
        _ => messages.push(Message::user_text(ask)),
    }
    let req = ChatRequest {
        system,
        messages,
        tools: tools_v,
        effort: t.effort.max(2),
        max_tokens: 16_000.min(router::max_output(&t.model, &t.route)),
        cache_key: format!("ol-{}", t.id),
    };
    match router::oneshot(
        h,
        Who {
            task_id: &t.id,
            sub: None,
            side: false,
            turn: false,
            usage_id: "summary",
        },
        &t.model,
        &req,
        cancel,
    )
    .await
    {
        Ok((summary, usage, target)) => {
            // The call is real, so it is billed like any other turn.
            let cost = usage.cost(&providers::model_info(&target.model_id));
            record_spend(h, cost, usage.input + usage.output).await;
            Ok(summary)
        }
        Err(RouteErr::Cancelled) => Err(INTERRUPTED.into()),
        Err(RouteErr::Refresh) => {
            Err("The conversation changed while it was being summarised.".into())
        }
        Err(RouteErr::TooLong(e)) | Err(RouteErr::Fatal(e)) => Err(e),
    }
}

/// Summarise the history and restart from the summary. Used automatically
/// near the context limit and by `/compact`. The summary request reuses the
/// task's cached prefix (same system + tools + history), so it's cheap.
/// Returns the summary, which is what a compacted fork starts from.
pub async fn compact(
    h: &Arc<Harness>,
    task_id: &str,
    cancel: &CancellationToken,
    instructions: Option<&str>,
) -> Result<String, String> {
    let t = h.task(task_id).await?;
    let (model, mut messages, effort, route_id) = {
        let t = t.lock().await;
        (
            t.model.clone(),
            t.messages.clone(),
            t.effort,
            t.route.clone(),
        )
    };
    if messages.is_empty() {
        return Ok(String::new());
    }
    let notice = Item::new(
        "notice",
        "Compacting conversation…",
        json!({"level": "info", "working": true}),
    );
    let nid = notice.id.clone();
    h.upsert_item(task_id, notice).await;
    let (system, mcp, plugins) = frozen_prefix(h, task_id).await?;
    let tools_v = main_tool_list(h, task_id, &plugins, mcp).await;
    let mut ask = prompt::compact_prompt().to_string();
    if let Some(i) = instructions.filter(|s| !s.trim().is_empty()) {
        ask.push_str(&format!("\n\nThe user asked you to focus on: {i}"));
    }
    ask.push_str("\n\nRespond with text only; do not call any tools.");
    fix_dangling(&mut messages);
    match messages.last_mut() {
        Some(m) if m.role == "user" => m.content.push(json!({"type": "text", "text": ask})),
        _ => messages.push(Message::user_text(ask)),
    }
    let req = ChatRequest {
        system,
        messages,
        tools: tools_v,
        effort: effort.max(2),
        max_tokens: 16_000.min(router::max_output(&model, &route_id)),
        cache_key: format!("ol-{task_id}"),
    };
    let res = router::oneshot(
        h,
        Who {
            task_id,
            sub: None,
            side: false,
            turn: false,
            usage_id: "compactor",
        },
        &model,
        &req,
        cancel,
    )
    .await;
    let (summary, usage, target) = match res {
        Ok(x) => x,
        Err(RouteErr::Cancelled) => return Err(INTERRUPTED.into()),
        Err(RouteErr::Refresh) => {
            // The user messaged this chat while it was frozen. The history above
            // predates that text, and compacting it would throw the message away
            // with everything else — so bail and let the run rebuild from history
            // that has it. The summary is only wanted if there's room to keep it.
            return Err(
                "The conversation changed while it was being compacted. Send /compact again."
                    .into(),
            );
        }
        Err(RouteErr::TooLong(e)) | Err(RouteErr::Fatal(e)) => {
            h.patch_item(task_id, &nid, |i| {
                i.text = format!("Compaction failed: {e}");
                i.data = json!({"level": "error"});
            })
            .await;
            return Err(e);
        }
    };
    let cost = usage.cost(&providers::model_info(&target.model_id));
    record_spend(h, cost, usage.input + usage.output).await;
    {
        let mut t = t.lock().await;
        let todos = if t.todos.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nCurrent todo list:\n{}",
                serde_json::to_string_pretty(&t.todos).unwrap_or_default()
            )
        };
        let goal = t
            .goal
            .as_ref()
            .filter(|g| g.status == "active")
            .map(|g| format!("\n\n{}", prompt::goal_intro(&g.text)))
            .unwrap_or_default();
        t.messages = vec![Message::user_text(format!(
            "This session continues from an earlier conversation that was compacted to free up context. Summary of everything so far:\n\n{summary}{todos}{goal}\n\nContinue the work from where it left off without asking the user to repeat anything."
        ))];
        t.checkpoints.clear();
        t.read_files.clear();
        t.usage.cost += cost;
        t.usage.last_context = usage.output + 2_000;
        // Fresh prefix: re-read project instructions + MCP tools, and re-announce modes.
        t.system.clear();
        t.mcp_tools.clear();
        t.plugins = Default::default();
        t.told.clear();
    }
    h.patch_item(task_id, &nid, |i| {
        h.stats.event_in(task_id, "compactions");
        i.text = "Conversation compacted · earlier turns summarised to free context".into();
        i.data = json!({"level": "info", "summary": summary});
    })
    .await;
    h.update_task(task_id, |_| {}).await;
    Ok(summary)
}

async fn record_spend(h: &Harness, cost: f64, tokens: u64) {
    let month = {
        let mut s = h.settings.write().await;
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        *s.spend.entry(day).or_insert(0.0) += cost;
        s.tokens_month += tokens;
        s.month_spend()
    };
    // Coalesced rather than written here: this ran once per model turn, and
    // it rewrote every account token in `settings.json` — with two fsyncs, on
    // the async runtime, behind the lock every other agent queues on — to record
    // a figure that moves by cents. The number on screen is read from memory
    // below, so it is exact either way.
    h.save_settings_debounced().await;
    let tokens = h.settings.read().await.tokens_month;
    (h.bus)("ol://usage", json!({"month": month, "tokens": tokens}));
}

// ───────────────────── cache keepalive ─────────────────────

/// Start the cache-keepalive loop for a run that just went idle.
///
/// Aider's `--cache-keepalive-pings`: a long task is full of pauses (waiting on
/// a review, on a build, on the user), and a provider evicts its cached prefix
/// after a few minutes of inactivity, so the turn after every pause re-reads the
/// whole conversation at full price. One tiny request every five minutes keeps
/// the prefix warm so that turn is mostly a cache *read*.
///
/// Three things bound it, all deliberately:
///   * off unless the user turned it on (`cache_keepalive`) — every ping is a
///     real, billable request, so this is never the default;
///   * at most `cache_keepalive_pings` pings per task (reset when the user
///     resumes), so a chat parked overnight cannot spend more than it would have;
///   * cancelled by `stop_tokens` and by `unpark`, so a pause, a stop, an
///     interrupt or the model parking the task all end it.
///
/// Called on `Idle` from `run_main`, which is the one place a run is known to be
/// over — the loop is what serves the *next* pause, so starting it from the
/// waiting banners (which are also physical pauses) is the point.
fn start_keepalive(h: Arc<Harness>, task_id: String) {
    let rt = h.runtime(&task_id);
    // Supersede any previous loop — one run's keepalive must not stack on the
    // last run's if both somehow lived to see a second idle.
    if let Some(c) = rt.keepalive_cancel.lock().unwrap().take() {
        c.cancel();
    }
    let cancel = CancellationToken::new();
    *rt.keepalive_cancel.lock().unwrap() = Some(cancel.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            // Sleep a full interval first: a ping at t=0 would be one more
            // request on a task that just made one, which is the opposite of the
            // point. The first real gap is the first thing worth keeping warm.
            if tokio::time::timeout(keepalive_interval(), cancel.cancelled())
                .await
                .is_ok()
            {
                return; // cancelled
            }
            let settings = h.settings.read().await.clone();
            let rt2 = h.runtime(&task_id);
            let running = rt2.running.load(Ordering::SeqCst);
            let paused = h.is_paused(&task_id).await;
            match keepalive_tick(
                settings.cache_keepalive,
                settings.cache_keepalive_pings,
                rt2.keepalive_pings.load(Ordering::SeqCst),
                paused,
                settings.paused_all,
                running,
            ) {
                KeepaliveTick::Wait => continue,
                KeepaliveTick::Stop => return,
                KeepaliveTick::Ping => {}
            }
            let has_history = match h.task(&task_id).await {
                Ok(t) => !t.lock().await.messages.is_empty(),
                Err(_) => false,
            };
            if !has_history {
                return;
            }
            rt.keepalive_pings.fetch_add(1, Ordering::SeqCst);
            match keepalive_ping(&h, &task_id, &cancel).await {
                KeepaliveOutcome::Sent => {}
                // Paused mid-ping, or the ping itself failed: not counted as a
                // ping that was *sent*, so the cap is on real spend.
                KeepaliveOutcome::Skipped => {
                    rt.keepalive_pings.fetch_sub(1, Ordering::SeqCst);
                }
                KeepaliveOutcome::Stop => return,
            }
        }
    });
}

/// What the keepalive loop should do when its interval elapses.
///
/// Pulled out as a pure function on purpose: the three properties that matter —
/// off by default, capped per task, and not pinging while the chat is actually
/// working — are a decision about a `(settings, counters)` tuple, and a decision
/// is worth testing without sleeping for five real minutes to observe it. The
/// loop is then only "sleep, ask, act".
#[derive(Debug, PartialEq)]
pub(crate) enum KeepaliveTick {
    /// Send a ping.
    Ping,
    /// Not now, but keep the loop alive: a run is in flight, or the app is
    /// globally paused.
    Wait,
    /// Nothing more this loop can usefully do — switched off, or out of budget.
    Stop,
}

pub(crate) fn keepalive_tick(
    enabled: bool,
    cap: u32,
    used: u32,
    paused: bool,
    paused_all: bool,
    running: bool,
) -> KeepaliveTick {
    if !enabled {
        return KeepaliveTick::Stop;
    }
    if cap == 0 || used >= cap {
        return KeepaliveTick::Stop;
    }
    // Stopped by a pause, exactly as the brief asks: a manual (or auto-raised)
    // per-chat pause, and the global "Pause all", both end the loop. A pause is
    // the user saying *stop spending*, and the keepalive exists to spend on
    // *idle* time — an idle chat between turns, or one waiting on the user's
    // answer — not to spend through a pause they asked for. `pause_task` cancels
    // the token to match; this branch is the belt to that braces, and the one the
    // test can drive without a live loop.
    if paused || paused_all {
        return KeepaliveTick::Stop;
    }
    // A turn is in flight, so the next request will shape the prefix rather than
    // hit it: not worth paying for, but the loop stays alive for the pause after.
    if running {
        return KeepaliveTick::Wait;
    }
    KeepaliveTick::Ping
}

#[derive(Debug, PartialEq)]
pub(crate) enum KeepaliveOutcome {
    Sent,
    Skipped,
    Stop,
}

/// One keepalive ping: re-send the frozen prefix and stop the model immediately.
///
/// Deliberately `router::oneshot` with `max_tokens: 1` and the tools stripped, so
/// the model is not asked to *say* anything — the request exists only to touch
/// the cache. `usage_id` is `"keepalive"`, which is what keeps this spend out of
/// the main agent's row in the stats panel: the entire reason to attribute by
/// agent is to see what the machinery (not the work) cost.
pub(crate) async fn keepalive_ping(
    h: &Arc<Harness>,
    task_id: &str,
    cancel: &CancellationToken,
) -> KeepaliveOutcome {
    let (messages, model, _route_id) = {
        let Ok(t) = h.task(task_id).await else {
            return KeepaliveOutcome::Stop;
        };
        let g = t.lock().await;
        (g.messages.clone(), g.model.clone(), g.route.clone())
    };
    if messages.is_empty() {
        return KeepaliveOutcome::Stop;
    }
    // The frozen prefix, and the *same* cache_key the real turns use, or the
    // ping warms a different cache entry than the one the next turn reads.
    let Ok((system, mcp, plugins)) = frozen_prefix(h, task_id).await else {
        return KeepaliveOutcome::Stop;
    };
    let tools_v = main_tool_list(h, task_id, &plugins, mcp).await;
    let mut messages = messages;
    // History with a dangling tool_use would be a malformed request; the same
    // repair the loop makes, and nothing else — a keepalive must not edit the
    // real history (it sends a clone).
    fix_dangling(&mut messages);
    let req = ChatRequest {
        system,
        messages,
        tools: tools_v,
        effort: 4,
        max_tokens: 1,
        cache_key: format!("ol-{task_id}"),
    };
    let res = router::oneshot(
        h,
        Who {
            task_id,
            sub: None,
            side: true,
            turn: false,
            usage_id: "keepalive",
        },
        &model,
        &req,
        cancel,
    )
    .await;
    match res {
        Ok((_text, usage, target)) => {
            let cost = usage.cost(&providers::model_info(&target.model_id));
            // `cache_read` is the whole point of this request — a warm prefix is
            // a read — so it is counted here with the other token columns. The
            // chat's `last_context` is deliberately *not* touched: a keepalive is
            // not a turn, and letting its size drive the compaction meter would
            // compact a chat because a ping went out.
            record_spend(
                h,
                cost,
                usage.input + usage.output + usage.cache_read + usage.cache_write,
            )
            .await;
            h.update_task(task_id, |t| {
                t.usage.input += usage.input;
                t.usage.output += usage.output;
                t.usage.cache_read += usage.cache_read;
                t.usage.cache_write += usage.cache_write;
                t.usage.cost += cost;
            })
            .await;
            KeepaliveOutcome::Sent
        }
        // A pause mid-ping, or a route that could not serve one request: no
        // spend happened, so do not count it against the cap.
        Err(RouteErr::Cancelled) | Err(RouteErr::Refresh) => KeepaliveOutcome::Skipped,
        Err(RouteErr::Fatal(_)) | Err(RouteErr::TooLong(_)) => KeepaliveOutcome::Skipped,
    }
}

/// A stand-in definition for an agent whose own entry has disappeared (deleted from
/// settings, or a role that never existed). `builtin_agents()` is a hard-coded vec,
/// but the release profile is `panic = "abort"`, so a future edit that empties it
/// must not take the app down: fall back to a neutral, fully-permissioned def so the
/// work still finishes. The id/name are overwritten by the caller where it knows the
/// role it is standing in for.
fn fallback_def(role: &str) -> AgentDef {
    let mut d = store::builtin_agents().pop().unwrap_or_default();
    d.id = role.to_string();
    d.name = role.to_string();
    d
}

/// Sub-agent definitions this task may use right now.
async fn allowed_agents(h: &Harness, t: &Task) -> Vec<AgentDef> {
    let s = h.settings.read().await;
    let mut v: Vec<AgentDef> = store::all_agents(&s, &t.project)
        .into_iter()
        .filter(|d| t.agents.contains(&d.id))
        .collect();
    // fuze only makes sense when workers own branches: either it's on, or a
    // worker already has a worktree whose branch still needs merging.
    if (t.ultra && (t.ultra_wt || task_x(t).is_some_and(|x| x.wt)))
        || t.subs.iter().any(|s| !s.cwd.is_empty())
    {
        v.push(store::fuze_agent());
    }
    v
}

// ───────────────────────────── the loop ─────────────────────────────

impl Agent {
    fn is_main(&self) -> bool {
        self.sub.is_none()
    }

    /// Depth of this agent in the ultrathread tree (main = 0).
    async fn depth(&self) -> u8 {
        let Some(s) = &self.sub else { return 0 };
        let Ok(t) = self.h.task(&self.task_id).await else {
            return 1;
        };
        let d = t
            .lock()
            .await
            .subs
            .iter()
            .find(|x| x.id == s.id)
            .map(|x| x.depth)
            .unwrap_or(1);
        d.max(1)
    }

    /// Ultrathread lets sub-agents launch their own sub-agents, down to the
    /// tree's depth cap: ULTRA_MAX_DEPTH for plain ultrathread, or the ladder's
    /// own height in ULTRATHREAD X.
    async fn can_nest(&self) -> bool {
        let Ok(t) = self.h.task(&self.task_id).await else {
            return false;
        };
        let cap = {
            let g = t.lock().await;
            if !g.ultra {
                return false;
            }
            ultra_max_depth(task_x(&g))
        };
        self.depth().await < cap
    }

    fn sub_id(&self) -> Option<&str> {
        self.sub.as_ref().map(|s| s.id.as_str())
    }

    fn inbox_key(&self) -> String {
        self.sub_id().unwrap_or("main").to_string()
    }

    /// The id this agent's own spend is attributed to in the per-chat by-agent
    /// split. `sub:<id>` and not the agent's *role* (`explore`, `general`, …):
    /// that role is what the app-wide `agents` dimension already keys on, and
    /// two explorers in one tree are two different spends.
    fn usage_id(&self) -> String {
        match self.sub_id() {
            Some(s) => format!("sub:{s}"),
            None => "main".to_string(),
        }
    }

    /// Save a sub-agent's conversation so it can be continued after a stop, a pause or an app restart.
    async fn save_sub_history(&self, local: &[Message]) {
        if let Some(s) = &self.sub {
            if let Ok(t) = self.h.task(&self.task_id).await {
                t.lock().await.sub_msgs.insert(s.id.clone(), local.to_vec());
            }
        }
    }

    /// On (re)start of the main agent: sub-agents that were cut off (Esc, app
    /// closed, crash) continue from their saved conversation, and their
    /// reports replace the "stopped" results the main agent hasn't seen yet.
    async fn resume_subs(&self) {
        let Ok(t) = self.h.task(&self.task_id).await else {
            return;
        };
        // "Wrap up" after a stop: cut-off subagents stay stopped; the main agent summarises.
        if std::mem::take(&mut t.lock().await.wrap_up) {
            return;
        }
        let (msgs, subs, saved, project) = {
            let t = t.lock().await;
            (
                t.messages.clone(),
                t.subs.clone(),
                t.sub_msgs.clone(),
                t.project.clone(),
            )
        };
        let Some(i) = msgs.iter().rposition(|m| m.role == "assistant") else {
            return;
        };
        let next = msgs.get(i + 1).filter(|m| m.role == "user");
        let cut_off = |id: &str| match next.and_then(|m| {
            m.content
                .iter()
                .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
        }) {
            None => true,
            Some(r) => {
                let c = tools::tool_text(&r["content"]);
                r["is_error"] == true
                    && (c.starts_with("Sub-agent stopped")
                        || c.starts_with("Interrupted")
                        || c.starts_with("This tool call was interrupted"))
            }
        };
        // Background sub-agents that were cut off go back to the background.
        let defs0 = store::all_agents(&*self.h.settings.read().await, &project);
        for s in subs
            .iter()
            .filter(|s| s.background && s.status == "stopped" && saved.contains_key(&s.id))
        {
            let def = defs0
                .iter()
                .find(|d| d.id == s.role)
                .cloned()
                .unwrap_or_else(|| fallback_def(&s.role));
            let mut init = saved.get(&s.id).cloned().unwrap_or_default();
            push_user_blocks(
                &mut init,
                vec![
                    json!({"type": "text", "text": "<system-reminder>You were interrupted. Continue the job exactly where you left off.</system-reminder>"}),
                ],
            );
            self.launch_background(
                s.id.clone(),
                def,
                s.item_id.clone(),
                init,
                Some(s.parent.clone()).filter(|p| !p.is_empty()),
            );
        }
        let todo: Vec<(String, SubInfo)> = msgs[i]
            .content
            .iter()
            .filter(|b| b["type"] == "tool_use" && b["name"] == "task")
            .filter_map(|b| b["id"].as_str().map(String::from))
            .filter(|id| cut_off(id))
            .filter_map(|id| {
                subs.iter()
                    .find(|s| s.call_id == id && s.status != "done" && saved.contains_key(&s.id))
                    .cloned()
                    .map(|s| (id, s))
            })
            .collect();
        if todo.is_empty() {
            return;
        }
        let defs = store::all_agents(&*self.h.settings.read().await, &project);
        let runs = todo.iter().map(|(call_id, s)| {
            let def = defs.iter().find(|d| d.id == s.role).cloned().unwrap_or_else(|| fallback_def(&s.role));
            let mut init = saved.get(&s.id).cloned().unwrap_or_default();
            push_user_blocks(&mut init, vec![json!({"type": "text", "text": "<system-reminder>You were interrupted. Continue the job exactly where you left off.</system-reminder>"})]);
            let (sid, iid, par) = (s.id.clone(), s.item_id.clone(), s.parent.clone());
            async move { (call_id.clone(), self.drive_sub(sid, def, iid, init, (!par.is_empty()).then_some(par), self.cancel.child_token()).await) }
        });
        self.h.upsert_item(&self.task_id, Item::new("notice", format!("Resuming {} subagent{}", todo.len(), if todo.len() == 1 { "" } else { "s" }), json!({"level": "event", "sum": format!("resumed {} subagent{}", todo.len(), if todo.len() == 1 { "" } else { "s" })}))).await;
        let results = join_all(runs).await;
        let mut t = t.lock().await;
        if t.messages.get(i + 1).is_none_or(|m| m.role != "user") {
            t.messages.insert(i + 1, Message::user(vec![]));
        }
        let slot = &mut t.messages[i + 1].content;
        for (call_id, res) in results {
            let mut r = json!({"type": "tool_result", "tool_use_id": call_id, "content": match &res { Ok(s) => s.clone(), Err(e) => e.clone() }});
            if res.is_err() {
                r["is_error"] = json!(true);
            }
            match slot
                .iter_mut()
                .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == call_id.as_str())
            {
                Some(b) => *b = r,
                None => slot.insert(0, r),
            }
        }
    }

    fn run_loop(&self, sub_init: Option<Vec<Message>>) -> BoxFut<'_, Result<String, String>> {
        Box::pin(async move {
            let h = &self.h;
            let t = h.task(&self.task_id).await?;
            let mut local: Vec<Message> = sub_init.unwrap_or_default();
            if self.is_main() {
                self.resume_subs().await;
            }
            let mut seen_plan: Option<bool> = None;
            let mut seen_ultra: Option<bool> = None;
            let mut seen_wt: Option<bool> = None;
            let mut ultra_nudges = 0u32;
            let mut plan_nudges = 0u32;
            let mut step_limited = false;
            let mut step_nudges = 0u32;

            // Sub-agents get their own (stable) prefix per run.
            let sub_prefix = match &self.sub {
                Some(s) => {
                    let (cwd, project, branch, worktree) = {
                        let t = t.lock().await;
                        let (cwd, branch) = t.cwd_for(Some(&s.id));
                        (
                            cwd,
                            t.project.clone(),
                            branch,
                            t.worktree || t.subs.iter().any(|x| x.id == s.id && !x.cwd.is_empty()),
                        )
                    };
                    let agent_id = format!("ol-{}/{}", self.task_id, s.id);
                    // The same snapshot the main agent's frozen prefix used, so a
                    // sub-agent's `screenshot` presence matches the parent's.
                    let tg = t.lock().await;
                    let plugins = prefix_plugins(h, &tg).await;
                    drop(tg);
                    let (skills, inject_global_claude) = {
                        let st = h.settings.read().await;
                        let out = (store::all_skills(&st, &project), st.inject_global_claude);
                        drop(st);
                        out
                    };
                    let sys = prompt::system(
                        &prompt::Env {
                            cwd: &cwd,
                            project: &project,
                            branch: &branch,
                            is_git: git::is_repo(&cwd),
                            worktree,
                            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
                            agent_id: &agent_id,
                            inject_global_claude,
                        },
                        Some(&s.def),
                        &skills,
                    );
                    let mut tl = tools::schemas(Some(&s.def.tools), &plugins);
                    let full_mcp = t.lock().await.mcp_tools.clone();
                    tl.extend(full_mcp);
                    if self.can_nest().await {
                        tl.extend(tools::schemas(None, &plugins).into_iter().filter(|x| {
                            x["name"] == "task"
                                || x["name"] == "task_status"
                                || x["name"] == "task_resume"
                        }));
                    }
                    Some((sys, tl))
                }
                None => None,
            };
            let mut budget_ok = false;
            let mut tool_uses_total = 0usize;
            let mut last_text = String::new();
            let mut guard = checks::LoopGuard::default();
            let mut sub_ctx: u64 = 0;
            let (mut edited_any, mut verified, mut nudged_verify, mut stop_hook_rounds) =
                (false, false, false, 0u32);

            loop {
                if self.cancel.is_cancelled() {
                    return Err(INTERRUPTED.into());
                }
                // A turn that parked (`wait_for_event`) ends right here, before
                // the next request is built: the trigger is armed and a watcher
                // owns the wait, so going round again would send one more request
                // — and one more turn the user has to read — for a task the model
                // just said it was done with. Only the main agent parks, so only
                // its loop checks.
                if self.is_main() && h.runtime(&self.task_id).wake_state.lock().unwrap().parked {
                    return Ok(last_text);
                }

                if let Some(s) = &self.sub {
                    if s.def.steps > 0 && tool_uses_total >= s.def.steps && step_nudges == 0 {
                        step_nudges = 1;
                        let note = format!("<system-reminder>You have reached the {}-step ceiling for the `{}` agent. Do not start more work. Summarise what you did, what you found, and the remaining steps for the parent agent.</system-reminder>", s.def.steps, s.def.name);
                        local.push(Message::user_text(note));
                        self.step("Summarising at the agent step limit".into())
                            .await;
                        continue;
                    }
                }
                // ── current settings (can change mid-run) ──
                let (plan, effort, model, route_id) = {
                    let t = t.lock().await;
                    let me = self
                        .sub
                        .as_ref()
                        .and_then(|s| t.subs.iter().find(|x| x.id == s.id));
                    // Resolved, not stored: a sub-agent with no model of its own
                    // follows the chat, so swapping the chat reaches the agents
                    // already running as well as the next ones spawned. The depth
                    // comes off the SubInfo we already hold — the task is locked,
                    // so it cannot go back to the task to ask.
                    let model = resolve_model(&t, me, me.map(|x| x.depth).unwrap_or(0), "");
                    // Effort too: its own setting, else its type's, else the chat's
                    // (read-only explorers at most Medium). Shared with the swap dialog
                    // so a level it lists is the level the request actually carries.
                    let effort = super::resolve_effort(
                        &t,
                        me,
                        self.sub.as_ref().and_then(|s| s.def.effort),
                        self.sub
                            .as_ref()
                            .is_some_and(|s| s.def.tools == "read_only"),
                    );
                    (t.plan, effort, model, t.route.clone())
                };

                // ── notes for the model: steering, mode changes, harness notes ──
                let mut reminders: Vec<String> = vec![];
                // Messages the user sent with images. These can't ride inside a
                // <system-reminder> (that's text), so they go into history as real
                // image blocks, with a short note so the model reads them as the
                // user's and not as its own tool output.
                let steer_blocks: Vec<Value> = vec![];
                if self.is_main() {
                    if seen_plan != Some(plan) {
                        if plan {
                            reminders.push(prompt::plan_reminder().into());
                        } else if seen_plan == Some(true) {
                            reminders.push(prompt::plan_off_reminder().into());
                        }
                        seen_plan = Some(plan);
                    }
                    let ultra = t.lock().await.ultra;
                    if seen_ultra != Some(ultra) {
                        if ultra {
                            reminders.push(prompt::ultra_reminder().into());
                        } else if seen_ultra == Some(true) {
                            reminders.push(prompt::ultra_off_reminder().into());
                        }
                        seen_ultra = Some(ultra);
                    }
                    let wt = ultra && t.lock().await.ultra_wt;
                    if self.is_main() && seen_wt != Some(wt) {
                        if wt {
                            reminders.push(prompt::ultra_wt_reminder().into());
                        } else if seen_wt == Some(true) && ultra {
                            reminders.push(prompt::ultra_wt_off_reminder().into());
                        }
                        seen_wt = Some(wt);
                    }
                    if let Some(n) = t.lock().await.stop_note.take() {
                        reminders.push(n);
                    }
                    reminders.extend(self.announce_changes().await);
                    // Alt-Enter messages are still waiting: the user asked for
                    // them to go out after this turn, so they go back on the queue
                    // and are read by the run that follows.
                    let wait = drain_queue(&h.runtime(&self.task_id)).await;
                    if !wait.is_empty() {
                        h.runtime(&self.task_id).queue.lock().await.extend(wait);
                    }
                } else if plan {
                    // A sub-agent in plan mode: its writes are refused by the same
                    // gate, but it has no `exit_plan_mode` to call, so without this it
                    // looks stuck rather than deliberate. Sent every turn, because
                    // plan mode can be switched on mid-run and this is what keeps a
                    // sub-agent on the read-only path.
                    reminders.push(prompt::plan_sub().into());
                }
                let notes: Vec<String> = h
                    .runtime(&self.task_id)
                    .inbox
                    .lock()
                    .await
                    .remove(&self.inbox_key())
                    .unwrap_or_default();
                // Drop the routing tag: it only ever existed for `wait` to read.
                reminders.extend(notes.into_iter().map(|n| {
                    n.split_once('\n')
                        .filter(|(head, _)| head.starts_with("<kind:"))
                        .map_or(n.clone(), |(_, body)| body.to_string())
                }));

                // ── snapshot history (append-only; reminders go on the newest user turn) ──
                let imgs: Vec<(String, String)> = h
                    .runtime(&self.task_id)
                    .inbox_images
                    .lock()
                    .await
                    .remove(&self.inbox_key())
                    .unwrap_or_default();
                let blocks = mid_turn_blocks(&reminders, &steer_blocks, &imgs);
                if !blocks.is_empty() {
                    if self.is_main() {
                        let mut t = t.lock().await;
                        push_user_blocks(&mut t.messages, blocks);
                    } else {
                        push_user_blocks(&mut local, blocks);
                    }
                }
                let mut messages = if self.is_main() {
                    let mut t = t.lock().await;
                    fix_dangling(&mut t.messages);
                    prune_images(&mut t.messages);
                    t.messages.clone()
                } else {
                    fix_dangling(&mut local);
                    prune_images(&mut local);
                    local.clone()
                };
                if messages.is_empty() {
                    return Ok(String::new());
                }

                // ── context + budget guards (main agent only) ──
                if self.is_main() {
                    let last_ctx = t.lock().await.usage.last_context;
                    if last_ctx > router::context_window(&model, &route_id) * 8 / 10 {
                        compact(h, &self.task_id, &self.cancel, None).await?;
                        continue;
                    }
                    let settings = h.settings.read().await.clone();
                    if !budget_ok
                        && settings.budget > 0.0
                        && settings.month_spend() >= settings.budget
                    {
                        // `?` on the Err arm, so a cancellation here reads as
                        // "Interrupted by user." rather than a budget refusal.
                        if !self
                            .ask_budget(settings.month_spend(), settings.budget)
                            .await?
                        {
                            return Err("Paused: monthly budget cap reached".into());
                        }
                        budget_ok = true;
                    }
                }

                // Subagents compact their own history near the limit too.
                if !self.is_main()
                    && sub_ctx > router::context_window(&model, &route_id) * 8 / 10
                    && self
                        .compact_sub(&mut local, &model, &route_id, effort)
                        .await?
                {
                    sub_ctx = 0;
                    continue;
                }

                // ── prefix: frozen system + tools ──
                let (system, tool_defs) = match (&sub_prefix, self.is_main()) {
                    (Some((s, tl)), _) => (s.clone(), tl.clone()),
                    (None, true) => {
                        let (s, _mcp, plugins) = frozen_prefix(h, &self.task_id).await?;
                        (s, main_tool_list(h, &self.task_id, &plugins, _mcp).await)
                    }
                    (None, false) => {
                        let (s, mcp, plugins) = frozen_prefix(h, &self.task_id).await?;
                        (s, main_tool_list(h, &self.task_id, &plugins, mcp).await)
                    }
                };

                // ── route the request ──
                if self.is_main() {
                    h.update_task(&self.task_id, |t| {
                        if t.todos.iter().all(|x| x.status != "in_progress") {
                            t.step = "Thinking".into();
                        }
                    })
                    .await;
                }
                messages.shrink_to_fit();
                // An agent that was sent the broadcast and is now working on it
                // has taken it on: that's the "who responded" signal.
                if h.runtime(&self.task_id)
                    .broadcast
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|b| {
                        b.who
                            .iter()
                            .any(|w| Some(w.as_str()) == self.sub_id() || self.is_main())
                    })
                {
                    mark_replied(h, &self.task_id, self.sub_id().unwrap_or("main")).await;
                }
                let cache_key = match self.sub_id() {
                    Some(s) => format!("ol-{}-{s}", self.task_id),
                    None => format!("ol-{}", self.task_id),
                };
                let req = ChatRequest {
                    system,
                    messages,
                    tools: tool_defs,
                    effort,
                    max_tokens: router::max_output(&model, &route_id).min(64_000),
                    cache_key,
                };
                let mut ui = StreamUi::new(
                    h.clone(),
                    self.task_id.clone(),
                    self.sub_id().map(String::from),
                );
                let result = {
                    let mut on = |e: StreamEvent| ui.on(e);
                    router::request(
                        h,
                        Who {
                            task_id: &self.task_id,
                            sub: self.sub_id(),
                            side: false,
                            turn: true,
                            usage_id: &self.usage_id(),
                        },
                        &model,
                        &req,
                        &mut on,
                        &self.cancel,
                    )
                    .await
                };
                ui.finish(
                    result
                        .as_ref()
                        .ok()
                        .map(|(_, t)| (t.model_id.clone(), t.label())),
                )
                .await;
                let (turn, target) = match result {
                    Ok(x) => x,
                    Err(RouteErr::Cancelled) => return Err(INTERRUPTED.into()),
                    // The user messaged this chat while it was frozen. Its
                    // message is in history now, so go round again and build the
                    // request from what the agent should actually answer.
                    Err(RouteErr::Refresh) => {
                        // Clear it for whoever is asking, not just the main
                        // agent. `steered` is per *task* and the router reads it
                        // on the way out of every wait, main agent and sub-agent
                        // alike — so a sub-agent that found it set rebuilt the
                        // identical request, was told to refresh again, and
                        // looped forever: the chat sat at "Working" with a
                        // sub-agent that could never finish or let go.
                        //
                        // Whoever rebuilds consumes the flag, which is the
                        // contract: `drop_undelivered` and the send paths own
                        // re-arming it for a genuinely new message.
                        h.runtime(&self.task_id)
                            .steered
                            .store(false, Ordering::SeqCst);
                        continue;
                    }
                    Err(RouteErr::TooLong(e)) => {
                        if self.is_main() {
                            compact(h, &self.task_id, &self.cancel, None).await?;
                            continue;
                        }
                        if self
                            .compact_sub(&mut local, &model, &route_id, effort)
                            .await?
                        {
                            sub_ctx = 0;
                            continue;
                        }
                        return Err(format!("Ran out of context: {e}"));
                    }
                    Err(RouteErr::Fatal(e)) => return Err(e),
                };

                // ── account ──
                let mi = providers::model_info(&target.model_id);
                let cost = turn.usage.cost(&mi);
                record_spend(
                    h,
                    cost,
                    turn.usage.input
                        + turn.usage.output
                        + turn.usage.cache_read
                        + turn.usage.cache_write,
                )
                .await;
                let label = target.label();
                let sid = self.sub_id().map(String::from);
                h.update_task(&self.task_id, |t| {
                    t.usage.input += turn.usage.input;
                    t.usage.output += turn.usage.output;
                    t.usage.cache_read += turn.usage.cache_read;
                    t.usage.cache_write += turn.usage.cache_write;
                    t.usage.cost += cost;
                    match &sid {
                        None => {
                            t.usage.last_context = turn.usage.context();
                            t.serving = label;
                        }
                        Some(s) => {
                            if let Some(x) = t.subs.iter_mut().find(|x| &x.id == s) {
                                x.serving = label;
                            }
                            sub_ctx = turn.usage.context();
                        }
                    }
                })
                .await;

                let text: String = turn
                    .content
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    last_text = text;
                }
                if !turn.content.is_empty() {
                    let msg = Message {
                        role: "assistant".into(),
                        content: turn.content.clone(),
                        model: turn.model.clone(),
                    };
                    if self.is_main() {
                        t.lock().await.messages.push(msg);
                    } else {
                        local.push(msg);
                        self.save_sub_history(&local).await;
                    }
                    attach_inputs(
                        h,
                        &self.task_id,
                        self.sub_id(),
                        &turn.content,
                        &ui.tool_items,
                    )
                    .await;
                }
                if turn.stop_reason == "refusal" {
                    return Err("The model declined to continue this request.".into());
                }

                let calls: Vec<Value> = turn
                    .content
                    .iter()
                    .filter(|b| b["type"] == "tool_use")
                    .cloned()
                    .collect();
                if calls.is_empty() {
                    // Only notes keep the loop going. Alt-Enter messages deliberately
                    // don't: they asked to wait for this turn to end, and holding the
                    // run open for them would be the opposite of that.
                    let rt = h.runtime(&self.task_id);
                    let has_notes = rt
                        .inbox
                        .lock()
                        .await
                        .get(&self.inbox_key())
                        .is_some_and(|v| !v.is_empty());
                    if has_notes {
                        continue;
                    }
                    if let Some(sub) = &self.sub {
                        if sub.def.steps > 0 && tool_uses_total >= sub.def.steps && !step_limited {
                            step_limited = true;
                            local.push(Message::user_text(format!("<system-reminder>You have reached the {}-step ceiling for `{}`. Do not start more work; summarise what you did, what remains, and any handoff advice for your parent agent.</system-reminder>", sub.def.steps, sub.def.name)));
                            self.step("Summarising at the agent step limit".into())
                                .await;
                            continue;
                        }
                    }
                    if turn.stop_reason == "max_tokens" {
                        let nudge = Message::user_text("<system-reminder>Your last response hit the output limit. Continue exactly where you stopped.</system-reminder>");
                        if self.is_main() {
                            t.lock().await.messages.push(nudge);
                        } else {
                            local.push(nudge);
                        }
                        continue;
                    }
                    // Edited but never checked: one nudge per run.
                    let (want_verify, hooks) = {
                        let s = h.settings.read().await;
                        (s.verify_nudge, s.hooks.clone())
                    };
                    if want_verify && edited_any && !verified && !nudged_verify {
                        nudged_verify = true;
                        let n = Message::user_text(checks::VERIFY_NUDGE);
                        if self.is_main() {
                            t.lock().await.messages.push(n);
                        } else {
                            local.push(n);
                        }
                        self.step("Checking its work".into()).await;
                        continue;
                    }
                    // Stop hooks: a failing one sends its output back (at most 3 rounds).
                    if self.is_main() && stop_hook_rounds < 3 {
                        let (project, cwd) = {
                            let task = t.lock().await;
                            (task.project.clone(), task.cwd.clone())
                        };
                        let outs = checks::stop(
                            &hooks,
                            &last_text,
                            &cwd,
                            &project,
                            &self.task_id,
                            &self.cancel,
                        )
                        .await;
                        let failed: Vec<String> = outs
                            .iter()
                            .filter(|o| !o.ok)
                            .map(|o| format!("`{}`:\n{}", o.command, o.output))
                            .collect();
                        if !failed.is_empty() {
                            stop_hook_rounds += 1;
                            let n = format!("<system-reminder>You tried to finish, but the user's stop hook failed:\n\n{}\n\nAddress this, then finish.</system-reminder>", failed.join("\n\n"));
                            t.lock().await.messages.push(Message::user_text(n));
                            h.upsert_item(
                                &self.task_id,
                                Item::new(
                                    "notice",
                                    "A stop hook failed · the agent keeps going",
                                    json!({"level": "event", "sum": "a stop hook failed"}),
                                ),
                            )
                            .await;
                            continue;
                        }
                    }
                    if self.is_main() {
                        // Plan mode: the user asked for a plan before any changes, so
                        // "you're not finished yet" is the wrong thing to tell an agent
                        // that is forbidden from making them. It wins over the goal and
                        // ultrathread guards below, which both read as "keep editing".
                        let plan_nudge = {
                            let t = t.lock().await;
                            if t.plan && plan_nudges < MAX_PLAN_NUDGES {
                                plan_nudges += 1;
                                Some(prompt::plan_nudge(plan_nudges, MAX_PLAN_NUDGES))
                            } else {
                                None
                            }
                        };
                        if let Some(n) = plan_nudge {
                            t.lock().await.messages.push(Message::user_text(n));
                            h.update_task(&self.task_id, |t| t.step = "Writing the plan".into())
                                .await;
                            continue;
                        }
                        // Goal mode: don't stop until the agent proves the goal is met.
                        let nudge = {
                            let mut t = t.lock().await;
                            let plan = t.plan;
                            match t.goal.as_mut() {
                                // Plan mode still applies once the plan nudges run
                                // out: the agent is barred from the work the goal asks
                                // for, so telling it to get on with it would only
                                // produce refused edits. It ends the run and leaves the
                                // goal open, which is the honest outcome.
                                Some(g)
                                    if g.status == "active"
                                        && !plan
                                        && g.nudges < MAX_GOAL_NUDGES =>
                                {
                                    g.nudges += 1;
                                    Some(prompt::goal_nudge(&g.text, g.nudges, MAX_GOAL_NUDGES))
                                }
                                Some(g) if g.status == "active" && !plan => {
                                    g.status = "gave_up".into();
                                    None
                                }
                                _ => None,
                            }
                        };
                        if let Some(n) = nudge {
                            t.lock().await.messages.push(Message::user_text(n));
                            h.update_task(&self.task_id, |t| t.step = "Checking the goal".into())
                                .await;
                            continue;
                        }
                        // Ultrathread: keep going while the todo list has open items.
                        let ultra_nudge = {
                            let t = t.lock().await;
                            let open: Vec<String> = t
                                .todos
                                .iter()
                                .filter(|x| x.status != "completed")
                                .map(|x| x.content.clone())
                                .collect();
                            let running = t.subs.iter().filter(|s| s.status == "running").count();
                            let unplanned = t.todos.is_empty() && ultra_nudges == 0;
                            if t.ultra
                                && !t.plan
                                && ultra_nudges < MAX_ULTRA_NUDGES
                                && (!open.is_empty() || running > 0 || unplanned)
                            {
                                ultra_nudges += 1;
                                Some(prompt::ultra_nudge(
                                    ultra_nudges,
                                    MAX_ULTRA_NUDGES,
                                    &open,
                                    running,
                                ))
                            } else {
                                None
                            }
                        };
                        if let Some(n) = ultra_nudge {
                            t.lock().await.messages.push(Message::user_text(n));
                            h.update_task(&self.task_id, |t| {
                                t.step = "Planning the next wave".into()
                            })
                            .await;
                            continue;
                        }
                        h.save_task(&self.task_id).await;
                    }
                    return Ok(last_text);
                }

                // ── run tools: consecutive concurrency-safe calls go in parallel ──
                tool_uses_total += calls.len();
                let mut results: Vec<Value> = Vec::with_capacity(calls.len());
                let mut i = 0;
                let safe = |c: &Value| {
                    let n = c["name"].as_str().unwrap_or("");
                    // `web_search_deep` / `web_read_many` are read-only against
                    // the web but cost real requests, so they batch here like
                    // `web_fetch` while still going through the normal
                    // permission gate (which still auto-allows read-only tools).
                    n == "task"
                        || (permissions::is_read_only_tool(n)
                            && !matches!(
                                n,
                                "ask_user"
                                    | "ask_nonblocking"
                                    | "notify_user"
                                    | "ask"
                                    | "exit_plan_mode"
                                    | "start_ultrathread"
                                    | "todo_write"
                                    | "goal_complete"
                                    | "wait"
                                    | "send_message"
                            ))
                        || permissions::is_web_read_tool(n)
                };
                while i < calls.len() {
                    if safe(&calls[i]) {
                        let mut j = i;
                        while j < calls.len() && safe(&calls[j]) {
                            j += 1;
                        }
                        let batch =
                            join_all(calls[i..j].iter().map(|c| self.exec(c, &ui.tool_items)))
                                .await;
                        results.extend(batch);
                        i = j;
                    } else {
                        results.push(self.exec(&calls[i], &ui.tool_items).await);
                        i += 1;
                    }
                }
                // ── harness feedback: loops, verification, diagnostics ──
                let cwd = t.lock().await.cwd_for(self.sub_id()).0;
                let mut notes: Vec<String> = vec![];
                let mut edited: Vec<std::path::PathBuf> = vec![];
                for (c, r) in calls.iter().zip(results.iter()) {
                    let name = c["name"].as_str().unwrap_or("");
                    let is_err = r["is_error"] == true;
                    if let Some(n) = guard.record(name, &c["input"], is_err) {
                        notes.push(n);
                    }
                    if !is_err && matches!(name, "edit_file" | "multi_edit" | "write_file") {
                        if let Some(p) = c["input"]["path"].as_str() {
                            edited.push(tools::resolve(&cwd, p));
                        }
                    }
                    if (name == "bash" || name == "shell")
                        && checks::is_verify_command(c["input"]["command"].as_str().unwrap_or(""))
                    {
                        verified = true;
                    }
                }
                if !edited.is_empty() {
                    edited_any = true;
                    if h.settings.read().await.diagnostics && !self.cancel.is_cancelled() {
                        self.step("Checking for errors".into()).await;
                        if let Some(d) = checks::diagnose(&cwd, &edited, &self.cancel).await {
                            notes.push(d);
                        }
                    }
                }
                results.extend(
                    notes
                        .into_iter()
                        .map(|n| json!({"type": "text", "text": n})),
                );
                let msg = Message::user(results);
                if self.is_main() {
                    t.lock().await.messages.push(msg);
                    h.save_task(&self.task_id).await;
                } else {
                    local.push(msg);
                    self.save_sub_history(&local).await;
                    if let Some(s) = &self.sub {
                        let n = tool_uses_total;
                        let sid = s.id.clone();
                        h.update_task(&self.task_id, |t| {
                            if let Some(x) = t.subs.iter_mut().find(|x| x.id == sid) {
                                x.meta = format!("{n} tool use{}", if n == 1 { "" } else { "s" });
                            }
                        })
                        .await;
                    }
                }
                if self.cancel.is_cancelled() {
                    return Err(INTERRUPTED.into());
                }
            }
        })
    }

    /// Summarise a subagent's own history and restart it from the summary.
    /// Ok(false) if there's nothing to compact.
    async fn compact_sub(
        &self,
        local: &mut Vec<Message>,
        model: &str,
        route_id: &str,
        effort: usize,
    ) -> Result<bool, String> {
        let Some(s) = &self.sub else { return Ok(false) };
        if local.len() < 3 {
            return Ok(false);
        }
        self.h
            .upsert_sub_item(
                &self.task_id,
                &s.id,
                Item::new(
                    "notice",
                    "Compacting this subagent's context…",
                    json!({"level": "info"}),
                ),
            )
            .await;
        let t = self.h.task(&self.task_id).await?;
        let (cwd, project, branch, worktree) = {
            let t = t.lock().await;
            let (cwd, branch) = t.cwd_for(Some(&s.id));
            (
                cwd,
                t.project.clone(),
                branch,
                t.worktree || t.subs.iter().any(|x| x.id == s.id && !x.cwd.is_empty()),
            )
        };
        let tg = t.lock().await;
        let plugins = prefix_plugins(&self.h, &tg).await;
        drop(tg);
        let (skills, inject_global_claude) = {
            let st = self.h.settings.read().await;
            let out = (store::all_skills(&st, &project), st.inject_global_claude);
            drop(st);
            out
        };
        let system = prompt::system(
            &prompt::Env {
                cwd: &cwd,
                project: &project,
                branch: &branch,
                is_git: git::is_repo(&cwd),
                worktree,
                date: chrono::Local::now().format("%Y-%m-%d").to_string(),
                agent_id: &format!("ol-{}/{}", self.task_id, s.id),
                inject_global_claude,
            },
            Some(&s.def),
            &skills,
        );
        let mut msgs = local.clone();
        fix_dangling(&mut msgs);
        prune_images(&mut msgs);
        let brief = local
            .first()
            .map(|m| tools::tool_text(&json!(m.content)))
            .unwrap_or_default();
        let ask = format!(
            "{}\n\nRespond with text only; do not call any tools.",
            prompt::compact_prompt()
        );
        push_user_blocks(&mut msgs, vec![json!({"type": "text", "text": ask})]);
        let req = ChatRequest {
            system,
            messages: msgs,
            tools: tools::schemas(Some(&s.def.tools), &plugins),
            effort: effort.max(2),
            max_tokens: 12_000.min(router::max_output(model, route_id)),
            cache_key: format!("ol-{}-{}", self.task_id, s.id),
        };
        let (summary, usage, target) = match router::oneshot(
            &self.h,
            Who {
                task_id: &self.task_id,
                sub: Some(&s.id),
                side: false,
                turn: false,
                usage_id: "compactor",
            },
            model,
            &req,
            &self.cancel,
        )
        .await
        {
            Ok(x) => x,
            Err(RouteErr::Cancelled) => return Err(INTERRUPTED.into()),
            Err(_) => return Ok(false),
        };
        let cost = usage.cost(&providers::model_info(&target.model_id));
        record_spend(&self.h, cost, usage.input + usage.output).await;
        self.h
            .update_task(&self.task_id, |t| t.usage.cost += cost)
            .await;
        let brief: String = brief.chars().take(20_000).collect();
        *local = vec![Message::user_text(format!("Your original brief:\n\n{brief}\n\n---\nYour context was compacted. Summary of your work so far:\n\n{summary}\n\nContinue from where you left off."))];
        self.save_sub_history(local).await;
        self.h.stats.event_in(&self.task_id, "compactions");
        Ok(true)
    }

    async fn tool_search(
        &self,
        input: &Value,
        mcp: &[Value],
        plugins: &super::plugins::PluginsCfg,
    ) -> Result<String, String> {
        let query = input["query"].as_str().unwrap_or("").trim();
        if query.is_empty() {
            return Err("query is required.".into());
        }
        let settings = self.h.settings.read().await.clone();
        let core = tools::schemas(None, plugins);
        let off: Vec<String> = settings
            .mcp
            .iter()
            .filter(|m| !m.defer)
            .map(|m| m.name.clone())
            .collect();
        let deferral = toolindex::Deferral::new(
            core.len(),
            mcp,
            &off,
            settings.defer_plugins,
            settings.tool_search,
        );
        let catalog = &deferral.catalog;
        let limit = input["limit"]
            .as_u64()
            .unwrap_or(toolindex::DEFAULT_RESULTS as u64) as usize;
        let loaded = toolindex::loaded(
            &self.h.runtime(&self.task_id),
            self.sub_id().unwrap_or("main"),
        )
        .await;
        let selected = query.strip_prefix("select:").map(str::trim);
        let hits: Vec<_> = if let Some(name) = selected {
            deferral
                .catalog
                .tools
                .iter()
                .filter(|x| x.name == name && x.defer)
                .collect()
        } else {
            deferral.catalog.search(query, limit)
        };
        if hits.is_empty() {
            return Ok(toolindex::search_result(&deferral.catalog, query, limit));
        }
        let deferral = toolindex::Deferral::from_catalog(catalog.clone(), true);
        let names = deferral.newly_loaded(&hits, &loaded);
        toolindex::remember(
            &self.h.runtime(&self.task_id),
            self.sub_id().unwrap_or("main"),
            &names,
        )
        .await;
        let schemas: Vec<String> = hits
            .iter()
            .map(|x| {
                serde_json::to_string_pretty(&x.schema).unwrap_or_else(|_| x.schema.to_string())
            })
            .collect();
        Ok(format!(
            "{}\n\nSchemas for these deferred tools:\n{}",
            toolindex::search_result(&catalog, query, limit),
            schemas.join("\n\n")
        ))
    }

    /// Compare what the model was last told against the task's current
    /// assist mode / sub-agent list and produce announcements for anything new.
    async fn announce_changes(&self) -> Vec<String> {
        let Ok(t) = self.h.task(&self.task_id).await else {
            return vec![];
        };
        // Only the fields this compares. Cloning the whole task copied the entire
        // transcript -- megabytes by the end of a long run -- once per turn, just
        // to read the assist mode and what had already been announced.
        let (assist, told_assist, subagents, told_agents) = {
            let g = t.lock().await;
            (
                g.assist.clone(),
                g.told
                    .get("assist")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                g.subagents,
                g.told
                    .get("agents")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            )
        };
        let mut out = vec![];
        if told_assist.as_deref() != Some(assist.as_str()) {
            out.push(prompt::assist_note(&assist, told_assist.is_some()));
        }
        // `allowed_agents` reads a handful of small fields; the lock is held
        // only for that read, not for a copy of the whole transcript.
        let defs = if subagents {
            let g = t.lock().await;
            allowed_agents(&self.h, &g).await
        } else {
            vec![]
        };
        let sig = defs
            .iter()
            .map(|d| format!("{}:{}:{}", d.id, d.tools, d.description))
            .collect::<Vec<_>>()
            .join("|");
        if told_agents.as_deref() != Some(sig.as_str()) {
            out.push(prompt::agents_note(&defs, told_agents.is_some()));
        }
        // The opt-in feature tools are in the frozen tool list either way, so a
        // running chat has them in hand even though the system prompt was built
        // before the user flipped the switch. Announce the change so the rules
        // that govern them arrive with the next turn.
        let (mem, told_mem) = {
            // Task first, settings second — never the other way round. Holding
            // the settings guard across a task-lock await would invert the order
            // the rest of the file uses, and a settings write landing in that
            // window would deadlock the turn.
            let g = t.lock().await;
            let told_mem = g.told.get("memory").and_then(|v| v.as_bool());
            drop(g);
            let s = self.h.settings.read().await;
            (s.memory, told_mem)
        };
        if mem != told_mem.unwrap_or(false) {
            out.push(
                if mem {
                    prompt::memory_on_note()
                } else {
                    prompt::memory_off_note()
                }
                .to_string(),
            );
        }
        if !out.is_empty() {
            let mut t = t.lock().await;
            t.told.insert("assist".into(), json!(assist));
            t.told.insert("agents".into(), json!(sig));
            t.told.insert("memory".into(), json!(mem));
        }
        out
    }

    async fn ask_budget(&self, spent: f64, cap: f64) -> Result<bool, String> {
        let item = Item::new(
            "approval",
            "Monthly budget cap reached",
            json!({"kind": "budget", "title": "Monthly budget cap reached", "detail": format!("${spent:.2} spent of ${cap:.0}"), "reason": "Agents pause and ask before crossing the cap."}),
        );
        let id = item.id.clone();
        self.h.upsert_item(&self.task_id, item).await;
        let r = self
            .h
            .wait_for_user(&self.task_id, &id, "approval", &self.cancel)
            .await;
        // `wait_for_user` returns None for BOTH "the user pressed Deny" and
        // "the run was cancelled". Treating them alike wrote a lie into the
        // timeline — a stopped run showed `resolved: deny` and reported a
        // budget failure instead of an interruption. Same split `gate` makes.
        let Some(v) = r else {
            self.h
                .patch_item(&self.task_id, &id, |i| {
                    i.data["resolved"] = json!("cancelled")
                })
                .await;
            return Err("Interrupted by user.".into());
        };
        let ok = v["decision"] != "deny";
        self.h
            .patch_item(&self.task_id, &id, |i| {
                i.data["resolved"] = json!(if ok { "once" } else { "deny" })
            })
            .await;
        Ok(ok)
    }

    // ───────────────────────────── tool execution ─────────────────────────────

    fn exec<'a>(&'a self, call: &'a Value, items: &'a [(String, String)]) -> BoxFut<'a, Value> {
        Box::pin(async move {
            let id = call["id"].as_str().unwrap_or("").to_string();
            // See exec_inner: `read`/`shell` are OpenCode-gatekeeper aliases.
            let name = match call["name"].as_str().unwrap_or("") {
                "read" => "read_file".to_string(),
                "shell" => "bash".to_string(),
                n => n.to_string(),
            };
            let input = call["input"].clone();
            let item_id = items
                .iter()
                .find(|(tid, _)| *tid == id)
                .map(|x| x.1.clone());
            let (project, cwd) = match self.h.task(&self.task_id).await {
                Ok(t) => {
                    let guard = t.lock().await;
                    (guard.project.clone(), guard.cwd_for(self.sub_id()).0)
                }
                Err(_) => (String::new(), String::new()),
            };
            let hooks = {
                let settings = self.h.settings.read().await;
                checks::hooks_for(&settings, &project)
            };
            let has_hooks = hooks.iter().any(|x| x.enabled && x.trusted);
            let (mcp, plugins) = match self.h.task(&self.task_id).await {
                Ok(task) => {
                    let task = task.lock().await;
                    (task.mcp_tools.clone(), task.plugins.clone())
                }
                Err(_) => (vec![], super::plugins::PluginsCfg::default()),
            };
            let pre = if has_hooks && !self.cancel.is_cancelled() {
                checks::pre_tool(
                    &hooks,
                    &name,
                    &input,
                    &cwd,
                    &project,
                    &self.task_id,
                    &self.cancel,
                )
                .await
            } else {
                checks::PreTool {
                    blocked: None,
                    updated_input: None,
                }
            };
            let effective_input = pre.updated_input.unwrap_or_else(|| input.clone());
            let blocked = pre.blocked;
            let (content, is_err): (Value, bool) = if self.cancel.is_cancelled() {
                (json!("Interrupted by user before this tool ran."), true)
            } else if let Some(b) = blocked {
                (
                    json!(format!(
                        "Blocked by pre_tool hook `{}`:\n{}",
                        b.command,
                        b.refusal().unwrap_or_else(|| b.output.clone())
                    )),
                    true,
                )
            } else if effective_input.get("__invalid_json").is_some() {
                (json!("INVALID_JSON: the tool input was not valid JSON (possibly truncated). Re-issue the call with complete, valid arguments."), true)
            } else if name == "tool_search" && self.is_main() {
                match self.tool_search(&effective_input, &mcp, &plugins).await {
                    Ok(v) => (json!(v), false),
                    Err(e) => (json!(e), true),
                }
            } else if name == "view_image" {
                let result: Result<Value, String> = async {
                    let path = effective_input["path"].as_str().ok_or("path is required")?;
                    let resolved = tools::resolve(&cwd, path);
                    let resolved = resolved.to_string_lossy().into_owned();
                    self.gate(&name, &effective_input, Some(&resolved)).await?;
                    self.exec_view_image(&effective_input, item_id.as_deref())
                        .await
                }
                .await;
                match result {
                    Ok(v) => (v, false),
                    Err(e) => (json!(e), true),
                }
            } else if name == "screenshot" {
                match self
                    .exec_screenshot(&effective_input, item_id.as_deref())
                    .await
                {
                    Ok(v) => (v, false),
                    Err(e) => (json!(e), true),
                }
            } else if name == "computer" {
                match self
                    .exec_computer(&effective_input, item_id.as_deref())
                    .await
                {
                    Ok(v) => (v, false),
                    Err(e) => (json!(e), true),
                }
            } else if name == "browser" {
                match self
                    .exec_browser(&effective_input, item_id.as_deref())
                    .await
                {
                    Ok(v) => (v, false),
                    Err(e) => (json!(e), true),
                }
            } else {
                match self
                    .exec_inner(&name, &effective_input, item_id.as_deref(), &id)
                    .await
                {
                    Ok(s) => (json!(s), false),
                    Err(e) => (json!(e), true),
                }
            };
            let mut content = content;
            if has_hooks && !self.cancel.is_cancelled() {
                let outs = checks::post_tool(
                    &hooks,
                    &name,
                    &effective_input,
                    &tools::tool_text(&content),
                    &cwd,
                    &project,
                    &self.task_id,
                    &self.cancel,
                )
                .await;
                let extra: Vec<String> = outs
                    .iter()
                    .filter(|o| !o.output.is_empty() || !o.ok)
                    .map(|o| {
                        format!(
                            "[post_tool hook `{}`{}]\n{}",
                            o.command,
                            if o.ok { "" } else { " failed" },
                            o.output
                        )
                    })
                    .collect();
                if !extra.is_empty() {
                    let add = extra.join("\n");
                    content = match content {
                        Value::String(s) => json!(format!("{s}\n\n{add}")),
                        Value::Array(mut a) => {
                            a.push(json!({"type": "text", "text": add}));
                            Value::Array(a)
                        }
                        v => v,
                    };
                }
            }
            let preview = tools::tool_text(&content);
            self.h.stats.tool_in(&self.task_id, &name, is_err);
            if let Some(iid) = &item_id {
                let content_c = content.clone();
                let preview_c = preview.clone();
                let denied = preview.starts_with("The user denied")
                    || preview.starts_with("Plan mode is active");
                let is_image = content.is_array();
                self.h
                    .patch_in(&self.task_id, self.sub_id(), iid, |it| {
                        it.data["status"] = json!(if denied {
                            "denied"
                        } else if is_err {
                            "error"
                        } else {
                            "ok"
                        });
                        if it.data["output"].is_null() {
                            it.data["output"] =
                                json!(preview_c.chars().take(20_000).collect::<String>());
                            // Keep the image bytes out of the text output: the UI
                            // renders `images` as thumbnails, providers get them
                            // from the tool_result content array.
                            if is_image {
                                if let Some(arr) = content_c.as_array() {
                                    let uris: Vec<Value> = arr
                                        .iter()
                                        .filter(|b| b["type"] == "image")
                                        .filter_map(|b| {
                                            let mt = b["source"]["media_type"].as_str()?;
                                            let data = b["source"]["data"].as_str()?;
                                            if data.len() > 12_000_000 {
                                                return None;
                                            }
                                            Some(json!(format!("data:{mt};base64,{data}")))
                                        })
                                        .collect();
                                    if !uris.is_empty() {
                                        it.data["images"] = json!(uris);
                                    }
                                }
                            }
                        }
                    })
                    .await;
            }
            let empty = match &content {
                Value::String(s) => s.is_empty(),
                Value::Array(a) => a.is_empty(),
                _ => false,
            };
            let mut out = json!({"type": "tool_result", "tool_use_id": id, "content": if empty { json!("(no output)") } else { content }});
            if is_err {
                out["is_error"] = json!(true);
            }
            out
        })
    }

    /// View an image file and return an Anthropic-style content array
    /// ([text, image]) so the model actually sees it. Also tags the timeline
    /// item with a thumbnail (`images`) + dimensions meta for the UI.
    async fn exec_view_image(&self, input: &Value, item_id: Option<&str>) -> Result<Value, String> {
        let t = self.h.task(&self.task_id).await?;
        let cwd = t.lock().await.cwd_for(self.sub_id()).0;
        let raw = input["path"].as_str().ok_or("path is required")?;
        let p = tools::resolve(&cwd, raw);
        // Sub-agents: view_image is read-only, allowed for every tool policy.
        self.step(format!(
            "Viewing {}",
            permissions::short(&p.to_string_lossy(), &cwd)
        ))
        .await;
        let img = tools::view_image(&p)?;
        let display = p.display().to_string();
        self.finish_image(item_id, &display, &img).await;
        Ok(tools::image_content(&display, &img))
    }

    /// Capture the screen and return it the same way as view_image.
    ///
    /// Gated by the computer-use toggle, like the `computer` tool itself.
    /// `screenshot` is a core tool, so it used to keep working with the plugin
    /// off — the agent could still see the user's whole screen (passwords, mail,
    /// other people's chat) while the setting plainly said computer use was
    /// disabled. If the plugin is on, this needs no approval of its own: the
    /// permission that matters is the one the `computer` tool already asks for,
    /// and a one-shot screenshot is strictly weaker than a click.
    async fn exec_screenshot(&self, input: &Value, item_id: Option<&str>) -> Result<Value, String> {
        if !self.h.settings.read().await.plugins.computer.enabled {
            return Err("Taking screenshots is part of the computer-use plugin, which is turned off (Settings → Plugins). To look at a web page without your screen, use `render` instead — it runs in a headless browser and never touches your desktop.".into());
        }
        let mon = input["monitor"].as_u64().map(|m| m as usize);
        self.step("Taking a screenshot".into()).await;
        // Screenshots can take a moment; don't block shutdown.
        let (img, label) = tools::screenshot(mon)?;
        let display = format!("screenshot ({label})");
        self.finish_image(item_id, &display, &img).await;
        Ok(tools::image_content(&display, &img))
    }

    /// Computer-use plugin (OpenAI-style): run a batch of actions after approval,
    /// then return one screenshot of the screen afterwards.
    async fn exec_computer(&self, input: &Value, item_id: Option<&str>) -> Result<Value, String> {
        use super::plugins;
        let (enabled, settle) = {
            let s = self.h.settings.read().await;
            (s.plugins.computer.enabled, s.plugins.computer.settle_ms)
        };
        if !enabled {
            return Err("The computer-use plugin is turned off (Settings → Plugins).".into());
        }
        let summary = plugins::computer_summary(input);
        let observe = plugins::computer_is_observe(input);
        if !observe {
            if let Some(s) = self.sub.as_ref().filter(|s| s.def.tools == "read_only") {
                return Err(format!("The {} agent can only take screenshots.", s.def.id));
            }
            self.gate("computer", input, None).await?;
        }
        self.step(format!(
            "Computer: {}",
            summary.chars().take(80).collect::<String>()
        ))
        .await;
        let acts = input["actions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .any(|x| x["type"] != "screenshot" && x["type"] != "wait")
            })
            .unwrap_or(false);
        // Tell the user the agent has their screen before it takes it. The
        // banner is the only warning they'd get: the app isn't focused while
        // the agent drives another window, so nothing else is visible.
        if acts {
            super::pcguard::begin(&self.task_id, &summary.chars().take(90).collect::<String>());
        }
        // The batch must stop the moment the user objects, without killing the
        // run: `computer_act` polls this, and the agent is told why separately.
        let pc_token = self.cancel.child_token();
        let objection_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watch_pc = {
            let seen = objection_seen.clone();
            let tok = pc_token.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(120)) => {
                            if super::pcguard::take_objection() {
                                remember_computer_objection(&seen, true);
                                tok.cancel();
                                return;
                            }
                        }
                    }
                }
            })
        };
        let (ran, failed) = plugins::computer_act(input.clone(), pc_token.clone()).await;
        watch_pc.abort();
        let _ = watch_pc.await;
        pc_token.cancel();
        super::pcguard::end();
        // An objection is not a cancellation: the user disliked the screen
        // takeover, not the whole run, so say so in the tool result and let the
        // agent decide what to do with the rest of its work.
        remember_computer_objection(&objection_seen, super::pcguard::take_objection());
        let objected = objection_seen.load(std::sync::atomic::Ordering::SeqCst);
        if objected {
            return Err(format!("Stopped after {ran} action(s): the user pressed Esc and objected to you controlling their screen. They have been told why. Don't try to take the screen again — carry on with anything that doesn't need the mouse or keyboard, and ask before touching the screen again."));
        }
        if self.cancel.is_cancelled() {
            return Err(format!("Interrupted by user after {ran} action(s)."));
        }
        if acts && ran > 0 {
            // Let the UI react before looking.
            tokio::time::sleep(std::time::Duration::from_millis(if settle == 0 {
                600
            } else {
                settle.min(5000)
            }))
            .await;
        }
        let shot = tokio::task::spawn_blocking(plugins::computer_shot)
            .await
            .map_err(|e| format!("screenshot thread failed: {e}"))??;
        let display = format!("screen {}x{}", shot.shot.0, shot.shot.1);
        self.finish_image(item_id, &display, &shot.img).await;
        let mut content = tools::image_content(&display, &shot.img);
        content[0]["text"] = json!(match &failed {
            Some(e) => format!("{e} ({ran} earlier action(s) ran.) Screen now: {display}."),
            None => format!("Screen after the actions: {display}."),
        });
        self.set_item(item_id, |i| {
            i.data["meta"] = json!(if failed.is_some() {
                "failed".to_string()
            } else {
                format!("{ran} action{}", if ran == 1 { "" } else { "s" })
            })
        })
        .await;
        Ok(content)
    }

    /// Browser plugin: drive a real headless browser — navigate, read, click,
    /// type, evaluate JS, screenshot — kept alive for the task.
    ///
    /// Nothing here touches the user's screen, mouse or keyboard, and the
    /// profile is a throwaway per task, so the reads (`navigate`, `read`,
    /// `screenshot`, `js`, `info`, `back`) need no approval: that is the whole
    /// point of the plugin, and the reason it is the tool the system prompt
    /// tells the agent to reach for instead of a screen tool.
    ///
    /// `click` and `type` are different in kind. They change the site — a
    /// submitted form, a login, a post — so they go through the permission gate
    /// like any other write, and the approval card names the action.
    async fn exec_browser(&self, input: &Value, item_id: Option<&str>) -> Result<Value, String> {
        use super::{browser, plugins};
        let cfg = self.h.settings.read().await.plugins.browser.clone();
        if !cfg.enabled {
            return Err("The browser plugin is turned off (Settings → Plugins). To read a page without it, use `web_fetch`; to see one as a picture, `render`.".into());
        }
        let action = input["action"].as_str().unwrap_or("").trim().to_string();
        if action.is_empty() {
            return Err(
                "`action` is required: navigate, read, screenshot, click, type, js, back or info."
                    .into(),
            );
        }
        let t = self.h.task(&self.task_id).await?;
        let cwd = t.lock().await.cwd_for(self.sub_id()).0;
        // One browser per task (and sub), so a session is a conversation's own.
        let owner = format!("{}:{}", self.task_id, self.sub_id().unwrap_or("main"));
        let s = browser::session_for(
            &owner,
            if cfg.width == 0 { 1280 } else { cfg.width },
            if cfg.height == 0 { 800 } else { cfg.height },
        )
        .await?;

        // Gate the two actions that change the world, before touching the page.
        if action == "click" || action == "type" {
            let what = if action == "click" {
                format!(
                    "click `{}`",
                    input["selector"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(60)
                        .collect::<String>()
                )
            } else {
                let t = input["text"].as_str().unwrap_or("");
                format!("type {} characters", t.chars().count())
            };
            let mut gate = input.clone();
            gate["action"] = json!(action.clone());
            gate["detail"] = json!(what.clone());
            gate["__page"] = json!(s.remembered().url);
            self.gate(
                "browser",
                &gate,
                Some(&format!("{action} · {what} on {}", s.remembered().url)),
            )
            .await?;
        }

        let step = match action.as_str() {
            "navigate" => {
                let raw = input["url"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("`navigate` needs a `url` (or a path to a local .html file).")?;
                let target = browser_target(raw, &cwd)?;
                let st = s.goto(&target).await?;
                format!("Browsing {}", short(&st.url))            }
            "read" => {
                s.settle().await?;
                let v = s
                    .eval("(() => { const t = document.body ? document.body.innerText : ''; return JSON.stringify({url: location.href, title: document.title, text: t.slice(0, 20000)}); })()")
                    .await?;
                return Ok(text_result(&v, "The page is empty or has no readable text — try `screenshot` to see it, or `js` to read the DOM."));
            }
            "info" => {
                let v = s
                    .eval("JSON.stringify({url: location.href, title: document.title, headings: [...document.querySelectorAll('h1,h2,h3')].slice(0,25).map(h=>h.innerText.trim()).filter(Boolean), links: [...document.querySelectorAll('a[href]')].slice(0,40).map(a=>a.innerText.trim().slice(0,60)+' -> '+a.href)})")
                    .await?;
                return Ok(text_result(&v, "Nowhere yet — `navigate` to a url first."));
            }
            "screenshot" => {
                let full = input["full_page"].as_bool().unwrap_or(false);
                let bytes = s.screenshot(full).await?;
                let img = plugins::browser_image(bytes)?;
                let label = s.label();
                self.finish_image(item_id, &label, &img).await;
                let mut content = tools::image_content(&label, &img);
                content[0]["text"] = json!(format!("Screenshot of {label} ({}).", img.media_type));
                return Ok(content);
            }
            "click" => {
                let sel = input["selector"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("`click` needs a `selector`.")?;
                s.click(sel).await?;
                format!("Clicked `{}`", short(sel))
            }
            "type" => {
                let text = input["text"].as_str().ok_or("`type` needs `text`.")?;
                s.type_text(text).await?;
                format!("Typed {} characters", text.chars().count())
            }
            "js" => {
                let expr = input["js"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("`js` needs a `js` expression.")?;
                let v = s.eval(expr).await?;
                return Ok(text_result(&v, "That expression returned nothing (null, undefined, or no value)."));
            }
            "back" => {
                // The history entry itself isn't worth reading back: `go back`
                // and the resulting page are the whole answer, and the state
                // call below reports where it landed.
                s.eval("history.back(); 'going back'").await.ok();
                s.settle().await?;
                let st = s.state().await?;
                format!("Back to {}", short(&st.url))
            }
            other => return Err(format!("Unknown browser action `{other}`. Use navigate, read, screenshot, click, type, js, back or info.")),
        };
        let st = s.state().await.unwrap_or_default();
        let where_ = if s.remembered().url.is_empty() {
            String::new()
        } else {
            format!(" in {}", s.browser())
        };
        Ok(json!(format!(
            "{step}{where_}\n\nNow at: {}\nTitle: {}",
            st.url, st.title
        )))
    }

    /// Shared UI tagging for image tools: meta + text preview now,
    /// `images` data-URIs are backfilled by exec() from the content array.
    async fn finish_image(&self, item_id: Option<&str>, display: &str, img: &tools::ImageData) {
        let content = tools::image_content(display, img);
        let short_mime = img
            .media_type
            .strip_prefix("image/")
            .unwrap_or(&img.media_type);
        let meta = if img.bytes >= 1024 * 1024 {
            format!(
                "{short_mime} · {:.1} MiB",
                img.bytes as f64 / (1024.0 * 1024.0)
            )
        } else if img.bytes >= 1024 {
            format!("{short_mime} · {:.0} KiB", img.bytes as f64 / 1024.0)
        } else {
            format!("{short_mime} · {} bytes", img.bytes)
        };
        let preview = tools::tool_text(&content);
        self.set_item(item_id, |i| {
            i.data["meta"] = json!(meta);
            if i.data["output"].is_null() {
                i.data["output"] = json!(preview);
            }
        })
        .await;
    }

    async fn set_item(&self, item_id: Option<&str>, f: impl FnOnce(&mut Item)) {
        if let Some(iid) = item_id {
            self.h.patch_in(&self.task_id, self.sub_id(), iid, f).await;
        }
    }

    /// Publish a step for the UI's "what is it doing" line. Emits only when the
    /// line actually changes: a chat waiting on a subagent calls this once a
    /// second with the same text, and each call used to serialize and ship the
    /// whole task summary (every subagent report included) to the frontend.
    async fn step(&self, s: String) {
        let sid = self.sub_id().map(String::from);
        self.h
            .update_task_quiet(&self.task_id, |t| match &sid {
                None => {
                    if t.todos.iter().all(|x| x.status != "in_progress") {
                        t.step = s;
                    }
                }
                Some(id) => {
                    if let Some(x) = t.subs.iter_mut().find(|x| &x.id == id) {
                        x.meta = s;
                    }
                }
            })
            .await;
    }

    pub(crate) fn counts_for_doom_loop(name: &str) -> bool {
        !permissions::is_read_only_tool(name)
            && !matches!(name, "edit_file" | "multi_edit" | "write_file")
    }

    fn repeat_key(sub_id: Option<&str>, name: &str, input: &Value) -> String {
        format!("{}:{name}:{input}", sub_id.unwrap_or("main"))
    }

    async fn gate(&self, name: &str, input: &Value, path: Option<&str>) -> Result<(), String> {
        let h = &self.h;
        let t = h.task(&self.task_id).await?;
        let (plan, cwd, project) = {
            let t = t.lock().await;
            (t.plan, t.cwd_for(self.sub_id()).0, t.project.clone())
        };
        if let Some(s) = &self.sub {
            if let Some(reason) = subagent_file_restriction_denial(&s.def, name, path, &cwd) {
                return Err(reason);
            }
        }
        let allow: Vec<AllowRule> = h.settings.read().await.allow.clone();
        let perm = t.lock().await.perm.clone();
        let ctx = permissions::Ctx {
            perm: &perm,
            plan,
            cwd: &cwd,
            project: &project,
            allow: &allow,
        };
        let base = permissions::check(name, input, path, &ctx);
        // Hard policy denials always win over the loop detector's ask threshold.
        if let Decision::Deny(reason) = &base {
            return Err(reason.clone());
        }
        // File operations have their own per-call permission and safety checks;
        // repeating one must not create the generic repeated-call approval prompt.
        // Read-only tools and direct file editors neither increment nor prompt.
        if !Self::counts_for_doom_loop(name) {
            return Ok(());
        }
        let repeat_key = Self::repeat_key(self.sub_id(), name, input);
        let repeats = {
            let rt = h.runtime(&self.task_id);
            let mut seen = rt.repeats.lock().await;
            let count = seen.entry(repeat_key.clone()).or_insert(0);
            let old = *count;
            *count = count.saturating_add(1);
            old
        };
        let decision = permissions::check_with_repeats(name, input, path, &ctx, repeats);
        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny(r) => Err(r),
            Decision::Ask {
                title,
                detail,
                reason,
                rule,
            } => {
                let kind = if name == "bash" || name == "shell" {
                    "command"
                } else if name.starts_with("mcp__") || name == "github" || name == "computer" {
                    "mcp"
                } else {
                    "edit"
                };
                let item = Item::new(
                    "approval",
                    title.clone(),
                    json!({"kind": kind, "tool": name, "title": title, "detail": detail, "reason": reason, "rule": rule, "repeat": reason.starts_with("This is occurrence "), "sub": self.sub.as_ref().map(|s| s.def.name.clone())}),
                );
                let aid = item.id.clone();
                h.upsert_item(&self.task_id, item).await;
                let resp = h
                    .wait_for_user(&self.task_id, &aid, "approval", &self.cancel)
                    .await;
                let Some(resp) = resp else {
                    h.patch_item(&self.task_id, &aid, |i| {
                        i.data["resolved"] = json!("cancelled")
                    })
                    .await;
                    return Err("Interrupted by user.".into());
                };
                let decision = resp["decision"].as_str().unwrap_or("deny").to_string();
                let feedback = resp["feedback"].as_str().unwrap_or("").trim().to_string();
                h.patch_item(&self.task_id, &aid, |i| {
                    i.data["resolved"] = json!(decision);
                    if !feedback.is_empty() {
                        i.data["feedback"] = json!(feedback);
                    }
                })
                .await;
                match decision.as_str() {
                    "always" => {
                        h.runtime(&self.task_id)
                            .repeats
                            .lock()
                            .await
                            .remove(&repeat_key);
                        if let Some(rule) = rule {
                            let mut s = h.settings.write().await;
                            if !s.allow.iter().any(|r| r.pattern == rule) {
                                s.allow.push(AllowRule {
                                    pattern: rule,
                                    project: project.clone(),
                                });
                            }
                            store::save_settings(&s);
                        } else if kind == "edit" {
                            h.update_task(&self.task_id, approve_auto_edits).await;
                        }
                        Ok(())
                    }
                    "once" => {
                        h.runtime(&self.task_id)
                            .repeats
                            .lock()
                            .await
                            .remove(&repeat_key);
                        Ok(())
                    }
                    _ => Err(if feedback.is_empty() {
                        "The user denied this action. Don't retry it; ask or choose a different approach.".into()
                    } else {
                        format!("The user denied this action and said: {feedback}")
                    }),
                }
            }
        }
    }

    /// Remember what a file held before the agent's first edit of it, so the
    /// review view can show a diff and revert a single file. Rewinding a
    /// message does *not* use this — it only rewinds the conversation.
    async fn snapshot_file(&self, key: &str, path: &Path) {
        let Ok(t) = self.h.task(&self.task_id).await else {
            return;
        };
        let mut t = t.lock().await;
        let original = std::fs::read_to_string(path).ok();
        t.touched.entry(key.to_string()).or_insert(original);
    }

    async fn exec_inner(
        &self,
        name: &str,
        input: &Value,
        item_id: Option<&str>,
        call_id: &str,
    ) -> Result<String, String> {
        // Wire-only aliases for the OpenCode free-tier gatekeeper (`read` /
        // `shell` are injected in providers.rs so the gateway sees an
        // official-looking tool list). They run the real implementations.
        let name = match name {
            "read" => "read_file",
            "shell" => "bash",
            n => n,
        };
        let h = &self.h;
        let t = h.task(&self.task_id).await?;
        let (cwd, project) = {
            let task = t.lock().await;
            (task.cwd_for(self.sub_id()).0, task.project.clone())
        };
        let path_of = |k: &str| input[k].as_str().map(|p| tools::resolve(&cwd, p));
        // Tool sets are fixed for caching; enforce per-agent limits here.
        if let Some(s) = &self.sub {
            if name == "artifact_preview" {
                return Err(format!(
                    "The {} agent can't use artifact_preview; only the main agent can attach inline previews.",
                    s.def.id
                ));
            }
            let writes = matches!(
                name,
                "edit_file"
                    | "multi_edit"
                    | "write_file"
                    | "kill_bash"
                    | "artifact_create"
                    | "artifact_revise"
                    | "artifact_respond"
            );
            if (s.def.tools == "read_only" && (writes || name.starts_with("mcp__")))
                || (s.def.tools == "no_shell" && matches!(name, "bash" | "kill_bash"))
            {
                return Err(format!("The {} agent can't use {name}.", s.def.id));
            }
            if s.def.tools == "read_only" && name == "bash" {
                let cmd = input["command"].as_str().unwrap_or("");
                if !permissions::read_only_command(cmd)
                    || input["run_in_background"].as_bool().unwrap_or(false)
                {
                    return Err(format!("The {} agent may only run read-only commands (git log/diff/show/blame, ls, wc, …). `{}` isn't one.", s.def.id, cmd.chars().take(80).collect::<String>()));
                }
            }
        }
        let path = if matches!(
            name,
            "read_file" | "view_image" | "edit_file" | "multi_edit" | "write_file"
        ) {
            Some(path_of("path").ok_or("path is required")?)
        } else if matches!(
            name,
            "artifact_create" | "artifact_revise" | "artifact_respond"
        ) {
            // Artifact storage is rooted at the task's original project, not its
            // cwd: the cwd may be an isolated worktree. Reuse that fixed project
            // path for the normal permission gate instead of accepting a model
            // supplied path (or dropping the permission scope entirely).
            Some(Path::new(&project).to_path_buf())
        } else {
            None
        };
        match name {
            "read_file" => {
                let p = path.as_ref().ok_or("path is required")?;
                self.gate(name, input, Some(&p.to_string_lossy())).await?;
                self.step(format!("Reading {}", permissions::short(&p.to_string_lossy(), &cwd))).await;
                let (out, total) = tools::read_file(p, input["offset"].as_u64(), input["limit"].as_u64())?;
                t.lock().await.read_files.insert(tools::path_key(p), tools::mtime(p));
                self.set_item(item_id, |i| i.data["meta"] = json!(format!("{total} lines"))).await;
                Ok(out)
            }
            "artifact_preview" => {
                self.gate(name, input, None).await?;
                let preview = tools::validate_artifact_preview(input)?;
                // The originating tool row becomes a chat-only card only after
                // complete tool input has passed validation. Streamed input
                // fragments remain inert draft metadata and never enter a frame.
                let card = json!({
                    "title": preview.title,
                    "kind": preview.kind,
                    "persistence": "session"
                });
                self.set_item(item_id, |i| i.data["artifact_card"] = card)
                    .await;
                Ok("Temporary inline preview attached to this chat.".into())
            }
            "artifact_list" => {
                self.gate(name, input, None).await?;
                let artifacts = crate::artifacts::list(&project)?;
                serde_json::to_string_pretty(&artifacts).map_err(|e| format!("Could not format artifact list: {e}"))
            }
            "artifact_get" => {
                self.gate(name, input, None).await?;
                let id = input["id"].as_str().filter(|id| !id.trim().is_empty()).ok_or("id is required")?;
                let artifact = crate::artifacts::get_for_agent(&project, id)?;
                serde_json::to_string_pretty(&artifact).map_err(|e| format!("Could not format artifact: {e}"))
            }
            "artifact_feedback_list" => {
                self.gate(name, input, None).await?;
                let artifact_id = input["artifact_id"].as_str().filter(|id| !id.trim().is_empty());
                let feedback = crate::artifacts::feedback_list_for_agent(&project, artifact_id)?;
                serde_json::to_string_pretty(&feedback).map_err(|e| format!("Could not format artifact feedback: {e}"))
            }
            "artifact_create" => {
                let path = path.as_ref().ok_or("artifact project is required")?;
                self.gate(name, input, Some(&path.to_string_lossy())).await?;
                let payload = artifact_input(input)?;
                let artifact = crate::artifacts::create(&project, payload)?;
                let agent_view = crate::artifacts::AgentArtifact::from(&artifact);
                let output = serde_json::to_string_pretty(&agent_view)
                    .map_err(|e| format!("Could not format artifact: {e}"))?;
                let card = json!({
                    "title": artifact.title,
                    "kind": artifact.kind,
                    "persistence": "project",
                    "artifact_id": artifact.id,
                    "version_id": artifact.current_version_id
                });
                self.set_item(item_id, |i| i.data["artifact_card"] = card)
                    .await;
                Ok(output)
            }
            "artifact_revise" => {
                let path = path.as_ref().ok_or("artifact project is required")?;
                self.gate(name, input, Some(&path.to_string_lossy())).await?;
                let id = input["id"].as_str().filter(|id| !id.trim().is_empty()).ok_or("id is required")?;
                let parent = input["parent_version_id"].as_str().filter(|id| !id.trim().is_empty()).ok_or("parent_version_id is required; read the artifact's current_version_id first")?;
                let payload = artifact_input(input)?;
                let artifact = crate::artifacts::revise(&project, id, parent, payload)?;
                let agent_view = crate::artifacts::AgentArtifact::from(&artifact);
                let output = serde_json::to_string_pretty(&agent_view)
                    .map_err(|e| format!("Could not format artifact: {e}"))?;
                let card = json!({
                    "title": artifact.title,
                    "kind": artifact.kind,
                    "persistence": "project",
                    "artifact_id": artifact.id,
                    "version_id": artifact.current_version_id
                });
                self.set_item(item_id, |i| i.data["artifact_card"] = card)
                    .await;
                Ok(output)
            }
            "artifact_respond" => {
                let path = path.as_ref().ok_or("artifact project is required")?;
                self.gate(name, input, Some(&path.to_string_lossy())).await?;
                let feedback_id = input["feedback_id"].as_str().filter(|id| !id.trim().is_empty()).ok_or("feedback_id is required")?;
                let decision = input["decision"].as_str().ok_or("decision is required")?;
                if !matches!(decision, "addressed" | "needs_clarification") {
                    return Err("decision must be addressed or needs_clarification".into());
                }
                // This tool can only address a handoff the user explicitly
                // submitted. Do not let a guessed id expose or modify draft feedback.
                let submitted = crate::artifacts::feedback_list_for_agent(&project, None)?;
                if !submitted.iter().any(|f| f.id == feedback_id) {
                    return Err("That feedback is not available to the agent; only user-submitted feedback can be answered.".into());
                }
                let response = input["response"].as_str().ok_or("response is required")?;
                let artifact = crate::artifacts::feedback_respond(&project, feedback_id, decision, response)?;
                let agent_view = crate::artifacts::AgentArtifact::from(&artifact);
                serde_json::to_string_pretty(&agent_view)
                    .map_err(|e| format!("Could not format artifact: {e}"))
            }
            "tool_search" if self.is_main() => {
                let task = self.h.task(&self.task_id).await?;
                let (mcp, plugins) = { let t = task.lock().await; (t.mcp_tools.clone(), t.plugins.clone()) };
                self.tool_search(input, &mcp, &plugins).await
            },
            "tool_search" => Err("Only the main agent can load deferred tools.".into()),
            "view_image" => {
                let p = path.as_ref().ok_or("path is required")?;
                self.gate(name, input, Some(&p.to_string_lossy())).await?;
                self.exec_view_image(input, item_id).await.map(|v| tools::tool_text(&v))
            }
            "glob" => {
                let base = path_of("path").unwrap_or_else(|| cwd.clone().into());
                let out = tools::glob(&base, input["pattern"].as_str().unwrap_or("*"))?;
                let n = if out == "No files found" { 0 } else { out.lines().count() };
                self.set_item(item_id, |i| i.data["meta"] = json!(format!("{n} files"))).await;
                Ok(out)
            }
            "grep" => {
                let base = path_of("path").unwrap_or_else(|| cwd.clone().into());
                self.step(format!("Searching for {}", input["pattern"].as_str().unwrap_or(""))).await;
                let (out, n) = tools::grep(&base, input)?;
                self.set_item(item_id, |i| i.data["meta"] = json!(format!("{n} match{}", if n == 1 { "" } else { "es" }))).await;
                Ok(out)
            }
            "web_fetch" => {
                let url = input["url"].as_str().unwrap_or("");
                self.step(format!("Fetching {url}")).await;
                tools::web_fetch(&h.http, url, input["offset"].as_u64().unwrap_or(0) as usize).await
            }
            "web_search" => {
                let q = input["query"].as_str().unwrap_or("");
                self.step(format!("Searching the web for {q}")).await;
                let count = input["count"].as_u64().unwrap_or(5) as usize;
                let out = tools::web_search(&h.http, q, count).await?;
                let n = out.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit()) && l.contains(". ")).count();
                self.set_item(item_id, |i| i.data["meta"] = json!(format!("{n} results"))).await;
                Ok(out)
            }
            "todo_write" => {
                if !self.is_main() { return Err("Subagents cannot change the main task's todo list.".into()); }
                let todos = parse_todos(input)?;
                h.update_task(&self.task_id, |t| {
                    if let Some(a) = todos.iter().find(|x| x.status == "in_progress") {
                        t.step = if a.active_form.is_empty() { a.content.clone() } else { a.active_form.clone() };
                    }
                    t.todos = todos.clone();
                })
                .await;
                Ok("Todo list updated. Keep it current as you work.".into())
            }
            "ask_user" if self.is_main() => self.ask_user(input, None).await,
            "ask_nonblocking" if self.is_main() => self.ask_nonblocking(input).await,
            "notify_user" if self.is_main() => {
                self.gate(name, input, None).await?;
                self.notify_user(input).await
            }
            "notify_user" => Err("Only the main agent can notify the user.".into()),
            "ask" if !self.is_main() => self.sub_ask(input).await,
            "goal_complete" if self.is_main() => self.goal_complete(input).await,
            "start_ultrathread" if self.is_main() => self.start_ultra(input).await,
            "exit_plan_mode" if self.is_main() => {
                if !t.lock().await.plan {
                    return Err("Plan mode is not active; just proceed with the work.".into());
                }
                self.exit_plan(input).await
            }
            "edit_file" | "multi_edit" | "write_file" => {
                let p = path_of("path").ok_or("path is required")?;
                let key = tools::path_key(&p);
                let exists = p.exists();
                // Read-before-write + staleness: never clobber what the model hasn't seen.
                if exists {
                    let seen = t.lock().await.read_files.get(&key).copied();
                    match seen {
                        None => return Err(format!("You must read {} with read_file before {}.", p.display(), if name == "write_file" { "overwriting it" } else { "editing it" })),
                        Some(m) if tools::mtime(&p) > m => return Err(format!("{} was modified since you last read it (by the user or a command). Read it again before changing it.", p.display())),
                        _ => {}
                    }
                } else if name != "write_file" {
                    return Err(format!("{} does not exist. Use write_file to create it.", p.display()));
                }
                let old = if exists { std::fs::read_to_string(&p).map_err(|e| format!("cannot read: {e}"))? } else { String::new() };
                let new = if name == "edit_file" {
                    tools::apply_edit(&old, input["old_string"].as_str().unwrap_or(""), input["new_string"].as_str().unwrap_or(""), input["replace_all"].as_bool().unwrap_or(false))?.new_content
                } else if name == "multi_edit" {
                    let edits = input["edits"].as_array().filter(|e| !e.is_empty()).ok_or("edits is required (at least one)")?;
                    let mut cur = old.clone();
                    for (i, e) in edits.iter().enumerate() {
                        cur = tools::apply_edit(&cur, e["old_string"].as_str().unwrap_or(""), e["new_string"].as_str().unwrap_or(""), e["replace_all"].as_bool().unwrap_or(false))
                            .map_err(|err| format!("Edit {} of {} failed, nothing was written: {err}", i + 1, edits.len()))?
                            .new_content;
                    }
                    cur
                } else {
                    input["content"].as_str().ok_or("content is required")?.to_string()
                };
                let (lines, add, del) = tools::diff_lines(&old, &new);
                self.set_item(item_id, |i| {
                    i.data["diff"] = json!(lines);
                    i.data["meta"] = json!(format!("+{add} −{del}"));
                })
                .await;
                self.gate(name, input, Some(&p.to_string_lossy())).await?;
                self.step(format!("Editing {}", permissions::short(&p.to_string_lossy(), &cwd))).await;
                self.snapshot_file(&key, &p).await;
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create directory: {e}"))?;
                }
                std::fs::write(&p, &new).map_err(|e| format!("write failed: {e}"))?;
                // Record ownership only after the write succeeded. The shadow
                // store must never infer that a whole-turn diff belongs to us:
                // shell commands, external tools and the user can all change files
                // between snapshots.
                let checkpoint_enabled = h.settings.read().await.checkpoints;
                if checkpoint_enabled {
                    let cp = p.clone();
                    let task_id = self.task_id.clone();
                    let worktree = cwd.clone();
                    let (items, messages) = {
                        let t = t.lock().await;
                        (t.items.len(), t.messages.len())
                    };
                    let _ = tokio::task::spawn_blocking(move || checkpoint::record_agent_write(&task_id, &worktree, items, messages, &cp)).await;
                }
                let key = tools::path_key(&p);
                t.lock().await.read_files.insert(key, tools::mtime(&p));
                Ok(if exists { format!("Updated {} (+{add} −{del}).", p.display()) } else { format!("Created {} ({} lines).", p.display(), new.lines().count()) })
            }
            "bash" => {
                let cmd = input["command"].as_str().ok_or("command is required")?.to_string();
                self.gate(name, input, None).await?;
                let desc = input["description"].as_str().map(String::from).unwrap_or_else(|| format!("Running {}", cmd.chars().take(60).collect::<String>()));
                // Paused (e.g. mid-batch): nothing new starts until resumed.
                if !self.h.wait_unpaused(&self.task_id, &self.cancel).await {
                    return Err("Interrupted by user before this tool ran.".into());
                }
                self.step(desc).await;
                if input["run_in_background"].as_bool().unwrap_or(false) {
                    let id = h.bg.spawn(&self.task_id, &cmd, &cwd)?;
                    self.h.emit(&self.task_id, "bg", json!(h.bg.list(&self.task_id)));
                    if let Some(job) = h.bg.get(&id) {
                        let (h2, tid) = (self.h.clone(), self.task_id.clone());
                        tokio::spawn(async move {
                            job.done.cancelled().await;
                            h2.refresh_busy(&tid).await;
                        });
                    }
                    self.h.refresh_busy(&self.task_id).await;
                    // The job id on the item, not only in the text below: the UI has to
                    // tell a background command that has since exited from one still
                    // running, and that is a join against the live job list.
                    self.set_item(item_id, |i| {
                        i.data["meta"] = json!("background");
                        i.data["bg_id"] = json!(id);
                    })
                    .await;
                    // Give it a moment so early failures surface immediately.
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    let (out, exit) = h.bg.read_new(&id).unwrap_or_default();
                    self.h.emit(&self.task_id, "bg", json!(h.bg.list(&self.task_id)));
                    return Ok(format!("Started background command {id}.{}\n{}", exit.map(|e| format!(" It already exited ({e}).")).unwrap_or_default(), out));
                }
                let timeout = input["timeout_ms"].as_u64().unwrap_or(120_000).clamp(1_000, 600_000);
                // Stops on Esc (self.cancel) or a force pause (rt.force), whichever comes first.
                let rt = self.h.runtime(&self.task_id);
                let force = rt.force.lock().unwrap().clone();
                let tok = self.cancel.child_token();
                let watch = {
                    let (tok, force) = (tok.clone(), force.clone());
                    tokio::spawn(async move {
                        tokio::select! { _ = force.cancelled() => tok.cancel(), _ = tok.cancelled() => {} }
                    })
                };
                // A guard, not a bare add/sub pair: if this future is dropped while
                // the command runs (an aborted turn, a cancelled batch) a trailing
                // fetch_sub never executes, and the leaked count reads as "waiting on
                // 1 command to finish" for the rest of the session.
                let fg_guard = FgGuard::new(rt.clone());
                self.h.refresh_busy(&self.task_id).await;
                // The row is the only place the UI can tell a command that is still
                // executing from a turn the pause has frozen. A pause blocks at the
                // `wait_unpaused` above, *before* this point: anything past it is
                // already running and will run to completion whatever the user does
                // next, which is why the banner counts it as in flight. Without this
                // flag the row draws the still frozen mark over a live `cargo build`,
                // so the transcript says "paused" about a process that is plainly
                // still going. Cleared below, and by `exec` on the way out.
                self.set_item(item_id, |i| i.data["exec"] = json!(true)).await;
                let r = super::shell::run(&cmd, &cwd, timeout, &tok).await;
                drop(fg_guard);
                watch.abort();
                self.set_item(item_id, |i| i.data["exec"] = json!(false)).await;
                self.h.refresh_busy(&self.task_id).await;
                let r = r?;
                let forced = r.interrupted && force.is_cancelled() && !self.cancel.is_cancelled();
                let meta = if r.timed_out { "timed out".into() } else if forced { "force paused".into() } else if r.interrupted { "interrupted".into() } else { format!("exit {}", r.code.map(|c| c.to_string()).unwrap_or("?".into())) };
                let out_c = r.output.clone();
                self.set_item(item_id, |i| {
                    i.data["meta"] = json!(meta);
                    i.data["output"] = json!(out_c);
                })
                .await;
                let mut s = r.output;
                if r.timed_out {
                    s.push_str(&format!("\n[Command timed out after {}ms and was killed. For long-running processes use run_in_background.]", timeout));
                } else if forced {
                    return Err(format!("{s}\n[Stopped by a force pause before it finished — the command did NOT complete. Run it again.]"));
                } else if r.interrupted {
                    return Err(format!("{s}\n[Interrupted by user]"));
                } else if r.code != Some(0) {
                    return Err(format!("{s}\n[exit code {}]", r.code.map(|c| c.to_string()).unwrap_or("unknown".into())));
                }
                Ok(s)
            }
            "bash_output" => {
                let id = input["id"].as_str().unwrap_or("");
                let (out, exit) = h.bg.read_new(id).ok_or(format!("No background command {id}"))?;
                self.h.emit(&self.task_id, "bg", json!(h.bg.list(&self.task_id)));
                Ok(format!("[{}]\n{}", exit.unwrap_or("running".into()), if out.is_empty() { "(no new output)".into() } else { out }))
            }
            "kill_bash" => {
                let id = input["id"].as_str().unwrap_or("");
                let ok = h.bg.kill(id);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                self.h.emit(&self.task_id, "bg", json!(h.bg.list(&self.task_id)));
                if ok {
                    Ok(format!("Stopped {id}"))
                } else {
                    Err(format!("No background command {id}"))
                }
            }
            "github" => {
                let cfg = h.settings.read().await.plugins.github.clone();
                if !cfg.enabled {
                    return Err("The GitHub plugin is turned off (Settings → Plugins).".into());
                }
                let write = super::plugins::github_is_write(input);
                if write {
                    if self.sub.as_ref().is_some_and(|s| s.def.tools == "read_only") {
                        return Err("This agent is read-only: it can look things up on GitHub but not change them.".into());
                    }
                    self.gate(name, input, None).await?;
                }
                let action = input["action"].as_str().unwrap_or("").replace('_', " ");
                self.step(format!("GitHub: {action}")).await;
                let (token, _) = tokio::task::spawn_blocking(move || super::plugins::github_token(&cfg))
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or("No GitHub token. Add one in Settings → Plugins → GitHub, set GH_TOKEN, or run `gh auth login`.")?;
                let gh = super::plugins::Gh { http: &h.http, token, cwd: &cwd };
                let out = tokio::select! {
                    r = gh.run(input) => r?,
                    _ = self.cancel.cancelled() => return Err("Interrupted by user.".into()),
                };
                self.set_item(item_id, |i| i.data["meta"] = json!(if write { "done" } else { "read" })).await;
                Ok(out)
            }
            "set_title" => self.set_title(input).await,
            "memory" if self.is_main() => self.memory(input).await,
            "web_search_deep" if self.is_main() => {
                let queries: Vec<String> = input["queries"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                let question = input["question"].as_str().unwrap_or("").trim().to_string();
                let read = input["read"].as_u64().unwrap_or(4) as usize;
                self.step(format!("Deep research: {} quer{}", queries.len(), if queries.len() == 1 { "y" } else { "ies" })).await;
                let out = tokio::select! {
                    r = tools::web_search_deep(&h.http, &queries, &question, read) => r?,
                    _ = self.cancel.cancelled() => return Err(INTERRUPTED.into()),
                };
                self.set_item(item_id, |i| i.data["meta"] = json!(format!("{} queries", queries.len()))).await;
                Ok(out)
            }
            "web_read_many" if self.is_main() => {
                let urls: Vec<String> = input["urls"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                self.step(format!("Reading {} pages", urls.len())).await;
                let out = tokio::select! {
                    r = tools::web_read_many(&h.http, &urls, input["offset"].as_u64().unwrap_or(0) as usize) => r?,
                    _ = self.cancel.cancelled() => return Err(INTERRUPTED.into()),
                };
                self.set_item(item_id, |i| i.data["meta"] = json!(format!("{} pages", urls.len()))).await;
                Ok(out)
            }
            "list_agents" => self.list_agents().await,
            "send_message" => self.send_message(input).await,
            "wait" => self.wait_for(input).await,
            // Park the task on a trigger. Only the main agent: a sub-agent that
            // parks would leave its parent waiting on a `task` result that never
            // comes, which is a hang dressed as a feature.
            "wait_for_event" if self.is_main() => self.wait_for_event(input).await,
            "wait_for_event" => {
                Err("Only the main agent can park the task. Finish your job and report back.".into())
            }
            "task" if self.is_main() || self.can_nest().await => self.spawn_sub(input, call_id).await,
            "task_resume" if self.is_main() || self.can_nest().await => self.resume_sub(input).await,
            "task_status" if self.is_main() || self.can_nest().await => self.task_status(input).await,
            n if n.starts_with("mcp__") => {
                self.gate(n, input, None).await?;
                self.step(format!("Using {}", n.replacen("mcp__", "", 1).replace("__", " "))).await;
                h.mcp.call(n, input.clone(), &self.cancel).await
            }
            other => Err(format!("There's no `{other}` tool. Use one of the tools you were given (e.g. read_file, grep, glob, edit_file, bash); for anything else, say what you need.")),
        }
    }

    /// Whether the agent may name this chat right now, and why not if it may
    /// not. Split out from `set_title` so the rule has one home and the lock
    /// can be exercised without a model in the loop.
    async fn rename_check(&self, want: &str) -> Result<(), String> {
        rename_check(&self.h, &self.task_id, want).await
    }

    /// The agent renaming its own chat. Two rules are the user's, not the
    /// model's, so they are enforced here rather than left to the prompt: the
    /// user turned this on, and the user hasn't claimed the name themselves.
    async fn set_title(&self, input: &Value) -> Result<String, String> {
        if !self.is_main() {
            return Err("Only the main agent names the chat; sub-agents don't.".into());
        }
        let raw: String = input["title"]
            .as_str()
            .unwrap_or("")
            .trim()
            .chars()
            .take(60)
            .collect();
        // Models like to wrap a short label in quotes or end it with a period.
        let want = raw
            .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == ':')
            .trim();
        if want.chars().count() < 2 {
            return Err("A title needs at least 2 characters.".into());
        }
        if let Err(why) = self.rename_check(want).await {
            return Err(format!("{why} Keep working."));
        }
        let want = want.to_string();
        self.h
            .update_task(&self.task_id, |t| t.title = want.clone())
            .await;
        self.h.save_task(&self.task_id).await;
        Ok(format!("Chat renamed to \"{want}\"."))
    }

    /// Read and write this project's memory (`~/.openleash/memory/<project>/`).
    ///
    /// The index (`MEMORY.md`) is the only part loaded at the start of a
    /// conversation, so it is capped: the first 200 lines or 25KB, whichever
    /// comes first, and anything past that is dropped at the next load. Near
    /// the cap we warn the model to compact the index; over it we refuse the
    /// write, because a note that is written and then silently never loaded
    /// again is worse than a refused one.
    async fn memory(&self, input: &Value) -> Result<String, String> {
        if !self.h.settings.read().await.memory {
            return Err("Memory is turned off in Settings → General.".into());
        }
        let dir = super::memory::dir_for(&self.h.task(&self.task_id).await?.lock().await.project);
        let _ = std::fs::create_dir_all(&dir);
        let action = input["action"].as_str().unwrap_or("").trim();
        match action {
            "recall" => {
                let st = super::memory::open(&dir);
                // No `file`: show the index — either all of it, or the lines
                // matching the query, so a targeted recall stays small.
                let file = input["file"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if let Some(f) = file {
                    if f.contains('/') || f.contains('\\') || f.starts_with('.') {
                        return Err(
                            "`file` is a bare filename from the index, like `feedback_testing.md`."
                                .into(),
                        );
                    }
                    let body = super::memory::body(&dir, f)?;
                    return Ok(format!("Memory `{f}`:\n\n{body}"));
                }
                if st.entries.is_empty() {
                    return Ok(format!(
                        "No memories saved for this project yet ({}).",
                        dir.display()
                    ));
                }
                let q: Vec<String> = input["query"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .split_whitespace()
                    .map(String::from)
                    .collect();
                let hits: Vec<&super::memory::Entry> = st
                    .entries
                    .iter()
                    .filter(|e| {
                        q.is_empty() || q.iter().any(|w| e.line().to_lowercase().contains(w))
                    })
                    .collect();
                if q.is_empty() {
                    let index = std::fs::read_to_string(dir.join("MEMORY.md")).unwrap_or_default();
                    return Ok(format!(
                        "{} memor{} for this project ({}). The index is {}/{} lines, {}/{} bytes — only the first {} lines load each session, so read a file when you need the detail.\n\n{}",
                        st.entries.len(),
                        if st.entries.len() == 1 { "y" } else { "ies" },
                        dir.display(),
                        index.lines().count(),
                        super::memory::INDEX_LINES,
                        index.len(),
                        super::memory::INDEX_BYTES,
                        super::memory::INDEX_LINES,
                        st.entries.iter().map(|e| format!("  {}\n", e.line())).collect::<String>()
                    ));
                }
                if hits.is_empty() {
                    return Ok(format!(
                        "No memory matches {:?}. {} memories are saved for this project.",
                        input["query"].as_str().unwrap_or(""),
                        st.entries.len()
                    ));
                }
                Ok(format!(
                    "{} matching memor{}:\n{}\nRead one with the memory tool (`action` `recall`, `file` set to the filename).",
                    hits.len(),
                    if hits.len() == 1 { "y" } else { "ies" },
                    hits.iter().map(|e| format!("  {}\n", e.line())).collect::<String>()
                ))
            }
            "forget" => {
                let file = input["file"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("`file` is required: the memory's filename from the index, e.g. `feedback_testing.md`.")?;
                if file.contains('/') || file.contains('\\') || file.starts_with('.') {
                    return Err(
                        "`file` is a bare filename from the index, like `feedback_testing.md`."
                            .into(),
                    );
                }
                let mut st = super::memory::open(&dir);
                let Some(entry) = st.entries.iter().find(|e| e.file == file).cloned() else {
                    return Err(format!(
                        "No memory with the file `{file}`. Recall the index to see what's saved."
                    ));
                };
                super::memory::remove(&dir, file)?;
                st.entries.retain(|e| e.file != file);
                super::memory::write_index(&dir, &st.entries)?;
                Ok(format!(
                    "Forgot \"{}\" and removed it from the index.",
                    entry.name
                ))
            }
            "save" => self.memory_save(input, &dir).await,
            other => Err(format!(
                "Unknown memory action `{other}`. Use `save`, `recall` or `forget`."
            )),
        }
    }

    /// Write one memory, and keep the index honest.
    ///
    /// Two things are enforced rather than suggested: the index must have room,
    /// and the summary must be one line. Both exist because the index is the
    /// only memory an agent reads for free — a memory that bloats it is paid
    /// for by every future session, and a summary that wraps defeats the whole
    /// point of the format.
    async fn memory_save(&self, input: &Value, dir: &std::path::Path) -> Result<String, String> {
        let kind = input["kind"].as_str().unwrap_or("").trim();
        if !super::memory::KINDS.contains(&kind) {
            return Err(format!("`kind` must be one of: {}. Pick the one that fits — `user` (who they are, how they work), `feedback` (a correction they gave), `project` (ongoing work and decisions), `reference` (where to find things outside the repo).", super::memory::KINDS.join(", ")));
        }
        let name = input["name"].as_str().unwrap_or("").trim();
        if name.chars().count() < 2 || name.chars().count() > 60 {
            return Err("`name` is a short title, 2-60 characters.".into());
        }
        let summary = input["summary"].as_str().unwrap_or("").trim();
        let content = input["content"].as_str().unwrap_or("").trim();
        if summary.is_empty() || content.is_empty() {
            return Err(
                "A memory needs both a `summary` (one line) and `content` (the detail).".into(),
            );
        }
        // A summary that wraps breaks the index format and the line budget.
        if summary.contains('\n') {
            return Err("`summary` must be a single line — it is what shows in the index. Put the detail in `content`.".into());
        }
        let st = super::memory::open(dir);
        // Refuse to file a near-duplicate: two memories saying the same thing
        // is the main way a memory folder rots.
        if let Some(dup) = st
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("There is already a memory called \"{}\" ({}). Add to it, or `forget` it and save a new one.", dup.name, dup.file));
        }
        let file = super::memory::file_for(dir, name).ok_or("That name has no letters or digits to build a filename from — give the memory a wordier name.")?;
        let index = std::fs::read_to_string(dir.join("MEMORY.md")).unwrap_or_default();
        let (l, b) = super::memory::room(&index);
        // Past the cap, a write that appends is invisible: the note would be
        // written and never loaded again. Refuse and make the model compact.
        if l <= 0 || b <= 0 {
            return Err(format!(
                "This project's memory index is over its limit ({} lines / {} bytes, limit {} / {}), so anything appended now would be dropped at the next load. Use `recall` to see the index, `forget` the entries that are stale or wrong, or merge several memories into one and `forget` the rest. Then save again.",
                index.lines().count(),
                index.len(),
                super::memory::INDEX_LINES,
                super::memory::INDEX_BYTES
            ));
        }
        let near = l <= super::memory::NEAR || b <= super::memory::NEAR * 100;
        let entry = super::memory::Entry {
            kind: kind.to_string(),
            name: name.to_string(),
            summary: summary.to_string(),
            file: file.clone(),
        };
        let mut entries = st.entries;
        entries.push(entry.clone());
        // Body first, then the index. The other order would leave a note that
        // is saved but unindexed — invisible to every future session, while
        // the tool result claims success. If the index write fails here, the
        // body is rolled back so the folder stays consistent.
        super::memory::write_body(dir, &file, content)?;
        if let Err(e) = super::memory::write_index(dir, &entries) {
            let _ = std::fs::remove_file(dir.join(&file));
            return Err(format!("{e} The memory was not saved."));
        }
        Ok(if near {
            format!(
                "Saved the {kind} memory \"{name}\" as {file}.\nHeads up: the index is close to its limit ({} lines / {} bytes of {} / {}). Keep new summaries to one line, move detail into the content, and merge or forget stale entries.",
                index.lines().count() + 1,
                index.len() + entry.line().len() + 1,
                super::memory::INDEX_LINES,
                super::memory::INDEX_BYTES
            )
        } else {
            format!("Saved the {kind} memory \"{name}\" as {file}.")
        })
    }

    /// Who this agent is, as other agents see it.
    fn me(&self) -> String {
        match &self.sub {
            Some(s) => format!("{} subagent `{}`", s.def.id, s.id),
            None => "the main agent".into(),
        }
    }

    async fn list_agents(&self) -> Result<String, String> {
        let t = self.h.task(&self.task_id).await?;
        let (subs, main_running) = {
            let t = t.lock().await;
            (
                t.subs.clone(),
                t.status == "running" || t.status == "waiting",
            )
        };
        let you = |id: &str| {
            if self.sub_id() == Some(id) {
                " ← you"
            } else {
                ""
            }
        };
        let mut out = format!(
            "main · the main agent · {}{}\n",
            if main_running { "running" } else { "idle" },
            if self.is_main() { " ← you" } else { "" }
        );
        for s in &subs {
            out.push_str(&format!(
                "{} · {} subagent{} · {} · {}{}\n",
                s.id,
                s.role,
                if s.background { " (background)" } else { "" },
                s.task,
                s.status,
                you(&s.id)
            ));
        }
        let bg = self.h.bg.list(&self.task_id);
        if !bg.is_empty() {
            out.push_str("\nBackground commands:\n");
            for b in bg {
                out.push_str(&format!(
                    "{} · {} · {}\n",
                    b.id,
                    b.cmd.chars().take(80).collect::<String>(),
                    b.exit.unwrap_or("running".into())
                ));
            }
        }
        Ok(out)
    }

    async fn send_message(&self, input: &Value) -> Result<String, String> {
        let to = input["to"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("`to` is required: main, a subagent id, or all")?
            .to_string();
        let msg = input["message"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("`message` is required")?
            .to_string();
        let t = self.h.task(&self.task_id).await?;
        let subs = t.lock().await.subs.clone();
        let from = self.me();
        let note = format!("<system-reminder>Message from {from}:</system-reminder>\n{msg}");
        let mut sent: Vec<String> = vec![];
        let to_main = to == "main" || (to == "all" && !self.is_main());
        if to_main {
            if self.is_main() {
                return Err("You are the main agent; message a subagent id or `all`.".into());
            }
            deliver_to_main(&self.h, &self.task_id, note.clone()).await;
            sent.push("main".into());
        }
        for s in subs.iter().filter(|s| {
            s.status == "running"
                && Some(s.id.as_str()) != self.sub_id()
                && (to == "all" || to == s.id)
        }) {
            self.h
                .note(&self.task_id, &s.id, Kinded::agent(&note).0)
                .await;
            self.h
                .upsert_sub_item(
                    &self.task_id,
                    &s.id,
                    Item::new(
                        "notice",
                        format!("Message from {from}: {msg}"),
                        json!({"level": "msg"}),
                    ),
                )
                .await;
            sent.push(s.id.clone());
        }
        if sent.is_empty() {
            return Err(match subs.iter().find(|s| s.id == to) {
                Some(s) => format!("`{to}` isn't running ({}), so it can't receive messages. Continue its work yourself or launch a new subagent.", s.status),
                None if to == "all" => "No other agents are running.".into(),
                None => format!("No agent `{to}`. Use list_agents to see ids."),
            });
        }
        let label = format!(
            "{} → {}: {}",
            if let Some(s) = &self.sub {
                s.def.id.clone()
            } else {
                "main".to_string()
            },
            sent.join(", "),
            msg.chars().take(300).collect::<String>()
        );
        self.h
            .upsert_item(
                &self.task_id,
                Item::new("notice", label, json!({"level": "msg"})),
            )
            .await;
        Ok(format!(
            "Delivered to {} (they see it on their next step).",
            sent.join(", ")
        ))
    }

    /// Park the task until an external trigger fires (`wait_for_event`).
    ///
    /// The alternative it replaces is the model polling — `task_status` in a
    /// loop, or worse a `bash` sleep — which burns a request per poll and keeps
    /// a turn loop alive for however long CI takes. Parking ends the turn and
    /// hands the waiting to a background watcher that costs nothing until it
    /// fires. When it does, the wake is a normal `resume`, so the follow-up turn
    /// is indistinguishable from the user pressing Resume.
    async fn wait_for_event(&self, input: &Value) -> Result<String, String> {
        let source = input["source"].as_str().unwrap_or("").to_string();
        if !matches!(source.as_str(), "timer" | "ci" | "pr") {
            return Err("`source` must be one of: timer, ci, pr.".into());
        }
        let reason = input["reason"]
            .as_str()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or("`reason` is required — say what you are waiting for, in a few words.")?
            .to_string();
        let mut src = WakeSource {
            source: source.clone(),
            reason,
            delay_s: input["delay_s"].as_u64().unwrap_or(0).clamp(30, 86_400),
            branch: input["branch"].as_str().unwrap_or("").to_string(),
            pr: input["pr"].as_u64().unwrap_or(0),
            repo: input["repo"].as_str().unwrap_or("").to_string(),
            baseline: String::new(),
        };
        if source == "timer" && input["delay_s"].as_u64().is_none() {
            return Err("`timer` needs `delay_s` (seconds, 30–86400).".into());
        }
        if source == "pr" && src.pr == 0 {
            return Err("`pr` needs the pull request `number`.".into());
        }
        // GitHub-backed waits need the plugin, and a baseline so the poll can
        // tell a *new* run/comment from the one that was already there — without
        // it a task parked just after a finished CI run would wake on that same
        // run instantly.
        if src.is_github() {
            let cfg = self.h.settings.read().await.plugins.github.clone();
            if !cfg.enabled {
                return Err("The GitHub plugin is off, so I can't watch CI or a PR. Turn it on in Settings → Plugins, or wait on a `timer` instead.".into());
            }
            if let Some(v) = gh_snapshot(&self.h, &self.task_id, &src).await {
                src.baseline = baseline_of(&src, &v);
            }
        }
        park_waiting(&self.h, &self.task_id, src).await?;
        Ok("Parked. This turn ends now; a follow-up turn starts automatically when the trigger fires. Do not poll.".into())
    }

    /// Wait for subagents and/or background commands: any (default) or all. Returns early on an incoming message.
    async fn wait_for(&self, input: &Value) -> Result<String, String> {
        let t = self.h.task(&self.task_id).await?;
        let all_mode = input["mode"] == "all";
        let want: Vec<String> = input["ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(
                input["timeout_s"].as_u64().unwrap_or(600).clamp(1, 1800),
            );
        let rt = self.h.runtime(&self.task_id);
        // What we're waiting on: explicit ids, or everything currently running in the background.
        let targets: Vec<String> = if want.is_empty() {
            let subs = t.lock().await.subs.clone();
            let mut v: Vec<String> = subs
                .iter()
                .filter(|s| {
                    s.status == "running"
                        && Some(s.id.as_str()) != self.sub_id()
                        && (s.background || !self.is_main())
                })
                .map(|s| s.id.clone())
                .collect();
            v.extend(
                self.h
                    .bg
                    .list(&self.task_id)
                    .into_iter()
                    .filter(|b| b.running)
                    .map(|b| b.id),
            );
            v
        } else {
            want
        };
        if targets.is_empty() {
            return Ok("Nothing is running in the background — no need to wait.".into());
        }
        for id in &targets {
            let known =
                t.lock().await.subs.iter().any(|s| &s.id == id) || self.h.bg.get(id).is_some();
            if !known {
                return Err(format!(
                    "No subagent or background command `{id}`. Use list_agents."
                ));
            }
        }
        self.step(format!(
            "Waiting for {}",
            if targets.len() == 1 {
                targets[0].clone()
            } else {
                format!(
                    "{} jobs ({})",
                    targets.len(),
                    if all_mode { "all" } else { "any" }
                )
            }
        ))
        .await;
        loop {
            // Only each target's status is needed to decide whether we're done.
            // Cloning `subs` here deep-copied every agent's full report on each
            // poll — once a second, in silence, for the whole time a chat waits
            // on its swarm. Reports are read once, below, when we finish.
            let running: HashMap<String, String> = {
                let g = t.lock().await;
                targets
                    .iter()
                    .filter_map(|id| {
                        g.subs
                            .iter()
                            .find(|s| s.id == **id)
                            .map(|s| (id.to_string(), s.status.clone()))
                    })
                    .collect()
            };
            let done: Vec<String> = targets
                .iter()
                .filter(|id| match running.get(*id) {
                    Some(status) => status != "running",
                    None => self
                        .h
                        .bg
                        .get(id)
                        .is_none_or(|j| j.exit.lock().unwrap().is_some()),
                })
                .cloned()
                .collect();
            let finished = if all_mode {
                done.len() == targets.len()
            } else {
                !done.is_empty()
            };
            let timed_out = std::time::Instant::now() > deadline;
            let has_mail = rt
                .inbox
                .lock()
                .await
                .get(&self.inbox_key())
                .is_some_and(|v| {
                    v.iter()
                        .any(|n| !n.starts_with("<kind:") || n.contains(MAIL_KINDS))
                });
            if finished || timed_out || has_mail {
                let mut out = String::new();
                let finished_subs: Vec<SubInfo> = {
                    let g = t.lock().await;
                    done.iter()
                        .filter_map(|id| g.subs.iter().find(|s| &s.id == id).cloned())
                        .collect()
                };
                for id in &done {
                    if let Some(s) = finished_subs.iter().find(|s| &s.id == id) {
                        out.push_str(&format!(
                            "── subagent {} ({}) {}:\n{}\n\n",
                            s.id,
                            s.role,
                            s.status,
                            if s.report.trim().is_empty() {
                                "(no report)"
                            } else {
                                &s.report
                            }
                        ));
                        // Its report is here now; drop the duplicate background note.
                        if let Some(v) = rt.inbox.lock().await.get_mut(&self.inbox_key()) {
                            v.retain(|n| !n.contains(&format!("(id `{}`)", s.id)));
                        }
                    } else if let Some((o, exit)) = self.h.bg.read_new(id) {
                        out.push_str(&format!(
                            "── command {id} [{}]:\n{}\n\n",
                            exit.unwrap_or("running".into()),
                            if o.is_empty() {
                                "(no new output)".into()
                            } else {
                                o
                            }
                        ));
                    }
                }
                let pending: Vec<&String> = targets.iter().filter(|x| !done.contains(*x)).collect();
                if !pending.is_empty() {
                    out.push_str(&format!(
                        "Still running: {}.",
                        pending
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if has_mail && !finished {
                    out.insert_str(0, "Stopped waiting: another agent sent you a message (below). Call wait again afterwards if you still need to.\n\n");
                } else if timed_out && !finished {
                    out.insert_str(0, "Timed out waiting.\n\n");
                }
                self.h
                    .emit(&self.task_id, "bg", json!(self.h.bg.list(&self.task_id)));
                return Ok(out.trim_end().to_string());
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(if cfg!(test) { 20 } else { 700 })) => {},
                _ = self.cancel.cancelled() => return Err("Interrupted by user.".into()),
            }
        }
    }

    /// Normalise a set of questions so the UI never has to guess, and so both
    /// question tools hand the frontend the same shape.
    fn normalise_questions(input: &Value) -> Result<Vec<Value>, String> {
        let mut qs = input["questions"].as_array().cloned().unwrap_or_default();
        if qs.is_empty() {
            return Err("questions is required (1-20).".into());
        }
        if qs.len() > 20 {
            return Err(format!(
                "Too many questions ({}). Ask at most 20 per call.",
                qs.len()
            ));
        }
        for (i, q) in qs.iter_mut().enumerate() {
            if q["question"].as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err(format!("Question {} has no text.", i + 1));
            }
            let has_opts = q["options"].as_array().is_some_and(|o| !o.is_empty());
            let ty = q["type"]
                .as_str()
                .unwrap_or(if has_opts { "single" } else { "text" })
                .to_string();
            if (ty == "single" || ty == "multi")
                && q["options"].as_array().map_or(0, |o| o.len()) < 2
            {
                return Err(format!(
                    "Question {} is `{ty}` but has fewer than 2 options.",
                    i + 1
                ));
            }
            q["type"] = json!(ty);
            if q.get("required").is_none_or(|v| v.is_null()) {
                q["required"] = json!(true);
            }
            // A choice question always offers a free-text box, so the flag is a
            // no-op now; keep it true so the UI never reads it as a hard limit.
            if q.get("allow_other").is_none_or(|v| v.is_null()) {
                q["allow_other"] = json!(true);
            }
            // A note is opt-in, and only a plain `true` enables it: any other
            // value is a malformed call, not a silent on-switch.
            if !q.get("note").is_some_and(|v| v.as_bool() == Some(true)) {
                q["note"] = json!(false);
            }
        }
        Ok(qs)
    }

    async fn ask_user(&self, input: &Value, from: Option<&str>) -> Result<String, String> {
        let qs = Self::normalise_questions(input)?;
        let item = Item::new(
            "question",
            input["title"].as_str().unwrap_or(""),
            json!({"questions": qs, "title": input["title"], "intro": input["intro"], "from": from}),
        );
        let id = item.id.clone();
        self.h.upsert_item(&self.task_id, item).await;
        let r = self
            .h
            .wait_for_user(&self.task_id, &id, "question", &self.cancel)
            .await
            .ok_or("Interrupted by user.")?;
        if r["dismissed"].as_bool().unwrap_or(false) {
            let why = r["note"].as_str().unwrap_or("").to_string();
            self.h
                .patch_item(&self.task_id, &id, |i| i.data["dismissed"] = json!(true))
                .await;
            return Err(format!(
                "The user dismissed the questions without answering.{} Use your best judgment, state your assumptions, and continue.",
                if why.is_empty() { String::new() } else { format!(" They said: {why}") }
            ));
        }
        let skips = render_answers(&mut String::new(), &qs, &r);
        self.h
            .patch_item(&self.task_id, &id, |i| {
                i.data["answers"] = r["answers"].clone();
                i.data["notes"] = r["notes"].clone();
                i.data["skipped"] = json!(skips);
            })
            .await;
        let mut s = String::from("The user answered:\n");
        render_answers(&mut s, &qs, &r);
        s.push_str("Proceed with these answers.");
        Ok(s)
    }

    /// A question the agent asked without waiting for the answer. The item goes
    /// up above the composer and the run carries straight on: the answer, if
    /// there ever is one, arrives as a note on the agent's next request
    /// (see `answer_nonblocking`). Nothing waits on this, so it can only be used
    /// for a question the agent can carry on without.
    ///
    /// It still announces itself. The card sits above the composer rather than in
    /// the transcript, so on a run that is producing a lot of output it is easy
    /// never to look up from — and this is the one question the user can still
    /// answer after the run has finished. Its own `nonblocking` attention kind is
    /// what lets the UI word it as an offer rather than a demand, in both the
    /// toast and the in-app notice.
    async fn ask_nonblocking(&self, input: &Value) -> Result<String, String> {
        let qs = Self::normalise_questions(input)?;
        let title = input["title"].as_str().unwrap_or("").to_string();
        let item = Item::new(
            "asklater",
            title.clone(),
            json!({"questions": qs, "title": input["title"], "intro": input["intro"]}),
        );
        self.h.upsert_item(&self.task_id, item).await;
        (self.h.bus)(
            "ol://attention",
            json!({"task_id": self.task_id, "kind": "nonblocking"}),
        );
        let asked: Vec<String> = qs
            .iter()
            .map(|q| q["question"].as_str().unwrap_or("").to_string())
            .collect();
        Ok(format!(
            "Asked {} question{} without waiting, and kept going: {}\n\n\
             This one does not block you: the user sees it above their send bar and the answer arrives as a note on your next request. \
             They may answer, skip, or never see it, and skipping means they don't want to decide — so carry on with your own reasonable choice, \
             say what you assumed, and revise it if the answer turns up later.",
            qs.len(),
            if qs.len() == 1 { "" } else { "s" },
            asked.join(" | "),
        ))
    }

    /// Deliver information, not a question. Existing items provide durable
    /// storage and live UI updates; no pending channel or inbox is involved.
    async fn notify_user(&self, input: &Value) -> Result<String, String> {
        let title = input["title"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or("title is required and must be a nonempty string")?;
        let message = input["message"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or("message is required and must be a nonempty string")?;
        let level = match input.get("level") {
            None => "info",
            Some(Value::String(level)) if matches!(level.as_str(), "info" | "warning") => level,
            _ => return Err("level must be info or warning".into()),
        };
        let item = Item::new(
            "user_notice",
            message,
            json!({"title": title, "level": level, "dismissed": false}),
        );
        let id = item.id.clone();
        self.h.deliver_user_notice(&self.task_id, item).await?;
        (self.h.bus)(
            "ol://attention",
            json!({"task_id": self.task_id, "kind": "notice", "item_id": id}),
        );
        Ok(format!(
            "Delivered informational notice {id} to the app; not waiting for a response. \
             It remains available until the user dismisses it. Delivery does not confirm \
             the user saw it or acted on it. Dismissal is not an answer; continue your work."
        ))
    }

    /// A sub-agent's question: the main agent answers from its context first
    /// (a side request on its cached prefix — its own history is untouched);
    /// if it can't, the question goes to the user unless assist mode is `necessary`.
    async fn sub_ask(&self, input: &Value) -> Result<String, String> {
        let s = self.sub.as_ref().ok_or("not a sub-agent")?;
        let question = input["question"]
            .as_str()
            .filter(|q| !q.trim().is_empty())
            .ok_or("question is required")?
            .to_string();
        let context = input["context"].as_str().unwrap_or("").to_string();
        let options: Vec<String> = input["options"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let h = &self.h;
        let t = h.task(&self.task_id).await?;
        let (model, mut messages, effort, assist) = {
            let t = t.lock().await;
            (
                t.model.clone(),
                t.messages.clone(),
                t.effort,
                t.assist.clone(),
            )
        };
        let qitem = Item::new(
            "notice",
            format!("Asked the main agent: {question}"),
            json!({"level": "ask", "question": question, "options": options}),
        );
        let qid = qitem.id.clone();
        h.upsert_sub_item(&self.task_id, &s.id, qitem).await;
        self.step("Waiting on the main agent".into()).await;

        let ask = format!(
            "<system-reminder>Your sub-agent `{}` (working on: {}) asks you a question. Answer it from what you know about the user's intent and this conversation. Reply with the answer only, in plain text, and don't call tools. If you genuinely don't know and it's the user's call, reply with exactly `ASK_USER` on the first line, then the question rephrased for the user.</system-reminder>\n\nQuestion: {question}{}{}",
            s.def.id,
            t.lock().await.subs.iter().find(|x| x.id == s.id).map(|x| x.task.clone()).unwrap_or_default(),
            if context.is_empty() { String::new() } else { format!("\nContext: {context}") },
            if options.is_empty() { String::new() } else { format!("\nOptions: {}", options.join(" / ")) },
        );
        // Main's pending tool calls (this sub-agent among them) get placeholder results so the side request is valid.
        fix_dangling(&mut messages);
        if let Some(last) = messages.last_mut() {
            if last.role == "user" {
                for b in last.content.iter_mut().filter(|b| {
                    b["type"] == "tool_result"
                        && tools::tool_text(&b["content"])
                            .starts_with("This tool call was interrupted")
                }) {
                    b["content"] = json!("(still running)");
                    b.as_object_mut().map(|o| o.remove("is_error"));
                }
            }
        }
        push_user_blocks(&mut messages, vec![json!({"type": "text", "text": ask})]);
        let (system, mcp, plugins) = frozen_prefix(h, &self.task_id).await?;
        let tl = main_tool_list(h, &self.task_id, &plugins, mcp).await;
        let req = ChatRequest {
            system,
            messages,
            tools: tl,
            effort,
            max_tokens: 4_000,
            cache_key: format!("ol-{}", self.task_id),
        };
        let answer = match router::oneshot(
            h,
            Who {
                task_id: &self.task_id,
                sub: None,
                side: false,
                turn: false,
                usage_id: "sub-ask",
            },
            &model,
            &req,
            &self.cancel,
        )
        .await
        {
            Ok((text, usage, target)) => {
                let cost = usage.cost(&providers::model_info(&target.model_id));
                record_spend(h, cost, usage.input + usage.output).await;
                h.update_task(&self.task_id, |t| t.usage.cost += cost).await;
                text
            }
            Err(RouteErr::Cancelled) => return Err("Interrupted by user.".into()),
            Err(_) => "ASK_USER".into(),
        };
        let answer = answer.trim().to_string();
        if !answer.is_empty() && !answer.starts_with("ASK_USER") {
            let a2 = answer.clone();
            h.patch_in(&self.task_id, Some(&s.id), &qid, |i| {
                i.data["answer"] = json!(a2)
            })
            .await;
            h.upsert_item(
                &self.task_id,
                Item::new(
                    "notice",
                    format!("{} asked: {question}", s.def.name),
                    json!({"level": "subq", "sub_id": s.id, "answer": answer, "by": "main"}),
                ),
            )
            .await;
            return Ok(format!("The main agent answered: {answer}"));
        }
        if assist == "necessary" {
            h.patch_in(&self.task_id, Some(&s.id), &qid, |i| {
                i.data["answer"] = json!("(no answer · decide yourself)")
            })
            .await;
            return Ok("Neither the main agent nor the user can answer right now (assist mode: necessary). Make the most reasonable choice, and list the assumption in your report.".into());
        }
        let rephrased = answer
            .strip_prefix("ASK_USER")
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .unwrap_or(&question)
            .to_string();
        let mut q = json!({"question": rephrased, "header": s.def.name, "description": context});
        if options.len() >= 2 {
            q["type"] = json!("single");
            q["options"] = json!(options
                .iter()
                .map(|o| json!({"label": o}))
                .collect::<Vec<_>>());
        } else {
            q["type"] = json!("text");
        }
        self.step("Waiting on you".into()).await;
        let res = self
            .ask_user(&json!({"questions": [q]}), Some(&s.def.name))
            .await;
        let a2 = res.clone().unwrap_or_else(|e| e);
        h.patch_in(&self.task_id, Some(&s.id), &qid, |i| {
            i.data["answer"] = json!(a2)
        })
        .await;
        res
    }

    async fn goal_complete(&self, input: &Value) -> Result<String, String> {
        let status = if input["status"] == "blocked" {
            "blocked"
        } else {
            "achieved"
        };
        let summary = input["summary"].as_str().unwrap_or("").to_string();
        let evidence = input["evidence"].as_str().unwrap_or("").to_string();
        let t = self.h.task(&self.task_id).await?;
        if t.lock()
            .await
            .goal
            .as_ref()
            .is_none_or(|g| g.status != "active")
        {
            return Err("No active goal. Just finish normally.".into());
        }
        let sm = summary.clone();
        self.h
            .update_task(&self.task_id, |t| {
                if let Some(g) = t.goal.as_mut() {
                    g.status = status.into();
                    g.summary = sm;
                }
            })
            .await;
        let text = if status == "achieved" {
            "Goal achieved"
        } else {
            "Goal blocked"
        };
        let level = if status == "achieved" {
            "goal"
        } else {
            "error"
        };
        self.h
            .upsert_item(
                &self.task_id,
                Item::new(
                    "notice",
                    text,
                    json!({"level": level, "goal": true, "summary": summary, "evidence": evidence}),
                ),
            )
            .await;
        Ok(if status == "achieved" {
            "Goal marked achieved. Give the user a short final summary.".into()
        } else {
            "Goal marked blocked. Tell the user exactly what you need from them.".into()
        })
    }

    /// The agent thinks the job needs a team: ask the user to turn on ultrathread.
    /// ULTRATHREAD X is deliberately never offered here: its ladder is a deliberate
    /// user decision, so this only ever proposes plain ultrathread or worktrees.
    async fn start_ultra(&self, input: &Value) -> Result<String, String> {
        let t = self.h.task(&self.task_id).await?;
        let (ultra, cwd) = {
            let t = t.lock().await;
            (t.ultra, t.cwd.clone())
        };
        if ultra {
            return Err("Ultrathread is already on.".into());
        }
        let reason = input["reason"].as_str().unwrap_or("").trim().to_string();
        if reason.is_empty() {
            return Err("`reason` is required: say why this needs a team.".into());
        }
        let streams = input["workstreams"].as_u64();
        let git = git::is_repo(&cwd);
        // The agent's worktree recommendation: Some(true) for, Some(false) against, None = no opinion.
        let rec_wt = if git {
            input["worktrees"].as_bool()
        } else {
            None
        };
        let rec_why = input["worktrees_why"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let title = match streams {
            Some(n) => format!("Switch to ultrathread? (~{n} workstreams)"),
            None => "Switch to ultrathread?".to_string(),
        };
        let item = Item::new(
            "approval",
            title.clone(),
            json!({"kind": "ultra", "title": title, "detail": reason, "git": git, "rec_wt": rec_wt, "rec_why": rec_why, "reason": "The agent thinks this is too much work for one agent."}),
        );
        let id = item.id.clone();
        self.h.upsert_item(&self.task_id, item).await;
        let r = self
            .h
            .wait_for_user(&self.task_id, &id, "approval", &self.cancel)
            .await
            .ok_or("Interrupted by user.")?;
        let decision = r["decision"].as_str().unwrap_or("deny").to_string();
        let feedback = r["feedback"].as_str().unwrap_or("").to_string();
        self.h
            .patch_item(&self.task_id, &id, |i| {
                i.data["resolved"] = json!(decision);
                i.data["feedback"] = json!(feedback);
            })
            .await;
        if decision == "deny" {
            return Err(if feedback.is_empty() {
                "The user declined ultrathread. Keep going yourself (subagents are still available as usual).".into()
            } else {
                format!("The user declined ultrathread: {feedback}")
            });
        }
        let wt = decision == "worktrees" && git;
        self.h
            .update_task(&self.task_id, |t| {
                t.ultra = true;
                t.ultra_wt = wt;
                // Never let an agent's request put the task into X.
                t.ultra_x = None;
            })
            .await;
        self.h.save_task(&self.task_id).await;
        Ok(format!("The user approved ultrathread{}. Its instructions arrive with your next request: plan the waves, then launch them.", if wt { " with worktrees" } else { "" }))
    }

    async fn exit_plan(&self, input: &Value) -> Result<String, String> {
        let plan = input["plan"].as_str().unwrap_or("").to_string();
        let item = Item::new(
            "approval",
            "Ready to implement this plan?",
            json!({"kind": "plan", "title": "Ready to implement this plan?", "detail": plan, "reason": "Approving turns plan mode off."}),
        );
        let id = item.id.clone();
        self.h.upsert_item(&self.task_id, item).await;
        let r = self
            .h
            .wait_for_user(&self.task_id, &id, "approval", &self.cancel)
            .await
            .ok_or("Interrupted by user.")?;
        let decision = r["decision"].as_str().unwrap_or("deny").to_string();
        let feedback = r["feedback"].as_str().unwrap_or("").to_string();
        self.h
            .patch_item(&self.task_id, &id, |i| {
                i.data["resolved"] = json!(decision);
                i.data["feedback"] = json!(feedback);
            })
            .await;
        if decision == "deny" {
            return Err(if feedback.is_empty() {
                "The user wants to keep planning. Ask what to change, or refine the plan.".into()
            } else {
                format!("The user rejected the plan: {feedback}")
            });
        }
        self.h
            .update_task(&self.task_id, |t| {
                t.plan = false;
                if decision == "always" {
                    approve_auto_edits(t);
                }
            })
            .await;
        Ok("The user approved the plan. Plan mode is off — implement it now, tracking progress with todo_write.".into())
    }

    async fn spawn_sub(&self, input: &Value, call_id: &str) -> Result<String, String> {
        let t = self.h.task(&self.task_id).await?;
        let snapshot = t.lock().await.clone();
        if !snapshot.subagents || snapshot.agents.is_empty() {
            return Err("Sub-agents are turned off for this task. Do the work yourself.".into());
        }
        let want = input["subagent_type"]
            .as_str()
            .unwrap_or("general")
            .to_string();
        let defs = allowed_agents(&self.h, &snapshot).await;
        let def = defs.iter().find(|d| d.id == want).cloned().ok_or_else(|| {
            format!(
                "`{want}` isn't an available sub-agent. Use one of: {}.",
                defs.iter()
                    .map(|d| d.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let desc = input["description"]
            .as_str()
            .unwrap_or("Sub-task")
            .to_string();
        let brief = input["prompt"]
            .as_str()
            .ok_or("prompt is required")?
            .to_string();
        let parent = self.sub_id().map(String::from);
        let depth = self.depth().await + 1;
        let x = task_x(&snapshot);
        // A per-layer fanout cap is this agent's own limit on live children.
        let fanout = layer_fanout(x, self.depth().await);

        // ULTRATHREAD X: a sub-agent runs on the ladder's model for its depth,
        // which beats the agent type's own model — the layer is the bigger decision.
        let layer = x.and_then(|x| x.layer(depth));
        // Store a model on the new agent only when it is this *agent's* to choose:
        // its type's own model. Deliberately not the layer's, and not the chat's —
        // those are read live per request, so editing the ladder or swapping the
        // chat reaches the agents already running. Stamping either here would pin
        // this agent to a decision that has since been made again.
        let model = if def.model.is_empty() {
            String::new()
        } else {
            snapshot
                .model_map
                .get(&def.model)
                .cloned()
                .unwrap_or_else(|| def.model.clone())
        };
        // Likewise for effort: the layer's setting, else the agent type's.
        let effort = layer.and_then(|l| l.effort).or(def.effort);

        if snapshot.ultra {
            if let Some(f) = fanout {
                let mine = parent
                    .as_deref()
                    .map(|p| {
                        snapshot
                            .subs
                            .iter()
                            .filter(|s| s.parent == p && s.status == "running")
                            .count()
                    })
                    .unwrap_or(0);
                if mine >= f as usize {
                    return Err(format!(
                        "You already have {mine} subagents running and this layer of the ULTRATHREAD X ladder is capped at {f} at a time. Wait for one to finish (task_status with wait: true), or send it more work, before launching another."
                    ));
                }
            }
            let running = snapshot
                .subs
                .iter()
                .filter(|s| s.status == "running")
                .count();
            let cap = ultra_running_cap(x);
            if running >= cap {
                return Err(format!(
                    "{cap} subagents are already running. Wait for some to finish (task_status with wait: true), then launch more."
                ));
            }
            // Total spawned, ever. The running cap alone doesn't bound spend: a
            // deep tree can churn through thousands of agent-lives over a long task.
            if let Some(x) = x {
                let total = x.total_cap();
                if snapshot.subs.len() >= total {
                    return Err(format!(
                        "This task has already spawned all {total} subagents its ULTRATHREAD X ladder allows. Integrate what's finished, verify it, and report — don't launch more."
                    ));
                }
            }
        }
        let sid = super::new_id();
        // Ultrathread worktrees: each top-level worker gets its own checkout + branch; nested
        // agents share their coordinator's. Read-only agents and the fuze agent stay in the task's.
        let wt_on = snapshot.ultra && (snapshot.ultra_wt || x.is_some_and(|x| x.wt));
        let (wt_cwd, wt_branch) = if wt_on
            && def.tools != "read_only"
            && def.id != "fuze"
            && git::is_repo(&snapshot.cwd)
        {
            match parent.as_deref() {
                Some(p) => snapshot.cwd_for(Some(p)),
                None => {
                    let base = git::head(&snapshot.cwd).unwrap_or_default();
                    let slug = format!("ut-{}", git::slug(&desc));
                    let cwd = snapshot.cwd.clone();
                    tokio::task::spawn_blocking(move || git::create_worktree(&cwd, &base, &slug))
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| format!("Couldn't create a worktree for this subagent: {e}"))?
                }
            }
        } else {
            (String::new(), String::new())
        };
        let own_wt = !wt_cwd.is_empty() && wt_cwd != snapshot.cwd;
        let mut worktree_setup_issue = None;
        if own_wt {
            let project_hooks = {
                let settings = self.h.settings.read().await;
                checks::hooks_for(&settings, &snapshot.project)
            };
            let setup = checks::setup_worktree(
                &project_hooks,
                &snapshot.project,
                &wt_cwd,
                &self.task_id,
                &self.cancel,
            )
            .await;
            self.h
                .upsert_in(
                    &self.task_id,
                    parent.as_deref(),
                    Item::new(
                        "notice",
                        "Created worktree",
                        json!({"level": "worktree", "path": wt_cwd, "branch": wt_branch}),
                    ),
                )
                .await;
            let failures: Vec<String> =
                setup
                    .copy_error
                    .into_iter()
                    .map(|error| format!("included-file copy failed: {error}"))
                    .chain(setup.hooks.into_iter().filter(|hook| !hook.ok).map(|hook| {
                        format!("setup hook `{}` failed: {}", hook.command, hook.output)
                    }))
                    .collect();
            if !failures.is_empty() {
                let issue = format!(
                    "Worktree setup in {wt_cwd} did not finish cleanly: {}",
                    failures.join("; ")
                );
                worktree_setup_issue = Some(issue.clone());
            }
        }
        let item = Item::new(
            "sub",
            desc.clone(),
            json!({"sub_id": sid, "role": def.id, "name": def.name, "color": def.color, "status": "running", "parent": parent.clone().unwrap_or_default(), "depth": depth}),
        );
        let iid = item.id.clone();
        let info = SubInfo {
            id: sid.clone(),
            role: def.id.clone(),
            task: desc.clone(),
            status: "running".into(),
            meta: "starting".into(),
            model,
            serving: String::new(),
            started: Some(chrono::Utc::now()),
            report: String::new(),
            call_id: call_id.to_string(),
            item_id: iid.clone(),
            background: false,
            parent: parent.clone().unwrap_or_default(),
            depth,
            effort,
            cwd: if own_wt {
                wt_cwd.clone()
            } else {
                String::new()
            },
            branch: if own_wt {
                wt_branch.clone()
            } else {
                String::new()
            },
        };
        self.h
            .upsert_in(&self.task_id, parent.as_deref(), item)
            .await;
        self.h
            .update_task(&self.task_id, |t| t.subs.push(info))
            .await;
        if own_wt {
            let detail = worktree_setup_issue.as_deref().unwrap_or_default();
            self.h
                .upsert_sub_item(
                    &self.task_id,
                    &sid,
                    Item::new(
                        "notice",
                        "Created worktree",
                        json!({"level": "worktree", "path": wt_cwd, "branch": wt_branch, "detail": detail}),
                    ),
                )
                .await;
        }
        // Images the parent hands over: the sub-agent sees them directly.
        // Without this the brief's "see the screenshot" is a dead end — it has
        // no way to reach an image that only exists in the parent's context.
        let mut images: Vec<Value> = vec![];
        if let Some(a) = input["images"].as_array() {
            for u in a.iter().filter_map(|x| x.as_str()) {
                match image_blocks(&[u.to_string()]) {
                    Ok(mut b) => images.append(&mut b),
                    Err(e) => return Err(format!("That image couldn't be passed on: {e}")),
                }
            }
        }
        let mut brief_data = json!({"brief": true});
        if !images.is_empty() {
            brief_data["has_images"] = json!(images.len());
        }
        self.h
            .upsert_sub_item(
                &self.task_id,
                &sid,
                Item::new("user", brief.clone(), brief_data),
            )
            .await;
        let ultra = if snapshot.ultra {
            format!(
                "\n\n{}",
                prompt::ultra_sub(depth, self.can_nest().await, fanout)
            )
        } else {
            String::new()
        };
        let ultra = if own_wt {
            format!(
                "{ultra}\n\n{}",
                prompt::ultra_wt_sub(&wt_cwd, &wt_branch, parent.is_none())
            )
        } else {
            ultra
        };
        let setup_reminder = worktree_setup_issue
            .as_ref()
            .map(|issue| format!("\n\n<system-reminder>{issue}</system-reminder>"))
            .unwrap_or_default();
        let brief_full = format!("{brief}\n\n{}{ultra}\n\nYour id is `{sid}`. Other agents in this task (list_agents) can message you; use send_message to share findings or coordinate with them or the main agent.{setup_reminder}", prompt::sub_assist(&snapshot.assist));
        let mut init_blocks = images;
        init_blocks.push(json!({"type": "text", "text": brief_full}));
        let init = vec![Message::user(init_blocks)];
        if input["run_in_background"].as_bool().unwrap_or(false) {
            self.h
                .patch_in(&self.task_id, parent.as_deref(), &iid, |i| {
                    i.data["background"] = json!(true)
                })
                .await;
            self.h
                .update_task(&self.task_id, |t| {
                    if let Some(x) = t.subs.iter_mut().find(|x| x.id == sid) {
                        x.background = true;
                    }
                })
                .await;
            self.launch_background(sid.clone(), def.clone(), iid, init, parent.clone());
            return Ok(format!(
                "Started the `{}` subagent in the background (id `{sid}`). Keep working on other things; its report is delivered to you automatically when it finishes. Use task_status with this id to check on it or wait for it.",
                def.id
            ));
        }
        self.drive_sub(sid, def, iid, init, parent, self.cancel.child_token())
            .await
    }

    /// Run a sub-agent detached from the current turn; its report arrives as a note.
    fn launch_background(
        &self,
        sid: String,
        def: AgentDef,
        iid: String,
        init: Vec<Message>,
        parent: Option<String>,
    ) {
        let cancel = self
            .h
            .runtime(&self.task_id)
            .bg_cancel
            .lock()
            .unwrap()
            .clone();
        let me = Agent {
            h: self.h.clone(),
            task_id: self.task_id.clone(),
            sub: None,
            cancel: cancel.clone(),
        };
        tauri::async_runtime::spawn(async move {
            let res = me
                .drive_sub(sid.clone(), def.clone(), iid, init, parent.clone(), cancel)
                .await;
            let (label, body) = match &res {
                Ok(r) => (
                    "finished",
                    if r.trim().is_empty() {
                        "(no report)".to_string()
                    } else {
                        r.clone()
                    },
                ),
                Err(e) if e.contains(INTERRUPTED) => return, // stopped with Esc: Continue resumes it
                Err(e) => ("failed", e.clone()),
            };
            let note = format!("<system-reminder>Your background `{}` subagent (id `{sid}`) {label}. Its report:</system-reminder>\n{body}", def.id);
            match parent {
                // Nested: the launching sub-agent picks it up on its next request.
                Some(p) => me.h.note(&me.task_id, &p, Kinded::agent(&note).0).await,
                None => deliver_to_main(&me.h, &me.task_id, note).await,
            }
        });
    }

    /// Continue a sub-agent that already exists, in its own context: it keeps
    /// everything it found, so a follow-up doesn't have to re-brief anyone.
    async fn resume_sub(&self, input: &Value) -> Result<String, String> {
        let t = self.h.task(&self.task_id).await?;
        if !t.lock().await.subagents {
            return Err("Sub-agents are turned off for this task. Do the work yourself.".into());
        }
        // Read fresh: a coordinator that launched children seconds ago holds an
        // older copy of this task, and its sub-agents aren't in that one.
        let (s, snapshot) = {
            let g = t.lock().await;
            let s = g
                .subs
                .iter()
                .find(|x| x.id == input["id"].as_str().unwrap_or_default())
                .cloned();
            (s, g.clone())
        };
        let id = input["id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("`id` is required: a subagent id from list_agents")?
            .to_string();
        let s = s.ok_or_else(|| match snapshot.subs.len() {
            0 => "This task has no subagents yet.".to_string(),
            _ => format!("No subagent with id `{id}`. Use list_agents to see the ids."),
        })?;
        // You may only steer your own children; a sibling has no business
        // taking over another agent's slot.
        if let Some(me) = self.sub_id() {
            if s.parent != me {
                return Err(format!("`{id}` isn't one of your subagents. You can only resume the ones you launched."));
            }
        }
        if s.status == "running" {
            return Err(format!("`{}` ({id}) is already running. Use send_message to give it more work, or task_status to wait for it.", s.role));
        }
        // Its type may have been edited or removed since it ran; fall back to
        // whatever its last report implies so a resume never loses the agent.
        let defs = store::all_agents(&*self.h.settings.read().await, &snapshot.project);
        let def = defs
            .iter()
            .find(|d| d.id == s.role)
            .cloned()
            .unwrap_or_else(|| fallback_def(&s.role));

        // A resume spends a subagent life like any launch, so it counts
        // against the same caps — otherwise a wave of resumes walks past the
        // ladder's budget entirely.
        if snapshot.ultra {
            if let Some(x) = task_x(&snapshot) {
                let total = x.total_cap();
                if snapshot.subs.len() >= total {
                    return Err(format!("This task has already spawned all {total} subagents its ULTRATHREAD X ladder allows. Integrate what's finished, verify it, and report — don't launch more."));
                }
            }
            let running = snapshot
                .subs
                .iter()
                .filter(|x| x.status == "running")
                .count();
            let cap = ultra_running_cap(task_x(&snapshot));
            if running >= cap {
                return Err(format!("{cap} subagents are already running. Wait for some to finish (task_status with wait: true), then resume others."));
            }
            if let Some(f) = layer_fanout(task_x(&snapshot), s.depth) {
                let mine = snapshot
                    .subs
                    .iter()
                    .filter(|x| x.parent == s.parent && x.status == "running")
                    .count();
                if mine >= f as usize {
                    return Err(format!("You already have {mine} subagents running and this layer of the ULTRATHREAD X ladder is capped at {f} at a time. Wait for one to finish, or send it more work, before resuming another."));
                }
            }
        }

        // Its conversation is the whole point of resuming; without one there is
        // nothing to continue, so say so rather than silently re-briefing.
        let parent = if s.parent.is_empty() {
            None
        } else {
            Some(s.parent.clone())
        };
        // A new user turn: the caller's message, or a reminder to pick up where
        // it left off.
        let note = input["message"].as_str().map(str::trim).filter(|m| !m.is_empty()).map(String::from).unwrap_or_else(|| {
            "<system-reminder>You were stopped. Continue the job exactly where you left off — re-check the current state, finish what remains, and report.</system-reminder>".to_string()
        });
        // Claim it and take its history in one atomic step: between reading the
        // task and marking it running, anything (the user, another agent) could
        // have started the same sub-agent, and two runs would then share one
        // transcript.
        let init = t.lock().await.try_resume_sub(&id, self.sub_id(), &note);
        let Some(init) = init else {
            // Give the reason that still holds: it started in the meantime, or
            // its conversation was never saved.
            if s.status == "running"
                || t.lock()
                    .await
                    .subs
                    .iter()
                    .find(|x| x.id == id)
                    .is_some_and(|x| x.status == "running")
            {
                return Err(format!("`{}` ({id}) is already running. Use send_message to give it more work, or task_status to wait for it.", s.role));
            }
            return Err(format!("`{id}` has no saved conversation, so there's nothing to resume. Launch a new subagent with `task` instead."));
        };
        // The pill lives in its parent's transcript, not the main timeline.
        self.h
            .patch_in(&self.task_id, parent.as_deref(), &s.item_id, |i| {
                i.data["status"] = json!("running");
                i.data["report"] = json!("");
            })
            .await;
        if parent.is_some() {
            self.h
                .upsert_sub_item(
                    &self.task_id,
                    &id,
                    Item::new("user", note.clone(), json!({"resumed": true})),
                )
                .await;
        }

        if input["background"].as_bool().unwrap_or(false) {
            self.h
                .update_task(&self.task_id, |t| {
                    if let Some(x) = t.subs.iter_mut().find(|x| x.id == id) {
                        x.background = true;
                    }
                })
                .await;
            self.launch_background(id.clone(), def, s.item_id.clone(), init, parent);
            return Ok(format!("Resumed `{}` (id `{id}`) in the background; it keeps its context. Keep working on other things — its report is delivered to you automatically when it finishes.", s.role));
        }
        self.step(format!("Resuming {}", s.role)).await;
        let report = self
            .drive_sub(
                id.clone(),
                def,
                s.item_id.clone(),
                init,
                parent,
                self.cancel.child_token(),
            )
            .await;
        Ok(match report {
            Ok(r) => format!("`{}` (id `{id}`) finished. Report:\n{r}", s.role),
            Err(e) => e,
        })
    }

    /// Check on (or wait for) a sub-agent by id.
    async fn task_status(&self, input: &Value) -> Result<String, String> {
        let id = input["id"].as_str().ok_or("id is required")?.to_string();
        let wait = input["wait"].as_bool().unwrap_or(false);
        let t = self.h.task(&self.task_id).await?;
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(input["timeout_s"].as_u64().unwrap_or(600).min(1800));
        loop {
            // Read the one agent's fields, not the whole sub list: `cloned()`
            // copied every agent's full report on each 1s poll, and a chat
            // waiting on a swarm deep-cloned all of them once a second while
            // saying nothing at all.
            let s = {
                let g = t.lock().await;
                let s = g
                    .subs
                    .iter()
                    .find(|s| s.id == id)
                    .ok_or_else(|| format!("No subagent with id `{id}`."))?;
                (
                    s.status.clone(),
                    s.role.clone(),
                    s.meta.clone(),
                    s.report.clone(),
                )
            };
            if s.0 != "running" || !wait || std::time::Instant::now() > deadline {
                return Ok(match s.0.as_str() {
                    "running" => format!(
                        "`{}` is still running ({}).{}",
                        s.1,
                        s.2,
                        if wait {
                            " Timed out waiting; check again later."
                        } else {
                            ""
                        }
                    ),
                    "done" => format!("`{}` finished. Report:\n{}", s.1, s.3),
                    other => format!("`{}` {other}: {}", s.1, s.3),
                });
            }
            self.step(format!("Waiting for {}", s.1)).await;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(if cfg!(test) { 20 } else { 1000 })) => {},
                _ = self.cancel.cancelled() => return Err("Interrupted by user.".into()),
            }
        }
    }

    /// Run (or continue) a sub-agent from `init` and record how it ended. `parent`
    /// is the sub-agent whose transcript holds its pill (None = the main timeline).
    async fn drive_sub(
        &self,
        sid: String,
        def: AgentDef,
        iid: String,
        init: Vec<Message>,
        parent: Option<String>,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        self.h
            .update_task(&self.task_id, |t| {
                if let Some(x) = t.subs.iter_mut().find(|x| x.id == sid) {
                    x.status = "running".into();
                }
                t.sub_msgs.insert(sid.clone(), init.clone());
            })
            .await;
        self.h
            .patch_in(&self.task_id, parent.as_deref(), &iid, |i| {
                i.data["status"] = json!("running")
            })
            .await;
        let child = Agent {
            h: self.h.clone(),
            task_id: self.task_id.clone(),
            sub: Some(SubCtx {
                id: sid.clone(),
                def,
            }),
            cancel,
        };
        let started = std::time::Instant::now();
        let res = child.run_loop(Some(init)).await;
        let (hook_project, hook_cwd) = if let Ok(task) = self.h.task(&self.task_id).await {
            let task = task.lock().await;
            (task.project.clone(), task.cwd_for(Some(&sid)).0)
        } else {
            (String::new(), String::new())
        };
        let hooks = {
            let settings = self.h.settings.read().await;
            checks::hooks_for(&settings, &hook_project)
        };
        let hook_report = res
            .as_ref()
            .map(String::as_str)
            .unwrap_or_else(|e| e.as_str());
        let _ = checks::subagent_stop(
            &hooks,
            &sid,
            hook_report,
            &hook_cwd,
            &hook_project,
            &self.task_id,
            &child.cancel,
        )
        .await;
        let secs = started.elapsed().as_secs();
        let (status, mut report) = match &res {
            Ok(r) => ("done", r.clone()),
            Err(e) => (
                if e == INTERRUPTED {
                    "stopped"
                } else {
                    "failed"
                },
                e.clone(),
            ),
        };
        // Worktree worker finished: commit its branch so the fuze agent can merge it.
        let mine = self
            .h
            .task(&self.task_id)
            .await?
            .lock()
            .await
            .subs
            .iter()
            .find(|x| x.id == sid && x.depth == 1 && !x.cwd.is_empty())
            .map(|x| (x.cwd.clone(), x.branch.clone(), x.task.clone()));
        if let (Some((cwd, branch, desc)), "done") = (mine, status) {
            let c = cwd.clone();
            let msg = format!("ultrathread: {desc}");
            let sha = tokio::task::spawn_blocking(move || git::commit(&c, &msg))
                .await
                .ok()
                .and_then(|r| r.ok());
            let what = match sha {
                Some(s) => format!("committed as {s}"),
                None => "nothing new to commit".to_string(),
            };
            report.push_str(&format!("\n\n[Worktree branch `{branch}` ({cwd}): {what}. Merge it into the main checkout with the `fuze` subagent.]"));
        }
        let rep = report.clone();
        self.h
            .update_task(&self.task_id, |t| {
                if let Some(x) = t.subs.iter_mut().find(|x| x.id == sid) {
                    x.status = status.into();
                    x.meta = format!("{} · {secs}s", x.meta.split(" · ").next().unwrap_or(""));
                    x.report = rep.chars().take(20_000).collect();
                }
            })
            .await;
        let rep = report.clone();
        self.h
            .patch_in(&self.task_id, parent.as_deref(), &iid, |i| {
                i.data["status"] = json!(status);
                i.data["report"] = json!(rep);
                i.data["secs"] = json!(secs);
            })
            .await;
        match res {
            Ok(r) => Ok(if r.trim().is_empty() {
                "(the sub-agent finished without a report)".into()
            } else {
                r
            }),
            Err(e) => Err(format!("Sub-agent {status}: {e}")),
        }
    }
}

/// Append blocks to the newest user turn (or start one). Never touches older turns.
/// Everything the user handed us mid-turn, as the content blocks for one user
/// turn: the reminders (text first, so the model reads them in order), then any
/// image blocks steering brought along.
///
/// A photo on its own still produces blocks. The guard used to be "are there any
/// text reminders?", which meant a screenshot pasted with no caption was dropped
/// and never reached the model at all.
/// Append blocks to the newest user turn (or start one). Never touches older turns.
/// A question set as the user answered it, the way `ask_user` and `ask_nonblocking`
/// both report one back to the model. Shared so a skip reads the same whether
/// the run was waiting on the answer or carried on without it.
fn render_answers(out: &mut String, qs: &[Value], r: &Value) -> Vec<usize> {
    let answers: Vec<Value> = r["answers"].as_array().cloned().unwrap_or_default();
    let notes: Vec<String> = r["notes"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or_default().trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    let blanks = |a: Option<&Value>| match a {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    };
    let mut skips: Vec<usize> = r["skipped"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64())
                .map(|x| x as usize)
                .filter(|i| *i < qs.len())
                .collect()
        })
        .unwrap_or_default();
    if skips.is_empty() {
        // A form sent before the skip list existed: an empty answer was a skip.
        skips = qs
            .iter()
            .enumerate()
            .filter(|(i, q)| {
                blanks(answers.get(*i))
                    && q.get("required")
                        .is_none_or(|v| v.as_bool().unwrap_or(true))
            })
            .map(|(i, _)| i)
            .collect();
    }
    for (i, q) in qs.iter().enumerate() {
        let a = answers.get(i).cloned().unwrap_or(Value::Null);
        // An empty answer is only a skip if the form said it was one: a
        // question marked optional that nobody had an opinion on is just
        // unanswered, and calling that a refusal would be a lie.
        let skipped = skips.contains(&i);
        let shown = if skipped && blanks(Some(&a)) {
            "(skipped)".to_string()
        } else if blanks(Some(&a)) {
            "(no answer — optional)".to_string()
        } else {
            // An option can arrive as `{label, note}` when the user extended
            // that one option; the extension belongs on that option's line.
            let one = |x: &Value| match x {
                Value::Object(m) if m.contains_key("label") => {
                    let label = m["label"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| m["label"].to_string());
                    match m
                        .get("note")
                        .and_then(|n| n.as_str())
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                    {
                        Some(n) => format!("{label} (note: {n})"),
                        None => label,
                    }
                }
                Value::String(x) => x.clone(),
                Value::Bool(b) => (if *b { "Yes" } else { "No" }).to_string(),
                other => other.to_string(),
            };
            match &a {
                Value::Array(v) if v.is_empty() => "(none selected)".to_string(),
                Value::Array(v) => v.iter().map(one).collect::<Vec<_>>().join(", "),
                other => one(other),
            }
        };
        out.push_str(&format!(
            "{}. {} → {}\n",
            i + 1,
            q["question"].as_str().unwrap_or(""),
            shown
        ));
        if let Some(n) = notes.get(i).filter(|n| !n.is_empty()) {
            out.push_str(&format!("   note: {n}\n"));
        }
    }
    if let Some(n) = r["note"].as_str().filter(|n| !n.trim().is_empty()) {
        out.push_str(&format!("\nAdditional note from the user: {n}\n"));
    }
    // Skipping is the user declining to decide, not leaving a field blank.
    // Say so plainly, or the agent reads an empty line as a hint and guesses
    // on something the user just refused to answer.
    if !skips.is_empty() {
        out.push_str(&format!(
            "\nThe user skipped {}. That is their answer: they don't want to decide this, so don't ask again. Make the call yourself, state the assumption you made, and keep going.\n",
            if skips.len() == 1 { "1 question".to_string() } else { format!("{} questions", skips.len()) }
        ));
    }
    skips
}

pub fn mid_turn_blocks(
    reminders: &[String],
    steer_blocks: &[Value],
    imgs: &[(String, String)],
) -> Vec<Value> {
    let mut blocks: Vec<Value> = reminders
        .iter()
        .map(|r| json!({"type": "text", "text": r}))
        .collect();
    blocks.extend(steer_blocks.iter().cloned());
    blocks.extend(imgs.iter().map(|(mt, data)| json!({"type": "image", "source": {"type": "base64", "media_type": mt, "data": data}})));
    blocks
}

pub fn push_user_blocks(msgs: &mut Vec<Message>, blocks: Vec<Value>) {
    match msgs.last_mut() {
        Some(m) if m.role == "user" => m.content.extend(blocks),
        _ => msgs.push(Message::user(blocks)),
    }
}

// ───────────────────────────── streaming → UI ─────────────────────────────

/// Turns provider stream events into timeline items (main timeline or a
/// sub-agent's transcript). Text/thinking stream as deltas; each tool call
/// gets an item as soon as the model starts writing it.
struct StreamUi {
    h: Arc<Harness>,
    task_id: String,
    sub: Option<String>,
    cur: Option<Item>,
    done: Vec<Item>,
    /// (tool_use_id, item_id)
    tool_items: Vec<(String, String)>,
    /// Streamed text not yet pushed to the UI, and when the last push was. A
    /// provider sends one event per token, and each one was its own emit: a
    /// 4 000-token answer meant 4 000 IPC round-trips and 4 000 serializes on
    /// the way, all of it to append a few characters to one item. They are
    /// held here and sent together a few times a second instead.
    pending: String,
    last_emit: Option<std::time::Instant>,
}

/// How often streamed text goes to the UI, and how much has to pile up first.
/// Fast enough that a reply still reads as typing, slow enough that a long
/// answer costs tens of events rather than thousands.
const DELTA_EVERY: std::time::Duration = std::time::Duration::from_millis(40);
const DELTA_BYTES: usize = 512;

const HIDDEN_TOOLS: &[&str] = &[
    "todo_write",
    "ask_user",
    "ask_nonblocking",
    "notify_user",
    "ask",
    "task",
    "task_resume",
    "task_status",
    "exit_plan_mode",
    "goal_complete",
    "start_ultrathread",
];

impl StreamUi {
    fn new(h: Arc<Harness>, task_id: String, sub: Option<String>) -> Self {
        Self {
            h,
            task_id,
            sub,
            cur: None,
            done: vec![],
            tool_items: vec![],
            pending: String::new(),
            last_emit: None,
        }
    }

    fn emit_item(&self, item: &Item) {
        match &self.sub {
            Some(s) => self
                .h
                .emit(&self.task_id, "sitem", json!({"sub_id": s, "item": item})),
            None => self.h.emit(&self.task_id, "item", json!(item)),
        }
    }

    fn on(&mut self, e: StreamEvent) {
        match e {
            StreamEvent::Text(s) | StreamEvent::Thinking(s) if s.is_empty() => {}
            StreamEvent::Text(s) => self.delta("text", s),
            StreamEvent::Thinking(s) => self.delta("thinking", s),
            StreamEvent::ToolStart { id, name } => {
                self.close();
                if HIDDEN_TOOLS.contains(&name.as_str()) {
                    return;
                }
                // Display gatekeeper aliases under their real names.
                let name = match name.as_str() {
                    "read" => "read_file".to_string(),
                    "shell" => "bash".to_string(),
                    _ => name,
                };
                let item = Item::new(
                    "tool",
                    "",
                    json!({"name": name, "status": "running", "tool_use_id": id}),
                );
                self.tool_items.push((id, item.id.clone()));
                self.emit_item(&item);
                self.done.push(item);
            }
            StreamEvent::BlockEnd => self.close(),
            StreamEvent::Reset => {
                // A failed attempt streamed partial output; take it back off the screen.
                self.close();
                let ids: Vec<String> = self.done.drain(..).map(|i| i.id).collect();
                self.tool_items.clear();
                if !ids.is_empty() {
                    self.h.emit(
                        &self.task_id,
                        "drop",
                        json!({"sub_id": self.sub, "ids": ids}),
                    );
                }
            }
        }
    }

    fn delta(&mut self, kind: &str, s: String) {
        if self.cur.as_ref().map(|c| c.kind != kind).unwrap_or(false) {
            self.close();
        }
        match &mut self.cur {
            Some(c) => {
                c.text.push_str(&s);
                self.pending.push_str(&s);
                self.maybe_emit();
            }
            None => {
                let item = Item::new(kind, s, json!({}));
                self.emit_item(&item);
                self.cur = Some(item);
            }
        }
    }

    /// Push the buffered text if enough time has passed (or enough of it has
    /// piled up), leaving the rest for the next token.
    fn maybe_emit(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let due = self.last_emit.is_none_or(|t| t.elapsed() >= DELTA_EVERY);
        if !due && self.pending.len() < DELTA_BYTES {
            return;
        }
        self.flush();
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let Some(id) = self.cur.as_ref().map(|c| c.id.clone()) else {
            self.pending.clear();
            return;
        };
        self.h.emit(
            &self.task_id,
            "delta",
            json!({"item_id": id, "text": self.pending, "sub_id": self.sub}),
        );
        self.pending.clear();
        self.last_emit = Some(std::time::Instant::now());
    }

    fn close(&mut self) {
        // The item is about to end, so whatever is still buffered belongs to it
        // and has to go out now — the next block starts a different item.
        self.flush();
        if let Some(c) = self.cur.take() {
            self.done.push(c);
        }
    }

    /// Persist streamed items; tool items get their final input attached later.
    /// `served`: the model (and account/key) that actually answered, stamped on each item
    /// (with fallbacks it can differ from the chat's model).
    async fn finish(&mut self, served: Option<(String, String)>) {
        self.close();
        let mut items = std::mem::take(&mut self.done);
        if let Some((model, via)) = &served {
            for it in items
                .iter_mut()
                .filter(|i| i.kind == "text" || i.kind == "thinking")
            {
                it.data["model"] = json!(model);
                it.data["via"] = json!(via);
                self.emit_item(it);
            }
        }
        if let Ok(t) = self.h.task(&self.task_id).await {
            let mut t = t.lock().await;
            let list = match &self.sub {
                Some(s) => t.sub_items.entry(s.clone()).or_default(),
                None => &mut t.items,
            };
            for it in items {
                if !list.iter().any(|x| x.id == it.id) {
                    list.push(it);
                }
            }
        }
    }
}

/// After a turn, attach each tool call's input to its timeline item.
pub async fn attach_inputs(
    h: &Harness,
    task_id: &str,
    sub: Option<&str>,
    content: &[Value],
    items: &[(String, String)],
) {
    for b in content.iter().filter(|b| b["type"] == "tool_use") {
        let tid = b["id"].as_str().unwrap_or("");
        if let Some((_, iid)) = items.iter().find(|(t, _)| t == tid) {
            let input = b["input"].clone();
            h.patch_in(task_id, sub, iid, |i| i.data["input"] = input)
                .await;
        }
    }
}

/// Approving automatic edits can raise a lower rung, but must not revoke Full Access.
fn approve_auto_edits(task: &mut Task) {
    if !matches!(task.perm.as_str(), "full" | "turbo") {
        task.perm = "auto".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_user_delivers_without_waiting_in_plan_and_disabled_modes() {
        use crate::agent::tests::{harness, task};

        let home = std::env::temp_dir().join(format!("openleash-notice-{}", uuid::Uuid::new_v4()));
        let _home = store::test_home(&home);
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut t = task("notice", "unused/model");
        t.plan = true;
        t.perm = "disabled".into();
        let (h, events) = harness(store::Settings::default(), vec![t]);
        let agent = Agent {
            h: h.clone(),
            task_id: "notice".into(),
            sub: None,
            cancel: CancellationToken::new(),
        };
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            agent.exec_inner(
                "notify_user",
                &json!({"title":"Unity modal", "message":"Dismiss the Unity modal when convenient."}),
                None,
                "notice-call",
            ),
        )
        .await
        .expect("informational notices must never wait for a user response")
        .expect("the main agent can deliver an informational notice");
        assert!(output.contains("Delivered") && output.contains("not waiting"));
        assert!(
            output.contains("does not confirm"),
            "delivery is not proof of viewing"
        );
        let t = h.task("notice").await.unwrap();
        let t = t.lock().await;
        assert_eq!(t.status, "running");
        assert!(t.waiting_kind.is_none());
        assert!(t.messages.is_empty());
        let notice = t.items.iter().find(|i| i.kind == "user_notice").unwrap();
        assert_eq!(notice.text, "Dismiss the Unity modal when convenient.");
        assert_eq!(
            notice.data,
            json!({"title":"Unity modal", "level":"info", "dismissed":false})
        );
        let id = notice.id.clone();
        drop(t);
        assert!(h.runtime("notice").pending.lock().await.is_empty());
        assert!(events.lock().unwrap().iter().any(|(name, data)| {
            name == "ol://attention" && data["kind"] == "notice" && data["item_id"] == id
        }));
        let saved = store::read_task_full(&store::task_path("notice")).unwrap();
        assert!(saved.items.iter().any(|i| i.id == id));

        // Validation errors must not create a card or emit another notification.
        for input in [
            json!({"message":"message"}),
            json!({"title":" \n", "message":"message"}),
            json!({"title":42, "message":"message"}),
            json!({"title":"title"}),
            json!({"title":"title", "message":"\t"}),
            json!({"title":"title", "message":false}),
            json!({"title":"title", "message":"message", "level":"error"}),
            json!({"title":"title", "message":"message", "level":null}),
        ] {
            assert!(agent
                .exec_inner("notify_user", &input, None, "invalid")
                .await
                .is_err());
        }
        let input = json!({"title":"  Heading  ", "message":"  Message  ", "level":"warning"});
        agent
            .exec_inner("notify_user", &input, None, "warning")
            .await
            .unwrap();
        let t = h.task("notice").await.unwrap();
        let t = t.lock().await;
        assert_eq!(t.items.len(), 2);
        assert_eq!(t.items[1].text, "Message");
        assert_eq!(t.items[1].data["title"], "Heading");
        assert_eq!(t.items[1].data["level"], "warning");
        drop(t);

        // Tool declarations are not authority: a fabricated sub-agent call is
        // refused even for workers with the full tool policy.
        for policy in ["all", "read_only", "no_shell"] {
            let subagent = Agent {
                sub: Some(SubCtx {
                    id: "worker".into(),
                    def: AgentDef {
                        tools: policy.into(),
                        ..Default::default()
                    },
                }),
                ..agent.clone()
            };
            let error = subagent
                .exec_inner("notify_user", &input, None, "sub")
                .await
                .unwrap_err();
            assert!(error.contains("Only the main agent"));
        }
        h.settings.write().await.allow.push(AllowRule {
            pattern: "!notify_user".into(),
            project: String::new(),
        });
        let error = agent
            .exec_inner("notify_user", &input, None, "denied")
            .await
            .unwrap_err();
        assert!(
            error.contains("Denied by the rule"),
            "user denies remain authoritative: {error}"
        );
        assert_eq!(h.task("notice").await.unwrap().lock().await.items.len(), 2);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, data)| { name == "ol://attention" && data["kind"] == "notice" })
                .count(),
            2
        );
        let _ = std::fs::remove_dir_all(home);
        });
    }

    #[test]
    fn notify_user_reports_persistence_failure_without_delivery() {
        use crate::agent::tests::{harness, task};

        let home =
            std::env::temp_dir().join(format!("openleash-notice-fail-{}", uuid::Uuid::new_v4()));
        let _home = store::test_home(&home);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                // A file instead of the data directory forces a portable save failure.
                std::fs::write(&home, "blocked").unwrap();
                let (h, events) = harness(
                    store::Settings::default(),
                    vec![task("notice", "unused/model")],
                );
                let agent = Agent {
                    h: h.clone(),
                    task_id: "notice".into(),
                    sub: None,
                    cancel: CancellationToken::new(),
                };
                let result = agent
                    .exec_inner(
                        "notify_user",
                        &json!({"title":"Heading", "message":"Message"}),
                        None,
                        "failed-notice",
                    )
                    .await;
                assert!(
                    result.is_err(),
                    "failed persistence must not report delivery: {result:?}"
                );
                assert!(h
                    .task("notice")
                    .await
                    .unwrap()
                    .lock()
                    .await
                    .items
                    .is_empty());
                assert!(h.user_notices().await.is_empty());
                assert!(
                    events.lock().unwrap().is_empty(),
                    "no card or attention before persistence"
                );
                assert!(h.runtime("notice").pending.lock().await.is_empty());
                std::fs::remove_file(&home).unwrap();
                // Retry can deliver once the disk problem is fixed.
                agent
                    .exec_inner(
                        "notify_user",
                        &json!({"title":"Heading", "message":"Message"}),
                        None,
                        "retry-notice",
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    store::read_task_full(&store::task_path("notice"))
                        .unwrap()
                        .items
                        .len(),
                    1
                );
                std::fs::remove_dir_all(home).unwrap();
            });
    }

    #[tokio::test]
    async fn artifact_preview_attaches_to_existing_item_without_project_storage() {
        use crate::agent::tests::{harness, task};

        let home = std::env::temp_dir().join(format!(
            "openleash-artifact-preview-home-{}",
            uuid::Uuid::new_v4()
        ));
        let _home = store::test_home(&home);
        let project = std::env::temp_dir().join(format!(
            "openleash-artifact-preview-project-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();

        let mut t = task("artifact-preview", "unused/model");
        t.project = project_path;
        t.cwd = project.to_string_lossy().into_owned();
        t.perm = "disabled".into();
        t.plan = true;
        let (h, _events) = harness(store::Settings::default(), vec![t]);
        let agent = Agent {
            h: h.clone(),
            task_id: "artifact-preview".into(),
            sub: None,
            cancel: CancellationToken::new(),
        };
        let row = Item::new(
            "tool",
            "",
            json!({"name":"artifact_preview", "status":"running", "tool_use_id":"preview-call"}),
        );
        let row_id = row.id.clone();
        h.upsert_item("artifact-preview", row).await;
        let input = json!({"title":"Live preview", "kind":"html", "content":"<h1>Inline</h1>"});

        agent
            .exec_inner("artifact_preview", &input, Some(&row_id), "preview-call")
            .await
            .expect("temporary preview should be available in plan and disabled modes");

        let task = h.task("artifact-preview").await.unwrap();
        let task = task.lock().await;
        let row = task.items.iter().find(|item| item.id == row_id).unwrap();
        assert_eq!(row.data["artifact_card"]["title"], "Live preview");
        assert_eq!(row.data["artifact_card"]["kind"], "html");
        assert_eq!(row.data["artifact_card"]["persistence"], "session");
        assert!(row.data["artifact_card"]["artifact_id"].is_null());
        assert!(!project.join(".openleash/artifacts").exists());
        drop(task);

        // attach_inputs runs once the model turn is assembled; it adds the exact
        // source to the same item without replacing its card metadata.
        attach_inputs(
            &h,
            "artifact-preview",
            None,
            &[json!({"type":"tool_use", "id":"preview-call", "input":input})],
            &[("preview-call".into(), row_id.clone())],
        )
        .await;
        let task = h.task("artifact-preview").await.unwrap();
        let task = task.lock().await;
        let row = task.items.iter().find(|item| item.id == row_id).unwrap();
        assert_eq!(row.data["input"]["content"], "<h1>Inline</h1>");
        assert_eq!(row.data["artifact_card"]["persistence"], "session");
        drop(task);

        let subagent = Agent {
            h: h.clone(),
            task_id: "artifact-preview".into(),
            sub: Some(SubCtx {
                id: "worker".into(),
                def: AgentDef {
                    id: "general".into(),
                    tools: "all".into(),
                    ..Default::default()
                },
            }),
            cancel: CancellationToken::new(),
        };
        let error = subagent
            .exec_inner("artifact_preview", &input, Some(&row_id), "sub-preview")
            .await
            .unwrap_err();
        assert!(
            error.contains("only the main agent"),
            "unexpected refusal: {error}"
        );
        assert!(crate::artifacts::list(&project.to_string_lossy())
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn artifact_mutations_bind_project_path_and_keep_write_policy() {
        use crate::agent::tests::{harness, task};

        let home = std::env::temp_dir().join(format!(
            "openleash-artifact-dispatch-home-{}",
            uuid::Uuid::new_v4()
        ));
        let _home = store::test_home(&home);
        let project = std::env::temp_dir().join(format!(
            "openleash-artifact-dispatch-project-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();

        let mut t = task("artifact-dispatch", "unused/model");
        t.project = project_path.clone();
        // Deliberately different in principle: permission checks use the
        // isolated working directory, but artifact storage uses task.project.
        t.cwd = project_path.clone();
        t.perm = "full".into();
        let (h, _events) = harness(store::Settings::default(), vec![t]);
        let agent = Agent {
            h: h.clone(),
            task_id: "artifact-dispatch".into(),
            sub: None,
            cancel: CancellationToken::new(),
        };
        let create = json!({
            "title":"Dispatch test", "kind":"markdown", "content":"# Artifact",
            "decisions":[], "constraints":[], "code_refs":[]
        });
        let row = Item::new(
            "tool",
            "",
            json!({"name":"artifact_create", "status":"running", "tool_use_id":"create-call"}),
        );
        let create_row_id = row.id.clone();
        h.upsert_item("artifact-dispatch", row).await;
        let created = agent
            .exec_inner(
                "artifact_create",
                &create,
                Some(&create_row_id),
                "create-call",
            )
            .await
            .expect(
                "full-access mutation should pass through the normal dispatch and permission gate",
            );
        let artifact: crate::artifacts::AgentArtifact = serde_json::from_str(&created).unwrap();
        assert_eq!(artifact.title, "Dispatch test");
        {
            let task = h.task("artifact-dispatch").await.unwrap();
            let task = task.lock().await;
            let row = task
                .items
                .iter()
                .find(|item| item.id == create_row_id)
                .unwrap();
            assert_eq!(row.data["artifact_card"]["title"], artifact.title);
            assert_eq!(row.data["artifact_card"]["kind"], artifact.kind);
            assert_eq!(row.data["artifact_card"]["persistence"], "project");
            assert_eq!(row.data["artifact_card"]["artifact_id"], artifact.id);
            assert_eq!(
                row.data["artifact_card"]["version_id"],
                artifact.current_version_id
            );
        }
        assert!(project
            .join(".openleash/artifacts")
            .join(format!("{}.json", artifact.id))
            .is_file());

        let private_annotation = "PRIVATE UNSENT REVIEW NOTE: do not disclose";
        crate::artifacts::annotate(
            &project_path,
            &artifact.id,
            &artifact.current_version_id,
            private_annotation,
            Some(json!({"selector": "h1", "target": "Artifact heading"})),
        )
        .unwrap();

        let row = Item::new(
            "tool",
            "",
            json!({"name":"artifact_revise", "status":"running", "tool_use_id":"revise-call"}),
        );
        let revise_row_id = row.id.clone();
        h.upsert_item("artifact-dispatch", row).await;
        let revised_output = agent
            .exec_inner(
                "artifact_revise",
                &json!({
                    "id": artifact.id,
                    "parent_version_id": artifact.current_version_id,
                    "title":"Dispatch test revised", "kind":"markdown", "content":"# Updated",
                    "decisions":[], "constraints":[], "code_refs":[]
                }),
                Some(&revise_row_id),
                "revise-call",
            )
            .await
            .expect("revision should use the same bound project path");
        assert!(
            !revised_output.contains(private_annotation),
            "the raw tool response must not disclose an unsubmitted annotation"
        );
        let revised: crate::artifacts::AgentArtifact =
            serde_json::from_str(&revised_output).unwrap();
        let revised_wire = serde_json::to_string(&revised).unwrap();
        assert_eq!(revised.versions.len(), 2);
        {
            let task = h.task("artifact-dispatch").await.unwrap();
            let task = task.lock().await;
            let row = task
                .items
                .iter()
                .find(|item| item.id == revise_row_id)
                .unwrap();
            assert_eq!(row.data["artifact_card"]["title"], revised.title);
            assert_eq!(row.data["artifact_card"]["kind"], revised.kind);
            assert_eq!(row.data["artifact_card"]["persistence"], "project");
            assert_eq!(row.data["artifact_card"]["artifact_id"], revised.id);
            assert_eq!(
                row.data["artifact_card"]["version_id"],
                revised.current_version_id
            );
        }
        assert!(revised.submitted_annotations.is_empty());
        assert!(!revised_wire.contains(private_annotation));
        assert!(
            !revised_wire.contains("Make the heading clearer"),
            "feedback has not been submitted yet at revision time"
        );

        crate::artifacts::feedback_submit(
            &project_path,
            &revised.id,
            &revised.current_version_id,
            "Make the heading clearer",
            vec![],
            None,
        )
        .unwrap();
        let feedback_id =
            crate::artifacts::feedback_list_for_agent(&project_path, Some(&revised.id)).unwrap()[0]
                .id
                .clone();
        let response_output = agent
            .exec_inner(
                "artifact_respond",
                &json!({"feedback_id": feedback_id, "decision":"addressed", "response":"Updated the heading."}),
                None,
                "respond-call",
            )
            .await
            .expect("response should use the same bound project path");
        assert!(
            !response_output.contains(private_annotation),
            "the raw tool response must not disclose an unsubmitted annotation"
        );
        let response: crate::artifacts::AgentArtifact =
            serde_json::from_str(&response_output).unwrap();
        let response_wire = serde_json::to_string(&response).unwrap();
        assert_eq!(response.feedback[0].status, "responded");
        assert!(response_wire.contains("Make the heading clearer"));
        assert!(response.submitted_annotations.is_empty());
        assert!(!response_wire.contains(private_annotation));

        // Plan mode must block every artifact mutation before the backend writes.
        h.update_task("artifact-dispatch", |t| t.plan = true).await;
        for (name, input) in [
            ("artifact_create", create.clone()),
            (
                "artifact_revise",
                json!({
                    "id": revised.id,
                    "parent_version_id": revised.current_version_id,
                    "title":"Blocked", "kind":"markdown", "content":"# Blocked",
                    "decisions":[], "constraints":[], "code_refs":[]
                }),
            ),
            (
                "artifact_respond",
                json!({
                    "feedback_id": "guessed-feedback-id", "decision":"addressed", "response":"Blocked"
                }),
            ),
        ] {
            let error = agent
                .exec_inner(name, &input, None, "plan-call")
                .await
                .unwrap_err();
            assert!(
                error.contains("Plan mode is active"),
                "{name}: unexpected refusal: {error}"
            );
        }
        assert_eq!(crate::artifacts::list(&project_path).unwrap().len(), 1);

        // Schemas may be omitted for a read-only worker, but dispatch remains a
        // hard backstop for a replayed/crafted tool call.
        let read_only = Agent {
            h: h.clone(),
            task_id: "artifact-dispatch".into(),
            sub: Some(SubCtx {
                id: "read-only-worker".into(),
                def: AgentDef {
                    id: "explore".into(),
                    tools: "read_only".into(),
                    ..Default::default()
                },
            }),
            cancel: CancellationToken::new(),
        };
        let error = read_only
            .exec_inner("artifact_create", &create, None, "readonly-call")
            .await
            .unwrap_err();
        assert!(
            error.contains("can't use artifact_create"),
            "unexpected refusal: {error}"
        );
        assert_eq!(crate::artifacts::list(&project_path).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn automatic_edit_approval_preserves_full_access() {
        for perm in ["full", "turbo", "auto", "ask", "allowlist", "disabled"] {
            let mut t = crate::agent::tests::task("edit-perm", "unused/model");
            t.perm = perm.into();
            approve_auto_edits(&mut t);
            let expected = if matches!(perm, "full" | "turbo") {
                perm
            } else {
                "auto"
            };
            assert_eq!(t.perm, expected, "permission {perm}");
        }
    }

    #[tokio::test]
    async fn exiting_plan_preserves_full_access_and_legacy_auto_upgrade() {
        use crate::agent::tests::{harness, task};

        let dir =
            std::env::temp_dir().join(format!("openleash-plan-perm-{}", uuid::Uuid::new_v4()));
        let _home = store::test_home(&dir);
        for perm in ["full", "turbo", "auto", "ask", "allowlist", "disabled"] {
            for decision in ["always", "once", "deny"] {
                let mut t = task("plan-perm", "unused/model");
                t.plan = true;
                t.perm = perm.into();
                let (h, _events) = harness(store::Settings::default(), vec![t]);
                let agent = Agent {
                    h: h.clone(),
                    task_id: "plan-perm".into(),
                    sub: None,
                    cancel: CancellationToken::new(),
                };
                let exit = tokio::spawn(async move {
                    agent
                        .exit_plan(&json!({"plan": "Implement and test the fix."}))
                        .await
                });
                let tx = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let rt = h.runtime("plan-perm");
                        let mut pending = rt.pending.lock().await;
                        if let Some(id) = pending.keys().next().cloned() {
                            break pending.remove(&id).unwrap();
                        }
                        drop(pending);
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("plan approval must register");
                tx.send(json!({"decision": decision})).unwrap();
                let result = exit.await.unwrap();
                assert_eq!(result.is_ok(), decision != "deny");
                let t = h.task("plan-perm").await.unwrap();
                let t = t.lock().await;
                assert_eq!(t.plan, decision == "deny");
                let expected = if decision == "always" && !matches!(perm, "full" | "turbo") {
                    "auto"
                } else {
                    perm
                };
                assert_eq!(t.perm, expected, "permission {perm}, decision {decision}");
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    fn file_regex_agent(group: &str, pattern: &str) -> AgentDef {
        AgentDef {
            id: "restricted-agent".into(),
            name: "Restricted Agent".into(),
            groups: vec![store::ToolGroup::Restricted((
                group.into(),
                store::GroupOpts {
                    file_regex: pattern.into(),
                    description: "test restriction".into(),
                },
            ))],
            ..AgentDef::default()
        }
    }

    #[test]
    fn invalid_file_regex_denies_writes_and_bash_but_not_read_only_tools() {
        let agent = file_regex_agent("edit", "(unclosed");
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        for tool in ["edit_file", "multi_edit", "write_file", "bash"] {
            let denial =
                subagent_file_restriction_denial(&agent, tool, Some("docs/readme.md"), &cwd);
            assert!(
                denial.is_some_and(|reason| reason.contains("FileRestrictionError")),
                "invalid fileRegex must deny {tool}"
            );
        }

        for tool in ["read_file", "glob", "grep", "view_image", "bash_output"] {
            assert!(
                subagent_file_restriction_denial(&agent, tool, Some("docs/readme.md"), &cwd,)
                    .is_none(),
                "invalid fileRegex must not block read-only tool {tool}"
            );
        }
    }

    #[test]
    fn uppercase_edit_group_enforces_valid_file_regex() {
        let agent = file_regex_agent("Edit", r"^docs/.*\.md$");
        let root =
            std::env::temp_dir().join(format!("openleash-file-regex-{}", std::process::id()));
        let docs = root.join("docs");
        let src = root.join("src");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(docs.join("readme.md"), "docs").unwrap();
        std::fs::write(src.join("lib.rs"), "source").unwrap();
        let cwd = root.to_string_lossy().into_owned();
        let matching_path = docs.join("readme.md").to_string_lossy().into_owned();

        let matching_path_denial =
            subagent_file_restriction_denial(&agent, "write_file", Some(&matching_path), &cwd);
        assert!(
            matching_path_denial.is_none(),
            "valid fileRegex must allow a matching path: {matching_path_denial:?}"
        );
        assert!(
            subagent_file_restriction_denial(&agent, "write_file", Some("src/lib.rs"), &cwd,)
                .is_some(),
            "uppercase Edit must still restrict a non-matching path"
        );
        assert!(
            subagent_file_restriction_denial(&agent, "bash", None, &cwd).is_some(),
            "bash must remain denied while a valid fileRegex is active"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validates_todo_updates_before_persisting() {
        let valid = json!({"todos": [
            {"content":" Inspect the UI ","activeForm":" Inspecting the UI ","status":"in_progress"},
            {"content":"Run tests","activeForm":"Running tests","status":"pending"}
        ]});
        let todos = parse_todos(&valid).unwrap();
        assert_eq!(todos[0].content, "Inspect the UI");
        assert_eq!(todos[0].active_form, "Inspecting the UI");
        assert!(parse_todos(&json!({"todos": [
            {"content":"One","status":"in_progress"},
            {"content":"Two","status":"in_progress"}
        ]}))
        .is_err());
        assert!(parse_todos(&json!({"todos": [{"content":"One","status":"unknown"}]})).is_err());
        assert!(parse_todos(&json!({"todos": [{"content":" ","status":"pending"}]})).is_err());
    }

    #[test]
    fn file_io_calls_do_not_enter_doom_loop_counting() {
        for name in [
            "read_file",
            "edit_file",
            "multi_edit",
            "write_file",
            "view_image",
            "glob",
            "grep",
            "bash_output",
        ] {
            assert!(
                !Agent::counts_for_doom_loop(name),
                "file I/O tool {name} must not reach repeat counting"
            );
        }
        assert!(Agent::counts_for_doom_loop("bash"));
        let main_key = Agent::repeat_key(None, "bash", &json!({"command":"npm test"}));
        let sub_key = Agent::repeat_key(Some("sub-1"), "bash", &json!({"command":"npm test"}));
        assert_ne!(
            main_key, sub_key,
            "repeat counters have an agent-local scope"
        );
    }

    #[test]
    fn repairs_dangling_tool_calls() {
        let mut m = vec![
            Message::user_text("hi"),
            Message {
                role: "assistant".into(),
                content: vec![
                    json!({"type":"tool_use","id":"a","name":"x","input":{}}),
                    json!({"type":"tool_use","id":"b","name":"x","input":{}}),
                ],
                model: String::new(),
            },
            Message::user(vec![
                json!({"type":"tool_result","tool_use_id":"a","content":"ok"}),
            ]),
            Message {
                role: "assistant".into(),
                content: vec![json!({"type":"tool_use","id":"c","name":"x","input":{}})],
                model: String::new(),
            },
        ];
        fix_dangling(&mut m);
        assert_eq!(m.len(), 5);
        assert_eq!(m[2].content.len(), 2);
        assert_eq!(m[2].content[0]["tool_use_id"], "b");
        assert_eq!(m[4].content[0]["tool_use_id"], "c");
    }

    #[test]
    fn prunes_old_screenshots_in_bulk() {
        let shot = |i: usize| {
            Message::user(vec![
                json!({"type":"tool_result","tool_use_id":format!("s{i}"),"content":[{"type":"text","text":"x"},{"type":"image","source":{}}]}),
            ])
        };
        let mut m: Vec<Message> = (0..6).map(shot).collect();
        assert!(!prune_images(&mut m), "under the threshold nothing changes");
        m.push(shot(6));
        assert!(prune_images(&mut m));
        let imgs = m
            .iter()
            .filter(|x| x.content[0]["content"][1]["type"] == "image")
            .count();
        assert_eq!(imgs, KEEP_IMAGES);
        assert_eq!(
            m[6].content[0]["content"][1]["type"], "image",
            "newest kept"
        );
    }

    #[test]
    fn stop_note_lists_cut_off_calls() {
        let mut m = vec![
            Message::user_text("build it"),
            Message {
                role: "assistant".into(),
                content: vec![
                    json!({"type":"tool_use","id":"a","name":"read_file","input":{"path":"x.rs"}}),
                    json!({"type":"tool_use","id":"b","name":"bash","input":{"command":"cargo build"}}),
                    json!({"type":"tool_use","id":"c","name":"write_file","input":{"path":"y.rs"}}),
                    json!({"type":"tool_use","id":"d","name":"task","input":{}}),
                ],
                model: String::new(),
            },
            Message::user(vec![
                json!({"type":"tool_result","tool_use_id":"a","content":"ok"}),
                json!({"type":"tool_result","tool_use_id":"b","content":"Compiling…\n[Interrupted by user]","is_error":true}),
            ]),
        ];
        fix_dangling(&mut m);
        let n = stop_note(&m);
        assert!(
            n.contains("bash: cargo build") && n.contains("write_file: y.rs"),
            "{n}"
        );
        assert!(
            !n.contains("read_file") && !n.contains("- task"),
            "finished calls and sub-agents are left out: {n}"
        );
        assert!(stop_note(&[Message::user_text("hi")]).contains("No tool calls were cut off"));
    }

    /// The model parking a task must end the turn *there*: the tool is the model
    /// saying "I am done until the trigger", and going round again would spend one
    /// more request — and one more turn for the user to read — on a task nobody
    /// asked to continue. Uses the crate's scripted HTTP mock, so this is the real
    /// loop, not a hand-call of the park path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parking_a_task_ends_the_turn_without_another_request() {
        use crate::agent::tests::{custom, harness, openai_call, task, Mock};
        let _g = router::ROUTES_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("openleash-park-turn-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let m = Mock::start().await;
        let (cp, key) = custom("pk", &m.base, false);
        let mut s = store::Settings::default();
        s.custom_providers.push(cp);
        s.providers.insert(key.0, key.1);
        let mut t = task("tp", "pk/m");
        t.messages = vec![Message::user_text("watch the build")];
        let (h, _events) = harness(s, vec![t]);

        m.push(openai_call(
            "wait_for_event",
            json!({"source": "timer", "reason": "the build", "delay_s": 600}),
        ));
        let before = m.reqs().len();
        let agent = Agent {
            h: h.clone(),
            task_id: "tp".into(),
            sub: None,
            cancel: CancellationToken::new(),
        };
        let res = agent.run_loop(None).await;
        assert!(res.is_ok(), "the park ended the turn cleanly: {res:?}");
        assert_eq!(
            m.reqs().len() - before,
            1,
            "one request, then the turn ended — parking must not send a second"
        );
        assert!(
            h.runtime("tp").wake_state.lock().unwrap().parked,
            "the tool armed the trigger before the turn ended"
        );
        // Tear the (still-parked) task down so its watcher cannot outlive the test.
        stop_tokens(&h, "tp");
        let _ = std::fs::remove_dir_all(dir);
    }
}
