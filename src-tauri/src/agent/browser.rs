//! A real browser the agent drives, in a throwaway profile, over CDP.
//!
//! The headless half of the screen tools, and the only one there is now: the
//! one-shot `render` plugin (HTML or a URL to a PNG) was folded in here, since
//! `navigate` + `screenshot` already did its whole job and a second way to
//! screenshot a page was one way too many.
//!
//! Two properties are the whole point of this living next to `computer`:
//!
//! - **It never touches the user's desktop.** Headless, its own throwaway
//!   profile, `CREATE_NO_WINDOW`, nothing shown. A model that only needs to see
//!   a page should never be able to reach the user's mouse, keyboard or screen,
//!   and doesn't have to ask.
//! - **It is not a read.** A session that logs in, submits forms and holds
//!   cookies changes the world, so `click`/`type` are gated through the normal
//!   permission path rather than waved through the way `view_image` is. What
//!   makes that safe is the profile: it is a fresh directory per task, so there
//!   is no cookie jar, no saved password and no logged-in session of the user's
//!   to abuse or leak. The blast radius is the throwaway profile, plus
//!   whatever the agent types into it.
//!
//! CDP is spoken directly over a WebSocket rather than through a driver crate:
//! the surface used here is small (navigate, evaluate, click, type, screenshot)
//! and a dependency that can run arbitrary scripts in a browser is worth not
//! having.

use super::plugins::{browser_name, browsers_installed};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Async mutexes throughout: every lock here is held across an `await`, and a
/// `std::sync::MutexGuard` is not `Send`, so the agent's `Box::pin`ed call
/// futures would stop being `Send`. `parking_lot` would not help either — the
/// guard is the problem, not the blocking.
type Lock<T> = tokio::sync::Mutex<T>;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Longest we wait for the browser to hand back its debugging endpoint.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Default ceiling on a single CDP call. A page that never settles should fail
/// the call, not wedge the agent forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a session may sit unused before the browser is shut down.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Ceiling on the frame between polls while waiting for the debug port file.
const PORT_POLL: Duration = Duration::from_millis(100);

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Where the agent is, so a later call (and the UI) can say where.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageState {
    pub url: String,
    pub title: String,
}

