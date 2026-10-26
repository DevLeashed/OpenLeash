//! Scheduled / recurring agent runs.
//!
//! A schedule is a saved prompt plus a timing rule. When it comes due the
//! scheduler starts an ordinary chat with it — the same `create_task` path the
//! composer and the chat connectors use — so a scheduled run is subject to
//! exactly the permission mode it was saved with. Nothing here approves
//! anything: a run that needs the user waits in "Needs you" like any other.
//!
//! Layout: the recurrence maths ([`next_after`]), the per-tick decision
//! ([`decide`], [`plan_tick`]) and the draft validation ([`build`]) are pure and
//! tested without Tauri or the machine's time zone. [`Scheduler`] is the thin
//! async layer that owns `schedules.json`, fires runs and publishes
//! `ol://schedules`.

use super::store;
use super::Harness;
use chrono::{
    DateTime, Datelike, Days, Local, LocalResult, NaiveDateTime, NaiveTime, TimeDelta, TimeZone,
    Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The global event every mutation, fire and skip publishes. Payload: the full
/// [`SchedulesState`], so a client never has to merge partial updates.
pub const EVENT: &str = "ol://schedules";
/// Newest-first run records kept per schedule.
pub const HISTORY_CAP: usize = 20;
/// How late a slot may be and still count as "on time". The loop ticks every
/// 30s, so anything later than this means the app was closed or the machine
/// was asleep, which is what `catch_up` is about.
pub const GRACE: TimeDelta = TimeDelta::minutes(5);
/// Shortest and longest interval a schedule may use, in minutes.
pub const MIN_INTERVAL_MINUTES: i64 = 5;
pub const MAX_INTERVAL_MINUTES: i64 = 7 * 24 * 60;
/// Delay before the first tick: long enough for tasks to be loaded and the
/// trust table published, short enough that a catch-up run is prompt.
const FIRST_TICK: std::time::Duration = std::time::Duration::from_secs(12);
const TICK: std::time::Duration = std::time::Duration::from_secs(30);

pub const MSG_MISSED: &str = "Missed while OpenLeash was closed";
pub const MSG_ACTIVE: &str = "Previous run still active";
pub const MSG_PAUSED_ALL: &str = "OpenLeash is paused";
pub const MSG_UNTRUSTED: &str = "Project folder hasn't been trusted yet — open it in OpenLeash and answer the trust prompt first.";
pub const MSG_STARTED: &str = "Started a new chat";

/// Permission modes a schedule may be saved with (the composer's set plus the
/// legacy "ask" spelling). Anything else is rejected rather than left to the
/// fail-closed fallback, so a typo surfaces when saving, not at 3 a.m.
const PERMS: &[&str] = &[
    "disabled",
    "allowlist",
    "reviewer",
    "auto",
    "turbo",
    "full",
    "ask",
];
const DAYS: &[(&str, Weekday)] = &[
    ("mon", Weekday::Mon),
    ("tue", Weekday::Tue),
    ("wed", Weekday::Wed),
    ("thu", Weekday::Thu),
    ("fri", Weekday::Fri),
    ("sat", Weekday::Sat),
    ("sun", Weekday::Sun),
];

// ───────────────────────────── wire types ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Timing {
    /// Every `every` minutes/hours, anchored so the grid never drifts.
    Interval { every: u32, unit: String },
    /// Every day at local wall-clock `time` ("HH:MM", 24h).
    Daily { time: String },
    /// On the given days ("mon".."sun") at local wall-clock `time`.
    Weekly { days: Vec<String>, time: String },
}

impl Default for Timing {
    fn default() -> Self {
        Timing::Daily {
            time: "09:00".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub at: DateTime<Utc>,
    /// started | skipped | failed
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub task_id: Option<String>,
}

fn yes() -> bool {
    true
}

fn local_workspace() -> String {
    "local".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub prompt: String,
    pub project: String,
    pub model: String,
    pub effort: usize,
    #[serde(
        default = "super::permissions::default_perm",
        deserialize_with = "super::permissions::deserialize_perm"
    )]
    pub perm: String,
    pub plan: bool,
    /// local | worktree
    #[serde(default = "local_workspace")]
    pub workspace: String,
    pub base_branch: String,
    pub timing: Timing,
    /// Run once on startup for slots missed while the app was closed.
    #[serde(default = "yes")]
    pub catch_up: bool,
    pub created_at: DateTime<Utc>,
    // Run state: owned by the server, ignored when a client sends a draft.
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    /// "" | started | skipped | failed
    pub last_status: String,
    pub last_message: String,
    pub last_task_id: Option<String>,
    /// Newest first, at most [`HISTORY_CAP`].
    pub history: Vec<RunRecord>,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            id: String::new(),
            name: String::new(),
            enabled: true,
            prompt: String::new(),
            project: String::new(),
            model: String::new(),
            effort: 0,
            perm: super::permissions::default_perm(),
            plan: false,
            workspace: local_workspace(),
            base_branch: String::new(),
            timing: Timing::default(),
            catch_up: true,
            created_at: Utc::now(),
            next_run_at: None,
            last_run_at: None,
            last_status: String::new(),
            last_message: String::new(),
            last_task_id: None,
            history: vec![],
        }
    }
}

/// Everything the Schedules screen shows; also the on-disk shape of
/// `schedules.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulesState {
    /// Monotonic server revision. Clients reject out-of-order snapshots.
    pub revision: u64,
    /// Global pause: nothing fires on time while set ("Run now" still works).
    pub paused: bool,
    pub schedules: Vec<Schedule>,
}

/// What a client sends to `schedule_save`: the same JSON as [`Schedule`], with
/// the run-state fields ignored. Optional fields are optional so "absent" can
/// mean "use the user's current default" rather than a hardcoded one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ScheduleDraft {
    /// Empty or absent = create.
    pub id: Option<String>,
    pub name: String,
    pub enabled: Option<bool>,
    pub prompt: String,
    pub project: String,
    pub model: Option<String>,
    pub effort: Option<usize>,
    pub perm: Option<String>,
    pub plan: bool,
    pub workspace: Option<String>,
    pub base_branch: String,
    pub timing: Option<Timing>,
    pub catch_up: Option<bool>,
}

/// The user's current defaults, applied to a draft that leaves them out.
#[derive(Debug, Clone)]
pub struct Defaults {
    pub perm: String,
    pub model: String,
    pub effort: usize,
}

// ───────────────────────────── recurrence ─────────────────────────────

/// Parse "HH:MM" (24h, hour may be one digit).
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    if !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn parse_day(s: &str) -> Option<Weekday> {
    let s = s.trim().to_ascii_lowercase();
    DAYS.iter().find(|(n, _)| *n == s).map(|(_, d)| *d)
}

/// An interval's period in minutes, or `None` for an unknown unit.
fn interval_minutes(every: u32, unit: &str) -> Option<i64> {
    match unit {
        "minutes" => Some(i64::from(every)),
        "hours" => Some(i64::from(every) * 60),
        _ => None,
    }
}

/// The earliest instant of a local result: the single answer, the earlier
/// reading of a fold (clocks went back, so the time happens twice — run on the
/// first), or nothing for a gap (clocks went forward over it).
pub fn earliest<T>(r: LocalResult<T>) -> Option<T> {
    r.earliest()
}

/// Map a local wall-clock time to an instant. A time inside a DST gap does not
/// exist that day, so it moves to the first valid wall-time after the gap,
/// independent of the requested minute (DST gaps are typically 30 or 60 minutes).
pub fn resolve_local<Tz: TimeZone>(tz: &Tz, wall: NaiveDateTime) -> Option<DateTime<Tz>> {
    (0..=180).find_map(|minutes| {
        let t = wall.checked_add_signed(TimeDelta::minutes(minutes))?;
        earliest(tz.from_local_datetime(&t))
    })
}

