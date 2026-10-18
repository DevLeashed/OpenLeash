//! Pure policy for the browser plugin: which network targets a page may reach,
//! which actions are reads, which targets are sensitive, and where downloads
//! and uploads may live.
//!
//! Everything here is synchronous, does no DNS and no I/O beyond
//! canonicalising paths, so it can be shared by three enforcement points that
//! must agree: the CDP request interception (`browser.rs`), the filtering
//! proxy every browser connection goes through (`browser_proxy.rs`, which adds
//! the DNS answer via `check_resolved`), and the permission arm in
//! `permissions.rs` (actions, sensitivity, site rules).
//!
//! # Network classes
//!
//! | class      | what                                                        | verdict |
//! |------------|-------------------------------------------------------------|---------|
//! | `Loopback` | 127.0.0.0/8, ::1, `localhost`, `*.localhost`                 | allowed, except `blocked_ports` (the CDP endpoint, the proxy) |
//! | `Lan`      | 10/8, 172.16/12, 192.168/16, 100.64/10 (CGNAT, incl. Tailscale), fc00::/7, fec0::/10, `.local` `.lan` `.internal` `.home.arpa` `.intranet` `.corp` `.home` `.localdomain`, single-label names | needs per-host approval (`browser:lan <host>`) |
//! | `Blocked`  | link-local 169.254/16 + fe80::/10 (every cloud's metadata service lives there), fd00:ec2::254 (AWS IMDS v6), 100.100.100.200 (Alibaba metadata, carved out of CGNAT), 0.0.0.0/8 + :: (reach loopback on Linux/macOS), 255.255.255.255, multicast 224/4 + ff00::/8, reserved 240/4, 192.0.0.0/24 (IETF protocol assignments), documentation ranges 192.0.2/24 198.51.100/24 203.0.113/24 2001:db8::/32, discard 100::/64, metadata host names, and any host string a browser would reject | never |
//! | `Public`   | everything else, including 198.18.0.0/15 (see below)         | allowed |
//!
//! 198.18.0.0/15 (benchmarking) is deliberately `Public`: "fake-IP" proxies
//! (Clash, Surge, sing-box) answer *every* DNS query from it, so classing it as
//! LAN or blocked would make the browser useless for those users, and nothing
//! real lives there otherwise. The documentation ranges are blocked instead of
//! public because nothing on the internet answers there — anything that does
//! is a local misconfiguration or a trick.
//!
//! IPv6 forms that embed an IPv4 address are judged by that address:
//! v4-mapped `::ffff:a.b.c.d`, v4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96`
//! (and the local-use `64:ff9b:1::/48`, never better than `Lan`), 6to4
//! `2002:AABB:CCDD::/16` and Teredo `2001::/32` (the client address, never
//! better than `Blocked` when it is not public).
//!
//! Host strings are parsed the way a browser's URL parser (WHATWG) does before
//! classifying, so `2852039166`, `0xA9FEA9FE`, `0251.0376.0251.0376`,
//! `169.254.43518`, `0xa9.254.0251.0xfe`, full-width digits, a trailing dot and
//! percent-encoding all land on 169.254.169.254 → `Blocked`. A host that ends
//! in a number but is not a valid IPv4 address is something a browser refuses;
//! it is `Blocked` rather than guessed at.
//!
//! # Actions
//!
//! `action_class` is the single classification the gate uses. `js` is an
//! *interaction*, not a read: a page script can do anything a click can —
//! submit a form, call the site's API as the signed-in page — so it asks by
//! default (rule `browser:js <host>`). The read-only ways to look at a page are
//! `snapshot`, `read` and `query`. A dialog is an interaction when accepted
//! (gated as `dialog_accept`, rule `browser:dialog_accept <host>`) and a read
//! when dismissed (`dialog`; see `dialog_class`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// What kind of network place a host is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HostClass {
    /// 127.0.0.0/8, ::1, `localhost`, `*.localhost`. Allowed: local dev servers
    /// are the core use case.
    Loopback,
    /// RFC1918, CGNAT 100.64/10, ULA fc00::/7, `.local`, `.internal`, `.lan`,
    /// `.home.arpa`, single-label names. Needs per-host approval.
    Lan,
    /// Cloud metadata, link-local, reserved, multicast, unspecified, and every
    /// alternate encoding of them. Never reachable.
    Blocked,
    /// Everything else.
    Public,
}

/// The class of an IP (after unwrapping v4-mapped / v4-compatible v6).
pub fn classify_ip(ip: IpAddr) -> HostClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> HostClass {
    let o = ip.octets();
    let in_net = |net: [u8; 4], bits: u32| -> bool {
        let mask = if bits == 0 {
            0
        } else {
            u32::MAX << (32 - bits)
        };
        (u32::from(ip) & mask) == (u32::from(Ipv4Addr::from(net)) & mask)
    };
    // Blocked first: the metadata carve-out has to win over CGNAT.
    if in_net([0, 0, 0, 0], 8) // "this network"; 0.0.0.0 reaches loopback
        || in_net([169, 254, 0, 0], 16) // link-local: 169.254.169.254 etc.
        || ip == Ipv4Addr::new(100, 100, 100, 200) // Alibaba Cloud metadata
        || in_net([224, 0, 0, 0], 4) // multicast
        || in_net([240, 0, 0, 0], 4) // reserved + 255.255.255.255
        || in_net([192, 0, 0, 0], 24) // IETF protocol assignments
        || in_net([192, 0, 2, 0], 24) // TEST-NET-1
        || in_net([198, 51, 100, 0], 24) // TEST-NET-2
        || in_net([203, 0, 113, 0], 24)
    // TEST-NET-3
    {
        return HostClass::Blocked;
    }
    if o[0] == 127 {
        return HostClass::Loopback;
    }
    if in_net([10, 0, 0, 0], 8)
        || in_net([172, 16, 0, 0], 12)
        || in_net([192, 168, 0, 0], 16)
        || in_net([100, 64, 0, 0], 10)
    {
        return HostClass::Lan;
    }
    HostClass::Public
}

fn classify_v6(ip: Ipv6Addr) -> HostClass {
    let s = ip.segments();
    let low32 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if ip.is_loopback() {
        return HostClass::Loopback;
    }
    if ip.is_unspecified() {
        return HostClass::Blocked;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return classify_v4(v4);
    }
    // v4-compatible ::a.b.c.d (deprecated, but parsers still route it).
    if s[..6].iter().all(|&x| x == 0) {
        return classify_v4(low32);
    }
    // NAT64 well-known prefix: the v4 address is what is reached.
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return classify_v4(low32);
    }
    // Local-use NAT64 (RFC 8215): an operator-chosen embedding inside the
    // site, so never better than LAN.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 {
        return match classify_v4(low32) {
            HostClass::Public => HostClass::Lan,
            c => c,
        };
    }
    // 6to4: 2002:AABB:CCDD::/48 tunnels to AA.BB.CC.DD.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        return classify_v4(v4);
    }
    // Teredo 2001:0::/32: the client's v4 is stored inverted in the low 32 bits.
    if s[0] == 0x2001 && s[1] == 0 {
        return match classify_v4(Ipv4Addr::from(!u32::from(low32))) {
            HostClass::Public => HostClass::Public,
            _ => HostClass::Blocked,
        };
    }
    if ip == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254) {
        return HostClass::Blocked; // AWS IMDS over IPv6
    }
    if (s[0] & 0xffc0) == 0xfe80 // link-local
        || (s[0] & 0xff00) == 0xff00 // multicast
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || s[..4] == [0x100, 0, 0, 0]
    // discard-only 100::/64
    {
        return HostClass::Blocked;
    }
    // fc00::/7 unique local, and the deprecated site-local fec0::/10 that some
    // old networks still hand out: both are "inside the site".
    if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfec0 {
        return HostClass::Lan;
    }
    HostClass::Public
}

