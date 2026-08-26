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

    def command(self, python: str | Path | None = None) -> list[str]:
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
        elif provider == "welearn":
            upstream = upstream_root / "welearn" / "welearn_decompiled.py"
        elif provider == "uai":
            upstream = upstream_root / "uai" / "配置我运行我.py"
        else:
            upstream = upstream_root / "cidaren"
        return WorkerSpec(
            provider=provider,
            script=worker / "worker.py",
            upstream=upstream,
            source_metadata=worker / "SOURCE.json",
            environment=environment,
        )

    def all(self) -> tuple[WorkerSpec, ...]:
        return tuple(self.get(provider) for provider in PROVIDER_IDS)

    def validate(self, spec: WorkerSpec) -> list[str]:
        missing = [
            str(path)
            for path in (spec.script, spec.upstream, spec.source_metadata)
            if not path.exists()
        ]
        return missing

    def environment_for(self, spec: WorkerSpec) -> dict[str, str]:
        result = os.environ.copy()
        result.update(spec.environment)
        return result