/// The first slot of `timing` strictly after `after`.
///
/// `anchor` only matters for intervals: slots are `anchor + k·period` (k ≥ 1),
/// computed arithmetically so a large gap costs nothing and the grid never
/// drifts with tick latency. Daily/weekly times are wall-clock times in
/// `after`'s zone (production passes `chrono::Local`).
pub fn next_after<Tz: TimeZone>(
    timing: &Timing,
    anchor: &DateTime<Utc>,
    after: &DateTime<Tz>,
) -> Option<DateTime<Tz>> {
    match timing {
        Timing::Interval { every, unit } => {
            let period = interval_minutes(*every, unit)?.checked_mul(60)?;
            if period <= 0 {
                return None;
            }
            let after_utc = after.with_timezone(&Utc);
            let diff = (after_utc - *anchor).num_seconds();
            // Truncating `diff` to whole seconds keeps the result strictly after:
            // anchor + (⌊diff/p⌋ + 1)·p ≥ anchor + diff + 1s > after.
            let k = if diff < 0 { 1 } else { diff / period + 1 };
            let at = anchor.checked_add_signed(TimeDelta::try_seconds(k.checked_mul(period)?)?)?;
            Some(at.with_timezone(&after.timezone()))
        }
        Timing::Daily { time } => next_wall(after, parse_time(time)?, None),
        Timing::Weekly { days, time } => {
            let days: Vec<Weekday> = days.iter().filter_map(|d| parse_day(d)).collect();
            if days.is_empty() {
                return None;
            }
            next_wall(after, parse_time(time)?, Some(&days))
        }
    }
}

fn next_wall<Tz: TimeZone>(
    after: &DateTime<Tz>,
    time: NaiveTime,
    days: Option<&[Weekday]>,
) -> Option<DateTime<Tz>> {
    let tz = after.timezone();
    let start = after.naive_local().date();
    // Eight days covers "same weekday next week" plus a gap shift.
    for i in 0..=8 {
        let day = start.checked_add_days(Days::new(i))?;
        if days.is_some_and(|ds| !ds.contains(&day.weekday())) {
            continue;
        }
        if let Some(at) = resolve_local(&tz, day.and_time(time)) {
            if at > *after {
                return Some(at);
            }
        }
    }
    None
}

// ───────────────────────────── decisions ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to do this tick.
    Idle,
    /// No `next_run_at` yet (new or legacy): compute it, don't fire.
    Init,
    /// Due, but late beyond [`GRACE`] with catch-up off.
    Missed,
    /// Due, but must not start (message says why).
    Skip(String),
    Fire,
}

/// What one tick does with one schedule. Pure.
pub fn decide(s: &Schedule, paused: bool, now: DateTime<Utc>, active: bool) -> Decision {
    if !s.enabled || paused {
        return Decision::Idle;
    }
    let Some(next) = s.next_run_at else {
        return Decision::Init;
    };
    if next > now {
        return Decision::Idle;
    }
    if !s.catch_up && now - next > GRACE {
        return Decision::Missed;
    }
    if active {
        return Decision::Skip(MSG_ACTIVE.into());
    }
    Decision::Fire
}

/// Move `next_run_at` to the first slot after `now`. Missed slots are never
/// iterated, so a catch-up is at most one run however long the app was shut.
/// Intervals stay on the grid of the previous slot (or of the last run /
/// creation for a schedule that has none yet).
pub fn advance<Tz: TimeZone>(s: &mut Schedule, now: &DateTime<Tz>) {
    let anchor = s.next_run_at.or(s.last_run_at).unwrap_or(s.created_at);
    s.next_run_at = next_after(&s.timing, &anchor, now).map(|t| t.with_timezone(&Utc));
}

/// Start the timing afresh from `now` (saved with a new timing, re-enabled,
/// or un-paused): an interval counts from now, and no stale slot fires.
pub fn rearm<Tz: TimeZone>(s: &mut Schedule, now: &DateTime<Tz>) {
    s.next_run_at = if s.enabled {
        let anchor = now.with_timezone(&Utc);
        next_after(&s.timing, &anchor, now).map(|t| t.with_timezone(&Utc))
    } else {
        None
    };
}

/// Append a run record (newest first, capped) and mirror it into `last_*`.
/// `last_task_id` only moves when a chat was actually created, so a skip
/// keeps pointing at the run that is still active.
pub fn record(
    s: &mut Schedule,
    at: DateTime<Utc>,
    status: &str,
    message: &str,
    task_id: Option<String>,
) {
    s.last_run_at = Some(at);
    s.last_status = status.into();
    s.last_message = message.into();
    if task_id.is_some() {
        s.last_task_id = task_id.clone();
    }
    s.history.insert(
        0,
        RunRecord {
            at,
            status: status.into(),
            message: message.into(),
            task_id,
        },
    );
    s.history.truncate(HISTORY_CAP);
}

/// One tick over every schedule. `active` says, per schedule id, whether its
/// last run is still going. Returns whether anything changed and the ids to
/// fire (their `next_run_at` has already been advanced, so a failed fire
/// never repeats on the next tick).
pub fn plan_tick<Tz: TimeZone>(
    st: &mut SchedulesState,
    now: &DateTime<Tz>,
    active: &HashMap<String, bool>,
) -> (bool, Vec<String>) {
    let now_utc = now.with_timezone(&Utc);
    let mut changed = false;
    let mut fire = vec![];
    for s in st.schedules.iter_mut() {
        let busy = active.get(&s.id).copied().unwrap_or(false);
        match decide(s, st.paused, now_utc, busy) {
            Decision::Idle => continue,
            Decision::Init => {}
            Decision::Missed => record(s, now_utc, "skipped", MSG_MISSED, None),
            Decision::Skip(msg) => record(s, now_utc, "skipped", &msg, None),
            Decision::Fire => fire.push(s.id.clone()),
        }
        advance(s, now);
        changed = true;
    }
    (changed, fire)
}

/// Whether a chat counts as "still running" for the overlap check.
///
/// Active means the run has not finished and may still continue on its own or
/// on the user's answer: status `running` or `waiting` (an approval/question
/// is open — the run resumes once answered), any `waiting_kind`, or a live
/// pause (`manual` / `exhausted` / `error` freeze a run mid-turn and resume
/// it). A `closed` pause is *not* active: `load_tasks` stamps it on a run the
/// app was closed during, and only a manual Resume revives it, so counting it
/// would block the schedule forever after one restart. `idle`, `done`,
/// `failed` and `stopped` are finished. The runtime flags (`running`,
/// `startup_pending`) are checked separately by the caller.
pub fn task_is_active(status: &str, waiting: bool, pause_kind: Option<&str>) -> bool {
    matches!(status, "running" | "waiting") || waiting || pause_kind.is_some_and(|k| k != "closed")
}

// ───────────────────────────── validation ─────────────────────────────

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Validate and normalize a timing.
pub fn normalize_timing(t: &Timing) -> Result<Timing, String> {
    let time = |s: &str| {
        parse_time(s)
            .map(|t| t.format("%H:%M").to_string())
            .ok_or_else(|| format!("\"{s}\" isn't a valid time — use 24-hour HH:MM, e.g. 09:00."))
    };
    match t {
        Timing::Interval { every, unit } => {
            let unit = unit.trim().to_ascii_lowercase();
            let mins = interval_minutes(*every, &unit).ok_or_else(|| {
                format!("Unknown interval unit \"{unit}\" — use minutes or hours.")
            })?;
            if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&mins) {
                return Err("The interval must be between 5 minutes and 7 days.".into());
            }
            Ok(Timing::Interval {
                every: *every,
                unit,
            })
        }
        Timing::Daily { time: t } => Ok(Timing::Daily { time: time(t)? }),
        Timing::Weekly { days, time: t } => {
            let mut picked = vec![];
            for d in days {
                let w = parse_day(d).ok_or_else(|| {
                    format!("\"{d}\" isn't a day — use mon, tue, wed, thu, fri, sat or sun.")
                })?;
                if !picked.contains(&w) {
                    picked.push(w);
                }
            }
            if picked.is_empty() {
                return Err("Pick at least one day of the week.".into());
            }
            let days = DAYS
                .iter()
                .filter(|(_, w)| picked.contains(w))
                .map(|(n, _)| n.to_string())
                .collect();
            Ok(Timing::Weekly {
                days,
                time: time(t)?,
            })
        }
    }
}

