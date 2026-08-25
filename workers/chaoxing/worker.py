#!/usr/bin/env python3
"""Thin read-only adapter around the pinned Samueli924/chaoxing donor."""

from __future__ import annotations

import importlib
import hashlib
import json
import os
import pathlib
import re
import sys
import threading
from types import ModuleType
from typing import Any, Mapping

sys.path.insert(0, str(pathlib.Path(__file__).parents[1]))
from common.runtime import (Events, Redactor, SourceMetadata, WorkerFailure,
                            capture_output, require_mapping, require_text, run)

PROTOCOL = "asterism.chaoxing.worker.v1"


def auxiliary_root() -> pathlib.Path | None:
    configured = os.environ.get("ASTERISM_CHAOXING_AUXILIARY_UPSTREAM")
    if not configured:
        return None
    root = pathlib.Path(configured)
    if not root.is_absolute():
        root = pathlib.Path(__file__).resolve().parents[2] / root
    root = root.resolve()
    manifest_path = pathlib.Path(__file__).with_name("AUXILIARY_SOURCES.json")
    try:
        source = json.loads(manifest_path.read_text(encoding="utf-8"))[0]
        for relative, expected in source["files"].items():
            received = hashlib.sha256((root / relative).read_bytes()).hexdigest()
            if received != expected:
                raise WorkerFailure("upstream_integrity_mismatch", f"CxKitty {relative} hash changed")
    except WorkerFailure:
        raise
    except (OSError, KeyError, TypeError, ValueError) as error:
        raise WorkerFailure("auxiliary_upstream_invalid", str(error)) from error
    return root


def load_cxkitty(events: Events, redactor: Redactor):
    root = auxiliary_root()
    if root is None:
        return None

    logger_module = ModuleType("logger")

    class EventLogger:
        def __init__(self, name, *args, **kwargs): self.name = name
        def _emit(self, level, message): events.emit("log", level=level, message=redactor.text(f"CxKitty/{self.name}: {message}"))
        def debug(self, message): self._emit("debug", message)
        def info(self, message): self._emit("info", message)
        def warning(self, message): self._emit("warning", message)
        def error(self, message, exc_info=False): self._emit("error", message)

    logger_module.Logger = EventLogger
    logger_module.set_log_filename = lambda _phone: None
    sys.modules["logger"] = logger_module
    # Preserve CxKitty's own bounded OCR strategy.  The worker runtime image
    # carries ddddocr; if a deployment omits it, the donor import fails cleanly
    # and the scheduler applies the normal account-level backoff policy.
    try:
        import ddddocr as ddddocr_module
    except ImportError as error:
        raise WorkerFailure("dependency_missing", "ddddocr is required for Chaoxing CAPTCHA handling") from error
    sys.modules["ddddocr"] = ddddocr_module
    # Face upload is another explicitly user-mediated gate. Avoid importing
    # CxKitty's top-level persistence/config helper just to construct the DTO.
    utility_module = ModuleType("utils")
    utility_module.get_face_path_by_puid = lambda _puid: None
    sys.modules["utils"] = utility_module
    sys.path.insert(0, str(root))
    try:
        with capture_output(events, redactor):
            cxapi = importlib.import_module("cxapi")
        return cxapi
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", error.name or str(error)) from error


def cxkitty_for(payload, events, redactor):
    cxapi = load_cxkitty(events, redactor)
    if cxapi is None:
        return None
    session_value = require_mapping(payload.get("session"), "payload.session")
    jar = session_value.get("cookies")
    if not isinstance(jar, Mapping):
        raise WorkerFailure("request_invalid", "session.cookies must be an object")
    api = cxapi.ChaoXingAPI()
    api.session.ck_load({str(key): str(value) for key, value in jar.items()})
    with capture_output(events, redactor):
        if not api.accinfo():
            raise WorkerFailure("authentication_failed", "CxKitty rejected the restored Chaoxing session")
        classes = api.fetch_classes()
    return cxapi, api, classes


def cxkitty_class_index(classes, course: Mapping[str, Any]) -> int:
    course_id, class_id = str(course["courseId"]), str(course["clazzId"])
    for index, item in enumerate(classes.classes):
        if str(item.course_id) == course_id and str(item.class_id) == class_id:
            return index
    raise WorkerFailure("task_stale", "CxKitty could not rebind the requested Chaoxing course")


def load(entry: pathlib.Path, events: Events, redactor: Redactor):
    root = entry.resolve().parents[1]
    sys.path.insert(0, str(root))
    os.chdir(root)
    try:
        # The donor logger adds a rotating file sink at import time. Keep the
        # same loguru calls but redirect them into the worker event capture so
        # a read-only operation never writes plaintext logs beside the donor.
        from loguru import logger
        logger.remove()
        logger.add(lambda message: sys.stderr.write(str(message)))
        logger_module = ModuleType("api.logger")
        logger_module.logger = logger
        sys.modules["api.logger"] = logger_module
        with capture_output(events, redactor):
            return importlib.import_module("api.base")
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", error.name or str(error)) from error


def install_cookie_store(module, initial=None):
    memory = {str(key): str(value) for key, value in (initial or {}).items()}
    module.use_cookies = lambda: dict(memory)
    module.save_cookies = lambda session: (memory.clear(), memory.update(session.cookies.get_dict()))
    return memory


def cookies(session) -> dict[str, str]: return session.cookies.get_dict()


def restore(module, value: Any):
    value = require_mapping(value, "payload.session")
    jar = value.get("cookies")
    if not isinstance(jar, Mapping):
        raise WorkerFailure("request_invalid", "session.cookies must be an object")
    install_cookie_store(module, jar)
    session = module.SessionManager.get_session()
    return session


def authenticate(module, payload: Mapping[str, Any], events: Events, redactor: Redactor):
    credential = require_mapping(payload.get("credentials"), "payload.credentials")
    username = credential.get("username")
    password = credential.get("password")
    cookie = credential.get("cookie")
    account = None
    login_with_cookies = False
    if isinstance(cookie, str) and cookie.strip():
        parsed = dict(part.strip().split("=", 1) for part in cookie.split(";") if "=" in part)
        install_cookie_store(module, parsed)
        login_with_cookies = True
    else:
        install_cookie_store(module)
        account = module.Account(require_text(username, "credentials.username"), require_text(password, "credentials.password"))
    bot = module.Chaoxing(account)
    with capture_output(events, redactor):
        result = bot.login(login_with_cookies=login_with_cookies)
        display_name = bot.get_name()
    if not result.get("status"):
        raise WorkerFailure("authentication_failed", str(result.get("msg", "login rejected")))
    return {"session": {"cookies": cookies(module.SessionManager.get_session()), "display_name": display_name}}


def bot_for(module, payload):
    restore(module, payload.get("session"))
    return module.Chaoxing(None)


def clean_inventory_text(value: Any, maximum_bytes: int, fallback: str) -> str:
    normalized = " ".join(str(value or "").split())
    if not normalized:
        normalized = fallback
    encoded = normalized.encode("utf-8")
    if len(encoded) <= maximum_bytes:
        return normalized
    encoded = encoded[:maximum_bytes]
    while encoded:
        try:
            return encoded.decode("utf-8").rstrip() or fallback
        except UnicodeDecodeError as error:
            encoded = encoded[:error.start]
    return fallback


def homework_inventory_state(text: str, answer_id: str) -> str:
    # A non-zero answerId is Chaoxing's durable learner submission record.
    # Subjective work can keep waiting for teacher grading indefinitely, but it
    # is already complete from the learner's perspective.
    if (answer_id and answer_id != "0") or "已完成" in text:
        return "completed"
    if any(marker in text for marker in ("待批阅", "未批阅", "已交", "已提交")):
        return "completed"
    if any(marker in text for marker in ("已过期", "已截止", "已结束", "已关闭")):
        return "expired"
    return "pending"


def include_completed_cards(module):
    decode_module = sys.modules.get("api.decode")
    original = getattr(decode_module, "_process_attachment_cards", None)
    if original is None:
        raise WorkerFailure("dependency_shape_mismatch", "donor card processor was absent")

    diagnostics = {"card_responses": 0, "successful_card_responses": 0,
                   "marg_responses": 0, "attachment_cards": 0,
                   "passed_cards": 0, "null_job_cards": 0,
                   "processed_jobs": 0,
                   "card_types": {}}

    # Observe only response shape while retaining the donor's exact request
    # sequence.  This distinguishes "the endpoint returned no mArg" from
    # "the donor filtered every completed attachment" without persisting any
    # course text, question text, or answer material.
    session = module.SessionManager.get_session()
    original_get = session.get

    def get(url, *args, **kwargs):
        response = original_get(url, *args, **kwargs)
        if "/knowledge/cards" in str(url):
            diagnostics["card_responses"] += 1
            if response.status_code == 200:
                diagnostics["successful_card_responses"] += 1
            matches = re.findall(r"mArg=\{(.*?)\};", response.text.replace(" ", ""))
            if matches:
                diagnostics["marg_responses"] += 1
                try:
                    data = json.loads("{" + matches[0] + "}")
                    # The attachment processor below owns detailed counts.
                    # Keep this parser solely as a presence check so malformed
                    # or drifted payloads remain visible as a shape mismatch.
                    if not isinstance(data.get("attachments", []), list):
                        diagnostics["attachments_not_list"] = diagnostics.get("attachments_not_list", 0) + 1
                except (TypeError, ValueError):
                    diagnostics["marg_decode_failures"] = diagnostics.get("marg_decode_failures", 0) + 1
        return response

    session.get = get

    original_decode = module.decode_course_card

    def decode_with_private_defaults(html_text):
        jobs, info = original_decode(html_text)
        matches = re.findall(r"mArg=\{(.*?)\};", html_text.replace(" ", ""))
        if matches:
            try:
                defaults = json.loads("{" + matches[0] + "}").get("defaults", {})
                if isinstance(defaults, Mapping):
                    for key in ("utenc",):
                        if defaults.get(key):
                            info[key] = defaults[key]
            except (TypeError, ValueError):
                pass
        # Chaoxing renders the exact reviewed-work URL into each card iframe.
        # Preserve that server-generated route instead of reconstructing its
        # dynamic enc/utenc/ktoken combination in the adapter.
        try:
            from bs4 import BeautifulSoup
            soup = BeautifulSoup(html_text, "lxml")
            work_urls = {}
            for frame in soup.select('iframe[_src]'):
                src = str(frame.get("_src") or "")
                if "/api/work" not in src:
                    continue
                identity = str(frame.get("jobid") or "")
                if not identity and frame.get("data"):
                    try:
                        frame_data = json.loads(str(frame.get("data")))
                        identity = str(frame_data.get("_jobid") or frame_data.get("jobid") or "")
                    except (TypeError, ValueError):
                        pass
                if identity:
                    work_urls[identity] = src
            for job in jobs:
                identity = str(job.get("jobid") or "")
                if identity in work_urls:
                    job["_asterism_work_url"] = work_urls[identity]
                    diagnostics["server_work_urls"] = diagnostics.get("server_work_urls", 0) + 1
        except (AttributeError, TypeError, ValueError):
            pass
        return jobs, info

    # api.base imported this function directly, so patch its donor-local alias
    # rather than replacing the upstream module on disk.
    module.decode_course_card = decode_with_private_defaults

    def card_identity(item, index):
        prop = item.get("property") if isinstance(item.get("property"), Mapping) else {}
        work_id = prop.get("workid") or prop.get("workId")
        if work_id and str(item.get("type") or "").lower() == "workid":
            work_id = str(work_id)
            if not work_id.startswith("work-"):
                work_id = f"work-{work_id}"
        candidates = (
            ("jobid", item.get("jobid")), ("id", item.get("id")),
            ("property._jobid", prop.get("_jobid")),
            ("property.jobid", prop.get("jobid")),
            ("property.workid", work_id), ("mid", item.get("mid")),
            ("objectId", item.get("objectId")),
            ("property.objectid", prop.get("objectid")),
            ("aid", item.get("aid")), ("position", f"attachment-{index}"),
        )
        source, value = next((source, value) for source, value in candidates if value)
        diagnostics["identity_sources"] = diagnostics.get("identity_sources", {})
        diagnostics["identity_sources"][source] = diagnostics["identity_sources"].get(source, 0) + 1
        return str(value)

    def process(cards):
        passed_ids = set()
        adjusted = []
        identities = []
        for index, card in enumerate(cards):
            item = dict(card)
            diagnostics["attachment_cards"] += 1
            card_type = str(item.get("type") or item.get("property", {}).get("type") or "unknown")
            diagnostics["card_types"][card_type] = diagnostics["card_types"].get(card_type, 0) + 1
            diagnostics["card_keys_by_type"] = diagnostics.get("card_keys_by_type", {})
            known_keys = diagnostics["card_keys_by_type"].setdefault(card_type, [])
            for key in sorted(str(key) for key in item.keys()):
                if key not in known_keys and len(known_keys) < 64:
                    known_keys.append(key)
            if card_type.lower() == "workid":
                diagnostics["work_property_keys"] = diagnostics.get("work_property_keys", [])
                prop = item.get("property") if isinstance(item.get("property"), Mapping) else {}
                for key in sorted(str(key) for key in prop.keys()):
                    if key not in diagnostics["work_property_keys"] and len(diagnostics["work_property_keys"]) < 64:
                        diagnostics["work_property_keys"].append(key)
            identity = card_identity(item, index)
            identities.append(identity)
            if item.get("isPassed"):
                passed_ids.add(identity)
                diagnostics["passed_cards"] += 1
            if item.get("job") is None:
                diagnostics["null_job_cards"] += 1
            item["isPassed"] = False
            adjusted.append(item)
        jobs = original(adjusted)

        # Completed Chaoxing cards commonly carry job=null.  The donor treats
        # every such card as a possible read task and discards completed
        # video/document/work cards before its type handlers run.  Reuse those
        # exact handlers for inventory only; execution continues to use the
        # untouched donor state machine and its completion checks.
        handled_ids = {str(job.get("jobid") or job.get("id") or "") for job in jobs
                       if job.get("jobid") or job.get("id")}
        handlers = {
            "video": getattr(decode_module, "_process_video_task"),
            "document": getattr(decode_module, "_process_document_task"),
            "workid": getattr(decode_module, "_process_work_task"),
        }
        for index, item in enumerate(adjusted):
            card_type = str(item.get("type") or "").lower()
            identity = identities[index]
            if identity in handled_ids:
                diagnostics["duplicate_cards"] = diagnostics.get("duplicate_cards", 0) + 1
                continue
            if item.get("job") is not None or card_type not in handlers:
                continue
            recovered = handlers[card_type](item)
            if recovered:
                # Keep the request-level jobid distinct from the stable card
                # identity.  Some completed/expired work cards expose only
                # property._jobid: the rendered iframe sends jobid="" while
                # originJobId carries that identity.  Filling both fields with
                # the recovered identity makes Chaoxing reject the read with
                # HTTP 403.
                recovered["_asterism_request_jobid"] = str(item.get("jobid") or "")
                if not recovered.get("jobid"):
                    recovered["jobid"] = identity
                recovered["_asterism_card_identity"] = identity
                jobs.append(recovered)
                handled_ids.add(identity)
                diagnostics["completed_jobs_recovered"] = diagnostics.get("completed_jobs_recovered", 0) + 1
        diagnostics["processed_jobs"] += len(jobs)
        for job in jobs:
            identity = str(job.get("_asterism_card_identity") or job.get("jobid") or job.get("id") or "")
            job["_asterism_is_passed"] = identity in passed_ids
        return jobs

    decode_module._process_attachment_cards = process
    module._asterism_card_diagnostics = diagnostics


