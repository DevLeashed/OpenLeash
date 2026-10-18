//! The file tools for a remote-SSH project: the same answers as `tools.rs`,
//! with the I/O done over SFTP (`ssh_session`) and the searches run on the
//! remote host. Formatting, matching and caps are `tools.rs`'s own shared
//! functions, so a remote project behaves exactly like a local one.
//!
//! Paths are `host::SshPath` throughout — never `Path`/`PathBuf`, which on
//! Windows would quietly turn `ssh://h/x` into a relative *local* path.

use super::host::{self, SshPath};
use super::ssh_session as ss;
use super::tools;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Largest file `read_file`/`edit_file` will pull over SFTP.
pub const MAX_TEXT_BYTES: u64 = 32 << 20;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Directories the searches never descend into (as `tools::walker`).
const PRUNE: [&str; 4] = [".git", "node_modules", "target", ".openleash"];

/// Resolve a tool's path argument against an `ssh://` cwd.
pub fn resolve(cwd: &str, p: &str) -> Result<SshPath, String> {
    host::resolve(cwd, p.trim())
}

/// Where `p` physically is on its host (symlinks resolved), as a path on
/// the same host. Read-before-write keys and write containment use this.
pub async fn real(p: &SshPath) -> Result<SshPath, String> {
    Ok(SshPath {
        host: p.host.clone(),
        path: ss::realpath(&p.host, &p.path).await?,
    })
}

/// A file's bytes and stat, refusing anything over `max`.
pub async fn read_all(p: &SshPath, max: u64) -> Result<(Vec<u8>, ss::Stat), String> {
    let (bytes, st) = ss::read(&p.host, &p.path, max + 1).await?;
    if bytes.len() as u64 > max || st.size > max {
        return Err(format!(
            "{} is {} (over the {} limit for this tool).",
            p.path,
            tools::human_bytes(st.size.max(bytes.len() as u64)),
            tools::human_bytes(max)
        ));
    }
    Ok((bytes, st))
}

/// `read_file` on a remote file: the formatted text, its line count, and the
/// bytes' content hash and mtime in ms (for read-before-write), both taken
/// from the same open handle as the bytes.
pub async fn read_file(
    p: &SshPath,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<(String, usize, String, u64), String> {
    let (bytes, st) = read_all(p, MAX_TEXT_BYTES).await.map_err(|e| {
        if e.ends_with("is a directory") {
            format!("{} is a directory. Use glob or bash ls to list it.", p.path)
        } else {
            format!("Cannot read {}: {e}", p.path)
        }
    })?;
    let (out, n) = tools::format_text(&bytes, &p.path, offset, limit)?;
    Ok((out, n, tools::content_hash(&bytes), mtime_ms(&st)))
}

/// SFTP mtime (whole seconds) in the milliseconds `read_files` stores.
pub fn mtime_ms(st: &ss::Stat) -> u64 {
    st.mtime_secs.max(0) as u64 * 1000
}

/// `view_image` on a remote file.
pub async fn view_image(p: &SshPath) -> Result<tools::ImageData, String> {
    let st = ss::stat(&p.host, &p.path)
        .await
        .map_err(|e| format!("Cannot read {}: {e}", p.path))?;
    if st.is_dir {
        return Err(format!(
            "{} is a directory. Use glob to find image files inside it.",
            p.path
        ));
    }
    if st.size > tools::MAX_IMAGE_BYTES {
        return Err(format!(
            "{} is {} (over the {} limit). Resize or compress it first, then view it.",
            p.path,
            tools::human_bytes(st.size),
            tools::human_bytes(tools::MAX_IMAGE_BYTES)
        ));
    }
    let (bytes, _) = ss::read(&p.host, &p.path, tools::MAX_IMAGE_BYTES + 1)
        .await
        .map_err(|e| format!("Cannot read {}: {e}", p.path))?;
    tools::model_image_from_bytes(&p.path, &ext_of(&p.path), bytes)
}

/// The extension of a POSIX path's file name, without the dot.
pub fn ext_of(path: &str) -> String {
    let name = host::file_name(path);
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_string(),
        _ => String::new(),
    }
}

