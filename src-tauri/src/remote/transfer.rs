//! Send a chat to another machine over SSH (the sending half; the receiving
//! half is `agent::transfer`).
//!
//! Protocol, all of it over the user's own `ssh`:
//!   1. Over ssh, create `$OPENLEASH_HOME/imports` (default
//!      `~/.openleash/imports`) and print its absolute path behind a marker
//!      (POSIX `sh`, falling back to Windows PowerShell). That is the folder
//!      the importer there reads, whatever the remote home is called.
//!   2. If the remote app has remote control on, open an `ssh -L` tunnel to it,
//!      pair with the pairing token read over SSH (`GET /pair`, cookie kept in
//!      memory only), and open the remote-control WebSocket. That link stays
//!      up while the dialog is open so it can probe and browse folders there.
//!   3. Send: the chat is written to a temp file as an `openleash-chats` v2
//!      bundle, streamed over ssh's stdin into `imports/<uuid>.json.part`,
//!      size-checked and renamed to `.json` in the same remote command, and
//!      `task_import_bundle {name, project}` is invoked over the WebSocket.
//!      The bundle itself never goes through the WebSocket.
//!   4. Close: the WebSocket goes, the session is logged out (so transfers
//!      don't eat the 20-session limit), the tunnel is killed.
//!
//! If the remote app isn't reachable, step 3 stops after the rename: the
//! bundle waits in `imports/` and is imported the next time OpenLeash starts
//! there. Move archives the chat here only after the import is confirmed.

use super::ssh::{self, SshHost, BASE_OPTS};
use crate::agent::host::{login_command, sh_quote};
use crate::agent::transfer::{self as xfer, Origin};
use crate::agent::{git, runner, Harness, Item};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as WsMsg;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Links idle this long are closed the next time one is opened.
const IDLE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shell {
    Posix,
    /// Windows OpenSSH, whose login shell is `cmd` or PowerShell; either
    /// can start `powershell -EncodedCommand`.
    Windows,
}

struct Conn {
    tunnel: tokio::process::Child,
    base: String,
    cookie: String,
    ws: Ws,
    next: u64,
}

struct Link {
    host: SshHost,
    shell: Shell,
    /// The remote imports folder, absolute, as that machine's shell names it.
    dir: String,
    conn: Option<Conn>,
    last: Instant,
}

type LinkRef = Arc<tokio::sync::Mutex<Link>>;

fn links() -> &'static Mutex<HashMap<String, LinkRef>> {
    static L: OnceLock<Mutex<HashMap<String, LinkRef>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// App exit: kill every tunnel. (No logout: there is no runtime left to send
/// it on. The remote side keeps one stale session, revocable there.)
pub fn close_all() {
    for (_, l) in links().lock().unwrap().drain() {
        if let Ok(mut l) = l.try_lock() {
            if let Some(c) = l.conn.as_mut() {
                let _ = c.tunnel.start_kill();
            }
        }
    }
}

fn ssh_err(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr).trim().to_string();
    if s.is_empty() {
        "ssh failed".into()
    } else {
        format!("ssh: {s}")
    }
}

fn spawn_err(what: &str, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("No `{what}` command found. Install OpenSSH (on Windows: Settings → Optional features → OpenSSH Client).")
    } else {
        format!("couldn't run {what}: {e}")
    }
}

/// Marks the line carrying the imports folder, so whatever a login script
/// prints around it (motd, banners, `echo` in `.bashrc`) can't be mistaken
/// for the path.
const DIR_MARK: &str = "OPENLEASH_IMPORTS_DIR=";

/// POSIX: create the folder the importer there reads and print where it is.
/// `login_command` keeps the script intact through any login shell (fish).
fn posix_prepare() -> String {
    login_command(
        r#"d="${OPENLEASH_HOME:-$HOME/.openleash}/imports"; mkdir -p -- "$d" && cd -- "$d" && echo "$1$PWD""#,
        &[DIR_MARK],
    )
}

