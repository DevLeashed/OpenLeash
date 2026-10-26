//! Request routing: turns "the model this agent uses" into a working response.
//!
//! A model id is one of
//! - `provider/model`          one API endpoint
//! - `codex/…` / `claude/…`    a pool: every connected subscription account, by priority
//! - `route/<id>`              a user-defined chain of the above
//!
//! Each target is retried per its failure class (transient noise gets
//! backoff, "insist" providers get dozens of tries, exhausted accounts are
//! skipped until their reset). When the whole chain is out, the task
//! **pauses** — main agent and every subagent — and the request resumes
//! exactly where it was once the user (or an account reset) unpauses it.
//! The model never hears about any of this.

use super::accounts::{self, now};
use super::providers::{self, ChatRequest, Fail, StreamEvent, Target, Turn};
use super::routerules;
use super::store::{Route, Settings};
use super::{Harness, Item, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

static ROUTES: std::sync::RwLock<Vec<Route>> = std::sync::RwLock::new(Vec::new());

/// The route table is process-global, so tests that set it take turns — from
/// either test module, which is why the lock lives here rather than next to one
/// of them.
#[cfg(test)]
pub static ROUTES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Publish the route table. Rules are normalized on the way in, so every entry
/// point (app, headless, ACP, an import) gets unique rule ids.
pub fn set_routes(r: &[Route]) {
    let mut routes = r.to_vec();
    normalize_routes(&mut routes);
    *ROUTES.write().unwrap() = routes;
}

/// A route by id (`abc`) or legacy model-style id (`route/abc`).
pub fn route(id: &str) -> Option<Route> {
    let rid = id.strip_prefix("route/").unwrap_or(id);
    if rid.is_empty() {
        return None;
    }
    ROUTES.read().unwrap().iter().find(|r| r.id == rid).cloned()
}

/// The route a model uses unless the user picks another: a route that names
/// this model as a head wins over an "all models" route; otherwise none.
pub fn default_route(model: &str) -> String {
    let routes = ROUTES.read().unwrap();
    routes
        .iter()
        .find(|r| r.heads.iter().any(|h| h == model))
        .or_else(|| routes.iter().find(|r| r.all))
        .map(|r| r.id.clone())
        .unwrap_or_default()
}

/// Ordered model ids to try: the chosen model first, then the route's
/// models in order (skipping the one already tried). Plus what to do when
/// all are out (pause | fail).
pub fn steps(model: &str, route_id: &str) -> (Vec<String>, String) {
    // Legacy: a whole route picked as the "model".
    if let Some(r) = model.strip_prefix("route/").and_then(|_| route(model)) {
        return (
            r.steps.clone(),
            if r.on_exhausted == "fail" {
                "fail".into()
            } else {
                "pause".into()
            },
        );
    }
    let mut chain = vec![model.to_string()];
    match route(route_id) {
        Some(r) => {
            chain.extend(r.steps.iter().filter(|s| *s != model).cloned());
            (
                chain,
                if r.on_exhausted == "fail" {
                    "fail".into()
                } else {
                    "pause".into()
                },
            )
        }
        None => (chain, "pause".into()),
    }
}

/// Smallest window across a chain, so compaction keeps every fallback usable.
///
/// Rule targets are deliberately not part of this: see
/// `effective_context_window`.
pub fn context_window(model: &str, route_id: &str) -> u64 {
    let (s, _) = steps(model, route_id);
    chain_window(&s)
}

pub fn max_output(model: &str, route_id: &str) -> u64 {
    let (s, _) = steps(model, route_id);
    chain_output(&s)
}

fn chain_output(chain: &[String]) -> u64 {
    chain
        .iter()
        .map(|m| providers::model_info(m).output)
        .min()
        .unwrap_or(32_000)
}

fn chain_window(chain: &[String]) -> u64 {
    chain
        .iter()
        .map(|m| providers::model_info(m).context)
        .min()
        .unwrap_or(128_000)
        .max(8_000)
}

// ───────────────────────────── router rules ─────────────────────────────
//
// `routerules` is the pure engine; this is where it meets a live request:
// the situation is read from the task, the runtime and the settings, the
// decision picks the model the fallback chain starts from, and the outcome is
// remembered per agent (`Runtime::router`) and shown in the transcript.

/// Newest entries kept in `Task::route_log`.
pub const ROUTE_LOG_CAP: usize = 500;

/// One agent's live router state. Kept in `Runtime::router`, so it is never
/// persisted: holds and stuck counters reset when the app restarts.
#[derive(Debug, Clone, Default)]
pub struct AgentRoute {
    pub signals: routerules::Signals,
    pub hold: Option<routerules::Hold>,
    /// The (model, effort) the router last moved this agent to; None = it is
    /// on its base model and effort. What compaction sizes its window from,
    /// and what a transcript notice is only written on a change of.
    pub last: Option<(String, usize)>,
}

/// One switch the router made, in a chat's audit log (`Task::route_log`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct RouteEvent {
    /// Unix seconds.
    pub at: i64,
    /// "main" or the subagent id.
    pub agent: String,
    /// "main", the subagent type, or a system kind ("compactor", …).
    pub agent_kind: String,
    pub from: String,
    pub to: String,
    pub effort_from: usize,
    pub effort_to: usize,
    pub action: String,
    pub rule_id: String,
    pub rule_name: String,
    pub reason: String,
}

/// Router-state key of an agent's own turns: "main" or the subagent id.
pub fn agent_key(sub: Option<&str>) -> String {
    sub.unwrap_or("main").to_string()
}

/// Router-state key for a request. One-shots get their own key so a routed
/// compaction never reads as the agent itself having moved (which would also
/// resize the agent's compaction window).
fn state_key(who: &Who<'_>) -> String {
    if who.turn {
        agent_key(who.sub)
    } else {
        format!("sys:{}:{}", who.usage_id, who.sub.unwrap_or("main"))
    }
}

/// Successful edits are what "progress" means to the `no_progress` condition.
fn is_edit_tool(name: &str) -> bool {
    matches!(name, "edit_file" | "multi_edit" | "write_file")
}

/// Feed one tool round's outcomes `(tool, input, is_error)` into an agent's
/// stuck signals. Called by the agent loop after every round, whether or not
/// any route has rules: the counters are cheap, and a rule added mid-run then
/// sees the run's real state.
pub fn record_round<'a>(
    rt: &super::Runtime,
    agent: &str,
    calls: impl IntoIterator<Item = (&'a str, &'a serde_json::Value, bool)>,
) {
    let mut map = rt.router.lock().unwrap();
    let st = map.entry(agent.to_string()).or_default();
    let mut progress = false;
    for (name, input, is_err) in calls {
        let p = !is_err && is_edit_tool(name);
        progress |= p;
        st.signals.record(name, input, is_err, p);
    }
    st.signals.round(progress);
}

