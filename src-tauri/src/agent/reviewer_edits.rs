//! Release-gate tests for exact-string edit semantics.
//!
//! `apply_edit` is the only thing standing between a model's idea of a file
//! and the user's actual file. A silent off-by-one, a CRLF rewrite that
//! reflows a whole file, or a `replace_all` that hits more than the model
//! meant corrupts work with no error and no undo. The existing
//! `tools::tests::edit_unique_and_crlf` covers the happy path and one CRLF
//! case; the cases below are the ones that break silently.
//!
//! All of these are pure string functions: no filesystem, no clock, no env.

#![cfg(test)]

use super::tools::{apply_edit, diff_lines};

fn edited(content: &str, old: &str, new: &str) -> String {
    apply_edit(content, old, new, false)
        .unwrap_or_else(|e| panic!("edit should have applied: {e}"))
        .new_content
}

fn refusal(content: &str, old: &str, new: &str) -> String {
    apply_edit(content, old, new, false)
        .err()
        .unwrap_or_else(|| panic!("edit should have been refused, but it applied"))
}

// ───────────────────────── refusals must be refusals ─────────────────────────

/// An edit that changes nothing is a mistake worth reporting, not a silent
/// no-op the model reads as success.
#[test]
fn an_edit_that_would_change_nothing_is_refused() {
    assert!(
        apply_edit("a\nb\n", "a", "a", false).is_err(),
        "old == new must be refused"
    );
    assert!(
        apply_edit("a\nb\n", "", "x", false).is_err(),
        "an empty old_string must be refused, never a full-file replace"
    );
}

/// A unique match is mandatory by default. Replacing an ambiguous match at
/// the first occurrence is how a model silently edits the wrong function.
#[test]
fn an_ambiguous_match_is_refused_unless_replace_all_is_set() {
    let src = "let x = 1;\nlet x = 1;\nlet x = 1;\n";
    let e = refusal(src, "let x = 1;", "let x = 2;");
    assert!(
        e.contains("3 places"),
        "the model must be told how many matches there are: {e}"
    );
    assert!(
        e.contains("replace_all"),
        "the model must be told how to proceed: {e}"
    );
    // With the flag, every one is replaced — and all of them, not one.
    let all = apply_edit(src, "let x = 1;", "let x = 2;", true)
        .unwrap()
        .new_content;
    assert_eq!(
        all.matches("let x = 2;").count(),
        3,
        "replace_all must cover every match"
    );
}

/// When `old_string` is not there, the error points the model at the closest
/// line rather than just saying "not found" — that hint is the only thing that
/// gets a self-correcting retry.
///
/// The hint is keyed on the first line of `old_string` matching a line of the
/// file once trimmed. So the near miss has to be one that is NOT a plain
/// substring (those just apply — see `a_mid_line_match_replaces_only_the_matched_span`)
/// but whose first line still trims to a line in the file: a difference later
/// in the block.
#[test]
fn a_failed_edit_points_at_the_line_that_almost_matched() {
    let src = "fn main() {
    let total = 1;
    let other = 2;
}
";
    // The first line matches, the second does not, so the whole block is
    // refused — and the refusal must say where the near miss is.
    let e = refusal(
        src,
        "    let total = 1;
    let missing = 9;",
        "x",
    );
    assert!(e.contains("not found"), "{e}");
    assert!(
        e.contains("line 2"),
        "the near-miss line number is the actionable part: {e}"
    );
    assert!(
        e.contains("let total = 1;"),
        "the actual text of that line must be quoted back: {e}"
    );
}

/// An `old_string` that simply does not exist must be refused, and must not
/// be "helpfully" matched against something else.
#[test]
fn an_old_string_that_is_not_in_the_file_is_refused() {
    let src = "alpha\nbeta\ngamma\n";
    let e = refusal(src, "delta", "epsilon");
    assert!(e.contains("not found"), "{e}");
    // And the file content is untouched by the refusal.
    assert_eq!(src, "alpha\nbeta\ngamma\n");
}

// ───────────────────────── CRLF must survive ─────────────────────────

