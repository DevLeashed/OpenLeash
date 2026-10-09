// OpenLeash — Tauri IPC layer. Domain logic lives in `agent/`; this file
// owns app state and exposes commands to the frontend. Live updates flow
// the other way as `ol://event` / `ol://attention` / `ol://usage` events.

mod agent;
mod artifacts;
mod visualization;
mod win;

use agent::providers::{self, ModelInfo};
use agent::store::{self, AgentDef, CustomProvider, McpServerCfg, Settings, SkillDef};
use agent::{
    accounts, checkpoint, git, router, runner, Checkpoint, Harness, Item, Task, TaskSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
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
        let connected = custom || !key.is_empty() || accts > 0 || (local && cfg.is_some());
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
        out.push(t.lock().await.summary());
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

/// Turn absolute paths from the file dialog / a window drop into attachments.
///
/// The browser `File` API never carries a real path, so anything picked or
/// dropped for the agent to read comes through here instead. Images are
/// reported as `image: true` and the UI re-reads them itself for the model to
/// see; everything else is just a path the agent opens with its own tools.
#[tauri::command]
fn files_attach(paths: Vec<String>) -> Result<Vec<FileRef>, String> {
    let mut out = vec![];
    for raw in paths {
        let p = PathBuf::from(&raw);
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
    })
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

/// User-facing artifact workspace IPC. The project root always comes from the
/// task record, never from an invoke argument, so a caller cannot use these
/// commands to select an arbitrary filesystem location.
async fn artifact_project(h: &Harness, task_id: &str) -> Result<String, String> {
    let task = h.task(task_id).await?;
    let project = task.lock().await.project.clone();
    if project.trim().is_empty() {
        return Err("This task has no project folder for artifacts.".into());
    }
    Ok(project)
}

#[tauri::command]
async fn artifact_list(
    h: H<'_>,
    task_id: String,
) -> Result<Vec<artifacts::ArtifactSummary>, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::list(&project)
}

#[tauri::command]
async fn artifact_get(
    h: H<'_>,
    task_id: String,
    artifact_id: String,
) -> Result<artifacts::Artifact, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::get(&project, &artifact_id)
}

#[tauri::command]
async fn artifact_create(
    h: H<'_>,
    task_id: String,
    input: artifacts::ArtifactInput,
) -> Result<artifacts::Artifact, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::create(&project, input)
}

#[tauri::command]
async fn artifact_revise(
    h: H<'_>,
    task_id: String,
    artifact_id: String,
    parent_version_id: String,
    input: artifacts::ArtifactInput,
) -> Result<artifacts::Artifact, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::revise(&project, &artifact_id, &parent_version_id, input)
}

#[tauri::command]
async fn artifact_annotate(
    h: H<'_>,
    task_id: String,
    artifact_id: String,
    version_id: String,
    text: String,
    anchor: Option<Value>,
) -> Result<artifacts::Artifact, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::annotate(&project, &artifact_id, &version_id, &text, anchor)
}

#[tauri::command]
async fn artifact_feedback_submit(
    h: H<'_>,
    task_id: String,
    artifact_id: String,
    version_id: String,
    text: String,
    annotation_ids: Vec<String>,
    interactive_state: Option<Value>,
) -> Result<artifacts::Artifact, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::feedback_submit(
        &project,
        &artifact_id,
        &version_id,
        &text,
        annotation_ids,
        interactive_state,
    )
}

#[tauri::command]
async fn artifact_feedback_list(
    h: H<'_>,
    task_id: String,
    artifact_id: Option<String>,
) -> Result<Vec<artifacts::Feedback>, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::feedback_list(&project, artifact_id.as_deref())
}

#[tauri::command]
async fn artifact_feedback_respond(
    h: H<'_>,
    task_id: String,
    feedback_id: String,
    decision: String,
    response: String,
) -> Result<artifacts::Artifact, String> {
    let project = artifact_project(&h, &task_id).await?;
    artifacts::feedback_respond(&project, &feedback_id, &decision, &response)
}

/// Explicit user input to the main agent's existing browser; never an agent tool.
#[tauri::command]
async fn browser_panel(
    h: H<'_>,
    task_id: String,
    action: agent::browser::PanelAction,
) -> Result<agent::browser::PanelFrame, String> {
    browser_panel_for(&h, &task_id, action).await
}

