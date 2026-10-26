# Releases

Back to the [README](../README.md).

The current release process produces **unsigned** builds. This repository does not publish GitHub Releases, sign binaries or assemble updater assets. Follow the [maintainer release checklist](release-checklist.md) before publishing anything.

## Versioning

The version lives in `package.json`, `package-lock.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock`, and Tauri reads it from `package.json`.

- `npm run version:set -- <version>` updates all four together.
- `npm run version:check` verifies they agree. Add `-- --tag v<version>` to reject a mismatched release tag.

## What exists

| Piece | Where | What it does |
|---|---|---|
| Version scripts | `scripts/version.mjs` | Sets and checks the version (see above). |
| Download builds | `.github/workflows/build-downloads.yml` | Manual (`workflow_dispatch`) only. Builds macOS ARM64, Windows AMD64/ARM64 and Linux AMD64/ARM64 and uploads them as run artifacts, kept for 14 days. |
| `build_installers` input | same workflow | Also builds the Windows NSIS installers. Off by default. |
| `release_tag` input | same workflow | Validated against the package version. Blank means that version, without a `v` prefix. |
| Staging | `scripts/stage-downloads.py` | Renames each leg's single product to `OpenLeash_<version>_<suffix>` and refuses a partial set. |
| Maintainer builds | `publish.bat`, `publish-macos.sh` | Local cross-target builds into `published/`. Unsigned, and not the release workflow. |

Per-script details are in [building.md](building.md#repository-scripts).

## Manual publication

1. Run the gates in the [checklist](release-checklist.md#run-the-gates).
2. Set and check the version (see [Versioning](#versioning)).
3. Run **Actions → Build downloads → Run workflow**, optionally with `build_installers` and `release_tag`.
4. Download the ZIPs from the run's **Artifacts** before they expire, check them, and attach them to a GitHub Release by hand.

Downloads are unsigned, so Windows SmartScreen and macOS Gatekeeper will warn users.

## In-app updates

The app checks the latest stable `DevLeashed/OpenLeash` GitHub release on startup and every six hours. Install-capable builds (Windows NSIS and Linux AppImage) update in one click: **Update now** finds the release if needed, downloads it, and restarts into it — never while agent work is active, where it keeps the downloaded package and leaves the restart to the user. With the automatic switches on, the app does the same without being asked, up to installing once nothing is running. Portable Windows builds, macOS DMGs and Debian packages show a link to the release instead.

**Update packages are not signature-verified.** `src-tauri/tauri.conf.json` contains a minisign public key, but the update path does not check a signature against it. Until verification is re-enabled in `src-tauri/src/updates.rs`, anything able to serve the release manifest also decides what the app runs. See [security model](security-model.md#app-updates). The update notice's wording should not be read as a signature check.

## Signing

Nothing is signed or notarized. A signed release would need:

- a Windows code-signing certificate (or an equivalent signing service) for Authenticode,
- a Developer ID certificate and notarization credentials for macOS,
- a minisign signing key whose private half exists only in the release environment, plus working signature checks in the updater.

Keep private keys and tokens out of the repository, logs and release notes.
