//! A local filtering HTTP(S) proxy every browser request is routed through
//! (`--proxy-server`), so the network policy is enforced on the address the
//! connection is actually made to — the defence against DNS rebinding that
//! request interception alone can't give.
//!
//! How: Chrome never resolves a proxied host itself. For each request the
//! proxy classifies the host as written (`classify_host`), resolves a name
//! itself, judges **every** address it got (`NetPolicy::check_resolved`), and
//! then connects to one of exactly those addresses. A name that answers
//! "public" to Chrome's check and "169.254.169.254" a millisecond later has
//! nowhere to go: there is no second lookup.
//!
//! Two request forms are served: `CONNECT host:port` (HTTPS and WSS, tunnelled
//! opaquely after the check) and absolute-form `GET http://host/…` (plain HTTP
//! and WS), which is rewritten to origin-form with `Proxy-*` headers stripped
//! and `Connection: close` forced both ways, so every request is judged on a
//! fresh connection. Origin-form requests (something talking to the proxy as
//! if it were a web server) are refused. Request and response bodies are only
//! ever copied, never inspected or logged.
//!
//! Limits (see docs/security-model.md): the proxy sees only what Chrome sends
//! through it. Chrome bypasses proxies for loopback unless launched with
//! `--proxy-bypass-list=<-loopback>`, and WebRTC can send UDP around a proxy
//! unless `--force-webrtc-ip-handling-policy=disable_non_proxied_udp` is set —
//! both are the launcher's job (`browser.rs`). The listener is on 127.0.0.1,
//! so other local processes can use it too; it never lets them reach anything
//! the policy would refuse the page.

use super::browser_policy::{classify_host, classify_ip, normalize_host, HostClass, NetVerdict};
use super::browser_policy::{NetPolicy, SharedPolicy};
use std::collections::VecDeque;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// Largest request head accepted. Chrome's own limit on request headers is
/// far below this; anything bigger is not a browser.
const MAX_REQUEST_HEAD: usize = 32 * 1024;
/// Response heads can carry many `Set-Cookie`s; still bounded.
const MAX_RESPONSE_HEAD: usize = 64 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
const DNS_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How many recent refusals `ProxyHandle::recent_blocks` keeps.
const RECENT_BLOCKS: usize = 32;

type ResolveFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;
/// Name → addresses. The system resolver in production; tests inject one so a
/// name can "rebind" to a metadata address without touching real DNS.
type Resolver = Arc<dyn Fn(String) -> ResolveFuture + Send + Sync>;
/// Test seam: where a judged address is actually dialled (identity in
/// production), so an approved "LAN" address can be served by a loopback
/// listener.
type Dialer = Arc<dyn Fn(SocketAddr) -> SocketAddr + Send + Sync>;

struct Config {
    policy: SharedPolicy,
    resolver: Resolver,
    dialer: Dialer,
    blocks: Arc<Mutex<VecDeque<String>>>,
}

/// A running proxy. Dropping it stops the listener.
pub struct ProxyHandle {
    /// 127.0.0.1 port the proxy listens on. Never 0 for a started proxy
    /// (0 meant "no proxy" only in the pre-implementation placeholder).
    pub port: u16,
    task: Option<JoinHandle<()>>,
    blocks: Arc<Mutex<VecDeque<String>>>,
}

