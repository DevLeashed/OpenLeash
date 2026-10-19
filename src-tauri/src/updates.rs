//! App updates are separate from agent tools: no URL, key, installer arguments or
//! executable path can be supplied by the webview. Only explicit install applies
//! a package. Release text is never interpreted as code or shell arguments.
use crate::agent::Harness;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::atomic::Ordering, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(not(windows))]
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::{Mutex, RwLock};

const RELEASE_API: &str = "https://api.github.com/repos/DevLeashed/OpenLeash/releases/latest";
const RELEASE_PAGE: &str = "https://github.com/DevLeashed/OpenLeash/releases/latest";
const EVENT: &str = "ol://app-update";

#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    pub status: &'static str,
    pub version: Option<String>,
    pub error: Option<String>,
    pub can_install: bool,
}

#[derive(Default)]
struct Pending {
    update: Option<Update>,
    bytes: Option<Vec<u8>>,
}

pub struct Updates {
    // Status has its own lock so observers can see progress during network I/O.
    status: RwLock<UpdateStatus>,
    operation: Mutex<Pending>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            status: RwLock::new(UpdateStatus {
                status: "idle",
                version: None,
                error: None,
                can_install: installation_capable(),
            }),
            operation: Mutex::new(Pending::default()),
        }
    }
}

impl Updates {
    async fn publish(&self, app: &AppHandle, status: UpdateStatus) -> UpdateStatus {
        *self.status.write().await = status.clone();
        let _ = app.emit(EVENT, &status);
        status
    }

    async fn transition(&self, app: &AppHandle, status: &'static str) -> UpdateStatus {
        let mut view = self.status.read().await.clone();
        view.status = status;
        view.error = None;
        self.publish(app, view).await
    }

    async fn fail(&self, app: &AppHandle, error: String) -> String {
        let mut view = self.status.read().await.clone();
        view.status = "error";
        view.error = Some(error.clone());
        self.publish(app, view).await;
        error
    }

    async fn check(&self, app: &AppHandle) -> Result<UpdateStatus, String> {
        let mut pending = self
            .operation
            .try_lock()
            .map_err(|_| "An update operation is already in progress".to_string())?;
        if pending.bytes.is_some() {
            return Ok(self.status.read().await.clone());
        }
        pending.update = None;
        self.transition(app, "checking").await;
        match self.check_inner(app, &mut pending).await {
            Ok(view) => Ok(self.publish(app, view).await),
            Err(error) => Err(self.fail(app, error).await),
        }
    }

