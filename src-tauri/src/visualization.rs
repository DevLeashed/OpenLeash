//! Ephemeral, independently served interactive documents. Never writes model HTML to disk.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{http, Manager, State, Webview};

pub const MAX_SOURCE: usize = 256 * 1024;
const MAX_DOCUMENTS: usize = 64;
const MAX_TOTAL: usize = 8 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(30 * 60);
/// Where a preview document may load a host-owned library from. The frame's
/// origin is opaque (`sandbox allow-scripts`, never `allow-same-origin`), so
/// `'self'` would match nothing; the library directory is named explicitly for
/// every form `convertFileSrc` produces. Only `LIBRARY_PATH` is ever served
/// there, and documents live at `/<uuid>` outside it.
pub const LIBRARY_SOURCES: &str =
    "http://openleash-viz.localhost/lib/ https://openleash-viz.localhost/lib/ openleash-viz://localhost/lib/";
/// The single library path, relative to the document, so the same markup
/// resolves under the native protocol (`/lib/echarts.js`) and remote access
/// (`/__openleash/viz/lib/echarts.js`).
pub const LIBRARY_PATH: &str = "lib/echarts.js";

/// Bundled Apache ECharts (Apache-2.0; npm `echarts`, pinned by
/// package-lock.json) followed by the host theme shim. Embedded rather than
/// inlined into each document: at ~1.1 MB it is several times `MAX_SOURCE`, and
/// eight charts would exhaust `MAX_TOTAL` on their own.
static ECHARTS: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut lib = include_bytes!("../../node_modules/echarts/dist/echarts.min.js").to_vec();
    lib.extend_from_slice(b"\n");
    lib.extend_from_slice(include_bytes!("viz_echarts_theme.js"));
    lib
});

/// The policy for preview documents. `library_sources` are the only scripts a
/// document may load from outside itself; remote access passes its own origin's
/// library directory, since its frames are not served from the native scheme.
pub fn csp(library_sources: &str) -> String {
    format!("default-src 'none'; script-src 'unsafe-inline' {library_sources}; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; sandbox allow-scripts")
}

