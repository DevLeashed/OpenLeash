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
//! **pauses** — main agent and every sub-agent — and the request resumes
//! exactly where it was once the user (or an account reset) unpauses it.
//! The model never hears about any of this.

use super::accounts::{self, now};
use super::providers::{self, ChatRequest, Fail, StreamEvent, Target, Turn};
use super::store::{Route, Settings};
use super::{Harness, Item};
use serde_json::json;
use std::borrow::Cow;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

static ROUTES: std::sync::RwLock<Vec<Route>> = std::sync::RwLock::new(Vec::new());

/// The route table is process-global, so tests that set it take turns — from
/// either test module, which is why the lock lives here rather than next to one
/// of them.
#[cfg(test)]
pub static ROUTES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn set_routes(r: &[Route]) {
    *ROUTES.write().unwrap() = r.to_vec();
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
pub fn context_window(model: &str, route_id: &str) -> u64 {
    let (s, _) = steps(model, route_id);
    s.iter()
        .map(|m| providers::model_info(m).context)
        .min()
        .unwrap_or(128_000)
        .max(8_000)
}

pub fn max_output(model: &str, route_id: &str) -> u64 {
    let (s, _) = steps(model, route_id);
    s.iter()
        .map(|m| providers::model_info(m).output)
        .min()
        .unwrap_or(32_000)
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

/// Who is asking: the main agent or one sub-agent of a task.
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
        // A sub-agent's own entry, for the model somebody chose for it and for the
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
            .filter(|a| h.accts.usable(a))
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
async fn live_effort(h: &Arc<Harness>, who: &Who<'_>, fallback: usize) -> usize {
    if !who.turn {
        return fallback;
    }
    let Ok(t) = h.task(who.task_id).await else {
        return fallback;
    };
    let t = t.lock().await;
    // A sub-agent's own override wins over the chat's, matching the loop.
    match who.sub {
        Some(sid) => t
            .subs
            .iter()
            .find(|x| x.id == sid)
            .and_then(|x| x.effort)
            .unwrap_or(t.effort),
        None => t.effort,
    }
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
    let mut first_label: Option<String> = None;
    let (mut route_id, mut chosen) = live(h, &who, model).await;
    let mut swapped_from: Option<String> = None;
    // The retry line this pass last put on the task, so a success can clear it.
    let mut retry_line: Option<String> = None;
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
        let pass = with_effort(req, live_effort(h, &who, req.effort).await);
        let (chain, on_out) = steps(&chosen, &route_id);
        let settings = h.settings.read().await.clone();
        let mut last_err = String::from("no provider configured");
        let mut failed: Vec<String> = vec![];

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
                failed.push(step.clone());
                continue;
            }
            let insist = providers::insist(&settings, prov);
            'target: for mut t in list {
                let mut tries = 0u32;
                let mut refreshed = false;
                loop {
                    tries += 1;
                    if let Some(a) = &t.account {
                        match accounts::fresh(h, &a.id, false).await {
                            Ok(a) => t.account = Some(a),
                            Err(e) => {
                                last_err = e;
                                break 'target;
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
                        h.accts.from_headers(&a.kind, &a.id, hd);
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
                        Fail::Transient(m) | Fail::Bad(m) => {
                            let bad = m.starts_with("HTTP 4");
                            let max = if insist {
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

        if on_out == "fail" {
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

/// The app-wide `agents` key: "main", a sub-agent's type (explore, general, …),
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

fn label_of(t: &Target) -> String {
    t.label()
}

/// The app-wide `agents` key for a request, with no `Harness` in the picture —
/// the naming rule `agent_kind` applies, exposed so it can be tested without
/// standing up a task.
///
/// `sub` is the sub-agent's *role* (what the caller resolved from the task), not
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
