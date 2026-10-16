//! `openleash-acp` — an Agent Client Protocol (ACP) server over stdio.
//!
//! This is the editor-facing entry point: the client (Zed, JetBrains, VS Code,
//! Neovim, …) launches this binary as a subprocess and drives it with JSON-RPC.
//! It boots the same `agent::Harness` the desktop app boots, but with a bus that
//! translates harness events into ACP `session/update` notifications instead of a
//! webview.
//!
//! # Spec
//!
//! Implemented against **ACP v1** (`protocolVersion: 1`):
//!
//! - Transports — stdio, **newline-delimited** JSON-RPC (each message is one
//!   line and MUST NOT contain embedded newlines):
//!   <https://agentclientprotocol.com/protocol/v1/transports>
//! - Initialization / capability negotiation / version negotiation:
//!   <https://agentclientprotocol.com/protocol/v1/initialization>
//! - Session setup (`session/new`, `session/load`, `session/resume`):
//!   <https://agentclientprotocol.com/protocol/v1/session-setup>
//! - Prompt turn (`session/prompt`, `session/update`, stop reasons, `session/cancel`):
//!   <https://agentclientprotocol.com/protocol/v1/prompt-turn>
//! - Tool calls + `session/request_permission`:
//!   <https://agentclientprotocol.com/protocol/v1/tool-calls>
//! - Overview / method list: <https://agentclientprotocol.com/protocol/v1/overview>
//!
//! # Mapping onto the harness
//!
//! - An ACP **session is an app task**: `session/new` mints a task id with the
//!   same `new_id()` the desktop app uses and persists it, so an ACP session
//!   shows up in the app's chat list and can be resumed there. The session id we
//!   return to the client *is* that task id, so `session/load`/`session/resume`
//!   address the app's own task store rather than a parallel one.
//! - Assistant/thinking text, tool calls and tool results are translated from
//!   the harness's `ol://event` bus (`item`/`delta` kinds) into
//!   `agent_message_chunk` / `agent_thought_chunk` / `tool_call` /
//!   `tool_call_update`.
//! - **Permissions go through the existing layer, never around it.** When the
//!   harness raises an `approval` item (because `permissions::check` returned
//!   `Ask`), we forward it to the client as `session/request_permission` and
//!   answer the harness with the client's choice (`once` / `always` / deny). The
//!   task runs under the `allowlist` permission mode, so risky tools really do
//!   ask. Nothing is ever auto-approved: a transport error, a cancelled outcome
//!   or an unknown option all resolve to *deny*.
//!
//! # What is deliberately not implemented (see the handoff report)
//!
//! - Client-provided `mcpServers` are accepted in the payload and ignored.
//! - Harness `question` items (`ask` / `ask_user`) have no ACP v1 equivalent
//!   here (elicitation is out of scope), so they are dismissed with a note.
//! - Subagent (`sitem`) activity is not forwarded.
//! - `notice`/`usage_update`/plan updates are not emitted.

