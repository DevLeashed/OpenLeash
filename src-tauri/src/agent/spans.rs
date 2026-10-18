//! The run timeline: where one chat's run spent its time and money.
//!
//! The harness already *counts* a run — `stats.rs` keeps per-chat totals and
//! `trace.rs` can ship OTLP spans to a collector the user names — but neither
//! keeps a per-chat record of *when* things happened. Stats are sums with no
//! timestamps, and trace export is off by default and leaves the machine. So
//! this module keeps a small, local list of timed [`Span`]s on the task itself
//! (`Task::spans`), saved with it, and [`assemble`] turns that list (plus the
//! transcript, for chats that predate it) into lanes the UI draws.
//!
//! What is recorded, and where:
//!
//! | Span | Recorded by |
//! |---|---|
//! | `model` — one attempt, success or failure | `router::request` |
//! | `wait` `retry` — the backoff between attempts | `router::request` |
//! | `tool` — one tool call | `runner` `exec` |
//! | `wait` `approval` / `question` — a blocking wait on the user | `Harness::wait_for_user` |
//! | `wait` `pause` — opened when the chat pauses, closed when it resumes | `Harness::pause_task`, `Harness::wait_unpaused` |
//! | `wait` `park` — `wait_for_event` | `runner::park_waiting` |
//! | `bg` — a background command, from start to exit | `runner` `bash` |
//!
//! Like trace export it carries names, counts, durations and outcomes only:
//! never a prompt, a tool argument or a tool's output. The one free-text field,
//! `detail`, holds a truncated provider error or pause reason — text the
//! transcript already shows as a notice.
//!
//! Old tasks have no `spans` field at all; `#[serde(default)]` on the task keeps
//! them loading, and [`assemble`] estimates their blocks from item timestamps
//! (flagged `estimated`) so reopening one shows a partial picture, not nothing.

use super::shell::BgInfo;
use super::{Harness, Item, Task};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Most spans one task keeps. A span is ~200 bytes on disk, so this caps the
/// timeline at about a megabyte per chat, which is small beside its transcript.
/// The oldest go first: the start of a very long run then falls back to the
/// estimated view `assemble` builds from the transcript, which degrades the
/// picture rather than losing the run.
pub const MAX_SPANS: usize = 6000;

/// How many to drop at once when the cap is hit, so a full list is not shifted
/// on every single push.
const DROP_CHUNK: usize = MAX_SPANS / 10;

/// Longest a single provider error or pause reason is kept.
const DETAIL_CHARS: usize = 160;

/// One timed thing that happened in a run. See the module table for kinds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// model | tool | wait | bg
    pub kind: String,
    /// The subagent this belongs to; empty = the main agent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    pub start: DateTime<Utc>,
    /// `None` while still open (a pause or park that has not ended yet).
    #[serde(default)]
    pub end: Option<DateTime<Utc>>,
    /// The model id, tool name, wait kind (approval | question | pause | park |
    /// retry) or background command line (truncated).
    #[serde(default)]
    pub name: String,
    /// ok | error | denied | cancelled | paused, a background job's exit text,
    /// or a wait's outcome (answered | cancelled).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// The transcript row this stands for, when there is one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub item: String,
    /// Model: the account label or key hint that served it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
    /// Model: the spend attribution id (`main`, `sub:<id>`, `compactor`, …).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub input: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub cost: f64,
    /// Model: which try against this target (1 = first).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub attempt: u32,
    /// Model: served by something other than the first choice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// A truncated provider error or pause reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_f(v: &f64) -> bool {
    *v == 0.0
}

impl Span {
    pub fn new(kind: &str, agent: Option<&str>, name: impl Into<String>) -> Self {
        Span {
            kind: kind.into(),
            agent: agent.unwrap_or_default().to_string(),
            start: Utc::now(),
            name: name.into(),
            ..Default::default()
        }
    }

    /// Set the free-text detail, truncated so a 4 KB provider error does not
    /// end up stored once per retry.
    pub fn detail(mut self, text: &str) -> Self {
        self.detail = text.chars().take(DETAIL_CHARS).collect();
        self
    }

    /// A pause or park still waiting for the run to come back.
    fn is_open_wait(&self) -> bool {
        self.kind == "wait" && self.end.is_none() && matches!(self.name.as_str(), "pause" | "park")
    }
}

/// Add a span without interpreting work as a state transition. Subagents,
/// side requests and background jobs can drain while the main agent is held.
/// Only an actual resume/wake transition may end a pause or park.
pub fn push(spans: &mut Vec<Span>, span: Span) {
    spans.push(span);
    if spans.len() > MAX_SPANS {
        let excess = spans.len() - MAX_SPANS + DROP_CHUNK;
        spans.drain(..excess.min(spans.len()));
    }
}

/// Close the named wait on an actual state transition, never on work draining.
/// Keep pause and park separate: lifting a global pause does not wake a park.
pub fn close_waits(spans: &mut [Span], name: &str, at: DateTime<Utc>) {
    for s in spans
        .iter_mut()
        .filter(|s| s.is_open_wait() && s.name == name)
    {
        s.end = Some(at.max(s.start));
    }
}