/// The route whose rules apply to a request. A legacy `route/<id>` model is
/// its own route (that is also the chain `steps` uses for it), otherwise the
/// task's route. None when there is no such route or it has no rules — the
/// router then behaves exactly as it did before rules existed.
pub fn rules_route(route_id: &str, base: &str) -> Option<Route> {
    let r = if base.starts_with("route/") {
        route(base)
    } else {
        route(route_id)
    };
    r.filter(|r| !r.rules.is_empty())
}

/// The fallback chain for a routed request: the routed model first, then the
/// base model's own chain, then the route's remaining steps. Keep the base
/// model explicitly: the route's `steps` list normally contains fallbacks,
/// not the model that the user picked. A routed model that is unavailable must
/// still fall back to the model the agent would otherwise have called.
pub fn routed_steps(routed: &str, base: &str, route_id: &str) -> (Vec<String>, String) {
    if routed == base {
        return steps(base, route_id);
    }
    if base.starts_with("route/") {
        if let Some(r) = route(base) {
            let mut chain = vec![routed.to_string()];
            chain.extend(r.steps.iter().filter(|s| *s != routed).cloned());
            let out = if r.on_exhausted == "fail" {
                "fail"
            } else {
                "pause"
            };
            return (chain, out.into());
        }
    }
    let mut chain = vec![routed.to_string()];
    if !base.is_empty() && base != routed {
        chain.push(base.to_string());
    }
    match route(route_id) {
        Some(r) => {
            chain.extend(
                r.steps
                    .iter()
                    .filter(|s| *s != routed && *s != base)
                    .cloned(),
            );
            (
                chain,
                if r.on_exhausted == "fail" {
                    "fail".into()
                } else {
                    "pause".into()
                },
            )
        }
        None => (chain, "pause".into()),
    }
}

/// Repair every route's rules (missing / duplicate ids, unset modes) so the
/// editor, the log and the dry-run can key on rule ids. Returns whether
/// anything changed.
pub fn normalize_routes(routes: &mut [Route]) -> bool {
    let mut changed = false;
    for r in routes {
        changed |= routerules::normalize(&mut r.rules);
    }
    changed
}

/// The model the router last moved this agent to, if it is routed right now.
fn routed_model(h: &Harness, task_id: &str, agent: &str) -> Option<String> {
    let rt = h.runtime_if(task_id)?;
    let map = rt.router.lock().unwrap();
    map.get(agent)?.last.as_ref().map(|(m, _)| m.clone())
}

/// The context window compaction should respect for this agent: the routed
/// model's chain while a rule or hold has moved it, else the base chain.
///
/// Folding rule targets into `context_window` instead would size every chat
/// by its smallest *possible* model, and the "big context → long-context
/// model" preset would compact at the very size it exists to get past.
pub fn effective_context_window(
    h: &Harness,
    task_id: &str,
    agent: &str,
    model: &str,
    route_id: &str,
) -> u64 {
    match routed_model(h, task_id, agent) {
        Some(m) if m != model && rules_route(route_id, model).is_some() => {
            // Context sizing follows the routed model's configured fallback
            // chain, not the base model inserted as a last-resort retry by
            // `routed_steps`; otherwise even a long-context target is sized
            // back down to the base model's window.
            chain_window(&steps(&m, route_id).0)
        }
        _ => context_window(model, route_id),
    }
}

/// Preview the chain this request would use before deciding to compact. Unlike
/// `effective_context_window`, this evaluates current rules (including a rule
/// triggered for the first time by this very prompt). Evaluation is read-only:
/// no holds are spent, counters consumed, or switches announced until request().
pub async fn prospective_context_window(
    h: &Arc<Harness>,
    who: &Who<'_>,
    model: &str,
    route_id: &str,
    req: &ChatRequest,
) -> u64 {
    let Some(r) = rules_route(route_id, model).filter(|_| who.usage_id != "keepalive") else {
        return context_window(model, route_id);
    };
    let s = live_situation(h, who, model, req.effort, route_id, req).await;
    let d = routerules::evaluate(&r.rules, &r.ladder, &s);
    // The prospective context guard is choosing whether this model can avoid
    // compaction; the base model's last-resort retry must not drag the window
    // back down before the new long-context target gets its first request.
    chain_window(&steps(&d.model, route_id).0)
}

/// `max_output`, by the same reasoning as `effective_context_window`. The
/// router clamps a request to the chain it actually starts, so a turn that
/// moves back to a smaller model is never sent a too-large `max_tokens`.
pub fn effective_max_output(
    h: &Harness,
    task_id: &str,
    agent: &str,
    model: &str,
    route_id: &str,
) -> u64 {
    match routed_model(h, task_id, agent) {
        Some(m) if m != model && rules_route(route_id, model).is_some() => {
            chain_output(&routed_steps(&m, model, route_id).0)
        }
        _ => max_output(model, route_id),
    }
}

/// Rough prompt size of a request, in tokens (the same heuristic as the
/// context ring's breakdown).
pub fn estimate_tokens(system: &str, tools: &[serde_json::Value], messages: &[Message]) -> u64 {
    let p = super::runner::context_parts(system, tools, messages);
    p.system + p.instructions + p.skills + p.tools + p.attachments + p.history + p.summary
}

/// Whether any message carries an image block.
pub fn has_images(messages: &[Message]) -> bool {
    messages
        .iter()
        .any(|m| m.content.iter().any(|b| b["type"] == "image"))
}

/// Remove `<system-reminder>…</system-reminder>` sections (harness notes, not
/// the user's words). An unclosed tag drops the rest of the text.
fn strip_reminders(s: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        match rest[i..].find(CLOSE) {
            Some(j) => rest = &rest[i + j + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of the latest genuine user message: the newest user turn with
/// non-empty text once harness reminders are stripped. A user turn that
/// carries tool results is a tool round — its text blocks are the harness's
/// loop / diagnostics notes — so it is skipped.
pub fn latest_user_text(messages: &[Message]) -> String {
    for m in messages.iter().rev() {
        if m.role != "user" || m.content.iter().any(|b| b["type"] == "tool_result") {
            continue;
        }
        let text = m
            .content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .map(strip_reminders)
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return text.trim().to_string();
        }
    }
    String::new()
}

/// Pool kind → percent left on its best usable account (100 − the fullest
/// reported window). Accounts that are off, cooling down, or have reported no
/// usage yet are left out, so "no usage reported" stays distinguishable.
pub fn headroom(views: &[accounts::AccountView]) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for v in views.iter().filter(|v| v.enabled && v.available) {
        let Some(u) = &v.usage else { continue };
        if u.windows.is_empty() {
            continue;
        }
        let used = u.windows.iter().map(|w| w.used).fold(0.0_f64, f64::max);
        let left = (100.0 - used).clamp(0.0, 100.0);
        let e = out.entry(v.kind.clone()).or_insert(left);
        if left > *e {
            *e = left;
        }
    }
    out
}

/// Everything about a request that is not read from its messages.
pub struct SituationInput<'a> {
    pub agent: String,
    pub sub: bool,
    pub turn: bool,
    pub base_model: &'a str,
    pub base_effort: usize,
    pub route_id: &'a str,
    pub plan: bool,
    pub context_tokens: u64,
    pub signals: &'a routerules::Signals,
    pub hold: Option<routerules::Hold>,
    pub headroom: BTreeMap<String, f64>,
    pub budget_used_pct: Option<f64>,
    pub now: chrono::DateTime<chrono::Local>,
}

