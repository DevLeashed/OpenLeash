//! A real browser the agent drives, over CDP, without ever touching the user's
//! own browser profile, screen, mouse or keyboard.
//!
//! The headless half of the screen tools, and the only one there is now: the
//! one-shot `render` plugin (HTML or a URL to a PNG) was folded in here, since
//! `navigate` + `screenshot` already did its whole job.
//!
//! # Model
//!
//! - **One browser process per mode** ([`Mode`]: throwaway/persistent ×
//!   headless/headed), shared by every agent that uses that mode, and launched
//!   on first use only. Every owner (`"{task}:{sub|main}"`) gets a [`Session`]:
//!   its own tabs, logs, downloads and network policy.
//! - **Throwaway (default):** each session is its own `Target.createBrowserContext`
//!   — an in-memory, incognito-like context — so cookies and storage are
//!   isolated between owners, and nothing is written to a profile anyone keeps.
//!   Each context gets its own filtering proxy (`proxyServer`).
//! - **Persistent profile:** one profile under the OpenLeash data dir
//!   (`<data>/browser-profile`, never the user's real Chrome profile), shared by
//!   every chat: one process, the default context, per-owner tabs. Chrome lets
//!   only one process use a user-data-dir, which the singleton registry gives
//!   us; a stale browser from a crashed run is detected and reported.
//!
//! # Connection
//!
//! One WebSocket to the browser target, flattened sessions for everything else.
//! A reader task demultiplexes responses by id (a oneshot per call) and routes
//! events by session id to the owning [`Session`]. Every socket and event task
//! runs on a small dedicated runtime ([`rt`]), so a browser outlives whichever
//! runtime happened to launch it (each `#[tokio::test]` has its own).
//!
//! # Network policy (layer 1)
//!
//! `Target.setAutoAttach {waitForDebuggerOnStart}` at the browser level and
//! again on every attached target pauses pages, popups, OOPIFs, workers and
//! service workers before they run; each gets `Fetch.enable` for every URL
//! before `Runtime.runIfWaitingForDebugger` releases it. Each paused request is
//! judged by [`NetPolicy`] (as written, then by its resolved addresses for
//! DNS rebinding) and failed with `BlockedByClient` when refused. The filtering
//! proxy (`browser_proxy`) is layer 2, judging the address actually connected to.

use super::browser_policy::{self as policy, NetPolicy, NetVerdict, SharedPolicy};
use super::browser_snapshot::{self as snap, SnapNode};
use super::plugins::{browser_name, browsers_installed};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

/// Longest we wait for the browser to hand back its debugging endpoint.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Default ceiling on a single CDP call. A page that never settles should fail
/// the call, not wedge the agent forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on one evaluation in the page.
const EVAL_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a session may sit unused before it is shut down.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Ceiling on the frame between polls while waiting for the debug port file.
const PORT_POLL: Duration = Duration::from_millis(100);
/// Ring-buffer sizes for the per-session logs.
const CONSOLE_CAP: usize = 500;
const NETWORK_CAP: usize = 1000;
/// The isolated world every helper runs in. The page's own scripts can't see it.
const WORLD: &str = "__openleash_agent";

/// Lock a std mutex, recovering from poisoning: the state behind these locks is
/// bookkeeping, and one panicked handler must not take the browser down with it.
fn lock<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The runtime every browser socket, reader and event handler lives on.
///
/// Dedicated rather than "whatever runtime called first": a browser process is
/// shared between agents (and, in tests, between `#[tokio::test]` runtimes that
/// each shut down when their test ends). If the reader task lived on the
/// launching runtime, the first test or chat to finish would silently cut every
/// other owner's connection.
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("openleash-browser")
            .enable_all()
            .build()
            .expect("browser runtime")
    })
}

// ───────────────────────────── connection ─────────────────────────────

type Waiter = (String, oneshot::Sender<Result<Value, String>>);

/// One CDP WebSocket: a writer channel, and the calls waiting for an answer.
struct Conn {
    tx: mpsc::UnboundedSender<String>,
    pending: StdMutex<HashMap<u64, Waiter>>,
    next: AtomicU64,
    closed: AtomicBool,
}

impl Conn {
    /// Queue a command; the receiver resolves with its `result`.
    fn send(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("browser: the browser has closed".into());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        // Registered before it is sent, so a fast answer can't beat it.
        lock(&self.pending).insert(id, (method.to_string(), tx));
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        if self.tx.send(msg.to_string()).is_err() {
            lock(&self.pending).remove(&id);
            return Err("browser: the debugging socket is closed".into());
        }
        Ok(rx)
    }

    async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let rx = self.send(session, method, params)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("browser: the browser closed the connection".into()),
            Err(_) => Err(format!(
                "browser: {method} didn't answer within {}s",
                timeout.as_secs()
            )),
        }
    }

    /// Fire and forget: the answer, when it comes, is dropped by the reader.
    fn fire(&self, session: Option<&str>, method: &str, params: Value) {
        let _ = self.send(session, method, params);
    }
}

/// Read every message: answers go to their waiter, events to the browser.
async fn read_loop<S>(mut rd: S, conn: Arc<Conn>, browser: Weak<Browser>)
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = rd.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let waiter = lock(&conn.pending).remove(&id);
            if let Some((method, tx)) = waiter {
                let r = match v.get("error") {
                    Some(e) => Err(format!(
                        "browser: {method} failed: {}",
                        e["message"].as_str().unwrap_or("unknown error")
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(r);
            }
            continue;
        }
        if let Some(b) = browser.upgrade() {
            b.on_event(&v);
        }
    }
    conn.closed.store(true, Ordering::SeqCst);
    let waiting: Vec<Waiter> = lock(&conn.pending).drain().map(|(_, w)| w).collect();
    for (_, tx) in waiting {
        let _ = tx.send(Err("browser: the browser closed the connection".into()));
    }
}

// ───────────────────────────── browser process ─────────────────────────────

/// Which kind of browser process a session needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mode {
    pub persistent: bool,
    pub headed: bool,
}

/// Where one CDP session sits: whose it is, and in which tab.
#[derive(Debug, Clone)]
struct Route {
    owner: Option<String>,
    target: String,
    kind: String,
    /// The tab (page target id) this session belongs to, if any.
    tab: Option<String>,
    parent: Option<String>,
}

#[derive(Default)]
struct Routes {
    sessions: HashMap<String, Route>,
    owners: HashMap<String, Weak<Session>>,
    contexts: HashMap<String, String>,
    /// Page targets → owner.
    targets: HashMap<String, String>,
    /// Frame id → tab target id, for routing downloads.
    frames: HashMap<String, String>,
    /// Download guid → owner.
    downloads: HashMap<String, String>,
    /// Targets whose interception is set up and that have been released.
    configured: std::collections::HashSet<String>,
}

/// One running browser process and its connection.
pub(crate) struct Browser {
    conn: Arc<Conn>,
    child: StdMutex<Option<std::process::Child>>,
    /// Throwaway: the temp dir holding the profile, removed on drop.
    temp: Option<PathBuf>,
    /// Where `allowAndName` drops downloads (as GUIDs) before they are moved
    /// into the owner's folder.
    staging: PathBuf,
    /// The CDP port, which no page may reach.
    port: u16,
    name: String,
    /// Process-wide proxy (persistent mode, where there is one context).
    proxy: StdMutex<Option<super::browser_proxy::ProxyHandle>>,
    /// Policy for targets nobody owns (the launch page, a stray worker) and for
    /// the process-wide proxy: no project, no LAN approvals of its own.
    proc_policy: SharedPolicy,
    routes: StdMutex<Routes>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Kill, then delete the profile: a browser still running holds files in
        // that dir, and on Windows the remove fails.
        self.conn.closed.store(true, Ordering::SeqCst);
        if let Some(ch) = lock(&self.child).as_mut() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
        if let Some(t) = &self.temp {
            let _ = std::fs::remove_dir_all(t);
        }
    }
}

/// Running browsers by mode.
fn browsers() -> &'static StdMutex<HashMap<Mode, Arc<Browser>>> {
    static B: OnceLock<StdMutex<HashMap<Mode, Arc<Browser>>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