    async fn check_inner(
        &self,
        app: &AppHandle,
        pending: &mut Pending,
    ) -> Result<UpdateStatus, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            // This API never needs a redirect. Do not let release metadata or a
            // redirect turn the check into a request to a local/private service.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("OpenLeash-update-check")
            .build()
            .map_err(|e| e.to_string())?;
        let release: GithubRelease = client
            .get(RELEASE_API)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let current = &app.package_info().version;
        let version = stable_newer(current, &release)?;
        let Some(version) = version else {
            return Ok(UpdateStatus {
                status: "current",
                version: None,
                error: None,
                can_install: installation_capable(),
            });
        };
        let mut view = UpdateStatus {
            status: "available",
            version: Some(version.to_string()),
            error: None,
            can_install: installation_capable(),
        };
        if view.can_install {
            let result = async {
                let updater = app
                    .updater_builder()
                    .timeout(Duration::from_secs(60))
                    .version_comparator(|current, remote| {
                        remote.version.pre.is_empty()
                            && remote.version.cmp_precedence(&current).is_gt()
                    })
                    .build()
                    .map_err(|e| e.to_string())?;
                let update = updater
                    .check()
                    .await
                    // The plugin's config schema requires `pubkey` (dropping the
                    // key fails deserialization, so it stays in tauri.conf.json
                    // unused) and rejects a manifest whose platform entry carries
                    // no `signature` field, though nothing verifies one here;
                    // publish `"signature": ""` to satisfy it.
                    .map_err(|e| e.to_string())?
                    .ok_or("The update manifest is not available yet")?;
                validate_manifest(&version, &update.version, update.download_url.as_str())?;
                Ok::<_, String>(update)
            }
            .await;
            match result {
                Ok(update) => pending.update = Some(update),
                Err(error) => {
                    // Still advertise the release and manual link if a target is
                    // missing or the manifest has not been published yet.
                    view.can_install = false;
                    view.error = Some(error);
                }
            }
        }
        let previous = self.status.read().await.version.clone();
        if previous.as_deref() != view.version.as_deref() {
            notify(
                app,
                if view.can_install {
                    "A new OpenLeash release is available. Downloading in the background."
                } else {
                    "A new OpenLeash release is available. Download it from GitHub Releases."
                },
            );
        }
        Ok(view)
    }

    async fn download(&self, app: &AppHandle) -> Result<UpdateStatus, String> {
        let mut pending = self
            .operation
            .try_lock()
            .map_err(|_| "An update operation is already in progress".to_string())?;
        if pending.bytes.is_some() {
            return Ok(self.status.read().await.clone());
        }
        let update = pending
            .update
            .as_ref()
            .ok_or("No installable update is available; use GitHub Releases")?
            .clone();
        self.transition(app, "downloading").await;
        match fetch_release_asset(&update).await {
            Ok(bytes) => {
                pending.bytes = Some(bytes);
                let mut view = self.transition(app, "ready").await;
                view.error = None;
                let view = self.publish(app, view).await;
                notify(
                    app,
                    "OpenLeash update downloaded and verified. Choose Restart and install when your tasks are finished.",
                );
                Ok(view)
            }
            Err(error) => {
                let error = error.to_string();
                let mut view = self.status.read().await.clone();
                view.status = "error";
                view.error = Some(error.clone());
                // The signed manifest entry remains suitable for retry.
                view.can_install = true;
                self.publish(app, view).await;
                Err(error)
            }
        }
    }
}

/// Fetch the release asset the manifest points at.
///
/// Deliberately not `Update::download`: that path verifies the artifact against
/// the updater public key compiled into the app, and this project publishes
/// unsigned update assets, so the check could only ever reject them. The URL has
/// already been pinned to an HTTPS asset of this repository's releases by
/// [`validate_manifest`]; the bytes themselves are not authenticated, which is
/// why applying them stays behind an explicit user action.
async fn fetch_release_asset(update: &Update) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        // GitHub redirects asset downloads to its CDN, so this client follows
        // redirects where the metadata client above deliberately does not.
        .timeout(Duration::from_secs(30 * 60))
        .user_agent("OpenLeash-update-download")
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(update.download_url.clone())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Update download failed with status {}",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn stable_newer(current: &Version, release: &GithubRelease) -> Result<Option<Version>, String> {
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|_| "GitHub release tag is not a semantic version".to_string())?;
    Ok((!release.draft
        && !release.prerelease
        && version.pre.is_empty()
        && version.cmp_precedence(current).is_gt())
    .then_some(version))
}

fn validate_manifest(expected: &Version, actual: &str, url: &str) -> Result<(), String> {
    let actual = Version::parse(actual).map_err(|_| "Invalid update manifest version")?;
    if &actual != expected || !actual.pre.is_empty() {
        return Err("Update manifest does not match the latest stable GitHub release".into());
    }
    let url = reqwest::Url::parse(url).map_err(|_| "Invalid update download URL")?;
    // Signatures authenticate bytes, not URLs. Avoid network access to arbitrary
    // hosts even when a release manifest is compromised. GitHub redirects asset
    // downloads to its CDN; only the initial URL comes from the manifest.
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || ![
            format!("/DevLeashed/OpenLeash/releases/download/{expected}/"),
            format!("/DevLeashed/OpenLeash/releases/download/v{expected}/"),
        ]
        .iter()
        .any(|prefix| url.path().starts_with(prefix))
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Update package must be an HTTPS asset in DevLeashed/OpenLeash releases".into(),
        );
    }
    Ok(())
}

fn windows_installed(exe: &Path) -> bool {
    // Tauri's NSIS installer places uninstall.exe beside the application. A
    // portable exe (including compiled/ dev builds) has no such marker. Never
    // execute this marker: it is only used to distinguish installed from portable.
    exe.parent()
        .is_some_and(|dir| dir.join("uninstall.exe").is_file())
}

