#!/usr/bin/env bash
#
# build-linux-arm64.sh - native ARM64 (aarch64) Ubuntu build of OpenLeash.
#
# WHAT THIS IS FOR
#   A one-shot build on a real aarch64 Ubuntu machine (or an aarch64 VM) that
#   has no build tooling installed yet: it installs the system dependencies,
#   the Rust toolchain and Node if they are missing, then runs the normal
#   `tauri build`. It does not push, publish, sign or install anything.
#
# WHY THIS RUNS NATIVELY ON ARM RATHER THAN CROSS-COMPILING
#   Tauri v2 links against WebKitGTK 4.1. Debian/Ubuntu cannot supply the arm64
#   -dev packages for a foreign architecture: gobject-introspection:arm64 pulls
#   python3-mako:arm64, which is Architecture: all and therefore has no
#   candidate, so apt rejects the whole transaction. The webview side cannot be
#   cross-built. To produce an ARM package from an x86 host, build under QEMU
#   with docker/build-linux.sh (driven by publish.bat) instead of this script.
#
# PREREQUISITES
#   - Ubuntu 24.04 (aarch64). Older releases ship PipeWire < 1.0 and SPA < 0.2
#     headers; the screen-capture bindings will not compile against them.
#   - sudo, for the apt step.
#   - Network access for apt, rustup and npm.
#
# OUTPUT
#   src-tauri/target/aarch64-unknown-linux-gnu/release/openleash   (bare binary)
#   src-tauri/target/aarch64-unknown-linux-gnu/release/bundle/deb/*.deb
#
# usage: ./build-linux-arm64.sh [--no-bundle]
#   --no-bundle   skip the .deb and only produce the bare executable
#
set -euo pipefail

cd "$(dirname "$0")"

BUNDLES="deb"
if [[ "${1:-}" == "--no-bundle" ]]; then
  BUNDLES=""
elif [[ -n "${1:-}" ]]; then
  echo "ERROR: unknown argument '$1'. Usage: $0 [--no-bundle]" >&2
  exit 1
fi

TARGET=aarch64-unknown-linux-gnu

# tauri/esbuild --ci only parses a real boolean; CI=1 is not one.
export CI=true
export DEBIAN_FRONTEND=noninteractive

# ---------------------------------------------------------------------------
# Fail early and loudly on the wrong machine / OS. This script is deliberately
# not a cross-build driver; pointing it at the wrong host should stop here
# rather than produce a broken or mis-linked binary.
# ---------------------------------------------------------------------------
if [[ "$(uname -m)" != "aarch64" ]]; then
  echo "ERROR: this script targets a native aarch64 host, but uname -m is" >&2
  echo "       '$(uname -m)'. Cross-compiling the WebKitGTK side is not" >&2
  echo "       possible; use docker/build-linux.sh under QEMU from x86 instead." >&2
  exit 1
fi

if [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  echo ">>> host: ${PRETTY_NAME:-unknown} ($(uname -m))"
  if [[ "${ID:-}" == "ubuntu" && "${VERSION_ID:-0}" < "24.04" ]]; then
    echo "WARNING: Ubuntu ${VERSION_ID} ships PipeWire/SPA headers older than" >&2
    echo "         1.0/0.2. The build will likely fail; 24.04+ is required." >&2
  fi
fi

# ---------------------------------------------------------------------------
# System dependencies. Same set the container build installs; libpipewire-0.3-dev
# is not in Tauri's documented list but the xcap crate (screen capture) pulls in
# libspa-sys, which links PipeWire.
# ---------------------------------------------------------------------------
echo ">>> system dependencies (sudo)"
sudo apt-get update -qq
sudo apt-get install -y -qq --no-install-recommends \
  build-essential \
  curl \
  ca-certificates \
  file \
  pkg-config \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libssl-dev \
  libxdo-dev \
  libdbus-1-dev \
  libxkbcommon-dev \
  libpipewire-0.3-dev \
  patchelf

# ---------------------------------------------------------------------------
# Rust (rustup) and Node, only if missing. On a native arch there is no cross
# target to add, so the target triple is already installed.
# ---------------------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  echo ">>> installing Rust (rustup, stable)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

if ! command -v npm >/dev/null 2>&1; then
  echo "ERROR: Node.js/npm not found. Install Node 20 or 22 from" >&2
  echo "       https://nodejs.org (the repo supports ^20.19.0 || ^22.12.0)." >&2
  exit 1
fi
echo ">>> node $(node -v), rust $(rustc --version)"

# ---------------------------------------------------------------------------
# Build. `npm ci` when the lockfile is present (reproducible), else fall back.
# ---------------------------------------------------------------------------
echo ">>> frontend dependencies"
if [[ -f package-lock.json ]]; then
  npm ci --no-audit --no-fund
else
  npm install --no-audit --no-fund
fi

echo ">>> tauri build (release LTO - the slow part)"
if [[ -n "$BUNDLES" ]]; then
  npx tauri build --target "$TARGET" --bundles "$BUNDLES" --ci
else
  npx tauri build --target "$TARGET" --no-bundle --ci
fi

OUT="src-tauri/target/$TARGET/release"
echo
echo ">>> done. artefacts:"
ls -la "$OUT/openleash" 2>/dev/null || true
ls -la "$OUT/bundle/deb/"*.deb 2>/dev/null || true
