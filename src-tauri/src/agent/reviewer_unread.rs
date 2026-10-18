//! The blue dot: which attention is news, and what clears it.
//!
//! The mark lives on the chat (`Task::unread`) and every summary carries it, so
//! the sidebar draws the same dot in the desktop window, in a second window and
//! in a browser driving this app over remote control — including a page that has
//! only just loaded and therefore never saw the `ol://attention` event at all.
//!
//! These rules used to live in the frontend's store (`src/unread.test.ts`), where
//! only the page that was running when the event fired ever learned about them: a
//! remote browser reloads on every reconnect (`remote/shim.js`) and a phone that
//! was asleep missed the event entirely, which is why a finished chat showed up
//! grey there and blue in the window. The rules moved with the state.
use super::tests::{harness, task};
use super::{attention_is_news, store, Task, TaskSummary};
use serde_json::json;

/// The mark as a client sees it: off the summary, not off the task.
async fn marked(h: &super::Harness, id: &str) -> bool {
    let t = h.task(id).await.unwrap();
    let summary: TaskSummary = t.lock().await.summary();
    summary.unread
}

/// Every `ol://event` summary pushed for this chat, oldest first, as its mark.
/// This is what a client that is merely connected learns — and the only thing a
/// page that loads later ever sees.
fn pushed(events: &super::tests::Events, id: &str) -> Vec<bool> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, v)| name == "ol://event" && v["task_id"] == id && v["kind"] == "task")
        .map(|(_, v)| v["payload"]["unread"].as_bool().unwrap_or(false))
        .collect()
}

/// A chat with the mark already set — something happened here while nobody was
/// looking, and here is the block that decides what that means.
fn waiting(id: &str, kind: Option<&str>) -> Task {
    let mut t = task(id, "unused/model");
    t.status = if kind.is_some() {
        "waiting".into()
    } else {
        "done".into()
    };
    t.waiting_kind = kind.map(str::to_string);
    t.unread = true;
    t
}

#[test]
fn only_news_is_a_reason_to_mark_a_chat() {
    let news = |kind: &str| attention_is_news(&json!({ "kind": kind }));
    for kind in [
        "done",
        "failed",
        "question",
        "approval",
        "nonblocking",
        "notice",
    ] {
        assert!(news(kind), "{kind} is something the user has not seen");
    }
    // A stop and a park are states the user asked for, or ones that announce
    // themselves when the trigger fires. `describe()` in `src/notify.ts` words a
    // toast for exactly the list above, so the dot follows the same list.
    for kind in ["stopped", "waiting", "", "idle"] {
        assert!(!news(kind), "{kind:?} is not news");
    }
    assert!(
        attention_is_news(&json!({"kind": "paused", "pause": "exhausted", "reason": "no models"})),
        "a pause the app raised on its own is the agent asking for help"
    );
    assert!(
        !attention_is_news(
            &json!({"kind": "paused", "pause": "manual", "reason": "Paused by you"})
        ),
        "a pause the user pressed is what they were looking at when they pressed it"
    );
}

#[test]
fn an_attention_event_marks_the_chat_and_pushes_it() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (h, events) = harness(store::Settings::default(), vec![task("t1", "unused/model")]);
            assert!(!marked(&h, "t1").await, "a fresh chat is not marked");
            h.attention("t1", json!({"task_id": "t1", "kind": "done"}))
                .await;
            assert!(marked(&h, "t1").await, "the run finished unlooked-at");
            assert_eq!(
                pushed(&events, "t1").last().copied(),
                Some(true),
                "and connected clients are told, by the summary itself"
            );
            assert!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(name, v)| name == "ol://attention" && v["kind"] == "done"),
                "the attention event is still published as before"
            );
        });
}

#[test]
fn a_stop_does_not_mark_the_chat() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (h, events) = harness(store::Settings::default(), vec![task("t1", "unused/model")]);
            h.attention("t1", json!({"task_id": "t1", "kind": "stopped"}))
                .await;
            assert!(!marked(&h, "t1").await, "you pressed stop");
            assert!(
                pushed(&events, "t1").is_empty(),
                "so there was nothing to republish"
            );
        });
}

