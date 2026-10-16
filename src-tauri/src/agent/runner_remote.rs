//! Tool dispatch for a remote-SSH task (its cwd is `ssh://<host>/<path>`).
//!
//! `exec_inner` hands the path-taking tools here after the per-agent limits
//! (read-only and no-shell subagents) have been applied, and before any
//! `Path` is built from the cwd. The permission gate, plan mode and approval
//! flow are the same `Agent::gate` local tools use; what differs is where the
//! bytes live and how containment is proven:
//!
//! - A write is checked and performed on the target's *realpath* on the host
//!   (`ssh_session::realpath`), against the realpath of the project root, so
//!   `proj/link -> /etc` is outside the root however it is spelled.
//! - Read-before-write keeps a content hash beside the mtime: SFTP mtimes are
//!   whole seconds, so a same-second change would otherwise go unseen.

use super::*;
use crate::agent::host::{self, SshPath};
use crate::agent::ssh_session as ss;
use crate::agent::tools_remote as tr;

/// The project root's realpath, as a URI: the `cwd` a remote write's
/// containment is judged against.
pub(super) async fn real_root(cwd: &str) -> Result<String, String> {
    let p =
        host::parse_ssh(cwd).ok_or_else(|| format!("{cwd} is not a valid SSH project path."))?;
    Ok(tr::real(&p)
        .await
        .map_err(|e| format!("Couldn't resolve the project folder on the SSH host: {e}"))?
        .uri())
}

fn base_of(cwd: &str, input: &Value) -> Result<SshPath, String> {
    match input["path"].as_str().filter(|p| !p.trim().is_empty()) {
        Some(p) => tr::resolve(cwd, p),
        None => {
            host::parse_ssh(cwd).ok_or_else(|| format!("{cwd} is not a valid SSH project path."))
        }
    }
}

impl Agent {
    /// `Some(result)` for a tool this module runs; `None` hands the call back
    /// to the shared dispatch (bash, web, todo, …), which has its own remote
    /// seams where it needs them.
    pub(super) async fn exec_remote(
        &self,
        name: &str,
        input: &Value,
        item_id: Option<&str>,
        cwd: &str,
    ) -> Option<Result<String, String>> {
        Some(match name {
            "read_file" => self.remote_read(input, item_id, cwd).await,
            "view_image" => {
                async {
                    let p = tr::resolve(cwd, input["path"].as_str().ok_or("path is required")?)?;
                    self.gate(name, input, Some(&p.uri())).await?;
                    self.exec_view_image(input, item_id)
                        .await
                        .map(|v| tools::tool_text(&v))
                }
                .await
            }
            "glob" => {
                async {
                    let base = base_of(cwd, input)?;
                    let out = tr::glob(
                        &base,
                        input["pattern"].as_str().unwrap_or("*"),
                        &self.cancel,
                    )
                    .await?;
                    let n = if out == "No files found" {
                        0
                    } else {
                        out.lines().count()
                    };
                    self.set_item(item_id, |i| i.data["meta"] = json!(format!("{n} files")))
                        .await;
                    Ok(out)
                }
                .await
            }
            "grep" => {
                async {
                    let base = base_of(cwd, input)?;
                    self.step(format!(
                        "Searching for {}",
                        input["pattern"].as_str().unwrap_or("")
                    ))
                    .await;
                    let (out, n) = tr::grep(&base, input, &self.cancel).await?;
                    self.set_item(item_id, |i| {
                        i.data["meta"] =
                            json!(format!("{n} match{}", if n == 1 { "" } else { "es" }))
                    })
                    .await;
                    Ok(out)
                }
                .await
            }
            "edit_file" | "multi_edit" | "write_file" => {
                self.remote_write(name, input, item_id, cwd).await
            }
            "artifict_create"
            | "artifict_revise"
            | "artifict_respond"
            | "artifict_list"
            | "artifict_get"
            | "artifict_feedback_list" => {
                Err("Artificts are not yet supported for SSH projects.".into())
            }
            _ => return None,
        })
    }

    async fn remote_read(
        &self,
        input: &Value,
        item_id: Option<&str>,
        cwd: &str,
    ) -> Result<String, String> {
        let p = tr::resolve(cwd, input["path"].as_str().ok_or("path is required")?)?;
        let uri = p.uri();
        self.gate("read_file", input, Some(&uri)).await?;
        self.step(format!("Reading {}", permissions::short(&uri, cwd)))
            .await;
        let (out, total, hash, mtime) =
            tr::read_file(&p, input["offset"].as_u64(), input["limit"].as_u64()).await?;
        // Keyed by where the file physically is, like the local canonical key,
        // so a later edit through another spelling (or a symlink) matches.
        let key = tr::real(&p).await.map(|r| r.uri()).unwrap_or(uri);
        {
            let t = self.h.task(&self.task_id).await?;
            let mut t = t.lock().await;
            t.read_files.insert(key.clone(), mtime);
            t.read_hashes.insert(key, hash);
        }
        self.set_item(item_id, |i| {
            i.data["meta"] = json!(format!("{total} lines"))
        })
        .await;
        Ok(out)
    }

    pub(super) async fn remote_view_image(
        &self,
        cwd: &str,
        raw: &str,
        item_id: Option<&str>,
    ) -> Result<Value, String> {
        let p = tr::resolve(cwd, raw)?;
        self.step(format!("Viewing {}", permissions::short(&p.uri(), cwd)))
            .await;
        let img = tr::view_image(&p).await?;
        self.finish_image(item_id, &p.path, &img).await;
        Ok(tools::image_content(&p.path, &img))
    }

