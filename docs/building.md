# Building from source

Back to the [README](../README.md).

OpenLeash is a Tauri 2 app: a Rust core plus a React 19 frontend. You need all
of Node, Rust and the platform's native webview libraries.

## Prerequisites

| | Windows | macOS | Linux |
|---|---|---|---|
| Node.js | `^20.19.0` or `^22.12.0` (22 LTS recommended) | `^20.19.0` or `^22.12.0` (22 LTS recommended) | `^20.19.0` or `^22.12.0` (22 LTS recommended) |
| Rust | [rustup](https://rustup.rs), MSVC toolchain | [rustup](https://rustup.rs) | [rustup](https://rustup.rs) |
| C++ build tools | "Desktop development with C++" in the Visual Studio Build Tools | `xcode-select --install` | `build-essential` |
| Webview / native libs | bundled with Windows (WebView2) | bundled with macOS (WKWebView) | `build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf libxdo-dev libssl-dev libpipewire-0.3-dev libspa-0.2-dev libclang-dev libxkbcommon-dev libxcb1-dev libxcb-randr0-dev libxrandr-dev libxi-dev libxtst-dev libdbus-1-dev libgbm-dev libfuse2` |

On Windows you also need the ARM64 cross-tools for the ARM64 build: Visual Studio
Installer → Modify → Individual components → *"C++ ARM64 build tools for
Windows"*.

## Platform dependencies

The direct `windows` and `windows-core` dependencies are Windows-only, so Linux and macOS builds no longer pull the Windows COM dependency that previously failed to compile. Linux production builds are covered by the manual download workflow; local builds need the native packages listed above.

Linux screen-capture bindings require PipeWire development headers version 1.0 or newer and
SPA development headers implementing the stable 0.2 API. The PipeWire and SPA
`pkg-config` package versions are different: `libpipewire-0.3` tracks the PipeWire
release, while `libspa-0.2` reports the SPA API version. Ubuntu 22.04 ships older
PipeWire headers, so use Ubuntu 24.04 or a distribution with compatible development
packages. Linux release packages are built on Ubuntu 24.04 and may not run on older
distributions; test downloads on the systems you intend to support.

## Download builds from GitHub Actions

`.github/workflows/build-downloads.yml` runs **only manually**. After pushing it
to the repository's default branch:

1. Open GitHub → **Actions** → **Build downloads** → **Run workflow**.
2. Select the branch and start the run.
3. Open the completed run and download the per-platform ZIPs under **Artifacts**.

It builds macOS ARM64, Windows AMD64/ARM64, and Linux AMD64/ARM64. AMD64 means
x86-64 on both Intel and AMD CPUs. Linux AMD64 includes an AppImage and Debian
package; Linux ARM64 uses a Debian package. See [Releases](releasing.md) for
artifact contents, signing limitations and retention. This workflow neither
creates nor publishes a GitHub Release and needs no signing secrets. The Linux
build uses Ubuntu 24.04 for the PipeWire 1.0 and SPA 0.2 headers required by the
Rust bindings; older Linux distributions may not run the resulting packages.

## Development

```sh
git clone https://github.com/DevLeashed/OpenLeash.git
cd OpenLeash
npm install
npm run tauri dev
```

## Production build

```sh
npm install
npm run tauri build
```

This produces the bundled installers and bare executable under
`src-tauri/target/release/`. For reproducible CI-style installs, use `npm ci` after
cloning a checkout with `package-lock.json`; `npm install` is also fine for local
development.


## Repository scripts

Conveniences for the maintainer, not part of the normal workflow. Two are
Windows-only, one only runs inside a container, one only on Apple Silicon.

| Script | What it does | Runs on |
|---|---|---|
| `build.bat` | **Fast dev build, not a release build.** `--debug` (skips release LTO and `codegen-units=1`), `--no-bundle` (no installer), `--config tauri.fast.conf.json` (skips the `tsc` typecheck). Copies the portable exe to `compiled/`. | Windows |
| `run.bat` | `npm run tauri dev`. | Windows |
| `refresh_icon.bat` | `npm run tauri icon src-tauri/icons/icon.png` — regenerates every platform icon set from the source PNG. | Windows |
| `publish.bat [strict]` | Maintainer cross-target build into `published/`. Windows targets produce an NSIS installer and portable `.exe`; Linux runs in Docker/Podman and produces an x64 AppImage or ARM64 raw ELF. This is not the GitHub release workflow and does not provide a signed installer or Linux packages. `strict` makes skipped targets fail the script. | Windows + Docker (or Podman) |
| `docker/build-linux.sh` | The in-container Linux build that `publish.bat` drives. Installs the system deps, then `npm install` and `tauri build`. Builds ARM under QEMU rather than cross-compiling, because Debian cannot supply arm64 WebKitGTK dev packages. | Linux container only |
| `publish-macos.sh` | Builds the Apple Silicon binary, which cannot be cross-compiled: the macOS SDK is not redistributable and `codesign` runs only on macOS. Produces `published/openleash-macos-arm64`. | macOS, Apple Silicon |

None of these hardcode a developer-local absolute path — they all resolve from
`%~dp0` or `$(dirname "$0")`. They do assume the toolchains named above.

## Signing

`publish.bat` and `publish-macos.sh` produce **unsigned** binaries. See
[Releases](releasing.md) for what that means for Windows SmartScreen and macOS
Gatekeeper, and what a signed release requires.

## Checks

```sh
npm run check                           # tsc --noEmit + eslint + vitest
npm run build                           # tsc + production Vite bundle
cd src-tauri && cargo fmt --all --check # Rust formatting gate
cd src-tauri && cargo test --locked     # Rust tests
```

CI also runs `npm audit --audit-level=high`, a pinned RustSec audit, and `npx eslint . --max-warnings 89`. Clippy is report-only in CI (`continue-on-error`) while existing warnings are addressed; it is useful locally but not a merge gate. See `.github/workflows/ci.yml` for the authoritative checks and current lint budget.
