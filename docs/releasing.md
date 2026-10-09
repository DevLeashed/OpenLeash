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

**Updates.** There is no updater: no `tauri-plugin-updater` in the dependency
tree, no `plugins.updater` block in `tauri.conf.json`, and no
`createUpdaterArtifacts` in the bundle config. That is a deliberate current
state, not an oversight, but it means **every user updates by downloading a new
binary from GitHub Releases by hand**. Adding an updater later means adding the
plugin, a signing key pair, a manifest endpoint, and — because an unsigned
update channel is a remote-code-execution hole — a real update-signing story
before it can be turned on.

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

This establishes release identity only. Future automatic updates still need a
signed update manifest/artifacts, endpoints and an explicit prerelease-channel
policy. No update checks, downloads or installation run as part of versioning.

## Manual downloads

`.github/workflows/build-downloads.yml` is triggered only
from **Actions → Build downloads → Run workflow**. Push the workflow to the
default branch first so GitHub displays its Run button. Select the branch to
build, then download each ZIP from the run's **Artifacts** section:

| Artifact | Contents |
|---|---|
| macOS ARM64 | DMG and archived `.app` bundle |
| Windows AMD64 | NSIS setup `.exe` and portable `.exe` |
| Windows ARM64 | NSIS setup `.exe` and portable `.exe` |
| Linux AMD64 | AppImage and `.deb` |
| Linux ARM64 | `.deb` |

Each job uploads its platform downloads as a ZIP under the Actions run's
**Artifacts** section. ZIPs expire after 14 days; they are not attached to a GitHub
Release. These are unsigned/ad-hoc-signed test builds, not notarized or
Authenticode-signed releases. No secrets, tags, release creation or publishing are
required. GitHub-hosted runner usage/storage may incur charges for private
repositories; check your Actions billing settings. Linux packages require the
corresponding architecture; an AppImage is not a promise of compatibility with
every distribution. GitHub's default artifact access controls apply: anyone
allowed to access the repository's Actions artifacts may be able to download
them. Do not use this channel for confidential or private distribution.

## Tag-based release workflow

There is currently no tag-based release or CI workflow in the checkout. The
manual-download workflow is the only build workflow. Versioning does not enable
publishing or install an updater.

When a release workflow is added under `.github/workflows/`, it should run
`npm run version:check -- --tag "$RELEASE_TAG"` before building a tagged release.
Tags must match `v` plus the canonical package version. Manual dispatch should
derive its tag from that version rather than use a fixed fallback. Prerelease
versions should mark the draft as a GitHub prerelease. Avoid reusing a tag that
belongs to another release commit, and review all assets, checksums, signing,
release notes and supported architectures before publishing a draft.

## No telemetry

There is no analytics, no crash reporting and no update check. Outbound network activity is generated by features the user configures or invokes: model-provider requests, connector authentication and MCP server traffic, and web searches/fetches. For the narrower application-specific examples and credential details, see [Security model](security-model.md). API keys are written in plaintext to `~/.openleash/settings.json`; on Unix the file is **not** mode `600`, so other local users on a shared machine can read them. The webview runs under a Content-Security-Policy that forbids inline and remote script, so a prompt-injected page or repository file cannot turn into code execution in the app window.
