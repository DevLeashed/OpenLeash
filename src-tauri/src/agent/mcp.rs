//! MCP client (JSON-RPC 2.0) over two transports: stdio and streamable HTTP.
//! Exposes server tools to the model as `mcp__<server>__<tool>`.
//!
//! stdio runs the configured command as a child and speaks newline-delimited
//! JSON. Streamable HTTP POSTs to a remote endpoint and reads back either one
//! JSON body or an SSE stream, which is how nearly every hosted MCP server
//! (Context7, Linear, Figma, Sentry…) is published.
//!
//! Two properties of this client are load-bearing and easy to "fix" into a
//! bug, so they are stated here:
//!
//! * It only ever POSTs. It never opens the `GET` server→client subscriptions
//!   stream, and it never calls `resources/subscribe` or a `…/list_changed`
//!   notification. So a *sessionless* server — one that never mints an
//!   `Mcp-Session-Id` and rejects that stream (GitHub's `api.githubcopilot.com/mcp`
//!   is the common one) — works without configuration: the session id is
//!   optional in both directions, and when a server never sends one none is
//!   echoed. There is nothing here to hang on.
//!
//! * The `mcp__<server>__<tool>` name is parsed with a single first-`__` split
//!   ([`split_tool`]), and that string is simultaneously what the model sees,
//!   what a permission rule is written against, and what routes the call back
//!   to a server. So a server name must not contain `_` at all: see
//!   [`server_name_error`], which rejects such names before a server is started.

use super::store::{AllowRule, McpServerCfg};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tokio_util::sync::CancellationToken;

/// The MCP protocol revision we negotiate. Servers are expected to answer with
/// their own version, and we echo theirs back on every later request.
const PROTOCOL: &str = "2025-06-18";

/// How this server is reached. Both share the JSON-RPC plumbing above them.
enum Transport {
    Stdio {
        stdin: Mutex<ChildStdin>,
        child: Mutex<Child>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        headers: Vec<(String, String)>,
        session: Mutex<Option<String>>,
        oauth: Option<McpServerCfg>,
    },
}

struct Conn {
    transport: Transport,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next: AtomicU64,
    tools: Vec<Value>,
}

