//! Opt-in, local, off-by-default OTLP/HTTP trace export.
//!
//! One span per model request and one per tool call, using the OpenTelemetry
//! GenAI semantic conventions where they apply. This is the opposite of
//! phoning home: there is no vendor default, no API key and no destination the
//! user did not type, and the whole feature is inert until they set
//! [`Settings::trace_endpoint`](crate::agent::store::Settings::trace_endpoint).
//!
//! Two invariants the surrounding code depends on:
//!
//! * **Off costs nothing.** [`model_request`] and [`tool_call`] check a single
//!   atomic first and return before allocating; no client, no queue and no
//!   thread exist until [`configure`] is given a URL.
//! * **Export can never delay or fail a run.** Spans go on a bounded queue that
//!   the exporter thread drains into batched POSTs on its own runtime. A slow,
//!   unreachable or erroring collector costs spans (a full queue drops) and
//!   nothing else.
//!
//! The complete attribute surface — pinned by
//! `spans_carry_names_counts_and_status_only`, so a new one has to be added
//! there on purpose:
//!
//! | Attribute | On | Value |
//! |---|---|---|
//! | `service.name` (resource) | both | `openleash` |
//! | `gen_ai.operation.name` | both | `chat` / `execute_tool` |
//! | `gen_ai.provider.name` | model | the harness provider id (`anthropic`, `openai`, …) |
//! | `gen_ai.request.model` | model | the model name sent on the wire |
//! | `gen_ai.usage.input_tokens` | model | the provider's own input-token count |
//! | `gen_ai.usage.output_tokens` | model | the provider's own output-token count |
//! | `gen_ai.tool.name` | tool | the tool's name (`read_file`, `bash`, …) |
//! | `error.type` | both, on failure | `_OTHER` |
//!
//! No prompt, message, file content, tool argument, tool result, tool
//! description, environment value, URL or credential is ever an attribute —
//! including the semconv's own content attributes, which are opt-in and are
//! never set here.

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The URL the settings screen suggests in its placeholder.
///
/// A suggestion, never a default: the stored value starts empty so exporting
/// nothing is what an install does until the user types their own collector in.
pub const SUGGESTED_ENDPOINT: &str = "http://localhost:4318/v1/traces";

/// `service.name` on the exported resource.
pub const SERVICE_NAME: &str = "openleash";

/// Instrumentation scope name, so a collector can tell these spans apart.
const SCOPE_NAME: &str = "openleash";

/// `gen_ai.operation.name` for a model request.
const OP_CHAT: &str = "chat";
/// `gen_ai.operation.name` for a tool call.
const OP_EXECUTE_TOOL: &str = "execute_tool";

/// `error.type` on a failed span.
///
/// The semconv value for "the instrumentation has no more specific code": the
/// harness hands this module booleans, not provider error codes, and a message
/// would be content this module must never carry.
const ERROR_TYPE_OTHER: &str = "_OTHER";

/// OTLP span kinds (`SpanKind`), which OTLP/JSON requires as integers.
const KIND_INTERNAL: i32 = 1;
const KIND_CLIENT: i32 = 3;

/// OTLP status codes (`StatusCode`).
const STATUS_UNSET: i32 = 0;
const STATUS_ERROR: i32 = 2;

/// How many spans may wait for the exporter before new ones are dropped.
///
/// The bound is the whole memory guarantee: a span is a name, an id pair, two
/// timestamps and a handful of attributes — a few hundred bytes — so a full
/// queue is a few hundred kilobytes, whatever the collector is doing. Dropping
/// is the correct failure: the alternative (growing, or blocking the caller)
/// would let a dead collector degrade the run it is only supposed to observe.
pub const QUEUE_BOUND: usize = 1024;

/// Most spans in one POST. A burst becomes a few requests rather than one per span.
const MAX_BATCH: usize = 256;

