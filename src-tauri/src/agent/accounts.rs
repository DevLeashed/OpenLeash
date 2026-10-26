//! Subscription accounts (ChatGPT/Codex, Claude Pro/Max over OAuth, OpenCode Go
//! over a workspace API key).
//!
//! - import: accepts the files the official CLIs write (`~/.codex/auth.json`,
//!   `~/.claude/.credentials.json`, opencode's `auth.json`) or a bare token
//! - refresh: rotates tokens before expiry and writes them back to the source
//!   file, so the CLI that owns the login keeps working. OpenCode Go keys do not
//!   expire and are never rotated, so that path is skipped for them.
//! - usage: reads each account's live limits (5h / weekly windows) so the
//!   router can skip exhausted accounts without burning a request on them
//! - selection: highest priority first; among equals, stay on the account
//!   that is already warm so the provider's prompt cache keeps hitting

use super::store::{self, Account, UsageCap};
use super::Harness;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Overridable (env) so tests can point the subscription endpoints at a local mock.
fn codex_base() -> String {
    std::env::var("OPENLEASH_CODEX_BASE")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api".into())
}
fn anthropic_base() -> String {
    std::env::var("OPENLEASH_ANTHROPIC_BASE").unwrap_or_else(|_| "https://api.anthropic.com".into())
}
pub fn codex_url() -> String {
    format!("{}/codex/responses", codex_base())
}
pub fn claude_url() -> String {
    format!("{}/v1/messages?beta=true", anthropic_base())
}
/// OpenCode Zen/Go gateway root. Overridable so tests can point the account
/// endpoints at a local mock, the same way codex/anthropic are.
fn zen_base() -> String {
    std::env::var("OPENLEASH_ZEN_BASE").unwrap_or_else(|_| "https://opencode.ai".into())
}
/// `{base}/zen/go/v1/{path}` — the Go plan's own endpoints (models, usage).
pub fn go_url(path: &str) -> String {
    format!("{}/zen/go/v1/{}", zen_base().trim_end_matches('/'), path)
}
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_UA: &str = "codex_cli_rs/0.46.0 (Windows 10.0.26200; x86_64) WindowsTerminal";

const CLAUDE_TOKEN_URLS: [&str; 2] = [
    "https://platform.claude.com/v1/oauth/token",
    "https://console.anthropic.com/v1/oauth/token",
];
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const CLAUDE_UA: &str = "claude-cli/2.1.280 (external, cli)";
pub const CLAUDE_BETA: &str = "oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14";
/// Subscription OAuth calls are rejected unless the system prompt opens with this line.
pub const CLAUDE_PRELUDE: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Window {
    /// "5h" | "Week" | "Week · Opus" …
    pub label: String,
    /// 0-100, percent of the window used. Both providers report "used"; the
    /// UI displays what's left (100 - used).
    pub used: f64,
    /// Unix seconds, 0 = unknown.
    pub resets_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcctUsage {
    pub windows: Vec<Window>,
    /// The provider says no more requests until a reset.
    pub limited: bool,
    pub plan: String,
    pub fetched: i64,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AcctState {
    usage: Option<AcctUsage>,
    /// Unix seconds; skip this account until then.
    cooldown_until: i64,
    cooldown_reason: String,
}

/// Live (non-persisted) account state.
#[derive(Default)]
pub struct AccountRt {
    pub(crate) state: std::sync::Mutex<HashMap<String, AcctState>>,
    /// kind -> account id currently serving (cache warmth).
    sticky: std::sync::Mutex<HashMap<String, String>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

const LIVE_LIMIT: &str = "Usage limit (per live usage)";

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Serialize, Clone)]
pub struct AccountView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub email: String,
    /// Masked workspace-key suffix only; OAuth credentials never leave the backend.
    pub key_hint: String,
    pub priority: i32,
    pub enabled: bool,
    pub source: String,
    pub disabled_reason: String,
    pub usage: Option<AcctUsage>,
    pub cooldown_until: i64,
    pub cooldown_reason: String,
    pub available: bool,
    pub active: bool,
    pub expires_at: i64,
    /// The usage caps standing on this account, expired ones left out.
    pub caps: Vec<CapView>,
}

/// A cap as the settings page draws it.
#[derive(Serialize, Clone)]
pub struct CapView {
    pub window: String,
    /// Percent of the window used at which the harness stops using the account.
    pub used: f64,
    /// Unix seconds the cap lifts on its own; 0 = until the user removes it.
    pub until: i64,
    /// This cap is what is keeping the account out of the pool right now.
    ///
    /// Computed from the same predicate the router applies, so a row that draws
    /// a cap as reached is a row the router is genuinely skipping — two
    /// independent answers to "is this account being used" is how a settings
    /// page starts lying about what the harness is doing.
    pub holding: bool,
}

impl AccountRt {
    pub fn views(&self, accts: &[Account]) -> Vec<AccountView> {
        let st = self.state.lock().unwrap();
        let sticky = self.sticky.lock().unwrap();
        accts
            .iter()
            .map(|a| {
                let s = st.get(&a.id).cloned().unwrap_or_default();
                // Built before the struct literal because `s.usage` is moved out
                // of `s` below, and the caps read the state it still holds.
                //
                // A temporary cap that has run out can never hold anything
                // again, so it is not a cap the page should offer to remove —
                // it is litter in the settings file until the next cap on this
                // window replaces it.
                let caps: Vec<CapView> = a
                    .caps
                    .iter()
                    .filter(|c| c.until == 0 || c.until > now())
                    .map(|c| CapView {
                        window: c.window.clone(),
                        used: c.used,
                        until: c.until,
                        holding: cap_holding(c, &s),
                    })
                    .collect();
                AccountView {
                    id: a.id.clone(),
                    kind: a.kind.clone(),
                    label: a.label.clone(),
                    email: a.email.clone(),
                    // A key-login account identifies by key fragment. That is
                    // by construction for Kimi Code (the parse path only
                    // accepts a pasted key there, and there is no local OAuth
                    // import to muddy it), but NOT for Go: its auth.json can
                    // also carry an OAuth login for the same provider id, and
                    // hinting a fragment of a session token is exactly the
                    // leak the shape check below refuses. OAuth kinds get
                    // nothing: their tokens are sessions, not keys.
                    key_hint: if key_only(&a.kind)
                        && (a.kind == "kimi-code" || looks_like_opencode_key(&a.access_token))
                    {
                        super::providers::key_hint(&a.access_token)
                    } else {
                        String::new()
                    },
                    priority: a.priority,
                    enabled: a.enabled,
                    source: a.source.clone(),
                    disabled_reason: a.disabled_reason.clone(),
                    available: usable_with(a, &s),
                    active: sticky.get(&a.kind) == Some(&a.id),
                    usage: s.usage,
                    cooldown_until: s.cooldown_until,
                    cooldown_reason: s.cooldown_reason,
                    expires_at: a.expires_at,
                    caps,
                }
            })
            .collect()
    }

    pub fn usable(&self, a: &Account) -> bool {
        let st = self.state.lock().unwrap();
        usable_with(a, &st.get(&a.id).cloned().unwrap_or_default())
    }

    /// The latest usage reading for one account, if any has arrived. Read-only:
    /// the quota burner (`agent::burner`) decides from the same reading the
    /// router and the settings page use, rather than keeping a second copy.
    pub fn usage_of(&self, id: &str) -> Option<AcctUsage> {
        self.state
            .lock()
            .unwrap()
            .get(id)
            .and_then(|s| s.usage.clone())
    }

    /// Free-named models may have a separate allowance from the account's paid
    /// usage windows, so let them try an account benched only for usage limits.
    /// Explicit account disables and credential failures still apply.
    pub fn usable_for_model(&self, a: &Account, model: &str) -> bool {
        if !model.to_ascii_lowercase().contains("free") {
            return self.usable(a);
        }
        let st = self.state.lock().unwrap();
        let state = st.get(&a.id).cloned().unwrap_or_default();
        let usage_limited = state.cooldown_reason == LIVE_LIMIT
            || state.cooldown_reason == "Usage limit reached"
            || state.cooldown_reason == "Rate limited";
        let mut eligible = state.clone();
        let mut model_account = a.clone();
        // A free model is allowed to use the account even when its paid-usage
        // cap or provider-reported usage window is exhausted.
        model_account.caps.clear();
        if usage_limited {
            eligible.cooldown_until = 0;
            eligible.cooldown_reason.clear();
            eligible.usage = None;
        }
        usable_with(&model_account, &eligible)
    }

    /// Earliest moment any of these accounts frees up (for "resumes at …").
    ///
    /// A cap is a blocker like any other, and it lifts two ways: a temporary one
    /// when its own clock runs out, a permanent one when the window it caps
    /// rolls over and the account starts that window under the cap again. A
    /// permanent cap whose window reports no reset time therefore frees up at no
    /// known moment and contributes nothing here, rather than a made-up time:
    /// the chat stays parked until the user lifts the cap, which is what they
    /// asked for by setting it.
    pub fn next_free(&self, accts: &[Account]) -> i64 {
        let t = now();
        let st = self.state.lock().unwrap();
        accts
            .iter()
            .filter_map(|a| {
                let s = st.get(&a.id)?;
                let mut at = s.cooldown_until;
                if let Some(u) = &s.usage {
                    for w in u.windows.iter().filter(|w| w.used >= 99.5) {
                        at = at.max(w.resets_at);
                    }
                }
                if let Some(c) = holding_cap(a, s) {
                    let reset = s
                        .usage
                        .as_ref()
                        .and_then(|u| u.windows.iter().find(|w| w.label == c.window))
                        .map(|w| w.resets_at)
                        .unwrap_or(0);
                    let lift = match (c.until > t, reset > t) {
                        (true, true) => c.until.min(reset),
                        (true, false) => c.until,
                        (false, true) => reset,
                        (false, false) => 0,
                    };
                    if lift > 0 {
                        at = at.max(lift);
                    }
                }
                (at > t).then_some(at)
            })
            .min()
            .unwrap_or(0)
    }

