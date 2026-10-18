//! `http_request`: an arbitrary HTTP request — the one built-in tool that can
//! *write* to the network (POST to an API, fire a webhook, poke a local dev
//! server).
//!
//! Two layers keep it honest, and both are needed:
//!
//! * **The permission gate** (`permissions::check`, arm `http_request`) judges
//!   the URL *as written*: method, target class, allow/deny rules, plan mode,
//!   rung. It is synchronous and pure, so it cannot resolve DNS.
//! * **Execution** ([`execute`]) re-checks everything the gate cannot see:
//!   what the hostname actually resolves to (DNS rebinding, internal DNS), and
//!   every redirect hop. The connection is pinned to the addresses it checked,
//!   and system proxies are bypassed — a proxy would resolve the name itself
//!   and the check would be decorative.
//!
//! The target classes ([`Target`]) are ordered by how close to the user they
//! are. `Blocked` (cloud metadata, link-local, multicast…) is refused on every
//! rung, whatever the rules say: there is no legitimate agent use for
//! `169.254.169.254`, and it is the classic SSRF prize. `Loopback` and
//! `Private` are legitimate (testing a local dev server is a core use case) but
//! only ever through an approval or an explicit per-host rule — never silently,
//! not even on Full Access, because "full access" was granted for *this
//! project's* work and a request to the router admin page or another local
//! service is not obviously that.
//!
//! URL parsing is the WHATWG parser (`reqwest::Url`), which is also what the
//! client connects with. That matters: `http://0251.0376.0251.0376/`,
//! `http://2852039166/` and `http://0xA9FEA9FE/` are all 169.254.169.254 to
//! the connector, so they have to be the same thing to the classifier. A
//! hand-rolled `split(':')` parse (like `tools::is_private_target`) sees a
//! dotted public-looking name.

use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

pub use reqwest::Url;

pub const TOOL: &str = "http_request";
pub const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
const READ_METHODS: [&str; 2] = ["GET", "HEAD"];

pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
pub const MAX_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_REDIRECTS: usize = 5;
const MAX_HEADERS: usize = 64;
const REDACTED: &str = "[redacted]";

/// The tool schema. A read-only subagent gets GET/HEAD only — it may check
/// what an endpoint returns (that is research), but the schema must not
/// advertise a write it will be refused at dispatch (see [`subagent_refusal`]).
pub fn schema(read_only: bool) -> Value {
    let methods: Vec<&str> = if read_only {
        READ_METHODS.to_vec()
    } else {
        METHODS.to_vec()
    };
    let description = if read_only {
        "Make a read-only HTTP request (GET or HEAD) and get back the status, the useful response headers and the body (JSON pretty-printed, binary summarised). Use it to check what an API endpoint or a local dev server returns. To read a docs page or article use web_fetch instead. Requests to localhost or a private network need the user's approval; cloud metadata and link-local addresses are always refused."
    } else {
        "Make an HTTP request and get back the status, the useful response headers and the body (JSON pretty-printed, binary summarised, long bodies truncated). Use it to call an API, test an endpoint on a local dev server (`http://localhost:3000/api/...`), or send a webhook. Do NOT use it to read docs, articles or web pages — use web_fetch, which strips HTML — and do NOT use it for GitHub — use the github tool. GET/HEAD to a public host runs like web_fetch; any other method (POST/PUT/PATCH/DELETE/OPTIONS), and any request to localhost or a private network, needs the user's approval (plan mode refuses writes outright). Cloud metadata and link-local addresses are always refused, and redirects into private space are not followed. Put credentials in `headers`, never in the URL; they are redacted from what the user sees, but don't invent or guess secrets. Send `json` for a JSON body (sets Content-Type) or `body` for anything else, not both."
    };
    json!({
        "name": TOOL,
        "description": description,
        "input_schema": {"type":"object","properties":{
            "method":{"type":"string","enum": methods,"description":"Default GET"},
            "url":{"type":"string","description":"http:// or https:// URL, without user:pass@"},
            "headers":{"type":"object","additionalProperties":{"type":"string"},"description":"Request headers, e.g. {\"Authorization\": \"Bearer …\", \"Accept\": \"application/json\"}. Host, Content-Length, Transfer-Encoding and Connection are set by the client."},
            "body":{"type":"string","description":"Raw request body (form data, XML, text). Use `json` for JSON."},
            "json":{"description":"A JSON request body; sets Content-Type: application/json unless you set it"},
            "timeout_ms":{"type":"integer","description":"Default 30000, max 60000"},
            "max_bytes":{"type":"integer","description":"Response body download cap in bytes. Default 1 MiB, max 10 MiB"},
            "follow_redirects":{"type":"boolean","description":"Default true, at most 5 hops"}},
            "required":["url"]}
    })
}

