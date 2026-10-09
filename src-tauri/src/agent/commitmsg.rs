//! Commit messages: what an agent-made commit says about itself.
//!
//! Three things live here, and the split matters.
//!
//! 1. **Shaping.** Models answer a "write me a commit message" request with a
//!    markdown fence, a "Here is your commit message:" preamble, a trailing
//!    "I hope this helps!", or a 400-character subject line. All of that is
//!    text wrangling with no I/O in it, so it lives in [`shape`] and is tested
//!    case by case in `reviewer_commit.rs`. This is where the bugs are.
//! 2. **Attribution.** A trailer so somebody reading `git log` can tell an
//!    agent wrote the commit, and can turn it off.
//! 3. **The prompt**, kept next to the shaping that has to undo it.
//!
//! The model call lives in [`complete`], the provider seam.

use super::git;
use super::{accounts, providers, router, Message};
use serde::Serialize;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Who gets the credit in the trailer. The app, not the model: the model id
/// changes per chat and per swap, and a trailer that says which model happened
/// to be selected when the user pressed Commit is noise, not provenance.
pub const AGENT_NAME: &str = "OpenLeash";
pub const AGENT_EMAIL: &str = "agent@openleash.dev";

/// Conventional Commits' soft limit on the subject line. Over this, `git log
/// --oneline` truncates and other tooling wraps it; we cut it at a word.
pub const MAX_SUBJECT: usize = 72;

/// Bound on what we will even look at. A model that answers with an essay, or
/// repeats the diff back, must not become a 200 kB commit message.
const MAX_RAW: usize = 8_000;

/// How much of the change the prompt carries. A commit message needs the shape
/// of the change, not every line of it, and this rides a cheap model anyway.
const MAX_PROMPT_FILES: usize = 40;
const MAX_PROMPT_LINES: usize = 200;
const MAX_PROMPT_CHARS: usize = 12_000;

/// Conventional Commits' type vocabulary. Used only to *recognise* a subject
/// (to skip past preamble, and to leave a real header alone); a subject that
/// does not use one is passed through rather than mangled.
const TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

/// The system prompt. Names the format, lists the types, and bans the two
/// habits the shaping below exists to undo (fences, commentary) so the common
/// case needs no cleanup at all.
pub const PROMPT: &str = "You write git commit messages. Reply with the commit message and nothing else — no code fences, no quotes, no explanation, no \"Here is your commit message\".

Use the Conventional Commits format:

    <type>[optional scope]: <description>

    [optional body]

    [optional footer(s)]

Rules:
- <type> is one of: feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert.
- The description is imperative mood, lower case, no trailing period, at most 72 characters.
- Add a body only when the change needs it: wrap at 72 columns, explain what and why, not how.
- Never mention the assistant, the model, tickets you were not told about, or files that did not change.
- Do not add Co-authored-by or any other trailer; the app adds its own.";

/// How an agent-made commit is attributed in `git log`.
///
/// A string-backed enum rather than a bool per style: the setting is one
/// choice, and settings.json round-trips it as the same string the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    None,
    CoAuthoredBy,
    AssistedBy,
    GeneratedWith,
}

impl Attribution {
    /// Parse the stored setting. Anything unrecognised — including the empty
    /// string a settings.json written before this field existed produces — is
    /// the *default*, not `None`: attribution is what makes an agent-made
    /// commit honest to a reader of `git log`, so an old settings file must not
    /// silently turn it off. Only an explicit "none" does that.
    pub fn parse(s: &str) -> Attribution {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "false" => Attribution::None,
            "assisted-by" | "assisted_by" => Attribution::AssistedBy,
            "generated-with" | "generated_with" => Attribution::GeneratedWith,
            _ => Attribution::CoAuthoredBy,
        }
    }

    /// The value as stored in settings.json.
    pub fn as_str(&self) -> &'static str {
        match self {
            Attribution::None => "none",
            Attribution::CoAuthoredBy => "co-authored-by",
            Attribution::AssistedBy => "assisted-by",
            Attribution::GeneratedWith => "generated-with",
        }
    }

    /// The line appended to the message, or empty for `None`.
    pub fn trailer(&self) -> String {
        match self {
            Attribution::None => String::new(),
            Attribution::CoAuthoredBy => format!("Co-authored-by: {AGENT_NAME} <{AGENT_EMAIL}>"),
            Attribution::AssistedBy => format!("Assisted-by: {AGENT_NAME} <{AGENT_EMAIL}>"),
            Attribution::GeneratedWith => format!("Generated with {AGENT_NAME}"),
        }
    }

    /// The marker used to tell "the model already wrote this trailer" from "we
    /// need to add it". Lower-cased before comparing.
    fn token(&self) -> &'static str {
        match self {
            Attribution::None => "",
            Attribution::CoAuthoredBy => "co-authored-by:",
            Attribution::AssistedBy => "assisted-by:",
            Attribution::GeneratedWith => "generated with",
        }
    }
}