impl Conn {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_or(method, params, None).await
    }

    /// `request`, but a stop cuts it short. The wait is on a `oneshot`, so it has
    /// to be in a `select!` with the token: a timeout alone would leave a chat
    /// sitting on a dead MCP server for two minutes after the user hit Stop.
    /// The server itself is shared and stays up — only this call is abandoned,
    /// and the late reply is dropped when it lands.
    async fn request_or(
        &self,
        method: &str,
        params: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        if let Err(e) = self.write(&msg).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        let v = match cancel {
            None => tokio::time::timeout(std::time::Duration::from_secs(120), rx).await.map_err(|_| format!("{method} timed out"))?,
            Some(tok) => {
                tokio::select! {
                    v = tokio::time::timeout(std::time::Duration::from_secs(120), rx) => v.map_err(|_| format!("{method} timed out"))?,
                    _ = tok.cancelled() => {
                        self.pending.lock().await.remove(&id);
                        return Err("Interrupted by user.".into());
                    }
                }
            }
        }
        .map_err(|_| "server closed".to_string())?;
        if let Some(e) = v.get("error") {
            return Err(e["message"].as_str().unwrap_or("MCP error").to_string());
        }
        Ok(v["result"].clone())
    }

    /// Fire-and-forget: notifications get no reply, so there is nothing to wait
    /// on. A failure is logged and dropped — the spec has no error channel for
    /// them, and treating one as fatal would break servers that ignore it.
    async fn notify(&self, method: &str, params: Value) {
        if let Err(e) = self
            .write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
        {
            eprintln!("mcp notify {method} failed: {e}");
        }
    }

    async fn write(&self, msg: &Value) -> Result<(), String> {
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut s = stdin.lock().await;
                s.write_all(format!("{msg}\n").as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                s.flush().await.map_err(|e| e.to_string())
            }
            // A notification has no id and therefore nothing to wait for, so the
            // response is dropped rather than read — a server that answers one
            // with an open SSE stream would otherwise hold the call hostage.
            Transport::Http { .. } => self.http_post(msg["id"].as_u64(), msg).await,
        }
    }

    /// POST one JSON-RPC message and route whatever comes back into the pending
    /// table, so `request_or` sees both transports the same way.
    async fn http_post(&self, id: Option<u64>, msg: &Value) -> Result<(), String> {
        let Transport::Http {
            client,
            url,
            headers,
            session,
            oauth,
        } = &self.transport
        else {
            unreachable!("http_post on a stdio server")
        };
        let mut req = client
            .post(url)
            .header("content-type", "application/json")
            // A streamable-HTTP server is allowed to answer either way, so the
            // client has to say it takes both.
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL)
            .json(msg)
            .timeout(std::time::Duration::from_secs(120));
        for (k, v) in headers {
            if oauth.is_some() && k.eq_ignore_ascii_case("authorization") {
                continue;
            }
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(cfg) = oauth {
            req = req.bearer_auth(super::mcp_oauth::access(cfg).await?);
        }
        if let Some(sid) = session.lock().await.as_ref() {
            req = req.header("mcp-session-id", sid.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();

        // The session id is minted on the initialize response and has to ride
        // on every later request, so capture it before the body is consumed.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *session.lock().await = Some(sid.to_string());
        }

        // 202/204 means "accepted, no content".
        if status.as_u16() == 202 || status.as_u16() == 204 {
            return Ok(());
        }
        if !status.is_success() {
            return Err(format!("MCP HTTP request rejected ({})", status.as_u16()));
        }
        let Some(id) = id else { return Ok(()) };

        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let reply = if ct.contains("text/event-stream") {
            self.read_sse(resp, id).await?
        } else {
            let text = resp.text().await.map_err(|e| format!("read failed: {e}"))?;
            serde_json::from_str::<Value>(&text).ok()
        };
        if let Some(v) = reply {
            if let Some(tx) = self.pending.lock().await.remove(&id) {
                let _ = tx.send(v);
            }
        }
        Ok(())
    }

    /// Walk the SSE body, dispatching every `data:` payload into the pending
    /// table. Stops at the first message carrying `id` — a server is allowed to
    /// keep the stream open afterwards for server-initiated messages, and
    /// waiting for the stream to close would hang until the timeout.
    async fn read_sse(&self, resp: reqwest::Response, id: u64) -> Result<Option<Value>, String> {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut reply = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream failed: {e}"))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // Events are separated by a blank line; a chunk can hold several,
            // or half of one, so everything already complete is drained each pass.
            while let Some(pos) = find_event_end(&buf) {
                let raw = buf[..pos].to_string();
                buf = buf[pos..].trim_start_matches(['\r', '\n']).to_string();
                let event = sse_event(&raw);
                let Some(data) = event.get("data") else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if v["id"].as_u64() == Some(id) {
                    reply = Some(v);
                } else if let Some(rid) = v["id"].as_u64() {
                    // Anything else on the stream is a server-initiated
                    // message. Nobody is waiting on it, so it is dropped.
                    self.pending.lock().await.remove(&rid);
                }
            }
            if reply.is_some() {
                break;
            }
        }
        Ok(reply)
    }

    async fn shutdown(&self) {
        if let Transport::Stdio { child, .. } = &self.transport {
            let _ = child.lock().await.start_kill();
        }
    }
}

/// Index of the blank line that ends the next SSE event, if the buffer holds a
/// complete one. `\r\n\r\n`, `\n\n` and `\r\r` all count.
fn find_event_end(s: &str) -> Option<usize> {
    let a = s.find("\n\n").map(|i| (i, 2));
    let b = s.find("\r\n\r\n").map(|i| (i, 4));
    let c = s.find("\r\r").map(|i| (i, 2));
    [a, b, c]
        .into_iter()
        .flatten()
        .min_by_key(|(i, len)| *i + len)
        .map(|(i, _)| i)
}

/// Collect one SSE block's `field: value` lines into a map.
fn sse_event(raw: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut data: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') || line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.strip_prefix(' ').unwrap_or(v);
        // `data` repeats and the payload is their newline join; any other
        // repeated field keeps its last value, which is what event-stream says.
        if k == "data" {
            data.push(v);
        } else {
            out.insert(k.to_string(), v.to_string());
        }
    }
    if !data.is_empty() {
        out.insert("data".into(), data.join("\n"));
    }
    out
}

