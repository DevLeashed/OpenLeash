"""Build the in-app updater manifest (latest.json) from release asset names.

The app (src-tauri/src/updates.rs) reads latest.json from the latest GitHub
release, pins every download URL to this repository's release assets, and
installs only on an explicit user action. Only formats the updater plugin can
apply in place are listed: the Windows NSIS installer and the Linux AppImage.
The macOS DMG is deliberately absent (the plugin needs an .app.tar.gz), so Mac
users keep getting the "Open latest release" link. `signature` is "" because
the plugin requires the field while the app does not verify it.

Usage: python scripts/assemble-updater.py <version> [tag] > latest.json
"""

import json
import sys
from datetime import datetime, timezone

from release_assets import asset_name, release_identity

REPO = "DevLeashed/OpenLeash"
PLATFORMS = {
    "windows-x86_64": "win_x64_setup.exe",
    "windows-aarch64": "win_arm64_setup.exe",
    "linux-x86_64": "linux_amd64.AppImage",
}


def manifest(version, tag="", pub_date=None):
    tag = release_identity(version, tag)
    base = f"https://github.com/{REPO}/releases/download/{tag}/"
    return {
        "version": version,
        "pub_date": pub_date or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": {
            key: {"url": base + asset_name(version, suffix), "signature": ""}
            for key, suffix in PLATFORMS.items()
        },
    }


if __name__ == "__main__":
    args = sys.argv[1:]
    if not 1 <= len(args) <= 2:
        sys.exit(__doc__)
    print(json.dumps(manifest(*args), indent=2))
