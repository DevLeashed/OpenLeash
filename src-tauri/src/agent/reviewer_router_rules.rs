//! Router rules wired into live requests: the routed model is the one called,
//! the fallback chain still follows it, stuck signals from the agent loop
//! escalate and a hold reverts, one-shots stay put unless named, and old data
//! still loads.
//!
//! Every test that sets the process-global route table holds
//! `router::ROUTES_LOCK` (then the `OPENLEASH_HOME` guard, the same order as
//! `tests::routing_end_to_end`) across its awaits on purpose.
#![allow(clippy::await_holding_lock)]

use super::providers::{ChatRequest, StreamEvent};
use super::router::{self, RouteErr, Who};
use super::routerules::{self, Action, Condition, Rule, Situation};
use super::store::{self, Route, Settings};
use super::tests::{custom, err, harness, openai_call, openai_ok, task, wait_idle, Mock};
use super::{Harness, Item, Message};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn rule(id: &str, when: Vec<Condition>, then: Action) -> Rule {
    Rule {
        id: id.into(),
        name: id.into(),
        when,
        then,
        ..Default::default()
    }
}

fn use_model(m: &str) -> Action {
    Action {
        kind: "use".into(),
        model: m.into(),
        ..Default::default()
    }
}

fn agent_cond(agents: &[&str]) -> Condition {
    Condition {
        kind: "agent".into(),
        agents: agents.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

fn settings_for(m: &Mock, prov: &str, routes: Vec<Route>) -> Settings {
    let (cp, key) = custom(prov, &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s.routes = routes;
    router::set_routes(&s.routes);
    s
}

fn req() -> ChatRequest {
    ChatRequest {
        system: "SYS".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        effort: 2,
        max_tokens: 1000,
        cache_key: "ol-rr".into(),
    }
}

async fn call(
    h: &Arc<Harness>,
    task_id: &str,
    usage_id: &str,
    turn: bool,
    model: &str,
) -> Result<(super::providers::Turn, super::providers::Target), RouteErr> {
    let mut sink = |_e: StreamEvent| {};
    router::request(
        h,
        Who {
            task_id,
            sub: None,
            side: usage_id == "keepalive",
            turn,
            usage_id,
        },
        model,
        &req(),
        &mut sink,
        &CancellationToken::new(),
    )
    .await
}

/// The wire models the mock was asked for, from `before` on.
fn models(m: &Mock, before: usize) -> Vec<String> {
    m.reqs()[before..]
        .iter()
        .map(|r| r.body["model"].as_str().unwrap_or("").to_string())
        .collect()
}

async fn router_notices(h: &Arc<Harness>, id: &str) -> Vec<Item> {
    h.task(id)
        .await
        .unwrap()
        .lock()
        .await
        .items
        .iter()
        .filter(|i| i.kind == "notice" && i.data["level"] == "router")
        .cloned()
        .collect()
}

/// A rule that switches model sends the request to the routed model, and when
/// that one rejects it, the route's own fallback steps still take over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_routed_request_calls_the_routed_model_and_keeps_the_fallbacks() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("ol-rr-routed-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let route = Route {
        id: "r".into(),
        name: "r".into(),
        steps: vec!["rr/fallback".into()],
        rules: vec![rule("big", vec![], use_model("rr/big"))],
        ..Default::default()
    };
    let s = settings_for(&m, "rr", vec![route]);
    let mut t = task("t1", "rr/cheap");
    t.route = "r".into();
    let (h, _) = harness(s, vec![t]);

    m.push(openai_ok("from big"));
    let before = m.reqs().len();
    let (_, target) = call(&h, "t1", "main", true, "rr/cheap").await.unwrap();
    assert_eq!(target.model_id, "rr/big");
    assert_eq!(models(&m, before), ["big"], "the routed model is called");

    let n = router_notices(&h, "t1").await;
    assert_eq!(n.len(), 1, "one notice for the switch: {n:?}");
    assert_eq!(n[0].data["to"], "rr/big");
    assert_eq!(n[0].data["from"], "rr/cheap");
    assert_eq!(n[0].data["action"], "use");
    assert!(
        n[0].text.starts_with("Router: switched to"),
        "{}",
        n[0].text
    );

    // Same decision again: no second notice. The routed model rejects the
    // request; the chain carries on to the route's fallback.
    m.push(err(404, json!({"error": {"message": "model not found"}}))); // routed big
    m.push(err(404, json!({"error": {"message": "model not found"}}))); // base cheap
    m.push(openai_ok("from fallback"));
    let before = m.reqs().len();
    let (_, target) = call(&h, "t1", "main", true, "rr/cheap").await.unwrap();
    assert_eq!(target.model_id, "rr/fallback");
    assert_eq!(models(&m, before), ["big", "cheap", "fallback"]);
    assert_eq!(router_notices(&h, "t1").await.len(), 1);
    let items = h.task("t1").await.unwrap().lock().await.items.clone();
    assert!(
        !items
            .iter()
            .any(|i| i.text.contains("This agent is now on")),
        "routing must never read as a user swap"
    );
    let log = h.task("t1").await.unwrap().lock().await.route_log.clone();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].agent, "main");
    assert_eq!(log[0].rule_id, "big");

    // The rule goes away: back to the base model, with a notice saying so.
    router::set_routes(&[Route {
        id: "r".into(),
        name: "r".into(),
        steps: vec!["rr/fallback".into()],
        rules: vec![rule(
            "plan-only",
            vec![Condition {
                kind: "plan".into(),
                ..Default::default()
            }],
            use_model("rr/big"),
        )],
        ..Default::default()
    }]);
    m.push(openai_ok("from cheap"));
    let before = m.reqs().len();
    call(&h, "t1", "main", true, "rr/cheap").await.unwrap();
    assert_eq!(models(&m, before), ["cheap"]);
    let n = router_notices(&h, "t1").await;
    assert_eq!(n.len(), 2);
    assert!(n[1].text.starts_with("Router: back to"), "{}", n[1].text);

    router::set_routes(&[]);
    let _ = std::fs::remove_dir_all(dir);
}

