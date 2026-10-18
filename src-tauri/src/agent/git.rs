//! Git integration: task worktrees, review diffs, per-file revert, commit.

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

fn output(cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    // A remote-SSH project runs git on its host. Never let the URI reach
    // `current_dir`: on Windows `ssh://h/x` is a relative local path.
    if super::host::is_ssh(cwd) {
        return remote_output(cwd, args);
    }
    // A repository inside WSL is driven by the distro's own git: Windows git
    // on a `\\wsl.localhost` path is slow, trips `safe.directory` (the files
    // belong to a Linux uid), and disagrees with the distro about line endings
    // and executable bits.
    #[cfg(windows)]
    if let Some((distro, linux)) = super::shell::wsl_target(cwd) {
        let mut c = Command::new("wsl.exe");
        c.args(["-d", &distro, "--cd", &linux, "--exec", "git"]);
        for a in args {
            c.arg(super::shell::wsl_arg(&distro, a));
        }
        c.env("GIT_TERMINAL_PROMPT", "0")
            .env("WSLENV", "GIT_TERMINAL_PROMPT/u");
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        return c.output();
    }
    let mut c = Command::new("git");
    c.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.output()
}

/// The remote command line for `git <args>`: every argument one quoted word.
pub fn remote_git_command(args: &[&str]) -> String {
    let mut c = "git".to_string();
    for a in args {
        c.push(' ');
        c.push_str(&super::host::sh_quote(a));
    }
    c
}

