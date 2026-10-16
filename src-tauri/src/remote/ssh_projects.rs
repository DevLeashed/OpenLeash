//! "Open over SSH…": the commands behind the dialog that picks a saved SSH
//! host, checks it, and browses its folders for a remote project.
//!
//! The project itself is then opened as `ssh://<host-id>/<path>` like any
//! other folder; everything here is read-only. Hosts are named by their
//! saved id only, never by an ssh target the UI passes in, so this surface
//! can't be used to point `ssh` at an arbitrary destination.
//!
//! Paths in a listing are plain absolute POSIX paths on that host (what the
//! user types into the picker's path box); the UI turns the chosen one into
//! a project URI. An `ssh://` URI for the *same* host is accepted as input
//! too, so a project path can be handed straight back.

use super::ssh::SshHost;
use super::{DirEntry, DirListing};
use crate::agent::host;
use crate::agent::ssh_session::{self, Entry, Stat};
use serde::Serialize;

/// Same cap as the local `remote_list_dir`, so the picker's "only the
/// first 2000" note is true for both.
const MAX_ENTRIES: usize = 2000;
/// Symlinks are listed without their target type (lstat); each one costs a
/// round trip to resolve, so a directory full of them (`/usr/lib`) is
/// resolved only this far and the rest are shown as files.
const MAX_LINK_STATS: usize = 256;

/// What the dialog shows after a successful connection test.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostTest {
    pub home: String,
    pub os: String,
    pub has_git: bool,
    pub has_rg: bool,
    pub has_bash: bool,
}

fn lookup_host(id: &str) -> Result<SshHost, String> {
    crate::remote::auth::Vault::load()
        .ssh_hosts
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(|| {
            "That SSH host isn't saved (any more). Add it again, or in Settings → Remote.".into()
        })
}

/// Turn what ssh (or the probe) said into something a user can act on, if
/// it is a connection problem we recognise. The original text is kept: it
/// is often the only clue for an odd setup.
fn hint_for(raw: &str, target: &str) -> Option<String> {
    let l = raw.to_lowercase();
    // ssh says "Permission denied (publickey,password)."; a filesystem
    // error from SFTP is a bare "…: permission denied" and is not an auth
    // problem.
    let hint = if l.contains("no `ssh` command") {
        return Some(raw.to_string());
    } else if l.contains("permission denied (")
        || l.contains("permission denied, please try again")
        || l.contains("too many authentication failures")
    {
        format!(
            "Key authentication to {target} failed. OpenLeash runs ssh in batch mode, so password prompts aren't supported: add your public key to the server (ssh-copy-id) or load it into your ssh agent, then check that `ssh {target}` logs in without asking for a password."
        )
    } else if l.contains("host key verification failed")
        || l.contains("remote host identification has changed")
    {
        format!(
            "{target}'s host key isn't trusted yet (or it changed). Run `ssh {target}` once in a terminal to check and accept it."
        )
    } else if l.contains("could not resolve hostname")
        || l.contains("name or service not known")
        || l.contains("no such host")
    {
        format!("{target} couldn't be found. Check the host name, or the alias in ~/.ssh/config.")
    } else if l.contains("connection refused")
        || l.contains("timed out")
        || l.contains("no route to host")
        || l.contains("network is unreachable")
        || l.contains("connection closed")
        || l.contains("connection reset")
        || l.contains("didn't answer in time")
    {
        format!(
            "{target} isn't reachable. Check that the machine is on, that sshd is running there, and that this network (or VPN) can reach it."
        )
    } else if l.contains("posix shell") {
        "That machine's login shell isn't a POSIX shell. Remote projects need Linux, macOS or BSD (Windows hosts aren't supported).".into()
    } else if l.contains("sftp") {
        format!(
            "Commands run on {target}, but its SFTP subsystem doesn't work. The server must allow sftp, and shell startup files must not print anything for non-interactive logins."
        )
    } else {
        return None;
    };
    Some(format!("{hint}\n\n{raw}"))
}

fn explain(raw: &str, target: &str) -> String {
    hint_for(raw, target).unwrap_or_else(|| format!("Couldn't connect to {target}: {raw}"))
}