/// The "switch when stuck" story through the real agent loop: the same call
/// fails twice, the next turn is escalated up the ladder and held for two
/// turns, then the agent drops back to its base model.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stuck_signals_from_the_loop_escalate_hold_and_revert() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("ol-rr-stuck-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let route = Route {
        id: "r".into(),
        name: "r".into(),
        ladder: vec!["rs/cheap".into(), "rs/big".into()],
        rules: vec![rule(
            "stuck",
            vec![Condition {
                kind: "stuck".into(),
                count: 2,
                ..Default::default()
            }],
            Action {
                kind: "escalate".into(),
                turns: 2,
                ..Default::default()
            },
        )],
        ..Default::default()
    };
    let s = settings_for(&m, "rs", vec![route]);
    let mut t = task("ts", "rs/cheap");
    t.route = "r".into();
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    let missing = |p: &str| {
        openai_call(
            "read_file",
            json!({"path": format!("ol-router-missing-{p}.txt")}),
        )
    };
    m.push(missing("a")); // cheap: fails once
    m.push(missing("a")); // cheap: the same call fails again → stuck
    m.push(missing("b")); // big: escalated (hold: 1 more turn)
    m.push(missing("c")); // big: held
    m.push(openai_ok("done")); // cheap: hold over
    let before = m.reqs().len();
    super::runner::send(h.clone(), "ts".into(), "do it".into())
        .await
        .unwrap();
    wait_idle(&h, "ts").await;
    assert_eq!(
        models(&m, before),
        ["cheap", "cheap", "big", "big", "cheap"],
        "escalate on the stuck turn, hold, then revert"
    );

    let n = router_notices(&h, "ts").await;
    let actions: Vec<&str> = n.iter().filter_map(|i| i.data["action"].as_str()).collect();
    assert_eq!(actions, ["escalate", "revert"], "{n:?}");
    assert!(
        n[0].text.starts_with("Router: escalated to"),
        "{}",
        n[0].text
    );
    assert_eq!(n[0].data["to"], "rs/big");
    assert_eq!(n[0].data["rule"], "stuck");
    assert!(n[1].text.starts_with("Router: back to"), "{}", n[1].text);
    let log = h.task("ts").await.unwrap().lock().await.route_log.clone();
    assert_eq!(log.len(), 2);
    assert_eq!(
        (log[0].from.as_str(), log[0].to.as_str()),
        ("rs/cheap", "rs/big")
    );
    assert_eq!(
        (log[1].from.as_str(), log[1].to.as_str()),
        ("rs/big", "rs/cheap")
    );
    assert_eq!(log[1].action, "revert");
    let (chain, _) = router::routed_steps("rs/big", "rs/cheap", "r");
    assert_eq!(chain, ["rs/big", "rs/cheap"]);
    assert_eq!(log[0].agent_kind, "main");
    // The hold is spent and the stuck counter was consumed when it fired.
    let rt = h.runtime("ts");
    let st = rt.router.lock().unwrap().get("main").cloned().unwrap();
    assert!(st.hold.is_none());
    assert!(st.last.is_none());
    assert!(st.signals.worst().0 <= 1);

    // Positive stuck counters must survive a negated stuck rule that matches
    // while there is no failure. Otherwise the next real failure never builds
    // a streak for a later positive rule.
    let (p, k) = custom("neg", &m.base, false);
    let mut neg_settings = Settings::default();
    neg_settings.custom_providers.push(p);
    neg_settings.providers.insert(k.0, k.1);
    neg_settings.routes = vec![Route {
        id: "neg-route".into(),
        rules: vec![rule(
            "not-stuck",
            vec![Condition {
                kind: "stuck".into(),
                not: true,
                ..Default::default()
            }],
            use_model("neg/big"),
        )],
        ..Default::default()
    }];
    router::set_routes(&neg_settings.routes);
    let mut nt = task("neg-task", "neg/cheap");
    nt.route = "neg-route".into();
    let (nh, _) = harness(neg_settings, vec![nt]);
    let rt = nh.runtime("neg-task");
    let mut sig = routerules::Signals::default();
    sig.record("bash", &json!({"command":"test"}), true, false);
    rt.router.lock().unwrap().insert(
        "main".into(),
        router::AgentRoute {
            signals: sig,
            ..Default::default()
        },
    );
    m.push(openai_ok("ok"));
    call(&nh, "neg-task", "main", true, "neg/cheap")
        .await
        .unwrap();
    assert_eq!(
        rt.router
            .lock()
            .unwrap()
            .get("main")
            .unwrap()
            .signals
            .worst()
            .0,
        1
    );

    router::set_routes(&[]);
    let _ = std::fs::remove_dir_all(dir);
}