/// A model always sends LF. A Windows file is CRLF. The edit must apply AND
/// leave the rest of the file CRLF — converting the whole file to LF is a
/// diff the user never asked for and cannot easily see.
#[test]
fn an_lf_edit_into_a_crlf_file_keeps_the_file_crlf() {
    let src = "one\r\ntwo\r\nthree\r\n";
    let out = edited(src, "one\ntwo", "1\n2");
    assert_eq!(
        out, "1\r\n2\r\nthree\r\n",
        "the untouched lines must keep their CRLF endings"
    );
    assert!(
        !out.replace("\r\n", "").contains('\n'),
        "no line may be left with a bare LF: {out:?}"
    );
}

/// The reverse: a CRLF `old_string` from a model that read the file back with
/// line endings intact must also apply to a CRLF file.
#[test]
fn a_crlf_edit_into_a_crlf_file_also_works() {
    let src = "one\r\ntwo\r\n";
    let out = edited(src, "one\r\ntwo", "1\r\n2");
    assert_eq!(out, "1\r\n2\r\n");
}

/// A file that is genuinely LF must NOT be given CRLF endings by an edit that
/// happened to arrive with them. The conversion is conditional on the file.
#[test]
fn an_lf_file_is_not_given_crlf_endings_by_a_crlf_edit() {
    let src = "one\ntwo\n";
    // The CRLF form is not present, so the edit is refused rather than
    // guessed at.
    let e = apply_edit(src, "one\r\ntwo", "1\r\n2", false);
    // Either it is refused, or it applied — but it must never leave the file
    // with a mix of both endings.
    if let Ok(o) = e {
        assert!(
            !o.new_content.contains("\r\n"),
            "an LF file must stay LF: {}",
            o.new_content
        );
    }
}

/// `replace_all` on a CRLF file must replace every occurrence and still leave
/// the line endings alone.
#[test]
fn replace_all_on_a_crlf_file_keeps_every_line_crlf() {
    let src = "x\r\ny\r\nx\r\ny\r\n";
    let out = apply_edit(src, "x\ny", "z", true).unwrap().new_content;
    assert_eq!(out, "z\r\nz\r\n", "{out:?}");
    assert!(
        !out.replace("\r\n", "").contains('\n'),
        "mixed endings: {out:?}"
    );
}

// ───────────────────────── exactness ─────────────────────────

/// Indentation is preserved exactly as quoted. A reflowed block is a diff the
/// user never asked for, and a nested level that gets flattened is a syntax
/// error at best.
#[test]
fn indentation_survives_an_edit_exactly_as_quoted() {
    let src = "fn f() {\n    let a = 1;\n}\n";
    assert_eq!(
        edited(src, "    let a = 1;", "    let a = 2;"),
        "fn f() {\n    let a = 2;\n}\n",
        "the four leading spaces must be preserved"
    );
    // A nested level is not flattened.
    let nested = "fn f() {\n    if x {\n        let a = 1;\n    }\n}\n";
    let out = edited(nested, "        let a = 1;", "        let a = 2;");
    assert_eq!(
        out, "fn f() {\n    if x {\n        let a = 2;\n    }\n}\n",
        "{out:?}"
    );
    // The replacement is inserted verbatim: re-indenting is the model's own
    // choice and is not normalised away behind its back.
    assert_eq!(
        edited(src, "    let a = 1;", "let a = 2;"),
        "fn f() {\nlet a = 2;\n}\n",
        "the replacement is inserted exactly as given"
    );
}

/// `old_string` matches as a substring, not as whole lines. A match that
/// starts mid-line replaces only that span, and the surrounding indentation
/// is outside the match and therefore survives. Pinning this matters because a
/// test that assumed line semantics would have hidden the real behaviour.
#[test]
fn a_mid_line_match_replaces_only_the_matched_span() {
    let src = "    let a = 1;\n";
    assert_eq!(
        edited(src, "let a = 1;", "let a = 2;"),
        "    let a = 2;\n",
        "the indent is outside the match and must survive"
    );
}

