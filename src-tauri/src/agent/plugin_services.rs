//! The hub for the "service" plugins: Linear, Jira, Slack, Notion and GitLab.
//!
//! Each one is a single tool with an `action` enum, lives in its own module
//! (`plugin_linear.rs`, `plugin_jira.rs`, `plugin_slack.rs`, `plugin_notion.rs`,
//! `plugin_gitlab.rs`) and is wired into the rest of the harness only through
//! this file, so the shared code paths — permissions, dispatch, settings
//! redaction, the Settings card — treat all five identically:
//!
//! - `schemas` adds a tool per *enabled* plugin to the frozen prefix (they ship
//!   off, like every plugin; see `plugins.rs`).
//! - `is_write` is the classification the permission gate uses. It is pure on the
//!   tool input and fails closed: an action nobody listed as a read is a write.
//! - `sub_may_call` is the read-only-subagent refusal, kept here so it is
//!   testable without a running harness.
//! - `run` / `whoami` do the network work; tokens are resolved from settings,
//!   then from the environment, at call time.
//! - `redact_secrets` / `restore_secrets` / `set_secret` keep tokens out of the
//!   webview: the UI never sees a token, it only learns *whether* one is saved.
//!
//! Modelled on the GitHub plugin in `plugins.rs`, which predates this hub and
//! keeps its own wiring.

use super::plugins::PluginsCfg;
use super::{plugin_gitlab, plugin_jira, plugin_linear, plugin_notion, plugin_slack};
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

/// Tool names, in the order their schemas are served (and the UI shows them).
pub const SERVICES: [&str; 5] = ["linear", "jira", "slack", "notion", "gitlab"];

pub fn is_service(name: &str) -> bool {
    SERVICES.contains(&name)
}

/// The product name, for approval titles, step lines and errors.
pub fn label(name: &str) -> &'static str {
    match name {
        "linear" => "Linear",
        "jira" => "Jira",
        "slack" => "Slack",
        "notion" => "Notion",
        "gitlab" => "GitLab",
        _ => "Plugin",
    }
}

pub fn enabled(cfg: &PluginsCfg, name: &str) -> bool {
    match name {
        "linear" => cfg.linear.enabled,
        "jira" => cfg.jira.enabled,
        "slack" => cfg.slack.enabled,
        "notion" => cfg.notion.enabled,
        "gitlab" => cfg.gitlab.enabled,
        _ => false,
    }
}

/// Schemas for every enabled service plugin, in `SERVICES` order.
pub fn schemas(cfg: &PluginsCfg) -> Vec<Value> {
    SERVICES
        .iter()
        .filter(|n| enabled(cfg, n))
        .map(|n| match *n {
            "linear" => plugin_linear::schema(),
            "jira" => plugin_jira::schema(),
            "slack" => plugin_slack::schema(),
            "notion" => plugin_notion::schema(),
            _ => plugin_gitlab::schema(),
        })
        .collect()
}

/// Whether a call changes something on the service (so it needs approval, is
/// refused in plan mode and in read-only subagents). Unknown tools and unknown
/// actions are writes: the safe direction for anything nobody classified.
pub fn is_write(name: &str, input: &Value) -> bool {
    match name {
        "linear" => plugin_linear::is_write(input),
        "jira" => plugin_jira::is_write(input),
        "slack" => plugin_slack::is_write(input),
        "notion" => plugin_notion::is_write(input),
        "gitlab" => plugin_gitlab::is_write(input),
        _ => true,
    }
}

/// The read-only-subagent rule: such an agent may look things up on a service
/// but never change anything there. `policy` is the subagent's tool policy
/// (`None` for the main agent).
pub fn sub_may_call(policy: Option<&str>, name: &str, input: &Value) -> Result<(), String> {
    if policy == Some("read_only") && is_write(name, input) {
        return Err(format!(
            "This agent is read-only: it can look things up on {} but not change them.",
            label(name)
        ));
    }
    Ok(())
}