/// The base model's window as a rule should see it. A legacy `route/<id>`
/// "model" has no catalog entry of its own, so it is its chain's window.
fn base_context(base: &str, route_id: &str) -> u64 {
    if base.starts_with("route/") {
        context_window(base, route_id)
    } else {
        providers::model_info(base).context
    }
}

/// Build a [`routerules::Situation`]. Pure: the live and the "load from this
/// chat" callers gather the inputs, this assembles them.
pub fn situation(i: SituationInput<'_>, messages: &[Message]) -> routerules::Situation {
    use chrono::{Datelike, Timelike};
    let (repeat_failures, failing_tool) = i.signals.worst();
    routerules::Situation {
        agent: i.agent,
        sub: i.sub,
        turn: i.turn,
        base_model: i.base_model.to_string(),
        base_effort: i.base_effort,
        base_context: base_context(i.base_model, i.route_id),
        plan: i.plan,
        images: has_images(messages),
        context_tokens: i.context_tokens,
        user_text: latest_user_text(messages),
        repeat_failures,
        failing_tool,
        errors_in_row: i.signals.errors_in_row,
        last_error_tool: i.signals.last_error_tool.clone(),
        no_progress_turns: i.signals.no_progress_turns,
        turns: i.signals.turns,
        headroom: i.headroom,
        budget_used_pct: i.budget_used_pct,
        minute_of_day: i.now.hour() * 60 + i.now.minute(),
        weekday: i.now.weekday().num_days_from_monday() as u8,
        hold: i.hold,
    }
}

fn budget_pct(s: &Settings) -> Option<f64> {
    (s.budget > 0.0).then(|| s.month_spend() / s.budget * 100.0)
}

/// An agent's signals and hold, as they stand before this request.
fn agent_state(
    h: &Harness,
    task_id: &str,
    key: &str,
) -> (routerules::Signals, Option<routerules::Hold>) {
    match h.runtime_if(task_id) {
        Some(rt) => {
            let map = rt.router.lock().unwrap();
            map.get(key)
                .map(|s| (s.signals.clone(), s.hold.clone()))
                .unwrap_or_default()
        }
        None => Default::default(),
    }
}

/// The situation of a live request.
async fn live_situation(
    h: &Arc<Harness>,
    who: &Who<'_>,
    base: &str,
    effort: usize,
    route_id: &str,
    req: &ChatRequest,
) -> routerules::Situation {
    let plan = match h.task(who.task_id).await {
        Ok(t) => t.lock().await.plan,
        Err(_) => false,
    };
    // A one-shot is named by what it is ("compactor", "title", …), even when it
    // runs for a subagent: a rule naming "explore" means the explorer's turns.
    let agent = if who.turn {
        agent_kind(h, who).await
    } else {
        who.usage_id.to_string()
    };
    let (signals, hold) = if who.turn {
        agent_state(h, who.task_id, &agent_key(who.sub))
    } else {
        Default::default()
    };
    let settings = h.settings.read().await;
    let input = SituationInput {
        agent,
        sub: who.sub.is_some(),
        turn: who.turn,
        base_model: base,
        base_effort: effort,
        route_id,
        plan,
        context_tokens: estimate_tokens(&req.system, &req.tools, &req.messages),
        signals: &signals,
        hold,
        headroom: headroom(&h.accts.views(&settings.accounts)),
        budget_used_pct: budget_pct(&settings),
        now: chrono::Local::now(),
    };
    drop(settings);
    situation(input, &req.messages)
}

/// The live situation of a chat's main agent (or one subagent), for the route
/// editor's "load from this chat". Best effort: the context size is the last
/// measured prompt where there is one, otherwise an estimate from history.
pub async fn situation_for_task(
    h: &Arc<Harness>,
    task_id: &str,
    agent: Option<&str>,
) -> Result<routerules::Situation, String> {
    let agent = agent.filter(|a| !a.is_empty() && *a != "main");
    // Settings first and released before the task lock is taken, so this
    // never holds one lock while waiting on the other.
    let settings = h.settings.read().await.clone();
    let (headroom, budget_used_pct) = {
        (
            headroom(&h.accts.views(&settings.accounts)),
            budget_pct(&settings),
        )
    };
    let t = h.task(task_id).await?;
    let t = t.lock().await;
    let sub = match agent {
        Some(id) => Some(
            t.subs
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("no subagent {id} in this chat"))?,
        ),
        None => None,
    };
    let base = super::resolve_model(&t, sub, sub.map(|s| s.depth).unwrap_or(0), &t.model);
    let effort = effort_for_agent(&settings, &t, sub);
    let empty = vec![];
    let messages: &[Message] = match sub {
        Some(s) => t.sub_msgs.get(&s.id).unwrap_or(&empty),
        None => &t.messages,
    };
    let context_tokens = match sub {
        None if t.usage.last_context > 0 => t.usage.last_context,
        _ => estimate_tokens(&t.system, &t.mcp_tools, messages),
    };
    let key = agent_key(agent);
    let (signals, hold) = agent_state(h, task_id, &key);
    let input = SituationInput {
        agent: sub.map(|s| s.role.clone()).unwrap_or_else(|| "main".into()),
        sub: sub.is_some(),
        turn: true,
        base_model: &base,
        base_effort: effort,
        route_id: &t.route,
        plan: t.plan,
        context_tokens,
        signals: &signals,
        hold,
        headroom,
        budget_used_pct,
        now: chrono::Local::now(),
    };
    Ok(situation(input, messages))
}

/// What the route editor's dry-run shows: the exact `evaluate` a live request
/// runs, on rules repaired the way loading repairs them.
pub fn dry_run(route: &Route, s: &routerules::Situation) -> routerules::Decision {
    let mut rules = route.rules.clone();
    routerules::normalize(&mut rules);
    routerules::evaluate(&rules, &route.ladder, s)
}

/// A model's human name, for notices.
fn model_name(m: &str) -> String {
    let n = providers::model_info(m).name;
    if n.is_empty() {
        m.to_string()
    } else {
        n
    }
}

const EFFORTS: [&str; 5] = ["Max", "Extra High", "High", "Medium", "Low"];

fn effort_name(e: usize) -> &'static str {
    EFFORTS[e.min(4)]
}