/// A generated message and where it came from, so the UI can say "the model is
/// not wired up yet, this is a local guess" instead of pretending.
#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub message: String,
    /// "model" | "fallback"
    pub source: String,
}

/// Turn whatever the model replied into a Conventional Commits message.
///
/// Returns an empty string when there is nothing usable in the reply; callers
/// fall back to [`fallback_message`] rather than committing an empty message
/// (git would refuse it, which reads as "the feature is broken").
pub fn shape(raw: &str) -> String {
    let raw = truncate_chars(raw.trim(), MAX_RAW);
    if raw.is_empty() {
        return String::new();
    }

    // A fenced answer wins outright: everything outside the fence is the model
    // talking to us, and this is the shape a model uses when it *is* being
    // careful about the message.
    let body = match first_fence(&raw) {
        Some(inner) => inner,
        // No fence: drop the introduction lines a model prepends. Only leading
        // ones, and only until the first line that is not one, so a body that
        // mentions "commit message" later survives.
        None => {
            let mut lines: Vec<&str> = raw.lines().collect();
            while let Some(first) = lines.first() {
                if first.trim().is_empty() || is_preamble(first) {
                    lines.remove(0);
                } else {
                    break;
                }
            }
            lines.join("\n")
        }
    };

    let mut lines: Vec<String> = body.lines().map(|l| l.trim_end().to_string()).collect();

    // `#` starts a comment in a commit message (git strips such lines when it
    // cleans one up), so a model that writes them means them as notes to us,
    // not as content. Drop every one, wherever it is.
    lines.retain(|l| !l.trim_start().starts_with('#'));

    // If a Conventional Commits header is present anywhere, that is the real
    // subject: start there, whatever preamble survived above it.
    if let Some(i) = lines.iter().position(|l| is_conventional(l)) {
        lines.drain(..i);
    }

    // Drop a trailing paragraph of assistant chatter — the "I hope this helps!"
    // that follows the message. Paragraph-wise, not line-wise: those remarks
    // run to more than one line, and half of one is worse than all of it.
    while let Some(last) = lines.last() {
        if last.trim().is_empty() {
            lines.pop();
            continue;
        }
        if !is_chatter(last) {
            break;
        }
        while let Some(l) = lines.last() {
            if l.trim().is_empty() {
                break;
            }
            lines.pop();
        }
    }

    let subject = lines
        .first()
        .map(|s| collapse_ws(s))
        .unwrap_or_default()
        .trim_end_matches('.')
        .trim_end()
        .to_string();
    let subject = if subject.chars().count() > MAX_SUBJECT {
        cut_at_word(&subject, MAX_SUBJECT)
    } else {
        subject
    };
    if subject.is_empty() {
        return String::new();
    }

    let mut rest: Vec<String> = lines.into_iter().skip(1).collect();
    while rest.first().is_some_and(|l| l.trim().is_empty()) {
        rest.remove(0);
    }
    while rest.last().is_some_and(|l| l.trim().is_empty()) {
        rest.pop();
    }
    let body = rest.join("\n").trim_end().to_string();
    if body.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{body}")
    }
}

/// Fold the attribution trailer onto a message.
///
/// Idempotent by design: the Generate button fills the composer with the final
/// message (trailer and all), and the commit path runs this again on whatever
/// the user left there — so it must not stack a second `Co-authored-by` on top
/// of the first. It also means a user who types their own trailer keeps it.
pub fn compose(message: &str, attribution: Attribution) -> String {
    let message = message.trim();
    let trailer = attribution.trailer();
    if trailer.is_empty() {
        return message.to_string();
    }
    if message
        .to_ascii_lowercase()
        .contains(&attribution.token().to_ascii_lowercase())
    {
        return message.to_string();
    }
    if message.is_empty() {
        return trailer;
    }
    format!("{message}\n\n{trailer}")
}

/// A message made from the change alone, for when there is no model answer to
/// use. Deterministic and honest: it never guesses a `feat:` where it cannot
/// know one, so `chore:` is the type.
pub fn fallback_message(files: &[git::FileDiff]) -> String {
    match files.len() {
        0 => "chore: no tracked changes".to_string(),
        1 => format!("chore: update {}", files[0].path),
        n => format!("chore: update {n} files"),
    }
}

