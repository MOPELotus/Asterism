#!/usr/bin/env python3
"""Thin JSON Lines adapter around the pinned UAI Python donor.

The donor owns authentication, protocol requests, decryption, task traversal,
answer encoding, submission, and progress semantics. This module only replaces
interactive configuration with a process boundary suitable for Asterism.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import math
import os
import pathlib
import shutil
import sys
import time
import traceback
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from types import ModuleType
from typing import Any

PROTOCOL = "asterism.uai.worker.v1"
MAX_REQUEST_BYTES = 4 * 1024 * 1024


class WorkerFailure(Exception):
    """A controlled failure safe to return across the worker boundary."""

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
    def load(cls, path: pathlib.Path) -> SourceMetadata:
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
            metadata = cls(**value)
        except (OSError, UnicodeError, json.JSONDecodeError, TypeError) as error:
            raise WorkerFailure("source_metadata_invalid", str(error)) from error
        if metadata.adapter_protocol != PROTOCOL:
            raise WorkerFailure(
                "protocol_mismatch",
                f"source metadata requires {metadata.adapter_protocol!r}",
            )
        return metadata


class EventWriter:
    """Writes protocol events without being captured by donor stdout redirection."""

    def __init__(self, request_id: str, operation: str) -> None:
        self.request_id = request_id
        self.operation = operation

    def emit(self, event_type: str, **payload: Any) -> None:
        event = {
            "protocol": PROTOCOL,
            "request_id": self.request_id,
            "operation": self.operation,
            "type": event_type,
            **payload,
        }
        sys.__stdout__.write(json.dumps(event, ensure_ascii=True, separators=(",", ":")))
        sys.__stdout__.write("\n")
        sys.__stdout__.flush()


class Redactor:
    def __init__(self, secrets: Iterable[str] = ()) -> None:
        self._secrets = tuple(
            sorted(
                {secret for secret in secrets if isinstance(secret, str) and secret},
                key=len,
                reverse=True,
            )
        )

    def text(self, value: object) -> str:
        redacted = str(value)
        for secret in self._secrets:
            redacted = redacted.replace(secret, "[REDACTED]")
        return redacted


class DonorLogStream(io.TextIOBase):
    def __init__(self, events: EventWriter, level: str, redactor: Redactor) -> None:
        self._events = events
        self._level = level
        self._redactor = redactor
        self._buffer = ""

    def writable(self) -> bool:
        return True

    def write(self, value: str) -> int:
        self._buffer += value
        while "\n" in self._buffer:
            line, self._buffer = self._buffer.split("\n", 1)
            self._emit(line)
        return len(value)

    def flush(self) -> None:
        if self._buffer:
            self._emit(self._buffer)
            self._buffer = ""

    def _emit(self, line: str) -> None:
        message = self._redactor.text(line).strip()
        if message:
            self._events.emit("log", level=self._level, message=message)


@contextlib.contextmanager
def capture_donor_output(events: EventWriter, redactor: Redactor):
    stdout = DonorLogStream(events, "info", redactor)
    stderr = DonorLogStream(events, "error", redactor)
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        try:
            yield
        finally:
            stdout.flush()
            stderr.flush()


def load_upstream(
    script_path: pathlib.Path,
    metadata: SourceMetadata,
    events: EventWriter,
    redactor: Redactor,
) -> ModuleType:
    try:
        script_bytes = script_path.read_bytes()
    except OSError as error:
        raise WorkerFailure("upstream_unavailable", str(error)) from error
    digest = hashlib.sha256(script_bytes).hexdigest()
    if digest != metadata.entrypoint_sha256:
        raise WorkerFailure(
            "upstream_integrity_mismatch",
            f"expected {metadata.entrypoint_sha256}, received {digest}",
        )

    spec = importlib.util.spec_from_file_location("asterism_uai_upstream", script_path)
    if spec is None or spec.loader is None:
        raise WorkerFailure("upstream_load_failed", "could not create module loader")
    module = importlib.util.module_from_spec(spec)
    try:
        with capture_donor_output(events, redactor):
            spec.loader.exec_module(module)
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", error.name or str(error)) from error
    except Exception as error:
        raise WorkerFailure("upstream_load_failed", redactor.text(error)) from error

    required_methods = (
        "login",
        "fetch_structure",
        "fetch_task_completion",
        "collect_groups",
        "get_content",
        "get_answer",
        "build_simple_submit_body",
        "submit",
        "process_task",
    )
    donor_class = getattr(module, "UnipusBot", None)
    missing = [name for name in required_methods if not hasattr(donor_class, name)]
    if missing:
        raise WorkerFailure(
            "upstream_shape_mismatch",
            f"UnipusBot is missing methods: {', '.join(missing)}",
        )
    return module


def require_mapping(value: Any, name: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise WorkerFailure("request_invalid", f"{name} must be an object")
    return value


def require_text(value: Any, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise WorkerFailure("request_invalid", f"{name} must be a non-empty string")
    return value


def request_secrets(payload: Mapping[str, Any]) -> list[str]:
    values: list[str] = []
    credentials = payload.get("credentials")
    if isinstance(credentials, Mapping):
        values.extend(
            str(credentials[key])
            for key in ("username", "password")
            if isinstance(credentials.get(key), str)
        )
    session = payload.get("session")
    if isinstance(session, Mapping):
        authorization = session.get("authorization")
        if isinstance(authorization, str):
            values.append(authorization)
        cookies = session.get("cookies")
        if isinstance(cookies, list):
            values.extend(
                str(cookie["value"])
                for cookie in cookies
                if isinstance(cookie, Mapping) and isinstance(cookie.get("value"), str)
            )
    return values


def new_bot(module: ModuleType):
    return module.UnipusBot()


def serialize_session(bot: Any) -> dict[str, Any]:
    authorization = bot.session.headers.get("Authorization")
    if not isinstance(authorization, str) or not authorization:
        raise WorkerFailure("session_invalid", "donor did not return Authorization")
    cookies = []
    for cookie in bot.session.cookies:
        cookies.append(
            {
                "name": cookie.name,
                "value": cookie.value,
                "domain": cookie.domain,
                "path": cookie.path,
                "secure": bool(cookie.secure),
                "expires": cookie.expires,
            }
        )
    return {
        "authorization": authorization,
        "cookies": cookies,
        "open_id": bot.open_id,
        "user_id": bot.user_id,
        "sso_id": bot.sso_id,
    }


def restore_session(bot: Any, value: Any) -> None:
    session = require_mapping(value, "payload.session")
    authorization = require_text(session.get("authorization"), "session.authorization")
    bot.session.headers["Authorization"] = authorization
    bot.open_id = require_text(session.get("open_id"), "session.open_id")
    bot.user_id = session.get("user_id")
    bot.sso_id = session.get("sso_id")
    cookies = session.get("cookies", [])
    if not isinstance(cookies, list):
        raise WorkerFailure("request_invalid", "session.cookies must be an array")
    for cookie in cookies:
        item = require_mapping(cookie, "session cookie")
        bot.session.cookies.set(
            require_text(item.get("name"), "cookie.name"),
            require_text(item.get("value"), "cookie.value"),
            domain=item.get("domain") or None,
            path=item.get("path") or "/",
            secure=bool(item.get("secure", False)),
            expires=item.get("expires"),
        )


def authenticate(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    credentials = require_mapping(payload.get("credentials"), "payload.credentials")
    username = require_text(credentials.get("username"), "credentials.username")
    password = require_text(credentials.get("password"), "credentials.password")
    module.ACCOUNT_USERNAME = username
    module.ACCOUNT_PASSWORD = password
    module.AI_ENABLED = False
    redactor = Redactor((username, password))
    bot = new_bot(module)
    with capture_donor_output(events, redactor):
        success = bot.login()
    if not success:
        raise WorkerFailure("authentication_failed", "upstream login was rejected")
    return {"session": serialize_session(bot)}


def list_courses(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    bot = new_bot(module)
    restore_session(bot, payload.get("session"))
    redactor = Redactor(request_secrets(payload))
    url = f"https://{module.UAI_HOST}/api/cmgt/course/getCourseListByStudent"
    try:
        with capture_donor_output(events, redactor):
            response = bot.session.get(url)
            data = response.json()
    except Exception as error:
        raise WorkerFailure("courses_failed", redactor.text(error)) from error
    rows = data.get("value", {}).get("courseList", [])
    if not isinstance(rows, list):
        raise WorkerFailure("upstream_shape_mismatch", "courseList is not an array")
    courses: list[dict[str, Any]] = []
    for course in rows:
        if not isinstance(course, Mapping):
            continue
        resources = course.get("courseResourceList", [])
        if not isinstance(resources, list):
            continue
        for resource in resources:
            if not isinstance(resource, Mapping) or resource.get("id") is None:
                continue
            courses.append(
                {
                    "remote_id": str(resource["id"]),
                    "title": " - ".join(
                        part
                        for part in (str(course.get("name", "")).strip(), str(resource.get("name", "")).strip())
                        if part
                    ),
                    "native": {
                        "resource_id": resource["id"],
                        "class_id": course.get("classId", ""),
                        "curricula_id": str(course.get("id", "")),
                        "course_name": course.get("name", ""),
                        "resource_name": resource.get("name", ""),
                    },
                }
            )
    return {"courses": courses, "session": serialize_session(bot)}


def select_course(bot: Any, module: ModuleType, course_value: Any, events: EventWriter, redactor: Redactor) -> None:
    course = require_mapping(course_value, "payload.course")
    resource_id = course.get("resource_id")
    if resource_id is None:
        raise WorkerFailure("request_invalid", "course.resource_id is required")
    bot.resource_id = resource_id
    bot.class_id = course.get("class_id", "")
    bot.curricula_id = str(course.get("curricula_id", ""))
    url = f"https://{module.UAI_HOST}/api/cmgt/course/getCourseResourceInfoById/{resource_id}"
    try:
        with capture_donor_output(events, redactor):
            detail = bot.session.get(url).json()
        bot.course_instance_id = detail["value"]["courseResource"]["courseInstanceId"]
    except Exception as error:
        raise WorkerFailure("course_select_failed", redactor.text(error)) from error


def task_path(group: Mapping[str, Any], ancestors: Iterable[Any]) -> str:
    parts: list[str] = []
    for node in ancestors:
        if not isinstance(node, Mapping):
            continue
        if node.get("role") in ("unit", "section", "node"):
            name = str(node.get("name", "")).strip()
            caption = str(node.get("caption", "")).strip()
            label = f"[{caption}] {name}".strip() if caption else name
            if label:
                parts.append(label)
    name = str(group.get("name", "")).strip()
    if name:
        parts.append(name)
    return " > ".join(parts)


def prepared_course_bot(module: ModuleType, payload: Mapping[str, Any], events: EventWriter):
    bot = new_bot(module)
    restore_session(bot, payload.get("session"))
    redactor = Redactor(request_secrets(payload))
    course = payload.get("course")
    if course is None and isinstance(payload.get("task"), Mapping):
        native = payload["task"].get("native")
        if isinstance(native, Mapping):
            course = native.get("course")
    select_course(bot, module, course, events, redactor)
    with capture_donor_output(events, redactor):
        if not bot.fetch_structure():
            raise WorkerFailure("task_inventory_failed", "upstream course structure was empty")
        bot.fetch_task_completion()
    return bot


def task_situation_records(bot: Any, module: ModuleType, course_native: Mapping[str, Any],
                           groups: list[tuple[Any, Any]], events: EventWriter,
                           redactor: Redactor) -> dict[str, Mapping[str, Any]]:
    """Read the official required/scoring record once per unit for scheduling policy."""
    resource_id = course_native.get("resource_id")
    if resource_id is None:
        return {}
    unit_ids = []
    for _group, ancestors in groups:
        unit_id = next((str(node.get("id") or node.get("nodeId")) for node in ancestors
                        if isinstance(node, Mapping) and node.get("role") == "unit"
                        and (node.get("id") is not None or node.get("nodeId") is not None)), None)
        if unit_id and unit_id not in unit_ids:
            unit_ids.append(unit_id)
    records: dict[str, Mapping[str, Any]] = {}

    def visit(value: Any) -> None:
        if isinstance(value, Mapping):
            node_id = value.get("nodeId")
            if node_id is not None:
                records[str(node_id)] = value
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)

    url = f"https://{module.UAI_HOST}/api/tla/learningDetail/studyRecord/unitTaskSituation"
    for unit_id in unit_ids:
        try:
            with capture_donor_output(events, redactor):
                response = bot.session.get(url, params={
                    "nodeId": unit_id, "id": resource_id,
                    "appUserId": bot.user_id, "ssoId": bot.sso_id,
                })
                data = response.json()
            if data.get("success") is True:
                visit(data.get("value", {}).get("list", []))
        except Exception as error:
            events.emit("log", level="warning", message=redactor.text(error))
    return records


def list_tasks(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    bot = prepared_course_bot(module, payload, events)
    course_native = dict(require_mapping(payload.get("course"), "payload.course"))
    groups = bot.collect_groups()
    redactor = Redactor(request_secrets(payload))
    situations = task_situation_records(bot, module, course_native, groups, events, redactor)
    tasks = []
    for group, ancestors in groups:
        task_id = str(group.get("id", ""))
        if not task_id:
            continue
        base = str(group.get("base", ""))
        category = bot.classify_task(base)
        situation = situations.get(task_id, {})
        tasks.append(
            {
                "remote_id": task_id,
                "title": str(group.get("name", "")) or task_path(group, ancestors),
                "path": task_path(group, ancestors),
                "state": "completed" if bot.task_completion.get(task_id) is True else "pending",
                "source_type": "discussion" if category == "discussion" else "resource",
                "capabilities": ["run", "duration_read"],
                "native": {
                    "course": course_native,
                    "group": group,
                    "base": base,
                    "category": category,
                    "required": situation.get("required"),
                    "score_task": situation.get("scoreTaskFlag"),
                    "finish_progress": situation.get("finishProgress"),
                    "task_score": situation.get("taskQuesTotalScore"),
                    "question_num": group.get("question_num", 1),
                    "unit_id": next((str(node.get("id") or node.get("nodeId")) for node in ancestors
                                     if isinstance(node, Mapping) and node.get("role") == "unit"
                                     and (node.get("id") is not None or node.get("nodeId") is not None)), None),
                },
            }
        )
    if os.environ.get("ASTERISM_UAI_BROWSER_UPSTREAM"):
        tasks.append({
            "remote_id": f"course-duration:{course_native['resource_id']}",
            "title": "课程学习时长（上游页面驻留）",
            "path": "课程学习时长",
            "state": "pending",
            "source_type": "resource",
            "capabilities": ["duration", "duration_read"],
            "native": {"route_kind": "course_duration", "course": course_native},
        })
    return {"tasks": tasks, "session": serialize_session(bot)}


def inspect_task(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    bot = prepared_course_bot(module, payload, events)
    target = require_mapping(payload.get("task"), "payload.task")
    # Older persisted tasks (and the first thin-worker fixtures) do not carry
    # the optional situation summary yet.  Fresh inspection must remain able
    # to rebind those tasks and simply leave the summary fields unknown.
    target_native_value = target.get("native")
    target_native = (require_mapping(target_native_value, "task.native")
                     if target_native_value is not None else {})
    remote_id = require_text(target.get("remote_id"), "task.remote_id")
    selected = None
    for group, ancestors in bot.collect_groups():
        if str(group.get("id", "")) == remote_id:
            selected = (group, ancestors)
            break
    if selected is None:
        raise WorkerFailure("task_not_found", "fresh upstream inventory did not contain the task")
    group, ancestors = selected
    redactor = Redactor(request_secrets(payload))
    with capture_donor_output(events, redactor):
        content = bot.get_content(remote_id)
        answers = bot.get_answer(remote_id)
    base = str(group.get("base", ""))
    return {
        "task": {
            "remote_id": remote_id,
            "title": str(group.get("name", "")),
            "path": task_path(group, ancestors),
            "state": "completed" if bot.task_completion.get(remote_id) is True else "pending",
            "native": {
                "group": group,
                "base": base,
                "category": bot.classify_task(base),
                "required": target_native.get("required"),
                "score_task": target_native.get("score_task"),
                "finish_progress": target_native.get("finish_progress"),
                "task_score": target_native.get("task_score"),
                "question_num": target_native.get("question_num"),
                "content": content,
                "provider_native_answers": answers,
            },
        },
        "session": serialize_session(bot),
    }


def _walk_question_nodes(value: Any):
    if isinstance(value, Mapping):
        keys = {str(key).lower() for key in value}
        if value.get("id") is not None and keys.intersection({"question", "title", "stem", "content", "options", "answer"}):
            yield value
        for child in value.values():
            yield from _walk_question_nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from _walk_question_nodes(child)


def _uai_question_kind(node: Mapping[str, Any], task_base: str) -> str:
    native = " ".join(str(value) for value in (
        node.get("questionType"), node.get("question_type"), node.get("componentType"),
        node.get("type"), node.get("base"), task_base,
    ) if value).lower()
    if "multiple" in native or "multi" in native:
        return "multiple_choice"
    if "single" in native or "choice" in native:
        return "single_choice"
    if "judge" in native or "true" in native:
        return "true_false"
    if "blank" in native or "fill" in native:
        return "fill_blank"
    if "short_answer" in native or "short-answer" in native or "short answer" in native:
        return "short_answer"
    if "oral" in native or "speak" in native or "record" in native:
        return "provider_native_oral"
    if "match" in native:
        return "matching"
    if "order" in native or "sequence" in native:
        return "ordering"
    return "provider_native"


def _native_shape(node: Mapping[str, Any], task_base: str) -> dict[str, Any]:
    option_value = (node.get("options") or node.get("choices") or node.get("option")
                    or node.get("items"))
    option_keys = []
    if isinstance(option_value, list):
        option_keys = sorted({str(key) for item in option_value if isinstance(item, Mapping)
                              for key in item.keys()})[:32]
    return {
        "task_base": task_base,
        "native_type": str(node.get("type") or node.get("questionType")
                           or node.get("question_type") or node.get("componentType") or ""),
        "keys": sorted(str(key) for key in node.keys())[:64],
        "option_container": type(option_value).__name__ if option_value is not None else None,
        "option_keys": option_keys,
    }


def scan_questions(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    inspected = inspect_task(module, payload, events)
    native = inspected["task"]["native"]
    content = native.get("content")
    evidence = native.get("provider_native_answers")
    answer_by_id = {}
    if isinstance(evidence, list):
        answer_by_id = {str(item.get("id")): item for item in evidence if isinstance(item, Mapping) and item.get("id") is not None}
    questions = []
    seen = set()
    for node in _walk_question_nodes(content):
        remote_id = str(node.get("id"))
        if remote_id in seen:
            continue
        seen.add(remote_id)
        prompt = (node.get("question") or node.get("title") or node.get("stem")
                  or node.get("quesText") or node.get("content") or "")
        if not isinstance(prompt, (str, int, float, bool)):
            prompt = ""
        questions.append({
            "remote_id": remote_id,
            "position": len(questions) + 1,
            "kind": _uai_question_kind(node, str(native.get("base", ""))),
            "prompt": str(prompt),
            "options": (node.get("options") or node.get("choices") or node.get("option")
                        or node.get("items") or []),
            "answer_evidence": answer_by_id.get(remote_id),
            "native_shape": _native_shape(node, str(native.get("base", ""))),
            "native": node,
        })
    return {"questions": questions, "provider_native_content": content if not questions else None,
            "provider_native_answers": evidence, "session": inspected["session"]}


def run_task(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    """Execute exactly one fresh-bound group through the donor's process_task."""
    target = require_mapping(payload.get("task"), "payload.task")
    native = target.get("native")
    if isinstance(native, Mapping) and native.get("route_kind") == "course_duration":
        return run_course_residence(module, payload, events)
    bot = prepared_course_bot(module, payload, events)
    remote_id = require_text(target.get("remote_id"), "task.remote_id")
    selected = next(((group, ancestors) for group, ancestors in bot.collect_groups()
                     if str(group.get("id", "")) == remote_id), None)
    if selected is None:
        raise WorkerFailure("task_not_found", "fresh upstream inventory did not contain the task")

    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    native_record = native if isinstance(native, Mapping) else {}
    required = native_record.get("required") is True
    generated_text = settings.get("generated_text")
    if generated_text is not None:
        generated_text = str(generated_text).strip()
        if not generated_text or len(generated_text.encode("utf-8")) > 16 * 1024:
            raise WorkerFailure("request_invalid", "generated_text is empty or oversized")
        module.retry_call_ai = lambda *_args, **_kwargs: generated_text
    # Keep the donor dispatch intact. Provider-native answers stay first; an
    # Asterism-generated plain-text answer is used only when explicitly passed.
    module.AI_ENABLED = bool(generated_text)
    module.SUBJECTIVE_ALLOW_EMPTY = False
    module.DISCUSSION_AI = bool(generated_text)
    module.DISCUSSION_ALLOW_EMPTY = False
    module.DISCUSSION_SKIP_CONTENT_SUBMIT_EMPTY = False
    module.UPLOAD_EMPTY_FILE = required
    module.SKIP_ORAL_TYPES = not required
    module.ORAL_SUBMIT_EMPTY = required
    module.SKIP_INVALID = True
    module.COMPOUND_ALLOW_EMPTY = required
    module.COOLDOWN_COUNT = int(settings.get("cooldown_count", getattr(module, "COOLDOWN_COUNT", 5)))
    module.COOLDOWN_SECONDS = int(settings.get("cooldown_seconds", getattr(module, "COOLDOWN_SECONDS", 120)))
    if module.COOLDOWN_COUNT < 1 or module.COOLDOWN_SECONDS < 0:
        raise WorkerFailure("request_invalid", "UAI cooldown settings are invalid")
    redactor = Redactor(request_secrets(payload))
    events.emit("progress", current=0, total=1)
    with capture_donor_output(events, redactor):
        accepted = bool(bot.process_task(*selected))
        bot.fetch_task_completion()
    completed = bot.task_completion.get(remote_id) is True
    if not accepted:
        raise WorkerFailure("execution_skipped", "upstream safely skipped this task type")
    events.emit("progress", current=1, total=1)
    return {
        "remote_state": "completed" if completed else "in_progress",
        "verified": completed,
        "result": {
            "upstream_accepted": True,
            "fresh_completion_observed": completed,
            "category": bot.classify_task(str(selected[0].get("base", ""))),
            "required": required,
            "used_generated_text": bool(generated_text),
        },
        "session": serialize_session(bot),
    }


