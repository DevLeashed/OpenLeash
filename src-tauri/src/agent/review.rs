//! Reviewer approvals: a second, cheap model answers the permission prompt in
//! the user's place.
//!
//! Turned on by the `reviewer` permission id ([`permissions::REVIEW_PERM`]), and
//! consulted from `Runner::gate` — the only place with both the `Harness` and an
//! async context, and the only place a `Decision::Ask` exists to replace.
//!
//! **It replaces the click, nothing else.** `permissions::check` still runs
//! first, so the deny tier, the read-only shortcut, allow rules, plan mode and
//! the chaining guard all decide before this module is reached. A reviewer
//! `Allow` is a one-shot answer for one call: no allow rule is written, nothing
//! is cached across calls, and `matches_rule` is never consulted — which is what
//! keeps a chained command's protection from eroding over time.
//!
//! **Fail closed is the whole design.** An unreachable model, a timeout, a
//! provider error, prose instead of a verdict, or a cancelled run all mean "no
//! answer", and no answer is not an approval. The failure mode of a broken
//! reviewer is *more* prompting, never an unreviewed action.

use super::{commitmsg, providers, router, Harness, Message};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The reviewer's standing instructions.
///
/// Doubles as the marker tests look for to prove the classifier was actually
/// consulted, rather than the command having been allowed by some other path.
pub const SYSTEM: &str = "\
You are the security reviewer for a coding agent's permission gate. The agent normally stops and asks its user to approve each action that is not already allowed; you are answering in the user's place, so approve only what you would be comfortable watching a competent developer run in this project.

Answer with exactly one line and nothing else — either:

ALLOW

or:

DENY: <one sentence saying what makes it risky>

Judge the single action you are shown, not the plan behind it and not the agent's intentions.
Approve ordinary development work: builds, tests, linters, formatters, dependency installs for this project, git reads, local commits, and edits inside the project.
Refuse anything destructive, irreversible, or reaching outside the project: deleting or overwriting files the agent did not create, force-pushing, history rewrites, `sudo`, changes to system or user configuration, credential or secret access, piping a download into a shell, or anything that sends project data to a third party.
Refuse when you cannot tell what the action does.";

/// What the reviewer decided.
///
/// Anything that is not a clear allow falls back to asking the user, so this
/// enum has no "unknown" variant to forget about: ambiguity is [`Verdict::Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask,
    Deny(String),
}

/// The single user turn describing the action under review.
pub fn user_prompt(tool: &str, input: &Value, cwd: &str, project: &str) -> String {
    // A shell command is the case this exists for, and quoting it as JSON would
    // bury it in escaping; everything else is small enough to show as-is.
    let detail = match tool {
        "bash" | "shell" => input["command"].as_str().unwrap_or("").to_string(),
        // The reviewer judges the request, not the credentials it carries.
        "http_request" => super::http_tool::redact_input(input).to_string(),
        _ => serde_json::to_string(input).unwrap_or_else(|_| input.to_string()),
    };
    format!("Tool: {tool}\nWorking directory: {cwd}\nProject root: {project}\n\n{detail}")
}

/// Read the reviewer's reply.
///
/// Deliberately strict: this decides whether an action runs with no human in the
/// loop, so only an explicit verdict on its own line counts. A whole line is
/// required — matching the word anywhere would let "I would not ALLOW this"
/// read as an approval — and anything unrecognised is `Ask`, never `Allow`.
pub fn parse(text: &str) -> Verdict {
    for raw in text.lines() {
        // Models reach for code fences and bullets even when told not to; those
        // are formatting, not hedging, so they are stripped before matching.
        let line = raw
            .trim()
            .trim_matches('`')
            .trim_start_matches(['-', '*', '>'])
            .trim();
        let upper = line.to_ascii_uppercase();
        if upper == "ALLOW" || upper == "ALLOW." {
            return Verdict::Allow;
        }
        if let Some(rest) = upper.strip_prefix("DENY") {
            // `to_ascii_uppercase` preserves byte length, so the offset into the
            // uppercased copy indexes the same place in the original — which is
            // how the reason keeps the model's own wording and casing.
            let rest = rest.trim_start_matches([':', '-', ' ', '.']);
            let reason = line[line.len() - rest.len()..].trim();
            return Verdict::Deny(if reason.is_empty() {
                "the reviewer refused this action without giving a reason".to_string()
            } else {
                reason.to_string()
            });
        }
    }
    Verdict::Ask
}

/// Ask the reviewer about one action. Never returns `Allow` unless the model
/// clearly said so.
pub async fn classify(
    h: &Arc<Harness>,
    task_id: &str,
    cancel: &CancellationToken,
    tool: &str,
    input: &Value,
    cwd: &str,
    project: &str,
) -> Verdict {
    // Reviews are serialized per task: see `Runtime::review`. The `Arc` is bound
    // to a local because the guard borrows through it, and a guard outliving the
    // `Arc` temporary would not compile.
    let rt = h.runtime(task_id);
    let _turn = rt.review.lock().await;

    let settings = h.settings.read().await.clone();
    let Ok(task) = h.task(task_id).await else {
        return Verdict::Ask;
    };
    let (task_model, route_id) = {
        let t = task.lock().await;
        (t.model.clone(), t.route.clone())
    };
    // Nothing connected to ask: fall back to the human rather than guess.
    let Some(model) = commitmsg::choose_model(h, &settings, &task_model, &route_id) else {
        return Verdict::Ask;
    };

    let req = providers::ChatRequest {
        system: SYSTEM.to_string(),
        messages: vec![Message::user_text(user_prompt(tool, input, cwd, project))],
        // A reviewer that could call tools would be a second agent with its own
        // view of the permission gate — which is the one thing this feature must
        // not create.
        tools: vec![],
        // Effort 1 and a short cap: this is a classification, not a conversation.
        effort: 1,
        max_tokens: 200,
        cache_key: format!("ol-review-{task_id}"),
        cwd: String::new(),
    };
    let out = router::oneshot(
        h,
        router::Who {
            task_id,
            sub: None,
            side: true,
            turn: false,
            usage_id: "permission-review",
        },
        &model,
        &req,
        cancel,
    )
    .await;

    match out {
        Ok((text, usage, target)) => {
            let cost = usage.cost(&providers::model_info(&target.model_id));
            let tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
            super::record_side_spend(h, cost, tokens).await;
            h.update_task(task_id, |t| {
                t.usage.input += usage.input;
                t.usage.output += usage.output;
                t.usage.cache_read += usage.cache_read;
                t.usage.cache_write += usage.cache_write;
                t.usage.cost += cost;
            })
            .await;
            parse(&text)
        }
        // Cancelled, every model out, a provider error, a reply that never
        // arrived: all of these are "no answer", and no answer is not an allow.
        Err(_) => Verdict::Ask,
    }
}