fn test_result(caps: &ssh_session::Caps) -> HostTest {
    HostTest {
        home: host::normalize(&caps.home),
        os: caps.os.clone(),
        has_git: caps.has_git,
        has_rg: caps.has_rg,
        has_bash: caps.has_bash,
    }
}

/// Connect to saved host `id` from scratch (a command and SFTP), and report
/// what the machine offers.
#[tauri::command]
pub async fn ssh_host_test(id: String) -> Result<HostTest, String> {
    let h = lookup_host(&id)?;
    // `test_connection` drops any pooled session first, so this is a real
    // end-to-end check rather than a cached answer.
    ssh_session::test_connection(&id)
        .await
        .map_err(|e| explain(&e, &h.target))?;
    let caps = ssh_session::probe(&id)
        .await
        .map_err(|e| explain(&e, &h.target))?;
    Ok(test_result(&caps))
}

/// The folder to list: the remote home by default, an absolute POSIX path,
/// `~`/`~/…`, or an `ssh://` URI on this same host. Relative paths are
/// refused: there is no current directory to resolve them against.
fn resolve_input(id: &str, path: Option<&str>, home: &str) -> Result<String, String> {
    let p = match path.map(str::trim).filter(|p| !p.is_empty()) {
        None => return Ok(host::normalize(home)),
        Some(p) => p,
    };
    if p.contains(['\0', '\n', '\r']) {
        return Err("Paths can't contain NUL or line-break characters.".into());
    }
    if host::is_ssh(p) {
        let t = host::parse_ssh(p).ok_or_else(|| format!("{p} isn't a valid ssh:// path."))?;
        if t.host != id {
            return Err(format!("{p} is on a different SSH host."));
        }
        return Ok(t.path);
    }
    if p == "~" {
        return Ok(host::normalize(home));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return Ok(host::join(&host::normalize(home), rest));
    }
    if !p.starts_with('/') {
        return Err(format!(
            "{p} isn't an absolute path. Use a path on the remote machine like /home/me/project."
        ));
    }
    Ok(host::normalize(p))
}

/// The remote calls a listing needs, so the listing logic can be tested
/// against an in-process SFTP server instead of a real host.
trait RemoteFs {
    async fn home(&self) -> Result<String, String>;
    async fn stat(&self, path: &str) -> Result<Stat, String>;
    async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, String>;
}

/// A saved host, through the shared session pool.
struct Pooled<'a>(&'a str);

impl RemoteFs for Pooled<'_> {
    async fn home(&self) -> Result<String, String> {
        Ok(ssh_session::probe(self.0).await?.home)
    }
    async fn stat(&self, path: &str) -> Result<Stat, String> {
        ssh_session::stat(self.0, path).await
    }
    async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, String> {
        ssh_session::read_dir(self.0, path).await
    }
}

