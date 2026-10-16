//! Live connections to remote-SSH project hosts.
//!
//! Two channels, both through the user's own `ssh` client (so their
//! `~/.ssh/config`, keys, agent and `known_hosts` decide everything about
//! authentication, and OpenLeash never handles a credential):
//!
//! - **Files**: one persistent `ssh -s -- <target> sftp` child per host,
//!   speaking SFTP (`russh-sftp`) over its stdio. Windows' OpenSSH has no
//!   ControlMaster, so reusing this one process is what keeps file tools
//!   from paying a full SSH handshake per call. It is dropped and
//!   reconnected once on a transport failure.
//! - **Commands**: a fresh `ssh -- <target> <command>` per command. The
//!   command first reports its remote process group (`host::PGID_MARKER`)
//!   so a timeout, cancel or kill can end the whole remote tree: killing
//!   the local `ssh` alone leaves a no-pty remote process running.
//!
//! Hosts are named by `SshHost.id` and resolved to their ssh target from the
//! saved Remote settings at connection time.
//!
//! Paths given to the file API must be absolute and already normalized
//! (`host::resolve` produces such paths). That is enforced, not assumed:
//! a containment check is only meaningful if the string that was checked
//! (via `realpath`) is the exact string that is then opened.

use super::host;
pub use super::host::Caps;
use crate::remote::ssh::{ssh, valid_target, BASE_OPTS};
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{Config, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio_util::sync::CancellationToken;

/// Per-SFTP-request timeout. The crate's default (10 s) is short for a big
/// read over a slow link; a dead link is still noticed by ServerAlive.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// ssh's own ConnectTimeout is 15 s; leave room for auth and agent prompts.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(40);
/// How long a command may take to report its process group.
const MARKER_TIMEOUT: Duration = Duration::from_secs(40);
/// Output kept per stream for a foreground command; beyond this the rest is
/// drained and dropped so a runaway command can't exhaust memory.
const STREAM_CAP: usize = 64 << 20;
/// Junk a login shell may print before the marker (motd-like rc output).
const MARKER_SCAN_MAX: u64 = 64 << 10;
const KILL_GRACE_SECS: u32 = 3;
const SYMLINK_HOPS: usize = 40;
/// Set for every remote command, as `shell.rs` does locally; a caller's
/// value for the same name wins.
pub const DEFAULT_ENV: [(&str, &str); 3] =
    [("GIT_TERMINAL_PROMPT", "0"), ("NO_COLOR", "1"), ("CI", "1")];
/// No tty; and keep "Warning: Permanently added …" out of command stderr
/// (real errors are still printed at ERROR).
const CMD_OPTS: [&str; 3] = ["-T", "-o", "LogLevel=ERROR"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    pub size: u64,
    pub mtime_secs: i64,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    /// Permission bits only (`& 0o7777`); the type is in the flags above.
    pub mode: u32,
}

impl Stat {
    fn from_attrs(a: &FileAttributes) -> Stat {
        // Decode S_IFMT ourselves: the crate's `is_dir()` etc. test bits with
        // `contains`, which calls a symlink (0o120000) a regular file too.
        let m = a.permissions.unwrap_or(0);
        let t = m & 0o170000;
        Stat {
            size: a.size.unwrap_or(0),
            mtime_secs: a.mtime.map(i64::from).unwrap_or(0),
            is_dir: t == 0o040000,
            is_file: t == 0o100000,
            is_symlink: t == 0o120000,
            mode: m & 0o7777,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// From the listing's own (lstat) attributes: a symlink to a directory
    /// is `is_symlink`, not `is_dir`.
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// `None` when killed (timeout/cancel) or ended by a signal.
    pub code: Option<i32>,
    pub timed_out: bool,
    pub interrupted: bool,
}

/// Why a file operation failed. Only `Transport` triggers a reconnect.
#[derive(Debug, Clone)]
enum Fail {
    NotFound(String),
    Fs(String),
    Transport(String),
}

impl Fail {
    fn msg(self) -> String {
        match self {
            Fail::NotFound(m) | Fail::Fs(m) | Fail::Transport(m) => m,
        }
    }
}

fn classify(e: SftpError, what: &str) -> Fail {
    match e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => Fail::NotFound(format!("{what}: no such file or directory")),
            StatusCode::PermissionDenied => Fail::Fs(format!("{what}: permission denied")),
            StatusCode::NoConnection | StatusCode::ConnectionLost => {
                Fail::Transport(format!("{what}: {}", s.status_code))
            }
            code => {
                let m = s.error_message.trim();
                Fail::Fs(format!(
                    "{what}: {}",
                    if m.is_empty() {
                        code.to_string()
                    } else {
                        m.to_string()
                    }
                ))
            }
        },
        SftpError::Limited(m) => Fail::Fs(format!("{what}: {m}")),
        other => Fail::Transport(format!("{what}: {other}")),
    }
}

fn classify_io(e: std::io::Error, what: &str) -> Fail {
    if e.kind() == std::io::ErrorKind::TimedOut {
        return Fail::Transport(format!("{what}: timed out"));
    }
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<SftpError>()) {
        return classify(inner.clone(), what);
    }
    Fail::Fs(format!("{what}: {e}"))
}

fn check_path(p: &str) -> Result<(), Fail> {
    if !p.starts_with('/') || p.contains(['\0', '\n', '\r']) || host::normalize(p) != p {
        return Err(Fail::Fs(format!(
            "internal error: {p:?} is not a normalized absolute remote path"
        )));
    }
    Ok(())
}

fn spawn_err(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "No `ssh` command found. Install OpenSSH (on Windows: Settings → Optional features → OpenSSH Client).".into()
    } else {
        format!("couldn't run ssh: {e}")
    }
}

/// Keep the last few KB of a stream (ssh's stderr) for error messages.
fn pump_tail<R: AsyncRead + Unpin + Send + 'static>(mut r: R, buf: Arc<Mutex<String>>) {
    tokio::spawn(async move {
        let mut chunk = [0u8; 2048];
        while let Ok(n) = r.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let mut b = buf.lock().unwrap();
            b.push_str(&String::from_utf8_lossy(&chunk[..n]));
            if b.len() > 8192 {
                let cut = b.len() - 4096;
                let cut = (cut..b.len()).find(|i| b.is_char_boundary(*i)).unwrap_or(0);
                b.drain(..cut);
            }
        }
    });
}

/// An SFTP session over any byte stream: the stdio of an `ssh -s … sftp`
/// child in production, an in-process server in tests.
pub struct Sftp {
    session: SftpSession,
    /// The `ssh` client carrying the session, if any (killed on drop).
    child: Option<Mutex<Child>>,
    stderr: Arc<Mutex<String>>,
}

