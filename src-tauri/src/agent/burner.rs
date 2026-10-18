//! "Spend quota before it resets": a per-project backlog of low-priority
//! prompts that run on subscription quota which would otherwise be forfeited.
//!
//! A subscription window (Claude's 5h, a ChatGPT week …) is use-it-or-lose-it:
//! whatever is left when it rolls over is gone. The watcher here looks at the
//! same usage readings the router and Settings → Accounts use, and when the
//! account the router would serve a provider from is about to roll over with
//! real headroom left, it starts the next backlog item as a fresh chat in a git
//! worktree, pinned to that provider's model and never to anything else.
//!
//! The run has a hard stop. It is interrupted when the window rolls over, when
//! any window of the account reaches the burner's spend cap or a usage cap the
//! user set, when the router would move it onto a different account, or when
//! the burner is switched off. Caps are never relaxed: the burner's ceiling for
//! a window is the *lower* of its own spend cap and every live user cap on that
//! window, and the router keeps enforcing the user's caps per request anyway.
//!
//! The decision logic is pure (`trigger`, `stop_reason`, `clamp_perm`,
//! `pick_model`) so it can be tested without a provider, an account, or a
//! clock; `tick` is the thin driver around it.

use super::accounts::{self, AcctUsage, Window};
use super::store::{self, Settings, UsageCap};
use super::{git, host, permissions, providers, runner, Harness, Item};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

/// A usage reading older than this is not trusted to start a run: the account
/// watcher refreshes every five minutes, so anything older means refreshes are
/// failing and the "headroom" on screen may already be spent.
pub const STALE_SECS: i64 = 20 * 60;

/// How many finished runs are kept for the Settings list. Active runs are
/// never dropped, whatever their number.
const KEEP_RUNS: usize = 50;

/// Prefix of the chat title, so a burner chat is recognisable in the sidebar,
/// the Needs-you inbox and anywhere else a title is shown.
pub const TITLE_PREFIX: &str = "[burner] ";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BurnerCfg {
    /// Off by default: the burner starts chats on its own, which nobody should
    /// discover by surprise.
    pub enabled: bool,
    /// Start only when a window resets within this many minutes.
    pub reset_within_min: u32,
    /// …and at least this many percentage points of it are still spendable
    /// (under the ceiling, see `ceiling`).
    pub min_headroom: f64,
    /// Percent of a window used at which a burner run is stopped. Applies to
    /// every window of the account, not only the one about to reset, so a
    /// burn never eats a weekly window that is not about to roll over.
    pub spend_cap: f64,
    /// The backlog, oldest first. Items leave the queue when they start.
    pub queue: Vec<BacklogItem>,
}