#[derive(Serialize, Clone)]
pub struct McpStatus {
    pub name: String,
    pub status: String,
    pub tools: usize,
    pub error: Option<String>,
    /// The command or URL this server was started from, so the UI doesn't have
    /// to rebuild it from the config fields it happens to know about.
    pub desc: String,
    /// The tool names the server actually offers, so Settings can list real
    /// names to pre-approve instead of asking the user to type one blind.
    pub tool_names: Vec<String>,
}

#[derive(Default)]
pub struct McpManager {
    conns: Mutex<HashMap<String, Arc<Conn>>>,
    status: Mutex<HashMap<String, McpStatus>>,
    identities: Mutex<HashMap<String, McpServerCfg>>,
}

impl McpManager {
    pub async fn sync(&self, cfgs: &[McpServerCfg]) {
        // Stop servers that were removed or disabled.
        let wanted: Vec<&str> = cfgs
            .iter()
            .filter(|c| c.enabled)
            .map(|c| c.name.as_str())
            .collect();
        let mut identities = self.identities.lock().await;
        let mut conns = self.conns.lock().await;
        let stale: Vec<String> = conns
            .keys()
            .filter(|k| {
                !wanted.contains(&k.as_str())
                    || !cfgs.iter().any(|cfg| {
                        identities
                            .get(*k)
                            .is_some_and(|old| super::mcp_oauth::same_identity(old, cfg))
                    })
            })
            .cloned()
            .collect();
        for k in stale {
            identities.remove(&k);
            if let Some(c) = conns.remove(&k) {
                c.shutdown().await;
            }
        }
        drop(conns);
        drop(identities);
        let mut st = self.status.lock().await;
        st.retain(|k, _| cfgs.iter().any(|c| &c.name == k));
        for c in cfgs.iter().filter(|c| !c.enabled) {
            st.insert(
                c.name.clone(),
                McpStatus {
                    name: c.name.clone(),
                    status: "off".into(),
                    tools: 0,
                    error: None,
                    desc: c.describe(),
                    tool_names: vec![],
                },
            );
        }
        drop(st);
        for c in cfgs.iter().filter(|c| c.enabled) {
            if self.conns.lock().await.contains_key(&c.name) {
                continue;
            }
            // A name the `mcp__server__tool` delimiter cannot carry, or an
            // auto-approve entry that is not a plain tool name, is refused
            // before anything is started: the wire name would be ambiguous, so a
            // permission rule written against it could cover another server's
            // tool and the call could be routed to the wrong server entirely.
            // Failing loudly here (status = error) beats silently mis-routing.
            if let Some(error) = config_error(c) {
                self.status.lock().await.insert(
                    c.name.clone(),
                    McpStatus {
                        name: c.name.clone(),
                        status: "error".into(),
                        tools: 0,
                        error: Some(error),
                        desc: c.describe(),
                        tool_names: vec![],
                    },
                );
                continue;
            }
            self.status.lock().await.insert(
                c.name.clone(),
                McpStatus {
                    name: c.name.clone(),
                    status: "starting".into(),
                    tools: 0,
                    error: None,
                    desc: c.describe(),
                    tool_names: vec![],
                },
            );
            let res = start(c).await;
            let mut st = self.status.lock().await;
            match res {
                Ok(conn) => {
                    st.insert(
                        c.name.clone(),
                        McpStatus {
                            name: c.name.clone(),
                            status: "connected".into(),
                            tools: conn.tools.len(),
                            error: None,
                            desc: c.describe(),
                            tool_names: conn
                                .tools
                                .iter()
                                .filter_map(|t| t["name"].as_str().map(String::from))
                                .collect(),
                        },
                    );
                    self.identities
                        .lock()
                        .await
                        .insert(c.name.clone(), c.clone());
                    self.conns
                        .lock()
                        .await
                        .insert(c.name.clone(), Arc::new(conn));
                }
                Err(e) => {
                    st.insert(
                        c.name.clone(),
                        McpStatus {
                            name: c.name.clone(),
                            status: "error".into(),
                            tools: 0,
                            error: Some(e),
                            desc: c.describe(),
                            tool_names: vec![],
                        },
                    );
                }
            }
        }
    }