async fn list<F: RemoteFs>(
    fs: &F,
    id: &str,
    path: Option<&str>,
    dirs_only: bool,
) -> Result<DirListing, String> {
    let home = host::normalize(&fs.home().await?);
    let p = resolve_input(id, path, &home)?;
    match fs.stat(&p).await {
        Ok(s) if s.is_dir => {}
        Ok(_) => return Err(format!("{p} isn't a folder.")),
        Err(e) => return Err(format!("Couldn't open {p}: {e}")),
    }
    let mut entries: Vec<DirEntry> = vec![];
    let mut truncated = false;
    let mut link_stats = 0;
    for e in fs.read_dir(&p).await? {
        let full = host::join(&p, &e.name);
        let mut dir = e.is_dir;
        if !dir && e.is_symlink && link_stats < MAX_LINK_STATS {
            link_stats += 1;
            dir = fs.stat(&full).await.is_ok_and(|s| s.is_dir);
        }
        if dirs_only && !dir {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(DirEntry {
            name: e.name,
            path: full,
            dir,
        });
    }
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let mut roots = vec![home.clone()];
    if home != "/" {
        roots.push("/".into());
    }
    Ok(DirListing {
        parent: (p != "/").then(|| host::parent(&p)),
        path: p,
        entries,
        roots,
        truncated,
    })
}

/// Browse folders on saved host `id` for the "Open over SSH" picker.
/// Read-only; no broader than what `ssh <target>` already gives the user.
#[tauri::command]
pub async fn ssh_list_dir(
    id: String,
    path: Option<String>,
    dirs_only: Option<bool>,
) -> Result<DirListing, String> {
    let h = lookup_host(&id)?;
    list(
        &Pooled(&id),
        &id,
        path.as_deref(),
        dirs_only.unwrap_or(true),
    )
    .await
    // A connection failure gets the same hints as the test; anything else
    // (no such folder, permission denied on it) is already plain.
    .map_err(|e| hint_for(&e, &h.target).unwrap_or(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ssh_session::tests::{sample_fs, session_on, Node};
    use crate::agent::ssh_session::Sftp;

    struct Local {
        s: Sftp,
        home: String,
    }

    impl RemoteFs for Local {
        async fn home(&self) -> Result<String, String> {
            Ok(self.home.clone())
        }
        async fn stat(&self, path: &str) -> Result<Stat, String> {
            self.s.stat(path).await
        }
        async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, String> {
            self.s.read_dir(path).await
        }
    }

    async fn local(home: &str) -> (Local, crate::agent::ssh_session::tests::Fs) {
        let fs = sample_fs();
        let s = session_on(fs.clone(), false).await;
        (
            Local {
                s,
                home: home.into(),
            },
            fs,
        )
    }

    fn names(l: &DirListing) -> Vec<&str> {
        l.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[tokio::test]
    async fn defaults_to_home_and_lists_folders_first_with_posix_paths() {
        let (fs, _) = local("/proj").await;
        let l = list(&fs, "h1", None, false).await.unwrap();
        assert_eq!(l.path, "/proj");
        assert_eq!(l.parent.as_deref(), Some("/"));
        assert_eq!(l.roots, ["/proj", "/"]);
        assert!(!l.truncated);
        // Directories first (incl. symlinks to directories), then files,
        // case-insensitively.
        let dirs: Vec<_> = l
            .entries
            .iter()
            .filter(|e| e.dir)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(dirs, ["inner", "link_out", "linkdir", "src"]);
        assert!(l.entries.iter().any(|e| e.name == "a.txt" && !e.dir));
        let src = l.entries.iter().find(|e| e.name == "src").unwrap();
        assert_eq!(src.path, "/proj/src");
        // A dangling link and a link loop are not folders.
        assert!(l.entries.iter().any(|e| e.name == "dangle" && !e.dir));
        assert!(l.entries.iter().any(|e| e.name == "loop" && !e.dir));
    }

    #[tokio::test]
    async fn dirs_only_hides_files_and_root_has_no_parent() {
        let (fs, _) = local("/proj").await;
        let l = list(&fs, "h1", Some("/proj"), true).await.unwrap();
        assert!(l.entries.iter().all(|e| e.dir), "{:?}", names(&l));
        let l = list(&fs, "h1", Some("/"), true).await.unwrap();
        assert_eq!(l.path, "/");
        assert_eq!(l.parent, None);
        assert!(names(&l).contains(&"proj"));
        let root = list(&fs, "h1", None, true).await;
        assert!(root.is_ok());
        let (fs, _) = local("/").await;
        assert_eq!(list(&fs, "h1", None, true).await.unwrap().roots, ["/"]);
    }

    #[tokio::test]
    async fn accepts_tilde_and_same_host_uris_and_normalizes() {
        let (fs, _) = local("/proj").await;
        assert_eq!(
            list(&fs, "h1", Some("~/src"), true).await.unwrap().path,
            "/proj/src"
        );
        assert_eq!(
            list(&fs, "h1", Some("~"), true).await.unwrap().path,
            "/proj"
        );
        assert_eq!(
            list(&fs, "h1", Some("ssh://h1/proj/src/../src/"), true)
                .await
                .unwrap()
                .path,
            "/proj/src"
        );
        assert_eq!(
            list(&fs, "h1", Some("  /deep//a/./b "), true)
                .await
                .unwrap()
                .path,
            "/deep/a/b"
        );
    }

    #[tokio::test]
    async fn rejects_relative_other_host_missing_and_non_folder_paths() {
        let (fs, _) = local("/proj").await;
        for (bad, want) in [
            ("proj/src", "absolute"),
            ("./src", "absolute"),
            ("C:\\Users", "absolute"),
            ("ssh://h2/proj", "different SSH host"),
            ("ssh://me@h1/proj", "valid ssh://"),
            ("/proj\n/etc", "line-break"),
            ("/proj/nope", "no such file"),
            ("/proj/a.txt", "isn't a folder"),
        ] {
            let e = list(&fs, "h1", Some(bad), true).await.err().unwrap();
            assert!(e.contains(want), "{bad:?}: {e}");
        }
    }

    #[tokio::test]
    async fn caps_entries_like_the_local_picker() {
        let (fs, raw) = local("/").await;
        {
            let mut m = raw.lock().unwrap();
            m.insert("/many".into(), Node::Dir);
            for i in 0..(MAX_ENTRIES + 5) {
                m.insert(format!("/many/d{i:05}"), Node::Dir);
            }
        }
        let l = list(&fs, "h1", Some("/many"), true).await.unwrap();
        assert_eq!(l.entries.len(), MAX_ENTRIES);
        assert!(l.truncated);
    }

    #[test]
    fn errors_are_explained_with_the_raw_text_kept() {
        let cases = [
            ("ssh: me@box: Permission denied (publickey).", "password prompts aren't supported"),
            ("ssh: Host key verification failed.", "host key"),
            ("ssh: Could not resolve hostname nobox: Name or service not known", "couldn't be found"),
            ("ssh: connect to host box port 22: Connection refused", "isn't reachable"),
            ("ssh: connect to host box port 22: Connection timed out", "isn't reachable"),
            (
                "The SSH host didn't answer like a POSIX shell (Linux, macOS or BSD are supported).",
                "isn't a POSIX shell",
            ),
            ("Commands work, but SFTP doesn't: subsystem request failed", "SFTP subsystem"),
        ];
        for (raw, want) in cases {
            let e = explain(raw, "me@box");
            assert!(e.contains(want), "{raw}: {e}");
            assert!(e.contains(raw), "the original text is kept: {e}");
        }
        let e = explain("something odd", "me@box");
        assert_eq!(e, "Couldn't connect to me@box: something odd");
        // Filesystem answers are not connection problems.
        assert_eq!(hint_for("stat /root: permission denied", "me@box"), None);
        assert_eq!(
            hint_for("list /x: no such file or directory", "me@box"),
            None
        );
        let missing = "No `ssh` command found. Install OpenSSH (on Windows: Settings → Optional features → OpenSSH Client).";
        assert_eq!(explain(missing, "x"), missing);
    }

    #[test]
    fn test_result_normalizes_home_and_keeps_caps() {
        let caps = ssh_session::Caps {
            os: "Linux".into(),
            home: "/home/me/".into(),
            has_git: true,
            has_rg: false,
            has_bash: true,
            ..Default::default()
        };
        assert_eq!(
            test_result(&caps),
            HostTest {
                home: "/home/me".into(),
                os: "Linux".into(),
                has_git: true,
                has_rg: false,
                has_bash: true,
            }
        );
    }

    // A plain test driving its own runtime: the home lock is held for the
    // whole test without being held across an `.await`.
    #[test]
    fn unknown_host_ids_are_refused_before_any_ssh() {
        let dir =
            std::env::temp_dir().join(format!("ol-ssh-projects-{}", uuid::Uuid::new_v4().simple()));
        let _g = crate::agent::store::test_home(&dir);
        // A saved host, to show it is the id that matters, not the vault
        // being empty.
        let mut v = crate::remote::auth::Vault::load();
        v.ssh_hosts.push(SshHost {
            id: "known01".into(),
            name: "Box".into(),
            target: "me@box".into(),
        });
        v.save();
        assert_eq!(lookup_host("known01").unwrap().target, "me@box");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for id in ["nope", "", "me@box", "known01 "] {
            let e = rt.block_on(ssh_host_test(id.into())).unwrap_err();
            assert!(e.contains("isn't saved"), "{id:?}: {e}");
            let e = rt
                .block_on(ssh_list_dir(id.into(), None, None))
                .err()
                .unwrap();
            assert!(e.contains("isn't saved"), "{id:?}: {e}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
