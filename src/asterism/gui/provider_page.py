from __future__ import annotations

import json
import threading
from typing import Any

from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QFormLayout,
    QGridLayout,
    QHBoxLayout,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from ..profiles import Profile
from .common import ERROR_CODE_LABELS as _ERROR_CODE_LABELS
from .common import (
    GRADE_COMPONENT_LABELS as _GRADE_COMPONENT_LABELS,
)
from .common import (
    QUESTION_KIND_LABELS as _QUESTION_KIND_LABELS,
)
from .common import (
    STATE_LABELS as _STATE_LABELS,
)
from .common import (
    TASK_TYPE_LABELS as _TASK_TYPE_LABELS,
)
from .common import (
    CallThread,
    ask_confirmation,
    ask_text,
    display_provider,
    display_scan_phase,
    make_title,
    show_notice,
)
from .common import (
    display_code as _display_code,
)
from .common import (
    redact_text as redact_worker_text,
)
from .controller import DesktopController
from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    CheckBox,
    ComboBox,
    FluentDialogBase,
    LineEdit,
    PrimaryPushButton,
    PushButton,
    ScrollArea,
    StrongBodyLabel,
    TableWidget,
    TextEdit,
    configure_scroll_area,
    configure_table,
    form_label,
)


class ProviderPage(QWidget):
    def __init__(self, controller: DesktopController, provider: str):
        super().__init__()
        self.controller = controller
        self.provider = provider
        self.provider_label = display_provider(provider)
        self._loaded_profile_id: str | None = None
        self.current_courses: list[dict[str, Any]] = []
        self.current_tasks: list[dict[str, Any]] = []
        self.current_routine_tasks: list[dict[str, Any]] = []
        self.current_formal_tasks: list[dict[str, Any]] = []
        self.current_questions: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
        self.cancel_event: threading.Event | None = None
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        scroll = ScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(scroll.Shape.NoFrame)
        content = QWidget()
        scroll.setWidget(content)
        configure_scroll_area(scroll)
        outer.addWidget(scroll)
        root = QVBoxLayout(content)
        root.setContentsMargins(28, 24, 28, 28)
        root.setSpacing(14)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(20, 16, 20, 16)
        intro_layout.setSpacing(6)
        intro_layout.addWidget(make_title(f"{self.provider_label} 账号"))
        intro_layout.addWidget(
            BodyLabel("先创建或选择一个本地账号，再点击认证。认证成功后即可读取课程和任务。")
        )
        intro_layout.addWidget(
            CaptionLabel("账号凭据只保存在本机；当前页面的课程、任务和题目操作默认是只读的。")
        )
        root.addWidget(intro)
        header = QHBoxLayout()
        header.addWidget(StrongBodyLabel("当前账号"))
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
        profile_actions.addWidget(CaptionLabel("账号管理"))
        self.edit_profile_button = PushButton("编辑账号")
        self.edit_profile_button.clicked.connect(self.edit_profile)
        profile_actions.addWidget(self.edit_profile_button)
        self.delete_profile_button = PushButton("删除账号")
        self.delete_profile_button.clicked.connect(self.delete_profile)
        profile_actions.addWidget(self.delete_profile_button)
        profile_actions.addStretch(1)
        root.addLayout(profile_actions)
        self.profile_status = CaptionLabel("尚未选择账号")
        root.addWidget(self.profile_status)

        actions = QGridLayout()
        self.operation_buttons: list[PushButton] = []
        self.profile_optional_buttons: set[PushButton] = set()
        action_items = [
            ("连接检查", self.health),
            ("读取课程", self.sync_courses),
            ("读取任务", self.sync_tasks),
            ("扫描题目", self.scan_questions),
            ("课程详情", self.show_course_detail),
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
                [("读取时长", self.read_duration), ("准备外部组件", self.install_donor)]
            )
        if provider == "cidaren":
            action_items.extend([("开始授权", self.oauth_begin), ("完成授权", self.oauth_exchange)])
        for index, (text, callback) in enumerate(action_items):
            button = PushButton(text)
            button.clicked.connect(callback)
            self.operation_buttons.append(button)
            if text in {"连接检查", "扫描账号", "准备外部组件"}:
                self.profile_optional_buttons.add(button)
            actions.addWidget(button, index // 4, index % 4)
        self.cancel_button = PushButton("取消当前操作")
        self.cancel_button.setEnabled(False)
        self.cancel_button.clicked.connect(self.cancel_current)
        cancel_index = len(action_items)
        actions.addWidget(self.cancel_button, cancel_index // 4, cancel_index % 4)
        root.addLayout(actions)
        execution_options = QHBoxLayout()
        execution_options.addWidget(BodyLabel("本次答案组合"))
        self.execution_combination = ComboBox()
        self._reload_answer_combinations()
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
        self.course_table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        configure_table(self.course_table)
        self.course_table.itemSelectionChanged.connect(self._course_selected)
        root.addWidget(StrongBodyLabel("课程"))
        root.addWidget(self.course_table)
        self.course_empty = CaptionLabel("尚未读取课程。")
        root.addWidget(self.course_empty)
        self.task_table = TableWidget()
        self.task_table.setColumnCount(4)
        self.task_table.setHorizontalHeaderLabels(["序号", "任务", "类型", "状态"])
        self.task_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.task_table.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        configure_table(self.task_table)
        root.addWidget(StrongBodyLabel("章节 / 知识点与普通任务（平台顺序）"))
        root.addWidget(self.task_table)
        self.task_empty = CaptionLabel("选择课程并读取任务后，这里会显示普通任务。")
        root.addWidget(self.task_empty)
        task_selection = QHBoxLayout()
        task_selection.addWidget(BodyLabel("批量选择"))
        self.select_routine_button = PushButton("全选普通任务")
        self.select_routine_button.clicked.connect(lambda: self._select_all_rows(self.task_table))
        task_selection.addWidget(self.select_routine_button)
        self.select_formal_button = PushButton("全选作业/考试")
        self.select_formal_button.clicked.connect(
            lambda: self._select_all_rows(self.formal_table)
        )
        task_selection.addWidget(self.select_formal_button)
        self.clear_selection_button = PushButton("清除选择")
        self.clear_selection_button.clicked.connect(self._clear_task_selection)
        task_selection.addWidget(self.clear_selection_button)
        self.selection_buttons = [
            self.select_routine_button,
            self.select_formal_button,
            self.clear_selection_button,
        ]
        task_selection.addStretch(1)
        root.addLayout(task_selection)
        self.formal_table = TableWidget()
        self.formal_table.setColumnCount(4)
        self.formal_table.setHorizontalHeaderLabels(["序号", "作业 / 考试", "类型", "状态"])
        self.formal_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.formal_table.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        configure_table(self.formal_table)
        root.addWidget(StrongBodyLabel("作业与考试"))
        root.addWidget(self.formal_table)
        self.formal_empty = CaptionLabel("当前没有已读取的作业或考试。")
        root.addWidget(self.formal_empty)
        self.question_table = TableWidget()
        self.question_table.setColumnCount(4)
        self.question_table.setHorizontalHeaderLabels(["序号", "题型", "题干", "选项"])
        self.question_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.question_table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        configure_table(self.question_table)
        root.addWidget(StrongBodyLabel("题目预览（只读）"))
        root.addWidget(self.question_table)
        self.question_empty = CaptionLabel("选择任务并读取题目后，这里会显示题目内容。")
        root.addWidget(self.question_empty)
        self.log = TextEdit()
        self.log.setReadOnly(True)
        root.addWidget(StrongBodyLabel("运行日志与结果"))
        root.addWidget(self.log)
        self.reload_profiles()
        self._refresh_empty_states()

    def _refresh_empty_states(self) -> None:
        self.course_empty.setVisible(self.course_table.rowCount() == 0)
        self.task_empty.setVisible(self.task_table.rowCount() == 0)
        self.formal_empty.setVisible(self.formal_table.rowCount() == 0)
        self.question_empty.setVisible(self.question_table.rowCount() == 0)

    def _reload_answer_combinations(self) -> None:
        selected = self.execution_combination.currentData()
        self.execution_combination.clear()
        models = self.controller.config.ensure().get("models", {})
        combinations = models.get("combinations", {})
        default = models.get("default", "")
        display_names = {"economy": "默认", "gpt_only": "高级"}
        for name, value in combinations.items():
            configured_name = (
                str(value.get("display_name") or "") if isinstance(value, dict) else ""
            )
            visible_name = configured_name or display_names.get(name, name)
            suffix = "（默认）" if name == default and visible_name != "默认" else ""
            self.execution_combination.addItem(
                f"{visible_name}{suffix}",
                userData=name,
            )
        if self.execution_combination.count() == 0:
            self.execution_combination.addItem("未配置（仅使用平台答案或精确缓存）", userData="")
        else:
            index = self.execution_combination.findData(selected or default)
            self.execution_combination.setCurrentIndex(index if index >= 0 else 0)

    def reload_profiles(self, select_profile_id: str | None = None) -> None:
        select_profile_id = select_profile_id or self.profile_combo.currentData()
        self.profile_combo.clear()
        self.profile_combo.addItem("选择账号", userData=None)
        for profile in self.controller.profiles.list(self.provider):
            state = "" if profile.enabled else " · 不参与全账号扫描"
            self.profile_combo.addItem(f"{profile.label}{state}", userData=profile.id)
        index = self.profile_combo.findData(select_profile_id)
        self.profile_combo.setCurrentIndex(index if index >= 0 else 0)
        self._profile_changed(self.profile_combo.currentIndex())
        window = self.window()
        if hasattr(window, "home_page"):
            window.home_page.update_summary()

    def _profile_changed(self, _index: int) -> None:
        profile_id = self.profile_combo.currentData()
        normalized_id = str(profile_id) if profile_id else None
        if normalized_id != self._loaded_profile_id:
            self._clear_loaded_data()
            self._loaded_profile_id = normalized_id
        if not profile_id:
            self.profile_status.setText("尚未选择账号 · 请先点击“新建账号”")
            self._refresh_control_state()
            return
        try:
            profile = self.controller.profiles.get(self.provider, str(profile_id))
            state = "参与全账号扫描" if profile.enabled else "不参与全账号扫描"
            self.profile_status.setText(f"当前：{profile.label} · {state} · 可点击“认证 / 登录”")
        except (OSError, ValueError):
            self.profile_status.setText("当前账号无法读取")
        self._refresh_control_state()

    def _clear_loaded_data(self) -> None:
        self.current_courses = []
        self.current_tasks = []
        self.current_routine_tasks = []
        self.current_formal_tasks = []
        self.current_questions = []
        for table in (
            self.course_table,
            self.task_table,
            self.formal_table,
            self.question_table,
        ):
            table.clearSelection()
            table.setRowCount(0)
        self._refresh_empty_states()

    def profile(self) -> Profile | None:
        profile_id = self.profile_combo.currentData()
        if not profile_id:
            show_notice(self, self.provider_label, "请先选择账号", "warning")
            return None
        try:
            return self.controller.profiles.get(self.provider, str(profile_id))
        except (OSError, ValueError) as error:
            show_notice(self, self.provider_label, str(error), "error")
            return None

    def create_profile(self) -> None:
        dialog = FluentDialogBase(f"新建 {self.provider_label} 账号", self, confirm_text="保存")
        form = QFormLayout()
        dialog.content_layout.addLayout(form)
        label = LineEdit()
        label.setPlaceholderText("例如：英语账号")
        username = LineEdit()
        password = LineEdit()
        password.setEchoMode(LineEdit.EchoMode.Password)
        credentials_json = TextEdit()
        credentials_json.setPlainText("{}")
        credentials_json.setPlaceholderText('高级凭据 JSON，例如 {"cookie":"..."} 或 token')
        settings_json = TextEdit()
        settings_json.setPlainText("{}")
        settings_json.setPlaceholderText("该账号覆盖的平台设置 JSON")
        enabled = CheckBox()
        enabled.setChecked(True)
        form.addRow(form_label("显示名称"), label)
        form.addRow(form_label("用户名"), username)
        form.addRow(form_label("密码"), password)
        form.addRow(form_label("高级凭据"), credentials_json)
        form.addRow(form_label("平台设置"), settings_json)
        form.addRow(form_label("参与全账号扫描"), enabled)
        dialog.set_validator(
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
        dialog.set_content_size(680, 600)
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
    ) -> bool:
        try:
            if not label.strip():
                raise ValueError("账号名称不能为空")
            credentials = self._json_object(credentials_text, "高级凭据")
            settings = self._json_object(settings_text, "平台设置")
            if username.strip():
                credentials["username"] = username.strip()
            if password:
                credentials["password"] = password
            saved = self.controller.save_profile(
                self.provider,
                label,
                credentials,
                settings=settings,
                enabled=enabled,
            )
            self.reload_profiles(saved.id)
            return True
        except (OSError, ValueError) as error:
            show_notice(self, self.provider_label, str(error), "error")
            return False

    def edit_profile(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        dialog = FluentDialogBase(f"编辑 {self.provider_label} 账号", self, confirm_text="保存")
        form = QFormLayout()
        dialog.content_layout.addLayout(form)
        label = LineEdit()
        label.setText(profile.label)
        username = LineEdit()
        username.setText(str(profile.credentials.get("username") or ""))
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
        enabled = CheckBox()
        enabled.setChecked(profile.enabled)
        form.addRow(form_label("显示名称"), label)
        form.addRow(form_label("用户名"), username)
        form.addRow(form_label("新密码"), password)
        form.addRow(form_label("高级凭据"), credentials_json)
        form.addRow(form_label("平台设置"), settings_json)
        form.addRow(form_label("参与全账号扫描"), enabled)
        dialog.set_validator(
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
        dialog.set_content_size(680, 600)
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
    ) -> bool:
        try:
            if not label.strip():
                raise ValueError("账号名称不能为空")
            credentials = self._json_object(credentials_text, "高级凭据")
            if username.strip() or "username" in credentials:
                credentials["username"] = username.strip()
            elif profile.credentials.get("username"):
                credentials["username"] = profile.credentials["username"]
            if password:
                credentials["password"] = password
            elif profile.credentials.get("password"):
                credentials["password"] = profile.credentials["password"]
            settings = self._json_object(settings_text, "平台设置")
            saved = self.controller.save_profile(
                profile.provider,
                label,
                credentials,
                settings=settings,
                profile_id=profile.id,
                enabled=enabled,
            )
            self.reload_profiles(saved.id)
            return True
        except (OSError, ValueError) as error:
            show_notice(self, self.provider_label, str(error), "error")
            return False

    @staticmethod
    def _json_object(text: str, name: str) -> dict[str, Any]:
        try:
            value = json.loads(text or "{}")
        except json.JSONDecodeError as error:
            raise ValueError(f"{name} 必须是合法 JSON：{error}") from error
        if not isinstance(value, dict):
            raise ValueError(f"{name} 必须是 JSON 对象")
        return dict(value)

    def delete_profile(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        if not ask_confirmation(
            self,
            self.provider_label,
            "删除本地账号及其会话状态？日志和草稿会保留，但关联草稿将无法继续提交。",
        ):
            return
        try:
            self.controller.delete_profile(profile)
            self.reload_profiles()
            self.log.append("[账号] 已删除")
        except (OSError, ValueError) as error:
            show_notice(self, self.provider_label, str(error), "error")

    def _call(self, callback, label: str) -> None:
        if self.worker_thread is not None and self.worker_thread.isRunning():
            show_notice(self, self.provider_label, "已有操作正在运行")
            return
        operation = self._operation_title(label)
        self.log.append(f"[{operation}] 开始")
        self._set_home_activity(operation)
        self.cancel_event = threading.Event()
        self.worker_thread = CallThread(callback)
        self._set_operation_running(True)
        self.worker_thread.event.connect(lambda event: self._event_received(label, event))
        self.worker_thread.succeeded.connect(lambda result: self._success(label, result))
        self.worker_thread.failed.connect(lambda error: self._failure(label, error))
        self.worker_thread.start()

    def _set_operation_running(self, running: bool) -> None:
        self._operation_is_running = running
        for widget in (
            self.profile_combo,
            self.new_profile,
            self.course_table,
            self.task_table,
            self.formal_table,
            self.question_table,
            self.execution_combination,
            *self.selection_buttons,
        ):
            widget.setEnabled(not running)
        if self.generated_text is not None:
            self.generated_text.setEnabled(not running)
        self.cancel_button.setEnabled(running)
        self.cancel_button.setText("取消当前操作")
        self._refresh_control_state()

    def _refresh_control_state(self) -> None:
        running = bool(getattr(self, "_operation_is_running", False))
        has_profile = bool(self.profile_combo.currentData())
        for widget in (
            self.course_table,
            self.task_table,
            self.formal_table,
            self.question_table,
            self.execution_combination,
        ):
            widget.setEnabled(has_profile and not running)
        if self.generated_text is not None:
            self.generated_text.setEnabled(has_profile and not running)
        self.select_routine_button.setEnabled(
            has_profile and not running and self.task_table.rowCount() > 0
        )
        self.select_formal_button.setEnabled(
            has_profile and not running and self.formal_table.rowCount() > 0
        )
        self.clear_selection_button.setEnabled(
            has_profile
            and not running
            and (self.task_table.rowCount() > 0 or self.formal_table.rowCount() > 0)
        )
        self.edit_profile_button.setEnabled(has_profile and not running)
        self.delete_profile_button.setEnabled(has_profile and not running)
        self.login_button.setEnabled(has_profile and not running)
        for button in self.operation_buttons:
            button.setEnabled(
                not running and (has_profile or button in self.profile_optional_buttons)
            )

    def _event_received(self, label: str, event: Any) -> None:
        """Render worker progress without allowing raw credentials into the UI."""
        if not isinstance(event, dict):
            return
        event_type = str(event.get("type") or "event")
        operation = self._operation_title(label)
        if event_type == "progress":
            current = event.get("current", "?")
            total = event.get("total")
            suffix = f"/{total}" if total not in (None, "") else ""
            message = self._redact_text(str(event.get("message") or ""))
            self.log.append(f"[{operation}] 进度 {current}{suffix} {message}".rstrip())
            self._set_home_activity(
                self._operation_title(label), current=current, total=total, message=message
            )
            return
        if event_type == "log":
            level = {
                "debug": "调试",
                "error": "错误",
                "info": "信息",
                "warning": "警告",
            }.get(str(event.get("level") or "info").casefold(), "信息")
            message = self._redact_text(self._preview_text(event.get("message"), limit=1000))
            self.log.append(f"[{operation}] {level} {message}".rstrip())
            return
        if event_type == "error":
            code = str(event.get("code") or "worker_error")
            error_label = _display_code(code, _ERROR_CODE_LABELS, "执行组件返回错误")
            message = self._redact_text(self._preview_text(event.get("message"), limit=1000))
            self.log.append(f"[{operation}] {error_label}：{message}".rstrip("："))
            return
        self.log.append(
            f"[{operation}] 事件 "
            + json.dumps(self._safe_preview(event), ensure_ascii=False, default=str)
        )

    def _success(self, label: str, result: Any) -> None:
        self._set_home_activity(self._operation_title(label), finished=True)
        if label == "courses":
            self.current_tasks = []
            self.current_routine_tasks = []
            self.current_formal_tasks = []
            self.current_questions = []
            self.task_table.setRowCount(0)
            self.formal_table.setRowCount(0)
            self.question_table.setRowCount(0)
            self.current_courses = result if isinstance(result, list) else []
            self.course_table.setRowCount(len(self.current_courses))
            for row, course in enumerate(self.current_courses):
                grade = course.get("provider_summary", {}).get("grade", {})
                grade_text = grade.get("overall_score", "") if isinstance(grade, dict) else ""
                values = (
                    str(row + 1),
                    course.get("title", ""),
                    _display_code(course.get("state"), _STATE_LABELS),
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
                    _display_code(
                        task.get("type", task.get("task_type", task.get("source_type", ""))),
                        _TASK_TYPE_LABELS,
                    ),
                    _display_code(task.get("state"), _STATE_LABELS),
                )
                for column, value in enumerate(values):
                    self.task_table.setItem(row, column, QTableWidgetItem(str(value)))
            self.formal_table.setRowCount(len(self.current_formal_tasks))
            for row, task in enumerate(self.current_formal_tasks):
                values = (
                    row + 1,
                    task.get("title", ""),
                    _display_code(
                        task.get("type", task.get("source_type", "")),
                        _TASK_TYPE_LABELS,
                    ),
                    _display_code(task.get("state"), _STATE_LABELS),
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
                    _display_code(
                        question.get("kind") or "provider_native",
                        _QUESTION_KIND_LABELS,
                    ),
                    self._display_rich_content(prompt),
                    self._display_rich_content(options),
                )
                for column, value in enumerate(values):
                    self.question_table.setItem(
                        row, column, QTableWidgetItem(str(value if value is not None else ""))
                    )
        if label in {"courses", "tasks", "questions"}:
            self._refresh_empty_states()
        elif label == "authenticate":
            profile_name = self.profile_combo.currentText().strip()
            self.profile_status.setText(f"当前：{profile_name} · 认证成功")
        data = result.data if hasattr(result, "data") else result
        if label == "oauth begin" and isinstance(data, dict):
            authorization_url = str(data.get("authorization_url") or "").strip()
            if authorization_url:
                self._show_oauth_authorization(authorization_url)
        self.log.append(
            f"[{self._operation_title(label)}] {self._result_message(label, data)}"
        )
        if label in {"prepare draft", "prepare drafts", "batch run"}:
            draft_values = []
            if hasattr(result, "id") and hasattr(result, "payload"):
                draft_values = [result]
            elif isinstance(result, list):
                draft_values = [item for item in result if hasattr(item, "id")]
            elif isinstance(result, dict):
                draft_values = [item for item in result.get("drafts", []) if hasattr(item, "id")]
            for _draft in draft_values:
                self.log.append("[草稿] 已生成待确认草稿")
            if draft_values:
                window = self.window()
                draft_page = getattr(window, "draft_page", None)
                if draft_page is not None:
                    draft_page.reload()
        event = "success"
        summary: dict[str, Any] = {"status": "success"}
        batch_results = result
        if label == "batch run" and isinstance(result, dict):
            drafts = result.get("drafts")
            if isinstance(drafts, list):
                if drafts:
                    self.log.append(f"[草稿] 已生成 {len(drafts)} 份待确认草稿")
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
                "[批量执行摘要] " + json.dumps(summary, ensure_ascii=False, separators=(",", ":"))
            )
        if label in {"run", "batch run"}:
            self.controller.notify(
                event,
                provider=self.provider,
                operation=label,
                summary=summary,
            )
        self._set_operation_running(False)
        self.cancel_event = None
        window = self.window()
        if hasattr(window, "home_page"):
            window.home_page.update_summary()

    def _result_message(self, label: str, data: Any) -> str:
        """Summarize successful operations without leaking protocol-shaped fields."""
        if label == "courses":
            return f"已读取 {len(self.current_courses)} 门课程"
        if label == "tasks":
            return (
                f"已读取 {len(self.current_tasks)} 个任务：普通任务 "
                f"{len(self.current_routine_tasks)} 个，作业/考试 "
                f"{len(self.current_formal_tasks)} 个"
            )
        if label == "questions":
            return f"已读取 {len(self.current_questions)} 道题"
        messages = {
            "authenticate": "认证成功",
            "health": "连接正常",
            "duration": "时长信息已读取",
            "inspect": "任务详情已读取",
            "run": "任务执行完成",
            "batch run": "批量执行完成",
            "scan all": "全量扫描完成",
            "scan profiles": "账号扫描完成",
            "scan status": "扫描状态已读取",
            "prepare draft": "待确认草稿已生成",
            "prepare drafts": "待确认草稿已生成",
            "install donor": "外部组件已准备",
            "oauth begin": "授权链接已生成",
            "oauth exchange": "授权完成",
        }
        if label in messages:
            return messages[label]
        preview = json.dumps(self._safe_preview(data), ensure_ascii=False, default=str)
        return f"完成 {preview}"

    def _show_oauth_authorization(self, authorization_url: str) -> None:
        dialog = FluentDialogBase(
            "Cidaren 授权",
            self,
            confirm_text="复制链接",
            cancel_text="关闭",
        )
        dialog.content_layout.addWidget(
            BodyLabel("请复制链接，在微信中打开并完成确认；确认后将回调链接粘贴回本页面。")
        )
        editor = LineEdit()
        editor.setText(authorization_url)
        editor.setReadOnly(True)
        dialog.content_layout.addWidget(editor)
        dialog.set_validator(lambda: QGuiApplication.clipboard().setText(authorization_url) or True)
        dialog.set_content_size(820, 180)
        dialog.show()

    def _failure(self, label: str, error: object) -> None:
        self._set_home_activity(f"{self._operation_title(label)}失败", finished=True)
        code = str(getattr(error, "code", "operation_failed") or "operation_failed")
        category = _display_code(code, _ERROR_CODE_LABELS, "操作失败")
        message = self._redact_text(str(error))
        self.log.append(
            f"[{self._operation_title(label)}] {category}：{message}".rstrip("：")
        )
        if label == "authenticate":
            profile_name = self.profile_combo.currentText().strip()
            self.profile_status.setText(f"当前：{profile_name} · 认证失败，请检查凭据或网络")
        if label in {"run", "batch run"}:
            self.controller.notify(
                "failure",
                provider=self.provider,
                operation=label,
                summary={"status": "failure", "error_code": "operation_failed"},
            )
        self._set_operation_running(False)
        self.cancel_event = None

    @staticmethod
    def _operation_title(label: str) -> str:
        return {
            "health": "连接检查",
            "authenticate": "认证 / 登录",
            "courses": "读取课程",
            "tasks": "读取任务",
            "questions": "扫描题目",
            "duration": "读取时长",
            "inspect": "读取任务详情",
            "run": "执行任务",
            "batch run": "批量执行",
            "scan all": "全量扫描",
            "scan profiles": "批量扫描账号",
            "scan status": "读取扫描状态",
            "prepare draft": "准备草稿",
            "prepare drafts": "准备草稿",
            "install donor": "准备外部组件",
            "oauth begin": "开始授权",
            "oauth exchange": "完成授权",
        }.get(label, label)

    def _set_home_activity(
        self,
        operation: str,
        *,
        current: object | None = None,
        total: object | None = None,
        message: str = "",
        finished: bool = False,
    ) -> None:
        window = self.window()
        if not hasattr(window, "home_page"):
            return
        profile = self.profile_combo.currentText().strip()
        context = self._activity_context()
        text = f"{self.provider_label} · {profile} · {operation}"
        if context:
            text += f"\n{context}"
        if message:
            text += f"\n{self._preview_text(message, limit=180)}"
        window.home_page.set_activity(
            text,
            current=current,
            total=total,
            finished=finished,
            activity_id=f"{self.provider}:{self.profile_combo.currentData() or 'global'}",
        )

    def _activity_context(self) -> str:
        task = self._selected_task()
        if isinstance(task, dict):
            title = str(task.get("title") or task.get("name") or "").strip()
            if title:
                return f"任务：{self._preview_text(title, limit=120)}"
        selected_course = self._selected_course()
        if selected_course is not None:
            _row, course = selected_course
            if isinstance(course, dict):
                title = str(course.get("title") or course.get("name") or "").strip()
                if title:
                    return f"课程：{self._preview_text(title, limit=120)}"
        return ""

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
        if hasattr(value, "__dict__"):
            return ProviderPage._safe_preview(vars(value), depth=depth + 1)
        if isinstance(value, str):
            return ProviderPage._redact_text(value)
        return value

    @staticmethod
    def _redact_text(value: str) -> str:
        """Remove credential-shaped values from untrusted Worker messages and errors."""
        return redact_worker_text(value)

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
            self.log.append("[当前操作] 已请求取消")
            self.cancel_button.setEnabled(False)
            self.cancel_button.setText("已请求取消")

    def health(self) -> None:
        self._call(
            lambda on_event: self.controller.health(self.provider, on_event=on_event), "health"
        )

    def install_donor(self) -> None:
        if self.provider != "welearn":
            return
        if not ask_confirmation(
            self,
            "Welearn",
            "将从固定版本获取外部组件。是否允许使用系统 Git 或联网下载？",
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
            self._refresh_empty_states()

    @staticmethod
    def _select_all_rows(table: TableWidget) -> None:
        table.selectAll()

    def _clear_task_selection(self) -> None:
        self.task_table.clearSelection()
        self.formal_table.clearSelection()

    def _ask_concurrency(self) -> int | None:
        """Read a positive worker count without imposing a product cap."""
        value, accepted = ask_text(self, self.provider_label, "并发数", "1")
        if not accepted:
            return None
        try:
            concurrency = int(value.strip())
        except (TypeError, ValueError):
            show_notice(self, self.provider_label, "并发数必须是正整数", "warning")
            return None
        if concurrency < 1:
            show_notice(self, self.provider_label, "并发数必须是正整数", "warning")
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

    @staticmethod
    def _display_rich_content(value: Any, *, limit: int = 500) -> str:
        """Present mixed text/media without exposing provider field names or identifiers."""
        parts: list[str] = []

        def visit(item: Any, depth: int = 0) -> None:
            if depth > 5 or item is None:
                return
            if isinstance(item, str):
                text = " ".join(item.split())
                if text:
                    parts.append(text)
                return
            if isinstance(item, (int, float, bool)):
                parts.append(str(item))
                return
            if isinstance(item, (list, tuple)):
                for child in item:
                    visit(child, depth + 1)
                return
            if not isinstance(item, dict):
                return
            for key, marker in {
                "image": "[图片]",
                "images": "[图片]",
                "audio": "[音频]",
                "video": "[视频]",
                "file": "[附件]",
                "attachment": "[附件]",
                "formula": "[公式]",
            }.items():
                if item.get(key):
                    parts.append(marker)
            preferred = (
                "text",
                "content",
                "label",
                "title",
                "value",
                "prompt",
                "material",
                "children",
                "parts",
            )
            visited = False
            for key in preferred:
                if key in item:
                    visit(item[key], depth + 1)
                    visited = True
            if not visited:
                for key, child in item.items():
                    lowered = str(key).casefold()
                    if lowered.endswith("id") or any(
                        marker in lowered
                        for marker in ("token", "cookie", "secret", "authorization")
                    ):
                        continue
                    visit(child, depth + 1)

        visit(value)
        text = "；".join(dict.fromkeys(part for part in parts if part))
        return ProviderPage._preview_text(text, limit=limit)

    def _selected_task(self) -> dict[str, Any] | None:
        formal_rows = self.formal_table.selectionModel().selectedRows()
        routine_rows = self.task_table.selectionModel().selectedRows()
        if len(formal_rows) + len(routine_rows) != 1:
            return None
        if formal_rows:
            formal_row = formal_rows[0].row()
            if 0 <= formal_row < len(self.current_formal_tasks):
                return self.current_formal_tasks[formal_row]
        if routine_rows:
            routine_row = routine_rows[0].row()
            if 0 <= routine_row < len(self.current_routine_tasks):
                return self.current_routine_tasks[routine_row]
        return None

    def _selected_course(self) -> tuple[int, dict[str, Any]] | None:
        rows = self.course_table.selectionModel().selectedRows()
        if not rows:
            return None
        row = rows[0].row()
        if not 0 <= row < len(self.current_courses):
            return None
        return row, self.current_courses[row]

    def sync_tasks(self) -> None:
        profile = self.profile()
        selected = self._selected_course()
        if not profile or selected is None:
            show_notice(self, self.provider_label, "请先同步并选择课程", "warning")
            return
        _row, course = selected
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
            show_notice(self, self.provider_label, "请先同步并选择任务", "warning")
            return
        allow_attempt = False
        if self.provider == "cidaren" or self._is_formal(task):
            if not ask_confirmation(
                self,
                self.provider_label,
                "读取该正式任务可能建立远端答题尝试或启动倒计时，是否继续？",
            ):
                return
            allow_attempt = True
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
            show_notice(self, self.provider_label, "请先同步并选择任务", "warning")
            return
        dialog = FluentDialogBase(
            f"{self.provider_label} 任务详情",
            self,
            confirm_text="关闭",
            show_cancel=False,
        )
        native = task.get("native")
        native = native if isinstance(native, dict) else {}
        rows = [
            ("任务名称", str(task.get("title") or task.get("name") or "未命名任务")),
            (
                "任务类型",
                _display_code(
                    task.get("type", task.get("task_type", task.get("source_type", ""))),
                    _TASK_TYPE_LABELS,
                    "平台原生任务",
                ),
            ),
            ("任务状态", _display_code(task.get("state"), _STATE_LABELS, "未知")),
            ("截止时间", str(task.get("deadline") or "平台未提供")),
            ("任务位置", str(native.get("provider_position") or "平台未提供")),
            (
                "可用操作",
                "、".join(
                    {"questions": "读取题目", "run": "执行"}.get(
                        str(item), "平台原生操作"
                    )
                    for item in task.get("capabilities", [])
                )
                or "平台未提供",
            ),
        ]
        table = TableWidget()
        table.setColumnCount(2)
        table.setHorizontalHeaderLabels(["项目", "内容"])
        table.setRowCount(len(rows))
        for row, values in enumerate(rows):
            table.setItem(row, 0, QTableWidgetItem(values[0]))
            table.setItem(row, 1, QTableWidgetItem(values[1]))
        configure_table(table)
        dialog.content_layout.addWidget(table)
        dialog.set_content_size(760, 480)
        dialog.show()

    def show_course_detail(self) -> None:
        selected = self._selected_course()
        if selected is None:
            show_notice(self, self.provider_label, "请先同步并选择课程", "warning")
            return
        _row, course = selected
        dialog = FluentDialogBase(
            f"{self.provider_label} 课程详情",
            self,
            confirm_text="关闭",
            show_cancel=False,
        )
        dialog.content_layout.addWidget(
            StrongBodyLabel(str(course.get("title") or course.get("name") or "课程"))
        )
        summary = course.get("provider_summary")
        summary = summary if isinstance(summary, dict) else {}
        grade = summary.get("grade")
        grade = grade if isinstance(grade, dict) else {}
        rows: list[tuple[str, str]] = [
            ("课程状态", _display_code(course.get("state"), _STATE_LABELS, "未知")),
        ]
        if grade.get("overall_score") is not None:
            rows.append(("总成绩", f"{grade['overall_score']} 分"))
        components = grade.get("components")
        if isinstance(components, list):
            for component in components:
                if not isinstance(component, dict):
                    continue
                kind = str(component.get("type") or "")
                label = _GRADE_COMPONENT_LABELS.get(kind, kind or "成绩模块")
                facts = []
                for key, title, suffix in (
                    ("weight_percent", "权重", "%"),
                    ("score", "得分", " 分"),
                    ("completion_percent", "完成度", "%"),
                    ("required_minutes", "要求", " 分钟"),
                    ("observed_minutes", "当前", " 分钟"),
                    ("remaining_gap", "剩余", ""),
                ):
                    if component.get(key) is not None:
                        facts.append(f"{title} {component[key]}{suffix}")
                if component.get("completion_condition"):
                    facts.append(f"完成条件：{component['completion_condition']}")
                rows.append((label, "；".join(facts) or "平台未提供细项"))
        if len(rows) == 1:
            rows.append(("成绩构成", "平台未返回可展示的成绩构成"))
        table = TableWidget()
        table.setColumnCount(2)
        table.setHorizontalHeaderLabels(["项目", "内容"])
        table.setRowCount(len(rows))
        for row, values in enumerate(rows):
            table.setItem(row, 0, QTableWidgetItem(values[0]))
            table.setItem(row, 1, QTableWidgetItem(values[1]))
        configure_table(table)
        dialog.content_layout.addWidget(table)
        dialog.set_content_size(780, 560)
        dialog.show()

    def scan_all(self) -> None:
        profile = self.profile()
        if profile is None:
            return
        allow_attempt = False
        if profile.provider == "cidaren":
            if not ask_confirmation(
                self,
                "Cidaren",
                "全量读取课堂任务可能建立远端答题尝试，是否继续？",
            ):
                return
            allow_attempt = True
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
        dialog = FluentDialogBase(
            f"{self.provider_label} 扫描状态",
            self,
            confirm_text="关闭",
            show_cancel=False,
        )
        table = TableWidget()
        table.setColumnCount(2)
        table.setHorizontalHeaderLabels(["项目", "内容"])
        values = (
            ("状态", _display_code(status.state, _STATE_LABELS, "尚未开始")),
            ("阶段", display_scan_phase(status.phase)),
            ("课程数", status.course_count),
            ("任务数", status.task_count),
            ("题目数", status.question_count),
            ("已完成任务", status.completed_tasks),
            ("重试次数", status.retries),
            ("最近错误", self._redact_text(status.last_error or "")),
            ("更新时间", status.updated_at),
        )
        table.setRowCount(len(values))
        for row, (key, value) in enumerate(values):
            table.setItem(row, 0, QTableWidgetItem(str(key)))
            table.setItem(row, 1, QTableWidgetItem(str(value)))
        configure_table(table)
        dialog.content_layout.addWidget(table)
        retry = PushButton("重新扫描当前账号")
        retry.clicked.connect(lambda: (dialog.close(), self.scan_all()))
        dialog.content_layout.addWidget(retry)
        dialog.set_content_size(720, 520)
        dialog.show()

    def scan_profiles(self) -> None:
        if self.provider != "chaoxing":
            return
        if not ask_confirmation(
            self,
            "Chaoxing",
            "按本地账号逐个执行可恢复的只读全量扫描？失败账号会记录并继续。",
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
            show_notice(self, self.provider_label, "请先同步并选择任务", "warning")
            return
        if self._is_formal(task):
            if not ask_confirmation(
                self,
                self.provider_label,
                "读取正式任务可能建立答题尝试或启动倒计时。继续后仅生成预填草稿，"
                "不会最终提交；提交仍需在草稿页面二次确认。是否继续？",
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
        return str(self.execution_combination.currentData() or "")

    def _execution_settings(self, combination: str | None = None) -> dict[str, Any]:
        selected = combination or self._execution_combination()
        settings = {"answer_combination": selected}
        models = self.controller.config.ensure().get("models", {})
        combinations = models.get("combinations", {}) if isinstance(models, dict) else {}
        combination_value = combinations.get(selected, {}) if isinstance(combinations, dict) else {}
        challenge = (
            combination_value.get("challenge", {}) if isinstance(combination_value, dict) else {}
        )
        if isinstance(challenge, dict):
            settings["challenge_retry_attempts"] = challenge.get("normal_attempts", 3)
            settings["challenge_escalation_attempts"] = challenge.get("escalation_attempts", 1)
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
            show_notice(self, self.provider_label, "请先选择一个或多个任务", "warning")
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
        if formal and not ask_confirmation(
            self,
            self.provider_label,
            f"读取选中的 {len(formal)} 个正式任务可能建立答题尝试或启动倒计时。"
            "继续后仅生成预填草稿，不会最终提交；提交仍需在草稿页面二次确认。是否继续？",
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
            show_notice(self, self.provider_label, "请先同步并选择任务", "warning")
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
            show_notice(self, self.provider_label, "请先同步并选择任务", "warning")
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
        callback_url, accepted = ask_text(self, "Cidaren", "粘贴微信确认后的回调链接")
        if accepted and callback_url.strip():
            self._call(
                lambda on_event: self.controller.service.oauth_exchange(
                    profile, callback_url.strip(), on_event=on_event
                ),
                "oauth exchange",
            )
