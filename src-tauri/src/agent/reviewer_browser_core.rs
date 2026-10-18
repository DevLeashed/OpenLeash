//! Real-browser tests for the CDP core (`browser.rs`): snapshots and refs,
//! targeting, input, frames, tabs, dialogs, downloads, logs, emulation, and the
//! network policy as the browser enforces it.
//!
//! Every test skips when no Chromium-based browser is installed, and every page
//! comes from a tiny HTTP server on 127.0.0.1 started by the test — never the
//! internet. Each test uses its own owner key, so the tests share one throwaway
//! browser process but never a context (cookies, tabs, logs).

use super::browser::{self, Session, SessionOpts, TargetSpec};
use super::browser_snapshot as snap;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn have_browser() -> bool {
    !super::plugins::browsers_installed().is_empty()
}

macro_rules! need_browser {
    () => {
        if !have_browser() {
            eprintln!("no chromium installed; skipping");
            return;
        }
    };
}

/// One canned response.
#[derive(Clone)]
pub(crate) struct Page {
    ctype: &'static str,
    body: String,
    headers: Vec<(String, String)>,
    status: u16,
}

pub(crate) fn html(body: &str) -> Page {
    Page {
        ctype: "text/html; charset=utf-8",
        body: body.to_string(),
        headers: vec![],
        status: 200,
    }
}

/// A request the server saw: path and lowercased headers.
pub(crate) type Seen = Arc<Mutex<Vec<(String, HashMap<String, String>)>>>;

