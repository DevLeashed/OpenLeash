//! Per-project agent memory, the way Claude Code's auto memory works.
//!
//! Each project gets a folder under `~/.openleash/memory/<project>/` holding a
//! `MEMORY.md` index and one small file per memory. The index is the only part
//! that is loaded at the start of a conversation, so it is capped: the first
//! [`INDEX_LINES`] lines, or [`INDEX_BYTES`], whichever comes first. Detail lives
//! in the topic files, which the agent reads on demand with its own file tools —
//! the same progressive-disclosure bargain as skills.
//!
//! Four kinds, mirroring Claude Code, because they are genuinely different
//! things to remember and a model blurs them without being told:
//!
//! - `user`     — who the user is and how they like to work
//! - `feedback` — corrections they gave and approaches they confirmed
//! - `project`  — ongoing work and decisions that aren't in the code or git
//! - `reference` — where to find things outside the repo (dashboards, trackers)
//!
//! Deliberately *not* stored here: anything derivable from the code. File
//! paths, architecture and build commands are a `grep` away and a memory that
//! goes stale is worse than no memory at all.

use super::store;
use std::path::{Path, PathBuf};

/// The index is what every session pays for, so it stays small. Claude Code
/// uses the same two limits: keep the index readable, push detail into topics.
pub const INDEX_LINES: usize = 200;
pub const INDEX_BYTES: usize = 25_000;

/// The note types. Anything else is rejected rather than silently coerced — a
/// memory filed under the wrong heading is a memory nobody will read.
pub const KINDS: &[&str] = &["user", "feedback", "project", "reference"];

/// One note in `MEMORY.md`.
///
/// The struct owns the line format: `write_index` renders from these fields and
/// `open` parses back into them, so the two can never disagree. Storing a
/// pre-rendered `line` string alongside the fields is how a format drifts into
/// writing one shape and reading another.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub kind: String,
    pub name: String,
    /// One line — this is all a future session sees without opening the file,
    /// so the writer is pushed to make it worth reading on its own.
    pub summary: String,
    pub file: String,
}

impl Entry {
    /// The one line this memory contributes to the index. The filename is in
    /// backticks at the end so a human editing the file by hand can see which
    /// file each line refers to, and so `open` can find it again.
    pub fn line(&self) -> String {
        format!(
            "- [{}] {} — {} `{}`",
            self.kind, self.name, self.summary, self.file
        )
    }
}

/// A project's index, read. The folder is the caller's to hold: every use
/// here already has it, and carrying it twice invites the two drifting.
#[derive(Debug, Clone)]
pub struct Store {
    pub entries: Vec<Entry>,
}

fn slug(text: &str) -> String {
    let s: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    let s: String = s.chars().take(32).collect();
    if s.is_empty() {
        "memory".into()
    } else {
        s.trim_end_matches('-').to_string()
    }
}

/// FNV-1a, so a folder name carries a fingerprint of the path it came from.
/// The slug alone is not enough: `D:\a\my-project` and `/home/me/my-project`
/// both slug to `my-project`, and two projects sharing one memory folder
/// would quietly poison each other's notes. Eight hex digits is enough to make
/// that collision a non-event without pulling in a hashing crate.
fn fingerprint(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:08x}")
}

/// The memory folder for a project, derived from the git repo so every
/// worktree and subdirectory of the same repository shares one memory — the
/// point of persisting anything. Falls back to the project path outside a repo.
///
/// The path goes through the same fold-and-hash treatment as anything else
/// that is turned into a filename: a Windows project path is not a legal
/// folder name, and two different projects must not collide on one.
pub fn dir_for(project: &str) -> PathBuf {
    let root = store::data_dir().join("memory");
    let _ = std::fs::create_dir_all(&root);
    // Prefer the repo root: `git rev-parse --show-toplevel` resolves worktrees
    // to the main checkout, so a worktree's memories land beside the main one's.
    let key = repo_root(project).unwrap_or_else(|| project.to_string());
    root.join(format!("{}-{}", slug(&key), fingerprint(&key)))
}

