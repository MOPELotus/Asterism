from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import Any

from PyQt6.QtCore import Qt, QThread, pyqtSignal
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QFormLayout,
    QGridLayout,
    QHBoxLayout,
    QInputDialog,
    QListWidget,
    QListWidgetItem,
    QMainWindow,
    QMessageBox,
    QStackedWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from ..constants import PROVIDER_IDS
from ..profiles import Profile
from .controller import DesktopController
from .fluent import (
    BodyLabel,
    ComboBox,
    LineEdit,
    PrimaryPushButton,
    PushButton,
    SubtitleLabel,
    TableWidget,
    TextEdit,
    ThemeMode,
    apply_theme,
    configure_table,
)


class CallThread(QThread):
    succeeded = pyqtSignal(object)
    failed = pyqtSignal(str)

    def __init__(self, callback):
        super().__init__()
        self.callback = callback

    def run(self) -> None:
        try:
            self.succeeded.emit(self.callback())
        except Exception as error:  # pragma: no cover - UI error path
            self.failed.emit(str(error))


def clear_layout(layout) -> None:
    while layout.count():
        item = layout.takeAt(0)
        if item.widget() is not None:
            item.widget().deleteLater()
        if item.layout() is not None:
            clear_layout(item.layout())


class HomePage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        layout = QVBoxLayout(self)
        layout.addWidget(SubtitleLabel("Asterism"))
        layout.addWidget(BodyLabel("本地桌面控制面：选择 Profile，读取课程，执行任务并管理题库。"))
        self.summary = BodyLabel()
        layout.addWidget(self.summary)
        self.refresh = PrimaryPushButton("刷新本地状态")
        self.refresh.clicked.connect(self.update_summary)
        layout.addWidget(self.refresh)
        layout.addStretch(1)
        self.update_summary()

    def update_summary(self) -> None:
        counts = {
            provider: len(self.controller.profiles.list(provider)) for provider in PROVIDER_IDS
        }
        self.summary.setText(
            "Profile："
            + "，".join(f"{provider} {counts[provider]}" for provider in PROVIDER_IDS)
            + f"\n数据目录：{self.controller.paths.root}"
        )