/// Serve `pages` on 127.0.0.1; returns the port and the request log.
pub(crate) async fn serve(pages: HashMap<&'static str, Page>) -> (u16, Seen) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Seen = Default::default();
    let log = seen.clone();
    let pages = Arc::new(pages);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = l.accept().await else {
                return;
            };
            let pages = pages.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if buf.len() > 64 * 1024 {
                        return;
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let mut lines = head.split("\r\n");
                let first = lines.next().unwrap_or("");
                let path = first.split(' ').nth(1).unwrap_or("/").to_string();
                let mut hs = HashMap::new();
                for l in lines.take_while(|l| !l.is_empty()) {
                    if let Some((k, v)) = l.split_once(':') {
                        hs.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                log.lock().unwrap().push((path.clone(), hs));
                let key = path.split('?').next().unwrap_or("/");
                let page = pages.get(key).cloned().unwrap_or(Page {
                    ctype: "text/plain",
                    body: "not found".into(),
                    headers: vec![],
                    status: 404,
                });
                let mut resp = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
                    page.status,
                    page.ctype,
                    page.body.len()
                );
                for (k, v) in &page.headers {
                    resp.push_str(&format!("{k}: {v}\r\n"));
                }
                resp.push_str("\r\n");
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(page.body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen)
}

fn owner(name: &str) -> String {
    format!("test-{name}-{}:main", uuid::Uuid::new_v4().simple())
}

async fn session(name: &str, project: Option<std::path::PathBuf>) -> (String, Arc<Session>) {
    let o = owner(name);
    let dl = std::env::temp_dir().join(format!("ol-br-dl-{}", uuid::Uuid::new_v4().simple()));
    let s = browser::session_with(
        &o,
        SessionOpts {
            width: 1000,
            height: 700,
            project,
            downloads: Some(dl),
            ..Default::default()
        },
    )
    .await
    .expect("session");
    (o, s)
}

fn by_ref(r: &str) -> TargetSpec {
    TargetSpec {
        ref_: Some(r.into()),
        ..Default::default()
    }
}

fn by_text(t: &str) -> TargetSpec {
    TargetSpec {
        text: Some(t.into()),
        ..Default::default()
    }
}

/// The ref on the first snapshot line containing `needle`.
fn ref_in(snapshot: &str, needle: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{snapshot}"));
    let start = line.find('[').expect("a ref") + 1;
    let end = line[start..].find(']').unwrap() + start;
    line[start..end].to_string()
}

async fn click(s: &Session, t: &TargetSpec) {
    let r = s.resolve(t).await.expect("resolve");
    let (x, y) = s
        .point_for(&r, false, Duration::from_secs(5))
        .await
        .expect("actionable");
    s.click_at(x, y, "left", 1, 0).await.expect("click");
    s.settle().await.unwrap();
}

async fn text_of(s: &Session, sel: &str) -> String {
    let v = s.query(sel, 1).await.unwrap();
    v["matches"][0]["text"].as_str().unwrap_or("").to_string()
}

const FORM: &str = r#"<!doctype html><title>Form</title><body style="font:16px sans-serif">
<h2>Settings</h2>
<label for=email>Email</label> <input id=email name=email>
<label>Password <input id=pw type=password></label>
<label><input id=agree type=checkbox> I agree</label>
<select id=plan aria-label=Plan><option value=free>Free</option><option value=pro>Pro</option></select>
<button id=save onclick="document.getElementById('out').textContent='SAVED '+email.value+' '+plan.value+' '+agree.checked">Save</button>
<button id=draft>Save draft</button>
<a href="/docs">Docs</a>
<div id=out></div>
<input id=keys onkeydown="if(event.key==='Enter')document.getElementById('out').textContent='ENTER '+this.value">
</body>"#;

#[tokio::test]
async fn snapshot_refs_and_targeting_drive_a_form() {
    need_browser!();
    let (port, _) = serve(HashMap::from([("/form", html(FORM))])).await;
    let (o, s) = session("form", None).await;
    let st = s
        .goto(&format!("http://127.0.0.1:{port}/form"))
        .await
        .expect("goto");
    assert_eq!(st.title, "Form");

    let snapshot = s.snapshot(None, false, 20_000).await.expect("snapshot");
    eprintln!("{snapshot}");
    assert!(
        snapshot.contains("heading \"Settings\" level=2"),
        "{snapshot}"
    );
    assert!(snapshot.contains("textbox \"Email\""), "{snapshot}");
    assert!(snapshot.contains("link \"Docs\" -> /docs"), "{snapshot}");
    assert!(
        snapshot.contains("checkbox \"I agree\" [unchecked]"),
        "{snapshot}"
    );
    let save = ref_in(&snapshot, "button \"Save\"");
    let email = ref_in(&snapshot, "textbox \"Email\"");

    // Refs are stable across snapshots of the same element.
    let again = s.snapshot(None, true, 20_000).await.unwrap();
    assert_eq!(ref_in(&again, "button \"Save\""), save);
    assert!(
        !again.contains("heading"),
        "interactive_only drops structure:\n{again}"
    );

    // fill → select → check → click by ref.
    let r = s.resolve(&by_ref(&email)).await.unwrap();
    assert!(s.focus(&r, true).await.unwrap());
    s.type_text("a@b.c").await.unwrap();
    let plan = s.resolve(&by_text("Plan")).await.unwrap();
    assert_eq!(
        s.select_option(&plan, &["Pro".into()]).await.unwrap(),
        ["Pro"]
    );
    click(&s, &by_text("I agree")).await;
    let agree = s
        .resolve(&TargetSpec {
            selector: Some("#agree".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(s.checked_of(&agree).await, Some(true));
    click(&s, &by_ref(&save)).await;
    assert_eq!(text_of(&s, "#out").await, "SAVED a@b.c pro true");

    // Click by visible text, and a near miss names the candidates.
    let err = s.resolve(&by_text("Sav draftt")).await.unwrap_err();
    assert!(
        err.contains("did you mean") && err.contains("Save draft"),
        "{err}"
    );
    let err = s.resolve(&by_ref("e9999")).await.unwrap_err();
    assert!(err.contains("no ref e9999"), "{err}");

    // Password values never leave the page in a snapshot.
    let pw = s
        .resolve(&TargetSpec {
            selector: Some("#pw".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    s.focus(&pw, true).await.unwrap();
    s.type_text("hunter2").await.unwrap();
    let snap2 = s.snapshot(None, false, 20_000).await.unwrap();
    assert!(!snap2.contains("hunter2"), "{snap2}");
    let facts = s.facts(&pw).await.unwrap();
    assert_eq!(facts.input_type, "password");

    // press: Enter in a field, and a combo.
    let k = s
        .resolve(&TargetSpec {
            selector: Some("#keys".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    s.focus(&k, false).await.unwrap();
    s.type_text("abc").await.unwrap();
    s.press(&snap::parse_keys("Control+A Backspace").unwrap())
        .await
        .unwrap();
    s.type_text("xyz").await.unwrap();
    s.press(&snap::parse_keys("Enter").unwrap()).await.unwrap();
    assert_eq!(text_of(&s, "#out").await, "ENTER xyz");

    browser::close(&o);
}

const COVERED: &str = r#"<!doctype html><title>Covered</title>
<button id=under onclick="this.textContent='HIT'">Under</button>
<div id=cover style="position:fixed;inset:0;background:rgba(0,0,0,.3)"><button>Dismiss</button></div>"#;

#[tokio::test]
async fn covered_elements_are_reported_not_clicked() {
    need_browser!();
    let (port, _) = serve(HashMap::from([("/c", html(COVERED))])).await;
    let (o, s) = session("covered", None).await;
    s.goto(&format!("http://127.0.0.1:{port}/c")).await.unwrap();
    let r = s.resolve(&by_text("Under")).await.unwrap();
    let err = s
        .point_for(&r, false, Duration::from_millis(600))
        .await
        .unwrap_err();
    assert!(
        err.contains("is covered by") && err.contains("force"),
        "{err}"
    );
    // force skips the hit test.
    assert!(s.point_for(&r, true, Duration::from_secs(2)).await.is_ok());
    browser::close(&o);
}

#[tokio::test]
async fn frames_and_shadow_dom_appear_in_the_snapshot_and_are_clickable() {
    need_browser!();
    let (port, _) = serve(HashMap::from([
        (
            "/frames",
            html(
                r#"<!doctype html><title>Frames</title>
<button>Top</button>
<iframe src="/inner" style="width:300px;height:120px"></iframe>
<iframe id=x src="http://localhost:{PORT}/xorigin" style="width:300px;height:120px;margin-left:40px"></iframe>
<open-el></open-el><closed-el></closed-el>
<script>
customElements.define('open-el', class extends HTMLElement { constructor() { super(); this.attachShadow({mode:'open'}).innerHTML = '<button>Open shadow</button>'; } });
customElements.define('closed-el', class extends HTMLElement { constructor() { super(); const r = this.attachShadow({mode:'closed'}); r.innerHTML = '<button>Closed shadow</button>'; } });
</script>"#
            ),
        ),
        ("/inner", html("<button onclick=\"this.textContent='INNER HIT'\">Inner</button>")),
        (
            "/xorigin",
            html("<button onclick=\"this.textContent='XO HIT'\">Cross origin</button>"),
        ),
    ]))
    .await;
    // The cross-origin frame needs the port in its url; patch it in.
    let (o, s) = session("frames", None).await;
    let url = format!("http://127.0.0.1:{port}/frames");
    s.goto(&url).await.unwrap();
    // `{PORT}` was literal in the page: rewrite the second iframe's src.
    s.eval(&format!(
        "document.getElementById('x').src = 'http://localhost:{port}/xorigin'; 1"
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    s.settle().await.unwrap();
    let snapshot = s.snapshot(None, false, 20_000).await.unwrap();
    eprintln!("{snapshot}");
    assert!(
        snapshot.contains("button \"Inner\""),
        "same-origin iframe:\n{snapshot}"
    );
    assert!(
        snapshot.contains("button \"Open shadow\""),
        "open shadow root:\n{snapshot}"
    );
    assert!(
        snapshot.contains("button \"Closed shadow\""),
        "closed shadow root:\n{snapshot}"
    );
    assert!(
        snapshot.contains("button \"Cross origin\""),
        "cross-origin iframe:\n{snapshot}"
    );
    let xo = ref_in(&snapshot, "button \"Cross origin\"");
    assert!(
        xo.starts_with('f'),
        "cross-origin refs carry their frame: {xo}"
    );

    click(&s, &by_ref(&ref_in(&snapshot, "button \"Inner\""))).await;
    click(&s, &by_ref(&xo)).await;
    let after = s.snapshot(None, false, 20_000).await.unwrap();
    assert!(after.contains("INNER HIT"), "{after}");
    assert!(
        after.contains("XO HIT"),
        "click landed in the cross-origin frame:\n{after}"
    );
    // A selector pierces open shadow DOM without extra syntax.
    assert!(s
        .resolve(&TargetSpec {
            selector: Some("open-el >>> button".into()),
            ..Default::default()
        })
        .await
        .is_ok());
    browser::close(&o);
}

#[tokio::test]
async fn popups_become_tabs_and_dialogs_wait_for_an_answer() {
    need_browser!();
    let (port, _) = serve(HashMap::from([
        (
            "/p",
            html(r#"<!doctype html><title>Opener</title>
<a href="/second" target="_blank">New tab</a>
<button id=c onclick="document.getElementById('o').textContent = confirm('Delete it?') ? 'YES' : 'NO'">Confirm</button>
<button id=p onclick="document.getElementById('o').textContent = prompt('Name?', 'x')">Prompt</button>
<div id=o></div>"#),
        ),
        ("/second", html("<title>Second</title><h1>Second page</h1>")),
    ]))
    .await;
    let (o, s) = session("popup", None).await;
    s.goto(&format!("http://127.0.0.1:{port}/p")).await.unwrap();

    // Dialogs: the click returns, the dialog is reported, evaluation refuses
    // rather than hanging, and accept/dismiss both work.
    click(
        &s,
        &TargetSpec {
            selector: Some("#c".into()),
            ..Default::default()
        },
    )
    .await;
    let d = s.pending_dialog().await.expect("a dialog");
    assert_eq!(
        (d.kind.as_str(), d.message.as_str()),
        ("confirm", "Delete it?")
    );
    let err = tokio::time::timeout(Duration::from_secs(10), s.eval("1"))
        .await
        .expect("must not hang")
        .unwrap_err();
    assert!(err.starts_with(browser::DIALOG_OPEN), "{err}");
    assert!(s.take_report().dialog.is_some());
    s.answer_dialog(false, None).await.unwrap();
    assert_eq!(text_of(&s, "#o").await, "NO");
    click(
        &s,
        &TargetSpec {
            selector: Some("#p".into()),
            ..Default::default()
        },
    )
    .await;
    s.answer_dialog(true, Some("Ada")).await.unwrap();
    assert_eq!(text_of(&s, "#o").await, "Ada");

    // target=_blank opens a tab in this session and makes it active.
    click(&s, &by_text("New tab")).await;
    let mut tabs = vec![];
    for _ in 0..50 {
        tabs = s.tabs().await.unwrap();
        if tabs.len() == 2 && tabs.iter().any(|t| t.active && t.url.ends_with("/second")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(tabs.len(), 2, "{tabs:?}");
    let active = tabs.iter().find(|t| t.active).unwrap();
    assert!(active.url.ends_with("/second"), "{tabs:?}");
    let rep = s.take_report();
    assert_eq!(rep.new_tabs.len(), 1, "{rep:?}");
    s.settle().await.unwrap();
    assert!(s.read_text(None, 1000).await.unwrap()["text"]
        .as_str()
        .unwrap()
        .contains("Second page"));
    // Switch back and close the popup.
    let first = tabs.iter().find(|t| !t.active).unwrap().id.clone();
    let second = active.id.clone();
    s.switch_tab(&first).await.unwrap();
    assert!(s.state().await.unwrap().url.ends_with("/p"));
    s.close_tab(&second).await.unwrap();
    assert_eq!(s.tabs().await.unwrap().len(), 1);
    // A tab of our own.
    let t = s
        .new_tab(Some(&format!("http://127.0.0.1:{port}/second")))
        .await
        .unwrap();
    assert!(t.url.ends_with("/second") || s.state().await.unwrap().url.ends_with("/second"));
    browser::close(&o);
}

#[tokio::test]
async fn downloads_land_sanitised_in_the_session_folder() {
    need_browser!();
    let mut file = html("hello download");
    file.ctype = "application/octet-stream";
    file.headers = vec![(
        "Content-Disposition".into(),
        "attachment; filename=\"..\\\\..\\\\evil:name.txt\"".into(),
    )];
    let (port, _) = serve(HashMap::from([
        (
            "/d",
            html("<title>D</title><a id=dl href=\"/file\">Get it</a>"),
        ),
        ("/file", file),
    ]))
    .await;
    let dir = std::env::temp_dir().join(format!("ol-br-dl-{}", uuid::Uuid::new_v4().simple()));
    let o = owner("download");
    let s = browser::session_with(
        &o,
        SessionOpts {
            width: 900,
            height: 600,
            downloads: Some(dir.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    s.goto(&format!("http://127.0.0.1:{port}/d")).await.unwrap();
    click(&s, &by_text("Get it")).await;
    let mut done = None;
    for _ in 0..100 {
        if let Some(d) = s.downloads().into_iter().find(|d| d.state == "completed") {
            done = Some(d);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let d = done.expect("the download completed");
    let path = std::path::PathBuf::from(d.path.clone().unwrap());
    assert!(path.starts_with(&dir), "{path:?} outside {dir:?}");
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(path.parent(), Some(dir.as_path()), "{path:?}");
    assert!(
        !name.contains(':') && !name.contains('\\') && !name.contains('/'),
        "{name}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello download");
    let rep = s.take_report();
    assert_eq!(rep.downloads.len(), 1, "reported once: {rep:?}");
    assert!(s.take_report().downloads.is_empty());
    browser::close(&o);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn console_and_network_logs_redact_credentials() {
    need_browser!();
    let mut api = html("{\"ok\":true}");
    api.ctype = "application/json";
    let (port, seen) = serve(HashMap::from([
        (
            "/logs",
            html(r#"<title>Logs</title><script>
console.log('hello', 42); console.error('boom');
fetch('/api', {headers: {Authorization: 'Bearer s3cret-token'}}).then(r => r.json()).then(j => console.info('api', j.ok));
setTimeout(() => { throw new Error('kaboom'); }, 10);
</script>"#),
        ),
        ("/api", api),
    ]))
    .await;
    let (o, s) = session("logs", None).await;
    s.goto(&format!("http://127.0.0.1:{port}/logs"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let console = s.console(None, false, 100);
    let joined: Vec<String> = console
        .iter()
        .map(|l| format!("[{}] {}", l.level, l.text))
        .collect();
    let joined = joined.join("\n");
    assert!(joined.contains("[log] hello 42"), "{joined}");
    assert!(joined.contains("[error] boom"), "{joined}");
    assert!(joined.contains("kaboom"), "{joined}");
    assert!(
        s.take_report().console_errors == 0,
        "reading the console marks errors seen"
    );
    let errors = s.console(Some("error"), true, 100);
    assert!(errors
        .iter()
        .all(|l| matches!(l.level.as_str(), "error" | "exception" | "assert")));
    assert!(
        s.console(None, false, 100).is_empty(),
        "clear empties the buffer"
    );

    let net = s.network(Some("/api"), false, false, 50);
    let e = net.first().expect("the api call is logged");
    assert_eq!((e.method.as_str(), e.status), ("GET", Some(200)));
    let headers = e.request_headers.to_string();
    assert!(!headers.contains("s3cret"), "{headers}");
    // The server did get it: redaction is in the log, not the request.
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|(p, h)| p == "/api" && h.get("authorization").is_some_and(|v| v.contains("s3cret"))));
    let (_, body) = s.network_body(&e.id, 1000).await.unwrap();
    assert_eq!(body, "{\"ok\":true}");
    browser::close(&o);
}

#[tokio::test]
async fn wait_emulate_pdf_marks_and_diff() {
    need_browser!();
    let (port, _) = serve(HashMap::from([(
        "/w",
        html(r#"<!doctype html><meta name=viewport content="width=device-width"><title>W</title><style>
body{background:#fff;color:#000} @media (prefers-color-scheme: dark){body{background:#000;color:#fff}}
</style><button onclick="document.body.insertAdjacentHTML('beforeend','<p>more</p>')">Add</button>
<a href="/x">Link</a><input placeholder=Search>
<script>setTimeout(() => document.body.insertAdjacentHTML('beforeend', '<div id=late>Late arrival</div>'), 400)</script>"#),
    )]))
    .await;
    let (o, s) = session("wait", None).await;
    s.goto(&format!("http://127.0.0.1:{port}/w")).await.unwrap();
    // wait_for building blocks.
    let mut ok = false;
    for _ in 0..40 {
        if s.selector_state("#late").await.unwrap()["visible"] == json!(true) {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ok);
    assert!(s.text_present("late arrival").await.unwrap());
    assert!(s.wait_network_idle(Duration::from_secs(5)).await);

    // Emulation: a phone in dark mode, then back.
    let dark_q = "matchMedia('(prefers-color-scheme: dark)').matches";
    // The OS theme decides the default; reset must return to it.
    let baseline = s.world_eval(dark_q).await.unwrap();
    let summary = s
        .emulate(
            snap::device("iphone-15"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("dark".into()),
            false,
        )
        .await
        .unwrap();
    assert!(
        summary.contains("393x852") && summary.contains("dark"),
        "{summary}"
    );
    let info = s.page_info().await.unwrap();
    assert_eq!(info["viewport"]["width"], json!(393));
    let dark = s
        .world_eval("matchMedia('(prefers-color-scheme: dark)').matches")
        .await
        .unwrap();
    assert_eq!(dark, json!(true));
    let ua = s.eval("navigator.userAgent").await.unwrap();
    assert!(ua.as_str().unwrap().contains("iPhone"), "{ua}");
    s.emulate(None, None, None, None, None, None, None, None, true)
        .await
        .unwrap();
    assert_eq!(
        s.page_info().await.unwrap()["viewport"]["width"],
        json!(1000)
    );
    assert_eq!(s.world_eval(dark_q).await.unwrap(), baseline);
    s.emulate(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("light".into()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(s.world_eval(dark_q).await.unwrap(), json!(false));
    s.emulate(None, None, None, None, None, None, None, None, true)
        .await
        .unwrap();

    // PDF.
    let pdf = s.pdf().await.unwrap();
    assert!(pdf.starts_with(b"%PDF"), "not a pdf");

    // Set-of-marks: labels drawn, then removed.
    let (png, n) = s.screenshot_marks().await.unwrap();
    assert!(png.len() > 1000 && n >= 3, "{n} marks");
    assert_eq!(
        s.query("#__openleash_marks__", 1).await.unwrap()["count"],
        json!(0),
        "overlay removed"
    );

    // Diff: baseline, a change, a highlighted region.
    // (The marks screenshot above isn't a baseline: only plain viewport
    // screenshots are, so the first diff here compares two clean frames or
    // starts the baseline.)
    let _ = s.diff().await.unwrap();
    let (same, _) = s.diff().await.unwrap();
    assert_eq!(same.unwrap().bbox, None, "nothing changed");
    click(&s, &by_text("Add")).await;
    let (changed, _) = s.diff().await.unwrap();
    let changed = changed.unwrap();
    assert!(changed.bbox.is_some() && changed.changed_pct > 0.0);

    // Element and full-page screenshots.
    let r = s.resolve(&by_text("Add")).await.unwrap();
    let el = s.screenshot_element(&r).await.unwrap();
    let img = image::load_from_memory(&el).unwrap();
    assert!(
        img.width() < 300 && img.height() < 100,
        "{}x{}",
        img.width(),
        img.height()
    );
    assert!(s.screenshot(true).await.unwrap().len() > 1000);
    browser::close(&o);
}

/// The browser's half of the network policy (request interception), with
/// the real `browser_policy`: metadata addresses in any spelling are failed
/// before they leave the browser, loopback dev servers work, and `file://`
/// outside the project is refused.
#[tokio::test]
async fn network_policy_blocks_metadata_and_foreign_files() {
    need_browser!();
    let (port, seen) = serve(HashMap::from([
        ("/ok", html("<title>OK</title><p>loopback works</p>")),
        (
            "/img",
            html(
                "<title>Img</title><img src=\"http://[::ffff:169.254.169.254]/latest/meta-data/\">",
            ),
        ),
    ]))
    .await;
    let project =
        std::env::temp_dir().join(format!("ol-br-proj-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("in.html"),
        "<title>Inside</title><p>inside the project</p>",
    )
    .unwrap();
    let outside =
        std::env::temp_dir().join(format!("ol-br-out-{}.html", uuid::Uuid::new_v4().simple()));
    std::fs::write(&outside, "<title>Outside</title><p>SECRET outside</p>").unwrap();
    let (o, s) = session("policy", Some(project.clone())).await;

    // Loopback dev servers are the core use case.
    let st = s
        .goto(&format!("http://127.0.0.1:{port}/ok"))
        .await
        .unwrap();
    assert_eq!(st.title, "OK");

    // Metadata, in the forms a page might use.
    for target in [
        "http://169.254.169.254/latest/meta-data/",
        "http://0xA9FEA9FE/",
        "http://[::ffff:169.254.169.254]/",
    ] {
        let v = s
            .eval(&format!(
                "fetch({}).then(() => 'REACHED', e => 'FAILED ' + e.message)",
                serde_json::to_string(target).unwrap()
            ))
            .await
            .unwrap();
        assert!(v.as_str().unwrap().starts_with("FAILED"), "{target}: {v}");
    }
    let rep = s.take_report();
    assert!(rep.blocked.len() >= 3, "every block is reported: {rep:?}");

    // An <img> to the v4-mapped metadata address.
    s.goto(&format!("http://127.0.0.1:{port}/img"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rep = s.take_report();
    assert!(
        rep.blocked
            .iter()
            .any(|b| b.contains("169.254.169.254") || b.contains("a9fe:a9fe")),
        "{rep:?}"
    );

    // Same-origin loopback fetches work; the CDP port itself is out of reach.
    let v = s.eval("fetch('/ok').then(r => r.status)").await.unwrap();
    assert_eq!(v, json!(200));
    let v = s
        .eval(&format!(
            "fetch('http://127.0.0.1:{}/json/version', {{mode: 'no-cors'}}).then(() => 'REACHED', e => 'FAILED')",
            s.cdp_port()
        ))
        .await
        .unwrap();
    assert_eq!(v, json!("FAILED"), "page script reached the debugging port");

    // file:// inside the project loads, outside it does not.
    let inside = format!(
        "file:///{}",
        project
            .join("in.html")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    assert_eq!(s.goto(&inside).await.unwrap().title, "Inside");
    let out_url = format!(
        "file:///{}",
        outside.display().to_string().replace('\\', "/")
    );
    assert!(matches!(
        s.verdict(&out_url).await,
        super::browser_policy::NetVerdict::Block(_)
    ));
    let outcome = s.goto(&out_url).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    eprintln!(
        "file:// outside the project: goto -> {outcome:?}; report {:?}",
        s.take_report()
    );
    let text = s
        .read_text(None, 1000)
        .await
        .map(|v| v["text"].as_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        !text.contains("SECRET"),
        "a file outside the project was readable: {text}"
    );
    // And a file:// page can't pull one in by link either.
    s.goto(&inside).await.unwrap();
    let v = s
        .eval(&format!(
            "fetch({}).then(r => r.text()).then(t => 'READ ' + t, e => 'FAILED')",
            serde_json::to_string(&out_url).unwrap()
        ))
        .await
        .unwrap();
    assert!(!v.as_str().unwrap_or("").contains("SECRET"), "{v}");

    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .all(|(p, _)| !p.contains("meta-data")));
    browser::close(&o);
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_file(outside);
}

/// Owners never share cookies: each throwaway session is its own context.
#[tokio::test]
async fn sessions_do_not_share_cookies() {
    need_browser!();
    let mut set = html("<title>Set</title>");
    set.headers = vec![
        ("Set-Cookie".into(), "who=alice; Path=/".into()),
        ("Set-Cookie".into(), "tok=hidden; Path=/; HttpOnly".into()),
    ];
    let (port, _) = serve(HashMap::from([
        ("/set", set),
        ("/see", html("<title>See</title>")),
    ]))
    .await;
    let (a_owner, a) = session("cookie-a", None).await;
    let (b_owner, b) = session("cookie-b", None).await;
    a.goto(&format!("http://127.0.0.1:{port}/set"))
        .await
        .unwrap();
    let ca = a.cookies().await.unwrap();
    assert!(
        ca.iter()
            .any(|c| c["name"] == "who" && c["value"] == "alice"),
        "{ca:?}"
    );
    assert!(
        ca.iter()
            .any(|c| c["name"] == "tok" && c["value"] == "[httpOnly]"),
        "httpOnly values hidden: {ca:?}"
    );
    b.goto(&format!("http://127.0.0.1:{port}/see"))
        .await
        .unwrap();
    let cb = b.cookies().await.unwrap();
    assert!(cb.is_empty(), "another owner's cookies leaked: {cb:?}");
    browser::close(&a_owner);
    browser::close(&b_owner);
}

/// Empirical notes the module docs rely on, printed for the record: does Fetch
/// see redirects hop by hop?
#[tokio::test]
async fn redirects_are_judged_hop_by_hop() {
    need_browser!();
    let mut redirect = html("");
    redirect.status = 302;
    redirect.headers = vec![(
        "Location".into(),
        "http://169.254.169.254/latest/meta-data/".into(),
    )];
    let (port, _) = serve(HashMap::from([("/r", redirect)])).await;
    let (o, s) = session("redirect", None).await;
    let r = s.goto(&format!("http://127.0.0.1:{port}/r")).await;
    assert!(r.is_err(), "a redirect to metadata must fail: {r:?}");
    let rep = s.take_report();
    assert!(
        rep.blocked.iter().any(|b| b.contains("169.254.169.254")),
        "{rep:?}"
    );
    browser::close(&o);
}

#[test]
fn launch_flags_never_open_the_debugger_to_web_origins() {
    // The flag list is private; the module's own text is the contract.
    let src = include_str!("browser.rs");
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("\"--remote-allow-origins"),
        "remote-allow-origins must not be passed"
    );
    for f in [
        "--disable-quic",
        "--dns-prefetch-disable",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--proxy-bypass-list=<-loopback>",
    ] {
        assert!(code.contains(f), "missing {f}");
    }
}

#[test]
fn target_spec_reads_text_only_where_it_names_the_target() {
    let v = json!({"text": "hello", "ref": "[e3]"});
    let click = TargetSpec::from_input(&v, true).unwrap();
    assert_eq!(click.ref_.as_deref(), Some("e3"));
    assert_eq!(click.text.as_deref(), Some("hello"));
    let typing = TargetSpec::from_input(&v, false).unwrap();
    assert_eq!(typing.text, None, "for type/fill, text is content");
    let named = TargetSpec::from_input(&json!({"name": "Save", "text": "x"}), false).unwrap();
    assert_eq!(named.text.as_deref(), Some("Save"));
    assert!(TargetSpec::from_input(&json!({"ref": "button"}), true).is_err());
    assert!(TargetSpec::from_input(&json!({"x": 3}), true).is_err());
    assert!(TargetSpec::from_input(&json!({}), true).unwrap().is_empty());
    let _: Value = json!(null);
}

/// Workers and service workers are attached (paused) and intercepted like
/// pages: a fetch from either to a metadata address never leaves the browser.
#[tokio::test]
async fn workers_and_service_workers_are_intercepted() {
    need_browser!();
    let mut sw = html(
        "self.addEventListener('message', e => fetch('http://169.254.169.254/sw').then(() => e.source.postMessage('REACHED'), () => e.source.postMessage('FAILED')));\nself.addEventListener('activate', e => e.waitUntil(self.clients.claim()));",
    );
    sw.ctype = "text/javascript";
    let mut worker = html(
        "onmessage = () => fetch('http://169.254.169.254/worker').then(() => postMessage('REACHED'), () => postMessage('FAILED'));",
    );
    worker.ctype = "text/javascript";
    let (port, seen) = serve(HashMap::from([
        ("/w", html("<title>Workers</title>")),
        ("/sw.js", sw),
        ("/worker.js", worker),
    ]))
    .await;
    let (o, s) = session("workers", None).await;
    s.goto(&format!("http://127.0.0.1:{port}/w")).await.unwrap();
    let w = s
        .eval("new Promise(r => { const w = new Worker('/worker.js'); w.onmessage = e => r(e.data); w.postMessage(1); setTimeout(() => r('TIMEOUT'), 8000); })")
        .await
        .unwrap();
    assert_ne!(w, json!("REACHED"), "a dedicated worker reached metadata");
    let v = s
        .eval("navigator.serviceWorker.register('/sw.js').then(() => navigator.serviceWorker.ready).then(reg => new Promise(r => { navigator.serviceWorker.onmessage = e => r(e.data); reg.active.postMessage(1); setTimeout(() => r('TIMEOUT'), 8000); }))")
        .await
        .unwrap();
    assert_ne!(v, json!("REACHED"), "a service worker reached metadata");
    let rep = s.take_report();
    eprintln!("worker: {w}, service worker: {v}, report: {rep:?}");
    assert!(
        rep.blocked.iter().any(|b| b.contains("/worker")),
        "worker fetch judged by interception: {rep:?}"
    );
    assert!(
        rep.blocked.iter().any(|b| b.contains("/sw")),
        "service worker fetch judged by interception: {rep:?}"
    );
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .all(|(p, _)| p != "/sw" && p != "/worker"));
    browser::close(&o);
}

/// "Keep sign-ins between chats": one profile under the data dir, shared by
/// every chat and surviving a session closing.
#[tokio::test]
// test_home's process-wide guard intentionally spans the async test.
#[allow(clippy::await_holding_lock)]
async fn persistent_profile_lives_under_the_data_dir_and_keeps_cookies() {
    need_browser!();
    let home = std::env::temp_dir().join(format!("ol-br-home-{}", uuid::Uuid::new_v4().simple()));
    let _home = super::store::test_home(&home);
    let mut set = html("<title>Set</title>");
    set.headers = vec![("Set-Cookie".into(), "kept=yes; Path=/; Max-Age=3600".into())];
    let (port, _) = serve(HashMap::from([
        ("/set", set),
        ("/see", html("<title>See</title>")),
    ]))
    .await;
    let opts = SessionOpts {
        width: 900,
        height: 600,
        persistent: true,
        ..Default::default()
    };
    let a = owner("persist-a");
    let s = browser::session_with(&a, opts.clone())
        .await
        .expect("persistent session");
    assert!(
        home.join("browser-profile").is_dir(),
        "profile under the data dir"
    );
    s.goto(&format!("http://127.0.0.1:{port}/set"))
        .await
        .unwrap();
    drop(s);
    browser::close(&a);
    let b = owner("persist-b");
    let s = browser::session_with(&b, opts).await.unwrap();
    s.goto(&format!("http://127.0.0.1:{port}/see"))
        .await
        .unwrap();
    let c = s.cookies().await.unwrap();
    assert!(
        c.iter().any(|c| c["name"] == "kept" && c["value"] == "yes"),
        "{c:?}"
    );
    drop(s);
    browser::close(&b);
    // Let the process go before the data dir is removed.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let _ = std::fs::remove_dir_all(&home);
}