/// Windows: the same, from `%OPENLEASH_HOME%` or the profile folder.
fn windows_prepare() -> String {
    powershell(&format!(
        "$ErrorActionPreference='Stop'; $h=$env:OPENLEASH_HOME; if (-not $h) {{ $h=[IO.Path]::Combine($env:USERPROFILE,'.openleash') }}; $d=[IO.Path]::Combine($h,'imports'); [void][IO.Directory]::CreateDirectory($d); [Console]::Out.Write({} + [IO.Path]::GetFullPath($d) + \"`n\")",
        ps_quote(DIR_MARK)
    ))
}

/// A PowerShell script as one command line. `-EncodedCommand` (base64 of
/// UTF-16LE) means neither `cmd` nor PowerShell as the login shell gets to
/// parse, split or expand any of it.
fn powershell(script: &str) -> String {
    use base64::Engine;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell -NoProfile -NonInteractive -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(utf16)
    )
}

/// A PowerShell single-quoted literal. PowerShell also closes a literal on
/// the typographic single quotes, so those are doubled too.
fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The marked folder from the prepare step's output, if it is a usable
/// absolute path for that shell. The last marked line wins.
fn marked_dir(stdout: &[u8], shell: Shell) -> Option<String> {
    let text = String::from_utf8(stdout.to_vec()).ok()?;
    let dir = text
        .lines()
        .filter_map(|l| l.strip_prefix(DIR_MARK))
        .last()?
        .trim_end_matches('\r');
    let absolute = match shell {
        Shell::Posix => dir.starts_with('/'),
        Shell::Windows => {
            let b = dir.as_bytes();
            dir.starts_with(r"\\")
                || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
        }
    };
    (absolute && !dir.chars().any(char::is_control)).then(|| dir.to_string())
}

/// Make sure the remote imports folder exists, learn its absolute path and
/// what shell is there.
async fn prepare(target: &str) -> Result<(Shell, String), String> {
    let run = |cmd: String| async move {
        ssh::ssh()
            .args(BASE_OPTS)
            .args(["--", target, &cmd])
            .output()
            .await
            .map_err(|e| spawn_err("ssh", e))
    };
    let no_dir = || {
        "Couldn't work out the OpenLeash imports folder on that machine, so the chat can't be put where OpenLeash there would find it.".to_string()
    };
    let out = run(posix_prepare()).await?;
    if out.status.success() {
        return marked_dir(&out.stdout, Shell::Posix)
            .map(|d| (Shell::Posix, d))
            .ok_or_else(no_dir);
    }
    // 255 is ssh's own failure (auth, network): a different shell won't help.
    if out.status.code() == Some(255) {
        return Err(ssh_err(&out.stderr));
    }
    let out2 = run(windows_prepare()).await?;
    if out2.status.success() {
        return marked_dir(&out2.stdout, Shell::Windows)
            .map(|d| (Shell::Windows, d))
            .ok_or_else(no_dir);
    }
    Err(ssh_err(if out2.stderr.is_empty() {
        &out.stderr
    } else {
        &out2.stderr
    }))
}

