import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from updater_assets import UPDATERS, asset_name, release_identity


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stage = load("stage-downloads")
manifest = load("assemble-updater")


class UpdaterTests(unittest.TestCase):
    def fixtures(self, root):
        extracted = root / "extracted"
        for label in stage.DOWNLOADS:
            release = root / label / "release"
            patterns = [pattern for pattern, _ in stage.DOWNLOADS[label]]
            if label in UPDATERS:
                pattern = UPDATERS[label][1]
                patterns += [pattern, pattern + ".sig"]
            for pattern in patterns:
                source = release / pattern.replace("*", "original")
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text("dummy signature" if pattern.endswith(".sig") else "dummy build")
            names = stage.stage_downloads(release, extracted / label, label, "0.1.0", True)
            self.assertEqual(sorted(names), sorted(p.name for p in (extracted / label).iterdir()))
        return extracted

    def test_exact_mapping_and_reused_appimage(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            extracted = root / "extracted"
            extracted.mkdir()
            self.fixtures(root)
            result = manifest.assemble(extracted, "0.1.0", pub_date="2026-10-12T00:00:00Z")
            expected = {
                "windows-x86_64": "win_x64_setup.exe",
                "windows-aarch64": "win_arm64_setup.exe",
                "darwin-aarch64": "macos_aarch64.app.tar.gz",
                "linux-x86_64": "linux_amd64.AppImage",
            }
            self.assertEqual(set(result["platforms"]), set(expected))
            for platform, suffix in expected.items():
                self.assertEqual(result["platforms"][platform], {
                    "url": f"https://github.com/DevLeashed/OpenLeash/releases/download/0.1.0/OpenLeash_0.1.0_{suffix}",
                    "signature": "dummy signature",
                })
            self.assertEqual(len(list(extracted.rglob("*.AppImage"))), 1)
            self.assertEqual(len(list(extracted.rglob("*.sig"))), 4)

    def test_missing_empty_signature_asset_or_mismatched_fragment_fails(self):
        for invalid in ("missing", "empty", "whitespace", "asset", "fragment", "duplicate"):
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / "extracted").mkdir()
                extracted = self.fixtures(root)
                name = asset_name("0.1.0", "win_x64_setup.exe")
                signature = extracted / "windows-x64" / (name + ".sig")
                if invalid == "missing":
                    signature.unlink()
                elif invalid == "empty":
                    signature.write_text("")
                elif invalid == "whitespace":
                    signature.write_text(" \n")
                elif invalid == "asset":
                    (signature.parent / name).unlink()
                elif invalid == "fragment":
                    (signature.parent / "updater-windows-x64.json").write_text("{}")
                else:
                    (extracted / signature.name).write_text("duplicate")
                with self.assertRaises(RuntimeError):
                    manifest.assemble(extracted, "0.1.0")

    def test_missing_signed_source_fails_before_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "openleash.exe").write_text("dummy")
            with self.assertRaises(RuntimeError):
                stage.stage_downloads(root, root / "downloads", "windows-x64", "0.1.0", True)
            self.assertFalse((root / "downloads").exists())

    def test_local_cli_writes_single_manifest_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "extracted").mkdir()
            extracted = self.fixtures(root)
            output = root / "latest.json"
            command = [sys.executable, "-B", str(Path(__file__).with_name("assemble-updater.py")),
                       str(extracted), "--version", "0.1.0", "--output", str(output)]
            first = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(json.loads(output.read_text())["version"], "0.1.0")
            before = output.read_bytes()
            second = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(output.read_bytes(), before)

    def test_release_tag_validation(self):
        self.assertEqual(release_identity("0.1.0"), "0.1.0")
        self.assertEqual(release_identity("0.1.0", "v0.1.0"), "v0.1.0")
        for tag in ("0.1.1", "../0.1.0", "0.1.0\n", "0.1.0?x", "0.1.0/asset"):
            with self.assertRaises(ValueError):
                release_identity("0.1.0", tag)
        for version in ("01.1.0", "0.1.0\n", "../0.1.0", "0.1.0-01"):
            with self.assertRaises(ValueError):
                release_identity(version)


if __name__ == "__main__":
    unittest.main()