    pub fn cool(&self, id: &str, until: i64, why: &str) {
        let mut st = self.state.lock().unwrap();
        let s = st.entry(id.into()).or_default();
        s.cooldown_until = s.cooldown_until.max(until);
        s.cooldown_reason = why.chars().take(200).collect();
    }

    pub fn ok(&self, kind: &str, id: &str) {
        self.sticky.lock().unwrap().insert(kind.into(), id.into());
        let mut st = self.state.lock().unwrap();
        let s = st.entry(id.into()).or_default();
        if s.cooldown_until <= now() {
            s.cooldown_reason.clear();
        }
    }

    /// Drop account benches that a *refresh failure* put there, which are 10
    /// minutes of "we could not renew this token" and nothing more.
    ///
    /// The refresh has no backoff of its own, so without this a transient network
    /// failure at 09:00 takes the account out of every chat's chain until 09:10
    /// with nothing on screen saying why — and a chat parked on it cannot be
    /// resumed at all, because every press rebuilt the same empty chain.
    ///
    /// Only the refresh's own bench is touched, and that is the line that matters.
    /// Two things bench an account: a quota the provider reported, and the
    /// router's token refresh failing (see `router::request`, which benches
    /// "Credential rejected" when `accounts::fresh` cannot renew). Only the second
    /// is a local failure — a refresh endpoint that is unreachable does not mean
    /// the key already in hand has stopped working — and the two share one field,
    /// so they are told apart by the reason the bench carries.
    ///
    /// A reported quota keeps its bench: the user is waiting for a window to roll
    /// over, and re-asking for it every few seconds is exactly what turns one
    /// exhausted account into a retry storm. `KeyRt::clear_benches` is the other
    /// half of this fix and is not shy about it — a key's bench is set in one
    /// place only, by the provider saying it is rate limited, so re-asking costs
    /// one request and re-benches itself if it is wrong.
    pub fn expire_refresh_benches(&self) {
        let mut st = self.state.lock().unwrap();
        for s in st.values_mut() {
            if s.cooldown_reason == "Credential rejected" {
                s.cooldown_until = 0;
                s.cooldown_reason.clear();
            }
        }
    }

    pub fn set_usage(&self, id: &str, u: AcctUsage) {
        let mut st = self.state.lock().unwrap();
        let s = st.entry(id.into()).or_default();
        if u.limited {
            let reset = u
                .windows
                .iter()
                .filter(|w| w.used >= 99.5)
                .map(|w| w.resets_at)
                .max()
                .unwrap_or(now() + 300);
            s.cooldown_until = s.cooldown_until.max(reset);
            s.cooldown_reason = LIVE_LIMIT.into();
        } else if s.cooldown_reason == LIVE_LIMIT {
            // The live read put it on the bench, and now says it's usable again.
            // (A 429 from the provider itself is trusted over a lagging usage read.)
            s.cooldown_until = 0;
            s.cooldown_reason.clear();
        }
        s.usage = Some(u);
    }

    /// Update windows from rate-limit headers that ride along on every response.
    pub fn update_from_headers(&self, kind: &str, id: &str, headers: &reqwest::header::HeaderMap) {
        let get = |k: &str| {
            headers
                .get(k)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        };
        let num = |k: &str| get(k).and_then(|v| v.parse::<f64>().ok());
        let mut windows = vec![];
        let mut limited = false;
        if kind == "codex" {
            for (p, label) in [("primary", "5h"), ("secondary", "Week")] {
                if let Some(used) = num(&format!("x-codex-{p}-used-percent"))
                    .filter(|u| u.is_finite() && (0.0..=100.0).contains(u))
                {
                    let after =
                        num(&format!("x-codex-{p}-reset-after-seconds")).unwrap_or(0.0) as i64;
                    let reset = num(&format!("x-codex-{p}-reset-at"))
                        .filter(|t| t.is_finite() && *t > 0.0)
                        .map(|t| t as i64)
                        .unwrap_or_else(|| if after > 0 { now() + after } else { 0 });
                    let mins = num(&format!("x-codex-{p}-window-minutes")).unwrap_or(0.0);
                    // Codex can send a zero-filled header family with no window
                    // metadata. Like the official client, treat it as absent,
                    // not a real reset to 0% used (100% left in the UI).
                    if used == 0.0 && mins <= 0.0 && reset == 0 {
                        continue;
                    }
                    let label = if mins > 0.0 {
                        window_label(mins as i64 * 60)
                    } else {
                        label.to_string()
                    };
                    windows.push(Window {
                        label,
                        used,
                        resets_at: reset,
                    });
                }
            }
        } else {
            for (p, label) in [("5h", "5h"), ("7d", "Week")] {
                if let Some(u) = num(&format!("anthropic-ratelimit-unified-{p}-utilization")) {
                    let reset = num(&format!("anthropic-ratelimit-unified-{p}-reset"))
                        .unwrap_or(0.0) as i64;
                    windows.push(Window {
                        label: label.into(),
                        used: if u <= 1.0 { u * 100.0 } else { u },
                        resets_at: reset,
                    });
                }
            }
            limited = get("anthropic-ratelimit-unified-status").is_some_and(|s| s == "rejected");
        }
        if windows.is_empty() {
            return;
        }
        let mut st = self.state.lock().unwrap();
        let s = st.entry(id.into()).or_default();
        let mut u = s.usage.clone().unwrap_or_default();
        for w in windows {
            match u.windows.iter_mut().find(|x| x.label == w.label) {
                Some(x) => *x = w,
                None => u.windows.push(w),
            }
        }
        u.limited = limited || u.windows.iter().any(|w| w.used >= 99.5);
        u.fetched = now();
        u.error.clear();
        s.usage = Some(u);
    }
}

/// Live (non-persisted) cooldown state for API keys in a provider's key pool.
#[derive(Debug, Clone, Default)]
pub struct KeyState {
    /// Unix seconds; skip this key until then.
    pub cooldown_until: i64,
    pub cooldown_reason: String,
}

/// Tracks which API keys are on cooldown so the router can skip them
/// (just like `AccountRt` does for subscription accounts).
#[derive(Default)]
pub struct KeyRt {
    pub(crate) state: std::sync::Mutex<HashMap<String, KeyState>>,
}

impl KeyRt {
    fn key_of(prov: &str, key: &str) -> String {
        format!("{prov}:{key}")
    }

    /// Bench a key until `until` (unix seconds).
    pub fn cool(&self, prov: &str, key: &str, until: i64, why: &str) {
        let k = Self::key_of(prov, key);
        let mut st = self.state.lock().unwrap();
        let s = st.entry(k).or_default();
        s.cooldown_until = s.cooldown_until.max(until);
        s.cooldown_reason = why.chars().take(200).collect();
    }

    /// Forget every bench, so the next request re-tests each key for real. Called
    /// from a resume, where the user has just said "go again" by hand.
    ///
    /// This is the main road into the "Resume needs four presses" bug. A chat
    /// frozen on a chain whose keys are all benched wakes up and walks that chain
    /// again — and the chain is built by skipping every benched key, so it comes
    /// back empty, not one request is sent, and the pause is raised again. Nothing
    /// clears a bench in between, because the code that would (`router`, as it
    /// re-walks) sits behind the pause being lifted. Every press rebuilt the same
    /// emptiness.
    ///
    /// Safe to be this blunt here, and deliberately blunter than the account
    /// equivalent, because a key's bench is set in exactly one place: the provider
    /// said this key is rate limited. Re-asking is one request, and a wrong guess
    /// re-benches itself with a fresh answer.
    pub fn clear_benches(&self) {
        let mut st = self.state.lock().unwrap();
        for s in st.values_mut() {
            s.cooldown_until = 0;
            s.cooldown_reason.clear();
        }
    }

    /// A key is usable when it's not on cooldown.
    pub fn usable(&self, prov: &str, key: &str) -> bool {
        let k = Self::key_of(prov, key);
        let st = self.state.lock().unwrap();
        st.get(&k).is_none_or(|s| s.cooldown_until <= now())
    }

    /// Earliest moment any of these keys frees up (for "resumes at …").
    pub fn next_free(&self, prov: &str, keys: &[String]) -> i64 {
        let st = self.state.lock().unwrap();
        keys.iter()
            .filter_map(|k| {
                let s = st.get(&Self::key_of(prov, k))?;
                (s.cooldown_until > now()).then_some(s.cooldown_until)
            })
            .min()
            .unwrap_or(0)
    }

