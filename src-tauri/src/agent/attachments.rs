//! Temporary, task-owned copies of files sent to a model.
//!
//! Local copies live under `<OS temp>/OpenLeash/<project>/...`. SSH projects
//! also receive remote copies; Docker gets a read-only mount of project temp.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 20;
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct StagedMessage {
    pub local_paths: Vec<String>,
    pub model_paths: Vec<String>,
    pub names: Vec<String>,
    pub sizes: Vec<u64>,
    /// How many of the staged copies are pictures rather than files the agent
    /// reads. The transcript needs this: it drops a message's picture blobs
    /// from memory on older messages, and the count of blobs still on the item
    /// is not the count of pictures the message went out with, so the UI cannot
    /// work the split out from the blobs alone.
    pub image_count: usize,
    /// The subset of the caller's image data URLs that really are pictures a
    /// model can see, in their original order. Anything else in that list —
    /// an SVG, say — is staged among the files instead, so the model is handed
    /// a path it can open rather than an image block its provider refuses.
    pub picture_urls: Vec<String>,
}

fn safe_component(raw: &str) -> String {
    let name = Path::new(raw)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out
        .trim_matches(['.', '_'])
        .chars()
        .take(80)
        .collect::<String>();
    if out.is_empty() {
        "attachment".into()
    } else {
        out
    }
}

