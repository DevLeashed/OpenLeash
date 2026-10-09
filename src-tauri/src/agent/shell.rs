//! Shell execution: foreground commands with timeout/interrupt, plus
//! long-lived background jobs the agent can poll (dev servers, watchers).

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
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

fn command(cmd: &str, cwd: &str) -> Command {
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
    let saved = spill(s)
        .map(|p| {
            format!(
                " Full output saved to {} — grep/read_file it instead of re-running.",
                p.display()
            )
        })
        .unwrap_or_default();
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
    let mut c = command(cmd, cwd);
    for (k, v) in env {
        c.env(k, v);
    }
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
        let mut child = command(cmd, cwd)
            .spawn()
            .map_err(|e| format!("failed to spawn: {e}"))?;
        let id = format!("bg_{}", &super::new_id()[..6]);
        let buf = Arc::new(Mutex::new(String::new()));
        pump(child.stdout.take(), buf.clone());
        pump(child.stderr.take(), buf.clone());
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
        });
        self.jobs.lock().unwrap().insert(id.clone(), job);
        self.prune_finished();
        tokio::spawn(async move {
            let res = tokio::select! {
                s = child.wait() => s.ok().and_then(|s| s.code()).map(|c| format!("exit {c}")).unwrap_or("exited".into()),
                _ = stop.cancelled() => {
                    kill_tree(&mut child);
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
        Some((truncate_output(&out), exit))
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
