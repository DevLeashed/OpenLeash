"""Release asset names, installer bundles and release identity validation (no network access)."""

import re


# A leg only builds these bundles when the run asks for installers (see the
# workflow). The Windows legs otherwise ship the portable executable alone, so
# the NSIS installer is the one artifact this map adds. Nothing here is signed:
# this workflow publishes no updater signature or manifest.
INSTALLER_BUNDLES = {
    "windows-x64": ("bundle/nsis/*.exe", "win_x64_setup.exe"),
    "windows-arm64": ("bundle/nsis/*.exe", "win_arm64_setup.exe"),
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