/// One live browser plus its CDP connection.
///
/// The socket sits behind a mutex because a CDP session is inherently serial:
/// every command is a numbered request whose answer arrives interleaved with
/// events, so two in-flight commands would have to demultiplex each other's
/// replies. Holding the lock across one whole call-and-reply is both simpler
/// and closer to how a person drives a browser — one thing at a time, each seen
/// to finish.
pub struct Session {
    child: Lock<Option<std::process::Child>>,
    /// WebSocket endpoint of the page target. Taken once, at launch.
    endpoint: String,
    /// The live connection plus the page session id, or `None` when it needs
    /// re-opening. The id is there because the debugging socket Chrome hands us
    /// is the *browser* target, and `Page.*` only exists on a page.
    conn: Lock<Option<(Ws, String)>>,
    next_id: AtomicU64,
    /// The last known page, kept so a screenshot and a later call agree.
    page: Lock<PageState>,
    /// Temp profile dir, removed when the session drops.
    dir: PathBuf,
    /// When this session was last used, for the idle sweep.
    touched: Lock<Instant>,
    browser: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Socket first, then kill, then delete the profile: a browser still
        // running holds files in that dir, and on Windows the remove fails.
        self.conn.get_mut().take();
        if let Some(ch) = self.child.get_mut().as_mut() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Session {
    /// Launch a headless browser and attach to it. The caller owns the key this
    /// is filed under (the task, plus its sub-agent), which is what keeps one
    /// conversation's page out of another's reach.
    pub async fn launch(width: u32, height: u32) -> Result<Arc<Self>, String> {
        let browsers = browsers_installed();
        let Some(browser) = browsers.first() else {
            return Err(
                "No Chromium browser found. Install Chrome or Edge (the browser plugin needs one)."
                    .into(),
            );
        };
        let who = browser_name(browser);
        let dir = std::env::temp_dir().join(format!(
            "openleash-browser-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let profile = dir.join("profile");
        std::fs::create_dir_all(&profile)
            .map_err(|e| format!("browser: can't create a profile dir: {e}"))?;

        // Port 0 = let the OS pick a free one, and we read back what it chose
        // from DevToolsActivePort rather than racing another browser for a
        // fixed port.
        let mut cmd = super::plugins::hidden_cmd(browser.to_str().unwrap_or_default());
        let child = cmd
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg("--disable-background-networking")
            .arg("--disable-sync")
            .arg("--disable-features=Translate,MediaRouter")
            .arg("--hide-scrollbars")
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=*")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!(
                "--window-size={},{}",
                width.clamp(320, 3840),
                height.clamp(240, 2160)
            ))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("browser: couldn't start {who}: {e}"))?;

        let mut child = Some(child);
        let endpoint = tokio::time::timeout(LAUNCH_TIMEOUT, async {
            loop {
                if let Ok(txt) = std::fs::read_to_string(profile.join("DevToolsActivePort")) {
                    let mut it = txt.lines();
                    if let (Some(port), Some(path)) = (it.next(), it.next()) {
                        if let Ok(port) = port.parse::<u16>() {
                            break Ok(format!("ws://127.0.0.1:{port}{path}"));
                        }
                    }
                }
                // A browser that died should say so, not spin to the timeout.
                if let Some(ch) = child.as_mut() {
                    if let Ok(Some(_)) = ch.try_wait() {
                        break Err(format!("browser: {who} exited before it was ready"));
                    }
                }
                tokio::time::sleep(PORT_POLL).await;
            }
        })
        .await
        .map_err(|_| {
            format!(
                "browser: {who} never reported a debugging port within {}s",
                LAUNCH_TIMEOUT.as_secs()
            )
        })??;

        let s = Arc::new(Session {
            child: Lock::new(child.take()),
            endpoint,
            conn: Lock::new(None),
            next_id: AtomicU64::new(1),
            page: Lock::new(PageState::default()),
            dir,
            touched: Lock::new(Instant::now()),
            browser: who,
        });
        // Attach eagerly, so the first real call isn't the one that finds out.
        s.open().await?;
        // Only the domains `goto`/`click`/`screenshot` need. No Network
        // interception, no cookie reading, no storage.
        let _ = s.raw("Page.enable", json!({})).await;
        let _ = s.raw("Runtime.enable", json!({})).await;
        Ok(s)
    }