fn appimage_installed(path: &Path) -> bool {
    path.is_absolute() && path.is_file() && path.extension().is_some_and(|ext| ext == "AppImage")
}

fn installation_capable() -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    if cfg!(target_os = "windows") {
        return std::env::current_exe().is_ok_and(|exe| windows_installed(&exe));
    }
    if cfg!(target_os = "linux") {
        return std::env::var_os("APPIMAGE").is_some_and(|p| appimage_installed(Path::new(&p)));
    }
    if cfg!(target_os = "macos") {
        return std::env::current_exe().is_ok_and(|exe| {
            exe.parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .is_some_and(|p| p.extension().is_some_and(|ext| ext == "app"))
        });
    }
    false
}

fn notify(app: &AppHandle, body: &str) {
    // Windows: go through our own toast so a click raises the window. The
    // plugin's toast carries no activation argument and goes nowhere on click.
    #[cfg(windows)]
    {
        let id = app.config().identifier.clone();
        crate::win::show_toast(
            &id,
            "OpenLeash update",
            &format!(
                "{body}
{RELEASE_PAGE}"
            ),
            "",
        );
        return;
    }
    #[cfg(not(windows))]
    let _ = app
        .notification()
        .builder()
        .title("OpenLeash update")
        .body(format!("{body}\n{RELEASE_PAGE}"))
        .show();
}

async fn has_active_work(h: &Harness) -> bool {
    {
        let runtimes = h.runtimes.lock().unwrap();
        if runtimes.values().any(|rt| {
            rt.running.load(Ordering::SeqCst)
                || rt.startup_pending.load(Ordering::SeqCst)
                || rt.fg.load(Ordering::SeqCst) > 0
        }) {
            return true;
        }
    }
    let tasks = h.tasks.read().await;
    for (id, task) in tasks.iter() {
        let task = task.lock().await;
        if task.busy > 0
            || matches!(task.status.as_str(), "running" | "waiting")
            || task
                .subs
                .iter()
                .any(|s| matches!(s.status.as_str(), "running" | "waiting"))
            || h.bg.list(id).iter().any(|job| job.running)
        {
            return true;
        }
    }
    false
}

#[tauri::command]
pub async fn app_update_status(updates: State<'_, Updates>) -> Result<UpdateStatus, String> {
    Ok(updates.status.read().await.clone())
}

#[tauri::command]
pub async fn app_update_check(
    app: AppHandle,
    updates: State<'_, Updates>,
) -> Result<UpdateStatus, String> {
    let view = updates.check(&app).await?;
    if view.status == "available" && view.can_install {
        let owned = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = owned.state::<Updates>().download(&owned).await;
        });
    }
    Ok(view)
}

#[tauri::command]
pub async fn app_update_download(
    app: AppHandle,
    updates: State<'_, Updates>,
) -> Result<UpdateStatus, String> {
    updates.download(&app).await
}

