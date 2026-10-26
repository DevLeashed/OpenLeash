// OpenLeash — Tauri IPC layer. Domain logic lives in `agent/`; this file
// owns app state and exposes commands to the frontend. Live updates flow
// the other way as `ol://event` / `ol://attention` / `ol://usage` events.

// Public so the `openleash-headless` binary (src/headless/main.rs) can drive a
// harness without a GUI. Nothing else about this module changes.
pub mod agent;
mod artificts;
mod connectors;
mod importer;
mod remote;
// Trace export is an agent-side concern, but the module is declared here rather
// than in `agent/mod.rs` because that file is being edited concurrently: a
// `#[path]` line costs nothing and the file still sits beside the other agent
// modules. Moving this declaration into `agent/mod.rs` (and renaming the
// references below to `agent::trace::`) is the tidier home once it is free.
#[path = "agent/trace.rs"]
pub mod trace;
mod updates;
mod visualization;
mod win;

use agent::providers::{self, ModelInfo};
use agent::store::{self, AgentDef, CustomProvider, McpServerCfg, Settings, SkillDef};
use agent::{
    accounts, antigravity, checkpoint, git, router, runner, Checkpoint, Harness, Item, Task,
    TaskSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{Manager, State};
use tokio::sync::{Mutex, RwLock};

type H<'a> = State<'a, Arc<Harness>>;

#[derive(Serialize)]
struct ProviderView {
    id: String,
    name: String,
    icon: String,
    mono: String,
    color: String,
    base_url: String,
    env: String,
    chip: String,
    kind: String,
    custom: bool,
    insist: bool,
    /// Shown in the UI: has a key, accounts, or is a custom endpoint.
    connected: bool,
    has_key: bool,
    key_hint: String,
    /// Last-4-char previews of every key in the pool (empty when not a key pool).
    key_hints: Vec<String>,
    /// Whether this provider rotates across `api_keys`.
    key_pool: bool,
    base_url_override: String,
    enabled: bool,
    local: bool,
    account: Option<providers::AccountProviderInfo>,
}

#[derive(Serialize)]
struct Boot {
    settings: Settings,
    providers: Vec<ProviderView>,
    provider_presets: Vec<providers::ProviderPreset>,
    models: Vec<ModelInfo>,
    tasks: Vec<TaskSummary>,
    /// Pending informational cards, including chats not opened this session.
    user_notices: HashMap<String, Vec<Item>>,
    accounts: Vec<accounts::AccountView>,
    agents: Vec<AgentDef>,
    skills: Vec<SkillDef>,
    shell: String,
    data_dir: String,
    month_spend: f64,
    /// Whether this install may offer the Claude subscription integration at
    /// all. See `claude_allowed`.
    claude_allowed: bool,
}

/// Settings as the UI sees them: API keys and tokens never leave the backend.
fn public_settings(s: &Settings) -> Settings {
    let mut s = s.clone();
    for p in s.providers.values_mut() {
        p.api_key = String::new();
        p.api_keys = vec![];
    }
    for a in s.accounts.iter_mut() {
        a.access_token = String::new();
        a.refresh_token = String::new();
    }
    s.plugins.github.token = String::new();
    agent::plugin_services::redact_secrets(&mut s.plugins);
    // An MCP server's bearer token lives in `headers`, and its `env` is exactly
    // where people put `GITHUB_TOKEN` and friends, so both are credentials.
    // `McpServerCfg`'s own Debug impl already redacts them for this reason; the
    // settings broadcast would otherwise ship them to the webview in cleartext
    // (and the UI's `{...x}` spread would send them straight back).
    //
    // The value is masked rather than the map emptied: the UI round-trips whole
    // MCP rows on an unrelated edit (toggling `enabled`), and an empty map would
    // read as "the user cleared this" and wipe the stored token. `settings_update`
    // restores the real value behind this marker; see `restore_mcp_secrets`.
    for m in s.mcp.iter_mut() {
        for v in m.headers.values_mut().chain(m.env.values_mut()) {
            if !v.is_empty() {
                *v = REDACTED.into();
            }
        }
    }
    s
}

/// Stands in for a secret on its way to the UI. `settings_update` puts the real
/// value back, so a masked entry means "unchanged", not "cleared".
pub const REDACTED: &str = "<redacted>";

/// Put the stored MCP secrets back after the UI round-tripped a masked copy.
/// A header/env entry that still carries the marker is one the user did not
/// touch, so the stored value wins. Anything the UI sent that isn't a marker is
/// a real edit and is kept as sent. Markers only refer to the same stored resource;
/// unmatched markers are removed rather than becoming literal credentials.
fn restore_mcp_secrets(next: &mut Settings, stored: &[McpServerCfg]) {
    fn put_back(fresh: &mut HashMap<String, String>, old: Option<&HashMap<String, String>>) {
        fresh.retain(|k, v| {
            if v != REDACTED {
                return true;
            }
            if let Some(secret) = old.and_then(|values| values.get(k)) {
                *v = secret.clone();
                true
            } else {
                false
            }
        });
    }
    for m in next.mcp.iter_mut() {
        let old = stored
            .iter()
            .find(|o| o.name == m.name && o.url == m.url && o.transport == m.transport);
        put_back(&mut m.headers, old.map(|o| &o.headers));
        put_back(&mut m.env, old.map(|o| &o.env));
    }
}

const CUSTOM_COLORS: [&str; 6] = [
    "#fbbf24", "#f472b6", "#34d399", "#60a5fa", "#c084fc", "#fb923c",
];

fn provider_views(s: &Settings) -> Vec<ProviderView> {
    let view = |id: &str,
                name: &str,
                icon: &str,
                mono: String,
                color: String,
                base: String,
                env: &str,
                chip: String,
                kind: String,
                custom: bool,
                local: bool,
                account: Option<providers::AccountProviderInfo>| {
        let cfg = s.providers.get(id);
        let pool_enabled = cfg.map(|c| c.key_pool).unwrap_or(false);
        let keys: Vec<String> = cfg
            .map(|c| {
                c.api_keys
                    .iter()
                    .filter(|k| !k.is_empty())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let pool_key = keys.first().cloned().unwrap_or_default();
        let key = if pool_enabled {
            pool_key
        } else {
            providers::api_key(s, id)
        };
        let accts = s.accounts.iter().filter(|a| a.kind == id).count();
        // A CLI-backed provider is "connected" only when the binary is actually
        // there: its credential lives in the vendor's own login, so there is no
        // key to check, and claiming it works without the CLI would put models in
        // every picker that fail on the first turn.
        let cli_ready = kind != "cli" || antigravity::locate().is_some();
        let connected =
            custom || !key.is_empty() || accts > 0 || (local && cfg.is_some() && cli_ready);
        let key_hints: Vec<String> = if pool_enabled {
            keys.iter()
                .map(|k| providers::key_hint(k))
                .filter(|h| !h.is_empty())
                .collect()
        } else {
            vec![]
        };
        ProviderView {
            insist: providers::insist(s, id),
            connected,
            id: id.into(),
            name: name.into(),
            icon: icon.into(),
            mono,
            color,
            base_url: base,
            env: env.into(),
            chip,
            kind,
            custom,
            has_key: !key.is_empty() || local || accts > 0 || custom || pool_enabled,
            // Same byte-vs-char trap as providers::key_hint (which this mirrors) —
            // delegate so a non-ASCII pasted key can't slice mid-codepoint and abort.
            key_hint: providers::key_hint(&key),
            key_hints,
            key_pool: pool_enabled,
            base_url_override: cfg.map(|c| c.base_url.clone()).unwrap_or_default(),
            enabled: cfg.map(|c| c.enabled).unwrap_or(true),
            local,
            account,
        }
    };
    let mut v: Vec<ProviderView> = providers::PROVIDERS
        .iter()
        .map(|p| {
            view(
                p.id,
                p.name,
                p.icon,
                p.mono.into(),
                p.color.into(),
                p.base_url.into(),
                p.env,
                p.chip.into(),
                p.kind.into(),
                false,
                p.local,
                p.account,
            )
        })
        .collect();
    for (i, c) in s.custom_providers.iter().enumerate() {
        let mono = c
            .name
            .chars()
            .find(|ch| ch.is_alphanumeric())
            .map(|ch| ch.to_ascii_uppercase().to_string())
            .unwrap_or("?".into());
        let chip = format!(
            "{} · {}",
            if c.kind == "anthropic" {
                "Anthropic-compatible"
            } else {
                "OpenAI-compatible"
            },
            c.base_url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
        );
        v.push(view(
            &c.id,
            &c.name,
            "",
            mono,
            CUSTOM_COLORS[i % CUSTOM_COLORS.len()].into(),
            c.base_url.clone(),
            "",
            chip,
            c.kind.clone(),
            true,
            false,
            None,
        ));
    }
    v
}

fn models(s: &Settings) -> Vec<ModelInfo> {
    // Statics are synced on every settings write; apply s directly too so a
    // stale static can never leak a deleted model back into the list.
    let mut v = providers::all_models();
    for c in &s.custom_models {
        if !v.iter().any(|m| &m.id == c) {
            let mut mi = providers::model_info(c);
            mi.enabled = !s.disabled_models.contains(c);
            if !s.removed_models.contains(c) {
                v.push(mi);
            }
        }
    }
    // Belt and braces: all_models() already filters, but s is authoritative here.
    v.retain(|m| m.custom || !s.removed_models.contains(&m.id));
    for m in v.iter_mut() {
        m.enabled = !s.disabled_models.contains(&m.id);
    }
    v
}

fn sync_model_state(s: &Settings) {
    providers::set_custom_models(&s.model_configs);
    providers::set_model_visibility(&s.disabled_models, &s.removed_models);
}

/// The few fields the account watcher needs from every task. Reading just these
/// avoids `TaskSummary::summary`, which clones every subagent's full report —
/// on a machine with many chats that was hundreds of megabytes copied a minute
/// to check whether anything had auto-paused.
#[derive(serde::Serialize, Clone)]
struct TaskLive {
    id: String,
    paused: Option<agent::Pause>,
    model: String,
    route: String,
    running_models: Vec<String>,
}

async fn live_tasks(h: &Harness) -> Vec<TaskLive> {
    let tasks: Vec<_> = h.tasks.read().await.values().cloned().collect();
    let mut out = vec![];
    for t in tasks {
        let t = t.lock().await;
        out.push(TaskLive {
            id: t.id.clone(),
            paused: t.paused.clone(),
            model: t.model.clone(),
            route: t.route.clone(),
            running_models: t
                .subs
                .iter()
                .filter(|s| s.status == "running")
                .map(|s| s.model.clone())
                .collect(),
        });
    }
    out
}

async fn summaries(h: &Harness) -> Vec<TaskSummary> {
    let tasks: Vec<_> = h.tasks.read().await.values().cloned().collect();
    let mut out = vec![];
    for t in tasks {
        let mut summary = t.lock().await.summary();
        // Reports are fetched only when a subagent panel opens; including every
        // report in every chat summary needlessly inflates remote boot payloads.
        for sub in &mut summary.subs {
            sub.report.clear();
        }
        out.push(summary);
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    out
}

/// Open a file an agent mentioned (`src/x.rs`, `src/x.rs:42`) with the system default app.
/// Relative paths resolve against the chat's working directory.
#[tauri::command]
fn open_path(cwd: String, path: String) -> Result<(), String> {
    let raw = path.trim().trim_matches('`');
    // Drop a trailing `:line` / `:line:col`.
    let mut p = raw;
    while let Some((head, tail)) = p.rsplit_once(':') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && head.len() > 2 {
            p = head;
        } else {
            break;
        }
    }
    if agent::host::is_ssh(&cwd) {
        return open_remote(&cwd, p);
    }
    let full = agent::tools::resolve(&cwd, p);
    if !full.exists() {
        return Err(format!("{p} doesn't exist"));
    }
    // `resolve` returns an absolute path unchanged, and the string that arrives
    // here is a chip in the agent's own markdown — a file:// URL, a UNC share
    // or a `../../..` path rendered as a link the user is invited to click.
    // Launching those hands an arbitrary path to the OS default handler, so
    // the click is confined to the task's working directory.
    if !agent::permissions::inside_public(&full.to_string_lossy(), &cwd) {
        return Err(format!(
            "{p} is outside this task's working directory, so OpenLeash won't open it. \
             Open it yourself from a file manager if that is what you meant."
        ));
    }
    tauri_plugin_opener::open_path(full.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// `open_path` for a remote-SSH chat: there is no local file to hand to the
/// OS, so the file opens in VS Code's Remote-SSH (`vscode://vscode-remote/…`)
/// on the same saved host. Confined to the chat's folder like a local open.
fn open_remote(cwd: &str, p: &str) -> Result<(), String> {
    let root = agent::host::parse_ssh(cwd).ok_or("Invalid SSH project path.")?;
    let t = agent::tools_remote::resolve(cwd, p)?;
    if !agent::host::inside(&root.path, &t.path) {
        return Err(format!(
            "{p} is outside this task's working directory, so OpenLeash won't open it."
        ));
    }
    let target = agent::ssh_session::target_for(&t.host)?;
    let url = format!("vscode://vscode-remote/ssh-remote+{}{}", target, t.path).replace(' ', "%20");
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| {
        format!("{p} is on the SSH host and couldn't be opened in VS Code Remote-SSH ({e}). Open it from a remote editor.")
    })
}

/// What the UI needs to show a picked file before the agent ever sees it.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileRef {
    path: String,
    name: String,
    size: u64,
    is_dir: bool,
    /// True for the image types the models can actually look at, so the
    /// composer can inline those and only hand the model a path for the rest.
    image: bool,
}

/// Read an image file the user attached into a `data:` URL, so it can ride
/// along as a real image block. Only the formats the models can see; anything
/// else stays a path and the agent reads it with its own tools.
#[tauri::command]
fn file_data_url(path: String) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let img = agent::tools::view_image(&p)?;
    Ok(format!("data:{};base64,{}", img.media_type, img.data_b64))
}

/// Read one of OpenLeash's own staged attachment copies back as a `data:` URL.
///
/// The transcript drops a message's picture blobs once a chat grows, to bound
/// what it holds in memory. The picture itself is not lost: the copy the
/// backend staged on disk is still there, and this reads it back so the bubble
/// can show the picture the user sent rather than a chip naming a file.
///
/// Only copies inside OpenLeash's staging folder are read — the path arrives
/// from the transcript, and the transcript is data the agent can put words
/// into, so it is not allowed to name a file anywhere on the disk.
#[tauri::command]
fn staged_image_url(path: String) -> Result<String, String> {
    agent::attachments::staged_image_data_url(&path)
}

/// A picture an agent put in its reply (`![chart](out/chart.png)` or an absolute
/// path to a screenshot it saved), as a `data:` URL the reply shows in place.
///
/// The path is the agent's own markdown, so this is deliberately narrower than
/// `file_data_url` (which only ever sees files the *user* picked) in what it
/// returns: the file must carry an image extension *and* start with that
/// format's magic bytes, so a `secrets.png` that is really text is refused
/// rather than base64-encoded into the page, and `view_image`'s size cap
/// applies. The pixels only go to this app's own window (and paired devices,
/// which are the user), so a real image anywhere on a local disk is fine to
/// show — confining it to the working folder made agents paste whole
/// screenshots as base64 instead, which is what this exists to stop.
///
/// What is refused is *where* it would be read from: a UNC path to another
/// machine (`\\host\share\x.png`, `//host/...`). Opening one makes Windows
/// authenticate to that host with the user's credentials, so an agent-written
/// path must not be able to trigger it. WSL's `\\wsl.localhost\…` is local,
/// and a project that itself lives on a share may show its own files.
fn reply_image_data(cwd: &str, path: &str) -> Result<String, String> {
    let trimmed = path.trim();
    // URL schemes are case-insensitive: `FILE://host/...` is the same share.
    let lowered;
    let raw = if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("file://") {
        lowered = format!("file://{}", &trimmed[7..]);
        lowered.as_str()
    } else {
        trimmed
    };
    // `file://host/share/x.png` names a host: same as a UNC path.
    if let Some(rest) = raw.strip_prefix("file://") {
        if !rest.starts_with('/') && !rest.to_ascii_lowercase().starts_with("localhost/") {
            return Err(format!(
                "{raw} is on another machine's network share, so OpenLeash won't open it"
            ));
        }
    }
    let mut p = raw
        .strip_prefix("file://localhost")
        .or_else(|| raw.strip_prefix("file://"))
        .unwrap_or(raw);
    // `file:///C:/x` → `C:/x`; `file:///home/x` keeps its leading slash.
    if p.len() > 3
        && p.starts_with('/')
        && p.as_bytes()[2] == b':'
        && p.as_bytes()[1].is_ascii_alphabetic()
    {
        p = &p[1..];
    }
    if p.is_empty() {
        return Err("No image path".into());
    }
    let unc = |s: &str| {
        let s = s.replace('/', "\\");
        // `\??\UNC\host\…` is the NT spelling of the same share.
        (s.starts_with("\\\\") || s.starts_with("\\??\\")) && agent::shell::wsl_target(&s).is_none()
    };
    let full = if cwd.trim().is_empty() {
        PathBuf::from(p)
    } else {
        agent::tools::resolve(cwd, p)
    };
    if !full.is_absolute() {
        return Err(format!(
            "{p} is relative and there is no folder to resolve it against"
        ));
    }
    let shown = full.to_string_lossy().to_string();
    let own_share =
        !cwd.trim().is_empty() && unc(cwd) && agent::permissions::inside_public(&shown, cwd);
    if (unc(p) || unc(&shown) || links_to_share(&full, &unc)) && !own_share {
        return Err(format!(
            "{p} is on another machine's network share, so OpenLeash won't open it"
        ));
    }
    if agent::tools::media_type_by_extension(&full).is_none() {
        return Err(format!(
            "{p} isn't a PNG, JPEG, GIF or WebP image, so it can't be shown here"
        ));
    }
    if !full.is_file() {
        return Err(format!("{p} doesn't exist"));
    }
    let mut head = [0u8; 16];
    let n = std::io::Read::read(
        &mut std::fs::File::open(&full).map_err(|e| format!("Cannot read {p}: {e}"))?,
        &mut head,
    )
    .map_err(|e| format!("Cannot read {p}: {e}"))?;
    if agent::tools::sniff_media_type(&head[..n]).is_none() {
        return Err(format!("{p} isn't really an image"));
    }
    let img = agent::tools::display_image(&full)?;
    Ok(format!("data:{};base64,{}", img.media_type, img.data_b64))
}

/// Whether any symlink or junction on the way to `path` points at a network
/// share. The lexical check alone missed `C:\proj\pics -> \\host\share`:
/// `is_file`/`open` would follow it and Windows would authenticate to `host`.
/// Each prefix is inspected with `symlink_metadata`, which never follows the
/// link it looks at, and a link's target is walked the same way (bounded, so
/// a loop can't spin). Anything unreadable on the way counts as unsafe.
fn links_to_share(path: &Path, unc: &dyn Fn(&str) -> bool) -> bool {
    fn walk(path: &Path, unc: &dyn Fn(&str) -> bool, hops: u32) -> bool {
        if hops > 40 {
            return true;
        }
        let mut prefix = PathBuf::new();
        for comp in path.components() {
            prefix.push(comp);
            let Ok(meta) = std::fs::symlink_metadata(&prefix) else {
                // Missing from here on: nothing further to follow.
                return false;
            };
            if meta.file_type().is_symlink() {
                let Ok(target) = std::fs::read_link(&prefix) else {
                    return true;
                };
                let target = if target.is_absolute() {
                    target
                } else {
                    prefix.parent().map(|d| d.join(&target)).unwrap_or(target)
                };
                let rest: PathBuf = path
                    .strip_prefix(&prefix)
                    .map(|r| target.join(r))
                    .unwrap_or(target.clone());
                return unc(&target.to_string_lossy()) || walk(&rest, unc, hops + 1);
            }
        }
        false
    }
    walk(path, unc, 0)
}

/// `reply_image_data` for a remote-SSH chat: a relative or absolute POSIX
/// path (or an `ssh://` URI on the same host) is read over SFTP, with the
/// same rules as a local one: an image extension *and* matching magic bytes,
/// at most `MAX_IMAGE_BYTES`.
async fn reply_image_remote(cwd: &str, path: &str) -> Result<String, String> {
    let raw = path.trim();
    let p = agent::tools_remote::resolve(cwd, raw)?;
    let ext = agent::tools_remote::ext_of(&p.path);
    if agent::tools::media_type_by_ext(&ext).is_none() {
        return Err(format!(
            "{raw} isn't a PNG, JPEG, GIF or WebP image, so it can't be shown here"
        ));
    }
    let st = agent::ssh_session::stat(&p.host, &p.path)
        .await
        .map_err(|_| format!("{raw} doesn't exist"))?;
    if !st.is_file {
        return Err(format!("{raw} doesn't exist"));
    }
    if st.size > agent::tools::MAX_IMAGE_BYTES {
        return Err(format!("{raw} is too large to show"));
    }
    let (bytes, _) = agent::ssh_session::read(&p.host, &p.path, agent::tools::MAX_IMAGE_BYTES + 1)
        .await
        .map_err(|e| format!("Cannot read {raw}: {e}"))?;
    if agent::tools::sniff_media_type(&bytes).is_none() {
        return Err(format!("{raw} isn't really an image"));
    }
    let img = agent::tools::image_from_bytes(&p.path, &ext, bytes)?;
    Ok(format!("data:{};base64,{}", img.media_type, img.data_b64))
}

/// A path in a remote chat's reply that still names *this* machine: a
/// Windows drive path or a `file://` URL (screenshots the app saved locally).
fn names_local_file(path: &str) -> bool {
    let p = path.trim();
    let b = p.as_bytes();
    p.get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("file://"))
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'/' || b[2] == b'\\'))
}

#[tauri::command]
async fn reply_image(cwd: String, path: String) -> Result<String, String> {
    if agent::host::is_ssh(&cwd) {
        if !names_local_file(&path) {
            return reply_image_remote(&cwd, &path).await;
        }
        // A local file named from a remote chat: shown with the local rules
        // and no project folder to resolve against.
        return tauri::async_runtime::spawn_blocking(move || reply_image_data("", &path))
            .await
            .map_err(|e| e.to_string())?;
    }
    tauri::async_runtime::spawn_blocking(move || reply_image_data(&cwd, &path))
        .await
        .map_err(|e| e.to_string())?
}

/// Turn absolute paths from the file dialog / a window drop into attachments.
///
/// The browser `File` API never carries a real path, so anything picked or
/// dropped for the agent to read comes through here instead. Images are
/// reported as `image: true` and the UI re-reads them itself for the model to
/// see; everything else is just a path the agent opens with its own tools.
#[tauri::command]
fn files_attach(paths: Vec<String>, cwd: Option<String>) -> Result<Vec<FileRef>, String> {
    // A remote (SSH) chat's agent reads files on the remote host, so a local
    // path handed to it would name the wrong file (or nothing). Images are
    // inlined by the UI and still fine; anything else is refused, clearly.
    let remote = cwd.as_deref().is_some_and(agent::host::is_ssh);
    let mut out = vec![];
    for raw in paths {
        let p = PathBuf::from(&raw);
        if remote && agent::tools::media_type_by_extension(&p).is_none() {
            return Err("This chat's project is on an SSH host, so its agent can't open files from this computer. Copy the file to the remote host and mention its path there (images can still be attached).".into());
        }
        // A path is a promise about the filesystem, so check it before the
        // agent is told about it. One bad path shouldn't cost you the rest of
        // the drop though, so it's skipped rather than fatal.
        let Ok(md) = std::fs::metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.display().to_string());
        out.push(FileRef {
            image: agent::tools::media_type_by_extension(&p).is_some(),
            path: p.display().to_string(),
            name,
            size: md.len(),
            is_dir: false,
        });
    }
    Ok(out)
}

#[derive(Serialize)]
struct RemoteFileRef {
    path: String,
    name: String,
    size: u64,
    is_dir: bool,
    image: bool,
}

/// Stage one file a remote device uploaded into the task-scoped temp folder the
/// rest of the attachment flow already uses.
///
/// `id` is an existing chat id, **or** a project folder path when the new-task
/// composer has no chat yet: a phone picks its files before the first message
/// creates one, so the project path is all there is to scope the staging folder
/// by. It names a directory component only — nothing is ever read from it.
#[tauri::command]
async fn remote_stage_upload(
    h: H<'_>,
    id: String,
    filename: String,
    bytes: Vec<u8>,
) -> Result<RemoteFileRef, String> {
    use std::path::Path;
    if filename.trim().is_empty()
        || filename.len() > 240
        || filename.as_bytes().iter().any(|b| matches!(b, b'/' | b'\\' | b'\r' | b'\n'))
    {
        return Err("Invalid upload file name".into());
    }
    if bytes.len() > agent::attachments::MAX_FILE_BYTES as usize {
        return Err("File is larger than the 64 MiB upload limit".into());
    }
    if id.trim().is_empty() {
        return Err("Pick a project folder first".into());
    }
    // A chat id names a task; anything else is the project folder the new-task
    // composer is scoped to. Either way staging lands in that project's folder.
    let (project, scope) = match h.task_ref(&id).await {
        Ok(task) => {
            let guard = task.lock().await;
            (guard.project.clone(), id.clone())
        }
        Err(_) => (id.clone(), id.clone()),
    };
    let batch = format!("remote-uploads/{}", agent::new_id());
    let staged = agent::attachments::stage(
        &project,
        &scope,
        &batch,
        &[agent::attachments::Attachment {
            name: filename.clone(),
            bytes,
        }],
        false,
    )
    .await?;
    let path = staged
        .local_paths
        .first()
        .ok_or("Upload could not be staged")?;
    let size = staged.sizes.first().copied().unwrap_or(0);
    Ok(RemoteFileRef {
        path: path.clone(),
        name: filename.clone(),
        size,
        is_dir: false,
        image: agent::tools::media_type_by_extension(Path::new(&filename)).is_some(),
    })
}

/// Name of the opt-in file that switches the Claude subscription integration on.
/// It sits next to the running executable, so the decision belongs to one install
/// rather than to the user's settings (which travel with `OPENLEASH_HOME`) or to
/// the source tree.
const ALLOW_CLAUDE: &str = "ALLOW_CLAUDE";

/// Is the Claude subscription integration enabled for this install?
///
/// The integration is hidden — no provider, no accounts, no model picker entry —
/// unless a file named `ALLOW_CLAUDE` exists beside the executable. This is an
/// opt-in, not a security boundary: the bundled provider is static, so anyone
/// with the binary can wire it back up themselves. It only decides whether the
/// app offers the thing at all.
fn claude_allowed() -> bool {
    claude_allowed_with(std::env::current_exe().ok().as_ref())
}

/// Testable core of [`claude_allowed`], taking the executable path explicitly so
/// a test never depends on where the test binary itself happens to live.
fn claude_allowed_with(exe: Option<&PathBuf>) -> bool {
    let Some(dir) = exe.and_then(|p| p.parent()) else {
        // No executable path (unusual, and true of some test runners): stay
        // closed rather than guess at a directory the user never marked.
        return false;
    };
    claude_allowed_in(dir)
}

/// Whether `dir` carries the opt-in marker. Only the file's existence matters:
/// its contents are never read, so whatever the user drops there works, even if
/// a text editor left it with a BOM or a newline.
fn claude_allowed_in(dir: &Path) -> bool {
    dir.join(ALLOW_CLAUDE).is_file()
}

/// The refusal shared by every path that can add an account, so none of them can
/// drift from the rule. The UI never offers the integration without the opt-in,
/// but hiding a control is not enforcement: a second frontend, a replayed IPC
/// call or a UI regression would still reach the commands that call this.
fn account_import_allowed(kind: &str, allow_claude: bool) -> Result<(), String> {
    if kind == "claude" && !allow_claude {
        return Err(CLAUDE_DISABLED.into());
    }
    Ok(())
}

/// What the connect path says when the integration is switched off. Deliberately
/// names the file, because the only person who can see this is the user with an
/// install they can still mark.
const CLAUDE_DISABLED: &str =
    "Claude subscriptions are not enabled for this install. Create a file named ALLOW_CLAUDE next to the OpenLeash executable and restart.";

/// Apply the trace-export endpoint that is currently in settings.
///
/// The switch is `Settings::trace_endpoint`, saved through `settings_update`
/// like any other setting; this is the step that starts, moves or stops the
/// exporter, and the only one that can fail. An endpoint that is not an
/// `http(s)` URL is refused with a message naming the problem, export stays
/// off, and the same message comes back in `Status::error` for the screen that
/// shows the field.
///
/// Deliberately takes no endpoint argument: where spans go is whatever went
/// through settings and the on-disk file, not an address a webview can hand the
/// exporter at call time. This is a destination for data, and one path to it is
/// enough.
#[tauri::command]
async fn trace_configure(h: H<'_>) -> Result<trace::Status, String> {
    let endpoint = h.settings.read().await.trace_endpoint.clone();
    trace::configure(&endpoint)?;
    Ok(trace::status())
}

#[tauri::command]
async fn app_boot(h: H<'_>) -> Result<Boot, String> {
    let s = h.settings.read().await.clone();
    Ok(Boot {
        accounts: h.accts.views(&s.accounts),
        agents: store::all_agents(&s, &s.project),
        skills: store::all_skills(&s, &s.project),
        providers: provider_views(&s),
        provider_presets: providers::PRESETS.to_vec(),
        models: models(&s),
        month_spend: s.month_spend(),
        settings: public_settings(&s),
        tasks: summaries(&h).await,
        user_notices: h.user_notices().await,
        shell: agent::shell::shell().name.into(),
        data_dir: store::data_dir().to_string_lossy().into(),
        claude_allowed: claude_allowed(),
    })
}

#[tauri::command]
async fn route_dry_run(route: store::Route, situation: agent::routerules::Situation) -> Result<agent::routerules::Decision, String> {
    Ok(router::dry_run(&route, &situation))
}

#[tauri::command]
async fn route_situation(h: H<'_>, id: String, agent: Option<String>) -> Result<agent::routerules::Situation, String> {
    router::situation_for_task(h.inner(), &id, agent.as_deref()).await
}

#[tauri::command]
async fn route_log(h: H<'_>, id: String) -> Result<Vec<router::RouteEvent>, String> {
    let t = h.task(&id).await?;
    let log = t.lock().await.route_log.clone();
    Ok(log)
}

#[tauri::command]
async fn tasks_list(h: H<'_>) -> Result<Vec<TaskSummary>, String> {
    Ok(summaries(&h).await)
}

#[tauri::command]
async fn task_get(h: H<'_>, id: String) -> Result<Value, String> {
    let t = h.task(&id).await?;
    let t = t.lock().await;
    Ok(
        json!({"summary": t.summary(), "items": t.items, "sub_items": t.sub_items, "bg": h.bg.list(&id)}),
    )
}

/// A timeline page: the items to show plus the cursor that asks for the page
/// before them. `cursor: None` means there is nothing older left to fetch.
pub struct TimelinePage<'a> {
    pub items: &'a [Item],
    pub cursor: Option<String>,
    pub total: usize,
}

/// One page of a timeline, anchored on a stable item id rather than an index.
///
/// Index-based paging is wrong here, and the reason is live traffic:
/// `upsert_item` appends to `items` while `remove_item`/`itemgone` take rows out
/// of it, and all of that happens *while* a page is in flight. An index cursor
/// written against one version of the array points somewhere else entirely by the
/// time the next page is built, so the older page then either repeats rows the
/// user has already read or skips past the ones in between. An item id names the
/// same row whatever happened to the array.
///
/// `cursor` is the id of the oldest item in the returned slice, so a caller hands
/// back what it just received and walks backwards one page at a time.
pub fn timeline_page<'a>(items: &'a [Item], cursor: Option<&str>, limit: usize) -> TimelinePage<'a> {
    let limit = limit.clamp(1, 200);
    // Everything strictly before the cursor item. A cursor naming an item a live
    // `itemgone` already took back resolves to the end — the newest page — because
    // wrapping around would resend history the caller is already holding.
    let end = match cursor {
        Some(c) => items.iter().position(|i| i.id == c).unwrap_or(items.len()),
        None => items.len(),
    };
    // Nothing older left to give, so the walk is over: a `None` cursor. Returning
    // `Some` here — as pointing at the item one index before `end` does when
    // `end == 0` — would hand back a cursor whose page is the same empty slice
    // forever, and the scroll-up loader would spin asking for it.
    if end == 0 {
        return TimelinePage { items: &[], cursor: None, total: items.len() };
    }
    let start = end.saturating_sub(limit);
    TimelinePage { items: &items[start..end], cursor: items.get(start).map(|i| i.id.clone()), total: items.len() }
}

/// Open only the newest slice of the main timeline. Subagent model histories
/// are not needed to show a chat and can dominate a large task file.
///
/// Paged on a stable *item id*, never on an index: live events insert and remove
/// items while the page is in flight (`upsert_item` appends, `remove_item` and
/// `itemgone` retain), so an index cursor drifts and the older page then either
/// repeats rows the user has already read or silently skips past some. `cursor`
/// is the oldest id this page returned; passing it back walks backwards safely.
#[tauri::command]
async fn task_open(h: H<'_>, id: String) -> Result<Value, String> {
    const PAGE: usize = 40;
    let task = h
        .tasks
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no task {id}"))?;
    // Opening a chat is the user looking at it, so whatever was waiting to be
    // looked at has been — and the mark is cleared here, in the backend, rather
    // than in the window that drew it, so a second client on the same chat (a
    // phone driving this app over remote control) agrees about it. Called before
    // the lock below is taken, so the rule can live in one testable place
    // (`Harness::seen`); the summary built after this carries the cleared mark.
    h.seen(&id).await;
    let t = task.lock().await;
    let page = timeline_page(&t.items, None, PAGE);
    let mut summary = t.summary();
    // Reports are fetched only when a subagent panel opens (`sub_report`), so
    // they are dead weight in an open payload.
    for sub in &mut summary.subs {
        sub.report.clear();
    }
    Ok(json!({
        "summary": summary,
        "items": page.items,
        "bg": h.bg.list(&id),
        "cursor": page.cursor,
        "total": page.total
    }))
}

/// One bounded older timeline page, anchored on the item id the previous page
/// started at. `cursor: null` means the whole history has been sent.
#[tauri::command]
async fn task_history(
    h: H<'_>,
    id: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let task = h
        .tasks
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no task {id}"))?;
    let t = task.lock().await;
    let page = timeline_page(&t.items, cursor.as_deref(), limit.unwrap_or(100));
    Ok(json!({"items": page.items, "cursor": page.cursor, "total": page.total}))
}

/// User-facing artifict workspace IPC. The project root always comes from the
/// task record, never from an invoke argument, so a caller cannot use these
/// commands to select an arbitrary filesystem location.
async fn artifict_project(h: &Harness, task_id: &str) -> Result<String, String> {
    let task = h.task(task_id).await?;
    let project = task.lock().await.project.clone();
    if project.trim().is_empty() {
        return Err("This task has no project folder for artificts.".into());
    }
    Ok(project)
}

#[tauri::command]
async fn artifict_list(
    h: H<'_>,
    task_id: String,
) -> Result<Vec<artificts::ArtifictSummary>, String> {
    let project = artifict_project(&h, &task_id).await?;
    artificts::list(&project)
}

#[tauri::command]
async fn artifict_get(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    artificts::get(&project, &artifict_id)
}

#[tauri::command]
async fn artifict_create(
    h: H<'_>,
    task_id: String,
    input: artificts::ArtifictInput,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict = artificts::create(&project, input)?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

#[tauri::command]
async fn artifict_revise(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    parent_version_id: String,
    input: artificts::ArtifictInput,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict = artificts::revise(&project, &artifict_id, &parent_version_id, input)?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

/// Bring an earlier version back as the newest one, labelled as the user's.
#[tauri::command]
async fn artifict_restore(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    parent_version_id: String,
    from_version_id: String,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict =
        artificts::restore(&project, &artifict_id, &parent_version_id, &from_version_id)?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

/// A content-only revision the user wrote in the studio (canvas or source).
#[tauri::command]
async fn artifict_edit(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    parent_version_id: String,
    content: String,
    note: String,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict =
        artificts::edit_by_user(&project, &artifict_id, &parent_version_id, &content, &note)?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

/// Save one version as a standalone file at the path the user picked in the
/// native save dialog (like `task_export`). The artifict still comes from the
/// task's own project; only the destination is an argument.
#[tauri::command]
async fn artifict_export(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    version_id: String,
    path: String,
    theme: Option<serde_json::Map<String, Value>>,
) -> Result<String, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict = artificts::get(&project, &artifict_id)?;
    let bytes = artificts::export_document(&artifict, &version_id, theme.as_ref())?;
    artificts::write_export(&path, &bytes)
}

#[tauri::command]
async fn artifict_annotate(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    version_id: String,
    text: String,
    anchor: Option<Value>,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    artificts::annotate(&project, &artifict_id, &version_id, &text, anchor)
}

/// Remove one private, unsent comment. Refused once it has been sent, because
/// that comment is part of the request the agent was handed.
#[tauri::command]
async fn artifict_annotate_remove(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    annotation_id: String,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict = artificts::remove_annotation(&project, &artifict_id, &annotation_id)?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

#[tauri::command]
async fn artifict_feedback_submit(
    h: H<'_>,
    task_id: String,
    artifict_id: String,
    version_id: String,
    text: String,
    annotation_ids: Vec<String>,
    interactive_state: Option<Value>,
) -> Result<artificts::Artifict, String> {
    submit_artifict_feedback(
        h.inner(),
        &task_id,
        &artifict_id,
        &version_id,
        text,
        annotation_ids,
        interactive_state,
    )
    .await
}

/// The body of `artifict_feedback_submit`, separate from the Tauri wrapper so a
/// test can drive the whole handoff without an app handle.
async fn submit_artifict_feedback(
    h: &Arc<Harness>,
    task_id: &str,
    artifict_id: &str,
    version_id: &str,
    text: String,
    annotation_ids: Vec<String>,
    interactive_state: Option<Value>,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(h, task_id).await?;
    let artifict = artificts::feedback_submit(
        &project,
        artifict_id,
        version_id,
        &text,
        annotation_ids,
        interactive_state,
    )?;
    h.emit_artifict(task_id, &artifict);
    // Submitting is the handoff, so it is delivered here rather than waiting for
    // the agent to happen to call `artifict_feedback_list` on a later turn. On
    // the same path the composer uses: a new run when the chat is idle, steering
    // when it is working, so pause, plan mode, hooks and permissions apply.
    if let Err(error) = deliver_artifict_feedback(h, task_id, &artifict).await {
        // The handoff did not happen, so the record is withdrawn rather than
        // left claiming the agent is waiting on it; the UI then shows the true
        // state and the user can retry without duplicating an unanswered request.
        if let Ok(()) = artificts::feedback_withdraw(
            &project,
            artifict
                .feedback
                .last()
                .map(|f| f.id.as_str())
                .unwrap_or(""),
        ) {
            let restored = artificts::get(&project, &artifict.id)?;
            h.emit_artifict(task_id, &restored);
        }
        return Err(error);
    }
    Ok(artifict)
}

/// Hand one submitted artifict request to the agent as an ordinary chat turn:
/// a new run when the chat is idle, steering when it is already working — the
/// same path the composer uses, so pause, plan mode and permissions apply as
/// they do to any typed message.
async fn deliver_artifict_feedback(
    h: &Arc<Harness>,
    task_id: &str,
    artifict: &artificts::Artifict,
) -> Result<(), String> {
    // `feedback_submit` appended exactly one record, and `validate_artifict`
    // rejects a file whose submitted feedback has no annotations/state to stand
    // on, so the last entry is the submission that just happened.
    let Some(feedback) = artifict.feedback.last() else {
        return Err("The submitted feedback could not be delivered.".into());
    };
    let number = artifict
        .versions
        .iter()
        .position(|v| v.id == feedback.version_id)
        .map(|i| i + 1)
        .unwrap_or(0);
    let first_line = feedback
        .text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("see annotations and captured state");
    let display = format!(
        "Artifict feedback · {} · v{number} — {}",
        artifict.title,
        first_line.chars().take(120).collect::<String>()
    );
    runner::send_as(
        h.clone(),
        task_id.to_string(),
        display,
        artificts::feedback_prompt(artifict, feedback),
    )
    .await
}

#[tauri::command]
async fn artifict_feedback_list(
    h: H<'_>,
    task_id: String,
    artifict_id: Option<String>,
) -> Result<Vec<artificts::Feedback>, String> {
    let project = artifict_project(&h, &task_id).await?;
    artificts::feedback_list(&project, artifict_id.as_deref())
}

#[tauri::command]
async fn artifict_feedback_respond(
    h: H<'_>,
    task_id: String,
    feedback_id: String,
    decision: String,
    response: String,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let artifict = artificts::feedback_respond(&project, &feedback_id, &decision, &response)?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

/// Explicit user input to the main agent's existing browser; never an agent tool.
#[tauri::command]
async fn browser_panel(
    h: H<'_>,
    task_id: String,
    action: agent::browser_panel::PanelAction,
) -> Result<agent::browser_panel::PanelFrame, String> {
    browser_panel_for(&h, &task_id, action).await
}

async fn browser_panel_for(
    h: &Harness,
    task_id: &str,
    action: agent::browser_panel::PanelAction,
) -> Result<agent::browser_panel::PanelFrame, String> {
    h.task(task_id).await?;
    action.validate()?;
    let session = panel_session(h, task_id).await?;
    agent::browser_panel::run(&session, action).await
}

/// The task's main browser session, for the user's own panel actions: the same
/// page and cookies the main agent drives. Refused while the plugin is off.
async fn panel_session(h: &Harness, task_id: &str) -> Result<Arc<agent::browser::Session>, String> {
    let cfg = h.settings.read().await.plugins.browser.clone();
    if !cfg.enabled {
        return Err("The browser plugin is turned off (Settings → Plugins).".into());
    }
    let mut opts = agent::browser::SessionOpts::from_cfg(&cfg);
    let cwd = h.task(task_id).await?.lock().await.cwd_for(None).0;
    if !agent::host::is_ssh(&cwd) {
        opts.project = Some(std::path::PathBuf::from(cwd));
    }
    agent::browser::session_with(&format!("{task_id}:main"), opts).await
}

/// Annotations the user made in the Browser view, sent to the agent as one chat
/// turn with the marked-up screenshot attached. The user's explicit send is the
/// handoff, as with artifict feedback.
#[tauri::command]
async fn browser_annotate(
    h: H<'_>,
    task_id: String,
    annotations: Vec<agent::browser_annotate::Annotation>,
) -> Result<(), String> {
    deliver_browser_annotations(h.inner(), &task_id, annotations).await
}

/// The body of `browser_annotate`, separate from the Tauri wrapper so a test can
/// drive it without an app handle. Everything checkable without the page is
/// checked before a browser is touched.
async fn deliver_browser_annotations(
    h: &Arc<Harness>,
    task_id: &str,
    annotations: Vec<agent::browser_annotate::Annotation>,
) -> Result<(), String> {
    h.task(task_id).await?;
    agent::browser_annotate::validate(&annotations)?;
    let session = panel_session(h, task_id).await?;
    let capture = agent::browser_annotate::capture(&session, &annotations).await?;
    let display = agent::browser_annotate::display(&capture.url, &annotations);
    let prompt = agent::browser_annotate::prompt(&capture.url, &capture.title, &annotations);
    let names = (0..capture.images.len())
        .map(|i| {
            if i == 0 {
                "browser-annotations.png".to_string()
            } else {
                format!("browser-annotation-{i}.png")
            }
        })
        .collect();
    // The composer's own image path: a new run when the chat is idle, steering
    // when it is working, so pause, plan mode and permissions apply as they do
    // to any typed message with a picture.
    runner::send_attachments_as(
        h.clone(),
        task_id.to_string(),
        display,
        prompt,
        capture.images,
        names,
        false,
    )
    .await
}

/// One subagent's own transcript, read when its panel opens. `task_open` sends
/// only the main timeline, so without this a chat reopened after launch shows a
/// subagent with no brief and no work.
#[tauri::command]
async fn sub_transcript(h: H<'_>, id: String, sub: String) -> Result<Vec<agent::Item>, String> {
    h.sub_transcript(&id, &sub).await
}

/// One subagent's report. The live `task` event leaves reports out (they are
/// huge and change rarely), so a subagent's panel asks for its own on open.
#[tauri::command]
async fn sub_report(h: H<'_>, id: String, sub: String) -> Result<String, String> {
    let t = h.task(&id).await?;
    let t = t.lock().await;
    t.subs
        .iter()
        .find(|s| s.id == sub)
        .map(|s| s.report.clone())
        .ok_or_else(|| format!("No subagent with id `{sub}`."))
}

#[derive(Deserialize)]
struct NewTask {
    prompt: String,
    project: String,
    model: String,
    effort: usize,
    #[serde(
        default = "agent::permissions::default_perm",
        deserialize_with = "agent::permissions::deserialize_perm"
    )]
    perm: String,
    plan: bool,
    worktree: bool,
    /// "local" | "worktree" | "docker". Absent from older frontends, which
    /// only knew the `worktree` switch.
    #[serde(default)]
    workspace: Option<String>,
    base_branch: String,
    #[serde(default)]
    assist: String,
    #[serde(default)]
    agents: Option<Vec<String>>,
    #[serde(default)]
    route: Option<String>,
    /// Pasted images as data URLs.
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    image_names: Vec<String>,
    #[serde(default)]
    ultra: bool,
    #[serde(default)]
    ultra_wt: bool,
    /// ULTRATHREAD X: the per-layer ladder. Present and >= 2 layers = X mode.
    #[serde(default)]
    ultra_x: Option<store::UltraX>,
}

/// Startup guard held from task creation through first-message dispatch. Stop/delete
/// cancel its token; Drop releases sends waiting for bootstrap and deletion waiting
/// for final workspace metadata.
struct StartupGuard {
    runtime: Arc<agent::Runtime>,
    cancel: tokio_util::sync::CancellationToken,
}

impl StartupGuard {
    fn new(runtime: Arc<agent::Runtime>) -> Self {
        let cancel = tokio_util::sync::CancellationToken::new();
        *runtime.startup_cancel.lock().unwrap() = Some(cancel.clone());
        Self { runtime, cancel }
    }
}

impl Drop for StartupGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        let mut startup = self.runtime.startup_cancel.lock().unwrap();
        startup.take();
        self.runtime.startup_done.notify_all();
        drop(startup);
        if self.runtime.startup_pending.swap(false, Ordering::SeqCst) {
            self.runtime.startup_ready.notify_waiters();
        }
    }
}

async fn wait_for_task_startup(h: &Harness, task_id: &str) {
    let rt = h.runtime(task_id);
    loop {
        let ready = rt.startup_ready.notified();
        tokio::pin!(ready);
        ready.as_mut().enable();
        if !rt.startup_pending.load(Ordering::SeqCst) {
            return;
        }
        ready.await;
    }
}

/// Prepare the project's workspace after the chat exists, so slow git/worktree
/// operations do not hold the task-create IPC call open. The first prompt is
/// still gated on completion, so agents never run before setup hooks finish.
async fn prepare_task_workspace(
    h: &Arc<Harness>,
    task_id: &str,
    project: &str,
    requested_branch: &str,
    want_worktree: bool,
    want_docker: bool,
    prompt: &str,
) {
    if want_docker {
        return prepare_docker_workspace(h, task_id, project, requested_branch, prompt).await;
    }
    let project_owned = project.to_string();
    let requested_branch = requested_branch.to_string();
    let prompt = prompt.to_string();
    let runtime = h.runtime(task_id);
    let startup_cancel = runtime
        .startup_cancel
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    let cancel_for_blocking = startup_cancel.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        if cancel_for_blocking.is_cancelled() {
            return Err("startup cancelled".to_string());
        }
        let repo = git::metadata(&project_owned);
        let base_branch = if requested_branch.is_empty() {
            repo.as_ref().map(|r| r.branch.clone()).unwrap_or_default()
        } else {
            requested_branch
        };
        if cancel_for_blocking.is_cancelled() {
            return Err("startup cancelled".to_string());
        }
        let worktree_result = if want_worktree && repo.is_some() {
            Some(git::create_worktree(
                &project_owned,
                &base_branch,
                &git::slug(&prompt),
            ))
        } else {
            None
        };
        // Keep a successful worktree result even if cancellation arrived while
        // Git was creating it. The caller must persist the path before deletion
        // decides whether the user asked to retain or remove that worktree.
        Ok((project_owned, repo, base_branch, worktree_result))
    })
    .await;
    let Ok(Ok((project, repo, base_branch, worktree_result))) = prepared else {
        if !startup_cancel.is_cancelled() && runtime.startup_pending.load(Ordering::SeqCst) {
            h.upsert_item(
                task_id,
                Item::new(
                    "notice",
                    "Could not inspect the project folder; using it directly.",
                    json!({"level":"warning"}),
                ),
            )
            .await;
            h.update_task(task_id, |t| t.branch.clear()).await;
            h.save_task_now(task_id).await;
        }
        return;
    };
    let mut cwd = project.clone();
    let mut branch = repo.as_ref().map(|r| r.branch.clone()).unwrap_or_default();
    let mut base_commit = repo.as_ref().and_then(|r| r.head.clone());
    let mut is_worktree = false;
    let mut notice = None;
    match worktree_result {
        Some(Ok((dir, br))) => {
            cwd = dir.clone();
            branch = br;
            is_worktree = true;
            // Persist the newly created workspace before honoring cancellation.
            // Deletion waits for startup to finish, then applies the user's choice
            // to this exact worktree instead of leaving an untracked checkout.
            h.update_task(task_id, |t| {
                t.cwd = cwd.clone();
                t.branch = branch.clone();
                t.base_branch = base_branch.clone();
                t.base_commit = None;
                t.worktree = true;
            })
            .await;
            h.save_task_now(task_id).await;
            if startup_cancel.is_cancelled() {
                return;
            }
            let hooks = {
                let s = h.settings.read().await;
                agent::checks::hooks_for(&s, &project)
            };
            let setup_cancel = startup_cancel.child_token();
            let setup =
                agent::checks::setup_worktree(&hooks, &project, &dir, task_id, &setup_cancel).await;
            if setup_cancel.is_cancelled() || !runtime.startup_pending.load(Ordering::SeqCst) {
                return;
            }
            let failed: Vec<String> = setup
                .hooks
                .iter()
                .filter(|o| !o.ok)
                .map(|o| format!("`{}`: {}", o.command, o.output))
                .collect();
            let mut issues = Vec::new();
            if let Some(error) = setup.copy_error {
                issues.push(format!("included-file copy failed: {error}"));
            }
            if !failed.is_empty() {
                issues.push(format!("setup hook failed: {}", failed.join("; ")));
            }
            if !issues.is_empty() {
                notice = Some(format!(
                    "The worktree was created, but setup did not finish cleanly: {}",
                    issues.join("; ")
                ));
            } else if setup.copied > 0 {
                notice = Some(format!(
                    "Copied {} file{} from `.worktreeinclude` into the new worktree.",
                    setup.copied,
                    if setup.copied == 1 { "" } else { "s" }
                ));
            }
            let dir_for_head = dir.clone();
            base_commit = tokio::task::spawn_blocking(move || git::head(&dir_for_head))
                .await
                .ok()
                .flatten();
        }
        Some(Err(e)) => {
            notice = Some(format!(
                "Couldn't create a worktree ({e}). Working directly in the project."
            ))
        }
        None if want_worktree && repo.is_none() => {
            notice =
                Some("This folder isn't a git repository, so the task runs directly in it.".into())
        }
        None => {}
    }
    if startup_cancel.is_cancelled() || !runtime.startup_pending.load(Ordering::SeqCst) {
        return;
    }
    if let Some(n) = notice {
        h.upsert_item(task_id, Item::new("notice", n, json!({"level": "info"})))
            .await;
    }
    h.update_task(task_id, |t| {
        t.cwd = cwd;
        t.branch = branch;
        t.base_branch = base_branch;
        t.base_commit = base_commit;
        t.worktree = is_worktree;
    })
    .await;
    h.save_task_now(task_id).await;
}

/// Which workspace a new task asked for. Older frontends send only the
/// `worktree` switch.
fn workspace_mode(requested: Option<&str>, worktree: bool) -> &'static str {
    match requested {
        Some("docker") => "docker",
        Some("worktree") => "worktree",
        Some("local") => "local",
        _ if worktree => "worktree",
        _ => "local",
    }
}

/// Docker mode builds an image from the project's own files (`RUN` steps are
/// arbitrary code) and runs the agent's commands in it, so it is offered only
/// for folders the user trusts, and only when the project opted in.
fn check_docker_allowed(project: &str) -> Result<(), String> {
    agent::host::refuse(project, "Docker workspaces are")?;
    if !agent::docker::has_config(project) {
        return Err(format!(
            "This project has no {}/Dockerfile, so it can't run in docker.",
            agent::docker::CONFIG_DIR
        ));
    }
    if !agent::trust::trusted_now(project) {
        return Err("Trust this folder before running it in docker: the image is built from the project's own files.".into());
    }
    agent::docker::load_config(project).map(|_| ())
}

/// Remove a docker workspace directory, refusing anything that is not one.
fn remove_docker_workspace(dir: &str) -> Result<(), String> {
    let root = agent::docker::workspaces_dir();
    let p = std::path::Path::new(dir);
    if p.parent()
        .map(|x| agent::trust::norm_path(&x.to_string_lossy()))
        != Some(agent::trust::norm_path(&root.to_string_lossy()))
    {
        return Err(format!("{dir} is not a docker workspace"));
    }
    match std::fs::remove_dir_all(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("couldn't remove {dir}: {e}")),
    }
}

/// Create a docker task's workspace and container. Unlike a failed worktree,
/// a failed docker setup does not fall back to running in the project: the
/// user picked docker to keep commands off the host, so the first message is
/// held back instead (the startup guard preserves it for a retry).
async fn prepare_docker_workspace(
    h: &Arc<Harness>,
    task_id: &str,
    project: &str,
    requested_branch: &str,
    prompt: &str,
) {
    let runtime = h.runtime(task_id);
    let startup_cancel = runtime
        .startup_cancel
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    let mut item = Item::new(
        "notice",
        "Setting up the docker workspace…",
        json!({"level": "info", "working": true}),
    );
    h.upsert_item(task_id, item.clone()).await;
    let (p, tid, base, slug) = (
        project.to_string(),
        task_id.to_string(),
        requested_branch.to_string(),
        git::slug(prompt),
    );
    let created = tokio::task::spawn_blocking(move || {
        let repo = git::metadata(&p);
        let base = if base.is_empty() {
            repo.as_ref().map(|r| r.branch.clone()).unwrap_or_default()
        } else {
            base
        };
        let cfg = agent::docker::load_config(&p)?;
        let (dir, branch) =
            agent::docker::create_workspace(&p, &tid, &base, &slug, repo.is_some())?;
        let head = git::head(&dir);
        Ok::<_, String>((dir, branch, base, head, cfg.guest_root()))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    let (dir, branch, base, head, guest_root) = match created {
        Ok(v) => v,
        Err(e) => {
            item.text = format!(
                "Couldn't create the docker workspace: {e}\n\nNothing was run in the project folder."
            );
            item.data = json!({"level": "error"});
            h.upsert_item(task_id, item).await;
            startup_cancel.cancel();
            return;
        }
    };
    let env = agent::docker::DockerEnv {
        container: agent::docker::container_name(task_id),
        project: project.to_string(),
        host_root: dir.clone(),
        guest_root,
    };
    agent::docker::register(task_id, &env);
    h.update_task(task_id, |t| {
        t.cwd = dir.clone();
        t.branch = branch.clone();
        t.base_branch = base.clone();
        t.base_commit = head.clone();
        t.worktree = false;
        t.docker = Some(env.clone());
    })
    .await;
    h.save_task_now(task_id).await;
    if startup_cancel.is_cancelled() {
        return;
    }
    let mut issues = vec![];
    if !branch.is_empty() {
        // Same carry-over as a worktree: `.worktreeinclude` files and the
        // user's post-setup hook, on the host side of the mount.
        let hooks = {
            let s = h.settings.read().await;
            agent::checks::hooks_for(&s, project)
        };
        let setup_cancel = startup_cancel.child_token();
        let setup =
            agent::checks::setup_worktree(&hooks, project, &dir, task_id, &setup_cancel).await;
        if let Some(e) = setup.copy_error {
            issues.push(format!("included-file copy failed: {e}"));
        }
        for o in setup.hooks.iter().filter(|o| !o.ok) {
            issues.push(format!("setup hook `{}` failed: {}", o.command, o.output));
        }
    }
    if startup_cancel.is_cancelled() {
        return;
    }
    let (h2, tid, iid) = (h.clone(), task_id.to_string(), item.id.clone());
    let progress = move |line: &str| {
        let (h2, tid, iid, line) = (
            h2.clone(),
            tid.clone(),
            iid.clone(),
            line.chars().take(240).collect::<String>(),
        );
        tokio::spawn(async move {
            h2.patch_item(&tid, &iid, |i| {
                i.text = format!("Setting up the docker workspace…\n{line}")
            })
            .await;
            h2.emit_container(&tid).await;
        });
    };
    // A task's first container start still builds the image if none exists, but
    // when automatic rebuilds are off it reuses an existing one rather than
    // rebuilding over a changed build context — the same rule every other start
    // path follows.
    let auto_rebuild = h.settings.read().await.docker_auto_rebuild;
    let started =
        agent::docker::ensure_running_with_rebuild(task_id, &env, auto_rebuild, &progress).await;
    h.emit_container(task_id).await;
    let mut text = match &started {
        Ok(()) => format!(
            "Running in docker container `{}`. The workspace is {}{}.",
            env.container,
            dir,
            if branch.is_empty() {
                String::new()
            } else {
                format!(" on branch `{branch}`")
            }
        ),
        Err(e) => format!("The workspace is ready, but the container couldn't start: {e}"),
    };
    if !issues.is_empty() {
        text.push_str(&format!(
            "\n\nSetup did not finish cleanly: {}",
            issues.join("; ")
        ));
    }
    let level = if started.is_ok() && issues.is_empty() {
        "info"
    } else {
        "warning"
    };
    h.patch_item(task_id, &item.id, |i| {
        i.text = text;
        i.data = json!({"level": level});
    })
    .await;
    if started.is_err() {
        // Keep commands off the host: hold the first message for a retry.
        startup_cancel.cancel();
    }
}

/// Stop docker workspaces that have been idle, and on launch any container a
/// previous run left behind. Does nothing at all (no `docker` process) while
/// the user has no docker chats.
async fn docker_reaper(h: Arc<Harness>) {
    use agent::docker::{self, State};
    if !docker::all_live().is_empty() {
        // Nothing is running at launch, so whatever is still up is idle.
        if let Ok(list) = docker::list_containers().await {
            for c in list.iter().filter(|c| c.state == "running") {
                let _ = docker::docker(&["stop", "-t", "5", &c.name]).await;
            }
        }
        for (id, _) in docker::all_live() {
            h.emit_container(&id).await;
        }
    }
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let default_idle = h.settings.read().await.docker_idle_minutes;
        for (id, live) in docker::all_live() {
            if live.state != State::Running {
                continue;
            }
            let Some(env) = live.env else { continue };
            let minutes = docker::load_config(&env.project)
                .ok()
                .and_then(|c| c.idle_minutes)
                .unwrap_or(default_idle);
            if minutes == 0 {
                continue;
            }
            let idle = live
                .last_active
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(minutes * 60));
            if !idle {
                continue;
            }
            // A working agent or a background job (a dev server) is activity
            // even when no new command has started for a while.
            let working = match h.task(&id).await {
                Ok(t) => t.lock().await.status == "running",
                Err(_) => false,
            };
            if working || h.bg.list(&id).iter().any(|b| b.running) {
                docker::touch(&id);
                continue;
            }
            if docker::stop(&id, &env).await.is_ok() {
                h.emit_container(&id).await;
            }
        }
    }
}

async fn task_docker(h: &Harness, id: &str) -> Result<agent::docker::DockerEnv, String> {
    let t = h.task(id).await?;
    let d = t.lock().await.docker.clone();
    d.ok_or_else(|| "This chat doesn't run in docker.".into())
}

/// What the new-task screen needs to offer docker mode. The engine is only
/// asked when `check_engine` is set (the user picked docker), so merely opening
/// a project never runs a docker command.
#[tauri::command]
async fn docker_project_info(project: String, check_engine: bool) -> Value {
    let has_config = agent::docker::has_config(&project);
    let config_error = if has_config {
        agent::docker::load_config(&project).err()
    } else {
        None
    };
    let engine = if check_engine && has_config {
        Some(match agent::docker::available().await {
            Ok(v) => json!({"ok": true, "version": v}),
            Err(e) => json!({"ok": false, "error": e}),
        })
    } else {
        None
    };
    json!({
        "has_config": has_config,
        "trusted": agent::trust::trusted_now(&project),
        "config_error": config_error,
        "engine": engine,
    })
}

/// Creates the starter `.openleash_docker/Dockerfile` after the user agreed to it.
#[tauri::command]
async fn docker_add_template(project: String) -> Result<(), String> {
    agent::docker::add_template(&project)
}

#[tauri::command]
async fn container_start(h: H<'_>, id: String) -> Result<(), String> {
    let env = task_docker(&h, &id).await?;
    let auto_rebuild = h.settings.read().await.docker_auto_rebuild;
    let (h2, id2) = (h.inner().clone(), id.clone());
    let progress = move |_: &str| {
        let (h2, id2) = (h2.clone(), id2.clone());
        tokio::spawn(async move { h2.emit_container(&id2).await });
    };
    let r = agent::docker::ensure_running_with_rebuild(&id, &env, auto_rebuild, &progress).await;
    h.emit_container(&id).await;
    r
}

#[tauri::command]
async fn container_stop(h: H<'_>, id: String) -> Result<(), String> {
    let env = task_docker(&h, &id).await?;
    // Background jobs die with the container; say so in their exit rather
    // than letting them read as crashed.
    h.bg.kill_task_with(&id, "the container was stopped");
    let r = agent::docker::stop(&id, &env).await;
    h.emit_container(&id).await;
    r
}

/// Remove just this chat's container. Its workspace files are left untouched;
/// a later message recreates the container from the project's Docker image.
#[tauri::command]
async fn container_remove(h: H<'_>, id: String) -> Result<(), String> {
    let env = task_docker(&h, &id).await?;
    h.bg.kill_task_with(&id, "the container was removed");
    agent::docker::remove(&id, &env).await?;
    h.emit_container(&id).await;
    Ok(())
}

/// Point a stopped chat at an existing folder. Workspace files are not copied
/// or deleted; only the chat's execution location changes. Works for any chat,
/// not just docker ones: it is the local counterpart of the folder choice a
/// transfer to another machine makes.
#[tauri::command]
async fn task_change_folder(h: H<'_>, id: String, folder: String) -> Result<(), String> {
    change_folder(h.inner(), &id, &folder).await
}

async fn change_folder(h: &Harness, id: &str, folder: &str) -> Result<(), String> {
    let folder = folder.trim().to_string();
    if !std::path::Path::new(&folder).is_dir() {
        return Err("Choose an existing folder.".into());
    }
    let task = h.task(id).await?;
    let docker_env = {
        let t = task.lock().await;
        if t.status == "running" || t.status == "waiting" || t.busy > 0 {
            return Err("Stop the chat before changing its work folder.".into());
        }
        t.docker.clone()
    };
    if let Some(env) = docker_env {
        h.bg.kill_task_with(id, "the chat's workspace was changed");
        agent::docker::remove(id, &env).await?;
    }
    {
        let mut t = task.lock().await;
        let old = t.cwd.clone();
        t.cwd = folder.clone();
        t.project = folder.clone();
        t.branch = if git::is_repo(&folder) {
            git::current_branch(&folder)
        } else {
            String::new()
        };
        t.base_branch.clear();
        t.base_commit = None;
        t.worktree = false;
        t.ultra_wt = false;
        t.docker = None;
        // The frozen prompt names the old folder and carries its project
        // instructions, memory and project-scoped tools; left in place the
        // agent would keep believing it was in `old`. Cleared, it is rebuilt
        // from the new folder on the next request (`told` too, so the
        // announcements are made again against it).
        t.system.clear();
        t.mcp_tools.clear();
        t.plugins = Default::default();
        t.told.clear();
        // Worker checkouts belonged to the old repository.
        for s in t.subs.iter_mut() {
            s.cwd.clear();
            s.branch.clear();
        }
        t.stop_note = Some(format!("<system-reminder>This chat's working folder was changed from {old} to {folder}. Its previous files were left where they were; only the conversation and future work continue in the new folder. Re-read any file before editing it, and check `git status` and the branch there: it is a different checkout from the one you were working in.</system-reminder>"));
    }
    h.update_task(id, |_| {}).await;
    h.save_task(id).await;
    Ok(())
}

/// Rebuild a project's image. Progress arrives as `ol://docker-build` events;
/// containers on the old image are recreated the next time they are used.
#[tauri::command]
async fn docker_rebuild(h: H<'_>, project: String, no_cache: bool) -> Result<String, String> {
    check_docker_allowed(&project)?;
    let bus = h.bus.clone();
    let p2 = project.clone();
    let progress = move |line: &str| bus("ol://docker-build", json!({"project": p2, "line": line}));
    let r = agent::docker::rebuild(&project, no_cache, &progress).await;
    (h.bus)(
        "ol://docker-build",
        json!({"project": project, "done": true, "error": r.as_ref().err()}),
    );
    for (id, live) in agent::docker::all_live() {
        if live.env.is_some_and(|e| {
            agent::trust::norm_path(&e.project) == agent::trust::norm_path(&project)
        }) {
            h.emit_container(&id).await;
        }
    }
    r
}

// ───────────────────────────── sandboxes ─────────────────────────────
//
// The agent's own computers (`agent/sandbox.rs`). Every command ends by
// telling the UI which sandbox changed (`ol://sandbox`), so the manager and the
// chat header refresh without polling.

fn sandbox_changed(h: &Harness, id: &str) {
    (h.bus)("ol://sandbox", json!({ "id": id }));
}

#[tauri::command]
async fn sandbox_list() -> Value {
    agent::sandbox::views().await
}

/// Install the desktop image now (or pull the user's own), so a sandbox never
/// has to do it mid-start. Progress arrives as `ol://sandbox-build` events.
#[tauri::command]
async fn sandbox_prepare(h: H<'_>) -> Result<(), String> {
    let bus = h.bus.clone();
    let progress = move |line: &str| bus("ol://sandbox-build", json!({"line": line}));
    let r = agent::sandbox::ensure_image(&progress).await;
    (h.bus)(
        "ol://sandbox-build",
        json!({"done": true, "error": r.as_ref().err()}),
    );
    sandbox_changed(&h, "");
    r
}

#[tauri::command]
async fn sandbox_image_set(h: H<'_>, image: String) -> Result<(), String> {
    agent::sandbox::set_custom_image(&image)?;
    sandbox_changed(&h, "");
    Ok(())
}

#[tauri::command]
async fn sandbox_create(
    h: H<'_>,
    name: String,
    width: Option<u32>,
    height: Option<u32>,
    memory: Option<String>,
    cpus: Option<f64>,
) -> Result<Value, String> {
    let sb = agent::sandbox::create(agent::sandbox::NewSandbox {
        name,
        width,
        height,
        memory: memory.unwrap_or_default(),
        cpus: cpus.unwrap_or(0.0),
    })?;
    sandbox_changed(&h, &sb.id);
    agent::sandbox::view_of(&sb.id).await
}

#[tauri::command]
async fn sandbox_update(
    h: H<'_>,
    id: String,
    patch: agent::sandbox::Patch,
) -> Result<Value, String> {
    agent::sandbox::update(&id, patch)?;
    agent::sandbox::apply_limits(&id).await;
    sandbox_changed(&h, &id);
    agent::sandbox::view_of(&id).await
}

/// Start a sandbox. The first ever start builds the desktop image; its output
/// arrives as `ol://sandbox-build` events.
#[tauri::command]
async fn sandbox_start(h: H<'_>, id: String) -> Result<Value, String> {
    let bus = h.bus.clone();
    let id2 = id.clone();
    let progress = move |line: &str| bus("ol://sandbox-build", json!({"id": id2, "line": line}));
    sandbox_changed(&h, &id);
    let r = agent::sandbox::start(&id, &progress).await;
    (h.bus)(
        "ol://sandbox-build",
        json!({"id": id, "done": true, "error": r.as_ref().err()}),
    );
    sandbox_changed(&h, &id);
    r?;
    agent::sandbox::view_of(&id).await
}

#[tauri::command]
async fn sandbox_stop(h: H<'_>, id: String) -> Result<Value, String> {
    let r = agent::sandbox::stop(&id).await;
    sandbox_changed(&h, &id);
    r?;
    agent::sandbox::view_of(&id).await
}

#[tauri::command]
async fn sandbox_reset(
    h: H<'_>,
    id: String,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<Value, String> {
    let size = match (width, height) {
        (Some(w), Some(ht)) => Some((w, ht)),
        _ => None,
    };
    agent::sandbox::reset(&id, size).await?;
    sandbox_changed(&h, &id);
    agent::sandbox::view_of(&id).await
}

#[tauri::command]
async fn sandbox_delete(h: H<'_>, id: String) -> Result<(), String> {
    agent::sandbox::delete(&id).await?;
    sandbox_changed(&h, &id);
    Ok(())
}

#[tauri::command]
async fn sandbox_clone(h: H<'_>, id: String, name: String) -> Result<Value, String> {
    let sb = agent::sandbox::clone_sandbox(&id, &name).await?;
    sandbox_changed(&h, &sb.id);
    agent::sandbox::view_of(&sb.id).await
}

#[tauri::command]
async fn sandbox_snapshot(h: H<'_>, id: String, name: String) -> Result<Value, String> {
    agent::sandbox::snapshot(&id, &name).await?;
    sandbox_changed(&h, &id);
    agent::sandbox::view_of(&id).await
}

#[tauri::command]
async fn sandbox_restore(h: H<'_>, id: String, snapshot: String) -> Result<Value, String> {
    agent::sandbox::restore(&id, &snapshot).await?;
    sandbox_changed(&h, &id);
    agent::sandbox::view_of(&id).await
}

#[tauri::command]
async fn sandbox_snapshot_delete(h: H<'_>, id: String, snapshot: String) -> Result<Value, String> {
    agent::sandbox::delete_snapshot(&id, &snapshot).await?;
    sandbox_changed(&h, &id);
    agent::sandbox::view_of(&id).await
}

/// The user takes (or hands back) the mouse and keyboard. While they have
/// them, the agent's actions on this sandbox are refused.
#[tauri::command]
async fn sandbox_control(h: H<'_>, id: String, user: bool) -> Result<(), String> {
    agent::sandbox::load(&id)?;
    agent::sandbox::set_user_control(&id, user);
    sandbox_changed(&h, &id);
    Ok(())
}

/// A small picture of the screen for cards and lists, as a data URL.
#[tauri::command]
async fn sandbox_thumb(id: String) -> Result<Option<String>, String> {
    use base64::Engine;
    agent::sandbox::load(&id)?;
    match agent::sandbox::thumbnail(&id).await {
        Ok(b) => Ok(Some(format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b)
        ))),
        // Not running, or not drawn yet: the card shows its placeholder.
        Err(_) => Ok(None),
    }
}

#[tauri::command]
async fn sandbox_files(id: String) -> Result<Vec<Value>, String> {
    agent::sandbox::list_files(&id)
}

#[tauri::command]
async fn sandbox_files_import(h: H<'_>, id: String, paths: Vec<String>) -> Result<usize, String> {
    let n = agent::sandbox::import_paths(&id, &paths)?;
    sandbox_changed(&h, &id);
    Ok(n)
}

#[tauri::command]
async fn sandbox_files_export(id: String, rel: String, dest: String) -> Result<(), String> {
    agent::sandbox::export_path(&id, &rel, &dest)
}

#[tauri::command]
async fn sandbox_files_delete(h: H<'_>, id: String, rel: String) -> Result<(), String> {
    agent::sandbox::delete_path(&id, &rel)?;
    sandbox_changed(&h, &id);
    Ok(())
}

/// Attach a chat to a sandbox (or back to the host with `None`). Takes effect
/// on the chat's next tool call, so it can be swapped mid-conversation.
#[tauri::command]
async fn task_set_sandbox(
    h: H<'_>,
    task_id: String,
    sandbox: Option<String>,
) -> Result<(), String> {
    if let Some(s) = &sandbox {
        agent::sandbox::load(s)?;
    }
    h.update_task(&task_id, |t| t.sandbox = sandbox.clone()).await;
    h.save_task(&task_id).await;
    if let Some(s) = &sandbox {
        sandbox_changed(&h, s);
    }
    Ok(())
}

/// Save what a sandbox is showing, or a file from it, as an artifict in the
/// chat's project: the user's own "keep this" button on the live view.
#[tauri::command]
async fn sandbox_publish(
    h: H<'_>,
    task_id: String,
    id: String,
    title: String,
    file: Option<String>,
) -> Result<artificts::Artifict, String> {
    let project = artifict_project(&h, &task_id).await?;
    let sb = agent::sandbox::load(&id)?;
    let (kind, content) = agent::sandbox::publish_content(&id, file.as_deref()).await?;
    let title = if title.trim().is_empty() {
        format!("{} screen", sb.name)
    } else {
        title
    };
    let artifict = artificts::create(
        &project,
        artificts::ArtifictInput {
            title: title.chars().take(200).collect(),
            kind,
            content,
            decisions: vec![],
            constraints: vec![format!("Made in sandbox \"{}\"", sb.name)],
            code_refs: vec![],
        },
    )?;
    h.emit_artifict(&task_id, &artifict);
    Ok(artifict)
}

/// Bring a docker chat's branch into the project repository.
#[tauri::command]
async fn docker_push_branch(h: H<'_>, id: String) -> Result<String, String> {
    let env = task_docker(&h, &id).await?;
    let branch = h.task(&id).await?.lock().await.branch.clone();
    tokio::task::spawn_blocking(move || {
        git::push_workspace_branch(&env.host_root, &env.project, &branch)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Every filesystem path a chat owns: its own checkout or docker workspace, plus
/// each of its ultrathread workers' own checkouts. Normalised, so they can be
/// compared with what a directory scan turns up.
///
/// One rule, in one place, because two decisions read it and must not drift: the
/// orphan scan (a path not in this set, and under one of our roots, is a leftover)
/// and `workspace_prune` (a path in this set belongs to a chat and must be
/// deleted with that chat, not out from under it). Worker worktrees used to be
/// in neither, so a running ultrathread wave's checkouts were listed as "Left
/// behind" and offered a Delete button while agents were working in them.
fn owned_paths(t: &agent::Task) -> Vec<String> {
    let mut paths: Vec<String> = vec![agent::trust::norm_path(&t.cwd)];
    if let Some(d) = &t.docker {
        paths.push(agent::trust::norm_path(&d.host_root));
    }
    paths.extend(
        t.subs
            .iter()
            .filter(|s| !s.cwd.is_empty())
            .map(|s| agent::trust::norm_path(&s.cwd)),
    );
    paths
}

/// The worker worktrees a chat owns, one row per agent (a coordinator's nested
/// workers share its checkout, so one path can appear more than once).
fn worker_worktrees(t: &agent::Task) -> Vec<Value> {
    t.subs
        .iter()
        .filter(|s| !s.cwd.is_empty())
        .map(|s| {
            json!({
                "task_id": t.id, "title": t.title, "sub_id": s.id,
                "branch": s.branch, "path": s.cwd, "status": s.status,
                "exists": std::path::Path::new(&s.cwd).is_dir(),
            })
        })
        .collect()
}

/// Every worktree and docker workspace OpenLeash made, with orphans (left
/// behind by a crash or a chat deleted while keeping its files). Docker is
/// only asked about containers when `scan_docker` is set or a docker chat
/// exists, so the manager never wakes an engine the user doesn't use.
#[tauri::command]
async fn workspaces_list(h: H<'_>, scan_docker: bool) -> Result<Value, String> {
    let tasks: Vec<agent::TaskRef> = h.tasks.read().await.values().cloned().collect();
    let mut worktrees = vec![];
    let mut dockers = vec![];
    let mut workers = vec![];
    let mut known: std::collections::HashSet<String> = Default::default();
    for t in tasks {
        let t = t.lock().await;
        let entry = json!({
            "task_id": t.id, "title": t.title, "project": t.project, "path": t.cwd,
            "branch": t.branch, "status": t.status, "archived": t.archived,
            "exists": std::path::Path::new(&t.cwd).is_dir(),
        });
        known.extend(owned_paths(&t));
        workers.extend(worker_worktrees(&t));
        if let Some(d) = &t.docker {
            let mut e = entry;
            e["container"] = json!(d.container);
            e["state"] = json!(agent::docker::live(&t.id)
                .map(|l| l.state)
                .unwrap_or_default());
            dockers.push(e);
        } else if t.worktree {
            worktrees.push(entry);
        }
    }
    let orphans_in = |dir: PathBuf| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.path().to_string_lossy().to_string())
                    .filter(|p| !known.contains(&agent::trust::norm_path(p)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let orphan_worktrees = orphans_in(store::data_dir().join("worktrees"));
    let orphan_workspaces = orphans_in(agent::docker::workspaces_dir());
    let (containers, docker_error) = if scan_docker || !dockers.is_empty() {
        match agent::docker::list_containers().await {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    let task_ids: std::collections::HashSet<String> =
        h.tasks.read().await.keys().cloned().collect();
    let orphan_containers: Vec<_> = containers
        .as_ref()
        .map(|c| {
            c.iter()
                .filter(|c| !task_ids.contains(&c.task_id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "worktrees": worktrees,
        "worker_worktrees": workers,
        "docker": dockers,
        "containers": containers,
        "docker_error": docker_error,
        "orphan_worktrees": orphan_worktrees,
        "orphan_workspaces": orphan_workspaces,
        "orphan_containers": orphan_containers,
        "idle_minutes": h.settings.read().await.docker_idle_minutes,
    }))
}

/// Remove something the manager listed as orphaned. Each kind is checked
/// against what it claims to be, so this can't be pointed at an arbitrary path
/// or a container OpenLeash didn't create.
#[tauri::command]
async fn workspace_prune(h: H<'_>, kind: String, target: String) -> Result<(), String> {
    let in_use = |p: &str| {
        let n = agent::trust::norm_path(p);
        let h = h.inner().clone();
        async move {
            for t in h.tasks.read().await.values() {
                let t = t.lock().await;
                // `owned_paths` is the same rule the orphan scan uses, so a path
                // the manager lists as a chat's own is exactly a path it refuses
                // to prune — the two cannot disagree about a worker's checkout.
                if owned_paths(&t).iter().any(|owned| *owned == n) {
                    return true;
                }
            }
            false
        }
    };
    match kind.as_str() {
        "container" => {
            let list = agent::docker::list_containers().await?;
            let c = list
                .iter()
                .find(|c| c.name == target)
                .ok_or("No OpenLeash container by that name.")?;
            if h.tasks.read().await.contains_key(&c.task_id) {
                return Err("That container belongs to a chat; delete the chat instead.".into());
            }
            agent::docker::docker(&["rm", "-f", &c.name])
                .await
                .map(|_| ())
        }
        "workspace" => {
            if in_use(&target).await {
                return Err("That workspace belongs to a chat; delete the chat instead.".into());
            }
            remove_docker_workspace(&target)
        }
        "worktree" => {
            let root = store::data_dir().join("worktrees");
            let p = std::path::Path::new(&target);
            if p.parent()
                .map(|x| agent::trust::norm_path(&x.to_string_lossy()))
                != Some(agent::trust::norm_path(&root.to_string_lossy()))
            {
                return Err(format!("{target} is not an OpenLeash worktree"));
            }
            if in_use(&target).await {
                return Err("That worktree belongs to a chat; delete the chat instead.".into());
            }
            let target2 = target.clone();
            tokio::task::spawn_blocking(move || {
                // Unregister it from its repository when we can find it (the
                // `.git` file names it); otherwise just delete the folder and
                // let `git worktree prune` in that repo tidy up later.
                let main = std::fs::read_to_string(std::path::Path::new(&target2).join(".git"))
                    .ok()
                    .and_then(|s| s.strip_prefix("gitdir:").map(|g| g.trim().to_string()))
                    .and_then(|g| {
                        std::path::Path::new(&g)
                            .ancestors()
                            .nth(3)
                            .map(|p| p.to_string_lossy().to_string())
                    });
                if let Some(main) = main {
                    if git::remove_worktree(&main, &target2).is_ok() {
                        return Ok(());
                    }
                }
                match std::fs::remove_dir_all(&target2) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(format!("couldn't remove {target2}: {e}")),
                }
            })
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Err(format!("unknown kind {kind}")),
    }
}

struct NewTaskStartup {
    task_id: String,
    startup_item: String,
    project: String,
    base_branch: String,
    worktree: bool,
    docker: bool,
    prompt: String,
    /// What the transcript shows for the first message, when it differs from
    /// what the model is sent. See `create_task`.
    display: Option<String>,
    images: Vec<String>,
    files: Vec<String>,
    image_names: Vec<String>,
    guard: StartupGuard,
    require_worktree: bool,
}

/// Dispatches the first prompt after workspace preparation. Hook checks and image
/// preparation can involve slow local work; the chat should be open and usable
/// while those checks finish.
fn start_new_task_prompt(h: Arc<Harness>, startup: NewTaskStartup) {
    let NewTaskStartup {
        task_id,
        startup_item,
        project,
        base_branch,
        worktree,
        docker,
        prompt,
        display,
        images,
        files,
        image_names,
        guard: startup,
        require_worktree,
    } = startup;
    tauri::async_runtime::spawn(async move {
        prepare_task_workspace(
            &h,
            &task_id,
            &project,
            &base_branch,
            worktree,
            docker,
            &prompt,
        )
        .await;
        if require_worktree && !strict_task_worktree_ready(&h, &task_id).await {
            h.patch_item(&task_id, &startup_item, |item| {
                item.text = format!("Required worktree unavailable; no message was sent and the original checkout was not used.

{prompt}");
                item.data["working"] = json!(false);
                item.data["level"] = json!("error");
            }).await;
            h.update_task(&task_id, |t| {
                t.status = "failed".into();
                t.step = "Required worktree unavailable".into();
            }).await;
            h.save_task_now(&task_id).await;
            return;
        }
        if startup.cancel.is_cancelled() || !startup.runtime.startup_pending.load(Ordering::SeqCst)
        {
            if startup.runtime.startup_pending.load(Ordering::SeqCst) {
                h.patch_item(&task_id, &startup_item, |item| {
                    item.text = format!(
                        "Startup stopped before the first message was sent. The message is preserved below for you to retry:\n\n{prompt}"
                    );
                    item.data["working"] = json!(false);
                    item.data["level"] = json!("warning");
                })
                .await;
            } else {
                drop(startup);
                h.remove_item(&task_id, &startup_item).await;
            }
            return;
        }
        h.patch_item(&task_id, &startup_item, |item| {
            item.text = "Starting…".into()
        })
        .await;
        if startup.runtime.startup_pending.load(Ordering::SeqCst) && startup.cancel.is_cancelled() {
            h.patch_item(&task_id, &startup_item, |item| {
                item.text = format!(
                    "Startup stopped before the first message was sent. The message is preserved below for you to retry:\n\n{prompt}"
                );
                item.data["working"] = json!(false);
                item.data["level"] = json!("warning");
            })
            .await;
            return;
        }
        let plain = images.is_empty() && files.is_empty();
        let result = match display {
            Some(display) if plain => {
                runner::send_as(h.clone(), task_id.clone(), display, prompt.clone()).await
            }
            _ if plain => dispatch(h.clone(), task_id.clone(), prompt.clone()).await,
            _ => {
                runner::send_attachments(
                    h.clone(),
                    task_id.clone(),
                    prompt.clone(),
                    images,
                    files,
                    image_names,
                    false,
                )
                .await
            }
        };
        drop(startup);
        h.remove_item(&task_id, &startup_item).await;
        if let Err(error) = result {
            h.upsert_item(
                &task_id,
                Item::new(
                    "notice",
                    format!("Couldn't send the first message: {error}\\n\\nYour message is preserved here so you can retry:\\n{prompt}"),
                    json!({"level": "error"}),
                ),
            ).await;
        }
    });
}

#[tauri::command]
async fn task_create(h: H<'_>, req: NewTask) -> Result<TaskSummary, String> {
    create_task(h.inner().clone(), req, None).await
}

/// Best-of-N race: one prompt, N worktree chats (see `agent::race`). Every
/// attempt goes through `create_task`, so it gets the same permission mode,
/// workspace setup, budget gate and usage caps as a chat made by hand.
#[tauri::command]
async fn race_create(h: H<'_>, req: agent::race::NewRace) -> Result<Value, String> {
    let h = h.inner().clone();
    if req.uses_docker() {
        check_docker_allowed(&req.project)?;
    }
    let (race, tasks) = agent::race::create(&req, |a| {
        let h = h.clone();
        let spec = req.clone();
        let docker = req.uses_docker();
        async move {
            let task = NewTask {
                prompt: a.prompt,
                project: spec.project,
                model: a.model,
                effort: a.effort,
                perm: spec.perm,
                plan: spec.plan,
                worktree: !docker,
                workspace: Some(if docker { "docker" } else { "worktree" }.into()),
                base_branch: spec.base_branch,
                assist: spec.assist,
                agents: spec.agents,
                // Each attempt's own model gets its default route: the
                // composer's route belongs to the composer's model, which an
                // attempt may not use.
                route: None,
                images: spec.images,
                files: spec.files,
                image_names: spec.image_names,
                ultra: false,
                ultra_wt: false,
                ultra_x: None,
            };
            let mut summary = create_task_external(h.clone(), task, None).await?;
            let id = summary.id.clone();
            // `titled` keeps the numbering: the agent's `set_title` is refused
            // on a chat with a fixed name, and "Race 2/3" is how the group reads.
            h.update_task(&id, |t| {
                t.title = a.title.clone();
                t.titled = true;
            })
            .await;
            h.save_task(&id).await;
            summary.title = a.title;
            summary.titled = true;
            Ok((id, summary))
        }
    })
    .await?;
    agent::race::emit(&h, &race);
    agent::race::watch(h.clone(), race.id.clone());
    Ok(json!({"race": race, "tasks": tasks}))
}

#[tauri::command]
async fn race_list(h: H<'_>) -> Result<Vec<agent::race::Race>, String> {
    let h = h.inner().clone();
    tokio::task::spawn_blocking(move || agent::race::refresh(&h))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn race_detail(h: H<'_>, id: String) -> Result<Value, String> {
    agent::race::detail(h.inner(), &id).await
}

#[tauri::command]
async fn race_judge(
    h: H<'_>,
    id: String,
    model: Option<String>,
    force: Option<bool>,
) -> Result<agent::race::Race, String> {
    agent::race::judge(h.inner(), &id, model, force.unwrap_or(false)).await
}

#[tauri::command]
async fn race_pick(
    h: H<'_>,
    id: String,
    task_id: Option<String>,
) -> Result<agent::race::Race, String> {
    let race = agent::race::pick(&id, task_id)?;
    agent::race::emit(h.inner(), &race);
    Ok(race)
}

#[tauri::command]
async fn race_merge(h: H<'_>, id: String) -> Result<Value, String> {
    let (race, message) = agent::race::merge(h.inner(), &id).await?;
    Ok(json!({"race": race, "message": message}))
}

/// Archive a race with its attempt chats, or bring it back. Archiving moves only
/// the attempts that are not already archived and records which, so a restore
/// brings back exactly those: a chat the user put away on its own stays away.
#[tauri::command]
async fn race_archive(h: H<'_>, id: String, archived: bool) -> Result<agent::race::Race, String> {
    archive_race(h.inner(), &id, archived).await
}

async fn archive_race(h: &Harness, id: &str, archived: bool) -> Result<agent::race::Race, String> {
    let id = id.to_string();
    let race = agent::race::get(&id)?;
    if archived == race.archived {
        return Ok(race);
    }
    let chats = if archived {
        let mut live = vec![];
        for a in &race.attempts {
            if let Ok(t) = h.task(&a.task_id).await {
                if !t.lock().await.archived {
                    live.push(a.task_id.clone());
                }
            }
        }
        archive_chats(h, &live).await?;
        live
    } else {
        for tid in &race.archived_chats {
            if h.task(tid).await.is_ok() {
                apply_patch(h, tid, json!({"archived": false})).await?;
            }
        }
        vec![]
    };
    let race = agent::race::set_archived(&id, archived, chats)?;
    agent::race::emit(h, &race);
    Ok(race)
}

#[tauri::command]
async fn race_forget(h: H<'_>, id: String) -> Result<(), String> {
    agent::race::remove(&id)?;
    agent::race::emit_removed(h.inner(), &id);
    Ok(())
}

/// Delete an archived race for good: the race record, its ranking, and the
/// attempt chats that are still put away with it. The chats go through the
/// same purge a chat's own "Delete forever" uses, so their transcripts, drafts,
/// attachments and worktrees are handled exactly as they are there.
///
/// Only an archived race can be deleted this way. A race that is still in the
/// sidebar may have attempts that are running, and dropping those would kill
/// work the user never asked to stop; archiving is the user's own "put this
/// away" before a permanent delete.
///
/// A chat the user restored on its own is left alone: it is a plain chat in the
/// sidebar now, with its own history, and the race is only the grouping that
/// was put away. It is named in the result so the UI can say so.
#[tauri::command]
async fn race_delete(h: H<'_>, id: String, remove_worktree: bool) -> Result<Value, String> {
    let h = h.inner().clone();
    // Purging runs git and file IO, which must not block the async runtime.
    tokio::task::spawn_blocking(move || delete_race(&h, &id, remove_worktree))
        .await
        .map_err(|e| e.to_string())?
}

fn delete_race(h: &Harness, id: &str, remove_worktree: bool) -> Result<Value, String> {
    let race = agent::race::get(id)?;
    if !race.archived {
        return Err("Archive the race before deleting it for good.".into());
    }
    let mut deleted = vec![];
    let mut kept = vec![];
    let mut failed = vec![];
    // The worktrees that stay on disk are counted before anything is purged: a
    // kept worktree is a folder the user has to clean up by hand, so the toast
    // has to name it.
    let mut kept_worktrees = 0usize;
    for a in &race.attempts {
        let Some(task) = h.tasks.blocking_read().get(&a.task_id).cloned() else {
            // A chat the user already deleted is not a failure: there is nothing
            // left to purge.
            continue;
        };
        let (archived, worktree, workers) = {
            let t = task.blocking_lock();
            (
                t.archived,
                t.worktree,
                t.subs.iter().filter(|s| !s.cwd.is_empty()).count(),
            )
        };
        if !archived {
            // Restored by hand: it is a chat of its own again, not part of the
            // archive the user is deleting.
            kept.push(a.task_id.clone());
            kept_worktrees += worktree as usize + workers;
            continue;
        }
        match purge_task(h, &a.task_id, remove_worktree) {
            Ok(()) => deleted.push(a.task_id.clone()),
            Err(e) => failed.push(format!("{}: {e}", a.task_id)),
        }
    }
    if !failed.is_empty() {
        // The race record stays, so the user can see what is left and retry.
        return Err(format!(
            "Couldn't delete every chat of this race: {}",
            failed.join("; ")
        ));
    }
    // The record goes last: a failure above must leave the race in place, so
    // its remaining chats can still be reached.
    agent::race::remove(id)?;
    agent::race::emit_removed(h, id);
    Ok(json!({
        "id": id,
        "deleted_chats": deleted,
        "kept_chats": kept,
        "kept_worktrees": if remove_worktree { 0 } else { kept_worktrees },
    }))
}

/// Create a chat and start its first prompt. `display`, when given, is what the
/// transcript shows (and the title is drawn from) while `req.prompt` is what the
/// model receives: a connector frames a chat message with where it came from,
/// and the user should see their own words, not the framing.
#[derive(Default)]
pub(crate) struct TaskCreationPolicy {
    pub require_worktree: bool,
    pub burner: Option<agent::burner_policy::BurnPolicy>,
}

fn strict_worktree_ready(required: bool, is_worktree: bool, cwd: &str, project: &str) -> bool {
    !required || (is_worktree && agent::trust::norm_path(cwd) != agent::trust::norm_path(project))
}

async fn strict_task_worktree_ready(h: &Harness, id: &str) -> bool {
    let Ok(task) = h.task(id).await else { return false; };
    let task = task.lock().await;
    strict_worktree_ready(true, task.worktree, &task.cwd, &task.project)
}

async fn create_task(
    h: Arc<Harness>,
    req: NewTask,
    display: Option<String>,
) -> Result<TaskSummary, String> {
    create_task_with_policy(
        h,
        req,
        display,
        TaskCreationPolicy::default(),
        CreateTaskOptions::Interactive,
    )
    .await
}

/// A task started by something other than the composer — a chat connector, a
/// race attempt, the quota burner.
///
/// It is created exactly like an interactive one, but it does not move the
/// new-task default: the model a chat that arrived from Slack happens to run on
/// is not the model the user wants their next new task to start on.
pub(crate) async fn create_task_external(
    h: Arc<Harness>,
    req: NewTask,
    display: Option<String>,
) -> Result<TaskSummary, String> {
    create_task_with_policy(
        h,
        req,
        display,
        TaskCreationPolicy::default(),
        CreateTaskOptions::External,
    )
    .await
}

pub(crate) async fn create_task_with_policy(
    h: Arc<Harness>,
    req: NewTask,
    display: Option<String>,
    policy: TaskCreationPolicy,
    options: CreateTaskOptions,
) -> Result<TaskSummary, String> {
    create_task_with_options(h, req, display, options, policy).await
}

#[derive(Clone, Copy)]
pub(crate) enum CreateTaskOptions {
    /// A new task the user started from the composer. The model it runs on is
    /// remembered as the default for the next new task.
    Interactive,
    /// A task started by something other than the composer: a chat connector,
    /// a race attempt, the quota burner. It still records the project and the
    /// recently-used models, but it does *not* move the new-task default — the
    /// model a background chat happens to run on is not the model the user
    /// wants their next new task to start on.
    External,
    /// A scheduled run. Writes nothing at all.
    Background,
}

async fn remember_task_creation(
    h: &Harness,
    project: &str,
    model: &str,
    effort: usize,
    options: CreateTaskOptions,
) {
    // Background creation must never write interactive choices, even briefly:
    // snapshot/restore would erase a user choice made while creation awaited.
    if matches!(options, CreateTaskOptions::Background) {
        return;
    }
    let mut s = h.settings.write().await;
    s.recent_models.retain(|m| m != model);
    s.recent_models.insert(0, model.to_string());
    s.recent_models.truncate(5);
    // The model the last new task actually ran on is the model the next new
    // task should start on. `settings.model` is what the new-task screen reads
    // on boot, and it used to only ever hold the build's default — so the pick
    // the user made was remembered in `recent_models` and nowhere else, and
    // the next new task came up on a model the user had never chosen. Writing
    // it here is what makes "the model I used last" the one that is
    // preselected, rather than something to pick again every time.
    //
    // Only an interactive task may move it: a chat that arrived from Slack, or
    // one attempt of a race, is not the user choosing a model for their next
    // task, and letting it write this field made the new-task screen come up
    // on a model nobody picked there.
    if matches!(options, CreateTaskOptions::Interactive) && !model.trim().is_empty() {
        s.model = model.to_string();
        // The reasoning level goes with the model. It is recorded here rather
        // than on every slider step because the slider is a control, not a
        // choice: one write per arrow key would let two quick presses race
        // each other's answers. The level of the task that actually ran is the
        // one the user meant, and it is what comes back on the next new task.
        s.effort = effort.min(4);
    }
    if !s.projects.iter().any(|p| p == project) {
        s.projects.insert(0, project.to_string());
    }
    s.project = project.to_string();
    // Coalesce writes rather than serializing every provider credential while
    // the settings lock is held on the interactive Send path.
    drop(s);
    h.save_settings_debounced().await;
}

async fn create_task_with_options(
    h: Arc<Harness>,
    req: NewTask,
    display: Option<String>,
    options: CreateTaskOptions,
    policy: TaskCreationPolicy,
) -> Result<TaskSummary, String> {
    let project = req.project.trim().to_string();
    if project.is_empty() || !project_dir_exists(&project).await {
        return Err("Pick a project folder first.".into());
    }
    // Refused before the chat exists: a chat made with no model could never
    // answer its first message, and the draft should survive for a retry.
    if req.model.trim().is_empty() {
        return Err(agent::NO_MODEL_PICKED.into());
    }
    let mode = workspace_mode(req.workspace.as_deref(), req.worktree);
    if agent::host::is_ssh(&project) && mode != "local" {
        return Err(format!(
            "{} not yet supported for SSH projects.",
            if mode == "docker" {
                "Docker workspaces are"
            } else {
                "Worktrees are"
            }
        ));
    }
    if mode == "docker" {
        check_docker_allowed(&project)?;
    }
    let id = agent::new_id();
    let title: String = display
        .as_deref()
        .unwrap_or(&req.prompt)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("New task")
        .trim()
        .chars()
        .take(70)
        .collect();
    // Git metadata inspection and optional worktree creation take place after
    // the chat is visible; the IPC create call only has to persist its shell.
    let cwd = project.clone();
    let branch = req.base_branch.clone();
    let base_branch = req.base_branch.clone();
    let base_commit = None;
    let worktree = false;
    let now = chrono::Utc::now();
    let mut agents = req
        .agents
        .clone()
        .unwrap_or_else(|| vec!["explore".into(), "general".into()]);
    // `explore` + `general` are always on, even if the caller toggled them off.
    store::ensure_required_agents(&mut agents);
    let mut task = Task {
        id: id.clone(),
        title,
        route_log: vec![],
        titled: false,
        status: "idle".into(),
        waiting_kind: None,
        step: "Starting".into(),
        project: project.clone(),
        base_commit,
        cwd,
        branch,
        base_branch,
        worktree,
        model: req.model.clone(),
        burner: policy.burner,
        effort: req.effort.min(4),
        perm: req.perm,
        plan: req.plan,
        // A plan artifict is created on approval, so a new chat has none.
        plan_artifict: None,
        ultra: req.ultra || req.ultra_wt || req.ultra_x.as_ref().is_some_and(|x| x.on()),
        ultra_wt: req.ultra_wt,
        ultra_x: req.ultra_x.map(|mut x| {
            x.normalize();
            x
        }),
        subagents: true,
        goal: None,
        assist: if matches!(req.assist.as_str(), "guide" | "necessary") {
            req.assist.clone()
        } else {
            "default".into()
        },
        agents,
        pending: Default::default(),
        paused: None,
        serving: String::new(),
        system: String::new(),
        mcp_tools: vec![],
        plugins: Default::default(),
        told: Default::default(),
        sub_items: HashMap::new(),
        sub_msgs: HashMap::new(),
        unpaused: false,
        stop_note: None,
        bg_live: vec![],
        busy: 0,
        // Nothing is alive in a task that has just been built: the mirror is
        // filled by the first recount, and every summary before that one is
        // telling the truth.
        bg_now: vec![],
        wrap_up: false,
        summarise_first: false,
        model_map: Default::default(),
        route: req
            .route
            .clone()
            .unwrap_or_else(|| router::default_route(&req.model)),
        archived: false,
        hidden: false,
        // A chat nobody has seen yet is not "waiting to be looked at": the
        // dot is for something arriving in a chat you already know about.
        unread: false,
        forked_from: None,
        pinned: false,
        artifict_open: false,
        order: 0.0,
        items: vec![],
        messages: vec![],
        todos: vec![],
        subs: vec![],
        usage: Default::default(),
        read_files: HashMap::new(),
        read_hashes: HashMap::new(),
        spans: vec![],
        checkpoints: vec![],
        touched: HashMap::new(),
        created_at: now,
        updated_at: now,
        // A new chat is one the user just started, so it opens at the top.
        touched_at: now,
        // Built in memory, so its subagent maps are here rather than on disk.
        hydrated: true,
        docker: None,
        sandbox: None,
    };
    let startup = Item::new(
        "notice",
        "Preparing your first message…",
        json!({"level": "info", "working": true, "startup": true}),
    );
    let startup_id = startup.id.clone();
    let startup_runtime = h.runtime(&id);
    startup_runtime
        .startup_pending
        .store(true, Ordering::SeqCst);
    let startup_guard = StartupGuard::new(startup_runtime);
    task.items.push(startup);
    let summary = task.summary();
    h.tasks
        .write()
        .await
        .insert(id.clone(), Arc::new(Mutex::new(task)));
    h.save_task(&id).await;
    remember_task_creation(&h, &project, &req.model, req.effort, options).await;
    start_new_task_prompt(
        h.clone(),
        NewTaskStartup {
            task_id: id,
            startup_item: startup_id,
            project,
            base_branch: req.base_branch,
            worktree: mode == "worktree",
            docker: mode == "docker",
            prompt: req.prompt.trim().to_string(),
            display,
            images: req.images,
            files: req.files,
            image_names: req.image_names,
            guard: startup_guard,
            require_worktree: policy.require_worktree,
        },
    );
    Ok(summary)
}

#[tauri::command]
async fn task_send(
    h: H<'_>,
    id: String,
    text: String,
    later: Option<bool>,
    images: Option<Vec<String>>,
    files: Option<Vec<String>>,
    image_names: Option<Vec<String>>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    crate::wait_for_task_startup(h.inner(), &id).await;
    let images = images.unwrap_or_default();
    let files = files.unwrap_or_default();
    let image_names = image_names.unwrap_or_default();
    if !images.is_empty() || !files.is_empty() {
        return runner::send_attachments(
            h.inner().clone(),
            id,
            text,
            images,
            files,
            image_names,
            later.unwrap_or(false),
        )
        .await;
    }
    if later.unwrap_or(false) && !text.starts_with('/') {
        return runner::send_opts(h.inner().clone(), id, text.clone(), text, true).await;
    }
    dispatch(h.inner().clone(), id, text).await
}

/// The question in a `/btw` line, or `None` when the line is not this command.
///
/// The word boundary is load-bearing and is the one Claude Code and Codex both
/// use: `/btw` and `/btw  why` are the command, but `/btwx` and `/btwish` are
/// ordinary messages that happen to start with those four characters. Without it
/// every such message was swallowed and answered as a question about the chat,
/// which reads as the app mishearing the user rather than as a bug.
fn btw_arg(rest: &str) -> Option<&str> {
    // Word characters are what the regex both Claude Code and Codex trigger on
    // count as a word, so the rule is theirs rather than one invented here:
    // `/btwx` is a longer word and stays an ordinary message, while `/btw?` and
    // `/btw why` are both this command.
    match rest.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => None,
        _ => Some(rest.trim()),
    }
}

/// Slash commands the backend owns; everything else goes to the agent.
async fn dispatch(h: Arc<Harness>, id: String, text: String) -> Result<(), String> {
    if let Some(rest) = text.strip_prefix("/btw") {
        let Some(q) = btw_arg(rest) else {
            return runner::send(h, id, text).await;
        };
        if q.is_empty() {
            return Err("Ask a question, e.g. /btw which file holds the retry logic?".into());
        }
        let q = q.to_string();
        // Keep one timeline row from submission through completion so the user
        // sees immediately that the side request started, even when routing or
        // the model takes a while to answer.
        let mut aside = Item::new(
            "notice",
            "Preparing side-question response…",
            json!({"level": "btw", "question": q, "working": true}),
        );
        h.upsert_item(&id, aside.clone()).await;
        let h2 = h.clone();
        tauri::async_runtime::spawn(async move {
            let c = tokio_util::sync::CancellationToken::new();
            match runner::btw(&h2, &id, &q, &c).await {
                Ok(answer) if answer.trim().is_empty() => {
                    aside.text = "Side question finished".into();
                    aside.data = json!({"level": "info"});
                    h2.upsert_item(&id, aside.clone()).await;
                    h2.upsert_item(
                        &id,
                        Item::new("notice", "No answer came back.", json!({"level":"info"})),
                    )
                    .await
                }
                Ok(answer) => {
                    aside.text = "Side question answered".into();
                    aside.data = json!({"level": "info"});
                    h2.upsert_item(&id, aside.clone()).await;
                    h2.upsert_item(
                        &id,
                        Item::new("notice", answer, json!({"level": "btw", "question": q})),
                    )
                    .await
                }
                Err(e) => {
                    aside.text = "Side question finished".into();
                    aside.data = json!({"level": "info"});
                    h2.upsert_item(&id, aside.clone()).await;
                    h2.upsert_item(
                        &id,
                        Item::new(
                            "notice",
                            format!("Side question failed: {e}"),
                            json!({"level":"error"}),
                        ),
                    )
                    .await
                }
            }
            h2.save_task(&id).await;
        });
        return Ok(());
    }
    if let Some(rest) = text.strip_prefix("/goal") {
        let rest = rest.trim().to_string();
        let t = h.task(&id).await?;
        if rest.is_empty() {
            let msg = match &t.lock().await.goal {
                Some(g) => format!("Goal ({}): {}{}", g.status, g.text, if g.summary.is_empty() { String::new() } else { format!("\n{}", g.summary) }),
                None => "No goal set. Use /goal <what done looks like> and the agent keeps working until it proves it's done.".into(),
            };
            h.upsert_item(&id, Item::new("notice", msg, json!({"level":"info"})))
                .await;
            return Ok(());
        }
        if matches!(rest.as_str(), "clear" | "off" | "stop") {
            h.update_task(&id, |t| t.goal = None).await;
            h.upsert_item(
                &id,
                Item::new("notice", "Goal cleared", json!({"level":"info"})),
            )
            .await;
            h.save_task(&id).await;
            return Ok(());
        }
        h.update_task(&id, |t| {
            t.goal = Some(agent::Goal {
                text: rest.clone(),
                status: "active".into(),
                summary: String::new(),
                nudges: 0,
            });
            if t.title.starts_with("/goal") {
                t.title = rest.chars().take(70).collect();
            }
        })
        .await;
        let model_text = format!("{}\n\n{}", agent::prompt::goal_intro(&rest), rest);
        return runner::send_as(h, id, format!("🎯 {rest}"), model_text).await;
    }
    if text == "/pause" {
        h.pause_task(&id, "manual", "Paused by you").await;
        return Ok(());
    }
    if let Some(rest) = text.strip_prefix("/resume") {
        let m = rest.trim();
        return runner::resume(&h, &id, (!m.is_empty()).then(|| m.to_string())).await;
    }
    if let Some(rest) = text.strip_prefix("/compact") {
        let h2 = h.clone();
        let instr = rest.trim().to_string();
        tauri::async_runtime::spawn(async move {
            let c = tokio_util::sync::CancellationToken::new();
            if let Err(e) = runner::compact(&h2, &id, &c, Some(&instr)).await {
                h2.upsert_item(
                    &id,
                    Item::new(
                        "notice",
                        format!("Compaction failed: {e}"),
                        json!({"level":"error"}),
                    ),
                )
                .await;
            }
            h2.save_task(&id).await;
        });
        return Ok(());
    }
    runner::send(h, id, text).await
}

#[tauri::command]
async fn task_interrupt(h: H<'_>, id: String) -> Result<(), String> {
    runner::interrupt(&h, &id).await;
    // Stopping is the user acting on this chat, so it comes to the top. (The
    // task may already be settling, which is fine: this is its last word.)
    h.touch(&id).await;
    Ok(())
}

/// Stop, the way the Stop button and `/stop` mean it: a force pause.
///
/// The run is frozen where it stands rather than cancelled, so a swarm is not
/// thrown away by the keystroke meant to stop it: every subagent keeps its own
/// transcript, the turn resumes at its next request, and each agent is told
/// which of its own background commands the stop killed. `task_interrupt` is
/// still the outright cancellation, and is what the paths that need the run
/// *ended* (`pcguard_stop`, dismissing a pause, archiving a paused chat) use.
#[tauri::command]
async fn task_stop(h: H<'_>, id: String) -> Result<(), String> {
    runner::stop(&h, &id).await;
    h.touch(&id).await;
    Ok(())
}

/// The window has this chat on screen and it just got attention, so the user
/// has already seen it. The backend marked the chat unread as it published the
/// event, and only opening a chat clears that mark: without this, a chat that
/// finished under you would show its dot again the moment you switched away.
/// Blocking waits stay marked (`Task::mark_seen`), so a question is not lost.
#[tauri::command]
async fn task_seen(h: H<'_>, id: String) -> Result<(), String> {
    h.seen(&id).await;
    Ok(())
}

/// Current PC-control banner state. The frontend calls this on open and then
/// follows `ol://pcguard` for changes; it never has to guess whether an agent
/// has the user's screen right now.
#[tauri::command]
async fn pcguard_state(app: tauri::AppHandle) -> Value {
    let _ = app;
    serde_json::to_value(agent::pcguard::state()).unwrap_or(Value::Null)
}

/// The banner's Stop button: end the run outright. Esc is deliberately gentler
/// (it only objects), so the button is the escape hatch for a runaway agent.
#[tauri::command]
async fn pcguard_stop(app: tauri::AppHandle, h: H<'_>, id: String) -> Result<(), String> {
    runner::interrupt(&h, &id).await;
    agent::pcguard::clear();
    show_guard(&app, false);
    Ok(())
}

#[tauri::command]
async fn task_respond(
    h: H<'_>,
    id: String,
    item_id: String,
    response: Value,
) -> Result<(), String> {
    h.respond_pending(&id, &item_id, response).await
}

/// The user answered a question the agent asked with `ask_nonblocking` — the kind it
/// raised without waiting. Nothing is pending on it, so this can't go through
/// the answer channel: it records the answer on the item and hands it to the
/// agent as a note, which it picks up on its next request (or, if the run has
/// already finished, as a new turn).
#[tauri::command]
async fn task_answer_nonblocking(
    h: H<'_>,
    id: String,
    item_id: String,
    response: Value,
) -> Result<(), String> {
    agent::runner::answer_nonblocking(&h, &id, &item_id, &response).await
}

#[tauri::command]
async fn task_dismiss_notice(h: H<'_>, id: String, item_id: String) -> Result<(), String> {
    h.dismiss_user_notice(&id, &item_id).await
}

fn task_assist(a: &str) -> String {
    if matches!(a, "guide" | "necessary") {
        a.into()
    } else {
        "default".into()
    }
}

/// Model / assist / agent changes: mid-run they wait for the next user
/// message (shown with a `*`) unless `now` is set, in which case the agent
/// picks them up on its very next request. Effort is never deferred: the run
/// loop re-reads it every request, and the model is never told it changed.
#[tauri::command]
async fn task_update(h: H<'_>, id: String, patch: Value) -> Result<TaskSummary, String> {
    apply_patch(&h, &id, patch).await
}

/// Apply a settings patch to one chat, with the task already locked.
async fn apply_patch(h: &Harness, id: &str, patch: Value) -> Result<TaskSummary, String> {
    let archive_paused = if patch["archived"].as_bool() == Some(true) {
        let paused_all = h.settings.read().await.paused_all;
        let task = h.task(id).await?;
        let task = task.lock().await;
        task.paused.is_some() || Harness::held_by_global(&task.summary(), paused_all)
    } else {
        false
    };
    if archive_paused {
        runner::interrupt(h, id).await;
    }
    let running = h
        .runtime(id)
        .running
        .load(std::sync::atomic::Ordering::SeqCst);
    let now = patch["now"].as_bool().unwrap_or(false);
    let t = h.task(id).await?;
    let mut t = t.lock().await;
    let defer = running && !now;
    for key in ["model", "assist", "agents"] {
        let Some(v) = patch.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let mut v = if key == "assist" {
            json!(task_assist(v.as_str().unwrap_or("")))
        } else {
            v.clone()
        };
        // `explore` + `general` are always on: re-add them to any agents change,
        // whether it applies now or waits for the next message.
        if key == "agents" {
            if let Some(arr) = v.as_array() {
                let mut ids: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                store::ensure_required_agents(&mut ids);
                v = json!(ids);
            }
        }
        if defer {
            let same = match key {
                "model" => v.as_str() == Some(t.model.as_str()),
                "assist" => v.as_str() == Some(t.assist.as_str()),
                _ => serde_json::to_value(&t.agents).ok().as_ref() == Some(&v),
            };
            if same {
                t.pending.remove(key);
            } else {
                t.pending.insert(key.into(), v);
            }
        } else {
            t.pending.remove(key);
            match key {
                "model" => t.model = v.as_str().unwrap_or(&t.model).to_string(),
                "assist" => t.assist = v.as_str().unwrap_or("default").to_string(),
                _ => {
                    t.agents = v
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    store::ensure_required_agents(&mut t.agents);
                    t.subagents = !t.agents.is_empty();
                }
            }
        }
    }
    if patch.get("apply_pending").and_then(|v| v.as_bool()) == Some(true) {
        runner::apply_pending(&mut t);
    }
    // Pending agents picked up above may predate the always-on rule: repair them too.
    if let Some(arr) = t.pending.get("agents").and_then(|v| v.as_array()).cloned() {
        let mut ids: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        store::ensure_required_agents(&mut ids);
        t.pending.insert("agents".into(), json!(ids));
    }
    if let Some(v) = patch["effort"].as_u64() {
        t.effort = (v as usize).min(4);
    }
    if let Some(v) = patch["perm"].as_str() {
        t.perm = v.into();
    }
    if let Some(v) = patch["plan"].as_bool() {
        t.plan = v;
    }
    if let Some(v) = patch["ultra"].as_bool() {
        t.ultra = v;
        if !v {
            t.ultra_wt = false;
            t.ultra_x = None;
        }
    }
    // Worktree mode implies ultrathread.
    if let Some(v) = patch["ultra_wt"].as_bool() {
        t.ultra_wt = v;
        if v {
            t.ultra = true;
        }
    }
    // ULTRATHREAD X. The ladder is normalized, and a ladder of fewer than two
    // layers is plain ultrathread again, so both agree on one representation.
    if patch.get("ultra_x").is_some() {
        match patch["ultra_x"].as_object() {
            Some(_) => {
                let mut x: store::UltraX = serde_json::from_value(patch["ultra_x"].clone())
                    .map_err(|e| format!("invalid ultrathread X ladder: {e}"))?;
                x.normalize();
                t.ultra_x = x.on().then_some(x);
                if t.ultra_x.is_some() {
                    t.ultra = true;
                    t.ultra_wt = false;
                }
            }
            None => t.ultra_x = None,
        }
    }
    if let Some(v) = patch["subagents"].as_bool() {
        // `explore` + `general` are always on, so subagents as a whole stay on:
        // turning them off would toggle the required agents off by the back door.
        if !v {
            store::ensure_required_agents(&mut t.agents);
            t.subagents = true;
        } else {
            t.subagents = v;
            if t.agents.is_empty() {
                t.agents = vec!["explore".into(), "general".into()];
            }
        }
    }
    // Belt and suspenders: required agents imply subagents.
    store::ensure_required_agents(&mut t.agents);
    if !t.agents.is_empty() {
        t.subagents = true;
    }
    if let Some(v) = patch["title"].as_str() {
        t.title = v.trim().chars().take(120).collect();
        // Renaming from the UI is the user claiming the name. Lock it so the
        // agent's `set_title` can't quietly undo their choice on the next turn.
        t.titled = true;
    }
    // The one way back: hand the name to the agent again.
    if patch["untitled"].as_bool() == Some(true) {
        t.titled = false;
    }
    if let Some(v) = patch["archived"].as_bool() {
        t.archived = v;
        if v {
            t.pinned = false;
            if archive_paused {
                t.paused = None;
                t.unpaused = true;
                t.status = "stopped".into();
                t.waiting_kind = None;
                if t.step.starts_with("Paused") {
                    t.step = "Stopped".into();
                }
            }
        }
    }
    if let Some(v) = patch["pinned"].as_bool() {
        t.pinned = v;
    }
    if let Some(v) = patch["artifict_open"].as_bool() {
        t.artifict_open = v;
    }
    if let Some(v) = patch["order"].as_f64() {
        t.order = v;
    }
    if let Some(v) = patch["route"].as_str() {
        t.route = v.into();
    }
    t.updated_at = chrono::Utc::now();
    h.emit_summary(&t);
    store::save_task(&t);
    Ok(t.summary())
}

/// Switch one subagent's model; it takes effect on the subagent's next request.
///
/// An empty model means "no model of your own" — the agent follows the chat
/// again, which is how a per-agent override is handed back rather than being a
/// one-way door set once at spawn.
#[tauri::command]
async fn sub_set_model(h: H<'_>, id: String, sub_id: String, model: String) -> Result<(), String> {
    h.update_task(&id, |t| {
        if let Some(s) = t.subs.iter_mut().find(|s| s.id == sub_id) {
            s.model = model;
        }
    })
    .await;
    Ok(())
}

/// Change one subagent's reasoning effort (None = back to its default); next request.
#[tauri::command]
async fn sub_set_effort(
    h: H<'_>,
    id: String,
    sub_id: String,
    effort: Option<usize>,
) -> Result<(), String> {
    h.update_task(&id, |t| {
        if let Some(s) = t.subs.iter_mut().find(|s| s.id == sub_id) {
            s.effort = effort.map(|e| e.min(4));
        }
    })
    .await;
    Ok(())
}

#[tauri::command]
async fn stats_get(h: H<'_>) -> Result<agent::stats::StatsData, String> {
    Ok(h.stats.snapshot())
}

/// One chat's stats, by id.
///
/// Separate from `stats_get` because that returns the whole ledger — every
/// model, day and chat — and a right-click on one chat does not need the other
/// 130 KB of it. The name comes back with the record because the caller is a
/// context menu on a chat that may since have been deleted, and a panel that
/// said only "this chat" after the row it was opened from is gone is useless.
#[tauri::command]
async fn stats_chat(
    h: H<'_>,
    id: String,
) -> Result<Option<(String, agent::stats::ChatStat)>, String> {
    let title = match h.task(&id).await {
        Ok(t) => t.lock().await.title.clone(),
        // A chat that no longer exists is not an error: its numbers are kept
        // after deletion on purpose, so the panel has something real to show.
        Err(_) => String::new(),
    };
    Ok(h.stats.chat(&id).map(|c| (title, c)))
}

/// One chat's run timeline: lanes, timed blocks and totals, assembled from the
/// spans recorded with the task (see `agent/spans.rs`). Built on request rather
/// than kept up to date, since nothing reads it until the panel is open.
#[tauri::command]
async fn task_run_timeline(h: H<'_>, id: String) -> Result<agent::spans::RunTimeline, String> {
    let live = h.bg.list(&id);
    let t = h.task(&id).await?;
    let t = t.lock().await;
    Ok(agent::spans::assemble(&t, &live, chrono::Utc::now()))
}

#[tauri::command]
async fn draft_get(key: String) -> Result<String, String> {
    Ok(store::load_draft(&key))
}

#[tauri::command]
async fn draft_set(key: String, text: String) -> Result<(), String> {
    store::save_draft(&key, &text);
    Ok(())
}

/// Type into a subagent's panel.
#[tauri::command]
async fn sub_send(
    h: H<'_>,
    id: String,
    sub_id: String,
    text: String,
    images: Option<Vec<String>>,
    image_names: Option<Vec<String>>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    let images = images.unwrap_or_default();
    let image_names = image_names.unwrap_or_default();
    if text.is_empty() && images.is_empty() {
        return Ok(());
    }
    runner::sub_message(h.inner(), &id, &sub_id, text, images, image_names).await
}

/// Stop one running subagent. Only that agent is cancelled; the chat carries on.
#[tauri::command]
async fn sub_stop(h: H<'_>, id: String, sub_id: String) -> Result<(), String> {
    runner::sub_stop(h.inner(), &id, &sub_id).await
}

/// A status report on what the chat is up to, without interrupting it.
#[tauri::command]
async fn task_status_summary(h: H<'_>, id: String) -> Result<Value, String> {
    runner::status_summary(h.inner(), &id).await
}

#[tauri::command]
async fn task_pause(h: H<'_>, id: String) -> Result<(), String> {
    h.pause_task(&id, "manual", "Paused by you").await;
    Ok(())
}

/// Pause without waiting: kill in-flight commands (foreground + background). Each one's
/// result tells the agent it was cut off, so it reruns them after resuming.
#[tauri::command]
async fn task_force_pause(h: H<'_>, id: String) -> Result<(), String> {
    h.pause_task(&id, "manual", "Force paused by you").await;
    h.force_pause(&id).await;
    Ok(())
}

/// `pause_all`, then kill every task's in-flight commands.
///
/// The kills run together rather than one chat after another: `force_pause`
/// sleeps 300 ms for the killed processes to record their exits, and this is the
/// button the user reaches for precisely when several chats are mid-build — run
/// in sequence, the last chat's command outlived the press by N × 300 ms, so
/// "kill them now" reached the chat in front of the user last. Each chat's
/// sweep is its own runtime and its own commands, so they are independent work.
#[tauri::command]
async fn force_pause_all(h: H<'_>) -> Result<(), String> {
    pause_all_chats(h.inner()).await?;
    let busy: Vec<String> = summaries(&h)
        .await
        .into_iter()
        .filter(|t| t.busy > 0)
        .map(|t| t.id)
        .collect();
    futures_util::future::join_all(busy.iter().map(|id| h.force_pause(id))).await;
    Ok(())
}

/// Dismiss a pause: turn it into a stop. Whatever was frozen is cancelled (the agent
/// is told on its next run, like any stop) and the chat no longer counts as paused.
#[tauri::command]
async fn task_dismiss_pause(h: H<'_>, id: String) -> Result<(), String> {
    runner::interrupt(&h, &id).await;
    h.update_task(&id, |t| {
        t.paused = None;
        // Opt out of a global pause too, without resuming.
        t.unpaused = true;
        if t.status == "running" || t.status == "waiting" {
            t.status = "stopped".into();
        }
        if t.step.starts_with("Paused") {
            t.step = "Stopped".into();
        }
    })
    .await;
    h.pause_bell.notify_waiters();
    h.save_task(&id).await;
    Ok(())
}

#[tauri::command]
async fn task_resume(
    h: H<'_>,
    id: String,
    message: Option<String>,
    mode: Option<String>,
) -> Result<(), String> {
    wait_for_task_startup(h.inner(), &id).await;
    h.touch(&id).await;
    let res = match mode.as_deref() {
        Some(m @ ("continue" | "summarise" | "wrap")) if message.is_none() => {
            runner::resume_after_stop(h.inner(), &id, m).await
        }
        _ => runner::resume(h.inner(), &id, message).await,
    };
    // This chat may have been the last one the global flag was holding, which
    // leaves the flag on over nothing — the state that made the banner claim
    // "Everything is paused" with no chat paused. Clearing it here is the same
    // check `tasks_resume` makes after a partial lift, and it is what stops the
    // flag being written back to disk for a next launch to find.
    h.clear_stranded_global_pause().await;
    res
}

/// One message to every agent working in this task, framed so each one ignores
/// it unless it's about their own work.
#[tauri::command]
async fn task_broadcast(h: H<'_>, id: String, text: String) -> Result<String, String> {
    let to = runner::broadcast(h.inner(), &id, &text).await?;
    let n = to.iter().filter(|x| *x != "main").count();
    Ok(if n == 0 {
        "Sent to the main agent. It has no subagents working right now.".to_string()
    } else {
        format!("Sent to the main agent and {n} working subagent{} ({}). Each one applies it only if it's about their own work.", if n == 1 { "" } else { "s" }, to[1..].join(", "))
    })
}

/// Freeze every agent in every task (e.g. before shutting the PC down).
///
/// The flag goes up *first*, ahead of the walk below, because that flag is the
/// freeze: `paused_all` is what every gate reads — `is_paused` on the way into a
/// model request, the per-attempt watcher that cancels one already in flight,
/// the bash handler before it starts anything — so every agent starts parking the
/// moment it lands. Walking the chats first and flipping the flag at the end (as
/// this did) froze *nothing* for the length of that walk, and the walk waits on
/// each chat's lock in turn, so one chat mid-turn held up the freeze for every
/// chat. That is the shape of "Pause all does not work / takes a while": the
/// press was heard, the freeze was just queued behind bookkeeping the UI only
/// reads afterwards.
///
/// What the walk is for, and none of it may gate the freeze:
///
///   * each chat's `unpaused` opt-out is reset, so a chat the user opted out of
///     (by messaging it under an earlier global pause) is part of *this* one. It
///     is user-visible state — `is_paused` reads it — so each reset is published
///     and saved: it was in memory only once, and a restart brought back a stale
///     `unpaused: true` and ran a chat the user had just frozen;
///   * the in-flight command counts are recounted, because the banner names them
///     ("Pausing… waiting on N commands to finish") rather than only counting
///     them. The counts are live — every command start and exit publishes them —
///     so this is a catch-up pass for a chat nothing has recounted since it was
///     restored, not the thing the banner is waiting for (see `ui/Home.tsx` for
///     the settling state that covers the gap).
async fn pause_all_chats(h: &Harness) -> Result<(), String> {
    let s = {
        let mut s = h.settings.write().await;
        s.paused_all = true;
        s.paused_reason = "Paused everything".into();
        // Through the coalescing writer rather than inline: same reasoning as
        // `task_create`. This is a deliberate user action rather than something
        // on the agent loop, so nothing here reads the file back — the event
        // below is built from memory.
        drop(s);
        let s = h.settings.read().await.clone();
        h.save_settings_debounced().await;
        s
    };
    (h.bus)(
        "ol://settings",
        serde_json::to_value(public_settings(&s)).unwrap_or_default(),
    );
    // The flag *is* the freeze, so this wake is what makes it take effect now:
    // every gate already parked on the bell re-reads it and parks for good, and
    // a request in flight is cancelled by its own watcher.
    h.pause_bell.notify_waiters();
    let refs: Vec<_> = h.tasks.read().await.values().cloned().collect();
    for t in &refs {
        let id = t.lock().await.id.clone();
        {
            let mut g = t.lock().await;
            g.unpaused = false;
            h.emit_summary(&g);
        }
        h.save_task(&id).await;
    }
    for t in summaries(h).await {
        h.refresh_busy(&t.id).await;
    }
    // A chat that had opted out was not held by the flag when the first wake went
    // out — its own `unpaused` said no — so nothing re-read the flag on its
    // behalf. The reset above is what brings it under this pause, and it needs the
    // same wake to be felt: its watcher registers for the bell's *next* ring.
    h.pause_bell.notify_waiters();
    Ok(())
}

/// The whole of "Pause all" lives in `pause_all_chats`, which is testable
/// without a Tauri `State`; this is only the command surface.
#[tauri::command]
async fn pause_all(h: H<'_>) -> Result<(), String> {
    pause_all_chats(h.inner()).await
}

/// Lift the global pause and every per-task pause; restart tasks the app closed on.
#[tauri::command]
async fn resume_all(h: H<'_>, message: Option<String>) -> Result<(), String> {
    let was_paused_all = {
        let mut s = h.settings.write().await;
        let was_paused_all = s.paused_all;
        s.paused_all = false;
        s.paused_reason.clear();
        drop(s);
        h.save_settings_debounced().await;
        // As in pause_all: the UI only learns the global pause moved from here.
        let s = h.settings.read().await.clone();
        let out = serde_json::to_value(public_settings(&s)).unwrap_or_default();
        (h.bus)("ol://settings", out);
        was_paused_all
    };
    h.pause_bell.notify_waiters();
    resume_all_tasks(h.inner(), summaries(&h).await, message, was_paused_all).await;
    Ok(())
}

async fn resume_all_tasks(
    h: &Arc<Harness>,
    tasks: Vec<TaskSummary>,
    message: Option<String>,
    was_paused_all: bool,
) {
    // Do not call `resume` on chats that were already running or had opted out of
    // the global pause. `resume` updates `updated_at`, and settled chats sort on
    // that timestamp — a no-op resume here made Resume all reshuffle the sidebar.
    for t in tasks {
        if t.paused.is_some() || Harness::held_by_global(&t, was_paused_all) {
            if let Err(e) = runner::resume(h, &t.id, message.clone()).await {
                eprintln!("[openleash] resume {}: {e}", t.id);
            }
        }
    }
}

/// Lift the pauses on a chosen set of chats, leaving the rest frozen.
///
/// `resume_all` is the blunt version: it drops the global pause, so it cannot
/// be used to thaw one chat under "Pause all" — that flag is what freezes every
/// chat at once, and a per-task resume underneath it stays frozen. This walks
/// the given ids instead. It only clears the global flag when *nothing* is left
/// to thaw, so a partial resume doesn't quietly release the chats that weren't
/// ticked. `message` (optional) goes to each agent that was actually lifted.
#[tauri::command]
async fn tasks_resume(
    h: H<'_>,
    ids: Vec<String>,
    message: Option<String>,
) -> Result<usize, String> {
    let msg = message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let mut lifted = 0usize;
    let mut candidates: Vec<String> = vec![];
    for t in summaries(&h).await {
        // Only chats a pause can still be holding: a finished or idle chat has
        // nothing to thaw, and resuming it would be a promise we can't keep.
        if !ids.contains(&t.id)
            || t.unpaused
            || (t.paused.is_none() && t.status != "running" && t.status != "waiting")
        {
            continue;
        }
        if let Err(e) = runner::resume(h.inner(), &t.id, msg.clone()).await {
            eprintln!("[openleash] resume {}: {e}", t.id);
            continue;
        }
        lifted += 1;
        candidates.push(t.id);
    }
    // Drop the global pause only if this lift left *nothing* frozen. The test has
    // to be against the same set `is_paused` freezes — chats that are running or
    // waiting and have not opted out — and the only honest way to ask that is to
    // ask the predicate itself. It used to compare the number of lifted chats
    // against the number of chats with their own pause and no opt-out, which are
    // different sets entirely: lifting one chat under "Pause all" could satisfy it
    // with nothing actually thawed, and the flag would fall, quietly releasing
    // every chat the user left frozen.
    if h.settings.read().await.paused_all {
        let still: Vec<bool> = {
            let ts = summaries(&h).await;
            let mut v = Vec::with_capacity(ts.len());
            for t in &ts {
                v.push(h.is_paused(&t.id).await);
            }
            v
        };
        if still.iter().all(|p| !p) {
            let mut s = h.settings.write().await;
            s.paused_all = false;
            s.paused_reason.clear();
            store::save_settings(&s);
            let out = serde_json::to_value(public_settings(&s)).unwrap_or_default();
            drop(s);
            (h.bus)("ol://settings", out);
        }
    }
    h.pause_bell.notify_waiters();
    for id in &candidates {
        h.update_task(id, |_| {}).await;
    }
    Ok(lifted)
}

/// Discard the pauses on a chosen set of chats: turn each into a stop, without
/// resuming anything.
///
/// The bulk half of `task_dismiss_pause`, for the same reason `tasks_resume`
/// exists — the per-row control on the paused list walks the list one chat at a
/// time, and "none of this is worth continuing" is the other thing a user with a
/// screen full of frozen chats actually wants to say. It cancels the frozen work
/// (`runner::interrupt`), clears `paused`, and leaves the chat stopped, which is
/// the same shape the single-chat command produces: the transcript stays and the
/// agent picks up again whenever the user continues or says something else.
///
/// Only ids that are *currently* frozen are touched, against the same predicate
/// the UI lists by, so a stale id cannot stop a chat that has since resumed and
/// started working. Returns how many were actually discarded.
#[tauri::command]
async fn tasks_dismiss_pause(h: H<'_>, ids: Vec<String>) -> Result<usize, String> {
    let mut gone = 0usize;
    for t in summaries(&h).await {
        if !ids.contains(&t.id) || !h.is_paused(&t.id).await {
            continue;
        }
        runner::interrupt(&h, &t.id).await;
        h.update_task(&t.id, |t| {
            t.paused = None;
            // Opt out of a global pause too, without resuming — same as the
            // single-chat command. Otherwise a chat stopped here is still held by
            // "Pause all" and the flag has nothing left worth clearing.
            t.unpaused = true;
            if t.status == "running" || t.status == "waiting" {
                t.status = "stopped".into();
            }
            if t.step.starts_with("Paused") {
                t.step = "Stopped".into();
            }
        })
        .await;
        h.save_task(&t.id).await;
        gone += 1;
    }
    // Nothing is frozen any more, so a global flag left over from "Pause all" has
    // nothing to hold. Same reconciliation `tasks_resume` does, and for the same
    // reason: leaving it set is what makes every other screen announce a pause
    // that no longer exists.
    h.clear_stranded_global_pause().await;
    h.pause_bell.notify_waiters();
    Ok(gone)
}

/// Send one message into many chats at once. Returns how many chats took it.
///
/// Each chat gets its own `runner::send`, so a chat that is already running has the
/// message steered into its next model call and an idle one starts a fresh turn.
/// A failure on one chat never stops the rest: we log it and keep going, the same
/// way `resume_all` does.
#[tauri::command]
async fn tasks_message(
    h: H<'_>,
    ids: Vec<String>,
    text: String,
    later: Option<bool>,
) -> Result<usize, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Nothing to send".into());
    }
    let later = later.unwrap_or(false);
    let mut sent = 0;
    for id in ids {
        // Ignore ids that vanished between picking them and pressing send.
        if h.task(&id).await.is_err() {
            continue;
        }
        wait_for_task_startup(h.inner(), &id).await;
        match runner::send_opts(
            h.inner().clone(),
            id.clone(),
            text.clone(),
            text.clone(),
            later,
        )
        .await
        {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[openleash] message {}: {e}", id),
        }
    }
    Ok(sent)
}

// ───────────────────────────── accounts ─────────────────────────────

#[tauri::command]
async fn accounts_list(h: H<'_>) -> Result<Vec<accounts::AccountView>, String> {
    Ok(h.accts.views(&h.settings.read().await.accounts))
}

async fn add_account(
    h: &Arc<Harness>,
    a: agent::store::Account,
) -> Result<Vec<accounts::AccountView>, String> {
    add_account_with(h, a, claude_allowed()).await
}

/// [`add_account`] with the install's opt-in passed in rather than probed, so a
/// test can pin both sides of the refusal without depending on where the test
/// binary happens to live.
async fn add_account_with(
    h: &Arc<Harness>,
    mut a: agent::store::Account,
    allow_claude: bool,
) -> Result<Vec<accounts::AccountView>, String> {
    account_import_allowed(&a.kind, allow_claude)?;
    // Setup tokens can't be refreshed, so prove they work before saving (no
    // silent duds). The same rule covers an OpenCode Go key: it is a bare
    // credential with nothing else to validate it against, and a typo would
    // otherwise only surface as every chat failing later.
    if a.kind == "claude" && a.refresh_token.is_empty() {
        accounts::claude_probe(h, &a).await?;
    } else if a.kind == "kimi-code" {
        accounts::kimi_probe(h, &a).await?;
    } else if accounts::key_only(&a.kind) {
        accounts::go_probe(h, &a).await?;
    }
    {
        let mut s = h.settings.write().await;
        if let Some(x) = s.accounts.iter_mut().find(|x| {
            x.kind == a.kind
                && ((!a.account_id.is_empty() && x.account_id == a.account_id)
                    || x.access_token == a.access_token
                    || (!a.email.is_empty() && x.email == a.email))
        }) {
            a.id = x.id.clone();
            a.priority = x.priority;
            a.label = if a.label.is_empty() {
                x.label.clone()
            } else {
                a.label.clone()
            };
            *x = a.clone();
        } else {
            let n = s.accounts.iter().filter(|x| x.kind == a.kind).count() + 1;
            if a.label.is_empty() || a.label == "max" || a.label == "pro" {
                let plan = a.label.clone();
                let name = providers::provider(&a.kind)
                    .and_then(|p| p.account)
                    .map(|a| a.short_name)
                    .unwrap_or("Account");
                a.label = format!(
                    "{name} {n}{}",
                    if plan.is_empty() {
                        String::new()
                    } else {
                        format!(" · {plan}")
                    }
                );
            }
            s.accounts.push(a.clone());
        }
        store::save_settings(&s);
    }
    let _ = accounts::fetch_usage(h, &a.id).await;
    refresh_pool_models(h).await;
    Ok(h.accts.views(&h.settings.read().await.accounts))
}

/// Pull the live model lists for connected subscriptions and push them to the UI.
async fn refresh_pool_models(h: &Arc<Harness>) {
    let kinds: Vec<String> = {
        let s = h.settings.read().await;
        let mut k: Vec<String> = s
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .map(|a| a.kind.clone())
            .collect();
        // Key-login pools can also be connected through the existing provider
        // key field (`OPENCODE_API_KEY`). Auto-discover Go models there too; the
        // usage/account screen is optional, but model discovery shouldn't become
        // less live merely because someone already had their key set up.
        k.extend(
            providers::PROVIDERS
                .iter()
                .filter(|p| {
                    p.account.is_some_and(|a| a.key_login)
                        && !providers::api_key(&s, p.id).is_empty()
                })
                .map(|p| p.id.to_string()),
        );
        k.sort();
        k.dedup();
        k
    };
    let mut changed = false;
    for kind in kinds {
        match accounts::fetch_pool_models(h, &kind).await {
            Ok(n) if n > 0 => changed = true,
            Ok(_) => {}
            Err(e) => eprintln!("[openleash] {kind} model list: {e}"),
        }
    }
    if changed {
        let s = h.settings.read().await.clone();
        (h.bus)(
            "ol://models",
            serde_json::to_value(models(&s)).unwrap_or_default(),
        );
    }
}

#[tauri::command]
async fn pool_models_refresh(h: H<'_>) -> Result<Vec<ModelInfo>, String> {
    refresh_pool_models(h.inner()).await;
    Ok(models(&*h.settings.read().await))
}

/// Paste a login (file contents or token).
#[tauri::command]
async fn account_import(
    h: H<'_>,
    kind: String,
    text: String,
) -> Result<Vec<accounts::AccountView>, String> {
    let a = accounts::parse(&text, &kind, "")?;
    add_account(h.inner(), a).await
}

/// Import the login the official CLI stored on this machine (and keep it in sync).
#[tauri::command]
async fn account_import_local(
    h: H<'_>,
    kind: String,
    path: Option<String>,
) -> Result<Vec<accounts::AccountView>, String> {
    let p = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => std::path::PathBuf::from(p),
        // A key-login provider with no local login file (Kimi Code) refuses
        // here with the fix named, rather than "no home directory" — the UI
        // hides the button for it, but this command is reachable from the
        // SDK and the phone browser too.
        None
            if providers::provider(&kind)
                .and_then(|p| p.account)
                .is_some_and(|a| a.login_file.is_empty()) =>
        {
            let name = providers::provider(&kind)
                .and_then(|p| p.account)
                .map(|a| a.display_name)
                .unwrap_or(kind.as_str());
            return Err(format!(
                "{name} has no local login to import — paste a key instead."
            ));
        }
        None => accounts::default_path(&kind).ok_or("no home directory")?,
    };
    let text = std::fs::read_to_string(&p).map_err(|_| {
        let command = providers::provider(&kind)
            .and_then(|p| p.account)
            .map(|a| a.login_command)
            .unwrap_or("provider login");
        format!(
            "Couldn't read {}. Run {command} first, or paste the file instead.",
            p.display()
        )
    })?;
    let a = accounts::parse(&text, &kind, &p.to_string_lossy())?;
    add_account(h.inner(), a).await
}

#[tauri::command]
async fn account_update(
    h: H<'_>,
    id: String,
    patch: Value,
) -> Result<Vec<accounts::AccountView>, String> {
    let mut s = h.settings.write().await;
    if let Some(a) = s.accounts.iter_mut().find(|a| a.id == id) {
        if let Some(v) = patch["label"].as_str() {
            a.label = v.trim().into();
        }
        if let Some(v) = patch["priority"].as_i64() {
            a.priority = v as i32;
        }
        if let Some(v) = patch["enabled"].as_bool() {
            a.enabled = v;
            if v {
                a.disabled_reason.clear();
            }
        }
    }
    // Move up/down: swap priorities with the neighbour in display order.
    if let Some(dir) = patch["move"].as_i64() {
        let kind = s
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.kind.clone())
            .unwrap_or_default();
        let mut order: Vec<usize> = (0..s.accounts.len())
            .filter(|&i| s.accounts[i].kind == kind)
            .collect();
        order.sort_by(|&a, &b| {
            s.accounts[b]
                .priority
                .cmp(&s.accounts[a].priority)
                .then(a.cmp(&b))
        });
        if let Some(pos) = order.iter().position(|&i| s.accounts[i].id == id) {
            let other = pos as i64 + if dir < 0 { -1 } else { 1 };
            if other >= 0 && (other as usize) < order.len() {
                order.swap(pos, other as usize);
            }
            let n = order.len() as i32;
            for (rank, &i) in order.iter().enumerate() {
                s.accounts[i].priority = (n - rank as i32) * 10;
            }
        }
    }
    store::save_settings(&s);
    Ok(h.accts.views(&s.accounts))
}

#[tauri::command]
async fn account_remove(h: H<'_>, id: String) -> Result<Vec<accounts::AccountView>, String> {
    let mut s = h.settings.write().await;
    s.accounts.retain(|a| a.id != id);
    store::save_settings(&s);
    Ok(h.accts.views(&s.accounts))
}

#[tauri::command]
async fn accounts_refresh(h: H<'_>) -> Result<Vec<accounts::AccountView>, String> {
    accounts::refresh_all(&h).await;
    Ok(h.accts.views(&h.settings.read().await.accounts))
}

/// Put a cap on one usage window across these accounts: each of them stops
/// being used once that window reaches `used` percent.
///
/// The wire carries a duration rather than an ending time, because the same
/// backend is driven from a phone browser whose clock is its own. `secs` of 0 is
/// a cap that stands until the user lifts it; anything else is stamped against
/// the backend's clock on arrival.
#[tauri::command]
async fn account_cap_set(
    h: H<'_>,
    ids: Vec<String>,
    window: String,
    used: f64,
    secs: i64,
) -> Result<Vec<accounts::AccountView>, String> {
    let window = window.trim().to_string();
    if window.is_empty() {
        return Err("A cap needs the usage window it applies to".into());
    }
    if !used.is_finite() || used <= 0.0 {
        return Err("A cap has to be a percentage of the window".into());
    }
    if secs < 0 {
        return Err("A cap cannot lift before it is set".into());
    }
    if ids.is_empty() {
        return Err("No account to cap".into());
    }
    let until = if secs > 0 { accounts::now() + secs } else { 0 };
    let mut s = h.settings.write().await;
    for a in s.accounts.iter_mut().filter(|a| ids.contains(&a.id)) {
        // One cap per window: setting a second one replaces it, the same way the
        // page draws one badge per line.
        let cap = store::UsageCap {
            window: window.clone(),
            used: used.min(100.0),
            until,
        };
        match a.caps.iter_mut().find(|c| c.window == window) {
            Some(c) => *c = cap,
            None => a.caps.push(cap),
        }
    }
    store::save_settings(&s);
    Ok(h.accts.views(&s.accounts))
}

/// Lift a cap, so the window may be spent as it was before.
#[tauri::command]
async fn account_cap_clear(
    h: H<'_>,
    ids: Vec<String>,
    window: String,
) -> Result<Vec<accounts::AccountView>, String> {
    let mut s = h.settings.write().await;
    for a in s.accounts.iter_mut().filter(|a| ids.contains(&a.id)) {
        a.caps.retain(|c| c.window != window);
    }
    store::save_settings(&s);
    Ok(h.accts.views(&s.accounts))
}

#[tauri::command]
async fn agents_list(h: H<'_>, project: Option<String>) -> Result<Vec<AgentDef>, String> {
    let s = h.settings.read().await;
    Ok(store::all_agents(
        &s,
        project.as_deref().unwrap_or(&s.project),
    ))
}

// ───────────────────────────── skills ─────────────────────────────

#[tauri::command]
async fn skills_list(h: H<'_>, project: Option<String>) -> Result<Vec<SkillDef>, String> {
    let s = h.settings.read().await;
    Ok(store::all_skills(
        &s,
        project.as_deref().unwrap_or(&s.project),
    ))
}

/// Import a skill from a `SKILL.md` file or a folder containing one.
/// Copies it into `~/.openleash/skills/<name>/` and re-enables it.
#[tauri::command]
async fn skill_import(h: H<'_>, path: String) -> Result<SkillDef, String> {
    let def = store::import_skill(&path)?;
    let mut s = h.settings.write().await;
    s.disabled_skills.retain(|n| n != &def.name);
    store::save_settings(&s);
    Ok(def)
}

/// Delete a user skill (`~/.openleash/skills/<name>/`). Project skills live
/// in the repo — delete them there instead.
#[tauri::command]
async fn skill_remove(h: H<'_>, name: String) -> Result<Vec<SkillDef>, String> {
    store::remove_skill(&name)?;
    let mut s = h.settings.write().await;
    s.disabled_skills.retain(|n| n != &name);
    store::save_settings(&s);
    let project = s.project.clone();
    Ok(store::all_skills(&s, &project))
}

#[tauri::command]
async fn skill_set_enabled(h: H<'_>, name: String, enabled: bool) -> Result<Vec<SkillDef>, String> {
    let project = h.settings.read().await.project.clone();
    if !store::all_skills(&*h.settings.read().await, &project)
        .iter()
        .any(|x| x.name == name)
    {
        return Err("Unknown skill.".into());
    }
    let mut s = h.settings.write().await;
    if enabled {
        s.disabled_skills.retain(|n| n != &name);
    } else if !s.disabled_skills.contains(&name) {
        s.disabled_skills.push(name);
    }
    store::save_settings(&s);
    Ok(store::all_skills(&s, &s.project.clone()))
}

/// Full `SKILL.md` text plus bundled file list, for the settings preview.
#[tauri::command]
async fn skill_read(h: H<'_>, name: String, project: Option<String>) -> Result<Value, String> {
    let s = h.settings.read().await;
    let p = project.unwrap_or_else(|| s.project.clone());
    match store::read_skill(&s, &p, &name) {
        Some((def, content, files)) => {
            Ok(json!({"skill": def, "content": content, "files": files}))
        }
        None => Err("Unknown skill.".into()),
    }
}

/// "Save as skill…" (see `agent::skillgen`). These three commands are the only
/// way in, and they are UI commands: no agent tool, slash command or connector
/// reaches them. The window check keeps the PC-control guard webview (which
/// shares the default command ACL) out; remote control invokes as the main
/// window, and a paired device is the user.
fn main_window_only(w: &tauri::Window) -> Result<(), String> {
    if w.label() != "main" {
        return Err("Saving a chat as a skill is only available from the main window.".into());
    }
    Ok(())
}

/// Ask a model to distil chat `id` into a SKILL.md draft for the user to edit.
/// Writes nothing.
#[tauri::command]
async fn skill_draft_from_chat(
    h: H<'_>,
    window: tauri::Window,
    id: String,
    model: Option<String>,
) -> Result<agent::skillgen::SkillDraft, String> {
    main_window_only(&window)?;
    let h = h.inner().clone();
    agent::skillgen::draft(&h, &id, model.as_deref()).await
}

#[tauri::command]
async fn skill_draft_cancel(window: tauri::Window, id: String) -> Result<(), String> {
    main_window_only(&window)?;
    agent::skillgen::cancel_draft(&id);
    Ok(())
}

/// Write the draft the user approved. Asks back (without writing) when the
/// skill exists or secrets remain, until the user confirms.
#[tauri::command]
async fn skill_save_from_chat(
    h: H<'_>,
    window: tauri::Window,
    req: agent::skillgen::SkillSaveReq,
) -> Result<agent::skillgen::SkillSaveResult, String> {
    main_window_only(&window)?;
    let h = h.inner().clone();
    agent::skillgen::save(&h, &req).await
}

const EXPORT_FORMAT: &str = "openleash-chats";

/// Chats as a portable JSON file (full history, timeline, subagent transcripts).
fn export_json(tasks: Vec<Task>) -> Result<String, String> {
    serde_json::to_string_pretty(&json!({"format": EXPORT_FORMAT, "version": 1, "exported_at": chrono::Utc::now(), "tasks": tasks})).map_err(|e| e.to_string())
}

/// Fine-tuning conversation as JSONL. OpenRouter-compatible chat format keeps
/// Fine-tuning conversation as JSONL.
///
/// Two shapes, because the two targets ask for different ones:
///   * `openrouter` — the chat-completion shape, system prompt inside
///     `messages` as its own turn. This is what OpenAI-style trainers and
///     OpenRouter's own dataset tooling expect.
///   * `claude` — the Anthropic shape, system prompt in the top-level `system`
///     field, and `messages` holding only user/assistant turns.
///
/// Only visible text survives: tool calls, tool results and the model's
/// hidden reasoning are dropped, because a text-only trainer has no way to
/// represent them and a half-represented tool turn trains the model to invent
/// tool calls. The user's own attachments survive as their text; images do
/// not, since that would need a content-block shape the two formats spell
/// differently.
///
/// The alternating rule is enforced for Claude (its trainer rejects two turns
/// of the same role) and repaired rather than rejected for OpenRouter, which
/// tolerates it.
fn training_jsonl(task: &Task, system: &str, format: &str) -> Result<String, String> {
    if format != "openrouter" && format != "claude" {
        return Err("Unknown training export format".into());
    }
    let mut messages = Vec::<Value>::new();
    for message in &task.messages {
        if message.role != "user" && message.role != "assistant" { continue; }
        let text = message.content.iter().filter_map(|block| {
            (block["type"] == "text").then(|| block["text"].as_str()).flatten()
        }).collect::<Vec<_>>().join("\n");
        if text.trim().is_empty() { continue; }
        messages.push(json!({"role":message.role, "content":text}));
    }
    // Consecutive turns of the same role: one turn with the two texts joined.
    let mut merged: Vec<Value> = Vec::with_capacity(messages.len());
    for m in messages {
        let role = m["role"].as_str().unwrap_or("").to_string();
        if merged.last().and_then(|l| l["role"].as_str()) == Some(role.as_str()) {
            let prior = merged.last().unwrap()["content"].as_str().unwrap_or("").to_string();
            let next = m["content"].as_str().unwrap_or("").to_string();
            merged.last_mut().unwrap()["content"] = json!(format!("{prior}\n\n{next}"));
        } else {
            merged.push(m);
        }
    }
    // Claude wants the first turn from the user and the last from the
    // assistant; drop a leading assistant turn rather than fail the export.
    if format == "claude" {
        while merged.first().and_then(|m| m["role"].as_str()) == Some("assistant") {
            merged.remove(0);
        }
        // Claude's Bedrock trainer requires the example to end with an
        // assistant turn too. An unfinished final user turn cannot teach a
        // target response, so omit it rather than write an invalid example.
        while merged.last().and_then(|m| m["role"].as_str()) == Some("user") {
            merged.pop();
        }
    }
    if merged.iter().all(|m| m["role"] != "assistant") {
        return Err("This chat has no assistant text to export".into());
    }
    let mut example = json!({"messages":merged});
    match format {
        "claude" => {
            if !system.trim().is_empty() { example["system"] = json!(system); }
        }
        _ => {
            if !system.trim().is_empty() {
                example["messages"].as_array_mut().unwrap().insert(0, json!({"role":"system", "content":system}));
            }
        }
    }
    Ok(format!("{}\n", serde_json::to_string(&example).map_err(|e| e.to_string())?))
}

/// Write one chat as a training dataset.
///
/// The command is a thin wrapper so the work can be tested against a plain
/// `&Harness` — `tauri::State` can't be built outside the app.
#[tauri::command]
async fn task_training_export(h: H<'_>, id: String, path: String, format: String) -> Result<(), String> {
    training_export(h.inner(), &id, &path, &format).await
}

async fn training_export(h: &Harness, id: &str, path: &str, format: &str) -> Result<(), String> {
    let task = h.task(id).await?.lock().await.clone();
    // The cached system prompt on a task is dropped when the chat is loaded, so
    // most chats carry none; rebuild it from the project files as they are
    // now, so the export carries the instructions the agent worked under.
    let system = runner::system_prompt(h, &task).await;
    let jsonl = training_jsonl(&task, &system, format)?;
    std::fs::write(path, jsonl).map_err(|e| format!("Couldn't write {path}: {e}"))
}

/// Put many chats away at once. Returns how many were actually archived.
///
/// The bulk half of `task_update {archived: true}`. It walks the ids rather than
/// the other way round, so a stale tick can't archive a chat the user has since
/// restored, and a chat that is already archived isn't counted as work done.
/// Pinning is dropped on the way through, exactly as the single-chat patch does —
/// an archived chat is out of the sidebar, so a pin on it is a pin on nothing.
/// Paused chats are stopped as they are archived, so they no longer remain in the
/// paused list after being hidden. Archiving ordinary running chats only hides
/// them; it does not cancel them, and the transcript stays where it was. What does
/// stop is the *global* model swap, which skips archived chats — so a bulk archive mid-swap
/// takes a chat out of a change that was already in flight.
///
/// The command is a thin wrapper so the work can be tested against a plain
/// `&Harness` — `tauri::State` can't be built outside the app.
#[tauri::command]
async fn tasks_archive(h: H<'_>, ids: Vec<String>) -> Result<usize, String> {
    archive_chats(h.inner(), &ids).await
}

async fn archive_chats(h: &Harness, ids: &[String]) -> Result<usize, String> {
    let mut gone = 0usize;
    for t in summaries(h).await {
        if !ids.contains(&t.id) || t.archived {
            continue;
        }
        apply_patch(h, &t.id, json!({"archived": true})).await?;
        gone += 1;
    }
    Ok(gone)
}

/// Write the named chats to one JSON file. Returns how many went in.
///
/// `tasks_export_all` walks every chat the app knows about, which is the wrong
/// set the moment the user has ticked three of them: "export these" and "export
/// everything" are different requests, and quietly including the other 200 is
/// the sort of thing that only surfaces when they open the file. Through
/// `task()`, like the bulk export, so no chat exports as an empty stub.
#[tauri::command]
async fn tasks_export(h: H<'_>, ids: Vec<String>, path: String) -> Result<usize, String> {
    export_chats(h.inner(), &ids, &path).await
}

async fn export_chats(h: &Harness, ids: &[String], path: &str) -> Result<usize, String> {
    let mut out = vec![];
    for id in ids {
        // Ignore ids that vanished between picking them and pressing save.
        let Ok(r) = h.task(id).await else { continue };
        out.push(r.lock().await.clone());
    }
    // An empty file is not an export. The single-chat path can't reach this —
    // `task_export` has already errored on a missing id — so the check is here,
    // where ids are a list the caller may have built from a stale selection.
    if out.is_empty() {
        return Err("None of those chats are here any more".into());
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    let n = out.len();
    std::fs::write(path, export_json(out)?).map_err(|e| format!("Couldn't write {path}: {e}"))?;
    Ok(n)
}

#[tauri::command]
async fn task_export(h: H<'_>, id: String, path: String) -> Result<(), String> {
    let t = h.task(&id).await?.lock().await.clone();
    std::fs::write(&path, export_json(vec![t])?).map_err(|e| format!("Couldn't write {path}: {e}"))
}

/// Every chat except archived ones (unless asked).
#[tauri::command]
async fn tasks_export_all(h: H<'_>, path: String, include_archived: bool) -> Result<usize, String> {
    let ids: Vec<String> = {
        let refs: Vec<_> = h.tasks.read().await.values().cloned().collect();
        let mut v = vec![];
        for r in refs {
            let id = r.lock().await.id.clone();
            if include_archived || !r.lock().await.archived {
                v.push(id);
            }
        }
        v
    };
    // Through `task()`, not the map: an export has to carry every chat's real
    // subagent history, and a chat whose body is still on disk would export as
    // an empty stub — a silent data loss that only shows up when someone tries
    // to restore the backup.
    let mut out = vec![];
    for id in ids {
        let Ok(r) = h.task(&id).await else { continue };
        out.push(r.lock().await.clone());
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    let n = out.len();
    std::fs::write(&path, export_json(out)?).map_err(|e| format!("Couldn't write {path}: {e}"))?;
    Ok(n)
}

/// A fresh, stopped copy of a task under a new id (import + duplicate).
fn clone_as_new(mut t: Task) -> Task {
    t.id = agent::new_id();
    // Brought in just now, so it belongs at the top of the list.
    t.touched_at = chrono::Utc::now();
    if matches!(t.status.as_str(), "running" | "waiting") {
        t.status = "stopped".into();
    }
    t.waiting_kind = None;
    t.paused = None;
    t.pending = Default::default();
    t.subs
        .iter_mut()
        .filter(|s| s.status == "running")
        .for_each(|s| s.status = "stopped".into());
    // Paths from another machine may not exist here; the task still opens and can be read.
    t.worktree = t.worktree && std::path::Path::new(&t.cwd).is_dir();
    t
}

#[tauri::command]
async fn task_duplicate(h: H<'_>, id: String) -> Result<TaskSummary, String> {
    let mut t = clone_as_new(h.task(&id).await?.lock().await.clone());
    t.title = format!("{} (copy)", t.title);
    t.pinned = false;
    t.order = 0.0;
    t.created_at = chrono::Utc::now();
    t.updated_at = t.created_at;
    // A copy the user just made is a chat they are working in: put it on top.
    t.touched_at = t.created_at;
    store::save_task(&t);
    let s = t.summary();
    h.tasks
        .write()
        .await
        .insert(t.id.clone(), Arc::new(Mutex::new(t)));
    Ok(s)
}

/// Fork a chat. `full` copies the whole conversation, `compact` starts a fresh
/// one from a summary of it — the same summary `/compact` writes, so the fork
/// starts with the context and none of the transcript.
///
/// Both land stopped, waiting for you to say what the fork is for.
#[tauri::command]
async fn task_fork(h: H<'_>, id: String, mode: String) -> Result<TaskSummary, String> {
    let compacting = match mode.as_str() {
        "full" => false,
        "compact" => true,
        _ => return Err(format!("unknown fork mode: {mode}")),
    };
    let h = h.inner().clone();
    let t = h.task(&id).await?;
    // A chat that has said nothing yet has no history to summarise, and a fork of
    // it needs none.
    let src = t.lock().await.clone();
    let summary = if src.messages.is_empty() {
        None
    } else {
        // Summarise the snapshot, never the chat itself: `compact` rewrites the
        // task it is given, so asking it for a fork's summary would compact the
        // original out from under the user the moment they copied it.
        let c = tokio_util::sync::CancellationToken::new();
        let summary = runner::summarise(&h, &src, &c).await?;
        // A model that hands back nothing has said nothing worth carrying over.
        if summary.trim().is_empty() {
            None
        } else {
            Some(summary)
        }
    };
    let title = src.title.clone();
    let todos = fork_todos_note(&h, &id).await;
    let mut f = clone_as_new(src);
    f.forked_from = Some(id.clone());
    // A fork is an ordinary chat: it sits in the sidebar with the rest, and says
    // where it came from in the transcript.
    f.hidden = false;
    f.pinned = false;
    f.order = 0.0;
    f.todos.clear();
    f.goal = None;
    f.created_at = chrono::Utc::now();
    f.updated_at = f.created_at;
    f.touched_at = f.created_at;
    if compacting {
        // The earlier turns are gone, so what is left of them is a one-line
        // origin notice in the transcript, and the whole conversation the fork
        // actually starts from is the summary.
        f.title = fork_title(&title);
        f.messages.clear();
        f.checkpoints.clear();
        f.items = vec![Item::new(
            "notice",
            format!("Forked from {title} with its history compacted into the context below"),
            json!({"level": "fork", "from": id, "mode": "compact", "title": title, "summary": summary.clone().unwrap_or_default()}),
        )];
        f.messages.push(agent::Message::user_text(format!(
            "This chat is a fork of an earlier conversation, compacted to free up its context. Summary of everything so far:\n\n{}{}\n\nContinue the work from where it left off without asking the user to repeat anything.",
            summary.unwrap_or_else(|| "The earlier conversation was compacted, but nothing came back.".into()),
            todos,
        )));
    } else {
        // A full copy keeps the whole transcript, so the notice goes at the top of
        // it rather than replacing anything. Every checkpoint is an index into
        // `items`, so they all move down by the one row just added.
        f.items.insert(
            0,
            Item::new(
                "notice",
                format!("Forked from {title}, whole conversation copied"),
                json!({"level": "fork", "from": id, "mode": "full", "title": title, "summary": ""}),
            ),
        );
        for c in f.checkpoints.iter_mut() {
            c.item_index += 1;
        }
    }
    store::save_task(&f);
    let s = f.summary();
    h.tasks
        .write()
        .await
        .insert(f.id.clone(), Arc::new(Mutex::new(f)));
    Ok(s)
}

/// A compacted fork's title: the same words, marked as the fork they are.
fn fork_title(title: &str) -> String {
    let base = title.split(" (fork").next().unwrap_or(title).trim();
    format!("{} (fork)", if base.is_empty() { "Fork" } else { base })
}

/// The todo list a fork inherits as a note, if the chat it came from has one.
async fn fork_todos_note(h: &Harness, id: &str) -> String {
    let Ok(t) = h.task(id).await else {
        return String::new();
    };
    let todos = t.lock().await.todos.clone();
    if todos.is_empty() {
        return String::new();
    }
    format!(
        "\n\nCurrent todo list:\n{}",
        serde_json::to_string_pretty(&todos).unwrap_or_default()
    )
}

/// Stop everything a chat is doing and erase it from memory and disk. Shared by
/// the single-chat and bulk commands, so one can't grow a cleanup step the other misses.
fn purge_task(h: &Harness, id: &str, remove_worktree: bool) -> Result<(), String> {
    // The async `interrupt` can't run here (this is a blocking thread), so take
    // the same two things it would: the agent's cancellation token and every
    // background command. A deleted chat leaves nothing running behind it.
    runner::stop_tokens(h, id);
    let runtime = h.runtime(id);
    runtime.bg_cancel.lock().unwrap().cancel();
    // Git creation runs on the blocking pool and cannot be interrupted in the
    // middle of its operation. Wait for startup to persist its final workspace
    // before honoring the caller's preserve/remove choice.
    let mut startup = runtime.startup_cancel.lock().unwrap();
    if let Some(cancel) = startup.as_ref() {
        cancel.cancel();
    }
    while startup.is_some() {
        startup = runtime.startup_done.wait(startup).unwrap();
    }
    drop(startup);
    if runtime.startup_pending.swap(false, Ordering::SeqCst) {
        runtime.startup_ready.notify_waiters();
    }
    h.bg.kill_task(id);
    // A checkpoint job still running would recreate the shadow store after
    // `checkpoint::purge` below, leaving a copy of the deleted chat's files on
    // disk. Drain work already queued before removing a worktree; deletion
    // can still fail below, so do not permanently close its queue yet.
    tauri::async_runtime::block_on(runtime.checkpoints_settled());
    let task = h.tasks.blocking_read().get(id).cloned();
    if let Some(task) = task {
        let (worktree, project, cwd, docker, worker_wts) = {
            let task = task.blocking_lock();
            // Ultrathread worker worktrees belong to this chat too. Deleting the
            // chat used to leave every one of them on disk — the chat's own
            // worktree was the only one considered — so a worktree-per-worker run
            // leaked a checkout and a branch per wave, findable again only as
            // "Left behind" folders. Branches are kept, exactly as they are for
            // the chat's own worktree.
            let mut wts: Vec<String> = task
                .subs
                .iter()
                .map(|s| s.cwd.clone())
                .filter(|c| !c.is_empty() && *c != task.cwd)
                .collect();
            wts.sort();
            wts.dedup();
            (
                task.worktree,
                task.project.clone(),
                task.cwd.clone(),
                task.docker.clone(),
                wts,
            )
        };
        if let Some(d) = &docker {
            // The container is disposable either way; only the workspace on
            // disk follows the user's keep/remove choice. Docker being down
            // must not make a chat undeletable: the manager prunes orphans.
            let _ = tauri::async_runtime::block_on(agent::docker::remove(id, d));
            if remove_worktree {
                remove_docker_workspace(&d.host_root)?;
            }
        }
        if remove_worktree && worktree {
            // Do not erase the chat's recovery information until Git confirms
            // removal. The caller can retry or inspect the worktree on failure.
            git::remove_worktree(&project, &cwd)?;
        }
        if remove_worktree {
            // Same rule as the chat's own worktree: a failure is reported rather
            // than swallowed, so a worker checkout that could not go is named and
            // the manager can still prune it. Every path is attempted first, so
            // one bad worktree does not leave the rest behind. A path that is
            // already gone is not a failure — there is nothing to remove.
            //
            // Each checkout is removed through the repository that owns it, not
            // through `project`: a docker workspace is its own clone, so its
            // workers' checkouts belong to that clone, and naming the original
            // project would fail and make the chat undeletable.
            let failed: Vec<String> = worker_wts
                .iter()
                .filter(|p| std::path::Path::new(p).is_dir())
                .filter(|p| git::remove_worktree_at(p).is_err())
                .cloned()
                .collect();
            if !failed.is_empty() {
                return Err(format!(
                    "could not remove these worker worktrees: {}",
                    failed.join(", ")
                ));
            }
        }
        let _ = agent::attachments::remove_task(&project, id);
        tauri::async_runtime::block_on(agent::attachments::remove_remote_task(&project, id));
    }
    // Cleanup succeeded: fence out late end-of-run captures and drain again
    // before removing the checkpoint store. A failed worktree removal above
    // leaves the task retryable, including its ability to take checkpoints.
    runtime.close_checkpoints();
    tauri::async_runtime::block_on(runtime.checkpoints_settled());
    h.tasks.blocking_write().remove(id);
    // Safe now and only now: the tokens are cancelled and the background
    // processes are dead, so nothing can still be holding this chat's runtime.
    h.forget_runtime(id);
    // Their entries as well as their processes: the map is keyed independently
    // of the runtime and nothing else ever removes a deleted chat's jobs.
    h.bg.forget_task(id);
    store::delete_task(id);
    store::save_draft(id, "");
    // The shadow store holds a full copy of that chat's files; leaving it would
    // keep a deleted chat's contents on disk indefinitely.
    checkpoint::purge(id);
    // Its proof clips and visual-diff screenshots are pictures of that chat's
    // work; they go with it.
    agent::proof::forget_task(id);
    // Its stats do not go with it. The spend is real whether the chat is or
    // not, and a usage ledger that shrinks as you tidy up stops adding up.
    Ok(())
}

#[tauri::command]
async fn task_delete(h: H<'_>, id: String, remove_worktree: bool) -> Result<(), String> {
    let h = h.inner().clone();
    // Purging runs git and file IO, which must not block the async runtime.
    tokio::task::spawn_blocking(move || purge_task(&h, &id, remove_worktree))
        .await
        .map_err(|e| e.to_string())?
}

/// Delete every archived chat — from memory, from disk and its draft — and
/// leave the active ones untouched. Returns how many went away.
fn delete_archived(h: &Harness) -> Result<usize, String> {
    let ids: Vec<String> = {
        let refs: Vec<_> = h.tasks.blocking_read().values().cloned().collect();
        let mut ids = vec![];
        for r in refs {
            let t = r.blocking_lock();
            if t.archived {
                ids.push(t.id.clone());
            }
        }
        ids
    };
    let mut removed = 0usize;
    let mut purged = std::collections::HashSet::new();
    for id in &ids {
        // This cleanup has no interactive confirmation; preserve the files by
        // detaching the chat but leaving its worktree registered on disk.
        //
        // Counted per chat rather than reporting `ids.len()`: a purge that fails
        // leaves the chat in place, and the caller puts this number in front of
        // the user ("Deleted 3 archived chats"), so it has to mean "gone".
        match purge_task(h, id, false) {
            Ok(()) => {
                removed += 1;
                purged.insert(id.clone());
            }
            Err(error) => eprintln!("[openleash] could not delete archived chat {id}: {error}"),
        }
    }
    // An archived race whose chats are all gone has nothing left to restore.
    if let Ok(dropped) = agent::race::prune_gone(&purged) {
        for rid in dropped {
            agent::race::emit_removed(h, &rid);
        }
    }
    Ok(removed)
}

#[tauri::command]
async fn tasks_delete_archived(h: H<'_>) -> Result<usize, String> {
    let h = h.inner().clone();
    tokio::task::spawn_blocking(move || delete_archived(&h))
        .await
        .map_err(|e| e.to_string())?
}

/// Take one queued message out of the queue: it was never sent, so the row and
/// the timeline item that stood for it both disappear.
///
/// The command is a thin wrapper so the queue work can be tested against a plain
/// `&Harness` — `tauri::State` can't be built outside the app.
#[tauri::command]
async fn task_queue_remove(h: H<'_>, id: String, item_id: String) -> Result<(), String> {
    queue_remove(h.inner(), &id, &item_id).await
}

async fn queue_remove(h: &Harness, id: &str, item_id: &str) -> Result<(), String> {
    let rt = h.runtime(id);
    let mut q = rt.queue.lock().await;
    let before = q.len();
    q.retain(|m| m.item_id != item_id);
    let taken = q.len() != before;
    drop(q);
    if taken {
        drop_item(h, id, item_id).await;
    }
    Ok(())
}

/// Replace a queued message with edited text, keeping its place in the order.
/// Images on the message are kept: editing the words doesn't mean discarding
/// the picture that came with them.
#[tauri::command]
async fn task_queue_edit(
    h: H<'_>,
    id: String,
    item_id: String,
    text: String,
) -> Result<(), String> {
    queue_edit(h.inner(), &id, &item_id, &text).await
}

async fn queue_edit(h: &Harness, id: &str, item_id: &str, text: &str) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return queue_remove(h, id, item_id).await;
    }
    let rt = h.runtime(id);
    let mut found = false;
    {
        let mut q = rt.queue.lock().await;
        if let Some(m) = q.iter_mut().find(|m| m.item_id == item_id) {
            // Keep the images, swap the words: the text block is the one the
            // user is editing, and it goes first so the agent reads it first.
            let images: Vec<Value> = m
                .blocks
                .iter()
                .filter(|b| b["type"] != "text")
                .cloned()
                .collect();
            m.blocks = [vec![json!({"type": "text", "text": text.clone()})], images].concat();
            found = true;
        }
    }
    if !found {
        return Err("That message is no longer waiting.".into());
    }
    h.patch_item(id, item_id, |i| i.text = text).await;
    Ok(())
}

/// Move a queued message to a new position. `before` is the id of the message it
/// should sit in front of, or None to put it last — so the top of the list is
/// reachable by naming whatever is currently first. Sequence numbers are
/// rewritten so the queue and the list above the composer agree on the order.
#[tauri::command]
async fn task_queue_move(
    h: H<'_>,
    id: String,
    item_id: String,
    before: Option<String>,
) -> Result<(), String> {
    queue_move(h.inner(), &id, &item_id, before.as_deref()).await
}

async fn queue_move(
    h: &Harness,
    id: &str,
    item_id: &str,
    before: Option<&str>,
) -> Result<(), String> {
    let rt = h.runtime(id);
    let mut found = false;
    {
        let mut q = rt.queue.lock().await;
        if let Some(from) = q.iter().position(|m| m.item_id == item_id) {
            let m = q.remove(from);
            let at = before
                .and_then(|b| q.iter().position(|o| o.item_id == b))
                .unwrap_or(q.len());
            q.insert(at, m);
            found = true;
        }
    }
    if !found {
        return Err("That message is no longer waiting.".into());
    }
    // One seq per position: renumbering in place is what makes the list jump
    // the way the row was dropped, rather than to the end.
    let ordered: Vec<(String, u64)> = {
        let mut q = rt.queue.lock().await;
        let base = rt.send_seq.load(Ordering::SeqCst);
        for (i, m) in q.iter_mut().enumerate() {
            m.seq = base + i as u64;
        }
        q.iter().map(|m| (m.item_id.clone(), m.seq)).collect()
    };
    for (iid, seq) in ordered {
        h.patch_item(id, &iid, |i| i.data["seq"] = json!(seq)).await;
    }
    Ok(())
}

/// Send one queued message now, without disturbing the rest of the queue.
///
/// A run in flight picks it up on its very next model call, the way any message
/// typed mid-turn steers. With nothing running there is no turn to steer, so it
/// becomes an ordinary user turn and starts one.
#[tauri::command]
async fn task_queue_send_now(h: H<'_>, id: String, item_id: String) -> Result<(), String> {
    queue_send_now(h.inner().clone(), &id, &item_id).await
}

async fn queue_send_now(h: Arc<Harness>, id: &str, item_id: &str) -> Result<(), String> {
    let rt = h.runtime(id);
    let Some(m) = rt
        .queue
        .lock()
        .await
        .iter()
        .find(|m| m.item_id == item_id)
        .cloned()
    else {
        return Err("That message is no longer waiting.".into());
    };
    if rt.running.load(std::sync::atomic::Ordering::SeqCst) {
        // A run is in flight, so this steers: the message goes to the main
        // agent's inbox, which the next model call picks up as a reminder.
        // That is what "send it now" means, and it leaves the rest of the
        // list waiting.
        let m = {
            let mut q = rt.queue.lock().await;
            let Some(at) = q.iter().position(|o| o.item_id == item_id) else {
                return Ok(());
            };
            q.remove(at)
        };
        let text = m
            .blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        runner::steer_now(&h, id, &text).await;
        let imgs: Vec<(String, String)> = m
            .blocks
            .iter()
            .filter(|b| b["type"] == "image")
            .filter_map(|b| {
                Some((
                    b["source"]["media_type"].as_str()?.to_string(),
                    b["source"]["data"].as_str()?.to_string(),
                ))
            })
            .collect();
        h.note_images(id, "main", imgs).await;
        runner::mark_delivered(&h, id, &[m]).await;
        return Ok(());
    }
    // Nothing running: there is no turn to steer, so it becomes an ordinary
    // user turn and starts one.
    let blocks = m.blocks.clone();
    {
        let mut q = rt.queue.lock().await;
        q.retain(|o| o.item_id != item_id);
    }
    if let Ok(t) = h.task(id).await {
        let mut t = t.lock().await;
        let cp = Checkpoint {
            item_index: t.items.len(),
            msg_index: t.messages.len(),
        };
        t.checkpoints.push(cp);
        runner::push_user_blocks(&mut t.messages, blocks);
    }
    runner::mark_delivered(&h, id, &[m]).await;
    runner::spawn_run(h.clone(), id.to_string());
    Ok(())
}

/// Send everything the queue is holding, in the order it is held.
#[tauri::command]
async fn task_queue_send_all(h: H<'_>, id: String) -> Result<(), String> {
    queue_send_all(h.inner().clone(), &id).await
}

async fn queue_send_all(h: Arc<Harness>, id: &str) -> Result<(), String> {
    let rt = h.runtime(id);
    if rt.running.load(std::sync::atomic::Ordering::SeqCst) {
        // Let the run in flight read the whole queue, in order.
        return Ok(());
    }
    let all = runner::drain_queue(&rt).await;
    if all.is_empty() {
        return Ok(());
    }
    let blocks: Vec<Value> = all.iter().flat_map(|m| m.blocks.iter().cloned()).collect();
    if let Ok(t) = h.task(id).await {
        let mut t = t.lock().await;
        let cp = Checkpoint {
            item_index: t.items.len(),
            msg_index: t.messages.len(),
        };
        t.checkpoints.push(cp);
        runner::push_user_blocks(&mut t.messages, blocks);
    }
    runner::mark_delivered(&h, id, &all).await;
    runner::spawn_run(h.clone(), id.to_string());
    Ok(())
}

/// Remove a timeline item that never reached the agent, and tell the UI.
async fn drop_item(h: &Harness, id: &str, item_id: &str) {
    if let Ok(t) = h.task(id).await {
        t.lock().await.items.retain(|i| i.id != item_id);
    }
    h.emit(id, "itemgone", json!({"id": item_id}));
}

/// Rewind to just before a user message: drop later history and return the
/// message text for editing. File changes are left alone on disk — the
/// review view (`task_review` / `task_revert_file`) is where files get undone.
#[tauri::command]
async fn task_rewind(h: H<'_>, id: String, item_id: String) -> Result<String, String> {
    if h.runtime(&id)
        .running
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("Stop the agent before rewinding.".into());
    }
    let t = h.task(&id).await?;
    let mut t = t.lock().await;
    let text = rewind_task(&mut t, item_id)?;
    // Everything still in flight when the agent stopped — typed while it was
    // working, or Alt-Enter, or a photo pasted into the running chat — lived only
    // in the runtime, never in `messages`. Discard it with the rest of the future:
    // a run is about to start from the rewind point, and the tail of these queues
    // is now a conversation that no longer exists. (Inside the lock, because a run
    // reads them on its next iteration: clearing them after the release would leave
    // a window where the agent could still pick one up.)
    h.runtime(&id).drop_undelivered().await;
    h.emit_summary(&t);
    store::save_task(&t);
    Ok(text)
}

/// Every checkpoint of a chat that can be rewound to, with what each one
/// changed on disk.
///
/// The file list per checkpoint is a diff between two *snapshots*, not between
/// the checkpoint and the task's base: `target(k)` is the folder before prompt
/// `k`, and `target(k+1)` (or the working tree, for the newest) is the folder
/// after it. That is the question the picker is asking — "what would this step
/// undo?" — and it stays right when a turn's own edits are what a later step
/// built on.
#[tauri::command]
async fn task_rewind_info(h: H<'_>, id: String) -> Result<Value, String> {
    let t = h.task(&id).await?;
    // The send's user snapshot may still be committing in the background; the
    // ledger read below is only complete once it has.
    h.runtime(&id).checkpoints_settled().await;
    let (cwd, marks, in_worktree, tools) = {
        let t = t.lock().await;
        let marks: Vec<(String, usize, usize)> = t
            .checkpoints
            .iter()
            .filter_map(|c| {
                let it = t.items.get(c.item_index)?;
                Some((it.id.clone(), c.item_index, c.msg_index))
            })
            .collect();
        // The items themselves, so the picker can show the prompt text and its
        // time. Read here rather than by index later: the list is truncated
        // between this call and the next, and an index is not a stable handle.
        let items: Vec<(String, String, String)> = t
            .items
            .iter()
            .map(|i| (i.id.clone(), i.text.clone(), i.ts.to_rfc3339()))
            .collect();
        (
            t.cwd.clone(),
            marks,
            t.worktree,
            (t.id.clone(), items, t.checkpoints.len()),
        )
    };
    let (chat, items, total) = tools;
    let info = tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        for (k, (item_id, _item_index, _msg_index)) in marks.iter().enumerate() {
            // A checkpoint with no snapshot behind it is still a step the
            // conversation can be rewound to — it just has nothing to put back
            // on disk, and `has_files` is what hides the file options. Dropping
            // the row instead would take a valid rewind target out of the
            // picker for no reason.
            let from = checkpoint::target(&chat, k);
            let files = match &from {
                // The state after this prompt: the next checkpoint's own
                // snapshot, or the live folder for the newest one.
                Some(from) => match checkpoint::target(&chat, k + 1) {
                    Some(to) if k + 1 != total => checkpoint::changed(&chat, from, &to),
                    _ => checkpoint::changed_now(&chat, &cwd, from),
                },
                None => vec![],
            };
            let (text, ts) = items
                .iter()
                .find(|(iid, _, _)| iid == item_id)
                .map(|(_, t, ts)| (t.clone(), ts.clone()))
                .unwrap_or_default();
            let label = checkpoint::label(&ts, &files);
            out.push(json!({
                "item_id": item_id,
                "text": text,
                "ts": ts,
                "label": label,
                "files": files,
                "has_files": from.is_some(),
                "current": k + 1 == total,
            }));
        }
        out
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(json!({"checkpoints": info, "in_worktree": in_worktree}))
}

/// The three-way rewind. `mode` is `files` | `conversation` | `both`.
///
/// Files first, files only *if the user's own edits are safe* — see
/// `checkpoint::restore`, which skips any file whose content is not what the
/// agent last wrote. The count of those is returned, because a rewind that
/// silently dropped a manual edit would be the worst possible failure for a
/// tool whose pitch is safety.
#[tauri::command]
async fn task_rewind_to(
    h: H<'_>,
    id: String,
    item_id: String,
    mode: String,
) -> Result<Value, String> {
    rewind_to(h.inner(), id, item_id, mode).await
}

/// The body of `task_rewind_to`, on a plain `&Harness` so it can be tested
/// without the app — `H<'_>` is a Tauri `State`, which only the app can build.
pub(crate) async fn rewind_to(
    h: &Harness,
    id: String,
    item_id: String,
    mode: String,
) -> Result<Value, String> {
    let want_files = matches!(mode.as_str(), "files" | "both");
    let want_conv = matches!(mode.as_str(), "conversation" | "both");
    if !want_files && !want_conv {
        return Err(format!("unknown rewind mode: {mode}"));
    }
    if h.runtime(&id)
        .running
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("Stop the agent before rewinding.".into());
    }
    // A user snapshot still committing in the background would land after the
    // truncation below, putting a marker from the abandoned branch back.
    h.runtime(&id).checkpoints_settled().await;
    let t = h.task(&id).await?;
    let (chat, cwd, k, text) = {
        let t = t.lock().await;
        let idx = t
            .items
            .iter()
            .position(|i| i.id == item_id)
            .ok_or("message not found")?;
        let (k, _, _) = rewind_point(&t, idx);
        (t.id.clone(), t.cwd.clone(), k, t.items[idx].text.clone())
    };
    // The snapshot is read before the truncation below, because the truncation
    // is what drops the checkpoint this is asking about.
    let target = k.and_then(|k| checkpoint::target(&chat, k));
    let mut restored = checkpoint::Restored::default();
    if want_files {
        match target.clone() {
            Some(commit) => {
                let paths = {
                    let c = chat.clone();
                    let w = cwd.clone();
                    let cm = commit.clone();
                    tokio::task::spawn_blocking(move || checkpoint::candidates(&c, &w, &cm))
                        .await
                        .map_err(|e| e.to_string())?
                };
                let (c, w, cm) = (chat.clone(), cwd.clone(), commit.clone());
                restored = tokio::task::spawn_blocking(move || checkpoint::restore(&c, &w, &cm, &paths))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            None => {
                return Err("This chat has no file snapshot for that step — there is nothing on disk to put back.".into())
            }
        }
    }
    {
        let mut t = t.lock().await;
        if want_conv {
            rewind_task(&mut t, item_id)?;
        }
        // Even files-only: the conversation may continue from here, and the
        // snapshots past this point described a branch that no longer exists.
        let keep = t.checkpoints.len();
        checkpoint::truncate(&chat, keep);
    }
    // Re-snapshot at the post-rewind file state so the *next* prompt's
    // checkpoint restores to where we just put the files, and not to whatever
    // the branch we abandoned had made. A user snapshot: a restore may have
    // written the agent's own content back, but it is not the agent acting, and
    // the ledger the restore just updated already says what the agent's content
    // is.
    if want_files {
        let enabled = h.settings.read().await.checkpoints;
        let snap = {
            let t = t.lock().await;
            checkpoint::Snapshot::of(&t, enabled)
        };
        if let Some(snap) = snap {
            // Through the chain, so a send that raced this rewind cannot have
            // its own snapshot recorded ahead of this one.
            let mut done = h.runtime(&id).queue_checkpoint(move || {
                snap.user().take();
            });
            let _ = done.wait_for(|d| *d).await;
        }
    }
    let t = h.task(&id).await?;
    {
        let t = t.lock().await;
        h.runtime(&id).drop_undelivered().await;
        h.emit_summary(&t);
        store::save_task(&t);
    }
    Ok(json!({
        "restored": restored.restored,
        "skipped": restored.skipped,
        "text": text,
    }))
}

/// Branch a chat at one of its checkpoints into a fresh git worktree.
///
/// Distinct from a rewind: a rewind changes this chat, a fork leaves it exactly
/// as it is and starts a second one. The new worktree is created the normal way
/// (a new `ol/<slug>` branch off the task's base) and then overwritten with the
/// checkpoint's snapshot, so it is a real sibling checkout of the project
/// rather than a second copy of our shadow store — the user can commit, diff
/// and merge it like any other.
#[tauri::command]
async fn task_fork_at(h: H<'_>, id: String, item_id: String) -> Result<TaskSummary, String> {
    fork_at(h.inner(), id, item_id).await
}

/// The body of `task_fork_at`, on a plain `&Harness` so it can be tested
/// without the app — `H<'_>` is a Tauri `State`, which only the app can build.
pub(crate) async fn fork_at(
    h: &Harness,
    id: String,
    item_id: String,
) -> Result<TaskSummary, String> {
    let t = h.task(&id).await?;
    // The ledger `target` reads below is complete only once any background
    // user snapshot has committed.
    h.runtime(&id).checkpoints_settled().await;
    let mut src = t.lock().await.clone();
    let idx = src
        .items
        .iter()
        .position(|i| i.id == item_id)
        .ok_or("message not found")?;
    let k = src
        .checkpoints
        .iter()
        .position(|c| c.item_index == idx)
        .ok_or("Can't fork at this message (it was sent mid-turn or before a compaction).")?;
    let msg_index = src.checkpoints[k].msg_index;
    let chat = src.id.clone();
    let target = checkpoint::target(&chat, k)
        .ok_or("This chat has no file snapshot at that step, so there is no state to fork from.")?;
    let base = if src.base_branch.is_empty() {
        src.branch.clone()
    } else {
        src.base_branch.clone()
    };
    let project = src.project.clone();
    let slug = git::slug(&format!("{}-{}", src.title, idx));
    let base_for_wt = base.clone();
    let (dir, branch) =
        tokio::task::spawn_blocking(move || git::create_worktree(&project, &base_for_wt, &slug))
            .await
            .map_err(|e| e.to_string())??;
    // Materialise the checkpoint's tree into the fresh checkout, then record
    // what it did so the fork's own review view starts from an honest base.
    let (c, cm, d) = (chat.clone(), target, dir.clone());
    let copied = tokio::task::spawn_blocking(move || checkpoint::materialize(&c, &cm, &d))
        .await
        .map_err(|e| {
            format!(
                "Created the worktree at {dir} on branch {branch}, but checkpoint setup stopped: {e}. Inspect or remove that worktree manually."
            )
        })?;
    if let Err(e) = copied {
        // Materialisation may have partially written files; preserve the checkout
        // and give the user its exact recovery location rather than cleaning it up.
        return Err(format!(
            "Created the worktree at {dir} on branch {branch}, but could not put the checkpoint files in it: {e}. Inspect or remove that worktree manually."
        ));
    }
    let hooks = {
        let settings = h.settings.read().await;
        agent::checks::hooks_for(&settings, &src.project)
    };
    let setup = agent::checks::setup_worktree(
        &hooks,
        &src.project,
        &dir,
        &id,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    let hook_failures: Vec<String> = setup
        .hooks
        .iter()
        .filter(|hook| !hook.ok)
        .map(|hook| format!("`{}`: {}", hook.command, hook.output))
        .collect();
    let setup_issues: Vec<String> = setup
        .copy_error
        .into_iter()
        .map(|error| format!("included-file copy failed: {error}"))
        .chain(
            hook_failures
                .into_iter()
                .map(|error| format!("setup hook failed: {error}")),
        )
        .collect();
    // The conversation up to — and not including — the message being branched
    // at, exactly like a rewind of the transcript, so the fork starts where the
    // user asked rather than repeating a prompt that already has a reply.
    // The run timeline is cut at the same moment, so the fork's does not show
    // work from the part of the conversation it never had.
    if let Some(cut) = src.items.get(idx).map(|i| i.ts) {
        src.spans.retain(|s| s.start < cut);
    }
    src.items.truncate(idx);
    src.messages.truncate(msg_index);
    src.checkpoints.truncate(k);
    src.read_files.clear();
    src.read_hashes.clear();
    src.todos.clear();
    src.sub_msgs.clear();
    src.status = "idle".into();
    src.paused = None;
    src.unpaused = true;
    src.stop_note = None;
    src.wrap_up = false;
    src.summarise_first = false;
    src.told.clear();
    src.usage.last_context = 0;
    // Same reason `last_context` is cleared: a fork/branch that drops history
    // must not still draw the parent's pre-truncation breakdown.
    src.usage.parts = Default::default();
    let title = src.title.clone();
    let mut f = clone_as_new(src);
    f.forked_from = Some(id.clone());
    f.hidden = false;
    f.pinned = false;
    f.order = 0.0;
    f.worktree = true;
    // A fork is a worktree of the project; it must not share the source
    // chat's container (or delete it when the fork is deleted).
    f.docker = None;
    f.cwd = dir;
    f.branch = branch;
    f.base_branch = base;
    f.base_commit = git::head(&f.cwd);
    f.step = "Forked · waiting for you".into();
    f.items.insert(
        0,
        Item::new(
            "notice",
            format!(
                "Forked from \"{title}\" at message {}, with the files as they were at that point",
                idx + 1
            ),
            json!({"level": "fork", "from": id, "mode": "checkpoint", "title": title, "summary": ""}),
        ),
    );
    if !setup_issues.is_empty() {
        f.items.insert(
            1,
            Item::new(
                "notice",
                format!(
                    "The forked worktree at {} was created, but setup did not finish cleanly: {}",
                    f.cwd,
                    setup_issues.join("; ")
                ),
                json!({"level": "info"}),
            ),
        );
    }
    // The notice is a row in `items`, so every checkpoint below it moves down
    // one — the same shift `task_fork`'s full copy does.
    for c in f.checkpoints.iter_mut() {
        c.item_index += 1 + usize::from(!setup_issues.is_empty());
    }
    store::save_task(&f);
    let s = f.summary();
    h.tasks
        .write()
        .await
        .insert(f.id.clone(), Arc::new(Mutex::new(f)));
    Ok(s)
}

/// Where a rewind to the item at `idx` cuts: (checkpoint to restore files from,
/// items kept, messages kept, checkpoints kept).
///
/// A message with its own checkpoint cuts exactly before itself. One without —
/// sent mid-turn (it steered the running agent, so it never started a turn), or
/// after a compaction wiped the checkpoints — used to be refused outright, which
/// made most of a chat un-editable. It now falls back to the turn it was sent
/// in: that turn's prompt is kept and everything after it dropped, items and
/// history together so the transcript never shows work the model has forgotten.
/// With no earlier checkpoint at all (compacted away) only the compaction
/// summary is kept, and there is no file snapshot to restore.
fn rewind_cut(t: &Task, idx: usize) -> (Option<usize>, usize, usize, usize) {
    if let Some(k) = t.checkpoints.iter().position(|c| c.item_index == idx) {
        return (Some(k), idx, t.checkpoints[k].msg_index, k);
    }
    match t.checkpoints.iter().rposition(|c| c.item_index < idx) {
        Some(k) => {
            let c = &t.checkpoints[k];
            let msgs = (c.msg_index + 1).min(t.messages.len());
            (Some(k), (c.item_index + 1).min(idx), msgs, k + 1)
        }
        None => (None, idx, 1.min(t.messages.len()), 0),
    }
}

fn rewind_point(t: &Task, idx: usize) -> (Option<usize>, usize, usize) {
    let (k, i, m, _) = rewind_cut(t, idx);
    (k, i, m)
}

/// The rewind itself, on an already-locked task. Conversation only: the
/// working tree is deliberately left as it is.
fn rewind_task(t: &mut Task, item_id: String) -> Result<String, String> {
    let idx = t
        .items
        .iter()
        .position(|i| i.id == item_id)
        .ok_or("message not found")?;
    let (_, item_cut, msg_cut, keep) = rewind_cut(t, idx);
    let text = t.items[idx].text.clone();
    t.items.truncate(item_cut);
    t.messages.truncate(msg_cut);
    t.checkpoints.truncate(keep);
    t.read_files.clear();
    t.read_hashes.clear();
    t.todos.clear();
    t.status = "idle".into();
    // The freeze goes with the future: this rewinds the chat to before that
    // point, so a pause set after it is part of what gets discarded. Left in
    // place it keeps a re-sent message from ever being answered — the
    // transcript shows it, and the agent never wakes.
    t.paused = None;
    t.unpaused = true;
    // Everything below is owed *to a turn that no longer exists*. The truncation
    // above only reaches `items`/`messages`/`checkpoints`; each of these is a
    // separate channel into the next request, so a rewind that left them behind
    // hands the model a conversation the user just deleted.
    //
    // `stop_note` is the obvious one: it is written when a run stops and read by
    // the next request, so rewinding a stopped chat and pressing Send told the
    // agent "The user STOPPED your previous run … check the state and retry" about
    // a turn the rewind had just thrown away. `wrap_up` is the same note with a
    // different instruction ("don't continue, summarise"), so "wrap up" then
    // edit-then-send asked for a summary of work that is no longer in context.
    t.stop_note = None;
    t.wrap_up = false;
    t.summarise_first = false;
    // `told` records what the model has already been told about (assist mode,
    // subagent set, memory), so those get announced as changes. It
    // survives a rewind deliberately for settings, which outlive any branch — but
    // after a rewind the branch is new, so the announcements are owed again.
    t.told.clear();
    // A subagent's saved transcript continues a `task` tool call whose parent
    // block was just discarded. `resume_subs` would splice its report back into
    // history at an index computed from messages that are gone, shifting every
    // checkpoint below it — a second, invisible misalignment on top of the one
    // the rewind just fixed.
    t.sub_msgs.clear();
    // Drives the compaction trigger (`last_context > window * 8/10`) and the
    // context meter. The transcript just got shorter, so carrying the old number
    // forward makes the next iteration compact a conversation that was
    // deliberately truncated back to a few messages — and compaction clears the
    // checkpoints, which would make the branch the user just rewound to
    // unrewindable.
    t.usage.last_context = 0;
    // Cleared with it, for the same reason: the breakdown described the longer
    // transcript that the rewind just threw away.
    t.usage.parts = Default::default();
    t.step = "Rewound · files kept".into();
    Ok(text)
}

/// The chat's before/after screenshots for Review. Empty unless the agent took
/// a `baseline`: nothing here launches a browser.
#[tauri::command]
async fn task_visual_diff(h: H<'_>, id: String) -> Result<agent::proof::VisualDiff, String> {
    h.task(&id).await?;
    Ok(agent::proof::load_visual(&id))
}

/// Re-take every "after" picture, on the user's click in Review.
#[tauri::command]
async fn task_visual_recapture(h: H<'_>, id: String) -> Result<agent::proof::VisualDiff, String> {
    visual_recapture_for(&h, &id).await
}

/// A fresh headless browser of its own rather than the agent's session: the
/// user pressing a button in Review must not navigate a page the agent is in
/// the middle of using. Same plugin toggle and same throwaway-profile browser
/// as the agent's, and only the http(s) URLs the store already validated.
async fn visual_recapture_for(h: &Harness, id: &str) -> Result<agent::proof::VisualDiff, String> {
    h.task(id).await?;
    let cfg = h.settings.read().await.plugins.browser.clone();
    if !cfg.enabled {
        return Err("The browser plugin is turned off (Settings → Plugins), so the after pictures can't be retaken.".into());
    }
    let entries = agent::proof::load_visual(id).entries;
    if entries.is_empty() {
        return Ok(agent::proof::VisualDiff::default());
    }
    let owner = format!("visual-recapture:{id}:{}", uuid::Uuid::new_v4());
    let session = agent::browser::session_for(
        &owner,
        if cfg.width == 0 { 1280 } else { cfg.width },
        if cfg.height == 0 { 800 } else { cfg.height },
    )
    .await?;
    let mut failed = vec![];
    for e in entries {
        let shot = async {
            // The manifest is a file on disk: re-check what it names before
            // pointing a browser at it.
            let url = agent::proof::validate_visual_url(&e.url)?;
            session.goto(&url).await?;
            let png = session.screenshot(false).await?;
            agent::proof::save_after(id, &e.url, &png)
        }
        .await;
        if let Err(err) = shot {
            failed.push(format!("{}: {err}", e.url));
        }
    }
    drop(session);
    agent::browser::close(&owner);
    if !failed.is_empty() {
        return Err(format!("Couldn't recapture {}", failed.join("; ")));
    }
    Ok(agent::proof::load_visual(id))
}

#[tauri::command]
async fn task_visual_clear(h: H<'_>, id: String) -> Result<(), String> {
    h.task(&id).await?;
    agent::proof::clear_visual(&id);
    Ok(())
}

#[tauri::command]
async fn task_review(h: H<'_>, id: String) -> Result<Value, String> {
    let t = h.task(&id).await?;
    let (cwd, base, touched) = {
        let t = t.lock().await;
        (t.cwd.clone(), t.base_commit.clone(), t.touched.clone())
    };
    // `git diff` of the whole change set plus every untracked file's text: on
    // the blocking pool, never a runtime worker.
    if let Some(base) = base {
        let c = cwd.clone();
        let files = tokio::task::spawn_blocking(move || {
            git::is_repo(&c).then(|| git::review(&c, &base)).transpose()
        })
        .await
        .map_err(|e| e.to_string())??;
        if let Some(files) = files {
            return Ok(json!({"git": true, "files": files}));
        }
    }
    Ok(json!({"git": false, "files": snapshot_review(&cwd, touched).await}))
}

/// Just the totals of `task_review` — files changed, lines added and removed —
/// for the session header, which shows nothing else. The full review builds
/// and ships every diff line; this is `git diff --numstat` against the same
/// base (see `git::review_stats`), off the async runtime.
#[tauri::command]
async fn task_review_stats(h: H<'_>, id: String) -> Result<git::ReviewStats, String> {
    review_stats(h.inner(), &id).await
}

/// The body of `task_review_stats`, on a plain `&Harness` for tests.
pub(crate) async fn review_stats(h: &Harness, id: &str) -> Result<git::ReviewStats, String> {
    let t = h.task(id).await?;
    let (cwd, base, touched) = {
        let t = t.lock().await;
        (t.cwd.clone(), t.base_commit.clone(), t.touched.clone())
    };
    if let Some(base) = base {
        let c = cwd.clone();
        let stats = tokio::task::spawn_blocking(move || {
            git::is_repo(&c)
                .then(|| git::review_stats(&c, &base))
                .transpose()
        })
        .await
        .map_err(|e| e.to_string())??;
        if let Some(stats) = stats {
            return Ok(stats);
        }
    }
    // No git: the same per-file snapshot diff the review screen shows, summed.
    let files = snapshot_review(&cwd, touched).await;
    Ok(git::ReviewStats {
        files: files.len(),
        add: files.iter().filter_map(|f| f["add"].as_u64()).sum::<u64>() as usize,
        del: files.iter().filter_map(|f| f["del"].as_u64()).sum::<u64>() as usize,
    })
}

/// No git: diff against the snapshots taken before the agent's first edit.
async fn snapshot_review(
    cwd: &str,
    touched: std::collections::HashMap<String, Option<String>>,
) -> Vec<Value> {
    let cwd = cwd.to_string();
    tokio::task::spawn_blocking(move || snapshot_review_blocking(&cwd, touched))
        .await
        .expect("snapshot review worker failed")
}

fn snapshot_review_blocking(
    cwd: &str,
    touched: std::collections::HashMap<String, Option<String>>,
) -> Vec<Value> {
    let mut files = vec![];
    for (path, original) in touched {
        let now = match agent::host::parse_ssh(&path) {
            // A remote chat's snapshot keys are `ssh://` realpaths. The bridge
            // drives the shared SSH runtime from this blocking worker.
            Some(p) => agent::ssh_session::block_on(agent::ssh_session::read(
                &p.host,
                &p.path,
                agent::tools_remote::MAX_TEXT_BYTES,
            ))
            .ok()
            .and_then(|(b, _)| String::from_utf8(b).ok()),
            None if agent::host::is_ssh(&path) => None,
            None => std::fs::read_to_string(&path).ok(),
        };
        if now == original {
            continue;
        }
        let (lines, add, del) = agent::tools::diff_lines(
            original.as_deref().unwrap_or(""),
            now.as_deref().unwrap_or(""),
        );
        let status = match (&original, &now) {
            (None, _) => "A",
            (_, None) => "D",
            _ => "M",
        };
        files.push(json!({"path": agent::permissions::short(&path, cwd), "abs": path, "status": status, "add": add, "del": del, "lines": lines}));
    }
    files
}

#[tauri::command]
async fn task_revert_file(h: H<'_>, id: String, path: String) -> Result<(), String> {
    let t = h.task(&id).await?;
    let t = t.lock().await;
    if let Some(base) = t.base_commit.clone().filter(|_| git::is_repo(&t.cwd)) {
        return git::revert_file(&t.cwd, &base, &path);
    }
    if agent::host::is_ssh(&t.cwd) {
        // Only a file the agent wrote (so a key in `touched`, which is the
        // realpath it checked and wrote) can be put back.
        let p = agent::tools_remote::resolve(&t.cwd, &path)?;
        let real = agent::tools_remote::real(&p).await?;
        return match t.touched.get(&real.uri()) {
            Some(Some(orig)) => {
                agent::ssh_session::write(&real.host, &real.path, orig.as_bytes()).await
            }
            Some(None) => agent::ssh_session::remove_file(&real.host, &real.path).await,
            None => Err("No snapshot for this file.".into()),
        };
    }
    let abs = agent::tools::resolve(&t.cwd, &path);
    let key = agent::tools::path_key(&abs);
    match t
        .touched
        .get(&key)
        .or_else(|| t.touched.get(&abs.to_string_lossy().to_string()))
    {
        Some(Some(orig)) => std::fs::write(&abs, orig).map_err(|e| e.to_string()),
        Some(None) => std::fs::remove_file(&abs).map_err(|e| e.to_string()),
        None => Err("No snapshot for this file.".into()),
    }
}

/// The chat's bisect timeline, for the Review view's "Find breaking edit…"
/// dialog, and whether a bisect can run here at all.
#[tauri::command]
async fn task_bisect_edits(h: H<'_>, id: String) -> Result<Value, String> {
    bisect_edits_info(h.inner(), &id).await
}

pub(crate) async fn bisect_edits_info(h: &Harness, id: &str) -> Result<Value, String> {
    let t = h.task(id).await?;
    let (cwd, count, docker) = {
        let t = t.lock().await;
        (t.cwd.clone(), t.checkpoints.len(), t.docker.is_some())
    };
    let enabled = h.settings.read().await.checkpoints;
    let chat = id.to_string();
    let edits = tokio::task::spawn_blocking(move || agent::bisect::timeline(&chat, count))
        .await
        .map_err(|e| e.to_string())?;
    let reason = agent::bisect::unavailable(enabled, &cwd, docker)
        .map(String::from)
        .or_else(|| {
            edits
                .is_empty()
                .then(|| "This chat has no recorded file edits yet.".to_string())
        });
    Ok(json!({
        "available": reason.is_none(),
        "reason": reason.unwrap_or_default(),
        "edits": edits,
    }))
}

/// Run a bisect the user asked for from the Review view. Progress streams as
/// `bisect` events; the final report is also the return value.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn task_bisect_run(
    h: H<'_>,
    id: String,
    command: String,
    from: Option<usize>,
    to: Option<usize>,
    timeout: Option<u64>,
    runs: Option<u32>,
    share: Option<Vec<String>>,
) -> Result<agent::bisect::Report, String> {
    let req = agent::bisect::Request::new(command, from, to, timeout, runs, share);
    bisect_run(h.inner(), &id, req).await
}

/// The body of `task_bisect_run`, on a plain `&Harness` so it can be tested.
///
/// The command is the user's, typed into a dialog that shows it and asks for
/// confirmation, but it is still judged by the same `permissions::check` the
/// agent's `bash` goes through: a deny rule, plan mode or the Disabled rung
/// refuses it here too (see `bisect::check_user_command`).
pub(crate) async fn bisect_run(
    h: &Arc<Harness>,
    id: &str,
    req: agent::bisect::Request,
) -> Result<agent::bisect::Report, String> {
    let t = h.task(id).await?;
    let (cwd, project, plan, perm, count, docker) = {
        let t = t.lock().await;
        (
            t.cwd.clone(),
            t.project.clone(),
            t.plan,
            t.perm.clone(),
            t.checkpoints.len(),
            t.docker.is_some(),
        )
    };
    let (enabled, allow) = {
        let s = h.settings.read().await;
        (s.checkpoints, s.allow.clone())
    };
    if let Some(why) = agent::bisect::unavailable(enabled, &cwd, docker) {
        return Err(why.into());
    }
    let ctx = agent::permissions::Ctx {
        perm: &perm,
        plan,
        cwd: &cwd,
        project: &project,
        allow: &allow,
    };
    agent::bisect::check_user_command(&req.command, &ctx)?;
    let chat = id.to_string();
    let steps = tokio::task::spawn_blocking(move || agent::bisect::timeline(&chat, count))
        .await
        .map_err(|e| e.to_string())?;
    let rt = h.runtime(id);
    // Unlike a runner turn, the Review action has no guaranteed run token. Use
    // the runtime-wide stop parent plus any active turn token, and snapshot the
    // force token before registering so force-pause/quit can interrupt it too.
    let cancel = tokio_util::sync::CancellationToken::new();
    let stop = rt.bg_cancel.lock().unwrap().child_token();
    let turn = rt.cancel.lock().unwrap().as_ref().map(|t| t.child_token());
    let force = rt.force.lock().unwrap().clone();
    let active = agent::bisect::begin(id, cancel.clone())?;
    let fg_guard = agent::runner::FgGuard::new(h.clone(), id);
    h.refresh_busy(id).await;
    tokio::select! {
        biased;
        unpaused = h.wait_unpaused(id, &cancel) => {
            if !unpaused { cancel.cancel(); }
        }
        _ = stop.cancelled() => cancel.cancel(),
        _ = force.cancelled() => cancel.cancel(),
        _ = cancel.cancelled() => {},
        _ = async { if let Some(turn) = turn.clone() { turn.cancelled().await } else { std::future::pending::<()>().await } } => cancel.cancel(),
    }
    let emit = |v: Value| h.emit(id, "bisect", v);
    let run = agent::bisect::run(id, &cwd, &steps, &req, &cancel, &emit);
    tokio::pin!(run);
    let report = tokio::select! {
        biased;
        result = &mut run => result,
        _ = stop.cancelled() => { cancel.cancel(); run.await },
        _ = force.cancelled() => { cancel.cancel(); run.await },
        _ = async { if let Some(turn) = turn { turn.cancelled().await } else { std::future::pending::<()>().await } } => {
            cancel.cancel();
            run.await
        }
    };
    drop(active);
    fg_guard.release().await;
    report
}

/// Stop the chat's running bisect (the user's or the agent's).
#[tauri::command]
async fn task_bisect_cancel(id: String) -> Result<bool, String> {
    Ok(agent::bisect::cancel(&id))
}

#[tauri::command]
async fn task_commit(h: H<'_>, id: String, message: String) -> Result<String, String> {
    let t = h.task(&id).await?;
    let cwd = t.lock().await.cwd.clone();
    // The two knobs are read here, not in the UI: they are settings, and a
    // webview that could pass `--no-verify: false` would let anything reaching
    // it re-enable repository-supplied hook execution with one IPC call.
    let (attribution, verify) = {
        let s = h.settings.read().await;
        (
            agent::commitmsg::Attribution::parse(&s.git_attribution),
            s.git_commit_verify,
        )
    };
    // Idempotent, so a message the Generate button already filled with its
    // trailer keeps exactly one.
    let message = agent::commitmsg::compose(&message, attribution);
    git::commit_with(&cwd, &message, !verify)
}

/// Draft a commit message for the current change.
///
/// Backs the review screen's Generate button. Never fails on an unusable model
/// answer: `commitmsg::generate` falls back to a deterministic message and says
/// so in `source`, because a button that reports an error instead of producing
/// something the user can edit is worse than a plain "chore: update 3 files".
#[tauri::command]
async fn task_commit_message(h: H<'_>, id: String) -> Result<agent::commitmsg::Draft, String> {
    let t = h.task(&id).await?;
    let (cwd, base) = {
        let t = t.lock().await;
        (t.cwd.clone(), t.base_commit.clone())
    };
    let base = base.filter(|_| git::is_repo(&cwd));
    // Only the git path: a commit is only offered for a git task, and the
    // snapshot path has no working tree for `git commit` to act on.
    let files = match base {
        Some(base) => git::review(&cwd, &base)?,
        None => vec![],
    };
    Ok(agent::commitmsg::generate(&h.inner().clone(), &id, &files).await)
}

#[tauri::command]
async fn bg_kill(h: H<'_>, task_id: String, bg_id: String) -> Result<(), String> {
    h.bg.kill(&bg_id);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    h.emit(&task_id, "bg", json!(h.bg.list(&task_id)));
    Ok(())
}

#[tauri::command]
async fn settings_update(
    app: tauri::AppHandle,
    h: H<'_>,
    patch: Value,
) -> Result<Settings, String> {
    let mut s = h.settings.write().await;
    let mut cur = serde_json::to_value(&*s).map_err(|e| e.to_string())?;

    if let (Some(obj), Some(p)) = (cur.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            // Keys, tokens and spend history are only changed through their own
            // commands. `hooks` joins them: each one is a shell command that runs
            // on a matching tool call, so accepting it from a generic settings
            // patch would let anything that can reach the webview — a future XSS
            // above all — install a command that executes on the next tool call
            // and outlives the session. Hooks are edited in Settings, which goes
            // through `hook_save` below.
            //
            // `trusted_hooks` has to be denied for the same reason, and it is the
            // more important half: it is the approval list, so writing it from the
            // generic patch would let whatever can reach the webview approve every
            // hook in the file — including a repository's — and the review gate
            // would then be gating nothing. It goes through `hooks_trust` only.
            if matches!(
                k.as_str(),
                "providers" | "spend" | "accounts" | "paused_all" | "hooks" | "trusted_hooks"
            ) {
                continue;
            }
            obj.insert(k.clone(), v.clone());
        }
    }
    let providers = s.providers.clone();
    let spend = s.spend.clone();
    let accts = s.accounts.clone();
    let gh_token = s.plugins.github.token.clone();
    let plugins_stored = s.plugins.clone();
    let mcp_stored = s.mcp.clone();
    let mut next: Settings = serde_json::from_value(cur).map_err(|e| e.to_string())?;
    // Store doc-selection paths normalized, like a trust decision's: the UI
    // filters by normalized path when it replaces a project's rule, so leaving a
    // raw spelling here would let two spellings of one folder accumulate.
    for r in next.project_docs.iter_mut() {
        r.path = agent::trust::norm_path(&r.path);
    }
    // Same for the projects that keep memory in the project, and deduplicated:
    // the list is a set, and the UI toggles by normalized path.
    let mut seen = std::collections::HashSet::new();
    next.memory_in_project = next
        .memory_in_project
        .iter()
        .map(|p| agent::trust::norm_path(p))
        .filter(|p| !p.is_empty() && seen.insert(p.clone()))
        .collect();
    next.providers = providers;
    next.spend = spend;
    next.accounts = accts;
    // The UI never sees the token; it's set through plugin_github_token.
    next.plugins.github.token = gh_token;
    // Same for the service plugins' tokens: set through plugin_secret_set only.
    agent::plugin_services::restore_secrets(&mut next.plugins, &plugins_stored);
    // Same deal for MCP bearer tokens: the UI only ever saw a masked copy, so a
    // masked entry coming back means "unchanged", not "cleared".
    restore_mcp_secrets(&mut next, &mcp_stored);
    // Keep `allow` in step with each server's `auto_approve`: every configured
    // entry gains its `mcp__<server>__<tool> *` rule if it is missing. Add-only
    // and idempotent, so it never revokes a rule — retracting a pre-approval
    // happens in the MCP tab, which filters the rule out of `allow` in the same
    // save (see `mcp::sync_auto_approve`).
    agent::mcp::sync_auto_approve(&mut next.allow, &next.mcp);
    // `explore` + `general` are always on: ignore attempts to toggle them off.
    store::ensure_required_agents(&mut next.default_agents);
    // `Hook::trusted` / `changed_from` are `#[serde(skip)]` — derived state that
    // must never be settable from a file — so the `from_value` above rebuilt
    // every hook with `trusted: false`. Re-resolving here is what keeps an
    // unrelated settings write (a theme, a budget, a model) from silently
    // disarming the user's hooks: without it every hook in the app goes dead the
    // first time anything else in Settings is saved, which is precisely the
    // "hook quietly stopped firing" outcome the trust design exists to avoid.
    let approved = next.trusted_hooks.clone();
    agent::checks::resolve_trust(&mut next.hooks, &approved);
    // Reject invalid agent group/fileRegex restrictions when agent definitions are edited.
    if patch.get("agents").is_some() {
        for d in &next.agents {
            store::validate_agent_groups(d)?;
        }
    }
    // Trust can also arrive through the generic patch. Keep the prompt cache and
    // every already-frozen task prefix in step with the exact trust change.
    let mut trust_prefixes = changed_trust_prefixes(&s.trust, &next.trust);
    // The per-project instruction selection can arrive the same way, and a frozen
    // prefix built under the old selection must be dropped for the same reason.
    trust_prefixes.extend(changed_doc_prefixes(&s.project_docs, &next.project_docs));
    // Cancel before the swap, under the same lock used by OAuth persistence.
    // Otherwise deleting and recreating a resource could revive an old flow.
    agent::mcp_oauth::invalidate_config_changes(&s.mcp, &next.mcp).await?;
    *s = next;
    router::set_routes(&s.routes);
    sync_model_state(&s);
    // Trust decisions reach the prompt path through this global, so a settings
    // write that changes them has to refresh it or the next prompt is stale.
    agent::trust::set_trust(&s.trust);
    store::save_settings(&s);
    let mcp_changed = patch.get("mcp").is_some();
    let out = public_settings(&s);
    let cfgs = s.mcp.clone();
    drop(s);
    // Every open view of the app hears about it, not only the caller: with
    // remote control a phone and the desktop window can both be open, and the
    // one that did not make the change would otherwise keep showing the old
    // settings until restarted.
    (h.bus)(
        "ol://settings",
        serde_json::to_value(&out).unwrap_or_default(),
    );
    invalidate_trust_prefixes(&h, &trust_prefixes).await;
    if mcp_changed {
        let h2 = h.inner().clone();
        tauri::async_runtime::spawn(async move { h2.mcp.sync(&cfgs).await });
    }
    // The update switches are read by the background pass, so a change to
    // them has to wake that pass: turning the automatic check on should take
    // effect now rather than at the next poll.
    if patch.get("auto_check_updates").is_some() || patch.get("auto_update").is_some() {
        updates::notify_policy(&app);
    }
    Ok(out)
}

/// Replace the user's hook list.
///
/// Hooks are shell commands that run on a matching tool call, so they are kept
/// out of the generic `settings_update` patch: that command takes whatever the
/// webview sends, and a hook installed through it would execute on the next
/// tool call and survive a restart. This is the one path that may set them,
/// and it is still the same trust boundary as every other IPC command — the
/// point is that a generic settings write cannot smuggle one in.
///
/// Saving does **not** approve. A hook that arrives here without an id is
/// assigned one and lands untrusted, so it shows up in Settings asking to be
/// reviewed rather than running on the next tool call — which is what "the
/// command is one the user typed" has to mean once a repository can also
/// contribute commands. Editing a trusted hook changes its hash, so the edit
/// un-trusts it on the way through `resolve_trust` and the UI shows the diff.
#[tauri::command]
async fn hooks_save(h: H<'_>, hooks: Vec<agent::checks::Hook>) -> Result<Settings, String> {
    let mut s = h.settings.write().await;
    let mut next = hooks;
    for x in next.iter_mut() {
        if x.id.trim().is_empty() {
            x.id = format!("hook-{}", agent::new_id());
        }
        // The UI cannot assign a scope to a user hook. Empty origin is the one
        // identity for the user's settings file; project origins come only from
        // the backend's read of the currently open project's hook file.
        x.source = "user".into();
        x.origin.clear();
    }
    s.hooks = next;
    let approved = s.trusted_hooks.clone();
    agent::checks::resolve_trust(&mut s.hooks, &approved);
    store::save_settings(&s);
    let out = public_settings(&s);
    drop(s);
    Ok(out)
}

/// Every hook in force for the open project, with its trust verdict.
///
/// The user's hooks and the project's `.openleash/hooks.json` together, each
/// marked trusted / needs-review / changed-since-approved, and with `can_edit`
/// false for the project's — the UI shows those read-only, because they live in
/// the repository and editing them in Settings would only write a copy that the
/// next read of the repo replaces.
#[tauri::command]
async fn hooks_status(h: H<'_>) -> Result<Value, String> {
    let s = h.settings.read().await;
    let project = s.project.clone();
    let mut all = s.hooks.clone();
    all.extend(agent::checks::project_hooks(&project, &s.trusted_hooks));
    agent::checks::resolve_trust(&mut all, &s.trusted_hooks);
    let hooks: Vec<agent::checks::HookView> = all.iter().map(agent::checks::HookView::of).collect();
    Ok(json!({ "project": project, "hooks": hooks }))
}

/// Review a hook: `approve` records the *current* body as trusted, `false`
/// withdraws it so the hook stops running.
///
/// The record is keyed by `(id, origin)` and stores the hash it approved, so an
/// approval can only ever cover the exact body that was on screen. Approving is
/// therefore also how a hook that changed gets re-trusted — the user reads the
/// diff `hooks_status` handed them and says yes to what is there now.
#[tauri::command]
async fn hooks_trust(
    h: H<'_>,
    id: String,
    origin: String,
    approve: bool,
) -> Result<Settings, String> {
    let mut s = h.settings.write().await;
    update_hook_trust(&mut s, &id, &origin, approve)?;
    store::save_settings(&s);
    let out = public_settings(&s);
    drop(s);
    Ok(out)
}

/// Whether an origin is a valid trust scope for the currently open project.
/// User hooks have an empty origin; project approvals are only actionable while
/// that exact project is open. This prevents arbitrary project origins from
/// entering the approval list through IPC.
fn valid_hook_origin(s: &Settings, origin: &str) -> bool {
    if origin.is_empty() {
        return true;
    }
    let project = s.project.trim();
    !project.is_empty() && origin == project && std::path::Path::new(project).is_dir()
}

/// Find a hook from the exact identity exposed by `hooks_status`.
fn hook_by_identity(s: &Settings, id: &str, origin: &str) -> Option<agent::checks::Hook> {
    if !valid_hook_origin(s, origin) {
        return None;
    }
    if origin.is_empty() {
        return s
            .hooks
            .iter()
            .find(|hook| hook.id == id && hook.origin.is_empty() && hook.source != "project")
            .cloned();
    }
    agent::checks::project_hooks(&s.project, &s.trusted_hooks)
        .into_iter()
        .find(|hook| hook.id == id && hook.origin == origin)
}

/// Apply a trust action, binding both approval and revocation to the exact
/// `(id, origin)` presented by the UI.
fn update_hook_trust(
    s: &mut Settings,
    id: &str,
    origin: &str,
    approve: bool,
) -> Result<(), String> {
    if approve {
        let hook = hook_by_identity(s, id, origin).ok_or_else(|| {
            "That hook is gone or its origin is no longer open — reopen Settings.".to_string()
        })?;
        agent::checks::approve(&hook, &mut s.trusted_hooks);
    } else {
        // Revoke only an identity the status view could have shown. This keeps
        // stale UI actions scoped to a real current hook as well as its origin.
        hook_by_identity(s, id, origin).ok_or_else(|| {
            "That hook is gone or its origin is no longer open — reopen Settings.".to_string()
        })?;
        agent::checks::revoke(id, origin, &mut s.trusted_hooks);
    }
    let approved = s.trusted_hooks.clone();
    agent::checks::resolve_trust(&mut s.hooks, &approved);
    Ok(())
}

/// List pull requests for an explicitly selected GitHub repository.
#[tauri::command]
async fn github_list_prs(h: H<'_>, project: String, state: String) -> Result<Value, String> {
    let remote = git::remote_url(&project).ok_or("This project has no Git remote.")?;
    let repo = agent::plugins::parse_repo(&remote)
        .ok_or("This project's origin is not a GitHub repository.")?;
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("This project's GitHub remote is invalid.".into());
    };
    let state = match state.as_str() {
        "closed" => "closed",
        "all" => "all",
        _ => "open",
    };
    let cfg = h.settings.read().await.plugins.github.clone();
    let token = tauri::async_runtime::spawn_blocking(move || agent::plugins::github_token(&cfg))
        .await
        .map_err(|e| e.to_string())?
        .map(|(token, _)| token)
        .ok_or("Sign in to GitHub in Settings → Connectors first.")?;
    let path = format!("/repos/{owner}/{name}/pulls?per_page=100&state={state}");
    let value = h
        .http
        .get(format!("https://api.github.com{}", path))
        .bearer_auth(token)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", "OpenLeash")
        .send()
        .await
        .map_err(|e| format!("GitHub request failed: {e}"))?;
    let status_code = value.status();
    let result: Value = value
        .json()
        .await
        .map_err(|e| format!("GitHub returned an invalid response: {e}"))?;
    if !status_code.is_success() {
        return Err(format!(
            "GitHub returned {}: {}",
            status_code.as_u16(),
            result["message"].as_str().unwrap_or("request failed")
        ));
    }
    let prs = result.as_array().into_iter().flatten().map(|p| json!({
        "number": p["number"], "title": p["title"], "state": p["state"], "draft": p["draft"],
        "html_url": p["html_url"], "user": p["user"]["login"], "head": p["head"]["ref"],
        "base": p["base"]["ref"], "updated_at": p["updated_at"]
    })).collect::<Vec<_>>();
    Ok(json!({ "repo": format!("{owner}/{name}"), "prs": prs }))
}

/// Plugin health for Settings → Plugins: GitHub login + token source, monitors for computer use.
#[tauri::command]
async fn plugins_status(h: H<'_>) -> Result<Value, String> {
    let cfg = h.settings.read().await.plugins.clone();
    let gh_cfg = cfg.github.clone();
    let tok = tauri::async_runtime::spawn_blocking(move || agent::plugins::github_token(&gh_cfg))
        .await
        .map_err(|e| e.to_string())?;
    let github = match tok {
        None => json!({"ok": false, "source": null, "error": "No token found"}),
        Some((token, source)) => {
            let gh = agent::plugins::Gh {
                http: &h.http,
                token,
                cwd: ".",
            };
            match gh.run(&json!({"action": "whoami"})).await {
                Ok(who) => {
                    json!({"ok": true, "source": source, "login": who.strip_prefix("Signed in as ").unwrap_or(&who)})
                }
                Err(e) => json!({"ok": false, "source": source, "error": e}),
            }
        }
    };
    let computer = tauri::async_runtime::spawn_blocking(|| match agent::tools::monitors() {
        Ok(ms) => {
            let list: Vec<Value> = ms
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "w": m.size.0,
                        "h": m.size.1,
                        "primary": m.primary,
                        // Listed in the order the tools index them (primary
                        // first), with the reason an agent may only *look* at
                        // one of them: the card has to say what the tool refuses.
                        "driveable": m.driveable(),
                        "rotation": m.rotation,
                        "x": m.origin.0,
                        "y": m.origin.1,
                    })
                })
                .collect();
            json!({"ok": !list.is_empty(), "monitors": list})
        }
        Err(e) => json!({"ok": false, "error": e, "monitors": []}),
    })
    .await
    .map_err(|e| e.to_string())?;
    // Which Chromium the browser plugin would drive, if any. Reported so the
    // toggle can say "not installed" up front instead of failing on the agent's
    // first navigate. The render plugin used to share this probe; it is gone.
    let browser =
        tauri::async_runtime::spawn_blocking(|| {
            match agent::plugins::browsers_installed().first() {
                Some(b) => json!({"ok": true, "browser": agent::plugins::browser_name(b)}),
                None => json!({"ok": false, "error": "No Chrome or Edge found"}),
            }
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(
        json!({"github": github, "computer": computer, "browser": browser, "has_token": !cfg.github.token.is_empty(), "can_login": agent::plugins::gh_client_id(&cfg.github).is_some(), "secrets": agent::plugin_services::secret_flags(&cfg)}),
    )
}

/// Bumped whenever a new sign-in starts, so a stale poll loop stops.
static GH_LOGIN_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// "Sign in with GitHub", step 1: returns {user_code, verification_uri, device_code, interval, expires_in}.
#[tauri::command]
async fn github_login_start(h: H<'_>) -> Result<Value, String> {
    let cfg = h.settings.read().await.plugins.github.clone();
    let cid = agent::plugins::gh_client_id(&cfg).ok_or(
        "No GitHub OAuth client id set up yet (see Settings → Plugins → GitHub → Advanced).",
    )?;
    GH_LOGIN_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    agent::plugins::device_start(&h.http, &cid).await
}

/// Step 2: wait until the user approves on github.com, then save the token. Returns the login.
#[tauri::command]
async fn github_login_wait(
    h: H<'_>,
    device_code: String,
    interval: u64,
    expires_in: u64,
) -> Result<String, String> {
    use std::sync::atomic::Ordering;
    let gen = GH_LOGIN_GEN.load(Ordering::SeqCst);
    let cid = agent::plugins::gh_client_id(&h.settings.read().await.plugins.github)
        .ok_or("No client id")?;
    let mut every = interval.max(5);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(expires_in.clamp(60, 1800));
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(every)).await;
        if GH_LOGIN_GEN.load(Ordering::SeqCst) != gen {
            return Err("cancelled".into());
        }
        if std::time::Instant::now() > deadline {
            return Err("The code expired. Start again.".into());
        }
        match agent::plugins::device_poll(&h.http, &cid, &device_code).await {
            Ok(None) => {}
            Ok(Some(token)) => {
                {
                    let mut s = h.settings.write().await;
                    s.plugins.github.token = token.clone();
                    s.plugins.github.enabled = true;
                    store::save_settings(&s);
                }
                let gh = agent::plugins::Gh {
                    http: &h.http,
                    token,
                    cwd: ".",
                };
                let who = gh
                    .run(&json!({"action": "whoami"}))
                    .await
                    .unwrap_or_default();
                let login = who
                    .strip_prefix("Signed in as ")
                    .unwrap_or(&who)
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_string();
                return Ok(login);
            }
            Err((k, _)) if k == "slow_down" => every += 5,
            Err((k, _)) if k == "network" => {}
            Err((_, msg)) => return Err(msg),
        }
    }
}

#[tauri::command]
async fn github_login_cancel() -> Result<(), String> {
    GH_LOGIN_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
async fn plugin_github_token(h: H<'_>, token: String) -> Result<(), String> {
    let mut s = h.settings.write().await;
    s.plugins.github.token = token.trim().to_string();
    store::save_settings(&s);
    Ok(())
}

/// Is any chat running its agent right now? Used by the time-in-app sampler.
///
/// `try_*` locks only: a chat mid-update holds its lock for an instant, and the
/// sampler must never queue behind the work it is measuring. A chat it cannot
/// read this tick is counted as running, since the lock is held by a running
/// agent far more often than by anything else.
fn any_chat_running(h: &Harness) -> bool {
    let Ok(tasks) = h.tasks.try_read() else {
        return false;
    };
    tasks.values().any(|r| match r.try_lock() {
        Ok(t) => !t.archived && t.status == "running",
        Err(_) => true,
    })
}

/// Save (or, empty, remove) a service plugin credential, e.g. (`slack`,
/// `user_token`). Settings saves never carry tokens; see `plugin_services`.
#[tauri::command]
async fn plugin_secret_set(
    h: H<'_>,
    plugin: String,
    field: String,
    value: String,
) -> Result<(), String> {
    let mut s = h.settings.write().await;
    agent::plugin_services::set_secret(&mut s.plugins, &plugin, &field, &value)?;
    store::save_settings(&s);
    Ok(())
}

/// "Test connection" for a service plugin: `{ok, identity, source}` or
/// `{ok: false, error}`. Works whether or not the plugin is switched on, so
/// the user can check a token before handing the tool to an agent.
#[tauri::command]
async fn plugin_service_test(h: H<'_>, plugin: String) -> Result<Value, String> {
    if !agent::plugin_services::is_service(&plugin) {
        return Err(format!("Unknown plugin `{plugin}`."));
    }
    let cfg = h.settings.read().await.plugins.clone();
    Ok(agent::plugin_services::test_connection(&h.http, &cfg, &plugin).await)
}

/// Chats that count as "in use" for a global model swap: live or paused.
fn swap_active(t: &Task) -> bool {
    !t.archived && (t.status == "running" || t.status == "waiting" || t.paused.is_some())
}

/// Where each model is used in one chat: main agent, its subagents, subagents it may spawn.
///
/// Each use is reported with the level it runs at; the dialog makes one row per
/// (model, level), so agents on one model at different levels can be changed
/// independently.
fn uses_in(t: &Task, defs: &[AgentDef], add: &mut dyn FnMut(String, String, usize)) {
    add(
        t.pending
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&t.model)
            .to_string(),
        "main agent".into(),
        t.effort,
    );
    for sub in t
        .subs
        .iter()
        .filter(|x| x.status == "running" || x.status == "stopped")
    {
        // What it is actually on, not what is stored on it: a subagent with no
        // model of its own is on the chat's, and a swap rewrites the chat — so
        // listing the stored value would show a model nothing is running on, and
        // miss the one every un-chosen agent really is on. Same for the effort.
        let def = defs.iter().find(|d| d.id == sub.role);
        add(
            agent::resolve_model(t, Some(sub), sub.depth, &t.model),
            format!("{} subagent ({})", sub.role, sub.status),
            agent::resolve_effort(
                t,
                Some(sub),
                def.and_then(|d| d.effort),
                def.is_some_and(|d| d.tools == "read_only"),
            ),
        );
    }
    for d in defs
        .iter()
        .filter(|d| t.agents.contains(&d.id) && !d.model.is_empty())
    {
        add(
            t.model_map
                .get(&d.model)
                .cloned()
                .unwrap_or(d.model.clone()),
            format!("{} subagent (new ones)", d.name),
            d.effort.unwrap_or(t.effort),
        );
    }
    // ULTRATHREAD X layers pick their own model, so they belong in the swap list
    // even before any agent on that layer has run. Reported as stored: a swap
    // rewrites the layer itself, and `resolve_model` reads it back unmapped.
    if let Some(x) = t.ultra_x.as_ref().filter(|x| x.on()) {
        for (i, l) in x.layers.iter().enumerate() {
            add(
                l.model.clone(),
                format!("ultrathread X layer {}", i + 1),
                l.effort.unwrap_or(t.effort),
            );
        }
    }
}
/// Models in use for the batch swap dialog: one chat, or (no id) everything live + defaults.
#[tauri::command]
async fn models_in_use(h: H<'_>, id: Option<String>) -> Result<Value, String> {
    let s = h.settings.read().await.clone();
    let mut order: Vec<(String, usize)> = vec![];
    // One row per (model, reasoning level). Agents on one model at different
    // levels are separate rows, because "just change the level of those" is a
    // real edit: the swap is keyed the same way (`SwapRule`), so each row only
    // reaches the agents it lists.
    let mut uses: HashMap<(String, usize), Vec<(String, u32)>> = HashMap::new();
    let mut add = |m: String, label: String, effort: usize| {
        if m.is_empty() {
            return;
        }
        // A model with no reasoning control has no levels to tell apart; whatever
        // stale level is stored on it must not split it into identical-looking rows.
        let effort = if providers::model_info(&m).reasoning_levels.is_empty() {
            0
        } else {
            effort.min(4)
        };
        let key = (m, effort);
        if !order.contains(&key) {
            order.push(key.clone());
        }
        let l = uses.entry(key).or_default();
        match l.iter_mut().find(|x| x.0 == label) {
            Some(x) => x.1 += 1,
            None => l.push((label, 1)),
        }
    };
    match &id {
        Some(id) => {
            let t = h.task(id).await?;
            let t = t.lock().await;
            let defs = store::all_agents(&s, &t.project);
            uses_in(&t, &defs, &mut add);
        }
        None => {
            // A disabled model is not something the user can run on, and a default
            // that merely names one is not traffic: listing it offered to "swap"
            // a model that is switched off and is not running anywhere.
            let usable = |m: &str| providers::model_info(m).enabled;
            if usable(&s.model) {
                add(s.model.clone(), "default for new chats".into(), s.effort);
            }
            for d in s
                .agents
                .iter()
                .filter(|d| !d.model.is_empty() && usable(&d.model))
            {
                add(
                    d.model.clone(),
                    format!("{} subagent default", d.name),
                    d.effort.unwrap_or(s.effort),
                );
            }
            let all: Vec<_> = h.tasks.read().await.values().cloned().collect();
            for t in all {
                let t = t.lock().await;
                if swap_active(&t) {
                    let defs = store::all_agents(&s, &t.project);
                    uses_in(&t, &defs, &mut add);
                }
            }
        }
    }
    // The UI reads `{rows, agents}`; returning the bare row list made `agents`
    // undefined, which crashed the dialog and left `rows` unset ("Looking…" forever).
    // Only the global dialog offers the spread toggle, so a chat gets no agents.
    let agents: Vec<Value> = if id.is_some() {
        vec![]
    } else {
        s.agents
            .iter()
            .filter(|d| !d.model.is_empty())
            .map(|d| json!({"id": d.id, "name": d.name}))
            .collect()
    };
    let rows: Vec<Value> = order
        .into_iter()
        .map(|(m, effort)| {
            let u: Vec<String> = uses
                .remove(&(m.clone(), effort))
                .unwrap_or_default()
                .into_iter()
                .map(|(l, n)| if n > 1 { format!("{l} ×{n}") } else { l })
                .collect();
            json!({"model": m, "effort": effort, "uses": u})
        })
        .collect();
    Ok(json!({"rows": rows, "agents": agents}))
}

/// Apply old -> new swaps to one chat. `effort` is keyed by the *source* model
/// exactly like `map`, so a row's reasoning travels with the model it belongs to
/// and a chat swapped Sonnet@Low -> Opus carries the level picked for Opus.
///
/// An effort is only written where one was already chosen, the same rule the model
/// follows. Stamping it everywhere instead would pin every read-only explorer that
/// nobody configured: they hold the chat's effort by design (`resolve_effort`), so
/// writing a value onto them would turn an inherited setting into a stored
/// override, and the next chat-level effort change would stop reaching them.
pub(crate) fn swap_task(
    t: &mut Task,
    map: &HashMap<String, String>,
    effort: &HashMap<String, usize>,
    future: bool,
) {
    if let Some(to) = map.get(&t.model) {
        t.model = to.clone();
        // The fallback chain belongs to the model, so it moves with it — the same
        // thing the picker does (`{ model, route: defaultRoute(model) }`). Leaving
        // it behind kept a route whose head the chat no longer used and whose
        // fallbacks were models the user had just swapped away from: the resumed
        // chat retried a chain that had nothing to do with what they picked, and
        // pausing on that chain is what made a swap look like it needed a restart
        // to resume. Only a restart recomputed it, because only a restart reads
        // the route back off disk.
        //
        // Only when the model actually moved. A chat the swap left alone may be on
        // a route somebody chose by hand, and re-deriving that would silently
        // discard their decision.
        t.route = router::default_route(to);
    }
    // Read *after* the swap above, so a level picked for a destination applies to
    // the destination rather than to the model the row was on when it was picked.
    // Keyed by the model the chat is on now, which is also what makes this a
    // reasoning-only change: the row moved its level and nothing else, which is
    // the one edit this dialog used to silently throw away.
    if let Some(e) = effort.get(t.model.as_str()) {
        t.effort = (*e).min(4);
    }
    if let Some(to) = t
        .pending
        .get("model")
        .and_then(|v| v.as_str())
        .and_then(|m| map.get(m))
        .cloned()
    {
        t.pending.insert("model".into(), json!(to));
    }
    // Only a model somebody *chose* is stored on a subagent, so this is the one
    // place a swap has to reach them: the ones with no model of their own follow
    // the chat, which was just rewritten.
    for sub in t.subs.iter_mut() {
        // Resolved against this agent's own model, and *only* when it has one.
        // An agent that follows the chat stores no model and so stores no level:
        // it holds the chat's effort by design, and writing one here would turn an
        // inherited setting into a stored override, which then stops the next
        // chat-level effort change from reaching it.
        if sub.model.is_empty() {
            if let Some(to) = map.get(t.model.as_str()) {
                sub.model = to.clone();
            }
            continue;
        }
        if let Some(to) = map.get(&sub.model) {
            sub.model = to.clone();
        }
        if let Some(e) = effort.get(sub.model.as_str()) {
            sub.effort = Some((*e).min(4));
        }
    }
    // An X ladder names its own models, which no swap above touches.
    if let Some(x) = t.ultra_x.as_mut() {
        for l in x.layers.iter_mut() {
            if let Some(to) = map.get(&l.model) {
                l.model = to.clone();
            }
            if let Some(e) = effort.get(l.model.as_str()) {
                l.effort = Some((*e).min(4));
            }
        }
    }
    // Carry the swap into the defaults later subagents are launched from — the
    // ladder's layers and the agent types' own models.
    //
    // Gated on `future`, and this is the gate that matters: `model_map` is what
    // `spawn_sub` consults when launching an agent whose *type* names a model, so
    // recording it unconditionally meant swapping a chat silently changed what every
    // future spawn of that type would run on — reached from a row that never mentioned
    // subagents, for a type that may not even be in this chat. "This chat's model"
    // and "the type's default" are two decisions; the dialog now asks for the second.
    //
    // Mappings are kept current rather than appended to. A stale entry is not a
    // harmless record: a second swap (B -> C) left `A -> B` behind, so a subagent
    // launched from a default still naming A ended up on B — a model the chat had
    // already been moved off two swaps ago. So an entry whose destination this
    // swap moves is carried along to the new destination, and an entry whose key
    // this swap names is dropped first: the new pair about to be inserted already
    // says where that id goes, and a no-op swap is not worth a record.
    update_model_map(t, map, future);
}

/// The `future` half of a swap: see the comment in `swap_task`.
fn update_model_map(t: &mut Task, map: &HashMap<String, String>, future: bool) {
    if future {
        for v in t.model_map.values_mut() {
            if let Some(to) = map.get(v.as_str()) {
                *v = to.clone();
            }
        }
        t.model_map
            .retain(|from, to| !map.contains_key(from) && to != from);
        for (from, to) in map {
            t.model_map.insert(from.clone(), to.clone());
        }
    }
}

/// One row of the swap dialog, as sent back: agents on `from` at level
/// `from_effort` move to `to` (which may be `from`) and, when `to_effort` is set,
/// onto that level. Matching on both is what lets two rows of the same model be
/// edited independently.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct SwapRule {
    from: String,
    from_effort: usize,
    to: String,
    #[serde(default)]
    to_effort: Option<usize>,
}

fn find_rule<'a>(rules: &'a [SwapRule], model: &str, effort: usize) -> Option<&'a SwapRule> {
    // Same normalization as the dialog's rows (`models_in_use`).
    let norm = |m: &str, e: usize| {
        if providers::model_info(m).reasoning_levels.is_empty() {
            0
        } else {
            e.min(4)
        }
    };
    rules
        .iter()
        .find(|r| r.from == model && norm(model, r.from_effort) == norm(model, effort))
}

/// `swap_task` for per-(model, level) rows. Every agent is matched against its
/// state *before* anything is rewritten, so chained rules (A->B, B->C) do not
/// cascade.
pub(crate) fn swap_task_rules(t: &mut Task, defs: &[AgentDef], rules: &[SwapRule], future: bool) {
    if rules.is_empty() {
        return;
    }
    let chat_effort = t.effort;
    // (index, model, effort) per subagent that has a model of its own; the ones
    // without follow the chat and move with it, exactly as in `swap_task`.
    let mut subs: Vec<(usize, String, Option<usize>)> = vec![];
    for (i, sub) in t.subs.iter().enumerate() {
        if sub.model.is_empty() {
            continue;
        }
        let def = defs.iter().find(|d| d.id == sub.role);
        let eff = agent::resolve_effort(
            t,
            Some(sub),
            def.and_then(|d| d.effort),
            def.is_some_and(|d| d.tools == "read_only"),
        );
        if let Some(r) = find_rule(rules, &sub.model, eff) {
            subs.push((i, r.to.clone(), r.to_effort.map(|e| e.min(4))));
        }
    }
    let layers: Vec<(usize, String, Option<usize>)> = t
        .ultra_x
        .as_ref()
        .map(|x| {
            x.layers
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    find_rule(rules, &l.model, l.effort.unwrap_or(chat_effort))
                        .map(|r| (i, r.to.clone(), r.to_effort.map(|e| e.min(4))))
                })
                .collect()
        })
        .unwrap_or_default();
    let pending = t
        .pending
        .get("model")
        .and_then(|v| v.as_str())
        .and_then(|m| find_rule(rules, m, chat_effort))
        .map(|r| r.to.clone());
    let main = find_rule(rules, &t.model, chat_effort).cloned();

    if let Some(r) = main {
        if r.to != t.model {
            t.model = r.to.clone();
            t.route = router::default_route(&r.to);
        }
        if let Some(e) = r.to_effort {
            t.effort = e.min(4);
        }
    }
    if let Some(to) = pending {
        t.pending.insert("model".into(), json!(to));
    }
    for (i, to, e) in subs {
        if let Some(sub) = t.subs.get_mut(i) {
            sub.model = to;
            if let Some(e) = e {
                sub.effort = Some(e);
            }
        }
    }
    if let Some(x) = t.ultra_x.as_mut() {
        for (i, to, e) in layers {
            if let Some(l) = x.layers.get_mut(i) {
                l.model = to;
                if let Some(e) = e {
                    l.effort = Some(e);
                }
            }
        }
    }
    let map: HashMap<String, String> = rules
        .iter()
        .filter(|r| r.to != r.from)
        .map(|r| (r.from.clone(), r.to.clone()))
        .collect();
    update_model_map(t, &map, future);
}

/// `swap_defaults` for per-(model, level) rows.
fn swap_defaults_rules(s: &mut Settings, rules: &[SwapRule], spread: bool) {
    if rules.is_empty() {
        return;
    }
    let base_effort = s.effort;
    if let Some(r) = find_rule(rules, &s.model, base_effort).cloned() {
        s.model = r.to;
        if let Some(e) = r.to_effort {
            s.effort = e.min(4);
        }
    }
    if spread {
        for d in s.agents.iter_mut().filter(|d| !d.model.is_empty()) {
            if let Some(r) = find_rule(rules, &d.model, d.effort.unwrap_or(base_effort)) {
                d.model = r.to.clone();
                if let Some(e) = r.to_effort {
                    d.effort = Some(e.min(4));
                }
            }
        }
    }
    store::save_settings(s);
}

/// Apply old -> new model swaps right away: in one chat, or (no id) defaults + every live chat.
///
/// `effort` is keyed by the *model the level belongs to*, which is the source
/// model when that row is also swapping and the model itself when it is not. A
/// level the user picked for a model they are keeping is a change to that model,
/// not a stray: it used to be filtered out here, which is why the dialog could
/// offer a level and then silently drop it, and why reasoning could only be
/// changed by first swapping away from the model and back.
/// `spread` is the user saying yes to "and the subagent types too". A separate
/// flag rather than another entry in `map`, because the two reach very different
/// things: without it a swap moves this chat and the live ones and nothing a
/// future spawn would read. With it, the agent types naming a swapped model have
/// that model rewritten, and each live chat records the mapping its future spawns
/// would follow.
#[tauri::command]
async fn models_swap(
    h: H<'_>,
    id: Option<String>,
    map: HashMap<String, String>,
    effort: Option<HashMap<String, usize>>,
    spread: Option<bool>,
    rules: Option<Vec<SwapRule>>,
) -> Result<Settings, String> {
    let spread = spread.unwrap_or(false);
    let rules: Vec<SwapRule> = rules
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.from.is_empty() && !r.to.is_empty())
        .collect();
    let s0 = h.settings.read().await.clone();
    let map: HashMap<String, String> = map
        .into_iter()
        .filter(|(f, t)| !t.is_empty() && f != t)
        .collect();
    // Re-keyed to the model the level will actually be applied to, so a row that
    // is only changing reasoning is applied on the model it is already on.
    let effort: HashMap<String, usize> = effort
        .unwrap_or_default()
        .into_iter()
        .filter(|(from, _)| !from.is_empty())
        .map(|(from, e)| {
            let to = map.get(&from).cloned().unwrap_or(from);
            (to, e.min(4))
        })
        .collect();
    let ids: Vec<String> = match &id {
        Some(id) => vec![id.clone()],
        None => {
            let all: Vec<_> = h.tasks.read().await.values().cloned().collect();
            let mut v = vec![];
            for t in all {
                let t = t.lock().await;
                if swap_active(&t) {
                    v.push(t.id.clone());
                }
            }
            v
        }
    };
    // Both halves, independently: `map` can be empty for a reasoning-only change
    // and `effort` can be empty for a pure swap, and each has to run or the
    // button the user pressed does nothing at all.
    if !map.is_empty() || !effort.is_empty() || !rules.is_empty() {
        for tid in &ids {
            h.update_task(tid, |t| {
                swap_task(t, &map, &effort, spread);
                let defs = store::all_agents(&s0, &t.project);
                swap_task_rules(t, &defs, &rules, spread);
            })
            .await;
            h.save_task(tid).await;
        }
    }
    let mut s = h.settings.write().await;
    // A global swap also moves the defaults, so the reasoning has to move with
    // them: a default left on the old model's level would send a level the new
    // model may not even have, on every chat started from here on.
    if id.is_none() {
        swap_defaults(&mut s, &map, &effort, spread);
        swap_defaults_rules(&mut s, &rules, spread);
    }
    Ok(public_settings(&s))
}

/// Move the global defaults a swap acts on: the model new chats start on, and —
/// only when the user asked — the subagent types that pin one of their own.
///
/// Split out of the command so it can be driven directly. `H<'_>` is a Tauri's
/// `State`, whose fields are private and which cannot be built outside the app,
/// so a test could not reach the settings half through the command at all, and
/// the half that needed a test is the one with a gate on it.
fn swap_defaults(
    s: &mut Settings,
    map: &HashMap<String, String>,
    effort: &HashMap<String, usize>,
    spread: bool,
) {
    // Both halves, independently: a reasoning-only change sends an empty `map`,
    // and a pure swap an empty `effort`, and each has to land on its own.
    if map.is_empty() && effort.is_empty() {
        return;
    }
    if let Some(to) = map.get(&s.model) {
        s.model = to.clone();
    }
    // Keyed on the model the default actually ends up on, so a row the user only
    // changed the reasoning of — no swap involved — still lands.
    if let Some(e) = effort.get(s.model.as_str()) {
        s.effort = *e;
    }
    // The agent types are the part that has to be asked for. This rewrote every
    // type naming a swapped model on every global swap, silently: the dialog
    // listed those models in rows that read like live traffic, so moving one
    // looked like "the model something is running on is changing" while what
    // actually changed was the default for every agent spawned from here on — in
    // every chat, live or not. Off unless the user opted in.
    if spread {
        for d in s.agents.iter_mut() {
            if let Some(to) = map.get(&d.model) {
                d.model = to.clone();
            }
            if let Some(e) = effort.get(d.model.as_str()) {
                d.effort = Some(*e);
            }
        }
    }
    store::save_settings(s);
}

#[tauri::command]
async fn provider_set(
    h: H<'_>,
    id: String,
    api_key: Option<String>,
    base_url: Option<String>,
    enabled: Option<bool>,
    key_pool: Option<bool>,
) -> Result<Vec<ProviderView>, String> {
    let mut s = h.settings.write().await;
    let e = s.providers.entry(id).or_insert_with(|| store::ProviderCfg {
        enabled: true,
        ..Default::default()
    });
    if let Some(k) = api_key {
        e.api_key = k.trim().to_string();
    }
    if let Some(b) = base_url {
        e.base_url = b.trim().to_string();
    }
    if let Some(en) = enabled {
        e.enabled = en;
    }
    if let Some(kp) = key_pool {
        if kp && e.api_keys.is_empty() && !e.api_key.is_empty() {
            // Migrate the existing single key into the pool when enabling.
            e.api_keys = vec![e.api_key.clone()];
        } else if !kp && !e.api_keys.is_empty() {
            // Move the first pool key back to the primary field when disabling.
            e.api_key = e.api_keys.first().cloned().unwrap_or_default();
            e.api_keys = vec![];
        }
        e.key_pool = kp;
    }
    store::save_settings(&s);
    Ok(provider_views(&s))
}

#[tauri::command]
async fn models_list(h: H<'_>) -> Result<Vec<ModelInfo>, String> {
    Ok(models(&*h.settings.read().await))
}

#[derive(Deserialize)]
struct NewProvider {
    name: String,
    base_url: String,
    kind: String,
    api_key: String,
    #[serde(default)]
    insist: bool,
    /// Connect a built-in provider (just stores the key) instead of adding a new endpoint.
    #[serde(default)]
    builtin: Option<String>,
}

/// Install and login state of the Antigravity CLI, plus a live model refresh.
///
/// The refresh pushes the CLI's own model list into the catalog, so the UI calls
/// this when the provider card opens and re-reads models afterwards. It is also
/// the only honest way to answer "is it signed in?" — `agy models` needs a
/// working login, so a list coming back is the proof.
#[tauri::command]
async fn antigravity_status() -> antigravity::Status {
    antigravity::status().await
}

#[tauri::command]
async fn provider_add(h: H<'_>, p: NewProvider) -> Result<Vec<ProviderView>, String> {
    if let Some(b) = p
        .builtin
        .filter(|b| providers::provider(b).is_some_and(|info| info.account.is_none()))
    {
        let mut s = h.settings.write().await;
        s.providers.insert(
            b,
            store::ProviderCfg {
                api_key: p.api_key.trim().into(),
                enabled: true,
                ..Default::default()
            },
        );
        store::save_settings(&s);
        return Ok(provider_views(&s));
    }
    let name = p.name.trim().to_string();
    let base = p.base_url.trim().trim_end_matches('/').to_string();
    if name.is_empty() || !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("Give it a name and an http(s) base URL.".into());
    }
    let mut s = h.settings.write().await;
    let mut id = git::slug(&name);
    while providers::provider(&id).is_some() || s.custom_providers.iter().any(|c| c.id == id) {
        id.push('2');
    }
    s.custom_providers.push(CustomProvider {
        id: id.clone(),
        name,
        base_url: base,
        kind: if p.kind == "anthropic" {
            "anthropic".into()
        } else {
            "openai".into()
        },
        insist: p.insist,
    });
    if !p.api_key.trim().is_empty() {
        s.providers.insert(
            id,
            store::ProviderCfg {
                api_key: p.api_key.trim().into(),
                enabled: true,
                ..Default::default()
            },
        );
    }
    store::save_settings(&s);
    Ok(provider_views(&s))
}

#[tauri::command]
async fn provider_insist(h: H<'_>, id: String, insist: bool) -> Result<Vec<ProviderView>, String> {
    let mut s = h.settings.write().await;
    if let Some(c) = s.custom_providers.iter_mut().find(|c| c.id == id) {
        c.insist = insist;
    }
    store::save_settings(&s);
    Ok(provider_views(&s))
}

#[derive(Deserialize)]
struct KeyPatch {
    op: String,
    key: Option<String>,
    index: Option<usize>,
}

/// Add or remove an API key in a provider's key pool.
#[tauri::command]
async fn provider_key(h: H<'_>, id: String, p: KeyPatch) -> Result<Vec<ProviderView>, String> {
    let mut s = h.settings.write().await;
    let cfg = s
        .providers
        .get_mut(&id)
        .ok_or_else(|| format!("No such provider: {id}"))?;
    match p.op.as_str() {
        "add" => {
            if let Some(k) = p.key {
                let k = k.trim().to_string();
                if !k.is_empty() && !cfg.api_keys.contains(&k) {
                    cfg.api_keys.push(k);
                }
            }
        }
        "remove" => {
            if let Some(i) = p.index {
                if i < cfg.api_keys.len() {
                    cfg.api_keys.remove(i);
                    if let Some(k2) = p.key {
                        cfg.api_keys.retain(|k| k != &k2);
                    }
                }
            }
        }
        "replace" => {
            if let (Some(i), Some(k)) = (p.index, p.key) {
                let k = k.trim().to_string();
                if i < cfg.api_keys.len() && !k.is_empty() {
                    cfg.api_keys[i] = k;
                }
            }
        }
        _ => return Err(format!("Unknown key operation: {}", p.op)),
    }
    store::save_settings(&s);
    Ok(provider_views(&s))
}

/// Custom providers are deleted; built-ins are disconnected (key forgotten).
#[tauri::command]
async fn provider_remove(h: H<'_>, id: String) -> Result<Vec<ProviderView>, String> {
    let mut s = h.settings.write().await;
    if providers::provider(&id).is_some() {
        s.providers.remove(&id);
        store::save_settings(&s);
        return Ok(provider_views(&s));
    }
    s.custom_providers.retain(|c| c.id != id);
    s.providers.remove(&id);
    let prefix = format!("{id}/");
    s.model_configs.retain(|m| !m.id.starts_with(&prefix));
    s.disabled_models.retain(|m| !m.starts_with(&prefix));
    s.removed_models.retain(|m| !m.starts_with(&prefix));
    s.custom_models.retain(|m| !m.starts_with(&prefix));
    sync_model_state(&s);
    store::save_settings(&s);
    Ok(provider_views(&s))
}

#[tauri::command]
async fn model_save(
    h: H<'_>,
    model: ModelInfo,
    replace: Option<String>,
) -> Result<Vec<ModelInfo>, String> {
    if !model.id.contains('/') || model.id.ends_with('/') {
        return Err("Model id must look like provider/model-id.".into());
    }
    if model.context < 1000 {
        return Err("Context window looks wrong.".into());
    }
    let mut s = h.settings.write().await;
    let old = replace.unwrap_or_else(|| model.id.clone());
    let mut model = model;
    // Saving revives a deleted model; the toggle state is preserved so
    // customizing a model that's off keeps it off.
    let want_enabled = model.enabled;
    s.removed_models.retain(|m| m != &model.id);
    if want_enabled {
        s.disabled_models.retain(|m| m != &model.id);
    } else if !s.disabled_models.contains(&model.id) {
        s.disabled_models.push(model.id.clone());
    }
    model.enabled = true;
    s.model_configs.retain(|m| m.id != old && m.id != model.id);
    s.model_configs.push(model);
    sync_model_state(&s);
    store::save_settings(&s);
    Ok(models(&s))
}

/// Deleting a custom model drops its override; deleting a built-in or pool
/// model hides it (stored in removed_models) so it stays gone across restarts.
/// Re-adding the same id via Add model revives it.
#[tauri::command]
async fn model_remove(h: H<'_>, id: String) -> Result<Vec<ModelInfo>, String> {
    let mut s = h.settings.write().await;
    s.model_configs.retain(|m| m.id != id);
    s.custom_models.retain(|m| m != &id);
    s.disabled_models.retain(|m| m != &id);
    if !s.removed_models.contains(&id) {
        s.removed_models.push(id);
    }
    sync_model_state(&s);
    store::save_settings(&s);
    Ok(models(&s))
}

/// Toggle a single model on/off. Works for built-in, pool and custom models
/// across every provider; off models stay listed in settings (dimmed) but are
/// hidden from the model picker and routing fallbacks.
#[tauri::command]
async fn model_set_enabled(h: H<'_>, id: String, enabled: bool) -> Result<Vec<ModelInfo>, String> {
    let mut s = h.settings.write().await;
    if enabled {
        s.disabled_models.retain(|m| m != &id);
    } else if !s.disabled_models.contains(&id) {
        s.disabled_models.push(id);
    }
    sync_model_state(&s);
    store::save_settings(&s);
    Ok(models(&s))
}

#[tauri::command]
async fn provider_models(h: H<'_>, id: String) -> Result<Vec<Value>, String> {
    let s = h.settings.read().await.clone();
    providers::list_remote_models(&h.http, &s, &id).await
}

#[tauri::command]
async fn model_detail(h: H<'_>, prov: String, model: String) -> Result<Value, String> {
    let s = h.settings.read().await.clone();
    providers::model_detail(&h.http, &s, &prov, &model).await
}

#[tauri::command]
async fn mcp_oauth_start(
    h: H<'_>,
    name: String,
    client_id: Option<String>,
    scopes: Option<Vec<String>>,
    redirect_port: Option<u16>,
) -> Result<agent::mcp_oauth::Started, String> {
    let settings = h.settings.read().await;
    let cfg = settings
        .mcp
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .ok_or("Unknown MCP server")?;
    // Keep the read guard until the flow is registered so a settings update
    // cannot invalidate the snapshot before start has made it cancellable.
    let result =
        agent::mcp_oauth::start(cfg, client_id, scopes.unwrap_or_default(), redirect_port).await;
    drop(settings);
    result
}
#[tauri::command]
async fn mcp_oauth_wait(h: H<'_>, flow_id: String) -> Result<(), String> {
    let (original, credential) = agent::mcp_oauth::wait(&flow_id).await?;
    let mut s = h.settings.write().await;
    let cfg = s
        .mcp
        .iter_mut()
        .find(|c| agent::mcp_oauth::same_identity(c, &original))
        .ok_or("MCP configuration changed during authorization")?;
    agent::mcp_oauth::persist(cfg, credential).await?;
    cfg.enabled = true;
    store::save_settings(&s);
    let cfgs = s.mcp.clone();
    let out = serde_json::to_value(public_settings(&s)).unwrap_or_default();
    drop(s);
    (h.bus)("ol://settings", out);
    h.mcp.sync(&cfgs).await;
    Ok(())
}
#[tauri::command]
async fn mcp_oauth_cancel(flow_id: String) -> Result<(), String> {
    agent::mcp_oauth::cancel(&flow_id).await;
    Ok(())
}
#[tauri::command]
async fn mcp_oauth_disconnect(h: H<'_>, name: String) -> Result<(), String> {
    let mut s = h.settings.write().await;
    if let Some(c) = s.mcp.iter_mut().find(|c| c.name == name) {
        c.enabled = false;
    }
    agent::mcp_oauth::disconnect(&name).await?;
    store::save_settings(&s);
    let cfgs = s.mcp.clone();
    let out = serde_json::to_value(public_settings(&s)).unwrap_or_default();
    drop(s);
    (h.bus)("ol://settings", out);
    h.mcp.sync(&cfgs).await;
    Ok(())
}

#[tauri::command]
async fn mcp_status(h: H<'_>) -> Result<Vec<agent::mcp::McpStatus>, String> {
    Ok(h.mcp.statuses().await)
}

#[tauri::command]
async fn mcp_reconnect(h: H<'_>) -> Result<(), String> {
    let cfgs: Vec<McpServerCfg> = h.settings.read().await.mcp.clone();
    h.mcp.sync(&cfgs).await;
    Ok(())
}

/// Whether a project path names an existing folder: on this machine, or on
/// the SSH host for an `ssh://` project (never as a local path).
async fn project_dir_exists(path: &str) -> bool {
    match agent::host::parse_ssh(path) {
        Some(p) => agent::ssh_session::stat(&p.host, &p.path)
            .await
            .is_ok_and(|s| s.is_dir),
        None => !agent::host::is_ssh(path) && Path::new(path).is_dir(),
    }
}

#[tauri::command]
async fn project_info(h: H<'_>, path: String) -> Result<Value, String> {
    // Several `git` child processes and a scan of instruction files: run off the
    // async workers so switching projects doesn't stall every other command.
    let git_path = path.clone();
    let (is_git, branch, branches) = tokio::task::spawn_blocking(move || {
        let is_git = git::is_repo(&git_path);
        if is_git {
            (true, git::current_branch(&git_path), git::branches(&git_path))
        } else {
            (false, String::new(), vec![])
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let (inject_global_claude, disabled_docs) = {
        let s = h.settings.read().await;
        (
            s.inject_global_claude,
            agent::prompt::disabled_docs(&s.project_docs, &path),
        )
    };
    Ok(json!({
        // Echoed back so the UI can tell which folder this inventory describes:
        // the settings panel offers a per-file switch whose write goes to
        // whatever project is *currently* selected, and a stale answer from a
        // previous folder must not be presented as this one's files.
        "path": path,
        "exists": project_dir_exists(&path).await,
        "git": is_git,
        "branch": branch,
        "branches": branches,
        // What actually reaches the prompt, not the raw inventory: for an
        // untrusted folder this is the globals alone.
        "memory": agent::prompt::memory_for_prompt(&path, inject_global_claude, &disabled_docs).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
        // The project's own instruction files that exist, whether or not the
        // user has switched them off — the Settings row offers a toggle per
        // name, so it needs the inventory, not what survived the selection.
        "docs": agent::prompt::project_files(&path, &[]).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "disabled_docs": disabled_docs,
        "trusted": agent::trust::trusted_now(&path),
        "decided": agent::trust::decided(&agent::trust::current(), &path),
    }))
}

/// What a folder would inject, whether or not it is trusted, for the trust
/// dialog and the manifest panel. Computed live so the answer is what the next
/// prompt would actually contain.
#[tauri::command]
async fn project_branch_create(
    path: String,
    name: String,
    base: String,
) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        git::create_branch(&path, &name, &base)?;
        let mut branches = git::branches(&path);
        branches.retain(|branch| branch != name.trim());
        branches.insert(0, name.trim().to_string());
        Ok(branches)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn trust_manifest(h: H<'_>, path: String) -> Result<Value, String> {
    let s = h.settings.read().await.clone();
    serde_json::to_value(agent::trust::manifest(&s, &path)).map_err(|e| e.to_string())
}

/// Record a trust decision for a folder (`kind`: `folder` | `parent`,
/// `decision`: `trusted` | `untrusted`). Returns the settings the UI mirrors.
///
/// A decision is saved immediately and published to the prompt path, so it
/// takes effect on the next request rather than the next launch. Existing chats'
/// frozen prefixes are dropped for every affected project so they rebuild —
/// otherwise a chat opened before the decision would keep running on the old
/// answer for the life of its prefix.
#[tauri::command]
async fn trust_set(
    h: H<'_>,
    path: String,
    kind: String,
    decision: String,
) -> Result<Settings, String> {
    if path.trim().is_empty() {
        return Err("No folder given.".into());
    }
    let kind = parse_trust_kind(&kind)?;
    let state = parse_trust_state(&decision)?;
    let (out, prefixes) = {
        let mut s = h.settings.write().await;
        let old_trust = s.trust.clone();
        agent::trust::upsert(&mut s.trust, &path, kind, state);
        let prefixes = changed_trust_prefixes(&old_trust, &s.trust);
        store::save_settings(&s);
        agent::trust::set_trust(&s.trust);
        (public_settings(&s), prefixes)
    };
    invalidate_trust_prefixes(&h, &prefixes).await;
    (h.bus)(
        "ol://settings",
        serde_json::to_value(&out).unwrap_or_default(),
    );
    Ok(out)
}

/// Forget every decision recorded for a folder, so it goes back to being a
/// folder nobody has decided about — which is untrusted, but the dialog asks
/// again. Distinct from an explicit "don't trust": that one is remembered.
#[tauri::command]
async fn trust_clear(h: H<'_>, path: String) -> Result<Settings, String> {
    let (out, prefixes) = {
        let mut s = h.settings.write().await;
        let old_trust = s.trust.clone();
        agent::trust::clear(&mut s.trust, &path);
        let prefixes = changed_trust_prefixes(&old_trust, &s.trust);
        store::save_settings(&s);
        agent::trust::set_trust(&s.trust);
        (public_settings(&s), prefixes)
    };
    invalidate_trust_prefixes(&h, &prefixes).await;
    (h.bus)(
        "ol://settings",
        serde_json::to_value(&out).unwrap_or_default(),
    );
    Ok(out)
}

/// What another coding tool's project configuration would import, without
/// writing anything. Read-only on purpose: the settings row shows the user the
/// count, a preview per source and what was skipped, and only then offers to
/// import. See `importer` for the sources and the skip rules.
#[tauri::command]
async fn import_scan(path: String) -> Result<Value, String> {
    serde_json::to_value(importer::scan(&path)).map_err(|e| e.to_string())
}

/// Append this project's importable rules to OpenLeash's own instruction file.
/// Refuses a folder that is not trusted, and is idempotent: a second call
/// writes nothing rather than duplicating a block. See `importer::apply`.
#[tauri::command]
async fn import_apply(path: String) -> Result<Value, String> {
    serde_json::to_value(importer::apply(&path)?).map_err(|e| e.to_string())
}

fn parse_trust_kind(k: &str) -> Result<agent::trust::TrustKind, String> {
    match k.trim().to_ascii_lowercase().as_str() {
        "folder" => Ok(agent::trust::TrustKind::Folder),
        "parent" => Ok(agent::trust::TrustKind::Parent),
        other => Err(format!("unknown trust kind \"{other}\"")),
    }
}

fn parse_trust_state(k: &str) -> Result<agent::trust::TrustState, String> {
    match k.trim().to_ascii_lowercase().as_str() {
        "trusted" | "trust" => Ok(agent::trust::TrustState::Trusted),
        "untrusted" | "untrust" => Ok(agent::trust::TrustState::Untrusted),
        other => Err(format!("unknown trust decision \"{other}\"")),
    }
}

/// One changed trust path and the scope of its effect. A folder decision is
/// exact; a parent decision also governs every project below it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustPrefix {
    path: String,
    descendants: bool,
}

fn invalidation_path(path: &str) -> String {
    let normalized = agent::trust::norm_path(path);
    let bytes = normalized.as_bytes();
    // Trust paths may contain Windows syntax even in tests or imported settings
    // on another host. Match Windows drive/UNC paths case-insensitively there too,
    // without folding case for ordinary Unix paths.
    if cfg!(windows)
        || path.starts_with("//")
        || path.contains('\\')
        || (bytes.len() >= 2 && bytes[1] == b':')
    {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

fn record_trust_prefix(prefixes: &mut Vec<TrustPrefix>, path: &str, kind: agent::trust::TrustKind) {
    let path = invalidation_path(path);
    let descendants = kind == agent::trust::TrustKind::Parent;
    if let Some(existing) = prefixes.iter_mut().find(|p| p.path == path) {
        // If either changed decision governed the whole tree, clearing only the
        // exact project would preserve a prompt built under that old decision.
        existing.descendants |= descendants;
    } else {
        prefixes.push(TrustPrefix { path, descendants });
    }
}

/// Every added, removed, or changed governing decision can affect the projects
/// in its scope. Compare the first decision for each `(path, kind)`: imported or
/// generic-patched settings can contain duplicate keys, and trust resolution uses
/// the first one, so treating the list as an unordered set would miss a reorder.
fn changed_trust_prefixes(
    old: &[agent::trust::TrustDecision],
    new: &[agent::trust::TrustDecision],
) -> Vec<TrustPrefix> {
    let mut keys: Vec<(String, agent::trust::TrustKind)> = Vec::new();
    for d in old.iter().chain(new) {
        let key = (invalidation_path(&d.path), d.kind);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    let mut prefixes = Vec::new();
    for (path, kind) in keys {
        let decision = |list: &[agent::trust::TrustDecision]| {
            list.iter()
                .find(|d| d.kind == kind && invalidation_path(&d.path) == path)
                .map(|d| d.decision)
        };
        if decision(old) != decision(new) {
            record_trust_prefix(&mut prefixes, &path, kind);
        }
    }
    prefixes
}

/// The projects whose per-project instruction selection changed. Unlike trust,
/// a doc rule governs only its exact folder (see `prompt::disabled_docs`), so
/// every one of these is an exact-path invalidation.
fn changed_doc_prefixes(
    old: &[store::ProjectDocRule],
    new: &[store::ProjectDocRule],
) -> Vec<TrustPrefix> {
    let mut keys: Vec<String> = Vec::new();
    for r in old.iter().chain(new) {
        let key = invalidation_path(&r.path);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let mut prefixes = Vec::new();
    for path in keys {
        // The disabled list is a set: order carries no meaning to the loader
        // (`project_files` asks "is this name in there"), so compare sorted —
        // otherwise a hand-edited or re-imported settings.json that reorders one
        // project's list would drop every warm prefix in that project for nothing.
        let rule = |list: &[store::ProjectDocRule]| {
            list.iter()
                .find(|r| invalidation_path(&r.path) == path)
                .map(|r| {
                    let mut d = r.disabled.clone();
                    d.sort();
                    d
                })
        };
        if rule(old) != rule(new) {
            record_trust_prefix(&mut prefixes, &path, agent::trust::TrustKind::Folder);
        }
    }
    prefixes
}

/// Whether `path` is the exact normalized path or a component below `parent`.
/// Exact component containment prevents `/work-old` matching `/work`; path
/// normalization handles both slash styles and Windows casing.
fn path_is_within(parent: &str, path: &str) -> bool {
    let parent = invalidation_path(parent);
    let path = invalidation_path(path);
    if path == parent {
        return true;
    }
    if parent.is_empty() {
        return false;
    }
    if parent == "/" {
        return path.starts_with('/');
    }
    path.strip_prefix(&parent)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

async fn invalidate_trust_prefixes(h: &Harness, prefixes: &[TrustPrefix]) {
    for prefix in prefixes {
        invalidate_prefixes(h, prefix).await;
    }
}

/// Drop frozen prompt state for the projects affected by one trust decision.
/// The task-map lock is released before task locks are awaited, matching the
/// runner's task lookup order and avoiding a map/task lock inversion.
async fn invalidate_prefixes(h: &Harness, prefix: &TrustPrefix) {
    let want = invalidation_path(&prefix.path);
    // Snapshot references before locking tasks: holding the map guard while
    // waiting on a task mutex would invert the runner's task lookup ordering.
    let tasks: Vec<_> = h.tasks.read().await.values().cloned().collect();
    for task in tasks {
        let mut task = task.lock().await;
        let affected = if prefix.descendants {
            path_is_within(&want, &task.project)
        } else {
            invalidation_path(&task.project) == want
        };
        if affected {
            task.system.clear();
            task.mcp_tools.clear();
            task.plugins = Default::default();
        }
    }
}

#[tauri::command]
async fn usage_get(h: H<'_>) -> Result<Value, String> {
    let s = h.settings.read().await;
    Ok(
        json!({"month": s.month_spend(), "spend": s.spend, "tokens": s.tokens_month, "budget": s.budget}),
    )
}

/// What the quota burner would do right now, per account, and the chats it
/// started. Read-only; the queue and thresholds are edited through
/// `settings_update` like every other setting.
#[tauri::command]
async fn burner_status(h: H<'_>) -> Result<agent::burner::Status, String> {
    Ok(agent::burner::status(&h).await)
}

/// Keeps account usage fresh for the UI, and wakes tasks that auto-paused
/// because every account was out once one of them frees up.
async fn watch_accounts(h: Arc<Harness>) {
    let mut tick: u64 = 0;
    loop {
        if tick.is_multiple_of(5) {
            accounts::refresh_all(&h).await;
        }
        if tick.is_multiple_of(30) {
            refresh_pool_models(&h).await;
        }
        let s = h.settings.read().await.clone();
        (h.bus)(
            "ol://accounts",
            serde_json::to_value(h.accts.views(&s.accounts)).unwrap_or_default(),
        );
        if s.automatic_resume && !s.paused_all {
            for t in live_tasks(&h).await {
                let Some(p) = &t.paused else { continue };
                if p.kind != "exhausted" {
                    continue;
                }
                let mut models = vec![t.model.clone()];
                models.extend(t.running_models.iter().cloned());
                let free = models.iter().any(|m| {
                    router::steps(m, &t.route).0.iter().any(|step| {
                        let prov = step.split('/').next().unwrap_or("");
                        if providers::is_pool(prov) {
                            accounts::pool(&h, &s.accounts, prov)
                                .iter()
                                // `usable_for_model`, not `usable`: the router
                                // admits an exhausted account for a *free* model
                                // in the chain, so the stricter check here left
                                // a chat parked that a Resume would have served.
                                .any(|a| h.accts.usable_for_model(a, step))
                        } else if providers::key_pool(&s, prov) {
                            s.providers.get(prov).is_some_and(|c| {
                                c.api_keys
                                    .iter()
                                    .any(|k| !k.is_empty() && h.keys.usable(prov, k))
                            })
                        } else {
                            false
                        }
                    })
                });
                if free {
                    let _ = runner::resume(&h, &t.id, None).await;
                    h.upsert_item(
                        &t.id,
                        Item::new(
                            "notice",
                            "An account or key freed up · resumed automatically",
                            json!({"level": "event", "sum": "resumed automatically"}),
                        ),
                    )
                    .await;
                }
            }
        }
        tick += 1;
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

/// The always-on-top PC-control banner. Created hidden at startup and shown only
/// while an agent is actually driving the mouse/keyboard, so an idle app costs
/// nothing and the user never sees a stray window.
///
/// It is a separate top-level window rather than part of the main UI on purpose:
/// the app is *not* focused while the agent works (it is clicking something
/// else), so anything drawn inside the main window would be invisible exactly
/// when the warning matters most.
fn build_guard_window(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    if let Some(w) = app.get_webview_window(agent::pcguard::BANNER) {
        // This function runs during startup. Reusing a pre-existing window must
        // not make the banner visible while no desktop-control batch is active.
        let _ = w.hide();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        agent::pcguard::BANNER,
        WebviewUrl::App("guard.html".into()),
    )
    .title("OpenLeash is controlling your computer · Esc to stop")
    .inner_size(560.0, 62.0)
    .position(0.0, 0.0)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .shadow(true)
    .visible(false)
    .focused(false)
    .build()?;
    Ok(())
}

/// Show the guard banner, parked at the bottom centre of the monitor the main
/// window is on. Re-anchors on every show: the user moves the app around, and a
/// banner stranded in a corner is worse than none.
pub fn show_guard(app: &tauri::AppHandle, show: bool) {
    use tauri::PhysicalPosition;
    let Some(w) = app.get_webview_window(agent::pcguard::BANNER) else {
        return;
    };
    if !show {
        let _ = w.hide();
        return;
    }
    // Anchor to the monitor, not the main window (which can be minimized while
    // the agent controls another app). Keep physical sizes and positions in the
    // same coordinate space: logical coordinates would scale these a second time.
    let monitor = app
        .get_webview_window("main")
        .and_then(|main| main.current_monitor().ok().flatten())
        .or_else(|| w.primary_monitor().ok().flatten());
    if let (Some(monitor), Ok(size)) = (monitor, w.outer_size()) {
        let pos = monitor.position();
        let screen = monitor.size();
        let margin = (48.0 * monitor.scale_factor()).round() as i32;
        let x = pos.x + (screen.width as i32 - size.width as i32).max(0) / 2;
        let y = pos.y + (screen.height as i32 - size.height as i32 - margin).max(0);
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    // Reassert z-order on every show/state update, not just window creation.
    // Do not focus it: that would redirect the agent's typing into the banner.
    let _ = w.set_always_on_top(true);
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Copy the window's own small icon into its big icon slot, which is the one
/// the taskbar and the Alt-Tab switcher actually read. Without this the button
/// shows a generic web-page glyph even though the app icon is bundled and the
/// exe embeds it correctly.
///
/// The cause is upstream: `tao` applies the configured window icon to
/// `IconType::Small` only (`set_window_icon`, platform_impl/windows/window.rs),
/// and the separate `set_taskbar_icon` path that fills the big slot reads a
/// window attribute nothing in Tauri ever sets. There is no `taskbarIcon` key
/// in the Tauri config schema to work around it, so this is done by hand.
///
/// Reusing the handle `WM_GETICON(ICON_SMALL)` returns keeps the icon in sync
/// with the bundled one without decoding it a second time; `WM_SETICON` does
/// not transfer ownership, so nothing here needs freeing. A no-op off Windows,
/// where the taskbar has no such slot.
#[cfg(target_os = "windows")]
fn set_taskbar_icon(app: &tauri::AppHandle) {
    use std::ffi::c_void;

    #[link(name = "user32")]
    extern "system" {
        fn SendMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    const WM_GETICON: u32 = 0x007F;
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;

    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let Ok(hwnd) = w.hwnd() else { return };
    // `HWND` is a newtype over the raw handle, and tao's event loop owns the
    // message pump, so the round trip has to go through SendMessage rather
    // than a direct call.
    let hwnd = hwnd.0;
    // The handle is an HICON owned by tao's window icon, which outlives this
    // call; `WM_SETICON` does not transfer ownership, so nothing to free.
    let icon = unsafe { SendMessageW(hwnd, WM_GETICON, ICON_SMALL, 0) };
    if icon == 0 {
        return;
    }
    unsafe {
        SendMessageW(hwnd, WM_SETICON, ICON_BIG, icon);
    }
}

#[cfg(not(target_os = "windows"))]
fn set_taskbar_icon(_app: &tauri::AppHandle) {}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("OpenLeash")
        .menu(&menu)
        .show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.on_menu_event(|app, e| match e.id.as_ref() {
        "show" => show_main(app),
        "quit" => {
            let h = app.state::<Arc<Harness>>().inner().clone();
            h.stats.flush();
            // Run the sweep synchronously, and leave only once it is done. The
            // alternative — spawn it and exit at once — is a coin toss on which
            // finishes first, and the freeze and the kills are the entire point of
            // this menu item: an `app.exit` racing them is how a chat ends up
            // half-marked and a build process survives the quit. It is a menu
            // handler, so nothing the user is looking at is blocked, and with
            // nothing in flight it is a handful of locks and one settings write.
            let n = tauri::async_runtime::block_on(h.force_pause_all());
            // Spend is written on a debounce, so the tail of the session is
            // still dirty here. Wait for it before exiting, or the last turns
            // are metered in memory and then dropped.
            tauri::async_runtime::block_on(h.save_settings_now());
            eprintln!(
                "[openleash] quit: stopped {n} chat{}",
                if n == 1 { "" } else { "s" }
            );
            app.exit(0)
        }
        _ => {}
    })
    .on_tray_icon_event(|tray, e| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            show_main(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}

/// Points Windows' toast identity at this app: the AppUserModelID the shell
/// resolves a toast's name and logo from, and the absolute path of the PNG it
/// will draw for that logo.
///
/// The icon has to be a real file on disk, not the `.ico` in `tauri.conf.json`
/// or the raw RGBA the tray icon holds: `IconUri` is resolved by the shell in a
/// different process than ours, from a string, so anything that is not a path on
/// disk comes back as no logo at all. The bundled 32x32 PNG is the smallest
/// thing present in every build, which is also the size the toast asks for.
fn register_toast_identity(app: &tauri::App) {
    let cfg = app.config();
    let id = cfg.identifier.clone();
    let name = cfg.product_name.clone().unwrap_or_else(|| id.clone());
    let icon = app
        .default_window_icon()
        .cloned()
        .and_then(|i| encode_png(i.rgba(), i.width(), i.height()))
        .and_then(|png| toast_icon_file(&png));
    win::register(&id, &name, icon.as_deref().and_then(|p| p.to_str()));
}

/// Emitted when a desktop toast is clicked, carrying the chat it was raised for.
/// The frontend listens and opens that chat; the window has already been raised
/// by the time this goes out.
pub const TOAST_CLICK: &str = "ol://toast-click";

/// The toast-click hand-off between the COM activator and the window.
///
/// A click can arrive before the window is listening for one: when the app was
/// closed, the shell launches it to deliver the click, and the activation lands
/// during setup, while the frontend is still booting. An event emitted then
/// reaches nothing, so the click is held as `pending` until the frontend calls
/// `take_toast_click` (which also marks it `listening`). After that, clicks go
/// straight out as `TOAST_CLICK` events.
#[derive(Default)]
struct ToastClicks {
    listening: bool,
    pending: Option<String>,
}

static TOAST_CLICKS: std::sync::Mutex<ToastClicks> = std::sync::Mutex::new(ToastClicks {
    listening: false,
    pending: None,
});

/// The chat a toast click asked for, if one arrived before the window was
/// listening. Also marks the window as listening from here on, so the caller
/// must invoke this only once its `TOAST_CLICK` listener is attached.
#[tauri::command]
fn take_toast_click() -> Option<String> {
    let mut clicks = TOAST_CLICKS.lock().unwrap_or_else(|e| e.into_inner());
    clicks.listening = true;
    clicks.pending.take()
}

/// Show a desktop toast that opens `task_id` when clicked.
///
/// This exists instead of `sendNotification` because the plugin's desktop path
/// cannot produce a clickable toast: it builds its XML from fixed `text1`/
/// `text2` fields with nowhere to put a `launch` argument, and it drops the
/// `NotificationHandle` that owns notify-rust's activation channel. On Windows
/// the document is built here so the argument survives, and paired with the
/// activator `win::install` registers. Off Windows it falls back to the plugin,
/// which is the only toast implementation there anyway.
///
/// A `task_id` that is not a task id still produces a toast — one that raises
/// the app without naming a chat. Refusing to show it would turn a cosmetic
/// problem into a missing notification.
#[tauri::command]
fn toast_show(app: tauri::AppHandle, task_id: String, title: String, body: String) {
    // The AppUserModelID is the config's `identifier` — the same string
    // `register_toast_identity` registered at startup. Read from the config
    // rather than repeated as a literal here, because a toast raised under an id
    // the shell has no registration for is dropped without an error.
    let app_id = app.config().identifier.clone();
    win::show_toast(&app_id, &title, &body, &task_id);
}

/// Wire a clicked toast to the chat it was raised for: raise the window, then
/// tell the frontend which chat to open.
///
/// The window is raised here rather than in the frontend because a click can
/// land while the app is hidden to the tray, and `show_main` is the same call
/// the tray icon makes — one way to bring the window back, not two that drift.
///
/// A click carrying no chat still raises the window: the user asked for
/// OpenLeash, and refusing to show it because the toast was from a build that
/// predates the `launch` argument would be worse than showing the app on
/// whatever chat was last open.
fn install_toast_click_handler(app: &tauri::AppHandle) {
    let handle = app.clone();
    let sink: std::sync::Arc<dyn Fn(Option<String>) + Send + Sync> =
        std::sync::Arc::new(move |task: Option<String>| {
            show_main(&handle);
            let Some(task) = task else { return };
            let listening = {
                let mut clicks = TOAST_CLICKS.lock().unwrap_or_else(|e| e.into_inner());
                if !clicks.listening {
                    clicks.pending = Some(task.clone());
                }
                clicks.listening
            };
            if listening {
                use tauri::Emitter;
                let _ = handle.emit(TOAST_CLICK, json!({ "task_id": task }));
            }
        });
    // Best-effort: a click that cannot be delivered leaves the user with the
    // toast they had before, which is strictly better than an app that will not
    // start. See `win::install`.
    if let Err(e) = win::install(sink) {
        eprintln!("[openleash] toast: no click handler, toasts won't open a chat: {e}");
    }
}

/// RGBA → PNG bytes. The bundled icon is stored as raw RGBA, and `IconUri` needs
/// a path to a real image file, so it has to be encoded first.
fn encode_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(buf)
}

/// Caches the encoded PNG in the app's own cache dir and returns its path.
///
/// The shell reads `IconUri` on every toast, so it has to keep resolving to a
/// file after this process exits — the next launch has to land on the same path,
/// or a toast raised from a background run outlives us by pointing at nothing.
/// Keyed off the product, not the exe, so a rebuild in place overwrites rather
/// than accumulating.
fn toast_icon_file(png: &[u8]) -> Option<std::path::PathBuf> {
    let dir = dirs::cache_dir()?.join("openleash");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("toast-icon.png");
    // Rewrite only when the bytes differ: this runs on every launch, and
    // rewriting unconditionally churns the file the shell may be reading.
    if std::fs::read(&path).ok().as_deref() != Some(png) {
        std::fs::write(&path, png).ok()?;
    }
    Some(path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(visualization::Registry::default())
        .manage(updates::Updates::default())
        .register_uri_scheme_protocol("openleash-viz", |context, request| {
            visualization::respond(context.app_handle(), request)
        })
        // First plugin on purpose. Tauri runs every plugin's `setup` hook at the
        // end of `Builder::build()`, which is before this app's own `setup` and
        // before the configured windows are built — so a second launch is
        // stopped here, having created no window and having run no part of the
        // harness. First among the plugins too: the ones below would otherwise
        // each get to touch shared state (the debounced task writer and the
        // stats flush are both unconditional) in a process that is about to
        // exit, and two writers on one settings/tasks file is the bug this
        // exists to prevent.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second launch: raise the window we already have rather than
            // starting a rival. `show_main` rather than a bare `show` because
            // close-to-tray means the window this finds is often hidden, and
            // because the user can have minimised it — clicking the taskbar
            // icon again and seeing nothing happen is the complaint this whole
            // plugin answers.
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Before anything can emit a toast: an unregistered AppUserModelID
            // is dropped by the Windows shell without an error, so this has to
            // happen on the way in rather than lazily on the first notification.
            // Best-effort and never fatal — see `win::register`.
            register_toast_identity(app);
            let settings = store::load_settings();
            // Resume trace export if the user has named a collector: the exporter
            // has to be started once per process, and boot is the only place that
            // knows the saved endpoint before the settings are moved into the
            // harness. A no-op when the field is empty, which is its default, and
            // a bad endpoint is reported in Settings rather than at boot.
            let _ = trace::configure(&settings.trace_endpoint);
            sync_model_state(&settings);
            router::set_routes(&settings.routes);
            // Publish the trust decisions to the process-global table the prompt
            // path reads. Done here because the prompt is built from an `Env`
            // that carries no settings — see `agent::trust::set_trust`.
            agent::trust::set_trust(&settings.trust);
            let mcp_cfgs = settings.mcp.clone();
            let mut tasks = HashMap::new();
            for t in store::load_tasks() {
                // Registering only records the env: no docker command runs at
                // launch unless the user actually has docker chats.
                if let Some(d) = &t.docker {
                    agent::docker::register(&t.id, d);
                }
                tasks.insert(t.id.clone(), Arc::new(Mutex::new(t)));
            }
            let handle = app.handle().clone();
            let h = Arc::new(Harness {
                bus: Arc::new(move |ev: &str, payload: Value| {
                    use tauri::Emitter;
                    let _ = handle.emit(ev, payload);
                }),
                tasks: RwLock::new(tasks),
                runtimes: Default::default(),
                settings: RwLock::new(settings),
                bg: Default::default(),
                mcp: Default::default(),
                http: reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(20))
                    .build()
                    .expect("http client"),
                pause_bell: Default::default(),
                accts: Default::default(),
                keys: Default::default(),
                stats: agent::stats::Stats::load(),
                dirty: Default::default(),
                saver: Default::default(),
                settings_dirty: Default::default(),
                me: Default::default(),
            });
            // The debounced task writer needs an `Arc` to call back through.
            agent::Harness::register(&h);
            app.manage(h.clone());
            let h2 = h.clone();
            tauri::async_runtime::spawn(async move { h2.mcp.sync(&mcp_cfgs).await });
            let h3 = h.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    h3.stats.flush();
                }
            });
            tauri::async_runtime::spawn(watch_accounts(h.clone()));
            // Off unless Settings → Accounts → Spend quota before it resets is on;
            // with it off and no live burner chat a pass returns at once.
            tauri::async_runtime::spawn(agent::burner::watch(h.clone()));
            // Time in the app: sampled, not event-driven, so a hidden or
            // minimised window and a missed focus event both just read as
            // "not focused" instead of leaving a counter running.
            let h_presence = h.clone();
            let app_presence = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let focused = app_presence
                        .get_webview_window("main")
                        .map(|w| w.is_focused().unwrap_or(false) && w.is_visible().unwrap_or(false))
                        .unwrap_or(false);
                    h_presence
                        .stats
                        .tick(focused, any_chat_running(&h_presence));
                }
            });
            tauri::async_runtime::spawn(docker_reaper(h.clone()));
            tauri::async_runtime::spawn(agent::lsp::reaper());
            // Chats sent from another machine while this app wasn't running.
            tauri::async_runtime::spawn(agent::transfer::import_pending(h.clone()));
            // A global pause left on by a quit finds nothing to hold once the tasks
            // are loaded: `load_tasks` turns anything that was running into
            // `stopped`, and the freeze only ever held working chats. Reconciled as
            // soon as the runtime is up, so a launch never opens onto a banner
            // announcing a pause that is holding nothing. The frontend also gates
            // its banner on this, but only the flag can actually be cleared.
            {
                let h4 = h.clone();
                tauri::async_runtime::spawn(async move {
                    if h4.clear_stranded_global_pause().await {
                        eprintln!(
                            "[openleash] start: cleared a global pause that was holding nothing"
                        );
                    }
                });
            }
            // Chat-platform connectors (Discord, Telegram, Slack): off until configured, and
            // back with the app when they are, like remote control.
            app.manage(connectors::Connectors::start(h.clone()));
            // Scheduled runs: loads schedules.json and starts the loop, which
            // fires enabled schedules whether or not their screen is shown.
            app.manage(agent::schedule::Scheduler::start(h.clone()));
            // The PC-control guard needs the harness (to tell the agent about
            // an objection) and its own always-on-top window.
            agent::pcguard::set_harness(h.clone());
            agent::pcguard::install_esc_hook();
            // The guard owns the policy; the app owns the window. It listens for
            // the same event the frontend does and shows/hides the banner, so a
            // batch that starts while the main window is unfocused still raises
            // it.
            {
                let app_h = app.handle().clone();
                use tauri::Listener;
                let owned = app_h.clone();
                app_h.listen(agent::pcguard::EVENT, move |e| {
                    // The bus is a `Fn`, not a typed emitter, so the payload
                    // arrives as a JSON string.
                    let show: bool = serde_json::from_str::<Value>(e.payload())
                        .map(|v| {
                            v["active"].as_bool().unwrap_or(false)
                                || v["objected"].as_bool().unwrap_or(false)
                        })
                        .unwrap_or(false);
                    show_guard(&owned, show);
                });
            }
            build_guard_window(app.handle())?;
            build_tray(app)?;
            set_taskbar_icon(app.handle());
            install_toast_click_handler(app.handle());
            updates::start(app.handle().clone(), h.clone());
            // Remote control is off unless the user turned it on; when it is,
            // it comes back with the app so a paired phone keeps working.
            let remote = remote::Remote::load();
            app.manage(remote.clone());
            remote.listen_attention(app.handle());
            if remote.enabled() {
                remote.apply(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray: hide instead of quitting, so running agents keep going.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let h = window.app_handle().state::<Arc<Harness>>().inner().clone();
                let to_tray = h
                    .settings
                    .try_read()
                    .map(|s| s.close_to_tray)
                    .unwrap_or(true);
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            connectors::discord_status,
            connectors::discord_configure,
            connectors::discord_set_token,
            connectors::discord_forget_chats,
            agent::schedule::schedules_list,
            agent::schedule::schedule_save,
            agent::schedule::schedule_delete,
            agent::schedule::schedule_set_enabled,
            agent::schedule::schedule_run_now,
            agent::schedule::schedules_set_paused,
            connectors::telegram_status,
            connectors::telegram_configure,
            connectors::telegram_set_token,
            connectors::telegram_test,
            connectors::telegram_forget_chats,
            connectors::slack_status,
            connectors::slack_configure,
            connectors::slack_set_token,
            connectors::slack_test,
            connectors::slack_forget_chats,
            remote::remote_status,
            remote::remote_configure,
            remote::remote_new_code,
            remote::remote_revoke,
            remote::remote_reset,
            remote::remote_ntfy_configure,
            remote::remote_push_disable,
            remote::remote_list_dir,
            remote::ssh_hosts_list,
            remote::ssh_host_save,
            remote::ssh_host_remove,
            remote::ssh_connect,
            remote::ssh_disconnect,
            remote::ssh_projects::ssh_host_test,
            remote::ssh_projects::ssh_list_dir,
            remote::transfer::transfer_open,
            remote::transfer::transfer_list_dir,
            remote::transfer::transfer_close,
            remote::transfer::transfer_check,
            remote::transfer::transfer_send,
            agent::transfer::task_import_bundle,
            agent::transfer::transfer_probe,
            docker_project_info,
            docker_add_template,
            container_start,
            container_stop,
            container_remove,
            task_change_folder,
            docker_rebuild,
            sandbox_list,
            sandbox_prepare,
            sandbox_image_set,
            sandbox_create,
            sandbox_update,
            sandbox_start,
            sandbox_stop,
            sandbox_reset,
            sandbox_delete,
            sandbox_clone,
            sandbox_snapshot,
            sandbox_restore,
            sandbox_snapshot_delete,
            sandbox_control,
            sandbox_thumb,
            sandbox_files,
            sandbox_files_import,
            sandbox_files_export,
            sandbox_files_delete,
            task_set_sandbox,
            sandbox_publish,
            docker_push_branch,
            workspaces_list,
            workspace_prune,
            updates::app_update_check,
            updates::app_update_download,
            updates::app_update_install,
            updates::app_update_now,
            updates::app_update_status,
            visualization::visualization_publish,
            visualization::visualization_read,
            visualization::visualization_release,
            artifict_list,
            artifict_get,
            artifict_create,
            artifict_revise,
            artifict_restore,
            artifict_edit,
            artifict_export,
            artifict_annotate,
            artifict_annotate_remove,
            artifict_feedback_submit,
            artifict_feedback_list,
            artifict_feedback_respond,
            app_boot,
            trace_configure,
            tasks_list,
            task_get,
            task_open,
            task_seen,
            task_history,
            sub_report,
            sub_transcript,
            task_create,
            race_create,
            race_list,
            race_detail,
            race_judge,
            race_pick,
            race_merge,
            race_archive,
            race_forget,
            race_delete,
            route_dry_run,
            route_situation,
            route_log,
            task_send,
            task_interrupt,
            task_stop,
            pcguard_state,
            pcguard_stop,
            task_respond,
            task_answer_nonblocking,
            task_dismiss_notice,
            task_update,
            task_delete,
            tasks_delete_archived,
            stats_get,
            tasks_archive,
            tasks_export,
            task_rewind,
            task_rewind_info,
            task_rewind_to,
            task_fork_at,
            task_queue_remove,
            task_queue_edit,
            task_queue_move,
            task_queue_send_now,
            task_queue_send_all,
            task_review,
            task_visual_diff,
            task_visual_recapture,
            task_visual_clear,
            task_review_stats,
            task_revert_file,
            task_bisect_edits,
            task_bisect_run,
            task_bisect_cancel,
            task_commit,
            task_commit_message,
            bg_kill,
            sub_set_model,
            sub_stop,
            sub_set_effort,
            sub_send,
            draft_get,
            stats_chat,
            task_run_timeline,
            draft_set,
            task_pause,
            task_status_summary,
            open_path,
            remote_stage_upload,
            files_attach,
            file_data_url,
            staged_image_url,
            reply_image,
            task_force_pause,
            force_pause_all,
            task_resume,
            tasks_resume,
            tasks_dismiss_pause,
            task_broadcast,
            task_dismiss_pause,
            pause_all,
            resume_all,
            tasks_message,
            accounts_list,
            account_import,
            account_import_local,
            account_update,
            account_remove,
            account_cap_set,
            account_cap_clear,
            accounts_refresh,
            agents_list,
            skills_list,
            skill_import,
            skill_remove,
            skill_set_enabled,
            skill_read,
            skill_draft_from_chat,
            skill_draft_cancel,
            skill_save_from_chat,
            pool_models_refresh,
            task_export,
            task_training_export,
            tasks_export_all,
            agent::transfer::tasks_import,
            task_duplicate,
            task_fork,
            provider_insist,
            provider_key,
            settings_update,
            hooks_save,
            hooks_status,
            hooks_trust,
            provider_set,
            models_list,
            provider_add,
            antigravity_status,
            provider_remove,
            model_save,
            model_remove,
            model_set_enabled,
            provider_models,
            model_detail,
            mcp_oauth_start,
            mcp_oauth_wait,
            mcp_oauth_cancel,
            mcp_oauth_disconnect,
            mcp_status,
            mcp_reconnect,
            plugins_status,
            browser_panel,
            browser_annotate,
            plugin_github_token,
            plugin_secret_set,
            plugin_service_test,
            github_list_prs,
            github_login_start,
            github_login_wait,
            github_login_cancel,
            models_in_use,
            models_swap,
            project_info,
            project_branch_create,
            agent::projects::wsl_list,
            agent::projects::wsl_start_dir,
            agent::projects::clone_preview,
            agent::projects::clone_repo,
            trust_manifest,
            trust_set,
            trust_clear,
            import_scan,
            import_apply,
            usage_get,
            burner_status,
            toast_show,
            take_toast_click
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // ssh children are not killed with us on every platform; an
                // orphaned forward would keep a port open to another machine.
                if let Some(r) = app.try_state::<Arc<remote::Remote>>() {
                    r.shutdown();
                }
                if let Some(c) = app.try_state::<Arc<connectors::Connectors>>() {
                    c.shutdown();
                }
                // Language servers live in a process-wide static, which is never
                // dropped, so `kill_on_drop` cannot end them: shut them down
                // here, bounded so a hung server cannot hold the exit.
                tauri::async_runtime::block_on(async {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        agent::lsp::global().shutdown_all(),
                    )
                    .await;
                });
            }
        });
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    #[test]
    fn strict_worktree_requirement_cannot_fall_back_to_the_project() {
        assert!(strict_worktree_ready(false, false, "C:/repo", "C:/repo"));
        assert!(!strict_worktree_ready(true, false, "C:/repo", "C:/repo"));
        assert!(!strict_worktree_ready(true, true, "C:/repo", "C:/repo"));
        assert!(strict_worktree_ready(true, true, "C:/repo/.openleash-worktree/task", "C:/repo"));
    }

    use super::*;
    use crate::agent::runner;
    use crate::agent::store::Route;
    use crate::agent::{Message, QueuedMsg};

    /// The model a new task starts on is the model the last new task ran on.
    ///
    /// It used to be only the build's default: `settings.model` was written by
    /// the model swap dialog and by nothing else, so the model the user picked
    /// in the composer was remembered in `recent_models` and nowhere else. The
    /// new-task screen then came up on a model the user had never chosen, and
    /// had to be told which model to use again for every new task.
    ///
    /// Only an *interactive* task may move it. A chat that arrived from a
    /// connector, or one attempt of a race, runs on a model the user did not
    /// pick for their next task, and letting it write this field put the
    /// new-task screen on a model nobody chose there.
    #[tokio::test]
    async fn the_last_new_tasks_model_becomes_the_default_for_the_next_one() {
        let dir = std::env::temp_dir().join("openleash-last-model");
        let _g = crate::agent::store::test_home(&dir);
        let settings = crate::agent::store::Settings {
            model: "anthropic/claude-opus-5".into(),
            ..Default::default()
        };
        let (h, _) = crate::agent::tests::harness(settings, vec![]);

        // A task the user started from the composer, on a model they chose.
        remember_task_creation(
            &h,
            "D:/work/app",
            "openrouter/some/model",
            1,
            CreateTaskOptions::Interactive,
        )
        .await;
        {
            let s = h.settings.read().await;
            assert_eq!(s.model, "openrouter/some/model");
            assert_eq!(s.effort, 1);
            assert_eq!(s.recent_models[0], "openrouter/some/model");
        }

        // A task that arrived from somewhere else does not move it.
        remember_task_creation(
            &h,
            "D:/work/app",
            "codex/some/other-model",
            0,
            CreateTaskOptions::External,
        )
        .await;
        {
            let s = h.settings.read().await;
            assert_eq!(
                s.model, "openrouter/some/model",
                "a connector's or race's model must not become the new-task default"
            );
            assert_eq!(s.effort, 1, "and neither must its reasoning level");
            // It is still remembered as recently used, which is what that list is.
            assert_eq!(s.recent_models[0], "codex/some/other-model");
        }

        // A scheduled run writes nothing at all.
        remember_task_creation(
            &h,
            "D:/work/app",
            "claude/some/scheduled-model",
            3,
            CreateTaskOptions::Background,
        )
        .await;
        {
            let s = h.settings.read().await;
            assert_eq!(s.model, "openrouter/some/model");
            assert_eq!(s.effort, 1);
            assert_eq!(s.recent_models[0], "codex/some/other-model");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A timeline with `n` items named `m0..m{n}`, oldest first.
    fn timeline(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item::new("text", format!("m{i}"), json!({})))
            .collect()
    }

    fn ids(items: &[Item]) -> Vec<String> {
        items.iter().map(|i| i.id.clone()).collect()
    }

    /// Paging on an index drifts when live events mutate the array between
    /// requests, so a page repeats rows the reader already has or skips past
    /// others. Anchoring on an item id is what makes this stable, and this is the
    /// case that regression was written for.
    #[test]
    fn a_page_does_not_shift_when_an_earlier_row_is_removed_mid_conversation() {
        let mut items = timeline(6);
        let first = timeline_page(&items, None, 6);
        assert_eq!(first.items.len(), 6, "the newest page is the whole history");
        let cursor = first.cursor.clone().unwrap();

        // A live `itemgone` takes the oldest row back between the two requests.
        // With an index cursor the page would start one row late and drop `m0`
        // from what the reader can ever scroll back to.
        let oldest = items[0].id.clone();
        items.retain(|i| i.id != oldest);

        let page = timeline_page(&items, Some(&cursor), 6);
        assert_eq!(
            ids(page.items),
            ids(&items),
            "the page must be the rows before the cursor, exactly"
        );
    }

    /// Walking the cursors back to the end yields the whole timeline, once each,
    /// in order — the invariant the scroll-up loader depends on.
    #[test]
    fn walking_every_page_reconstructs_the_timeline_exactly_once() {
        let items = timeline(7);
        let first = timeline_page(&items, None, 3);
        // Mirror the loader (`src/store.ts`'s `loadPreviousHistory`): hold what is
        // loaded and splice each older page *above* it. Collecting pages by
        // appending instead would order them newest-page-first and pins nothing
        // about what the user actually sees.
        let mut held = ids(first.items);
        let mut cursor = first.cursor;
        let mut pages = 1;
        while let Some(c) = cursor {
            let page = timeline_page(&items, Some(&c), 3);
            held.splice(0..0, ids(page.items));
            cursor = page.cursor;
            pages += 1;
            assert!(pages < 50, "paging must terminate");
        }
        assert_eq!(
            held,
            ids(&items),
            "the walk must rebuild the timeline exactly, in order"
        );
    }

    /// A cursor naming a row a live event already removed must fall back to the
    /// newest page, never wrap around and resend history the reader holds.
    #[test]
    fn a_cursor_for_a_removed_item_starts_from_the_newest_page() {
        let items = timeline(5);
        let gone = format!("gone-{}", uuid::Uuid::new_v4());
        let page = timeline_page(&items, Some(&gone), 10);
        assert_eq!(
            ids(page.items),
            ids(&items),
            "an unknown cursor is the newest page"
        );
    }

    /// Every page is bounded, so one long chat can never be shipped in a single
    /// response however large it grows.
    #[test]
    fn a_page_is_always_bounded() {
        let items = timeline(1000);
        let page = timeline_page(&items, None, 1_000_000);
        assert_eq!(
            page.items.len(),
            200,
            "the page clamp caps what one call returns"
        );
        assert_eq!(page.total, 1000, "the total is still reported in full");
    }

    #[tokio::test]
    async fn resume_all_does_not_refresh_chats_that_were_not_paused() {
        let h = Arc::new(harness(vec![
            chat("running"),
            chat("waiting"),
            chat("stopped"),
        ]));
        let mut before_state = Vec::new();
        for (id, status, order) in [
            ("running", "running", 1_000.0),
            ("waiting", "waiting", 2_000.0),
            ("stopped", "stopped", 3_000.0),
        ] {
            let task = h.task(id).await.unwrap();
            let mut task = task.lock().await;
            task.status = status.into();
            task.order = order;
            before_state.push((id.to_string(), task.updated_at, task.order));
        }

        resume_all_tasks(&h, summaries(&h).await, None, false).await;

        for (id, updated_at, order) in before_state {
            let task = h.task(&id).await.unwrap();
            let task = task.lock().await;
            assert_eq!(
                task.updated_at, updated_at,
                "chat {id} that was not paused must not be touched"
            );
            assert_eq!(
                task.order, order,
                "chat {id} that was not paused must keep its manual position"
            );
        }
    }

    /// "Pause all" has to freeze first and walk the chats afterwards.
    ///
    /// That walk waits on each chat's lock in turn, and a chat in the middle of a
    /// turn holds one — so flipping the flag at the end of it left *nothing*
    /// frozen for as long as any single chat took to answer, which is the shape of
    /// the "Pause all doesn't work / takes a while" report. The freeze is the flag
    /// and the settings event that carries it, and both have to be out before the
    /// walk starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_all_freezes_before_it_walks_the_chats() {
        let dir = std::env::temp_dir().join(format!("openleash-pause-all-{}", agent::new_id()));
        let _home = store::test_home(&dir);

        // The settings event is the backend saying "the freeze is on": the UI's
        // flag, its banner and the chip's acknowledgement all come from it.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        let bus: agent::Bus = Arc::new(move |name: &str, payload: Value| {
            if name == "ol://settings" {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(payload);
                }
            }
        });

        let mut opted_out = chat("opted-out");
        opted_out.status = "running".into();
        // A chat that opted out of an earlier global pause (by messaging it under
        // one). The walk is what brings it under *this* pause, so it still matters.
        opted_out.unpaused = true;
        let h = Arc::new(harness_with_bus(vec![opted_out, chat("idle")], bus));

        // Hold that chat's lock, exactly as a chat mid-turn does.
        let held = h.task("opted-out").await.unwrap();
        let lock = held.lock().await;
        let walk = {
            let h = h.clone();
            tokio::spawn(async move { pause_all_chats(&h).await })
        };

        let payload = tokio::time::timeout(std::time::Duration::from_secs(2), rx)
            .await
            .expect("the freeze must land while the walk is still stuck on a chat's lock")
            .expect("the settings event is the UI's flag");
        assert_eq!(
            payload["paused_all"],
            json!(true),
            "and it says every chat is frozen"
        );
        assert!(
            h.settings.read().await.paused_all,
            "the flag every gate reads is up"
        );

        drop(lock);
        walk.await.unwrap().unwrap();
        assert!(
            !h.task("opted-out").await.unwrap().lock().await.unpaused,
            "the walk still brings an opted-out chat under the pause"
        );
    }

    #[tokio::test]
    async fn bootstraps_a_new_chat_before_background_workspace_setup() {
        let dir = std::env::temp_dir().join(format!("openleash-startup-{}", agent::new_id()));
        let _home = store::test_home(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.to_string_lossy().to_string();
        let h = Arc::new(harness(vec![]));
        let id = "startup".to_string();
        let task = chat(&id);
        let mut task = task;
        task.project = path.clone();
        task.cwd = path;
        let startup = Item::new(
            "notice",
            "Preparing your first message…",
            json!({"startup": true}),
        );
        let _startup_id = startup.id.clone();
        task.items.push(startup);
        h.tasks
            .write()
            .await
            .insert(id.clone(), Arc::new(Mutex::new(task)));
        let rt = h.runtime(&id);
        rt.startup_pending.store(true, Ordering::SeqCst);
        let startup_guard = StartupGuard::new(rt.clone());
        let task_ref = h.task(&id).await.unwrap();
        let task_guard = task_ref.lock().await;
        store::save_task(&task_guard);
        drop(task_guard);

        let wait_h = h.clone();
        let wait_id = id.clone();
        let waiting = tokio::spawn(async move { wait_for_task_startup(&wait_h, &wait_id).await });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "follow-up must wait while workspace setup is pending"
        );

        prepare_task_workspace(
            &h,
            &id,
            &project.to_string_lossy(),
            "",
            false,
            false,
            "initial prompt",
        )
        .await;
        assert!(
            rt.startup_pending.load(Ordering::SeqCst),
            "workspace setup keeps first-message gate until send dispatches"
        );
        let result = dispatch(h.clone(), id.clone(), "first prompt".to_string()).await;
        assert!(result.is_ok());
        drop(startup_guard);
        assert!(!rt.startup_pending.load(Ordering::SeqCst));
        assert!(waiting.await.is_ok());
        assert!(
            store::read_task_full(&store::task_path(&id)).is_ok(),
            "the task body is persisted"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stopping_a_chat_cancels_startup_and_releases_waiting_sends() {
        let dir = std::env::temp_dir().join(format!("openleash-startup-stop-{}", agent::new_id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![chat("startup-stop")]));
        let runtime = h.runtime("startup-stop");
        runtime.startup_pending.store(true, Ordering::SeqCst);
        let guard = StartupGuard::new(runtime.clone());
        let cancel = guard.cancel.clone();
        let startup = tokio::spawn(async move {
            cancel.cancelled().await;
            drop(guard);
        });
        let wait_h = h.clone();
        let waiting =
            tokio::spawn(async move { wait_for_task_startup(&wait_h, "startup-stop").await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        runner::stop_tokens(&h, "startup-stop");
        startup.await.unwrap();
        assert!(!runtime.startup_pending.load(Ordering::SeqCst));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("stop releases waiting sends")
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn deleting_a_chat_cancels_startup_and_releases_waiting_sends() {
        let dir =
            std::env::temp_dir().join(format!("openleash-startup-delete-{}", agent::new_id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![chat("startup-delete")]));
        let runtime = h.runtime("startup-delete");
        runtime.startup_pending.store(true, Ordering::SeqCst);
        let guard = StartupGuard::new(runtime.clone());
        let cancel = guard.cancel.clone();
        let startup = tokio::spawn(async move {
            cancel.cancelled().await;
            drop(guard);
        });
        let wait_h = h.clone();
        let waiting =
            tokio::spawn(async move { wait_for_task_startup(&wait_h, "startup-delete").await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        let delete_h = h.clone();
        tokio::task::spawn_blocking(move || purge_task(&delete_h, "startup-delete", false))
            .await
            .unwrap()
            .unwrap();
        startup.await.unwrap();

        assert!(!runtime.startup_pending.load(Ordering::SeqCst));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("deletion releases waiting sends")
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_task_without_permission_mode_defaults_to_full_access() {
        let req: NewTask = serde_json::from_value(json!({
            "prompt": "test",
            "project": "project",
            "model": "test-model",
            "effort": 0,
            "plan": false,
            "worktree": false,
            "base_branch": ""
        }))
        .unwrap();
        assert_eq!(req.perm, "turbo");

        let explicit: NewTask = serde_json::from_value(json!({
            "prompt": "test",
            "project": "project",
            "model": "test-model",
            "effort": 0,
            "perm": "auto",
            "plan": false,
            "worktree": false,
            "base_branch": ""
        }))
        .unwrap();
        assert_eq!(explicit.perm, "auto");
    }

    #[test]
    fn hook_trust_actions_bind_identity_to_the_open_origin() {
        let project_dir =
            std::env::temp_dir().join(format!("openleash-hook-trust-{}", agent::new_id()));
        std::fs::create_dir_all(project_dir.join(".openleash")).unwrap();
        let project = project_dir.to_string_lossy().to_string();
        std::fs::write(
            project_dir.join(".openleash/hooks.json"),
            r#"[{"id":"shared","event":"stop","command":"echo project"}]"#,
        )
        .unwrap();

        let mut s = Settings {
            project: project.clone(),
            ..Default::default()
        };
        // Same visible id may exist in user and project scopes; approval must
        // use the exact pair, never the first entry with this id.
        s.hooks.push(agent::checks::Hook {
            id: "project:shared".into(),
            event: "stop".into(),
            command: "echo user".into(),
            enabled: true,
            source: "user".into(),
            ..Default::default()
        });

        assert!(
            update_hook_trust(&mut s, "project:shared", "D:/not-the-open-project", true).is_err()
        );
        assert!(
            s.trusted_hooks.is_empty(),
            "a caller cannot approve another origin"
        );

        update_hook_trust(&mut s, "project:shared", "", true).unwrap();
        update_hook_trust(&mut s, "project:shared", &project, true).unwrap();
        assert_eq!(
            s.trusted_hooks.len(),
            2,
            "same id has separate user/project approvals"
        );
        assert!(s
            .trusted_hooks
            .iter()
            .any(|a| a.id == "project:shared" && a.origin.is_empty()));
        assert!(s
            .trusted_hooks
            .iter()
            .any(|a| a.id == "project:shared" && a.origin == project));

        // A matching id at a different project origin must survive revoking the
        // open project's approval. This reproduces the formerly ambiguous lookup.
        let mut elsewhere = s
            .trusted_hooks
            .iter()
            .find(|a| a.origin == project)
            .unwrap()
            .clone();
        elsewhere.origin = "D:/different-project".into();
        s.trusted_hooks.push(elsewhere);
        update_hook_trust(&mut s, "project:shared", &project, false).unwrap();
        assert!(!s
            .trusted_hooks
            .iter()
            .any(|a| a.id == "project:shared" && a.origin == project));
        assert!(s
            .trusted_hooks
            .iter()
            .any(|a| a.id == "project:shared" && a.origin.is_empty()));
        assert!(s
            .trusted_hooks
            .iter()
            .any(|a| a.id == "project:shared" && a.origin == "D:/different-project"));

        let _ = std::fs::remove_dir_all(project_dir);
    }

    /// `/btw` ends at a word boundary, the same one Claude Code and Codex use.
    /// `/btwx` is a message about a variable: swallowing it into a side question
    /// would answer something the user never asked *and* leave the real message
    /// undelivered, which reads as the app mishearing them.
    #[test]
    fn btw_stops_at_a_word_boundary() {
        assert_eq!(btw_arg(" how many retries?"), Some("how many retries?"));
        assert_eq!(btw_arg(""), Some(""));
        assert_eq!(btw_arg("   "), Some(""));
        assert_eq!(btw_arg("\thow many?"), Some("how many?"));
        // `rest` is what follows the literal `/btw`, so these are the no-space
        // forms. Punctuation is not a word character, which means `/btw?` and
        // `/btw--verbose` are still this command.
        assert_eq!(btw_arg("?"), Some("?"));
        assert_eq!(btw_arg("--verbose?"), Some("--verbose?"));
        // A word character straight after `/btw` continues the word instead, so
        // `/btwwhy` is a message about the letters and not a side question.
        for longer in ["why?", "x", "wx", "ish hello", "_x", "42"] {
            assert_eq!(
                btw_arg(longer),
                None,
                "`{longer}` is a longer word, so the line is an ordinary message"
            );
        }
    }

    /// The Claude subscription integration is an opt-in decided by a file beside
    /// the running executable — not a setting (which would travel with
    /// `OPENLEASH_HOME` between installs) and not a source-tree flag. This pins
    /// the read: the exe's own folder, the marker being a regular file, and its
    /// contents never mattering.
    #[test]
    fn claude_is_off_until_an_allow_claude_file_sits_beside_the_executable() {
        let dir = std::env::temp_dir().join(format!("openleash-allow-claude-{}", agent::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("openleash.exe");

        assert!(!claude_allowed_in(&dir), "an unmarked install stays off");
        assert!(!claude_allowed_with(Some(&exe)));
        // No executable path at all is unusual, and the safe answer is closed
        // rather than opening the gate on a directory the user never marked.
        assert!(!claude_allowed_with(None));

        // A *directory* called ALLOW_CLAUDE is not the marker; only a file is.
        std::fs::create_dir_all(dir.join(ALLOW_CLAUDE)).unwrap();
        assert!(!claude_allowed_in(&dir));
        std::fs::remove_dir_all(dir.join(ALLOW_CLAUDE)).unwrap();

        // Contents are never read, so a stray "false"/newline/BOM from an editor
        // still counts as opted in.
        std::fs::write(dir.join(ALLOW_CLAUDE), "false\n").unwrap();
        assert!(claude_allowed_in(&dir));
        assert!(
            claude_allowed_with(Some(&exe)),
            "the marker is read from the executable's own folder"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Hiding the provider in the UI is not the gate. The import paths have to
    /// refuse a Claude account themselves, or a second frontend, a replayed IPC
    /// call or a UI regression could still park a subscription where every chat
    /// will route it — and the switch must not disturb the other account kinds.
    #[test]
    fn importing_a_claude_account_needs_the_opt_in() {
        let err =
            account_import_allowed("claude", false).expect_err("no opt-in, no Claude account");
        assert!(
            err.contains(ALLOW_CLAUDE),
            "the refusal has to name the file that turns it on: {err}"
        );
        assert!(account_import_allowed("claude", true).is_ok());
        for kind in ["codex", "opencode-go", "anthropic"] {
            assert!(
                account_import_allowed(kind, false).is_ok(),
                "{kind} must not be affected by the Claude switch"
            );
        }
    }

    /// The refusal lives in the command, not only in the screen that calls it.
    /// This account carries a refresh token, so the probe that normally gates a
    /// Claude import is skipped and the opt-in check is the only thing standing
    /// between it and `settings.accounts`.
    #[tokio::test]
    async fn the_account_command_refuses_claude_without_the_opt_in() {
        let h = Arc::new(harness(vec![]));
        let a: agent::store::Account = serde_json::from_value(json!({
            "kind": "claude", "access_token": "sk-ant-oat-test", "refresh_token": "test-refresh"
        }))
        .unwrap();

        let Err(err) = add_account_with(&h, a, false).await else {
            panic!("an account import must refuse without the opt-in");
        };
        assert!(err.contains(ALLOW_CLAUDE), "{err}");
        assert!(
            h.settings.read().await.accounts.is_empty(),
            "a refused import must not leave an account behind"
        );
    }

    /// An MCP server's bearer token must not ride the settings broadcast to the
    /// webview: `public_settings` masks everything else that is a credential,
    /// and this is the one that had been missed. The UI round-trips whole MCP
    /// rows, so the mask has to survive a write-back rather than read as a
    /// cleared value.
    #[test]
    fn mcp_secrets_are_masked_out_and_restored() {
        let mut s = Settings::default();
        s.providers.insert(
            "anthropic".into(),
            agent::store::ProviderCfg {
                api_key: "sk-live-secret".into(),
                ..Default::default()
            },
        );
        s.plugins.github.token = "ghp_secret".into();
        s.plugins.slack.user_token = "xoxp-user-secret".into();
        s.plugins.gitlab.token = "glpat-secret".into();
        s.mcp.push(McpServerCfg {
            name: "ctx7".into(),
            url: "https://mcp.context7.com".into(),
            transport: "http".into(),
            headers: HashMap::from([(
                "Authorization".to_string(),
                "Bearer tok_secret".to_string(),
            )]),
            env: HashMap::from([("GITHUB_TOKEN".to_string(), "ghp_env_secret".to_string())]),
            ..Default::default()
        });

        let view = public_settings(&s);
        assert_eq!(view.mcp[0].headers["Authorization"], REDACTED);
        assert_eq!(view.mcp[0].env["GITHUB_TOKEN"], REDACTED);
        // The rest of the redaction still holds, and nothing else leaked.
        assert!(view.providers["anthropic"].api_key.is_empty());
        assert!(view.plugins.github.token.is_empty());
        assert!(view.plugins.slack.user_token.is_empty());
        assert!(view.plugins.gitlab.token.is_empty());
        // The stored copy is untouched — masking is a view, not a mutation.
        assert_eq!(s.mcp[0].headers["Authorization"], "Bearer tok_secret");

        // The UI sends the masked row back (toggling `enabled` sends the whole
        // object). The stored secret must survive that untouched.
        let mut next = view.clone();
        next.mcp[0].enabled = false;
        restore_mcp_secrets(&mut next, &s.mcp);
        assert_eq!(next.mcp[0].headers["Authorization"], "Bearer tok_secret");
        assert_eq!(next.mcp[0].env["GITHUB_TOKEN"], "ghp_env_secret");
        assert!(!next.mcp[0].enabled);

        // A real edit still lands: a non-marker value is the user's new secret.
        let mut edited = view;
        edited.mcp[0]
            .headers
            .insert("Authorization".into(), "Bearer rotated".into());
        restore_mcp_secrets(&mut edited, &s.mcp);
        assert_eq!(edited.mcp[0].headers["Authorization"], "Bearer rotated");
    }

    #[test]
    fn mcp_secret_markers_are_bound_to_the_stored_resource() {
        let mut stored = Settings::default();
        stored.mcp.push(McpServerCfg {
            name: "resource".into(),
            url: "https://original.example/mcp".into(),
            transport: "http".into(),
            headers: HashMap::from([("Authorization".into(), "Bearer dummy-secret".into())]),
            env: HashMap::from([("TOKEN".into(), "dummy-env-secret".into())]),
            ..Default::default()
        });
        for change in ["url", "name", "transport", "unknown_key"] {
            let mut next = public_settings(&stored);
            match change {
                "url" => next.mcp[0].url = "https://other.example/mcp".into(),
                "name" => next.mcp[0].name = "other".into(),
                "transport" => next.mcp[0].transport = "sse".into(),
                _ => {
                    next.mcp[0]
                        .headers
                        .insert("Unknown".into(), REDACTED.into());
                    next.mcp[0].env.insert("UNKNOWN".into(), REDACTED.into());
                }
            }
            next.mcp[0]
                .headers
                .insert("Explicit".into(), "new-value".into());
            restore_mcp_secrets(&mut next, &stored.mcp);
            assert_eq!(next.mcp[0].headers["Explicit"], "new-value");
            assert!(
                !next.mcp[0]
                    .headers
                    .values()
                    .chain(next.mcp[0].env.values())
                    .any(|v| v == REDACTED),
                "{change}"
            );
            if change != "unknown_key" {
                assert!(
                    !next.mcp[0].headers.contains_key("Authorization"),
                    "{change}"
                );
                assert!(!next.mcp[0].env.contains_key("TOKEN"), "{change}");
            } else {
                assert_eq!(next.mcp[0].headers["Authorization"], "Bearer dummy-secret");
                assert!(!next.mcp[0].headers.contains_key("Unknown"));
                assert!(!next.mcp[0].env.contains_key("UNKNOWN"));
            }
        }
    }

    #[test]
    fn export_import_round_trip() {
        let t: Task = serde_json::from_value(json!({
            "id": "abc", "title": "hi", "status": "running", "step": "", "project": "/nope", "cwd": "/nope", "branch": "",
            "worktree": true, "model": "openai/gpt-5", "effort": 2, "perm": "auto", "plan": false,
            "items": [{"id": "i1", "kind": "user", "text": "hi", "data": {}, "ts": "2026-09-01T00:00:00Z"}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
            "todos": [], "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let text = export_json(vec![t]).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["format"], EXPORT_FORMAT);
        let back: Vec<Task> = serde_json::from_value(v["tasks"].clone()).unwrap();
        let n = clone_as_new(back[0].clone());
        assert_ne!(n.id, "abc", "imports never overwrite");
        assert_eq!(n.status, "stopped", "a running chat comes back stopped");
        assert!(!n.worktree, "missing folders don't claim a worktree");
        assert_eq!((n.items.len(), n.messages.len()), (1, 1));
    }

    #[test]
    fn swap_task_rewrites_main_subs_and_future() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [{"id": "s1", "role": "explore", "task": "", "status": "running", "meta": "", "model": "gpt-sol"},
                     {"id": "s2", "role": "general", "task": "", "status": "running", "meta": "", "model": "gpt-astra"}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &map, &HashMap::new(), false);
        assert_eq!(t.model, "fable-5");
        assert_eq!(
            (t.subs[0].model.as_str(), t.subs[1].model.as_str()),
            ("fable-5", "gpt-astra"),
            "untouched models stay"
        );
        assert!(
            t.model_map.is_empty(),
            "and nothing a future spawn would read is touched without the opt-in: {:?}",
            t.model_map
        );
    }

    /// Two agents on one model at different levels are separate rows, and a rule
    /// for one must not touch the other.
    #[test]
    fn swap_rules_reach_only_the_matching_level() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "openai/gpt-5", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [{"id": "s1", "role": "general", "task": "", "status": "running", "meta": "", "model": "openai/gpt-5", "effort": 4},
                     {"id": "s2", "role": "general", "task": "", "status": "running", "meta": "", "model": "openai/gpt-5", "effort": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let rules = vec![SwapRule {
            from: "openai/gpt-5".into(),
            from_effort: 4,
            to: "openai/gpt-5".into(),
            to_effort: Some(0),
        }];
        swap_task_rules(&mut t, &[], &rules, false);
        assert_eq!(t.effort, 2, "the main agent is on another level");
        assert_eq!(t.subs[0].effort, Some(0));
        assert_eq!(t.subs[1].effort, Some(1));
    }

    /// The load-bearing one for `future`. A chat swap used to write `model_map`
    /// unconditionally, and `spawn_sub` consults it when launching an agent whose
    /// *type* names a model — so changing this chat's model silently changed what
    /// every future spawn of that type would run on, in a row that never mentioned
    /// subagents. Off has to mean off, and on has to still work.
    #[test]
    fn a_chat_swap_only_moves_future_spawns_when_asked() {
        let mk = || -> Task {
            serde_json::from_value(json!({
                "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
                "model": "gpt-sol", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
                "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
                "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
            }))
            .unwrap()
        };
        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();

        let mut off = mk();
        swap_task(&mut off, &map, &HashMap::new(), false);
        assert_eq!(off.model, "fable-5", "the chat itself moved either way");
        assert!(
            off.model_map.is_empty(),
            "a chat swap alone must not reach what a future spawn of a type would read: {:?}",
            off.model_map
        );

        let mut on = mk();
        swap_task(&mut on, &map, &HashMap::new(), true);
        assert_eq!(
            on.model_map.get("gpt-sol").map(String::as_str),
            Some("fable-5"),
            "with the opt-in the type's future spawns follow"
        );
    }

    /// The swap dialog lists what a chat is *using*, so it has to resolve each
    /// agent the way the agent resolves itself. An agent with no model of its own
    /// is on the chat's, and a swap rewrites the chat — listing the stored (empty)
    /// value offered to replace a model nothing was on while missing the one every
    /// agent really was using.
    #[test]
    fn a_swap_lists_the_models_the_agents_are_actually_on() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [
                {"id": "s1", "role": "explore", "task": "", "status": "running", "meta": "", "depth": 1},
                {"id": "s2", "role": "general", "task": "", "status": "running", "meta": "", "depth": 1, "model": "gpt-astra"},
                {"id": "s3", "role": "explore", "task": "", "status": "done", "meta": "", "depth": 1}
            ],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let mut seen: Vec<(String, String, usize)> = vec![];
        uses_in(&t, &[], &mut |m, label, effort| {
            seen.push((m, label, effort))
        });
        let on = |label: &str| {
            seen.iter()
                .find(|(_, l, _)| l == label)
                .map(|(m, _, e)| (m.as_str(), *e))
        };

        assert_eq!(on("main agent"), Some(("gpt-sol", 2)));
        assert_eq!(
            on("explore subagent (running)").map(|m| m.0),
            Some("gpt-sol"),
            "an agent nobody chose a model for is listed on the chat's"
        );
        assert_eq!(
            on("general subagent (running)").map(|m| m.0),
            Some("gpt-astra"),
            "one with a model of its own is listed on it"
        );
        assert!(
            on("explore subagent (done)").is_none(),
            "settled agents are not in use: {seen:?}"
        );

        // And after a swap the list follows, rather than keeping the old model.
        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &map, &HashMap::new(), false);
        seen.clear();
        uses_in(&t, &[], &mut |m, label, effort| {
            seen.push((m, label, effort))
        });
        let on = |label: &str| {
            seen.iter()
                .find(|(_, l, _)| l == label)
                .map(|(m, _, e)| (m.as_str(), *e))
        };
        assert_eq!(on("main agent").map(|m| m.0), Some("fable-5"));
        assert_eq!(
            on("explore subagent (running)").map(|m| m.0),
            Some("fable-5")
        );
        assert_eq!(
            on("general subagent (running)").map(|m| m.0),
            Some("gpt-astra"),
            "a model of its own is untouched by a swap of the chat's"
        );
    }

    /// A global swap rewrote every subagent type naming a swapped model, silently.
    /// The dialog listed those models in rows that read like live traffic, so moving
    /// one looked like "the model something is running on is changing" while what
    /// actually changed was the default for every agent spawned from here on, in
    /// every chat — live or not. The new-chat default still moves; the types do not
    /// unless the user opted in.
    ///
    /// Driven through `swap_defaults` rather than the command: `H<'_>` is a Tauri's
    /// `State`, built only by the app, so the settings half is unreachable from a
    /// test otherwise — and the gate is the whole point of the change.
    #[test]
    fn a_global_swap_leaves_subagent_types_alone_unless_asked() {
        let mk = || Settings {
            model: "gpt-sol".into(),
            effort: 2,
            agents: vec![AgentDef {
                id: "reviewer".into(),
                name: "Reviewer".into(),
                model: "gpt-sol".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();

        let mut off = mk();
        swap_defaults(&mut off, &map, &HashMap::new(), false);
        assert_eq!(
            off.model, "fable-5",
            "new chats do move: that is the global part"
        );
        assert_eq!(
            off.agents[0].model, "gpt-sol",
            "but the type's default is not touched without the opt-in"
        );

        let mut on = mk();
        swap_defaults(&mut on, &map, &HashMap::new(), true);
        assert_eq!(on.model, "fable-5");
        assert_eq!(
            on.agents[0].model, "fable-5",
            "with the opt-in the type follows"
        );
    }

    /// A second swap must not leave the first one's mapping behind, stale. `A -> B`
    /// followed by `B -> C` used to keep `A -> B`, so a subagent launched from a
    /// default still naming A ran on B — a model the chat had been moved off two
    /// swaps earlier. Only reachable through the `spread` opt-in, the one thing
    /// that writes `model_map` any more.
    #[test]
    fn a_second_swap_carries_the_first_one_along() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let first: HashMap<String, String> =
            [("gpt-sol".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &first, &HashMap::new(), true);
        let second: HashMap<String, String> =
            [("fable-5".to_string(), "gpt-astra".to_string())].into();
        swap_task(&mut t, &second, &HashMap::new(), true);
        assert_eq!(t.model, "gpt-astra");
        assert_eq!(
            t.model_map.get("gpt-sol").map(String::as_str),
            Some("gpt-astra"),
            "an old id follows the chat to where it actually went"
        );
        assert_eq!(
            t.model_map.get("fable-5").map(String::as_str),
            Some("gpt-astra"),
            "and the id the chat is leaving now is recorded too"
        );

        // A swap that moves a destination on must not leave a mapping pointing at
        // a model this chat no longer uses.
        t.model_map.insert("old".into(), "gpt-astra".into());
        let third: HashMap<String, String> =
            [("gpt-astra".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &third, &HashMap::new(), true);
        assert_eq!(
            t.model_map.get("old").map(String::as_str),
            Some("fable-5"),
            "mappings follow the models they name"
        );
        assert!(
            !t.model_map.values().any(|v| v == "gpt-astra"),
            "nothing is left pointing at the model the chat left: {:?}",
            t.model_map
        );
    }

    /// A subagent with no model of its own follows the chat, so a swap reaches the
    /// agents already running and not only the ones spawned afterwards. This is
    /// what the swap dialog has to report, or it offers to replace a model nothing
    /// is on while missing the one every un-chosen agent really is using.
    #[test]
    fn a_swap_reaches_the_subagents_already_running() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [{"id": "s1", "role": "explore", "task": "", "status": "running", "meta": "", "depth": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(
            t.subs[0].model, "",
            "nothing is stored until somebody chooses"
        );
        let sub = t.subs[0].clone();
        assert_eq!(
            agent::resolve_model(&t, Some(&sub), sub.depth, ""),
            "gpt-sol"
        );

        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &map, &HashMap::new(), false);
        let sub = t.subs[0].clone();
        assert_eq!(
            agent::resolve_model(&t, Some(&sub), sub.depth, ""),
            "fable-5",
            "the live subagent follows the chat onto the new model"
        );
    }

    /// Reasoning moves with the model, everywhere the model moves: the chat, a
    /// subagent that had a level of its own, and an X layer that names its own.
    /// A level left on the old model is a level the new one may not have.
    #[test]
    fn a_swap_moves_the_reasoning_with_it() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 1, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [
                {"id": "s1", "role": "explore", "task": "", "status": "running", "meta": "", "model": "gpt-sol", "effort": 1},
                {"id": "s2", "role": "explore", "task": "", "status": "running", "meta": "", "depth": 1},
                {"id": "s3", "role": "general", "task": "", "status": "running", "meta": "", "depth": 1, "model": "gpt-astra", "effort": 0}
            ],
            "ultra_x": {"layers": [{"model": "gpt-sol", "effort": 1, "fanout": 2}], "wt": false},
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();

        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        let effort: HashMap<String, usize> = [("fable-5".to_string(), 4)].into();
        swap_task(&mut t, &map, &effort, false);

        assert_eq!(t.effort, 4, "the chat's level follows its model");
        assert_eq!(
            t.subs[0].effort,
            Some(4),
            "a subagent with a level of its own has it swapped too"
        );
        assert_eq!(
            t.subs[2].effort,
            Some(0),
            "an agent on a model this swap does not touch keeps its level"
        );
        assert_eq!(
            t.ultra_x.as_ref().unwrap().layers[0].effort,
            Some(4),
            "an X layer names its own model, so the swap has to reach its level too"
        );

        // The agent that follows the chat rather than storing anything still has
        // nothing stored: writing a level here would turn an inherited setting into
        // a fixed one, and the next chat-level effort change would stop reaching it.
        let sub = t.subs[1].clone();
        assert_eq!(
            sub.effort, None,
            "no level is invented for a following agent"
        );
        assert_eq!(
            agent::resolve_effort(&t, Some(&sub), None, false),
            4,
            "and it reads the chat's new level"
        );
    }

    /// The route follows the model, exactly as the picker does. A swap moved the
    /// chat off a model but left the fallback chain that model was picked with, so
    /// the resumed chat ran the *old* model's route: it tried a chain whose head
    /// it no longer used, fell back onto models the user had swapped away from,
    /// and exhausted a route that has nothing to do with what they asked for.
    ///
    /// A restart was the only thing that fixed it, because only a restart
    /// recomputes a route from the model on disk.
    #[test]
    fn a_swap_carries_the_route_to_the_new_model() {
        let _g = router::ROUTES_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        router::set_routes(&[
            Route {
                id: "generic".into(),
                name: "Any".into(),
                all: true,
                steps: vec!["openrouter/x".into()],
                ..Default::default()
            },
            Route {
                id: "subs".into(),
                name: "Subs".into(),
                heads: vec!["gpt-sol".into()],
                steps: vec!["gpt-sol".into(), "codex/gpt-6-sol".into()],
                ..Default::default()
            },
        ]);
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        // The route the picker attached when this chat was put on gpt-sol.
        t.route = router::default_route("gpt-sol");

        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &map, &HashMap::new(), false);

        assert_eq!(t.model, "fable-5");
        let want = router::default_route("fable-5");
        assert_eq!(
            t.route, want,
            "the chain is the one the destination model is picked with, not the one \
             the old model's chain the chat was left holding"
        );
        router::set_routes(&[]);
    }

    /// A chat the swap never touched keeps the route the user chose for it. A
    /// blanket "recompute every route" would silently re-point a chat that had
    /// been given a route by hand, which is a decision, not a stale default.
    #[test]
    fn a_swap_leaves_a_route_it_did_not_move_alone() {
        let _g = router::ROUTES_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        router::set_routes(&[Route {
            id: "generic".into(),
            name: "Any".into(),
            all: true,
            steps: vec!["openrouter/x".into()],
            ..Default::default()
        }]);
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-astra", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [], "route": "generic", "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        swap_task(&mut t, &map, &HashMap::new(), false);
        assert_eq!(
            t.route, "generic",
            "a chat this swap does not move keeps its route"
        );
        router::set_routes(&[]);
    }

    /// A level named for a model the swap does not move must not rewrite anything:
    /// it describes a level of the model the chat is still on.
    #[test]
    fn a_swap_ignores_a_level_for_a_model_it_does_not_move() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-astra", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let map: HashMap<String, String> = [("gpt-sol".to_string(), "fable-5".to_string())].into();
        // Keyed by the source model, as the command re-keys it: this one names a
        // model this chat is not on.
        let effort: HashMap<String, usize> = [("gpt-sol".to_string(), 0)].into();
        swap_task(&mut t, &map, &effort, false);
        assert_eq!(t.model, "gpt-astra", "untouched");
        assert_eq!(t.effort, 2, "and so is its reasoning");
    }

    /// Changing only the reasoning of a model you are keeping, with no swap at all.
    ///
    /// This is the edit the dialog used to be unable to make. `effort` arrived
    /// keyed by the row's model and was filtered to rows whose model was *moving*,
    /// so a level picked on the chat's own model was discarded — the row stayed
    /// plain, the counter read "no changes yet", and Apply did nothing. The only
    /// way to change reasoning on a model you were keeping was to swap away from
    /// it and back, which rewrites the route the user had chosen by hand on the way.
    #[test]
    fn a_reasoning_change_alone_reaches_the_agents_on_that_model() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 1, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [
                {"id": "s1", "role": "explore", "task": "", "status": "running", "meta": "", "model": "gpt-sol", "effort": 1},
                {"id": "s2", "role": "explore", "task": "", "status": "running", "meta": "", "depth": 1},
                {"id": "s3", "role": "general", "task": "", "status": "running", "meta": "", "depth": 1, "model": "gpt-astra", "effort": 0}
            ],
            "ultra_x": {"layers": [{"model": "gpt-sol", "effort": 1, "fanout": 2}], "wt": false},
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();

        // No swap at all: the map is empty, exactly what a reasoning-only Apply sends.
        let map: HashMap<String, String> = HashMap::new();
        let effort: HashMap<String, usize> = [("gpt-sol".to_string(), 4)].into();
        swap_task(&mut t, &map, &effort, false);

        assert_eq!(t.model, "gpt-sol", "the model did not move");
        assert_eq!(t.effort, 4, "the chat's reasoning did");
        assert_eq!(
            t.subs[0].effort,
            Some(4),
            "an agent pinned to that model too"
        );
        assert_eq!(
            t.ultra_x.as_ref().unwrap().layers[0].effort,
            Some(4),
            "and the X layer that names it"
        );
        assert_eq!(
            t.subs[2].effort,
            Some(0),
            "an agent on a different model keeps its own level"
        );
        // The agent that follows the chat still stores nothing: the level it sends
        // is the chat's, so writing one here would fix a value that must keep
        // following the chat.
        assert_eq!(t.subs[1].effort, None, "a following agent stores no level");
        assert_eq!(
            agent::resolve_effort(&t, Some(&t.subs[1]), None, false),
            4,
            "and it does pick up the chat's new level"
        );
    }

    /// The dialog lists what agents are actually running at, so the level shown has
    /// to be the one the request carries: a subagent's own override, its type's,
    /// or the chat's — with read-only explorers held to at most Medium.
    #[test]
    fn a_swap_lists_the_reasoning_the_agents_are_actually_at() {
        let t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "gpt-sol", "effort": 0, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [
                {"id": "s1", "role": "explore", "task": "", "status": "running", "meta": "", "depth": 1},
                {"id": "s2", "role": "explore", "task": "", "status": "running", "meta": "", "depth": 1, "effort": 3},
                {"id": "s3", "role": "general", "task": "", "status": "running", "meta": "", "depth": 1}
            ],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let defs = vec![
            agent::store::AgentDef {
                id: "explore".into(),
                tools: "read_only".into(),
                ..Default::default()
            },
            agent::store::AgentDef {
                id: "general".into(),
                tools: "all".into(),
                effort: Some(2),
                ..Default::default()
            },
        ];
        let mut seen: Vec<(String, String, usize)> = vec![];
        uses_in(&t, &defs, &mut |m, label, effort| {
            seen.push((m, label, effort))
        });
        let effort_of = |label: &str| seen.iter().find(|(_, l, _)| l == label).map(|(_, _, e)| *e);

        assert_eq!(effort_of("main agent"), Some(0), "the chat's own level");
        assert_eq!(
            effort_of("explore subagent (running)"),
            Some(3),
            "a read-only explorer nobody configured is held to at most Medium"
        );
        assert_eq!(
            effort_of("general subagent (running)"),
            Some(2),
            "its agent type's level, not the chat's"
        );
    }

    /// An agent with a model of its own keeps it through a swap of the chat's, and
    /// an X layer beats the chat. Precedence, in one place so the two callers that
    /// resolve a model (the run loop and a parked router request) cannot disagree.
    #[test]
    fn model_precedence_is_own_choice_then_layer_then_chat() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "chat-model", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let sub = agent::SubInfo {
            id: "s".into(),
            role: "explore".into(),
            depth: 2,
            ..Default::default()
        };
        assert_eq!(
            agent::resolve_model(&t, Some(&sub), sub.depth, ""),
            "chat-model"
        );

        // The main agent has no layer and so is always the chat's.
        t.ultra_x = Some(store::UltraX {
            layers: vec![
                store::UltraXLayer {
                    model: "layer-1".into(),
                    effort: None,
                    fanout: 0,
                },
                store::UltraXLayer {
                    model: "layer-2".into(),
                    effort: None,
                    fanout: 0,
                },
                store::UltraXLayer {
                    model: "layer-3".into(),
                    effort: None,
                    fanout: 0,
                },
            ],
            max_running: 0,
            max_total: 0,
            wt: false,
        });
        assert_eq!(
            agent::resolve_model(&t, None, 0, ""),
            "chat-model",
            "the main agent is not a layer"
        );
        assert_eq!(
            agent::resolve_model(&t, Some(&sub), sub.depth, ""),
            "layer-2"
        );

        // Its own choice outranks the layer, and survives a swap of the chat.
        let mut own = sub.clone();
        own.model = "picked".into();
        assert_eq!(
            agent::resolve_model(&t, Some(&own), own.depth, ""),
            "picked"
        );
        t.model = "swapped".into();
        assert_eq!(
            agent::resolve_model(&t, Some(&own), own.depth, ""),
            "picked"
        );
        assert_eq!(
            agent::resolve_model(&t, Some(&sub), sub.depth, ""),
            "layer-2"
        );
        assert_eq!(agent::resolve_model(&t, None, 0, ""), "swapped");
    }

    /// A swap made mid-turn waits for the user's next message, and that promise
    /// holds for the main agent — but not for a subagent, which has no turn
    /// boundary to wait for and would otherwise stay behind on a model the user
    /// has already replaced while the rest of the chat moves.
    #[test]
    fn a_queued_swap_moves_the_subagents_not_yet_the_lead() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "a", "title": "", "status": "running", "step": "", "project": "", "cwd": "", "branch": "", "worktree": false,
            "model": "old", "effort": 2, "perm": "auto", "plan": false, "items": [], "messages": [], "todos": [],
            "subs": [], "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        t.pending.insert("model".into(), json!("new"));
        let sub = agent::SubInfo {
            id: "s".into(),
            role: "explore".into(),
            depth: 1,
            ..Default::default()
        };
        assert_eq!(
            agent::resolve_model(&t, Some(&sub), sub.depth, ""),
            "new",
            "a subagent follows the chat onto the queued swap"
        );
        assert_eq!(
            agent::resolve_model(&t, None, 0, ""),
            "old",
            "the main agent waits for the next message, as the badge says"
        );

        // Once it lands, they agree.
        runner::apply_pending(&mut t);
        assert_eq!(agent::resolve_model(&t, Some(&sub), sub.depth, ""), "new");
        assert_eq!(agent::resolve_model(&t, None, 0, ""), "new");
    }

    /// Rewinding has to throw away the whole future, and that includes messages
    /// the user typed while the agent was working. Those are delivered out of the
    /// runtime's queues, never through `messages`, so truncating the history alone
    /// used to leave them queued — a run started after the rewind would then splice
    /// a conversation that no longer exists onto the branch you just rewound to.
    #[tokio::test]
    async fn rewind_clears_messages_that_never_ran() {
        let dir = std::env::temp_dir().join(format!("openleash-rewind-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let t: Task = serde_json::from_value(json!({
            "id": "rw", "title": "", "status": "stopped", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "m0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "m1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"},
                {"id": "m2", "kind": "user", "text": "steer me", "data": {"queued": true}, "ts": "2026-09-01T00:00:02Z"},
                {"id": "m3", "kind": "user", "text": "and after this turn", "data": {"queued": true, "later": true}, "ts": "2026-09-01T00:00:03Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]},
                         {"role": "user", "content": [{"type": "text", "text": "second"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        let id = t.id.clone();
        let h = Arc::new(Harness {
            bus: Arc::new(|_, _| {}),
            tasks: RwLock::new([(id, Arc::new(Mutex::new(t)))].into_iter().collect()),
            runtimes: Default::default(),
            settings: RwLock::new(Settings::default()),
            bg: Default::default(),
            mcp: Default::default(),
            http: reqwest::Client::new(),
            pause_bell: Default::default(),
            accts: Default::default(),
            keys: Default::default(),
            stats: Default::default(),
            dirty: Default::default(),
            saver: Default::default(),
            settings_dirty: Default::default(),
            me: Default::default(),
        });
        let rt = h.runtime("rw");
        // What the user typed mid-run: the agent was stopped, so neither was ever
        // folded into `messages` — they only ever lived in the queue.
        rt.queue.lock().await.push(QueuedMsg {
            item_id: "m2".into(),
            seq: 0,
            blocks: vec![json!({"type": "text", "text": "steer me"})],
        });
        rt.queue.lock().await.push(QueuedMsg {
            item_id: "m3".into(),
            seq: 1,
            blocks: vec![json!({"type": "text", "text": "and after this turn"})],
        });

        let task = h.task("rw").await.unwrap();
        let mut t = task.lock().await;
        let text = rewind_task(&mut t, "m1".into()).expect("rewind works");
        assert_eq!(text, "second", "the message comes back for editing");
        assert_eq!(
            t.items.len(),
            1,
            "later history and the undelivered messages are both gone"
        );
        assert_eq!(t.messages.len(), 1);
        assert_eq!(
            t.checkpoints.len(),
            1,
            "the checkpoint we rewound to is kept, later ones are gone"
        );
        drop(t);
        // The command's other half: with the future gone, the queue goes too.
        rt.queue.lock().await.clear();
        assert!(rt.queue.lock().await.is_empty(), "nothing is left to fire");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A chat with three messages typed while the agent was working, and a
    /// queue behind them. The queue-list commands act on the queue and the
    /// timeline item together, because the row the user clicked *is* the item.
    fn queued_task() -> Task {
        serde_json::from_value(json!({
            "id": "q", "title": "", "status": "running", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "a", "kind": "user", "text": "one", "data": {"queued": true, "seq": 0}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "b", "kind": "user", "text": "two", "data": {"queued": true, "seq": 1}, "ts": "2026-09-01T00:00:01Z"},
                {"id": "c", "kind": "user", "text": "three", "data": {"queued": true, "seq": 2}, "ts": "2026-09-01T00:00:02Z"}
            ],
            "messages": [], "checkpoints": [],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap()
    }

    async fn fill_queue(h: &Harness) {
        let rt = h.runtime("q");
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            rt.queue.lock().await.push(QueuedMsg {
                item_id: (*id).into(),
                seq: i as u64,
                blocks: vec![json!({"type": "text", "text": "x"})],
            });
        }
    }

    #[tokio::test]
    async fn removing_a_queued_message_takes_the_row_with_it() {
        let dir = std::env::temp_dir().join(format!("openleash-qremove-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = harness(vec![queued_task()]);
        fill_queue(&h).await;
        queue_remove(&h, "q", "b").await.unwrap();
        let left: Vec<String> = h
            .runtime("q")
            .queue
            .lock()
            .await
            .iter()
            .map(|m| m.item_id.clone())
            .collect();
        assert_eq!(left, vec!["a", "c"], "the message leaves the queue");
        let t = h.task("q").await.unwrap();
        let rows: Vec<String> = t.lock().await.items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(
            rows,
            vec!["a", "c"],
            "and so does the row the user was looking at"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn editing_a_queued_message_rewrites_the_row_in_place() {
        let dir = std::env::temp_dir().join(format!("openleash-qedit-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = harness(vec![queued_task()]);
        h.runtime("q").queue.lock().await.push(QueuedMsg {
            item_id: "b".into(),
            seq: 1,
            blocks: vec![json!({"type": "text", "text": "two"})],
        });
        queue_edit(&h, "q", "b", "two, but faster").await.unwrap();
        assert_eq!(
            h.runtime("q").queue.lock().await[0].blocks[0]["text"].as_str(),
            Some("two, but faster"),
            "the agent reads the edit, not the original"
        );
        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        let b = t.items.iter().find(|i| i.id == "b").unwrap();
        assert_eq!(
            b.text, "two, but faster",
            "the row shows what the agent will read"
        );
        assert_eq!(
            b.data["seq"].as_u64(),
            Some(1),
            "an edit doesn't move the message"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Reordering renumbers in place, because the list above the composer sorts
    /// on the very same sequence number the agent reads the queue in.
    #[tokio::test]
    async fn moving_a_queued_message_renumbers_so_the_list_follows() {
        let dir = std::env::temp_dir().join(format!("openleash-qmove-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = harness(vec![queued_task()]);
        fill_queue(&h).await;
        let order = async |h: &Harness| {
            h.runtime("q")
                .queue
                .lock()
                .await
                .iter()
                .map(|m| m.item_id.clone())
                .collect::<Vec<_>>()
        };
        queue_move(&h, "q", "c", None).await.unwrap();
        assert_eq!(
            order(&h).await,
            vec!["a", "b", "c"],
            "no anchor means the end, where it already was"
        );
        queue_move(&h, "q", "c", Some("a")).await.unwrap();
        assert_eq!(
            order(&h).await,
            vec!["c", "a", "b"],
            "naming a row puts the message in front of it"
        );
        queue_move(&h, "q", "a", Some("b")).await.unwrap();
        assert_eq!(
            order(&h).await,
            vec!["c", "a", "b"],
            "a row already in front of that one stays put"
        );
        queue_move(&h, "q", "c", Some("b")).await.unwrap();
        assert_eq!(
            order(&h).await,
            vec!["a", "c", "b"],
            "rows land where they were dropped"
        );
        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        let seq = |id: &str| {
            t.items.iter().find(|i| i.id == id).unwrap().data["seq"]
                .as_u64()
                .unwrap()
        };
        assert!(
            seq("a") < seq("c") && seq("c") < seq("b"),
            "the rows sort into the new order: {} {} {}",
            seq("a"),
            seq("c"),
            seq("b")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Stopping must not send anything. This is why the queue is a list the user
    /// can act on: Esc Esc means "enough", and the agent coming straight back
    /// with the message you were told to stop is the bug this fixes.
    #[tokio::test]
    async fn a_stopped_run_leaves_the_queue_alone() {
        let dir = std::env::temp_dir().join(format!("openleash-qstop-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = harness(vec![queued_task()]);
        fill_queue(&h).await;
        // `run_main` bails out here once it has settled on a status, before it
        // would otherwise drain the queue into a fresh run.
        let drained = |status: &str| status != "stopped";
        assert!(
            !drained("stopped"),
            "a stop returns before the queue is touched"
        );
        let waiting: Vec<String> = h
            .runtime("q")
            .queue
            .lock()
            .await
            .iter()
            .map(|m| m.item_id.clone())
            .collect();
        assert_eq!(
            waiting.len(),
            3,
            "every message is still waiting, to be edited or sent by hand"
        );
        assert!(
            h.task("q").await.unwrap().lock().await.messages.is_empty(),
            "and none of them reached the conversation"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// "Steer" on a row is the whole point of the list: one waiting message goes
    /// out at once, and the rest stay where they are. Mid-turn it is delivered as
    /// a reminder the next model call picks up, not dropped and not left waiting.
    #[tokio::test]
    async fn steering_one_row_sends_it_and_leaves_the_rest() {
        let dir = std::env::temp_dir().join(format!("openleash-qsteer-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let inner = harness(vec![queued_task()]);
        let h = Arc::new(inner);
        {
            let rt = h.runtime("q");
            for (i, id) in ["a", "b", "c"].iter().enumerate() {
                let text = ["one", "two", "three"][i];
                rt.queue.lock().await.push(QueuedMsg {
                    item_id: (*id).into(),
                    seq: i as u64,
                    blocks: vec![json!({"type": "text", "text": text})],
                });
            }
        }
        h.runtime("q")
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        queue_send_now(h.clone(), "q", "b").await.unwrap();

        let waiting: Vec<String> = h
            .runtime("q")
            .queue
            .lock()
            .await
            .iter()
            .map(|m| m.item_id.clone())
            .collect();
        assert_eq!(
            waiting,
            vec!["a", "c"],
            "only the steered message leaves the queue"
        );
        let notes = h
            .runtime("q")
            .inbox
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert!(
            notes.concat().contains("two"),
            "the agent is handed the message it was sent"
        );
        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        let b = t.items.iter().find(|i| i.id == "b").unwrap();
        assert!(
            b.data["queued"].is_null(),
            "a sent message stops being queued, so it rejoins the transcript"
        );
        assert!(
            t.items.iter().find(|i| i.id == "a").unwrap().data["queued"] == json!(true),
            "the others are still waiting"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A 1x1 transparent PNG, as a data URL — the smallest thing that is really
    /// an image, so these tests exercise the image path and not a stub.
    const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    /// A running chat, so `send` takes the mid-turn path.
    fn running_task() -> Task {
        serde_json::from_value(json!({
            "id": "q", "title": "", "status": "running", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [], "messages": [], "checkpoints": [],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn as_running(h: &Harness) {
        h.runtime("q")
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// A plain Enter mid-run is steering, not queueing: it reaches the agent at
    /// once and never appears in the list above the composer. Only Alt-Enter waits.
    #[tokio::test]
    async fn a_plain_message_steers_and_only_alt_enter_queues() {
        let dir = std::env::temp_dir().join(format!("openleash-steer-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);

        runner::send_opts(
            h.clone(),
            "q".into(),
            "steer me".into(),
            "steer me".into(),
            false,
        )
        .await
        .unwrap();
        assert!(
            h.runtime("q").queue.lock().await.is_empty(),
            "a plain message is not queued"
        );
        let notes = h
            .runtime("q")
            .inbox
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert!(
            notes.concat().contains("steer me"),
            "it steers: the agent is told right away"
        );

        runner::send_opts(
            h.clone(),
            "q".into(),
            "after this turn".into(),
            "after this turn".into(),
            true,
        )
        .await
        .unwrap();
        let waiting: Vec<String> = h
            .runtime("q")
            .queue
            .lock()
            .await
            .iter()
            .map(|m| m.item_id.clone())
            .collect();
        assert_eq!(waiting.len(), 1, "Alt-Enter is the only thing that waits");
        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        assert!(
            t.items
                .iter()
                .all(|i| i.data["queued"] != json!(true) || i.text == "after this turn"),
            "and only that row is queued"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A queued message is stored in the *runtime*, which is not persisted. The
    /// timeline item is the only durable trace of it, so what that item carries
    /// is all the UI has after a restart — and `enqueue` overwrote `data`
    /// wholesale, dropping anything the message arrived with.
    #[tokio::test]
    async fn enqueue_keeps_the_data_the_message_arrived_with() {
        let dir = std::env::temp_dir().join(format!("openleash-qdata-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);

        // A queued photo: the item carries the images the row renders thumbnails
        // from. Overwriting `data` with {queued, seq} left the row with no images
        // at all — the picture the user attached showed as a row with no picture.
        let mut item = Item::new("user", "look at this", json!({"images": [PIXEL]}));
        let id = item.id.clone();
        runner::enqueue(
            &h,
            "q",
            &mut item,
            vec![json!({"type": "text", "text": "look at this"})],
        )
        .await;

        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        let row = t
            .items
            .iter()
            .find(|i| i.id == id)
            .expect("the row is on the timeline");
        assert_eq!(row.data["queued"], json!(true), "and it is still waiting");
        assert_eq!(
            row.data["seq"].as_u64(),
            Some(0),
            "with its place in the order"
        );
        assert_eq!(
            row.data["images"].as_array().map(Vec::len),
            Some(1),
            "the attached photo survives the queue: {:?}",
            row.data
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The exact bug: a photo sent mid-run used to be dropped, leaving the agent
    /// with the words and no picture. A real image block has to reach the request.
    #[tokio::test]
    async fn a_steered_photo_reaches_the_agent_as_an_image() {
        let dir = std::env::temp_dir().join(format!("openleash-img-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);

        runner::send_attachments(
            h.clone(),
            "q".into(),
            "what is this?".into(),
            vec![PIXEL.into()],
            vec![],
            vec![],
            false,
        )
        .await
        .unwrap();
        let imgs = h
            .runtime("q")
            .inbox_images
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert_eq!(imgs.len(), 1, "the photo waits for the next model call");
        assert_eq!(
            imgs[0].0, "image/png",
            "with its real media type, not a caption"
        );
        assert!(!imgs[0].1.is_empty(), "and the bytes come with it");
        let notes = h
            .runtime("q")
            .inbox
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert!(
            notes.concat().contains("what is this?"),
            "the words steer as well"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file the user attached is copied under `<temp>/OpenLeash/<project>/…`
    /// and the model is told the staged path — but the timeline keeps only what
    /// the user typed, and the staged copies ride on the item's own field for a
    /// later resend. This is the path every attachment send goes through.
    #[tokio::test]
    async fn attachments_are_staged_and_the_model_is_told_the_plain_text() {
        let dir = std::env::temp_dir().join(format!("openleash-stage-{}", std::process::id()));
        let _home = store::test_home(&dir);
        // A real file to attach, in its own temp home.
        let src = dir.join("report.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&src, b"attached bytes").unwrap();
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);

        runner::send_attachments(
            h.clone(),
            "q".into(),
            "here is the file".into(),
            vec![],
            vec![src.to_string_lossy().to_string()],
            vec![],
            false,
        )
        .await
        .unwrap();

        // The staged copy exists under OpenLeash's temp folder, with the bytes.
        let staged = super::agent::attachments::task_dir(".", "q");
        assert!(
            staged.exists(),
            "the temp copy lives under the task's folder"
        );
        let file = std::fs::read_dir(&staged)
            .unwrap()
            .filter_map(|e| e.ok())
            .flat_map(|b| {
                std::fs::read_dir(b.path())
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
            })
            .find(|e| e.file_name().to_string_lossy().contains("report"))
            .expect("the attachment was copied");
        assert_eq!(std::fs::read(file.path()).unwrap(), b"attached bytes");

        // The model is handed the staged path; the transcript keeps the typed text.
        let notes = h
            .runtime("q")
            .inbox
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        let model_text = notes.concat();
        assert!(model_text.contains("here is the file"));
        assert!(
            model_text.contains("<openleash-attachments>")
                && model_text.contains(&file.path().to_string_lossy().to_string()),
            "the model is told the staged path: {model_text}"
        );
        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        let row = t
            .items
            .iter()
            .find(|i| i.kind == "user")
            .expect("the user's message is on the timeline");
        assert_eq!(
            row.text, "here is the file",
            "the visible text has no path note in it"
        );
        let atts = row.data["attachments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(atts.len(), 1, "the staged copy is recorded on the item");
        assert_eq!(atts[0]["name"], "report.txt");

        let _ = super::agent::attachments::remove_task(".", "q");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An SVG in the image list is a file, not a picture. A saved prompt or an
    /// older edited message can still carry one; it must reach the agent as a
    /// staged path and never as an image block, because a provider refuses
    /// `image/svg+xml` and fails the whole request every turn.
    #[tokio::test]
    async fn an_svg_attachment_reaches_the_agent_as_a_file_not_an_image_block() {
        let dir = std::env::temp_dir().join(format!("openleash-svg-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);
        let svg = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=";

        runner::send_attachments(
            h.clone(),
            "q".into(),
            "here is the drawing".into(),
            vec![svg.into()],
            vec![],
            vec!["drawing.svg".into()],
            false,
        )
        .await
        .unwrap();

        // Nothing waits as an image: the picture-only inbox is empty, and the
        // model was told where the file was staged instead.
        let imgs = h
            .runtime("q")
            .inbox_images
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert!(imgs.is_empty(), "no image block for an SVG: {imgs:?}");
        let notes = h
            .runtime("q")
            .inbox
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        let model_text = notes.concat();
        assert!(model_text.contains("here is the drawing"));
        assert!(
            model_text.contains("<openleash-attachments>") && model_text.contains("drawing.svg"),
            "the model is told the staged file: {model_text}"
        );

        let t = h.task("q").await.unwrap();
        let t = t.lock().await;
        let row = t
            .items
            .iter()
            .find(|i| i.kind == "user")
            .expect("the user's message is on the timeline");
        assert_eq!(row.data["images"], json!([]), "no blob to send as an image");
        assert_eq!(row.data["image_attachments"], json!(0));
        let atts = row.data["attachments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(atts.len(), 1, "the staged copy is recorded on the item");
        assert_eq!(atts[0]["name"], "drawing.svg");
        let path = atts[0]["path"].as_str().unwrap();
        assert!(path.ends_with("drawing.svg"), "staged as a file: {path}");
        assert_eq!(
            std::fs::read(path).unwrap(),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            "the file holds the SVG bytes"
        );

        let _ = super::agent::attachments::remove_task(".", "q");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Same for the list: an Alt-Enter photo is queued whole, so sending it later
    /// hands over the image too rather than a message about a missing picture.
    #[tokio::test]
    async fn a_queued_photo_is_still_a_photo_when_it_goes_out() {
        let dir = std::env::temp_dir().join(format!("openleash-qimg-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);

        runner::send_attachments(
            h.clone(),
            "q".into(),
            "look at this".into(),
            vec![PIXEL.into()],
            vec![],
            vec![],
            true,
        )
        .await
        .unwrap();
        let rt = h.runtime("q");
        let id = {
            let q = rt.queue.lock().await;
            assert_eq!(
                q.len(),
                1,
                "an Alt-Enter photo waits like any other message"
            );
            let has_image = q[0]
                .blocks
                .iter()
                .any(|b| b["type"] == "image" && b["source"]["media_type"] == "image/png");
            assert!(
                has_image,
                "the image block is queued, so it can't be forgotten on the way out"
            );
            q[0].item_id.clone()
        };
        // "Send now" on that row: mid-run it steers, and the photo goes with it.
        queue_send_now(h.clone(), "q", &id).await.unwrap();
        let imgs = rt
            .inbox_images
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert_eq!(imgs.len(), 1, "sending it now delivers the photo too");
        assert!(rt.queue.lock().await.is_empty(), "and it leaves the list");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A photo steered with nothing typed alongside it used to be dropped whole:
    /// the turn only got built when there was a text reminder, so a user who
    /// pasted a screenshot and said no more got no image on the request.
    #[test]
    fn a_steered_photo_with_no_caption_still_reaches_the_model() {
        let imgs = vec![("image/png".to_string(), "AAAA".to_string())];
        let blocks = runner::mid_turn_blocks(&[], &[], &imgs);
        assert_eq!(blocks.len(), 1, "a picture on its own is still a message");
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "AAAA");
    }

    /// The words come first, then the photos, in the order the user sent them:
    /// the model needs the question before it can answer it about the picture.
    #[test]
    fn steering_puts_the_words_before_the_picture() {
        let imgs = vec![("image/png".to_string(), "AAAA".to_string())];
        let blocks = runner::mid_turn_blocks(&["what is this?".into()], &[], &imgs);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "what is this?");
        assert_eq!(blocks[1]["type"], "image");
    }

    #[test]
    fn nothing_pending_means_no_turn_at_all() {
        assert!(
            runner::mid_turn_blocks(&[], &[], &[]).is_empty(),
            "no stray empty turn"
        );
    }

    /// Editing a message and pressing Send rewinds first. The rewind has to clear
    /// the pause too: left in place it kept the re-sent message from ever being
    /// answered, and the chat just sat there.
    #[test]
    fn rewinding_a_paused_chat_unfreezes_it() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "e", "title": "", "status": "paused", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "paused": {"kind": "manual", "reason": "Paused by you", "since": "2026-09-01T00:00:00Z"},
            "items": [
                {"id": "i0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "i1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]},
                         {"role": "user", "content": [{"type": "text", "text": "second"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        assert!(t.paused.is_some(), "it was frozen to begin with");
        let text = rewind_task(&mut t, "i1".into()).expect("rewind works");
        assert_eq!(text, "second", "the message comes back for editing");
        assert!(
            t.paused.is_none(),
            "the freeze goes with the future, or the re-sent message is never answered"
        );
        assert!(t.unpaused);
        assert_eq!(
            t.status, "idle",
            "and the chat is ready for the new message"
        );
    }

    /// The same bug the other way round: rewinding an unpaused chat must not
    /// invent a freeze, and must not leave `unpaused` false where it was.
    #[test]
    fn rewinding_a_running_chat_leaves_it_running() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "e", "title": "", "status": "running", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "i0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "i1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]},
                         {"role": "user", "content": [{"type": "text", "text": "second"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        rewind_task(&mut t, "i1".into()).expect("rewind works");
        assert!(t.paused.is_none(), "no freeze invented");
        assert_eq!(t.status, "idle", "and the chat waits for the new message");
    }

    /// The transcript is only one of the channels into the next request. `stop_note`
    /// is written when a run stops and read on the *next* one, so rewinding a
    /// stopped chat and pressing Send used to tell the agent "The user STOPPED
    /// your previous run … check the state and retry anything cut off" about a
    /// turn the rewind had just deleted — which is also how the agent ended up
    /// re-running work the user had explicitly edited out.
    #[test]
    fn rewind_drops_the_stop_note_owed_to_the_discarded_turn() {
        let mut t = stopped_task_with_a_stop_note();
        assert!(
            t.stop_note.as_deref().unwrap_or("").contains("STOPPED"),
            "a stop left a note for the agent to read"
        );
        // "Wrap up" adds its own instruction to that same note.
        t.wrap_up = true;
        rewind_task(&mut t, "i1".into()).expect("rewind works");
        assert!(
            t.stop_note.is_none(),
            "the note describes a turn that no longer exists"
        );
        assert!(
            !t.wrap_up,
            "and so does 'don't continue, summarise' — the work it describes is gone"
        );
    }

    /// `last_context` is the compaction trigger: `run_loop` compacts when it
    /// exceeds 80% of the window, and compaction *clears the checkpoints*. So a
    /// rewind that left the old number in place made the very next iteration
    /// compact a conversation the user had just truncated back to a few
    /// messages — destroying the branch they rewound to and making every
    /// message on it unrewindable.
    #[test]
    fn rewind_resets_the_context_size_that_triggers_compaction() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "e", "title": "", "status": "stopped", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "i0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "i1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]},
                         {"role": "user", "content": [{"type": "text", "text": "second"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 190_000},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        rewind_task(&mut t, "i1".into()).expect("rewind works");
        assert_eq!(
            t.usage.last_context, 0,
            "the branch is now two messages, not a full context window"
        );
        assert_eq!(t.messages.len(), 1);
        assert!(
            !t.checkpoints.is_empty(),
            "and the branch stays rewindable: compaction would have cleared these"
        );
    }

    /// Subagent transcripts continue a `task` tool call. `resume_subs` runs at
    /// the start of every run and splices a saved report back in at
    /// `rposition(assistant) + 1` — an index into messages the rewind just
    /// truncated. Left in place it both re-attaches reports for calls that no
    /// longer exist and shifts every message below that point, moving every
    /// checkpoint with it.
    #[test]
    fn rewind_drops_saved_subagent_transcripts() {
        let mut t: Task = serde_json::from_value(json!({
            "id": "e", "title": "", "status": "stopped", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "i0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "i1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]},
                         {"role": "user", "content": [{"type": "text", "text": "second"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "sub_msgs": {"s1": [{"role": "assistant", "content": [{"type": "text", "text": "half a report"}]}]},
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        rewind_task(&mut t, "i1".into()).expect("rewind works");
        assert!(
            t.sub_msgs.is_empty(),
            "a saved subagent continues a tool call the rewind discarded"
        );
    }

    /// A stopped chat with the note a stop leaves behind, for the rewind tests.
    fn stopped_task_with_a_stop_note() -> Task {
        serde_json::from_value(json!({
            "id": "e", "title": "", "status": "stopped", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "i0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "i1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]},
                         {"role": "user", "content": [{"type": "text", "text": "second"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "stop_note": Some("<system-reminder>The user STOPPED your previous run (this was a stop, not a pause): everything in flight was cancelled. No tool calls were cut off; your last response may have been cut short.</system-reminder>"),
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap()
    }

    /// The other half of the same bug, on the runtime side. A message typed
    /// while the agent was working, and a photo pasted into that message, are
    /// delivered out of `rt.inbox`/`rt.inbox_images` — never through `messages`.
    /// Truncating the transcript cannot reach them, so a rewind used to leave the
    /// deleted message sitting in the inbox to be spliced onto the re-sent turn
    /// as "The user sent this message while you were working".
    #[tokio::test]
    async fn rewind_drops_notes_that_were_never_delivered() {
        let dir =
            std::env::temp_dir().join(format!("openleash-rewind-inbox-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let id = "rw2".to_string();
        let h = Arc::new(Harness {
            bus: Arc::new(|_, _| {}),
            tasks: RwLock::new(
                [(
                    id.clone(),
                    Arc::new(Mutex::new(stopped_task_with_a_stop_note())),
                )]
                .into_iter()
                .collect(),
            ),
            runtimes: Default::default(),
            settings: RwLock::new(Settings::default()),
            bg: Default::default(),
            mcp: Default::default(),
            http: reqwest::Client::new(),
            pause_bell: Default::default(),
            accts: Default::default(),
            keys: Default::default(),
            stats: Default::default(),
            dirty: Default::default(),
            saver: Default::default(),
            settings_dirty: Default::default(),
            me: Default::default(),
        });
        let rt = h.runtime(&id);
        // What typing into a paused-but-running chat leaves behind.
        h.note(
            &id,
            "main",
            "<kind:user-msg>\nthe message being deleted".into(),
        )
        .await;
        h.note_images(&id, "main", vec![("image/png".into(), "AAAA".into())])
            .await;
        rt.queue.lock().await.push(QueuedMsg {
            item_id: "i1".into(),
            seq: 0,
            blocks: vec![json!({"type": "text", "text": "typed mid-run"})],
        });
        // A chat that was frozen when the user rewound it: the router holds the
        // request it froze on, built before the rewind existed.
        rt.steered.store(true, std::sync::atomic::Ordering::SeqCst);

        rt.drop_undelivered().await;

        assert!(
            rt.inbox.lock().await.is_empty(),
            "the deleted message must not be told to the model on the next request"
        );
        assert!(
            rt.inbox_images.lock().await.is_empty(),
            "and neither must a photo pasted into it"
        );
        assert!(rt.queue.lock().await.is_empty(), "nothing left to fire");
        assert!(
            !rt.steered.load(std::sync::atomic::Ordering::SeqCst),
            "a frozen chat must rebuild from the rewound history, not replay the request it froze on"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A full fork keeps the whole transcript and only *adds* the origin notice on
    /// top, so the checkpoints — which are indices into `items` — have to move down
    /// with it. Forget that and rewinding in the fork lands one row early, on the
    /// notice, and reports the wrong message.
    #[test]
    fn full_fork_notice_shifts_every_checkpoint_down_one() {
        let t: Task = serde_json::from_value(json!({
            "id": "src", "title": "Original", "status": "stopped", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false, "todos": [], "subs": [],
            "items": [
                {"id": "i0", "kind": "user", "text": "first", "data": {}, "ts": "2026-09-01T00:00:00Z"},
                {"id": "i1", "kind": "user", "text": "second", "data": {}, "ts": "2026-09-01T00:00:01Z"}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "first"}]}],
            "checkpoints": [{"item_index": 0, "msg_index": 0}, {"item_index": 1, "msg_index": 1}],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();

        // The shape `task_fork` builds for `mode: "full"`.
        let mut f = clone_as_new(t.clone());
        f.forked_from = Some(t.id.clone());
        f.items.insert(0, Item::new("notice", "Forked from Original", json!({"level": "fork", "from": t.id, "mode": "full", "title": "Original", "summary": ""})));
        for c in f.checkpoints.iter_mut() {
            c.item_index += 1;
        }

        assert_eq!(
            f.items[0].data["level"], "fork",
            "the origin notice is the first row"
        );
        assert_eq!(
            f.items.len(),
            3,
            "nothing was dropped: the whole transcript came across"
        );
        assert_eq!(
            f.items[1].id, "i0",
            "the copy's first message is still the original first message"
        );
        // Every checkpoint still points at the same user turn it did before the fork.
        for c in &f.checkpoints {
            let text = f.items[c.item_index].text.clone();
            assert!(
                text == "first" || text == "second",
                "checkpoint {c:?} landed on a real message, not the notice"
            );
        }
        assert_ne!(f.id, t.id, "a fork is its own chat");
        assert_eq!(t.items.len(), 2, "the chat that was forked is untouched");
        assert_eq!(t.checkpoints[1].item_index, 1, "including its checkpoints");
    }

    #[test]
    fn trust_path_containment_is_component_aware_and_cross_platform() {
        assert!(path_is_within("/work", "/work"), "exact path matches");
        assert!(path_is_within("/work", "/work/a"), "a child path matches");
        assert!(
            !path_is_within("/work", "/work-old/a"),
            "a sibling with a shared string prefix is not a child"
        );
        assert!(
            path_is_within(r"C:\Work\Parent", "c:/work/parent/child"),
            "Windows separators and case compare consistently even on non-Windows hosts"
        );
        assert!(
            !path_is_within(r"C:\Work", "C:/workspace/project"),
            "the component boundary also applies to Windows paths"
        );
        assert!(
            path_is_within("//Server/Share", "\\\\server\\share\\repo"),
            "UNC paths compare case-insensitively with either separator style"
        );
    }

    fn trust_decision(
        path: &str,
        kind: agent::trust::TrustKind,
        decision: agent::trust::TrustState,
    ) -> agent::trust::TrustDecision {
        agent::trust::TrustDecision {
            path: path.into(),
            kind,
            decision,
            decided_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn generic_settings_trust_patch_invalidates_parent_descendants() {
        // This is the trust diff `settings_update` computes after deserializing a
        // generic settings patch; use a live harness to prove it clears each
        // affected frozen prefix, and not a project that only shares a string prefix.
        let mut tasks = Vec::new();
        for (id, project) in [
            ("a", "/work/a"),
            ("b", "/work/b"),
            ("root", "/work"),
            ("sibling", "/work-old/repo"),
        ] {
            let mut task = chat(id);
            task.project = project.into();
            task.system = format!("old-prefix-{id}");
            task.mcp_tools = vec![json!({"old": id})];
            task.plugins.computer.enabled = true;
            tasks.push(task);
        }
        let h = harness(tasks);
        let settings = Settings {
            trust: vec![trust_decision(
                "/work",
                agent::trust::TrustKind::Parent,
                agent::trust::TrustState::Trusted,
            )],
            ..Default::default()
        };
        let patched = vec![trust_decision(
            "/work",
            agent::trust::TrustKind::Parent,
            agent::trust::TrustState::Untrusted,
        )];
        // Apply the `trust` key just as settings_update does before rebuilding
        // Settings. It is not on that command's protected-key denylist.
        let mut current = serde_json::to_value(&settings).unwrap();
        let patch = json!({"trust": serde_json::to_value(&patched).unwrap()});
        if let (Some(obj), Some(patch)) = (current.as_object_mut(), patch.as_object()) {
            for (key, value) in patch {
                obj.insert(key.clone(), value.clone());
            }
        }
        let next: Settings = serde_json::from_value(current).unwrap();
        let prefixes = changed_trust_prefixes(&settings.trust, &next.trust);
        assert_eq!(prefixes.len(), 1);
        assert!(prefixes[0].descendants);
        invalidate_trust_prefixes(&h, &prefixes).await;

        let tasks: Vec<_> = h.tasks.read().await.values().cloned().collect();
        let mut remaining = HashMap::new();
        for task in tasks {
            let task = task.lock().await;
            remaining.insert(task.id.clone(), task.system.clone());
            if task.id != "sibling" {
                assert!(
                    task.system.is_empty(),
                    "{} was under the changed parent",
                    task.id
                );
                assert!(
                    task.mcp_tools.is_empty(),
                    "{} kept stale MCP tools",
                    task.id
                );
                assert!(
                    !task.plugins.computer.enabled,
                    "{} kept the old plugin snapshot",
                    task.id
                );
            }
        }
        assert_eq!(
            remaining["sibling"], "old-prefix-sibling",
            "the shared string prefix alone does not invalidate a sibling"
        );
        let sibling = h.task("sibling").await.unwrap();
        assert!(sibling.lock().await.plugins.computer.enabled);
    }

    #[tokio::test]
    async fn a_doc_selection_change_invalidates_only_that_project() {
        // Switching a file off changes what the prompt says, so a frozen prefix
        // built under the old selection has to go — otherwise a chat opened
        // before the toggle keeps injecting the file for the life of its prefix.
        // The rule governs one exact folder, so a nested project is untouched.
        let mut exact = chat("docs-exact");
        exact.project = "/work/a".into();
        exact.system = "stale docs prefix".into();
        let mut child = chat("docs-child");
        child.project = "/work/a/child".into();
        child.system = "keep child prefix".into();
        let h = harness(vec![exact, child]);

        let rule = |path: &str, disabled: &[&str]| store::ProjectDocRule {
            path: path.into(),
            disabled: disabled.iter().map(|s| s.to_string()).collect(),
        };
        let prefixes = changed_doc_prefixes(&[], &[rule("/work/a", &["CLAUDE.md"])]);
        assert_eq!(prefixes.len(), 1);
        assert!(
            !prefixes[0].descendants,
            "a doc rule never reaches child folders"
        );
        invalidate_trust_prefixes(&h, &prefixes).await;

        let tasks: Vec<_> = h.tasks.read().await.values().cloned().collect();
        for task in tasks {
            let task = task.lock().await;
            if task.id == "docs-exact" {
                assert!(task.system.is_empty(), "the stale prefix is dropped");
            } else {
                assert_eq!(
                    task.system, "keep child prefix",
                    "a nested project is not governed by the parent's rule"
                );
            }
        }

        // A no-op write (same rule again) must not invalidate anything: every
        // unrelated settings save would otherwise drop every warm prefix.
        assert!(changed_doc_prefixes(
            &[rule("/work/a", &["CLAUDE.md"])],
            &[rule("/work/a", &["CLAUDE.md"])]
        )
        .is_empty());
        // Reordering the disabled list is not a change either.
        assert!(changed_doc_prefixes(
            &[rule("/work/a", &["CLAUDE.md", "AGENTS.md"])],
            &[rule("/work/a", &["AGENTS.md", "CLAUDE.md"])]
        )
        .is_empty());
        // Switching a file back on is.
        assert_eq!(
            changed_doc_prefixes(
                &[rule("/work/a", &["CLAUDE.md"])],
                &[rule("/work/a", &["CLAUDE.md", "AGENTS.md"])]
            )
            .len(),
            1
        );
    }

    #[tokio::test]
    async fn folder_trust_change_invalidates_only_exact_project() {
        let mut exact = chat("exact");
        exact.project = "/work/a".into();
        exact.system = "stale exact prefix".into();
        let mut child = chat("child");
        child.project = "/work/a/child".into();
        child.system = "keep child prefix".into();
        let h = harness(vec![exact, child]);
        let prefixes = changed_trust_prefixes(
            &[],
            &[trust_decision(
                "/work/a",
                agent::trust::TrustKind::Folder,
                agent::trust::TrustState::Untrusted,
            )],
        );
        assert_eq!(prefixes.len(), 1);
        assert!(!prefixes[0].descendants);
        invalidate_trust_prefixes(&h, &prefixes).await;

        let tasks: Vec<_> = h.tasks.read().await.values().cloned().collect();
        for task in tasks {
            let task = task.lock().await;
            if task.id == "exact" {
                assert!(task.system.is_empty());
            } else {
                assert_eq!(task.system, "keep child prefix");
            }
        }
    }

    /// A harness holding `tasks` in memory, with UI events dropped.
    /// The Review button never launches a browser the user has turned off, and
    /// never for a chat that doesn't exist.
    #[tokio::test]
    async fn visual_recapture_rejects_unknown_tasks_and_disabled_plugin_before_launch() {
        let h = harness(vec![chat("visual-test")]);
        assert!(visual_recapture_for(&h, "missing").await.is_err());
        let disabled = visual_recapture_for(&h, "visual-test").await;
        assert!(disabled.err().unwrap().contains("turned off"));
    }

    #[tokio::test]
    async fn browser_panel_rejects_unknown_tasks_and_disabled_plugin_before_launch() {
        let h = harness(vec![chat("browser-test")]);
        // The plugin is on by default now; the disabled half has to say so.
        h.settings.write().await.plugins.browser.enabled = false;
        let unknown = browser_panel_for(
            &h,
            "missing",
            agent::browser_panel::PanelAction::Snapshot {},
        )
        .await;
        assert!(unknown.is_err());
        let disabled = browser_panel_for(
            &h,
            "browser-test",
            agent::browser_panel::PanelAction::Snapshot {},
        )
        .await;
        assert!(disabled.err().unwrap().contains("turned off"));
        // A malformed action is refused before the plugin or a browser is consulted.
        let invalid = browser_panel_for(
            &h,
            "browser-test",
            agent::browser_panel::PanelAction::Navigate {
                url: "file:///etc/passwd".into(),
            },
        )
        .await;
        assert!(invalid.err().unwrap().contains("HTTP"));
    }

    #[tokio::test]
    async fn browser_annotations_are_refused_before_launch_and_never_delivered() {
        let h = Arc::new(harness(vec![chat("annotate-test")]));
        let note = || agent::browser_annotate::Annotation {
            comment: "Make the heading larger".into(),
            bbox: agent::browser::Rect {
                x: 10.0,
                y: 10.0,
                width: 100.0,
                height: 20.0,
            },
            element: None,
            styles: vec![],
        };
        let unknown = deliver_browser_annotations(&h, "missing", vec![note()]).await;
        assert!(unknown.is_err());
        // Validation runs before the plugin check, so these fail on their own merits.
        let empty = deliver_browser_annotations(&h, "annotate-test", vec![]).await;
        assert!(empty.err().unwrap().contains("no annotations"));
        let blank = deliver_browser_annotations(
            &h,
            "annotate-test",
            vec![agent::browser_annotate::Annotation {
                comment: "  ".into(),
                ..note()
            }],
        )
        .await;
        assert!(blank.err().unwrap().contains("comment"));
        let too_many = deliver_browser_annotations(&h, "annotate-test", vec![note(); 11]).await;
        assert!(too_many.err().unwrap().contains("at most 10"));
        h.settings.write().await.plugins.browser.enabled = false;
        let disabled = deliver_browser_annotations(&h, "annotate-test", vec![note()]).await;
        assert!(disabled.err().unwrap().contains("turned off"));
        // Nothing reached the chat on any refused path.
        let task = h.task("annotate-test").await.unwrap();
        let task = task.lock().await;
        assert!(task.items.iter().all(|i| i.kind != "user"));
        assert!(task.messages.is_empty());
    }

    fn harness(tasks: Vec<Task>) -> Harness {
        harness_with_bus(tasks, Arc::new(|_, _| {}))
    }

    /// A harness whose UI events go to `bus`, for the tests that assert what the
    /// user is told and *when* they were told it.
    fn harness_with_bus(tasks: Vec<Task>, bus: agent::Bus) -> Harness {
        Harness {
            bus,
            tasks: RwLock::new(
                tasks
                    .into_iter()
                    .map(|t| (t.id.clone(), Arc::new(Mutex::new(t))))
                    .collect(),
            ),
            runtimes: Default::default(),
            settings: RwLock::new(Settings::default()),
            bg: Default::default(),
            mcp: Default::default(),
            http: reqwest::Client::new(),
            pause_bell: Default::default(),
            accts: Default::default(),
            stats: Default::default(),
            keys: Default::default(),
            dirty: Default::default(),
            saver: Default::default(),
            settings_dirty: Default::default(),
            me: Default::default(),
        }
    }

    fn chat(id: &str) -> Task {
        let mut t: Task = serde_json::from_value(json!({
            "id": id, "title": id, "status": "stopped", "step": "", "project": ".", "cwd": ".", "branch": "",
            "worktree": false, "model": "m", "effort": 2, "perm": "auto", "plan": false,
            "items": [], "messages": [], "todos": [], "subs": [], "touched": {},
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        // `hydrated` is `#[serde(skip)]`, so deserialising a literal leaves it
        // false -- and `save_task` refuses to write an unhydrated stub rather than
        // overwrite real subagent history with two empty maps. These fixtures are
        // built whole, so they are hydrated by construction; without this every
        // `save_task` in this module is silently a no-op.
        t.hydrated = true;
        t
    }

    async fn archived_ids(h: &Harness) -> Vec<String> {
        let refs: Vec<_> = h.tasks.read().await.values().cloned().collect();
        let mut ids = vec![];
        for r in refs {
            if r.lock().await.archived {
                ids.push(r.lock().await.id.clone());
            }
        }
        ids.sort();
        ids
    }

    /// Renaming a chat by hand locks the name against the agent, and the one
    /// way back is the sidebar's "let the agent name it". Both halves matter:
    /// without the lock the agent quietly undoes the user's word on the next
    /// turn, and without the unlock a typo in a title is permanent.
    #[tokio::test]
    async fn renaming_by_hand_locks_the_name_until_it_is_handed_back() {
        let dir = std::env::temp_dir().join(format!("openleash-titled-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![chat("t")]));
        {
            let t = h.task("t").await.unwrap();
            let t = t.lock().await;
            assert!(!t.titled, "a fresh chat is the agent's to name");
        }

        apply_patch(&h, "t", json!({"title": "  My name for it  "}))
            .await
            .expect("the rename patch applies");
        let t = h.task("t").await.unwrap();
        let t = t.lock().await;
        assert_eq!(t.title, "My name for it", "the patch is trimmed as before");
        assert!(t.titled, "but now the user has claimed the name");
        drop(t);

        // The agent is still refused while it is locked.
        assert!(runner::rename_check(&h, "t", "something else")
            .await
            .is_err());

        apply_patch(&h, "t", json!({"untitled": true}))
            .await
            .expect("the untitle patch applies");
        let t = h.task("t").await.unwrap();
        assert!(!t.lock().await.titled, "handing it back releases the lock");
        drop(t);
        assert!(
            runner::rename_check(&h, "t", "something else")
                .await
                .is_ok(),
            "and the agent can name it again"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn delete_archived_leaves_active_chats_alone() {
        let dir =
            std::env::temp_dir().join(format!("openleash-del-archived-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let mut a = chat("a");
        let mut b = chat("b");
        a.archived = true;
        b.archived = true;
        let preserved_worktree = dir.join("worktrees").join("b");
        std::fs::create_dir_all(&preserved_worktree).unwrap();
        std::fs::write(preserved_worktree.join("uncommitted.txt"), "keep me\n").unwrap();
        b.worktree = true;
        b.cwd = preserved_worktree.to_string_lossy().to_string();
        b.project = dir.join("project").to_string_lossy().to_string();
        let live = chat("live");
        for t in [&a, &b, &live] {
            store::save_task(t);
        }
        store::save_draft("b", "unsent words");

        let h = harness(vec![a, b, live]);
        assert_eq!(
            archived_ids(&h).await,
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            tokio::task::spawn_blocking(move || delete_archived(&h))
                .await
                .unwrap()
                .unwrap(),
            2,
            "the archived chats are deleted and their worktrees preserved"
        );
        assert_eq!(
            store::load_tasks()
                .into_iter()
                .map(|t| t.id)
                .collect::<Vec<_>>(),
            vec!["live".to_string()],
            "active chats stay"
        );
        assert!(
            !dir.join("tasks").join("b.json").exists(),
            "an archived chat is gone from disk"
        );
        assert!(
            !dir.join("drafts").join("b.txt").exists(),
            "and so is its draft"
        );
        assert!(dir.join("tasks").join("live.json").exists());
        assert_eq!(
            std::fs::read_to_string(preserved_worktree.join("uncommitted.txt")).unwrap(),
            "keep me\n",
            "bulk transcript deletion does not discard the archived chat's worktree"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A race record over the given attempt chats, for the delete tests.
    fn race_stub(id: &str, chats: &[&str], archived: bool) -> agent::race::Race {
        agent::race::Race {
            id: id.into(),
            prompt: "fix the bug".into(),
            project: ".".into(),
            base_branch: "main".into(),
            created_at: chrono::Utc::now(),
            attempts: chats
                .iter()
                .map(|t| agent::race::RaceAttempt {
                    task_id: (*t).into(),
                    model: "m".into(),
                    prompt: "fix the bug".into(),
                    effort: 2,
                    checks: vec![],
                    checked: false,
                })
                .collect(),
            judge_model: "m".into(),
            judge_effort: 2,
            state: "judged".into(),
            judge: None,
            winner: None,
            merged: None,
            error: None,
            archived,
            archived_chats: vec![],
        }
    }

    /// An archived race can be deleted for good, and the chats still archived
    /// with it go with it. A race that is not archived is refused: it may have
    /// attempts that are running right now.
    #[tokio::test]
    async fn deleting_an_archived_race_takes_its_archived_chats_with_it() {
        let dir =
            std::env::temp_dir().join(format!("openleash-del-race-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let mut a = chat("a");
        let mut b = chat("b");
        a.archived = true;
        b.archived = true;
        let live = chat("live");
        for t in [&a, &b, &live] {
            store::save_task(t);
        }
        agent::race::upsert(&race_stub("r1", &["a", "b"], true)).unwrap();
        agent::race::upsert(&race_stub("r2", &["live"], false)).unwrap();

        let h = Arc::new(harness(vec![a, b, live]));
        // The race that is still in the sidebar is refused: its attempts may be
        // running, and dropping them would stop work the user never asked to stop.
        let h2 = h.clone();
        let err = tokio::task::spawn_blocking(move || delete_race(&h2, "r2", false))
            .await
            .unwrap()
            .unwrap_err();
        assert!(err.contains("Archive the race"), "{err}");
        assert!(
            agent::race::get("r2").is_ok(),
            "the refused race is still there"
        );
        assert!(h.tasks.read().await.contains_key("live"));

        let h2 = h.clone();
        let out = tokio::task::spawn_blocking(move || delete_race(&h2, "r1", false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out["deleted_chats"], json!(["a", "b"]));
        assert_eq!(out["kept_chats"], json!([]));
        assert!(agent::race::get("r1").is_err(), "the race record is gone");
        assert!(!h.tasks.read().await.contains_key("a"));
        assert!(!h.tasks.read().await.contains_key("b"));
        assert!(!dir.join("tasks").join("a.json").exists());
        assert!(!dir.join("tasks").join("b.json").exists());
        // The chat that was never part of the race stays.
        assert!(h.tasks.read().await.contains_key("live"));
        assert!(dir.join("tasks").join("live.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A chat the user restored on its own is a chat of its own again, so
    /// deleting the race leaves it alone and says so.
    #[tokio::test]
    async fn deleting_a_race_keeps_a_chat_the_user_restored_on_its_own() {
        let dir =
            std::env::temp_dir().join(format!("openleash-del-race-kept-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let mut a = chat("a");
        a.archived = true;
        let mut b = chat("b");
        // "b" was restored by hand: it is out of the archive.
        b.archived = false;
        for t in [&a, &b] {
            store::save_task(t);
        }
        agent::race::upsert(&race_stub("r1", &["a", "b"], true)).unwrap();

        let h = Arc::new(harness(vec![a, b]));
        let h2 = h.clone();
        let out = tokio::task::spawn_blocking(move || delete_race(&h2, "r1", false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out["deleted_chats"], json!(["a"]));
        assert_eq!(out["kept_chats"], json!(["b"]));
        assert!(!h.tasks.read().await.contains_key("a"));
        assert!(
            h.tasks.read().await.contains_key("b"),
            "the restored chat is not the race's to delete"
        );
        assert!(agent::race::get("r1").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An ultrathread worker's own checkout belongs to its chat: it must not be
    /// listed as a leftover worth deleting while that chat exists, and deleting
    /// the chat with its files must take the worker checkouts with it. Before
    /// this, a worktree-per-worker run put every worker checkout under the
    /// manager's "Left behind" heading (its chat was, in fact, alive), offered a
    /// Delete button for a folder an agent was working in, and leaked every one
    /// of them when the chat was deleted.
    #[tokio::test]
    async fn worker_worktrees_are_their_chats_and_go_when_it_does() {
        let dir = std::env::temp_dir().join(format!("openleash-worker-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _home = store::test_home(&dir);
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cwd = proj.to_string_lossy().to_string();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "t@example.invalid"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(&args)
                .output()
                .expect("git should be installed");
            assert!(out.status.success(), "git {args:?} failed");
        }
        std::fs::write(proj.join("base.txt"), "base\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "init"]] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        // The same helper the spawn path uses, so the checkout really is one.
        let (wt_cwd, wt_branch) =
            git::create_worktree(&cwd, "HEAD", "ut-worker").expect("a worker worktree");
        std::fs::write(std::path::Path::new(&wt_cwd).join("work.txt"), "worker\n").unwrap();

        let mut t = chat("worker-chat");
        t.project = cwd.clone();
        t.cwd = cwd.clone();
        t.subs.push(agent::SubInfo {
            id: "s1".into(),
            role: "general".into(),
            task: "a worker".into(),
            status: "done".into(),
            cwd: wt_cwd.clone(),
            branch: wt_branch.clone(),
            ..Default::default()
        });

        // The path is the chat's own, so it is neither a leftover nor prunable.
        let owned = owned_paths(&t);
        assert!(
            owned.contains(&agent::trust::norm_path(&wt_cwd)),
            "a worker's checkout is one of its chat's paths"
        );
        assert_eq!(
            worker_worktrees(&t).len(),
            1,
            "and is listed once, under that chat"
        );

        let h = harness(vec![t]);
        // `purge_task` blocks (it takes the task lock and runs git), so it runs
        // on the blocking pool here exactly as the delete command runs it.
        let (hp, wcwd) = (h, wt_cwd.clone());
        tokio::task::spawn_blocking(move || purge_task(&hp, "worker-chat", true))
            .await
            .unwrap()
            .expect("the chat deletes with its worker worktrees");
        assert!(
            !std::path::Path::new(&wcwd).is_dir(),
            "deleting the chat with its files removes the worker's checkout"
        );
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["rev-parse", "--verify", "--quiet", &wt_branch])
                .output()
                .unwrap()
                .status
                .success(),
            "the branch itself is kept for recovery"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Archiving a race moves its attempt chats, and a restore brings back only
    /// those: a chat the user had archived on its own must stay put.
    #[tokio::test]
    async fn archiving_a_race_restores_only_the_chats_it_moved() {
        let dir =
            std::env::temp_dir().join(format!("openleash-race-archive-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let mut own = chat("own");
        own.archived = true;
        let h = harness(vec![chat("a"), chat("b"), own]);
        let attempt = |t: &str| agent::race::RaceAttempt {
            task_id: t.into(),
            model: "m".into(),
            prompt: "p".into(),
            effort: 2,
            checks: vec![],
            checked: false,
        };
        let race = agent::race::Race {
            id: "rA".into(),
            prompt: "p".into(),
            project: ".".into(),
            base_branch: String::new(),
            created_at: chrono::Utc::now(),
            attempts: vec![
                attempt("a"),
                attempt("b"),
                attempt("own"),
                attempt("deleted"),
            ],
            judge_model: "m".into(),
            judge_effort: 2,
            state: "judged".into(),
            judge: None,
            winner: None,
            merged: None,
            error: None,
            archived: false,
            archived_chats: vec![],
        };
        agent::race::upsert(&race).unwrap();

        let r = archive_race(&h, "rA", true).await.unwrap();
        assert!(r.archived);
        assert_eq!(
            r.archived_chats,
            vec!["a".to_string(), "b".to_string()],
            "a deleted chat and an already-archived one are not claimed"
        );
        let mut now = archived_ids(&h).await;
        now.sort();
        assert_eq!(
            now,
            vec!["a".to_string(), "b".to_string(), "own".to_string()]
        );
        assert!(
            archive_race(&h, "rA", true).await.unwrap().archived,
            "archiving twice is a no-op"
        );

        let r = archive_race(&h, "rA", false).await.unwrap();
        assert!(!r.archived && r.archived_chats.is_empty());
        assert_eq!(
            archived_ids(&h).await,
            vec!["own".to_string()],
            "the chat archived on its own stays archived"
        );
        assert!(archive_race(&h, "nope", true).await.is_err());
        agent::race::remove("rA").unwrap();
    }

    /// A bulk archive is the one command that hides a screenful of chats at
    /// once, so the two ways it can go wrong are checked here: reaching a chat
    /// nobody ticked, and reporting a chat it did nothing to as work done. Both
    /// are the shape of a lost transcript, which is why a stale tick in the
    /// sidebar (a chat restored elsewhere, then archived again from the same
    /// selection) cannot quietly do this.
    #[tokio::test]
    async fn archiving_several_chats_leaves_the_rest_and_only_counts_real_work() {
        // `test_home` is process-wide, so this test must not write task files at
        // all: a neighbour reading `load_tasks` would see them. Everything
        // checked here is in memory, which is where a bulk archive's decisions
        // live — the assertions are about which chats it was asked to touch.
        let dir =
            std::env::temp_dir().join(format!("openleash-bulk-archive-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let mut pinned = chat("pinned");
        pinned.pinned = true;
        let mut paused = chat("paused");
        paused.status = "running".into();
        paused.step = "Paused for review".into();
        paused.paused = Some(agent::Pause {
            kind: "manual".into(),
            reason: "review".into(),
            ..Default::default()
        });
        let mut already = chat("already");
        already.archived = true;
        let mut alive = chat("alive");
        alive.archived = true;
        let h = harness(vec![pinned, paused, already, alive]);

        assert_eq!(
            archive_chats(
                &h,
                &[
                    "pinned".into(),
                    "paused".into(),
                    "already".into(),
                    "ghost".into()
                ]
            )
            .await
            .unwrap(),
            2,
            "only the pinned and paused chats were live to archive; a vanished id is not work done"
        );
        assert_eq!(
            archived_ids(&h).await,
            vec![
                "alive".to_string(),
                "already".to_string(),
                "paused".to_string(),
                "pinned".to_string()
            ]
        );
        {
            let paused = h.task("paused").await.unwrap();
            let paused = paused.lock().await;
            assert_eq!(paused.status, "stopped", "archiving a pause stops it");
            assert!(
                paused.paused.is_none(),
                "the archived chat no longer counts as paused"
            );
        }
        assert!(
            !h.task("pinned").await.unwrap().lock().await.pinned,
            "archiving unpins, same as the single-chat patch"
        );
        assert_eq!(
            archive_chats(&h, &["pinned".into()]).await.unwrap(),
            0,
            "a second pass over the same chat archives nothing and counts nothing"
        );
        assert_eq!(
            archived_ids(&h).await,
            vec![
                "alive".to_string(),
                "already".to_string(),
                "paused".to_string(),
                "pinned".to_string()
            ],
            "and it left the rest alone"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// One file, and only the chats that were asked for. `tasks_export_all` is
    /// the wrong set the moment the user has ticked three of them, and quietly
    /// including the other two hundred is the kind of thing that only surfaces
    /// when someone opens the file.
    #[tokio::test]
    async fn exporting_several_chats_writes_just_those() {
        // No `test_home`: the export path is an explicit argument, so the file
        // goes somewhere this test owns. Taking the shared home would write
        // task JSON into a directory other tests load from.
        let dir =
            std::env::temp_dir().join(format!("openleash-bulk-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let h = harness(vec![chat("a"), chat("b"), chat("c")]);
        let path = dir.join("out.json");

        assert_eq!(
            export_chats(
                &h,
                &["b".into(), "c".into(), "ghost".into()],
                &path.to_string_lossy()
            )
            .await
            .unwrap(),
            2
        );
        let j: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut ids: Vec<String> = j["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["b".to_string(), "c".to_string()],
            "and not chat a"
        );
        assert!(
            export_chats(&h, &["ghost".into()], &path.to_string_lossy())
                .await
                .is_err(),
            "nothing to write is an error, not an empty file"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// The training export keeps the conversation and nothing else: the visible
    /// text of each turn, the system prompt where the target wants it, and no
    /// tool traffic or hidden reasoning.
    #[test]
    fn training_export_keeps_text_and_drops_tool_traffic() {
        let mut t = chat("t");
        t.system = "PROJECT RULES".into();
        t.messages = vec![
            Message::user_text("do the thing"),
            serde_json::from_value(json!({
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "secret reasoning"},
                    {"type": "text", "text": "here is the answer"},
                    {"type": "tool_use", "id": "c1", "name": "bash", "input": {"command": "ls"}}
                ]
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "c1", "content": "(no output)"}]
            }))
            .unwrap(),
            Message::user_text("and now this"),
            Message::user_text("more of the same turn"),
        ];

        let or = training_jsonl(&t, &t.system, "openrouter").unwrap();
        let lines: Vec<&str> = or.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "one conversation per line");
        let v: Value = serde_json::from_str(lines[0]).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "PROJECT RULES");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "do the thing");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "here is the answer");
        let whole = lines[0];
        assert!(!whole.contains("secret reasoning"));
        assert!(!whole.contains("tool_use"));
        assert!(!whole.contains("(no output)"));

        // Claude wants the system prompt outside `messages`.
        let cl = training_jsonl(&t, &t.system, "claude").unwrap();
        let v: Value = serde_json::from_str(cl.trim_end()).unwrap();
        assert_eq!(v["system"], "PROJECT RULES");
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user", "no system turn inside messages");
        assert!(
            msgs.windows(2).all(|w| w[0]["role"] != w[1]["role"]),
            "Claude's trainer rejects two turns of the same role"
        );
        assert_eq!(msgs[1]["content"], "here is the answer");
    }

    /// A chat with nothing the model said is not training data.
    #[test]
    fn training_export_needs_an_assistant_turn() {
        let mut t = chat("t");
        t.messages = vec![Message::user_text("just me talking")];
        assert!(training_jsonl(&t, "", "openrouter").is_err());
        assert!(training_jsonl(&t, "", "claude").is_err());
        assert!(training_jsonl(&t, "", "nonsense").is_err());
    }

    /// The whole path: a chat on disk becomes a JSONL file, with the project
    /// instructions in it.
    #[tokio::test]
    async fn training_export_writes_a_file_with_the_project_instructions() {
        let dir = std::env::temp_dir().join(format!("openleash-train-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _home = store::test_home(&dir.join("home"));
        // A project with its own rules, which the system prompt must carry.
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("AGENTS.md"), "RULE ONE: never touch the physics folder.").unwrap();

        let mut t = chat("a");
        t.project = proj.to_string_lossy().to_string();
        t.cwd = proj.to_string_lossy().to_string();
        t.messages = vec![Message::user_text("hello"), Message::user_text("more")];
        t.messages.push(serde_json::from_value(json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi there"}]
        })).unwrap());
        let h = harness(vec![t]);

        // A folder nobody has decided about is untrusted, and an untrusted
        // folder's own instructions are not injected — into the prompt or into
        // a training set built from it. So the test trusts it first, which is
        // also what the user has to have done for the agent to have worked
        // under those rules in the first place.
        let _trust = crate::agent::trust::TRUST_LOCK.lock().unwrap();
        crate::agent::trust::set_trust(&[crate::agent::trust::TrustDecision {
            path: crate::agent::trust::norm_path(&proj.to_string_lossy()),
            kind: crate::agent::trust::TrustKind::Folder,
            decision: crate::agent::trust::TrustState::Trusted,
            decided_at: chrono::Utc::now(),
        }]);

        let path = dir.join("train.jsonl");
        training_export(&h, "a", &path.to_string_lossy(), "openrouter").await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(text.trim_end()).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert!(
            msgs[0]["content"].as_str().unwrap().contains("RULE ONE"),
            "the project's own instructions travel with the training data: {}",
            msgs[0]["content"]
        );
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hello\n\nmore");
        assert_eq!(msgs[2]["role"], "assistant");

        // A chat that isn't there is an error, not an empty file.
        assert!(training_export(&h, "ghost", &path.to_string_lossy(), "openrouter").await.is_err());

        crate::agent::trust::set_trust(&[]);
        drop(_trust);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The exporter, run against a chat that went through the app's own file
    /// format rather than a hand-built fixture: the shapes that matter (a tool
    /// call in the middle, a tool result, two turns of the same role) all show
    /// up in a real transcript, and a fixture with only the easy cases proves
    /// nothing about the hard ones.
    #[tokio::test]
    async fn training_export_of_a_saved_chat_is_well_formed() {
        let dir = std::env::temp_dir().join(format!("openleash-trainreal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _home = store::test_home(&dir.join("home"));
        let mut t = chat("real");
        t.messages = vec![
            Message::user_text("first question"),
            serde_json::from_value(json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "on it"},
                    {"type": "tool_use", "id": "c1", "name": "read_file", "input": {"path": "a.rs"}}
                ]
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "c1", "content": "fn main() {}"}]
            }))
            .unwrap(),
            Message::user_text("second question"),
            Message::user_text("third question, same turn"),
            serde_json::from_value(json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "here is the answer"}]
            }))
            .unwrap(),
        ];
        let h = harness(vec![t]);
        // Write it the way the app does, so the file round-trip is part of what
        // is under test rather than a fixture that skipped the disk entirely.
        let snapshot = h.task("real").await.unwrap().lock().await.clone();
        store::save_task(&snapshot);

        let path = dir.join("real.jsonl");
        training_export(&h, "real", &path.to_string_lossy(), "claude").await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "one conversation per line");
        let v: Value = serde_json::from_str(lines[0]).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert!(
            msgs.windows(2).all(|w| w[0]["role"] != w[1]["role"]),
            "Claude's trainer rejects two turns of the same role: {msgs:?}"
        );
        assert_eq!(msgs.first().unwrap()["role"], "user", "Claude starts with a user turn");
        assert_eq!(msgs.last().unwrap()["role"], "assistant", "Claude ends with an assistant turn");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "first question");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "on it");
        assert!(
            !lines[0].contains("tool_use") && !lines[0].contains("fn main"),
            "tool traffic is not training data: {}",
            lines[0]
        );
        let last = msgs.last().unwrap();
        assert_eq!(last["role"], "assistant");
        assert_eq!(last["content"], "here is the answer");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Changing a chat's folder has to re-home it, not just rewrite two path
    /// fields: the frozen system prompt names the old folder and holds its
    /// project instructions, so a chat left with it would go on believing it
    /// was working in the old place. A running chat is refused, and so is a
    /// folder that isn't there.
    #[tokio::test]
    async fn changing_a_chats_folder_rehomes_it() {
        let dir = std::env::temp_dir().join(format!("openleash-chgdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _home = store::test_home(&dir.join("home"));
        let new = dir.join("elsewhere");
        std::fs::create_dir_all(&new).unwrap();
        let mut t = chat("a");
        t.project = "/old/proj".into();
        t.cwd = "/old/proj/.wt".into();
        t.worktree = true;
        t.base_commit = Some("abc".into());
        t.system = "Working directory: /old/proj".into();
        t.mcp_tools = vec![json!({"name": "mcp__x__y"})];
        t.told.insert("assist".into(), json!("guide"));
        t.subs = vec![serde_json::from_value(json!({
            "id": "s1", "role": "general", "task": "", "status": "done", "meta": "",
            "model": "m", "cwd": "/old/proj/.wt-s1", "branch": "s1"
        }))
        .unwrap()];
        let mut running = chat("r");
        running.status = "running".into();
        let h = harness(vec![t, running]);

        let new_s = new.to_string_lossy().to_string();
        assert!(
            change_folder(&h, "a", &dir.join("missing").to_string_lossy())
                .await
                .is_err()
        );
        assert!(
            change_folder(&h, "r", &new_s).await.is_err(),
            "a running chat is refused"
        );
        change_folder(&h, "a", &format!("  {new_s}  "))
            .await
            .unwrap();
        let t = h.task("a").await.unwrap();
        let t = t.lock().await;
        assert_eq!(
            (t.project.as_str(), t.cwd.as_str()),
            (new_s.as_str(), new_s.as_str())
        );
        assert!(!t.worktree && t.base_commit.is_none());
        assert!(
            t.system.is_empty(),
            "the prompt naming the old folder is rebuilt"
        );
        assert!(t.mcp_tools.is_empty() && t.told.is_empty());
        assert!(t.subs[0].cwd.is_empty() && t.subs[0].branch.is_empty());
        assert!(t.stop_note.as_deref().unwrap().contains("/old/proj/.wt"));
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Without git the header's totals are the snapshot review's own, summed:
    /// the header and the review screen never disagree.
    #[tokio::test]
    async fn review_stats_without_git_sum_the_snapshot_review() {
        let dir = std::env::temp_dir().join(format!("openleash-rstats-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let work = dir.join("nogit");
        std::fs::create_dir_all(&work).unwrap();
        let (a, b) = (work.join("a.txt"), work.join("b.txt"));
        std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
        std::fs::write(&b, "new\n").unwrap();
        let mut t = chat("rs");
        t.cwd = work.to_string_lossy().to_string();
        t.base_commit = None;
        t.touched
            .insert(a.to_string_lossy().to_string(), Some("one\n".into()));
        t.touched.insert(b.to_string_lossy().to_string(), None);
        let h = harness(vec![t]);
        let stats = review_stats(&h, "rs").await.unwrap();
        assert_eq!(
            stats,
            agent::git::ReviewStats {
                files: 2,
                add: 3,
                del: 0
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    // ─────────────────────────── checkpoint / rewind / fork ───────────────────────────

    /// A conversation-only rewind must not touch a single byte on disk. It is
    /// the one mode whose whole point is "undo the transcript, keep the files" —
    /// the user asked to re-word a prompt, not to lose whatever the agent had
    /// written when they did. The dangerous failure is `truncate` + a restore
    /// that the mode was supposed to skip: the transcript looks right and the
    /// files are silently rolled back a turn.
    #[tokio::test]
    async fn a_conversation_rewind_leaves_the_files_alone() {
        let dir = std::env::temp_dir().join(format!("openleash-cp-conv-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let work = dir.join("convwork");
        std::fs::create_dir_all(&work).unwrap();
        let cwd = work.to_string_lossy().to_string();

        // The state before the first prompt, and the state after it: the shadow
        // store has both, so a files-mode rewind would have something to undo.
        std::fs::write(work.join("f.txt"), "before\n").unwrap();
        checkpoint::snapshot("conv", &cwd, 0, checkpoint::Kind::User).unwrap();
        std::fs::write(work.join("f.txt"), "after\n").unwrap();
        checkpoint::snapshot("conv", &cwd, 1, checkpoint::Kind::Agent).unwrap();

        // One user row, one checkpoint at it — the minimum a rewind needs.
        let mut t = chat("conv");
        t.cwd = cwd.clone();
        t.items = vec![Item::new("user", "hello", json!({}))];
        t.messages = vec![agent::Message::user_text("hello")];
        t.checkpoints = vec![Checkpoint {
            item_index: 0,
            msg_index: 0,
        }];
        let item_id = t.items[0].id.clone();
        let h = harness(vec![t]);

        let out = rewind_to(&h, "conv".into(), item_id, "conversation".into())
            .await
            .expect("a conversation rewind is allowed");
        assert_eq!(
            out["restored"].as_u64(),
            Some(0),
            "no file work in a conversation-only rewind: {out}"
        );

        assert_eq!(
            std::fs::read_to_string(work.join("f.txt")).unwrap(),
            "after\n",
            "the file the agent's turn wrote must survive the rewind"
        );
        let t = h.task("conv").await.unwrap();
        let t = t.lock().await;
        assert!(
            t.items.is_empty(),
            "but the conversation is truncated back to before the prompt"
        );
        assert!(t.messages.is_empty(), "and so is the model history");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A checkpoint fork is a real sibling checkout of the project, seeded with
    /// the files as they were at the message forked at. The subtle half is the
    /// transcript: `fork_at` drops the row it branches at and then *prepends* an
    /// origin notice, so every checkpoint — an index into `items` — has to move
    /// down one. Get that off and rewinding in the fork lands a row early, on
    /// the notice, and reports the wrong message.
    #[tokio::test]
    async fn a_checkpoint_fork_starts_from_the_forked_turns_files() {
        let dir = std::env::temp_dir().join(format!("openleash-cp-fork-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cwd = proj.to_string_lossy().to_string();

        // A real repository, because the fork creates a git worktree of it.
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "t@example.invalid"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(&args)
                .output()
                .expect("git should be installed");
            assert!(out.status.success(), "git {args:?} failed");
        }
        std::fs::write(proj.join("tracked.txt"), "base\n").unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap()
            .success());
        // `git init`'s default branch name is a host setting, so read it back
        // rather than assuming `master`: the fork bases its worktree on it.
        let branch = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();

        // The snapshots a real turn loop would take, and the counts it would
        // stamp them with. Checkpoint `k` is the folder *before* prompt `k` —
        // the newest snapshot with `count <= k` — and the count is
        // `checkpoints.len()` at the moment of the snapshot, so the agent
        // snapshot that *ends* turn `n` carries count `n + 1`.
        let _ = checkpoint::snapshot("fork", &cwd, 0, checkpoint::Kind::User)
            .unwrap()
            .expect("the brand-new worktree is untracked, so the store commits it");
        std::fs::write(proj.join("a.txt"), "turn zero\n").unwrap();
        checkpoint::snapshot("fork", &cwd, 1, checkpoint::Kind::Agent)
            .unwrap()
            .expect("a.txt/one is turn 0's work");
        std::fs::write(proj.join("b.txt"), "turn one\n").unwrap();
        checkpoint::snapshot("fork", &cwd, 2, checkpoint::Kind::Agent)
            .unwrap()
            .expect("b.txt is the second turn's work");
        let mut t = chat("fork");
        t.title = "Fork me".into();
        t.project = cwd.clone();
        t.cwd = cwd.clone();
        t.branch = branch.clone();
        t.base_branch = branch.clone();
        t.items = vec![
            Item::new("user", "first prompt", json!({})),
            Item::new("user", "second prompt", json!({})),
        ];
        t.messages = vec![
            agent::Message::user_text("first prompt"),
            agent::Message::user_text("second prompt"),
        ];
        t.checkpoints = vec![
            Checkpoint {
                item_index: 0,
                msg_index: 0,
            },
            Checkpoint {
                item_index: 1,
                msg_index: 1,
            },
        ];
        let at = t.items[1].id.clone();
        let h = harness(vec![t]);

        let sum = fork_at(&h, "fork".into(), at)
            .await
            .expect("the fork is made");
        assert_ne!(sum.id, "fork", "a fork is its own chat");
        assert_eq!(
            sum.base_branch, branch,
            "the fork is based on the chat's own base branch"
        );

        let f = h.task(&sum.id).await.unwrap();
        let f = f.lock().await;
        assert_eq!(
            f.forked_from.as_deref(),
            Some("fork"),
            "the origin is recorded"
        );
        // The transcript stops before the message forked at, then the notice is
        // prepended, so the notice is row 0 and exactly one real message follows.
        assert_eq!(f.items.len(), 2, "one kept prompt plus the notice");
        assert_eq!(f.items[0].kind, "notice", "the notice is the first row");
        assert_eq!(
            f.items[1].text, "first prompt",
            "the fork starts one turn back"
        );
        assert!(f.worktree, "a fork gets its own checkout");
        assert!(
            PathBuf::from(&f.cwd).is_dir(),
            "the worktree directory exists: {}",
            f.cwd
        );

        // Checkpoint 0 wanted the folder *before* prompt 1 — `a`'s file state —
        // and the worktree was seeded with it. The `b.txt` written for prompt 1
        // must not be there: a fork starts from the files of the turn it forked
        // at, not from the branch tip it never got to.
        assert_eq!(
            std::fs::read_to_string(PathBuf::from(&f.cwd).join("a.txt")).unwrap(),
            "turn zero\n",
            "the checkpoint's file is in the new worktree"
        );
        assert!(
            !PathBuf::from(&f.cwd).join("b.txt").exists(),
            "and the later turn's file is not"
        );

        // Every checkpoint moved down one for the notice. The fork kept only the
        // first, so the shift is checked against where it points now.
        assert_eq!(
            f.checkpoints.len(),
            1,
            "the forked-at checkpoint is dropped"
        );
        assert_eq!(
            f.checkpoints[0].item_index, 1,
            "the notice shifts the surviving checkpoint down one"
        );
        assert_eq!(
            f.items[f.checkpoints[0].item_index].text, "first prompt",
            "and it still points at the prompt it always did, not the notice"
        );

        // Only what this test created: the worktree branch and directory.
        let (wt, branch) = (f.cwd.clone(), f.branch.clone());
        drop(f);
        git::remove_worktree(&cwd, &wt).unwrap();
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["branch", "-D", &branch])
                .output()
                .unwrap()
                .status
                .success(),
            "the fork's branch is cleaned up too"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A remote (SSH) chat's reply images are read over SFTP with the same
    /// rules: image extension *and* magic bytes, and only on the chat's host.
    #[tokio::test]
    async fn reply_image_reads_remote_project_images_with_the_same_checks() {
        use agent::ssh_session::tests::{host_id, install_fs, sample_fs, Node};
        let host = host_id();
        let fs = sample_fs();
        let png = [
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..],
            b"rest",
        ]
        .concat();
        fs.lock()
            .unwrap()
            .insert("/proj/shot.png".into(), Node::File(png, 0o644));
        fs.lock().unwrap().insert(
            "/proj/fake.png".into(),
            Node::File(b"secret".to_vec(), 0o644),
        );
        install_fs(&host, fs);
        let cwd = agent::host::ssh_uri(&host, "/proj");
        assert!(reply_image(cwd.clone(), "shot.png".into())
            .await
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(reply_image(cwd.clone(), "/proj/shot.png".into())
            .await
            .is_ok());
        for bad in [
            "fake.png",
            "a.txt",
            "missing.png",
            "ssh://elsewhere/proj/shot.png",
            "link_out/passwd",
        ] {
            assert!(reply_image(cwd.clone(), bad.into()).await.is_err(), "{bad}");
        }
        assert!(names_local_file("C:/Users/me/shot.png") && names_local_file("file:///tmp/x.png"));
        assert!(!names_local_file("/tmp/x.png") && !names_local_file("out/x.png"));
    }

    // A reply's `![x](path)` is the agent's own text, so the command that turns
    // it into pixels must be at least as strict as `open_path`.
    #[test]
    fn reply_image_shows_project_images_and_refuses_everything_else() {
        let base =
            std::env::temp_dir().join(format!("ol-replyimg-{}", uuid::Uuid::new_v4().simple()));
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join("out")).unwrap();
        let png: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        std::fs::write(proj.join("out").join("chart.png"), png).unwrap();
        std::fs::write(proj.join("fake.png"), b"API_KEY=sk-dummy").unwrap();
        std::fs::write(proj.join(".env"), b"API_KEY=sk-dummy").unwrap();
        std::fs::write(base.join("outside.png"), png).unwrap();
        let cwd = proj.to_string_lossy().to_string();

        let ok = reply_image_data(&cwd, "out/chart.png").unwrap();
        assert!(ok.starts_with("data:image/png;base64,"), "{ok}");
        let abs = proj.join("out").join("chart.png");
        assert!(reply_image_data(&cwd, &abs.to_string_lossy()).is_ok());
        let url = format!("file:///{}", abs.to_string_lossy().replace('\\', "/"));
        assert!(reply_image_data(&cwd, &url).is_ok(), "{url}");

        // Outside the folder is fine for a real image: the user's own window shows it.
        assert!(reply_image_data(&cwd, "../outside.png").is_ok());
        assert!(reply_image_data(&cwd, &base.join("outside.png").to_string_lossy()).is_ok());
        assert!(reply_image_data("", &base.join("outside.png").to_string_lossy()).is_ok());
        // Another machine's share must never be opened from agent text: on
        // Windows that authenticates to the host with the user's credentials.
        for share in [
            r"\\attacker\share\x.png",
            "//attacker/share/x.png",
            "file://attacker/share/x.png",
            "FILE://attacker/share/x.png",
            r"\\?\UNC\attacker\share\x.png",
            r"\\.\UNC\attacker\share\x.png",
            r"\??\UNC\attacker\share\x.png",
            r"\\?\GLOBALROOT\Device\Mup\attacker\share\x.png",
            r"\\.\GLOBALROOT\Device\Mup\attacker\share\x.png",
            r"\\wsl.localhost.attacker.com\share\x.png",
        ] {
            let err = reply_image_data(&cwd, share).unwrap_err();
            assert!(err.contains("network share"), "{share}: {err}");
        }
        // …nor through a link on a local disk that points at one. Creating a
        // symlink needs Developer Mode on Windows, so skip where it can't.
        #[cfg(windows)]
        let linked =
            std::os::windows::fs::symlink_dir(r"\\attacker\share", proj.join("pics")).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink("//attacker/share", proj.join("pics")).is_ok();
        if linked {
            let err = reply_image_data(&cwd, "pics/x.png").unwrap_err();
            assert!(err.contains("network share"), "{err}");
        }
        // Not an image by name, or by bytes.
        assert!(reply_image_data(&cwd, ".env").is_err());
        assert!(reply_image_data(&cwd, "fake.png").is_err());
        // Missing, empty, and relative with no folder to resolve against.
        assert!(reply_image_data(&cwd, "nope.png").is_err());
        assert!(reply_image_data(&cwd, "  ").is_err());
        assert!(reply_image_data("", "out/chart.png").is_err());

        let _ = std::fs::remove_dir_all(base);
    }
    // ───────── artificts: submitting feedback is the handoff ─────────

    /// A task whose project folder is `project`, for the artifict handoff tests.
    fn artifict_task(id: &str, project: &str) -> Task {
        serde_json::from_value(json!({
            "id": id, "title": "", "status": "idle", "step": "", "project": project, "cwd": project,
            "branch": "", "worktree": false, "model": "unused/model", "effort": 2,
            "perm": "disabled", "plan": false, "todos": [], "subs": [], "items": [], "messages": [],
            "checkpoints": [],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn artifict_input(content: &str) -> artificts::ArtifictInput {
        artificts::ArtifictInput {
            title: "Design".into(),
            kind: "html".into(),
            content: content.into(),
            decisions: vec![],
            constraints: vec![],
            code_refs: vec![],
        }
    }

    /// A submitted request must actually reach the agent. Before this, the
    /// workspace wrote the feedback file and stopped: the agent only ever saw it
    /// if it happened to call `artifict_feedback_list` on a later turn, so
    /// "Send" looked delivered when nothing had been.
    #[tokio::test]
    async fn submitted_artifict_feedback_is_delivered_to_the_agent_as_a_turn() {
        let home = std::env::temp_dir().join(format!("openleash-art-fdbk-{}", std::process::id()));
        let _home = store::test_home(&home);
        let project =
            std::env::temp_dir().join(format!("openleash-art-fdbk-proj-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();
        let h = Arc::new(harness(vec![artifict_task("q", &project_path)]));

        let created = artificts::create(&project_path, artifict_input("<h1>v1</h1>")).unwrap();
        let annotated = artificts::annotate(
            &project_path,
            &created.id,
            &created.current_version_id,
            "Move this button",
            Some(json!({"selector": "button", "target": "Save", "x": 25.0, "y": 40.0})),
        )
        .unwrap();
        let annotation_id = annotated.annotations[0].id.clone();

        let submitted = submit_artifict_feedback(
            &h,
            "q",
            &created.id,
            &created.current_version_id,
            "Make the save button primary".into(),
            vec![annotation_id],
            Some(json!({"selected": "primary"})),
        )
        .await
        .unwrap();
        assert_eq!(submitted.feedback.len(), 1, "the request is recorded");

        // It reached the agent as a real turn: the idled chat took the message
        // into its own history and started a run, so what the model will read is
        // the history itself (a *running* chat steers through the inbox instead —
        // see the mid-run test below).
        let task_ref = h.task("q").await.unwrap();
        let delivered = serde_json::to_string(&task_ref.lock().await.messages).unwrap();
        let notes = delivered.clone();
        assert!(notes.contains("Make the save button primary"), "{notes}");
        assert!(
            notes.contains(&created.current_version_id),
            "names the exact revision under review: {notes}"
        );
        assert!(notes.contains("Move this button"), "carries the annotation");
        assert!(notes.contains("primary"), "carries the captured state");
        assert!(
            notes.contains(&submitted.feedback[0].id),
            "names the feedback id so the agent can respond to it"
        );

        // The user's own request is visible in the transcript, not absorbed
        // silently: the turn names the artifict it is about.
        let items = { task_ref.lock().await.items.clone() };
        assert!(
            items
                .iter()
                .any(|i| i.kind == "user" && i.text.contains("Design")),
            "the request shows in the transcript: {items:?}"
        );

        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(home);
    }

    /// Steering, not queueing: a request sent while the agent is already working
    /// is folded into that run's next model call, exactly like typing into the
    /// composer mid-turn.
    #[tokio::test]
    async fn artifict_feedback_sent_mid_run_steers_the_working_agent() {
        let home = std::env::temp_dir().join(format!("openleash-art-steer-{}", std::process::id()));
        let _home = store::test_home(&home);
        let project =
            std::env::temp_dir().join(format!("openleash-art-steer-proj-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();
        let h = Arc::new(harness(vec![artifict_task("q", &project_path)]));
        h.runtime("q")
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let created = artificts::create(&project_path, artifict_input("<h1>v1</h1>")).unwrap();
        submit_artifict_feedback(
            &h,
            "q",
            &created.id,
            &created.current_version_id,
            "Tighten the spacing".into(),
            vec![],
            None,
        )
        .await
        .unwrap();

        assert!(
            h.runtime("q").queue.lock().await.is_empty(),
            "a plain request steers rather than waiting for the end of the turn"
        );
        let notes = h
            .runtime("q")
            .inbox
            .lock()
            .await
            .get("main")
            .cloned()
            .unwrap_or_default()
            .concat();
        assert!(notes.contains("Tighten the spacing"), "{notes}");

        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(home);
    }

    /// A delivery that is refused must not leave a record claiming the agent is
    /// waiting on it, or a retry would silently duplicate an unanswered request.
    #[tokio::test]
    async fn refused_artifict_feedback_is_withdrawn_rather_than_left_pending() {
        let home =
            std::env::temp_dir().join(format!("openleash-art-withdraw-{}", std::process::id()));
        let _home = store::test_home(&home);
        let project = std::env::temp_dir().join(format!(
            "openleash-art-withdraw-proj-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();
        let h = Arc::new(harness(vec![artifict_task("q", &project_path)]));

        let created = artificts::create(&project_path, artifict_input("<h1>v1</h1>")).unwrap();
        let annotated = artificts::annotate(
            &project_path,
            &created.id,
            &created.current_version_id,
            "A private note",
            None,
        )
        .unwrap();
        let annotation_id = annotated.annotations[0].id.clone();

        // A chat with no project folder cannot take artificts, so delivery fails
        // after the feedback record has already been written.
        h.update_task("q", |t| t.project = String::new()).await;
        let outcome = submit_artifict_feedback(
            &h,
            "q",
            &created.id,
            &created.current_version_id,
            "Do something".into(),
            vec![annotation_id.clone()],
            None,
        )
        .await;
        assert!(outcome.is_err(), "the refused handoff surfaces its reason");

        // Nothing is left behind that the agent could answer or the UI would
        // show as pending, and the annotation returns to being private.
        let after = artificts::get(&project_path, &created.id).unwrap();
        assert!(
            after.feedback.is_empty(),
            "a refused submission leaves no pending record: {:?}",
            after.feedback
        );
        assert!(
            artificts::feedback_list_for_agent(&project_path, None)
                .unwrap()
                .is_empty(),
            "the agent is offered nothing to answer"
        );
        assert!(
            !after
                .annotations
                .iter()
                .find(|a| a.id == annotation_id)
                .unwrap()
                .submitted,
            "the withdrawn annotation is private again"
        );

        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(home);
    }

    /// A comment the agent was handed must not be removable: it is part of the
    /// request the agent may already have answered, and `validate_artifict`
    /// requires every submitted comment to stand behind a submission, so
    /// allowing this would also write a file the store itself rejects.
    #[tokio::test]
    async fn a_sent_comment_cannot_be_removed_but_an_unsent_one_can() {
        let home = std::env::temp_dir().join(format!("openleash-art-rm-{}", std::process::id()));
        let _home = store::test_home(&home);
        let project =
            std::env::temp_dir().join(format!("openleash-art-rm-proj-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();

        let created = artificts::create(&project_path, artifict_input("<h1>v1</h1>")).unwrap();
        let version = created.current_version_id.clone();

        // One comment that gets sent, one that is only ever private.
        let first = artificts::annotate(&project_path, &created.id, &version, "Sent one", None)
            .unwrap()
            .annotations[0]
            .id
            .clone();
        let with_two =
            artificts::annotate(&project_path, &created.id, &version, "Private one", None).unwrap();
        let second = with_two
            .annotations
            .iter()
            .find(|a| a.id != first)
            .unwrap()
            .id
            .clone();
        artificts::feedback_submit(
            &project_path,
            &created.id,
            &version,
            "Do the sent one",
            vec![first.clone()],
            None,
        )
        .unwrap();

        assert!(
            artificts::remove_annotation(&project_path, &created.id, &first).is_err(),
            "a comment already handed to the agent stays in the record"
        );
        // The refusal left the artifict untouched, not half-removed.
        let after = artificts::get(&project_path, &created.id).unwrap();
        assert!(after.annotations.iter().any(|a| a.id == first));
        assert_eq!(after.feedback.len(), 1);

        let pruned = artificts::remove_annotation(&project_path, &created.id, &second).unwrap();
        assert!(
            !pruned.annotations.iter().any(|a| a.id == second),
            "an unsent comment is the user's to remove"
        );
        assert!(
            pruned.annotations.iter().any(|a| a.id == first),
            "and removing it does not disturb what was sent"
        );
        // What the agent can still answer is unchanged by the removal.
        assert_eq!(
            artificts::feedback_list_for_agent(&project_path, None)
                .unwrap()
                .len(),
            1
        );
        // A comment from another artifict, or a bad id, cannot be aimed at this one.
        assert!(artificts::remove_annotation(&project_path, &created.id, "../outside").is_err());

        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(home);
    }
}