impl Sftp {
    pub async fn from_stream<S>(stream: S) -> Result<Sftp, String>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let session = new_session(stream)
            .await
            .map_err(|e| format!("Couldn't start SFTP: {e}"))?;
        Ok(Sftp {
            session,
            child: None,
            stderr: Arc::default(),
        })
    }

    /// Start `ssh -s -- <target> sftp` and open SFTP over its stdio.
    pub async fn connect(target: &str) -> Result<Sftp, String> {
        if !valid_target(target) {
            return Err("That SSH destination isn't valid. Use a host alias or user@host.".into());
        }
        let mut c = ssh();
        c.args(BASE_OPTS)
            .args(["-o", "LogLevel=ERROR", "-s", "--", target, "sftp"])
            .stdin(Stdio::piped());
        let mut child = c.spawn().map_err(spawn_err)?;
        let (Some(out), Some(inp)) = (child.stdout.take(), child.stdin.take()) else {
            return Err("couldn't run ssh: no stdio".into());
        };
        let errbuf: Arc<Mutex<String>> = Arc::default();
        if let Some(e) = child.stderr.take() {
            pump_tail(e, errbuf.clone());
        }
        let err = match new_session(tokio::io::join(out, inp)).await {
            Ok(session) => {
                return Ok(Sftp {
                    session,
                    child: Some(Mutex::new(child)),
                    stderr: errbuf,
                })
            }
            Err(e) => e,
        };
        // ssh usually explains itself on stderr as it exits.
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        let _ = child.start_kill();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let said = errbuf.lock().unwrap().trim().to_string();
        Err(if !said.is_empty() {
            format!("ssh {target}: {said}")
        } else {
            format!(
                "Couldn't start SFTP on {target} ({err}). The server must allow the sftp subsystem, and shell startup files must not print anything for non-interactive logins."
            )
        })
    }

    /// False once the carrying `ssh` process has exited.
    fn alive(&self) -> bool {
        match &self.child {
            Some(c) => matches!(c.lock().unwrap().try_wait(), Ok(None)),
            None => true,
        }
    }

    /// A transport failure, with what ssh said about it.
    fn annotate(&self, f: Fail) -> Fail {
        match f {
            Fail::Transport(m) => {
                let said = self.stderr.lock().unwrap().trim().to_string();
                Fail::Transport(if said.is_empty() {
                    m
                } else {
                    format!("{m} (ssh: {said})")
                })
            }
            f => f,
        }
    }

    async fn attrs(&self, path: &str, follow: bool) -> Result<FileAttributes, Fail> {
        check_path(path)?;
        let r = if follow {
            self.session.metadata(path).await
        } else {
            self.session.symlink_metadata(path).await
        };
        r.map_err(|e| self.annotate(classify(e, &format!("stat {path}"))))
    }

    pub async fn stat(&self, path: &str) -> Result<Stat, String> {
        self.stat_f(path).await.map_err(Fail::msg)
    }
    async fn stat_f(&self, path: &str) -> Result<Stat, Fail> {
        Ok(Stat::from_attrs(&self.attrs(path, true).await?))
    }

    pub async fn lstat(&self, path: &str) -> Result<Stat, String> {
        self.lstat_f(path).await.map_err(Fail::msg)
    }
    async fn lstat_f(&self, path: &str) -> Result<Stat, Fail> {
        Ok(Stat::from_attrs(&self.attrs(path, false).await?))
    }

    pub async fn exists(&self, path: &str) -> Result<bool, String> {
        self.exists_f(path).await.map_err(Fail::msg)
    }
    async fn exists_f(&self, path: &str) -> Result<bool, Fail> {
        match self.attrs(path, true).await {
            Ok(_) => Ok(true),
            Err(Fail::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Up to `max_bytes` of the file, and its stat taken on the same open
    /// handle (so size/mtime describe the bytes read, not a later version).
    /// `bytes.len() < stat.size` means the read was capped.
    pub async fn read(&self, path: &str, max_bytes: u64) -> Result<(Vec<u8>, Stat), String> {
        self.read_f(path, max_bytes).await.map_err(Fail::msg)
    }
    async fn read_f(&self, path: &str, max_bytes: u64) -> Result<(Vec<u8>, Stat), Fail> {
        check_path(path)?;
        let what = format!("read {path}");
        let mut file = match self.session.open(path).await {
            Ok(f) => f,
            Err(e) => {
                let f = self.annotate(classify(e, &what));
                // Some servers refuse to open a directory at all; say so
                // plainly rather than passing on a bare "Failure".
                if matches!(f, Fail::Fs(_)) && self.stat_f(path).await.is_ok_and(|s| s.is_dir) {
                    return Err(Fail::Fs(format!("{path} is a directory")));
                }
                return Err(f);
            }
        };
        let stat = Stat::from_attrs(
            &file
                .metadata()
                .await
                .map_err(|e| self.annotate(classify(e, &what)))?,
        );
        if stat.is_dir {
            let _ = file.close().await;
            return Err(Fail::Fs(format!("{path} is a directory")));
        }
        let mut buf = Vec::with_capacity(stat.size.min(max_bytes).min(STREAM_CAP as u64) as usize);
        (&mut file)
            .take(max_bytes)
            .read_to_end(&mut buf)
            .await
            .map_err(|e| self.annotate(classify_io(e, &what)))?;
        let _ = file.close().await;
        Ok((buf, stat))
    }

    /// Replace the file's contents. An existing file is truncated and
    /// rewritten in place, keeping its mode, owner, ACLs and hard links (a
    /// write-temp-then-rename would silently reset all of them). A new
    /// file is created 0644 (less the server's umask).
    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_f(path, bytes).await.map_err(Fail::msg)
    }
    async fn write_f(&self, path: &str, bytes: &[u8]) -> Result<(), Fail> {
        check_path(path)?;
        let what = format!("write {path}");
        let attrs = FileAttributes {
            permissions: Some(0o644),
            ..FileAttributes::empty()
        };
        let mut file = self
            .session
            .open_with_flags_and_attributes(
                path,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                attrs,
            )
            .await
            .map_err(|e| self.annotate(classify(e, &what)))?;
        file.write_all(bytes)
            .await
            .map_err(|e| self.annotate(classify_io(e, &what)))?;
        // Waits for every write to be acknowledged, then the close.
        file.shutdown()
            .await
            .map_err(|e| self.annotate(classify_io(e, &what)))
    }

    pub async fn create_dir_all(&self, path: &str) -> Result<(), String> {
        self.create_dir_all_f(path).await.map_err(Fail::msg)
    }
    async fn create_dir_all_f(&self, path: &str) -> Result<(), Fail> {
        check_path(path)?;
        if self.stat_f(path).await.is_ok_and(|s| s.is_dir) {
            return Ok(());
        }
        let mut cur = String::new();
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            cur.push('/');
            cur.push_str(comp);
            match self.stat_f(&cur).await {
                Ok(s) if s.is_dir => continue,
                Ok(_) => return Err(Fail::Fs(format!("{cur} exists and is not a directory"))),
                Err(Fail::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
            if let Err(e) = self.session.create_dir(cur.as_str()).await {
                // Lost a race with another creator: fine if it's a dir now.
                if !self.stat_f(&cur).await.is_ok_and(|s| s.is_dir) {
                    return Err(self.annotate(classify(e, &format!("create directory {cur}"))));
                }
            }
        }
        Ok(())
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), String> {
        self.remove_file_f(path).await.map_err(Fail::msg)
    }
    async fn remove_file_f(&self, path: &str) -> Result<(), Fail> {
        check_path(path)?;
        self.session
            .remove_file(path)
            .await
            .map_err(|e| self.annotate(classify(e, &format!("remove {path}"))))
    }

    pub async fn remove_dir(&self, path: &str) -> Result<(), String> {
        self.remove_dir_f(path).await.map_err(Fail::msg)
    }
    async fn remove_dir_f(&self, path: &str) -> Result<(), Fail> {
        check_path(path)?;
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| self.annotate(classify(e, &format!("remove directory {path}"))))
    }

    /// Directory entries (without `.`/`..`), sorted by name.
    pub async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, String> {
        self.read_dir_f(path).await.map_err(Fail::msg)
    }
    async fn read_dir_f(&self, path: &str) -> Result<Vec<Entry>, Fail> {
        check_path(path)?;
        let rd = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| self.annotate(classify(e, &format!("list {path}"))))?;
        let mut v: Vec<Entry> = rd
            .map(|e| {
                let s = Stat::from_attrs(&e.metadata());
                Entry {
                    name: e.file_name(),
                    is_dir: s.is_dir,
                    is_symlink: s.is_symlink,
                    size: s.size,
                    mtime: s.mtime_secs,
                }
            })
            .filter(|e| !e.name.is_empty() && !e.name.contains('/'))
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn canon(&self, path: &str) -> Result<String, Fail> {
        let c = self
            .session
            .canonicalize(path)
            .await
            .map_err(|e| self.annotate(classify(e, &format!("resolve {path}"))))?;
        if !c.starts_with('/') {
            return Err(Fail::Fs(format!(
                "resolve {path}: the server returned a relative path"
            )));
        }
        Ok(host::normalize(&c))
    }

    /// Where `path` physically is, symlinks resolved — the thing to compare
    /// against a (realpath'd) project root before reading or writing it.
    ///
    /// Works for paths that don't exist yet: the deepest ancestor the server
    /// can canonicalize is resolved by the server, and the rest is walked
    /// component by component. A dangling symlink is followed to where a
    /// write through it would land (treating it as a plain missing name
    /// would let `root/link → /etc/x` pass as inside the root).
    pub async fn realpath(&self, path: &str) -> Result<String, String> {
        self.realpath_f(path).await.map_err(Fail::msg)
    }
    async fn realpath_f(&self, path: &str) -> Result<String, Fail> {
        check_path(path)?;
        match self.canon(path).await {
            Ok(c) => return Ok(c),
            Err(Fail::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let mut head = path.to_string();
        let mut tail: Vec<String> = vec![];
        let base = loop {
            if head == "/" {
                break "/".to_string();
            }
            tail.push(host::file_name(&head));
            head = host::parent(&head);
            match self.canon(&head).await {
                Ok(c) => break c,
                Err(Fail::NotFound(_)) => continue,
                Err(e) => return Err(e),
            }
        };
        tail.reverse();
        self.walk(base, tail).await
    }

    /// Resolve `comps` under the canonical directory `base` like the kernel
    /// would, without requiring the final components to exist.
    async fn walk(&self, base: String, comps: Vec<String>) -> Result<String, Fail> {
        let mut resolved = base;
        let mut rest: VecDeque<String> = comps.into();
        let mut hops = 0;
        let mut missing = false;
        while let Some(c) = rest.pop_front() {
            match c.as_str() {
                "" | "." => continue,
                ".." => {
                    // Past a missing directory the kernel would fail with
                    // ENOENT; a lexical `..` would instead land somewhere
                    // real and unchecked. Refuse rather than guess.
                    if missing {
                        return Err(Fail::Fs(format!(
                            "resolve {}: a path can't climb out of a directory that doesn't exist",
                            resolved
                        )));
                    }
                    resolved = host::parent(&resolved);
                    continue;
                }
                _ => {}
            }
            let cand = host::join(&resolved, &c);
            if missing {
                resolved = cand;
                continue;
            }
            match self.lstat_f(&cand).await {
                Ok(s) if s.is_symlink => {
                    hops += 1;
                    if hops > SYMLINK_HOPS {
                        return Err(Fail::Fs(format!(
                            "resolve {cand}: too many levels of symbolic links"
                        )));
                    }
                    let t = self
                        .session
                        .read_link(cand.as_str())
                        .await
                        .map_err(|e| self.annotate(classify(e, &format!("readlink {cand}"))))?;
                    if t.is_empty() || t.contains('\0') {
                        return Err(Fail::Fs(format!("resolve {cand}: unusable symlink target")));
                    }
                    if t.starts_with('/') {
                        resolved = "/".into();
                    }
                    for comp in t.split('/').rev() {
                        rest.push_front(comp.to_string());
                    }
                }
                Ok(_) => resolved = cand,
                Err(Fail::NotFound(_)) => {
                    missing = true;
                    resolved = cand;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(resolved)
    }
}

async fn new_session<S>(stream: S) -> Result<SftpSession, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let cfg = Config {
        request_timeout_secs: REQUEST_TIMEOUT_SECS,
        ..Config::default()
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, SftpSession::new_with_config(stream, cfg)).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out".into()),
    }
}

// ───────────────────────────── connection pool ─────────────────────────────

type ConnectFut = Pin<Box<dyn Future<Output = Result<Sftp, String>> + Send>>;
type Connector = Arc<dyn Fn(String) -> ConnectFut + Send + Sync>;
type Slot = Arc<tokio::sync::Mutex<Option<Arc<Sftp>>>>;

/// One SFTP session per host, created on first use. Each host has its own
/// slot lock, so a slow connect to one host doesn't stall the others.
struct Pool {
    slots: Mutex<HashMap<String, Slot>>,
    connect: Connector,
}

impl Pool {
    fn new(connect: Connector) -> Self {
        Pool {
            slots: Mutex::default(),
            connect,
        }
    }

    fn slot(&self, host: &str) -> Slot {
        self.slots
            .lock()
            .unwrap()
            .entry(host.to_string())
            .or_default()
            .clone()
    }

    async fn get(&self, host: &str) -> Result<Arc<Sftp>, String> {
        let slot = self.slot(host);
        let mut g = slot.lock().await;
        if let Some(s) = g.as_ref().filter(|s| s.alive()) {
            return Ok(s.clone());
        }
        let s = Arc::new((self.connect)(host.to_string()).await?);
        *g = Some(s.clone());
        Ok(s)
    }

    /// Drop `dead` if it is still the pooled session (another caller may
    /// already have replaced it).
    async fn invalidate(&self, host: &str, dead: &Arc<Sftp>) {
        let slot = self.slot(host);
        let mut g = slot.lock().await;
        if g.as_ref().is_some_and(|s| Arc::ptr_eq(s, dead)) {
            *g = None;
        }
    }

    async fn forget(&self, host: &str) {
        let slot = self.slots.lock().unwrap().remove(host);
        if let Some(slot) = slot {
            *slot.lock().await = None;
        }
    }

    /// Run `op` on the host's session; on a transport failure, reconnect
    /// and retry once. Every op used here is idempotent (whole-file write,
    /// mkdir -p, stat, …), so a retry after an unknown outcome is safe.
    async fn run<T, F, Fut>(&self, host: &str, op: F) -> Result<T, String>
    where
        F: Fn(Arc<Sftp>) -> Fut,
        Fut: Future<Output = Result<T, Fail>>,
    {
        let s = self.get(host).await?;
        match op(s.clone()).await {
            Err(Fail::Transport(first)) => {
                self.invalidate(host, &s).await;
                let s = self.get(host).await.map_err(|e| {
                    format!("The SSH connection failed ({first}), and reconnecting failed: {e}")
                })?;
                match op(s.clone()).await {
                    Err(Fail::Transport(e)) => {
                        self.invalidate(host, &s).await;
                        Err(format!("The SSH connection failed twice: {e}"))
                    }
                    r => r.map_err(Fail::msg),
                }
            }
            r => r.map_err(Fail::msg),
        }
    }
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        Pool::new(Arc::new(|host: String| -> ConnectFut {
            Box::pin(async move {
                #[cfg(test)]
                if let Some(fs) = tests::fs_for(&host) {
                    return Ok(tests::session_on(fs, false).await);
                }
                let target = target_for(&host)?;
                Sftp::connect(&target).await
            })
        }))
    })
}

/// The ssh target saved for host id `host` (Settings → Remote).
pub fn target_for(host: &str) -> Result<String, String> {
    #[cfg(test)]
    if tests::is_local_sh(host) {
        return Ok(tests::LOCAL_SH_TARGET.into());
    }
    let vault = crate::remote::auth::Vault::load();
    let h = vault
        .ssh_hosts
        .into_iter()
        .find(|h| h.id == host)
        .ok_or_else(|| format!("This project's SSH host ({host}) is no longer saved. Add it again in Settings → Remote."))?;
    let t = h.target.trim().to_string();
    if !valid_target(&t) {
        return Err(format!(
            "The saved SSH destination for {} isn't valid.",
            h.name
        ));
    }
    Ok(t)
}

pub async fn read(host: &str, path: &str, max_bytes: u64) -> Result<(Vec<u8>, Stat), String> {
    pool()
        .run(host, |s| async move { s.read_f(path, max_bytes).await })
        .await
}

pub async fn stat(host: &str, path: &str) -> Result<Stat, String> {
    pool()
        .run(host, |s| async move { s.stat_f(path).await })
        .await
}

pub async fn lstat(host: &str, path: &str) -> Result<Stat, String> {
    pool()
        .run(host, |s| async move { s.lstat_f(path).await })
        .await
}

pub async fn exists(host: &str, path: &str) -> Result<bool, String> {
    pool()
        .run(host, |s| async move { s.exists_f(path).await })
        .await
}

/// See `Sftp::realpath`.
pub async fn realpath(host: &str, path: &str) -> Result<String, String> {
    pool()
        .run(host, |s| async move { s.realpath_f(path).await })
        .await
}

/// See `Sftp::write`.
pub async fn write(host: &str, path: &str, bytes: &[u8]) -> Result<(), String> {
    pool()
        .run(host, |s| async move { s.write_f(path, bytes).await })
        .await
}

pub async fn create_dir_all(host: &str, path: &str) -> Result<(), String> {
    pool()
        .run(host, |s| async move { s.create_dir_all_f(path).await })
        .await
}

pub async fn remove_file(host: &str, path: &str) -> Result<(), String> {
    pool()
        .run(host, |s| async move { s.remove_file_f(path).await })
        .await
}

pub async fn remove_dir(host: &str, path: &str) -> Result<(), String> {
    pool()
        .run(host, |s| async move { s.remove_dir_f(path).await })
        .await
}

pub async fn read_dir(host: &str, path: &str) -> Result<Vec<Entry>, String> {
    pool()
        .run(host, |s| async move { s.read_dir_f(path).await })
        .await
}

/// Close the host's pooled SFTP session and forget its probed caps (after
/// the host is edited or removed, or to force a fresh connection).
pub async fn forget(host: &str) {
    pool().forget(host).await;
    caps_cache().lock().unwrap().remove(host);
}

// ───────────────────────────────── commands ─────────────────────────────────

fn with_defaults<'a>(env: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
    let mut v: Vec<(&str, &str)> = DEFAULT_ENV
        .iter()
        .filter(|(k, _)| !env.iter().any(|(e, _)| e == k))
        .copied()
        .collect();
    v.extend(env.iter().map(|(k, v)| (*k, v.as_str())));
    v
}

fn spawn_job(target: &str, command: &str, stdin: bool) -> Result<Child, String> {
    if !valid_target(target) {
        return Err("That SSH destination isn't valid. Use a host alias or user@host.".into());
    }
    #[cfg(test)]
    let mut c = if target == tests::LOCAL_SH_TARGET {
        // `sh -c <command>`, as sshd runs it. Through a file rather than
        // argv: on Windows the argv hop to an MSYS `sh` re-quotes `"` and
        // `\`, which sshd (receiving one opaque string) never does.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let f = std::env::temp_dir().join(format!(
            "ol-ssh-job-{}-{}.txt",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&f, command).map_err(|e| e.to_string())?;
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c")
            .arg(r#"c=$(cat "$1"; printf x); rm -f "$1"; exec sh -c "${c%x}""#)
            .arg("sh")
            .arg(f.to_string_lossy().replace('\\', "/"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    } else {
        let mut c = ssh();
        c.args(BASE_OPTS)
            .args(CMD_OPTS)
            .arg("--")
            .arg(target)
            .arg(command);
        c
    };
    #[cfg(not(test))]
    let mut c = ssh();
    #[cfg(not(test))]
    c.args(BASE_OPTS)
        .args(CMD_OPTS)
        .arg("--")
        .arg(target)
        .arg(command);
    if stdin {
        c.stdin(Stdio::piped());
    }
    c.spawn().map_err(spawn_err)
}

/// Read lines until the pgid marker. Earlier lines (output of the remote
/// login shell's startup files) are discarded. `None`: stdout ended, or
/// too much came first, before any marker.
async fn read_marker<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> std::io::Result<Option<u32>> {
    let mut seen = 0u64;
    let mut line = Vec::new();
    while seen < MARKER_SCAN_MAX {
        line.clear();
        let n = (&mut *r)
            .take(MARKER_SCAN_MAX - seen)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            return Ok(None);
        }
        seen += n as u64;
        if let Some(p) = host::parse_pgid_line(&line) {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// Collect a stream, keeping at most `cap` bytes (the rest is drained so the
/// remote side never blocks on a full pipe).
fn collect<R: AsyncRead + Unpin + Send + 'static>(
    r: Option<R>,
    cap: usize,
) -> tokio::task::JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut out = Vec::new();
        let Some(mut r) = r else { return out };
        let mut chunk = vec![0u8; 16384];
        while let Ok(n) = r.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let room = cap.saturating_sub(out.len());
            out.extend_from_slice(&chunk[..n.min(room)]);
        }
        out
    })
}