fn repo_root(project: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Read the index, dropping any line that doesn't parse or names a file that
/// has since been deleted. A memory folder is editable by hand — people do
/// delete these files — so a dangling index line is expected, not exceptional.
pub fn open(dir: &Path) -> Store {
    let Ok(raw) = std::fs::read_to_string(dir.join("MEMORY.md")) else {
        return Store { entries: vec![] };
    };
    let mut entries = vec![];
    for line in raw.lines() {
        let l = line.trim();
        if !l.starts_with("- [") {
            continue;
        }
        let Some(rest) = l.strip_prefix("- [") else {
            continue;
        };
        let Some((kind, rest)) = rest.split_once(']') else {
            continue;
        };
        if !KINDS.contains(&kind.trim()) {
            continue;
        }
        // The filename is the trailing backticked token — peel it off the end
        // first, so a summary that itself contains " — " or a backtick can't
        // shift the fields. Splitting on the first dash instead would treat the
        // summary as the filename and drop every entry.
        let tail = rest.trim();
        let tail = tail.strip_suffix('`').unwrap_or(tail);
        let Some((head, file)) = tail.rsplit_once('`') else {
            continue;
        };
        let Some((name, summary)) = head.trim().split_once(" — ") else {
            continue;
        };
        let name = name.trim().to_string();
        let summary = summary.trim().to_string();
        let file = file.trim();
        if name.is_empty()
            || summary.is_empty()
            || file.is_empty()
            || file.contains('/')
            || file.contains('\\')
            || file.starts_with('.')
        {
            continue;
        }
        if !dir.join(file).exists() {
            continue;
        }
        entries.push(Entry {
            kind: kind.trim().to_string(),
            name,
            summary,
            file: file.to_string(),
        });
    }
    Store { entries }
}

/// Rewrite the index in place, leaving topic files alone.
///
/// Falls back to a plain write: the rename inside `try_write_atomic` can fail
/// on a locked file on Windows, and a memory that is saved but not indexed is
/// invisible forever, which is worse than a small window of non-atomicity.
pub fn write_index(dir: &Path, entries: &[Entry]) -> Result<(), String> {
    let mut s = String::from("# Memory\n");
    s.push_str("Index of this project's memory. One line per memory; the detail lives in the file beside it.\n");
    for e in entries {
        s.push_str(&e.line());
        s.push('\n');
    }
    let p = dir.join("MEMORY.md");
    store::try_write_atomic(&p, &s)
        .map_err(|e| e.to_string())
        .or_else(|e| {
            std::fs::write(&p, &s)
                .map_err(|_| format!("cannot write the memory index {}: {e}", p.display()))
        })
}

/// How much room is left in the index, in lines and bytes. A negative number
/// means it is already over that limit. Adding a line costs one more, so
/// `room` is called on the index *before* the new line is appended.
pub fn room(index: &str) -> (i64, i64) {
    let lines = index.lines().count() as i64 + 1;
    (
        INDEX_LINES as i64 - lines,
        INDEX_BYTES as i64 - index.len() as i64,
    )
}

/// Under this many lines or bytes of headroom, a write warns the model to
/// compact the index. Over the limit, the runner refuses the write outright:
/// an entry past the limit is loaded by nobody, so "saved" would be a lie.
pub const NEAR: i64 = 20;

/// A safe filename for a memory: slug of the name, with a numeric suffix if
/// one already exists. Returns `None` if the slug is unusable.
pub fn file_for(dir: &Path, name: &str) -> Option<String> {
    let base = slug(name);
    if base == "memory" {
        return None;
    }
    let ext = "md";
    let first = format!("{base}.{ext}");
    if !dir.join(&first).exists() {
        return Some(first);
    }
    for n in 2..200 {
        let f = format!("{base}-{n}.{ext}");
        if !dir.join(&f).exists() {
            return Some(f);
        }
    }
    None
}

pub fn body(dir: &Path, file: &str) -> Result<String, String> {
    std::fs::read_to_string(dir.join(file)).map_err(|e| format!("cannot read memory `{file}`: {e}"))
}

pub fn write_body(dir: &Path, file: &str, body: &str) -> Result<(), String> {
    store::try_write_atomic(&dir.join(file), body).map_err(|e| e.to_string())
}

pub fn remove(dir: &Path, file: &str) -> Result<(), String> {
    std::fs::remove_file(dir.join(file)).map_err(|e| format!("cannot delete memory `{file}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        // Unique per test *and* per run: these run on parallel threads, and a
        // shared folder made them delete each other's files mid-test.
        let d = std::env::temp_dir().join(format!(
            "ol-mem-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn index_roundtrips_and_drops_dangling() {
        let d = tmp("roundtrip");
        std::fs::write(d.join("user_role.md"), "x").unwrap();
        let e = Entry {
            kind: "user".into(),
            name: "role".into(),
            summary: "who the user is".into(),
            file: "user_role.md".into(),
        };
        write_index(&d, std::slice::from_ref(&e)).unwrap();
        // The index must parse back into exactly what went in — this is the
        // round trip every save and every recall depends on.
        let s = open(&d);
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].name, "role");
        assert_eq!(s.entries[0].summary, "who the user is");
        assert_eq!(s.entries[0].file, "user_role.md");
        assert_eq!(s.entries[0].kind, "user");
        // A hand-deleted topic file leaves a line the loader must skip.
        std::fs::write(d.join("MEMORY.md"), "- [user] role — who the user is `user_role.md`\n- [project] gone — removed by hand `gone.md`\n").unwrap();
        let s = open(&d);
        assert_eq!(s.entries.len(), 1);
        // Junk lines are ignored rather than fatal.
        std::fs::write(
            d.join("MEMORY.md"),
            "not a list item\n- [nope] x — y `z.md`\n- [user] no file — z\n",
        )
        .unwrap();
        assert!(open(&d).entries.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn index_rejects_traversal_names() {
        let d = tmp("traversal");
        std::fs::write(d.join("MEMORY.md"), "- [user] a — x `../../etc/passwd`\n- [user] b — x `.hidden`\n- [user] c — x `sub/x.md`\n").unwrap();
        assert!(
            open(&d).entries.is_empty(),
            "a name outside the folder must never be followed"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn room_and_limits() {
        let (l, b) = room("# Memory\n- [user] a — b\n");
        assert!(l <= INDEX_LINES as i64 && b <= INDEX_BYTES as i64);
        // One entry per line, so a full index is over on both counts.
        let full = format!(
            "{}\n{}",
            "# Memory",
            (0..INDEX_LINES + 5)
                .map(|i| format!("- [user] m{i} — summary {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let (l, _b) = room(&full);
        assert!(l <= 0, "a full index must leave no room to append");
        // A single enormous line blows the byte cap even at one line.
        let (l, b) = room(&"x".repeat(INDEX_BYTES + 10));
        assert!(l > 0 && b < 0, "one long line is over on bytes, not lines");
    }

    #[test]
    fn file_names_never_collide() {
        let d = tmp("names");
        assert_eq!(file_for(&d, "Test Role").as_deref(), Some("test-role.md"));
        std::fs::write(d.join("test-role.md"), "x").unwrap();
        assert_eq!(file_for(&d, "Test Role").as_deref(), Some("test-role-2.md"));
        // A name that slugs to nothing is refused rather than writing a file
        // called `memory.md` for every such memory.
        assert!(file_for(&d, "///").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fingerprint_separates_same_named_projects() {
        // The folder name is slug + fingerprint of the full path. The slug is
        // only for humans; the fingerprint is what makes it unique, so two
        // projects must never collide whatever the slug happens to do.
        let a = "/srv/workspace/checkout/my-project";
        let b = "/home/dev/my-project";
        let folder = |p: &str| format!("{}-{}", slug(p), fingerprint(p));
        assert_ne!(
            folder(a),
            folder(b),
            "different paths must get different folders"
        );
        // The same path is stable, so a project's memories keep landing in one
        // place across sessions.
        assert_eq!(folder(a), folder(a));
        // Slugs are prefix-truncated, so a deeper path can slug to something
        // different from the same-named shallow one — the fingerprint is what
        // keeps that from being a collision.
        assert!(slug(a).chars().count() <= 32);
    }
}
