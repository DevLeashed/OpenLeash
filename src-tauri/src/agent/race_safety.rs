//! Live lifecycle state for race checks and judges, independent of persisted races.
//!
//! Status summaries are not liveness: main-run teardown marks even detached
//! subagents stopped, and background shells deliberately do not count as busy.

use super::Harness;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Whether an attempt can still change its workspace, or has not started yet.
/// Inspect without allocating a runtime for a historical/deleted chat.
pub async fn attempt_active(h: &Harness, id: &str) -> bool {
    let Ok(task) = h.task(id).await else {
        return false;
    };
    let task = task.lock().await;
    if task.busy > 0
        || task.paused.is_some()
        || matches!(task.status.as_str(), "running" | "waiting")
        || task.messages.is_empty()
    {
        return true;
    }
    drop(task);

    let runtime_active = h.runtime_if(id).is_some_and(|runtime| {
        runtime
            .startup_pending
            .load(std::sync::atomic::Ordering::SeqCst)
            || runtime.running.load(std::sync::atomic::Ordering::SeqCst)
            || runtime.fg.load(std::sync::atomic::Ordering::SeqCst) > 0
            || !runtime
                .sub_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
    });
    runtime_active || h.bg.list(id).iter().any(|job| job.running)
}

struct Run {
    tasks: Vec<String>,
    cancel: CancellationToken,
    finished: CancellationToken,
}

static RUNS: OnceLock<Mutex<HashMap<String, Arc<Run>>>> = OnceLock::new();

fn runs() -> &'static Mutex<HashMap<String, Arc<Run>>> {
    RUNS.get_or_init(Default::default)
}

/// Owns one pass, including cancellation cleanup. Keep this guard alive until
/// checks and the judge have actually unwound, not only until Stop is pressed.
pub struct Registration {
    id: String,
    run: Arc<Run>,
}

/// Reserve a race before spawning its checks/judge. Cancellation does not make
/// the reservation free: a replacement must not race a process still draining.
pub fn start(id: &str, task_ids: &[String]) -> Result<Registration, String> {
    let mut all = runs().lock().unwrap_or_else(|e| e.into_inner());
    if all.contains_key(id) {
        return Err("This race already has an active checks/judge pass.".into());
    }
    let run = Arc::new(Run {
        tasks: task_ids.to_vec(),
        cancel: CancellationToken::new(),
        finished: CancellationToken::new(),
    });
    all.insert(id.to_string(), run.clone());
    Ok(Registration {
        id: id.to_string(),
        run,
    })
}

/// Includes canceled passes that have not finished cleanup. Also use this to
/// refuse merge while a pass is inspecting the worktrees.
pub fn active(id: &str) -> bool {
    runs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(id)
}

/// Forgetting a race must cancel its provider request and check commands first.
pub fn cancel(id: &str) {
    if let Some(run) = runs().lock().unwrap_or_else(|e| e.into_inner()).get(id) {
        run.cancel.cancel();
    }
}

/// Stop/delete entrypoints call this before touching the participating chat.
/// Membership is captured by `start`, not loaded from possibly deleted records.
#[allow(dead_code)] // wired into the task stop/delete IPC path in a later patch
pub fn cancel_task(task_id: &str) {
    for run in runs().lock().unwrap_or_else(|e| e.into_inner()).values() {
        if run.tasks.iter().any(|id| id == task_id) {
            run.cancel.cancel();
        }
    }
}

impl Registration {
    pub fn token(&self) -> CancellationToken {
        self.run.cancel.clone()
    }