/// Cloud metadata services reachable by name. Their addresses are blocked
/// already; the names are blocked too so a resolver that maps them (GCE's
/// does) is never even asked.
const METADATA_NAMES: &[&str] = &[
    "metadata",
    "metadata.google.internal",
    "metadata.goog",
    "instance-data",
    "instance-data.ec2.internal",
    "metadata.azure.com",
    "metadata.azure.internal",
    "metadata.tencentyun.com",
    "metadata.platformequinix.com",
];

/// Suffixes that only ever mean "on this network".
const LAN_SUFFIXES: &[&str] = &[
    "local",
    "lan",
    "internal",
    "home.arpa",
    "intranet",
    "corp",
    "home",
    "localdomain",
    "private",
];

/// Lowercase, strip brackets, one trailing dot and surrounding whitespace —
/// the comparable spelling of a host for rules and approvals.
pub fn normalize_host(host: &str) -> String {
    let h = host.trim();
    let h = h
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(h);
    let h = h.strip_suffix('.').unwrap_or(h);
    h.to_lowercase()
}

/// Percent-decode a host the way the URL parser does before anything else.
/// `None` when the result is not UTF-8 (the browser refuses such a host).
fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The slice of UTS #46 mapping that matters for addresses: full-width ASCII
/// (U+FF01..U+FF5E) and the ideographic full stops all become ASCII, so
/// `１２７。０。０。１` is the 127.0.0.1 a browser would make of it.
fn fold_width(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3002}' | '\u{FF0E}' | '\u{FF61}' => '.',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            c => c,
        })
        .collect()
}

/// One IPv4 "number" in the WHATWG sense: `0x..` hex (empty = 0), a leading
/// `0` octal, else decimal.
fn ipv4_number(part: &str) -> Option<u64> {
    if part.is_empty() {
        return None;
    }
    let (digits, radix) =
        if let Some(h) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            (h, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (part, 10)
        };
    if digits.is_empty() {
        return Some(0);
    }
    if !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    // Saturate instead of overflowing: anything this big is out of range.
    Some(u64::from_str_radix(digits, radix).unwrap_or(u64::MAX))
}

/// Whether the WHATWG host parser would treat this as an IPv4 address ("ends
/// in a number"), in which case failure to parse is a hard error, not a name.
fn ends_in_a_number(host: &str) -> bool {
    let mut parts: Vec<&str> = host.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    !last.is_empty() && (last.chars().all(|c| c.is_ascii_digit()) || ipv4_number(last).is_some())
}

/// The WHATWG IPv4 parser: 1-4 parts, every part but the last ≤ 255, the last
/// filling the remaining bytes.
fn parse_whatwg_ipv4(host: &str) -> Option<Ipv4Addr> {
    let mut parts: Vec<&str> = host.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let nums: Vec<u64> = parts
        .iter()
        .map(|p| ipv4_number(p))
        .collect::<Option<_>>()?;
    let (last, init) = nums.split_last()?;
    if init.iter().any(|&n| n > 255) {
        return None;
    }
    if *last >= 256u64.pow(5 - nums.len() as u32) {
        return None;
    }
    let mut v: u64 = *last;
    for (i, n) in init.iter().enumerate() {
        v += n * 256u64.pow(3 - i as u32);
    }
    Some(Ipv4Addr::from(v as u32))
}

/// The class of a host as written in a URL, without DNS: IP literals in any
/// encoding (decimal, hex, octal, short forms, bracketed v6) and known names.
pub fn classify_host(host: &str) -> HostClass {
    let Some(decoded) = percent_decode(host.trim()) else {
        return HostClass::Blocked;
    };
    let folded = fold_width(&decoded);
    let raw = folded.trim();
    // Bracketed (or bare, as a CONNECT target may carry) IPv6.
    if let Some(inner) = raw.strip_prefix('[') {
        let Some(inner) = inner.strip_suffix(']') else {
            return HostClass::Blocked;
        };
        // A zone id (`fe80::1%eth0`) is not something a browser accepts; judge
        // the address without it, which is link-local and blocked anyway.
        let addr = inner.split('%').next().unwrap_or("");
        return match addr.parse::<Ipv6Addr>() {
            Ok(v6) => classify_v6(v6),
            Err(_) => HostClass::Blocked,
        };
    }
    if raw.contains(':') {
        let addr = raw.split('%').next().unwrap_or("");
        return match addr.parse::<Ipv6Addr>() {
            Ok(v6) => classify_v6(v6),
            Err(_) => HostClass::Blocked,
        };
    }
    let h = raw.to_lowercase();
    if h.is_empty() || h == "." {
        return HostClass::Blocked;
    }
    // Forbidden host code points: the browser rejects the URL outright.
    if h.chars()
        .any(|c| c.is_control() || c.is_whitespace() || "#/<>?@[\\]^|%".contains(c) || c == '\u{0}')
    {
        return HostClass::Blocked;
    }
    if ends_in_a_number(&h) {
        return match parse_whatwg_ipv4(&h) {
            Some(v4) => classify_v4(v4),
            None => HostClass::Blocked,
        };
    }
    let name = h.strip_suffix('.').unwrap_or(&h);
    if name.is_empty() || name.contains("..") {
        return HostClass::Blocked;
    }
    if name == "localhost" || name.ends_with(".localhost") || name == "localhost.localdomain" {
        return HostClass::Loopback;
    }
    if METADATA_NAMES.contains(&name) {
        return HostClass::Blocked;
    }
    if !name.contains('.') {
        return HostClass::Lan;
    }
    if LAN_SUFFIXES
        .iter()
        .any(|s| name == *s || name.ends_with(&format!(".{s}")))
    {
        return HostClass::Lan;
    }
    HostClass::Public
}

/// The verdict on one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetVerdict {
    Allow,
    /// Never allowed; the reason is shown to the agent.
    Block(String),
    /// A LAN host nobody approved yet. Carries the host to approve
    /// (rule `browser:lan <host>`).
    NeedsLan(String),
}

/// Per-session network policy. Shared (behind `SharedPolicy`) between the CDP
/// request interception and the filtering proxy, and updated when the user
/// approves a LAN host.
#[derive(Debug, Clone, Default)]
pub struct NetPolicy {
    /// The project folder: `file://` is limited to it.
    pub project: Option<PathBuf>,
    /// This session's download folder: `file://` may also read it.
    pub downloads: Option<PathBuf>,
    /// Lowercased LAN hosts approved by rule or in this session. Entries may
    /// be `*.suffix` patterns (same matching as `site_matches`).
    pub lan_allowed: HashSet<String>,
    /// Loopback ports a page must never reach (the CDP endpoint, the proxy).
    pub blocked_ports: Vec<u16>,
}

pub type SharedPolicy = Arc<RwLock<NetPolicy>>;

impl NetPolicy {
    /// Whether this LAN host was approved (exactly, or by a `*.` entry).
    pub fn lan_approved(&self, host: &str) -> bool {
        let h = normalize_host(host);
        !h.is_empty()
            && self
                .lan_allowed
                .iter()
                .any(|p| normalize_host(p) == h || site_matches(p, &h))
    }

    /// Judge a URL as written (no DNS). Schemes: http/https/ws/wss by host
    /// class; `file:` by path scope; `data:`/`blob:`/`about:blank` allowed;
    /// everything else (chrome:, devtools:, view-source:, extension, …) blocked.
    pub fn check_url(&self, url: &str) -> NetVerdict {
        let Ok(u) = reqwest::Url::parse(url.trim()) else {
            return NetVerdict::Block(format!("`{}` is not a valid URL", clip(url, 120)));
        };
        match u.scheme() {
            "http" | "https" | "ws" | "wss" => {
                let host = u.host_str().unwrap_or("");
                if host.is_empty() {
                    return NetVerdict::Block("a URL with no host".into());
                }
                // The URL parser has already turned every IPv4 spelling into
                // dotted form and bracketed IPv6; classify_host takes both.
                let class = classify_host(host);
                let shown = normalize_host(host);
                match class {
                    HostClass::Blocked => NetVerdict::Block(format!(
                        "{shown} is a cloud-metadata, link-local or reserved address, which the browser never reaches"
                    )),
                    HostClass::Lan if !self.lan_approved(&shown) => NetVerdict::NeedsLan(shown),
                    HostClass::Loopback => {
                        let port = u.port_or_known_default().unwrap_or(0);
                        if self.blocked_ports.contains(&port) {
                            NetVerdict::Block(format!(
                                "port {port} on this machine belongs to OpenLeash's own browser plumbing"
                            ))
                        } else {
                            NetVerdict::Allow
                        }
                    }
                    _ => NetVerdict::Allow,
                }
            }
            "data" | "blob" => NetVerdict::Allow,
            "about" => match u.path() {
                "blank" | "srcdoc" => NetVerdict::Allow,
                p => NetVerdict::Block(format!("`about:{p}` is a browser-internal page")),
            },
            "file" => self.check_file(&u),
            s => NetVerdict::Block(format!(
                "the `{s}:` scheme is not allowed (only http, https, ws, wss, file inside the project, data, blob and about:blank)"
            )),
        }
    }