/// One launch at a time: two agents asking at once must share one process.
fn launch_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Drop browsers no session uses any more (the registry's is the only Arc).
async fn sweep_browsers() {
    // Serialize shutdown with launch too: after removing the last Arc from the
    // registry, dropping it still has to kill/wait for Chromium to release its
    // profile. A new launch must not race that final shutdown.
    let _launch = launch_lock().lock().await;
    let dead: Vec<Arc<Browser>> = {
        let mut m = lock(browsers());
        let keys: Vec<Mode> = m
            .iter()
            .filter(|(mode, b)| {
                (!mode.persistent && Arc::strong_count(b) == 1)
                    || b.conn.closed.load(Ordering::SeqCst)
            })
            .map(|(k, _)| *k)
            .collect();
        keys.iter().filter_map(|k| m.remove(k)).collect()
    };
    // Killed outside the lock: dropping waits for the process.
    drop(dead);
}

/// Chrome preferences that keep credentials out of the agent's browser: no
/// password manager, no autofill of addresses or cards. Merged into an
/// existing Preferences file so a persistent profile keeps its other settings.
fn write_prefs(profile: &Path) {
    let dir = profile.join("Default");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Preferences");
    let mut v: Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let set = |v: &mut Value, path: &[&str], val: Value| {
        let mut cur = v;
        for p in &path[..path.len() - 1] {
            if !cur[*p].is_object() {
                cur[*p] = json!({});
            }
            cur = &mut cur[*p];
        }
        cur[path[path.len() - 1]] = val;
    };
    set(&mut v, &["credentials_enable_service"], json!(false));
    set(&mut v, &["credentials_enable_autosignin"], json!(false));
    set(
        &mut v,
        &["profile", "password_manager_enabled"],
        json!(false),
    );
    set(&mut v, &["autofill", "profile_enabled"], json!(false));
    set(&mut v, &["autofill", "credit_card_enabled"], json!(false));
    set(&mut v, &["autofill", "enabled"], json!(false));
    set(&mut v, &["download", "prompt_for_download"], json!(false));
    // A persistent profile reopened after a crash must not offer to restore.
    set(&mut v, &["profile", "exit_type"], json!("Normal"));
    set(&mut v, &["profile", "exited_cleanly"], json!(true));
    let _ = std::fs::write(&file, v.to_string());
}

/// The launch flags, apart from profile, port and window size.
///
/// `--remote-allow-origins` is deliberately absent: with `*`, any web page that
/// finds the debugging port could open a CDP WebSocket (browsers send an
/// Origin header, which Chrome then checks) and take the browser over. The
/// harness's own client sends no Origin header, which Chrome accepts.
fn launch_flags(headed: bool, proxy_port: u16) -> Vec<String> {
    let mut v: Vec<String> = [
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-component-extensions-with-background-pages",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-default-apps",
        "--disable-domain-reliability",
        "--disable-client-side-phishing-detection",
        "--disable-sync",
        "--no-pings",
        "--metrics-recording-only",
        "--password-store=basic",
        "--use-mock-keychain",
        // QUIC and DNS prefetch reach the network outside the request path the
        // policy sees; WebRTC UDP would go around the proxy entirely.
        "--disable-quic",
        "--dns-prefetch-disable",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--disable-features=Translate,MediaRouter,DialMediaRouteProvider,OptimizationHints,AutofillServerCommunication,PasswordLeakDetection,InterestFeedContentSuggestions,NetworkPrediction",
        "--disable-renderer-backgrounding",
        "--disable-backgrounding-occluded-windows",
        "--disable-background-timer-throttling",
        "--remote-debugging-port=0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !headed {
        v.push("--headless=new".into());
        v.push("--hide-scrollbars".into());
        v.push("--mute-audio".into());
        v.push("--disable-gpu".into());
    }
    if proxy_port != 0 {
        // `<-loopback>` removes Chrome's implicit loopback bypass, so local
        // traffic goes through the proxy too and the reserved ports are
        // enforced at both layers.
        v.push(format!("--proxy-server=http://127.0.0.1:{proxy_port}"));
        v.push("--proxy-bypass-list=<-loopback>".into());
    }
    v
}

/// The install hint when no Chromium browser is found.
pub fn no_browser_message() -> String {
    "No Chromium-based browser found, and the browser plugin needs one. Install Google Chrome, Microsoft Edge or Chromium (any recent version), then try again.".into()
}

impl Browser {
    /// The running browser for `mode`, launching it if there isn't a live one.
    async fn get(mode: Mode) -> Result<Arc<Browser>, String> {
        if let Some(b) = lock(browsers()).get(&mode).cloned() {
            if b.alive() {
                return Ok(b);
            }
        }
        let _g = launch_lock().lock().await;
        // Someone else may have launched it while we waited for the lock.
        if let Some(b) = lock(browsers()).get(&mode).cloned() {
            if b.alive() {
                return Ok(b);
            }
        }
        if mode.persistent {
            // Headed and headless share the *same* persistent profile. Never
            // launch a second process with its user-data-dir: Chromium would
            // forward the launch to the first process and exit.
            let other = lock(browsers())
                .iter()
                .any(|(m, b)| m.persistent && *m != mode && b.alive());
            if other {
                return Err(r#"browser: the persistent browser is already running in the other window mode. Close its chats before changing "Show the browser window", or turn off "Keep sign-ins between chats"."#.into());
            }
        }
        let stale = lock(browsers()).remove(&mode);
        drop(stale);
        let b = Browser::launch(mode).await?;
        lock(browsers()).insert(mode, b.clone());
        Ok(b)
    }

    fn alive(&self) -> bool {
        if self.conn.closed.load(Ordering::SeqCst) {
            return false;
        }
        match lock(&self.child).as_mut() {
            Some(ch) => !matches!(ch.try_wait(), Ok(Some(_))),
            None => true,
        }
    }

    async fn launch(mode: Mode) -> Result<Arc<Browser>, String> {
        let installed = browsers_installed();
        // Test the same core against every installed Chromium, without a
        // production binary-path override or an internet dependency.
        #[cfg(test)]
        let installed = {
            let pick = std::env::var("OPENLEASH_TEST_BROWSER").unwrap_or_default();
            let matching: Vec<_> = installed
                .iter()
                .filter(|p| browser_name(p).eq_ignore_ascii_case(&pick))
                .cloned()
                .collect();
            if matching.is_empty() {
                installed
            } else {
                matching
            }
        };
        let Some(exe) = installed.first().cloned() else {
            return Err(no_browser_message());
        };
        let who = browser_name(&exe);
        let (profile, staging, temp) = if mode.persistent {
            let home = super::store::data_dir();
            let profile = home.join("browser-profile");
            let staging = home.join("browser-downloads-staging");
            // Chrome allows one process per user-data-dir. A browser left over
            // from a crashed run still holds the lock file; launching another
            // one would just hand the URL to it and exit, so say what's wrong.
            let lockfile = profile.join("lockfile");
            if lockfile.exists() && std::fs::remove_file(&lockfile).is_err() {
                return Err(format!(
                    "browser: the persistent browser profile ({}) is in use by another browser process — probably one left over from an earlier OpenLeash run. Close it (or end its {who} process), or turn off \"Keep sign-ins between chats\".",
                    profile.display()
                ));
            }
            let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
            (profile, staging, None)
        } else {
            let dir = std::env::temp_dir().join(format!(
                "openleash-browser-{}",
                uuid::Uuid::new_v4().simple()
            ));
            (dir.join("profile"), dir.join("downloads"), Some(dir))
        };
        std::fs::create_dir_all(&profile)
            .map_err(|e| format!("browser: can't create a profile dir: {e}"))?;
        std::fs::create_dir_all(&staging)
            .map_err(|e| format!("browser: can't create a download dir: {e}"))?;
        write_prefs(&profile);

        let proc_policy: SharedPolicy = Arc::new(std::sync::RwLock::new(NetPolicy::default()));
        // One context in persistent mode, so one proxy for the whole process.
        let proxy = if mode.persistent {
            Some(start_proxy(proc_policy.clone()).await?)
        } else {
            None
        };
        let proxy_port = proxy.as_ref().map(|p| p.port).unwrap_or(0);

        let mut cmd = super::plugins::hidden_cmd(exe.to_str().unwrap_or_default());
        cmd.args(launch_flags(mode.headed, proxy_port))
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--window-size=1280,800")
            .arg("about:blank")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("browser: couldn't start {who}: {e}"))?;

        let started = Instant::now();
        let endpoint = loop {
            if let Ok(txt) = std::fs::read_to_string(profile.join("DevToolsActivePort")) {
                let mut it = txt.lines();
                if let (Some(port), Some(path)) = (it.next(), it.next()) {
                    if let Ok(port) = port.trim().parse::<u16>() {
                        break Ok((port, format!("ws://127.0.0.1:{port}{}", path.trim())));
                    }
                }
            }
            if let Ok(Some(_)) = child.try_wait() {
                break Err(if mode.persistent {
                    format!("browser: {who} exited before it was ready. If another {who} is using the persistent profile, close it, or turn off \"Keep sign-ins between chats\".")
                } else {
                    format!("browser: {who} exited before it was ready")
                });
            }
            if started.elapsed() > LAUNCH_TIMEOUT {
                break Err(format!(
                    "browser: {who} never reported a debugging port within {}s",
                    LAUNCH_TIMEOUT.as_secs()
                ));
            }
            tokio::time::sleep(PORT_POLL).await;
        };
        let (port, endpoint) = match endpoint {
            Ok(x) => x,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(t) = &temp {
                    let _ = std::fs::remove_dir_all(t);
                }
                return Err(e);
            }
        };

        // Connect on the browser runtime, so the socket lives there.
        let ws = rt()
            .spawn(async move { tokio_tungstenite::connect_async(endpoint).await })
            .await
            .map_err(|e| format!("browser: connect task failed: {e}"))?;
        let (ws, _) = match ws {
            Ok(x) => x,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("browser: can't reach the debugging socket: {e}"));
            }
        };
        let (mut sink, stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(Conn {
            tx,
            pending: StdMutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });
        {
            let mut p = proc_policy.write().unwrap_or_else(|e| e.into_inner());
            p.blocked_ports.push(port);
            if proxy_port != 0 {
                p.blocked_ports.push(proxy_port);
            }
        }
        let b = Arc::new(Browser {
            conn: conn.clone(),
            child: StdMutex::new(Some(child)),
            temp,
            staging: staging.clone(),
            port,
            name: who,
            proxy: StdMutex::new(proxy),
            proc_policy,
            routes: StdMutex::new(Routes::default()),
        });
        let c2 = conn.clone();
        rt().spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(Message::Text(m.into())).await.is_err() {
                    break;
                }
            }
            c2.closed.store(true, Ordering::SeqCst);
        });
        rt().spawn(read_loop(stream, conn.clone(), Arc::downgrade(&b)));

        let c = &b.conn;
        c.call(
            None,
            "Target.setDiscoverTargets",
            json!({"discover": true}),
            CALL_TIMEOUT,
        )
        .await?;
        // Every top-level target (pages, popups, service and shared workers),
        // paused until it has been given request interception.
        c.call(
            None,
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            CALL_TIMEOUT,
        )
        .await?;
        let _ = c
            .call(
                None,
                "Browser.setDownloadBehavior",
                json!({"behavior": "allowAndName", "downloadPath": staging.to_string_lossy(), "eventsEnabled": true}),
                CALL_TIMEOUT,
            )
            .await;
        Ok(b)
    }

    // ── event routing ──────────────────────────────────────────────────

    fn session_of(&self, owner: &Option<String>) -> Option<Arc<Session>> {
        let o = owner.as_ref()?;
        lock(&self.routes).owners.get(o)?.upgrade()
    }

    fn on_event(self: &Arc<Self>, v: &Value) {
        let method = v["method"].as_str().unwrap_or("");
        let p = &v["params"];
        let sid = v["sessionId"].as_str();
        match (method, sid) {
            ("Target.attachedToTarget", parent) => self.on_attached(parent, p),
            ("Target.detachedFromTarget", _) => {
                let gone = p["sessionId"].as_str().unwrap_or("");
                let route = lock(&self.routes).sessions.remove(gone);
                if let Some(r) = route {
                    if let Some(s) = self.session_of(&r.owner) {
                        s.on_detached(gone);
                    }
                }
            }
            ("Target.targetDestroyed", None) => {
                let t = p["targetId"].as_str().unwrap_or("");
                let owner = lock(&self.routes).targets.remove(t);
                if let Some(s) = self.session_of(&owner) {
                    s.remove_tab(t);
                }
            }
            ("Target.targetInfoChanged", None) => {
                let info = &p["targetInfo"];
                if info["type"] == "page" {
                    let t = info["targetId"].as_str().unwrap_or("");
                    let owner = lock(&self.routes).targets.get(t).cloned();
                    if let Some(s) = self.session_of(&owner) {
                        s.tab_info_changed(t, info);
                    }
                }
            }
            ("Browser.downloadWillBegin", None) => {
                let frame = p["frameId"].as_str().unwrap_or("");
                let guid = p["guid"].as_str().unwrap_or("").to_string();
                let owner = {
                    let mut r = lock(&self.routes);
                    let tab = r
                        .frames
                        .get(frame)
                        .cloned()
                        .unwrap_or_else(|| frame.to_string());
                    let o = r.targets.get(&tab).cloned();
                    if let Some(o) = &o {
                        r.downloads.insert(guid.clone(), o.clone());
                    }
                    o
                };
                match self.session_of(&owner) {
                    Some(s) => s.download_began(&guid, p),
                    // Nobody's: never let it land.
                    None => self
                        .conn
                        .fire(None, "Browser.cancelDownload", json!({"guid": guid})),
                }
            }
            ("Browser.downloadProgress", None) => {
                let guid = p["guid"].as_str().unwrap_or("");
                let owner = lock(&self.routes).downloads.get(guid).cloned();
                let state = p["state"].as_str().unwrap_or("");
                match self.session_of(&owner) {
                    Some(s) => s.download_progress(&self.staging, guid, p),
                    None if state == "completed" => {
                        let _ = std::fs::remove_file(self.staging.join(guid));
                    }
                    None => {}
                }
            }
            ("Fetch.requestPaused", Some(s)) => self.on_paused(s, p),
            ("Fetch.authRequired", Some(s)) => {
                // Never answer a credentials prompt on anyone's behalf.
                self.conn.fire(
                    Some(s),
                    "Fetch.continueWithAuth",
                    json!({"requestId": p["requestId"], "authChallengeResponse": {"response": "CancelAuth"}}),
                );
            }
            (_, Some(s)) => {
                let route = lock(&self.routes).sessions.get(s).cloned();
                if let Some(r) = route {
                    if method == "Page.frameAttached" || method == "Page.frameNavigated" {
                        let fid = if method == "Page.frameAttached" {
                            p["frameId"].as_str()
                        } else {
                            p["frame"]["id"].as_str()
                        };
                        if let (Some(f), Some(tab)) = (fid, r.tab.clone()) {
                            lock(&self.routes).frames.insert(f.to_string(), tab);
                        }
                    }
                    if let Some(sess) = self.session_of(&r.owner) {
                        sess.on_event(s, &r, method, p);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_attached(self: &Arc<Self>, parent: Option<&str>, p: &Value) {
        let sid = p["sessionId"].as_str().unwrap_or("").to_string();
        let info = &p["targetInfo"];
        let tid = info["targetId"].as_str().unwrap_or("").to_string();
        let kind = info["type"].as_str().unwrap_or("").to_string();
        let waiting = p["waitingForDebugger"].as_bool().unwrap_or(false);
        if sid.is_empty() {
            return;
        }
        let (owner, tab) = {
            let mut r = lock(&self.routes);
            let (owner, tab) = match parent.and_then(|p| r.sessions.get(p)) {
                Some(pr) => (
                    pr.owner.clone(),
                    pr.tab
                        .clone()
                        .or_else(|| (pr.kind == "page").then(|| pr.target.clone())),
                ),
                None => {
                    let ctx = info["browserContextId"].as_str().unwrap_or("");
                    let owner = r
                        .targets
                        .get(&tid)
                        .cloned()
                        .or_else(|| r.contexts.get(ctx).cloned())
                        .or_else(|| {
                            info["openerId"]
                                .as_str()
                                .and_then(|o| r.targets.get(o).cloned())
                        });
                    (owner, (kind == "page").then(|| tid.clone()))
                }
            };
            if kind == "page" && parent.is_none() {
                if let Some(o) = &owner {
                    r.targets.insert(tid.clone(), o.clone());
                }
            }
            if kind == "iframe" {
                if let Some(t) = &tab {
                    r.frames.insert(tid.clone(), t.clone());
                }
            }
            r.sessions.insert(
                sid.clone(),
                Route {
                    owner: owner.clone(),
                    target: tid.clone(),
                    kind: kind.clone(),
                    tab: tab.clone(),
                    parent: parent.map(str::to_string),
                },
            );
            (owner, tab)
        };
        let session = self.session_of(&owner);
        if let Some(s) = &session {
            s.on_attached(&sid, &tid, &kind, parent, info);
        }
        let me = self.clone();
        let _ = tab;
        rt().spawn(async move { me.configure(sid, tid, kind, waiting, session).await });
    }

    /// Give a freshly attached target interception (and logging), then let it
    /// run. Fetch must be on before the target executes a single request.
    async fn configure(
        self: Arc<Self>,
        sid: String,
        tid: String,
        kind: String,
        waiting: bool,
        session: Option<Arc<Session>>,
    ) {
        let c = &self.conn;
        let s = Some(sid.as_str());
        let fetch = c.send(
            s,
            "Fetch.enable",
            json!({"patterns": [{"urlPattern": "*", "requestStage": "Request"}], "handleAuthRequests": true}),
        );
        let mut rest = Vec::new();
        let attach = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
        match kind.as_str() {
            "page" | "iframe" => {
                for (m, p) in [
                    ("Page.enable", json!({})),
                    ("Runtime.enable", json!({})),
                    ("Network.enable", json!({"maxPostDataSize": 0})),
                    ("Log.enable", json!({})),
                    ("Page.setLifecycleEventsEnabled", json!({"enabled": true})),
                    ("Target.setAutoAttach", attach.clone()),
                ] {
                    rest.push(c.send(s, m, p));
                }
                if kind == "page" {
                    if let Some(sess) = &session {
                        for (m, p) in sess.emulation_calls() {
                            rest.push(c.send(s, &m, p));
                        }
                    }
                }
            }
            "worker" | "shared_worker" | "service_worker" => {
                for (m, p) in [
                    ("Runtime.enable", json!({})),
                    ("Network.enable", json!({"maxPostDataSize": 0})),
                    ("Target.setAutoAttach", attach.clone()),
                ] {
                    rest.push(c.send(s, m, p));
                }
            }
            _ => {}
        }
        let fetch_ok = match fetch {
            Ok(rx) => match tokio::time::timeout(CALL_TIMEOUT, rx).await {
                Ok(Ok(Ok(_))) => Ok(()),
                Ok(Ok(Err(e))) => Err(e),
                _ => Err("no answer".to_string()),
            },
            Err(e) => Err(e),
        };
        // A dedicated worker has no Fetch domain of its own; its requests are
        // paused by the owning page's interception instead (verified by
        // `workers_and_service_workers_are_intercepted`), so that's no gap.
        let covered_by_parent = kind == "worker";
        if let (Err(e), false) = (&fetch_ok, covered_by_parent) {
            // Not silent: the agent is told, and the proxy (layer 2) still
            // filters this target's traffic.
            if let Some(sess) = &session {
                sess.note(format!(
                    "request interception is unavailable in a {kind} ({e}); its traffic is filtered by the proxy only"
                ));
            }
        }
        if waiting {
            c.fire(s, "Runtime.runIfWaitingForDebugger", json!({}));
        }
        // Only now wait for the rest: renderer-side domains (Page, Runtime)
        // don't answer a target that is still paused, and everything was sent
        // before the resume, so it is in effect before the first request.
        for rx in rest.into_iter().flatten() {
            let _ = tokio::time::timeout(CALL_TIMEOUT, rx).await;
        }
        // Whose it is *now*: a tab created in persistent mode is claimed by
        // its creator after the attach event, possibly while this ran.
        let owner = {
            let mut r = lock(&self.routes);
            r.configured.insert(tid.clone());
            r.sessions.get(&sid).and_then(|x| x.owner.clone())
        };
        if let Some(sess) = self.session_of(&owner) {
            sess.mark_ready(&tid);
        }
    }

    fn on_paused(self: &Arc<Self>, sid: &str, p: &Value) {
        let route = lock(&self.routes).sessions.get(sid).cloned();
        let session = route.as_ref().and_then(|r| self.session_of(&r.owner));
        let pol = session
            .as_ref()
            .map(|s| s.policy.clone())
            .unwrap_or_else(|| self.proc_policy.clone());
        let url = p["request"]["url"].as_str().unwrap_or("").to_string();
        let rid = p["requestId"].clone();
        let sid = sid.to_string();
        let me = self.clone();
        rt().spawn(async move {
            let verdict = judge(&pol, &url).await;
            match verdict {
                NetVerdict::Allow => {
                    me.conn.fire(
                        Some(&sid),
                        "Fetch.continueRequest",
                        json!({"requestId": rid}),
                    );
                }
                v => {
                    me.conn.fire(
                        Some(&sid),
                        "Fetch.failRequest",
                        json!({"requestId": rid, "errorReason": "BlockedByClient"}),
                    );
                    if let Some(s) = session {
                        s.record_block(&url, &v);
                    }
                }
            }
        });
    }
}

/// Start a filtering proxy on the browser runtime (so it outlives the caller's).
async fn start_proxy(pol: SharedPolicy) -> Result<super::browser_proxy::ProxyHandle, String> {
    rt().spawn(async move { super::browser_proxy::start(pol).await })
        .await
        .map_err(|e| format!("browser: the proxy task failed: {e}"))?
}

/// Recently resolved hosts, so every subresource doesn't pay for a lookup. Kept
/// short: this is the interception layer; the proxy judges each connection.
type DnsCache = StdMutex<HashMap<String, (Vec<std::net::IpAddr>, Instant)>>;

fn dns_cache() -> &'static DnsCache {
    static C: OnceLock<DnsCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Judge one request URL: as written, then by where its host resolves.
pub(crate) async fn judge(pol: &SharedPolicy, url: &str) -> NetVerdict {
    let (verdict, ports) = {
        let p = pol.read().unwrap_or_else(|e| e.into_inner());
        (p.check_url(url), p.blocked_ports.clone())
    };
    if verdict != NetVerdict::Allow {
        return verdict;
    }
    let Ok(u) = reqwest::Url::parse(url) else {
        return verdict;
    };
    if !matches!(u.scheme(), "http" | "https" | "ws" | "wss") {
        return verdict;
    }
    let host = u
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_string();
    let port = u.port_or_known_default().unwrap_or(80);
    let reserved = |ip: &std::net::IpAddr| ip.is_loopback() && ports.contains(&port);
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if reserved(&ip) {
            return NetVerdict::Block(format!("port {port} on this machine is reserved"));
        }
        return verdict;
    }
    if policy::classify_host(&host) == policy::HostClass::Loopback {
        if ports.contains(&port) {
            return NetVerdict::Block(format!("port {port} on this machine is reserved"));
        }
        return verdict;
    }
    let cached = lock(dns_cache())
        .get(&host)
        .filter(|(_, t)| t.elapsed() < Duration::from_secs(10))
        .map(|(ips, _)| ips.clone());
    let ips = match cached {
        Some(ips) => ips,
        None => {
            let looked = tokio::time::timeout(
                Duration::from_secs(4),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await;
            match looked {
                Ok(Ok(addrs)) => {
                    let ips: Vec<std::net::IpAddr> = addrs.map(|a| a.ip()).collect();
                    lock(dns_cache()).insert(host.clone(), (ips.clone(), Instant::now()));
                    ips
                }
                // Unresolvable here means unreachable there: the browser
                // fails it itself, and the proxy re-checks any connection.
                _ => return verdict,
            }
        }
    };
    if ips.iter().any(reserved) {
        return NetVerdict::Block(format!(
            "{host} resolves to a reserved port on this machine"
        ));
    }
    let p = pol.read().unwrap_or_else(|e| e.into_inner());
    p.check_resolved(&host, &ips)
}

// ───────────────────────────── session ─────────────────────────────

/// Where the agent is, so a later call (and the UI) can say where.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageState {
    pub url: String,
    pub title: String,
}

/// What a session needs to know about its owner.
#[derive(Debug, Clone, Default)]
pub struct SessionOpts {
    pub width: u32,
    pub height: u32,
    pub persistent: bool,
    pub headed: bool,
    /// `file://` is limited to this folder (and `downloads`); `None` refuses
    /// `file://` altogether.
    pub project: Option<PathBuf>,
    pub downloads: Option<PathBuf>,
    /// LAN hosts already approved by a permission rule.
    pub lan_allowed: Vec<String>,
}

impl SessionOpts {
    pub fn from_cfg(cfg: &super::plugins::BrowserCfg) -> Self {
        SessionOpts {
            width: if cfg.width == 0 { 1280 } else { cfg.width },
            height: if cfg.height == 0 { 800 } else { cfg.height },
            persistent: cfg.persistent_profile,
            headed: cfg.headed,
            ..Default::default()
        }
    }
    fn mode(&self) -> Mode {
        Mode {
            persistent: self.persistent,
            headed: self.headed,
        }
    }
}

#[derive(Debug, Clone)]
struct Tab {
    id: String,
    target: String,
    session: String,
    url: String,
    title: String,
    ready: bool,
}

/// One console line (or uncaught exception).
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub level: String,
    pub text: String,
    pub source: String,
    pub tab: String,
}

/// One request in the network log.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetEntry {
    pub id: String,
    #[serde(skip)]
    session: String,
    #[serde(skip)]
    request_id: String,
    pub method: String,
    pub url: String,
    pub kind: String,
    pub status: Option<i64>,
    pub mime: String,
    #[serde(skip)]
    start: f64,
    pub ms: Option<u64>,
    pub size: u64,
    pub failed: Option<String>,
    pub blocked: bool,
    pub request_headers: Value,
    pub response_headers: Value,
    pub tab: String,
}

/// One download.
#[derive(Debug, Clone, Serialize)]
pub struct Download {
    #[serde(skip)]
    guid: String,
    pub url: String,
    pub suggested: String,
    pub path: Option<String>,
    pub state: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default)]