    /// Fallback for non-IPC stop paths and deletion. Snapshot each task's
    /// background parent: `stop_tokens` cancels and replaces it even when the
    /// main model loop has already completed. A status == stopped test cannot
    /// distinguish that new Stop from a stopped attempt being judged normally.
    /// Already-deleted attempts are permitted; only new deletions cancel a pass.
    pub async fn watch_tasks(&self, h: &Arc<Harness>) {
        let mut watched = vec![];
        for id in &self.run.tasks {
            if h.task(id).await.is_ok() {
                let rt = h.runtime(id);
                let stopped = rt.bg_cancel.lock().unwrap().clone();
                watched.push((id.clone(), stopped));
            }
        }
        let h = h.clone();
        let run = self.run.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = run.finished.cancelled() => return,
                    _ = run.cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
                for (id, stopped) in &watched {
                    if stopped.is_cancelled()
                        || h.task(id).await.is_err()
                        || attempt_active(&h, id).await
                    {
                        run.cancel.cancel();
                        return;
                    }
                }
            }
        });
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Dropping an aborted owner also stops any children holding its token.
        self.run.cancel.cancel();
        self.run.finished.cancel();
        let mut all = runs().lock().unwrap_or_else(|e| e.into_inner());
        if all.get(&self.id).is_some_and(|r| Arc::ptr_eq(r, &self.run)) {
            all.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::store::Settings;
    use crate::agent::tests::{harness, task};
    use crate::agent::{Message, Pause, SubInfo};
    use std::sync::atomic::Ordering;

    fn ready(id: &str) -> crate::agent::Task {
        let mut t = task(id, "dummy/model");
        t.status = "done".into();
        t.messages.push(Message::user_text("temporary fixture"));
        t
    }

    #[tokio::test]
    async fn live_runtime_outweighs_finished_summary() {
        let (h, _) = harness(Settings::default(), vec![ready("attempt")]);
        let rt = h.runtime("attempt");
        assert!(!attempt_active(&h, "attempt").await);
        rt.startup_pending.store(true, Ordering::SeqCst);
        assert!(
            attempt_active(&h, "attempt").await,
            "setup is still running"
        );
        rt.startup_pending.store(false, Ordering::SeqCst);
        rt.running.store(true, Ordering::SeqCst);
        assert!(
            attempt_active(&h, "attempt").await,
            "main loop is still live"
        );
        rt.running.store(false, Ordering::SeqCst);
        rt.fg.store(1, Ordering::SeqCst);
        assert!(attempt_active(&h, "attempt").await, "commands are draining");
        rt.fg.store(0, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        cancel.cancel();
        rt.sub_cancel.lock().unwrap().insert("child".into(), cancel);
        assert!(
            attempt_active(&h, "attempt").await,
            "canceled child has not unregistered"
        );
        rt.sub_cancel.lock().unwrap().clear();
        assert!(!attempt_active(&h, "attempt").await);
    }

    #[tokio::test]
    async fn pending_and_paused_tasks_are_not_finished() {
        let mut new = ready("new");
        new.status = "idle".into();
        new.messages.clear();
        let mut paused = ready("paused");
        paused.paused = Some(Pause::default());
        let child = ready("child");
        let (h, _) = harness(Settings::default(), vec![new, paused, child, ready("old")]);
        for id in ["new", "paused"] {
            assert!(attempt_active(&h, id).await, "{id} is not finished");
        }
        assert!(!attempt_active(&h, "old").await);
        assert!(
            !attempt_active(&h, "child").await,
            "stale sub status is not liveness"
        );
        assert!(!attempt_active(&h, "deleted").await);
        assert!(
            h.runtimes.lock().unwrap().is_empty(),
            "reads do not allocate runtimes"
        );
    }

    #[tokio::test]
    async fn stopped_summary_does_not_hide_live_background_subagent() {
        let mut t = ready("attempt");
        t.status = "stopped".into();
        t.subs.push(SubInfo {
            id: "detached".into(),
            status: "stopped".into(),
            background: true,
            ..Default::default()
        });
        let (h, _) = harness(Settings::default(), vec![t]);
        h.runtime("attempt")
            .sub_cancel
            .lock()
            .unwrap()
            .insert("detached".into(), CancellationToken::new());
        assert!(attempt_active(&h, "attempt").await);
    }

    #[tokio::test]
    async fn live_background_shell_is_active_until_it_exits() {
        let dir =
            std::env::temp_dir().join(format!("openleash-race-safety-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (h, _) = harness(Settings::default(), vec![ready("attempt"), ready("other")]);
        let command = "sleep 30";
        let id =
            h.bg.spawn("attempt", "", command, dir.to_str().unwrap())
                .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(h.bg.list("attempt").iter().any(|job| job.running));
        let live = attempt_active(&h, "attempt").await;
        assert!(!attempt_active(&h, "other").await);
        assert!(live, "background shell does not contribute to Task.busy");
        h.bg.kill(&id);
        tokio::time::timeout(
            Duration::from_secs(10),
            h.bg.get(&id).unwrap().done.cancelled(),
        )
        .await
        .unwrap();
        assert!(!attempt_active(&h, "attempt").await);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancellation_keeps_reservation_until_cleanup_and_is_scoped() {
        let race = crate::agent::new_id();
        let other = crate::agent::new_id();
        let task = crate::agent::new_id();
        let first = start(&race, &[task.clone()]).unwrap();
        let second = start(&other, &["unrelated".into()]).unwrap();
        let token = first.token();
        cancel_task(&task);
        assert!(token.is_cancelled());
        assert!(!second.token().is_cancelled());
        assert!(active(&race));
        assert!(
            start(&race, &[]).is_err(),
            "canceled does not mean cleaned up"
        );
        drop(first);
        assert!(!active(&race));
        let retry = start(&race, &[]).unwrap();
        assert!(!retry.token().is_cancelled());
        cancel(&race);
        assert!(retry.token().is_cancelled());
        assert!(!second.token().is_cancelled());
    }

    #[test]
    fn dropping_owner_cancels_children_and_unregisters() {
        let race = crate::agent::new_id();
        let pass = start(&race, &[]).unwrap();
        let child = pass.token().child_token();
        drop(pass);
        assert!(child.is_cancelled());
        assert!(!active(&race));
    }

    #[tokio::test]
    async fn monitor_observes_stop_even_after_main_finished() {
        let (h, _) = harness(Settings::default(), vec![ready("attempt")]);
        let pass = start(&crate::agent::new_id(), &["attempt".into()]).unwrap();
        pass.watch_tasks(&h).await;
        crate::agent::runner::stop_tokens(&h, "attempt");
        tokio::time::timeout(Duration::from_secs(2), pass.token().cancelled())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn monitor_observes_new_deletion_but_allows_predeleted_attempt() {
        let (h, _) = harness(Settings::default(), vec![ready("attempt")]);
        let pass = start(
            &crate::agent::new_id(),
            &["attempt".into(), "already-gone".into()],
        )
        .unwrap();
        pass.watch_tasks(&h).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!pass.token().is_cancelled());
        h.tasks.write().await.remove("attempt");
        tokio::time::timeout(Duration::from_secs(2), pass.token().cancelled())
            .await
            .unwrap();
    }
}