/// Where a request lands, ordered from "anywhere" to "never".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The public internet.
    Public,
    /// This machine: localhost, 127/8, ::1, 0.0.0.0, ::.
    Loopback,
    /// A private network: RFC 1918, CGNAT, unique-local v6, intranet names.
    Private,
    /// Never: cloud metadata, link-local, multicast, broadcast, reserved.
    Blocked,
}

/// A parsed, validated request line.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub target: Target,
}

pub fn is_read_method(m: &str) -> bool {
    READ_METHODS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(m.trim()))
}

/// Parse and validate a URL for this tool: http(s) only, a host, and no
/// userinfo. `https://api.example.com@evil.com/` is a request to evil.com, and
/// a rule or an approval card reading "api.example.com" there is a trap; and a
/// password in a URL ends up in logs. Credentials belong in a header.
pub fn parse_url(s: &str) -> Result<Url, String> {
    let s = s.trim();
    let u = Url::parse(s).map_err(|e| format!("`{}` is not a valid URL: {e}", redact_url(s)))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!(
            "Only http:// and https:// URLs are supported, not `{}:`.",
            u.scheme()
        ));
    }
    if u.host_str().is_none_or(|h| h.is_empty()) {
        return Err("The URL has no host.".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("Refused: the URL carries credentials (`user:pass@host`). That form hides the real host and leaks the secret into logs; send credentials in a header such as Authorization instead.".into());
    }
    Ok(u)
}

/// Parse the method and URL from tool input. A missing method is GET.
pub fn parse_request(input: &Value) -> Result<Request, String> {
    let method = match &input["method"] {
        Value::Null => "GET".to_string(),
        Value::String(m) => m.trim().to_ascii_uppercase(),
        _ => return Err("`method` must be a string.".into()),
    };
    if !METHODS.contains(&method.as_str()) {
        return Err(format!(
            "Method `{method}` is not supported. Use one of {}.",
            METHODS.join(", ")
        ));
    }
    let url = input["url"]
        .as_str()
        .filter(|u| !u.trim().is_empty())
        .ok_or("`url` is required.")?;
    let url = parse_url(url)?;
    let target = classify_url(&url);
    Ok(Request {
        method,
        url,
        target,
    })
}

/// Host names that are the metadata service by name. Not exhaustive — the
/// address checks at execution are what actually stop a name that resolves
/// into link-local space — but these are refused before any DNS is done.
const METADATA_NAMES: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
    "metadata",
    "instance-data",
    "instance-data.ec2.internal",
    "metadata.azure.internal",
    "metadata.platformequinix.com",
];

/// Suffixes that only exist on a private network.
const PRIVATE_SUFFIXES: &[&str] = &[
    ".local",
    ".internal",
    ".lan",
    ".home.arpa",
    ".intranet",
    ".corp",
    ".home",
    ".localdomain",
];

/// The URL's host, typed. `url::Host` is not reachable without a direct `url`
/// dependency, so this reads the serialised host instead — which is safe
/// because the WHATWG parser has already normalised it: every IPv4 spelling
/// (decimal, hex, octal, short) serialises as canonical dotted-quad, and IPv6
/// as a bracketed literal. A domain can therefore never parse as an IP here.
enum HostKind<'a> {
    Ip(IpAddr),
    Domain(&'a str),
}

fn host_kind(u: &Url) -> Option<HostKind<'_>> {
    let h = u.host_str().filter(|h| !h.is_empty())?;
    if let Some(inner) = h.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner
            .parse::<Ipv6Addr>()
            .ok()
            .map(|ip| HostKind::Ip(IpAddr::V6(ip)));
    }
    Some(match h.parse::<Ipv4Addr>() {
        Ok(ip) => HostKind::Ip(IpAddr::V4(ip)),
        Err(_) => HostKind::Domain(h),
    })
}

pub fn classify_url(u: &Url) -> Target {
    match host_kind(u) {
        Some(HostKind::Ip(ip)) => classify_ip(ip),
        Some(HostKind::Domain(d)) => classify_name(d),
        None => Target::Blocked,
    }
}

