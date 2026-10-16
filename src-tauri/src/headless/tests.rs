//! Integration tests for the headless path: a scripted local HTTP provider is
//! driven through `run_prompt`, and the NDJSON contract, resume-by-id and the
//! exit-code table are asserted end to end.
//!
//! The `OPENLEASH_HOME` / model-catalog / route globals are process-wide, so
//! every test holds `SERIAL` for its whole run — the same reason
//! `agent::tests` holds its own locks across `.await` points. Do not drop it to
//! quiet the lint.
#![allow(clippy::await_holding_lock)]

use super::*;
use openleash_lib::agent::store::{CustomProvider, ProviderCfg, Settings};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serializes the tests against the process-global registries.
static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A temp data folder for one test, with `OPENLEASH_HOME` pointed at it.
fn home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "openleash-headless-{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("OPENLEASH_HOME", &dir);
    dir
}

/// Shared stdout capture: the sink writes here, and the test reads it back.
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
            .enumerate()
            .map(|(i, l)| {
                serde_json::from_str::<Value>(l)
                    .unwrap_or_else(|e| panic!("line {} is not JSON ({e}): {l}", i + 1))
            })
            .collect()
    }
}

// ────────────────────────────── mock provider ──────────────────────────────

/// A scripted OpenAI-compatible endpoint. Each request pops the next scripted
/// response; every request body is logged so tests can assert on the
/// conversation the harness actually sent.
struct Mock {
    base: String,
    log: Arc<Mutex<Vec<Value>>>,
    queue: Arc<Mutex<VecDeque<(u16, String)>>>,
}

impl Mock {
    async fn start() -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log: Arc<Mutex<Vec<Value>>> = Default::default();
        let queue: Arc<Mutex<VecDeque<(u16, String)>>> = Default::default();
        let (l, q) = (log.clone(), queue.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (l, q) = (l.clone(), q.clone());
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
                    let body: Value =
                        serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                    l.lock().unwrap().push(body);
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
        Mock { base, log, queue }
    }

    fn push_sse(&self, events: &[Value]) {
        let mut s: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        s.push_str("data: [DONE]\n\n");
        self.queue.lock().unwrap().push_back((200, s));
    }

    /// A text answer carrying the provider's reported usage.
    fn push_text(&self, text: &str) {
        self.push_sse(&[
            json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":80}}}),
        ]);
    }

    fn push_tool_call(&self, name: &str, args: Value) {
        self.push_sse(&[json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call_{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})]);
    }

    fn push_err(&self, status: u16, message: &str) {
        self.queue
            .lock()
            .unwrap()
            .push_back((status, json!({"error": {"message": message}}).to_string()));
    }

    fn reqs(&self) -> Vec<Value> {
        self.log.lock().unwrap().clone()
    }
}

