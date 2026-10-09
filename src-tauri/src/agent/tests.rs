//! Integration tests for routing, retries, account rotation, pausing and the
//! subscription backends, against a scripted local HTTP server.

use super::providers::{ChatRequest, StreamEvent};
use super::router::{self, RouteErr, Who};
use super::store::{Account, CustomProvider, McpServerCfg, ProviderCfg, Route, Settings};
use super::*;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// The custom-model registry is process-global too: tests that call
/// `set_custom_models` must not overlap, or one clobbers the other's catalog.
static MODELS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ───────────────────────────── mock server ─────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct Req {
    #[allow(dead_code)]
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: String,
    pub(crate) body: Value,
}

#[derive(Clone)]
pub(crate) struct Resp {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
    sse: bool,
}

pub(crate) fn sse(events: &[Value]) -> Resp {
    Resp {
        status: 200,
        body: events
            .iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n",
        headers: vec![],
        sse: true,
    }
}
pub(crate) fn err(status: u16, body: Value) -> Resp {
    Resp {
        status,
        body: body.to_string(),
        headers: vec![],
        sse: false,
    }
}
pub(crate) fn openai_ok(text: &str) -> Resp {
    sse(&[
        json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":80}}}),
    ])
}

type Tagged = Arc<std::sync::Mutex<Vec<(String, VecDeque<Resp>)>>>;

pub(crate) struct Mock {
    pub(crate) base: String,
    log: Arc<std::sync::Mutex<Vec<Req>>>,
    script: Arc<std::sync::Mutex<VecDeque<Resp>>>,
    /// Responses reserved for requests whose system prompt contains a tag (concurrent agents).
    tagged: Tagged,
}