    /// The live socket plus a page session, connecting if we don't have one.
    async fn open(&self) -> Result<(), String> {
        let mut guard = self.conn.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(&self.endpoint)
            .await
            .map_err(|e| format!("browser: can't reach the debugging socket: {e}"))?;
        // `DevToolsActivePort` points at the browser target, where `Page.*` does
        // not exist. Attach to an actual page, flattened, so every later
        // command carries the session id and needs no envelope of its own.
        let target = self.first_command(&mut ws).await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        ws.send(Message::Text(json!({ "id": id, "method": "Target.attachToTarget", "params": { "targetId": target, "flatten": true } }).to_string().into()))
            .await
            .map_err(|e| format!("browser: can't attach to the page: {e}"))?;
        let session;
        loop {
            let Some(Ok(m)) = ws.next().await else {
                return Err("browser: the browser closed the connection while attaching".into());
            };
            let Message::Text(t) = m else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&t) else {
                continue;
            };
            if v.get("method").is_some() {
                continue;
            }
            if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                return Err(format!(
                    "browser: can't attach to the page: {}",
                    e["message"].as_str().unwrap_or("unknown error")
                ));
            }
            session = v["result"]["sessionId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            break;
        }
        if session.is_empty() {
            return Err("browser: the page attach gave no session".into());
        }
        *guard = Some((ws, session));
        Ok(())
    }
    /// One command, used only by `open` before a page session exists.
    async fn first_command(&self, ws: &mut Ws) -> Result<String, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        ws.send(Message::Text(
            json!({ "id": id, "method": "Target.getTargets" })
                .to_string()
                .into(),
        ))
        .await
        .map_err(|e| format!("browser: can't list targets: {e}"))?;
        loop {
            let Some(Ok(m)) = ws.next().await else {
                return Err("browser: the browser closed the connection".into());
            };
            let Message::Text(t) = m else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&t) else {
                continue;
            };
            if v.get("method").is_some() || v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                return Err(format!(
                    "browser: can't list targets: {}",
                    e["message"].as_str().unwrap_or("unknown error")
                ));
            }
            // An existing page is preferred: one is already open and warm. Only
            // make a new one if the browser came up with none.
            let infos = v["result"]["targetInfos"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if let Some(t) = infos
                .iter()
                .find(|t| t["type"] == "page" && t["targetId"].is_string())
            {
                return Ok(t["targetId"].as_str().unwrap_or_default().to_string());
            }
            let id2 = self.next_id.fetch_add(1, Ordering::Relaxed);
            ws.send(Message::Text(json!({ "id": id2, "method": "Target.createTarget", "params": { "url": "about:blank" } }).to_string().into()))
                .await
                .map_err(|e| format!("browser: can't open a page: {e}"))?;
            loop {
                let Some(Ok(m)) = ws.next().await else {
                    return Err("browser: the browser closed the connection".into());
                };
                let Message::Text(t) = m else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                if v.get("method").is_some() || v.get("id").and_then(|x| x.as_u64()) != Some(id2) {
                    continue;
                }
                return v["result"]["targetId"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "browser: the new page came back with no id".to_string());
            }
        }
    }

    /// Issue one CDP command and return its `result`, discarding the event
    /// traffic that arrives while we wait for the answer.
    pub async fn raw(&self, method: &str, params: Value) -> Result<Value, String> {
        self.open().await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + CALL_TIMEOUT;
        let mut guard = self.conn.lock().await;
        let (ws, session) = guard.as_mut().ok_or("browser: this session is closed")?;
        ws.send(Message::Text(
            json!({ "id": id, "sessionId": session, "method": method, "params": params })
                .to_string()
                .into(),
        ))
        .await
        .map_err(|e| format!("browser: can't send {method}: {e}"))?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "browser: {method} didn't answer within {}s",
                    CALL_TIMEOUT.as_secs()
                ));
            }
            let msg = match tokio::time::timeout(left, ws.next()).await {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => {
                    return Err(format!("browser: the debugging socket failed: {e}"))
                }
                Ok(None) => return Err("browser: the browser closed the connection".into()),
                Err(_) => {
                    return Err(format!(
                        "browser: {method} timed out after {}s",
                        CALL_TIMEOUT.as_secs()
                    ))
                }
            };
            let text = match msg {
                Message::Text(t) => t,
                // tungstenite answers pings itself; nothing here needs a pong.
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => continue,
                Message::Close(_) => {
                    return Err("browser: the browser closed the connection".into())
                }
                Message::Frame(_) => continue,
            };
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| format!("browser: unreadable message: {e}"))?;
            if v.get("method").is_some() {
                continue; // an event, not our answer
            }
            if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(format!(
                    "browser: {method} failed: {}",
                    err["message"].as_str().unwrap_or("unknown error")
                ));
            }
            let mut t = self.touched.lock().await;
            *t = Instant::now();
            return Ok(v["result"].clone());
        }
    }

    /// Evaluate `expr` in the page and return its value.
    ///
    /// `awaitPromise` is on: the common case is an expression ending in a fetch
    /// or a click that triggers navigation, and without it the agent gets a
    /// pending Promise it cannot use.
    pub async fn eval(&self, expr: &str) -> Result<Value, String> {
        let r = self
            .raw("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true, "awaitPromise": true, "userGesture": true }))
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

    /// Navigate, then wait for the page to settle, so a screenshot afterwards
    /// shows a page that has painted rather than a spinner.
    pub async fn goto(&self, url: &str) -> Result<PageState, String> {
        self.raw("Page.navigate", json!({ "url": url })).await?;
        self.settle().await?;
        self.state().await
    }

    /// Wait until the page stops changing: poll the body text until it has been
    /// stable for a few ticks, or give up. Cheap, and it covers the common
    /// "spinner, then content" case without a lifecycle-event dependency.
    pub async fn settle(&self) -> Result<(), String> {
        let _ = self
            .eval(
                r#"new Promise(r => {
                  const t0 = Date.now(); let last = -1, stable = 0;
                  const tick = () => {
                    const n = document.body ? document.body.innerText.length : 0;
                    if (n === last) { if (++stable >= 3) return r(1); } else { stable = 0; last = n; }
                    if (Date.now() - t0 > 8000) return r(0);
                    setTimeout(tick, 120);
                  };
                  tick();
                })"#,
            )
            .await;
        Ok(())
    }

    /// Where we are, read from the live document rather than remembered.
    pub async fn state(&self) -> Result<PageState, String> {
        let v = self
            .eval("JSON.stringify({url: location.href, title: document.title})")
            .await
            .unwrap_or(Value::Null);
        let st: PageState = v
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if !st.url.is_empty() {
            let mut p = self.page.lock().await;
            *p = st.clone();
        }
        Ok(st)
    }

    /// The last known page, for a label when we don't want to round-trip.
    pub fn remembered(&self) -> PageState {
        // A read of a value only ever overwritten, so the blocking lock is safe
        // here: this is never called from inside a runtime worker.
        self.page.blocking_lock().clone()
    }

    /// A PNG of the page. `full` captures the whole scroll height.
    pub async fn screenshot(&self, full: bool) -> Result<Vec<u8>, String> {
        if full {
            // captureBeyondViewport is what makes a tall page come out whole.
            let m = self.raw("Page.getLayoutMetrics", json!({})).await?;
            let h = m["cssContentSize"]["height"].as_f64().unwrap_or(0.0);
            let w = m["cssContentSize"]["width"].as_f64().unwrap_or(0.0);
            if h > 0.0 && h < 30_000.0 {
                self.raw("Emulation.setDeviceMetricsOverride", json!({ "width": w.ceil().max(320.0), "height": h.ceil(), "deviceScaleFactor": 1, "mobile": false })).await?;
            }
        }
        let shot = self
            .raw(
                "Page.captureScreenshot",
                json!({ "format": "png", "captureBeyondViewport": full }),
            )
            .await?;
        if full {
            let _ = self
                .raw("Emulation.clearDeviceMetricsOverride", json!({}))
                .await;
        }
        let data = shot["data"]
            .as_str()
            .ok_or("browser: the screenshot came back empty")?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|e| format!("browser: bad screenshot data: {e}"))
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

    /// Type into whatever has focus. `insertText` fires the input events a real
    /// keystroke does, without needing per-character key events (and so is not
    /// at the mercy of the OS keymap).
    pub async fn type_text(&self, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Err("browser: `text` is empty".into());
        }
        self.raw("Input.insertText", json!({ "text": text }))
            .await?;
        Ok(())
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

    /// Which browser binary this is ("Chrome"/"Edge"), so a result can say so.
    pub fn browser(&self) -> &str {
        &self.browser
    }

    pub fn is_idle(&self) -> bool {
        self.touched
            .try_lock()
            .is_ok_and(|t| t.elapsed() > IDLE_TIMEOUT)
    }

    /// Narrow, explicit user controls. No caller-supplied JavaScript or CDP method.
    pub async fn panel(&self, action: PanelAction) -> Result<PanelFrame, String> {
        action.validate()?;
        match action {
            PanelAction::Snapshot {} => {}
            PanelAction::Navigate { url } => {
                self.goto(&url).await?;
            }
            PanelAction::Click { x, y } => {
                let metrics = self.raw("Page.getLayoutMetrics", json!({})).await?;
                let viewport = &metrics["cssVisualViewport"];
                if x >= viewport["clientWidth"].as_f64().unwrap_or(0.0)
                    || y >= viewport["clientHeight"].as_f64().unwrap_or(0.0)
                {
                    return Err("browser: click is outside the viewport".into());
                }
                for event in ["mousePressed", "mouseReleased"] {
                    self.raw(
                        "Input.dispatchMouseEvent",
                        json!({"type": event, "x": x, "y": y, "button": "left", "clickCount": 1}),
                    )
                    .await?;
                }
                self.settle().await?;
            }
            PanelAction::Type { text } => {
                self.type_text(&text).await?;
            }
            PanelAction::Key { key } => {
                let key = match key {
                    PanelKey::Enter => "Enter",
                    PanelKey::Tab => "Tab",
                };
                for event in ["keyDown", "keyUp"] {
                    self.raw("Input.dispatchKeyEvent", json!({"type":event,"key":key,"code":key,"windowsVirtualKeyCode":if key == "Enter" {13} else {9}})).await?;
                }
                self.settle().await?;
            }
            PanelAction::Scroll { delta_y } => {
                self.raw(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseWheel","x":1,"y":1,"deltaX":0,"deltaY":delta_y}),
                )
                .await?;
            }
            PanelAction::Back {} => {
                let history = self.raw("Page.getNavigationHistory", json!({})).await?;
                let index = history["currentIndex"].as_u64().unwrap_or(0);
                if index > 0 {
                    let entry = &history["entries"][index as usize - 1];
                    validate_panel_url(entry["url"].as_str().unwrap_or(""))?;
                    self.raw(
                        "Page.navigateToHistoryEntry",
                        json!({"entryId": entry["id"]}),
                    )
                    .await?;
                    self.settle().await?;
                }
            }
            PanelAction::Refresh {} => {
                self.raw("Page.reload", json!({})).await?;
                self.settle().await?;
            }
        }
        let state = self.state().await?;
        let bytes = self.screenshot(false).await?;
        let image = image::load_from_memory(&bytes)
            .map_err(|e| format!("browser: invalid screenshot: {e}"))?;
        let metrics = self.raw("Page.getLayoutMetrics", json!({})).await?;
        Ok(PanelFrame {
            url: state.url,
            title: state.title,
            png: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
            width: image.width(),
            height: image.height(),
            viewport_width: metrics["cssVisualViewport"]["clientWidth"]
                .as_f64()
                .unwrap_or(image.width() as f64),
            viewport_height: metrics["cssVisualViewport"]["clientHeight"]
                .as_f64()
                .unwrap_or(image.height() as f64),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PanelAction {
    Snapshot {},
    Navigate { url: String },
    Click { x: f64, y: f64 },
    Type { text: String },
    Key { key: PanelKey },
    Scroll { delta_y: f64 },
    Back {},
    Refresh {},
}

#[derive(Debug, Deserialize)]
pub enum PanelKey {
    Enter,
    Tab,
}

impl PanelAction {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Scroll { delta_y } if !delta_y.is_finite() || delta_y.abs() > 2000.0 => {
                Err("browser: invalid scroll distance".into())
            }
            Self::Navigate { url } => validate_panel_url(url),
            Self::Click { x, y } if !x.is_finite() || !y.is_finite() || *x < 0.0 || *y < 0.0 => {
                Err("browser: invalid click coordinates".into())
            }
            Self::Type { text } if text.is_empty() || text.len() > 100_000 => {
                Err("browser: text must contain 1–100000 bytes".into())
            }
            _ => Ok(()),
        }
    }
}

fn validate_panel_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "browser: invalid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("browser: only HTTP and HTTPS URLs are allowed".into());
    }
    Ok(())
}

