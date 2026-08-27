from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOWED_LICENSES = {"Apache-2.0", "GPL-3.0", "MIT", "BSD-2-Clause", "BSD-3-Clause"}
SOURCE_LOCATIONS = (
    (Path("workers/chaoxing/SOURCE.json"), Path("upstreams/chaoxing")),
    (Path("workers/chaoxing/AUXILIARY_SOURCES.json"), Path("upstreams/chaoxing-exam")),
    (Path("workers/welearn/SOURCE.json"), Path("upstreams/welearn")),
    (Path("workers/uai/SOURCE.json"), Path("upstreams/uai")),
    (Path("workers/uai/BROWSER_SOURCE.json"), Path("upstreams/uai-browser")),
    (Path("workers/cidaren/SOURCE.json"), Path("upstreams/cidaren")),
)


def validate_native_architecture(architecture: str) -> None:
    machine = platform.machine().casefold().replace("-", "_")
    aliases = {
        "x64": {"amd64", "x86_64", "x64"},
        "arm64": {"arm64", "aarch64"},
    }
    if architecture not in aliases:
        raise SystemExit(f"unsupported portable architecture: {architecture}")
    if machine not in aliases[architecture]:
        raise SystemExit(
            f"portable {architecture} build requires a native {architecture} runner; "
            f"detected {platform.machine() or 'unknown'}"
        )


def run(
    command: list[str], *, cwd: Path = ROOT, env: dict[str, str] | None = None
) -> None:
    print("+", " ".join(command), flush=True)
    completed = subprocess.run(command, cwd=cwd, check=False, env=env)
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)


def _source_records(metadata_path: Path) -> list[dict[str, object]]:
    value = json.loads(metadata_path.read_text(encoding="utf-8"))
    records = value if isinstance(value, list) else [value]
    if not records or not all(isinstance(record, dict) for record in records):
        raise SystemExit(f"invalid donor metadata records: {metadata_path}")
    return records


def _safe_source_file(donor_root: Path, relative_name: str) -> Path:
    relative = Path(relative_name)
    if relative.is_absolute() or ".." in relative.parts:
        raise SystemExit(f"unsafe donor entrypoint path: {relative_name}")
    root = donor_root.resolve()
    candidate = (root / relative).resolve()
    try:
        candidate.relative_to(root)
    except ValueError as error:
        raise SystemExit(f"donor entrypoint escapes source root: {relative_name}") from error
    return candidate


def validate_source_integrity(metadata_path: Path, donor_root: Path) -> None:
    """Verify pinned donor files and, when available, the checkout revision."""
    if not metadata_path.is_file():
        raise SystemExit(f"missing donor metadata: {metadata_path}")
    if not donor_root.is_dir():
        raise SystemExit(f"missing donor source: {donor_root}")

    records = _source_records(metadata_path)
    expected_revisions: set[str] = set()
    for record in records:
        name = str(record.get("name") or metadata_path.stem)
        revision = str(record.get("revision") or "").casefold()
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise SystemExit(f"invalid pinned revision for {name}: {revision or 'missing'}")
        expected_revisions.add(revision)

        declared_files = record.get("files")
        if declared_files is None:
            entrypoint = str(record.get("entrypoint") or "")
            digest = str(record.get("entrypoint_sha256") or "").casefold()
            files = {entrypoint: digest}
        elif isinstance(declared_files, dict):
            files = {str(path): str(digest).casefold() for path, digest in declared_files.items()}
        else:
            raise SystemExit(f"invalid donor file manifest for {name}")
        if not files or any(not path for path in files):
            raise SystemExit(f"missing donor entrypoint manifest for {name}")

        for relative_name, expected_digest in files.items():
            if not re.fullmatch(r"[0-9a-f]{64}", expected_digest):
                raise SystemExit(f"invalid donor SHA-256 for {name}: {relative_name}")
            source_file = _safe_source_file(donor_root, relative_name)
            if not source_file.is_file():
                raise SystemExit(f"missing pinned donor file for {name}: {relative_name}")
            actual_digest = hashlib.sha256(source_file.read_bytes()).hexdigest()
            if actual_digest != expected_digest:
                raise SystemExit(
                    f"pinned donor file hash mismatch for {name}: {relative_name}"
                )

    git_marker = donor_root / ".git"
    if git_marker.exists():
        completed = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=donor_root,
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise SystemExit(f"unable to read donor revision: {donor_root}")
        actual_revision = completed.stdout.strip().casefold()
        if expected_revisions != {actual_revision}:
            expected = ", ".join(sorted(expected_revisions))
            raise SystemExit(
                f"pinned donor revision mismatch for {donor_root.name}: "
                f"expected {expected}, found {actual_revision or 'unknown'}"
            )