impl Mock {
    pub(crate) async fn start() -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log: Arc<std::sync::Mutex<Vec<Req>>> = Default::default();
        let script: Arc<std::sync::Mutex<VecDeque<Resp>>> = Default::default();
        let tagged: Tagged = Default::default();
        let (l2, s2, g2) = (log.clone(), script.clone(), tagged.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (log, script, tagged) = (l2.clone(), s2.clone(), g2.clone());
                tokio::spawn(async move {
                    let mut buf = vec![];
                    let mut tmp = [0u8; 8192];
                    let head_end = loop {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break p + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: Value =
                        serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                    let mut first = head.lines().next().unwrap_or("").split_whitespace();
                    let method = first.next().unwrap_or("").to_string();
                    let path = first.next().unwrap_or("").to_string();
                    let resp = if path.contains("/wham/usage") || path.contains("/oauth/usage") {
                        // Usage endpoints: always "fine", never consumes the script.
                        Resp { status: 200, body: json!({"rate_limit": {"primary_window": {"used_percent": 12.0, "limit_window_seconds": 18000, "reset_after_seconds": 3600}}, "five_hour": {"utilization": 12.0}}).to_string(), headers: vec![], sse: false }
                    } else if path.contains("/zen/go/v1/usage") {
                        // OpenCode Go's own shape: percent is 0-100 already, and the
                        // reset is an ISO timestamp rather than a countdown. Logged,
                        // unlike the two above, because the Go test asserts on the
                        // path and the bearer that carried the key.
                        log.lock().unwrap().push(Req {
                            method: method.clone(),
                            path: path.clone(),
                            headers: head.to_lowercase(),
                            body: body.clone(),
                        });
                        Resp { status: 200, body: json!({"usage": {
                            "rolling": {"status": "ok", "percent": 25, "resetsAt": "2030-01-01T05:00:00.000Z"},
                            "weekly": {"status": "ok", "percent": 40, "resetsAt": "2030-01-06T00:00:00.000Z"},
                            "monthly": {"status": "ok", "percent": 10, "resetsAt": "2030-02-01T00:00:00.000Z"}
                        }}).to_string(), headers: vec![], sse: false }
                    } else if path.contains("/zen/go/v1/models") {
                        log.lock().unwrap().push(Req {
                            method: method.clone(),
                            path: path.clone(),
                            headers: head.to_lowercase(),
                            body: body.clone(),
                        });
                        Resp {
                            status: 200,
                            body: json!({"object": "list", "data": [
                                {"id": "kimi-k3", "object": "model", "owned_by": "opencode"},
                                {"id": "qwen3.7-plus", "object": "model", "owned_by": "opencode"},
                                {"id": "grok-4.7", "object": "model", "owned_by": "opencode"}
                            ]})
                            .to_string(),
                            headers: vec![],
                            sse: false,
                        }
                    } else {
                        let sys = body["messages"][0]["content"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        log.lock().unwrap().push(Req {
                            method,
                            path,
                            headers: head.to_lowercase(),
                            body,
                        });
                        let own = tagged
                            .lock()
                            .unwrap()
                            .iter_mut()
                            .find(|(t, q)| sys.contains(t.as_str()) && !q.is_empty())
                            .and_then(|(_, q)| q.pop_front());
                        own.or_else(|| script.lock().unwrap().pop_front())
                            .unwrap_or(err(599, json!({"error": {"message": "script exhausted"}})))
                    };
                    let mut out = format!(
                        "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                        resp.status,
                        if resp.sse { "text/event-stream" } else { "application/json" },
                        resp.body.len()
                    );
                    for (k, v) in &resp.headers {
                        out.push_str(&format!("{k}: {v}\r\n"));
                    }
                    out.push_str("\r\n");
                    out.push_str(&resp.body);
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Mock {
            base,
            log,
            script,
            tagged,
        }
    }
    fn push_for(&self, tag: &str, r: Resp) {
        let mut g = self.tagged.lock().unwrap();
        match g.iter_mut().find(|(t, _)| t == tag) {
            Some((_, q)) => q.push_back(r),
            None => g.push((tag.to_string(), VecDeque::from([r]))),
        }
    }
    pub(crate) fn push(&self, r: Resp) {
        self.script.lock().unwrap().push_back(r);
    }
    /// Every test shares one mock and one response queue, so a scenario that
    /// deliberately answers nothing must not leave a script behind for the next
    /// one to inherit: leftovers are reported rather than silently consumed.
    fn assert_queue_empty(&self) {
        let left = self.script.lock().unwrap();
        assert!(
            left.is_empty(),
            "{} scripted response(s) left unconsumed; the next scenario would inherit them",
            left.len()
        );
    }
    pub(crate) fn reqs(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

/// A minimal streamable-HTTP MCP server for integration tests. It serves the
/// handshake and a fixed catalog, so tests exercise `McpManager::sync` and
/// `tool_schemas` rather than injecting schemas directly into a Task.
struct MockMcpServer {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl MockMcpServer {
    async fn start(tool_count: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let tools: Vec<Value> = (0..tool_count)
            .map(|i| {
                json!({
                    "name": format!("lookup_{i}"),
                    "description": format!("look up documentation reference {i}"),
                    "inputSchema": {"type":"object","properties":{}}
                })
            })
            .collect();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let tools = tools.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let head_end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                    let content_len = head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + content_len {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let request: Value =
                        serde_json::from_slice(&buf[head_end..head_end + content_len])
                            .unwrap_or(Value::Null);
                    let (status, body) = if request.get("id").is_none() {
                        // MCP notifications do not have a JSON-RPC response.
                        (202, String::new())
                    } else {
                        let result = match request["method"].as_str().unwrap_or("") {
                            "initialize" => json!({
                                "protocolVersion":"2025-06-18",
                                "capabilities":{"tools":{}},
                                "serverInfo":{"name":"mock-docs","version":"1.0"}
                            }),
                            "tools/list" => json!({"tools":tools}),
                            method => panic!("unexpected MCP method in fixture: {method}"),
                        };
                        (
                            200,
                            json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self { base, task }
    }
}

impl Drop for MockMcpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ───────────────────────────── harness fixture ─────────────────────────────

/// A minimal in-memory task. `pub(crate)` so other `reviewer_*` modules can use
/// it rather than repeating this literal — `Task` has ~50 fields and a
/// hand-rolled copy silently rots the moment one is added.
pub(crate) fn task(id: &str, model: &str) -> Task {
    let now = chrono::Utc::now();
    Task {
        id: id.into(),
        title: "t".into(),
        titled: false,
        status: "running".into(),
        waiting_kind: None,
        step: String::new(),
        project: ".".into(),
        cwd: ".".into(),
        branch: String::new(),
        base_branch: String::new(),
        base_commit: None,
        worktree: false,
        model: model.into(),
        effort: 2,
        perm: "auto".into(),
        plan: false,
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
        system: String::new(),
        mcp_tools: vec![],
        plugins: Default::default(),
        told: Default::default(),
        sub_items: Default::default(),
        sub_msgs: Default::default(),
        route: String::new(),
        unpaused: false,
        stop_note: None,
        bg_live: vec![],
        busy: 0,
        wrap_up: false,
        model_map: Default::default(),
        archived: false,
        pinned: false,
        order: 0.0,
        items: vec![],
        messages: vec![],
        todos: vec![],
        subs: vec![],
        usage: Default::default(),
        read_files: Default::default(),
        checkpoints: vec![],
        touched: Default::default(),
        hidden: false,
        forked_from: None,
        created_at: now,
        updated_at: now,
        touched_at: now,
        // Fixtures build in-memory tasks, so they hold their sub-agent maps
        // rather than leaving them on disk.
        hydrated: true,
    }
}

pub(crate) type Events = Arc<std::sync::Mutex<Vec<(String, Value)>>>;

pub(crate) fn harness(settings: Settings, tasks: Vec<Task>) -> (Arc<Harness>, Events) {
    let events: Events = Default::default();
    let ev2 = events.clone();
    let h = Harness {
        bus: Arc::new(move |name: &str, v: Value| ev2.lock().unwrap().push((name.to_string(), v))),
        tasks: tokio::sync::RwLock::new(
            tasks
                .into_iter()
                .map(|t| (t.id.clone(), Arc::new(tokio::sync::Mutex::new(t))))
                .collect(),
        ),
        runtimes: Default::default(),
        settings: tokio::sync::RwLock::new(settings),
        bg: Default::default(),
        mcp: Default::default(),
        http: reqwest::Client::new(),
        pause_bell: Default::default(),
        accts: Default::default(),
        stats: Default::default(),
        keys: Default::default(),
        // No writer behind a hand-built harness: `save_task` is a no-op here and
        // the tests below that need a file on disk use `save_task_now`.
        dirty: Default::default(),
        saver: Default::default(),
        settings_dirty: Default::default(),
        me: Default::default(),
    };
    (Arc::new(h), events)
}

pub(crate) fn custom(
    id: &str,
    base: &str,
    insist: bool,
) -> (CustomProvider, (String, ProviderCfg)) {
    (
        CustomProvider {
            id: id.into(),
            name: id.into(),
            base_url: base.into(),
            kind: "openai".into(),
            insist,
        },
        (
            id.into(),
            ProviderCfg {
                api_key: "k-".to_string() + id,
                base_url: String::new(),
                enabled: true,
                api_keys: vec![],
                key_pool: false,
            },
        ),
    )
}

fn acct(id: &str, kind: &str, prio: i32) -> Account {
    Account {
        id: id.into(),
        kind: kind.into(),
        label: id.into(),
        access_token: format!("tok-{id}"),
        account_id: format!("acc-{id}"),
        priority: prio,
        enabled: true,
        ..Default::default()
    }
}

fn req() -> ChatRequest {
    ChatRequest {
        system: "SYS".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        effort: 2,
        max_tokens: 1000,
        cache_key: "ol-t1".into(),
    }
}

async fn run(
    h: &Arc<Harness>,
    model: &str,
) -> Result<(providers::Turn, providers::Target), RouteErr> {
    let mut sink = |_e: StreamEvent| {};
    router::request(
        h,
        Who {
            task_id: "t1",
            sub: None,
            side: false,
            turn: true,
            usage_id: "main",
        },
        model,
        &req(),
        &mut sink,
        &CancellationToken::new(),
    )
    .await
}

fn text(t: &providers::Turn) -> String {
    t.content
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect()
}

/// Every test shares one mock + env (the endpoint overrides are process-wide),
/// so scenarios run in sequence inside a single test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routing_end_to_end() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-test-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    std::env::set_var("OPENLEASH_CODEX_BASE", &m.base);
    std::env::set_var("OPENLEASH_ANTHROPIC_BASE", &m.base);

    transient_errors_are_retried(&m).await;
    a_retry_line_does_not_outlive_its_failure(&m).await;
    bad_requests_give_up_fast_unless_insist(&m).await;
    codex_pool_rotates_past_exhausted_account(&m).await;
    key_pool_rotates_keys(&m).await;
    route_falls_back_then_pauses_and_resumes(&m).await;
    swap_while_paused_retries_the_new_model(&m).await;
    swap_while_paused_resumes_on_the_new_models_chain(&m).await;
    claude_subscription_sends_prelude_and_oauth(&m).await;
    too_long_is_reported_for_compaction(&m).await;
    pause_interrupts_a_stream_and_retries(&m).await;
    message_into_a_paused_chat_never_answers_the_frozen_turn(&m).await;
    setup_tokens_read_usage_from_headers(&m).await;

    let _ = std::fs::remove_dir_all(dir);
}

async fn transient_errors_are_retried(m: &Mock) {
    let (cp, key) = custom("flaky", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let (h, _) = harness(s, vec![task("t1", "flaky/m")]);
    let before = m.reqs().len();
    m.push(err(500, json!({"error": {"message": "boom"}})));
    m.push(err(502, json!({"error": {"message": "bad gateway"}})));
    m.push(openai_ok("hello"));
    let (turn, t) = run(&h, "flaky/m")
        .await
        .expect("should succeed after retries");
    assert_eq!(text(&turn), "hello");
    assert_eq!(t.model_id, "flaky/m");
    assert_eq!(turn.usage.cache_read, 80);
    let reqs = &m.reqs()[before..];
    assert_eq!(reqs.len(), 3, "two failures + one success");
    assert!(reqs[0].headers.contains("authorization: bearer k-flaky"));
    assert_eq!(reqs[0].body["model"], "m");
}

/// A blip that gets retried must not keep reading as a retry after it works:
/// the step line is what the chat's spinner shows, so a stale one looks like
/// the agent hung on a failure that is long over.
async fn a_retry_line_does_not_outlive_its_failure(m: &Mock) {
    let (cp, key) = custom("blip", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let (h, _) = harness(s, vec![task("t1", "blip/m")]);
    m.push(err(500, json!({"error": {"message": "boom"}})));
    m.push(openai_ok("recovered"));
    let (turn, _) = run(&h, "blip/m").await.expect("recovers on retry");
    assert_eq!(text(&turn), "recovered");
    assert_eq!(
        step_of(&h, "t1").await,
        "",
        "the retry line is gone once the attempt lands"
    );
}

async fn step_of(h: &Arc<Harness>, id: &str) -> String {
    h.task(id).await.unwrap().lock().await.step.clone()
}

async fn bad_requests_give_up_fast_unless_insist(m: &Mock) {
    // Normal provider: an odd 404 gets 3 tries, then the route (set to fail) gives up.
    let (cp, key) = custom("odd", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.routes.push(Route {
        id: "r".into(),
        name: "r".into(),
        steps: vec!["odd/m".into()],
        on_exhausted: "fail".into(),
        ..Default::default()
    });
    router::set_routes(&s.routes);
    let (h, _) = harness(s, vec![task("t1", "route/r")]);
    let before = m.reqs().len();
    for _ in 0..3 {
        m.push(err(
            404,
            json!({"error": {"message": "model not found (lol)"}}),
        ));
    }
    assert!(matches!(run(&h, "route/r").await, Err(RouteErr::Fatal(_))));
    assert_eq!(m.reqs().len() - before, 3);

    // Insist provider: pushes through 8 weird errors.
    let (cp, key) = custom("stubborn", &m.base, true);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let (h, _) = harness(s, vec![task("t1", "stubborn/m")]);
    for i in 0..8 {
        m.push(err(
            if i % 2 == 0 { 404 } else { 500 },
            json!({"error": {"message": "no channel available"}}),
        ));
    }
    m.push(openai_ok("finally"));
    let (turn, _) = run(&h, "stubborn/m").await.expect("insist keeps going");
    assert_eq!(text(&turn), "finally");
}

fn codex_ok(text: &str, call: bool) -> Resp {
    let mut ev = vec![
        json!({"type":"response.output_item.added","item":{"type":"message","id":"m1"}}),
        json!({"type":"response.output_text.delta","delta":text}),
    ];
    if call {
        ev.push(json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc1","call_id":"call_9","name":"read_file"}}));
        ev.push(json!({"type":"response.function_call_arguments.delta","item_id":"fc1","delta":"{\"path\":"}));
        ev.push(json!({"type":"response.function_call_arguments.delta","item_id":"fc1","delta":"\"a.txt\"}"}));
        ev.push(json!({"type":"response.output_item.done","item":{"type":"function_call","id":"fc1","call_id":"call_9","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}));
    }
    ev.push(json!({"type":"response.completed","response":{"usage":{"input_tokens":1000,"input_tokens_details":{"cached_tokens":900},"output_tokens":20}}}));
    let mut r = sse(&ev);
    r.headers = vec![
        ("x-codex-primary-used-percent".into(), "42".into()),
        ("x-codex-primary-window-minutes".into(), "300".into()),
        ("x-codex-primary-reset-after-seconds".into(), "600".into()),
    ];
    r
}

async fn codex_pool_rotates_past_exhausted_account(m: &Mock) {
    let s = Settings {
        accounts: vec![acct("low", "codex", 10), acct("high", "codex", 20)],
        ..Default::default()
    };
    let (h, _) = harness(s, vec![task("t1", "codex/gpt-6-sol")]);
    let before = m.reqs().len();
    m.push(err(429, json!({"error": {"type": "usage_limit_reached", "message": "The usage limit has been reached", "resets_in_seconds": 7200}})));
    m.push(codex_ok("from low", true));
    let (turn, t) = run(&h, "codex/gpt-6-sol")
        .await
        .expect("second account serves");
    assert_eq!(t.account.as_ref().unwrap().id, "low");
    assert_eq!(text(&turn), "from low");
    let call = turn
        .content
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("function call parsed");
    assert_eq!(call["id"], "call_9");
    assert_eq!(call["input"]["path"], "a.txt");
    assert_eq!(turn.stop_reason, "tool_use");
    assert_eq!((turn.usage.input, turn.usage.cache_read), (100, 900));

    let reqs = &m.reqs()[before..];
    assert!(
        reqs[0].headers.contains("authorization: bearer tok-high"),
        "priority first"
    );
    assert!(reqs[0].headers.contains("chatgpt-account-id: acc-high"));
    assert!(reqs[1].headers.contains("authorization: bearer tok-low"));
    assert_eq!(reqs[0].path, "/codex/responses");
    let b = &reqs[0].body;
    assert_eq!(
        (
            b["instructions"].as_str(),
            b["store"].as_bool(),
            b["prompt_cache_key"].as_str()
        ),
        (Some("SYS"), Some(false), Some("ol-t1"))
    );
    assert_eq!(b["input"][0]["content"][0]["type"], "input_text");

    // The exhausted account is benched; rate-limit headers fed the usage view.
    let views = h.accts.views(&h.settings.read().await.accounts);
    let high = views.iter().find(|v| v.id == "high").unwrap();
    assert!(!high.available && high.cooldown_until > accounts::now() + 3600);
    let low = views.iter().find(|v| v.id == "low").unwrap();
    assert!(low.active);
    assert_eq!(low.usage.as_ref().unwrap().windows[0].used, 42.0);
    assert_eq!(low.usage.as_ref().unwrap().windows[0].label, "5h");

    // Stats: one success on "low", one error on "high", cache read + time-to-first-token captured.
    let st = h.stats.snapshot();
    let mc = &st.models["codex/gpt-6-sol"];
    assert_eq!(
        (mc.requests, mc.errors, mc.cache_read, mc.ttft_n),
        (1, 1, 900, 1)
    );
    assert_eq!(
        (st.accounts["low"].requests, st.accounts["high"].errors),
        (1, 1)
    );
    assert_eq!(st.agents["main"].requests, 1);
    assert_eq!(st.events["retries"], 1);
    // Next request goes straight to the warm, available account (no wasted call on "high").
    let before = m.reqs().len();
    m.push(codex_ok("again", false));
    let (_, t) = run(&h, "codex/gpt-6-sol").await.ok().unwrap();
    assert_eq!(t.account.unwrap().id, "low");
    assert_eq!(m.reqs().len() - before, 1);
}

async fn key_pool_rotates_keys(m: &Mock) {
    let (cp, mut key) = custom("kp", &m.base, false);
    key.1.api_keys = vec![
        "key-a".to_string(),
        "key-b".to_string(),
        "key-c".to_string(),
    ];
    key.1.key_pool = true;
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let (h, _) = harness(s, vec![task("t1", "kp/m")]);
    let before = m.reqs().len();

    // Key A returns 401 (bad key) → key B returns 429 (rate-limited) → key C succeeds.
    m.push(err(401, json!({"error": {"message": "invalid"}})));
    m.push(err(
        429,
        json!({"error": {"type": "rate_limit", "message": "too many"}}),
    ));
    m.push(openai_ok("c wins"));
    let (turn, t) = run(&h, "kp/m").await.expect("last key succeeds");
    assert_eq!(text(&turn), "c wins");
    assert_eq!(t.api_key.as_deref(), Some("key-c"));

    let reqs = &m.reqs()[before..];
    assert_eq!(reqs.len(), 3);
    assert!(reqs[0].headers.contains("authorization: bearer key-a"));
    assert!(reqs[1].headers.contains("authorization: bearer key-b"));
    assert!(reqs[2].headers.contains("authorization: bearer key-c"));
}

async fn route_falls_back_then_pauses_and_resumes(m: &Mock) {
    let (cp, key) = custom("backup", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.accounts = vec![acct("only", "codex", 10)];
    s.routes.push(Route {
        id: "subs".into(),
        name: "Subs first".into(),
        steps: vec!["codex/gpt-6-sol".into(), "backup/m".into()],
        on_exhausted: "pause".into(),
        ..Default::default()
    });
    router::set_routes(&s.routes);
    let (h, events) = harness(s, vec![task("t1", "route/subs")]);

    // Codex account out → falls to backup provider.
    m.push(err(
        429,
        json!({"error": {"type": "usage_limit_reached", "message": "limit"}}),
    ));
    m.push(openai_ok("backup served"));
    let (turn, t) = run(&h, "route/subs").await.expect("fallback works");
    assert_eq!(
        (text(&turn).as_str(), t.prov.as_str()),
        ("backup served", "backup")
    );
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|(_, v)| v["payload"]["text"]
                .as_str()
                .is_some_and(|s| s.starts_with("Now using backup/m"))),
        "user is told about the switch"
    );

    // Everything out → the task pauses instead of failing; resuming retries from the top.
    // The codex account is still benched from above, so only the backup is tried.
    m.push(err(
        402,
        json!({"error": {"message": "insufficient credits"}}),
    ));
    let h2 = h.clone();
    let job = tokio::spawn(async move { run(&h2, "route/subs").await.map(|(t, _)| text(&t)) });
    let mut paused = false;
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if h.is_paused("t1").await {
            paused = true;
            break;
        }
    }
    assert!(paused, "chain exhausted → paused");
    let p = h
        .task("t1")
        .await
        .unwrap()
        .lock()
        .await
        .paused
        .clone()
        .unwrap();
    assert_eq!(p.kind, "exhausted");
    assert!(!job.is_finished(), "request is parked, not failed");
    m.push(openai_ok("after resume"));
    runner::resume(&h, "t1", None).await.ok();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), job)
        .await
        .expect("resumes")
        .unwrap();
    assert_eq!(out.ok().as_deref(), Some("after resume"));
}

/// A swap has to take the chat's *fallback chain* with it, not just the model.
/// The old model's route named it as a head, so the chat kept a chain built
/// around the model the user had just swapped away from — and resuming walked
/// that chain, landing on models the swap was supposed to replace. Restarting
/// fixed it only because a restart recomputes the route from the model on disk.
async fn swap_while_paused_resumes_on_the_new_models_chain(m: &Mock) {
    let (old, old_key) = custom("old", &m.base, false);
    let (new, new_key) = custom("new", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(old);
    s.custom_providers.push(new);
    s.providers.insert(old_key.0, old_key.1);
    s.providers.insert(new_key.0, new_key.1);
    // The chain `old/m` is picked with, and the one `new/m` would be picked with.
    s.routes = vec![
        Route {
            id: "oldchain".into(),
            name: "Old".into(),
            heads: vec!["old/m".into()],
            steps: vec!["old/m".into(), "old/backup".into()],
            ..Default::default()
        },
        Route {
            id: "newchain".into(),
            name: "New".into(),
            heads: vec!["new/m".into()],
            steps: vec!["new/m".into(), "new/backup".into()],
            ..Default::default()
        },
    ];
    router::set_routes(&s.routes);
    let mut t = task("t1", "old/m");
    t.route = router::default_route("old/m");
    assert_eq!(t.route, "oldchain", "the chat starts on its model's chain");
    let (h, _) = harness(s, vec![t]);

    // Every model on the old chain is out → the chat parks, waiting for the user.
    m.push(err(
        429,
        json!({"error": {"type": "usage_limit_reached", "message": "out of usage"}}),
    ));
    m.push(err(
        429,
        json!({"error": {"type": "usage_limit_reached", "message": "out of usage"}}),
    ));
    let h2 = h.clone();
    let job = tokio::spawn(async move { run(&h2, "old/m").await.map(|(t, _)| text(&t)) });
    let mut paused = false;
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if h.is_paused("t1").await {
            paused = true;
            break;
        }
    }
    assert!(paused, "the old chain is out → paused");

    // The user swaps in the pause bar and resumes. Driven through the real
    // `swap_task` (via `update_task`), not a hand-written route: the bug was in
    // what the swap writes, so a test that sets the route itself would agree with
    // itself and prove nothing.
    h.update_task("t1", |t| {
        let map: HashMap<String, String> = [("old/m".to_string(), "new/m".to_string())].into();
        crate::swap_task(t, &map, &HashMap::new(), false);
    })
    .await;
    assert_eq!(
        h.task("t1").await.unwrap().lock().await.route,
        "newchain",
        "the chain follows the model the swap moved the chat to"
    );
    m.push(openai_ok("served by the new chain"));
    runner::resume(&h, "t1", None).await.ok();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), job)
        .await
        .expect("resumes")
        .unwrap();
    assert_eq!(out.ok().as_deref(), Some("served by the new chain"));

    let reqs = m.reqs();
    let last = reqs.last().unwrap();
    assert_eq!(
        last.body["model"], "m",
        "the resumed request went to the new provider"
    );
    assert!(
        last.headers.contains("authorization: bearer k-new"),
        "and off the new provider's key, not the old chain's: {}",
        last.headers
    );
    router::set_routes(&[]);
}

/// The exact bug from the field: a chat runs out of usage, the user swaps the
/// model in the pause bar, hits Resume — and the old, dead model was still
/// what the parked request retried. The swap has to reach the parked request.
async fn swap_while_paused_retries_the_new_model(m: &Mock) {
    let (dead, dead_key) = custom("dead", &m.base, false);
    let (live, live_key) = custom("live", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(dead);
    s.custom_providers.push(live);
    s.providers.insert(dead_key.0, dead_key.1);
    s.providers.insert(live_key.0, live_key.1);
    let (h, _) = harness(s, vec![task("t1", "dead/m")]);

    // Out of usage → parked, waiting for the user.
    m.push(err(
        429,
        json!({"error": {"type": "usage_limit_reached", "message": "out of usage"}}),
    ));
    let h2 = h.clone();
    let job = tokio::spawn(async move { run(&h2, "dead/m").await.map(|(t, _)| text(&t)) });
    let mut paused = false;
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if h.is_paused("t1").await {
            paused = true;
            break;
        }
    }
    assert!(paused, "out of usage → paused");

    // The user swaps the model for this chat while it sits paused, then resumes.
    // (What models_swap writes: the chat's model.)
    h.update_task("t1", |t| t.model = "live/m".into()).await;
    m.push(openai_ok("new model served"));
    runner::resume(&h, "t1", None).await.ok();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), job)
        .await
        .expect("resumes")
        .unwrap();
    assert_eq!(
        out.ok().as_deref(),
        Some("new model served"),
        "resumed on the swapped-in model"
    );
    let reqs = m.reqs();
    assert!(
        reqs.last()
            .unwrap()
            .headers
            .contains("authorization: bearer k-live"),
        "went to the new provider, not the dead one"
    );
}

/// A chat's effort is re-read on every retry pass so a swap made while the request
/// waited reaches it. That re-read must NOT touch a request that picked its own
/// level: compaction caps itself at Medium (`.max(2)`, and effort counts down from
/// Max), so a chat sitting on Max must not have the summary written at Max.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_shot_request_keeps_the_level_it_picked() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-oneshot-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pe", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.model_configs.push(providers::ModelInfo {
        id: "pe/m".into(),
        name: "Pe M".into(),
        provider: "pe".into(),
        context: 128_000,
        output: 8_000,
        input_price: 0.0,
        output_price: 0.0,
        effort: true,
        input_types: vec!["text".into()],
        capabilities: vec![],
        reasoning_levels: vec!["low".into(), "medium".into(), "high".into()],
        reasoning_param: "reasoning_effort".into(),
        custom: true,
        enabled: true,
    });
    // The chat is on Max, which is exactly the level compaction holds itself back
    // from — re-reading it would spend the dear one on the summary.
    let mut t = task("t1", "pe/m");
    t.effort = 0;
    t.status = "idle".into();
    t.messages.push(Message::user_text("hi"));
    t.messages.push(Message {
        role: "assistant".into(),
        content: vec![json!({"type": "text", "text": "hello"})],
        model: String::new(),
    });
    let (h, _) = harness(s, vec![t]);
    providers::set_custom_models(&h.settings.read().await.model_configs);

    m.push(openai_ok("a summary"));
    runner::compact(&h, "t1", &CancellationToken::new(), None)
        .await
        .expect("compacts");

    assert_eq!(
        m.reqs().last().unwrap().body["reasoning_effort"],
        "medium",
        "the summary kept its own cap instead of following the chat up to max"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The same pause, one level deeper: the reasoning travels too. The request was
/// built while the chat was on one model *and* one level; swapping both while it
/// sat parked and resuming has to send the pair the user just chose.
///
/// The model half of this already had a test, and effort had none — so the stale
/// level went out silently, on a model whose ladder it may not even share.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn swap_while_paused_retries_the_new_model_and_effort() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-swap-effort-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (dead, dead_key) = custom("dead", &m.base, false);
    let (live, live_key) = custom("live", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(dead);
    s.custom_providers.push(live);
    s.providers.insert(dead_key.0, dead_key.1);
    s.providers.insert(live_key.0, live_key.1);
    // Both models take a level, so what is on the wire says which model and which
    // level served the resumed request.
    for (id, prov) in [("dead/m", "dead"), ("live/m", "live")] {
        s.model_configs.push(providers::ModelInfo {
            id: id.into(),
            name: id.into(),
            provider: prov.into(),
            context: 128_000,
            output: 8_000,
            input_price: 0.0,
            output_price: 0.0,
            effort: true,
            input_types: vec!["text".into()],
            capabilities: vec![],
            reasoning_levels: vec!["low".into(), "medium".into(), "high".into()],
            reasoning_param: "reasoning_effort".into(),
            custom: true,
            enabled: true,
        });
    }
    let mut t = task("t1", "dead/m");
    // Effort 0 is Max, so it is a level the model cannot confuse with the chat's.
    t.effort = 0;
    let (h, _) = harness(s, vec![t]);
    providers::set_custom_models(&h.settings.read().await.model_configs);

    m.push(err(
        429,
        json!({"error": {"type": "usage_limit_reached", "message": "out of usage"}}),
    ));
    let h2 = h.clone();
    let job = tokio::spawn(async move { run(&h2, "dead/m").await.map(|(t, _)| text(&t)) });
    let mut paused = false;
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if h.is_paused("t1").await {
            paused = true;
            break;
        }
    }
    assert!(paused, "out of usage → paused");

    // What models_swap writes for one row: the new model *and* the level picked
    // for it. Level 4 is Low, distinct from the Max the chat was parked on.
    h.update_task("t1", |t| {
        t.model = "live/m".into();
        t.effort = 4;
    })
    .await;
    m.push(openai_ok("new model served"));
    runner::resume(&h, "t1", None).await.ok();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), job)
        .await
        .expect("resumes")
        .unwrap();
    assert_eq!(out.ok().as_deref(), Some("new model served"));

    let reqs = m.reqs();
    let last = reqs.last().unwrap();
    assert!(
        last.headers.contains("authorization: bearer k-live"),
        "went to the new provider, not the dead one"
    );
    assert_eq!(
        last.body["reasoning_effort"], "low",
        "the resumed request carries the level the swap set, not the one it parked on"
    );
}

async fn claude_subscription_sends_prelude_and_oauth(m: &Mock) {
    let s = Settings {
        accounts: vec![acct("c1", "claude", 1)],
        ..Default::default()
    };
    let (h, _) = harness(s, vec![task("t1", "claude/claude-sonnet-5")]);
    let before = m.reqs().len();
    let evs = [
        json!({"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":500}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hey"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ];
    let mut r = sse(&evs);
    r.headers = vec![(
        "anthropic-ratelimit-unified-5h-utilization".into(),
        "0.3".into(),
    )];
    m.push(r);
    let (turn, _) = run(&h, "claude/claude-sonnet-5")
        .await
        .expect("claude sub works");
    assert_eq!(text(&turn), "hey");
    assert_eq!(turn.model, "claude-sonnet-5");
    let r = &m.reqs()[before];
    assert!(r.path.starts_with("/v1/messages"));
    assert!(r.headers.contains("authorization: bearer tok-c1"));
    assert!(r.headers.contains("oauth-2025-04-20"));
    assert!(!r.headers.contains("x-api-key"));
    assert_eq!(r.body["system"][0]["text"], accounts::CLAUDE_PRELUDE);
    assert_eq!(r.body["system"][1]["text"], "SYS");
    assert!(
        r.body["system"][1]["cache_control"].is_object(),
        "our prompt keeps its cache breakpoint"
    );
    let v = h.accts.views(&h.settings.read().await.accounts);
    assert_eq!(v[0].usage.as_ref().unwrap().windows[0].used, 30.0);
}

async fn too_long_is_reported_for_compaction(m: &Mock) {
    let (cp, key) = custom("small", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let (h, _) = harness(s, vec![task("t1", "small/m")]);
    m.push(err(
        400,
        json!({"error": {"message": "This model's maximum context length is 8192 tokens"}}),
    ));
    assert!(matches!(
        run(&h, "small/m").await,
        Err(RouteErr::TooLong(_))
    ));
}

async fn pause_interrupts_a_stream_and_retries(m: &Mock) {
    let (cp, key) = custom("slowpoke", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let (h, _) = harness(s, vec![task("t1", "slowpoke/m")]);
    // Manual pause before the request: it waits, sends nothing, then goes once resumed.
    h.pause_task("t1", "manual", "Paused by you").await;
    let before = m.reqs().len();
    m.push(openai_ok("waited"));
    let h2 = h.clone();
    let job = tokio::spawn(async move { run(&h2, "slowpoke/m").await.map(|(t, _)| text(&t)) });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(m.reqs().len(), before, "nothing sent while paused");
    runner::resume(&h, "t1", None).await.ok();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.ok().as_deref(), Some("waited"));
}

/// Messaging a paused chat resumes it, and the model must answer the *message* —
/// not the turn it was frozen on. The frozen request was built before the text
/// existed, so replaying it verbatim is a reply to a turn the user has already
/// spoken over. The router abandons that attempt, and the message waits in
/// history for the replacement request the run builds around it.
async fn message_into_a_paused_chat_never_answers_the_frozen_turn(m: &Mock) {
    let (cp, key) = custom("frozen", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "frozen/m");
    t.messages = vec![Message::user_text("do the thing")];
    let (h, _) = harness(s, vec![t]);
    h.pause_task("t1", "manual", "Paused by you").await;
    let before = m.reqs().len();

    // A request is built and the chat is frozen holding it.
    m.push(openai_ok("answer to the frozen turn"));
    let h2 = h.clone();
    let frozen = tokio::spawn(async move { run(&h2, "frozen/m").await.map(|(t, _)| text(&t)) });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        m.reqs().len(),
        before,
        "nothing is sent while the chat is frozen"
    );

    // The user types into the frozen chat: the text goes to history and the pause
    // lifts, in that order, in one call.
    runner::send(h.clone(), "t1".into(), "actually, do this instead".into())
        .await
        .ok();
    {
        let t = h.task("t1").await.unwrap();
        let t = t.lock().await;
        assert!(t.paused.is_none(), "messaging lifts the pause");
        assert!(t.unpaused, "and opts this one chat out of a global pause");
        let last = t.messages.last().expect("the message is in history");
        assert_eq!(
            last.content[0]["text"], "actually, do this instead",
            "after the turn it answers, not before"
        );
    }

    // The frozen attempt wakes up. It must give the attempt up, not run it: the
    // whole turn it was holding is one the user has just answered.
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), frozen)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(out, Err(RouteErr::Refresh)),
        "the frozen attempt is abandoned, not replayed (got {out:?})"
    );
    assert_eq!(
        m.reqs().len(),
        before,
        "and the frozen turn was never answered"
    );
    // The fix is the *absence* of a request, so the response scripted for it is
    // still queued. Drop it here: every scenario shares this queue, and leaving it
    // here would hand the next one a stale answer.
    m.script.lock().unwrap().clear();
}

/// A chat that is stopped but not running must still start when the user types
/// into it. `send` lifts a pause and returns — on the assumption that something
/// is in flight to read the message, or will be shortly. After a stop there is
/// nothing in flight, so the message just sat in history: the transcript showed
/// it, the chat stayed "stopped", and the agent never woke. Editing a message and
/// pressing Send takes this same path, which is why an edit after a stop looked
/// like it had done nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sending_to_a_stopped_chat_starts_a_run() {
    let dir = std::env::temp_dir().join(format!("openleash-sendstop-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "p/m");
    // Exactly what a stop leaves: no run, and a pause still holding the chat
    // (Esc pauses, Esc again stops — and dismiss_pause is what clears it).
    t.status = "stopped".into();
    t.paused = Some(Pause {
        reason: "Paused by you".into(),
        kind: "manual".into(),
        since: chrono::Utc::now(),
    });
    let (h, _) = harness(s, vec![t]);

    m.push(openai_ok("on it"));
    runner::send(h.clone(), "t1".into(), "do it this way instead".into())
        .await
        .unwrap();
    wait_idle(&h, "t1").await;

    assert!(
        !m.reqs().is_empty(),
        "the message reached the model, so a run really started"
    );
    let t = h.task("t1").await.unwrap();
    let t = t.lock().await;
    assert!(
        t.paused.is_none(),
        "and the pause is lifted, or the next turn could not run either"
    );
    assert_eq!(
        t.status, "done",
        "the chat is working again, not still stopped"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ───────────────────────────── the global pause ─────────────────────────────

/// The field bug: a new chat started while "Pause all" was on never got a
/// reply. The chat was created, the prompt went into history, a run was spawned
/// — and then parked forever in `wait_unpaused`, because `is_paused` froze every
/// chat under the flag regardless of status, while the UI listed only the ones
/// that were running or waiting. A brand-new chat is `idle` at creation, so it
/// was frozen by the predicate and invisible everywhere else: created, silent,
/// with nothing saying why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_chat_under_a_global_pause_is_answered() {
    let dir = std::env::temp_dir().join(format!("openleash-newpaused-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "p/m");
    // Exactly what `task_create` builds: running has not started yet.
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);
    h.settings.write().await.paused_all = true;

    // A chat that isn't working has nothing for the global pause to hold, or a
    // prompt sent to it could never be answered.
    assert!(
        !h.is_paused("t1").await,
        "an idle chat is not frozen by a global pause"
    );

    m.push(openai_ok("on it"));
    let before = m.reqs().len();
    runner::send(h.clone(), "t1".into(), "do the thing".into())
        .await
        .unwrap();
    wait_idle(&h, "t1").await;

    assert!(
        m.reqs().len() > before,
        "the prompt was actually sent, not parked"
    );
    let t = h.task("t1").await.unwrap();
    let t = t.lock().await;
    assert_eq!(t.status, "done", "and answered");
    assert!(
        t.messages.iter().any(|m| m
            .content
            .iter()
            .any(|b| b["text"].as_str() == Some("do the thing"))),
        "the prompt it was given is the one it answered"
    );
    // The other chats are still frozen: letting this one through is a per-chat
    // opt-out, not a release.
    assert!(
        h.settings.read().await.paused_all,
        "and the global pause is still on for everyone else"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The other half of the same fix, and the reason the UI and backend had to
/// agree: the global pause *does* still freeze a chat the moment it is working.
/// Only the chats that had nothing running are let through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_global_pause_still_freezes_a_working_chat() {
    let s = Settings::default();
    let (h, _) = harness(s, vec![task("t1", "p/m")]);
    assert!(!h.is_paused("t1").await, "nothing is paused yet");
    h.settings.write().await.paused_all = true;
    assert!(
        h.is_paused("t1").await,
        "a working chat is frozen by the global flag"
    );
    // …and opts out of it, which is what resuming one chat does.
    h.update_task("t1", |t| t.unpaused = true).await;
    assert!(!h.is_paused("t1").await, "and thawed by its own opt-out");
    // Its own pause still holds it, opt-out or not.
    h.pause_task("t1", "manual", "Paused by you").await;
    assert!(
        h.is_paused("t1").await,
        "a per-chat pause is independent of the global one"
    );
}

/// The second field bug: with everything paused, resuming one chat went through
/// the global resume, which cleared the flag and woke every other chat. This is
/// the per-chat resume's own contract — it lifts exactly one chat and leaves
/// `paused_all` alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resuming_one_chat_leaves_the_others_frozen() {
    let s = Settings::default();
    let (h, _) = harness(s, vec![task("a", "p/m"), task("b", "p/m")]);
    h.settings.write().await.paused_all = true;
    assert!(
        h.is_paused("a").await && h.is_paused("b").await,
        "both frozen by the global pause"
    );

    runner::resume(&h, "a", None).await.ok();

    assert!(
        !h.is_paused("a").await,
        "the chat that was resumed is running again"
    );
    assert!(
        h.is_paused("b").await,
        "and the other one is still frozen — the bug was that it wasn't"
    );
    assert!(
        h.settings.read().await.paused_all,
        "the global pause itself is untouched by a per-chat resume"
    );
}

/// A drag is a position, not a property of the chat: the user put this chat
/// somewhere on purpose, so it must stay there until they do something *in* it.
///
/// The field bug this pins: dragging a chat gave it an `order`, and `order`
/// wins outright over `touched_at` in the sidebar's sort key (`sortKey` in
/// `ui/Chrome.tsx`). Nothing ever cleared it — not sending a message, not
/// answering a question, not resuming. So the ten call sites that say "the user
/// did something, so put this chat at the top" were silently false for every
/// chat that had ever been dragged: the touch landed, `touched_at` moved, and
/// the row never moved. The manual layout was also unescapable, because
/// `order` could not be cleared back to 0 from the UI.
///
/// Touching releases the position and the chat goes back to sorting by when the
/// user last used it, which is what those call sites claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interacting_with_a_dragged_chat_releases_its_manual_position() {
    let (h, _) = harness(Settings::default(), vec![task("t1", "p/m")]);
    // The user dragged it to the top of the list.
    h.update_task("t1", |t| t.order = 5_000.0).await;
    assert_eq!(h.task("t1").await.unwrap().lock().await.order, 5_000.0);

    // And now they sent a message in it.
    h.touch("t1").await;

    let t = h.task("t1").await.unwrap();
    let t = t.lock().await;
    assert_eq!(
        t.order, 0.0,
        "a touch has to clear the manual position, or the chat is stranded \
         wherever the drag left it and 'the user did something, so it goes to \
         the top' is a lie"
    );
}

/// The position is only released by a user action, and the touch is the whole
/// of that: a chat whose *agent* is busy must not lose its spot. `updated_at`
/// moves on every agent step, so sorting on it would let a working background
/// chat shove a deliberately-placed chat down the list.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_step_does_not_release_a_manual_position() {
    let (h, _) = harness(Settings::default(), vec![task("t1", "p/m")]);
    h.update_task("t1", |t| t.order = 5_000.0).await;

    // Every step of a run goes through `update_task`.
    h.update_task("t1", |t| t.step = "tool".into()).await;

    assert_eq!(
        h.task("t1").await.unwrap().lock().await.order,
        5_000.0,
        "the agent working in a chat is not the user rearranging the sidebar"
    );
}

/// A flag that holds nothing is not a pause, and must not stay on as one.
///
/// The field bug this pins: "it says everything is paused when literally nothing
/// is paused". The flag is sticky — a quit sets it, and it comes back off disk —
/// but what it holds is only chats that are *working*. When every chat it froze
/// has since finished, been stopped, or been closed out by the next launch, the
/// flag holds nothing while everything that reads it raw still says a global
/// pause is on. The paused list asks and says "Nothing is frozen right now"; the
/// banner did not ask, and said "Everything is paused".
///
/// So the flag is dropped as soon as there is nothing left for it to hold. The
/// frontend gates its own banner on the same rule, but only this can actually
/// clear the flag — and clearing it is what stops it being written back out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_global_pause_that_holds_nothing_is_cleared() {
    let s = Settings {
        paused_all: true,
        paused_reason: "Everything stopped for the app to close".into(),
        ..Default::default()
    };
    // Nothing is working: the shape left behind by a quit, since `load_tasks`
    // turns anything that was running into `stopped`.
    let (h, _) = harness(s, vec![task("a", "p/m"), task("b", "p/m")]);
    for id in ["a", "b"] {
        h.update_task(id, |t| t.status = "stopped".into()).await;
    }
    assert!(
        !h.is_paused("a").await && !h.is_paused("b").await,
        "nothing is actually held — this is the report"
    );

    assert!(
        h.clear_stranded_global_pause().await,
        "an inert flag is dropped, not left to lie"
    );
    assert!(
        !h.settings.read().await.paused_all,
        "so the next launch does not find it again"
    );
    assert!(
        h.settings.read().await.paused_reason.is_empty(),
        "and the reason goes with it"
    );
}

/// The other half of that: a flag still holding a working chat is a real pause,
/// and the reconcile must not touch it. Getting this wrong releases chats the
/// user deliberately froze — the worse failure of the two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_global_pause_still_holding_a_chat_is_left_alone() {
    let s = Settings {
        paused_all: true,
        ..Default::default()
    };
    let (h, _) = harness(s, vec![task("a", "p/m"), task("b", "p/m")]);
    // "a" finished, so it is not held. "b" is still working, so the flag is real.
    h.update_task("a", |t| t.status = "done".into()).await;

    assert!(
        !h.clear_stranded_global_pause().await,
        "one working chat is enough to keep the flag"
    );
    assert!(
        h.settings.read().await.paused_all,
        "and the flag stays on for the chat it is holding"
    );
    assert!(h.is_paused("b").await, "which is still frozen");
    assert!(
        !h.is_paused("a").await,
        "and the finished one is not, with or without the flag"
    );
}

/// A working chat that opted out does not keep the flag alive either: the freeze
/// never held it, so a flag holding only opted-out chats is holding nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_opted_out_chat_does_not_keep_a_global_pause_alive() {
    let s = Settings {
        paused_all: true,
        ..Default::default()
    };
    let (h, _) = harness(s, vec![task("a", "p/m")]);
    h.update_task("a", |t| t.unpaused = true).await;

    assert!(h.clear_stranded_global_pause().await);
    assert!(!h.settings.read().await.paused_all);
    assert!(
        !h.is_paused("a").await,
        "and the chat that opted out is running, as the user asked"
    );
}

// ───────────────────────────── /btw side questions ─────────────────────────────

/// Point a task at the mock through a `custom()` provider. `pe/m` carries a real
/// reasoning dial, so the effort assertions below mean something.
async fn pe(h: &Arc<Harness>, m: &Mock) {
    let (cp, key) = custom("pe", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.model_configs.push(providers::ModelInfo {
        id: "pe/m".into(),
        name: "Pe M".into(),
        provider: "pe".into(),
        context: 128_000,
        output: 8_000,
        input_price: 0.0,
        output_price: 0.0,
        effort: true,
        input_types: vec!["text".into()],
        capabilities: vec![],
        reasoning_levels: vec!["low".into(), "medium".into(), "high".into()],
        reasoning_param: "reasoning_effort".into(),
        custom: true,
        enabled: true,
    });
    *h.settings.write().await = s;
    providers::set_custom_models(&h.settings.read().await.model_configs);
}

/// One exchange, idle and warm.
fn talked(model: &str) -> Task {
    let mut t = task("t1", model);
    t.status = "idle".into();
    t.messages
        .push(Message::user_text("add a retry to the fetcher"));
    t.messages.push(Message {
        role: "assistant".into(),
        content: vec![json!({"type": "text", "text": "Added three retries with backoff."})],
        model: String::new(),
    });
    t
}

/// `/btw` is a side question, and the two properties that make it one are on the
/// wire: **no tools** are offered, and the reply goes nowhere near the chat's
/// history. Both are asserted against the request that went out and the task as
/// it stands afterwards.
///
/// The empty tool list is the security-shaped half. A model can emit a
/// `tool_use` for a tool it was never offered, so the list is the guard and the
/// prompt is not — and the prompt is the half a long conversation can talk its
/// way around.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_side_question_sends_no_tools_and_leaves_history_alone() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-btw-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let before = talked("pe/m").messages;
    let (h, _) = harness(Settings::default(), vec![talked("pe/m")]);
    pe(&h, &m).await;

    m.push(openai_ok("The fetcher retries three times."));
    let answer = runner::btw(&h, "t1", "how many retries?", &CancellationToken::new())
        .await
        .expect("answers");

    assert_eq!(answer, "The fetcher retries three times.");
    let req = m.reqs().pop().expect("one request");
    // Absent and empty are both fine — which one depends on the provider's
    // serializer. A non-empty list is the failure: the model could have gone and
    // done the work the user only asked about.
    let sent = req.body["tools"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        sent, 0,
        "a side question was offered {sent} tool(s): it could have gone and done the \
         work the user only asked about"
    );
    assert!(
        serde_json::to_string(&req.body["messages"])
            .unwrap()
            .contains("how many retries?"),
        "the question reaches the model"
    );
    // The whole point: not one message added, and the frozen prefix untouched, so
    // the next real turn reads exactly what it read before. Compared as JSON
    // because `Message` has no `PartialEq` — and this is the shape it persists as.
    let tr = h.task("t1").await.unwrap();
    let g = tr.lock().await;
    assert_eq!(
        serde_json::to_value(&g.messages).unwrap(),
        serde_json::to_value(&before).unwrap(),
        "the side answer was added to history"
    );
    assert!(g.system.is_empty(), "the chat's prefix was rewritten");
    assert!(
        g.items.is_empty(),
        "the caller owns the timeline row, not the request"
    );
    drop(g);
    m.assert_queue_empty();
    let _ = std::fs::remove_dir_all(dir);
}

/// The cached tail is the quiet way a side question would still pollute a chat:
/// sharing the task's `cache_key` would let this reply become the prefix the next
/// real turn reads, so the key has to be its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_side_question_gets_its_own_cache_key() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-btwkey-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (h, _) = harness(Settings::default(), vec![talked("pe/m")]);
    pe(&h, &m).await;

    m.push(openai_ok("three"));
    runner::btw(&h, "t1", "how many?", &CancellationToken::new())
        .await
        .unwrap();

    let req = m.reqs().pop().unwrap();
    assert_ne!(
        req.body["prompt_cache_key"].as_str().unwrap_or_default(),
        "ol-t1",
        "a side answer shared the chat's cache key: it would become the cached tail \
         the next real turn reads"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A model can call a tool it was never offered. Nothing executes it — the list
/// was empty, so there is no path to execution — but a turn that is *only* a tool
/// call holds no answer, and returning "" would render as a blank row that reads
/// like a broken command.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_side_question_that_only_reaches_for_a_tool_says_so() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-btwtool-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (h, _) = harness(Settings::default(), vec![talked("pe/m")]);
    pe(&h, &m).await;

    m.push(openai_call("grep", json!({"pattern": "retry"})));
    let err = runner::btw(&h, "t1", "where is it?", &CancellationToken::new())
        .await
        .expect_err("reports instead of returning nothing");

    assert!(
        err.contains("can't use tools"),
        "the failure names the reason, got: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A question on an empty chat has nothing to answer from, and saying so beats a
/// confident answer invented out of a system prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_side_question_on_an_empty_chat_says_it_has_nothing_to_go_on() {
    let (h, _) = harness(Settings::default(), vec![task("t1", "pe/m")]);
    let err = runner::btw(&h, "t1", "what is this?", &CancellationToken::new())
        .await
        .expect_err("refuses");
    assert!(err.contains("nothing to go on"), "got: {err}");
}

// ───────────────────────────── the agent loop ─────────────────────────────
pub(crate) fn openai_call(name: &str, args: Value) -> Resp {
    sse(&[
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call_{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]}),
    ])
}

/// The runner exposes `tool_search` only once the frozen catalog crosses its
/// threshold, and a searched tool remains declared on the following request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_search_loads_a_deferred_mcp_tool_into_the_next_request() {
    let dir = std::env::temp_dir().join(format!("openleash-toolsearch-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let mcp_server = MockMcpServer::start(12).await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.tool_search = true;
    s.mcp.push(McpServerCfg {
        name: "docs".into(),
        transport: "http".into(),
        url: mcp_server.base.clone(),
        enabled: true,
        defer: true,
        ..Default::default()
    });
    let mut t = task("toolsearch", "p/m");
    t.status = "idle".into();
    let (h, _) = harness(s.clone(), vec![t]);
    h.mcp.sync(&h.settings.read().await.mcp).await;
    assert_eq!(
        h.mcp.tool_schemas().await.len(),
        12,
        "schemas come from the connected MCP server"
    );
    m.push(openai_call(
        "tool_search",
        json!({"query":"lookup documentation", "limit":1}),
    ));
    m.push(openai_ok("The documentation lookup tool is ready."));
    runner::send(h.clone(), "toolsearch".into(), "Find docs".into())
        .await
        .unwrap();
    wait_idle(&h, "toolsearch").await;
    let reqs = m.reqs();
    assert_eq!(reqs.len(), 2, "search then a follow-up request");
    let first: Vec<&str> = reqs[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["function"]["name"].as_str())
        .collect();
    assert!(
        first.contains(&"tool_search"),
        "the search helper is offered when MCP tools are deferred"
    );
    assert!(
        !first.contains(&"mcp__docs__lookup_0"),
        "the tail starts deferred"
    );
    let second: Vec<&str> = reqs[1].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["function"]["name"].as_str())
        .collect();
    assert!(
        second.contains(&"tool_search"),
        "other MCP tools are still deferred, so search remains available"
    );
    assert!(
        second.contains(&"mcp__docs__lookup_0"),
        "a searched schema is declared next turn"
    );

    // With the same connected MCP catalog but deferral disabled, the tool list
    // includes the MCP schemas directly and does not advertise tool_search.
    s.mcp[0].defer = false;
    let mut no_defer_task = task("toolsearch-nodefer", "p/m");
    no_defer_task.status = "idle".into();
    let (h_no_defer, _) = harness(s, vec![no_defer_task]);
    h_no_defer
        .mcp
        .sync(&h_no_defer.settings.read().await.mcp)
        .await;
    m.push(openai_ok(
        "All MCP tools are declared when deferral is off.",
    ));
    runner::send(
        h_no_defer.clone(),
        "toolsearch-nodefer".into(),
        "Find docs without deferral".into(),
    )
    .await
    .unwrap();
    wait_idle(&h_no_defer, "toolsearch-nodefer").await;
    let reqs = m.reqs();
    assert_eq!(
        reqs.len(),
        3,
        "the non-deferred request follows the search pair"
    );
    let without_deferral: Vec<&str> = reqs[2].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["function"]["name"].as_str())
        .collect();
    assert!(
        !without_deferral.contains(&"tool_search"),
        "tool_search is absent when the MCP server does not defer tools"
    );
    assert!(
        without_deferral.contains(&"mcp__docs__lookup_0")
            && without_deferral.contains(&"mcp__docs__lookup_11"),
        "all MCP schemas are directly declared when deferral is off"
    );
    h_no_defer.mcp.sync(&[]).await;
    h.mcp.sync(&[]).await;
    drop(mcp_server);
    let _ = std::fs::remove_dir_all(dir);
}