fn classify_name(d: &str) -> Target {
    let d = d.trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        return Target::Blocked;
    }
    if METADATA_NAMES.contains(&d.as_str())
        || METADATA_NAMES
            .iter()
            .any(|m| d.ends_with(&format!(".{m}")) && m.contains('.'))
    {
        return Target::Blocked;
    }
    if d == "localhost" || d.ends_with(".localhost") || d == "localhost.localdomain" {
        return Target::Loopback;
    }
    // A bare, un-dotted name has no public DNS entry: it is an intranet host.
    if !d.contains('.') || PRIVATE_SUFFIXES.iter().any(|s| d.ends_with(s)) {
        return Target::Private;
    }
    Target::Public
}

pub fn classify_ip(ip: IpAddr) -> Target {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(v4: Ipv4Addr) -> Target {
    let o = v4.octets();
    if v4.is_link_local() // 169.254/16: AWS/GCP/Azure metadata, ECS credentials
        || v4.is_multicast()
        || v4.is_broadcast()
        || o[0] >= 240 // reserved
        || v4 == Ipv4Addr::new(100, 100, 100, 200) // Alibaba metadata
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
    // 192.0.0/24, incl. Oracle's 192.0.0.192
    {
        return Target::Blocked;
    }
    if v4.is_loopback() || o[0] == 0 {
        // 0.0.0.0/8 reaches this machine on Linux and macOS.
        return Target::Loopback;
    }
    if v4.is_private()
        || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
    // benchmarking, used on internal nets
    {
        return Target::Private;
    }
    Target::Public
}

/// An IPv4 address carried inside an IPv6 one, in every form a connector
/// might route back into v4 space.
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = v6.segments();
    let tail = || Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if let Some(v4) = v6.to_ipv4_mapped() {
        return Some(v4); // ::ffff:a.b.c.d
    }
    if s[..6] == [0, 0, 0, 0, 0, 0] && !(s[6] == 0 && s[7] <= 1) {
        return Some(tail()); // ::a.b.c.d (deprecated compatible form)
    }
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return Some(tail()); // NAT64 well-known prefix
    }
    if s[0] == 0x2002 {
        // 6to4: 2002:AABB:CCDD::/48 carries AA.BB.CC.DD.
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    None
}

fn classify_v6(v6: Ipv6Addr) -> Target {
    if v6.is_loopback() || v6.is_unspecified() {
        return Target::Loopback;
    }
    if let Some(v4) = embedded_v4(v6) {
        return classify_v4(v4);
    }
    let s = v6.segments();
    if v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254) // AWS IMDS over v6
        || (s[0] & 0xffc0) == 0xfe80 // link-local
        || (s[0] & 0xff00) == 0xff00
    // multicast
    {
        return Target::Blocked;
    }
    if (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (s[0] & 0xffc0) == 0xfec0 // deprecated site-local
        || (s[0] == 0x2001 && s[1] == 0)
    // Teredo: the embedded client is obfuscated, so do not call it public
    {
        return Target::Private;
    }
    Target::Public
}

/// `host[:port]` as rules and titles name it: lowercase, IPv6 bracketed, the
/// port only when it is not the scheme's default, no trailing dot.
pub fn authority(u: &Url) -> String {
    let host = u
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    }
}

/// A scheme- and effective-port-bound allow-rule key, e.g.
/// `http_request:POST https://api.example.com:443`. The effective port is
/// always explicit: `https://localhost:443` and `http://localhost:80` reach
/// different services, even though neither URL writes its port. Matched
/// exactly, never as a glob.
pub fn rule_key(method: &str, u: &Url) -> String {
    format!(
        "{TOOL}:{} {}://{}:{}",
        method.trim().to_ascii_uppercase(),
        u.scheme(),
        u.host_str()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_ascii_lowercase(),
        u.port_or_known_default().unwrap_or(0)
    )
}

/// The `METHOD host[:port]` subject deny rules are matched against.
pub fn deny_subject(input: &Value) -> Option<String> {
    let r = parse_request(input).ok()?;
    Some(format!("{} {}", r.method, authority(&r.url)))
}