impl ProxyHandle {
    /// The most recent refusals, oldest first, as `host:port — reason`. Hosts
    /// and reasons only: never a path, query, header or body. Lets the CDP
    /// core explain a `net::ERR_TUNNEL_CONNECTION_FAILED` to the agent.
    pub fn recent_blocks(&self) -> Vec<String> {
        self.blocks
            .lock()
            .map(|b| b.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        // Aborting the accept task drops its JoinSet, which aborts every
        // connection still open — a stopped session leaves no tunnel behind.
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

/// Start a proxy enforcing `policy` on 127.0.0.1 with an OS-chosen port.
pub async fn start(policy: SharedPolicy) -> Result<ProxyHandle, String> {
    let resolver: Resolver = Arc::new(|host: String| {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    });
    start_with(policy, resolver, Arc::new(|a| a)).await
}

async fn start_with(
    policy: SharedPolicy,
    resolver: Resolver,
    dialer: Dialer,
) -> Result<ProxyHandle, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| format!("browser proxy: can't listen on 127.0.0.1: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("browser proxy: {e}"))?
        .port();
    // The proxy must never be its own upstream, even before the launcher
    // adds the CDP port to this shared policy.
    policy
        .write()
        .map_err(|_| "browser proxy: policy unavailable".to_string())?
        .blocked_ports
        .push(port);
    let blocks = Arc::new(Mutex::new(VecDeque::new()));
    let cfg = Arc::new(Config {
        policy,
        resolver,
        dialer,
        blocks: blocks.clone(),
    });
    let task = tokio::spawn(async move {
        let mut conns = JoinSet::new();
        loop {
            tokio::select! {
                r = listener.accept() => match r {
                    Ok((sock, _)) => {
                        let _ = sock.set_nodelay(true);
                        conns.spawn(serve(sock, cfg.clone()));
                    }
                    // Out of descriptors and the like: back off, don't spin.
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                },
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    });
    Ok(ProxyHandle {
        port,
        task: Some(task),
        blocks,
    })
}

/// Read up to and including the blank line ending an HTTP head. Returns the
/// head and whatever arrived after it (which belongs to the body or tunnel).
async fn read_head<R: AsyncRead + Unpin>(
    r: &mut R,
    max: usize,
) -> Result<(Vec<u8>, Vec<u8>), HeadError> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = find_head_end(&buf) {
            if end > max {
                return Err(HeadError::TooLarge);
            }
            let rest = buf.split_off(end);
            return Ok((buf, rest));
        }
        if buf.len() > max {
            return Err(HeadError::TooLarge);
        }
        let n = r.read(&mut chunk).await.map_err(|_| HeadError::Io)?;
        if n == 0 {
            return Err(HeadError::Io);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

#[derive(Debug)]
enum HeadError {
    TooLarge,
    Io,
}

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

fn parse_head(raw: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let version = first.next()?;
    if first.next().is_some() || !version.starts_with("HTTP/1.") || method.is_empty() {
        return None;
    }
    let mut headers = vec![];
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':')?;
        let v = v.trim();
        if k.is_empty()
            || !k
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || v.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
        {
            return None;
        }
        headers.push((k.to_string(), v.to_string()));
    }
    Some(Head {
        method,
        target,
        headers,
    })
}

async fn respond<W: AsyncWrite + Unpin>(w: &mut W, code: u16, phrase: &str, body: &str) {
    let body = format!("OpenLeash browser proxy: {body}\n");
    let msg = format!(
        "HTTP/1.1 {code} {phrase}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nProxy-Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = w.write_all(msg.as_bytes()).await;
    let _ = w.shutdown().await;
}

fn record_block(cfg: &Config, host: &str, port: u16, reason: &str) {
    if let Ok(mut b) = cfg.blocks.lock() {
        if b.len() >= RECENT_BLOCKS {
            b.pop_front();
        }
        let line = format!("{host}:{port} — {reason}");
        b.push_back(line.chars().take(240).collect());
    }
}

/// Split a CONNECT authority (`host:port`, `[v6]:port`).
fn split_authority(a: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = a.strip_prefix('[') {
        let (h, p) = rest.split_once("]:")?;
        (format!("[{h}]"), p)
    } else {
        let (h, p) = a.rsplit_once(':')?;
        (h.to_string(), p)
    };
    let port: u16 = port.parse().ok()?;
    (port != 0 && !host.is_empty()).then_some((host, port))
}

/// A refusal: status code and the reason shown to the browser and recorded.
type Refusal = (u16, &'static str, String);

/// The policy decision for one target: the addresses to dial, or a refusal.
async fn decide(cfg: &Config, host: &str, port: u16) -> Result<Vec<SocketAddr>, Refusal> {
    let forbidden = |r: String| (403, "Forbidden", r);
    // Snapshot the policy: the lock is never held across a lookup.
    let pol: NetPolicy = cfg.policy.read().map(|p| p.clone()).map_err(|_| {
        (
            500,
            "Internal Server Error",
            "policy unavailable".to_string(),
        )
    })?;
    let name = normalize_host(host);
    let class = classify_host(host);
    if class == HostClass::Blocked {
        return Err(forbidden(format!(
            "{name} is a cloud-metadata, link-local or reserved address and is never reachable"
        )));
    }
    let ips: Vec<IpAddr> = if let Ok(ip) = name.parse::<IpAddr>() {
        vec![ip]
    } else if class == HostClass::Loopback {
        // `localhost` / `*.localhost` mean this machine, whatever a hosts
        // file or resolver says: pinned, not looked up.
        vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
    } else {
        if class == HostClass::Lan && !pol.lan_approved(&name) {
            return Err(forbidden(format!("LAN host {name} needs approval")));
        }
        match tokio::time::timeout(DNS_TIMEOUT, (cfg.resolver)(name.clone())).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err((502, "Bad Gateway", format!("can't resolve {name}: {e}"))),
            Err(_) => {
                return Err((
                    504,
                    "Gateway Timeout",
                    format!("resolving {name} timed out"),
                ))
            }
        }
    };
    let verdict = if name.parse::<IpAddr>().is_ok() {
        match classify_ip(ips[0]) {
            HostClass::Blocked => NetVerdict::Block(format!("{name} is never reachable")),
            HostClass::Lan if !pol.lan_approved(&name) => NetVerdict::NeedsLan(name.clone()),
            _ => NetVerdict::Allow,
        }
    } else {
        pol.check_resolved(&name, &ips)
    };
    match verdict {
        NetVerdict::Allow => {}
        NetVerdict::Block(r) => return Err(forbidden(r)),
        NetVerdict::NeedsLan(h) => return Err(forbidden(format!("LAN host {h} needs approval"))),
    }
    let addrs: Vec<SocketAddr> = ips
        .into_iter()
        .filter(|ip| {
            !(classify_ip(*ip) == HostClass::Loopback && pol.blocked_ports.contains(&port))
        })
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    if addrs.is_empty() {
        return Err(forbidden(format!(
            "port {port} on this machine belongs to OpenLeash's own browser plumbing"
        )));
    }
    Ok(addrs)
}

/// Dial the first judged address that answers.
async fn dial(cfg: &Config, addrs: &[SocketAddr]) -> Result<TcpStream, Refusal> {
    let mut last = String::from("no address");
    for a in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((cfg.dialer)(*a))).await {
            Ok(Ok(s)) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "timed out".into(),
        }
    }
    Err((502, "Bad Gateway", format!("couldn't connect: {last}")))
}

async fn serve(mut client: TcpStream, cfg: Arc<Config>) {
    let (raw, rest) =
        match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client, MAX_REQUEST_HEAD)).await {
            Ok(Ok(v)) => v,
            Ok(Err(HeadError::TooLarge)) => {
                respond(
                    &mut client,
                    431,
                    "Request Header Fields Too Large",
                    "request head too large",
                )
                .await;
                // Read (and discard) what the client is still sending for a
                // moment: closing a socket with unread input resets it, and
                // the client would see a reset instead of the 431.
                let _ = tokio::time::timeout(Duration::from_secs(1), async {
                    let mut b = [0u8; 8192];
                    let mut total = 0usize;
                    while total < (1 << 20) {
                        match client.read(&mut b).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => total += n,
                        }
                    }
                })
                .await;
                return;
            }
            Ok(Err(HeadError::Io)) | Err(_) => return,
        };
    let Some(head) = parse_head(&raw) else {
        return respond(&mut client, 400, "Bad Request", "malformed request").await;
    };
    if head.method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_authority(&head.target) else {
            return respond(&mut client, 400, "Bad Request", "CONNECT needs host:port").await;
        };
        let addrs = match decide(&cfg, &host, port).await {
            Ok(a) => a,
            Err((code, phrase, why)) => {
                record_block(&cfg, &normalize_host(&host), port, &why);
                return respond(&mut client, code, phrase, &why).await;
            }
        };
        let mut upstream = match dial(&cfg, &addrs).await {
            Ok(s) => s,
            Err((code, phrase, why)) => return respond(&mut client, code, phrase, &why).await,
        };
        if client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }
        if !rest.is_empty() && upstream.write_all(&rest).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        return;
    }
    // Absolute-form only: `GET http://host/…`. Origin-form (`GET /`) is a
    // client that thinks this is a web server — or something probing it.
    let lower = head.target.to_ascii_lowercase();
    if !lower.starts_with("http://") {
        let why = if head.target.starts_with('/') {
            "this is a proxy, not a web server"
        } else {
            "only absolute http:// requests and CONNECT are proxied"
        };
        return respond(&mut client, 400, "Bad Request", why).await;
    }
    let Ok(url) = reqwest::Url::parse(&head.target) else {
        return respond(&mut client, 400, "Bad Request", "unparseable request URL").await;
    };
    let host = url.host_str().unwrap_or("").to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    if host.is_empty() {
        return respond(&mut client, 400, "Bad Request", "request URL has no host").await;
    }
    let addrs = match decide(&cfg, &host, port).await {
        Ok(a) => a,
        Err((code, phrase, why)) => {
            record_block(&cfg, &normalize_host(&host), port, &why);
            return respond(&mut client, code, phrase, &why).await;
        }
    };
    let upstream = match dial(&cfg, &addrs).await {
        Ok(s) => s,
        Err((code, phrase, why)) => return respond(&mut client, code, phrase, &why).await,
    };
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    let upgrade = head
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("upgrade"));
    let mut out = format!("{} {path} HTTP/1.1\r\n", head.method);
    // Never send a caller-supplied virtual host that disagrees with the
    // authority we judged and dialled.
    let authority = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    out.push_str(&format!("Host: {authority}\r\n"));
    for (k, v) in &head.headers {
        let l = k.to_ascii_lowercase();
        if l.starts_with("proxy-") || matches!(l.as_str(), "host" | "connection" | "keep-alive") {
            continue;
        }
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(if upgrade {
        "Connection: Upgrade\r\n\r\n"
    } else {
        "Connection: close\r\n\r\n"
    });
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();
    if uw.write_all(out.as_bytes()).await.is_err() {
        return;
    }
    if !rest.is_empty() && uw.write_all(&rest).await.is_err() {
        return;
    }
    let up = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut cr, &mut uw).await;
        let _ = uw.shutdown().await;
    });
    // The response head gets `Connection: close` too, so the browser opens a
    // new proxy connection — judged afresh — for its next request instead of
    // reusing this one, which is pinned to this upstream.
    if let Ok((rhead, rrest)) = read_head(&mut ur, MAX_RESPONSE_HEAD).await {
        let rewritten = close_response(&rhead);
        if cw.write_all(&rewritten).await.is_ok() && cw.write_all(&rrest).await.is_ok() {
            let _ = tokio::io::copy(&mut ur, &mut cw).await;
        }
    }
    let _ = cw.shutdown().await;
    up.abort();
}

