//! The quota burner's driver (`burner::tick`) against a real harness: the pure
//! decisions are covered beside them in `burner.rs`; these check that the
//! driver actually feeds them the account's live reading, honours a usage cap
//! the user set, and interrupts a live burner chat at its hard stop.

#![cfg(test)]
// The data-folder lock is held across the awaits on purpose: `tick` writes
// `burner.json` and `settings.json`, and another test pointing OPENLEASH_HOME
// elsewhere mid-tick would send those writes to the wrong folder.
#![allow(clippy::await_holding_lock)]

use super::accounts::{now, AcctUsage, Window};
use super::burner::{self, BacklogItem, BurnRun};
use super::store::{self, Account, Settings, UsageCap};
use super::tests::{harness, task};
use super::Harness;
use std::sync::Arc;

fn home(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ol-burner-{name}-{}", std::process::id()))
}

fn account(caps: Vec<UsageCap>) -> Account {
    Account {
        id: "a1".into(),
        kind: "claude".into(),
        label: "work".into(),
        enabled: true,
        caps,
        ..Default::default()
    }
}

/// Settings with the burner on and one backlog item for `project`.
fn settings(project: &str, caps: Vec<UsageCap>) -> Settings {
    let mut s = Settings {
        model: "claude/claude-sonnet-5".into(),
        perm: "auto".into(),
        accounts: vec![account(caps)],
        ..Default::default()
    };
    s.burner.enabled = true;
    s.burner.queue.push(BacklogItem {
        id: "q1".into(),
        project: project.into(),
        prompt: "sweep TODOs".into(),
        ..Default::default()
    });
    s
}

fn read(h: &Harness, used: f64, resets_in: i64) {
    h.accts.set_usage(
        "a1",
        AcctUsage {
            windows: vec![Window {
                label: "5h".into(),
                used,
                resets_at: now() + resets_in,
            }],
            fetched: now(),
            ..Default::default()
        },
    );
}

/// A plain folder: the burner refuses it (no worktree possible), which is the
/// observable proof that it *tried* to start the item without ever reaching
/// a provider.
fn plain_dir(dir: &std::path::Path) -> String {
    let p = dir.join("project");
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().to_string()
}

async fn queue(h: &Arc<Harness>) -> Vec<BacklogItem> {
    h.settings.read().await.burner.queue.clone()
}

#[test]
fn two_eligible_providers_start_one_shared_queue_item_only_once() {
    let s = settings("/dummy/project", vec![]);
    let mut started = std::collections::HashSet::new();
    let mut starts = vec![];
    for kind in ["claude", "codex"] {
        let usage = AcctUsage {
            windows: vec![Window {
                label: "5h".into(),
                used: 10.0,
                resets_at: now() + 600,
            }],
            fetched: now(),
            ..Default::default()
        };
        assert!(burner::trigger(&usage, &[], &s.burner, now()).is_ok());
        if let Some(item) = burner::claim_next_item(&s.burner, kind, &mut started) {
            // Stand in for a successful start, without a worktree or live API.
            starts.push((kind, item.id.clone()));
        }
    }
    assert_eq!(starts, vec![("claude", "q1".into())]);
    assert!(started.contains("q1"), "reserve before the next provider");
}