/// Open the remote-control session: tunnel, pair, WebSocket.
async fn connect(host: &SshHost) -> Result<Conn, String> {
    let vault = ssh::remote_vault(&host.target).await?;
    if !vault.enabled {
        return Err(ssh::remote_off(&host.name));
    }
    let (mut tunnel, local) = ssh::open_tunnel(&host.target, vault.port).await?;
    match pair_and_open(local, &vault.pair_token, &host.name).await {
        Ok((cookie, ws)) => Ok(Conn {
            tunnel,
            base: format!("http://127.0.0.1:{local}"),
            cookie,
            ws,
            next: 1,
        }),
        Err(e) => {
            let _ = tunnel.start_kill();
            Err(e)
        }
    }
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .user_agent(format!(
            "OpenLeash/{} chat transfer",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|e| e.to_string())
}

/// The `ol_session_*` pair from a `Set-Cookie`, attributes dropped.
fn session_cookie(set_cookie: &str) -> Option<String> {
    let pair = set_cookie.split(';').next()?.trim();
    let (name, value) = pair.split_once('=')?;
    (name.starts_with("ol_session_") && !value.is_empty()).then(|| pair.to_string())
}

async fn pair_and_open(local: u16, token: &str, name: &str) -> Result<(String, Ws), String> {
    let base = format!("http://127.0.0.1:{local}");
    let not_running = || format!("OpenLeash doesn't seem to be running on {name}.");
    let res = http()?
        .get(format!("{base}/pair?t={}", urlencoding::encode(token)))
        .send()
        .await
        .map_err(|_| not_running())?;
    if !res.status().is_redirection() {
        return Err(format!("{name} refused the pairing ({}).", res.status()));
    }
    let cookie = res
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(session_cookie)
        .ok_or_else(|| format!("{name} didn't hand out a session."))?;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{local}/__openleash/ws")
        .into_client_request()
        .map_err(|e| e.to_string())?;
    let hv = |s: &str| {
        s.parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
            .map_err(|e| e.to_string())
    };
    req.headers_mut().insert("Cookie", hv(&cookie)?);
    // The server insists Origin is exactly the host it was addressed as.
    req.headers_mut().insert("Origin", hv(&base)?);
    let (ws, _) = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async(req),
    )
    .await
    .map_err(|_| not_running())?
    .map_err(|e| format!("Couldn't open the remote session on {name}: {e}"))?;
    Ok((cookie, ws))
}

/// Why a remote call failed: the remote command said no, or the link broke.
enum CallErr {
    Refused(String),
    Link(String),
}

impl CallErr {
    fn text(self) -> String {
        match self {
            CallErr::Refused(s) | CallErr::Link(s) => s,
        }
    }
}

impl Conn {
    async fn call(&mut self, cmd: &str, args: Value, wait: Duration) -> Result<Value, CallErr> {
        let id = self.next;
        self.next += 1;
        let msg = json!({"id": id, "cmd": cmd, "args": args}).to_string();
        self.ws
            .send(WsMsg::Text(msg.into()))
            .await
            .map_err(|e| CallErr::Link(format!("The connection dropped: {e}")))?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let m = tokio::time::timeout_at(deadline, self.ws.next())
                .await
                .map_err(|_| CallErr::Link("The other machine didn't answer in time.".into()))?;
            let text = match m {
                Some(Ok(WsMsg::Text(t))) => t.to_string(),
                Some(Ok(WsMsg::Close(_))) | None => {
                    return Err(CallErr::Link(
                        "The other machine closed the connection.".into(),
                    ))
                }
                Some(Err(e)) => return Err(CallErr::Link(format!("The connection dropped: {e}"))),
                _ => continue,
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if v["t"] == "bye" {
                return Err(CallErr::Link("The other machine ended the session.".into()));
            }
            if v["t"] != "result" || v["id"].as_u64() != Some(id) {
                continue;
            }
            return if v["ok"] == true {
                Ok(v["data"].clone())
            } else {
                Err(CallErr::Refused(xfer::value_text(&v["error"])))
            };
        }
    }

    /// Close the socket, revoke the session, drop the tunnel. Best effort.
    async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), self.ws.close(None)).await;
        if let Ok(c) = http() {
            let _ = c
                .post(format!("{}/__openleash/logout", self.base))
                .header("Cookie", &self.cookie)
                .header("Origin", &self.base)
                .send()
                .await;
        }
        let _ = self.tunnel.start_kill();
    }
}

async fn close_link(id: &str) {
    let l = links().lock().unwrap().remove(id);
    if let Some(l) = l {
        let conn = l.lock().await.conn.take();
        if let Some(c) = conn {
            c.close().await;
        }
    }
}

async fn reap_idle() {
    let ids: Vec<String> = {
        let map = links().lock().unwrap();
        map.iter()
            .filter(|(_, l)| l.try_lock().is_ok_and(|l| l.last.elapsed() > IDLE))
            .map(|(k, _)| k.clone())
            .collect()
    };
    for id in ids {
        close_link(&id).await;
    }
}