struct Emulation {
    width: u32,
    height: u32,
    dpr: f64,
    mobile: bool,
    touch: bool,
    user_agent: String,
    color_scheme: String,
}

#[derive(Default)]
struct State {
    tabs: Vec<Tab>,
    active: Option<String>,
    next_tab: u32,
    /// (session, frame) → isolated world context id.
    worlds: HashMap<(String, String), i64>,
    frame_idx: HashMap<String, u32>,
    frame_by_idx: HashMap<u32, (String, String)>,
    next_frame: u32,
    /// OOPIF frame id → its session; child session → parent session.
    oopifs: HashMap<String, String>,
    parent_of: HashMap<String, String>,
    console: VecDeque<LogLine>,
    console_errors: u64,
    console_errors_seen: u64,
    network: VecDeque<NetEntry>,
    net_next: u64,
    inflight: HashMap<(String, String), (String, Instant)>,
    last_net: Option<Instant>,
    blocked: Vec<String>,
    blocked_seen: usize,
    lan_blocked: BTreeSet<String>,
    downloads: Vec<Download>,
    downloads_seen: usize,
    dialogs: HashMap<String, (String, DialogInfo)>,
    history: VecDeque<String>,
    emulation: Option<Emulation>,
    last_shot: Option<Vec<u8>>,
    loading: HashMap<String, bool>,
    drag: Option<Value>,
    notes: Vec<String>,
    notes_seen: usize,
    new_tabs: Vec<String>,
    perf_on: bool,
    /// Proxy refusals already reported.
    proxy_seen: Vec<String>,
}

/// What changed since the last action's report.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub new_tabs: Vec<String>,
    pub blocked: Vec<String>,
    pub lan_hosts: Vec<String>,
    pub downloads: Vec<Download>,
    pub console_errors: u64,
    pub dialog: Option<DialogInfo>,
    pub tabs: usize,
    pub notes: Vec<String>,
}

/// A target an action resolved to.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    sid: String,
    frame: String,
    prefix: String,
    pub ref_: String,
    pub role: String,
    pub name: String,
    pub tag: String,
    pub matches: u64,
}

impl Resolved {
    /// `[e12] button "Save"`, for details and results.
    pub fn describe(&self) -> String {
        snap::Candidate {
            ref_: self.ref_.clone(),
            role: if self.role.is_empty() {
                self.tag.clone()
            } else {
                self.role.clone()
            },
            name: self.name.clone(),
        }
        .describe()
    }
}

/// How an action names its element.
#[derive(Debug, Clone, Default)]
pub struct TargetSpec {
    pub ref_: Option<String>,
    pub selector: Option<String>,
    pub text: Option<String>,
    pub point: Option<(f64, f64)>,
}

impl TargetSpec {
    /// `text_targets`: whether `text` names the element (click, hover…) or is
    /// content (type, fill), in which case only `name` does.
    pub fn from_input(v: &Value, text_targets: bool) -> Result<Self, String> {
        let s = |k: &str| {
            v[k].as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let ref_ = match s("ref") {
            Some(r) => Some(snap::parse_ref(&r).ok_or_else(|| {
                format!("`{r}` is not a ref. Refs look like e12 (from a snapshot).")
            })?),
            None => None,
        };
        let point = match (v["x"].as_f64(), v["y"].as_f64()) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 => {
                Some((x, y))
            }
            (None, None) => None,
            _ => return Err("`x` and `y` must both be given, as non-negative numbers".into()),
        };
        Ok(TargetSpec {
            ref_,
            selector: s("selector"),
            text: s("name").or_else(|| if text_targets { s("text") } else { None }),
            point,
        })
    }
    pub fn is_empty(&self) -> bool {
        self.ref_.is_none()
            && self.selector.is_none()
            && self.text.is_none()
            && self.point.is_none()
    }
}

/// A boxed future, for the recursive frame walks.
type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Error prefix for "a dialog is in the way", so callers can tell it apart.
pub const DIALOG_OPEN: &str = "browser: a JavaScript dialog is open";

/// One owner's browser: its tabs, logs, downloads and network policy.
pub struct Session {
    owner: String,
    browser: Arc<Browser>,
    mode: Mode,
    context: Option<String>,
    _proxy: Option<super::browser_proxy::ProxyHandle>,
    policy: SharedPolicy,
    st: StdMutex<State>,
    dialog_tx: watch::Sender<u64>,
    width: u32,
    height: u32,
    touched: StdMutex<Instant>,
    downloads_dir: StdMutex<Option<PathBuf>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let tabs: Vec<String> = lock(&self.st)
            .tabs
            .iter()
            .map(|t| t.target.clone())
            .collect();
        {
            let mut r = lock(&self.browser.routes);
            r.owners.remove(&self.owner);
            r.targets.retain(|_, o| o != &self.owner);
            r.downloads.retain(|_, o| o != &self.owner);
            if let Some(c) = &self.context {
                r.contexts.remove(c);
            }
            for s in r.sessions.values_mut() {
                if s.owner.as_deref() == Some(&self.owner) {
                    s.owner = None;
                }
            }
        }
        let b = self.browser.clone();
        let ctx = self.context.clone();
        // The context (and with it every tab and cookie) goes on the browser
        // runtime; the process itself goes when no session needs it.
        rt().spawn(async move {
            if let Some(c) = ctx {
                let _ = b
                    .conn
                    .call(
                        None,
                        "Target.disposeBrowserContext",
                        json!({"browserContextId": c}),
                        Duration::from_secs(5),
                    )
                    .await;
            } else {
                for t in tabs {
                    b.conn
                        .fire(None, "Target.closeTarget", json!({"targetId": t}));
                }
            }
            drop(b);
            sweep_browsers().await;
        });
    }
}

impl Session {
    async fn create(owner: &str, opts: &SessionOpts) -> Result<Arc<Session>, String> {
        let mode = opts.mode();
        let browser = Browser::get(mode).await?;
        let pol: SharedPolicy = Arc::new(std::sync::RwLock::new(NetPolicy {
            project: opts.project.clone(),
            downloads: opts.downloads.clone(),
            lan_allowed: opts
                .lan_allowed
                .iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            blocked_ports: vec![browser.port],
        }));
        let (proxy, context) = if mode.persistent {
            if let Some(p) = lock(&browser.proxy).as_ref() {
                if p.port != 0 {
                    pol.write()
                        .unwrap_or_else(|e| e.into_inner())
                        .blocked_ports
                        .push(p.port);
                }
            }
            // Owners share the default context; the process proxy's policy
            // learns each owner's LAN approvals (interception stays per owner).
            (None, None)
        } else {
            let proxy = start_proxy(pol.clone()).await?;
            let mut params = json!({"disposeOnDetach": false});
            if proxy.port != 0 {
                params["proxyServer"] = json!(format!("http://127.0.0.1:{}", proxy.port));
                params["proxyBypassList"] = json!("<-loopback>");
                pol.write()
                    .unwrap_or_else(|e| e.into_inner())
                    .blocked_ports
                    .push(proxy.port);
            }
            let r = browser
                .conn
                .call(None, "Target.createBrowserContext", params, CALL_TIMEOUT)
                .await?;
            let ctx = r["browserContextId"]
                .as_str()
                .ok_or("browser: no browser context id")?
                .to_string();
            let _ = browser
                .conn
                .call(
                    None,
                    "Browser.setDownloadBehavior",
                    json!({"behavior": "allowAndName", "browserContextId": ctx, "downloadPath": browser.staging.to_string_lossy(), "eventsEnabled": true}),
                    CALL_TIMEOUT,
                )
                .await;
            (Some(proxy), Some(ctx))
        };
        let (dialog_tx, _) = watch::channel(0u64);
        let s = Arc::new(Session {
            owner: owner.to_string(),
            browser: browser.clone(),
            mode,
            context: context.clone(),
            _proxy: proxy,
            policy: pol,
            st: StdMutex::new(State::default()),
            dialog_tx,
            width: opts.width.clamp(320, 3840),
            height: opts.height.clamp(240, 2160),
            touched: StdMutex::new(Instant::now()),
            downloads_dir: StdMutex::new(opts.downloads.clone()),
        });
        {
            let mut r = lock(&browser.routes);
            r.owners.insert(owner.to_string(), Arc::downgrade(&s));
            if let Some(c) = &context {
                r.contexts.insert(c.clone(), owner.to_string());
            }
        }
        s.sync_proc_lan();
        s.open_tab(None).await?;
        Ok(s)
    }

    /// Update what the owner may reach (the runner calls this every time).
    pub fn configure(&self, opts: &SessionOpts) {
        {
            let mut p = self.policy.write().unwrap_or_else(|e| e.into_inner());
            p.project = opts.project.clone();
            p.downloads = opts.downloads.clone();
            for h in &opts.lan_allowed {
                p.lan_allowed.insert(h.to_ascii_lowercase());
            }
        }
        *lock(&self.downloads_dir) = opts.downloads.clone();
        self.sync_proc_lan();
    }

    /// In persistent mode the proxy is process-wide: give it the union of the
    /// owners' LAN approvals. Interception still enforces each owner's own.
    fn sync_proc_lan(&self) {
        if !self.mode.persistent {
            return;
        }
        let mine: Vec<String> = self
            .policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .lan_allowed
            .iter()
            .cloned()
            .collect();
        let mut p = self
            .browser
            .proc_policy
            .write()
            .unwrap_or_else(|e| e.into_inner());
        p.lan_allowed.extend(mine);
    }

    fn touch(&self) {
        *lock(&self.touched) = Instant::now();
    }

    fn alive(&self) -> bool {
        self.browser.alive()
    }

    /// A session whose page closed its last tab gets a blank one back.
    async fn ensure_tab(&self) -> Result<(), String> {
        if lock(&self.st).tabs.is_empty() {
            self.open_tab(None).await?;
        }
        Ok(())
    }

