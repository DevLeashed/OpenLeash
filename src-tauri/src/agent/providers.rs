//! Model backends. History is stored in Anthropic block shape; each backend
//! converts on the way out:
//! - `anthropic`     Messages API (API key, Anthropic-compatible proxy, or a Claude subscription)
//! - `codex`         Responses API on a ChatGPT subscription
//! - `openai_compat` chat-completions for everything else
//!
//! One call = one attempt against one target. Retries, fallbacks, account
//! rotation and pausing live in `router.rs`.

use super::accounts::{self, now};
use super::store::{Account, Settings};
use super::Message;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct AccountProviderInfo {
    pub display_name: &'static str,
    pub short_name: &'static str,
    pub login_file: &'static str,
    pub login_command: &'static str,
    pub paste_hint: &'static str,
    pub setup_command: &'static str,
    pub token_prefix: &'static str,
    pub warning: &'static str,
    /// A standing take-it-or-leave-it notice that gates the Add flow: the UI
    /// must make the user acknowledge it before the connect dialog opens.
    /// `None` on providers that need no consent (codex), `Some` where the
    /// provider's own terms say something a user has to agree to *know*.
    pub terms_gate: Option<AccountTermsGate>,
    /// The provider is also connectable with a plain API key, so the account
    /// block adds to the key flow instead of replacing it.
    ///
    /// True only for OpenCode Go: its "login" is a workspace API key, and keys
    /// were the only way to use Go before accounts existed. Codex and Claude
    /// are OAuth-only, so `false` there — which is what lets the UI hide the
    /// key field and keeps `router::targets` accounts-only for them.
    pub key_login: bool,
}

/// Copy for the acknowledgement the user has to click through before an
/// account of this provider can be added.
///
/// This is not the same thing as `warning`: the warning is a footnote under the
/// form, which anyone who already knows the answer skips. The gate is a
/// blocking screen in front of the form, for providers whose terms forbid the
/// thing we are about to do. It belongs here rather than in the UI because it
/// is a fact about the provider, and a new provider that needs one should not be
/// able to add an account without somebody noticing the gate is missing.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AccountTermsGate {
    /// The heading. Names the consequence, not the rulebook.
    pub title: &'static str,
    /// One line stating plainly what this does. Shown above the fold.
    pub lede: &'static str,
    /// The specific clauses being relied on, as bullets.
    pub points: &'static [&'static str],
    pub terms_url: &'static str,
    /// What the user gets for accepting. Stops the dialog reading as a
    /// pointless "are you sure?".
    pub accept: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub mono: &'static str,
    pub color: &'static str,
    pub base_url: &'static str,
    pub env: &'static str,
    pub chip: &'static str,
    pub kind: &'static str,
    pub local: bool,
    pub account: Option<AccountProviderInfo>,
}

pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo { id: "codex", name: "ChatGPT (Codex)", icon: "openai", mono: "C", color: "#10a37f", base_url: "", env: "", chip: "ChatGPT subscription accounts", kind: "codex", local: false, account: Some(AccountProviderInfo { display_name: "ChatGPT / Codex", short_name: "ChatGPT", login_file: "~/.codex/auth.json", login_command: "codex login", paste_hint: "Paste auth.json contents", setup_command: "", token_prefix: "", warning: "", terms_gate: None, key_login: false }) },
    ProviderInfo { id: "claude", name: "Claude (subscription)", icon: "claude", mono: "✳", color: "#d97757", base_url: "", env: "", chip: "Claude Pro / Max accounts", kind: "anthropic", local: false, account: Some(AccountProviderInfo { display_name: "Claude Pro / Max", short_name: "Claude", login_file: "~/.claude/.credentials.json", login_command: "claude /login", paste_hint: "Paste credentials or sk-ant-oat… token", setup_command: "claude setup-token", token_prefix: "sk-ant-oat", warning: "Using a Claude subscription outside Claude Code may violate Anthropic's consumer terms.", terms_gate: Some(AccountTermsGate {
        title: "This is not what Claude Code is for",
        lede: "Anthropic's consumer terms do not cover this. You are about to drive your Claude Pro / Max subscription from a third-party agent, and that is your call to make and your account to risk.",
        // Quoted from the Consumer Terms so nobody has to take this dialog's word
        // for it. Both are verbatim from https://www.anthropic.com/legal/consumer-terms
        // as of the date below; re-read them there if the wording matters.
        points: &[
            "\u{201c}Except when you are accessing our Services via an Anthropic API Key or where we otherwise explicitly permit it, to access the Services through automated or non-human means, whether through a bot, script, or otherwise.\u{201d} — §3, Use of our Services",
            "\u{201c}You may not share your Account login information, Anthropic API key, or Account credentials with anyone else or make your Account available to anyone else.\u{201d} — §2, Account creation and access",
        ],
        terms_url: "https://www.anthropic.com/legal/consumer-terms",
        accept: "I understand, and I'm doing this at my own risk",
    }), key_login: false }) },
    ProviderInfo { id: "anthropic", name: "Anthropic", icon: "anthropic", mono: "A", color: "#e8967a", base_url: "https://api.anthropic.com", env: "ANTHROPIC_API_KEY", chip: "Anthropic API key", kind: "anthropic", local: false, account: None },
    ProviderInfo { id: "openai", name: "OpenAI", icon: "openai", mono: "O", color: "#e4e4e7", base_url: "https://api.openai.com/v1", env: "OPENAI_API_KEY", chip: "OpenAI API key", kind: "openai", local: false, account: None },
    ProviderInfo { id: "google", name: "Google", icon: "gemini", mono: "G", color: "#8ab4ff", base_url: "https://generativelanguage.googleapis.com/v1beta/openai", env: "GEMINI_API_KEY", chip: "Google AI Studio key", kind: "openai", local: false, account: None },
    ProviderInfo { id: "openrouter", name: "OpenRouter", icon: "openrouter", mono: "R", color: "#b69cff", base_url: "https://openrouter.ai/api/v1", env: "OPENROUTER_API_KEY", chip: "OpenRouter API key", kind: "openai", local: false, account: None },
    ProviderInfo { id: "zai", name: "Z.ai", icon: "zai", mono: "Z", color: "#7dd3fc", base_url: "https://api.z.ai/api/paas/v4", env: "ZAI_API_KEY", chip: "Z.ai API key", kind: "openai", local: false, account: None },
    ProviderInfo { id: "opencode", name: "OpenCode Zen", icon: "opencode", mono: "Ze", color: "#f472b6", base_url: "https://opencode.ai/zen/v1", env: "OPENCODE_API_KEY", chip: "OpenCode Zen API key · pay-per-use", kind: "openai", local: false, account: None },
    ProviderInfo { id: "opencode-go", name: "OpenCode Go", icon: "opencode", mono: "Go", color: "#fbbf24", base_url: "https://opencode.ai/zen/go/v1", env: "OPENCODE_API_KEY", chip: "OpenCode Go subscription · $10/mo", kind: "openai", local: false, account: Some(AccountProviderInfo {
        display_name: "OpenCode Go",
        short_name: "OpenCode",
        login_file: "~/.local/share/opencode/auth.json",
        login_command: "opencode auth login",
        paste_hint: "Paste an OpenCode Go API key (or its auth.json)",
        setup_command: "",
        token_prefix: "",
        // Not a subscription being driven around a vendor's CLI: Go sells API
        // access and documents the endpoints for "other coding agents", so
        // there is nothing here to acknowledge the way Anthropic's terms are.
        warning: "",
        terms_gate: None,
        key_login: true,
    }) },
    ProviderInfo { id: "ollama", name: "Ollama", icon: "ollama", mono: "L", color: "#86efac", base_url: "http://localhost:11434/v1", env: "", chip: "Local · localhost:11434", kind: "openai", local: true, account: None },
];

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub url: &'static str,
    pub kind: &'static str,
    pub icon: &'static str,
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "DeepSeek",
        url: "https://api.deepseek.com/v1",
        kind: "openai",
        icon: "deepseek",
    },
    ProviderPreset {
        name: "Groq",
        url: "https://api.groq.com/openai/v1",
        kind: "openai",
        icon: "groq",
    },
    ProviderPreset {
        name: "xAI",
        url: "https://api.x.ai/v1",
        kind: "openai",
        icon: "xai",
    },
    ProviderPreset {
        name: "Mistral",
        url: "https://api.mistral.ai/v1",
        kind: "openai",
        icon: "mistral",
    },
    ProviderPreset {
        name: "Together",
        url: "https://api.together.xyz/v1",
        kind: "openai",
        icon: "together",
    },
    ProviderPreset {
        name: "Fireworks",
        url: "https://api.fireworks.ai/inference/v1",
        kind: "openai",
        icon: "fireworks",
    },
    ProviderPreset {
        name: "Moonshot",
        url: "https://api.moonshot.ai/v1",
        kind: "openai",
        icon: "moonshot",
    },
    ProviderPreset {
        name: "LM Studio",
        url: "http://localhost:1234/v1",
        kind: "openai",
        icon: "lmstudio",
    },
    ProviderPreset {
        name: "vLLM",
        url: "http://localhost:8000/v1",
        kind: "openai",
        icon: "vllm",
    },
];

/// Providers backed by subscription accounts instead of a key.
pub fn is_pool(prov: &str) -> bool {
    provider(prov).is_some_and(|p| p.account.is_some())
}

/// Providers whose account login *is* an API key (see `AccountProviderInfo::key_login`).
///
/// These are pool providers AND key providers at once, which is the one case
/// `is_pool` alone cannot express: `router::targets` has to know it may fall
/// back to a plain configured key when no account exists, or an install that
/// only ever set `OPENCODE_API_KEY` would start failing requests the moment Go
/// gained an account type.
pub fn key_login(prov: &str) -> bool {
    provider(prov).is_some_and(|p| p.account.is_some_and(|a| a.key_login))
}

/// True when the provider has a key pool configured (multiple API keys to rotate across).
pub fn key_pool(settings: &Settings, prov: &str) -> bool {
    settings
        .providers
        .get(prov)
        .map(|p| p.key_pool && !p.api_keys.is_empty())
        .unwrap_or(false)
}

