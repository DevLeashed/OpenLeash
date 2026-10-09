//! Git integration: task worktrees, review diffs, per-file revert, commit.

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

fn output(cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
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

fn git(cwd: &str, args: &[&str]) -> Result<String, String> {
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
    // An unborn repo prints `true` and `HEAD` before rev-parse exits unsuccessfully.
    // Preserve that as a repo with no branch or base commit, as the old helpers did.
    let out = output(
        cwd,
        &["rev-parse", "--is-inside-work-tree", "--abbrev-ref", "HEAD"],
    )
    .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines().map(str::trim);
    if lines.next()? != "true" {
        return None;
    }
    let branch = lines.next().unwrap_or_default();
    if !out.status.success() {
        return Some(RepoMetadata {
            branch: String::new(),
            head: None,
        });
    }
    let head = git(cwd, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string());
    Some(RepoMetadata {
        branch: branch.to_string(),
        head,
    })
}

pub fn is_repo(cwd: &str) -> bool {
    git(cwd, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
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

/// Remove a worktree only after the caller has obtained explicit destructive
/// intent: `--force` discards uncommitted and untracked files in that checkout.
pub fn remove_worktree(project: &str, dir: &str) -> Result<(), String> {
    git(project, &["worktree", "remove", "--force", dir]).map(|_| ())
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
            let content = std::fs::read_to_string(Path::new(cwd).join(p)).unwrap_or_default();
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

pub fn revert_file(cwd: &str, base: &str, path: &str) -> Result<(), String> {
    let existed = git(cwd, &["cat-file", "-e", &format!("{base}:{path}")]).is_ok();
    if existed {
        git(cwd, &["checkout", base, "--", path]).map(|_| ())
    } else {
        let _ = git(cwd, &["rm", "--cached", "--quiet", "--", path]);
        std::fs::remove_file(Path::new(cwd).join(path)).map_err(|e| e.to_string())
    }
}

pub fn commit(cwd: &str, message: &str) -> Result<String, String> {
    git(cwd, &["add", "-A"])?;
    git(cwd, &["commit", "-m", message])?;
    Ok(git(cwd, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string())
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
