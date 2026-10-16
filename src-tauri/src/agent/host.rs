//! Addressing and shell-building for remote-SSH projects.
//!
//! A remote project's root is recorded as `ssh://<host-id>/<absolute posix
//! path>`, where `<host-id>` is the `SshHost.id` of a host saved in Settings
//! (never the ssh target itself: the target is looked up at connection time,
//! so editing a host's address doesn't orphan its projects, and nothing an
//! agent writes can choose what `ssh` connects to).
//!
//! Everything here is pure and synchronous so the security-relevant parts —
//! path containment and shell quoting — can be tested exhaustively without
//! a network. `ssh_session.rs` does the I/O.
//!
//! Paths are POSIX and handled lexically. A lexical check alone is not a
//! containment proof on a filesystem with symlinks: callers must compare the
//! *realpath* (`ssh_session::realpath`) of a path against the project root,
//! and then operate on exactly the string they checked.

/// The project-URI scheme, lowercase. Parsing accepts any case.
pub const SCHEME: &str = "ssh://";

/// The line a remote job prints (on stdout, before the user's command runs)
/// to report its process group, so a timeout or kill can end the whole
/// remote tree rather than only the local `ssh` client.
pub const PGID_MARKER: &str = "__OPENLEASH_PGID__=";

/// A path on a saved SSH host. `path` is always absolute and normalized.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SshPath {
    pub host: String,
    pub path: String,
}

impl SshPath {
    pub fn uri(&self) -> String {
        ssh_uri(&self.host, &self.path)
    }

    /// `rel` joined onto this path (an absolute `rel` replaces it).
    pub fn join(&self, rel: &str) -> SshPath {
        SshPath {
            host: self.host.clone(),
            path: join(&self.path, rel),
        }
    }
}

/// Bytes allowed unescaped in the host part. Saved host ids are 10 hex
/// characters today, but nothing guarantees that for hand-edited settings.
fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Strict decode: only unreserved bytes and well-formed `%XX` escapes, so
/// every host has exactly one spelling and comparisons can't be fooled.
fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else if unreserved(b[i]) {
            out.push(b[i]);
            i += 1;
        } else {
            return None;
        }
    }
    String::from_utf8(out).ok()
}

fn bad_chars(s: &str) -> bool {
    s.contains(['\0', '\n', '\r'])
}

/// `ssh://<host>/<path>` for a host id and an absolute path.
pub fn ssh_uri(host: &str, path: &str) -> String {
    format!("{SCHEME}{}{}", pct_encode(host), normalize(path))
}

pub fn is_ssh(s: &str) -> bool {
    s.get(..SCHEME.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(SCHEME))
}

/// Parse a project URI. `ssh://host` alone means the host's `/`. The path
/// is normalized; a NUL, CR or LF anywhere makes the whole URI invalid.
pub fn parse_ssh(s: &str) -> Option<SshPath> {
    if !is_ssh(s) || bad_chars(s) {
        return None;
    }
    let rest = &s[SCHEME.len()..];
    let (h, p) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = pct_decode(h)?;
    if host.is_empty() || host.chars().any(char::is_control) {
        return None;
    }
    Some(SshPath {
        host,
        path: normalize(p),
    })
}

/// Lexical POSIX normalization: collapse `//` and `.`, apply `..`, and
/// never climb above `/`. The result is always absolute (a relative input
/// is taken as relative to `/`).
pub fn normalize(p: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    format!("/{}", out.join("/"))
}

/// `rel` resolved against the absolute directory `base`.
pub fn join(base: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        normalize(rel)
    } else {
        normalize(&format!("{base}/{rel}"))
    }
}

/// The parent of a normalized absolute path (`/` is its own parent).
pub fn parent(p: &str) -> String {
    let p = normalize(p);
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// The last component of a normalized path, or `""` for `/`.
pub fn file_name(p: &str) -> String {
    let p = normalize(p);
    p.rsplit('/').next().unwrap_or("").to_string()
}

fn has_scheme(p: &str) -> bool {
    if p.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("file:")) {
        return true;
    }
    // `scheme://` per RFC 3986. A bare `name:thing` is a legal file name.
    match p.find("://") {
        Some(i) if i > 0 => {
            let s = &p[..i];
            s.as_bytes()[0].is_ascii_alphabetic()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
        }
        _ => false,
    }
}