/// Run one call. The caller has already checked `enabled`, the subagent policy
/// and the permission gate. `cwd` is the agent's working directory (GitLab uses
/// its `origin` remote when no project is named).
pub async fn run(
    http: &reqwest::Client,
    cfg: &PluginsCfg,
    name: &str,
    input: &Value,
    cwd: &str,
) -> Result<String, String> {
    match name {
        "linear" => plugin_linear::run(http, &cfg.linear, input).await,
        "jira" => plugin_jira::run(http, &cfg.jira, input).await,
        "slack" => plugin_slack::run(http, &cfg.slack, input).await,
        "notion" => plugin_notion::run(http, &cfg.notion, input).await,
        "gitlab" => plugin_gitlab::run(http, &cfg.gitlab, input, cwd).await,
        other => Err(format!("Unknown plugin `{other}`.")),
    }
}

/// "Test connection" for Settings: who the credentials authenticate as, and
/// where they came from (`settings` or the env var name).
/// `{ok, identity?, source?, error?}` — never the token itself.
pub async fn test_connection(http: &reqwest::Client, cfg: &PluginsCfg, name: &str) -> Value {
    let r = match name {
        "linear" => plugin_linear::whoami(http, &cfg.linear).await,
        "jira" => plugin_jira::whoami(http, &cfg.jira).await,
        "slack" => plugin_slack::whoami(http, &cfg.slack).await,
        "notion" => plugin_notion::whoami(http, &cfg.notion).await,
        "gitlab" => plugin_gitlab::whoami(http, &cfg.gitlab).await,
        other => Err(format!("Unknown plugin `{other}`.")),
    };
    match r {
        Ok(w) => json!({"ok": true, "identity": w.identity, "source": w.source}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

/// What `whoami` reports: the identity (e.g. `Ada Lovelace <ada@x.com> · Acme`)
/// and where the credentials came from (`settings`, `LINEAR_API_KEY`, …).
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub identity: String,
    pub source: String,
}

// ───────────────────────────── secrets ─────────────────────────────

/// `(plugin, field)` pairs that hold credentials. Everything listed here is
/// blanked before settings reach the webview and restored on a settings save.
pub const SECRET_FIELDS: [(&str, &str); 6] = [
    ("linear", "token"),
    ("jira", "token"),
    ("slack", "token"),
    ("slack", "user_token"),
    ("notion", "token"),
    ("gitlab", "token"),
];

fn secret_mut<'a>(cfg: &'a mut PluginsCfg, plugin: &str, field: &str) -> Option<&'a mut String> {
    Some(match (plugin, field) {
        ("linear", "token") => &mut cfg.linear.token,
        ("jira", "token") => &mut cfg.jira.token,
        ("slack", "token") => &mut cfg.slack.token,
        ("slack", "user_token") => &mut cfg.slack.user_token,
        ("notion", "token") => &mut cfg.notion.token,
        ("gitlab", "token") => &mut cfg.gitlab.token,
        _ => return None,
    })
}

fn secret_ref<'a>(cfg: &'a PluginsCfg, plugin: &str, field: &str) -> Option<&'a String> {
    Some(match (plugin, field) {
        ("linear", "token") => &cfg.linear.token,
        ("jira", "token") => &cfg.jira.token,
        ("slack", "token") => &cfg.slack.token,
        ("slack", "user_token") => &cfg.slack.user_token,
        ("notion", "token") => &cfg.notion.token,
        ("gitlab", "token") => &cfg.gitlab.token,
        _ => return None,
    })
}

/// Blank every service token, for the copy of settings the UI sees.
pub fn redact_secrets(cfg: &mut PluginsCfg) {
    for (p, f) in SECRET_FIELDS {
        if let Some(s) = secret_mut(cfg, p, f) {
            s.clear();
        }
    }
}

/// Put the stored tokens back into settings coming from the UI. The UI never
/// had them, so whatever it sent in those fields means "unchanged"; tokens are
/// only ever changed through `set_secret`.
pub fn restore_secrets(next: &mut PluginsCfg, stored: &PluginsCfg) {
    for (p, f) in SECRET_FIELDS {
        if let (Some(dst), Some(src)) = (secret_mut(next, p, f), secret_ref(stored, p, f)) {
            dst.clone_from(src);
        }
    }
}

