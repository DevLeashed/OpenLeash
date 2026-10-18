//! Remote control: drive this app from a phone or another computer's browser.
//!
//! The server hands out the app's own frontend and bridges it to the backend
//! over a WebSocket. A small shim (`shim.js`) is injected ahead of the bundle
//! and stands in for `window.__TAURI_INTERNALS__`, so the unmodified React app
//! runs in any browser: `invoke` becomes a WebSocket message, `listen` becomes
//! a server-side subscription.
//!
//! On the server, every invoke is handed to the main webview's own IPC entry
//! point (`Webview::on_message`, Tauri's supported hook for custom invoke
//! systems). That is what gives full parity without a second command table,
//! and why the security story is simple: a remote call passes exactly the ACL
//! checks a call from the main window passes, under the main window's
//! capability. The PC-control guard webview and its narrower capability are
//! untouched, since nothing here ever invokes as that window.
//!
//! What remote control does *not* do is make anything safer: a paired device is
//! the user. See `auth.rs` for how devices pair, and `docs/remote.md`.

pub mod auth;
pub mod http;
pub mod push;
pub mod ssh;
pub mod ssh_projects;
pub mod transfer;

use auth::{CodeCheck, ShortCode, Vault};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// The bridge script, served at `/__openleash/remote.js`.
const SHIM_JS: &str = include_str!("shim.js");
const LOGIN_HTML: &str = include_str!("login.html");
const LOGIN_JS: &str = include_str!("login.js");
const APPROVE_HTML: &str = include_str!("approve.html");
const APPROVE_JS: &str = include_str!("approve.js");
const LOGIN_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// Event the desktop UI listens to so it can show who is connected. Carries a
/// count only: the full status includes the pairing link.
pub const EVENT: &str = "ol://remote";
/// Largest WebSocket message accepted (pasted screenshots arrive as data URLs).
const MAX_WS_MESSAGE: usize = 32 * 1024 * 1024;

pub struct Remote {
    inner: Mutex<Inner>,
    tunnels: ssh::Tunnels,
    attention_listener: std::sync::atomic::AtomicBool,
}

struct Inner {
    vault: Vault,
    code: Option<ShortCode>,
    limiter: auth::Limiter,
    server: Option<Server>,
    clients: HashMap<u64, Client>,
    next_client: u64,
    /// Why the server is not running although it is enabled.
    error: String,
    /// Outstanding notification action tokens are memory-only.
    grants: push::Grants,
}

struct Server {
    stop: CancellationToken,
    addr: SocketAddr,
}

struct Client {
    session: String,
    label: String,
    ip: IpAddr,
    since: i64,
    kill: CancellationToken,
}