#[tauri::command]
pub async fn app_update_install(
    app: AppHandle,
    updates: State<'_, Updates>,
    h: State<'_, Arc<Harness>>,
) -> Result<(), String> {
    let pending = updates
        .operation
        .try_lock()
        .map_err(|_| "An update operation is already in progress".to_string())?;
    if !installation_capable() {
        return Err("This installation requires a manual update from GitHub Releases".into());
    }
    let update = pending
        .update
        .as_ref()
        .ok_or("No installable update is available")?;
    let bytes = pending
        .bytes
        .as_ref()
        .ok_or("Download the update before installing")?;
    if has_active_work(&h).await {
        return Err(
            "Stop all active tasks, subagents and background commands before installing an update"
                .into(),
        );
    }
    // Flush durable state first. On Windows plugin install launches the signed
    // installer and exits the process directly; the installer relaunches the app.
    // macOS/AppImage replace the bundle then need an explicit restart below.
    let ids: Vec<String> = h.tasks.read().await.keys().cloned().collect();
    for id in ids {
        h.save_task_now(&id).await;
    }
    h.save_settings_now().await;
    h.stats.flush();
    if has_active_work(&h).await {
        return Err("Work started while preparing the update; stop it before installing".into());
    }
    if let Err(error) = update.install(bytes) {
        let mut view = updates.status.read().await.clone();
        view.status = "ready";
        view.error = Some(error.to_string());
        view.can_install = true;
        updates.publish(&app, view).await;
        return Err(error.to_string());
    }
    app.restart();
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Let the webview and notification identity finish initialization first.
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let updates = app.state::<Updates>();
            if let Ok(view) = updates.check(&app).await {
                if view.status == "available" && view.can_install {
                    let _ = updates.download(&app).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_stable_versions_are_offered() {
        let current_with_build = Version::parse("1.2.3+installed").unwrap();
        let current = Version::parse("1.2.3").unwrap();
        for tag in ["v1.2.2", "1.2.3", "v2.0.0-beta.1", "1.2.3+build"] {
            let release = GithubRelease {
                tag_name: tag.into(),
                draft: false,
                prerelease: false,
            };
            assert!(stable_newer(&current, &release).unwrap().is_none(), "{tag}");
            assert!(
                stable_newer(&current_with_build, &release)
                    .unwrap()
                    .is_none(),
                "{tag}"
            );
        }
        for (draft, prerelease) in [(true, false), (false, true)] {
            let release = GithubRelease {
                tag_name: "v2.0.0".into(),
                draft,
                prerelease,
            };
            assert!(stable_newer(&current, &release).unwrap().is_none());
        }
        let release = GithubRelease {
            tag_name: "v1.2.4".into(),
            draft: false,
            prerelease: false,
        };
        assert_eq!(
            stable_newer(&current, &release)
                .unwrap()
                .unwrap()
                .to_string(),
            "1.2.4"
        );
        assert!(stable_newer(
            &current,
            &GithubRelease {
                tag_name: "$(evil)".into(),
                draft: false,
                prerelease: false
            }
        )
        .is_err());
    }

    #[test]
    fn manifest_must_match_release_and_trusted_asset_path() {
        let expected = Version::parse("1.2.4").unwrap();
        let valid =
            "https://github.com/DevLeashed/OpenLeash/releases/download/v1.2.4/OpenLeash.exe";
        assert!(validate_manifest(&expected, "1.2.4", valid).is_ok());
        for version in ["1.2.3", "1.2.5", "1.2.4-beta.1", "bad"] {
            assert!(validate_manifest(&expected, version, valid).is_err());
        }
        for url in [
            "http://github.com/DevLeashed/OpenLeash/releases/download/v1/a.exe",
            "https://localhost/a.exe",
            "file:///tmp/a.exe",
            "https://github.com.evil.test/DevLeashed/OpenLeash/releases/download/v1/a.exe",
            "https://github.com/evil/OpenLeash/releases/download/v1/a.exe",
            "https://user:pass@github.com/DevLeashed/OpenLeash/releases/download/v1/a.exe",
            "https://github.com/DevLeashed/OpenLeash/releases/download/v1/a.exe?redirect=evil",
            "https://github.com/DevLeashed/OpenLeash/releases/download/../../evil/a.exe",
        ] {
            assert!(validate_manifest(&expected, "1.2.4", url).is_err(), "{url}");
        }
    }

    #[test]
    fn portable_windows_and_non_appimage_paths_are_rejected() {
        let dir =
            std::env::temp_dir().join(format!("openleash-updater-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("OpenLeash.exe");
        assert!(!windows_installed(&exe));
        std::fs::create_dir(dir.join("uninstall.exe")).unwrap();
        assert!(!windows_installed(&exe));
        std::fs::remove_dir(dir.join("uninstall.exe")).unwrap();
        std::fs::write(dir.join("uninstall.exe"), b"test marker, never executed").unwrap();
        assert!(windows_installed(&exe));
        let image = dir.join("OpenLeash.AppImage");
        assert!(!appimage_installed(&image));
        std::fs::write(&image, b"test").unwrap();
        assert!(appimage_installed(&image));
        assert!(!appimage_installed(Path::new("relative.AppImage")));
        assert!(!appimage_installed(&exe));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
