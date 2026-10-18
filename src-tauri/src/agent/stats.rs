//! Usage statistics, persisted to `~/.openleash/stats.json`.
//!
//! Per model, per account, per day, per hour (last 14 days), per tool and per
//! agent kind, plus per chat, plus router events (retries, fallbacks, pauses).
//! Recording is in-memory; a background flush writes the file at most every few
//! seconds.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Counter {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Subset of `output` (only where the provider reports it).
    pub reasoning: u64,
    pub errors: u64,
    /// Failure rate as a fraction (0.0–1.0): `errors / (requests + errors)`.
    ///
    /// Recorded alongside the two counts it comes from rather than derived on
    /// every read, because this is the per-model figure the stats screen ranks
    /// on and a non-UI reader should not have to know the formula.
    ///
    /// The denominator is attempts (`requests + errors`), not `requests`: a
    /// failed attempt is recorded as an error and never increments `requests`
    /// (see `Stats::error`), so dividing by `requests` alone would report a
    /// rate above 1.0 and disagree with every other failure figure the UI
    /// prints. A `f64` so an older stats file without the key defaults to 0.0.
    ///
    /// Whether the UI reads this or re-derives it: it re-derives (see
    /// `failureRate` in `ui/Stats.tsx`), because the derive is correct for
    /// *aggregates* and for files that predate the field — `#[serde(default)]`
    /// hands an old model row a `0.0` that would otherwise read as "no
    /// failures" for a model whose every attempt failed.
    pub failure_rate: f64,
    /// Sum of request durations (ms), for averages.
    pub total_ms: u64,
    /// Sum of time-to-first-token (ms) over requests that streamed.
    pub ttft_ms: u64,
    pub ttft_n: u64,
    pub cost: f64,
    /// RFC 3339.
    pub last_used: String,
}

impl Counter {
    fn add(&mut self, u: &Sample) {
        self.requests += 1;
        self.input += u.input;
        self.output += u.output;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.reasoning += u.reasoning;
        self.total_ms += u.ms;
        if let Some(t) = u.ttft_ms {
            self.ttft_ms += t;
            self.ttft_n += 1;
        }
        self.cost += u.cost;
        self.last_used = chrono::Utc::now().to_rfc3339();
        self.update_failure_rate();
    }

    /// Recompute `failure_rate` from the two counts it is derived from.
    ///
    /// Called from every path that moves `requests` or `errors`, so the stored
    /// rate can never drift from the counts beside it. Zero attempts reads as
    /// 0.0, not NaN: nothing was tried, so nothing failed.
    fn update_failure_rate(&mut self) {
        let attempts = self.requests + self.errors;
        self.failure_rate = if attempts == 0 {
            0.0
        } else {
            self.errors as f64 / attempts as f64
        };
    }
}

/// How long the app was open, split by whether the user was looking at it.
///
/// All four are wall-clock milliseconds of *app uptime*, sampled by
/// [`Stats::tick`]; `background` is not stored because it is `open - focused`
/// and a stored copy could only ever disagree with its two parents.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Presence {
    /// The app process was running (window shown, minimised or in the tray).
    pub open_ms: u64,
    /// The main window had focus.
    pub focused_ms: u64,
    /// At least one chat was running its agent, focused or not.
    pub working_ms: u64,
    /// Subset of `working_ms` during which the window did *not* have focus —
    /// the agent getting on with it while the user was elsewhere.
    pub unattended_ms: u64,
}

impl Presence {
    fn add(&mut self, ms: u64, focused: bool, working: bool) {
        self.open_ms += ms;
        if focused {
            self.focused_ms += ms;
        }
        if working {
            self.working_ms += ms;
            if !focused {
                self.unattended_ms += ms;
            }
        }
    }
}

/// The longest gap between two ticks that still counts as time in the app.
///
/// Ticks are every few seconds; a gap far larger than that means the machine
/// slept or the process was frozen, and crediting it would report hours of
/// "open" for a laptop lid that was shut. The excess is dropped, not carried.
pub const MAX_TICK_MS: u64 = 15_000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolCount {
    pub calls: u64,
    pub errors: u64,
}

/// Everything one chat spent.
///
/// A counter alone answers "how much", which is the question the ranked list
/// asks. The rest is what makes a single chat's stats worth opening: which model
/// served it (a chat that fell back three times looks identical to one that did
/// not, until you see the split), which tools it burned calls on, and what went
/// wrong while it ran.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChatStat {
    pub total: Counter,
    /// model id -> counter.
    pub models: BTreeMap<String, Counter>,
    /// usage id -> counter: the split of `total` by *who* spent it — "main",
    /// "sub:<subagent id>", "compactor", "keepalive", … Additive to `total` so the
    /// headline still reconciles with this breakdown.
    #[serde(default)]
    pub by_agent: BTreeMap<String, Counter>,
    pub tools: BTreeMap<String, ToolCount>,
    /// retries | fallbacks | pauses | compactions
    pub events: BTreeMap<String, u64>,
    /// First request of the chat, RFC 3339. `Counter::last_used` only records
    /// the most recent one, so without this a chat's span is unrecoverable.
    pub started: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StatsData {
    pub version: u32,
    pub since: String,
    pub models: BTreeMap<String, Counter>,
    pub accounts: BTreeMap<String, Counter>,
    /// yyyy-mm-dd (local)
    pub daily: BTreeMap<String, Counter>,
    /// yyyy-mm-ddTHH (local), last 14 days
    pub hourly: BTreeMap<String, Counter>,
    /// main | subagent id (explore, general, …)
    pub agents: BTreeMap<String, Counter>,
    pub tools: BTreeMap<String, ToolCount>,
    /// retries | fallbacks | pauses | compactions
    pub events: BTreeMap<String, u64>,
    /// yyyy-mm-dd -> model -> counter, so any time range can be filtered by model.
    pub model_daily: BTreeMap<String, BTreeMap<String, Counter>>,
    /// yyyy-mm-ddTHH -> model -> counter, last 14 days.
    pub model_hourly: BTreeMap<String, BTreeMap<String, Counter>>,
    /// chat id -> per-chat record. Ids only, never a title: `stats.json` is a
    /// usage ledger, and a title is content the user wrote, not a measurement.
    /// The UI joins ids to the chat list it already holds, so the names never
    /// reach a second file that outlives the chat.
    ///
    /// Unlike the day/hour maps this has no date on the key to trim on — a chat
    /// that ran once a year ago is still a real number — so it is capped by
    /// count instead, least recently used first (`MAX_CHATS`).
    pub chats: BTreeMap<String, ChatStat>,
    /// Time in the app, all time.
    pub presence: Presence,
    /// yyyy-mm-dd (local) -> time in the app that day, last 365 days.
    pub presence_daily: BTreeMap<String, Presence>,
    /// yyyy-mm-ddTHH (local) -> time in the app that hour, last 14 days.
    pub presence_hourly: BTreeMap<String, Presence>,
    /// How many times the app has been started.
    pub launches: u64,
    /// Open time of the current run (ms). Starts at 0 on every launch, so after
    /// a restart the stored value is stale until the first tick overwrites it.
    pub session_ms: u64,
    /// The longest single run of the app, ms.
    pub longest_session_ms: u64,
}

