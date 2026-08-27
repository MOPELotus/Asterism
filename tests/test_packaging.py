from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from subprocess import CompletedProcess, run
from unittest.mock import patch


def load_validator():
    path = Path(__file__).resolve().parents[1] / "packaging" / "validate_portable.py"
    spec = importlib.util.spec_from_file_location("asterism_validate_portable", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load portable validator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_builder():
    path = Path(__file__).resolve().parents[1] / "packaging" / "build_portable.py"
    spec = importlib.util.spec_from_file_location("asterism_build_portable", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load portable builder")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PortableValidationTests(unittest.TestCase):
    def test_builder_verifies_pinned_donor_file_hashes(self) -> None:
        builder = load_builder()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            donor = root / "donor"
            donor.mkdir()
            (donor / "entry.py").write_bytes(b"pinned source")
            (donor / "helper.py").write_bytes(b"pinned helper")
            metadata = root / "SOURCE.json"
            metadata.write_text(
                json.dumps(
                    [
                        {
                            "name": "test/donor",
                            "revision": "a" * 40,
                            "files": {
                                "entry.py": hashlib.sha256(b"pinned source").hexdigest(),
                                "helper.py": hashlib.sha256(b"pinned helper").hexdigest(),
                            },
                        }
                    ]
                ),
                encoding="utf-8",
            )
            builder.validate_source_integrity(metadata, donor)
            (donor / "helper.py").write_bytes(b"changed")
            with self.assertRaises(SystemExit):
                builder.validate_source_integrity(metadata, donor)

    def test_builder_stages_configured_playwright_chromium(self) -> None:
        builder = load_builder()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "playwright" / "chromium-1" / "chrome-win64" / "chrome.exe"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"browser")
            resources = root / "resources"
            resources.mkdir()
            with patch.dict(
                "os.environ", {"PLAYWRIGHT_BROWSERS_PATH": str(root / "playwright")}, clear=False
            ):
                self.assertTrue(builder.stage_browser_resources(resources))
            staged = resources / "browsers" / "chromium" / "chrome-win64" / "chrome.exe"
            self.assertEqual(staged.read_bytes(), b"browser")

    def test_builder_stages_non_win64_playwright_directory_layout(self) -> None:
        builder = load_builder()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "playwright" / "chromium-1" / "chrome-win" / "chrome.exe"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"arm-compatible-browser")
            resources = root / "resources"
            resources.mkdir()
            with patch.dict(
                "os.environ", {"PLAYWRIGHT_BROWSERS_PATH": str(root / "playwright")}, clear=False
            ):
                self.assertTrue(builder.stage_browser_resources(resources))
            staged = resources / "browsers" / "chromium" / "chrome-win" / "chrome.exe"
            self.assertEqual(staged.read_bytes(), b"arm-compatible-browser")

    def test_builder_copies_only_git_tracked_donor_files(self) -> None:
        builder = load_builder()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            donor = root / "donor"
            destination = root / "staged"
            donor.mkdir()
            run(["git", "init", "-q"], cwd=donor, check=True)
            (donor / ".gitignore").write_text("state/\n", encoding="utf-8")
            (donor / "entry.py").write_text("tracked\n", encoding="utf-8")
            run(["git", "add", ".gitignore", "entry.py"], cwd=donor, check=True)
            ignored = donor / "state" / "session.json"
            ignored.parent.mkdir()
            ignored.write_text('{"token":"must-not-ship"}', encoding="utf-8")

            builder.copy_git_tracked_tree(donor, destination)

            self.assertTrue((destination / "entry.py").is_file())
            self.assertTrue((destination / ".gitignore").is_file())
            self.assertFalse((destination / "state").exists())

    def test_builder_rejects_cross_architecture_label(self) -> None:
        builder = load_builder()
        with patch.object(builder.platform, "machine", return_value="AMD64"):
            builder.validate_native_architecture("x64")
            with self.assertRaises(SystemExit):
                builder.validate_native_architecture("arm64")

    def test_builder_exposes_workers_common_to_nuitka(self) -> None:
        builder = load_builder()
        with tempfile.TemporaryDirectory() as temporary:
            entry = Path(temporary) / "worker.py"
            entry.write_text("print('ok')\n", encoding="utf-8")
            output = Path(temporary) / "out"
            captured = {}

            def fake_run(command, *, cwd=builder.ROOT, env=None):
                captured["command"] = command
                captured["env"] = env

            with patch.object(builder, "run", side_effect=fake_run), self.assertRaises(SystemExit):
                builder.build_executable("python", output, entry, "worker", [], gui=False)
            self.assertIn("--include-module=common.runtime", captured["command"])
            self.assertTrue(captured["env"]["PYTHONPATH"].startswith(str(builder.ROOT / "workers")))

    def test_browser_smoke_requires_marker_and_zero_exit(self) -> None:
        validator = load_validator()
        executable = Path("chrome.exe")
        with patch.object(
            validator.subprocess,
            "run",
            return_value=CompletedProcess([], 0, "<body>asterism-browser-ok</body>", ""),
        ):
            validator.smoke_browser(executable)
        with patch.object(
            validator.subprocess,
            "run",
            return_value=CompletedProcess([], 1, "", "failed"),
        ), self.assertRaises(SystemExit):
            validator.smoke_browser(executable)

    def test_validator_allows_system_browser_fallback(self) -> None:
        validator = load_validator()
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            self.assertIsNone(validator.packaged_browser(package))

    def test_manifest_verification_rejects_tampering(self) -> None:
        validator = load_validator()
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            payload = package / "Asterism.exe"
            payload.write_bytes(b"portable")
            digest = hashlib.sha256(payload.read_bytes()).hexdigest()
            manifest = package / "SHA256SUMS.json"
            manifest.write_text(
                json.dumps({"files": {"Asterism.exe": digest}}), encoding="utf-8"
            )
            self.assertEqual(validator.verify_manifest(package, manifest), {"Asterism.exe": digest})
            payload.write_bytes(b"tampered")
            with self.assertRaises(SystemExit):
                validator.verify_manifest(package, manifest)

    def test_manifest_verification_rejects_unlisted_files(self) -> None:
        validator = load_validator()
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            payload = package / "Asterism.exe"
            payload.write_bytes(b"portable")
            digest = hashlib.sha256(payload.read_bytes()).hexdigest()
            manifest = package / "SHA256SUMS.json"
            manifest.write_text(
                json.dumps({"files": {"Asterism.exe": digest}}), encoding="utf-8"
            )
            (package / "unexpected.dll").write_bytes(b"unlisted")
            with self.assertRaises(SystemExit):
                validator.verify_manifest(package, manifest)

    def test_manifest_verification_rejects_symlinks(self) -> None:
        validator = load_validator()
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            payload = package / "Asterism.exe"
            payload.write_bytes(b"portable")
            digest = hashlib.sha256(payload.read_bytes()).hexdigest()
            manifest = package / "SHA256SUMS.json"
            manifest.write_text(
                json.dumps({"files": {"Asterism.exe": digest}}), encoding="utf-8"
            )
            link = package / "unexpected.dll"
            try:
                link.symlink_to(payload)
            except (OSError, NotImplementedError):
                self.skipTest("symlink creation is unavailable")
            with self.assertRaises(SystemExit):
                validator.verify_manifest(package, manifest)
