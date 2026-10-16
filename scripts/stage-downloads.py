"""Stage one release-named file per Actions artifact, without extra ZIP contents."""

import json
import os
from pathlib import Path
import shutil

from release_assets import INSTALLER_BUNDLES, asset_name, release_identity


DOWNLOADS = {
    "windows-x64": [("openleash.exe", "win_x64.exe")],
    "windows-arm64": [("openleash.exe", "win_arm64.exe")],
    "macos-arm64": [("bundle/dmg/*.dmg", "macos_aarch64.dmg")],
    "linux-x64": [
        ("bundle/appimage/*.AppImage", "linux_amd64.AppImage"),
        ("bundle/deb/*.deb", "linux_amd64.deb"),
    ],
    "linux-arm64": [("bundle/deb/*.deb", "linux_arm64.deb")],
}


def stage_downloads(release, stage, label, version, build_installers=False):
    # Refuse to name assets after anything that is not a canonical version.
    release_identity(version)
    downloads = list(DOWNLOADS[label])
    installer = INSTALLER_BUNDLES.get(label) if build_installers else None
    if installer and installer not in downloads:
        downloads.append(installer)
    # Validate every source before staging, rather than publishing a partial set.
    sources = []
    for pattern, suffix in downloads:
        matches = sorted(release.glob(pattern))
        if len(matches) != 1:
            raise RuntimeError(f"Expected exactly one download: {release / pattern}; found {len(matches)}")
        source = matches[0]
        if not source.is_file() or source.stat().st_size == 0:
            raise RuntimeError(f"Invalid download: {source}")
        sources.append((source, asset_name(version, suffix)))

    stage.mkdir(exist_ok=False)
    names = []
    for source, name in sources:
        shutil.copy2(source, stage / name)
        names.append(name)
    return names


def main():
    version = json.loads(Path("package.json").read_text(encoding="utf-8"))["version"]
    release = Path("src-tauri/target") / os.environ["BUILD_TARGET"] / "release"
    names = stage_downloads(
        release, Path("downloads"), os.environ["BUILD_LABEL"], version,
        os.environ.get("BUILD_INSTALLERS") == "true",
    )
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for index, name in enumerate(names, start=1):
            output.write(f"file_{index}={name}\n")


if __name__ == "__main__":
    main()
