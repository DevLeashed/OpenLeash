# Releases

Back to the [README](../README.md).

There are no production-signed binaries attached to this repository yet. The
manual download workflow creates short-lived Actions artifacts but does not
publish a GitHub Release. Follow the [maintainer release checklist](release-checklist.md)
before publishing a release. What signing would involve:

**Windows.** An Authenticode code-signing certificate (OV or EV, from a CA —
SSL/TLS certificates do not work). Tauri's own build path wants
`bundle.windows.certificateThumbprint`, `digestAlgorithm` and `timestampUrl` in
`tauri.conf.json`, or a `signCommand` for anything else. A *bare executable* is
signed the same way an installer is, but a bare `.exe` with no installer gives
users no uninstall path — so `publish.bat` emits both for each Windows target,
and the installer is the one to point people at. SmartScreen reputation accrues
per-signing-identity over release history, so the first few releases will show a
warning no matter which certificate you buy. Also note the app is likely to trip
heuristic AV/EDR on the `WH_KEYBOARD_LL` hook regardless of signature.

**macOS.** Requires a paid Apple Developer account and a **Developer ID
Application** certificate, and it must be built on a Mac. Notarization
additionally requires `APPLE_API_ISSUER` / `APPLE_API_KEY` /
`APPLE_API_KEY_PATH` (App Store Connect API) or `APPLE_ID` / `APPLE_PASSWORD` /
`APPLE_TEAM_ID`. `publish-macos.sh` ships a bare Mach-O executable, not a
`.app` bundle or `.dmg` — notarization and Gatekeeper are far happier with a
signed `.app`, so a real macOS release should switch to
`tauri build --bundles app,dmg`. Ad-hoc signing (`"signingIdentity": "-"`) will
at least stop Apple Silicon treating the binary as damaged, but it still cannot
be notarized.

**Linux.** No signing is required. The AppImage needs FUSE at runtime.

**Updates.** The app checks for updates and downloads verified signed artifacts;
installation is initiated by the user. The updater uses a public key embedded in
the app and a static `latest.json` attached to the latest GitHub Release. Updater
signatures are mandatory and distinct from Authenticode or Apple signing: they
protect update authenticity, not SmartScreen/Gatekeeper reputation. Windows
updates require the NSIS-installed app (portable `.exe` downloads are not an
installer); macOS uses an `.app` archive, and Linux uses the AMD64 AppImage.
Linux ARM64 Debian packages remain manual downloads, not an updater target.

## Versioning

`package.json` → `version` is the source of truth. Tauri reads it through
`"version": "../package.json"`, so native app metadata and installer versions use
the same value. Cargo's package version and the root entries in both lockfiles
are synchronized by the version tool. Settings → General → About displays the
installed binary's version (or the package version in a browser-only preview).
MCP clients report the compiled Cargo package version.

Use semantic versions: `MAJOR.MINOR.PATCH`, optionally with a prerelease suffix
such as `0.2.0-beta.1`. Increment patch for fixes, minor for features, and major
for breaking changes; while the app is pre-1.0, a minor release may break
compatibility. A prerelease suffix is an explicit release marker, not an update
channel setting. Do not reuse a version for a different published release.

```sh
npm run version:set -- 0.1.1           # sets app and lockfile versions together
npm run version:check                  # read-only validation; fails on drift
npm run version:check -- --tag v0.1.1   # also validates a proposed release tag
```

Alternatively, `npm version patch --no-git-tag-version` uses npm's version
lifecycle to synchronize Cargo metadata. These commands do not publish anything;
the `version:set` command does not create commits or tags. Review and commit the
version metadata alongside the release changes. Checks run during normal dev,
build and `npm run check`, including the fast native build's frontend hook.

This establishes release identity only. Update artifacts must also be signed,
reviewed and manually attached to a release; versioning never publishes anything.
No update checks, downloads or installation run as part of versioning.

## Manual downloads

`.github/workflows/build-downloads.yml` is triggered only
from **Actions → Build downloads → Run workflow**. Push the workflow to the
default branch first so GitHub displays its Run button. Select the branch to
build, then download each ZIP from the run's **Artifacts** section:

| Artifact / contained file (`<version>` comes from `package.json`) | Contents |
|---|---|
| `OpenLeash_<version>_macos_aarch64.dmg` | macOS ARM64 disk image |
| `OpenLeash_<version>_win_x64.exe` | Windows AMD64 portable executable |
| `OpenLeash_<version>_win_arm64.exe` | Windows ARM64 portable executable |
| `OpenLeash_<version>_linux_amd64.AppImage` | Linux AMD64 AppImage |
| `OpenLeash_<version>_linux_amd64.deb` | Linux AMD64 Debian package |
| `OpenLeash_<version>_linux_arm64.deb` | Linux ARM64 Debian package |

