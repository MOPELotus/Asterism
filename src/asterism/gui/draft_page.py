from __future__ import annotations

from typing import Any

from PyQt6.QtWidgets import (
    QAbstractItemView,
    QHBoxLayout,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from .common import DRAFT_STATUS_LABELS as _DRAFT_STATUS_LABELS
from .common import ERROR_CODE_LABELS as _ERROR_CODE_LABELS
from .common import (
    CallThread,
    ask_confirmation,
    display_provider,
    make_title,
    redact_text,
    show_notice,
)
from .common import (
    display_code as _display_code,
)
from .controller import DesktopController
from .draft_editor import FormalDraftEditor
from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    PrimaryPushButton,
    PushButton,
    ScrollArea,
    StrongBodyLabel,
    TableWidget,
    TextEdit,
    configure_scroll_area,
    configure_table,
)


class DraftPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        self.current_rows: list[dict[str, Any]] = []
        self.worker_thread: CallThread | None = None
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
        intro_layout.setSpacing(5)
        intro_layout.addWidget(make_title("作业与考试草稿"))
        intro_layout.addWidget(BodyLabel("这里集中显示需要人工确认的正式作业和考试。"))
        intro_layout.addWidget(
            CaptionLabel(
                "编辑和保存不会自动提交；只有点击“确认并提交”并再次确认后才会调用平台提交。"
            )
        )
        root.addWidget(intro)
        table_card = CardWidget()
        table_layout = QVBoxLayout(table_card)
        table_layout.setContentsMargins(16, 14, 16, 14)
        table_layout.addWidget(StrongBodyLabel("待处理草稿"))
        self.table = TableWidget()
        self.table.setColumnCount(5)
        self.table.setHorizontalHeaderLabels(["平台", "账号", "任务", "状态", "更新时间"])
        configure_table(self.table)
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        table_layout.addWidget(self.table)
        self.empty_state = CaptionLabel("暂无待处理草稿。")
        table_layout.addWidget(self.empty_state)
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
        self.action_buttons = [refresh, edit, save_remote, submit, discard]
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
            task_title = str(value.get("task_title") or "待确认任务")
            visible = (
                display_provider(provider),
                profile_label,
                task_title,
                _display_code(value.get("status"), _DRAFT_STATUS_LABELS),
                value.get("updated_at"),
            )
            for column, item in enumerate(visible):
                self.table.setItem(row, column, QTableWidgetItem(str(item or "")))
        self.empty_state.setVisible(not self.current_rows)

    def _selected(self):
        rows = self.table.selectionModel().selectedRows()
        if not rows:
            show_notice(self, "草稿", "请先选择草稿", "warning")
            return None
        row = rows[0].row()
        if row < 0 or row >= len(self.current_rows):
            show_notice(self, "草稿", "所选草稿已不存在，请刷新后重试", "warning")
            return None
        value = self.current_rows[row]
        try:
            return self.controller.load_draft(
                str(value["provider"]), str(value["profile_id"]), str(value["id"])
            )
        except (OSError, TypeError, ValueError, KeyError) as error:
            show_notice(self, "草稿", str(error), "error")
            return None

    def _operation_running(self) -> bool:
        return self.worker_thread is not None and self.worker_thread.isRunning()

    def _refuse_if_busy(self) -> bool:
        if not self._operation_running():
            return False
        show_notice(self, "草稿", "已有草稿保存或提交操作正在运行")
        return True

    def _set_busy(self, busy: bool) -> None:
        for button in self.action_buttons:
            button.setEnabled(not busy)

    def edit_selected(self) -> None:
        if self._refuse_if_busy():
            return
        draft = self._selected()
        if draft is None:
            return
        if draft.status != "draft":
            show_notice(self, "草稿", "只有待确认草稿可以编辑")
            return
        dialog = FormalDraftEditor(draft.payload, self)
        dialog.setWindowTitle("编辑草稿")

        def save_payload(payload: object) -> None:
            try:
                if not isinstance(payload, dict):
                    raise ValueError("草稿内容必须是 JSON 对象")
                self.controller.update_draft(draft, payload)
                self.log.append("[草稿] 草稿已保存")
                self.reload()
                dialog.accept()
            except (OSError, TypeError, ValueError) as error:
                show_notice(dialog, "草稿", str(error), "error")

        dialog.saved.connect(save_payload)
        dialog.show()

    def submit_selected(self) -> None:
        if self._refuse_if_busy():
            return
        draft = self._selected()
        if draft is None:
            return
        if draft.status != "draft":
            show_notice(self, "草稿", "只有草稿状态可以提交")
            return
        if not ask_confirmation(self, "草稿", "将使用当前草稿答案调用平台原生提交，确认继续？"):
            return
        self.log.append("[草稿] 正在提交草稿")
        self._set_busy(True)
        self.worker_thread = CallThread(lambda _on_event: self.controller.submit_draft(draft))
        self.worker_thread.succeeded.connect(self._submit_succeeded)
        self.worker_thread.failed.connect(self._operation_failed)
        self.worker_thread.start()

    def save_selected_to_provider(self) -> None:
        if self._refuse_if_busy():
            return
        draft = self._selected()
        if draft is None:
            return
        if draft.provider != "chaoxing":
            show_notice(self, "草稿", "该平台没有已确认的只保存接口")
            return
        if draft.status != "draft":
            show_notice(self, "草稿", "只有草稿状态可以保存")
            return
        if not ask_confirmation(self, "草稿", "将当前答案保存到平台，但不执行最终提交。确认继续？"):
            return
        self.log.append("[草稿] 正在保存到平台（不提交）")
        self._set_busy(True)
        self.worker_thread = CallThread(
            lambda _on_event: self.controller.save_draft_to_provider(draft)
        )
        self.worker_thread.succeeded.connect(self._save_remote_succeeded)
        self.worker_thread.failed.connect(self._operation_failed)
        self.worker_thread.start()

    def _submit_succeeded(self, _result: Any) -> None:
        self._set_busy(False)
        self.log.append("[草稿] 已提交")
        self.reload()

    def _save_remote_succeeded(self, _result: Any) -> None:
        self._set_busy(False)
        self.log.append("[草稿] 已保存到平台，未最终提交")

    def _operation_failed(self, error: object) -> None:
        self._set_busy(False)
        code = str(getattr(error, "code", "operation_failed") or "operation_failed")
        category = _display_code(code, _ERROR_CODE_LABELS, "操作失败")
        self.log.append(f"[草稿] {category}：{redact_text(str(error))}".rstrip("："))

    def discard_selected(self) -> None:
        if self._refuse_if_busy():
            return
        draft = self._selected()
        if draft is None:
            return
        if draft.status != "draft":
            show_notice(self, "草稿", "只有待确认草稿可以丢弃")
            return
        if ask_confirmation(self, "草稿", "确认丢弃这份草稿？"):
            self.controller.drafts.set_status(draft, "discarded")
            self.reload()