/// Check what a hostname resolved to against what the gate approved.
///
/// A name that was written as public was auto-approved (for GET) as a web
/// read; if it resolves into loopback/private space that is DNS rebinding or an
/// internal DNS entry, and the approval never covered it. A target the user
/// did approve as local may resolve locally — but nothing ever reaches a
/// blocked address.
pub fn check_resolved(written: Target, addrs: &[IpAddr]) -> Result<(), String> {
    if addrs.is_empty() {
        return Err("The host name did not resolve to any address.".into());
    }
    for ip in addrs {
        match classify_ip(*ip) {
            Target::Blocked => {
                return Err(format!(
                    "Refused: the host resolves to {ip}, a link-local / cloud-metadata / reserved address. http_request never reaches those."
                ))
            }
            Target::Public => {}
            local if written == Target::Public => {
                let what = if local == Target::Loopback {
                    "this machine"
                } else {
                    "a private network"
                };
                return Err(format!(
                    "Refused: the host name looks public but resolves to {ip} ({what}). That is how DNS rebinding reaches local services, so it is not followed. If you meant to call a local service, request it by `localhost` or its IP so the user can approve it."
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// What to do with one redirect.
#[derive(Debug)]
pub enum Hop {
    /// Follow it with this method; `keep_body` = resend the request body.
    Follow {
        url: Url,
        method: String,
        keep_body: bool,
    },
    /// Do not follow; hand the 3xx back to the model with this note.
    Stop(String),
    /// Do not follow, and fail the call.
    Refuse(String),
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str()
            .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
            == b.host_str()
                .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Decide one redirect hop. `origin` is the URL the gate approved, `cur` the
/// one that answered with `status` + `location`.
///
/// * A blocked target is refused, always.
/// * A loopback/private target is only followed inside the approved origin
///   (`/login` → `/dashboard` on the same dev server). Anything else would let
///   a public page — or one local service — bounce the call somewhere the
///   user never approved.
/// * 307/308 resend the method and body. Cross-origin, that is a write to a
///   host nobody approved, so it is handed back rather than followed.
/// * 303, and 301/302 after a POST, become a body-less GET (what browsers do).
pub fn redirect_step(origin: &Url, cur: &Url, method: &str, status: u16, location: &str) -> Hop {
    let Ok(next) = cur.join(location.trim()) else {
        return Hop::Refuse(format!(
            "the server sent an invalid redirect location `{}`.",
            redact_url(location)
        ));
    };
    if !matches!(next.scheme(), "http" | "https") {
        return Hop::Refuse(format!(
            "the redirect goes to a `{}:` URL, which this tool does not follow.",
            next.scheme()
        ));
    }
    if !next.username().is_empty() || next.password().is_some() {
        return Hop::Refuse(
            "the redirect location carries `user:pass@` credentials, which hide the real host."
                .into(),
        );
    }
    let method = method.to_ascii_uppercase();
    let new_method = match status {
        303 if method == "HEAD" => "HEAD".to_string(),
        303 => "GET".to_string(),
        301 | 302 if method == "POST" => "GET".to_string(),
        301 | 302 | 307 | 308 => method.clone(),
        _ => {
            return Hop::Stop(format!(
                "status {status} is not a redirect this tool follows."
            ))
        }
    };
    let keep_body = !is_read_method(&new_method);
    let shown = redact_url(next.as_str());
    let target = classify_url(&next);
    if target == Target::Blocked {
        return Hop::Refuse(format!(
            "it points at {shown}, a link-local / cloud-metadata / reserved address."
        ));
    }
    if target != Target::Public && !same_origin(&next, origin) {
        return Hop::Refuse(format!(
            "it points at {shown}, on this machine or a private network, outside the origin that was approved. Request it directly if you need it — that asks the user."
        ));
    }
    if keep_body && !same_origin(&next, origin) {
        return Hop::Stop(format!(
            "The server redirected this {method} ({status}) to {shown}. It was NOT resent there: that host was not approved for a {new_method}. Make a new request to it if that is intended."
        ));
    }
    Hop::Follow {
        url: next,
        method: new_method,
        keep_body,
    }
}

/// Which subagents may use which methods. Read-only agents research; a write
/// is not research, whatever the rung. `None` = no refusal.
pub fn subagent_refusal(policy: &str, input: &Value) -> Option<String> {
    if policy != "read_only" {
        return None;
    }
    let m = input["method"].as_str().unwrap_or("GET");
    if is_read_method(m) {
        return None;
    }
    Some(format!(
        "can only make GET or HEAD requests; `{}` changes something, so it is the parent agent's call.",
        m.trim().to_ascii_uppercase()
    ))
}

// ───────────────────────────── redaction ─────────────────────────────

/// Whether a header, query parameter or JSON key names a secret. Generous on
/// purpose: over-redacting a display costs a little readability, missing a
/// token costs the token.
pub fn is_sensitive_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    const EXACT: &[&str] = &["code", "sig", "jwt", "pass", "pwd", "otp", "pin"];
    const PARTS: &[&str] = &[
        "token",
        "secret",
        "key",
        "auth",
        "password",
        "passwd",
        "session",
        "signature",
        "credential",
        "cookie",
        "csrf",
        "xsrf",
        "bearer",
        "private",
    ];
    EXACT.contains(&n.as_str()) || PARTS.iter().any(|p| n.contains(p))
}

/// A path segment or bare value that looks like a random token (a webhook
/// secret in a Slack/Discord URL path). UUIDs are ids, not secrets, and stay.
fn looks_like_token(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 24
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.~".contains(c))
    {
        return false;
    }
    let uuid = s.len() == 36
        && s.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    !uuid && s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| c.is_ascii_alphabetic())
}

pub fn redact_header_value(name: &str, value: &str) -> String {
    if !is_sensitive_name(name) {
        return value.to_string();
    }
    let n = name.trim().to_ascii_lowercase();
    if n == "authorization" || n == "proxy-authorization" {
        if let Some((scheme, _)) = value.trim().split_once(' ') {
            return format!("{scheme} {REDACTED}");
        }
    }
    REDACTED.to_string()
}

/// `a=1&token=x` → `a=1&token=[redacted]`, keeping the original encoding of
/// everything it does not redact.
fn redact_pairs(q: &str) -> String {
    q.split('&')
        .map(|part| match part.split_once('=') {
            Some((k, v)) => {
                let key = urlencoding::decode(k)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| k.to_string());
                if is_sensitive_name(&key) || looks_like_token(v) {
                    format!("{k}={REDACTED}")
                } else {
                    part.to_string()
                }
            }
            None => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// A URL for display: secret-named query/fragment params and token-looking
/// path segments redacted, any password dropped.
pub fn redact_url(s: &str) -> String {
    let Ok(u) = Url::parse(s.trim()) else {
        // Not a URL we will ever send; still don't echo a query back verbatim.
        return match s.split_once('?') {
            Some((a, q)) => format!("{a}?{}", redact_pairs(q)),
            None => s.to_string(),
        };
    };
    let mut base = u.clone();
    base.set_query(None);
    base.set_fragment(None);
    if base.password().is_some() {
        let _ = base.set_password(Some("redacted"));
    }
    let mut out = base.to_string();
    if base.cannot_be_a_base() {
        // Nothing path-shaped to redact.
    } else if let Some(segs) = base.path_segments() {
        let segs: Vec<String> = segs
            .map(|s| {
                if looks_like_token(s) {
                    REDACTED.to_string()
                } else {
                    s.to_string()
                }
            })
            .collect();
        let path = format!("/{}", segs.join("/"));
        let origin_end = out.len() - base.path().len();
        out = format!("{}{path}", &out[..origin_end]);
    }
    if let Some(q) = u.query() {
        out.push('?');
        out.push_str(&redact_pairs(q));
    }
    if let Some(f) = u.fragment() {
        out.push('#');
        out.push_str(&redact_pairs(f));
    }
    out
}

pub fn redact_json(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let v = if is_sensitive_name(k) {
                        json!(REDACTED)
                    } else {
                        redact_json(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact_json).collect()),
        other => other.clone(),
    }
}

/// A string body for display: JSON and form encodings are redacted by key.
/// Free text is shown as is — there is no reliable way to find a secret in it.
pub fn redact_body(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            return redact_json(&v).to_string();
        }
    }
    if t.contains('=') && !t.contains(char::is_whitespace) {
        return redact_pairs(t);
    }
    s.to_string()
}

/// A copy of the tool input safe to show: the transcript row, ACP clients,
/// the repeated-call card and the reviewer model all see this, never the raw
/// input. (The model wrote the raw input itself, so its own history keeps it.)
pub fn redact_input(input: &Value) -> Value {
    let mut out = input.clone();
    if let Some(u) = input["url"].as_str() {
        out["url"] = json!(redact_url(u));
    }
    if let Some(h) = input["headers"].as_object() {
        out["headers"] = Value::Object(
            h.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => json!(redact_header_value(k, s)),
                        other if is_sensitive_name(k) => {
                            let _ = other;
                            json!(REDACTED)
                        }
                        other => other.clone(),
                    };
                    (k.clone(), v)
                })
                .collect(),
        );
    }
    if !input["json"].is_null() {
        out["json"] = redact_json(&input["json"]);
    }
    if let Some(b) = input["body"].as_str() {
        out["body"] = json!(redact_body(b));
    }
    out
}

/// The approval card's detail: the request as it will be sent, redacted.
pub fn approval_detail(input: &Value, req: &Request) -> String {
    let mut s = format!("{} {}", req.method, redact_url(req.url.as_str()));
    if let Some(h) = input["headers"].as_object() {
        for (k, v) in h {
            let v = v
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string());
            s.push_str(&format!("\n{k}: {}", redact_header_value(k, &v)));
        }
    }
    let body = if !input["json"].is_null() {
        serde_json::to_string_pretty(&redact_json(&input["json"])).unwrap_or_default()
    } else {
        input["body"].as_str().map(redact_body).unwrap_or_default()
    };
    if !body.is_empty() {
        let n = body.chars().count();
        let preview: String = body.chars().take(600).collect();
        s.push_str("\n\n");
        s.push_str(&preview);
        if n > 600 {
            s.push_str(&format!("\n… ({} more characters)", n - 600));
        }
    }
    s
}

