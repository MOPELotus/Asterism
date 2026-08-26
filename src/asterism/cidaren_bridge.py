from __future__ import annotations

import json
import secrets
import threading
from collections.abc import Callable, Mapping
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlsplit


class _BridgeServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class CidarenAnswerBridge:
    """Short-lived loopback bridge from the cidaren donor to local answer policy."""

    def __init__(
        self,
        resolve: Callable[[dict[str, Any]], Mapping[str, Any]],
        observe: Callable[[dict[str, Any]], Mapping[str, Any] | None],
    ) -> None:
        self.ticket = secrets.token_urlsafe(32)
        self._resolve = resolve
        self._observe = observe
        bridge = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, _format: str, *_args: Any) -> None:
                return

            def do_POST(self) -> None:  # noqa: N802 - stdlib handler API
                if urlsplit(self.path).path != "/answer":
                    self._send(404, {"error": "not_found"})
                    return
                authorization = self.headers.get("Authorization", "")
                if authorization != f"Bearer {bridge.ticket}":
                    self._send(401, {"error": "unauthorized"})
                    return
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    if length < 0 or length > 2 * 1024 * 1024:
                        raise ValueError("request too large")
                    raw = self.rfile.read(length)
                    document = json.loads(raw.decode("utf-8"))
                    if not isinstance(document, dict):
                        raise ValueError("request must be an object")
                    kind = str(document.get("kind") or "")
                    if kind == "resolve_answer":
                        result = bridge._resolve(document)
                    elif kind == "answer_observation":
                        result = bridge._observe(document) or {"ok": True}
                    else:
                        result = {"error": "unsupported_kind"}
                    self._send(200, dict(result))
                except (
                    OSError,
                    TypeError,
                    ValueError,
                    UnicodeError,
                    json.JSONDecodeError,
                ) as error:
                    self._send(400, {"error": str(error)})

            def _send(self, status: int, value: Mapping[str, Any]) -> None:
                body = json.dumps(dict(value), ensure_ascii=False, separators=(",", ":")).encode(
                    "utf-8"
                )
                self.send_response(status)
                self.send_header("Content-Type", "application/json; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self._server = _BridgeServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            name="asterism-cidaren-answer-bridge",
            daemon=True,
        )
        self._thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self._server.server_port}/answer"

    def settings(self, *, execution_id: str, task_id: str, remote_task_id: str) -> dict[str, Any]:
        return {
            "url": self.url,
            "ticket": self.ticket,
            "execution_id": execution_id,
            "task_id": task_id,
            "remote_task_id": remote_task_id,
        }

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=2)
