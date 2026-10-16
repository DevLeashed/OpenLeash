//! Docker workspaces: a chat that runs its commands in a container.
//!
//! The third workspace mode next to "local" and "worktree". The task gets a
//! private copy of the project on the host (a `git clone` for repositories, a
//! filtered copy otherwise) under `~/.openleash/workspaces/<task>`, bind-mounted
//! into a container built from the project's `.openleash_docker/` directory.
//!
//! Only *commands* cross into the container. Files stay on the host side of
//! the mount, so every file tool, the read-before-write and staleness checks,
//! checkpoints and the review view work exactly as they do for a worktree. That
//! is deliberate: re-implementing file access over `docker exec` would mean a
//! second copy of the safety rules around edits, and the first copy is the one
//! that is tested.
//!
//! Containers belong to the harness, not the user. They are started when a
//! message is sent, stopped after a stretch of inactivity, rebuilt when the
//! contents of `.openleash_docker/` change, and removed with their chat.
//!
//! This is isolation for the project, not a security boundary: the container
//! shares the Docker daemon's privileges, the workspace is writable from both
//! sides, and the agent's file tools still run on the host. See
//! `docs/security-model.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// The project directory that opts a project into docker workspaces. It is the
/// image's build context: everything the Dockerfile needs lives in it.
pub const CONFIG_DIR: &str = ".openleash_docker";
/// Optional harness settings for the container, inside `CONFIG_DIR`.
pub const CONFIG_FILE: &str = "openleash.json";
/// Where the workspace is mounted unless `openleash.json` says otherwise.
pub const DEFAULT_GUEST_ROOT: &str = "/workspace";
/// Minutes without activity before a container is stopped.
pub const DEFAULT_IDLE_MINUTES: u64 = 15;
/// Label every container and image we create carries, so the manager can find
/// them (and orphans) without guessing from names.
pub const LABEL_TASK: &str = "openleash.task";
pub const LABEL_PROJECT: &str = "openleash.project";

/// A task's container, persisted on the task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DockerEnv {
    /// Container name, `openleash-<task id>`.
    pub container: String,
    /// The project whose `.openleash_docker/` defines the image.
    pub project: String,
    /// The task's workspace on the host (the bind mount's source).
    pub host_root: String,
    /// Where that workspace appears inside the container.
    pub guest_root: String,
}

/// `.openleash_docker/openleash.json`. Every field is optional.
///
/// Deliberately small. In particular there is no way to pass raw `docker run`
/// flags: the file comes from the repository, and `--privileged` or
/// `-v /:/host` from a cloned repo would turn "run this project in a box" into
/// "give this project the machine".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Mount point for the workspace inside the container.
    pub workdir: Option<String>,
    /// Extra environment variables for every command.
    pub env: BTreeMap<String, String>,
    /// `"bridge"` (default) or `"none"` to cut the container off the network.
    pub network: Option<String>,
    /// Stop the container after this many idle minutes (0 = never).
    pub idle_minutes: Option<u64>,
    /// Container ports published on the host's loopback interface only.
    pub ports: Vec<u16>,
    /// Who commands run as: `"host"` (your own uid:gid, the default on Linux so
    /// files the agent creates stay yours), `"root"`, or a user the image has.
    /// Ignored on Windows and macOS, where Docker Desktop maps ownership itself.
    pub user: Option<String>,
}

impl Config {
    pub fn guest_root(&self) -> String {
        match self.workdir.as_deref().map(str::trim) {
            Some(w) if w.starts_with('/') && w.len() > 1 && !w.contains("..") => {
                w.trim_end_matches('/').to_string()
            }
            _ => DEFAULT_GUEST_ROOT.into(),
        }
    }

    /// The `--user` value for `docker exec`, if any. On Linux a container
    /// running as root writes root-owned files into the bind-mounted workspace,
    /// which the user then can't edit or delete without sudo.
    pub fn exec_user(&self, host_root: &str) -> Option<String> {
        let want = self.user.as_deref().map(str::trim).unwrap_or("host");
        if !cfg!(target_os = "linux") {
            return match want {
                "host" | "" => None,
                u => Some(u.to_string()),
            };
        }
        match want {
            "root" => Some("root".into()),
            "host" | "" => host_ids(host_root),
            u => Some(u.to_string()),
        }
    }

    pub fn network(&self) -> &str {
        match self.network.as_deref() {
            Some("none") => "none",
            _ => "bridge",
        }
    }
}

/// `uid:gid` of whoever owns the workspace, read from the directory itself so
/// no libc binding is needed.
#[allow(unused_variables)]
fn host_ids(host_root: &str) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(host_root).ok()?;
        Some(format!("{}:{}", m.uid(), m.gid()))
    }
    #[cfg(not(unix))]
    None
}

/// Mount flag suffix: SELinux hosts (Fedora, RHEL) refuse container access to
/// a bind mount without a relabel. Docker Desktop has no SELinux, so only Linux.
fn mount_suffix() -> &'static str {
    if cfg!(target_os = "linux") {
        ":z"
    } else {
        ""
    }
}

pub fn config_dir(project: &str) -> PathBuf {
    Path::new(project).join(CONFIG_DIR)
}

/// Does this project opt into docker workspaces?
pub fn has_config(project: &str) -> bool {
    config_dir(project).join("Dockerfile").is_file()
}

const TEMPLATE_DOCKERFILE: &str = "# Build context for OpenLeash docker workspaces: everything COPYed must live in this folder.\nFROM node:22-bookworm\nRUN apt-get update && apt-get install -y --no-install-recommends git ripgrep && rm -rf /var/lib/apt/lists/*\n";

