"""Stage one release-named file per Actions artifact, without extra ZIP contents."""

import json
import os
from pathlib import Path
import shutil

from updater_assets import UPDATERS, asset_name, asset_url, release_identity, signature_text


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


def stage_downloads(release, stage, label, version, build_updater=False, release_tag=""):
    tag = release_identity(version, release_tag)
    downloads = list(DOWNLOADS[label])
    updater = UPDATERS.get(label) if build_updater else None
    if updater:
        _, pattern, suffix = updater
        if (pattern, suffix) not in downloads:
            downloads.append((pattern, suffix))
        downloads.append((pattern + ".sig", suffix + ".sig"))
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

    fragment = None
    if updater:
        platform, _, suffix = updater
        signature = signature_text(sources[-1][0])
        fragment = {
            "version": version,
            "release_tag": tag,
            "platforms": {platform: {
                "url": asset_url(tag, asset_name(version, suffix)),
                "signature": signature,
            }},
        }

    stage.mkdir(exist_ok=False)
    names = []
    for source, name in sources:
        shutil.copy2(source, stage / name)
        names.append(name)
    if fragment:
        name = f"updater-{label}.json"
        (stage / name).write_text(json.dumps(fragment, indent=2) + "\n", encoding="utf-8")
        names.append(name)
    return names


def main():
    version = json.loads(Path("package.json").read_text(encoding="utf-8"))["version"]
    release = Path("src-tauri/target") / os.environ["BUILD_TARGET"] / "release"
    names = stage_downloads(
        release, Path("downloads"), os.environ["BUILD_LABEL"], version,
        os.environ.get("BUILD_UPDATER") == "true", os.environ.get("RELEASE_TAG", ""),
    )
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for index, name in enumerate(names, start=1):
            output.write(f"file_{index}={name}\n")


if __name__ == "__main__":
    main()
