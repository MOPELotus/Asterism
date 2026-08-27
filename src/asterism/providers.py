from __future__ import annotations

import os
import sys
from dataclasses import dataclass, field
from pathlib import Path

from .constants import PROVIDER_IDS


@dataclass(frozen=True)
class WorkerSpec:
    provider: str
    script: Path
    upstream: Path
    source_metadata: Path
    environment: dict[str, str] = field(default_factory=dict)
    executable: Path | None = None

    def command(self, python: str | Path | None = None) -> list[str]:
        if self.executable is not None and self.executable.exists():
            return [
                str(self.executable),
                "--upstream",
                str(self.upstream),
                "--source-metadata",
                str(self.source_metadata),
            ]
        return [
            str(python or sys.executable),
            str(self.script),
            "--upstream",
            str(self.upstream),
            "--source-metadata",
            str(self.source_metadata),
        ]


class ProviderRegistry:
    def __init__(self, source_root: Path, python: str | Path | None = None) -> None:
        self.source_root = source_root.resolve()
        self.python = str(python or sys.executable)

    def get(self, provider: str) -> WorkerSpec:
        if provider not in PROVIDER_IDS:
            raise ValueError(f"unsupported provider id: {provider}")
        worker = self.source_root / "workers" / provider
        upstream_root = self.source_root / "upstreams"
        environment: dict[str, str] = {}
        if provider == "chaoxing":
            upstream = upstream_root / "chaoxing"
            environment["ASTERISM_CHAOXING_AUXILIARY_UPSTREAM"] = str(
                upstream_root / "chaoxing-exam"
            )
            environment["ASTERISM_CHAOXING_AUXILIARY_SOURCES"] = str(
                worker / "AUXILIARY_SOURCES.json"
            )
        elif provider == "welearn":
            upstream = upstream_root / "welearn" / "welearn_decompiled.py"
        elif provider == "uai":
            upstream = upstream_root / "uai" / "配置我运行我.py"
            environment["ASTERISM_UAI_BROWSER_UPSTREAM"] = str(upstream_root / "uai-browser")
            environment["ASTERISM_UAI_BROWSER_SOURCE_METADATA"] = str(
                worker / "BROWSER_SOURCE.json"
            )
        else:
            upstream = upstream_root / "cidaren"
        executable = worker / "worker.exe"
        return WorkerSpec(
            provider=provider,
            script=worker / "worker.py",
            upstream=upstream,
            source_metadata=worker / "SOURCE.json",
            environment=environment,
            executable=executable if executable.exists() else None,
        )

    def all(self) -> tuple[WorkerSpec, ...]:
        return tuple(self.get(provider) for provider in PROVIDER_IDS)

    def validate(self, spec: WorkerSpec) -> list[str]:
        executable = spec.executable if spec.executable is not None else spec.script
        required = [
            str(path)
            for path in (executable, spec.upstream, spec.source_metadata)
            if not path.exists()
        ]
        if spec.provider == "chaoxing":
            required.extend(
                str(path)
                for path in (
                    Path(value)
                    for value in (
                        spec.environment.get("ASTERISM_CHAOXING_AUXILIARY_UPSTREAM"),
                        spec.environment.get("ASTERISM_CHAOXING_AUXILIARY_SOURCES"),
                    )
                    if value
                )
                if not path.exists()
            )
        elif spec.provider == "uai":
            required.extend(
                str(path)
                for path in (
                    Path(value)
                    for value in (
                        spec.environment.get("ASTERISM_UAI_BROWSER_UPSTREAM"),
                        spec.environment.get("ASTERISM_UAI_BROWSER_SOURCE_METADATA"),
                    )
                    if value
                )
                if not path.exists()
            )
        return required

    def environment_for(self, spec: WorkerSpec) -> dict[str, str]:
        result = os.environ.copy()
        result.update(spec.environment)
        return result