    /// The debugging port (tests check pages can't reach it).
    #[cfg(test)]
    pub fn cdp_port(&self) -> u16 {
        self.browser.port
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_idle(&self) -> bool {
        lock(&self.touched).elapsed() > IDLE_TIMEOUT
    }

    /// Which browser binary this is ("Chrome"/"Edge"), so a result can say so.
    pub fn browser(&self) -> &str {
        &self.browser.name
    }

    // ── tabs ───────────────────────────────────────────────────────────

    fn active(&self) -> Result<(String, String), String> {
        let st = lock(&self.st);
        let t = st
            .active
            .as_ref()
            .and_then(|a| st.tabs.iter().find(|t| &t.target == a))
            .or_else(|| st.tabs.last())
            .ok_or("browser: this session has no open tab")?;
        Ok((t.target.clone(), t.session.clone()))
    }

    fn on_attached(&self, sid: &str, tid: &str, kind: &str, parent: Option<&str>, info: &Value) {
        let mut st = lock(&self.st);
        if let Some(p) = parent {
            st.parent_of.insert(sid.to_string(), p.to_string());
            if kind == "iframe" {
                st.oopifs.insert(tid.to_string(), sid.to_string());
            }
            return;
        }
        if kind != "page" || st.tabs.iter().any(|t| t.target == tid) {
            return;
        }
        st.next_tab += 1;
        let id = format!("t{}", st.next_tab);
        let opened_by_page = info["openerId"].as_str().is_some();
        st.tabs.push(Tab {
            id: id.clone(),
            target: tid.to_string(),
            session: sid.to_string(),
            url: info["url"].as_str().unwrap_or("").to_string(),
            title: info["title"].as_str().unwrap_or("").to_string(),
            ready: false,
        });
        if opened_by_page {
            // A popup / target=_blank: the page meant the user to look at it.
            st.active = Some(tid.to_string());
            st.new_tabs.push(id);
        }
    }

    fn on_detached(&self, sid: &str) {
        let mut st = lock(&self.st);
        st.worlds.retain(|(s, _), _| s != sid);
        st.oopifs.retain(|_, s| s != sid);
        st.parent_of.remove(sid);
        st.inflight.retain(|(s, _), _| s != sid);
    }

    fn mark_ready(&self, tid: &str) {
        if let Some(t) = lock(&self.st).tabs.iter_mut().find(|t| t.target == tid) {
            t.ready = true;
        }
    }

    fn remove_tab(&self, tid: &str) {
        let mut st = lock(&self.st);
        st.tabs.retain(|t| t.target != tid);
        st.dialogs.remove(tid);
        if st.active.as_deref() == Some(tid) {
            st.active = st.tabs.last().map(|t| t.target.clone());
        }
    }

    fn tab_info_changed(&self, tid: &str, info: &Value) {
        let mut st = lock(&self.st);
        if let Some(t) = st.tabs.iter_mut().find(|t| t.target == tid) {
            if let Some(u) = info["url"].as_str() {
                t.url = u.to_string();
            }
            if let Some(ti) = info["title"].as_str() {
                t.title = ti.to_string();
            }
        }
    }

    /// Create a tab in this session's context, wait for it to be intercepted
    /// and released, and make it active.
    async fn open_tab(&self, url: Option<&str>) -> Result<TabInfo, String> {
        let mut params = json!({"url": "about:blank"});
        if let Some(c) = &self.context {
            params["browserContextId"] = json!(c);
        }
        let r = self
            .browser
            .conn
            .call(None, "Target.createTarget", params, CALL_TIMEOUT)
            .await?;
        let tid = r["targetId"]
            .as_str()
            .ok_or("browser: the new tab came back with no id")?
            .to_string();
        // Claim it: in persistent mode the attach event can't know whose it
        // is. Done under the routes lock, which `configure` also takes to
        // publish readiness, so exactly one side sees the other.
        {
            let mut routes = lock(&self.browser.routes);
            routes.targets.insert(tid.clone(), self.owner.clone());
            let found = routes
                .sessions
                .iter_mut()
                .find(|(_, r)| r.target == tid && r.parent.is_none())
                .map(|(sid, r)| {
                    r.owner = Some(self.owner.clone());
                    r.tab = Some(tid.clone());
                    sid.clone()
                });
            if let Some(sid) = found {
                self.on_attached(&sid, &tid, "page", None, &json!({"url": "about:blank"}));
                if routes.configured.contains(&tid) {
                    self.mark_ready(&tid);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ready = lock(&self.st)
                .tabs
                .iter()
                .any(|t| t.target == tid && t.ready);
            if ready {
                break;
            }
            if Instant::now() > deadline {
                return Err("browser: the new tab never became ready".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        lock(&self.st).active = Some(tid.clone());
        if let Some(u) = url {
            self.goto(u).await?;
        }
        let st = lock(&self.st);
        let t = st
            .tabs
            .iter()
            .find(|t| t.target == tid)
            .ok_or("browser: the new tab closed")?;
        Ok(TabInfo {
            id: t.id.clone(),
            url: t.url.clone(),
            title: t.title.clone(),
            active: true,
        })
    }

    // ── calls ──────────────────────────────────────────────────────────

    async fn call(&self, sid: &str, method: &str, params: Value) -> Result<Value, String> {
        self.touch();
        self.browser
            .conn
            .call(Some(sid), method, params, CALL_TIMEOUT)
            .await
    }

    fn dialog_for_session(&self, sid: &str) -> Option<DialogInfo> {
        let st = lock(&self.st);
        st.dialogs
            .values()
            .find(|(s, _)| s == sid)
            .map(|(_, d)| d.clone())
            .or_else(|| {
                // A dialog in the tab blocks its frames too.
                let tab = st
                    .tabs
                    .iter()
                    .find(|t| t.session == sid)
                    .map(|t| t.target.clone());
                tab.and_then(|t| st.dialogs.get(&t).map(|(_, d)| d.clone()))
            })
    }

    fn dialog_error(d: &DialogInfo) -> String {
        format!(
            "{DIALOG_OPEN}: {}() says {} — answer it with the `dialog` action (accept: true/false) first.",
            d.kind,
            json_str(&d.message.chars().take(200).collect::<String>())
        )
    }

    /// A call that a JavaScript dialog can block (anything the renderer must
    /// answer). Returns early when a dialog opens instead of hanging until the
    /// call times out: an `alert()` freezes the page's main thread.
    async fn guarded(
        &self,
        sid: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if let Some(d) = self.dialog_for_session(sid) {
            if !method.starts_with("Input.") {
                return Err(Self::dialog_error(&d));
            }
        }
        self.touch();
        let mut rx = self.dialog_tx.subscribe();
        let call = self.browser.conn.call(Some(sid), method, params, timeout);
        tokio::pin!(call);
        loop {
            tokio::select! {
                r = &mut call => return r,
                ch = rx.changed() => {
                    if ch.is_err() {
                        return (&mut call).await;
                    }
                    if let Some(d) = self.dialog_for_session(sid) {
                        // Input that opened a dialog was delivered: that is
                        // its effect, not a failure.
                        return if method.starts_with("Input.") { Ok(Value::Null) } else { Err(Self::dialog_error(&d)) };
                    }
                }
            }
        }
    }

    /// Issue one CDP command on the active tab and return its `result`.
    pub async fn raw(&self, method: &str, params: Value) -> Result<Value, String> {
        let (_, sid) = self.active()?;
        self.guarded(&sid, method, params, CALL_TIMEOUT).await
    }

    /// `cdp` action: a raw command on the active tab, or the browser with
    /// `browser: true`. Developer mode only, and always approved first.
    pub async fn cdp(
        &self,
        method: &str,
        params: Value,
        browser_level: bool,
    ) -> Result<Value, String> {
        if browser_level {
            return self
                .browser
                .conn
                .call(None, method, params, CALL_TIMEOUT)
                .await;
        }
        self.raw(method, params).await
    }

    /// Evaluate `expr` in the page's own JavaScript context and return its
    /// value (the `js` action). `awaitPromise` is on: the common case ends in a
    /// fetch or a click that navigates.
    pub async fn eval(&self, expr: &str) -> Result<Value, String> {
        let (_, sid) = self.active()?;
        let r = self
            .guarded(
                &sid,
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true, "userGesture": true }),
                EVAL_TIMEOUT,
            )
            .await?;
        if let Some(d) = r.get("exceptionDetails") {
            let why = d["exception"]["description"]
                .as_str()
                .or(d["text"].as_str())
                .unwrap_or("the expression threw");
            return Err(format!("browser: {why}"));
        }
        Ok(r["result"].get("value").cloned().unwrap_or(Value::Null))
    }

    /// The isolated world for `frame` in `sid`, created on first use.
    async fn world(&self, sid: &str, frame: &str) -> Result<i64, String> {
        if let Some(c) = lock(&self.st)
            .worlds
            .get(&(sid.to_string(), frame.to_string()))
        {
            return Ok(*c);
        }
        let r = self
            .guarded(
                sid,
                "Page.createIsolatedWorld",
                json!({"frameId": frame, "worldName": WORLD, "grantUniveralAccess": false}),
                CALL_TIMEOUT,
            )
            .await?;
        let ctx = r["executionContextId"]
            .as_i64()
            .ok_or("browser: no isolated world for that frame")?;
        lock(&self.st)
            .worlds
            .insert((sid.to_string(), frame.to_string()), ctx);
        Ok(ctx)
    }

    /// Run `call` (an expression using `__ol`) in a frame's isolated world.
    async fn ol_raw(
        &self,
        sid: &str,
        frame: &str,
        prefix: &str,
        call: &str,
        by_value: bool,
    ) -> Result<Value, String> {
        let expr = format!("({})({});\n{call}", snap::RUNTIME_JS, json_str(prefix));
        let mut last = String::new();
        for _ in 0..3 {
            let ctx = self.world(sid, frame).await?;
            let r = self
                .guarded(
                    sid,
                    "Runtime.evaluate",
                    json!({"expression": expr, "contextId": ctx, "returnByValue": by_value, "awaitPromise": true, "userGesture": true}),
                    EVAL_TIMEOUT,
                )
                .await;
            match r {
                Ok(r) => {
                    if let Some(d) = r.get("exceptionDetails") {
                        let why = d["exception"]["description"]
                            .as_str()
                            .or(d["text"].as_str())
                            .unwrap_or("the page helper threw");
                        return Err(format!("browser: {why}"));
                    }
                    return Ok(r["result"].clone());
                }
                Err(e) if e.contains("context") || e.contains("Execution context") => {
                    // The document changed under us: a new world, then retry.
                    lock(&self.st)
                        .worlds
                        .remove(&(sid.to_string(), frame.to_string()));
                    last = e;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    async fn ol(&self, sid: &str, frame: &str, prefix: &str, call: &str) -> Result<Value, String> {
        let r = self.ol_raw(sid, frame, prefix, call, true).await?;
        Ok(r.get("value").cloned().unwrap_or(Value::Null))
    }

    /// `__ol` call in the active tab's main frame.
    async fn ol_main(&self, call: &str) -> Result<Value, String> {
        let (tid, sid) = self.active()?;
        self.ol(&sid, &tid, "e", call).await
    }

    /// Where a ref lives: (session, frame, prefix).
    fn locate(&self, r: &str) -> Result<(String, String, String), String> {
        match snap::ref_frame(r) {
            None => {
                let (tid, sid) = self.active()?;
                Ok((sid, tid, "e".into()))
            }
            Some(n) => {
                let st = lock(&self.st);
                let (sid, frame) =
                    st.frame_by_idx.get(&n).cloned().ok_or_else(|| {
                        format!("browser: frame f{n} is gone — take a new snapshot")
                    })?;
                Ok((sid, frame, format!("f{n}e")))
            }
        }
    }

    /// Offset of `frame`'s viewport in the tab's viewport: the content box of
    /// its owner element, plus the owner document's own offset when that is an
    /// out-of-process frame itself.
    fn frame_offset<'a>(
        &'a self,
        sid: &'a str,
        frame: &'a str,
    ) -> BoxFut<'a, Result<(f64, f64), String>> {
        Box::pin(async move {
            let (tab, tab_sid) = self.active()?;
            if frame == tab {
                return Ok((0.0, 0.0));
            }
            // An OOPIF's owner element lives in its parent session; a frame in
            // the same process has its owner in the same session.
            let owner_sid = {
                let st = lock(&self.st);
                match st.oopifs.get(frame) {
                    Some(s) if s == sid => st
                        .parent_of
                        .get(sid)
                        .cloned()
                        .unwrap_or_else(|| tab_sid.clone()),
                    _ => sid.to_string(),
                }
            };
            let o = self
                .call(&owner_sid, "DOM.getFrameOwner", json!({"frameId": frame}))
                .await?;
            let bm = self
                .call(
                    &owner_sid,
                    "DOM.getBoxModel",
                    json!({"backendNodeId": o["backendNodeId"]}),
                )
                .await?;
            let q = &bm["model"]["content"];
            let (x, y) = (q[0].as_f64().unwrap_or(0.0), q[1].as_f64().unwrap_or(0.0));
            if owner_sid == tab_sid {
                return Ok((x, y));
            }
            let root = lock(&self.browser.routes)
                .sessions
                .get(&owner_sid)
                .map(|r| r.target.clone())
                .ok_or("browser: that frame's parent is gone")?;
            let (px, py) = self.frame_offset(&owner_sid, &root).await?;
            Ok((x + px, y + py))
        })
    }

    // ── navigation ─────────────────────────────────────────────────────

    /// Navigate, then wait for the page to settle, so a screenshot afterwards
    /// shows a page that has painted rather than a spinner.
    pub async fn goto(&self, url: &str) -> Result<PageState, String> {
        let (_, sid) = self.active()?;
        let r = self
            .guarded(&sid, "Page.navigate", json!({ "url": url }), CALL_TIMEOUT)
            .await?;
        if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            let why = self.block_reason_for(url);
            return Err(match why {
                Some(w) => format!("browser: {url} was blocked: {w}"),
                None if e.contains("BLOCKED_BY_CLIENT") => {
                    format!("browser: {url} was blocked by the browser's network policy")
                }
                None => format!("browser: couldn't open {url}: {e}"),
            });
        }
        self.settle().await?;
        self.state().await
    }

    fn block_reason_for(&self, url: &str) -> Option<String> {
        let st = lock(&self.st);
        st.blocked
            .iter()
            .rev()
            .find(|b| b.contains(url) || url.contains(b.split(' ').next().unwrap_or("\u{0}")))
            .cloned()
    }

    /// Wait until the page stops changing: the load finishes, the network goes
    /// quiet, the DOM stops mutating — each step bounded, and none of them
    /// waiting on a page frozen behind a dialog.
    pub async fn settle(&self) -> Result<(), String> {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let (tid, sid) = self.active()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.dialog_for_session(&sid).is_some() {
                return Ok(());
            }
            let loading = lock(&self.st).loading.get(&tid).copied().unwrap_or(false);
            if !loading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = self
            .network_idle(Duration::from_millis(300), Duration::from_secs(2))
            .await;
        if self.dialog_for_session(&sid).is_none() {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                self.ol(&sid, &tid, "e", "__ol.stable()"),
            )
            .await;
        }
        Ok(())
    }

    /// True once no request (younger than 5s — long polls don't count) has
    /// been in flight for `quiet`.
    async fn network_idle(&self, quiet: Duration, max: Duration) -> bool {
        let (tid, _) = match self.active() {
            Ok(x) => x,
            Err(_) => return true,
        };
        let deadline = Instant::now() + max;
        let mut since = Instant::now();
        loop {
            let busy = lock(&self.st)
                .inflight
                .values()
                .any(|(tab, t)| tab == &tid && t.elapsed() < Duration::from_secs(5));
            if busy {
                since = Instant::now();
            } else if since.elapsed() >= quiet {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Where we are, from the browser's own target info: never blocked by a
    /// dialog or a busy page.
    pub async fn state(&self) -> Result<PageState, String> {
        let (tid, _) = self.active()?;
        let r = self
            .browser
            .conn
            .call(
                None,
                "Target.getTargetInfo",
                json!({"targetId": tid}),
                CALL_TIMEOUT,
            )
            .await?;
        let st = PageState {
            url: r["targetInfo"]["url"].as_str().unwrap_or("").to_string(),
            title: r["targetInfo"]["title"].as_str().unwrap_or("").to_string(),
        };
        self.tab_info_changed(&tid, &r["targetInfo"]);
        Ok(st)
    }

    /// The last known page, without a round trip.
    pub fn remembered(&self) -> PageState {
        let st = lock(&self.st);
        let t = st
            .active
            .as_ref()
            .and_then(|a| st.tabs.iter().find(|t| &t.target == a))
            .or_else(|| st.tabs.last());
        t.map(|t| PageState {
            url: t.url.clone(),
            title: t.title.clone(),
        })
        .unwrap_or_default()
    }

    /// Click a CSS selector with a real input event at its centre, rather than
    /// `element.click()`: the scripted version skips hit-testing and anything
    /// listening for a trusted event.
    pub async fn click(&self, selector: &str) -> Result<PageState, String> {
        let expr = format!(
            r#"(() => {{
              const el = document.querySelector({sel});
              if (!el) return JSON.stringify({{ error: "no element matches {raw}" }});
              el.scrollIntoView({{ block: "center", inline: "center" }});
              const r = el.getBoundingClientRect();
              if (r.width === 0 || r.height === 0) return JSON.stringify({{ error: "{raw} has no size, so it is probably hidden" }});
              return JSON.stringify({{ x: r.left + r.width / 2, y: r.top + r.height / 2 }});
            }})()"#,
            sel = json_str(selector),
            raw = selector.replace('"', "'")
        );
        let v = self.eval(&expr).await?;
        let pt: Value = serde_json::from_str(v.as_str().unwrap_or("{}")).unwrap_or(json!({}));
        if let Some(e) = pt["error"].as_str() {
            return Err(format!("browser: {e}"));
        }
        let (Some(x), Some(y)) = (pt["x"].as_f64(), pt["y"].as_f64()) else {
            return Err(format!(
                "browser: could not work out where `{selector}` is on screen"
            ));
        };
        for ev in ["mousePressed", "mouseReleased"] {
            self.raw(
                "Input.dispatchMouseEvent",
                json!({ "type": ev, "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
        }
        self.settle().await?;
        self.state().await
    }

    /// A short label for the UI and the transcript.
    pub fn label(&self) -> String {
        let p = self.remembered();
        if p.title.is_empty() {
            p.url
        } else {
            format!("{} — {}", p.title, p.url)
        }
    }

    async fn history_step(&self, delta: i64) -> Result<bool, String> {
        let h = self.raw("Page.getNavigationHistory", json!({})).await?;
        let idx = h["currentIndex"].as_i64().unwrap_or(0) + delta;
        let entries = h["entries"].as_array().cloned().unwrap_or_default();
        if idx < 0 || idx as usize >= entries.len() {
            return Ok(false);
        }
        let e = &entries[idx as usize];
        let url = e["url"].as_str().unwrap_or("");
        // History can hold a page the policy has since refused (a LAN host
        // approval that was session-only, a file outside a new project).
        if let NetVerdict::Block(why) = self.policy_check(url) {
            return Err(format!("browser: {url} is blocked: {why}"));
        }
        self.raw("Page.navigateToHistoryEntry", json!({"entryId": e["id"]}))
            .await?;
        self.settle().await?;
        Ok(true)
    }

    /// The full verdict on opening `url`: as written, then where it resolves.
    pub async fn verdict(&self, url: &str) -> NetVerdict {
        judge(&self.policy, url).await
    }

    pub fn policy_check(&self, url: &str) -> NetVerdict {
        self.policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .check_url(url)
    }

    // ── snapshot & targets ─────────────────────────────────────────────

    /// Hand closed shadow roots to the isolated world: neither the page's
    /// script nor ours can reach them from JavaScript, but CDP can.
    async fn register_closed_roots(&self, sid: &str, frame: &str) {
        let n = self
            .ol(sid, frame, "e", "__ol.count()")
            .await
            .ok()
            .and_then(|v| v.as_u64());
        if n.unwrap_or(u64::MAX) > 6000 {
            return;
        }
        let Ok(doc) = self
            .call(sid, "DOM.getDocument", json!({"depth": -1, "pierce": true}))
            .await
        else {
            return;
        };
        fn collect(n: &Value, out: &mut Vec<i64>) {
            for r in n["shadowRoots"].as_array().into_iter().flatten() {
                if r["shadowRootType"] == "closed" {
                    if let Some(id) = r["backendNodeId"].as_i64() {
                        out.push(id);
                    }
                }
                collect(r, out);
            }
            // Not into iframes: their nodes belong to another world.
            for c in n["children"].as_array().into_iter().flatten() {
                collect(c, out);
            }
        }
        let mut ids = Vec::new();
        collect(&doc["root"], &mut ids);
        let _ = self.call(sid, "DOM.disable", json!({})).await;
        if ids.is_empty() {
            return;
        }
        let Ok(ctx) = self.world(sid, frame).await else {
            return;
        };
        for id in ids.into_iter().take(100) {
            let Ok(o) = self
                .call(
                    sid,
                    "DOM.resolveNode",
                    json!({"backendNodeId": id, "executionContextId": ctx}),
                )
                .await
            else {
                continue;
            };
            if let Some(obj) = o["object"]["objectId"].as_str() {
                let _ = self
                    .call(
                        sid,
                        "Runtime.callFunctionOn",
                        json!({"objectId": obj, "functionDeclaration": "function(){ return globalThis.__ol ? globalThis.__ol.registerClosed(this) : false; }", "returnByValue": true}),
                    )
                    .await;
            }
        }
    }

    /// A page snapshot: an outline of the page with a ref on every element the
    /// agent can act on. Cross-origin frames are snapshotted in their own
    /// frame (and process) and spliced in.
    pub async fn snapshot(
        &self,
        selector: Option<&str>,
        interactive_only: bool,
        budget: usize,
    ) -> Result<String, String> {
        let (tid, sid) = self.active()?;
        self.register_closed_roots(&sid, &tid).await;
        let opts =
            json!({"selector": selector, "interactiveOnly": interactive_only, "maxNodes": 5000});
        let v = self
            .ol(&sid, &tid, "e", &format!("__ol.snapshot({opts})"))
            .await?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("browser: {e}"));
        }
        let mut nodes: Vec<SnapNode> =
            serde_json::from_value(v["nodes"].clone()).unwrap_or_default();
        let mut frames_left = 12usize;
        self.splice_frames(
            &mut nodes,
            &sid,
            &tid,
            "e",
            interactive_only,
            0,
            &mut frames_left,
        )
        .await;
        let r = snap::format_snapshot(&nodes, budget);
        let mut out = String::new();
        if r.text.is_empty() {
            out.push_str("(nothing visible on the page)");
        } else {
            out.push_str(&r.text);
        }
        if r.omitted > 0 || v["truncated"].as_bool().unwrap_or(false) {
            out.push_str(&format!(
                "\n[… snapshot truncated: {} more nodes not shown. Pass `selector` to snapshot one part of the page, or `interactive_only: true`.]",
                r.omitted.max(1)
            ));
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn splice_frames<'a>(
        &'a self,
        nodes: &'a mut [SnapNode],
        sid: &'a str,
        frame: &'a str,
        prefix: &'a str,
        interactive_only: bool,
        depth: usize,
        left: &'a mut usize,
    ) -> BoxFut<'a, ()> {
        Box::pin(async move {
            for n in nodes.iter_mut() {
                if n.frame == "remote" && !n.ref_.is_empty() && depth < 3 && *left > 0 {
                    *left -= 1;
                    if let Ok((csid, cframe, cprefix)) =
                        self.frame_for_ref(sid, frame, prefix, &n.ref_).await
                    {
                        let opts = json!({"interactiveOnly": interactive_only, "maxNodes": 2000});
                        if let Ok(v) = self
                            .ol(&csid, &cframe, &cprefix, &format!("__ol.snapshot({opts})"))
                            .await
                        {
                            let mut kids: Vec<SnapNode> =
                                serde_json::from_value(v["nodes"].clone()).unwrap_or_default();
                            self.splice_frames(
                                &mut kids,
                                &csid,
                                &cframe,
                                &cprefix,
                                interactive_only,
                                depth + 1,
                                left,
                            )
                            .await;
                            n.children = kids;
                        }
                    }
                } else if !n.children.is_empty() {
                    self.splice_frames(
                        &mut n.children,
                        sid,
                        frame,
                        prefix,
                        interactive_only,
                        depth,
                        left,
                    )
                    .await;
                }
            }
        })
    }

    /// The frame an iframe element (by ref) hosts: its session, id and prefix.
    async fn frame_for_ref(
        &self,
        sid: &str,
        frame: &str,
        prefix: &str,
        r: &str,
    ) -> Result<(String, String, String), String> {
        let o = self
            .ol_raw(
                sid,
                frame,
                prefix,
                &format!("__ol.el({})", json_str(r)),
                false,
            )
            .await?;
        let obj = o["objectId"].as_str().ok_or("browser: no such iframe")?;
        let d = self
            .call(sid, "DOM.describeNode", json!({"objectId": obj}))
            .await?;
        let _ = self
            .call(sid, "Runtime.releaseObject", json!({"objectId": obj}))
            .await;
        let child = d["node"]["frameId"]
            .as_str()
            .ok_or("browser: that iframe has no frame yet")?
            .to_string();
        let mut st = lock(&self.st);
        let csid = st
            .oopifs
            .get(&child)
            .cloned()
            .unwrap_or_else(|| sid.to_string());
        let idx = match st.frame_idx.get(&child) {
            Some(i) => *i,
            None => {
                st.next_frame += 1;
                let i = st.next_frame;
                st.frame_idx.insert(child.clone(), i);
                i
            }
        };
        st.frame_by_idx.insert(idx, (csid.clone(), child.clone()));
        Ok((csid, child, format!("f{idx}e")))
    }

    /// Resolve an action's target to one element, or explain what is there.
    pub async fn resolve(&self, t: &TargetSpec) -> Result<Resolved, String> {
        let (sid, frame, prefix, call, kind, query) = if let Some(r) = &t.ref_ {
            let (sid, frame, prefix) = self.locate(r)?;
            (sid, frame, prefix, json!({"ref": r}), "ref", r.clone())
        } else if let Some(sel) = &t.selector {
            let (tid, sid) = self.active()?;
            (
                sid,
                tid,
                "e".into(),
                json!({"selector": sel}),
                "selector",
                sel.clone(),
            )
        } else if let Some(text) = &t.text {
            let (tid, sid) = self.active()?;
            (
                sid,
                tid,
                "e".into(),
                json!({"text": text}),
                "text",
                text.clone(),
            )
        } else if let Some((x, y)) = t.point {
            let (tid, sid) = self.active()?;
            let v = self
                .ol(&sid, &tid, "e", &format!("__ol.elementAt({x}, {y})"))
                .await?;
            if v.is_null() {
                return Err(format!("browser: there is nothing at ({x}, {y})"));
            }
            return Ok(Resolved {
                sid,
                frame: tid,
                prefix: "e".into(),
                ref_: v["ref"].as_str().unwrap_or("").to_string(),
                role: v["role"].as_str().unwrap_or("").to_string(),
                name: v["name"].as_str().unwrap_or("").to_string(),
                tag: v["tag"].as_str().unwrap_or("").to_string(),
                matches: 1,
            });
        } else {
            return Err("browser: this action needs a target: `ref` (from a snapshot), `selector`, `text`, or `x`/`y`.".into());
        };
        let v = self
            .ol(&sid, &frame, &prefix, &format!("__ol.resolve({call})"))
            .await?;
        if let Some(e) = v["error"].as_str() {
            let cands: Vec<snap::Candidate> =
                serde_json::from_value(v["candidates"].clone()).unwrap_or_default();
            return Err(if e == "nomatch" {
                format!("browser: {}", snap::no_match(kind, &query, &cands))
            } else {
                let _ = cands;
                format!("browser: {e}")
            });
        }
        Ok(Resolved {
            sid,
            frame,
            prefix,
            ref_: v["ref"].as_str().unwrap_or("").to_string(),
            role: v["role"].as_str().unwrap_or("").to_string(),
            name: v["name"].as_str().unwrap_or("").to_string(),
            tag: v["tag"].as_str().unwrap_or("").to_string(),
            matches: v["matches"].as_u64().unwrap_or(1),
        })
    }

    /// Facts about a resolved element for the sensitivity check.
    pub async fn facts(&self, r: &Resolved) -> Option<policy::TargetFacts> {
        if r.ref_.is_empty() {
            return None;
        }
        let v = self
            .ol(
                &r.sid,
                &r.frame,
                &r.prefix,
                &format!("__ol.facts({})", json_str(&r.ref_)),
            )
            .await
            .ok()?;
        serde_json::from_value(v).ok()
    }

    /// The focused element (for `press`/`type` with no target), if any.
    pub async fn focused(&self) -> Option<Resolved> {
        let (tid, sid) = self.active().ok()?;
        let v = self.ol(&sid, &tid, "e", "__ol.focusedRef()").await.ok()?;
        let r = v.as_str()?.to_string();
        self.resolve(&TargetSpec {
            ref_: Some(r),
            ..Default::default()
        })
        .await
        .ok()
    }

    /// Wait until `r` can take a pointer event, and return where (tab
    /// viewport coordinates).
    pub async fn point_for(
        &self,
        r: &Resolved,
        force: bool,
        timeout: Duration,
    ) -> Result<(f64, f64), String> {
        let opts = json!({"hitTest": !force, "allowLabel": true, "needEnabled": !force});
        let deadline = Instant::now() + timeout;
        let what = r.describe();
        let mut last;
        loop {
            let v = self
                .ol(
                    &r.sid,
                    &r.frame,
                    &r.prefix,
                    &format!("__ol.actionable({}, {opts})", json_str(&r.ref_)),
                )
                .await?;
            let state = v["state"].as_str().unwrap_or("");
            match state {
                "ok" => {
                    let (ox, oy) = self.frame_offset(&r.sid, &r.frame).await?;
                    return Ok((
                        v["x"].as_f64().unwrap_or(0.0) + ox,
                        v["y"].as_f64().unwrap_or(0.0) + oy,
                    ));
                }
                "detached" => {
                    return Err(format!(
                        "browser: {what} is gone (the page changed) — take a new snapshot"
                    ))
                }
                "covered" => {
                    let by = &v["by"];
                    last = format!(
                        "is covered by {} at its click point (a modal, banner or overlay?). Close what covers it, or pass `force: true`",
                        snap::Candidate {
                            ref_: by["ref"].as_str().unwrap_or("").into(),
                            role: by["role"].as_str().or(by["tag"].as_str()).unwrap_or("").into(),
                            name: by["name"].as_str().unwrap_or("").into(),
                        }
                        .describe()
                    );
                }
                "hidden" => last = "is not visible".into(),
                "disabled" => last = "is disabled".into(),
                "moving" => last = "is still moving (animating)".into(),
                "offscreen" => {
                    last = "is outside the viewport and can't be scrolled into it".into()
                }
                other => last = format!("is not actionable ({other})"),
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "browser: {what} {last} (waited {:.1}s)",
                    timeout.as_secs_f64()
                ));
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
    }

    // ── input ──────────────────────────────────────────────────────────

    async fn mouse(&self, kind: &str, x: f64, y: f64, extra: Value) -> Result<(), String> {
        let (_, sid) = self.active()?;
        let mut p = json!({"type": kind, "x": x, "y": y});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        self.guarded(&sid, "Input.dispatchMouseEvent", p, CALL_TIMEOUT)
            .await
            .map(|_| ())
    }

    /// A real click (or double/right click) at a point.
    pub async fn click_at(
        &self,
        x: f64,
        y: f64,
        button: &str,
        count: u32,
        modifiers: u32,
    ) -> Result<(), String> {
        self.mouse("mouseMoved", x, y, json!({"modifiers": modifiers}))
            .await?;
        for i in 1..=count.clamp(1, 3) {
            let e = json!({"button": button, "clickCount": i, "modifiers": modifiers, "buttons": if button == "right" { 2 } else { 1 }});
            self.mouse("mousePressed", x, y, e.clone()).await?;
            self.mouse("mouseReleased", x, y, e).await?;
        }
        Ok(())
    }

    pub async fn hover_at(&self, x: f64, y: f64) -> Result<(), String> {
        self.mouse("mouseMoved", x, y, json!({})).await
    }

    pub async fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<(), String> {
        self.mouse("mouseWheel", x, y, json!({"deltaX": dx, "deltaY": dy}))
            .await
    }

    /// Drag with real mouse events, upgraded to an HTML5 drag-and-drop when
    /// the page starts one (Chrome won't run native DnD from synthetic input
    /// unless the harness intercepts and replays it).
    pub async fn drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
        let (_, sid) = self.active()?;
        lock(&self.st).drag = None;
        let _ = self
            .call(&sid, "Input.setInterceptDrags", json!({"enabled": true}))
            .await;
        self.mouse("mouseMoved", from.0, from.1, json!({})).await?;
        self.mouse(
            "mousePressed",
            from.0,
            from.1,
            json!({"button": "left", "buttons": 1, "clickCount": 1}),
        )
        .await?;
        let steps = 8;
        for i in 1..=steps {
            let f = i as f64 / steps as f64;
            let (x, y) = (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
            self.mouse("mouseMoved", x, y, json!({"button": "left", "buttons": 1}))
                .await?;
            if lock(&self.st).drag.is_some() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let data = lock(&self.st).drag.take();
        if let Some(data) = data {
            for kind in ["dragEnter", "dragOver", "drop"] {
                self.guarded(
                    &sid,
                    "Input.dispatchDragEvent",
                    json!({"type": kind, "x": to.0, "y": to.1, "data": data}),
                    CALL_TIMEOUT,
                )
                .await?;
            }
        }
        self.mouse(
            "mouseReleased",
            to.0,
            to.1,
            json!({"button": "left", "buttons": 0, "clickCount": 1}),
        )
        .await?;
        let _ = self
            .call(&sid, "Input.setInterceptDrags", json!({"enabled": false}))
            .await;
        Ok(())
    }

    /// Type into whatever has focus. `insertText` fires the input events a real
    /// keystroke does, without needing per-character key events.
    pub async fn type_text(&self, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Err("browser: `text` is empty".into());
        }
        self.raw("Input.insertText", json!({ "text": text }))
            .await?;
        Ok(())
    }

    /// Press keys / combos (`Control+A`, `Enter`, `Shift+Tab`).
    pub async fn press(&self, combos: &[snap::Combo]) -> Result<(), String> {
        let (_, sid) = self.active()?;
        for c in combos {
            let mut mods = 0u32;
            for m in &c.mods {
                mods |= match m.key.as_str() {
                    "Alt" => snap::MOD_ALT,
                    "Control" => snap::MOD_CTRL,
                    "Meta" => snap::MOD_META,
                    _ => snap::MOD_SHIFT,
                };
                self.guarded(&sid, "Input.dispatchKeyEvent", json!({"type": "rawKeyDown", "key": m.key, "code": m.code, "windowsVirtualKeyCode": m.vk, "modifiers": mods, "location": m.location}), CALL_TIMEOUT).await?;
            }
            let k = &c.key;
            let mut down = json!({"type": if k.text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": k.key, "code": k.code, "windowsVirtualKeyCode": k.vk, "modifiers": c.modifiers, "location": k.location});
            if let Some(t) = &k.text {
                down["text"] = json!(t);
                down["unmodifiedText"] = json!(t);
            }
            if !c.commands.is_empty() {
                down["commands"] = json!(c.commands);
            }
            self.guarded(&sid, "Input.dispatchKeyEvent", down, CALL_TIMEOUT)
                .await?;
            self.guarded(&sid, "Input.dispatchKeyEvent", json!({"type": "keyUp", "key": k.key, "code": k.code, "windowsVirtualKeyCode": k.vk, "modifiers": c.modifiers, "location": k.location}), CALL_TIMEOUT).await?;
            for m in c.mods.iter().rev() {
                mods &= !match m.key.as_str() {
                    "Alt" => snap::MOD_ALT,
                    "Control" => snap::MOD_CTRL,
                    "Meta" => snap::MOD_META,
                    _ => snap::MOD_SHIFT,
                };
                self.guarded(&sid, "Input.dispatchKeyEvent", json!({"type": "keyUp", "key": m.key, "code": m.code, "windowsVirtualKeyCode": m.vk, "modifiers": mods, "location": m.location}), CALL_TIMEOUT).await?;
            }
        }
        Ok(())
    }

    /// Focus an element (optionally selecting its contents, for `fill`).
    pub async fn focus(&self, r: &Resolved, select_all: bool) -> Result<bool, String> {
        let v = self
            .ol(
                &r.sid,
                &r.frame,
                &r.prefix,
                &format!("__ol.focus({}, {select_all})", json_str(&r.ref_)),
            )
            .await?;
        if v["error"].is_string() {
            return Err(format!(
                "browser: {} is gone — take a new snapshot",
                r.describe()
            ));
        }
        Ok(v["editable"].as_bool().unwrap_or(false))
    }

    pub async fn value_of(&self, r: &Resolved) -> Option<String> {
        self.ol(
            &r.sid,
            &r.frame,
            &r.prefix,
            &format!("__ol.valueNow({})", json_str(&r.ref_)),
        )
        .await
        .ok()?
        .as_str()
        .map(str::to_string)
    }

    pub async fn checked_of(&self, r: &Resolved) -> Option<bool> {
        self.ol(
            &r.sid,
            &r.frame,
            &r.prefix,
            &format!("__ol.checkedNow({})", json_str(&r.ref_)),
        )
        .await
        .ok()?
        .as_bool()
    }

    pub async fn select_option(
        &self,
        r: &Resolved,
        values: &[String],
    ) -> Result<Vec<String>, String> {
        let v = self
            .ol(
                &r.sid,
                &r.frame,
                &r.prefix,
                &format!(
                    "__ol.selectOption({}, {})",
                    json_str(&r.ref_),
                    json!(values)
                ),
            )
            .await?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("browser: {e}"));
        }
        Ok(v["selected"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Put files into a file input (`DOM.setFileInputFiles`). The runner has
    /// already checked every path is inside the project.
    pub async fn set_files(&self, r: &Resolved, files: &[PathBuf]) -> Result<(), String> {
        let v = self
            .ol(
                &r.sid,
                &r.frame,
                &r.prefix,
                &format!("__ol.fileInput({})", json_str(&r.ref_)),
            )
            .await?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("browser: {e}"));
        }
        if files.len() > 1 && !v["multiple"].as_bool().unwrap_or(false) {
            return Err("browser: that file input takes one file".into());
        }
        let input_ref = v["ref"].as_str().unwrap_or(&r.ref_).to_string();
        let o = self
            .ol_raw(
                &r.sid,
                &r.frame,
                &r.prefix,
                &format!("__ol.el({})", json_str(&input_ref)),
                false,
            )
            .await?;
        let obj = o["objectId"]
            .as_str()
            .ok_or("browser: the file input is gone")?;
        let paths: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let res = self
            .guarded(
                &r.sid,
                "DOM.setFileInputFiles",
                json!({"files": paths, "objectId": obj}),
                CALL_TIMEOUT,
            )
            .await;
        let _ = self
            .call(&r.sid, "Runtime.releaseObject", json!({"objectId": obj}))
            .await;
        res.map(|_| ())
    }

    // ── reading ────────────────────────────────────────────────────────

    pub async fn read_text(&self, selector: Option<&str>, max: usize) -> Result<Value, String> {
        let v = self
            .ol_main(&format!("__ol.readText({max}, {})", json!(selector)))
            .await?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("browser: {e}"));
        }
        Ok(v)
    }

    pub async fn query(&self, selector: &str, limit: usize) -> Result<Value, String> {
        let v = self
            .ol_main(&format!("__ol.queryAll({}, {limit})", json_str(selector)))
            .await?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("browser: {e}"));
        }
        Ok(v)
    }

    pub async fn page_info(&self) -> Result<Value, String> {
        self.ol_main("__ol.info()").await
    }

    /// An isolated-world expression in the main frame (storage, perf timing).
    pub async fn world_eval(&self, expr: &str) -> Result<Value, String> {
        self.ol_main(expr).await
    }

    pub async fn selector_state(&self, sel: &str) -> Result<Value, String> {
        self.ol_main(&format!("__ol.selectorState({})", json_str(sel)))
            .await
    }

    pub async fn text_present(&self, text: &str) -> Result<bool, String> {
        Ok(self
            .ol_main(&format!("__ol.textPresent({})", json_str(text)))
            .await?
            .as_bool()
            .unwrap_or(false))
    }

    pub async fn wait_network_idle(&self, timeout: Duration) -> bool {
        self.network_idle(Duration::from_millis(500), timeout).await
    }

    pub fn is_loading(&self) -> bool {
        match self.active() {
            Ok((tid, _)) => lock(&self.st).loading.get(&tid).copied().unwrap_or(false),
            Err(_) => false,
        }
    }

    pub async fn cookies(&self) -> Result<Vec<Value>, String> {
        let url = self.state().await?.url;
        if !url.starts_with("http") {
            return Ok(vec![]);
        }
        let r = self
            .raw("Network.getCookies", json!({"urls": [url]}))
            .await?;
        Ok(r["cookies"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|c| {
                let http_only = c["httpOnly"].as_bool().unwrap_or(false);
                json!({
                    "name": c["name"],
                    "value": if http_only { json!("[httpOnly]") } else { json!(c["value"].as_str().unwrap_or("").chars().take(300).collect::<String>()) },
                    "domain": c["domain"], "path": c["path"], "secure": c["secure"],
                    "httpOnly": http_only, "sameSite": c["sameSite"], "session": c["session"],
                })
            })
            .collect())
    }

    pub async fn perf(&self) -> Result<Value, String> {
        let (_, sid) = self.active()?;
        let first = {
            let mut st = lock(&self.st);
            let f = !st.perf_on;
            st.perf_on = true;
            f
        };
        if first {
            let _ = self.call(&sid, "Performance.enable", json!({})).await;
        }
        let m = self.raw("Performance.getMetrics", json!({})).await?;
        let wanted = [
            "Documents",
            "Frames",
            "JSEventListeners",
            "Nodes",
            "LayoutCount",
            "RecalcStyleCount",
            "LayoutDuration",
            "ScriptDuration",
            "TaskDuration",
            "JSHeapUsedSize",
            "JSHeapTotalSize",
        ];
        let mut metrics = serde_json::Map::new();
        for x in m["metrics"].as_array().into_iter().flatten() {
            let n = x["name"].as_str().unwrap_or("");
            if wanted.contains(&n) {
                metrics.insert(n.to_string(), x["value"].clone());
            }
        }
        let timing = self
            .world_eval("(() => { const n = performance.getEntriesByType('navigation')[0]; const p = {}; for (const e of performance.getEntriesByType('paint')) p[e.name] = Math.round(e.startTime); const r = performance.getEntriesByType('resource'); return { navigation: n ? { type: n.type, ttfb: Math.round(n.responseStart), domContentLoaded: Math.round(n.domContentLoadedEventEnd), load: Math.round(n.loadEventEnd), transferSize: n.transferSize, decodedBodySize: n.decodedBodySize } : null, paint: p, resources: r.length, resourceBytes: r.reduce((a, e) => a + (e.transferSize || 0), 0) }; })()")
            .await
            .unwrap_or(Value::Null);
        Ok(json!({"metrics": metrics, "timing": timing}))
    }

    // ── screenshots ────────────────────────────────────────────────────

    async fn capture(&self, params: Value) -> Result<Vec<u8>, String> {
        let shot = self.raw("Page.captureScreenshot", params).await?;
        let data = shot["data"]
            .as_str()
            .ok_or("browser: the screenshot came back empty")?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|e| format!("browser: bad screenshot data: {e}"))
    }

    /// A PNG of the page. `full` captures the whole scroll height.
    pub async fn screenshot(&self, full: bool) -> Result<Vec<u8>, String> {
        let png = if full {
            // captureBeyondViewport + a clip of the content size, rather than
            // resizing the viewport: an emulated device must stay emulated.
            let m = self.raw("Page.getLayoutMetrics", json!({})).await?;
            let h = m["cssContentSize"]["height"].as_f64().unwrap_or(0.0);
            let w = m["cssContentSize"]["width"].as_f64().unwrap_or(0.0);
            if h > 0.0 && w > 0.0 {
                self.capture(json!({"format": "png", "captureBeyondViewport": true, "clip": {"x": 0, "y": 0, "width": w.ceil(), "height": h.min(16_000.0).ceil(), "scale": 1}}))
                    .await?
            } else {
                self.capture(json!({"format": "png"})).await?
            }
        } else {
            self.capture(json!({"format": "png"})).await?
        };
        if !full {
            lock(&self.st).last_shot = Some(png.clone());
        }
        Ok(png)
    }

    /// PNG of a viewport rectangle (CSS pixels).
    pub async fn screenshot_clip(&self, rect: Rect) -> Result<Vec<u8>, String> {
        if !(rect.width > 0.0 && rect.height > 0.0) {
            return Err("browser: that rectangle is empty".into());
        }
        // Clip coordinates are in document space: add the scroll offset.
        let m = self.raw("Page.getLayoutMetrics", json!({})).await?;
        let px = m["cssLayoutViewport"]["pageX"].as_f64().unwrap_or(0.0);
        let py = m["cssLayoutViewport"]["pageY"].as_f64().unwrap_or(0.0);
        self.capture(json!({"format": "png", "captureBeyondViewport": true, "clip": {"x": rect.x + px, "y": rect.y + py, "width": rect.width, "height": rect.height, "scale": 1}}))
            .await
    }

    /// A PNG of one element, scrolled into view first.
    pub async fn screenshot_element(&self, r: &Resolved) -> Result<Vec<u8>, String> {
        let _ = self.point_for(r, true, Duration::from_secs(3)).await?;
        let v = self
            .ol(
                &r.sid,
                &r.frame,
                &r.prefix,
                &format!("__ol.rectOf({})", json_str(&r.ref_)),
            )
            .await?;
        let (ox, oy) = self.frame_offset(&r.sid, &r.frame).await?;
        let rect = Rect {
            x: v["x"].as_f64().unwrap_or(0.0) + ox,
            y: v["y"].as_f64().unwrap_or(0.0) + oy,
            width: v["width"].as_f64().unwrap_or(0.0),
            height: v["height"].as_f64().unwrap_or(0.0),
        };
        self.screenshot_clip(rect).await
    }

    /// Set-of-marks: a viewport screenshot with every interactive element
    /// outlined and labelled with its ref. The overlay is gone afterwards.
    pub async fn screenshot_marks(&self) -> Result<(Vec<u8>, u64), String> {
        let n = self
            .ol_main("__ol.marks(true)")
            .await?
            .as_u64()
            .unwrap_or(0);
        let shot = self.capture(json!({"format": "png"})).await;
        let _ = self.ol_main("__ol.marks(false)").await;
        Ok((shot?, n))
    }

    /// Compare a fresh viewport screenshot with the previous one.
    pub async fn diff(&self) -> Result<(Option<snap::ImageDiff>, Vec<u8>), String> {
        let prev = lock(&self.st).last_shot.clone();
        let cur = self.screenshot(false).await?;
        match prev {
            Some(p) => Ok((Some(snap::image_diff(&p, &cur)?), cur)),
            None => Ok((None, cur)),
        }
    }

    pub async fn pdf(&self) -> Result<Vec<u8>, String> {
        let r = self
            .raw("Page.printToPDF", json!({"printBackground": true, "preferCSSPageSize": true}))
            .await
            .map_err(|e| {
                if self.mode.headed {
                    format!("{e} (PDF export needs the headless browser: turn off \"Show the browser window\")")
                } else {
                    e
                }
            })?;
        let data = r["data"]
            .as_str()
            .ok_or("browser: the PDF came back empty")?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|e| format!("browser: bad PDF data: {e}"))
    }

