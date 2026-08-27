from __future__ import annotations

import json
import os
import threading
from contextlib import suppress
from pathlib import Path
from typing import Any

from PyQt6.QtCore import Qt, QThread, pyqtSignal
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QDialog,
    QDialogButtonBox,
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
from .draft_editor import FormalDraftEditor
from .fluent import (
    FLUENT_AVAILABLE,
    BodyLabel,
    CaptionLabel,
    CardWidget,
    CheckBox,
    ComboBox,
    FluentIcon,
    FluentWindow,
    LineEdit,
    NavigationItemPosition,
    PrimaryPushButton,
    PushButton,
    StrongBodyLabel,
    SubtitleLabel,
    TableWidget,
    TextEdit,
    ThemeMode,
    TitleLabel,
    apply_theme,
    configure_table,
)


class CallThread(QThread):
    succeeded = pyqtSignal(object)
    failed = pyqtSignal(str)
    event = pyqtSignal(object)

    def __init__(self, callback):
        super().__init__()
        self.callback = callback

    def run(self) -> None:
        try:
            self.succeeded.emit(self.callback(self.event.emit))
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
        layout.setContentsMargins(28, 24, 28, 28)
        layout.setSpacing(16)

        hero = CardWidget()
        hero_layout = QVBoxLayout(hero)
        hero_layout.setContentsMargins(24, 20, 24, 20)
        hero_layout.setSpacing(6)
        hero_layout.addWidget(TitleLabel("Asterism"))
        hero_layout.addWidget(BodyLabel("本地桌面控制台"))
        hero_layout.addWidget(
            CaptionLabel("选择平台账号，完成认证后读取课程和任务。正式提交始终需要明确确认。")
        )
        layout.addWidget(hero)

        metrics = QGridLayout()
        metrics.setHorizontalSpacing(12)
        metrics.setVerticalSpacing(12)
        self.metric_labels: dict[str, Any] = {}
        for column, (key, title, note) in enumerate(
            (
                ("profiles", "本地账号", "四个平台的 Profile 总数"),
                ("courses", "课程缓存", "当前本地已读取的课程"),
                ("tasks", "任务缓存", "当前本地已读取的任务"),
                ("bank", "题库题目", "全局题库中的题目数量"),
            )
        ):
            card = CardWidget()
            card_layout = QVBoxLayout(card)
            card_layout.setContentsMargins(16, 14, 16, 14)
            card_layout.setSpacing(3)
            card_layout.addWidget(CaptionLabel(title))
            value = TitleLabel("0")
            value.setObjectName(f"metric_{key}")
            card_layout.addWidget(value)
            card_layout.addWidget(CaptionLabel(note))
            metrics.addWidget(card, 0, column)
            self.metric_labels[key] = value
        layout.addLayout(metrics)

        overview = CardWidget()
        overview_layout = QVBoxLayout(overview)
        overview_layout.setContentsMargins(20, 16, 20, 16)
        overview_layout.setSpacing(8)
        overview_layout.addWidget(StrongBodyLabel("运行概况"))
        self.summary = BodyLabel()
        self.summary.setWordWrap(True)
        overview_layout.addWidget(self.summary)
        self.refresh = PrimaryPushButton("刷新本地状态")
        self.refresh.clicked.connect(self.update_summary)
        overview_layout.addWidget(self.refresh, 0, Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(overview)

        tips = CardWidget()
        tips_layout = QVBoxLayout(tips)
        tips_layout.setContentsMargins(20, 16, 20, 16)
        tips_layout.setSpacing(5)
        tips_layout.addWidget(StrongBodyLabel("开始使用"))
        tips_layout.addWidget(
            CaptionLabel(
                "1. 打开左侧平台页面；2. 新建账号 Profile；"
                "3. 点击认证 / 登录；4. 读取课程。"
            )
        )
        tips_layout.addWidget(
            CaptionLabel("课程、任务和题目读取不会提交平台数据；作业和考试需在草稿页人工确认后才能提交。")
        )
        layout.addWidget(tips)
        layout.addStretch(1)
        self.update_summary()

    def update_summary(self) -> None:
        counts = {
            provider: len(self.controller.profiles.list(provider)) for provider in PROVIDER_IDS
        }
        self.metric_labels["profiles"].setText(str(sum(counts.values())))
        self.summary.setText(
            "Profile："
            + "，".join(f"{provider} {counts[provider]}" for provider in PROVIDER_IDS)
            + f"\n数据目录：{self.controller.paths.root}\n"
            "从平台页完成认证后，课程和任务会显示在对应页面。"
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
        self.current_questions: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
        self.cancel_event: threading.Event | None = None
        root = QVBoxLayout(self)
        root.setContentsMargins(28, 24, 28, 28)
        root.setSpacing(14)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(20, 16, 20, 16)
        intro_layout.setSpacing(6)
        intro_layout.addWidget(TitleLabel(f"{provider} 账号"))
        intro_layout.addWidget(
            BodyLabel("先创建或选择一个本地 Profile，再点击认证。认证成功后即可读取课程和任务。")
        )
        intro_layout.addWidget(
            CaptionLabel(
                "账号凭据只保存在本机 Profile；当前页面的课程、任务和题目操作默认是只读的。"
            )
        )
        root.addWidget(intro)
        header = QHBoxLayout()
        header.addWidget(StrongBodyLabel("当前 Profile"))
        self.profile_combo = ComboBox()
        self.profile_combo.setMinimumWidth(260)
        self.profile_combo.currentIndexChanged.connect(self._profile_changed)
        header.addWidget(self.profile_combo, 1)
        self.new_profile = PrimaryPushButton("新建账号")
        self.new_profile.clicked.connect(self.create_profile)
        header.addWidget(self.new_profile)
        self.login_button = PrimaryPushButton("认证 / 登录")
        self.login_button.clicked.connect(self.authenticate)
        header.addWidget(self.login_button)
        header.addStretch(1)
        root.addLayout(header)
        profile_actions = QHBoxLayout()
        profile_actions.addWidget(CaptionLabel("Profile 管理"))
        self.edit_profile_button = PushButton("编辑 Profile")
        self.edit_profile_button.clicked.connect(self.edit_profile)
        profile_actions.addWidget(self.edit_profile_button)
        self.delete_profile_button = PushButton("删除 Profile")
        self.delete_profile_button.clicked.connect(self.delete_profile)
        profile_actions.addWidget(self.delete_profile_button)
        profile_actions.addStretch(1)
        root.addLayout(profile_actions)
        self.profile_status = CaptionLabel("尚未选择 Profile")
        root.addWidget(self.profile_status)

        actions = QGridLayout()
        action_items = [
            ("连接检查", self.health),
            ("读取课程", self.sync_courses),
            ("读取任务", self.sync_tasks),
            ("扫描题目", self.scan_questions),
            ("任务详情", self.show_task_detail),
            ("执行任务", self.run_selected),
            ("批量执行", self.run_batch),
        ]
        if provider == "chaoxing":
            action_items.extend(
                [
                    ("扫描全部", self.scan_all),
                    ("扫描账号", self.scan_profiles),
                    ("扫描状态", self.scan_status),
                ]
            )
        if provider == "uai":
            action_items.extend(
                [("读取必做项", self.inspect_task), ("读取时长", self.read_duration)]
            )
        elif provider == "welearn":
            action_items.extend(
                [("读取时长", self.read_duration), ("安装 donor", self.install_donor)]
            )
        if provider == "cidaren":
            action_items.extend(
                [("开始授权", self.oauth_begin), ("完成授权", self.oauth_exchange)]
            )
        for index, (text, callback) in enumerate(action_items):
            button = PushButton(text)
            button.clicked.connect(callback)
            actions.addWidget(button, index // 5, index % 5)
        self.cancel_button = PushButton("取消当前操作")
        self.cancel_button.setEnabled(False)
        self.cancel_button.clicked.connect(self.cancel_current)
        cancel_index = len(action_items)
        actions.addWidget(self.cancel_button, cancel_index // 5, cancel_index % 5)
        root.addLayout(actions)
        execution_options = QHBoxLayout()
        execution_options.addWidget(BodyLabel("本次答案组合"))
        self.execution_combination = ComboBox()
        self.execution_combination.addItem("economy", "economy")
        self.execution_combination.addItem("gpt_only", "gpt_only")
        execution_options.addWidget(self.execution_combination)
        execution_options.addWidget(
            BodyLabel("仅影响需要答案的执行；不需要答题的平台会沿用其原生路径。")
        )
        execution_options.addStretch(1)
        root.addLayout(execution_options)
        self.generated_text = None
        if provider == "uai":
            text_options = QVBoxLayout()
            text_options.addWidget(BodyLabel("本次讨论/主观文本（可选，发送前请自行确认）"))
            self.generated_text = TextEdit()
            self.generated_text.setPlaceholderText(
                "留空则沿用 Provider 原生行为；填写后仅作为本次执行的纯文本内容。"
            )
            self.generated_text.setFixedHeight(88)
            text_options.addWidget(self.generated_text)
            root.addLayout(text_options)

        self.course_table = TableWidget()
        self.course_table.setColumnCount(4)
        self.course_table.setHorizontalHeaderLabels(["序号", "课程", "状态", "成绩"])
        self.course_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        configure_table(self.course_table)
        self.course_table.itemSelectionChanged.connect(self._course_selected)
        root.addWidget(StrongBodyLabel("课程"))
        root.addWidget(self.course_table)
        self.task_table = TableWidget()
        self.task_table.setColumnCount(4)
        self.task_table.setHorizontalHeaderLabels(["序号", "任务", "类型", "状态"])
        self.task_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.task_table.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.task_table.itemSelectionChanged.connect(
            lambda: self.formal_table.clearSelection() if self.task_table.selectedItems() else None
        )
        configure_table(self.task_table)
        root.addWidget(StrongBodyLabel("任务"))
        root.addWidget(self.task_table)
        task_selection = QHBoxLayout()
        task_selection.addWidget(BodyLabel("批量选择"))
        select_routine = PushButton("全选普通任务")
        select_routine.clicked.connect(lambda: self._select_all_rows(self.task_table))
        task_selection.addWidget(select_routine)
        select_formal = PushButton("全选作业/考试")
        select_formal.clicked.connect(lambda: self._select_all_rows(self.formal_table))
        task_selection.addWidget(select_formal)
        clear_selection = PushButton("清除选择")
        clear_selection.clicked.connect(self._clear_task_selection)
        task_selection.addWidget(clear_selection)
        task_selection.addStretch(1)
        root.addLayout(task_selection)
        self.formal_table = TableWidget()
        self.formal_table.setColumnCount(4)
        self.formal_table.setHorizontalHeaderLabels(["序号", "作业 / 考试", "类型", "状态"])
        self.formal_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.formal_table.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.formal_table.itemSelectionChanged.connect(
            lambda: self.task_table.clearSelection() if self.formal_table.selectedItems() else None
        )
        configure_table(self.formal_table)
        root.addWidget(StrongBodyLabel("作业与考试"))
        root.addWidget(self.formal_table)
        self.question_table = TableWidget()
        self.question_table.setColumnCount(4)
        self.question_table.setHorizontalHeaderLabels(["序号", "题型", "题干", "选项"])
        self.question_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        configure_table(self.question_table)
        root.addWidget(StrongBodyLabel("题目预览（只读）"))
        root.addWidget(self.question_table)
        self.log = TextEdit()
        self.log.setReadOnly(True)
        root.addWidget(StrongBodyLabel("运行日志与结果"))
        root.addWidget(self.log)
        self.reload_profiles()

    def reload_profiles(self) -> None:
        self.profile_combo.clear()
        self.profile_combo.addItem("选择 Profile", None)
        for profile in self.controller.profiles.list(self.provider):
            state = "" if profile.enabled else " · disabled"
            self.profile_combo.addItem(
                f"{profile.label}{state}", profile.id
            )
        self._profile_changed(self.profile_combo.currentIndex())

    def _profile_changed(self, _index: int) -> None:
        profile_id = self.profile_combo.currentData()
        if not profile_id:
            self.profile_status.setText("尚未选择 Profile · 请先点击“新建账号”")
            return
        try:
            profile = self.controller.profiles.get(self.provider, str(profile_id))
            state = "已启用" if profile.enabled else "已停用"
            self.profile_status.setText(f"当前：{profile.label} · {state} · 可点击“认证 / 登录”")
        except (OSError, ValueError):
            self.profile_status.setText("当前 Profile 无法读取")

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
        dialog = QDialog(self)
        dialog.setWindowTitle(f"{self.provider} Profile")
        form = QFormLayout(dialog)
        label = LineEdit()
        label.setPlaceholderText("显示名称")
        username = LineEdit()
        password = LineEdit()
        password.setEchoMode(LineEdit.EchoMode.Password)
        credentials_json = TextEdit()
        credentials_json.setPlainText("{}")
        credentials_json.setPlaceholderText('高级凭据 JSON，例如 {"cookie":"..."} 或 token')
        settings_json = TextEdit()
        settings_json.setPlainText("{}")
        settings_json.setPlaceholderText("该 Profile 覆盖的 Provider 设置 JSON")
        enabled = CheckBox("参与全 Profile 手动扫描")
        enabled.setChecked(True)
        form.addRow("label", label)
        form.addRow("username", username)
        form.addRow("password", password)
        form.addRow("credentials", credentials_json)
        form.addRow("settings", settings_json)
        form.addRow("enabled", enabled)
        save = PrimaryPushButton("保存")
        form.addRow(save)
        save.clicked.connect(
            lambda: self._save_new_profile(
                dialog,
                label.text(),
                username.text(),
                password.text(),
                credentials_json.toPlainText(),
                settings_json.toPlainText(),
                enabled.isChecked(),
            )
        )
        dialog.resize(640, 560)
        dialog.exec()

    def _save_new_profile(
        self,
        dialog,
        label: str,
        username: str,
        password: str,
        credentials_text: str,
        settings_text: str,
        enabled: bool,
    ) -> None:
        try:
            credentials = self._json_object(credentials_text, "credentials")
            settings = self._json_object(settings_text, "settings")
            if username.strip():
                credentials["username"] = username.strip()
            if password:
                credentials["password"] = password
            self.controller.save_profile(
                self.provider,
                label,
                credentials,
                settings=settings,
                enabled=enabled,
            )
            dialog.close()
            self.reload_profiles()
        except (OSError, ValueError) as error:
            QMessageBox.critical(self, self.provider, str(error))

    def edit_profile(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        dialog = QDialog(self)
        dialog.setWindowTitle(f"{self.provider} Profile")
        form = QFormLayout(dialog)
        label = LineEdit(profile.label)
        username = LineEdit(str(profile.credentials.get("username") or ""))
        password = LineEdit()
        password.setEchoMode(LineEdit.EchoMode.Password)
        password.setPlaceholderText("留空则保持现有密码")
        advanced_credentials = {
            key: value
            for key, value in profile.credentials.items()
            if key not in {"username", "password"}
        }
        credentials_json = TextEdit()
        credentials_json.setPlainText(
            json.dumps(advanced_credentials, ensure_ascii=False, indent=2)
        )
        settings_json = TextEdit()
        settings_json.setPlainText(json.dumps(profile.settings, ensure_ascii=False, indent=2))
        enabled = CheckBox("参与全 Profile 手动扫描")
        enabled.setChecked(profile.enabled)
        form.addRow("label", label)
        form.addRow("username", username)
        form.addRow("password", password)
        form.addRow("credentials", credentials_json)
        form.addRow("settings", settings_json)
        form.addRow("enabled", enabled)
        save = PrimaryPushButton("保存")
        form.addRow(save)
        save.clicked.connect(
            lambda: self._save_profile_edit(
                dialog,
                profile,
                label.text(),
                username.text(),
                password.text(),
                credentials_json.toPlainText(),
                settings_json.toPlainText(),
                enabled.isChecked(),
            )
        )
        dialog.resize(640, 560)
        dialog.exec()

    def _save_profile_edit(
        self,
        dialog,
        profile: Profile,
        label: str,
        username: str,
        password: str,
        credentials_text: str,
        settings_text: str,
        enabled: bool,
    ) -> None:
        try:
            if not label.strip():
                raise ValueError("Profile 名称不能为空")
            credentials = self._json_object(credentials_text, "credentials")
            if username.strip() or "username" in credentials:
                credentials["username"] = username.strip()
            elif profile.credentials.get("username"):
                credentials["username"] = profile.credentials["username"]
            if password:
                credentials["password"] = password
            elif profile.credentials.get("password"):
                credentials["password"] = profile.credentials["password"]
            settings = self._json_object(settings_text, "settings")
            self.controller.save_profile(
                profile.provider,
                label,
                credentials,
                settings=settings,
                profile_id=profile.id,
                enabled=enabled,
            )
            dialog.close()
            self.reload_profiles()
        except (OSError, ValueError) as error:
            QMessageBox.critical(self, self.provider, str(error))

    @staticmethod
    def _json_object(text: str, name: str) -> dict[str, Any]:
        try:
            value = json.loads(text or "{}")
        except json.JSONDecodeError as error:
            raise ValueError(f"{name} 必须是合法 JSON：{error}") from error
        if not isinstance(value, dict):
            raise ValueError(f"{name} 必须是 JSON object")
        return dict(value)

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
        self.worker_thread.event.connect(lambda event: self._event_received(label, event))
        self.worker_thread.succeeded.connect(lambda result: self._success(label, result))
        self.worker_thread.failed.connect(lambda error: self._failure(label, error))
        self.worker_thread.start()

    def _event_received(self, label: str, event: Any) -> None:
        """Render worker progress without allowing raw credentials into the UI."""
        if not isinstance(event, dict):
            return
        event_type = str(event.get("type") or "event")
        if event_type == "progress":
            current = event.get("current", "?")
            total = event.get("total")
            suffix = f"/{total}" if total not in (None, "") else ""
            message = str(event.get("message") or "")
            self.log.append(f"[{label}] progress {current}{suffix} {message}".rstrip())
            return
        if event_type == "log":
            level = str(event.get("level") or "info").upper()
            message = self._preview_text(event.get("message"), limit=1000)
            self.log.append(f"[{label}] {level} {message}".rstrip())
            return
        if event_type == "error":
            code = str(event.get("code") or "worker_error")
            message = self._preview_text(event.get("message"), limit=1000)
            self.log.append(f"[{label}] ERROR {code}: {message}".rstrip())
            return
        self.log.append(
            f"[{label}] {event_type} "
            + json.dumps(self._safe_preview(event), ensure_ascii=False, default=str)
        )

    def _success(self, label: str, result: Any) -> None:
        if label == "courses":
            self.current_courses = result if isinstance(result, list) else []
            self.course_table.setRowCount(len(self.current_courses))
            for row, course in enumerate(self.current_courses):
                grade = course.get("provider_summary", {}).get("grade", {})
                grade_text = grade.get("overall_score", "") if isinstance(grade, dict) else ""
                values = (
                    str(row + 1),
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
            self.current_questions = []
            self.question_table.setRowCount(0)
            self.current_formal_tasks = [
                task for task in self.current_tasks if self._is_formal(task)
            ]
            self.current_routine_tasks = [
                task for task in self.current_tasks if not self._is_formal(task)
            ]
            self.task_table.setRowCount(len(self.current_routine_tasks))
            for row, task in enumerate(self.current_routine_tasks):
                values = (
                    row + 1,
                    task.get("title", ""),
                    task.get("type", task.get("task_type", task.get("source_type", ""))),
                    task.get("state", ""),
                )
                for column, value in enumerate(values):
                    self.task_table.setItem(row, column, QTableWidgetItem(str(value)))
            self.formal_table.setRowCount(len(self.current_formal_tasks))
            for row, task in enumerate(self.current_formal_tasks):
                values = (
                    row + 1,
                    task.get("title", ""),
                    task.get("type", task.get("source_type", "")),
                    task.get("state", ""),
                )
                for column, value in enumerate(values):
                    self.formal_table.setItem(row, column, QTableWidgetItem(str(value)))
        elif label == "questions":
            self.current_questions = result if isinstance(result, list) else []
            self.question_table.setRowCount(len(self.current_questions))
            for row, question in enumerate(self.current_questions):
                if not isinstance(question, dict):
                    continue
                prompt = question.get("prompt") or question.get("question") or question.get("stem")
                options = question.get("options") or question.get("choices") or []
                values = (
                    str(row + 1),
                    question.get("kind") or "provider_native",
                    self._preview_text(prompt),
                    self._preview_text(options),
                )
                for column, value in enumerate(values):
                    self.question_table.setItem(
                        row, column, QTableWidgetItem(str(value if value is not None else ""))
                    )
        data = result.data if hasattr(result, "data") else result
        if label == "oauth begin" and isinstance(data, dict):
            authorization_url = str(data.get("authorization_url") or "").strip()
            if authorization_url:
                self._show_oauth_authorization(authorization_url)
        preview = json.dumps(self._safe_preview(data), ensure_ascii=False, default=str)
        self.log.append(f"[{label}] {preview}")
        if label in {"prepare draft", "prepare drafts", "batch run"}:
            draft_values = []
            if hasattr(result, "id") and hasattr(result, "payload"):
                draft_values = [result]
            elif isinstance(result, list):
                draft_values = [item for item in result if hasattr(item, "id")]
            elif isinstance(result, dict):
                draft_values = [item for item in result.get("drafts", []) if hasattr(item, "id")]
            for draft in draft_values:
                self.log.append(f"[draft] {draft.id}")
            if draft_values:
                window = self.window()
                if isinstance(window, MainWindow):
                    for page in window.pages:
                        if isinstance(page, DraftPage):
                            page.reload()
                            break
        event = "success"
        summary: dict[str, Any] = {"status": "success"}
        batch_results = result
        if label == "batch run" and isinstance(result, dict):
            drafts = result.get("drafts")
            if isinstance(drafts, list):
                for draft in drafts:
                    self.log.append(f"[draft] {getattr(draft, 'id', '')}")
                batch_results = result.get("routine_results", [])
            else:
                batch_results = []
        if label == "batch run" and isinstance(batch_results, list):
            failed = sum(1 for item in batch_results if getattr(item, "error_code", None))
            completed = len(batch_results) - failed
            event = "failure" if failed else "success"
            summary = {"status": event, "completed": completed, "failed": failed}
        elif label == "scan profiles" and isinstance(result, list):
            failed = sum(1 for item in result if getattr(item, "state", "") != "completed")
            completed = len(result) - failed
            event = "failure" if failed else "success"
            summary = {"status": event, "completed": completed, "failed": failed}
        if label == "batch run" and isinstance(summary, dict) and "completed" in summary:
            self.log.append(
                "[batch summary] "
                + json.dumps(summary, ensure_ascii=False, separators=(",", ":"))
            )
        if label in {"run", "batch run"}:
            self.controller.notify(
                event,
                provider=self.provider,
                operation=label,
                summary=summary,
            )
        self.cancel_button.setEnabled(False)
        self.cancel_event = None

    def _show_oauth_authorization(self, authorization_url: str) -> None:
        dialog = QDialog(self)
        dialog.setWindowTitle("cidaren OAuth")
        layout = QVBoxLayout(dialog)
        layout.addWidget(BodyLabel("请复制链接，在微信中打开并完成确认；确认后将回调链接粘贴回本页面。"))
        editor = LineEdit()
        editor.setText(authorization_url)
        editor.setReadOnly(True)
        layout.addWidget(editor)
        actions = QHBoxLayout()
        copy = PushButton("复制链接")
        copy.clicked.connect(lambda: QGuiApplication.clipboard().setText(authorization_url))
        actions.addWidget(copy)
        close = PushButton("关闭")
        close.clicked.connect(dialog.close)
        actions.addWidget(close)
        actions.addStretch(1)
        layout.addLayout(actions)
        dialog.resize(820, 180)
        dialog.show()

    def _failure(self, label: str, error: str) -> None:
        self.log.append(f"[{label}] ERROR {error}")
        if label in {"run", "batch run"}:
            self.controller.notify(
                "failure",
                provider=self.provider,
                operation=label,
                summary={"status": "failure", "error_code": "operation_failed"},
            )
        self.cancel_button.setEnabled(False)
        self.cancel_event = None

    @staticmethod
    def _safe_preview(value: Any, *, depth: int = 0) -> Any:
        """Keep the UI useful without echoing credentials or huge payloads."""
        if depth > 4:
            return "<truncated>"
        if hasattr(value, "task_remote_id") and hasattr(value, "error_code"):
            return {
                "task_remote_id": "<internal>",
                "error_code": getattr(value, "error_code", None),
                "error_message": ProviderPage._preview_text(
                    getattr(value, "error_message", ""), limit=300
                ),
            }
        if hasattr(value, "operation") and hasattr(value, "data"):
            return {
                "operation": str(getattr(value, "operation", "")),
                "data": ProviderPage._safe_preview(getattr(value, "data", {}), depth=depth + 1),
            }
        if isinstance(value, dict):
            result = {}
            for key, child in value.items():
                name = str(key)
                lowered = name.casefold()
                if lowered == "authorization_url":
                    # The full OAuth URL contains a short-lived state/marker;
                    # it is shown only in the explicit copy dialog and must
                    # never be echoed into the persistent UI log.
                    result[name] = "<redacted authorization url>"
                elif (
                    lowered in {"id", "remote_id", "task_remote_id", "profile_id", "task_ref"}
                    or lowered.endswith("_id")
                    or lowered.endswith("_ref")
                ):
                    result[name] = "<internal>"
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
        self._call(
            lambda on_event: self.controller.health(self.provider, on_event=on_event), "health"
        )

    def install_donor(self) -> None:
        if self.provider != "welearn":
            return
        if (
            QMessageBox.question(
                self,
                "welearn",
                "将从固定 revision 获取外部 donor。是否允许使用系统 Git或联网下载？",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        self._call(
            lambda _on_event: self.controller.install_external_upstream(self.provider),
            "install donor",
        )

    def authenticate(self) -> None:
        profile = self.profile()
        if profile:
            self._call(
                lambda on_event: self.controller.service.authenticate(profile, on_event=on_event),
                "authenticate",
            )

    def sync_courses(self) -> None:
        profile = self.profile()
        if profile:
            self._call(
                lambda on_event: self.controller.sync_courses(
                    profile, cancel=self.cancel_event, on_event=on_event
                ),
                "courses",
            )

    def _course_selected(self) -> None:
        row = self.course_table.currentRow()
        if 0 <= row < len(self.current_courses):
            self.current_tasks = []
            self.current_routine_tasks = []
            self.current_formal_tasks = []
            self.current_questions = []
            self.task_table.setRowCount(0)
            self.formal_table.setRowCount(0)
            self.question_table.setRowCount(0)

    @staticmethod
    def _select_all_rows(table: TableWidget) -> None:
        table.selectAll()

    def _clear_task_selection(self) -> None:
        self.task_table.clearSelection()
        self.formal_table.clearSelection()

    def _ask_concurrency(self) -> int | None:
        """Read a positive worker count without imposing a product cap."""
        value, accepted = QInputDialog.getText(self, self.provider, "并发数", text="1")
        if not accepted:
            return None
        try:
            concurrency = int(value.strip())
        except (TypeError, ValueError):
            QMessageBox.warning(self, self.provider, "并发数必须是正整数")
            return None
        if concurrency < 1:
            QMessageBox.warning(self, self.provider, "并发数必须是正整数")
            return None
        return concurrency

    def _batch_concurrency(self) -> int | None:
        """Only chaoxing exposes cross-task concurrency to the desktop operator."""
        if self.provider != "chaoxing":
            return 1
        return self._ask_concurrency()

    @staticmethod
    def _preview_text(value: Any, *, limit: int = 500) -> str:
        if isinstance(value, (dict, list, tuple)):
            text = json.dumps(value, ensure_ascii=False, default=str)
        else:
            text = str(value or "")
        text = " ".join(text.split())
        return text if len(text) <= limit else text[: limit - 1] + "…"

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
            lambda on_event: self.controller.sync_tasks(
                profile, course, cancel=self.cancel_event, on_event=on_event
            ),
            "tasks",
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
            lambda on_event: self.controller.scan_questions(
                profile,
                task,
                allow_read_that_starts_attempt=allow_attempt,
                cancel=self.cancel_event,
                on_event=on_event,
            ),
            "questions",
        )

    def show_task_detail(self) -> None:
        task = self._selected_task()
        if task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        dialog = QDialog(self)
        dialog.setWindowTitle(f"{self.provider} task detail")
        layout = QVBoxLayout(dialog)
        viewer = TextEdit()
        viewer.setReadOnly(True)
        viewer.setPlainText(json.dumps(self._safe_preview(task), ensure_ascii=False, indent=2))
        layout.addWidget(viewer)
        close = PushButton("关闭")
        close.clicked.connect(dialog.close)
        layout.addWidget(close)
        dialog.resize(900, 640)
        dialog.show()

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
            lambda on_event: self.controller.scan_all(
                profile,
                allow_cidaren_attempt=allow_attempt,
                cancel=self.cancel_event,
                on_update=lambda status: on_event(
                    {
                        "type": "progress",
                        "current": status.completed_tasks,
                        "total": status.task_count or None,
                        "message": status.last_error or status.phase,
                    }
                ),
            ),
            "scan all",
        )

    def scan_status(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        status = self.controller.scan_status(profile)
        dialog = QDialog(self)
        dialog.setWindowTitle(f"{self.provider} scan status")
        layout = QVBoxLayout(dialog)
        table = TableWidget()
        table.setColumnCount(2)
        table.setHorizontalHeaderLabels(["项目", "内容"])
        values = (
            ("状态", status.state),
            ("阶段", status.phase),
            ("课程数", status.course_count),
            ("任务数", status.task_count),
            ("题目数", status.question_count),
            ("已完成任务", status.completed_tasks),
            ("重试次数", status.retries),
            ("扫描位置", status.cursor),
            ("最近错误", status.last_error),
            ("更新时间", status.updated_at),
        )
        table.setRowCount(len(values))
        for row, (key, value) in enumerate(values):
            table.setItem(row, 0, QTableWidgetItem(str(key)))
            table.setItem(row, 1, QTableWidgetItem(str(value)))
        configure_table(table)
        layout.addWidget(table)
        actions = QHBoxLayout()
        retry = PushButton("重新扫描当前 Profile")
        retry.clicked.connect(lambda: (dialog.close(), self.scan_all()))
        actions.addWidget(retry)
        close = PushButton("关闭")
        close.clicked.connect(dialog.close)
        actions.addWidget(close)
        actions.addStretch(1)
        layout.addLayout(actions)
        dialog.resize(720, 520)
        dialog.show()

    def scan_profiles(self) -> None:
        if self.provider != "chaoxing":
            return
        if (
            QMessageBox.question(
                self,
                "chaoxing",
                "按本地 Profile 逐个执行可恢复的只读全量扫描？失败账号会记录并继续。",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        self._call(
            lambda on_event: self.controller.scan_all_profiles(
                cancel=self.cancel_event,
                on_update=lambda profile, status: on_event(
                    {
                        "type": "progress",
                        "current": status.completed_tasks,
                        "total": status.task_count or None,
                        "message": f"{profile.label}: {status.last_error or status.phase}",
                    }
                ),
            ),
            "scan profiles",
        )

    def run_selected(self) -> None:
        profile = self.profile()
        task = self._selected_task()
        if not profile or task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        if self._is_formal(task):
            if (
                QMessageBox.question(
                    self,
                    self.provider,
                    "读取正式任务可能建立答题尝试或启动倒计时。继续后仅生成预填草稿，"
                    "不会最终提交；提交仍需在 drafts 页面二次确认。是否继续？",
                )
                != QMessageBox.StandardButton.Yes
            ):
                return
            combination = self._execution_combination()
            self._call(
                lambda on_event: self.controller.prepare_formal_draft(
                    profile,
                    task,
                    combination=combination,
                    settings=self._execution_settings(combination),
                    allow_read_that_starts_attempt=True,
                    cancel=self.cancel_event,
                    on_event=on_event,
                ),
                "prepare draft",
            )
            return
        combination = self._execution_combination()
        execution_settings = self._execution_settings(combination)
        self._call(
            lambda on_event: self._execute_task(
                profile, task, on_event, combination, execution_settings
            ),
            "run",
        )

    def _execution_combination(self) -> str:
        return str(self.execution_combination.currentData() or "economy")

    def _execution_settings(self, combination: str | None = None) -> dict[str, Any]:
        settings = {"answer_combination": combination or self._execution_combination()}
        if self.provider == "uai" and self.generated_text is not None:
            text = self.generated_text.toPlainText().strip()
            if text:
                settings["generated_text"] = text
        return settings

    def _execute_task(
        self,
        profile: Profile,
        task: dict[str, Any],
        on_event=None,
        combination: str = "economy",
        execution_settings: dict[str, Any] | None = None,
    ) -> Any:
        answers = None
        if self.provider == "chaoxing":
            native = task.get("native") if isinstance(task.get("native"), dict) else {}
            route = "timed" if native.get("route_kind") == "course_exam" else "untimed"
            answers = self.controller.prepare_answers(
                profile, task, combination=combination, route=route
            )
        return self.controller.run_task(
            profile,
            task,
            answers=answers,
            settings=dict(execution_settings or self._execution_settings(combination)),
            cancel=self.cancel_event,
            on_event=on_event,
        )

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
                f"读取选中的 {len(formal)} 个正式任务可能建立答题尝试或启动倒计时。"
                "继续后仅生成预填草稿，不会最终提交；提交仍需在 drafts 页面二次确认。"
                "是否继续？",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        combination = self._execution_combination()
        execution_settings = self._execution_settings(combination)
        concurrency = 1
        if routine:
            concurrency = self._batch_concurrency()
            if concurrency is None:
                return
        if formal:
            def execute_selected(on_event):
                drafts = self.controller.prepare_formal_drafts(
                    profile,
                    formal,
                    combination=combination,
                    settings=execution_settings,
                    allow_read_that_starts_attempt=True,
                    cancel=self.cancel_event,
                    on_event=on_event,
                )
                if not routine:
                    return drafts
                results = self.controller.run_batch(
                    profile,
                    routine,
                    concurrency=concurrency,
                    settings=execution_settings,
                    answer_provider=(
                        lambda task: (
                            self.controller.prepare_answers(
                                profile,
                                task,
                                combination=combination,
                                route=(
                                    "timed"
                                    if isinstance(task.get("native"), dict)
                                    and task["native"].get("route_kind") == "course_exam"
                                    else "untimed"
                                ),
                                cancel=self.cancel_event,
                                on_event=on_event,
                            )
                            if self.provider == "chaoxing"
                            else None
                        )
                    ),
                    cancel=self.cancel_event,
                    on_event=on_event,
                )
                return {"drafts": drafts, "routine_results": results}

            self._call(
                execute_selected,
                "prepare drafts" if not routine else "batch run",
            )
            return
        if not routine:
            return
        self._call(
            lambda on_event: self.controller.run_batch(
                profile,
                routine,
                concurrency=concurrency,
                settings=execution_settings,
                answer_provider=(
                    lambda task: (
                        self.controller.prepare_answers(
                            profile,
                            task,
                            combination=combination,
                            route=(
                                "timed"
                                if isinstance(task.get("native"), dict)
                                and task["native"].get("route_kind") == "course_exam"
                                else "untimed"
                            ),
                            cancel=self.cancel_event,
                            on_event=on_event,
                        )
                        if self.provider == "chaoxing"
                        else None
                    )
                ),
                cancel=self.cancel_event,
                on_event=on_event,
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
            lambda on_event: self.controller.read_duration(
                profile, task, cancel=self.cancel_event, on_event=on_event
            ),
            "duration",
        )

    def inspect_task(self) -> None:
        profile = self.profile()
        task = self._selected_task()
        if not profile or task is None:
            QMessageBox.warning(self, self.provider, "请先同步并选择 task")
            return
        self._call(
            lambda on_event: self.controller.inspect_task(
                profile, task, cancel=self.cancel_event, on_event=on_event
            ),
            "inspect",
        )

    def oauth_begin(self) -> None:
        profile = self.profile()
        if profile:
            self._call(
                lambda on_event: self.controller.service.oauth_begin(profile, on_event=on_event),
                "oauth begin",
            )

    def oauth_exchange(self) -> None:
        profile = self.profile()
        if not profile:
            return
        callback_url, accepted = QInputDialog.getText(self, "cidaren", "粘贴微信确认后的回调链接")
        if accepted and callback_url.strip():
            self._call(
                lambda on_event: self.controller.service.oauth_exchange(
                    profile, callback_url.strip(), on_event=on_event
                ),
                "oauth exchange",
            )


class DraftPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        self.current_rows: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
        root = QVBoxLayout(self)
        root.setContentsMargins(28, 24, 28, 28)
        root.setSpacing(14)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(20, 16, 20, 16)
        intro_layout.setSpacing(5)
        intro_layout.addWidget(TitleLabel("作业与考试草稿"))
        intro_layout.addWidget(BodyLabel("这里集中显示需要人工确认的正式作业和考试。"))
        intro_layout.addWidget(CaptionLabel("编辑和保存不会自动提交；只有点击“确认并提交”并再次确认后才会调用平台提交。"))
        root.addWidget(intro)
        table_card = CardWidget()
        table_layout = QVBoxLayout(table_card)
        table_layout.setContentsMargins(16, 14, 16, 14)
        table_layout.addWidget(StrongBodyLabel("待处理草稿"))
        self.table = TableWidget()
        self.table.setColumnCount(5)
        self.table.setHorizontalHeaderLabels(
            ["平台", "账号", "任务", "状态", "更新时间"]
        )
        configure_table(self.table)
        table_layout.addWidget(self.table)
        root.addWidget(table_card, 1)
        actions = QHBoxLayout()
        refresh = PushButton("刷新")
        refresh.clicked.connect(self.reload)
        actions.addWidget(refresh)
        edit = PushButton("编辑草稿")
        edit.clicked.connect(self.edit_selected)
        actions.addWidget(edit)
        save_remote = PushButton("保存到平台（不提交）")
        save_remote.clicked.connect(self.save_selected_to_provider)
        actions.addWidget(save_remote)
        submit = PrimaryPushButton("确认并提交")
        submit.clicked.connect(self.submit_selected)
        actions.addWidget(submit)
        discard = PushButton("丢弃")
        discard.clicked.connect(self.discard_selected)
        actions.addWidget(discard)
        root.addLayout(actions)
        log_card = CardWidget()
        log_layout = QVBoxLayout(log_card)
        log_layout.setContentsMargins(16, 14, 16, 14)
        log_layout.addWidget(StrongBodyLabel("操作记录"))
        self.log = TextEdit()
        self.log.setReadOnly(True)
        log_layout.addWidget(self.log)
        root.addWidget(log_card)
        self.reload()

    def reload(self) -> None:
        self.current_rows = self.controller.draft_rows()
        self.table.setRowCount(len(self.current_rows))
        for row, value in enumerate(self.current_rows):
            provider = str(value.get("provider") or "")
            profile_label = "本地账号"
            try:
                profile = self.controller.profiles.get(provider, str(value.get("profile_id") or ""))
                profile_label = profile.label
            except (OSError, ValueError):
                pass
            payload = value.get("payload") if isinstance(value.get("payload"), dict) else {}
            task_title = str(payload.get("title") or payload.get("task_title") or "待确认任务")
            visible = (
                provider,
                profile_label,
                task_title,
                value.get("status"),
                value.get("updated_at"),
            )
            for column, item in enumerate(visible):
                self.table.setItem(row, column, QTableWidgetItem(str(item or "")))

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

    def _operation_running(self) -> bool:
        return self.worker_thread is not None and self.worker_thread.isRunning()

    def _refuse_if_busy(self) -> bool:
        if not self._operation_running():
            return False
        QMessageBox.information(self, "drafts", "已有草稿保存或提交操作正在运行")
        return True

    def edit_selected(self) -> None:
        if self._refuse_if_busy():
            return
        draft = self._selected()
        if draft is None:
            return
        dialog = FormalDraftEditor(draft.payload, self)
        dialog.setWindowTitle("编辑草稿")

        def save_payload(payload: object) -> None:
            try:
                if not isinstance(payload, dict):
                    raise ValueError("草稿内容必须是 JSON object")
                self.controller.update_draft(draft, payload)
                self.log.append(f"[draft] saved {draft.id}")
                self.reload()
            except (OSError, TypeError, ValueError) as error:
                QMessageBox.critical(dialog, "drafts", str(error))

        dialog.saved.connect(save_payload)
        dialog.show()

    def submit_selected(self) -> None:
        if self._refuse_if_busy():
            return
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
        self.worker_thread = CallThread(lambda _on_event: self.controller.submit_draft(draft))
        self.worker_thread.succeeded.connect(self._submit_succeeded)
        self.worker_thread.failed.connect(lambda error: self.log.append(f"[draft] ERROR {error}"))
        self.worker_thread.start()

    def save_selected_to_provider(self) -> None:
        if self._refuse_if_busy():
            return
        draft = self._selected()
        if draft is None:
            return
        if draft.provider != "chaoxing":
            QMessageBox.information(self, "drafts", "该 Provider 没有已确认的只保存接口")
            return
        if draft.status != "draft":
            QMessageBox.information(self, "drafts", "只有 draft 状态可以保存")
            return
        if (
            QMessageBox.question(
                self,
                "drafts",
                "将当前答案保存到平台，但不执行最终提交。确认继续？",
            )
            != QMessageBox.StandardButton.Yes
        ):
            return
        self.log.append(f"[draft] saving without final submit {draft.id}")
        self.worker_thread = CallThread(
            lambda _on_event: self.controller.save_draft_to_provider(draft)
        )
        self.worker_thread.succeeded.connect(
            lambda _result: self.log.append("[draft] saved without final submit")
        )
        self.worker_thread.failed.connect(lambda error: self.log.append(f"[draft] ERROR {error}"))
        self.worker_thread.start()

    def _submit_succeeded(self, _result: Any) -> None:
        self.log.append("[draft] submitted")
        self.reload()

    def discard_selected(self) -> None:
        if self._refuse_if_busy():
            return
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
        root.setContentsMargins(28, 24, 28, 28)
        root.setSpacing(14)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(20, 16, 20, 16)
        intro_layout.setSpacing(5)
        intro_layout.addWidget(TitleLabel("全局题库"))
        intro_layout.addWidget(
            BodyLabel(
                "题目按题干、材料、选项内容和媒体语义匹配，平台题号和选项 ID 不作为唯一依据。"
            )
        )
        intro_layout.addWidget(
            CaptionLabel(
                "AI 结果、平台返回的正误观察和人工答案都会写入本机缓存；"
                "当前页面不直接提交平台答案。"
            )
        )
        root.addWidget(intro)
        stats_card = CardWidget()
        stats_layout = QVBoxLayout(stats_card)
        stats_layout.setContentsMargins(16, 14, 16, 14)
        stats_layout.addWidget(StrongBodyLabel("题库状态"))
        self.summary = BodyLabel()
        stats_layout.addWidget(self.summary)
        root.addWidget(stats_card)
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
        evidence = PushButton("查看候选与证据")
        evidence.clicked.connect(self.show_evidence)
        controls.addWidget(evidence)
        manual = PushButton("保存人工答案")
        manual.clicked.connect(self.save_manual_answer)
        controls.addWidget(manual)
        root.addLayout(controls)
        table_card = CardWidget()
        table_layout = QVBoxLayout(table_card)
        table_layout.setContentsMargins(16, 14, 16, 14)
        table_layout.addWidget(StrongBodyLabel("题目"))
        self.table = TableWidget()
        self.table.setColumnCount(4)
        self.table.setHorizontalHeaderLabels(["平台", "题型", "题干", "选项"])
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        configure_table(self.table)
        table_layout.addWidget(self.table, 1)
        root.addWidget(table_card, 1)
        log_card = CardWidget()
        log_layout = QVBoxLayout(log_card)
        log_layout.setContentsMargins(16, 14, 16, 14)
        log_layout.addWidget(StrongBodyLabel("操作记录"))
        self.log = TextEdit()
        self.log.setReadOnly(True)
        log_layout.addWidget(self.log)
        root.addWidget(log_card)
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
                    self._preview_text(
                        content.get("options") if isinstance(content, dict) else ""
                    ),
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
            lambda _on_event: self.controller.answer_question(
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

    def _selected_question(self) -> dict[str, Any] | None:
        row = self.table.currentRow()
        if row < 0 or row >= len(self.rows):
            QMessageBox.warning(self, "question bank", "请先选择题目")
            return None
        return self.rows[row]

    def show_evidence(self) -> None:
        question = self._selected_question()
        if question is None:
            return
        try:
            value = self.controller.question_evidence(int(question["id"]))
        except (KeyError, OSError, TypeError, ValueError) as error:
            QMessageBox.critical(self, "question bank", str(error))
            return
        dialog = QDialog(self)
        dialog.setWindowTitle("候选与证据")
        layout = QVBoxLayout(dialog)
        viewer = TextEdit()
        viewer.setReadOnly(True)
        viewer.setPlainText(json.dumps(value, ensure_ascii=False, indent=2))
        layout.addWidget(viewer)
        dialog.resize(760, 560)
        dialog.show()

    def save_manual_answer(self) -> None:
        question = self._selected_question()
        if question is None:
            return
        dialog = QDialog(self)
        dialog.setWindowTitle("保存人工答案")
        layout = QVBoxLayout(dialog)
        layout.addWidget(
            BodyLabel("选择题填写选项完整内容；多选、连线、排序等结构化答案填写 JSON。")
        )
        editor = TextEdit()
        editor.setPlaceholderText('例如：选项正文；或 ["选项一", "选项二"]')
        layout.addWidget(editor)
        save = PrimaryPushButton("确认为正确答案并保存")
        layout.addWidget(save)

        def persist() -> None:
            text = editor.toPlainText().strip()
            if not text:
                QMessageBox.warning(dialog, "question bank", "答案不能为空")
                return
            try:
                try:
                    value = json.loads(text)
                except json.JSONDecodeError:
                    value = text
                candidate_id = self.controller.save_manual_answer(question, value)
                self.log.append(f"[manual] saved candidate {candidate_id}")
                dialog.close()
            except (OSError, TypeError, ValueError) as error:
                QMessageBox.critical(dialog, "question bank", str(error))

        save.clicked.connect(persist)
        dialog.resize(620, 360)
        dialog.show()


class SettingsPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        root = QVBoxLayout(self)
        root.setContentsMargins(28, 24, 28, 28)
        root.setSpacing(14)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(20, 16, 20, 16)
        intro_layout.setSpacing(5)
        intro_layout.addWidget(TitleLabel("设置"))
        intro_layout.addWidget(BodyLabel("主题、模型组合、通知和 Provider 默认值保存在本机配置。"))
        intro_layout.addWidget(CaptionLabel("配置内容不会自动上传；凭据和会话状态保存在本地数据目录。"))
        root.addWidget(intro)
        appearance = CardWidget()
        appearance_layout = QHBoxLayout(appearance)
        appearance_layout.setContentsMargins(20, 14, 20, 14)
        appearance_layout.addWidget(StrongBodyLabel("外观主题"))
        self.theme = ComboBox()
        self.theme.addItem("跟随系统", ThemeMode.SYSTEM.value)
        self.theme.addItem("浅色", ThemeMode.LIGHT.value)
        self.theme.addItem("深色", ThemeMode.DARK.value)
        current_theme = str(
            controller.config.ensure().get("ui", {}).get("theme", ThemeMode.SYSTEM.value)
        )
        self.theme.setCurrentIndex(max(0, self.theme.findData(current_theme)))
        self.theme.currentIndexChanged.connect(self.apply_selected_theme)
        appearance_layout.addWidget(self.theme)
        appearance_layout.addStretch(1)
        root.addWidget(appearance)
        language = CardWidget()
        language_layout = QHBoxLayout(language)
        language_layout.setContentsMargins(20, 14, 20, 14)
        language_layout.addWidget(StrongBodyLabel("界面语言 / Language"))
        self.language = ComboBox()
        self.language.addItem("简体中文", "zh-CN")
        self.language.addItem("English", "en-US")
        current_language = str(
            controller.config.ensure().get("ui", {}).get("language", "zh-CN")
        )
        self.language.setCurrentIndex(max(0, self.language.findData(current_language)))
        self.language.currentIndexChanged.connect(self.apply_selected_language)
        language_layout.addWidget(self.language)
        language_layout.addWidget(
            CaptionLabel("切换后重新打开窗口生效 / Restart the window to apply")
        )
        language_layout.addStretch(1)
        root.addWidget(language)
        editor_card = CardWidget()
        editor_layout = QVBoxLayout(editor_card)
        editor_layout.setContentsMargins(20, 16, 20, 16)
        editor_layout.setSpacing(8)
        editor_layout.addWidget(StrongBodyLabel("高级配置"))
        editor_layout.addWidget(
            CaptionLabel(
                "需要精细调整模型或 Provider 参数时，可直接编辑 JSON。"
                "保存前请确认格式正确。"
            )
        )
        self.editor = TextEdit()
        self.editor.setPlainText(
            json.dumps(controller.config.ensure(), ensure_ascii=False, indent=2)
        )
        editor_layout.addWidget(self.editor)
        save = PrimaryPushButton("保存配置")
        save.clicked.connect(self.save)
        editor_layout.addWidget(save, 0, Qt.AlignmentFlag.AlignLeft)
        root.addWidget(editor_card, 1)

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

    def apply_selected_language(self) -> None:
        try:
            value = json.loads(self.editor.toPlainText())
            value.setdefault("ui", {})["language"] = self.language.currentData()
            self.editor.setPlainText(json.dumps(value, ensure_ascii=False, indent=2))
            self.controller.config.save(value)
        except (TypeError, ValueError, json.JSONDecodeError) as error:
            self.language.setToolTip(f"配置暂不可解析：{error}")

    def log_theme_error(self, message: str) -> None:
        # Keep theme selection non-destructive if the JSON editor is temporarily invalid.
        self.theme.setToolTip(f"配置暂不可解析：{message}")


class MainWindow(FluentWindow if FLUENT_AVAILABLE else QMainWindow):
    def __init__(self, data_root: Path | None = None, source_root: Path | None = None):
        super().__init__()
        self.controller = DesktopController.create(data_root, source_root)
        self.language_code = str(
            self.controller.config.ensure().get("ui", {}).get("language", "zh-CN")
        )
        self._show_first_run_wizard()
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
        self.pages: list[QWidget] = []
        if FLUENT_AVAILABLE:
            self._init_fluent_shell()
        else:
            self._init_classic_shell()
        self.add_page("home", HomePage(self.controller))
        for provider in PROVIDER_IDS:
            self.add_page(provider, ProviderPage(self.controller, provider))
        self.add_page("drafts", DraftPage(self.controller))
        self.add_page("question-bank", QuestionBankPage(self.controller))
        self.add_page("settings", SettingsPage(self.controller))
        if not FLUENT_AVAILABLE:
            self.navigation.currentRowChanged.connect(self.stack.setCurrentIndex)
            self.navigation.setCurrentRow(0)

    def _init_classic_shell(self) -> None:
        shell = QWidget()
        self.setCentralWidget(shell)
        layout = QHBoxLayout(shell)
        self.navigation = QListWidget()
        self.navigation.setFixedWidth(150)
        self.stack = QStackedWidget()
        layout.addWidget(self.navigation)
        layout.addWidget(self.stack, 1)

    def _init_fluent_shell(self) -> None:
        # Mica is optional and can crash on older Windows/remote desktop sessions.
        # FluentWindow still provides the native title bar and navigation without it.
        with suppress(AttributeError, RuntimeError):
            self.setMicaEffectEnabled(False)
        self.navigation = None
        self.stack = None
        self.setMinimumSize(960, 640)

    def _show_first_run_wizard(self) -> None:
        """Give a local operator a safe first-run orientation without credentials."""
        if (
            os.environ.get("ASTERISM_NONINTERACTIVE") == "1"
            or os.environ.get("QT_QPA_PLATFORM", "").casefold() in {"offscreen", "minimal"}
        ):
            return
        config = self.controller.config.ensure()
        if config.get("onboarding_completed") is True:
            return
        dialog = QDialog(self)
        dialog.setWindowTitle("Asterism 首次启动")
        dialog.setModal(True)
        layout = QVBoxLayout(dialog)
        layout.addWidget(SubtitleLabel("欢迎使用 Asterism 本地桌面版"))
        layout.addWidget(
            BodyLabel(
                "这是单一可信操作者工具。账号 Profile、会话状态、日志、草稿和全局题库都保存在本机；"
                "不会创建 Asterism 用户，也不会把平台凭据上传到云端。"
            )
        )
        layout.addWidget(BodyLabel(f"当前数据目录：{self.controller.paths.root}"))
        layout.addWidget(
            BodyLabel(
                "下一步：在左侧平台页新建 Profile，填写对应平台凭据；在设置页配置可选模型端点，"
                "然后先执行 health、登录和课程只读刷新。正式作业/考试仍需在草稿页人工确认。"
            )
        )
        buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        buttons.accepted.connect(dialog.accept)
        buttons.rejected.connect(dialog.reject)
        layout.addWidget(buttons)
        dialog.resize(620, 300)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            config["onboarding_completed"] = True
            self.controller.config.save(config)

    def add_page(self, name: str, page: QWidget) -> None:
        labels = {
            "home": "主页",
            "drafts": "草稿",
            "question-bank": "题库",
            "settings": "设置",
        }
        english_labels = {
            "home": "Home",
            "drafts": "Drafts",
            "question-bank": "Question bank",
            "settings": "Settings",
        }
        label = (english_labels if self.language_code == "en-US" else labels).get(name, name)
        if FLUENT_AVAILABLE:
            page.setObjectName(name.replace("-", "_"))
            icon = {
                "home": FluentIcon.HOME,
                "chaoxing": FluentIcon.BOOK_SHELF,
                "welearn": FluentIcon.EDUCATION,
                "uai": FluentIcon.LANGUAGE,
                "cidaren": FluentIcon.CHAT,
                "drafts": FluentIcon.EDIT,
                "question-bank": FluentIcon.DICTIONARY,
                "settings": FluentIcon.SETTING,
            }.get(name, FluentIcon.APPLICATION)
            position = (
                NavigationItemPosition.BOTTOM
                if name == "settings"
                else NavigationItemPosition.TOP
            )
            self.addSubInterface(page, icon, label, position)
        else:
            self.navigation.addItem(QListWidgetItem(label))
            self.stack.addWidget(page)
        self.pages.append(page)
