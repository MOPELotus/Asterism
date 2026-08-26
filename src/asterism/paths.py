from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from pathlib import Path


def application_root() -> Path:
    """Return the immutable application/resource root."""
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parents[2]


def default_data_root() -> Path:
    configured = os.environ.get("ASTERISM_DATA_ROOT")
    if configured:
        return Path(configured).expanduser().resolve()
    return application_root()


@dataclass(frozen=True)
class DataPaths:
    root: Path

    @classmethod
    def resolve(cls, root: str | Path | None = None) -> DataPaths:
        value = Path(root).expanduser().resolve() if root is not None else default_data_root()
        return cls(value)

    @property
    def accounts(self) -> Path:
        return self.root / "accounts"

    @property
    def state(self) -> Path:
        return self.root / "state"

    @property
    def drafts(self) -> Path:
        return self.root / "drafts"

    @property
    def logs(self) -> Path:
        return self.root / "logs"

    @property
    def data(self) -> Path:
        return self.root / "data"

    @property
    def database(self) -> Path:
        return self.data / "question-bank.sqlite"

    @property
    def config(self) -> Path:
        return self.root / "config.local.json"

    def initialize(self) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        for directory in (self.accounts, self.state, self.drafts, self.logs, self.data):
            directory.mkdir(parents=True, exist_ok=True)