use openleash_lib::agent::{
    docker, new_id, providers, router, runner, stats, store, trust, Harness, Task,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// The protocol major version this server speaks. See the "Protocol version"
/// section of the initialization spec: it is a single integer and is only bumped
/// on breaking changes.
const PROTOCOL_VERSION: i64 = 1;

const AGENT_NAME: &str = "openleash-acp";
const AGENT_TITLE: &str = "OpenLeash";
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The harness event bus: the `(event name, payload)` pairs the app publishes.
/// A named alias so the `dyn Fn` type is written once.
pub type Bus = Arc<dyn Fn(&str, Value) + Send + Sync>;

// JSON-RPC 2.0 error codes (the ACP schema reuses the standard ones).
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

// ────────────────────────────── sessions ──────────────────────────────

/// One ACP session. The session id the client holds *is* `task_id`, so the
/// session is the app's own task rather than a parallel store.
struct Session {
    task_id: String,
    /// Set by the event pump when the run announces a terminal state, so the
    /// prompt handler knows the turn ended without polling `Task`.
    attention: Mutex<Option<String>>,
    /// Set by `session/cancel`, so the prompt response can report `cancelled`.
    cancelled: AtomicBool,
}

impl Session {
    fn new(task_id: String) -> Self {
        Session {
            task_id,
            attention: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        }
    }
}

// ────────────────────────────── server ──────────────────────────────

/// The JSON-RPC endpoint. One per process.
///
/// Writes are serialized through `out`; every message the transport emits is a
/// single line. Reads are turned into calls to [`Server::dispatch`].
pub struct Server {
    h: Arc<Harness>,
    out: Mutex<Box<dyn Write + Send>>,
    /// Ids for the requests *we* send to the client (e.g. `session/request_permission`).
    next_out_id: AtomicI64,
    /// Our outstanding requests, awaiting the client's response.
    awaiting: Mutex<HashMap<i64, oneshot::Sender<Result<Value, Value>>>>,
    /// Live sessions, keyed by session id (== task id).
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Text item ids already streamed, so the harness's final re-publish of a
    /// completed message is not appended a second time.
    seen_text: Mutex<HashSet<String>>,
    /// Tool item ids already announced with a `tool_call`.
    started_tools: Mutex<HashSet<String>>,
    /// `approval`/`question` item ids already forwarded, so a re-emit (the patch
    /// that records the resolution) does not open the prompt twice.
    handled: Mutex<HashSet<String>>,
}

/// Build the same `Harness` the desktop app boots, with `bus` in place of the
/// webview. Split out so tests can point the bus at a channel of their own.
pub fn boot(settings: store::Settings, tasks: Vec<Task>, bus: Bus) -> Arc<Harness> {
    providers::set_custom_models(&settings.model_configs);
    providers::set_model_visibility(&settings.disabled_models, &settings.removed_models);
    router::set_routes(&settings.routes);
    trust::set_trust(&settings.trust);

    let mut map: HashMap<String, openleash_lib::agent::TaskRef> = HashMap::new();
    for t in tasks {
        if let Some(d) = &t.docker {
            docker::register(&t.id, d);
        }
        map.insert(t.id.clone(), Arc::new(tokio::sync::Mutex::new(t)));
    }
    let h = Arc::new(Harness {
        bus,
        tasks: tokio::sync::RwLock::new(map),
        runtimes: Default::default(),
        settings: tokio::sync::RwLock::new(settings),
        bg: Default::default(),
        mcp: Default::default(),
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new()),
        pause_bell: Default::default(),
        accts: Default::default(),
        keys: Default::default(),
        stats: stats::Stats::load(),
        dirty: Default::default(),
        saver: Default::default(),
        settings_dirty: Default::default(),
        me: Default::default(),
    });
    Harness::register(&h);
    h
}

/// Assemble a `Server` plus the receiver of the harness event stream it will be
/// fed by [`pump`]. Exposed for tests, which drive the same wiring in-process.
pub fn make_server(
    settings: store::Settings,
    tasks: Vec<Task>,
    out: Box<dyn Write + Send>,
) -> (Arc<Server>, mpsc::UnboundedReceiver<(String, Value)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let bus: Bus = Arc::new(move |name: &str, v: Value| {
        let _ = tx.send((name.to_string(), v));
    });
    let h = boot(settings, tasks, bus);
    (Arc::new(Server::new(h, out)), rx)
}

impl Server {
    fn new(h: Arc<Harness>, out: Box<dyn Write + Send>) -> Self {
        Server {
            h,
            out: Mutex::new(out),
            next_out_id: AtomicI64::new(1),
            awaiting: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            seen_text: Mutex::new(HashSet::new()),
            started_tools: Mutex::new(HashSet::new()),
            handled: Mutex::new(HashSet::new()),
        }
    }

    // ── transport ──────────────────────────────────────────────────────────

