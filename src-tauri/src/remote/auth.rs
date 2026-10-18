//! Who may drive this app from another device, and how they proved it.
//!
//! Remote control has full parity with the desktop window, so a paired device
//! can do anything the user can — including running shell commands through an
//! agent. Everything here exists to keep that power with the devices the user
//! deliberately paired, and nowhere else:
//!
//! - **Pair token**: a long random secret, carried in the QR code / pairing link.
//!   It is only ever exchanged for a session, never used as a session itself, so
//!   it does not sit in a browser's history or cookie jar.
//! - **Short code**: six digits for typing on a phone, valid for five minutes,
//!   burned after a handful of wrong guesses. Five guesses at one in a million
//!   is the whole brute-force budget per code.
//! - **Sessions**: the cookie a paired browser keeps. Only a SHA-256 of each
//!   secret is stored, so `remote.json` leaking does not hand out live sessions.
//!   Each one is listed in Settings by device and can be revoked on its own.
//!
//! Loopback is deliberately *not* trusted on its own. A token-less localhost
//! server (the zcode default) is reachable by every web page the user opens:
//! a malicious page can drive it through DNS rebinding or a simple form post.
//! Requiring a session cookie on every request closes that, wherever we bind.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Default TCP port. High and unassigned, so it rarely collides with dev servers.
pub const DEFAULT_PORT: u16 = 17420;
/// Paired devices kept; pairing another evicts the least recently seen.
pub const MAX_SESSIONS: usize = 20;
/// How long a typed pairing code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);
/// Wrong guesses a single short code survives before it is burned.
pub const CODE_MAX_FAILS: u32 = 5;
/// Failed pairing attempts one address may make inside `FAIL_WINDOW`.
pub const IP_MAX_FAILS: u32 = 10;
pub const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Persisted remote-control state: `remote.json` in the data directory.
///
/// Kept out of `settings.json` on purpose: settings are sent to the UI, written
/// by a generic patch command, and get copied around when people share configs.
/// None of that should ever carry the pairing secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Vault {
    pub enabled: bool,
    /// Listen on every interface rather than loopback only.
    pub lan: bool,
    pub port: u16,
    pub pair_token: String,
    pub sessions: Vec<Session>,
    /// Other machines this app opens over SSH (`ssh.rs`).
    pub ssh_hosts: Vec<super::ssh::SshHost>,
    /// Ntfy server credentials, kept in the private remote vault.
    pub ntfy: Ntfy,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            enabled: false,
            lan: false,
            port: DEFAULT_PORT,
            pair_token: String::new(),
            sessions: vec![],
            ssh_hosts: vec![],
            ntfy: Ntfy::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Public handle, shown in the UI and used to revoke. Not a secret.
    pub id: String,
    /// Hex SHA-256 of the cookie secret.
    pub hash: String,
    /// Device description derived from the browser's User-Agent.
    pub label: String,
    pub created: i64,
    pub last_seen: i64,
    /// Phone notification preferences are scoped to this paired device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushCfg>,
}

/// Ntfy server credentials persisted only in remote.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ntfy {
    pub server: String,
    pub token: String,
}

/// One paired device's ntfy subscription (not a credential itself).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushCfg {
    pub topic: String,
    #[serde(default)]
    pub details: bool,
    #[serde(default)]
    pub confirm_all: bool,
    #[serde(default = "yes")]
    pub questions: bool,
    #[serde(default = "yes")]
    pub done: bool,
    pub base_url: String,
    #[serde(rename = "gen")]
    pub r#gen: String,
    pub since: i64,
}

fn yes() -> bool {
    true
}

/// 244 bits of OS randomness, URL-safe. `uuid` v4 draws from `getrandom`, so
/// this needs no RNG dependency of its own.
pub fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn hash(secret: &str) -> String {
    let d = Sha256::digest(secret.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time string comparison, so response timing never reveals how much
/// of a guessed token was right.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn vault_path() -> PathBuf {
    crate::agent::store::data_dir().join("remote.json")
}

impl Vault {
    pub fn load() -> Self {
        let mut v: Vault = std::fs::read_to_string(vault_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if v.port == 0 {
            v.port = DEFAULT_PORT;
        }
        if v.pair_token.len() < 32 {
            v.pair_token = secret();
        }
        v
    }

    pub fn save(&self) {
        let path = vault_path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(s) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, s).is_ok() {
                restrict_permissions(&tmp);
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    /// Mint a session for a device that just proved itself. Returns
    /// `(public id, cookie secret)`; only the hash of the secret is kept.
    pub fn create_session(&mut self, label: &str, now: i64) -> (String, String) {
        let secret = secret();
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        self.sessions.push(Session {
            id: id.clone(),
            hash: hash(&secret),
            label: label.chars().take(80).collect(),
            created: now,
            last_seen: now,
            push: None,
        });
        if self.sessions.len() > MAX_SESSIONS {
            self.sessions
                .sort_by_key(|s| std::cmp::Reverse(s.last_seen));
            self.sessions.truncate(MAX_SESSIONS);
        }
        (id, secret)
    }

    /// The session a cookie secret belongs to, if it is still valid.
    pub fn find_session(&self, secret: &str) -> Option<&Session> {
        if secret.is_empty() {
            return None;
        }
        let h = hash(secret);
        self.sessions.iter().find(|s| ct_eq(&s.hash, &h))
    }

    pub fn touch(&mut self, id: &str, now: i64) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
            s.last_seen = now;
        }
    }

    pub fn revoke(&mut self, id: &str) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        before != self.sessions.len()
    }

    /// New pairing secret and no sessions: every device has to pair again.
    pub fn reset(&mut self) {
        self.pair_token = secret();
        self.sessions.clear();
    }
}