/// A harness wired to `mock`, with one custom OpenAI-compatible provider, plus
/// the sink its own events flow into.
fn fixture(mock: &Mock, buffer: &Buf, tasks: Vec<Task>) -> (Arc<Harness>, Sink) {
    let s = Settings {
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
    boot(s, tasks, Box::new(buffer.clone()))
}

fn opts(prompt: &str, work: &std::path::Path, resume: Option<String>) -> RunOpts {
    RunOpts {
        prompt: prompt.to_string(),
        cwd: work.to_string_lossy().to_string(),
        model: None,
        perm: Some("auto".to_string()),
        plan: false,
        resume,
        timeout: Duration::from_secs(60),
    }
}

/// A private working directory for one test, so the harness never sweeps the
/// shared system temp folder looking for checkpoints.
fn work(dir: &std::path::Path) -> PathBuf {
    let w = dir.join("work");
    std::fs::create_dir_all(&w).expect("test working directory");
    w
}

/// The whole happy path: a session line, a tool call and its result, the
/// assistant's answer, and a result line carrying provider-reported usage.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_emits_ndjson_and_exits_zero() {
    let _g = lock();
    let dir = home("ndjson");
    let work = work(&dir);
    let mock = Mock::start().await;
    let buffer = Buf::default();
    let (h, sink) = fixture(&mock, &buffer, vec![]);
    std::fs::write(work.join("hello.txt"), "hello from the fixture\n").unwrap();

    // `todo_write` is deliberately *hidden* from the timeline, so the visible
    // tool call in this fixture is a real `read_file`.
    mock.push_tool_call("read_file", json!({"path": "hello.txt"}));
    mock.push_text("Done.");

    let code = run_prompt(&h, &sink, &opts("please do the thing", &work, None)).await;
    assert_eq!(code, EXIT_OK, "a clean run exits 0");

    // (a) every line parses as its own JSON object
    let events = buffer.json();
    assert!(events.len() >= 5, "saw {} lines", events.len());

    // (b) the first event is the session, and it carries the resumable id
    assert_eq!(events[0]["type"], "session");
    let session_id = events[0]["session_id"].as_str().unwrap_or("").to_string();
    assert!(session_id.len() >= 8, "session id: {session_id:?}");

    // (c) the tool call and its result both appear, with the real arguments
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().unwrap_or(""))
        .collect();
    assert!(kinds.contains(&"tool_call"), "types: {kinds:?}");
    assert!(kinds.contains(&"tool_result"), "types: {kinds:?}");
    let call = events
        .iter()
        .find(|e| e["type"] == "tool_call")
        .expect("tool_call");
    assert_eq!(call["name"], "read_file");
    assert_eq!(call["input"]["path"], "hello.txt");
    let result = events
        .iter()
        .find(|e| e["type"] == "tool_result")
        .expect("tool_result");
    assert_eq!(result["status"], "ok");
    let output = result["output"].as_str().unwrap_or("");
    assert!(
        output.contains("hello from the fixture"),
        "the result carries the tool's real output: {output:?}"
    );
    assert!(kinds.contains(&"assistant"), "the answer is streamed");

    // (d) the terminal event carries the usage the provider reported. The mock
    // answers with `prompt_tokens: 100` of which `cached_tokens: 80`, so the
    // billable input is 100-80 = 20 — the number the provider sent, not an
    // estimate of the prompt.
    let last = events.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["session_id"], session_id);
    assert_eq!(last["ok"], true);
    assert_eq!(last["exit_code"], 0);
    assert_eq!(last["status"], "done");
    assert_eq!(last["usage"]["input"], 20);
    assert_eq!(last["usage"]["output"], 5);
    assert_eq!(last["usage"]["cache_read"], 80);
    assert!(last["cost"].as_f64().is_some(), "cost is a number");

    // The answer text is the model's own words, not a summary.
    let assistant = events.iter().find(|e| e["type"] == "assistant").unwrap();
    assert_eq!(assistant["text"], "Done.");

    let _ = std::fs::remove_dir_all(dir);
}

/// `--resume <id>` continues the same task — in a fresh process, reading the
/// task back off disk — instead of starting a second one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_by_id_reuses_the_same_task() {
    let _g = lock();
    let dir = home("resume");
    let work = work(&dir);
    let mock = Mock::start().await;

    // Process 1: a new task.
    let first = Buf::default();
    let (h1, sink1) = fixture(&mock, &first, vec![]);
    mock.push_text("First answer.");
    let code = run_prompt(&h1, &sink1, &opts("first prompt", &work, None)).await;
    assert_eq!(code, EXIT_OK);
    let session_id = first.json()[0]["session_id"].as_str().unwrap().to_string();
    assert_eq!(h1.tasks.read().await.len(), 1);
    drop(h1);

    // Process 2: nothing in memory, everything off disk.
    //
    // The id set is captured before and after rather than assuming one file:
    // `OPENLEASH_HOME` is process-global, so a straggling writer from an earlier
    // test in this same binary can drop its own task file here. Set equality is
    // what "resuming must not create a second task" actually means.
    let second = Buf::default();
    let (h2, sink2) = fixture(&mock, &second, store::load_tasks());
    let before: std::collections::HashSet<String> = h2.tasks.read().await.keys().cloned().collect();
    assert!(
        before.contains(&session_id),
        "the first run's task was persisted: {before:?}"
    );
    mock.push_text("Second answer.");
    let code = run_prompt(
        &h2,
        &sink2,
        &opts("second prompt", &work, Some(session_id.clone())),
    )
    .await;
    assert_eq!(code, EXIT_OK);

    let events = second.json();
    assert_eq!(events[0]["type"], "session");
    assert_eq!(events[0]["session_id"], session_id, "same task, same id");
    assert_eq!(events[0]["resumed"], true);
    let after: std::collections::HashSet<String> = h2.tasks.read().await.keys().cloned().collect();
    assert_eq!(after, before, "resuming must not create a second task");

    // The resumed run sent the *original* prompt as history, which is what
    // makes it a continuation rather than a fresh conversation.
    let last = mock.reqs().last().cloned().expect("a request");
    assert!(
        last.to_string().contains("first prompt"),
        "resume carried the earlier conversation: {last}"
    );
    assert!(last.to_string().contains("second prompt"));

    let _ = std::fs::remove_dir_all(dir);
}

