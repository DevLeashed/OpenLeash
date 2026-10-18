//! Shell execution: foreground commands with timeout/interrupt, plus
//! long-lived background jobs the agent can poll (dev servers, watchers).

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

const MAX_OUT: usize = 30_000;

/// Which shell we drive. Git Bash on Windows when present (the model is far
/// better at POSIX sh than at cmd/PowerShell), PowerShell otherwise.
#[derive(Clone, Debug)]
pub struct ShellKind {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub name: &'static str,
}

pub fn shell() -> ShellKind {
    #[cfg(windows)]
    {
        let candidates = [
            std::env::var("OPENLEASH_BASH").ok().map(PathBuf::from),
            Some(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")),
            Some(PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe")),
            dirs::home_dir().map(|h| h.join(r"AppData\Local\Programs\Git\bin\bash.exe")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return ShellKind {
                    program: c,
                    args: vec!["-c".into()],
                    name: "bash (Git Bash)",
                };
            }
        }
        ShellKind {
            program: "powershell.exe".into(),
            args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
            ],
            name: "PowerShell",
        }
    }
    #[cfg(not(windows))]
    {
        ShellKind {
            program: "/bin/bash".into(),
            args: vec!["-c".into()],
            name: "bash",
        }
    }
}

/// A project inside a WSL distro, addressed the way Windows sees it:
/// `\\wsl.localhost\<distro>\<path>` (or the older `\\wsl$\...`). Returns
/// the distro and the Linux path. Commands for such a project run *inside*
/// the distro through `wsl.exe`, so the agent gets the project's real
/// toolchain; the file tools keep using the UNC path, which Windows serves
/// natively.
pub fn wsl_target(path: &str) -> Option<(String, String)> {
    let p = path.replace('/', "\\");
    let rest = p
        .strip_prefix("\\\\wsl.localhost\\")
        .or_else(|| p.strip_prefix("\\\\wsl$\\"))
        .or_else(|| p.strip_prefix("\\\\?\\UNC\\wsl.localhost\\"))
        .or_else(|| p.strip_prefix("\\\\?\\UNC\\wsl$\\"))?;
    let mut parts = rest.splitn(2, '\\');
    let distro = parts.next().filter(|d| !d.is_empty())?.to_string();
    // A distro name never starts with `-`: wsl.exe would read it as a flag.
    if distro.starts_with('-') {
        return None;
    }
    let linux = parts
        .next()
        .unwrap_or("")
        .trim_end_matches('\\')
        .replace('\\', "/");
    Some((distro, format!("/{linux}")))
}

/// Map a Windows path argument for a command running in `distro`: a path in
/// the same distro becomes its Linux path, a drive path becomes `/mnt/<drive>`.
/// Anything else is passed through untouched.
pub fn wsl_arg(distro: &str, arg: &str) -> String {
    if let Some((d, linux)) = wsl_target(arg) {
        if d.eq_ignore_ascii_case(distro) {
            return linux;
        }
    }
    let b = arg.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return format!(
            "/mnt/{}/{}",
            (b[0] as char).to_ascii_lowercase(),
            arg[3..].replace('\\', "/")
        );
    }
    arg.to_string()
}

/// The local shell must never be started "in" a remote project's `ssh://`
/// URI: that command would run on *this* machine (and on Windows the URI is
/// a relative local path). Remote projects use `run_remote`/`spawn_remote`.
fn local_cwd(cwd: &str) -> Result<(), String> {
    if super::host::is_ssh(cwd) {
        return Err(format!(
            "internal error: {cwd} is a remote (SSH) folder; refusing to run the command locally"
        ));
    }
    Ok(())
}

