#!/usr/bin/env python3
"""Credential-safe live account probe for the four 0.0.1 workers.

Credentials are read only from environment variables and are never written to
the report.  The default report contains only counts and observed
normalized/native kinds.  An explicit diagnostic flag may add course titles,
teachers and per-course status, but never prompts, answers, sessions or
Provider-private payloads.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import uuid
from collections import Counter
from typing import Any


PROTOCOLS = {
    "chaoxing": "asterism.chaoxing.worker.v1",
    "welearn": "asterism.welearn.worker.v1",
    "uai": "asterism.uai.worker.v1",
    "cidaren": "asterism.cidaren.worker.v1",
}


def invoke(args, operation: str, payload: dict[str, Any]) -> dict[str, Any]:
    request_id = str(uuid.uuid4())
    completed = subprocess.run(
        [args.python, str(args.adapter), "--upstream", str(args.upstream),
         "--source-metadata", str(args.source_metadata)],
        input=json.dumps({"request_id": request_id, "operation": operation, "payload": payload}) + "\n",
        text=True, capture_output=True, timeout=args.timeout, check=False,
    )
    events = []
    for line in completed.stdout.splitlines():
        event = json.loads(line)
        if event.get("protocol") != PROTOCOLS[args.provider] or event.get("request_id") != request_id:
            raise RuntimeError("worker returned an unbound event")
        events.append(event)
    error = next((event for event in events if event.get("type") == "error"), None)
    if error:
        raise RuntimeError(f'{operation} failed: {error.get("code")} ({error.get("message")})')
    result = next((event.get("data") for event in events if event.get("type") == "result"), None)
    if result is None or completed.returncode != 0:
        raise RuntimeError(f"{operation} failed without a terminal result")
    return result


def credentials(provider: str) -> dict[str, str]:
    if provider == "cidaren":
        token = os.environ.get("ASTERISM_WORKER_TOKEN", "")
        if not token: raise RuntimeError("ASTERISM_WORKER_TOKEN is required")
        return {"token": token}
    cookie = os.environ.get("ASTERISM_WORKER_COOKIE", "")
    if cookie and provider in ("chaoxing", "welearn"):
        return {"cookie": cookie}
    username, password = os.environ.get("ASTERISM_WORKER_USERNAME", ""), os.environ.get("ASTERISM_WORKER_PASSWORD", "")
    if not username or not password: raise RuntimeError("ASTERISM_WORKER_USERNAME and ASTERISM_WORKER_PASSWORD are required")
    return {"username": username, "password": password}


def limited(rows, maximum): return rows if maximum == 0 else rows[:maximum]


def task_window(rows, offset, maximum):
    rows = rows[offset:]
    return rows if maximum == 0 else rows[:maximum]


def write_course_checkpoint(path, provider, total, statuses):
    if path is None:
        return
    path.write_text(json.dumps({
        "provider": provider,
        "course_count": total,
        "completed_count": sum(status.get("state") in ("complete", "failed") for status in statuses),
        "course_status": statuses,
    }, ensure_ascii=False, indent=2), encoding="utf-8")


def main(argv=None) -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="backslashreplace")
        sys.stderr.reconfigure(encoding="utf-8", errors="backslashreplace")
    parser = argparse.ArgumentParser()
    parser.add_argument("provider", choices=PROTOCOLS)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--adapter", required=True, type=pathlib.Path)
    parser.add_argument("--upstream", required=True, type=pathlib.Path)
    parser.add_argument("--source-metadata", required=True, type=pathlib.Path)
    parser.add_argument("--timeout", default=60, type=int)
    parser.add_argument("--max-courses", default=0, type=int)
    parser.add_argument("--course-offset", default=0, type=int)
    parser.add_argument("--max-tasks-per-course", default=0, type=int)
    parser.add_argument("--task-offset-per-course", default=0, type=int)
    parser.add_argument("--max-question-tasks-per-course", default=0, type=int)
    parser.add_argument("--skip-questions", action="store_true")
    parser.add_argument(
        "--diagnostic-questions",
        action="store_true",
        help="inspect provider-private question/content shapes even when the task does not advertise a product questions capability",
    )
    parser.add_argument("--include-course-status", action="store_true")
    parser.add_argument("--course-status-path", type=pathlib.Path)
    parser.add_argument("--allow-read-that-starts-attempt", action="store_true")
    args = parser.parse_args(argv)

    health = invoke(args, "health", {})
    authenticated = invoke(args, "authenticate", {"credentials": credentials(args.provider)})
    session = authenticated["session"]
    course_result = invoke(args, "courses", {"session": session})
    session = course_result.get("session", session)
    all_courses = course_result.get("courses", [])
    courses = limited(all_courses[args.course_offset:], args.max_courses)
    task_count = question_count = native_inventory_count = 0
    kinds: Counter[str] = Counter()
    native_shapes: Counter[str] = Counter()
    native_inventory_shapes: Counter[str] = Counter()
    task_native_types: Counter[str] = Counter()
    failures: Counter[str] = Counter()
    course_status = []
    for position, course in enumerate(courses, args.course_offset + 1):
        status = {
            "position": position,
            "title": str(course.get("title") or ""),
            "teacher": course.get("teacher"),
            "state": "scanning",
            "task_count": 0,
            "question_tasks_scanned": 0,
            "question_count": 0,
            "question_kinds": {},
        }
        course_status.append(status)
        write_course_checkpoint(args.course_status_path, args.provider,
                                len(all_courses), course_status)
        if args.include_course_status:
            print(
                f"[{position}/{len(all_courses)}] {status['title']}",
                file=sys.stderr,
                flush=True,
            )
        try:
            task_result = invoke(args, "tasks", {"session": session, "course": course.get("native", {})})
            session = task_result.get("session", session)
        except RuntimeError as error:
            failure = str(error).split("(", 1)[0].strip()
            failures[failure] += 1
            status["state"] = "failed"
            status["error"] = failure
            write_course_checkpoint(args.course_status_path, args.provider,
                                    len(all_courses), course_status)
            continue
        tasks = task_window(task_result.get("tasks", []), args.task_offset_per_course,
                            args.max_tasks_per_course)
        task_count += len(tasks)
        for task in tasks:
            native = task.get("native", {})
            if isinstance(native, dict):
                native_type = native.get("base") or native.get("task_family") or native.get("type")
                if not native_type and isinstance(native.get("job"), dict):
                    native_type = native["job"].get("type")
                if native_type:
                    task_native_types[str(native_type)] += 1
        status["state"] = "attachments_scanned"
        status["task_count"] = len(tasks)
        if isinstance(task_result.get("scan_diagnostics"), dict):
            status["scan_diagnostics"] = task_result["scan_diagnostics"]
        write_course_checkpoint(args.course_status_path, args.provider,
                                len(all_courses), course_status)
        if args.skip_questions:
            status["state"] = "complete"
            write_course_checkpoint(args.course_status_path, args.provider,
                                    len(all_courses), course_status)
            continue
        status["state"] = "questions_scanning"
        question_tasks = 0
        course_question_kinds: Counter[str] = Counter()
        for task in tasks:
            if ("questions" not in task.get("capabilities", [])
                    and not args.diagnostic_questions):
                continue
            if (args.max_question_tasks_per_course
                    and question_tasks >= args.max_question_tasks_per_course):
                continue
            question_tasks += 1
            if args.provider == "cidaren" and task.get("native", {}).get("task_family") == "class" and not args.allow_read_that_starts_attempt:
                failures["cidaren class question scan skipped: may start attempt"] += 1
                continue
            try:
                scanned = invoke(args, "questions", {
                    "session": session,
                    "task": task,
                    "allow_read_that_starts_attempt": args.allow_read_that_starts_attempt,
                })
                session = scanned.get("session", session)
                rows = scanned.get("questions", [])
                question_count += len(rows)
                observed_kinds = [str(row.get("kind", "provider_native")) for row in rows]
                kinds.update(observed_kinds)
                course_question_kinds.update(observed_kinds)
                for row in rows:
                    shape = row.get("native_shape")
                    if isinstance(shape, dict):
                        signature = "|".join((
                            str(shape.get("task_base") or ""),
                            str(shape.get("native_type") or ""),
                            ",".join(str(key) for key in shape.get("keys", [])[:64]),
                        ))
                        native_shapes[signature] += 1
                private_inventory = scanned.get("provider_native_inventory") or scanned.get("provider_native_content")
                native_inventory_count += int(bool(private_inventory))
                if isinstance(private_inventory, dict):
                    native_inventory_shapes[json.dumps(private_inventory, sort_keys=True, ensure_ascii=False)] += 1
            except RuntimeError as error:
                failures[str(error).split("(", 1)[0].strip()] += 1
            status["question_tasks_scanned"] = question_tasks
            status["question_count"] = sum(course_question_kinds.values())
            status["question_kinds"] = dict(sorted(course_question_kinds.items()))
            write_course_checkpoint(args.course_status_path, args.provider,
                                    len(all_courses), course_status)
        status["state"] = "complete"
        write_course_checkpoint(args.course_status_path, args.provider,
                                len(all_courses), course_status)
    report = {"provider": args.provider, "revision": health["source"]["revision"],
              "account_authenticated": True, "course_count": len(all_courses),
              "scanned_course_count": len(courses), "task_count": task_count,
              "question_count": question_count, "question_kinds": dict(sorted(kinds.items())),
              "task_native_types": dict(sorted(task_native_types.items())),
              "provider_native_shapes": dict(sorted(native_shapes.items())),
              "provider_native_inventory_shapes": dict(sorted(native_inventory_shapes.items())),
              "provider_native_inventory_count": native_inventory_count, "failures": dict(sorted(failures.items()))}
    if args.include_course_status:
        report["course_status"] = course_status
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__": raise SystemExit(main())