    async fn remote_write(
        &self,
        name: &str,
        input: &Value,
        item_id: Option<&str>,
        cwd: &str,
    ) -> Result<String, String> {
        let t = self.h.task(&self.task_id).await?;
        let p = tr::resolve(cwd, input["path"].as_str().ok_or("path is required")?)?;
        // Everything below — the read-before-write key, the permission check
        // and the write itself — uses this one physical path, so what was
        // checked is exactly what is written (a symlink can't redirect it).
        let target = tr::real(&p).await?;
        let key = target.uri();
        // Fail closed: a write the path resolution placed outside the project
        // is refused, not prompted-and-written. The gate's `Ask` is dropped for
        // file tools (see `gate`), so relying on it would let `proj/link ->
        // /etc` through in Auto mode. Local writes keep their long-standing
        // behaviour; this is the remote project's own guard, and it is the
        // thing the phase-1 risk notes asked for (realpath, then write exactly
        // the string that was checked).
        if !permissions::inside_public(&key, &real_root(cwd).await?) {
            return Err(format!(
                "{} resolves to {} on the SSH host, which is outside this project, so OpenLeash won't write it. Use a path inside the project (or `path` of a file inside it), or ask the user to open the right folder.",
                p.path, target.path
            ));
        }
        let existing = match ss::stat(&target.host, &target.path).await {
            Ok(st) if st.is_dir => {
                return Err(format!("{} is a directory.", p.path));
            }
            Ok(_) => true,
            Err(e) if e.contains("no such file") => false,
            Err(e) => return Err(e),
        };
        let mut old = String::new();
        if existing {
            let (seen, seen_hash) = {
                let t = t.lock().await;
                (
                    t.read_files.get(&key).copied(),
                    t.read_hashes.get(&key).cloned(),
                )
            };
            let Some(seen) = seen else {
                return Err(format!(
                    "You must read {} with read_file before {}.",
                    p.path,
                    if name == "write_file" {
                        "overwriting it"
                    } else {
                        "editing it"
                    }
                ));
            };
            let (bytes, st) = tr::read_all(&target, tr::MAX_TEXT_BYTES).await?;
            let stale = tr::mtime_ms(&st) > seen
                || seen_hash.as_deref() != Some(tools::content_hash(&bytes).as_str());
            if stale {
                return Err(format!(
                    "{} was modified since you last read it (by the user or a command). Read it again before changing it.",
                    p.path
                ));
            }
            old = String::from_utf8(bytes)
                .map_err(|_| format!("cannot read: {} is not valid UTF-8", p.path))?;
        } else if name != "write_file" {
            return Err(format!(
                "{} does not exist. Use write_file to create it.",
                p.path
            ));
        }
        let new = if name == "edit_file" {
            tools::apply_edit(
                &old,
                input["old_string"].as_str().unwrap_or(""),
                input["new_string"].as_str().unwrap_or(""),
                input["replace_all"].as_bool().unwrap_or(false),
            )?
            .new_content
        } else if name == "multi_edit" {
            let edits = input["edits"]
                .as_array()
                .filter(|e| !e.is_empty())
                .ok_or("edits is required (at least one)")?;
            let mut cur = old.clone();
            for (i, e) in edits.iter().enumerate() {
                cur = tools::apply_edit(
                    &cur,
                    e["old_string"].as_str().unwrap_or(""),
                    e["new_string"].as_str().unwrap_or(""),
                    e["replace_all"].as_bool().unwrap_or(false),
                )
                .map_err(|err| {
                    format!(
                        "Edit {} of {} failed, nothing was written: {err}",
                        i + 1,
                        edits.len()
                    )
                })?
                .new_content;
            }
            cur
        } else {
            input["content"]
                .as_str()
                .ok_or("content is required")?
                .to_string()
        };
        let (lines, add, del) = tools::diff_lines(&old, &new);
        self.set_item(item_id, |i| {
            i.data["diff"] = json!(lines);
            i.data["meta"] = json!(format!("+{add} −{del}"));
        })
        .await;
        self.gate(name, input, Some(&key)).await?;
        self.step(format!("Editing {}", permissions::short(&key, cwd)))
            .await;
        // The review view's per-file original (no checkpoints for SSH yet).
        t.lock()
            .await
            .touched
            .entry(key.clone())
            .or_insert(existing.then(|| old.clone()));
        ss::create_dir_all(&target.host, &host::parent(&target.path))
            .await
            .map_err(|e| format!("cannot create directory: {e}"))?;
        ss::write(&target.host, &target.path, new.as_bytes())
            .await
            .map_err(|e| format!("write failed: {e}"))?;
        let mtime = ss::stat(&target.host, &target.path)
            .await
            .map(|s| tr::mtime_ms(&s))
            .unwrap_or(0);
        {
            let mut t = t.lock().await;
            t.read_files.insert(key.clone(), mtime);
            t.read_hashes
                .insert(key, tools::content_hash(new.as_bytes()));
        }
        Ok(if existing {
            format!("Updated {} (+{add} −{del}).", p.path)
        } else {
            format!("Created {} ({} lines).", p.path, new.lines().count())
        })
    }
}

#[cfg(test)]
#[path = "runner_remote_tests.rs"]
mod tests;