/// Record a finished span on a task. Silent when the task is gone: a timing is
/// never worth failing the work it measured.
///
/// The caller must not hold this task's lock.
pub async fn record(h: &Harness, task_id: &str, span: Span) {
    if let Ok(t) = h.task(task_id).await {
        push(&mut t.lock().await.spans, span);
    }
}

/// Which agent's transcript holds `item_id`: empty for the main agent's.
pub fn agent_of_item(t: &Task, item_id: &str) -> String {
    if t.items.iter().rev().any(|i| i.id == item_id) {
        return String::new();
    }
    t.sub_items
        .iter()
        .find(|(_, list)| list.iter().any(|i| i.id == item_id))
        .map(|(sid, _)| sid.clone())
        .unwrap_or_default()
}

// ─────────────────────────────── assembly ───────────────────────────────

/// One lane: the main agent, a subagent, or the background-command lane.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lane {
    /// `main`, a subagent id, or `bg`.
    pub id: String,
    pub label: String,
    pub role: String,
    /// The launching subagent's id; empty = launched by the main agent.
    pub parent: String,
    pub depth: u8,
    pub status: String,
    /// The transcript row that stands for this lane (a subagent's pill).
    pub item: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    /// Stacked rows needed for overlapping blocks (at least 1).
    pub rows: u32,
}

