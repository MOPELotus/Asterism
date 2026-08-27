from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("package", type=Path)
    args = parser.parse_args()
    package = args.package.resolve()
    executable = package / "Asterism.exe"
    manifest = package / "SHA256SUMS.json"
    if not executable.exists() or not manifest.exists():
        raise SystemExit("portable package is missing Asterism.exe or SHA256SUMS.json")
    json.loads(manifest.read_text(encoding="utf-8"))
    for provider in ("chaoxing", "welearn", "uai", "cidaren"):
        worker = package / "resources" / "workers" / provider / "worker.exe"
        source = package / "resources" / "workers" / provider / "SOURCE.json"
        if not worker.exists() or not source.exists():
            raise SystemExit(f"portable package is missing {provider} worker resources")
    process = subprocess.Popen([str(executable)], cwd=package)
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise SystemExit(f"Asterism.exe exited during smoke startup ({process.returncode})")
            if (package / "data" / "question-bank.sqlite").exists():
                break
            time.sleep(0.25)
        else:
            raise SystemExit("Asterism.exe did not initialize its local data directory")
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
    print(json.dumps({"status": "ok", "package": str(package)}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
