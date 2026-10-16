//! End-to-end tests for the ACP server.
//!
//! The server is driven **in-process**: `dispatch_line` is fed the exact JSON-RPC
//! a client would write to stdin, and the responses/notifications the server
//! writes are captured from a buffer in place of stdout. A scripted local HTTP
//! provider stands in for the model, so a prompt runs a real harness turn with
//! real tool dispatch and real permission checks — including the case that
//! matters most: a tool the client declines must not run.
//!
//! The `OPENLEASH_HOME` / model-catalog / route registries are process-wide, so
//! every test holds `SERIAL` for its whole run (the same reason the other test
//! modules hold their locks across `.await` points). Do not drop it to quiet the
//! lint.
#![allow(clippy::await_holding_lock)]

use super::*;
use openleash_lib::agent::store::{CustomProvider, ProviderCfg, Settings};
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serializes the tests against the process-global registries.
static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A temp data folder for one test, with `OPENLEASH_HOME` pointed at it.
fn home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("openleash-acp-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("OPENLEASH_HOME", &dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A private working directory, so the harness never touches a real project.
fn work(home: &Path) -> PathBuf {
    let w = home.join("work");
    std::fs::create_dir_all(&w).unwrap();
    w
}

// ────────────────────────────── captured transport ──────────────────────────────

/// stdout capture: the server writes here and the test reads it back.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn lines(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .expect("stdout is utf-8")
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }
    fn json(&self) -> Vec<Value> {
        self.lines()
            .iter()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))
            })
            .collect()
    }
    /// Wait until a line matching `pred` has been written, then return it.
    async fn wait(&self, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..750 {
            for v in self.json() {
                if pred(&v) {
                    return v;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "timed out waiting for a message; captured so far:\n{}",
            self.lines().join("\n")
        );
    }
}

// ────────────────────────────── mock provider ──────────────────────────────

/// A scripted OpenAI-compatible endpoint. Each request pops the next response.
struct Mock {
    base: String,
    queue: Arc<Mutex<VecDeque<(u16, String)>>>,
}

impl Mock {
    async fn start() -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let queue: Arc<Mutex<VecDeque<(u16, String)>>> = Default::default();
        let q = queue.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let q = q.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
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
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let (status, resp) = q.lock().unwrap().pop_front().unwrap_or_else(|| {
                        (
                            599,
                            json!({"error": {"message": "script exhausted"}}).to_string(),
                        )
                    });
                    let ctype = if resp.starts_with("data:") {
                        "text/event-stream"
                    } else {
                        "application/json"
                    };
                    let out = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                        resp.len()
                    );
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Mock { base, queue }
    }

    fn push_sse(&self, events: &[Value]) {
        let mut s: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        s.push_str("data: [DONE]\n\n");
        self.queue.lock().unwrap().push_back((200, s));
    }

    fn push_text(&self, text: &str) {
        self.push_sse(&[
            json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":5}}),
        ]);
    }

    fn push_tool_call(&self, name: &str, args: Value) {
        self.push_sse(&[json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call_{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})]);
    }
}

// ────────────────────────────── fixture ──────────────────────────────

/// A `Server` wired to `mock`, with its event pump already running, plus the
/// buffer its writes land in.
fn fixture(mock: &Mock, tasks: Vec<Task>) -> (Arc<Server>, Buf) {
    let settings = Settings {
        model: "p/m".to_string(),
        custom_providers: vec![CustomProvider {
            id: "p".into(),
            name: "p".into(),
            base_url: mock.base.clone(),
            kind: "openai".into(),
            insist: false,
        }],
        providers: HashMap::from([(
            "p".to_string(),
            ProviderCfg {
                api_key: "k-p".to_string(),
                base_url: String::new(),
                enabled: true,
                api_keys: vec![],
                key_pool: false,
            },
        )]),
        ..Settings::default()
    };
    let buf = Buf::default();
    let (server, rx) = make_server(settings, tasks, Box::new(buf.clone()));
    tokio::spawn(pump(server.clone(), rx));
    (server, buf)
}

fn send(msg: Value) -> String {
    msg.to_string()
}

// ────────────────────────────── tests ──────────────────────────────

