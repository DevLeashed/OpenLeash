//! Router rules: the part of a route that decides *which model* serves a
//! request, before the fallback chain decides what to do when it fails.
//!
//! A route (see `store::Route`) has always been a fallback chain: the chosen
//! model, then `steps` in order, then pause or fail. Rules sit in front of that
//! chain. Each request is described as a [`Situation`] (who is asking, how big
//! the context is, whether the agent keeps failing the same call, …), the
//! route's rules are evaluated top to bottom, and the first rule whose
//! conditions hold picks the model (and optionally the effort) for that request.
//! The chain then runs from the picked model exactly as before, so usage caps,
//! account pools, key pools and the pause-when-everything-is-out behaviour all
//! apply unchanged to a routed model.
//!
//! Everything here is pure: no harness, no clock, no I/O. The caller builds the
//! situation (`router::situation`), this module answers, and the caller applies
//! the answer and stores the new [`Hold`]. That is what makes the settings
//! page's dry-run panel trustworthy — it calls [`evaluate`] itself, so "what
//! would the router pick" and "what the router picks" are one function.
//!
//! Conditions and actions are deliberately plain structs with a `kind` string
//! rather than tagged enums. A settings file can carry a rule written by a newer
//! build (or a hand-edited import) whose kind this build does not know; with an
//! enum that would fail to deserialize and take the *whole* settings file down
//! with it. Here it loads, the unknown condition simply never matches, and the
//! dry-run trace says why.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Condition kinds this build understands, in the order the editor offers them.
pub const CONDITION_KINDS: &[&str] = &[
    "stuck",
    "errors_in_row",
    "no_progress",
    "agent",
    "context",
    "images",
    "plan",
    "keyword",
    "headroom",
    "budget",
    "time",
    "model",
    "turns",
    "always",
];

/// Request kinds that are not an agent's own turn. A rule only routes these
/// when it names them in an `agent` condition: a "big context → long-context
/// model" rule written for the agent must not silently move compaction, and
/// the keepalive is never routed at all (see `router::request`).
pub const SYSTEM_AGENTS: &[&str] = &[
    "compactor",
    "title",
    "summary",
    "side",
    "commit-message",
    "permission-review",
    "sub-ask",
];

/// One ordered rule of a route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// "all" (every condition must hold) | "any" (one is enough).
    #[serde(rename = "match")]
    pub mode: String,
    pub when: Vec<Condition>,
    pub then: Action,
    /// Free text for the user; never read by the router.
    pub note: String,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            // A rule that loads from a file without the field was written to
            // be used: defaulting to off would silently disable imports.
            enabled: true,
            mode: "all".into(),
            when: vec![],
            then: Action::default(),
            note: String::new(),
        }
    }
}

/// One condition. Which fields are read depends on `kind`; the rest keep their
/// defaults and are ignored. See `CONDITION_KINDS`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Condition {
    pub kind: String,
    /// Invert the condition.
    pub not: bool,
    /// stuck: same failing call repeated this many times (default 2);
    /// errors_in_row: this many failing tool calls in a row (default 3);
    /// no_progress: this many tool rounds without a successful edit (default 5);
    /// turns: at least this many agent turns in the run (default 1).
    pub count: u32,
    /// stuck / errors_in_row: only these tools count (empty = any).
    pub tools: Vec<String>,
    /// agent: "main", "subagent" (any subagent), a subagent type ("explore",
    /// "general", …) or a system kind from `SYSTEM_AGENTS`.
    pub agents: Vec<String>,
    /// context: estimated prompt tokens, inclusive bounds; 0 = unbounded.
    pub min_tokens: u64,
    pub max_tokens: u64,
    /// context: at least this percent of the base model's window (0 = unused).
    pub min_pct: f64,
    /// keyword: text (or regex) searched in the latest user message.
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// headroom: subscription pool kind ("claude", "codex", …); empty = the
    /// base model's provider.
    pub provider: String,
    /// headroom: matches when the best account has less than this percent left.
    pub below_pct: f64,
    /// budget: matches when at least this percent of the monthly budget is spent.
    pub above_pct: f64,
    /// time: local "HH:MM" window; `from > to` wraps past midnight.
    pub from: String,
    pub to: String,
    /// time: 0 = Monday … 6 = Sunday; empty = every day.
    pub days: Vec<u8>,
    /// model: the base model is one of these (`prefix*` matches a prefix).
    pub models: Vec<String>,
}

/// What a matching rule does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Action {
    /// "use" | "escalate" | "deescalate" | "effort" | "stay".
    pub kind: String,
    /// use: the model to switch to.
    pub model: String,
    /// escalate / deescalate: ladder steps to move (default 1).
    pub steps: u32,
    /// Reasoning effort for the request (0 = Max … 4 = Low); None = unchanged.
    pub effort: Option<usize>,
    /// 0 = this request only. K > 0 = hold the switch for K of this agent's
    /// turns (counting the one that triggered it), then drop back.
    pub turns: u32,
}

impl Default for Action {
    fn default() -> Self {
        Self {
            kind: "use".into(),
            model: String::new(),
            steps: 1,
            effort: None,
            turns: 0,
        }
    }
}

/// A switch that outlives the request that made it: "escalate for 3 turns".
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Hold {
    pub model: String,
    pub effort: Option<usize>,
    /// Agent turns still to be served by this hold *after* the current one.
    pub turns_left: u32,
    pub rule_id: String,
    pub rule_name: String,
    pub reason: String,
    /// The base model when the hold started, for the "back to …" notice.
    pub from_model: String,
    /// The action that started the hold ("use", "escalate", …).
    pub action: String,
}