def validate_sources() -> list[dict[str, str]]:
    blocked: list[dict[str, str]] = []
    notices: list[dict[str, str]] = []
    for metadata_name, donor_name in SOURCE_LOCATIONS:
        validate_source_integrity(ROOT / metadata_name, ROOT / donor_name)
    for metadata_path in sorted((ROOT / "workers").glob("*/SOURCE.json")):
        metadata = _source_records(metadata_path)[0]
        license_name = str(metadata.get("license") or "NOASSERTION")
        item = {
            "name": str(metadata.get("name") or metadata_path.parent.name),
            "repository": str(metadata.get("repository") or ""),
            "revision": str(metadata.get("revision") or ""),
            "license": license_name,
        }
        if license_name not in ALLOWED_LICENSES:
            blocked.append(item)
        notices.append(item)
    for metadata_path in (
        ROOT / "workers" / "chaoxing" / "AUXILIARY_SOURCES.json",
        ROOT / "workers" / "uai" / "BROWSER_SOURCE.json",
    ):
        if not metadata_path.exists():
            continue
        for metadata in _source_records(metadata_path):
            license_name = str(metadata.get("license") or "NOASSERTION")
            item = {
                "name": str(metadata.get("name") or metadata_path.stem),
                "repository": str(metadata.get("repository") or ""),
                "revision": str(metadata.get("revision") or ""),
                "license": license_name,
            }
            if license_name not in ALLOWED_LICENSES:
                blocked.append(item)
            notices.append(item)
    if blocked:
        names = ", ".join(f"{item['name']} ({item['license']})" for item in blocked)
        raise SystemExit(
            "portable package blocked by unresolved donor license: "
            + names
            + ". Update SOURCE.json after confirming redistribution rights."
        )
    return notices


def copy_tree(source: Path, destination: Path) -> None:
    ignored = shutil.ignore_patterns(
        ".git",
        ".gitmodules",
        "__pycache__",
        ".pytest_cache",
        ".ruff_cache",
        "*.pyc",
        "*.pyo",
        "*.db",
        "*.sqlite*",
        "*.bak",
        "*.log",
        "*.env",
        "*.secrets",
        "*.token",
        "*.cookies",
        "*.session",
    )
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination, ignore=ignored)


