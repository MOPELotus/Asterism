from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_manifest(package: Path, manifest_path: Path) -> dict[str, str]:
    package_root = package.resolve()
    manifest_resolved = manifest_path.resolve()
    if manifest_path.is_symlink() or (
        manifest_resolved != package_root and package_root not in manifest_resolved.parents
    ):
        raise SystemExit("portable manifest must be a regular file inside the package")
    value = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or not isinstance(value.get("files"), dict):
        raise SystemExit("portable manifest must contain a files object")
    verified: dict[str, str] = {}
    for raw_name, expected in value["files"].items():
        name = str(raw_name)
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise SystemExit(f"portable manifest contains unsafe path: {name}")
        path = (package_root / relative).resolve()
        if path != package_root and package_root not in path.parents:
            raise SystemExit(f"portable manifest path escapes package: {name}")
        if not path.is_file():
            raise SystemExit(f"portable manifest file is missing: {name}")
        actual = sha256(path)
        if str(expected).casefold() != actual:
            raise SystemExit(f"portable manifest hash mismatch: {name}")
        verified[name] = actual
    actual_files = {
        path.relative_to(package_root).as_posix()
        for path in package_root.rglob("*")
        if path.is_file() and not path.is_symlink() and path.resolve() != manifest_resolved
    }
    symlinks = [
        path.relative_to(package_root).as_posix()
        for path in package_root.rglob("*")
        if path.is_symlink()
    ]
    if symlinks:
        raise SystemExit(
            "portable package contains unsupported symlinks: " + ", ".join(symlinks[:8])
        )
    listed_files = set(verified)
    missing = sorted(listed_files - actual_files)
    extra = sorted(actual_files - listed_files)
    if missing:
        raise SystemExit("portable manifest files are missing: " + ", ".join(missing[:8]))
    if extra:
        raise SystemExit("portable package contains unlisted files: " + ", ".join(extra[:8]))
    return verified


def packaged_browser(package: Path) -> Path | None:
    root = package / "resources" / "browsers" / "chromium"
    if not root.is_dir():
        return None
    matches = sorted(
        path
        for path in root.rglob("*.exe")
        if path.name.casefold() in {"chrome.exe", "chromium.exe"}
    )
    return matches[0] if matches else None


def smoke_browser(executable: Path) -> None:
    marker = "asterism-browser-ok"
    with tempfile.TemporaryDirectory(prefix="asterism-browser-smoke-") as temporary:
        try:
            completed = subprocess.run(
                [
                    str(executable),
                    "--headless=new",
                    "--disable-gpu",
                    "--no-first-run",
                    "--no-default-browser-check",
                    f"--user-data-dir={temporary}",
                    "--dump-dom",
                    f"data:text/html,<body>{marker}</body>",
                ],
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=30,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise SystemExit(f"bundled Chromium smoke failed: {error}") from error
    if completed.returncode != 0 or marker not in completed.stdout:
        raise SystemExit(
            f"bundled Chromium smoke failed with exit code {completed.returncode}"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("package", type=Path)
    parser.add_argument(
        "--require-browser",
        action="store_true",
        help="require and smoke-test the bundled Chromium executable",
    )
    args = parser.parse_args()
    package = args.package.resolve()
    executable = package / "Asterism.exe"
    manifest = package / "SHA256SUMS.json"
    if not executable.exists() or not manifest.exists():
        raise SystemExit("portable package is missing Asterism.exe or SHA256SUMS.json")
    verify_manifest(package, manifest)
    browser = packaged_browser(package)
    if browser is None:
        if args.require_browser:
            raise SystemExit("portable package is missing its bundled Chromium executable")
    else:
        smoke_browser(browser)
    for provider in ("chaoxing", "welearn", "uai", "cidaren"):
        worker = package / "resources" / "workers" / provider / "worker.exe"
        source = package / "resources" / "workers" / provider / "SOURCE.json"
        if not worker.exists() or not source.exists():
            raise SystemExit(f"portable package is missing {provider} worker resources")
    environment = os.environ.copy()
    environment["ASTERISM_NONINTERACTIVE"] = "1"
    process = subprocess.Popen([str(executable)], cwd=package, env=environment)
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
        second = subprocess.Popen([str(executable)], cwd=package, env=environment)
        try:
            second_code = second.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            second.kill()
            second.wait(timeout=5)
            raise SystemExit(
                "second Asterism.exe instance did not reject the occupied data directory"
            ) from error
        if second_code != 2:
            raise SystemExit(
                f"second Asterism.exe instance returned unexpected code {second_code}"
            )
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