fn link(id: &str) -> Result<LinkRef, String> {
    links()
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| "That connection has closed. Open the dialog again.".into())
}

/// The remote command that writes ssh's stdin to `<dir>/<name>.part`, checks
/// it is exactly `len` bytes, and only then renames it to `<dir>/<name>`.
/// Only a finished, complete copy gets the `.json` name the importer looks
/// for, so a launch there mid-copy, or a connection that drops (which ends
/// `cat` with a clean EOF), can't import half a chat.
///
/// The folder goes in as a quoted literal rather than through scp: scp's
/// remote path is shell-parsed by legacy scp but taken verbatim by SFTP-mode
/// scp (OpenSSH 9+), so no single quoting is right for both.
fn upload_command(shell: Shell, dir: &str, name: &str, len: u64) -> String {
    let part = format!("{name}.part");
    match shell {
        // Paths travel as `sh -c` positional arguments, never spliced into the
        // script: `login_command` quotes each argument once, and a path quoted
        // again inside the script would come out mangled (or unbalanced).
        Shell::Posix => login_command(
            r#"cat > "$1" && n=$(wc -c < "$1" | tr -d ' ') && [ "$n" = "$3" ] && mv -f -- "$1" "$2" || { rm -f -- "$1"; exit 3; }"#,
            &[
                &part_path(dir, &part),
                &part_path(dir, name),
                &len.to_string(),
            ],
        ),
        Shell::Windows => powershell(&format!(
            "$ErrorActionPreference='Stop'; $d={d}; $p=[IO.Path]::Combine($d,{part}); $n=[IO.Path]::Combine($d,{name}); $f=[IO.File]::Create($p); try {{ [Console]::OpenStandardInput().CopyTo($f) }} finally {{ $f.Close() }}; if ((New-Object IO.FileInfo $p).Length -ne {len}) {{ [IO.File]::Delete($p); exit 3 }}; if ([IO.File]::Exists($n)) {{ [IO.File]::Delete($n) }}; [IO.File]::Move($p,$n)",
            d = ps_quote(dir),
            part = ps_quote(&part),
            name = ps_quote(name),
        )),
    }
}

fn part_path(dir: &str, part: &str) -> String {
    format!("{}/{part}", dir.trim_end_matches('/'))
}

async fn upload(l: &Link, local: &std::path::Path, name: &str) -> Result<(), String> {
    let file = std::fs::File::open(local).map_err(|e| format!("Couldn't read the bundle: {e}"))?;
    let len = file
        .metadata()
        .map_err(|e| format!("Couldn't read the bundle: {e}"))?
        .len();
    let cmd = upload_command(l.shell, &l.dir, name, len);
    let out = ssh::ssh()
        .stdin(file)
        .args(BASE_OPTS)
        .args(["--", &l.host.target, &cmd])
        .output()
        .await
        .map_err(|e| spawn_err("ssh", e))?;
    if !out.status.success() {
        return Err(format!("Copying the chat failed. {}", ssh_err(&out.stderr)));
    }
    Ok(())
}

// ───────────────────────────── commands ─────────────────────────────

type R<'a> = tauri::State<'a, Arc<super::Remote>>;
type H<'a> = tauri::State<'a, Arc<Harness>>;

#[derive(Serialize)]
pub struct LinkView {
    pub link: String,
    pub host_name: String,
    /// OpenLeash answered over the tunnel.
    pub online: bool,
    /// Why not, when it didn't.
    pub reason: String,
    pub probe: Option<Value>,
}

