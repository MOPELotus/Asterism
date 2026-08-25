#!/usr/bin/env python3
"""Thin token/session adapter around MOPELotus/Easy_Cidaren."""

from __future__ import annotations

import importlib
import json
import pathlib
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from types import SimpleNamespace
from typing import Any, Mapping

sys.path.insert(0, str(pathlib.Path(__file__).parents[1]))
from common.runtime import (Events, Redactor, SourceMetadata, WorkerFailure,
                            capture_output, require_mapping, require_text, run)

PROTOCOL = "asterism.cidaren.worker.v1"
ANSWER_BRIDGE_PROTOCOL = "asterism.cidaren.answer-bridge.v1"


def _json_public(value, *, depth=0):
    """Return the donor's public exam shape without transport/session secrets."""
    if depth > 16:
        return None
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, Mapping):
        result = {}
        for key, child in value.items():
            name = str(key)
            lowered = name.lower()
            if any(marker in lowered for marker in ("token", "cookie", "authorization", "password", "secret", "ticket")):
                continue
            result[name] = _json_public(child, depth=depth + 1)
        return result
    if isinstance(value, (list, tuple)):
        return [_json_public(child, depth=depth + 1) for child in value]
    return str(value)


def _answer_bridge_settings(settings):
    nested = settings.get("answer_bridge") if isinstance(settings.get("answer_bridge"), Mapping) else {}
    url = str(nested.get("url") or settings.get("answer_bridge_url") or "").strip()
    ticket = str(nested.get("ticket") or settings.get("answer_bridge_ticket") or "").strip()
    execution_id = str(nested.get("execution_id") or settings.get("execution_id") or settings.get("answer_bridge_execution_id") or "").strip()
    task_id = str(nested.get("task_id") or settings.get("task_id") or settings.get("answer_bridge_task_id") or "").strip()
    remote_task_id = str(nested.get("remote_task_id") or settings.get("remote_task_id") or "").strip()
    expires_at = nested.get("expires_at") or settings.get("answer_bridge_expires_at")
    if not url:
        return None
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "http" or (parsed.hostname or "").lower() not in {"127.0.0.1", "::1", "localhost"}:
        raise WorkerFailure("request_invalid", "Cidaren answer bridge must use a loopback HTTP URL")
    if not ticket or not execution_id or not task_id:
        raise WorkerFailure("request_invalid", "Cidaren answer bridge requires ticket, execution_id, and task_id bindings")
    if expires_at not in (None, ""):
        try:
            # Contract uses Unix seconds (integer/float) to avoid timezone
            # ambiguity at the worker boundary.
            if float(expires_at) <= time.time():
                raise WorkerFailure("request_invalid", "Cidaren answer bridge ticket has expired")
        except (TypeError, ValueError) as error:
            raise WorkerFailure("request_invalid", "Cidaren answer_bridge_expires_at must be Unix seconds") from error
    return {"url": url, "ticket": ticket, "execution_id": execution_id, "task_id": task_id,
            "remote_task_id": remote_task_id, "expires_at": expires_at}


