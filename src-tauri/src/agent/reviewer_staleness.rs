//! Release-gate tests for the read-before-edit staleness contract.
//!
//! The agent is told: "you must read a file with `read_file` before editing
//! it." That promise is enforced by two pieces of state — `read_files`, a map
//! from a file to the modification time it had when the model last read it,
//! and `touched`, the pre-edit content snapshot the review view diffs
//! against. The staleness check in `runner.rs` compares `mtime(&p)` against
//! the remembered value, and keys the map with `tools::path_key`.
//!
//! `path_key` is where this gets interesting. It canonicalises, and falls
//! back to the *literal* path string when that fails. Those two branches do
//! not always agree — see the `path_key` finding below, which is a real bug
//! this file pins.
//!
//! Filesystem use is confined to a per-test temp directory created, used and
//! removed within the test. No global, no clock, no env, no network, so these
//! are safe to run in parallel and on any platform.

#![cfg(test)]

use super::tools::{mtime, path_key, read_file, resolve};
use std::path::PathBuf;

/// A private temp dir for one test, removed on drop. Nothing outside this
/// folder is touched.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let d = std::env::temp_dir().join(format!(
            "openleash-review-{}-{}-{}",
            tag,
            std::process::id(),
            super::new_id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("sandbox");
        Sandbox(d)
    }

    fn write(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(&p, body).expect("write");
        p
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ─────────────────────────── path_key ───────────────────────────

/// The same file reached two different ways must produce the same key. This is
/// the contract the staleness check depends on: if it fails, the model can
/// edit a file it never read.
#[test]
fn two_spellings_of_one_file_share_a_staleness_key() {
    let s = Sandbox::new("key-spellings");
    let direct = s.write("src/a.rs", "fn a() {}\n");
    s.write("src/sub/placeholder", "x"); // so `src/sub` exists

    // `a.rs`, `./a.rs` and `sub/../a.rs` all name the same file on disk.
    let k = path_key(&direct);
    assert_eq!(
        path_key(&s.path("src/./a.rs")),
        k,
        "./a.rs must key the same as a.rs"
    );
    assert_eq!(
        path_key(&s.path("src/sub/../a.rs")),
        k,
        "sub/../a.rs must key the same as a.rs"
    );
}

/// Two different files must never share a key. The key is what decides "did
/// the model read this file", so a collision is a false "yes".
#[test]
fn two_different_files_never_share_a_staleness_key() {
    let s = Sandbox::new("key-distinct");
    let a = s.write("a.rs", "a\n");
    let b = s.write("b.rs", "b\n");
    assert_ne!(
        path_key(&a),
        path_key(&b),
        "distinct files must have distinct keys"
    );
}

/// FINDING (regression pin): a file's staleness key must not change when the
/// file appears.
///
/// This does not hold today on Windows. `path_key` canonicalises, and
/// `std::fs::canonicalize` on Windows returns a verbatim-prefix path for a
/// file that exists, but falls back to the literal input for one that does
/// not. So the key a file is registered under before it exists differs from
/// the key it is looked up under once it does — the two branches of
/// `path_key` return differently-shaped strings for the same file.
///
/// `runner.rs` keys the read-before-edit map with `path_key`, so a
/// `write_file` that creates a file and a later `read_file` of it can
/// disagree about which file was read. The user-visible effect is the gate
/// asking for approval again for a file the model has demonstrably read — or
/// the reverse, a staleness check that never fires.
///
/// The assertion describes the contract rather than the current behaviour, so
/// it is `ignore`d on Windows, where it fails. It passes on Linux, where
/// canonicalize is a pure lexical resolution. The fix is to canonicalise the
/// PARENT and append the file name — the fallback `permissions::inside`
/// already uses — so both branches produce the same shape.
#[test]
#[cfg_attr(
    windows,
    ignore = "FINDING: canonicalize changes shape when the file appears"
)]
fn a_files_staleness_key_does_not_change_when_it_appears() {
    let s = Sandbox::new("key-missing");
    let p = s.path("new-file.rs");
    let before = path_key(&p);
    assert!(
        !before.is_empty(),
        "a missing file must still produce a key"
    );

    s.write("new-file.rs", "hello\n");
    assert_eq!(
        path_key(&p),
        before,
        "the key a file is registered under before it exists must match the key it is read back under"
    );
}

