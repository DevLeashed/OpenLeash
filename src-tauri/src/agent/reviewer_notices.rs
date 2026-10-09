//! Informational notices are durable UI data, never questions or permissions.
use super::tests::{harness, task};
use super::{plugins, store, toolindex, tools, Item};
use serde_json::{json, Value};

#[test]
fn notify_user_is_core_and_main_agent_only() {
    let plugins = plugins::PluginsCfg::default();
    let schemas = tools::schemas(None, &plugins);
    let notice = schemas.iter().find(|t| t["name"] == "notify_user").unwrap();
    assert_eq!(
        notice["input_schema"]["required"],
        json!(["title", "message"])
    );
    assert_eq!(
        notice["input_schema"]["properties"]["level"]["default"],
        "info"
    );
    let catalog = toolindex::Catalog::build(&schemas, &[], true);
    assert!(catalog.always().iter().any(|t| t["name"] == "notify_user"));
    for policy in ["all", "read_only", "no_shell"] {
        assert!(!tools::schemas(Some(policy), &plugins)
            .iter()
            .any(|t| t["name"] == "notify_user"));
    }
}

#[test]
fn notice_dismissal_reports_failure_and_preserves_pending_card() {
    let home =
        std::env::temp_dir().join(format!("openleash-dismiss-fail-{}", uuid::Uuid::new_v4()));
    // Keep the process-wide home guard outside the future while serializing the entire test.
    let _home = store::test_home(&home);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let notice = Item::new(
                "user_notice",
                "Message",
                json!({"title":"Heading", "level":"info"}),
            );
            let id = notice.id.clone();
            let original_data = notice.data.clone();
            let mut t = task("notices", "unused/model");
            t.items.push(notice);
            store::save_task(&t);
            let path = store::task_path("notices");
            let saved_bytes = std::fs::read(&path).unwrap();
            let backup = path.with_extension("backup");
            std::fs::rename(&path, &backup).unwrap();
            // Atomic replacement must fail when the destination is a directory.
            std::fs::create_dir(&path).unwrap();
            let (h, events) = harness(store::Settings::default(), vec![t]);
            h.save_task("notices").await;
            let result = h.dismiss_user_notice("notices", &id).await;
            assert!(
                result.is_err(),
                "failed save must not dismiss successfully: {result:?}"
            );
            assert_eq!(h.user_notices().await["notices"][0].data, original_data);
            assert!(events.lock().unwrap().is_empty());
            assert!(h.dirty.lock().unwrap().contains("notices"));
            assert_eq!(std::fs::read(&backup).unwrap(), saved_bytes);
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                2,
                "failed atomic write cleans its temp file"
            );
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&backup, &path).unwrap();
            h.dismiss_user_notice("notices", &id).await.unwrap();
            assert!(h.user_notices().await.is_empty());
            assert_eq!(
                store::read_task_full(&path).unwrap().items[0].data["dismissed"],
                true
            );
            std::fs::remove_dir_all(home).unwrap();
        });
}

#[test]
fn notice_transactions_and_immediate_saves_wait_for_snapshot_write() {
    let home =
        std::env::temp_dir().join(format!("openleash-notice-order-{}", uuid::Uuid::new_v4()));
    // Keep the process-wide home guard outside the future while serializing the entire test.
    let _home = store::test_home(&home);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let notice = Item::new(
                "user_notice",
                "Message",
                json!({"title":"Heading", "dismissed":false}),
            );
            let id = notice.id.clone();
            let mut t = task("notices", "unused/model");
            t.items.push(notice);
            let (h, _) = harness(store::Settings::default(), vec![t.clone()]);
            let rt = h.runtime("notices");
            // Model a debounced snapshot already serialized but not yet written. All
            // newer writes must wait for its replacement, not just for its task lock.
            let snapshot_write = rt.task_save.lock().await;
            let save = h.save_task_now("notices");
            tokio::pin!(save);
            assert!(futures_util::poll!(&mut save).is_pending());
            let dismiss = h.dismiss_user_notice("notices", &id);
            tokio::pin!(dismiss);
            assert!(futures_util::poll!(&mut dismiss).is_pending());
            let deliver = h.deliver_user_notice(
                "notices",
                Item::new("user_notice", "New", json!({"title":"New"})),
            );
            tokio::pin!(deliver);
            assert!(futures_util::poll!(&mut deliver).is_pending());
            store::save_task(&t);
            drop(snapshot_write);
            save.await;
            dismiss.await.unwrap();
            deliver.await.unwrap();
            let saved = store::read_task_full(&store::task_path("notices")).unwrap();
            assert_eq!(saved.items.len(), 2);
            assert_eq!(saved.items[0].data["dismissed"], true);
            std::fs::remove_dir_all(home).unwrap();
        });
}

