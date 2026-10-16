//! Open another machine's OpenLeash over SSH.
//!
//! The agent keeps running *there*, on that machine's files and toolchain, and
//! this app only provides the window: it starts an SSH port forward to the
//! remote app's remote-control server and opens the forwarded page in the
//! browser. That is the Codex / VS Code remote model, built on the remote
//! control server rather than a second protocol.
//!
//! Everything SSH is the system `ssh` client: it already knows the user's
//! `~/.ssh/config`, agent, keys and `known_hosts`, so OpenLeash never handles
//! a key, a password, or a host-key decision. `BatchMode` makes it fail
//! instead of prompting, because there is no terminal for it to prompt on.
//!
//! Pairing: anyone who can SSH into the machine as the user can already read
//! `~/.openleash/remote.json` and do anything the app can, so reading the
//! pairing secret over the same SSH connection adds no power — it only saves
//! the user from walking to the other machine to read a code.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::{Child, Command};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshHost {
    pub id: String,
    /// Display name.
    pub name: String,
    /// What you would type after `ssh`: an alias from `~/.ssh/config`, or
    /// `user@host`. Never an option.
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SshHostView {
    #[serde(flatten)]
    pub host: SshHost,
    pub connected: bool,
    pub local_url: Option<String>,
    pub error: String,
}

struct Tunnel {
    child: Child,
    local_port: u16,
}

#[derive(Default)]
pub struct Tunnels {
    live: Mutex<HashMap<String, Tunnel>>,
    errors: Mutex<HashMap<String, String>>,
}

/// Reject anything ssh would read as an option, or that is not a plain
/// destination. A target comes from the user, but it is also the one string
/// that reaches a command line, so it is held to the narrow grammar.
pub fn valid_target(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty()
        && t.len() <= 255
        && !t.starts_with('-')
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._-:[]".contains(c))
}

pub(crate) fn ssh() -> Command {
    quiet(Command::new(
        std::env::var("OPENLEASH_SSH").unwrap_or_else(|_| "ssh".into()),
    ))
}

fn quiet(mut c: Command) -> Command {
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

pub(crate) const BASE_OPTS: [&str; 6] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=15",
    "-o",
    "ServerAliveInterval=30",
];

/// The remote app's remote-control settings, read over SSH. Tries a POSIX
/// home first, then a Windows one.
///
/// The POSIX attempt is ONE argument on purpose: ssh joins the remote argv
/// with spaces and hands the result to the login shell unquoted, so the old
/// `["sh", "-c", "cat …"]` ran `sh -c cat <path>` — a `cat` of nothing.
/// Single quotes keep the `${…:-…}` away from a non-POSIX login shell (fish).
/// See `WIN_VAULT` for the Windows attempt.
pub(crate) async fn remote_vault(target: &str) -> Result<super::auth::Vault, String> {
    let attempts: [&[&str]; 2] = [
        &["sh -c 'cat \"${OPENLEASH_HOME:-$HOME/.openleash}/remote.json\"'"],
        &[WIN_VAULT],
    ];
    let mut last = String::new();
    for cmd in attempts {
        let out = ssh()
            .args(BASE_OPTS)
            .arg("--")
            .arg(target)
            .args(cmd)
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "No `ssh` command found. Install OpenSSH (on Windows: Settings → Optional features → OpenSSH Client).".to_string()
                } else {
                    format!("couldn't run ssh: {e}")
                }
            })?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            return serde_json::from_str(text.trim())
                .map_err(|_| "The remote OpenLeash's settings couldn't be read.".to_string());
        }
        last = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // An authentication or connection failure won't improve on retry.
        if out.status.code() == Some(255) {
            break;
        }
    }
    Err(if last.is_empty() {
        "OpenLeash doesn't seem to be set up on that machine (no ~/.openleash/remote.json).".into()
    } else {
        format!("ssh: {last}")
    })
}

/// The Windows attempt (the default `cmd` login shell). One argument, and the
/// path quoted: `%USERPROFILE%` expands before `type` sees it, so a profile
/// folder with a space (`C:\Users\Jane Doe`) would otherwise split in two.
const WIN_VAULT: &str = r#"cmd /d /c type "%USERPROFILE%\.openleash\remote.json""#;

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

pub(crate) fn remote_off(name: &str) -> String {
    format!(
        "Remote control is off on {name}. Turn it on there in Settings → Remote (\"This computer\" is enough: the tunnel arrives locally)."
    )
}