async fn join_within(h: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(2), h)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn ssh_failure(stderr: &[u8], code: Option<i32>) -> String {
    let said = String::from_utf8_lossy(stderr).trim().to_string();
    if !said.is_empty() {
        format!("ssh: {said}")
    } else {
        match code {
            Some(c) => format!("ssh exited with code {c} before the command started."),
            None => "ssh ended before the command started.".into(),
        }
    }
}

/// Run `cmd` in `cwd` on host `host` and collect its output. A timeout or
/// cancel kills the remote process group, not just the local `ssh`.
pub async fn exec(
    host: &str,
    cwd: &str,
    cmd: &str,
    env: &[(&str, String)],
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<ExecOutput, String> {
    let target = target_for(host)?;
    let command = host::job_command(cwd, &with_defaults(env), cmd)?;
    exec_command(&target, &command, stdin, timeout, cancel).await
}

async fn exec_command(
    target: &str,
    command: &str,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<ExecOutput, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut child = spawn_job(target, command, stdin.is_some())?;
    if let (Some(data), Some(mut w)) = (stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = w.write_all(&data).await;
            let _ = w.shutdown().await;
        });
    }
    let err_task = collect(child.stderr.take(), STREAM_CAP);
    let Some(stdout) = child.stdout.take() else {
        return Err("couldn't run ssh: no stdout".into());
    };
    let mut out = BufReader::new(stdout);

    enum Start {
        Marker(Option<u32>),
        TimedOut,
        Cancelled,
    }
    let start = tokio::select! {
        m = read_marker(&mut out) => Start::Marker(m.ok().flatten()),
        _ = tokio::time::sleep_until(deadline) => Start::TimedOut,
        _ = cancel.cancelled() => Start::Cancelled,
    };
    let pgid = match start {
        Start::Marker(Some(p)) => p,
        Start::Marker(None) => {
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            let _ = child.start_kill();
            let code = child.try_wait().ok().flatten().and_then(|s| s.code());
            return Err(ssh_failure(&join_within(err_task).await, code));
        }
        // Before the marker there is no remote group to kill yet; ending the
        // client is all that can be done.
        s @ (Start::TimedOut | Start::Cancelled) => {
            let _ = child.start_kill();
            return Ok(ExecOutput {
                stderr: join_within(err_task).await,
                timed_out: matches!(s, Start::TimedOut),
                interrupted: matches!(s, Start::Cancelled),
                ..Default::default()
            });
        }
    };

    let out_task = collect(Some(out), STREAM_CAP);
    let (status, timed_out, interrupted) = tokio::select! {
        s = child.wait() => (s.ok(), false, false),
        _ = tokio::time::sleep_until(deadline) => (None, true, false),
        _ = cancel.cancelled() => (None, false, true),
    };
    if timed_out || interrupted {
        let grace = Duration::from_secs(KILL_GRACE_SECS as u64 + 20);
        let _ = tokio::time::timeout(grace, kill_target(target, pgid)).await;
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    Ok(ExecOutput {
        stdout: join_within(out_task).await,
        stderr: join_within(err_task).await,
        code: status.and_then(|s| s.code()),
        timed_out,
        interrupted,
    })
}