/// Writes a starter `.openleash_docker/Dockerfile`. Never overwrites: an
/// existing Dockerfile is the user's work.
pub fn add_template(project: &str) -> Result<(), String> {
    let dir = config_dir(project);
    let file = dir.join("Dockerfile");
    if file.exists() {
        return Err(format!("{CONFIG_DIR}/Dockerfile already exists."));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{CONFIG_DIR}: {e}"))?;
    std::fs::write(&file, TEMPLATE_DOCKERFILE).map_err(|e| format!("{CONFIG_DIR}/Dockerfile: {e}"))
}

pub fn load_config(project: &str) -> Result<Config, String> {
    let p = config_dir(project).join(CONFIG_FILE);
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{CONFIG_DIR}/{CONFIG_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{CONFIG_DIR}/{CONFIG_FILE}: {e}")),
    }
}

/// Content hash of the build context: every file's relative path and bytes,
/// in a stable order. Any change to anything in `.openleash_docker/` changes
/// the image tag, which is what triggers a rebuild.
pub fn context_hash(project: &str) -> Result<String, String> {
    let root = config_dir(project);
    if !root.is_dir() {
        return Err(format!("{CONFIG_DIR} not found in {project}"));
    }
    let mut files: Vec<PathBuf> = vec![];
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for e in rd.flatten() {
            let ft = e.file_type().map_err(|e| e.to_string())?;
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                files.push(e.path());
            }
        }
        if files.len() > 10_000 {
            return Err(format!("{CONFIG_DIR} has too many files to hash"));
        }
    }
    let rel = |p: &Path| {
        p.strip_prefix(&root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    files.sort_by_key(|p| rel(p));
    let mut h = Sha256::new();
    for f in &files {
        let r = rel(f);
        let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
        h.update((r.len() as u64).to_le_bytes());
        h.update(r.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Lowercase `[a-z0-9-]`, as Docker's repository-name grammar requires.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').chars().take(40).collect::<String>();
    if out.is_empty() {
        "project".into()
    } else {
        out
    }
}

/// `openleash/<project>-<path hash>:<context hash>`. The path hash keeps two
/// checkouts named `app` from sharing (and rebuilding over) one repository.
pub fn image_tag(project: &str, hash: &str) -> String {
    let name = Path::new(project)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut d = Sha256::new();
    d.update(super::trust::norm_path(project).as_bytes());
    let id: String = d
        .finalize()
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "openleash/{}-{id}:{}",
        slug(&name),
        &hash[..hash.len().min(12)]
    )
}

pub fn container_name(task_id: &str) -> String {
    format!("openleash-{}", slug(task_id))
}

pub fn workspaces_dir() -> PathBuf {
    super::store::data_dir().join("workspaces")
}

/// Map a host path inside the workspace to the path a command in the
/// container sees. Paths outside the workspace map to the guest root.
pub fn guest_path(env: &DockerEnv, host: &str) -> String {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_string();
    let (root, host) = (norm(&env.host_root), norm(host));
    let rel = if host.eq_ignore_ascii_case(&root) {
        ""
    } else if host.len() > root.len()
        && host[..root.len()].eq_ignore_ascii_case(&root)
        && host.as_bytes()[root.len()] == b'/'
    {
        &host[root.len() + 1..]
    } else {
        ""
    };
    if rel.is_empty() {
        env.guest_root.clone()
    } else {
        format!("{}/{rel}", env.guest_root)
    }
}

/// The inverse: a container path the model copied out of command output
/// (`/workspace/src/main.rs`) mapped back to the host file it names.
pub fn host_path(env: &DockerEnv, guest: &str) -> Option<PathBuf> {
    let g = guest.trim();
    let rest = if g == env.guest_root {
        ""
    } else {
        g.strip_prefix(&format!("{}/", env.guest_root))?
    };
    if rest.split('/').any(|s| s == "..") {
        return None;
    }
    let mut p = PathBuf::from(&env.host_root);
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    Some(p)
}

/// Most files a non-git project may have before we refuse to copy it: past
/// this it is almost certainly a home directory or a build tree, not a project.
const MAX_COPY_FILES: usize = 100_000;

/// Create a task's private workspace and return `(host dir, branch)`. A git
/// project gets a clone on a fresh `ol/<slug>` branch; anything else gets a
/// copy that skips what `.gitignore`/`.ignore` exclude (dependency folders and
/// build output get rebuilt inside the container anyway).
pub fn create_workspace(
    project: &str,
    task_id: &str,
    base: &str,
    slug: &str,
    is_repo: bool,
) -> Result<(String, String), String> {
    let dir = workspaces_dir().join(task_id);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let dir_s = dir.to_string_lossy().to_string();
    if is_repo {
        let branch = super::git::clone_workspace(project, &dir_s, base, slug).map_err(|e| {
            let _ = std::fs::remove_dir_all(&dir);
            format!("couldn't clone the project: {e}")
        })?;
        return Ok((dir_s, branch));
    }
    copy_project(Path::new(project), &dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&dir);
        e
    })?;
    Ok((dir_s, String::new()))
}

