#!/usr/bin/env python3
"""Import Provider-reviewed answers for latest snapshots with a resumable safe checkpoint.

The checkpoint stores only task/snapshot identities, aggregate candidate counts and
sanitized error codes. It never stores question text, answers, credentials or sessions.
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


def latest_snapshots(database: str, account_id: str) -> list[tuple[str, str]]:
    connection = sqlite3.connect(database)
    rows = connection.execute(
        "SELECT snapshot.task_id, snapshot.id, task.remote_id, task.source_type, "
        "snapshot.captured_at, task.rowid FROM question_snapshots snapshot "
        "JOIN tasks task ON task.id = snapshot.task_id "
        "WHERE task.provider_account_id = ? AND snapshot.provider_id = 'chaoxing' "
        "AND EXISTS (SELECT 1 FROM question_snapshot_items item "
        "WHERE item.snapshot_id = snapshot.id AND "
        "json_extract(item.question_json, "
        "'$.metadata_sanitized.provider_private_shape.historical_answer_present') = 1) "
        "AND snapshot.id = (SELECT candidate.id FROM question_snapshots candidate "
        "WHERE candidate.task_id = snapshot.task_id "
        "ORDER BY candidate.captured_at DESC, candidate.id DESC LIMIT 1) "
        "ORDER BY task.rowid, snapshot.captured_at",
        (account_id,),
    ).fetchall()
    selected: dict[str, tuple[str, str, str, str, int]] = {}
    for task_id, snapshot_id, remote_id, source_type, captured_at, rowid in rows:
        key = str(remote_id)
        candidate = (str(task_id), str(snapshot_id), str(source_type), str(captured_at), int(rowid))
        prior = selected.get(key)
        if (prior is None
                or (prior[2] == "other" and candidate[2] != "other")
                or (prior[2] == candidate[2] and candidate[3] > prior[3])):
            selected[key] = candidate
    return [(row[0], row[1]) for row in sorted(selected.values(), key=lambda row: row[4])]


def load_checkpoint(path: pathlib.Path, account_id: str) -> dict[str, Any]:
    if path.exists() and path.stat().st_size:
        value = json.loads(path.read_text(encoding="utf-8"))
        if value.get("provider_account_id") != account_id:
            raise RuntimeError("checkpoint belongs to another Provider account")
        return value
    return {
        "provider": "chaoxing",
        "provider_account_id": account_id,
        "completed_snapshot_ids": [],
        "candidate_count": 0,
        "failures": {},
        "failed_snapshots": {},
    }


def save_checkpoint(path: pathlib.Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    temporary.replace(path)


def resolve(base_url: str, token: str, task_id: str, snapshot_id: str) -> int:
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/api/v1/tasks/{task_id}/question-snapshots/"
        f"{snapshot_id}/provider-answer-candidates",
        method="POST",
        headers={"Authorization": f"Bearer {token}", "x-request-id": str(uuid.uuid4())},
    )
    try:
        with urllib.request.urlopen(request, timeout=900) as response:
            value = json.load(response)
            return len(value.get("candidates", [])) if isinstance(value, dict) else 0
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
    snapshots = latest_snapshots(args.database, args.account_id)
    if args.limit:
        snapshots = snapshots[:args.limit]
    report = load_checkpoint(args.checkpoint, args.account_id)
    eligible_snapshot_ids = {snapshot_id for _task_id, snapshot_id in snapshots}
    # A later Question scan replaces each task's latest snapshot.  Old
    # checkpoint identities are no longer retryable and must not inflate the
    # completed/failure totals for the current canonical snapshot set.
    completed = set(report.get("completed_snapshot_ids", [])) & eligible_snapshot_ids
    failed_snapshots = {
        snapshot_id: value
        for snapshot_id, value in report.get("failed_snapshots", {}).items()
        if snapshot_id in eligible_snapshot_ids
    }
    report["failed_snapshots"] = failed_snapshots
    failures = Counter(
        str(value.get("code") or "")
        for value in failed_snapshots.values()
        if isinstance(value, dict) and value.get("code")
    )
    report["completed_snapshot_ids"] = sorted(completed)
    report["completed_snapshot_count"] = len(completed)
    report["failures"] = dict(sorted(failures.items()))
    report["eligible_snapshot_count"] = len(snapshots)
    for task_id, snapshot_id in snapshots:
        if snapshot_id in completed and not (args.retry_failures and snapshot_id in failed_snapshots):
            continue
        prior_failure = failed_snapshots.get(snapshot_id)
        if isinstance(prior_failure, dict):
            prior_code = str(prior_failure.get("code") or "")
            if failures.get(prior_code, 0) > 1:
                failures[prior_code] -= 1
            else:
                failures.pop(prior_code, None)
        try:
            report["candidate_count"] += resolve(args.base_url, token, task_id, snapshot_id)
            failed_snapshots.pop(snapshot_id, None)
        except Exception as error:
            code = str(error).splitlines()[0][:128]
            failures[code] += 1
            failed_snapshots[snapshot_id] = {"task_id": task_id, "code": code}
        completed.add(snapshot_id)
        report["completed_snapshot_ids"] = sorted(completed)
        report["completed_snapshot_count"] = len(completed)
        report["failures"] = dict(sorted(failures.items()))
        save_checkpoint(args.checkpoint, report)
    print(json.dumps({
        "eligible_snapshot_count": report["eligible_snapshot_count"],
        "completed_snapshot_count": report.get("completed_snapshot_count", 0),
        "candidate_count": report["candidate_count"],
        "failures": report["failures"],
        "failed_snapshot_count": len(report["failed_snapshots"]),
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