#[derive(Serialize, Clone, Debug)]
pub struct SessionView {
    pub id: String,
    pub label: String,
    pub created: i64,
    pub last_seen: i64,
    pub connected: usize,
    pub push: bool,
    pub push_details: bool,
    pub push_confirm_all: bool,
    pub push_questions: bool,
    pub push_done: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ClientView {
    pub session: String,
    pub label: String,
    pub ip: String,
    pub since: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct CodeView {
    pub code: String,
    pub expires_in: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Status {
    pub enabled: bool,
    pub lan: bool,
    pub port: u16,
    pub running: bool,
    pub error: String,
    /// Addresses the server can be reached at (no secrets).
    pub urls: Vec<String>,
    /// One-tap pairing link for the most useful address. A secret.
    pub pair_url: String,
    /// The pairing link as an SVG QR code.
    pub qr_svg: String,
    pub sessions: Vec<SessionView>,
    pub clients: Vec<ClientView>,
    pub code: Option<CodeView>,
    pub ntfy_server: String,
    pub ntfy_token_set: bool,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn cookie_name(port: u16) -> String {
    // Cookies are scoped by host, not port, so two instances on one machine
    // (a dev build next to the installed app) would otherwise clobber each
    // other's sessions.
    format!("ol_session_{port}")
}

impl Remote {
    pub fn load() -> Arc<Self> {
        let vault = Vault::load();
        vault.save();
        Arc::new(Self {
            inner: Mutex::new(Inner {
                vault,
                code: None,
                limiter: Default::default(),
                server: None,
                clients: HashMap::new(),
                next_client: 1,
                error: String::new(),
                grants: Default::default(),
            }),
            tunnels: Default::default(),
            attention_listener: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// App exit: close SSH tunnels and stop serving.
    pub fn shutdown(&self) {
        self.tunnels.disconnect_all();
        transfer::close_all();
        self.stop_server();
    }

    pub fn enabled(&self) -> bool {
        self.inner.lock().unwrap().vault.enabled
    }

    /// Start (or restart) the server to match the saved configuration.
    pub fn apply(self: &Arc<Self>, app: &AppHandle) {
        self.listen_attention(app);
        self.stop_server();
        let (enabled, lan, port) = {
            let i = self.inner.lock().unwrap();
            (i.vault.enabled, i.vault.lan, i.vault.port)
        };
        if !enabled {
            self.inner.lock().unwrap().error.clear();
            return;
        }
        let ip = if lan {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        // Bind synchronously so a port conflict is reported to the caller
        // rather than discovered later inside a background task.
        let listener = match std::net::TcpListener::bind((ip, port)) {
            Ok(l) => l,
            Err(e) => {
                self.inner.lock().unwrap().error = format!("Couldn't listen on port {port}: {e}");
                return;
            }
        };
        let _ = listener.set_nonblocking(true);
        let addr = listener.local_addr().unwrap_or(SocketAddr::new(ip, port));
        let stop = CancellationToken::new();
        {
            let mut i = self.inner.lock().unwrap();
            i.error.clear();
            i.server = Some(Server {
                stop: stop.clone(),
                addr,
            });
        }
        let me = self.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    me.inner.lock().unwrap().error = format!("Couldn't start the server: {e}");
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    r = listener.accept() => {
                        let Ok((stream, peer)) = r else { continue };
                        let (me, app) = (me.clone(), app.clone());
                        tokio::spawn(async move { me.connection(app, stream, peer).await });
                    }
                }
            }
        });
    }

    /// Register the approval/completion notification listener once per Remote.
    /// `apply` calls this even when the server is disabled, allowing later toggles.
    pub fn listen_attention(self: &Arc<Self>, app: &AppHandle) {
        use std::sync::atomic::Ordering;
        if self.attention_listener.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = Arc::downgrade(self);
        let handle = app.clone();
        app.listen_any("ol://attention", move |event| {
            let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                return;
            };
            let task = payload["task_id"].as_str().unwrap_or("").to_string();
            let item = payload["item_id"].as_str().map(str::to_string);
            let kind = payload["kind"].as_str().unwrap_or("").to_string();
            if task.is_empty() {
                return;
            }
            let Some(me) = me.upgrade() else {
                return;
            };
            let app = handle.clone();
            tauri::async_runtime::spawn(async move {
                let Some(h) = app.try_state::<Arc<crate::agent::Harness>>() else {
                    return;
                };
                me.push_attention(&h, &task, item.as_deref(), &kind).await;
            });
        });
    }

    async fn push_attention(
        &self,
        h: &crate::agent::Harness,
        task: &str,
        item: Option<&str>,
        kind: &str,
    ) {
        let wants = |cfg: &auth::PushCfg| match kind {
            "approval" => true,
            "question" | "nonblocking" => cfg.questions,
            "done" | "failed" => cfg.done,
            _ => false,
        };
        let (chat, current) = match item {
            Some(item) => match push_find_item(h, task, item).await {
                Some((chat, value)) => (chat, Some(value)),
                None => return,
            },
            None => match h.task(task).await {
                Ok(t) => (t.lock().await.title.clone(), None),
                Err(_) => return,
            },
        };
        let (server, token, messages) = {
            let mut inner = self.inner.lock().unwrap();
            if inner.server.is_none() {
                return;
            }
            let server = push::server_of(&inner.vault);
            let token = inner.vault.ntfy.token.clone();
            let devices: Vec<(String, auth::PushCfg)> = inner
                .vault
                .sessions
                .iter()
                .filter_map(|s| s.push.clone().map(|cfg| (s.id.clone(), cfg)))
                .filter(|(_, cfg)| wants(cfg))
                .collect();
            let now = Instant::now();
            let messages = devices
                .into_iter()
                .map(|(sid, cfg)| {
                    let grant = if kind == "approval" {
                        item.map(|item| inner.grants.mint(&sid, &cfg.r#gen, task, item, now))
                    } else {
                        None
                    };
                    push::message(
                        &cfg,
                        task,
                        item.unwrap_or(""),
                        kind,
                        &chat,
                        current.as_ref(),
                        grant.as_deref(),
                    )
                })
                .collect::<Vec<_>>();
            (server, token, messages)
        };
        for message in messages {
            if let Err(error) = push::publish(&server, &token, &message).await {
                eprintln!("[remote] ntfy notification failed: {error}");
            }
        }
    }

    fn stop_server(&self) {
        let mut i = self.inner.lock().unwrap();
        if let Some(s) = i.server.take() {
            s.stop.cancel();
        }
        for c in i.clients.values() {
            c.kill.cancel();
        }
    }

    pub fn status(&self) -> Status {
        let i = self.inner.lock().unwrap();
        let v = &i.vault;
        let running = i.server.is_some();
        let port = i.server.as_ref().map(|s| s.addr.port()).unwrap_or(v.port);
        let mut urls = vec![format!("http://127.0.0.1:{port}")];
        let mut primary = urls[0].clone();
        if v.lan {
            if let Some(ip) = lan_ip() {
                primary = format!("http://{ip}:{port}");
                urls.insert(0, primary.clone());
            }
            if let Some(h) = hostname() {
                urls.push(format!("http://{h}:{port}"));
            }
        }
        let pair_url = format!("{primary}/pair?t={}", v.pair_token);
        let qr_svg = qr_svg(&pair_url);
        let sessions = v
            .sessions
            .iter()
            .map(|s| SessionView {
                id: s.id.clone(),
                label: s.label.clone(),
                created: s.created,
                last_seen: s.last_seen,
                connected: i.clients.values().filter(|c| c.session == s.id).count(),
                push: s.push.is_some(),
                push_details: s.push.as_ref().is_some_and(|p| p.details),
                push_confirm_all: s.push.as_ref().is_some_and(|p| p.confirm_all),
                push_questions: s.push.as_ref().is_some_and(|p| p.questions),
                push_done: s.push.as_ref().is_some_and(|p| p.done),
            })
            .collect();
        let mut clients: Vec<ClientView> = i
            .clients
            .values()
            .map(|c| ClientView {
                session: c.session.clone(),
                label: c.label.clone(),
                ip: c.ip.to_string(),
                since: c.since,
            })
            .collect();
        clients.sort_by_key(|c| c.since);
        let code = i.code.as_ref().and_then(|c| {
            let left = c.expires.checked_duration_since(Instant::now())?;
            (c.fails < auth::CODE_MAX_FAILS).then(|| CodeView {
                code: c.code.clone(),
                expires_in: left.as_secs(),
            })
        });
        Status {
            enabled: v.enabled,
            lan: v.lan,
            port,
            running,
            error: i.error.clone(),
            urls,
            pair_url,
            qr_svg,
            sessions,
            clients,
            code,
            ntfy_server: push::server_of(v),
            ntfy_token_set: !v.ntfy.token.is_empty(),
        }
    }

    pub fn configure(&self, enabled: bool, lan: bool, port: u16) -> Result<(), String> {
        if port != 0 && port < 1024 {
            return Err("Pick a port from 1024 to 65535.".into());
        }
        let mut i = self.inner.lock().unwrap();
        i.vault.enabled = enabled;
        i.vault.lan = lan;
        i.vault.port = if port == 0 { auth::DEFAULT_PORT } else { port };
        i.grants.forget_session(None);
        i.vault.save();
        Ok(())
    }

    pub fn new_code(&self) {
        self.inner.lock().unwrap().code = Some(ShortCode::new(Instant::now()));
    }

    fn kill_session_clients(i: &Inner, id: Option<&str>) {
        for c in i.clients.values() {
            if id.is_none_or(|id| c.session == id) {
                c.kill.cancel();
            }
        }
    }

    pub fn revoke(&self, id: &str) -> bool {
        let mut i = self.inner.lock().unwrap();
        let ok = i.vault.revoke(id);
        i.grants.forget_session(Some(id));
        i.vault.save();
        Self::kill_session_clients(&i, Some(id));
        ok
    }

    pub fn reset(&self) {
        let mut i = self.inner.lock().unwrap();
        i.vault.reset();
        i.code = None;
        i.grants.forget_session(None);
        i.vault.save();
        Self::kill_session_clients(&i, None);
    }

    /// Configure ntfy publishing. A server change without a replacement token
    /// clears the old bearer credential rather than sending it to a new origin.
    pub fn configure_ntfy(&self, server: &str, token: Option<&str>) -> Result<(), String> {
        let mut i = self.inner.lock().unwrap();
        push::configure_ntfy(&mut i.vault, server, token)?;
        i.grants.forget_session(None);
        i.vault.save();
        Ok(())
    }

    pub fn push_disable(&self, id: &str) -> bool {
        let mut i = self.inner.lock().unwrap();
        let ok = {
            let Inner { vault, grants, .. } = &mut *i;
            push::disable(vault, id, grants)
        };
        i.vault.save();
        ok
    }

    fn session_for(&self, req: &http::Request, port: u16) -> Option<String> {
        let secret = req.cookie(&cookie_name(port))?;
        let i = self.inner.lock().unwrap();
        i.vault.find_session(&secret).map(|s| s.id.clone())
    }

    fn port(&self) -> u16 {
        let i = self.inner.lock().unwrap();
        i.server
            .as_ref()
            .map(|s| s.addr.port())
            .unwrap_or(i.vault.port)
    }

    fn mint_session(&self, req: &http::Request) -> String {
        let mut i = self.inner.lock().unwrap();
        let label = auth::device_label(req.header("user-agent"));
        let (_, secret) = i.vault.create_session(&label, now());
        i.vault.save();
        format!(
            "{}={secret}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
            cookie_name(
                i.server
                    .as_ref()
                    .map(|s| s.addr.port())
                    .unwrap_or(i.vault.port)
            )
        )
    }

    async fn connection(self: Arc<Self>, app: AppHandle, mut stream: TcpStream, peer: SocketAddr) {
        let _ = stream.set_nodelay(true);
        let req = match tokio::time::timeout(Duration::from_secs(10), read_head(&stream)).await {
            Ok(Ok(r)) => r,
            Ok(Err(status)) => {
                let _ = stream
                    .write_all(&http::Response::text(status, "Bad request").to_bytes())
                    .await;
                return;
            }
            Err(_) => return,
        };
        let port = self.port();
        if req.is_websocket() {
            if req.path != "/__openleash/ws" {
                let _ = stream
                    .write_all(&http::Response::text(404, "Not found").to_bytes())
                    .await;
                return;
            }
            let session = self.session_for(&req, port);
            // A browser always sends Origin on a WebSocket handshake; requiring
            // it to name this very host is what stops another site the user has
            // open from riding the session cookie (cross-site WebSocket hijack).
            // SameSite=Strict already withholds the cookie there; this is the
            // second lock on the same door.
            let same_origin = origin_matches(req.header("origin"), req.header("host"));
            let Some(session) = session.filter(|_| same_origin) else {
                let _ = stream
                    .write_all(&http::Response::text(403, "Forbidden").to_bytes())
                    .await;
                return;
            };
            let label = auth::device_label(req.header("user-agent"));
            let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(MAX_WS_MESSAGE))
                .max_frame_size(Some(MAX_WS_MESSAGE));
            // The handshake reads the request itself: `read_head` only peeked.
            let Ok(ws) = tokio_tungstenite::accept_async_with_config(stream, Some(cfg)).await
            else {
                return;
            };
            self.client(app, ws, session, label, peer.ip()).await;
            return;
        }
        // Consume the head we peeked, then the (small) body if any.
        let mut head = vec![0u8; req.head_len];
        if stream.read_exact(&mut head).await.is_err() {
            return;
        }
        let len = req.content_length();
        let body_limit = match req.path.as_str() {
            "/__openleash/upload" => http::MAX_UPLOAD_BODY,
            "/__openleash/push/act" | "/__openleash/push/approval" => 16 * 1024,
            _ => http::MAX_BODY,
        };
        if len > body_limit {
            let _ = stream
                .write_all(&http::Response::text(413, "Too large").to_bytes())
                .await;
            return;
        }
        let mut body = vec![0u8; len];
        if len > 0
            && tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut body))
                .await
                .map(|r| r.is_err())
                .unwrap_or(true)
        {
            return;
        }
        let res = self.route(&app, &req, &body, peer.ip(), port).await;
        let _ = stream.write_all(&res.to_bytes()).await;
        let _ = stream.shutdown().await;
    }

    async fn route(
        &self,
        app: &AppHandle,
        req: &http::Request,
        body: &[u8],
        ip: IpAddr,
        port: u16,
    ) -> http::Response {
        let path = req.path.as_str();
        match path {
            "/__openleash/login.js" => {
                return http::Response::new(200)
                    .header("Content-Type", http::mime("x.js"))
                    .body(LOGIN_JS)
            }
            // Before the session check on purpose: see `library_response`.
            p if p == format!("/__openleash/viz/{}", crate::visualization::LIBRARY_PATH) => {
                return viz_library(req);
            }
            "/__openleash/push/approve" => {
                return http::Response::new(200)
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("Content-Security-Policy", LOGIN_CSP)
                    .body(APPROVE_HTML)
            }
            "/__openleash/push/approve.js" => {
                return http::Response::new(200)
                    .header("Content-Type", http::mime("x.js"))
                    .body(APPROVE_JS)
            }
            "/__openleash/push/act" => {
                if req.method != "POST" {
                    return http::Response::text(405, "POST only");
                }
                let Some(h) = app.try_state::<Arc<crate::agent::Harness>>() else {
                    return http::Response::text(503, "Not ready");
                };
                let body = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
                let caller = self.push_caller(req, port);
                return match self.push_act(&h, caller, &body).await {
                    Ok(d) => http::Response::json(200, &json!({"ok": true, "decision": d})),
                    Err(r) => push_refusal_response(r),
                };
            }
            "/pair" => {
                let token = req.query.get("t").cloned().unwrap_or_default();
                let ok = {
                    let mut i = self.inner.lock().unwrap();
                    let n = Instant::now();
                    if i.limiter.limited(ip, n) {
                        None
                    } else if auth::ct_eq(&token, &i.vault.pair_token) {
                        Some(true)
                    } else {
                        i.limiter.fail(ip, n);
                        Some(false)
                    }
                };
                return match ok {
                    Some(true) => {
                        let cookie = self.mint_session(req);
                        notify(app);
                        http::Response::redirect("/").header("Set-Cookie", cookie)
                    }
                    Some(false) => login_page(
                        403,
                        "That pairing link is no longer valid. Ask for a new one, or enter a code.",
                    ),
                    None => login_page(
                        429,
                        "Too many attempts from this device. Try again in a few minutes.",
                    ),
                };
            }
            "/__openleash/login" => {
                if req.method != "POST" {
                    return http::Response::text(405, "POST only");
                }
                if !origin_matches(req.header("origin"), req.header("host")) {
                    return http::Response::text(403, "Forbidden");
                }
                let guess = serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v["code"].as_str().map(str::to_string))
                    .unwrap_or_default();
                let check = {
                    let mut i = self.inner.lock().unwrap();
                    let Inner { code, limiter, .. } = &mut *i;
                    auth::check_code(code, limiter, ip, &guess, Instant::now())
                };
                return match check {
                    CodeCheck::Ok => {
                        let cookie = self.mint_session(req);
                        notify(app);
                        http::Response::json(200, &json!({"ok": true})).header("Set-Cookie", cookie)
                    }
                    CodeCheck::Wrong => {
                        http::Response::json(403, &json!({"error": "That code isn't right."}))
                    }
                    CodeCheck::NoCode => http::Response::json(
                        403,
                        &json!({"error": "No pairing code is active. Create one in Settings → Remote on the computer."}),
                    ),
                    CodeCheck::Limited => http::Response::json(
                        429,
                        &json!({"error": "Too many attempts. Try again in a few minutes."}),
                    ),
                };
            }
            _ => {}
        }
        let Some(session) = self.session_for(req, port) else {
            let wants_html = req.method == "GET"
                && (path == "/"
                    || path == "/index.html"
                    || req.header("accept").contains("text/html"));
            return if wants_html {
                login_page(200, "")
            } else {
                http::Response::text(401, "Not paired")
            };
        };
        match path {
            "/__openleash/upload" => {
                if req.method != "POST" || !origin_matches(req.header("origin"), req.header("host"))
                {
                    return http::Response::text(403, "Forbidden");
                }
                if req
                    .header("content-type")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    != "application/octet-stream"
                {
                    return http::Response::text(415, "Expected binary upload");
                }
                if body.len() > http::MAX_UPLOAD_BODY {
                    return http::Response::text(413, "Too large");
                }
                // Both headers are percent-encoded by the browser side: a task
                // id is a plain token, but a *project path* (the new-task
                // composer's scope) contains "/" and may contain non-ASCII, and
                // neither is a legal header value. Decoded below.
                let name = decode_header(req.header("x-openleash-filename"));
                let task_id = decode_header(req.header("x-openleash-task"));
                // A file name is one path component, so "/" and "\\" stay
                // rejected; a task id may legitimately be a path, so only control
                // characters are refused there.
                if task_id.is_empty()
                    || name.trim().is_empty()
                    || name.len() > 240
                    || name
                        .as_bytes()
                        .iter()
                        .any(|b| matches!(b, b'/' | b'\\' | b'\r' | b'\n'))
                    || task_id.len() > 1024
                    || task_id.as_bytes().iter().any(|b| *b < 0x20)
                {
                    return http::Response::text(400, "Invalid task or file name");
                }
                let args = json!({"filename": name, "id": task_id, "bytes": body});
                match remote_invoke(app, "remote_stage_upload", args).await {
                    Ok(value) => http::Response::json(200, &value),
                    Err(error) => http::Response::json(400, &json!({"error": error})),
                }
            }
            "/__openleash/remote.js" => http::Response::new(200)
                .header("Content-Type", http::mime("x.js"))
                .body(SHIM_JS),
            p if p.starts_with("/__openleash/push/") => {
                self.push_route(app, req, body, &session).await
            }
            "/__openleash/logout" => {
                if req.method != "POST" || !origin_matches(req.header("origin"), req.header("host"))
                {
                    return http::Response::text(403, "Forbidden");
                }
                self.revoke(&session);
                notify(app);
                http::Response::json(200, &json!({"ok": true})).header(
                    "Set-Cookie",
                    format!("{}=; Path=/; Max-Age=0", cookie_name(port)),
                )
            }
            p if p.starts_with("/__openleash/viz/") => {
                let id = &p["/__openleash/viz/".len()..];
                let request = tauri::http::Request::builder()
                    .method("GET")
                    .uri(format!("http://openleash-viz.localhost/{id}"))
                    .body(vec![])
                    .ok();
                match request {
                    Some(request) => {
                        let res = crate::visualization::respond(app, request);
                        let mut out = http::Response::new(res.status().as_u16());
                        for (k, v) in res.headers() {
                            // The native policy names the native scheme's library
                            // directory; this frame loads it from our own origin.
                            if k == "content-security-policy" {
                                out = out.header(
                                    k.as_str(),
                                    crate::visualization::csp(&remote_library_sources(
                                        req.header("host"),
                                    )),
                                );
                                continue;
                            }
                            out = out.header(k.as_str(), v.to_str().unwrap_or(""));
                        }
                        // The preview is framed by the app itself.
                        out.header("X-Frame-Options", "SAMEORIGIN")
                            .body(res.into_body())
                    }
                    None => http::Response::text(404, "Not found"),
                }
            }
            p if p.starts_with("/__openleash/") => http::Response::text(404, "Not found"),
            _ => {
                if req.method != "GET" && req.method != "HEAD" {
                    return http::Response::text(405, "GET only");
                }
                serve_asset(app, req).await
            }
        }
    }

    async fn client(
        self: Arc<Self>,
        app: AppHandle,
        ws: tokio_tungstenite::WebSocketStream<TcpStream>,
        session: String,
        label: String,
        ip: IpAddr,
    ) {
        let kill = CancellationToken::new();
        let id = {
            let mut i = self.inner.lock().unwrap();
            let id = i.next_client;
            i.next_client += 1;
            i.clients.insert(
                id,
                Client {
                    session: session.clone(),
                    label,
                    ip,
                    since: now(),
                    kill: kill.clone(),
                },
            );
            i.vault.touch(&session, now());
            i.vault.save();
            id
        };
        notify(&app);
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let writer_kill = kill.clone();
        let writer = tokio::spawn(async move {
            let mut ping = tokio::time::interval(Duration::from_secs(25));
            loop {
                tokio::select! {
                    _ = writer_kill.cancelled() => {
                        let _ = sink.send(Message::Text(json!({"t": "bye", "reason": "revoked"}).to_string().into())).await;
                        let _ = sink.close().await;
                        break;
                    }
                    _ = ping.tick() => { if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break } }
                    m = rx.recv() => {
                        let Some(m) = m else { break };
                        if sink.send(Message::Text(m.to_string().into())).await.is_err() { break }
                    }
                }
            }
        });
        let mut listeners: HashSet<tauri::EventId> = HashSet::new();
        loop {
            let msg = tokio::select! {
                _ = kill.cancelled() => break,
                m = stream.next() => m,
            };
            let text = match msg {
                Some(Ok(Message::Text(t))) => t.to_string(),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => continue,
            };
            let Ok(m) = serde_json::from_str::<WsIn>(&text) else {
                continue;
            };
            handle_invoke(&app, &tx, &mut listeners, m);
        }
        for l in listeners {
            app.unlisten(l);
        }
        kill.cancel();
        let _ = writer.await;
        self.inner.lock().unwrap().clients.remove(&id);
        notify(&app);
    }
}

