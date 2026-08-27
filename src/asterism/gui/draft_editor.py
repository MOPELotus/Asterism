from __future__ import annotations

import json
from copy import deepcopy
from typing import Any

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QButtonGroup,
    QFormLayout,
    QHBoxLayout,
    QListWidgetItem,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from .common import make_title, show_notice
from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    CheckBox,
    ComboBox,
    FluentDialogBase,
    LineEdit,
    ListWidget,
    PrimaryPushButton,
    PushButton,
    RadioButton,
    ScrollArea,
    StrongBodyLabel,
    TableWidget,
    TextEdit,
    configure_scroll_area,
    configure_table,
    form_label,
)

QUESTION_KIND_LABELS = {
    "composite": "复合题",
    "discussion": "讨论题",
    "essay": "作文题",
    "fill_blank": "填空题",
    "long_answer": "主观题",
    "matching": "连线题",
    "multiple_choice": "多选题",
    "ordering": "排序题",
    "provider_native": "平台原生题型",
    "short_answer": "简答题",
    "single_choice": "单选题",
    "subjective": "主观题",
    "true_false": "判断题",
}

SUBJECTIVE_KINDS = {
    "discussion",
    "essay",
    "long_answer",
    "short_answer",
    "subjective",
}


def _display_content(value: Any) -> str:
    """Render rich option content without exposing Provider identifiers."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    if isinstance(value, (int, float, bool)):
        return str(value)
    if isinstance(value, list):
        return " · ".join(filter(None, (_display_content(item) for item in value)))
    if isinstance(value, dict):
        parts: list[str] = []
        for key in ("text", "content", "label", "title", "prompt", "alt"):
            text = _display_content(value.get(key))
            if text and text not in parts:
                parts.append(text)
        for key, marker in (
            ("image", "[图片]"),
            ("image_url", "[图片]"),
            ("audio", "[音频]"),
            ("video", "[视频]"),
            ("file", "[附件]"),
            ("attachment", "[附件]"),
        ):
            if value.get(key) and marker not in parts:
                parts.append(marker)
        if not parts:
            ignored = {"id", "key", "code", "value", "letter", "answer_tag"}
            parts.extend(
                text
                for key, child in value.items()
                if str(key).casefold() not in ignored and (text := _display_content(child))
            )
        return " ".join(parts) or "[富媒体选项]"
    return str(value)


def _option_rows(options: Any) -> list[tuple[str, str]]:
    """Return the current question's submit binding and a human label."""
    if isinstance(options, dict):
        items = [(str(key), value) for key, value in options.items()]
    elif isinstance(options, list):
        items = []
        for index, value in enumerate(options):
            default = chr(ord("A") + index) if index < 26 else str(index + 1)
            explicit = None
            if isinstance(value, dict):
                explicit = next(
                    (
                        value.get(key)
                        for key in ("key", "letter", "code", "answer_tag")
                        if value.get(key) not in (None, "")
                    ),
                    None,
                )
            binding = str(explicit or default)
            items.append((binding, value))
    else:
        return []
    rows = []
    for binding, value in items:
        content = _display_content(value)
        label = f"{binding}. {content}" if content else binding
        # The cache is content-addressed, but the resolved answer has already
        # been rebound to this task's current option key.  Preserve that key at
        # the UI/Worker boundary so shuffled A/B order remains unambiguous.
        rows.append((binding, label))
    return rows