/// Give up points for one POST, so a hung collector cannot pin the exporter
/// thread (and therefore the queue) for the life of the process.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// What the settings screen can show: whether export is live, where it points,
/// how many spans were dropped, and why a configured endpoint was refused.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    /// The endpoint last passed to [`configure`]; empty when export is off.
    pub endpoint: String,
    /// Spans dropped because the queue was full, since the endpoint was set.
    pub dropped: u64,
    /// Set when the configured endpoint was refused (see [`validate`]).
    pub error: Option<String>,
}

/// One finished model request.
#[derive(Debug)]
pub struct ModelRequest<'a> {
    /// The harness provider id the request went to (`anthropic`, `openai`, …).
    pub provider: &'a str,
    /// The model name as the provider knows it.
    pub model: &'a str,
    /// The provider's own reported input-token count. `None` when it reported
    /// none — this module never estimates a count from anything.
    pub input_tokens: Option<u64>,
    /// The provider's own reported output-token count. See `input_tokens`.
    pub output_tokens: Option<u64>,
    /// Wall-clock duration of the request.
    pub duration: Duration,
    /// The request failed (the span keeps whatever counts it did report).
    pub error: bool,
}

/// One finished tool call.
#[derive(Debug)]
pub struct ToolCall<'a> {
    /// The tool's name. Never its arguments or output.
    pub tool: &'a str,
    /// Wall-clock duration of the call.
    pub duration: Duration,
    /// The call failed, was denied, or was interrupted.
    pub error: bool,
}

enum AttrValue {
    Str(String),
    Int(u64),
}

/// One span attribute. The key is always a literal from this file, which is
/// what makes "no content ever leaves" checkable rather than aspirational.
struct Attr {
    key: &'static str,
    value: AttrValue,
}

struct Span {
    trace_id: String,
    span_id: String,
    name: String,
    kind: i32,
    start_ns: u64,
    end_ns: u64,
    error: bool,
    attrs: Vec<Attr>,
}

struct State {
    /// Endpoint [`configure`] was last called with. `""` means off.
    endpoint: String,
    /// Whether [`configure`] has run at all, so the first call with `""` is
    /// distinguishable from a later one with the same value.
    configured: bool,
    /// The export queue. `None` means no exporter exists — no thread, nothing.
    sender: Option<SyncSender<Span>>,
    /// Why the endpoint in `endpoint` was refused, if it was.
    error: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    endpoint: String::new(),
    configured: false,
    sender: None,
    error: None,
});

/// The hot path's whole check. Kept separate from `STATE` so an emit with no
/// endpoint configured never touches a lock.
static EXPORTING: AtomicBool = AtomicBool::new(false);

/// Spans dropped because the queue was full (or closed mid-restart).
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static SPAWNS: AtomicU64 = AtomicU64::new(0);

fn lock() -> std::sync::MutexGuard<'static, State> {
    // A panic while holding this must not disable export forever, and it must
    // not propagate: the only thing guarded here is exporter bookkeeping.
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether spans are being exported right now.
pub fn enabled() -> bool {
    EXPORTING.load(Ordering::Relaxed)
}

/// What the settings screen should show.
pub fn status() -> Status {
    let st = lock();
    Status {
        enabled: enabled(),
        endpoint: st.endpoint.clone(),
        dropped: DROPPED.load(Ordering::Relaxed),
        error: st.error.clone(),
    }
}

