from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


def load_validator():
    path = Path(__file__).resolve().parents[1] / "packaging" / "validate_portable.py"
    spec = importlib.util.spec_from_file_location("asterism_validate_portable", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load portable validator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PortableValidationTests(unittest.TestCase):
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