/// The transcript line for a switch. `now` = None means back on the base.
fn notice_text(
    s: &routerules::Situation,
    d: &routerules::Decision,
    now: Option<&(String, usize)>,
) -> String {
    let reason = if d.reason.is_empty() {
        "no rule applies".to_string()
    } else {
        d.reason.clone()
    };
    let Some((model, effort)) = now else {
        return format!("Router: back to {} — {reason}", model_name(&s.base_model));
    };
    if *model == s.base_model {
        return format!("Router: effort → {} — {reason}", effort_name(*effort));
    }
    let with_effort = if *effort != s.base_effort {
        format!(" · {} effort", effort_name(*effort))
    } else {
        String::new()
    };
    let verb = match d.action.as_str() {
        "escalate" => "escalated to",
        "deescalate" => "stepped down to",
        _ => "switched to",
    };
    format!(
        "Router: {verb} {}{with_effort} — {reason}",
        model_name(model)
    )
}

/// Remember what this decision moved the agent to, and when that differs from
/// where it was, say so in the transcript and the chat's route log.
async fn announce(
    h: &Arc<Harness>,
    who: &Who<'_>,
    s: &routerules::Situation,
    d: &routerules::Decision,
) {
    let now = d.routed(s).then(|| (d.model.clone(), d.effort));
    let prev = {
        let rt = h.runtime(who.task_id);
        let mut map = rt.router.lock().unwrap();
        let e = map.entry(state_key(who)).or_default();
        std::mem::replace(&mut e.last, now.clone())
    };
    // Unchanged, or base → base (nothing ever moved): nothing to tell.
    if now == prev {
        return;
    }
    let (from, effort_from) = prev
        .clone()
        .unwrap_or_else(|| (s.base_model.clone(), s.base_effort));
    let (to, effort_to) = now
        .clone()
        .unwrap_or_else(|| (s.base_model.clone(), s.base_effort));
    let action = if now.is_none() && d.action != "revert" {
        // Back on the base because no rule picks anything now (a one-request
        // rule stopped matching) or a `stay` rule suspended a hold.
        if d.action == "stay" {
            "stay".to_string()
        } else {
            "base".to_string()
        }
    } else {
        d.action.clone()
    };
    let text = notice_text(s, d, now.as_ref());
    h.stats.event_in(who.task_id, "router_switches");
    let ev = RouteEvent {
        at: now_secs(),
        agent: agent_key(who.sub),
        agent_kind: s.agent.clone(),
        from: from.clone(),
        to: to.clone(),
        effort_from,
        effort_to,
        action: action.clone(),
        rule_id: d.rule_id.clone(),
        rule_name: d.rule_name.clone(),
        reason: d.reason.clone(),
    };
    if let Ok(t) = h.task(who.task_id).await {
        let mut t = t.lock().await;
        t.route_log.push(ev);
        let over = t.route_log.len().saturating_sub(ROUTE_LOG_CAP);
        if over > 0 {
            t.route_log.drain(..over);
        }
    }
    if who.side {
        return;
    }
    h.upsert_in(
        who.task_id,
        who.sub,
        Item::new(
            "notice",
            text,
            json!({
                "level": "router",
                "action": action,
                "from": from,
                "to": to,
                "effort_from": effort_from,
                "effort_to": effort_to,
                "rule": d.rule_name,
                "rule_id": d.rule_id,
                "reason": d.reason,
            }),
        ),
    )
    .await;
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A served turn spends its hold: store the hold the decision left behind, and
/// reset the stuck counters a stuck-style rule acted on. Only on success, so a
/// request that was cancelled, refreshed or failed — and is then built again
/// for the same turn — never counts a hold turn twice.
fn commit(h: &Harness, who: &Who<'_>, d: &routerules::Decision) {
    let rt = h.runtime(who.task_id);
    let mut map = rt.router.lock().unwrap();
    let e = map.entry(agent_key(who.sub)).or_default();
    e.hold = d.hold.clone();
    if d.consume_stuck {
        e.signals.consume();
    }
}

/// The route lost its rules (or the chat its route): drop a hold and the
/// "routed" mark, so neither outlives the rules that made them.
fn forget(h: &Harness, who: &Who<'_>) {
    if let Some(rt) = h.runtime_if(who.task_id) {
        if let Some(e) = rt.router.lock().unwrap().get_mut(&state_key(who)) {
            e.hold = None;
            e.last = None;
        }
    }
}

/// Clamp `max_tokens` to what every model of the chain can produce.
fn with_output_cap<'a>(req: Cow<'a, ChatRequest>, cap: u64) -> Cow<'a, ChatRequest> {
    if cap == 0 || req.max_tokens <= cap {
        return req;
    }
    match req {
        Cow::Owned(mut r) => {
            r.max_tokens = cap;
            Cow::Owned(r)
        }
        Cow::Borrowed(r) => Cow::Owned(ChatRequest {
            system: r.system.clone(),
            messages: r.messages.clone(),
            tools: r.tools.clone(),
            effort: r.effort,
            max_tokens: cap,
            cache_key: r.cache_key.clone(),
            cwd: r.cwd.clone(),
        }),
    }
}

pub enum RouteErr {
    Cancelled,
    /// A resume message landed while this request was frozen. The attempt is
    /// abandoned rather than retried: the request it holds was built from the
    /// history *before* that message, so replaying it would answer a turn the
    /// user has already spoken over. The caller re-reads history and builds a
    /// fresh request, which carries the new message.
    Refresh,
    TooLong(String),
    Fatal(String),
}

impl std::fmt::Debug for RouteErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Cancelled"),
            Self::Refresh => f.write_str("Refresh"),
            Self::TooLong(m) => write!(f, "TooLong({m})"),
            Self::Fatal(m) => write!(f, "Fatal({m})"),
        }
    }
}

/// Who is asking: the main agent or one subagent of a task.
pub struct Who<'a> {
    pub task_id: &'a str,
    pub sub: Option<&'a str>,
    /// A side request (e.g. a status summary): ignores the task's pause, never touches its
    /// step line or timeline, and fails instead of pausing the task when every model is out.
    pub side: bool,
    /// An agent's own turn, whose effort is the chat's and is re-read on every
    /// pass so a swap made while it waited reaches the resumed request.
    ///
    /// Everything else picks a level for itself and keeps it. Compaction is the
    /// one that matters: it floors its own effort (`.max(2)`) because a summary
    /// written at Low becomes the whole conversation's context afterwards, and
    /// re-reading the chat's level would quietly drop that floor.
    pub turn: bool,
    /// The identity this spend is attributed to, for the per-chat by-agent
    /// split: `"main"`, `"sub:<subagent id>"`, or a system label
    /// (`"compactor"`, `"title"`, `"keepalive"`, `"side"`, `"summary"`).
    ///
    /// Deliberately *not* derived from `sub` alone: compaction, titling and the
    /// keepalive all run with `sub: None` and would otherwise be recorded as the
    /// main agent, hiding their spend inside the task total — the exact thing
    /// this dimension exists to stop. An explicit id at each call site is the
    /// only way to tell them apart, and a call site that forgets one shows up as
    /// its own row in the stats panel rather than silently folded into "main".
    pub usage_id: &'a str,
}