    fn check_file(&self, u: &reqwest::Url) -> NetVerdict {
        // `file://server/share` is an SMB share: a network fetch with the
        // user's Windows credentials, not a local file.
        match u.host_str() {
            None | Some("") | Some("localhost") => {}
            Some(h) => {
                return NetVerdict::Block(format!(
                    "file://{h}/… is a network share, not a local file"
                ))
            }
        }
        let Ok(path) = u.to_file_path() else {
            return NetVerdict::Block("that file: URL does not name a local path".into());
        };
        let Ok(real) = path.canonicalize() else {
            return NetVerdict::Block(format!("{} does not exist", path.display()));
        };
        for root in [&self.project, &self.downloads].into_iter().flatten() {
            if let Ok(r) = root.canonicalize() {
                if real.starts_with(&r) {
                    return NetVerdict::Allow;
                }
            }
        }
        NetVerdict::Block(format!(
            "{} is outside the project and the download folder; the browser only opens local files from those",
            path.display()
        ))
    }

    /// Judge the addresses a hostname actually resolved to (DNS-rebinding
    /// check). Any blocked address blocks; a LAN address needs the host
    /// approved. A name that is not itself a loopback name (`localhost`) but
    /// resolves to loopback is treated like a LAN host — that is exactly the
    /// rebinding attack against local dev servers, and `lvh.me`-style names
    /// still work after one approval. No addresses at all blocks.
    pub fn check_resolved(&self, host: &str, ips: &[IpAddr]) -> NetVerdict {
        let shown = normalize_host(host);
        if ips.is_empty() {
            return NetVerdict::Block(format!("{shown} did not resolve to any address"));
        }
        let name_class = classify_host(host);
        if name_class == HostClass::Blocked {
            return NetVerdict::Block(format!("{shown} is never reachable"));
        }
        let mut needs_lan = false;
        for ip in ips {
            match classify_ip(*ip) {
                HostClass::Blocked => {
                    return NetVerdict::Block(format!(
                        "{shown} resolves to {ip}, a metadata, link-local or reserved address"
                    ))
                }
                HostClass::Lan => needs_lan = true,
                HostClass::Loopback if name_class != HostClass::Loopback => needs_lan = true,
                _ => {}
            }
        }
        if needs_lan && !self.lan_approved(&shown) {
            return NetVerdict::NeedsLan(shown);
        }
        NetVerdict::Allow
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// How the permission layer treats a browser action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    /// Looks, navigates, waits, reads logs. Free; allowed in plan mode and for
    /// read-only subagents.
    Read,
    /// Changes the page or the site (click, type, fill, select, check, drag,
    /// press, upload, dialog accept, js). Asks by default.
    Interact,
    /// Raw CDP. Always asks; needs developer mode.
    Developer,
}

/// `None` for an unknown action.
pub fn action_class(action: &str) -> Option<ActionClass> {
    match action {
        "navigate" | "back" | "forward" | "reload" | "snapshot" | "read" | "query" | "info"
        | "screenshot" | "diff" | "wait_for" | "hover" | "scroll" | "tabs" | "tab_new"
        | "tab_switch" | "tab_close" | "emulate" | "pdf" | "console" | "network"
        | "network_body" | "cookies" | "storage" | "perf" | "downloads" => Some(ActionClass::Read),
        "click" | "type" | "fill" | "press" | "select" | "check" | "uncheck" | "drag"
        | "upload" | "js" | "dialog_accept" => Some(ActionClass::Interact),
        // Dismissing a dialog only gets it out of the way; the runner gates
        // an accept separately, as `dialog_accept`.
        "dialog" => Some(ActionClass::Read),
        "cdp" => Some(ActionClass::Developer),
        _ => None,
    }
}

/// A dialog: accepting one confirms whatever the page asked ("Delete this?"),
/// dismissing it only gets it out of the way.
pub fn dialog_class(accept: bool) -> ActionClass {
    if accept {
        ActionClass::Interact
    } else {
        ActionClass::Read
    }
}

/// Facts about an interaction's target, gathered from the page before acting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetFacts {
    pub tag: String,
    pub role: String,
    /// `type` attribute of an input/button.
    pub input_type: String,
    /// Accessible name / visible text / value / aria-label, trimmed.
    pub text: String,
    /// id + name + class attributes, joined (submit buttons are often only
    /// identifiable by these).
    pub attrs: String,
    pub autocomplete: String,
    /// The enclosing form, if any.
    pub in_form: bool,
    pub form_action: String,
    /// The `method` *attribute*, lowercased; empty when absent. (Not the DOM
    /// property, which reports "get" for a form that never said so — a
    /// script-driven form with no method is not known to be a harmless GET.)
    pub form_method: String,
    /// The enclosing form contains a password / payment-card field.
    pub form_has_password: bool,
    pub form_has_payment: bool,
}

/// Words that make an action sensitive wherever they appear (visible text,
/// accessible name, id/name/class, form action).
const STRONG_WORDS: &[&str] = &[
    "pay",
    "pay now",
    "payment",
    "payments",
    "checkout",
    "buy",
    "buy now",
    "purchase",
    "place order",
    "place your order",
    "order now",
    "complete order",
    "submit order",
    "confirm order",
    "complete purchase",
    "confirm purchase",
    "confirm payment",
    "delete",
    "remove",
    "destroy",
    "erase",
    "wipe",
    "transfer",
    "send money",
    "withdraw",
    "donate",
    "subscribe",
    "unsubscribe",
    "cancel subscription",
    "close account",
    "delete account",
    "deactivate",
    "terminate",
    "revoke",
    "authorize",
    "authorise",
    "grant access",
    "merge",
    "deploy",
    "publish",
    "factory reset",
    "reset password",
    "change password",
    // multilingual-lite (Latin scripts, matched on word boundaries)
    "kaufen",
    "jetzt kaufen",
    "bezahlen",
    "zahlungspflichtig bestellen",
    "löschen",
    "entfernen",
    "acheter",
    "payer",
    "supprimer",
    "commander",
    "comprar",
    "pagar",
    "eliminar",
    "borrar",
    "acquista",
    "paga",
    "elimina",
    "kopen",
    "betalen",
    "verwijderen",
    "kup",
    "zapłać",
    "usuń",
    "купить",
    "оплатить",
    "удалить",
];

/// Scripts without word spaces: matched as substrings.
const CJK_WORDS: &[&str] = &[
    "購入",
    "支払",
    "支払い",
    "削除",
    "注文",
    "決済",
    "购买",
    "支付",
    "删除",
    "下单",
    "付款",
    "结账",
    "結帳",
    "刪除",
    "결제",
    "구매",
    "삭제",
    "주문",
];

/// Words that commit a form but are harmless on a plain GET search form.
const COMMIT_WORDS: &[&str] = &[
    "submit", "send", "confirm", "post", "apply", "sign up", "register", "senden", "absenden",
    "envoyer", "enviar", "invia",
];

fn word_regex(words: &[&str]) -> regex::Regex {
    let alts: Vec<String> = words
        .iter()
        .map(|w| regex::escape(w).replace(' ', r"\s+"))
        .collect();
    regex::Regex::new(&format!(
        r"(?i)(?:^|[^\p{{L}}\p{{N}}])({})(?:$|[^\p{{L}}\p{{N}}])",
        alts.join("|")
    ))
    .expect("static word list compiles")
}

