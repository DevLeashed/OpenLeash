# Maintainer release checklist

Back to [Releases](releasing.md) or the [README](../README.md).

This checklist describes how to prepare a release. It is not evidence that a
release is production-ready: only the manual-download workflow currently exists,
and its builds are unsigned/ad-hoc signed. Add and verify CI and a tag-release
workflow before relying on automated publication.

## Before the first public release

- [ ] Decide whether unsigned binaries are acceptable. Unsigned Windows downloads
can trigger SmartScreen; macOS downloads may be blocked by Gatekeeper. Adding
secrets alone does not enable signing. See [signing details](releasing.md).
- [ ] Decide which architectures are supported for the public release. The manual
  download workflow builds macOS ARM64, Windows x64/ARM64 and Linux x64/ARM64, but
  those artifacts are short-lived Actions artifacts, not attached to a release.
- [ ] Add CI and a tag-release workflow under `.github/workflows/`. Validate version
  metadata and tags before building, choose a platform matrix, and state supported
  architectures prominently in the release notes and download page.
- [ ] Decide whether users should rely on manual downloads for updates. There is
no in-app updater; users need to find and download each new release themselves.
- [ ] Review the known security limitations in [Security model](security-model.md)
and mention the important ones in user-facing release notes. In particular, agent
file reads are not sandboxed, local credentials are stored in plaintext, and
model-authored shell commands can affect the user's machine.
- [ ] Confirm the repository is public if public distribution is intended. Manual
  workflow artifact access follows repository Actions permissions; do not use it for
  confidential distribution.

## Prepare a version

- [ ] Choose a semantic version and release notes. Use
`npm run version:set -- <version>` to update `package.json`, `package-lock.json`,
`src-tauri/Cargo.toml`, and `src-tauri/Cargo.lock` together. Tauri reads the
version from `package.json`; keep its `../package.json` reference intact.
- [ ] Run `npm run version:check -- --tag v<version>` to reject metadata drift
or a mismatched release tag. See [Versioning](releasing.md#versioning).
- [ ] Describe user-visible changes, supported OS/architectures, known limitations,
security-relevant changes, and any migration or configuration notes.
- [ ] Check that the working tree contains only intended release changes and that
the commit to release is the one you intend users to run.

## Run the gates

Run these checks on the release commit. When CI is added, ensure it enforces the
same gates and confirm the matching run is green:

```sh
npm ci
npm run version:check
npm audit --audit-level=high
npx tsc --noEmit
npx eslint . --max-warnings 89
npm test
npm run build
cd src-tauri
cargo fmt --all --check
cargo test --locked
cargo install cargo-audit --locked --version 0.22.2
cargo audit
```

Also run `cargo clippy --locked --all-targets -- -D warnings` and report its
existing failures honestly. The `npm run check` shortcut does not apply the
explicit ESLint warning ceiling, build the production bundle, or run dependency
audits, so it is not a substitute for this list.

## Build and inspect downloads

1. Push the intended commit to the branch you want to test.
2. Run **Actions → Build downloads → Run workflow** and select that branch.
3. Confirm every selected build succeeded; successful targets remain downloadable
   even if another target fails.
4. Download each platform ZIP from the run's **Artifacts** section before it expires
   after 14 days. Verify the asset names, sizes, file types, architecture, and
   SHA-256 checksums; test installers on their intended platforms.
5. The workflow does not create or publish a GitHub Release. Follow a separate
   release process before publishing anything to users.

## After publication

- [ ] Confirm the published GitHub Release page and every public asset are visible
and downloadable without maintainer permissions.
- [ ] Verify the final public links and checksums from a clean browser/session.
- [ ] Watch CI/release workflow results and the private security-report channel.
- [ ] Record any failed or unsupported platform clearly; do not imply that an
artifact is supported just because the workflow produced it.

## Current release readiness gaps

These are deliberate visible gaps, not tasks that this checklist silently claims
are complete:

- No CI or tag-based release workflow is currently present.
- No production signing or notarization is configured.
- No in-app updater is configured.
- No published release has yet established install/upgrade behavior across the
  supported OS matrix. Linux installers target Ubuntu 24.04; older distributions
  may not be compatible.

Until these are addressed or explicitly accepted, describe downloads as unsigned
and set user expectations accordingly.
