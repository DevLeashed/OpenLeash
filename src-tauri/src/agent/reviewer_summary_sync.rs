//! A summary's `rev` is what lets the UI order two summaries of the same chat.
//!
//! The desktop window and a paired browser each hold a chat list they update
//! from live `task` events, and they also *read* the list back — at startup
//! (`app_boot`), when a chat is opened (`task_open`), and when the page comes
//! back into view. A snapshot is built at one instant and lands whenever it
//! lands, so it races every event published while its request was in flight.
//! Without a marker the store applied it anyway, which put the chat back where
//! it was before those events: "running" (spinner and all) after the run had
//! finished, "waiting" for an approval that had already been given — and nothing
//! republished it, because a settled chat publishes nothing more. A reload
//! re-ran the same race, so it came back intermittently and sometimes outlived
//! the reload.
//!
//! `SUMMARY_REV` is stamped on *every* summary, live event or snapshot, which is
//! what makes them comparable. These tests pin that contract: a summary built
//! after a change is strictly newer than one built before it.
use super::tests::{harness, task};
use serde_json::Value;

/// The two properties the UI relies on: every summary is stamped, and a later
/// one is strictly greater. Written as a sequence rather than a single pair,
/// because a marker that only moves once per task — or one taken from the wall
/// clock — satisfies a one-shot comparison while breaking ordering under load.
#[tokio::test]
async fn every_summary_is_stamped_and_later_ones_outrank_earlier_ones() {
    let (h, _events) = harness(
        Default::default(),
        vec![task("a", "anthropic/claude-opus-5")],
    );
    let t = h.task("a").await.unwrap();

    let mut revs = vec![];
    for _ in 0..50 {
        revs.push(t.lock().await.summary().rev);
    }
    for pair in revs.windows(2) {
        assert!(
            pair[1] > pair[0],
            "summaries must be strictly ordered, got {} then {}",
            pair[0],
            pair[1]
        );
    }
}

/// The shape the store actually meets: a snapshot taken *before* a change, and
/// the event published *for* that change. The event has to win.
#[tokio::test]
async fn an_event_outranks_a_snapshot_taken_before_it() {
    let (h, events) = harness(
        Default::default(),
        vec![task("a", "anthropic/claude-opus-5")],
    );
    let t = h.task("a").await.unwrap();

    // What `app_boot`/`task_open` read back, while the run below is still going.
    let snapshot = t.lock().await.summary();
    assert_ne!(snapshot.status, "done", "the snapshot predates the change");

    // The run ends and publishes.
    h.update_task("a", |t| t.status = "done".into()).await;
    let published = events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(name, payload)| {
            name == "ol://event" && payload["task_id"] == "a" && payload["kind"] == "task"
        })
        .map(|(_, payload)| payload["payload"].clone())
        .expect("the status change is published as a task event");

    assert_eq!(published["status"], "done");
    assert!(
        published_rev(&published) > snapshot.rev,
        "the event for the change must outrank a snapshot read before it, or the UI \
         paints the snapshot over it and the chat is stuck on {:?}",
        snapshot.status
    );
}

/// Both routes out of the harness are stamped: the summary an event carries and
/// the summary a snapshot is built from come from the same counter.
#[tokio::test]
async fn the_event_and_the_snapshot_carry_that_stamp() {
    let (h, events) = harness(
        Default::default(),
        vec![task("a", "anthropic/claude-opus-5")],
    );
    h.emit_container("a").await;
    let published = events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(name, payload)| name == "ol://event" && payload["kind"] == "task")
        .map(|(_, payload)| payload["payload"].clone())
        .expect("emit_container publishes a task summary");
    let live = h.task("a").await.unwrap().lock().await.summary();
    assert!(
        published["rev"].is_u64(),
        "a published summary must carry its stamp, got {}",
        published["rev"]
    );
    assert!(
        live.rev > published_rev(&published),
        "the snapshot read after it is newer"
    );
}

fn published_rev(payload: &Value) -> u64 {
    payload["rev"]
        .as_u64()
        .unwrap_or_else(|| panic!("a published summary must carry a numeric rev: {payload}"))
}

/// A published summary leaves the reports out, and the full one keeps them.
///
/// The rule that `task` events do not carry subagent reports is older than this
/// test; what it pins is *where* the reports are dropped. They used to be cloned
/// whole by `summary()` and cleared afterwards, which cost a deep copy of every
/// agent's report — up to 20k characters each — on an event that fires for every
/// status change of every agent, so a 20-agent swarm built and threw away
/// several hundred kilobytes per event. The optimisation must not leak into the
/// snapshot path: a panel that opens a subagent reads `summary()` (via
/// `task_get`) and still needs the report there, so both halves are asserted.
#[tokio::test]
async fn a_published_summary_drops_reports_but_the_full_one_keeps_them() {
    let mut t = task("a", "anthropic/claude-opus-5");
    t.subs = vec![
        super::SubInfo {
            id: "w1".into(),
            role: "explore".into(),
            status: "done".into(),
            report: "the long report w1".into(),
            ..Default::default()
        },
        super::SubInfo {
            id: "w2".into(),
            role: "general".into(),
            status: "running".into(),
            report: "the long report w2".into(),
            ..Default::default()
        },
    ];
    let (h, events) = harness(Default::default(), vec![t]);

    h.emit_container("a").await;
    let published = events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(name, payload)| name == "ol://event" && payload["kind"] == "task")
        .map(|(_, payload)| payload["payload"].clone())
        .expect("emit_container publishes a task summary");

    let subs = published["subs"]
        .as_array()
        .expect("the published summary carries its subagents");
    assert_eq!(subs.len(), 2, "both agents are still published");
    for (sub, id) in subs.iter().zip(["w1", "w2"]) {
        assert_eq!(sub["id"], id, "the same agents, in order");
        assert_eq!(
            sub["report"], "",
            "a published summary must not carry {id}'s report"
        );
    }

    // The other half: the full summary a subagent panel reads still has them,
    // and the variant the emit path uses is the only one that drops them.
    let full = h.task("a").await.unwrap().lock().await.summary();
    let dropped = h
        .task("a")
        .await
        .unwrap()
        .lock()
        .await
        .summary_without_reports();
    assert_eq!(
        full.subs
            .iter()
            .map(|s| s.report.as_str())
            .collect::<Vec<_>>(),
        vec!["the long report w1", "the long report w2"],
        "summary() keeps the reports: a panel that opens an agent reads them here"
    );
    assert!(
        dropped.subs.iter().all(|s| s.report.is_empty()),
        "summary_without_reports() is what the emit path builds"
    );
    // Everything else about the two summaries has to agree, or the optimisation
    // would be changing what the UI sees rather than only what is copied.
    assert_eq!(full.subs.len(), dropped.subs.len());
    for (a, b) in full.subs.iter().zip(&dropped.subs) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.status, b.status);
        assert_eq!(a.meta, b.meta);
        assert_eq!(a.item_id, b.item_id);
        assert_eq!(a.depth, b.depth);
        assert_eq!(a.parent, b.parent);
    }
}
