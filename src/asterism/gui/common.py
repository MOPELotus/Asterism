from __future__ import annotations

import re
from typing import Any

from PyQt6.QtCore import QThread, pyqtSignal
from PyQt6.QtWidgets import QDialog, QMessageBox, QWidget

from .fluent import (
    FLUENT_AVAILABLE,
    BodyLabel,
    FluentDialogBase,
    LineEdit,
    TitleLabel,
)


class CallThread(QThread):
    succeeded = pyqtSignal(object)
    failed = pyqtSignal(object)
    event = pyqtSignal(object)

    def __init__(self, callback):
        super().__init__()
        self.callback = callback

    def run(self) -> None:
        try:
            self.succeeded.emit(self.callback(self.event.emit))
        except Exception as error:  # pragma: no cover - UI error path
            # Preserve structured error codes across the thread boundary so
            # the UI can present a stable localized category.  Rendering and
            # redaction still happen on the GUI thread.
            self.failed.emit(error)


def make_title(text: str) -> TitleLabel:
    label = TitleLabel(text)
    label.setMinimumHeight(34)
    label.setSizePolicy(label.sizePolicy().horizontalPolicy(), label.sizePolicy().verticalPolicy())
    return label


def display_provider(provider: str) -> str:
    """Human-facing provider label; internal IDs remain lowercase and stable."""
    return provider[:1].upper() + provider[1:] if provider else provider


STATE_LABELS = {
    "active": "进行中",
    "available": "可执行",
    "completed": "已完成",
    "complete": "已完成",
    "cancelled": "已取消",
    "canceled": "已取消",
    "done": "已完成",
    "expired": "已过期",
    "failed": "失败",
    "in_progress": "进行中",
    "locked": "未开放",
    "not_open": "未开放",
    "not_started": "未开始",
    "pending": "待处理",
    "paused": "已暂停",
    "running": "运行中",
    "scanning": "扫描中",
    "skipped": "已跳过",
    "submitted": "已提交",
    "unknown": "未知",
    "waiting": "等待中",
    "waiting_user": "等待操作",
}

TASK_TYPE_LABELS = {
    "chapter": "章节",
    "course_duration": "课程时长",
    "course_exam": "考试",
    "course_homework": "作业",
    "discussion": "讨论",
    "document": "文档",
    "exam": "考试",
    "homework": "作业",
    "knowledge_point": "知识点",
    "live": "直播",
    "practice": "练习",
    "reading": "阅读",
    "resource": "资源",
    "sign": "签到",
    "talk": "口语",
    "video": "视频",
    "work": "作业",
}

QUESTION_KIND_LABELS = {
    "composite": "复合题",
    "fill_blank": "填空题",
    "matching": "连线题",
    "multiple_choice": "多选题",
    "ordering": "排序题",
    "provider_native": "平台原生题型",
    "short_answer": "简答题",
    "single_choice": "单选题",
    "true_false": "判断题",
}

DRAFT_STATUS_LABELS = {
    "discarded": "已丢弃",
    "draft": "待确认",
    "failed": "提交失败",
    "saved": "已保存到平台",
    "submitted": "已提交",
}

GRADE_COMPONENT_LABELS = {
    "audio": "音频",
    "discussion": "讨论",
    "document": "文档",
    "exam": "考试",
    "homework": "作业",
    "live": "直播",
    "reading": "阅读",
    "video": "视频",
}