/// Everything a rule can look at. Built by `router::situation` for a live
/// request and by the settings page for a dry-run.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Situation {
    /// "main", a subagent type, or a system kind from `SYSTEM_AGENTS`.
    pub agent: String,
    /// The request comes from a subagent.
    pub sub: bool,
    /// An agent's own turn (main or subagent), as opposed to a one-shot.
    pub turn: bool,
    /// The model the chat (or subagent) is set to, before routing.
    pub base_model: String,
    pub base_effort: usize,
    /// The base model's context window in tokens (0 = unknown).
    pub base_context: u64,
    pub plan: bool,
    pub images: bool,
    pub context_tokens: u64,
    /// The latest user message's text.
    pub user_text: String,
    /// Times the most-repeated failing call has failed (since it last succeeded
    /// or the router last acted on it).
    pub repeat_failures: u32,
    /// The tool of that call.
    pub failing_tool: String,
    /// Failing tool calls in a row, and the tool of the latest one.
    pub errors_in_row: u32,
    pub last_error_tool: String,
    /// Tool rounds since the last successful file edit.
    pub no_progress_turns: u32,
    /// Agent turns so far in this run.
    pub turns: u32,
    /// Pool kind → percent left on its best usable account.
    pub headroom: BTreeMap<String, f64>,
    /// Percent of the monthly budget spent; None = no budget set.
    pub budget_used_pct: Option<f64>,
    /// Local time: minutes since midnight and weekday (0 = Monday).
    pub minute_of_day: u32,
    pub weekday: u8,
    /// The agent's active hold, if any.
    pub hold: Option<Hold>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CondTrace {
    pub kind: String,
    pub matched: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RuleTrace {
    pub rule_id: String,
    pub rule_name: String,
    pub enabled: bool,
    pub matched: bool,
    /// Why the rule was not considered at all (disabled, system request, …).
    pub skipped: String,
    pub conditions: Vec<CondTrace>,
}

/// The router's answer for one request.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Decision {
    pub model: String,
    pub effort: usize,
    /// "base" | "use" | "escalate" | "deescalate" | "effort" | "stay" | "hold" | "revert".
    pub action: String,
    pub rule_id: String,
    pub rule_name: String,
    /// Human-readable, for the transcript notice and the audit log.
    pub reason: String,
    /// The agent's hold after this request (None = no hold).
    pub hold: Option<Hold>,
    /// A stuck-style condition triggered this: the caller resets those
    /// counters, so the same failures cannot fire the rule again every turn.
    pub consume_stuck: bool,
    pub trace: Vec<RuleTrace>,
}

impl Decision {
    /// Model or effort differs from what the agent would have used unrouted.
    pub fn routed(&self, s: &Situation) -> bool {
        self.model != s.base_model || self.effort != s.base_effort
    }
}

fn default_count(c: &Condition) -> u32 {
    if c.count > 0 {
        return c.count;
    }
    match c.kind.as_str() {
        "stuck" => 2,
        "errors_in_row" => 3,
        "no_progress" => 5,
        _ => 1,
    }
}

fn tool_ok(filter: &[String], tool: &str) -> bool {
    filter.is_empty() || filter.iter().any(|t| t == tool)
}

fn model_matches(pat: &str, model: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(p) => model.starts_with(p),
        None => pat == model,
    }
}

/// "HH:MM" → minutes since midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

fn is_stuck_kind(k: &str) -> bool {
    matches!(k, "stuck" | "errors_in_row" | "no_progress")
}