    /// Clear a cooldown (key worked). Mostly a no-op today since a successful
    /// request never sets a cooldown, but keeps the door open.
    pub fn ok(&self, prov: &str, key: &str) {
        let k = Self::key_of(prov, key);
        let mut st = self.state.lock().unwrap();
        if let Some(s) = st.get_mut(&k) {
            if s.cooldown_until <= now() {
                s.cooldown_reason.clear();
            }
        }
    }
}

fn usable_with(a: &Account, s: &AcctState) -> bool {
    a.enabled
        && a.disabled_reason.is_empty()
        && s.cooldown_until <= now()
        && holding_cap(a, s).is_none()
        && !s.usage.as_ref().is_some_and(|u| {
            u.limited
                && u.windows
                    .iter()
                    .filter(|w| w.used >= 99.5)
                    .any(|w| w.resets_at == 0 || w.resets_at > now())
        })
}

/// Is this cap keeping the account out of the pool right now?
///
/// It holds when it is still live and the window it names has reached it. The
/// window is found by label, not by position, so a provider that stops reporting
/// a window leaves its cap armed and inert instead of applying it to whichever
/// window took its place.
fn cap_holding(c: &UsageCap, s: &AcctState) -> bool {
    if c.until > 0 && c.until <= now() {
        return false;
    }
    s.usage.as_ref().is_some_and(|u| {
        u.windows
            .iter()
            .any(|w| w.label == c.window && w.used >= c.used)
    })
}

/// The cap holding this account out of the pool, if one is.
fn holding_cap<'a>(a: &'a Account, s: &AcctState) -> Option<&'a UsageCap> {
    a.caps.iter().find(|c| cap_holding(c, s))
}

fn window_label(secs: i64) -> String {
    match secs {
        s if s <= 0 => "Window".into(),
        s if s < 86_400 => format!("{}h", (s + 1800) / 3600),
        s if (6 * 86_400..=8 * 86_400).contains(&s) => "Week".into(),
        s => format!("{}d", (s + 43_200) / 86_400),
    }
}

/// Upcoming weekly reset (unix secs); i64::MAX when unknown or already past.
fn weekly_reset(s: Option<&AcctState>) -> i64 {
    let t = now();
    s.and_then(|s| s.usage.as_ref())
        .into_iter()
        .flat_map(|u| u.windows.iter())
        .filter(|w| w.label.starts_with("Week") && w.resets_at > t)
        .map(|w| w.resets_at)
        .min()
        .unwrap_or(i64::MAX)
}

/// Accounts of a kind in the order the router should try them.
pub fn pool(h: &Harness, accts: &[Account], kind: &str) -> Vec<Account> {
    let sticky = h.accts.sticky.lock().unwrap().get(kind).cloned();
    let mut v: Vec<(usize, Account)> = accts
        .iter()
        .cloned()
        .enumerate()
        .filter(|(_, a)| a.kind == kind && a.enabled && a.disabled_reason.is_empty())
        .collect();
    // Within a priority tier, burn the account whose weekly window resets
    // soonest: its remaining quota is about to be forfeited anyway. This sits
    // above stickiness (cache warmth) on purpose; reset times are stable, so it
    // does not make the choice flap between turns. Unknown reset sorts last.
    let reset: HashMap<String, i64> = {
        let st = h.accts.state.lock().unwrap();
        v.iter()
            .map(|(_, a)| (a.id.clone(), weekly_reset(st.get(&a.id))))
            .collect()
    };
    v.sort_by(|(ia, a), (ib, b)| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| reset[&a.id].cmp(&reset[&b.id]))
            .then_with(|| (Some(&b.id) == sticky.as_ref()).cmp(&(Some(&a.id) == sticky.as_ref())))
            .then(ia.cmp(ib))
    });
    v.into_iter().map(|(_, a)| a).collect()
}

// ───────────────────────────── import ─────────────────────────────

fn b64url(s: &str) -> Vec<u8> {
    let mut out = vec![];
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => continue,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

pub fn jwt_claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|p| serde_json::from_slice(&b64url(p)).ok())
        .unwrap_or(Value::Null)
}

