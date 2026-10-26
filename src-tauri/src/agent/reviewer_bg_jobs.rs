//! A summary names the background commands that are still alive.
//!
//! A pause does not stop them. A dev server, a watcher or a build is meant to
//! outlive one, which is why `refresh_busy` deliberately leaves them out of
//! `busy` — a job that never exits by itself would hold "Pausing… waiting on N
//! commands to finish" up forever. The price of that decision is a banner
//! reading "Everything is paused" over a machine that is still serving on a
//! port and rebuilding on every save, so the summary carries the live list and
//! the home screen names it under that banner.
//!
//! Two things have to hold for that to be honest, and each has a test here.
//! The list only ever holds jobs that are running *now* — it is a live mirror,
//! not the `bg_live` record of what died with the app — and a job starting or
//! exiting publishes a summary, because every other field in the UI's key
//! deliberately stays still when one does.
//!
//! The `test_home` guard has to span the whole test to serialize `OPENLEASH_HOME`,
//! so it is deliberately held across `.await` points. See AGENTS.md: do not drop
//! the lock to quiet the lint. Same allowance as `tests.rs` and the other modules
//! that spawn real background jobs.
#![allow(clippy::await_holding_lock)]

use super::store::Settings;
use super::tests::{harness, task, Events};
use serde_json::Value;

/// The most recent summary published for a chat.
fn published(events: &Events, id: &str) -> Value {
    events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(name, payload)| {
            name == "ol://event" && payload["task_id"] == id && payload["kind"] == "task"
        })
        .map(|(_, payload)| payload["payload"].clone())
        .expect("the chat published a summary")
}

/// A job that is up is named, and its exit takes it away again — through the
/// summary the UI reads, not only through the chat that started it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_job_is_named_in_the_summary_and_leaves_when_it_exits() {
    let dir = std::env::temp_dir().join(format!("openleash-bgjobs-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let (h, events) = harness(Settings::default(), vec![task("t1", "m1")]);
    let t = h.task("t1").await.unwrap();
    assert!(
        t.lock().await.summary().bg_now.is_empty(),
        "a chat with nothing running names nothing"
    );

    // Long-lived on purpose: this is the shape the banner has to own up to.
    let id = h.bg.spawn("t1", "", "sleep 30", ".").unwrap();
    h.refresh_busy("t1").await;
    let s = t.lock().await.summary();
    assert_eq!(
        s.bg_now.len(),
        1,
        "the job is alive and named: {:?}",
        s.bg_now
    );
    assert_eq!(s.bg_now[0].id, id);
    assert_eq!(s.bg_now[0].cmd, "sleep 30");
    assert!(s.bg_now[0].running);
    drop(s);
    assert_eq!(
        published(&events, "t1")["bg_now"][0]["id"],
        id,
        "a job starting has to be published, not only listed: nothing else in the key moves for it"
    );

    // What the runner's exit watcher does the moment the process is gone.
    assert!(h.bg.kill(&id), "the job was there to kill");
    let _ = h.bg.get(&id).unwrap().done.cancelled().await;
    h.refresh_busy("t1").await;
    assert!(
        t.lock().await.summary().bg_now.is_empty(),
        "a job that has exited is not live work"
    );
    assert!(
        published(&events, "t1")["bg_now"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "and its exit publishes too, or the banner keeps naming a server that is already gone"
    );
    h.bg.kill_task_with_reasons("t1", "test cleanup");
}

/// The pair this feature exists for: a long-lived job is not counted as the
/// pause draining, and the summary names it anyway.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_chat_still_names_the_jobs_its_pause_does_not_stop() {
    let dir = std::env::temp_dir().join(format!("openleash-bgpause-{}", std::process::id()));
    let _home = super::store::test_home(&dir);
    let (h, _events) = harness(Settings::default(), vec![task("t1", "m1")]);
    let id = h.bg.spawn("t1", "", "sleep 30", ".").unwrap();

    // No recount of our own: the pause's own recount is what has to fill the
    // list, since that is the instant the banner appears.
    h.pause_task("t1", "manual", "Paused by you").await;
    let s = h.task("t1").await.unwrap().lock().await.summary();
    assert_eq!(
        s.busy, 0,
        "a job that never exits by itself is not the pause draining"
    );
    assert_eq!(
        s.bg_now.len(),
        1,
        "the pause does not stop it, so the summary still names it: {:?}",
        s.bg_now
    );
    assert_eq!(s.bg_now[0].id, id);
    h.bg.kill_task_with_reasons("t1", "test cleanup");
}

/// The record of what died with the app is not a live list.
///
/// `bg_live` is written as the app quits — after the kills, so a later session
/// can tell the agent to start those commands again — and it therefore names
/// jobs that no longer exist. A summary built from it would list them as
/// running, which is the exact lie the home banner is being fixed for.
#[tokio::test]
async fn the_record_of_what_died_with_the_app_is_not_published_as_live() {
    let mut dead = task("t1", "m1");
    dead.bg_live = vec!["bg1: npm run dev".into()];
    let (h, _events) = harness(Settings::default(), vec![dead]);
    let s = h.task("t1").await.unwrap().lock().await.summary();
    assert!(
        s.bg_now.is_empty(),
        "nothing is running under this chat, whatever the record says: {:?}",
        s.bg_now
    );
}