def browser_donor_source() -> pathlib.Path:
    root_value = os.environ.get("ASTERISM_UAI_BROWSER_UPSTREAM")
    if not root_value:
        raise WorkerFailure("browser_required", "UAI page-residence donor is not configured")
    root = pathlib.Path(root_value)
    if not root.is_absolute():
        root = pathlib.Path(__file__).resolve().parents[2] / root
    manifest_path = pathlib.Path(
        os.environ.get("ASTERISM_UAI_BROWSER_SOURCE_METADATA")
        or pathlib.Path(__file__).with_name("BROWSER_SOURCE.json")
    )
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        source = root.resolve() / manifest["entrypoint"]
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        if digest != manifest["entrypoint_sha256"]:
            raise WorkerFailure("upstream_integrity_mismatch", "UAI browser donor hash changed")
        return source
    except WorkerFailure:
        raise
    except (OSError, KeyError, TypeError, ValueError) as error:
        raise WorkerFailure("browser_upstream_invalid", str(error)) from error


def find_browser(configured: str | None = None) -> pathlib.Path | None:
    """Find a Chromium-family executable without requiring PATH setup."""
    values = [configured, os.environ.get("ASTERISM_UAI_BROWSER_EXECUTABLE")]
    if os.name == "nt":
        roots = [
            os.environ.get("PROGRAMFILES"),
            os.environ.get("PROGRAMFILES(X86)"),
            os.environ.get("LOCALAPPDATA"),
        ]
        values.extend(
            str(pathlib.Path(root) / relative)
            for root in roots
            if root
            for relative in (
                "Microsoft/Edge/Application/msedge.exe",
                "Google/Chrome/Application/chrome.exe",
                "Chromium/Application/chrome.exe",
            )
        )
    for name in ("msedge", "msedge.exe", "chrome", "chrome.exe", "chromium", "chromium.exe"):
        values.append(shutil.which(name))
    for value in values:
        if not value:
            continue
        path = pathlib.Path(value).expanduser()
        if not path.is_absolute():
            path = pathlib.Path(__file__).resolve().parents[2] / path
        if path.is_file():
            return path.resolve()
    return None