    pub async fn statuses(&self) -> Vec<McpStatus> {
        self.status.lock().await.values().cloned().collect()
    }

    pub async fn tool_schemas(&self) -> Vec<Value> {
        let conns = self.conns.lock().await;
        let mut names: Vec<&String> = conns.keys().collect();
        names.sort(); // deterministic order keeps the prompt cache warm
        let mut out = vec![];
        for n in names {
            for t in &conns[n].tools {
                out.push(json!({
                    "name": wire_tool(n, t["name"].as_str().unwrap_or("")),
                    "description": t["description"].as_str().unwrap_or("").chars().take(1024).collect::<String>(),
                    "input_schema": t.get("inputSchema").cloned().unwrap_or(json!({"type":"object","properties":{}})),
                }));
            }
        }
        out
    }

    /// Call a tool, abandoning the wait the moment `cancel` fires. The MCP
    /// server is shared with the rest of the app and deliberately outlives a
    /// run, so a stop gives up on the answer rather than killing the server.
    pub async fn call(
        &self,
        full: &str,
        args: Value,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let (server, tool) = split_tool(full).ok_or_else(|| {
            if full.starts_with("mcp__") {
                "bad MCP tool name".to_string()
            } else {
                "not an MCP tool".to_string()
            }
        })?;
        let conn = {
            let conns = self.conns.lock().await;
            conns
                .iter()
                // Exact, not folded: a name that would need folding is refused
                // at sync (see `server_name_error`), so the connection map can
                // only hold names whose wire form is themselves.
                .find(|(k, _)| k.as_str() == server)
                .map(|(_, c)| c.clone())
        }
        .ok_or(format!("MCP server {server} is not connected"))?;
        let r = conn
            .request_or(
                "tools/call",
                json!({"name": tool, "arguments": args}),
                Some(cancel),
            )
            .await?;
        let text = r["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        if c["type"] == "text" {
                            c["text"].as_str().unwrap_or("").to_string()
                        } else {
                            format!("[{} content]", c["type"].as_str().unwrap_or("?"))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| r.to_string());
        if r["isError"].as_bool().unwrap_or(false) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

/// The wire name of one tool on one server. One function so the schema the
/// model sees and the name the gate is asked about can never drift apart.
///
/// A valid server name is ASCII alphanumeric or `-` (see [`server_name_error`]),
/// and a tool name is passed through as the server reported it, so the first
/// `__` in the result is always the separator.
pub(crate) fn wire_tool(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Split an `mcp__<server>__<tool>` name back into its two halves. `None` for
/// anything that is not an MCP tool name.
///
/// The split is at the *first* `__` after `mcp__`, and that is only correct
/// because a server name may not contain `_` at all — see [`server_name_error`],
/// which rejects one before the server is ever started. The tool half is left
/// whole on purpose: a tool's own name may contain `__`, and only the server
/// half has to be unambiguous.
pub(crate) fn split_tool(full: &str) -> Option<(&str, &str)> {
    full.strip_prefix("mcp__")?.split_once("__")
}

/// Why this server name cannot be carried in an `mcp__<server>__<tool>` name,
/// or `None` when it is fine.
///
/// This client used to fold a server name through `sanitize` (every character
/// that is not ASCII alphanumeric or `-` became `_`) before building the wire
/// name. That is what turns a `_` from merely unusual into a security problem:
/// the name is parsed back with a cut at the first `__` ([`split_tool`]), so
///
///   * a name containing `__` mis-splits — server `add__x`, tool `t`, comes back
///     as server `add`, tool `x__t`, so the call is routed to whatever server is
///     named `add` while the rule the gate checked was for `add__x`;
///   * a name *ending* in `_` mis-splits too — `add_` + `t` is `add___t`, whose
///     first `__` is one character early, giving `add` + `_t`;
///   * two names that differ only in a folded character collapse to one wire
///     name (`a.b` and `a b` both became `mcp__a_b__…`), so an allow rule
///     written for one silently covered the other.
///
/// So the rule is the strict one: a name is letters, digits and dashes, and then
/// the wire name is literally `mcp__<name>__<tool>` with the first `__` always
/// the separator. This is the principle that a naming choice in one layer must
/// not be able to silently break a rule in another, so a bad name is refused at
/// config load with an error the user can read — never quietly renamed, which
/// would just relocate the surprise.
///
/// Widening the check to reject anything outside `[A-Za-z0-9-]` (not just `_`)
/// also closes the `a.b` / `a b` collapse, at the cost of refusing a name that
/// needed folding in the first place — which is what the old `sanitize` was
/// papering over.
pub fn server_name_error(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some("MCP server name is empty.".into());
    }
    let bad = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-'));
    let Some(c) = bad else { return None };
    let why = if c == '_' {
        "an underscore collides with the `__` that separates the server from the tool".to_string()
    } else {
        format!("`{c}` is not allowed in an MCP tool name")
    };
    Some(format!(
        "MCP server name {name:?} can't be used: {why}. Use letters, digits and dashes (e.g. \"github\")."
    ))
}

/// Expand a server's `auto_approve` list into the exact allow rules the
/// permission gate already understands.
///
/// One entry is one MCP **tool name** on this server, and it expands to exactly
/// the rule the approval prompt itself offers when the user presses "always
/// allow" — `mcp__<server>__<tool> *`. That is the whole design: this adds no
/// second approval path and no new matching, it just writes the rules the user
/// would otherwise write by hand, one per tool, and nothing wider. A `*` in an
/// entry is refused rather than honoured, because a pattern whose star could
/// span the `__` separators is exactly the widening this must never do.
///
/// The rules are matched literally elsewhere (`permissions::check` compares the
/// whole pattern string), so an entry can only ever approve the one tool whose
/// name it spells out.
pub fn auto_approve_rules(cfg: &McpServerCfg) -> Result<Vec<AllowRule>, String> {
    if let Some(e) = server_name_error(&cfg.name) {
        return Err(e);
    }
    let mut out: Vec<AllowRule> = Vec::new();
    for raw in &cfg.auto_approve {
        let entry = raw.trim();
        if entry.is_empty() {
            return Err(format!(
                "MCP server {:?}: an auto-approve entry is empty.",
                cfg.name
            ));
        }
        if entry.contains('*') || entry.chars().any(char::is_whitespace) {
            return Err(format!(
                "MCP server {:?}: auto-approve entry {raw:?} is not a tool name — it must not contain a `*` or spaces. \
                 Pre-approval is per tool and exact; to pre-approve a whole server, list each of its tools.",
                cfg.name
            ));
        }
        if entry.starts_with("mcp__") {
            return Err(format!(
                "MCP server {:?}: auto-approve entry {raw:?} looks like a full tool name. Give the tool's own name \
                 (e.g. \"create_issue\") — it is scoped to this server for you.",
                cfg.name
            ));
        }
        out.push(AllowRule {
            pattern: format!("{} *", wire_tool(&cfg.name, entry)),
            project: String::new(),
        });
    }
    // Two spellings of the same tool must not become two identical rules.
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.pattern.clone()));
    Ok(out)
}

