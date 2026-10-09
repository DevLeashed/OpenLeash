//! Persistent, versioned project artifacts and explicit user feedback handoff.
//!
//! Artifacts live under the task's canonical project root, not its cwd (which
//! can be a worktree outside the project). Project files are repository-owned
//! data, not trusted instructions or a source of authority. Agent-facing reads
//! deliberately omit annotations until the user explicitly submits them.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const ARTIFACTS_DIR: &str = ".openleash/artifacts";
const MAX_CONTENT: usize = 256 * 1024;
const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 128;
const MAX_VERSIONS: usize = 32;
const MAX_ANNOTATIONS: usize = 256;
const MAX_FEEDBACK: usize = 128;
const MAX_JSON_STATE: usize = 16 * 1024;
static ARTIFACTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRef {
    pub path: String,
    #[serde(default, alias = "start_line")]
    pub start_line: Option<u32>,
    #[serde(default, alias = "end_line")]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactInput {
    pub title: String,
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default, alias = "code_refs")]
    pub code_refs: Vec<CodeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactVersion {
    pub id: String,
    pub parent_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub content: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub code_refs: Vec<CodeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub id: String,
    pub version_id: String,
    pub text: String,
    #[serde(default)]
    pub anchor: Option<Value>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub submitted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feedback {
    pub id: String,
    pub version_id: String,
    pub text: String,
    #[serde(default)]
    pub annotation_ids: Vec<String>,
    #[serde(default)]
    pub interactive_state: Option<Value>,
    pub created_at: DateTime<Utc>,
    /// `submitted` until an agent records one response; then `responded`.
    pub status: String,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub response_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub current_version_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub versions: Vec<ArtifactVersion>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub feedback: Vec<Feedback>,
}

/// The compact list shape used by the workspace UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSummary {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub current_version_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Agent view of an artifact: private, unsent annotations never leave the UI
/// boundary. Contents and submitted collaboration data remain untrusted input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentArtifact {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub current_version_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub versions: Vec<ArtifactVersion>,
    pub submitted_annotations: Vec<Annotation>,
    pub feedback: Vec<Feedback>,
}

impl From<&Artifact> for ArtifactSummary {
    fn from(a: &Artifact) -> Self {
        Self {
            id: a.id.clone(),
            title: a.title.clone(),
            kind: a.kind.clone(),
            current_version_id: a.current_version_id.clone(),
            created_at: a.created_at,
            updated_at: a.updated_at,
        }
    }
}

impl From<&Artifact> for AgentArtifact {
    fn from(a: &Artifact) -> Self {
        Self {
            id: a.id.clone(),
            title: a.title.clone(),
            kind: a.kind.clone(),
            current_version_id: a.current_version_id.clone(),
            created_at: a.created_at,
            updated_at: a.updated_at,
            versions: a.versions.clone(),
            submitted_annotations: a
                .annotations
                .iter()
                .filter(|x| x.submitted)
                .cloned()
                .collect(),
            // Every feedback record is created only by the explicit user
            // submission handoff. Keep responded records so the agent can see
            // the status of feedback it just handled; `feedback_list_for_agent`
            // separately filters pending records for `artifact_respond`.
            feedback: a.feedback.clone(),
        }
    }
}