def _bridge_post(bridge, document, timeout):
    body = json.dumps(document, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    request = urllib.request.Request(
        bridge["url"], body, method="POST",
        headers={
            "Authorization": f"Bearer {bridge['ticket']}",
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        if getattr(response, "status", 200) < 200 or getattr(response, "status", 200) >= 300:
            return None
        raw = response.read(1024 * 1024)
    result = json.loads(raw.decode("utf-8")) if raw else None
    return result if isinstance(result, Mapping) else None


def _resolve_donor_answer(exam, value):
    if value is None:
        return None
    options = exam.get("options") or []
    option_rows = [row for row in options if isinstance(row, Mapping)]
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return int(value)
    if isinstance(value, (list, tuple, dict)):
        return value
    text_value = str(value).strip()
    if not text_value:
        return None
    for row in option_rows:
        tag = row.get("answer_tag")
        content = row.get("content")
        if text_value == str(tag) or text_value == str(content).strip():
            return tag if tag is not None else content
    # Normalized choice answers may be represented as A/B/C…; map those to
    # the donor's zero-based option index only when bounded.
    if len(text_value) == 1 and text_value.isalpha():
        index = ord(text_value.upper()) - ord("A")
        if 0 <= index < len(options):
            return index
    return text_value


def load(entry, events, redactor):
    root = entry.resolve().parents[1]
    sys.path.insert(0, str(root))
    try:
        with capture_output(events, redactor):
            return (importlib.import_module("api.login"), importlib.import_module("api.request_header"),
                    importlib.import_module("api.basic_api"), importlib.import_module("api.main_api"),
                    importlib.import_module("decryptencrypt.debase64"),
                    importlib.import_module("api.oauth"), importlib.import_module("api.runner"))
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", error.name or str(error)) from error


def restore(request_header, decoder, value):
    session = require_mapping(value, "payload.session")
    token = require_text(session.get("token"), "session.token")
    crypto_document = session.get("crypto_document")
    if crypto_document is not None:
        decoder.set_crypto_document(crypto_document)
    else:
        decoder.clear_crypto_document()
    request_header.set_token(token)
    return token


def authenticate(modules, payload, events, redactor):
    login, _, _, _, decoder, _, _ = modules
    credentials = require_mapping(payload.get("credentials"), "payload.credentials")
    token = require_text(credentials.get("token"), "credentials.token")
    crypto_document = credentials.get("crypto_document")
    if crypto_document is not None:
        decoder.set_crypto_document(require_mapping(crypto_document, "credentials.crypto_document"))
    with capture_output(events, redactor): result = login.verify_token(token)
    if not isinstance(result, Mapping): raise WorkerFailure("authentication_failed", f"upstream token validation failed ({result})")
    data = result.get("data", {}) if isinstance(result.get("data"), Mapping) else {}
    user = data.get("user_info", {}) if isinstance(data.get("user_info"), Mapping) else {}
    session = {"token": token}
    if crypto_document is not None:
        session["crypto_document"] = crypto_document
    return {"session": session, "account": {"display_name": user.get("student_name") or user.get("name"),
            "course_id": user.get("course_id")}}


def new_public():
    return SimpleNamespace(course_id="", all_unit=[], class_task=[], task_total_count=0,
        task_id="", release_id="", now_unit="", is_self_built=False, get_word_list_result={}, exam="",
        spend_min_time=1, spend_max_time=2, topic_code="")


def course_title(document, course_id):
    preferred_keys = ("course_name", "book_name", "course_title", "book_title", "title", "name")

    def visit(value):
        if isinstance(value, Mapping):
            value_course_id = value.get("course_id")
            if value_course_id in (None, "", course_id, str(course_id)):
                for key in preferred_keys:
                    candidate = value.get(key)
                    if isinstance(candidate, str) and candidate.strip():
                        return candidate.strip()
            for child in value.values():
                found = visit(child)
                if found:
                    return found
        elif isinstance(value, list):
            for child in value:
                found = visit(child)
                if found:
                    return found
        return None

    return visit(document) or f"词达人课程 {course_id}"


def class_task_capabilities(progress, is_complete):
    if progress >= 100 or is_complete:
        return ["run"]
    return ["questions", "run"]


def courses(modules, payload, events, redactor):
    login, request_header, basic, _, decoder, _, _ = modules; token = restore(request_header, decoder, payload.get("session"))
    with capture_output(events, redactor): result = login.verify_token(token)
    if not isinstance(result, Mapping): raise WorkerFailure("session_invalid", "token validation failed")
    user = result.get("data", {}).get("user_info", {})
    course_id = user.get("course_id")
    if course_id in (None, ""): raise WorkerFailure("course_shape_mismatch", "course_id missing")
    public = new_public()
    public.course_id = course_id
    with capture_output(events, redactor): basic.get_all_unit(public)
    title = course_title({"login": result.get("data", {}), "study": public.all_unit}, course_id)
    return {"courses": [{"remote_id": f"course:{course_id}", "title": title,
                          "native": {"course_id": course_id}}],
            "session": payload.get("session")}


def tasks(modules, payload, events, redactor):
    _, request_header, basic, main, decoder, _, _ = modules; token = restore(request_header, decoder, payload.get("session"))
    course = require_mapping(payload.get("course"), "payload.course"); public = new_public(); public.course_id = course.get("course_id")
    result = []
    with capture_output(events, redactor):
        basic.get_all_unit(public)
        units = public.all_unit.get("task_list", []) if isinstance(public.all_unit, Mapping) else public.all_unit
        for unit_index, unit in enumerate(units if isinstance(units, list) else []):
            if not isinstance(unit, Mapping): continue
            remote_id = str(unit.get("list_id") or unit.get("id") or unit_index + 1)
            progress = int(unit.get("progress") or 0)
            result.append({"remote_id": f"study-task:{public.course_id}:{remote_id}",
                           "global_remote_id": True, "source_type": "practice",
                           "assessment_class": "routine",
                           "title": str(unit.get("task_name") or unit.get("list_name") or unit.get("name") or remote_id),
                           "state": "completed" if progress >= 100 or unit.get("is_complete") else "pending",
                           "progress_percent": progress, "capabilities": ["run"],
                           "native": {"task_family": "unit", "course_id": public.course_id,
                                      "unit": {**unit, "course_id": public.course_id}}})
        page = 1
        while page == 1 or len(public.class_task) * 10 < int(public.task_total_count or 0):
            main.get_class_task(public, page); page += 1
            if page > 100: raise WorkerFailure("task_inventory_unbounded", "class task pagination exceeded 100 pages")
    for page_data in public.class_task:
        rows = page_data.get("records") or page_data.get("list") or page_data.get("rows") or page_data.get("data") or []
        for item in rows if isinstance(rows, list) else []:
            if not isinstance(item, Mapping): continue
            remote_id = str(item.get("release_id") or item.get("task_id") or item.get("id"))
            progress = int(item.get("progress") or 0)
            task_type = int(item.get("task_type") or 1)
            result.append({"remote_id": f"class-task:{remote_id}", "global_remote_id": True,
                           "source_type": "exam" if task_type == 2 else "practice",
                           # Cidaren class exams are independent assessments;
                           # preserve the upstream runner but require the same
                           # final human-confirmation boundary as other exams.
                           "assessment_class": "formal" if task_type == 2 else "routine",
                           "title": str(item.get("task_name") or item.get("name") or remote_id),
                           "state": "completed" if progress >= 100 or item.get("is_complete") else "pending",
                           "progress_percent": progress,
                           "capabilities": class_task_capabilities(progress, item.get("is_complete")),
                           "native": {"task_family": "class", "course_id": public.course_id,
                                      "task": {**item, "course_id": public.course_id}}})
    return {"tasks": result, "session": payload.get("session")}


def native_kind(exam):
    value = str(exam.get("topic_mode") or exam.get("topic_type") or exam.get("type") or "").lower()
    if value in ("1", "single", "choice"): return "single_choice"
    if value in ("2", "multiple"): return "multiple_choice"
    if value in ("3", "judge", "true_false"): return "true_false"
    if "match" in value: return "matching"
    if "order" in value or "sequence" in value: return "ordering"
    return "provider_native"


def historical_answer_evidence(exam, events, redactor):
    """Bind donor history to a freshly read question before Core import."""
    prompt = str(exam.get("topic_title") or exam.get("question") or exam.get("word") or "").strip()
    remark = str(exam.get("remark") or exam.get("word_zh") or "").strip()
    try:
        answer_lib = importlib.import_module("util.answer_lib")
        historical = answer_lib.lookup(prompt) if prompt else None
        lookup_key = prompt
        lookup_family = "phrase"
        if not historical and remark:
            historical = answer_lib.lookup_word(remark)
            lookup_key = remark
            lookup_family = "word"
        if isinstance(historical, list):
            values = [str(value).strip() for value in historical if str(value).strip()]
            if values:
                return {
                    "source": "cidaren_answer_lib",
                    "historical_values": values,
                    "value": "#".join(values),
                    "verified": False,
                    "import_mode": "identity_bound_lazy",
                    "lookup_family": lookup_family,
                    "lookup_key_present": bool(lookup_key),
                }
    except Exception as error:
        events.emit("log", level="debug", message=redactor.text(f"answer_lib unavailable: {error}"))
    return None


def questions(modules, payload, events, redactor):
    _, request_header, basic, main, decoder, _, _ = modules; token = restore(request_header, decoder, payload.get("session"))
    task = require_mapping(payload.get("task"), "payload.task"); native = require_mapping(task.get("native"), "task.native")
    public = new_public(); public.course_id = native.get("course_id")
    family = native.get("task_family")
    with capture_output(events, redactor):
        if family == "unit":
            unit = require_mapping(native.get("unit"), "task.native.unit")
            public.now_unit = unit.get("list_id") or unit.get("id")
            basic.get_unit_words(public)
            raw = public.get_word_list_result
            return {"questions": [], "provider_native_inventory": raw,
                    "scan_note": "unit vocabulary inventory retained as Provider-native; no answer session was started",
                    "session": payload.get("session")}
        if payload.get("allow_read_that_starts_attempt") is not True:
            raise WorkerFailure(
                "explicit_read_authorization_required",
                "Cidaren class question discovery calls StartAnswer and may create a remote attempt",
            )
        item = require_mapping(native.get("task"), "task.native.task")
        public.task_id = item.get("task_id") or item.get("id")
        public.release_id = item.get("release_id", "")
        task_type = int(item.get("task_type", 1))
        main.PublicInfo.task_type_int = task_type
        main.PublicInfo.task_type = "ClassTask" if task_type == 2 else "StudyTask"
        main.get_exam(public)
    if public.exam == "complete" or not isinstance(public.exam, Mapping):
        return {"questions": [], "scan_note": "donor reported no current question", "session": payload.get("session")}
    exam = public.exam
    prompt = exam.get("topic_title") or exam.get("question") or exam.get("word") or ""
    # Import the donor's historical answer library into the Asterism question
    # stream as evidence.  The Core decides whether it is reusable; this
    # worker never treats an answer-lib hit as proof of correctness.
    answer_evidence = historical_answer_evidence(exam, events, redactor)
    return {"questions": [{"remote_id": str(exam.get("topic_code") or exam.get("id") or "first"), "position": 1,
                            "kind": native_kind(exam), "prompt": str(prompt),
                            "options": exam.get("options") or exam.get("option") or [], "answer_evidence": answer_evidence,
                            "native_shape": {
                                "native_type": str(exam.get("topic_mode") or exam.get("topic_type") or exam.get("type") or ""),
                                "keys": sorted(str(key) for key in exam.keys())[:64],
                                "option_container": type(exam.get("options") or exam.get("option")).__name__,
                            },
                            "native": exam}],
            "scan_note": "StartAnswer reads only the first donor question and may establish a remote attempt; no answer/next/submit call is made",
            "session": payload.get("session")}


def oauth_begin(modules):
    authorization = modules[5].begin_authorization()
    return {
        "authorization_url": authorization.authorization_url,
        "state_digest": authorization.state_digest,
        "marker_digest": authorization.marker_digest,
    }


def oauth_exchange(modules, payload, events, redactor):
    oauth = modules[5]
    callback_url = require_text(payload.get("callback_url"), "payload.callback_url")
    binding = require_mapping(payload.get("binding"), "payload.binding")
    with capture_output(events, redactor):
        result = oauth.exchange_callback(
            callback_url,
            require_text(binding.get("state_digest"), "binding.state_digest"),
            require_text(binding.get("marker_digest"), "binding.marker_digest"),
        )
    return {"session": {"token": result.token, "crypto_document": result.crypto_document}}


def execute_task(modules, payload, entry, events, redactor):
    _, request_header, _, _, decoder, _, runner_module = modules
    restore(request_header, decoder, payload.get("session"))
    task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native")
    family = native.get("task_family")

    def progress(done, total, message):
        events.emit("progress", current=done, total=total if total > 0 else None,
                    message=redactor.text(message))

    def log(message):
        events.emit("log", level="info", message=redactor.text(message))

    worker = runner_module.HeadlessTaskRunner(entry.resolve().parents[1], progress=progress, log=log)
    worker.public.course_id = native.get("course_id")
    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    answer_route = str(settings.get("answer_route", "untimed")).strip().lower()
    if answer_route not in {"timed", "untimed", "escalation"}:
        raise WorkerFailure("request_invalid", "Cidaren answer_route must be timed, untimed, or escalation")
    try:
        instant_timeout = int(settings.get("instant_timeout_seconds", 8))
        fallback_grace = int(settings.get("instant_fallback_grace_seconds", 2))
    except (TypeError, ValueError) as error:
        raise WorkerFailure("request_invalid", "Cidaren instant budgets must be integers") from error
    if answer_route == "timed" and (instant_timeout < 1 or instant_timeout > 300 or fallback_grace < 0 or fallback_grace > 120):
        raise WorkerFailure("request_invalid", "Cidaren timed answer budgets are outside the supported bounds")
    bridge = _answer_bridge_settings(settings)
    total_answer_budget = max(1, instant_timeout + fallback_grace)
    bridge_timeout = min(
        10,
        max(
            1,
            min(
                instant_timeout,
                (total_answer_budget * 65) // 100 if answer_route == "timed" else instant_timeout,
            ),
        ),
    )
    supplied_answers = payload.get("answers")
    by_remote_id = {
        str(row.get("remote_id")): row.get("value")
        for row in supplied_answers
        if isinstance(row, Mapping) and row.get("remote_id") is not None
    } if isinstance(supplied_answers, list) else {}
    selected_sources = {}
    if (by_remote_id or bridge is not None) and hasattr(worker, "set_answer_override"):
        # Keep the donor's option/tag protocol at the boundary. Static
        # Asterism answers win; only questions without one call the short-lived
        # loopback bridge. The donor remains responsible for fallback and
        # submission semantics.

        def answer_override(public, _mode):
            exam = public.exam if isinstance(public.exam, Mapping) else {}
            topic_code = str(exam.get("topic_code") or "")
            if topic_code in by_remote_id:
                resolved = _resolve_donor_answer(exam, by_remote_id[topic_code])
                if resolved is not None:
                    selected_sources[topic_code] = "supplied"
                return resolved
            if bridge is None:
                return None
            request = {
                "protocol": ANSWER_BRIDGE_PROTOCOL,
                "kind": "resolve_answer",
                "execution_id": bridge["execution_id"],
                "task_id": bridge["task_id"],
                "remote_task_id": bridge["remote_task_id"],
                "route": answer_route,
                "timeout_seconds": bridge_timeout,
                "remote_id": topic_code,
                "mode": _mode,
                "exam": _json_public(exam),
            }
            started_at = time.monotonic()
            try:
                response = _bridge_post(bridge, request, bridge_timeout)
            except (OSError, ValueError, json.JSONDecodeError, urllib.error.URLError):
                response = None
            if response is None and answer_route == "timed" and fallback_grace > 0:
                remaining = max(1, int(total_answer_budget - (time.monotonic() - started_at)))
                if remaining > 0:
                    retry_request = dict(request)
                    retry_request["route"] = "escalation"
                    retry_request["timeout_seconds"] = min(10, remaining)
                    try:
                        response = _bridge_post(bridge, retry_request, retry_request["timeout_seconds"])
                    except (OSError, ValueError, json.JSONDecodeError, urllib.error.URLError):
                        response = None
            if not response or response.get("answer_available") is False:
                return None
            raw_value = response.get("donor_value") if "donor_value" in response else response.get("value", response.get("answer"))
            # Unknown donor modes are allowed to reach the resolver with their
            # complete public exam, but only an explicit donor-native value is
            # safe to return. Otherwise preserve the donor answer_lib/skip path.
            known_modes = {11, 13, 15, 16, 17, 18, 21, 22, 31, 32, 41, 42, 43, 44, 51, 52, 53, 54, 73}
            if _mode not in known_modes and "donor_value" not in response:
                return None
            resolved = _resolve_donor_answer(exam, raw_value)
            if resolved is not None:
                selected_sources[topic_code] = "bridge"
            return resolved

        worker.set_answer_override(answer_override)
    try:
        spend_min = int(settings.get("spend_min_time", getattr(worker.public, "spend_min_time", 1)))
        spend_max = int(settings.get("spend_max_time", getattr(worker.public, "spend_max_time", 2)))
    except (TypeError, ValueError) as error:
        raise WorkerFailure("request_invalid", "Cidaren delay settings must be integers") from error
    if spend_min < 0 or spend_max < spend_min or spend_max > 3600:
        raise WorkerFailure("request_invalid", "Cidaren delay settings must satisfy 0 <= min <= max <= 3600")
    if hasattr(worker.public, "set_spend_time"):
        worker.public.set_spend_time(spend_min, spend_max)
    else:
        worker.public.spend_min_time = spend_min
        worker.public.spend_max_time = spend_max
    original_submit = getattr(runner_module, "submit", None)
    if bridge is not None and callable(original_submit):
        def observed_submit(public, option):
            exam = public.exam if isinstance(public.exam, Mapping) else {}
            topic_code = str(exam.get("topic_code") or "")
            right_before = int(getattr(public, "right_count", 0) or 0)
            wrong_before = int(getattr(public, "wrong_count", 0) or 0)
            result_value = original_submit(public, option)
            right_delta = int(getattr(public, "right_count", 0) or 0) - right_before
            wrong_delta = int(getattr(public, "wrong_count", 0) or 0) - wrong_before
            correctness = (
                "correct" if right_delta > 0 and wrong_delta == 0 else
                "wrong" if wrong_delta > 0 and right_delta == 0 else
                "mixed" if right_delta > 0 and wrong_delta > 0 else
                "unknown"
            )
            observation = {
                "protocol": ANSWER_BRIDGE_PROTOCOL,
                "kind": "answer_observation",
                "execution_id": bridge["execution_id"],
                "task_id": bridge["task_id"],
                "remote_task_id": bridge["remote_task_id"],
                "remote_id": topic_code,
                "mode": exam.get("topic_mode"),
                "submitted": _json_public(option),
                "source": selected_sources.pop(topic_code, "donor"),
                "outcome": {
                    "correctness": correctness,
                    "right_delta": max(0, right_delta),
                    "wrong_delta": max(0, wrong_delta),
                },
            }
            try:
                _bridge_post(bridge, observation, bridge_timeout)
            except (OSError, ValueError, json.JSONDecodeError, urllib.error.URLError):
                pass
            return result_value

        runner_module.submit = observed_submit
    try:
        with capture_output(events, redactor):
            if family == "unit":
                result = worker.run_unit(require_mapping(native.get("unit"), "task.native.unit"))
            elif family == "class":
                result = worker.run_class_task(require_mapping(native.get("task"), "task.native.task"))
            else:
                raise WorkerFailure("task_unsupported", f"unsupported Cidaren task family: {family}")
    finally:
        if bridge is not None and callable(original_submit):
            runner_module.submit = original_submit
    complete = result.get("complete") is True
    return {
        "remote_state": "completed" if complete else "in_progress",
        "verified": complete,
        "answer_policy": {
            "route": answer_route,
            "instant_timeout_seconds": instant_timeout,
            "instant_fallback_grace_seconds": fallback_grace,
            "model_budget_seconds": bridge_timeout,
            "fallback_decision_seconds": max(bridge_timeout, (total_answer_budget * 85) // 100),
            "submission_reserve_seconds": max(1, total_answer_budget - ((total_answer_budget * 85) // 100)),
        },
        "result": result,
        "session": payload.get("session"),
    }


def dispatch(operation, payload, entry, metadata, events, redactor):
    modules = load(entry, events, redactor)
    if operation == "health": return {"status": "ok", "source": metadata.__dict__, "python": sys.version.split()[0], "operations": ["health", "oauth_begin", "oauth_exchange", "authenticate", "courses", "tasks", "questions", "run"]}
    if operation == "oauth_begin": return oauth_begin(modules)
    if operation == "oauth_exchange": return oauth_exchange(modules, payload, events, redactor)
    if operation == "authenticate": return authenticate(modules, payload, events, redactor)
    if operation == "courses": return courses(modules, payload, events, redactor)
    if operation == "tasks": return tasks(modules, payload, events, redactor)
    if operation == "questions": return questions(modules, payload, events, redactor)
    if operation == "run": return execute_task(modules, payload, entry, events, redactor)
    raise WorkerFailure("operation_unsupported", operation)


if __name__ == "__main__": raise SystemExit(run(PROTOCOL, dispatch))