async fn status(h: &Harness, who: &Who<'_>, text: String) {
    if who.side {
        return;
    }
    let sub = who.sub.map(String::from);
    h.update_task(who.task_id, |t| match &sub {
        Some(s) => {
            if let Some(x) = t.subs.iter_mut().find(|x| &x.id == s) {
                x.meta = text;
            }
        }
        None => t.step = text,
    })
    .await;
}

/// Take a retry line back down once the attempt it was about finally lands.
///
/// Only when it is still ours: a step the agent set in the meantime (a todo's
/// active form, a tool it is running) belongs to the run, not to the failure
/// that came before it. Without this a chat that blipped once — and then
/// answered — kept reading "<model>: … · retry 1/6 in 2s" for the rest of the
/// turn, which looks like a hang that isn't one.
async fn unstatus(h: &Harness, who: &Who<'_>, was: &str) {
    if who.side {
        return;
    }
    let sub = who.sub.map(String::from);
    h.update_task_quiet(who.task_id, |t| match &sub {
        Some(s) => {
            if let Some(x) = t.subs.iter_mut().find(|x| &x.id == s) {
                if x.meta == was {
                    x.meta.clear();
                }
            }
        }
        None => {
            if t.step == was {
                t.step.clear();
            }
        }
    })
    .await;
}

async fn notice(h: &Harness, who: &Who<'_>, text: String) {
    if who.side {
        return;
    }
    h.upsert_in(
        who.task_id,
        who.sub,
        Item::new("notice", text, json!({"level": "route"})),
    )
    .await;
}

/// Sleep, but stop early if the task gets paused or cancelled. False = cancelled.
async fn nap(h: &Harness, who: &Who<'_>, secs: u64, cancel: &CancellationToken) -> bool {
    let bell = h.pause_bell.notified();
    tokio::select! {
        _ = tokio::time::sleep(if cfg!(test) { std::time::Duration::from_millis(secs * 3) } else { std::time::Duration::from_secs(secs) }) => {},
        _ = bell => {},
        _ = cancel.cancelled() => return false,
    }
    who.side || h.wait_unpaused(who.task_id, cancel).await
}

fn backoff(tries: u32, insist: bool) -> u64 {
    if insist {
        (2 + tries as u64 / 4).min(8)
    } else {
        // saturating_sub: a caller that reports "0 tries so far" must not underflow
        // into a huge index (which would panic the index, not clamp).
        [2, 4, 8, 15, 30, 30][(tries as usize).saturating_sub(1).min(5)]
    }
}

/// The task's route, and the model this agent is on right now — the same reading
/// the agent loop does per turn, by the same resolver, so a parked request and
/// the loop that made it can never disagree about which model it is on. Falls
/// back to the caller's model when the chat is gone.
///
/// Side requests are left alone: they pick a model on purpose (a status summary on
/// a frozen prefix, the "which past chat is this" lookup on another chat), not
/// "whatever this chat is using".
async fn live(h: &Arc<Harness>, who: &Who<'_>, model: &str) -> (String, String) {
    let Ok(t) = h.task(who.task_id).await else {
        return (String::new(), model.to_string());
    };
    let t = t.lock().await;
    let m = if who.side {
        model.to_string()
    } else {
        // A subagent's own entry, for the model somebody chose for it and for the
        // depth its X layer is picked by. The main agent has neither.
        let sub = who.sub.and_then(|s| t.subs.iter().find(|x| x.id == s));
        super::resolve_model(&t, sub, sub.map(|s| s.depth).unwrap_or(0), model)
    };
    (
        t.route.clone(),
        if m.is_empty() { model.to_string() } else { m },
    )
}

fn targets(h: &Harness, s: &Settings, step: &str) -> Vec<Target> {
    let (prov, wire) = step.split_once('/').unwrap_or(("openai", step));
    let base = Target {
        model_id: step.to_string(),
        prov: prov.to_string(),
        model: wire.to_string(),
        account: None,
        api_key: None,
    };
    if providers::is_pool(prov) {
        let pool: Vec<Target> = accounts::pool(h, &s.accounts, prov)
            .into_iter()
            .filter(|a| h.accts.usable_for_model(a, step))
            .map(|a| Target {
                account: Some(a),
                ..base.clone()
            })
            .collect();
        // OpenCode Go is a pool provider *and* a key provider: it was usable with
        // `OPENCODE_API_KEY` before accounts existed. So only a key-login provider
        // with no account of this kind configured at all falls through to the key
        // path below — an account that is merely out of usage keeps the pool, and
        // its "every account is out" message, rather than silently retrying the
        // same key that the account already represents.
        let no_accounts = !s.accounts.iter().any(|a| a.kind == prov);
        if !pool.is_empty() || !providers::key_login(prov) || !no_accounts {
            return pool;
        }
    }
    if providers::key_pool(s, prov) {
        if let Some(cfg) = s.providers.get(prov) {
            return cfg
                .api_keys
                .iter()
                .filter(|k| !k.is_empty() && h.keys.usable(prov, k))
                .map(|k| Target {
                    api_key: Some(k.clone()),
                    ..base.clone()
                })
                .collect();
        }
    }
    vec![base]
}

/// The reasoning effort this agent is on right now, for the chat's main turn.
///
/// Read live, and re-read on every pass, for the same reason `live` re-reads the
/// model: `req` was built before the wait and is reused verbatim, so a chat parked
/// with every model out and then swapped would resume on the NEW model still
/// asking for the reasoning the OLD one was on. The stale model is visible in the
/// transcript; the stale reasoning is invisible and quietly wrong.
///
/// Only for the main turn. A one-shot request (`oneshot`) picks its own level on
/// purpose — a summary, a side question — and that level is the request's, not the
/// chat's, so it is left alone.
/// Resolve the current agent definition just as the runner does at launch.
/// Unknown/deleted roles inherit the chat; explicit per-sub overrides still win.
fn effort_for_agent(settings: &Settings, t: &super::Task, sub: Option<&super::SubInfo>) -> usize {
    let def = sub.and_then(|s| {
        super::store::all_agents(settings, &t.project)
            .into_iter()
            .find(|d| d.id == s.role)
    });
    super::resolve_effort(
        t,
        sub,
        def.as_ref().and_then(|d| d.effort),
        def.as_ref().is_some_and(|d| d.tools == "read_only"),
    )
}

async fn live_effort(h: &Arc<Harness>, who: &Who<'_>, fallback: usize) -> usize {
    if !who.turn {
        return fallback;
    }
    let settings = h.settings.read().await.clone();
    let Ok(t) = h.task(who.task_id).await else {
        return fallback;
    };
    let t = t.lock().await;
    let sub = who.sub.and_then(|sid| t.subs.iter().find(|s| s.id == sid));
    effort_for_agent(&settings, &t, sub)
}

/// The caller's request with one field replaced, without requiring `ChatRequest`
/// to be `Clone`: it holds the whole history, and the common case — the effort has
/// not moved — must not pay to copy it on every retry pass.
fn with_effort(req: &ChatRequest, effort: usize) -> Cow<'_, ChatRequest> {
    if effort == req.effort {
        return Cow::Borrowed(req);
    }
    Cow::Owned(ChatRequest {
        system: req.system.clone(),
        messages: req.messages.clone(),
        tools: req.tools.clone(),
        effort,
        max_tokens: req.max_tokens,
        cache_key: req.cache_key.clone(),
        cwd: req.cwd.clone(),
    })
}