/// An OpenCode Zen/Go workspace key: `sk-` followed by 64 alphanumerics.
///
/// Deliberately narrow. `parse` has to tell a pasted Go key from a pasted Codex
/// JWT, and the length+alphabet is what makes that unambiguous — a `sk-` alone
/// would collide with an OpenAI key and the hint is not always supplied (the
/// paste box is shared).
fn looks_like_opencode_key(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("sk-") else {
        return false;
    };
    rest.len() == 64 && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Pull an OpenCode Go key out of opencode's own `auth.json`.
///
/// The file is a map of provider id → credential, and the two shapes that
/// matter are the API key (`{"opencode-go": {"type": "api", "key": "…"}}`) and
/// the OAuth login for the same provider. Both carry the credential that the
/// gateway wants as a bearer token, so both are accepted — but only for an id
/// that names Go or Zen, because a ChatGPT or Anthropic login parked in this
/// file is a Codex/Claude account and belongs on those providers instead.
///
/// Deliberately *not* `find`-based like the Codex/Claude readers: `find` walks
/// the whole tree for the first object with a matching key, which would happily
/// return the `google` or `openrouter` entry from the same file.
fn opencode_login(v: &Value) -> Option<String> {
    let m = v.as_object()?;
    for (id, cred) in m {
        // Tolerate the trailing-slash variant opencode itself strips.
        let id = id.trim_end_matches('/');
        if id != "opencode-go" && id != "opencode" {
            continue;
        }
        let c = cred.as_object()?;
        let key = ["key", "access", "token", "apiKey"]
            .iter()
            .find_map(|k| c.get(*k).and_then(|x| x.as_str()))
            .unwrap_or_default()
            .trim()
            .to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

fn codex_meta(access: &str, id_token: &str) -> (String, String, i64) {
    let c = jwt_claims(access);
    let idc = jwt_claims(id_token);
    let acct = c
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| idc.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let email = c
        .pointer("/https:~1~1api.openai.com~1profile/email")
        .or_else(|| idc.get("email"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (acct, email, c["exp"].as_i64().unwrap_or(0))
}

/// Find the first object in `v` (searched recursively) that has any of `keys`.
fn find<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match v {
        Value::Object(m) => {
            if keys.iter().any(|k| m.contains_key(*k)) {
                return Some(v);
            }
            m.values().find_map(|x| find(x, keys))
        }
        _ => None,
    }
}

fn s(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| v[*k].as_str())
        .unwrap_or("")
        .to_string()
}

/// Accounts whose "login" is a long-lived API key rather than OAuth tokens.
///
/// OpenCode Go and Kimi Code are the odd ones out: you subscribe, copy a key
/// the vendor documents for third-party tools, and paste it. Neither expires
/// and neither has a refresh endpoint, so every piece of the OAuth machinery —
/// expiry accounting, `fresh`'s refresh call, writing rotated tokens back to
/// the CLI's file — has to skip them rather than fail on them. Kept as one
/// predicate so the call sites cannot drift apart.
pub fn key_only(kind: &str) -> bool {
    kind == "opencode-go" || kind == "kimi-code"
}

/// Parse a pasted file/token. `hint` = codex | claude | opencode-go | "" (guess).
pub fn parse(text: &str, hint: &str, source: &str) -> Result<Account, String> {
    let text = text.trim();
    let mut a = Account {
        id: super::new_id(),
        enabled: true,
        source: source.into(),
        ..Default::default()
    };
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if let Some(o) = find(&v, &["claudeAiOauth"])
            .map(|x| &x["claudeAiOauth"])
            .or_else(|| find(&v, &["accessToken"]))
        {
            a.kind = "claude".into();
            a.access_token = s(o, &["accessToken"]);
            a.refresh_token = s(o, &["refreshToken"]);
            a.expires_at = o["expiresAt"].as_i64().map(|ms| ms / 1000).unwrap_or(0);
            a.label = s(o, &["subscriptionType"]);
        } else if let Some(o) = find(&v, &["CLAUDE_CODE_OAUTH_TOKEN"]) {
            a.kind = "claude".into();
            a.access_token = s(o, &["CLAUDE_CODE_OAUTH_TOKEN"]);
        } else if let Some(k) = opencode_login(&v) {
            // opencode's own auth.json. Checked before the generic `access_token`
            // hunt below: that one would otherwise claim this file as a Codex
            // login, because the same OAuth blob shape appears in both.
            a.kind = "opencode-go".into();
            a.access_token = k;
        } else if let Some(o) = find(&v, &["access_token", "access"]) {
            a.kind = "codex".into();
            a.access_token = s(o, &["access_token", "access"]);
            a.refresh_token = s(o, &["refresh_token", "refresh"]);
            let (acct, email, exp) = codex_meta(&a.access_token, &s(o, &["id_token"]));
            a.account_id = [s(o, &["account_id", "accountId"]), acct]
                .into_iter()
                .find(|x| !x.is_empty())
                .unwrap_or_default();
            a.email = email;
            a.expires_at = o["expires"]
                .as_i64()
                .map(|ms| if ms > 10_000_000_000 { ms / 1000 } else { ms })
                .unwrap_or(exp);
            if a.access_token.starts_with("sk-ant-") {
                a.kind = "claude".into();
            }
        } else {
            return Err("Couldn't find a login in that JSON. Paste ~/.codex/auth.json, ~/.claude/.credentials.json, or a token.".into());
        }
    } else if text.starts_with("sk-ant-oat") {
        a.kind = "claude".into();
        a.access_token = text.into();
    } else if hint == "opencode-go" || hint == "kimi-code" || looks_like_opencode_key(text) {
        // A pasted key-login credential: Go's workspace key, or a Kimi Code
        // key from the console. Only when the hint names the provider (the
        // connect dialog is per-provider) or the shape is unmistakably Go's:
        // `parse` is also the path for pasting a bare Codex JWT, so a loose
        // shape match here would steal that login — which is also why this
        // branch sits ahead of the two-dot JWT heuristic below. Kimi keys
        // have no documented shape to match, so there the hint is what makes
        // the paste unambiguous, and the probe on import is what proves the
        // key works. The kind follows the *matched* reason, not the hint, so
        // a Go-shaped key pasted into the Claude dialog still errors instead
        // of being accepted as a Claude account.
        a.kind = if hint == "kimi-code" { "kimi-code" } else { "opencode-go" }.into();
        a.access_token = text.into();
    } else if text.matches('.').count() == 2 && text.len() > 100 {
        a.kind = "codex".into();
        a.access_token = text.into();
        let (acct, email, exp) = codex_meta(text, "");
        a.account_id = acct;
        a.email = email;
        a.expires_at = exp;
    } else {
        return Err("That doesn't look like a Codex or Claude login.".into());
    }
    if a.access_token.is_empty() {
        return Err(if a.kind == "claude" {
            "That Claude login has no token in it: Claude Code on this PC keeps the real login in the system credential store, not the file. Run `claude setup-token` and paste the sk-ant-oat… token it prints (lasts a year).".into()
        } else {
            "The login has no access token.".into()
        });
    }
    if !hint.is_empty() && hint != a.kind {
        return Err(format!("That's a {} login, not {}.", a.kind, hint));
    }
    if a.kind == "codex" && a.account_id.is_empty() {
        return Err(
            "The Codex login has no ChatGPT account id. Log in again with `codex login`.".into(),
        );
    }
    Ok(a)
}

pub fn default_path(kind: &str) -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    Some(match kind {
        "codex" => std::env::var("CODEX_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or(home.join(".codex"))
            .join("auth.json"),
        // opencode resolves its data dir through the xdg-basedir crate, which has
        // no Windows branch — so Windows really does get `~/.local/share/opencode`
        // and not `%APPDATA%`. Hardcoding the Windows-local path here would point
        // the importer at a file that never exists.
        "opencode-go" => std::env::var("OPENCODE_DATA_DIR")
            .or_else(|_| std::env::var("XDG_DATA_HOME"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| home.join(".local").join("share"))
            .join("opencode")
            .join("auth.json"),
        // The Kimi Code CLI keeps an OAuth session under `~/.kimi-code/`, but
        // that is the CLI's own login, not a credential OpenLeash may replay —
        // the same line the Antigravity integration refuses to cross, and
        // Kimi's terms tie the client identity to the account. The importable
        // credential is the key the console hands out for exactly this use.
        "kimi-code" => return None,
        _ => home.join(".claude").join(".credentials.json"),
    })
}

/// Write rotated tokens back into the file the login came from, keeping its layout.
fn write_back(a: &Account) {
    if a.source.is_empty() || key_only(&a.kind) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&a.source) else {
        return;
    };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    fn walk(v: &mut Value, a: &Account) -> bool {
        let Value::Object(m) = v else { return false };
        if m.contains_key("claudeAiOauth") {
            let o = &mut m["claudeAiOauth"];
            o["accessToken"] = json!(a.access_token);
            o["refreshToken"] = json!(a.refresh_token);
            o["expiresAt"] = json!(a.expires_at * 1000);
            return true;
        }
        if m.contains_key("access_token") {
            m.insert("access_token".into(), json!(a.access_token));
            m.insert("refresh_token".into(), json!(a.refresh_token));
            return true;
        }
        if m.contains_key("access") && m.contains_key("refresh") {
            m.insert("access".into(), json!(a.access_token));
            m.insert("refresh".into(), json!(a.refresh_token));
            m.insert("expires".into(), json!(a.expires_at * 1000));
            return true;
        }
        m.values_mut().any(|x| walk(x, a))
    }
    if walk(&mut v, a) {
        if let Ok(out) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::write(&a.source, out);
        }
    }
}

// ───────────────────────────── refresh ─────────────────────────────

/// Persist a refreshed account.
///
/// Written now, not through the debounced writer: providers rotate refresh
/// tokens, so the one just used is dead, and a crash inside the debounce
/// window would leave only that dead token on disk — the user logged out for
/// no reason. Update and clone under the lock, then release it before the
/// serialization/fsync work on the blocking pool. Every model request reads
/// settings, so holding the write lock across disk I/O stalled unrelated chats.
async fn save_account(h: &Harness, a: &Account) {
    let snapshot = {
        let mut s = h.settings.write().await;
        if let Some(x) = s.accounts.iter_mut().find(|x| x.id == a.id) {
            *x = a.clone();
        }
        s.clone()
    };
    let _ = tokio::task::spawn_blocking(move || store::save_settings(&snapshot)).await;
}

/// A usable access token for this account, refreshing first if it's about to expire.
pub async fn fresh(h: &Harness, id: &str, force: bool) -> Result<Account, String> {
    let get = || async {
        h.settings
            .read()
            .await
            .accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| "account removed".to_string())
    };
    let a = get().await?;
    // A key-login account has no expiry and no refresh endpoint. `stale` would
    // be false anyway (expires_at is 0), but `force` — the "the provider just
    // rejected this" path in the router — would otherwise POST garbage to a
    // refresh endpoint that does not exist for this kind, and report a network
    // error where the honest answer is that the credential is simply wrong.
    if key_only(&a.kind) {
        return Ok(a);
    }
    let stale = a.expires_at > 0 && a.expires_at - now() < 300;
    if !(force || stale) || a.refresh_token.is_empty() {
        return Ok(a);
    }
    let _g = h.accts.refresh_lock.lock().await;
    // Someone else may have refreshed while we waited.
    let mut a = get().await?;
    if !force && a.expires_at - now() >= 300 {
        return Ok(a);
    }
    let res = if a.kind == "codex" {
        h.http
            .post(CODEX_TOKEN_URL)
            .form(&[
                ("client_id", CODEX_CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", a.refresh_token.as_str()),
                ("scope", "openid profile email"),
            ])
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
    } else {
        let mut last = None;
        for url in CLAUDE_TOKEN_URLS {
            let r = h
                .http
                .post(url)
                .header("user-agent", CLAUDE_UA)
                .json(&json!({"grant_type": "refresh_token", "refresh_token": a.refresh_token, "client_id": CLAUDE_CLIENT_ID}))
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await;
            let not_found = r.as_ref().is_ok_and(|r| r.status().as_u16() == 404);
            last = Some(r);
            if !not_found {
                break;
            }
        }
        // `last` is only empty if CLAUDE_TOKEN_URLS is empty; degrade to a clear
        // error rather than aborting the process (the release profile is panic=abort).
        match last {
            Some(r) => r,
            None => return Err("no Claude token endpoint configured".into()),
        }
    };
    let resp = res.map_err(|e| format!("token refresh failed: {e}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        if matches!(status.as_u16(), 400 | 401 | 403) {
            a.disabled_reason = "Login expired · re-import this account".into();
            save_account(h, &a).await;
        }
        return Err(format!("token refresh rejected (HTTP {})", status.as_u16()));
    }
    let access = v["access_token"]
        .as_str()
        .ok_or("refresh returned no token")?
        .to_string();
    a.access_token = access;
    if let Some(r) = v["refresh_token"].as_str() {
        a.refresh_token = r.into();
    }
    if a.kind == "codex" {
        let (acct, email, exp) = codex_meta(&a.access_token, v["id_token"].as_str().unwrap_or(""));
        if !acct.is_empty() {
            a.account_id = acct;
        }
        if !email.is_empty() {
            a.email = email;
        }
        a.expires_at = exp;
    } else {
        a.expires_at = now() + v["expires_in"].as_i64().unwrap_or(28_800);
    }
    a.disabled_reason.clear();
    save_account(h, &a).await;
    write_back(&a);
    Ok(a)
}

// ───────────────────────────── usage ─────────────────────────────

fn iso(v: &Value) -> i64 {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .or_else(|| v.as_i64())
        .unwrap_or(0)
}

/// Send the smallest possible Claude request and read usage from its
/// rate-limit headers. Also proves the token works. Setup tokens
/// (`claude setup-token`) can't call the usage endpoint, so this is their path.
pub async fn claude_probe(h: &Harness, a: &Account) -> Result<(), String> {
    let body = json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 1,
        "system": [{"type": "text", "text": CLAUDE_PRELUDE}],
        "messages": [{"role": "user", "content": "."}],
    });
    let resp = h
        .http
        .post(claude_url())
        .bearer_auth(&a.access_token)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", CLAUDE_BETA)
        .header("user-agent", CLAUDE_UA)
        .header("x-app", "cli")
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("couldn't reach Claude: {e}"))?;
    let status = resp.status().as_u16();
    h.accts.update_from_headers("claude", &a.id, resp.headers());
    let text = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    match status {
        200..=299 | 429 => {
            // 429 still carries the limit headers: the token is fine, the account is just out.
            let mut st = h.accts.state.lock().unwrap();
            let s = st.entry(a.id.clone()).or_default();
            let mut u = s.usage.clone().unwrap_or_default();
            u.fetched = now();
            u.error.clear();
            if u.windows.is_empty() {
                u.error = if status == 429 {
                    "Out of usage (no reset time given)".into()
                } else {
                    "Token works · Claude didn't report limits".into()
                };
            }
            if status == 429 {
                u.limited = true;
            }
            s.usage = Some(u);
            Ok(())
        }
        401 | 403 => Err(format!(
            "Claude rejected this token ({status}){}. Make a fresh one with `claude setup-token`.",
            if msg.is_empty() {
                String::new()
            } else {
                format!(": {msg}")
            }
        )),
        _ => Err(format!(
            "Claude answered HTTP {status}{}",
            if msg.is_empty() {
                String::new()
            } else {
                format!(": {msg}")
            }
        )),
    }
}