/// The approval card's reason line.
pub fn approval_reason(req: &Request, auto_rung: bool) -> String {
    match req.target {
        Target::Loopback => "This reaches a service on this machine (the one running OpenLeash). Local services are outside the public-internet guard, so every call asks unless you allow this method on this host.".into(),
        Target::Private => "This reaches a private network address (LAN, VPN or intranet), which the public-internet guard does not cover, so every call asks unless you allow this method on this host.".into(),
        _ => format!(
            "A {} request can change data on {}.{}",
            req.method,
            authority(&req.url),
            if auto_rung {
                " Auto-edit doesn't cover network writes."
            } else {
                " Requests that change data need your approval."
            }
        ),
    }
}

// ───────────────────────────── execution ─────────────────────────────

pub struct Options<'a> {
    /// Spill a long result to a local file (false for SSH tasks, whose file
    /// tools could not read a local spill path).
    pub keep_spill: bool,
    /// The caller's deny policy for the effective method and URL of a hop.
    pub check_hop: &'a (dyn Fn(&str, &Url) -> Result<(), String> + Send + Sync),
}

impl Options<'_> {
    pub fn redirect_step(
        &self,
        origin: &Url,
        cur: &Url,
        method: &str,
        status: u16,
        location: &str,
    ) -> Hop {
        let mut hop = redirect_step(origin, cur, method, status, location);
        if let Hop::Follow { url, method, .. } = &hop {
            if let Err(reason) = (self.check_hop)(method, url) {
                hop = Hop::Refuse(format!("Redirect refused by the HTTP deny rules: {reason}"));
            }
        }
        hop
    }
}