def course_duration_total(bot, module, resource_id, events, redactor) -> int:
    url = f"https://{module.UAI_HOST}/api/tla/learningDetail/studyRecord/totalAndUnitSituation"
    with capture_donor_output(events, redactor):
        data = bot.session.get(url, params={"id": resource_id, "appUserId": bot.user_id}).json()
    duration = data.get("value", {}).get("totalDetail", {}).get("duration")
    if not isinstance(duration, int) or isinstance(duration, bool) or duration < 0:
        raise WorkerFailure("upstream_shape_mismatch", "course duration was not nonnegative seconds")
    return duration


def run_course_residence(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    """Run the audited userscript in its original rendered-page environment."""
    task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native")
    course = require_mapping(native.get("course"), "task.native.course")
    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    seconds = settings.get("duration_seconds", 60)
    if not isinstance(seconds, int) or isinstance(seconds, bool) or seconds < 60 or seconds > 3600:
        raise WorkerFailure("request_invalid", "UAI residence must be 60..3600 seconds")
    minutes = math.ceil(seconds / 60)
    source = browser_donor_source()
    configured_browser = settings.get("browser_executable")
    browser_path = find_browser(str(configured_browser) if configured_browser else None)
    if browser_path is None:
        raise WorkerFailure(
            "browser_unavailable",
            "no Edge, Chrome, Chromium, or configured browser executable was found",
        )

    bot = new_bot(module)
    restore_session(bot, payload.get("session"))
    redactor = Redactor(request_secrets(payload))
    select_course(bot, module, course, events, redactor)
    before = course_duration_total(bot, module, course["resource_id"], events, redactor)
    try:
        from playwright.sync_api import sync_playwright
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", "playwright") from error

    events.emit("progress", current=0, total=seconds)
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=str(browser_path), headless=True)
            try:
                context = browser.new_context(extra_http_headers={
                    "Authorization": bot.session.headers.get("Authorization", ""),
                })
                browser_cookies = []
                for cookie in bot.session.cookies:
                    row = {"name": cookie.name, "value": cookie.value,
                           "domain": cookie.domain or ".unipus.cn", "path": cookie.path or "/",
                           "secure": bool(cookie.secure)}
                    if cookie.expires:
                        row["expires"] = float(cookie.expires)
                    browser_cookies.append(row)
                if browser_cookies:
                    context.add_cookies(browser_cookies)
                context.add_init_script(source.read_text(encoding="utf-8"))
                page = context.new_page()
                page.goto(
                    f"https://{module.UAI_HOST}/_explorationpc_default/pc.html?cid={course['resource_id']}",
                    wait_until="domcontentloaded", timeout=60_000,
                )
                page.locator("#unipus-time-input").wait_for(state="visible", timeout=60_000)
                page.locator("#unipus-time-input").fill(str(minutes))
                start = page.get_by_role("button", name="🚀 开始刷课")
                start.wait_for(state="visible", timeout=60_000)
                deadline = time.monotonic() + 60
                while start.is_disabled() and time.monotonic() < deadline:
                    page.wait_for_timeout(500)
                if start.is_disabled():
                    raise WorkerFailure("browser_shape_mismatch", "UAI donor did not discover a course menu")
                start.click()
                started_at = time.monotonic()
                deadline = started_at + seconds + 180
                while time.monotonic() < deadline:
                    log_text = page.locator("#unipus-log").inner_text()
                    if "🎉 刷课完成" in log_text:
                        break
                    elapsed = min(seconds, int(time.monotonic() - started_at))
                    events.emit("progress", current=elapsed, total=seconds)
                    page.wait_for_timeout(1000)
                else:
                    raise WorkerFailure("browser_timeout", "UAI donor residence did not reach its terminal log")
            finally:
                browser.close()
    except WorkerFailure:
        raise
    except Exception as error:
        raise WorkerFailure("browser_execution_failed", redactor.text(error)) from error
    after = course_duration_total(bot, module, course["resource_id"], events, redactor)
    events.emit("progress", current=seconds, total=seconds)
    verified = after > before
    return {
        "remote_state": "completed" if verified else "in_progress", "verified": verified,
        "result": {"task_type": "course_duration", "requested_seconds": seconds,
                   "duration_before": before, "duration_after": after,
                   "donor_terminal_observed": True},
        "session": serialize_session(bot),
    }