/// Save (or, with an empty value, remove) one token.
pub fn set_secret(
    cfg: &mut PluginsCfg,
    plugin: &str,
    field: &str,
    value: &str,
) -> Result<(), String> {
    let slot = secret_mut(cfg, plugin, field)
        .ok_or_else(|| format!("`{plugin}.{field}` is not a credential field."))?;
    *slot = value.trim().to_string();
    Ok(())
}

/// `{linear: {token: true}, slack: {token: false, user_token: true}, …}`:
/// whether each credential is saved in settings, for "Saved · paste to replace".
pub fn secret_flags(cfg: &PluginsCfg) -> Value {
    let mut out = json!({});
    for (p, f) in SECRET_FIELDS {
        let set = secret_ref(cfg, p, f).is_some_and(|s| !s.trim().is_empty());
        out[p][f] = json!(set);
    }
    out
}

// ───────────────────────────── shared helpers ─────────────────────────────

/// The value from settings if set, else the first non-empty env var, with
/// where it came from. `env` is injected so tests never touch the process
/// environment (tests run in parallel and share it).
pub fn setting_or_env(
    setting: &str,
    keys: &[&'static str],
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, &'static str)> {
    if !setting.trim().is_empty() {
        return Some((setting.trim().to_string(), "settings"));
    }
    keys.iter().find_map(|k| {
        env(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(|v| (v, *k))
    })
}

/// The real process environment, for `setting_or_env` outside tests.
pub fn real_env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// Whether a raw `api` call's method is a read. Only an explicit `GET` (or no
/// method, which the schemas document as GET) is; anything else — including a
/// method nobody recognises or a non-string — is a write.
pub fn method_is_read(method: &Value) -> bool {
    match method {
        Value::Null => true,
        Value::String(m) => m.trim().eq_ignore_ascii_case("GET"),
        _ => false,
    }
}

/// Whether a raw `api` path is a plain relative path that can only be joined
/// onto the configured base — no scheme, no `//host`, no backslash (which URL
/// parsers treat as a slash), no whitespace or control characters. An absolute
/// URL can be pinned by `pin_to_base`, but classification stays conservative:
/// anything that is not obviously relative is a write.
pub fn raw_path_is_relative(path: &str) -> bool {
    let p = path.trim();
    let rel = p.trim_start_matches('/').to_ascii_lowercase();
    !p.is_empty()
        && !p.contains("://")
        && !p.starts_with("//")
        && !p.contains('\\')
        && !path.chars().any(char::is_control)
        && !p.chars().any(char::is_whitespace)
        && !rel.starts_with("%2f")
        && !rel.starts_with("%5c")
}

/// Join a raw API path onto `base` (e.g. `https://gitlab.example.com/api/v4`),
/// refusing anything that would leave its scheme, host and port. Every request
/// carries the user's token, so a path that escapes the host hands that token
/// to whoever runs the other host.
///
/// Accepted: `/projects`, `projects?per_page=5`, and an absolute URL on the
/// very same origin. Refused: `//evil.com/x`, `https://evil.com`,
/// `https://base.host@evil.com`, `https://base.host.evil.com`, backslashes,
/// whitespace/control characters, credentials in the URL.
pub fn pin_to_base(base: &str, path: &str) -> Result<reqwest::Url, String> {
    let base_url = reqwest::Url::parse(base.trim())
        .map_err(|_| format!("The configured base URL `{base}` is not a valid URL."))?;
    if !matches!(base_url.scheme(), "https" | "http") || base_url.host_str().is_none() {
        return Err(format!(
            "The configured base URL `{base}` must be an http(s) URL with a host."
        ));
    }
    let p = path.trim();
    if p.is_empty() {
        return Err("`path` is required.".into());
    }
    if p.contains('\\') || path.chars().any(char::is_control) || p.chars().any(char::is_whitespace)
    {
        return Err(format!(
            "Refusing path `{p}`: it contains a backslash, whitespace or a control character."
        ));
    }
    let joined = if p.contains("://") || p.starts_with("//") {
        // An absolute URL: allowed only on the base's own origin. Parsed and
        // compared rather than string-matched, so `base@evil` and
        // `base.evil.com` cannot pass as the base.
        reqwest::Url::parse(p).map_err(|_| format!("Refusing `{p}`: not a valid URL."))?
    } else {
        let rel = p.trim_start_matches('/');
        let low = rel.to_ascii_lowercase();
        if low.starts_with("%2f") || low.starts_with("%5c") {
            return Err(format!("Refusing path `{p}`: encoded leading slash."));
        }
        let b = base_url.as_str().trim_end_matches('/');
        reqwest::Url::parse(&format!("{b}/{rel}"))
            .map_err(|_| format!("Refusing path `{p}`: it does not form a valid URL."))?
    };
    let same_origin = joined.scheme() == base_url.scheme()
        && joined.host_str().map(str::to_ascii_lowercase)
            == base_url.host_str().map(str::to_ascii_lowercase)
        && joined.port_or_known_default() == base_url.port_or_known_default()
        && joined.username().is_empty()
        && joined.password().is_none();
    if !same_origin {
        return Err(format!(
            "Refusing `{p}`: raw API calls must stay on {} (the configured host), because every request carries your token.",
            base_url.origin().ascii_serialization()
        ));
    }
    Ok(joined)
}

/// Replace every secret in `text` with `[redacted]`. Used on every error that
/// might echo a request back (a URL, a header dump, a server's own message).
pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for s in secrets {
        let s = s.trim();
        // A very short "secret" would shred ordinary text; real tokens are long.
        if s.len() >= 6 {
            out = out.replace(s, "[redacted]");
        }
    }
    out
}

/// Same clip as the GitHub plugin's: char-safe, with a visible marker.
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "\n[… truncated]"
}