def courses(module, payload, events, redactor):
    bot = bot_for(module, payload)
    original_decode = module.decode_course_list

    def decode_with_routes(html_text):
        rows = original_decode(html_text)
        try:
            from bs4 import BeautifulSoup
            soup = BeautifulSoup(html_text, "lxml")
            routes = {}
            for node in soup.select("div.course"):
                course_id = node.select_one("input.courseId")
                class_id = node.select_one("input.clazzId")
                anchor = node.select_one("a[href]")
                if course_id and class_id and anchor:
                    routes[(str(course_id.get("value")), str(class_id.get("value")))] = str(anchor.get("href"))
            for row in rows:
                route = routes.get((str(row.get("courseId")), str(row.get("clazzId"))))
                if route:
                    row["_asterism_course_href"] = route
        except (AttributeError, TypeError, ValueError):
            pass
        return rows

    module.decode_course_list = decode_with_routes
    with capture_output(events, redactor):
        rows = bot.get_course_list()
    session = module.SessionManager.get_session()
    result_rows = []
    for row in rows:
        grade_summary = discover_course_grade_summary(session, row, events, redactor)
        result_rows.append({
            "remote_id": str(row["clazzId"]),
            "title": clean_inventory_text(
                row.get("title"), 512, f'Chaoxing course {row["clazzId"]}'
            ),
            "teacher": clean_inventory_text(row.get("teacher"), 256, "Chaoxing")
            if row.get("teacher") else None,
            "provider_summary": {"grade": grade_summary} if grade_summary else {},
            "native": row,
        })
    return {"courses": result_rows,
            "session": {"cookies": cookies(module.SessionManager.get_session())}}


_GRADE_COMPONENT_LABELS = (
    ("video", ("视频", "音视频")),
    ("chapter_test", ("章节测验", "章节测试", "章节任务点", "任务点")),
    ("homework", ("作业",)),
    ("exam", ("考试",)),
    ("reading", ("阅读",)),
    ("live", ("直播",)),
    ("discussion", ("讨论",)),
    ("check_in", ("签到",)),
    ("document", ("文档",)),
    ("visit", ("访问",)),
    ("class_activity", ("课堂互动", "课堂活动", "互动")),
)


def _bounded_grade_number(value: str, *, percent: bool = False) -> float | None:
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return None
    maximum = 100.0 if percent else 1000.0
    return parsed if 0.0 <= parsed <= maximum else None


def parse_course_grade_summary(html_text: str, source_url: str = "") -> dict[str, Any] | None:
    """Extract only explicit learner-visible grade facts from one first-party page."""
    from bs4 import BeautifulSoup
    from urllib.parse import urlsplit

    soup = BeautifulSoup(html_text, "lxml")
    overall_scores = []
    for text in soup.stripped_strings:
        compact = " ".join(str(text).split())
        match = re.search(
            r"(?:综合成绩|总成绩|最终成绩)\s*[:：]?\s*(\d+(?:\.\d{1,3})?)\s*(?:分|$)",
            compact,
        )
        if match:
            value = _bounded_grade_number(match.group(1), percent=True)
            if value is not None:
                overall_scores.append(value)

    components: dict[str, dict[str, Any]] = {}
    for node in soup.select("tr, li, .scoreItem, .score-item, .item, .detail-item, .dataItem"):
        text = " ".join(node.get_text(" ", strip=True).split())
        if not text or len(text) > 1024:
            continue
        component = next(
            (key for key, labels in _GRADE_COMPONENT_LABELS if any(label in text for label in labels)),
            None,
        )
        if component is None:
            continue
        facts: dict[str, Any] = {"type": component}
        explicit = False
        patterns = (
            ("weight_percent", r"(?:权重|占比)\s*[:：]?\s*(\d+(?:\.\d{1,3})?)\s*%", True),
            ("completion_percent", r"(?:完成度|完成率|进度)\s*[:：]?\s*(\d+(?:\.\d{1,3})?)\s*%", True),
            ("score", r"(?:得分|成绩)\s*[:：]?\s*(\d+(?:\.\d{1,3})?)\s*(?:分|$)", True),
            ("required_minutes", r"(?:要求|满分|需|应)\D{0,8}(\d+(?:\.\d{1,2})?)\s*分钟", False),
            ("observed_minutes", r"(?:已读|已看|观看|阅读|直播|时长)\D{0,8}(\d+(?:\.\d{1,2})?)\s*分钟", False),
        )
        for name, pattern, is_percent in patterns:
            match = re.search(pattern, text)
            if match:
                value = _bounded_grade_number(match.group(1), percent=is_percent)
                if value is not None:
                    facts[name] = value
                    explicit = True
        if explicit:
            facts["label"] = clean_inventory_text(text, 512, component)
            previous = components.get(component, {})
            components[component] = {**previous, **facts}

    unique_overall = sorted(set(overall_scores))
    if len(unique_overall) > 1:
        # Conflicting visible totals are not safe to normalize.
        unique_overall = []
    if not unique_overall and not components:
        return None
    return {
        "overall_score": unique_overall[0] if len(unique_overall) == 1 else None,
        "components": list(components.values()),
        "source_path": urlsplit(source_url).path[:512] if source_url else None,
    }


def discover_course_grade_summary(session, course: Mapping[str, Any], events, redactor):
    """Follow a small set of course-local score links without mutating course state."""
    from bs4 import BeautifulSoup
    from urllib.parse import urljoin, urlsplit

    course_href = str(course.get("_asterism_course_href") or "").strip()
    if not course_href:
        return None
    try:
        with capture_output(events, redactor):
            home = session.get(urljoin("https://mooc2-ans.chaoxing.com", course_href))
        if home.status_code != 200:
            return None
        documents = [(str(home.url), home.text)]
        soup = BeautifulSoup(home.text, "lxml")
        candidates = []
        for node in soup.select("a, [data-url], [data-href]"):
            label = " ".join(node.get_text(" ", strip=True).split())
            values = [node.get("href"), node.get("data-url"), node.get("data-href"), node.get("onclick")]
            for raw in values:
                raw = str(raw or "").strip()
                if not raw:
                    continue
                quoted = re.search(r"['\"](https?://[^'\"]+|/[^'\"]+)['\"]", raw)
                route = quoted.group(1) if quoted else raw
                if "成绩" not in label and not re.search(r"(?:score|grade|statistic|achievement)", route, re.I):
                    continue
                target = urljoin(str(home.url), route)
                parsed = urlsplit(target)
                host = parsed.hostname or ""
                if parsed.scheme == "https" and (host == "chaoxing.com" or host.endswith(".chaoxing.com")):
                    candidates.append(target)
        seen = {str(home.url)}
        for target in candidates[:5]:
            if target in seen:
                continue
            seen.add(target)
            with capture_output(events, redactor):
                response = session.get(target)
            if response.status_code == 200:
                documents.append((str(response.url), response.text))
        summaries = [parse_course_grade_summary(text, url) for url, text in documents]
        summaries = [summary for summary in summaries if summary]
        if not summaries:
            return None
        overall = {summary["overall_score"] for summary in summaries if summary["overall_score"] is not None}
        components = {}
        for summary in summaries:
            for component in summary["components"]:
                components[component["type"]] = {**components.get(component["type"], {}), **component}
        return {
            "overall_score": next(iter(overall)) if len(overall) == 1 else None,
            "components": list(components.values()),
            "source_paths": sorted({summary["source_path"] for summary in summaries if summary["source_path"]}),
        }
    except Exception as error:
        events.emit("log", level="warning", message=redactor.text(
            f"Chaoxing grade composition read skipped: {clean_inventory_text(error, 256, 'read failed')}"
        ))
        return None