/// Everything about a server's config that makes it unusable before it is even
/// started. Reported as `status = "error"` so Settings shows it in the same
/// place as a connection failure, rather than a server that simply never
/// connects with no explanation.
fn config_error(cfg: &McpServerCfg) -> Option<String> {
    server_name_error(&cfg.name).or_else(|| auto_approve_rules(cfg).err())
}

/// Fold every server's `auto_approve` list into `Settings.allow`.
///
/// The gate (`permissions::check`) never learns these rules came from an MCP
/// config — they are ordinary `mcp__<server>__<tool> *` allow rules, matched by
/// the same code as a hand-typed one. That is the point: this adds no second
/// approval path, it just writes down the rules the user would otherwise write
/// by hand, one per named tool, and nothing wider.
///
/// **Add-only on purpose.** It runs at both settings boundaries —
/// `store::load_settings` (boot) and `settings_update` (every live edit) — so
/// running it again must never take a permission away: it only ever *adds* the
/// rule a configured entry is missing, and produces no duplicate when the rule
/// is already there (idempotent). Retracting a pre-approval is therefore an
/// explicit act, done where the user removes the entry (the MCP tab filters the
/// matching rule out of `allow` in the same save). A reconcile that deleted
/// rules absent from `auto_approve` would silently revoke a hand-typed
/// `mcp__gh__read *`, or every rule of a server whose list briefly failed to
/// parse — an un-askable permission loss, which is the failure this shape
/// avoids.
///
/// An invalid entry (see [`auto_approve_rules`]) contributes **nothing** for
/// that whole server: the visible error is the server's `status` in the MCP tab,
/// and until it is fixed none of its tools are pre-approved. Failing closed is
/// what keeps a typo from becoming a wider grant. Nothing is ever reordered.
pub fn sync_auto_approve(allow: &mut Vec<AllowRule>, mcp: &[McpServerCfg]) {
    let present: std::collections::HashSet<String> =
        allow.iter().map(|r| r.pattern.clone()).collect();
    let mut added: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cfg in mcp {
        match auto_approve_rules(cfg) {
            Ok(rules) => {
                for r in rules {
                    if !present.contains(&r.pattern) && added.insert(r.pattern.clone()) {
                        allow.push(r);
                    }
                }
            }
            Err(e) => eprintln!("[openleash] MCP auto-approve for {:?}: {e}", cfg.name),
        }
    }
}