def read_duration(module: ModuleType, payload: Mapping[str, Any], events: EventWriter) -> dict[str, Any]:
    """Read exact task duration or the donor-documented course total."""
    bot = prepared_course_bot(module, payload, events)
    task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native")
    remote_id = require_text(task.get("remote_id"), "task.remote_id")
    course = require_mapping(native.get("course"), "task.native.course")
    resource_id = course.get("resource_id")
    if resource_id is None:
        raise WorkerFailure("request_invalid", "task.native.course.resource_id is required")
    redactor = Redactor(request_secrets(payload))
    if native.get("route_kind") == "course_duration":
        return {
            "duration_seconds": course_duration_total(
                bot, module, resource_id, events, redactor
            ),
            "native_record": {"route_kind": "course_duration"},
        }

    unit_id = require_text(native.get("unit_id"), "task.native.unit_id")
    url = f"https://{module.UAI_HOST}/api/tla/learningDetail/studyRecord/unitTaskSituation"
    try:
        with capture_donor_output(events, redactor):
            response = bot.session.get(url, params={
                "nodeId": unit_id, "id": resource_id,
                "appUserId": bot.user_id, "ssoId": bot.sso_id,
            })
            data = response.json()
    except Exception as error:
        raise WorkerFailure("duration_read_failed", redactor.text(error)) from error
    if data.get("success") is not True:
        raise WorkerFailure("duration_read_failed", str(data.get("msg", "upstream rejected duration read")))

    matches = []

    def visit(value):
        if isinstance(value, Mapping):
            if str(value.get("nodeId", "")) == remote_id:
                matches.append(value)
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)

    visit(data.get("value", {}).get("list", []))
    if len(matches) != 1:
        raise WorkerFailure("upstream_shape_mismatch", "duration tree did not contain one exact task")
    duration = matches[0].get("duration")
    if not isinstance(duration, int) or isinstance(duration, bool) or duration < 0:
        raise WorkerFailure("upstream_shape_mismatch", "task duration was not nonnegative seconds")
    return {"duration_seconds": duration, "native_record": {
        key: matches[0].get(key) for key in
        ("finishProgress", "duration", "required", "scoreTaskFlag", "taskQuesTotalScore")
        if key in matches[0]
    }, "session": serialize_session(bot)}