def inventory(module, payload, events, redactor):
    bot = bot_for(module, payload)
    include_completed_cards(module)
    # The donor treats an empty card page as something to "study" and calls a
    # progress endpoint from inside the otherwise read-looking get_job_list.
    # Inventory must remain side-effect free, so retain its card parser and
    # request order while suppressing only that fallback mutation.
    bot.study_emptypage = lambda *_args, **_kwargs: None
    course = dict(require_mapping(payload.get("course"), "payload.course"))
    with capture_output(events, redactor):
        points = bot.get_course_point(course["courseId"], course["clazzId"], course["cpi"])["points"]
    tasks = []
    seen_remote_ids = set()
    for point_index, point in enumerate(points):
        with capture_output(events, redactor):
            jobs, job_info = bot.get_job_list(course, point)
        remote_id = f'knowledge:{point["id"]}'
        if remote_id in seen_remote_ids:
            module._asterism_card_diagnostics["duplicate_task_remote_ids"] = (
                module._asterism_card_diagnostics.get("duplicate_task_remote_ids", 0) + 1
            )
            continue
        seen_remote_ids.add(remote_id)
        work_items = [job for job in jobs if job.get("type") == "workid"]
        completed = bool(point.get("has_finished")) or bool(
            jobs and all(job.get("_asterism_is_passed") for job in jobs)
        )
        not_open = bool(point.get("need_unlock") or job_info.get("notOpen"))
        capabilities = ["run"]
        if work_items:
            capabilities.insert(0, "questions")
        tasks.append({
            "remote_id": remote_id,
            "title": clean_inventory_text(
                point.get("title"), 512, f"Chaoxing knowledge point {point['id']}"
            ),
            "state": "completed" if completed else "not_open" if not_open else "pending",
            "source_type": "chapter",
            "capabilities": capabilities,
            "native": {
                "route_kind": "knowledge_point",
                "provider_position": point_index + 1,
                "course": course,
                "point": point,
                "jobs": [
                    {"job": job, "job_info": job_info, "job_index": index}
                    for index, job in enumerate(jobs)
                ],
            },
        })

    exam_count = 0
    auxiliary = cxkitty_for(payload, events, redactor)
    if auxiliary is not None:
        _cxapi, api, classes = auxiliary
        class_index = cxkitty_class_index(classes, course)
        with capture_output(events, redactor):
            exams = classes.get_exam_by_index(class_index)
        for exam in exams:
            remote_id = f"exam:{exam.exam_id}"
            if remote_id in seen_remote_ids:
                continue
            seen_remote_ids.add(remote_id)
            completed = str(getattr(exam.status, "value", exam.status)) == "已完成"
            tasks.append({
                "remote_id": remote_id,
                "title": clean_inventory_text(exam.name, 512, f"Chaoxing exam {exam.exam_id}"),
                "state": "completed" if completed else "pending",
                "source_type": "exam",
                # Independent exams are not chapter task points. Asterism must
                # require the separate submit confirmation before this route
                # may perform the donor's final_submit call.
                "assessment_class": "formal",
                "deadline": clean_inventory_text(exam.expire_time, 128, "") or None,
                "capabilities": ["questions", "run"],
                "native": {
                    "route_kind": "course_exam",
                    "course": course,
                    "exam": {
                        "exam_id": exam.exam_id,
                        "course_id": exam.course_id,
                        "class_id": exam.class_id,
                        "cpi": exam.cpi,
                        "enc_task": exam.enc_task,
                        "status": str(getattr(exam.status, "value", exam.status)),
                        "expire_time": exam.expire_time,
                    },
                },
            })
            exam_count += 1

    homework_count = 0
    homework_diagnostics = {}
    session = module.SessionManager.get_session()
    from urllib.parse import parse_qs, urljoin, urlsplit
    course_href = str(course.get("_asterism_course_href") or "")
    course_query = parse_qs(urlsplit(course_href).query)
    course_home_values = {}
    if course_href:
        try:
            with capture_output(events, redactor):
                course_home = session.get(urljoin("https://mooc2-ans.chaoxing.com", course_href))
            from bs4 import BeautifulSoup
            course_home_soup = BeautifulSoup(course_home.text, "lxml")
            for key in ("courseid", "courseId", "clazzid", "classId", "cpi", "enc", "workEnc", "t"):
                node = course_home_soup.select_one(f"#{key}, input[name='{key}']")
                if node and node.get("value"):
                    course_home_values[key] = str(node.get("value"))
        except Exception as error:
            homework_diagnostics["course_home_error"] = clean_inventory_text(error, 512, "course home read failed")
    homework_params = {
        "courseId": course["courseId"], "classId": course["clazzId"],
        "cpi": course.get("cpi", ""), "ut": "s",
    }
    stuenc = (course_home_values.get("enc") or next(iter(course_query.get("stuenc", [])
              or course_query.get("enc", [])), ""))
    work_enc = course_home_values.get("workEnc", "")
    if stuenc:
        homework_params["stuenc"] = stuenc
    if work_enc or stuenc:
        # Current pages separate workEnc/stuenc; older pages expose one enc.
        homework_params["enc"] = work_enc or stuenc
    if course_home_values.get("t"):
        homework_params["t"] = course_home_values["t"]
    try:
        with capture_output(events, redactor):
            response = session.get("https://mooc1.chaoxing.com/mooc2/work/list", params=homework_params)
        homework_diagnostics = {
            "http_status": response.status_code,
            "final_path": urlsplit(str(response.url)).path,
        }
        if response.status_code == 200:
            from bs4 import BeautifulSoup
            first_soup = BeautifulSoup(response.text, "lxml")
            scripts = "\n".join(node.get_text(" ", strip=False) for node in first_soup.select("script"))
            page_match = re.search(r"pageNum\s*:\s*(\d+)", scripts)
            page_count = max(1, int(page_match.group(1))) if page_match else 1
            pages = [(response, first_soup)]
            for page_num in range(2, min(page_count, 200) + 1):
                page_params = dict(homework_params)
                page_params.update({"status": "0", "topicId": "0", "pageNum": str(page_num)})
                with capture_output(events, redactor):
                    page_response = session.get(
                        "https://mooc1.chaoxing.com/mooc-ans/mooc2/work/list", params=page_params
                    )
                if page_response.status_code != 200:
                    break
                pages.append((page_response, BeautifulSoup(page_response.text, "lxml")))
            homework_diagnostics["pages"] = len(pages)
            homework_diagnostics["work_enc_present"] = bool(work_enc)
            homework_diagnostics["stuenc_present"] = bool(stuenc)
            for page_response, soup in pages:
                for index, item in enumerate(soup.select("li[data]")):
                    route = str(item.get("data") or "").strip()
                    if not route:
                        continue
                    absolute_route = urljoin(str(page_response.url), route)
                    query = parse_qs(urlsplit(absolute_route).query)
                    work_id = next(iter(query.get("workId", []) or query.get("workid", [])), None)
                    if not work_id:
                        continue
                    answer_id = next(iter(query.get("answerId", [])), "")
                    parsed_route = urlsplit(absolute_route)
                    if parsed_route.path.endswith("/mooc2/work/task"):
                        target_path = ("/mooc-ans/mooc2/work/view" if answer_id and answer_id != "0"
                                       else "/mooc-ans/mooc2/work/dowork")
                        absolute_route = parsed_route._replace(path=target_path).geturl()
                    label = " ".join(str(item.get("aria-label") or "").split())
                    text = label or " ".join(item.get_text(" ", strip=True).split())
                    state = homework_inventory_state(text, answer_id)
                    title_node = item.select_one(".workTit, .overHidden2, h3, p")
                    title = (label.split(";")[0] if label else
                             title_node.get_text(" ", strip=True) if title_node else text)
                    remote_id = f"homework:{work_id}"
                    if remote_id in seen_remote_ids:
                        continue
                    seen_remote_ids.add(remote_id)
                    tasks.append({
                        "remote_id": remote_id,
                        "title": clean_inventory_text(title, 512, f"Chaoxing homework {index + 1}"),
                        "state": state,
                        "source_type": "work",
                        # Independent homework follows the same fill/review/
                        # confirm boundary as an exam. Chapter workid cards stay
                        # routine and may be submitted automatically.
                        "assessment_class": "formal",
                        "capabilities": ["questions", "run"],
                        "native": {
                            "route_kind": "course_homework", "course": course,
                            "homework": {"route": absolute_route, "work_id": str(work_id),
                                         "answer_id": answer_id,
                                         "title": clean_inventory_text(title, 512, "Chaoxing homework"),
                                         "list_text": clean_inventory_text(text, 1024, "")},
                        },
                    })
                    homework_count += 1
    except Exception as error:
        homework_diagnostics["error"] = clean_inventory_text(error, 512, "homework inventory failed")
    return {"tasks": tasks,
            "scan_diagnostics": {"points": len(points), "course_exams": exam_count,
                                 "course_homeworks": homework_count,
                                 "course_homework_scan": homework_diagnostics,
                                 **module._asterism_card_diagnostics},
            "session": {"cookies": cookies(module.SessionManager.get_session())}}


class QuestionsCaptured(Exception):
    pass


class CaptureTiku:
    DISABLE = False
    is_manual = False
    skip_answer_validation = True
    true_list: list[str] = []
    false_list: list[str] = []

    def __init__(self): self.questions = []
    def query_all(self, questions, query_delay=0):
        self.questions = questions
        raise QuestionsCaptured


class AsterismTiku:
    """Minimal donor-compatible answer source; never invents a missing answer."""
    DISABLE = False
    is_manual = False
    skip_answer_validation = True
    COVER_RATE = 0.9
    true_list = ["正确", "对", "true", "1"]
    false_list = ["错误", "错", "false", "0"]

    def __init__(self, answers: Mapping[str, Any], cover_rate: float = 0.9):
        self.answers = {str(key): value for key, value in answers.items()}
        self.COVER_RATE = max(0.0, min(1.0, float(cover_rate)))

    def query_all(self, questions, query_delay=0):
        values = []
        for question in questions:
            question_id = str(question.get("id", ""))
            if question_id not in self.answers:
                values.append(None)
                continue
            value = self.answers[question_id]
            if value is None or value == "" or value == []:
                values.append(None)
                continue
            values.append(value)
        return values

    def judgement_select(self, value):
        normalized = str(value).strip().lower()
        if normalized in {item.lower() for item in self.true_list}:
            return True
        if normalized in {item.lower() for item in self.false_list}:
            return False
        raise WorkerFailure("answer_invalid", "true/false answer was not recognized")

    @staticmethod
    def get_submit_params():
        return ""


def map_question(item: Mapping[str, Any], position: int):
    question_id = str(item.get("id", position))
    answer_field = item.get("answerField") if isinstance(item.get("answerField"), Mapping) else {}
    native_type = str(answer_field.get(f"answertype{question_id}") or item.get("type", "")).strip()
    # Keep Chaoxing's native code authoritative.  The OCS donor documents the
    # extended families that Samueli's five-value mapping currently misses.
    kind = {
        "0": "single_choice", "1": "multiple_choice", "2": "short_answer",
        "3": "true_false", "4": "fill_blank", "5": "short_answer",
        "6": "short_answer", "8": "fill_blank", "11": "matching",
        "14": "provider_native_cloze", "15": "provider_native_reading",
        "single": "single_choice", "multiple": "multiple_choice",
        "completion": "fill_blank", "judgement": "true_false",
        "shortanswer": "short_answer", "单选题": "single_choice",
        "多选题": "multiple_choice", "填空题": "fill_blank",
        "判断题": "true_false", "简答题": "short_answer",
    }.get(native_type, "provider_native")
    options = [line.strip() for line in str(item.get("options", "")).splitlines() if line.strip()]
    return {"remote_id": question_id, "position": position, "kind": kind,
            "prompt": str(item.get("title", "")), "options": options,
            "answer_evidence": None,
            "native_shape": {
                "native_type": native_type,
                "donor_type": str(item.get("type", "")),
                "keys": sorted(str(key) for key in item.keys())[:64],
                "option_container": type(item.get("options")).__name__,
            },
            "native": dict(item)}


def _clean_node_text(node) -> str:
    return " ".join(node.get_text(" ", strip=True).split()) if node else ""


