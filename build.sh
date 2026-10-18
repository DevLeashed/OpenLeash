#!/usr/bin/env bash
#
# build.sh - build OpenLeash natively on Linux ARM64 (aarch64), the Linux
# counterpart of build.bat. Output is a bare executable copied to ./compiled/.
#
#   usage: ./build.sh [--release]
#     (default)   fast debug build: --debug, --no-bundle, tauri.fast.conf.json
#                 (skips the tsc typecheck), same as build.bat
#     --release   optimised build (LTO, codegen-units=1, stripped) with the
#                 typecheck. Slow on a small board; use it for a binary you
#                 intend to run long-term.
#
# WHY NATIVE, NOT CROSS-COMPILED
#   Tauri links against WebKitGTK 4.1, and Debian/Ubuntu cannot install the
#   arm64 -dev packages on an x86 host. Build on the ARM machine itself. See
#   docs/building.md.
#
# WHAT THIS DOES NOT DO
#   It does not install system packages (that needs sudo, so it only reports
#   what is missing and prints the apt command), and it does not sign, publish
#   or bundle anything. For a one-shot setup on a bare Ubuntu 24.04 ARM box that
#   also installs Rust and the apt packages, use build-linux-arm64.sh instead.
#
# PREREQUISITES
#   - aarch64 Linux (Ubuntu 24.04+ recommended: PipeWire 1.0 / SPA 0.2 headers)
#   - Node.js ^20.19.0 or ^22.12.0, npm
#   - Rust stable (rustup), cargo
#   - WebKitGTK 4.1, GTK 3, PipeWire and SPA -dev packages (checked below)
#
set -euo pipefail

cd "$(dirname "$0")"

MODE=fast
case "${1:-}" in
  "")         ;;
  --release)  MODE=release ;;
  -h|--help)  sed -n '2,20p' "$0"; exit 0 ;;
  *)          echo "ERROR: unknown argument '$1'. Usage: $0 [--release]" >&2; exit 1 ;;
esac

NAME=openleash
OUT_DIR=compiled
OUT_NAME="$NAME-linux-arm64"

# tauri/esbuild --ci only parses a real boolean; CI=1 is not one.
export CI=true

echo
echo "============================================================"
echo " OpenLeash build - Linux ARM64 ($MODE)"
echo "============================================================"
echo

# ---------------------------------------------------------------------------
# Host checks. Fail here rather than halfway through a long Rust build.
# ---------------------------------------------------------------------------
if [[ "$(uname -s)" != "Linux" ]]; then
  echo "ERROR: this script only runs on Linux. You are on $(uname -s)." >&2
  echo "       Use build.bat on Windows." >&2
  exit 1
fi

case "$(uname -m)" in
  aarch64|arm64) ;;
  *)
    echo "ERROR: this script targets a native ARM64 host, but uname -m is '$(uname -m)'." >&2
    echo "       The WebKitGTK side cannot be cross-compiled from x86. Build on an" >&2
    echo "       ARM64 machine, or use docker/build-linux.sh under QEMU via publish.bat." >&2
    exit 1
    ;;
esac

if [[ -r /etc/os-release ]]; then
  # Source in a subshell: os-release defines NAME (e.g. "Debian GNU/Linux"),
  # which would silently overwrite this script's NAME=openleash and make the
  # binary lookup fail after a successful build.
  # shellcheck disable=SC1091
  echo ">>> host: $(. /etc/os-release; echo "${PRETTY_NAME:-unknown}") ($(uname -m))"
fi

missing=0
for tool in node npm cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "ERROR: '$tool' not found." >&2
    case "$tool" in
      cargo) echo "       Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2 ;;
      *)     echo "       Install Node 22 LTS from https://nodejs.org (or your package manager)." >&2 ;;
    esac
    missing=1
  fi
done
[[ $missing -eq 0 ]] || exit 1

echo ">>> node $(node -v), npm $(npm -v), $(rustc --version 2>/dev/null || echo 'rustc not found')"
case "$(node -p 'process.versions.node.split(".")[0]')" in
  20|22) ;;
  *) echo "WARNING: package.json supports Node ^20.19.0 || ^22.12.0; this build may fail on $(node -v)." >&2 ;;
esac

# ---------------------------------------------------------------------------
# Native libraries. pkg-config tells us exactly what the Rust -sys crates will
# look for, so a missing package is reported by name, not as a linker error.
# ---------------------------------------------------------------------------
if ! command -v pkg-config >/dev/null 2>&1; then
  echo "ERROR: pkg-config not found. Install it first." >&2
  echo "       sudo apt-get install -y pkg-config build-essential" >&2
  exit 1
fi

REQUIRED_PC=(
  "webkit2gtk-4.1:libwebkit2gtk-4.1-dev"
  "gtk+-3.0:libgtk-3-dev"
  "librsvg-2.0:librsvg2-dev"
  "openssl:libssl-dev"
  "libpipewire-0.3:libpipewire-0.3-dev"
  "libspa-0.2:libpipewire-0.3-dev"
)
MISSING_PKGS=()
for entry in "${REQUIRED_PC[@]}"; do
  pc="${entry%%:*}"
  deb="${entry##*:}"
  if ! pkg-config --exists "$pc"; then
    echo "ERROR: pkg-config cannot find '$pc'." >&2
    MISSING_PKGS+=("$deb")
  fi