/// Last-4-char preview of a key (same threshold as the single-key `key_hint` in lib.rs).
pub fn key_hint(key: &str) -> String {
    if key.len() > 8 {
        // Count chars, not bytes: a key with non-ASCII (CJK/emoji) can end on a byte
        // that isn't a char boundary, so `&key[key.len() - 4..]` would panic. Taking
        // the last 4 chars instead degrades gracefully; `get(..n)` would collapse to
        // an empty hint whenever the boundary splits a codepoint.
        let tail: String = key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if tail.is_empty() {
            String::new()
        } else {
            format!("\u{2026}{tail}")
        }
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelInfo {
    /// "provider/model-id"
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context: u64,
    pub output: u64,
    /// $ per 1M tokens
    pub input_price: f64,
    pub output_price: f64,
    /// Derived: true when the model has reasoning levels the effort slider can drive.
    pub effort: bool,
    /// text | image | video | pdf
    pub input_types: Vec<String>,
    /// structured_output | web_search | mid_system
    pub capabilities: Vec<String>,
    /// Ordered low → high. The 5-step effort slider is spread across these.
    pub reasoning_levels: Vec<String>,
    /// How a level is sent: none | reasoning_effort | reasoning.effort | thinking | anthropic_effort | thinking_budget
    pub reasoning_param: String,
    /// User-defined (editable) rather than built in.
    pub custom: bool,
    /// Toggled off in Settings → Models. Hidden from pickers, still listed (dimmed) in settings.
    #[serde(default = "on")]
    pub enabled: bool,
}

fn on() -> bool {
    true
}

fn m(
    p: &str,
    id: &str,
    name: &str,
    ctx: u64,
    out: u64,
    ip: f64,
    op: f64,
    effort: bool,
) -> ModelInfo {
    let (levels, param): (&[&str], &str) = match (p, effort) {
        (_, false) => (&[], "none"),
        ("anthropic" | "claude", _) if id.contains("haiku") => {
            (&["0", "4000", "8000", "16000", "32000"], "thinking_budget")
        }
        ("anthropic" | "claude", _) => (
            &["low", "medium", "high", "xhigh", "max"],
            "anthropic_effort",
        ),
        ("codex", _) => (&["low", "medium", "high"], "reasoning.effort"),
        ("zai", _) => (&["off", "on"], "thinking"),
        _ => (&["low", "medium", "high"], "reasoning_effort"),
    };
    ModelInfo {
        id: format!("{p}/{id}"),
        name: name.into(),
        provider: p.into(),
        context: ctx,
        output: out,
        input_price: ip,
        output_price: op,
        effort,
        input_types: if matches!(
            p,
            "anthropic" | "claude" | "openai" | "codex" | "google" | "opencode" | "opencode-go"
        ) {
            vec!["text".into(), "image".into(), "pdf".into()]
        } else {
            vec!["text".into()]
        },
        capabilities: vec![],
        reasoning_levels: levels.iter().map(|s| s.to_string()).collect(),
        reasoning_param: param.into(),
        custom: false,
        enabled: true,
    }
}

static CUSTOM_MODELS: std::sync::RwLock<Vec<ModelInfo>> = std::sync::RwLock::new(Vec::new());
static DISABLED_MODELS: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());
static REMOVED_MODELS: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// User-configured models override/extend the catalog. Called whenever settings change.
pub fn set_custom_models(models: &[ModelInfo]) {
    *CUSTOM_MODELS.write().unwrap() = models
        .iter()
        .cloned()
        .map(|mut m| {
            m.custom = true;
            m.effort = !m.reasoning_levels.is_empty() && m.reasoning_param != "none";
            // A toggled-off custom model stays off: its id is also in the disabled list.
            m.enabled = true;
            if m.name.trim().is_empty() {
                m.name =
                    m.id.split_once('/')
                        .map(|x| x.1)
                        .unwrap_or(&m.id)
                        .to_string();
            }
            m
        })
        .collect();
}

/// Toggled-off (dimmed in settings, hidden from pickers) and deleted
/// (hidden everywhere) model ids. Applies to built-in, pool and custom models.
pub fn set_model_visibility(disabled: &[String], removed: &[String]) {
    *DISABLED_MODELS.write().unwrap() = disabled.to_vec();
    *REMOVED_MODELS.write().unwrap() = removed.to_vec();
}

fn apply_visibility(mut v: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let disabled = DISABLED_MODELS.read().unwrap().clone();
    let removed = REMOVED_MODELS.read().unwrap().clone();
    // A custom override revives a deleted built-in: it is kept even if its id is removed.
    v.retain(|m| m.custom || !removed.contains(&m.id));
    for m in v.iter_mut() {
        m.enabled = !disabled.contains(&m.id);
    }
    v
}

/// Catalog + custom models, custom winning on id collisions.
pub fn all_models() -> Vec<ModelInfo> {
    let custom = CUSTOM_MODELS.read().unwrap().clone();
    let pool = POOL_MODELS.read().unwrap().clone();
    // A fetched subscription list replaces that provider's built-in guesses.
    let fetched: std::collections::HashSet<String> =
        pool.iter().map(|m| m.provider.clone()).collect();
    let mut v: Vec<ModelInfo> = catalog()
        .into_iter()
        .filter(|m| !fetched.contains(&m.provider))
        .collect();
    v.extend(pool);
    v.retain(|m| !custom.iter().any(|c| c.id == m.id));
    v.extend(custom);
    apply_visibility(v)
}

/// Models the subscription accounts actually offer, fetched live (see `accounts::fetch_pool_models`).
static POOL_MODELS: std::sync::RwLock<Vec<ModelInfo>> = std::sync::RwLock::new(Vec::new());

/// Replace one pool provider's fetched models. Empty `models` restores the built-in list.
pub fn set_pool_models(prov: &str, models: Vec<ModelInfo>) {
    let mut p = POOL_MODELS.write().unwrap();
    p.retain(|m| m.provider != prov);
    p.extend(models);
}

/// Parse `/codex/models` (the Codex CLI's list) into model infos.
pub fn parse_codex_models(v: &Value) -> Vec<ModelInfo> {
    let arr = v["models"]
        .as_array()
        .or(v["data"].as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter(|x| {
            x["visibility"]
                .as_str()
                .is_none_or(|v| v != "hide" && v != "hidden")
                && x["supported_in_api"].as_bool() != Some(false)
        })
        .filter_map(|x| {
            let id = x["slug"].as_str().or(x["id"].as_str())?.to_string();
            let name = x["display_name"]
                .as_str()
                .or(x["name"].as_str())
                .map(String::from)
                .unwrap_or_else(|| id.clone());
            let mut mi = m(
                "codex",
                &id,
                &name,
                x["context_window"].as_u64().unwrap_or(400_000),
                x["max_output_tokens"].as_u64().unwrap_or(128_000),
                0.0,
                0.0,
                true,
            );
            let levels: Vec<String> = x["supported_reasoning_levels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l["effort"].as_str().or(l.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !levels.is_empty() {
                mi.reasoning_levels = levels;
            }
            Some(mi)
        })
        .collect()
}

/// Parse OpenCode Go's `/zen/go/v1/models` into subscription model infos.
///
/// The payload is the plain OpenAI list shape — `{"data": [{"id": …}]}` — with
/// no context window, no reasoning levels and no price: everything the app
/// needs to send a working request has to be filled in here. Two things matter:
///
/// - the name is tidied from the id, because Go returns `kimi-k3`, not `Kimi K3`;
/// - the wire protocol is decided per model, exactly as the live path does it
///   (`zen_wire`), so a registered Go model is dispatched through Messages or
///   Responses rather than falling through to chat/completions. Getting this
///   wrong is silent: the request is accepted and answered by the wrong endpoint.
pub fn parse_go_models(v: &Value) -> Vec<ModelInfo> {
    let arr = v["data"].as_array().cloned().unwrap_or_default();
    arr.iter()
        .filter_map(|x| {
            let id = x["id"].as_str()?.to_string();
            if id.is_empty() {
                return None;
            }
            let name = go_model_name(&id);
            let mut mi = m(
                "opencode-go",
                &id,
                &name,
                go_context(&id),
                32_000,
                0.0,
                0.0,
                true,
            );
            // Go bills the subscription, not the token, so the price fields stay 0
            // (as Zen's do) and `model_info` reports it free — the spend screen is
            // fed by the account's own usage windows instead.
            if let Some(w) = zen_wire(&id) {
                mi.reasoning_param = match w.as_str() {
                    "anthropic" => "anthropic_effort".into(),
                    _ => "reasoning_effort".into(),
                };
            }
            Some(mi)
        })
        .collect()
}

/// `kimi-k3` → `Kimi K3`, `deepseek-v4.1-flash` → `DeepSeek V4.1 Flash`.
///
/// Only the casing is reconstructed; the words are the provider's own. A table
/// of display names would go stale the moment Go adds a model, and a wrong name
/// is cosmetic where a guessed context window is not.
fn go_model_name(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(go_word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One hyphen-separated segment of a model id.
fn go_word(w: &str) -> String {
    // Brand forms no rule can derive.
    match w {
        "gpt" => return "GPT".into(),
        "glm" => return "GLM".into(),
        "ai" => return "AI".into(),
        "mimo" => return "MiMo".into(),
        "minimax" => return "MiniMax".into(),
        "deepseek" => return "DeepSeek".into(),
        "longcat" => return "LongCat".into(),
        _ => {}
    }
    let letters: String = w.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let rest = &w[letters.len()..];
    // A single letter followed by a digit is a model version — k3, v4, m2.7, h3
    // — and its letter is upper-cased so `kimi-k3` reads "Kimi K3" and not
    // "Kimi k3". Longer runs are left to the capitalise rule below, which keeps
    // `hy4` as "Hy4" rather than "HY4".
    if letters.len() == 1 && rest.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("{}{}", letters.to_uppercase(), rest);
    }
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Context window for a Go model, from the family. Go does not publish one per
/// model, so these are the vendors' own numbers for the model each id names.
fn go_context(id: &str) -> u64 {
    let m = id.to_ascii_lowercase();
    if m.starts_with("claude") {
        1_000_000
    } else if m.contains("kimi") || m.starts_with("qwen") || m.starts_with("grok") {
        256_000
    } else if m.starts_with("gpt-6") || m.starts_with("gpt-5") {
        272_000
    } else if m.starts_with("glm") || m.starts_with("mimo") || m.starts_with("muse") {
        200_000
    } else {
        // DeepSeek, MiniMax, LongCat, Hy and anything new: the smallest of the
        // plausible windows. Guessing high is the dangerous direction — it lets
        // a conversation grow past what the model accepts before compaction.
        128_000
    }
}

/// Parse Anthropic `/v1/models` into subscription model infos.
pub fn parse_claude_models(v: &Value) -> Vec<ModelInfo> {
    let arr = v["data"].as_array().cloned().unwrap_or_default();
    arr.iter()
        .filter_map(|x| {
            let id = x["id"].as_str()?.to_string();
            let name = format!(
                "{} (sub)",
                x["display_name"]
                    .as_str()
                    .unwrap_or(&id)
                    .trim_start_matches("Claude ")
            );
            let known = catalog()
                .into_iter()
                .find(|c| c.provider == "anthropic" && id.starts_with(&c.id["anthropic/".len()..]));
            let (ctx, out) =
                known
                    .map(|k| (k.context, k.output))
                    .unwrap_or(if id.contains("haiku") {
                        (200_000, 64_000)
                    } else {
                        (1_000_000, 128_000)
                    });
            let ctx = x["max_input_tokens"].as_u64().unwrap_or(ctx);
            let out = x["max_tokens"].as_u64().unwrap_or(out);
            Some(m("claude", &id, &name, ctx, out, 0.0, 0.0, true))
        })
        .collect()
}

/// Spread the 0 (max) … 4 (low) slider over a model's own low→high levels.
pub fn pick_level(levels: &[String], effort: usize) -> Option<String> {
    if levels.is_empty() {
        return None;
    }
    let t = (4 - effort.min(4)) as f64 / 4.0;
    let i = (t * (levels.len() - 1) as f64).round() as usize;
    levels.get(i).cloned()
}

/// The built-in model table, built once.
///
/// The table is a literal — these models and their limits don't change while the
/// app runs — but it used to be rebuilt from scratch on every call, allocating
/// ~66 `ModelInfo`s and their six `String`/`Vec` fields each time. That is on
/// the per-turn path: `model_info` is called for the context window, the output
/// cap and the price of every request, and `context_window`/`max_output` each
/// walk the chain, so a turn paid for several full rebuilds to read constants.
///
/// `OnceLock` rather than a `static`: the table is assembled by `m()` calls and
/// a post-pass, so it can't be a `const`, and a `Mutex` would put a lock on the
/// same per-turn path. Callers still receive an owned `Vec`, so nothing
/// downstream can mutate the shared copy.
static CATALOG: std::sync::OnceLock<Vec<ModelInfo>> = std::sync::OnceLock::new();

pub fn catalog() -> Vec<ModelInfo> {
    CATALOG.get_or_init(build_catalog).clone()
}

fn build_catalog() -> Vec<ModelInfo> {
    let mut v: Vec<ModelInfo> = vec![
        m(
            "codex",
            "gpt-6-astra",
            "GPT-6 Astra",
            400_000,
            128_000,
            0.0,
            0.0,
            true,
        ),
        m(
            "codex",
            "gpt-6-sol",
            "GPT-6 Sol",
            400_000,
            128_000,
            0.0,
            0.0,
            true,
        ),
        m(
            "codex",
            "gpt-6-luna",
            "GPT-6 Luna",
            400_000,
            128_000,
            0.0,
            0.0,
            true,
        ),
        m(
            "openai",
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            1_100_000,
            128_000,
            0.20,
            1.20,
            true,
        ),
        m(
            "openai",
            "gpt-5.6-terra",
            "GPT-5.6 Terra",
            1_100_000,
            128_000,
            2.0,
            12.0,
            true,
        ),
        m(
            "openai",
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            1_100_000,
            128_000,
            5.0,
            30.0,
            true,
        ),
        m(
            "openai", "gpt-5.5", "GPT-5.5", 1_100_000, 128_000, 5.0, 30.0, true,
        ),
        m(
            "openai", "gpt-5.4", "GPT-5.4", 1_100_000, 128_000, 2.50, 15.0, true,
        ),
        m(
            "openai",
            "gpt-5.4-mini",
            "GPT-5.4 mini",
            400_000,
            128_000,
            0.75,
            4.50,
            true,
        ),
        m(
            "openai",
            "gpt-5.4-nano",
            "GPT-5.4 nano",
            400_000,
            128_000,
            0.20,
            1.25,
            true,
        ),
        m(
            "openai",
            "gpt-5.3-codex",
            "GPT-5.3 Codex",
            400_000,
            128_000,
            1.75,
            14.0,
            true,
        ),
        m(
            "openai",
            "gpt-5.2-codex",
            "GPT-5.2 Codex",
            400_000,
            128_000,
            1.75,
            14.0,
            true,
        ),
        m(
            "openai", "gpt-5.2", "GPT-5.2", 400_000, 128_000, 1.75, 14.0, true,
        ),
        m(
            "openai", "gpt-5.1", "GPT-5.1", 400_000, 128_000, 1.25, 10.0, true,
        ),
        m(
            "openai",
            "gpt-5-codex",
            "GPT-5 Codex",
            400_000,
            128_000,
            1.25,
            10.0,
            true,
        ),
        m(
            "openai", "gpt-5", "GPT-5", 400_000, 128_000, 1.25, 10.0, true,
        ),
        m(
            "openai",
            "gpt-5-mini",
            "GPT-5 mini",
            400_000,
            128_000,
            0.25,
            2.0,
            true,
        ),
        m(
            "openai",
            "gpt-5-nano",
            "GPT-5 nano",
            400_000,
            128_000,
            0.05,
            0.40,
            true,
        ),
        m(
            "openai", "gpt-4.1", "GPT-4.1", 1_000_000, 32_000, 2.0, 8.0, false,
        ),
        m(
            "openai",
            "gpt-4.1-mini",
            "GPT-4.1 mini",
            1_000_000,
            32_000,
            0.40,
            1.60,
            false,
        ),
        m("openai", "o3", "o3", 200_000, 100_000, 2.0, 8.0, true),
        m(
            "openai", "o4-mini", "o4-mini", 200_000, 100_000, 1.10, 4.40, true,
        ),
        m(
            "google",
            "gemini-2.5-pro",
            "Gemini 2.5 Pro",
            1_000_000,
            64_000,
            1.25,
            10.0,
            true,
        ),
        m(
            "google",
            "gemini-2.5-flash",
            "Gemini 2.5 Flash",
            1_000_000,
            64_000,
            0.3,
            2.5,
            true,
        ),
        // OpenRouter (top picks for coding/agents; a fetched /models list overrides these).
        m(
            "openrouter",
            "openai/gpt-5.6-terra",
            "GPT-5.6 Terra",
            1_050_000,
            128_000,
            2.0,
            12.0,
            true,
        ),
        m(
            "openrouter",
            "anthropic/claude-opus-4.8",
            "Claude Opus 4.8",
            1_000_000,
            128_000,
            5.0,
            25.0,
            false,
        ),
        m(
            "openrouter",
            "anthropic/claude-sonnet-5",
            "Claude Sonnet 5",
            1_000_000,
            128_000,
            2.0,
            10.0,
            false,
        ),
        m(
            "openrouter",
            "google/gemini-3.1-pro-preview",
            "Gemini 3.1 Pro",
            1_048_000,
            64_000,
            2.0,
            12.0,
            false,
        ),
        m(
            "openrouter",
            "google/gemini-3.8-flash",
            "Gemini 3.8 Flash",
            1_048_000,
            64_000,
            0.75,
            3.75,
            false,
        ),
        m(
            "openrouter",
            "deepseek/deepseek-v4.1-flash",
            "DeepSeek V4.1 Flash",
            1_048_000,
            64_000,
            0.099,
            0.60,
            false,
        ),
        m(
            "openrouter",
            "deepseek/deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_048_000,
            64_000,
            0.047,
            0.094,
            false,
        ),
        m(
            "openrouter",
            "qwen/qwen3.8-max-prime",
            "Qwen3.8 Max Prime",
            1_000_000,
            64_000,
            4.0,
            12.0,
            false,
        ),
        m(
            "openrouter",
            "qwen/qwen3-coder-next",
            "Qwen3 Coder Next",
            262_000,
            64_000,
            0.12,
            0.80,
            false,
        ),
        m(
            "openrouter",
            "moonshotai/kimi-k3",
            "Kimi K3",
            1_048_000,
            64_000,
            3.0,
            15.0,
            false,
        ),
        m(
            "openrouter",
            "z-ai/glm-5.3",
            "GLM-5.3",
            1_310_000,
            128_000,
            1.40,
            4.40,
            false,
        ),
        m(
            "openrouter",
            "minimax/minimax-m3",
            "MiniMax M3",
            1_048_000,
            64_000,
            0.30,
            1.20,
            false,
        ),
        m(
            "openrouter",
            "mistralai/mistral-large-2512",
            "Mistral Large 3",
            262_000,
            32_000,
            0.50,
            1.50,
            false,
        ),
        m(
            "openrouter",
            "x-ai/grok-4.7",
            "Grok 4.7",
            500_000,
            64_000,
            1.60,
            4.80,
            false,
        ),
        m(
            "zai", "glm-4.6", "GLM-4.6", 200_000, 128_000, 0.6, 2.2, false,
        ),
        m(
            "opencode",
            "gpt-5.5",
            "GPT-5.5 (Zen)",
            400_000,
            128_000,
            1.25,
            10.0,
            true,
        ),
        m(
            "opencode",
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5 (Zen)",
            1_000_000,
            128_000,
            3.0,
            15.0,
            true,
        ),
        m(
            "opencode",
            "claude-opus-4-5",
            "Claude Opus 4.5 (Zen)",
            1_000_000,
            128_000,
            5.0,
            25.0,
            true,
        ),
        m(
            "opencode",
            "gemini-3-flash",
            "Gemini 3 Flash (Zen)",
            1_000_000,
            64_000,
            0.5,
            3.0,
            true,
        ),
        m(
            "opencode",
            "kimi-k2.6",
            "Kimi K2.6 (Zen)",
            256_000,
            32_000,
            0.6,
            2.5,
            false,
        ),
        m(
            "opencode",
            "glm-5",
            "GLM-5 (Zen)",
            200_000,
            128_000,
            0.6,
            2.2,
            false,
        ),
        m(
            "opencode",
            "qwen3.8-max",
            "Qwen3.8 Max (Zen)",
            256_000,
            64_000,
            0.4,
            1.6,
            false,
        ),
        m(
            "opencode",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro (Zen)",
            128_000,
            32_000,
            0.3,
            1.2,
            false,
        ),
        m(
            "opencode",
            "muse-spark-1.3",
            "Muse Spark 1.3 (Zen)",
            200_000,
            64_000,
            0.5,
            2.0,
            true,
        ),
        m(
            "opencode-go",
            "kimi-k3",
            "Kimi K3 (Go)",
            256_000,
            32_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "opencode-go",
            "glm-5",
            "GLM-5 (Go)",
            200_000,
            128_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "opencode-go",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro (Go)",
            128_000,
            32_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "opencode-go",
            "qwen3.7-max",
            "Qwen3.7 Max (Go)",
            256_000,
            64_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "opencode-go",
            "minimax-m2.7",
            "MiniMax M2.7 (Go)",
            200_000,
            128_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "opencode-go",
            "mimo-v2.6-pro",
            "MiMo V2.6 Pro (Go)",
            256_000,
            32_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "opencode-go",
            "muse-spark-1.3-contributor",
            "Muse Spark 1.3 (Go)",
            200_000,
            64_000,
            0.0,
            0.0,
            true,
        ),
        m(
            "ollama",
            "qwen3-coder:30b",
            "Qwen3 Coder 30B",
            256_000,
            32_000,
            0.0,
            0.0,
            false,
        ),
        m(
            "ollama",
            "gpt-oss:20b",
            "gpt-oss 20b",
            128_000,
            32_000,
            0.0,
            0.0,
            true,
        ),
    ];
    // GPT-5.2 and later accept the fifth reasoning effort (xhigh); the effort slider maps onto it exactly.
    const XHIGH: [&str; 5] = ["minimal", "low", "medium", "high", "xhigh"];
    for mi in v.iter_mut() {
        let slug = mi.id.trim_start_matches("openai/");
        if mi.provider == "openai"
            && ["gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5", "gpt-5.6"]
                .iter()
                .any(|p| slug.starts_with(p))
        {
            mi.reasoning_levels = XHIGH.iter().map(|s| s.to_string()).collect();
        }
    }
    v
}

/// Known model, or a sensible default for custom "provider/any-id" strings.
pub fn model_info(id: &str) -> ModelInfo {
    let disabled = DISABLED_MODELS.read().unwrap().contains(&id.to_string());
    let apply = |mut mi: ModelInfo| {
        mi.enabled = !disabled;
        mi
    };
    if let Some(mi) = CUSTOM_MODELS.read().unwrap().iter().find(|m| m.id == id) {
        return apply(mi.clone());
    }
    if let Some(mi) = POOL_MODELS.read().unwrap().iter().find(|m| m.id == id) {
        return apply(mi.clone());
    }
    if let Some(mi) = catalog().into_iter().find(|m| m.id == id) {
        return apply(mi);
    }
    let (p, rest) = id.split_once('/').unwrap_or(("openai", id));
    let price = if p == "opencode-go"
        || provider(p).is_some_and(|info| info.local || info.account.is_some())
    {
        0.0
    } else {
        1.0
    };
    m(
        p,
        rest,
        rest,
        128_000,
        32_000,
        price,
        price * 4.0,
        is_pool(p),
    )
}

pub fn provider(id: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Wire protocol: anthropic | codex | openai.
/// Wire protocol for one model. Gateways like OpenCode Zen/Go serve each model
/// family through a different API: GPT/Grok/Muse via Responses, Claude and
/// Qwen3.5+ via Messages, the rest via chat/completions.
/// Returns "anthropic" | "codex" (Responses) | "responses" (Responses, API key) | "openai".
pub fn wire(settings: &Settings, prov: &str, model: &str) -> String {
    let k = kind(settings, prov);
    if matches!(prov, "opencode" | "opencode-go") {
        return zen_wire(model).unwrap_or(k);
    }
    k
}

fn zen_wire(model: &str) -> Option<String> {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude")
        || m.starts_with("qwen3.5-plus")
        || m.starts_with("qwen3.6-plus")
        || m.starts_with("qwen3.7-")
        || m.starts_with("qwen3.8-flash")
    {
        return Some("anthropic".into());
    }
    if m.starts_with("gpt")
        || m.starts_with("grok")
        || m.starts_with("muse")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        return Some("responses".into());
    }
    None
}

/// `{base}/{path}` for APIs whose base may or may not already end in /v1.
fn api_url(base: &str, path: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.ends_with("/v1") {
        format!("{b}/{path}")
    } else {
        format!("{b}/v1/{path}")
    }
}

pub fn kind(settings: &Settings, prov: &str) -> String {
    provider(prov)
        .map(|p| p.kind.to_string())
        .or_else(|| {
            settings
                .custom_providers
                .iter()
                .find(|c| c.id == prov)
                .map(|c| c.kind.clone())
        })
        .unwrap_or_else(|| "openai".into())
}

pub fn insist(settings: &Settings, prov: &str) -> bool {
    settings
        .custom_providers
        .iter()
        .any(|c| c.id == prov && c.insist)
}

pub fn api_key(settings: &Settings, prov: &str) -> String {
    if let Some(cfg) = settings.providers.get(prov) {
        if cfg.key_pool {
            if let Some(k) = cfg.api_keys.iter().find(|k| !k.is_empty()) {
                return k.trim().to_string();
            }
        }
        let k = cfg.api_key.trim().to_string();
        if !k.is_empty() {
            return k;
        }
    }
    provider(prov)
        .filter(|p| !p.env.is_empty())
        .and_then(|p| std::env::var(p.env).ok())
        .unwrap_or_default()
}

/// The bearer credential for one attempt, in precedence order:
/// an explicit pool key, then the account's own token when its login *is* a key
/// (OpenCode Go), then the provider's configured key.
///
/// The middle case is what a key-login account needs and an OAuth account must
/// never take: a ChatGPT/Claude account token is a session for the vendor's own
/// backend, and sending it as `Authorization: Bearer` to the provider's public
/// API would leak it to the wrong host.
pub fn credential(settings: &Settings, t: &Target) -> String {
    if let Some(k) = &t.api_key {
        return k.clone();
    }
    if let Some(a) = &t.account {
        if accounts::key_only(&a.kind) {
            return a.access_token.clone();
        }
    }
    api_key(settings, &t.prov)
}

/// The account's *OAuth* login, when it has one. `None` for a key-login account
/// (OpenCode Go), because none of the OAuth handling in the backends applies to
/// it: no vendor prelude, no vendor beta headers, no vendor endpoint.
pub fn oauth_account(t: &Target) -> Option<&Account> {
    t.account.as_ref().filter(|a| !accounts::key_only(&a.kind))
}

pub fn base_url(settings: &Settings, prov: &str) -> String {
    let b = settings
        .providers
        .get(prov)
        .map(|p| p.base_url.trim().to_string())
        .unwrap_or_default();
    let b = if b.is_empty() {
        provider(prov)
            .map(|p| p.base_url.to_string())
            .or_else(|| {
                settings
                    .custom_providers
                    .iter()
                    .find(|c| c.id == prov)
                    .map(|c| c.base_url.clone())
            })
            .unwrap_or_default()
    } else {
        b
    };
    b.trim_end_matches('/').to_string()
}

/// Clone so the router can re-ground a parked request's effort without moving the
/// caller's copy — it holds the whole history, and the common case (effort
/// unchanged) still borrows rather than copies.
#[derive(Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Value>,
    pub effort: usize,
    pub max_tokens: u64,
    /// Stable per conversation: codex prompt_cache_key / session id.
    pub cache_key: String,
}

/// One concrete thing to send a request to.
#[derive(Clone, Debug)]
pub struct Target {
    /// Full "provider/model" id (model metadata lookups).
    pub model_id: String,
    pub prov: String,
    /// Model name on the wire.
    pub model: String,
    pub account: Option<Account>,
    /// When set (key pool), use this specific API key instead of the provider-level one.
    pub api_key: Option<String>,
}

impl Target {
    /// What the chat shows as "serving". Deliberately the account *label*
    /// ("Codex 1 · max") and never `email`: this string is written into the
    /// transcript, the status panel and every screenshot of a shared chat, and
    /// an address is not something a user means to publish when they paste a
    /// bug report. The label is enough to tell two pooled accounts apart, which
    /// is the only thing this is for.
    pub fn label(&self) -> String {
        match &self.account {
            Some(a) if !a.label.is_empty() => format!("{} · {}", self.model_id, a.label),
            // A nameless account would otherwise leave a trailing " · " on every
            // "serving" line, and on a notice the user reads mid-conversation.
            Some(_) => self.model_id.clone(),
            None => {
                if let Some(k) = &self.api_key {
                    let hint = key_hint(k);
                    if hint.is_empty() {
                        self.model_id.clone()
                    } else {
                        format!("{} · key {}", self.model_id, hint)
                    }
                } else {
                    self.model_id.clone()
                }
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TurnUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Part of `output` spent reasoning, when the provider reports it (OpenAI-style APIs; Anthropic doesn't).
    pub reasoning: u64,
}

impl TurnUsage {
    pub fn cost(&self, mi: &ModelInfo) -> f64 {
        (self.input as f64 * mi.input_price
            + self.cache_write as f64 * mi.input_price * 1.25
            + self.cache_read as f64 * mi.input_price * 0.1
            + self.output as f64 * mi.output_price)
            / 1e6
    }
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

pub struct Turn {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub usage: TurnUsage,
    /// Wire model that produced it.
    pub model: String,
}

pub enum StreamEvent {
    Text(String),
    Thinking(String),
    ToolStart {
        id: String,
        name: String,
    },
    BlockEnd,
    /// A failed attempt already streamed output: drop it, a retry follows.
    Reset,
}

/// Why an attempt failed — decides retry vs rotate vs fall back.
#[derive(Debug, Clone)]
pub enum Fail {
    Cancel,
    /// Network blips, 5xx, overloaded, truncated streams, weird provider noise.
    Transient(String),
    /// Out of quota (hard) or rate limited (soft). `until` = unix secs it frees up (0 = unknown).
    Exhausted {
        msg: String,
        until: i64,
        hard: bool,
    },
    Auth(String),
    Bad(String),
    /// The prompt is over the model's context window.
    TooLong(String),
}

use Fail::*;

fn looks_too_long(s: &str) -> bool {
    let l = s.to_lowercase();
    [
        "prompt is too long",
        "context length",
        "context window",
        "maximum context",
        "too many tokens",
        "context_length_exceeded",
        "input is too long",
        "reduce the length",
    ]
    .iter()
    .any(|k| l.contains(k))
}

fn looks_quota(s: &str) -> bool {
    let l = s.to_lowercase();
    [
        "usage_limit",
        "usage limit",
        "insufficient_quota",
        "quota",
        "limit reached",
        "credit",
        "billing",
        "exceeded your",
        "out of",
    ]
    .iter()
    .any(|k| l.contains(k))
}

/// Seconds until reset from the usual places a 429 hides them.
fn reset_at(headers: &reqwest::header::HeaderMap, body: &Value) -> i64 {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    if let Some(s) = body
        .pointer("/error/resets_in_seconds")
        .and_then(|v| v.as_i64())
    {
        return now() + s;
    }
    if let Some(t) = body.pointer("/error/resets_at").and_then(|v| v.as_i64()) {
        return t;
    }
    if let Some(t) = h("anthropic-ratelimit-unified-reset").and_then(|v| v.parse::<i64>().ok()) {
        return t;
    }
    if let Some(s) = h("retry-after").and_then(|v| v.parse::<f64>().ok()) {
        return now() + s.ceil() as i64;
    }
    0
}

/// Pull secrets out of text that is about to be shown to the user or written
/// into a task file.
///
/// The bodies we echo come from a provider the user configured. That includes
/// arbitrary custom `base_url` gateways, and a misconfigured proxy happily
/// reflects the request back — including the `Authorization` header. Without
/// this, a key could end up in the chat transcript and in
/// `~/.openleash/tasks/*.json` via nothing more than a typo in a base URL.
fn scrub_secrets(s: &str) -> String {
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static SK: OnceLock<regex::Regex> = OnceLock::new();
    static JWTISH: OnceLock<regex::Regex> = OnceLock::new();
    let bearer = BEARER.get_or_init(|| {
        regex::Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{8,}").expect("literal")
    });
    let sk = SK.get_or_init(|| regex::Regex::new(r"sk-[A-Za-z0-9_-]{8,}").expect("literal"));
    // OAuth access/refresh tokens and long opaque secrets that are not `sk-`.
    let jwtish = JWTISH.get_or_init(|| regex::Regex::new("(?i)((?:x-api-key|api[_-]?key|authorization|token)[\"'\\s:=]{1,4})[A-Za-z0-9._~+/=-]{12,}").expect("literal"));
    let out = bearer.replace_all(s, "${1}<redacted>");
    let out = sk.replace_all(&out, "<redacted>");
    jwtish.replace_all(&out, "${1}<redacted>").into_owned()
}

pub fn classify(status: u16, body: &str, headers: &reqwest::header::HeaderMap) -> Fail {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let detail = v
        .pointer("/error/message")
        .or(v.pointer("/detail"))
        .or(v.pointer("/message"))
        .and_then(|m| m.as_str())
        .map(String::from)
        .unwrap_or_else(|| body.chars().take(600).collect());
    let detail = scrub_secrets(&detail);
    let kind = v
        .pointer("/error/type")
        .or(v.pointer("/error/code"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    let msg = format!(
        "HTTP {status}: {}",
        if detail.is_empty() { kind } else { &detail }
    );
    match status {
        429 => Exhausted {
            until: reset_at(headers, &v),
            hard: looks_quota(&detail) || looks_quota(kind),
            msg,
        },
        402 => Exhausted {
            until: 0,
            hard: true,
            msg,
        },
        401 | 403 => {
            if looks_quota(&detail) {
                Exhausted {
                    until: reset_at(headers, &v),
                    hard: true,
                    msg,
                }
            } else {
                Auth(msg)
            }
        }
        413 => TooLong(msg),
        400 | 404 | 422 if looks_too_long(&detail) => TooLong(msg),
        400 | 404 | 422 => Bad(msg),
        s if s >= 500 || s == 408 || s == 409 => Transient(msg),
        _ => Bad(msg),
    }
}

/// Seconds to wait for response *headers*. The client only has a connect
/// timeout, so a server that accepts the connection and then stalls before
/// answering would otherwise hang the turn forever. This deliberately covers
/// only the way to the headers: the body is a stream that may legitimately run
/// for minutes, and `for_each_sse`'s idle watchdog owns that.
const FIRST_BYTE: std::time::Duration = std::time::Duration::from_secs(60);

async fn send(
    rb: reqwest::RequestBuilder,
    cancel: &CancellationToken,
    hdrs: &mut Option<reqwest::header::HeaderMap>,
) -> Result<reqwest::Response, Fail> {
    let resp = tokio::select! {
        r = tokio::time::timeout(FIRST_BYTE, rb.send()) => match r {
            // A slow *connection* is a transient failure, not a dead turn.
            Err(_) => return Err(Transient("the provider accepted the connection but sent no response headers within 60s".into())),
            Ok(Err(e)) => return Err(Transient(format!("network error: {e}"))),
            Ok(Ok(r)) => r,
        },
        _ = cancel.cancelled() => return Err(Cancel),
    };
    *hdrs = Some(resp.headers().clone());
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let headers = resp.headers().clone();
    let body = resp.text().await.unwrap_or_default();
    Err(classify(status.as_u16(), &body, &headers))
}

/// Hard ceiling on the unparsed tail of the SSE buffer. A server that accepts
/// the connection and streams megabytes without ever sending the blank line
/// that ends an event would otherwise grow this for the whole request.
const SSE_BUF_MAX: usize = 8 * 1024 * 1024;

/// Minimal SSE reader: yields (event name, data) for each event.
///
/// Chunks are buffered as *bytes*, not text: a TCP chunk boundary lands wherever
/// the network says, which can fall in the middle of a multi-byte character.
/// Decoding per chunk turned every split codepoint into a U+FFFD, permanently
/// corrupting non-ASCII model output (CJK especially). Only complete
/// `\n\n`-delimited events are decoded, so a character is never decoded until
/// all of its bytes have arrived.
async fn for_each_sse(
    resp: reqwest::Response,
    cancel: &CancellationToken,
    mut f: impl FnMut(&str, &str) -> Result<(), Fail>,
) -> Result<(), Fail> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = tokio::select! {
            c = stream.next() => c,
            _ = cancel.cancelled() => return Err(Cancel),
            // A stalled stream is a dead stream.
            _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => return Err(Transient("stream stalled for 5 minutes".into())),
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|e| Transient(format!("stream error: {e}")))?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = find_event_end(&buf) {
            let raw: Vec<u8> = buf.drain(..pos).collect();
            // The event is complete and self-delimiting, so every codepoint in
            // it is whole. Anything still invalid here is the provider's doing,
            // not our framing.
            let event = String::from_utf8_lossy(&raw);
            let name = event
                .lines()
                .find_map(|l| l.strip_prefix("event:"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let data: Vec<&str> = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|l| l.trim_start())
                .collect();
            if !data.is_empty() {
                f(&name, &data.join("\n"))?;
            }
        }
        if buf.len() > SSE_BUF_MAX {
            return Err(Transient(format!(
                "stream sent {} bytes with no event boundary",
                buf.len()
            )));
        }
    }
    Ok(())
}

/// Byte index just past the next `\n\n` (or `\r\n\r\n`), CRLF folded first by
/// the caller. Returns `None` when no complete event is buffered yet.
fn find_event_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' {
            if buf[i + 1] == b'\n' {
                return Some(i + 2);
            }
            // `\n\r\n` and `\r\n\n` also read as a blank line.
            if buf[i + 1] == b'\r' && i + 2 < buf.len() && buf[i + 2] == b'\n' {
                return Some(i + 3);
            }
        }
        i += 1;
    }
    None
}

/// Dispatch one attempt to the right backend.
pub async fn call(
    http: &reqwest::Client,
    settings: &Settings,
    t: &Target,
    req: &ChatRequest,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &CancellationToken,
    hdrs: &mut Option<reqwest::header::HeaderMap>,
) -> Result<Turn, Fail> {
    match wire(settings, &t.prov, &t.model).as_str() {
        "anthropic" => anthropic(http, settings, t, req, on, cancel, hdrs).await,
        "codex" | "responses" => codex(http, settings, t, req, on, cancel, hdrs).await,
        _ => openai_compat(http, settings, t, req, on, cancel, hdrs).await,
    }
}

/// The growing body of one streamed content block, held outside the block's
/// JSON so appending a delta is O(delta) instead of O(whole answer).
///
/// Anthropic deltas arrive as `text_delta` / `thinking_delta` / `signature_delta`
/// and the block has to end up carrying all three back to the Messages API.
/// `tool_use` args are the exception — those stream as `input_json_delta` and are
/// already buffered in `partial` before being parsed at `content_block_stop`.
#[derive(Default)]
struct StreamAcc {
    text: String,
    thinking: String,
    signature: String,
}

// ───────────────────────────── Anthropic ─────────────────────────────

const ANTHROPIC_BLOCKS: &[&str] = &[
    "text",
    "thinking",
    "redacted_thinking",
    "tool_use",
    "tool_result",
    "image",
    "document",
];

/// History → Messages API: known block types only, and thinking blocks only
/// when they were produced by this same model (signatures don't transfer).
fn anthropic_messages(msgs: &[Message], model: &str) -> Vec<Value> {
    msgs.iter()
        .map(|m| {
            let content: Vec<Value> = m
                .content
                .iter()
                .filter(|b| ANTHROPIC_BLOCKS.contains(&b["type"].as_str().unwrap_or("")))
                .filter(|b| !(matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")) && m.model != model))
                .cloned()
                .collect();
            json!({"role": m.role, "content": if content.is_empty() { vec![json!({"type":"text","text":"(empty)"})] } else { content }})
        })
        .collect()
}

async fn anthropic(
    http: &reqwest::Client,
    settings: &Settings,
    t: &Target,
    req: &ChatRequest,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &CancellationToken,
    hdrs: &mut Option<reqwest::header::HeaderMap>,
) -> Result<Turn, Fail> {
    // Only a genuine OAuth login takes the vendor's own endpoint, prelude and
    // beta headers. A key-login account (OpenCode Go) is an ordinary key that
    // happens to be stored as an account, so it goes down the key path below.
    let sub = oauth_account(t);
    let key = credential(settings, t);
    if sub.is_none() && key.is_empty() {
        let env = provider(&t.prov).map(|p| p.env).unwrap_or("");
        return Err(Auth(format!(
            "No API key for {}. Add one in Settings → Models{}.",
            t.prov,
            if env.is_empty() {
                String::new()
            } else {
                format!(", or set {env}")
            }
        )));
    }
    let model = t.model.as_str();
    let mi = model_info(&t.model_id);
    let level = pick_level(&mi.reasoning_levels, req.effort);

    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            let mut t = t.clone();
            t["eager_input_streaming"] = json!(true);
            t
        })
        .collect();

    let mut system = vec![];
    if sub.is_some() {
        system.push(json!({"type": "text", "text": accounts::CLAUDE_PRELUDE}));
    }
    system
        .push(json!({"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}));
    let mut body = json!({
        "model": model,
        "max_tokens": req.max_tokens,
        "stream": true,
        // Auto-cache the growing conversation; the system block has its own breakpoint
        // so tools + system stay cached even when the tail changes.
        "cache_control": {"type": "ephemeral"},
        "system": system,
        "messages": anthropic_messages(&req.messages, model),
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    match (mi.reasoning_param.as_str(), level) {
        ("thinking_budget", Some(l)) => {
            let budget = budget_tokens(&l);
            if budget >= 1024 {
                body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                body["max_tokens"] = json!(req.max_tokens.max(budget + 8_000));
            }
        }
        ("anthropic_effort", Some(l)) => {
            body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
            body["output_config"] = json!({"effort": l});
        }
        _ => {}
    }

    let mut rb = match sub {
        Some(a) => http
            .post(accounts::claude_url())
            .bearer_auth(&a.access_token)
            .header("anthropic-beta", accounts::CLAUDE_BETA)
            .header("user-agent", accounts::CLAUDE_UA)
            .header("x-app", "cli")
            .header("anthropic-dangerous-direct-browser-access", "true"),
        None => http
            .post(api_url(&base_url(settings, &t.prov), "messages"))
            .header("x-api-key", &key)
            .bearer_auth(&key),
    };
    if sub.is_none() && is_opencode(&t.prov) {
        rb = opencode_headers(rb, req);
    }
    rb = rb
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    if sub.is_none()
        && t.prov == "anthropic"
        && (model == "claude-opus-5" || model == "claude-fable-5-1")
    {
        rb = rb.header("anthropic-beta", "server-side-fallback-2026-07-01");
        body["fallbacks"] = json!("default");
    }
    let resp = send(rb.json(&body), cancel, hdrs).await?;

    let mut blocks: Vec<Value> = vec![];
    let mut partial: Vec<String> = vec![];
    // Streamed text, keyed the same as `partial`, accumulated outside the block.
    //
    // This used to write each delta straight back into `blocks[idx]` as
    // `b["text"] = json!(cur + s)`, which re-copied and re-serialised the whole
    // accumulated answer once per token — O(n²) over the reply. A 4,000-token
    // answer copied ~16M chars to render one ~16k-char block, on the thread that
    // is also draining the socket. The Codex and OpenAI-compat readers already
    // built a `String` and `push_str`; this path now does the same, and folds the
    // finished string into the block once at `content_block_stop`.
    let mut acc: Vec<StreamAcc> = vec![];
    let mut usage = TurnUsage::default();
    let mut stop = String::new();
    let mut done = false;

    for_each_sse(resp, cancel, |_, data| {
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &ev["message"]["usage"];
                usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                let block = ev["content_block"].clone();
                while blocks.len() <= idx {
                    blocks.push(Value::Null);
                    partial.push(String::new());
                    acc.push(StreamAcc::default());
                }
                if block["type"] == "tool_use" {
                    on(StreamEvent::ToolStart {
                        id: block["id"].as_str().unwrap_or("").into(),
                        name: block["name"].as_str().unwrap_or("").into(),
                    });
                }
                blocks[idx] = block;
            }
            "content_block_delta" => {
                let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                let d = &ev["delta"];
                let Some(b) = blocks.get_mut(idx) else {
                    return Ok(());
                };
                // `b` is only borrowed to prove the index is live; the text itself
                // goes to `acc` and lands in the block at `content_block_stop`.
                let _ = b;
                let Some(slot) = acc.get_mut(idx) else {
                    return Ok(());
                };
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let s = d["text"].as_str().unwrap_or("");
                        slot.text.push_str(s);
                        on(StreamEvent::Text(s.into()));
                    }
                    "thinking_delta" => {
                        let s = d["thinking"].as_str().unwrap_or("");
                        slot.thinking.push_str(s);
                        on(StreamEvent::Thinking(s.into()));
                    }
                    "signature_delta" => {
                        slot.signature
                            .push_str(d["signature"].as_str().unwrap_or(""));
                    }
                    "input_json_delta" => {
                        partial[idx].push_str(d["partial_json"].as_str().unwrap_or(""))
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                if let Some(b) = blocks.get_mut(idx) {
                    // Fold the streamed text in once. A block that streamed
                    // nothing keeps whatever `content_block_start` gave it.
                    if let Some(slot) = acc.get(idx) {
                        if !slot.text.is_empty() {
                            b["text"] = Value::String(slot.text.clone());
                        }
                        if !slot.thinking.is_empty() {
                            b["thinking"] = Value::String(slot.thinking.clone());
                        }
                        if !slot.signature.is_empty() {
                            b["signature"] = Value::String(slot.signature.clone());
                        }
                    }
                    if b["type"] == "tool_use" {
                        let raw = partial[idx].trim();
                        // Eager input streaming skips server-side validation: guard the parse.
                        b["input"] = if raw.is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str::<Value>(raw)
                                .unwrap_or_else(|_| json!({"__invalid_json": raw}))
                        };
                    }
                }
                on(StreamEvent::BlockEnd);
            }
            "message_delta" => {
                if let Some(s) = ev["delta"]["stop_reason"].as_str() {
                    stop = s.into();
                }
                if let Some(o) = ev["usage"]["output_tokens"].as_u64() {
                    usage.output = o;
                }
            }
            "message_stop" => done = true,
            "error" => {
                let msg = ev["error"]["message"]
                    .as_str()
                    .unwrap_or("stream error")
                    .to_string();
                return Err(match ev["error"]["type"].as_str().unwrap_or("") {
                    "rate_limit_error" => Exhausted {
                        hard: looks_quota(&msg),
                        msg,
                        until: 0,
                    },
                    "invalid_request_error" if looks_too_long(&msg) => TooLong(msg),
                    "invalid_request_error" => Bad(msg),
                    _ => Transient(msg),
                });
            }
            _ => {}
        }
        Ok(())
    })
    .await?;
    if !done && stop.is_empty() {
        return Err(Transient(
            "the stream ended before the response finished".into(),
        ));
    }

    blocks.retain(|b| !b.is_null());
    // Empty text blocks are rejected when replayed.
    blocks.retain(|b| !(b["type"] == "text" && b["text"].as_str().is_none_or(|s| s.is_empty())));
    Ok(Turn {
        content: blocks,
        stop_reason: stop,
        usage,
        model: model.into(),
    })
}

// ───────────────────────────── Codex (ChatGPT subscription) ─────────────────────────────

fn tool_result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        v => v.to_string(),
    }
}

/// A single image block's (media_type, base64), or None if it isn't one.
fn block_image(b: &Value) -> Option<(String, String)> {
    if b["type"] != "image" {
        return None;
    }
    let mt = b["source"]["media_type"].as_str()?.to_string();
    let data = b["source"]["data"].as_str()?.to_string();
    (!mt.is_empty() && !data.is_empty()).then_some((mt, data))
}

/// Image blocks anywhere in a message (Anthropic shape):
/// `{"type":"image","source":{"media_type":..,"data":..}}`. Used both for
/// tool_result content arrays and for images the user pasted into a turn.
fn images_in(arr: &[Value]) -> Vec<(String, String)> {
    arr.iter()
        .filter(|x| x["type"] == "image")
        .filter_map(|x| {
            let mt = x["source"]["media_type"].as_str()?.to_string();
            let data = x["source"]["data"].as_str()?.to_string();
            if mt.is_empty() || data.is_empty() {
                return None;
            }
            Some((mt, data))
        })
        .collect()
}

/// Images inside a tool_result `content` array (Anthropic shape):
/// `{"type":"image","source":{"media_type":..,"data":..}}`.
/// Returns (media_type, base64) pairs.
fn tool_result_images(b: &Value) -> Vec<(String, String)> {
    match &b["content"] {
        Value::Array(a) => images_in(a),
        _ => vec![],
    }
}

fn codex_input(msgs: &[Message]) -> Vec<Value> {
    let mut out = vec![];
    for m in msgs {
        for b in &m.content {
            match (m.role.as_str(), b["type"].as_str().unwrap_or("")) {
                ("assistant", "text") => out.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": b["text"]}]})),
                ("assistant", "tool_use") => out.push(json!({"type": "function_call", "call_id": b["id"], "name": b["name"], "arguments": b["input"].to_string()})),
                (_, "tool_result") => {
                    let txt = tool_result_text(b);
                    // Responses API function_call_output is text-only: the text
                    // goes there, each image follows as its own user message so
                    // the model still sees it, right after its tool result.
                    out.push(json!({"type": "function_call_output", "call_id": b["tool_use_id"], "output": if txt.is_empty() { "(image shown)".to_string() } else { txt }}));
                    for (mt, data) in tool_result_images(b) {
                        out.push(json!({"type": "message", "role": "user", "content": [{"type": "input_image", "image_url": format!("data:{mt};base64,{data}"), "detail": "auto"}]}));
                    }
                }
                ("user", "text") => out.push(json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": b["text"]}]})),
                ("user", "image") => {
                    // Pasted/viewed image in a user turn: its own user message,
                    // same as the images that follow a tool result.
                    let Some((mt, data)) = block_image(b) else { continue };
                    out.push(json!({"type": "message", "role": "user", "content": [{"type": "input_image", "image_url": format!("data:{mt};base64,{data}"), "detail": "auto"}]}));
                }
                _ => {}
            }
        }
    }
    out
}

async fn codex(
    http: &reqwest::Client,
    settings: &Settings,
    t: &Target,
    req: &ChatRequest,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &CancellationToken,
    hdrs: &mut Option<reqwest::header::HeaderMap>,
) -> Result<Turn, Fail> {
    let mi = model_info(&t.model_id);
    let tools: Vec<Value> = req.tools.iter().map(|x| json!({"type": "function", "name": x["name"], "description": x["description"], "parameters": x["input_schema"], "strict": false})).collect();
    let mut body = json!({
        "model": t.model,
        "instructions": req.system,
        "input": codex_input(&req.messages),
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "store": false,
        "stream": true,
        "prompt_cache_key": req.cache_key,
    });
    if let Some(l) = pick_level(&mi.reasoning_levels, req.effort) {
        body["reasoning"] = json!({"effort": l, "summary": "auto"});
    }
    let rb = match oauth_account(t) {
        // ChatGPT subscription (Codex backend).
        Some(a) => http
            .post(accounts::codex_url())
            .bearer_auth(&a.access_token)
            .header("chatgpt-account-id", &a.account_id)
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("user-agent", accounts::CODEX_UA)
            .header("session_id", &req.cache_key),
        // Plain Responses API with a key (OpenCode Zen/Go GPT/Grok/Muse, …).
        // A key-login account lands here too: `credential` hands back the
        // account's own key, which is exactly what this endpoint wants.
        None => {
            if t.prov == "codex" {
                return Err(Auth(
                    "No ChatGPT account connected. Add one in Settings → Accounts.".into(),
                ));
            }
            let key = credential(settings, t);
            if key.is_empty() {
                return Err(Auth(format!(
                    "No API key for {}. Add one in Settings → Models.",
                    t.prov
                )));
            }
            http.post(format!(
                "{}/responses",
                base_url(settings, &t.prov).trim_end_matches('/')
            ))
            .bearer_auth(key)
        }
    };
    // The gateway routes and caches on these headers, so the Responses path
    // needs them as much as the chat/completions one does.
    let rb = if is_opencode(&t.prov) {
        opencode_headers(rb, req)
    } else {
        rb
    };
    let rb = rb.header("accept", "text/event-stream").json(&body);
    let resp = send(rb, cancel, hdrs).await?;

    let mut text = String::new();
    let mut calls: Vec<(String, String, String, String)> = vec![]; // item_id, call_id, name, args
    let mut usage = TurnUsage::default();
    let mut finished = false;
    let mut incomplete = false;
    for_each_sse(resp, cancel, |name, data| {
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        let ty = ev["type"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| name.to_string());
        match ty.as_str() {
            "response.output_text.delta" => {
                let s = ev["delta"].as_str().unwrap_or("");
                text.push_str(s);
                on(StreamEvent::Text(s.into()));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => on(
                StreamEvent::Thinking(ev["delta"].as_str().unwrap_or("").into()),
            ),
            "response.reasoning_summary_part.done" => on(StreamEvent::Thinking("\n\n".into())),
            "response.output_item.added" if ev["item"]["type"] == "function_call" => {
                let it = &ev["item"];
                let (iid, cid, n) = (
                    it["id"].as_str().unwrap_or("").to_string(),
                    it["call_id"].as_str().unwrap_or("").to_string(),
                    it["name"].as_str().unwrap_or("").to_string(),
                );
                on(StreamEvent::ToolStart {
                    id: cid.clone(),
                    name: n.clone(),
                });
                calls.push((iid, cid, n, String::new()));
            }
            "response.function_call_arguments.delta" => {
                let iid = ev["item_id"].as_str().unwrap_or("");
                if let Some(c) = calls.iter_mut().find(|c| c.0 == iid) {
                    c.3.push_str(ev["delta"].as_str().unwrap_or(""));
                }
            }
            "response.output_item.done" => {
                let it = &ev["item"];
                if it["type"] == "function_call" {
                    let iid = it["id"].as_str().unwrap_or("");
                    if let Some(c) = calls.iter_mut().find(|c| c.0 == iid) {
                        if let Some(args) = it["arguments"].as_str() {
                            c.3 = args.into();
                        }
                    }
                }
                on(StreamEvent::BlockEnd);
            }
            "response.completed" | "response.incomplete" => {
                finished = true;
                incomplete = ty == "response.incomplete";
                let u = &ev["response"]["usage"];
                let cached = u
                    .pointer("/input_tokens_details/cached_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                usage.input = u["input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(cached);
                usage.cache_read = cached;
                usage.output = u["output_tokens"].as_u64().unwrap_or(0);
                usage.reasoning = u
                    .pointer("/output_tokens_details/reasoning_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
            }
            "response.failed" | "error" => {
                let e = if ev["response"]["error"].is_object() {
                    &ev["response"]["error"]
                } else if ev["error"].is_object() {
                    &ev["error"]
                } else {
                    &ev
                };
                let msg = e["message"].as_str().unwrap_or("codex error").to_string();
                let code = e["code"].as_str().or(e["type"].as_str()).unwrap_or("");
                return Err(
                    if code.contains("usage_limit")
                        || code.contains("rate_limit")
                        || looks_quota(&msg)
                    {
                        Exhausted {
                            until: e["resets_in_seconds"]
                                .as_i64()
                                .map(|s| now() + s)
                                .unwrap_or(0),
                            hard: code.contains("usage_limit") || looks_quota(&msg),
                            msg,
                        }
                    } else if code.contains("context_length") || looks_too_long(&msg) {
                        TooLong(msg)
                    } else {
                        Transient(format!("{code} {msg}").trim().to_string())
                    },
                );
            }
            _ => {}
        }
        Ok(())
    })
    .await?;
    if !finished {
        return Err(Transient(
            "the stream ended before the response finished".into(),
        ));
    }
    let mut content = vec![];
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for (_, cid, name, args) in calls {
        let input = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&args).unwrap_or_else(|_| json!({"__invalid_json": args}))
        };
        content.push(json!({"type": "tool_use", "id": cid, "name": name, "input": input}));
    }
    let has_tools = content.iter().any(|b| b["type"] == "tool_use");
    let stop = if has_tools {
        "tool_use"
    } else if incomplete {
        "max_tokens"
    } else {
        "end_turn"
    };
    Ok(Turn {
        content,
        stop_reason: stop.into(),
        usage,
        model: t.model.clone(),
    })
}

// ───────────────────────── OpenAI-compatible ─────────────────────────

fn to_openai_messages(system: &str, msgs: &[Message]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for m in msgs {
        if m.role == "assistant" {
            let text: String = m
                .content
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let calls: Vec<Value> = m
                .content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| json!({"id": b["id"], "type": "function", "function": {"name": b["name"], "arguments": b["input"].to_string()}}))
                .collect();
            let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                msg["tool_calls"] = json!(calls);
            }
            out.push(msg);
        } else {
            let mut texts = vec![];
            let mut parts = vec![];
            for b in &m.content {
                match b["type"].as_str() {
                    Some("tool_result") => {
                        let images = tool_result_images(b);
                        if images.is_empty() {
                            out.push(json!({"role": "tool", "tool_call_id": b["tool_use_id"], "content": tool_result_text(b)}));
                        } else {
                            // Chat-completions tool messages accept array content:
                            // text + image_url parts in the same message.
                            let mut parts = vec![];
                            let txt = tool_result_text(b);
                            if !txt.is_empty() {
                                parts.push(json!({"type": "text", "text": txt}));
                            }
                            for (mt, data) in images {
                                parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mt};base64,{data}")}}));
                            }
                            out.push(json!({"role": "tool", "tool_call_id": b["tool_use_id"], "content": parts}));
                        }
                    }
                    Some("text") => texts.push(b["text"].as_str().unwrap_or("").to_string()),
                    // Images the user pasted (or a sub-agent was handed) live
                    // directly on a user turn. Without this they were dropped
                    // silently here, so the model only ever saw the caption.
                    Some("image") => {
                        if let Some((mt, data)) = block_image(b) {
                            parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mt};base64,{data}")}}));
                        }
                    }
                    _ => {}
                }
            }
            if !texts.is_empty() {
                parts.insert(0, json!({"type": "text", "text": texts.join("\n\n")}));
            }
            if !parts.is_empty() {
                // A user turn carrying only images (no caption) still has to
                // become a message, or it vanishes along with the rest.
                let content = if parts.len() == 1 && parts[0]["type"] == "text" {
                    Value::String(parts[0]["text"].as_str().unwrap_or("").to_string())
                } else {
                    Value::Array(parts)
                };
                out.push(json!({"role": "user", "content": content}));
            }
        }
    }
    out
}

// ───────────────────────── OpenCode spoof ─────────────────────────
// The Zen/Go free tier is UA-gated: only the official `opencode` client gets
// the free pool. Anything else gets `429 FreeUsageLimitError`, and since
// Sep 2026 free models also get `403 "OpenCode's free tier can only be used
// from within OpenCode"` unless the request looks like the CLI:
//   - `stream: true` (we always stream)
//   - `tools` declares `shell` (or `bash`) + `read`
//   - headers `User-Agent: opencode/<ver>`, `x-opencode-client: cli`,
//     `x-opencode-session: ses_…`, `x-opencode-request: msg_…`
// See `packages/opencode/src/session/llm/request.ts` (USER_AGENT,
// x-opencode-session/request/client/project). We spoof all of it so free
// models work from OpenLeash.

/// The User-Agent the official CLI sends. Public because `accounts.rs` reuses it
/// for the Go usage read: the gateway gates on it, so a monitor request that
/// arrives as a generic HTTP client is answered differently from the real one.
pub const OPENCODE_UA: &str = "opencode/1.18.31";

/// Deterministic `ses_<12 hex><14 alnum>` session id from the task's
/// cache_key (`ol-{task}`), stable per task so prompt caching keeps hitting.
fn opencode_session_id(cache_key: &str) -> String {
    fn fnv(seed: u64, s: &str) -> u64 {
        let mut h = seed;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        h
    }
    let h1 = fnv(0xcbf2_9ce4_8422_2325, cache_key);
    let h2 = fnv(h1 ^ 0x9e37_79b9_7f4a_7c15, cache_key);
    let hex12 = format!("{:012x}", h1 & 0xffff_ffff_ffff);
    const ALNUM: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut x = h2 ^ h1.wrapping_mul(0x2545_f491_4f6c_dd1d);
    // xorshift64* to stretch 64 bits into 14 alnum chars.
    let mut s14 = String::with_capacity(14);
    for _ in 0..14 {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x = x.wrapping_mul(0x2545_f491_4f6c_dd1d);
        s14.push(ALNUM[(x % 62) as usize] as char);
    }
    format!("ses_{hex12}{s14}")
}

/// Random `msg_<24 hex>` request id, fresh per request like the CLI's user id.
fn opencode_request_id() -> String {
    format!("msg_{}{}", super::new_id(), super::new_id())
}

/// True for the two OpenCode gateway providers (Zen pay-per-use and Go).
fn is_opencode(prov: &str) -> bool {
    prov == "opencode" || prov == "opencode-go"
}

/// The UA/session headers the gateway expects, applied to any request that goes
/// to `opencode`/`opencode-go` — chat/completions, Messages and Responses alike.
///
/// These used to be set only on the chat/completions path, so once Go gained
/// models that speak Messages (Qwen3.5+) and Responses (Grok, GPT, Muse) the
/// other two paths sent the model's request with no session id at all. The
/// gateway routes and caches on `x-opencode-session`, so the cost was quiet: the
/// prompt cache missed on every turn for exactly the models routed off the
/// default path.
fn opencode_headers(rb: reqwest::RequestBuilder, req: &ChatRequest) -> reqwest::RequestBuilder {
    rb.header("User-Agent", OPENCODE_UA)
        .header("x-opencode-client", "cli")
        .header("x-opencode-session", opencode_session_id(&req.cache_key))
        .header("x-opencode-request", opencode_request_id())
        .header("x-opencode-project", "global")
}

/// The free-tier gatekeeper needs exact `read` + `shell` names present in the
/// chat/completions `tools` array (see the note above). Our real tools are
/// `bash` + `read_file`, so this adds wire-only aliases; `runner.rs` maps them
/// back (`read` → `read_file`, `shell` → `bash`) when the model calls them.
///
/// Only the chat/completions path gets this. It exists for the *free* pool,
/// which is served through that endpoint; Go's paid models are not gated on it,
/// and Messages/Responses reject unknown function names rather than ignoring
/// them.
fn opencode_tool_aliases(tools: &mut Vec<Value>, req: &ChatRequest) {
    let has_read = req.tools.iter().any(|t| t["name"] == "read");
    let has_shell = req.tools.iter().any(|t| t["name"] == "shell");
    if !has_read {
        tools.push(json!({"type": "function", "function": {"name": "read", "description": "Compatibility alias for read_file — prefer read_file. Read a file from the local filesystem.", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}}));
    }
    if !has_shell {
        tools.push(json!({"type": "function", "function": {"name": "shell", "description": "Compatibility alias for bash — prefer bash. Run a shell command in the working directory.", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}}));
    }
}

async fn openai_compat(
    http: &reqwest::Client,
    settings: &Settings,
    t: &Target,
    req: &ChatRequest,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &CancellationToken,
    hdrs: &mut Option<reqwest::header::HeaderMap>,
) -> Result<Turn, Fail> {
    let prov = t.prov.as_str();
    let key = credential(settings, t);
    let custom = settings.custom_providers.iter().any(|c| c.id == prov);
    let local = provider(prov).is_some_and(|p| p.local);
    if key.is_empty() && !local && !custom {
        let env = provider(prov).map(|p| p.env).unwrap_or("");
        return Err(Auth(format!(
            "No API key for {prov}. Add one in Settings → Models{}.",
            if env.is_empty() {
                String::new()
            } else {
                format!(", or set {env}")
            }
        )));
    }
    let mut tools: Vec<Value> = req.tools.iter().map(|t| json!({"type": "function", "function": {"name": t["name"], "description": t["description"], "parameters": t["input_schema"]}})).collect();
    if is_opencode(prov) {
        opencode_tool_aliases(&mut tools, req);
    }
    let mut body = json!({
        "model": t.model,
        "stream": true,
        "messages": to_openai_messages(&req.system, &req.messages),
    });
    if !local {
        body["stream_options"] = json!({"include_usage": true});
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let mi = model_info(&t.model_id);
    let out_cap = req.max_tokens.min(mi.output.max(1024));
    if prov == "openai" {
        body["max_completion_tokens"] = json!(out_cap);
    } else {
        body["max_tokens"] = json!(out_cap);
    }
    if let Some(l) = pick_level(&mi.reasoning_levels, req.effort) {
        match mi.reasoning_param.as_str() {
            "reasoning_effort" => body["reasoning_effort"] = json!(l),
            "reasoning.effort" => body["reasoning"] = json!({"effort": l}),
            "thinking" => {
                let off = matches!(
                    l.to_lowercase().as_str(),
                    "off" | "none" | "disabled" | "false" | "0"
                );
                body["thinking"] = json!({"type": if off { "disabled" } else { "enabled" }});
            }
            "thinking_budget" => {
                let b = budget_tokens(&l);
                if b >= 1024 {
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": b});
                }
            }
            _ => {}
        }
    }
    let mut rb = http
        .post(format!("{}/chat/completions", base_url(settings, prov)))
        .header("content-type", "application/json");
    if !key.is_empty() {
        rb = rb.bearer_auth(key);
    }
    if prov == "openrouter" {
        rb = rb.header("X-Title", "OpenLeash");
    }
    if is_opencode(prov) {
        // Look like the official CLI: UA-gated free pool + ses_/msg_ ids the
        // gateway uses for routing/caching.
        rb = opencode_headers(rb, req);
    }
    let resp = send(rb.json(&body), cancel, hdrs).await?;

    let mut text = String::new();
    // index -> (id, name, args)
    let mut calls: Vec<(String, String, String)> = vec![];
    let mut usage = TurnUsage::default();
    let mut finish = String::new();
    let mut any = false;

    for_each_sse(resp, cancel, |_, data| {
        if data.trim() == "[DONE]" {
            return Ok(());
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        any = true;
        if let Some(e) = ev.get("error").filter(|e| !e.is_null()) {
            let msg = e["message"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| e.to_string());
            return Err(if looks_too_long(&msg) {
                TooLong(msg)
            } else if looks_quota(&msg) {
                Exhausted {
                    msg,
                    until: 0,
                    hard: true,
                }
            } else {
                Transient(msg)
            });
        }
        if let Some(u) = ev.get("usage").filter(|u| !u.is_null()) {
            let cached = u
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            usage.input = u["prompt_tokens"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(cached);
            usage.cache_read = cached;
            usage.output = u["completion_tokens"].as_u64().unwrap_or(0);
            usage.reasoning = u
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
        }
        let Some(ch) = ev["choices"].get(0) else {
            return Ok(());
        };
        let d = &ch["delta"];
        for k in ["reasoning_content", "reasoning"] {
            if let Some(s) = d[k].as_str() {
                on(StreamEvent::Thinking(s.into()));
            }
        }
        if let Some(s) = d["content"].as_str() {
            if !s.is_empty() {
                text.push_str(s);
                on(StreamEvent::Text(s.into()));
            }
        }
        if let Some(tcs) = d["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
                while calls.len() <= idx {
                    calls.push(Default::default());
                }
                let c = &mut calls[idx];
                if let Some(id) = tc["id"].as_str() {
                    c.0 = id.into();
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    if c.1.is_empty() {
                        c.1 = n.into();
                        on(StreamEvent::ToolStart {
                            id: c.0.clone(),
                            name: n.into(),
                        });
                    }
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    c.2.push_str(a);
                }
            }
        }
        if let Some(f) = ch["finish_reason"].as_str() {
            finish = f.into();
        }
        Ok(())
    })
    .await?;
    if !any {
        return Err(Transient("the provider returned an empty stream".into()));
    }
    if finish == "error" {
        return Err(Transient(
            "the provider reported an error mid-stream".into(),
        ));
    }

    let mut content = vec![];
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for (i, (id, name, args)) in calls.into_iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let input = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&args).unwrap_or_else(|_| json!({"__invalid_json": args}))
        };
        let id = if id.is_empty() {
            format!("call_{i}_{}", super::new_id())
        } else {
            id
        };
        content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
    }
    if content.is_empty() && finish.is_empty() {
        return Err(Transient("the provider returned nothing".into()));
    }
    let has_tools = content.iter().any(|b| b["type"] == "tool_use");
    let stop = if has_tools {
        "tool_use".to_string()
    } else if finish == "length" {
        "max_tokens".into()
    } else {
        "end_turn".into()
    };
    on(StreamEvent::BlockEnd);
    Ok(Turn {
        content,
        stop_reason: stop,
        usage,
        model: t.model.clone(),
    })
}

pub fn budget_tokens(level: &str) -> u64 {
    level
        .trim()
        .parse::<u64>()
        .unwrap_or(match level.to_lowercase().as_str() {
            "minimal" => 1024,
            "low" => 4_000,
            "medium" => 8_000,
            "high" => 16_000,
            "xhigh" => 24_000,
            "max" => 32_000,
            _ => 0,
        })
}

/// List model ids a provider serves (`GET /models`), for the add-model picker.
pub async fn list_remote_models(
    http: &reqwest::Client,
    settings: &Settings,
    prov: &str,
) -> Result<Vec<Value>, String> {
    let key = api_key(settings, prov);
    let base = base_url(settings, prov);
    if base.is_empty() {
        return Err("This provider has no base URL.".into());
    }
    let anth = kind(settings, prov) == "anthropic";
    let url = if anth {
        format!("{base}/v1/models?limit=1000")
    } else {
        format!("{base}/models")
    };
    let mut rb = http.get(url).timeout(std::time::Duration::from_secs(20));
    if anth {
        rb = rb
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01");
    } else if !key.is_empty() {
        rb = rb.bearer_auth(&key);
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad response: {e}"))?;
    if !status.is_success() {
        return Err(v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or(format!("HTTP {}", status.as_u16())));
    }
    let arr = v["data"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .map(|m| {
            json!({
                "id": m["id"].as_str().or(m["name"].as_str()).unwrap_or(""),
                "name": m["display_name"].as_str().or(m["name"].as_str()).unwrap_or(""),
                "context": m["context_length"].as_u64().or(m["max_input_tokens"].as_u64()).or(m.pointer("/top_provider/context_length").and_then(|x| x.as_u64())),
                "output": m["max_tokens"].as_u64().or(m.pointer("/top_provider/max_completion_tokens").and_then(|x| x.as_u64())),
                "input_price": m.pointer("/pricing/prompt").and_then(|x| x.as_str()).and_then(|x| x.parse::<f64>().ok()).map(|x| x * 1e6),
                "output_price": m.pointer("/pricing/completion").and_then(|x| x.as_str()).and_then(|x| x.parse::<f64>().ok()).map(|x| x * 1e6),
                "modalities": m.pointer("/architecture/input_modalities").cloned(),
                "reasoning_levels": m.get("reasoning_levels").or_else(|| m.get("supported_reasoning_levels")),
                "reasoning_param": m.get("reasoning_param").or_else(|| m.get("reasoning_parameter")),
                "supported_parameters": m.get("supported_parameters"),
            })
        })
        .filter(|m| m["id"].as_str().is_some_and(|s| !s.is_empty()))
        .collect())
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn zen_models_use_their_protocol() {
        let s = Settings::default();
        assert_eq!(
            wire(&s, "opencode", "muse-spark-1.3-contributor-free"),
            "responses"
        );
        assert_eq!(wire(&s, "opencode", "gpt-6-astra"), "responses");
        assert_eq!(wire(&s, "opencode", "claude-sonnet-4-5"), "anthropic");
        assert_eq!(wire(&s, "opencode-go", "qwen3.7-max"), "anthropic");
        assert_eq!(wire(&s, "opencode", "kimi-k2.6"), "openai");
        assert_eq!(
            wire(&s, "openai", "gpt-5"),
            "openai",
            "other providers unchanged"
        );
        assert_eq!(
            api_url("https://opencode.ai/zen/v1", "messages"),
            "https://opencode.ai/zen/v1/messages"
        );
        assert_eq!(
            api_url("https://api.anthropic.com", "messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    /// A registered Go model has to be dispatched through the endpoint its family
    /// speaks. The list endpoint returns ids only, so the wire is re-derived from
    /// the id — and getting it wrong is silent: the gateway answers whichever
    /// endpoint you called with whichever body you sent, or rejects it as a
    /// format mismatch, rather than telling you the model wanted the other one.
    #[test]
    fn registered_go_models_keep_their_families_protocol() {
        let list = json!({"object": "list", "data": [
            {"id": "kimi-k3"}, {"id": "glm-5.3"}, {"id": "deepseek-v4-pro"},
            {"id": "qwen3.7-plus"}, {"id": "grok-4.7"}, {"id": "gpt-6-luna"},
            {"id": "muse-spark-1.3-contributor"}
        ]});
        let models = parse_go_models(&list);
        assert_eq!(models.len(), 7);
        assert!(models.iter().all(|m| m.provider == "opencode-go"));
        assert!(models.iter().all(|m| m.id.starts_with("opencode-go/")));
        // Names are rebuilt from the id, not left lowercase.
        let kimi = models
            .iter()
            .find(|m| m.id == "opencode-go/kimi-k3")
            .unwrap();
        assert_eq!(kimi.name, "Kimi K3");
        assert_eq!(
            models
                .iter()
                .find(|m| m.id == "opencode-go/deepseek-v4-pro")
                .unwrap()
                .name,
            "DeepSeek V4 Pro"
        );
        // The whole real lineup, so a new model with an odd shape shows up here
        // rather than as a mangled picker label. `glm-5.3-flash` → "GLM 5.3 Flash",
        // `mimo-v2.6-pro` → "MiMo V2.6 Pro", `muse-spark-1.3-contributor` keeps
        // its brand. A regression in `go_word` lands on one of these.
        for (id, want) in [
            ("glm-5.3-flash", "GLM 5.3 Flash"),
            ("glm-5.3", "GLM 5.3"),
            ("kimi-k2.7-code", "Kimi K2.7 Code"),
            ("mimo-v2.6-pro", "MiMo V2.6 Pro"),
            ("minimax-m2.7", "MiniMax M2.7"),
            ("qwen3.8-max", "Qwen3.8 Max"),
            ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
            ("muse-spark-1.3-contributor", "Muse Spark 1.3 Contributor"),
            ("gpt-6-luna", "GPT 6 Luna"),
            ("grok-4.7", "Grok 4.7"),
            ("longcat-2.0", "LongCat 2.0"),
            ("hy3", "Hy3"),
        ] {
            assert_eq!(go_model_name(id), want, "display name for {id}");
        }
        // …and the protocol matches what the live path would pick for the same id.
        let s = Settings::default();
        for m in &models {
            let wire = wire(&s, "opencode-go", m.id.split_once('/').unwrap().1);
            assert_ne!(wire, "", "{} has a wire", m.id);
        }
        assert_eq!(wire(&s, "opencode-go", "qwen3.7-plus"), "anthropic");
        assert_eq!(wire(&s, "opencode-go", "grok-4.7"), "responses");
        assert_eq!(wire(&s, "opencode-go", "kimi-k3"), "openai");
    }

    /// The three account kinds differ in one way that decides which host a
    /// credential is sent to: codex/claude carry an OAuth session for the
    /// vendor's own backend, Go carries an API key. `credential` must hand back
    /// the account token for a key-login account and the provider key for the
    /// rest, or an OAuth session would be posted to a public API as a bearer.
    #[test]
    fn only_a_key_login_account_gives_up_its_token_as_a_credential() {
        let mut s = Settings::default();
        s.providers.insert(
            "opencode-go".into(),
            crate::agent::store::ProviderCfg {
                api_key: "sk-configured".into(),
                ..Default::default()
            },
        );
        let go_acct = Account {
            id: "g".into(),
            kind: "opencode-go".into(),
            access_token: "sk-account".into(),
            ..Default::default()
        };
        let t = Target {
            model_id: "opencode-go/kimi-k3".into(),
            prov: "opencode-go".into(),
            model: "kimi-k3".into(),
            account: Some(go_acct.clone()),
            api_key: None,
        };
        assert_eq!(credential(&s, &t), "sk-account");
        assert!(
            oauth_account(&t).is_none(),
            "a key login takes no OAuth path"
        );

        let codex = Account {
            id: "c".into(),
            kind: "codex".into(),
            access_token: "session-token".into(),
            ..Default::default()
        };
        let t = Target {
            model_id: "codex/gpt-6-sol".into(),
            prov: "codex".into(),
            model: "gpt-6-sol".into(),
            account: Some(codex),
            api_key: None,
        };
        assert!(oauth_account(&t).is_some(), "codex keeps its OAuth session");
        // An explicit pool key still wins, for every kind.
        let t = Target {
            api_key: Some("pool-key".into()),
            ..t
        };
        assert_eq!(credential(&s, &t), "pool-key");
    }

    /// The Go provider is the one that is both a pool *and* a key provider, so
    /// that an install which only ever set `OPENCODE_API_KEY` keeps working.
    #[test]
    fn opencode_go_is_connectable_by_key_or_account() {
        assert!(is_pool("opencode-go") && key_login("opencode-go"));
        for p in ["codex", "claude"] {
            assert!(is_pool(p) && !key_login(p), "{p} is accounts-only");
        }
        for p in ["anthropic", "openai", "opencode"] {
            assert!(!is_pool(p) && !key_login(p), "{p} is key-only");
        }
    }
}

#[cfg(test)]
mod account_terms_tests {
    use super::*;

    fn acct(id: &str) -> AccountProviderInfo {
        PROVIDERS
            .iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("{id} is in PROVIDERS"))
            .account
            .unwrap_or_else(|| panic!("{id} is an account provider"))
    }

    /// Anthropic's consumer terms do not cover driving a subscription from a
    /// third-party agent, so the Claude provider has to carry a gate and the
    /// user has to acknowledge it before they reach the connect dialog. If this
    /// fails, somebody dropped the gate or the strings behind it and the app is
    /// one click from importing a subscription it should have warned about.
    #[test]
    fn claude_carries_a_terms_gate_the_user_has_to_acknowledge() {
        let gate = acct("claude").terms_gate.expect("claude is gated");
        assert!(
            !gate.title.is_empty() && !gate.lede.is_empty() && !gate.accept.is_empty(),
            "every part of the gate has copy: {gate:?}"
        );
        assert!(
            gate.points.len() >= 2,
            "a gate that cites no clause is just a scolding: {gate:?}"
        );
        assert!(
            gate.points.iter().all(|p| p.len() > 40),
            "each point is the clause itself, not a label: {gate:?}"
        );
        assert!(
            gate.terms_url
                .starts_with("https://www.anthropic.com/legal/"),
            "the link must be the terms, not a blog post about them: {}",
            gate.terms_url
        );
        // "At my own risk" is the whole point of the accept label. If it is
        // edited down to something reassuring the gate stops doing its job.
        assert!(
            gate.accept.to_lowercase().contains("own risk"),
            "the accept button has to name the risk: {}",
            gate.accept
        );
    }

    /// The other direction, and the one a new account provider silently fails:
    /// a provider that gains an `account` block with no gate gets the connect
    /// dialog with nothing in front of it. Codex is ungated because its terms
    /// do not forbid this, so `None` there is a decision — pin it, so the day
    /// somebody adds a gated provider they have to think about this list.
    #[test]
    fn every_account_provider_has_thought_about_the_gate() {
        let gated: Vec<&str> = PROVIDERS
            .iter()
            .filter_map(|p| {
                let a = p.account?;
                a.terms_gate.is_some().then_some(p.id)
            })
            .collect();
        assert_eq!(
            gated,
            ["claude"],
            "the gated set is a decision, not an accident"
        );
        assert!(
            acct("codex").terms_gate.is_none(),
            "codex needs no gate: its terms do not forbid a third-party harness"
        );
    }

    /// Both surfaces have to say it. The gate is what the user reads before
    /// committing; the footer warning is what they can still see after the fact
    /// while the account sits in the list. Removing either one leaves a hole.
    #[test]
    fn the_gate_and_the_footer_warning_agree_that_claude_is_not_covered() {
        let a = acct("claude");
        assert!(
            !a.warning.is_empty(),
            "the connect form still carries the footnote"
        );
        let w = a.warning.to_lowercase();
        assert!(
            w.contains("terms") && (w.contains("violate") || w.contains("may")),
            "the footnote has to name the terms and the possibility: {}",
            a.warning
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hint_never_panics_on_a_key_that_ends_mid_codepoint() {
        // 9 ASCII bytes, but a trailing 3-byte CJK char: `key.len() - 4` lands
        // inside it. The release profile is panic=abort, so this used to kill the app.
        assert_eq!(
            key_hint("sk-abc123\u{4e2d}\u{6587}\u{5b57}"),
            "\u{2026}3\u{4e2d}\u{6587}\u{5b57}"
        );
        assert_eq!(key_hint("sk-abcdefgh"), "\u{2026}efgh");
        // 4-byte emoji: the last 4 *chars* span fewer than 4 bytes.
        assert_eq!(key_hint("sk-123456\u{1f600}"), "\u{2026}456\u{1f600}");
        assert_eq!(key_hint("short"), "");
        assert_eq!(key_hint(""), "");
    }

    #[test]
    fn sse_framing_survives_a_codepoint_split_across_chunks() {
        // The bug this guards: decoding per chunk turned a multi-byte char that
        // straddled a TCP boundary into U+FFFD, corrupting model output forever.
        let src = "data: {\"t\":\"中文 ok\"}\n\ndata: second\n\n";
        let bytes = src.as_bytes();
        for split in 1..bytes.len() {
            let mut buf: Vec<u8> = Vec::new();
            let mut got: Vec<String> = Vec::new();
            for part in [&bytes[..split], &bytes[split..]] {
                buf.extend_from_slice(part);
                while let Some(pos) = find_event_end(&buf) {
                    let raw: Vec<u8> = buf.drain(..pos).collect();
                    let event = String::from_utf8_lossy(&raw);
                    let data: Vec<&str> = event
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:"))
                        .map(|l| l.trim_start())
                        .collect();
                    if !data.is_empty() {
                        got.push(data.join("\n"));
                    }
                }
            }
            assert_eq!(
                got,
                vec!["{\"t\":\"中文 ok\"}", "second"],
                "split at byte {split} lost data"
            );
        }
    }

    #[test]
    fn sse_framing_handles_crlf_and_a_trailing_partial_event() {
        assert_eq!(find_event_end(b"a\r\n\r\nb"), Some(5));
        assert_eq!(find_event_end(b"a\n\nb"), Some(3));
        assert_eq!(
            find_event_end(b"a\nb"),
            None,
            "no blank line yet, keep buffering"
        );
        assert_eq!(
            find_event_end(b"trailing"),
            None,
            "an unterminated tail is not an event"
        );
    }

    #[test]
    fn provider_catalog_has_complete_unique_entries() {
        let mut ids = std::collections::HashSet::new();
        for p in PROVIDERS {
            assert!(ids.insert(p.id), "duplicate provider: {}", p.id);
            assert!(!p.name.is_empty() && !p.icon.is_empty() && !p.kind.is_empty());
            assert_eq!(is_pool(p.id), p.account.is_some());
            if let Some(account) = p.account {
                assert!(!account.display_name.is_empty() && !account.login_command.is_empty());
            }
        }
        assert_eq!(
            Settings::default().home_usage,
            PROVIDERS
                .iter()
                .filter(|p| p.account.is_some())
                .map(|p| p.id.to_string())
                .collect::<Vec<_>>()
        );
        assert!(PRESETS
            .iter()
            .all(|p| !p.name.is_empty() && !p.url.is_empty() && !p.icon.is_empty()));
    }

    #[test]
    fn effort_spreads_over_custom_levels() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let three = l(&["low", "medium", "high"]);
        assert_eq!(pick_level(&three, 0).as_deref(), Some("high"));
        assert_eq!(pick_level(&three, 2).as_deref(), Some("medium"));
        assert_eq!(pick_level(&three, 4).as_deref(), Some("low"));
        let two = l(&["off", "on"]);
        assert_eq!(pick_level(&two, 4).as_deref(), Some("off"));
        assert_eq!(pick_level(&two, 1).as_deref(), Some("on"));
        assert_eq!(pick_level(&[], 2), None);
        assert_eq!(budget_tokens("high"), 16_000);
        assert_eq!(budget_tokens("12000"), 12_000);
    }

    #[test]
    fn classifies_errors() {
        let h = reqwest::header::HeaderMap::new();
        assert!(matches!(
            classify(
                429,
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":60}}"#,
                &h
            ),
            Fail::Exhausted { hard: true, .. }
        ));
        assert!(matches!(
            classify(429, r#"{"error":{"message":"slow down"}}"#, &h),
            Fail::Exhausted { hard: false, .. }
        ));
        assert!(matches!(
            classify(
                400,
                r#"{"error":{"message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#,
                &h
            ),
            Fail::TooLong(_)
        ));
        assert!(matches!(
            classify(404, r#"{"error":{"message":"model not found"}}"#, &h),
            Fail::Bad(_)
        ));
        assert!(matches!(
            classify(529, "overloaded", &h),
            Fail::Transient(_)
        ));
        assert!(matches!(classify(401, "nope", &h), Fail::Auth(_)));
    }

    #[test]
    fn error_bodies_never_carry_a_key_into_the_transcript() {
        // A misconfigured custom `base_url` is usually a proxy, and proxies
        // reflect requests. Whatever comes back is echoed into the chat and
        // persisted to ~/.openleash/tasks/*.json, so the scrub has to hold.
        let h = reqwest::header::HeaderMap::new();
        // Only the classified message is user-visible; the raw body is not.
        let shown = |body: &str| match classify(502, body, &h) {
            Fail::Transient(m) | Fail::Bad(m) | Fail::Auth(m) => m,
            other => format!("{other:?}"),
        };

        let cases = [
            "upstream said: Bearer sk-ant-api03-AAAAbbbbCCCCddddEEEEffff",
            r#"{"error":{"message":"invalid x-api-key: sk-live-9f8e7d6c5b4a3210"}}"#,
            r#"{"error":{"message":"bad Authorization: api_key=abcd1234efgh5678ijkl"}}"#,
            "<html><body>token ghp_16CharactersOfStuffHere was rejected</body></html>",
        ];
        for body in cases {
            let out = shown(body);
            assert!(!out.contains("sk-ant-api03"), "sk key survived: {out}");
            assert!(!out.contains("sk-live-9f8e"), "sk key survived: {out}");
            assert!(
                !out.contains("abcd1234efgh"),
                "api_key value survived: {out}"
            );
            assert!(!out.contains("ghp_16Char"), "bare token survived: {out}");
        }

        // The scrubbing must not eat the error's actual meaning.
        let kept = shown(r#"{"error":{"message":"model claude-x not found"}}"#);
        assert!(
            kept.contains("not found"),
            "over-scrubbed a useful message: {kept}"
        );
    }

    /// Streamed deltas are accumulated outside the block's JSON and folded in
    /// once at `content_block_stop`. That fold is the whole point of the change
    /// (it turns a per-token whole-answer copy into a `push_str`), so the shape
    /// the fold produces is pinned here rather than left to the wire: three
    /// interleaved blocks, each streaming several deltas, all landing in the
    /// right slot with nothing bled between them.
    #[test]
    fn streamed_deltas_fold_into_their_own_blocks() {
        use super::StreamAcc;
        let mut blocks: Vec<Value> = vec![];
        let mut acc: Vec<StreamAcc> = vec![];

        // Same growth the reader sees: `content_block_start` opens the slot.
        for (idx, kind) in [(0usize, "text"), (1, "thinking"), (2, "text")] {
            while blocks.len() <= idx {
                blocks.push(Value::Null);
                acc.push(StreamAcc::default());
            }
            blocks[idx] = json!({"type": kind});
        }
        // Interleaved deltas, as they actually arrive on the wire.
        for (idx, field, chunk) in [
            (0usize, "text", "Hello"),
            (1, "thinking", "let me "),
            (2, "text", "Second"),
            (0, "text", ", world"),
            (1, "thinking", "think"),
            (1, "signature", "sig-abc"),
            (2, "text", " block"),
        ] {
            match field {
                "text" => acc[idx].text.push_str(chunk),
                "thinking" => acc[idx].thinking.push_str(chunk),
                _ => acc[idx].signature.push_str(chunk),
            }
        }
        // The fold `content_block_stop` performs.
        for (i, b) in blocks.iter_mut().enumerate() {
            if !acc[i].text.is_empty() {
                b["text"] = Value::String(acc[i].text.clone());
            }
            if !acc[i].thinking.is_empty() {
                b["thinking"] = Value::String(acc[i].thinking.clone());
            }
            if !acc[i].signature.is_empty() {
                b["signature"] = Value::String(acc[i].signature.clone());
            }
        }

        assert_eq!(blocks[0]["text"], json!("Hello, world"));
        assert_eq!(blocks[1]["thinking"], json!("let me think"));
        assert_eq!(blocks[1]["signature"], json!("sig-abc"));
        assert_eq!(blocks[2]["text"], json!("Second block"));
        // A text block never picks up a thinking field from a neighbour.
        assert!(
            blocks[0].get("thinking").is_none(),
            "field bled across blocks"
        );
    }

    /// A block that opened with text already in `content_block_start` and then
    /// streamed nothing must keep it, and a block that streamed text must not
    /// lose its other `content_block_start` keys (id/name/type). The fold is
    /// conditional on the accumulator being non-empty precisely so the first
    /// case survives.
    #[test]
    fn a_block_that_streams_nothing_keeps_what_it_started_with() {
        use super::StreamAcc;
        let block = json!({"type":"tool_use","id":"t1","name":"read_file"});
        let acc = StreamAcc::default();
        let mut b = block.clone();
        if !acc.text.is_empty() {
            b["text"] = Value::String(acc.text.clone());
        }
        assert_eq!(b, block, "an empty accumulator must not rewrite the block");
    }

    #[test]
    fn thinking_only_replays_to_its_model() {
        let msgs = vec![Message {
            role: "assistant".into(),
            content: vec![
                json!({"type":"thinking","thinking":"x","signature":"s"}),
                json!({"type":"text","text":"hi"}),
            ],
            model: "claude-opus-5".into(),
        }];
        assert_eq!(
            anthropic_messages(&msgs, "claude-opus-5")[0]["content"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            anthropic_messages(&msgs, "claude-sonnet-5")[0]["content"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn image_tool_results_convert_per_backend() {
        let tool = json!({"type":"tool_result","tool_use_id":"c1","content":[
            {"type":"text","text":"Viewed a.png (image/png, 100 bytes)."},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"QUJD"}},
        ]});
        assert_eq!(
            tool_result_text(&tool),
            "Viewed a.png (image/png, 100 bytes)."
        );
        assert_eq!(
            tool_result_images(&tool),
            vec![("image/png".to_string(), "QUJD".to_string())]
        );
        // Anthropic keeps the array as-is.
        let msgs = vec![
            Message::user_text("see it"),
            Message {
                role: "assistant".into(),
                content: vec![
                    json!({"type":"tool_use","id":"c1","name":"view_image","input":{"path":"a.png"}}),
                ],
                model: String::new(),
            },
            Message::user(vec![tool]),
        ];
        let a = anthropic_messages(&msgs, "m");
        assert_eq!(
            a[2]["content"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        // OpenAI-compat: one tool message with text + image_url parts.
        let o = to_openai_messages("sys", &msgs);
        let tm = o.iter().find(|m| m["role"] == "tool").unwrap();
        assert_eq!(tm["tool_call_id"], "c1");
        assert_eq!(tm["content"][0]["type"], "text");
        assert_eq!(tm["content"][1]["type"], "image_url");
        assert!(tm["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        // Codex: text stays in function_call_output, image follows as a user message.
        let c = codex_input(&msgs);
        assert_eq!(c[1]["type"], "function_call");
        assert_eq!(c[2]["type"], "function_call_output");
        assert_eq!(c[3]["type"], "message");
        assert_eq!(c[3]["content"][0]["type"], "input_image");
    }

    /// Images the user pasted (or a parent agent handed a sub-agent) sit
    /// directly on a user turn. They used to be dropped silently on every
    /// non-Anthropic backend, so the model only ever saw the caption.
    #[test]
    fn user_turn_images_reach_every_backend() {
        let img = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}});
        let with_text = vec![Message::user(vec![
            img.clone(),
            json!({"type": "text", "text": "why is this broken?"}),
        ])];
        // OpenAI-compat (openrouter / openai / google / zai / opencode).
        let o = to_openai_messages("sys", &with_text);
        let um = o
            .iter()
            .find(|m| m["role"] == "user")
            .expect("the user turn survives");
        let parts = um["content"].as_array().expect("array content");
        assert_eq!(parts.len(), 2, "caption + image: {parts:?}");
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert!(parts[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        // An image with no caption at all still has to become a message.
        let o2 = to_openai_messages("sys", &[Message::user(vec![img])]);
        let um2 = o2
            .iter()
            .find(|m| m["role"] == "user")
            .expect("image-only turn survives");
        assert_eq!(
            um2["content"][0]["type"], "image_url",
            "{:?}",
            um2["content"]
        );
        // A text-only turn stays a plain string (unchanged behaviour).
        let o3 = to_openai_messages("sys", &[Message::user_text("just text")]);
        assert_eq!(o3[1]["content"], "just text");
        // Codex / Responses API: its own user message with input_image.
        let c = codex_input(&with_text);
        let cu = c
            .iter()
            .find(|m| m["content"][0]["type"] == "input_image")
            .expect("image reaches codex");
        assert_eq!(cu["role"], "user");
        assert!(cu["content"][0]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        // Anthropic keeps the blocks verbatim.
        let a = anthropic_messages(&with_text, "m");
        assert_eq!(a[0]["content"][0]["type"], "image");
        assert_eq!(a[0]["content"][1]["type"], "text");
    }

    #[test]
    fn opencode_spoof_ids_match_gatekeeper_format() {
        // ses_<12 hex><14 alnum>, stable per task.
        let a = opencode_session_id("ol-abc123def456");
        let b = opencode_session_id("ol-abc123def456");
        assert_eq!(a, b);
        assert_ne!(a, opencode_session_id("ol-other-task-1"));
        assert!(a.starts_with("ses_"), "{a}");
        let tail = &a[4..];
        assert_eq!(tail.len(), 26, "{a}");
        assert!(
            tail[..12]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{a}"
        );
        assert!(tail[12..].chars().all(|c| c.is_ascii_alphanumeric()), "{a}");
        let r = opencode_request_id();
        assert!(r.starts_with("msg_"), "{r}");
    }
}
