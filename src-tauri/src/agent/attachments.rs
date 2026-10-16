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

pub fn project_dir(project: &str) -> PathBuf {
    std::env::temp_dir().join("OpenLeash").join(format!(
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

fn image_attachments(images: &[String], names: &[String]) -> Result<Vec<Attachment>, String> {
    images
        .iter()
        .enumerate()
        .map(|(index, url)| {
            let (media, data) = super::runner::parse_data_url(url)?;
            // Bound the allocation before base64 decoding; the encoded form is about
            // one third larger than the decoded bytes.
            if data.len() as u64 > MAX_FILE_BYTES.saturating_mul(4).div_ceil(3) + 4 {
                return Err(
                    "An image is larger than the 64 MiB attachment staging limit".to_string(),
                );
            }
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                .map_err(|_| "That image couldn't be read.".to_string())?;
            let ext = match media.as_str() {
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                "image/png" => "png",
                _ => "img",
            };
            let name = names
                .get(index)
                .filter(|n| !n.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| format!("image-{}.{}", index + 1, ext));
            Ok(Attachment { name, bytes })
        })
        .collect()
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
    let mut attachments = image_attachments(images, image_names)?;
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
    stage(project, task_id, batch_id, &attachments, docker).await
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
        "\n\n{ATTACHMENT_MARKER}\nThe user sent these attachments. OpenLeash saved copies in its temporary attachment folder. Use the staged paths below when you need to read or process the original files. Images are also included as image blocks so you can inspect their pixels directly.\n{}\n</openleash-attachments>",
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
}
