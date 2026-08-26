"""Emit a credential-free deployment acceptance snapshot.

The script intentionally reads only aggregate account/course/task/harvest facts
and the public daemon health endpoint. It never selects titles, native payloads,
credentials, cookies, tokens or answer content.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sqlite3
import urllib.request
from pathlib import Path


def health(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=5) as response:  # noqa: S310 - operator-supplied local URL
        value = json.load(response)
    allowed = {
        "service", "version", "status", "database", "schema_version",
        "registered_providers", "outbox_pending", "outbox_dead_letter",
        "secret_store_configured", "master_initialized",
    }
    return {key: value[key] for key in allowed if key in value}


def aggregates(database_path: Path) -> list[dict]:
    database = sqlite3.connect(f"file:{database_path.as_posix()}?mode=ro", uri=True)
    rows = database.execute(
        "SELECT accounts.provider_id, "
        "json_extract(accounts.auth_state_json, '$.state') AS auth_state, "
        "(SELECT COUNT(*) FROM courses WHERE provider_account_id = accounts.id), "
        "(SELECT COUNT(*) FROM tasks WHERE provider_account_id = accounts.id), "
        "(SELECT COUNT(*) FROM tasks WHERE provider_account_id = accounts.id AND remote_state = 'completed'), "
        "(SELECT MAX(last_seen_at) FROM courses WHERE provider_account_id = accounts.id), "
        "(SELECT MAX(updated_at) FROM tasks WHERE provider_account_id = accounts.id) "
        "FROM provider_accounts AS accounts ORDER BY accounts.provider_id"
    ).fetchall()
    harvest = {
        provider_id: {"state": state, "scanned_task_count": scanned}
        for provider_id, state, scanned in database.execute(
            "SELECT provider_id, state, scanned_task_count FROM answer_bootstrap_harvests "
            "WHERE provider_id = 'chaoxing'"
        )
    }
    database.close()
    return [
        {
            "provider": provider_id,
            "auth_state": auth_state,
            "course_count": course_count,
            "task_count": task_count,
            "completed_task_count": completed_count or 0,
            "last_course_scan_at": last_course_scan_at,
            "last_task_scan_at": last_task_scan_at,
            "answer_harvest": harvest.get(provider_id),
        }
        for (
            provider_id,
            auth_state,
            course_count,
            task_count,
            completed_count,
            last_course_scan_at,
            last_task_scan_at,
        ) in rows
    ]


def deployment_readiness(database_path: Path) -> dict:
    database = sqlite3.connect(f"file:{database_path.as_posix()}?mode=ro", uri=True)
    ai_configured = database.execute(
        "SELECT EXISTS(SELECT 1 FROM deployment_ai_config)"
    ).fetchone()[0] == 1
    pricing_configured = database.execute(
        "SELECT EXISTS(SELECT 1 FROM pricing_catalog_revisions)"
    ).fetchone()[0] == 1
    now = dt.datetime.now(dt.timezone.utc).isoformat()
    gateway_rows = database.execute(
        "SELECT scopes_json FROM service_tokens "
        "WHERE revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?)",
        (now,),
    ).fetchall()
    gateway_ready = False
    for (scopes_json,) in gateway_rows:
        try:
            scopes = set(json.loads(scopes_json))
        except (TypeError, json.JSONDecodeError):
            continue
        if {
            "provider_read",
            "provider_manage",
            "task_read",
            "task_execute",
            "qq_identity_assert",
            "task_command_proxy",
            "notification_delivery_report",
        } <= scopes:
            gateway_ready = True
            break
    database.close()
    return {
        "ai_configured": ai_configured,
        "pricing_catalog_configured": pricing_configured,
        "qq_gateway_service_token_configured": gateway_ready,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--database", type=Path, default=Path("asterism.db"))
    parser.add_argument("--health-url", default="http://127.0.0.1:8068/health")
    args = parser.parse_args()
    result = {
        "health": health(args.health_url),
        "providers": aggregates(args.database),
        "deployment_readiness": deployment_readiness(args.database),
    }
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