#[test]
fn notice_save_refuses_unhydrated_task_without_overwriting_history() {
    let home =
        std::env::temp_dir().join(format!("openleash-notice-header-{}", uuid::Uuid::new_v4()));
    let _home = store::test_home(&home);
    let t = task("notices", "unused/model");
    store::try_save_task(&t).unwrap();
    let path = store::task_path("notices");
    let original = std::fs::read(&path).unwrap();
    let mut header = store::read_task_header(&path).unwrap();
    header
        .items
        .push(Item::new("user_notice", "Must not write", json!({})));
    assert!(store::try_save_task(&header).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn notice_dismissal_is_scoped_persistent_and_never_an_answer() {
    let home =
        std::env::temp_dir().join(format!("openleash-notice-dismiss-{}", uuid::Uuid::new_v4()));
    // Keep the process-wide home guard outside the future while serializing the entire test.
    let _home = store::test_home(&home);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let notice = Item::new(
                "user_notice",
                "Message",
                json!({"title":"Heading", "level":"info"}),
            );
            let id = notice.id.clone();
            let question = Item::new("question", "Question", json!({"questions":[]}));
            let question_id = question.id.clone();
            let mut t = task("notices", "unused/model");
            t.status = "waiting".into();
            t.waiting_kind = Some("question".into());
            t.items = vec![notice, question];
            store::save_task(&t);
            // Header-only boot must restore cards without reading heavy transcripts.
            let header = store::read_task_header(&store::task_path("notices")).unwrap();
            assert!(!header.hydrated);
            let mut archived = task("archived", "unused/model");
            archived.archived = true;
            archived.items = t.items.clone();
            let mut hidden = archived.clone();
            hidden.id = "hidden".into();
            hidden.archived = false;
            hidden.hidden = true;
            let (h, events) = harness(
                store::Settings::default(),
                vec![header, archived, hidden, task("other", "unused/model")],
            );
            let pending = h.user_notices().await;
            assert_eq!(pending.len(), 1);
            assert_eq!(pending["notices"][0].id, id);
            assert!(!h.tasks.read().await["notices"].lock().await.hydrated);
            let rt = h.runtime("notices");
            let (tx, mut rx) = tokio::sync::oneshot::channel::<Value>();
            rt.pending.lock().await.insert(question_id.clone(), tx);

            assert!(h.dismiss_user_notice("other", &id).await.is_err());
            assert!(h
                .dismiss_user_notice("notices", &question_id)
                .await
                .is_err());
            assert!(h.dismiss_user_notice("missing", &id).await.is_err());
            h.dismiss_user_notice("notices", &id).await.unwrap();
            h.dismiss_user_notice("notices", &id).await.unwrap(); // double-click is harmless
            assert!(h.user_notices().await.is_empty());
            let saved = store::read_task_full(&store::task_path("notices")).unwrap();
            assert_eq!(saved.items[0].data["dismissed"], true);
            assert_eq!(saved.status, "waiting");
            assert_eq!(saved.waiting_kind.as_deref(), Some("question"));
            assert!(saved.messages.is_empty());
            assert!(saved.items[1].data.get("answers").is_none());
            assert_eq!(rt.pending.lock().await.len(), 1);
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert!(rt.inbox.lock().await.is_empty());
            assert!(rt.queue.lock().await.is_empty());
            assert!(rt.cancel.lock().unwrap().is_none());
            {
                let evs = events.lock().unwrap();
                assert!(!evs.iter().any(|(name, _)| name == "ol://attention"));
                assert!(evs.iter().any(|(name, data)| {
                    name == "ol://event"
                        && data["kind"] == "item"
                        && data["payload"]["id"] == id
                        && data["payload"]["data"]["dismissed"] == true
                }));
            }

            // Tasks from older versions need no new fields and default to no notices.
            let (old, _) = harness(
                store::Settings::default(),
                vec![task("old", "unused/model")],
            );
            assert!(old.user_notices().await.is_empty());
            let _ = std::fs::remove_dir_all(home);
        });
}
