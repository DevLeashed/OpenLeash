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
    }
}

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
/// sub-agents, each with its own row; past this the least expensive fall out.
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
        Stats {
            data: Mutex::new(d),
            dirty: AtomicBool::new(false),
        }
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
            // every flush, and a big ultrathread tree is dozens of sub-agents each
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
    /// Not called when a chat is deleted — the numbers for a chat that is gone
    /// are still what was spent on it, and a usage ledger that shrinks as you
    /// tidy up stops adding up. This exists for the case where a chat's id is
    /// *wrong* rather than gone (a fork reusing one), which would otherwise
    /// silently merge two chats' history into one row forever.
    #[allow(dead_code)]
    pub fn forget_chat(&self, chat: &str) {
        let mut d = self.data.lock().unwrap();
        if d.chats.remove(chat).is_some() {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    pub fn error(&self, chat: &str, model: &str, account: Option<&str>) {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut d = self.data.lock().unwrap();
        d.models.entry(model.into()).or_default().errors += 1;
        if let Some(a) = account {
            d.accounts.entry(a.into()).or_default().errors += 1;
        }
        d.daily.entry(day.clone()).or_default().errors += 1;
        // A failed attempt is the chat's spend of attention too, and unlike a tool call
        // this one *creates* the row: a chat whose every attempt failed is
        // exactly the one a user opens stats on to find out why, and it would
        // otherwise show up as a chat with nothing recorded at all. `last_used`
        // is stamped because the row exists now and the LRU trim reads it.
        let c = d.chats.entry(chat.into()).or_default();
        c.total.errors += 1;
        c.total.last_used = chrono::Utc::now().to_rfc3339();
        if c.models.get(model).is_none() {
            // Keep the model's `last_used` in step with the chat's: the LRU trim
            // reads the chat's stamp, but a model row with an empty one is the
            // shape the per-model charts sort on, and "never used" would read
            // as older than everything.
            c.models.entry(model.into()).or_default().last_used = c.total.last_used.clone();
        }
        c.models.entry(model.into()).or_default().errors += 1;
        if c.started.is_empty() {
            c.started = c.total.last_used.clone();
        }
        d.model_daily
            .entry(day)
            .or_default()
            .entry(model.into())
            .or_default()
            .errors += 1;
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
        *self.data.lock().unwrap() = StatsData {
            version: 1,
            since: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
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
            c.tools.iter().map(|(_, t)| t.calls).sum::<u64>(),
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

    /// An ultrathread tree spawns dozens of sub-agents, and this map is
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
}
