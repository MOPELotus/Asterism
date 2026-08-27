from __future__ import annotations

import contextlib
import json
import os
import queue
import signal
import subprocess
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from uuid import uuid4

from .profiles import Profile, ProfileStateStore
from .providers import ProviderRegistry, WorkerSpec


class RunnerError(RuntimeError):
    def __init__(self, code: str, message: str, events: tuple[dict[str, Any], ...] = ()) -> None:
        super().__init__(message)
        self.code = code
        self.events = events


@dataclass(frozen=True)
class RunResult:
    request_id: str
    provider: str
    operation: str
    data: dict[str, Any]
    events: tuple[dict[str, Any], ...]
    log_path: Path


class RunnerManager:
    def __init__(
        self,
        registry: ProviderRegistry,
        logs_root: Path,
        state_store: ProfileStateStore | None = None,
    ) -> None:
        self.registry = registry
        self.logs_root = logs_root
        self.state_store = state_store

    def invoke(
        self,
        spec: WorkerSpec,
        operation: str,
        payload: dict[str, Any] | None = None,
        *,
        timeout: float = 120.0,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
        profile: Profile | None = None,
    ) -> RunResult:
        missing = self.registry.validate(spec)
        if missing:
            raise RunnerError(
                "worker_unavailable", "missing worker resource: " + ", ".join(missing)
            )
        request_id = str(uuid4())
        request_payload = dict(payload or {})
        if profile is not None:
            if profile.provider != spec.provider:
                raise ValueError("profile provider does not match worker")
            request_payload.setdefault("credentials", dict(profile.credentials))
            request_payload.setdefault("settings", dict(profile.settings))
            if self.state_store is not None and "session" not in request_payload:
                session = self.state_store.load(profile, "session")
                if session is not None:
                    request_payload["session"] = session
        request = {
            "request_id": request_id,
            "operation": operation,
            "payload": request_payload,
        }
        secret_values = self._secret_values(request_payload)
        provider_log_root = (
            self.logs_root / spec.provider / (profile.id if profile else "diagnostics")
        )
        provider_log_root.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S.%fZ")
        log_path = provider_log_root / f"{stamp}-{request_id}.jsonl"
        creation_flags = 0
        start_new_session = os.name != "nt"
        if os.name == "nt":
            creation_flags = subprocess.CREATE_NEW_PROCESS_GROUP
        process = subprocess.Popen(
            spec.command(self.registry.python),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=self.registry.environment_for(spec),
            creationflags=creation_flags,
            start_new_session=start_new_session,
        )
        assert (
            process.stdin is not None and process.stdout is not None and process.stderr is not None
        )
        process.stdin.write(json.dumps(request, ensure_ascii=True, separators=(",", ":")) + "\n")
        process.stdin.flush()
        process.stdin.close()
        events: list[dict[str, Any]] = []
        stderr_parts: list[str] = []
        stdout_lines: queue.Queue[str | None] = queue.Queue()

        def consume_stdout() -> None:
            for output_line in process.stdout:
                stdout_lines.put(output_line)
            stdout_lines.put(None)

        def consume_stderr() -> None:
            stderr_parts.append(process.stderr.read())

        stderr_thread = threading.Thread(target=consume_stderr, daemon=True)
        stdout_thread = threading.Thread(target=consume_stdout, daemon=True)
        stderr_thread.start()
        stdout_thread.start()
        started = time.monotonic()
        terminal: dict[str, Any] | None = None
        try:
            with log_path.open("w", encoding="utf-8", newline="\n") as log:
                while True:
                    if cancel is not None and cancel.is_set():
                        self._stop_owned_process(process)
                        raise RunnerError("cancelled", "run was cancelled", tuple(events))
                    if time.monotonic() - started > timeout:
                        self._stop_owned_process(process)
                        raise RunnerError(
                            "timeout", f"worker exceeded {timeout:g} seconds", tuple(events)
                        )
                    try:
                        line = stdout_lines.get(timeout=0.05)
                    except queue.Empty:
                        line = ""
                    if line is None:
                        if process.poll() is not None:
                            break
                    elif line:
                        try:
                            event = json.loads(line)
                        except json.JSONDecodeError as error:
                            self._stop_owned_process(process)
                            raise RunnerError(
                                "protocol_invalid", f"invalid worker JSON: {error}", tuple(events)
                            ) from error
                        if not isinstance(event, dict) or event.get("request_id") != request_id:
                            self._stop_owned_process(process)
                            raise RunnerError(
                                "protocol_invalid", "worker event identity mismatch", tuple(events)
                            )
                        event.pop("protocol", None)
                        log.write(
                            json.dumps(
                                self._event_for_log(event),
                                ensure_ascii=False,
                                separators=(",", ":"),
                            )
                            + "\n"
                        )
                        log.flush()
                        events.append(event)
                        if on_event is not None:
                            on_event(dict(event))
                        if event.get("type") in {"result", "error"}:
                            terminal = event
                            break
            try:
                return_code = process.wait(timeout=2)
            except subprocess.TimeoutExpired as error:
                self._stop_owned_process(process)
                raise RunnerError(
                    "protocol_invalid",
                    "worker did not exit after its terminal event",
                    tuple(events),
                ) from error
        finally:
            if process.poll() is None:
                self._stop_owned_process(process)
            stdout_thread.join(timeout=1)
            stderr_thread.join(timeout=1)
            process.stdout.close()
            process.stderr.close()
        if terminal is None:
            message = "worker exited without a terminal event"
            stderr = "".join(stderr_parts).strip()
            if stderr:
                message += f": {self._redact(stderr[-1000:], secret_values)}"
            raise RunnerError("worker_exited", message, tuple(events))
        if terminal.get("type") == "error":
            raise RunnerError(
                str(terminal.get("code") or "worker_error"),
                str(terminal.get("message") or "worker failed"),
                tuple(events),
            )
        if return_code != 0:
            raise RunnerError(
                "worker_exit_code", f"worker exited with code {return_code}", tuple(events)
            )
        data = terminal.get("data")
        if not isinstance(data, dict):
            raise RunnerError("protocol_invalid", "result data must be an object", tuple(events))
        if profile is not None and self.state_store is not None:
            session = data.get("session")
            if isinstance(session, dict):
                self.state_store.save(profile, "session", session)
        return RunResult(request_id, spec.provider, operation, data, tuple(events), log_path)

    @staticmethod
    def _secret_values(payload: dict[str, Any]) -> tuple[str, ...]:
        """Collect credential-like values for redacting donor stderr/errors."""
        values: set[str] = set()

        def collect(value: Any) -> None:
            if isinstance(value, str) and value:
                values.add(value)
            elif isinstance(value, dict):
                for child in value.values():
                    collect(child)
            elif isinstance(value, list):
                for child in value:
                    collect(child)

        collect(payload.get("credentials"))
        collect(payload.get("session"))
        settings = payload.get("settings")
        if isinstance(settings, dict):
            for key, value in settings.items():
                markers = ("password", "token", "cookie", "secret", "ticket", "api_key")
                if any(marker in str(key).casefold() for marker in markers):
                    collect(value)
        return tuple(sorted(values, key=len, reverse=True))

    @staticmethod
    def _redact(value: str, secrets: tuple[str, ...]) -> str:
        for secret in secrets:
            value = value.replace(secret, "[REDACTED]")
        return value

    @staticmethod
    def _event_for_log(event: dict[str, Any]) -> dict[str, Any]:
        if event.get("type") != "result":
            return dict(event)
        data = event.get("data")
        summary: dict[str, Any] = {}
        if isinstance(data, dict):
            summary["keys"] = sorted(str(key) for key in data if key != "session")
            for key, value in data.items():
                if isinstance(value, list):
                    summary[f"{key}_count"] = len(value)
        return {
            "request_id": event.get("request_id"),
            "operation": event.get("operation"),
            "type": "result",
            "data_summary": summary,
        }

    @staticmethod
    def _stop_owned_process(process: subprocess.Popen[str]) -> None:
        if process.poll() is not None:
            return
        try:
            if os.name == "nt":
                process.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=2)
            return
        except (OSError, subprocess.TimeoutExpired):
            pass
        if os.name == "nt":
            subprocess.run(
                ["taskkill.exe", "/PID", str(process.pid), "/T", "/F"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
        else:
            process.kill()
        with contextlib.suppress(subprocess.TimeoutExpired):
            process.wait(timeout=2)