/// One drawn block. The model fields are empty/zero for other kinds.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Block {
    pub lane: String,
    pub row: u32,
    pub kind: String,
    pub name: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Still running: `end_ms` is the assembly's `now_ms`.
    pub open: bool,
    pub status: String,
    pub item: String,
    /// Guessed from transcript timestamps, not measured.
    pub estimated: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub account: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "is_zero")]
    pub input: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub output: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    #[serde(skip_serializing_if = "is_zero_f")]
    pub cost: f64,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub attempt: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Block {
    fn ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ModelTotal {
    pub model: String,
    pub requests: u64,
    pub failed: u64,
    pub cost: f64,
    pub ms: i64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SlowTool {
    pub name: String,
    pub ms: i64,
    pub lane: String,
    pub item: String,
    pub start_ms: i64,
}

/// Headline figures. `you_ms` … `idle_ms` are an *exclusive* split of
/// `wall_ms`: each instant is counted once, in the first category active on any
/// lane in the order you → paused → model → retry → tool → idle. Summing per
/// block instead would let three parallel subagents report three hours of
/// "model time" inside one hour of wall clock, which reads as nonsense.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Totals {
    pub wall_ms: i64,
    pub you_ms: i64,
    pub paused_ms: i64,
    pub model_ms: i64,
    pub retry_ms: i64,
    pub tool_ms: i64,
    pub idle_ms: i64,
    /// Union of background-command time; runs alongside the split above.
    pub bg_ms: i64,
    pub requests: u64,
    pub failed_requests: u64,
    pub retries: u64,
    pub fallbacks: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    /// Sum of the recorded model attempts.
    pub cost: f64,
    /// The chat's own running total, which also covers requests made before
    /// timings were recorded.
    pub task_cost: f64,
    pub by_model: Vec<ModelTotal>,
    pub slowest_tools: Vec<SlowTool>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunTimeline {
    pub task_id: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub now_ms: i64,
    /// The task has recorded spans at all.
    pub recorded: bool,
    /// Some blocks are estimated from the transcript.
    pub partial: bool,
    pub lanes: Vec<Lane>,
    pub blocks: Vec<Block>,
    pub totals: Totals,
}

/// How many slowest tools the summary lists.
const SLOWEST: usize = 8;

/// An estimated gap longer than this is the user away, not work, and is left
/// out rather than drawn as a half-day tool call.
const ESTIMATE_CAP_MS: i64 = 30 * 60 * 1000;

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn block_from(s: &Span, lane: String, start_ms: i64, end_ms: i64, open: bool) -> Block {
    Block {
        lane,
        row: 0,
        kind: s.kind.clone(),
        name: s.name.clone(),
        start_ms,
        end_ms: end_ms.max(start_ms),
        open,
        status: s.status.clone(),
        item: s.item.clone(),
        estimated: false,
        account: s.account.clone(),
        role: s.role.clone(),
        ttft_ms: s.ttft_ms,
        input: s.input,
        output: s.output,
        cache_read: s.cache_read,
        cache_write: s.cache_write,
        cost: s.cost,
        attempt: s.attempt,
        fallback: s.fallback,
        detail: s.detail.clone(),
    }
}

fn estimated(lane: &str, kind: &str, name: &str, start: i64, end: i64, item: &str) -> Block {
    Block {
        lane: lane.into(),
        row: 0,
        kind: kind.into(),
        name: name.into(),
        start_ms: start,
        end_ms: end.max(start),
        open: false,
        status: String::new(),
        item: item.into(),
        estimated: true,
        account: String::new(),
        role: String::new(),
        ttft_ms: None,
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cost: 0.0,
        attempt: 0,
        fallback: false,
        detail: String::new(),
    }
}

/// Blocks for one transcript, guessed from consecutive item timestamps, for the
/// stretch before anything was recorded (`before`).
///
/// Each gap between two rows is credited to what the first row was doing: a
/// tool row's gap is that tool running, an approval or question's is the user
/// deciding, and a user message's or reply's is the model producing what came
/// next. Coarse — a parallel batch reads as one tool after another — which is
/// why every such block is flagged and drawn differently.
fn estimate_items(lane: &str, items: &[Item], model: &str, before: Option<i64>) -> Vec<Block> {
    let rows: Vec<&Item> = items
        .iter()
        .filter(|i| {
            matches!(
                i.kind.as_str(),
                "user" | "text" | "thinking" | "tool" | "approval" | "question"
            )
        })
        .filter(|i| before.is_none_or(|b| ms(i.ts) < b))
        .collect();
    let mut out = vec![];
    for pair in rows.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (start, end) = (ms(a.ts), ms(b.ts));
        let end = before.map_or(end, |b| end.min(b));
        if end <= start || end - start > ESTIMATE_CAP_MS {
            continue;
        }
        let block = match a.kind.as_str() {
            "tool" => {
                let name = a.data["name"].as_str().unwrap_or("tool");
                let mut b = estimated(lane, "tool", name, start, end, &a.id);
                b.status = a.data["status"].as_str().unwrap_or_default().to_string();
                b
            }
            "approval" | "question" => estimated(lane, "wait", &a.kind, start, end, &a.id),
            // A user turn or the agent's own reply: the model working on what
            // comes next. Jump target is that next row, the model's output.
            _ => estimated(lane, "model", model, start, end, &b.id),
        };
        out.push(block);
    }
    out
}

/// The first row of `items` (sorted by time) the model wrote during a request
/// that ran `[start, end]`, so clicking the request jumps to what it produced.
fn output_item(items: &[Item], start: i64, end: i64) -> String {
    let from = items.partition_point(|i| ms(i.ts) < start - 1000);
    items[from..]
        .iter()
        .take_while(|i| ms(i.ts) <= end + 2000)
        .find(|i| matches!(i.kind.as_str(), "text" | "thinking" | "tool"))
        .map(|i| i.id.clone())
        .unwrap_or_default()
}

/// Greedy interval packing: each block takes the first row free at its start.
fn pack(blocks: &mut [Block]) -> u32 {
    blocks.sort_by_key(|b| (b.start_ms, std::cmp::Reverse(b.end_ms)));
    let mut ends: Vec<i64> = vec![];
    for b in blocks.iter_mut() {
        match ends.iter().position(|&e| e <= b.start_ms) {
            Some(r) => {
                ends[r] = b.end_ms.max(b.start_ms + 1);
                b.row = r as u32;
            }
            None => {
                ends.push(b.end_ms.max(b.start_ms + 1));
                b.row = (ends.len() - 1) as u32;
            }
        }
    }
    ends.len().max(1) as u32
}

/// Exclusive category split over all non-background blocks. Returns
/// `[you, paused, model, retry, tool]` milliseconds and the idle remainder.
fn split(blocks: &[Block], start: i64, end: i64) -> ([i64; 5], i64) {
    let cat = |b: &Block| -> Option<usize> {
        match (b.kind.as_str(), b.name.as_str()) {
            ("wait", "approval" | "question") => Some(0),
            ("wait", "pause" | "park") => Some(1),
            ("model", _) => Some(2),
            ("wait", "retry") => Some(3),
            ("tool", _) => Some(4),
            _ => None,
        }
    };
    let mut ev: Vec<(i64, i32, usize)> = vec![];
    for b in blocks {
        if let Some(c) = cat(b) {
            if b.end_ms > b.start_ms {
                ev.push((b.start_ms, 1, c));
                ev.push((b.end_ms, -1, c));
            }
        }
    }
    ev.sort();
    let mut active = [0i32; 5];
    let mut out = [0i64; 5];
    let mut idle = 0i64;
    let mut prev = start;
    for (t, d, c) in ev {
        let t = t.clamp(start, end);
        if t > prev {
            match active.iter().position(|&n| n > 0) {
                Some(k) => out[k] += t - prev,
                None => idle += t - prev,
            }
            prev = t;
        }
        active[c] += d;
    }
    if end > prev {
        idle += end - prev;
    }
    (out, idle)
}

fn union_ms(mut iv: Vec<(i64, i64)>) -> i64 {
    iv.sort();
    let (mut total, mut cur): (i64, Option<(i64, i64)>) = (0, None);
    for (s, e) in iv {
        match cur {
            Some((cs, ce)) if s <= ce => cur = Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                total += ce - cs;
                cur = Some((s, e));
            }
            None => cur = Some((s, e)),
        }
    }
    if let Some((cs, ce)) = cur {
        total += ce - cs;
    }
    total
}

