//! `openleash-headless` — run one OpenLeash prompt to completion, then exit.
//!
//! This is the non-interactive ("headless") entry point: no Tauri window, no
//! webview, no `AppHandle`. It boots the same `agent::Harness` the desktop app
//! boots, drives a single turn through `agent::runner`, and writes the run to
//! stdout as **NDJSON** (one JSON object per line). stderr carries human
//! progress only, so `openleash-headless ... | jq` and `2>/dev/null` both work.
//!
//! # CLI
//!
//! ```text
//! openleash-headless [OPTIONS] [PROMPT]
//!
//!   -p, --prompt <TEXT>      the prompt to run (also accepted positionally)
//!       --cwd <DIR>          working directory (default: current directory)
//!       --model <ID>         model id (default: the app's saved default model;
//!                            on --resume, the task's own model)
//!       --perm <MODE>        disabled | allowlist | reviewer | auto | turbo
//!                            (default: allowlist; ignored on --resume unless given)
//!       --plan               start the run in plan mode (writes nothing)
//!       --resume <TASK-ID>   continue an existing task instead of starting one
//!       --timeout-secs <N>   give up after N seconds (default 900)
//!       --json               accepted for compatibility; NDJSON is already the
//!                            default and is the only machine format
//!   -h, --help               print this help
//! ```
//!
//! # stdout contract (NDJSON)
//!
//! The first line is always a `session` event; the last is always a `result`
//! event. Everything in between is per-step:
//!
//! ```json
//! {"type":"session","session_id":"…","model":"…","cwd":"…","perm":"…","resumed":false}
//! {"type":"assistant","text":"…"}
//! {"type":"thinking","text":"…"}
//! {"type":"tool_call","name":"read_file","input":{…},"id":"…"}
//! {"type":"tool_result","name":"read_file","status":"ok","output":"…","id":"…"}
//! {"type":"notice","text":"…"}
//! {"type":"result","session_id":"…","ok":true,"exit_code":0,"status":"done",
//!  "model":"…","usage":{"input":0,"output":0,"cache_read":0,"cache_write":0},"cost":0.0}
//! ```
//!
//! The `session_id` reported in the `session` event is a task id, and it is
//! exactly what `--resume <id>` accepts. Subagent steps carry an extra `"sub"`
//! field. Usage is copied from the task's accumulated, **provider-reported**
//! token counts; `cost` is the app's own ledger (reported tokens priced against
//! the model's published rates). Nothing here is estimated.
//!
//! # Exit codes
//!
//! | code | meaning |
//! |-----:|---------|
//! | 0 | the run finished (`status: "done"`) |
//! | 1 | the run failed (`status: "failed"` / `"stopped"`, or a dispatch error) |
//! | 2 | usage error — bad arguments, unknown `--resume` id, no model |
//! | 3 | gave up / hit a limit — the chat paused on provider limits or parked, or `--timeout-secs` elapsed |
//! | 4 | blocked on a prompt (approval / question) that non-interactive mode cannot answer |
//!
//! Code 4 exists because headless mode has nobody to click "Approve": rather
//! than hang forever, it stops and says so. Raise `--perm` (e.g. `auto`) or use
//! `turbo` for genuinely unattended runs; the permission layer itself is
//! unchanged and still enforces whatever mode you name.