/// The keepalive is never routed, even by a rule naming it; a system request
/// (compaction) ignores a rule that does not name it and follows one that does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keepalive_is_never_routed_and_system_requests_must_be_named() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("ol-rr-sys-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let generic = Route {
        id: "r".into(),
        name: "r".into(),
        rules: vec![rule("all", vec![], use_model("ry/big"))],
        ..Default::default()
    };
    let s = settings_for(&m, "ry", vec![generic]);
    let mut t = task("ty", "ry/cheap");
    t.route = "r".into();
    let (h, _) = harness(s, vec![t]);

    for _ in 0..2 {
        m.push(openai_ok("ok"));
    }
    let before = m.reqs().len();
    call(&h, "ty", "keepalive", false, "ry/cheap")
        .await
        .unwrap();
    call(&h, "ty", "compactor", false, "ry/cheap")
        .await
        .unwrap();
    assert_eq!(
        models(&m, before),
        ["cheap", "cheap"],
        "a rule without an agent condition leaves one-shots alone"
    );

    router::set_routes(&[Route {
        id: "r".into(),
        name: "r".into(),
        rules: vec![rule(
            "sys",
            vec![agent_cond(&["compactor", "keepalive"])],
            use_model("ry/summ"),
        )],
        ..Default::default()
    }]);
    for _ in 0..2 {
        m.push(openai_ok("ok"));
    }
    let before = m.reqs().len();
    call(&h, "ty", "keepalive", false, "ry/cheap")
        .await
        .unwrap();
    call(&h, "ty", "compactor", false, "ry/cheap")
        .await
        .unwrap();
    assert_eq!(
        models(&m, before),
        ["cheap", "summ"],
        "named: compaction is routed, the keepalive never is"
    );
    // A routed one-shot does not mark the agent itself as moved, so the
    // agent's compaction window is untouched by it.
    let rt = h.runtime("ty");
    assert!(rt
        .router
        .lock()
        .unwrap()
        .get("main")
        .is_none_or(|s| s.last.is_none()));

    router::set_routes(&[]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A hold or rule that moved the agent onto a long-context model is compacted
/// at that model's window, not the base model's; with no routing the plain
/// window stands.
#[test]
fn compaction_window_follows_the_routed_model() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    router::set_routes(&[Route {
        id: "r".into(),
        name: "r".into(),
        rules: vec![rule(
            "ctx",
            vec![Condition {
                kind: "context".into(),
                min_pct: 70.0,
                ..Default::default()
            }],
            use_model("openai/gpt-4.1"),
        )],
        ..Default::default()
    }]);
    let (h, _) = harness(Settings::default(), vec![task("tc", "pz/small")]);
    let plain = router::context_window("pz/small", "r");
    assert_eq!(
        router::effective_context_window(&h, "tc", "main", "pz/small", "r"),
        plain
    );
    h.runtime("tc").router.lock().unwrap().insert(
        "main".into(),
        router::AgentRoute {
            last: Some(("openai/gpt-4.1".into(), 2)),
            ..Default::default()
        },
    );
    let routed = router::effective_context_window(&h, "tc", "main", "pz/small", "r");
    assert!(routed > plain, "{routed} should exceed {plain}");
    assert_eq!(
        routed,
        super::providers::model_info("openai/gpt-4.1").context
    );
    // Rule targets are never folded into the plain window.
    assert_eq!(router::context_window("pz/small", "r"), plain);
    // Another agent of the chat is not affected.
    assert_eq!(
        router::effective_context_window(&h, "tc", "sub1", "pz/small", "r"),
        plain
    );
    // Rules removed: a stale mark no longer counts.
    router::set_routes(&[Route {
        id: "r".into(),
        name: "r".into(),
        ..Default::default()
    }]);
    assert_eq!(
        router::effective_context_window(&h, "tc", "main", "pz/small", "r"),
        plain
    );
    router::set_routes(&[]);
}

