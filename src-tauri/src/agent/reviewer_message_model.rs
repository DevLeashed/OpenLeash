//! Regression coverage: a message, or a new chat, with no model picked is refused
//! before anything is recorded, instead of being sent with an empty model id.

#![cfg(test)]

use super::reviewer_sandbox::Sandbox;
use super::store::Settings;
use super::tests::{harness, task};
use super::{require_chat_model, runner, NO_MODEL_PICKED};
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn a_chat_needs_a_model_to_take_a_message() {
    let t = task("nm-empty", "");
    assert_eq!(require_chat_model(&t), Err(NO_MODEL_PICKED.to_string()));

    let t = task("nm-set", "anthropic/claude-opus-5");
    assert_eq!(require_chat_model(&t), Ok(()));
}

#[test]
fn a_queued_model_swap_counts_as_the_models_the_message_runs_on() {
    // The chat itself has no model, but the swap queued for its next message
    // does, and that is what the message will run on.
    let mut t = task("nm-pending", "");
    t.pending.insert("model".into(), json!("anthropic/claude-opus-5"));
    assert_eq!(require_chat_model(&t), Ok(()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_to_a_chat_with_no_model_is_refused_before_anything_is_recorded() {
    let (h, _) = harness(Settings::default(), vec![task("nm-send", "")]);

    let err = runner::send(h.clone(), "nm-send".into(), "hello".into())
        .await
        .expect_err("a message with no model must not be sent");
    assert_eq!(err, NO_MODEL_PICKED);

    let t = h.task("nm-send").await.unwrap();
    let t = t.lock().await;
    assert!(t.items.is_empty(), "no timeline item is written");
    assert!(t.messages.is_empty(), "no history entry is written");
    assert!(
        !h.runtime("nm-send").running.load(Ordering::SeqCst),
        "no run starts"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attachment_message_to_a_chat_with_no_model_is_refused_before_staging() {
    let (h, _) = harness(Settings::default(), vec![task("nm-att", "")]);

    let err = runner::send_attachments(
        h.clone(),
        "nm-att".into(),
        "look at this".into(),
        vec![],
        vec![],
        vec![],
        false,
    )
    .await
    .expect_err("an attachment message with no model must not be sent");
    assert_eq!(err, NO_MODEL_PICKED);

    let t = h.task("nm-att").await.unwrap();
    let t = t.lock().await;
    assert!(t.items.is_empty(), "no timeline item is written");
    assert!(t.messages.is_empty(), "no history entry is written");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_chat_with_no_model_is_not_created() {
    let sandbox = Sandbox::new("no-model-create");
    let project = sandbox.root().to_string_lossy().to_string();
    let (h, _) = harness(Settings::default(), vec![]);
    let req: crate::NewTask = serde_json::from_value(json!({
        "prompt": "hello",
        "project": project,
        "model": "",
        "effort": 2,
        "plan": false,
        "worktree": false,
        "base_branch": "",
    }))
    .expect("a minimal new-chat request parses");

    let err = crate::create_task(h.clone(), req, None)
        .await
        .expect_err("a chat with no model must not be created");
    assert_eq!(err, NO_MODEL_PICKED);
    assert!(
        h.tasks.read().await.is_empty(),
        "no chat shell is inserted"
    );
}