/// Main agent delegates to a sub-agent, the sub-agent asks a question that the
/// main agent answers on a side request, then a mid-task assist-mode change is
/// announced — while the cached prefix (system + tools + earlier history) stays byte-identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_loop_keeps_cache_prefix_and_relays_questions() {
    let dir = std::env::temp_dir().join(format!("openleash-loop-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t2", "p/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call("task", json!({"description": "find it", "prompt": "Find where X lives", "subagent_type": "explore"}))); // main 1
    m.push(openai_call(
        "ask",
        json!({"question": "Which folder should I search?"}),
    )); // sub 1
    m.push(openai_ok("Search src/.")); // side question to main
    m.push(openai_ok("X lives in src/a.rs:3")); // sub 2 (report)
    m.push(openai_ok("Found it.")); // main 2
    runner::send(h.clone(), "t2".into(), "where is X?".into())
        .await
        .unwrap();
    wait_idle(&h, "t2").await;

    let reqs = m.reqs();
    assert_eq!(
        reqs.len(),
        5,
        "{:#?}",
        reqs.iter()
            .map(|r| r.body["messages"].as_array().map(|a| a.len()))
            .collect::<Vec<_>>()
    );
    let (main1, sub1, side, sub2, main2) = (
        &reqs[0].body,
        &reqs[1].body,
        &reqs[2].body,
        &reqs[3].body,
        &reqs[4].body,
    );

    // Identity + roles
    let sys = main1["messages"][0]["content"].as_str().unwrap();
    assert!(sys.contains("Your agent id: ol-t2"));
    let subsys = sub1["messages"][0]["content"].as_str().unwrap();
    assert!(subsys.contains("sub-agent (`explore`)") && subsys.contains("ol-t2/"));
    assert!(sub1["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "ask"));
    assert!(!sub1["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "task"));

    // First turn announces assist mode + the sub-agent list (inside the user turn, not the system prompt).
    let first_user = main1["messages"][1]["content"].as_str().unwrap();
    assert!(first_user.contains("Assist mode: DEFAULT") && first_user.contains("`explore`"));

    // Side question reuses main's exact prefix; the sub gets the answer.
    let n1 = main1["messages"].as_array().unwrap().len();
    assert_eq!(
        side["messages"].as_array().unwrap()[..n1],
        main1["messages"].as_array().unwrap()[..]
    );
    let tool_names = |body: &Value| -> Vec<String> {
        body["tools"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|t| t["function"]["name"].as_str().map(String::from))
            .collect()
    };
    assert_eq!(
        tool_names(side),
        tool_names(main1),
        "side/main tool names differ"
    );
    assert!(side["messages"]
        .to_string()
        .contains("Which folder should I search?"));
    assert!(sub2["messages"]
        .to_string()
        .contains("The main agent answered: Search src/."));

    // Main gets the sub's report as the task result, and its prefix is untouched.
    assert!(main2["messages"]
        .to_string()
        .contains("X lives in src/a.rs:3"));
    assert_eq!(
        main2["messages"].as_array().unwrap()[..n1],
        main1["messages"].as_array().unwrap()[..]
    );
    assert_eq!(main2["tools"], main1["tools"]);

    // Change assist mode between turns: announced as a new reminder, earlier turns unchanged.
    h.update_task("t2", |t| t.assist = "guide".into()).await;
    m.push(openai_ok("ok"));
    runner::send(h.clone(), "t2".into(), "next".into())
        .await
        .unwrap();
    wait_idle(&h, "t2").await;
    let main3 = &m.reqs()[5].body;
    let n2 = main2["messages"].as_array().unwrap().len();
    assert_eq!(
        main3["messages"].as_array().unwrap()[..n2],
        main2["messages"].as_array().unwrap()[..],
        "history is append-only"
    );
    assert_eq!(
        main3["messages"][0], main1["messages"][0],
        "system prompt byte-stable"
    );
    assert_eq!(main3["tools"], main1["tools"], "tool list byte-stable");
    let last = main3["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        last.contains("changed the assist mode") && last.contains("GUIDE") && last.contains("next")
    );

    // Sub-agent transcript was recorded for the UI.
    let t = h.task("t2").await.unwrap();
    let t = t.lock().await;
    assert_eq!(t.subs.len(), 1);
    assert_eq!(t.subs[0].status, "done");
    let log = t.sub_items.get(&t.subs[0].id).unwrap();
    assert!(log
        .iter()
        .any(|i| i.data["level"] == "ask" && i.data["answer"] == "Search src/."));
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

async fn wait_idle(h: &Arc<Harness>, id: &str) {
    for _ in 0..500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if !h
            .runtime(id)
            .running
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let st = h.task(id).await.unwrap().lock().await.status.clone();
            if st != "running" && st != "idle" {
                return;
            }
        }
    }
    panic!("agent never finished");
}

/// A background command is meant to outlive the run that started it — that is
/// the whole point of a dev server. So stopping the chat has to be the thing
/// that ends it: before this, `interrupt` cancelled the agent but left every
/// watcher and server running, with the chat showing "stopped" and nothing to
/// stop them but a hidden per-job button. It also has to tell the agent, so the
/// next run reruns the server instead of talking to a port that is now closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_a_chat_kills_its_background_commands() {
    let dir = std::env::temp_dir().join(format!("openleash-stopbg-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    std::fs::create_dir_all(&dir).expect("cwd for the command");
    let (h, _ev) = harness(Settings::default(), vec![task("t1", "m")]);
    let cwd = dir.to_string_lossy().to_string();

    let id =
        h.bg.spawn("t1", &format!("cd '{}' && echo up && sleep 120", cwd), &cwd)
            .expect("spawn");
    // Wait for the job to register as running before stopping anything.
    for _ in 0..100 {
        if h.bg.list("t1").iter().any(|b| b.running) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(h.bg.list("t1").iter().any(|b| b.running), "the job is up");

    // The main agent's token, the way a live run holds it.
    let cancel = CancellationToken::new();
    *h.runtime("t1").cancel.lock().unwrap() = Some(cancel.clone());
    runner::interrupt(&h, "t1").await;

    assert!(cancel.is_cancelled(), "the agent is stopped");
    for _ in 0..200 {
        if !h.bg.list("t1").iter().any(|b| b.running) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let left = h.bg.list("t1");
    assert!(
        !left.iter().any(|b| b.running),
        "no command outlives the stop: {left:?}"
    );
    let killed = &left[0];
    assert!(
        killed.exit.as_deref().unwrap_or("").contains("killed"),
        "and it is recorded as killed: {killed:?}"
    );

    // The agent is told, so the next turn starts the server again.
    let notes = h
        .runtime("t1")
        .inbox
        .lock()
        .await
        .get("main")
        .cloned()
        .unwrap_or_default();
    let note = notes.concat();
    assert!(
        note.contains("STOPPED") && note.contains("sleep 120") && note.contains(&id),
        "the agent learns what died: {note}"
    );

    // Nothing to kill twice: a second stop changes nothing and says nothing.
    let before = h
        .runtime("t1")
        .inbox
        .lock()
        .await
        .get("main")
        .map_or(0, Vec::len);
    runner::interrupt(&h, "t1").await;
    assert_eq!(
        h.runtime("t1")
            .inbox
            .lock()
            .await
            .get("main")
            .map_or(0, Vec::len),
        before,
        "a finished job is not reported as killed again"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// Answer a question/approval the agent is blocked on, the way the UI's
/// `task_respond` command does.
async fn answer(h: &Arc<Harness>, task_id: &str, item_id: &str, v: Value) {
    let tx = h
        .runtime(task_id)
        .pending
        .lock()
        .await
        .remove(item_id)
        .expect("nothing is pending");
    let _ = tx.send(v);
}

/// Wait for the question item to register its answer channel, then take its id.
async fn await_question(h: &Arc<Harness>, task_id: &str) -> String {
    for _ in 0..500 {
        if let Some(k) = h
            .runtime(task_id)
            .pending
            .lock()
            .await
            .keys()
            .next()
            .cloned()
        {
            return k;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the agent never asked its questions");
}

/// A note typed against a single-choice question reaches the agent alongside the
/// chosen label, and is stored on the item next to the answer it belongs to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_on_a_choice_question_travels_with_its_answer() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-qnote-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pn", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tn", "pn/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    // `note: true` on one question only; the others ask for no box.
    m.push(openai_call(
        "ask_user",
        json!({"questions": [
            {"question": "Which database?", "type": "single", "note": true,
             "options": [{"label": "Postgres"}, {"label": "SQLite"}]},
            {"question": "Which cache?", "type": "single", "note": "yes",
             "options": [{"label": "Redis"}, {"label": "None"}]},
            {"question": "Anything else?", "type": "single",
             "options": [{"label": "Nope"}, {"label": "Sure"}]}
        ]}),
    ));
    m.push(openai_ok("all done"));
    runner::send(h.clone(), "tn".into(), "pick something".into())
        .await
        .unwrap();
    let id = await_question(&h, "tn").await;
    answer(
        &h,
        "tn",
        &id,
        json!({
            "answers": ["Postgres", "SQLite", "Nope"],
            "notes": ["the FTS index needs it", "", ""]
        }),
    )
    .await;
    wait_idle(&h, "tn").await;

    let reqs = m.reqs();
    let last = reqs[reqs.len() - 1].body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    // A note rides on the line of the question it belongs to, not a loose trailer.
    let line = last
        .lines()
        .find(|l| l.contains("Which database?"))
        .unwrap_or_default()
        .to_string();
    assert!(
        line.contains("Postgres"),
        "the chosen label is in the result: {last}"
    );
    assert!(
        line.contains("the FTS index needs it"),
        "the note sits with its own answer: {last}"
    );

    // The item keeps both arrays, so the UI can render the note after the fact.
    let t = h.task("tn").await.unwrap();
    let t = t.lock().await;
    let q = t
        .items
        .iter()
        .find(|i| i.kind == "question")
        .expect("the question item");
    assert_eq!(q.data["answers"][0], "Postgres");
    assert_eq!(q.data["notes"][0], "the FTS index needs it");
    assert_eq!(q.data["notes"][1], "");
    // Only a real `true` turns the box on: `"yes"` is a malformed call, not a yes.
    assert_eq!(q.data["questions"][0]["note"], true);
    assert_eq!(q.data["questions"][1]["note"], false);
    assert_eq!(q.data["questions"][2]["note"], false);

    let _ = std::fs::remove_dir_all(dir);
}

/// Skipping a question is an answer: the agent is told the user declined to
/// decide, so it makes the call itself instead of reading an empty line as a hint
/// or asking the same thing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skipped_question_is_reported_as_a_refusal_to_decide() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-qskip-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("ps", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("ts", "ps/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    // Both are `required` (the default) — that must not stop the user skipping.
    m.push(openai_call("ask_user", json!({"questions": [
        {"question": "Which database?", "type": "single", "options": [{"label": "Postgres"}, {"label": "SQLite"}]},
        {"question": "Rename the crate?", "type": "single", "options": [{"label": "Yes"}, {"label": "No"}]}
    ]})));
    m.push(openai_ok("all done"));
    runner::send(h.clone(), "ts".into(), "pick something".into())
        .await
        .unwrap();
    let id = await_question(&h, "ts").await;
    answer(
        &h,
        "ts",
        &id,
        json!({"answers": ["Postgres", null], "notes": ["", ""]}),
    )
    .await;
    wait_idle(&h, "ts").await;

    let reqs = m.reqs();
    let last = reqs[reqs.len() - 1].body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    assert!(
        last.contains("(skipped)"),
        "the skipped question comes back empty: {last}"
    );
    assert!(
        last.contains("skipped 1 question"),
        "and it is named as a skip: {last}"
    );
    assert!(
        last.contains("don't want to decide"),
        "which the agent is told plainly: {last}"
    );
    // The one that was answered is untouched by any of this.
    assert!(
        last.contains("Postgres"),
        "the answered question still reads: {last}"
    );

    // The item records which ones were skipped, so the transcript can say so.
    let t = h.task("ts").await.unwrap();
    let t = t.lock().await;
    let q = t
        .items
        .iter()
        .find(|i| i.kind == "question")
        .expect("the question item");
    assert_eq!(q.data["skipped"], json!([1]));

    let _ = std::fs::remove_dir_all(dir);
}

/// A question left blank because it was optional is not a skip: the user just
/// had no preference, which is not a refusal to decide.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_optional_question_left_blank_is_not_called_a_skip() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-qblank-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pb", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tb", "pb/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call("ask_user", json!({"questions": [
        {"question": "Which cache?", "type": "single", "required": false, "options": [{"label": "Redis"}, {"label": "None"}]}
    ]})));
    m.push(openai_ok("all done"));
    runner::send(h.clone(), "tb".into(), "pick something".into())
        .await
        .unwrap();
    let id = await_question(&h, "tb").await;
    answer(&h, "tb", &id, json!({"answers": [null], "notes": [""]})).await;
    wait_idle(&h, "tb").await;

    let reqs = m.reqs();
    let last = reqs[reqs.len() - 1].body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    assert!(
        !last.contains("don't want to decide"),
        "a blank optional field is not a refusal: {last}"
    );
    // And it doesn't get labelled a skip either: it was just unanswered.
    assert!(
        !last.contains("(skipped)"),
        "an unanswered optional field isn't a skip: {last}"
    );
    assert!(last.contains("no answer"), "it reads as unanswered: {last}");

    let t = h.task("tb").await.unwrap();
    let t = t.lock().await;
    let q = t
        .items
        .iter()
        .find(|i| i.kind == "question")
        .expect("the question item");
    assert_eq!(q.data["skipped"], json!([]));

    let _ = std::fs::remove_dir_all(dir);
}

/// A question raised with `ask_nonblocking` must not stop the run: the agent carries
/// straight on to its next tool call, and the answer lands later as a note
/// rather than as the result of a waiting call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_nonblocking_does_not_block_the_run_and_its_answer_arrives_as_a_note() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-al-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pa", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("ta", "pa/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    // The agent asks without waiting, then keeps going: the very next request
    // is made with the question already asked.
    m.push(openai_call("ask_nonblocking", json!({"title": "Naming", "questions": [
        {"question": "What should the crate be called?", "type": "single", "options": [{"label": "openleash"}, {"label": "leash"}]}
    ]})));
    m.push(openai_ok("kept working"));
    runner::send(h.clone(), "ta".into(), "set it up".into())
        .await
        .unwrap();
    wait_idle(&h, "ta").await;

    // It was never waiting: the run finished on its own with nobody answering.
    let reqs = m.reqs();
    assert_eq!(
        reqs.len(),
        2,
        "the run continued past the question: {:#?}",
        reqs.iter()
            .map(|r| r.body["messages"].as_array().map(|a| a.len()))
            .collect::<Vec<_>>()
    );
    // The agent's own reply is in the response, not the next request, so the
    // proof that it carried on is the second request: it went out with the
    // question already asked and the tool result in hand.
    let second = reqs[1].body["messages"].to_string();
    assert!(
        second.contains("ask_nonblocking"),
        "the second request carries the ask_nonblocking call: {second}"
    );
    assert!(
        second.contains("without waiting"),
        "and a result that says it didn't block: {second}"
    );
    // The answer may never come, so the result has to say what to do meanwhile.
    assert!(
        second.contains("say what you assumed"),
        "the result says to carry on with its own call: {second}"
    );

    // The question is on the timeline as its own kind, unanswered, and the
    // chat never went into the `waiting` state.
    let t = h.task("ta").await.unwrap();
    let t = t.lock().await;
    let it = t
        .items
        .iter()
        .find(|i| i.kind == "asklater")
        .expect("the ask_nonblocking item");
    assert_eq!(it.data["questions"].as_array().unwrap().len(), 1);
    assert!(it.data.get("answers").is_none(), "nobody answered it yet");
    assert!(t.waiting_kind.is_none(), "the run was not parked on it");

    // Now the user answers, after the run has already finished. It must reach
    // the agent rather than being dropped on the floor.
    let id = it.id.clone();
    let qs = it.data["questions"].clone();
    drop(t);
    // The run was already over, so the answer starts a turn of its own — which
    // needs a response, or the run would sit there with nothing to say.
    m.push(openai_ok("noted"));
    runner::answer_nonblocking(
        &h,
        "ta",
        &id,
        &json!({"answers": ["leash"], "notes": [""], "skipped": []}),
    )
    .await
    .unwrap();
    wait_idle(&h, "ta").await;

    let reqs = m.reqs();
    let last = reqs[reqs.len() - 1].body["messages"].to_string();
    assert!(
        last.contains("leash"),
        "the answer reached the agent: {last}"
    );
    assert!(
        last.contains("asked earlier with ask_nonblocking"),
        "and it says which question it answers: {last}"
    );
    // The form it was written against is what got answered.
    let t = h.task("ta").await.unwrap();
    let t = t.lock().await;
    let it = t
        .items
        .iter()
        .find(|i| i.id == id)
        .expect("the ask_nonblocking item");
    assert_eq!(
        it.data["questions"], qs,
        "the questions are the ones that were asked"
    );
    assert_eq!(
        it.data["answers"],
        json!(["leash"]),
        "the answer is stored on the item"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A non-blocking question still has to be announced, or it is invisible.
///
/// The card lives above the composer rather than in the transcript, so nothing
/// on screen moves when it arrives: on a run that is streaming output there is
/// no scroll, no new row, nothing to notice. It used to emit no attention event
/// at all, which meant no desktop toast and no in-app notice — the one question
/// the user could still answer after the run finished was also the only one that
/// could not tell them it was there.
///
/// It gets its own `nonblocking` kind rather than reusing `question`, because the
/// two are opposites for the reader: `question` means the agent is frozen until
/// you answer, and wording an optional question that way is a lie the notification
/// would be repeating.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_without_waiting_still_announces_itself() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-alannounce-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pa", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tann", "pa/m");
    t.status = "idle".into();
    let (h, events) = harness(s, vec![t]);

    m.push(openai_call("ask_nonblocking", json!({"title": "Naming", "questions": [
        {"question": "What should the crate be called?", "type": "single", "options": [{"label": "openleash"}, {"label": "leash"}]}
    ]})));
    m.push(openai_ok("kept working"));
    runner::send(h.clone(), "tann".into(), "set it up".into())
        .await
        .unwrap();
    wait_idle(&h, "tann").await;

    let evs = events.lock().unwrap();
    let attention: Vec<&Value> = evs
        .iter()
        .filter(|(n, _)| n == "ol://attention")
        .map(|(_, v)| v)
        .collect();
    assert!(
        !attention.is_empty(),
        "the question announced itself: {:#?}",
        evs.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let kinds: Vec<String> = attention
        .iter()
        .map(|v| v["kind"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        kinds.iter().any(|k| k == "nonblocking"),
        "on its own kind, so the UI can word it as an offer: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "question"),
        "and never as a blocking question — the run was not waiting: {kinds:?}"
    );
    // The run is untouched by the announcement: it finished on its own. Its own
    // end-of-run `done` attention is a separate, expected event — the UI has a
    // "task finished" notification for it — so this pins that the question is
    // announced once, not that it is the only attention the run ever raised.
    assert_eq!(
        kinds.iter().filter(|k| *k == "nonblocking").count(),
        1,
        "one announcement, not one per question page: {kinds:?}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A skipped `ask_nonblocking` question is a refusal to decide, and has to read as
/// one — the same rule as the blocking form, and for the same reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skipping_an_ask_nonblocking_question_is_told_as_a_refusal() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-alskip-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("ps", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("ts2", "ps/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call("ask_nonblocking", json!({"questions": [{"question": "Which theme?", "type": "single", "options": [{"label": "Dark"}, {"label": "Light"}]}]})));
    m.push(openai_ok("working"));
    runner::send(h.clone(), "ts2".into(), "go".into())
        .await
        .unwrap();
    wait_idle(&h, "ts2").await;

    let id = h
        .task("ts2")
        .await
        .unwrap()
        .lock()
        .await
        .items
        .iter()
        .find(|i| i.kind == "asklater")
        .expect("the item")
        .id
        .clone();
    // The run was already over, so the answer starts a turn of its own — which
    // needs a response, or the run would sit there with nothing to say.
    m.push(openai_ok("noted"));
    runner::answer_nonblocking(
        &h,
        "ts2",
        &id,
        &json!({"answers": [null], "notes": [""], "skipped": [0]}),
    )
    .await
    .unwrap();
    wait_idle(&h, "ts2").await;

    let reqs = m.reqs();
    let last = reqs[reqs.len() - 1].body["messages"].to_string();
    assert!(
        last.contains("don't want to decide"),
        "a skip is their answer: {last}"
    );
    assert!(last.contains("(skipped)"), "and it is labelled one: {last}");

    let _ = std::fs::remove_dir_all(dir);
}

/// Extending one of several picked options has to arrive as an extension of
/// that option. Flattening it into a string list loses which one it was, and on
/// a multi question that is the whole point of the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_extended_option_reaches_the_agent_next_to_its_label() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-qopt-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("po", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("to", "po/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call("ask_user", json!({"questions": [
        {"question": "Which caches?", "type": "multi", "options": [{"label": "Redis"}, {"label": "In-process"}]}
    ]})));
    m.push(openai_ok("all done"));
    runner::send(h.clone(), "to".into(), "pick some".into())
        .await
        .unwrap();
    let id = await_question(&h, "to").await;
    // Only the first option was extended; the second is a bare label.
    answer(
        &h,
        "to",
        &id,
        json!({
            "answers": [[{"label": "Redis", "note": "the existing one"}, "In-process"]],
            "notes": [""]
        }),
    )
    .await;
    wait_idle(&h, "to").await;

    let reqs = m.reqs();
    let last = reqs[reqs.len() - 1].body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    assert!(
        last.contains("Redis (note: the existing one)"),
        "the extension rides with its own option: {last}"
    );
    assert!(
        last.contains("In-process"),
        "the unextended option is still there: {last}"
    );
    // A bare option must not grow an empty "(note: )" onto the line.
    assert!(
        !last.contains("In-process (note"),
        "no empty extension is invented: {last}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn parses_subscription_model_lists() {
    let codex = json!({"models": [
        {"slug": "gpt-6-sol", "display_name": "GPT-6 Sol", "context_window": 272000, "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}, {"effort": "high"}, {"effort": "xhigh"}], "visibility": "list"},
        {"slug": "internal-thing", "visibility": "hide"},
    ]});
    let c = providers::parse_codex_models(&codex);
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].id.as_str(), c[0].context, c[0].reasoning_levels.len()),
        ("codex/gpt-6-sol", 272_000, 4)
    );
    let claude = json!({"data": [{"id": "claude-opus-6-2", "display_name": "Claude Opus 6.2"}, {"id": "claude-haiku-6-2-20261001", "display_name": "Claude Haiku 6.2"}]});
    let a = providers::parse_claude_models(&claude);
    assert_eq!(a[0].id, "claude/claude-opus-6-2");
    assert_eq!(a[0].name, "Opus 6.2 (sub)");
    assert_eq!(a[0].context, 1_000_000);
    assert_eq!(a[1].reasoning_param, "thinking_budget");
    assert_eq!(a[1].context, 200_000);
    // The built-in catalog has no Claude guesses: subscription entries come from
    // the live account list instead of going stale between Anthropic releases.
    assert!(
        providers::catalog()
            .iter()
            .all(|m| !matches!(m.provider.as_str(), "claude" | "anthropic"))
    );
    providers::set_pool_models("claude", a);
    let all = providers::all_models();
    assert!(all.iter().any(|m| m.id == "claude/claude-opus-6-2"));
    assert!(!all.iter().any(|m| m.id == "claude/claude-sonnet-5"));
    providers::set_pool_models("claude", vec![]);
    // A fetched list replaces the built-in guesses for that provider only.
    providers::set_pool_models("codex", c);
    let all = providers::all_models();
    assert!(
        all.iter().any(|m| m.id == "codex/gpt-6-sol")
            && !all.iter().any(|m| m.id == "codex/gpt-6-astra")
    );
    assert!(!all.iter().any(|m| m.id == "claude/claude-sonnet-5"));
    providers::set_pool_models("codex", vec![]);
}