/// The caller's request with any image over the provider's pixel limit shrunk
/// (or stubbed). One such image makes the provider reject the whole request with
/// a 400, and no retry or fallback can fix that. Borrowed in the normal case: the
/// check reads image headers only, and the history is copied only to change it.
fn with_fitted_images(req: &ChatRequest) -> Cow<'_, ChatRequest> {
    if !super::imagefit::needs_fit(&req.messages) {
        return Cow::Borrowed(req);
    }
    let mut messages = req.messages.clone();
    super::imagefit::fit_messages(&mut messages);
    Cow::Owned(ChatRequest {
        system: req.system.clone(),
        messages,
        tools: req.tools.clone(),
        effort: req.effort,
        max_tokens: req.max_tokens,
        cache_key: req.cache_key.clone(),
        cwd: req.cwd.clone(),
    })
}

/// Run one model request through the task's chain. Pauses (and waits) when everything is out.
pub async fn request(
    h: &Arc<Harness>,
    who: Who<'_>,
    model: &str,
    req: &ChatRequest,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &CancellationToken,
) -> Result<(Turn, Target), RouteErr> {
    // Every request passes here, including the one-shots (compaction, side
    // questions, keepalives) that send history without the main loop's repairs.
    let fitted = with_fitted_images(req);
    let req: &ChatRequest = &fitted;
    let mut first_label: Option<String> = None;
    let (mut route_id, mut chosen) = live(h, &who, model).await;
    let mut swapped_from: Option<String> = None;
    // The retry line this pass last put on the task, so a success can clear it.
    let mut retry_line: Option<String> = None;
    // Re-evaluate from the same pre-request signals/hold on every retry pass.
    // This refreshes time/headroom/budget after an exhausted pause without ever
    // spending a hold twice; `commit` runs only once a pass is served.
    let mut first_sit: Option<routerules::Situation> = None;
    let rules_on = who.usage_id != "keepalive";
    loop {
        if !who.side {
            if !h.wait_unpaused(who.task_id, cancel).await {
                return Err(RouteErr::Cancelled);
            }
            // The chat was lifted by a message rather than by a resume: the user
            // typed while it was frozen. The request we hold predates that text,
            // so answering it now means replying to a turn the user has already
            // spoken over. Hand control back for a fresh build, which includes it.
            if h.runtime(who.task_id).steered.load(Ordering::SeqCst) {
                return Err(RouteErr::Refresh);
            }
        }
        // The model is re-read per pass, exactly as the route is: a chat that paused
        // with every model out, and was swapped before the resume, retries the NEW
        // model instead of the dead one it was frozen on.
        let cur = live(h, &who, model).await;
        if cur.0 != route_id {
            route_id = cur.0;
        }
        if cur.1 != chosen {
            swapped_from = Some(chosen.clone());
            chosen = cur.1;
            // A chain that falls back on the way there announces itself with "Now
            // using …" once it lands, so only say it here when the chosen model
            // serves the request itself.
            first_label = None;
        }
        // The effort is re-read per pass, exactly as the model is: `req` was built
        // before the wait and is reused verbatim, so a chat parked with every
        // model out and then swapped would resume on the NEW model still asking
        // for the reasoning the OLD one was on. Sending the stale level is the
        // worse half of the bug — on a model with a different ladder it lands as
        // a level that does not exist, or as a much cheaper or dearer answer than
        // the user just asked for.
        let base_effort = live_effort(h, &who, req.effort).await;
        // `chosen` stays the BASE model throughout: the swap detection above
        // compares it with the live reading, so a routed model in its place
        // would read as a user swap and announce one that never happened.
        let rules = if rules_on {
            rules_route(&route_id, &chosen)
        } else {
            None
        };
        let decided = match rules {
            None => {
                // Cheap (a runtime lookup), and it keeps a hold or a
                // "routed" mark from outliving the rules that made it.
                if rules_on {
                    forget(h, &who);
                }
                None
            }
            Some(r) => {
                let s = live_situation(h, &who, &chosen, base_effort, &route_id, req).await;
                let s = if first_sit.as_ref().is_some_and(|first| {
                    first.base_model != chosen || first.base_effort != base_effort
                }) {
                    // A user-selected model/effort swap while paused starts from
                    // that choice, not a hold or an effort override from before it.
                    routerules::Situation {
                        hold: None,
                        base_model: chosen.clone(),
                        base_effort,
                        base_context: base_context(&chosen, &route_id),
                        ..s
                    }
                } else {
                    s
                };
                if first_sit.is_none() {
                    first_sit = Some(s.clone());
                }
                let d = routerules::evaluate(&r.rules, &r.ladder, &s);
                let consume_stuck = d.consume_stuck
                    && r.rules
                        .iter()
                        .find(|rule| rule.id == d.rule_id)
                        .is_some_and(|rule| {
                            rule.when.iter().any(|c| {
                                let threshold = if c.count > 0 {
                                    c.count
                                } else {
                                    match c.kind.as_str() {
                                        "stuck" => 2,
                                        "errors_in_row" => 3,
                                        "no_progress" => 5,
                                        _ => 1,
                                    }
                                };
                                !c.not
                                    && match c.kind.as_str() {
                                        "stuck" => s.repeat_failures >= threshold,
                                        "errors_in_row" => s.errors_in_row >= threshold,
                                        "no_progress" => s.no_progress_turns >= threshold,
                                        _ => false,
                                    }
                            })
                        });
                let d = routerules::Decision { consume_stuck, ..d };
                announce(h, &who, &s, &d).await;
                Some(((route_id.clone(), chosen.clone(), base_effort), d))
            }
        };
        let (start, effort) = match &decided {
            Some((_, d)) => (d.model.clone(), d.effort),
            None => (chosen.clone(), base_effort),
        };
        let pass = with_effort(req, effort);
        let (chain, on_out) = if decided.is_some() {
            routed_steps(&start, &chosen, &route_id)
        } else {
            steps(&chosen, &route_id)
        };
        // A routed model may produce less than the base one the caller sized
        // `max_tokens` for; a request over a model's limit is a 400.
        let pass = if decided.is_some() {
            with_output_cap(pass, chain_output(&chain))
        } else {
            pass
        };
        let settings = h.settings.read().await.clone();
        let mut last_err = String::from("no provider configured");
        let mut failed: Vec<String> = vec![];
        // Whether anything in this pass failed in a way waiting could fix. A
        // pass where every target rejected the request itself (400/404/422) has
        // nothing to wait for: pausing as "exhausted" and resuming just sends
        // the same rejected payload again, forever.
        let mut retryable = false;

        for step in &chain {
            let prov = step.split('/').next().unwrap_or("");
            let list = targets(h, &settings, step);
            if list.is_empty() && providers::is_pool(prov) {
                let any = settings.accounts.iter().any(|a| a.kind == prov);
                last_err = if any {
                    format!("every {prov} account is out of usage")
                } else {
                    format!("no {prov} accounts connected")
                };
                retryable = true;
                failed.push(step.clone());
                continue;
            }
            let insist = providers::insist(&settings, prov);
            for mut t in list {
                let mut tries = 0u32;
                let mut refreshed = false;
                loop {
                    tries += 1;
                    if let Some(a) = &t.account {
                        match accounts::fresh(h, &a.id, false).await {
                            Ok(a) => t.account = Some(a),
                            Err(e) => {
                                // This account's token could not be refreshed.
                                // The other accounts of this step are separate
                                // logins with their own tokens, so failing over
                                // is the whole point of having several: ending
                                // the walk here parked the chat with an untried
                                // healthy account behind it, which read as
                                // "it will not switch accounts".
                                last_err = e;
                                break;
                            }
                        }
                    }
                    // A per-attempt token the pause watcher can pull without cancelling the agent.
                    let tok = cancel.child_token();
                    let watcher = {
                        let (h, tid, tok, side) =
                            (h.clone(), who.task_id.to_string(), tok.clone(), who.side);
                        tokio::spawn(async move {
                            loop {
                                let bell = h.pause_bell.notified();
                                if !side && h.is_paused(&tid).await {
                                    tok.cancel();
                                    return;
                                }
                                tokio::select! { _ = bell => {}, _ = tok.cancelled() => return }
                            }
                        })
                    };
                    let mut emitted = false;
                    let mut hdrs = None;
                    let started = std::time::Instant::now();
                    let mut first_at: Option<std::time::Instant> = None;
                    let res = {
                        let mut wrapped = |e: StreamEvent| {
                            emitted = true;
                            first_at.get_or_insert_with(std::time::Instant::now);
                            on(e)
                        };
                        providers::call(
                            &h.http,
                            &settings,
                            &t,
                            &pass,
                            &mut wrapped,
                            &tok,
                            &mut hdrs,
                        )
                        .await
                    };
                    watcher.abort();
                    if let (Some(a), Some(hd)) = (&t.account, &hdrs) {
                        h.accts.update_from_headers(&a.kind, &a.id, hd);
                    }
                    let err = match res {
                        Ok(turn) => {
                            if let Some(l) = retry_line.take() {
                                unstatus(h, &who, &l).await;
                            }
                            if let Some(a) = &t.account {
                                h.accts.ok(&a.kind, &a.id);
                            } else if let Some(k) = &t.api_key {
                                h.keys.ok(&t.prov, k);
                            }
                            let u = &turn.usage;
                            let sample = super::stats::Sample {
                                input: u.input,
                                output: u.output,
                                cache_read: u.cache_read,
                                cache_write: u.cache_write,
                                reasoning: u.reasoning,
                                ms: started.elapsed().as_millis() as u64,
                                ttft_ms: first_at.map(|f| (f - started).as_millis() as u64),
                                cost: u.cost(&providers::model_info(&t.model_id)),
                            };
                            let stats_label_owned: Option<String> = t
                                .account
                                .as_ref()
                                .map(|a| a.label.clone())
                                .or_else(|| t.api_key.as_ref().map(|k| providers::key_hint(k)));
                            h.stats.record(
                                who.task_id,
                                &t.model_id,
                                stats_label_owned.as_deref(),
                                &agent_kind(h, &who).await,
                                who.usage_id,
                                &sample,
                            );
                            // Opt-in trace export. A no-op unless the user has
                            // named a collector: it reads one atomic and returns.
                            // Provider-reported counts only, never estimated.
                            crate::trace::model_request(&crate::trace::ModelRequest {
                                provider: &t.prov,
                                model: &t.model,
                                input_tokens: Some(u.input),
                                output_tokens: Some(u.output),
                                duration: started.elapsed(),
                                error: false,
                            });
                            let label = t.label();
                            // The user swapped this agent's model while it was waiting
                            // (out of usage, paused): say so, unless the chain fell
                            // back on the way here — "Now using …" already explains it.
                            if let Some(was) = swapped_from.take() {
                                if failed.is_empty() {
                                    notice(
                                        h,
                                        &who,
                                        format!("This agent is now on {label} (was {was})"),
                                    )
                                    .await;
                                }
                            }
                            if !failed.is_empty()
                                || first_label.as_deref().is_some_and(|l| l != label)
                            {
                                h.stats.event_in(who.task_id, "fallbacks");
                                notice(h, &who, format!("Now using {label}")).await;
                            }
                            if who.turn {
                                if let Some((_, d)) = &decided {
                                    commit(h, &who, d);
                                }
                            }
                            return Ok((turn, t));
                        }
                        Err(e) => e,
                    };
                    first_label.get_or_insert_with(|| t.label());
                    if !matches!(err, Fail::Cancel) {
                        let stats_label_owned: Option<String> = t
                            .account
                            .as_ref()
                            .map(|a| a.label.clone())
                            .or_else(|| t.api_key.as_ref().map(|k| providers::key_hint(k)));
                        h.stats
                            .error(who.task_id, &t.model_id, stats_label_owned.as_deref());
                        // The failing half of the same span: no counts (the
                        // provider reported none) but the duration and the fact
                        // that it failed, which is the interesting part.
                        crate::trace::model_request(&crate::trace::ModelRequest {
                            provider: &t.prov,
                            model: &t.model,
                            input_tokens: None,
                            output_tokens: None,
                            duration: started.elapsed(),
                            error: true,
                        });
                        h.stats.event_in(who.task_id, "retries");
                    }
                    if emitted {
                        on(StreamEvent::Reset);
                    }
                    let label = t.label();
                    match err {
                        Fail::Cancel if cancel.is_cancelled() => return Err(RouteErr::Cancelled),
                        Fail::Cancel => {
                            // Paused mid-request: wait, then redo this same attempt.
                            if !h.wait_unpaused(who.task_id, cancel).await {
                                return Err(RouteErr::Cancelled);
                            }
                            tries -= 1;
                            continue;
                        }
                        Fail::TooLong(m) => return Err(RouteErr::TooLong(m)),
                        Fail::Auth(m) => {
                            retryable = true;
                            last_err = m;
                            if let Some(a) = &t.account {
                                if !refreshed {
                                    refreshed = true;
                                    if accounts::fresh(h, &a.id, true).await.is_ok() {
                                        continue;
                                    }
                                }
                                h.accts.cool(&a.id, now() + 600, "Credential rejected");
                            } else if let Some(k) = &t.api_key {
                                h.keys.cool(&t.prov, k, now() + 600, "Key rejected");
                            }
                            break;
                        }
                        Fail::Exhausted { msg, until, hard } => {
                            retryable = true;
                            last_err = msg.clone();
                            if let Some(a) = &t.account {
                                let until = if until > now() {
                                    until
                                } else {
                                    now() + if hard { 1800 } else { 60 }
                                };
                                h.accts.cool(
                                    &a.id,
                                    until,
                                    if hard {
                                        "Usage limit reached"
                                    } else {
                                        "Rate limited"
                                    },
                                );
                                // Pull fresh numbers in the background for the UI.
                                let (h2, id) = (h.clone(), a.id.clone());
                                tokio::spawn(async move {
                                    let _ = accounts::fetch_usage(&h2, &id).await;
                                });
                                break;
                            }
                            if let Some(k) = t.api_key.as_ref() {
                                // Key pool: cool this key and immediately move to the next.
                                let until = if until > now() {
                                    until
                                } else {
                                    now() + if hard { 1800 } else { 60 }
                                };
                                h.keys.cool(
                                    &t.prov,
                                    k,
                                    until,
                                    if hard {
                                        "Usage limit reached"
                                    } else {
                                        "Rate limited"
                                    },
                                );
                                break;
                            }
                            let max = if insist {
                                40
                            } else if hard {
                                1
                            } else {
                                15
                            };
                            if tries >= max {
                                break;
                            }
                            let wait = if until > now() {
                                (until - now()).clamp(3, 90) as u64
                            } else {
                                backoff(tries, insist).max(5)
                            };
                            let line = format!(
                                "Rate limited by {label} · retrying in {wait}s ({tries}/{max})"
                            );
                            status(h, &who, line.clone()).await;
                            retry_line = Some(line);
                            if !nap(h, &who, wait, cancel).await {
                                return Err(RouteErr::Cancelled);
                            }
                        }
                        // The provider rejected this exact request: resending it
                        // gets the same answer (a malformed history 400'd three
                        // times in a row before this). Another target in the
                        // chain may still take it, so move on, not give up.
                        // An `insist` gateway is the exception by design: those
                        // say 4xx for "no channel free right now".
                        Fail::Bad(m) if !insist && rejects_request(&m) => {
                            last_err = m;
                            break;
                        }
                        Fail::Transient(m) | Fail::Bad(m) => {
                            retryable = true;
                            // The gateway's "endpoint is unavailable" blip gets
                            // fifteen tries before the chain is declared out.
                            // Everything else keeps its old ladder: noise gets
                            // six tries, a 4xx the provider might still accept
                            // gets three.
                            let bad = m.starts_with("HTTP 4");
                            let max = if providers::looks_endpoint_unavailable(&m) {
                                15
                            } else if insist {
                                40
                            } else if bad {
                                3
                            } else {
                                6
                            };
                            last_err = m.clone();
                            if tries >= max {
                                break;
                            }
                            let wait = backoff(tries, insist);
                            let short: String = m.chars().take(90).collect();
                            let line = format!("{label}: {short} · retry {tries}/{max} in {wait}s");
                            status(h, &who, line.clone()).await;
                            retry_line = Some(line);
                            if !nap(h, &who, wait, cancel).await {
                                return Err(RouteErr::Cancelled);
                            }
                        }
                    }
                }
                failed.push(label_of(&t));
            }
        }

        if on_out == "fail" || (!retryable && !failed.is_empty()) {
            return Err(RouteErr::Fatal(last_err));
        }
        let pool_accts: Vec<_> = settings
            .accounts
            .iter()
            .filter(|a| chain.iter().any(|s| s.starts_with(&format!("{}/", a.kind))))
            .cloned()
            .collect();
        let mut free = h.accts.next_free(&pool_accts);
        for step in &chain {
            let prov = step.split('/').next().unwrap_or("");
            if providers::key_pool(&settings, prov) {
                if let Some(cfg) = settings.providers.get(prov) {
                    free = free.max(h.keys.next_free(prov, &cfg.api_keys));
                }
            }
        }
        let when = if free > 0 {
            format!(
                " · an account or key frees up {}",
                chrono::DateTime::from_timestamp(free, 0)
                    .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
                    .unwrap_or_default()
            )
        } else {
            String::new()
        };
        let short: String = last_err.chars().take(4000).collect();
        if who.side {
            return Err(RouteErr::Fatal(format!(
                "Every model in the chain failed: {short}{when}"
            )));
        }
        h.stats.event_in(who.task_id, "pauses");
        h.pause_task(
            who.task_id,
            "exhausted",
            &format!("Every model in the chain failed: {short}{when}"),
        )
        .await;
        if !h.wait_unpaused(who.task_id, cancel).await {
            return Err(RouteErr::Cancelled);
        }
    }
}