fn copy_project(src: &Path, dst: &Path) -> Result<usize, String> {
    let walk = ignore::WalkBuilder::new(src)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .build();
    let mut n = 0usize;
    for e in walk.flatten() {
        let Ok(rel) = e.path().strip_prefix(src) else {
            continue;
        };
        if rel.as_os_str().is_empty()
            || rel
                .components()
                .next()
                .is_some_and(|c| c.as_os_str() == ".git")
        {
            continue;
        }
        let to = dst.join(rel);
        match e.file_type() {
            Some(t) if t.is_dir() => {
                std::fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            }
            Some(t) if t.is_file() => {
                n += 1;
                if n > MAX_COPY_FILES {
                    return Err(format!("the project has more than {MAX_COPY_FILES} files to copy; make it a git repository or add an .ignore file"));
                }
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                std::fs::copy(e.path(), &to)
                    .map_err(|err| format!("{}: {err}", e.path().display()))?;
            }
            // Symlinks are skipped: one pointing outside the project would let
            // the container write through it to the host.
            _ => {}
        }
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    Ok(n)
}

// ───────────────────────────── registry ─────────────────────────────

/// Live state the harness keeps per docker task. Containers are cheap to ask
/// about but not free (`docker inspect` is a process spawn), and the idle
/// reaper needs to know when each was last used.
#[derive(Debug, Clone, Default)]
pub struct Live {
    pub env: Option<DockerEnv>,
    pub state: State,
    pub last_active: Option<Instant>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Unknown,
    Building,
    Starting,
    Running,
    Stopped,
    Error,
}

struct Registry {
    live: Mutex<HashMap<String, Live>>,
    /// Per-task async lock so a message and a tool call that both need the
    /// container start it once, not twice.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-project build lock: two chats on one project share one image.
    builds: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| Registry {
        live: Mutex::new(HashMap::new()),
        locks: Mutex::new(HashMap::new()),
        builds: Mutex::new(HashMap::new()),
    })
}

pub fn register(task_id: &str, env: &DockerEnv) {
    let mut l = registry().live.lock().unwrap();
    let e = l.entry(task_id.to_string()).or_default();
    e.env = Some(env.clone());
}

pub fn unregister(task_id: &str) {
    registry().live.lock().unwrap().remove(task_id);
}

pub fn live(task_id: &str) -> Option<Live> {
    registry().live.lock().unwrap().get(task_id).cloned()
}

pub fn all_live() -> Vec<(String, Live)> {
    registry()
        .live
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn set_state(task_id: &str, state: State, detail: &str) {
    let mut l = registry().live.lock().unwrap();
    let e = l.entry(task_id.to_string()).or_default();
    e.state = state;
    e.detail = detail.to_string();
    if state == State::Running {
        e.last_active = Some(Instant::now());
    }
}

/// Note activity so the idle reaper leaves this container alone.
pub fn touch(task_id: &str) {
    if let Some(e) = registry().live.lock().unwrap().get_mut(task_id) {
        e.last_active = Some(Instant::now());
    }
}

/// The docker environment whose workspace contains `cwd`, if any. This is how
/// path resolution and the system prompt learn about the container without
/// every caller threading the task through.
pub fn env_for_cwd(cwd: &str) -> Option<DockerEnv> {
    let c = cwd.replace('\\', "/").to_lowercase();
    registry().live.lock().unwrap().values().find_map(|l| {
        let e = l.env.as_ref()?;
        let r = e
            .host_root
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase();
        (c == r || c.starts_with(&format!("{r}/"))).then(|| e.clone())
    })
}

fn task_lock(task_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    registry()
        .locks
        .lock()
        .unwrap()
        .entry(task_id.to_string())
        .or_default()
        .clone()
}

fn build_lock(project: &str) -> Arc<tokio::sync::Mutex<()>> {
    registry()
        .builds
        .lock()
        .unwrap()
        .entry(super::trust::norm_path(project))
        .or_default()
        .clone()
}

// ───────────────────────────── docker CLI ─────────────────────────────

pub fn docker_command() -> Command {
    let mut c = Command::new(std::env::var("OPENLEASH_DOCKER").unwrap_or_else(|_| "docker".into()));
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Run `docker <args>` and return stdout, or stderr as the error.
pub async fn docker(args: &[&str]) -> Result<String, String> {
    let out = docker_command().args(args).output().await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "Docker isn't installed (no `docker` command found).".to_string()
        } else {
            format!("couldn't run docker: {e}")
        }
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("docker {} failed", args.first().unwrap_or(&""))
        } else {
            err
        })
    }
}

/// Docker's server version if the CLI exists and the daemon answers.
pub async fn available() -> Result<String, String> {
    match tokio::time::timeout(
        Duration::from_secs(8),
        docker(&["version", "--format", "{{.Server.Version}}"]),
    )
    .await
    {
        Ok(Ok(v)) if !v.is_empty() => Ok(v),
        Ok(Ok(_)) => Err("Docker is installed but its engine isn't running.".into()),
        Ok(Err(e)) if e.contains("isn't installed") => Err(e),
        Ok(Err(_)) => Err("Docker is installed but its engine isn't running. Start Docker Desktop (or the docker service) and try again.".into()),
        Err(_) => Err("Docker didn't answer. Is the engine running?".into()),
    }
}

pub async fn image_id(tag: &str) -> Option<String> {
    docker(&["image", "inspect", "--format", "{{.Id}}", tag])
        .await
        .ok()
}