async fn setup_tokens_read_usage_from_headers(m: &Mock) {
    // A `claude setup-token` account: no refresh token, can't use the usage endpoint.
    let mut a = acct("st", "claude", 1);
    a.access_token = "sk-ant-oat01-test".into();
    let s = Settings {
        accounts: vec![a.clone()],
        ..Default::default()
    };
    let (h, _) = harness(s, vec![task("t1", "claude/claude-sonnet-5")]);
    let before = m.reqs().len();
    let mut ok = err(
        200,
        json!({"type": "message", "content": [{"type": "text", "text": "."}]}),
    );
    ok.headers = vec![
        (
            "anthropic-ratelimit-unified-5h-utilization".into(),
            "0.4".into(),
        ),
        (
            "anthropic-ratelimit-unified-7d-utilization".into(),
            "0.1".into(),
        ),
        (
            "anthropic-ratelimit-unified-5h-reset".into(),
            (accounts::now() + 3600).to_string(),
        ),
    ];
    m.push(ok);
    let u = accounts::fetch_usage(&h, "st")
        .await
        .expect("usage via headers");
    let w: Vec<(String, f64)> = u
        .windows
        .iter()
        .map(|w| (w.label.clone(), w.used))
        .collect();
    assert_eq!(
        w,
        vec![("5h".to_string(), 40.0), ("Week".to_string(), 10.0)]
    );
    let r = &m.reqs()[before];
    assert!(
        r.path.starts_with("/v1/messages") && r.body["max_tokens"] == 1,
        "a 1-token probe, not the usage endpoint"
    );

    // A dud token is rejected loudly (import uses the same probe), and the error sticks to the account.
    m.push(err(
        401,
        json!({"error": {"message": "invalid bearer token"}}),
    ));
    let e = accounts::fetch_usage(&h, "st").await.unwrap_err();
    assert!(e.contains("rejected") && e.contains("setup-token"), "{e}");
    let v = h.accts.views(&h.settings.read().await.accounts);
    assert!(v[0].usage.as_ref().unwrap().error.contains("rejected"));
}