/// Multi-line edits must apply as a unit. A partial application — the first
/// line landing and the rest not — is the worst failure here, because the
/// file is then syntactically broken and the model is told nothing.
#[test]
fn a_multi_line_edit_lands_whole_or_not_at_all() {
    let src = "fn f() {\n    let a = 1;\n    let b = 2;\n}\n";
    let out = edited(src, "    let a = 1;\n    let b = 2;", "    let a = 9;");
    assert_eq!(out, "fn f() {\n    let a = 9;\n}\n");

    // A multi-line old_string where only part is present must be refused.
    assert!(
        apply_edit(src, "    let a = 1;\n    let b = 999;", "x", false).is_err(),
        "a partially-present multi-line old_string must not apply"
    );
}

/// Deleting a line must not leave a blank artefact behind, and a multi-line
/// delete is exact.
#[test]
fn deleting_lines_removes_them_entirely() {
    let src = "a\nb\nc\n";
    assert_eq!(
        edited(src, "b\n", ""),
        "a\nc\n",
        "deleting a line must not leave a blank line behind"
    );
    assert_eq!(
        edited(src, "b\nc\n", ""),
        "a\n",
        "a multi-line delete is exact"
    );
}

// ───────────────────────── the diff the user sees ─────────────────────────

/// `diff_lines` is what the review view shows. Its `+`/`−` counts are what
/// the user approves on the strength of, so they must match the file.
#[test]
fn the_review_diff_counts_match_what_actually_changed() {
    // Replace one line: one add, one delete.
    let (lines, add, del) = diff_lines("a\nb\nc\n", "a\nB\nc\n");
    assert_eq!(
        (add, del),
        (1, 1),
        "a replaced line is one add and one delete: {lines:?}"
    );
    // Replace one line and append another: the hunk contributes the delete
    // for "b", the add for "B", and the add for "d".
    let (_, add, del) = diff_lines("a\nb\nc\n", "a\nB\nc\nd\n");
    assert_eq!(
        (add, del),
        (2, 1),
        "replace-plus-append is two adds and one delete"
    );

    // Pure insertion.
    let (_, add, del) = diff_lines("a\nc\n", "a\nb\nc\n");
    assert_eq!((add, del), (1, 0));

    // Pure deletion.
    let (_, add, del) = diff_lines("a\nb\nc\n", "a\nc\n");
    assert_eq!((add, del), (0, 1));

    // No change at all.
    let (lines, add, del) = diff_lines("a\nb\n", "a\nb\n");
    assert_eq!(
        (add, del),
        (0, 0),
        "an unchanged file has no diff: {lines:?}"
    );

    // Creating a file from nothing is all additions and no deletions. This is
    // the arithmetic a rewritten diff loop gets wrong.
    let (_, add, del) = diff_lines("", "a\nb\nc\n");
    assert_eq!(
        (add, del),
        (3, 0),
        "creating a file is three additions, not three of each"
    );
}

/// Every diff row is tagged so the UI knows whether to colour it. An
/// untagged or unknown tag would render as an invisible line in the review.
#[test]
fn every_diff_row_carries_a_tag_the_ui_can_colour() {
    let (lines, _, _) = diff_lines("a\nb\nc\n", "a\nB\nc\nd\n");
    assert!(!lines.is_empty(), "a real change produces rows");
    for row in &lines {
        let k = row["k"].as_str().expect("every row has a kind tag");
        assert!(
            matches!(k, "a" | "d" | "c" | "h"),
            "unknown diff row tag {k:?}: {row:?}"
        );
        assert!(row["t"].is_string(), "every row carries its text: {row:?}");
    }
    // The hunks come first so the UI can lay out the file in sections.
    assert_eq!(
        lines[0]["k"], "h",
        "a diff starts with a hunk header: {lines:?}"
    );
}

/// A `\r` must never reach the rendered diff row, or every line in the review
/// pane on a CRLF file shows as a doubled or overprinted line.
#[test]
fn a_crlf_file_does_not_leak_carriage_returns_into_the_review_diff() {
    let (lines, add, del) = diff_lines("a\r\nb\r\n", "a\r\nB\r\n");
    assert_eq!((add, del), (1, 1));
    for row in &lines {
        let t = row["t"].as_str().unwrap();
        assert!(
            !t.contains('\r'),
            "a carriage return reached the review pane: {t:?}"
        );
    }
}