/// Prove an OpenCode Go key works before saving it, and read its limits in the
/// same call — the usage endpoint is the cheapest thing that authenticates.
///
/// Mirrors `claude_probe`: no silent duds. Unlike Claude there is no separate
/// "logged in but out of quota" case to tolerate — a valid key on a workspace
/// with no Go subscription is the one failure worth letting through, because the
/// key genuinely works and Zen still answers it. That case is recorded on the
/// account as its error rather than as a refusal to save.
pub async fn go_probe(h: &Harness, a: &Account) -> Result<(), String> {
    let resp = h
        .http
        .get(go_url("usage"))
        .bearer_auth(&a.access_token)
        .header("user-agent", super::providers::OPENCODE_UA)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("couldn't reach OpenCode: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status == 401 {
        return Err("OpenCode rejected that key. Copy it again from the OpenCode console.".into());
    }
    if status == 403 {
        // EntitlementError: the key is fine, the workspace just isn't on Go.
        return Err(v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| "That key has no OpenCode Go subscription.".into()));
    }
    if status >= 400 {
        return Err(format!(
            "OpenCode answered HTTP {status} checking that key."
        ));
    }
    Ok(())
}

/// The Kimi Code gateway root, honoring the provider's base URL override —
/// the two region hosts serve the same API and a key works on one of them,
/// so the override is also how a key bought on the other region gets used.
/// The settings are cloned rather than held across the request: every model
/// request reads them, and holding the lock across I/O stalls other chats.
async fn kimi_base(h: &Harness) -> String {
    let settings = h.settings.read().await.clone();
    super::providers::base_url(&settings, "kimi-code")
}

/// The probe's verdict for one HTTP status: `None` accepts the key, `Some`
/// refuses it with the message the user sees.
///
/// 404 counts as acceptance-eligible (the caller retries `/models`): a gateway
/// that simply does not publish the usage endpoint says nothing about whether
/// the key works, and refusing a working key over a missing monitor endpoint
/// would lock the account out at import.
fn kimi_auth_error(status: u16) -> Option<String> {
    match status {
        200..=299 | 404 => None,
        401 | 403 => Some(
            "Kimi rejected that key. Create a fresh one in the Kimi Code console \
             (kimi.com/code/console) — and if your plan is on the other region's \
             host, point the provider's base URL there first."
                .into(),
        ),
        s => Some(format!("Kimi answered HTTP {s} checking that key.")),
    }
}

async fn kimi_get(h: &Harness, base: &str, path: &str, a: &Account) -> Result<u16, String> {
    Ok(h
        .http
        .get(format!("{base}{path}"))
        .bearer_auth(&a.access_token)
        .header("user-agent", super::providers::KIMI_CODE_UA)
        .header("accept", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("couldn't reach Kimi: {e}"))?
        .status()
        .as_u16())
}