/// Whether one condition holds, before `not` is applied, with a detail line.
fn check(c: &Condition, s: &Situation) -> (bool, String) {
    match c.kind.as_str() {
        "always" => (true, "always".into()),
        "stuck" => {
            let n = default_count(c);
            let ok = s.repeat_failures >= n && tool_ok(&c.tools, &s.failing_tool);
            let tool = if s.failing_tool.is_empty() {
                "a call".to_string()
            } else {
                format!("`{}`", s.failing_tool)
            };
            (
                ok,
                format!(
                    "{tool} failed the same way {}× (needs {n}{})",
                    s.repeat_failures,
                    if c.tools.is_empty() {
                        String::new()
                    } else {
                        format!(", tools: {}", c.tools.join(", "))
                    }
                ),
            )
        }
        "errors_in_row" => {
            let n = default_count(c);
            let ok = s.errors_in_row >= n && tool_ok(&c.tools, &s.last_error_tool);
            (
                ok,
                format!("{} tool errors in a row (needs {n})", s.errors_in_row),
            )
        }
        "no_progress" => {
            let n = default_count(c);
            (
                s.no_progress_turns >= n,
                format!(
                    "{} tool rounds without a successful edit (needs {n})",
                    s.no_progress_turns
                ),
            )
        }
        "turns" => {
            let n = default_count(c);
            (s.turns >= n, format!("turn {} (needs {n})", s.turns))
        }
        "agent" => {
            let ok = c.agents.iter().any(|a| {
                a == &s.agent || (a == "subagent" && s.sub) || (a == "main" && s.agent == "main")
            });
            (
                ok,
                format!("agent is {} (wants {})", s.agent, c.agents.join(", ")),
            )
        }
        "context" => {
            let mut ok = true;
            if c.min_tokens > 0 && s.context_tokens < c.min_tokens {
                ok = false;
            }
            if c.max_tokens > 0 && s.context_tokens > c.max_tokens {
                ok = false;
            }
            let pct = if s.base_context > 0 {
                s.context_tokens as f64 * 100.0 / s.base_context as f64
            } else {
                0.0
            };
            if c.min_pct > 0.0 && (s.base_context == 0 || pct < c.min_pct) {
                ok = false;
            }
            (
                ok,
                format!(
                    "context ≈{}k tokens ({pct:.0}% of the base window)",
                    s.context_tokens / 1000
                ),
            )
        }
        "images" => (
            s.images,
            if s.images {
                "the request has images".into()
            } else {
                "no images".into()
            },
        ),
        "plan" => (
            s.plan,
            if s.plan {
                "plan mode is on".into()
            } else {
                "plan mode is off".into()
            },
        ),
        "keyword" => {
            if c.pattern.is_empty() {
                return (false, "no pattern set".into());
            }
            if c.regex {
                let re = regex::RegexBuilder::new(&c.pattern)
                    .case_insensitive(!c.case_sensitive)
                    // A user regex runs on every request: keep a hostile or
                    // careless pattern from compiling into something huge.
                    .size_limit(1 << 20)
                    .build();
                match re {
                    Ok(re) => {
                        let ok = re.is_match(&s.user_text);
                        (
                            ok,
                            format!(
                                "/{}/ {}",
                                c.pattern,
                                if ok { "matched" } else { "no match" }
                            ),
                        )
                    }
                    Err(_) => (false, format!("invalid regex /{}/", c.pattern)),
                }
            } else {
                let ok = if c.case_sensitive {
                    s.user_text.contains(&c.pattern)
                } else {
                    s.user_text
                        .to_lowercase()
                        .contains(&c.pattern.to_lowercase())
                };
                (
                    ok,
                    format!(
                        "\"{}\" {}",
                        c.pattern,
                        if ok { "found" } else { "not found" }
                    ),
                )
            }
        }
        "headroom" => {
            let prov = if c.provider.is_empty() {
                s.base_model.split('/').next().unwrap_or("").to_string()
            } else {
                c.provider.clone()
            };
            match s.headroom.get(&prov) {
                Some(left) => (
                    *left < c.below_pct,
                    format!("{prov}: {left:.0}% left (below {:.0}%?)", c.below_pct),
                ),
                None => (false, format!("no usage reported for {prov}")),
            }
        }
        "budget" => match s.budget_used_pct {
            Some(p) => (
                p >= c.above_pct,
                format!(
                    "{p:.0}% of the monthly budget spent (at least {:.0}%?)",
                    c.above_pct
                ),
            ),
            None => (false, "no monthly budget set".into()),
        },
        "time" => {
            let (Some(from), Some(to)) = (parse_hhmm(&c.from), parse_hhmm(&c.to)) else {
                return (false, "invalid time window".into());
            };
            let m = s.minute_of_day;
            let in_window = if from <= to {
                m >= from && m < to
            } else {
                m >= from || m < to
            };
            let day_ok = c.days.is_empty() || c.days.contains(&s.weekday);
            (
                in_window && day_ok,
                format!(
                    "local time {:02}:{:02} (window {}–{})",
                    m / 60,
                    m % 60,
                    c.from,
                    c.to
                ),
            )
        }
        "model" => {
            let ok = c.models.iter().any(|p| model_matches(p, &s.base_model));
            (ok, format!("base model is {}", s.base_model))
        }
        other => (false, format!("unknown condition \"{other}\"")),
    }
}

fn eval_rule(r: &Rule, s: &Situation) -> (bool, Vec<CondTrace>) {
    let conds: Vec<CondTrace> = r
        .when
        .iter()
        .map(|c| {
            let (ok, detail) = check(c, s);
            // An unknown kind never matches, negated or not: "not <something
            // this build cannot read>" must not turn into "always".
            let known = CONDITION_KINDS.contains(&c.kind.as_str());
            let matched = known && (ok != c.not);
            CondTrace {
                kind: c.kind.clone(),
                matched,
                detail: if c.not && known {
                    format!("not: {detail}")
                } else {
                    detail
                },
            }
        })
        .collect();
    let matched = if conds.is_empty() {
        // A rule with no conditions is an unconditional default.
        true
    } else if r.mode == "any" {
        conds.iter().any(|c| c.matched)
    } else {
        conds.iter().all(|c| c.matched)
    };
    (matched, conds)
}

/// One ladder move from `from`. A model not on the ladder escalates onto its
/// first rung and de-escalates onto the cheapest one.
pub fn ladder_move(ladder: &[String], from: &str, steps: i64) -> Option<String> {
    if ladder.is_empty() {
        return None;
    }
    let last = ladder.len() as i64 - 1;
    let i = match ladder.iter().position(|m| m == from) {
        Some(i) => (i as i64 + steps).clamp(0, last),
        None if steps > 0 => (steps - 1).clamp(0, last),
        None => 0,
    };
    ladder.get(i as usize).cloned()
}

fn reason_of(r: &Rule, conds: &[CondTrace]) -> String {
    let why: Vec<&str> = conds
        .iter()
        .filter(|c| c.matched)
        .map(|c| c.detail.as_str())
        .collect();
    let name = if r.name.is_empty() {
        "rule"
    } else {
        r.name.as_str()
    };
    if why.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {}", why.join("; "))
    }
}