def _rich_node_text(node) -> tuple[str, int, int]:
    """Preserve visual blanks/underlines that plain ``get_text`` destroys."""
    if not node:
        return "", 0, 0
    from bs4 import NavigableString, Tag

    blank_index = 0
    underline_count = 0

    def marker() -> str:
        nonlocal blank_index
        blank_index += 1
        return f" [BLANK_{blank_index}] "

    def is_underlined(element: Tag) -> bool:
        classes = " ".join(str(value).lower() for value in (element.get("class") or []))
        style = re.sub(r"\s+", "", str(element.get("style") or "").lower())
        return (
            element.name in {"u", "ins"}
            or "underline" in classes
            or "text-decoration:underline" in style
            or "text-decoration-line:underline" in style
            or "border-bottom:" in style
        )

    def render(element) -> str:
        nonlocal underline_count
        if isinstance(element, NavigableString):
            value = str(element).replace("\xa0", " ")
            return re.sub(r"_{2,}|＿{2,}", lambda _match: marker(), value)
        if not isinstance(element, Tag):
            return ""
        if element.name == "img":
            from urllib.parse import urlsplit, urlunsplit

            source = str(element.get("src") or element.get("data-original") or "").strip()
            if source.startswith("//"):
                source = f"https:{source}"
            parsed = urlsplit(source)
            if parsed.scheme in {"http", "https"} and parsed.netloc:
                source = urlunsplit(("https", parsed.netloc, parsed.path, "", ""))
            elif source and not source.startswith("/"):
                source = "embedded"
            return f" [QUESTION_IMAGE:{source or 'embedded'}] "
        if element.name in {"audio", "video", "source"}:
            from urllib.parse import urlsplit, urlunsplit

            source = str(element.get("src") or "").strip()
            if not source and element.name in {"audio", "video"}:
                nested_source = element.find("source", src=True)
                source = str(nested_source.get("src") or "").strip() if nested_source else ""
            if source.startswith("//"):
                source = f"https:{source}"
            parsed = urlsplit(source)
            if parsed.scheme in {"http", "https"} and parsed.netloc:
                source = urlunsplit(("https", parsed.netloc, parsed.path, "", ""))
            elif source:
                source = "embedded"
            media_kind = (
                "AUDIO" if element.name == "audio"
                or str(element.get("type") or "").startswith("audio/") else "VIDEO"
            )
            return f" [QUESTION_{media_kind}:{source or 'embedded'}] "
        classes = " ".join(str(value).lower() for value in (element.get("class") or []))
        if element.name == "math" or any(
            marker in classes for marker in ("mathjax", "latex", "katex", "formula")
        ):
            formula = " ".join(element.get_text(" ", strip=True).split()).replace("]", "］")
            return f" [QUESTION_FORMULA:{formula or 'embedded'}] "
        if element.name == "a":
            from urllib.parse import urlsplit, urlunsplit

            href = str(element.get("href") or "").strip()
            parsed = urlsplit(href)
            if re.search(r"\.(?:pdf|docx?|xlsx?|pptx?|zip|rar|7z|txt|csv)(?:$|[?#])", href, re.I):
                stable = (
                    urlunsplit(("https", parsed.netloc, parsed.path, "", ""))
                    if parsed.netloc else "embedded"
                )
                label = " ".join(element.get_text(" ", strip=True).split()).replace("]", "］")
                return f" [QUESTION_FILE:{stable or 'embedded'}|{label or 'file'}] "
        if element.name in {"input", "textarea"} or element.get("contenteditable") == "true":
            input_type = str(element.get("type") or "text").lower()
            if input_type not in {"hidden", "radio", "checkbox", "button", "submit"}:
                return marker()
        if element.name == "br":
            return "\n"
        rendered = "".join(render(child) for child in element.children)
        if is_underlined(element):
            meaningful = " ".join(rendered.split())
            if not meaningful:
                return marker()
            underline_count += 1
            return f" [UNDERLINE]{meaningful}[/UNDERLINE] "
        return rendered

    value = render(node)
    value = re.sub(r"[ \t\r\f\v]+", " ", value)
    value = re.sub(r" *\n *", "\n", value)
    return value.strip(), blank_index, underline_count


def _shared_option_bank(soup) -> list[str]:
    """Capture page-level word/option banks without mistaking navigation for answers."""
    selectors = (
        ".wordBank", ".word-bank", ".word_box", ".wordBox", ".wordsBox",
        ".optionBank", ".option-bank", ".sharedOptions", ".shared-options",
        "[class*='wordBank']", "[class*='optionBank']",
    )
    values: list[str] = []
    for container in soup.select(", ".join(selectors)):
        if container.find_parent(class_=lambda value: value and (
            "singleQuesId" in value or "questionLi" in value
        )):
            continue
        nodes = container.select("li, option, [data-option], .word, .option")
        if not nodes:
            nodes = [container]
        for item in nodes:
            text = _clean_node_text(item)
            if text and len(text.encode("utf-8")) <= 512 and text not in values:
                values.append(text)
    return values[:256]


def parse_completed_work_result(html: str) -> list[dict[str, Any]]:
    """Parse Chaoxing's reviewed-result DOM using the audited OCS selectors."""
    from bs4 import BeautifulSoup

    soup = BeautifulSoup(html, "lxml")
    rows = []
    shared_options = _shared_option_bank(soup)
    page_type_text = _clean_node_text(soup.select_one(".tepytitH3, .typeTit, .questionType"))
    page_type_match = re.search(r"(?:【([^】]+)】|((?:单选|多选|判断|填空|简答|连线|排序|完形填空|阅读理解|共用选项|资料|口语|听力)题))", page_type_text)
    page_type_label = next(
        (part.strip() for part in (page_type_match.groups() if page_type_match else ()) if part),
        "",
    )
    label_kinds = {
        "单选题": "single_choice", "多选题": "multiple_choice",
        "判断题": "true_false", "填空题": "fill_blank",
        "简答题": "short_answer", "名词解释": "short_answer",
        "论述题": "short_answer", "连线题": "matching", "排序题": "ordering",
        "计算题": "provider_native_calculation",
        "完形填空": "provider_native_cloze",
        "阅读理解": "provider_native_reading",
        "共用选项题": "provider_native_shared_options",
        "资料题": "provider_native_composite",
        "口语题": "provider_native_oral",
        "听力题": "provider_native_listening",
    }
    seen_remote_ids: dict[str, int] = {}
    for position, root in enumerate(soup.select(".singleQuesId, .questionLi"), 1):
        title_node = root.select_one(".Zy_TItle, .tit, .mark_name")
        title, blank_count, underline_count = _rich_node_text(title_node)
        label_match = re.search(r"【([^】]+)】", title)
        inline_type_label = next(
            (label for label in label_kinds if label in title[:48]),
            "",
        )
        type_label = (
            label_match.group(1).strip() if label_match
            else inline_type_label or page_type_label
        )
        first = [_clean_node_text(item) for item in root.select(".firstUlList > li")][1:]
        second = [_clean_node_text(item) for item in root.select(".secondUlList > li")][1:]
        is_matching = bool(first and second)
        structural_classes = {
            str(value)
            for node in root.select("[class]")
            for value in (node.get("class") or [])
        }
        # Some Exam detail endpoints return one lazily loaded question without
        # the usual ``【题型】`` heading.  Preserve the native fallback unless
        # the result DOM itself exposes an unambiguous question-family class.
        structural_kinds = (
            ("single_choice", {"singleChoice", "single_choice", "danxuan"}),
            ("multiple_choice", {"multipleChoice", "multiple_choice", "duoxuan"}),
            ("true_false", {"trueOrFalse", "true_false", "panduan"}),
            ("fill_blank", {"fillBlank", "fill_blank", "tiankong"}),
        )
        structural_kind = next(
            (candidate for candidate, markers in structural_kinds if structural_classes & markers),
            None,
        )
        kind = (
            "matching" if is_matching
            else label_kinds.get(type_label, structural_kind or "provider_native")
        )
        if is_matching:
            options = first + second
        else:
            option_nodes = root.select(".answerBg .answer_p, .textDIV, .eidtDiv")
            if not option_nodes:
                option_nodes = root.select("ul li")
            options = [
                rich for item in option_nodes
                if (rich := _rich_node_text(item)[0])
            ]
        answer_node = root.select_one(".myAnswer .answerCon, .my-answer")
        submitted_answer = _clean_node_text(answer_node)
        if submitted_answer.startswith("我的答案："):
            submitted_answer = submitted_answer.removeprefix("我的答案：").strip()
        mark_text = _clean_node_text(root.select_one(".mark_answer, .mark-answer"))
        official_match = re.search(r"正确答案[：:]\s*(.+)$", mark_text)
        submitted_match = re.search(r"我的答案[：:]\s*(.*?)(?=\s*正确答案[：:]|$)", mark_text)
        if submitted_match:
            submitted_answer = submitted_match.group(1).strip()
        official_answer = official_match.group(1).strip() if official_match else ""
        submitted_correct = None
        if submitted_answer and official_answer:
            normalize_for_compare = lambda value: re.sub(r"[\s、,，;；#]", "", value).lower()
            submitted_correct = normalize_for_compare(submitted_answer) == normalize_for_compare(official_answer)
        trusted_answer = official_answer or (submitted_answer if submitted_correct is True else "")
        hidden_id = root.select_one("input[name='questionId'], input[name^='questionId']")
        provider_remote_id = str(
            root.get("data") or (hidden_id.get("value") if hidden_id else None)
            or str(root.get("id") or "").removeprefix("question") or position
        )
        occurrence = seen_remote_ids.get(provider_remote_id, 0) + 1
        seen_remote_ids[provider_remote_id] = occurrence
        # Shared-option/composite result pages can repeat the parent Provider
        # question ID for several visible child rows.  Keep that native ID for
        # later Provider-specific encoding, while giving each snapshot row a
        # deterministic identity accepted by Asterism's Question boundary.
        remote_id = (provider_remote_id if occurrence == 1
                     else f"{provider_remote_id}:child:{occurrence}")
        rows.append({
            "remote_id": remote_id,
            "position": position,
            "kind": kind,
            "prompt": title,
            "options": options,
            "answer_evidence": ({
                "source": "chaoxing_reviewed_result",
                "value": trusted_answer,
                "submitted_value": submitted_answer or None,
                "official_value": official_answer or None,
                "submitted_correct": submitted_correct,
            } if submitted_answer or official_answer else None),
            "native_shape": {
                "native_type": type_label,
                "root_classes": sorted(root.get("class") or []),
                "descendant_classes": sorted(structural_classes)[:64],
                "data_attributes": sorted(
                    str(key) for key in root.attrs if str(key).startswith("data-")
                )[:32],
                "input_names": sorted({
                    str(node.get("name")) for node in root.select("input[name]")
                    if node.get("name")
                })[:32],
                "first_group_count": len(first),
                "second_group_count": len(second),
                "historical_answer_present": bool(submitted_answer or official_answer),
                "official_answer_present": bool(official_answer),
                "blank_count": blank_count,
                "underline_count": underline_count,
                "shared_options": shared_options,
                "provider_remote_id": provider_remote_id,
                "provider_remote_id_occurrence": occurrence,
            },
            "native": {
                "type_label": type_label,
                "provider_remote_id": provider_remote_id,
                "shared_options": shared_options,
                "matching_groups": {"left": first, "right": second} if is_matching else None,
            },
        })
    return rows


def cxkitty_exam(payload, native, events, redactor):
    auxiliary = cxkitty_for(payload, events, redactor)
    if auxiliary is None:
        raise WorkerFailure("auxiliary_upstream_required", "CxKitty is required for course Exam tasks")
    cxapi, api, classes = auxiliary
    course = require_mapping(native.get("course"), "task.native.course")
    class_index = cxkitty_class_index(classes, course)
    target = require_mapping(native.get("exam"), "task.native.exam")
    with capture_output(events, redactor):
        current = classes.get_exam_by_index(class_index)
    module = next((item for item in current if str(item.exam_id) == str(target.get("exam_id"))), None)
    if module is None:
        raise WorkerFailure("task_stale", "Chaoxing Exam was absent during fresh rediscovery")
    exam = cxapi.ExamDto(
        api.session, api.acc, module.exam_id, module.course_id, module.class_id,
        module.cpi, module.enc_task,
    )

    def human_gate(*_args, **_kwargs):
        raise WorkerFailure("human_interaction_required", "Chaoxing Exam requires a browser/user verification gate")

    # CxKitty can automate these gates, but the thin worker deliberately hands
    # them back to Asterism/the user instead of silently solving or spoofing.
    exam._ExamDto__resolve_face_detection = human_gate
    exam._ExamDto__resolve_captcha = human_gate
    return cxapi, api, exam, module


def exam_cover_params(api, exam, *, redo=1):
    return {
        "redo": redo, "taskrefId": exam.exam_id, "courseId": exam.course_id,
        "classId": exam.class_id, "userId": api.acc.puid, "role": "", "source": 0,
        "enc_task": exam.enc_task, "cpi": exam.cpi, "vx": 0, "examsignal": 1,
    }


def exam_preview_response(exam):
    exam_module = importlib.import_module("cxapi.exam")
    utils_module = importlib.import_module("cxapi.utils")
    return exam.session.get(
        exam_module.PAGE_EXAM_PREVIEW,
        params={
            "courseId": exam.course_id, "classId": exam.class_id, "source": 0,
            "imei": utils_module.get_imei(), "start": 0, "cpi": exam.cpi,
            "examRelationId": exam.exam_id, "examRelationAnswerId": exam.exam_answer_id,
            "monitorStatus": 0, "monitorOp": -1,
            "remainTimeParam": exam.enc_remain_time,
            "relationAnswerLastUpdateTime": exam.last_update_time, "enc": exam.enc,
        },
    )


def work_read_params(course: Mapping[str, Any], native: Mapping[str, Any],
                     job: Mapping[str, Any], job_info: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "api": "1", "workId": str(job["jobid"]).replace("work-", ""),
        "jobid": job.get("_asterism_request_jobid", job["jobid"]),
        "originJobId": job["jobid"],
        "needRedirect": "true", "skipHeader": "true",
        "knowledgeid": str(job_info.get("knowledgeid") or native.get("point", {}).get("id", "")),
        "ktoken": job_info.get("ktoken", ""), "cpi": job_info.get("cpi") or course.get("cpi", ""),
        "ut": "s", "clazzId": course["clazzId"], "type": "",
        "enc": job.get("enc", ""), "mooc2": "1", "courseid": course["courseId"],
        "utenc": job_info.get("utenc", ""),
    }