/// The response head with its connection headers replaced by `close`. A
/// `101 Switching Protocols` (WebSocket) keeps its `Upgrade` semantics.
fn close_response(head: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(head) else {
        return head.to_vec();
    };
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or("");
    if status.split(' ').nth(1) == Some("101") {
        return head.to_vec();
    }
    let mut out = format!("{status}\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let name = line
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if name == "connection" || name == "keep-alive" || name == "proxy-connection" {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\nProxy-Connection: close\r\n\r\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

    fn shared(p: NetPolicy) -> SharedPolicy {
        Arc::new(RwLock::new(p))
    }

    fn fake_dns(map: &[(&str, &[&str])]) -> Resolver {
        let m: HashMap<String, Vec<IpAddr>> = map
            .iter()
            .map(|(h, ips)| {
                (
                    h.to_string(),
                    ips.iter().map(|i| i.parse().unwrap()).collect(),
                )
            })
            .collect();
        let m = Arc::new(m);
        Arc::new(move |host: String| {
            let m = m.clone();
            Box::pin(async move {
                m.get(&host).cloned().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no such host")
                })
            })
        })
    }

    /// An echo server on loopback; returns its port.
    async fn echo() -> u16 {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        port
    }

    async fn send(proxy: u16, req: &[u8]) -> TcpStream {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy))
            .await
            .unwrap();
        s.write_all(req).await.unwrap();
        s
    }

    /// Read until the response head is complete; returns it as text.
    async fn head_of(s: &mut TcpStream) -> String {
        let (h, _) = tokio::time::timeout(Duration::from_secs(10), read_head(s, 1 << 20))
            .await
            .expect("proxy answered in time")
            .expect("a response head");
        String::from_utf8_lossy(&h).into_owned()
    }

    async fn read_all(s: &mut TcpStream) -> String {
        let mut v = vec![];
        let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut v)).await;
        String::from_utf8_lossy(&v).into_owned()
    }

    async fn roundtrip(s: &mut TcpStream, msg: &[u8]) -> Vec<u8> {
        s.write_all(msg).await.unwrap();
        let mut got = vec![0u8; msg.len()];
        tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut got))
            .await
            .unwrap()
            .unwrap();
        got
    }

    #[tokio::test]
    async fn connect_to_an_allowed_loopback_server_tunnels() {
        let target = echo().await;
        let p = start_with(shared(NetPolicy::default()), fake_dns(&[]), Arc::new(|a| a))
            .await
            .unwrap();
        assert_ne!(p.port, 0);
        let mut s = send(
            p.port,
            format!("CONNECT 127.0.0.1:{target} HTTP/1.1\r\nHost: 127.0.0.1:{target}\r\n\r\n")
                .as_bytes(),
        )
        .await;
        assert!(head_of(&mut s).await.starts_with("HTTP/1.1 200"));
        assert_eq!(roundtrip(&mut s, b"ping").await, b"ping");
        // `localhost` is pinned to loopback without a lookup.
        let mut s = send(
            p.port,
            format!("CONNECT localhost:{target} HTTP/1.1\r\n\r\n").as_bytes(),
        )
        .await;
        assert!(head_of(&mut s).await.starts_with("HTTP/1.1 200"));
        assert_eq!(roundtrip(&mut s, b"pong").await, b"pong");
    }

    #[tokio::test]
    async fn a_name_that_rebinds_to_metadata_is_refused() {
        let p = start_with(
            shared(NetPolicy::default()),
            fake_dns(&[
                ("rebind.test", &["169.254.169.254"]),
                ("mixed.test", &["93.184.216.34", "169.254.169.254"]),
                ("loop.test", &["127.0.0.1"]),
            ]),
            Arc::new(|a| a),
        )
        .await
        .unwrap();
        for host in ["rebind.test", "mixed.test", "loop.test"] {
            let mut s = send(
                p.port,
                format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n").as_bytes(),
            )
            .await;
            let h = head_of(&mut s).await;
            assert!(h.starts_with("HTTP/1.1 403"), "{host}: {h}");
        }
        let blocks = p.recent_blocks();
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(blocks[0].starts_with("rebind.test:443"), "{blocks:?}");
    }

    #[tokio::test]
    async fn a_lan_address_needs_approval_then_connects() {
        let target = echo().await;
        let policy = shared(NetPolicy::default());
        // The "LAN" address is really served by the loopback echo server.
        let dialer: Dialer = Arc::new(move |a: SocketAddr| {
            if a.ip() == "192.168.77.5".parse::<IpAddr>().unwrap() {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), target)
            } else {
                a
            }
        });
        let p = start_with(
            policy.clone(),
            fake_dns(&[("lan.test", &["192.168.77.5"])]),
            dialer,
        )
        .await
        .unwrap();
        for host in ["lan.test", "192.168.77.5", "printer.local"] {
            let mut s = send(
                p.port,
                format!("CONNECT {host}:8080 HTTP/1.1\r\n\r\n").as_bytes(),
            )
            .await;
            let r = read_all(&mut s).await;
            assert!(r.starts_with("HTTP/1.1 403"), "{host}: {r}");
            assert!(r.contains("needs approval"), "{host}: {r}");
        }
        policy
            .write()
            .unwrap()
            .lan_allowed
            .insert("lan.test".into());
        policy
            .write()
            .unwrap()
            .lan_allowed
            .insert("192.168.77.5".into());
        for host in ["lan.test", "192.168.77.5"] {
            let mut s = send(
                p.port,
                format!("CONNECT {host}:8080 HTTP/1.1\r\n\r\n").as_bytes(),
            )
            .await;
            let h = head_of(&mut s).await;
            assert!(h.starts_with("HTTP/1.1 200"), "{host}: {h}");
            assert_eq!(roundtrip(&mut s, b"hello").await, b"hello");
        }
    }

    #[tokio::test]
    async fn metadata_literals_blocked_ports_and_origin_form_are_refused() {
        let target = echo().await;
        let p = start_with(
            shared(NetPolicy {
                blocked_ports: vec![target],
                ..Default::default()
            }),
            fake_dns(&[]),
            Arc::new(|a| a),
        )
        .await
        .unwrap();
        let cases: Vec<(String, &str)> = vec![
            ("CONNECT 169.254.169.254:80 HTTP/1.1\r\n\r\n".into(), "403"),
            ("CONNECT [::ffff:169.254.169.254]:443 HTTP/1.1\r\n\r\n".into(), "403"),
            ("CONNECT metadata.google.internal:80 HTTP/1.1\r\n\r\n".into(), "403"),
            ("GET http://169.254.169.254/latest/meta-data/ HTTP/1.1\r\nHost: 169.254.169.254\r\n\r\n".into(), "403"),
            ("GET http://0xA9FEA9FE/ HTTP/1.1\r\n\r\n".into(), "403"),
            (format!("CONNECT 127.0.0.1:{target} HTTP/1.1\r\n\r\n"), "403"),
            (format!("CONNECT localhost:{target} HTTP/1.1\r\n\r\n"), "403"),
            (format!("GET http://[::1]:{target}/ HTTP/1.1\r\n\r\n"), "403"),
            ("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".into(), "400"),
            ("GET https://example.com/ HTTP/1.1\r\n\r\n".into(), "400"),
            ("CONNECT example.com HTTP/1.1\r\n\r\n".into(), "400"),
            ("garbage\r\n\r\n".into(), "400"),
        ];
        for (req, code) in cases {
            let mut s = send(p.port, req.as_bytes()).await;
            let h = head_of(&mut s).await;
            assert!(h.starts_with(&format!("HTTP/1.1 {code}")), "{req:?} → {h}");
        }
    }

    #[tokio::test]
    async fn an_oversized_request_head_is_rejected() {
        let p = start_with(shared(NetPolicy::default()), fake_dns(&[]), Arc::new(|a| a))
            .await
            .unwrap();
        let mut req = b"GET http://127.0.0.1/ HTTP/1.1\r\n".to_vec();
        for i in 0..2000 {
            req.extend_from_slice(format!("X-Filler-{i}: {}\r\n", "a".repeat(40)).as_bytes());
        }
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, p.port))
            .await
            .unwrap();
        // The proxy may answer and close before the whole head is written.
        let _ = s.write_all(&req).await;
        let h = head_of(&mut s).await;
        assert!(h.starts_with("HTTP/1.1 431"), "{h}");
    }

    #[tokio::test]
    async fn plain_http_is_rewritten_to_origin_form_and_closed() {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let (h, _) = read_head(&mut s, 1 << 20).await.unwrap();
            let _ = tx.send(String::from_utf8_lossy(&h).into_owned());
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\n\r\nok")
                .await
                .unwrap();
            let _ = s.shutdown().await;
        });
        let p = start_with(shared(NetPolicy::default()), fake_dns(&[]), Arc::new(|a| a))
            .await
            .unwrap();
        let mut s = send(
            p.port,
            format!(
                "GET http://127.0.0.1:{target}/path?q=1 HTTP/1.1\r\nHost: 127.0.0.1:{target}\r\nProxy-Authorization: Basic eDp5\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
        let resp = read_all(&mut s).await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
        assert!(resp.contains("Connection: close"), "{resp}");
        assert!(!resp.to_ascii_lowercase().contains("keep-alive"), "{resp}");
        assert!(resp.ends_with("ok"), "{resp}");
        let upstream = rx.await.unwrap();
        assert!(
            upstream.starts_with("GET /path?q=1 HTTP/1.1\r\n"),
            "{upstream}"
        );
        assert!(upstream.contains("Connection: close"), "{upstream}");
        assert!(upstream.contains("Accept: */*"), "{upstream}");
        assert!(
            !upstream.to_ascii_lowercase().contains("proxy-"),
            "{upstream}"
        );
        assert!(!upstream.contains("keep-alive"), "{upstream}");
    }

    #[tokio::test]
    async fn complete_heads_are_bounded_and_header_controls_are_refused() {
        let raw = b"GET http://example.com/ HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut reader = &raw[..];
        assert!(matches!(
            read_head(&mut reader, raw.len() - 1).await,
            Err(HeadError::TooLarge)
        ));
        for raw in [
            &b"GET http://example.com/ HTTP/1.1\r\nX-A: x\ny\r\n\r\n"[..],
            &b"GET http://example.com/ HTTP/1.1\r\nBad(Name: x\r\n\r\n"[..],
        ] {
            assert!(parse_head(raw).is_none());
        }
    }

    #[tokio::test]
    async fn dropping_the_handle_stops_the_listener() {
        let p = start_with(shared(NetPolicy::default()), fake_dns(&[]), Arc::new(|a| a))
            .await
            .unwrap();
        let port = p.port;
        drop(p);
        tokio::time::sleep(Duration::from_millis(200)).await;
        match TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
            Err(_) => {}
            Ok(mut s) => {
                // Accepted by a lingering backlog at most: never served.
                let _ = s.write_all(b"GET / HTTP/1.1\r\n\r\n").await;
                let mut b = [0u8; 16];
                let n = tokio::time::timeout(Duration::from_secs(2), s.read(&mut b))
                    .await
                    .map(|r| r.unwrap_or(0))
                    .unwrap_or(0);
                assert_eq!(n, 0, "a dropped proxy must not answer");
            }
        }
    }
}