/// OpenCode Go: the third account kind, and the only one that is not OAuth.
///
/// Three things have to hold together, and each fails silently on its own:
/// 1. usage reads its own endpoint — `{usage:{rolling,weekly,monthly}}`, percent
///    already 0-100, ISO resets — and lands on the account as windows;
/// 2. the model list is pulled from Go's list endpoint and registered under
///    `opencode-go/…`;
/// 3. a request carrying a Go account authenticates as `Authorization: Bearer
///    <key>` against the public gateway, NOT as a vendor OAuth session against
///    the ChatGPT/Claude backend. Number three is the one that matters: an
///    OAuth account would send the token to `accounts::codex_url()` instead, so
///    the mock would answer with the wrong endpoint's shape and the turn would
///    fail — which is exactly what this asserts against.
///
/// Its own `#[tokio::test]` rather than another scenario inside
/// `routing_end_to_end`, because it needs `MODELS_LOCK`: `set_pool_models`
/// writes a process-global registry, so this has to be serialised against the
/// other model-registry tests. `MODELS_LOCK` is a `std::sync::Mutex` and the
/// body awaits, which is only sound at the top of a test — taking it inside
/// `routing_end_to_end`, which already holds `ROUTES_LOCK` across its awaits,
/// deadlocks the runtime as soon as a third worker starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_go_account_reads_usage_models_and_sends_its_key() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-goacct-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    std::env::set_var("OPENLEASH_ZEN_BASE", &m.base);
    // The inference path reads the provider's configured base_url, which is the
    // real gateway — so point it at the mock the same way a custom provider is.
    let key = format!("sk-{}", "g".repeat(64));
    let mut a = acct("go1", "opencode-go", 1);
    a.access_token = key.clone();
    let mut s = Settings {
        accounts: vec![a],
        ..Default::default()
    };
    s.providers.insert(
        "opencode-go".into(),
        ProviderCfg {
            base_url: format!("{}/zen/go/v1", m.base),
            ..Default::default()
        },
    );
    let (h, _) = harness(s, vec![task("t1", "opencode-go/kimi-k3")]);

    // 1. Usage: the Go windows, converted from the gateway's own shape.
    let before = m.reqs().len();
    let u = accounts::fetch_usage(&h, "go1").await.expect("Go usage");
    let w: Vec<(String, f64)> = u
        .windows
        .iter()
        .map(|w| (w.label.clone(), w.used))
        .collect();
    assert_eq!(
        w,
        vec![
            ("5h".to_string(), 25.0),
            ("Week".to_string(), 40.0),
            ("Month".to_string(), 10.0)
        ],
        "Go's rolling/weekly/monthly, at its own 0-100 scale"
    );
    assert!(
        u.windows.iter().all(|w| w.resets_at > accounts::now()),
        "ISO resets parsed"
    );
    assert_eq!(u.plan, "Go");
    let r = &m.reqs()[before];
    assert!(r.path.contains("/zen/go/v1/usage"), "{}", r.path);
    assert!(
        r.headers
            .contains(&format!("authorization: bearer {}", key.to_lowercase())),
        "the key travels as a bearer"
    );

    // 2. Models pulled from Go's list and registered under its provider id.
    let n = accounts::fetch_pool_models(&h, "opencode-go")
        .await
        .expect("Go model list");
    assert_eq!(n, 3);
    let ids: Vec<String> = providers::all_models()
        .into_iter()
        .filter(|md| md.provider == "opencode-go")
        .map(|md| md.id)
        .collect();
    assert!(ids.contains(&"opencode-go/kimi-k3".to_string()), "{ids:?}");
    assert!(
        ids.contains(&"opencode-go/qwen3.7-plus".to_string()),
        "{ids:?}"
    );

    // 3. A request on a Go account goes to the gateway as a bearer key. The
    // model id picks chat/completions here; the important part is the host and
    // the credential, and that it did NOT go to the Codex backend.
    let before = m.reqs().len();
    m.push(openai_ok("go says hi"));
    let (turn, t) = run(&h, "opencode-go/kimi-k3").await.expect("Go serves");
    assert_eq!(text(&turn), "go says hi");
    assert_eq!(t.account.as_ref().unwrap().id, "go1");
    let r = &m.reqs()[before];
    assert!(
        r.headers
            .contains(&format!("authorization: bearer {}", key.to_lowercase())),
        "the account's key is the credential: {}",
        r.headers
    );
    assert!(
        r.headers.contains("x-opencode-session: ses_"),
        "session header rides every Go path, not just chat/completions"
    );
    assert_ne!(
        r.headers.contains("chatgpt-account-id:"),
        true,
        "a Go account must not be sent as a ChatGPT session"
    );

    // The Messages path is the other half of the same coin: a Go model whose
    // family speaks Anthropic (qwen3.7-plus) must reach the gateway's
    // /messages with the key — not the Claude subscription endpoint, which is
    // what an OAuth account would use. `run` resolves the model from the task,
    // so the task has to be pointed at this one first.
    h.task("t1").await.unwrap().lock().await.model = "opencode-go/qwen3.7-plus".into();
    let before = m.reqs().len();
    m.push(sse(&[
        json!({"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":500}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ]));
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        run(&h, "opencode-go/qwen3.7-plus"),
    )
    .await;
    if res.is_err() {
        let seen: Vec<String> = m.reqs()[before..].iter().map(|r| r.path.clone()).collect();
        panic!("hang on the Messages path; mock saw: {seen:?}");
    }
    let (turn, _) = res.unwrap().expect("Go anthropic path");
    assert_eq!(text(&turn), "ok");
    let r = &m.reqs()[before];
    assert!(r.path.contains("/messages"), "{}", r.path);
    assert!(
        r.headers
            .contains(&format!("authorization: bearer {}", key.to_lowercase())),
        "key bearer on the Messages path too: {}",
        r.headers
    );
    assert!(
        r.headers.contains("x-opencode-session: ses_"),
        "the session header rides the Messages path, not only chat/completions"
    );
    // The Claude prelude belongs to a Claude subscription, not to a Go account.
    assert_eq!(
        r.body["system"][0]["text"], "SYS",
        "no Claude Code prelude is injected for a Go account"
    );
    providers::set_pool_models("opencode-go", vec![]);
    std::env::remove_var("OPENLEASH_ZEN_BASE");
    let _ = std::fs::remove_dir_all(dir);
}

/// OpenCode Go models register whether its credential arrived through Accounts
/// or the pre-existing API-key provider flow. A regression here would leave
/// existing `OPENCODE_API_KEY` installs on a stale hard-coded model catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_go_key_fetches_live_models_without_an_imported_account() {
    let _models = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-go-key-models-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    std::env::set_var("OPENLEASH_ZEN_BASE", &m.base);
    let mut s = Settings::default();
    s.providers.insert(
        "opencode-go".into(),
        ProviderCfg {
            api_key: format!("sk-{}", "g".repeat(64)),
            base_url: format!("{}/zen/go/v1", m.base),
            ..Default::default()
        },
    );
    let (h, _) = harness(s, vec![]);
    let n = accounts::fetch_pool_models(&h, "opencode-go")
        .await
        .expect("key-backed Go model list");
    assert_eq!(n, 3);
    assert!(providers::all_models()
        .iter()
        .any(|m| m.id == "opencode-go/qwen3.7-plus"));
    let req = m.reqs().pop().expect("models request recorded");
    assert!(req.path.ends_with("/zen/go/v1/models"), "{}", req.path);
    assert!(req.headers.contains("authorization: bearer sk-"));
    providers::set_pool_models("opencode-go", vec![]);
    std::env::remove_var("OPENLEASH_ZEN_BASE");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn routes_pick_dedicated_over_generic_and_chain_after_the_chosen_model() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    router::set_routes(&[
        Route {
            id: "generic".into(),
            name: "Any".into(),
            all: true,
            steps: vec!["openrouter/x".into()],
            ..Default::default()
        },
        Route {
            id: "subs".into(),
            name: "Subs".into(),
            heads: vec!["codex/gpt-6-sol".into(), "claude/claude-opus-5".into()],
            steps: vec![
                "claude/claude-opus-5".into(),
                "codex/gpt-6-sol".into(),
                "zai/glm-4.6".into(),
            ],
            on_exhausted: "fail".into(),
            ..Default::default()
        },
    ]);
    // A model named as a head gets its dedicated route, even though a generic one exists (and comes first).
    assert_eq!(router::default_route("codex/gpt-6-sol"), "subs");
    assert_eq!(router::default_route("anthropic/claude-opus-5"), "generic");
    // Chosen model first, then the rest of the route, without trying the chosen one twice.
    let (chain, out) = router::steps("codex/gpt-6-sol", "subs");
    assert_eq!(
        chain,
        vec!["codex/gpt-6-sol", "claude/claude-opus-5", "zai/glm-4.6"]
    );
    assert_eq!(out, "fail");
    // Heads don't restrict a route: any model can use any route.
    assert_eq!(router::steps("openai/gpt-5", "subs").0[0], "openai/gpt-5");
    // No route: just the model, then pause.
    assert_eq!(
        router::steps("openai/gpt-5", ""),
        (vec!["openai/gpt-5".to_string()], "pause".to_string())
    );
    router::set_routes(&[]);
    assert_eq!(router::default_route("codex/gpt-6-sol"), "");
}