/// Connect to a saved SSH host for a transfer. Succeeds without the remote
/// app (`online: false`) as long as SSH itself works: the chat can still be
/// left in the imports folder there.
#[tauri::command]
pub async fn transfer_open(
    r: R<'_>,
    h: H<'_>,
    host_id: String,
    task_id: String,
) -> Result<LinkView, String> {
    reap_idle().await;
    let host = super::ssh_hosts(&r)
        .into_iter()
        .find(|x| x.id == host_id)
        .ok_or("No such SSH host.")?;
    if !ssh::valid_target(&host.target) {
        return Err("That SSH destination isn't valid. Use a host alias or user@host.".into());
    }
    let cwd = {
        let t = h.task(&task_id).await?;
        let t = t.lock().await;
        if t.project.is_empty() {
            t.cwd.clone()
        } else {
            t.project.clone()
        }
    };
    let url =
        tokio::task::spawn_blocking(move || git::remote_url(&cwd).map(|u| xfer::redact_url(&u)))
            .await
            .unwrap_or(None);
    let (shell, dir) = prepare(&host.target).await?;
    let (conn, reason) = match connect(&host).await {
        Ok(c) => (Some(c), String::new()),
        Err(e) => (None, e),
    };
    let mut l = Link {
        host: host.clone(),
        shell,
        dir,
        conn,
        last: Instant::now(),
    };
    let mut probe = None;
    let mut reason = reason;
    if let Some(c) = l.conn.as_mut() {
        match c
            .call(
                "transfer_probe",
                json!({"originUrl": url}),
                Duration::from_secs(60),
            )
            .await
        {
            Ok(v) => probe = Some(v),
            // An older OpenLeash there has no transfer commands: it can still
            // pick the bundle up if a newer one starts, but not import it now.
            Err(e) => {
                reason = format!(
                    "OpenLeash on {} can't receive chats yet (update it there): {}",
                    host.name,
                    e.text()
                );
                if let Some(c) = l.conn.take() {
                    c.close().await;
                }
            }
        }
    }
    let online = l.conn.is_some();
    let id = crate::agent::new_id();
    links()
        .lock()
        .unwrap()
        .insert(id.clone(), Arc::new(tokio::sync::Mutex::new(l)));
    Ok(LinkView {
        link: id,
        host_name: host.name,
        online,
        reason,
        probe,
    })
}

/// Browse folders on the other machine (its own `remote_list_dir`).
#[tauri::command]
pub async fn transfer_list_dir(link: String, path: Option<String>) -> Result<Value, String> {
    let l = self::link(&link)?;
    let mut l = l.lock().await;
    l.last = Instant::now();
    let c = l.conn.as_mut().ok_or(
        "OpenLeash isn't running there, so its folders can't be browsed. Type a path instead.",
    )?;
    c.call(
        "remote_list_dir",
        json!({"path": path, "dirsOnly": true}),
        Duration::from_secs(60),
    )
    .await
    .map_err(CallErr::text)
}

#[tauri::command]
pub async fn transfer_close(link: String) {
    close_link(&link).await;
}

