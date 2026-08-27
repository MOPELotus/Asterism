from __future__ import annotations

import json
import os
import re
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

    @staticmethod
    def command_argv(command: str, *, windows: bool | None = None) -> list[str]:
        is_windows = os.name == "nt" if windows is None else windows
        argv = shlex.split(command, posix=not is_windows)
        if is_windows:
            argv = [
                token[1:-1]
                if len(token) >= 2 and token[0] == token[-1] and token[0] in {'"', "'"}
                else token
                for token in argv
            ]
        return argv

    @staticmethod
    def safe_summary(summary: dict[str, Any]) -> dict[str, Any]:
        safe: dict[str, Any] = {}
        status = str(summary.get("status") or "unknown").casefold()
        safe["status"] = status if status in {"success", "failure"} else "unknown"
        for key in ("completed", "failed"):
            value = summary.get(key)
            if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
                safe[key] = value
        code = str(summary.get("error_code") or "").casefold()
        if re.fullmatch(r"[a-z0-9_-]{1,64}", code):
            safe["error_code"] = code
        return safe

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
            argv = self.command_argv(command)
            if not argv:
                return NotificationResult(False, error="notifications.command is empty")
            payload = json.dumps(
                {
                    "event": event,
                    "provider": provider,
                    "operation": operation,
                    "summary": self.safe_summary(summary),
                },
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