    // ── emulation ──────────────────────────────────────────────────────

    fn emulation_calls(&self) -> Vec<(String, Value)> {
        let e = lock(&self.st).emulation.clone();
        let Some(e) = e else {
            if self.mode.headed {
                // A visible window keeps the size the user gives it.
                return vec![];
            }
            return vec![(
                "Emulation.setDeviceMetricsOverride".into(),
                json!({"width": self.width, "height": self.height, "deviceScaleFactor": 1, "mobile": false}),
            )];
        };
        let mut v = vec![
            (
                "Emulation.setDeviceMetricsOverride".to_string(),
                json!({"width": e.width, "height": e.height, "deviceScaleFactor": e.dpr, "mobile": e.mobile}),
            ),
            (
                "Emulation.setTouchEmulationEnabled".into(),
                json!({"enabled": e.touch, "maxTouchPoints": if e.touch { 5 } else { 1 }}),
            ),
        ];
        if !e.user_agent.is_empty() {
            v.push((
                "Emulation.setUserAgentOverride".into(),
                json!({"userAgent": e.user_agent}),
            ));
        }
        v.push((
            "Emulation.setEmulatedMedia".into(),
            json!({"features": if e.color_scheme.is_empty() { json!([]) } else { json!([{"name": "prefers-color-scheme", "value": e.color_scheme}]) }}),
        ));
        v
    }