#[test]
fn a_chat_finished_on_screen_is_read_once_the_window_says_so() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (h, events) = harness(store::Settings::default(), vec![task("t1", "unused/model")]);
            // It finishes while the window has it open: the backend marks it,
            // then the window (which can see the chat is on screen) acknowledges
            // it with `task_seen`. Left alone, the dot would return on switching away.
            h.attention("t1", json!({"task_id": "t1", "kind": "done"}))
                .await;
            assert!(marked(&h, "t1").await, "the run finished");
            h.seen("t1").await;
            assert!(!marked(&h, "t1").await, "the window had it on screen");
            assert_eq!(pushed(&events, "t1").last().copied(), Some(false));
        });
}

#[test]
fn opening_a_chat_clears_the_mark_for_every_client() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (h, events) = harness(store::Settings::default(), vec![waiting("t1", None)]);
            h.seen("t1").await;
            assert!(!marked(&h, "t1").await, "looking at it is acknowledging it");
            assert_eq!(
                pushed(&events, "t1").last().copied(),
                Some(false),
                "and the other clients are told the dot is out"
            );
            // Opening it again is not news either, and resends nothing.
            let before = pushed(&events, "t1").len();
            h.seen("t1").await;
            assert_eq!(pushed(&events, "t1").len(), before);
        });
}

#[test]
fn a_pending_question_keeps_its_mark_through_an_open() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (h, events) = harness(
                store::Settings::default(),
                vec![
                    waiting("q", Some("question")),
                    waiting("a", Some("approval")),
                ],
            );
            h.seen("q").await;
            h.seen("a").await;
            assert!(marked(&h, "q").await, "read it, then left it unanswered");
            assert!(marked(&h, "a").await, "same for an approval");
            assert!(
                pushed(&events, "q").is_empty(),
                "and nothing was resent over it"
            );
        });
}

#[test]
fn ending_a_blocking_wait_clears_the_mark() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (h, _events) = harness(store::Settings::default(), vec![task("q", "unused/model")]);
            let cancel = tokio_util::sync::CancellationToken::new();
            let ask = h.wait_for_user("q", "item-1", "question", &cancel);
            // Answered from another device, so nothing here ever opened the
            // chat: the mark has to go out anyway, or the chat stays blue on
            // every client with nothing left to clear it.
            let answer = async {
                let tx = h
                    .runtime("q")
                    .pending
                    .lock()
                    .await
                    .remove("item-1")
                    .expect("the wait registers its pending item");
                let _ = tx.send(json!({"answer": "postgres"}));
            };
            let (res, ()) = tokio::join!(ask, answer);
            assert!(res.is_some(), "the answer reached the waiting run");
            assert!(!marked(&h, "q").await);
            let t = h.task("q").await.unwrap();
            let t = t.lock().await;
            assert_eq!(t.waiting_kind, None, "the wait is over");
            assert_eq!(t.status, "running", "and the run carries on");
        });
}

#[test]
fn resuming_a_chat_acknowledges_the_mark_it_had() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // A chat that finished while nobody was looking, then is picked back up
            // from the list (not opened). Resuming is the user acting on it, so the
            // old "done" mark must not survive into the resumed chat — otherwise a
            // finished-looking chat shows the blue dot as soon as it is resumed.
            let (h, events) = harness(store::Settings::default(), vec![waiting("t1", None)]);
            assert!(marked(&h, "t1").await, "precondition: the chat is unread");
            super::runner::resume(&h, "t1", None).await.unwrap();
            assert!(
                !marked(&h, "t1").await,
                "resuming acknowledged the old finish"
            );
            assert_eq!(
                pushed(&events, "t1").last().copied(),
                Some(false),
                "and connected clients are told the dot is out"
            );
        });
}

#[test]
fn resuming_does_not_clear_a_pending_question() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // The question is still unanswered: resuming must not hide it.
            let (h, _events) = harness(
                store::Settings::default(),
                vec![waiting("q", Some("question"))],
            );
            super::runner::resume(&h, "q", None).await.unwrap();
            assert!(marked(&h, "q").await, "the blocking wait keeps its mark");
        });
}