fn command(cmd: &str, cwd: &str) -> Command {
    if let Some((distro, linux)) = wsl_target(cwd).filter(|_| cfg!(windows)) {
        // `--exec` hands the arguments to bash verbatim, with no default
        // shell re-parsing the command line in between.
        let mut c = Command::new("wsl.exe");
        c.args(["-d", &distro, "--cd", &linux, "--exec", "bash", "-lc", cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c.env("GIT_TERMINAL_PROMPT", "0")
            .env("NO_COLOR", "1")
            .env("CI", "1")
            .env("WSLENV", "GIT_TERMINAL_PROMPT/u:NO_COLOR/u:CI/u");
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        return c;
    }
    let sh = shell();
    let mut c = Command::new(&sh.program);
    c.args(&sh.args)
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .env("CI", "1");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Kill the whole process tree (bash → node → …), not just the shell.
pub fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut k = std::process::Command::new("taskkill");
        k.args(["/T", "/F", "/PID", &pid.to_string()]);
        use std::os::windows::process::CommandExt;
        k.creation_flags(0x0800_0000);
        let _ = k.output();
    }
    let _ = child.start_kill();
}

pub fn truncate_output(s: &str) -> String {
    truncate_output_with(s, true)
}

/// `truncate_output`, choosing whether to keep the whole output on disk.
///
/// A remote (SSH) project passes `false`: the spill file is on *this*
/// machine, but that agent's `read_file`/`grep` go to the remote host, so
/// pointing it at a local spill path would send it looking for a file that
/// isn't there (or, worse, one that is, under the same name, remotely).
pub fn truncate_output_with(s: &str, keep: bool) -> String {
    if s.len() <= MAX_OUT {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_OUT / 3).collect();
    let tail_start = s
        .char_indices()
        .rev()
        .nth(MAX_OUT * 2 / 3)
        .map(|x| x.0)
        .unwrap_or(0);
    let saved = if keep {
        spill(s)
            .map(|p| {
                format!(
                    " Full output saved to {} — grep/read_file it instead of re-running.",
                    p.display()
                )
            })
            .unwrap_or_default()
    } else {
        " To see all of it, re-run with the output redirected to a file and grep/read_file that."
            .to_string()
    };
    format!(
        "{head}\n\n… [{} characters truncated.{saved}] …\n\n{}",
        s.len() - MAX_OUT,
        &s[tail_start..]
    )
}

/// Keep the whole of a truncated output on disk (newest 50 kept).
fn spill(s: &str) -> Option<std::path::PathBuf> {
    let dir = super::store::data_dir().join("spill");
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(format!(
        "{}-{}.txt",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        super::new_id()
    ));
    std::fs::write(&p, s).ok()?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        if files.len() > 50 {
            files.sort();
            for f in &files[..files.len() - 50] {
                let _ = std::fs::remove_file(f);
            }
        }
    }
    Some(p)
}

pub struct ExecResult {
    pub output: String,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub interrupted: bool,
}

pub async fn run(
    cmd: &str,
    cwd: &str,
    timeout_ms: u64,
    cancel: &CancellationToken,
) -> Result<ExecResult, String> {
    run_env(cmd, cwd, &[], timeout_ms, cancel).await
}

/// `run` with extra environment variables (hooks get the tool call this way).
pub async fn run_env(
    cmd: &str,
    cwd: &str,
    env: &[(&str, String)],
    timeout_ms: u64,
    cancel: &CancellationToken,
) -> Result<ExecResult, String> {
    local_cwd(cwd)?;
    let mut c = command(cmd, cwd);
    for (k, v) in env {
        c.env(k, v);
    }
    if wsl_target(cwd).is_some() && cfg!(windows) && !env.is_empty() {
        // Windows variables only cross into WSL when WSLENV names them.
        let mut list = "GIT_TERMINAL_PROMPT/u:NO_COLOR/u:CI/u".to_string();
        for (k, _) in env {
            list.push(':');
            list.push_str(k);
            list.push_str("/u");
        }
        c.env("WSLENV", list);
    }
    run_built(c, timeout_ms, cancel, None).await
}

/// `run` for a remote-SSH project (`cwd` is its `ssh://` URI): the command
/// runs on the host in that directory, and a timeout or cancel kills the
/// remote process group (`ssh_session::exec`). stdout then stderr, since the
/// two arrive on separate channels.
pub async fn run_remote(
    cmd: &str,
    cwd: &str,
    timeout_ms: u64,
    cancel: &CancellationToken,
) -> Result<ExecResult, String> {
    let p = super::host::parse_ssh(cwd)
        .ok_or_else(|| format!("{cwd} is not a valid SSH project path."))?;
    let r = super::ssh_session::exec(
        &p.host,
        &p.path,
        cmd,
        &[],
        None,
        std::time::Duration::from_millis(timeout_ms),
        cancel,
    )
    .await?;
    let mut out = String::from_utf8_lossy(&r.stdout).to_string();
    let err = String::from_utf8_lossy(&r.stderr);
    if !err.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&err);
    }
    Ok(ExecResult {
        output: truncate_output_with(&out, false),
        code: r.code,
        timed_out: r.timed_out,
        interrupted: r.interrupted,
    })
}