/// Turn a client draft into a schedule: validate, fill defaults, and carry
/// the server-owned run state over from `existing` on an update. `next_run_at`
/// is recomputed when the timing changes or the schedule is (re-)enabled.
pub fn build<Tz: TimeZone>(
    existing: Option<&Schedule>,
    d: ScheduleDraft,
    defaults: &Defaults,
    now: &DateTime<Tz>,
) -> Result<Schedule, String> {
    let prompt = d.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Write a prompt for the scheduled run.".into());
    }
    let project = d.project.trim().to_string();
    if project.is_empty() {
        return Err("Pick a project folder.".into());
    }
    let name = match d.name.trim() {
        "" => clip(
            prompt
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim(),
            60,
        ),
        n => n.to_string(),
    };
    let perm = match d.perm.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => defaults.perm.clone(),
    };
    if !PERMS.contains(&perm.as_str()) {
        return Err(format!("Unknown permission mode \"{perm}\"."));
    }
    let model = match d.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => defaults.model.clone(),
    };
    if model.is_empty() {
        return Err("Pick a model for the scheduled run.".into());
    }
    let workspace = match d.workspace.as_deref().map(str::trim) {
        None | Some("") => "local".to_string(),
        Some(w @ ("local" | "worktree")) => w.to_string(),
        Some(w) => {
            return Err(format!(
                "Unknown workspace \"{w}\" — scheduled runs use local or worktree."
            ))
        }
    };
    let timing = normalize_timing(d.timing.as_ref().ok_or("Choose when the schedule runs.")?)?;
    let mut s = existing.cloned().unwrap_or_else(|| Schedule {
        id: super::new_id(),
        created_at: now.with_timezone(&Utc),
        next_run_at: None,
        ..Schedule::default()
    });
    let was_enabled = existing.is_some_and(|e| e.enabled);
    let old_timing = existing.map(|e| e.timing.clone());
    s.name = name;
    s.enabled = d.enabled.unwrap_or(existing.is_none_or(|e| e.enabled));
    s.prompt = prompt;
    s.project = project;
    s.model = model;
    s.effort = d.effort.unwrap_or(defaults.effort).min(4);
    s.perm = perm;
    s.plan = d.plan;
    s.workspace = workspace;
    s.base_branch = d.base_branch.trim().to_string();
    s.catch_up = d.catch_up.unwrap_or(existing.is_none_or(|e| e.catch_up));
    s.timing = timing;
    if !s.enabled {
        s.next_run_at = None;
    } else if !was_enabled || old_timing.as_ref() != Some(&s.timing) || s.next_run_at.is_none() {
        rearm(&mut s, now);
    }
    Ok(s)
}

// ───────────────────────────── persistence ─────────────────────────────

pub fn path() -> PathBuf {
    store::data_dir().join("schedules.json")
}

/// Load a schedules file. Missing → empty. Unreadable JSON → empty, with the
/// bad file moved aside to `schedules.json.bad` so the next save cannot
/// silently destroy what the user had.
pub fn load_from(path: &Path) -> SchedulesState {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SchedulesState::default(),
        Err(e) => {
            eprintln!(
                "[openleash] schedules: could not read {}: {e}",
                path.display()
            );
            return SchedulesState::default();
        }
    };
    match serde_json::from_str::<SchedulesState>(&text) {
        Ok(mut st) => {
            for s in st.schedules.iter_mut() {
                s.history.truncate(HISTORY_CAP);
            }
            st
        }
        Err(e) => {
            let bad = path.with_extension("json.bad");
            eprintln!(
                "[openleash] schedules: {} is not valid ({e}); moved it to {} and starting empty",
                path.display(),
                bad.display()
            );
            if let Err(e) = std::fs::rename(path, &bad) {
                eprintln!("[openleash] schedules: could not back up the bad file: {e}");
            }
            SchedulesState::default()
        }
    }
}

pub fn load() -> SchedulesState {
    load_from(&path())
}

pub fn save(st: &SchedulesState) {
    match serde_json::to_string_pretty(st) {
        Ok(text) => store::write_atomic(&path(), &text),
        Err(e) => eprintln!("[openleash] schedules: could not serialize: {e}"),
    }
}

// ───────────────────────────── the scheduler ─────────────────────────────

/// The settings a fire needs, read once.
struct FireEnv {
    paused_all: bool,
    decided: bool,
    assist: String,
    agents: Vec<String>,
}

pub struct Scheduler {
    h: Arc<Harness>,
    state: Mutex<SchedulesState>,
    /// Serializes firing (timed ticks and "Run now") so the overlap check and
    /// the chat it guards happen as one step: two fires of one schedule can
    /// never both see "not active". Mutating commands only take `state`, which
    /// is never held across `create_task`.
    firing: Mutex<()>,
}

impl Scheduler {
    pub fn new(h: Arc<Harness>) -> Arc<Scheduler> {
        Arc::new(Scheduler {
            h,
            state: Mutex::new(load()),
            firing: Mutex::new(()),
        })
    }

