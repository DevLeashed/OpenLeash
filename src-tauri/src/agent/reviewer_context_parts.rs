//! Context-window attribution: the category split the context ring draws beside
//! the total.
//!
//! The ring's number is provider truth. The split under it is an estimate by
//! construction — a provider reports only a prompt *total*, and the serialized
//! request is built and dropped inside `providers::call`, so nothing downstream
//! can see the pieces. These tests pin the two properties that keep the estimate
//! honest: the section markers really are in the frozen prompt (so a reworded
//! heading fails a test instead of silently misattributing bytes), and the
//! buckets are rescaled so they sum to the provider's own count rather than to
//! whatever the character heuristic happened to produce.

use super::prompt::{self, Env};
use super::runner::{context_parts, COMPACT_SUMMARY_MARKER};
use super::store::SkillDef;
use super::trust::{self, TrustDecision, TrustKind, TrustState};
use super::{ContextParts, Message, Usage};
use serde_json::json;

/// The heading of the folder-trust note, which `prompt::split_system` folds into
/// the `instructions` bucket. Not exported from `prompt`, because nothing in
/// production looks for it by name — but pinned here, because reword it and the
/// split silently misfiles the note without any other test noticing.
const FOLDER_TRUST_HEADING: &str = "\n# Folder trust";

fn project(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ol-ctx-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn env_for(project: &str) -> Env<'static> {
    // Leaks a String on purpose, exactly as `reviewer_trust` does: `Env` borrows,
    // and the test process is short-lived.
    Env {
        cwd: ".",
        project: Box::leak(project.to_string().into_boxed_str()),
        branch: "",
        is_git: false,
        worktree: false,
        date: "2026-01-01".into(),
        agent_id: "ol-test",
        inject_global_claude: false,
        disabled_docs: &[],
    }
}

/// Publish a folder decision, holding the process-global lock so a parallel test
/// cannot build its prompt against this table.
fn trust(project: &str, state: TrustState) -> std::sync::MutexGuard<'static, ()> {
    let g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let decisions = match state {
        TrustState::Trusted => vec![TrustDecision {
            path: trust::norm_path(project),
            kind: TrustKind::Folder,
            decision: TrustState::Trusted,
            decided_at: chrono::Utc::now(),
        }],
        TrustState::Untrusted => vec![],
    };
    trust::set_trust(&decisions);
    g
}

fn skill(name: &str) -> SkillDef {
    SkillDef {
        name: name.into(),
        description: "does a thing".into(),
        source: "user".into(),
        path: format!("/tmp/{name}/SKILL.md"),
        enabled: true,
        files: 0,
        disable_model_invocation: false,
        user_invocable: false,
    }
}

#[test]
fn token_estimate_never_reports_a_non_empty_string_as_free() {
    assert_eq!(prompt::token_estimate(""), 0);
    assert_eq!(prompt::token_estimate("abcd"), 1);
    // Rounds up: one byte still costs a token, or a hundred tiny blocks would
    // measure as nothing.
    assert_eq!(prompt::token_estimate("a"), 1);
    assert!(prompt::token_estimate(&"x".repeat(4_000)) >= 1_000);
}

