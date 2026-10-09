//! Cache keepalive and sleep-and-wake, from the runner's side.
//!
//! The two features share a shape: a background loop that spends (a ping, a
//! poll) which must be off by default and must stop the moment the user says so.
//! So the tests here are mostly about *stopping* — a keepalive that runs after a
//! stop, or a wake that fires after the user took the chat back, is a bug that
//! costs money or restarts cancelled work.
//!
//! Where the interesting logic is time, it is pulled out as a pure function
//! (`keepalive_tick`, `timer_elapsed`, `wake_reason`) and tested directly, so no
//! test sleeps for five real minutes to observe a decision about three integers.
//!
//! `agent_kind` is also covered here rather than in router.rs: what the app-wide
//! `agents` dimension is called is a naming rule, and the interesting case — a
//! system one-shot has to stop reading as "main" — is the one the per-agent split
//! depends on.

use super::router::agent_kind_for_test;
use super::runner::{
    baseline_of, keepalive_ping, keepalive_tick, park_waiting, timer_elapsed,
    wake_generation_is_current, wake_nap_ms, wake_now, wake_reason, KeepaliveOutcome,
    KeepaliveTick, WakeSource,
};
use super::store::Settings;
use super::tests::{custom, err, harness, openai_call, openai_ok, Mock};
use super::*;
use serde_json::json;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

fn parked(source: &str, extra: serde_json::Value) -> WakeSource {
    let mut v = json!({"source": source, "reason": "the thing", "delay_s": 0});
    if let serde_json::Value::Object(m) = &extra {
        for (k, x) in m {
            v[k] = x.clone();
        }
    }
    serde_json::from_value(v).unwrap()
}

// ───────────────────────── keepalive: the decision ─────────────────────────

#[test]
fn keepalive_is_off_by_default() {
    // The setting's default is false, and a ping is a real request. This is the
    // property the whole feature hangs on: an upgrading user must not start
    // paying for pings they never asked for.
    assert!(!store::Settings::default().cache_keepalive);
    assert_eq!(store::Settings::default().cache_keepalive_pings, 4);
    assert_eq!(
        keepalive_tick(false, 4, 0, false, false, false),
        KeepaliveTick::Stop,
        "with the switch off the loop stops rather than waiting"
    );
}

#[test]
fn keepalive_stops_once_the_cap_is_reached() {
    // The cap is the second half of the safety story: even switched on, a chat
    // parked overnight must stop spending rather than ping until morning.
    assert_eq!(
        keepalive_tick(true, 4, 3, false, false, false),
        KeepaliveTick::Ping
    );
    assert_eq!(
        keepalive_tick(true, 4, 4, false, false, false),
        KeepaliveTick::Stop,
        "the fourth ping was the last one this task may send"
    );
    assert_eq!(
        keepalive_tick(true, 0, 0, false, false, false),
        KeepaliveTick::Stop,
        "a cap of zero means no pings at all, not unlimited"
    );
}

#[test]
fn keepalive_is_stopped_by_a_pause() {
    // The brief's requirement: stopped on pause, stop or interrupt. The loop's
    // own check is what makes it true even if the cancel token is missed — and a
    // global pause is included, because the whole app being frozen is the user
    // saying "spend nothing".
    assert_eq!(
        keepalive_tick(true, 4, 1, true, false, false),
        KeepaliveTick::Stop
    );
    assert_eq!(
        keepalive_tick(true, 4, 1, false, true, false),
        KeepaliveTick::Stop
    );
}

#[test]
fn keepalive_waits_while_a_turn_is_in_flight() {
    // Not a stop: a run generating history is the reason the loop exists, and
    // the pause *after* it is what it is for. It must stay alive to serve it.
    assert_eq!(
        keepalive_tick(true, 4, 1, false, false, true),
        KeepaliveTick::Wait
    );
    // ...and the ping is only the idle case.
    assert_eq!(
        keepalive_tick(true, 4, 1, false, false, false),
        KeepaliveTick::Ping
    );
}

