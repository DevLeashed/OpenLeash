//! Cross-chat tools: `list_chats` and `message_chat` (Settings → Chats).
//!
//! These are the tests that would fail if the feature were built the obvious
//! way, because the obvious implementation is the dangerous one. Delivering a
//! message with `runner::send` / `send_opts` is the natural thing to reach for —
//! it is how every other message into a chat is delivered — and it would hand an
//! agent the user's own authority in another conversation: an appended user
//! turn, a user bubble in that transcript, its `user_prompt_submit` hooks, and a
//! lifted pause. So that is what these pin, worst consequence first:
//!
//! 1. **A message is never the user.** Nothing lands in the receiving chat's
//!    `messages` as a user turn, and no `user` item is drawn; it goes to the
//!    agent inbox tagged `<kind:agent-msg>`, so `wakes_wait`/`late_steer` treat
//!    it as a peer's note rather than as something to drop everything for.
//! 2. **A pause is never lifted.** Messaging a paused chat — or one held by
//!    "Pause all" — is refused, and the receiving chat stays exactly as frozen.
//! 3. **Off means off.** With the setting down both tools refuse, say where the
//!    switch is, and describe nothing about the chats they declined to list.
//! 4. **Listing is a read.** It reads task *headers* (a chat's subagent history
//!    is most of a several-hundred-megabyte file) and takes no `Runtime` for a
//!    chat that has none.
//!
//! The dispatch-level halves — that the arms route here, that a subagent is
//! refused, and that the frozen prompt gains the section and the toggle is
//! announced — live with the runner's own internals in `runner.rs`.

use super::chats;
use super::store::Settings;
use super::tests::{harness, task};
use super::{Harness, SubInfo, Task};
use serde_json::{json, Value};
use std::sync::Arc;

fn settings_on() -> Settings {
    Settings {
        cross_chat: true,
        ..Settings::default()
    }
}

/// A chat the user has open and is not touching.
fn idle(id: &str, title: &str) -> Task {
    let mut t = task(id, "unused/model");
    t.title = title.into();
    t.status = "idle".into();
    t
}

#[test]
fn both_schemas_are_declared_for_the_main_agent_alone() {
    // Where the two tools live in the tool list. They are in the *core* literal
    // (so the frozen prefix stays byte-stable) but dropped for a subagent, which
    // is what the `__drop__` sentinel means — a subagent gets a fresh brief and
    // one job, so handing it a tool that could only answer "only the main agent
    // may" is a wasted turn. The dispatch refusals in `runner.rs` cover the case
    // where a model calls one anyway.
    let plugins = super::plugins::PluginsCfg::default();
    let main = super::tools::schemas(None, &plugins);
    for name in ["list_chats", "message_chat"] {
        assert_eq!(
            main.iter().filter(|t| t["name"] == name).count(),
            1,
            "{name} is declared exactly once for the main agent"
        );
    }
    for policy in ["all", "read_only", "no_shell"] {
        let sub = super::tools::schemas(Some(policy), &plugins);
        for name in ["list_chats", "message_chat"] {
            assert!(
                !sub.iter().any(|t| t["name"] == name),
                "{name} must not be in the {policy} subagent list"
            );
        }
    }

    // The permission layer agrees with that split: `list_chats` is a read, so
    // plan mode and the Disabled rung let it through — a plan is built out of
    // facts, and "what else is open on this" is one. `message_chat` starts work
    // in a conversation the user is not looking at, so it asks, and plan mode
    // refuses it outright.
    use super::permissions::{check, is_read_only_tool, Ctx, Decision};
    assert!(is_read_only_tool("list_chats"));
    assert!(!is_read_only_tool("message_chat"));
    let ctx = Ctx {
        perm: "disabled",
        plan: true,
        cwd: "D:/p",
        project: "D:/p",
        allow: &[],
    };
    assert!(matches!(
        check("list_chats", &json!({}), None, &ctx),
        Decision::Allow
    ));
    match check(
        "message_chat",
        &json!({"to": "Payroll", "message": "hi"}),
        None,
        &Ctx {
            plan: false,
            perm: "allowlist",
            ..ctx
        },
    ) {
        Decision::Ask { rule, reason, .. } => {
            assert_eq!(rule.as_deref(), Some("message_chat *"));
            assert!(
                reason.contains("not looking at"),
                "the card says why it is worth a look: {reason}"
            );
        }
        _ => panic!("a cross-chat message must ask"),
    }
    assert!(
        matches!(
            check(
                "message_chat",
                &json!({"to": "x", "message": "hi"}),
                None,
                &ctx
            ),
            Decision::Deny(_)
        ),
        "plan mode refuses it"
    );
    // A call with no chat named fails closed rather than drawing a card headed
    // "which chat?", which is not a question the user can usefully answer.
    assert!(matches!(
        check(
            "message_chat",
            &json!({"message": "hi"}),
            None,
            &Ctx {
                plan: false,
                perm: "turbo",
                ..ctx
            }
        ),
        Decision::Deny(_)
    ));

    // A deny rule wins over every rung, as deny rules do — and it is per tool:
    // denying `message_chat` leaves the read alone.
    let deny = vec![super::store::AllowRule {
        pattern: "!message_chat".into(),
        project: String::new(),
    }];
    let turbo = Ctx {
        perm: "turbo",
        plan: false,
        allow: &deny,
        ..ctx
    };
    assert!(matches!(
        check(
            "message_chat",
            &json!({"to": "x", "message": "hi"}),
            None,
            &turbo
        ),
        Decision::Deny(_)
    ));
    assert!(
        matches!(
            check("list_chats", &json!({}), None, &turbo),
            Decision::Allow
        ),
        "a rule against messaging does not take the read away"
    );
    let deny_read = vec![super::store::AllowRule {
        pattern: "!list_chats".into(),
        project: String::new(),
    }];
    assert!(
        matches!(
            check(
                "list_chats",
                &json!({}),
                None,
                &Ctx {
                    allow: &deny_read,
                    ..turbo
                }
            ),
            Decision::Deny(_)
        ),
        "and a rule against the read denies even on Full Access"
    );
}

