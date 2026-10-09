#!/usr/bin/env bash
#
# Runs INSIDE a Linux container. Driven by publish.bat - not meant to be run by
# hand outside a throwaway container.
#
#   usage: build-linux.sh <rust-target-triple> <appimage|raw>
#
# ---------------------------------------------------------------------------
# WHY A CONTAINER
#   A Linux binary is only as portable as the glibc it was linked against.
#   Tauri v2 needs WebKitGTK 4.1, which rules out the very old distros, so
#   Debian 12 (bookworm) is the oldest base that still works. node:20-bookworm
#   is exactly that base with Node already in it.
#
#   There is no separate "Debian build" and no "Fedora build". Building against
#   Fedora's newer glibc would only raise the minimum glibc the binary needs,
#   making it refuse to start on older systems. One low-glibc binary per
#   architecture runs on Debian, Ubuntu, Fedora and Arch alike.
#
# ---------------------------------------------------------------------------
# WHY ARM IS BUILT NATIVELY UNDER QEMU, NOT CROSS-COMPILED
#   The obvious approach - `dpkg --add-architecture arm64`, then install
#   libwebkit2gtk-4.1-dev:arm64 - does not work, and not because of anything
#   under our control. Debian bookworm ships python3-mako and python3-markdown
#   as Architecture: all, so `python3-mako:arm64` has no candidate at all, and
#   gobject-introspection:arm64 (pulled in by WebKitGTK's -dev package) depends
#   on it. apt then rejects the whole transaction:
#
#       E: Unable to correct problems, you have held broken packages.
#       gobject-introspection:arm64 : Depends: python3-mako:arm64 but it is
#                                     not installable
#
#   This is a known upstream packaging gap. Tauri's own maintainer confirms on
#   tauri-apps/tauri discussion #13246 that the multi-arch recipe does not
#   work and that ARM builds have to happen on ARM. Docker can do that for us
#   by emulating an aarch64 CPU: slow, but the build runs against a genuine
#   arm64 userland where every -dev package resolves normally.
# ---------------------------------------------------------------------------
set -euo pipefail

TARGET="${1:?target triple required}"
MODE="${2:?mode required (appimage|raw)}"

# tauri/esbuild --ci only parses a real boolean; CI=1 is not one.
export CI=true
export DEBIAN_FRONTEND=noninteractive

case "$TARGET" in
  x86_64-unknown-linux-gnu) NATIVE=x86_64 ;;
  aarch64-unknown-linux-gnu) NATIVE=aarch64 ;;
  *) echo "unsupported target: $TARGET" >&2; exit 1 ;;
esac

HOST_ARCH="$(uname -m)"
CROSSED=0
[[ "$HOST_ARCH" != "$NATIVE" ]] && CROSSED=1

echo ">>> container arch=$HOST_ARCH target=$TARGET cross=$CROSSED"
if [[ $CROSSED -eq 1 ]]; then
  echo ">>> emulating a $NATIVE CPU. Slow - expect 30-60 min - but it is the"
  echo ">>> only way to get a real arm64 WebKitGTK to link against."
fi

echo ">>> system dependencies"
apt-get update -qq
# libpipewire-0.3-dev is not in Tauri's documented list, but the xcap crate
# (screen capture) pulls in libspa-sys, which links PipeWire. Without it the
# build dies with "The system library `libpipewire-0.3` required by crate
# `libspa-sys` was not found".
apt-get install -y -qq --no-install-recommends \
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
  patchelf \
  fuse3 \
  >/dev/null

# appimagetool shells out to FUSE to squash the AppImage. Containers have no
# FUSE device, so make it extract and run instead of mounting.
export APPIMAGE_EXTRACT_AND_RUN=1

if ! command -v cargo >/dev/null 2>&1; then
  echo ">>> installing rust"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

# On a native arch there is no cross target to add, and asking rustup for one
# would fail, so only add it when we really are crossing.
if [[ $CROSSED -eq 1 ]]; then
  rustup target add "$TARGET"
fi

cd /app

echo ">>> npm install"
npm install --no-audit --no-fund

echo ">>> tauri build (lto=true, codegen-units=1 - the slow part)"
if [[ "$MODE" == "appimage" ]]; then
  # linuxdeploy ships x86_64 only, so an AppImage can only be produced on an
  # x86_64 host. Callers only ask for appimage on x86_64.
  npx tauri build --target "$TARGET" --bundles appimage --ci
else
  npx tauri build --target "$TARGET" --no-bundle --ci
fi

echo ">>> done"
ls -la "src-tauri/target/$TARGET/release/" 2>/dev/null | head -20 || true
