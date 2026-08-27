from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOWED_LICENSES = {"Apache-2.0", "GPL-3.0", "MIT", "BSD-2-Clause", "BSD-3-Clause"}


def run(command: list[str], *, cwd: Path = ROOT) -> None:
    print("+", " ".join(command), flush=True)
    completed = subprocess.run(command, cwd=cwd, check=False)
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)


def validate_sources() -> list[dict[str, str]]:
    blocked: list[dict[str, str]] = []
    notices: list[dict[str, str]] = []
    for metadata_path in sorted((ROOT / "workers").glob("*/SOURCE.json")):
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
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
        value = json.loads(metadata_path.read_text(encoding="utf-8"))
        records = value if isinstance(value, list) else [value]
        for metadata in records:
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
        "*.pyc",
        "*.pyo",
        "*.db",
        "*.sqlite*",
    )
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination, ignore=ignored)


def stage_resources(stage: Path, notices: list[dict[str, str]]) -> Path:
    resources = stage / "resources"
    resources.mkdir(parents=True, exist_ok=True)
    copy_tree(ROOT / "upstreams", resources / "upstreams")
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
    (licenses / "SOURCES.json").write_text(
        json.dumps(notices, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return resources


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
        *(["--enable-plugin=pyqt6", "--windows-console-mode=disable"] if gui else []),
        *include_data,
        str(entry),
    ]
    run(command)
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