/// Decide the model and effort for one request.
///
/// Order of precedence:
/// 1. The first enabled rule (top to bottom) whose conditions hold acts. A rule
///    with no `agent` condition only considers agent turns; system requests
///    (compaction, titles, …) need to be named explicitly.
/// 2. Otherwise an active hold for this agent serves the turn and counts down;
///    a hold with no turns left drops back to the base model ("revert").
/// 3. Otherwise the base model and effort.
///
/// A matching non-holding rule (turns = 0) wins for this request only and
/// leaves an active hold untouched, neither counted nor cleared.
pub fn evaluate(rules: &[Rule], ladder: &[String], s: &Situation) -> Decision {
    let mut trace = Vec::with_capacity(rules.len());
    let mut hit: Option<(&Rule, Vec<CondTrace>)> = None;
    for r in rules {
        let mut t = RuleTrace {
            rule_id: r.id.clone(),
            rule_name: r.name.clone(),
            enabled: r.enabled,
            ..Default::default()
        };
        if hit.is_some() {
            t.skipped = "an earlier rule already matched".into();
            trace.push(t);
            continue;
        }
        if !r.enabled {
            t.skipped = "disabled".into();
            trace.push(t);
            continue;
        }
        let names_agent = r
            .when
            .iter()
            .any(|c| c.kind == "agent" && !c.not && c.agents.iter().any(|a| a == &s.agent));
        if !s.turn && !names_agent {
            t.skipped = format!(
                "{} is a system request; add an Agent condition naming it to route it",
                s.agent
            );
            trace.push(t);
            continue;
        }
        let (matched, conds) = eval_rule(r, s);
        t.matched = matched;
        t.conditions = conds.clone();
        trace.push(t);
        if matched {
            hit = Some((r, conds));
        }
    }

    // Holds belong to an agent's own turns; a one-shot never sees or spends one.
    // A user-picked base model change cancels a hold from the old base. The
    // hold is a temporary override of that base, never a way to pin a model
    // against a deliberate model swap.
    let hold = if s.turn {
        s.hold
            .clone()
            .filter(|h| h.from_model.is_empty() || h.from_model == s.base_model)
    } else {
        None
    };
    let mut d = Decision {
        model: s.base_model.clone(),
        effort: s.base_effort,
        action: "base".into(),
        hold: hold.clone(),
        ..Default::default()
    };

    if let Some((r, conds)) = hit {
        let reason = reason_of(r, &conds);
        d.rule_id = r.id.clone();
        d.rule_name = r.name.clone();
        d.consume_stuck = r
            .when
            .iter()
            .zip(conds.iter())
            .any(|(source, trace)| trace.matched && !source.not && is_stuck_kind(&source.kind));
        // Escalation climbs from where the agent is now, which during a hold is
        // the held model — a second "still stuck" goes one rung further up.
        let current = hold
            .as_ref()
            .map(|h| h.model.clone())
            .unwrap_or_else(|| s.base_model.clone());
        let a = &r.then;
        let steps = a.steps.max(1) as i64;
        let target = match a.kind.as_str() {
            "use" if !a.model.is_empty() => Some(a.model.clone()),
            "use" => None,
            "escalate" => ladder_move(ladder, &current, steps),
            "deescalate" => ladder_move(ladder, &current, -steps),
            "effort" => Some(current.clone()),
            "stay" => {
                // Explicitly the base model: stops evaluation and suspends a
                // hold for this request (e.g. "never route the explorer").
                d.action = "stay".into();
                d.reason = reason;
                return finish(d, trace);
            }
            other => {
                d.reason = format!("{}: unknown action \"{other}\"", r.name);
                return finish(d, trace);
            }
        };
        let Some(model) = target else {
            d.action = a.kind.clone();
            d.reason = format!("{reason} (no model to switch to: the ladder is empty)");
            return finish(d, trace);
        };
        let effort = a
            .effort
            .or_else(|| hold.as_ref().and_then(|h| h.effort))
            .unwrap_or(s.base_effort)
            .min(4);
        d.model = model.clone();
        d.effort = effort;
        d.action = a.kind.clone();
        d.reason = if a.turns > 0 {
            format!(
                "{reason} · for {} turn{}",
                a.turns,
                if a.turns == 1 { "" } else { "s" }
            )
        } else {
            reason.clone()
        };
        if a.turns > 0 && s.turn {
            d.hold = Some(Hold {
                model,
                effort: a.effort.or_else(|| hold.as_ref().and_then(|h| h.effort)),
                turns_left: a.turns - 1,
                rule_id: r.id.clone(),
                rule_name: r.name.clone(),
                reason,
                from_model: hold
                    .as_ref()
                    .map(|h| h.from_model.clone())
                    .unwrap_or_else(|| s.base_model.clone()),
                action: a.kind.clone(),
            });
            // A hold that would be over before its next turn leaves nothing to keep.
            if a.turns == 1 {
                d.hold = None;
            }
        }
        return finish(d, trace);
    }

    if let Some(h) = hold {
        if h.turns_left == 0 {
            d.hold = None;
            d.action = "revert".into();
            d.rule_id = h.rule_id.clone();
            d.rule_name = h.rule_name.clone();
            d.reason = format!(
                "{} hold ended",
                if h.rule_name.is_empty() {
                    "router"
                } else {
                    h.rule_name.as_str()
                }
            );
            return finish(d, trace);
        }
        d.model = h.model.clone();
        d.effort = h.effort.unwrap_or(s.base_effort);
        d.action = "hold".into();
        d.rule_id = h.rule_id.clone();
        d.rule_name = h.rule_name.clone();
        d.reason = format!(
            "{} · {} more turn{} after this",
            h.reason,
            h.turns_left - 1,
            if h.turns_left - 1 == 1 { "" } else { "s" }
        );
        d.hold = Some(Hold {
            turns_left: h.turns_left - 1,
            ..h
        });
        // A hold that reaches zero here is kept, not dropped: this was its last
        // turn, and the next one reverts with a "back to …" reason.
    }
    finish(d, trace)
}

fn finish(d: Decision, trace: Vec<RuleTrace>) -> Decision {
    Decision { trace, ..d }
}

/// Repair a route's rules after a load or import: missing ids get one (the
/// editor keys rows by id), an unset match mode becomes "all". Returns whether
/// anything changed. Unknown kinds are kept as they are — see the module docs.
pub fn normalize(rules: &mut [Rule]) -> bool {
    let mut changed = false;
    let mut seen = std::collections::HashSet::new();
    for (i, r) in rules.iter_mut().enumerate() {
        if r.id.is_empty() || !seen.insert(r.id.clone()) {
            let mut n = i + 1;
            while seen.contains(&format!("rule-{n}")) {
                n += 1;
            }
            r.id = format!("rule-{n}");
            seen.insert(r.id.clone());
            changed = true;
        }
        if r.mode != "all" && r.mode != "any" {
            r.mode = "all".into();
            changed = true;
        }
        if r.then.kind.is_empty() {
            r.then.kind = "use".into();
            changed = true;
        }
    }
    changed
}