class AnswerValueEditor(QWidget):
    """Fluent, type-aware editor for one formal-task answer."""

    def __init__(self, question: dict[str, Any], current: Any = None, parent=None):
        super().__init__(parent)
        self.question = question
        self.kind = str(question.get("kind") or "provider_native").casefold()
        self.current = current
        self._mode = "text"
        self._choice_buttons: list[tuple[Any, str]] = []
        self._blank_fields: list[LineEdit] = []
        self._matching_fields: list[tuple[str, ComboBox]] = []

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(10)
        prompt = question.get("prompt") or question.get("question") or question.get("stem")
        prompt_label = BodyLabel(_display_content(prompt) or "（题干未提供）")
        prompt_label.setWordWrap(True)
        layout.addWidget(prompt_label)
        material = question.get("material") or question.get("shared_material")
        material_text = _display_content(material)
        if material_text:
            material_label = CaptionLabel(f"材料：{material_text}")
            material_label.setWordWrap(True)
            layout.addWidget(material_label)

        options = _option_rows(question.get("options") or question.get("choices") or [])
        if self.kind in {"single_choice", "true_false"}:
            self._build_single(layout, options)
        elif self.kind == "multiple_choice":
            self._build_multiple(layout, options)
        elif self.kind == "fill_blank":
            self._build_blanks(layout)
        elif self.kind == "matching":
            self._build_matching(layout, options)
        elif self.kind == "ordering":
            self._build_ordering(layout, options)
        else:
            self._build_text(layout)

    @staticmethod
    def _same(left: Any, right: Any) -> bool:
        if isinstance(left, bool):
            left = "true" if left else "false"
        if isinstance(right, bool):
            right = "true" if right else "false"
        return str(left).strip().casefold() == str(right).strip().casefold()

    @classmethod
    def _matches_option(cls, current: Any, submit_value: Any, label: str) -> bool:
        content = label.split(". ", 1)[1] if ". " in label else label
        return cls._same(current, submit_value) or cls._same(current, content)

    def _current_values(self) -> list[Any]:
        if isinstance(self.current, list):
            return self.current
        if self.current in (None, ""):
            return []
        return [self.current]

    def _build_single(self, layout: QVBoxLayout, options: list[tuple[str, str]]) -> None:
        self._mode = "single"
        if self.kind == "true_false" and not options:
            options = [("true", "正确"), ("false", "错误")]
        if not options:
            self._build_text(layout)
            return
        group = QButtonGroup(self)
        self._single_group = group
        current = self._current_values()
        for submit_value, label in options:
            button = RadioButton(label)
            group.addButton(button)
            if current and self._matches_option(current[0], submit_value, label):
                button.setChecked(True)
            self._choice_buttons.append((button, submit_value))
            layout.addWidget(button)

    def _build_multiple(self, layout: QVBoxLayout, options: list[tuple[str, str]]) -> None:
        self._mode = "multiple"
        if not options:
            self._build_text(layout)
            return
        current = self._current_values()
        for submit_value, label in options:
            button = CheckBox(label)
            button.setChecked(
                any(self._matches_option(value, submit_value, label) for value in current)
            )
            self._choice_buttons.append((button, submit_value))
            layout.addWidget(button)

    def _build_blanks(self, layout: QVBoxLayout) -> None:
        self._mode = "blanks"
        shape = self.question.get("native_shape")
        blank_count = int(shape.get("blank_count") or 0) if isinstance(shape, dict) else 0
        current = self._current_values()
        blank_count = max(1, blank_count, len(current))
        form = QFormLayout()
        for index in range(blank_count):
            field = LineEdit()
            field.setPlaceholderText(f"第 {index + 1} 空")
            if index < len(current):
                field.setText(str(current[index]))
            self._blank_fields.append(field)
            form.addRow(form_label(f"第 {index + 1} 空"), field)
        layout.addLayout(form)

    def _build_matching(
        self, layout: QVBoxLayout, fallback_options: list[tuple[str, str]]
    ) -> None:
        native = self.question.get("native")
        groups = native.get("matching_groups") if isinstance(native, dict) else None
        left = groups.get("left") if isinstance(groups, dict) else None
        right = groups.get("right") if isinstance(groups, dict) else None
        if not isinstance(left, list) or not isinstance(right, list) or not left or not right:
            self._build_text(layout)
            return
        self._mode = "matching"
        form = QFormLayout()
        current = self.current if isinstance(self.current, dict) else {}
        right_rows = _option_rows(right)
        if not right_rows:
            right_rows = fallback_options[len(fallback_options) // 2 :]
        for raw_left in left:
            left_text = _display_content(raw_left)
            combo = ComboBox()
            combo.addItem("请选择", userData=None)
            for submit_value, label in right_rows:
                combo.addItem(label, userData=submit_value)
            wanted = current.get(left_text)
            selected = combo.findData(wanted)
            if selected < 0 and wanted not in (None, ""):
                selected = next(
                    (
                        index
                        for index in range(combo.count())
                        if self._matches_option(
                            wanted, combo.itemData(index), combo.itemText(index)
                        )
                    ),
                    -1,
                )
            if selected >= 0:
                combo.setCurrentIndex(selected)
            self._matching_fields.append((left_text, combo))
            form.addRow(form_label(left_text), combo)
        layout.addLayout(form)

    def _build_ordering(
        self, layout: QVBoxLayout, options: list[tuple[str, str]]
    ) -> None:
        self._mode = "ordering"
        if not options and not isinstance(self.current, list):
            self._build_text(layout)
            return
        layout.addWidget(CaptionLabel("选中一项后使用“上移”或“下移”调整为正确顺序。"))
        self.ordering_list = ListWidget()
        ordered_rows: list[tuple[str, str]] = []
        used: set[int] = set()
        current_values = self.current if isinstance(self.current, list) else []
        for current in current_values:
            match = next(
                (
                    (index, submit_value, label)
                    for index, (submit_value, label) in enumerate(options)
                    if index not in used and self._matches_option(current, submit_value, label)
                ),
                None,
            )
            if match is not None:
                index, submit_value, label = match
                used.add(index)
                ordered_rows.append((submit_value, label))
            else:
                ordered_rows.append((str(current), _display_content(current)))
        ordered_rows.extend(row for index, row in enumerate(options) if index not in used)
        for submit_value, label in ordered_rows:
            item = QListWidgetItem(label or str(submit_value))
            item.setData(Qt.ItemDataRole.UserRole, submit_value)
            self.ordering_list.addItem(item)
        layout.addWidget(self.ordering_list, 1)
        actions = QHBoxLayout()
        move_up = PushButton("上移")
        move_up.clicked.connect(lambda: self._move_ordering_item(-1))
        actions.addWidget(move_up)
        move_down = PushButton("下移")
        move_down.clicked.connect(lambda: self._move_ordering_item(1))
        actions.addWidget(move_down)
        actions.addStretch(1)
        layout.addLayout(actions)

    def _move_ordering_item(self, offset: int) -> None:
        row = self.ordering_list.currentRow()
        target = row + offset
        if row < 0 or target < 0 or target >= self.ordering_list.count():
            return
        item = self.ordering_list.takeItem(row)
        self.ordering_list.insertItem(target, item)
        self.ordering_list.setCurrentRow(target)

    def _build_text(self, layout: QVBoxLayout) -> None:
        self._mode = "text"
        self.text = TextEdit()
        if self.current not in (None, ""):
            self.text.setPlainText(
                self.current
                if isinstance(self.current, str)
                else json.dumps(self.current, ensure_ascii=False, indent=2)
            )
        if self.kind not in SUBJECTIVE_KINDS:
            self.text.setPlaceholderText("填写平台可接受的答案；多项内容可每行填写一项。")
        layout.addWidget(self.text, 1)

    def value(self) -> Any:
        if self._mode == "single":
            return next(
                (value for button, value in self._choice_buttons if button.isChecked()), None
            )
        if self._mode == "multiple":
            values = [value for button, value in self._choice_buttons if button.isChecked()]
            return values or None
        if self._mode == "blanks":
            values = [field.text().strip() for field in self._blank_fields]
            if not any(values):
                return None
            return values[0] if len(values) == 1 else values
        if self._mode == "matching":
            values = {
                left: combo.currentData()
                for left, combo in self._matching_fields
                if combo.currentData() not in (None, "")
            }
            return values or None
        if self._mode == "ordering":
            return [
                self.ordering_list.item(index).data(Qt.ItemDataRole.UserRole)
                for index in range(self.ordering_list.count())
            ] or None
        text = self.text.toPlainText().strip()
        if not text:
            return None
        return FormalDraftEditor._parse_answer(text, self.kind)


def draft_notice(parent, title: str, message: str, *, error: bool = False) -> None:
    show_notice(parent, title, message, "error" if error else "warning")


class FormalDraftEditor(FluentDialogBase):
    saved = pyqtSignal(object)

    def __init__(self, payload: dict[str, Any], parent=None):
        super().__init__("作业 / 考试草稿", parent, show_confirm=False, show_cancel=False)
        self.payload = deepcopy(payload)
        self.questions = [
            dict(item) for item in payload.get("questions", []) if isinstance(item, dict)
        ]
        self.answers = self._answer_map(payload.get("answers"))
        self.set_content_size(980, 680)
        root = self.content_layout
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(12)
        intro = CardWidget()
        intro_layout = QVBoxLayout(intro)
        intro_layout.setContentsMargins(18, 14, 18, 14)
        intro_layout.setSpacing(4)
        intro_layout.addWidget(make_title("逐题确认与补漏"))
        intro_layout.addWidget(
            BodyLabel("选择、填空、连线和排序题使用对应编辑器；主观题填写普通纯文本。")
        )
        intro_layout.addWidget(CaptionLabel("这里只保存本地草稿，不会自动提交到平台。"))
        root.addWidget(intro)
        table_card = CardWidget()
        table_layout = QVBoxLayout(table_card)
        table_layout.setContentsMargins(14, 12, 14, 12)
        table_layout.addWidget(StrongBodyLabel("题目列表"))
        self.table = TableWidget()
        self.table.setColumnCount(5)
        self.table.setHorizontalHeaderLabels(["序号", "题型", "题干", "答案", "状态"])
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        configure_table(self.table)
        table_layout.addWidget(self.table, 1)
        self.empty_state = CaptionLabel("草稿中没有可编辑的题目。")
        table_layout.addWidget(self.empty_state)
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
        cancel = PushButton("取消")
        cancel.clicked.connect(self.reject)
        actions.addWidget(cancel)
        save = PrimaryPushButton("保存草稿")
        save.clicked.connect(self.save_payload)
        actions.addWidget(save)
        root.addLayout(actions)
        self.reload()

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

    @staticmethod
    def _parse_answer(text: str, kind: str) -> Any:
        stripped = text.strip()
        if not stripped:
            return None
        if kind.casefold() in SUBJECTIVE_KINDS:
            return stripped
        try:
            return json.loads(stripped)
        except json.JSONDecodeError:
            return stripped

    def reload(self) -> None:
        self.table.setRowCount(len(self.questions))
        for row, question in enumerate(self.questions):
            remote_id = str(question.get("remote_id") or "")
            prompt = question.get("prompt") or question.get("question") or question.get("stem")
            answer_present = remote_id in self.answers and self.answers[remote_id] is not None
            values = (
                str(row + 1),
                QUESTION_KIND_LABELS.get(
                    str(question.get("kind") or "provider_native").casefold(),
                    "平台原生题型",
                ),
                _display_content(prompt),
                self._preview(self.answers[remote_id]) if answer_present else "",
                "已有答案" if answer_present else "待补充",
            )
            for column, value in enumerate(values):
                self.table.setItem(row, column, QTableWidgetItem(value))
        self.empty_state.setVisible(not self.questions)

    def _selected_question(self) -> dict[str, Any] | None:
        selected = self.table.selectionModel().selectedRows()
        row = selected[0].row() if selected else -1
        if row < 0 or row >= len(self.questions):
            draft_notice(self, "草稿", "请先选择题目")
            return None
        return self.questions[row]

    def edit_selected(self) -> None:
        question = self._selected_question()
        if question is None:
            return
        remote_id = str(question.get("remote_id") or "")
        if not remote_id:
            draft_notice(self, "草稿", "该题缺少平台题目标识，无法安全编辑", error=True)
            return
        dialog = FluentDialogBase("编辑答案", self, confirm_text="应用答案")
        scroll = ScrollArea()
        configure_scroll_area(scroll)
        editor = AnswerValueEditor(question, self.answers.get(remote_id))
        scroll.setWidget(editor)
        scroll.setWidgetResizable(True)
        dialog.content_layout.addWidget(scroll, 1)

        def apply() -> bool:
            value = editor.value()
            if value is None:
                self.answers.pop(remote_id, None)
            else:
                self.answers[remote_id] = value
            self.reload()
            return True

        dialog.set_validator(apply)
        dialog.set_content_size(680, 440)
        dialog.exec()

    def clear_selected(self) -> None:
        question = self._selected_question()
        if question is None:
            return
        remote_id = str(question.get("remote_id") or "")
        if not remote_id:
            draft_notice(self, "草稿", "该题缺少平台题目标识，无法安全编辑", error=True)
            return
        self.answers.pop(remote_id, None)
        self.reload()

    def edit_advanced(self) -> None:
        dialog = FluentDialogBase("高级草稿 JSON", self, confirm_text="应用 JSON")
        editor = TextEdit()
        editor.setPlainText(json.dumps(self.current_payload(), ensure_ascii=False, indent=2))
        dialog.content_layout.addWidget(editor)

        def apply_json() -> bool:
            try:
                value = json.loads(editor.toPlainText())
                if not isinstance(value, dict):
                    raise ValueError("草稿必须是 JSON 对象")
                questions = value.get("questions")
                if not isinstance(questions, list):
                    raise ValueError("草稿 questions 必须是 JSON 数组")
                self.payload = value
                self.questions = [dict(item) for item in questions if isinstance(item, dict)]
                self.answers = self._answer_map(value.get("answers"))
            except (TypeError, ValueError, json.JSONDecodeError) as error:
                draft_notice(dialog, "草稿", str(error), error=True)
                return False
            self.reload()
            return True

        dialog.set_validator(apply_json)
        dialog.set_content_size(820, 620)
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
