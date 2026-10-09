//! Ephemeral, independently served interactive documents. Never writes model HTML to disk.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{http, Manager, State, Webview};

pub const MAX_SOURCE: usize = 256 * 1024;
const MAX_DOCUMENTS: usize = 64;
const MAX_TOTAL: usize = 8 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(30 * 60);
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; sandbox allow-scripts";

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
fn request_id(request: &http::Request<Vec<u8>>) -> Option<&str> {
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
    let id = uri.path().strip_prefix('/')?;
    if uuid::Uuid::parse_str(id).ok()?.to_string() != id {
        return None;
    }
    Some(id)
}
pub fn respond(app: &tauri::AppHandle, request: http::Request<Vec<u8>>) -> http::Response<Vec<u8>> {
    let source =
        request_id(&request).and_then(|id| app.state::<Registry>().read(id, Instant::now()));
    http::Response::builder()
        .status(if source.is_some() { 200 } else { 404 })
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", CSP)
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
    fn document_policy_is_isolated() {
        for directive in [
            "connect-src 'none'",
            "frame-src 'none'",
            "worker-src 'none'",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "sandbox allow-scripts",
        ] {
            assert!(CSP.contains(directive));
        }
        assert!(!CSP.contains("allow-same-origin"));
        assert!(!CSP.contains("https:"));
    }
}