fn is_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('\\')
        || (b.len() >= 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/'))
}

/// Resolve a path an agent or user gave for a remote project whose current
/// directory is `cwd_uri`. Accepts a path relative to the cwd, an absolute
/// POSIX path, or an `ssh://` URI on the *same* host. Anything that names
/// another machine, another host, or a local file is refused rather than
/// guessed at.
pub fn resolve(cwd_uri: &str, p: &str) -> Result<SshPath, String> {
    let cwd = parse_ssh(cwd_uri)
        .ok_or_else(|| format!("{cwd_uri} is not a remote (ssh://) project path."))?;
    if bad_chars(p) {
        return Err("Paths can't contain NUL or line-break characters.".into());
    }
    if is_ssh(p) {
        let t = parse_ssh(p).ok_or_else(|| format!("{p} is not a valid ssh:// path."))?;
        if t.host != cwd.host {
            return Err(format!("{p} is on a different SSH host than this project."));
        }
        return Ok(t);
    }
    if has_scheme(p) {
        return Err(format!(
            "{p} is not on this project's SSH host. Use a path on the remote machine."
        ));
    }
    if is_windows_path(p) {
        return Err(format!(
            "{p} looks like a Windows path, but this project is on a remote POSIX machine. Use a path like /home/... or one relative to the project."
        ));
    }
    if p == "~" || p.starts_with("~/") {
        return Err(
            "`~` isn't expanded here. Use an absolute path or one relative to the project.".into(),
        );
    }
    Ok(cwd.join(p))
}

/// Fail closed for a feature that only knows local folders: `Err` with a
/// clear message when `p` is an `ssh://` project path. Used at the entry of
/// every local-only subsystem (checkpoints, worktrees, artificts, docker) so
/// an `ssh://` string can never reach `Path`/`PathBuf` there — on Windows it
/// would silently be a *relative local* path.
pub fn refuse(p: &str, what: &str) -> Result<(), String> {
    if is_ssh(p) {
        Err(format!("{what} not yet supported for SSH projects."))
    } else {
        Ok(())
    }
}

/// Lexical containment: `path` is `root` or below it. Both are normalized
/// first. This says nothing about symlinks; see the module docs.
pub fn inside(root: &str, path: &str) -> bool {
    let (root, path) = (normalize(root), normalize(path));
    root == "/" || path == root || path.strip_prefix(&root).is_some_and(|r| r.starts_with('/'))
}