/// Esc (or an app close) cut a subagent off mid-job. Continuing the chat
/// continues the subagent from its saved conversation, and the main agent
/// receives the real report instead of "stopped".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopped_subagents_continue_on_resume() {
    let dir = std::env::temp_dir().join(format!("openleash-subres-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t3", "p/m");
    t.status = "stopped".into();
    t.messages = vec![
        Message::user_text("find X"),
        Message {
            role: "assistant".into(),
            content: vec![
                json!({"type":"tool_use","id":"call_t","name":"task","input":{"description":"find","prompt":"Find X","subagent_type":"explore"}}),
            ],
            model: String::new(),
        },
        Message::user(vec![
            json!({"type":"tool_result","tool_use_id":"call_t","content":"Sub-agent stopped: interrupted","is_error":true}),
        ]),
    ];
    t.subs = vec![SubInfo {
        id: "s1".into(),
        role: "explore".into(),
        task: "find".into(),
        status: "stopped".into(),
        call_id: "call_t".into(),
        item_id: "i1".into(),
        ..Default::default()
    }];
    t.sub_msgs.insert(
        "s1".into(),
        vec![
            Message::user_text("Find X (brief)"),
            Message {
                role: "assistant".into(),
                content: vec![json!({"type":"text","text":"looking in src"})],
                model: String::new(),
            },
        ],
    );
    let (h, _) = harness(s, vec![t]);

    m.push(openai_ok("X is in src/x.rs:1")); // the subagent, continued
    m.push(openai_ok("Found it: src/x.rs:1")); // the main agent
    runner::resume(&h, "t3", None).await.unwrap();
    wait_idle(&h, "t3").await;

    let reqs = m.reqs();
    assert_eq!(reqs.len(), 2);
    let sub = reqs[0].body["messages"].to_string();
    assert!(
        sub.contains("Find X (brief)")
            && sub.contains("looking in src")
            && sub.contains("You were interrupted"),
        "continues its own conversation"
    );
    let main = reqs[1].body["messages"].to_string();
    assert!(
        main.contains("X is in src/x.rs:1"),
        "main gets the real report"
    );
    assert!(
        !main.contains("Sub-agent stopped"),
        "the stopped placeholder was replaced before the main agent saw it"
    );
    let t = h.task("t3").await.unwrap();
    let t = t.lock().await;
    assert_eq!(t.subs[0].status, "done");
    assert_eq!(t.status, "done");
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

/// A picture the user pasted can be handed to a sub-agent through `task`'s
/// `images` argument: the sub-agent's own request must carry the real image,
/// not just the brief that mentions it. On the OpenAI-compatible path (every
/// non-Anthropic provider) these blocks used to be dropped on the floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_tool_hands_images_to_the_subagent() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-subimg-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pi", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("timg", "pi/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);
    const SUB: &str = "sub-agent (`explore`)";

    m.push(openai_call(
        "task",
        json!({"description": "check mockup", "prompt": "Does this match?", "subagent_type": "explore", "images": ["data:image/png;base64,QUJD"]}),
    ));
    m.push(openai_ok("Launched."));
    m.push_for(SUB, openai_ok("The header is off by 8px."));
    runner::send(h.clone(), "timg".into(), "check this mockup".into())
        .await
        .unwrap();
    wait_idle(&h, "timg").await;

    let sub = m
        .reqs()
        .into_iter()
        .find(|r| {
            r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
        })
        .expect("the subagent made a request");
    let body = sub.body.to_string();
    assert!(
        body.contains("data:image/png;base64,QUJD"),
        "the image reached the subagent"
    );
    assert!(
        body.contains("image_url"),
        "as a real image part, not a caption: {body}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// The exact bug: the chat's model was changed while a sub-agent was already
/// working, and that sub-agent carried on with the old one. Its model used to be
/// stamped on it at spawn and never re-read, so every agent a chat had already
/// launched was stuck on whatever it was launched on for the rest of the task —
/// the swap only ever reached the ones spawned afterwards.
///
/// Driven through the real `task` tool rather than a hand-built `SubInfo`, because
/// the frozen value was stamped at spawn: a test that sets up the sub-agent by hand
/// never touches the code that caused it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mid_task_model_change_reaches_the_subagents_already_running() {
    let dir = std::env::temp_dir().join(format!("openleash-submodel-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    // Two models behind one endpoint, so the swap is a model id and not a provider.
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tsw", "p/old");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);
    const SUB: &str = "sub-agent (`explore`)";

    // Turn 1: the main agent launches a sub-agent, which is still working when the
    // user swaps the chat's model.
    m.push(openai_call(
        "task",
        json!({"description": "find", "prompt": "Find X", "subagent_type": "explore"}),
    ));
    m.push(openai_ok("Launched."));
    m.push_for(SUB, openai_call("glob", json!({"pattern": "**/*"})));
    runner::send(h.clone(), "tsw".into(), "find X".into())
        .await
        .unwrap();

    // Wait for the sub-agent to exist and be running, then swap under it.
    let sid = loop {
        let arc = h.task("tsw").await.unwrap();
        let running = arc
            .lock()
            .await
            .subs
            .iter()
            .find(|s| s.status == "running")
            .map(|s| s.id.clone());
        if let Some(id) = running {
            break id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    h.update_task("tsw", |t| t.model = "p/new".into()).await;
    {
        let arc = h.task("tsw").await.unwrap();
        let g = arc.lock().await;
        let s = g.subs.iter().find(|s| s.id == sid).expect("the sub-agent");
        assert_eq!(
            s.model, "",
            "a sub-agent carries no model of its own unless one was chosen for it: \
             it follows the chat, which is what makes a swap reach the agents \
             already running"
        );
    }

    // The sub-agent finishes its tool call and asks again: that request has to go
    // to the new model, not the one it was launched on.
    m.push_for(SUB, openai_ok("X is in src/x.rs:1"));
    m.push(openai_ok("Found it: src/x.rs:1"));
    wait_idle(&h, "tsw").await;

    let subs = m
        .reqs()
        .into_iter()
        .filter(|r| {
            r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
        })
        .collect::<Vec<_>>();
    assert!(
        subs.len() >= 2,
        "the subagent asked again after the swap: {} request(s)",
        subs.len()
    );
    assert_eq!(
        subs.last().unwrap().body["model"],
        "new",
        "the request the already-running subagent made after the swap went to the new model"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A sub-agent that finds the chat was messaged while it was parked used to spin
/// forever. `RouteErr::Refresh` is only cleared for the main agent, but the
/// `steered` flag it keys on is per *task* — so a sub-agent hitting the same
/// condition re-read it, rebuilt the identical request, got Refresh again, and
/// looped without ever sending anything or releasing the slot it held. The chat
/// sat at "Working" with a live subagent that never finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subagent_parked_while_the_user_messaged_does_not_spin_forever() {
    let dir = std::env::temp_dir().join(format!("openleash-subrefresh-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "p/m");
    t.status = "idle".into();
    t.agents = vec!["explore".into()];
    let (h, _) = harness(s, vec![t]);
    const SUB: &str = "sub-agent (`explore`)";

    // Turn 1: the main agent launches a sub-agent, which parks (every model out).
    // The 429s are for the sub-agent's own request.
    m.push(openai_call(
        "task",
        json!({"description": "find", "prompt": "Find X", "subagent_type": "explore"}),
    ));
    m.push(openai_ok("Launched."));
    m.push_for(
        SUB,
        err(
            429,
            json!({"error": {"type": "usage_limit_reached", "message": "out of usage"}}),
        ),
    );
    runner::send(h.clone(), "t1".into(), "find X".into())
        .await
        .unwrap();

    // Wait for the sub-agent to exist *and* for the park to land: the pause is
    // raised when the chain is found empty, which is after the launch returns, so
    // resuming on the launch alone would be resuming a chat nobody has frozen yet.
    let mut saw_park = false;
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        if h.is_paused("t1").await {
            saw_park = true;
            break;
        }
    }
    assert!(saw_park, "the sub-agent's park landed on the chat");

    // Now the user messages the frozen chat. That is the real trigger: it sets
    // `steered` (so the frozen router abandons the request it holds) and lifts
    // the pause, in one gesture.
    m.push_for(SUB, openai_ok("X is in src/x.rs:1"));
    m.push(openai_ok("Found it: src/x.rs:1"));
    runner::send(h.clone(), "t1".into(), "also check the tests".into())
        .await
        .unwrap();

    wait_idle(&h, "t1").await;

    let t = h.task("t1").await.unwrap();
    let t = t.lock().await;
    assert_ne!(
        t.subs.first().map(|s| s.status.as_str()),
        Some("running"),
        "the sub-agent that was told to rebuild actually finished"
    );
    assert_eq!(
        t.status, "done",
        "and the chat completed rather than hanging"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Background subagent: the main agent keeps going, gets the report delivered
/// (waking it up if it had finished its turn), and the user can then message
/// that subagent directly for a follow-up that also reaches the main agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_subagents_and_direct_messages() {
    let dir = std::env::temp_dir().join(format!("openleash-bg-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t4", "p/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);
    const SUB: &str = "sub-agent (`explore`)";

    m.push(openai_call("task", json!({"description": "scan", "prompt": "Scan the repo", "subagent_type": "explore", "run_in_background": true})));
    m.push(openai_ok(
        "Scanning in the background; meanwhile I'll plan.",
    ));
    m.push(openai_ok("Got the scan results.")); // after the report arrives
    m.push_for(SUB, openai_ok("BG REPORT: 3 modules"));
    runner::send(h.clone(), "t4".into(), "scan and plan".into())
        .await
        .unwrap();

    // Wait until the main agent has seen the report (it may have gone idle first and been woken).
    let mut seen = false;
    for _ in 0..500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if m.reqs().iter().any(|r| {
            !r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
                && r.body["messages"]
                    .to_string()
                    .contains("BG REPORT: 3 modules")
        }) {
            seen = true;
            break;
        }
    }
    assert!(seen, "report delivered to the main agent");
    wait_idle(&h, "t4").await;
    let mains: Vec<Req> = m
        .reqs()
        .into_iter()
        .filter(|r| {
            !r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
        })
        .collect();
    assert!(
        mains[1].body["messages"]
            .to_string()
            .contains("in the background"),
        "task returned immediately"
    );
    let sid = h.task("t4").await.unwrap().lock().await.subs[0].id.clone();
    assert!(h.task("t4").await.unwrap().lock().await.subs[0].background);

    // Direct message to the finished subagent → it continues → follow-up reaches main.
    m.push_for(SUB, openai_ok("FOLLOW-UP: tests live in /tests"));
    m.push(openai_ok("Noted the follow-up."));
    runner::sub_message(&h, "t4", &sid, "also where are the tests?".into(), vec![])
        .await
        .unwrap();
    let mut got = false;
    for _ in 0..500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if m.reqs().iter().any(|r| {
            !r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
                && r.body["messages"]
                    .to_string()
                    .contains("FOLLOW-UP: tests live")
        }) {
            got = true;
            break;
        }
    }
    assert!(got, "follow-up delivered to the main agent");
    let subreq = m
        .reqs()
        .into_iter()
        .rfind(|r| {
            r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
        })
        .unwrap();
    let conv = subreq.body["messages"].to_string();
    assert!(
        conv.contains("BG REPORT") && conv.contains("also where are the tests?"),
        "continues its own conversation with the user's message"
    );
    wait_idle(&h, "t4").await;
    let _ = std::fs::remove_dir_all(dir);
}

/// `set_title` renames the chat from inside a real run, but only when naming
/// is on and the user hasn't named the chat themselves. Three things are checked
/// in the harness rather than trusted to the prompt, because the prompt is
/// advice and this is the rule: the setting is off, the chat is user-named, or
/// the model keeps renaming with the same words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_renames_a_chat_only_when_allowed() {
    let dir = std::env::temp_dir().join(format!("openleash-title-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pt", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);

    // 1. A chat the user has NOT named: the agent's rename lands.
    let mut open = task("topen", "pt/m");
    open.status = "idle".into();
    open.title = "fix the parser".into();
    let (h, _ev) = harness(s.clone(), vec![open]);
    m.push(openai_call(
        "set_title",
        json!({"title": "Rewrite the tokenizer"}),
    ));
    m.push(openai_ok("Renamed."));
    runner::send(h.clone(), "topen".into(), "fix the parser".into())
        .await
        .unwrap();
    wait_idle(&h, "topen").await;
    let t = h.task("topen").await.unwrap();
    let t = t.lock().await;
    assert_eq!(t.title, "Rewrite the tokenizer", "the agent named the chat");
    assert!(!t.titled, "an agent rename is not a user claim on the name");
    drop(t);

    // 2. A chat the user renamed: refused, and the title is untouched.
    let mut named = task("tnamed", "pt/m");
    named.status = "idle".into();
    named.title = "My name for this".into();
    named.titled = true;
    let (h2, _ev2) = harness(s.clone(), vec![named]);
    m.push(openai_call("set_title", json!({"title": "Something else"})));
    m.push(openai_ok("Understood, leaving it alone."));
    runner::send(h2.clone(), "tnamed".into(), "carry on".into())
        .await
        .unwrap();
    wait_idle(&h2, "tnamed").await;
    let t2 = h2.task("tnamed").await.unwrap();
    let t2 = t2.lock().await;
    assert_eq!(
        t2.title, "My name for this",
        "a user-named chat is never renamed"
    );
    drop(t2);

    // 3. The setting off: refused even though the chat was never user-named.
    let mut s_off = s.clone();
    s_off.agent_titles = false;
    let mut off = task("toff", "pt/m");
    off.status = "idle".into();
    off.title = "untouched".into();
    let (h3, _ev3) = harness(s_off, vec![off]);
    m.push(openai_call(
        "set_title",
        json!({"title": "Should not apply"}),
    ));
    m.push(openai_ok("Fine."));
    runner::send(h3.clone(), "toff".into(), "carry on".into())
        .await
        .unwrap();
    wait_idle(&h3, "toff").await;
    let t3 = h3.task("toff").await.unwrap();
    let t3 = t3.lock().await;
    assert_eq!(t3.title, "untouched", "naming can still be turned off");

    let _ = std::fs::remove_dir_all(dir);
}

/// The user broadcasts one message to every working agent: the main agent and
/// every sub-agent, nested ones included, each carrying the rule that says to
/// ignore it unless it's about their own work. The main agent takes it as its
/// own turn so it can act on it at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broadcast_reaches_every_agent_with_the_ignore_rule() {
    let dir = std::env::temp_dir().join(format!("openleash-cast-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tc", "p/m");
    t.status = "idle".into();
    t.agents = vec!["explore".into(), "general".into()];
    // One direct sub-agent and one nested under it, both working right now.
    let sub = |id: &str, role: &str, parent: &str, depth: u8| SubInfo {
        id: id.into(),
        role: role.into(),
        task: format!("work on {id}"),
        status: "running".into(),
        call_id: format!("call_{id}"),
        item_id: format!("item_{id}"),
        parent: parent.into(),
        depth,
        ..Default::default()
    };
    t.subs = vec![sub("w1", "explore", "", 1), sub("w2", "general", "w1", 2)];
    let (h, _) = harness(s, vec![t]);

    m.push(openai_ok("Done.")); // the main agent's broadcast turn
    let to = runner::broadcast(&h, "tc", "the lighting is bad, undo it")
        .await
        .unwrap();
    assert_eq!(
        to,
        vec!["main", "w1", "w2"],
        "reaches the nested worker too"
    );
    wait_idle(&h, "tc").await;

    // Every recipient has it queued, with the rule that says to ignore it when
    // it isn't about their work, and without the tag `wait` reads.
    let rt = h.runtime("tc");
    let inbox = rt.inbox.lock().await;
    for id in ["w1", "w2"] {
        let got = inbox.get(id).cloned().unwrap_or_default().join("\n");
        assert!(
            got.contains("the lighting is bad, undo it"),
            "{id} got the message"
        );
        assert!(
            got.contains("ignore it completely"),
            "{id} got the rule to ignore it when it doesn't apply"
        );
        assert!(
            got.starts_with("<kind:user-msg>"),
            "{id}'s copy is tagged as coming from the user"
        );
    }
    drop(inbox);

    // The main agent's own turn is the broadcast, framed, and it can act now.
    let reqs = m.reqs();
    let turn = reqs.last().unwrap().body["messages"].to_string();
    assert!(
        turn.contains("the lighting is bad, undo it"),
        "the main agent's turn is the broadcast"
    );
    assert!(
        turn.contains("ignore it completely"),
        "the main agent is held to the same rule"
    );
    assert!(
        !turn.contains("<kind:"),
        "the main agent never sees the routing tag"
    );

    let t = h.task("tc").await.unwrap();
    let t = t.lock().await;
    assert!(
        t.items
            .iter()
            .any(|i| i.kind == "notice" && i.data["global"] == true),
        "the timeline shows what you broadcast"
    );
    assert!(
        t.sub_items["w1"].iter().any(|i| i.data["global"] == true)
            && t.sub_items["w2"].iter().any(|i| i.data["global"] == true),
        "each working agent's panel shows it"
    );
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

/// Ultrathread: a sub-agent gets the task tool and launches its own sub-agent;
/// the nested one sits at the deepest level and can't spawn further. The main
/// agent is told it's in ultrathread and isn't allowed to stop without a plan.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ultrathread_nests_subagents() {
    let dir = std::env::temp_dir().join(format!("openleash-ultra-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tu", "p/m");
    t.status = "idle".into();
    t.ultra = true;
    t.agents = vec!["explore".into(), "general".into()];
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call("task", json!({"description": "coordinate", "prompt": "Split and cover area A", "subagent_type": "general"}))); // main 1
    m.push(openai_call(
        "task",
        json!({"description": "leaf", "prompt": "Scan A/1", "subagent_type": "explore"}),
    )); // coordinator 1
    m.push(openai_ok("A/1 scanned")); // leaf report
    m.push(openai_ok("Area A covered")); // coordinator report
    m.push(openai_ok("Done.")); // main 2: tries to stop without a todo list
    m.push(openai_ok("Done, verified.")); // main 3: after the ultrathread nudge
    runner::send(h.clone(), "tu".into(), "attack everything".into())
        .await
        .unwrap();
    wait_idle(&h, "tu").await;

    let reqs = m.reqs();
    assert_eq!(reqs.len(), 6);
    let names = |b: &Value| {
        b["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str().map(String::from))
            .collect::<Vec<_>>()
    };
    assert!(
        reqs[0].body["messages"][1]["content"]
            .as_str()
            .unwrap_or("")
            .contains("ULTRATHREAD is ON")
            || reqs[0].body.to_string().contains("ULTRATHREAD is ON")
    );
    assert!(
        names(&reqs[1].body).contains(&"task".to_string()),
        "coordinator can spawn"
    );
    assert!(
        !names(&reqs[2].body).contains(&"task".to_string()),
        "deepest level can't spawn"
    );
    assert!(
        reqs[5].body.to_string().contains("ULTRATHREAD check 1/"),
        "stop guard nudged"
    );

    let t = h.task("tu").await.unwrap();
    let t = t.lock().await;
    assert_eq!(t.subs.len(), 2);
    let coord = t.subs.iter().find(|x| x.role == "general").unwrap();
    let leaf = t.subs.iter().find(|x| x.role == "explore").unwrap();
    assert_eq!((coord.depth, coord.parent.as_str()), (1, ""));
    assert_eq!((leaf.depth, leaf.parent.as_str()), (2, coord.id.as_str()));
    // The leaf's pill lives in the coordinator's transcript, not the main timeline.
    assert!(t.sub_items[&coord.id]
        .iter()
        .any(|i| i.kind == "sub" && i.data["sub_id"] == leaf.id.as_str()));
    assert!(!t
        .items
        .iter()
        .any(|i| i.kind == "sub" && i.data["sub_id"] == leaf.id.as_str()));
}

/// Plan mode's stop guard: an agent that explores and then just writes prose has
/// not done what the user asked, because nothing is waiting for approval. It is
/// sent back to propose a plan — and once it proposes one, the guard goes quiet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_mode_nudges_an_agent_that_never_proposes() {
    let dir = std::env::temp_dir().join(format!("openleash-plan-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tp", "p/m");
    t.status = "idle".into();
    t.plan = true;
    let (h, _) = harness(s, vec![t]);

    m.push(openai_ok("Here's what I'd do: ...")); // ends the turn with prose only
    m.push(openai_ok("Still just describing it.")); // nudged once, prose again
    m.push(openai_call(
        "exit_plan_mode",
        json!({"plan": "Edit src/x.rs and add a test."}),
    )); // finally proposes
    m.push(openai_call("read_file", json!({"path": "src/x.rs"}))); // approved: off it goes
    m.push(openai_ok("Done."));
    runner::send(h.clone(), "tp".into(), "make plan mode useful".into())
        .await
        .unwrap();

    // The plan card is waiting on the user, which is the whole point.
    let item = await_question(&h, "tp").await;
    let kind = {
        let t = h.task("tp").await.unwrap();
        let t = t.lock().await;
        t.items
            .iter()
            .find(|i| i.id == item)
            .map(|i| i.data["kind"].clone())
    };
    assert_eq!(
        kind,
        Some(json!("plan")),
        "the plan is put to the user as an approval card"
    );

    answer(&h, "tp", &item, json!({"decision": "once"})).await;
    wait_idle(&h, "tp").await;

    // Every request that could still be holding an unapproved plan.
    let bodies: Vec<String> = m.reqs().iter().map(|r| r.body.to_string()).collect();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("Plan mode is still active")),
        "the prose turn was nudged"
    );
    let after = m
        .reqs()
        .iter()
        .skip_while(|r| !r.body.to_string().contains("exit_plan_mode"))
        .count();
    assert!(
        !m.reqs()[after..]
            .iter()
            .any(|r| r.body.to_string().contains("Plan mode is still active")),
        "the guard goes quiet once a plan is on the table"
    );
    assert!(
        !h.task("tp").await.unwrap().lock().await.plan,
        "approving turned plan mode off"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Plan mode and ultrathread are both stop guards, and they disagree: ultrathread
/// says "the todo list is open, keep going", which an agent in plan mode is
/// forbidden from acting on. Plan mode has to win, or the run ends in a loop of
/// refused edits instead of a plan.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_mode_wins_over_the_ultrathread_stop_guard() {
    let dir = std::env::temp_dir().join(format!("openleash-planultra-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tpu", "p/m");
    t.status = "idle".into();
    t.plan = true;
    t.ultra = true;
    t.agents = vec!["explore".into(), "general".into()];
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call(
        "todo_write",
        json!({"todos": [{"content": "edit the parser", "status": "pending"}]}),
    ));
    m.push(openai_ok("I'll start on the parser.")); // ends with the todo list open
    m.push(openai_call(
        "exit_plan_mode",
        json!({"plan": "Rewrite the parser, then run the tests."}),
    ));
    m.push(openai_ok("Reworking the plan.")); // rejected, still in plan mode
    m.push(openai_ok("Still planning.")); // plan guard fires again (2/3)
    m.push(openai_ok("Out of ideas.")); // plan guard fires again (3/3), then the cap stops it
    m.push(openai_ok("Giving up on the plan."));
    runner::send(h.clone(), "tpu".into(), "do a big job".into())
        .await
        .unwrap();

    let item = await_question(&h, "tpu").await;
    answer(&h, "tpu", &item, json!({"decision": "deny"})).await;
    wait_idle(&h, "tpu").await;

    let bodies: Vec<String> = m.reqs().iter().map(|r| r.body.to_string()).collect();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("Plan mode is still active")),
        "plan mode nudged instead of letting the run end"
    );
    assert!(
        !bodies.iter().any(|b| b.contains("ULTRATHREAD check")),
        "the ultrathread guard must never fire in plan mode"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A sub-agent in plan mode can't call `exit_plan_mode` (it's main-only) and its
/// writes are refused, so it has to be told to investigate and report rather than
/// discovering the wall by hitting it.
/// A file-byte read goes through the same deny tier as an edit/command. Before the
/// runner called `gate` for `read_file`, `.env` was correctly classified by the
/// pure policy but the dispatcher never consulted it and sent the secret to the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn env_read_is_denied_before_file_contents_reach_the_model() {
    let dir = std::env::temp_dir().join(format!("openleash-env-read-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let project = dir.join("repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".env"), "API_SECRET=must-not-leak\n").unwrap();
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("env-read", "p/m");
    t.status = "idle".into();
    t.cwd = project.to_string_lossy().to_string();
    t.project = t.cwd.clone();
    let (h, _) = harness(s, vec![t]);
    m.push(openai_call("read_file", json!({"path": ".env"})));
    m.push(openai_ok("I was denied the env-file read."));
    runner::send(
        h.clone(),
        "env-read".into(),
        "Read the environment file".into(),
    )
    .await
    .unwrap();
    wait_idle(&h, "env-read").await;
    let reqs = m.reqs();
    assert_eq!(
        reqs.len(),
        2,
        "the second response handles the denial without exposing bytes"
    );
    assert!(!reqs
        .iter()
        .any(|r| r.body.to_string().contains("must-not-leak")));
    let denied = reqs[1].body.to_string();
    assert!(
        denied.contains("secret files") || denied.contains("credential"),
        "{denied}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subagent_in_plan_mode_is_told_to_stay_read_only() {
    let dir = std::env::temp_dir().join(format!("openleash-plansub-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tps", "p/m");
    t.status = "idle".into();
    t.plan = true;
    t.agents = vec!["explore".into(), "general".into()];
    let (h, _) = harness(s, vec![t]);
    const SUB: &str = "sub-agent (`explore`)";

    m.push(openai_call("task", json!({"description": "survey", "prompt": "Find the plan-mode plumbing", "subagent_type": "explore"})));
    m.push_for(SUB, openai_ok("permissions.rs and runner.rs handle it."));
    m.push(openai_call(
        "exit_plan_mode",
        json!({"plan": "Gate writes in permissions.rs, nudge in runner.rs."}),
    ));
    m.push(openai_call(
        "read_file",
        json!({"path": "src-tauri/src/agent/permissions.rs"}),
    )); // approved: off it goes
    m.push(openai_ok("Read it."));
    runner::send(h.clone(), "tps".into(), "plan this".into())
        .await
        .unwrap();

    let item = await_question(&h, "tps").await;
    answer(&h, "tps", &item, json!({"decision": "once"})).await;
    wait_idle(&h, "tps").await;

    let sub = m
        .reqs()
        .into_iter()
        .find(|r| {
            r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains(SUB)
        })
        .expect("the subagent made a request");
    assert!(
        sub.body.to_string().contains("PLAN MODE"),
        "the subagent was told it is in plan mode"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A mid-turn reasoning-effort change must reach the very next request, and the
/// model must never be told about it (unlike assist mode / the agent list, which
/// are announced as <system-reminder> blocks).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn effort_change_lands_on_the_next_request_without_telling_the_agent() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-effort-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pe", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    // A custom model with reasoning levels, so the effort value is visible on the wire.
    s.model_configs.push(providers::ModelInfo {
        id: "pe/m".into(),
        name: "Pe M".into(),
        provider: "pe".into(),
        context: 128_000,
        output: 8_000,
        input_price: 0.0,
        output_price: 0.0,
        effort: true,
        input_types: vec!["text".into()],
        capabilities: vec![],
        reasoning_levels: vec!["low".into(), "medium".into(), "high".into()],
        reasoning_param: "reasoning_effort".into(),
        custom: true,
        enabled: true,
    });
    let mut t = task("te", "pe/m");
    t.status = "idle".into();
    t.effort = 2;
    let (h, _) = harness(s, vec![t]);
    providers::set_custom_models(&h.settings.read().await.model_configs);

    // Turn 1 asks a question, which blocks the run until it is answered: a
    // deterministic point to change the effort while the turn is still open.
    m.push(openai_call(
        "ask_user",
        json!({"questions": [{"question": "Which folder?"}]}),
    ));
    m.push(openai_ok("all done"));
    runner::send(h.clone(), "te".into(), "look around".into())
        .await
        .unwrap();

    // Wait until the question is on screen and its answer channel registered,
    // then change the effort, before the agent can build request 2.
    let item = await_question(&h, "te").await;
    h.update_task("te", |t| t.effort = 4).await;
    answer(&h, "te", &item, json!({"answers": ["src"]})).await;
    wait_idle(&h, "te").await;

    let reqs = m.reqs();
    assert_eq!(
        reqs.len(),
        2,
        "one question round-trip, then the final turn"
    );
    // Effort 2 maps to "medium" on the first request, 4 to "low" on the next.
    assert_eq!(
        reqs[0].body["reasoning_effort"], "medium",
        "first request used the starting effort"
    );
    assert_eq!(
        reqs[1].body["reasoning_effort"], "low",
        "the new effort reached the very next request"
    );
    // Nothing about the change is announced to the model: the second request only
    // adds the answer to the question. (The system prompt legitimately says
    // "effort" and "<system-reminder>", so check the delta, not the whole body.)
    let first = reqs[0].body["messages"].as_array().unwrap();
    let second = reqs[1].body["messages"].as_array().unwrap();
    assert!(
        second.len() > first.len(),
        "the answer reached the next request"
    );
    let added = Value::Array(second[first.len()..].to_vec())
        .to_string()
        .to_lowercase();
    for word in ["effort", "reasoning", "thinking", "system-reminder"] {
        assert!(
            !added.contains(word),
            "the agent was told about the effort change ({word}): {added}"
        );
    }
    // History stays append-only, so the cached prefix still hits.
    assert_eq!(second[..first.len()], first[..], "history is append-only");

    let _ = std::fs::remove_dir_all(dir);
}
/// A sub-agent that already reported is resumable in place: the follow-up runs
/// in its own conversation (its brief and its earlier findings are both still
/// there) and the new report comes back to the main agent, which never had to
/// re-brief anyone. A sub-agent that is still running is not resumable — send
/// it more work instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_resume_continues_a_finished_subagent_in_place() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-subresume-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pr", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tr", "pr/m");
    t.status = "idle".into();
    t.subs = vec![SubInfo {
        id: "s1".into(),
        role: "explore".into(),
        task: "find X".into(),
        status: "done".into(),
        report: "X is in src/x.rs".into(),
        call_id: "call_0".into(),
        item_id: "i1".into(),
        ..Default::default()
    }];
    t.sub_msgs.insert(
        "s1".into(),
        vec![
            Message::user_text("Find X (brief)"),
            Message {
                role: "assistant".into(),
                content: vec![json!({"type":"text","text":"looking in src"})],
                model: String::new(),
            },
        ],
    );
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call(
        "task_resume",
        json!({"id": "s1", "message": "now check the tests"}),
    )); // main
    m.push(openai_ok("src/x_test.rs covers it")); // the subagent, continued
    m.push(openai_ok("All covered."));
    runner::send(h.clone(), "tr".into(), "follow up on X".into())
        .await
        .unwrap();
    wait_idle(&h, "tr").await;

    let reqs = m.reqs();
    let sub = reqs
        .iter()
        .find(|r| {
            r.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .contains("sub-agent (`explore`)")
        })
        .expect("the subagent was resumed");
    let msgs = sub.body["messages"].to_string();
    assert!(
        msgs.contains("Find X (brief)") && msgs.contains("looking in src"),
        "it kept its own conversation, not a fresh context"
    );
    assert!(
        msgs.contains("now check the tests"),
        "the follow-up landed as a real turn"
    );
    let names = |b: &Value| {
        b["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str().map(String::from))
            .collect::<Vec<_>>()
    };
    assert!(
        !names(&sub.body).contains(&"task_resume".to_string()),
        "a subagent at the bottom can't resume anything"
    );
    let main = reqs.last().unwrap().body["messages"].to_string();
    assert!(
        main.contains("src/x_test.rs covers it"),
        "the main agent got the new report: {main}"
    );

    let t = h.task("tr").await.unwrap();
    let t = t.lock().await;
    assert_eq!(t.subs[0].status, "done");
    assert!(
        t.subs[0].report.contains("src/x_test.rs"),
        "the pill shows the new report"
    );
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

/// Resuming a sub-agent that is already running is refused, with a pointer to
/// the tool that does apply — and it must not burn a request doing so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_resume_refuses_a_running_subagent() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-subresume2-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pr2", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tr2", "pr2/m");
    t.status = "idle".into();
    t.subs = vec![SubInfo {
        id: "s1".into(),
        role: "general".into(),
        task: "long job".into(),
        status: "running".into(),
        call_id: "call_0".into(),
        item_id: "i1".into(),
        ..Default::default()
    }];
    t.sub_msgs
        .insert("s1".into(), vec![Message::user_text("Do the long job")]);
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call(
        "task_resume",
        json!({"id": "s1", "message": "also do this"}),
    )); // main
    m.push(openai_ok("Ok.")); // main reads the error and stops
    runner::send(h.clone(), "tr2".into(), "poke s1".into())
        .await
        .unwrap();
    wait_idle(&h, "tr2").await;

    let reqs = m.reqs();
    assert_eq!(
        reqs.len(),
        2,
        "the refusal came back as a tool result, without running the subagent again"
    );
    assert!(
        reqs[1].body["messages"]
            .to_string()
            .contains("already running"),
        "the error points at the reason: {}",
        reqs[1].body["messages"]
    );
    assert!(
        reqs[1].body["messages"]
            .to_string()
            .contains("send_message"),
        "and names the tool that does apply"
    );

    let t = h.task("tr2").await.unwrap();
    let t = t.lock().await;
    // Still no follow-up message, and it never ran again.
    assert_eq!(t.subs[0].report, "", "the running subagent was left alone");
    assert_eq!(t.sub_msgs["s1"].len(), 1, "its conversation wasn't touched");
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

/// In a nested (ultrathread) tree, a coordinator resumes its own child. It must
/// not be able to reach a sibling or the main agent's children, and the child's
/// In a nested tree a coordinator resumes its own child and is refused the
/// ones that aren't its children. The child keeps its context across the
/// resume, and its pill is updated in the coordinator's transcript, not the
/// main timeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_resume_is_scoped_to_your_own_children() {
    let _m = MODELS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-subresume3-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("pr3", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("tr3", "pr3/m");
    t.status = "idle".into();
    t.ultra = true;
    t.agents = vec!["explore".into(), "general".into()];
    // The child's pill, as the coordinator's transcript holds it. Its id is the
    // one the sub-agent's `item_id` points at, exactly as a real launch does.
    let pill = Item::new(
        "sub",
        "scan A",
        json!({"sub_id": "leaf", "role": "explore", "status": "done", "report": "A/1 scanned", "parent": "c1", "depth": 2}),
    );
    t.subs = vec![
        // The coordinator, and the child it owns.
        SubInfo {
            id: "c1".into(),
            role: "general".into(),
            task: "coordinate A".into(),
            status: "stopped".into(),
            depth: 1,
            call_id: "call_c".into(),
            item_id: "i_c".into(),
            ..Default::default()
        },
        SubInfo {
            id: "leaf".into(),
            role: "explore".into(),
            task: "scan A".into(),
            status: "done".into(),
            report: "A/1 scanned".into(),
            parent: "c1".into(),
            depth: 2,
            call_id: "call_l".into(),
            item_id: pill.id.clone(),
            ..Default::default()
        },
        // The main agent's own child: not the coordinator's to touch.
        SubInfo {
            id: "other".into(),
            role: "explore".into(),
            task: "scan B".into(),
            status: "done".into(),
            report: "B/1 scanned".into(),
            depth: 1,
            call_id: "call_o".into(),
            item_id: "i_o".into(),
            ..Default::default()
        },
    ];
    t.sub_msgs.insert(
        "c1".into(),
        vec![
            Message::user_text("Cover area A (brief)"),
            Message {
                role: "assistant".into(),
                content: vec![json!({"type":"text","text":"splitting A"})],
                model: String::new(),
            },
        ],
    );
    t.sub_msgs.insert(
        "leaf".into(),
        vec![
            Message::user_text("Scan A/1 (brief)"),
            Message {
                role: "assistant".into(),
                content: vec![json!({"type":"text","text":"A/1 scanned"})],
                model: String::new(),
            },
        ],
    );
    t.sub_msgs
        .insert("other".into(), vec![Message::user_text("Scan B/1 (brief)")]);
    t.sub_items.insert("c1".into(), vec![pill.clone()]);
    let (h, _) = harness(s, vec![t]);

    // Foreground calls, so the run is strictly sequential.
    m.push(openai_call(
        "task_resume",
        json!({"id": "leaf", "message": "now go deeper"}),
    )); // coordinator
    m.push(openai_ok("A/2 scanned")); // the leaf, continued
    m.push(openai_call("task_resume", json!({"id": "other"}))); // not the coordinator's child
    m.push(openai_ok("Area A covered")); // coordinator's report
    runner::sub_message(&h, "tr3", "c1", "keep going".into(), vec![])
        .await
        .unwrap();
    for _ in 0..500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if h.task("tr3")
            .await
            .unwrap()
            .lock()
            .await
            .subs
            .iter()
            .find(|x| x.id == "c1")
            .is_none_or(|x| x.status != "running")
        {
            break;
        }
    }

    let reqs = m.reqs();
    let names = |b: &Value| {
        b["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str().map(String::from))
            .collect::<Vec<_>>()
    };
    let sys = |i: usize| {
        reqs[i].body["messages"][0]["content"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    assert!(sys(0).contains("ol-tr3/c1"), "request 0 is the coordinator");
    assert!(
        names(&reqs[0].body).contains(&"task_resume".to_string()),
        "a coordinator can resume"
    );
    assert!(
        sys(1).contains("ol-tr3/leaf"),
        "request 1 is the leaf, continued"
    );
    assert!(
        !names(&reqs[1].body).contains(&"task_resume".to_string()),
        "the leaf at the bottom can't resume anything"
    );
    let leaf_msgs = reqs[1].body["messages"].to_string();
    assert!(
        leaf_msgs.contains("Scan A/1 (brief)") && leaf_msgs.contains("now go deeper"),
        "the leaf kept its context and got the follow-up"
    );

    let t = h.task("tr3").await.unwrap();
    let t = t.lock().await;
    assert_eq!(
        t.subs.iter().find(|x| x.id == "leaf").unwrap().report,
        "A/2 scanned",
        "the child finished its follow-up"
    );
    assert_eq!(
        t.subs.iter().find(|x| x.id == "other").unwrap().report,
        "B/1 scanned",
        "someone else's child was untouched"
    );
    assert_eq!(t.subs.iter().find(|x| x.id == "c1").unwrap().status, "done");
    // The child's pill lives in the coordinator's transcript, not the main timeline.
    assert!(
        t.sub_items["c1"].iter().any(|i| i.kind == "sub"
            && i.data["status"] == "done"
            && i.data["report"]
                .as_str()
                .unwrap_or("")
                .contains("A/2 scanned")),
        "the nested pill carries the new report"
    );
    // The coordinator was told why it couldn't touch the other child.
    let coord_msgs = Value::Array(
        t.sub_msgs["c1"]
            .iter()
            .flat_map(|m| m.content.clone())
            .collect(),
    )
    .to_string();
    assert!(
        coord_msgs.contains("isn't one of your subagents"),
        "the refusal came back to the coordinator: {coord_msgs}"
    );
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

// ───────────────────────── what the chat list sorts on ─────────────────────────

/// The chat list sorts on when the *user* last did something in a chat. An
/// agent working on its own must not move that chat up the list, or a busy
/// background run would keep shoving the chat you just opened back down it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_users_actions_move_a_chat_up_the_list() {
    let dir = std::env::temp_dir().join(format!("openleash-touch-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("toucher", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "toucher/m");
    t.status = "idle".into();
    t.messages.clear();
    let (h, _) = harness(s, vec![t]);

    // A whole agent turn: the task is written to, summarised and streamed many
    // times over. None of that is the user, and none of it may reorder the list.
    m.push(openai_ok("Working on it."));
    runner::send(h.clone(), "t1".into(), "do the thing".into())
        .await
        .unwrap();
    wait_idle(&h, "t1").await;

    // (The message that started it was the user's, so `send` moved the chat.
    // Measure from just after, across the turn the agent then ran.)
    let arc = h.task("t1").await.unwrap();
    let before = arc.lock().await.touched_at;
    // Exactly what every streamed step does to a task, repeated.
    for _ in 0..5 {
        h.update_task("t1", |t| t.step = "Working".into()).await;
    }
    let after = arc.lock().await;
    assert!(after.updated_at > before, "the agent did change the chat");
    assert_eq!(
        after.touched_at, before,
        "but that must not reorder the chat list"
    );
    drop(after);

    // The user pausing is them acting, so it counts.
    let before = arc.lock().await.touched_at;
    h.pause_task("t1", "manual", "Paused by you").await;
    assert!(
        arc.lock().await.touched_at > before,
        "pausing by hand is a user action"
    );

    // A pause the app raised by itself is the agent's news, not the user's.
    let before = arc.lock().await.touched_at;
    h.pause_task("t1", "exhausted", "Every model in the chain failed")
        .await;
    assert_eq!(
        arc.lock().await.touched_at,
        before,
        "an exhausted pause is not a user action"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// ───────────────────────── what quitting does to the chats ─────────────────────────

/// What the tray's Quit leaves behind: every chat frozen where it stood, its
/// in-flight commands cut, and a note owed to the agent about the calls that were
/// cut — *without* any run being cancelled. Cancelling is what makes a chat read as
/// stopped, and stopped is not what a force pause is for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_pause_all_freezes_chats_and_owes_the_agent_a_rerun() {
    let dir = std::env::temp_dir().join(format!("openleash-quit-{}", std::process::id()));
    let _home = super::store::test_home(&dir);

    let mut busy = task("busy", "m1");
    busy.messages = vec![
        Message::user_text("build it"),
        Message {
            role: "assistant".into(),
            content: vec![
                json!({"type":"tool_use","id":"b1","name":"bash","input":{"command":"cargo build"}}),
            ],
            model: String::new(),
        },
    ];
    busy.items = vec![Item::new(
        "tool",
        "Running cargo build",
        json!({"name": "bash", "status": "running", "tool_use_id": "b1"}),
    )];
    // A sub-agent mid-flight too, so the swarm case is covered by the same sweep.
    busy.subs = vec![SubInfo {
        id: "s1".into(),
        role: "explore".into(),
        task: "scan".into(),
        status: "running".into(),
        depth: 1,
        call_id: "c1".into(),
        item_id: "i1".into(),
        ..Default::default()
    }];
    busy.sub_msgs.insert("s1".into(), vec![Message::user_text("scan"), Message { role: "assistant".into(), content: vec![json!({"type":"tool_use","id":"s1a","name":"bash","input":{"command":"rg -n x"}})], model: String::new() }]);

    let mut idle = task("idle", "m1");
    idle.status = "idle".into();
    let mut archived = task("archived", "m1");
    archived.archived = true;

    let (h, ev) = harness(Settings::default(), vec![busy, idle, archived]);
    // A real command, so the sweep has something in flight to cut: `fg` is what the
    // runner uses for a foreground shell, and it is what the sweep keys on.
    let bg = h.bg.spawn("busy", "echo hi", ".").unwrap();
    h.runtime("busy")
        .fg
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(
        h.force_pause_all().await,
        1,
        "one chat had work cut; the archived one is left alone"
    );

    let s = h.settings.read().await;
    assert!(
        s.paused_all,
        "a clean quit leaves the freeze on, so nothing restarts by itself"
    );
    assert!(!s.paused_reason.is_empty(), "and says why");
    drop(s);
    assert!(
        ev.lock().unwrap().iter().any(|(k, _)| k == "ol://settings"),
        "the UI is told the pause moved"
    );

    let busy = h.task("busy").await.unwrap();
    let b = busy.lock().await;
    // Paused, not stopped: still "running", still holding its own conversation.
    assert_eq!(
        b.paused.as_ref().map(|p| p.kind.clone()),
        Some("quit".into()),
        "the chat is frozen by the quit, not by a user pause: {:?}",
        b.paused
    );
    assert_eq!(
        b.status, "running",
        "a force pause never ends a run, so the chat is not stopped"
    );
    assert!(
        b.stop_note.as_deref().unwrap_or("").contains("cargo build"),
        "the agent is told what it lost: {:?}",
        b.stop_note
    );
    // Every tool_use now has its tool_result, or the next request is rejected.
    let last = b.messages.last().unwrap();
    assert_eq!(last.role, "user");
    assert_eq!(last.content[0]["tool_use_id"], "b1");
    assert_eq!(last.content[0]["is_error"], json!(true));
    assert!(
        last.content[0]["content"]
            .as_str()
            .unwrap()
            .contains("Run it again"),
        "…and told to run it again"
    );
    assert_eq!(
        b.items[0].data["status"],
        json!("error"),
        "the spinner is closed, not left turning"
    );
    assert_eq!(b.items[0].data["meta"], json!("cut off by quit"));
    assert_eq!(
        b.subs[0].status, "stopped",
        "a sub-agent is stopped as well"
    );
    let sub = b.sub_msgs.get("s1").unwrap();
    assert_eq!(
        sub.last().unwrap().content[0]["tool_use_id"],
        "s1a",
        "and its cut call is closed too"
    );
    assert!(
        b.bg_live.iter().any(|l| l.contains(&bg)),
        "the background command is recorded for the next session: {:?}",
        b.bg_live
    );
    drop(b);
    assert_eq!(
        busy.lock().await.busy,
        2,
        "and still counted as in flight, because its kill has not landed yet"
    );

    // A chat with nothing in flight gets no per-chat pause of its own: the global
    // one is what holds it, and a pause reason on every chat that was merely idle
    // would fill the paused list with work that was never stopped.
    let idle = h.task("idle").await.unwrap();
    let i = idle.lock().await;
    assert!(
        i.paused.is_none(),
        "an idle chat is not dressed up as stopped: {:?}",
        i.paused
    );
    assert!(
        i.stop_note.is_none(),
        "nothing was cut, so there is nothing to say about it"
    );
    assert!(i.messages.is_empty(), "and a no-op history is left alone");
    drop(i);

    // Archived: not in the paused list, not frozen, not told anything.
    let a = h.task("archived").await.unwrap();
    assert!(
        a.lock().await.paused.is_none(),
        "an archived chat is not brought back to life"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// ─────────────────── when a chat republishes its summary ───────────────────
//
// `update_task_quiet` exists so a quiet chat doesn't ship a whole cloned
// `TaskSummary` — the `subs` in it carry every agent's full report — to the
// frontend for no visible change. It decides with `emit_key`: change the key,
// publish; leave it, publish nothing.
//
// The key has to be *complete*, not minimal. Missing a field the UI renders
// turns a cheap update into a missed one, and the failure is silent and
// inverted: the chat reads as finished because nothing arrives, which is
// indistinguishable from a crashed agent. The transcript's extent is in the
// key for that reason — a turn that streamed text and finished in the same
// status moves no other field, so without it a chat doing real work published
// nothing at all.

#[tokio::test]
async fn a_turn_that_only_streamed_output_still_republishes() {
    let (h, _events) = harness(
        Settings::default(),
        vec![task("a", "anthropic/claude-opus-5")],
    );
    // The agent produced a reply. Status, step, todos and subs all stay put —
    // this is the turn that used to publish nothing. The item has to go in
    // *inside* the closure: `update_task_quiet` takes its `before` key first,
    // so anything mutated beforehand is already in both keys.
    let published = h
        .update_task_quiet("a", |t| {
            t.items
                .push(Item::new("text", "here is the answer", json!({})));
        })
        .await;
    assert!(
        published,
        "a turn that streamed output must republish, or a working chat reads as finished"
    );
}

#[tokio::test]
async fn a_tool_call_alone_republishes() {
    let (h, _events) = harness(
        Settings::default(),
        vec![task("a", "anthropic/claude-opus-5")],
    );
    assert!(
        h.update_task_quiet("a", |t| {
            t.items
                .push(Item::new("tool", "", json!({"name": "read_file"})));
        })
        .await,
        "a tool call is visible progress even when nothing else moved"
    );
}

#[tokio::test]
async fn an_idle_chat_stays_quiet() {
    // The other direction. If everything moved the key this method is just a
    // slower `update_task` and buys nothing at all.
    let (h, _events) = harness(
        Settings::default(),
        vec![task("a", "anthropic/claude-opus-5")],
    );
    assert!(
        !h.update_task_quiet("a", |_| {}).await,
        "a chat with nothing to report must not publish"
    );
}

/// The resume that needed pressing four times.
///
/// The shape, as reported: a chat paused because every model in its chain ran out
/// of usage, with a swarm of sub-agents parked behind the same pause. The user
/// swaps the model and presses Resume — and nothing happens. Again. Then again.
///
/// It is not the wakeup. `wait_unpaused` polls `is_paused` on every pass, so a
/// resume that lifts the flag wakes every agent in the swarm at once whatever the
/// bell does — that part scales to any size. The press *does* arrive. The chat
/// re-freezes before it draws a breath.
///
/// What re-freezes it is the chain the woken agent then walks, and `targets()`
/// builds that chain out of the accounts that are *not* on cooldown. The 429 that
/// parked this chat benched every account in the pool, and nothing re-tests one
/// while the chat is frozen — `router` expires a cooldown as it re-walks the
/// chain, but that code sits behind the very pause being lifted. So the chain
/// comes back empty, `request` reaches the bottom, raises the pause again and
/// parks. Every press repeats it against the same stale bench, which is why the
/// count felt arbitrary: each press was correct, and none had a target to wake.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_after_a_model_swap_is_not_re_frozen_by_a_stale_cooldown() {
    let dir =
        std::env::temp_dir().join(format!("openleash-resume-cooldown-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    // A key pool, which is the shape the router actually benches: the primary key
    // serves every model on the provider, so the 429 the old model drew leaves the
    // new model's chain with nothing in it — the swap cannot outrun the bench.
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    let mut cfg = key.1.clone();
    cfg.key_pool = true;
    cfg.api_keys = vec![key.1.api_key.clone()];
    s.providers.insert(key.0.clone(), cfg);
    let mut t = task("trc", "p/old");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    // The one key behind this provider is out of usage, which is what parks it.
    m.push(err(
        429,
        json!({"error": {"type": "usage_limit_reached", "message": "out of usage"}}),
    ));
    runner::send(h.clone(), "trc".into(), "start".into())
        .await
        .unwrap();
    let mut parked = false;
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        if h.is_paused("trc").await {
            parked = true;
            break;
        }
    }
    assert!(parked, "the chat parked on an out-of-usage chain");

    // The user swaps the model and presses Resume — once.
    h.update_task("trc", |t| t.model = "p/new".into()).await;
    m.push(openai_ok("Running on the new model."));
    runner::resume(&h, "trc", None).await.unwrap();
    wait_idle(&h, "trc").await;

    let arc = h.task("trc").await.unwrap();
    let g = arc.lock().await;
    assert_eq!(
        g.status, "done",
        "one resume after a swap has to run the chat, not park it again on a \
         bench nothing has re-tested since the model that set it was left behind"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The same stuck chat, by the other road: an *account* benched by a token
/// refresh the machine could not perform.
///
/// `router` benches "Credential rejected" for ten minutes when `accounts::fresh`
/// fails, and a refresh fails for any reason at all — including reasons that have
/// nothing to do with the credential, like the refresh endpoint being unreachable.
/// So one flaky request at 09:00 took the account out of every chat's chain for
/// ten minutes, and a chat parked on it could not be resumed at all: every press
/// walked the same empty chain, sent nothing, and re-parked.
///
/// The quota bench is deliberately left alone. A user waiting for a usage window
/// to roll over should not have a resume re-ask for it every few seconds, and
/// that path is what `route_falls_back_then_pauses_and_resumes` pins: its codex
/// account is still benched after a resume, so the backup is the only thing tried.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_retries_an_account_benched_by_a_failed_token_refresh() {
    let dir = std::env::temp_dir().join(format!("openleash-resume-refresh-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.accounts = vec![acct("only", "codex", 10)];
    let mut t = task("trr", "p/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    // The provider rejects the credential, and the refresh that would rescue it
    // cannot run here — so the account is benched and dropped from every chain.
    h.accts
        .cool("only", super::accounts::now() + 600, "Credential rejected");
    let only = acct("only", "codex", 10);
    assert!(
        !h.accts.usable(&only),
        "the failed refresh took it out of the chain"
    );

    runner::resume(&h, "trr", None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        h.accts.usable(&acct("only", "codex", 10)),
        "a resume has to put a refresh-failed account back in the chain: the \
         credential it holds is untouched, only our ability to renew it was"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The other half of the same rule: a quota the provider actually reported keeps
/// its bench across a resume.
#[tokio::test]
async fn a_resume_keeps_a_reported_quota_benched() {
    let (h, _events) = harness(
        Settings::default(),
        vec![task("t1", "anthropic/claude-opus-5")],
    );
    h.accts.cool(
        "spent",
        super::accounts::now() + 1800,
        "Usage limit reached",
    );
    runner::resume(&h, "t1", None).await.unwrap();
    assert!(
        !h.accts.usable(&acct("spent", "claude", 10)),
        "re-asking a quota the provider just reported is a retry storm, and the \
         user is waiting for a window to roll over — not for a resume"
    );
}

/// What the composer's Stop button does, on a *live* run: the chat is left in
/// the state the button's own label promises.
///
/// `interrupt` cancels the token and kills background jobs. The status the whole
/// composer keys off — "stopped", with the Resume choice in place of the stop
/// button — is written by `run_main` when the cancelled run unwinds. Esc Esc
/// never depended on that: `task_dismiss_pause` writes `stopped` itself. So a
/// stop that cannot unwind leaves the chat still reading as running, the stop
/// button still on screen, and the press looking like it did nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stop_button_leaves_a_stopped_chat_stopped() {
    let dir = std::env::temp_dir().join(format!("openleash-stopbtn-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "p/m");
    t.status = "idle".into();
    t.perm = "full".into();
    let (h, _) = harness(s, vec![t]);

    // A long foreground command, so the run is genuinely in flight when the
    // button is pressed rather than between rounds.
    m.push(openai_call(
        "bash",
        json!({"command": "sleep 30", "description": "sleeping"}),
    ));
    runner::send(h.clone(), "t1".into(), "go".into())
        .await
        .unwrap();

    let mut started = false;
    for _ in 0..300 {
        if h.runtime("t1").fg.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            started = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(started, "the run reached the command before we stopped it");

    // Exactly what the button's command runs.
    runner::interrupt(&h, "t1").await;

    let mut status = String::new();
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let st = h.task("t1").await.unwrap().lock().await.status.clone();
        if st != "running" && st != "waiting" {
            status = st;
            break;
        }
    }
    assert_eq!(
        status, "stopped",
        "the stop button must leave the chat stopped, or the composer keeps showing a stop button that looks broken"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The one thing the pause banner counts — a command still running — is drawn on
/// the transcript row, and the row used to freeze its glyph with everything else.
///
/// A pause blocks at `wait_unpaused` at the *top* of the bash handler, so a command
/// that already started runs to completion whatever the user does next. The banner
/// says so ("waiting on N commands to finish") and counts it in `busy`; the row drew
/// the still frozen mark over it anyway. The user could watch a live `cargo build`
/// under a row labelled paused and reasonably conclude the pause had broken it.
///
/// So the row carries `exec` for exactly as long as the process is alive, and this
/// is what keeps that honest: true while it runs, false once it is done — and
/// false again after a pause, so a frozen *turn* is never drawn as live work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_command_is_marked_exec_so_a_pause_does_not_freeze_its_glyph() {
    let dir = std::env::temp_dir().join(format!("openleash-exec-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("p", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = task("t1", "p/m");
    t.status = "idle".into();
    t.perm = "full".into();
    let (h, _) = harness(s, vec![t]);

    m.push(openai_call(
        "bash",
        json!({"command": "sleep 30", "description": "sleeping"}),
    ));
    runner::send(h.clone(), "t1".into(), "go".into())
        .await
        .unwrap();

    // The row is the only place the UI can tell the two apart, so it has to be
    // marked before the command finishes — not as part of closing it out.
    async fn exec(h: &crate::Harness) -> Option<Option<bool>> {
        h.task("t1")
            .await
            .unwrap()
            .lock()
            .await
            .items
            .iter()
            .find(|i| i.kind == "tool")
            .map(|i| i.data["exec"].as_bool())
    }
    let mut live = None;
    for _ in 0..300 {
        if h.runtime("t1").fg.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            live = exec(&h).await;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        live.flatten(),
        Some(true),
        "a command past the pause gate is still running, and the row has to say so \
         while it is"
    );

    // And it must not outlive the command: a stale `exec` would leave the spinner
    // animating over a finished call for the rest of the chat's life.
    runner::interrupt(&h, "t1").await;
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if exec(&h).await.flatten() == Some(false) {
            break;
        }
    }
    assert_eq!(
        exec(&h).await.flatten(),
        Some(false),
        "the mark is cleared once the command is done, or the row animates forever"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A background job (dev server, watcher) outlives a pause by design and never
/// exits by itself, so counting it in `busy` made "Pausing… waiting on N commands
/// to finish" a banner that could never clear. Only foreground commands drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_job_does_not_hold_the_pausing_banner_open() {
    let dir = std::env::temp_dir().join(format!("openleash-busybg-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let (h, _) = harness(Settings::default(), vec![task("t1", "m1")]);
    let id = h.bg.spawn("t1", "sleep 30", ".").unwrap();
    h.pause_task("t1", "manual", "Paused by you").await;
    assert_eq!(
        h.task("t1").await.unwrap().lock().await.busy,
        0,
        "a long-lived job is not draining"
    );
    h.runtime("t1")
        .fg
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    h.refresh_busy("t1").await;
    assert_eq!(
        h.task("t1").await.unwrap().lock().await.busy,
        1,
        "a foreground command still is"
    );
    h.runtime("t1")
        .fg
        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    h.bg.kill_task_with_reasons("t1", "test cleanup");
    let _ = id;
}