done

if (( ${#MISSING_PKGS[@]} > 0 )); then
  # De-duplicate (libpipewire-0.3-dev is listed for two .pc files).
  mapfile -t UNIQUE < <(printf '%s\n' "${MISSING_PKGS[@]}" | sort -u)
  echo >&2
  echo "Install the missing development packages and re-run:" >&2
  echo "  sudo apt-get update && sudo apt-get install -y --no-install-recommends \\" >&2
  echo "    build-essential curl ca-certificates file pkg-config patchelf \\" >&2
  echo "    libayatana-appindicator3-dev libxdo-dev libdbus-1-dev libxkbcommon-dev \\" >&2
  echo "    ${UNIQUE[*]}" >&2
  echo >&2
  echo "(build-linux-arm64.sh installs the full set in one go.)" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Frontend dependencies. npm ci is reproducible when the lockfile is present.
# ---------------------------------------------------------------------------
echo ">>> frontend dependencies"
if [[ -f package-lock.json ]]; then
  npm ci --no-audit --no-fund
else
  npm install --no-audit --no-fund
fi

# ---------------------------------------------------------------------------
# Build. No --target: the host triple is the target, and the output lands in
# src-tauri/target/<mode>/ (not target/<triple>/), matching build.bat.
# ---------------------------------------------------------------------------
# Ask cargo where it really puts output: CARGO_TARGET_DIR or a cargo config
# `build.target-dir` relocates it, and a hard-coded src-tauri/target/ would then
# point at nothing.
TARGET_DIR="$(cd src-tauri && cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(JSON.parse(s).target_directory)}catch(e){}})')"
[[ -n "$TARGET_DIR" ]] || TARGET_DIR="$PWD/src-tauri/target"

# run_with_progress <cmd...>: run a long build and, every 30s, print what it is
# doing. The last cargo step ("Compiling openleash") is one rustc process that
# prints nothing for many minutes, which looks like a hang; this shows elapsed
# time, the busiest rustc/linker process and free memory. It also names the
# usual silent killer on small boards - the kernel OOM killer - on failure.
run_with_progress() {
  local start=$SECONDS hb rc=0
  (
    while sleep 30; do
      el=$((SECONDS - start))
      mem=$(awk '/MemAvailable/ {printf "%d MiB free", $2/1024}' /proc/meminfo 2>/dev/null || echo "?")
      proc=$(ps -eo rss,etimes,comm --sort=-rss 2>/dev/null \
        | awk '$3 ~ /^(rustc|cc|ld|lld|collect2|node|cargo)/ {printf "%s (%d MiB RSS, %ss)", $3, $1/1024, $2; exit}')
      printf '>>> [%dm%02ds] still building - %s%s\n' $((el/60)) $((el%60)) "$mem" "${proc:+ - busiest: $proc}"
    done
  ) &
  hb=$!
  "$@" || rc=$?
  kill "$hb" 2>/dev/null || true
  wait "$hb" 2>/dev/null || true
  if (( rc != 0 )); then
    echo "ERROR: build failed (exit $rc) after $(( (SECONDS - start) / 60 ))m$(( (SECONDS - start) % 60 ))s." >&2
    if (( rc == 137 || rc == 143 )) || dmesg 2>/dev/null | tail -50 | grep -qi 'out of memory\|oom-kill'; then
      echo "       Looks like the kernel killed it for lack of RAM. Add swap, close other" >&2
      echo "       apps, or limit parallelism:  CARGO_BUILD_JOBS=2 ./build.sh" >&2
    fi
    exit "$rc"
  fi
  echo ">>> build step finished in $(( (SECONDS - start) / 60 ))m$(( (SECONDS - start) % 60 ))s"
}

# Live cargo progress bar even when not on a TTY, and --verbose so Tauri prints
# each phase (frontend build, cargo invocation, binary location).
export CARGO_TERM_PROGRESS_WHEN=always CARGO_TERM_PROGRESS_WIDTH=80

if [[ "$MODE" == "release" ]]; then
  echo ">>> tauri build --release (LTO, codegen-units=1 - slow)"
  run_with_progress npx tauri build --verbose --no-bundle --ci
  BUILT="$TARGET_DIR/release/$NAME"
else
  echo ">>> tauri build --debug (fast; typecheck skipped, see tauri.fast.conf.json)"
  run_with_progress npx tauri build --verbose --debug --no-bundle --ci --config src-tauri/tauri.fast.conf.json
  BUILT="$TARGET_DIR/debug/$NAME"
fi

if [[ ! -f "$BUILT" ]]; then
  echo "ERROR: expected binary not found at $BUILT" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
cp -f "$BUILT" "$OUT_DIR/$OUT_NAME"
chmod +x "$OUT_DIR/$OUT_NAME"
# Also publish it under the plain name, like build.bat's compiled/openleash.exe.
cp -f "$BUILT" "$OUT_DIR/$NAME"
chmod +x "$OUT_DIR/$NAME"

echo
echo "============================================================"
echo " Done. Artifacts in ./$OUT_DIR/"
echo "============================================================"
ls -la "$OUT_DIR/$OUT_NAME" "$OUT_DIR/$NAME"
echo
echo " Run it with: ./$OUT_DIR/$OUT_NAME"
echo