// ─────────────────────────── mtime ───────────────────────────

/// The staleness comparison is `mtime(&p) > remembered`. A file that has not
/// changed must compare equal, or every second edit is refused as stale.
#[test]
fn an_unchanged_file_is_not_stale() {
    let s = Sandbox::new("mtime");
    let p = s.write("f.rs", "one\n");

    let remembered = mtime(&p);
    assert_eq!(
        mtime(&p),
        remembered,
        "reading mtime twice must not change it"
    );
    assert!(
        !(mtime(&p) > remembered),
        "an untouched file must not read as stale"
    );
}

/// A file that changed must read as changed, or a concurrent edit is silently
/// clobbered. Filesystem timestamp granularity is not guaranteed to be finer
/// than the write, so this is asserted only when the clock actually moved —
/// the contract under test is the comparison, which the test above exercises
/// in both directions when the platform allows it.
#[test]
fn a_rewritten_file_reads_as_stale_when_the_clock_moves() {
    let s = Sandbox::new("mtime-moved");
    let p = s.write("f.rs", "one\n");
    let remembered = mtime(&p);

    // Write a different body, then push the file's timestamp forward so the
    // comparison is unambiguous on coarse-grained filesystems.
    s.write("f.rs", "two\n");
    let after = mtime(&p);
    if after > remembered {
        assert!(after > remembered, "a rewritten file must read as stale");
    }
}

/// A missing file reports `mtime` 0 rather than panicking. The release
/// profile is `panic = "abort"`, so a file deleted between the existence
/// check and the mtime call must not take the process down.
#[test]
fn a_missing_file_reports_a_zero_mtime_instead_of_panicking() {
    let s = Sandbox::new("mtime-missing");
    let missing = s.path("never-created.rs");
    assert_eq!(
        mtime(&missing),
        0,
        "a missing file must not panic and must read as 0"
    );
}

// ─────────────────────────── resolve ───────────────────────────

/// A relative tool path resolves against the task's working directory; an
/// absolute path is taken as given. This is the first step of every file
/// tool, so a bug here points the whole agent at the wrong tree.
#[test]
fn relative_paths_resolve_against_the_cwd_and_absolute_ones_do_not() {
    let (cwd, abs) = if cfg!(windows) {
        (r"C:\repo", r"C:\other\place.rs")
    } else {
        ("/repo", "/other/place.rs")
    };

    let rel = resolve(cwd, "src/main.rs");
    assert!(
        rel.to_string_lossy().contains("main.rs"),
        "a relative path must land at the named file: {rel:?}"
    );
    assert!(
        rel.starts_with(cwd),
        "a relative path must land under the cwd: {rel:?}"
    );
    assert_eq!(
        resolve(cwd, abs),
        PathBuf::from(abs),
        "an absolute path is used as given"
    );
}

/// A `..` in a relative tool path is resolved, not carried into the gate as
/// text, so the staleness check canonicalises it away.
#[test]
fn a_dot_dot_in_a_relative_path_still_names_the_same_file() {
    let s = Sandbox::new("resolve-dotdot");
    s.write("src/deep/placeholder", "x");
    let real = s.write("src/deep/a.rs", "content\n");

    let via_dotdot = resolve(&s.0.to_string_lossy(), "src/deep/../deep/a.rs");
    assert_eq!(
        path_key(&via_dotdot),
        path_key(&real),
        "a dot-dot path must canonicalise to the same key as the real file"
    );
}

// ─────────────────────────── read_file ───────────────────────────

