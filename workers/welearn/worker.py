#!/usr/bin/env python3
"""Read-only WELearn adapter retaining the pinned donors' session workflow."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import re
import sys
from typing import Any, Mapping

sys.path.insert(0, str(pathlib.Path(__file__).parents[1]))
from common.runtime import (Events, Redactor, SourceMetadata, WorkerFailure,
                            capture_output, require_mapping, require_text, run)

PROTOCOL = "asterism.welearn.worker.v1"


def load(entry, events, redactor):
    spec = importlib.util.spec_from_file_location("asterism_welearn_upstream", entry)
    module = importlib.util.module_from_spec(spec)
    try:
        with capture_output(events, redactor): spec.loader.exec_module(module)
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", error.name or str(error)) from error
    return module


def cookie_dict(module): return module.session.cookies.get_dict()


def retry_read(call):
    last = None
    for _ in range(3):
        try:
            return call()
        except Exception as error:
            last = error
    raise last


def restore(module, value):
    value = require_mapping(value, "payload.session")
    jar = value.get("cookies")
    if not isinstance(jar, Mapping): raise WorkerFailure("request_invalid", "session.cookies must be an object")
    module.session.cookies.update({str(k): str(v) for k, v in jar.items()})


def authenticate(module, payload, events, redactor):
    credential = require_mapping(payload.get("credentials"), "payload.credentials")
    cookie = credential.get("cookie")
    with capture_output(events, redactor):
        if isinstance(cookie, str) and cookie.strip():
            module.session.cookies.update(dict(x.strip().split("=", 1) for x in cookie.split(";") if "=" in x))
        else:
            result = module.sso_login(require_text(credential.get("username"), "credentials.username"),
                                      require_text(credential.get("password"), "credentials.password"))
            if not result: raise WorkerFailure("authentication_failed", "upstream SSO login was rejected")
            module.session.cookies.update(result)
        response = retry_read(lambda: module.session.get(
            "https://welearn.sflep.com/ajax/authCourse.aspx?action=gmc",
            headers={"Referer": "https://welearn.sflep.com/student/index.aspx"}))
        data = response.json()
    if not isinstance(data.get("clist"), list): raise WorkerFailure("authentication_failed", "course probe was rejected")
    return {"session": {"cookies": cookie_dict(module)}, "account": {"course_count": len(data["clist"])}}


def courses(module, payload, events, redactor):
    restore(module, payload.get("session"))
    with capture_output(events, redactor):
        rows = retry_read(lambda: module.session.get(
            "https://welearn.sflep.com/ajax/authCourse.aspx?action=gmc",
            headers={"Referer": "https://welearn.sflep.com/student/index.aspx"})).json().get("clist", [])
    return {"courses": [{"remote_id": str(x["cid"]), "title": str(x.get("name", "")),
                           "remote_status": str(x.get("per", "")), "native": x} for x in rows],
            "session": {"cookies": cookie_dict(module)}}


def course_context(module, course):
    cid = str(course["cid"])
    url = f"https://welearn.sflep.com/student/course_info.aspx?cid={cid}"
    text = retry_read(lambda: module.session.get(
        url,
        headers={"Referer": "https://welearn.sflep.com/student/index.aspx"},
        timeout=15,
    )).text
    uid_match, class_match = re.search(r'"uid":(.*?),', text), re.search(r'"classid":"(.*?)"', text)
    if not uid_match or not class_match: raise WorkerFailure("course_shape_mismatch", "uid/classid were absent")
    return cid, uid_match.group(1), class_match.group(1), url


def tasks(module, payload, events, redactor):
    restore(module, payload.get("session")); course = require_mapping(payload.get("course"), "payload.course")
    with capture_output(events, redactor):
        cid, uid, classid, course_url = course_context(module, course)
        units = retry_read(lambda: module.session.post(
            "https://welearn.sflep.com/ajax/StudyStat.aspx",
            data={"action": "courseunits", "cid": cid, "uid": uid},
            headers={"Referer": course_url}, timeout=15,
        )).json().get("info", [])
        result = []
        for unit_index, unit in enumerate(units):
            items = retry_read(lambda: module.session.get(
                f"https://welearn.sflep.com/ajax/StudyStat.aspx?action=scoLeaves&cid={cid}&uid={uid}&unitidx={unit_index}&classid={classid}",
                headers={"Referer": course_url}, timeout=15,
            )).json().get("info", [])
            for item in items:
                if isinstance(item, Mapping):
                    result.append({"remote_id": str(item.get("id")), "title": str(item.get("location") or item.get("name") or item.get("id")),
                                   "state": "completed" if str(item.get("iscomplete", "")).lower() in ("true", "已完成") else "pending",
                                   "source_type": "resource",
                                   "capabilities": ["run", "duration"], "native": {"course": dict(course), "unit": unit,
                                   "unit_index": unit_index, "item": item, "uid": uid, "cid": cid, "classid": classid}})
    return {"tasks": result, "session": {"cookies": cookie_dict(module)}}


def infer_kind(interaction):
    native = str(interaction.get("type") or interaction.get("interaction_type") or "").lower()
    return {"choice": "single_choice", "multiple_choice": "multiple_choice", "true-false": "true_false",
            "fill-in": "fill_blank", "long-fill-in": "short_answer", "matching": "matching",
            "sequencing": "ordering"}.get(native, "provider_native")


def native_shape(value, depth=0):
    if depth >= 4:
        return type(value).__name__
    if isinstance(value, Mapping):
        return {str(key): native_shape(child, depth + 1) for key, child in value.items()}
    if isinstance(value, list):
        return {"type": "list", "length": len(value),
                "item": native_shape(value[0], depth + 1) if value else None}
    return type(value).__name__


def questions(module, payload, events, redactor):
    restore(module, payload.get("session")); task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native"); item = require_mapping(native.get("item"), "task.native.item")
    common = {"uid": native.get("uid"), "cid": native.get("cid"), "scoid": item.get("id")}
    with capture_output(events, redactor):
        response = module.session.post(f"https://welearn.sflep.com/Ajax/SCO.aspx?uid={native.get('uid')}",
            data={**common, "action": "getscoinfo_v7"}, headers={"Referer": "https://welearn.sflep.com/student/StudyCourse.aspx"})
        value = response.json().get("comment", "{}")
    try: parsed = json.loads(value) if isinstance(value, str) else {}
    except json.JSONDecodeError as error: raise WorkerFailure("question_shape_mismatch", str(error)) from error
    cmi = parsed.get("cmi", {}) if isinstance(parsed, Mapping) else {}
    interactions = cmi.get("interactions", [])
    questions = []
    for index, interaction in enumerate(interactions if isinstance(interactions, list) else []):
        if not isinstance(interaction, Mapping): continue
        evidence = interaction.get("learner_response") or interaction.get("result")
        questions.append({"remote_id": f'{index + 1}:{interaction.get("id", index + 1)}', "position": index + 1,
                          "kind": infer_kind(interaction), "prompt": str(interaction.get("description") or interaction.get("id", "")),
                          "options": interaction.get("choices", []), "answer_evidence": evidence, "native": interaction})
    return {"questions": questions, "scan_source": "donor getscoinfo_v7 CMI interactions",
            "provider_native_content": native_shape(parsed) if not questions else None,
            "session": {"cookies": cookie_dict(module)}}


def fresh_item_status(module, native, item):
    """Re-read the donor's authoritative SCO leaf after a mutation."""
    cid = str(native.get("cid", ""))
    uid = str(native.get("uid", ""))
    classid = str(native.get("classid", ""))
    unit_index = int(native.get("unit_index", 0))
    course_url = f"https://welearn.sflep.com/student/course_info.aspx?cid={cid}"
    rows = module.session.get(
        f"https://welearn.sflep.com/ajax/StudyStat.aspx?action=scoLeaves&cid={cid}"
        f"&uid={uid}&unitidx={unit_index}&classid={classid}",
        headers={"Referer": course_url}, timeout=15,
    ).json().get("info", [])
    target_id = str(item.get("id", ""))
    fresh = next((row for row in rows if isinstance(row, Mapping)
                  and str(row.get("id", "")) == target_id), None)
    if fresh is None:
        raise WorkerFailure("verification_unavailable", "fresh WELearn SCO was absent")
    return str(fresh.get("iscomplete", "")).lower() in ("true", "已完成")