pub async fn latest_image_tag(project: &str) -> Option<(String, String)> {
    let label = format!("{LABEL_PROJECT}={}", super::trust::norm_path(project));
    let out = docker(&[
        "image",
        "ls",
        "--no-trunc",
        "--filter",
        &format!("label={label}"),
        "--format",
        "{{.Repository}}:{{.Tag}} {{.ID}} {{.CreatedAt}}",
    ])
    .await
    .ok()?;
    let mut rows = out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let tag = fields.next()?;
            let id = fields.next()?;
            (tag.starts_with("openleash/") && !tag.ends_with(":<none>")).then(|| {
                (
                    tag.to_string(),
                    id.to_string(),
                    fields.collect::<Vec<_>>().join(" "),
                )
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    rows.into_iter().next().map(|(tag, id, _)| (tag, id))
}

/// Build the project's image, streaming output lines to `progress`.
pub async fn build(
    project: &str,
    tag: &str,
    no_cache: bool,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<(), String> {
    let ctx = config_dir(project);
    let label = format!("{LABEL_PROJECT}={}", super::trust::norm_path(project));
    let mut args: Vec<String> = vec![
        "build".into(),
        "--progress=plain".into(),
        "-t".into(),
        tag.into(),
        "--label".into(),
        label,
    ];
    if no_cache {
        args.push("--no-cache".into());
    }
    args.push(ctx.to_string_lossy().to_string());
    let mut child = docker_command()
        .args(&args)
        .spawn()
        .map_err(|e| format!("couldn't run docker build: {e}"))?;
    let tail: Arc<Mutex<Vec<String>>> = Default::default();
    let pump = |r: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
                tail: Arc<Mutex<Vec<String>>>| async move {
        let Some(r) = r else { return };
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let mut t = tail.lock().unwrap();
            t.push(l);
            if t.len() > 40 {
                t.remove(0);
            }
        }
    };
    let out = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let err = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let (t1, t2) = (tail.clone(), tail.clone());
    let a = tokio::spawn(pump(out, t1));
    let b = tokio::spawn(pump(err, t2));
    // Report progress from the tail on a timer rather than per line: a build
    // can print thousands of lines, and each report is a UI event.
    let status = loop {
        tokio::select! {
            s = child.wait() => break s,
            _ = tokio::time::sleep(Duration::from_millis(700)) => {
                let last = tail.lock().unwrap().last().cloned().unwrap_or_default();
                if !last.is_empty() { progress(&last); }
            }
        }
    };
    let _ = a.await;
    let _ = b.await;
    let status = status.map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        let t = tail.lock().unwrap().join("\n");
        Err(format!("docker build failed:\n{t}"))
    }
}

#[derive(Debug, Clone)]
pub struct Inspect {
    pub running: bool,
    pub image_id: String,
}

pub async fn inspect(name: &str) -> Option<Inspect> {
    let s = docker(&[
        "container",
        "inspect",
        "--format",
        "{{.State.Running}} {{.Image}}",
        name,
    ])
    .await
    .ok()?;
    let mut it = s.split_whitespace();
    Some(Inspect {
        running: it.next() == Some("true"),
        image_id: it.next().unwrap_or("").to_string(),
    })
}

/// The `docker run` arguments for a task's container. Pure, so the flags we
/// pass (and the ones we never pass) are testable.
pub fn run_args(env: &DockerEnv, task_id: &str, tag: &str, cfg: &Config) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        env.container.clone(),
        "--label".into(),
        format!("{LABEL_TASK}={task_id}"),
        "--label".into(),
        format!("{LABEL_PROJECT}={}", super::trust::norm_path(&env.project)),
        "--init".into(),
        "--network".into(),
        cfg.network().into(),
        "-v".into(),
        format!("{}:{}{}", env.host_root, env.guest_root, mount_suffix()),
        "-v".into(),
        format!(
            "{}:/tmp/OpenLeash:{}",
            super::attachments::project_dir(&env.project).display(),
            if cfg!(target_os = "linux") {
                "ro,z"
            } else {
                "ro"
            }
        ),
        "-w".into(),
        env.guest_root.clone(),
    ];
    for p in &cfg.ports {
        if *p > 0 {
            // Loopback only: a dev server in a box is not something to put on
            // the LAN just because the agent started it.
            a.push("-p".into());
            a.push(format!("127.0.0.1:{p}:{p}"));
        }
    }
    for (k, v) in &cfg.env {
        if !k.is_empty() && !k.contains('=') {
            a.push("-e".into());
            a.push(format!("{k}={v}"));
        }
    }
    // Keep the container alive whatever the image's own CMD is; commands are
    // started with `docker exec`. Needs only /bin/sh.
    a.extend([
        "--entrypoint".into(),
        "/bin/sh".into(),
        tag.into(),
        "-c".into(),
        "trap 'exit 0' TERM INT; while :; do sleep 3600 & wait $!; done".into(),
    ]);
    a
}