fn strong_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| word_regex(STRONG_WORDS))
}

fn commit_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| word_regex(COMMIT_WORDS))
}

/// Split identifiers into words: `placeOrderBtn` / `place_order-btn` →
/// `place Order Btn`, so the word lists match class names and ids too.
fn identifier_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' || c == '/' || c == '#' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.push(c);
    }
    out
}

fn find_word(re: &regex::Regex, s: &str) -> Option<String> {
    re.captures(s)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())
}

fn find_cjk(s: &str) -> Option<String> {
    CJK_WORDS
        .iter()
        .find(|w| s.contains(*w))
        .map(|w| w.to_string())
}

fn is_password_field(f: &TargetFacts) -> bool {
    let ac = f.autocomplete.to_ascii_lowercase();
    f.input_type.eq_ignore_ascii_case("password")
        || ac.contains("current-password")
        || ac.contains("new-password")
}

fn is_payment_field(f: &TargetFacts) -> bool {
    let ac = f.autocomplete.to_ascii_lowercase();
    if ac.split_whitespace().any(|t| t.starts_with("cc-")) {
        return true;
    }
    let ids = identifier_words(&f.attrs).to_lowercase();
    // Short codes only as whole words (`disc-scroll` must not read as `csc`);
    // the long, unambiguous ones also inside a squashed identifier.
    if ids
        .split(|c: char| !c.is_alphanumeric())
        .any(|t| matches!(t, "cvc" | "cvv" | "cvv2" | "csc" | "iban" | "ccnum"))
    {
        return true;
    }
    let squashed: String = ids.chars().filter(|c| c.is_alphanumeric()).collect();
    [
        "cardnumber",
        "ccnumber",
        "creditcard",
        "cardcvc",
        "securitycode",
        "cardexpiry",
    ]
    .iter()
    .any(|k| squashed.contains(k))
}

fn is_otp_field(f: &TargetFacts) -> bool {
    f.autocomplete
        .to_ascii_lowercase()
        .contains("one-time-code")
}

/// Why acting on this target is sensitive (submit/pay/buy/delete/transfer
/// semantics, password or payment fields), or `None`.
///
/// `action` is the tool action. `press` is assumed to be a submitting key
/// (Enter): the caller should only ask about the keys that can submit.
pub fn sensitive_reason(action: &str, f: &TargetFacts) -> Option<String> {
    let action = action.to_ascii_lowercase();
    match action.as_str() {
        "upload" => return Some("it uploads a file from this machine to the site".into()),
        "js" | "cdp" => return None,
        _ => {}
    }
    if is_password_field(f) {
        return Some(match action.as_str() {
            "type" | "fill" => "it types into a password field".into(),
            _ => "the target is a password field".into(),
        });
    }
    if is_payment_field(f) {
        return Some("the target is a payment-card field".into());
    }
    if is_otp_field(f) && matches!(action.as_str(), "type" | "fill" | "press") {
        return Some("it enters a one-time verification code".into());
    }
    // Any interaction inside a credential/payment form can trigger page
    // handlers (including typing the username), not just its submit button.
    if f.form_has_password {
        return Some("the target is in a form that contains a password field".into());
    }
    if f.form_has_payment {
        return Some("the target is in a form with payment-card fields".into());
    }
    // Typing ordinary text changes nothing until something submits it — that
    // later click or Enter is what gets judged.
    if matches!(action.as_str(), "type" | "fill") {
        return None;
    }
    let tag = f.tag.to_ascii_lowercase();
    let ty = f.input_type.to_ascii_lowercase();
    let submits = f.in_form
        && match action.as_str() {
            "click" => {
                (tag == "button" && (ty.is_empty() || ty == "submit"))
                    || (tag == "input" && (ty == "submit" || ty == "image"))
            }
            // Enter in a text field submits its form.
            "press" => tag == "input" || tag == "select" || (tag == "button" && ty != "button"),
            _ => false,
        };
    let text = f.text.trim();
    let ids = identifier_words(&f.attrs);
    let label = clip(text, 60);
    if let Some(w) = find_word(strong_re(), text).or_else(|| find_cjk(text)) {
        return Some(format!("the target says “{label}” (“{w}”)"));
    }
    if let Some(w) = find_word(strong_re(), &ids).or_else(|| find_cjk(&ids)) {
        return Some(format!(
            "the target's id/name/class says “{w}” ({})",
            clip(f.attrs.trim(), 60)
        ));
    }
    if !submits {
        return None;
    }
    let action_words = identifier_words(&form_action_path(&f.form_action));
    if let Some(w) = find_word(strong_re(), &action_words) {
        return Some(format!(
            "it submits a form to {} (“{w}”)",
            clip(&f.form_action, 80)
        ));
    }
    let method = f.form_method.trim().to_ascii_lowercase();
    if method == "post" {
        return Some("it submits a form that posts data to the site".into());
    }
    if method != "get" {
        if let Some(w) = find_word(commit_re(), text).or_else(|| find_word(commit_re(), &ids)) {
            return Some(format!("it submits a form (“{w}”)"));
        }
    }
    None
}

/// The path + query of a form action, without scheme and host (a host like
/// `pay.example.com` says nothing about what this particular form does).
fn form_action_path(action: &str) -> String {
    match reqwest::Url::parse(action) {
        Ok(u) => format!("{} {}", u.path(), u.query().unwrap_or("")),
        Err(_) => action.to_string(),
    }
}

/// The longest file name `sanitize_filename` produces, in bytes. Comfortably
/// under every filesystem's 255 and leaves room for ` (99)`.
const MAX_NAME_BYTES: usize = 150;

fn is_reserved_device(stem: &str) -> bool {
    let s = stem.trim_end_matches([' ', '.']).to_ascii_uppercase();
    if matches!(
        s.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    // COM0-9 / LPT0-9, and the superscript digits Windows also reserves.
    let up = stem.trim_end_matches([' ', '.']).to_uppercase();
    for p in ["COM", "LPT"] {
        if let Some(rest) = up.strip_prefix(p) {
            let mut cs = rest.chars();
            if let (Some(c), None) = (cs.next(), cs.next()) {
                if c.is_ascii_digit() || matches!(c, '¹' | '²' | '³') {
                    return true;
                }
            }
        }
    }
    false
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Split `name` into stem and extension (without the dot); the extension only
/// counts when short and plain, so `a.very long sentence` keeps its words.
fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let ext = &name[i + 1..];
            if !ext.is_empty()
                && ext.len() <= 16
                && ext
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                (&name[..i], Some(ext))
            } else {
                (name, None)
            }
        }
        _ => (name, None),
    }
}

