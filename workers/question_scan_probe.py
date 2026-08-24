#!/usr/bin/env python3
"""Scan already-discovered task questions through Asterism without saving content.

The checkpoint contains task IDs, course titles, counts, kinds and sanitized
error codes only.  Prompts, options, answers, sessions and credentials are
never written.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sqlite3
import urllib.error
import urllib.request
import uuid
from collections import Counter
from typing import Any


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--account-id", required=True)
    parser.add_argument("--database", default="asterism.db")
    parser.add_argument("--base-url", default="http://127.0.0.1:8068")
    parser.add_argument("--checkpoint", type=pathlib.Path, required=True)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--retry-failures", action="store_true")
    return parser.parse_args()


def eligible_tasks(database: str, account_id: str) -> list[dict[str, str]]:
    connection = sqlite3.connect(database)
    rows = connection.execute(
        "SELECT t.id, c.title, t.remote_id, t.source_type, t.rowid FROM tasks t "
        "LEFT JOIN courses c ON c.id = t.course_id "
        "WHERE t.provider_account_id = ? "
        "AND instr(t.capabilities_json, 'question_inventory') > 0 "
        "ORDER BY c.rowid, t.rowid",
        (account_id,),
    ).fetchall()
    # A local database can contain one historical `other` row and one newer
    # typed row after a Provider source-type correction. Prefer the typed row
    # without deleting history or scanning the same remote task twice.
    selected: dict[str, dict[str, Any]] = {}
    for task_id, course, remote_id, source_type, rowid in rows:
        key = str(remote_id)
        candidate = {
            "task_id": str(task_id), "course": str(course or "Unscoped"),
            "source_type": str(source_type), "rowid": int(rowid),
        }
        prior = selected.get(key)
        if prior is None or (prior["source_type"] == "other" and source_type != "other"):
            selected[key] = candidate
    return [
        {"task_id": row["task_id"], "course": row["course"]}
        for row in sorted(selected.values(), key=lambda row: row["rowid"])
    ]


def load_checkpoint(path: pathlib.Path, account_id: str) -> dict[str, Any]:
    if path.exists() and path.stat().st_size:
        value = json.loads(path.read_text(encoding="utf-8"))
        if value.get("provider_account_id") != account_id:
            raise RuntimeError("checkpoint belongs to another Provider account")
        return value
    return {
        "provider": "chaoxing",
        "provider_account_id": account_id,
        "eligible_task_count": 0,
        "completed_task_ids": [],
        "question_count": 0,
        "question_kinds": {},
        "courses": {},
        "failures": {},
        "failed_tasks": {},
    }


def save_checkpoint(path: pathlib.Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    temporary.replace(path)


def read_questions(base_url: str, token: str, task_id: str) -> list[dict[str, Any]]:
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/api/v1/tasks/{task_id}/questions",
        headers={
            "Authorization": f"Bearer {token}",
            "x-request-id": str(uuid.uuid4()),
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=900) as response:
            if response.status == 204:
                return []
            value = json.load(response)
            return value.get("questions", []) if isinstance(value, dict) else []
    except urllib.error.HTTPError as error:
        try:
            value = json.loads(error.read().decode("utf-8", "replace"))
            detail = value.get("error", {}) if isinstance(value, dict) else {}
            code = str(detail.get("code") or f"http_{error.code}")
        except (TypeError, ValueError):
            code = f"http_{error.code}"
        raise RuntimeError(code) from error


def main() -> int:
    args = arguments()
    token = os.environ.get("ASTERISM_TOKEN", "")
    if not token:
        raise RuntimeError("ASTERISM_TOKEN is required")
    tasks = eligible_tasks(args.database, args.account_id)
    if args.limit:
        tasks = tasks[:args.limit]
    report = load_checkpoint(args.checkpoint, args.account_id)
    report["eligible_task_count"] = len(tasks)
    completed = set(report.get("completed_task_ids", []))
    kinds = Counter(report.get("question_kinds", {}))
    courses = report.setdefault("courses", {})
    failures = report.setdefault("failures", {})
    failed_tasks = report.setdefault("failed_tasks", {})

    for task in tasks:
        task_id, course = task["task_id"], task["course"]
        if task_id in completed and not (args.retry_failures and task_id in failed_tasks):
            continue
        course_report = courses.setdefault(course, {
            "scanned_tasks": 0, "question_count": 0, "question_kinds": {}, "failures": 0,
        })
        prior_failure = failed_tasks.get(task_id)
        if isinstance(prior_failure, dict):
            prior_code = str(prior_failure.get("code") or "")
            if failures.get(prior_code, 0) > 1:
                failures[prior_code] -= 1
            elif prior_code in failures:
                failures.pop(prior_code)
            course_report["failures"] = max(0, int(course_report.get("failures", 0)) - 1)
        try:
            questions = read_questions(args.base_url, token, task_id)
            observed = Counter(str(question.get("kind") or "unknown") for question in questions)
            kinds.update(observed)
            course_kinds = Counter(course_report.get("question_kinds", {}))
            course_kinds.update(observed)
            course_report["question_kinds"] = dict(sorted(course_kinds.items()))
            course_report["question_count"] += len(questions)
            report["question_count"] += len(questions)
            failed_tasks.pop(task_id, None)
        except Exception as error:  # checkpoint the sanitized terminal code and continue
            code = str(error).splitlines()[0][:128]
            failures[code] = failures.get(code, 0) + 1
            course_report["failures"] += 1
            failed_tasks[task_id] = {"code": code, "course": course}
        if prior_failure is None:
            course_report["scanned_tasks"] += 1
        completed.add(task_id)
        report["completed_task_ids"] = sorted(completed)
        report["completed_task_count"] = len(completed)
        report["question_kinds"] = dict(sorted(kinds.items()))
        save_checkpoint(args.checkpoint, report)

    print(json.dumps({
        "eligible_task_count": report["eligible_task_count"],
        "completed_task_count": report.get("completed_task_count", 0),
        "question_count": report["question_count"],
        "question_kinds": report["question_kinds"],
        "failures": report["failures"],
        "failed_task_count": len(report["failed_tasks"]),
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