struct Document {
    source: String,
    expires: Instant,
}
#[derive(Default)]
pub struct Registry(Mutex<HashMap<String, Document>>);
impl Registry {
    fn publish(&self, source: String, now: Instant) -> Result<String, String> {
        if source.is_empty() || source.len() > MAX_SOURCE {
            return Err("Visualization exceeds 256 KiB or is empty".into());
        }
        let mut docs = self
            .0
            .lock()
            .map_err(|_| "Visualization registry unavailable")?;
        docs.retain(|_, d| d.expires > now);
        if docs.len() >= MAX_DOCUMENTS
            || docs.values().map(|d| d.source.len()).sum::<usize>() + source.len() > MAX_TOTAL
        {
            return Err("Visualization memory limit reached; stop another preview".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        docs.insert(
            id.clone(),
            Document {
                source,
                expires: now + TTL,
            },
        );
        Ok(id)
    }
    fn read(&self, id: &str, now: Instant) -> Option<String> {
        let mut docs = self.0.lock().ok()?;
        docs.retain(|_, d| d.expires > now);
        docs.get(id).map(|d| d.source.clone())
    }
    fn release(&self, id: &str) {
        if let Ok(mut docs) = self.0.lock() {
            docs.remove(id);
        }
    }
}
fn trusted_caller(webview: &Webview) -> Result<(), String> {
    let url = webview.url().map_err(|e| e.to_string())?;
    if webview.label() == "main" && trusted_url(&url) {
        Ok(())
    } else {
        Err("Visualization commands require the main app webview".into())
    }
}
fn trusted_url(url: &tauri::Url) -> bool {
    match (url.scheme(), url.host_str(), url.port()) {
        ("tauri", Some("localhost"), None) => true,
        ("http" | "https", Some("tauri.localhost"), None) => true,
        #[cfg(debug_assertions)]
        ("http", Some("localhost"), Some(1420)) => true,
        _ => false,
    }
}
#[tauri::command]
pub fn visualization_publish(
    webview: Webview,
    registry: State<'_, Registry>,
    source: String,
) -> Result<String, String> {
    trusted_caller(&webview)?;
    registry.publish(source, Instant::now())
}
#[tauri::command]
pub fn visualization_read(
    webview: Webview,
    registry: State<'_, Registry>,
    id: String,
) -> Result<Option<String>, String> {
    trusted_caller(&webview)?;
    Ok(registry.read(&id, Instant::now()))
}
#[tauri::command]
pub fn visualization_release(
    webview: Webview,
    registry: State<'_, Registry>,
    id: String,
) -> Result<(), String> {
    trusted_caller(&webview)?;
    registry.release(&id);
    Ok(())
}
/// The request's path without its leading slash, when it is a plain GET for
/// the visualization host.
fn request_path(request: &http::Request<Vec<u8>>) -> Option<&str> {
    let uri = request.uri();
    let host = uri.host()?;
    if request.method() != http::Method::GET
        || uri.query().is_some()
        || uri.port_u16().is_some()
        || !matches!(
            (uri.scheme_str()?, host),
            ("http", "openleash-viz.localhost")
                | ("https", "openleash-viz.localhost")
                | ("openleash-viz", "localhost")
        )
    {
        return None;
    }
    uri.path().strip_prefix('/')
}
fn request_id(request: &http::Request<Vec<u8>>) -> Option<&str> {
    let id = request_path(request)?;
    if uuid::Uuid::parse_str(id).ok()?.to_string() != id {
        return None;
    }
    Some(id)
}
/// The host-owned library a request names, if any. Exact match only.
fn request_library(request: &http::Request<Vec<u8>>) -> Option<&'static [u8]> {
    (request_path(request)? == LIBRARY_PATH).then(|| ECHARTS.as_slice())
}
/// Serve the bundled chart library. Public, static, non-secret bytes, so remote
/// access serves it before pairing: a script request from an opaque-origin
/// frame is cross-site and never carries the SameSite=Strict session cookie.
pub fn library_response(request: &http::Request<Vec<u8>>) -> Option<http::Response<Vec<u8>>> {
    let body = request_library(request)?;
    Some(
        http::Response::builder()
            .status(200)
            .header("Content-Type", "text/javascript; charset=utf-8")
            .header("Cache-Control", "max-age=3600")
            .header("X-Content-Type-Options", "nosniff")
            .header("Cross-Origin-Resource-Policy", "cross-origin")
            .body(body.to_vec())
            .expect("fixed response headers"),
    )
}
pub fn respond(app: &tauri::AppHandle, request: http::Request<Vec<u8>>) -> http::Response<Vec<u8>> {
    if let Some(response) = library_response(&request) {
        return response;
    }
    let source =
        request_id(&request).and_then(|id| app.state::<Registry>().read(id, Instant::now()));
    http::Response::builder()
        .status(if source.is_some() { 200 } else { 404 })
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", csp(LIBRARY_SOURCES))
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer")
        .header("Permissions-Policy", "camera=(), microphone=(), geolocation=(), clipboard-read=(), clipboard-write=(), fullscreen=(), payment=(), usb=()")
        .body(source.unwrap_or_else(|| "Preview expired. Stop and preview again.".into()).into_bytes()).expect("fixed response headers")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_ephemeral_registry() {
        let r = Registry::default();
        let now = Instant::now();
        assert!(r.publish(String::new(), now).is_err());
        assert!(r.publish("x".repeat(MAX_SOURCE + 1), now).is_err());
        let id = r.publish("<script>1</script>".into(), now).unwrap();
        assert_eq!(r.read(&id, now).as_deref(), Some("<script>1</script>"));
        assert!(r.read(&id, now + TTL).is_none());
        let id = r.publish("a".into(), now).unwrap();
        r.release(&id);
        assert!(r.read(&id, now).is_none());
        for _ in 0..MAX_DOCUMENTS {
            r.publish("a".into(), now).unwrap();
        }
        assert!(r.publish("a".into(), now).is_err());
    }
    #[test]
    fn total_memory_cap() {
        let r = Registry::default();
        let now = Instant::now();
        for _ in 0..MAX_TOTAL / MAX_SOURCE {
            r.publish("x".repeat(MAX_SOURCE), now).unwrap();
        }
        assert!(r.publish("x".into(), now).is_err());
    }
    #[test]
    fn strict_requests_and_caller_origins() {
        let id = uuid::Uuid::new_v4().to_string();
        for base in [
            "http://openleash-viz.localhost",
            "openleash-viz://localhost",
        ] {
            let r = http::Request::builder()
                .uri(format!("{base}/{id}"))
                .body(vec![])
                .unwrap();
            assert_eq!(request_id(&r), Some(id.as_str()));
        }
        for uri in [
            format!("http://openleash-viz.evil/{id}"),
            format!("http://openleash-viz.localhost/{id}?x"),
            "http://openleash-viz.localhost/../../file".into(),
        ] {
            let r = http::Request::builder().uri(uri).body(vec![]).unwrap();
            assert!(request_id(&r).is_none());
        }
        assert!(!trusted_url(
            &tauri::Url::parse("http://openleash-viz.localhost/").unwrap()
        ));
        assert!(!trusted_url(
            &tauri::Url::parse("https://evil.example/").unwrap()
        ));
        assert!(trusted_url(
            &tauri::Url::parse("tauri://localhost/").unwrap()
        ));
    }
    #[test]
    fn chart_library_is_served_only_at_its_exact_path() {
        for base in [
            "http://openleash-viz.localhost",
            "https://openleash-viz.localhost",
            "openleash-viz://localhost",
        ] {
            let r = http::Request::builder()
                .uri(format!("{base}/{LIBRARY_PATH}"))
                .body(vec![])
                .unwrap();
            let res = library_response(&r).expect("library served");
            assert_eq!(res.status(), 200);
            assert!(res.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/javascript"));
            let body = String::from_utf8_lossy(res.body());
            assert!(body.contains("echarts"));
            assert!(body.contains("registerTheme(\"openleash\""));
            // The library is not a document id, so it never reads the registry.
            assert!(request_id(&r).is_none());
        }
        for uri in [
            format!("http://openleash-viz.evil/{LIBRARY_PATH}"),
            format!("http://openleash-viz.localhost/{LIBRARY_PATH}?x"),
            format!("http://openleash-viz.localhost:8080/{LIBRARY_PATH}"),
            "http://openleash-viz.localhost/lib/echarts.min.js".into(),
            "http://openleash-viz.localhost/lib/".into(),
            "http://openleash-viz.localhost/lib/../lib/echarts.js".into(),
        ] {
            let r = http::Request::builder().uri(uri).body(vec![]).unwrap();
            assert!(library_response(&r).is_none());
        }
        let post = http::Request::builder()
            .method("POST")
            .uri(format!("http://openleash-viz.localhost/{LIBRARY_PATH}"))
            .body(vec![])
            .unwrap();
        assert!(library_response(&post).is_none());
    }
    #[test]
    fn document_policy_is_isolated() {
        let csp = csp(LIBRARY_SOURCES);
        // Scripts beyond inline ones come only from the host library directory.
        assert!(csp.contains(&format!("script-src 'unsafe-inline' {LIBRARY_SOURCES};")));
        for source in LIBRARY_SOURCES.split(' ') {
            assert!(source.ends_with("/lib/"), "{source}");
        }
        let policy = csp.as_str();
        for directive in [
            "connect-src 'none'",
            "frame-src 'none'",
            "worker-src 'none'",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "sandbox allow-scripts",
        ] {
            assert!(policy.contains(directive));
        }
        assert!(!policy.contains("allow-same-origin"));
        // No scheme-wide or wildcard source: only the named library directory.
        for token in policy.split([' ', ';']) {
            assert!(!matches!(token, "https:" | "http:" | "*"), "{token}");
        }
        assert!(!policy.contains("script-src 'unsafe-inline' data:"));
        assert!(!policy.contains("'unsafe-eval'"));
    }
    #[test]
    fn remote_library_sources_join_the_policy() {
        let remote = csp("http://box:7777/__openleash/viz/lib/");
        assert!(remote.contains("script-src 'unsafe-inline' http://box:7777/__openleash/viz/lib/;"));
    }
}