    /// Apply (or with `None`, reset) emulation on every open tab, and remember
    /// it for tabs opened later.
    #[allow(clippy::too_many_arguments)]
    pub async fn emulate(
        &self,
        device: Option<&snap::Device>,
        width: Option<u32>,
        height: Option<u32>,
        dpr: Option<f64>,
        mobile: Option<bool>,
        touch: Option<bool>,
        user_agent: Option<String>,
        color_scheme: Option<String>,
        reset: bool,
    ) -> Result<String, String> {
        let original_ua = if reset || device.is_some() || user_agent.is_some() {
            self.browser
                .conn
                .call(None, "Browser.getVersion", json!({}), CALL_TIMEOUT)
                .await
                .ok()
                .and_then(|v| v["userAgent"].as_str().map(str::to_string))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let summary = {
            let mut st = lock(&self.st);
            if reset {
                st.emulation = None;
            } else {
                let mut e = st.emulation.clone().unwrap_or(Emulation {
                    width: self.width,
                    height: self.height,
                    dpr: 1.0,
                    ..Default::default()
                });
                if let Some(d) = device {
                    e.width = d.width;
                    e.height = d.height;
                    e.dpr = d.dpr;
                    e.mobile = d.mobile;
                    e.touch = d.touch;
                    e.user_agent = if d.user_agent.is_empty() {
                        original_ua.clone()
                    } else {
                        d.user_agent.to_string()
                    };
                }
                if let Some(w) = width {
                    e.width = w.clamp(200, 3840);
                }
                if let Some(h) = height {
                    e.height = h.clamp(200, 4000);
                }
                if let Some(d) = dpr {
                    e.dpr = d.clamp(0.5, 4.0);
                }
                if let Some(m) = mobile {
                    e.mobile = m;
                }
                if let Some(t) = touch {
                    e.touch = t;
                }
                if let Some(u) = user_agent {
                    e.user_agent = u;
                }
                if let Some(c) = color_scheme {
                    e.color_scheme = c;
                }
                st.emulation = Some(e);
            }
            match &st.emulation {
                None => format!("emulation reset to {}x{}", self.width, self.height),
                Some(e) => format!(
                    "{}x{} @{}x{}{}{}",
                    e.width,
                    e.height,
                    e.dpr,
                    if e.mobile { ", mobile" } else { "" },
                    if e.touch { ", touch" } else { "" },
                    if e.color_scheme.is_empty() {
                        String::new()
                    } else {
                        format!(", {} color scheme", e.color_scheme)
                    }
                ),
            }
        };
        let mut calls = self.emulation_calls();
        if reset {
            calls.push(("Emulation.setEmulatedMedia".into(), json!({"features": []})));
            calls.push((
                "Emulation.setTouchEmulationEnabled".into(),
                json!({"enabled": false}),
            ));
            if !original_ua.is_empty() {
                calls.push((
                    "Emulation.setUserAgentOverride".into(),
                    json!({"userAgent": original_ua}),
                ));
            }
        }
        let sessions: Vec<String> = lock(&self.st)
            .tabs
            .iter()
            .map(|t| t.session.clone())
            .collect();
        for s in sessions {
            for (m, p) in &calls {
                self.call(&s, m, p.clone()).await?;
            }
        }
        Ok(summary)
    }

    // ── logs ───────────────────────────────────────────────────────────

    pub fn console(&self, level: Option<&str>, clear: bool, limit: usize) -> Vec<LogLine> {
        let mut st = lock(&self.st);
        let want = |l: &str| match level {
            None | Some("") | Some("all") => true,
            Some("error") => matches!(l, "error" | "exception" | "assert"),
            Some("warning") | Some("warn") => {
                matches!(l, "warning" | "warn" | "error" | "exception" | "assert")
            }
            Some(x) => l == x,
        };
        let out: Vec<LogLine> = st
            .console
            .iter()
            .filter(|l| want(&l.level))
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        st.console_errors_seen = st.console_errors;
        if clear {
            st.console.clear();
        }
        out
    }

    pub fn network(
        &self,
        filter: Option<&str>,
        failed_only: bool,
        clear: bool,
        limit: usize,
    ) -> Vec<NetEntry> {
        let mut st = lock(&self.st);
        let f = filter.unwrap_or("").to_ascii_lowercase();
        let out: Vec<NetEntry> = st
            .network
            .iter()
            .filter(|e| {
                f.is_empty()
                    || e.url.to_ascii_lowercase().contains(&f)
                    || e.kind.to_ascii_lowercase() == f
                    || e.method.eq_ignore_ascii_case(&f)
            })
            .filter(|e| {
                !failed_only
                    || e.failed.is_some()
                    || e.blocked
                    || e.status.is_some_and(|s| s >= 400)
            })
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if clear {
            st.network.clear();
        }
        out
    }

    /// The body of a logged response, capped.
    pub async fn network_body(&self, id: &str, max: usize) -> Result<(NetEntry, String), String> {
        let e = lock(&self.st)
            .network
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .ok_or_else(|| format!("browser: no request `{id}` in the network log (ids look like r12; see the `network` action)"))?;
        let r = self
            .call(
                &e.session,
                "Network.getResponseBody",
                json!({"requestId": e.request_id}),
            )
            .await
            .map_err(|err| {
                format!("{err} (bodies are only kept while the page that loaded them is open)")
            })?;
        let body = r["body"].as_str().unwrap_or("");
        let text = if r["base64Encoded"].as_bool().unwrap_or(false) {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body)
                .unwrap_or_default();
            match String::from_utf8(bytes.clone()) {
                Ok(s) => s,
                Err(_) => format!("[binary body, {} bytes]", bytes.len()),
            }
        } else {
            body.to_string()
        };
        Ok((e, snap::clip_chars(&text, max)))
    }