fn artifact_root(project: &str, create: bool) -> Result<PathBuf, String> {
    let raw = Path::new(project.trim());
    if project.trim().is_empty() {
        return Err("This task has no project folder for artifacts.".into());
    }
    let root = fs::canonicalize(raw).map_err(|e| format!("Cannot resolve task project: {e}"))?;
    if !root.is_dir() {
        return Err("The task project folder is not a directory.".into());
    }
    let mut parent = root.clone();
    for component in [".openleash", "artifacts"] {
        parent.push(component);
        match fs::symlink_metadata(&parent) {
            Ok(md) => {
                if md.file_type().is_symlink() || !md.is_dir() {
                    return Err(format!(
                        "Refusing unsafe artifact directory {}",
                        parent.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&parent)
                    .map_err(|e| format!("Cannot create artifact directory: {e}"))?;
                let md = fs::symlink_metadata(&parent)
                    .map_err(|e| format!("Cannot inspect artifact directory: {e}"))?;
                if md.file_type().is_symlink() || !md.is_dir() {
                    return Err(format!(
                        "Refusing unsafe artifact directory {}",
                        parent.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(root.join(ARTIFACTS_DIR));
            }
            Err(e) => return Err(format!("Cannot inspect artifact directory: {e}")),
        }
        let canonical = fs::canonicalize(&parent)
            .map_err(|e| format!("Cannot resolve artifact directory: {e}"))?;
        if !canonical.starts_with(&root) {
            return Err("Artifact directory resolves outside the task project.".into());
        }
        parent = canonical;
    }
    Ok(parent)
}

fn valid_id(id: &str) -> Result<String, String> {
    let parsed = Uuid::parse_str(id).map_err(|_| "Invalid artifact id.".to_string())?;
    if parsed.to_string() != id {
        return Err("Invalid artifact id.".into());
    }
    Ok(id.to_string())
}

fn file_for(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let id = valid_id(id)?;
    Ok(dir.join(format!("{id}.json")))
}

fn safe_read(path: &Path) -> Result<Artifact, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("Cannot inspect artifact: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Refusing to follow an unsafe artifact file.".into());
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err("Artifact file exceeds the size limit.".into());
    }
    let parent = path.parent().ok_or("Invalid artifact path")?;
    let canonical_parent = fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let canonical_path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if canonical_path.parent() != Some(canonical_parent.as_path()) {
        return Err("Artifact file resolves outside the artifact directory.".into());
    }
    let data = fs::read(path).map_err(|e| format!("Cannot read artifact: {e}"))?;
    if data.len() > MAX_ARTIFACT_BYTES {
        return Err("Artifact file exceeds the size limit.".into());
    }
    let artifact: Artifact =
        serde_json::from_slice(&data).map_err(|e| format!("Artifact file is invalid: {e}"))?;
    validate_artifact(&artifact)?;
    Ok(artifact)
}

fn read_all(dir: &Path) -> Result<Vec<(PathBuf, Artifact, u64)>, String> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("Cannot list artifacts: {e}")),
    };
    let mut out = Vec::new();
    let mut total = 0u64;
    for entry in rd {
        let entry = entry.map_err(|e| format!("Cannot list artifacts: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else {
            continue;
        };
        if valid_id(stem).is_err() {
            continue;
        }
        let artifact = safe_read(&path)?;
        if artifact.id != stem {
            return Err("Artifact filename does not match its id.".into());
        }
        let size = entry
            .metadata()
            .map_err(|e| format!("Cannot inspect artifact: {e}"))?
            .len();
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err("Project artifact storage exceeds the size limit.".into());
        }
        out.push((path, artifact, size));
        if out.len() > MAX_ARTIFACTS {
            return Err("Project contains too many artifacts.".into());
        }
    }
    Ok(out)
}

fn save(
    dir: &Path,
    artifact: &Artifact,
    previous_size: u64,
    current_total: u64,
) -> Result<(), String> {
    validate_artifact(artifact)?;
    let path = file_for(dir, &artifact.id)?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("Refusing to overwrite an unsafe artifact file.".into());
        }
    }
    let content = serde_json::to_string_pretty(artifact).map_err(|e| e.to_string())?;
    if content.len() > MAX_ARTIFACT_BYTES {
        return Err("Artifact history exceeds the size limit.".into());
    }
    let total = current_total
        .saturating_sub(previous_size)
        .saturating_add(content.len() as u64);
    if total > MAX_TOTAL_BYTES {
        return Err("Project artifact storage exceeds the size limit.".into());
    }
    crate::agent::store::try_write_atomic(&path, &content)
        .map_err(|e| format!("Cannot save artifact: {e}"))
}

fn find_artifact<'a>(
    all: &'a [(PathBuf, Artifact, u64)],
    id: &str,
) -> Result<&'a (PathBuf, Artifact, u64), String> {
    all.iter()
        .find(|(_, a, _)| a.id == id)
        .ok_or_else(|| format!("No artifact `{id}` in this task's project."))
}

fn bounded_string(value: &str, name: &str, max: usize, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > max {
        return Err(format!(
            "Invalid {name} (must be {}{} bytes or less).",
            max,
            if allow_empty { "" } else { " and non-empty" }
        ));
    }
    Ok(())
}

fn safe_ref_path(path: &str) -> bool {
    if path.contains(':')
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.starts_with("//")
    {
        return false;
    }
    !path
        .replace('\\', "/")
        .split('/')
        .any(|part| part == ".." || part.is_empty())
}