/// Prove a Kimi Code key works before saving it, and read its limits in the
/// same call — the usage endpoint is the cheapest thing that authenticates,
/// exactly as on Go. The endpoint lives on the gateway the provider's base
/// URL points at, so a key from the other region's host fails here with the
/// fix in the message rather than a bare 401.
pub async fn kimi_probe(h: &Harness, a: &Account) -> Result<(), String> {
    let base = kimi_base(h).await;
    let mut status = kimi_get(h, &base, "/usages", a).await?;
    // A 404 says nothing about the key (see `kimi_auth_error`): fall back to
    // /models, the cheapest other authenticated read, before refusing. The
    // usage row reports the same after the account is saved.
    if status == 404 {
        status = kimi_get(h, &base, "/models", a).await?;
    }
    match kimi_auth_error(status) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Parse Kimi Code's `/usages` payload into usage windows.
///
/// `{"usages": {"limit_5h": {"used_ratio": 0..1, "reset_time": ISO}, …}}` —
/// the same windows the official CLI's `/usage` reads. The weekly window
/// exists only on legacy plans, and the monthly total is the whole Kimi
/// membership's shared quota, not a coding-specific one. `used_ratio` arrives
/// as a number or a numeric string; either way it is a fraction, not the
/// percent the other kinds report, so it is scaled here where the scaling
/// cannot be forgotten.
pub fn parse_kimi_usage(v: &Value, u: &mut AcctUsage) {
    for (k, label) in [
        ("limit_5h", "5h"),
        ("limit_7d", "Week"),
        ("limit_month_total", "Month"),
    ] {
        let w = &v["usages"][k];
        if w.is_null() {
            continue;
        }
        let ratio = w["used_ratio"]
            .as_f64()
            .or_else(|| w["used_ratio"].as_str().and_then(|s| s.parse::<f64>().ok()))
            .filter(|r| r.is_finite());
        let Some(ratio) = ratio else {
            continue;
        };
        u.windows.push(Window {
            label: label.into(),
            used: (ratio * 100.0).clamp(0.0, 100.0),
            resets_at: iso(&w["reset_time"]),
        });
    }
    u.limited = u.windows.iter().any(|w| w.used >= 99.5);
    // The usage payload names no plan, and `/me`'s level fields are
    // gamification tiers rather than the membership plan, so "Kimi Code"
    // is all that can honestly be said here — the same rule that has Go
    // reporting "Go".
    u.plan = "Kimi Code".into();
}

pub async fn fetch_usage(h: &Harness, id: &str) -> Result<AcctUsage, String> {
    let res = fetch_usage_inner(h, id).await;
    // Never lose an error: the Accounts screen shows it on the account.
    if let Err(e) = &res {
        let mut st = h.accts.state.lock().unwrap();
        let s = st.entry(id.to_string()).or_default();
        let mut u = s.usage.clone().unwrap_or_default();
        u.error = e.clone();
        u.fetched = now();
        s.usage = Some(u);
    }
    res
}

async fn fetch_usage_inner(h: &Harness, id: &str) -> Result<AcctUsage, String> {
    let a = fresh(h, id, false).await?;
    // Setup tokens have no refresh token and no profile scope: go straight to the header probe.
    if a.kind == "claude" && a.refresh_token.is_empty() {
        claude_probe(h, &a).await?;
        return h
            .accts
            .state
            .lock()
            .unwrap()
            .get(id)
            .and_then(|s| s.usage.clone())
            .ok_or_else(|| "no usage reported".to_string());
    }
    let rb = if a.kind == "codex" {
        h.http
            .get(format!("{}/wham/usage", codex_base()))
            .bearer_auth(&a.access_token)
            .header("chatgpt-account-id", &a.account_id)
            .header("user-agent", CODEX_UA)
            .header("originator", "codex_cli_rs")
    } else if a.kind == "opencode-go" {
        h.http
            .get(go_url("usage"))
            .bearer_auth(&a.access_token)
            .header("user-agent", crate::agent::providers::OPENCODE_UA)
    } else if a.kind == "kimi-code" {
        h.http
            .get(format!("{}/usages", kimi_base(h).await))
            .bearer_auth(&a.access_token)
            .header("user-agent", super::providers::KIMI_CODE_UA)
            .header("accept", "application/json")
    } else {
        h.http
            .get(format!("{}/api/oauth/usage", anthropic_base()))
            .bearer_auth(&a.access_token)
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("user-agent", CLAUDE_UA)
    };
    let resp = rb
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("usage request failed: {e}"))?;
    let status = resp.status();
    if a.kind == "claude" && matches!(status.as_u16(), 401 | 403) {
        // Token may lack the usage scope; fall back to reading headers off a tiny request.
        let a = if status.as_u16() == 401 && !a.refresh_token.is_empty() {
            fresh(h, id, true).await?
        } else {
            a
        };
        claude_probe(h, &a).await?;
        return h
            .accts
            .state
            .lock()
            .unwrap()
            .get(id)
            .and_then(|s| s.usage.clone())
            .ok_or_else(|| "no usage reported".to_string());
    }
    if status.as_u16() == 401 {
        fresh(h, id, true).await?;
        return Err(if a.kind == "opencode-go" {
            "OpenCode rejected this key. Check it in the OpenCode console, or import auth.json again."
                .into()
        } else if a.kind == "kimi-code" {
            "Kimi rejected this key. Check it in the Kimi Code console, or import it again."
                .into()
        } else {
            "Login was stale; refreshed it. Hit Refresh again.".to_string()
        });
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad usage response: {e}"))?;
    if !status.is_success() {
        // Go answers 403 EntitlementError for a valid key on a workspace with no
        // Go subscription. That is not a broken account — the key still works
        // against Zen — so it is reported as the state it is.
        if a.kind == "opencode-go" && status.as_u16() == 403 {
            return Err(v
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(String::from)
                .unwrap_or_else(|| "No OpenCode Go subscription on this key".into()));
        }
        return Err(format!("usage endpoint said HTTP {}", status.as_u16()));
    }
    let mut u = AcctUsage {
        fetched: now(),
        ..Default::default()
    };
    if a.kind == "opencode-go" {
        // `{"usage": {"rolling": {status, percent, resetsAt}, "weekly": …, "monthly": …}}`
        // — percent is already 0-100, unlike Anthropic's 0-1 utilization, and the
        // reset is an ISO timestamp rather than a countdown.
        for (k, label) in [("rolling", "5h"), ("weekly", "Week"), ("monthly", "Month")] {
            let w = &v["usage"][k];
            if w.is_null() {
                continue;
            }
            u.windows.push(Window {
                label: label.into(),
                used: w["percent"].as_f64().unwrap_or(0.0),
                resets_at: iso(&w["resetsAt"]),
            });
        }
        u.limited = v["usage"]["rolling"]["status"].as_str() == Some("rate-limited")
            || u.windows.iter().any(|w| w.used >= 99.5);
        // The plan name is not in the usage payload; the limit table is per model,
        // so "Go" is all that can honestly be said here.
        u.plan = "Go".into();
    } else if a.kind == "codex" {
        u.plan = v["plan_type"].as_str().unwrap_or("").into();
        let rl = &v["rate_limit"];
        for (k, fallback) in [("primary_window", "5h"), ("secondary_window", "Week")] {
            let w = &rl[k];
            if w.is_null() {
                continue;
            }
            let secs = w["limit_window_seconds"].as_i64().unwrap_or(0);
            let reset = w["reset_at"]
                .as_i64()
                .or_else(|| w["reset_after_seconds"].as_i64().map(|s| now() + s))
                .unwrap_or(0);
            u.windows.push(Window {
                label: if secs > 0 {
                    window_label(secs)
                } else {
                    fallback.into()
                },
                used: w["used_percent"].as_f64().unwrap_or(0.0),
                resets_at: reset,
            });
        }
        u.limited = rl["limit_reached"].as_bool().unwrap_or(false)
            || rl["allowed"].as_bool() == Some(false);
    } else if a.kind == "kimi-code" {
        parse_kimi_usage(&v, &mut u);
    } else {
        for (k, label) in [
            ("five_hour", "5h"),
            ("seven_day", "Week"),
            ("seven_day_opus", "Week · Opus"),
            ("seven_day_sonnet", "Week · Sonnet"),
        ] {
            let w = &v[k];
            if w.is_null() {
                continue;
            }
            u.windows.push(Window {
                label: label.into(),
                used: w["utilization"].as_f64().unwrap_or(0.0),
                resets_at: iso(&w["resets_at"]),
            });
        }
        u.limited = u.windows.iter().take(2).any(|w| w.used >= 99.5);
        if a.email.is_empty() {
            if let Ok(r) = h
                .http
                .get(format!("{}/api/oauth/profile", anthropic_base()))
                .bearer_auth(&a.access_token)
                .header("anthropic-beta", "oauth-2025-04-20")
                .header("user-agent", CLAUDE_UA)
                .send()
                .await
            {
                if let Ok(p) = r.json::<Value>().await {
                    let email = p
                        .pointer("/account/email_address")
                        .or(p.pointer("/account/email"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let plan = p
                        .pointer("/organization/organization_type")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut a2 = a.clone();
                    if !email.is_empty() {
                        a2.email = email;
                        save_account(h, &a2).await;
                    }
                    u.plan = plan;
                }
            }
        }
    }
    h.accts.set_usage(id, u.clone());
    Ok(u)
}

/// Ask a subscription which models it offers, using its best available account.
/// Returns how many were found; on failure the built-in list stays.
pub async fn fetch_pool_models(h: &Harness, kind: &str) -> Result<usize, String> {
    let accts = h.settings.read().await.accounts.clone();
    let selected = pool(h, &accts, kind).into_iter().next();
    let key_login = super::providers::key_login(kind);
    let (key, account_id) = match selected {
        Some(a) => {
            let a = fresh(h, &a.id, false).await?;
            (a.access_token, a.account_id)
        }
        None if key_login => {
            let settings = h.settings.read().await.clone();
            (super::providers::api_key(&settings, kind), String::new())
        }
        None => return Err(format!("no {kind} accounts")),
    };
    if key.is_empty() {
        return Err(format!("no {kind} account or API key"));
    }
    let rb = if kind == "codex" {
        h.http
            .get(format!(
                "{}/codex/models?client_version=0.46.0",
                codex_base()
            ))
            .bearer_auth(&key)
            .header("chatgpt-account-id", account_id)
            .header("originator", "codex_cli_rs")
            .header("user-agent", CODEX_UA)
    } else if kind == "opencode-go" {
        h.http
            .get(go_url("models"))
            .bearer_auth(&key)
            .header("user-agent", super::providers::OPENCODE_UA)
    } else if kind == "kimi-code" {
        let settings = h.settings.read().await.clone();
        h.http
            .get(format!(
                "{}/models",
                super::providers::base_url(&settings, "kimi-code")
            ))
            .bearer_auth(&key)
            .header("user-agent", super::providers::KIMI_CODE_UA)
    } else {
        h.http
            .get(format!("{}/v1/models?limit=100", anthropic_base()))
            .bearer_auth(&key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("user-agent", CLAUDE_UA)
    };
    let resp = rb
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("model list request failed: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad model list: {e}"))?;
    if !status.is_success() {
        return Err(format!("model list: HTTP {}", status.as_u16()));
    }
    let models = match kind {
        "codex" => super::providers::parse_codex_models(&v),
        "opencode-go" => super::providers::parse_go_models(&v),
        "kimi-code" => super::providers::parse_kimi_models(&v),
        _ => super::providers::parse_claude_models(&v),
    };
    let n = models.len();
    if n > 0 {
        super::providers::set_pool_models(kind, models);
    }
    Ok(n)
}

/// Refresh every account's usage now (and on a timer from `lib.rs`).
pub async fn refresh_all(h: &Harness) {
    let ids: Vec<String> = h
        .settings
        .read()
        .await
        .accounts
        .iter()
        .filter(|a| a.enabled)
        .map(|a| a.id.clone())
        .collect();
    let futs = ids.iter().map(|id| async move {
        let _ = fetch_usage(h, id).await;
    });
    futures_util::future::join_all(futs).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rotated refresh token is on disk by the time `save_account` returns
    /// (a crash a moment later must not strand the user on a dead token), and
    /// the settings lock is free again afterwards.
    #[tokio::test]
    // The `test_home` guard serializes `OPENLEASH_HOME` for the whole test.
    #[allow(clippy::await_holding_lock)]
    async fn a_refreshed_account_is_on_disk_when_save_returns() {
        let home =
            std::env::temp_dir().join(format!("ol-acct-save-{}", uuid::Uuid::new_v4().simple()));
        let _home = store::test_home(&home);
        let old = Account {
            id: "a1".into(),
            kind: "claude".into(),
            refresh_token: "dummy-refresh-old".into(),
            ..Default::default()
        };
        let mut settings = store::Settings::default();
        settings.accounts.push(old.clone());
        let (h, _) = crate::agent::tests::harness(settings, vec![]);
        let new = Account {
            refresh_token: "dummy-refresh-rotated".into(),
            ..old
        };
        save_account(&h, &new).await;
        let disk = std::fs::read_to_string(store::data_dir().join("settings.json")).unwrap();
        assert!(disk.contains("dummy-refresh-rotated"));
        assert!(!disk.contains("dummy-refresh-old"));
        assert!(h.settings.try_write().is_ok(), "no guard outlives the save");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn account_views_only_expose_masked_keys_for_key_login_kinds() {
        let key = format!("sk-{}AB12", "x".repeat(60));
        let kimi = format!("kimi-{}XY90", "k".repeat(60));
        let accounts: Vec<Account> = [
            ("opencode-go", key.as_str()),
            ("codex", "dummy-codex-oauth-access"),
            ("claude", "sk-ant-oat-dummy-claude-access"),
            // Go's auth.json can carry an OAuth login for the same provider
            // id; hinting a fragment of a session token is exactly the leak
            // the shape check refuses, so an unrecognized token gets nothing.
            ("opencode-go", "dummy-opencode-oauth-access"),
            ("opencode-go", "short"),
            ("opencode-go", ""),
            ("opencode-go", "sk-😀dummy-unrecognized-key"),
            ("codex", key.as_str()),
            // Kimi Code's credential is a pasted console key by construction
            // (parse accepts nothing else for the kind, and there is no local
            // OAuth import), so the hint applies without a shape check.
            ("kimi-code", kimi.as_str()),
            ("kimi-code", "short"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (kind, access))| Account {
            id: format!("account-{i}"),
            kind: kind.into(),
            email: format!("account-{i}@example.invalid"),
            access_token: access.into(),
            refresh_token: format!("dummy-refresh-secret-{i}"),
            ..Default::default()
        })
        .collect();
        let views = AccountRt::default().views(&accounts);
        assert_eq!(views[0].key_hint, "…AB12");
        assert!(views[1..8].iter().all(|view| view.key_hint.is_empty()));
        assert_eq!(views[8].key_hint, "…XY90");
        // Under the 4-char-preview threshold `key_hint` itself stays silent —
        // the UI falls back to the email, not to a hint of nothing.
        assert!(views[9].key_hint.is_empty());
        let serialized = serde_json::to_string(&views).unwrap();
        for (account, view) in accounts.iter().zip(&views) {
            assert_eq!(view.email, account.email);
            assert!(!serialized.contains(&account.refresh_token));
            if !account.access_token.is_empty() {
                assert!(!serialized.contains(&account.access_token));
            }
        }
        let value = serde_json::to_value(&views).unwrap();
        for view in value.as_array().unwrap() {
            assert!(view.get("access_token").is_none());
            assert!(view.get("refresh_token").is_none());
        }
    }

    #[test]
    fn codex_placeholder_headers_do_not_replace_real_usage() {
        let rt = AccountRt::default();
        rt.set_usage(
            "codex",
            usage_of(&[("5h", 42.0, 1_900_000_000), ("Week", 25.0, 1_900_100_000)]),
        );
        let headers = reqwest::header::HeaderMap::from_iter([
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-used-percent"),
                "0".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-window-minutes"),
                "0".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-secondary-used-percent"),
                "0".parse().unwrap(),
            ),
        ]);
        rt.update_from_headers("codex", "codex", &headers);
        let u = rt.usage_of("codex").unwrap();
        assert_eq!(u.windows[0].used, 42.0);
        assert_eq!(u.windows[1].used, 25.0);
        assert_eq!(u.windows[0].resets_at, 1_900_000_000);
    }

    #[test]
    fn codex_partial_headers_update_only_the_reported_window() {
        let rt = AccountRt::default();
        rt.set_usage(
            "codex",
            usage_of(&[("5h", 42.0, 1_900_000_000), ("Week", 25.0, 1_900_100_000)]),
        );
        let headers = reqwest::header::HeaderMap::from_iter([
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-used-percent"),
                "0".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-secondary-used-percent"),
                "26".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-secondary-reset-after-seconds"),
                "600".parse().unwrap(),
            ),
        ]);
        rt.update_from_headers("codex", "codex", &headers);
        let u = rt.usage_of("codex").unwrap();
        assert_eq!(u.windows[0].used, 42.0);
        assert_eq!(u.windows[1].used, 26.0);
        assert!((now() + 599..=now() + 601).contains(&u.windows[1].resets_at));
    }

    #[test]
    fn codex_real_zero_usage_and_absolute_reset_headers_are_accepted() {
        let rt = AccountRt::default();
        rt.set_usage("codex", usage_of(&[("5h", 42.0, 1_900_000_000)]));
        let headers = reqwest::header::HeaderMap::from_iter([
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-used-percent"),
                "0".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-window-minutes"),
                "300".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-reset-at"),
                "1900000100".parse().unwrap(),
            ),
            (
                reqwest::header::HeaderName::from_static("x-codex-primary-reset-after-seconds"),
                "600".parse().unwrap(),
            ),
        ]);
        rt.update_from_headers("codex", "codex", &headers);
        let u = rt.usage_of("codex").unwrap();
        assert_eq!(
            u.windows[0].used, 0.0,
            "a genuine reset must still lower usage"
        );
        assert_eq!(
            u.windows[0].resets_at, 1_900_000_100,
            "absolute timestamp takes precedence"
        );
    }

    #[test]
    fn codex_invalid_header_percentages_leave_the_last_reading_intact() {
        let rt = AccountRt::default();
        rt.set_usage("codex", usage_of(&[("5h", 42.0, 1_900_000_000)]));
        for value in ["NaN", "inf", "-1", "101"] {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("x-codex-primary-used-percent", value.parse().unwrap());
            headers.insert("x-codex-primary-window-minutes", "300".parse().unwrap());
            rt.update_from_headers("codex", "codex", &headers);
            assert_eq!(
                rt.usage_of("codex").unwrap().windows[0].used,
                42.0,
                "{value}"
            );
        }
    }

    #[test]
    fn parses_cli_files() {
        let claude = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"sk-ant-ort01-y","expiresAt":1900000000000,"subscriptionType":"max"}}"#;
        let a = parse(claude, "", "").unwrap();
        assert_eq!(a.kind, "claude");
        assert_eq!(a.expires_at, 1_900_000_000);
        assert_eq!(
            parse("sk-ant-oat01-abc", "claude", "").unwrap().kind,
            "claude"
        );
        let codex =
            r#"{"tokens":{"access_token":"a.b.c","refresh_token":"r","account_id":"acc-1"}}"#;
        let c = parse(codex, "", "").unwrap();
        assert_eq!((c.kind.as_str(), c.account_id.as_str()), ("codex", "acc-1"));
        assert!(parse(codex, "claude", "").is_err());
        assert_eq!(window_label(18_000), "5h");
        assert_eq!(window_label(604_800), "Week");
        let empty = r#"{"mcpOAuth":{"x":{"accessToken":""}},"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":1}}"#;
        assert!(parse(empty, "claude", "")
            .unwrap_err()
            .contains("setup-token"));
    }

    /// OpenCode Go is the third account kind, and the only one whose *login* is
    /// a bare key. The import has to take it from both shapes it comes in —
    /// opencode's own auth.json and a pasted key — while never stealing a Codex
    /// or Claude login, because `parse` is one shared entry point and the hint
    /// is not always supplied.
    #[test]
    fn parses_opencode_go_keys() {
        let key = format!("sk-{}", "a".repeat(64));
        // opencode's auth.json: a map of provider id -> credential.
        let auth = json!({"opencode-go": {"type": "api", "key": key}, "google": {"type": "api", "key": "AIza-should-not-win"}}).to_string();
        let a = parse(&auth, "opencode-go", "").unwrap();
        assert_eq!(
            (a.kind.as_str(), a.access_token.as_str()),
            ("opencode-go", key.as_str())
        );
        // A bare key, with and without the hint.
        assert_eq!(parse(&key, "opencode-go", "").unwrap().kind, "opencode-go");
        assert_eq!(parse(&key, "", "").unwrap().kind, "opencode-go");
        // A Zen entry is the same workspace key, so it is accepted too.
        assert_eq!(
            parse(
                &json!({"opencode": {"type": "api", "key": key}}).to_string(),
                "",
                ""
            )
            .unwrap()
            .kind,
            "opencode-go"
        );
        // A ChatGPT or Anthropic login parked in that file is not ours: it
        // belongs to codex/claude, and matching on the wrong entry would send
        // the wrong credential to the wrong provider.
        assert!(parse(
            &json!({"google": {"type": "api", "key": "AIza-x"}}).to_string(),
            "",
            ""
        )
        .is_err());
        // And the shape test is deliberately strict enough not to claim a
        // 40-char OpenAI-style key or a Codex JWT.
        assert!(!looks_like_opencode_key("sk-short"));
        assert!(!looks_like_opencode_key(&format!("sk-{}", "a".repeat(63))));
        assert!(!looks_like_opencode_key(&format!(
            "sk-{}",
            "a".repeat(64) + "-x"
        )));
        assert!(looks_like_opencode_key(&key));
        // A key-login account never takes the OAuth path.
        assert!(key_only("opencode-go"));
        assert!(key_only("kimi-code"));
        assert!(!key_only("codex") && !key_only("claude"));
    }

    /// Kimi Code is the second key-login kind, and the only one with no
    /// documented key shape: the connect dialog's hint is what makes a paste
    /// unambiguous, so the kind follows the hint there — while a Go-shaped key
    /// pasted into some other provider's dialog still refuses rather than
    /// being adopted by whichever box it landed in.
    #[test]
    fn parses_kimi_code_keys_only_through_their_hint() {
        // Any non-JSON text pasted into the Kimi dialog is that dialog's key;
        // the probe on import is what proves it actually works.
        assert_eq!(
            parse("kimi-console-key-dummy", "kimi-code", "")
                .unwrap()
                .kind,
            "kimi-code"
        );
        // A Go-shaped key pasted into the Kimi dialog belongs to the dialog:
        // the hint named Kimi, and guessing Go instead would save a Kimi
        // account whose credential is a foreign key.
        assert_eq!(
            parse(&format!("sk-{}", "a".repeat(64)), "kimi-code", "")
                .unwrap()
                .kind,
            "kimi-code"
        );
        // The reverse: a Go-shaped key pasted into the Claude dialog matches
        // the shape branch, but the kind follows the matched reason — so the
        // hint check refuses it instead of saving it as a Claude account.
        assert!(parse(&format!("sk-{}", "a".repeat(64)), "claude", "").is_err());
        // JSON stays JSON: the Kimi dialog is not a license to claim a Codex
        // or Claude file, which the JSON branch classifies first and the hint
        // check then rejects.
        assert!(parse(r#"{"access_token":"x"}"#, "kimi-code", "").is_err());
        // And with no hint at all, an unrecognized paste still refuses —
        // Kimi keys have no shape to guess from.
        assert!(parse("kimi-console-key-dummy", "", "").is_err());
    }

    /// The Go endpoints hang off the gateway root, not the inference base: a
    /// request to `.../zen/go/v1/usage` is the one that reports the plan windows.
    #[test]
    fn go_endpoints_hang_off_the_gateway_root() {
        // `zen_base()` is env-overridable for tests; with it unset this is the
        // public gateway.
        if std::env::var("OPENLEASH_ZEN_BASE").is_err() {
            assert_eq!(go_url("usage"), "https://opencode.ai/zen/go/v1/usage");
            assert_eq!(go_url("models"), "https://opencode.ai/zen/go/v1/models");
        }
    }

    /// Kimi's `/usages` reports fractions (0..1), not percents, and names no
    /// plan — the scaling and the honest "Kimi Code" label happen in the
    /// parser, where a mistake would read as a 100× under-report on the row.
    #[test]
    fn kimi_usage_scales_fractions_and_skips_missing_windows() {
        let v = json!({
            "usages": {
                "limit_5h": {"used_ratio": 0.42, "reset_time": "2030-01-01T00:00:00Z"},
                "limit_7d": {"used_ratio": "0.25", "reset_time": "2030-01-06T00:00:00Z"},
                "limit_month_total": {"used_ratio": 1, "reset_time": "2030-02-01T00:00:00Z"}
            }
        });
        let mut u = AcctUsage::default();
        parse_kimi_usage(&v, &mut u);
        assert_eq!(u.plan, "Kimi Code");
        assert!(u.limited, "a full window is the limit-reached signal");
        assert_eq!(
            u.windows
                .iter()
                .map(|w| (w.label.as_str(), w.used))
                .collect::<Vec<_>>(),
            [("5h", 42.0), ("Week", 25.0), ("Month", 100.0)]
        );
        // The reset times parse through the shared ISO helper.
        assert!(u.windows.iter().all(|w| w.resets_at > 0));
        // A current plan reports no weekly window, and a junk ratio is
        // skipped rather than read as zero — a fake 0% would hide real usage.
        let mut u = AcctUsage::default();
        parse_kimi_usage(
            &json!({"usages": {"limit_5h": {"used_ratio": "not-a-number"}}}),
            &mut u,
        );
        assert!(u.windows.is_empty() && !u.limited);
        // An empty payload (the 404-fallback world) is no windows, but the
        // plan label still identifies the account kind.
        let mut u = AcctUsage::default();
        parse_kimi_usage(&json!({}), &mut u);
        assert!(u.windows.is_empty());
        assert_eq!(u.plan, "Kimi Code");
    }

    /// The probe's status handling, without HTTP: 404 must not read as "bad
    /// key" (the caller retries /models), and 401/403 must name the console
    /// and the region fix rather than a bare code.
    #[test]
    fn kimi_probe_verdicts() {
        assert!(kimi_auth_error(200).is_none());
        assert!(
            kimi_auth_error(404).is_none(),
            "404 means try /models, not bad key"
        );
        let e = kimi_auth_error(401).unwrap();
        assert!(e.contains("kimi.com/code/console"));
        assert!(e.contains("base URL"), "the other-region fix is named");
        assert_eq!(kimi_auth_error(403), kimi_auth_error(401));
        assert!(kimi_auth_error(500).unwrap().contains("500"));
    }

    // ── caps ──────────────────────────────────────────────────────────────
    //
    // A cap is the user telling the harness to stop short of spending a window.
    // It is not a cooldown, and these tests are as much about what it is *not*:
    // it has to survive the things that clear a cooldown, it has to leave the
    // windows beside it alone, and a temporary one has to be able to run out.

    /// One enabled account carrying one cap.
    fn capped(id: &str, window: &str, used: f64, until: i64) -> Account {
        Account {
            id: id.into(),
            kind: "codex".into(),
            label: id.into(),
            enabled: true,
            caps: vec![UsageCap {
                window: window.into(),
                used,
                until,
            }],
            ..Default::default()
        }
    }

    /// What a usage read leaves behind: `(window, percent used, resets at)`.
    fn usage_of(windows: &[(&str, f64, i64)]) -> AcctUsage {
        AcctUsage {
            windows: windows
                .iter()
                .map(|(label, used, resets_at)| Window {
                    label: (*label).into(),
                    used: *used,
                    resets_at: *resets_at,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn read(windows: &[(&str, f64, i64)]) -> AcctState {
        AcctState {
            usage: Some(usage_of(windows)),
            ..Default::default()
        }
    }

    #[test]
    fn free_named_model_retries_usage_limited_account_but_respects_other_blocks() {
        let rt = AccountRt::default();
        let a = Account {
            id: "only".into(),
            kind: "codex".into(),
            enabled: true,
            ..Default::default()
        };
        rt.set_usage(
            "only",
            AcctUsage {
                limited: true,
                windows: vec![Window {
                    label: "Week".into(),
                    used: 100.0,
                    resets_at: now() + 3600,
                }],
                ..Default::default()
            },
        );
        assert!(!rt.usable_for_model(&a, "codex/model"));
        assert!(rt.usable_for_model(&a, "codex/model-free"));
        assert!(rt.usable_for_model(&a, "codex/FREE-tier"));

        let capped = Account {
            caps: vec![UsageCap {
                window: "Week".into(),
                used: 50.0,
                until: 0,
            }],
            ..a.clone()
        };
        rt.set_usage("only", usage_of(&[("Week", 80.0, 0)]));
        assert!(rt.usable_for_model(&capped, "codex/model-free"));
        let disabled = Account {
            enabled: false,
            ..a
        };
        assert!(!rt.usable_for_model(&disabled, "codex/model-free"));
    }

    #[test]
    fn free_named_model_bypasses_caps_without_usage_state() {
        let rt = AccountRt::default();
        let capped = capped("only", "Week", 50.0, 0);
        rt.set_usage("only", usage_of(&[("Week", 80.0, 0)]));
        assert!(!rt.usable_for_model(&capped, "codex/model"));
        assert!(rt.usable_for_model(&capped, "codex/model-free"));
    }

    #[test]
    fn a_cap_stops_the_account_at_the_line_the_user_drew() {
        let a = capped("only", "5h", 50.0, 0);
        assert!(
            usable_with(&a, &read(&[("5h", 49.9, 0)])),
            "under the cap the account is the router's as usual"
        );
        assert!(!usable_with(&a, &read(&[("5h", 50.0, 0)])), "at the cap");
        assert!(!usable_with(&a, &read(&[("5h", 90.0, 0)])), "past the cap");
        // A cap on one window says nothing about the windows beside it.
        assert!(usable_with(
            &a,
            &read(&[("5h", 20.0, 0), ("Week", 96.0, 0)])
        ));
        // A cap for a window the provider has not reported yet is armed, not
        // spent: there is no reading to compare it against.
        assert!(usable_with(
            &capped("only", "Month", 50.0, 0),
            &read(&[("5h", 90.0, 0)])
        ));
        assert!(usable_with(&a, &AcctState::default()));
    }

    #[test]
    fn a_temporary_cap_lifts_by_itself_and_an_expired_one_holds_nothing() {
        let t = now();
        let live = capped("only", "5h", 50.0, t + 600);
        assert!(!usable_with(&live, &read(&[("5h", 90.0, 0)])));
        let spent = capped("only", "5h", 50.0, t - 1);
        assert!(
            usable_with(&spent, &read(&[("5h", 90.0, 0)])),
            "a temporary cap is not a bench: nothing has to clear it by hand"
        );
    }

    /// The badge on the row and the router's own answer are the same answer.
    #[test]
    fn the_row_draws_the_cap_the_router_is_acting_on() {
        let rt = AccountRt::default();
        let a = capped("only", "5h", 50.0, 0);
        rt.set_usage("only", usage_of(&[("5h", 80.0, 0)]));
        let v = rt.views(&[a.clone()]);
        assert!(!v[0].available, "reached: the router is skipping it");
        assert_eq!(v[0].caps.len(), 1);
        assert!(v[0].caps[0].holding, "the row has to say why it is skipped");

        rt.set_usage("only", usage_of(&[("5h", 10.0, 0)]));
        let v = rt.views(&[a]);
        assert!(v[0].available);
        assert_eq!(v[0].caps.len(), 1, "an armed cap is still shown");
        assert!(!v[0].caps[0].holding);

        // A temporary cap whose time is up can never hold anything again, so it
        // is not a cap the page offers to remove.
        let past = capped("only", "5h", 50.0, now() - 1);
        rt.set_usage("only", usage_of(&[("5h", 80.0, 0)]));
        let v = rt.views(&[past]);
        assert!(v[0].available);
        assert!(v[0].caps.is_empty());
    }

    #[test]
    fn a_capped_account_says_when_it_comes_back() {
        let rt = AccountRt::default();
        let t = now();
        let reset = t + 7200;
        let perm = capped("only", "Week", 50.0, 0);
        rt.set_usage("only", usage_of(&[("Week", 70.0, reset)]));
        assert_eq!(
            rt.next_free(&[perm.clone()]),
            reset,
            "a permanent cap lifts when the window rolls over under it"
        );
        // A temporary cap that lifts before the reset is the earlier moment.
        let temp = capped("only", "Week", 50.0, t + 600);
        assert_eq!(rt.next_free(&[temp]), t + 600);
        // A permanent cap on a window that reports no reset has no moment it
        // frees up on, so nothing here promises one: the chat stays parked until
        // the user lifts the cap, which is what setting it meant.
        rt.set_usage("only", usage_of(&[("Week", 70.0, 0)]));
        assert_eq!(rt.next_free(&[perm]), 0);
    }
}