/// Uncommitted or unpushed work in the chat's project: only the conversation travels.
#[tauri::command]
pub async fn transfer_check(h: H<'_>, task_id: String) -> Result<xfer::CodeState, String> {
    let cwd = h.task(&task_id).await?.lock().await.cwd.clone();
    tokio::task::spawn_blocking(move || xfer::code_state(&cwd))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct SendView {
    /// imported | pending | unconfirmed
    pub status: String,
    pub message: String,
    pub remote: Option<Value>,
}

/// Deletes the temp bundle however the send ends.
struct TempFile(std::path::PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tauri::command]
pub async fn transfer_send(
    h: H<'_>,
    link: String,
    task_id: String,
    mode: String,
    project: Option<String>,
    resume: bool,
) -> Result<SendView, String> {
    let moved = match mode.as_str() {
        "move" => true,
        "copy" => false,
        _ => return Err(format!("unknown transfer mode: {mode}")),
    };
    let h = h.inner().clone();
    let l = self::link(&link)?;
    let mut l = l.lock().await;
    l.last = Instant::now();
    let to = l.host.name.clone();
    let r = send(&h, &mut l, &task_id, moved, project, resume).await;
    l.last = Instant::now();
    drop(l);
    match &r {
        Ok(v) => {
            let (text, archive) = sent_notice(moved, &to, v);
            h.upsert_item(
                &task_id,
                Item::new("notice", text, json!({"level": "info"})),
            )
            .await;
            h.save_task_now(&task_id).await;
            if archive {
                crate::apply_patch(&h, &task_id, json!({"archived": true})).await?;
            }
        }
        Err(e) => {
            h.upsert_item(
                &task_id,
                Item::new(
                    "notice",
                    format!("Couldn't send this chat to {to}: {e}"),
                    json!({"level": "error"}),
                ),
            )
            .await;
            h.save_task_now(&task_id).await;
        }
    }
    r
}

/// The notice a finished send leaves in the chat, and whether to archive it.
/// Only an import the other machine confirmed archives a moved chat: a
/// `pending` or `unconfirmed` bundle may never be imported (a disk that
/// fills, an app that is never started there), and archiving the original
/// then would leave the user with no live copy anywhere.
fn sent_notice(moved: bool, to: &str, v: &SendView) -> (String, bool) {
    let verb = if moved { "Moved" } else { "Copied" };
    if v.status == "imported" {
        let p = v
            .remote
            .as_ref()
            .and_then(|x| x["project"].as_str())
            .unwrap_or("");
        return (format!("{verb} to {to} · {p}"), moved);
    }
    if moved {
        return (
            format!(
                "Sent to {to} · {}. Not archived here: {to} hasn't confirmed the import, so the chat waits on {to}. This chat is stopped; archive it yourself once it shows up there.",
                v.message
            ),
            false,
        );
    }
    (format!("{verb} to {to} · {}", v.message), false)
}

async fn send(
    h: &Arc<Harness>,
    l: &mut Link,
    task_id: &str,
    moved: bool,
    project: Option<String>,
    resume: bool,
) -> Result<SendView, String> {
    let t = h.task(task_id).await?;
    let (was_running, active_subs) = {
        let t = t.lock().await;
        (
            matches!(t.status.as_str(), "running" | "waiting"),
            t.subs
                .iter()
                .filter(|s| s.status == "running")
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
        )
    };
    if moved {
        runner::interrupt(h, task_id).await;
        // Let the run wind down so the snapshot is the stopped chat.
        let rt = h.runtime(task_id);
        for _ in 0..80 {
            if !rt.running.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(125)).await;
        }
    }
    h.save_task_now(task_id).await;
    let snap = t.lock().await.clone();
    let (project_a, cwd) = (snap.project.clone(), snap.cwd.clone());
    let git_dir = if cwd.is_empty() {
        project_a.clone()
    } else {
        cwd.clone()
    };
    let (head, url) = tokio::task::spawn_blocking(move || {
        (
            git::head(&git_dir),
            git::remote_url(&git_dir).map(|u| xfer::redact_url(&u)),
        )
    })
    .await
    .unwrap_or((None, None));
    let origin = Origin {
        host: xfer::machine_name(),
        os: xfer::os_name(),
        project: project_a,
        cwd,
        branch: snap.branch.clone(),
        head_commit: head,
        git_remote_url: url,
        task_id: task_id.to_string(),
        moved,
        was_running,
        active_subs,
        resume,
    };
    let name = format!("{}.json", uuid::Uuid::new_v4().simple());
    let tmp = TempFile(std::env::temp_dir().join(format!("openleash-transfer-{name}")));
    let path = tmp.0.clone();
    tokio::task::spawn_blocking(move || xfer::write_bundle(&path, &origin, &snap))
        .await
        .map_err(|e| e.to_string())??;
    upload(l, &tmp.0, &name).await?;
    drop(tmp);
    let host = l.host.name.clone();
    let Some(c) = l.conn.as_mut() else {
        return Ok(SendView {
            status: "pending".into(),
            message: format!("waiting in the imports folder on {host}; it opens there the next time OpenLeash starts"),
            remote: None,
        });
    };
    let args = json!({"name": name, "project": project.filter(|p| !p.trim().is_empty())});
    match c.call("task_import_bundle", args, Duration::from_secs(15 * 60)).await {
        Ok(v) => Ok(SendView {
            status: "imported".into(),
            message: String::new(),
            remote: Some(v),
        }),
        // The other side looked at it and said no: nothing was imported, and
        // it set the file aside. The original stays here, unarchived.
        Err(CallErr::Refused(e)) => Err(format!("{host} couldn't import it: {e}")),
        // The file is there; whether the import ran is unknown. Either way the
        // conversation is on that machine now.
        Err(CallErr::Link(e)) => Ok(SendView {
            status: "unconfirmed".into(),
            message: format!("delivered, but {host} didn't confirm the import ({e}); if it doesn't appear there, it opens the next time OpenLeash starts"),
            remote: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_session_cookie_is_parsed_without_attributes() {
        assert_eq!(
            session_cookie("ol_session_17420=abc; Path=/; HttpOnly; SameSite=Strict").as_deref(),
            Some("ol_session_17420=abc")
        );
        assert_eq!(session_cookie("other=1; Path=/"), None);
        assert_eq!(session_cookie("ol_session_1=; Max-Age=0"), None);
    }

    #[test]
    fn transfer_imports_dir_comes_from_the_marked_line() {
        let out = b"Welcome to Ubuntu\nOPENLEASH_IMPORTS_DIR=/srv/my home/ol data/imports\nbye\n";
        assert_eq!(
            marked_dir(out, Shell::Posix).as_deref(),
            Some("/srv/my home/ol data/imports")
        );
        // Git-for-Windows sh: fine now, the upload goes through the same sh.
        assert_eq!(
            marked_dir(
                b"OPENLEASH_IMPORTS_DIR=/c/Users/Jane Doe/.openleash/imports\r\n",
                Shell::Posix
            )
            .as_deref(),
            Some("/c/Users/Jane Doe/.openleash/imports")
        );
        assert_eq!(
            marked_dir(
                b"OPENLEASH_IMPORTS_DIR=C:\\Users\\Jane Doe\\.openleash\\imports\r\n",
                Shell::Windows
            )
            .as_deref(),
            Some(r"C:\Users\Jane Doe\.openleash\imports")
        );
        // No marker, a relative path, or control characters: no folder.
        assert_eq!(
            marked_dir(b"/home/me/.openleash/imports\n", Shell::Posix),
            None
        );
        assert_eq!(
            marked_dir(b"OPENLEASH_IMPORTS_DIR=imports\n", Shell::Posix),
            None
        );
        assert_eq!(
            marked_dir(b"OPENLEASH_IMPORTS_DIR=/a\x1b[0m/b\n", Shell::Posix),
            None
        );
        assert_eq!(
            marked_dir(b"OPENLEASH_IMPORTS_DIR=/home/me\n", Shell::Windows),
            None
        );
    }

    #[test]
    fn transfer_upload_quotes_the_imports_dir() {
        let dir = "/srv/x'; rm -rf ~; '/my imports";
        let cmd = upload_command(Shell::Posix, dir, "abc.json", 42);
        // The script is quote-free and the paths ride along as `sh -c`
        // arguments, each one a single-quoted word whatever it holds.
        assert_eq!(
            cmd,
            login_command(
                r#"cat > "$1" && n=$(wc -c < "$1" | tr -d ' ') && [ "$n" = "$3" ] && mv -f -- "$1" "$2" || { rm -f -- "$1"; exit 3; }"#,
                &[
                    "/srv/x'; rm -rf ~; '/my imports/abc.json.part",
                    "/srv/x'; rm -rf ~; '/my imports/abc.json",
                    "42"
                ]
            )
        );

        let win = upload_command(
            Shell::Windows,
            r"C:\Users\O'Brien Jane\.openleash\imports",
            "abc.json",
            42,
        );
        let b64 = win
            .strip_prefix("powershell -NoProfile -NonInteractive -EncodedCommand ")
            .unwrap();
        // Nothing for cmd or PowerShell to parse on the command line itself.
        assert!(b64
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)));
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains(r"$d='C:\Users\O''Brien Jane\.openleash\imports';"));
        assert!(script.contains("-ne 42"));
    }

    #[test]
    fn transfer_ps_quote_doubles_every_single_quote() {
        assert_eq!(ps_quote("a'b"), "'a''b'");
        assert_eq!(ps_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(ps_quote(r"C:\x $y"), r"'C:\x $y'");
    }

    /// Run a login command line through a local `sh` (standing in for the
    /// remote login shell), or `None` when there is no `sh` here. Only runs
    /// the command the real path would run: an ssh session's exit status and
    /// stdout are what the code above acts on.
    fn local_sh(
        cmd: &str,
        env: &[(&str, &str)],
        stdin: Option<&std::path::Path>,
    ) -> Option<std::process::Output> {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd).envs(env.iter().copied());
        c.stdin(match stdin {
            Some(p) => std::process::Stdio::from(std::fs::File::open(p).unwrap()),
            None => std::process::Stdio::null(),
        });
        c.output().ok()
    }

    #[test]
    fn transfer_posix_prepare_and_upload_run_in_a_real_sh() {
        let root = std::env::temp_dir().join(format!("ol-xfer-{}", uuid::Uuid::new_v4().simple()));
        let home = root.join("my home/it's here");
        std::fs::create_dir_all(&home).unwrap();
        // `pwd` has to come out as `/…`: with a Windows path (no MSYS mount
        // table) it prints `C:/Users/…`, which `marked_dir` deliberately
        // refuses, so the test would only be testing itself. Step aside then.
        let posix_pwd = std::process::Command::new("sh")
            .arg("-c")
            .arg("pwd")
            .output()
            .is_ok_and(|o| o.status.success() && o.stdout.first() == Some(&b'/'));
        if !posix_pwd {
            eprintln!("no POSIX sh on PATH; skipping");
            std::fs::remove_dir_all(&root).ok();
            return;
        }
        let home_s = home.to_string_lossy().replace('\\', "/");
        let Some(out) = local_sh(&posix_prepare(), &[("OPENLEASH_HOME", &home_s)], None) else {
            eprintln!("no sh here; skipping");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let dir = marked_dir(&out.stdout, Shell::Posix).expect("marked dir");
        assert!(dir.ends_with("/my home/it's here/imports"), "{dir}");
        assert!(home.join("imports").is_dir());

        let src = root.join("bundle.json");
        std::fs::write(&src, b"{\"x\":1}").unwrap();
        let ok = local_sh(
            &upload_command(Shell::Posix, &dir, "a.json", 7),
            &[],
            Some(&src),
        )
        .unwrap();
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
        assert_eq!(
            std::fs::read(home.join("imports/a.json")).unwrap(),
            b"{\"x\":1}"
        );
        assert!(!home.join("imports/a.json.part").exists());

        // A short copy (a dropped connection ends `cat` with a clean EOF) is
        // never renamed into place.
        let bad = local_sh(
            &upload_command(Shell::Posix, &dir, "b.json", 999),
            &[],
            Some(&src),
        )
        .unwrap();
        assert!(!bad.status.success());
        assert!(!home.join("imports/b.json").exists());
        assert!(!home.join("imports/b.json.part").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    fn view(status: &str) -> SendView {
        SendView {
            status: status.into(),
            message: "waiting in the imports folder on Desk".into(),
            remote: (status == "imported").then(|| json!({"project": "/home/me/p"})),
        }
    }

    #[test]
    fn transfer_move_archives_only_a_confirmed_import() {
        let (text, archive) = sent_notice(true, "Desk", &view("imported"));
        assert!(archive);
        assert_eq!(text, "Moved to Desk · /home/me/p");
        for s in ["pending", "unconfirmed"] {
            let (text, archive) = sent_notice(true, "Desk", &view(s));
            assert!(!archive, "{s} must not archive");
            assert!(text.contains("Not archived here"), "{text}");
            assert!(text.contains("archive it yourself"), "{text}");
        }
        for s in ["imported", "pending", "unconfirmed"] {
            assert!(
                !sent_notice(false, "Desk", &view(s)).1,
                "copy never archives"
            );
        }
    }
}
