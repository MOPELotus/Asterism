from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

MUTABLE_RUNTIME_DIRECTORIES = {"accounts", "state", "drafts", "logs", "data"}
MUTABLE_RUNTIME_FILES = {"config.local.json", ".asterism.lock"}
WORKER_PROTOCOLS = {
    "chaoxing": "asterism.chaoxing.worker.v1",
    "welearn": "asterism.welearn.worker.v1",
    "uai": "asterism.uai.worker.v1",
    "cidaren": "asterism.cidaren.worker.v1",
}


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
    actual_files = set()
    for path in package_root.rglob("*"):
        if not path.is_file() or path.is_symlink() or path.resolve() == manifest_resolved:
            continue
        relative = path.relative_to(package_root)
        if relative.parts[0] in MUTABLE_RUNTIME_DIRECTORIES:
            continue
        if len(relative.parts) == 1 and relative.name in MUTABLE_RUNTIME_FILES:
            continue
        actual_files.add(relative.as_posix())
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


def smoke_worker_health(package: Path, provider: str) -> dict[str, object]:
    """Start one packaged Worker and verify its side-effect-free health path.

    This catches missing frozen imports and resource path mistakes that a mere
    file-existence check cannot see, while deliberately sending no credentials.
    """
    worker_root = package / "resources" / "workers" / provider
    worker = worker_root / "worker.exe"
    upstream_root = package / "resources" / "upstreams"
    upstream = (
        upstream_root / "chaoxing"
        if provider == "chaoxing"
        else upstream_root / "welearn" / "welearn_decompiled.py"
        if provider == "welearn"
        else upstream_root / "uai" / "配置我运行我.py"
        if provider == "uai"
        else upstream_root / "cidaren"
    )
    metadata = worker_root / "SOURCE.json"
    request = {
        "request_id": f"portable-health-{provider}",
        "operation": "health",
        "payload": {},
    }
    try:
        completed = subprocess.run(
            [
                str(worker),
                "--upstream",
                str(upstream),
                "--source-metadata",
                str(metadata),
            ],
            input=json.dumps(request, ensure_ascii=True) + "\n",
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SystemExit(f"{provider} packaged Worker health smoke failed: {error}") from error
    if completed.returncode != 0:
        raise SystemExit(
            f"{provider} packaged Worker health smoke failed with exit code "
            f"{completed.returncode}"
        )
    terminals: list[dict[str, object]] = []
    for line in completed.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("request_id") != request["request_id"]:
            raise SystemExit(f"{provider} packaged Worker returned a mismatched request id")
        if event.get("operation") != "health":
            raise SystemExit(f"{provider} packaged Worker returned a mismatched operation")
        if event.get("protocol") != WORKER_PROTOCOLS[provider]:
            raise SystemExit(f"{provider} packaged Worker returned a mismatched protocol")
        if event.get("type") in {"result", "error"}:
            terminals.append(event)
    if len(terminals) != 1 or terminals[0].get("type") != "result":
        raise SystemExit(f"{provider} packaged Worker returned an invalid terminal event")
    result = terminals[0].get("data")
    if not isinstance(result, dict) or result.get("status") != "ok":
        raise SystemExit(f"{provider} packaged Worker returned no healthy result")
    return result


def stop_owned_process(process: subprocess.Popen[object], *, timeout: float = 5) -> None:
    """Best-effort cleanup for a process created by this validator only."""
    if process.poll() is not None:
        return
    try:
        process.terminate()
        process.wait(timeout=timeout)
        return
    except (OSError, subprocess.TimeoutExpired):
        pass
    try:
        process.kill()
        process.wait(timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        pass


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
        smoke_worker_health(package, provider)
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
            stop_owned_process(second)
            raise SystemExit(
                "second Asterism.exe instance did not reject the occupied data directory"
            ) from error
        if second_code != 2:
            raise SystemExit(
                f"second Asterism.exe instance returned unexpected code {second_code}"
            )
    finally:
        stop_owned_process(process)
    print(json.dumps({"status": "ok", "package": str(package)}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