Names follow the [0.1.0 release assets](https://github.com/DevLeashed/OpenLeash/releases/tag/0.1.0),
using the version being built rather than a fixed release version. Each artifact
ZIP contains only its named file at the root: additional installers, signatures,
app archives and JSON metadata each get their own single-file ZIP when opted in.
Linux AMD64 uploads its two formats as separate
artifacts under the Actions run's **Artifacts** section. ZIPs expire after 14 days;
they are not attached to a GitHub Release. These are unsigned/ad-hoc-signed test
builds, not notarized or
Authenticode-signed releases. The default run needs no secrets; no run creates
tags, releases or publishes assets. GitHub-hosted runner usage/storage may incur charges for private
repositories; check your Actions billing settings. Linux packages require the
corresponding architecture; an AppImage is not a promise of compatibility with
every distribution. GitHub's default artifact access controls apply: anyone
allowed to access the repository's Actions artifacts may be able to download
them. Do not use this channel for confidential or private distribution.

## Signed updater assets (manual release upload)

1. In GitHub **Settings → Secrets and variables → Actions → New repository secret**,
   add `TAURI_SIGNING_PRIVATE_KEY` with the **contents** of the existing private
   updater key (not its local filename). Add `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
   with its password; an unencrypted key uses an empty password (the password
   secret may be omitted). Keep the private key backed up securely, outside git;
   never attach it to a release or paste it into logs. The public key in
   `plugins.updater.pubkey` must match. Losing/rotating the key prevents existing
   installations accepting updates signed only by the new key.
2. Bump the app version and run **Build downloads** with **build_updater** checked.
   Leave **release_tag** blank for the exact package version (e.g. `0.1.0`, not
   `v0.1.0`), matching existing releases. If using a `v` tag, explicitly enter
   `v<version>`. Different versions, whitespace and path/URL injection are rejected.
3. The workflow retains all six normal downloads and additionally uploads:
   `OpenLeash_<version>_win_x64_setup.exe`,
   `OpenLeash_<version>_win_arm64_setup.exe`, and
   `OpenLeash_<version>_macos_aarch64.app.tar.gz`; each has a separate `.sig`
   artifact. The existing Linux AMD64 AppImage gets its own `.sig` artifact.
   Each supported target also uploads a single-file `updater-<target>.json`
   metadata fragment. After **all builds succeed**, an aggregation job downloads
   and validates them and uploads **latest.json**, again a single-file ZIP.
4. Download/extract the ZIPs. Review the manifest version, URLs and signatures,
   test the installers/updates on supported machines, then **manually** attach
   the six normal files, three updater extras, four `.sig` files and `latest.json`
   to the release with exactly the selected tag. Upload the contained files,
   **not the Actions ZIPs**. Fragments are assembly inputs, not release assets.
   `latest.json` URLs point at
   `https://github.com/DevLeashed/OpenLeash/releases/download/<tag>/<asset>`;
   publish only after every referenced asset is attached. Do not reuse versions.

With **build_updater** unchecked (default), the six unsigned downloads still
build without signing secrets. Signed mode merges
`src-tauri/tauri.updater.conf.json` (`createUpdaterArtifacts: true`) and omits
`--no-sign`: Tauri's CLI skips updater signatures as well as OS signing when that
flag is set. A Tauri updater key does **not** provide an Authenticode certificate,
Apple Developer ID, or notarization; those production distribution requirements
remain separate. Debian assets are not included in this updater manifest.

For a local rebuild of the manifest from downloaded/extracted artifacts:

```sh
python -B scripts/assemble-updater.py extracted --version 0.1.1 --output latest.json
# Optional: --release-tag v0.1.1 --notes "Release notes" --pub-date 2026-10-12T00:00:00Z
```

Nested extraction folders are supported. Assembly fails on missing, empty,
duplicate assets/signatures or inconsistent fragments, and refuses to overwrite
an existing output. It reads no private key, performs no network requests, and
publishes nothing. This validates packaging consistency; only the app's embedded
public-key verification establishes the cryptographic validity of signatures.

## Tag-based release workflow

There is currently no tag-based release or CI workflow in the checkout. The
manual-download workflow is the only build workflow. Versioning does not enable
publishing. Updates require the signed manual-release process above.

When a release workflow is added under `.github/workflows/`, it should run
`npm run version:check -- --tag "$RELEASE_TAG"` before building a tagged release.
Tags should match the canonical package version, optionally prefixed with `v`.
The current version-check tool's `--tag` option requires the `v` form; for a
historical unprefixed tag, use plain `version:check` and the updater scripts'
release-identity validation. Manual dispatch derives its tag from the version
when none is supplied. Prerelease
versions should mark the draft as a GitHub prerelease. Avoid reusing a tag that
belongs to another release commit, and review all assets, checksums, signing,
release notes and supported architectures before publishing a draft.

## No telemetry

There is no analytics or crash reporting. The updater checks GitHub for a signed manifest and downloads applicable updates; installation remains user initiated. Other outbound network activity is generated by features the user configures or invokes: model-provider requests, connector authentication and MCP server traffic, and web searches/fetches. For the narrower application-specific examples and credential details, see [Security model](security-model.md). API keys are written in plaintext to `~/.openleash/settings.json`; on Unix the file is **not** mode `600`, so other local users on a shared machine can read them. The webview runs under a Content-Security-Policy that forbids inline and remote script, so a prompt-injected page or repository file cannot turn into code execution in the app window.