/// Check an endpoint without starting anything.
///
/// `Ok(())` for the empty string, which is the off switch rather than a
/// mistake. Everything else has to be an absolute `http`/`https` URL with a
/// host: this text is both where spans go and the only thing the user types to
/// turn the feature on, so "silently accept something we cannot POST to" would
/// look like working export that never arrives.
pub fn validate(endpoint: &str) -> Result<(), String> {
    let e = endpoint.trim();
    if e.is_empty() {
        return Ok(());
    }
    let url = reqwest::Url::parse(e).map_err(|err| {
        format!("`{e}` is not a usable trace endpoint ({err}). It needs to be a full URL, like `{SUGGESTED_ENDPOINT}`.")
    })?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "`{other}://` trace endpoints are not supported; use an http:// or https:// URL \
                 pointing at a collector you run (e.g. `{SUGGESTED_ENDPOINT}`)."
            ))
        }
    }
    if url.host_str().unwrap_or("").is_empty() {
        return Err(format!("`{e}` names no host to send spans to."));
    }
    // A URL carrying `user:password@` would put a credential in settings.json
    // and on the wire as a Basic-auth header. Nothing in this feature
    // authenticates to anything, so a secret here is a mistake to refuse
    // rather than keep.
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "A trace endpoint must not contain credentials (user:password@); export sends no \
             authentication. Point it at an unauthenticated collector endpoint."
                .into(),
        );
    }
    Ok(())
}

/// Point export at `endpoint`, or turn it off with `""`.
///
/// Idempotent for an unchanged endpoint: the caller may be a settings save that
/// had nothing to do with traces, and restarting the exporter (dropping queued
/// spans, reopening a connection) on every unrelated toggle would be both
/// wasteful and surprising.
///
/// Returns the refusal for an endpoint that fails [`validate`]; export stays
/// off in that case, and [`status`] reports why.
pub fn configure(endpoint: &str) -> Result<(), String> {
    let want = endpoint.trim().to_string();
    let mut st = lock();
    if st.configured && st.endpoint == want {
        // Nothing moved. Report the same verdict as last time without touching
        // the running exporter.
        return match &st.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        };
    }
    st.configured = true;
    st.endpoint = want.clone();
    st.error = None;
    DROPPED.store(0, Ordering::Relaxed);
    // Stopping first, always: dropping the sender is what ends the old
    // exporter thread (it drains what it already holds, then exits), so a
    // move from one collector to another cannot leave two running.
    st.sender = None;
    EXPORTING.store(false, Ordering::Relaxed);
    if want.is_empty() {
        return Ok(());
    }
    if let Err(e) = validate(&want) {
        st.error = Some(e.clone());
        return Err(e);
    }
    st.sender = Some(spawn(&want));
    EXPORTING.store(true, Ordering::Relaxed);
    #[cfg(test)]
    SPAWNS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// One span per model request. Does nothing (and allocates nothing) when no
/// endpoint is configured.
pub fn model_request(r: &ModelRequest<'_>) {
    if !enabled() {
        return;
    }
    let mut attrs = Vec::with_capacity(5);
    attrs.push(str_attr("gen_ai.operation.name", OP_CHAT));
    attrs.push(Attr {
        key: "gen_ai.provider.name",
        value: AttrValue::Str(r.provider.to_string()),
    });
    if !r.model.is_empty() {
        attrs.push(Attr {
            key: "gen_ai.request.model",
            value: AttrValue::Str(r.model.to_string()),
        });
    }
    if let Some(n) = r.input_tokens {
        attrs.push(int_attr("gen_ai.usage.input_tokens", n));
    }
    if let Some(n) = r.output_tokens {
        attrs.push(int_attr("gen_ai.usage.output_tokens", n));
    }
    let name = if r.model.is_empty() {
        OP_CHAT.to_string()
    } else {
        format!("{OP_CHAT} {}", r.model)
    };
    emit(Span {
        trace_id: new_trace_id(),
        span_id: new_span_id(),
        name,
        kind: KIND_CLIENT,
        start_ns: now_nanos().saturating_sub(nanos(r.duration)),
        end_ns: now_nanos(),
        error: r.error,
        attrs,
    });
}

/// One span per tool call. Does nothing (and allocates nothing) when no
/// endpoint is configured.
pub fn tool_call(t: &ToolCall<'_>) {
    if !enabled() {
        return;
    }
    let attrs = vec![
        str_attr("gen_ai.operation.name", OP_EXECUTE_TOOL),
        Attr {
            key: "gen_ai.tool.name",
            value: AttrValue::Str(t.tool.to_string()),
        },
    ];
    emit(Span {
        trace_id: new_trace_id(),
        span_id: new_span_id(),
        name: format!("{OP_EXECUTE_TOOL} {}", t.tool),
        kind: KIND_INTERNAL,
        start_ns: now_nanos().saturating_sub(nanos(t.duration)),
        end_ns: now_nanos(),
        error: t.error,
        attrs,
    });
}