fn validate_anchor(anchor: &Option<Value>) -> Result<(), String> {
    let Some(anchor) = anchor else { return Ok(()) };
    let object = anchor
        .as_object()
        .ok_or("Annotation anchor must be an object.")?;
    if object
        .keys()
        .any(|key| key != "selector" && key != "target")
    {
        return Err("Annotation anchor supports only selector and target.".into());
    }
    for (key, value) in object {
        let text = value
            .as_str()
            .ok_or_else(|| format!("Annotation anchor {key} must be text."))?;
        bounded_string(text, "annotation anchor text", 2_000, true)?;
    }
    Ok(())
}

fn validate_input(input: &ArtifactInput) -> Result<(), String> {
    bounded_string(&input.title, "title", 200, false)?;
    if !matches!(input.kind.as_str(), "html" | "svg" | "json" | "markdown") {
        return Err("Artifact kind must be html, svg, json, or markdown.".into());
    }
    bounded_string(&input.content, "content", MAX_CONTENT, false)?;
    if input.decisions.len() > 64 || input.constraints.len() > 64 || input.code_refs.len() > 128 {
        return Err("Artifact metadata exceeds the item limit.".into());
    }
    for s in input.decisions.iter().chain(input.constraints.iter()) {
        bounded_string(s, "decision or constraint", 2_000, false)?;
    }
    for r in &input.code_refs {
        bounded_string(&r.path, "code reference path", 2_000, false)?;
        if !safe_ref_path(&r.path) {
            return Err("Code reference path must be relative to the project and cannot traverse outside it.".into());
        }
        bounded_string(&r.description, "code reference description", 2_000, true)?;
        if r.start_line.zip(r.end_line).is_some_and(|(s, e)| s > e) {
            return Err("Code reference start line must not follow its end line.".into());
        }
    }
    Ok(())
}

fn validate_artifact(a: &Artifact) -> Result<(), String> {
    valid_id(&a.id)?;
    bounded_string(&a.title, "title", 200, false)?;
    if !matches!(a.kind.as_str(), "html" | "svg" | "json" | "markdown") {
        return Err("Artifact kind must be html, svg, json, or markdown.".into());
    }
    if a.versions.is_empty() || a.versions.len() > MAX_VERSIONS {
        return Err("Artifact version history is empty or exceeds the revision limit.".into());
    }
    if a.annotations.len() > MAX_ANNOTATIONS || a.feedback.len() > MAX_FEEDBACK {
        return Err("Artifact collaboration history exceeds its limit.".into());
    }
    let mut version_ids = std::collections::HashSet::new();
    for (i, v) in a.versions.iter().enumerate() {
        valid_id(&v.id)?;
        if !version_ids.insert(v.id.as_str()) {
            return Err("Artifact has duplicate version ids.".into());
        }
        if (i == 0 && v.parent_id.is_some())
            || (i > 0 && v.parent_id.as_deref() != Some(a.versions[i - 1].id.as_str()))
        {
            return Err("Artifact version history has a broken parent chain.".into());
        }
        let input = ArtifactInput {
            title: a.title.clone(),
            kind: a.kind.clone(),
            content: v.content.clone(),
            decisions: v.decisions.clone(),
            constraints: v.constraints.clone(),
            code_refs: v.code_refs.clone(),
        };
        validate_input(&input)?;
    }
    if a.versions.last().map(|v| v.id.as_str()) != Some(a.current_version_id.as_str()) {
        return Err("Artifact current version does not match its history.".into());
    }
    let mut annotation_ids = std::collections::HashSet::new();
    for annotation in &a.annotations {
        valid_id(&annotation.id)?;
        if !annotation_ids.insert(annotation.id.as_str())
            || !version_ids.contains(annotation.version_id.as_str())
        {
            return Err("Artifact has an invalid annotation reference.".into());
        }
        bounded_string(&annotation.text, "annotation", 10_000, false)?;
        validate_anchor(&annotation.anchor)?;
    }
    let mut feedback_ids = std::collections::HashSet::new();
    for feedback in &a.feedback {
        valid_id(&feedback.id)?;
        if !feedback_ids.insert(feedback.id.as_str())
            || !version_ids.contains(feedback.version_id.as_str())
        {
            return Err("Artifact has an invalid feedback reference.".into());
        }
        bounded_string(&feedback.text, "feedback", 20_000, true)?;
        if !matches!(feedback.status.as_str(), "submitted" | "responded") {
            return Err("Artifact has an invalid feedback status.".into());
        }
        if feedback.annotation_ids.iter().any(|id| {
            a.annotations
                .iter()
                .find(|annotation| annotation.id == *id)
                .is_none_or(|annotation| {
                    !annotation.submitted || annotation.version_id != feedback.version_id
                })
        }) {
            return Err(
                "Feedback references an annotation that is not submitted for this version.".into(),
            );
        }
        if feedback
            .interactive_state
            .as_ref()
            .is_some_and(|v| serde_json::to_vec(v).map_or(true, |b| b.len() > MAX_JSON_STATE))
        {
            return Err("Interactive state exceeds the size limit.".into());
        }
        match feedback.status.as_str() {
            "submitted"
                if feedback.decision.is_some()
                    || feedback.response.is_some()
                    || feedback.response_at.is_some() =>
            {
                return Err("Unanswered feedback cannot have an agent response.".into())
            }
            "responded"
                if !matches!(
                    feedback.decision.as_deref(),
                    Some("addressed" | "needs_clarification")
                ) || feedback
                    .response
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                    || feedback.response_at.is_none() =>
            {
                return Err("Answered feedback is incomplete.".into())
            }
            _ => {}
        }
    }
    for annotation in a.annotations.iter().filter(|a| a.submitted) {
        if !a
            .feedback
            .iter()
            .any(|f| f.annotation_ids.iter().any(|id| id == &annotation.id))
        {
            return Err("Submitted annotation has no feedback submission.".into());
        }
    }
    Ok(())
}