/// Build the lanes, blocks and totals the UI draws for one task.
///
/// `live_bg` is the task's background-job list right now, so a command still
/// running shows as an open block; finished ones come from their recorded span.
pub fn assemble(t: &Task, live_bg: &[BgInfo], now: DateTime<Utc>) -> RunTimeline {
    let now_ms = ms(now);
    let known: std::collections::HashSet<&str> = t.subs.iter().map(|s| s.id.as_str()).collect();
    let lane_of = |s: &Span| -> String {
        if s.kind == "bg" {
            "bg".into()
        } else if s.agent.is_empty() || !known.contains(s.agent.as_str()) {
            "main".into()
        } else {
            s.agent.clone()
        }
    };

    let mut blocks: Vec<Block> = vec![];
    for s in &t.spans {
        let start = ms(s.start);
        let (end, open) = match s.end {
            Some(e) => (ms(e), false),
            None => {
                // An open wait remains open as long as its own state is held.
                // Other lanes, background jobs and side requests may finish
                // during it; none of those imply a resume or wake.
                let held = match s.name.as_str() {
                    "park" => t.waiting_kind.as_deref() == Some("wake"),
                    "pause" => t.paused.is_some(),
                    _ => false,
                };
                if held {
                    (now_ms, true)
                } else {
                    // Legacy/recovered records with no transition timestamp:
                    // label their last persisted change, not another span.
                    (ms(t.updated_at).max(start), false)
                }
            }
        };
        blocks.push(block_from(s, lane_of(s), start, end, open));
    }
    let recorded = !blocks.is_empty();

    // Background commands still running: open blocks, unless already recorded.
    for b in live_bg.iter().filter(|b| b.running) {
        let name: String = b.cmd.chars().take(200).collect();
        let start = ms(b.started);
        if blocks
            .iter()
            .any(|x| x.kind == "bg" && x.start_ms == start && x.name == name)
        {
            continue;
        }
        let mut blk = estimated("bg", "bg", &name, start, now_ms, "");
        blk.estimated = false;
        blk.open = true;
        blk.status = "running".into();
        blocks.push(blk);
    }

    // Before the first recorded span (or for the whole chat, when nothing was
    // recorded) estimate from the transcript. Per lane: a subagent launched
    // after recording began has nothing to estimate.
    let first_of = |lane: &str| -> Option<i64> {
        t.spans
            .iter()
            .filter(|s| lane_of(s) == lane)
            .map(|s| ms(s.start))
            .min()
            .or_else(|| t.spans.first().map(|s| ms(s.start)))
    };
    let mut est = estimate_items("main", &t.items, &t.model, first_of("main"));
    for sub in &t.subs {
        if let Some(items) = t.sub_items.get(&sub.id) {
            let model = if sub.model.is_empty() {
                &t.model
            } else {
                &sub.model
            };
            est.extend(estimate_items(&sub.id, items, model, first_of(&sub.id)));
        }
    }
    let partial = !est.is_empty();
    blocks.extend(est);

    // Click-through for recorded model requests: the row the model produced.
    for b in blocks
        .iter_mut()
        .filter(|b| b.kind == "model" && b.item.is_empty())
    {
        let items = if b.lane == "main" {
            Some(&t.items)
        } else {
            t.sub_items.get(&b.lane)
        };
        if let Some(items) = items {
            b.item = output_item(items, b.start_ms, b.end_ms);
        }
    }

    // Lanes: main, then subagents depth-first in launch order, then bg.
    let mut by_lane: HashMap<String, Vec<Block>> = HashMap::new();
    for b in blocks {
        by_lane.entry(b.lane.clone()).or_default().push(b);
    }
    let mut order: Vec<Lane> = vec![Lane {
        id: "main".into(),
        label: "Main agent".into(),
        role: "main".into(),
        parent: String::new(),
        depth: 0,
        status: t.status.clone(),
        item: String::new(),
        start_ms: None,
        end_ms: None,
        rows: 1,
    }];
    let mut children: HashMap<&str, Vec<&super::SubInfo>> = HashMap::new();
    for s in &t.subs {
        let parent = if known.contains(s.parent.as_str()) {
            s.parent.as_str()
        } else {
            ""
        };
        children.entry(parent).or_default().push(s);
    }
    for list in children.values_mut() {
        // Stable: subs without a start keep their launch (vector) order.
        list.sort_by_key(|s| s.started.map(ms).unwrap_or(i64::MAX));
    }
    let mut stack: Vec<&super::SubInfo> = children
        .get("")
        .map(|l| l.iter().rev().copied().collect())
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    while let Some(s) = stack.pop() {
        if !seen.insert(s.id.as_str()) {
            continue;
        }
        let brief: String = s
            .task
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        order.push(Lane {
            id: s.id.clone(),
            label: if brief.is_empty() {
                s.role.clone()
            } else {
                format!("{} · {}", s.role, brief)
            },
            role: s.role.clone(),
            parent: s.parent.clone(),
            depth: s.depth.max(1),
            status: s.status.clone(),
            item: s.item_id.clone(),
            start_ms: s.started.map(ms),
            end_ms: if s.status == "running" {
                Some(now_ms)
            } else {
                None
            },
            rows: 1,
        });
        if let Some(kids) = children.get(s.id.as_str()) {
            stack.extend(kids.iter().rev().copied());
        }
    }
    if by_lane.contains_key("bg") {
        order.push(Lane {
            id: "bg".into(),
            label: "Background commands".into(),
            role: "bg".into(),
            parent: String::new(),
            depth: 0,
            status: String::new(),
            item: String::new(),
            start_ms: None,
            end_ms: None,
            rows: 1,
        });
    }

    let mut blocks: Vec<Block> = vec![];
    for lane in &mut order {
        let mut list = by_lane.remove(&lane.id).unwrap_or_default();
        lane.rows = pack(&mut list);
        if let Some(lo) = list.iter().map(|b| b.start_ms).min() {
            lane.start_ms = Some(lane.start_ms.map_or(lo, |s| s.min(lo)));
        }
        if let Some(hi) = list.iter().map(|b| b.end_ms).max() {
            lane.end_ms = Some(lane.end_ms.map_or(hi, |e| e.max(hi)));
        }
        blocks.extend(list);
    }

    let start_ms = blocks.iter().map(|b| b.start_ms).min();
    let end_ms = blocks.iter().map(|b| b.end_ms).max();
    let totals = totals(t, &blocks, start_ms, end_ms);
    RunTimeline {
        task_id: t.id.clone(),
        start_ms,
        end_ms,
        now_ms,
        recorded,
        partial,
        lanes: order,
        blocks,
        totals,
    }
}

