//! Stop is a force pause.
//!
//! The keystroke that means "stop doing that" must not throw the work away. It
//! cuts what is running, freezes the chat where it stands, and tells each agent
//! which of *its own* commands died — while every subagent keeps its transcript
//! and the turn resumes at its next request. Cancelling the run instead is what
//! ended the turn and left a swarm to be resurrected from saved conversations by
//! the next turn, which read as "stop killed my subagents".
//!
//! `interrupt` is still the outright cancellation, and carries the same
//! per-agent note: a subagent whose dev server the user killed has to hear about
//! it whichever kind of stop took it down.
//!
//! The per-agent scoping is why `BgJob::sub_id` is read rather than just the
//! chat-wide list: a note naming another agent's build is noise the reader has to
//! rule out before it can decide whether its own server is still up.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::runner;
use super::store::Settings;
use super::tests::{harness, task};
use super::{Harness, SubInfo};

/// Let a killed job's process report its exit, and fail loudly if it never does.
async fn settled(h: &Harness, id: &str) {
    let job = h.bg.get(id).expect("the background job");
    let _ = tokio::time::timeout(Duration::from_secs(20), job.done.cancelled()).await;
    assert!(
        job.exit.lock().unwrap().is_some(),
        "the command was asked to stop but never reported its exit"
    );
}

async fn notes(h: &Harness, task_id: &str, agent: &str) -> String {
    let rt = h.runtime(task_id);
    let q = rt.inbox.lock().await;
    q.get(agent).cloned().unwrap_or_default().join("\n")
}

/// Stopping a running chat freezes it. The run is not cancelled, the subagents
/// stay running (paused with the chat), and each agent is told about its own
/// killed commands and nobody else's.
#[tokio::test]
async fn stop_freezes_the_chat_and_tells_each_agent_about_its_own_commands() {
    let (h, _) = harness(Settings::default(), vec![task("tstop", "m/x")]);
    {
        let t = h.task("tstop").await.unwrap();
        t.lock().await.subs.push(SubInfo {
            id: "subA".into(),
            role: "general".into(),
            task: "build it".into(),
            status: "running".into(),
            background: true,
            ..Default::default()
        });
    }
    // A live run, as `run_main_reserved` leaves it while the turn is in flight.
    h.runtime("tstop").running.store(true, Ordering::SeqCst);
    let chats =
        h.bg.spawn("tstop", "", "sleep 30", ".")
            .expect("the chat's own command");
    let worker =
        h.bg.spawn("tstop", "subA", "sleep 30", ".")
            .expect("the subagent's command");

    runner::stop(&h, "tstop").await;

    let t = h.task("tstop").await.unwrap();
    let g = t.lock().await;
    let pause = g.paused.clone().expect("a stop freezes the chat");
    assert_eq!(
        pause.kind, "manual",
        "the user's own freeze, not an exhausted pause"
    );
    assert_eq!(
        g.subs[0].status, "running",
        "the subagent pauses with the chat instead of being marked stopped"
    );
    drop(g);
    assert!(
        h.runtime("tstop").running.load(Ordering::SeqCst),
        "a stop does not cancel the run: Resume continues it"
    );

    settled(&h, &chats).await;
    settled(&h, &worker).await;

    let main = notes(&h, "tstop", "main").await;
    let sub = notes(&h, "tstop", "subA").await;
    assert!(
        main.contains(&chats) && !main.contains(&worker),
        "the chat's agent hears about the chat's own command only: {main}"
    );
    assert!(
        sub.contains(&worker) && !sub.contains(&chats),
        "the subagent hears about its own command only: {sub}"
    );
    assert!(
        sub.contains("killed before they finished"),
        "the note has to say the work is gone, not just that it stopped: {sub}"
    );
}

/// Stop on a chat with nothing in flight is a no-op, not a freeze: an idle chat
/// that starts reading as paused has nothing to resume and no reason to say so.
#[tokio::test]
async fn stop_on_an_idle_chat_does_not_pause_it() {
    let (h, _) = harness(Settings::default(), vec![task("tidle", "m/x")]);

    runner::stop(&h, "tidle").await;

    let t = h.task("tidle").await.unwrap();
    assert!(
        t.lock().await.paused.is_none(),
        "an idle chat must not be frozen by a stop"
    );
}

/// The outright cancellation keeps its own meaning — the run ends — and still
/// tells the subagent what it lost, which used to reach the main agent only.
#[tokio::test]
async fn the_outright_stop_still_tells_the_subagent_about_its_commands() {
    let (h, _) = harness(Settings::default(), vec![task("thard", "m/x")]);
    {
        let t = h.task("thard").await.unwrap();
        t.lock().await.subs.push(SubInfo {
            id: "subB".into(),
            role: "general".into(),
            task: "watch it".into(),
            status: "running".into(),
            background: true,
            ..Default::default()
        });
    }
    let worker =
        h.bg.spawn("thard", "subB", "sleep 30", ".")
            .expect("the subagent's command");

    runner::interrupt(&h, "thard").await;

    settled(&h, &worker).await;
    let sub = notes(&h, "thard", "subB").await;
    assert!(
        sub.contains(&worker),
        "the subagent is told its own command was killed: {sub}"
    );
    assert!(
        notes(&h, "thard", "main").await.is_empty(),
        "nothing of the chat's own was killed, so its agent has nothing to hear"
    );
}