fn make_version(input: ArtifactInput, parent_id: Option<String>) -> ArtifactVersion {
    ArtifactVersion {
        id: Uuid::new_v4().to_string(),
        parent_id,
        created_at: Utc::now(),
        content: input.content,
        decisions: input.decisions,
        constraints: input.constraints,
        code_refs: input.code_refs,
    }
}

pub fn list(project: &str) -> Result<Vec<ArtifactSummary>, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    let dir = artifact_root(project, false)?;
    let mut out: Vec<_> = read_all(&dir)?
        .iter()
        .map(|(_, a, _)| ArtifactSummary::from(a))
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    Ok(out)
}

pub fn get(project: &str, id: &str) -> Result<Artifact, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    let id = valid_id(id)?;
    let dir = artifact_root(project, false)?;
    let all = read_all(&dir)?;
    Ok(find_artifact(&all, &id)?.1.clone())
}

pub fn get_for_agent(project: &str, id: &str) -> Result<AgentArtifact, String> {
    get(project, id).map(|a| AgentArtifact::from(&a))
}

pub fn create(project: &str, input: ArtifactInput) -> Result<Artifact, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    validate_input(&input)?;
    let dir = artifact_root(project, true)?;
    let all = read_all(&dir)?;
    if all.len() >= MAX_ARTIFACTS {
        return Err("Project contains too many artifacts.".into());
    }
    let version = make_version(input.clone(), None);
    let artifact = Artifact {
        id: Uuid::new_v4().to_string(),
        title: input.title,
        kind: input.kind,
        current_version_id: version.id.clone(),
        created_at: version.created_at,
        updated_at: version.created_at,
        versions: vec![version],
        annotations: vec![],
        feedback: vec![],
    };
    let total = all.iter().map(|(_, _, n)| *n).sum();
    save(&dir, &artifact, 0, total)?;
    Ok(artifact)
}

/// Append an immutable version. The current-version precondition prevents a
/// stale model response from silently replacing a newer user's review target.
pub fn revise(
    project: &str,
    id: &str,
    parent_version_id: &str,
    input: ArtifactInput,
) -> Result<Artifact, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    validate_input(&input)?;
    let id = valid_id(id)?;
    let dir = artifact_root(project, true)?;
    let all = read_all(&dir)?;
    let (path, old, old_size) = find_artifact(&all, &id)?;
    if old.current_version_id != parent_version_id {
        return Err(format!("Artifact changed since this revision began (current version {}). Reload it before revising.", old.current_version_id));
    }
    if old.kind != input.kind {
        return Err("Artifact kind cannot change between revisions.".into());
    }
    if old.versions.len() >= MAX_VERSIONS {
        return Err("Artifact has reached the revision limit.".into());
    }
    let mut updated = old.clone();
    let version = make_version(input.clone(), Some(old.current_version_id.clone()));
    updated.current_version_id = version.id.clone();
    updated.title = input.title;
    updated.updated_at = version.created_at;
    updated.versions.push(version);
    let total = all.iter().map(|(_, _, n)| *n).sum();
    let _ = path;
    save(&dir, &updated, *old_size, total)?;
    Ok(updated)
}