async fn browser_panel_for(
    h: &Harness,
    task_id: &str,
    action: agent::browser::PanelAction,
) -> Result<agent::browser::PanelFrame, String> {
    h.task(task_id).await?;
    action.validate()?;
    let cfg = h.settings.read().await.plugins.browser.clone();
    if !cfg.enabled {
        return Err("The browser plugin is turned off (Settings → Plugins).".into());
    }
    let session = agent::browser::session_for(
        &format!("{task_id}:main"),
        if cfg.width == 0 { 1280 } else { cfg.width },
        if cfg.height == 0 { 800 } else { cfg.height },
    )
    .await?;
    session.panel(action).await
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
    ultra: bool,
    #[serde(default)]
    ultra_wt: bool,
    /// ULTRATHREAD X: the per-layer ladder. Present and >= 2 layers = X mode.
    #[serde(default)]
    ultra_x: Option<store::UltraX>,
}

#[tauri::command]
async fn task_create(h: H<'_>, req: NewTask) -> Result<TaskSummary, String> {
    let project = req.project.trim().to_string();
    if project.is_empty() || !std::path::Path::new(&project).is_dir() {
        return Err("Pick a project folder first.".into());
    }
    let id = agent::new_id();
    let title: String = req
        .prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("New task")
        .trim()
        .chars()
        .take(70)
        .collect();
    let repo = git::metadata(&project);
    let is_git = repo.is_some();
    let mut cwd = project.clone();
    let mut branch = repo.as_ref().map(|m| m.branch.clone()).unwrap_or_default();
    let mut base_commit = repo.as_ref().and_then(|m| m.head.clone());
    let mut notice = None;
    let base_branch = if req.base_branch.is_empty() {
        branch.clone()
    } else {
        req.base_branch.clone()
    };
    let mut worktree = false;
    if req.worktree && is_git {
        match git::create_worktree(&project, &base_branch, &git::slug(&req.prompt)) {
            Ok((dir, br)) => {
                // A fresh worktree is a checkout of the *committed* tree, so the
                // `.env`, `node_modules` and untracked output the user's own
                // checkout has are all missing from it, and the agent's first
                // command fails for a reason that has nothing to do with its
                // task. Put back what the repository asked for (its
                // `.worktreeinclude`) and give a `post_setup_worktree` hook the
                // chance to do the rest — see `checks::setup_worktree`. Done
                // here, before the task exists, so the very first tool call runs
                // in a usable tree.
                let hooks = {
                    let s = h.settings.read().await;
                    agent::checks::hooks_for(&s, &project)
                };
                let setup = agent::checks::setup_worktree(
                    &hooks,
                    &project,
                    &dir,
                    &id,
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await;
                let failed: Vec<String> = setup
                    .hooks
                    .iter()
                    .filter(|o| !o.ok)
                    .map(|o| format!("`{}`: {}", o.command, o.output))
                    .collect();
                let mut setup_issues = Vec::new();
                if let Some(error) = setup.copy_error {
                    setup_issues.push(format!("included-file copy failed: {error}"));
                }
                if !failed.is_empty() {
                    setup_issues.push(format!("setup hook failed: {}", failed.join("; ")));
                }
                if !setup_issues.is_empty() {
                    // Told, not fatal: a setup failure must not cost the user
                    // their task, but it must not leave the agent hitting the
                    // same missing dependency with no explanation.
                    notice = Some(format!(
                        "The worktree was created, but setup did not finish cleanly: {}",
                        setup_issues.join("; ")
                    ));
                } else if setup.copied > 0 {
                    notice = Some(format!(
                        "Copied {} file{} from `.worktreeinclude` into the new worktree.",
                        setup.copied,
                        if setup.copied == 1 { "" } else { "s" }
                    ));
                }
                cwd = dir;
                branch = br;
                // A selected base branch may differ from the project’s current
                // HEAD, so read the new checkout only on the worktree path.
                base_commit = git::head(&cwd);
                worktree = true;
            }
            Err(e) => {
                notice = Some(format!(
                    "Couldn't create a worktree ({e}). Working directly in the project."
                ))
            }
        }
    } else if req.worktree {
        notice =
            Some("This folder isn't a git repository, so the task runs directly in it.".into());
    }
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
        effort: req.effort.min(4),
        perm: req.perm,
        plan: req.plan,
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
        wrap_up: false,
        model_map: Default::default(),
        route: req
            .route
            .clone()
            .unwrap_or_else(|| router::default_route(&req.model)),
        archived: false,
        hidden: false,
        forked_from: None,
        pinned: false,
        order: 0.0,
        items: vec![],
        messages: vec![],
        todos: vec![],
        subs: vec![],
        usage: Default::default(),
        read_files: HashMap::new(),
        checkpoints: vec![],
        touched: HashMap::new(),
        created_at: now,
        updated_at: now,
        // A new chat is one the user just started, so it opens at the top.
        touched_at: now,
        // Built in memory, so its sub-agent maps are here rather than on disk.
        hydrated: true,
    };
    if let Some(n) = notice {
        task.items
            .push(Item::new("notice", n, json!({"level": "info"})));
    }
    let summary = task.summary();
    h.tasks
        .write()
        .await
        .insert(id.clone(), Arc::new(Mutex::new(task)));
    {
        let mut s = h.settings.write().await;
        s.model = req.model.clone();
        s.effort = req.effort;
        if !req.assist.is_empty() {
            s.assist = task_assist(&req.assist);
        }
        if let Some(a) = &req.agents {
            s.default_agents = store::with_required_agents(a.clone());
        } else {
            // Even when the caller doesn't touch agents, the defaults stay whole.
            store::ensure_required_agents(&mut s.default_agents);
        }
        s.recent_models.retain(|m| m != &req.model);
        s.recent_models.insert(0, req.model);
        s.recent_models.truncate(5);
        if !s.projects.contains(&project) {
            s.projects.insert(0, project.clone());
        }
        s.project = project;
        store::save_settings(&s);
    }
    if req.images.is_empty() {
        dispatch(h.inner().clone(), id, req.prompt.trim().to_string()).await?;
    } else {
        runner::send_images(
            h.inner().clone(),
            id,
            req.prompt.trim().to_string(),
            req.images.clone(),
            false,
        )
        .await?;
    }
    Ok(summary)
}