/// Owner-only file permissions where the platform has them. On Windows the
/// file inherits the profile directory's ACL, which is already per-user.
fn restrict_permissions(_p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(_p, std::fs::Permissions::from_mode(0o600));
    }
}

/// A typed pairing code and how much of its budget is left.
#[derive(Debug, Clone)]
pub struct ShortCode {
    pub code: String,
    pub expires: Instant,
    pub fails: u32,
}

impl ShortCode {
    pub fn new(now: Instant) -> Self {
        // Uniform over 000000..=999999: reject the top sliver of u32 that
        // would bias the modulo, rather than skewing toward low codes.
        let n = loop {
            let b = uuid::Uuid::new_v4().into_bytes();
            let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            if n < u32::MAX - (u32::MAX % 1_000_000) {
                break n % 1_000_000;
            }
        };
        Self {
            code: format!("{n:06}"),
            expires: now + CODE_TTL,
            fails: 0,
        }
    }
}

/// Outcome of a typed pairing attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum CodeCheck {
    Ok,
    Wrong,
    /// No code is active, it expired, or it was burned by wrong guesses.
    NoCode,
    /// This address has failed too often recently.
    Limited,
}

/// Per-address failure accounting for pairing endpoints.
#[derive(Default)]
pub struct Limiter {
    fails: HashMap<std::net::IpAddr, Vec<Instant>>,
}

impl Limiter {
    pub fn limited(&mut self, ip: std::net::IpAddr, now: Instant) -> bool {
        let v = self.fails.entry(ip).or_default();
        v.retain(|t| now.duration_since(*t) < FAIL_WINDOW);
        v.len() as u32 >= IP_MAX_FAILS
    }

    pub fn fail(&mut self, ip: std::net::IpAddr, now: Instant) {
        self.fails.entry(ip).or_default().push(now);
        // Bound memory against a scan from many addresses.
        if self.fails.len() > 4096 {
            self.fails
                .retain(|_, v| v.iter().any(|t| now.duration_since(*t) < FAIL_WINDOW));
        }
    }
}

/// Check a typed code, spending the code's and the address's failure budget.
pub fn check_code(
    code: &mut Option<ShortCode>,
    limiter: &mut Limiter,
    ip: std::net::IpAddr,
    guess: &str,
    now: Instant,
) -> CodeCheck {
    if limiter.limited(ip, now) {
        return CodeCheck::Limited;
    }
    let Some(c) = code.as_mut() else {
        limiter.fail(ip, now);
        return CodeCheck::NoCode;
    };
    if now >= c.expires || c.fails >= CODE_MAX_FAILS {
        *code = None;
        limiter.fail(ip, now);
        return CodeCheck::NoCode;
    }
    let guess: String = guess.chars().filter(|c| c.is_ascii_digit()).collect();
    if ct_eq(&guess, &c.code) {
        // Single use: a code that paired one device cannot pair a second.
        *code = None;
        return CodeCheck::Ok;
    }
    c.fails += 1;
    if c.fails >= CODE_MAX_FAILS {
        *code = None;
    }
    limiter.fail(ip, now);
    CodeCheck::Wrong
}