    pub fn downloads(&self) -> Vec<Download> {
        lock(&self.st).downloads.clone()
    }

    /// What happened since the last report (and mark it reported).
    pub fn take_report(&self) -> Report {
        let mut st = lock(&self.st);
        let r = Report {
            new_tabs: std::mem::take(&mut st.new_tabs)
                .into_iter()
                .filter_map(|id| {
                    st.tabs.iter().find(|t| t.id == id).map(|t| {
                        format!(
                            "{} {}",
                            t.id,
                            if t.title.is_empty() {
                                t.url.clone()
                            } else {
                                format!("\"{}\" {}", t.title, t.url)
                            }
                        )
                    })
                })
                .collect(),
            blocked: st.blocked[st.blocked_seen.min(st.blocked.len())..].to_vec(),
            lan_hosts: st.lan_blocked.iter().cloned().collect(),
            downloads: st.downloads[st.downloads_seen.min(st.downloads.len())..]
                .iter()
                .filter(|d| d.state != "inProgress")
                .cloned()
                .collect(),
            console_errors: st.console_errors.saturating_sub(st.console_errors_seen),
            dialog: st
                .active
                .as_ref()
                .and_then(|a| st.dialogs.get(a))
                .map(|(_, d)| d.clone()),
            tabs: st.tabs.len(),
            notes: st.notes[st.notes_seen.min(st.notes.len())..].to_vec(),
        };
        // Layer 2's refusals (a connection the proxy wouldn't make shows up in
        // the page only as ERR_TUNNEL_CONNECTION_FAILED): say which and why.
        let proxied = match &self._proxy {
            Some(p) => p.recent_blocks(),
            // The persistent profile's proxy is shared by every chat: its
            // log would tell this agent where other chats' pages went.
            None => Vec::new(),
        };
        let mut r = r;
        for b in &proxied {
            if !st.proxy_seen.contains(b) {
                r.blocked.push(format!("{b} (proxy)"));
            }
        }
        st.proxy_seen = proxied;
        st.blocked_seen = st.blocked.len();
        st.notes_seen = st.notes.len();
        st.console_errors_seen = st.console_errors;
        // Only finished downloads are reported (and so marked seen).
        let done = st
            .downloads
            .iter()
            .rposition(|d| d.state != "inProgress")
            .map(|i| i + 1)
            .unwrap_or(st.downloads_seen);
        st.downloads_seen = st.downloads_seen.max(done);
        r
    }

    fn note(&self, n: String) {
        let mut st = lock(&self.st);
        if !st.notes.contains(&n) {
            st.notes.push(n);
        }
    }

    fn record_block(&self, url: &str, v: &NetVerdict) {
        let short: String = url.chars().take(160).collect();
        let mut st = lock(&self.st);
        match v {
            NetVerdict::NeedsLan(h) => {
                st.lan_blocked.insert(h.clone());
                st.blocked
                    .push(format!("{short} (LAN host {h} not approved)"));
            }
            NetVerdict::Block(why) => st.blocked.push(format!("{short} ({why})")),
            NetVerdict::Allow => {}
        }
        if st.blocked.len() > 200 {
            let n = st.blocked.len() - 200;
            st.blocked.drain(..n);
            st.blocked_seen = st.blocked_seen.saturating_sub(n);
        }
    }

    // ── events ─────────────────────────────────────────────────────────

    fn tab_short(st: &State, tab: &Option<String>) -> String {
        tab.as_ref()
            .and_then(|t| st.tabs.iter().find(|x| &x.target == t))
            .map(|t| t.id.clone())
            .unwrap_or_default()
    }

    fn on_event(&self, sid: &str, route: &Route, method: &str, p: &Value) {
        match method {
            "Page.javascriptDialogOpening" => {
                let tab = route.tab.clone().unwrap_or_else(|| route.target.clone());
                lock(&self.st).dialogs.insert(
                    tab,
                    (
                        sid.to_string(),
                        DialogInfo {
                            kind: p["type"].as_str().unwrap_or("alert").to_string(),
                            message: p["message"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(2000)
                                .collect(),
                            default_prompt: p["defaultPrompt"].as_str().unwrap_or("").to_string(),
                        },
                    ),
                );
                self.dialog_tx.send_modify(|g| *g += 1);
            }
            "Page.javascriptDialogClosed" => {
                let tab = route.tab.clone().unwrap_or_else(|| route.target.clone());
                lock(&self.st).dialogs.remove(&tab);
                self.dialog_tx.send_modify(|g| *g += 1);
            }
            "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                let f = p["frameId"].as_str().unwrap_or("");
                if route.kind == "page" && f == route.target {
                    lock(&self.st)
                        .loading
                        .insert(route.target.clone(), method == "Page.frameStartedLoading");
                }
            }
            "Page.frameNavigated" => {
                let frame = &p["frame"];
                let url = frame["url"].as_str().unwrap_or("").to_string();
                let top = route.kind == "page" && frame["parentId"].is_null();
                if top {
                    let mut st = lock(&self.st);
                    if let Some(t) = st.tabs.iter_mut().find(|t| t.target == route.target) {
                        t.url = url.clone();
                    }
                    if url.starts_with("http") || url.starts_with("file:") {
                        if st.history.front() != Some(&url) {
                            st.history.push_front(url.clone());
                        }
                        st.history.truncate(50);
                    }
                }
                // Fetch doesn't see every scheme (file: among them in some
                // builds); a frame that lands somewhere the policy refuses is
                // sent away again.
                if let NetVerdict::Block(why) = self.policy_check(&url) {
                    if !url.is_empty() {
                        self.record_block(&url, &NetVerdict::Block(why));
                        let fid = frame["id"].clone();
                        self.browser.conn.fire(
                            Some(sid),
                            "Page.navigate",
                            json!({"url": "about:blank", "frameId": fid}),
                        );
                    }
                }
            }
            "Runtime.executionContextDestroyed" => {
                let id = p["executionContextId"].as_i64();
                lock(&self.st)
                    .worlds
                    .retain(|(s, _), c| !(s == sid && Some(*c) == id));
            }
            "Runtime.executionContextsCleared" => {
                lock(&self.st).worlds.retain(|(s, _), _| s != sid);
            }
            "Runtime.consoleAPICalled" => {
                let level = match p["type"].as_str().unwrap_or("log") {
                    "warning" => "warning",
                    "error" => "error",
                    "assert" => "assert",
                    "info" => "info",
                    "debug" => "debug",
                    _ => "log",
                };
                let text: Vec<String> = p["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| match &a["value"] {
                        Value::String(s) => s.clone(),
                        Value::Null => a["description"]
                            .as_str()
                            .or(a["unserializableValue"].as_str())
                            .or(a["type"].as_str())
                            .unwrap_or("")
                            .to_string(),
                        v => v.to_string(),
                    })
                    .collect();
                let frame0 = &p["stackTrace"]["callFrames"][0];
                let source = match frame0["url"].as_str() {
                    Some(u) if !u.is_empty() => {
                        format!("{u}:{}", frame0["lineNumber"].as_i64().unwrap_or(0) + 1)
                    }
                    _ => String::new(),
                };
                self.push_log(level, text.join(" "), source, route);
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let text = d["exception"]["description"]
                    .as_str()
                    .or(d["text"].as_str())
                    .unwrap_or("exception")
                    .to_string();
                let source = d["url"]
                    .as_str()
                    .map(|u| format!("{u}:{}", d["lineNumber"].as_i64().unwrap_or(0) + 1))
                    .unwrap_or_default();
                self.push_log("exception", text, source, route);
            }
            "Log.entryAdded" => {
                let e = &p["entry"];
                let level = match e["level"].as_str().unwrap_or("info") {
                    "error" => "error",
                    "warning" => "warning",
                    "verbose" => "debug",
                    _ => "info",
                };
                self.push_log(
                    level,
                    e["text"].as_str().unwrap_or("").to_string(),
                    e["url"].as_str().unwrap_or("").to_string(),
                    route,
                );
            }
            "Network.requestWillBeSent" => self.net_request(sid, route, p),
            "Network.responseReceived" => {
                let resp = &p["response"];
                let key = (
                    sid.to_string(),
                    p["requestId"].as_str().unwrap_or("").to_string(),
                );
                let mut st = lock(&self.st);
                if let Some(e) = st
                    .network
                    .iter_mut()
                    .rev()
                    .find(|e| e.session == key.0 && e.request_id == key.1)
                {
                    e.status = resp["status"].as_i64();
                    e.mime = resp["mimeType"].as_str().unwrap_or("").to_string();
                    let mut h = resp["headers"].clone();
                    policy::redact_headers(&mut h);
                    e.response_headers = h;
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                let key = (
                    sid.to_string(),
                    p["requestId"].as_str().unwrap_or("").to_string(),
                );
                let mut st = lock(&self.st);
                st.inflight.remove(&key);
                st.last_net = Some(Instant::now());
                if let Some(e) = st
                    .network
                    .iter_mut()
                    .rev()
                    .find(|e| e.session == key.0 && e.request_id == key.1)
                {
                    let t = p["timestamp"].as_f64().unwrap_or(e.start);
                    e.ms = Some(((t - e.start).max(0.0) * 1000.0) as u64);
                    if method == "Network.loadingFinished" {
                        e.size = p["encodedDataLength"].as_f64().unwrap_or(0.0) as u64;
                    } else {
                        let why = p["blockedReason"]
                            .as_str()
                            .map(|b| format!("blocked: {b}"))
                            .unwrap_or_else(|| {
                                p["errorText"].as_str().unwrap_or("failed").to_string()
                            });
                        e.blocked = why.contains("BLOCKED_BY_CLIENT") || why.contains("blocked");
                        e.failed = Some(why);
                    }
                }
            }
            "Input.dragIntercepted" => {
                lock(&self.st).drag = Some(p["data"].clone());
            }
            "Inspector.targetCrashed" => {
                self.note("the page crashed — reload it or navigate again".into());
            }
            _ => {}
        }
    }

