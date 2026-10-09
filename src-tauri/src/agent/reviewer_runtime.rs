//! What a task's live state holds once nobody is using it any more.
//!
//! Two shapes of leak, both the same mistake: something that grows per task or
//! per agent with nothing that ever removes it.
//!
//! 1. `Harness::runtimes` inserts on every `runtime()` call and had no
//!    `remove`, so a chat that was opened and then deleted kept its `Runtime`
//!    — and whatever the pasted-image buffer inside it was holding — for the
//!    life of the process.
//! 2. `Runtime::inbox_images` took base64 and never took any back out. Images
//!    pasted into a chat that is already busy queue up behind each other, and a
//!    chat that is paused, or whose agent never runs again, is never drained.
//!
//! The third case is the memory that actually moves the needle: `prune_task_images`
//! strips image blobs out of old timeline items, which are base64 inside a
//! `serde_json::Value` in a `Vec` that compaction never touches.

use super::runner::{prune_item_images, prune_task_images, KEEP_ITEM_IMAGES};
use super::tests::task;
use super::*;
use serde_json::json;

/// A 12 MB screenshot's worth of base64, as a stand-in. Building a real one
/// would make the test slow and prove no more: what is being tested is that the
/// bytes stop being held, not what is inside them.
fn blob() -> String {
    "data:image/png;base64,".to_string() + &"A".repeat(64)
}

fn shot_item() -> Item {
    Item::new("tool", "Screenshot", json!({"images": [blob()]}))
}

fn harness() -> Arc<Harness> {
    Arc::new(Harness {
        bus: Arc::new(|_: &str, _: Value| {}),
        tasks: tokio::sync::RwLock::new(Default::default()),
        runtimes: Default::default(),
        settings: tokio::sync::RwLock::new(Default::default()),
        bg: Default::default(),
        mcp: Default::default(),
        http: reqwest::Client::new(),
        pause_bell: Default::default(),
        accts: Default::default(),
        stats: Default::default(),
        keys: Default::default(),
        dirty: Default::default(),
        saver: Default::default(),
        settings_dirty: Default::default(),
        me: Default::default(),
    })
}

#[test]
fn a_deleted_chat_does_not_keep_its_runtime() {
    // Before the fix there was no `forget_runtime` at all: the map was insert-only,
    // so this asserted nothing and the entries lived until the process did.
    let h = harness();
    h.runtime("chat-a");
    h.runtime("chat-b");
    assert_eq!(h.runtimes.lock().unwrap().len(), 2);

    h.forget_runtime("chat-a");
    let left = h.runtimes.lock().unwrap();
    assert!(
        !left.contains_key("chat-a"),
        "a deleted chat's runtime must not outlive it"
    );
    assert!(
        left.contains_key("chat-b"),
        "another chat is not ours to drop"
    );
    assert_eq!(left.len(), 1);
}

#[test]
fn the_harnesss_own_runtime_is_never_dropped() {
    // The empty id is not a chat: it is the runtime the shared task writer keeps
    // its notify on. Dropping it because a purge passed "" would stop saves.
    let h = harness();
    h.runtime("");
    h.forget_runtime("");
    assert!(
        h.runtimes.lock().unwrap().contains_key(""),
        "the writer's runtime has no chat to be deleted with"
    );
}

#[tokio::test]
async fn pasted_images_do_not_pile_up_unbounded() {
    let h = harness();
    let one = vec![("image/png".to_string(), blob())];
    // Pasted one at a time into a chat that is already busy, which is exactly
    // how a user actually does it: the buffer only drains when the agent gets
    // another request, and this one never does.
    for _ in 0..INBOX_IMAGES * 5 {
        h.note_images("t", "main", one.clone()).await;
    }
    let rt = h.runtime("t");
    let q = rt.inbox_images.lock().await;
    let got = q.get("main").map(Vec::len).unwrap_or(0);
    assert_eq!(
        got, INBOX_IMAGES,
        "the buffer must stop at the cap, not track every paste"
    );
}

#[tokio::test]
async fn the_newest_pasted_images_are_the_ones_kept() {
    // Dropping the newest would be worse than dropping nothing: the pictures
    // the user just sent are the ones this turn is supposed to carry.
    let h = harness();
    for i in 0..INBOX_IMAGES + 3 {
        h.note_images("t", "main", vec![("image/png".into(), format!("img{i}"))])
            .await;
    }
    let rt = h.runtime("t");
    let q = rt.inbox_images.lock().await;
    let kept: Vec<&String> = q.get("main").unwrap().iter().map(|(_, d)| d).collect();
    assert_eq!(kept.len(), INBOX_IMAGES);
    assert_eq!(*kept.last().unwrap(), &format!("img{}", INBOX_IMAGES + 2));
    assert!(
        !kept.iter().any(|k| k.as_str() == "img0"),
        "the oldest went first"
    );
}