// ───────────────────────── keepalive: the ping ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keepalive_ping_is_billed_to_its_own_row_and_does_not_touch_history() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-keepalive-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("ka", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = super::tests::task("tka", "ka/m");
    t.messages = vec![Message::user_text("do the thing")];
    let (h, _events) = harness(s, vec![t]);

    m.push(openai_ok(""));
    let outcome = keepalive_ping(&h, "tka", &CancellationToken::new()).await;
    assert_eq!(outcome, KeepaliveOutcome::Sent);

    // One request went out, and it was tiny — the point is to touch the cache,
    // not to make the model work.
    let reqs = m.reqs();
    assert_eq!(reqs.len(), 1, "exactly one ping request");
    assert_eq!(reqs[0].body["max_tokens"], 1);

    // The chat's *total* cost moved (it is a real request), but the history the
    // model will read is untouched — a ping must never become a turn.
    let g = h.task("tka").await.unwrap();
    let g = g.lock().await;
    assert!(g.usage.cost > 0.0, "the ping is real spend, so it counts");
    assert_eq!(
        g.messages.len(),
        1,
        "a keepalive is not a conversation turn"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_ping_is_not_counted_against_the_cap() {
    // The cap is on *spend*. A ping that never reached a model (every route out,
    // or cancelled mid-flight) must not burn one of the four the user paid for.
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-keepalive-fail-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("kaf", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = super::tests::task("tkaf", "kaf/m");
    t.messages = vec![Message::user_text("go")];
    let (h, _events) = harness(s, vec![t]);

    // A hard 400 that will not be retried into success.
    m.push(err(400, json!({"error": {"message": "nope"}})));
    let outcome = keepalive_ping(&h, "tkaf", &CancellationToken::new()).await;
    assert!(
        matches!(outcome, KeepaliveOutcome::Skipped | KeepaliveOutcome::Stop),
        "a failed ping reports back so the loop can refund the ping"
    );
    // `Sent` is the only outcome that keeps the count; everything else refunds.
    assert_ne!(outcome, KeepaliveOutcome::Sent);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn keepalive_is_cancelled_by_a_stop() {
    // `stop_tokens` is the one path every stop already takes (interrupt, force
    // pause, purge), so wiring the keepalive's token through it is what makes
    // "stopped on stop/interrupt" true for all of them at once.
    let h = {
        let (h, _) =
            super::tests::harness(Settings::default(), vec![super::tests::task("t", "x/m")]);
        h
    };
    let rt = h.runtime("t");
    let cancel = CancellationToken::new();
    *rt.keepalive_cancel.lock().unwrap() = Some(cancel.clone());
    assert!(!cancel.is_cancelled());
    runner::stop_tokens(&h, "t");
    assert!(
        cancel.is_cancelled(),
        "a stop has to reach the keepalive loop"
    );
    assert!(
        rt.keepalive_cancel.lock().unwrap().is_none(),
        "and take the token away, so nothing can re-adopt it"
    );
}

// ───────────────────────── wake: the pure decisions ─────────────────────────

#[test]
fn a_timer_fires_only_after_its_delay() {
    // The requirement "timer logic testable without real time passing": these are
    // the three integers the watcher supplies, and no clock is involved.
    assert!(!timer_elapsed(1_000, 30, 1_000), "not yet");
    assert!(!timer_elapsed(1_000, 30, 30_999), "one ms short");
    assert!(timer_elapsed(1_000, 30, 31_000), "exactly the delay");
    assert!(timer_elapsed(1_000, 30, 99_999), "long past");
    // The former 240 × 5-second watcher budget expired at 20 minutes. A one-hour
    // timer must still be sleeping at that point and fire only at its deadline.
    assert!(
        !timer_elapsed(1_000, 3_600, 1_201_000),
        "20 minutes is not an hour"
    );
    assert!(timer_elapsed(1_000, 3_600, 3_601_000), "one hour elapsed");
    // A clock that somehow went backwards must not read as elapsed.
    assert!(!timer_elapsed(5_000, 30, 1_000));
}

#[test]
fn a_timer_naps_its_remaining_time_and_a_poll_naps_its_interval() {
    assert_eq!(wake_nap_ms("timer", 2_000, 60_000), 2_000);
    assert_eq!(wake_nap_ms("timer", 10, 60_000), 50, "never a busy-spin");
    assert_eq!(
        wake_nap_ms("timer", 999_999, 60_000),
        5_000,
        "never sleeps past a stop"
    );
    assert_eq!(
        wake_nap_ms("ci", 0, 60_000),
        60_000,
        "a GitHub wake polls on its own cadence"
    );
}

#[test]
fn a_park_has_an_explicit_state_even_without_a_timer_deadline() {
    let (h, _) = harness(Settings::default(), vec![super::tests::task("tg", "x/m")]);
    let before = h.runtime("tg").wake_state.lock().unwrap().generation;
    let rt = h.runtime("tg");
    {
        let mut state = rt.wake_state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1).max(1);
        state.parked = true;
        state.deadline_ms = 0;
    }
    let generation = rt.wake_state.lock().unwrap().generation;
    assert!(rt.wake_state.lock().unwrap().parked);
    assert!(wake_generation_is_current(&h, "tg", generation));
    assert!(generation > before);
}

#[test]
fn a_stale_watcher_generation_cannot_act_on_a_newer_park() {
    let (h, _) = harness(Settings::default(), vec![super::tests::task("tsg", "x/m")]);
    let rt = h.runtime("tsg");
    {
        let mut state = rt.wake_state.lock().unwrap();
        state.generation = 10;
        state.parked = true;
    }
    assert!(!wake_generation_is_current(&h, "tsg", 9));
    assert!(wake_generation_is_current(&h, "tsg", 10));
    assert!(
        rt.wake_state.lock().unwrap().parked,
        "stale checks do not clear the new park"
    );
}

#[test]
fn a_ci_wake_fires_on_a_new_conclusion_not_the_one_it_parked_on() {
    let mut src = parked("ci", json!({}));
    src.baseline = "100".into();
    // Still running: no conclusion, no wake.
    assert!(wake_reason(
        &src,
        &json!({"workflow_runs": [{"id": 101, "conclusion": null}]})
    )
    .is_none());
    // The very run we parked after finishes: not news to *us*.
    assert!(wake_reason(
        &src,
        &json!({"workflow_runs": [{"id": 100, "conclusion": "success"}]})
    )
    .is_none());
    // A *different* run concluding is the wake.
    let r = wake_reason(
        &src,
        &json!({"workflow_runs": [{"id": 102, "conclusion": "failure", "name": "CI", "head_branch": "main"}]}),
    )
    .expect("a new run finishing wakes the task");
    assert!(r.contains("failure") && r.contains("102"));
}

#[test]
fn a_pr_wake_fires_on_a_comment_newer_than_the_baseline() {
    let mut src = parked("pr", json!({"pr": 42}));
    src.baseline = "7".into();
    // Only the comments we already saw: nothing new.
    assert!(wake_reason(&src, &json!([{"id": 6}, {"id": 7}])).is_none());
    // A new one: wake, and name the PR.
    let r = wake_reason(&src, &json!([{"id": 7}, {"id": 8}])).expect("a new comment wakes it");
    assert!(r.contains("42"));
    // The baseline a fresh park records is the same mark `wake_reason` compares
    // against — park and poll must agree, or the first poll always fires.
    assert_eq!(baseline_of(&src, &json!([{"id": 6}, {"id": 7}])), "7");
}

#[test]
fn a_fresh_park_baselines_the_ci_run_that_is_already_there() {
    // Without this, parking "wait for CI" just after a run finished would wake
    // on that same finished run the instant the first poll ran.
    let src = parked("ci", json!({}));
    let snap = json!({"workflow_runs": [{"id": 55, "conclusion": "success"}]});
    assert_eq!(baseline_of(&src, &snap), "55");
    assert!(
        wake_reason(
            &WakeSource {
                baseline: baseline_of(&src, &snap),
                ..src
            },
            &snap
        )
        .is_none(),
        "the run we parked on must not immediately wake us"
    );
}

// ───────────────────────── wake: the park and the woken turn ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_woken_task_produces_a_follow_up_turn() {
    // The end-to-end shape: park on a timer, let it fire, and assert the chat
    // answered *again* rather than merely becoming "done" a second time.
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-wake-turn-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("wk", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = super::tests::task("tw", "wk/m");
    t.messages = vec![Message::user_text("watch the build")];
    t.status = "done".into();
    let (h, _events) = harness(s, vec![t]);

    // Park on a timer, then fire the wake directly — the watcher is then only a
    // sleep between the two, and the turn loop is what is under test.
    park_waiting(&h, "tw", parked("timer", json!({"delay_s": 30})))
        .await
        .unwrap();
    {
        let g = h.task("tw").await.unwrap();
        let g = g.lock().await;
        assert_eq!(g.status, "waiting");
        assert_eq!(g.waiting_kind.as_deref(), Some("wake"));
        assert!(
            h.runtime("tw").wake_state.lock().unwrap().parked,
            "CI/timer parks have explicit state"
        );
    }

    m.push(openai_ok("the build passed, carrying on"));
    wake_now(&h, "tw", "the timer elapsed".into()).await;
    // `resume` spawns the run; wait for it to finish.
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let g = h.task("tw").await.unwrap();
        let st = g.lock().await.status.clone();
        if st == "done" {
            break;
        }
    }

    let g = h.task("tw").await.unwrap();
    let g = g.lock().await;
    assert_eq!(g.status, "done", "the wake ran to completion");
    assert_eq!(
        g.messages.iter().filter(|m| m.role == "assistant").count(),
        1,
        "the wake produced a real follow-up assistant turn, not just a status flip"
    );
    assert!(
        !h.runtime("tw").wake_state.lock().unwrap().parked,
        "the park is cleared after firing"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parked_task_is_not_started_by_a_plain_resume_before_its_time() {
    // The deadline is enforced in `resume`, not only by the watcher sleeping:
    // otherwise `resume_all` (which walks every waiting chat) or an app restart
    // would start a task the model asked to hold.
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-wake-early-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let s = Settings::default();
    let mut t = super::tests::task("te", "anthropic/claude-opus-5");
    t.messages = vec![Message::user_text("hold")];
    let (h, _events) = harness(s, vec![t]);
    park_waiting(&h, "te", parked("timer", json!({"delay_s": 3600})))
        .await
        .unwrap();

    let err = runner::resume(&h, "te", None).await;
    assert!(
        err.is_err(),
        "an early resume must be refused while the park is live"
    );
    assert!(
        !h.runtime("te").running.load(Ordering::SeqCst),
        "and start no run"
    );

    // The *user's* message always wins, even before the deadline: a chat they
    // typed into is not waiting on anything any more.
    let _r = runner::resume(&h, "te", Some("actually, do this now".into())).await;
    assert!(
        !h.runtime("te").wake_state.lock().unwrap().parked,
        "a message clears the park"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_clears_a_park() {
    // A parked chat that the user stops must not later wake: the watcher keys on
    // `wake_at`, and `interrupt`/`force_pause` both go through `stop_tokens`.
    let dir = std::env::temp_dir().join(format!("openleash-wake-stop-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let mut t = super::tests::task("ts", "anthropic/claude-opus-5");
    t.messages = vec![Message::user_text("hold")];
    let (h, _events) = harness(Settings::default(), vec![t]);
    park_waiting(&h, "ts", parked("timer", json!({"delay_s": 3600})))
        .await
        .unwrap();
    assert!(h.runtime("ts").wake_state.lock().unwrap().parked);

    runner::interrupt(&h, "ts").await;
    assert!(
        !h.runtime("ts").wake_state.lock().unwrap().parked,
        "a stop drops the trigger"
    );
    let g = h.task("ts").await.unwrap();
    let g = g.lock().await;
    assert_ne!(
        g.waiting_kind.as_deref(),
        Some("wake"),
        "a stopped chat is not still 'waiting for' the trigger"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ───────────────────────── per-agent cost attribution (end to end) ─────────────────────────

/// A real sub-agent's spend lands in its own `by_agent` row, not the main
/// agent's. The stats *unit* is covered in `stats.rs`; this drives the actual
/// turn loop so the plumbing — which `usage_id` the router records each request
/// under — is what is under test. A well-tested `Stats::record` with the router
/// still passing `"main"` for a sub-agent is exactly the bug that hides spend.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agents_spend_is_a_row_of_its_own() {
    let _g = router::ROUTES_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("openleash-cost-split-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let m = Mock::start().await;
    let (cp, key) = custom("cs", &m.base, false);
    let mut s = Settings::default();
    s.custom_providers.push(cp);
    s.providers.insert(key.0, key.1);
    let mut t = super::tests::task("tc", "cs/m");
    t.status = "idle".into();
    let (h, _) = harness(s, vec![t]);

    // Main asks a sub-agent, the sub-agent answers, the main reports back.
    m.push(openai_call(
        "task",
        json!({"description": "look it up", "prompt": "Where is X?", "subagent_type": "explore"}),
    ));
    m.push(openai_ok("X is in src/a.rs"));
    m.push(openai_ok("Done."));
    runner::send(h.clone(), "tc".into(), "where is X?".into())
        .await
        .unwrap();
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if !h.runtime("tc").running.load(Ordering::SeqCst) {
            break;
        }
    }

    let chat = h.stats.chat("tc").expect("the chat recorded spend");
    let by = &chat.by_agent;
    assert!(
        by.contains_key("main"),
        "the main agent's requests are their own row"
    );
    let sub_key = by
        .keys()
        .find(|k| k.starts_with("sub:"))
        .unwrap_or_else(|| {
            panic!("a sub-agent must have its own row, not be folded into main: {by:?}")
        });
    assert!(
        by[sub_key].requests >= 1,
        "the sub-agent's request is recorded"
    );
    // The two rows are genuinely different slices: main and the sub both made
    // requests, and neither total swallowed the other.
    assert_eq!(
        by["main"].requests + by[sub_key].requests,
        chat.total.requests,
        "every request belongs to exactly one spender"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ───────────────────────── system one-shots are identifiable ─────────────────────────

/// A compaction, a keepalive, a ticker — every one-shot with `sub: None` — must
/// not read as the main agent in the app-wide `agents` dimension. This is the
/// "hidden system agents" half of the attribution work: without it, "how much did
/// compaction cost me" is unanswerable because the spend is filed under `main`.
#[test]
fn a_system_oneshot_is_its_own_agent_row() {
    // The main agent is the one `usage_id` that is an agent of its own.
    assert_eq!(agent_kind_for_test("main", None), "main");
    // Everything else is a system call with its own row.
    assert_eq!(agent_kind_for_test("compactor", None), "compactor");
    assert_eq!(agent_kind_for_test("keepalive", None), "keepalive");
    assert_eq!(agent_kind_for_test("summary", None), "summary");
    // A sub-agent keeps its *role* here (the dimension is per type), which is
    // how `by_agent`'s per-*instance* `sub:<id>` and this per-type key differ.
    assert_eq!(
        agent_kind_for_test("sub:abc123", Some("explore")),
        "explore"
    );
}