/// Shape a model reply, falling back to a deterministic message.
///
/// Split out from [`generate`] so the whole "reply in, message out" rule is
/// testable without a harness or a provider in the loop.
pub fn draft_from_raw(raw: &str, files: &[git::FileDiff]) -> Draft {
    let message = shape(raw);
    if message.is_empty() {
        Draft {
            message: fallback_message(files),
            source: "fallback".to_string(),
        }
    } else {
        Draft {
            message,
            source: "model".to_string(),
        }
    }
}

/// Draft a message for the change, using the model when available.
///
/// A provider failure is not an error here: the Generate button has to produce
/// something the user can commit, so a dead provider or unusable answer falls
/// back to the deterministic message and says so in `source`.
pub async fn generate(h: &Arc<super::Harness>, task_id: &str, files: &[git::FileDiff]) -> Draft {
    match complete(h, task_id, PROMPT, &user_prompt(files)).await {
        Ok(raw) => draft_from_raw(&raw, files),
        Err(_) => Draft {
            message: fallback_message(files),
            source: "fallback".to_string(),
        },
    }
}

/// The provider seam. This is a side request: it has no tools or chat history,
/// cannot pause the task, and records its spend under a distinct task usage id.
/// The best enabled, connected cheaper model in the task model's provider is
/// preferred; the router still owns retries and any user-configured fallbacks.
pub async fn complete(
    h: &Arc<super::Harness>,
    task_id: &str,
    system_prompt: &str,
    user: &str,
) -> Result<String, String> {
    let task = h.task(task_id).await?;
    let (task_model, route_id) = {
        let task = task.lock().await;
        (task.model.clone(), task.route.clone())
    };
    let settings = h.settings.read().await.clone();
    let model = choose_model(h, &settings, &task_model, &route_id)?;
    let req = request(system_prompt, user, task_id);
    let (text, usage, target) = router::oneshot(
        h,
        router::Who {
            task_id,
            sub: None,
            side: true,
            turn: false,
            usage_id: "commit-message",
        },
        &model,
        &req,
        &CancellationToken::new(),
    )
    .await
    .map_err(route_error)?;

    let cost = usage.cost(&providers::model_info(&target.model_id));
    let tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
    record_spend(h, cost, tokens).await;
    h.update_task(task_id, |task| {
        task.usage.input += usage.input;
        task.usage.output += usage.output;
        task.usage.cache_read += usage.cache_read;
        task.usage.cache_write += usage.cache_write;
        task.usage.cost += cost;
    })
    .await;
    h.save_task(task_id).await;

    // Router::oneshot already writes the detailed request sample to Stats,
    // including model, account, task, agent kind and this call's usage_id.
    Ok(text)
}

fn request(system: &str, user: &str, task_id: &str) -> providers::ChatRequest {
    providers::ChatRequest {
        system: system.to_string(),
        messages: vec![Message::user_text(user)],
        tools: vec![],
        effort: 4,
        max_tokens: 512,
        cache_key: format!("ol-commitmsg-{task_id}"),
    }
}

fn route_error(error: router::RouteErr) -> String {
    match error {
        router::RouteErr::Fatal(message) | router::RouteErr::TooLong(message) => message,
        router::RouteErr::Cancelled | router::RouteErr::Refresh => {
            "commit-message generation was cancelled".to_string()
        }
    }
}

/// Resolve a live task model, preferring any enabled cheaper model sibling
/// that is actually connected. Provider fallbacks remain owned by the router.
fn choose_model(
    h: &Arc<super::Harness>,
    settings: &super::store::Settings,
    task_model: &str,
    route_id: &str,
) -> Result<String, String> {
    let (chain, _) = router::steps(task_model, route_id);
    let base = chain
        .iter()
        .find(|model| model_available(h, settings, model))
        .ok_or_else(|| {
            "no connected model is available for commit-message generation".to_string()
        })?;
    let base_info = model_info(settings, base);
    let weak = providers::all_models()
        .into_iter()
        .filter(|candidate| {
            candidate.provider == base_info.provider
                && candidate.id.as_str() != base.as_str()
                && candidate.enabled
                && !settings.disabled_models.contains(&candidate.id)
                && !settings.removed_models.contains(&candidate.id)
                && model_available(h, settings, &candidate.id)
                && cheaper_or_weaker(candidate, &base_info)
        })
        .min_by(|a, b| {
            let price_a = a.input_price + a.output_price;
            let price_b = b.input_price + b.output_price;
            price_a
                .partial_cmp(&price_b)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| weak_model(b).cmp(&weak_model(a)))
        });
    Ok(weak.map_or_else(|| task_model.to_string(), |model| model.id))
}

