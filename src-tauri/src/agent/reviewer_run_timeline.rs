//! Release-gate tests for the run timeline's recording (`spans.rs`): that the
//! real request, tool, wait and pause paths leave the spans the timeline is
//! drawn from, and that they survive a save and reload.
//!
//! The pure assembly is tested beside it in `spans.rs`; these drive the
//! harness end to end against the local mock provider, with temporary homes
//! and dummy keys only.

#![cfg(test)]
// The route and home locks serialise process-global state for the whole test,
// so they are held across `.await` on purpose (AGENTS.md: do not drop a test
// lock to quiet the lint). Same allowance as `tests.rs`.
#![allow(clippy::await_holding_lock)]

use super::providers::StreamEvent;
use super::router::{self, Who};
use super::spans::Span;
use super::store::{self, Settings};
use super::tests::{
    answer, await_question, custom, err, harness, openai_call, openai_ok, task, wait_idle, Mock,
};
use super::{runner, Harness, Message};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

async fn spans_of(h: &Arc<Harness>, id: &str) -> Vec<Span> {
    h.task(id).await.unwrap().lock().await.spans.clone()
}

fn settings_for(name: &str, base: &str) -> Settings {
    let (cp, key) = custom(name, base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    s
}

/// A failed attempt, its backoff and the retry that lands are three blocks,
/// with the outcome, try number, tokens and the short error on each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_request_attempt_and_its_backoff_is_recorded() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let m = Mock::start().await;
    let (h, _) = harness(settings_for("rt", &m.base), vec![task("rt1", "rt/m")]);
    m.push(err(500, json!({"error": {"message": "boom"}})));
    m.push(openai_ok("hello"));
    let mut sink = |_e: StreamEvent| {};
    let req = super::providers::ChatRequest {
        system: "SYS".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        effort: 2,
        max_tokens: 1000,
        cache_key: "ol-rt".into(),
    };
    router::request(
        &h,
        Who {
            task_id: "rt1",
            sub: None,
            side: false,
            turn: true,
            usage_id: "main",
        },
        "rt/m",
        &req,
        &mut sink,
        &CancellationToken::new(),
    )
    .await
    .expect("succeeds on the retry");

    let spans = spans_of(&h, "rt1").await;
    let shape: Vec<(&str, &str, &str, u32)> = spans
        .iter()
        .map(|s| {
            (
                s.kind.as_str(),
                s.name.as_str(),
                s.status.as_str(),
                s.attempt,
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("model", "rt/m", "error", 1),
            ("wait", "retry", "ok", 0),
            ("model", "rt/m", "ok", 2),
        ]
    );
    assert!(spans[0].detail.contains("boom"), "{:?}", spans[0].detail);
    assert_eq!(spans[2].cache_read, 80, "provider-reported counts");
    assert_eq!(spans[2].role, "main");
    assert!(spans.iter().all(|s| s.end.is_some_and(|e| e >= s.start)));
    assert!(
        spans.iter().all(|s| s.agent.is_empty()),
        "main agent's lane"
    );
    // Strictly in order: the backoff sits between the two attempts.
    assert!(spans[1].start >= spans[0].end.unwrap());
    assert!(spans[2].start >= spans[1].end.unwrap());
}

/// A turn that asks the user a question: each request, the tool call, and the
/// wait on the user land on the timeline, the wait linked to its question row,
/// and all of it comes back after a save and reload.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_records_requests_tools_and_waits_and_they_persist() {
    let dir = std::env::temp_dir().join(format!("openleash-runtl-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let mut t = task("rtq", "rq/m");
    t.status = "idle".into();
    let (h, _) = harness(settings_for("rq", &m.base), vec![t]);
    // A visible tool that fails (the file does not exist), then a question.
    m.push(openai_call(
        "read_file",
        json!({"path": "openleash-run-timeline-missing.txt"}),
    ));
    m.push(openai_call(
        "ask_user",
        json!({"questions": [{"question": "Which?", "type": "single",
            "options": [{"label": "A"}, {"label": "B"}]}]}),
    ));
    m.push(openai_ok("done"));
    runner::send(h.clone(), "rtq".into(), "go".into())
        .await
        .unwrap();
    let qid = await_question(&h, "rtq").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    answer(&h, "rtq", &qid, json!({"answers": ["A"]})).await;
    wait_idle(&h, "rtq").await;

    let spans = spans_of(&h, "rtq").await;
    let models: Vec<&Span> = spans
        .iter()
        .filter(|s| s.kind == "model" && s.role == "main")
        .collect();
    assert_eq!(models.len(), 3, "{spans:#?}");
    let wait = spans
        .iter()
        .find(|s| s.kind == "wait" && s.name == "question")
        .expect("the wait on the user");
    assert_eq!(wait.item, qid);
    assert_eq!(wait.status, "answered");
    assert!(wait.end.unwrap() - wait.start >= chrono::Duration::milliseconds(25));
    let tool = spans
        .iter()
        .find(|s| s.kind == "tool" && s.name == "ask_user")
        .expect("the tool call");
    assert_eq!(tool.status, "ok");
    // `ask_user` has no tool row of its own (its question row is the wait's);
    // a visible tool links to its row, failure and all.
    assert!(tool.item.is_empty());
    let read = spans
        .iter()
        .find(|s| s.kind == "tool" && s.name == "read_file")
        .expect("the failed read");
    assert_eq!(read.status, "error");
    let row = h
        .task("rtq")
        .await
        .unwrap()
        .lock()
        .await
        .items
        .iter()
        .find(|i| i.id == read.item)
        .map(|i| i.kind.clone());
    assert_eq!(row.as_deref(), Some("tool"), "linked to its transcript row");
    // The question was asked from inside the tool call.
    assert!(tool.start <= wait.start && tool.end.unwrap() >= wait.end.unwrap());

    let tl = {
        let t = h.task("rtq").await.unwrap();
        let t = t.lock().await;
        super::spans::assemble(&t, &[], chrono::Utc::now())
    };
    assert!(tl.recorded);
    assert!(tl.totals.you_ms >= 25, "{:?}", tl.totals);
    assert_eq!(tl.totals.requests, 3);
    assert_eq!((tl.totals.tool_calls, tl.totals.tool_errors), (2, 1));

    // Persisted with the task: a reopened chat shows the same timeline.
    h.save_task_now("rtq").await;
    let back = store::read_task_full(&store::task_path("rtq")).expect("reload");
    assert_eq!(back.spans, spans);
    let _ = std::fs::remove_dir_all(dir);
}

/// A pause opens a block when the chat freezes and a waiter seeing it lift
/// closes it; a global "Pause all", which opens nothing, is recorded once by
/// the first waiter however many were held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pauses_open_and_close_on_the_timeline() {
    let (h, _) = harness(Settings::default(), vec![task("rp", "x/m")]);
    h.pause_task("rp", "manual", "Paused by you").await;
    let spans = spans_of(&h, "rp").await;
    assert_eq!(spans.len(), 1);
    assert_eq!(
        (spans[0].name.as_str(), spans[0].detail.as_str()),
        ("pause", "manual")
    );
    assert_eq!(spans[0].end, None, "still frozen");

    let waiter = {
        let h = h.clone();
        tokio::spawn(async move { h.wait_unpaused("rp", &CancellationToken::new()).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    h.update_task("rp", |t| t.paused = None).await;
    h.pause_bell.notify_waiters();
    assert!(waiter.await.unwrap());
    let spans = spans_of(&h, "rp").await;
    assert_eq!(spans.len(), 1, "closed, not duplicated: {spans:#?}");
    assert!(spans[0].end.is_some());

    // Pause all: two waiters, one block.
    h.settings.write().await.paused_all = true;
    let waiters: Vec<_> = (0..2)
        .map(|_| {
            let h = h.clone();
            tokio::spawn(async move { h.wait_unpaused("rp", &CancellationToken::new()).await })
        })
        .collect();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    h.settings.write().await.paused_all = false;
    h.pause_bell.notify_waiters();
    for w in waiters {
        assert!(w.await.unwrap());
    }
    let spans = spans_of(&h, "rp").await;
    let global: Vec<&Span> = spans.iter().filter(|s| s.detail == "global").collect();
    assert_eq!(global.len(), 1, "{spans:#?}");
    assert!(global[0].end.unwrap() > global[0].start);
}

/// Main is frozen while a child tool/model, side retry and background process
/// finish. Their finished spans are not a resume: only Resume or the park's
/// wake transition ends the wait, even with no model waiter alive.
#[tokio::test]
async fn draining_subagents_and_background_jobs_do_not_resume_the_main_timeline() {
    use super::spans::{assemble, record, Span};
    use chrono::Utc;
    use std::sync::atomic::Ordering;

    for parked in [false, true] {
        let mut t = task("drain", "x/m");
        t.subs.push(super::SubInfo {
            id: "child".into(),
            status: "running".into(),
            ..Default::default()
        });
        let (h, _) = harness(Settings::default(), vec![t]);
        // No loop/provider required: Resume sees an already-running agent.
        h.runtime("drain").running.store(true, Ordering::SeqCst);
        let name = if parked { "park" } else { "pause" };
        if parked {
            h.runtime("drain").wake_state.lock().unwrap().parked = true;
            h.update_task("drain", |t| {
                t.status = "waiting".into();
                t.waiting_kind = Some("wake".into());
                super::spans::push(&mut t.spans, Span::new("wait", None, "park"));
            })
            .await;
        } else {
            h.pause_task("drain", "manual", "Paused by you").await;
        }
        for (kind, agent, label) in [
            ("tool", Some("child"), "bash"),
            ("model", Some("child"), "x/m"),
            ("bg", Some("child"), "server"),
            ("wait", None, "retry"),
            ("model", None, "x/m"),
        ] {
            let mut s = Span::new(kind, agent, label);
            s.end = Some(Utc::now());
            s.role = "skill".into();
            record(&h, "drain", s).await;
        }
        let t = h.task("drain").await.unwrap();
        {
            let t = t.lock().await;
            assert_eq!(t.spans[0].end, None, "{name} must survive draining work");
            let tl = assemble(&t, &[], Utc::now());
            assert!(tl.blocks.iter().find(|b| b.name == name).unwrap().open);
            // A persisted/reopened task must preserve the active wait too.
            let reopened: super::Task =
                serde_json::from_value(serde_json::to_value(&*t).unwrap()).unwrap();
            assert!(
                assemble(&reopened, &[], Utc::now())
                    .blocks
                    .iter()
                    .find(|b| b.name == name)
                    .unwrap()
                    .open
            );
        }
        let before = Utc::now();
        if parked {
            runner::wake_now(&h, "drain", "trigger fired".into()).await;
        } else {
            runner::resume(&h, "drain", None).await.unwrap();
        }
        let after = Utc::now();
        let t = t.lock().await;
        let end = t.spans[0].end.expect("actual resume/wake closes the wait");
        assert!(end >= before && end <= after);
        assert!(
            !assemble(&t, &[], after)
                .blocks
                .iter()
                .find(|b| b.name == name)
                .unwrap()
                .open
        );
    }
}