#[derive(Serialize)]
pub struct PanelFrame {
    pub url: String,
    pub title: String,
    pub png: String,
    pub width: u32,
    pub height: u32,
    pub viewport_width: f64,
    pub viewport_height: f64,
}

/// JSON-encode a string for embedding in a JS literal.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Sessions by task id, so a task keeps its page between calls.
pub fn sessions() -> &'static Lock<std::collections::HashMap<String, Arc<Session>>> {
    static S: std::sync::OnceLock<Lock<std::collections::HashMap<String, Arc<Session>>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Lock::new(std::collections::HashMap::new()))
}

/// The session for `owner`, launching one if there isn't a live one.
pub async fn session_for(owner: &str, width: u32, height: u32) -> Result<Arc<Session>, String> {
    let mut map = sessions().lock().await;
    {
        if let Some(s) = map.get(owner) {
            // A browser that died between calls leaves a dead session behind;
            // replace it rather than failing every call from here on.
            if s.raw("Page.getFrameTree", json!({})).await.is_ok() {
                return Ok(s.clone());
            }
        }
    }
    let fresh = Session::launch(width, height).await?;
    map.insert(owner.to_string(), fresh.clone());
    // Every new browser is a moment to notice the ones nobody is using any
    // more. A finished sub-agent's page is never closed explicitly (only a
    // task's main one is, at the end of a run), so without this an ultrathread
    // would leave a headless Chrome per worker running for the session.
    let stale: Vec<String> = map
        .iter()
        .filter(|(k, s)| k.as_str() != owner && s.is_idle())
        .map(|(k, _)| k.clone())
        .collect();
    drop(map);
    for k in stale {
        close(&k);
    }
    Ok(fresh)
}