/// Headers the client owns. Letting the model set them invites request
/// smuggling (Content-Length vs Transfer-Encoding) or virtual-host confusion.
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "proxy-connection",
];

/// Response headers worth a model's tokens.
const SHOWN_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "content-encoding",
    "location",
    "etag",
    "last-modified",
    "cache-control",
    "retry-after",
    "allow",
    "www-authenticate",
    "link",
    "x-request-id",
    "request-id",
    "x-correlation-id",
    "set-cookie",
];
const SHOWN_PREFIXES: &[&str] = &["x-ratelimit-", "ratelimit-", "access-control-allow-"];

fn build_headers(input: &Value) -> Result<reqwest::header::HeaderMap, String> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut map = HeaderMap::new();
    let Some(obj) = (match &input["headers"] {
        Value::Null => None,
        Value::Object(o) => Some(o),
        _ => return Err("`headers` must be an object of name → value.".into()),
    }) else {
        return Ok(map);
    };
    if obj.len() > MAX_HEADERS {
        return Err(format!("At most {MAX_HEADERS} headers."));
    }
    for (k, v) in obj {
        if FORBIDDEN_HEADERS.contains(&k.trim().to_ascii_lowercase().as_str()) {
            return Err(format!(
                "Header `{k}` can't be set: the HTTP client sets it from the URL and body."
            ));
        }
        let v = match v {
            Value::String(s) => s.clone(),
            Value::Number(_) | Value::Bool(_) => v.to_string(),
            _ => return Err(format!("Header `{k}` must have a string value.")),
        };
        let name = HeaderName::from_bytes(k.trim().as_bytes())
            .map_err(|_| format!("`{k}` is not a valid header name."))?;
        let value = HeaderValue::from_str(&v)
            .map_err(|_| format!("Header `{k}` has an invalid value (control characters?)."))?;
        map.append(name, value);
    }
    Ok(map)
}