fn fingerprint(project: &str) -> String {
    Sha256::digest(super::trust::norm_path(project).as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn project_name(project: &str) -> String {
    let raw = if super::host::is_ssh(project) {
        super::host::parse_ssh(project)
            .map(|p| {
                Path::new(&p.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_default()
    } else {
        Path::new(project)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };
    safe_component(&raw)
}

/// The root of OpenLeash's own attachment staging area, under the OS temp
/// folder. Every copy the backend makes for a message lives inside it.
pub fn staging_root() -> PathBuf {
    std::env::temp_dir().join("OpenLeash")
}

pub fn project_dir(project: &str) -> PathBuf {
    staging_root().join(format!(
        "{}-{}",
        project_name(project),
        fingerprint(project)
    ))
}

pub fn task_dir(project: &str, task_id: &str) -> PathBuf {
    project_dir(project).join(safe_component(task_id))
}

fn remote_project_dir(project: &str) -> String {
    format!(
        "/tmp/OpenLeash/{}-{}",
        project_name(project),
        fingerprint(project)
    )
}

fn remote_task_dir(project: &str, task_id: &str) -> String {
    format!(
        "{}/{}/",
        remote_project_dir(project),
        safe_component(task_id)
    )
    .trim_end_matches('/')
    .to_string()
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("couldn't create attachment folder {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("couldn't secure attachment folder {}: {e}", path.display()))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("couldn't create staged attachment {}: {e}", path.display()))?;
    std::io::Write::write_all(&mut file, bytes)
        .map_err(|e| format!("couldn't write staged attachment {}: {e}", path.display()))
}

/// Make a local copy and the path visible to the agent's tools.
pub async fn stage(
    project: &str,
    task_id: &str,
    batch_id: &str,
    attachments: &[Attachment],
    docker_mode: bool,
) -> Result<StagedMessage, String> {
    if attachments.is_empty() {
        return Ok(StagedMessage::default());
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(format!(
            "A message can contain at most {MAX_ATTACHMENTS} attachments"
        ));
    }
    let total = attachments.iter().try_fold(0u64, |sum, a| {
        let size = a.bytes.len() as u64;
        if size > MAX_FILE_BYTES {
            return Err(format!(
                "{} is larger than the 64 MiB attachment staging limit",
                a.name
            ));
        }
        sum.checked_add(size)
            .filter(|n| *n <= MAX_TOTAL_BYTES)
            .ok_or_else(|| "Attachments exceed the 128 MiB per-message staging limit".to_string())
    })?;
    let _ = total;
    let dir = task_dir(project, task_id).join(safe_component(batch_id));
    let local_root = project_dir(project);
    create_private_dir(&local_root)?;
    create_private_dir(&task_dir(project, task_id))?;
    create_private_dir(&dir)?;
    let mut staged = StagedMessage::default();
    let result = async {
        let remote_dir = if super::host::is_ssh(project) {
            let sp = super::host::parse_ssh(project).ok_or("invalid remote project path")?;
            let remote_dir = format!(
                "{}/{}/{}",
                remote_project_dir(project),
                safe_component(task_id),
                safe_component(batch_id)
            );
            super::ssh_session::create_dir_all(&sp.host, &remote_dir).await?;
            Some((sp.host, remote_dir))
        } else {
            None
        };
        for (index, attachment) in attachments.iter().enumerate() {
            let name = format!("{:02}-{}", index + 1, safe_component(&attachment.name));
            let local = dir.join(&name);
            write_private_file(&local, &attachment.bytes)?;
            let model_path = if let Some((host, remote_dir)) = &remote_dir {
                let remote_path = format!("{remote_dir}/{name}");
                super::ssh_session::write(host, &remote_path, &attachment.bytes).await?;
                remote_path
            } else if docker_mode {
                let root = project_dir(project)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                format!(
                    "/tmp/OpenLeash/{root}/{}/{}/{}",
                    safe_component(task_id),
                    safe_component(batch_id),
                    name
                )
            } else {
                local.to_string_lossy().to_string()
            };
            staged.local_paths.push(local.to_string_lossy().to_string());
            staged.model_paths.push(model_path);
            staged.names.push(attachment.name.clone());
            staged.sizes.push(attachment.bytes.len() as u64);
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&dir);
        if super::host::is_ssh(project) {
            remove_remote_batch(project, task_id, batch_id).await;
        }
        return Err(error);
    }
    Ok(staged)
}

/// Decode one `data:` URL into its media type and bytes, with the staging size
/// cap checked before the allocation.
fn decode_image_url(url: &str) -> Result<(String, Vec<u8>), String> {
    let (media, data) = super::runner::parse_data_url(url)?;
    let media = media.to_ascii_lowercase();
    // Bound the allocation before base64 decoding; the encoded form is about
    // one third larger than the decoded bytes.
    if data.len() as u64 > MAX_FILE_BYTES.saturating_mul(4).div_ceil(3) + 4 {
        return Err("An image is larger than the 64 MiB attachment staging limit".to_string());
    }
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
        .map_err(|_| "That image couldn't be read.".to_string())?;
    Ok((media, bytes))
}

fn image_attachment(url: &str, name: Option<&str>, index: usize) -> Result<Attachment, String> {
    let (media, bytes) = decode_image_url(url)?;
    let ext = match media.as_str() {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/png" => "png",
        _ => "img",
    };
    let name = name
        .filter(|n| !n.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("image-{}.{}", index + 1, ext));
    Ok(Attachment { name, bytes })
}

fn image_attachments(images: &[String], names: &[String]) -> Result<Vec<Attachment>, String> {
    images
        .iter()
        .enumerate()
        .map(|(index, url)| image_attachment(url, names.get(index).map(String::as_str), index))
        .collect()
}

/// The extension a staged copy of a data URL should carry. A type the model
/// cannot see is a file — an SVG, a BMP, a PDF that arrived in an image slot —
/// and the agent opens it by name with its own tools, so the name must not
/// pretend to be a PNG.
fn file_ext_for_media(media: &str) -> &'static str {
    match media.to_ascii_lowercase().as_str() {
        "image/svg+xml" => "svg",
        "image/bmp" | "image/x-ms-bmp" => "bmp",
        "image/avif" => "avif",
        "image/tiff" => "tiff",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "application/pdf" => "pdf",
        _ => "img",
    }
}

/// Turn a data URL the model cannot see into the file it actually is.
///
/// The supplied name is the stem: `drawing.svg` stays `drawing.svg`, while a
/// generic `image-1` or a mismatched `image-1.png` gets the extension its bytes
/// call for. A file called `.png` that holds SVG would send the agent to
/// `view_image`, which is the same dead end this conversion exists to avoid.
fn file_attachment(url: &str, name: Option<&str>, index: usize) -> Result<Attachment, String> {
    let (media, bytes) = decode_image_url(url)?;
    let ext = file_ext_for_media(&media);
    let stem = name
        .filter(|n| !n.trim().is_empty())
        .and_then(|n| Path::new(n).file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("image-{}", index + 1));
    Ok(Attachment {
        name: format!("{stem}.{ext}"),
        bytes,
    })
}

/// Stage both image blocks and file references in one task-owned batch.
pub async fn stage_user_attachments(
    project: &str,
    task_id: &str,
    batch_id: &str,
    images: &[String],
    image_names: &[String],
    paths: &[String],
    docker: bool,
) -> Result<StagedMessage, String> {
    if images.len().saturating_add(paths.len()) > MAX_ATTACHMENTS {
        return Err(format!(
            "A message can contain at most {MAX_ATTACHMENTS} attachments"
        ));
    }
    let image_encoded = images.iter().try_fold(0u64, |sum, url| {
        let (_, data) = super::runner::parse_data_url(url)?;
        if data.len() as u64 > MAX_FILE_BYTES.saturating_mul(4).div_ceil(3) + 4 {
            return Err("An image is larger than the 64 MiB attachment staging limit".to_string());
        }
        sum.checked_add(data.len() as u64)
            .filter(|n| *n <= MAX_TOTAL_BYTES.saturating_mul(4).div_ceil(3) + 4)
            .ok_or_else(|| "Attachments exceed the 128 MiB per-message staging limit".to_string())
    })?;
    let _ = image_encoded;
    // Split what the UI (or a saved prompt) called "images" into the pictures a
    // model can see and the files it cannot. An SVG reaches this list when an
    // older saved prompt or message carried one as a data URL, and sending it
    // as an image block makes the provider refuse the whole request. As a file
    // it is exactly what the agent needs: a path it opens with its own tools.
    let mut pictures: Vec<String> = Vec::new();
    let mut picture_names: Vec<String> = Vec::new();
    let mut image_files: Vec<Attachment> = Vec::new();
    for (index, url) in images.iter().enumerate() {
        let (media, _) = super::runner::parse_data_url(url)?;
        let name = image_names.get(index).map(String::as_str);
        if super::tools::is_model_image(&media) {
            pictures.push(url.clone());
            picture_names.push(name.unwrap_or("").to_string());
        } else {
            image_files.push(file_attachment(url, name, index)?);
        }
    }
    let mut attachments = image_attachments(&pictures, &picture_names)?;
    attachments.extend(image_files);
    let mut total = attachments
        .iter()
        .map(|a| a.bytes.len() as u64)
        .sum::<u64>();
    for raw in paths {
        let path = PathBuf::from(raw);
        let metadata =
            std::fs::metadata(&path).map_err(|e| format!("couldn't read attachment {raw}: {e}"))?;
        if !metadata.is_file() {
            return Err(format!("{raw} isn't a regular file"));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!(
                "{raw} is larger than the 64 MiB attachment staging limit"
            ));
        }
        total = total
            .checked_add(metadata.len())
            .filter(|n| *n <= MAX_TOTAL_BYTES)
            .ok_or_else(|| {
                "Attachments exceed the 128 MiB per-message staging limit".to_string()
            })?;
        let file = std::fs::File::open(&path)
            .map_err(|e| format!("couldn't read attachment {raw}: {e}"))?;
        let mut bytes = Vec::with_capacity(metadata.len().min(MAX_FILE_BYTES) as usize);
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("couldn't read attachment {raw}: {e}"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(format!(
                "{raw} is larger than the 64 MiB attachment staging limit"
            ));
        }
        attachments.push(Attachment {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            bytes,
        });
    }
    let mut staged = stage(project, task_id, batch_id, &attachments, docker).await?;
    // The pictures come first in the staged list, then the files (including
    // anything that arrived in an image slot but isn't a picture a model can
    // see). The transcript needs that split even after it has dropped the blobs
    // itself, so it is recorded rather than left to be guessed from what is
    // still on the item.
    staged.image_count = pictures.len();
    staged.picture_urls = pictures;
    Ok(staged)
}

/// Read a staged copy of a picture back as a `data:` URL.
///
/// The transcript drops a message's picture blobs from memory once a chat grows
/// (`prune_item_images`), but the copy the backend staged on disk is still
/// there, so the picture can still be shown — which is the point: a user's
/// picture must not turn into a paperclip chip because the app ran out of
/// memory to keep it twice.
///
/// Only a copy inside OpenLeash's own staging folder is read. The path comes
/// from the transcript, and the transcript is data the agent can influence (a
/// tool result, a reply), so a path outside that folder is refused rather than
/// turned into a file read.
pub fn staged_image_data_url(path: &str) -> Result<String, String> {
    let p = PathBuf::from(path);
    // Resolve before comparing, so `..` cannot walk out of the folder.
    let root = staging_root();
    let full = p
        .canonicalize()
        .map_err(|_| "That attachment is no longer on disk.".to_string())?;
    let root = root
        .canonicalize()
        .map_err(|_| "That attachment is no longer on disk.".to_string())?;
    if !full.starts_with(&root) {
        return Err("That attachment is not one of OpenLeash's own copies.".into());
    }
    // The bytes decide what this is, not the name: a staged copy is named by
    // what the user picked it as, and that can be anything.
    let bytes = std::fs::read(&full).map_err(|e| format!("couldn't read that attachment: {e}"))?;
    let media = super::tools::sniff_media_type(&bytes)
        .ok_or_else(|| "That attachment is not a picture any more.".to_string())?;
    Ok(format!(
        "data:{};base64,{}",
        media,
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
    ))
}

pub fn remove_task(project: &str, task_id: &str) -> Result<(), String> {
    match std::fs::remove_dir_all(task_dir(project, task_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "couldn't remove temporary attachments for task {task_id}: {e}"
        )),
    }
}

async fn remove_remote_dir(host: &str, path: &str) -> Result<(), String> {
    let mut current = vec![path.to_string()];
    while let Some(dir) = current.pop() {
        for entry in super::ssh_session::read_dir(host, &dir).await? {
            let child = format!("{dir}/{}", entry.name);
            if entry.is_dir && !entry.is_symlink {
                current.push(child);
            } else {
                super::ssh_session::remove_file(host, &child).await?;
            }
        }
        super::ssh_session::remove_dir(host, &dir).await?;
    }
    Ok(())
}

async fn remove_remote_batch(project: &str, task_id: &str, batch_id: &str) {
    let Some(sp) = super::host::parse_ssh(project) else {
        return;
    };
    let dir = format!(
        "{}/{}/{}",
        remote_project_dir(project),
        safe_component(task_id),
        safe_component(batch_id)
    );
    let _ = remove_remote_dir(&sp.host, &dir).await;
}

pub async fn remove_remote_task(project: &str, task_id: &str) {
    let Some(sp) = super::host::parse_ssh(project) else {
        return;
    };
    let dir = remote_task_dir(project, task_id);
    let _ = remove_remote_dir(&sp.host, &dir).await;
}

/// The host file behind a staged path the model was given.
///
/// In a docker workspace the model is told the container path
/// (`/tmp/OpenLeash/<root>/<task>/<batch>/<name>`), and that file exists only
/// inside the container's read-only mount — so a reader on the host has to map
/// it back to the copy OpenLeash staged. Everywhere else the staged path is
/// already the host path and this returns `None`.
///
/// The mapping is deliberately narrow: only OpenLeash's own mount, and only
/// the file names under it. A path that climbs out of the mount is refused
/// rather than resolved.
pub fn host_path_for(project: &str, model_path: &str) -> Option<PathBuf> {
    let root = project_dir(project);
    let root_name = root.file_name()?.to_string_lossy().to_string();
    let prefix = format!("/tmp/OpenLeash/{root_name}/");
    let normalized = model_path.replace('\\', "/");
    let rest = normalized.strip_prefix(&prefix)?;
    if rest.is_empty() || rest.split('/').any(|s| s.is_empty() || s == "..") {
        return None;
    }
    let mut out = root;
    for seg in rest.split('/') {
        out.push(seg);
    }
    Some(out)
}

/// Opens the note appended to a user turn that carried attachments. Named so the
/// context-attribution scan in `runner::context_parts` finds the same bytes this
/// writer emits, instead of keeping its own copy of the literal.
pub const ATTACHMENT_MARKER: &str = "<openleash-attachments>";

pub fn prompt_paths(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let safe = |p: &str| {
        p.chars()
            .map(|c| match c {
                '&' => "&amp;".to_string(),
                '<' => "&lt;".to_string(),
                '>' => "&gt;".to_string(),
                '"' => "&quot;".to_string(),
                '\'' => "&apos;".to_string(),
                c if c.is_control() => format!("\\u{{{:x}}}", c as u32),
                c => c.to_string(),
            })
            .collect::<String>()
    };
    format!(
        "\n\n{ATTACHMENT_MARKER}\nThe user sent these attachments. OpenLeash saved copies in its temporary attachment folder. Use the staged paths below when you need to read or process the original files. Pictures a model can see (PNG, JPEG, GIF or WebP) are also included as image blocks so you can inspect their pixels directly. Anything else — an SVG, a PDF — is only the file named below: open it with your own tools (an SVG is markup, so `read_file`, not `view_image`). To show one of these pictures to a subagent, pass its path in the `task` tool's `image_paths` — the subagent receives the picture itself, so it can judge it without asking you for it.\n{}\n</openleash-attachments>",
        paths.iter().map(|p| format!("- `{}`", safe(p))).collect::<Vec<_>>().join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_folders_are_sanitized_and_collision_resistant() {
        let a = project_dir("C:/work/My Project");
        let b = project_dir("D:/other/My Project");
        assert!(a.to_string_lossy().contains("OpenLeash"));
        assert!(a
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("My_Project-"));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn copies_attachments_with_unique_names_and_scoped_cleanup() {
        let project = std::env::temp_dir().join(format!("ol-stage-proj-{}", uuid::Uuid::new_v4()));
        let task = format!("task-{}", uuid::Uuid::new_v4());
        let staged = stage(
            &project.to_string_lossy(),
            &task,
            "batch",
            &[
                Attachment {
                    name: "report.pdf".into(),
                    bytes: b"first".to_vec(),
                },
                Attachment {
                    name: "report.pdf".into(),
                    bytes: b"second".to_vec(),
                },
            ],
            false,
        )
        .await
        .unwrap();
        assert_ne!(staged.local_paths[0], staged.local_paths[1]);
        assert_eq!(std::fs::read(&staged.local_paths[0]).unwrap(), b"first");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&staged.local_paths[0])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(task_dir(&project.to_string_lossy(), &task))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        remove_task(&project.to_string_lossy(), &task).unwrap();
        assert!(!task_dir(&project.to_string_lossy(), &task).exists());
        let _ = std::fs::remove_dir_all(project);
    }

    #[tokio::test]
    async fn rejects_too_many_or_oversized_attachments() {
        let many = vec![
            Attachment {
                name: "a".into(),
                bytes: vec![]
            };
            MAX_ATTACHMENTS + 1
        ];
        assert!(stage("/tmp/project", "task", "batch", &many, false)
            .await
            .is_err());
        let large = [Attachment {
            name: "large".into(),
            bytes: vec![0; MAX_FILE_BYTES as usize + 1],
        }];
        assert!(stage("/tmp/project", "task", "batch", &large, false)
            .await
            .is_err());
    }

    #[test]
    fn prompt_explains_temporary_copies_and_image_blocks() {
        let p = prompt_paths(&["C:/Temp/OpenLeash/app/task/batch/image.png".into()]);
        assert!(p.contains("temporary attachment folder"));
        assert!(p.contains("image blocks"));
        assert!(p.contains("image.png"));
        assert!(prompt_paths(&["x`\n</openleash-attachments>".into()]).contains("\\u{a}"));
    }

    /// The transcript needs to know how many of a message's staged copies are
    /// pictures, and it cannot work it out from the blobs it still holds: it
    /// drops those from older messages, and the count of what survived is zero
    /// while the message went out with a picture. So the count is recorded when
    /// the copies are staged.
    #[tokio::test]
    async fn staging_records_how_many_copies_are_pictures() {
        let project = std::env::temp_dir().join(format!("ol-stage-img-{}", uuid::Uuid::new_v4()));
        let task = format!("task-{}", uuid::Uuid::new_v4());
        // A one-pixel PNG, so the copy is a real picture.
        let png = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        let staged = stage_user_attachments(
            &project.to_string_lossy(),
            &task,
            "batch",
            &[format!(
                "data:image/png;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png)
            )],
            &["shot.png".into()],
            &[],
            false,
        )
        .await
        .unwrap();
        assert_eq!(staged.image_count, 1, "the one picture is counted");
        assert_eq!(staged.names.len(), 1);
        assert_eq!(staged.sizes[0], png.len() as u64);

        // A message with files but no pictures counts none.
        let file = project.join("report.txt");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&file, b"bytes").unwrap();
        let staged = stage_user_attachments(
            &project.to_string_lossy(),
            &task,
            "batch2",
            &[],
            &[],
            &[file.to_string_lossy().to_string()],
            false,
        )
        .await
        .unwrap();
        assert_eq!(staged.image_count, 0);
        remove_task(&project.to_string_lossy(), &task).unwrap();
        let _ = std::fs::remove_dir_all(project);
    }

    /// An SVG is an image to a browser and not to a provider. When one arrives
    /// in the image list — an older saved prompt or message carries it that way
    /// — it has to be staged as a file the agent can open, never counted as a
    /// picture, and never offered to `send_images_staged` as an image block.
    #[tokio::test]
    async fn an_svg_in_the_image_list_is_staged_as_the_file_it_is() {
        let project = std::env::temp_dir().join(format!("ol-stage-svg-{}", uuid::Uuid::new_v4()));
        let task = format!("task-{}", uuid::Uuid::new_v4());
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="1" height="1"/></svg>"#;
        let url = format!(
            "data:image/svg+xml;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, svg)
        );
        let png_url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==".to_string();
        let text = project.join("report.txt");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&text, b"bytes").unwrap();
        let staged = stage_user_attachments(
            &project.to_string_lossy(),
            &task,
            "batch",
            &[png_url.clone(), url],
            &["shot.png".into(), "drawing.svg".into()],
            &[text.to_string_lossy().to_string()],
            false,
        )
        .await
        .unwrap();
        // Only the raster picture is a picture: it is what the send may turn
        // into an image block, and the count the transcript splits on.
        assert_eq!(staged.image_count, 1);
        assert_eq!(staged.picture_urls, [png_url]);
        // The pictures come first, then the SVG and the file. The SVG keeps
        // the name it arrived with, so the agent knows to read it as markup.
        assert_eq!(
            staged.names,
            vec![
                "shot.png".to_string(),
                "drawing.svg".to_string(),
                "report.txt".to_string()
            ]
        );
        assert!(staged.model_paths[1].ends_with("02-drawing.svg"));
        assert_eq!(std::fs::read(&staged.local_paths[1]).unwrap(), svg);
        remove_task(&project.to_string_lossy(), &task).unwrap();
        let _ = std::fs::remove_dir_all(project);
    }

    /// A name that claims to be a picture must not survive the conversion: a
    /// staged `image-1.png` holding SVG sends the agent to `view_image`, which
    /// refuses it. The extension follows the bytes.
    #[tokio::test]
    async fn a_converted_image_file_gets_the_extension_its_bytes_call_for() {
        let project = std::env::temp_dir().join(format!("ol-stage-ext-{}", uuid::Uuid::new_v4()));
        let task = format!("task-{}", uuid::Uuid::new_v4());
        let url = format!(
            "data:image/svg+xml;base64,{}",
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"<svg xmlns='http://www.w3.org/2000/svg'/>"
            )
        );
        let staged = stage_user_attachments(
            &project.to_string_lossy(),
            &task,
            "batch",
            &[url],
            &["image-1.png".into()],
            &[],
            false,
        )
        .await
        .unwrap();
        assert_eq!(staged.names, vec!["image-1.svg".to_string()]);
        remove_task(&project.to_string_lossy(), &task).unwrap();
        let _ = std::fs::remove_dir_all(project);
    }

    /// A picture the transcript has dropped is still on disk, in OpenLeash's
    /// own staging folder, and can be read back so it can be shown as a picture
    /// again rather than as a chip naming a file.
    #[tokio::test]
    async fn a_staged_picture_can_be_read_back_as_a_data_url() {
        let project = std::env::temp_dir().join(format!("ol-stage-read-{}", uuid::Uuid::new_v4()));
        let task = format!("task-{}", uuid::Uuid::new_v4());
        let png = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        let staged = stage_user_attachments(
            &project.to_string_lossy(),
            &task,
            "batch",
            &[format!(
                "data:image/png;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png)
            )],
            &["shot.png".into()],
            &[],
            false,
        )
        .await
        .unwrap();
        let url = staged_image_data_url(&staged.local_paths[0]).unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        let back = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            url.trim_start_matches("data:image/png;base64,"),
        )
        .unwrap();
        assert_eq!(back, png, "the same bytes come back");

        // A file that is not a picture is refused rather than shown as one.
        let not_an_image = project.join("notes.txt");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&not_an_image, b"just text").unwrap();
        let copy = task_dir(&project.to_string_lossy(), &task)
            .join("batch")
            .join("02-notes.txt");
        std::fs::copy(&not_an_image, &copy).unwrap();
        assert!(staged_image_data_url(&copy.to_string_lossy()).is_err());

        remove_task(&project.to_string_lossy(), &task).unwrap();
        let _ = std::fs::remove_dir_all(project);
    }

    /// The path comes from the transcript, and the transcript is data the agent
    /// can put words into, so it is not allowed to name a file anywhere on the
    /// disk — only OpenLeash's own staging folder is read.
    #[test]
    fn only_copies_inside_the_staging_folder_are_read_back() {
        let outside = std::env::temp_dir().join(format!("ol-outside-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&outside, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        let err = staged_image_data_url(&outside.to_string_lossy()).unwrap_err();
        assert!(
            err.contains("not one of OpenLeash's own copies"),
            "the read is confined to the staging folder: {err}"
        );
        // A path that walks out of it is refused too.
        let escape = staging_root().join("..").join("outside.png");
        assert!(staged_image_data_url(&escape.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(&outside);
    }

    /// In a docker workspace the model is told the container's path for a
    /// staged copy, and the file itself is on the host. The mapping back has to
    /// work — and it has to stay inside OpenLeash's own mount.
    #[test]
    fn a_container_staged_path_maps_back_to_the_host_copy() {
        let project = format!("C:/work/{}", uuid::Uuid::new_v4());
        let root = project_dir(&project);
        let name = root.file_name().unwrap().to_string_lossy().to_string();
        let model_path = format!("/tmp/OpenLeash/{name}/task-1/batch-1/01-shot.png");
        let host = host_path_for(&project, &model_path).expect("the staged path maps back");
        assert_eq!(
            host,
            root.join("task-1").join("batch-1").join("01-shot.png")
        );

        // A path outside the mount is not mapped at all.
        assert!(host_path_for(&project, "/etc/passwd").is_none());
        assert!(host_path_for(&project, "/tmp/OpenLeash/other/task/01-shot.png").is_none());
        // Neither is one that climbs out of it.
        assert!(
            host_path_for(&project, &format!("/tmp/OpenLeash/{name}/../../etc/passwd")).is_none()
        );
        // Nor a path with an empty segment.
        assert!(
            host_path_for(&project, &format!("/tmp/OpenLeash/{name}/task//shot.png")).is_none()
        );
    }

    /// The mapped path has to be a file that can actually be read, because
    /// that is the whole point of the mapping: the model is given the
    /// container's path and the picture has to be read from the host.
    ///
    /// The order matters and was a bug once: `/tmp/OpenLeash/...` is *not* an
    /// absolute path on Windows, so resolving it against the working directory
    /// first joined the two and the mapping never matched.
    #[tokio::test]
    async fn a_container_staged_path_reads_the_picture_behind_it() {
        let project = format!("C:/work/{}", uuid::Uuid::new_v4());
        let root = project_dir(&project);
        let name = root.file_name().unwrap().to_string_lossy().to_string();
        let dir = root.join("task-1").join("batch-1");
        std::fs::create_dir_all(&dir).unwrap();
        let png = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        std::fs::write(dir.join("01-shot.png"), &png).unwrap();

        let model_path = format!("/tmp/OpenLeash/{name}/task-1/batch-1/01-shot.png");
        let host = host_path_for(&project, &model_path).expect("the staged path maps back");
        let img = super::super::tools::view_image(&host).expect("the picture reads back");
        assert_eq!(img.media_type, "image/png");
        assert_eq!(
            img.bytes,
            png.len() as u64,
            "and it is the bytes that were staged"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