/// Close a task's browser, if it has one.
pub fn close(owner: &str) {
    // Taken out and dropped outside the lock: dropping kills the browser, and
    // doing that while holding the registry lock would stall every other task.
    let victim = sessions().try_lock().ok().and_then(|mut m| m.remove(owner));
    drop(victim);
}

/// Close every browser a task owns, main agent and sub-agents alike.
pub fn close_task(task_id: &str) {
    let mut map = match sessions().try_lock() {
        Ok(m) => m,
        Err(_) => {
            let task_id = task_id.to_string();
            tokio::spawn(async move {
                let mut map = sessions().lock().await;
                let keys: Vec<String> = map
                    .keys()
                    .filter(|k| k.as_str() == task_id || k.starts_with(&format!("{task_id}:")))
                    .cloned()
                    .collect();
                let victims: Vec<_> = keys.iter().filter_map(|k| map.remove(k)).collect();
                drop(map);
                drop(victims);
            });
            return;
        }
    };
    let keys: Vec<String> = map
        .keys()
        .filter(|k| k.as_str() == task_id || k.starts_with(&format!("{task_id}:")))
        .cloned()
        .collect();
    let victims: Vec<Arc<Session>> = keys.iter().filter_map(|k| map.remove(k)).collect();
    // Killed outside the lock, for the same reason as `close`.
    drop(map);
    drop(victims);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_validation_is_narrow() {
        for url in [
            "file:///secret",
            "javascript:alert(1)",
            "data:text/html,x",
            "about:blank",
            "not a url",
        ] {
            assert!(PanelAction::Navigate { url: url.into() }
                .validate()
                .is_err());
        }
        assert!(PanelAction::Navigate {
            url: "http://localhost:3000/".into()
        }
        .validate()
        .is_ok());
        assert!(PanelAction::Click {
            x: f64::NAN,
            y: 0.0
        }
        .validate()
        .is_err());
        assert!(PanelAction::Click { x: -1.0, y: 0.0 }.validate().is_err());
        assert!(PanelAction::Type {
            text: String::new()
        }
        .validate()
        .is_err());
        assert!(serde_json::from_value::<PanelAction>(
            json!({"kind":"js","expression":"alert(1)"})
        )
        .is_err());
        assert!(
            serde_json::from_value::<PanelAction>(json!({"kind":"snapshot","owner":"other"}))
                .is_err()
        );
    }

    fn have_browser() -> bool {
        !browsers_installed().is_empty()
    }

    #[tokio::test]
    async fn drives_a_real_page() {
        if !have_browser() {
            eprintln!("no chromium installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ol-br-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("p.html");
        std::fs::write(
            &f,
            r#"<html><body style="font:30px sans-serif;padding:40px">
            <h1 id="t">BROWSER OK</h1>
            <input id="q" style="font:20px sans-serif">
            <button id="go" onclick="document.getElementById('out').textContent='CLICKED ' + document.getElementById('q').value">go</button>
            <div id="out"></div></body></html>"#,
        )
        .unwrap();
        let url = format!("file:///{}", f.display().to_string().replace('\\', "/"));
        let s = Session::launch(900, 600).await.expect("launch");

        let st = s.goto(&url).await.expect("goto");
        assert!(st.url.starts_with("file:///"), "url was {}", st.url);

        // Read the page, which is what `read` does.
        let heading = s
            .eval("document.getElementById('t').textContent")
            .await
            .expect("eval");
        assert_eq!(heading.as_str(), Some("BROWSER OK"));

        // Type into a field, then click a button and see the effect.
        s.eval("document.getElementById('q').focus()")
            .await
            .unwrap();
        s.type_text("hello").await.expect("type");
        s.click("#go").await.expect("click");
        let out = s
            .eval("document.getElementById('out').textContent")
            .await
            .expect("eval");
        assert_eq!(
            out.as_str(),
            Some("CLICKED hello"),
            "click did not reach the handler"
        );

        // A real screenshot of the page the agent just drove.
        let png = s.screenshot(false).await.expect("screenshot");
        assert!(png.len() > 1000, "suspiciously small png: {}", png.len());
        let full = s.screenshot(true).await.expect("full screenshot");
        assert!(full.len() > 1000);

        // A missing selector is an error the agent can act on, not a panic.
        assert!(s.click("#nope").await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