pub fn annotate(
    project: &str,
    id: &str,
    version_id: &str,
    text: &str,
    anchor: Option<Value>,
) -> Result<Artifact, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    bounded_string(text, "annotation", 10_000, false)?;
    validate_anchor(&anchor)?;
    let id = valid_id(id)?;
    let dir = artifact_root(project, true)?;
    let all = read_all(&dir)?;
    let (path, old, old_size) = find_artifact(&all, &id)?;
    if !old.versions.iter().any(|v| v.id == version_id) {
        return Err("The selected artifact version does not exist.".into());
    }
    if old.annotations.len() >= MAX_ANNOTATIONS {
        return Err("Artifact has reached the annotation limit.".into());
    }
    let mut updated = old.clone();
    updated.annotations.push(Annotation {
        id: Uuid::new_v4().to_string(),
        version_id: version_id.to_string(),
        text: text.to_string(),
        anchor,
        created_at: Utc::now(),
        submitted: false,
    });
    updated.updated_at = Utc::now();
    let total = all.iter().map(|(_, _, n)| *n).sum();
    let _ = path;
    save(&dir, &updated, *old_size, total)?;
    Ok(updated)
}

/// This is the only operation that exposes host annotations/state to the
/// agent. It is invoked from an explicit user action in the workspace.
pub fn feedback_submit(
    project: &str,
    id: &str,
    version_id: &str,
    text: &str,
    annotation_ids: Vec<String>,
    interactive_state: Option<Value>,
) -> Result<Artifact, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    bounded_string(text, "feedback", 20_000, true)?;
    if interactive_state
        .as_ref()
        .is_some_and(|v| serde_json::to_vec(v).map_or(true, |b| b.len() > MAX_JSON_STATE))
    {
        return Err("Interactive state exceeds the size limit.".into());
    }
    if text.trim().is_empty() && annotation_ids.is_empty() && interactive_state.is_none() {
        return Err(
            "Add feedback, select an annotation, or capture interactive state before sharing."
                .into(),
        );
    }
    let id = valid_id(id)?;
    let dir = artifact_root(project, true)?;
    let all = read_all(&dir)?;
    let (path, old, old_size) = find_artifact(&all, &id)?;
    if !old.versions.iter().any(|v| v.id == version_id) {
        return Err("The selected artifact version does not exist.".into());
    }
    if old.feedback.len() >= MAX_FEEDBACK {
        return Err("Artifact has reached the feedback limit.".into());
    }
    let mut selected = std::collections::HashSet::new();
    for annotation_id in &annotation_ids {
        valid_id(annotation_id)?;
        if !selected.insert(annotation_id.as_str()) {
            return Err("An annotation was selected more than once.".into());
        }
        let annotation = old
            .annotations
            .iter()
            .find(|a| a.id == *annotation_id)
            .ok_or("Selected annotation does not belong to this artifact.")?;
        if annotation.version_id != version_id || annotation.submitted {
            return Err(
                "Selected annotation is from another version or was already submitted.".into(),
            );
        }
    }
    let mut updated = old.clone();
    for annotation in updated
        .annotations
        .iter_mut()
        .filter(|a| selected.contains(a.id.as_str()))
    {
        annotation.submitted = true;
    }
    updated.feedback.push(Feedback {
        id: Uuid::new_v4().to_string(),
        version_id: version_id.to_string(),
        text: text.to_string(),
        annotation_ids,
        interactive_state,
        created_at: Utc::now(),
        status: "submitted".into(),
        decision: None,
        response: None,
        response_at: None,
    });
    updated.updated_at = Utc::now();
    let total = all.iter().map(|(_, _, n)| *n).sum();
    let _ = path;
    save(&dir, &updated, *old_size, total)?;
    Ok(updated)
}

pub fn feedback_list(project: &str, artifact_id: Option<&str>) -> Result<Vec<Feedback>, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    let dir = artifact_root(project, false)?;
    let all = read_all(&dir)?;
    let wanted = artifact_id.map(valid_id).transpose()?;
    let mut out = Vec::new();
    for (_, artifact, _) in all {
        if wanted.as_deref().is_some_and(|id| id != artifact.id) {
            continue;
        }
        out.extend(artifact.feedback);
    }
    out.sort_by_key(|f| f.created_at);
    Ok(out)
}