/// A background command on a remote host. Shaped for `shell::BgManager`:
/// pump `stdout`/`stderr` like a local child's, `child.wait()` for the
/// exit, and call `kill` (not just `child.start_kill()`) to stop it.
pub struct RemoteJob {
    pub host: String,
    pub pgid: u32,
    /// The local `ssh` client; its stdout/stderr have been taken.
    pub child: Child,
    /// The command's stdout (the pgid marker already consumed).
    pub stdout: BufReader<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

impl RemoteJob {
    /// TERM the remote process group, KILL it after a grace period, then end
    /// the local client.
    pub async fn kill(&mut self) -> Result<(), String> {
        let r = kill(&self.host, self.pgid).await;
        let _ = self.child.start_kill();
        r
    }
}

/// Start `cmd` in the background on host `host`. Returns once the remote
/// side has reported its process group.
pub async fn spawn_bg(
    host: &str,
    cwd: &str,
    cmd: &str,
    env: &[(&str, String)],
) -> Result<RemoteJob, String> {
    let target = target_for(host)?;
    let command = host::job_command(cwd, &with_defaults(env), cmd)?;
    let mut child = spawn_job(&target, &command, false)?;
    let Some(stdout) = child.stdout.take() else {
        return Err("couldn't run ssh: no stdout".into());
    };
    let mut out = BufReader::new(stdout);
    match tokio::time::timeout(MARKER_TIMEOUT, read_marker(&mut out)).await {
        Ok(Ok(Some(pgid))) => Ok(RemoteJob {
            host: host.to_string(),
            pgid,
            stderr: child.stderr.take(),
            child,
            stdout: out,
        }),
        r => {
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            let _ = child.start_kill();
            let code = child.try_wait().ok().flatten().and_then(|s| s.code());
            let err = join_within(collect(child.stderr.take(), 64 << 10)).await;
            Err(if r.is_err() && err.is_empty() {
                "The remote command didn't start in time.".into()
            } else {
                ssh_failure(&err, code)
            })
        }
    }
}

/// End remote process group `pgid` on host `host` (TERM, then KILL after a
/// short grace period).
pub async fn kill(host: &str, pgid: u32) -> Result<(), String> {
    kill_target(&target_for(host)?, pgid).await
}

async fn kill_target(target: &str, pgid: u32) -> Result<(), String> {
    let command = host::kill_command(pgid, KILL_GRACE_SECS)
        .ok_or("Refusing to signal process group 0 or 1.")?;
    let child = spawn_job(target, &command, false)?;
    let out = tokio::time::timeout(
        Duration::from_secs(KILL_GRACE_SECS as u64 + 20),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "Timed out stopping the remote command.".to_string())?
    .map_err(|e| format!("couldn't run ssh: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ssh_failure(&out.stderr, out.status.code()))
    }
}

// ─────────────────────────────── probing ───────────────────────────────

/// The line `exec_machine` prints right before the real command's output.
/// Everything a login shell's startup files print comes before it.
pub const OUT_MARKER: &str = "__OPENLEASH_OUT_7f3a9c__";

/// What `exec_machine` returns: stdout after the marker, the full stderr,
/// and the exit code (`None` = killed or no status).
#[derive(Debug, Clone, Default)]
pub struct MachineOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub interrupted: bool,
}

/// The text after the first `OUT_MARKER` line, or `None` when the marker
/// never appeared (the command didn't run, or its output can't be trusted
/// to be its own).
pub fn after_marker(stdout: &[u8]) -> Option<Vec<u8>> {
    let m = format!("{OUT_MARKER}\n");
    let m = m.as_bytes();
    let i = stdout.windows(m.len()).position(|w| w == m)?;
    Some(stdout[i + m.len()..].to_vec())
}

/// Run a command whose stdout is *parsed* (git porcelain, file lists, rg
/// output): a marker line is printed first and everything before it is
/// dropped, so whatever the remote login profile prints can't be mistaken
/// for data (phase-1 risk 2). `cmd` must be built with `host::sh_quote`.
pub async fn exec_machine(
    host: &str,
    cwd: &str,
    cmd: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<MachineOutput, String> {
    let full = format!("printf '%s\\n' {}; {cmd}", host::sh_quote(OUT_MARKER));
    let out = exec(host, cwd, &full, &[], None, timeout, cancel).await?;
    if out.timed_out || out.interrupted {
        return Ok(MachineOutput {
            stderr: out.stderr,
            timed_out: out.timed_out,
            interrupted: out.interrupted,
            ..Default::default()
        });
    }
    let Some(stdout) = after_marker(&out.stdout) else {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() {
            "The remote command produced no usable output.".into()
        } else {
            said
        });
    };
    Ok(MachineOutput {
        stdout,
        stderr: out.stderr,
        code: out.code,
        timed_out: false,
        interrupted: false,
    })
}

/// Run an `ssh_session` call from synchronous code (git's seam, the prompt
/// builder, trust manifests). It runs on a fresh OS thread driving the app's
/// shared async runtime, so it is safe to call from inside an async task as
/// well as from a blocking one (a nested `block_on` on a runtime thread
/// would panic), and pooled SFTP sessions stay on the long-lived runtime.
pub fn block_on<F>(f: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|s| {
        s.spawn(|| tauri::async_runtime::block_on(f))
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// The probed caps for `host`, if a probe has already succeeded.
pub fn cached_caps(host: &str) -> Option<Caps> {
    caps_cache().lock().unwrap().get(host).cloned()
}

fn caps_cache() -> &'static Mutex<HashMap<String, Caps>> {
    static CAPS: OnceLock<Mutex<HashMap<String, Caps>>> = OnceLock::new();
    CAPS.get_or_init(Mutex::default)
}

/// What the host offers (OS, bash/rg/git/setsid/realpath, home), probed
/// once per host with one `ssh` round trip and then cached.
pub async fn probe(host: &str) -> Result<Caps, String> {
    if let Some(c) = caps_cache().lock().unwrap().get(host) {
        return Ok(c.clone());
    }
    let out = exec(
        host,
        "/",
        host::PROBE_SCRIPT,
        &[],
        None,
        Duration::from_secs(45),
        &CancellationToken::new(),
    )
    .await?;
    if out.timed_out {
        return Err("The SSH host didn't answer in time.".into());
    }
    let caps = host::parse_probe(&String::from_utf8_lossy(&out.stdout));
    if out.code != Some(0) || caps.home.is_empty() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() {
            "The SSH host didn't answer like a POSIX shell (Linux, macOS or BSD are supported)."
                .into()
        } else {
            format!("Probing the SSH host failed: {said}")
        });
    }
    caps_cache()
        .lock()
        .unwrap()
        .insert(host.to_string(), caps.clone());
    Ok(caps)
}