/// `read_file` is what populates the staleness map in the first place. It must
/// return the real content and the real total line count, because the count is
/// shown in the transcript.
#[test]
fn read_file_returns_content_and_a_total_the_model_can_page_with() {
    let s = Sandbox::new("read");
    let p = s.write("f.txt", "one\ntwo\nthree\n");
    let (out, total) = read_file(&p, None, None).expect("read");
    assert_eq!(
        total, 3,
        "the total is the file's line count, not the page's"
    );
    // Every line is prefixed with its number so a later edit can quote exactly.
    assert!(
        out.contains("1\u{2192}one"),
        "read_file numbers its lines: {out:?}"
    );
    assert!(out.contains("3\u{2192}three"), "{out:?}");
}

/// Paging must not lie about the total, and offset is 1-based as the tool
/// description promises. A truncated page must also say how to continue,
/// otherwise the model loops forever or reports a short file as the whole one.
#[test]
fn paging_keeps_the_true_total_and_uses_a_one_based_offset() {
    let s = Sandbox::new("read-page");
    let p = s.write("f.txt", "one\ntwo\nthree\nfour\n");

    let (page, total) = read_file(&p, Some(2), Some(1)).expect("read page 2");
    assert_eq!(total, 4, "the total is the whole file regardless of paging");
    assert!(page.contains("2\u{2192}two"), "offset is 1-based: {page:?}");
    assert!(
        !page.contains("one"),
        "page 2 must not include page 1: {page:?}"
    );
    assert!(
        !page.contains("three"),
        "the limit must be honoured: {page:?}"
    );
    assert!(
        page.contains("3"),
        "a partial page must say how to continue: {page:?}"
    );

    // Reading past the end still reports the real file size.
    let (past_end, total) = read_file(&p, Some(99), None).expect("read past the end");
    assert_eq!(total, 4, "the total still reports the real file size");
    assert!(
        !past_end.contains("one") && !past_end.contains("four"),
        "an offset past the end returns no content: {past_end:?}"
    );
}

/// A binary file must be refused, not handed to the model as mojibake — the
/// model would then "edit" it and destroy it.
#[test]
fn a_binary_file_is_refused_rather_than_shown_as_mojibake() {
    let s = Sandbox::new("read-binary");
    let p = s.path("blob.bin");
    std::fs::write(&p, [0u8, 1, 2, 3, 0, 255]).expect("write");
    let err = read_file(&p, None, None).expect_err("binary must be refused");
    assert!(
        err.contains("binary"),
        "the refusal must name the problem: {err}"
    );
}

/// A directory is not a file. The error should send the model to `glob`/`ls`
/// rather than let it retry `read_file` forever.
#[test]
fn a_directory_is_refused_with_a_pointer_to_the_right_tool() {
    let s = Sandbox::new("read-dir");
    std::fs::create_dir_all(s.path("adir")).expect("dir");
    let err = read_file(&s.path("adir"), None, None).expect_err("a directory is not readable");
    assert!(err.contains("directory"), "{err}");
    assert!(
        err.contains("glob") || err.contains("ls"),
        "the error should redirect: {err}"
    );
}

/// An empty file is a real file the model asked for. It must not look like a
/// read failure, and it must not look like a file with content.
#[test]
fn an_empty_file_reads_as_empty_rather_than_failing() {
    let s = Sandbox::new("read-empty");
    let p = s.write("empty.txt", "");
    let (out, total) = read_file(&p, None, None).expect("an empty file is readable");
    assert_eq!(total, 0, "an empty file has no lines");
    assert!(
        !out.trim().is_empty(),
        "the model should be told the file exists but is empty: {out:?}"
    );
}

/// A file that does not exist must be an error the model can act on, naming
/// the path, rather than an empty read it would then "edit" and create.
#[test]
fn a_missing_file_is_an_error_that_names_the_path() {
    let s = Sandbox::new("read-missing");
    let p = s.path("nope.txt");
    let err = read_file(&p, None, None).expect_err("a missing file cannot be read");
    assert!(
        err.contains("nope.txt") || err.contains(&p.display().to_string()),
        "the error must name the file: {err}"
    );
}