/// A safe file name for a download: no separators, no reserved names, no
/// control characters, bounded length.
pub fn sanitize_filename(name: &str) -> String {
    // Only the last path component: `../../etc/passwd` is `passwd`.
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| {
            !c.is_control()
                // Bidi controls and BOM: `invoice\u{202E}fdp.exe` spoofs a PDF.
                && !matches!(*c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
        .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return "download".into();
    }
    let (stem, ext) = split_ext(trimmed);
    let stem = if is_reserved_device(stem.split('.').next().unwrap_or(stem)) {
        format!("_{stem}")
    } else {
        stem.to_string()
    };
    let ext_len = ext.map(|e| e.len() + 1).unwrap_or(0);
    let room = MAX_NAME_BYTES.saturating_sub(ext_len).max(1);
    let stem = truncate_bytes(&stem, room).trim_end_matches(['.', ' ']);
    let stem = if stem.is_empty() { "download" } else { stem };
    match ext {
        Some(e) => format!("{stem}.{}", truncate_bytes(e, 16)),
        None => stem.to_string(),
    }
}

/// Where a download named `suggested` lands inside `dir`: sanitised, never
/// outside `dir`, and suffixed ` (2)`, ` (3)` … rather than overwriting.
///
/// The existence check races a concurrent writer; the caller should create
/// the file with `create_new` and retry on `AlreadyExists`.
pub fn download_target(dir: &Path, suggested: &str) -> PathBuf {
    let name = sanitize_filename(suggested);
    let taken = |p: &Path| p.symlink_metadata().is_ok();
    let first = dir.join(&name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = split_ext(&name);
    for n in 2..10_000u32 {
        let suffix = format!(" ({n})");
        let room =
            MAX_NAME_BYTES.saturating_sub(suffix.len() + ext.map(|e| e.len() + 1).unwrap_or(0));
        let s = truncate_bytes(stem, room.max(1)).trim_end_matches(['.', ' ']);
        let candidate = match ext {
            Some(e) => format!("{s}{suffix}.{e}"),
            None => format!("{s}{suffix}"),
        };
        let p = dir.join(candidate);
        if !taken(&p) {
            return p;
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("download-{nanos}"))
}

/// Resolve an upload path against the project and refuse anything outside it
/// (after canonicalising, so `..` and symlinks can't escape).
///
/// Also refused, inside the project: anything under `.git` (its `config` can
/// carry remote credentials, and no page needs repository internals) and env
/// files (`permissions::is_env_path`: `.env`, `.env.local`, … — the same
/// credentials the read gate keeps from leaving the machine; an upload is an
/// exfiltration channel with a nicer name). Only regular files.
pub fn upload_allowed(project: &Path, path: &str) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("no file named to upload".into());
    }
    let given = Path::new(path);
    let p = if given.is_absolute() {
        given.to_path_buf()
    } else {
        project.join(given)
    };
    let root = project
        .canonicalize()
        .map_err(|e| format!("the project folder can't be resolved: {e}"))?;
    let real = p.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    let Ok(rel) = real.strip_prefix(&root) else {
        return Err(format!(
            "{path} is outside the project folder; the browser only uploads files from the project"
        ));
    };
    if rel.components().any(|c| match c {
        Component::Normal(s) => s.to_string_lossy().eq_ignore_ascii_case(".git"),
        _ => false,
    }) {
        return Err(format!(
            "{path} is inside the repository's .git folder, which is never uploaded"
        ));
    }
    if super::permissions::is_env_path(&real.to_string_lossy()) {
        return Err(format!(
            "{path} is an environment file (credentials); it is never uploaded"
        ));
    }
    let meta = std::fs::metadata(&real).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    Ok(real)
}

/// The lowercased host a permission rule is keyed on, or `None` (file:, about:).
pub fn site_of(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https" | "ws" | "wss") {
        return None;
    }
    let h = normalize_host(u.host_str()?);
    (!h.is_empty()).then_some(h)
}

/// Whether a rule's host pattern covers `host` (`example.com` exact,
/// `*.example.com` subdomains and the apex).
///
/// Never a suffix trick: `*.example.com` does not cover `evil-example.com` or
/// `example.com.evil.com`, and a bare `*` (or any other `*` placement) covers
/// nothing — "every site" is `browser *`, which deliberately does not reach
/// sensitive actions.
pub fn site_matches(pattern: &str, host: &str) -> bool {
    let p = normalize_host(pattern);
    let h = normalize_host(host);
    if p.is_empty() || h.is_empty() || h.contains('*') {
        return false;
    }
    if let Some(base) = p.strip_prefix("*.") {
        if base.is_empty() || base.contains('*') || base.starts_with('.') {
            return false;
        }
        return h == base || h.ends_with(&format!(".{base}"));
    }
    !p.contains('*') && p == h
}

/// Whether a header name carries a credential.
pub fn is_secret_header(name: &str) -> bool {
    let l = name.trim().to_ascii_lowercase();
    matches!(
        l.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "set-cookie2"
    ) || [
        "token", "secret", "api-key", "api_key", "apikey", "auth", "cookie", "password", "session",
        "csrf", "xsrf",
    ]
    .iter()
    .any(|k| l.contains(k))
}

const REDACTED: &str = "[redacted]";