    /// Load the schedules and start the loop. Enabled schedules run whether or
    /// not the Schedules screen is shown (`Settings::show_schedules`).
    pub fn start(h: Arc<Harness>) -> Arc<Scheduler> {
        let me = Scheduler::new(h);
        let looped = me.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(FIRST_TICK).await;
            loop {
                looped.tick().await;
                tokio::time::sleep(TICK).await;
            }
        });
        me
    }

    /// Persist and publish. Called with `state` held so writes land in order.
    fn commit(&self, st: &mut SchedulesState) {
        st.revision = st.revision.wrapping_add(1);
        save(st);
        (self.h.bus)(EVENT, serde_json::to_value(&*st).unwrap_or_default());
    }

    pub async fn list(&self) -> SchedulesState {
        self.state.lock().await.clone()
    }

    async fn defaults(&self) -> Defaults {
        let s = self.h.settings.read().await;
        Defaults {
            perm: s.perm.clone(),
            model: s
                .recent_models
                .first()
                .cloned()
                .unwrap_or_else(|| s.model.clone()),
            effort: s.effort,
        }
    }

    pub async fn save_draft(&self, d: ScheduleDraft) -> Result<Schedule, String> {
        let defaults = self.defaults().await;
        let now = Local::now();
        let mut st = self.state.lock().await;
        let id = d.id.clone().filter(|i| !i.trim().is_empty());
        let out = match id {
            Some(id) => {
                let slot = st
                    .schedules
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or("That schedule no longer exists.")?;
                let next = build(Some(slot), d, &defaults, &now)?;
                *slot = next.clone();
                next
            }
            None => {
                let s = build(None, d, &defaults, &now)?;
                st.schedules.push(s.clone());
                s
            }
        };
        self.commit(&mut st);
        Ok(out)
    }

    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let mut st = self.state.lock().await;
        let before = st.schedules.len();
        st.schedules.retain(|s| s.id != id);
        if st.schedules.len() == before {
            return Err("That schedule no longer exists.".into());
        }
        self.commit(&mut st);
        Ok(())
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Schedule, String> {
        let now = Local::now();
        let mut st = self.state.lock().await;
        let s = st
            .schedules
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or("That schedule no longer exists.")?;
        if s.enabled != enabled || (enabled && s.next_run_at.is_none()) {
            s.enabled = enabled;
            rearm(s, &now);
        }
        let out = s.clone();
        self.commit(&mut st);
        Ok(out)
    }

    pub async fn set_paused(&self, paused: bool) -> SchedulesState {
        let now = Local::now();
        let mut st = self.state.lock().await;
        if st.paused && !paused {
            // Slots that passed while paused were deliberately not run; don't
            // let un-pausing turn them into a burst of catch-up runs.
            for s in st.schedules.iter_mut() {
                rearm(s, &now);
            }
        }
        st.paused = paused;
        self.commit(&mut st);
        st.clone()
    }

    /// Fire one schedule now: same overlap/trust checks as a timed fire, but
    /// `next_run_at` is untouched and it works while disabled or paused.
    pub async fn run_now(&self, id: &str) -> Result<Schedule, String> {
        let _f = self.firing.lock().await;
        self.fire(id, false)
            .await?
            .ok_or_else(|| "That schedule no longer exists.".into())
    }

    async fn reserve_timed(&self, id: &str) -> bool {
        // Locking state is the linearization point shared with disable and
        // pause. After releasing it, a mutation is ordered after this run's
        // reservation and does not retroactively cancel startup.
        let st = self.state.lock().await;
        let Some(schedule) = st.schedules.iter().find(|s| s.id == id) else {
            return false;
        };
        !st.paused && schedule.enabled
    }

    /// One pass of the loop.
    pub async fn tick(&self) {
        let _f = self.firing.lock().await;
        let ids: Vec<(String, Option<String>)> = {
            let st = self.state.lock().await;
            st.schedules
                .iter()
                .map(|s| (s.id.clone(), s.last_task_id.clone()))
                .collect()
        };
        // Task locks are taken outside `state` so a busy chat never stalls the
        // Schedules screen.
        let mut active = HashMap::new();
        for (id, task) in ids {
            active.insert(id, self.is_active(task.as_deref()).await);
        }
        let fire = {
            let mut st = self.state.lock().await;
            let (changed, fire) = plan_tick(&mut st, &Local::now(), &active);
            if changed {
                self.commit(&mut st);
            }
            fire
        };
        for id in fire {
            // A schedule deleted in between is simply gone; the timed path
            // revalidates enabled/paused at the actual start reservation.
            let _ = self.fire(&id, true).await;
        }
    }

    /// Whether a chat still owns live work for the overlap check. Runtime
    /// activity counts independently of the main chat status: detached subagents
    /// and foreground/background commands can outlive a completed main turn.
    /// A cancelled subagent token is finished; an uncancelled token still owns
    /// live work even if its Task::Sub status was already persisted as stopped.
    async fn is_active(&self, task_id: Option<&str>) -> bool {
        let Some(id) = task_id else {
            return false;
        };
        if let Some(rt) = self.h.runtime_if(id) {
            if rt.running.load(Ordering::SeqCst)
                || rt.startup_pending.load(Ordering::SeqCst)
                || rt.fg.load(Ordering::SeqCst) > 0
                || rt
                    .sub_cancel
                    .lock()
                    .unwrap()
                    .values()
                    .any(|cancel| !cancel.is_cancelled())
                || self.h.bg.list(id).iter().any(|job| job.running)
            {
                return true;
            }
        }
        // `task_ref`, not `task`: the status lives in the header, so there is
        // no reason to hydrate a large transcript just to look at it. A
        // deleted chat is not active.
        let Ok(t) = self.h.task_ref(id).await else {
            return false;
        };
        let t = t.lock().await;
        task_is_active(
            &t.status,
            t.waiting_kind.is_some(),
            t.paused.as_ref().map(|p| p.kind.as_str()),
        )
    }

    async fn env(&self, project: &str) -> FireEnv {
        let s = self.h.settings.read().await;
        FireEnv {
            paused_all: s.paused_all,
            // The settings record, not the published snapshot: it is the
            // source of truth and is loaded before this loop starts.
            decided: super::trust::decided(&s.trust, project),
            assist: s.assist.clone(),
            agents: s.default_agents.clone(),
        }
    }

    /// Check, reserve, start the chat and record the outcome. Caller holds
    /// `firing`. Timed work is rechecked after asynchronous trust/overlap reads;
    /// the final state-lock check is its linearization point. A later disable
    /// or pause may proceed, but cannot cancel a run already reserved.
    async fn fire(&self, id: &str, timed: bool) -> Result<Option<Schedule>, String> {
        let Some(s) = self
            .state
            .lock()
            .await
            .schedules
            .iter()
            .find(|s| s.id == id)
            .cloned()
        else {
            return if timed {
                Ok(None)
            } else {
                Err("That schedule no longer exists.".into())
            };
        };
        let env = self.env(&s.project).await;
        let active = self.is_active(s.last_task_id.as_deref()).await;
        // Recheck after both awaited reads, even if the outcome will be a
        // skip/failure: a queued timed decision that lost eligibility should
        // not record against the schedule's new state.
        if timed && !self.reserve_timed(id).await {
            return Ok(None);
        }
        let (status, message, task_id) = if env.paused_all {
            ("skipped", MSG_PAUSED_ALL.to_string(), None)
        } else if active {
            ("skipped", MSG_ACTIVE.to_string(), None)
        } else if !env.decided {
            // Never decided = never asked. An explicit "don't trust" is the
            // user's decision and runs, without project context, exactly as
            // an interactive chat would.
            ("failed", MSG_UNTRUSTED.to_string(), None)
        } else {
            match self.create(&s, &env).await {
                Ok(tid) => ("started", MSG_STARTED.to_string(), Some(tid)),
                Err(e) => ("failed", e, None),
            }
        };
        let mut st = self.state.lock().await;
        let slot = st
            .schedules
            .iter_mut()
            .find(|x| x.id == id)
            .ok_or("That schedule was deleted while it was starting.")?;
        record(slot, Utc::now(), status, &message, task_id);
        let out = slot.clone();
        self.commit(&mut st);
        Ok(Some(out))
    }

    /// Start the chat, the way the chat connectors do: headless `create_task`,
    /// then a summary event so the app learns about a chat it didn't create.
    async fn create(&self, s: &Schedule, env: &FireEnv) -> Result<String, String> {
        let req = crate::NewTask {
            prompt: s.prompt.clone(),
            project: s.project.clone(),
            model: s.model.clone(),
            effort: s.effort,
            perm: s.perm.clone(),
            plan: s.plan,
            worktree: s.workspace == "worktree",
            workspace: Some(s.workspace.clone()),
            base_branch: s.base_branch.clone(),
            assist: env.assist.clone(),
            agents: (!env.agents.is_empty()).then(|| env.agents.clone()),
            route: None,
            images: vec![],
            files: vec![],
            image_names: vec![],
            ultra: false,
            ultra_wt: false,
            ultra_x: None,
        };
        let display = format!("⏰ Scheduled run · {}\n\n{}", s.name, s.prompt);
        let summary = crate::create_task_with_options(
            self.h.clone(),
            req,
            Some(display),
            crate::CreateTaskOptions::Background,
            crate::TaskCreationPolicy::default(),
        )
        .await?;
        if let Ok(t) = self.h.task(&summary.id).await {
            {
                let mut g = t.lock().await;
                g.title = clip(&format!("⏰ {}", s.name), 70);
                // Locks the name like a user rename does, so the agent's own
                // `set_title` can't drop the scheduled-run marker.
                g.titled = true;
            }
            // Outside the task lock: the saver takes its own lock order.
            self.h.save_task(&summary.id).await;
            self.h.emit_summary(&*t.lock().await);
        }
        Ok(summary.id)
    }
}

// ───────────────────────────── commands ─────────────────────────────

type S<'a> = tauri::State<'a, Arc<Scheduler>>;

#[tauri::command]
pub async fn schedules_list(s: S<'_>) -> Result<SchedulesState, String> {
    Ok(s.list().await)
}

#[tauri::command]
pub async fn schedule_save(s: S<'_>, schedule: ScheduleDraft) -> Result<Schedule, String> {
    s.save_draft(schedule).await
}

#[tauri::command]
pub async fn schedule_delete(s: S<'_>, id: String) -> Result<(), String> {
    s.delete(&id).await
}

#[tauri::command]
pub async fn schedule_set_enabled(s: S<'_>, id: String, enabled: bool) -> Result<Schedule, String> {
    s.set_enabled(&id, enabled).await
}

#[tauri::command]
pub async fn schedule_run_now(s: S<'_>, id: String) -> Result<Schedule, String> {
    s.run_now(&id).await
}