/// The last `lines` lines of a log.
pub fn tail(s: &str, lines: usize) -> String {
    let v: Vec<&str> = s.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

/// Default request timeout for every service call.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// A response read to the end.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// A separate transport for credential-bearing service requests. The desktop's
/// shared client follows redirects by default; reqwest strips `Authorization`
/// in some cross-host cases but not `PRIVATE-TOKEN`, and 307/308 can forward a
/// request body. These APIs carry credentials in headers, so redirects are
/// rejected for every service request rather than trying to enumerate which
/// headers/body fields a server might echo or forwarding. We build from default
/// client settings so normal proxy and TLS behavior remains available.
fn credential_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("Couldn't create a secure service API client: {e}"))
    }) {
        Ok(client) => Ok(client),
        Err(e) => Err(e.clone()),
    }
}

/// Sanitize a response before any plugin can format, clip or return its fields.
/// Replacing raw text alone misses JSON escapes (e.g. `\\u002d`) and string
/// values that are decoded before a whoami formatter sees them.
fn redact_response(body: &str, secrets: &[&str]) -> String {
    fn value(v: Value, secrets: &[&str], changed: &mut bool) -> Value {
        match v {
            Value::String(s) => {
                let redacted = redact(&s, secrets);
                *changed |= redacted != s;
                Value::String(redacted)
            }
            Value::Array(a) => {
                Value::Array(a.into_iter().map(|x| value(x, secrets, changed)).collect())
            }
            Value::Object(o) => {
                let mut next = Map::new();
                for (k, v) in o {
                    let key = redact(&k, secrets);
                    *changed |= key != k;
                    next.insert(key, value(v, secrets, changed));
                }
                Value::Object(next)
            }
            other => other,
        }
    }
    let Some(parsed) = serde_json::from_str::<Value>(body).ok() else {
        return redact(body, secrets);
    };
    let mut changed = false;
    let safe = value(parsed, secrets, &mut changed);
    if changed {
        safe.to_string()
    } else {
        redact(body, secrets)
    }
}