/// Forward a free local port to `remote_port` on the far side's loopback and
/// wait until it accepts connections. The caller owns the `ssh -N` child: drop
/// or kill it to close the tunnel. Opens nothing in a browser, so the chat
/// transfer can use the same tunnel the Remote screen does.
pub(crate) async fn open_tunnel(target: &str, remote_port: u16) -> Result<(Child, u16), String> {
    if !valid_target(target) {
        return Err("That SSH destination isn't valid. Use a host alias or user@host.".into());
    }
    let local = free_port()?;
    let forward = format!("127.0.0.1:{local}:127.0.0.1:{remote_port}");
    let mut child = ssh()
        .args(BASE_OPTS)
        .args([
            "-o",
            "ExitOnForwardFailure=yes",
            "-N",
            "-L",
            &forward,
            "--",
            target,
        ])
        .spawn()
        .map_err(|e| format!("couldn't run ssh: {e}"))?;
    // Wait for the forward to accept connections, or for ssh to give up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = e.read_to_string(&mut err).await;
            }
            return Err(format!("ssh: {}", err.trim()));
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", local))
            .await
            .is_ok()
        {
            return Ok((child, local));
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.start_kill();
            return Err("The SSH tunnel didn't come up in time.".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

impl Tunnels {
    pub fn view(&self, hosts: &[SshHost]) -> Vec<SshHostView> {
        let mut live = self.live.lock().unwrap();
        // Drop tunnels whose ssh process has exited (network drop, remote quit).
        let mut dead = vec![];
        for (id, t) in live.iter_mut() {
            if !matches!(t.child.try_wait(), Ok(None)) {
                dead.push(id.clone());
            }
        }
        for id in dead {
            live.remove(&id);
            self.errors
                .lock()
                .unwrap()
                .entry(id)
                .or_insert_with(|| "The SSH connection closed.".into());
        }
        let errors = self.errors.lock().unwrap();
        hosts
            .iter()
            .map(|h| {
                let t = live.get(&h.id);
                SshHostView {
                    host: h.clone(),
                    connected: t.is_some(),
                    local_url: t.map(|t| format!("http://127.0.0.1:{}", t.local_port)),
                    error: errors.get(&h.id).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Forward a local port to the remote app and return a one-time pairing
    /// URL for it.
    pub async fn connect(&self, host: &SshHost) -> Result<String, String> {
        if !valid_target(&host.target) {
            return Err("That SSH destination isn't valid. Use a host alias or user@host.".into());
        }
        self.disconnect(&host.id);
        self.errors.lock().unwrap().remove(&host.id);
        let r = self.connect_inner(host).await;
        if let Err(e) = &r {
            self.errors
                .lock()
                .unwrap()
                .insert(host.id.clone(), e.clone());
        }
        r
    }

    async fn connect_inner(&self, host: &SshHost) -> Result<String, String> {
        let vault = remote_vault(&host.target).await?;
        if !vault.enabled {
            return Err(remote_off(&host.name));
        }
        let (child, local) = open_tunnel(&host.target, vault.port).await?;
        self.live.lock().unwrap().insert(
            host.id.clone(),
            Tunnel {
                child,
                local_port: local,
            },
        );
        Ok(format!(
            "http://127.0.0.1:{local}/pair?t={}",
            vault.pair_token
        ))
    }

    pub fn disconnect(&self, id: &str) {
        if let Some(mut t) = self.live.lock().unwrap().remove(id) {
            let _ = t.child.start_kill();
        }
    }

    pub fn disconnect_all(&self) {
        for (_, mut t) in self.live.lock().unwrap().drain() {
            let _ = t.child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_cannot_be_options_or_carry_shell_syntax() {
        for ok in [
            "devbox",
            "me@devbox",
            "me@10.0.0.4",
            "build-01.internal",
            "me@[fe80::1]",
        ] {
            assert!(valid_target(ok), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=calc",
            "-L",
            "a b",
            "me@host;rm -rf /",
            "host\nx",
            "$(id)",
            "`id`",
            "a|b",
        ] {
            assert!(!valid_target(bad), "{bad:?}");
        }
    }

    #[test]
    fn windows_vault_paths_are_quoted() {
        // One argument, so ssh doesn't re-split it; every expansion quoted.
        assert_eq!(
            WIN_VAULT,
            r#"cmd /d /c type "%USERPROFILE%\.openleash\remote.json""#
        );
    }
}