/// Hand a span to the exporter, or drop it.
///
/// `try_send` and nothing else: waiting for room would put the collector's
/// latency in the middle of a turn, which is the one outcome this module exists
/// to avoid.
fn emit(span: Span) {
    let st = lock();
    let Some(tx) = st.sender.as_ref() else {
        return;
    };
    match tx.try_send(span) {
        Ok(()) => {}
        // Full: the collector is slower than the run. Disconnected: the
        // exporter thread is gone (its runtime or client failed to build).
        // Either way the span is not exported, which is exactly what the
        // counter is for — the caller is never told, because a run must not
        // fail over telemetry.
        Err(_) => {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Start the exporter thread for `endpoint`.
///
/// Its own current-thread runtime rather than the app's: the thread outlives
/// any single turn, must not compete with agent work, and must keep working
/// when nothing else is running (a `spawn_blocking` closure in the app's pool
/// would also work, but this cannot be starved by it).
fn spawn(endpoint: &str) -> SyncSender<Span> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Span>(QUEUE_BOUND);
    let endpoint = endpoint.to_string();
    let spawned = std::thread::Builder::new()
        .name("openleash-trace".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                let Ok(client) = reqwest::Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    .timeout(REQUEST_TIMEOUT)
                    .build()
                else {
                    return;
                };
                let mut warned = false;
                // Blocks (this thread's only job) until the sender is dropped,
                // after which `recv` yields what is already queued and then `Err`.
                while let Ok(first) = rx.recv() {
                    let mut batch = Vec::with_capacity(MAX_BATCH);
                    batch.push(first);
                    while batch.len() < MAX_BATCH {
                        match rx.try_recv() {
                            Ok(s) => batch.push(s),
                            Err(_) => break,
                        }
                    }
                    let body = payload(&batch).to_string();
                    let res = client
                        .post(&endpoint)
                        .header("content-type", "application/json")
                        .body(body)
                        .send()
                        .await;
                    // Say so once per exporter, not once per failure: a wrong
                    // endpoint would otherwise write a line every few seconds
                    // for the whole session.
                    if let Err(e) = res {
                        if !warned {
                            warned = true;
                            eprintln!(
                                "[openleash] trace export to {endpoint} failed ({e}); \
                                 spans are being dropped"
                            );
                        }
                    }
                }
            });
        });
    if spawned.is_err() {
        // No thread: export silently does nothing, which is the same outcome as
        // an unreachable collector. Dropping `tx` closes the channel.
        return tx;
    }
    tx
}

/// The `ExportTraceServiceRequest` for one batch, as OTLP/HTTP JSON.
///
/// Shapes and encodings follow the OTLP spec's deviations from proto3 JSON:
/// `traceId`/`spanId` are hex strings, enum values are integers, keys are
/// lowerCamelCase, and 64-bit integers are decimal strings.
fn payload(spans: &[Span]) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": { "stringValue": SERVICE_NAME }
                }]
            },
            "scopeSpans": [{
                "scope": { "name": SCOPE_NAME, "version": env!("CARGO_PKG_VERSION") },
                "spans": spans.iter().map(span_json).collect::<Vec<_>>()
            }]
        }]
    })
}