#[derive(Deserialize)]
struct WsIn {
    id: u64,
    cmd: String,
    #[serde(default)]
    args: Value,
}

async fn remote_invoke(app: &AppHandle, cmd: &str, body: Value) -> Result<Value, String> {
    let wv = app
        .get_webview_window("main")
        .ok_or("The OpenLeash window is not available.")?;
    let url = wv
        .url()
        .map_err(|_| "The OpenLeash window is not available.")?;
    let request = tauri::webview::InvokeRequest {
        cmd: cmd.to_string(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url,
        body: tauri::ipc::InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: app.invoke_key().to_string(),
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    wv.on_message(
        request,
        Box::new(move |_wv, _cmd, response, _cb, _err| {
            use tauri::ipc::{InvokeResponse, InvokeResponseBody};
            let result = match response {
                InvokeResponse::Ok(InvokeResponseBody::Json(s)) => {
                    serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())
                }
                InvokeResponse::Ok(InvokeResponseBody::Raw(_)) => {
                    Err("Unexpected binary IPC response".into())
                }
                InvokeResponse::Err(e) => Err(e.0.to_string()),
            };
            let _ = tx.send(result);
        }),
    );
    rx.await
        .map_err(|_| "Upload handling was interrupted".to_string())?
}

fn handle_invoke(
    app: &AppHandle,
    tx: &mpsc::UnboundedSender<Value>,
    listeners: &mut HashSet<tauri::EventId>,
    m: WsIn,
) {
    let reply = |tx: &mpsc::UnboundedSender<Value>, r: Result<Value, Value>| {
        let msg = match r {
            Ok(data) => json!({"t": "result", "id": m.id, "ok": true, "data": data}),
            Err(error) => json!({"t": "result", "id": m.id, "ok": false, "error": error}),
        };
        let _ = tx.send(msg);
    };
    match m.cmd.as_str() {
        "plugin:event|listen" => {
            let event = m.args["event"].as_str().unwrap_or("").to_string();
            let handler = m.args["handler"].as_u64().unwrap_or(0);
            // Window-system events (`tauri://drag-drop`, focus, resize …) are
            // about the desktop window, not this browser. Forwarding a drag-drop
            // would attach files dropped on the desktop to the remote composer.
            if event.is_empty() || event.starts_with("tauri://") {
                return reply(tx, Err(json!("event not available remotely")));
            }
            let etx = tx.clone();
            let ev_name = event.clone();
            let id = app.listen_any(event, move |e| {
                let payload: Value = serde_json::from_str(e.payload()).unwrap_or(Value::Null);
                let _ = etx.send(json!({
                    "t": "event",
                    "handler": handler,
                    "event": {"event": ev_name, "id": e.id(), "payload": payload},
                }));
            });
            listeners.insert(id);
            reply(tx, Ok(json!(id)))
        }
        "plugin:event|unlisten" => {
            let id = m.args["eventId"].as_u64().unwrap_or(0) as tauri::EventId;
            // Only listeners this client registered: ids are small integers
            // and must not let one client tear down another's (or the app's).
            if listeners.remove(&id) {
                app.unlisten(id);
            }
            reply(tx, Ok(Value::Null))
        }
        "plugin:event|emit" | "plugin:event|emit_to" => {
            let event = m.args["event"].as_str().unwrap_or("").to_string();
            if event.is_empty() || event.starts_with("tauri://") {
                return reply(tx, Err(json!("event not available remotely")));
            }
            let _ = app.emit(&event, m.args["payload"].clone());
            reply(tx, Ok(Value::Null))
        }
        _ => {
            let Some(wv) = app.get_webview_window("main") else {
                return reply(tx, Err(json!("The OpenLeash window is not available.")));
            };
            let Ok(url) = wv.url() else {
                return reply(tx, Err(json!("The OpenLeash window is not available.")));
            };
            let body = if m.args.is_null() {
                json!({})
            } else {
                m.args.clone()
            };
            let request = tauri::webview::InvokeRequest {
                cmd: m.cmd.clone(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url,
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: app.invoke_key().to_string(),
            };
            let rtx = tx.clone();
            let id = m.id;
            wv.on_message(
                request,
                Box::new(move |_wv, _cmd, response, _cb, _err| {
                    use tauri::ipc::{InvokeResponse, InvokeResponseBody};
                    let msg = match response {
                        InvokeResponse::Ok(InvokeResponseBody::Json(s)) => json!({
                            "t": "result", "id": id, "ok": true,
                            "data": serde_json::from_str::<Value>(&s).unwrap_or(Value::Null),
                        }),
                        InvokeResponse::Ok(InvokeResponseBody::Raw(b)) => {
                            json!({"t": "result", "id": id, "ok": true, "data": b})
                        }
                        InvokeResponse::Err(e) => {
                            json!({"t": "result", "id": id, "ok": false, "error": e.0})
                        }
                    };
                    let _ = rtx.send(msg);
                }),
            );
        }
    }
}

async fn push_find_item(
    h: &crate::agent::Harness,
    task: &str,
    item: &str,
) -> Option<(String, crate::agent::Item)> {
    let task_ref = h.task(task).await.ok()?;
    let task_ref = task_ref.lock().await;
    let item = task_ref
        .items
        .iter()
        .find(|entry| entry.id == item)?
        .clone();
    Some((task_ref.title.clone(), item))
}

fn push_refusal_response(refusal: push::Refusal) -> http::Response {
    use push::Refusal;
    let (status, code, error) = match refusal {
        Refusal::BadToken => (403, "bad_token", "This link isn't valid for this request."),
        Refusal::Revoked => (
            403,
            "revoked",
            "This device can no longer answer from notifications.",
        ),
        Refusal::Expired => (410, "expired", "This notification has expired."),
        Refusal::Answered(_) => (409, "answered", "This request was already answered."),
        Refusal::ConfirmRequired => (
            403,
            "confirm_required",
            "This answer needs the confirm page.",
        ),
        Refusal::BadDecision => (
            400,
            "bad_decision",
            "That answer isn't offered for this request.",
        ),
    };
    http::Response::json(status, &json!({"code": code, "error": error}))
}

impl Remote {
    fn push_caller(&self, req: &http::Request, port: u16) -> push::Caller {
        match self.session_for(req, port) {
            Some(session) if origin_matches(req.header("origin"), req.header("host")) => {
                push::Caller::Session(session)
            }
            _ => push::Caller::Token,
        }
    }

    async fn push_act(
        &self,
        h: &crate::agent::Harness,
        caller: push::Caller,
        body: &Value,
    ) -> Result<String, push::Refusal> {
        let field = |name: &str| body[name].as_str().unwrap_or("").to_string();
        let (task, item, token, decision) = (
            field("task"),
            field("item"),
            field("token"),
            field("decision"),
        );
        if !matches!(decision.as_str(), "once" | "always" | "deny") {
            return Err(push::Refusal::BadDecision);
        }
        let cfg = {
            let inner = self.inner.lock().unwrap();
            push::check(
                &inner.vault,
                &inner.grants,
                &caller,
                &task,
                &item,
                &token,
                Instant::now(),
            )?
        };
        if caller == push::Caller::Token && (decision == "always" || cfg.confirm_all) {
            return Err(push::Refusal::ConfirmRequired);
        }
        let Some((_, current)) = push_find_item(h, &task, &item).await else {
            return Err(push::Refusal::Answered(None));
        };
        if current.kind != "approval" {
            return Err(push::Refusal::BadToken);
        }
        if decision == "always" && push::always_label(&current.data).is_none() {
            return Err(push::Refusal::BadDecision);
        }
        {
            let mut inner = self.inner.lock().unwrap();
            let cfg = {
                let Inner { vault, grants, .. } = &mut *inner;
                push::consume(vault, grants, &caller, &task, &item, &token, Instant::now())?
            };
            if caller == push::Caller::Token && (decision == "always" || cfg.confirm_all) {
                return Err(push::Refusal::ConfirmRequired);
            }
        }
        if current.data["resolved"].is_string() {
            return Err(push::Refusal::Answered(
                current.data["resolved"].as_str().map(str::to_string),
            ));
        }
        h.respond_pending(&task, &item, json!({"decision": decision}))
            .await
            .map_err(|_| push::Refusal::Answered(None))?;
        Ok(decision)
    }

    async fn push_approval(
        &self,
        h: &crate::agent::Harness,
        session: &str,
        body: &Value,
    ) -> http::Response {
        let task = body["task"].as_str().unwrap_or("");
        let item = body["item"].as_str().unwrap_or("");
        let token = body["token"].as_str().unwrap_or("");
        let status = {
            let inner = self.inner.lock().unwrap();
            match push::check(
                &inner.vault,
                &inner.grants,
                &push::Caller::Session(session.to_string()),
                task,
                item,
                token,
                Instant::now(),
            ) {
                Ok(_) => "pending",
                Err(push::Refusal::Answered(_)) => "answered",
                Err(push::Refusal::Expired) => "expired",
                Err(r) => return push_refusal_response(r),
            }
        };
        let Some((chat, current)) = push_find_item(h, task, item).await else {
            return http::Response::json(
                200,
                &json!({"status":"answered","chat":"","title":"","detail":"","kind":"","always":null,"resolved":null}),
            );
        };
        let pending = h.runtime(task).pending.lock().await.contains_key(item);
        let status = if status == "pending" && !pending {
            "answered"
        } else {
            status
        };
        http::Response::json(
            200,
            &json!({"status":status,"chat":chat,"title":current.data["title"].as_str().unwrap_or(&current.text),"detail":current.data["detail"].as_str().unwrap_or(""),"kind":current.data["kind"].as_str().unwrap_or(""),"always":push::always_label(&current.data),"resolved":current.data["resolved"]}),
        )
    }

    async fn push_route(
        &self,
        app: &AppHandle,
        req: &http::Request,
        body: &[u8],
        session: &str,
    ) -> http::Response {
        if req.method != "GET" && req.method != "POST" {
            return http::Response::text(405, "GET or POST only");
        }
        if req.method == "POST" && !origin_matches(req.header("origin"), req.header("host")) {
            return http::Response::text(403, "Forbidden");
        }
        let body = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
        if req.path == "/__openleash/push/state" && req.method == "GET" {
            return http::Response::json(
                200,
                &push::state_json(&self.inner.lock().unwrap().vault, session),
            );
        }
        match req.path.as_str() {
            "/__openleash/push/enable" if req.method == "POST" => {
                let mut inner = self.inner.lock().unwrap();
                let ok = {
                    let Inner { vault, grants, .. } = &mut *inner;
                    push::enable(
                        vault,
                        session,
                        req.header("origin"),
                        push::Options::from_json(&body),
                        now(),
                        Some(grants),
                    )
                };
                if !ok {
                    return http::Response::text(401, "Not paired");
                }
                inner.vault.save();
                http::Response::json(200, &push::state_json(&inner.vault, session))
            }
            "/__openleash/push/settings" if req.method == "POST" => {
                let mut inner = self.inner.lock().unwrap();
                let ok = {
                    let Inner { vault, grants, .. } = &mut *inner;
                    push::set_options(vault, session, push::Options::from_json(&body), grants)
                };
                if !ok {
                    return http::Response::json(
                        409,
                        &json!({"error":"Notifications are off on this device."}),
                    );
                }
                inner.vault.save();
                http::Response::json(200, &push::state_json(&inner.vault, session))
            }
            "/__openleash/push/disable" if req.method == "POST" => {
                self.push_disable(session);
                let inner = self.inner.lock().unwrap();
                http::Response::json(200, &push::state_json(&inner.vault, session))
            }
            "/__openleash/push/test" if req.method == "POST" => match self.push_test(session).await
            {
                Ok(()) => http::Response::json(200, &json!({"ok":true})),
                Err(error) => http::Response::json(502, &json!({"error":error})),
            },
            "/__openleash/push/approval" if req.method == "POST" => {
                let Some(h) = app.try_state::<Arc<crate::agent::Harness>>() else {
                    return http::Response::text(503, "Not ready");
                };
                self.push_approval(&h, session, &body).await
            }
            _ => http::Response::text(404, "Not found"),
        }
    }

    async fn push_test(&self, session: &str) -> Result<(), String> {
        let (server, token, cfg) = {
            let inner = self.inner.lock().unwrap();
            let cfg = inner
                .vault
                .sessions
                .iter()
                .find(|s| s.id == session)
                .and_then(|s| s.push.clone())
                .ok_or("Notifications are off on this device.")?;
            (
                push::server_of(&inner.vault),
                inner.vault.ntfy.token.clone(),
                cfg,
            )
        };
        push::publish(&server, &token, &json!({"topic":cfg.topic,"title":"OpenLeash","message":"Test notification. Approvals will arrive like this.","tags":["white_check_mark"],"click":format!("{}/",cfg.base_url)})).await
    }
}

/// Tell the desktop UI the set of paired/connected devices changed.
fn notify(app: &AppHandle) {
    let n = app
        .try_state::<Arc<Remote>>()
        .map(|r| r.inner.lock().unwrap().clients.len())
        .unwrap_or(0);
    let _ = app.emit(EVENT, json!({"clients": n}));
}

/// A percent-encoded header value, decoded. Browsers encode "/" and non-ASCII
/// out of header values, and a remote upload's task id is a project path exactly
/// as often as it is a chat id.
fn decode_header(raw: &str) -> String {
    urlencoding::decode(raw)
        .map(|v| v.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// Is `origin` exactly this server, as the browser addressed it?
fn origin_matches(origin: &str, host: &str) -> bool {
    !host.is_empty() && (origin == format!("http://{host}") || origin == format!("https://{host}"))
}

async fn read_head(stream: &TcpStream) -> Result<http::Request, u16> {
    let mut buf = vec![0u8; http::MAX_HEAD];
    loop {
        let n = stream.peek(&mut buf).await.map_err(|_| 400u16)?;
        if n == 0 {
            return Err(400);
        }
        match http::parse(&buf[..n]) {
            Ok(r) => return Ok(r),
            Err(http::Parse::Partial) => {
                // `peek` returns at once with what is buffered; wait for more.
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            Err(http::Parse::TooLarge) => return Err(431),
            Err(http::Parse::Bad) => return Err(400),
        }
    }
}

/// The bundled chart library for remote preview frames.
fn viz_library(req: &http::Request) -> http::Response {
    if req.method != "GET" && req.method != "HEAD" {
        return http::Response::text(405, "GET only");
    }
    let request = tauri::http::Request::builder()
        .uri(format!(
            "http://openleash-viz.localhost/{}",
            crate::visualization::LIBRARY_PATH
        ))
        .body(vec![])
        .ok();
    match request
        .as_ref()
        .and_then(crate::visualization::library_response)
    {
        Some(res) => {
            let mut out = http::Response::new(200);
            for (k, v) in res.headers() {
                out = out.header(k.as_str(), v.to_str().unwrap_or(""));
            }
            out.body(res.into_body())
        }
        None => http::Response::text(404, "Not found"),
    }
}

/// Script sources a remote preview frame may load the chart library from: this
/// server's own library directory, over either scheme (a TLS proxy may front
/// it). The Host header is client-supplied and lands inside a policy, so
/// anything but a plain host[:port] yields no extra source at all.
fn remote_library_sources(host: &str) -> String {
    let plain = !host.is_empty()
        && host.len() <= 255
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    if !plain {
        return String::new();
    }
    format!("http://{host}/__openleash/viz/lib/ https://{host}/__openleash/viz/lib/")
}

fn login_page(status: u16, message: &str) -> http::Response {
    let esc = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    http::Response::new(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header(
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        )
        .body(LOGIN_HTML.replace("{{message}}", &esc))
}

/// Content-Security-Policy for the app itself when served remotely: the
/// desktop policy, with `ipc:` swapped for this server's WebSocket and the
/// visualization frames served from our own origin.
fn app_csp(host: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; \
         img-src 'self' data: blob: https:; connect-src 'self' ws://{host} wss://{host}; object-src 'none'; \
         frame-src 'self'; base-uri 'self'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// Put the bridge in front of every other script on the page.
pub fn inject_shim(html: &str) -> String {
    let tag = "<script src=\"/__openleash/remote.js\"></script>";
    match html.find("<head>") {
        Some(i) => format!("{}{tag}{}", &html[..i + 6], &html[i + 6..]),
        None => format!("{tag}{html}"),
    }
}

async fn serve_asset(app: &AppHandle, req: &http::Request) -> http::Response {
    let Some(path) = http::safe_asset_path(&req.path) else {
        return http::Response::text(400, "Bad path");
    };
    let is_index = path == "/" || path == "/index.html";
    let (bytes, mime) =
        match load_asset(app, if is_index { "/index.html" } else { &path }, req).await {
            Some(a) => a,
            // Client-side routes have no file of their own.
            None if !path.rsplit('/').next().unwrap_or("").contains('.') => {
                match load_asset(app, "/index.html", req).await {
                    Some(a) => a,
                    None => return http::Response::text(404, "Not found"),
                }
            }
            None => return http::Response::text(404, "Not found"),
        };
    let mut res = http::Response::new(200).header("Content-Type", mime.clone());
    if mime.starts_with("text/html") {
        let html = inject_shim(&String::from_utf8_lossy(&bytes));
        // Vite's dev server relies on inline module preambles; only the
        // production bundle is held to the strict policy.
        if !cfg!(dev) {
            res = res.header("Content-Security-Policy", app_csp(req.header("host")));
        }
        res.body(html.into_bytes())
    } else {
        if path.starts_with("/assets/") && !cfg!(dev) {
            // Vite fingerprints everything under /assets.
            res = res.header("Cache-Control", "public, max-age=31536000, immutable");
        }
        res.body(bytes)
    }
}

async fn load_asset(app: &AppHandle, path: &str, req: &http::Request) -> Option<(Vec<u8>, String)> {
    #[cfg(dev)]
    {
        // `tauri dev` serves the frontend from Vite, not from embedded assets,
        // so proxy to it and remote clients see the same live code.
        let base = app.config().build.dev_url.clone()?;
        let q = req
            .query
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut url = base.join(path.trim_start_matches('/')).ok()?;
        if !q.is_empty() {
            url.set_query(Some(&q));
        }
        let r = reqwest::get(url).await.ok()?;
        if !r.status().is_success() {
            return None;
        }
        let mime = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| http::mime(path).to_string());
        let bytes = r.bytes().await.ok()?.to_vec();
        Some((bytes, mime))
    }
    #[cfg(not(dev))]
    {
        let _ = req;
        let a = app.asset_resolver().get(path.to_string())?;
        let mime = if a.mime_type.is_empty() {
            http::mime(path).to_string()
        } else if a.mime_type.starts_with("text/") && !a.mime_type.contains("charset") {
            format!("{}; charset=utf-8", a.mime_type)
        } else {
            a.mime_type
        };
        Some((a.bytes, mime))
    }
}

/// The address other devices on the network most likely reach us at: the
/// source address of the default route. Connecting a UDP socket sends nothing.
fn lan_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

fn hostname() -> Option<String> {
    // `machine_name` falls back to /etc/hostname and `hostname` where the
    // environment doesn't say (HOSTNAME is rarely exported to a GUI app).
    Some(crate::agent::transfer::machine_name())
        .filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(|h| h.to_ascii_lowercase())
}

fn qr_svg(data: &str) -> String {
    use qrcode::render::svg;
    match qrcode::QrCode::new(data.as_bytes()) {
        Ok(code) => code
            .render::<svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
        Err(_) => String::new(),
    }
}

// ───────────────────────────── commands ─────────────────────────────

type R<'a> = tauri::State<'a, Arc<Remote>>;

#[tauri::command]
pub fn remote_status(r: R<'_>) -> Status {
    r.status()
}

#[tauri::command]
pub fn remote_configure(
    app: AppHandle,
    r: R<'_>,
    enabled: bool,
    lan: bool,
    port: u16,
) -> Result<Status, String> {
    r.configure(enabled, lan, port)?;
    r.inner().apply(&app);
    notify(&app);
    Ok(r.status())
}

#[tauri::command]
pub fn remote_new_code(r: R<'_>) -> Status {
    r.new_code();
    r.status()
}

#[tauri::command]
pub fn remote_revoke(app: AppHandle, r: R<'_>, id: String) -> Status {
    r.revoke(&id);
    notify(&app);
    r.status()
}

#[tauri::command]
pub fn remote_reset(app: AppHandle, r: R<'_>) -> Status {
    r.reset();
    notify(&app);
    r.status()
}

#[tauri::command]
pub fn remote_ntfy_configure(
    r: R<'_>,
    server: String,
    token: Option<String>,
) -> Result<Status, String> {
    r.configure_ntfy(&server, token.as_deref())?;
    Ok(r.status())
}

#[tauri::command]
pub fn remote_push_disable(app: AppHandle, r: R<'_>, id: String) -> Status {
    r.push_disable(&id);
    notify(&app);
    r.status()
}

fn ssh_hosts(r: &Remote) -> Vec<ssh::SshHost> {
    r.inner.lock().unwrap().vault.ssh_hosts.clone()
}

#[tauri::command]
pub fn ssh_hosts_list(r: R<'_>) -> Vec<ssh::SshHostView> {
    r.tunnels.view(&ssh_hosts(&r))
}

#[tauri::command]
pub fn ssh_host_save(
    r: R<'_>,
    id: Option<String>,
    name: String,
    target: String,
) -> Result<Vec<ssh::SshHostView>, String> {
    let target = target.trim().to_string();
    if !ssh::valid_target(&target) {
        return Err("Use an SSH host alias or user@host.".into());
    }
    let name = if name.trim().is_empty() {
        target.clone()
    } else {
        name.trim().chars().take(60).collect()
    };
    let mut edited = None;
    {
        let mut i = r.inner.lock().unwrap();
        match id.and_then(|id| i.vault.ssh_hosts.iter_mut().find(|h| h.id == id)) {
            Some(h) => {
                h.name = name;
                h.target = target;
                edited = Some(h.id.clone());
            }
            None => {
                let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
                i.vault.ssh_hosts.push(ssh::SshHost { id, name, target });
            }
        }
        i.vault.save();
    }
    // A remote project's open SFTP session and probed caps belong to the old
    // destination; the next use reconnects to the saved one.
    if let Some(id) = edited {
        forget_ssh_session(id);
    }
    Ok(r.tunnels.view(&ssh_hosts(&r)))
}

fn forget_ssh_session(id: String) {
    tauri::async_runtime::spawn(async move { crate::agent::ssh_session::forget(&id).await });
}

#[tauri::command]
pub fn ssh_host_remove(r: R<'_>, id: String) -> Vec<ssh::SshHostView> {
    r.tunnels.disconnect(&id);
    {
        let mut i = r.inner.lock().unwrap();
        i.vault.ssh_hosts.retain(|h| h.id != id);
        i.vault.save();
    }
    forget_ssh_session(id.clone());
    r.tunnels.view(&ssh_hosts(&r))
}

/// Tunnel to the host's OpenLeash and open it in the browser, already paired.
/// (A Tauri window can't host it: Tauri pins its own IPC object in every page
/// it shows, and the remote UI needs the bridge in that slot.)
#[tauri::command]
pub async fn ssh_connect(
    app: AppHandle,
    r: R<'_>,
    id: String,
) -> Result<Vec<ssh::SshHostView>, String> {
    let host = ssh_hosts(&r)
        .into_iter()
        .find(|h| h.id == id)
        .ok_or("No such SSH host.")?;
    let url = r.tunnels.connect(&host).await?;
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("The tunnel is up, but the browser couldn't be opened: {e}"))?;
    Ok(r.tunnels.view(&ssh_hosts(&r)))
}

#[tauri::command]
pub fn ssh_disconnect(r: R<'_>, id: String) -> Vec<ssh::SshHostView> {
    r.tunnels.disconnect(&id);
    r.tunnels.view(&ssh_hosts(&r))
}

#[derive(Serialize)]
pub struct DirEntry {
    name: String,
    path: String,
    dir: bool,
}

#[derive(Serialize)]
pub struct DirListing {
    path: String,
    parent: Option<String>,
    entries: Vec<DirEntry>,
    roots: Vec<String>,
    truncated: bool,
}

/// Directory listing for the remote folder picker, which stands in for the
/// native dialog a browser can't open on the host. Read-only, and no broader
/// than what the user could already reach with the desktop dialog.
#[tauri::command]
pub async fn remote_list_dir(path: Option<String>, dirs_only: bool) -> Result<DirListing, String> {
    tokio::task::spawn_blocking(move || list_dir(path, dirs_only))
        .await
        .map_err(|e| e.to_string())?
}

fn list_dir(path: Option<String>, dirs_only: bool) -> Result<DirListing, String> {
    const MAX: usize = 2000;
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let p = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => std::path::PathBuf::from(p),
        None => home.clone(),
    };
    let rd = std::fs::read_dir(&p).map_err(|e| format!("Couldn't open {}: {e}", p.display()))?;
    let mut entries: Vec<DirEntry> = vec![];
    let mut truncated = false;
    for e in rd.flatten() {
        let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false)
            || (e.file_type().map(|t| t.is_symlink()).unwrap_or(false) && e.path().is_dir());
        if dirs_only && !dir {
            continue;
        }
        if entries.len() >= MAX {
            truncated = true;
            break;
        }
        entries.push(DirEntry {
            name: e.file_name().to_string_lossy().to_string(),
            path: e.path().to_string_lossy().to_string(),
            dir,
        });
    }
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let mut roots = vec![home.to_string_lossy().to_string()];
    #[cfg(windows)]
    for l in b'A'..=b'Z' {
        let d = format!("{}:\\", l as char);
        if std::path::Path::new(&d).exists() {
            roots.push(d);
        }
    }
    // WSL distros, so a phone can open a project inside one just like the
    // desktop dialog can (commands for it then run in the distro).
    #[cfg(windows)]
    roots.extend(
        crate::agent::projects::wsl_distros_cached()
            .iter()
            .map(|d| crate::agent::projects::wsl_root(d)),
    );
    #[cfg(not(windows))]
    roots.push("/".into());
    Ok(DirListing {
        path: p.to_string_lossy().to_string(),
        parent: p.parent().map(|x| x.to_string_lossy().to_string()),
        entries,
        roots,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_preview_policy_names_only_this_hosts_library() {
        assert_eq!(
            remote_library_sources("box.tail1234.ts.net:7777"),
            "http://box.tail1234.ts.net:7777/__openleash/viz/lib/ https://box.tail1234.ts.net:7777/__openleash/viz/lib/"
        );
        assert_eq!(
            remote_library_sources("[::1]:80"),
            "http://[::1]:80/__openleash/viz/lib/ https://[::1]:80/__openleash/viz/lib/"
        );
        // A Host header that tries to add its own sources or directives adds nothing.
        for hostile in [
            "",
            "a.example; script-src *",
            "a.example *",
            "evil/x",
            "a'b",
            "a,b",
        ] {
            assert_eq!(remote_library_sources(hostile), "", "{hostile}");
        }
    }

    #[test]
    fn remote_chart_library_is_served_without_a_session() {
        let get = |method: &str| http::Request {
            method: method.into(),
            path: format!("/__openleash/viz/{}", crate::visualization::LIBRARY_PATH),
            query: Default::default(),
            headers: Default::default(),
            head_len: 0,
        };
        let res = viz_library(&get("GET"));
        assert_eq!(res.status, 200);
        assert!(String::from_utf8_lossy(&res.body).contains("registerTheme(\"openleash\""));
        assert_eq!(viz_library(&get("POST")).status, 405);
    }

    #[test]
    fn shim_goes_first_in_head() {
        let out = inject_shim("<!doctype html><html><head><meta charset=\"UTF-8\" /><script type=\"module\" src=\"/x.js\"></script></head></html>");
        let shim = out.find("/__openleash/remote.js").unwrap();
        assert!(shim < out.find("charset").unwrap());
        assert!(shim < out.find("/x.js").unwrap());
        assert!(
            inject_shim("<p>no head</p>").starts_with("<script src=\"/__openleash/remote.js\">")
        );
    }

    /// The upload's task id is a *project path* as often as it is a chat id: the
    /// new-task composer has no chat yet, so it sends the folder instead. That
    /// means the header value is percent-encoded (a path has "/" in it) and the
    /// decoded form must survive the round trip, or every new-task upload is
    /// refused with "no task" before it reaches the backend.
    #[test]
    fn an_encoded_project_path_header_survives_decoding() {
        assert_eq!(decode_header("D%3A%2Fwork%2Fmy%20app"), "D:/work/my app");
        // A plain chat id needs no encoding, and must not be damaged by having
        // been sent through it anyway.
        assert_eq!(decode_header("t-3f9a1c"), "t-3f9a1c");
        // Undecodable input is passed through rather than silently dropped.
        assert_eq!(decode_header("%zz"), "%zz");
    }

    /// The scope validation that rejected a path outright. `/` is legal in a task
    /// id precisely because it is a project path, so the guard only refuses
    /// control characters — while the *file name* half still rejects `/`.
    #[test]
    fn a_project_path_is_a_valid_scope_but_a_path_is_not_a_file_name() {
        let slash = 0x2fu8;
        let not_control = |b: u8| b >= 0x20;
        assert!(not_control(slash));
        // "D:/work/app" carries no control character, so it passes as a scope.
        assert!("D:/work/app".as_bytes().iter().all(|b| not_control(*b)));
        // A CR is refused either way.
        assert!(!"D:/work/app".contains(0x0du8 as char));
        let has_sep = |s: &str| s.as_bytes().iter().any(|b| *b == b'/' || *b == 0x5c);
        assert!(has_sep("a/b.txt"), "a file name is one component");
    }

    #[test]
    fn origin_must_name_this_exact_host() {
        assert!(origin_matches(
            "http://192.168.1.4:17420",
            "192.168.1.4:17420"
        ));
        assert!(origin_matches(
            "https://box.tail1234.ts.net",
            "box.tail1234.ts.net"
        ));
        assert!(!origin_matches("http://evil.example", "192.168.1.4:17420"));
        assert!(!origin_matches(
            "http://192.168.1.4:17421",
            "192.168.1.4:17420"
        ));
        assert!(!origin_matches("", "192.168.1.4:17420"));
        assert!(!origin_matches("null", "192.168.1.4:17420"));
        assert!(!origin_matches("http://", ""));
    }

    #[test]
    fn csp_allows_only_this_hosts_websocket() {
        let c = app_csp("10.0.0.2:17420");
        assert!(c.contains("connect-src 'self' ws://10.0.0.2:17420 wss://10.0.0.2:17420;"));
        assert!(c.contains("script-src 'self';"));
        assert!(c.contains("frame-ancestors 'none'"));
    }

    #[test]
    fn cookie_names_are_per_port() {
        assert_ne!(cookie_name(17420), cookie_name(17421));
    }

    #[test]
    fn qr_encodes_a_pairing_link() {
        let svg = qr_svg("http://192.168.1.4:17420/pair?t=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
    }

    #[test]
    fn list_dir_sorts_dirs_first_and_can_hide_files() {
        let tmp =
            std::env::temp_dir().join(format!("ol-remote-ls-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(tmp.join("Beta")).unwrap();
        std::fs::create_dir_all(tmp.join("alpha")).unwrap();
        std::fs::write(tmp.join("aaa.txt"), "x").unwrap();
        let l = list_dir(Some(tmp.to_string_lossy().to_string()), false).unwrap();
        let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Beta", "aaa.txt"]);
        let l = list_dir(Some(tmp.to_string_lossy().to_string()), true).unwrap();
        assert_eq!(l.entries.len(), 2);
        assert!(l.parent.is_some());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