/// Redact credential-bearing headers (`Authorization`, `Cookie`,
/// `Set-Cookie`, `Proxy-Authorization`, …) in a CDP headers object, in place.
///
/// Accepts the shapes CDP uses: an object `{name: value}`, an array of
/// `{name, value}` entries, and raw `headersText` (`Name: value` lines).
pub fn redact_headers(headers: &mut Value) {
    match headers {
        Value::Object(o) => {
            for (k, v) in o.iter_mut() {
                if is_secret_header(k) {
                    *v = Value::String(REDACTED.into());
                }
            }
        }
        Value::Array(a) => {
            for entry in a.iter_mut() {
                let secret = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(is_secret_header);
                if secret {
                    if let Some(v) = entry.get_mut("value") {
                        *v = Value::String(REDACTED.into());
                    }
                }
            }
        }
        Value::String(s) => {
            let out: Vec<String> = s
                .split("\r\n")
                .map(|line| match line.split_once(':') {
                    Some((k, _)) if is_secret_header(k) => format!("{k}: {REDACTED}"),
                    _ => line.to_string(),
                })
                .collect();
            *s = out.join("\r\n");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch folder removed on drop (no `tempfile` dependency here).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!("ol-bpol-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ip_table() {
        use HostClass::*;
        let table: &[(&str, HostClass)] = &[
            ("127.0.0.1", Loopback),
            ("127.255.0.9", Loopback),
            ("::1", Loopback),
            ("10.1.2.3", Lan),
            ("172.16.0.1", Lan),
            ("172.31.255.255", Lan),
            ("172.32.0.1", Public),
            ("192.168.1.1", Lan),
            ("100.64.0.1", Lan),
            ("100.127.255.255", Lan),
            ("100.128.0.1", Public),
            ("100.100.100.200", Blocked),
            ("100.100.100.199", Lan),
            ("169.254.169.254", Blocked),
            ("169.254.0.23", Blocked),
            ("0.0.0.0", Blocked),
            ("0.1.2.3", Blocked),
            ("255.255.255.255", Blocked),
            ("224.0.0.251", Blocked),
            ("239.255.255.250", Blocked),
            ("240.0.0.1", Blocked),
            ("192.0.0.170", Blocked),
            ("192.0.2.1", Blocked),
            ("198.51.100.7", Blocked),
            ("203.0.113.9", Blocked),
            ("198.18.0.1", Public),
            ("8.8.8.8", Public),
            ("1.1.1.1", Public),
            ("::", Blocked),
            ("fe80::1", Blocked),
            ("febf::1", Blocked),
            ("fd00:ec2::254", Blocked),
            ("fd00:ec2::253", Lan),
            ("fc00::1", Lan),
            ("fd12:3456::1", Lan),
            ("fec0::1", Lan),
            ("ff02::1", Blocked),
            ("2001:db8::1", Blocked),
            ("100::1", Blocked),
            ("2606:4700::1111", Public),
            ("::ffff:169.254.169.254", Blocked),
            ("::ffff:127.0.0.1", Loopback),
            ("::ffff:192.168.0.1", Lan),
            ("::ffff:8.8.8.8", Public),
            ("::169.254.169.254", Blocked),
            ("::127.0.0.1", Loopback),
            ("64:ff9b::a9fe:a9fe", Blocked),
            ("64:ff9b::10.0.0.1", Lan),
            ("64:ff9b::8.8.8.8", Public),
            ("64:ff9b:1::8.8.8.8", Lan),
            ("64:ff9b:1::169.254.169.254", Blocked),
            ("2002:a9fe:a9fe::1", Blocked),
            ("2002:7f00:1::", Loopback),
            ("2002:c0a8:101::1", Lan),
            ("2002:0808:0808::1", Public),
        ];
        for (s, want) in table {
            assert_eq!(classify_ip(ip(s)), *want, "{s}");
        }
        // Teredo: client 169.254.169.254 stored inverted.
        let inv = !u32::from(Ipv4Addr::new(169, 254, 169, 254));
        let t = Ipv6Addr::new(
            0x2001,
            0,
            0x4136,
            0xe378,
            0x8000,
            0x63bf,
            (inv >> 16) as u16,
            inv as u16,
        );
        assert_eq!(classify_v6(t), Blocked);
        let inv = !u32::from(Ipv4Addr::new(8, 8, 8, 8));
        let t = Ipv6Addr::new(
            0x2001,
            0,
            0x4136,
            0xe378,
            0x8000,
            0x63bf,
            (inv >> 16) as u16,
            inv as u16,
        );
        assert_eq!(classify_v6(t), Public);
    }

    #[test]
    fn host_encodings_of_metadata_are_all_blocked() {
        for h in [
            "169.254.169.254",
            "2852039166",
            "0xA9FEA9FE",
            "0xa9fea9fe",
            "0251.0376.0251.0376",
            "169.254.43518",
            "169.16689662",
            "0xa9.254.0251.0xfe",
            "169.254.169.254.",
            "[::ffff:169.254.169.254]",
            "[::ffff:a9fe:a9fe]",
            "[fe80::1%25eth0]",
            "[fd00:ec2::254]",
            "%31%36%39.254.169.254",
            "１６９。２５４。１６９。２５４",
            "metadata.google.internal",
            "METADATA.Google.Internal.",
            "metadata",
            "metadata.goog",
            "instance-data",
            "instance-data.ec2.internal",
            "100.100.100.200",
            "0",
            "0x0",
            "0.0.0.0",
        ] {
            assert_eq!(classify_host(h), HostClass::Blocked, "{h}");
        }
    }

    #[test]
    fn host_table() {
        use HostClass::*;
        let table: &[(&str, HostClass)] = &[
            ("localhost", Loopback),
            ("LOCALHOST.", Loopback),
            ("app.localhost", Loopback),
            ("127.1", Loopback),
            ("0x7f.1", Loopback),
            ("2130706433", Loopback),
            ("017700000001", Loopback),
            ("[::1]", Loopback),
            ("::1", Loopback),
            ("printer.local", Lan),
            ("nas.lan", Lan),
            ("db.internal", Lan),
            ("router.home.arpa", Lan),
            ("wiki.intranet", Lan),
            ("git.corp", Lan),
            ("intranet", Lan),
            ("jenkins", Lan),
            ("192.168.1.10", Lan),
            ("10.0.0.1", Lan),
            ("0xc0a80101", Lan),
            ("[fd00::1]", Lan),
            ("example.com", Public),
            ("Example.COM.", Public),
            ("8.8.8.8", Public),
            ("134744072", Public),
            ("xn--bcher-kva.example", Public),
            ("local.example.com", Public),
            // A browser rejects these; never guessed at.
            ("1.2.3.4.5", Blocked),
            ("256.1.1.1", Blocked),
            ("4294967296", Blocked),
            ("09.1.1.1", Blocked),
            ("foo.123", Blocked),
            ("0xgg", Lan), // not numeric: a single-label name
            ("", Blocked),
            ("[::1", Blocked),
            ("[nope]", Blocked),
            ("a b.com", Blocked),
            ("a..b.com", Blocked),
            ("%ff.com", Blocked),
        ];
        for (h, want) in table {
            assert_eq!(classify_host(h), *want, "{h}");
        }
    }

    fn policy() -> NetPolicy {
        NetPolicy {
            blocked_ports: vec![9222, 4555],
            ..Default::default()
        }
    }

    #[test]
    fn check_url_by_scheme_and_class() {
        let p = policy();
        for u in [
            "https://example.com/",
            "http://localhost:5173/app",
            "ws://127.0.0.1:3000/hmr",
            "wss://example.com/socket",
            "data:text/html,<h1>hi</h1>",
            "blob:https://example.com/0b4f",
            "about:blank",
            "about:srcdoc",
        ] {
            assert_eq!(p.check_url(u), NetVerdict::Allow, "{u}");
        }
        for u in [
            "http://169.254.169.254/latest/meta-data/",
            "http://0xA9FEA9FE/",
            "http://2852039166/",
            "http://[::ffff:169.254.169.254]/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://localhost:9222/json",
            "ws://127.0.0.1:9222/devtools/browser/x",
            "http://[::1]:4555/",
            "chrome://settings",
            "chrome-extension://abc/x.html",
            "devtools://devtools/bundled/inspector.html",
            "view-source:https://example.com",
            "javascript:alert(1)",
            "filesystem:https://example.com/temporary/x",
            "ftp://example.com/x",
            "about:settings",
            "not a url",
            "file://server/share/x.html",
        ] {
            assert!(
                matches!(p.check_url(u), NetVerdict::Block(_)),
                "{u}: {:?}",
                p.check_url(u)
            );
        }
        assert_eq!(
            p.check_url("http://192.168.1.20:8080/"),
            NetVerdict::NeedsLan("192.168.1.20".into())
        );
        assert_eq!(
            p.check_url("http://NAS.local/"),
            NetVerdict::NeedsLan("nas.local".into())
        );
        let mut ok = policy();
        ok.lan_allowed.insert("192.168.1.20".into());
        ok.lan_allowed.insert("*.home.arpa".into());
        assert_eq!(ok.check_url("http://192.168.1.20:8080/"), NetVerdict::Allow);
        assert_eq!(ok.check_url("http://router.home.arpa/"), NetVerdict::Allow);
        assert!(matches!(
            ok.check_url("http://192.168.1.21/"),
            NetVerdict::NeedsLan(_)
        ));
    }

    #[test]
    fn file_urls_stay_inside_the_project_and_downloads() {
        let tmp = TempDir::new();
        let project = tmp.path().join("proj");
        let downloads = tmp.path().join("dl");
        let outside = tmp.path().join("outside");
        for d in [&project, &downloads, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(project.join("index.html"), "x").unwrap();
        std::fs::write(project.join("a b.html"), "x").unwrap();
        std::fs::write(downloads.join("report.pdf"), "x").unwrap();
        std::fs::write(outside.join("secret.html"), "x").unwrap();
        let p = NetPolicy {
            project: Some(project.clone()),
            downloads: Some(downloads.clone()),
            ..Default::default()
        };
        let url = |path: &Path| reqwest::Url::from_file_path(path).unwrap().to_string();
        assert_eq!(
            p.check_url(&url(&project.join("index.html"))),
            NetVerdict::Allow
        );
        assert_eq!(
            p.check_url(&url(&project.join("a b.html"))),
            NetVerdict::Allow
        );
        assert_eq!(
            p.check_url(&url(&downloads.join("report.pdf"))),
            NetVerdict::Allow
        );
        assert!(matches!(
            p.check_url(&url(&outside.join("secret.html"))),
            NetVerdict::Block(_)
        ));
        // `..` out of the project (the URL parser normalises it; the result
        // is still outside).
        let sneaky = format!(
            "{}/../outside/secret.html",
            url(&project).trim_end_matches('/')
        );
        assert!(
            matches!(p.check_url(&sneaky), NetVerdict::Block(_)),
            "{sneaky}"
        );
        // Percent-encoded `..`.
        let sneaky = format!(
            "{}/%2e%2e/outside/secret.html",
            url(&project).trim_end_matches('/')
        );
        assert!(
            matches!(p.check_url(&sneaky), NetVerdict::Block(_)),
            "{sneaky}"
        );
        // Missing file, and no roots at all.
        assert!(matches!(
            p.check_url(&url(&project.join("nope.html"))),
            NetVerdict::Block(_)
        ));
        let none = NetPolicy::default();
        assert!(matches!(
            none.check_url(&url(&project.join("index.html"))),
            NetVerdict::Block(_)
        ));
        #[cfg(windows)]
        {
            assert!(matches!(
                p.check_url("file:///C:/Windows/win.ini"),
                NetVerdict::Block(_)
            ));
            assert!(matches!(
                p.check_url("file://server/share/x"),
                NetVerdict::Block(_)
            ));
        }
        #[cfg(unix)]
        {
            let link = project.join("link.html");
            std::os::unix::fs::symlink(outside.join("secret.html"), &link).unwrap();
            assert!(matches!(p.check_url(&url(&link)), NetVerdict::Block(_)));
        }
    }

    #[test]
    fn resolved_addresses_defeat_rebinding() {
        let p = policy();
        assert_eq!(
            p.check_resolved("example.com", &[ip("93.184.216.34")]),
            NetVerdict::Allow
        );
        assert!(matches!(
            p.check_resolved("rebind.test", &[ip("93.184.216.34"), ip("169.254.169.254")]),
            NetVerdict::Block(_)
        ));
        assert_eq!(
            p.check_resolved("Rebind.Test.", &[ip("192.168.0.10")]),
            NetVerdict::NeedsLan("rebind.test".into())
        );
        // A public name pointing at loopback is the rebinding attack on local
        // dev servers: needs approval like a LAN host.
        assert_eq!(
            p.check_resolved("lvh.me", &[ip("127.0.0.1")]),
            NetVerdict::NeedsLan("lvh.me".into())
        );
        assert_eq!(
            p.check_resolved("localhost", &[ip("127.0.0.1"), ip("::1")]),
            NetVerdict::Allow
        );
        assert!(matches!(
            p.check_resolved("example.com", &[]),
            NetVerdict::Block(_)
        ));
        assert!(matches!(
            p.check_resolved("metadata.google.internal", &[ip("8.8.8.8")]),
            NetVerdict::Block(_)
        ));
        let mut ok = policy();
        ok.lan_allowed.insert("lvh.me".into());
        assert_eq!(
            ok.check_resolved("lvh.me", &[ip("127.0.0.1")]),
            NetVerdict::Allow
        );
        // Approval never unblocks metadata.
        ok.lan_allowed.insert("rebind.test".into());
        assert!(matches!(
            ok.check_resolved("rebind.test", &[ip("169.254.169.254")]),
            NetVerdict::Block(_)
        ));
    }

    #[test]
    fn action_classes() {
        for a in [
            "navigate",
            "back",
            "forward",
            "reload",
            "snapshot",
            "read",
            "query",
            "info",
            "screenshot",
            "diff",
            "wait_for",
            "hover",
            "scroll",
            "tabs",
            "tab_new",
            "tab_switch",
            "tab_close",
            "emulate",
            "pdf",
            "console",
            "network",
            "network_body",
            "cookies",
            "storage",
            "perf",
            "downloads",
        ] {
            assert_eq!(action_class(a), Some(ActionClass::Read), "{a}");
        }
        assert_eq!(action_class("dialog"), Some(ActionClass::Read));
        for a in [
            "click",
            "type",
            "fill",
            "press",
            "select",
            "check",
            "uncheck",
            "drag",
            "upload",
            "js",
            "dialog_accept",
        ] {
            assert_eq!(action_class(a), Some(ActionClass::Interact), "{a}");
        }
        assert_eq!(action_class("cdp"), Some(ActionClass::Developer));
        for a in ["", "Click", "eval", "render", "lan"] {
            assert_eq!(action_class(a), None, "{a}");
        }
        assert_eq!(dialog_class(true), ActionClass::Interact);
        assert_eq!(dialog_class(false), ActionClass::Read);
    }

    fn facts(tag: &str, ty: &str, text: &str) -> TargetFacts {
        TargetFacts {
            tag: tag.into(),
            input_type: ty.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn sensitive_targets() {
        let in_form = |mut f: TargetFacts, method: &str| {
            f.in_form = true;
            f.form_method = method.into();
            f
        };
        // (action, facts, sensitive?)
        let cases: Vec<(&str, TargetFacts, bool)> = vec![
            ("click", facts("button", "", "Place order"), true),
            ("click", facts("button", "button", "PAY NOW"), true),
            ("click", facts("a", "", "Delete repository"), true),
            ("click", facts("button", "", "Remove from cart"), true),
            ("click", facts("button", "", "Buy"), true),
            ("click", facts("button", "", "Proceed to checkout"), true),
            ("click", facts("button", "", "Transfer funds"), true),
            ("click", facts("button", "", "Unsubscribe"), true),
            ("click", facts("button", "", "Close account"), true),
            ("click", facts("button", "", "Withdraw"), true),
            ("click", facts("button", "", "Confirm payment"), true),
            ("click", facts("button", "", "Merge pull request"), true),
            ("click", facts("button", "", "Jetzt kaufen"), true),
            ("click", facts("button", "", "Supprimer"), true),
            ("click", facts("button", "", "立即购买"), true),
            ("click", facts("button", "", "削除する"), true),
            (
                "click",
                facts("button", "", "Zahlungspflichtig bestellen"),
                true,
            ),
            (
                "click",
                TargetFacts {
                    tag: "button".into(),
                    attrs: "placeOrderBtn primary".into(),
                    ..Default::default()
                },
                true,
            ),
            (
                "click",
                TargetFacts {
                    tag: "div".into(),
                    role: "button".into(),
                    attrs: "js-delete-item".into(),
                    ..Default::default()
                },
                true,
            ),
            ("type", facts("input", "password", ""), true),
            ("fill", facts("input", "password", ""), true),
            ("click", facts("input", "password", ""), true),
            (
                "fill",
                TargetFacts {
                    tag: "input".into(),
                    autocomplete: "new-password".into(),
                    ..Default::default()
                },
                true,
            ),
            (
                "type",
                TargetFacts {
                    tag: "input".into(),
                    autocomplete: "cc-number".into(),
                    ..Default::default()
                },
                true,
            ),
            (
                "type",
                TargetFacts {
                    tag: "input".into(),
                    attrs: "cardNumber".into(),
                    ..Default::default()
                },
                true,
            ),
            (
                "type",
                TargetFacts {
                    tag: "input".into(),
                    attrs: "card-cvc".into(),
                    ..Default::default()
                },
                true,
            ),
            ("upload", facts("input", "file", ""), true),
            (
                "fill",
                TargetFacts {
                    in_form: true,
                    form_has_password: true,
                    ..facts("input", "text", "Username")
                },
                true,
            ),
            (
                "check",
                TargetFacts {
                    in_form: true,
                    form_has_password: true,
                    ..facts("input", "checkbox", "Remember me")
                },
                true,
            ),
            (
                "fill",
                TargetFacts {
                    in_form: true,
                    form_has_payment: true,
                    ..facts("input", "text", "Name")
                },
                true,
            ),
            // Submitting: password form, payment form, POST, sensitive action.
            (
                "click",
                in_form(
                    TargetFacts {
                        form_has_password: true,
                        ..facts("button", "submit", "Sign in")
                    },
                    "post",
                ),
                true,
            ),
            (
                "press",
                in_form(
                    TargetFacts {
                        form_has_password: true,
                        ..facts("input", "text", "")
                    },
                    "",
                ),
                true,
            ),
            (
                "click",
                in_form(
                    TargetFacts {
                        form_has_payment: true,
                        ..facts("button", "", "Continue")
                    },
                    "",
                ),
                true,
            ),
            (
                "click",
                in_form(facts("button", "submit", "Post comment"), "post"),
                true,
            ),
            ("press", in_form(facts("input", "text", ""), "post"), true),
            (
                "click",
                in_form(
                    TargetFacts {
                        form_action: "https://shop.example/cart/checkout".into(),
                        ..facts("button", "submit", "Go")
                    },
                    "get",
                ),
                true,
            ),
            (
                "click",
                in_form(facts("button", "submit", "Submit"), ""),
                true,
            ),
            ("click", in_form(facts("input", "submit", "Send"), ""), true),
            // Not sensitive: searches, navigation, ordinary typing.
            (
                "click",
                in_form(facts("button", "submit", "Search"), "get"),
                false,
            ),
            (
                "click",
                in_form(
                    TargetFacts {
                        attrs: "search-submit btn".into(),
                        ..facts("button", "submit", "Search")
                    },
                    "get",
                ),
                false,
            ),
            ("press", in_form(facts("input", "search", ""), "get"), false),
            (
                "click",
                in_form(facts("input", "submit", "Submit"), "get"),
                false,
            ),
            ("click", facts("a", "", "Documentation"), false),
            ("click", facts("button", "", "Add to cart"), false),
            ("click", facts("button", "button", "Next page"), false),
            ("click", facts("a", "", "Sort by order date"), false),
            ("click", facts("a", "", "Payload reference"), false),
            ("click", facts("button", "", "Display options"), false),
            ("type", facts("input", "text", "Delete"), false),
            ("type", facts("input", "search", ""), false),
            ("fill", facts("textarea", "", ""), false),
            ("hover", facts("button", "", "Settings"), false),
            ("check", facts("input", "checkbox", "Remember me"), false),
            ("js", facts("", "", "Delete"), false),
            (
                "click",
                TargetFacts {
                    attrs: "nav-link paypal-docs".into(),
                    ..facts("a", "", "Guides")
                },
                false,
            ),
        ];
        for (action, f, want) in cases {
            let got = sensitive_reason(action, &f);
            assert_eq!(got.is_some(), want, "{action} {f:?} → {got:?}");
        }
        let why = sensitive_reason("click", &facts("button", "", "Place order")).unwrap();
        assert!(why.contains("Place order"), "{why}");
    }

    #[test]
    fn filenames_are_sanitised() {
        let table: &[(&str, &str)] = &[
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            ("C:\\Windows\\x", "x"),
            ("..\\..\\boot.ini", "boot.ini"),
            ("con.txt", "_con.txt"),
            ("CON", "_CON"),
            ("nul", "_nul"),
            ("Com1.tar.gz", "_Com1.tar.gz"),
            ("lpt9.log", "_lpt9.log"),
            ("com¹.txt", "_com¹.txt"),
            ("console.log", "console.log"),
            ("a<b>c:d\"e|f?g*h.txt", "a_b_c_d_e_f_g_h.txt"),
            (".bashrc", "bashrc"),
            ("...", "download"),
            ("", "download"),
            ("   ", "download"),
            ("name. . .", "name"),
            ("tab\there\n.txt", "tabhere.txt"),
            ("invoice\u{202E}fdp.exe", "invoicefdp.exe"),
            ("résumé – final.docx", "résumé – final.docx"),
            ("数据.csv", "数据.csv"),
            ("dir/", "download"),
        ];
        for (input, want) in table {
            assert_eq!(sanitize_filename(input), *want, "{input:?}");
        }
        let long = format!("{}.tar", "ä".repeat(400));
        let s = sanitize_filename(&long);
        assert!(s.len() <= MAX_NAME_BYTES, "{}", s.len());
        assert!(s.ends_with(".tar"));
        let long = "x".repeat(1000);
        assert_eq!(sanitize_filename(&long).len(), MAX_NAME_BYTES);
    }

    #[test]
    fn download_targets_never_escape_or_overwrite() {
        let tmp = TempDir::new();
        let dir = tmp.path();
        for name in [
            "../../etc/passwd",
            "C:\\Windows\\x",
            "con.txt",
            "a/b/c.zip",
            "..",
        ] {
            let t = download_target(dir, name);
            assert_eq!(t.parent(), Some(dir), "{name}");
        }
        let a = download_target(dir, "report.pdf");
        assert_eq!(a, dir.join("report.pdf"));
        std::fs::write(&a, "1").unwrap();
        let b = download_target(dir, "report.pdf");
        assert_eq!(b, dir.join("report (2).pdf"));
        std::fs::write(&b, "2").unwrap();
        assert_eq!(
            download_target(dir, "report.pdf"),
            dir.join("report (3).pdf")
        );
        std::fs::write(dir.join("noext"), "1").unwrap();
        assert_eq!(download_target(dir, "noext"), dir.join("noext (2)"));
        let long = "y".repeat(400);
        std::fs::write(download_target(dir, &long), "1").unwrap();
        let second = download_target(dir, &long);
        assert!(second.file_name().unwrap().len() <= MAX_NAME_BYTES);
        assert!(second.to_string_lossy().ends_with(" (2)"));
    }

    #[test]
    fn uploads_stay_inside_the_project() {
        let tmp = TempDir::new();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join("assets")).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(project.join("assets/logo.png"), "png").unwrap();
        std::fs::write(project.join(".git/config"), "[remote]").unwrap();
        std::fs::write(project.join(".env"), "KEY=1").unwrap();
        std::fs::write(project.join(".env.example"), "KEY=").unwrap();
        std::fs::write(tmp.path().join("outside.txt"), "x").unwrap();
        let ok = upload_allowed(&project, "assets/logo.png").unwrap();
        assert!(ok.ends_with("logo.png"));
        let abs = project.join("assets").join("logo.png");
        assert!(upload_allowed(&project, &abs.to_string_lossy()).is_ok());
        assert!(upload_allowed(&project, ".env.example").is_ok());
        for bad in [
            "../outside.txt",
            "assets/../../outside.txt",
            ".git/config",
            ".env",
            "assets",
            "missing.png",
            "",
        ] {
            assert!(upload_allowed(&project, bad).is_err(), "{bad}");
        }
        let outside = tmp.path().join("outside.txt");
        assert!(upload_allowed(&project, &outside.to_string_lossy()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, project.join("link.txt")).unwrap();
            assert!(upload_allowed(&project, "link.txt").is_err());
        }
    }

    #[test]
    fn sites_and_patterns() {
        assert_eq!(
            site_of("https://Example.COM./a"),
            Some("example.com".into())
        );
        assert_eq!(site_of("http://[::1]:3000/"), Some("::1".into()));
        assert_eq!(
            site_of("http://0xA9FEA9FE/"),
            Some("169.254.169.254".into())
        );
        assert_eq!(site_of("file:///C:/x.html"), None);
        assert_eq!(site_of("about:blank"), None);
        assert_eq!(site_of("data:text/html,x"), None);
        assert!(site_matches("example.com", "EXAMPLE.com."));
        assert!(!site_matches("example.com", "a.example.com"));
        assert!(site_matches("*.example.com", "a.example.com"));
        assert!(site_matches("*.example.com", "a.b.example.com"));
        assert!(site_matches("*.example.com", "example.com"));
        for bad in [
            "evil-example.com",
            "example.com.evil.com",
            "notexample.com",
            "com",
            "",
        ] {
            assert!(!site_matches("*.example.com", bad), "{bad}");
        }
        assert!(!site_matches("*", "example.com"));
        assert!(!site_matches("*.", "example.com"));
        assert!(!site_matches("ex*.com", "example.com"));
        assert!(!site_matches("", "example.com"));
        assert!(!site_matches("example.com", "*"));
    }

    #[test]
    fn credential_headers_are_redacted() {
        let mut o = json!({
            "Authorization": "Bearer x",
            "cookie": "a=b",
            "Set-Cookie": "s=1",
            "Proxy-Authorization": "Basic y",
            "X-Api-Key": "k",
            "X-Auth-Token": "t",
            "x-goog-api-key": "g",
            "X-Amz-Security-Token": "a",
            "X-Client-Secret": "s",
            "Content-Type": "text/html",
            "Accept": "*/*",
        });
        redact_headers(&mut o);
        for k in [
            "Authorization",
            "cookie",
            "Set-Cookie",
            "Proxy-Authorization",
            "X-Api-Key",
            "X-Auth-Token",
            "x-goog-api-key",
            "X-Amz-Security-Token",
            "X-Client-Secret",
        ] {
            assert_eq!(o[k], "[redacted]", "{k}");
        }
        assert_eq!(o["Content-Type"], "text/html");
        assert_eq!(o["Accept"], "*/*");
        let mut a = json!([
            {"name": "AUTHORIZATION", "value": "Bearer x"},
            {"name": "Accept", "value": "*/*"}
        ]);
        redact_headers(&mut a);
        assert_eq!(a[0]["value"], "[redacted]");
        assert_eq!(a[1]["value"], "*/*");
        let mut t = json!("GET / HTTP/1.1\r\nHost: x\r\nCookie: a=b\r\n\r\n");
        redact_headers(&mut t);
        let t = t.as_str().unwrap();
        assert!(t.contains("Cookie: [redacted]") && t.contains("Host: x") && !t.contains("a=b"));
    }
}