/// POSIX single-quoting: the result is one shell word whose value is `s`,
/// with nothing inside interpreted. A `'` becomes `'\''`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quoting for the one word the remote *login* shell parses. sshd hands the
/// command to the user's shell, which may be fish rather than POSIX; fish
/// also unescapes `\\` inside single quotes, so backslashes are taken
/// outside the quotes too (`\\` means `\` in both).
fn login_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str(r"'\''"),
            '\\' => out.push_str(r"'\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

pub fn valid_env_key(k: &str) -> bool {
    let mut b = k.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// The POSIX script that runs `cmd` in `cwd` with `env` set: `bash -lc`
/// (the user's login PATH, like their terminal) or `sh -lc` when there is
/// no bash. Fails on an invalid variable name or a NUL, which no argument
/// can carry.
pub fn remote_script(cwd: &str, env: &[(&str, &str)], cmd: &str) -> Result<String, String> {
    if cwd.contains('\0') || cmd.contains('\0') {
        return Err("Commands and paths can't contain NUL characters.".into());
    }
    let mut assigns = String::new();
    for (k, v) in env {
        if !valid_env_key(k) {
            return Err(format!("Invalid environment variable name: {k:?}"));
        }
        if v.contains('\0') {
            return Err(format!("The value of {k} contains a NUL character."));
        }
        assigns.push_str(&format!("{k}={} ", sh_quote(v)));
    }
    // `--` before the command string: a command starting with `-` would
    // otherwise be read as shell options.
    let q = sh_quote(cmd);
    Ok(format!(
        "cd -- {cwd} && if command -v bash >/dev/null 2>&1; then exec env {assigns}bash -lc -- {q}; else exec env {assigns}sh -lc -- {q}; fi",
        cwd = sh_quote(cwd),
    ))
}

/// A POSIX `script` (with optional positional `args`) as the single command
/// string for `ssh -- <target> <command>`. The login shell only ever sees
/// `exec sh -c '<…>' sh '<…>'…`, whatever shell it is.
pub fn login_command(script: &str, args: &[&str]) -> String {
    let mut s = format!("exec sh -c {} sh", login_quote(script));
    for a in args {
        s.push(' ');
        s.push_str(&login_quote(a));
    }
    s
}

/// Makes the job its own process-group leader before running `$1`. sshd
/// already starts a no-pty session with `setsid()`, and the `exec` chain
/// keeps that pid, so normally this is a no-op; if the group differs (the
/// login shell forked) and `setsid` exists, it re-execs under `setsid`,
/// which doesn't fork for a non-leader, so the exit status still flows
/// back. Group detection: `/proc` first (comm is `(sh)`, no spaces), then
/// `ps`; if neither works, trust sshd.
const LAUNCHER: &str = r#"pg=; if [ -r /proc/$$/stat ]; then read -r _ _ _ _ pg _ < /proc/$$/stat; else pg=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' '); fi; if [ -n "$pg" ] && [ "$pg" != "$$" ] && command -v setsid >/dev/null 2>&1; then exec setsid sh -c "$1" sh; fi; exec sh -c "$1" sh"#;

/// The full ssh command string for a job: launcher → report pgid on stdout
/// (`PGID_MARKER<n>`) → `remote_script`.
pub fn job_command(cwd: &str, env: &[(&str, &str)], cmd: &str) -> Result<String, String> {
    let inner = format!(
        "printf '%s%s\\n' {} \"$$\"; {}",
        sh_quote(PGID_MARKER),
        remote_script(cwd, env, cmd)?
    );
    Ok(login_command(LAUNCHER, &[&inner]))
}

/// Parse a marker line (with or without its line ending). A pgid of 0 or 1
/// is refused: `kill -- -0` / `-1` would signal the caller's own group or
/// every process the user owns.
pub fn parse_pgid_line(line: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(line).ok()?;
    let n = s.trim_end_matches(['\n', '\r']).strip_prefix(PGID_MARKER)?;
    if n.is_empty() || n.len() > 10 || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse::<u32>().ok().filter(|&p| p > 1)
}

/// ssh command string that TERMs remote process group `pgid`, waits up to
/// `grace_secs` for it to go, then KILLs it. Two spellings of each kill:
/// dash only accepts `--` after `-s SIG`, busybox wants `-SIG -<pgid>`.
pub fn kill_command(pgid: u32, grace_secs: u32) -> Option<String> {
    if pgid <= 1 {
        return None;
    }
    let g = grace_secs.min(60);
    let script = format!(
        "kill -s TERM -- -{pgid} 2>/dev/null || kill -TERM -{pgid} 2>/dev/null || exit 0; i=0; while [ $i -lt {g} ] && {{ kill -s 0 -- -{pgid} 2>/dev/null || kill -0 -{pgid} 2>/dev/null; }}; do sleep 1; i=$((i+1)); done; kill -s KILL -- -{pgid} 2>/dev/null || kill -KILL -{pgid} 2>/dev/null; exit 0"
    );
    Some(login_command(&script, &[]))
}

/// What a remote host offers; from `probe_script`'s output.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Caps {
    /// `uname -s`: `Linux`, `Darwin`, `FreeBSD`, …
    pub os: String,
    pub has_bash: bool,
    pub has_rg: bool,
    pub has_git: bool,
    pub has_setsid: bool,
    pub has_realpath: bool,
    pub home: String,
}

/// Run as the *command* of `remote_script` (so tool lookup sees the same
/// login PATH real commands get). Each answer is on its own tagged line so
/// stray output from shell startup files can't be mistaken for one.
pub const PROBE_SCRIPT: &str = r#"printf '__ol_os=%s\n' "$(uname -s 2>/dev/null)"; printf '__ol_home=%s\n' "$HOME"; for t in bash rg git setsid realpath; do if command -v "$t" >/dev/null 2>&1; then printf '__ol_%s=1\n' "$t"; else printf '__ol_%s=0\n' "$t"; fi; done"#;

pub fn parse_probe(out: &str) -> Caps {
    let mut c = Caps::default();
    for line in out.lines() {
        let Some((k, v)) = line
            .trim_end_matches('\r')
            .strip_prefix("__ol_")
            .and_then(|l| l.split_once('='))
        else {
            continue;
        };
        let yes = v == "1";
        match k {
            "os" => c.os = v.to_string(),
            "home" => c.home = v.to_string(),
            "bash" => c.has_bash = yes,
            "rg" => c.has_rg = yes,
            "git" => c.has_git = yes,
            "setsid" => c.has_setsid = yes,
            "realpath" => c.has_realpath = yes,
            _ => {}
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: &[&str] = &[
        "plain",
        "it's",
        "''",
        "'\\''",
        "$(touch /tmp/pwned)",
        "`id`",
        "a; rm -rf /",
        "a && b",
        "a || b",
        "a | b",
        "line1\nline2",
        "--opt",
        "-c",
        "\\",
        "back\\slash\\\\",
        "\"double\" $HOME ${HOME}",
        "glob * ? [a]",
        "tab\there",
        "ünïcødé ✓ 日本",
        "",
        "!event",
        "#comment",
        "~",
        "a\rb",
    ];

    #[test]
    fn uri_round_trips_and_normalizes() {
        assert_eq!(
            ssh_uri("a1b2c3d4e5", "/home/me/proj"),
            "ssh://a1b2c3d4e5/home/me/proj"
        );
        let p = parse_ssh("ssh://a1b2c3d4e5/home/me/proj").unwrap();
        assert_eq!(
            p,
            SshPath {
                host: "a1b2c3d4e5".into(),
                path: "/home/me/proj".into()
            }
        );
        assert_eq!(p.uri(), "ssh://a1b2c3d4e5/home/me/proj");
        assert_eq!(parse_ssh("SSH://h/x/./y//z/").unwrap().path, "/x/y/z");
        assert_eq!(parse_ssh("ssh://h").unwrap().path, "/");
        assert_eq!(parse_ssh("ssh://h/../../etc").unwrap().path, "/etc");
        assert_eq!(
            parse_ssh("ssh://h/with space/ünï").unwrap().path,
            "/with space/ünï"
        );
        assert!(is_ssh("Ssh://x/") && !is_ssh("ssh:/x") && !is_ssh("/ssh://x") && !is_ssh("é"));
    }

    #[test]
    fn odd_host_ids_are_percent_encoded_with_one_spelling() {
        for host in ["me@box:22", "a/b", "sp ace", "pct%", "ünï", "a?b#c"] {
            let uri = ssh_uri(host, "/p");
            assert!(
                !uri[SCHEME.len()..]
                    .split('/')
                    .next()
                    .unwrap()
                    .contains(['@', ':', ' ', '?', '#']),
                "{uri}"
            );
            let back = parse_ssh(&uri).unwrap();
            assert_eq!(back.host, host);
            assert_eq!(back.path, "/p");
        }
        // Raw reserved characters, bad escapes and empty hosts are refused.
        for bad in [
            "ssh://me@box/p",
            "ssh://a:b/p",
            "ssh://%zz/p",
            "ssh://%4/p",
            "ssh:///p",
            "ssh://",
            "ssh://%00/p",
            "ssh://h/p\0",
            "ssh://h/p\nx",
            "ssh://h/p\rx",
            "ssh://%FF/p",
        ] {
            assert!(parse_ssh(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn normalize_never_climbs_above_root() {
        for (i, o) in [
            ("/", "/"),
            ("", "/"),
            ("//", "/"),
            ("/a/b/../c", "/a/c"),
            ("/a/./b/", "/a/b"),
            ("/../../..", "/"),
            ("/a/../../b", "/b"),
            ("a/b", "/a/b"),
            ("/a//b///c", "/a/b/c"),
            ("/a/...", "/a/..."),
            ("/a/..b", "/a/..b"),
        ] {
            assert_eq!(normalize(i), o, "{i}");
        }
        assert_eq!(join("/p", "src/x.rs"), "/p/src/x.rs");
        assert_eq!(join("/p", "/etc/hosts"), "/etc/hosts");
        assert_eq!(join("/p/q", "../../../../x"), "/x");
        assert_eq!(parent("/a/b"), "/a");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(file_name("/a/b.txt"), "b.txt");
        assert_eq!(file_name("/"), "");
    }

    #[test]
    fn resolve_accepts_same_host_forms() {
        let cwd = "ssh://h1/home/me/proj";
        assert_eq!(
            resolve(cwd, "src/main.rs").unwrap().path,
            "/home/me/proj/src/main.rs"
        );
        assert_eq!(resolve(cwd, "").unwrap().path, "/home/me/proj");
        assert_eq!(resolve(cwd, ".").unwrap().path, "/home/me/proj");
        assert_eq!(resolve(cwd, "/etc/hosts").unwrap().path, "/etc/hosts");
        assert_eq!(
            resolve(cwd, "ssh://h1/tmp/x").unwrap(),
            SshPath {
                host: "h1".into(),
                path: "/tmp/x".into()
            }
        );
        assert_eq!(resolve(cwd, "SSH://h1/tmp/../x").unwrap().path, "/x");
        // Odd but legal POSIX names are not mistaken for schemes or drives.
        assert_eq!(
            resolve(cwd, "name:thing").unwrap().path,
            "/home/me/proj/name:thing"
        );
        assert!(resolve(cwd, "c:").unwrap_err().contains("Windows"));
        assert_eq!(resolve(cwd, "cc:/x").unwrap().path, "/home/me/proj/cc:/x");
        assert_eq!(resolve(cwd, "~user").unwrap().path, "/home/me/proj/~user");
        assert_eq!(resolve(cwd, "--opt").unwrap().path, "/home/me/proj/--opt");
        assert_eq!(resolve(cwd, "$(id)").unwrap().path, "/home/me/proj/$(id)");
    }

    #[test]
    fn resolve_rejects_other_hosts_local_paths_and_control_chars() {
        let cwd = "ssh://h1/home/me/proj";
        for bad in [
            "ssh://h2/home/me/proj",
            "ssh://h1x/x",
            "ssh://me@h1/x",
            "file:///etc/passwd",
            "FILE:/etc/passwd",
            "http://example.com/x",
            "sftp://h1/x",
            "C:\\Users\\me",
            "c:/Users/me",
            "D:",
            "\\\\server\\share",
            "\\\\wsl.localhost\\Ubuntu\\home",
            "\\x",
            "~",
            "~/x",
            "a\0b",
            "a\nb",
            "a\rb",
        ] {
            assert!(resolve(cwd, bad).is_err(), "{bad:?}");
        }
        assert!(resolve("/home/me/proj", "x").is_err());
        assert!(resolve("C:\\proj", "x").is_err());
    }

    #[test]
    fn traversal_is_lexically_contained_only_when_it_stays_inside() {
        let root = "/home/me/proj";
        let cwd = "ssh://h/home/me/proj";
        let r = |p| resolve(cwd, p).unwrap().path;
        assert!(inside(root, &r("a/b")));
        assert!(inside(root, &r("a/../b")));
        assert!(inside(root, &r(".")));
        assert!(!inside(root, &r("..")));
        assert!(!inside(root, &r("../proj2/x")));
        assert!(!inside(root, &r("../../../../../../etc/passwd")));
        assert!(!inside(root, &r("/etc/passwd")));
        assert!(!inside(root, "/home/me/projx"));
        assert!(inside(root, "/home/me/proj/"));
        assert!(inside("/", "/anything"));
        assert!(!inside("/a/b", "/a"));
    }

    #[test]
    fn sh_quote_is_one_inert_word() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        for s in HOSTILE {
            let q = sh_quote(s);
            assert!(q.starts_with('\'') && q.ends_with('\''));
            // Outside the quotes there is nothing but escaped quotes.
            let unwrapped = &q[1..q.len() - 1];
            assert!(
                !unwrapped.replace(r"'\''", "").contains('\''),
                "{s:?} → {q}"
            );
        }
    }

    #[test]
    fn env_keys_are_validated() {
        for ok in ["A", "_x", "GIT_TERMINAL_PROMPT", "a1"] {
            assert!(valid_env_key(ok), "{ok}");
        }
        for bad in ["", "1A", "A-B", "A B", "A=B", "$(x)", "A;B", "-x", "ü"] {
            assert!(!valid_env_key(bad), "{bad}");
            assert!(remote_script("/", &[(bad, "v")], "true").is_err(), "{bad}");
        }
        assert!(remote_script("/", &[("A", "a\0")], "true").is_err());
        assert!(remote_script("/", &[], "a\0").is_err());
        assert!(remote_script("/\0", &[], "true").is_err());
    }

    #[test]
    fn remote_script_shape() {
        let s = remote_script(
            "/home/me/my proj",
            &[("CI", "1"), ("X", "it's $(id)")],
            "cargo test; echo `id`",
        )
        .unwrap();
        assert_eq!(
            s,
            r#"cd -- '/home/me/my proj' && if command -v bash >/dev/null 2>&1; then exec env CI='1' X='it'\''s $(id)' bash -lc -- 'cargo test; echo `id`'; else exec env CI='1' X='it'\''s $(id)' sh -lc -- 'cargo test; echo `id`'; fi"#
        );
        let s = remote_script("/", &[], "--version").unwrap();
        assert!(s.contains("bash -lc -- '--version'"), "{s}");
    }

    #[test]
    fn login_quote_survives_fish_and_posix_rules() {
        // Never a backslash inside single quotes (fish would unescape it),
        // and every quote is the `'\''` idiom.
        for s in HOSTILE {
            let q = login_quote(s);
            let mut in_q = false;
            let mut prev_bs = false;
            for c in q.chars() {
                if c == '\'' && !prev_bs {
                    in_q = !in_q;
                } else if c == '\\' && in_q {
                    panic!("backslash inside quotes for {s:?}: {q}");
                }
                prev_bs = c == '\\' && !in_q && !prev_bs;
            }
            assert!(!in_q, "{s:?} → {q}");
        }
        assert_eq!(login_quote(r"a\b"), r"'a'\\'b'");
        assert_eq!(
            login_command("echo hi", &["x y"]),
            "exec sh -c 'echo hi' sh 'x y'"
        );
    }

    #[test]
    fn job_command_reports_pgid_before_the_command() {
        let c = job_command("/p", &[], "make").unwrap();
        assert!(c.starts_with("exec sh -c 'pg=;"), "{c}");
        let marker = c.find(PGID_MARKER).unwrap();
        let cd = c.find("cd -- ").unwrap();
        assert!(marker < cd, "{c}");
        assert!(job_command("/p", &[("BAD-KEY", "v")], "make").is_err());
    }

    #[test]
    fn pgid_lines_are_strict() {
        assert_eq!(parse_pgid_line(b"__OPENLEASH_PGID__=4242\n"), Some(4242));
        assert_eq!(parse_pgid_line(b"__OPENLEASH_PGID__=4242\r\n"), Some(4242));
        assert_eq!(parse_pgid_line(b"__OPENLEASH_PGID__=2"), Some(2));
        for bad in [
            &b"__OPENLEASH_PGID__=0\n"[..],
            b"__OPENLEASH_PGID__=1\n",
            b"__OPENLEASH_PGID__=-5\n",
            b"__OPENLEASH_PGID__=\n",
            b"__OPENLEASH_PGID__=12 34\n",
            b"__OPENLEASH_PGID__=99999999999\n",
            b" __OPENLEASH_PGID__=12\n",
            b"motd: welcome\n",
            b"__OPENLEASH_PGID__=12;rm\n",
        ] {
            assert_eq!(
                parse_pgid_line(bad),
                None,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn kill_never_targets_pgid_0_or_1() {
        assert!(kill_command(0, 5).is_none());
        assert!(kill_command(1, 5).is_none());
        let k = kill_command(4242, 5).unwrap();
        assert!(
            k.contains("kill -s TERM -- -4242") && k.contains("kill -s KILL -- -4242"),
            "{k}"
        );
        assert!(k.contains("[ $i -lt 5 ]"), "{k}");
        assert!(kill_command(4242, 9999).unwrap().contains("[ $i -lt 60 ]"));
    }

    #[test]
    fn probe_output_ignores_noise() {
        let out = "Welcome to box!\n__ol_os=Linux\r\n__ol_home=/home/me\n__ol_bash=1\n__ol_rg=0\n__ol_git=1\n__ol_setsid=1\n__ol_realpath=1\nos=Evil\n";
        let c = parse_probe(out);
        assert_eq!(
            c,
            Caps {
                os: "Linux".into(),
                home: "/home/me".into(),
                has_bash: true,
                has_rg: false,
                has_git: true,
                has_setsid: true,
                has_realpath: true,
            }
        );
    }

    /// `sh -c <command>`, the way sshd runs a remote command. The command
    /// goes through a file rather than argv: on Windows the test's own
    /// argv hop to an MSYS `sh` re-quotes `"` and `\`, which is not what
    /// is under test (sshd receives the command as one opaque string).
    fn run_like_sshd(command: &str) -> std::process::Output {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let f = std::env::temp_dir().join(format!(
            "ol-host-cmd-{}-{}.txt",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&f, command).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(r#"c=$(cat "$1"; printf x); exec sh -c "${c%x}""#)
            .arg("sh")
            .arg(f.to_string_lossy().replace('\\', "/"))
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&f);
        out
    }

    /// Run the generated command the way sshd would (`$SHELL -c <command>`)
    /// with a local POSIX `sh`, and check hostile values arrive byte-exact.
    /// Skipped where no `sh` is installed; no network, no ssh.
    #[test]
    fn job_command_round_trips_hostile_values_through_a_real_sh() {
        let has_sh = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_sh {
            eprintln!("skipping: no sh");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ol-host-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("it's a dir $(x)"));
        // Through the shell's own view of the path (msys on Windows).
        let posix = std::process::Command::new("sh")
            .arg("-c")
            .arg("cd -- \"$1\" && pwd")
            .arg("sh")
            .arg(dir.join("it's a dir $(x)"))
            .output()
            .unwrap();
        let cwd = String::from_utf8_lossy(&posix.stdout).trim().to_string();
        assert!(!cwd.is_empty());
        // Every hostile value at once (each `sh` start is slow on Windows):
        // as environment values, and quoted into the command string itself.
        let vals: Vec<&str> = HOSTILE
            .iter()
            .copied()
            .filter(|v| !v.contains('\r'))
            .collect();
        let keys: Vec<String> = (0..vals.len()).map(|i| format!("OL_V{i}")).collect();
        let env: Vec<(&str, &str)> = keys
            .iter()
            .map(String::as_str)
            .zip(vals.iter().copied())
            .collect();
        let mut cmd = "printf '@@'; ".to_string();
        for k in &keys {
            cmd.push_str(&format!("printf '<%s>' \"${k}\"; "));
        }
        cmd.push_str("printf '|'; printf '%s|'");
        for v in &vals {
            cmd.push(' ');
            cmd.push_str(&sh_quote(v));
        }
        cmd.push_str("; printf '\\n'; pwd");
        let c = job_command(&cwd, &env, &cmd).unwrap();
        let out = run_like_sshd(&c);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let (first, rest) = stdout.split_once('\n').unwrap_or_default();
        assert!(
            parse_pgid_line(first.as_bytes()).is_some(),
            "{stdout:?} / {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // `@@` skips whatever the user's login profile prints first.
        let rest = rest.split_once("@@").map(|x| x.1).unwrap_or("");
        let mut want: String = vals.iter().map(|v| format!("<{v}>")).collect();
        want.push('|');
        want.extend(vals.iter().map(|v| format!("{v}|")));
        assert_eq!(
            rest,
            format!("{want}\n{cwd}\n"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // A command starting with `-` is a command, not shell options.
        // (Without the `--`, `bash -lc --version` would print bash's version.)
        let c = job_command(&cwd, &[], "--version").unwrap();
        let out = run_like_sshd(&c);
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout)
            .to_lowercase()
            .contains("version"));
        // A failing cd stops before the command runs.
        let c = job_command("/definitely/not/here/ol", &[], "echo RAN").unwrap();
        let out = run_like_sshd(&c);
        assert!(!String::from_utf8_lossy(&out.stdout).contains("RAN"));
        assert!(!out.status.success());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
