from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
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
