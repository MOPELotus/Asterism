#!/usr/bin/env python3
"""Thin token/session adapter around MOPELotus/Easy_Cidaren."""

from __future__ import annotations

import importlib
import pathlib
import sys
from types import SimpleNamespace
from typing import Any, Mapping

sys.path.insert(0, str(pathlib.Path(__file__).parents[1]))
from common.runtime import (Events, Redactor, SourceMetadata, WorkerFailure,
                            capture_output, require_mapping, require_text, run)

PROTOCOL = "asterism.cidaren.worker.v1"


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
                           "assessment_class": "routine",
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
    return {"questions": [{"remote_id": str(exam.get("topic_code") or exam.get("id") or "first"), "position": 1,
                            "kind": native_kind(exam), "prompt": str(prompt),
                            "options": exam.get("options") or exam.get("option") or [], "answer_evidence": None,
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
    with capture_output(events, redactor):
        if family == "unit":
            result = worker.run_unit(require_mapping(native.get("unit"), "task.native.unit"))
        elif family == "class":
            result = worker.run_class_task(require_mapping(native.get("task"), "task.native.task"))
        else:
            raise WorkerFailure("task_unsupported", f"unsupported Cidaren task family: {family}")
    complete = result.get("complete") is True
    return {
        "remote_state": "completed" if complete else "in_progress",
        "verified": complete,
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