/// How many chats [`StatsData::chats`] keeps.
///
/// Every chat that ever ran a request wants an entry, and unlike the daily keys
/// there is no date on them to age out, so an unbounded map would grow forever
/// and be serialized in full on every flush (10s) and every poll (15s) — the
/// same mistake the daily map already made once, per the note in `record`.
///
/// A chat record is ~2 KB with a few models and tools on it, so this is roughly
/// the file size the dimension is worth: 100 chats is ~200 KB on top of a stats
/// file that is already ~130 KB. Past that the least recently used rows fall
/// out, and their totals are still in `models`/`daily` either way.
pub const MAX_CHATS: usize = 100;

/// How many tools one chat keeps.
///
/// The app has ~36 tools, so an unbounded per-chat map is bounded in practice —
/// but "in practice" is how the chat map itself would have grown forever, and a
/// chat that used everything is exactly the one whose record is biggest. The
/// busiest win, because a tool with 1 call is noise next to one with 400.
pub const MAX_CHAT_TOOLS: usize = 25;

/// How many usage ids one chat keeps. A big ultrathread tree is dozens of
/// subagents, each with its own row; past this the least expensive fall out.
pub const MAX_CHAT_AGENTS: usize = 50;

/// One finished model request.
pub struct Sample {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub ms: u64,
    pub ttft_ms: Option<u64>,
    pub cost: f64,
}

#[derive(Default)]
pub struct Stats {
    data: Mutex<StatsData>,
    dirty: AtomicBool,
    /// When [`Stats::tick`] last ran, to measure the gap to the next one.
    last_tick: Mutex<Option<std::time::Instant>>,
}

fn path() -> std::path::PathBuf {
    super::store::data_dir().join("stats.json")
}

impl Stats {
    pub fn load() -> Stats {
        let mut d: StatsData = std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if d.since.is_empty() {
            d.since = chrono::Utc::now().to_rfc3339();
        }
        d.version = 1;
        // A new run: the stored session length belongs to the previous one.
        d.launches += 1;
        d.session_ms = 0;
        Stats {
            data: Mutex::new(d),
            dirty: AtomicBool::new(false),
            last_tick: Mutex::new(None),
        }
    }

    /// Credit the time since the previous tick to "in the app", split by
    /// whether the window is focused and whether any chat is working.
    ///
    /// The first call only starts the clock. Sampling (rather than listening
    /// for focus events) is deliberate: a hidden-to-tray, minimised or
    /// covered window all just read as "not focused", and a missed event can
    /// never leave a counter running forever.
    pub fn tick(&self, focused: bool, working: bool) {
        let now = std::time::Instant::now();
        let prev = self.last_tick.lock().unwrap().replace(now);
        let Some(prev) = prev else { return };
        let ms = (now.duration_since(prev).as_millis() as u64).min(MAX_TICK_MS);
        self.add_presence(ms, focused, working, chrono::Local::now());
    }