/// The shell snippet that lists files under the cwd, one relative path per
/// line: ripgrep (respects .gitignore), else git's own view of the tree,
/// else `find` with the same prunes.
pub fn list_script() -> String {
    let rg_globs: String = PRUNE.iter().map(|d| format!(" -g '!{d}'")).collect();
    let find_prunes = PRUNE
        .iter()
        .map(|d| format!("-name '{d}'"))
        .collect::<Vec<_>>()
        .join(" -o ");
    format!(
        "if command -v rg >/dev/null 2>&1; then rg --files --hidden{rg_globs} 2>/dev/null; \
elif command -v git >/dev/null 2>&1 && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then git ls-files -co --exclude-standard 2>/dev/null; \
else find . \\( {find_prunes} \\) -prune -o -type f -print 2>/dev/null; fi; exit 0"
    )
}

/// Clean one line of `list_script` output into a relative path, dropping
/// anything under a pruned directory (git's listing doesn't prune them).
pub fn clean_listed(line: &str) -> Option<String> {
    let l = line.trim_end_matches('\r');
    let l = l.strip_prefix("./").unwrap_or(l);
    if l.is_empty() || l.starts_with('/') || l.split('/').any(|c| c == ".." || PRUNE.contains(&c)) {
        return None;
    }
    Some(l.to_string())
}