async fn start(cfg: &McpServerCfg) -> Result<Conn, String> {
    if cfg.is_http() {
        start_http(cfg).await
    } else {
        start_stdio(cfg).await
    }
}

async fn start_stdio(cfg: &McpServerCfg) -> Result<Conn, String> {
    if cfg.command.trim().is_empty() {
        return Err("no command configured".into());
    }
    #[cfg(windows)]
    let mut cmd = {
        // npx/uvx are .cmd shims on Windows — go through cmd so they resolve.
        let mut c = Command::new("cmd");
        c.arg("/C").arg(&cfg.command);
        c.creation_flags(0x0800_0000);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .envs(&cfg.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Default::default();
    let p2 = pending.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(id) = v["id"].as_u64() {
                if v.get("result").is_some() || v.get("error").is_some() {
                    if let Some(tx) = p2.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                }
            }
        }
    });
    let mut conn = Conn {
        transport: Transport::Stdio {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
        },
        pending,
        next: AtomicU64::new(1),
        tools: vec![],
    };
    handshake(&mut conn).await?;
    Ok(conn)
}

async fn start_http(cfg: &McpServerCfg) -> Result<Conn, String> {
    let url = cfg.url.trim();
    if url.is_empty() {
        return Err("no URL configured".into());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must start with http:// or https://.".into());
    }
    let mut headers: Vec<(String, String)> = cfg
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if cfg.oauth {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
        headers.push((
            "authorization".into(),
            format!("Bearer {}", super::mcp_oauth::access(cfg).await?),
        ));
    }
    // A bare Authorization header is the common case; promote it from headers so
    // it is sent exactly once, ahead of anything the user typed.
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
    {
        if let Some(tok) = cfg
            .env
            .get("MCP_TOKEN")
            .or_else(|| cfg.headers.get("Authorization"))
        {
            headers.push((
                "authorization".into(),
                if tok.starts_with("Bearer ") {
                    tok.clone()
                } else {
                    format!("Bearer {tok}")
                },
            ));
        }
    }
    // Even an explicitly configured bearer token is compromised if it travels
    // over plain HTTP. Refuse the connection before handshake sends any bytes.
    if cleartext_http_has_authorization(url, &headers) {
        return Err(
            "Refusing to send an Authorization header over cleartext HTTP. Use HTTPS instead."
                .into(),
        );
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut conn = Conn {
        transport: Transport::Http {
            client,
            url: url.to_string(),
            headers,
            session: Mutex::new(None),
            oauth: cfg.oauth.then(|| cfg.clone()),
        },
        pending: Default::default(),
        next: AtomicU64::new(1),
        tools: vec![],
    };
    handshake(&mut conn).await?;
    Ok(conn)
}

fn cleartext_http_has_authorization(url: &str, headers: &[(String, String)]) -> bool {
    url.starts_with("http://")
        && headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && !value.trim().is_empty()
        })
}

fn initialize_params() -> Value {
    json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"openleash","version":env!("CARGO_PKG_VERSION")}})
}