/// `run`, inside a docker workspace's container. `cwd` is the host path; it
/// is mapped to the matching directory under the container's mount.
pub async fn run_docker(
    denv: &super::docker::DockerEnv,
    cmd: &str,
    cwd: &str,
    timeout_ms: u64,
    cancel: &CancellationToken,
) -> Result<ExecResult, String> {
    let cfg = super::docker::load_config(&denv.project).unwrap_or_default();
    let job = format!("fg_{}", &super::new_id()[..8]);
    let c = docker_exec(denv, &job, cmd, cwd, &cfg);
    run_built(c, timeout_ms, cancel, Some((denv.clone(), job))).await
}

fn docker_exec(
    denv: &super::docker::DockerEnv,
    job: &str,
    cmd: &str,
    cwd: &str,
    cfg: &super::docker::Config,
) -> Command {
    let mut c = super::docker::docker_command();
    c.args(super::docker::exec_args(denv, job, cmd, cwd, &[], cfg));
    c
}

/// Spawn a built command and collect its output, killing it on timeout or
/// cancel. `docker_job` names the in-container process tree to kill too:
/// ending the local `docker exec` client leaves the real process running.
async fn run_built(
    mut c: Command,
    timeout_ms: u64,
    cancel: &CancellationToken,
    docker_job: Option<(super::docker::DockerEnv, String)>,
) -> Result<ExecResult, String> {
    let mut child = c
        .spawn()
        .map_err(|e| format!("failed to spawn shell: {e}"))?;
    let buf = Arc::new(Mutex::new(String::new()));
    let r1 = pump(child.stdout.take(), buf.clone());
    let r2 = pump(child.stderr.take(), buf.clone());
    let mut timed_out = false;
    let mut interrupted = false;
    let status = tokio::select! {
        s = child.wait() => s.ok(),
        _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => { timed_out = true; kill_tree(&mut child); None }
        _ = cancel.cancelled() => { interrupted = true; kill_tree(&mut child); None }
    };
    if let (true, Some((denv, job))) = (timed_out || interrupted, &docker_job) {
        Box::pin(super::docker::kill_job(denv, job)).await;
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let _ = r1.await;
        let _ = r2.await;
    })
    .await;
    let output = buf.lock().unwrap().clone();
    Ok(ExecResult {
        output: truncate_output(&output),
        code: status.and_then(|s| s.code()),
        timed_out,
        interrupted,
    })
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    r: Option<R>,
    buf: Arc<Mutex<String>>,
) -> tokio::task::JoinHandle<()> {
    pump_counted(r, buf, Arc::new(AtomicUsize::new(0)))
}

/// `pump`, also counting the bytes the bound drops off the front. A reader that
/// keeps its own position (a `wait_for_event` job trigger) needs the count to
/// turn "absolute bytes seen" into an index into what is left of the buffer;
/// without it, a trim after 2 MB would make it re-read or skip output.
fn pump_counted<R: AsyncRead + Unpin + Send + 'static>(
    r: Option<R>,
    buf: Arc<Mutex<String>>,
    dropped: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(mut r) = r else { return };
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut b = buf.lock().unwrap();
                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    // Keep background buffers bounded.
                    if b.len() > 2_000_000 {
                        let cut = b.len() - 1_000_000;
                        let cut = (cut..b.len()).find(|i| b.is_char_boundary(*i)).unwrap_or(0);
                        b.drain(..cut);
                        dropped.fetch_add(cut, Ordering::SeqCst);
                    }
                }
            }
        }
    })
}

// ───────────────────────────── background jobs ─────────────────────────────