/// Send a request with the shared timeout and turn transport failures and
/// HTTP 429 into clear, token-free errors. Other statuses come back as a
/// `Reply` for the plugin to interpret (see `http_error`). The request is
/// rebuilt onto a no-redirect client: callers may construct it from `h.http`,
/// but credentials/body are never handed to a redirect target.
pub async fn send(
    service: &str,
    rq: reqwest::RequestBuilder,
    secrets: &[&str],
) -> Result<Reply, String> {
    let request = rq
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| redact(&format!("{service} request failed: {e}"), secrets))?;
    let resp = credential_client()?
        .execute(request)
        .await
        .map_err(|e| redact(&format!("{service} request failed: {e}"), secrets))?;
    let status = resp.status().as_u16();
    let retry = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let body = resp
        .text()
        .await
        .map_err(|e| redact(&format!("{service} read failed: {e}"), secrets))?;
    if (300..400).contains(&status) {
        // Never follow the Location: destination: every request carries a
        // credential, and some providers use 307/308 which preserve the body.
        // In particular reqwest's redirect stripping does not cover custom
        // headers such as GitLab's `PRIVATE-TOKEN`.
        return Err(redact(
            &format!("{service} returned HTTP {status} redirect; the credential-bearing request was not followed."),
            secrets,
        ));
    }
    let body = redact_response(&body, secrets);
    if status == 429 {
        return Err(redact(&rate_limited(service, retry), secrets));
    }
    Ok(Reply { status, body })
}

/// The message for an HTTP 429.
pub fn rate_limited(service: &str, retry_after: Option<u64>) -> String {
    match retry_after {
        Some(s) => format!("{service} rate limit hit (HTTP 429). It asks to retry after {s}s — wait before calling again, and batch or narrow the requests."),
        None => format!("{service} rate limit hit (HTTP 429). Wait a minute before calling again, and batch or narrow the requests."),
    }
}