/// The app-wide `agents` key: "main", a subagent's type (explore, general, …),
/// or a system label for a one-shot that belongs to no agent at all.
///
/// The `usage_id` is read for the system case and not derived from `sub`, because
/// the one-shots that matter most all run with `sub: None` — compaction, titling,
/// the keepalive. Recording them as "main" is how their spend hid inside the main
/// agent's line; a small closed set of labels here is what makes "what did
/// compaction cost me" answerable from the stats screen, and it is the same
/// hidden-system-agent idea opencode uses.
async fn agent_kind(h: &Harness, who: &Who<'_>) -> String {
    let Some(sid) = who.sub else {
        return agent_kind_for_test(who.usage_id, None);
    };
    match h.task(who.task_id).await {
        Ok(t) => t
            .lock()
            .await
            .subs
            .iter()
            .find(|s| s.id == sid)
            .map(|s| s.role.clone())
            .unwrap_or_else(|| "subagent".into()),
        Err(_) => "subagent".into(),
    }
}

/// A failure that is about the request itself, so the same payload fails the
/// same way every time. `classify` gives these codes `Bad`; other `Bad` codes
/// (an odd status from a gateway) keep their few retries.
fn rejects_request(msg: &str) -> bool {
    ["HTTP 400", "HTTP 404", "HTTP 422"]
        .iter()
        .any(|p| msg.starts_with(p))
}

fn label_of(t: &Target) -> String {
    t.label()
}

/// The app-wide `agents` key for a request, with no `Harness` in the picture —
/// the naming rule `agent_kind` applies, exposed so it can be tested without
/// standing up a task.
///
/// `sub` is the subagent's *role* (what the caller resolved from the task), not
/// its id: the dimension is per agent type, unlike the per-chat `by_agent` split
/// which is per agent instance.
pub fn agent_kind_for_test(usage_id: &str, sub_role: Option<&str>) -> String {
    match sub_role {
        Some(role) => role.to_string(),
        None if usage_id == "main" => "main".into(),
        None => usage_id.to_string(),
    }
}

/// Run a one-shot request with the task's frozen prefix (compaction, side questions).
pub async fn oneshot(
    h: &Arc<Harness>,
    who: Who<'_>,
    model: &str,
    req: &ChatRequest,
    cancel: &CancellationToken,
) -> Result<(String, providers::TurnUsage, Target), RouteErr> {
    let mut sink = |_e: StreamEvent| {};
    let (turn, t) = request(h, who, model, req, &mut sink, cancel).await?;
    let text = turn
        .content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((text, turn.usage, t))
}