    fn push_log(&self, level: &str, text: String, source: String, route: &Route) {
        let mut st = lock(&self.st);
        let tab = Self::tab_short(&st, &route.tab);
        if matches!(level, "error" | "exception" | "assert") {
            st.console_errors += 1;
        }
        st.console.push_back(LogLine {
            level: level.to_string(),
            text: snap::clip_chars(&text, 2000),
            source: source.chars().take(200).collect(),
            tab,
        });
        while st.console.len() > CONSOLE_CAP {
            st.console.pop_front();
        }
    }

    fn net_request(&self, sid: &str, route: &Route, p: &Value) {
        let rid = p["requestId"].as_str().unwrap_or("").to_string();
        let req = &p["request"];
        let url = req["url"].as_str().unwrap_or("");
        if url.starts_with("data:") {
            return;
        }
        let ts = p["timestamp"].as_f64().unwrap_or(0.0);
        let mut st = lock(&self.st);
        // A redirect reuses the request id: close the hop that redirected.
        if let Some(rr) = p.get("redirectResponse").filter(|r| r.is_object()) {
            if let Some(e) = st
                .network
                .iter_mut()
                .rev()
                .find(|e| e.session == sid && e.request_id == rid && e.status.is_none())
            {
                e.status = rr["status"].as_i64();
                e.ms = Some(((ts - e.start).max(0.0) * 1000.0) as u64);
            }
        }
        st.net_next += 1;
        let id = format!("r{}", st.net_next);
        let tab = Self::tab_short(&st, &route.tab);
        let mut h = req["headers"].clone();
        policy::redact_headers(&mut h);
        st.network.push_back(NetEntry {
            id,
            session: sid.to_string(),
            request_id: rid.clone(),
            method: req["method"].as_str().unwrap_or("GET").to_string(),
            url: url.chars().take(500).collect(),
            kind: p["type"].as_str().unwrap_or("").to_string(),
            start: ts,
            request_headers: h,
            response_headers: Value::Null,
            tab,
            ..Default::default()
        });
        while st.network.len() > NETWORK_CAP {
            st.network.pop_front();
        }
        let tab_target = route.tab.clone().unwrap_or_default();
        st.inflight
            .insert((sid.to_string(), rid), (tab_target, Instant::now()));
        st.last_net = Some(Instant::now());
    }

    fn download_began(&self, guid: &str, p: &Value) {
        lock(&self.st).downloads.push(Download {
            guid: guid.to_string(),
            url: p["url"].as_str().unwrap_or("").chars().take(300).collect(),
            suggested: p["suggestedFilename"]
                .as_str()
                .unwrap_or("download")
                .to_string(),
            path: None,
            state: "inProgress".into(),
            bytes: 0,
        });
    }

    fn download_progress(&self, staging: &Path, guid: &str, p: &Value) {
        let state = p["state"].as_str().unwrap_or("inProgress").to_string();
        let bytes = p["receivedBytes"].as_f64().unwrap_or(0.0) as u64;
        let suggested = {
            let mut st = lock(&self.st);
            let Some(d) = st.downloads.iter_mut().find(|d| d.guid == guid) else {
                return;
            };
            d.bytes = bytes;
            if state != "completed" {
                d.state = state.clone();
                return;
            }
            d.suggested.clone()
        };
        let src = staging.join(guid);
        let dir = lock(&self.downloads_dir).clone();
        let placed = match dir {
            Some(dir) => {
                let _ = std::fs::create_dir_all(&dir);
                let dst = policy::download_target(&dir, &suggested);
                let moved = std::fs::rename(&src, &dst).is_ok()
                    || (std::fs::copy(&src, &dst).is_ok() && std::fs::remove_file(&src).is_ok());
                if moved {
                    Some(dst)
                } else {
                    Some(src)
                }
            }
            None => Some(src),
        };
        let mut st = lock(&self.st);
        if let Some(d) = st.downloads.iter_mut().find(|d| d.guid == guid) {
            d.state = "completed".into();
            d.path = placed.map(|p| p.to_string_lossy().to_string());
        }
    }

    // ── Panel-facing API ─────────────────────────────────────────────────
    // CONTRACT shared between the CDP core and the Browser panel.

    /// Every tab (page target) this session owns, in strip order.
    pub async fn tabs(&self) -> Result<Vec<TabInfo>, String> {
        let st = lock(&self.st);
        let active = st
            .active
            .clone()
            .or_else(|| st.tabs.last().map(|t| t.target.clone()));
        Ok(st
            .tabs
            .iter()
            .map(|t| TabInfo {
                id: t.id.clone(),
                url: t.url.clone(),
                title: t.title.clone(),
                active: Some(&t.target) == active.as_ref(),
            })
            .collect())
    }

    /// Open a new tab (optionally at `url`) and make it the active one.
    pub async fn new_tab(&self, url: Option<&str>) -> Result<TabInfo, String> {
        if lock(&self.st).tabs.len() >= 20 {
            return Err("browser: 20 tabs are open — close some first".into());
        }
        self.open_tab(url).await
    }

    fn tab_by_id(&self, id: &str) -> Result<(String, String), String> {
        let st = lock(&self.st);
        st.tabs
            .iter()
            .find(|t| t.id == id || t.target == id)
            .map(|t| (t.target.clone(), t.session.clone()))
            .ok_or_else(|| {
                format!(
                    "browser: no tab `{id}`; open tabs: {}",
                    st.tabs
                        .iter()
                        .map(|t| t.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Make the tab `id` the one every later call acts on.
    pub async fn switch_tab(&self, id: &str) -> Result<(), String> {
        let (tid, sid) = self.tab_by_id(id)?;
        lock(&self.st).active = Some(tid.clone());
        let _ = self.call(&sid, "Page.bringToFront", json!({})).await;
        Ok(())
    }

    /// Close the tab `id`. Closing the last tab leaves a blank one.
    pub async fn close_tab(&self, id: &str) -> Result<(), String> {
        let (tid, _) = self.tab_by_id(id)?;
        let last = lock(&self.st).tabs.len() <= 1;
        if last {
            self.open_tab(None).await?;
        }
        self.browser
            .conn
            .call(
                None,
                "Target.closeTarget",
                json!({"targetId": tid}),
                CALL_TIMEOUT,
            )
            .await?;
        self.remove_tab(&tid);
        Ok(())
    }

    pub async fn go_back(&self) -> Result<(), String> {
        self.history_step(-1).await.map(|_| ())
    }

    pub async fn go_forward(&self) -> Result<(), String> {
        self.history_step(1).await.map(|_| ())
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.raw("Page.reload", json!({})).await?;
        self.settle().await
    }

    /// Whether the active tab has history behind / ahead of it.
    pub async fn can_go(&self) -> (bool, bool) {
        match self.raw("Page.getNavigationHistory", json!({})).await {
            Ok(h) => {
                let i = h["currentIndex"].as_i64().unwrap_or(0);
                let n = h["entries"].as_array().map(|a| a.len()).unwrap_or(0) as i64;
                (i > 0, i + 1 < n)
            }
            Err(_) => (false, false),
        }
    }

    /// The element at viewport point (x, y), with a snapshot ref assigned so the
    /// agent can act on exactly what the user pointed at.
    pub async fn element_at(&self, x: f64, y: f64) -> Result<ElementInfo, String> {
        let v = self.ol_main(&format!("__ol.elementAt({x}, {y})")).await?;
        if v.is_null() {
            return Err(format!("browser: there is nothing at ({x}, {y})"));
        }
        serde_json::from_value(v).map_err(|e| format!("browser: unreadable element info: {e}"))
    }

    /// Recently visited URLs in this session, newest first, deduplicated.
    pub async fn history(&self) -> Vec<String> {
        let st = lock(&self.st);
        let mut seen = std::collections::HashSet::new();
        st.history
            .iter()
            .filter(|u| seen.insert(u.as_str().to_string()))
            .cloned()
            .collect()
    }

    /// The user (or an approval) allowed this LAN host for this session.
    pub fn allow_lan_host(&self, host: &str) {
        let h = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
        if h.is_empty() {
            return;
        }
        self.policy
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .lan_allowed
            .insert(h.clone());
        lock(&self.st).lan_blocked.remove(&h);
        self.sync_proc_lan();
    }

    /// An open alert/confirm/prompt on the active tab, if any.
    pub async fn pending_dialog(&self) -> Option<DialogInfo> {
        let (tid, _) = self.active().ok()?;
        lock(&self.st).dialogs.get(&tid).map(|(_, d)| d.clone())
    }

    /// Accept or dismiss the open dialog (`text` answers a prompt()).
    pub async fn answer_dialog(&self, accept: bool, text: Option<&str>) -> Result<(), String> {
        let (tid, _) = self.active()?;
        let sid = lock(&self.st)
            .dialogs
            .get(&tid)
            .map(|(s, _)| s.clone())
            .ok_or("browser: no dialog is open")?;
        let mut p = json!({"accept": accept});
        if let Some(t) = text {
            p["promptText"] = json!(t);
        }
        self.call(&sid, "Page.handleJavaScriptDialog", p).await?;
        lock(&self.st).dialogs.remove(&tid);
        Ok(())
    }
}

/// One tab of a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// A rectangle in viewport CSS pixels.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What the user pointed at in the panel (Annotate / inspect).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElementInfo {
    /// Snapshot ref (`e12`) the agent's `ref` targeting accepts.
    #[serde(rename = "ref")]
    pub ref_: String,
    /// A CSS selector that matched exactly this element when it was taken.
    pub selector: String,
    pub tag: String,
    pub role: String,
    /// Accessible name / short visible text.
    pub name: String,
    pub bbox: Rect,
    /// Computed values of the style properties the panel can adjust.
    pub styles: std::collections::BTreeMap<String, String>,
}

/// An open JavaScript dialog.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DialogInfo {
    /// alert | confirm | prompt | beforeunload
    pub kind: String,
    pub message: String,
    pub default_prompt: String,
}

/// JSON-encode a string for embedding in a JS literal.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

// ───────────────────────────── registry ─────────────────────────────

/// Sessions by owner (`"{task}:{sub|main}"`), so a conversation keeps its page.
fn sessions() -> &'static StdMutex<HashMap<String, Arc<Session>>> {
    static S: OnceLock<StdMutex<HashMap<String, Arc<Session>>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// One launch per owner at a time.
fn owner_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The session for `owner` as it is, or a default (throwaway, headless) one.
/// The Browser panel uses this; the agent goes through [`session_with`].
pub async fn session_for(owner: &str, width: u32, height: u32) -> Result<Arc<Session>, String> {
    let found = lock(sessions()).get(owner).cloned();
    if let Some(s) = found {
        if s.alive() {
            s.ensure_tab().await?;
            return Ok(s);
        }
    }
    session_with(
        owner,
        SessionOpts {
            width,
            height,
            ..Default::default()
        },
    )
    .await
}

/// The session for `owner`, launching (or replacing a dead or wrong-mode one)
/// as needed, configured with `opts`.
pub async fn session_with(owner: &str, opts: SessionOpts) -> Result<Arc<Session>, String> {
    let _g = owner_lock().lock().await;
    let existing = lock(sessions()).get(owner).cloned();
    if let Some(s) = existing {
        if s.alive() && s.mode() == opts.mode() {
            s.configure(&opts);
            s.touch();
            s.ensure_tab().await?;
            return Ok(s);
        }
        let old = lock(sessions()).remove(owner);
        drop(old);
    }
    let fresh = Session::create(owner, &opts).await?;
    lock(sessions()).insert(owner.to_string(), fresh.clone());
    // Every new session is a moment to notice the ones nobody is using any
    // more. A finished subagent's page is never closed explicitly (only a
    // task's are, at the end of a run), so without this an ultrathread would
    // keep a context per worker alive for the whole app session.
    let stale: Vec<Arc<Session>> = {
        let mut m = lock(sessions());
        let keys: Vec<String> = m
            .iter()
            .filter(|(k, s)| k.as_str() != owner && s.is_idle())
            .map(|(k, _)| k.clone())
            .collect();
        keys.iter().filter_map(|k| m.remove(k)).collect()
    };
    drop(stale);
    Ok(fresh)
}

/// The live session for `owner`, if there is one (never launches).
pub fn existing(owner: &str) -> Option<Arc<Session>> {
    lock(sessions()).get(owner).cloned()
}

/// Close one owner's browser session, if it has one.
pub fn close(owner: &str) {
    // Taken out and dropped outside the lock: dropping tears the context down.
    let victim = lock(sessions()).remove(owner);
    drop(victim);
}

/// Close every browser session a task owns, main agent and subagents alike.
pub fn close_task(task_id: &str) {
    let victims: Vec<Arc<Session>> = {
        let mut m = lock(sessions());
        let keys: Vec<String> = m
            .keys()
            .filter(|k| k.as_str() == task_id || k.starts_with(&format!("{task_id}:")))
            .cloned()
            .collect();
        keys.iter().filter_map(|k| m.remove(k)).collect()
    };
    drop(victims);
}
