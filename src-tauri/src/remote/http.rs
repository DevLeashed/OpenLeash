//! Just enough HTTP/1.1 for the remote-control server: parse one request head,
//! write one response, close. Every page is a handful of static assets plus a
//! WebSocket, so keep-alive, chunked bodies and pipelining buy nothing here and
//! each would be more parser surface facing the network.

use std::collections::HashMap;

/// Largest request head we accept. Real browsers send well under 8 KiB.
pub const MAX_HEAD: usize = 16 * 1024;
/// Largest body we read (only the pairing form has one).
pub const MAX_BODY: usize = 4 * 1024;
/// Hard ceiling for one authenticated device upload (individual staged file cap is lower).
pub const MAX_UPLOAD_BODY: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path without the query string, percent-decoding *not* applied.
    pub path: String,
    pub query: HashMap<String, String>,
    /// Lowercased header names.
    pub headers: HashMap<String, String>,
    /// Byte length of the head including the blank line.
    pub head_len: usize,
}

impl Request {
    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or("")
    }

    pub fn cookie(&self, name: &str) -> Option<String> {
        self.header("cookie").split(';').find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
    }

    pub fn is_websocket(&self) -> bool {
        self.header("upgrade").eq_ignore_ascii_case("websocket")
            && self
                .header("connection")
                .split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    }

    pub fn content_length(&self) -> usize {
        self.header("content-length").trim().parse().unwrap_or(0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parse {
    /// More bytes are needed.
    Partial,
    Bad,
    TooLarge,
}

/// Parse a request head from the bytes received so far.
pub fn parse(buf: &[u8]) -> Result<Request, Parse> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    let status = match req.parse(buf) {
        Ok(s) => s,
        Err(httparse::Error::TooManyHeaders) => return Err(Parse::TooLarge),
        Err(_) => return Err(Parse::Bad),
    };
    let head_len = match status {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => {
            return Err(if buf.len() >= MAX_HEAD {
                Parse::TooLarge
            } else {
                Parse::Partial
            })
        }
    };
    let method = req.method.ok_or(Parse::Bad)?.to_string();
    let target = req.path.ok_or(Parse::Bad)?;
    // Origin-form only. Absolute-form ("GET http://x/ HTTP/1.1") is for proxies.
    if !target.starts_with('/') {
        return Err(Parse::Bad);
    }
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let mut hs = HashMap::new();
    for h in req.headers.iter() {
        let v = String::from_utf8_lossy(h.value).to_string();
        hs.insert(h.name.to_ascii_lowercase(), v);
    }
    Ok(Request {
        method,
        path: path.to_string(),
        query: parse_query(q),
        headers: hs,
        head_len,
    })
}

pub fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let dec = |s: &str| {
                urlencoding::decode(&s.replace('+', " "))
                    .map(|c| c.into_owned())
                    .ok()
            };
            Some((dec(k)?, dec(v)?))
        })
        .collect()
}

/// Is this a path we would ever serve? Rejects traversal and odd encodings
/// before anything looks the asset up.
pub fn safe_asset_path(path: &str) -> Option<String> {
    let decoded = urlencoding::decode(path).ok()?.into_owned();
    if decoded.contains('\\') || decoded.contains('\0') {
        return None;
    }
    if decoded.split('/').any(|seg| seg == ".." || seg == ".") {
        return None;
    }
    Some(decoded)
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
        }
    }

    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }

    pub fn body(mut self, b: impl Into<Vec<u8>>) -> Self {
        self.body = b.into();
        self
    }

    pub fn text(status: u16, msg: &str) -> Self {
        Self::new(status)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(msg.as_bytes().to_vec())
    }

    pub fn json(status: u16, v: &serde_json::Value) -> Self {
        Self::new(status)
            .header("Content-Type", "application/json")
            .body(v.to_string().into_bytes())
    }

    pub fn redirect(to: &str) -> Self {
        Self::new(303).header("Location", to)
    }

    /// Serialize with the hardening headers every response carries.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        let has = |k: &str| self.headers.iter().any(|(h, _)| h.eq_ignore_ascii_case(k));
        let defaults = [
            ("X-Content-Type-Options", "nosniff"),
            ("Referrer-Policy", "no-referrer"),
            ("X-Frame-Options", "DENY"),
            ("Cross-Origin-Opener-Policy", "same-origin"),
            ("Cache-Control", "no-store"),
        ];
        let missing: Vec<_> = defaults.iter().filter(|(k, _)| !has(k)).collect();
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {}\r\n", v.replace(['\r', '\n'], "")));
        }
        for (k, v) in missing {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.body.len()
        ));
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