def questions(module, payload, events, redactor):
    bot = bot_for(module, payload)
    task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native")
    if native.get("route_kind") == "course_exam":
        _cxapi, api, exam, exam_module = cxkitty_exam(payload, native, events, redactor)
        completed = str(getattr(exam_module.status, "value", exam_module.status)) == "已完成"
        with capture_output(events, redactor):
            if completed:
                cx_exam = importlib.import_module("cxapi.exam")
                response = exam.session.get(
                    cx_exam.PAGE_EXAM_COVER,
                    params=exam_cover_params(api, exam),
                    allow_redirects=True,
                )
            else:
                try:
                    exam.get_meta()
                    if exam.need_code:
                        raise WorkerFailure("human_interaction_required", "Chaoxing Exam requires an entry code")
                    try:
                        exam.start()
                    except NotImplementedError:
                        # CxKitty has already established the exact Exam
                        # session; its typed parser simply does not model this
                        # uncommon question family. Preserve the native DOM.
                        pass
                    response = exam_preview_response(exam)
                except WorkerFailure:
                    raise
        rows = parse_completed_work_result(response.text)
        answer_sheet_followed = False
        answer_sheet_target_path = ""
        look_detail_identifiers: list[str] = []
        look_detail_query_keys: list[str] = []
        summary_input_shapes: list[str] = []
        if not rows and completed:
            from bs4 import BeautifulSoup
            from urllib.parse import urljoin, urlsplit

            summary_soup = BeautifulSoup(response.text, "lxml")
            look_detail_index = response.text.find("/exam-ans/exam/phone/look-detail")
            if look_detail_index >= 0:
                look_detail_window = response.text[
                    max(0, look_detail_index - 1200):look_detail_index + 1200
                ]
                look_detail_identifiers = sorted({
                    identifier for identifier in re.findall(
                        r"[A-Za-z_][A-Za-z0-9_]{2,}", look_detail_window
                    )
                    if any(marker in identifier.lower()
                           for marker in ("answer", "exam", "relation", "enc", "course", "class", "url"))
                })[:32]
                look_detail_query_keys = sorted(set(re.findall(
                    r"[?&]([A-Za-z_][A-Za-z0-9_]*)=", look_detail_window
                )))[:32]
            summary_input_shapes = [
                f"{node.get('id', '')}:{node.get('name', '')}:{bool(str(node.get('value') or '').strip())}"
                for node in summary_soup.select("input[id]")
                if any(marker in str(node.get("id", "")).lower()
                       for marker in ("answer", "exam", "relation", "enc", "course", "class"))
            ][:24]
            answer_sheet = summary_soup.select_one("#answerSheetUrl")
            answer_sheet_value = str(
                (answer_sheet.get("value") if answer_sheet else "") or ""
            ).strip()
            if not answer_sheet_value and "/exam-ans/exam/phone/look-detail" in response.text:
                def hidden_value(selector: str) -> str:
                    node = summary_soup.select_one(selector)
                    return str((node.get("value") if node else "") or "").strip()

                answer_id = hidden_value("#answerId")
                relation_id = hidden_value("#examRelationId")
                exam_enc = hidden_value("#examEnc")
                course_id = hidden_value("#courseId")
                class_id = hidden_value("#classId")
                if answer_id and relation_id and exam_enc:
                    answer_sheet_url = urljoin(
                        str(response.url), "/exam-ans/exam/phone/look-detail"
                    )
                    answer_sheet_followed = True
                    answer_sheet_target_path = urlsplit(answer_sheet_url).path
                    detail_params = {
                        "answerId": answer_id,
                        "examRelationAnswerId": answer_id,
                        "examAnswerId": answer_id,
                        "examRelationId": relation_id,
                        "examId": relation_id,
                        "isDetail": "true",
                        "questionLinkId": "",
                        "times": "0",
                        "newVersion": "1",
                        "selfAnswerId": "",
                        "enc": exam_enc,
                        "examEnc": exam_enc,
                        "courseId": course_id,
                        "classId": class_id,
                    }
                    detail_referer = str(response.url)
                    with capture_output(events, redactor):
                        response = exam.session.get(
                            answer_sheet_url,
                            params=detail_params,
                            headers={"Referer": detail_referer},
                            allow_redirects=True,
                        )
                    rows = parse_completed_work_result(response.text)
                    detail_soup = BeautifulSoup(response.text, "lxml")
                    question_link_ids = []
                    for node in detail_soup.select(".ansCardBox [data], .queNumBox [data]"):
                        link_id = str(node.get("data") or "").strip()
                        if re.fullmatch(r"\d{1,32}", link_id) and link_id not in question_link_ids:
                            question_link_ids.append(link_id)
                    if question_link_ids:
                        rows = []
                        for link_id in question_link_ids:
                            per_question_params = dict(detail_params)
                            per_question_params["questionLinkId"] = link_id
                            with capture_output(events, redactor):
                                per_question = exam.session.get(
                                    answer_sheet_url,
                                    params=per_question_params,
                                    headers={"Referer": detail_referer},
                                    allow_redirects=True,
                                )
                            parsed = parse_completed_work_result(per_question.text)
                            if not parsed:
                                raise WorkerFailure(
                                    "question_shape_mismatch",
                                    "Chaoxing Exam answer-card item returned no question DOM",
                                )
                            rows.extend(parsed)
                            response = per_question
                        seen_exam_ids: dict[str, int] = {}
                        for position, row in enumerate(rows, 1):
                            provider_id = str(row["remote_id"])
                            occurrence = seen_exam_ids.get(provider_id, 0) + 1
                            seen_exam_ids[provider_id] = occurrence
                            if occurrence > 1:
                                row["remote_id"] = f"{provider_id}:child:{occurrence}"
                            row["position"] = position
            if answer_sheet_value:
                answer_sheet_url = urljoin(str(response.url), answer_sheet_value)
                answer_sheet_target = urlsplit(answer_sheet_url)
                if (answer_sheet_target.scheme != "https"
                        or not (answer_sheet_target.hostname or "").endswith(".chaoxing.com")):
                    raise WorkerFailure(
                        "upstream_shape_mismatch",
                        "Chaoxing Exam answer-sheet URL left the allowed Provider host",
                    )
                answer_sheet_followed = True
                answer_sheet_target_path = answer_sheet_target.path
                with capture_output(events, redactor):
                    response = exam.session.get(answer_sheet_url, allow_redirects=True)
                rows = parse_completed_work_result(response.text)
        if not rows:
            from bs4 import BeautifulSoup

            final_path = __import__("urllib.parse", fromlist=["urlsplit"]).urlsplit(
                str(response.url)
            ).path
            diagnostic_soup = BeautifulSoup(response.text, "lxml")
            if "该试卷不允许查看" in diagnostic_soup.get_text(" ", strip=True):
                raise WorkerFailure(
                    "question_access_denied",
                    "Chaoxing Exam owner disabled reviewed-paper access",
                )
            structural_classes = sorted({
                str(class_name)
                for node in diagnostic_soup.select("[class]")
                for class_name in (node.get("class") or [])
                if any(marker in str(class_name).lower()
                       for marker in ("question", "answer", "exam", "mark", "result", "test",
                                      "que", "subject", "topic", "detail", "stem", "tit", "ans"))
            })[:24]
            from collections import Counter

            common_classes = [
                name for name, _count in Counter(
                    str(class_name)
                    for node in diagnostic_soup.select("[class]")
                    for class_name in (node.get("class") or [])
                ).most_common(32)
            ]
            question_attribute_names = sorted({
                str(attribute)
                for node in diagnostic_soup.select("*")
                for attribute in node.attrs
                if any(marker in str(attribute).lower()
                       for marker in ("question", "answer", "type", "data"))
            })[:32]
            urlsplit = __import__("urllib.parse", fromlist=["urlsplit"]).urlsplit
            all_structural_paths = {
                urlsplit(str(node.get("href") or node.get("action") or "")).path
                for node in diagnostic_soup.select("a[href], form[action]")
                if str(node.get("href") or node.get("action") or "").strip()
            } | {
                match for match in re.findall(
                    r"(/exam-ans/[A-Za-z0-9_./-]+)", response.text
                )
            }
            structural_paths = [
                path for path in sorted(all_structural_paths)
                if not re.search(r"\.(?:css|js|png|jpg|jpeg|gif|svg|woff2?)$", path, re.I)
            ][:32]
            script_identifiers = sorted({
                identifier
                for script in diagnostic_soup.select("script:not([src])")
                for identifier in re.findall(
                    r"[A-Za-z_][A-Za-z0-9_]{3,}", script.get_text(" ")
                )
                if any(marker in identifier.lower()
                       for marker in ("ajax", "load", "question", "answer", "detail", "exam", "url"))
            })[:48]
            structural_ids = sorted({
                str(node.get("id")) for node in diagnostic_soup.select("[id]")
                if any(marker in str(node.get("id")).lower()
                       for marker in ("question", "answer", "exam", "mark", "result", "test"))
            })[:16]
            answer_sheet_node = diagnostic_soup.select_one("#answerSheetUrl")
            answer_sheet_tag = answer_sheet_node.name if answer_sheet_node else "absent"
            answer_sheet_attrs = sorted(str(key) for key in (answer_sheet_node.attrs if answer_sheet_node else {}))
            raise WorkerFailure(
                "question_inventory_empty",
                "Chaoxing Exam page exposed no recognized question rows "
                f"(path={final_path}, single={response.text.count('singleQuesId')}, "
                f"question_li={response.text.count('questionLi')}, bytes={len(response.content)}, "
                f"classes={','.join(structural_classes)}, ids={','.join(structural_ids)}, "
                f"common_classes={','.join(common_classes)}, attrs={','.join(question_attribute_names)}, "
                f"paths={','.join(structural_paths)}, script_vars={','.join(script_identifiers)}, "
                f"answer_sheet={answer_sheet_tag}:"
                f"{','.join(answer_sheet_attrs)}, followed={answer_sheet_followed}, "
                f"target={answer_sheet_target_path}, vars={','.join(look_detail_identifiers)}, "
                f"query_keys={','.join(look_detail_query_keys)}, "
                f"inputs={','.join(summary_input_shapes)})",
            )
        return {
            "questions": rows,
            "provider_native_inventory": {
                "route_kind": "course_exam", "http_status": response.status_code,
                "final_path": __import__("urllib.parse", fromlist=["urlsplit"]).urlsplit(str(response.url)).path,
                "single_question_nodes": response.text.count("singleQuesId"),
                "completed_before_scan": completed,
            },
            "session": {"cookies": exam.session.ck_dump()},
        }
    if native.get("route_kind") == "knowledge_point":
        course = dict(require_mapping(native.get("course"), "task.native.course"))
        point = dict(require_mapping(native.get("point"), "task.native.point"))
        children = native.get("jobs")
        if not isinstance(children, list):
            raise WorkerFailure("request_invalid", "task.native.jobs must be an array")
        rows = []
        for fallback_index, child_value in enumerate(children):
            child = require_mapping(child_value, "task.native.jobs item")
            job = dict(require_mapping(child.get("job"), "task.native.jobs item.job"))
            if job.get("type") != "workid":
                continue
            job_info = dict(require_mapping(
                child.get("job_info"), "task.native.jobs item.job_info"
            ))
            job_key = knowledge_job_key(job, int(child.get("job_index", fallback_index)))
            child_payload = dict(payload)
            child_payload["task"] = {
                "remote_id": task.get("remote_id"),
                "native": {
                    "course": course,
                    "point": point,
                    "job": job,
                    "job_info": job_info,
                },
            }
            child_result = questions(module, child_payload, events, redactor)
            for question in child_result.get("questions", []):
                row = dict(require_mapping(question, "knowledge-point question"))
                provider_question_id = require_text(
                    row.get("remote_id"), "knowledge-point question.remote_id"
                )
                row["remote_id"] = f"{job_key}:{provider_question_id}"
                row["position"] = len(rows) + 1
                native_shape = row.get("native_shape")
                native_shape = dict(native_shape) if isinstance(native_shape, Mapping) else {}
                native_shape.update({
                    "knowledge_point_id": str(point.get("id", "")),
                    "knowledge_job_id": job_key,
                    "provider_question_id": provider_question_id,
                })
                row["native_shape"] = native_shape
                rows.append(row)
        return {
            "questions": rows,
            "scan_source": "Chaoxing knowledge point work cards",
            "session": {"cookies": cookies(module.SessionManager.get_session())},
        }
    if native.get("route_kind") == "course_homework":
        homework = require_mapping(native.get("homework"), "task.native.homework")
        route = require_text(homework.get("route"), "task.native.homework.route")
        session = module.SessionManager.get_session()
        with capture_output(events, redactor):
            response = session.get(route)
        rows = parse_completed_work_result(response.text)
        if not rows and "ueditor_0" in response.text:
            rows = [{
                "remote_id": f"homework:{homework['work_id']}",
                "position": 1,
                "kind": "provider_native",
                "prompt": str(homework.get("title") or "Chaoxing rich-text homework"),
                "options": [],
                "answer_evidence": None,
                "native_shape": {
                    "native_type": "rich_text_homework",
                    "ueditor_present": True,
                    "submit_form_present": "submitForm" in response.text,
                },
                "native": {"route_kind": "course_homework_rich_text"},
            }]
        if not rows and "作业互评" in response.text and "/mooc2/work/eval-view" in response.text:
            from urllib.parse import parse_qs, urljoin, urlsplit

            review_soup = __import__("bs4", fromlist=["BeautifulSoup"]).BeautifulSoup(
                response.text, "lxml"
            )
            view_node = next((
                node for node in review_soup.select("[onclick]")
                if "evalView(" in str(node.get("onclick") or "")
            ), None)
            view_call = str(view_node.get("onclick") or "") if view_node else ""
            view_match = re.search(
                r"evalView\(\s*['\"]?([^,'\")\s]+)['\"]?\s*,\s*['\"]?([^,'\")\s]+)",
                view_call,
            )
            route_query = parse_qs(urlsplit(route).query)
            if view_match:
                detail_params = {
                    key: values[0] for key, values in route_query.items()
                    if key in {"workId", "courseId", "courseid", "classId", "clazzId", "cpi", "enc"}
                    and values
                }
                detail_params.update({
                    "evaluationId": view_match.group(1),
                    "answerId": view_match.group(2),
                })
                with capture_output(events, redactor):
                    detail_response = session.get(
                        urljoin(str(response.url), "/mooc2/work/eval-view"),
                        params=detail_params,
                        headers={"Referer": str(response.url)},
                    )
                detail_rows = parse_completed_work_result(detail_response.text)
                if detail_rows:
                    rows = detail_rows
                    response = detail_response
        if not rows:
            from bs4 import BeautifulSoup

            diagnostic_soup = BeautifulSoup(response.text, "lxml")
            structural_classes = sorted({
                str(value)
                for node in diagnostic_soup.select("[class]")
                for value in (node.get("class") or [])
                if any(marker in str(value).lower() for marker in (
                    "question", "answer", "work", "home", "subject", "topic",
                    "stem", "title", "tit", "option", "form", "editor",
                ))
            })[:48]
            structural_ids = sorted({
                str(node.get("id")) for node in diagnostic_soup.select("[id]")
                if any(marker in str(node.get("id")).lower() for marker in (
                    "question", "answer", "work", "subject", "topic", "form", "editor",
                ))
            })[:32]
            input_names = sorted({
                str(node.get("name")) for node in diagnostic_soup.select("input[name], textarea[name]")
                if node.get("name")
            })[:48]
            from urllib.parse import urlsplit

            page_plain_text = diagnostic_soup.get_text(" ", strip=True)
            if "无权限查看" in page_plain_text:
                raise WorkerFailure(
                    "question_access_denied",
                    "Chaoxing independent homework is not viewable by this account",
                )
            structural_paths = sorted({
                urlsplit(str(node.get("href") or node.get("action") or "")).path
                for node in diagnostic_soup.select("a[href], form[action]")
                if str(node.get("href") or node.get("action") or "").strip()
            })[:32]
            inline_code = " ".join(
                [node.get_text(" ") for node in diagnostic_soup.select("script:not([src])")]
                + [str(node.get("onclick") or "") for node in diagnostic_soup.select("[onclick]")]
            )
            action_identifiers = sorted({
                identifier for identifier in re.findall(
                    r"[A-Za-z_][A-Za-z0-9_]{2,}", inline_code
                )
                if any(marker in identifier.lower()
                       for marker in ("review", "view", "look", "detail", "work"))
            })[:32]
            script_paths = sorted(set(re.findall(
                r"(/[A-Za-z0-9_./-]*(?:review|view|look|detail|work)[A-Za-z0-9_./-]*)",
                inline_code,
                flags=re.I,
            )))[:32]
            function_signatures = sorted({
                f"{name}({','.join(part.strip() for part in params.split(',') if part.strip())})"
                for name, params in re.findall(
                    r"function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(([^)]*)\)", inline_code
                )
                if any(marker in name.lower() for marker in ("review", "view", "eval"))
            })[:16]
            endpoint_query_keys = sorted(set(re.findall(
                r"[?&]([A-Za-z_][A-Za-z0-9_]*)=",
                inline_code,
            )))[:32]
            raise WorkerFailure(
                "question_shape_mismatch",
                "Chaoxing independent homework exposed no recognized question rows "
                f"(status={response.status_code}, bytes={len(response.content)}, "
                f"classes={','.join(structural_classes)}, ids={','.join(structural_ids)}, "
                f"inputs={','.join(input_names)}, paths={','.join(structural_paths)}, "
                f"peer_review={('作业互评' in page_plain_text)}, "
                f"actions={','.join(action_identifiers)}, "
                f"script_paths={','.join(script_paths)}, "
                f"functions={','.join(function_signatures)}, "
                f"query_keys={','.join(endpoint_query_keys)})",
            )
        homework_answer_id = str(homework.get("answer_id") or "")
        result_markers = {
            "answer_record_present": bool(homework_answer_id and homework_answer_id != "0"),
            "graded_result_present": "selectWorkQuestionYiPiYue" in response.text,
            "my_answer_present": "我的答案" in response.text,
            "awaiting_review_present": any(
                marker in response.text for marker in ("等待教师批阅", "待批阅")
            ),
        }
        for row in rows:
            native_shape = row.get("native_shape")
            native_shape = dict(native_shape) if isinstance(native_shape, Mapping) else {}
            native_shape["homework_result"] = result_markers
            row["native_shape"] = native_shape
        from urllib.parse import urlsplit
        return {
            "questions": rows,
            "provider_native_inventory": {
                "route_kind": "course_homework", "http_status": response.status_code,
                "final_path": urlsplit(str(response.url)).path,
                "single_question_nodes": response.text.count("singleQuesId"),
                "question_li_nodes": response.text.count("questionLi"),
                "ueditor_present": "ueditor_0" in response.text,
                "submit_form_present": "submitForm" in response.text,
            },
            "session": {"cookies": cookies(session)},
        }
    job = dict(require_mapping(native.get("job"), "task.native.job"))
    if job.get("type") != "workid":
        return {"questions": [], "unsupported_reason": "task is not a donor work item"}
    course = dict(require_mapping(native.get("course"), "task.native.course"))
    job_info = dict(require_mapping(native.get("job_info"), "task.native.job_info"))

    # A completed card redirects the same donor GET to the reviewed-result
    # page.  Samueli's form parser cannot consume that DOM because .TiMu is the
    # root itself; OCS already documents the stable result selectors.
    session = module.SessionManager.get_session()
    with capture_output(events, redactor):
        if job.get("_asterism_work_url"):
            from urllib.parse import urljoin
            response = session.get(urljoin("https://mooc1.chaoxing.com", str(job["_asterism_work_url"])))
        else:
            response = session.get(
                "https://mooc1.chaoxing.com/mooc-ans/api/work",
                params=work_read_params(course, native, job, job_info),
            )
    reviewed = parse_completed_work_result(response.text) if response.status_code == 200 else []
    if reviewed:
        return {"questions": reviewed,
                "session": {"cookies": cookies(module.SessionManager.get_session())}}

    capture = CaptureTiku()
    bot.tiku = capture
    try:
        with capture_output(events, redactor):
            bot.study_work(course, job, job_info)
    except QuestionsCaptured:
        pass
    if not capture.questions:
        from urllib.parse import urlsplit
        return {
            "questions": [],
            "provider_native_inventory": {
                "http_status": response.status_code,
                "final_path": urlsplit(str(response.url)).path,
                "single_question_nodes": response.text.count("singleQuesId"),
                "reviewed_result_marker": "selectWorkQuestionYiPiYue" in str(response.url),
                "unfinished_teacher_marker": "教师未创建完成该测验" in response.text,
                "html_bytes": len(response.content),
            },
            "session": {"cookies": cookies(module.SessionManager.get_session())},
        }
    return {"questions": [map_question(q, i + 1) for i, q in enumerate(capture.questions)],
            "session": {"cookies": cookies(module.SessionManager.get_session())}}