pub struct BgJob {
    pub id: String,
    pub task_id: String,
    pub cmd: String,
    pub started: chrono::DateTime<chrono::Utc>,
    pub buf: Arc<Mutex<String>>,
    pub read_pos: Mutex<usize>,
    pub exit: Arc<Mutex<Option<String>>>,
    pub stop: CancellationToken,
    /// Why it was killed, if the harness (not the process) ended it.
    pub kill_reason: Arc<Mutex<Option<String>>>,
    /// Cancelled once `exit` is set.
    pub done: CancellationToken,
    /// Runs on an SSH host: its output is never spilled to a local file.
    pub remote: bool,
    /// Bytes trimmed off the front of `buf` so far (see `pump_counted`).
    pub dropped: Arc<AtomicUsize>,
}

impl BgJob {
    /// Absolute output position: every byte ever captured, trimmed or not.
    pub fn output_end(&self) -> usize {
        let b = self.buf.lock().unwrap();
        self.dropped.load(Ordering::SeqCst) + b.len()
    }

    /// Output captured after absolute position `from`, and the new end.
    ///
    /// Independent of `read_pos` on purpose: this is for a watcher, and moving
    /// the agent's own `bash_output` cursor would hide output it never saw.
    /// Output already trimmed off the front is gone; the text starts at what
    /// is left.
    pub fn output_since(&self, from: usize) -> (String, usize) {
        let b = self.buf.lock().unwrap();
        let dropped = self.dropped.load(Ordering::SeqCst);
        let start = from.saturating_sub(dropped).min(b.len());
        let start = (start..=b.len())
            .find(|i| b.is_char_boundary(*i))
            .unwrap_or(b.len());
        (b[start..].to_string(), dropped + b.len())
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct BgInfo {
    pub id: String,
    pub cmd: String,
    pub started: chrono::DateTime<chrono::Utc>,
    pub last_line: String,
    pub running: bool,
    pub exit: Option<String>,
}

#[derive(Default)]
pub struct BgManager {
    jobs: Mutex<HashMap<String, Arc<BgJob>>>,
}

impl BgManager {
    pub fn spawn(&self, task_id: &str, cmd: &str, cwd: &str) -> Result<String, String> {
        local_cwd(cwd)?;
        let id = format!("bg_{}", &super::new_id()[..6]);
        self.spawn_built(task_id, cmd, command(cmd, cwd), id, None)
    }

    /// `spawn` for a remote-SSH project: started over ssh (async, since the
    /// job first reports its remote process group), killed with
    /// `RemoteJob::kill` so the remote tree ends too, not just the client.
    pub async fn spawn_remote(
        &self,
        task_id: &str,
        cmd: &str,
        cwd: &str,
    ) -> Result<String, String> {
        let p = super::host::parse_ssh(cwd)
            .ok_or_else(|| format!("{cwd} is not a valid SSH project path."))?;
        let job = super::ssh_session::spawn_bg(&p.host, &p.path, cmd, &[]).await?;
        // Taken apart so the output pumps own the streams while the waiter
        // keeps what `RemoteJob::kill` needs (host, pgid, the local client).
        let super::ssh_session::RemoteJob {
            host,
            pgid,
            mut child,
            stdout,
            stderr,
        } = job;
        let id = format!("bg_{}", &super::new_id()[..6]);
        let buf = Arc::new(Mutex::new(String::new()));
        let dropped = Arc::new(AtomicUsize::new(0));
        pump_counted(Some(stdout), buf.clone(), dropped.clone());
        pump_counted(stderr, buf.clone(), dropped.clone());
        let (exit, stop) = (Arc::new(Mutex::new(None)), CancellationToken::new());
        let (kill_reason, done) = (Arc::new(Mutex::new(None)), CancellationToken::new());
        let bg = Arc::new(BgJob {
            id: id.clone(),
            task_id: task_id.into(),
            cmd: cmd.into(),
            started: chrono::Utc::now(),
            buf,
            read_pos: Mutex::new(0),
            exit: exit.clone(),
            stop: stop.clone(),
            kill_reason: kill_reason.clone(),
            done: done.clone(),
            remote: true,
            dropped,
        });
        self.jobs.lock().unwrap().insert(id.clone(), bg);
        self.prune_finished();
        tokio::spawn(async move {
            let waited = tokio::select! {
                s = child.wait() => Some(s),
                _ = stop.cancelled() => None,
            };
            let res = match waited {
                Some(s) => s
                    .ok()
                    .and_then(|s| s.code())
                    .map(|c| format!("exit {c}"))
                    .unwrap_or("exited".into()),
                None => {
                    let _ = super::ssh_session::kill(&host, pgid).await;
                    let _ = child.start_kill();
                    kill_reason
                        .lock()
                        .unwrap()
                        .clone()
                        .map(|r| format!("killed: {r}"))
                        .unwrap_or("killed".into())
                }
            };
            *exit.lock().unwrap() = Some(res);
            done.cancel();
        });
        Ok(id)
    }

    /// `spawn`, inside a docker workspace's container.
    pub fn spawn_docker(
        &self,
        denv: &super::docker::DockerEnv,
        task_id: &str,
        cmd: &str,
        cwd: &str,
    ) -> Result<String, String> {
        let cfg = super::docker::load_config(&denv.project).unwrap_or_default();
        let id = format!("bg_{}", &super::new_id()[..6]);
        let c = docker_exec(denv, &id, cmd, cwd, &cfg);
        self.spawn_built(task_id, cmd, c, id.clone(), Some((denv.clone(), id)))
    }

    fn spawn_built(
        &self,
        task_id: &str,
        cmd: &str,
        mut c: Command,
        id: String,
        docker_job: Option<(super::docker::DockerEnv, String)>,
    ) -> Result<String, String> {
        let mut child = c.spawn().map_err(|e| format!("failed to spawn: {e}"))?;
        let buf = Arc::new(Mutex::new(String::new()));
        let dropped = Arc::new(AtomicUsize::new(0));
        pump_counted(child.stdout.take(), buf.clone(), dropped.clone());
        pump_counted(child.stderr.take(), buf.clone(), dropped.clone());
        let exit = Arc::new(Mutex::new(None));
        let stop = CancellationToken::new();
        let (kill_reason, done) = (Arc::new(Mutex::new(None)), CancellationToken::new());
        let job = Arc::new(BgJob {
            id: id.clone(),
            task_id: task_id.into(),
            cmd: cmd.into(),
            started: chrono::Utc::now(),
            buf,
            read_pos: Mutex::new(0),
            exit: exit.clone(),
            stop: stop.clone(),
            kill_reason: kill_reason.clone(),
            done: done.clone(),
            remote: false,
            dropped,
        });
        self.jobs.lock().unwrap().insert(id.clone(), job);
        self.prune_finished();
        tokio::spawn(async move {
            let res = tokio::select! {
                s = child.wait() => s.ok().and_then(|s| s.code()).map(|c| format!("exit {c}")).unwrap_or("exited".into()),
                _ = stop.cancelled() => {
                    kill_tree(&mut child);
                    if let Some((denv, job)) = &docker_job {
                        Box::pin(super::docker::kill_job(denv, job)).await;
                    }
                    kill_reason.lock().unwrap().clone().map(|r| format!("killed: {r}")).unwrap_or("killed".into())
                }
            };
            *exit.lock().unwrap() = Some(res);
            done.cancel();
        });
        Ok(id)
    }

    /// Drop the output of long-finished jobs. Each job holds up to 2 MB of
    /// captured output and the map never shrank, so a chat that started a
    /// hundred dev servers or watchers kept all of it for the life of the
    /// process. Running jobs are never touched, and the most recent finished
    /// ones are kept so their output can still be read.
    fn prune_finished(&self) {
        /// Finished jobs kept per task, newest first.
        const KEEP: usize = 8;
        let mut jobs = self.jobs.lock().unwrap();
        let mut done: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = jobs
            .iter()
            .filter(|(_, j)| j.exit.lock().unwrap().is_some())
            .map(|(id, j)| (id.clone(), j.task_id.clone(), j.started))
            .collect();
        if done.len() <= KEEP {
            return;
        }
        // Newest first, then drop everything past the per-task cap.
        done.sort_by_key(|a| std::cmp::Reverse(a.2));
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (id, task, _) in done {
            let n = seen.entry(task).or_default();
            *n += 1;
            if *n > KEEP {
                jobs.remove(&id);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<BgJob>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    /// New output since the last read.
    pub fn read_new(&self, id: &str) -> Option<(String, Option<String>)> {
        let j = self.get(id)?;
        let buf = j.buf.lock().unwrap();
        let mut pos = j.read_pos.lock().unwrap();
        let start = (*pos).min(buf.len());
        let start = (start..=buf.len())
            .find(|i| buf.is_char_boundary(*i))
            .unwrap_or(buf.len());
        let out = buf[start..].to_string();
        *pos = buf.len();
        drop(pos);
        drop(buf);
        let exit = j.exit.lock().unwrap().clone();
        Some((truncate_output_with(&out, !j.remote), exit))
    }

    pub fn kill(&self, id: &str) -> bool {
        match self.get(id) {
            Some(j) => {
                j.stop.cancel();
                true
            }
            None => false,
        }
    }

    pub fn kill_task(&self, task_id: &str) {
        for j in self.jobs.lock().unwrap().values() {
            if j.task_id == task_id {
                j.stop.cancel();
            }
        }
    }

    /// Kill every running job of a task, recording why (shown to the agent as its exit).
    pub fn kill_task_with(&self, task_id: &str, reason: &str) {
        self.kill_task_with_reasons(task_id, reason);
    }

    /// `kill_task_with`, plus the jobs it actually stopped. A job whose process
    /// had already exited is left out, so a caller telling the agent what was
    /// cut off never claims credit for killing something that had finished.
    pub fn kill_task_with_reasons(&self, task_id: &str, reason: &str) -> Vec<BgInfo> {
        let mut killed: Vec<BgInfo> = vec![];
        for j in self.jobs.lock().unwrap().values() {
            if j.task_id == task_id && j.exit.lock().unwrap().is_none() {
                // Read it as it was *before* the kill: the process may record
                // its exit the instant the token fires, and a job that was
                // already on its way out is not one the user stopped.
                let before = bg_info(j);
                *j.kill_reason.lock().unwrap() = Some(reason.to_string());
                j.stop.cancel();
                killed.push(before);
            }
        }
        killed.sort_by_key(|b| b.started);
        killed
    }

    pub fn list(&self, task_id: &str) -> Vec<BgInfo> {
        let mut v: Vec<BgInfo> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| j.task_id == task_id)
            .map(|j| bg_info(j))
            .collect();
        v.sort_by_key(|b| b.started);
        v
    }
}

/// One job as the UI sees it: `running` until the process records an exit.
fn bg_info(j: &BgJob) -> BgInfo {
    let exit = j.exit.lock().unwrap().clone();
    let last_line = j
        .buf
        .lock()
        .unwrap()
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    BgInfo {
        id: j.id.clone(),
        cmd: j.cmd.clone(),
        started: j.started,
        last_line,
        running: exit.is_none(),
        exit,
    }
}

#[cfg(test)]
mod wsl_tests {
    use super::*;

    #[test]
    fn wsl_paths_parse() {
        assert_eq!(
            wsl_target(r"\\wsl.localhost\Ubuntu\home\me\app"),
            Some(("Ubuntu".into(), "/home/me/app".into()))
        );
        assert_eq!(
            wsl_target(r"\\wsl$\Debian\srv\"),
            Some(("Debian".into(), "/srv".into()))
        );
        assert_eq!(
            wsl_target("//wsl.localhost/Ubuntu"),
            Some(("Ubuntu".into(), "/".into()))
        );
        assert_eq!(
            wsl_target(r"\\?\UNC\wsl.localhost\Ubuntu\x"),
            Some(("Ubuntu".into(), "/x".into()))
        );
        assert_eq!(wsl_target(r"C:\code\app"), None);
        assert_eq!(wsl_target(r"\\server\share\x"), None);
        assert_eq!(
            wsl_target(r"\\wsl.localhost\-x\y"),
            None,
            "a distro can't smuggle in a flag"
        );
    }

    #[test]
    fn wsl_args_map_windows_paths() {
        assert_eq!(
            wsl_arg("Ubuntu", r"C:\Users\me\.openleash\worktrees\a"),
            "/mnt/c/Users/me/.openleash/worktrees/a"
        );
        assert_eq!(
            wsl_arg("Ubuntu", r"\\wsl.localhost\Ubuntu\home\me"),
            "/home/me"
        );
        assert_eq!(
            wsl_arg("Ubuntu", r"\\wsl.localhost\Debian\home\me"),
            r"\\wsl.localhost\Debian\home\me"
        );
        assert_eq!(wsl_arg("Ubuntu", "HEAD"), "HEAD");
        assert_eq!(wsl_arg("Ubuntu", "--quiet"), "--quiet");
    }
}