async fn list(h: &Arc<Harness>, input: Value) -> Result<String, String> {
    chats::list(h, "me", &input).await
}

async fn send(h: &Arc<Harness>, input: Value) -> Result<String, String> {
    chats::send(h, "me", &input).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_tools_refuse_while_the_setting_is_off_and_list_leaks_nothing() {
    let mut other = idle("other", "Payroll import");
    other.step = "Running the migration".into();
    let (h, _) = harness(Settings::default(), vec![task("me", "m"), other]);

    let err = list(&h, json!({})).await.unwrap_err();
    assert!(err.contains("turned off"), "got: {err}");
    assert!(
        // The page it names has to be the page the switch is actually on, or the
        // model sends the user somewhere that does not have it.
        err.contains("Settings") && err.contains("Chats") && err.contains("Other chats"),
        "the refusal has to say where the switch is: {err}"
    );
    assert!(
        !err.contains("Payroll") && !err.contains("migration"),
        "a refusal must not describe the chats it declined to list: {err}"
    );

    let err = send(&h, json!({"to": "other", "message": "please stop"}))
        .await
        .unwrap_err();
    assert!(err.contains("turned off"), "got: {err}");

    // And nothing was delivered anywhere: no note, no timeline, no run.
    assert!(
        h.runtime_if("other").is_none(),
        "nothing may be sent while the switch is off"
    );
    let t = h.task("other").await.unwrap();
    let t = t.lock().await;
    assert!(t.items.is_empty(), "no notice was written: {:?}", t.items);
    assert!(t.messages.is_empty(), "no turn was started");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_chats_reports_live_state_of_the_other_chats() {
    let mut running = task("worker", "m");
    running.title = "Fix the flaky test".into();
    running.status = "running".into();
    running.step = "Running npm test".into();
    running.branch = "fix/flaky".into();
    running.subs = vec![
        SubInfo {
            id: "s1".into(),
            status: "running".into(),
            ..Default::default()
        },
        SubInfo {
            id: "s2".into(),
            status: "done".into(),
            ..Default::default()
        },
    ];
    let mut asking = idle("asking", "Deploy plan");
    asking.status = "waiting".into();
    asking.waiting_kind = Some("question".into());
    let mut frozen = idle("frozen", "Old work");
    frozen.paused = Some(super::Pause {
        reason: "manual".into(),
        kind: "manual".into(),
        since: chrono::Utc::now(),
    });
    let mut put_away = idle("archived", "Long finished");
    put_away.archived = true;

    let tasks = vec![task("me", "m"), running, asking, frozen, put_away];
    let (h, _) = harness(settings_on(), tasks);

    let out = list(&h, json!({})).await.unwrap();
    assert!(
        out.contains("Fix the flaky test"),
        "the title is there: {out}"
    );
    assert!(out.contains("running"), "its live status is there: {out}");
    assert!(
        out.contains("Running npm test"),
        "and what it is doing: {out}"
    );
    assert!(
        out.contains("1 subagent running"),
        "a running fan-out is worth knowing about: {out}"
    );
    assert!(
        out.contains("needs an answer from the user"),
        "a chat blocked on the user says what it wants, not just `waiting`: {out}"
    );
    assert!(out.contains("paused"), "a frozen chat reads paused: {out}");
    assert!(
        !out.contains("Long finished") && out.contains("1 archived"),
        "archived chats are counted, not listed, by default: {out}"
    );
    assert!(
        out.contains("← this chat"),
        "the model can tell which row is the chat it is in: {out}"
    );
    assert!(
        out.contains("worker"),
        "the ids it can address a message to are there: {out}"
    );

    // Search narrows to the same rows the app's own palette would find.
    let hits = list(&h, json!({"query": "flaky"})).await.unwrap();
    assert!(hits.contains("Fix the flaky test"), "got: {hits}");
    assert!(!hits.contains("Deploy plan"), "one word, one chat: {hits}");

    // A query matching nothing says so without dumping the list instead.
    let none = list(&h, json!({"query": "kubernetes"})).await.unwrap();
    assert!(none.contains("No chat matches"), "got: {none}");
    assert!(
        !none.contains("Deploy plan"),
        "no accidental fallback: {none}"
    );

    // Archived chats can be asked for explicitly, and then say that they are.
    let all = list(
        &h,
        json!({"query": "Long finished", "include_archived": true}),
    )
    .await
    .unwrap();
    assert!(all.contains("Long finished"), "got: {all}");
    assert!(all.contains("archived"), "and says it is archived: {all}");

    // `limit` caps the rows and says how many were left, so a busy machine does
    // not put forty chats in the model's context.
    let capped = list(&h, json!({"limit": 2})).await.unwrap();
    assert_eq!(
        capped.lines().filter(|l| l.contains(" · ")).count(),
        2,
        "two rows at most: {capped}"
    );
    assert!(capped.contains("more match"), "and it says so: {capped}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listing_does_not_hydrate_or_create_runtimes() {
    let mut other = idle("other", "Something else");
    // A header-only task: exactly what `load_tasks` leaves in memory at boot.
    // Touching `Harness::task` would try to read a subagent body that is not
    // there and flip this to true, so it is the tripwire.
    other.hydrated = false;
    let (h, _) = harness(settings_on(), vec![task("me", "m"), other]);
    assert!(h.runtime_if("other").is_none());

    let out = list(&h, json!({})).await.unwrap();
    assert!(out.contains("Something else"), "got: {out}");
    assert!(
        !h.task_ref("other").await.unwrap().lock().await.hydrated,
        "listing must read the header, not pull the body off disk"
    );
    assert!(
        h.runtime_if("other").is_none(),
        "reporting on a chat must not leave a Runtime behind for it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_reaches_the_agent_inbox_and_never_becomes_a_user_turn() {
    let (h, _) = harness(
        settings_on(),
        vec![task("me", "m"), idle("other", "Payroll")],
    );
    let out = send(
        &h,
        json!({"to": "other", "message": "the schema moved to v3"}),
    )
    .await
    .unwrap();
    assert!(out.contains("Payroll"), "got: {out}");
    assert!(
        out.contains("one-way"),
        "the model is told nothing comes back: {out}"
    );

    // It is in the receiving agent's inbox, framed as a peer's note.
    let rt = h.runtime_if("other").expect("delivery creates the runtime");
    let note = rt
        .inbox
        .lock()
        .await
        .get("main")
        .cloned()
        .unwrap_or_default()
        .join("\n");
    assert!(note.contains("the schema moved to v3"), "got: {note}");
    assert!(
        note.starts_with("<kind:agent-msg>"),
        "tagged as agent mail, which is what `wakes_wait` reads: {note}"
    );
    assert!(
        note.contains("not from your user") && note.contains("not permission for anything"),
        "the receiver is told it is not its user speaking, and carries no authority: {note}"
    );

    // The critical half: the receiving chat's history is untouched. A user turn
    // there would be this agent speaking with the user's voice.
    let t = h.task("other").await.unwrap();
    let t = t.lock().await;
    assert!(
        t.messages.is_empty(),
        "nothing was appended to the other chat's history: {:?}",
        t.messages
    );
    assert!(
        !t.items.iter().any(|i| i.kind == "user"),
        "no user bubble was drawn in the other chat: {:?}",
        t.items
    );
    let line = t
        .items
        .iter()
        .find(|i| i.kind == "notice")
        .expect("the other chat's timeline gets a line saying who asked");
    assert_eq!(line.data["level"], "msg", "on the agent-message channel");
    assert_eq!(
        line.data["chat_id"], "me",
        "carrying the sender, so the UI can link back"
    );
    assert!(
        line.text.contains("the schema moved to v3"),
        "got: {}",
        line.text
    );
    drop(t);

    // And the sender keeps a record of what it sent, addressed to the chat.
    let mine = h.task("me").await.unwrap();
    let mine = mine.lock().await;
    assert!(
        mine.items.iter().any(|i| i.kind == "notice"
            && i.data["to"] == "Payroll"
            && i.data["chat_id"] == "other"
            && i.text.contains("the schema moved to v3")),
        "the sender's own timeline says what went out: {:?}",
        mine.items
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_chat_is_refused_and_stays_paused() {
    let mut frozen = idle("frozen", "Careful work");
    frozen.paused = Some(super::Pause {
        reason: "the user paused this".into(),
        kind: "manual".into(),
        since: chrono::Utc::now(),
    });
    let (h, _) = harness(settings_on(), vec![task("me", "m"), frozen]);

    let err = send(&h, json!({"to": "frozen", "message": "carry on please"}))
        .await
        .unwrap_err();
    assert!(err.contains("paused"), "got: {err}");
    assert!(
        err.contains("only the user resumes"),
        "the refusal explains why, so the model tells the user instead: {err}"
    );

    let t = h.task("frozen").await.unwrap();
    let t = t.lock().await;
    assert!(t.paused.is_some(), "the pause is still there");
    assert!(t.messages.is_empty(), "nothing was written into it");
    assert!(
        t.items.is_empty(),
        "and nothing is drawn on its timeline: {:?}",
        t.items
    );
    assert!(
        !t.unpaused,
        "and it never opted itself out of the global pause"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_global_pause_holds_a_chat_against_a_message_too() {
    // "Pause all" is the user freezing everything at once. A chat it holds
    // carries no `paused` of its own — the flag is global — so a check that only
    // looked at the task would walk straight through the user's freeze.
    let mut working = task("worker", "m");
    working.title = "Half-done refactor".into();
    working.status = "running".into();
    let settings = Settings {
        paused_all: true,
        ..settings_on()
    };
    let (h, _) = harness(settings, vec![task("me", "m"), working]);

    let err = send(&h, json!({"to": "worker", "message": "keep going"}))
        .await
        .unwrap_err();
    assert!(err.contains("paused"), "got: {err}");
    // The list says so too, or the model would keep trying a chat that refuses.
    let out = list(&h, json!({})).await.unwrap();
    assert!(
        out.contains("Half-done refactor · paused"),
        "the frozen chat reads paused in the list: {out}"
    );

    // With the global pause lifted it goes through: this is a check on the
    // pause, not a blanket refusal.
    h.settings.write().await.paused_all = false;
    assert!(send(&h, json!({"to": "worker", "message": "keep going"}))
        .await
        .is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parked_chat_is_told_without_being_woken() {
    // A chat parked by `wait_for_event` is deliberately waiting on a trigger. It
    // gets the note, but no run: waking it would start a turn that immediately
    // parks again, and the note is read when the trigger fires.
    let (h, _) = harness(
        settings_on(),
        vec![task("me", "m"), idle("parked", "Waiting on CI")],
    );
    h.runtime("parked").wake_state.lock().unwrap().parked = true;

    let out = send(&h, json!({"to": "parked", "message": "the branch moved"}))
        .await
        .unwrap();
    assert!(
        out.contains("parked on a trigger"),
        "the sender is told what will happen to it: {out}"
    );
    let rt = h.runtime_if("parked").unwrap();
    assert!(
        !rt.running.load(std::sync::atomic::Ordering::SeqCst),
        "a parked chat is not woken by a message from another chat"
    );
    assert_eq!(
        rt.inbox.lock().await.get("main").map(Vec::len),
        Some(1),
        "the note is waiting for it"
    );
    // The list says it is parked rather than calling it idle.
    let out = list(&h, json!({})).await.unwrap();
    assert!(out.contains("waiting on a trigger"), "got: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_archived_chat_is_refused_by_title_and_a_missing_one_says_so() {
    let mut away = idle("away", "Put away");
    away.archived = true;
    let (h, _) = harness(
        settings_on(),
        vec![task("me", "m"), away, idle("real", "Open one")],
    );

    // Archived chats are hidden from the list, so this is how a model would name
    // one it saw earlier — and it must be refused, not silently delivered.
    let err = send(&h, json!({"to": "Put away", "message": "hi"}))
        .await
        .unwrap_err();
    assert!(err.contains("archived"), "got: {err}");

    let err = send(
        &h,
        json!({"to": "a chat that does not exist", "message": "hi"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("No chat is called"), "got: {err}");
    assert!(
        err.contains("list_chats"),
        "it is pointed at the tool that would find the right name: {err}"
    );

    // Messaging the chat you are in is a no-op, not a loop.
    let err = send(&h, json!({"to": "me", "message": "hi"}))
        .await
        .unwrap_err();
    assert!(err.contains("this chat"), "got: {err}");

    // A title is enough when it is unique, which is the case a person means.
    assert!(send(&h, json!({"to": "Open one", "message": "hi"}))
        .await
        .is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ambiguous_title_is_refused_rather_than_guessed() {
    let (h, _) = harness(
        settings_on(),
        vec![
            task("me", "m"),
            idle("a1", "Refactor the parser"),
            idle("a2", "Refactor the parser"),
        ],
    );
    let err = send(&h, json!({"to": "Refactor the parser", "message": "hi"}))
        .await
        .unwrap_err();
    assert!(
        err.contains("a1") && err.contains("a2"),
        "both candidates are named: {err}"
    );
    assert!(
        h.runtime_if("a1").is_none() && h.runtime_if("a2").is_none(),
        "guessing wrong is worse than not sending, so neither was written to"
    );
    // By id it works — which is why the list prints ids first.
    assert!(send(&h, json!({"to": "a1", "message": "hi"})).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_is_capped_and_must_say_what_it_wants() {
    let (h, _) = harness(
        settings_on(),
        vec![task("me", "m"), idle("other", "Elsewhere")],
    );

    let err = send(&h, json!({"to": "Elsewhere", "message": "  "}))
        .await
        .unwrap_err();
    assert!(err.contains("`message` is required"), "got: {err}");

    let err = send(&h, json!({"to": "  ", "message": "hi"}))
        .await
        .unwrap_err();
    assert!(err.contains("`to` is required"), "got: {err}");

    let huge = "x".repeat(chats::MAX_MESSAGE + 1);
    let err = send(&h, json!({"to": "Elsewhere", "message": huge}))
        .await
        .unwrap_err();
    assert!(err.contains("under"), "got: {err}");
    assert!(
        h.runtime_if("other").is_none(),
        "an oversized message never reaches the other chat"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_chat_is_woken_so_the_message_is_actually_read() {
    // The other half of `a_parked_chat_is_told_without_being_woken`: a chat that
    // is merely idle has nobody to read a note, so it gets a run as well — the
    // same hand-off a finished background subagent's report gets.
    let (h, _) = harness(
        settings_on(),
        vec![task("me", "m"), idle("other", "Elsewhere")],
    );
    let out = send(&h, json!({"to": "Elsewhere", "message": "look at this"}))
        .await
        .unwrap();
    assert!(out.contains("woke it"), "got: {out}");
    assert!(
        h.runtime_if("other")
            .is_some_and(|rt| rt.running.load(std::sync::atomic::Ordering::SeqCst)),
        "an idle chat is started so the note is read"
    );
}