#[test]
fn old_routes_tasks_and_unknown_rules_load() {
    // A route saved before rules existed.
    let r: Route = serde_json::from_value(json!({
        "id": "r", "name": "Old", "heads": ["a/m"], "all": false,
        "steps": ["b/m"], "on_exhausted": "fail"
    }))
    .unwrap();
    assert!(r.rules.is_empty() && r.ladder.is_empty() && r.description.is_empty());
    assert_eq!(r.steps, ["b/m"]);

    // A task saved before `route_log` existed, and one that has entries.
    let mut v = serde_json::to_value(task("old", "a/m")).unwrap();
    assert!(
        v.get("route_log").is_none(),
        "an empty log is not written at all"
    );
    v.as_object_mut().unwrap().remove("route_log");
    let t: super::Task = serde_json::from_value(v.clone()).unwrap();
    assert!(t.route_log.is_empty());
    v["route_log"] = json!([{"at": 1, "agent": "main", "to": "b/m", "future": 1}]);
    let t: super::Task = serde_json::from_value(v).unwrap();
    assert_eq!(t.route_log[0].to, "b/m");

    // Settings carrying a rule this build does not understand, with no id.
    let dir = std::env::temp_dir().join(format!("ol-rr-load-{}", std::process::id()));
    let _home = store::test_home(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        store::data_dir().join("settings.json"),
        json!({"routes": [{"id": "r", "name": "r", "rules": [
            {"when": [{"kind": "quantum", "foo": 1}], "then": {"kind": "teleport"}},
            {"name": "x", "when": [], "then": {"kind": "use", "model": "c/m"}}
        ]}]})
        .to_string(),
    )
    .unwrap();
    let mut s = store::load_settings();
    assert_eq!(s.routes.len(), 1, "the route survived the unknown rule");
    assert_eq!(s.routes[0].rules[0].when[0].kind, "quantum");
    assert!(router::normalize_routes(&mut s.routes));
    let ids: Vec<&str> = s.routes[0].rules.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.iter().all(|i| !i.is_empty()));
    assert_ne!(ids[0], ids[1]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn dry_run_is_evaluate() {
    let route = Route {
        id: "r".into(),
        ladder: vec!["a/m".into(), "b/m".into()],
        rules: vec![
            // No id: the dry-run normalizes exactly as loading does.
            Rule {
                id: String::new(),
                name: "plan".into(),
                when: vec![Condition {
                    kind: "plan".into(),
                    ..Default::default()
                }],
                then: Action {
                    kind: "escalate".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let s = Situation {
        agent: "main".into(),
        turn: true,
        base_model: "a/m".into(),
        base_effort: 2,
        plan: true,
        ..Default::default()
    };
    let d = router::dry_run(&route, &s);
    let mut rules = route.rules.clone();
    routerules::normalize(&mut rules);
    assert_eq!(d, routerules::evaluate(&rules, &route.ladder, &s));
    assert_eq!(d.model, "b/m");
    assert_eq!(d.rule_id, "rule-1");
    // And over IPC the shapes round-trip.
    let back: routerules::Decision =
        serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
    assert_eq!(back, d);
}

#[test]
fn situation_reads_the_users_words_and_the_accounts() {
    let msgs = vec![
        Message::user_text("please REFACTOR the parser"),
        Message {
            role: "assistant".into(),
            content: vec![json!({"type": "text", "text": "ok"})],
            model: String::new(),
        },
        // A tool round: results plus a harness note. Not the user's words.
        Message::user(vec![
            json!({"type": "tool_result", "tool_use_id": "x", "content": "boom"}),
            json!({"type": "text", "text": "You keep repeating the same call"}),
        ]),
        Message::user(vec![
            json!({"type": "text", "text": "<system-reminder>plan mode</system-reminder>"}),
        ]),
    ];
    assert_eq!(
        router::latest_user_text(&msgs),
        "please REFACTOR the parser"
    );
    let steer = vec![Message::user(vec![
        json!({"type": "text", "text": "<system-reminder>note</system-reminder> and explain why"}),
    ])];
    assert_eq!(router::latest_user_text(&steer), "and explain why");
    assert!(!router::has_images(&msgs));
    let img = vec![Message::user(vec![
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}),
    ])];
    assert!(router::has_images(&img));

    let mut sig = routerules::Signals::default();
    sig.record("bash", &json!({"command": "x"}), true, false);
    sig.record("bash", &json!({"command": "x"}), true, false);
    sig.round(false);
    let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 10, 14, 9, 30, 0)
        .single()
        .unwrap(); // a Wednesday
    let mut head = std::collections::BTreeMap::new();
    head.insert("claude".to_string(), 42.0);
    let s = router::situation(
        router::SituationInput {
            agent: "main".into(),
            sub: false,
            turn: true,
            base_model: "openai/gpt-4.1",
            base_effort: 3,
            route_id: "",
            plan: true,
            context_tokens: 1234,
            signals: &sig,
            hold: None,
            headroom: head.clone(),
            budget_used_pct: Some(50.0),
            now,
        },
        &msgs,
    );
    assert_eq!(s.repeat_failures, 2);
    assert_eq!(s.failing_tool, "bash");
    assert_eq!(s.errors_in_row, 2);
    assert_eq!(s.no_progress_turns, 1);
    assert_eq!(s.turns, 1);
    assert_eq!(s.base_context, 1_000_000);
    assert_eq!((s.minute_of_day, s.weekday), (9 * 60 + 30, 2));
    assert_eq!(s.user_text, "please REFACTOR the parser");
    assert!(s.plan && !s.images);
    assert_eq!(s.headroom, head);

    // Headroom: best usable account per kind, from its fullest window.
    let view = |id: &str, kind: &str, used: &[f64], available: bool| super::accounts::AccountView {
        id: id.into(),
        kind: kind.into(),
        label: id.into(),
        email: String::new(),
        key_hint: String::new(),
        priority: 0,
        enabled: true,
        source: String::new(),
        disabled_reason: String::new(),
        usage: Some(super::accounts::AcctUsage {
            windows: used
                .iter()
                .map(|u| super::accounts::Window {
                    label: "w".into(),
                    used: *u,
                    resets_at: 0,
                })
                .collect(),
            ..Default::default()
        }),
        cooldown_until: 0,
        cooldown_reason: String::new(),
        available,
        active: false,
        expires_at: 0,
        caps: vec![],
    };
    let h = router::headroom(&[
        view("a", "claude", &[30.0, 90.0], true), // 10% left
        view("b", "claude", &[40.0], true),       // 60% left: the best
        view("c", "claude", &[0.0], false),       // out of the pool
        view("d", "codex", &[], true),            // nothing reported
    ]);
    assert_eq!(h.get("claude"), Some(&60.0));
    assert!(!h.contains_key("codex"));
}

/// The first large-context turn must preview the long model before compaction,
/// without creating runtime state or announcing a switch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_large_context_request_routes_before_compacting() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _m = Mock::start().await;
    let mut settings = Settings::default();
    settings.routes = vec![Route {
        id: "first-context".into(),
        rules: vec![rule(
            "big context",
            vec![Condition {
                kind: "context".into(),
                min_pct: 60.0,
                ..Default::default()
            }],
            Action {
                turns: 3,
                ..use_model("openai/gpt-4.1")
            },
        )],
        ..Default::default()
    }];
    router::set_routes(&settings.routes);
    let mut t = task("first-context", "openai/small");
    t.route = "first-context".into();
    t.system = "SYS".into();
    t.messages = vec![Message::user_text("x".repeat(440_000))];
    let (h, _) = harness(settings, vec![t]);
    let req = ChatRequest {
        system: "SYS".into(),
        messages: vec![Message::user_text("x".repeat(440_000))],
        tools: vec![],
        effort: 2,
        max_tokens: 1000,
        cache_key: "ol-first-context".into(),
    };
    let who = Who {
        task_id: "first-context",
        sub: None,
        side: false,
        turn: true,
        usage_id: "main",
    };
    let window =
        router::prospective_context_window(&h, &who, "pz/small", "first-context", &req).await;
    assert_eq!(
        window,
        router::context_window("openai/gpt-4.1", "first-context")
    );
    assert!(window > router::context_window("pz/small", "first-context"));
    // Previewing the decision does not announce it, install a hold, or spend it.
    let rt = h.runtime("first-context");
    assert!(rt.router.lock().unwrap().is_empty());
    assert!(h
        .task("first-context")
        .await
        .unwrap()
        .lock()
        .await
        .route_log
        .is_empty());
    router::set_routes(&[]);
}

/// "Load from this chat": the editor gets the main agent's live situation
/// (its model, plan mode, last user words, stuck counters, hold), a
/// subagent's on request, and an error for a chat or agent that is not there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_editor_can_load_a_chats_situation() {
    let mut t = task("tl", "openai/gpt-4.1");
    t.plan = true;
    t.effort = 1;
    t.usage.last_context = 4321;
    t.messages = vec![Message::user_text("why is the build red?")];
    t.subs.push(super::SubInfo {
        id: "s1".into(),
        role: "explore".into(),
        model: "pz/sub".into(),
        ..Default::default()
    });
    t.sub_msgs
        .insert("s1".into(), vec![Message::user_text("map the parser")]);
    let (h, _) = harness(Settings::default(), vec![t]);
    router::record_round(
        &h.runtime("tl"),
        "main",
        [("bash", &json!({"command": "npm test"}), true)],
    );
    let s = router::situation_for_task(&h, "tl", None).await.unwrap();
    assert_eq!(s.agent, "main");
    assert!(s.turn && !s.sub && s.plan);
    assert_eq!(
        (s.base_model.as_str(), s.base_effort),
        ("openai/gpt-4.1", 1)
    );
    assert_eq!(s.context_tokens, 4321);
    assert_eq!(s.user_text, "why is the build red?");
    assert_eq!((s.repeat_failures, s.failing_tool.as_str()), (1, "bash"));
    assert_eq!(s.turns, 1);

    let s = router::situation_for_task(&h, "tl", Some("s1"))
        .await
        .unwrap();
    assert_eq!(s.agent, "explore");
    assert!(s.sub);
    assert_eq!(s.base_model, "pz/sub");
    assert_eq!(
        s.base_effort, 3,
        "an unconfigured explorer is at most Medium"
    );
    assert_eq!(s.user_text, "map the parser");
    assert_eq!(s.repeat_failures, 0, "signals are per agent");

    assert!(router::situation_for_task(&h, "tl", Some("nope"))
        .await
        .is_err());
    assert!(router::situation_for_task(&h, "missing", None)
        .await
        .is_err());
}