fn reason(s: u16) -> &'static str {
    match s {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "",
    }
}

/// MIME type by extension, for the few kinds the bundle contains.
pub fn mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_browser_request() {
        let raw = b"GET /pair?t=ab%20c&x HTTP/1.1\r\nHost: 192.168.1.2:17420\r\nCookie: a=1; ol_session=xyz\r\nUser-Agent: UA\r\n\r\nleftover";
        let r = parse(raw).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/pair");
        assert_eq!(r.query.get("t").map(String::as_str), Some("ab c"));
        assert_eq!(r.query.get("x").map(String::as_str), Some(""));
        assert_eq!(r.header("host"), "192.168.1.2:17420");
        assert_eq!(r.cookie("ol_session").as_deref(), Some("xyz"));
        assert_eq!(r.cookie("missing"), None);
        assert_eq!(r.head_len, raw.len() - "leftover".len());
    }

    #[test]
    fn partial_and_oversized_heads() {
        assert_eq!(
            parse(b"GET / HTTP/1.1\r\nHost: x\r\n").unwrap_err(),
            Parse::Partial
        );
        let mut big = b"GET / HTTP/1.1\r\nX: ".to_vec();
        big.resize(MAX_HEAD + 10, b'a');
        assert_eq!(parse(&big).unwrap_err(), Parse::TooLarge);
        assert_eq!(
            parse(b"GET http://evil/ HTTP/1.1\r\n\r\n").unwrap_err(),
            Parse::Bad
        );
        assert_eq!(parse(b"\x00\x01garbage\r\n\r\n").unwrap_err(), Parse::Bad);
    }

    #[test]
    fn detects_websocket_upgrades() {
        let r = parse(b"GET /__openleash/ws HTTP/1.1\r\nHost: h\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\n\r\n").unwrap();
        assert!(r.is_websocket());
        let r = parse(b"GET / HTTP/1.1\r\nHost: h\r\n\r\n").unwrap();
        assert!(!r.is_websocket());
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(
            safe_asset_path("/assets/a.js").as_deref(),
            Some("/assets/a.js")
        );
        assert!(safe_asset_path("/../secret").is_none());
        assert!(safe_asset_path("/assets/%2e%2e/x").is_none());
        assert!(safe_asset_path("/a%5cb").is_none());
        assert!(safe_asset_path("/a%00b").is_none());
    }

    #[test]
    fn responses_carry_hardening_headers_and_strip_header_injection() {
        let b = Response::text(200, "hi")
            .header("Location", "/x\r\nSet-Cookie: evil=1")
            .to_bytes();
        let s = String::from_utf8(b).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("X-Frame-Options: DENY\r\n"));
        assert!(s.contains("Location: /xSet-Cookie: evil=1\r\n"));
        assert!(!s.contains("\r\nSet-Cookie"));
        assert!(s.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn explicit_headers_override_defaults() {
        let s = String::from_utf8(
            Response::text(200, "")
                .header("Cache-Control", "public, max-age=60")
                .to_bytes(),
        )
        .unwrap();
        assert!(s.contains("Cache-Control: public, max-age=60"));
        assert!(!s.contains("no-store"));
    }
}