impl Default for BurnerCfg {
    fn default() -> Self {
        BurnerCfg {
            enabled: false,
            reset_within_min: 30,
            min_headroom: 20.0,
            spend_cap: 90.0,
            queue: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct BacklogItem {
    pub id: String,
    /// Project folder the chat is started in (as a worktree of it).
    pub project: String,
    pub prompt: String,
    /// Full model id (`claude/…`, `codex/…`). Empty = any subscription
    /// provider, on the model `pick_model` finds for it. A model also limits
    /// the item to that provider's windows.
    pub model: String,
    /// Permission mode for the run. Empty = the user's default mode. Never
    /// stronger than the default either way, see `clamp_perm`.
    pub perm: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Why the last start attempt failed; an item with an error is skipped
    /// until the user edits or clears it.
    pub error: String,
}

/// One chat the burner started. Kept in `burner.json` rather than in
/// `settings.json`: settings are patched whole from the UI, which would put a
/// stale copy of this list back over the watcher's.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct BurnRun {
    pub task_id: String,
    pub item_id: String,
    pub project: String,
    pub prompt: String,
    pub model: String,
    pub perm: String,
    pub account_id: String,
    pub account_label: String,
    /// Account kind, which is also the pool provider id.
    pub kind: String,
    /// The window whose rollover the run is spending ahead of.
    pub window: String,
    /// When that window resets (unix seconds) — the run's hard deadline.
    pub resets_at: i64,
    pub started_at: i64,
    /// 0 while the run is live.
    pub ended_at: i64,
    pub end_reason: String,
}

impl BurnRun {
    pub fn active(&self) -> bool {
        self.ended_at == 0
    }
}

/// What the burner will spend a window up to: its own spend cap, lowered by any
/// live usage cap the user put on the same window. "Used reached the ceiling"
/// is `used >= ceiling`, the same comparison `accounts::cap_holding` makes.
pub fn ceiling(label: &str, caps: &[UsageCap], spend_cap: f64, now: i64) -> f64 {
    caps.iter()
        .filter(|c| c.window == label && (c.until == 0 || c.until > now))
        .map(|c| c.used)
        .fold(spend_cap.clamp(0.0, 100.0), f64::min)
}

/// A window that has already rolled over says nothing about now: its usage
/// figure belongs to the old window, and the next reading starts it again.
fn current(w: &Window, now: i64) -> bool {
    w.resets_at == 0 || w.resets_at > now
}

/// The window a run may be started ahead of, or why none qualifies.
///
/// Fires only when *all* of these hold: the reading is fresh and the provider
/// is not limiting the account; no current window is at its ceiling (a burn
/// spends every window of the account, so a nearly spent weekly window vetoes
/// a burn of the 5h one); and some window resets within `reset_within_min`
/// with at least `min_headroom` points left under its ceiling. Of several, the
/// one resetting soonest is chosen.
pub fn trigger(
    u: &AcctUsage,
    caps: &[UsageCap],
    cfg: &BurnerCfg,
    now: i64,
) -> Result<Window, String> {
    if u.limited {
        return Err("the provider reports the account as limited".into());
    }
    if u.fetched <= 0 || now - u.fetched > STALE_SECS {
        return Err("no fresh usage reading".into());
    }
    if let Some(w) = u
        .windows
        .iter()
        .filter(|w| current(w, now))
        .find(|w| w.used >= ceiling(&w.label, caps, cfg.spend_cap, now))
    {
        return Err(format!("{} is already at its ceiling", w.label));
    }
    let within = i64::from(cfg.reset_within_min) * 60;
    let mut best: Option<&Window> = None;
    let mut why = "no window resets soon".to_string();
    for w in u.windows.iter().filter(|w| w.resets_at > now) {
        if w.resets_at - now > within {
            continue;
        }
        let room = ceiling(&w.label, caps, cfg.spend_cap, now) - w.used;
        if room < cfg.min_headroom {
            why = format!(
                "{} resets soon but only {:.0}% is left to spend",
                w.label,
                room.max(0.0)
            );
            continue;
        }
        if best.is_none_or(|b| w.resets_at < b.resets_at) {
            best = Some(w);
        }
    }
    best.cloned().ok_or(why)
}

/// Why a live run must stop now, or `None` to let it carry on.
///
/// `usage` is the account's latest reading (`None` = none yet), `usable` the
/// router's own verdict on the account, and `head` the account the router
/// would serve this provider from right now.
pub fn stop_reason(
    run: &BurnRun,
    usage: Option<&AcctUsage>,
    caps: &[UsageCap],
    cfg: &BurnerCfg,
    usable: bool,
    head: Option<&str>,
    now: i64,
) -> Option<String> {
    if !cfg.enabled {
        return Some("the quota burner was switched off".into());
    }
    if run.resets_at > 0 && now >= run.resets_at {
        return Some(format!("the {} window reset", run.window));
    }
    if let Some(u) = usage {
        // A reset time that moved well past the recorded one is a window that
        // already rolled over (clocks and header rounding wobble by seconds).
        if u.windows
            .iter()
            .any(|w| w.label == run.window && w.resets_at > run.resets_at + 120)
        {
            return Some(format!("the {} window reset", run.window));
        }
        if let Some(w) = u
            .windows
            .iter()
            .filter(|w| current(w, now))
            .find(|w| w.used >= ceiling(&w.label, caps, cfg.spend_cap, now))
        {
            let c = ceiling(&w.label, caps, cfg.spend_cap, now);
            let whose = if c < cfg.spend_cap.clamp(0.0, 100.0) {
                "your usage cap"
            } else {
                "the burner's spend cap"
            };
            return Some(format!(
                "{} reached {whose} ({:.0}% of {:.0}%)",
                w.label, w.used, c
            ));
        }
    }
    if !usable {
        return Some("the account is no longer available (limit, cap or cooldown)".into());
    }
    if head.is_some_and(|h| h != run.account_id) {
        return Some("the provider would now be served by a different account".into());
    }
    None
}

/// The item's permission mode, never stronger than the user's default.
///
/// The default is what the user picked for chats they start themselves; a run
/// nobody is watching must not get more than that, so an item asking for more
/// is held down to it.
pub fn clamp_perm(item: &str, default: &str) -> String {
    let item = item.trim();
    if item.is_empty() || permissions::strength(item) > permissions::strength(default) {
        default.to_string()
    } else {
        item.to_string()
    }
}

fn provider_of(model: &str) -> &str {
    model.split('/').next().unwrap_or("")
}

/// The model a run on `kind` uses: the item's own if it names this provider
/// (an item for another provider never runs here), else the user's default or
/// most recent / favourite model on this provider, else the first catalog
/// model the provider offers.
pub fn pick_model(
    kind: &str,
    item_model: &str,
    s: &Settings,
    catalog: &[providers::ModelInfo],
) -> Option<String> {
    let item_model = item_model.trim();
    if !item_model.is_empty() {
        return (provider_of(item_model) == kind).then(|| item_model.to_string());
    }
    let usable = |m: &&String| {
        provider_of(m) == kind && !s.disabled_models.contains(m) && !s.removed_models.contains(m)
    };
    std::iter::once(&s.model)
        .chain(s.recent_models.iter())
        .chain(s.favorite_models.iter())
        .find(usable)
        .cloned()
        .or_else(|| {
            catalog
                .iter()
                .find(|m| m.provider == kind)
                .map(|m| m.id.clone())
        })
}

/// The first queue item that may run on `kind`: not already started, not
/// carrying an error, and either provider-agnostic or for this provider.
pub fn next_item<'a>(
    cfg: &'a BurnerCfg,
    kind: &str,
    started: &HashSet<String>,
) -> Option<&'a BacklogItem> {
    cfg.queue.iter().find(|i| {
        !started.contains(&i.id)
            && i.error.is_empty()
            && !i.prompt.trim().is_empty()
            && !i.project.trim().is_empty()
            && (i.model.trim().is_empty() || provider_of(&i.model) == kind)
    })
}