def _provided_answers(value: Any) -> dict[str, Any]:
    if value is None:
        return {}
    if not isinstance(value, list):
        raise WorkerFailure("request_invalid", "payload.answers must be an array")
    answers = {}
    for item in value:
        row = require_mapping(item, "payload answer")
        remote_id = require_text(row.get("remote_id"), "answer.remote_id")
        if remote_id in answers:
            raise WorkerFailure("request_invalid", "payload.answers contains duplicate question IDs")
        answers[remote_id] = row.get("value")
    return answers


def knowledge_job_key(job: Mapping[str, Any], fallback_index: int) -> str:
    value = (job.get("_asterism_card_identity") or job.get("jobid")
             or job.get("id") or f"job-{fallback_index + 1}")
    return clean_inventory_text(value, 160, f"job-{fallback_index + 1}")


def fill_cxkitty_answer(question, value):
    type_name = str(getattr(question.type, "name", ""))
    if type_name == "单选题":
        raw = str(value).strip()
        if raw in question.options:
            question.answer = raw
            return
        matches = [key for key, text in question.options.items() if str(text).strip() == raw]
        if len(matches) == 1:
            question.answer = matches[0]
            return
    elif type_name == "多选题":
        values = value if isinstance(value, list) else [part for part in re.split(r"[#;,，；\s]+", str(value)) if part]
        keys = []
        for raw_value in values:
            raw = str(raw_value).strip()
            if raw in question.options:
                keys.append(raw)
                continue
            matches = [key for key, text in question.options.items() if str(text).strip() == raw]
            if len(matches) != 1:
                raise WorkerFailure("answer_invalid", f"reviewed answer did not match Exam option for {question.id}")
            keys.append(matches[0])
        if keys:
            question.answer = "".join(sorted(set(keys)))
            return
    elif type_name == "判断题":
        question.answer = AsterismTiku({}).judgement_select(value)
        return
    elif type_name == "填空题":
        values = value if isinstance(value, list) else [str(value)]
        if len(values) == len(question.options) and all(str(item).strip() for item in values):
            question.answer = [str(item) for item in values]
            return
    raise WorkerFailure("answer_invalid", f"reviewed answer could not be encoded for Exam question {question.id}")


def run_course_exam(payload, native, events, redactor):
    _cxapi, _api, exam, current = cxkitty_exam(payload, native, events, redactor)
    completed = str(getattr(current.status, "value", current.status)) == "已完成"
    if completed:
        return {
            "remote_state": "completed", "verified": True,
            "result": {"task_type": "course_exam", "already_completed": True},
            "session": {"cookies": exam.session.ck_dump()},
        }
    answers = _provided_answers(payload.get("answers"))
    if not answers:
        raise WorkerFailure("answer_required", "course Exam requires reviewed Asterism answers")
    events.emit("progress", current=0, total=None)
    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    final_submit = str(settings.get("assessment_mode") or "submit") == "submit"
    with capture_output(events, redactor):
        exam.get_meta()
        if exam.need_code:
            raise WorkerFailure("human_interaction_required", "Chaoxing Exam requires an entry code")
        try:
            exam.start()
        except NotImplementedError as error:
            raise WorkerFailure("question_type_unsupported", "CxKitty cannot encode the first Exam question type") from error
        questions = exam.fetch_all()
        for index, question in enumerate(questions):
            question_id = str(question.id)
            if question_id not in answers:
                raise WorkerFailure("answer_required", f"no reviewed answer for Exam question {question_id}")
            fill_cxkitty_answer(question, answers[question_id])
            exam.submit(index=index, question=question)
            events.emit("progress", current=index + 1, total=len(questions))
        if final_submit:
            exam.final_submit()
    if not final_submit:
        return {
            "remote_state": "in_progress", "verified": False,
            "result": {"task_type": "course_exam", "answers_saved": True,
                       "final_submit": False, "saved_questions": len(questions)},
            "session": {"cookies": exam.session.ck_dump()},
        }
    # CxKitty's course list is the donor's authoritative status endpoint.
    _cxapi, _api, fresh_exam, fresh = cxkitty_exam(payload, native, events, redactor)
    verified = str(getattr(fresh.status, "value", fresh.status)) == "已完成"
    return {
        "remote_state": "completed" if verified else "submitted",
        "verified": verified,
        "result": {"task_type": "course_exam", "submitted_questions": len(questions)},
        "session": {"cookies": fresh_exam.session.ck_dump()},
    }