ERROR_CODE_LABELS = {
    "answer_invalid": "答案格式不符合平台要求",
    "answer_required": "缺少必要答案",
    "authentication_failed": "平台认证失败",
    "auxiliary_upstream_invalid": "辅助组件版本无效",
    "auxiliary_upstream_required": "缺少必要的辅助组件",
    "browser_execution_failed": "浏览器执行失败",
    "browser_required": "此操作需要浏览器",
    "browser_shape_mismatch": "平台页面结构已变化",
    "browser_timeout": "浏览器操作超时",
    "browser_unavailable": "浏览器不可用",
    "course_select_failed": "课程选择失败",
    "course_shape_mismatch": "课程数据结构已变化",
    "courses_failed": "课程读取失败",
    "cancelled": "操作已取消",
    "dependency_missing": "缺少运行依赖",
    "dependency_shape_mismatch": "运行依赖版本不兼容",
    "duration_read_failed": "时长读取失败",
    "execution_failed": "任务执行失败",
    "execution_skipped": "任务未执行",
    "human_interaction_required": "需要平台侧人工操作",
    "local_error": "本地执行失败",
    "network": "网络请求失败",
    "operation_failed": "操作失败",
    "operation_unsupported": "当前操作不受支持",
    "protocol_mismatch": "平台协议响应已变化",
    "protocol_invalid": "执行组件响应无效",
    "question_shape_mismatch": "题目结构已变化",
    "question_type_unsupported": "题型暂不支持提交",
    "request_invalid": "操作参数无效",
    "request_missing": "缺少操作参数",
    "request_too_large": "操作数据过大",
    "session_invalid": "平台会话已失效",
    "task_expired": "任务已过期",
    "task_inventory_failed": "任务清单读取失败",
    "task_inventory_unbounded": "任务清单范围异常",
    "task_not_found": "任务不存在",
    "task_not_open": "任务尚未开放",
    "task_stale": "任务状态已经变化",
    "task_unsupported": "当前任务不受支持",
    "upstream_integrity_mismatch": "外部组件完整性校验失败",
    "upstream_load_failed": "外部组件加载失败",
    "upstream_shape_mismatch": "外部组件接口已变化",
    "upstream_unavailable": "外部组件不可用",
    "verification_budget_exhausted": "验证重试次数已用尽",
    "verification_unavailable": "平台验证服务暂不可用",
    "worker_error": "执行组件返回错误",
    "worker_exit_code": "执行组件异常退出",
    "worker_exited": "执行组件意外退出",
    "worker_unavailable": "执行组件不可用",
    "timeout": "操作超时",
}


def display_code(value: Any, labels: dict[str, str], empty: str = "") -> str:
    text = str(value or "").strip()
    return labels.get(text.casefold(), text or empty)


def display_scan_phase(value: Any) -> str:
    phase = str(value or "").strip()
    prefix = phase.split(":", 1)[0].casefold()
    return {
        "accounts": "账号队列",
        "courses": "读取课程",
        "tasks": "读取任务",
        "questions": "扫描题目",
        "completed": "扫描完成",
        "retry": "等待重试",
    }.get(prefix, display_code(phase, STATE_LABELS, "尚未开始"))


def show_notice(parent: QWidget | None, title: str, message: str, level: str = "info") -> None:
    if FLUENT_AVAILABLE:
        dialog = FluentDialogBase(title, parent, show_cancel=False)
        label = BodyLabel(message)
        label.setWordWrap(True)
        dialog.content_layout.addWidget(label)
        dialog.set_content_size(520, 210)
        dialog.exec()
        return
    handler = {
        "warning": QMessageBox.warning,
        "error": QMessageBox.critical,
    }.get(level, QMessageBox.information)
    handler(parent, title, message)


def ask_confirmation(parent: QWidget, title: str, message: str) -> bool:
    if FLUENT_AVAILABLE:
        dialog = FluentDialogBase(title, parent)
        label = BodyLabel(message)
        label.setWordWrap(True)
        dialog.content_layout.addWidget(label)
        dialog.set_content_size(520, 230)
        return dialog.exec() == QDialog.DialogCode.Accepted
    return QMessageBox.question(parent, title, message) == QMessageBox.StandardButton.Yes


def ask_text(parent: QWidget, title: str, prompt: str, default: str = "") -> tuple[str, bool]:
    dialog = FluentDialogBase(title, parent)
    dialog.content_layout.addWidget(BodyLabel(prompt))
    editor = LineEdit()
    editor.setText(default)
    dialog.content_layout.addWidget(editor)
    dialog.set_validator(lambda: bool(editor.text().strip()))
    dialog.set_content_size(460, 150)
    accepted = dialog.exec() == QDialog.DialogCode.Accepted
    return editor.text(), accepted


def redact_text(value: str) -> str:
    """Remove credential-shaped values from untrusted Worker messages and errors."""
    text = str(value or "")
    patterns = (
        (r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+", r"\1<redacted>"),
        (
            r"(?i)(password|passwd|token|cookie|secret|authorization|api[_-]?key)"
            r"(\s*[:=]\s*)[^\s,;}&]+",
            r"\1\2<redacted>",
        ),
        (
            r"(?i)([?&](?:access_token|token|code|state|ticket|key)=)[^&#\s]+",
            r"\1<redacted>",
        ),
    )
    for pattern, replacement in patterns:
        text = re.sub(pattern, replacement, text)
    return text