    /// Write one JSON-RPC message as a single line. The stdio transport forbids
    /// embedded newlines; `serde_json::to_string` escapes any in strings, so a
    /// message is always exactly one line.
    fn write_line(&self, v: Value) {
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    fn result(&self, id: &Value, result: Value) {
        self.write_line(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn error(&self, id: &Value, code: i64, message: &str) {
        self.write_line(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }));
    }

    fn session_update(&self, task_id: &str, update: Value) {
        self.write_line(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": task_id, "update": update},
        }));
    }

    /// Send a request to the client and await its response (JSON-RPC id
    /// correlation). Used for `session/request_permission`.
    async fn request(&self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_out_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.awaiting.lock().unwrap().insert(id, tx);
        self.write_line(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        match rx.await {
            Ok(r) => r,
            Err(_) => Err(json!({"code": INTERNAL_ERROR, "message": "client connection closed"})),
        }
    }

    fn route_response(&self, id: i64, result: Result<Value, Value>) {
        if let Some(tx) = self.awaiting.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }

    // ── dispatch ───────────────────────────────────────────────────────────

    async fn dispatch_line(self: &Arc<Self>, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(msg) => self.dispatch(msg).await,
            Err(_) => self.error(&Value::Null, PARSE_ERROR, "the message is not valid JSON"),
        }
    }

    async fn dispatch(self: &Arc<Self>, msg: Value) {
        let id = msg.get("id").cloned();
        match msg.get("method").and_then(Value::as_str) {
            // A request: has both `method` and `id`.
            Some(method) => match id {
                Some(id) => {
                    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                    self.handle_request(id, method, params).await;
                }
                // A notification: `method` without `id`. Never answered.
                None => self.handle_notification(method, msg.get("params")).await,
            },
            // No `method`: a response to one of our own requests.
            None => {
                let Some(idn) = id.as_ref().and_then(Value::as_i64) else {
                    return;
                };
                if let Some(err) = msg.get("error") {
                    self.route_response(idn, Err(err.clone()));
                } else {
                    self.route_response(idn, Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
        }
    }

    async fn handle_request(self: &Arc<Self>, id: Value, method: &str, params: Value) {
        match method {
            "initialize" => self.handle_initialize(id, params).await,
            "session/new" => self.handle_new_session(id, params).await,
            "session/load" => self.handle_load(id, params).await,
            "session/resume" => self.handle_resume(id, params).await,
            // A prompt runs a whole turn, so it must not block the read loop
            // (which still has to deliver `session/cancel` and permission
            // responses).
            "session/prompt" => {
                let me = self.clone();
                tokio::spawn(async move { me.handle_prompt(id, params).await });
            }
            other => self.error(
                &id,
                METHOD_NOT_FOUND,
                &format!("method not found: `{other}`"),
            ),
        }
    }

    async fn handle_notification(self: &Arc<Self>, method: &str, params: Option<&Value>) {
        // Unknown notifications are ignored: JSON-RPC gives them no reply, and
        // there is nothing useful to tell the client.
        if method == "session/cancel" {
            if let Some(p) = params {
                self.handle_cancel(p).await;
            }
        }
    }

    // ── initialize ─────────────────────────────────────────────────────────

    async fn handle_initialize(&self, id: Value, params: Value) {
        // Version negotiation: the client sends the latest version it supports;
        // we answer with the version we will actually speak. We only speak v1, so
        // that is always 1 (a client that wanted something else must close).
        let _requested = params["protocolVersion"].as_i64();
        let result = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false,
                },
                "sessionCapabilities": {"resume": {}},
                "mcpCapabilities": {"http": false, "sse": false},
            },
            "agentInfo": {
                "name": AGENT_NAME,
                "title": AGENT_TITLE,
                "version": AGENT_VERSION,
            },
            "authMethods": [],
        });
        self.result(&id, result);
    }

    // ── session/new ────────────────────────────────────────────────────────

    async fn handle_new_session(&self, id: Value, params: Value) {
        let cwd = params["cwd"].as_str().unwrap_or("").trim().to_string();
        if cwd.is_empty() {
            return self.error(&id, INVALID_PARAMS, "session/new requires `cwd`");
        }
        // `mcpServers` is accepted and ignored (see the module docs).
        let (model, agents) = {
            let s = self.h.settings.read().await;
            (s.model.clone(), s.default_agents.clone())
        };
        let task_id = new_id();
        let task = match new_task(&task_id, &cwd, &model, "allowlist", &agents) {
            Ok(t) => t,
            Err(e) => return self.error(&id, INTERNAL_ERROR, &e),
        };
        self.h
            .tasks
            .write()
            .await
            .insert(task_id.clone(), Arc::new(tokio::sync::Mutex::new(task)));
        self.h.save_task_now(&task_id).await;
        self.sessions
            .lock()
            .unwrap()
            .insert(task_id.clone(), Arc::new(Session::new(task_id.clone())));
        self.result(&id, json!({"sessionId": task_id}));
    }

    // ── session/load & session/resume ──────────────────────────────────────

    async fn handle_load(&self, id: Value, params: Value) {
        let Some((task_id, cwd, msgs)) = self.lookup(&id, &params).await else {
            return;
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(task_id.clone(), Arc::new(Session::new(task_id.clone())));
        // The spec requires the whole conversation to be replayed as updates
        // before the response. We replay the text blocks of the stored messages.
        for m in msgs {
            let kind = match m.role.as_str() {
                "user" => "user_message_chunk",
                "assistant" => "agent_message_chunk",
                _ => continue,
            };
            for b in &m.content {
                if b["type"] == "text" {
                    if let Some(text) = b["text"].as_str() {
                        if !text.is_empty() {
                            self.session_update(
                                &task_id,
                                json!({
                                    "sessionUpdate": kind,
                                    "content": {"type": "text", "text": text},
                                }),
                            );
                        }
                    }
                }
            }
        }
        let _ = cwd;
        self.result(&id, json!({}));
    }

    async fn handle_resume(&self, id: Value, params: Value) {
        let Some((task_id, _, _)) = self.lookup(&id, &params).await else {
            return;
        };
        // Resume restores context without replaying history.
        self.sessions
            .lock()
            .unwrap()
            .insert(task_id.clone(), Arc::new(Session::new(task_id.clone())));
        self.result(&id, json!({}));
    }

    /// Resolve a `sessionId` to an existing app task. Returns `(task_id, cwd, messages)`.
    async fn lookup(
        &self,
        id: &Value,
        params: &Value,
    ) -> Option<(String, String, Vec<openleash_lib::agent::Message>)> {
        let sid = params["sessionId"].as_str().unwrap_or("").to_string();
        if sid.is_empty() {
            self.error(id, INVALID_PARAMS, "`sessionId` is required");
            return None;
        }
        match self.h.task(&sid).await {
            Ok(t) => {
                let t = t.lock().await;
                let cwd = params["cwd"]
                    .as_str()
                    .filter(|c| !c.trim().is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| t.cwd.clone());
                Some((sid, cwd, t.messages.clone()))
            }
            Err(_) => {
                self.error(id, INVALID_PARAMS, &format!("unknown sessionId `{sid}`"));
                None
            }
        }
    }

    // ── session/prompt ─────────────────────────────────────────────────────

    async fn handle_prompt(self: Arc<Self>, id: Value, params: Value) {
        let sid = params["sessionId"].as_str().unwrap_or("").to_string();
        // A session is the app's own task, so an id the client already knows
        // (from `session/list`, or a task it resumed elsewhere) resolves to the
        // task store directly. New/load/resume register eagerly; this is the
        // fallback for an id handed to us without one of those.
        let existing = self.sessions.lock().unwrap().get(&sid).cloned();
        let sess = match existing {
            Some(s) => s,
            None => match self.h.task(&sid).await {
                Ok(_) => {
                    let s = Arc::new(Session::new(sid.clone()));
                    self.sessions.lock().unwrap().insert(sid.clone(), s.clone());
                    s
                }
                Err(_) => {
                    return self.error(&id, INVALID_PARAMS, &format!("unknown sessionId `{sid}`"))
                }
            },
        };
        let text = prompt_text(&params["prompt"]);
        // Clear the previous turn's terminal marker before the new one starts.
        *sess.attention.lock().unwrap() = None;
        sess.cancelled.store(false, Ordering::SeqCst);

        if let Err(e) = runner::send(self.h.clone(), sid.clone(), text).await {
            return self.error(&id, INTERNAL_ERROR, &e);
        }
        let stop = self.wait_turn(&sess).await;
        self.result(&id, json!({"stopReason": stop}));
    }

    /// Block until the turn ends and map the harness's terminal state to an ACP
    /// `StopReason`.
    async fn wait_turn(&self, sess: &Arc<Session>) -> String {
        let task_id = &sess.task_id;
        loop {
            if sess.cancelled.load(Ordering::SeqCst) {
                return "cancelled".to_string();
            }
            if let Some(k) = sess.attention.lock().unwrap().clone() {
                return match k.as_str() {
                    "stopped" if sess.cancelled.load(Ordering::SeqCst) => "cancelled".to_string(),
                    _ => "end_turn".to_string(),
                };
            }
            let running = self.h.runtime(task_id).running.load(Ordering::SeqCst);
            let status = match self.h.task(task_id).await {
                Ok(t) => t.lock().await.status.clone(),
                Err(_) => String::new(),
            };
            if !running && matches!(status.as_str(), "done" | "failed" | "stopped") {
                return if sess.cancelled.load(Ordering::SeqCst) {
                    "cancelled".to_string()
                } else {
                    "end_turn".to_string()
                };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // ── session/cancel ─────────────────────────────────────────────────────

    async fn handle_cancel(&self, params: &Value) {
        let sid = params["sessionId"].as_str().unwrap_or("");
        let sess = self.sessions.lock().unwrap().get(sid).cloned();
        if let Some(sess) = sess {
            sess.cancelled.store(true, Ordering::SeqCst);
            runner::interrupt(&self.h, &sess.task_id).await;
        }
    }

    // ── event pump ─────────────────────────────────────────────────────────

    /// Translate one harness bus event into ACP notifications. Synchronous: it
    /// only serializes to `out` and spawns async work (the permission prompt).
    fn on_event(self: &Arc<Self>, name: &str, payload: Value) {
        match name {
            "ol://event" => {
                let kind = payload["kind"].as_str().unwrap_or("").to_string();
                // The task id is on the envelope; the inner payload is the item
                // (or delta) and does not carry it.
                let task_id = payload["task_id"].as_str().unwrap_or("").to_string();
                let body = &payload["payload"];
                match kind.as_str() {
                    "item" => self.on_item(&task_id, body),
                    "delta" => self.on_delta(&task_id, body),
                    _ => {}
                }
            }
            "ol://attention" => {
                let task_id = payload["task_id"].as_str().unwrap_or("").to_string();
                let kind = payload["kind"].as_str().unwrap_or("");
                if matches!(kind, "done" | "failed" | "stopped" | "waiting") {
                    if let Some(s) = self.sessions.lock().unwrap().get(&task_id).cloned() {
                        *s.attention.lock().unwrap() = Some(kind.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn on_delta(self: &Arc<Self>, task_id: &str, payload: &Value) {
        // Subagent deltas are not forwarded (see module docs).
        if !payload["sub_id"].is_null() {
            return;
        }
        let item_id = payload["item_id"].as_str().unwrap_or("").to_string();
        let text = payload["text"].as_str().unwrap_or("").to_string();
        if item_id.is_empty() || text.is_empty() {
            return;
        }
        self.seen_text.lock().unwrap().insert(item_id.clone());
        self.chunk(task_id, "agent_message_chunk", &item_id, &text);
    }

    fn on_item(self: &Arc<Self>, task_id: &str, item: &Value) {
        let kind = item["kind"].as_str().unwrap_or("");
        let id = item["id"].as_str().unwrap_or("").to_string();
        match kind {
            "text" | "thinking" => {
                if id.is_empty() {
                    return;
                }
                // The first `item` carries the first chunk; later chunks arrive
                // as `delta`; the final re-publish repeats the whole message. Emit
                // only the first sighting so the text is not doubled.
                if !self.seen_text.lock().unwrap().insert(id.clone()) {
                    return;
                }
                let text = item["text"].as_str().unwrap_or("").to_string();
                let ty = if kind == "thinking" {
                    "agent_thought_chunk"
                } else {
                    "agent_message_chunk"
                };
                self.chunk(task_id, ty, &id, &text);
            }
            "tool" => self.on_tool(task_id, item, &id),
            "approval" => {
                if id.is_empty() || !self.handled.lock().unwrap().insert(id.clone()) {
                    return;
                }
                if !item["data"]["resolved"].is_null() {
                    return;
                }
                let me = self.clone();
                let item = item.clone();
                let task_id = task_id.to_string();
                tokio::spawn(async move { handle_approval(me, task_id, item).await });
            }
            "question" => {
                if id.is_empty() || !self.handled.lock().unwrap().insert(id.clone()) {
                    return;
                }
                let data = &item["data"];
                if !data["answers"].is_null() || data["dismissed"].as_bool() == Some(true) {
                    return;
                }
                let me = self.clone();
                let task_id = task_id.to_string();
                tokio::spawn(async move { handle_question(me, task_id, id).await });
            }
            _ => {}
        }
    }

    fn chunk(&self, task_id: &str, session_update: &str, message_id: &str, text: &str) {
        if task_id.is_empty() || text.is_empty() {
            return;
        }
        self.session_update(
            task_id,
            json!({
                "sessionUpdate": session_update,
                "messageId": message_id,
                "content": {"type": "text", "text": text},
            }),
        );
    }

    fn on_tool(self: &Arc<Self>, task_id: &str, item: &Value, id: &str) {
        if task_id.is_empty() || id.is_empty() {
            return;
        }
        let data = &item["data"];
        let name = data["name"].as_str().unwrap_or("").to_string();
        let status = data["status"].as_str().unwrap_or("").to_string();
        let input = data["input"].clone();
        let title = if name.is_empty() {
            "Tool".to_string()
        } else {
            name.clone()
        };
        let kind = tool_kind(&name);

        let fresh = self.started_tools.lock().unwrap().insert(id.to_string());
        if status == "running" {
            // The first `running` item has no input yet; wait for the patch that
            // carries the real arguments before announcing the call.
            if fresh && input.is_null() {
                self.started_tools.lock().unwrap().remove(id);
                return;
            }
            if !self.started_tools.lock().unwrap().contains(id) {
                return;
            }
            // Already announced by an earlier patch; nothing more to say.
            if !fresh {
                return;
            }
            let mut call = json!({
                "sessionUpdate": "tool_call",
                "toolCallId": id,
                "name": name,
                "title": title,
                "kind": kind,
                "status": "in_progress",
            });
            if !input.is_null() {
                call["rawInput"] = input;
            }
            self.session_update(task_id, call);
            return;
        }

        // Finished (ok / error / denied).
        if fresh {
            self.session_update(
                task_id,
                json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": id,
                    "name": name,
                    "title": title,
                    "kind": kind,
                    "status": "pending",
                }),
            );
        }
        let acp_status = match status.as_str() {
            "ok" => "completed",
            _ => "failed",
        };
        let mut update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": acp_status,
        });
        if let Some(output) = data["output"].as_str() {
            if !output.is_empty() {
                update["content"] = json!([
                    {"type": "content", "content": {"type": "text", "text": output}}
                ]);
            }
        }
        if !input.is_null() {
            update["rawInput"] = input;
        }
        self.session_update(task_id, update);
    }
}

/// Feed the harness bus into the server. Runs for the life of the process.
pub async fn pump(server: Arc<Server>, mut rx: mpsc::UnboundedReceiver<(String, Value)>) {
    while let Some((name, payload)) = rx.recv().await {
        server.on_event(&name, payload);
    }
}

// ────────────────────────────── permission bridge ──────────────────────────────

/// Forward a harness approval to the client as `session/request_permission`,
/// then answer the harness with the choice. Never auto-approves: any doubt
/// (transport error, cancelled outcome, unknown option) denies.
async fn handle_approval(server: Arc<Server>, task_id: String, item: Value) {
    let item_id = item["id"].as_str().unwrap_or("").to_string();
    let data = &item["data"];
    let title = data["title"]
        .as_str()
        .or_else(|| item["text"].as_str())
        .unwrap_or("Permission required")
        .to_string();
    let tool_name = data["tool"].as_str().unwrap_or("").to_string();

    let tool_call = json!({
        "toolCallId": item_id,
        "title": title,
        "kind": approval_kind(data["kind"].as_str().unwrap_or("")),
        "status": "pending",
        "rawInput": {
            "tool": tool_name,
            "detail": data["detail"],
            "reason": data["reason"],
            "rule": data["rule"],
        },
    });
    let options = json!([
        {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
        {"optionId": "allow-always", "name": "Allow always", "kind": "allow_always"},
        {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
    ]);

    let resp = server
        .request(
            "session/request_permission",
            json!({
                "sessionId": task_id,
                "toolCall": tool_call,
                "options": options,
            }),
        )
        .await;

    let decision = match resp {
        Ok(v) => match v["outcome"]["outcome"].as_str().unwrap_or("") {
            "selected" => match v["outcome"]["optionId"].as_str().unwrap_or("") {
                "allow-once" => "once",
                "allow-always" => "always",
                // Anything else (reject_once, or an option we did not offer) denies.
                _ => "deny",
            },
            // "cancelled" (or an unrecognised outcome) denies.
            _ => "deny",
        },
        Err(_) => "deny",
    };

    answer_pending(&server.h, &task_id, &item_id, json!({"decision": decision})).await;
}

/// A harness `question` item has no ACP v1 equivalent here (elicitation is out
/// of scope). Dismissing with a note is the harness's own fallback for an
/// unanswered question: the agent proceeds and states its assumptions. It is not
/// an authorization, so this is not a permission bypass.
async fn handle_question(server: Arc<Server>, task_id: String, item_id: String) {
    let note = "This ACP client has no interactive question channel, so the question was \
                skipped. Proceed using your best judgment and state any assumptions.";
    answer_pending(
        &server.h,
        &task_id,
        &item_id,
        json!({"dismissed": true, "note": note}),
    )
    .await;
}

/// Answer a blocking harness prompt the way the app's `task_respond` command
/// does: take the item's oneshot out of the runtime's `pending` map and send the
/// response. The map is filled by `wait_for_user` *after* the item was published,
/// so a very fast client can beat the registration — retry briefly.
async fn answer_pending(h: &Arc<Harness>, task_id: &str, item_id: &str, value: Value) {
    let rt = h.runtime(task_id);
    for _ in 0..300 {
        if let Some(tx) = rt.pending.lock().await.remove(item_id) {
            let _ = tx.send(value);
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The ACP `ToolKind` for an approval's harness `kind` string.
fn approval_kind(kind: &str) -> &'static str {
    match kind {
        "command" => "execute",
        "edit" => "edit",
        "mcp" => "other",
        _ => "other",
    }
}

/// The ACP `ToolKind` for a tool name.
fn tool_kind(name: &str) -> &'static str {
    match name {
        "read_file" | "view_image" | "list_dir" | "glob" | "grep" => "read",
        "write_file" | "edit_file" | "multi_edit" => "edit",
        "bash" | "shell" => "execute",
        "web_search" | "web_fetch" | "browser" => "fetch",
        _ => "other",
    }
}

// ────────────────────────────── task construction ──────────────────────────────

/// Build a fresh task record from a JSON object rather than a field-by-field
/// literal, so a field added to `Task` does not silently rot this copy (the same
/// reason the headless entry point does it this way).
fn new_task(
    id: &str,
    cwd: &str,
    model: &str,
    perm: &str,
    agents: &[String],
) -> Result<Task, String> {
    let now = chrono::Utc::now();
    let mut agents = agents.to_vec();
    store::ensure_required_agents(&mut agents);
    let mut t: Task = serde_json::from_value(json!({
        "id": id,
        "title": "ACP session",
        "status": "idle",
        "step": "",
        "project": cwd,
        "cwd": cwd,
        "branch": "",
        "worktree": false,
        "model": model,
        "effort": 2,
        "perm": perm,
        "plan": false,
        "subagents": true,
        "assist": "default",
        "agents": agents,
        "items": [],
        "messages": [],
        "todos": [],
        "subs": [],
        "usage": {
            "input": 0u64, "output": 0u64, "cache_read": 0u64, "cache_write": 0u64,
            "cost": 0.0f64, "last_context": 0u64,
        },
        "created_at": now,
        "updated_at": now,
        "touched_at": now,
    }))
    .map_err(|e| format!("could not build a task record: {e}"))?;
    // An in-memory task is hydrated by construction; without this the first
    // `task()` would go looking for a file the run has not written yet.
    t.hydrated = true;
    Ok(t)
}

/// Flatten an ACP `ContentBlock[]` prompt into the text the harness will read.
/// Text blocks pass through; resource links and embedded resources contribute
/// their URI (we do not fetch client resources — see the module docs).
fn prompt_text(prompt: &Value) -> String {
    prompt
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| match b["type"].as_str() {
                    Some("text") => b["text"].as_str().map(String::from),
                    Some("resource_link") => b["uri"].as_str().map(|u| format!("[{u}]")),
                    Some("resource") => b["resource"]["uri"].as_str().map(|u| format!("[{u}]")),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

// ────────────────────────────── main ──────────────────────────────

const HELP: &str = "\
openleash-acp — an Agent Client Protocol (ACP) agent over stdio.

The client launches this process and speaks newline-delimited JSON-RPC 2.0 on
stdin/stdout (ACP v1). Diagnostics go to stderr. There are no command-line
arguments.

    initialize · session/new · session/prompt · session/load · session/resume
    session/update (notification) · session/cancel (notification)
    session/request_permission (agent -> client)";

fn main() {
    // stdout is the ACP transport: nothing but JSON-RPC messages may go there.
    // All human-readable diagnostics go to stderr.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }

    let settings = store::load_settings();
    let tasks = store::load_tasks();
    let (server, rx) = make_server(settings, tasks, Box::new(std::io::stdout()));

    // Read stdin on a dedicated thread so the async runtime is never blocked.
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if line_tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("openleash-acp: cannot start the async runtime: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async {
        tokio::spawn(pump(server.clone(), rx));
        while let Some(line) = line_rx.recv().await {
            server.dispatch_line(&line).await;
        }
    });
}

#[cfg(test)]
mod tests;
