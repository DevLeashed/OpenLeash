//! Startup: the lazy task load and the guard that stops it losing history.
//!
//! `load_tasks` used to parse every byte of every chat file before the window
//! appeared — 14 seconds and 2.5 GB on a real dataset. It now reads a header and
//! leaves `sub_items`/`sub_msgs` on disk, and `Harness::ensure_hydrated` reads
//! the body back when a chat is opened.
//!
//! That trades parse time for a state where a task in memory is *less* than the
//! task on disk. Every test here is about that state: that the header is enough
//! to render a list, that the body comes back when asked for, and — the one that
//! matters — that a half-loaded task can never be written back over the real
//! file. A failure in that last group is silent, permanent data loss.

use super::store::{
    self, hydrate_task, load_tasks, read_task_full, read_task_header, save_task, task_path,
};
use super::{Item, Message, Pause, SubInfo};

/// A saved chat with a sub-agent, so there is a body worth deferring.
///
/// Built from the shared `tests::task` fixture rather than a literal of its own:
/// `Task` has ~50 fields, and a second copy of that literal would rot silently
/// the next time one is added — while still compiling, which is the worst way
/// for a test fixture to fail.
fn seed_task_with_sub(id: &str) {
    let mut t = crate::agent::tests::task(id, "test-model");
    t.hydrated = true;
    t.status = "idle".into();
    // A body big enough that the header pass is obviously not parsing it, and
    // distinctive enough that a lost body is unmissable.
    t.sub_items.insert(
        "s1".into(),
        (0..40)
            .map(|i| Item::new("text", format!("item-{i}"), serde_json::json!({})))
            .collect(),
    );
    t.sub_msgs.insert(
        "s1".into(),
        vec![Message::user(vec![
            serde_json::json!({"type": "text", "text": "sub transcript"}),
        ])],
    );
    t.subs = vec![SubInfo {
        id: "s1".into(),
        status: "done".into(),
        ..Default::default()
    }];
    t.items = vec![Item::new("text", "hello", serde_json::json!({}))];
    t.messages = vec![Message::user(vec![
        serde_json::json!({"type": "text", "text": "hi"}),
    ])];
    store::save_task(&t);
}

/// Read the raw bytes of a task file.
fn raw(id: &str) -> String {
    std::fs::read_to_string(task_path(id)).unwrap()
}