fn span_json(s: &Span) -> Value {
    let mut attrs: Vec<Value> = s.attrs.iter().map(attr_json).collect();
    if s.error {
        attrs.push(attr_json(&str_attr("error.type", ERROR_TYPE_OTHER)));
    }
    let mut o = Map::new();
    o.insert("traceId".into(), json!(s.trace_id));
    o.insert("spanId".into(), json!(s.span_id));
    o.insert("name".into(), json!(s.name));
    o.insert("kind".into(), json!(s.kind));
    o.insert("startTimeUnixNano".into(), json!(s.start_ns.to_string()));
    o.insert("endTimeUnixNano".into(), json!(s.end_ns.to_string()));
    o.insert("attributes".into(), Value::Array(attrs));
    o.insert(
        "status".into(),
        json!({ "code": if s.error { STATUS_ERROR } else { STATUS_UNSET } }),
    );
    Value::Object(o)
}

fn attr_json(a: &Attr) -> Value {
    let value = match &a.value {
        AttrValue::Str(s) => json!({ "stringValue": s }),
        AttrValue::Int(n) => json!({ "intValue": n.to_string() }),
    };
    json!({ "key": a.key, "value": value })
}

fn str_attr(key: &'static str, v: &str) -> Attr {
    Attr {
        key,
        value: AttrValue::Str(v.to_string()),
    }
}

fn int_attr(key: &'static str, n: u64) -> Attr {
    Attr {
        key,
        value: AttrValue::Int(n),
    }
}