/// A short, human label for a device from its User-Agent ("Safari on iPhone").
pub fn device_label(ua: &str) -> String {
    let os = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "Android"),
        ("Windows", "Windows"),
        ("Mac OS X", "macOS"),
        ("Macintosh", "macOS"),
        ("CrOS", "ChromeOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map(|(_, v)| *v);
    // Order matters: Edge and Opera also say "Chrome", and Chrome says "Safari".
    let browser = [
        ("Edg/", "Edge"),
        ("OPR/", "Opera"),
        ("Firefox/", "Firefox"),
        ("FxiOS", "Firefox"),
        ("CriOS", "Chrome"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map(|(_, v)| *v);
    match (browser, os) {
        (Some(b), Some(o)) => format!("{b} on {o}"),
        (Some(b), None) => b.to_string(),
        (None, Some(o)) => format!("Browser on {o}"),
        (None, None) => "Unknown device".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));

    #[test]
    fn secrets_are_long_unique_and_url_safe() {
        let a = secret();
        let b = secret();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn ct_eq_matches_only_identical_strings() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "a"));
    }

    #[test]
    fn sessions_store_only_hashes_and_can_be_revoked() {
        let mut v = Vault::default();
        let (id, secret) = v.create_session("Safari on iPhone", 1);
        assert!(!serde_json::to_string(&v).unwrap().contains(&secret));
        assert_eq!(
            v.find_session(&secret).map(|s| s.id.clone()),
            Some(id.clone())
        );
        assert!(v.find_session("not-it").is_none());
        assert!(v.find_session("").is_none());
        assert!(v.revoke(&id));
        assert!(v.find_session(&secret).is_none());
        assert!(!v.revoke(&id));
    }

    #[test]
    fn reset_revokes_every_session_and_rotates_the_pair_token() {
        let mut v = Vault {
            pair_token: secret(),
            ..Vault::default()
        };
        let old = v.pair_token.clone();
        let (_, s1) = v.create_session("a", 1);
        v.reset();
        assert_ne!(v.pair_token, old);
        assert!(v.find_session(&s1).is_none());
    }

    #[test]
    fn session_count_is_bounded_keeping_the_most_recent() {
        let mut v = Vault::default();
        let (_, oldest) = v.create_session("old", 0);
        for i in 1..=MAX_SESSIONS as i64 {
            v.create_session("d", i);
        }
        assert_eq!(v.sessions.len(), MAX_SESSIONS);
        assert!(v.find_session(&oldest).is_none());
    }

    #[test]
    fn short_code_is_single_use() {
        let now = Instant::now();
        let mut code = Some(ShortCode::new(now));
        let c = code.as_ref().unwrap().code.clone();
        assert_eq!(c.len(), 6);
        let mut lim = Limiter::default();
        assert_eq!(check_code(&mut code, &mut lim, IP, &c, now), CodeCheck::Ok);
        assert_eq!(
            check_code(&mut code, &mut lim, IP, &c, now),
            CodeCheck::NoCode
        );
    }

    #[test]
    fn short_code_tolerates_spaces_but_not_wrong_digits() {
        let now = Instant::now();
        let mut code = Some(ShortCode {
            code: "123456".into(),
            expires: now + CODE_TTL,
            fails: 0,
        });
        let mut lim = Limiter::default();
        assert_eq!(
            check_code(&mut code, &mut lim, IP, "123 457", now),
            CodeCheck::Wrong
        );
        assert_eq!(
            check_code(&mut code, &mut lim, IP, "123 456", now),
            CodeCheck::Ok
        );
    }

    #[test]
    fn short_code_burns_after_max_failures() {
        let now = Instant::now();
        let mut code = Some(ShortCode {
            code: "123456".into(),
            expires: now + CODE_TTL,
            fails: 0,
        });
        let mut lim = Limiter::default();
        for i in 0..CODE_MAX_FAILS {
            // Different addresses, so the per-address limiter is not what stops it.
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, i as u8));
            assert_eq!(
                check_code(&mut code, &mut lim, ip, "000000", now),
                CodeCheck::Wrong
            );
        }
        assert_eq!(
            check_code(&mut code, &mut lim, IP, "123456", now),
            CodeCheck::NoCode
        );
    }

    #[test]
    fn short_code_expires() {
        let now = Instant::now();
        let mut code = Some(ShortCode {
            code: "123456".into(),
            expires: now + CODE_TTL,
            fails: 0,
        });
        let mut lim = Limiter::default();
        assert_eq!(
            check_code(&mut code, &mut lim, IP, "123456", now + CODE_TTL),
            CodeCheck::NoCode
        );
    }

    #[test]
    fn an_address_that_keeps_failing_is_locked_out_even_with_the_right_code() {
        let now = Instant::now();
        let mut lim = Limiter::default();
        for _ in 0..IP_MAX_FAILS {
            lim.fail(IP, now);
        }
        // Outlives the lockout window, so the second check sees a live code.
        let mut code = Some(ShortCode {
            code: "123456".into(),
            expires: now + FAIL_WINDOW * 2,
            fails: 0,
        });
        assert_eq!(
            check_code(&mut code, &mut lim, IP, "123456", now),
            CodeCheck::Limited
        );
        // The lockout lapses with the window.
        assert_eq!(
            check_code(&mut code, &mut lim, IP, "123456", now + FAIL_WINDOW),
            CodeCheck::Ok
        );
    }

    #[test]
    fn device_labels() {
        assert_eq!(
            device_label(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
            ),
            "Safari on iPhone"
        );
        assert_eq!(
            device_label(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0"
            ),
            "Edge on Windows"
        );
        assert_eq!(
            device_label(
                "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36"
            ),
            "Chrome on Android"
        );
        assert_eq!(device_label(""), "Unknown device");
    }
}