/// Stuck signals for one agent, fed by the agent loop after every tool round
/// and read into a [`Situation`] before every request.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    /// Hash of `name:input` → (tool, consecutive failures of exactly that call).
    /// Hashed rather than kept verbatim: a `write_file` that keeps failing would
    /// otherwise pin its whole content in memory until it succeeds.
    fails: std::collections::HashMap<u64, (String, u32)>,
    pub errors_in_row: u32,
    pub last_error_tool: String,
    pub no_progress_turns: u32,
    pub turns: u32,
}

impl Signals {
    /// One tool call's outcome. `progress` = a successful file edit.
    pub fn record(&mut self, name: &str, input: &serde_json::Value, is_err: bool, progress: bool) {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            name.hash(&mut h);
            input.to_string().hash(&mut h);
            h.finish()
        };
        if is_err {
            let e = self
                .fails
                .entry(key)
                .or_insert_with(|| (name.to_string(), 0));
            e.1 += 1;
            self.errors_in_row += 1;
            self.last_error_tool = name.to_string();
        } else {
            self.fails.remove(&key);
            self.errors_in_row = 0;
        }
        if progress {
            self.no_progress_turns = 0;
        }
    }

    /// End of a tool round (one model turn's calls).
    pub fn round(&mut self, made_progress: bool) {
        self.turns += 1;
        if !made_progress {
            self.no_progress_turns += 1;
        }
    }

    /// The most-repeated failing call: (count, tool).
    pub fn worst(&self) -> (u32, String) {
        self.fails
            .values()
            .max_by_key(|(_, n)| *n)
            .map(|(t, n)| (*n, t.clone()))
            .unwrap_or_default()
    }

    /// The router acted on these signals: start counting afresh, so the same
    /// failures cannot re-fire the rule on every following turn.
    pub fn consume(&mut self) {
        self.fails.clear();
        self.errors_in_row = 0;
        self.no_progress_turns = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cond(kind: &str) -> Condition {
        Condition {
            kind: kind.into(),
            ..Default::default()
        }
    }

    fn rule(id: &str, when: Vec<Condition>, then: Action) -> Rule {
        Rule {
            id: id.into(),
            name: id.into(),
            when,
            then,
            ..Default::default()
        }
    }

    fn use_model(m: &str, turns: u32) -> Action {
        Action {
            kind: "use".into(),
            model: m.into(),
            turns,
            ..Default::default()
        }
    }

    fn sit() -> Situation {
        Situation {
            agent: "main".into(),
            turn: true,
            base_model: "cheap/m".into(),
            base_effort: 3,
            base_context: 100_000,
            ..Default::default()
        }
    }

    fn ladder() -> Vec<String> {
        vec!["cheap/m".into(), "mid/m".into(), "big/m".into()]
    }

    #[test]
    fn no_rules_is_base() {
        let d = evaluate(&[], &[], &sit());
        assert_eq!(d.model, "cheap/m");
        assert_eq!(d.effort, 3);
        assert_eq!(d.action, "base");
        assert!(!d.routed(&sit()));
    }

    /// One row per condition kind: a situation it should match and one it
    /// should not, so a kind that silently stops matching shows up here.
    #[test]
    fn condition_matrix() {
        let base = sit();
        let mut cases: Vec<(Condition, Situation, Situation)> = vec![];
        let mut c = cond("stuck");
        c.count = 2;
        cases.push((
            c.clone(),
            Situation {
                repeat_failures: 2,
                failing_tool: "bash".into(),
                ..base.clone()
            },
            Situation {
                repeat_failures: 1,
                ..base.clone()
            },
        ));
        c.tools = vec!["edit_file".into()];
        cases.push((
            c,
            Situation {
                repeat_failures: 3,
                failing_tool: "edit_file".into(),
                ..base.clone()
            },
            Situation {
                repeat_failures: 3,
                failing_tool: "bash".into(),
                ..base.clone()
            },
        ));
        cases.push((
            cond("errors_in_row"),
            Situation {
                errors_in_row: 3,
                ..base.clone()
            },
            Situation {
                errors_in_row: 2,
                ..base.clone()
            },
        ));
        cases.push((
            cond("no_progress"),
            Situation {
                no_progress_turns: 5,
                ..base.clone()
            },
            Situation {
                no_progress_turns: 4,
                ..base.clone()
            },
        ));
        let mut c = cond("agent");
        c.agents = vec!["explore".into()];
        cases.push((
            c,
            Situation {
                agent: "explore".into(),
                sub: true,
                ..base.clone()
            },
            base.clone(),
        ));
        let mut c = cond("agent");
        c.agents = vec!["subagent".into()];
        cases.push((
            c,
            Situation {
                agent: "general".into(),
                sub: true,
                ..base.clone()
            },
            base.clone(),
        ));
        let mut c = cond("context");
        c.min_tokens = 50_000;
        cases.push((
            c,
            Situation {
                context_tokens: 60_000,
                ..base.clone()
            },
            Situation {
                context_tokens: 40_000,
                ..base.clone()
            },
        ));
        let mut c = cond("context");
        c.min_pct = 70.0;
        cases.push((
            c,
            Situation {
                context_tokens: 75_000,
                ..base.clone()
            },
            Situation {
                context_tokens: 65_000,
                ..base.clone()
            },
        ));
        let mut c = cond("context");
        c.max_tokens = 10_000;
        cases.push((
            c,
            Situation {
                context_tokens: 9_000,
                ..base.clone()
            },
            Situation {
                context_tokens: 11_000,
                ..base.clone()
            },
        ));
        cases.push((
            cond("images"),
            Situation {
                images: true,
                ..base.clone()
            },
            base.clone(),
        ));
        cases.push((
            cond("plan"),
            Situation {
                plan: true,
                ..base.clone()
            },
            base.clone(),
        ));
        let mut c = cond("keyword");
        c.pattern = "REFACTOR".into();
        cases.push((
            c.clone(),
            Situation {
                user_text: "please refactor this".into(),
                ..base.clone()
            },
            Situation {
                user_text: "fix the typo".into(),
                ..base.clone()
            },
        ));
        c.case_sensitive = true;
        cases.push((
            c,
            Situation {
                user_text: "REFACTOR now".into(),
                ..base.clone()
            },
            Situation {
                user_text: "refactor now".into(),
                ..base.clone()
            },
        ));
        let mut c = cond("keyword");
        c.pattern = r"\b(why|explain)\b".into();
        c.regex = true;
        cases.push((
            c,
            Situation {
                user_text: "Explain the bug".into(),
                ..base.clone()
            },
            Situation {
                user_text: "explanation".into(),
                ..base.clone()
            },
        ));
        let mut c = cond("headroom");
        c.provider = "claude".into();
        c.below_pct = 20.0;
        let mut low = BTreeMap::new();
        low.insert("claude".to_string(), 10.0);
        let mut high = BTreeMap::new();
        high.insert("claude".to_string(), 50.0);
        cases.push((
            c,
            Situation {
                headroom: low,
                ..base.clone()
            },
            Situation {
                headroom: high,
                ..base.clone()
            },
        ));
        let mut c = cond("budget");
        c.above_pct = 80.0;
        cases.push((
            c,
            Situation {
                budget_used_pct: Some(85.0),
                ..base.clone()
            },
            Situation {
                budget_used_pct: None,
                ..base.clone()
            },
        ));
        let mut c = cond("time");
        c.from = "22:00".into();
        c.to = "06:00".into();
        cases.push((
            c.clone(),
            Situation {
                minute_of_day: 23 * 60,
                ..base.clone()
            },
            Situation {
                minute_of_day: 12 * 60,
                ..base.clone()
            },
        ));
        c.from = "09:00".into();
        c.to = "17:00".into();
        c.days = vec![0, 1, 2, 3, 4];
        cases.push((
            c,
            Situation {
                minute_of_day: 10 * 60,
                weekday: 2,
                ..base.clone()
            },
            Situation {
                minute_of_day: 10 * 60,
                weekday: 6,
                ..base.clone()
            },
        ));
        let mut c = cond("model");
        c.models = vec!["cheap/*".into()];
        cases.push((
            c,
            base.clone(),
            Situation {
                base_model: "big/m".into(),
                ..base.clone()
            },
        ));
        let mut c = cond("turns");
        c.count = 10;
        cases.push((
            c,
            Situation {
                turns: 10,
                ..base.clone()
            },
            Situation {
                turns: 9,
                ..base.clone()
            },
        ));
        cases.push((cond("always"), base.clone(), {
            // "always" has no negative case; pair it with its negation below.
            base.clone()
        }));

        for (i, (c, yes, no)) in cases.into_iter().enumerate() {
            let r = [rule("r", vec![c.clone()], use_model("target/m", 0))];
            assert_eq!(
                evaluate(&r, &[], &yes).model,
                "target/m",
                "case {i} ({}) should match: {:?}",
                c.kind,
                evaluate(&r, &[], &yes).trace
            );
            if c.kind != "always" {
                assert_eq!(
                    evaluate(&r, &[], &no).model,
                    no.base_model,
                    "case {i} ({}) should not match",
                    c.kind
                );
            }
            // Negation flips it.
            let mut n = c.clone();
            n.not = true;
            let r = [rule("r", vec![n], use_model("target/m", 0))];
            assert_eq!(
                evaluate(&r, &[], &yes).model,
                yes.base_model,
                "case {i} negated"
            );
        }
    }

    #[test]
    fn unknown_kinds_never_match_even_negated() {
        let mut c = cond("from-the-future");
        let r = [rule("r", vec![c.clone()], use_model("big/m", 0))];
        assert_eq!(evaluate(&r, &[], &sit()).model, "cheap/m");
        c.not = true;
        let r = [rule("r", vec![c], use_model("big/m", 0))];
        let d = evaluate(&r, &[], &sit());
        assert_eq!(d.model, "cheap/m");
        assert!(d.trace[0].conditions[0].detail.contains("unknown"));
        // Unknown action: no switch, explained.
        let r = [rule(
            "r",
            vec![],
            Action {
                kind: "teleport".into(),
                ..Default::default()
            },
        )];
        let d = evaluate(&r, &[], &sit());
        assert_eq!(d.model, "cheap/m");
        assert!(d.reason.contains("unknown action"));
    }

    #[test]
    fn invalid_regex_does_not_match() {
        let mut c = cond("keyword");
        c.pattern = "(unclosed".into();
        c.regex = true;
        let r = [rule("r", vec![c], use_model("big/m", 0))];
        let d = evaluate(
            &r,
            &[],
            &Situation {
                user_text: "(unclosed".into(),
                ..sit()
            },
        );
        assert_eq!(d.model, "cheap/m");
        assert!(d.trace[0].conditions[0].detail.contains("invalid regex"));
    }

    #[test]
    fn first_match_wins_and_order_matters() {
        let rules = vec![
            rule("off", vec![], use_model("never/m", 0)),
            rule("a", vec![cond("plan")], use_model("plan/m", 0)),
            rule("b", vec![], use_model("default/m", 0)),
        ];
        let mut rules = rules;
        rules[0].enabled = false;
        let d = evaluate(
            &rules,
            &[],
            &Situation {
                plan: true,
                ..sit()
            },
        );
        assert_eq!(d.model, "plan/m");
        assert_eq!(d.trace[0].skipped, "disabled");
        assert_eq!(d.trace[2].skipped, "an earlier rule already matched");
        assert_eq!(evaluate(&rules, &[], &sit()).model, "default/m");
        rules.swap(1, 2);
        let d = evaluate(
            &rules,
            &[],
            &Situation {
                plan: true,
                ..sit()
            },
        );
        assert_eq!(d.model, "default/m", "reordered: the default now wins");
    }

    #[test]
    fn any_mode() {
        let mut r = rule(
            "r",
            vec![cond("plan"), cond("images")],
            use_model("big/m", 0),
        );
        assert_eq!(
            evaluate(
                std::slice::from_ref(&r),
                &[],
                &Situation {
                    images: true,
                    ..sit()
                }
            )
            .model,
            "cheap/m"
        );
        r.mode = "any".into();
        assert_eq!(
            evaluate(
                &[r],
                &[],
                &Situation {
                    images: true,
                    ..sit()
                }
            )
            .model,
            "big/m"
        );
    }

    #[test]
    fn signal_conditions_consume_only_when_a_positive_signal_matched() {
        let mut stuck = cond("stuck");
        stuck.not = true;
        let negated = [rule("not-stuck", vec![stuck], use_model("big/m", 0))];
        let s = Situation {
            repeat_failures: 0,
            ..sit()
        };
        assert!(!evaluate(&negated, &[], &s).consume_stuck);

        let mut r = rule(
            "any",
            vec![cond("stuck"), cond("images")],
            use_model("big/m", 0),
        );
        r.mode = "any".into();
        let s = Situation {
            repeat_failures: 0,
            images: true,
            ..sit()
        };
        let d = evaluate(&[r], &[], &s);
        assert_eq!(d.model, "big/m");
        assert!(
            !d.consume_stuck,
            "the unrelated image match must not consume failures"
        );
    }

    #[test]
    fn explicit_base_model_swap_cancels_the_old_hold() {
        let hold = Hold {
            model: "big/m".into(),
            from_model: "cheap/m".into(),
            turns_left: 2,
            ..Default::default()
        };
        let s = Situation {
            base_model: "new-base/m".into(),
            hold: Some(hold),
            ..sit()
        };
        let d = evaluate(&[], &[], &s);
        assert_eq!(d.model, "new-base/m");
        assert_eq!(d.action, "base");
        assert!(d.hold.is_none());
    }

    #[test]
    fn system_requests_need_an_agent_condition() {
        let compactor = Situation {
            agent: "compactor".into(),
            turn: false,
            ..sit()
        };
        let generic = [rule("r", vec![], use_model("big/m", 0))];
        let d = evaluate(&generic, &[], &compactor);
        assert_eq!(d.model, "cheap/m");
        assert!(d.trace[0].skipped.contains("system request"));
        let mut c = cond("agent");
        c.agents = vec!["compactor".into()];
        let named = [rule("r", vec![c], use_model("summarizer/m", 0))];
        assert_eq!(evaluate(&named, &[], &compactor).model, "summarizer/m");
    }

    #[test]
    fn system_request_must_be_named_even_in_any_rule() {
        let mut c = cond("agent");
        c.agents = vec!["main".into()];
        let mut r = rule("r", vec![c, cond("always")], use_model("big/m", 0));
        r.mode = "any".into();
        let s = Situation {
            agent: "compactor".into(),
            turn: false,
            ..sit()
        };
        assert_eq!(evaluate(&[r], &[], &s).model, "cheap/m");
    }

    #[test]
    fn imported_effort_is_bounded() {
        let r = rule(
            "r",
            vec![],
            Action {
                effort: Some(999),
                ..use_model("big/m", 0)
            },
        );
        assert_eq!(evaluate(&[r], &[], &sit()).effort, 4);
    }

    #[test]
    fn escalate_and_deescalate_along_ladder() {
        let esc = Action {
            kind: "escalate".into(),
            ..Default::default()
        };
        let r = [rule("e", vec![], esc.clone())];
        assert_eq!(evaluate(&r, &ladder(), &sit()).model, "mid/m");
        // From the top it stays on top.
        let top = Situation {
            base_model: "big/m".into(),
            ..sit()
        };
        assert_eq!(evaluate(&r, &ladder(), &top).model, "big/m");
        // Two steps.
        let r2 = [rule(
            "e",
            vec![],
            Action {
                steps: 2,
                ..esc.clone()
            },
        )];
        assert_eq!(evaluate(&r2, &ladder(), &sit()).model, "big/m");
        // Off-ladder base: onto the first rung.
        let off = Situation {
            base_model: "other/m".into(),
            ..sit()
        };
        assert_eq!(evaluate(&r, &ladder(), &off).model, "cheap/m");
        let de = [rule(
            "d",
            vec![],
            Action {
                kind: "deescalate".into(),
                ..Default::default()
            },
        )];
        assert_eq!(evaluate(&de, &ladder(), &top).model, "mid/m");
        assert_eq!(evaluate(&de, &ladder(), &sit()).model, "cheap/m");
        // Empty ladder: explained no-op.
        let d = evaluate(&r, &[], &sit());
        assert_eq!(d.model, "cheap/m");
        assert!(d.reason.contains("ladder is empty"));
    }

    #[test]
    fn effort_only_and_stay() {
        let r = [rule(
            "e",
            vec![cond("plan")],
            Action {
                kind: "effort".into(),
                effort: Some(0),
                ..Default::default()
            },
        )];
        let s = Situation {
            plan: true,
            ..sit()
        };
        let d = evaluate(&r, &[], &s);
        assert_eq!((d.model.as_str(), d.effort), ("cheap/m", 0));
        assert!(d.routed(&s));
        // `use` with an effort sets both.
        let r = [rule(
            "u",
            vec![],
            Action {
                effort: Some(1),
                ..use_model("big/m", 0)
            },
        )];
        let d = evaluate(&r, &[], &sit());
        assert_eq!((d.model.as_str(), d.effort), ("big/m", 1));
        // Stay suspends a hold for this request without spending it.
        let hold = Hold {
            model: "big/m".into(),
            turns_left: 2,
            ..Default::default()
        };
        let mut c = cond("agent");
        c.agents = vec!["main".into()];
        let r = [rule(
            "s",
            vec![c],
            Action {
                kind: "stay".into(),
                ..Default::default()
            },
        )];
        let d = evaluate(
            &r,
            &[],
            &Situation {
                hold: Some(hold.clone()),
                ..sit()
            },
        );
        assert_eq!(d.model, "cheap/m");
        assert_eq!(d.action, "stay");
        assert_eq!(d.hold, Some(hold));
    }

    /// The "switch when stuck" story end to end: cheap by default; the same
    /// command fails twice; the router escalates for 3 turns and drops back.
    #[test]
    fn stuck_escalates_for_k_turns_then_reverts() {
        let mut c = cond("stuck");
        c.count = 2;
        let rules = [rule(
            "stuck",
            vec![c],
            Action {
                kind: "escalate".into(),
                turns: 3,
                ..Default::default()
            },
        )];
        let mut sig = Signals::default();
        let mut hold: Option<Hold> = None;
        let mut picks = vec![];
        let mut actions = vec![];
        let cmd = json!({"command": "npm test"});
        for turn in 0..7 {
            let (n, tool) = sig.worst();
            let s = Situation {
                repeat_failures: n,
                failing_tool: tool,
                hold: hold.clone(),
                ..sit()
            };
            let d = evaluate(&rules, &ladder(), &s);
            if d.consume_stuck {
                sig.consume();
            }
            hold = d.hold.clone();
            picks.push(d.model.clone());
            actions.push(d.action.clone());
            // Turns 0 and 1 run the same failing command; after that it passes.
            let failing = turn < 2;
            sig.record("bash", &cmd, failing, false);
            sig.round(false);
        }
        assert_eq!(
            picks,
            ["cheap/m", "cheap/m", "mid/m", "mid/m", "mid/m", "cheap/m", "cheap/m"]
        );
        assert_eq!(
            actions,
            ["base", "base", "escalate", "hold", "hold", "revert", "base"]
        );
    }

    #[test]
    fn still_stuck_during_hold_climbs_further() {
        let rules = [rule(
            "stuck",
            vec![cond("stuck")],
            Action {
                kind: "escalate".into(),
                turns: 3,
                ..Default::default()
            },
        )];
        let hold = Hold {
            model: "mid/m".into(),
            turns_left: 2,
            from_model: "cheap/m".into(),
            ..Default::default()
        };
        let d = evaluate(
            &rules,
            &ladder(),
            &Situation {
                repeat_failures: 2,
                hold: Some(hold),
                ..sit()
            },
        );
        assert_eq!(d.model, "big/m");
        let h = d.hold.unwrap();
        assert_eq!(h.turns_left, 2);
        assert_eq!(
            h.from_model, "cheap/m",
            "revert still goes to the original base"
        );
    }

    #[test]
    fn non_holding_rule_leaves_hold_untouched() {
        let mut c = cond("images");
        c.not = false;
        let rules = [rule("vision", vec![c], use_model("vision/m", 0))];
        let hold = Hold {
            model: "big/m".into(),
            turns_left: 2,
            ..Default::default()
        };
        let d = evaluate(
            &rules,
            &[],
            &Situation {
                images: true,
                hold: Some(hold.clone()),
                ..sit()
            },
        );
        assert_eq!(d.model, "vision/m");
        assert_eq!(d.hold, Some(hold));
    }

    #[test]
    fn one_shots_ignore_holds() {
        let hold = Hold {
            model: "big/m".into(),
            turns_left: 2,
            ..Default::default()
        };
        let d = evaluate(
            &[],
            &[],
            &Situation {
                agent: "compactor".into(),
                turn: false,
                hold: Some(hold),
                ..sit()
            },
        );
        assert_eq!(d.model, "cheap/m");
        assert_eq!(d.hold, None);
    }

    #[test]
    fn signals_track_repeats_errors_and_progress() {
        let mut s = Signals::default();
        let a = json!({"path": "a"});
        let b = json!({"path": "b"});
        s.record("edit_file", &a, true, false);
        s.record("edit_file", &b, true, false);
        s.record("edit_file", &a, true, false);
        assert_eq!(s.worst(), (2, "edit_file".to_string()));
        assert_eq!(s.errors_in_row, 3);
        s.round(false);
        assert_eq!(s.no_progress_turns, 1);
        // Success of that exact call resets its count and the error streak.
        s.record("edit_file", &a, false, true);
        assert_eq!(s.worst().0, 1);
        assert_eq!(s.errors_in_row, 0);
        assert_eq!(s.no_progress_turns, 0);
        s.round(true);
        assert_eq!(s.no_progress_turns, 0);
        assert_eq!(s.turns, 2);
        s.consume();
        assert_eq!(s.worst().0, 0);
    }

    #[test]
    fn old_and_partial_json_loads() {
        // A rule from a hand-written import: no id, no enabled, no match.
        let r: Rule = serde_json::from_value(json!({
            "name": "x", "when": [{"kind": "stuck"}], "then": {"kind": "escalate", "turns": 2}
        }))
        .unwrap();
        assert!(r.enabled);
        assert_eq!(r.mode, "all");
        assert_eq!(r.then.steps, 1);
        let mut rs = vec![r.clone(), r];
        assert!(normalize(&mut rs));
        assert_ne!(rs[0].id, rs[1].id);
        assert!(!normalize(&mut rs));
        // Unknown fields and kinds load.
        let c: Condition = serde_json::from_value(json!({"kind": "quantum", "foo": 1})).unwrap();
        assert_eq!(c.kind, "quantum");
    }

    #[test]
    fn hhmm() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("9"), None);
    }
}