/// A run the provider rejects outright fails, and the exit code says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_run_exits_non_zero() {
    let _g = lock();
    let dir = home("failure");
    let work = work(&dir);
    let mock = Mock::start().await;
    let buffer = Buf::default();
    let (h, sink) = fixture(&mock, &buffer, vec![]);

    mock.push_err(400, "messages.3.content.0: unexpected `tool_use_id`");

    let code = run_prompt(&h, &sink, &opts("do something impossible", &work, None)).await;
    assert_ne!(code, 0, "a rejected request must not report success");
    assert_eq!(code, EXIT_FAILURE);

    let events = buffer.json();
    let last = events.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["ok"], false);
    assert_eq!(last["exit_code"], EXIT_FAILURE);
    assert_eq!(last["status"], "failed");

    let _ = std::fs::remove_dir_all(dir);
}

/// An unknown `--resume` id is a usage error, reported before anything runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resuming_an_unknown_task_is_a_usage_error() {
    let _g = lock();
    let dir = home("unknown");
    let work = work(&dir);
    let mock = Mock::start().await;
    let buffer = Buf::default();
    let (h, sink) = fixture(&mock, &buffer, vec![]);

    let code = run_prompt(&h, &sink, &opts("hello", &work, Some("nope".into()))).await;
    assert_eq!(code, EXIT_USAGE);
    assert!(
        buffer.lines().is_empty(),
        "nothing was started, so nothing is reported"
    );
    assert!(mock.reqs().is_empty(), "no model call for an unknown task");

    let _ = std::fs::remove_dir_all(dir);
}

/// A run that blocks on an approval nobody can answer reports its own exit code
/// instead of hanging forever. This is the one part of the contract that only
/// exists because headless mode has no user, so it gets its own test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_blocked_on_an_approval_exits_needs_input() {
    let _g = lock();
    let dir = home("needsinput");
    let work = work(&dir);
    let mock = Mock::start().await;
    let buffer = Buf::default();
    let (h, sink) = fixture(&mock, &buffer, vec![]);

    // `allowlist` asks before a command with side effects, and there is nobody
    // here to answer the card it puts up.
    mock.push_tool_call("bash", json!({"command": "echo hi > side-effect.txt"}));

    let mut o = opts("write a file", &work, None);
    o.perm = Some("allowlist".into());
    o.timeout = Duration::from_secs(30);
    let code = run_prompt(&h, &sink, &o).await;
    assert_eq!(
        code, EXIT_NEEDS_INPUT,
        "a prompt nobody can answer has its own exit code"
    );
    assert!(
        !work.join("side-effect.txt").exists(),
        "the unapproved command did not run"
    );

    let events = buffer.json();
    let last = events.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["ok"], false);
    assert_eq!(last["exit_code"], EXIT_NEEDS_INPUT);

    let _ = std::fs::remove_dir_all(dir);
}