/// `initialize` → `notifications/initialized` → `tools/list`, shared by both
/// transports. Fills the connection's tool list in place.
async fn handshake(conn: &mut Conn) -> Result<(), String> {
    conn.request("initialize", initialize_params()).await?;
    conn.notify("notifications/initialized", json!({})).await;
    let tools = conn.request("tools/list", json!({})).await?;
    conn.tools = tools["tools"].as_array().cloned().unwrap_or_default();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_reports_the_compiled_app_version() {
        let package: Value = serde_json::from_str(include_str!("../../../package.json")).unwrap();
        let params = initialize_params();
        assert_eq!(params["clientInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(params["clientInfo"]["version"], package["version"]);
        assert_eq!(params["clientInfo"]["name"], "openleash");
        assert_eq!(params["protocolVersion"], PROTOCOL);
    }

    #[tokio::test]
    async fn refuses_authorization_on_cleartext_http_before_connecting() {
        for (headers, env) in [
            (
                HashMap::from([("Authorization".to_string(), "Bearer secret".to_string())]),
                HashMap::new(),
            ),
            (
                HashMap::new(),
                HashMap::from([("MCP_TOKEN".to_string(), "secret".to_string())]),
            ),
        ] {
            let cfg = McpServerCfg {
                name: "insecure".into(),
                transport: "http".into(),
                url: "http://localhost/mcp".into(),
                headers,
                env,
                ..Default::default()
            };
            let err = match start_http(&cfg).await {
                Ok(_) => panic!("cleartext HTTP with authorization must be refused"),
                Err(err) => err,
            };
            assert!(err.contains("cleartext HTTP"), "unexpected error: {err}");
        }
    }

    #[test]
    fn authorization_is_never_sent_over_cleartext_http() {
        let auth = vec![("Authorization".to_string(), "Bearer secret".to_string())];
        let lower_case_auth = vec![("authorization".to_string(), "Bearer secret".to_string())];
        assert!(cleartext_http_has_authorization(
            "http://localhost/mcp",
            &auth
        ));
        assert!(cleartext_http_has_authorization(
            "http://localhost/mcp",
            &lower_case_auth
        ));
        assert!(!cleartext_http_has_authorization(
            "https://localhost/mcp",
            &auth
        ));
        assert!(!cleartext_http_has_authorization(
            "http://localhost/mcp",
            &[]
        ));
        assert!(!cleartext_http_has_authorization(
            "http://localhost/mcp",
            &[("authorization".to_string(), "  ".to_string())]
        ));
    }

    #[test]
    fn finds_the_blank_line_that_ends_an_event() {
        assert_eq!(find_event_end("data: 1\n\ndata: 2"), Some(7));
        assert_eq!(find_event_end("data: 1\r\n\r\ndata: 2"), Some(7));
        // A lone newline between fields is not a terminator, and a partial
        // event must not be mistaken for a complete one.
        assert_eq!(find_event_end("event: message\ndata: 1"), None);
        assert_eq!(find_event_end("data: 1\n"), None);
    }

    #[test]
    fn repeated_data_lines_join_with_newlines() {
        let e = sse_event("event: message\ndata: {\"a\":\ndata: 1}");
        assert_eq!(e.get("event").map(String::as_str), Some("message"));
        assert_eq!(e.get("data").map(String::as_str), Some("{\"a\":\n1}"));
    }

    #[test]
    fn comments_and_field_spaces_are_handled() {
        // A leading colon is a comment keep-alive, and the space after the
        // field name is optional padding, not part of the value.
        let e = sse_event(": ping\nevent:message\ndata:hello");
        assert_eq!(e.get("data").map(String::as_str), Some("hello"));
    }

    #[test]
    fn a_stdio_config_without_a_transport_field_is_stdio() {
        // Every settings.json written before the HTTP transport existed has to
        // keep meaning "run this command", not silently become a URL.
        let cfg: McpServerCfg =
            serde_json::from_str(r#"{"name":"gh","command":"npx","args":["-y"],"enabled":true}"#)
                .unwrap();
        assert!(!cfg.is_http());
        assert_eq!(cfg.describe(), "npx -y");
    }

    #[test]
    fn an_http_config_describes_itself_by_url() {
        let cfg: McpServerCfg = serde_json::from_str(r#"{"name":"ctx7","command":"","url":"https://mcp.context7.com/mcp","transport":"http","enabled":true}"#).unwrap();
        assert!(cfg.is_http());
        assert_eq!(cfg.describe(), "https://mcp.context7.com/mcp");
    }
}