use openleash_lib::agent::{
    docker, new_id, providers, router, runner, stats, store, trust, Harness, Task, Usage,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_LIMIT: i32 = 3;
pub const EXIT_NEEDS_INPUT: i32 = 4;

// ────────────────────────────── NDJSON emitter ──────────────────────────────

/// Where NDJSON lines go. Shared with the harness's event bus, so it must be
/// `Send + Sync`; every write goes through one mutex, which is also what keeps
/// the terminal `result` line last.
pub type Sink = Arc<Mutex<Emitter<Box<dyn Write + Send>>>>;

pub fn sink(w: Box<dyn Write + Send>) -> Sink {
    Arc::new(Mutex::new(Emitter::new(w)))
}

/// One streamed text block (assistant reply or reasoning), reassembled from the
/// `item` + `delta` events the run publishes.
struct TextBlock {
    id: String,
    /// The item's own text the first time it was seen — the first streamed chunk.
    base: String,
    /// Concatenated `delta` payloads, which are every chunk *after* the first.
    deltas: String,
    /// The longest `item.text` seen; the run re-publishes the whole block with
    /// the model that served it, so this ends up holding the complete text.
    longest: String,
    kind: String,
    sub: Option<String>,
}

/// Turns harness events into the NDJSON contract documented at the top.
pub struct Emitter<W: Write> {
    out: W,
    cur: Option<TextBlock>,
    calls: HashSet<String>,
    results: HashSet<String>,
    /// Lines written so far. The driver watches this to know a run has stopped
    /// publishing before it writes the terminal `result` line.
    lines: u64,
    /// Set by the bus when a run announces a terminal state (`done`/`failed`/
    /// `stopped`/`waiting`). The driver polls this rather than guessing.
    attention: Arc<Mutex<Option<String>>>,
}

impl<W: Write> Emitter<W> {
    fn new(out: W) -> Self {
        Emitter {
            out,
            cur: None,
            calls: Default::default(),
            results: Default::default(),
            lines: 0,
            attention: Arc::new(Mutex::new(None)),
        }
    }

    /// A handle on the terminal-state flag, for the driver's wait loop.
    fn attention(&self) -> Arc<Mutex<Option<String>>> {
        self.attention.clone()
    }

    /// How many NDJSON lines have been written.
    pub fn line_count(&self) -> u64 {
        self.lines
    }

    fn line(&mut self, v: Value) {
        // A broken pipe (`| head`) is not worth crashing a run over.
        if writeln!(self.out, "{v}").is_ok() {
            self.lines += 1;
        }
        let _ = self.out.flush();
    }

    pub fn session(&mut self, task_id: &str, model: &str, cwd: &str, perm: &str, resumed: bool) {
        self.line(json!({
            "type": "session",
            "session_id": task_id,
            "model": model,
            "cwd": cwd,
            "perm": perm,
            "resumed": resumed,
        }));
    }

    pub fn result(
        &mut self,
        task_id: &str,
        ok: bool,
        exit_code: i32,
        status: &str,
        model: &str,
        usage: &Usage,
    ) {
        self.flush_text();
        self.line(json!({
            "type": "result",
            "session_id": task_id,
            "ok": ok,
            "exit_code": exit_code,
            "status": status,
            "model": model,
            "usage": {
                "input": usage.input,
                "output": usage.output,
                "cache_read": usage.cache_read,
                "cache_write": usage.cache_write,
                "last_context": usage.last_context,
            },
            "cost": usage.cost,
        }));
    }

    /// The bus entry point: exactly the `(event name, payload)` pair the harness
    /// publishes. `ol://event` wraps a per-kind body as
    /// `{task_id, kind, payload}`; only the kinds that carry a user-visible step
    /// are translated, and everything else (`bg`, `artifict`, `drop`,
    /// `ol://usage`, …) is ignored so the stream stays a clean transcript.
    pub fn on_event(&mut self, name: &str, payload: Value) {
        match name {
            "ol://event" => {
                let kind = payload["kind"].as_str().unwrap_or("").to_string();
                let body = &payload["payload"];
                match kind.as_str() {
                    // `item` is the `Item` itself; `sitem` wraps it with the
                    // subagent it belongs to.
                    "item" => self.on_item(body, None),
                    "sitem" => {
                        let sub = body["sub_id"].as_str().map(String::from);
                        self.on_item(&body["item"], sub);
                    }
                    "delta" => self.on_delta(body),
                    _ => {}
                }
            }
            "ol://attention" => {
                let kind = payload["kind"].as_str().unwrap_or("");
                if matches!(kind, "done" | "failed" | "stopped" | "waiting") {
                    if let Ok(mut a) = self.attention.lock() {
                        *a = Some(kind.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn on_delta(&mut self, payload: &Value) {
        let id = payload["item_id"].as_str().unwrap_or("").to_string();
        let text = payload["text"].as_str().unwrap_or("").to_string();
        if id.is_empty() || text.is_empty() {
            return;
        }
        let sub = payload["sub_id"].as_str().map(String::from);
        match &mut self.cur {
            Some(c) if c.id == id => c.deltas.push_str(&text),
            _ => {
                self.flush_text();
                self.cur = Some(TextBlock {
                    id,
                    base: String::new(),
                    deltas: text,
                    longest: String::new(),
                    kind: "text".into(),
                    sub,
                });
            }
        }
    }

    fn on_item(&mut self, item: &Value, sub: Option<String>) {
        let kind = item["kind"].as_str().unwrap_or("");
        let id = item["id"].as_str().unwrap_or("").to_string();
        match kind {
            "text" | "thinking" => {
                let text = item["text"].as_str().unwrap_or("").to_string();
                match &mut self.cur {
                    Some(c) if c.id == id => {
                        if text.len() > c.longest.len() {
                            c.longest = text;
                        }
                    }
                    _ => {
                        self.flush_text();
                        self.cur = Some(TextBlock {
                            id,
                            base: text.clone(),
                            deltas: String::new(),
                            longest: text,
                            kind: kind.to_string(),
                            sub,
                        });
                    }
                }
            }
            "tool" => {
                self.flush_text();
                self.on_tool(item, id, sub);
            }
            "notice" => {
                self.flush_text();
                let text = item["text"].as_str().unwrap_or("").to_string();
                if !text.is_empty() {
                    self.line(json!({"type": "notice", "text": text, "sub": sub}));
                }
            }
            _ => {}
        }
    }

    fn on_tool(&mut self, item: &Value, id: String, sub: Option<String>) {
        let data = &item["data"];
        let name = data["name"].as_str().unwrap_or("").to_string();
        let status = data["status"].as_str().unwrap_or("").to_string();
        let input = data["input"].clone();
        if id.is_empty() {
            return;
        }
        let call = |s: &mut Self| {
            s.calls.insert(id.clone());
            s.line(json!({
                "type": "tool_call", "name": name, "input": input,
                "id": id, "sub": sub,
            }));
        };
        if status == "running" {
            // The first `item` for a tool has no `input` yet; it is patched in
            // once the turn's arguments are known. Wait for that before
            // announcing the call, so the event carries the real arguments.
            if !input.is_null() && !self.calls.contains(&id) {
                call(self);
            }
            return;
        }
        if !self.calls.contains(&id) {
            call(self);
        }
        if !self.results.contains(&id) {
            self.results.insert(id.clone());
            self.line(json!({
                "type": "tool_result", "name": name, "status": status,
                "output": data["output"], "id": id, "sub": sub,
            }));
        }
    }

    fn flush_text(&mut self) {
        let Some(c) = self.cur.take() else { return };
        let combined = format!("{}{}", c.base, c.deltas);
        let full = if combined.len() >= c.longest.len() {
            combined
        } else {
            c.longest
        };
        if full.is_empty() {
            return;
        }
        let ty = if c.kind == "thinking" {
            "thinking"
        } else {
            "assistant"
        };
        self.line(json!({"type": ty, "text": full, "sub": c.sub}));
    }
}

// ────────────────────────────── harness boot ──────────────────────────────

type EventSink = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// Build the same `Harness` the desktop app boots, but with an NDJSON sink on
/// the bus instead of a webview — and hand back that *same* sink, so the driver
/// writes its `session`/`result` bookends through the one emitter that also sees
/// the harness's terminal-state announcement.
///
/// This is the only place that knows about process-global state (routes, model
/// catalog, trust), and it mirrors `lib.rs`'s `setup`.
pub fn boot(
    settings: store::Settings,
    tasks: Vec<Task>,
    out: Box<dyn Write + Send>,
) -> (Arc<Harness>, Sink) {
    providers::set_custom_models(&settings.model_configs);
    providers::set_model_visibility(&settings.disabled_models, &settings.removed_models);
    router::set_routes(&settings.routes);
    trust::set_trust(&settings.trust);

    let emitter = sink(out);
    let handle = emitter.clone();
    let bus: EventSink = Arc::new(move |name: &str, payload: Value| {
        if let Ok(mut e) = handle.lock() {
            e.on_event(name, payload);
        }
    });

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
    // The debounced writer needs an `Arc` to call back through.
    Harness::register(&h);
    (h, emitter)
}

// ────────────────────────────── one headless run ──────────────────────────────

pub struct RunOpts {
    pub prompt: String,
    pub cwd: String,
    pub model: Option<String>,
    pub perm: Option<String>,
    pub plan: bool,
    pub resume: Option<String>,
    pub timeout: Duration,
}

enum Terminal {
    /// The run announced a terminal state (`done`/`failed`/`stopped`/`waiting`).
    Announced(String),
    /// The task settled on a terminal status without an announcement.
    Status(String),
    /// The run is blocked on an approval/question nobody can answer.
    NeedsInput(String),
    Timeout,
}

/// Create a brand-new task record. Built through `serde_json` rather than a
/// field-by-field literal, so a field added to `Task` does not silently rot
/// this copy.
fn new_task(
    id: &str,
    prompt: &str,
    project: &str,
    model: &str,
    perm: &str,
    plan: bool,
    agents: &[String],
) -> Result<Task, String> {
    let now = chrono::Utc::now();
    let title: String = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("headless run")
        .trim()
        .chars()
        .take(80)
        .collect();
    let mut agents = agents.to_vec();
    store::ensure_required_agents(&mut agents);
    let mut t: Task = serde_json::from_value(json!({
        "id": id,
        "title": if title.is_empty() { "headless run".to_string() } else { title },
        "status": "idle",
        "step": "",
        "project": project,
        "cwd": project,
        "branch": "",
        "worktree": false,
        "model": model,
        "effort": 2,
        "perm": perm,
        "plan": plan,
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

/// Run `opts` to completion. Returns the process exit code (see the module
/// docs) and writes the `session`/`result` bookends to `sink`.
pub async fn run_prompt(h: &Arc<Harness>, sink: &Sink, opts: &RunOpts) -> i32 {
    let (default_model, default_agents) = {
        let s = h.settings.read().await;
        (s.model.clone(), s.default_agents.clone())
    };
    let perm_default = opts.perm.clone().unwrap_or_else(|| "allowlist".to_string());

    let (task_id, resumed) = if let Some(id) = &opts.resume {
        if h.task_ref(id).await.is_err() {
            eprintln!("openleash-headless: no task with id `{id}`; nothing was started");
            return EXIT_USAGE;
        }
        // `--model`/`--perm` are optional on resume: the task keeps its own
        // unless the caller overrides them explicitly.
        if opts.model.is_some() || opts.perm.is_some() || opts.plan {
            let (m, p, plan) = (opts.model.clone(), opts.perm.clone(), opts.plan);
            h.update_task(id, |t| {
                if let Some(m) = m {
                    t.model = m;
                }
                if let Some(p) = p {
                    t.perm = p;
                }
                if plan {
                    t.plan = true;
                }
            })
            .await;
        }
        (id.clone(), true)
    } else {
        let model = opts
            .model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .or_else(|| (!default_model.trim().is_empty()).then(|| default_model.clone()));
        let Some(model) = model else {
            eprintln!("openleash-headless: no model. Pass --model <id> (or set one in the app).");
            return EXIT_USAGE;
        };
        let id = new_id();
        let task = match new_task(
            &id,
            &opts.prompt,
            &opts.cwd,
            &model,
            &perm_default,
            opts.plan,
            &default_agents,
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("openleash-headless: {e}");
                return EXIT_USAGE;
            }
        };
        h.tasks
            .write()
            .await
            .insert(id.clone(), Arc::new(tokio::sync::Mutex::new(task)));
        h.save_task_now(&id).await;
        (id, false)
    };

    let (model, perm, cwd) = match h.task(&task_id).await {
        Ok(t) => {
            let t = t.lock().await;
            (t.model.clone(), t.perm.clone(), t.cwd.clone())
        }
        Err(e) => {
            eprintln!("openleash-headless: {e}");
            return EXIT_FAILURE;
        }
    };

    let attention = {
        let mut g = sink.lock().unwrap();
        g.session(&task_id, &model, &cwd, &perm, resumed);
        g.attention()
    };
    eprintln!(
        "openleash-headless: session {task_id} · model {model} · perm {perm}{}",
        if resumed { " · resumed" } else { "" }
    );

    let started = Instant::now();
    if let Err(e) = runner::send(h.clone(), task_id.clone(), opts.prompt.clone()).await {
        eprintln!("openleash-headless: {e}");
        let code = EXIT_FAILURE;
        emit_result(h, sink, &task_id, code).await;
        return code;
    }

    let terminal = wait_terminal(h, &task_id, opts.timeout, &attention).await;
    let elapsed = started.elapsed();

    let (status, paused, usage) = match h.task(&task_id).await {
        Ok(t) => {
            let t = t.lock().await;
            (t.status.clone(), t.paused.clone(), t.usage.clone())
        }
        Err(e) => {
            eprintln!("openleash-headless: {e}");
            (String::from("unknown"), None, Usage::default())
        }
    };

    let code = match terminal {
        Terminal::NeedsInput(kind) => {
            eprintln!(
                "openleash-headless: the run is waiting on a {kind} that non-interactive mode \
                 cannot answer. Re-run with a permission mode that does not ask (e.g. --perm auto)."
            );
            runner::interrupt(h.as_ref(), &task_id).await;
            EXIT_NEEDS_INPUT
        }
        Terminal::Timeout => {
            eprintln!(
                "openleash-headless: gave up after {:.0}s (--timeout-secs); the run was interrupted.",
                elapsed.as_secs_f64()
            );
            runner::interrupt(h.as_ref(), &task_id).await;
            EXIT_LIMIT
        }
        Terminal::Announced(k) | Terminal::Status(k) => {
            if paused.is_some() {
                EXIT_LIMIT
            } else {
                match k.as_str() {
                    "done" => EXIT_OK,
                    "waiting" => EXIT_LIMIT,
                    _ => EXIT_FAILURE,
                }
            }
        }
    };

    // Everything the run still has to say (the notice it leaves behind, a denied
    // tool's outcome) lands before the terminal line, so `result` really is last.
    settle(h, &task_id, sink).await;

    eprintln!(
        "openleash-headless: {status} · {:.1}s · in {} out {} · ${:.4}{}",
        elapsed.as_secs_f64(),
        usage.input,
        usage.output,
        usage.cost,
        if paused.is_some() {
            " · paused on a provider limit"
        } else {
            ""
        }
    );
    emit_result_with(h, sink, &task_id, code, &status, &model, &usage).await;
    // The run's own save is debounced; the process is about to exit, so make
    // sure the final state (and the usage the result event reported) is on disk
    // before `--resume` can be asked for it.
    h.save_task_now(&task_id).await;
    code
}

async fn emit_result(h: &Arc<Harness>, sink: &Sink, task_id: &str, code: i32) {
    let (status, model, usage) = match h.task(task_id).await {
        Ok(t) => {
            let t = t.lock().await;
            (t.status.clone(), t.model.clone(), t.usage.clone())
        }
        Err(_) => (String::from("unknown"), String::new(), Usage::default()),
    };
    emit_result_with(h, sink, task_id, code, &status, &model, &usage).await;
}

async fn emit_result_with(
    _h: &Arc<Harness>,
    sink: &Sink,
    task_id: &str,
    code: i32,
    status: &str,
    model: &str,
    usage: &Usage,
) {
    let mut g = sink.lock().unwrap();
    g.result(task_id, code == EXIT_OK, code, status, model, usage);
}

/// Wait until a run has stopped publishing before the terminal `result` line is
/// written.
///
/// A run's own unwind happens *after* it stops working: the notice it leaves for
/// a refusal, and the item patched with a denied tool's outcome, are both
/// published once `running` is already false. Without this window the driver
/// would print `result` and then another event would arrive behind it, and the
/// "the last line is always the result" contract would be a lie. The quiet
/// period is what makes it true; the deadline stops a wedged run from blocking
/// the exit forever.
async fn settle(h: &Arc<Harness>, task_id: &str, sink: &Sink) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let count = |s: &Sink| s.lock().map(|e| e.line_count()).unwrap_or(0);
    let mut last = count(sink);
    let mut quiet = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = count(sink);
        if now != last {
            last = now;
            quiet = Instant::now();
        }
        if !h.runtime(task_id).running.load(Ordering::SeqCst)
            && quiet.elapsed() >= Duration::from_millis(250)
        {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
    }
}

/// Wait for the run to end. Three things end it, in order of preference: the
/// terminal `ol://attention` announcement (which the run emits last), a settled
/// task status, or a prompt nobody can answer — which is reported rather than
/// waited on, because it never resolves on its own.
async fn wait_terminal(
    h: &Arc<Harness>,
    task_id: &str,
    timeout: Duration,
    attention: &Arc<Mutex<Option<String>>>,
) -> Terminal {
    let started = Instant::now();
    let mut blocked: Option<(String, Instant)> = None;
    loop {
        if let Ok(a) = attention.lock() {
            if let Some(k) = a.clone() {
                return Terminal::Announced(k);
            }
        }
        let running = h.runtime(task_id).running.load(Ordering::SeqCst);
        let (status, waiting_kind) = match h.task(task_id).await {
            Ok(t) => {
                let t = t.lock().await;
                (t.status.clone(), t.waiting_kind.clone())
            }
            Err(_) => (String::from("idle"), None),
        };
        match waiting_kind.as_deref() {
            // A park ("wake") is a legitimate end for a headless run — the chat
            // is waiting on a trigger, not on a person. An approval or question
            // is not: nothing here can answer it, so it never clears.
            Some("wake") => return Terminal::Announced("waiting".into()),
            Some(k) => {
                // Give the tool a moment to be answered internally before
                // declaring the run blocked (subagent questions are answered
                // by the main agent and never reach this state).
                let since = blocked.get_or_insert_with(|| (k.to_string(), Instant::now()));
                since.0 = k.to_string();
                if since.1.elapsed() >= Duration::from_secs(3) {
                    return Terminal::NeedsInput(k.to_string());
                }
            }
            None => blocked = None,
        }
        if !running && status != "running" && status != "idle" {
            return Terminal::Status(status);
        }
        if started.elapsed() >= timeout {
            return Terminal::Timeout;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ────────────────────────────── CLI ──────────────────────────────

const HELP: &str = "\
openleash-headless — run one OpenLeash prompt to completion, without a GUI.

USAGE:
    openleash-headless [OPTIONS] [PROMPT]

OPTIONS:
    -p, --prompt <TEXT>     the prompt to run (also accepted positionally)
        --cwd <DIR>         working directory (default: current directory)
        --model <ID>        model id (default: the app's saved default model)
        --perm <MODE>       disabled | allowlist | reviewer | auto | turbo (default: allowlist)
        --plan              start in plan mode (writes nothing)
        --resume <TASK-ID>  continue an existing task instead of starting one
        --timeout-secs <N>  give up after N seconds (default 900)
        --json              accepted for compatibility; NDJSON is the default
    -h, --help              print this help

stdout is NDJSON: a `session` line, then per-step lines, then a `result` line.
Exit codes: 0 ok · 1 failed · 2 usage · 3 gave up / limit · 4 needs input.";

struct Args {
    positional: Vec<String>,
    flags: HashMap<String, String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut out = Args {
        positional: vec![],
        flags: HashMap::new(),
    };
    let takes_value = [
        "prompt",
        "p",
        "cwd",
        "model",
        "perm",
        "resume",
        "timeout-secs",
    ];
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(rest) = a.strip_prefix("--") {
            let (key, inline) = match rest.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if !takes_value.contains(&key.as_str()) {
                out.flags
                    .insert(key, inline.unwrap_or_else(|| "true".into()));
                i += 1;
                continue;
            }
            let value = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    argv.get(i)
                        .cloned()
                        .ok_or_else(|| format!("--{key} needs a value"))?
                }
            };
            out.flags.insert(key, value);
        } else if a == "-h" {
            out.flags.insert("help".into(), "true".into());
        } else if let Some(rest) = a.strip_prefix("-p") {
            if rest.is_empty() {
                i += 1;
                let v = argv
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "-p needs a value".to_string())?;
                out.flags.insert("prompt".into(), v);
            } else {
                out.flags.insert("prompt".into(), rest.to_string());
            }
        } else if a.starts_with('-') {
            return Err(format!("unknown option `{a}`"));
        } else {
            out.positional.push(a.clone());
        }
        i += 1;
    }
    Ok(out)
}

fn options_from(args: Args) -> Result<RunOpts, String> {
    let prompt = args
        .flags
        .get("prompt")
        .cloned()
        .or_else(|| args.positional.first().cloned())
        .filter(|p| !p.trim().is_empty())
        .ok_or("a prompt is required (positionally or with --prompt)")?;
    let cwd = match args.flags.get("cwd") {
        Some(d) => d.clone(),
        None => std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?
            .to_string_lossy()
            .to_string(),
    };
    let perm = args.flags.get("perm").cloned();
    if let Some(p) = &perm {
        if !matches!(
            p.as_str(),
            "disabled" | "allowlist" | "auto" | "turbo" | "reviewer"
        ) {
            return Err(format!(
                "unknown --perm `{p}` (disabled | allowlist | reviewer | auto | turbo)"
            ));
        }
    }
    let timeout = match args.flags.get("timeout-secs") {
        Some(v) => Duration::from_secs(
            v.parse::<u64>()
                .map_err(|_| format!("--timeout-secs needs a number, got `{v}`"))?,
        ),
        None => Duration::from_secs(900),
    };
    Ok(RunOpts {
        prompt,
        cwd,
        model: args
            .flags
            .get("model")
            .cloned()
            .filter(|m| !m.trim().is_empty()),
        perm,
        plan: args.flags.contains_key("plan"),
        resume: args
            .flags
            .get("resume")
            .cloned()
            .filter(|r| !r.trim().is_empty()),
        timeout,
    })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("openleash-headless: {e}\n\n{HELP}");
            std::process::exit(EXIT_USAGE);
        }
    };
    if args.flags.contains_key("help") {
        println!("{HELP}");
        std::process::exit(EXIT_OK);
    }
    let opts = match options_from(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("openleash-headless: {e}\n\n{HELP}");
            std::process::exit(EXIT_USAGE);
        }
    };

    let settings = store::load_settings();
    let tasks = store::load_tasks();
    let (h, sink) = boot(settings, tasks, Box::new(std::io::stdout()));

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("openleash-headless: cannot start the async runtime: {e}");
            std::process::exit(EXIT_FAILURE);
        }
    };
    let code = runtime.block_on(run_prompt(&h, &sink, &opts));
    std::process::exit(code);
}

#[cfg(test)]
mod tests;