#[tokio::test]
async fn the_cap_is_per_agent() {
    // A swarm's agents each get their own buffer; a cap that counted them all
    // together would starve whichever agent pasted last.
    let h = harness();
    for _ in 0..INBOX_IMAGES * 3 {
        h.note_images("t", "main", vec![("image/png".into(), "a".into())])
            .await;
        h.note_images("t", "sub1", vec![("image/png".into(), "b".into())])
            .await;
    }
    let rt = h.runtime("t");
    let q = rt.inbox_images.lock().await;
    assert_eq!(q.get("main").unwrap().len(), INBOX_IMAGES);
    assert_eq!(
        q.get("sub1").unwrap().len(),
        INBOX_IMAGES,
        "one agent's pastes must not spend another's room"
    );
}

#[test]
fn old_timeline_items_stop_holding_their_screenshots() {
    let mut items: Vec<Item> = (0..KEEP_ITEM_IMAGES + 6).map(|_| shot_item()).collect();
    let held: usize = items
        .iter()
        .map(|i| i.data["images"].as_array().map(|a| a.len()).unwrap_or(0))
        .sum();
    assert!(held > 0);

    assert!(prune_item_images(&mut items), "there was something to drop");

    let left: usize = items
        .iter()
        .map(|i| i.data["images"].as_array().map(|a| a.len()).unwrap_or(0))
        .sum();
    assert_eq!(
        left, KEEP_ITEM_IMAGES,
        "only the newest few keep their blobs"
    );
    // And it says so, so a row does not read as if it never took a screenshot.
    let dropped: usize = items
        .iter()
        .map(|i| i.data["images_dropped"].as_u64().unwrap_or(0) as usize)
        .sum();
    assert_eq!(dropped, 6);
}

#[test]
fn pruning_a_timeline_leaves_the_newest_row_intact() {
    let mut items: Vec<Item> = (0..KEEP_ITEM_IMAGES + 1).map(|_| shot_item()).collect();
    prune_item_images(&mut items);
    let last = items.last().unwrap();
    assert!(
        last.data["images"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the newest screenshots are the ones being looked at"
    );
    assert!(
        last.data.get("images_dropped").is_none(),
        "a kept row is not marked as having lost anything"
    );
}

#[test]
fn pruning_is_idempotent_and_free_when_there_is_nothing_to_do() {
    let mut items: Vec<Item> = vec![shot_item(), shot_item()];
    assert!(
        !prune_item_images(&mut items),
        "under the cap nothing changes and it should say so"
    );
    let mut full: Vec<Item> = (0..KEEP_ITEM_IMAGES + 4).map(|_| shot_item()).collect();
    assert!(prune_item_images(&mut full));
    assert!(
        !prune_item_images(&mut full),
        "a second sweep has nothing left to do"
    );
}

#[test]
fn items_without_images_are_left_completely_alone() {
    // The common case, and the one that must not be disturbed: a text reply, a
    // diff, a notice. If this ever started rewriting them it would corrupt the
    // transcript of every chat.
    let mut items = vec![
        Item::new("text", "hello", json!({})),
        Item::new("tool", "Read", json!({"output": "x"})),
        Item::new("user", "fix it", json!({"queued": true})),
    ];
    let before = serde_json::to_string(&items).unwrap();
    assert!(!prune_item_images(&mut items));
    assert_eq!(
        serde_json::to_string(&items).unwrap(),
        before,
        "an item with no images must come out byte-identical"
    );
}

#[test]
fn a_sub_agents_own_timeline_is_swept_too() {
    // Sub-agent transcripts are a separate list that grows exactly the same way,
    // and a swarm keeps one per agent. Sweeping only the main list left the
    // larger half of a computer-use swarm's screenshots untouched.
    let mut t = task("t1", "gpt");
    t.items = (0..KEEP_ITEM_IMAGES + 2).map(|_| shot_item()).collect();
    t.sub_items = [(
        "s1".to_string(),
        (0..KEEP_ITEM_IMAGES + 5).map(|_| shot_item()).collect(),
    )]
    .into_iter()
    .collect();
    assert!(prune_task_images(&mut t));

    let imgs = |list: &[Item]| {
        list.iter()
            .map(|i| i.data["images"].as_array().map(|a| a.len()).unwrap_or(0))
            .sum::<usize>()
    };
    assert_eq!(imgs(&t.items), KEEP_ITEM_IMAGES);
    assert_eq!(imgs(&t.sub_items["s1"]), KEEP_ITEM_IMAGES);
}

#[test]
fn the_model_still_gets_its_own_images() {
    // The load-bearing check. `prune_task_images` touches `items` only; the
    // pixels the provider sees live in the tool_result content blocks in
    // `messages`, and pruning those is `prune_images`' separate job. If a future
    // edit reaches across into `messages` from here, the agent goes blind.
    let mut t = task("t1", "gpt");
    t.items = (0..KEEP_ITEM_IMAGES + 3).map(|_| shot_item()).collect();
    t.messages = vec![Message::user(vec![json!({
        "type": "tool_result",
        "tool_use_id": "s0",
        "content": [{"type": "image", "source": {"data": blob()}}],
    })])];
    prune_task_images(&mut t);
    assert_eq!(
        t.messages[0].content[0]["content"][0]["type"], "image",
        "the request the model receives must keep its image block"
    );
}