/// Make sure the task's container exists, runs, and uses the current image.
/// Builds the image when it is missing, recreates the container when its image
/// is stale, and — when `auto_rebuild` is false — reuses an existing image
/// instead of rebuilding over a changed `.openleash_docker/`. The workspace
/// lives on the host, so recreating the container loses nothing but processes.
pub async fn ensure_running_with_rebuild(
    task_id: &str,
    env: &DockerEnv,
    auto_rebuild: bool,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<(), String> {
    register(task_id, env);
    let lock = task_lock(task_id);
    let _g = lock.lock().await;
    // Fast path: we started it and nothing has changed since.
    if live(task_id).is_some_and(|l| l.state == State::Running) {
        let hash = context_hash(&env.project)?;
        let tag = image_tag(&env.project, &hash);
        if let (Some(i), Some(want)) = (inspect(&env.container).await, image_id(&tag).await) {
            if i.running && i.image_id == want {
                touch(task_id);
                return Ok(());
            }
        }
    }
    if let Err(e) = available().await {
        set_state(task_id, State::Error, &e);
        return Err(e);
    }
    let cfg = load_config(&env.project)?;
    let hash = context_hash(&env.project)?;
    let tag = image_tag(&env.project, &hash);
    let mut selected_tag = tag.clone();
    let mut want = image_id(&tag).await;
    if want.is_none() && !auto_rebuild {
        if let Some((cached_tag, cached_id)) = latest_image_tag(&env.project).await {
            if inspect(&env.container)
                .await
                .is_some_and(|i| i.image_id == cached_id)
            {
                progress(
                    "Automatic rebuilds are disabled; keeping the container on its current image.",
                );
                touch(task_id);
                return Ok(());
            }
            selected_tag = cached_tag;
            want = Some(cached_id);
        }
    }
    let want = match want {
        Some(id) => id,
        None => {
            set_state(task_id, State::Building, "Building the image…");
            progress("Building the docker image (first run, or .openleash_docker changed)…");
            let bl = build_lock(&env.project);
            let _b = bl.lock().await;
            // Someone else may have built it while we waited for the lock.
            if image_id(&tag).await.is_none() {
                if let Err(e) = build(&env.project, &tag, false, progress).await {
                    set_state(task_id, State::Error, "Image build failed");
                    return Err(e);
                }
            }
            image_id(&tag)
                .await
                .ok_or("the image was built but can't be found")?
        }
    };
    set_state(task_id, State::Starting, "Starting the container…");
    match inspect(&env.container).await {
        Some(i) if i.image_id == want => {
            if !i.running {
                progress("Starting the container…");
                docker(&["start", &env.container]).await.map_err(|e| {
                    set_state(task_id, State::Error, &e);
                    e
                })?;
            }
        }
        existing => {
            if existing.is_some() {
                progress("The image changed; recreating the container…");
                let _ = docker(&["rm", "-f", &env.container]).await;
            } else {
                progress("Creating the container…");
            }
            std::fs::create_dir_all(&env.host_root).map_err(|e| e.to_string())?;
            let args = run_args(env, task_id, &selected_tag, &cfg);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            docker(&refs).await.map_err(|e| {
                set_state(task_id, State::Error, &e);
                format!("couldn't start the container: {e}")
            })?;
        }
    }
    set_state(task_id, State::Running, "");
    Ok(())
}

pub async fn stop(task_id: &str, env: &DockerEnv) -> Result<(), String> {
    let lock = task_lock(task_id);
    let _g = lock.lock().await;
    let r = docker(&["stop", "-t", "5", &env.container])
        .await
        .map(|_| ());
    set_state(
        task_id,
        if r.is_ok() {
            State::Stopped
        } else {
            State::Error
        },
        "",
    );
    r
}

pub async fn remove(task_id: &str, env: &DockerEnv) -> Result<(), String> {
    let r = match docker(&["rm", "-f", &env.container]).await {
        Ok(_) => Ok(()),
        Err(e) if e.contains("No such container") => Ok(()),
        Err(e) => Err(e),
    };
    unregister(task_id);
    r
}

/// Force a rebuild of a project's image, then recreate its containers on next
/// use (their image id no longer matches the tag).
pub async fn rebuild(
    project: &str,
    no_cache: bool,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<String, String> {
    available().await?;
    let hash = context_hash(project)?;
    let tag = image_tag(project, &hash);
    let bl = build_lock(project);
    let _b = bl.lock().await;
    build(project, &tag, no_cache, progress).await?;
    // Running containers on the old image are now stale: mark them so the
    // next ensure_running takes the slow path and recreates them.
    let key = super::trust::norm_path(project);
    for (_, l) in registry().live.lock().unwrap().iter_mut() {
        if l.env
            .as_ref()
            .is_some_and(|e| super::trust::norm_path(&e.project) == key)
        {
            l.state = State::Unknown;
        }
    }
    Ok(tag)
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerInfo {
    pub name: String,
    pub task_id: String,
    pub project: String,
    pub state: String,
    pub status: String,
    pub image: String,
    pub created: String,
}

/// Every container we created, including ones whose chat is gone.
pub async fn list_containers() -> Result<Vec<ContainerInfo>, String> {
    let out = docker(&[
        "ps",
        "-a",
        "--filter",
        &format!("label={LABEL_TASK}"),
        "--format",
        "{{json .}}",
    ])
    .await?;
    Ok(out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| {
            let labels = v["Labels"].as_str().unwrap_or("");
            let label = |k: &str| {
                labels
                    .split(',')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                    .unwrap_or("")
                    .to_string()
            };
            ContainerInfo {
                name: v["Names"].as_str().unwrap_or("").to_string(),
                task_id: label(LABEL_TASK),
                project: label(LABEL_PROJECT),
                state: v["State"].as_str().unwrap_or("").to_string(),
                status: v["Status"].as_str().unwrap_or("").to_string(),
                image: v["Image"].as_str().unwrap_or("").to_string(),
                created: v["CreatedAt"].as_str().unwrap_or("").to_string(),
            }
        })
        .collect())
}

// ───────────────────────────── commands ─────────────────────────────

/// Kill a command's whole process tree inside the container. Killing the
/// local `docker exec` client does not do it: the process keeps running in
/// the container, which is how a "stopped" dev server keeps its port.
pub async fn kill_job(env: &DockerEnv, job: &str) {
    let pidfile = format!("/tmp/.openleash/{job}.pid");
    let script = format!(
        "p=$(cat {pidfile} 2>/dev/null) || exit 0; \
         k() {{ for c in $(cat /proc/$1/task/*/children 2>/dev/null); do k $c; done; kill -KILL $1 2>/dev/null; }}; \
         kill -KILL -- -$p 2>/dev/null; k $p; rm -f {pidfile}"
    );
    let _ = docker(&["exec", &env.container, "/bin/sh", "-c", &script]).await;
}

/// `docker exec` arguments for one shell command. The wrapper records the
/// process id so `kill_job` can find the tree later.
pub fn exec_args(
    env: &DockerEnv,
    job: &str,
    cmd: &str,
    cwd: &str,
    extra_env: &[(&str, String)],
    cfg: &Config,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "exec".into(),
        "-i".into(),
        "-w".into(),
        guest_path(env, cwd),
        "-e".into(),
        "GIT_TERMINAL_PROMPT=0".into(),
        "-e".into(),
        "NO_COLOR=1".into(),
        "-e".into(),
        "CI=1".into(),
        "-e".into(),
        format!("OL_JOB={job}"),
        "-e".into(),
        format!("OL_CMD={cmd}"),
    ];
    for (k, v) in cfg
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .chain(extra_env.iter().cloned())
    {
        if !k.is_empty() && !k.contains('=') {
            a.push("-e".into());
            a.push(format!("{k}={v}"));
        }
    }
    if let Some(u) = cfg.exec_user(&env.host_root) {
        // A uid the image has no passwd entry for still works, but has no
        // writable home; give it one so tools that cache there don't fail.
        a.extend([
            "--user".into(),
            u,
            "-e".into(),
            "HOME=/tmp/.openleash/home".into(),
        ]);
    }
    a.push(env.container.clone());
    a.push("/bin/sh".into());
    a.push("-c".into());
    // The command travels in an environment variable, never spliced into this
    // script, so no quoting of the model's command is needed here.
    a.push(
        "mkdir -p /tmp/.openleash/home 2>/dev/null; echo $$ > /tmp/.openleash/$OL_JOB.pid; \
         if command -v bash >/dev/null 2>&1; then exec bash -lc \"$OL_CMD\"; else exec sh -lc \"$OL_CMD\"; fi"
            .into(),
    );
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> DockerEnv {
        DockerEnv {
            container: "openleash-abc".into(),
            project: "D:\\code\\app".into(),
            host_root: "C:\\Users\\me\\.openleash\\workspaces\\abc".into(),
            guest_root: "/workspace".into(),
        }
    }

    #[test]
    fn guest_paths_map_inside_the_workspace_only() {
        let e = env();
        assert_eq!(
            guest_path(&e, "C:\\Users\\me\\.openleash\\workspaces\\abc"),
            "/workspace"
        );
        assert_eq!(
            guest_path(&e, "C:\\Users\\me\\.openleash\\workspaces\\abc\\src\\lib"),
            "/workspace/src/lib"
        );
        assert_eq!(
            guest_path(&e, "c:/users/me/.openleash/workspaces/abc/src"),
            "/workspace/src"
        );
        // A sibling directory that merely shares the prefix is not inside it.
        assert_eq!(
            guest_path(&e, "C:\\Users\\me\\.openleash\\workspaces\\abcdef\\x"),
            "/workspace"
        );
        assert_eq!(guest_path(&e, "D:\\elsewhere"), "/workspace");
    }

    #[test]
    fn host_paths_map_back_and_refuse_traversal() {
        let e = env();
        assert_eq!(
            host_path(&e, "/workspace/src/main.rs"),
            Some(
                PathBuf::from("C:\\Users\\me\\.openleash\\workspaces\\abc")
                    .join("src")
                    .join("main.rs")
            )
        );
        assert_eq!(
            host_path(&e, "/workspace"),
            Some(PathBuf::from(&e.host_root))
        );
        assert_eq!(host_path(&e, "/workspace/../etc/passwd"), None);
        assert_eq!(host_path(&e, "/workspaces/x"), None);
        assert_eq!(host_path(&e, "/etc/passwd"), None);
    }

    #[test]
    fn config_rejects_unknown_fields_like_raw_docker_flags() {
        assert!(serde_json::from_str::<Config>(r#"{"run_args": ["--privileged"]}"#).is_err());
        let c: Config =
            serde_json::from_str(r#"{"env": {"A": "1"}, "network": "none", "ports": [3000]}"#)
                .unwrap();
        assert_eq!(c.network(), "none");
        assert_eq!(Config::default().network(), "bridge");
        let c: Config = serde_json::from_str(r#"{"network": "host"}"#).unwrap();
        assert_eq!(c.network(), "bridge", "host networking is not offered");
    }

    #[test]
    fn workdir_must_be_a_plain_absolute_path() {
        let w = |s: &str| {
            Config {
                workdir: Some(s.into()),
                ..Default::default()
            }
            .guest_root()
        };
        assert_eq!(w("/src/"), "/src");
        assert_eq!(w("relative"), DEFAULT_GUEST_ROOT);
        assert_eq!(w("/"), DEFAULT_GUEST_ROOT);
        assert_eq!(w("/a/../etc"), DEFAULT_GUEST_ROOT);
    }

    #[test]
    fn run_args_never_widen_the_container() {
        let cfg = Config {
            ports: vec![3000],
            env: [
                ("A".to_string(), "1".to_string()),
                ("B=C".to_string(), "x".to_string()),
            ]
            .into(),
            ..Default::default()
        };
        let a = run_args(&env(), "abc", "openleash/app-1234:deadbeef", &cfg).join(" ");
        assert!(a.contains("--label openleash.task=abc"));
        assert!(
            a.contains("-p 127.0.0.1:3000:3000"),
            "ports bind loopback only: {a}"
        );
        assert!(a.contains("-e A=1"));
        assert!(!a.contains("B=C"), "a key containing = is dropped");
        assert!(a.contains("-v C:\\Users\\me\\.openleash\\workspaces\\abc:/workspace"));
        for bad in [
            "--privileged",
            "--cap-add",
            "/var/run/docker.sock",
            "--network host",
            "--pid",
        ] {
            assert!(!a.contains(bad), "{bad} in {a}");
        }
    }

    /// The container sees the task's staged attachments at `/tmp/OpenLeash`, and
    /// that mount is read-only — a repo's own `.openleash_docker` config must not
    /// turn the host's temp folder into a write target.
    #[test]
    fn run_args_mount_the_staged_attachments_read_only() {
        let e = env();
        let a = run_args(&e, "abc", "openleash/app-1234:deadbeef", &Config::default());
        let root = super::super::attachments::project_dir(&e.project)
            .display()
            .to_string();
        let want = format!("{root}:/tmp/OpenLeash:");
        let mount = a
            .iter()
            .position(|x| x.starts_with(&want))
            .map(|i| a[i].clone())
            .unwrap_or_else(|| panic!("no attachment mount in {a:?}"));
        let mode = mount.trim_start_matches(&want);
        assert!(
            mode.starts_with("ro"),
            "the attachment mount must be read-only, got {mode:?}"
        );
        // The guest path lines up with what `attachments::stage` names.
        let guest = super::super::attachments::project_dir(&e.project);
        assert!(guest.to_string_lossy().contains("OpenLeash"));
    }

    #[test]
    fn exec_passes_the_command_through_the_environment_not_the_script() {
        let cmd = "echo \"$(whoami)\"; rm -rf '/tmp/x' && `id`";
        let a = exec_args(
            &env(),
            "bg_1",
            cmd,
            "C:\\Users\\me\\.openleash\\workspaces\\abc\\web",
            &[],
            &Config::default(),
        );
        assert!(a.contains(&format!("OL_CMD={cmd}")));
        let script = a.last().unwrap();
        assert!(
            !script.contains("whoami"),
            "the command is never spliced into the script"
        );
        let w = a.iter().position(|x| x == "-w").unwrap();
        assert_eq!(a[w + 1], "/workspace/web");
    }

    #[test]
    fn exec_user_defaults() {
        let c = |u: Option<&str>| Config {
            user: u.map(String::from),
            ..Default::default()
        };
        assert_eq!(c(Some("root")).exec_user("/nope"), Some("root".into()));
        assert_eq!(c(Some("node")).exec_user("/nope"), Some("node".into()));
        if cfg!(target_os = "linux") {
            let tmp = std::env::temp_dir();
            assert!(
                c(None)
                    .exec_user(&tmp.to_string_lossy())
                    .unwrap()
                    .contains(':'),
                "Linux runs as the host user by default"
            );
        } else {
            assert_eq!(
                c(None).exec_user("C:/x"),
                None,
                "Docker Desktop maps ownership itself"
            );
        }
    }

    #[test]
    fn image_tags_are_valid_and_change_with_content() {
        let t1 = image_tag("D:\\Code\\My App!", "0123456789abcdef");
        assert!(t1.starts_with("openleash/my-app-"), "{t1}");
        assert!(t1.ends_with(":0123456789ab"));
        assert_ne!(t1, image_tag("D:\\Code\\My App!", "fedcba9876543210"));
        assert_ne!(
            t1,
            image_tag("E:\\Other\\My App!", "0123456789abcdef"),
            "same name, different checkout"
        );
        assert!(t1
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "/:-".contains(c)));
    }

    #[test]
    fn context_hash_tracks_every_file() {
        let tmp = std::env::temp_dir().join(format!("ol-docker-{}", uuid::Uuid::new_v4().simple()));
        let ctx = tmp.join(CONFIG_DIR);
        std::fs::create_dir_all(ctx.join("scripts")).unwrap();
        std::fs::write(ctx.join("Dockerfile"), "FROM alpine\n").unwrap();
        let p = tmp.to_string_lossy().to_string();
        assert!(has_config(&p));
        let h1 = context_hash(&p).unwrap();
        assert_eq!(h1, context_hash(&p).unwrap(), "stable");
        std::fs::write(ctx.join("scripts").join("setup.sh"), "echo hi\n").unwrap();
        let h2 = context_hash(&p).unwrap();
        assert_ne!(h1, h2, "a new file changes the hash");
        std::fs::write(ctx.join("scripts").join("setup.sh"), "echo bye\n").unwrap();
        assert_ne!(h2, context_hash(&p).unwrap(), "an edit changes the hash");
        std::fs::remove_dir_all(&tmp).unwrap();
        assert!(!has_config(&p));
    }

    #[test]
    fn copies_a_plain_project_without_ignored_files_or_symlinks() {
        let tmp = std::env::temp_dir().join(format!("ol-copy-{}", uuid::Uuid::new_v4().simple()));
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("node_modules").join("x")).unwrap();
        std::fs::create_dir_all(src.join("lib")).unwrap();
        std::fs::write(src.join(".ignore"), "node_modules/\n").unwrap();
        std::fs::write(src.join("node_modules").join("x").join("big.js"), "x").unwrap();
        std::fs::write(src.join("lib").join("a.txt"), "a").unwrap();
        std::fs::write(src.join(".env"), "SECRET=1").unwrap();
        let dst = tmp.join("dst");
        let n = copy_project(&src, &dst).unwrap();
        assert!(dst.join("lib").join("a.txt").is_file());
        assert!(
            dst.join(".env").is_file(),
            "dotfiles are part of the project"
        );
        assert!(
            !dst.join("node_modules").exists(),
            "ignored folders are not copied"
        );
        assert_eq!(n, 3);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn env_lookup_by_cwd() {
        let mut e = env();
        e.host_root = format!("C:\\tmp\\ol-test-{}", uuid::Uuid::new_v4().simple());
        let id = format!("t-{}", uuid::Uuid::new_v4().simple());
        register(&id, &e);
        assert_eq!(
            env_for_cwd(&format!("{}\\src", e.host_root)),
            Some(e.clone())
        );
        assert_eq!(env_for_cwd(&format!("{}x", e.host_root)), None);
        unregister(&id);
        assert_eq!(env_for_cwd(&e.host_root), None);
    }

    /// End to end against a real Docker engine. Ignored by default (CI has no
    /// engine); run with `cargo test --locked docker_end_to_end -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn docker_end_to_end() {
        use std::process::Command as Std;
        let root =
            std::env::temp_dir().join(format!("ol-docker-e2e-{}", uuid::Uuid::new_v4().simple()));
        let _home = super::super::store::test_home(&root.join("home"));
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(CONFIG_DIR)).unwrap();
        std::fs::write(
            project.join(CONFIG_DIR).join("Dockerfile"),
            "FROM alpine:3.20\nRUN apk add --no-cache bash\n",
        )
        .unwrap();
        std::fs::write(project.join("README.md"), "hello\n").unwrap();
        let git = |args: &[&str]| {
            let o = Std::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let p = project.to_string_lossy().to_string();
        let task = format!("e2e{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);

        // Workspace: a clone on its own branch, not the project itself.
        let (dir, branch) = create_workspace(&p, &task, "main", "try-it", true).unwrap();
        assert_eq!(branch, "ol/try-it");
        assert!(std::path::Path::new(&dir).join("README.md").is_file());
        let env = DockerEnv {
            container: container_name(&task),
            project: p.clone(),
            host_root: dir.clone(),
            guest_root: DEFAULT_GUEST_ROOT.into(),
        };
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let l2 = lines.clone();
        let progress = move |l: &str| l2.lock().unwrap().push(l.to_string());

        let cleanup = |env: DockerEnv, p: String| async move {
            let _ = docker(&["rm", "-f", &env.container]).await;
            if let Ok(h) = context_hash(&p) {
                let _ = docker(&["rmi", "-f", &image_tag(&p, &h)]).await;
            }
        };
        let result = std::panic::AssertUnwindSafe(async {
            ensure_running_with_rebuild(&task, &env, true, &progress)
                .await
                .unwrap();
            assert_eq!(live(&task).unwrap().state, State::Running);
            assert!(inspect(&env.container).await.unwrap().running);

            // Commands run in the container, in the mapped directory, and
            // their files land in the host workspace.
            let c = tokio_util::sync::CancellationToken::new();
            let sub = std::path::Path::new(&dir).join("sub");
            std::fs::create_dir_all(&sub).unwrap();
            let r = super::super::shell::run_docker(
                &env,
                "pwd; echo made > made.txt; cat /etc/alpine-release >/dev/null && echo alpine",
                &sub.to_string_lossy(),
                60_000,
                &c,
            )
            .await
            .unwrap();
            assert_eq!(r.code, Some(0), "{}", r.output);
            assert!(r.output.contains("/workspace/sub"), "{}", r.output);
            assert!(r.output.contains("alpine"), "{}", r.output);
            assert_eq!(
                std::fs::read_to_string(sub.join("made.txt"))
                    .unwrap()
                    .trim(),
                "made"
            );
            // A container path handed back to the file tools maps to the host file.
            assert_eq!(
                super::super::tools::resolve(&dir, "/workspace/sub/made.txt"),
                sub.join("made.txt")
            );

            // A timed-out command is killed inside the container, not just locally.
            let r = super::super::shell::run_docker(
                &env,
                "sleep 300 & sleep 301; echo never",
                &dir,
                1_500,
                &c,
            )
            .await
            .unwrap();
            assert!(r.timed_out);
            tokio::time::sleep(Duration::from_millis(500)).await;
            let ps = docker(&["exec", &env.container, "ps", "-o", "args"])
                .await
                .unwrap();
            assert!(
                !ps.contains("sleep 30"),
                "process tree survived the timeout:\n{ps}"
            );

            // Background jobs too.
            let bg = super::super::shell::BgManager::default();
            let id = bg.spawn_docker(&env, &task, "sleep 302", &dir).unwrap();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert!(docker(&["exec", &env.container, "ps", "-o", "args"])
                .await
                .unwrap()
                .contains("sleep 302"));
            bg.kill(&id);
            bg.get(&id).unwrap().done.cancelled().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(
                !docker(&["exec", &env.container, "ps", "-o", "args"])
                    .await
                    .unwrap()
                    .contains("sleep 302"),
                "background job survived kill"
            );

            // Idle stop, then resume on demand.
            stop(&task, &env).await.unwrap();
            assert!(!inspect(&env.container).await.unwrap().running);
            ensure_running_with_rebuild(&task, &env, true, &progress)
                .await
                .unwrap();
            assert!(inspect(&env.container).await.unwrap().running);
            assert!(
                sub.join("made.txt").is_file(),
                "the workspace survives a stop"
            );

            // Changing .openleash_docker rebuilds and recreates.
            let old_image = inspect(&env.container).await.unwrap().image_id;
            let old_tag = image_tag(&p, &context_hash(&p).unwrap());
            std::fs::write(
                project.join(CONFIG_DIR).join("Dockerfile"),
                "FROM alpine:3.20\nRUN apk add --no-cache bash\nENV OL_E2E=2\n",
            )
            .unwrap();
            lines.lock().unwrap().clear();
            ensure_running_with_rebuild(&task, &env, true, &progress)
                .await
                .unwrap();
            assert_ne!(
                inspect(&env.container).await.unwrap().image_id,
                old_image,
                "the container was not recreated on the new image"
            );
            let r = super::super::shell::run_docker(&env, "echo $OL_E2E", &dir, 30_000, &c)
                .await
                .unwrap();
            assert_eq!(r.output.trim(), "2");
            assert!(
                sub.join("made.txt").is_file(),
                "the workspace survives a rebuild"
            );
            let _ = docker(&["rmi", "-f", &old_tag]).await;

            // The branch can be pushed back into the project.
            let o = Std::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(o.status.success());
            let o = Std::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-qm",
                    "work",
                ])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            super::super::git::push_workspace_branch(&dir, &p, &branch).unwrap();
            let o = Std::new("git")
                .args(["rev-parse", "--verify", "ol/try-it"])
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(o.status.success(), "branch not in the project");
        })
        .catch_unwind_compat()
        .await;
        cleanup(env.clone(), p.clone()).await;
        unregister(&task);
        let _ = std::fs::remove_dir_all(&root);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// `FutureExt::catch_unwind` without pulling in the trait just for a test.
    trait CatchUnwindCompat: std::future::Future + Sized {
        fn catch_unwind_compat(self) -> futures_util::future::CatchUnwind<Self>
        where
            Self: std::panic::UnwindSafe,
        {
            futures_util::FutureExt::catch_unwind(self)
        }
    }
    impl<F: std::future::Future> CatchUnwindCompat for F {}
}
