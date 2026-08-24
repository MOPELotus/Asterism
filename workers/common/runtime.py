"""Small shared process boundary for 0.0.1 upstream-backed workers.

This module deliberately knows nothing about Provider data or workflows.  It
only binds one request to one JSONL response, verifies the pinned donor entry,
and prevents donor stdout/stderr from corrupting the protocol stream.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import io
import json
import pathlib
import sys
import traceback
from dataclasses import dataclass
from typing import Any, Callable, Iterable, Mapping

MAX_REQUEST_BYTES = 4 * 1024 * 1024


class WorkerFailure(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


@dataclass(frozen=True)
class SourceMetadata:
    name: str
    repository: str
    revision: str
    entrypoint: str
    entrypoint_sha256: str
    license: str
    adapter_protocol: str

    @classmethod
    def load(cls, path: pathlib.Path, protocol: str) -> "SourceMetadata":
        try:
            result = cls(**json.loads(path.read_text(encoding="utf-8")))
        except (OSError, UnicodeError, json.JSONDecodeError, TypeError) as error:
            raise WorkerFailure("source_metadata_invalid", str(error)) from error
        if result.adapter_protocol != protocol:
            raise WorkerFailure("protocol_mismatch", result.adapter_protocol)
        return result

    def verify(self, upstream: pathlib.Path) -> pathlib.Path:
        entry = upstream / self.entrypoint if upstream.is_dir() else upstream
        try:
            digest = hashlib.sha256(entry.read_bytes()).hexdigest()
        except OSError as error:
            raise WorkerFailure("upstream_unavailable", str(error)) from error
        if digest != self.entrypoint_sha256:
            raise WorkerFailure(
                "upstream_integrity_mismatch",
                f"expected {self.entrypoint_sha256}, received {digest}",
            )
        return entry


def require_mapping(value: Any, name: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise WorkerFailure("request_invalid", f"{name} must be an object")
    return value


def require_text(value: Any, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise WorkerFailure("request_invalid", f"{name} must be a non-empty string")
    return value


class Redactor:
    def __init__(self, secrets: Iterable[str] = ()) -> None:
        self.secrets = tuple(sorted({x for x in secrets if x}, key=len, reverse=True))

    def text(self, value: object) -> str:
        result = str(value)
        for secret in self.secrets:
            result = result.replace(secret, "[REDACTED]")
        return result


def payload_secrets(payload: Mapping[str, Any]) -> list[str]:
    values: list[str] = []
    callback_url = payload.get("callback_url")
    if isinstance(callback_url, str):
        values.append(callback_url)
    for container_name in ("credentials", "session"):
        container = payload.get(container_name)
        if isinstance(container, Mapping):
            for value in container.values():
                if isinstance(value, str):
                    values.append(value)
                elif isinstance(value, list):
                    values.extend(str(item.get("value")) for item in value if isinstance(item, Mapping) and item.get("value"))
    return values


class Events:
    def __init__(self, protocol: str, request_id: str, operation: str) -> None:
        self.protocol, self.request_id, self.operation = protocol, request_id, operation

    def emit(self, event_type: str, **payload: Any) -> None:
        value = {"protocol": self.protocol, "request_id": self.request_id,
                 "operation": self.operation, "type": event_type, **payload}
        # JSONL is an ASCII wire format even when a Windows donor changes the
        # console code page. This keeps the Rust subprocess reader UTF-8 safe.
        sys.__stdout__.write(json.dumps(value, ensure_ascii=True, separators=(",", ":")) + "\n")
        sys.__stdout__.flush()


class _LogStream(io.TextIOBase):
    def __init__(self, events: Events, level: str, redactor: Redactor) -> None:
        self.events, self.level, self.redactor, self.buffered = events, level, redactor, ""

    def writable(self) -> bool:
        return True

    def write(self, value: str) -> int:
        self.buffered += value
        while "\n" in self.buffered:
            line, self.buffered = self.buffered.split("\n", 1)
            self._emit(line)
        return len(value)

    def flush(self) -> None:
        if self.buffered:
            self._emit(self.buffered)
            self.buffered = ""

    def _emit(self, line: str) -> None:
        line = self.redactor.text(line).strip()
        if line:
            self.events.emit("log", level=self.level, message=line)


@contextlib.contextmanager
def capture_output(events: Events, redactor: Redactor):
    stdout, stderr = _LogStream(events, "info", redactor), _LogStream(events, "error", redactor)
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        try:
            yield
        finally:
            stdout.flush(); stderr.flush()


def run(protocol: str, dispatch: Callable[[str, Mapping[str, Any], pathlib.Path, SourceMetadata, Events, Redactor], Mapping[str, Any]], argv=None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--upstream", required=True, type=pathlib.Path)
    parser.add_argument("--source-metadata", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    events, redactor = Events(protocol, "unbound", "unknown"), Redactor()
    try:
        raw = sys.stdin.buffer.readline(MAX_REQUEST_BYTES + 1)
        if not raw:
            raise WorkerFailure("request_missing", "expected one JSON line")
        if len(raw) > MAX_REQUEST_BYTES:
            raise WorkerFailure("request_too_large", "request exceeds worker input limit")
        request = require_mapping(json.loads(raw), "request")
        request_id = require_text(request.get("request_id"), "request.request_id")
        operation = require_text(request.get("operation"), "request.operation")
        payload = require_mapping(request.get("payload", {}), "request.payload")
        events = Events(protocol, request_id, operation)
        redactor = Redactor(payload_secrets(payload))
        metadata = SourceMetadata.load(args.source_metadata, protocol)
        entry = metadata.verify(args.upstream)
        data = dispatch(operation, payload, entry, metadata, events, redactor)
        events.emit("result", data=data)
        return 0
    except WorkerFailure as error:
        events.emit("error", code=error.code, message=redactor.text(error.message))
        return 2
    except Exception as error:  # pragma: no cover
        events.emit("error", code="worker_internal", message=redactor.text(error))
        sys.__stderr__.write(redactor.text(traceback.format_exc()))
        return 3