/// Resolve the URL's host and check each address. Returns the addresses to
/// pin the connection to (empty for an IP-literal host, which needs no DNS).
async fn resolve_checked(u: &Url, deadline: Instant) -> Result<Vec<SocketAddr>, String> {
    let written = classify_url(u);
    if written == Target::Blocked {
        return Err(format!(
            "Refused: {} is a link-local / cloud-metadata / reserved address. http_request never reaches those.",
            redact_url(u.as_str())
        ));
    }
    let port = u.port_or_known_default().unwrap_or(80);
    match host_kind(u) {
        Some(HostKind::Ip(ip)) => check_resolved(written, &[ip]).map(|_| vec![]),
        Some(HostKind::Domain(d)) => {
            let left = deadline.saturating_duration_since(Instant::now());
            let addrs: Vec<SocketAddr> =
                tokio::time::timeout(left, tokio::net::lookup_host((d, port)))
                    .await
                    .map_err(|_| "Timed out resolving the host name.".to_string())?
                    .map_err(|e| format!("Could not resolve `{d}`: {e}"))?
                    .collect();
            let ips: Vec<IpAddr> = addrs.iter().map(|a| a.ip()).collect();
            check_resolved(written, &ips)?;
            Ok(addrs)
        }
        None => Err("The URL has no host.".into()),
    }
}

fn header_lines(h: &reqwest::header::HeaderMap) -> String {
    let mut out = String::new();
    for (k, v) in h {
        let name = k.as_str();
        if !(SHOWN_HEADERS.contains(&name) || SHOWN_PREFIXES.iter().any(|p| name.starts_with(p))) {
            continue;
        }
        let v = String::from_utf8_lossy(v.as_bytes());
        let shown = if name == "set-cookie" {
            // The cookie's name is useful; its value is a session.
            match v.split_once('=') {
                Some((n, _)) => format!("{n}={REDACTED}"),
                None => REDACTED.to_string(),
            }
        } else if name == "location" {
            redact_url(&v)
        } else {
            v.chars().take(500).collect()
        };
        out.push_str(&format!("{name}: {shown}\n"));
    }
    out
}

fn is_textual(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("text/")
        || [
            "json",
            "xml",
            "javascript",
            "ecmascript",
            "x-www-form-urlencoded",
            "graphql",
            "yaml",
            "csv",
            "html",
        ]
        .iter()
        .any(|t| ct.contains(t))
}

fn render_body(bytes: &[u8], truncated: bool, ct: &str, max_bytes: u64) -> String {
    if bytes.is_empty() {
        return "(empty body)".into();
    }
    let textual = is_textual(ct)
        || (ct.is_empty() && std::str::from_utf8(bytes).is_ok_and(|s| !s.contains('\0')));
    let mut out = if !textual {
        format!(
            "[binary body: {}{} bytes, content-type {} — not shown]",
            if truncated { "at least " } else { "" },
            bytes.len(),
            if ct.is_empty() { "unknown" } else { ct }
        )
    } else {
        let text = String::from_utf8_lossy(bytes).into_owned();
        if !truncated && ct.to_ascii_lowercase().contains("json") {
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or(text)
        } else {
            text
        }
    };
    if truncated {
        out.push_str(&format!(
            "\n\n[… body truncated at {max_bytes} bytes (max_bytes). Raise max_bytes (up to {MAX_MAX_BYTES}) or ask the API for less.]"
        ));
    }
    out
}