#[tauri::command]
async fn task_send(
    h: H<'_>,
    id: String,
    text: String,
    later: Option<bool>,
    images: Option<Vec<String>>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    if let Some(images) = images.filter(|i| !i.is_empty()) {
        return runner::send_images(h.inner().clone(), id, text, images, later.unwrap_or(false))
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
    let rt = h.runtime(&id);
    let tx = rt.pending.lock().await.remove(&item_id);
    match tx {
        Some(tx) => {
            // Answering is the user acting on this chat, so it comes to the top.
            h.touch(&id).await;
            let _ = tx.send(response);
            Ok(())
        }
        None => Err("That request is no longer pending.".into()),
    }
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
        }
    }
    if let Some(v) = patch["pinned"].as_bool() {
        t.pinned = v;
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

/// Switch one sub-agent's model; it takes effect on the sub-agent's next request.
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

/// Change one sub-agent's reasoning effort (None = back to its default); next request.
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

#[tauri::command]
async fn stats_reset(h: H<'_>) -> Result<(), String> {
    h.stats.reset();
    Ok(())
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

#[tauri::command]
async fn draft_get(key: String) -> Result<String, String> {
    Ok(store::load_draft(&key))
}

#[tauri::command]
async fn draft_set(key: String, text: String) -> Result<(), String> {
    store::save_draft(&key, &text);
    Ok(())
}

/// Type into a sub-agent's panel.
#[tauri::command]
async fn sub_send(
    h: H<'_>,
    id: String,
    sub_id: String,
    text: String,
    images: Option<Vec<String>>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    let images = images.unwrap_or_default();
    if text.is_empty() && images.is_empty() {
        return Ok(());
    }
    runner::sub_message(h.inner(), &id, &sub_id, text, images).await
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
#[tauri::command]
async fn force_pause_all(h: H<'_>) -> Result<(), String> {
    pause_all(h.clone()).await?;
    for t in summaries(&h).await {
        if t.busy > 0 {
            h.force_pause(&t.id).await;
        }
    }
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
    // Resuming is the user picking this chat back up, so it comes to the top.
    // (A global "resume all" is deliberately not this command, and must not
    // reorder the list — every chat would jump at once.)
    h.touch(&id).await;
    let res = match mode.as_deref() {
        Some(m @ ("continue" | "wrap")) if message.is_none() => {
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
        "Sent to the main agent. It has no sub-agents working right now.".to_string()
    } else {
        format!("Sent to the main agent and {n} working subagent{} ({}). Each one applies it only if it's about their own work.", if n == 1 { "" } else { "s" }, to[1..].join(", "))
    })
}

/// Freeze every agent in every task (e.g. before shutting the PC down).
///
/// The order here is what the pause banner is made of, so it is the order that
/// matters. `busy` is not persisted and reads 0 for every chat until something
/// recounts it, and the banner says "Pausing… waiting on N commands" only while
/// `busy` is above zero — so the counts have to be published *before* `paused_all`
/// flips. Setting the flag first (as this did) opened a window of a second or two
/// where the UI had the flag but no counts, and it rendered that as the flat
/// "Everything is paused": the one wording that reads as "nothing is happening,
/// quit the app". The per-chat `unpaused` reset rides along ahead of the flag for
/// the same reason — otherwise a chat that opted out earlier stays opted out and
/// carries on under a pause the user believes is global.
#[tauri::command]
async fn pause_all(h: H<'_>) -> Result<(), String> {
    let refs: Vec<_> = h.tasks.read().await.values().cloned().collect();
    for t in &refs {
        let id = t.lock().await.id.clone();
        {
            let mut g = t.lock().await;
            g.unpaused = false;
            h.emit_summary(&g);
        }
        // The reset is user-visible state (`is_paused` reads `unpaused`), and it was
        // in memory only: a restart brought back a stale `unpaused: true` and ran a
        // chat the user had just frozen.
        h.save_task(&id).await;
    }
    for t in summaries(&h).await {
        h.refresh_busy(&t.id).await;
    }
    let s = {
        let mut s = h.settings.write().await;
        s.paused_all = true;
        s.paused_reason = "Paused everything".into();
        store::save_settings(&s);
        s.clone()
    };
    (h.bus)(
        "ol://settings",
        serde_json::to_value(public_settings(&s)).unwrap_or_default(),
    );
    h.pause_bell.notify_waiters();
    Ok(())
}

/// Lift the global pause and every per-task pause; restart tasks the app closed on.
#[tauri::command]
async fn resume_all(h: H<'_>, message: Option<String>) -> Result<(), String> {
    let was_paused_all = {
        let mut s = h.settings.write().await;
        let was_paused_all = s.paused_all;
        s.paused_all = false;
        s.paused_reason.clear();
        store::save_settings(&s);
        // As in pause_all: the UI only learns the global pause moved from here.
        let out = serde_json::to_value(public_settings(&s)).unwrap_or_default();
        drop(s);
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
    mut a: agent::store::Account,
) -> Result<Vec<accounts::AccountView>, String> {
    // Setup tokens can't be refreshed, so prove they work before saving (no
    // silent duds). The same rule covers an OpenCode Go key: it is a bare
    // credential with nothing else to validate it against, and a typo would
    // otherwise only surface as every chat failing later.
    if a.kind == "claude" && a.refresh_token.is_empty() {
        accounts::claude_probe(h, &a).await?;
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

const EXPORT_FORMAT: &str = "openleash-chats";

/// Chats as a portable JSON file (full history, timeline, sub-agent transcripts).
fn export_json(tasks: Vec<Task>) -> Result<String, String> {
    serde_json::to_string_pretty(&json!({"format": EXPORT_FORMAT, "version": 1, "exported_at": chrono::Utc::now(), "tasks": tasks})).map_err(|e| e.to_string())
}

/// Put many chats away at once. Returns how many were actually archived.
///
/// The bulk half of `task_update {archived: true}`. It walks the ids rather than
/// the other way round, so a stale tick can't archive a chat the user has since
/// restored, and a chat that is already archived isn't counted as work done.
/// Pinning is dropped on the way through, exactly as the single-chat patch does —
/// an archived chat is out of the sidebar, so a pin on it is a pin on nothing.
///
/// Nothing here stops a running agent: archiving hides the chat, it does not
/// cancel it, and the transcript stays where it was. What does stop is the
/// *global* model swap, which skips archived chats — so a bulk archive mid-swap
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
    std::fs::write(&path, export_json(out)?).map_err(|e| format!("Couldn't write {path}: {e}"))?;
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
    // sub-agent history, and a chat whose body is still on disk would export as
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
async fn tasks_import(h: H<'_>, path: String) -> Result<Vec<TaskSummary>, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("Couldn't read {path}: {e}"))?;
    let v: Value =
        serde_json::from_str(&text).map_err(|_| "That file isn't valid JSON.".to_string())?;
    let list = if v["format"] == EXPORT_FORMAT {
        v["tasks"].clone()
    } else if v.is_array() {
        v
    } else {
        json!([v])
    };
    let tasks: Vec<Task> =
        serde_json::from_value(list).map_err(|e| format!("Not an OpenLeash chat export: {e}"))?;
    if tasks.is_empty() {
        return Err("No chats in that file.".into());
    }
    let mut out = vec![];
    for t in tasks {
        let t = clone_as_new(t);
        store::save_task(&t);
        out.push(t.summary());
        h.tasks
            .write()
            .await
            .insert(t.id.clone(), Arc::new(Mutex::new(t)));
    }
    Ok(out)
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
    h.bg.kill_task(id);
    let task = h.tasks.blocking_read().get(id).cloned();
    if let Some(task) = task {
        let (worktree, project, cwd) = {
            let task = task.blocking_lock();
            (task.worktree, task.project.clone(), task.cwd.clone())
        };
        if remove_worktree && worktree {
            // Do not erase the chat's recovery information until Git confirms
            // removal. The caller can retry or inspect the worktree on failure.
            git::remove_worktree(&project, &cwd)?;
        }
    }
    h.tasks.blocking_write().remove(id);
    // Safe now and only now: the tokens are cancelled and the background
    // processes are dead, so nothing can still be holding this chat's runtime.
    h.forget_runtime(id);
    store::delete_task(id);
    store::save_draft(id, "");
    // The shadow store holds a full copy of that chat's files; leaving it would
    // keep a deleted chat's contents on disk indefinitely.
    checkpoint::purge(id);
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
    for id in &ids {
        // This cleanup has no interactive confirmation; preserve the files by
        // detaching the chat but leaving its worktree registered on disk.
        if let Err(error) = purge_task(h, id, false) {
            eprintln!("[openleash] could not delete archived chat {id}: {error}");
        }
    }
    Ok(ids.len())
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
    let t = h.task(&id).await?;
    let (chat, cwd, k, text) = {
        let t = t.lock().await;
        let idx = t
            .items
            .iter()
            .position(|i| i.id == item_id)
            .ok_or("message not found")?;
        let k = t
            .checkpoints
            .iter()
            .position(|c| c.item_index == idx)
            .ok_or("Can't rewind to this message (it was sent mid-turn or before a compaction).")?;
        (t.id.clone(), t.cwd.clone(), k, t.items[idx].text.clone())
    };
    // The snapshot is read before the truncation below, because the truncation
    // is what drops the checkpoint this is asking about.
    let target = checkpoint::target(&chat, k);
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
            let _ = tokio::task::spawn_blocking(move || snap.user().take()).await;
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
    src.items.truncate(idx);
    src.messages.truncate(msg_index);
    src.checkpoints.truncate(k);
    src.read_files.clear();
    src.todos.clear();
    src.sub_msgs.clear();
    src.status = "idle".into();
    src.paused = None;
    src.unpaused = true;
    src.stop_note = None;
    src.wrap_up = false;
    src.told.clear();
    src.usage.last_context = 0;
    let title = src.title.clone();
    let mut f = clone_as_new(src);
    f.forked_from = Some(id.clone());
    f.hidden = false;
    f.pinned = false;
    f.order = 0.0;
    f.worktree = true;
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

/// The rewind itself, on an already-locked task. Conversation only: the
/// working tree is deliberately left as it is.
fn rewind_task(t: &mut Task, item_id: String) -> Result<String, String> {
    let idx = t
        .items
        .iter()
        .position(|i| i.id == item_id)
        .ok_or("message not found")?;
    let k = t
        .checkpoints
        .iter()
        .position(|c| c.item_index == idx)
        .ok_or("Can't rewind to this message (it was sent mid-turn or before a compaction).")?;
    let text = t.items[idx].text.clone();
    let msg_index = t.checkpoints[k].msg_index;
    t.items.truncate(idx);
    t.messages.truncate(msg_index);
    t.checkpoints.truncate(k);
    t.read_files.clear();
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
    // `told` records what the model has already been told about (assist mode,
    // subagent set, memory), so those get announced as changes. It
    // survives a rewind deliberately for settings, which outlive any branch — but
    // after a rewind the branch is new, so the announcements are owed again.
    t.told.clear();
    // A sub-agent's saved transcript continues a `task` tool call whose parent
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
    t.step = "Rewound · files kept".into();
    Ok(text)
}

#[tauri::command]
async fn task_review(h: H<'_>, id: String) -> Result<Value, String> {
    let t = h.task(&id).await?;
    let (cwd, base, touched) = {
        let t = t.lock().await;
        (t.cwd.clone(), t.base_commit.clone(), t.touched.clone())
    };
    if let Some(base) = base.filter(|_| git::is_repo(&cwd)) {
        let files = git::review(&cwd, &base)?;
        return Ok(json!({"git": true, "files": files}));
    }
    // No git: diff against the snapshots taken before the agent's first edit.
    let mut files = vec![];
    for (path, original) in touched {
        let now = std::fs::read_to_string(&path).ok();
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
        files.push(json!({"path": agent::permissions::short(&path, &cwd), "abs": path, "status": status, "add": add, "del": del, "lines": lines}));
    }
    Ok(json!({"git": false, "files": files}))
}

#[tauri::command]
async fn task_revert_file(h: H<'_>, id: String, path: String) -> Result<(), String> {
    let t = h.task(&id).await?;
    let t = t.lock().await;
    if let Some(base) = t.base_commit.clone().filter(|_| git::is_repo(&t.cwd)) {
        return git::revert_file(&t.cwd, &base, &path);
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
async fn settings_update(h: H<'_>, patch: Value) -> Result<Settings, String> {
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
    let mcp_stored = s.mcp.clone();
    let mut next: Settings = serde_json::from_value(cur).map_err(|e| e.to_string())?;
    next.providers = providers;
    next.spend = spend;
    next.accounts = accts;
    // The UI never sees the token; it's set through plugin_github_token.
    next.plugins.github.token = gh_token;
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
    let trust_prefixes = changed_trust_prefixes(&s.trust, &next.trust);
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
    invalidate_trust_prefixes(&h, &trust_prefixes).await;
    if mcp_changed {
        let h2 = h.inner().clone();
        tauri::async_runtime::spawn(async move { h2.mcp.sync(&cfgs).await });
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
    let computer = tauri::async_runtime::spawn_blocking(|| match xcap::Monitor::all() {
        Ok(ms) => {
            let list: Vec<Value> = ms.iter().map(|m| json!({"name": m.name().unwrap_or_default(), "w": m.width().unwrap_or(0), "h": m.height().unwrap_or(0), "primary": m.is_primary().unwrap_or(false)})).collect();
            json!({"ok": !list.is_empty(), "monitors": list})
        }
        Err(e) => json!({"ok": false, "error": e.to_string(), "monitors": []}),
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
        json!({"github": github, "computer": computer, "browser": browser, "has_token": !cfg.github.token.is_empty(), "can_login": agent::plugins::gh_client_id(&cfg.github).is_some()}),
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

/// Chats that count as "in use" for a global model swap: live or paused.
fn swap_active(t: &Task) -> bool {
    !t.archived && (t.status == "running" || t.status == "waiting" || t.paused.is_some())
}

/// Where each model is used in one chat: main agent, its subagents, subagents it may spawn.
///
/// One row per *model*, not per (model, effort): a swap moves everything on that
/// model onto one destination with one reasoning level, so two agents on one model
/// at different levels are not two independent choices. The level reported is the
/// first use recorded — the main agent's, since it is added first — which is the
/// one a swap of that model rewrites.
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
        // What it is actually on, not what is stored on it: a sub-agent with no
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
    let mut order: Vec<String> = vec![];
    // One row per model. A swap moves everything on a model to one destination and
    // one reasoning level, so agents on one model at different levels are not
    // independent choices — and the map the dialog sends is keyed by model, so a
    // second row for the same model would have nowhere to go.
    let mut uses: HashMap<String, Vec<(String, u32)>> = HashMap::new();
    // The level this model is on, kept from the first use recorded. The main agent
    // is added first, so a model's row reports the chat's level for it — which is
    // the one `swap_task` rewrites.
    let mut efforts: HashMap<String, usize> = HashMap::new();
    let mut add = |m: String, label: String, effort: usize| {
        if m.is_empty() {
            return;
        }
        if !order.contains(&m) {
            order.push(m.clone());
            efforts.insert(m.clone(), effort);
        }
        let l = uses.entry(m).or_default();
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
            add(s.model.clone(), "default for new chats".into(), s.effort);
            for d in s.agents.iter().filter(|d| !d.model.is_empty()) {
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
        .map(|m| {
            let effort = efforts.get(&m).copied().unwrap_or(0);
            let u: Vec<String> = uses
                .remove(&m)
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
    // Only a model somebody *chose* is stored on a sub-agent, so this is the one
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
    // Carry the swap into the defaults later sub-agents are launched from — the
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
    // harmless record: a second swap (B -> C) left `A -> B` behind, so a sub-agent
    // launched from a default still naming A ended up on B — a model the chat had
    // already been moved off two swaps ago. So an entry whose destination this
    // swap moves is carried along to the new destination, and an entry whose key
    // this swap names is dropped first: the new pair about to be inserted already
    // says where that id goes, and a no-op swap is not worth a record.
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
) -> Result<Settings, String> {
    let spread = spread.unwrap_or(false);
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
    if !map.is_empty() || !effort.is_empty() {
        for tid in &ids {
            h.update_task(tid, |t| swap_task(t, &map, &effort, spread))
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

#[tauri::command]
async fn project_info(h: H<'_>, path: String) -> Result<Value, String> {
    let is_git = git::is_repo(&path);
    let inject_global_claude = h.settings.read().await.inject_global_claude;
    Ok(json!({
        "exists": std::path::Path::new(&path).is_dir(),
        "git": is_git,
        "branch": if is_git { git::current_branch(&path) } else { String::new() },
        "branches": if is_git { git::branches(&path) } else { vec![] },
        // What actually reaches the prompt, not the raw inventory: for an
        // untrusted folder this is the globals alone.
        "memory": agent::prompt::memory_for_prompt(&path, inject_global_claude).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "trusted": agent::trust::trusted_now(&path),
        "decided": agent::trust::decided(&agent::trust::current(), &path),
    }))
}

/// What a folder would inject, whether or not it is trusted, for the trust
/// dialog and the manifest panel. Computed live so the answer is what the next
/// prompt would actually contain.
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
        if !s.paused_all {
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
                                .any(|a| h.accts.usable(a))
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
    .title("OpenLeash is controlling your PC")
    .inner_size(420.0, 62.0)
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
            use tauri::Emitter;
            let _ = handle.emit(TOAST_CLICK, json!({ "task_id": task }));
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
        .setup(|app| {
            // Before anything can emit a toast: an unregistered AppUserModelID
            // is dropped by the Windows shell without an error, so this has to
            // happen on the way in rather than lazily on the first notification.
            // Best-effort and never fatal — see `win::register`.
            register_toast_identity(app);
            let settings = store::load_settings();
            sync_model_state(&settings);
            router::set_routes(&settings.routes);
            // Publish the trust decisions to the process-global table the prompt
            // path reads. Done here because the prompt is built from an `Env`
            // that carries no settings — see `agent::trust::set_trust`.
            agent::trust::set_trust(&settings.trust);
            let mcp_cfgs = settings.mcp.clone();
            let mut tasks = HashMap::new();
            for t in store::load_tasks() {
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
            // The PC-control guard needs the harness (to tell the agent about
            // an objection) and its own always-on-top window.
            agent::pcguard::set_harness(h);
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
            visualization::visualization_publish,
            visualization::visualization_read,
            visualization::visualization_release,
            artifact_list,
            artifact_get,
            artifact_create,
            artifact_revise,
            artifact_annotate,
            artifact_feedback_submit,
            artifact_feedback_list,
            artifact_feedback_respond,
            app_boot,
            tasks_list,
            task_get,
            sub_report,
            task_create,
            task_send,
            task_interrupt,
            pcguard_state,
            pcguard_stop,
            task_respond,
            task_answer_nonblocking,
            task_dismiss_notice,
            task_update,
            task_delete,
            tasks_delete_archived,
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
            task_revert_file,
            task_commit,
            task_commit_message,
            bg_kill,
            sub_set_model,
            sub_set_effort,
            sub_send,
            draft_get,
            stats_get,
            stats_reset,
            stats_chat,
            draft_set,
            task_pause,
            task_status_summary,
            open_path,
            files_attach,
            file_data_url,
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
            accounts_refresh,
            agents_list,
            skills_list,
            skill_import,
            skill_remove,
            skill_set_enabled,
            skill_read,
            pool_models_refresh,
            task_export,
            tasks_export_all,
            tasks_import,
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
            provider_remove,
            model_save,
            model_remove,
            model_set_enabled,
            provider_models,
            mcp_oauth_start,
            mcp_oauth_wait,
            mcp_oauth_cancel,
            mcp_oauth_disconnect,
            mcp_status,
            mcp_reconnect,
            plugins_status,
            browser_panel,
            plugin_github_token,
            github_login_start,
            github_login_wait,
            github_login_cancel,
            models_in_use,
            models_swap,
            project_info,
            trust_manifest,
            trust_set,
            trust_clear,
            usage_get,
            toast_show
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::runner;
    use crate::agent::store::Route;
    use crate::agent::QueuedMsg;

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

        let mut s = Settings::default();
        s.project = project.clone();
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
        let mk = || {
            let mut s = Settings::default();
            s.model = "gpt-sol".into();
            s.effort = 2;
            s.agents = vec![AgentDef {
                id: "reviewer".into(),
                name: "Reviewer".into(),
                model: "gpt-sol".into(),
                ..Default::default()
            }];
            s
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
    /// followed by `B -> C` used to keep `A -> B`, so a sub-agent launched from a
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

    /// A sub-agent with no model of its own follows the chat, so a swap reaches the
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
            "the live sub-agent follows the chat onto the new model"
        );
    }

    /// Reasoning moves with the model, everywhere the model moves: the chat, a
    /// sub-agent that had a level of its own, and an X layer that names its own.
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
            "a sub-agent with a level of its own has it swapped too"
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
    /// to be the one the request carries: a sub-agent's own override, its type's,
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
    /// holds for the main agent — but not for a sub-agent, which has no turn
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
            "a sub-agent follows the chat onto the queued swap"
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

        runner::send_images(
            h.clone(),
            "q".into(),
            "what is this?".into(),
            vec![PIXEL.into()],
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

    /// Same for the list: an Alt-Enter photo is queued whole, so sending it later
    /// hands over the image too rather than a message about a missing picture.
    #[tokio::test]
    async fn a_queued_photo_is_still_a_photo_when_it_goes_out() {
        let dir = std::env::temp_dir().join(format!("openleash-qimg-{}", std::process::id()));
        let _home = store::test_home(&dir);
        let h = Arc::new(harness(vec![running_task()]));
        as_running(&h);

        runner::send_images(
            h.clone(),
            "q".into(),
            "look at this".into(),
            vec![PIXEL.into()],
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

    /// Sub-agent transcripts continue a `task` tool call. `resume_subs` runs at
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
            "a saved sub-agent continues a tool call the rewind discarded"
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
        let mut settings = Settings::default();
        settings.trust = vec![trust_decision(
            "/work",
            agent::trust::TrustKind::Parent,
            agent::trust::TrustState::Trusted,
        )];
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
    #[tokio::test]
    async fn browser_panel_rejects_unknown_tasks_and_disabled_plugin_before_launch() {
        let h = harness(vec![chat("browser-test")]);
        let unknown =
            browser_panel_for(&h, "missing", agent::browser::PanelAction::Snapshot {}).await;
        assert!(unknown.is_err());
        let disabled =
            browser_panel_for(&h, "browser-test", agent::browser::PanelAction::Snapshot {}).await;
        assert!(disabled.err().unwrap().contains("turned off"));
    }

    fn harness(tasks: Vec<Task>) -> Harness {
        Harness {
            bus: Arc::new(|_, _| {}),
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
            "items": [], "messages": [], "todos": [], "subs": [],
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0, "last_context": 0},
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap();
        // `hydrated` is `#[serde(skip)]`, so deserialising a literal leaves it
        // false -- and `save_task` refuses to write an unhydrated stub rather than
        // overwrite real sub-agent history with two empty maps. These fixtures are
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
        let mut already = chat("already");
        already.archived = true;
        let mut alive = chat("alive");
        alive.archived = true;
        let h = harness(vec![pinned, already, alive]);

        assert_eq!(
            archive_chats(&h, &["pinned".into(), "already".into(), "ghost".into()])
                .await
                .unwrap(),
            1,
            "only the pinned chat was live to archive; a vanished id is not work done"
        );
        assert_eq!(
            archived_ids(&h).await,
            vec![
                "alive".to_string(),
                "already".to_string(),
                "pinned".to_string()
            ]
        );
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
}