fn model_info(settings: &super::store::Settings, id: &str) -> providers::ModelInfo {
    providers::all_models()
        .into_iter()
        .find(|model| model.id.as_str() == id)
        .or_else(|| {
            settings
                .model_configs
                .iter()
                .find(|model| model.id.as_str() == id)
                .cloned()
        })
        .unwrap_or_else(|| providers::model_info(id))
}

fn model_available(h: &Arc<super::Harness>, settings: &super::store::Settings, id: &str) -> bool {
    if id.starts_with("route/")
        || settings
            .disabled_models
            .iter()
            .any(|disabled| disabled == id)
        || settings.removed_models.iter().any(|removed| removed == id)
    {
        return false;
    }
    let info = model_info(settings, id);
    if !info.enabled || info.provider.is_empty() {
        return false;
    }
    let provider = &info.provider;
    let configured = providers::provider(provider).is_some()
        || settings
            .custom_providers
            .iter()
            .any(|custom| custom.id.as_str() == provider);
    if !configured
        || settings
            .providers
            .get(provider)
            .is_some_and(|config| !config.enabled)
    {
        return false;
    }

    if providers::is_pool(provider) {
        let pool = accounts::pool(h, &settings.accounts, provider);
        if pool.iter().any(|account| h.accts.usable(account)) {
            return true;
        }
        // Go can be configured as a key provider as well as an account pool;
        // router::targets only uses that key when no Go account is configured.
        return providers::key_login(provider)
            && !settings
                .accounts
                .iter()
                .any(|account| account.kind.as_str() == provider)
            && has_key(settings, provider, h);
    }

    if providers::provider(provider).is_some_and(|info| info.local) {
        return settings.providers.contains_key(provider);
    }
    if settings
        .custom_providers
        .iter()
        .any(|custom| custom.id.as_str() == provider)
    {
        return !providers::base_url(settings, provider).is_empty();
    }
    has_key(settings, provider, h)
}

fn has_key(settings: &super::store::Settings, provider: &str, h: &Arc<super::Harness>) -> bool {
    if providers::key_pool(settings, provider) {
        return settings.providers.get(provider).is_some_and(|config| {
            config
                .api_keys
                .iter()
                .any(|key| !key.trim().is_empty() && h.keys.usable(provider, key))
        });
    }
    !providers::api_key(settings, provider).is_empty()
}

fn weak_model(model: &providers::ModelInfo) -> bool {
    let id = model.id.to_ascii_lowercase();
    let name = model.name.to_ascii_lowercase();
    ["mini", "nano", "haiku", "flash", "lite", "small", "fast"]
        .iter()
        .any(|marker| id.contains(marker) || name.contains(marker))
}

fn cheaper_or_weaker(candidate: &providers::ModelInfo, base: &providers::ModelInfo) -> bool {
    let candidate_price = candidate.input_price + candidate.output_price;
    let base_price = base.input_price + base.output_price;
    candidate_price < base_price
        || (candidate_price <= base_price && weak_model(candidate) && !weak_model(base))
}

/// Keep commit-message spend in the same monthly budget ledger as agent turns.
/// The router already recorded the detailed sample through `Stats::record`.
fn record_spend(
    h: &super::Harness,
    cost: f64,
    tokens: u64,
) -> impl std::future::Future<Output = ()> + '_ {
    async move {
        let month = {
            let mut settings = h.settings.write().await;
            let day = chrono::Local::now().format("%Y-%m-%d").to_string();
            *settings.spend.entry(day).or_insert(0.0) += cost;
            settings.tokens_month += tokens;
            settings.month_spend()
        };
        h.save_settings_debounced().await;
        let tokens = h.settings.read().await.tokens_month;
        (h.bus)(
            "ol://usage",
            serde_json::json!({ "month": month, "tokens": tokens }),
        );
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn request_has_only_the_commit_prompt_and_bounded_output() {
        let req = request("system instructions", "diff summary", "task-123");

        assert_eq!(req.system, "system instructions");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content[0]["text"], "diff summary");
        assert!(req.tools.is_empty());
        assert_eq!(req.effort, 4);
        assert_eq!(req.max_tokens, 512);
        assert_eq!(req.cache_key, "ol-commitmsg-task-123");
    }

    #[test]
    fn a_named_weak_sibling_is_preferred_to_an_equal_price_strong_model() {
        let mut base = providers::model_info("claude/base");
        base.provider = "test-provider".into();
        base.input_price = 0.0;
        base.output_price = 0.0;
        let mut weak = providers::model_info("claude/haiku");
        weak.provider = "test-provider".into();
        weak.input_price = 0.0;
        weak.output_price = 0.0;

        assert!(cheaper_or_weaker(&weak, &base));
        assert!(!cheaper_or_weaker(&base, &weak));
    }
}