def run_task(module, payload, events, redactor):
    """Run one SCO through the donor's original completion or duration function."""
    restore(module, payload.get("session"))
    task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native")
    item = dict(require_mapping(native.get("item"), "task.native.item"))
    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    action = str(settings.get("action") or "complete")
    module.uid = str(native.get("uid", ""))
    module.cid = str(native.get("cid", ""))
    module.classid = str(native.get("classid", ""))
    for name in ("way1Succeed", "way2Succeed", "way1Failed", "way2Failed"):
        setattr(module, name, [])

    events.emit("progress", current=0, total=1)
    with capture_output(events, redactor):
        if action == "complete":
            correctness = int(settings.get("correctness", 100))
            if not 0 <= correctness <= 100:
                raise WorkerFailure("request_invalid", "correctness must be between 0 and 100")
            module.startstudy(correctness, item)
            accepted = bool(module.way1Succeed or module.way2Succeed)
        elif action == "duration":
            seconds = int(settings.get("duration_seconds", 0))
            if not 0 < seconds <= 24 * 60 * 60:
                raise WorkerFailure("request_invalid", "duration_seconds must be between 1 and 86400")
            statuses = [{"status": "待开始", "elapsed": 0}]
            module.startstudy_time(0, statuses, seconds, item)
            accepted = statuses[0]["status"] == "已完成"
        else:
            raise WorkerFailure("request_invalid", "settings.action must be complete or duration")
    if not accepted:
        raise WorkerFailure("execution_failed", f"upstream {action} operation was rejected")
    completed = str(item.get("iscomplete", "")).lower() in ("true", "已完成")
    verification_error = None
    try:
        with capture_output(events, redactor):
            completed = fresh_item_status(module, native, item)
    except Exception:
        # The donor mutation may already have succeeded. Do not replay it just
        # because the post-write inventory endpoint was temporarily unavailable.
        verification_error = "fresh_status_unavailable"
    events.emit("progress", current=1, total=1)
    return {
        "remote_state": "completed" if completed else "in_progress",
        "verified": completed if action == "complete" else False,
        "result": {"action": action, "upstream_accepted": True,
                   "fresh_completion_observed": completed,
                   "verification_error": verification_error},
        "session": {"cookies": cookie_dict(module)},
    }


def dispatch(operation, payload, entry, metadata, events, redactor):
    module = load(entry, events, redactor)
    if operation == "health": return {"status": "ok", "source": metadata.__dict__, "python": sys.version.split()[0], "operations": ["health", "authenticate", "courses", "tasks", "questions", "run"]}
    if operation == "authenticate": return authenticate(module, payload, events, redactor)
    if operation == "courses": return courses(module, payload, events, redactor)
    if operation == "tasks": return tasks(module, payload, events, redactor)
    if operation == "questions": return questions(module, payload, events, redactor)
    if operation == "run": return run_task(module, payload, events, redactor)
    raise WorkerFailure("operation_unsupported", operation)


if __name__ == "__main__": raise SystemExit(run(PROTOCOL, dispatch))