// ----- persistence of runs -------------------------------------------------

static RUNS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn runs_path() -> std::path::PathBuf {
    store::data_dir().join("burner.json")
}

pub fn load_runs() -> Vec<BurnRun> {
    let _g = RUNS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::read_to_string(runs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_runs(runs: &[BurnRun]) {
    let _g = RUNS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keep: Vec<BurnRun> = runs.to_vec();
    // Newest last; trim the oldest finished runs only.
    let finished = keep.iter().filter(|r| !r.active()).count();
    if finished > KEEP_RUNS {
        let mut drop_n = finished - KEEP_RUNS;
        keep.retain(|r| {
            if drop_n > 0 && !r.active() {
                drop_n -= 1;
                false
            } else {
                true
            }
        });
    }
    if let Ok(s) = serde_json::to_string_pretty(&keep) {
        store::write_atomic(&runs_path(), &s);
    }
}

// ----- status for the UI ---------------------------------------------------

/// One account as the burner sees it, for the Settings panel: what it would do
/// right now and why.
#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub account_id: String,
    pub label: String,
    pub kind: String,
    /// The account the router serves this provider from right now.
    pub head: bool,
    /// The window a run would start ahead of, if one qualifies.
    pub window: Option<Window>,
    /// Why nothing would start (empty when `window` is set).
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub runs: Vec<BurnRun>,
    pub accounts: Vec<AccountStatus>,
}

pub async fn status(h: &Harness) -> Status {
    let s = h.settings.read().await;
    let t = accounts::now();
    let mut out = vec![];
    for a in s.accounts.iter().filter(|a| providers::is_pool(&a.kind)) {
        let head = head_account(h, &s, &a.kind);
        let is_head = head.as_deref() == Some(a.id.as_str());
        let res = if !a.enabled || !a.disabled_reason.is_empty() {
            Err("account is disabled".to_string())
        } else if !h.accts.usable(a) {
            Err("account is not usable right now (limit, cap or cooldown)".to_string())
        } else {
            match h.accts.usage_of(&a.id) {
                Some(u) => trigger(&u, &a.caps, &s.burner, t),
                None => Err("no usage reading yet".into()),
            }
        };
        let (window, mut reason) = match res {
            Ok(w) => (Some(w), String::new()),
            Err(e) => (None, e),
        };
        if window.is_some() && !is_head {
            reason = "another account of this provider is served first".into();
        }
        out.push(AccountStatus {
            account_id: a.id.clone(),
            label: if a.label.is_empty() {
                a.email.clone()
            } else {
                a.label.clone()
            },
            kind: a.kind.clone(),
            head: is_head,
            window: window.filter(|_| is_head),
            reason,
        });
    }
    Status {
        runs: load_runs(),
        accounts: out,
    }
}

// ----- the driver ----------------------------------------------------------

/// The account the router would try first for this provider: pool order, then
/// the same usability filter `router::targets` applies.
fn head_account(h: &Harness, s: &Settings, kind: &str) -> Option<String> {
    accounts::pool(h, &s.accounts, kind)
        .into_iter()
        .find(|a| h.accts.usable(a))
        .map(|a| a.id)
}

/// Is this chat still doing something the burner is responsible for?
async fn still_running(h: &Harness, task_id: &str) -> Option<bool> {
    let t = h.task_ref(task_id).await.ok()?;
    let (status, paused) = {
        let t = t.lock().await;
        (t.status.clone(), t.paused.is_some())
    };
    let rt = h.runtime_if(task_id);
    let live = rt.as_ref().is_some_and(|r| {
        r.running.load(std::sync::atomic::Ordering::SeqCst)
            || r.startup_pending.load(std::sync::atomic::Ordering::SeqCst)
    });
    Some(live || paused || matches!(status.as_str(), "running" | "waiting"))
}

/// Runs forever: one pass a minute, after a short delay so the first usage
/// refresh has landed.
pub async fn watch(h: Arc<Harness>) {
    tokio::time::sleep(std::time::Duration::from_secs(45)).await;
    loop {
        tick(&h).await;
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

/// Select a queue item for this pass before starting it on a provider.
pub(super) fn claim_next_item<'a>(
    cfg: &'a BurnerCfg,
    kind: &str,
    started: &mut HashSet<String>,
) -> Option<&'a BacklogItem> {
    let item = next_item(cfg, kind, started)?;
    started.insert(item.id.clone());
    Some(item)
}

/// One pass: stop runs that must stop, then start at most one run per provider.
static TICK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn tick(h: &Arc<Harness>) {
    let _tick = TICK_LOCK.lock().await;
    let s = h.settings.read().await.clone();
    let mut runs = load_runs();
    if !s.burner.enabled && !runs.iter().any(BurnRun::active) {
        return;
    }
    let t = accounts::now();
    let mut dirty = false;

    for run in runs.iter_mut().filter(|r| r.active()) {
        let Some(running) = still_running(h, &run.task_id).await else {
            run.ended_at = t;
            run.end_reason = "chat deleted".into();
            dirty = true;
            continue;
        };
        if !running {
            run.ended_at = t;
            run.end_reason = "finished".into();
            dirty = true;
            continue;
        }
        let Ok(task_ref) = h.task_ref(&run.task_id).await else {
            run.ended_at = t;
            run.end_reason = "chat deleted".into();
            dirty = true;
            continue;
        };
        let mut task = task_ref.lock().await;
        if task.burner.as_ref().is_some_and(|_| run.active()) {
            task.burner = None;
        }
        drop(task);
        let acct = s.accounts.iter().find(|a| a.id == run.account_id);
        let usage = h.accts.usage_of(&run.account_id);
        let caps: &[UsageCap] = acct.map(|a| a.caps.as_slice()).unwrap_or(&[]);
        let usable = acct.is_some_and(|a| h.accts.usable(a));
        let head = head_account(h, &s, &run.kind);
        if let Some(why) = stop_reason(
            run,
            usage.as_ref(),
            caps,
            &s.burner,
            usable,
            head.as_deref(),
            t,
        ) {
            runner::interrupt(h, &run.task_id).await;
            h.upsert_item(
                &run.task_id,
                Item::new(
                    "notice",
                    format!("Quota burner stopped this chat: {why}. Send a message to carry on by hand."),
                    json!({"level": "event", "sum": "burner stopped"}),
                ),
            )
            .await;
            run.ended_at = t;
            run.end_reason = why;
            dirty = true;
        }
    }

    let mut errors: Vec<(String, String)> = vec![];
    if s.burner.enabled && !s.paused_all {
        let mut started: HashSet<String> = runs.iter().map(|r| r.item_id.clone()).collect();
        let busy: HashSet<String> = runs
            .iter()
            .filter(|r| r.active())
            .map(|r| r.kind.clone())
            .collect();
        let mut kinds: Vec<String> = s
            .accounts
            .iter()
            .map(|a| a.kind.clone())
            .filter(|k| providers::is_pool(k))
            .collect();
        kinds.sort();
        kinds.dedup();
        let catalog = providers::all_models();
        for kind in kinds.iter().filter(|k| !busy.contains(*k)) {
            let Some(head) = head_account(h, &s, kind) else {
                continue;
            };
            let Some(acct) = s.accounts.iter().find(|a| a.id == head) else {
                continue;
            };
            let Some(usage) = h.accts.usage_of(&head) else {
                continue;
            };
            let Ok(window) = trigger(&usage, &acct.caps, &s.burner, t) else {
                continue;
            };
            let Some(item) = claim_next_item(&s.burner, kind, &mut started) else {
                continue;
            };
            match start(h, &s, item, acct, &window, &catalog).await {
                Ok(run) => {
                    runs.push(run);
                    dirty = true;
                }
                Err(e) => errors.push((item.id.clone(), e)),
            }
        }
    }

    if dirty {
        save_runs(&runs);
    }
    // Started items leave the queue; failed ones keep their place with the
    // reason. The UI may have patched the queue meanwhile, so this edits the
    // live copy by id rather than writing back the snapshot taken above.
    let started: HashSet<String> = runs.iter().map(|r| r.item_id.clone()).collect();
    let mut st = h.settings.write().await;
    let before = st.burner.queue.clone();
    st.burner.queue.retain(|i| !started.contains(&i.id));
    for (id, e) in &errors {
        if let Some(i) = st.burner.queue.iter_mut().find(|i| &i.id == id) {
            i.error = e.chars().take(300).collect();
        }
    }
    if st.burner.queue != before {
        store::save_settings(&st);
        let out = serde_json::to_value(crate::public_settings(&st)).unwrap_or_default();
        drop(st);
        (h.bus)("ol://settings", out);
    }
}

/// Start one backlog item on `acct`'s provider. Errors are user-facing.
async fn start(
    h: &Arc<Harness>,
    s: &Settings,
    item: &BacklogItem,
    acct: &store::Account,
    window: &Window,
    catalog: &[providers::ModelInfo],
) -> Result<BurnRun, String> {
    let project = item.project.trim().to_string();
    if host::is_ssh(&project) {
        return Err("SSH projects can't run in a worktree".into());
    }
    let p2 = project.clone();
    let repo = tokio::task::spawn_blocking(move || git::is_repo(&p2))
        .await
        .unwrap_or(false);
    if !repo {
        return Err("the project is not a git repository, so it can't get a worktree".into());
    }
    let model = pick_model(&acct.kind, &item.model, s, catalog)
        .ok_or_else(|| format!("no model found for {}", acct.kind))?;
    let perm = clamp_perm(&item.perm, &s.perm);
    let framed = format!(
        "{}\n\n(Started automatically by the quota burner on spare subscription quota, in its own git worktree. \
         It is a low-priority backlog task and will be stopped when the usage window resets, so commit or \
         summarise progress as you go.)",
        item.prompt.trim()
    );
    let req = crate::NewTask {
        prompt: framed,
        project: project.clone(),
        model: model.clone(),
        effort: s.effort,
        perm: perm.clone(),
        plan: false,
        worktree: true,
        workspace: Some("worktree".into()),
        base_branch: String::new(),
        assist: s.assist.clone(),
        agents: (!s.default_agents.is_empty()).then(|| s.default_agents.clone()),
        // No route: the chain is the one pinned model, so an exhausted account
        // pauses the chat instead of falling through to another provider.
        route: Some(String::new()),
        images: vec![],
        files: vec![],
        image_names: vec![],
        ultra: false,
        ultra_wt: false,
        ultra_x: None,
    };
    // `create_task` makes the new chat's folder the app's current project and
    // its model the most recent one, which is right for a chat the user just
    // started and wrong for one started behind their back: the app would jump
    // to another folder in the middle of whatever they are doing.
    let (was_project, was_recent) = {
        let st = h.settings.read().await;
        (st.project.clone(), st.recent_models.clone())
    };
    let summary = crate::create_task_with_policy(
        h.clone(),
        req,
        Some(item.prompt.trim().to_string()),
        crate::TaskCreationPolicy {
            require_worktree: true,
            burner: Some(super::burner_policy::BurnPolicy {
                account_id: acct.id.clone(),
                model: model.clone(),
                window: window.label.clone(),
                resets_at: window.resets_at,
                spend_cap: s.burner.spend_cap,
            }),
        },
    )
    .await?;
    {
        let mut st = h.settings.write().await;
        st.project = was_project;
        st.recent_models = was_recent;
    }
    h.save_settings_debounced().await;
    let label = if acct.label.is_empty() {
        acct.email.clone()
    } else {
        acct.label.clone()
    };
    h.update_task(&summary.id, |t| {
        if !t.title.starts_with(TITLE_PREFIX) {
            t.title = format!("{TITLE_PREFIX}{}", t.title);
        }
        // The prefix is the label; the agent renaming the chat would drop it.
        t.titled = true;
    })
    .await;
    let resets = chrono::DateTime::from_timestamp(window.resets_at, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default();
    h.upsert_item(
        &summary.id,
        Item::new(
            "notice",
            format!(
                "Started by the quota burner on {label} ({}): the {} window resets at {resets} with {:.0}% used. \
                 This chat stops at the reset or when any window reaches {:.0}% (or your own cap).",
                acct.kind, window.label, window.used, s.burner.spend_cap
            ),
            json!({"level": "event", "sum": "started by the quota burner", "burner": true}),
        ),
    )
    .await;
    h.save_task(&summary.id).await;
    Ok(BurnRun {
        task_id: summary.id,
        item_id: item.id.clone(),
        project,
        prompt: item.prompt.chars().take(200).collect(),
        model,
        perm,
        account_id: acct.id.clone(),
        account_label: label,
        kind: acct.kind.clone(),
        window: window.label.clone(),
        resets_at: window.resets_at,
        started_at: accounts::now(),
        ended_at: 0,
        end_reason: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn cfg() -> BurnerCfg {
        BurnerCfg {
            enabled: true,
            ..Default::default()
        }
    }

    fn w(label: &str, used: f64, resets_in: i64) -> Window {
        Window {
            label: label.into(),
            used,
            resets_at: NOW + resets_in,
        }
    }

    fn usage(windows: Vec<Window>) -> AcctUsage {
        AcctUsage {
            windows,
            fetched: NOW - 60,
            ..Default::default()
        }
    }

    fn cap(window: &str, used: f64) -> UsageCap {
        UsageCap {
            window: window.into(),
            used,
            until: 0,
        }
    }

    fn run() -> BurnRun {
        BurnRun {
            task_id: "t1".into(),
            item_id: "i1".into(),
            account_id: "a1".into(),
            kind: "claude".into(),
            window: "5h".into(),
            resets_at: NOW + 600,
            started_at: NOW - 600,
            ..Default::default()
        }
    }

    #[test]
    fn fires_when_a_window_resets_soon_with_headroom() {
        let u = usage(vec![w("5h", 30.0, 15 * 60), w("Week", 40.0, 3 * 86_400)]);
        let got = trigger(&u, &[], &cfg(), NOW).expect("should fire");
        assert_eq!(got.label, "5h");
    }

    #[test]
    fn does_not_fire_when_the_reset_is_far_away() {
        let u = usage(vec![w("5h", 30.0, 2 * 3600), w("Week", 40.0, 3 * 86_400)]);
        assert!(trigger(&u, &[], &cfg(), NOW).is_err());
    }

    #[test]
    fn does_not_fire_without_enough_headroom() {
        // 90 (spend cap) - 75 = 15 < 20 (min headroom).
        let u = usage(vec![w("5h", 75.0, 10 * 60)]);
        let e = trigger(&u, &[], &cfg(), NOW).unwrap_err();
        assert!(e.contains("15%"), "{e}");
    }

    #[test]
    fn a_nearly_spent_other_window_vetoes_the_burn() {
        let u = usage(vec![w("5h", 10.0, 10 * 60), w("Week", 91.0, 3 * 86_400)]);
        assert!(trigger(&u, &[], &cfg(), NOW).is_err());
    }

    #[test]
    fn stale_or_limited_readings_never_fire() {
        let mut u = usage(vec![w("5h", 10.0, 10 * 60)]);
        u.fetched = NOW - STALE_SECS - 1;
        assert!(trigger(&u, &[], &cfg(), NOW).is_err());
        let mut u = usage(vec![w("5h", 10.0, 10 * 60)]);
        u.limited = true;
        assert!(trigger(&u, &[], &cfg(), NOW).is_err());
    }

    #[test]
    fn picks_the_window_resetting_soonest() {
        let u = usage(vec![w("Week", 10.0, 25 * 60), w("5h", 10.0, 5 * 60)]);
        assert_eq!(trigger(&u, &[], &cfg(), NOW).unwrap().label, "5h");
    }

    #[test]
    fn a_user_cap_lowers_the_ceiling() {
        // Burner cap 90, user cap 50: only 50 - 35 = 15 is spendable.
        let u = usage(vec![w("5h", 35.0, 10 * 60)]);
        assert!(trigger(&u, &[], &cfg(), NOW).is_ok());
        assert!(trigger(&u, &[cap("5h", 50.0)], &cfg(), NOW).is_err());
        // A cap the window already reached blocks outright.
        let u = usage(vec![w("5h", 10.0, 10 * 60), w("Week", 60.0, 86_400)]);
        assert!(trigger(&u, &[cap("Week", 60.0)], &cfg(), NOW).is_err());
        // An expired temporary cap no longer counts.
        let expired = UsageCap {
            window: "5h".into(),
            used: 40.0,
            until: NOW - 1,
        };
        let u = usage(vec![w("5h", 35.0, 10 * 60)]);
        assert!(trigger(&u, &[expired], &cfg(), NOW).is_ok());
    }

    #[test]
    fn ceiling_is_never_above_a_cap_or_the_spend_cap() {
        assert_eq!(ceiling("5h", &[], 90.0, NOW), 90.0);
        assert_eq!(ceiling("5h", &[cap("5h", 70.0)], 90.0, NOW), 70.0);
        assert_eq!(ceiling("5h", &[cap("5h", 95.0)], 90.0, NOW), 90.0);
        assert_eq!(ceiling("5h", &[cap("Week", 10.0)], 90.0, NOW), 90.0);
        assert_eq!(ceiling("5h", &[], 250.0, NOW), 100.0);
    }

    #[test]
    fn stops_at_the_window_reset() {
        let r = run();
        let u = usage(vec![w("5h", 50.0, 600)]);
        assert!(stop_reason(&r, Some(&u), &[], &cfg(), true, Some("a1"), NOW).is_none());
        let why = stop_reason(&r, Some(&u), &[], &cfg(), true, Some("a1"), r.resets_at).unwrap();
        assert!(why.contains("reset"), "{why}");
        // The reading already shows the next window.
        let rolled = usage(vec![w("5h", 2.0, 5 * 3600)]);
        let why = stop_reason(&r, Some(&rolled), &[], &cfg(), true, Some("a1"), NOW).unwrap();
        assert!(why.contains("reset"), "{why}");
    }

    #[test]
    fn stops_at_the_spend_cap_and_at_a_user_cap() {
        let r = run();
        let u = usage(vec![w("5h", 90.0, 600)]);
        let why = stop_reason(&r, Some(&u), &[], &cfg(), true, Some("a1"), NOW).unwrap();
        assert!(why.contains("spend cap"), "{why}");
        let u = usage(vec![w("5h", 60.0, 600)]);
        let why = stop_reason(
            &r,
            Some(&u),
            &[cap("5h", 60.0)],
            &cfg(),
            true,
            Some("a1"),
            NOW,
        )
        .unwrap();
        assert!(why.contains("your usage cap"), "{why}");
        // Another window reaching the spend cap stops it too.
        let u = usage(vec![w("5h", 30.0, 600), w("Week", 92.0, 86_400)]);
        assert!(stop_reason(&r, Some(&u), &[], &cfg(), true, Some("a1"), NOW).is_some());
    }

    #[test]
    fn stops_when_the_account_is_unusable_or_replaced_or_burner_off() {
        let r = run();
        let u = usage(vec![w("5h", 30.0, 600)]);
        assert!(stop_reason(&r, Some(&u), &[], &cfg(), false, Some("a1"), NOW).is_some());
        assert!(stop_reason(&r, Some(&u), &[], &cfg(), true, Some("a2"), NOW).is_some());
        let off = BurnerCfg::default();
        assert!(stop_reason(&r, Some(&u), &[], &off, true, Some("a1"), NOW).is_some());
    }

    #[test]
    fn permission_is_never_stronger_than_the_default() {
        assert_eq!(clamp_perm("", "auto"), "auto");
        assert_eq!(clamp_perm("turbo", "auto"), "auto");
        assert_eq!(clamp_perm("full", "ask"), "ask");
        assert_eq!(clamp_perm("disabled", "turbo"), "disabled");
        assert_eq!(clamp_perm("ask", "auto"), "ask");
        assert_eq!(clamp_perm("reviewer", "ask"), "reviewer");
        // An unknown value ranks as the middle rung, where it is enforced.
        assert_eq!(clamp_perm("bogus", "disabled"), "disabled");
    }

    #[test]
    fn model_is_pinned_to_the_provider() {
        let mut s = Settings {
            model: "anthropic/claude-opus-5".into(),
            ..Default::default()
        };
        s.recent_models = vec!["codex/gpt-5".into(), "claude/claude-sonnet-5".into()];
        assert_eq!(
            pick_model("claude", "", &s, &[]).as_deref(),
            Some("claude/claude-sonnet-5")
        );
        assert_eq!(
            pick_model("codex", "", &s, &[]).as_deref(),
            Some("codex/gpt-5")
        );
        // An item for another provider never runs on this one.
        assert_eq!(pick_model("codex", "claude/claude-sonnet-5", &s, &[]), None);
        let cat = vec![providers::ModelInfo {
            id: "opencode-go/kimi".into(),
            provider: "opencode-go".into(),
            ..Default::default()
        }];
        assert_eq!(
            pick_model("opencode-go", "", &s, &cat).as_deref(),
            Some("opencode-go/kimi")
        );
        s.disabled_models = vec!["codex/gpt-5".into()];
        assert_eq!(pick_model("codex", "", &s, &[]), None);
    }

    #[test]
    fn claim_next_item_reserves_the_item_across_provider_iterations() {
        let item = |id: &str| BacklogItem {
            id: id.into(),
            project: "/p".into(),
            prompt: "work".into(),
            ..Default::default()
        };
        let cfg = BurnerCfg {
            queue: vec![item("shared")],
            ..cfg()
        };
        let mut started = HashSet::new();
        assert_eq!(
            claim_next_item(&cfg, "claude", &mut started).unwrap().id,
            "shared"
        );
        assert!(claim_next_item(&cfg, "codex", &mut started).is_none());
    }

    #[test]
    fn next_item_skips_started_errored_and_foreign_items() {
        let item = |id: &str, model: &str, error: &str| BacklogItem {
            id: id.into(),
            project: "/p".into(),
            prompt: "sweep TODOs".into(),
            model: model.into(),
            error: error.into(),
            ..Default::default()
        };
        let c = BurnerCfg {
            queue: vec![
                item("a", "", ""),
                item("b", "", "boom"),
                item("c", "codex/gpt-5", ""),
                item("d", "", ""),
            ],
            ..cfg()
        };
        let started: HashSet<String> = ["a".to_string()].into();
        assert_eq!(next_item(&c, "claude", &started).unwrap().id, "d");
        assert_eq!(next_item(&c, "codex", &started).unwrap().id, "c");
    }

    #[test]
    fn settings_without_the_burner_load_with_it_off() {
        let old: Settings =
            serde_json::from_str(r#"{"ui_size":"v2","ui_zoom":110,"model":"x/y"}"#).unwrap();
        assert_eq!(old.burner, BurnerCfg::default());
        assert!(!old.burner.enabled);
        assert!(old.burner.queue.is_empty());
        // A partial object (older build of this feature, or a hand edit) keeps the defaults it lacks.
        let partial: Settings = serde_json::from_str(
            r#"{"burner":{"enabled":true,"queue":[{"prompt":"audit deps"}]}}"#,
        )
        .unwrap();
        assert!(partial.burner.enabled);
        assert_eq!(partial.burner.reset_within_min, 30);
        assert_eq!(partial.burner.queue[0].prompt, "audit deps");
        assert!(partial.burner.queue[0].perm.is_empty());
    }

    #[test]
    fn settings_and_runs_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("ol-burner-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let mut s = Settings::default();
        s.burner.enabled = true;
        s.burner.spend_cap = 75.0;
        s.burner.queue.push(BacklogItem {
            id: "q1".into(),
            project: "/p".into(),
            prompt: "sweep TODOs".into(),
            perm: "auto".into(),
            created_at: NOW,
            ..Default::default()
        });
        store::save_settings(&s);
        let back = store::load_settings();
        assert_eq!(back.burner, s.burner);
        assert!(back.unknown.is_empty(), "burner must be a known key");

        assert!(load_runs().is_empty());
        let mut runs = vec![run()];
        for i in 0..(KEEP_RUNS + 5) {
            runs.push(BurnRun {
                task_id: format!("old{i}"),
                ended_at: NOW,
                ..Default::default()
            });
        }
        save_runs(&runs);
        let back = load_runs();
        assert_eq!(back.len(), KEEP_RUNS + 1, "old finished runs are trimmed");
        assert!(back.iter().any(|r| r.task_id == "t1" && r.active()));
        assert_eq!(
            back.last().unwrap().task_id,
            format!("old{}", KEEP_RUNS + 4)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
