#!/usr/bin/env bash
#
# buildx.sh - build the native x86_64 Linux executable into ./compiled/.
#
#   usage: ./buildx.sh
#
# Produces compiled/openleash-linux-x64 and compiled/openleash. This is a
# release-mode executable build without installer bundles. It installs the
# required Ubuntu packages and Rust stable if missing; Node.js/npm must already
# be installed. Ubuntu 24.04 or newer is recommended for PipeWire 1.0 / SPA 0.2.
#
set -euo pipefail

cd "$(dirname "$0")"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,10p' "$0"
  exit 0
elif [[ $# -gt 0 ]]; then
  echo "ERROR: unknown argument '$1'. Usage: $0" >&2
  exit 1
fi

TARGET=x86_64-unknown-linux-gnu
NAME=openleash
OUT_DIR=compiled
OUT_NAME="$NAME-linux-x64"

# Tauri/esbuild --ci only parses a real boolean; CI=1 is not one.
export CI=true
export DEBIAN_FRONTEND=noninteractive

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "ERROR: this script only runs on Linux (found $(uname -s))." >&2
  exit 1
fi
case "$(uname -m)" in
  x86_64|amd64) ;;
  *)
    echo "ERROR: this script targets a native x86_64 host (found $(uname -m))." >&2
    exit 1
    ;;
esac

if [[ $EUID -eq 0 ]]; then
  SUDO=()
elif command -v sudo >/dev/null 2>&1; then
  SUDO=(sudo)
else
  echo "ERROR: sudo is required to install Linux build dependencies." >&2
  exit 1
fi

echo ">>> installing Linux build dependencies"
"${SUDO[@]}" apt-get update -qq
"${SUDO[@]}" apt-get install -y -qq --no-install-recommends \
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
  libspa-0.2-dev \
  clang \
  libclang-dev \
  libgbm-dev \
  libxcb1-dev \
  libxcb-randr0-dev \
  libxrandr-dev \
  libxi-dev \
  libxtst-dev \
  patchelf

if ! command -v cargo >/dev/null 2>&1; then
  echo ">>> installing Rust stable (rustup)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

if ! command -v npm >/dev/null 2>&1; then
  echo "ERROR: Node.js/npm not found. Install Node ^20.19.0 or ^22.12.0 from" >&2
  echo "       https://nodejs.org, then rerun this script." >&2
  exit 1
fi

echo ">>> node $(node -v), npm $(npm -v), $(rustc --version)"

# Reproducible frontend install when the lockfile is present.
echo ">>> frontend dependencies"
if [[ -f package-lock.json ]]; then
  npm ci --no-audit --no-fund
else
  npm install --no-audit --no-fund
fi

echo ">>> building x86_64 Linux executable (release, no bundles)"
npx tauri build --target "$TARGET" --no-bundle --ci

BUILT="src-tauri/target/$TARGET/release/$NAME"
if [[ ! -f "$BUILT" ]]; then
  echo "ERROR: expected executable not found at $BUILT" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
cp -f "$BUILT" "$OUT_DIR/$OUT_NAME"
cp -f "$BUILT" "$OUT_DIR/$NAME"
chmod +x "$OUT_DIR/$OUT_NAME" "$OUT_DIR/$NAME"

echo
echo ">>> executable built:"
ls -lh "$OUT_DIR/$OUT_NAME" "$OUT_DIR/$NAME"
echo "    Run with: ./$OUT_DIR/$OUT_NAME"