/// A fresh end-to-end check for the UI: run a command, then open SFTP.
pub async fn test_connection(host: &str) -> Result<String, String> {
    forget(host).await;
    let caps = probe(host).await?;
    let home = realpath(host, &host::normalize(&caps.home))
        .await
        .map_err(|e| format!("Commands work, but SFTP doesn't: {e}"))?;
    let missing: Vec<&str> = [
        ("bash", caps.has_bash),
        ("git", caps.has_git),
        ("rg", caps.has_rg),
    ]
    .iter()
    .filter(|(_, has)| !has)
    .map(|(n, _)| *n)
    .collect();
    let os = if caps.os.is_empty() {
        "unknown OS"
    } else {
        caps.os.as_str()
    };
    let mut msg = format!("Connected: {os}, home {home}.");
    if !missing.is_empty() {
        msg.push_str(&format!(" Not installed there: {}.", missing.join(", ")));
    }
    Ok(msg)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use russh_sftp::protocol::{Attrs, Data, File as PFile, Handle, Name, Status, Version};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── An in-memory POSIX-ish filesystem served over real SFTP. In memory
    // rather than a temp dir: Windows can't create symlinks without
    // Developer Mode, and the symlink cases are the point.

    #[derive(Clone, Debug)]
    pub(crate) enum Node {
        Dir,
        File(Vec<u8>, u32),
        Link(String),
    }
    pub(crate) type Fs = Arc<Mutex<BTreeMap<String, Node>>>;

    struct Vfs {
        fs: Fs,
        /// OpenSSH's realpath accepts a missing last component; a strict
        /// server refuses any missing path. Both must work.
        strict: bool,
        files: HashMap<String, String>,
        dirs: HashMap<String, (String, bool)>,
        next: u64,
    }

    fn comps(p: &str) -> VecDeque<String> {
        p.split('/').map(str::to_string).collect()
    }

    /// Kernel-style lookup: (physical path, node if it exists).
    fn lookup(
        fs: &BTreeMap<String, Node>,
        path: &str,
        follow_last: bool,
    ) -> Result<(String, Option<Node>), StatusCode> {
        let mut resolved = "/".to_string();
        let mut rest = comps(path);
        let mut hops = 0;
        while let Some(c) = rest.pop_front() {
            if c.is_empty() || c == "." {
                continue;
            }
            if c == ".." {
                resolved = host::parent(&resolved);
                continue;
            }
            let last = rest.iter().all(|c| c.is_empty() || c == ".");
            let cand = host::join(&resolved, &c);
            match fs.get(&cand) {
                Some(Node::Link(t)) if !last || follow_last => {
                    hops += 1;
                    if hops > 40 {
                        return Err(StatusCode::NoSuchFile);
                    }
                    if t.starts_with('/') {
                        resolved = "/".into();
                    }
                    for c in t.split('/').rev() {
                        rest.push_front(c.to_string());
                    }
                }
                Some(Node::File(..)) if !last => return Err(StatusCode::NoSuchFile),
                Some(_) => resolved = cand,
                None if last => return Ok((cand, None)),
                None => return Err(StatusCode::NoSuchFile),
            }
        }
        let node = fs.get(&resolved).cloned();
        Ok((resolved, node))
    }

    fn attrs_of(n: &Node) -> FileAttributes {
        let (size, perm) = match n {
            Node::Dir => (0, 0o040755),
            Node::File(d, m) => (d.len() as u64, 0o100000 | m),
            Node::Link(t) => (t.len() as u64, 0o120777),
        };
        FileAttributes {
            size: Some(size),
            permissions: Some(perm),
            mtime: Some(1_700_000_000),
            atime: Some(1_700_000_000),
            ..FileAttributes::empty()
        }
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }

    fn abs(p: &str) -> String {
        if p.starts_with('/') {
            p.to_string()
        } else {
            format!("/{p}")
        }
    }

    impl russh_sftp::server::Handler for Vfs {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn init(
            &mut self,
            _v: u32,
            _e: HashMap<String, String>,
        ) -> Result<Version, Self::Error> {
            Ok(Version::new())
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            pflags: OpenFlags,
            attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            let mut fs = self.fs.lock().unwrap();
            let (p, node) = lookup(&fs, &abs(&filename), true)?;
            match node {
                Some(Node::Dir) => return Err(StatusCode::Failure),
                Some(Node::File(_, m)) => {
                    if pflags.contains(OpenFlags::TRUNCATE) {
                        fs.insert(p.clone(), Node::File(vec![], m));
                    }
                }
                Some(Node::Link(_)) => return Err(StatusCode::Failure),
                None if pflags.contains(OpenFlags::CREATE) => {
                    let m = attrs.permissions.unwrap_or(0o666) & 0o7777 & !0o022;
                    fs.insert(p.clone(), Node::File(vec![], m));
                }
                None => return Err(StatusCode::NoSuchFile),
            }
            self.next += 1;
            let handle = format!("f{}", self.next);
            self.files.insert(handle.clone(), p);
            Ok(Handle { id, handle })
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            if self.files.remove(&handle).is_none() && self.dirs.remove(&handle).is_none() {
                return Err(StatusCode::Failure);
            }
            Ok(ok(id))
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let p = self.files.get(&handle).ok_or(StatusCode::Failure)?;
            let fs = self.fs.lock().unwrap();
            let Some(Node::File(d, _)) = fs.get(p) else {
                return Err(StatusCode::Failure);
            };
            let off = offset as usize;
            if off >= d.len() {
                return Err(StatusCode::Eof);
            }
            let end = (off + len as usize).min(d.len());
            Ok(Data {
                id,
                data: d[off..end].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let p = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
            let mut fs = self.fs.lock().unwrap();
            let Some(Node::File(d, _)) = fs.get_mut(&p) else {
                return Err(StatusCode::Failure);
            };
            let off = offset as usize;
            if d.len() < off + data.len() {
                d.resize(off + data.len(), 0);
            }
            d[off..off + data.len()].copy_from_slice(&data);
            Ok(ok(id))
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let fs = self.fs.lock().unwrap();
            match lookup(&fs, &abs(&path), true)? {
                (_, Some(n)) => Ok(Attrs {
                    id,
                    attrs: attrs_of(&n),
                }),
                _ => Err(StatusCode::NoSuchFile),
            }
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let fs = self.fs.lock().unwrap();
            match lookup(&fs, &abs(&path), false)? {
                (_, Some(n)) => Ok(Attrs {
                    id,
                    attrs: attrs_of(&n),
                }),
                _ => Err(StatusCode::NoSuchFile),
            }
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            let p = self.files.get(&handle).ok_or(StatusCode::Failure)?;
            let fs = self.fs.lock().unwrap();
            let n = fs.get(p).ok_or(StatusCode::NoSuchFile)?;
            Ok(Attrs {
                id,
                attrs: attrs_of(n),
            })
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            let fs = self.fs.lock().unwrap();
            let (p, node) = lookup(&fs, &abs(&path), true)?;
            if !matches!(node, Some(Node::Dir)) {
                return Err(StatusCode::NoSuchFile);
            }
            self.next += 1;
            let handle = format!("d{}", self.next);
            self.dirs.insert(handle.clone(), (p, false));
            Ok(Handle { id, handle })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
            let (dir, listed) = self.dirs.get_mut(&handle).ok_or(StatusCode::Failure)?;
            if *listed {
                return Err(StatusCode::Eof);
            }
            *listed = true;
            let fs = self.fs.lock().unwrap();
            let prefix = if dir == "/" {
                "/".to_string()
            } else {
                format!("{dir}/")
            };
            let mut files = vec![PFile::dummy("."), PFile::dummy("..")];
            for (k, n) in fs.iter() {
                if let Some(name) = k.strip_prefix(&prefix) {
                    if !name.is_empty() && !name.contains('/') {
                        files.push(PFile::new(name, attrs_of(n)));
                    }
                }
            }
            Ok(Name { id, files })
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
            let mut fs = self.fs.lock().unwrap();
            match lookup(&fs, &abs(&filename), false)? {
                (p, Some(Node::File(..) | Node::Link(_))) => {
                    fs.remove(&p);
                    Ok(ok(id))
                }
                (_, Some(Node::Dir)) => Err(StatusCode::Failure),
                (_, None) => Err(StatusCode::NoSuchFile),
            }
        }

        async fn mkdir(
            &mut self,
            id: u32,
            path: String,
            _attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let mut fs = self.fs.lock().unwrap();
            match lookup(&fs, &abs(&path), false)? {
                (_, Some(_)) => Err(StatusCode::Failure),
                (p, None) => {
                    fs.insert(p, Node::Dir);
                    Ok(ok(id))
                }
            }
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            let fs = self.fs.lock().unwrap();
            let (p, node) = lookup(&fs, &abs(&path), true)?;
            if node.is_none() && self.strict {
                return Err(StatusCode::NoSuchFile);
            }
            Ok(Name {
                id,
                files: vec![PFile::dummy(p)],
            })
        }

        async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            let fs = self.fs.lock().unwrap();
            match fs.get(&host::normalize(&path)) {
                Some(Node::Link(t)) => Ok(Name {
                    id,
                    files: vec![PFile::dummy(t.clone())],
                }),
                _ => Err(StatusCode::Failure),
            }
        }
    }

    pub(crate) fn sample_fs() -> Fs {
        let mut m = BTreeMap::new();
        let mut put = |p: &str, n: Node| {
            m.insert(p.to_string(), n);
        };
        for d in [
            "/",
            "/proj",
            "/proj/src",
            "/proj/src/sub",
            "/etc",
            "/outside",
            "/deep",
            "/deep/a",
            "/deep/a/b",
        ] {
            put(d, Node::Dir);
        }
        put("/proj/a.txt", Node::File(b"hello\nworld\n".to_vec(), 0o600));
        put(
            "/proj/run.sh",
            Node::File(b"#!/bin/sh\necho old\n".to_vec(), 0o755),
        );
        put(
            "/proj/src/main.rs",
            Node::File(b"fn main() {}\n".to_vec(), 0o644),
        );
        put("/etc/passwd", Node::File(b"root:x:0:0\n".to_vec(), 0o644));
        // Escapes: absolute, dangling, relative, through a dir link, loop.
        put("/proj/link_out", Node::Link("/etc".into()));
        put("/proj/dangle", Node::Link("/outside/new.txt".into()));
        put("/proj/rel_up", Node::Link("../secret".into()));
        put("/proj/linkdir", Node::Link("/deep/a/b".into()));
        put("/deep/a/b/up", Node::Link("../x".into()));
        put("/proj/loop", Node::Link("/proj/loop".into()));
        put(
            "/proj/sneaky",
            Node::Link("nothere/../../etc/passwd".into()),
        );
        put("/proj/inner", Node::Link("src".into()));
        Arc::new(Mutex::new(m))
    }

    fn vfs(fs: Fs, strict: bool) -> Vfs {
        Vfs {
            fs,
            strict,
            files: HashMap::new(),
            dirs: HashMap::new(),
            next: 0,
        }
    }

    pub(crate) async fn session_on(fs: Fs, strict: bool) -> Sftp {
        let (client, server) = tokio::io::duplex(1 << 20);
        russh_sftp::server::run(server, vfs(fs, strict)).await;
        Sftp::from_stream(client).await.unwrap()
    }

    // ── Pointing the *global* pool and command runner at test doubles, for
    // the remote-project tests in other modules (no ssh, no network).

    /// A fresh host id for one test: the pool and caps cache are global.
    pub(crate) fn host_id() -> String {
        format!("t{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
    }

    fn sftp_hosts() -> &'static Mutex<HashMap<String, Fs>> {
        static H: OnceLock<Mutex<HashMap<String, Fs>>> = OnceLock::new();
        H.get_or_init(Mutex::default)
    }

    fn sh_hosts() -> &'static Mutex<std::collections::HashSet<String>> {
        static H: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
        H.get_or_init(Mutex::default)
    }

    /// Serve host `host`'s files from `fs` through the global pool.
    pub(crate) fn install_fs(host: &str, fs: Fs) {
        sftp_hosts().lock().unwrap().insert(host.to_string(), fs);
    }

    pub(crate) fn fs_for(host: &str) -> Option<Fs> {
        sftp_hosts().lock().unwrap().get(host).cloned()
    }

    /// Run host `host`'s commands with the local `sh -c <command>`, which is
    /// how sshd hands the command string to a POSIX login shell.
    pub(crate) fn install_local_sh(host: &str) {
        sh_hosts().lock().unwrap().insert(host.to_string());
    }

    pub(crate) fn is_local_sh(host: &str) -> bool {
        sh_hosts().lock().unwrap().contains(host)
    }

    /// The pseudo ssh target that `spawn_job` turns into a local `sh -c`.
    pub(crate) const LOCAL_SH_TARGET: &str = "openleash-test-local-sh";

    pub(crate) fn has_sh() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// The local sh's own spelling of a local directory (msys `/c/...` on
    /// Windows): the "remote" path of that directory for a local-sh host.
    pub(crate) fn sh_path(dir: &std::path::Path) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("cd -- \"$1\" && pwd")
            .arg("sh")
            .arg(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn write_then_read_round_trips_and_new_files_are_0644() {
        let fs = sample_fs();
        let s = session_on(fs.clone(), false).await;
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        s.write("/proj/new.bin", &big).await.unwrap();
        let (got, st) = s.read("/proj/new.bin", u64::MAX).await.unwrap();
        assert_eq!(got, big);
        assert_eq!(st.size, big.len() as u64);
        assert!(st.is_file && !st.is_dir && !st.is_symlink);
        assert_eq!(st.mode, 0o644);
        assert_eq!(st.mtime_secs, 1_700_000_000);
    }

    #[tokio::test]
    async fn rewriting_an_existing_file_truncates_in_place_and_keeps_its_mode() {
        let fs = sample_fs();
        let s = session_on(fs.clone(), false).await;
        s.write("/proj/run.sh", b"#!/bin/sh\n").await.unwrap();
        let (got, st) = s.read("/proj/run.sh", 1 << 20).await.unwrap();
        assert_eq!(got, b"#!/bin/sh\n");
        assert_eq!(st.mode, 0o755, "the executable bit must survive an edit");
        assert_eq!(st.size, 10);
    }

    #[tokio::test]
    async fn read_caps_at_max_bytes_and_reports_the_full_size() {
        let s = session_on(sample_fs(), false).await;
        let (got, st) = s.read("/proj/a.txt", 5).await.unwrap();
        assert_eq!(got, b"hello");
        assert_eq!(st.size, 12);
        assert_eq!(st.mode, 0o600);
        let (got, _) = s.read("/proj/a.txt", 0).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn read_refuses_directories_and_reports_missing_files() {
        let s = session_on(sample_fs(), false).await;
        assert!(s
            .read("/proj/src", 100)
            .await
            .unwrap_err()
            .contains("is a directory"));
        let e = s.read("/proj/nope.txt", 100).await.unwrap_err();
        assert!(e.contains("no such file"), "{e}");
        assert!(!s.exists("/proj/nope.txt").await.unwrap());
        assert!(s.exists("/proj/a.txt").await.unwrap());
        // Reading through a symlink reaches the target (callers must have
        // realpath-checked it first).
        let (got, _) = s.read("/proj/link_out/passwd", 100).await.unwrap();
        assert_eq!(got, b"root:x:0:0\n");
    }

    #[tokio::test]
    async fn paths_must_be_normalized_absolute() {
        let s = session_on(sample_fs(), false).await;
        for bad in [
            "proj/a.txt",
            "/proj/../etc/passwd",
            "/proj/./a.txt",
            "/proj//a.txt",
            "/proj/a.txt/",
            "/proj/a\0",
            "",
        ] {
            assert!(
                s.read(bad, 10)
                    .await
                    .unwrap_err()
                    .contains("not a normalized"),
                "{bad:?}"
            );
            assert!(s.write(bad, b"x").await.is_err(), "{bad:?}");
            assert!(s.realpath(bad).await.is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn stat_follows_symlinks_and_lstat_does_not() {
        let s = session_on(sample_fs(), false).await;
        let st = s.stat("/proj/link_out").await.unwrap();
        assert!(st.is_dir && !st.is_symlink);
        let l = s.lstat("/proj/link_out").await.unwrap();
        assert!(l.is_symlink && !l.is_dir && !l.is_file);
        assert!(s
            .stat("/proj/dangle")
            .await
            .unwrap_err()
            .contains("no such file"));
        assert!(s.lstat("/proj/dangle").await.unwrap().is_symlink);
    }

    #[tokio::test]
    async fn realpath_of_existing_and_missing_paths() {
        for strict in [false, true] {
            let s = session_on(sample_fs(), strict).await;
            assert_eq!(s.realpath("/proj/a.txt").await.unwrap(), "/proj/a.txt");
            assert_eq!(s.realpath("/proj").await.unwrap(), "/proj");
            assert_eq!(s.realpath("/").await.unwrap(), "/");
            assert_eq!(
                s.realpath("/proj/new.txt").await.unwrap(),
                "/proj/new.txt",
                "strict={strict}"
            );
            assert_eq!(
                s.realpath("/proj/x/y/z.txt").await.unwrap(),
                "/proj/x/y/z.txt",
                "strict={strict}"
            );
            assert_eq!(
                s.realpath("/proj/inner/main.rs").await.unwrap(),
                "/proj/src/main.rs"
            );
            assert_eq!(
                s.realpath("/proj/inner/new/deeper.rs").await.unwrap(),
                "/proj/src/new/deeper.rs",
                "strict={strict}"
            );
        }
    }

    #[tokio::test]
    async fn realpath_exposes_every_symlink_escape() {
        for strict in [false, true] {
            let s = session_on(sample_fs(), strict).await;
            let root = s.realpath("/proj").await.unwrap();
            let cases = [
                ("/proj/link_out/passwd", "/etc/passwd"),
                // A new file under a symlinked dir lands outside the root.
                ("/proj/link_out/new.txt", "/etc/new.txt"),
                ("/proj/link_out/a/b/c", "/etc/a/b/c"),
                // Dangling: the write would create the link's target.
                ("/proj/dangle", "/outside/new.txt"),
                ("/proj/rel_up", "/secret"),
                // `..` in a link target is taken from the link's physical
                // directory (/deep/a/b), not lexically from /proj/linkdir.
                ("/proj/linkdir/up", "/deep/a/x"),
                ("/proj/linkdir/up/more", "/deep/a/x/more"),
            ];
            for (p, want) in cases {
                let got = s.realpath(p).await.unwrap();
                assert_eq!(got, want, "{p} strict={strict}");
                assert!(!host::inside(&root, &got), "{p} must not count as inside");
                assert!(
                    host::inside(&root, p),
                    "{p} is lexically inside; only realpath catches it"
                );
            }
            assert!(s.realpath("/proj/loop").await.is_err(), "strict={strict}");
            assert!(s.realpath("/proj/loop/x").await.is_err(), "strict={strict}");
            let e = s.realpath("/proj/sneaky").await.unwrap_err();
            assert!(e.contains("doesn't exist"), "{e}");
        }
    }

    #[tokio::test]
    async fn create_dir_all_is_idempotent_and_stops_at_files() {
        let fs = sample_fs();
        let s = session_on(fs.clone(), false).await;
        s.create_dir_all("/proj/x/y/z").await.unwrap();
        s.create_dir_all("/proj/x/y/z").await.unwrap();
        s.create_dir_all("/proj").await.unwrap();
        assert!(s.stat("/proj/x/y/z").await.unwrap().is_dir);
        assert!(matches!(
            fs.lock().unwrap().get("/proj/x/y"),
            Some(Node::Dir)
        ));
        let e = s.create_dir_all("/proj/a.txt/sub").await.unwrap_err();
        assert!(e.contains("not a directory"), "{e}");
    }

    #[tokio::test]
    async fn read_dir_lists_entries_with_types() {
        let s = session_on(sample_fs(), false).await;
        let v = s.read_dir("/proj/src").await.unwrap();
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["main.rs", "sub"]);
        assert!(!v[0].is_dir && v[0].size == 13 && v[0].mtime == 1_700_000_000);
        assert!(v[1].is_dir);
        let v = s.read_dir("/proj").await.unwrap();
        let link = v.iter().find(|e| e.name == "link_out").unwrap();
        assert!(link.is_symlink && !link.is_dir);
        assert!(!v.iter().any(|e| e.name == "." || e.name == ".."));
        assert!(s.read_dir("/proj/missing").await.is_err());
    }

    #[tokio::test]
    async fn remove_file_removes_files_but_not_directories() {
        let s = session_on(sample_fs(), false).await;
        s.remove_file("/proj/a.txt").await.unwrap();
        assert!(!s.exists("/proj/a.txt").await.unwrap());
        assert!(s
            .remove_file("/proj/a.txt")
            .await
            .unwrap_err()
            .contains("no such file"));
        assert!(s.remove_file("/proj/src").await.is_err());
        // Removing a symlink removes the link, not its target.
        s.remove_file("/proj/link_out").await.unwrap();
        assert!(s.exists("/etc/passwd").await.unwrap());
    }

    /// A connection whose transport the test can cut.
    async fn cuttable(fs: Fs) -> (Sftp, tokio::task::AbortHandle) {
        let (client, mut near) = tokio::io::duplex(1 << 20);
        let (mut far, server) = tokio::io::duplex(1 << 20);
        russh_sftp::server::run(server, vfs(fs, false)).await;
        let proxy = tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut near, &mut far).await;
        });
        (
            Sftp::from_stream(client).await.unwrap(),
            proxy.abort_handle(),
        )
    }

    #[tokio::test]
    async fn pool_reconnects_once_after_the_transport_drops() {
        let fs = sample_fs();
        let connects = Arc::new(AtomicUsize::new(0));
        let cuts: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();
        let (fs2, c2, cuts2) = (fs.clone(), connects.clone(), cuts.clone());
        let pool = Pool::new(Arc::new(move |host: String| -> ConnectFut {
            let (fs, c, cuts) = (fs2.clone(), c2.clone(), cuts2.clone());
            Box::pin(async move {
                assert_eq!(host, "h1");
                c.fetch_add(1, Ordering::SeqCst);
                let (s, cut) = cuttable(fs).await;
                cuts.lock().unwrap().push(cut);
                Ok(s)
            })
        }));
        let r = pool
            .run("h1", |s| async move { s.read_f("/proj/a.txt", 100).await })
            .await;
        assert_eq!(r.unwrap().0, b"hello\nworld\n");
        // The session is reused.
        pool.run("h1", |s| async move { s.stat_f("/proj").await })
            .await
            .unwrap();
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        // A filesystem error is an answer, not a reason to reconnect.
        let e = pool
            .run("h1", |s| async move { s.stat_f("/nope").await })
            .await
            .unwrap_err();
        assert!(e.contains("no such file"), "{e}");
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        // Cut the wire: the next call reconnects once and succeeds.
        cuts.lock().unwrap()[0].abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let r = pool
            .run("h1", |s| async move { s.read_f("/proj/a.txt", 100).await })
            .await;
        assert_eq!(r.unwrap().0, b"hello\nworld\n");
        assert_eq!(connects.load(Ordering::SeqCst), 2);
        pool.forget("h1").await;
        pool.run("h1", |s| async move { s.stat_f("/proj").await })
            .await
            .unwrap();
        assert_eq!(connects.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn pool_gives_up_after_one_failed_reconnect() {
        let fs = sample_fs();
        let connects = Arc::new(AtomicUsize::new(0));
        let c2 = connects.clone();
        let pool = Pool::new(Arc::new(move |_host: String| -> ConnectFut {
            let (fs, c) = (fs.clone(), c2.clone());
            Box::pin(async move {
                if c.fetch_add(1, Ordering::SeqCst) > 0 {
                    return Err("ssh: Permission denied (publickey).".into());
                }
                let (s, cut) = cuttable(fs).await;
                cut.abort();
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(s)
            })
        }));
        let e = pool
            .run("h", |s| async move { s.stat_f("/proj").await })
            .await
            .unwrap_err();
        assert!(e.contains("Permission denied (publickey)"), "{e}");
        assert_eq!(connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn marker_scan_skips_startup_noise_and_keeps_the_rest() {
        let data = b"Welcome!\n__OPENLEASH_PGID__=777\nreal output\n".to_vec();
        let mut r = BufReader::new(std::io::Cursor::new(data));
        assert_eq!(read_marker(&mut r).await.unwrap(), Some(777));
        let mut rest = String::new();
        r.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "real output\n");
        // No marker: None, not a hang or a guess.
        let mut r = BufReader::new(std::io::Cursor::new(b"ssh: connect refused\n".to_vec()));
        assert_eq!(read_marker(&mut r).await.unwrap(), None);
        // Endless junk is bounded.
        let junk = vec![b'x'; (MARKER_SCAN_MAX as usize) * 2];
        let mut r = BufReader::new(std::io::Cursor::new(junk));
        assert_eq!(read_marker(&mut r).await.unwrap(), None);
    }

    #[test]
    fn default_env_is_overridable() {
        let env = [("CI", "0".to_string()), ("X", "y".to_string())];
        let v = with_defaults(&env);
        assert_eq!(
            v,
            [
                ("GIT_TERMINAL_PROMPT", "0"),
                ("NO_COLOR", "1"),
                ("CI", "0"),
                ("X", "y")
            ]
        );
    }

    #[test]
    fn stat_decodes_file_types_exactly() {
        let a = |m| FileAttributes {
            permissions: Some(m),
            ..FileAttributes::empty()
        };
        let l = Stat::from_attrs(&a(0o120777));
        assert!(l.is_symlink && !l.is_file && !l.is_dir);
        let s = Stat::from_attrs(&a(0o140755));
        assert!(
            !s.is_dir && !s.is_file && !s.is_symlink,
            "a socket is none of them"
        );
        assert_eq!(Stat::from_attrs(&a(0o104755)).mode, 0o4755);
        assert!(Stat::from_attrs(&a(0o040700)).is_dir);
    }
}