def run_course_homework_browser(session, route, answers, homework, events, redactor,
                                final_submit=True):
    """Keep new homework DOM/AJAX semantics in a real browser, as audited upstreams do."""
    browser_value = os.environ.get("ASTERISM_CHAOXING_BROWSER_EXECUTABLE")
    if not browser_value:
        raise WorkerFailure("browser_required", "Chaoxing DOM homework requires a configured browser")
    browser_path = pathlib.Path(browser_value)
    if not browser_path.is_absolute():
        browser_path = pathlib.Path(__file__).resolve().parents[2] / browser_path
    if not browser_path.is_file():
        raise WorkerFailure("browser_unavailable", "configured Chaoxing browser executable is absent")
    try:
        from playwright.sync_api import sync_playwright
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", "playwright") from error

    events.emit("progress", current=0, total=len(answers) + 1)
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=str(browser_path), headless=True)
            try:
                context = browser.new_context()
                browser_cookies = [{
                    "name": str(name), "value": str(value), "domain": ".chaoxing.com",
                    "path": "/", "secure": True,
                } for name, value in cookies(session).items()]
                if browser_cookies:
                    context.add_cookies(browser_cookies)
                page = context.new_page()
                page.goto(route, wait_until="domcontentloaded", timeout=90_000)
                page.wait_for_timeout(3_000)
                if "passport" in page.url or "login" in page.url:
                    raise WorkerFailure("authentication_failed", "Chaoxing browser session was rejected")
                if page.locator("input[type=file]").count():
                    raise WorkerFailure(
                        "artifact_required",
                        "Chaoxing attachment homework requires an explicit uploaded artifact",
                    )
                for index, (question_id, value) in enumerate(answers.items(), 1):
                    result = page.evaluate(
                        r"""({questionId, value}) => {
                          const root = document.querySelector(`div.singleQuesId[data="${questionId}"]`)
                            || document.querySelector(`div.questionLi[data="${questionId}"]`)
                            || document.getElementById(`sigleQuestionDiv_${questionId}`);
                          const values = Array.isArray(value) ? value.map(String) : [String(value)];
                          const html = text => String(text).replace(/[&<>"']/g, char => ({
                            '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
                          })[char]);
                          if (root) {
                            const spans = [...root.querySelectorAll('span.num_option')];
                            if (spans.length) {
                              const wanted = typeof value === 'boolean'
                                ? (value ? ['true', '正确', '对', 'A'] : ['false', '错误', '错', 'B'])
                                : values;
                              let matched = 0;
                              for (const span of spans) {
                                const label = (span.textContent || '').trim();
                                const data = (span.getAttribute('data') || '').trim();
                                const option = (span.parentElement?.innerText || '').trim();
                                if (wanted.some(item => item === label || item === data || item === option)) {
                                  if (!span.classList.contains('check_answer')) span.parentElement.click();
                                  matched++;
                                }
                              }
                              return matched ? `choice:${matched}` : 'choice:no-match';
                            }
                            if (window.UE) {
                              let saved = 0;
                              values.forEach((text, offset) => {
                                const editor = UE.getEditor(`answerEditor${questionId}${offset + 1}`);
                                if (editor) { editor.setContent(`<p>${html(text)}</p>`); editor.sync(); saved++; }
                              });
                              const save = document.getElementById(`save_${questionId}`);
                              if (save) save.click();
                              return saved ? `editor:${saved}` : 'editor:no-match';
                            }
                          }
                          const frame = document.querySelector('#ueditor_0');
                          if (frame && values.length === 1) {
                            const doc = frame.contentDocument || frame.contentWindow.document;
                            doc.body.innerHTML = `<p>${html(values[0])}</p>`;
                            const textarea = document.querySelector('textarea');
                            if (textarea) textarea.value = doc.body.innerHTML;
                            return 'rich-text:1';
                          }
                          return 'question:not-found';
                        }""",
                        {"questionId": question_id, "value": value},
                    )
                    if str(result).endswith("no-match") or result == "question:not-found":
                        raise WorkerFailure(
                            "question_type_unsupported",
                            f"Chaoxing browser donor could not bind question {question_id}",
                        )
                    page.wait_for_timeout(1_500)
                    events.emit("progress", current=index, total=len(answers) + 1)
                if final_submit:
                    submit = page.get_by_text("提交", exact=True)
                    visible = [submit.nth(index) for index in range(submit.count())
                               if submit.nth(index).is_visible()]
                    if not visible:
                        raise WorkerFailure("browser_shape_mismatch", "Chaoxing homework submit control was absent")
                    visible[-1].click()
                    page.wait_for_timeout(1_500)
                    confirm = page.get_by_text(re.compile(r"^(提交|确定)$"))
                    visible_confirm = [confirm.nth(index) for index in range(confirm.count())
                                       if confirm.nth(index).is_visible()]
                    if visible_confirm:
                        visible_confirm[-1].click()
                    page.wait_for_timeout(5_000)
                    terminal_text = " ".join(page.locator("body").inner_text().split())
                    verified = any(marker in terminal_text for marker in
                                   ("提交成功", "等待教师批阅", "作业详情", "我的答案"))
                else:
                    # Some homework pages expose an explicit save control;
                    # others persist on blur/navigation. Never click submit in
                    # this branch.
                    page.locator("body").click(position={"x": 2, "y": 2})
                    save = page.get_by_text(re.compile(r"^(保存|暂存|保存答案)$"))
                    saves = [save.nth(index) for index in range(save.count())
                             if save.nth(index).is_visible()]
                    if saves:
                        saves[-1].click()
                    page.wait_for_timeout(3_000)
                    verified = False
            finally:
                browser.close()
    except WorkerFailure:
        raise
    except Exception as error:
        raise WorkerFailure("browser_execution_failed", redactor.text(error)) from error
    events.emit("progress", current=len(answers) + 1, total=len(answers) + 1)
    return {
        "remote_state": "completed" if verified else "in_progress", "verified": verified,
        "result": {"task_type": "course_homework", "browser_dom": True,
                   "submitted_questions": len(answers) if final_submit else 0,
                   "saved_questions": len(answers), "answers_saved": not final_submit,
                   "final_submit": final_submit,
                   "work_id": str(homework["work_id"])},
        "session": {"cookies": cookies(session)},
    }


def chapter_work_requires_browser(html: str, answers: Mapping[str, Any]) -> bool:
    """Keep Samueli on its proven 0-4 types; route native shapes to the DOM."""
    from bs4 import BeautifulSoup

    soup = BeautifulSoup(html, "lxml")
    type_by_id = {}
    for root in soup.select(".singleQuesId[data], .questionLi[data]"):
        question_id = str(root.get("data") or "").strip()
        type_node = root.select_one(".TiMu[data], [name^='answertype']")
        native_type = str(
            (type_node.get("data") if type_node and type_node.has_attr("data") else None)
            or (type_node.get("value") if type_node else None) or ""
        ).strip()
        if question_id:
            type_by_id[question_id] = native_type
    return any(
        isinstance(value, Mapping)
        or type_by_id.get(str(question_id), "") not in {"", "0", "1", "2", "3", "4"}
        for question_id, value in answers.items()
    )


def run_chapter_work_browser(session, route: str, answers: Mapping[str, Any], events, redactor):
    """Bind Provider-native chapter questions in the real Chaoxing DOM."""
    browser_value = os.environ.get("ASTERISM_CHAOXING_BROWSER_EXECUTABLE")
    if not browser_value:
        raise WorkerFailure("browser_required", "Chaoxing native chapter question requires a configured browser")
    browser_path = pathlib.Path(browser_value)
    if not browser_path.is_absolute():
        browser_path = pathlib.Path(__file__).resolve().parents[2] / browser_path
    if not browser_path.is_file():
        raise WorkerFailure("browser_unavailable", "configured Chaoxing browser executable is absent")
    try:
        from playwright.sync_api import sync_playwright
    except ModuleNotFoundError as error:
        raise WorkerFailure("dependency_missing", "playwright") from error

    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=str(browser_path), headless=True)
            try:
                context = browser.new_context()
                values = [{
                    "name": str(name), "value": str(value), "domain": ".chaoxing.com",
                    "path": "/", "secure": True,
                } for name, value in cookies(session).items()]
                if values:
                    context.add_cookies(values)
                page = context.new_page()
                page.goto(route, wait_until="domcontentloaded", timeout=90_000)
                page.wait_for_timeout(2_000)
                if "passport" in page.url or "login" in page.url:
                    raise WorkerFailure("authentication_failed", "Chaoxing browser session was rejected")
                for index, (question_id, value) in enumerate(answers.items(), 1):
                    result = page.evaluate(
                        r"""({questionId, value}) => {
                          const root = document.querySelector(`.singleQuesId[data="${questionId}"]`)
                            || document.querySelector(`.questionLi[data="${questionId}"]`)
                            || document.getElementById(`question${questionId}`);
                          if (!root) return 'question:not-found';
                          const norm = text => String(text ?? '').replace(/[\s、,，;；:：.．()（）]/g, '').toLowerCase();
                          const wanted = Array.isArray(value) ? value : [value];
                          const controls = [...root.querySelectorAll('input[type=radio], input[type=checkbox]')];
                          if (controls.length && (typeof value !== 'object' || Array.isArray(value))) {
                            let matched = 0;
                            for (const control of controls) {
                              const label = control.closest('label,li,.answerBg,.option')?.innerText || control.value;
                              if (wanted.some(item => norm(item) === norm(control.value) || norm(label).includes(norm(item)))) {
                                if (!control.checked) control.click();
                                matched++;
                              }
                            }
                            if (matched) return `choice:${matched}`;
                          }
                          const selects = [...root.querySelectorAll('select')];
                          if (selects.length) {
                            const pairs = value && !Array.isArray(value) && typeof value === 'object'
                              ? Object.entries(value) : wanted.map((item, offset) => [String(offset + 1), item]);
                            let matched = 0;
                            for (let offset = 0; offset < pairs.length; offset++) {
                              const [left, right] = pairs[offset];
                              const select = selects.find(candidate => norm(candidate.closest('li,tr,.line,.item')?.innerText).includes(norm(left))) || selects[offset];
                              if (!select) continue;
                              const option = [...select.options].find(candidate => norm(candidate.value) === norm(right) || norm(candidate.text).includes(norm(right)));
                              if (!option) continue;
                              select.value = option.value;
                              select.dispatchEvent(new Event('change', { bubbles: true }));
                              matched++;
                            }
                            if (matched === pairs.length) return `select:${matched}`;
                            return 'select:no-match';
                          }
                          const textValues = wanted.flat(Infinity).map(item => typeof item === 'object' ? JSON.stringify(item) : String(item));
                          const fields = [...root.querySelectorAll('textarea,input:not([type]),input[type=text],input[type=hidden][name*="answer"]')];
                          if (fields.length) {
                            fields.forEach((field, offset) => {
                              field.value = textValues[Math.min(offset, textValues.length - 1)];
                              field.dispatchEvent(new Event('input', { bubbles: true }));
                              field.dispatchEvent(new Event('change', { bubbles: true }));
                            });
                            return `field:${fields.length}`;
                          }
                          return 'native:no-control';
                        }""",
                        {"questionId": question_id, "value": value},
                    )
                    if str(result).endswith("no-match") or result in {"question:not-found", "native:no-control"}:
                        raise WorkerFailure("question_type_unsupported", f"Chaoxing DOM could not bind native question {question_id}")
                    events.emit("progress", current=index, total=len(answers))
                submit = page.get_by_text("提交", exact=True)
                visible = [submit.nth(index) for index in range(submit.count()) if submit.nth(index).is_visible()]
                if not visible:
                    raise WorkerFailure("browser_shape_mismatch", "Chaoxing chapter submit control was absent")
                visible[-1].click()
                page.wait_for_timeout(1_000)
                confirm = page.get_by_text(re.compile(r"^(提交|确定)$"))
                confirms = [confirm.nth(index) for index in range(confirm.count()) if confirm.nth(index).is_visible()]
                if confirms:
                    confirms[-1].click()
                page.wait_for_timeout(4_000)
            finally:
                browser.close()
    except WorkerFailure:
        raise
    except Exception as error:
        raise WorkerFailure("browser_execution_failed", redactor.text(error)) from error