async fn list_files(base: &SshPath, cancel: &CancellationToken) -> Result<Vec<String>, String> {
    let out = ss::exec_machine(
        &base.host,
        &base.path,
        &list_script(),
        SEARCH_TIMEOUT,
        cancel,
    )
    .await?;
    if out.interrupted {
        return Err("Interrupted by user.".into());
    }
    if out.timed_out {
        return Err("Listing files on the remote host timed out.".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v: Vec<String> = text.lines().filter_map(clean_listed).collect();
    v.sort();
    v.dedup();
    Ok(v)
}

/// `glob` on a remote directory: the file list comes from the host, the
/// matching is `tools::glob_matcher`, and the newest-first order uses each
/// hit's SFTP mtime.
pub async fn glob(
    base: &SshPath,
    pattern: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let g = tools::glob_matcher(pattern)?;
    let files = list_files(base, cancel).await?;
    let matched: Vec<String> = files.into_iter().filter(|f| g.is_match(f)).collect();
    // mtimes only matter for the ones that can be shown; stat a bounded set.
    let stats = futures_util::future::join_all(matched.iter().take(1000).map(|rel| {
        let p = base.join(rel);
        async move {
            ss::stat(&p.host, &p.path)
                .await
                .map(|s| s.mtime_secs)
                .unwrap_or(0)
        }
    }))
    .await;
    let hits = matched
        .into_iter()
        .enumerate()
        .map(|(i, rel)| (stats.get(i).copied().unwrap_or(0).max(0) as u64 * 1000, rel))
        .collect();
    Ok(tools::format_glob(hits))
}

/// The command that names the files under the cwd that may match `pattern`.
/// ripgrep's regex dialect is Rust's, the same as the final match; `grep -E`
/// is POSIX ERE, so with it a pattern using Rust-only syntax (`\d`, `(?i)`,
/// lookaround-free classes like `\w` on some greps) may find fewer files.
pub fn candidates_script(pattern: &str, ci: bool, has_rg: bool) -> String {
    let q = host::sh_quote(pattern);
    if has_rg {
        let globs: String = PRUNE.iter().map(|d| format!(" -g '!{d}'")).collect();
        format!(
            "rg -l --hidden --no-messages --max-filesize 3M{}{globs} -e {q} -- . ; s=$?; [ $s -le 1 ] && exit 0; exit $s",
            if ci { " -i" } else { "" }
        )
    } else {
        let excl: String = PRUNE
            .iter()
            .map(|d| format!(" --exclude-dir='{d}'"))
            .collect();
        format!(
            "grep -rlIE{}{excl} -e {q} -- . 2>/dev/null; s=$?; [ $s -le 1 ] && exit 0; exit $s",
            if ci { "i" } else { "" }
        )
    }
}

/// `grep` on a remote path. The host narrows the files down (`rg -l`, or
/// `grep -rlIE` without ripgrep); each candidate is then read over SFTP and
/// matched with the same `tools::Grep` as a local search, so the output is
/// identical. Returns the output, the match count and, when `grep -E` was
/// used, a note on the dialect difference.
pub async fn grep(
    base: &SshPath,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<(String, usize), String> {
    let mut g = tools::Grep::parse(input)?;
    let st = ss::stat(&base.host, &base.path).await?;
    if st.is_file {
        if st.size <= tools::GREP_MAX_FILE {
            let (bytes, _) = ss::read(&base.host, &base.path, tools::GREP_MAX_FILE).await?;
            let rel = host::file_name(&base.path);
            if g.wants(&rel) {
                g.feed(&rel, &bytes);
            }
        }
        return Ok(g.finish());
    }
    let caps = ss::probe(&base.host).await?;
    let pattern = input["pattern"].as_str().unwrap_or("");
    let ci = input["case_insensitive"].as_bool().unwrap_or(false);
    let out = ss::exec_machine(
        &base.host,
        &base.path,
        &candidates_script(pattern, ci, caps.has_rg),
        SEARCH_TIMEOUT,
        cancel,
    )
    .await?;
    if out.interrupted {
        return Err("Interrupted by user.".into());
    }
    if out.timed_out {
        return Err("Searching on the remote host timed out.".into());
    }
    let mut note = String::new();
    let files: Vec<String> = if out.code == Some(0) {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut v: Vec<String> = text.lines().filter_map(clean_listed).collect();
        v.sort();
        v.dedup();
        v
    } else {
        // The host's grep couldn't take the pattern: search everything
        // (bounded) with the Rust regex instead.
        note = "\n[The remote grep rejected this pattern; searched the first 2000 files directly.]"
            .into();
        list_files(base, cancel)
            .await?
            .into_iter()
            .take(2000)
            .collect()
    };
    if !caps.has_rg && note.is_empty() {
        note = "\n[No ripgrep on the remote host: files were preselected with `grep -E` (POSIX ERE), so patterns using Rust-only regex syntax may miss files.]".into();
    }
    for rel in files {
        if cancel.is_cancelled() {
            return Err("Interrupted by user.".into());
        }
        if !g.wants(&rel) {
            continue;
        }
        let p = base.join(&rel);
        let Ok((bytes, st)) = ss::read(&p.host, &p.path, tools::GREP_MAX_FILE + 1).await else {
            continue;
        };
        if st.size > tools::GREP_MAX_FILE || bytes.len() as u64 > tools::GREP_MAX_FILE {
            continue;
        }
        if !g.feed(&rel, &bytes) {
            break;
        }
    }
    let (s, n) = g.finish();
    Ok(if n == 0 { (s, 0) } else { (s + &note, n) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ssh_session::tests::{install_fs, sample_fs};

    #[test]
    fn listed_paths_are_relative_and_pruned() {
        assert_eq!(clean_listed("./src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(clean_listed("src/a.rs\r").as_deref(), Some("src/a.rs"));
        for bad in [
            "",
            "/etc/passwd",
            "node_modules/x/y.js",
            "a/.git/HEAD",
            "../x",
            "target/debug/a",
        ] {
            assert!(clean_listed(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn search_scripts_quote_the_pattern() {
        for pat in ["a'b", "$(touch /tmp/x)", "`id`", "a;b|c&&d", "-e", "x\ny"] {
            for rg in [true, false] {
                let s = candidates_script(pat, false, rg);
                assert!(s.contains(&format!("-e {}", host::sh_quote(pat))), "{s}");
            }
        }
        assert!(candidates_script("x", true, false).contains("grep -rlIEi"));
    }

    #[tokio::test]
    async fn remote_reads_use_the_shared_formatting() {
        let h = crate::agent::ssh_session::tests::host_id();
        install_fs(&h, sample_fs());
        let p = SshPath {
            host: h.clone(),
            path: "/proj/a.txt".into(),
        };
        let (out, n, hash, _) = read_file(&p, None, None).await.unwrap();
        let (want, wn) = tools::format_text(b"hello\nworld\n", "/proj/a.txt", None, None).unwrap();
        assert_eq!((out, n), (want, wn));
        assert_eq!(hash, tools::content_hash(b"hello\nworld\n"));
        let e = read_file(
            &SshPath {
                host: h.clone(),
                path: "/proj/src".into(),
            },
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(e.contains("is a directory"), "{e}");
        assert_eq!(ext_of("/a/b.PNG"), "PNG");
        assert_eq!(ext_of("/a/.png"), "");
        let e = view_image(&p).await.err().unwrap();
        assert!(e.contains("doesn't look like a viewable image"), "{e}");
    }
}