pub fn feedback_list_for_agent(
    project: &str,
    artifact_id: Option<&str>,
) -> Result<Vec<Feedback>, String> {
    // Only user-submitted feedback is available to tools. The full history is
    // still available to the UI, and prior responses remain in the transcript.
    Ok(feedback_list(project, artifact_id)?
        .into_iter()
        .filter(|f| f.status == "submitted")
        .collect())
}

/// Record one agent response against a submitted feedback item. Responses are
/// single-assignment history, not mutable status that can rewrite prior turns.
pub fn feedback_respond(
    project: &str,
    feedback_id: &str,
    decision: &str,
    response: &str,
) -> Result<Artifact, String> {
    let _guard = ARTIFACTS_LOCK
        .lock()
        .map_err(|_| "Artifact store is unavailable")?;
    valid_id(feedback_id)?;
    if !matches!(decision, "addressed" | "needs_clarification") {
        return Err("Decision must be addressed or needs_clarification.".into());
    }
    bounded_string(response, "response", 10_000, false)?;
    let dir = artifact_root(project, true)?;
    let all = read_all(&dir)?;
    let (path, old, old_size) = all
        .iter()
        .find(|(_, a, _)| a.feedback.iter().any(|f| f.id == feedback_id))
        .ok_or_else(|| format!("No submitted feedback `{feedback_id}` in this task's project."))?;
    let Some(feedback) = old.feedback.iter().find(|f| f.id == feedback_id) else {
        unreachable!()
    };
    if feedback.status != "submitted" {
        return Err("That feedback already has an agent response.".into());
    }
    let mut updated = old.clone();
    let feedback = updated
        .feedback
        .iter_mut()
        .find(|f| f.id == feedback_id)
        .unwrap();
    feedback.status = "responded".into();
    feedback.decision = Some(decision.into());
    feedback.response = Some(response.to_string());
    feedback.response_at = Some(Utc::now());
    updated.updated_at = Utc::now();
    let total = all.iter().map(|(_, _, n)| *n).sum();
    let _ = path;
    save(&dir, &updated, *old_size, total)?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    fn project_dir(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("openleash-artifact-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn input(content: &str) -> ArtifactInput {
        ArtifactInput {
            title: "Test design".into(),
            kind: "html".into(),
            content: content.into(),
            decisions: vec!["Use a compact status panel".into()],
            constraints: vec!["No network access".into()],
            code_refs: vec![CodeRef {
                path: "src/main.rs".into(),
                start_line: Some(2),
                end_line: Some(4),
                description: "Status model".into(),
            }],
        }
    }

    fn temp_home(name: &str) -> (PathBuf, MutexGuard<'static, ()>) {
        let dir =
            std::env::temp_dir().join(format!("openleash-artifact-home-{name}-{}", Uuid::new_v4()));
        let guard = crate::agent::store::test_home(&dir);
        (dir, guard)
    }

    #[test]
    fn artifact_agent_tool_snake_case_input_keeps_code_reference_metadata() {
        let parsed: ArtifactInput = serde_json::from_value(serde_json::json!({
            "title": "Flow", "kind": "markdown", "content": "# Flow",
            "code_refs": [{
                "path": "src/main.rs", "start_line": 3, "end_line": 7,
                "description": "Entry point"
            }]
        }))
        .unwrap();
        assert_eq!(parsed.code_refs.len(), 1);
        assert_eq!(parsed.code_refs[0].start_line, Some(3));
        assert_eq!(parsed.code_refs[0].end_line, Some(7));
        assert_eq!(parsed.code_refs[0].description, "Entry point");

        let camel: ArtifactInput = serde_json::from_value(serde_json::json!({
            "title": "Flow", "kind": "markdown", "content": "# Flow",
            "codeRefs": [{
                "path": "src/main.rs", "startLine": 4, "endLine": 8,
                "description": "Camel case remains accepted"
            }]
        }))
        .unwrap();
        assert_eq!(camel.code_refs[0].start_line, Some(4));
    }

    #[test]
    fn artifact_round_trip_uses_stable_camel_case_contract() {
        let artifact = Artifact {
            id: Uuid::new_v4().to_string(),
            title: "Design".into(),
            kind: "html".into(),
            current_version_id: Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            versions: vec![],
            annotations: vec![],
            feedback: vec![],
        };
        let wire = serde_json::to_value(&artifact).unwrap();
        assert!(wire.get("currentVersionId").is_some());
        assert!(wire.get("createdAt").is_some());
        assert!(wire.get("current_version_id").is_none());
    }

    #[test]
    fn persistent_revisions_and_project_scoped_artifacts_round_trip() {
        let (_home, _guard) = temp_home("revisions");
        let root = project_dir("revisions");
        let project = root.to_string_lossy();
        let first = create(&project, input("<h1>First</h1>")).unwrap();
        let second = revise(
            &project,
            &first.id,
            &first.current_version_id,
            input("<h1>Second</h1>"),
        )
        .unwrap();
        assert_eq!(second.versions.len(), 2);
        assert_eq!(second.versions[0].content, "<h1>First</h1>");
        assert_eq!(
            second.versions[1].parent_id.as_deref(),
            Some(first.current_version_id.as_str())
        );
        assert_eq!(
            get(&project, &first.id).unwrap().current_version_id,
            second.current_version_id
        );
        assert_eq!(list(&project).unwrap().len(), 1);
        let other_root = project_dir("other");
        assert!(get(&other_root.to_string_lossy(), &first.id).is_err());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(other_root);
    }

    #[test]
    fn explicit_feedback_is_the_only_agent_handoff_and_is_single_assignment() {
        let (_home, _guard) = temp_home("feedback");
        let root = project_dir("feedback");
        let project = root.to_string_lossy();
        let artifact = create(&project, input("<button>Save</button>")).unwrap();
        let annotated = annotate(
            &project,
            &artifact.id,
            &artifact.current_version_id,
            "Make this button more prominent",
            Some(serde_json::json!({"selector":"button","target":"Save"})),
        )
        .unwrap();
        let annotation_id = annotated.annotations[0].id.clone();
        let agent_before = get_for_agent(&project, &artifact.id).unwrap();
        assert!(agent_before.submitted_annotations.is_empty());
        assert!(agent_before.feedback.is_empty());
        let submitted = feedback_submit(
            &project,
            &artifact.id,
            &artifact.current_version_id,
            "Please address the marked issue",
            vec![annotation_id.clone()],
            Some(serde_json::json!({"selected":"primary"})),
        )
        .unwrap();
        let agent_after = get_for_agent(&project, &artifact.id).unwrap();
        assert_eq!(agent_after.submitted_annotations.len(), 1);
        assert_eq!(agent_after.feedback.len(), 1);
        assert_eq!(
            agent_after.feedback[0].interactive_state.as_ref().unwrap()["selected"],
            "primary"
        );
        assert!(feedback_submit(
            &project,
            &artifact.id,
            &artifact.current_version_id,
            "",
            vec![annotation_id],
            None
        )
        .is_err());
        let feedback_id = submitted.feedback[0].id.clone();
        let response = feedback_respond(
            &project,
            &feedback_id,
            "addressed",
            "Revision 2 emphasizes the primary action.",
        )
        .unwrap();
        assert_eq!(response.feedback[0].decision.as_deref(), Some("addressed"));
        assert!(feedback_respond(&project, &feedback_id, "addressed", "Second answer").is_err());
        assert_eq!(
            feedback_list(&project, Some(&artifact.id)).unwrap()[0].status,
            "responded"
        );
        assert!(
            feedback_list_for_agent(&project, Some(&artifact.id))
                .unwrap()
                .is_empty(),
            "responded feedback is no longer pending for an agent response"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_revision_bad_ids_limits_and_symlink_directories_fail_closed() {
        let (_home, _guard) = temp_home("safety");
        let root = project_dir("safety");
        let project = root.to_string_lossy();
        let artifact = create(&project, input("original")).unwrap();
        assert!(revise(&project, &artifact.id, "stale", input("overwrite")).is_err());
        assert!(get(&project, "../outside").is_err());
        assert!(create(
            &project,
            ArtifactInput {
                code_refs: vec![CodeRef {
                    path: "../../outside".into(),
                    start_line: None,
                    end_line: None,
                    description: String::new()
                }],
                ..input("x")
            }
        )
        .is_err());
        let link_root = project_dir("linked-dir");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&link_root, root.join(".openleash")).unwrap();
            assert!(list(&project).is_err());
        }
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&link_root, root.join(".openleash")).is_ok() {
            assert!(list(&project).is_err());
        }
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(link_root);
    }
}