class ProviderPage(QWidget):
    def __init__(self, controller: DesktopController, provider: str):
        super().__init__()
        self.controller = controller
        self.provider = provider
        self.current_profile: Profile | None = None
        self.current_courses: list[dict[str, Any]] = []
        self.current_tasks: list[dict[str, Any]] = []
        self.current_routine_tasks: list[dict[str, Any]] = []
        self.current_formal_tasks: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
        self.cancel_event: threading.Event | None = None
        root = QVBoxLayout(self)
        header = QHBoxLayout()
        header.addWidget(SubtitleLabel(provider))
        self.profile_combo = ComboBox()
        header.addWidget(self.profile_combo, 1)
        self.new_profile = PushButton("新建 Profile")
        self.new_profile.clicked.connect(self.create_profile)
        header.addWidget(self.new_profile)
        self.edit_profile_button = PushButton("编辑 Profile")
        self.edit_profile_button.clicked.connect(self.edit_profile)
        header.addWidget(self.edit_profile_button)
        self.delete_profile_button = PushButton("删除 Profile")
        self.delete_profile_button.clicked.connect(self.delete_profile)
        header.addWidget(self.delete_profile_button)
        root.addLayout(header)

        actions = QGridLayout()
        action_items = [
            ("health", self.health),
            ("authenticate", self.authenticate),
            ("courses", self.sync_courses),
            ("tasks", self.sync_tasks),
            ("questions", self.scan_questions),
            ("run", self.run_selected),
            ("batch run", self.run_batch),
        ]
        if provider == "chaoxing":
            action_items.extend([("scan all", self.scan_all), ("scan status", self.scan_status)])
        if provider == "uai":
            action_items.extend([("inspect", self.inspect_task), ("duration", self.read_duration)])
        elif provider == "welearn":
            action_items.append(("duration", self.read_duration))
        if provider == "cidaren":
            action_items.extend(
                [("oauth begin", self.oauth_begin), ("oauth exchange", self.oauth_exchange)]
            )
        for index, (text, callback) in enumerate(action_items):
            button = PushButton(text)
            button.clicked.connect(callback)
            actions.addWidget(button, index // 5, index % 5)
        self.cancel_button = PushButton("cancel")
        self.cancel_button.setEnabled(False)
        self.cancel_button.clicked.connect(self.cancel_current)
        cancel_index = len(action_items)
        actions.addWidget(self.cancel_button, cancel_index // 5, cancel_index % 5)
        root.addLayout(actions)

        self.course_table = TableWidget()
        self.course_table.setColumnCount(4)
        self.course_table.setHorizontalHeaderLabels(["remote_id", "title", "state", "grade"])
        self.course_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        configure_table(self.course_table)
        self.course_table.itemSelectionChanged.connect(self._course_selected)
        root.addWidget(BodyLabel("courses"))
        root.addWidget(self.course_table)
        self.task_table = TableWidget()
        self.task_table.setColumnCount(4)
        self.task_table.setHorizontalHeaderLabels(["remote_id", "title", "type", "state"])
        self.task_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.task_table.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.task_table.itemSelectionChanged.connect(
            lambda: self.formal_table.clearSelection() if self.task_table.selectedItems() else None
        )
        configure_table(self.task_table)
        root.addWidget(BodyLabel("tasks"))
        root.addWidget(self.task_table)
        self.formal_table = TableWidget()
        self.formal_table.setColumnCount(4)
        self.formal_table.setHorizontalHeaderLabels(["remote_id", "title", "type", "state"])
        self.formal_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.formal_table.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.formal_table.itemSelectionChanged.connect(
            lambda: self.task_table.clearSelection() if self.formal_table.selectedItems() else None
        )
        configure_table(self.formal_table)
        root.addWidget(BodyLabel("formal work / exam"))
        root.addWidget(self.formal_table)
        self.log = TextEdit()
        self.log.setReadOnly(True)
        root.addWidget(BodyLabel("events / result"))
        root.addWidget(self.log)
        self.reload_profiles()

    def reload_profiles(self) -> None:
        self.profile_combo.clear()
        self.profile_combo.addItem("选择 Profile", None)
        for profile in self.controller.profiles.list(self.provider):
            self.profile_combo.addItem(f"{profile.label} [{profile.id[:8]}]", profile.id)

    def profile(self) -> Profile | None:
        profile_id = self.profile_combo.currentData()
        if not profile_id:
            QMessageBox.warning(self, self.provider, "请先选择 Profile")
            return None
        try:
            return self.controller.profiles.get(self.provider, str(profile_id))
        except (OSError, ValueError) as error:
            QMessageBox.critical(self, self.provider, str(error))
            return None

    def create_profile(self) -> None:
        dialog = QWidget(self, flags=Qt.WindowType.Dialog)
        dialog.setWindowTitle(f"{self.provider} Profile")
        form = QFormLayout(dialog)
        label = LineEdit()
        label.setPlaceholderText("显示名称")
        username = LineEdit()
        password = LineEdit()
        password.setEchoMode(LineEdit.EchoMode.Password)
        form.addRow("label", label)
        form.addRow("username", username)
        form.addRow("password", password)
        save = PrimaryPushButton("保存")
        form.addRow(save)
        save.clicked.connect(
            lambda: self._save_new_profile(dialog, label.text(), username.text(), password.text())
        )
        dialog.resize(420, 180)
        dialog.show()

    def _save_new_profile(self, dialog, label: str, username: str, password: str) -> None:
        try:
            self.controller.save_profile(
                self.provider,
                label,
                {"username": username, "password": password} if username or password else {},
            )
            dialog.close()
            self.reload_profiles()
        except (OSError, ValueError) as error:
            QMessageBox.critical(self, self.provider, str(error))

    def edit_profile(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        dialog = QWidget(self, flags=Qt.WindowType.Dialog)
        dialog.setWindowTitle(f"{self.provider} Profile")
        form = QFormLayout(dialog)
        label = LineEdit(profile.label)
        username = LineEdit(str(profile.credentials.get("username") or ""))
        password = LineEdit()
        password.setEchoMode(LineEdit.EchoMode.Password)
        password.setPlaceholderText("留空则保持现有密码")
        form.addRow("label", label)
        form.addRow("username", username)
        form.addRow("password", password)
        save = PrimaryPushButton("保存")
        form.addRow(save)
        save.clicked.connect(
            lambda: self._save_profile_edit(
                dialog, profile, label.text(), username.text(), password.text()
            )
        )
        dialog.resize(460, 190)
        dialog.show()

    def _save_profile_edit(
        self, dialog, profile: Profile, label: str, username: str, password: str
    ) -> None:
        try:
            if not label.strip():
                raise ValueError("Profile 名称不能为空")
            credentials = dict(profile.credentials)
            if username.strip() or "username" in credentials:
                credentials["username"] = username.strip()
            if password:
                credentials["password"] = password
            self.controller.save_profile(
                profile.provider,
                label,
                credentials,
                settings=profile.settings,
                profile_id=profile.id,
            )
            dialog.close()
            self.reload_profiles()
        except (OSError, ValueError) as error:
            QMessageBox.critical(self, self.provider, str(error))

    def delete_profile(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        if (
            QMessageBox.question(
                self,
                self.provider,
                "删除本地 Profile 及其会话状态？日志和草稿会保留。",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        try:
            self.controller.delete_profile(profile)
            self.reload_profiles()
            self.log.append(f"[profile] deleted {profile.id}")
        except (OSError, ValueError) as error:
            QMessageBox.critical(self, self.provider, str(error))

    def _call(self, callback, label: str) -> None:
        if self.worker_thread is not None and self.worker_thread.isRunning():
            QMessageBox.information(self, self.provider, "已有操作正在运行")
            return
        self.log.append(f"[{label}] starting")
        self.cancel_event = threading.Event()
        self.worker_thread = CallThread(callback)
        self.cancel_button.setEnabled(True)
        self.worker_thread.succeeded.connect(lambda result: self._success(label, result))
        self.worker_thread.failed.connect(lambda error: self._failure(label, error))
        self.worker_thread.start()

    def _success(self, label: str, result: Any) -> None:
        if label == "courses":
            self.current_courses = result if isinstance(result, list) else []
            self.course_table.setRowCount(len(self.current_courses))
            for row, course in enumerate(self.current_courses):
                grade = course.get("provider_summary", {}).get("grade", {})
                grade_text = grade.get("overall_score", "") if isinstance(grade, dict) else ""
                values = (
                    course.get("remote_id", ""),
                    course.get("title", ""),
                    course.get("state", ""),
                    grade_text,
                )
                for column, value in enumerate(values):
                    self.course_table.setItem(
                        row, column, QTableWidgetItem(str(value if value is not None else ""))
                    )
        elif label == "tasks":
            self.current_tasks = result if isinstance(result, list) else []
            self.current_formal_tasks = [
                task for task in self.current_tasks if self._is_formal(task)
            ]
            self.current_routine_tasks = [
                task for task in self.current_tasks if not self._is_formal(task)
            ]
            self.task_table.setRowCount(len(self.current_routine_tasks))
            for row, task in enumerate(self.current_routine_tasks):
                for column, key in enumerate(("remote_id", "title", "type", "state")):
                    value = task.get(key, task.get("task_type", task.get("source_type", "")))
                    self.task_table.setItem(row, column, QTableWidgetItem(str(value)))
            self.formal_table.setRowCount(len(self.current_formal_tasks))
            for row, task in enumerate(self.current_formal_tasks):
                for column, key in enumerate(("remote_id", "title", "type", "state")):
                    value = task.get(key, task.get("source_type", ""))
                    self.formal_table.setItem(row, column, QTableWidgetItem(str(value)))
        data = result.data if hasattr(result, "data") else result
        preview = json.dumps(self._safe_preview(data), ensure_ascii=False, default=str)
        self.log.append(f"[{label}] {preview}")
        self.controller.notify(
            "success",
            provider=self.provider,
            operation=label,
            summary={"status": "success"},
        )
        self.cancel_button.setEnabled(False)
        self.cancel_event = None

    def _failure(self, label: str, error: str) -> None:
        self.log.append(f"[{label}] ERROR {error}")
        self.controller.notify(
            "failure",
            provider=self.provider,
            operation=label,
            summary={"status": "failure", "error_code": error[:120]},
        )
        self.cancel_button.setEnabled(False)
        self.cancel_event = None

    @staticmethod
    def _safe_preview(value: Any, *, depth: int = 0) -> Any:
        """Keep the UI useful without echoing credentials or huge payloads."""
        if depth > 4:
            return "<truncated>"
        if isinstance(value, dict):
            result = {}
            for key, child in value.items():
                name = str(key)
                lowered = name.casefold()
                if lowered == "authorization_url":
                    result[name] = str(child)
                elif any(
                    marker in lowered
                    for marker in (
                        "password",
                        "token",
                        "cookie",
                        "secret",
                        "authorization",
                        "session",
                    )
                ):
                    result[name] = "<redacted>"
                else:
                    result[name] = ProviderPage._safe_preview(child, depth=depth + 1)
            return result
        if isinstance(value, (list, tuple)):
            return {
                "count": len(value),
                "items": [ProviderPage._safe_preview(item, depth=depth + 1) for item in value[:8]],
            }
        if hasattr(value, "to_dict") and callable(value.to_dict):
            return ProviderPage._safe_preview(value.to_dict(), depth=depth + 1)
        return value

    @staticmethod
    def _is_formal(task: dict[str, Any]) -> bool:
        route = task.get("native", {}).get("route_kind")
        return task.get("assessment_class") == "formal" or route in {
            "course_exam",
            "course_homework",
        }

    def cancel_current(self) -> None:
        if self.cancel_event is not None:
            self.cancel_event.set()
            self.log.append("[cancel] requested")

    def health(self) -> None:
        self._call(lambda: self.controller.health(self.provider), "health")

    def authenticate(self) -> None:
        profile = self.profile()
        if profile:
            self._call(lambda: self.controller.service.authenticate(profile), "authenticate")

    def sync_courses(self) -> None:
        profile = self.profile()
        if profile:
            self._call(
                lambda: self.controller.sync_courses(profile, cancel=self.cancel_event), "courses"
            )

    def _course_selected(self) -> None:
        row = self.course_table.currentRow()
        if 0 <= row < len(self.current_courses):
            self.current_tasks = []
            self.current_routine_tasks = []
            self.current_formal_tasks = []
            self.task_table.setRowCount(0)
            self.formal_table.setRowCount(0)

    def _selected_task(self) -> dict[str, Any] | None:
        formal_row = self.formal_table.currentRow()
        if 0 <= formal_row < len(self.current_formal_tasks):
            return self.current_formal_tasks[formal_row]
        routine_row = self.task_table.currentRow()
        if 0 <= routine_row < len(self.current_routine_tasks):
            return self.current_routine_tasks[routine_row]
        return None

    def sync_tasks(self) -> None:
        profile = self.profile()
        row = self.course_table.currentRow()
        if not profile or row < 0 or row >= len(self.current_courses):
            QMessageBox.warning(self, self.provider, "请先同步并选择 course")
            return
        course = self.current_courses[row]
        self._call(
            lambda: self.controller.sync_tasks(profile, course, cancel=self.cancel_event), "tasks"
        )

    def scan_questions(self) -> None:
        profile = self.profile()
        task = self._selected_task()
        if not profile or task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        allow_attempt = (
            self.provider == "cidaren"
            and QMessageBox.question(
                self,
                "cidaren",
                "该读取路径可能建立远端答题尝试，是否明确授权？",
            )
            == QMessageBox.StandardButton.Yes
        )
        self._call(
            lambda: self.controller.scan_questions(
                profile,
                task,
                allow_read_that_starts_attempt=allow_attempt,
                cancel=self.cancel_event,
            ),
            "questions",
        )

    def scan_all(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        allow_attempt = (
            profile.provider == "cidaren"
            and QMessageBox.question(
                self,
                "cidaren",
                "全量读取 class task 可能建立远端答题尝试，是否明确授权？",
            )
            == QMessageBox.StandardButton.Yes
        )
        self._call(
            lambda: self.controller.scan_all(
                profile,
                allow_cidaren_attempt=allow_attempt,
                cancel=self.cancel_event,
            ),
            "scan all",
        )

    def scan_status(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        status = self.controller.scan_status(profile)
        preview = json.dumps(self._safe_preview(status), ensure_ascii=False)
        self.log.append(f"[scan status] {preview}")

    def run_selected(self) -> None:
        profile = self.profile()
        task = self._selected_task()
        if not profile or task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        if self._is_formal(task):
            if (
                QMessageBox.question(self, self.provider, "仅保存草稿，不进行最终提交？")
                != QMessageBox.StandardButton.Yes
            ):
                return
            draft = self.controller.save_draft(
                profile, str(task.get("remote_id")), {"task": task, "answers": {}}
            )
            self.log.append(f"[draft] {draft.id}")
            return
        self._call(lambda: self.controller.run_task(profile, task, cancel=self.cancel_event), "run")

    def run_batch(self) -> None:
        profile = self.profile()
        routine_rows = sorted(
            {index.row() for index in self.task_table.selectionModel().selectedRows()}
        )
        formal_rows = sorted(
            {index.row() for index in self.formal_table.selectionModel().selectedRows()}
        )
        if not profile or not (routine_rows or formal_rows):
            QMessageBox.warning(self, self.provider, "请先选择一个或多个 task")
            return
        routine = [
            self.current_routine_tasks[row]
            for row in routine_rows
            if 0 <= row < len(self.current_routine_tasks)
        ]
        formal = [
            self.current_formal_tasks[row]
            for row in formal_rows
            if 0 <= row < len(self.current_formal_tasks)
        ]
        if (
            formal
            and QMessageBox.question(
                self,
                self.provider,
                f"选中的 {len(formal)} 个正式任务只保存草稿，不进行最终提交？",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        for task in formal:
            draft = self.controller.save_draft(
                profile, str(task.get("remote_id")), {"task": task, "answers": {}}
            )
            self.log.append(f"[draft] {draft.id}")
        if not routine:
            return
        concurrency, accepted = QInputDialog.getInt(self, self.provider, "并发数", 1, 1, 256)
        if not accepted:
            return
        self._call(
            lambda: self.controller.run_batch(
                profile, routine, concurrency=concurrency, cancel=self.cancel_event
            ),
            "batch run",
        )

    def read_duration(self) -> None:
        profile = self.profile()
        task = self._selected_task()
        if not profile or task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        self._call(
            lambda: self.controller.read_duration(profile, task, cancel=self.cancel_event),
            "duration",
        )

    def inspect_task(self) -> None:
        profile = self.profile()
        task = self._selected_task()
        if not profile or task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        self._call(
            lambda: self.controller.inspect_task(profile, task, cancel=self.cancel_event),
            "inspect",
        )

    def oauth_begin(self) -> None:
        profile = self.profile()
        if profile:
            self._call(lambda: self.controller.service.oauth_begin(profile), "oauth begin")

    def oauth_exchange(self) -> None:
        profile = self.profile()
        if not profile:
            return
        callback_url, accepted = QInputDialog.getText(self, "cidaren", "粘贴微信确认后的回调链接")
        if accepted and callback_url.strip():
            self._call(
                lambda: self.controller.service.oauth_exchange(profile, callback_url.strip()),
                "oauth exchange",
            )


class DraftPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        self.current_rows: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
        root = QVBoxLayout(self)
        root.addWidget(SubtitleLabel("drafts"))
        self.table = TableWidget()
        self.table.setColumnCount(5)
        self.table.setHorizontalHeaderLabels(
            ["provider", "profile_id", "task_ref", "status", "updated_at"]
        )
        configure_table(self.table)
        root.addWidget(self.table)
        actions = QHBoxLayout()
        refresh = PushButton("刷新")
        refresh.clicked.connect(self.reload)
        actions.addWidget(refresh)
        edit = PushButton("编辑草稿")
        edit.clicked.connect(self.edit_selected)
        actions.addWidget(edit)
        submit = PrimaryPushButton("确认并提交")
        submit.clicked.connect(self.submit_selected)
        actions.addWidget(submit)
        discard = PushButton("丢弃")
        discard.clicked.connect(self.discard_selected)
        actions.addWidget(discard)
        root.addLayout(actions)
        self.log = TextEdit()
        self.log.setReadOnly(True)
        root.addWidget(self.log)
        self.reload()

    def reload(self) -> None:
        self.current_rows = self.controller.draft_rows()
        self.table.setRowCount(len(self.current_rows))
        for row, value in enumerate(self.current_rows):
            for column, key in enumerate(
                ("provider", "profile_id", "task_ref", "status", "updated_at")
            ):
                self.table.setItem(row, column, QTableWidgetItem(str(value.get(key) or "")))

    def _selected(self):
        row = self.table.currentRow()
        if row < 0 or row >= len(self.current_rows):
            QMessageBox.warning(self, "drafts", "请先选择草稿")
            return None
        value = self.current_rows[row]
        try:
            return self.controller.load_draft(
                str(value["provider"]), str(value["profile_id"]), str(value["id"])
            )
        except (OSError, TypeError, ValueError, KeyError) as error:
            QMessageBox.critical(self, "drafts", str(error))
            return None

    def edit_selected(self) -> None:
        draft = self._selected()
        if draft is None:
            return
        dialog = QWidget(self, flags=Qt.WindowType.Dialog)
        dialog.setWindowTitle(f"draft {draft.id[:8]}")
        layout = QVBoxLayout(dialog)
        editor = TextEdit()
        editor.setPlainText(json.dumps(draft.payload, ensure_ascii=False, indent=2))
        layout.addWidget(editor)
        save = PrimaryPushButton("保存草稿")
        layout.addWidget(save)

        def save_payload() -> None:
            try:
                payload = json.loads(editor.toPlainText())
                if not isinstance(payload, dict):
                    raise ValueError("草稿内容必须是 JSON object")
                self.controller.update_draft(draft, payload)
                self.log.append(f"[draft] saved {draft.id}")
                dialog.close()
                self.reload()
            except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
                QMessageBox.critical(dialog, "drafts", str(error))

        save.clicked.connect(save_payload)
        dialog.resize(760, 560)
        dialog.show()

    def submit_selected(self) -> None:
        draft = self._selected()
        if draft is None:
            return
        if draft.status != "draft":
            QMessageBox.information(self, "drafts", "只有 draft 状态可以提交")
            return
        if (
            QMessageBox.question(
                self,
                "drafts",
                "将使用当前草稿答案调用平台原生提交，确认继续？",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        self.log.append(f"[draft] submitting {draft.id}")
        self.worker_thread = CallThread(lambda: self.controller.submit_draft(draft))
        self.worker_thread.succeeded.connect(self._submit_succeeded)
        self.worker_thread.failed.connect(lambda error: self.log.append(f"[draft] ERROR {error}"))
        self.worker_thread.start()

    def _submit_succeeded(self, _result: Any) -> None:
        self.log.append("[draft] submitted")
        self.reload()

    def discard_selected(self) -> None:
        draft = self._selected()
        if draft is None:
            return
        if (
            QMessageBox.question(self, "drafts", "确认丢弃这份草稿？")
            == QMessageBox.StandardButton.Yes
        ):
            self.controller.drafts.set_status(draft, "discarded")
            self.reload()


class QuestionBankPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        self.rows: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
        root = QVBoxLayout(self)
        root.addWidget(SubtitleLabel("question bank"))
        self.summary = BodyLabel()
        root.addWidget(self.summary)
        controls = QHBoxLayout()
        refresh = PushButton("刷新")
        refresh.clicked.connect(self.reload)
        controls.addWidget(refresh)
        controls.addWidget(BodyLabel("组合"))
        self.combination = ComboBox()
        self.combination.addItem("economy", "economy")
        self.combination.addItem("gpt_only", "gpt_only")
        controls.addWidget(self.combination)
        controls.addWidget(BodyLabel("route"))
        self.route = ComboBox()
        self.route.addItem("untimed", "untimed")
        self.route.addItem("timed", "timed")
        controls.addWidget(self.route)
        answer = PrimaryPushButton("AI 解题并缓存")
        answer.clicked.connect(self.answer_selected)
        controls.addWidget(answer)
        root.addLayout(controls)
        self.table = TableWidget()
        self.table.setColumnCount(4)
        self.table.setHorizontalHeaderLabels(["provider", "kind", "prompt", "identity"])
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        configure_table(self.table)
        root.addWidget(self.table, 1)
        self.log = TextEdit()
        self.log.setReadOnly(True)
        root.addWidget(self.log)
        self.reload()

    def reload(self) -> None:
        self.rows = self.controller.list_questions()
        self.table.setRowCount(len(self.rows))
        for row, value in enumerate(self.rows):
            content = value.get("content", {})
            prompt = content.get("prompt", "") if isinstance(content, dict) else ""
            for column, item in enumerate(
                (
                    value.get("provider"),
                    value.get("native_kind"),
                    prompt,
                    value.get("identity_hash"),
                )
            ):
                self.table.setItem(row, column, QTableWidgetItem(str(item or "")))
        with self.controller.bank.connect() as connection:
            questions = connection.execute("SELECT COUNT(*) FROM questions").fetchone()[0]
            candidates = connection.execute("SELECT COUNT(*) FROM answer_candidates").fetchone()[0]
            observations = connection.execute(
                "SELECT COUNT(*) FROM answer_observations"
            ).fetchone()[0]
        self.summary.setText(
            f"questions: {questions}\ncandidates: {candidates}\nobservations: {observations}"
        )

    def answer_selected(self) -> None:
        row = self.table.currentRow()
        if row < 0 or row >= len(self.rows):
            QMessageBox.warning(self, "question bank", "请先选择题目")
            return
        value = self.rows[row]
        provider = str(value.get("provider") or "")
        question = value.get("content")
        if not provider or not isinstance(question, dict):
            QMessageBox.warning(self, "question bank", "题目内容不完整")
            return
        if self.worker_thread is not None and self.worker_thread.isRunning():
            QMessageBox.information(self, "question bank", "已有 AI 请求正在运行")
            return
        combination = str(self.combination.currentData() or "economy")
        route = str(self.route.currentData() or "untimed")
        self.log.append(f"[ai] {provider} {combination}/{route} starting")
        self.worker_thread = CallThread(
            lambda: self.controller.answer_question(
                provider, question, combination=combination, route=route
            )
        )
        self.worker_thread.succeeded.connect(
            lambda result: self.log.append(
                f"[ai] {json.dumps(ProviderPage._safe_preview(result), ensure_ascii=False)}"
            )
        )
        self.worker_thread.failed.connect(lambda error: self.log.append(f"[ai] ERROR {error}"))
        self.worker_thread.start()


class SettingsPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        root = QVBoxLayout(self)
        root.addWidget(SubtitleLabel("settings"))
        root.addWidget(BodyLabel("模型组合、通知和 Provider 默认值保存在 config.local.json。"))
        self.theme = ComboBox()
        self.theme.addItem("跟随系统", ThemeMode.SYSTEM.value)
        self.theme.addItem("浅色", ThemeMode.LIGHT.value)
        self.theme.addItem("深色", ThemeMode.DARK.value)
        current_theme = str(
            controller.config.ensure().get("ui", {}).get("theme", ThemeMode.SYSTEM.value)
        )
        self.theme.setCurrentIndex(max(0, self.theme.findData(current_theme)))
        self.theme.currentIndexChanged.connect(self.apply_selected_theme)
        root.addWidget(self.theme)
        self.editor = TextEdit()
        self.editor.setPlainText(
            json.dumps(controller.config.ensure(), ensure_ascii=False, indent=2)
        )
        root.addWidget(self.editor)
        save = PrimaryPushButton("保存配置")
        save.clicked.connect(self.save)
        root.addWidget(save)

    def save(self) -> None:
        try:
            value = json.loads(self.editor.toPlainText())
            self.controller.config.save(value)
        except (ValueError, OSError, TypeError) as error:
            QMessageBox.critical(self, "settings", str(error))

    def apply_selected_theme(self) -> None:
        try:
            value = json.loads(self.editor.toPlainText())
            value.setdefault("ui", {})["theme"] = self.theme.currentData()
            self.editor.setPlainText(json.dumps(value, ensure_ascii=False, indent=2))
            app = QGuiApplication.instance()
            if app is not None:
                apply_theme(app, str(self.theme.currentData()))
        except (TypeError, ValueError, json.JSONDecodeError) as error:
            self.log_theme_error(str(error))

    def log_theme_error(self, message: str) -> None:
        # Keep theme selection non-destructive if the JSON editor is temporarily invalid.
        self.theme.setToolTip(f"配置暂不可解析：{message}")


class MainWindow(QMainWindow):
    def __init__(self, data_root: Path | None = None, source_root: Path | None = None):
        super().__init__()
        self.controller = DesktopController.create(data_root, source_root)
        apply_theme(
            QGuiApplication.instance(),
            str(self.controller.config.ensure().get("ui", {}).get("theme", ThemeMode.SYSTEM.value)),
        )
        self.setWindowTitle("Asterism")
        screen = QGuiApplication.primaryScreen()
        available = screen.availableGeometry() if screen is not None else None
        if available is not None:
            self.resize(
                min(1440, max(960, int(available.width() * 0.82))),
                min(960, max(640, int(available.height() * 0.82))),
            )
        else:
            self.resize(1280, 820)
        shell = QWidget()
        self.setCentralWidget(shell)
        layout = QHBoxLayout(shell)
        self.navigation = QListWidget()
        self.navigation.setFixedWidth(150)
        self.stack = QStackedWidget()
        layout.addWidget(self.navigation)
        layout.addWidget(self.stack, 1)
        self.pages: list[QWidget] = []
        self.add_page("home", HomePage(self.controller))
        for provider in PROVIDER_IDS:
            self.add_page(provider, ProviderPage(self.controller, provider))
        self.add_page("drafts", DraftPage(self.controller))
        self.add_page("question-bank", QuestionBankPage(self.controller))
        self.add_page("settings", SettingsPage(self.controller))
        self.navigation.currentRowChanged.connect(self.stack.setCurrentIndex)
        self.navigation.setCurrentRow(0)

    def add_page(self, name: str, page: QWidget) -> None:
        self.navigation.addItem(QListWidgetItem(name))
        self.stack.addWidget(page)
        self.pages.append(page)
