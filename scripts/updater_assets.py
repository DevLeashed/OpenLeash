"""Shared updater asset names and release identity validation (no network access)."""

import re
from urllib.parse import quote


UPDATERS = {
    "windows-x64": ("windows-x86_64", "bundle/nsis/*.exe", "win_x64_setup.exe"),
    "windows-arm64": ("windows-aarch64", "bundle/nsis/*.exe", "win_arm64_setup.exe"),
    "macos-arm64": ("darwin-aarch64", "bundle/macos/*.app.tar.gz", "macos_aarch64.app.tar.gz"),
    "linux-x64": ("linux-x86_64", "bundle/appimage/*.AppImage", "linux_amd64.AppImage"),
}
NUMERIC = r"(?:0|[1-9][0-9]*)"
PRERELEASE = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = re.compile(rf"{NUMERIC}\.{NUMERIC}\.{NUMERIC}(?:-{PRERELEASE}(?:\.{PRERELEASE})*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?")


def release_identity(version, tag=""):
    if not isinstance(version, str) or not SEMVER.fullmatch(version):
        raise ValueError("Expected a canonical SemVer version")
    tag = tag or version
    # Historical releases use no v prefix; permit either convention explicitly,
    # but never a different version, path separator, whitespace or URL injection.
    if tag not in (version, f"v{version}"):
        raise ValueError("Release tag must equal the version or v plus the version")
    return tag


def asset_name(version, suffix):
    return f"OpenLeash_{version}_{suffix}"


def asset_url(tag, name):
    return f"https://github.com/DevLeashed/OpenLeash/releases/download/{quote(tag, safe='')}/{quote(name, safe='')}"


def signature_text(path):
    if not path.is_file() or path.stat().st_size == 0:
        raise RuntimeError(f"Missing or empty signature: {path}")
    signature = path.read_text(encoding="utf-8").strip()
    if not signature:
        raise RuntimeError(f"Empty signature: {path}")
    return signature
