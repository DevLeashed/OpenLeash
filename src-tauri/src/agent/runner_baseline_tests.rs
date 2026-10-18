//! The first-send checkpoint baseline must not sit in front of the model
//! request, and must still land before anything the agent does.
//!
//! `Harness::touch` takes a *user* snapshot of the work tree — on a new chat
//! that is `init --bare`, config, `status`, `add -A` of the whole checkout and a
//! commit, seconds on a large repository. The send path used to await it before
//! starting the run. It now runs in the background behind a per-task barrier
//! (`Runtime::checkpoints_settled`), and every tool waits on that barrier: a
//! command or edit that landed first would be folded into the "user's own
//! work" baseline, and a later rewind would refuse to put it back.

// The checkpoint write lock and the `test_home` guard are std mutexes held
// across awaits on purpose: they stand in for a slow snapshot and serialize
// `OPENLEASH_HOME` for the whole test.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::agent::tests::{harness, task};
use std::time::Duration;

struct Fx {
    h: Arc<Harness>,
    work: String,
    home: std::path::PathBuf,
    _home: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// An idle local chat with checkpoints on, whose folder holds one user file.
fn fixture(name: &str) -> Fx {
    let home = std::env::temp_dir().join(format!(
        "ol-baseline-{name}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let _home = store::test_home(&home);
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("mine.txt"), "the user's file\n").unwrap();
    let work = work.to_string_lossy().to_string();
    let mut t = task("cp", "unused/model");
    t.status = "done".into();
    t.project = work.clone();
    t.cwd = work.clone();
    t.perm = "auto".into();
    let (h, _) = harness(store::Settings::default(), vec![t]);
    Fx {
        h,
        work,
        home,
        _home,
    }
}

/// The send path's touch must return while the user snapshot is still
/// waiting on git. Before the fix `touch` awaited the snapshot, so with the
/// checkpoint store busy it could not return at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn touch_does_not_wait_for_the_user_snapshot() {
    let fx = fixture("touch");
    let hold = checkpoint::hold_writes_for_test();
    let returned = tokio::time::timeout(Duration::from_secs(3), fx.h.touch("cp")).await;
    assert!(
        returned.is_ok(),
        "touch blocked on the checkpoint snapshot; the first model request waits behind it"
    );
    assert_eq!(
        checkpoint::target("cp", 0),
        None,
        "the snapshot cannot have committed while the store is held"
    );
    drop(hold);
    // Still taken, and with the marker fixed at touch time (count 0).
    tokio::time::timeout(
        Duration::from_secs(60),
        fx.h.runtime("cp").checkpoints_settled(),
    )
    .await
    .expect("the queued snapshot finishes once the store is free");
    let base = checkpoint::target("cp", 0).expect("the user baseline was recorded");
    assert!(
        checkpoint::changed_now("cp", &fx.work, &base).is_empty(),
        "the baseline holds the user's file"
    );
}

fn agent(fx: &Fx, cancel: CancellationToken) -> Agent {
    Agent {
        h: fx.h.clone(),
        task_id: "cp".into(),
        sub: None,
        cancel,
    }
}

/// A tool dispatched while the baseline is still committing waits for it, so
/// the agent's write is not folded into "the user's work" — which is what
/// would make a rewind refuse to remove the file the agent created.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_does_not_run_before_the_user_baseline_commits() {
    let fx = fixture("tool");
    let hold = checkpoint::hold_writes_for_test();
    fx.h.touch("cp").await;
    let a = agent(&fx, CancellationToken::new());
    let written = Path::new(&fx.work).join("agent.txt");
    let tool = tokio::spawn(async move {
        a.exec_inner(
            "write_file",
            &json!({"path": "agent.txt", "content": "the agent's file\n"}),
            None,
            "c",
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !written.exists() && !tool.is_finished(),
        "the write ran while the user baseline was still pending"
    );
    drop(hold);
    let out = tokio::time::timeout(Duration::from_secs(60), tool)
        .await
        .expect("the tool runs once the baseline lands")
        .unwrap();
    assert!(out.is_ok(), "{out:?}");
    assert!(written.exists());
    let base = checkpoint::target("cp", 0).expect("the user marker is in the ledger");
    let changed = checkpoint::changed_now("cp", &fx.work, &base);
    assert!(
        changed
            .iter()
            .any(|f| f.path == "agent.txt" && f.status == "A"),
        "the agent's file must be a change after the baseline, not part of it: {changed:?}"
    );
}

/// Esc during the wait stops the tool rather than hanging until git is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_tool_stops_waiting_for_the_baseline_and_runs_nothing() {
    let fx = fixture("cancel");
    let hold = checkpoint::hold_writes_for_test();
    fx.h.touch("cp").await;
    let cancel = CancellationToken::new();
    let a = agent(&fx, cancel.clone());
    let tool = tokio::spawn(async move {
        a.exec_inner(
            "write_file",
            &json!({"path": "agent.txt", "content": "x"}),
            None,
            "c",
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let out = tokio::time::timeout(Duration::from_secs(3), tool)
        .await
        .expect("cancelling ends the wait")
        .unwrap();
    assert!(out.unwrap_err().contains("Interrupted"));
    assert!(!Path::new(&fx.work).join("agent.txt").exists());
    drop(hold);
    fx.h.runtime("cp").checkpoints_settled().await;
}

/// Jobs run in the order they were queued, whatever each one costs: the
/// `turns` ledger is read last-wins, and the git write lock is not FIFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checkpoint_jobs_run_in_the_order_they_were_queued() {
    let rt = Runtime::default();
    let order = Arc::new(std::sync::Mutex::new(vec![]));
    for (i, ms) in [(1, 300u64), (2, 0), (3, 100)] {
        let order = order.clone();
        rt.queue_checkpoint(move || {
            std::thread::sleep(Duration::from_millis(ms));
            order.lock().unwrap().push(i);
        });
    }
    tokio::time::timeout(Duration::from_secs(10), rt.checkpoints_settled())
        .await
        .unwrap();
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
}

/// A finishing run can enqueue its capture after deletion began: draining a
/// single observed job is not enough without fencing out later registrations.
#[tokio::test]
async fn closing_checkpoints_drains_old_jobs_and_refuses_late_jobs() {
    let rt = Runtime::default();
    let order = Arc::new(std::sync::Mutex::new(vec![]));
    let first = order.clone();
    rt.queue_checkpoint(move || {
        std::thread::sleep(Duration::from_millis(50));
        first.lock().unwrap().push(1);
    });
    rt.close_checkpoints();
    let late = order.clone();
    rt.queue_checkpoint(move || late.lock().unwrap().push(2));
    rt.checkpoints_settled().await;
    assert_eq!(*order.lock().unwrap(), vec![1]);
}