def stage_resources(stage: Path, notices: list[dict[str, str]]) -> Path:
    resources = stage / "resources"
    resources.mkdir(parents=True, exist_ok=True)
    copy_tree(ROOT / "upstreams", resources / "upstreams")
    stage_browser_resources(resources)
    worker_resources = resources / "workers"
    for provider in ("chaoxing", "welearn", "uai", "cidaren"):
        source = ROOT / "workers" / provider
        destination = worker_resources / provider
        destination.mkdir(parents=True, exist_ok=True)
        for name in (
            "worker.py",
            "SOURCE.json",
            "AUXILIARY_SOURCES.json",
            "BROWSER_SOURCE.json",
            "README.md",
            "requirements.txt",
        ):
            path = source / name
            if path.exists():
                shutil.copy2(path, destination / name)
    licenses = resources / "licenses"
    licenses.mkdir(parents=True, exist_ok=True)
    _stage_license_files(licenses)
    (licenses / "SOURCES.json").write_text(
        json.dumps(notices, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return resources


def stage_browser_resources(resources: Path) -> bool:
    """Copy a Playwright Chromium installation when the build environment has one."""
    candidates: list[Path] = []
    configured = os.environ.get("PLAYWRIGHT_BROWSERS_PATH")
    if configured and configured != "0":
        candidates.append(Path(configured).expanduser())
    local_app_data = os.environ.get("LOCALAPPDATA")
    if local_app_data:
        candidates.append(Path(local_app_data) / "ms-playwright")
    for root in candidates:
        if not root.exists():
            continue
        for executable in sorted(root.glob("chromium-*/chrome-*/chrome.exe")):
            browser_root = executable.parent.parent
            destination = resources / "browsers" / "chromium"
            destination.parent.mkdir(parents=True, exist_ok=True)
            copy_tree(browser_root, destination)
            return True
    print(
        "warning: Playwright Chromium not found; packaged UAI browser will fall back to system Edge"
    )
    return False


def _stage_license_files(destination: Path) -> None:
    """Copy donor notices into an explicit package location.

    The complete donor trees remain under ``resources/upstreams`` for runtime
    compatibility.  Keeping a second, predictable license directory makes the
    ZIP self-auditing without copying any repository metadata or local paths.
    """
    donor_dirs = {
        "chaoxing": ROOT / "upstreams" / "chaoxing",
        "chaoxing-exam": ROOT / "upstreams" / "chaoxing-exam",
        "welearn": ROOT / "upstreams" / "welearn",
        "uai": ROOT / "upstreams" / "uai",
        "uai-browser": ROOT / "upstreams" / "uai-browser",
        "cidaren": ROOT / "upstreams" / "cidaren",
    }
    for donor, source in donor_dirs.items():
        if not source.exists():
            continue
        candidates = [
            path
            for path in source.iterdir()
            if path.is_file()
            and re.fullmatch(r"(?i)(license|copying|notice)(?:[._-].*)?", path.name)
        ]
        for index, path in enumerate(sorted(candidates)):
            suffix = path.suffix or ".txt"
            name = f"{donor}-{index}{suffix}"
            shutil.copy2(path, destination / name)


def build_executable(
    python: str,
    output: Path,
    entry: Path,
    name: str,
    include_data: list[str],
    *,
    gui: bool = False,
) -> Path:
    command = [
        python,
        "-m",
        "nuitka",
        "--standalone",
        "--assume-yes-for-downloads",
        "--remove-output",
        f"--output-dir={output}",
        f"--output-filename={name}.exe",
        "--follow-imports",
        "--include-module=common.runtime",
        *(["--enable-plugin=pyqt6", "--windows-console-mode=disable"] if gui else []),
        *include_data,
        str(entry),
    ]
    environment = os.environ.copy()
    workers_path = str(ROOT / "workers")
    existing_python_path = environment.get("PYTHONPATH")
    environment["PYTHONPATH"] = (
        workers_path
        if not existing_python_path
        else workers_path + os.pathsep + existing_python_path
    )
    run(command, env=environment)
    result = output / f"{entry.stem}.dist" / f"{name}.exe"
    if not result.exists():
        matches = list(output.rglob(f"{name}.exe"))
        if len(matches) != 1:
            raise SystemExit(f"Nuitka did not produce one {name}.exe under {output}")
        result = matches[0]
    return result


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--architecture", choices=("x64", "arm64"), required=True)
    parser.add_argument("--version", default="dev")
    parser.add_argument("--output-root", type=Path, default=ROOT / "release")
    args = parser.parse_args()
    validate_native_architecture(args.architecture)
    notices = validate_sources()
    stage = ROOT / "build" / "portable" / args.architecture
    if stage.exists():
        shutil.rmtree(stage)
    resources = stage_resources(stage, notices)
    python = sys.executable
    gui_dist = build_executable(
        python,
        stage / "gui",
        ROOT / "packaging" / "portable_launcher.py",
        "Asterism",
        [f"--include-data-dir={resources}=resources"],
        gui=True,
    )
    package = stage / "package"
    package.mkdir(parents=True, exist_ok=True)
    shutil.copytree(gui_dist.parent, package, dirs_exist_ok=True)
    for provider in ("chaoxing", "welearn", "uai", "cidaren"):
        worker_entry = ROOT / "workers" / provider / "worker.py"
        worker_dist = build_executable(
            python,
            stage / f"worker-{provider}",
            worker_entry,
            "worker",
            [],
        )
        destination = package / "resources" / "workers" / provider
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copytree(worker_dist.parent, destination, dirs_exist_ok=True)
    (package / "README.txt").write_text(
        "Asterism local desktop portable package. Double-click Asterism.exe.\n"
        "Local accounts and data are created beside the executable.\n",
        encoding="utf-8",
    )
    manifest = {
        "version": args.version,
        "architecture": args.architecture,
        "files": {
            str(path.relative_to(package)).replace("\\", "/"): sha256(path)
            for path in sorted(package.rglob("*"))
            if path.is_file()
        },
    }
    (package / "SHA256SUMS.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    archive = args.output_root / f"asterism-windows-{args.architecture}-{args.version}.zip"
    args.output_root.mkdir(parents=True, exist_ok=True)
    if archive.exists():
        archive.unlink()
    shutil.make_archive(str(archive.with_suffix("")), "zip", package)
    print(f"created {archive}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
