from __future__ import annotations

import json
import os
import shlex
import subprocess
from dataclasses import dataclass
from typing import Any

from .config import LocalConfigStore


@dataclass(frozen=True)
class NotificationResult:
    sent: bool
    return_code: int | None = None
    error: str | None = None


class NotificationDispatcher:
    """Optional local terminal notification without a shell or secret payload."""

    def __init__(self, config: LocalConfigStore) -> None:
        self.config = config

    def send(
        self, event: str, *, provider: str, operation: str, summary: dict[str, Any]
    ) -> NotificationResult:
        settings = self.config.ensure().get("notifications", {})
        if not isinstance(settings, dict) or settings.get("enabled") is not True:
            return NotificationResult(False)
        command = str(settings.get("command") or "").strip()
        if not command:
            return NotificationResult(False, error="notifications.command is empty")
        try:
            argv = shlex.split(command, posix=False)
            if not argv:
                return NotificationResult(False, error="notifications.command is empty")
            payload = json.dumps(
                {"event": event, "provider": provider, "operation": operation, "summary": summary},
                ensure_ascii=False,
                separators=(",", ":"),
            )
            result = subprocess.run(
                [*argv, payload],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                env={"PATH": os.environ.get("PATH", "")},
                timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            return NotificationResult(result.returncode == 0, result.returncode)
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            return NotificationResult(False, error=str(error))