/// A uniform error for a non-2xx reply: `Jira 404 on GET /rest/api/3/issue/X:
/// <server message>`, clipped and with secrets removed. `hint` is appended
/// (e.g. where to fix a token on 401).
pub fn http_error(
    service: &str,
    status: u16,
    what: &str,
    server_msg: &str,
    hint: &str,
    secrets: &[&str],
) -> String {
    let safe = redact(server_msg.trim(), secrets);
    redact(
        &format!(
            "{service} {status} on {what}: {}{}{hint}",
            clip(&safe, 600),
            if hint.is_empty() { "" } else { " " }
        ),
        secrets,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A redirect response must never forward a credential-bearing request,
    /// including non-standard auth headers reqwest does not strip on redirects.
    /// Both endpoints are local fixtures; no provider or real token is used.
    #[tokio::test]
    async fn service_redirects_never_forward_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        for (service, header, token) in [
            ("GitLab", "PRIVATE-TOKEN", "glpat-dummy-secret"),
            ("Linear", "Authorization", "lin_api_dummy_secret"),
            ("Jira", "Authorization", "Basic ZHVtbXk6c2VjcmV0"),
            ("Notion", "Authorization", "Bearer ntn_dummy_secret"),
            ("Slack", "Authorization", "Bearer xoxb-dummy-secret"),
        ] {
            for status in [302, 307, 308] {
                let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let target = destination.local_addr().unwrap();
                let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let source = origin.local_addr().unwrap();
                let source_task = tokio::spawn(async move {
                    let (mut stream, _) = origin.accept().await.unwrap();
                    let mut request = [0; 8192];
                    let n = stream.read(&mut request).await.unwrap();
                    let request = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
                    assert!(request.contains(&header.to_ascii_lowercase()));
                    assert!(request.contains(&token.to_ascii_lowercase()));
                    let response = format!("HTTP/1.1 {status} Redirect\r\nLocation: http://{target}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    stream.write_all(response.as_bytes()).await.unwrap();
                });
                let destination_task = tokio::spawn(async move {
                    let received = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        destination.accept(),
                    )
                    .await;
                    if let Ok(Ok((mut stream, _))) = received {
                        let mut request = [0; 8192];
                        let n = stream.read(&mut request).await.unwrap();
                        let stolen = String::from_utf8_lossy(&request[..n]).contains(token);
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                        return Some(stolen);
                    }
                    None
                });
                // Intentionally supply an unsafe client, just as h.http is today.
                // The shared service transport, not every caller, owns this boundary.
                let http = reqwest::Client::builder().no_proxy().build().unwrap();
                let result = send(
                    service,
                    http.post(format!("http://{source}/api"))
                        .header(header, token)
                        .body("private-issue-description"),
                    &[token],
                )
                .await;
                source_task.await.unwrap();
                let forwarded = destination_task.await.unwrap();
                assert!(
                    forwarded.is_none(),
                    "{service}: redirect target was requested (credential present: {forwarded:?})"
                );
                let error = result.expect_err("redirects must be surfaced, not followed");
                assert!(
                    error.contains(&status.to_string()) && error.contains("redirect"),
                    "{error}"
                );
                assert!(!error.contains(token));
            }
        }
    }

    async fn local_reply(status: &str, header: &str, body: &str) -> Result<Reply, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\n{header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let result = send(
            "GitLab",
            reqwest::Client::new().get(format!("http://{addr}/")),
            &["dummy-token-must-never-leak"],
        )
        .await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn successful_responses_and_identities_redact_echoed_credentials() {
        let token = "dummy-token-must-never-leak";
        // Both plain text and JSON (including escaped JSON values and keys)
        // must be safe BEFORE provider formatting, truncation and whoami IPC.
        for body in [
            format!("hello {token}"),
            format!(r#"{{"name":"{token}","message":"hello"}}"#),
            r#"{"name":"dummy\u002dtoken-must-never-leak","dummy\u002dtoken-must-never-leak":"hello"}"#.to_string(),
        ] {
            let reply = local_reply("200 OK", "", &body).await.unwrap();
            let parsed = serde_json::from_str::<Value>(&reply.body).ok();
            assert!(!reply.body.contains(token), "response echoed a credential");
            assert!(!parsed.as_ref().is_some_and(|v| v.to_string().contains(token)), "decoded JSON echoed a credential");
            assert!(reply.body.contains("[redacted]"));
            assert!(reply.body.contains("hello"));
        }
    }

    #[tokio::test]
    async fn retry_after_cannot_echo_credentials_or_arbitrary_text() {
        for retry in [
            "dummy-token-must-never-leak",
            "arbitrary-untrusted-text",
            "12",
        ] {
            let error = local_reply(
                "429 Too Many Requests",
                &format!("Retry-After: {retry}\r\n"),
                "",
            )
            .await
            .unwrap_err();
            assert!(
                !error.contains("dummy-token") && !error.contains("arbitrary-untrusted-text"),
                "{error}"
            );
            assert!(error.contains("429"));
            if retry == "12" {
                assert!(error.contains("12s"));
            }
        }
    }

    #[test]
    fn errors_redact_credentials_before_clipping() {
        let token = "dummy-token-must-never-leak";
        let message = "x".repeat(590) + token;
        let error = http_error("Jira", 400, "request", &message, "", &[token]);
        assert!(
            !error.contains("dummy-toke"),
            "partial credential leaked: {error}"
        );
    }

    #[test]
    fn only_get_or_no_method_is_a_read() {
        assert!(method_is_read(&Value::Null));
        assert!(method_is_read(&json!("GET")));
        assert!(method_is_read(&json!(" get ")));
        for m in [
            json!("POST"),
            json!("PUT"),
            json!("PATCH"),
            json!("DELETE"),
            json!("HEAD"),
            json!("FETCH"),
            json!(""),
            json!(1),
            json!(["GET"]),
            json!({"m": "GET"}),
        ] {
            assert!(!method_is_read(&m), "{m} must be a write");
        }
    }

    #[test]
    fn relative_paths_only() {
        for ok in ["/projects", "projects?x=1", "/rest/api/3/myself", "/a/@b"] {
            assert!(raw_path_is_relative(ok), "{ok}");
        }
        for bad in [
            "",
            "https://evil.com/x",
            "//evil.com/x",
            "http://gitlab.com/api",
            "\\\\evil.com\\x",
            "/\\evil.com",
            "/x y",
            "/x\n",
            "%2F%2Fevil.com",
        ] {
            assert!(!raw_path_is_relative(bad), "{bad:?}");
        }
    }

    #[test]
    fn pinning_keeps_requests_on_the_base_host() {
        let base = "https://gitlab.example.com/api/v4";
        assert_eq!(
            pin_to_base(base, "/projects?per_page=5").unwrap().as_str(),
            "https://gitlab.example.com/api/v4/projects?per_page=5"
        );
        assert_eq!(
            pin_to_base(base, "projects").unwrap().as_str(),
            "https://gitlab.example.com/api/v4/projects"
        );
        // A path containing `@` stays a path.
        assert_eq!(
            pin_to_base(base, "/@evil.com").unwrap().host_str(),
            Some("gitlab.example.com")
        );
        assert!(pin_to_base(base, "https://gitlab.example.com/api/v4/user").is_ok());
        assert!(pin_to_base(base, "https://GITLAB.example.com:443/api/v4/user").is_ok());
        for bad in [
            "//evil.com/x",
            "https://evil.com/api/v4/user",
            "http://gitlab.example.com/api/v4/user",
            "https://gitlab.example.com@evil.com/x",
            "https://user:pw@gitlab.example.com/x",
            "https://gitlab.example.com.evil.com/x",
            "https://gitlab.example.com:8443/x",
            "/\\evil.com",
            "\\\\evil.com",
            "%2F%2Fevil.com/x",
            "/%2f%2fevil.com",
            "/x\r\nHost: evil.com",
            "",
        ] {
            assert!(pin_to_base(base, bad).is_err(), "{bad:?} must be refused");
        }
        assert!(pin_to_base("not a url", "/x").is_err());
        assert!(pin_to_base("file:///etc", "/x").is_err());
    }

    #[test]
    fn secrets_never_survive_redaction() {
        let s = redact(
            "failed for https://x/?t=xoxb-123456 with xoxb-123456",
            &["xoxb-123456", ""],
        );
        assert!(!s.contains("xoxb-123456"), "{s}");
        assert_eq!(
            redact("abc", &["ab"]),
            "abc",
            "too-short secrets are ignored"
        );
    }

    #[test]
    fn settings_beat_env_and_env_order_is_respected() {
        let env = |k: &str| match k {
            "A" => Some("  ".to_string()),
            "B" => Some("from-b".to_string()),
            _ => None,
        };
        assert_eq!(
            setting_or_env(" mine ", &["A", "B"], &env),
            Some(("mine".to_string(), "settings"))
        );
        assert_eq!(
            setting_or_env("", &["A", "B"], &env),
            Some(("from-b".to_string(), "B"))
        );
        assert_eq!(setting_or_env("", &["C"], &env), None);
    }

    #[test]
    fn tokens_are_redacted_for_the_ui_and_restored_on_save() {
        let mut stored = PluginsCfg::default();
        for (p, f) in SECRET_FIELDS {
            set_secret(&mut stored, p, f, &format!("secret-{p}-{f}")).unwrap();
        }
        let mut view = stored.clone();
        redact_secrets(&mut view);
        let shown = serde_json::to_string(&view).unwrap();
        assert!(!shown.contains("secret-"), "{shown}");
        let flags = secret_flags(&stored);
        assert_eq!(flags["slack"]["user_token"], true);
        assert_eq!(secret_flags(&view)["linear"]["token"], false);
        // The UI sends the redacted copy back (e.g. after toggling a switch).
        let mut next = view.clone();
        next.jira.enabled = true;
        restore_secrets(&mut next, &stored);
        assert!(next.jira.enabled);
        for (p, f) in SECRET_FIELDS {
            assert_eq!(secret_ref(&next, p, f).unwrap(), &format!("secret-{p}-{f}"));
        }
        // Even a UI that tried to write a token through settings cannot.
        let mut sneaky = view.clone();
        sneaky.linear.token = "injected".into();
        restore_secrets(&mut sneaky, &stored);
        assert_eq!(sneaky.linear.token, "secret-linear-token");
        assert!(set_secret(&mut stored, "linear", "email", "x").is_err());
        assert!(set_secret(&mut stored, "github", "token", "x").is_err());
        set_secret(&mut stored, "notion", "token", "  ").unwrap();
        assert!(stored.notion.token.is_empty());
    }

    #[test]
    fn debug_output_never_prints_a_token() {
        let mut c = PluginsCfg::default();
        for (p, f) in SECRET_FIELDS {
            set_secret(&mut c, p, f, &format!("secret-{p}-{f}")).unwrap();
        }
        let dbg = format!(
            "{:?}{:?}{:?}{:?}{:?}",
            c.linear, c.jira, c.slack, c.notion, c.gitlab
        );
        assert!(!dbg.contains("secret-"), "{dbg}");
    }

    #[test]
    fn every_service_ships_off_and_old_settings_still_load() {
        let d = PluginsCfg::default();
        for n in SERVICES {
            assert!(!enabled(&d, n), "{n} must ship off");
        }
        assert!(schemas(&d).is_empty());
        // A settings.json written before these plugins existed.
        let old: PluginsCfg = serde_json::from_str(
            r#"{"github":{"enabled":true,"token":"ghp_x"},"computer":{},"browser":{"enabled":false}}"#,
        )
        .unwrap();
        for n in SERVICES {
            assert!(!enabled(&old, n));
        }
        assert!(old.github.enabled);
        // Partial blocks fill in from Default.
        let partial: PluginsCfg = serde_json::from_str(
            r#"{"gitlab":{"enabled":true},"jira":{"base_url":"https://x.atlassian.net"}}"#,
        )
        .unwrap();
        assert!(partial.gitlab.enabled && partial.gitlab.token.is_empty());
        assert!(!partial.jira.enabled);
    }

    #[test]
    fn schemas_follow_toggles_and_names_match() {
        let mut c = PluginsCfg::default();
        c.linear.enabled = true;
        c.jira.enabled = true;
        c.slack.enabled = true;
        c.notion.enabled = true;
        c.gitlab.enabled = true;
        let names: Vec<String> = schemas(&c)
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, SERVICES);
        for s in schemas(&c) {
            let enum_ = s["input_schema"]["properties"]["action"]["enum"]
                .as_array()
                .expect("every service tool has an action enum");
            assert!(!enum_.is_empty());
            assert_eq!(s["input_schema"]["required"], json!(["action"]));
        }
    }

    /// Every action in a schema's enum is classified, and the unknown ones are
    /// writes — the fail-closed direction.
    #[test]
    fn unknown_actions_and_tools_are_writes() {
        for n in SERVICES {
            assert!(
                is_write(n, &json!({"action": "definitely_not_an_action"})),
                "{n}"
            );
            assert!(is_write(n, &json!({})), "{n}: a missing action is a write");
        }
        assert!(is_write("not_a_service", &json!({"action": "whoami"})));
        for n in SERVICES {
            assert!(
                !is_write(n, &json!({"action": "whoami"})),
                "{n}: whoami is a read"
            );
        }
    }

    #[test]
    fn a_read_only_sub_can_read_but_not_write() {
        for n in SERVICES {
            assert!(sub_may_call(Some("read_only"), n, &json!({"action": "whoami"})).is_ok());
            let e = sub_may_call(Some("read_only"), n, &json!({"action": "nope"})).unwrap_err();
            assert!(e.contains(label(n)), "{e}");
            assert!(sub_may_call(Some("all"), n, &json!({"action": "nope"})).is_ok());
            assert!(sub_may_call(None, n, &json!({"action": "nope"})).is_ok());
        }
    }

    #[test]
    fn rate_limit_and_http_errors_are_clear_and_token_free() {
        assert!(rate_limited("Slack", Some(30)).contains("30s"));
        assert!(rate_limited("Slack", None).contains("429"));
        let e = http_error(
            "GitLab",
            401,
            "GET /user",
            "bad token glpat-abcdef",
            "Check Settings.",
            &["glpat-abcdef"],
        );
        assert!(e.starts_with("GitLab 401 on GET /user: "), "{e}");
        assert!(!e.contains("glpat-abcdef"));
        assert!(e.ends_with("Check Settings."));
    }
}