/// The three headings the split relies on must actually be in the prompt the
/// model is sent. This is the guard against drift: reword one and the ring
/// quietly credits the wrong category, which no other test would notice.
#[test]
fn the_frozen_prompt_contains_the_instructions_and_trust_markers() {
    let d = project("markers");
    std::fs::write(d.join("AGENTS.md"), "MARKER-RULE").unwrap();
    let p = d.to_string_lossy().into_owned();

    {
        let _g = trust(&p, TrustState::Trusted);
        let sys = prompt::system(&env_for(&p), None, &[]);
        assert!(
            sys.contains(prompt::INSTRUCTIONS_MARKER),
            "a trusted folder's rules must sit under the split marker:\n{sys}"
        );
    }
    {
        let _g = trust(&p, TrustState::Untrusted);
        let sys = prompt::system(&env_for(&p), None, &[]);
        assert!(
            sys.contains(FOLDER_TRUST_HEADING),
            "an untrusted folder must be marked, because the absence of \
             instructions is deliberate rather than an empty repo:\n{sys}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_skills_marker_is_the_one_skills_section_emits() {
    let sec = prompt::skills_section(&[skill("deploy")]);
    assert!(
        sec.contains(prompt::SKILLS_MARKER),
        "the skills section must carry the marker the split looks for:\n{sec}"
    );
    assert!(sec.contains("deploy"));
}

#[test]
fn split_system_attributes_each_section_and_loses_nothing() {
    let s = format!(
        "CORE PROMPT\n\
         {INSTR}\nthe project rules\n\
         {TRUST}\nthis folder is not trusted\n\
         {SKILLS}\nskills live here\n",
        INSTR = prompt::INSTRUCTIONS_MARKER.trim_start_matches('\n'),
        TRUST = FOLDER_TRUST_HEADING.trim_start_matches('\n'),
        SKILLS = prompt::SKILLS_MARKER.trim_start_matches('\n'),
    );
    let (system, instructions, skills) = prompt::split_system(&s);
    assert!(
        system > 0 && instructions > 0 && skills > 0,
        "every section must be measured"
    );
    // Rounding up per slice can only add, never lose bytes.
    let whole = prompt::token_estimate(&s);
    let sum = system + instructions + skills;
    assert!(
        sum >= whole && sum <= whole + 3,
        "the split must account for the whole prompt: {sum} vs {whole}"
    );
}

#[test]
fn split_system_puts_a_prompt_with_no_sections_entirely_in_system() {
    let (system, instructions, skills) = prompt::split_system("just the core prompt");
    assert_eq!(instructions, 0);
    assert_eq!(skills, 0);
    assert_eq!(system, prompt::token_estimate("just the core prompt"));
}

#[test]
fn context_parts_separates_the_prompt_the_tools_and_the_history() {
    let system = format!(
        "CORE\n{INSTR}\nrules\n",
        INSTR = prompt::INSTRUCTIONS_MARKER
    );
    let tools = vec![json!({"name": "read_file", "description": "x".repeat(400)})];
    let messages = vec![
        Message::user_text("hello there"),
        Message {
            role: "assistant".into(),
            content: vec![json!({"type": "text", "text": "hi"})],
            model: String::new(),
        },
    ];
    let p = context_parts(&system, &tools, &messages);
    assert!(p.system > 0, "the core prompt is a category");
    assert!(p.instructions > 0, "the rules block is its own category");
    assert!(
        p.tools >= 100,
        "the tool schema must be measured: {}",
        p.tools
    );
    assert!(p.history > 0, "the conversation must be measured");
    assert_eq!(p.attachments, 0);
    assert_eq!(p.summary, 0);
}

#[test]
fn an_attachment_note_counts_as_attachments_not_conversation() {
    let bare = context_parts("core", &[], &[Message::user_text("look at this")]);
    let note = super::attachments::prompt_paths(&["/tmp/a.png".to_string()]);
    let text = format!("look at this{note}");
    let p = context_parts("core", &[], &[Message::user_text(text)]);
    assert!(
        p.attachments > 0,
        "the staged-path note is an attachment cost"
    );
    // The user's own words stay conversation: staging a file must not inflate
    // the bucket that represents the chat itself.
    assert_eq!(
        p.history, bare.history,
        "a staged attachment must not change the conversation bucket"
    );
    assert!(
        p.attachments > p.history,
        "the note is the bigger cost here and must be attributed to attachments"
    );
}

#[test]
fn an_image_block_counts_towards_attachments() {
    let img =
        json!({"type": "image", "source": {"media_type": "image/png", "data": "A".repeat(1_600)}});
    let messages = vec![Message::user(vec![
        json!({"type": "text", "text": "what is this"}),
        img,
    ])];
    let p = context_parts("core", &[], &messages);
    // 1_600 base64 chars ≈ 1_200 decoded bytes ≈ 300 tokens.
    assert!(
        (250..=350).contains(&p.attachments),
        "a screenshot's cost must be visible, got {}",
        p.attachments
    );
}

#[test]
fn a_compaction_summary_is_its_own_bucket_and_not_history() {
    let messages = vec![
        Message::user_text(format!(
            "{COMPACT_SUMMARY_MARKER} Summary of everything so far:\n\nwe did things"
        )),
        Message::user_text("and now this"),
    ];
    let p = context_parts("core", &[], &messages);
    assert!(p.summary > 0, "the summary is what the window mostly holds");
    assert!(
        p.history > 0,
        "turns after the compaction are still conversation"
    );
    assert!(
        p.summary > p.history,
        "the summary must not be folded into history"
    );
}

#[test]
fn only_the_first_message_can_be_the_summary() {
    let messages = vec![
        Message::user_text("an ordinary first turn"),
        Message::user_text(format!(
            "{COMPACT_SUMMARY_MARKER} looks like a summary but is mid-history"
        )),
    ];
    let p = context_parts("core", &[], &messages);
    assert_eq!(
        p.summary, 0,
        "a marker quoted mid-conversation is not a summary"
    );
    assert!(p.history > 0);
}

#[test]
fn scaled_parts_sum_to_the_provider_count_exactly() {
    let raw = ContextParts {
        system: 300,
        instructions: 40,
        skills: 7,
        tools: 250,
        attachments: 3,
        history: 1_111,
        summary: 9,
    };
    for total in [0u64, 1, 7, 100, 1_234, 200_000, 1_048_576] {
        let scaled = raw.scaled_to(total);
        assert_eq!(
            scaled.total(),
            total,
            "the bars must add up to the ring, not to the heuristic (total {total})"
        );
    }
}

#[test]
fn scaling_an_unmeasured_request_stays_empty_rather_than_dividing_by_zero() {
    let scaled = ContextParts::default().scaled_to(123_456);
    assert!(scaled.is_empty());
    assert_eq!(scaled.total(), 0);
}

#[test]
fn a_zero_total_leaves_every_bucket_at_zero() {
    let raw = ContextParts {
        system: 10,
        history: 10,
        ..Default::default()
    };
    assert_eq!(raw.scaled_to(0).total(), 0);
}

#[test]
fn an_old_saved_task_without_a_split_still_loads() {
    // Every task written before this field existed, and every fixture in
    // `lib.rs`/`transfer.rs`, has no `parts` key. `#[serde(default)]` is the
    // only thing keeping them parseable.
    let old = json!({
        "input": 1, "output": 2, "cache_read": 3, "cache_write": 4,
        "cost": 0.5, "last_context": 1234
    });
    let usage: Usage = serde_json::from_value(old).expect("a pre-existing task must still load");
    assert_eq!(usage.last_context, 1234);
    assert!(usage.parts.is_empty());
}

#[test]
fn an_unmeasured_split_is_left_out_of_the_saved_task() {
    let usage = Usage {
        last_context: 99,
        ..Default::default()
    };
    let wire = serde_json::to_value(&usage).unwrap();
    assert!(
        wire.get("parts").is_none(),
        "an empty split must not add a key to every saved task: {wire}"
    );

    let measured = Usage {
        last_context: 99,
        parts: ContextParts {
            history: 99,
            ..Default::default()
        },
        ..Default::default()
    };
    let wire = serde_json::to_value(&measured).unwrap();
    assert_eq!(wire["parts"]["history"], 99);
    // And it survives the round trip it just went through.
    let back: Usage = serde_json::from_value(wire).unwrap();
    assert_eq!(back.parts.history, 99);
}