/// The user turn: a bounded rendering of the change.
pub fn user_prompt(files: &[git::FileDiff]) -> String {
    let mut out = String::from("Changed files:\n");
    for f in files.iter().take(MAX_PROMPT_FILES) {
        out.push_str(&format!(
            " {} {} (+{}/-{})\n",
            f.status, f.path, f.add, f.del
        ));
    }
    if files.len() > MAX_PROMPT_FILES {
        out.push_str(&format!(" … and {} more\n", files.len() - MAX_PROMPT_FILES));
    }
    out.push_str("\nDiff:\n");
    let mut lines_left = MAX_PROMPT_LINES;
    let mut chars_left = MAX_PROMPT_CHARS.saturating_sub(out.chars().count());
    'outer: for f in files.iter().take(MAX_PROMPT_FILES) {
        out.push_str(&format!("--- {} ---\n", f.path));
        for l in &f.lines {
            if lines_left == 0 || chars_left == 0 {
                out.push_str("… (truncated)\n");
                break 'outer;
            }
            let t = l["t"].as_str().unwrap_or("");
            let prefix = match l["k"].as_str().unwrap_or("c") {
                "a" => "+",
                "d" => "-",
                "h" => "",
                _ => " ",
            };
            let line = format!("{prefix}{t}\n");
            chars_left = chars_left.saturating_sub(line.chars().count());
            out.push_str(&line);
            lines_left -= 1;
        }
    }
    out
}

// ───────────────────────── text helpers ─────────────────────────

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Collapse runs of whitespace to one space and trim. A subject line arriving
/// with a hard-wrapped or doubled space is a model artifact, not intent.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut to `max` characters, preferring a word boundary in the second half so
/// the result reads as a phrase rather than a truncation.
fn cut_at_word(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let hard: String = chars[..max].iter().collect();
    match hard.rfind(' ') {
        Some(i) if i >= max / 2 => hard[..i].trim_end().to_string(),
        _ => hard.trim_end().to_string(),
    }
}

/// Whether a line looks like a Conventional Commits header.
pub fn is_conventional(line: &str) -> bool {
    let s = line.trim();
    let Some((ty, _)) = s.split_once(':') else {
        return false;
    };
    let ty = ty.trim_end_matches('!');
    let base = ty.split('(').next().unwrap_or(ty).trim();
    TYPES.contains(&base)
}

/// A leading line that introduces the message rather than being it. Applied
/// only to the top of the reply, and never to something that is itself a valid
/// header (a subject like "feat: rework the commit message" mentions the
/// phrase and must not be eaten by this).
fn is_preamble(line: &str) -> bool {
    if is_conventional(line) {
        return false;
    }
    let l = line.trim().to_ascii_lowercase();
    l.contains("commit message")
        || l.contains("here is your")
        || l.contains("here's your")
        || l.starts_with("sure,")
        || l.starts_with("sure!")
        || l.starts_with("sure ")
}

/// A trailing line of the model talking to the user rather than the commit.
fn is_chatter(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    l.starts_with("i hope")
        || l.starts_with("hope this")
        || l.starts_with("hope that")
        || l.starts_with("let me know")
        || l.starts_with("please let me know")
        || l.starts_with("feel free")
        || l.starts_with("this commit message")
}

/// The body of the first fenced code block, if the reply has one. `Some("")`
/// when a fence was opened and held nothing: the model gave us no message, and
/// the caller must fall back rather than commit a stray fence.
fn first_fence(text: &str) -> Option<String> {
    let mut found = false;
    let mut in_fence = false;
    let mut out: Vec<&str> = vec![];
    for l in text.lines() {
        let is_fence = l.trim_start().starts_with("```");
        if !in_fence {
            if is_fence && !found {
                in_fence = true;
                found = true;
            }
            continue;
        }
        if is_fence {
            break;
        }
        out.push(l);
    }
    found.then(|| out.join("\n"))
}