/// `output` for an `ssh://` cwd. stdout is taken after a marker line
/// (`ssh_session::exec_machine`), so a login profile that prints something
/// can't be parsed as porcelain.
fn remote_output(cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    let p = super::host::parse_ssh(cwd)
        .ok_or_else(|| std::io::Error::other(format!("{cwd} is not a valid SSH project path")))?;
    let cmd = remote_git_command(args);
    let out = super::ssh_session::block_on(super::ssh_session::exec_machine(
        &p.host,
        &p.path,
        &cmd,
        std::time::Duration::from_secs(120),
        &tokio_util::sync::CancellationToken::new(),
    ))
    .map_err(std::io::Error::other)?;
    if out.timed_out {
        return Err(std::io::Error::other("git on the SSH host timed out"));
    }
    let code = out.code.unwrap_or(255);
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw((code & 0xff) << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    };
    Ok(std::process::Output {
        status,
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

pub(crate) fn git(cwd: &str, args: &[&str]) -> Result<String, String> {
    let out = output(cwd, args).map_err(|e| format!("git not available: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Git metadata needed when a task is first created. One `rev-parse` process
/// gets repository and branch state; the commit lookup is only needed in repos
/// with a valid HEAD, replacing three serial launches on the hot path.
#[derive(Debug, PartialEq, Eq)]
pub struct RepoMetadata {
    pub branch: String,
    pub head: Option<String>,
}

pub fn metadata(cwd: &str) -> Option<RepoMetadata> {
    // One process for all three answers. Plain `HEAD` comes before
    // `--abbrev-ref`, which applies to every revision after it. An unborn repo
    // prints `true` and then fails on `HEAD`; preserve that as a repo with no
    // branch or base commit, as the old helpers did.
    let out = output(
        cwd,
        &[
            "rev-parse",
            "--is-inside-work-tree",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ],
    )
    .ok();
    let meta = out.and_then(|out| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut lines = stdout.lines().map(str::trim);
        if lines.next()? != "true" {
            return None;
        }
        if !out.status.success() {
            return Some(RepoMetadata {
                branch: String::new(),
                head: None,
            });
        }
        let head = lines.next().map(str::to_string);
        let branch = lines.next().unwrap_or_default().to_string();
        Some(RepoMetadata { branch, head })
    });
    // Fresh from the process just run, so it seeds the prompt builder's cache.
    remember_repo(cwd, meta.is_some());
    meta
}

pub fn is_repo(cwd: &str) -> bool {
    git(cwd, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// How long [`is_repo_cached`] trusts an answer. The system prompt is built
/// once per agent, from many agents in a chat, and each build asked git again;
/// a folder becoming (or ceasing to be) a repository within a minute of a chat
/// starting is rare, and the cost of being wrong is only the prompt's "is a
/// git repository" line.
const REPO_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

fn repo_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (bool, std::time::Instant)>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, (bool, std::time::Instant)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn remember_repo(cwd: &str, is_repo: bool) {
    repo_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cwd.to_string(), (is_repo, std::time::Instant::now()));
}

fn cached_repo(cwd: &str) -> Option<bool> {
    repo_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(cwd)
        .filter(|(_, at)| at.elapsed() < REPO_CACHE_TTL)
        .map(|(r, _)| *r)
}

/// [`is_repo`], answered from a short-lived per-folder cache when it can be.
/// Blocking on a miss: async callers use [`is_repo_async`].
pub fn is_repo_cached(cwd: &str) -> bool {
    if let Some(r) = cached_repo(cwd) {
        return r;
    }
    let r = is_repo(cwd);
    remember_repo(cwd, r);
    r
}

/// [`is_repo_cached`] for async code: a miss runs git on the blocking pool,
/// never on a runtime worker.
pub async fn is_repo_async(cwd: &str) -> bool {
    if let Some(r) = cached_repo(cwd) {
        return r;
    }
    let cwd = cwd.to_string();
    tokio::task::spawn_blocking(move || is_repo_cached(&cwd))
        .await
        .unwrap_or(false)
}

pub fn current_branch(cwd: &str) -> String {
    git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn remote_url(cwd: &str) -> Option<String> {
    git(cwd, &["remote", "get-url", "origin"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn head(cwd: &str) -> Option<String> {
    git(cwd, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
}

/// Whether this repository has `sha` as a commit object. `sha` may come from
/// another machine (a transferred chat), so anything that isn't a plain hex
/// object name is refused before it reaches git's argv.
pub fn has_commit(cwd: &str, sha: &str) -> bool {
    let hex = (7..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit());
    hex && git(cwd, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
}

/// What `git status` and the upstream say about a checkout: uncommitted
/// entries, and commits not on the upstream (`None` = no upstream at all).
pub fn sync_state(cwd: &str) -> (usize, Option<usize>) {
    let dirty = git(cwd, &["status", "--porcelain"])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let ahead = git(cwd, &["rev-list", "--count", "@{u}..HEAD"])
        .ok()
        .and_then(|s| s.trim().parse().ok());
    (dirty, ahead)
}

pub fn branches(cwd: &str) -> Vec<String> {
    git(
        cwd,
        &[
            "branch",
            "--format=%(refname:short)",
            "--sort=-committerdate",
        ],
    )
    .map(|s| {
        s.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(30)
            .collect()
    })
    .unwrap_or_default()
}

/// Create `name` at `base` (`HEAD` when empty) without checking it out. The
/// project's checkout and every chat's worktree stay where they are; the branch
/// is there to pick as the base for new chats. It never moves an existing branch.
pub fn create_branch(cwd: &str, name: &str, base: &str) -> Result<(), String> {
    let name = name.trim();
    let base = if base.trim().is_empty() {
        "HEAD"
    } else {
        base.trim()
    };
    // `git branch` reads a leading `-` as an option. `refs/heads/` makes
    // check-ref-format apply the full refname rules without expanding shorthand
    // such as `@{-1}`, which `--branch` would resolve and then accept.
    if name.is_empty()
        || name.starts_with('-')
        || git(cwd, &["check-ref-format", &format!("refs/heads/{name}")]).is_err()
    {
        return Err(format!("'{name}' is not a valid branch name."));
    }
    if base.starts_with('-') {
        return Err(format!("'{base}' is not a branch to start from."));
    }
    git(cwd, &["branch", name, base]).map(|_| ())
}

pub fn slug(text: &str) -> String {
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
        "task".into()
    } else {
        s.trim_end_matches('-').to_string()
    }
}

/// A stable checkout location that distinguishes same-named projects. The
/// human-readable prefix and branch slug keep worktrees easy to inspect; the
/// short hash prevents two repositories named `app` from targeting one folder.
fn worktree_directory(project: &str, branch: &str) -> PathBuf {
    let project_path = std::fs::canonicalize(project).unwrap_or_else(|_| PathBuf::from(project));
    let name = project_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());
    let mut digest = Sha256::new();
    digest.update(project_path.to_string_lossy().as_bytes());
    let identity: String = digest
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    super::store::data_dir().join("worktrees").join(format!(
        "{name}-{identity}-{}",
        branch.trim_start_matches("ol/")
    ))
}

/// Create `ol/<slug>` in a uniquely named sibling worktree under ~/.openleash/worktrees.
pub fn create_worktree(project: &str, base: &str, slug: &str) -> Result<(String, String), String> {
    super::host::refuse(project, "Worktrees are")?;
    let base = if base.is_empty() { "HEAD" } else { base };
    let mut n = 1;
    loop {
        let branch = if n == 1 {
            format!("ol/{slug}")
        } else {
            format!("ol/{slug}-{n}")
        };
        let dir = worktree_directory(project, &branch);
        if git(
            project,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok()
        {
            n += 1;
            continue;
        }
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("could not create worktree folder {}: {e}", parent.display())
            })?;
        }
        let dir_s = dir.to_string_lossy().to_string();
        match git(project, &["worktree", "add", "-b", &branch, &dir_s, base]) {
            Ok(_) => return Ok((dir_s, branch)),
            Err(_error)
                if git(
                    project,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{branch}"),
                    ],
                )
                .is_ok() =>
            {
                // Another task (or OpenLeash instance) won this branch name
                // between the lookup and the atomic ref creation; try a suffix.
                n += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// A docker workspace's private repository: a local clone of `project` at
/// `dir`, on a new `ol/<slug>` branch from `base`.
///
/// A clone rather than a worktree because the container has to be able to run
/// git too, and a worktree's `.git` file points at the main repository by an
/// absolute host path (`D:\code\app\.git\worktrees\x`) that does not
/// exist inside the container. A clone is self-contained, so the same history
/// works on both sides of the mount. `push_workspace_branch` brings the branch
/// back into the project.
pub fn clone_workspace(project: &str, dir: &str, base: &str, slug: &str) -> Result<String, String> {
    super::host::refuse(project, "Docker workspaces are")?;
    let parent = Path::new(dir).parent().ok_or("bad workspace path")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    git(
        &parent.to_string_lossy(),
        &["clone", "--quiet", "--no-checkout", project, dir],
    )?;
    let start = if base.is_empty() || base == "HEAD" {
        "origin/HEAD".to_string()
    } else {
        format!("origin/{base}")
    };
    let branch = format!("ol/{slug}");
    // A repository with no commits yet clones to an unborn HEAD: start the
    // branch empty rather than failing the whole task.
    if git(dir, &["checkout", "--quiet", "-b", &branch, &start]).is_err() {
        git(dir, &["checkout", "--quiet", "--orphan", &branch])?;
    }
    Ok(branch)
}

/// Push a docker workspace's branch into the project it was cloned from, so
/// the work can be merged there like any other branch. Never force-pushes: a
/// branch the user already changed in the project is left for them to merge.
pub fn push_workspace_branch(dir: &str, project: &str, branch: &str) -> Result<String, String> {
    if branch.is_empty() {
        return Err("this workspace has no branch".into());
    }
    git(
        dir,
        &[
            "push",
            "--quiet",
            project,
            &format!("HEAD:refs/heads/{branch}"),
        ],
    )?;
    Ok(branch.to_string())
}

/// Remove a worktree only after the caller has obtained explicit destructive
/// intent: `--force` discards uncommitted and untracked files in that checkout.
pub fn remove_worktree(project: &str, dir: &str) -> Result<(), String> {
    super::host::refuse(project, "Worktrees are")?;
    git(project, &["worktree", "remove", "--force", dir]).map(|_| ())
}

/// Remove one checkout, finding the repository that owns it from the checkout
/// itself rather than being told.
///
/// `remove_worktree` has to be handed the main repository, and for a chat's own
/// worktree the caller always has it. A *worker's* checkout is not so simple: a
/// docker workspace is its own clone, so an ultrathread worker in one is a
/// worktree of that clone, not of the original project the chat was opened on.
/// Asking the wrong repository makes `git worktree remove` fail with "is not a
/// working tree", and for a delete that is fatal — the chat could not be deleted
/// at all. A worktree's `.git` file names its owning repository, so it is read
/// from the checkout here. A path that is not a registered worktree (or whose
/// `.git` is gone) is deleted outright, which is what the caller wanted anyway.
pub fn remove_worktree_at(dir: &str) -> Result<(), String> {
    if let Some(main) = owning_repo(dir) {
        if remove_worktree(&main, dir).is_ok() {
            return Ok(());
        }
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("couldn't remove {dir}: {e}")),
    }
}

/// The main repository a linked worktree belongs to, read from the checkout's
/// own `.git` file: `gitdir: <repo>/.git/worktrees/<name>` — three levels above
/// the file it names.
fn owning_repo(dir: &str) -> Option<String> {
    let text = std::fs::read_to_string(Path::new(dir).join(".git")).ok()?;
    let gitdir = text.strip_prefix("gitdir:")?.trim();
    Path::new(gitdir)
        .ancestors()
        .nth(3)
        .map(|p| p.to_string_lossy().to_string())
}

// ─────────────────────── worktree setup (post_setup_worktree) ───────────────────────
//
// A new worktree is a fresh checkout of the *committed* tree, so everything a
// working copy accumulates and git deliberately does not carry — `.env`,
// `.env.local`, `node_modules`, untracked build output — is missing from it. The
// agent's first command then fails for a reason that has nothing to do with its
// task ("Cannot find module", "DATABASE_URL is not set"), which is the bug this
// block exists to fix.
//
// Two mechanisms, deliberately both:
//   * `.worktreeinclude` — Roo's mechanism, and the default answer. A repository
//     lists the paths it needs; only the intersection with `.gitignore` is
//     copied, so a file the repo does not ignore (a real source file) is never
//     touched, and the file list cannot execute anything.
//   * the `post_setup_worktree` hook (in `checks::setup_worktree`) — for what a
//     file list cannot express: a symlink into the original `node_modules`, an
//     install, a secret from a keychain. `OL_ROOT` points at the original
//     checkout, which is where the worktree came from.
//
// This block is additive; nothing else in this file changed.

/// Paths a repository wants carried into a new worktree, read from
/// `.worktreeinclude` at its root.
///
/// Same syntax as `.gitignore` (blank lines and `#` comments skipped, leading
/// `!` honoured), because it is read by the same matcher and a repository author
/// already knows what that means.
fn worktree_include_lines(project: &Path) -> Vec<String> {
    std::fs::read_to_string(project.join(".worktreeinclude"))
        .map(|t| {
            t.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Copy the intersection of `.worktreeinclude` and `.gitignore` from `project`
/// into an existing worktree at `worktree`. Returns how many files were copied.
///
/// The intersection is what makes this safe to run unattended on a repository
/// nobody has reviewed: `.worktreeinclude` alone would let a hostile repo name
/// `src/main.rs` and have the working copy overwritten, and `.gitignore` alone
/// would copy every build artifact. Requiring both means a repository can only
/// ask for files that are *already* outside version control — which is exactly
/// the set a worktree is missing.
///
/// Directories are copied shallow-first and only when the directory itself is
/// ignored, so `node_modules` arrives whole in one pass rather than file by
/// file. A path that is neither ignored nor a directory is skipped, silently:
/// a wrong entry in a repo's include file must not fail the worktree.
pub fn populate_worktree(project: &str, worktree: &str) -> Result<usize, String> {
    use ignore::gitignore::GitignoreBuilder;
    super::host::refuse(project, "Worktrees are")?;

    let root = Path::new(project);
    let dst = Path::new(worktree);
    let lines = worktree_include_lines(root);
    if lines.is_empty() {
        return Ok(0);
    }
    for (label, path) in [("project", root), ("worktree", dst)] {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|e| format!("could not inspect {label} {}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "{label} {} must be a real directory",
                path.display()
            ));
        }
    }
    // The repo's root `.gitignore`. Only that one file: a nested `.gitignore`
    // applies to its own subtree, and a `.worktreeinclude` entry is written from
    // the repo root, so resolving it against one matcher is the rule a reader
    // would expect. The cost of that choice is that an entry ignored only by a
    // *nested* `.gitignore` is skipped — a no-op, not a copy of the wrong file.
    let mut ignore = GitignoreBuilder::new(root);
    if let Some(e) = ignore.add(root.join(".gitignore")) {
        // A malformed `.gitignore` is not fatal: keep the rules that parsed.
        eprintln!("[openleash] .gitignore: {e}");
    }
    let ignore = ignore.build().map_err(|e| e.to_string())?;

    let mut copied = 0usize;
    for line in lines {
        let rel = line.trim_start_matches('/');
        // A glob entry (`*.log`, `dist/**`) cannot be resolved to one path, and
        // expanding it would copy whatever a hostile repo's pattern happened to
        // match. Only literal paths are honoured; document that.
        if rel.is_empty()
            || rel.contains('*')
            || rel.contains('?')
            || rel.contains('[')
            || rel.contains("..")
        {
            continue;
        }
        let Some(rel_path) = safe_include_path(rel) else {
            continue;
        };
        let src = root.join(&rel_path);
        let metadata = match std::fs::symlink_metadata(&src) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("could not inspect {}: {e}", src.display())),
        };
        ensure_no_symlink_components(root, &rel_path)?;
        ensure_no_symlink_components(dst, &rel_path)?;
        if metadata.file_type().is_symlink() {
            return Err(format!("refusing to copy symlink {}", src.display()));
        }
        let is_dir = metadata.is_dir();
        // The intersection: git must already ignore it, or it is a tracked file
        // whose working copy has no business being replaced.
        let ignored = ignore.matched_path_or_any_parents(rel, is_dir).is_ignore();
        if !ignored {
            continue;
        }
        copied += copy_entry(&src, &dst.join(&rel_path))?;
    }
    Ok(copied)
}

/// Resolve one `.worktreeinclude` entry without allowing it to escape either
/// checkout. Non-normal components (absolute paths, `.` and `..`) are ignored.
fn safe_include_path(rel: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;

    let mut safe = std::path::PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => safe.push(part),
            _ => return None,
        }
    }
    (!safe.as_os_str().is_empty()).then_some(safe)
}

/// Reject symlink components before a copy can traverse them. The source tree
/// and the checkout are both repository-controlled; following a link on either
/// side would let setup read or write outside the selected worktree.
fn ensure_no_symlink_components(root: &Path, rel: &Path) -> Result<(), String> {
    let mut path = root.to_path_buf();
    for component in rel.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("refusing to traverse symlink {}", path.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("could not inspect {}: {e}", path.display())),
        }
    }
    Ok(())
}

/// Copy one file or directory, returning how many files landed.
///
/// A directory is walked, not copied wholesale, so a `.worktreeinclude` naming
/// a parent directory cannot smuggle a tracked file in with it: what is copied
/// is still only what is missing from the worktree. Symlinks and filesystem
/// errors fail visibly instead of being followed or silently dropping files.
fn copy_entry(src: &Path, dst: &Path) -> Result<usize, String> {
    let metadata = std::fs::symlink_metadata(src)
        .map_err(|e| format!("could not inspect {}: {e}", src.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing to copy symlink {}", src.display()));
    }
    let dst_metadata = match std::fs::symlink_metadata(dst) {
        Ok(metadata) => Some(metadata),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not inspect {}: {e}", dst.display())),
    };
    if dst_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "refusing to write through symlink {}",
            dst.display()
        ));
    }

    if metadata.is_dir() {
        // Created even when it turns out to hold nothing, so a repo that names
        // an empty needed directory (a log or cache dir) gets it.
        if dst_metadata
            .as_ref()
            .is_some_and(|metadata| !metadata.is_dir())
        {
            return Ok(0);
        }
        std::fs::create_dir_all(dst)
            .map_err(|e| format!("could not create {}: {e}", dst.display()))?;
        let mut n = 0;
        let rd =
            std::fs::read_dir(src).map_err(|e| format!("could not read {}: {e}", src.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("could not read {}: {e}", src.display()))?;
            n += copy_entry(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(n);
    }
    // Never clobber something the checkout already has: the worktree's own
    // committed copy is the one the agent should be working on.
    if dst_metadata.is_some() {
        return Ok(0);
    }
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("could not create {}: {e}", p.display()))?;
    }
    std::fs::copy(src, dst)
        .map_err(|e| format!("could not copy {} to {}: {e}", src.display(), dst.display()))?;
    Ok(1)
}

#[cfg(test)]
mod worktree_tests {
    use super::*;
    use std::path::PathBuf;

    fn output(cwd: &Path, args: &[&str]) -> std::process::Output {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .expect("git should be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    #[test]
    fn worktree_paths_are_unique_for_same_named_projects() {
        let home = std::env::temp_dir().join(format!("ol-git-paths-{}", std::process::id()));
        let _home = super::super::store::test_home(&home);
        let first = PathBuf::from("/one/app");
        let second = PathBuf::from("/two/app");
        assert_ne!(
            worktree_directory(first.to_str().unwrap(), "ol/task"),
            worktree_directory(second.to_str().unwrap(), "ol/task")
        );
    }

    #[test]
    fn worktree_creation_retries_branch_collisions_and_cleanup_reports_errors() {
        let temp = std::env::temp_dir().join(format!("ol-git-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        output(&temp, &["init", "-q"]);
        output(&temp, &["config", "user.name", "Test"]);
        output(&temp, &["config", "user.email", "t@example.invalid"]);
        std::fs::write(temp.join("base.txt"), "base\n").unwrap();
        output(&temp, &["add", "-A"]);
        output(&temp, &["commit", "-q", "-m", "init"]);
        output(&temp, &["branch", "ol/collision"]);

        let project = temp.to_string_lossy().to_string();
        let home = temp.join("openleash-home");
        let _home = super::super::store::test_home(&home);
        let worktree_result = create_worktree(&project, "HEAD", "collision");
        let (worktree, branch) = worktree_result.unwrap();
        assert_eq!(branch, "ol/collision-2");
        std::fs::write(
            Path::new(&worktree).join("uncommitted.txt"),
            "preserve me\n",
        )
        .unwrap();
        assert!(
            remove_worktree(&project, "not-a-worktree").is_err(),
            "Git cleanup failures must be returned, not silently ignored"
        );
        assert!(Path::new(&worktree).join("uncommitted.txt").is_file());
        remove_worktree(&project, &worktree).unwrap();
        assert!(
            git(
                &project,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/heads/ol/collision-2"
                ]
            )
            .is_ok(),
            "removing the checkout preserves its branch for recovery"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn populated_worktrees_reject_symlinks_on_both_sides() {
        let temp = std::env::temp_dir().join(format!("ol-git-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let root = temp.join("root");
        let worktree = temp.join("worktree");
        let outside = temp.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join(".gitignore"), "cache/\n").unwrap();
        std::fs::write(root.join(".worktreeinclude"), "cache\n").unwrap();
        std::fs::write(outside.join("sentinel"), "untouched\n").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("cache")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, root.join("cache")).unwrap();
        let error = populate_worktree(root.to_str().unwrap(), worktree.to_str().unwrap())
            .expect_err("a source symlink must not be followed");
        assert!(error.contains("symlink"), "{error}");
        assert_eq!(
            std::fs::read_to_string(outside.join("sentinel")).unwrap(),
            "untouched\n"
        );

        #[cfg(unix)]
        std::fs::remove_file(root.join("cache")).unwrap();
        #[cfg(windows)]
        std::fs::remove_dir(root.join("cache")).unwrap();
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("cache/data"), "copy me").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, worktree.join("cache")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, worktree.join("cache")).unwrap();
        let error = populate_worktree(root.to_str().unwrap(), worktree.to_str().unwrap())
            .expect_err("a destination symlink must not be written through");
        assert!(error.contains("symlink"), "{error}");
        assert_eq!(
            std::fs::read_to_string(outside.join("sentinel")).unwrap(),
            "untouched\n"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn a_worktree_is_removed_through_the_repo_that_owns_it() {
        let temp = std::env::temp_dir().join(format!("ol-git-owning-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let home = temp.join("openleash-home");
        let _home = super::super::store::test_home(&home);
        let repo = temp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        output(&repo, &["init", "-q"]);
        output(&repo, &["config", "user.name", "Test"]);
        output(&repo, &["config", "user.email", "t@example.invalid"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        output(&repo, &["add", "-A"]);
        output(&repo, &["commit", "-q", "-m", "init"]);
        let repo_s = repo.to_string_lossy().to_string();

        let (wt, branch) = create_worktree(&repo_s, "HEAD", "owner").unwrap();
        remove_worktree_at(&wt).unwrap();
        assert!(!Path::new(&wt).is_dir(), "the checkout is gone");
        assert!(
            git(
                &repo_s,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}")
                ]
            )
            .is_ok(),
            "the branch survives"
        );
        // A folder git has never heard of is deleted outright rather than refused:
        // the caller asked for the checkout to go, and a chat has to stay
        // deletable when its worktree's registry entry was already cleaned by hand.
        let stray = temp.join("stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("junk.txt"), "junk\n").unwrap();
        remove_worktree_at(&stray.to_string_lossy()).unwrap();
        assert!(
            !stray.is_dir(),
            "a folder that is not a worktree is still removed"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_clean_worktree_is_distinguished_from_a_failed_commit() {
        let temp = std::env::temp_dir().join(format!("ol-git-cleanru-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        output(&temp, &["init", "-q"]);
        output(&temp, &["config", "user.name", "Test"]);
        output(&temp, &["config", "user.email", "t@example.invalid"]);
        std::fs::write(temp.join("base.txt"), "base\n").unwrap();
        output(&temp, &["add", "-A"]);
        output(&temp, &["commit", "-q", "-m", "init"]);
        let dir = temp.to_string_lossy().to_string();

        // Nothing changed: the one unambiguous "clean" answer.
        assert_eq!(commit_worktree(&dir, "ultrathread: nothing").unwrap(), None);

        std::fs::write(temp.join("work.txt"), "real work\n").unwrap();
        let sha = commit_worktree(&dir, "ultrathread: real work")
            .unwrap()
            .expect("a dirty worktree commits");
        assert_eq!(sha.len(), 7, "the short sha is reported: {sha}");

        // A refused commit must be an error, never `Ok(None)` — the bug this
        // helper exists to fix is a failing hook reading as "no work produced".
        let hooks = temp.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(temp.join("more.txt"), "more work\n").unwrap();
        let error =
            commit_worktree(&dir, "ultrathread: refused").expect_err("a failing hook is an error");
        assert!(!error.trim().is_empty(), "the failure names a reason");
        let _ = std::fs::remove_dir_all(&temp);
    }
}

#[derive(Serialize)]
pub struct FileDiff {
    pub path: String,
    pub status: String,
    pub add: usize,
    pub del: usize,
    pub lines: Vec<Value>,
}

/// Everything that changed since `base` (committed + uncommitted + untracked).
pub fn review(cwd: &str, base: &str) -> Result<Vec<FileDiff>, String> {
    let raw = git(
        cwd,
        &["diff", "--no-color", "--no-ext-diff", "-U3", base, "--"],
    )?;
    let mut files: Vec<FileDiff> = vec![];
    for chunk in raw
        .split("\ndiff --git ")
        .map(|c| c.trim_start_matches("diff --git "))
    {
        if chunk.trim().is_empty() {
            continue;
        }
        let mut path = String::new();
        let mut status = "M".to_string();
        let mut lines = vec![];
        let (mut add, mut del) = (0, 0);
        let mut in_hunk = false;
        for l in chunk.lines() {
            if !in_hunk {
                if let Some(p) = l.strip_prefix("+++ b/") {
                    path = p.to_string();
                } else if l.starts_with("--- a/") && path.is_empty() {
                    path = l[6..].to_string();
                } else if l.starts_with("new file") {
                    status = "A".into();
                } else if l.starts_with("deleted file") {
                    status = "D".into();
                } else if l.starts_with("rename to ") {
                    status = "R".into();
                    path = l.trim_start_matches("rename to ").to_string();
                }
            }
            if l.starts_with("@@") {
                in_hunk = true;
                lines.push(json!({"k":"h","t":l}));
            } else if in_hunk {
                let (k, t) = match l.chars().next() {
                    Some('+') => {
                        add += 1;
                        ("a", &l[1..])
                    }
                    Some('-') => {
                        del += 1;
                        ("d", &l[1..])
                    }
                    Some('\\') => continue,
                    _ => ("c", l.get(1..).unwrap_or("")),
                };
                if lines.len() < 3000 {
                    lines.push(json!({"k":k,"t":t}));
                }
            }
        }
        if path.is_empty() {
            path = chunk
                .lines()
                .next()
                .unwrap_or("")
                .split(" b/")
                .last()
                .unwrap_or("")
                .to_string();
        }
        if lines.is_empty() {
            lines.push(json!({"k":"h","t":"binary or mode-only change"}));
        }
        files.push(FileDiff {
            path,
            status,
            add,
            del,
            lines,
        });
    }
    if let Ok(untracked) = git(cwd, &["ls-files", "--others", "--exclude-standard"]) {
        for p in untracked.lines().filter(|l| !l.is_empty()).take(200) {
            let content = read_worktree_file(cwd, p).unwrap_or_default();
            let mut lines =
                vec![json!({"k":"h","t":format!("@@ -0,0 +1,{} @@", content.lines().count())})];
            let mut add = 0;
            for l in content.lines().take(3000) {
                add += 1;
                lines.push(json!({"k":"a","t":l}));
            }
            files.push(FileDiff {
                path: p.to_string(),
                status: "A".into(),
                add,
                del: 0,
                lines,
            });
        }
    }
    Ok(files)
}

/// What [`review`] adds up to, for the session header: files changed and
/// lines added/removed, without building (and shipping over IPC) every line.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewStats {
    pub files: usize,
    pub add: usize,
    pub del: usize,
}

/// [`review`]'s totals from `git diff --numstat` against the same `base`.
///
/// Kept to `review`'s exact semantics so the header and the review screen
/// never disagree: a binary file counts as a file with no lines (numstat's
/// `-`), and untracked files are the same first 200 that `review` lists, each
/// counted as at most 3000 added lines of UTF-8 text, as `review` counts them.
pub fn review_stats(cwd: &str, base: &str) -> Result<ReviewStats, String> {
    let raw = git(cwd, &["diff", "--numstat", "--no-ext-diff", base, "--"])?;
    let mut s = ReviewStats::default();
    for line in raw.lines().filter(|l| !l.is_empty()) {
        let mut it = line.splitn(3, '\t');
        s.add += it.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        s.del += it.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        s.files += 1;
    }
    if let Ok(untracked) = git(cwd, &["ls-files", "--others", "--exclude-standard"]) {
        for p in untracked.lines().filter(|l| !l.is_empty()).take(200) {
            let content = read_worktree_file(cwd, p).unwrap_or_default();
            s.add += content.lines().take(3000).count();
            s.files += 1;
        }
    }
    Ok(s)
}

pub fn revert_file(cwd: &str, base: &str, path: &str) -> Result<(), String> {
    let existed = git(cwd, &["cat-file", "-e", &format!("{base}:{path}")]).is_ok();
    if existed {
        git(cwd, &["checkout", base, "--", path]).map(|_| ())
    } else {
        let _ = git(cwd, &["rm", "--cached", "--quiet", "--", path]);
        if let Some(root) = super::host::parse_ssh(cwd) {
            let target = remote_rel(&root, path)?;
            return super::ssh_session::block_on(super::ssh_session::remove_file(
                &target.host,
                &target.path,
            ));
        }
        std::fs::remove_file(Path::new(cwd).join(path)).map_err(|e| e.to_string())
    }
}

/// `rel` (a path git printed, relative to the checkout) under a remote root.
/// Refused if it climbs out: it reaches SFTP as an exact path string.
fn remote_rel(root: &super::host::SshPath, rel: &str) -> Result<super::host::SshPath, String> {
    let t = root.join(rel.trim_start_matches('/'));
    if !super::host::inside(&root.path, &t.path) || t.path == root.path {
        return Err(format!("{rel} is not inside the project."));
    }
    Ok(t)
}

/// A file in the checkout as text (lossy), local or over SFTP.
fn read_worktree_file(cwd: &str, rel: &str) -> Option<String> {
    if let Some(root) = super::host::parse_ssh(cwd) {
        let t = remote_rel(&root, rel).ok()?;
        let (bytes, _) =
            super::ssh_session::block_on(super::ssh_session::read(&t.host, &t.path, 4 << 20))
                .ok()?;
        return String::from_utf8(bytes).ok();
    }
    std::fs::read_to_string(Path::new(cwd).join(rel)).ok()
}

pub fn commit(cwd: &str, message: &str) -> Result<String, String> {
    git(cwd, &["add", "-A"])?;
    git(cwd, &["commit", "-m", message])?;
    Ok(git(cwd, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string())
}

/// Commit everything in a finished ultrathread worktree, and say which of the
/// two outcomes happened: a commit, or a worktree that was already clean.
///
/// This exists because `commit`'s single `Result` cannot tell those apart from a
/// *failure*. The runner used to read `commit(...).ok()` and call every miss
/// "nothing new to commit" — so a repo with a failing pre-commit hook, or one
/// with no `user.name`/`user.email` configured at all (common in a fresh CI or
/// container checkout), silently reported that a worker had produced no work.
/// The worker's branch then looked empty to the fuze agent, which is a lost
/// change that reads as an empty task.
///
/// The tree is checked first, deliberately: `git status --porcelain` naming
/// nothing is the one unambiguous "clean" signal, and on Windows a failed
/// `git commit` returns non-zero with *empty stderr* (see `commit_with`), so the
/// failure path has to be told apart before git is asked to commit.
///
/// `Ok(None)` = clean, nothing to commit. `Err` carries git's own reason.
pub fn commit_worktree(cwd: &str, message: &str) -> Result<Option<String>, String> {
    let dirty = git(cwd, &["status", "--porcelain"])?;
    if dirty.trim().is_empty() {
        return Ok(None);
    }
    git(cwd, &["add", "-A"])?;
    git(cwd, &["commit", "-m", message]).map_err(|e| {
        if e.trim().is_empty() {
            "a pre-commit hook refused it (it printed nothing), or git has no user.name/user.email configured"
                .to_string()
        } else {
            e
        }
    })?;
    Ok(Some(
        git(cwd, &["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string(),
    ))
}

/// Commit with the two knobs the review screen needs: a message that already
/// carries its trailer, and whether repo-supplied hooks may run.
///
/// Deliberately a sibling of `commit` rather than a widening of it. `commit` is
/// on the ultrathread path (runner.rs) and in this file's tests; giving it a
/// new parameter would have changed both call sites for a feature that only the
/// UI uses, and this file is being edited concurrently. The two functions are
/// three lines of intentional overlap.
///
/// `no_verify` maps straight onto `git commit --no-verify`, and the caller
/// decides it from a setting. The security argument for the default lives at
/// the setting (`Settings::git_commit_verify` in store.rs), not here: this
/// function does what it is told, so a test can pin both directions.
pub fn commit_with(cwd: &str, message: &str, no_verify: bool) -> Result<String, String> {
    git(cwd, &["add", "-A"])?;
    let mut args = vec!["commit", "-m", message];
    if no_verify {
        args.push("--no-verify");
    }
    // `git` surfaces a failed command's *stderr* and nothing else, and on
    // Windows a pre-commit hook that exits non-zero writes nothing at all —
    // verified against git 2.52: `git commit` returns 1 with an empty stderr
    // while the hook's own output goes nowhere. So the failure this feature
    // makes possible (see `Settings::git_commit_verify`) would reach the user as
    // an empty toast. An empty message only ever means "git failed without
    // saying why", and the two things that can cause it here are a refused hook
    // and a commit with nothing staged, so name both rather than neither.
    git(cwd, &args).map_err(|e| {
        if e.trim().is_empty() {
            "commit refused — a pre-commit hook failed (it printed nothing), or there is nothing to commit".to_string()
        } else {
            e
        }
    })?;
    Ok(git(cwd, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string())
}

#[cfg(test)]
mod branch_create_tests {
    use super::{branches, create_branch, current_branch, git};

    /// Run `git` in `cwd` with a fixed identity; the test must not depend on the
    /// developer's config (signing, user name).
    fn run(cwd: &str, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repository with one commit on whatever branch git starts on.
    fn repo(tag: &str) -> (super::super::reviewer_sandbox::Sandbox, String) {
        let sb = super::super::reviewer_sandbox::Sandbox::new(tag);
        let cwd = sb.root().to_string_lossy().to_string();
        run(&cwd, &["init", "-q"]);
        sb.write("a.txt", "one\n");
        run(&cwd, &["add", "-A"]);
        run(&cwd, &["commit", "-q", "-m", "base"]);
        (sb, cwd)
    }

    fn rev(cwd: &str, name: &str) -> String {
        git(cwd, &["rev-parse", name]).unwrap().trim().to_string()
    }

    #[test]
    fn a_new_branch_starts_at_the_base_and_the_checkout_stays_put() {
        let (_sb, cwd) = repo("branch-create-ok");
        let base = current_branch(&cwd);

        create_branch(&cwd, "feature/login", &base).unwrap();

        assert_eq!(
            current_branch(&cwd),
            base,
            "creating a branch must not switch to it"
        );
        assert!(branches(&cwd).contains(&"feature/login".to_string()));
        assert_eq!(rev(&cwd, "feature/login"), rev(&cwd, &base));
    }

    #[test]
    fn an_empty_base_starts_from_the_checked_out_commit() {
        let (_sb, cwd) = repo("branch-create-head");
        create_branch(&cwd, "from-head", "  ").unwrap();
        assert_eq!(rev(&cwd, "from-head"), rev(&cwd, "HEAD"));
    }

    #[test]
    fn names_git_would_reject_are_refused_and_nothing_is_created() {
        let (_sb, cwd) = repo("branch-create-names");
        let before = branches(&cwd);
        // Empty, a leading dash (an option to `git branch`), spaces, `..`,
        // `@{` (a revision shorthand), `~`, a `.lock` suffix and a trailing slash.
        for bad in [
            "",
            "-x",
            "--all",
            "a b",
            "a..b",
            "x@{-1}",
            "bad~name",
            "x.lock",
            "trailing/",
        ] {
            assert!(
                create_branch(&cwd, bad, "HEAD").is_err(),
                "{bad:?} must be refused"
            );
        }
        assert_eq!(
            branches(&cwd),
            before,
            "a refused name must not leave a branch behind"
        );
    }

    #[test]
    fn a_base_that_reads_as_an_option_is_refused() {
        let (_sb, cwd) = repo("branch-create-base");
        assert!(create_branch(&cwd, "ok-name", "--force").is_err());
        assert!(!branches(&cwd).contains(&"ok-name".to_string()));
    }

    #[test]
    fn an_existing_branch_is_never_moved() {
        let (sb, cwd) = repo("branch-create-existing");
        let base = current_branch(&cwd);
        create_branch(&cwd, "keep", &base).unwrap();
        let kept = rev(&cwd, "keep");

        // Move the base on, so a silent force would be visible as a changed commit.
        sb.write("b.txt", "two\n");
        run(&cwd, &["add", "-A"]);
        run(&cwd, &["commit", "-q", "-m", "second"]);

        assert!(create_branch(&cwd, "keep", &base).is_err());
        assert_eq!(
            rev(&cwd, "keep"),
            kept,
            "the existing branch must keep its commit"
        );
    }
}

#[cfg(test)]
mod review_stats_tests {
    use super::{git, review, review_stats, ReviewStats};

    /// The session header reads `review_stats`, the review screen reads
    /// `review`; they must add up to the same thing for every kind of change.
    #[test]
    fn review_stats_add_up_to_what_review_shows() {
        let sb = super::super::reviewer_sandbox::Sandbox::new("review-stats");
        let cwd = sb.root().to_string_lossy().to_string();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=T",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&cwd)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        sb.write("keep.txt", "a\nb\nc\n");
        sb.write("gone.txt", "1\n2\n");
        sb.write("old-name.txt", "same\ncontent\nhere\n");
        std::fs::write(sb.root().join("blob.bin"), [0u8, 1, 2, 0, 3]).unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "base"]);
        let base = git(&cwd, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        sb.write("keep.txt", "a\nB\nc\nd\n");
        std::fs::remove_file(sb.root().join("gone.txt")).unwrap();
        std::fs::write(sb.root().join("blob.bin"), [9u8, 0, 9, 0]).unwrap();
        run(&["mv", "old-name.txt", "new-name.txt"]);
        sb.write("staged.txt", "s1\ns2\n");
        run(&["add", "staged.txt"]);
        run(&["commit", "-q", "-m", "more"]);
        sb.write("untracked.txt", "u1\nu2\nu3\n");

        let files = review(&cwd, &base).unwrap();
        let want = ReviewStats {
            files: files.len(),
            add: files.iter().map(|f| f.add).sum(),
            del: files.iter().map(|f| f.del).sum(),
        };
        assert!(want.files >= 5, "the fixture covers each kind: {want:?}");
        assert_eq!(review_stats(&cwd, &base).unwrap(), want);
    }
}