fn totals(t: &Task, blocks: &[Block], start: Option<i64>, end: Option<i64>) -> Totals {
    let mut out = Totals {
        task_cost: t.usage.cost,
        ..Default::default()
    };
    let (Some(start), Some(end)) = (start, end) else {
        return out;
    };
    out.wall_ms = end - start;
    let (cats, idle) = split(blocks, start, end);
    [
        out.you_ms,
        out.paused_ms,
        out.model_ms,
        out.retry_ms,
        out.tool_ms,
    ] = cats;
    out.idle_ms = idle;
    out.bg_ms = union_ms(
        blocks
            .iter()
            .filter(|b| b.kind == "bg")
            .map(|b| (b.start_ms, b.end_ms))
            .collect(),
    );
    let mut models: HashMap<&str, ModelTotal> = HashMap::new();
    for b in blocks.iter().filter(|b| !b.estimated) {
        match b.kind.as_str() {
            "model" => {
                let ok = b.status.is_empty() || b.status == "ok";
                if ok {
                    out.requests += 1;
                } else {
                    out.failed_requests += 1;
                }
                out.retries += u64::from(b.attempt > 1);
                out.fallbacks += u64::from(b.fallback);
                out.cost += b.cost;
                let m = models.entry(b.name.as_str()).or_insert_with(|| ModelTotal {
                    model: b.name.clone(),
                    ..Default::default()
                });
                if ok {
                    m.requests += 1;
                } else {
                    m.failed += 1;
                }
                m.cost += b.cost;
                m.ms += b.ms();
                m.input += b.input;
                m.output += b.output;
                m.cache_read += b.cache_read;
                m.cache_write += b.cache_write;
            }
            "tool" => {
                out.tool_calls += 1;
                out.tool_errors += u64::from(matches!(b.status.as_str(), "error" | "denied"));
            }
            _ => {}
        }
    }
    out.by_model = models.into_values().collect();
    out.by_model.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.ms.cmp(&a.ms))
            .then(a.model.cmp(&b.model))
    });
    // Measured tools when there are any; an old chat only has estimates.
    let any_measured = blocks.iter().any(|b| b.kind == "tool" && !b.estimated);
    let mut tools: Vec<&Block> = blocks
        .iter()
        .filter(|b| b.kind == "tool" && b.estimated != any_measured)
        .collect();
    tools.sort_by_key(|b| (std::cmp::Reverse(b.ms()), b.start_ms));
    out.slowest_tools = tools
        .into_iter()
        .take(SLOWEST)
        .map(|b| SlowTool {
            name: b.name.clone(),
            ms: b.ms(),
            lane: b.lane.clone(),
            item: b.item.clone(),
            start_ms: b.start_ms,
        })
        .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::SubInfo;
    use chrono::TimeZone;
    use serde_json::json;

    fn at(s: i64) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_700_000_000_000 + s * 1000)
            .unwrap()
    }

    fn span(kind: &str, agent: &str, name: &str, from: i64, to: Option<i64>) -> Span {
        Span {
            kind: kind.into(),
            agent: agent.into(),
            name: name.into(),
            start: at(from),
            end: to.map(at),
            ..Default::default()
        }
    }

    fn item(id: &str, kind: &str, s: i64, data: serde_json::Value) -> Item {
        Item {
            id: id.into(),
            kind: kind.into(),
            text: String::new(),
            data,
            ts: at(s),
        }
    }

    fn sub(id: &str, parent: &str, depth: u8, started: i64, status: &str) -> SubInfo {
        SubInfo {
            id: id.into(),
            role: "general".into(),
            task: format!("work for {id}\nmore lines"),
            status: status.into(),
            parent: parent.into(),
            depth,
            started: Some(at(started)),
            item_id: format!("pill-{id}"),
            ..Default::default()
        }
    }

    fn task() -> Task {
        let mut t = crate::agent::tests::task("tl", "anthropic/claude-sonnet");
        t.status = "done".into();
        t.model = "anthropic/claude-sonnet".into();
        t.updated_at = at(500);
        t
    }

    /// The fixture: main agent with a failed-then-retried request, a tool, an
    /// approval inside it; a subagent and its own nested subagent running in
    /// parallel; a background job; and a span with missing optional fields.
    fn fixture() -> Task {
        let mut t = task();
        t.subs = vec![sub("s1", "", 1, 20, "done"), sub("s2", "s1", 2, 30, "done")];
        let mut failed = span("model", "", "anthropic/claude-sonnet", 0, Some(5));
        failed.status = "error".into();
        failed.attempt = 1;
        failed.detail = "HTTP 529 overloaded".into();
        let retry = span("wait", "", "retry", 5, Some(7));
        let mut ok = span("model", "", "openai/gpt-5", 7, Some(15));
        ok.status = "ok".into();
        ok.attempt = 2;
        ok.fallback = true;
        ok.cost = 0.25;
        ok.input = 1000;
        ok.output = 200;
        ok.ttft_ms = Some(900);
        let mut tool = span("tool", "", "bash", 15, Some(60));
        tool.status = "ok".into();
        tool.item = "tool-1".into();
        let mut appr = span("wait", "", "approval", 16, Some(26));
        appr.item = "appr-1".into();
        let mut sub_model = span("model", "s1", "anthropic/claude-sonnet", 20, Some(40));
        sub_model.cost = 0.5;
        sub_model.status = "ok".into();
        let mut nested_tool = span("tool", "s2", "read_file", 30, Some(35));
        nested_tool.status = "error".into();
        // Missing every optional field: no status, no item, no attempt.
        let bare = span("model", "s2", "", 35, Some(38));
        let mut bg = span("bg", "", "npm run dev", 40, Some(90));
        bg.status = "exit 0".into();
        t.spans = vec![
            failed,
            retry,
            ok,
            tool,
            appr,
            sub_model,
            nested_tool,
            bare,
            bg,
        ];
        t.items = vec![
            item("u1", "user", 0, json!({})),
            item("txt-1", "text", 9, json!({})),
            item("tool-1", "tool", 14, json!({"name": "bash"})),
        ];
        t.usage.cost = 0.9;
        t
    }

    #[test]
    fn nested_subagents_get_lanes_in_tree_order() {
        let tl = assemble(&fixture(), &[], at(100));
        let ids: Vec<&str> = tl.lanes.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["main", "s1", "s2", "bg"]);
        let s2 = &tl.lanes[2];
        assert_eq!((s2.parent.as_str(), s2.depth), ("s1", 2));
        assert_eq!(s2.label, "general · work for s2");
        assert_eq!(s2.item, "pill-s2");
        assert!(tl.recorded);
        assert!(!tl.partial, "everything here was recorded");
    }

    #[test]
    fn retries_fallbacks_and_costs_are_totalled() {
        let tl = assemble(&fixture(), &[], at(100));
        let tt = &tl.totals;
        assert_eq!(tt.requests, 3, "ok + sub ok + the bare one");
        assert_eq!(tt.failed_requests, 1);
        assert_eq!(tt.retries, 1);
        assert_eq!(tt.fallbacks, 1);
        assert!((tt.cost - 0.75).abs() < 1e-9);
        assert!((tt.task_cost - 0.9).abs() < 1e-9);
        assert_eq!(tt.tool_calls, 2);
        assert_eq!(tt.tool_errors, 1);
        assert_eq!(tt.by_model[0].model, "anthropic/claude-sonnet");
        assert_eq!(tt.by_model[0].failed, 1);
        assert_eq!(tt.slowest_tools[0].name, "bash");
        assert_eq!(tt.slowest_tools[0].ms, 45_000);
        assert_eq!(tt.slowest_tools[0].item, "tool-1");
    }

    #[test]
    fn the_wall_clock_split_is_exclusive_and_adds_up() {
        let tl = assemble(&fixture(), &[], at(100));
        let tt = &tl.totals;
        assert_eq!(tt.wall_ms, 90_000);
        assert_eq!(
            tt.you_ms + tt.paused_ms + tt.model_ms + tt.retry_ms + tt.tool_ms + tt.idle_ms,
            tt.wall_ms
        );
        // The approval (16..26) outranks the bash call it happened inside.
        assert_eq!(tt.you_ms, 10_000);
        assert_eq!(tt.retry_ms, 2_000);
        // model: 0..5, 7..15, and the sub's 20..40 beyond the approval (26..40)
        assert_eq!(tt.model_ms, 5_000 + 8_000 + 14_000);
        // tool: 15..16, then 40..60 once the sub's request is done
        assert_eq!(tt.tool_ms, 1_000 + 20_000);
        assert_eq!(tt.idle_ms, 30_000, "60..90 only the bg job ran");
        assert_eq!(tt.bg_ms, 50_000);
    }

    #[test]
    fn overlapping_blocks_stack_into_rows() {
        let tl = assemble(&fixture(), &[], at(100));
        let main = &tl.lanes[0];
        assert_eq!(main.rows, 2, "the approval overlaps the bash call");
        let appr = tl.blocks.iter().find(|b| b.name == "approval").unwrap();
        let bash = tl.blocks.iter().find(|b| b.name == "bash").unwrap();
        assert_ne!(appr.row, bash.row);
    }

    #[test]
    fn a_model_request_links_to_what_it_wrote() {
        let tl = assemble(&fixture(), &[], at(100));
        let ok = tl.blocks.iter().find(|b| b.name == "openai/gpt-5").unwrap();
        assert_eq!(ok.item, "txt-1");
        assert_eq!(ok.ttft_ms, Some(900));
    }

    #[test]
    fn an_old_task_with_no_spans_is_estimated_from_its_transcript() {
        let mut t = task();
        t.items = vec![
            item("u1", "user", 0, json!({})),
            item("r1", "text", 4, json!({})),
            item("t1", "tool", 5, json!({"name": "grep", "status": "ok"})),
            item("q1", "question", 9, json!({})),
            // The user went to lunch: this gap is not work and is left out.
            item("u2", "user", 9 + 3600, json!({})),
            item("r2", "text", 9 + 3605, json!({})),
        ];
        let tl = assemble(&t, &[], at(10_000));
        assert!(!tl.recorded);
        assert!(tl.partial);
        assert!(tl.blocks.iter().all(|b| b.estimated));
        let kinds: Vec<(&str, &str)> = tl
            .blocks
            .iter()
            .map(|b| (b.kind.as_str(), b.name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("model", "anthropic/claude-sonnet"),
                ("model", "anthropic/claude-sonnet"),
                ("tool", "grep"),
                ("model", "anthropic/claude-sonnet"),
            ]
        );
        assert_eq!(tl.blocks[0].item, "r1", "model time jumps to its output");
        assert_eq!(tl.totals.slowest_tools[0].name, "grep");
        assert_eq!(
            tl.totals.requests, 0,
            "estimates are never counted as requests"
        );
    }

    #[test]
    fn an_empty_old_task_assembles_to_nothing_without_panicking() {
        let tl = assemble(&task(), &[], at(0));
        assert!(!tl.recorded && !tl.partial);
        assert_eq!(tl.start_ms, None);
        assert_eq!(tl.lanes.len(), 1);
        assert_eq!(tl.totals.wall_ms, 0);
    }

    #[test]
    fn transcript_before_the_first_span_is_estimated_after_it_is_not() {
        let mut t = task();
        t.items = vec![
            item("u1", "user", 0, json!({})),
            item("t1", "tool", 2, json!({"name": "ls"})),
            item("r1", "text", 5, json!({})),
            item("t2", "tool", 20, json!({"name": "bash"})),
            item("r2", "text", 30, json!({})),
        ];
        t.spans = vec![span("tool", "", "bash", 20, Some(30))];
        let tl = assemble(&t, &[], at(40));
        assert!(tl.recorded && tl.partial);
        let est: Vec<&Block> = tl.blocks.iter().filter(|b| b.estimated).collect();
        assert!(est.iter().all(|b| b.end_ms <= ms(at(20))));
        assert!(est.iter().any(|b| b.name == "ls"));
    }

    #[test]
    fn draining_work_does_not_end_an_open_pause_or_park() {
        for name in ["pause", "park"] {
            let mut t = task();
            t.paused = (name == "pause").then(Default::default);
            t.waiting_kind = (name == "park").then(|| "wake".into());
            t.subs = vec![sub("child", "", 1, 0, "running")];
            push(&mut t.spans, span("wait", "", name, 6, None));
            let mut skill = span("model", "", "m", 14, Some(15));
            skill.role = "skill".into();
            let draining = vec![
                span("tool", "child", "bash", 7, Some(10)),
                span("model", "child", "m", 8, Some(12)),
                span("bg", "child", "server", 9, Some(13)),
                span("wait", "", "retry", 12, Some(14)),
                skill,
            ];
            for s in draining {
                push(&mut t.spans, s);
                assert_eq!(t.spans[0].end, None, "{name} is still active");
            }
            let tl = assemble(&t, &[], at(50));
            let wait = tl.blocks.iter().find(|b| b.name == name).unwrap();
            assert!(
                wait.open,
                "assembly must not infer a resume from draining work"
            );
            assert_eq!(wait.end_ms, ms(at(50)));
            assert_eq!(tl.totals.paused_ms, 44_000);
        }
    }

    #[test]
    fn assembly_does_not_infer_a_resume_from_later_recorded_activity() {
        let mut t = task();
        t.paused = Some(Default::default());
        // Saved records can be appended directly by deserialization; assembly
        // must not repeat the same activity-implies-resume heuristic as push.
        t.spans = vec![
            span("wait", "", "pause", 6, None),
            span("tool", "child", "bash", 7, Some(10)),
            span("bg", "", "server", 9, Some(13)),
        ];
        let tl = assemble(&t, &[], at(50));
        let wait = tl.blocks.iter().find(|b| b.name == "pause").unwrap();
        assert!(wait.open);
        assert_eq!(wait.end_ms, ms(at(50)));
    }

    #[test]
    fn an_open_pause_ends_where_the_run_resumed_or_is_still_open() {
        let mut t = task();
        t.spans = vec![
            span("model", "", "m", 0, Some(5)),
            span("wait", "", "pause", 6, None),
        ];
        t.paused = Some(Default::default());
        let tl = assemble(&t, &[], at(50));
        let p = tl.blocks.iter().find(|b| b.name == "pause").unwrap();
        assert!(p.open);
        assert_eq!(p.end_ms, ms(at(50)));
        assert_eq!(tl.totals.paused_ms, 44_000);

        // A resume closes the pause explicitly, regardless of any keepalive
        // or draining request that occurred while it was held.
        close_waits(&mut t.spans, "pause", at(30));
        t.paused = None;
        let mut ka = span("model", "", "m", 10, Some(11));
        ka.role = "keepalive".into();
        t.spans.push(ka);
        t.spans.push(span("model", "", "m", 30, Some(35)));
        let tl = assemble(&t, &[], at(50));
        let p = tl.blocks.iter().find(|b| b.name == "pause").unwrap();
        assert!(!p.open);
        assert_eq!(p.end_ms, ms(at(30)));
    }

    #[test]
    fn push_preserves_open_waits_and_caps_the_list() {
        let mut v = vec![];
        push(&mut v, span("wait", "", "park", 0, None));
        let mut ka = span("model", "", "m", 3, Some(4));
        ka.role = "keepalive".into();
        push(&mut v, ka);
        assert_eq!(v[0].end, None, "a keepalive is not the run coming back");
        push(&mut v, span("model", "", "m", 9, Some(12)));
        assert_eq!(v[0].end, None, "activity does not imply a wake");
        close_waits(&mut v, "park", at(13));
        assert_eq!(v[0].end, Some(at(13)));

        let mut v = vec![span("wait", "", "pause", 0, None)];
        close_waits(&mut v, "pause", at(4));
        assert_eq!(v[0].end, Some(at(4)));

        let mut v = vec![];
        for i in 0..(MAX_SPANS as i64 + 1) {
            push(&mut v, span("tool", "", "t", i, Some(i)));
        }
        assert!(v.len() <= MAX_SPANS);
        assert_eq!(v.last().unwrap().start, at(MAX_SPANS as i64), "newest kept");
    }

    #[test]
    fn running_background_jobs_show_as_open_blocks() {
        let t = task();
        let live = vec![BgInfo {
            id: "bg_1".into(),
            cmd: "cargo watch".into(),
            started: at(0),
            last_line: String::new(),
            running: true,
            exit: None,
        }];
        let tl = assemble(&t, &live, at(60));
        let b = tl.blocks.iter().find(|b| b.kind == "bg").unwrap();
        assert!(b.open && !b.estimated);
        assert_eq!(b.end_ms, ms(at(60)));
        assert_eq!(tl.lanes.last().unwrap().id, "bg");
        assert_eq!(
            tl.totals.idle_ms, 60_000,
            "a bg job is not the agent working"
        );
    }

    #[test]
    fn spans_of_an_unknown_subagent_fall_back_to_the_main_lane() {
        let mut t = task();
        t.spans = vec![span("tool", "gone", "ls", 0, Some(1))];
        let tl = assemble(&t, &[], at(5));
        assert_eq!(tl.blocks[0].lane, "main");
    }

    #[test]
    fn a_task_saved_before_spans_existed_still_loads() {
        let mut v = serde_json::to_value(task()).unwrap();
        v.as_object_mut().unwrap().remove("spans");
        let back: Task = serde_json::from_value(v).unwrap();
        assert!(back.spans.is_empty());
        // And an empty list is not written, so old files don't churn.
        let out = serde_json::to_value(&back).unwrap();
        assert!(out.get("spans").is_none());
    }

    #[test]
    fn spans_round_trip_with_missing_optional_fields() {
        let v = json!({"kind": "model", "start": "2026-01-01T00:00:00Z"});
        let s: Span = serde_json::from_value(v).unwrap();
        assert_eq!(s.end, None);
        assert!(s.agent.is_empty() && s.name.is_empty());
        let full = Span {
            ttft_ms: Some(3),
            cost: 0.1,
            attempt: 2,
            fallback: true,
            ..span("model", "s1", "m", 0, Some(1))
        };
        let back: Span = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn agent_of_item_finds_the_owning_transcript() {
        let mut t = task();
        t.items = vec![item("m1", "approval", 0, json!({}))];
        t.sub_items
            .insert("s9".into(), vec![item("x1", "question", 0, json!({}))]);
        assert_eq!(agent_of_item(&t, "m1"), "");
        assert_eq!(agent_of_item(&t, "x1"), "s9");
        assert_eq!(agent_of_item(&t, "nope"), "");
    }

    #[test]
    fn span_detail_is_truncated() {
        let s = Span::new("model", None, "m").detail(&"x".repeat(5000));
        assert_eq!(s.detail.chars().count(), DETAIL_CHARS);
    }
}