/// 128 random bits as 32 hex characters, which is what OTLP wants for `traceId`.
fn new_trace_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 64 random bits as 16 hex characters, which is what OTLP wants for `spanId`.
fn new_span_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn nanos(d: Duration) -> u64 {
    d.as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;

    /// The exporter is process-global, so every test that touches it runs alone.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// A loopback OTLP/HTTP endpoint that records the body of every POST.
    ///
    /// A hand-rolled listener rather than a test framework: the point is to
    /// assert what arrives on the wire, and this keeps the test dependency-free
    /// and strictly on 127.0.0.1.
    struct Collector {
        endpoint: String,
        bodies: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Collector {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    /// `stall` = accept the connection, read the body, then never answer, which
    /// is how a stalled collector behaves from the client's side.
    fn collector(stall: bool) -> Collector {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (b, s) = (bodies.clone(), stop.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let body = read_body(&mut stream);
                        if stall {
                            std::thread::sleep(Duration::from_secs(30));
                        } else {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                  Content-Length: 2\r\nConnection: close\r\n\r\n{}",
                            );
                        }
                        if let Some(body) = body {
                            b.lock().unwrap_or_else(|e| e.into_inner()).push(body);
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Collector {
            endpoint: format!("http://127.0.0.1:{}/v1/traces", addr.port()),
            bodies,
            stop,
        }
    }

    impl Collector {
        fn requests(&self) -> Vec<String> {
            self.bodies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        /// Wait for `want` spans to arrive, then return them, flattened out of
        /// whatever batches they came in.
        fn spans(&self, want: usize) -> Vec<Value> {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                let spans = self
                    .requests()
                    .iter()
                    .filter_map(|b| serde_json::from_str::<Value>(b).ok())
                    .filter_map(|v| {
                        v["resourceSpans"][0]["scopeSpans"][0]["spans"]
                            .as_array()
                            .cloned()
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                if spans.len() >= want || std::time::Instant::now() > deadline {
                    return spans;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn read_body(stream: &mut TcpStream) -> Option<String> {
        stream
            .set_read_timeout(Some(Duration::from_millis(2000)))
            .ok()?;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
        let len = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == "content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let end = (head_end + len).min(buf.len());
        Some(String::from_utf8_lossy(&buf[head_end..end]).to_string())
    }

    /// Attribute `key -> value`, with `intValue` un-stringified for comparison.
    fn attrs(span: &Value) -> Map<String, Value> {
        let mut out = Map::new();
        for a in span["attributes"].as_array().into_iter().flatten() {
            let key = a["key"].as_str().unwrap_or_default().to_string();
            let v = &a["value"];
            let value = if let Some(s) = v["stringValue"].as_str() {
                Value::String(s.to_string())
            } else if let Some(n) = v["intValue"].as_str() {
                json!(n.parse::<u64>().expect("intValue is a decimal string"))
            } else {
                v.clone()
            };
            out.insert(key, value);
        }
        out
    }

    fn hex_of(s: &Value, len: usize) -> bool {
        s.as_str()
            .is_some_and(|s| s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit()))
    }

    /// Turn export off and forget the previous test's counters.
    fn off() {
        configure("").expect("off");
        DROPPED.store(0, Ordering::Relaxed);
    }

    #[test]
    fn an_empty_endpoint_starts_nothing_and_emits_nothing() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let c = collector(false);
        off();
        assert!(!enabled(), "no endpoint means export is off");
        {
            let st = lock();
            assert!(
                st.sender.is_none(),
                "an off exporter must have no queue and no thread"
            );
            assert!(st.error.is_none());
            assert_eq!(st.endpoint, "");
        }
        assert_eq!(status().dropped, 0);
        for _ in 0..50 {
            model_request(&ModelRequest {
                provider: "anthropic",
                model: "claude-sonnet-4",
                input_tokens: Some(100),
                output_tokens: Some(10),
                duration: Duration::from_millis(5),
                error: false,
            });
            tool_call(&ToolCall {
                tool: "read_file",
                duration: Duration::from_millis(1),
                error: false,
            });
        }
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            c.requests().is_empty(),
            "an off exporter sent something to a configured-looking collector"
        );
        assert_eq!(status().dropped, 0);
    }

    #[test]
    fn a_configured_endpoint_receives_well_formed_otlp_json() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let c = collector(false);
        configure(&c.endpoint).expect("configure");
        assert!(enabled());

        model_request(&ModelRequest {
            provider: "anthropic",
            model: "claude-sonnet-4",
            input_tokens: Some(1234),
            output_tokens: Some(567),
            duration: Duration::from_millis(250),
            error: false,
        });
        tool_call(&ToolCall {
            tool: "read_file",
            duration: Duration::from_millis(30),
            error: false,
        });

        let spans = c.spans(2);
        assert_eq!(
            spans.len(),
            2,
            "one span per model request and one per tool call"
        );

        // Resource and scope, from a real request body.
        let doc: Value = serde_json::from_str(&c.requests()[0]).expect("OTLP JSON");
        let rs = &doc["resourceSpans"][0];
        let service = rs["resource"]["attributes"]
            .as_array()
            .expect("resource attributes")
            .iter()
            .find(|a| a["key"] == "service.name")
            .expect("service.name");
        assert_eq!(service["value"]["stringValue"], SERVICE_NAME);
        assert_eq!(rs["scopeSpans"][0]["scope"]["name"], SCOPE_NAME);
        assert_eq!(
            rs["scopeSpans"][0]["scope"]["version"],
            env!("CARGO_PKG_VERSION")
        );

        let model = spans
            .iter()
            .find(|s| s["name"] == "chat claude-sonnet-4")
            .expect("model request span");
        assert_eq!(model["kind"], KIND_CLIENT);
        let a = attrs(model);
        assert_eq!(a["gen_ai.operation.name"], "chat");
        assert_eq!(a["gen_ai.provider.name"], "anthropic");
        assert_eq!(a["gen_ai.request.model"], "claude-sonnet-4");
        assert_eq!(a["gen_ai.usage.input_tokens"], 1234);
        assert_eq!(a["gen_ai.usage.output_tokens"], 567);
        assert_eq!(model["status"]["code"], STATUS_UNSET);
        assert!(
            hex_of(&model["traceId"], 32),
            "traceId: {}",
            model["traceId"]
        );
        assert!(hex_of(&model["spanId"], 16), "spanId: {}", model["spanId"]);
        let start: u64 = model["startTimeUnixNano"]
            .as_str()
            .expect("nanos as a decimal string")
            .parse()
            .expect("decimal");
        let end: u64 = model["endTimeUnixNano"]
            .as_str()
            .expect("nanos as a decimal string")
            .parse()
            .expect("decimal");
        let ms = (end - start) / 1_000_000;
        assert!(
            (200..=400).contains(&ms),
            "the span should carry the request's own duration, got {ms} ms"
        );

        let tool = spans
            .iter()
            .find(|s| s["name"] == "execute_tool read_file")
            .expect("tool span");
        assert_eq!(tool["kind"], KIND_INTERNAL);
        let t = attrs(tool);
        assert_eq!(t["gen_ai.operation.name"], "execute_tool");
        assert_eq!(t["gen_ai.tool.name"], "read_file");

        off();
    }

    #[test]
    fn a_dead_endpoint_neither_fails_nor_slows_a_turn() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // 203.0.113.0/24 is TEST-NET-3 (RFC 5737): reserved, so nothing answers
        // and nothing routes. Export must not notice.
        configure("http://203.0.113.1:4318/v1/traces").expect("configure");
        assert!(
            enabled(),
            "a configured endpoint is configured, reachable or not"
        );
        let t0 = std::time::Instant::now();
        for _ in 0..5000 {
            model_request(&ModelRequest {
                provider: "openai",
                model: "gpt-5",
                input_tokens: Some(9),
                output_tokens: Some(4),
                duration: Duration::from_millis(500),
                error: false,
            });
            tool_call(&ToolCall {
                tool: "bash",
                duration: Duration::from_millis(20),
                error: true,
            });
        }
        let ms = t0.elapsed().as_millis();
        // Reaching here is "the turn completed": neither emit returns a result,
        // so a failure would be a panic or a blocked caller. The bound is far
        // below a blocking export's first connect timeout.
        assert!(
            ms < 1000,
            "export to a dead endpoint delayed a turn by {ms} ms"
        );
        off();
    }

    #[test]
    fn a_stalled_collector_fills_the_queue_and_drops_instead() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let c = collector(true);
        configure(&c.endpoint).expect("configure");
        let total = (QUEUE_BOUND * 3) as u64;
        let t0 = std::time::Instant::now();
        for _ in 0..total {
            tool_call(&ToolCall {
                tool: "bash",
                duration: Duration::from_millis(1),
                error: false,
            });
        }
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "a stalled collector must not block the caller"
        );
        let dropped = status().dropped;
        assert!(dropped > 0, "a full queue must drop, not grow");
        // Everything not dropped is either in the queue or in the one batch the
        // exporter took out of it, so twice the queue bound is not reachable.
        let taken = total - dropped;
        assert!(
            taken <= (QUEUE_BOUND + MAX_BATCH + 8) as u64,
            "the queue grew past its bound: {taken} spans left it"
        );
        off();
    }

    #[test]
    fn spans_carry_names_counts_and_status_only() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let c = collector(false);
        configure(&c.endpoint).expect("configure");
        // A representative request: exactly what the call sites hold after a
        // model call and a tool call that failed.
        model_request(&ModelRequest {
            provider: "anthropic",
            model: "claude-sonnet-4",
            input_tokens: Some(100),
            output_tokens: Some(10),
            duration: Duration::from_millis(12),
            error: false,
        });
        tool_call(&ToolCall {
            tool: "read_file",
            duration: Duration::from_millis(3),
            error: true,
        });
        let spans = c.spans(2);
        // At least the two this test emitted — deliberately not an exact count.
        // The exporter is process-global and the rest of the suite runs in
        // parallel, so any other test that drives a real turn emits spans into
        // this same collector (the model-request and tool-call call sites in
        // `router.rs`/`runner.rs`). The property under test is what a span
        // *carries*, which is per-span and unaffected by how many arrive.
        assert!(
            spans.len() >= 2,
            "expected our two spans, got {}",
            spans.len()
        );

        // Every attribute this module can emit, pinned. A new one has to be
        // added here deliberately, which is the review this feature needs.
        let mut keys: Vec<String> = spans
            .iter()
            .flat_map(|s| attrs(s).keys().cloned().collect::<Vec<_>>())
            .collect();
        keys.sort();
        keys.dedup();
        assert_eq!(
            keys,
            vec![
                "error.type",
                "gen_ai.operation.name",
                "gen_ai.provider.name",
                "gen_ai.request.model",
                "gen_ai.tool.name",
                "gen_ai.usage.input_tokens",
                "gen_ai.usage.output_tokens",
            ]
        );

        let raw = c.requests().join("\n");
        // The semconv's content attributes are opt-in and must never appear.
        for banned in [
            "gen_ai.input.messages",
            "gen_ai.output.messages",
            "gen_ai.system_instructions",
            "gen_ai.tool.call.arguments",
            "gen_ai.tool.call.result",
            "gen_ai.tool.description",
            "gen_ai.prompt",
        ] {
            assert!(!raw.contains(banned), "{banned} must never be exported");
        }
        // And nothing shaped like a credential or like prompt text.
        for sentinel in [
            "sk-ant-",
            "sk-proj-",
            "AIza",
            "ghp_",
            "Bearer",
            "Authorization",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "-----BEGIN",
            "password",
            "api_key",
            "You are a helpful assistant",
        ] {
            assert!(
                !raw.contains(sentinel),
                "the exported payload contains {sentinel:?}"
            );
        }

        off();
    }

    #[test]
    fn an_endpoint_that_is_not_http_is_refused_with_a_clear_message() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        off();
        let err = configure("ftp://example.com/v1/traces").unwrap_err();
        assert!(err.contains("ftp"), "the scheme must be named: {err}");
        assert!(err.contains("http"), "the fix must be named: {err}");
        assert!(!enabled(), "a refused endpoint must not export");
        assert_eq!(status().error.as_deref(), Some(err.as_str()));
        assert_eq!(status().endpoint, "ftp://example.com/v1/traces");

        // A missing scheme is the mistake a user actually makes.
        let err = configure("localhost:4318").unwrap_err();
        assert!(err.contains("localhost:4318"), "{err}");
        assert!(err.contains(SUGGESTED_ENDPOINT), "{err}");
        assert!(!enabled());

        // A URL is not a place to keep a credential.
        let err = configure("http://user:pw@example.com/v1/traces").unwrap_err();
        assert!(err.contains("credentials"), "{err}");
        assert!(!enabled());

        // Non-empty, refused, and then turned off: no error left over.
        configure("").expect("empty is the off switch, not an error");
        let st = status();
        assert!(!st.enabled);
        assert!(st.error.is_none(), "a cleared endpoint keeps no error");
    }

    #[test]
    fn configuring_the_same_endpoint_again_does_not_restart_the_exporter() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let c = collector(false);
        off();
        let before = SPAWNS.load(Ordering::Relaxed);
        configure(&c.endpoint).expect("configure");
        configure(&c.endpoint).expect("configure again");
        configure(&format!(" {} ", c.endpoint)).expect("whitespace is not a new endpoint");
        assert_eq!(
            SPAWNS.load(Ordering::Relaxed),
            before + 1,
            "only the first configure may start an exporter"
        );
        configure("").expect("off");
        configure("").expect("off again");
        assert_eq!(
            SPAWNS.load(Ordering::Relaxed),
            before + 1,
            "turning an off exporter off again must not start one"
        );
    }

    #[test]
    fn validate_accepts_the_suggested_endpoint_and_rejects_the_rest() {
        assert!(validate(SUGGESTED_ENDPOINT).is_ok());
        assert!(validate("").is_ok(), "empty is the off switch");
        assert!(validate("http://192.168.1.9:4318/v1/traces").is_ok());
        assert!(validate("https://collector.internal/v1/traces").is_ok());
        assert!(validate("grpc://localhost:4317").is_err());
        assert!(validate("file:///tmp/traces").is_err());
        assert!(validate("localhost:4318/v1/traces").is_err());
        assert!(validate("http://").is_err());
    }
}
