import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("stage_downloads", Path(__file__).with_name("stage-downloads.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class StageDownloadsTests(unittest.TestCase):
    def test_all_release_asset_names_and_only_expected_files(self):
        expected = {
            "windows-x64": ["win_x64.exe"],
            "windows-arm64": ["win_arm64.exe"],
            "macos-arm64": ["macos_aarch64.dmg"],
            "linux-x64": ["linux_amd64.AppImage", "linux_amd64.deb"],
            "linux-arm64": ["linux_arm64.deb"],
        }
        for label, suffixes in expected.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                release = root / "release"
                for pattern, _ in module.DOWNLOADS[label]:
                    source = release / pattern.replace("*", "original-build-name")
                    source.parent.mkdir(parents=True, exist_ok=True)
                    source.write_bytes(b"dummy build")
                # Unneeded build products must never appear in downloads.
                (release / "extra.txt").write_text("not a download")
                stage = root / "downloads"
                names = module.stage_downloads(release, stage, label, "0.1.0")
                self.assertEqual(names, [f"OpenLeash_0.1.0_{suffix}" for suffix in suffixes])
                self.assertEqual(sorted(path.name for path in stage.iterdir()), sorted(names))
                for name in names:
                    self.assertEqual((stage / name).read_bytes(), b"dummy build")

    def test_missing_empty_and_ambiguous_sources_fail_before_staging(self):
        for invalid in ("missing", "empty", "ambiguous"):
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                release = root / "release"
                dmg = release / "bundle/dmg"
                dmg.mkdir(parents=True)
                if invalid != "missing":
                    (dmg / "one.dmg").write_bytes(b"" if invalid == "empty" else b"dummy")
                if invalid == "ambiguous":
                    (dmg / "two.dmg").write_bytes(b"dummy")
                stage = root / "downloads"
                with self.assertRaises(RuntimeError):
                    module.stage_downloads(release, stage, "macos-arm64", "0.1.1")
                self.assertFalse(stage.exists())

    def test_installers_are_staged_only_when_requested(self):
        # build_installers adds the Windows NSIS installer and nothing else: no
        # signature and no updater fragment may reach the downloads, even when
        # the build tree still holds them.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release = root / "release"
            (release / "bundle/nsis").mkdir(parents=True)
            (release / "openleash.exe").write_bytes(b"dummy build")
            installer = release / "bundle/nsis/OpenLeash-setup.exe"
            installer.write_bytes(b"dummy build")
            installer.with_name(installer.name + ".sig").write_text("dummy signature")
            stage = root / "downloads"
            names = module.stage_downloads(release, stage, "windows-x64", "0.1.0", True)
            self.assertEqual(names, ["OpenLeash_0.1.0_win_x64.exe", "OpenLeash_0.1.0_win_x64_setup.exe"])
            self.assertEqual(sorted(path.name for path in stage.iterdir()), sorted(names))

    def test_macos_stages_only_the_dmg(self):
        # macOS has no installer bundle and no updater artifact, so requesting
        # installers adds nothing and a leftover tarball/signature is ignored.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release = root / "release"
            (release / "bundle/dmg").mkdir(parents=True)
            (release / "bundle/dmg/OpenLeash.dmg").write_bytes(b"dummy build")
            (release / "bundle/macos").mkdir(parents=True)
            tarball = release / "bundle/macos/OpenLeash.app.tar.gz"
            tarball.write_bytes(b"dummy build")
            tarball.with_name(tarball.name + ".sig").write_text("dummy signature")
            stage = root / "downloads"
            names = module.stage_downloads(release, stage, "macos-arm64", "0.1.0", True)
            self.assertEqual(names, ["OpenLeash_0.1.0_macos_aarch64.dmg"])
            self.assertEqual(sorted(path.name for path in stage.iterdir()), sorted(names))

    def test_current_version_is_used(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "openleash.exe").write_bytes(b"dummy")
            names = module.stage_downloads(root, root / "downloads", "windows-x64", "0.1.1")
            self.assertEqual(names, ["OpenLeash_0.1.1_win_x64.exe"])


if __name__ == "__main__":
    unittest.main()
