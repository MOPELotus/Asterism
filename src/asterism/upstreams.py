from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
import tempfile
import urllib.error
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path


class UpstreamError(RuntimeError):
    """Raised when a pinned upstream cannot be located or installed safely."""


@dataclass(frozen=True)
class UpstreamSpec:
    provider: str
    repository: str
    revision: str
    entrypoint: str
    entrypoint_sha256: str


def load_spec(source_root: Path, provider: str) -> UpstreamSpec:
    metadata_path = source_root / "workers" / provider / "SOURCE.json"
    try:
        value = json.loads(metadata_path.read_text(encoding="utf-8"))
        if isinstance(value, list):
            value = value[0]
        if not isinstance(value, dict):
            raise ValueError("metadata must be an object")
        return UpstreamSpec(
            provider=provider,
            repository=str(value["repository"]),
            revision=str(value["revision"]),
            entrypoint=str(value["entrypoint"]),
            entrypoint_sha256=str(value["entrypoint_sha256"]),
        )
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        raise UpstreamError(f"invalid {provider} upstream metadata: {metadata_path}") from error


def _entrypoint(root: Path, spec: UpstreamSpec) -> Path:
    relative = Path(spec.entrypoint)
    if relative.is_absolute() or ".." in relative.parts:
        raise UpstreamError(f"unsafe upstream entrypoint: {spec.entrypoint}")
    candidate = (root / relative).resolve()
    try:
        candidate.relative_to(root.resolve())
    except ValueError as error:
        raise UpstreamError(f"upstream entrypoint escapes root: {spec.entrypoint}") from error
    return candidate


def validate_checkout(root: Path, spec: UpstreamSpec) -> Path:
    entrypoint = _entrypoint(root, spec)
    if not entrypoint.is_file():
        raise UpstreamError(f"missing pinned upstream entrypoint: {entrypoint}")
    digest = hashlib.sha256(entrypoint.read_bytes()).hexdigest()
    if digest.casefold() != spec.entrypoint_sha256.casefold():
        raise UpstreamError(f"upstream entrypoint hash mismatch: {entrypoint}")
    git_dir = root / ".git"
    if git_dir.exists():
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True, check=False
        )
        if result.returncode != 0 or result.stdout.strip().casefold() != spec.revision.casefold():
            raise UpstreamError(f"upstream revision mismatch: {root}")
    else:
        marker = root / ".asterism-revision"
        marker_revision = ""
        if marker.is_file():
            marker_revision = marker.read_text(encoding="utf-8").strip().casefold()
        if marker_revision and marker_revision != spec.revision.casefold():
            raise UpstreamError(f"upstream revision marker mismatch: {root}")
    return entrypoint


def discover_local(source_root: Path, data_root: Path, spec: UpstreamSpec) -> Path | None:
    candidates = (
        source_root / "upstreams" / spec.provider,
        data_root / "data" / "upstreams" / spec.provider,
        data_root / "upstreams" / spec.provider,
    )
    configured = None
    if spec.provider == "welearn":
        configured = data_root / "config.local.json"
        if configured.is_file():
            try:
                value = json.loads(configured.read_text(encoding="utf-8"))
                configured_path = value.get("upstreams", {}).get(spec.provider)
                if isinstance(configured_path, str) and configured_path.strip():
                    candidates = (Path(configured_path).expanduser(), *candidates)
            except (OSError, TypeError, ValueError, json.JSONDecodeError):
                pass
    for candidate in candidates:
        try:
            validate_checkout(candidate, spec)
        except UpstreamError:
            continue
        return candidate
    return None


def _git_executable() -> str | None:
    return shutil.which("git")


def fetch_pinned(spec: UpstreamSpec, destination: Path, *, allow_network: bool) -> Path:
    if not allow_network:
        raise UpstreamError("upstream is not installed; enable network download explicitly")
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=f"asterism-{spec.provider}-"))
    try:
        git = _git_executable()
        if git:
            checkout = temporary / "checkout"
            clone = subprocess.run(
                [git, "clone", "--no-tags", "--filter=blob:none", spec.repository, str(checkout)],
                capture_output=True,
                text=True,
                check=False,
            )
            if clone.returncode == 0:
                pinned = subprocess.run(
                    [git, "-C", str(checkout), "checkout", "--detach", spec.revision],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                if pinned.returncode == 0:
                    validate_checkout(checkout, spec)
                    if destination.exists():
                        shutil.rmtree(destination)
                    shutil.move(str(checkout), str(destination))
                    (destination / ".asterism-revision").write_text(
                        spec.revision + "\n", encoding="utf-8"
                    )
                    return validate_checkout(destination, spec)

        archive_url = spec.repository.rstrip("/") + f"/archive/{spec.revision}.zip"
        archive = temporary / "upstream.zip"
        try:
            urllib.request.urlretrieve(archive_url, archive)
            archive_root = temporary / "archive"
            archive_root.mkdir()
            with zipfile.ZipFile(archive) as bundle:
                root = archive_root.resolve()
                for member in bundle.infolist():
                    target = (archive_root / member.filename).resolve()
                    try:
                        target.relative_to(root)
                    except ValueError as error:
                        raise UpstreamError("upstream archive contains an unsafe path") from error
                bundle.extractall(archive_root)
        except (OSError, urllib.error.URLError, zipfile.BadZipFile) as error:
            raise UpstreamError(f"unable to download pinned upstream: {archive_url}") from error
        roots = [path for path in (temporary / "archive").iterdir() if path.is_dir()]
        if len(roots) != 1:
            raise UpstreamError("pinned upstream archive has an unexpected layout")
        validate_checkout(roots[0], spec)
        if destination.exists():
            shutil.rmtree(destination)
        shutil.move(str(roots[0]), str(destination))
        (destination / ".asterism-revision").write_text(spec.revision + "\n", encoding="utf-8")
        return validate_checkout(destination, spec)
    finally:
        shutil.rmtree(temporary, ignore_errors=True)


def resolve(
    source_root: Path,
    data_root: Path,
    provider: str,
    *,
    allow_network: bool = False,
) -> Path:
    spec = load_spec(source_root, provider)
    local = discover_local(source_root, data_root, spec)
    if local is not None:
        return local
    destination = data_root / "data" / "upstreams" / provider
    return fetch_pinned(spec, destination, allow_network=allow_network)
