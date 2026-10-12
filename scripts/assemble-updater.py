"""Assemble latest.json from extracted single-file Actions artifacts; never publish."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

from updater_assets import UPDATERS, asset_name, asset_url, release_identity, signature_text


def one_file(root, name):
    matches = list(root.rglob(name))
    if len(matches) != 1 or not matches[0].is_file() or matches[0].stat().st_size == 0:
        raise RuntimeError(f"Expected exactly one nonempty {name}; found {len(matches)}")
    return matches[0]


def assemble(root, version, tag="", notes="", pub_date=None):
    tag = release_identity(version, tag)
    platforms = {}
    for label, (platform, _, suffix) in UPDATERS.items():
        name = asset_name(version, suffix)
        one_file(root, name)
        signature = signature_text(one_file(root, name + ".sig"))
        expected = {"url": asset_url(tag, name), "signature": signature}
        fragment = json.loads(one_file(root, f"updater-{label}.json").read_text(encoding="utf-8"))
        if fragment != {"version": version, "release_tag": tag, "platforms": {platform: expected}}:
            raise RuntimeError(f"Updater fragment does not match release assets: {label}")
        platforms[platform] = expected
    date = pub_date or datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    parsed = datetime.fromisoformat(date.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("Publication date must include a timezone")
    return {"version": version, "notes": notes, "pub_date": date, "platforms": platforms}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", type=Path, help="directory containing extracted artifacts (nested directories allowed)")
    parser.add_argument("--version", required=True)
    parser.add_argument("--release-tag", default="", help="defaults to exact version, without v")
    parser.add_argument("--notes", default="")
    parser.add_argument("--pub-date", help="RFC 3339 date; defaults to current UTC time")
    parser.add_argument("--output", type=Path, default=Path("latest.json"))
    args = parser.parse_args()
    manifest = assemble(args.artifacts, args.version, args.release_tag, args.notes, args.pub_date)
    # Refuse to overwrite a previously reviewed manifest accidentally.
    with args.output.open("x", encoding="utf-8") as output:
        output.write(json.dumps(manifest, indent=2) + "\n")
    print(f"Created {args.output}; review and upload manually. Nothing was published.")


if __name__ == "__main__":
    main()