/// Make the request. The permission gate has already run; this re-checks the
/// target itself anyway, so a dispatch path that skipped the gate still never
/// reaches a blocked address, and it checks what the gate could not: DNS and
/// redirects.
pub async fn execute(input: &Value, opts: &Options<'_>) -> Result<String, String> {
    use futures_util::StreamExt;
    let req = parse_request(input)?;
    let mut headers = build_headers(input)?;
    if !input["json"].is_null() && input["body"].is_string() {
        return Err("Send either `json` or `body`, not both.".into());
    }
    let body: Option<Vec<u8>> = if !input["json"].is_null() {
        if !headers.contains_key(reqwest::header::CONTENT_TYPE) {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
        }
        Some(serde_json::to_vec(&input["json"]).map_err(|e| e.to_string())?)
    } else {
        input["body"].as_str().map(|s| s.as_bytes().to_vec())
    };
    if !headers.contains_key(reqwest::header::USER_AGENT) {
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("OpenLeash/0.1 (+agent harness)"),
        );
    }
    let timeout = Duration::from_millis(
        input["timeout_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS),
    );
    let max_bytes = input["max_bytes"]
        .as_u64()
        .unwrap_or(DEFAULT_MAX_BYTES)
        .clamp(1, MAX_MAX_BYTES);
    let follow = input["follow_redirects"].as_bool().unwrap_or(true);
    let deadline = Instant::now() + timeout;

    let origin = req.url.clone();
    let mut url = req.url.clone();
    let mut method = req.method.clone();
    let mut keep_body = true;
    let mut hops = 0usize;
    let mut stop_note: Option<String> = None;
    let resp = loop {
        let pinned = resolve_checked(&url, deadline).await?;
        let mut b = reqwest::Client::builder()
            // A proxy resolves the name itself, which would make the address
            // check above meaningless.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15));
        if let (false, Some(host)) = (pinned.is_empty(), url.host_str()) {
            b = b.resolve_to_addrs(host, &pinned);
        }
        let client = b.build().map_err(|e| format!("HTTP client failed: {e}"))?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("Timed out after {} ms.", timeout.as_millis()));
        }
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut rb = client
            .request(m, url.clone())
            .headers(headers.clone())
            .timeout(left);
        if let (true, Some(b)) = (keep_body, &body) {
            rb = rb.body(b.clone());
        }
        let resp = rb.send().await.map_err(|e| {
            if e.is_timeout() {
                format!("Timed out after {} ms.", timeout.as_millis())
            } else {
                format!("Request failed: {}", without_url(&e))
            }
        })?;
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let Some(location) = location.filter(|_| follow && resp.status().is_redirection()) else {
            break resp;
        };
        if hops >= MAX_REDIRECTS {
            return Err(format!(
                "Stopped after {MAX_REDIRECTS} redirects (last at {}). The server is redirecting in a loop or too far.",
                redact_url(url.as_str())
            ));
        }
        match opts.redirect_step(&origin, &url, &method, status, &location) {
            Hop::Follow {
                url: next,
                method: m,
                keep_body: k,
            } => {
                if !same_origin(&next, &url) {
                    // Credentials were meant for the host the user approved.
                    let drop: Vec<_> = headers
                        .keys()
                        .filter(|n| is_sensitive_name(n.as_str()))
                        .cloned()
                        .collect();
                    for n in drop {
                        headers.remove(n);
                    }
                }
                if !k {
                    headers.remove(reqwest::header::CONTENT_TYPE);
                }
                url = next;
                method = m;
                keep_body = keep_body && k;
                hops += 1;
            }
            Hop::Stop(note) => {
                stop_note = Some(note);
                break resp;
            }
            Hop::Refuse(why) => {
                return Err(format!(
                    "Redirect refused (from {}): {why}",
                    redact_url(url.as_str())
                ))
            }
        }
    };

    let status = resp.status();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut out = format!(
        "HTTP {} {} · {} {}\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
        method,
        redact_url(url.as_str())
    );
    if hops > 0 {
        out.push_str(&format!(
            "(redirected from {} after {hops} hop{})\n",
            redact_url(origin.as_str()),
            if hops == 1 { "" } else { "s" }
        ));
    }
    out.push_str(&header_lines(resp.headers()));
    if let Some(n) = &stop_note {
        out.push_str(&format!("[{n}]\n"));
    }
    if method == "HEAD" {
        return Ok(out.trim_end().to_string());
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = resp.bytes_stream();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let next = match tokio::time::timeout(left, stream.next()).await {
            Ok(n) => n,
            Err(_) => {
                truncated = true;
                out.push_str("[timed out while reading the body; showing what arrived]\n");
                break;
            }
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| format!("Reading the body failed: {}", without_url(&e)))?;
        buf.extend_from_slice(&chunk);
        if buf.len() as u64 > max_bytes {
            buf.truncate(max_bytes as usize);
            truncated = true;
            break;
        }
    }
    out.push('\n');
    out.push_str(&render_body(&buf, truncated, &ct, max_bytes));
    Ok(super::shell::truncate_output_with(&out, opts.keep_spill))
}

/// A reqwest error's text without the URL it embeds, which may carry a
/// token in its query string.
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s.replace(u.as_str(), &redact_url(u.as_str()));
    }
    s
}