#[tauri::command]
pub async fn schedules_set_paused(s: S<'_>, paused: bool) -> Result<SchedulesState, String> {
    Ok(s.set_paused(paused).await)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]
    use super::*;
    use chrono::{FixedOffset, NaiveDate, Offset};

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn fixed(hours: i32) -> FixedOffset {
        FixedOffset::east_opt(hours * 3600).unwrap()
    }

    fn interval(every: u32, unit: &str) -> Timing {
        Timing::Interval {
            every,
            unit: unit.into(),
        }
    }

    fn daily(t: &str) -> Timing {
        Timing::Daily { time: t.into() }
    }

    fn weekly(days: &[&str], t: &str) -> Timing {
        Timing::Weekly {
            days: days.iter().map(|d| d.to_string()).collect(),
            time: t.into(),
        }
    }

    fn sched(timing: Timing) -> Schedule {
        Schedule {
            id: "s1".into(),
            name: "n".into(),
            prompt: "p".into(),
            project: "/proj".into(),
            model: "m".into(),
            timing,
            created_at: utc(2026, 1, 1, 0, 0),
            ..Schedule::default()
        }
    }

    // ── next_after: interval ──

    #[test]
    fn interval_slots_sit_on_the_anchor_grid_and_are_strictly_after() {
        let anchor = utc(2026, 5, 1, 10, 0);
        let t = interval(30, "minutes");
        // Exactly on a slot → the next one, not the same one.
        let at = utc(2026, 5, 1, 11, 0);
        assert_eq!(
            next_after(&t, &anchor, &at).unwrap(),
            utc(2026, 5, 1, 11, 30)
        );
        // Between slots → the next grid point, not `after + period`.
        let at = utc(2026, 5, 1, 11, 7) + TimeDelta::seconds(13);
        assert_eq!(
            next_after(&t, &anchor, &at).unwrap(),
            utc(2026, 5, 1, 11, 30)
        );
        // Before the anchor → the first slot after it.
        let at = utc(2026, 4, 1, 0, 0);
        assert_eq!(
            next_after(&t, &anchor, &at).unwrap(),
            utc(2026, 5, 1, 10, 30)
        );
        // Hours.
        let t = interval(2, "hours");
        let at = utc(2026, 5, 1, 13, 0);
        assert_eq!(
            next_after(&t, &anchor, &at).unwrap(),
            utc(2026, 5, 1, 14, 0)
        );
        // Sub-second past a slot still moves on.
        let at = utc(2026, 5, 1, 14, 0) + TimeDelta::milliseconds(1);
        assert_eq!(
            next_after(&t, &anchor, &at).unwrap(),
            utc(2026, 5, 1, 16, 0)
        );
    }

    #[test]
    fn interval_does_not_drift_when_ticks_are_late() {
        // Fire late each time and advance from the previous slot: the grid holds.
        let mut s = sched(interval(30, "minutes"));
        s.next_run_at = Some(utc(2026, 5, 1, 10, 0));
        for i in 1..=10 {
            let late = s.next_run_at.unwrap() + TimeDelta::seconds(29);
            advance(&mut s, &late);
            assert_eq!(
                s.next_run_at.unwrap(),
                utc(2026, 5, 1, 10, 0) + TimeDelta::minutes(30 * i)
            );
        }
    }

    #[test]
    fn interval_large_gap_is_arithmetic() {
        let anchor = utc(2000, 1, 1, 0, 0);
        let t = interval(7, "minutes");
        let at = utc(2026, 5, 1, 10, 3);
        let start = std::time::Instant::now();
        let next = next_after(&t, &anchor, &at).unwrap();
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
        assert!(next > at && next - at <= TimeDelta::minutes(7));
        assert_eq!((next - anchor).num_seconds() % (7 * 60), 0);
    }

    #[test]
    fn interval_result_keeps_the_callers_zone() {
        let anchor = utc(2026, 5, 1, 10, 0);
        let at = utc(2026, 5, 1, 10, 10).with_timezone(&fixed(9));
        let next = next_after(&interval(15, "minutes"), &anchor, &at).unwrap();
        assert_eq!(next.offset().fix(), fixed(9));
        assert_eq!(next.with_timezone(&Utc), utc(2026, 5, 1, 10, 15));
    }

    // ── next_after: daily / weekly ──

    #[test]
    fn daily_is_today_before_the_time_and_tomorrow_after_it() {
        for off in [fixed(0), fixed(-7), fixed(5)] {
            let anchor = utc(2026, 1, 1, 0, 0);
            let local = |d, h, m| off.with_ymd_and_hms(2026, 5, d, h, m, 0).unwrap();
            let t = daily("09:00");
            assert_eq!(
                next_after(&t, &anchor, &local(4, 8, 59)).unwrap(),
                local(4, 9, 0)
            );
            // Exactly at the time is not "after".
            assert_eq!(
                next_after(&t, &anchor, &local(4, 9, 0)).unwrap(),
                local(5, 9, 0)
            );
            assert_eq!(
                next_after(&t, &anchor, &local(4, 23, 0)).unwrap(),
                local(5, 9, 0)
            );
        }
    }

    #[test]
    fn daily_is_wall_clock_in_the_given_zone() {
        // The same instant, two zones: each fires at *its* 09:00.
        let at = utc(2026, 5, 4, 12, 0);
        let anchor = utc(2026, 1, 1, 0, 0);
        let east = next_after(&daily("09:00"), &anchor, &at.with_timezone(&fixed(2))).unwrap();
        let west = next_after(&daily("09:00"), &anchor, &at.with_timezone(&fixed(-5))).unwrap();
        assert_eq!(east.with_timezone(&Utc), utc(2026, 5, 5, 7, 0));
        assert_eq!(west.with_timezone(&Utc), utc(2026, 5, 4, 14, 0));
    }

    #[test]
    fn weekly_handles_same_day_later_multiple_days_and_week_wrap() {
        let anchor = utc(2026, 1, 1, 0, 0);
        let off = fixed(1);
        // 2026-05-04 is a Monday.
        let local = |d, h, m| off.with_ymd_and_hms(2026, 5, d, h, m, 0).unwrap();
        assert_eq!(local(4, 0, 0).weekday(), Weekday::Mon);
        let t = weekly(&["mon", "fri"], "18:30");
        // Same day, later.
        assert_eq!(
            next_after(&t, &anchor, &local(4, 10, 0)).unwrap(),
            local(4, 18, 30)
        );
        // After Monday's slot → Friday.
        assert_eq!(
            next_after(&t, &anchor, &local(4, 18, 30)).unwrap(),
            local(8, 18, 30)
        );
        // After Friday's slot → wraps to next Monday.
        assert_eq!(
            next_after(&t, &anchor, &local(8, 19, 0)).unwrap(),
            local(11, 18, 30)
        );
        // A single day, just past it → a full week later.
        let t = weekly(&["mon"], "09:00");
        assert_eq!(
            next_after(&t, &anchor, &local(4, 9, 1)).unwrap(),
            local(11, 9, 0)
        );
        // Sunday wraps across the month boundary too.
        let t = weekly(&["sun"], "07:00");
        let sat = off.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        assert_eq!(
            next_after(&t, &anchor, &sat).unwrap(),
            off.with_ymd_and_hms(2026, 5, 31, 7, 0, 0).unwrap()
        );
        // No valid day → no slot.
        assert!(next_after(&weekly(&[], "07:00"), &anchor, &sat).is_none());
    }

    // ── DST: a tiny zone with one gap and one fold ──

    /// Central-European-like rules for 2026: +1, +2 from 2026-03-29 01:00 UTC
    /// (local 02:00 → 03:00, a gap), back to +1 at 2026-10-25 01:00 UTC (local
    /// 03:00 → 02:00, a fold).
    #[derive(Debug, Clone, Copy)]
    struct Dst;

    fn spring() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 3, 29)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap()
    }
    fn autumn() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap()
    }

    impl TimeZone for Dst {
        type Offset = FixedOffset;
        fn from_offset(_: &FixedOffset) -> Self {
            Dst
        }
        fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<FixedOffset> {
            self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<FixedOffset> {
            let gap_start = spring() + TimeDelta::hours(1); // local 02:00
            let gap_end = spring() + TimeDelta::hours(2); // local 03:00
            let fold_start = autumn() + TimeDelta::hours(1); // local 02:00
            let fold_end = autumn() + TimeDelta::hours(2); // local 03:00
            if *local >= gap_start && *local < gap_end {
                LocalResult::None
            } else if *local >= fold_start && *local < fold_end {
                LocalResult::Ambiguous(fixed(2), fixed(1))
            } else if *local >= gap_end && *local < fold_start {
                LocalResult::Single(fixed(2))
            } else {
                LocalResult::Single(fixed(1))
            }
        }
        fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
            self.offset_from_utc_datetime(&utc.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
            if *utc >= spring() && *utc < autumn() {
                fixed(2)
            } else {
                fixed(1)
            }
        }
    }

    fn wall(mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    #[test]
    fn earliest_picks_the_first_reading_and_nothing_for_a_gap() {
        assert_eq!(earliest(LocalResult::Single(3)), Some(3));
        assert_eq!(earliest(LocalResult::Ambiguous(1, 2)), Some(1));
        assert_eq!(earliest::<i32>(LocalResult::None), None);
    }

    #[test]
    fn a_time_in_the_spring_gap_runs_at_the_first_valid_time_after_it() {
        for requested in [(2, 30), (2, 31), (2, 44)] {
            let at = resolve_local(&Dst, wall(3, 29, requested.0, requested.1)).unwrap();
            assert_eq!(at.naive_local(), wall(3, 29, 3, 0));
            assert_eq!(at.with_timezone(&Utc), utc(2026, 3, 29, 1, 0));
        }
        // Through next_after: the day before the change, then the gap day.
        let anchor = utc(2026, 1, 1, 0, 0);
        let after = utc(2026, 3, 28, 12, 0).with_timezone(&Dst);
        let next = next_after(&daily("02:30"), &anchor, &after).unwrap();
        assert_eq!(next.with_timezone(&Utc), utc(2026, 3, 29, 1, 0));
        // And the day after it is back to normal (02:30 at +2).
        let next2 = next_after(&daily("02:30"), &anchor, &next).unwrap();
        assert_eq!(next2.naive_local(), wall(3, 30, 2, 30));
        assert_eq!(next2.with_timezone(&Utc), utc(2026, 3, 30, 0, 30));
    }

    #[test]
    fn a_time_in_the_autumn_fold_runs_once_on_its_first_occurrence() {
        let anchor = utc(2026, 1, 1, 0, 0);
        let after = utc(2026, 10, 24, 12, 0).with_timezone(&Dst);
        let next = next_after(&daily("02:30"), &anchor, &after).unwrap();
        // The earlier reading: 02:30 at +2 = 00:30 UTC.
        assert_eq!(next.with_timezone(&Utc), utc(2026, 10, 25, 0, 30));
        // Not again at the second 02:30 (01:30 UTC) — the next run is tomorrow.
        let next2 = next_after(&daily("02:30"), &anchor, &next).unwrap();
        assert_eq!(next2.with_timezone(&Utc), utc(2026, 10, 26, 1, 30));
    }

    // ── decide / plan_tick ──

    #[test]
    fn a_new_or_legacy_schedule_initializes_without_firing() {
        let s = sched(daily("09:00"));
        let now = utc(2026, 5, 4, 12, 0);
        assert_eq!(decide(&s, false, now, false), Decision::Init);
        let mut st = SchedulesState {
            revision: 0,
            paused: false,
            schedules: vec![s],
        };
        let (changed, fire) = plan_tick(&mut st, &now, &HashMap::new());
        assert!(changed);
        assert!(fire.is_empty());
        assert_eq!(st.schedules[0].next_run_at, Some(utc(2026, 5, 5, 9, 0)));
        assert!(st.schedules[0].history.is_empty());
    }

    #[test]
    fn disabled_or_paused_schedules_never_fire() {
        let mut s = sched(daily("09:00"));
        let now = utc(2026, 5, 4, 12, 0);
        s.next_run_at = Some(utc(2026, 5, 4, 9, 0));
        assert_eq!(decide(&s, true, now, false), Decision::Idle);
        s.enabled = false;
        assert_eq!(decide(&s, false, now, false), Decision::Idle);
        let mut st = SchedulesState {
            revision: 0,
            paused: false,
            schedules: vec![s],
        };
        let (changed, fire) = plan_tick(&mut st, &now, &HashMap::new());
        assert!(!changed && fire.is_empty());
        st.schedules[0].enabled = true;
        st.paused = true;
        let (changed, fire) = plan_tick(&mut st, &now, &HashMap::new());
        assert!(!changed && fire.is_empty());
    }

    #[test]
    fn not_yet_due_is_idle_and_due_fires() {
        let mut s = sched(daily("09:00"));
        s.next_run_at = Some(utc(2026, 5, 4, 9, 0));
        assert_eq!(
            decide(&s, false, utc(2026, 5, 4, 8, 59), false),
            Decision::Idle
        );
        assert_eq!(
            decide(&s, false, utc(2026, 5, 4, 9, 0), false),
            Decision::Fire
        );
        // Within the grace a late tick still fires even with catch-up off.
        s.catch_up = false;
        assert_eq!(
            decide(&s, false, utc(2026, 5, 4, 9, 4), false),
            Decision::Fire
        );
    }

    #[test]
    fn an_active_previous_run_skips_and_records_it() {
        let mut s = sched(interval(30, "minutes"));
        s.next_run_at = Some(utc(2026, 5, 4, 9, 0));
        s.last_task_id = Some("t-old".into());
        let now = utc(2026, 5, 4, 9, 0) + TimeDelta::seconds(10);
        assert_eq!(
            decide(&s, false, now, true),
            Decision::Skip(MSG_ACTIVE.into())
        );
        let mut st = SchedulesState {
            revision: 0,
            paused: false,
            schedules: vec![s],
        };
        let active = HashMap::from([("s1".to_string(), true)]);
        let (changed, fire) = plan_tick(&mut st, &now, &active);
        assert!(changed && fire.is_empty());
        let s = &st.schedules[0];
        assert_eq!(s.last_status, "skipped");
        assert_eq!(s.last_message, MSG_ACTIVE);
        // Still points at the run that is going.
        assert_eq!(s.last_task_id.as_deref(), Some("t-old"));
        assert_eq!(s.next_run_at, Some(utc(2026, 5, 4, 9, 30)));
    }

    #[test]
    fn catch_up_fires_exactly_once_after_many_missed_slots() {
        let mut s = sched(interval(5, "minutes"));
        s.next_run_at = Some(utc(2026, 5, 1, 0, 0));
        // Closed for three days: hundreds of slots missed.
        let now = utc(2026, 5, 4, 10, 2);
        let mut st = SchedulesState {
            revision: 0,
            paused: false,
            schedules: vec![s],
        };
        let (changed, fire) = plan_tick(&mut st, &now, &HashMap::new());
        assert!(changed);
        assert_eq!(fire, vec!["s1".to_string()]);
        let next = st.schedules[0].next_run_at.unwrap();
        assert!(next > now);
        assert_eq!(next, utc(2026, 5, 4, 10, 5));
        // The next tick does nothing more.
        let (_, fire) = plan_tick(&mut st, &(now + TimeDelta::seconds(30)), &HashMap::new());
        assert!(fire.is_empty());
    }

    #[test]
    fn without_catch_up_a_missed_slot_is_recorded_not_run() {
        let mut s = sched(daily("09:00"));
        s.catch_up = false;
        s.next_run_at = Some(utc(2026, 5, 1, 9, 0));
        let now = utc(2026, 5, 4, 12, 0);
        assert_eq!(decide(&s, false, now, false), Decision::Missed);
        let mut st = SchedulesState {
            revision: 0,
            paused: false,
            schedules: vec![s],
        };
        let (changed, fire) = plan_tick(&mut st, &now, &HashMap::new());
        assert!(changed && fire.is_empty());
        let s = &st.schedules[0];
        assert_eq!(s.last_status, "skipped");
        assert_eq!(s.last_message, MSG_MISSED);
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.next_run_at, Some(utc(2026, 5, 5, 9, 0)));
    }

    #[test]
    fn history_is_newest_first_and_capped() {
        let mut s = sched(daily("09:00"));
        for i in 0..30 {
            record(
                &mut s,
                utc(2026, 5, 1, 0, 0) + TimeDelta::minutes(i),
                "started",
                "",
                Some(format!("t{i}")),
            );
        }
        assert_eq!(s.history.len(), HISTORY_CAP);
        assert_eq!(s.history[0].task_id.as_deref(), Some("t29"));
        assert_eq!(s.last_task_id.as_deref(), Some("t29"));
        record(&mut s, utc(2026, 6, 1, 0, 0), "failed", "boom", None);
        assert_eq!(
            s.last_task_id.as_deref(),
            Some("t29"),
            "a failure keeps the last chat"
        );
        assert_eq!(s.last_status, "failed");
    }

    #[test]
    fn active_status_definition() {
        assert!(task_is_active("running", false, None));
        assert!(task_is_active("waiting", true, None));
        assert!(task_is_active("idle", true, None));
        assert!(task_is_active("running", false, Some("manual")));
        assert!(task_is_active("stopped", false, Some("exhausted")));
        // A run the app was closed during stays stopped until the user resumes it.
        assert!(!task_is_active("stopped", false, Some("closed")));
        for done in ["idle", "done", "failed", "stopped"] {
            assert!(!task_is_active(done, false, None), "{done}");
        }
    }

    // ── validation ──

    fn defaults() -> Defaults {
        Defaults {
            perm: "allowlist".into(),
            model: "default-model".into(),
            effort: 2,
        }
    }

    fn draft() -> ScheduleDraft {
        ScheduleDraft {
            prompt: "Check the nightly build\nand report".into(),
            project: "/proj".into(),
            timing: Some(daily("9:05")),
            ..ScheduleDraft::default()
        }
    }

    #[test]
    fn a_minimal_draft_takes_the_users_defaults() {
        let now = utc(2026, 5, 4, 12, 0);
        let s = build(None, draft(), &defaults(), &now).unwrap();
        assert!(!s.id.is_empty());
        assert_eq!(s.name, "Check the nightly build");
        assert_eq!(s.perm, "allowlist", "user default, not the hardcoded one");
        assert_eq!(s.model, "default-model");
        assert_eq!(s.effort, 2);
        assert_eq!(s.workspace, "local");
        assert!(s.enabled && s.catch_up);
        assert_eq!(s.timing, daily("09:05"));
        assert_eq!(s.next_run_at, Some(utc(2026, 5, 5, 9, 5)));
        // Empty perm is "absent" too.
        let d = ScheduleDraft {
            perm: Some(" ".into()),
            ..draft()
        };
        assert_eq!(build(None, d, &defaults(), &now).unwrap().perm, "allowlist");
        // The same via JSON, as the frontend sends it.
        let d: ScheduleDraft = serde_json::from_value(serde_json::json!({
            "id": "", "prompt": "x", "project": "/p",
            "timing": {"kind": "weekly", "days": ["fri", "mon", "mon"], "time": "18:30"}
        }))
        .unwrap();
        let s = build(None, d, &defaults(), &now).unwrap();
        assert_eq!(s.perm, "allowlist");
        assert_eq!(s.timing, weekly(&["mon", "fri"], "18:30"));
    }

    #[test]
    fn validation_rejects_bad_input() {
        let now = utc(2026, 5, 4, 12, 0);
        let bad = |d: ScheduleDraft| build(None, d, &defaults(), &now).unwrap_err();
        bad(ScheduleDraft {
            prompt: "  ".into(),
            ..draft()
        });
        bad(ScheduleDraft {
            project: "".into(),
            ..draft()
        });
        assert!(bad(ScheduleDraft {
            perm: Some("yolo".into()),
            ..draft()
        })
        .contains("yolo"));
        bad(ScheduleDraft {
            workspace: Some("docker".into()),
            ..draft()
        });
        bad(ScheduleDraft {
            timing: None,
            ..draft()
        });
        for t in [
            "24:00", "12:60", "noon", "12:5", "", "1:2:3", "-1:00", "+1:00",
        ] {
            bad(ScheduleDraft {
                timing: Some(daily(t)),
                ..draft()
            });
        }
        bad(ScheduleDraft {
            timing: Some(weekly(&[], "09:00")),
            ..draft()
        });
        bad(ScheduleDraft {
            timing: Some(weekly(&["mon", "funday"], "09:00")),
            ..draft()
        });
        for (every, unit) in [
            (4, "minutes"),
            (0, "hours"),
            (169, "hours"),
            (10081, "minutes"),
            (5, "days"),
        ] {
            bad(ScheduleDraft {
                timing: Some(interval(every, unit)),
                ..draft()
            });
        }
        for (every, unit) in [(5, "minutes"), (168, "hours"), (10080, "minutes")] {
            build(
                None,
                ScheduleDraft {
                    timing: Some(interval(every, unit)),
                    ..draft()
                },
                &defaults(),
                &now,
            )
            .unwrap();
        }
        let no_model = Defaults {
            model: String::new(),
            ..defaults()
        };
        build(None, draft(), &no_model, &now).unwrap_err();
    }

    #[test]
    fn an_update_keeps_run_state_and_rearms_only_on_timing_or_enable_change() {
        let now = utc(2026, 5, 4, 12, 0);
        let mut s = build(None, draft(), &defaults(), &now).unwrap();
        record(&mut s, now, "started", MSG_STARTED, Some("t1".into()));
        s.next_run_at = Some(utc(2026, 5, 5, 9, 5));
        let later = utc(2026, 5, 4, 13, 0);
        // Same timing, new prompt: run state and next_run_at survive.
        let d = ScheduleDraft {
            id: Some(s.id.clone()),
            prompt: "new prompt".into(),
            name: "Mine".into(),
            ..draft()
        };
        let u = build(Some(&s), d, &defaults(), &later).unwrap();
        assert_eq!(u.id, s.id);
        assert_eq!(u.created_at, s.created_at);
        assert_eq!(u.name, "Mine");
        assert_eq!(u.last_task_id.as_deref(), Some("t1"));
        assert_eq!(u.history.len(), 1);
        assert_eq!(u.next_run_at, s.next_run_at);
        // New timing → recomputed from now.
        let d = ScheduleDraft {
            id: Some(s.id.clone()),
            timing: Some(interval(30, "minutes")),
            ..draft()
        };
        let u = build(Some(&s), d, &defaults(), &later).unwrap();
        assert_eq!(u.next_run_at, Some(utc(2026, 5, 4, 13, 30)));
        // Disabled → no next run; re-enabled → rearmed.
        let off = build(
            Some(&s),
            ScheduleDraft {
                enabled: Some(false),
                ..draft()
            },
            &defaults(),
            &later,
        )
        .unwrap();
        assert!(!off.enabled && off.next_run_at.is_none());
        let kept = build(Some(&off), draft(), &defaults(), &later).unwrap();
        assert!(
            !kept.enabled,
            "an absent `enabled` keeps the existing state"
        );
        let on = build(
            Some(&off),
            ScheduleDraft {
                enabled: Some(true),
                ..draft()
            },
            &defaults(),
            &later,
        )
        .unwrap();
        assert!(on.enabled);
        assert_eq!(on.next_run_at, Some(utc(2026, 5, 5, 9, 5)));
    }

    // ── persistence ──

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openleash-schedule-{name}-{}", std::process::id()))
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = temp("missing");
        let _g = store::test_home(&dir);
        let st = load();
        assert!(!st.paused && st.schedules.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_partial_json_loads_with_defaults() {
        let dir = temp("partial");
        let _g = store::test_home(&dir);
        std::fs::write(
            path(),
            r#"{"schedules":[{"id":"a","prompt":"p","project":"/x","model":"m",
                "timing":{"kind":"daily","time":"07:00"},"created_at":"2026-01-01T00:00:00Z"},
               {"id":"b","perm":"","timing":{"kind":"interval","every":2,"unit":"hours"}}]}"#,
        )
        .unwrap();
        let st = load();
        assert!(!st.paused);
        assert_eq!(st.schedules.len(), 2);
        let a = &st.schedules[0];
        assert!(a.catch_up && a.enabled);
        assert_eq!(a.perm, super::super::permissions::default_perm());
        assert_eq!(a.workspace, "local");
        assert!(a.history.is_empty() && a.next_run_at.is_none());
        assert_eq!(
            st.schedules[1].perm,
            super::super::permissions::default_perm()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_loads_empty() {
        let dir = temp("corrupt");
        let _g = store::test_home(&dir);
        std::fs::write(path(), "{not json").unwrap();
        let st = load();
        assert!(st.schedules.is_empty());
        let bad = store::data_dir().join("schedules.json.bad");
        assert_eq!(std::fs::read_to_string(bad).unwrap(), "{not json");
        assert!(!path().exists(), "moved aside, not left to be overwritten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = temp("roundtrip");
        let _g = store::test_home(&dir);
        let now = utc(2026, 5, 4, 12, 0);
        let mut s = build(None, draft(), &defaults(), &now).unwrap();
        record(&mut s, now, "started", MSG_STARTED, Some("t1".into()));
        let st = SchedulesState {
            revision: 0,
            paused: true,
            schedules: vec![s.clone()],
        };
        save(&st);
        let back = load();
        assert!(back.paused);
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&st).unwrap()
        );
        // Wire shape the frontend relies on.
        let v = serde_json::to_value(&back.schedules[0]).unwrap();
        assert_eq!(
            v["timing"],
            serde_json::json!({"kind": "daily", "time": "09:05"})
        );
        assert_eq!(v["history"][0]["status"], "started");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── the async layer (no create_task: every path here stops before it) ──

    fn trusted_settings(project: &str) -> store::Settings {
        let mut st = store::Settings::default();
        super::super::trust::upsert(
            &mut st.trust,
            project,
            super::super::trust::TrustKind::Folder,
            super::super::trust::TrustState::Trusted,
        );
        st
    }

    async fn scheduler_with(
        settings: store::Settings,
        tasks: Vec<super::super::Task>,
        s: Schedule,
    ) -> (Arc<Scheduler>, super::super::tests::Events) {
        let (h, events) = super::super::tests::harness(settings, tasks);
        let sch = Scheduler::new(h);
        sch.state.lock().await.schedules.push(s);
        (sch, events)
    }

    fn published(events: &super::super::tests::Events) -> usize {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == EVENT)
            .count()
    }

    #[tokio::test]
    async fn run_now_on_an_undecided_project_fails_without_a_chat() {
        let dir = temp("untrusted");
        let _g = store::test_home(&dir);
        let (sch, events) =
            scheduler_with(store::Settings::default(), vec![], sched(daily("09:00"))).await;
        let out = sch.run_now("s1").await.unwrap();
        assert_eq!(out.last_status, "failed");
        assert_eq!(out.last_message, MSG_UNTRUSTED);
        assert!(out.last_task_id.is_none());
        assert!(sch.h.tasks.read().await.is_empty());
        assert_eq!(published(&events), 1);
        assert_eq!(load().schedules[0].last_status, "failed", "persisted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_now_skips_while_the_previous_chat_is_running() {
        let dir = temp("overlap");
        let _g = store::test_home(&dir);
        let mut t = super::super::tests::task("t-old", "m");
        t.status = "waiting".into();
        let mut s = sched(daily("09:00"));
        s.last_task_id = Some("t-old".into());
        let (sch, _events) = scheduler_with(trusted_settings("/proj"), vec![t], s).await;
        let out = sch.run_now("s1").await.unwrap();
        assert_eq!(out.last_status, "skipped");
        assert_eq!(out.last_message, MSG_ACTIVE);
        assert_eq!(sch.h.tasks.read().await.len(), 1, "no new chat");
        // A live runtime flag counts too, even on an idle-looking chat.
        let rt = sch.h.runtime("t-old");
        rt.running.store(true, Ordering::SeqCst);
        assert!(sch.is_active(Some("t-old")).await);
        rt.running.store(false, Ordering::SeqCst);
        // Detached subagent tokens remain live after the main task finishes.
        rt.sub_cancel
            .lock()
            .unwrap()
            .insert("sub".into(), tokio_util::sync::CancellationToken::new());
        assert!(sch.is_active(Some("t-old")).await);
        let out = sch.run_now("s1").await.unwrap();
        assert_eq!(out.last_status, "skipped");
        assert_eq!(out.last_message, MSG_ACTIVE);
        assert_eq!(sch.h.tasks.read().await.len(), 1, "no new chat");
        {
            let tasks = sch.h.tasks.read().await;
            tasks["t-old"].lock().await.status = "done".into();
        }
        // Completed/cancelled subagent tokens no longer block a schedule.
        rt.sub_cancel.lock().unwrap().get("sub").unwrap().cancel();
        assert!(!sch.is_active(Some("t-old")).await);
        // A deleted chat is not active.
        assert!(!sch.is_active(Some("gone")).await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_now_skips_while_openleash_is_paused() {
        let dir = temp("pausedall");
        let _g = store::test_home(&dir);
        let mut settings = trusted_settings("/proj");
        settings.paused_all = true;
        let (sch, _events) = scheduler_with(settings, vec![], sched(daily("09:00"))).await;
        let out = sch.run_now("s1").await.unwrap();
        assert_eq!(out.last_status, "skipped");
        assert_eq!(out.last_message, MSG_PAUSED_ALL);
        assert!(sch.h.tasks.read().await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn timed_fire_rechecks_eligibility_after_it_was_queued() {
        let dir = temp("timed-recheck");
        let _g = store::test_home(&dir);
        let mut s = sched(daily("09:00"));
        s.next_run_at = Some(Utc::now() - TimeDelta::minutes(1));
        let settings = trusted_settings("/proj");
        let (sch, events) = scheduler_with(settings, vec![], s).await;
        // Hold settings while timed fire clones its queued schedule and blocks
        // in the awaited environment read. Disable before releasing it, then
        // it must reject at the final reservation point.
        let settings = sch.h.settings.write().await;
        let fire = {
            let sch = sch.clone();
            tokio::spawn(async move { sch.fire("s1", true).await.unwrap() })
        };
        tokio::task::yield_now().await;
        sch.set_enabled("s1", false).await.unwrap();
        drop(settings);
        assert!(fire.await.unwrap().is_none());
        let state = sch.list().await;
        let s = &state.schedules[0];
        assert!(!s.enabled);
        assert!(s.history.is_empty());
        assert!(sch.h.tasks.read().await.is_empty());
        // Global schedule pause rejects queued timed work as well.
        sch.set_enabled("s1", true).await.unwrap();
        sch.set_paused(true).await;
        assert!(sch.fire("s1", true).await.unwrap().is_none());
        assert!(sch.list().await.schedules[0].history.is_empty());
        assert_eq!(published(&events), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn background_creation_preserves_concurrent_interactive_choices() {
        let dir = temp("background-choices");
        let _g = store::test_home(&dir);
        let (h, _) = super::super::tests::harness(store::Settings::default(), vec![]);
        // Freeze the creation settings stage while the user changes their
        // choices. No provider, startup task, real project or timing sleeps.
        let mut settings = h.settings.write().await;
        settings.project = "before".into();
        settings.recent_models = vec!["before-model".into()];
        let reached = Arc::new(tokio::sync::Notify::new());
        let create = {
            let h = h.clone();
            let reached = reached.clone();
            tokio::spawn(async move {
                reached.notify_one();
                crate::remember_task_creation(
                    &h,
                    "scheduled-project",
                    "scheduled-model",
                    2,
                    crate::CreateTaskOptions::Background,
                )
                .await;
            })
        };
        reached.notified().await;
        settings.project = "user-selected-project".into();
        settings.recent_models = vec!["user-selected-model".into(), "other-model".into()];
        let expected = (settings.project.clone(), settings.recent_models.clone());
        drop(settings);
        create.await.unwrap();
        {
            let settings = h.settings.read().await;
            assert_eq!(settings.project, expected.0);
            assert_eq!(settings.recent_models, expected.1);
        }
        // Existing composer / connector callers still remember their choices.
        crate::remember_task_creation(
            &h,
            "interactive-project",
            "interactive-model",
            2,
            crate::CreateTaskOptions::Interactive,
        )
        .await;
        let settings = h.settings.read().await;
        assert_eq!(settings.project, "interactive-project");
        assert_eq!(settings.recent_models[0], "interactive-model");
        drop(settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn commands_mutate_persist_and_publish() {
        let dir = temp("commands");
        let _g = store::test_home(&dir);
        let settings = store::Settings {
            perm: "auto".into(),
            recent_models: vec!["recent".into()],
            ..Default::default()
        };
        let (h, events) = super::super::tests::harness(settings, vec![]);
        let sch = Scheduler::new(h);
        let s = sch.save_draft(draft()).await.unwrap();
        assert_eq!(s.perm, "auto");
        assert_eq!(s.model, "recent");
        assert!(s.next_run_at.is_some());
        let off = sch.set_enabled(&s.id, false).await.unwrap();
        assert!(!off.enabled && off.next_run_at.is_none());
        let on = sch.set_enabled(&s.id, true).await.unwrap();
        assert!(on.next_run_at.is_some());
        let st = sch.set_paused(true).await;
        assert!(st.paused);
        assert!(load().paused);
        sch.save_draft(ScheduleDraft {
            id: Some("nope".into()),
            ..draft()
        })
        .await
        .unwrap_err();
        sch.delete(&s.id).await.unwrap();
        sch.delete(&s.id).await.unwrap_err();
        assert!(load().schedules.is_empty());
        assert_eq!(published(&events), 5);
        let last = events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(n, _)| n == EVENT)
            .cloned()
            .unwrap();
        assert_eq!(
            last.1,
            serde_json::json!({"paused": true, "revision": 5, "schedules": []})
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tick_initializes_without_firing_and_respects_the_global_pause() {
        let dir = temp("tick");
        let _g = store::test_home(&dir);
        let (sch, events) =
            scheduler_with(store::Settings::default(), vec![], sched(daily("09:00"))).await;
        sch.tick().await;
        let st = sch.list().await;
        assert!(st.schedules[0].next_run_at.is_some());
        assert!(st.schedules[0].history.is_empty());
        assert_eq!(published(&events), 1);
        // Nothing changes on a quiet tick, so nothing is published.
        sch.tick().await;
        assert_eq!(published(&events), 1);
        // Overdue while globally paused: nothing happens.
        sch.state.lock().await.schedules[0].next_run_at = Some(Utc::now() - TimeDelta::minutes(1));
        sch.set_paused(true).await;
        sch.tick().await;
        assert!(sch.list().await.schedules[0].history.is_empty());
        // Un-pausing rearms into the future rather than firing the stale slot.
        let st = sch.set_paused(false).await;
        assert!(st.schedules[0].next_run_at.unwrap() > Utc::now());
        // Due and untrusted: a timed fire records the failure and moves on.
        sch.state.lock().await.schedules[0].next_run_at = Some(Utc::now() - TimeDelta::seconds(5));
        sch.tick().await;
        let s = &sch.list().await.schedules[0];
        assert_eq!(s.last_status, "failed");
        assert!(s.next_run_at.unwrap() > Utc::now());
        assert!(sch.h.tasks.read().await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