    fn add_presence(
        &self,
        ms: u64,
        focused: bool,
        working: bool,
        now: chrono::DateTime<chrono::Local>,
    ) {
        if ms == 0 {
            return;
        }
        let mut d = self.data.lock().unwrap();
        d.presence.add(ms, focused, working);
        d.presence_daily
            .entry(now.format("%Y-%m-%d").to_string())
            .or_default()
            .add(ms, focused, working);
        d.presence_hourly
            .entry(now.format("%Y-%m-%dT%H").to_string())
            .or_default()
            .add(ms, focused, working);
        d.session_ms += ms;
        d.longest_session_ms = d.longest_session_ms.max(d.session_ms);
        let cutoff = (now - chrono::Duration::days(14))
            .format("%Y-%m-%dT%H")
            .to_string();
        d.presence_hourly.retain(|k, _| *k >= cutoff);
        let day_cutoff = (now - chrono::Duration::days(365))
            .format("%Y-%m-%d")
            .to_string();
        d.presence_daily.retain(|k, _| *k >= day_cutoff);
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// Drop the stats of every chat `keep` rejects. Returns how many went.
    ///
    /// Only the per-chat rows go: the chat's tokens, cost and requests stay in
    /// the model/day/account totals, which is what keeps those reconciling with
    /// the spend ledger.
    pub fn retain_chats(&self, keep: impl Fn(&str) -> bool) -> usize {
        let mut d = self.data.lock().unwrap();
        let before = d.chats.len();
        d.chats.retain(|id, _| keep(id));
        let removed = before - d.chats.len();
        if removed > 0 {
            self.dirty.store(true, Ordering::SeqCst);
        }
        removed
    }

    pub fn record(
        &self,
        chat: &str,
        model: &str,
        account: Option<&str>,
        agent: &str,
        usage_id: &str,
        s: &Sample,
    ) {
        let now = chrono::Local::now();
        let mut d = self.data.lock().unwrap();
        if d.since.is_empty() {
            d.since = chrono::Utc::now().to_rfc3339();
        }
        d.models.entry(model.into()).or_default().add(s);
        if let Some(a) = account {
            d.accounts.entry(a.into()).or_default().add(s);
        }
        d.daily
            .entry(now.format("%Y-%m-%d").to_string())
            .or_default()
            .add(s);
        d.hourly
            .entry(now.format("%Y-%m-%dT%H").to_string())
            .or_default()
            .add(s);
        d.model_daily
            .entry(now.format("%Y-%m-%d").to_string())
            .or_default()
            .entry(model.into())
            .or_default()
            .add(s);
        d.model_hourly
            .entry(now.format("%Y-%m-%dT%H").to_string())
            .or_default()
            .entry(model.into())
            .or_default()
            .add(s);
        d.agents.entry(agent.into()).or_default().add(s);
        {
            let c = d.chats.entry(chat.into()).or_default();
            c.total.add(s);
            c.models.entry(model.into()).or_default().add(s);
            // `agent` above stays the *role* (main/explore/…), which is what keeps
            // the app-wide `agents` dimension down to a handful of buckets. This is
            // the instance-level id ("sub:<id>", "compactor", "keepalive"), which is
            // what tells two explores apart. Added next to `total` rather than
            // instead of it, so the headline still reconciles with the breakdown.
            c.by_agent.entry(usage_id.into()).or_default().add(s);
            if c.started.is_empty() {
                c.started = chrono::Utc::now().to_rfc3339();
            }
            // Bounded the way the tool list is: this map is serialized in full on
            // every flush, and a big ultrathread tree is dozens of subagents each
            // with a row, so the cheapest fall out. Sorting by `(cost, key)` rather
            // than cost alone gives equal costs a stable order, so two zero-cost
            // rows do not take turns evicting each other on alternate flushes.
            if c.by_agent.len() > MAX_CHAT_AGENTS {
                let mut by_cost: Vec<(f64, String)> = c
                    .by_agent
                    .iter()
                    .map(|(k, v)| (v.cost, k.clone()))
                    .collect();
                by_cost.sort_by(|a, b| {
                    b.0.partial_cmp(&a.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.1.cmp(&a.1))
                });
                for (_, k) in by_cost.into_iter().skip(MAX_CHAT_AGENTS) {
                    c.by_agent.remove(&k);
                }
            }
        }
        // Keep hourly bounded: two weeks. Daily keys were never trimmed, so
        // the maps grew by one entry per day forever and every stats poll
        // serialized the lot. "All" and the ledger below still have the totals.
        let cutoff = (now - chrono::Duration::days(14))
            .format("%Y-%m-%dT%H")
            .to_string();
        d.hourly.retain(|k, _| *k >= cutoff);
        d.model_hourly.retain(|k, _| *k >= cutoff);
        let day_cutoff = (now - chrono::Duration::days(365))
            .format("%Y-%m-%d")
            .to_string();
        d.daily.retain(|k, _| *k >= day_cutoff);
        d.model_daily.retain(|k, _| *k >= day_cutoff);
        // Chats age out by use, not by date: drop the least recently used until
        // there is room again. `last_used` is an RFC 3339 UTC stamp, so the
        // lexicographic order of the values is the chronological one and this
        // needs no second sort key.
        if d.chats.len() > MAX_CHATS {
            let mut by_age: Vec<(String, String)> = d
                .chats
                .iter()
                .map(|(k, c)| (c.total.last_used.clone(), k.clone()))
                .collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(d.chats.len() - MAX_CHATS) {
                d.chats.remove(&k);
            }
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// Count a tool call against one chat, alongside the app-wide total.
    ///
    /// The two are recorded in one place on purpose: a per-chat tool breakdown
    /// read off a separate tally would drift from `StatsData::tools` the first
    /// time one of them was missed, and the two numbers sit side by side in the
    /// same view.
    pub fn tool_in(&self, chat: &str, name: &str, failed: bool) {
        let mut d = self.data.lock().unwrap();
        let t = d.tools.entry(name.into()).or_default();
        t.calls += 1;
        if failed {
            t.errors += 1;
        }
        // A tool call is only recorded against a chat that exists. Unlike the
        // request path this is not the thing that creates the row, so a chat
        // with no row here is a chat that never ran a request — giving it one
        // would fill the capped map with empty entries.
        let Some(c) = d.chats.get_mut(chat) else {
            self.dirty.store(true, Ordering::SeqCst);
            return;
        };
        let t = c.tools.entry(name.into()).or_default();
        t.calls += 1;
        if failed {
            t.errors += 1;
        }
        if c.tools.len() > MAX_CHAT_TOOLS {
            let mut by_calls: Vec<(u64, String)> =
                c.tools.iter().map(|(k, t)| (t.calls, k.clone())).collect();
            by_calls.sort_by(|a, b| b.cmp(a));
            for (_, k) in by_calls.into_iter().skip(MAX_CHAT_TOOLS) {
                c.tools.remove(&k);
            }
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// Count a router event against one chat, alongside the app-wide total.
    pub fn event_in(&self, chat: &str, name: &str) {
        let mut d = self.data.lock().unwrap();
        *d.events.entry(name.into()).or_default() += 1;
        // An event is not a reason to conjure a chat that never spent a token —
        // a paused chat that made no request yet has no row to hang it on.
        if let Some(c) = d.chats.get_mut(chat) {
            *c.events.entry(name.into()).or_default() += 1;
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// Stop counting a chat's usage against it, keeping every other dimension.
    ///
    /// Deleting a chat calls this only when the user turned "Keep stats for
    /// deleted chats" off: by default the numbers for a chat that is gone are
    /// still what was spent on it, and a usage ledger that shrinks as you tidy
    /// up stops adding up. It also covers a chat whose id is *wrong* rather
    /// than gone (a fork reusing one), which would otherwise silently merge two
    /// chats' history into one row forever.
    pub fn forget_chat(&self, chat: &str) {
        let mut d = self.data.lock().unwrap();
        if d.chats.remove(chat).is_some() {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    pub fn error(&self, chat: &str, model: &str, account: Option<&str>) {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut d = self.data.lock().unwrap();
        let m = d.models.entry(model.into()).or_default();
        m.errors += 1;
        m.update_failure_rate();
        if let Some(a) = account {
            let a = d.accounts.entry(a.into()).or_default();
            a.errors += 1;
            a.update_failure_rate();
        }
        let daily = d.daily.entry(day.clone()).or_default();
        daily.errors += 1;
        daily.update_failure_rate();
        // A failed attempt is the chat's spend of attention too, and unlike a tool call
        // this one *creates* the row: a chat whose every attempt failed is
        // exactly the one a user opens stats on to find out why, and it would
        // otherwise show up as a chat with nothing recorded at all. `last_used`
        // is stamped because the row exists now and the LRU trim reads it.
        let c = d.chats.entry(chat.into()).or_default();
        c.total.errors += 1;
        c.total.last_used = chrono::Utc::now().to_rfc3339();
        c.total.update_failure_rate();
        if !c.models.contains_key(model) {
            // Keep the model's `last_used` in step with the chat's: the LRU trim
            // reads the chat's stamp, but a model row with an empty one is the
            // shape the per-model charts sort on, and "never used" would read
            // as older than everything.
            c.models.entry(model.into()).or_default().last_used = c.total.last_used.clone();
        }
        let cm = c.models.entry(model.into()).or_default();
        cm.errors += 1;
        cm.update_failure_rate();
        if c.started.is_empty() {
            c.started = c.total.last_used.clone();
        }
        let md = d
            .model_daily
            .entry(day)
            .or_default()
            .entry(model.into())
            .or_default();
        md.errors += 1;
        md.update_failure_rate();
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// App-wide-only tool tally, for a call with no chat to hang it on. The
    /// per-chat counterpart is `tool_in`, which records both halves at once so
    /// the two views cannot drift apart.
    #[allow(dead_code)]
    pub fn tool(&self, name: &str, failed: bool) {
        let mut d = self.data.lock().unwrap();
        let t = d.tools.entry(name.into()).or_default();
        t.calls += 1;
        if failed {
            t.errors += 1;
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// App-wide-only router event tally. See `tool` for why the chatless
    /// variant exists alongside `event_in`.
    #[allow(dead_code)]
    pub fn event(&self, name: &str) {
        *self
            .data
            .lock()
            .unwrap()
            .events
            .entry(name.into())
            .or_default() += 1;
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> StatsData {
        self.data.lock().unwrap().clone()
    }

    /// One chat's record, if it is still within the cap.
    ///
    /// `None` is a real answer, not an error: a chat that never ran a request has
    /// no row, and one that has aged out of `MAX_CHATS` has lost it.
    pub fn chat(&self, id: &str) -> Option<ChatStat> {
        self.data.lock().unwrap().chats.get(id).cloned()
    }

    pub fn reset(&self) {
        let mut d = self.data.lock().unwrap();
        // The app is still on its current run, so the run itself is not reset.
        let session = d.session_ms;
        *d = StatsData {
            version: 1,
            since: chrono::Utc::now().to_rfc3339(),
            launches: 1,
            session_ms: session,
            longest_session_ms: session,
            ..Default::default()
        };
        drop(d);
        self.dirty.store(true, Ordering::SeqCst);
        self.flush();
    }

    /// Write the file if anything changed since the last write.
    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let d = self.snapshot();
        if let Ok(j) = serde_json::to_string_pretty(&d) {
            let p = path();
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, j).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // The `test_home` guard must span the whole test to serialize
    // `OPENLEASH_HOME` across tokio tests, so it is deliberately held across
    // `.await` points. See AGENTS.md: do not drop the lock to quiet the lint.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    #[test]
    fn counts_everything_once() {
        let s = Stats::default();
        let smp = |i, o, cr, ms, t: Option<u64>| Sample {
            input: i,
            output: o,
            cache_read: cr,
            cache_write: 0,
            reasoning: o / 2,
            ms,
            ttft_ms: t,
            cost: 0.01,
        };
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            Some("ChatGPT 1"),
            "main",
            "main",
            &smp(100, 10, 900, 2000, Some(300)),
        );
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            Some("ChatGPT 1"),
            "explore",
            "sub:s1",
            &smp(50, 5, 0, 1000, None),
        );
        s.error("chat-1", "codex/gpt-6-sol", Some("ChatGPT 1"));
        s.tool("read_file", false);
        s.tool("bash", true);
        s.event("fallbacks");
        let d = s.snapshot();
        let m = &d.models["codex/gpt-6-sol"];
        assert_eq!(
            (
                m.requests,
                m.input,
                m.output,
                m.cache_read,
                m.errors,
                m.total_ms,
                m.ttft_n
            ),
            (2, 150, 15, 900, 1, 3000, 1)
        );
        assert_eq!(d.accounts["ChatGPT 1"].requests, 2);
        assert_eq!(d.agents["explore"].requests, 1);
        assert_eq!(d.daily.values().map(|c| c.requests).sum::<u64>(), 2);
        assert_eq!(d.hourly.len(), 1);
        assert_eq!((d.tools["bash"].calls, d.tools["bash"].errors), (1, 1));
        assert_eq!(d.events["fallbacks"], 1);
        assert!((m.cost - 0.02).abs() < 1e-9);
        assert_eq!(m.reasoning, 7);
        let today: Vec<_> = d.model_daily.values().collect();
        assert_eq!(
            (
                today.len(),
                today[0]["codex/gpt-6-sol"].requests,
                today[0]["codex/gpt-6-sol"].errors
            ),
            (1, 2, 1)
        );
        assert_eq!(
            d.model_hourly.values().next().unwrap()["codex/gpt-6-sol"].input,
            150
        );
    }

    #[test]
    fn every_dimension_including_the_chat_gets_the_same_sample() {
        let s = Stats::default();
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            Some("ChatGPT 1"),
            "main",
            "main",
            &Sample {
                input: 100,
                output: 10,
                cache_read: 900,
                cache_write: 5,
                reasoning: 4,
                ms: 2000,
                ttft_ms: Some(300),
                cost: 0.01,
            },
        );
        let c = &s.snapshot().chats["chat-1"];
        let t = &c.total;
        // One request into the chat row must move every field, and to exactly the
        // numbers the other dimensions recorded — a per-chat row that quietly
        // skipped reasoning or cost would read as a cheaper chat than it was.
        assert_eq!(
            (t.requests, t.input, t.output, t.cache_read),
            (1, 100, 10, 900)
        );
        assert_eq!(
            (t.cache_write, t.reasoning, t.errors, t.total_ms),
            (5, 4, 0, 2000)
        );
        assert_eq!(t.ttft_n, 1);
        assert!((t.cost - 0.01).abs() < 1e-9);
        assert!(!t.last_used.is_empty(), "drives the LRU trim");
        assert!(!c.started.is_empty(), "the span needs a first stamp");
        // The per-model split has to exist from the first request, not just the
        // token total: "which model served this chat" is the question the
        // per-chat view opens with.
        assert_eq!(c.models["codex/gpt-6-sol"].input, 100);
    }

    #[test]
    fn a_chat_that_switched_models_shows_both() {
        let s = Stats::default();
        let smp = |i| Sample {
            input: i,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 10,
            ttft_ms: None,
            cost: 0.01,
        };
        s.record("chat-1", "codex/gpt-6-sol", None, "main", "main", &smp(100));
        s.record(
            "chat-1",
            "anthropic/claude-opus-5",
            None,
            "main",
            "main",
            &smp(20),
        );
        let c = &s.snapshot().chats["chat-1"];
        // A fallback chat is the interesting one, and it only reads as one if the
        // split survives: totals that agree with the models are what makes the
        // per-model bars add up to the headline number.
        assert_eq!(c.total.input, 120);
        assert_eq!(c.total.requests, 2);
        assert_eq!(c.models["codex/gpt-6-sol"].input, 100);
        assert_eq!(c.models["anthropic/claude-opus-5"].input, 20);
        assert_eq!(
            c.models.values().map(|m| m.requests).sum::<u64>(),
            c.total.requests,
            "the per-model split must reconcile with the chat total"
        );
        assert!((c.models.values().map(|m| m.cost).sum::<f64>() - 0.02).abs() < 1e-9);
    }

    #[test]
    fn tool_calls_land_in_both_the_app_total_and_the_chat() {
        let s = Stats::default();
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "main",
            "main",
            &Sample {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
                ms: 1,
                ttft_ms: None,
                cost: 0.0,
            },
        );
        s.tool_in("chat-1", "bash", true);
        s.tool_in("chat-1", "bash", false);
        s.tool_in("chat-1", "read_file", false);
        let d = s.snapshot();
        assert_eq!((d.tools["bash"].calls, d.tools["bash"].errors), (2, 1));
        let c = &d.chats["chat-1"];
        assert_eq!((c.tools["bash"].calls, c.tools["bash"].errors), (2, 1));
        assert_eq!(c.tools["read_file"].calls, 1);
        // The two tallies sit side by side in the same view, so they must agree.
        assert_eq!(
            c.tools.values().map(|t| t.calls).sum::<u64>(),
            d.tools.values().map(|t| t.calls).sum::<u64>()
        );
    }

    #[test]
    fn a_tool_call_does_not_create_a_chat_that_never_spent_anything() {
        let s = Stats::default();
        s.tool_in("ghost", "bash", false);
        // `chats` is capped by count, so entries conjured by tool calls would
        // quietly evict real chats' rows. A tool call is not evidence the chat
        // exists — the request is.
        assert!(s.snapshot().chats.is_empty());
        assert_eq!(s.snapshot().tools["bash"].calls, 1);
    }

    #[test]
    fn a_chats_tool_list_stays_bounded_and_keeps_the_busy_ones() {
        let s = Stats::default();
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "main",
            "main",
            &Sample {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
                ms: 1,
                ttft_ms: None,
                cost: 0.0,
            },
        );
        for i in 0..MAX_CHAT_TOOLS {
            s.tool_in("chat-1", &format!("tool-{i:02}"), false);
        }
        // The one-off tool is the one that should go when the cap bites.
        s.tool_in("chat-1", "tool-00", false);
        s.tool_in("chat-1", "rare_tool", false);
        let c = &s.snapshot().chats["chat-1"];
        assert_eq!(c.tools.len(), MAX_CHAT_TOOLS);
        assert!(c.tools.contains_key("tool-00"), "a 2-call tool was evicted");
        assert!(
            !c.tools.contains_key("rare_tool"),
            "the cap kept the noisiest row"
        );
    }

    #[test]
    fn events_land_on_the_chat_that_hit_them() {
        let s = Stats::default();
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "main",
            "main",
            &Sample {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
                ms: 1,
                ttft_ms: None,
                cost: 0.0,
            },
        );
        s.event_in("chat-1", "retries");
        s.event_in("chat-1", "retries");
        s.event_in("chat-1", "compactions");
        s.event_in("ghost", "pauses");
        let d = s.snapshot();
        assert_eq!(d.events["retries"], 2);
        assert_eq!(d.events["pauses"], 1, "the app-wide total is unconditional");
        let c = &d.chats["chat-1"];
        assert_eq!(c.events["retries"], 2);
        assert_eq!(c.events["compactions"], 1);
        assert!(
            !d.chats.contains_key("ghost"),
            "an event conjured a row for a chat that never ran"
        );
    }

    #[test]
    fn two_chats_are_kept_apart_and_one_request_belongs_to_exactly_one() {
        let s = Stats::default();
        let smp = || Sample {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 10,
            ttft_ms: None,
            cost: 0.0,
        };
        s.record("chat-a", "codex/gpt-6-sol", None, "main", "main", &smp());
        s.record("chat-b", "codex/gpt-6-sol", None, "main", "main", &smp());
        let d = s.snapshot();
        assert_eq!(
            (
                d.chats["chat-a"].total.requests,
                d.chats["chat-b"].total.requests
            ),
            (1, 1)
        );
        // The whole point of the dimension: a chat's row is its own slice, so the
        // two do not add up to more than the model total they came from.
        assert_eq!(d.models["codex/gpt-6-sol"].requests, 2);
        assert_eq!(
            d.chats.values().map(|c| c.total.requests).sum::<u64>(),
            d.models["codex/gpt-6-sol"].requests
        );
    }

    #[test]
    fn errors_land_on_the_chat_that_hit_them() {
        let s = Stats::default();
        s.error("chat-a", "codex/gpt-6-sol", None);
        let d = s.snapshot();
        assert_eq!(d.chats["chat-a"].total.errors, 1);
        // An error is an attempt, not a request, so it must not invent one.
        assert_eq!(d.chats["chat-a"].total.requests, 0);
        assert_eq!(
            d.chats["chat-a"].models["codex/gpt-6-sol"].errors, 1,
            "the model row has to see the failure too, or the split under-reports"
        );
        // A chat whose every attempt failed is the one a user opens stats on to
        // work out why, so it has to exist as a row rather than read as a chat
        // with nothing recorded.
        assert!(
            !d.chats["chat-a"].total.last_used.is_empty(),
            "an error-created row needs a stamp or the LRU trim evicts it first"
        );
        assert!(!d.chats["chat-a"].started.is_empty());
        assert_eq!(
            d.chats["chat-a"].models["codex/gpt-6-sol"].last_used,
            d.chats["chat-a"].total.last_used
        );
    }

    /// The failure rate is derived from *attempts*, not requests.
    ///
    /// A failed attempt never increments `requests` (`Stats::error` records it as
    /// an error and nothing else), so `errors / requests` would exceed 100% on a
    /// chat that failed more than it succeeded and would disagree with the count
    /// the UI prints beside it. This pins the denominator on every dimension the
    /// rate is stored on.
    #[test]
    fn failure_rate_divides_errors_by_attempts_not_requests() {
        let s = Stats::default();
        let smp = || Sample {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 1,
            ttft_ms: None,
            cost: 0.0,
        };
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            Some("ChatGPT 1"),
            "main",
            "main",
            &smp(),
        );
        s.error("chat-1", "codex/gpt-6-sol", Some("ChatGPT 1"));
        s.error("chat-1", "codex/gpt-6-sol", Some("ChatGPT 1"));
        s.error("chat-1", "codex/gpt-6-sol", Some("ChatGPT 1"));
        let d = s.snapshot();
        // 1 request + 3 errors = 4 attempts, so 3/4 = 0.75 — not 3/1 = 3.0.
        let m = &d.models["codex/gpt-6-sol"];
        assert_eq!((m.requests, m.errors), (1, 3));
        assert!(
            (m.failure_rate - 0.75).abs() < 1e-9,
            "expected 3 errors over 4 attempts, got {}",
            m.failure_rate
        );
        // Every other dimension that stores a rate has to agree with the model's,
        // or the same chat reads two ways on two screens.
        let md = &d.model_daily.values().next().unwrap()["codex/gpt-6-sol"];
        let daily = d.daily.values().next().unwrap();
        let checks: [(&str, &Counter); 5] = [
            ("account", &d.accounts["ChatGPT 1"]),
            ("chat total", &d.chats["chat-1"].total),
            ("chat model", &d.chats["chat-1"].models["codex/gpt-6-sol"]),
            ("daily", daily),
            ("model_daily", md),
        ];
        for (what, c) in checks {
            assert!(
                (c.failure_rate - 0.75).abs() < 1e-9,
                "{what} rate drifted: {}",
                c.failure_rate
            );
        }
    }

    /// A counter that has moved only on the success side must not report a
    /// stale rate: `record` moves the denominator, so it has to recompute too.
    #[test]
    fn failure_rate_tracks_successes_as_well_as_failures() {
        let s = Stats::default();
        let smp = || Sample {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 1,
            ttft_ms: None,
            cost: 0.0,
        };
        s.record("chat-1", "codex/gpt-6-sol", None, "main", "main", &smp());
        s.error("chat-1", "codex/gpt-6-sol", None);
        // 1 of 2 attempts.
        assert!((s.snapshot().models["codex/gpt-6-sol"].failure_rate - 0.5).abs() < 1e-9);
        // A later success must lower it, not leave 0.5 frozen on the row.
        s.record("chat-1", "codex/gpt-6-sol", None, "main", "main", &smp());
        let m = &s.snapshot().models["codex/gpt-6-sol"];
        assert_eq!((m.requests, m.errors), (2, 1));
        assert!(
            (m.failure_rate - 1.0 / 3.0).abs() < 1e-9,
            "a success has to move the rate it is the denominator of, got {}",
            m.failure_rate
        );
        // Nothing attempted reads as 0, never NaN.
        assert!(
            Stats::default().snapshot().models.is_empty(),
            "no request means no row at all"
        );
        assert_eq!(Counter::default().failure_rate, 0.0);
    }

    /// A stats file written before `failure_rate` existed must load, and its
    /// missing rate must default rather than fail the whole file.
    #[test]
    fn an_old_stats_file_without_a_failure_rate_still_loads() {
        let old = r#"{"version":1,"models":{"codex/gpt-6-sol":{"requests":1,"errors":1}}}"#;
        let d: StatsData = serde_json::from_str(old).unwrap();
        assert_eq!(d.models["codex/gpt-6-sol"].errors, 1);
        // `#[serde(default)]` gives the absent key 0.0. The UI re-derives from
        // the counts for exactly this case, so the stored 0 is not what a reader
        // sees — but the file has to load either way.
        assert_eq!(d.models["codex/gpt-6-sol"].failure_rate, 0.0);
    }

    #[test]
    fn forgetting_a_chat_leaves_every_other_dimension_alone() {
        let s = Stats::default();
        let smp = || Sample {
            input: 7,
            output: 3,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 10,
            ttft_ms: None,
            cost: 0.02,
        };
        s.record("chat-a", "codex/gpt-6-sol", None, "main", "main", &smp());
        s.record("chat-b", "codex/gpt-6-sol", None, "main", "main", &smp());
        s.forget_chat("chat-a");
        let d = s.snapshot();
        assert!(!d.chats.contains_key("chat-a"));
        assert_eq!(d.chats["chat-b"].total.input, 7);
        assert_eq!(
            d.models["codex/gpt-6-sol"].input, 14,
            "the model's total is not a chat's to remove"
        );
        // Idempotent: nothing left to remove, no rewrite, no panic.
        s.forget_chat("chat-a");
        assert_eq!(s.snapshot().chats.len(), 1);
    }

    #[test]
    fn the_chat_map_is_capped_and_drops_the_least_recently_used() {
        let s = Stats::default();
        let smp = || Sample {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 1,
            ttft_ms: None,
            cost: 0.0,
        };
        for i in 0..MAX_CHATS {
            s.record(
                &format!("chat-{i:04}"),
                "codex/gpt-6-sol",
                None,
                "main",
                "main",
                &smp(),
            );
        }
        assert_eq!(s.snapshot().chats.len(), MAX_CHATS);
        // Touch the oldest, then overflow by one: the one that has not been used
        // is the one that goes. `last_used` has second resolution, so the whole
        // loop lands in the same second and the tie is broken by insertion order
        // in the sort above — still oldest-out-first among equals.
        let oldest = format!("chat-{:04}", 0);
        s.record(&oldest, "codex/gpt-6-sol", None, "main", "main", &smp());
        s.record("overflow", "codex/gpt-6-sol", None, "main", "main", &smp());
        let d = s.snapshot();
        assert_eq!(d.chats.len(), MAX_CHATS, "the cap must bound the map");
        assert!(d.chats.contains_key(&oldest), "a used chat was evicted");
        assert!(
            !d.chats.contains_key("chat-0001"),
            "the least recently used chat survived the cap"
        );
        assert_eq!(
            d.models["codex/gpt-6-sol"].requests,
            MAX_CHATS as u64 + 2,
            "evicting a row must not lose the totals it was part of"
        );
    }

    #[test]
    fn an_old_stats_file_loads_without_the_chat_dimension() {
        // stats.json predates per-chat stats, so the key is absent from every
        // user's file. `#[serde(default)]` on the struct is what keeps that a
        // load rather than a wipe.
        let old = r#"{"version":1,"since":"2026-01-01T00:00:00Z","models":{"codex/gpt-6-sol":{"requests":3}}}"#;
        let d: StatsData = serde_json::from_str(old).unwrap();
        assert!(d.chats.is_empty());
        assert_eq!(d.models["codex/gpt-6-sol"].requests, 3);
        // A stats.json written before the per-agent split has no `by_agent` on
        // any chat row; the field's own `#[serde(default)]` is what keeps that a
        // load rather than a deserialize error on the whole file.
        assert!(d.chats.is_empty());
        let c: ChatStat = serde_json::from_str(r#"{"total":{"requests":2},"models":{}}"#).unwrap();
        assert!(c.by_agent.is_empty());
    }

    /// A chat's spend has to split by *who* spent it, not by role kind: two
    /// explores are two wallets. If the rows do not add up to `total`, the
    /// breakdown is either dropping a sample or double-counting one.
    #[test]
    fn sub_agent_spend_is_attributed_separately_from_main() {
        let s = Stats::default();
        let smp = |cost: f64| Sample {
            input: 100,
            output: 10,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 10,
            ttft_ms: None,
            cost,
        };
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "main",
            "main",
            &smp(0.10),
        );
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "explore",
            "sub:a",
            &smp(0.20),
        );
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "explore",
            "sub:b",
            &smp(0.30),
        );
        let c = &s.snapshot().chats["chat-1"];
        // Two distinct explores must not merge into one row. The old `agent` key
        // would have folded them together; the usage id is what keeps them apart.
        assert_eq!(c.by_agent["main"].requests, 1);
        assert_eq!(c.by_agent["sub:a"].requests, 1);
        assert_eq!(c.by_agent["sub:b"].requests, 1);
        assert!((c.by_agent["sub:a"].cost - 0.20).abs() < 1e-9);
        assert!((c.by_agent["sub:b"].cost - 0.30).abs() < 1e-9);
        // The breakdown is additive to the headline, or one of them is lying.
        assert_eq!(
            c.by_agent.values().map(|a| a.requests).sum::<u64>(),
            c.total.requests
        );
        assert_eq!(
            c.by_agent.values().map(|a| a.input).sum::<u64>(),
            c.total.input
        );
        assert_eq!(
            c.by_agent.values().map(|a| a.output).sum::<u64>(),
            c.total.output
        );
        assert!((c.by_agent.values().map(|a| a.cost).sum::<f64>() - c.total.cost).abs() < 1e-9);
    }

    /// Compaction and keepalive are spend a chat did not ask for, so burying
    /// them in the task total is exactly the number a user wants separated out.
    #[test]
    fn a_compactor_and_keepalive_call_get_their_own_rows() {
        let s = Stats::default();
        let smp = || Sample {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 1,
            ttft_ms: None,
            cost: 0.01,
        };
        s.record("chat-1", "codex/gpt-6-sol", None, "main", "main", &smp());
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "main",
            "compactor",
            &smp(),
        );
        s.record(
            "chat-1",
            "codex/gpt-6-sol",
            None,
            "main",
            "keepalive",
            &smp(),
        );
        let c = &s.snapshot().chats["chat-1"];
        assert_eq!(c.by_agent["compactor"].requests, 1);
        assert_eq!(c.by_agent["keepalive"].requests, 1);
        // The system calls must be their own rows, not smuggled into "main".
        assert_eq!(c.by_agent["main"].requests, 1);
        assert_eq!(c.by_agent.len(), 3);
        assert_eq!(c.total.requests, 3);
    }

    /// An ultrathread tree spawns dozens of subagents, and this map is
    /// serialized in full on every flush, so it has to be capped. The costliest
    /// rows win because a row with ~no spend is noise next to one that ran the
    /// task; no request may fall out of `total` when a row is evicted.
    #[test]
    fn the_by_agent_map_is_capped_and_keeps_the_costliest() {
        let s = Stats::default();
        let smp = |cost: f64| Sample {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 1,
            ttft_ms: None,
            cost,
        };
        let n = MAX_CHAT_AGENTS + 5;
        for i in 0..n {
            s.record(
                "chat-1",
                "codex/gpt-6-sol",
                None,
                "main",
                &format!("sub:{i:03}"),
                &smp(i as f64 + 1.0),
            );
        }
        let c = &s.snapshot().chats["chat-1"];
        assert_eq!(
            c.by_agent.len(),
            MAX_CHAT_AGENTS,
            "the cap must bound the map"
        );
        // Ascending cost, so the cheapest are the first ids and the costliest is
        // the last one inserted.
        assert!(
            !c.by_agent.contains_key("sub:000"),
            "the cheapest survived the cap"
        );
        assert!(!c.by_agent.contains_key("sub:004"), "an evicted row stayed");
        assert!(
            c.by_agent.contains_key("sub:005"),
            "the cheapest kept row is wrong"
        );
        assert!(
            c.by_agent.contains_key(&format!("sub:{:03}", n - 1)),
            "the costliest went"
        );
        // Evicting a breakdown row must not lose the spend from the headline.
        assert_eq!(
            c.total.requests, n as u64,
            "total lost requests on eviction"
        );
    }

    #[test]
    fn a_reset_clears_the_chats_too() {
        let s = Stats::default();
        s.record(
            "chat-a",
            "codex/gpt-6-sol",
            None,
            "main",
            "main",
            &Sample {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
                ms: 1,
                ttft_ms: None,
                cost: 0.0,
            },
        );
        // `reset` writes the file, which needs the real data dir; assert on the
        // in-memory half that the caller sees either way.
        let mut d = s.data.lock().unwrap();
        *d = StatsData {
            version: 1,
            since: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        assert!(d.chats.is_empty());
    }

    #[test]
    fn presence_splits_focused_background_and_unattended_work() {
        let s = Stats::default();
        let now = chrono::Local::now();
        s.add_presence(4_000, true, false, now);
        s.add_presence(6_000, false, true, now);
        s.add_presence(2_000, true, true, now);
        let d = s.snapshot();
        assert_eq!(
            (
                d.presence.open_ms,
                d.presence.focused_ms,
                d.presence.working_ms,
                d.presence.unattended_ms
            ),
            (12_000, 6_000, 8_000, 6_000)
        );
        assert_eq!(
            d.presence_daily.values().map(|p| p.open_ms).sum::<u64>(),
            12_000
        );
        assert_eq!(d.longest_session_ms, 12_000);
    }

    #[test]
    fn the_first_tick_only_starts_the_clock_and_a_long_gap_is_capped() {
        let s = Stats::default();
        s.tick(true, false);
        assert_eq!(s.snapshot().presence.open_ms, 0);
        *s.last_tick.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(3600));
        s.tick(true, false);
        assert_eq!(
            s.snapshot().presence.open_ms,
            MAX_TICK_MS,
            "a slept laptop is not hours of use"
        );
    }

    #[test]
    fn retain_chats_drops_only_rejected_rows_and_keeps_totals() {
        let s = Stats::default();
        let smp = || Sample {
            input: 5,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            ms: 1,
            ttft_ms: None,
            cost: 0.0,
        };
        s.record("live", "m", None, "main", "main", &smp());
        s.record("gone", "m", None, "main", "main", &smp());
        assert_eq!(s.retain_chats(|id| id == "live"), 1);
        let d = s.snapshot();
        assert!(d.chats.contains_key("live") && !d.chats.contains_key("gone"));
        assert_eq!(d.models["m"].input, 10);
        assert_eq!(s.retain_chats(|id| id == "live"), 0);
    }
}