#[tokio::test]
async fn overlapping_ticks_wait_before_reading_their_snapshots() {
    let dir = home("overlap");
    let _g = store::test_home(&dir);
    let (first_h, _) = harness(Settings::default(), vec![]);
    let (second_h, _) = harness(Settings::default(), vec![]);
    // Hold the first tick inside its critical section, before its snapshot.
    let settings = first_h.settings.write().await;
    let first = burner::tick(&first_h);
    tokio::pin!(first);
    assert!(futures_util::poll!(&mut first).is_pending());
    let second = burner::tick(&second_h);
    tokio::pin!(second);
    assert!(
        futures_util::poll!(&mut second).is_pending(),
        "another tick must not read runs/settings while the first is in flight"
    );
    drop(settings);
    first.await;
    second.await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_reached_usage_cap_keeps_the_burner_from_starting() {
    let dir = home("cap");
    let _g = store::test_home(&dir);
    let project = plain_dir(&dir);
    let cap = UsageCap {
        window: "5h".into(),
        used: 40.0,
        until: 0,
    };
    let (h, _) = harness(settings(&project, vec![cap]), vec![]);
    // Plenty of window left by the burner's own measure, and resetting soon —
    // but the user capped this window at 40% and it is there.
    read(&h, 40.0, 10 * 60);
    burner::tick(&h).await;
    assert!(burner::load_runs().is_empty());
    let q = queue(&h).await;
    assert_eq!(q.len(), 1);
    assert!(q[0].error.is_empty(), "the item must not even be attempted");

    // A cap that leaves less headroom than asked for blocks too.
    h.settings.write().await.accounts[0].caps[0].used = 50.0;
    read(&h, 35.0, 10 * 60);
    burner::tick(&h).await;
    assert!(queue(&h).await[0].error.is_empty());

    // The same reading with the cap lifted is a start attempt (refused only
    // because the folder is not a git repository).
    h.settings.write().await.accounts[0].caps.clear();
    burner::tick(&h).await;
    let q = queue(&h).await;
    assert!(q[0].error.contains("git"), "{:?}", q[0].error);
    assert!(burner::load_runs().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn nothing_starts_while_off_or_far_from_a_reset() {
    let dir = home("off");
    let _g = store::test_home(&dir);
    let project = plain_dir(&dir);
    let mut s = settings(&project, vec![]);
    s.burner.enabled = false;
    let (h, _) = harness(s, vec![]);
    read(&h, 10.0, 10 * 60);
    burner::tick(&h).await;
    assert!(queue(&h).await[0].error.is_empty(), "off means off");

    h.settings.write().await.burner.enabled = true;
    read(&h, 10.0, 3 * 3600);
    burner::tick(&h).await;
    assert!(queue(&h).await[0].error.is_empty(), "reset is hours away");
    let _ = std::fs::remove_dir_all(&dir);
}

fn live_run(resets_in: i64) -> BurnRun {
    BurnRun {
        task_id: "t1".into(),
        item_id: "q0".into(),
        account_id: "a1".into(),
        kind: "claude".into(),
        window: "5h".into(),
        resets_at: now() + resets_in,
        started_at: now() - 600,
        ..Default::default()
    }
}

async fn stopped_notice(h: &Arc<Harness>) -> bool {
    let t = h.task("t1").await.unwrap();
    let t = t.lock().await;
    t.items
        .iter()
        .any(|i| i.text.starts_with("Quota burner stopped this chat"))
}

#[tokio::test]
async fn a_live_run_is_stopped_when_its_window_resets() {
    let dir = home("reset");
    let _g = store::test_home(&dir);
    let project = plain_dir(&dir);
    let (h, _) = harness(
        settings(&project, vec![]),
        vec![task("t1", "claude/claude-sonnet-5")],
    );
    read(&h, 30.0, 600);
    // Still inside the window: carries on.
    burner::save_runs(&[live_run(600)]);
    burner::tick(&h).await;
    assert!(burner::load_runs()[0].active());
    assert!(!stopped_notice(&h).await);
    // Past the recorded reset: stopped and told why.
    burner::save_runs(&[live_run(-5)]);
    burner::tick(&h).await;
    let r = &burner::load_runs()[0];
    assert!(!r.active());
    assert!(r.end_reason.contains("reset"), "{}", r.end_reason);
    assert!(stopped_notice(&h).await);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_live_run_is_stopped_at_the_spend_cap_or_a_user_cap() {
    let dir = home("spend");
    let _g = store::test_home(&dir);
    let project = plain_dir(&dir);
    let (h, _) = harness(
        settings(&project, vec![]),
        vec![task("t1", "claude/claude-sonnet-5")],
    );
    burner::save_runs(&[live_run(600)]);
    read(&h, 90.0, 600);
    burner::tick(&h).await;
    let r = &burner::load_runs()[0];
    assert!(!r.active());
    assert!(r.end_reason.contains("spend cap"), "{}", r.end_reason);

    // A user cap below the spend cap stops it first.
    h.settings.write().await.accounts[0].caps = vec![UsageCap {
        window: "5h".into(),
        used: 50.0,
        until: 0,
    }];
    burner::save_runs(&[live_run(600)]);
    read(&h, 55.0, 600);
    burner::tick(&h).await;
    let r = &burner::load_runs()[0];
    assert!(!r.active());
    assert!(r.end_reason.contains("usage cap"), "{}", r.end_reason);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_finished_or_deleted_chat_ends_its_run_without_a_stop() {
    let dir = home("done");
    let _g = store::test_home(&dir);
    let project = plain_dir(&dir);
    let mut done = task("t1", "claude/claude-sonnet-5");
    done.status = "done".into();
    let (h, _) = harness(settings(&project, vec![]), vec![done]);
    read(&h, 30.0, 4 * 3600);
    let mut gone = live_run(600);
    gone.task_id = "missing".into();
    burner::save_runs(&[live_run(600), gone]);
    burner::tick(&h).await;
    let runs = burner::load_runs();
    assert_eq!(runs[0].end_reason, "finished");
    assert_eq!(runs[1].end_reason, "chat deleted");
    assert!(!stopped_notice(&h).await);
    // A started item never comes back, even if a stale UI copy re-adds it.
    h.settings.write().await.burner.queue[0].id = "q0".into();
    burner::tick(&h).await;
    assert!(queue(&h).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
