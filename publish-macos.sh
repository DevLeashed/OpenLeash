#!/usr/bin/env bash
#
# publish-macos.sh - build the one binary publish.bat cannot make for you.
#
# WHY THIS IS A SEPARATE SCRIPT
#   The macOS binary is a Mach-O executable linked against the macOS SDK and
#   signed with Apple's codesign. The SDK cannot be redistributed and codesign
#   does not run on any other OS, so there is no cross-compiler for it. The
#   binary has to be produced on a real Mac, which is why publish.bat reports
#   macOS as SKIPPED instead of pretending.
#
#   Windows users: clone this repo on your Mac, run this script, then copy
#   published/openleash-macos-arm64 back to the Windows machine.
#
# PREREQUISITES ON THE MAC
#   1. Xcode command line tools:      xcode-select --install
#   2. Rust (rustup):                 curl --proto '=https' --tlsv1.2 https://sh.rustup.rs -sSf | sh
#   3. Node.js 20+ from https://nodejs.org
#   4. This rust target:              rustup target add aarch64-apple-darwin
#
# OUTPUT
#   published/openleash-macos-arm64
#
#   It is a single self-contained executable: the Rust binary with the frontend
#   already embedded. It is NOT a .app bundle and NOT a .dmg, because both of
#   those are directory/structures rather than one file. Running it directly is
#   fine, but macOS Gatekeeper will quarantine the unsigned file, so the first
#   launch needs a right-click > Open (or: xattr -d com.apple.quarantine
#   published/openleash-macos-arm64).
#
set -euo pipefail

cd "$(dirname "$0")"

OUT=published
NAME=openleash

# --ci wants a parseable boolean; CI=1 is not one.
export CI=true

echo
echo "============================================================"
echo " OpenLeash publish - macOS Apple Silicon"
echo "============================================================"
echo

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "ERROR: this script only runs on macOS. You are on $(uname -s)." >&2
  echo "       The macOS binary cannot be built anywhere else." >&2
  exit 1
fi

if ! command -v xcode-select >/dev/null 2>&1; then
  echo "ERROR: Xcode command line tools missing. Run: xcode-select --install" >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found. Install rustup from https://rustup.rs" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "ERROR: npm not found. Install Node.js 20+ from https://nodejs.org" >&2
  exit 1
fi

mkdir -p "$OUT"

if ! rustup target list --installed | grep -qx "aarch64-apple-darwin"; then
  echo "  adding rust target aarch64-apple-darwin"
  rustup target add aarch64-apple-darwin
fi

# Release, no bundler. --bundles is not passed because the .app wrapper and the
# DMG are multi-file artefacts; we only want the single executable.
echo "  building (this takes a few minutes on first run)..."
npm install
npx tauri build \
  --target aarch64-apple-darwin \
  --no-bundle \
  --ci

BUILT="src-tauri/target/aarch64-apple-darwin/release/$NAME"
if [[ ! -f "$BUILT" ]]; then
  echo "ERROR: expected binary not found at $BUILT" >&2
  exit 1
fi

cp -f "$BUILT" "$OUT/$NAME-macos-arm64"
chmod +x "$OUT/$NAME-macos-arm64"

echo
echo "============================================================"
echo " published/"
echo "============================================================"
for f in "$OUT"/*; do
  [[ -e "$f" ]] || continue
  printf "   %-34s %s bytes\n" "$(basename "$f")" "$(stat -f%z "$f")"
done
echo
echo " macOS build is unsigned. First launch: right-click > Open,"
echo " or run:  xattr -d com.apple.quarantine $OUT/$NAME-macos-arm64"
echo
echo " Copy that file back to the Windows machine to complete the set."
echo