def run_course_homework(bot, module, payload, native, events, redactor):
    homework = require_mapping(native.get("homework"), "task.native.homework")
    route = require_text(homework.get("route"), "task.native.homework.route")
    course = dict(require_mapping(native.get("course"), "task.native.course"))
    session = module.SessionManager.get_session()
    with capture_output(events, redactor):
        response = session.get(route)
    text = " ".join(response.text.split())
    list_text = str(homework.get("list_text") or "")
    answer_id = str(homework.get("answer_id") or "")
    if (answer_id and answer_id != "0") or any(
        marker in list_text for marker in ("已完成", "等待教师批阅", "待批阅", "已交")
    ) or any(
        marker in text for marker in ("提交成功", "等待教师批阅", "selectWorkQuestionYiPiYue")
    ):
        return {
            "remote_state": "completed", "verified": True,
            "result": {"task_type": "course_homework", "already_completed": True},
            "session": {"cookies": cookies(session)},
        }
    if "已过期" in list_text:
        raise WorkerFailure("task_expired", "Chaoxing course homework is expired and not submittable")
    answers = _provided_answers(payload.get("answers"))
    if not answers:
        raise WorkerFailure("answer_required", "course homework requires reviewed Asterism answers")
    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    final_submit = str(settings.get("assessment_mode") or "submit") == "submit"
    if (not final_submit or "ueditor_0" in response.text
            or "questionLi" in response.text and "singleQuesId" not in response.text):
        return run_course_homework_browser(
            session, str(response.url), answers, homework, events, redactor,
            final_submit=final_submit,
        )

    # Feed the exact server-rendered independent-homework page into Samueli's
    # established question decoding and submission order. Only its initial GET
    # is adapted; answer mapping and addStudentWorkNew remain donor-owned.
    cover_rate = float(settings.get("minimum_answer_coverage", 0.9))
    bot.tiku = AsterismTiku(answers, cover_rate)

    def refuse_random_answer(*_args, **_kwargs):
        # The donor asks for a random value when an answer is absent or cannot
        # be rebound. Preserve its partial-save/coverage flow without inventing
        # a value: blank remains blank in the outgoing form.
        return ""

    module.random_answer = refuse_random_answer
    original_get = session.get

    def route_get(url, *args, **kwargs):
        if str(url).rstrip("/").endswith("/mooc-ans/api/work"):
            return response
        return original_get(url, *args, **kwargs)

    session.get = route_get
    events.emit("progress", current=0, total=1)
    try:
        with capture_output(events, redactor):
            result = bot.study_work(
                course,
                {"jobid": f"work-{homework['work_id']}", "enc": "", "type": "workid"},
                {"knowledgeid": "", "ktoken": "", "cpi": course.get("cpi", "")},
            )
    finally:
        session.get = original_get
    if result.is_failure():
        raise WorkerFailure("execution_failed", "upstream rejected course homework execution")
    with capture_output(events, redactor):
        fresh = session.get(route)
    fresh_text = " ".join(fresh.text.split())
    verified = any(
        marker in fresh_text for marker in ("提交成功", "等待教师批阅", "selectWorkQuestionYiPiYue")
    )
    events.emit("progress", current=1, total=1)
    return {
        "remote_state": "completed" if verified else "submitted", "verified": verified,
        "result": {"task_type": "course_homework", "upstream_accepted": True},
        "session": {"cookies": cookies(session)},
    }


def run_knowledge_point(bot, module, payload, native, events, redactor):
    """Keep the donor's public unit of work: one knowledge point."""
    course = dict(require_mapping(native.get("course"), "task.native.course"))
    point = dict(require_mapping(native.get("point"), "task.native.point"))
    if point.get("has_finished"):
        return {
            "remote_state": "completed", "verified": True,
            "result": {"task_type": "knowledge_point", "already_completed": True},
            "session": {"cookies": cookies(module.SessionManager.get_session())},
        }
    if point.get("need_unlock"):
        raise WorkerFailure("task_not_open", "Chaoxing knowledge point is locked")
    children = native.get("jobs")
    if not isinstance(children, list):
        raise WorkerFailure("request_invalid", "task.native.jobs must be an array")
    supplied_answers = _provided_answers(payload.get("answers"))
    processed = 0
    skipped_answer_jobs = 0
    skipped_unsupported_jobs = 0
    if not children:
        with capture_output(events, redactor):
            result = bot.study_emptypage(course, point)
        if result.is_failure():
            raise WorkerFailure("execution_failed", "upstream rejected empty knowledge point")
        processed = 1
    for fallback_index, child_value in enumerate(children):
        child = require_mapping(child_value, "task.native.jobs item")
        job = dict(require_mapping(child.get("job"), "task.native.jobs item.job"))
        if job.get("_asterism_is_passed"):
            continue
        job_type = str(job.get("type", ""))
        if job_type not in ("video", "document", "read", "workid", "live"):
            skipped_unsupported_jobs += 1
            continue
        job_info = dict(require_mapping(
            child.get("job_info"), "task.native.jobs item.job_info"
        ))
        job_key = knowledge_job_key(job, int(child.get("job_index", fallback_index)))
        child_answers = []
        prefix = f"{job_key}:"
        for remote_id, value in supplied_answers.items():
            if remote_id.startswith(prefix):
                child_answers.append({"remote_id": remote_id[len(prefix):], "value": value})
        if job_type == "workid" and not child_answers:
            skipped_answer_jobs += 1
            continue
        child_payload = dict(payload)
        child_payload["answers"] = child_answers
        child_payload["task"] = {
            "remote_id": require_mapping(payload.get("task"), "payload.task").get("remote_id"),
            "native": {
                "course": course,
                "point": point,
                "job": job,
                "job_info": job_info,
            },
        }
        run_task(module, child_payload, events, redactor)
        processed += 1

    with capture_output(events, redactor):
        fresh_points = bot.get_course_point(
            course["courseId"], course["clazzId"], course["cpi"]
        ).get("points", [])
    fresh = next((row for row in fresh_points if str(row.get("id")) == str(point.get("id"))), None)
    verified = bool(fresh and fresh.get("has_finished"))
    return {
        "remote_state": "completed" if verified else "in_progress",
        "verified": verified,
        "result": {
            "task_type": "knowledge_point",
            "processed_jobs": processed,
            "skipped_answer_jobs": skipped_answer_jobs,
            "skipped_unsupported_jobs": skipped_unsupported_jobs,
            "fresh_completion_observed": verified,
        },
        "session": {"cookies": cookies(module.SessionManager.get_session())},
    }


def run_task(module, payload, events, redactor):
    """Run one chapter attachment through Samueli's original task methods."""
    bot = bot_for(module, payload)
    task = require_mapping(payload.get("task"), "payload.task")
    native = require_mapping(task.get("native"), "task.native")
    if native.get("route_kind") == "course_exam":
        return run_course_exam(payload, native, events, redactor)
    if native.get("route_kind") == "course_homework":
        return run_course_homework(bot, module, payload, native, events, redactor)
    if native.get("route_kind") == "knowledge_point":
        return run_knowledge_point(bot, module, payload, native, events, redactor)
    course = dict(require_mapping(native.get("course"), "task.native.course"))
    point = dict(require_mapping(native.get("point"), "task.native.point"))
    job = dict(require_mapping(native.get("job"), "task.native.job"))
    job_info = dict(require_mapping(native.get("job_info"), "task.native.job_info"))
    job_type = str(job.get("type", ""))
    if job.get("_asterism_is_passed") or point.get("has_finished"):
        return {"remote_state": "completed", "verified": True,
                "result": {"task_type": job_type, "already_completed": True},
                "session": {"cookies": cookies(module.SessionManager.get_session())}}

    settings = payload.get("settings") if isinstance(payload.get("settings"), Mapping) else {}
    speed = float(settings.get("speed", 2.0))
    if not 0 < speed or not speed < float("inf"):
        raise WorkerFailure("request_invalid", "speed must be a finite value greater than 0")
    events.emit("progress", current=0, total=1)
    with capture_output(events, redactor):
        if job_type == "video":
            result = bot.study_video(course, job, job_info, _speed=speed, _type="Video")
            if result.is_failure():
                result = bot.study_video(course, job, job_info, _speed=speed, _type="Audio")
        elif job_type == "document":
            result = bot.study_document(course, job)
        elif job_type == "read":
            result = bot.study_read(course, job, job_info)
        elif job_type == "workid":
            answers = _provided_answers(payload.get("answers"))
            if not answers:
                raise WorkerFailure("answer_required", "chapter work requires reviewed Asterism answers")
            session = module.SessionManager.get_session()
            response = session.get(
                "https://mooc1.chaoxing.com/mooc-ans/api/work",
                params=work_read_params(course, native, job, job_info),
            )
            if chapter_work_requires_browser(response.text, answers):
                run_chapter_work_browser(
                    session, str(response.url), answers, events, redactor
                )
                result = module.StudyResult.SUCCESS
            else:
                cover_rate = float(settings.get("minimum_answer_coverage", 0.9))
                bot.tiku = AsterismTiku(answers, cover_rate)

                def refuse_random_answer(*_args, **_kwargs):
                    return ""

                module.random_answer = refuse_random_answer
                result = bot.study_work(course, job, job_info)
        elif job_type == "live":
            live_module = importlib.import_module("api.live")
            live_process_module = importlib.import_module("api.live_process")
            defaults = {
                "userid": bot.get_uid(),
                "clazzId": course.get("clazzId"),
                "knowledgeid": job_info.get("knowledgeid"),
            }
            live = live_module.Live(
                attachment=job,
                defaults=defaults,
                course_id=course.get("courseId"),
            )
            thread = threading.Thread(
                target=live_process_module.LiveProcessor.run_live,
                # Accelerated live playback can report local completion
                # without satisfying Provider duration, so live stays 1x.
                args=(live, 1.0),
                daemon=True,
            )
            thread.start()
            thread.join()
            result = module.StudyResult.SUCCESS
        else:
            raise WorkerFailure("task_unsupported", f"Samueli does not execute task type {job_type!r}")
    if result.is_failure():
        raise WorkerFailure("execution_failed", f"upstream rejected {job_type} execution")

    # Reuse the donor's fresh card discovery to verify completion. Suppress
    # only its empty-page fallback mutation, exactly as read-only inventory.
    include_completed_cards(module)
    bot.study_emptypage = lambda *_args, **_kwargs: None
    with capture_output(events, redactor):
        fresh_jobs, _ = bot.get_job_list(course, point)
    target_identity = str(job.get("_asterism_card_identity") or job.get("jobid") or job.get("id") or "")
    fresh = next((row for row in fresh_jobs if str(row.get("_asterism_card_identity")
                 or row.get("jobid") or row.get("id") or "") == target_identity), None)
    completed = bool(fresh and fresh.get("_asterism_is_passed"))
    events.emit("progress", current=1, total=1)
    return {"remote_state": "completed" if completed else "in_progress", "verified": completed,
            "result": {"task_type": job_type, "upstream_accepted": True,
                       "fresh_completion_observed": completed},
            "session": {"cookies": cookies(module.SessionManager.get_session())}}


def dispatch(operation, payload, entry, metadata, events, redactor):
    module = load(entry, events, redactor)
    if operation == "health":
        return {"status": "ok", "source": metadata.__dict__, "python": sys.version.split()[0],
                "operations": ["health", "authenticate", "courses", "tasks", "questions", "run"]}
    if operation == "authenticate": return authenticate(module, payload, events, redactor)
    if operation == "courses": return courses(module, payload, events, redactor)
    if operation == "tasks": return inventory(module, payload, events, redactor)
    if operation == "questions": return questions(module, payload, events, redactor)
    if operation == "run": return run_task(module, payload, events, redactor)
    raise WorkerFailure("operation_unsupported", operation)


if __name__ == "__main__":
    raise SystemExit(run(PROTOCOL, dispatch))
