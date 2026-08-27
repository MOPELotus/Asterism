from __future__ import annotations

import json
from copy import deepcopy
from typing import Any

from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QDialog,
    QHBoxLayout,
    QMessageBox,
    QTableWidgetItem,
    QVBoxLayout,
)

from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    PrimaryPushButton,
    PushButton,
    StrongBodyLabel,
    TableWidget,
    TextEdit,
    TitleLabel,
    configure_table,
)


class FormalDraftEditor(QDialog):
    saved = pyqtSignal(object)

    def __init__(self, payload: dict[str, Any], parent=None):
        super().__init__(parent)
        self.payload = deepcopy(payload)
        self.questions = [
            dict(item) for item in payload.get("questions", []) if isinstance(item, dict)
        ]
        self.answers = self._answer_map(payload.get("answers"))
        self.setWindowTitle("作业 / 考试草稿")
        self.setMinimumSize(860, 560)
        root = QVBoxLayout(self)
        root.setContentsMargins(24, 20, 24, 22)
        root.setSpacing(12)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(18, 14, 18, 14)
        intro_layout.setSpacing(4)
        intro_layout.addWidget(TitleLabel("逐题确认与补漏"))
        intro_layout.addWidget(
            BodyLabel("选择、连线、排序等结构化答案填写 JSON；主观题填写普通纯文本。")
        )
        intro_layout.addWidget(CaptionLabel("这里只保存本地草稿，不会自动提交到平台。"))
        root.addWidget(intro)
        table_card = CardWidget()
        table_layout = QVBoxLayout(table_card)
        table_layout.setContentsMargins(14, 12, 14, 12)
        table_layout.addWidget(StrongBodyLabel("题目列表"))
        self.table = TableWidget()
        self.table.setColumnCount(5)
        self.table.setHorizontalHeaderLabels(["remote_id", "kind", "prompt", "answer", "state"])
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        configure_table(self.table)
        table_layout.addWidget(self.table, 1)
        root.addWidget(table_card, 1)
        actions = QHBoxLayout()
        edit = PrimaryPushButton("编辑所选答案")
        edit.clicked.connect(self.edit_selected)
        actions.addWidget(edit)
        clear = PushButton("清除所选答案")
        clear.clicked.connect(self.clear_selected)
        actions.addWidget(clear)
        advanced = PushButton("高级 JSON")
        advanced.clicked.connect(self.edit_advanced)
        actions.addWidget(advanced)
        actions.addStretch(1)
        save = PrimaryPushButton("保存草稿")
        save.clicked.connect(self.save_payload)
        actions.addWidget(save)
        root.addLayout(actions)
        self.reload()
        self.resize(980, 680)

    @staticmethod
    def _answer_map(value: Any) -> dict[str, Any]:
        if isinstance(value, dict):
            nested = value.get("rows", value.get("items", value.get("answers")))
            if isinstance(nested, list):
                value = nested
            elif "remote_id" not in value:
                return {str(key): child for key, child in value.items()}
            else:
                value = [value]
        if not isinstance(value, list):
            return {}
        return {
            str(row["remote_id"]): row.get("value")
            for row in value
            if isinstance(row, dict) and str(row.get("remote_id") or "").strip()
        }

    @staticmethod
    def _preview(value: Any, limit: int = 180) -> str:
        text = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
        return text if len(text) <= limit else text[: limit - 1] + "…"

    def reload(self) -> None:
        self.table.setRowCount(len(self.questions))
        for row, question in enumerate(self.questions):
            remote_id = str(question.get("remote_id") or "")
            prompt = question.get("prompt") or question.get("question") or question.get("stem")
            answer_present = remote_id in self.answers and self.answers[remote_id] is not None
            values = (
                remote_id,
                str(question.get("kind") or "provider_native"),
                self._preview(prompt),
                self._preview(self.answers[remote_id]) if answer_present else "",
                "resolved" if answer_present else "unresolved",
            )
            for column, value in enumerate(values):
                self.table.setItem(row, column, QTableWidgetItem(value))

    def _selected_question(self) -> dict[str, Any] | None:
        row = self.table.currentRow()
        if row < 0 or row >= len(self.questions):
            QMessageBox.warning(self, "formal draft", "请先选择题目")
            return None
        return self.questions[row]

    def edit_selected(self) -> None:
        question = self._selected_question()
        if question is None:
            return
        remote_id = str(question.get("remote_id") or "")
        dialog = QDialog(self)
        dialog.setWindowTitle(f"编辑答案 · {remote_id}")
        layout = QVBoxLayout(dialog)
        editor = TextEdit()
        if remote_id in self.answers:
            value = self.answers[remote_id]
            editor.setPlainText(
                value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, indent=2)
            )
        layout.addWidget(editor)
        save = PrimaryPushButton("应用答案")
        layout.addWidget(save)

        def apply() -> None:
            text = editor.toPlainText().strip()
            if not text:
                value = None
            else:
                try:
                    value = json.loads(text)
                except json.JSONDecodeError:
                    value = text
            if value is None:
                self.answers.pop(remote_id, None)
            else:
                self.answers[remote_id] = value
            dialog.accept()
            self.reload()

        save.clicked.connect(apply)
        dialog.resize(680, 440)
        dialog.exec()

    def clear_selected(self) -> None:
        question = self._selected_question()
        if question is None:
            return
        self.answers.pop(str(question.get("remote_id") or ""), None)
        self.reload()

    def edit_advanced(self) -> None:
        dialog = QDialog(self)
        dialog.setWindowTitle("高级草稿 JSON")
        layout = QVBoxLayout(dialog)
        editor = TextEdit()
        editor.setPlainText(json.dumps(self.current_payload(), ensure_ascii=False, indent=2))
        layout.addWidget(editor)
        apply = PrimaryPushButton("应用 JSON")
        layout.addWidget(apply)

        def apply_json() -> None:
            try:
                value = json.loads(editor.toPlainText())
                if not isinstance(value, dict):
                    raise ValueError("草稿必须是 JSON object")
                questions = value.get("questions")
                if not isinstance(questions, list):
                    raise ValueError("草稿 questions 必须是 array")
                self.payload = value
                self.questions = [dict(item) for item in questions if isinstance(item, dict)]
                self.answers = self._answer_map(value.get("answers"))
            except (TypeError, ValueError, json.JSONDecodeError) as error:
                QMessageBox.critical(dialog, "formal draft", str(error))
                return
            dialog.accept()
            self.reload()

        apply.clicked.connect(apply_json)
        dialog.resize(820, 620)
        dialog.exec()

    def current_payload(self) -> dict[str, Any]:
        value = deepcopy(self.payload)
        known_ids = [
            str(question.get("remote_id"))
            for question in self.questions
            if str(question.get("remote_id") or "").strip()
        ]
        value["questions"] = deepcopy(self.questions)
        value["answers"] = [
            {"remote_id": remote_id, "value": deepcopy(self.answers[remote_id])}
            for remote_id in known_ids
            if remote_id in self.answers and self.answers[remote_id] is not None
        ]
        value["unresolved_question_ids"] = [
            remote_id
            for remote_id in known_ids
            if remote_id not in self.answers or self.answers[remote_id] is None
        ]
        return value

    def save_payload(self) -> None:
        self.saved.emit(self.current_payload())
        self.accept()