/// The handshake completes, echoes the request id, and negotiates protocol v1
/// with the capabilities we implement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initialize_negotiates_protocol_version_one() {
    let _g = lock();
    let dir = home("init");
    let _ = work(&dir);
    let mock = Mock::start().await;
    let (server, buf) = fixture(&mock, vec![]);

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": true}, "terminal": true},
                "clientInfo": {"name": "fake", "version": "0"},
            },
        })))
        .await;

    let resp = buf.wait(|v| v["id"] == json!(0)).await;
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["result"]["protocolVersion"], json!(1));
    assert_eq!(
        resp["result"]["agentCapabilities"]["loadSession"],
        json!(true)
    );
    assert_eq!(resp["result"]["agentInfo"]["name"], json!("openleash-acp"));
}

/// `session/new` returns a non-empty id, and later methods can address it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_new_returns_a_non_empty_id() {
    let _g = lock();
    let dir = home("new");
    let w = work(&dir);
    let mock = Mock::start().await;
    let (server, buf) = fixture(&mock, vec![]);

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": w.to_string_lossy(), "mcpServers": []},
        })))
        .await;

    let resp = buf.wait(|v| v["id"] == json!(1)).await;
    let sid = resp["result"]["sessionId"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(!sid.is_empty(), "session id must be non-empty: {resp}");
    // The session is the app's own task, so it is addressable as one.
    assert!(server.h.task(&sid).await.is_ok(), "session id is a task id");
}

/// A prompt against a scripted model streams the assistant's text as
/// `session/update` notifications and ends with `end_turn`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_streams_assistant_text_and_ends_the_turn() {
    let _g = lock();
    let dir = home("text");
    let w = work(&dir);
    let mock = Mock::start().await;
    let t = super::new_task(
        "s-text",
        &w.to_string_lossy(),
        "p/m",
        "allowlist",
        &["explore".into()],
    )
    .unwrap();
    let (server, buf) = fixture(&mock, vec![t]);

    mock.push_text("hello from the model");

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/prompt",
            "params": {"sessionId": "s-text", "prompt": [{"type": "text", "text": "hi"}]},
        })))
        .await;

    // The assistant text arrives as an agent message chunk.
    let chunk = buf
        .wait(|v| {
            v["method"] == json!("session/update")
                && v["params"]["update"]["sessionUpdate"] == json!("agent_message_chunk")
        })
        .await;
    assert_eq!(chunk["params"]["sessionId"], json!("s-text"));
    assert_eq!(
        chunk["params"]["update"]["content"]["text"],
        json!("hello from the model")
    );

    let resp = buf.wait(|v| v["id"] == json!(7)).await;
    assert_eq!(resp["result"]["stopReason"], json!("end_turn"));
}

/// **The security test.** A tool that needs approval raises a
/// `session/request_permission`; when the client declines, the tool must not run
/// and the run reports the denial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declined_tool_call_does_not_run() {
    let _g = lock();
    let dir = home("deny");
    let w = work(&dir);
    let marker = w.join("openleash_acp_marker.txt");
    assert!(!marker.exists(), "precondition: no marker before the run");

    let mock = Mock::start().await;
    let t = super::new_task(
        "s-deny",
        &w.to_string_lossy(),
        "p/m",
        "allowlist",
        &["explore".into()],
    )
    .unwrap();
    let (server, buf) = fixture(&mock, vec![t]);

    // The model asks for a command that writes a file — never read-only, so the
    // allowlist rung asks. If it ever ran despite the denial, the marker appears.
    mock.push_tool_call(
        "bash",
        json!({"command": "echo pwned > openleash_acp_marker.txt"}),
    );
    mock.push_text("done");

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "session/prompt",
            "params": {"sessionId": "s-deny", "prompt": [{"type": "text", "text": "go"}]},
        })))
        .await;

    // The approval is forwarded to the client.
    let req = buf
        .wait(|v| v["method"] == json!("session/request_permission"))
        .await;
    assert_eq!(req["params"]["sessionId"], json!("s-deny"));
    let options = req["params"]["options"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        options.iter().any(|o| o["kind"] == json!("reject_once")),
        "a reject option must be offered: {req}"
    );

    // The client declines.
    let req_id = req["id"].clone();
    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {"outcome": {"outcome": "selected", "optionId": "reject-once"}},
        })))
        .await;

    let resp = buf.wait(|v| v["id"] == json!(42)).await;
    assert_eq!(resp["result"]["stopReason"], json!("end_turn"));

    // The tool did not run.
    assert!(
        !marker.exists(),
        "a declined tool must not have run (found {})",
        marker.display()
    );
    // And the client was told the tool failed.
    let failed = buf
        .wait(|v| {
            v["method"] == json!("session/update")
                && v["params"]["update"]["sessionUpdate"] == json!("tool_call_update")
                && v["params"]["update"]["status"] == json!("failed")
        })
        .await;
    assert_eq!(failed["params"]["sessionId"], json!("s-deny"));
}

