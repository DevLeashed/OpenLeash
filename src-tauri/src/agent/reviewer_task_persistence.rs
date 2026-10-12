//! Task saves must use the blocking pool without changing writer ordering.

use super::{store, tests, Harness, Item, Task, SAVE_DEBOUNCE};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
}

fn task() -> Task {
    let mut t = tests::task("persist", "dummy-model");
    t.items = (0..5)
        .map(|n| Item::new("tool", format!("image-{n}"), json!({"images": ["dummy"]})))
        .collect();
    t.sub_items.insert("sub".into(), t.items.clone());
    t
}

fn assert_saved(title: &str) {
    let t = store::read_task_full(&store::task_path("persist")).unwrap();
    assert_eq!(t.title, title);
    for items in [&t.items, &t.sub_items["sub"]] {
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].data["images"], json!([]));
        assert_eq!(items[0].data["images_dropped"], json!(1));
        assert_eq!(items[4].data["images"], json!(["dummy"]));
    }
}

/// Occupy the only blocking worker. Tests can then observe a queued save
/// deterministically, without timing a big serialization or slow filesystem.
fn occupy_pool() -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let job = tokio::task::spawn_blocking(move || {
        started_tx.send(()).unwrap();
        // Bounded even on assertion failure, so runtime shutdown cannot hang.
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    (release_tx, job)
}

#[test]
fn immediate_save_waits_for_blocking_pool_and_persists_pruned_task() {
    let root = std::env::temp_dir().join(format!("ol-save-now-{}", super::new_id()));
    let _home = store::test_home(&root);
    runtime().block_on(async {
        let (h, _) = tests::harness(store::Settings::default(), vec![task()]);
        h.save_task("persist").await;
        let (release, blocker) = occupy_pool();
        let save = h.save_task_now("persist");
        tokio::pin!(save);
        assert!(futures_util::poll!(&mut save).is_pending());
        assert!(!store::task_path("persist").exists());
        assert!(h.runtime("persist").task_save.try_lock().is_err());
        assert!(h.dirty.lock().unwrap().contains("persist"));
        // On the single async thread we are still responsive while the save
        // waits, and queued serialization does not hold Task.
        let t = h.task("persist").await.unwrap();
        assert!(t.try_lock().is_ok());
        release.send(()).unwrap();
        blocker.await.unwrap();
        save.await;
        assert_saved("t");
        assert!(h.dirty.lock().unwrap().is_empty());
        assert!(h.runtime("persist").task_save.try_lock().is_ok());
        assert_eq!(t.lock().await.items[0].data["images"], json!([]));
    });
    std::fs::remove_dir_all(root).unwrap();
}

async fn stop_saver(h: &Arc<Harness>) {
    let handle = h.saver.lock().unwrap().take().unwrap();
    handle.abort();
    let _ = handle.await;
}

#[test]
fn background_and_immediate_saves_are_ordered_and_keep_inflight_dirty_marks() {
    let root = std::env::temp_dir().join(format!("ol-save-bg-{}", super::new_id()));
    let _home = store::test_home(&root);
    runtime().block_on(async {
        let (h, _) = tests::harness(store::Settings::default(), vec![task()]);
        Harness::register(&h);
        let (release, blocker) = occupy_pool();
        h.save_task("persist").await;
        // The existing saver starts dormant; a subsequent request wakes it.
        // Exercise that scheduling as-is rather than changing it in this fix.
        h.save_task("persist").await;
        // Scheduling/debounce are intentionally unchanged. Wait until the
        // writer has taken the dirty mark and queued its blocking work.
        tokio::time::timeout(SAVE_DEBOUNCE + Duration::from_secs(5), async {
            loop {
                if !h.dirty.lock().unwrap().contains("persist")
                    && h.runtime("persist").task_save.try_lock().is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let t = h.task("persist").await.unwrap();
        t.lock().await.title = "newer".into();
        h.save_task("persist").await;
        assert!(h.dirty.lock().unwrap().contains("persist"));
        let now = h.save_task_now("persist");
        tokio::pin!(now);
        assert!(futures_util::poll!(&mut now).is_pending());
        assert!(!store::task_path("persist").exists());
        release.send(()).unwrap();
        blocker.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), now)
            .await
            .unwrap();
        // The background write must finish before save_task_now returns; it
        // cannot later overwrite the immediate save with an older snapshot.
        assert_saved("newer");
        assert!(h.dirty.lock().unwrap().is_empty());
        stop_saver(&h).await;
    });
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn background_save_alone_writes_pruned_task_and_drains_dirty() {
    let root = std::env::temp_dir().join(format!("ol-save-only-bg-{}", super::new_id()));
    let _home = store::test_home(&root);
    runtime().block_on(async {
        let (h, _) = tests::harness(store::Settings::default(), vec![task()]);
        Harness::register(&h);
        h.save_task("persist").await;
        h.save_task("persist").await;
        tokio::time::timeout(SAVE_DEBOUNCE + Duration::from_secs(5), async {
            loop {
                // The dirty mark is removed before writing, so also require
                // the ordering lock to be free before reading back the file.
                if !h.dirty.lock().unwrap().contains("persist")
                    && h.runtime("persist").task_save.try_lock().is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_saved("t");
        stop_saver(&h).await;
    });
    std::fs::remove_dir_all(root).unwrap();
}