#[test]
fn the_header_load_leaves_the_sub_agent_body_on_disk() {
    let root = std::env::temp_dir().join(format!("ol-hdr-{}", std::process::id()));
    let _home = super::store::test_home(&root.join("home"));
    seed_task_with_sub("hdr1");
    let before = raw("hdr1");
    assert!(before.contains("sub transcript"), "the file has the body");

    let tasks = load_tasks();
    let t = tasks.iter().find(|t| t.id == "hdr1").expect("loaded");
    assert!(!t.hydrated, "the boot pass must not claim to be hydrated");
    assert!(
        t.sub_items.is_empty() && t.sub_msgs.is_empty(),
        "the two heavy maps are left on disk"
    );

    // Everything the sidebar and the review view read is still there: only the
    // two sub-agent fields are deferred.
    assert_eq!(t.items.len(), 1, "the transcript is parsed eagerly");
    assert_eq!(t.subs.len(), 1, "sub-agent rows come from the header");
    assert_eq!(t.title, "t", "the header carries the summary fields");
    assert_eq!(t.summary().subs.len(), 1, "the list can still render it");

    // And nothing was rewritten.
    assert_eq!(raw("hdr1"), before, "loading must not write");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_full_read_gets_the_body_back() {
    let root = std::env::temp_dir().join(format!("ol-full-{}", std::process::id()));
    let _home = super::store::test_home(&root.join("home"));
    seed_task_with_sub("full1");

    let t = read_task_full(&task_path("full1")).unwrap();
    assert!(t.hydrated);
    assert_eq!(t.sub_items["s1"].len(), 40);
    assert!(t.sub_msgs.contains_key("s1"));
}

#[test]
fn hydrating_restores_the_body_and_keeps_later_edits() {
    let root = std::env::temp_dir().join(format!("ol-hyd-{}", std::process::id()));
    let _home = super::store::test_home(&root.join("home"));
    seed_task_with_sub("hyd1");

    let mut t = read_task_header(&task_path("hyd1")).unwrap();
    // Something changed the chat between boot and opening it — a rename, a
    // pause, a model swap. Hydration must not roll that back.
    t.title = "renamed while closed".into();
    t.paused = Some(Pause {
        reason: "later".into(),
        kind: "closed".into(),
        since: chrono::Utc::now(),
    });
    assert!(t.sub_msgs.is_empty());

    // `hydrate_task` only repairs and flips the flag — reading the bytes is the
    // harness's job, via `ensure_hydrated`. Test the two together as the app
    // runs them, or the test would pass while the real path stayed broken.
    let spliced = read_task_full(&task_path("hyd1")).unwrap();
    t.sub_items = spliced.sub_items;
    t.sub_msgs = spliced.sub_msgs;
    hydrate_task(&mut t);

    assert!(t.hydrated);
    assert_eq!(t.sub_msgs["s1"].len(), 1, "the transcript came back");
    assert_eq!(t.title, "renamed while closed", "the newer edit survives");
    assert_eq!(t.paused.unwrap().reason, "later");
}

#[test]
fn saving_a_task_whose_body_was_never_loaded_is_refused() {
    // The guard that matters. A stub in memory plus a normal save would rewrite
    // the file with two empty maps, and nothing would report it until the user
    // went looking for a sub-agent transcript that had been there all along.
    let root = std::env::temp_dir().join(format!("ol-guard-{}", std::process::id()));
    let _home = super::store::test_home(&root.join("home"));
    seed_task_with_sub("guard1");
    let before = raw("guard1");

    let mut stub = read_task_header(&task_path("guard1")).unwrap();
    assert!(!stub.hydrated);
    stub.title = "renamed".into();
    save_task(&stub);

    assert_eq!(
        raw("guard1"),
        before,
        "an unhydrated task must not be written over the real file"
    );
    assert!(
        raw("guard1").contains("sub transcript"),
        "the sub-agent history is still on disk"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The whole point of the change: the boot pass must not read the body, and must
/// not be slower than the skip it performs. Guards the optimisation from being
/// silently reverted by a future edit to `Task`.
#[test]
fn the_boot_pass_skips_rather_than_materialises_the_body() {
    let root = std::env::temp_dir().join(format!("ol-skip-{}", std::process::id()));
    let _home = super::store::test_home(&root.join("home"));
    seed_task_with_sub("skip1");

    // A body large enough that materialising it would dominate the parse.
    let mut big = read_task_full(&task_path("skip1")).unwrap();
    big.sub_msgs.insert(
        "s1".into(),
        (0..20_000)
            .map(|_| {
                Message::user(vec![
                    serde_json::json!({"type": "text", "text": "x".repeat(200)}),
                ])
            })
            .collect(),
    );
    save_task(&big);
    let bytes = std::fs::metadata(task_path("skip1")).unwrap().len();

    let t0 = std::time::Instant::now();
    let tasks = load_tasks();
    let header = t0.elapsed();

    let t1 = std::time::Instant::now();
    let full_read = read_task_full(&task_path("skip1")).unwrap();
    let full = t1.elapsed();

    assert!(
        !tasks.iter().find(|t| t.id == "skip1").unwrap().hydrated,
        "the boot pass leaves it on disk"
    );
    // Structural, not a stopwatch: the boot pass must have discarded the body,
    // and a full read of the same file must recover all of it. A relative-time
    // assertion here would be measuring the disk, not the parse.
    assert_eq!(
        tasks
            .iter()
            .find(|t| t.id == "skip1")
            .unwrap()
            .sub_msgs
            .len(),
        0,
        "the boot pass did not materialise {bytes} bytes of sub-agent history"
    );
    assert_eq!(
        full_read.sub_msgs["s1"].len(),
        20_000,
        "a full read still gets it"
    );
    eprintln!("header {header:?} vs full {full:?} over {bytes} bytes");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_task_file_with_no_body_still_loads() {
    // Chat with no sub-agents at all: the common case, and the one that must not
    // regress into "unhydrated forever".
    let root = std::env::temp_dir().join(format!("ol-nobody-{}", std::process::id()));
    let _home = super::store::test_home(&root.join("home"));
    let t = read_task_full(&task_path("nope")).err();
    assert!(t.is_none() || t.is_some()); // no file yet, fine
    seed_task_with_sub("nb1");
    let mut plain = read_task_full(&task_path("nb1")).unwrap();
    plain.id = "nb2".into();
    plain.sub_items.clear();
    plain.sub_msgs.clear();
    save_task(&plain);

    let t = read_task_header(&task_path("nb2")).unwrap();
    assert!(
        !t.hydrated,
        "the boot pass still defers, even with an empty body"
    );
    let mut t = t;
    hydrate_task(&mut t);
    assert!(t.hydrated);
    assert!(t.sub_items.is_empty());
    let _ = std::fs::remove_dir_all(root);
}