/// An unknown method gets a well-formed JSON-RPC error, not a panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_method_returns_a_jsonrpc_error() {
    let _g = lock();
    let dir = home("unknown");
    let _ = work(&dir);
    let mock = Mock::start().await;
    let (server, buf) = fixture(&mock, vec![]);

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "does/not/exist",
            "params": {},
        })))
        .await;

    let resp = buf.wait(|v| v["id"] == json!(9)).await;
    assert_eq!(resp["error"]["code"], json!(METHOD_NOT_FOUND));
    assert!(resp["error"]["message"].as_str().is_some());
    assert!(resp.get("result").is_none());
}

/// Garbage on the wire is answered with a parse error rather than killing the
/// server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_json_returns_a_parse_error() {
    let _g = lock();
    let dir = home("parse");
    let _ = work(&dir);
    let mock = Mock::start().await;
    let (server, buf) = fixture(&mock, vec![]);

    server.dispatch_line("{ this is not json").await;

    let resp = buf.wait(|v| v["error"]["code"] == json!(PARSE_ERROR)).await;
    assert!(resp["id"].is_null());
}

/// `session/load` replays the stored conversation before responding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_load_replays_history() {
    let _g = lock();
    let dir = home("load");
    let w = work(&dir);
    let mock = Mock::start().await;

    let mut t = super::new_task(
        "s-load",
        &w.to_string_lossy(),
        "p/m",
        "allowlist",
        &["explore".into()],
    )
    .unwrap();
    t.messages = vec![
        serde_json::from_value(
            json!({"role": "user", "content": [{"type": "text", "text": "prior question"}]}),
        )
        .unwrap(),
        serde_json::from_value(
            json!({"role": "assistant", "content": [{"type": "text", "text": "prior answer"}]}),
        )
        .unwrap(),
    ];
    let (server, buf) = fixture(&mock, vec![t]);

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/load",
            "params": {"sessionId": "s-load", "cwd": w.to_string_lossy(), "mcpServers": []},
        })))
        .await;

    let resp = buf.wait(|v| v["id"] == json!(3)).await;
    assert!(resp.get("result").is_some());
    let kinds: Vec<String> = buf
        .json()
        .iter()
        .filter_map(|v| {
            (v["method"] == json!("session/update"))
                .then(|| {
                    v["params"]["update"]["sessionUpdate"]
                        .as_str()
                        .map(String::from)
                })
                .flatten()
        })
        .collect();
    assert!(kinds.contains(&"user_message_chunk".to_string()));
    assert!(kinds.contains(&"agent_message_chunk".to_string()));
}

/// A `session/cancel` notification stops the run and the prompt reports
/// `cancelled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_cancel_reports_cancelled() {
    let _g = lock();
    let dir = home("cancel");
    let w = work(&dir);
    let mock = Mock::start().await;
    let t = super::new_task(
        "s-can",
        &w.to_string_lossy(),
        "p/m",
        "allowlist",
        &["explore".into()],
    )
    .unwrap();
    let (server, buf) = fixture(&mock, vec![t]);

    // A tool call parks the run while it waits for permission, giving the test a
    // stable point to cancel from.
    mock.push_tool_call(
        "bash",
        json!({"command": "echo pwned > openleash_acp_marker.txt"}),
    );
    mock.push_text("done");

    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session/prompt",
            "params": {"sessionId": "s-can", "prompt": [{"type": "text", "text": "go"}]},
        })))
        .await;

    // Wait until the approval is outstanding, then cancel. Per the spec the
    // client answers the pending permission request with the `cancelled` outcome.
    let req = buf
        .wait(|v| v["method"] == json!("session/request_permission"))
        .await;
    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": "s-can"},
        })))
        .await;
    server
        .dispatch_line(&send(json!({
            "jsonrpc": "2.0",
            "id": req["id"].clone(),
            "result": {"outcome": {"outcome": "cancelled"}},
        })))
        .await;

    let resp = buf.wait(|v| v["id"] == json!(5)).await;
    assert_eq!(resp["result"]["stopReason"], json!("cancelled"));
    assert!(!w.join("openleash_acp_marker.txt").exists());
}