def dispatch(module: ModuleType, operation: str, payload: Mapping[str, Any], events: EventWriter, metadata: SourceMetadata) -> dict[str, Any]:
    if operation == "health":
        return {
            "status": "ok",
            "source": {
                "name": metadata.name,
                "revision": metadata.revision,
                "license": metadata.license,
                "entrypoint_sha256": metadata.entrypoint_sha256,
            },
            "python": sys.version.split()[0],
            "operations": ["health", "authenticate", "courses", "tasks", "inspect", "questions", "run", "duration"],
        }
    if operation == "authenticate":
        return authenticate(module, payload, events)
    if operation == "courses":
        return list_courses(module, payload, events)
    if operation == "tasks":
        return list_tasks(module, payload, events)
    if operation == "inspect":
        return inspect_task(module, payload, events)
    if operation == "questions":
        return scan_questions(module, payload, events)
    if operation == "run":
        return run_task(module, payload, events)
    if operation == "duration":
        return read_duration(module, payload, events)
    raise WorkerFailure("operation_unsupported", f"unsupported operation {operation!r}")


def read_request() -> Mapping[str, Any]:
    raw = sys.stdin.buffer.readline(MAX_REQUEST_BYTES + 1)
    if len(raw) > MAX_REQUEST_BYTES:
        raise WorkerFailure("request_too_large", "request exceeds worker input limit")
    if not raw:
        raise WorkerFailure("request_missing", "expected one JSON line on stdin")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise WorkerFailure("request_invalid", str(error)) from error
    return require_mapping(value, "request")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--upstream", required=True, type=pathlib.Path)
    parser.add_argument(
        "--source-metadata",
        type=pathlib.Path,
        default=pathlib.Path(__file__).with_name("SOURCE.json"),
    )
    args = parser.parse_args(argv)

    request_id = "unbound"
    operation = "unknown"
    events = EventWriter(request_id, operation)
    redactor = Redactor()
    try:
        request = read_request()
        request_id = require_text(request.get("request_id"), "request.request_id")
        operation = require_text(request.get("operation"), "request.operation")
        payload = require_mapping(request.get("payload", {}), "request.payload")
        events = EventWriter(request_id, operation)
        redactor = Redactor(request_secrets(payload))
        metadata = SourceMetadata.load(args.source_metadata)
        module = load_upstream(args.upstream, metadata, events, redactor)
        events.emit("result", data=dispatch(module, operation, payload, events, metadata))
        return 0
    except WorkerFailure as error:
        events.emit("error", code=error.code, message=redactor.text(error.message))
        return 2
    except Exception as error:  # pragma: no cover - last-resort boundary
        events.emit("error", code="worker_internal", message=redactor.text(error))
        sys.__stderr__.write(redactor.text(traceback.format_exc()))
        sys.__stderr__.flush()
        return 3


if __name__ == "__main__":
    raise SystemExit(main())
