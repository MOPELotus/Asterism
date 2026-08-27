from __future__ import annotations

import os
import re
from copy import deepcopy
from typing import Any

import httpx
from PyQt6.QtCore import QThread, pyqtSignal
from PyQt6.QtWidgets import QDialog, QFormLayout, QHBoxLayout, QVBoxLayout, QWidget

from .common import ask_confirmation, make_title, show_notice
from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    ComboBox,
    EditableComboBox,
    FluentDialogBase,
    LineEdit,
    PrimaryPushButton,
    PushButton,
    ScrollArea,
    StrongBodyLabel,
    configure_scroll_area,
    form_label,
    wrapped_caption,
)


def _title(text: str):
    return make_title(text)


def _notice(parent: QWidget, title: str, text: str) -> None:
    show_notice(parent, title, text)


def _confirm(parent: QWidget, title: str, text: str) -> bool:
    return ask_confirmation(parent, title, text)


def _models_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    return base + "/models" if base.endswith("/v1") else base + "/v1/models"


class _ScanThread(QThread):
    done = pyqtSignal(str, object)
    failed = pyqtSignal(str, str)

    def __init__(self, name: str, endpoint: dict[str, Any]):
        super().__init__()
        self.name, self.endpoint = name, endpoint

    def run(self) -> None:
        try:
            headers = {"Accept": "application/json"}
            api_key = str(self.endpoint.get("api_key") or "")
            api_key_env = str(self.endpoint.get("api_key_env") or "")
            api_key = api_key or (os.environ.get(api_key_env, "") if api_key_env else "")
            if api_key:
                headers["Authorization"] = f"Bearer {api_key}"
            response = httpx.get(
                _models_url(str(self.endpoint.get("base_url") or "")), headers=headers, timeout=15
            )
            response.raise_for_status()
            body = response.json()
            rows = body.get("data", body.get("models", [])) if isinstance(body, dict) else []
            values = sorted(
                {
                    str(row.get("id") or row.get("name"))
                    for row in rows
                    if isinstance(row, dict) and (row.get("id") or row.get("name"))
                }
            )
            if not values:
                raise RuntimeError("站点没有返回模型")
            self.done.emit(self.name, values)
        except Exception as error:
            self.failed.emit(self.name, str(error))


class _EndpointDialog(FluentDialogBase):
    def __init__(self, names: list[str], name: str, value: dict[str, Any] | None, parent=None):
        super().__init__("AI 站点", parent, confirm_text="保存")
        value = value or {}
        self.original = name
        form = QFormLayout()
        self.content_layout.addLayout(form)
        form.setContentsMargins(0, 0, 0, 0)
        self.name = LineEdit()
        self.name.setText(name)
        self.name.setPlaceholderText("例如 router_backup")
        self.url = LineEdit()
        self.url.setText(str(value.get("base_url") or ""))
        self.url.setPlaceholderText("https://example.com/v1")
        self.key = LineEdit()
        self.key.setText(str(value.get("api_key") or ""))
        self.key.setEchoMode(LineEdit.EchoMode.Password)
        self.protocol = ComboBox()
        self.protocol.addItem("Responses", userData="responses")
        self.protocol.addItem("Chat Completions", userData="chat_completions")
        self.protocol.setCurrentIndex(
            max(0, self.protocol.findData(value.get("protocol", "responses")))
        )
        form.addRow(form_label("站点标识"), self.name)
        form.addRow(form_label("API 地址"), self.url)
        form.addRow(form_label("API Key"), self.key)
        form.addRow(form_label("协议"), self.protocol)
        self.set_validator(self._validate)
        self.names = set(names)
        self.set_content_size(620, 300)

    def _validate(self) -> bool:
        name = self.name.text().strip()
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", name):
            _notice(self, "AI 站点", "站点标识只能使用小写字母、数字、下划线和连字符。")
            return False
        if name != self.original and name in self.names:
            _notice(self, "AI 站点", "该站点标识已经存在。")
            return False
        if not self.url.text().strip():
            _notice(self, "AI 站点", "请填写 API 地址。")
            return False
        return True

    def value(self) -> tuple[str, dict[str, Any]]:
        return self.name.text().strip(), {
            "base_url": self.url.text().strip(),
            "api_key": self.key.text(),
            "protocol": self.protocol.currentData(),
        }


class _CombinationNameDialog(FluentDialogBase):
    def __init__(
        self,
        names: list[str],
        suggested_id: str = "new_combo",
        suggested_display_name: str = "新组合",
        parent=None,
    ):
        super().__init__("新建答案组合", parent, confirm_text="创建")
        self.names = set(names)
        form = QFormLayout()
        self.content_layout.addLayout(form)
        form.setContentsMargins(0, 0, 0, 0)
        form.setSpacing(12)
        self.display_name = LineEdit()
        self.display_name.setText(suggested_display_name)
        self.display_name.setPlaceholderText("例如 默认、考试高正确率")
        self.combo_id = LineEdit()
        self.combo_id.setText(suggested_id)
        self.combo_id.setPlaceholderText("小写英文、数字、下划线或连字符")
        form.addRow(form_label("组合名称"), self.display_name)
        form.addRow(form_label("组合 ID"), self.combo_id)
        form.addRow(
            wrapped_caption("名称用于界面显示；ID 只用于配置引用，保存后不建议频繁修改。")
        )
        self.set_validator(self._validate)
        self.set_content_size(620, 250)

    def _validate(self) -> bool:
        display_name = self.display_name.text().strip()
        combo_id = self.combo_id.text().strip()
        if not display_name:
            _notice(self, "答案组合", "请填写组合名称。")
            return False
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", combo_id):
            _notice(
                self,
                "答案组合",
                "组合 ID 只能使用小写字母、数字、下划线和连字符，且必须以字母开头。",
            )
            return False
        if combo_id in self.names:
            _notice(self, "答案组合", "该组合 ID 已经存在，请换一个 ID。")
            return False
        return True

    def value(self) -> tuple[str, str]:
        return self.display_name.text().strip(), self.combo_id.text().strip()


class AISettingsPage(QWidget):
    EFFORTS = ("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")
    EFFORT_LABELS = {
        "none": "不启用",
        "minimal": "最少",
        "low": "低",
        "medium": "中",
        "high": "高",
        "xhigh": "很高",
        "max": "最大",
        "ultra": "超高",
    }

    def __init__(self, controller):
        super().__init__()
        self.controller = controller
        self._scan_thread = None
        self._pending_combo: dict[str, Any] | None = None
        self._pending_combo_id: str | None = None
        self._pending_combo_display_name: str | None = None
        self._current_combo_id: str | None = None
        self._condition_rules: list[dict[str, Any]] = []
        self._condition_editor_index: int | None = None
        self._condition_loading = False
        self._combo_loading = False
        self._combo_dirty = False
        self._loaded_combo_id: str | None = None
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        scroll = ScrollArea()
        scroll.setWidgetResizable(True)
        content = QWidget()
        scroll.setWidget(content)
        configure_scroll_area(scroll)
        outer.addWidget(scroll)
        root = QVBoxLayout(content)
        root.setContentsMargins(28, 24, 28, 28)
        root.setSpacing(14)
        intro = CardWidget()
        il = QVBoxLayout(intro)
        il.setContentsMargins(20, 16, 20, 16)
        il.addWidget(_title("AI 配置"))
        il.addWidget(
            BodyLabel("选择一个答案组合，再按任务条件设置站点、模型、思考等级、超时和灾备。")
        )
        il.addWidget(
            wrapped_caption(
                "内置组合可以修改；也可以复制或新建任意组合。API Key 只保存在本机。"
            )
        )
        root.addWidget(intro)
        site_card = CardWidget()
        sl = QVBoxLayout(site_card)
        sl.setContentsMargins(20, 16, 20, 16)
        sl.addWidget(StrongBodyLabel("AI 站点与模型"))
        sl.addWidget(
            wrapped_caption(
                "先添加站点并扫描模型；若站点不支持模型列表，也可以在模型框中直接输入模型 ID。"
            )
        )
        self.site_choice = ComboBox()
        sl.addWidget(self.site_choice)
        site_buttons = QHBoxLayout()
        self.site_action_buttons = []
        for text, callback in (
            ("新增站点", self.add_site),
            ("编辑站点", self.edit_site),
            ("删除站点", self.delete_site),
            ("扫描模型", self.scan_site),
        ):
            button = PrimaryPushButton(text) if text == "新增站点" else PushButton(text)
            button.clicked.connect(callback)
            site_buttons.addWidget(button)
            self.site_action_buttons.append(button)
        site_buttons.addStretch(1)
        sl.addLayout(site_buttons)
        root.addWidget(site_card)
        combo_card = CardWidget()
        cl = QVBoxLayout(combo_card)
        cl.setContentsMargins(20, 16, 20, 16)
        cl.addWidget(StrongBodyLabel("答案组合"))
        self.combo_choice = ComboBox()
        cl.addWidget(self.combo_choice)
        combo_buttons = QHBoxLayout()
        self.combo_action_buttons = []
        for text, callback in (
            ("新建组合", self.new_combo),
            ("复制当前", self.copy_combo),
            ("设为默认", self.set_default_combo),
            ("删除当前", self.delete_combo),
        ):
            button = PrimaryPushButton(text) if text == "新建组合" else PushButton(text)
            button.clicked.connect(callback)
            combo_buttons.addWidget(button)
            self.combo_action_buttons.append(button)
        combo_buttons.addStretch(1)
        cl.addLayout(combo_buttons)
        self.current_combo_label = CaptionLabel("请选择一个组合，或点击“新建组合”开始配置")
        self.current_combo_label.hide()
        cl.addWidget(self.current_combo_label)
        root.addWidget(combo_card)
        self.route_cards: dict[str, dict[str, QWidget]] = {}
        for route, label in (("timed", "当任务为限时任务时"), ("untimed", "当任务为一般任务时")):
            root.addWidget(self._build_route(route, label))
        root.addWidget(self._build_condition_card())
        root.addWidget(self._build_challenge_card())
        self.save_combo_button = PrimaryPushButton("保存当前组合")
        self.save_combo_button.clicked.connect(self.save_combo)
        root.addWidget(self.save_combo_button, 0)
        root.addStretch(1)
        self.combo_choice.currentIndexChanged.connect(self._combo_selection_changed)
        self.site_choice.currentIndexChanged.connect(self._update_action_state)
        self._wire_combo_dirty_tracking()
        self.reload()

    def _wire_combo_dirty_tracking(self) -> None:
        widgets: list[QWidget] = []
        for fields in self.route_cards.values():
            widgets.extend(fields.values())
        widgets.extend(
            (
                self.kind,
                self.kind_site,
                self.kind_model,
                self.kind_effort,
                self.kind_timeout,
                self.kind_retries,
                self.kind_fallback,
                self.kind_fallback_model,
                self.kind_fallback_effort,
                self.challenge_attempts,
                self.challenge_site,
                self.challenge_model,
                self.challenge_effort,
                self.challenge_timeout,
                self.challenge_request_retries,
                self.challenge_retries,
            )
        )
        for widget in widgets:
            signal = getattr(widget, "textChanged", None)
            if signal is None:
                signal = getattr(widget, "currentIndexChanged", None)
            if signal is not None:
                signal.connect(self._mark_combo_dirty)

    def _mark_combo_dirty(self, *_args) -> None:
        if not self._combo_loading and not self._condition_loading:
            self._combo_dirty = True

    def _allow_discard_combo_edits(self) -> bool:
        return not self._combo_dirty or _confirm(
            self,
            "未保存的答案组合",
            "当前答案组合有尚未保存的修改。继续会丢弃这些修改，是否继续？",
        )

    def _combo_selection_changed(self, _index: int = -1) -> None:
        target = self._selected_value(
            self.combo_choice,
            getattr(self, "combination_names", None),
        )
        if (
            self._loaded_combo_id is not None
            and target != self._loaded_combo_id
            and not self._allow_discard_combo_edits()
        ):
            self.combo_choice.blockSignals(True)
            self.combo_choice.setCurrentIndex(
                self.combo_choice.findData(self._loaded_combo_id)
            )
            self.combo_choice.blockSignals(False)
            return
        self.load_combo()

    def _build_route(self, route: str, label: str) -> CardWidget:
        card = CardWidget()
        form = QFormLayout(card)
        form.setContentsMargins(20, 16, 20, 16)
        form.setSpacing(10)
        form.addRow(StrongBodyLabel(label), CaptionLabel("主站点失败后按重试次数切换灾备"))
        primary = ComboBox()
        model = EditableComboBox()
        effort = ComboBox()
        timeout = LineEdit()
        retries = LineEdit()
        fallback = ComboBox()
        fallback_model = EditableComboBox()
        fallback_effort = ComboBox()
        for editable in (model, fallback_model):
            if hasattr(editable, "setEditable"):
                editable.setEditable(True)
        for item in self.EFFORTS:
            effort.addItem(self.EFFORT_LABELS[item], userData=item)
            fallback_effort.addItem(self.EFFORT_LABELS[item], userData=item)
        fallback.addItem("不切换灾备", userData="")
        form.addRow(form_label("优先站点"), primary)
        form.addRow(form_label("优先模型"), model)
        form.addRow(form_label("思考等级"), effort)
        form.addRow(form_label("超时（秒）"), timeout)
        form.addRow(form_label("失败重试次数"), retries)
        form.addRow(form_label("重试后站点"), fallback)
        form.addRow(form_label("灾备模型"), fallback_model)
        form.addRow(form_label("灾备思考等级"), fallback_effort)
        self.route_cards[route] = {
            "primary": primary,
            "model": model,
            "effort": effort,
            "timeout": timeout,
            "retries": retries,
            "fallback": fallback,
            "fallback_model": fallback_model,
            "fallback_effort": fallback_effort,
        }
        primary.currentIndexChanged.connect(lambda _=0, r=route: self._refresh_models(r, False))
        fallback.currentIndexChanged.connect(lambda _=0, r=route: self._refresh_models(r, True))
        return card

    def _build_condition_card(self) -> CardWidget:
        card = CardWidget()
        layout = QVBoxLayout(card)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.addWidget(StrongBodyLabel("题型条件"))
        self.condition_caption = wrapped_caption(
            "可选；例如题型为 matching、fill_blank 时覆盖该组合的通用规则。"
        )
        layout.addWidget(self.condition_caption)
        condition_header = QHBoxLayout()
        self.condition_choice = ComboBox()
        condition_header.addWidget(self.condition_choice, 1)
        self.add_condition_button = PushButton("新增规则")
        self.add_condition_button.clicked.connect(self._add_condition)
        condition_header.addWidget(self.add_condition_button)
        self.delete_condition_button = PushButton("删除当前规则")
        self.delete_condition_button.clicked.connect(self._delete_condition)
        condition_header.addWidget(self.delete_condition_button)
        layout.addLayout(condition_header)
        self.kind = LineEdit()
        self.kind.setPlaceholderText("题型标识，例如 matching")
        self.kind_site = ComboBox()
        self.kind_model = EditableComboBox()
        self.kind_effort = ComboBox()
        self.kind_timeout = LineEdit()
        self.kind_retries = LineEdit()
        self.kind_fallback = ComboBox()
        self.kind_fallback_model = EditableComboBox()
        self.kind_fallback_effort = ComboBox()
        for editable in (self.kind_model, self.kind_fallback_model):
            if hasattr(editable, "setEditable"):
                editable.setEditable(True)
        for item in self.EFFORTS:
            self.kind_effort.addItem(self.EFFORT_LABELS[item], userData=item)
            self.kind_fallback_effort.addItem(self.EFFORT_LABELS[item], userData=item)
        form = QFormLayout()
        form.addRow(form_label("题型"), self.kind)
        form.addRow(form_label("使用站点"), self.kind_site)
        form.addRow(form_label("使用模型"), self.kind_model)
        form.addRow(form_label("思考等级"), self.kind_effort)
        form.addRow(form_label("超时（秒）"), self.kind_timeout)
        form.addRow(form_label("失败重试次数"), self.kind_retries)
        form.addRow(form_label("重试后站点"), self.kind_fallback)
        form.addRow(form_label("灾备模型"), self.kind_fallback_model)
        form.addRow(form_label("灾备思考等级"), self.kind_fallback_effort)
        layout.addLayout(form)
        self.kind_site.currentIndexChanged.connect(lambda: self._refresh_kind_models())
        self.kind_fallback.currentIndexChanged.connect(
            lambda: self._refresh_kind_fallback_models()
        )
        self.condition_choice.currentIndexChanged.connect(self._condition_selected)
        return card

    def _build_challenge_card(self) -> CardWidget:
        card = CardWidget()
        form = QFormLayout(card)
        form.setContentsMargins(20, 16, 20, 16)
        form.addRow(
            StrongBodyLabel("Chaoxing 挑战模式"),
            wrapped_caption("连续失败后切换专用模型；不影响普通限时/一般任务规则。"),
        )
        self.challenge_source_label = wrapped_caption("")
        form.addRow(self.challenge_source_label)
        self.challenge_attempts = LineEdit()
        self.challenge_site = ComboBox()
        self.challenge_model = EditableComboBox()
        self.challenge_effort = ComboBox()
        self.challenge_timeout = LineEdit()
        self.challenge_request_retries = LineEdit()
        self.challenge_retries = LineEdit()
        if hasattr(self.challenge_model, "setEditable"):
            self.challenge_model.setEditable(True)
        for item in self.EFFORTS:
            self.challenge_effort.addItem(self.EFFORT_LABELS[item], userData=item)
        form.addRow(form_label("普通模型最多失败次数"), self.challenge_attempts)
        form.addRow(form_label("升级后站点"), self.challenge_site)
        form.addRow(form_label("升级后模型"), self.challenge_model)
        form.addRow(form_label("升级后思考等级"), self.challenge_effort)
        form.addRow(form_label("模型超时（秒）"), self.challenge_timeout)
        form.addRow(form_label("模型请求重试次数"), self.challenge_request_retries)
        form.addRow(form_label("升级后再尝试次数"), self.challenge_retries)
        self.challenge_site.currentIndexChanged.connect(self._refresh_challenge_models)
        return card

    def _models(self):
        return self.controller.config.ensure().setdefault("models", {})

    def _endpoints(self):
        return self._models().setdefault("endpoints", {})

    def _save_models(self, models: dict[str, Any]) -> None:
        config = self.controller.config.ensure()
        config["models"] = models
        self.controller.config.save(config)

    @staticmethod
    def _selected_value(combo: ComboBox, fallback: list[str] | None = None) -> str | None:
        value = combo.currentData()
        if value not in (None, ""):
            return str(value)
        index = combo.currentIndex()
        if fallback is not None and 0 <= index < len(fallback):
            return str(fallback[index])
        text = combo.currentText().strip()
        return text or None

    def reload(self, *, select_site: str | None = None, select_combo: str | None = None):
        select_site = select_site or self._selected_value(
            self.site_choice, getattr(self, "endpoint_names", None)
        )
        select_combo = select_combo or self._selected_value(
            self.combo_choice, getattr(self, "combination_names", None)
        )
        models = self._models()
        endpoints = models.setdefault("endpoints", {})
        self.endpoint_names = list(endpoints)
        self.site_choice.blockSignals(True)
        self.site_choice.clear()
        for name in endpoints:
            self.site_choice.addItem(name, userData=name)
        site_index = self.site_choice.findData(select_site)
        self.site_choice.setCurrentIndex(
            site_index if site_index >= 0 else (0 if self.endpoint_names else -1)
        )
        self.site_choice.blockSignals(False)
        self.combo_choice.blockSignals(True)
        self.combo_choice.clear()
        combinations = models.setdefault("combinations", {})
        self.combination_names = list(combinations)
        display_names = {"economy": "默认", "gpt_only": "高级"}
        for name in self.combination_names:
            value = (
                combinations.get(name, {}) if isinstance(combinations.get(name, {}), dict) else {}
            )
            label = str(value.get("display_name") or display_names.get(name, name))
            if models.get("default") == name and label != "默认":
                label += "（默认）"
            self.combo_choice.addItem(label, userData=name)
        combo_index = self.combo_choice.findData(select_combo)
        self.combo_choice.setCurrentIndex(
            combo_index if combo_index >= 0 else (0 if self.combination_names else -1)
        )
        self.combo_choice.blockSignals(False)
        self._refresh_route_endpoints()
        self._refresh_condition_endpoints()
        self._refresh_challenge_endpoints()
        self.load_combo()
        self._update_action_state()

    def _update_action_state(self) -> None:
        has_site = bool(self.site_choice.currentData())
        has_combo = bool(self.combo_choice.currentData())
        for index, button in enumerate(self.site_action_buttons):
            if index > 0:
                button.setEnabled(has_site)
        for index, button in enumerate(self.combo_action_buttons):
            if index > 0:
                button.setEnabled(has_combo)

    def _set_combo_editor_enabled(self, enabled: bool) -> None:
        widgets = []
        for fields in self.route_cards.values():
            widgets.extend(fields.values())
        widgets.extend(
            (
                self.kind,
                self.kind_site,
                self.kind_model,
                self.kind_effort,
                self.kind_timeout,
                self.kind_retries,
                self.kind_fallback,
                self.kind_fallback_model,
                self.kind_fallback_effort,
                self.condition_choice,
                self.add_condition_button,
                self.delete_condition_button,
                self.challenge_attempts,
                self.challenge_site,
                self.challenge_model,
                self.challenge_effort,
                self.challenge_timeout,
                self.challenge_request_retries,
                self.challenge_retries,
            )
        )
        for widget in widgets:
            widget.setEnabled(enabled)
        self.save_combo_button.setEnabled(enabled)

    def _refresh_route_endpoints(self):
        for route, fields in self.route_cards.items():
            for key in ("primary", "fallback"):
                combo = fields[key]
                current = combo.currentData()
                combo.blockSignals(True)
                combo.clear()
                if key == "primary":
                    combo.addItem("请选择站点", userData="")
                else:
                    combo.addItem("不切换灾备", userData="")
                for name in self._endpoints():
                    combo.addItem(name, userData=name)
                index = combo.findData(current)
                combo.setCurrentIndex(index if index >= 0 else 0)
                combo.blockSignals(False)
            self._refresh_models(route, False)
            self._refresh_models(route, True)

    def _refresh_models(self, route: str, backup: bool):
        fields = self.route_cards[route]
        endpoint = fields["fallback"] if backup else fields["primary"]
        target = fields["fallback_model"] if backup else fields["model"]
        self._fill_models(target, str(endpoint.currentData() or ""))

    def _fill_models(self, target: ComboBox, endpoint: str, current: str = ""):
        current = current or str(target.currentData() or target.currentText() or "")
        target.blockSignals(True)
        target.clear()
        value = self._endpoints().get(endpoint, {})
        value = value if isinstance(value, dict) else {}
        scanned = value.get("models", [])
        scanned = scanned if isinstance(scanned, list) else []
        options = [current, value.get("model", ""), *scanned]
        for item in dict.fromkeys(str(x) for x in options if x):
            target.addItem(item, userData=item)
        index = target.findData(current)
        target.setCurrentIndex(index if index >= 0 else (0 if target.count() else -1))
        target.blockSignals(False)

    def _refresh_condition_endpoints(self):
        self._fill_endpoint_combo(self.kind_site, allow_empty=True, empty_text="不使用题型覆盖")
        self._fill_endpoint_combo(
            self.kind_fallback,
            allow_empty=True,
            empty_text="不切换灾备",
        )
        self._refresh_kind_models()
        self._refresh_kind_fallback_models()

    def _refresh_kind_models(self):
        self._fill_models(self.kind_model, str(self.kind_site.currentData() or ""))

    def _refresh_kind_fallback_models(self):
        self._fill_models(
            self.kind_fallback_model,
            str(self.kind_fallback.currentData() or ""),
        )

    def _reload_condition_choice(self, select_index: int | None = None) -> None:
        if select_index is None:
            select_index = 0 if self._condition_rules else None
        self.condition_choice.blockSignals(True)
        self.condition_choice.clear()
        if not self._condition_rules:
            self.condition_choice.addItem("尚未添加题型覆盖规则", userData=None)
        else:
            for index, rule in enumerate(self._condition_rules):
                kind = str(rule.get("kind") or "未命名规则")
                self.condition_choice.addItem(f"{index + 1}. {kind}", userData=index)
        target = self.condition_choice.findData(select_index)
        self.condition_choice.setCurrentIndex(target if target >= 0 else 0)
        self.condition_choice.blockSignals(False)
        self._condition_editor_index = (
            int(select_index)
            if select_index is not None and 0 <= int(select_index) < len(self._condition_rules)
            else None
        )
        self._load_condition_editor()

    def _condition_selected(self) -> None:
        if self._condition_loading:
            return
        self._commit_condition_editor()
        selected = self.condition_choice.currentData()
        self._condition_editor_index = int(selected) if selected is not None else None
        self._load_condition_editor()

    def _load_condition_editor(self) -> None:
        self._condition_loading = True
        index = self._condition_editor_index
        rule = (
            self._condition_rules[index]
            if index is not None and 0 <= index < len(self._condition_rules)
            else {}
        )
        self.kind.setText(str(rule.get("kind") or ""))
        site_index = self.kind_site.findData(rule.get("primary", ""))
        self.kind_site.setCurrentIndex(site_index if site_index >= 0 else 0)
        self._fill_models(
            self.kind_model,
            str(self.kind_site.currentData() or ""),
            str(rule.get("model") or ""),
        )
        effort_index = self.kind_effort.findData(rule.get("reasoning_effort", "medium"))
        self.kind_effort.setCurrentIndex(effort_index if effort_index >= 0 else 0)
        self.kind_timeout.setText(str(rule.get("timeout_seconds", "")))
        self.kind_retries.setText(str(rule.get("retry_attempts", 0)))
        fallback_index = self.kind_fallback.findData(rule.get("fallback", ""))
        self.kind_fallback.setCurrentIndex(fallback_index if fallback_index >= 0 else 0)
        self._fill_models(
            self.kind_fallback_model,
            str(self.kind_fallback.currentData() or ""),
            str(rule.get("fallback_model") or ""),
        )
        fallback_effort_index = self.kind_fallback_effort.findData(
            rule.get("fallback_reasoning_effort", "medium")
        )
        self.kind_fallback_effort.setCurrentIndex(
            fallback_effort_index if fallback_effort_index >= 0 else 0
        )
        enabled = index is not None and self.save_combo_button.isEnabled()
        for widget in (
            self.kind,
            self.kind_site,
            self.kind_model,
            self.kind_effort,
            self.kind_timeout,
            self.kind_retries,
            self.kind_fallback,
            self.kind_fallback_model,
            self.kind_fallback_effort,
        ):
            widget.setEnabled(enabled)
        self.delete_condition_button.setEnabled(enabled)
        self.condition_caption.setText(
            f"共 {len(self._condition_rules)} 条规则；选择一条即可逐项编辑。"
            if self._condition_rules
            else "可选；点击“新增规则”为某种题型配置独立模型。"
        )
        self._condition_loading = False

    def _commit_condition_editor(self) -> None:
        if self._condition_loading:
            return
        index = self._condition_editor_index
        if index is None or not 0 <= index < len(self._condition_rules):
            return
        current = self._condition_rules[index]
        self._condition_rules[index] = {
            **current,
            "kind": self.kind.text().strip(),
            "primary": str(self.kind_site.currentData() or ""),
            "model": str(self.kind_model.currentData() or self.kind_model.currentText()),
            "reasoning_effort": str(self.kind_effort.currentData() or "medium"),
            "timeout_seconds": self.kind_timeout.text().strip(),
            "retry_attempts": self.kind_retries.text().strip(),
            "fallback": str(self.kind_fallback.currentData() or ""),
            "fallback_model": str(
                self.kind_fallback_model.currentData()
                or self.kind_fallback_model.currentText()
            ),
            "fallback_reasoning_effort": str(
                self.kind_fallback_effort.currentData() or "medium"
            ),
        }
        self.condition_choice.setItemText(
            index, f"{index + 1}. {self.kind.text().strip() or '未命名规则'}"
        )

    def _add_condition(self) -> None:
        self._commit_condition_editor()
        self._condition_rules.append(
            {
                "kind": "",
                "primary": "",
                "model": "",
                "reasoning_effort": "medium",
                "timeout_seconds": 30,
                "retry_attempts": 1,
                "fallback": "",
                "fallback_model": "",
                "fallback_reasoning_effort": "medium",
            }
        )
        self._reload_condition_choice(len(self._condition_rules) - 1)
        self._mark_combo_dirty()

    def _delete_condition(self) -> None:
        index = self._condition_editor_index
        if index is None or not 0 <= index < len(self._condition_rules):
            return
        self._condition_rules.pop(index)
        next_index = min(index, len(self._condition_rules) - 1) if self._condition_rules else None
        self._reload_condition_choice(next_index)
        self._mark_combo_dirty()

    def _refresh_challenge_endpoints(self):
        self._fill_endpoint_combo(self.challenge_site, allow_empty=True)
        self._refresh_challenge_models()

    def _fill_endpoint_combo(
        self,
        combo: ComboBox,
        *,
        allow_empty: bool = False,
        empty_text: str = "不启用专用升级模型",
    ):
        current = combo.currentData()
        combo.blockSignals(True)
        combo.clear()
        if allow_empty:
            combo.addItem(empty_text, userData="")
        for name in self._endpoints():
            combo.addItem(name, userData=name)
        index = combo.findData(current)
        combo.setCurrentIndex(index if index >= 0 else (0 if combo.count() else -1))
        combo.blockSignals(False)

    def _refresh_challenge_models(self):
        self._fill_models(self.challenge_model, str(self.challenge_site.currentData() or ""))

    def load_combo(self):
        self._combo_loading = True
        name = self.combo_choice.currentData()
        combinations = self._models().setdefault("combinations", {})
        pending = self._pending_combo is not None
        if pending and name is not None:
            self._pending_combo = None
            self._pending_combo_id = None
            self._pending_combo_display_name = None
            pending = False
        value = self._pending_combo if pending else combinations.get(name, {}) if name else {}
        if not isinstance(value, dict):
            value = {}
        if pending:
            self._current_combo_id = self._pending_combo_id
            display_name = self._pending_combo_display_name or self._pending_combo_id or "新组合"
        else:
            self._current_combo_id = str(name) if name else None
            current = combinations.get(name, {}) if name else {}
            display_name = current.get("display_name") if isinstance(current, dict) else None
            display_name = display_name or (
                {"economy": "默认", "gpt_only": "高级"}.get(str(name), str(name or "新组合"))
            )
        if pending:
            self.current_combo_label.setText(f"正在编辑未保存的新组合：{display_name}")
            self.current_combo_label.show()
        elif name is None:
            self.current_combo_label.setText("尚无答案组合，请先点击“新建组合”。")
            self.current_combo_label.show()
        else:
            self.current_combo_label.hide()
        self._set_combo_editor_enabled(pending or name is not None)
        for route, fields in self.route_cards.items():
            row = value.get(route, {}) if isinstance(value.get(route), dict) else {}
            for key, field in (
                ("primary", "primary"),
                ("fallback", "fallback"),
                ("effort", "reasoning_effort"),
            ):
                fields[key].setCurrentIndex(max(0, fields[key].findData(row.get(field, ""))))
            fields["timeout"].setText(str(row.get("timeout_seconds", "")))
            fields["retries"].setText(str(row.get("retry_attempts", 0)))
            self._fill_models(
                fields["model"],
                str(fields["primary"].currentData() or ""),
                str(row.get("model", "")),
            )
            self._fill_models(
                fields["fallback_model"],
                str(fields["fallback"].currentData() or ""),
                str(row.get("fallback_model", "")),
            )
            fields["fallback_effort"].setCurrentIndex(
                max(
                    0,
                    fields["fallback_effort"].findData(
                        row.get("fallback_reasoning_effort", "medium")
                    ),
                )
            )
        conditions = value.get("conditions")
        self._condition_rules = [
            deepcopy(item) for item in conditions if isinstance(item, dict)
        ] if isinstance(conditions, list) else []
        self._reload_condition_choice()
        challenge = value.get("challenge")
        inherited_challenge = not isinstance(challenge, dict)
        if inherited_challenge:
            models = self._models()
            high_id = str(models.get("gpt_only") or "gpt_only")
            high_combo = combinations.get(high_id, {})
            high_combo = high_combo if isinstance(high_combo, dict) else {}
            high_route = high_combo.get("untimed", {})
            high_route = high_route if isinstance(high_route, dict) else {}
            provider = self.controller.config.ensure().get("providers", {}).get("chaoxing", {})
            provider = provider if isinstance(provider, dict) else {}
            challenge = {
                **deepcopy(high_route),
                "normal_attempts": provider.get("challenge_retry_attempts", 3),
                "escalation_attempts": 1,
            }
        if not isinstance(challenge, dict):
            challenge = {}
        if inherited_challenge and high_route:
            source_text = (
                "当前沿用“高级”组合的一般任务模型；保存后会写入本组合的独立挑战规则。"
            )
        elif inherited_challenge:
            source_text = "当前没有可继承的高级组合，挑战升级模型尚未配置。"
        else:
            source_text = "当前组合已配置独立挑战规则。"
        self.challenge_source_label.setText(source_text)
        self.challenge_attempts.setText(
            str(challenge.get("normal_attempts", challenge.get("retry_attempts", 3)))
        )
        self.challenge_site.setCurrentIndex(
            max(
                0,
                self.challenge_site.findData(
                    challenge.get("primary", challenge.get("escalation_endpoint", ""))
                ),
            )
        )
        self._fill_models(
            self.challenge_model,
            str(self.challenge_site.currentData() or ""),
            str(challenge.get("model", challenge.get("escalation_model", ""))),
        )
        self.challenge_effort.setCurrentIndex(
            max(0, self.challenge_effort.findData(challenge.get("reasoning_effort", "xhigh")))
        )
        self.challenge_timeout.setText(str(challenge.get("timeout_seconds", "")))
        self.challenge_request_retries.setText(str(challenge.get("retry_attempts", 0)))
        self.challenge_retries.setText(
            str(challenge.get("escalation_attempts", challenge.get("escalation_retries", 1)))
        )
        self._update_action_state()
        self._loaded_combo_id = str(name) if name is not None else None
        self._combo_dirty = False
        self._combo_loading = False

    def save_combo(self):
        models = self._models()
        combinations = models.setdefault("combinations", {})
        name = (
            self._pending_combo_id
            or self._current_combo_id
            or str(self.combo_choice.currentData() or "")
        )
        current_value = combinations.get(name, {}) if name else {}
        display_name = (
            self._pending_combo_display_name
            or (current_value.get("display_name") if isinstance(current_value, dict) else None)
            or {"economy": "默认", "gpt_only": "高级"}.get(name, name)
        )
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", name):
            _notice(self, "答案组合", "组合标识格式不正确。")
            return
        old = self.combo_choice.currentData()
        value: dict[str, Any] = {"display_name": display_name}
        try:
            for route, fields in self.route_cards.items():
                primary = str(fields["primary"].currentData() or "")
                model = str(fields["model"].currentData() or fields["model"].currentText())
                if not primary or not model:
                    raise ValueError(f"{self.ROUTE_LABELS[route]}必须选择站点和模型")
                row = {
                    "primary": primary,
                    "model": model,
                    "reasoning_effort": fields["effort"].currentData(),
                    "timeout_seconds": self._nonnegative_number(
                        fields["timeout"].text(), "超时", floating=True
                    ),
                    "retry_attempts": self._nonnegative_number(
                        fields["retries"].text(), "失败重试次数"
                    ),
                }
                if fields["fallback"].currentData():
                    fallback_model = str(
                        fields["fallback_model"].currentData()
                        or fields["fallback_model"].currentText()
                    )
                    if not fallback_model:
                        raise ValueError(f"{self.ROUTE_LABELS[route]}的灾备站点必须选择模型")
                    row.update(
                        {
                            "fallback": fields["fallback"].currentData(),
                            "fallback_model": fallback_model,
                            "fallback_reasoning_effort": fields["fallback_effort"].currentData(),
                        }
                    )
                value[route] = row
            self._commit_condition_editor()
            condition_rows = []
            for index, condition in enumerate(self._condition_rules, start=1):
                kind = str(condition.get("kind") or "").strip()
                if not kind:
                    continue
                kind_site = str(condition.get("primary") or "")
                kind_model = str(condition.get("model") or "")
                if not kind_site or not kind_model:
                    raise ValueError(f"第 {index} 条题型条件必须选择站点和模型")
                condition_rows.append(
                    {
                        **condition,
                        "kind": kind,
                        "primary": kind_site,
                        "model": kind_model,
                        "reasoning_effort": condition.get("reasoning_effort") or "medium",
                        "timeout_seconds": self._nonnegative_number(
                            str(condition.get("timeout_seconds") or ""),
                            f"第 {index} 条题型条件超时",
                            floating=True,
                        ),
                        "retry_attempts": self._nonnegative_number(
                            str(condition.get("retry_attempts") or ""),
                            f"第 {index} 条题型条件重试次数",
                        ),
                    }
                )
                if condition.get("fallback"):
                    fallback_model = str(condition.get("fallback_model") or "")
                    if not fallback_model:
                        raise ValueError(f"第 {index} 条题型条件的灾备站点必须选择模型")
                    condition_rows[-1].update(
                        {
                            "fallback": str(condition["fallback"]),
                            "fallback_model": fallback_model,
                            "fallback_reasoning_effort": condition.get(
                                "fallback_reasoning_effort"
                            )
                            or "medium",
                        }
                    )
            if condition_rows:
                value["conditions"] = condition_rows
            challenge_site = str(self.challenge_site.currentData() or "")
            challenge_model = str(
                self.challenge_model.currentData() or self.challenge_model.currentText()
            )
            if challenge_site:
                if not challenge_model:
                    raise ValueError("挑战模式升级站点必须选择模型")
                value["challenge"] = {
                    "primary": challenge_site,
                    "model": challenge_model,
                    "reasoning_effort": self.challenge_effort.currentData(),
                    "timeout_seconds": self._nonnegative_number(
                        self.challenge_timeout.text(), "挑战模式模型超时", floating=True
                    ),
                    "retry_attempts": self._nonnegative_number(
                        self.challenge_request_retries.text(), "挑战模式模型请求重试次数"
                    ),
                    "normal_attempts": self._nonnegative_number(
                        self.challenge_attempts.text(), "普通模型失败次数"
                    ),
                    "escalation_attempts": self._nonnegative_number(
                        self.challenge_retries.text(), "升级后尝试次数"
                    ),
                }
        except ValueError as error:
            _notice(self, "AI 配置", str(error))
            return
        if old and old != name and self._pending_combo_id is not None:
            combinations.pop(old, None)
        combinations[name] = value
        models["default"] = models.get("default") or name
        self._save_models(models)
        self._pending_combo = None
        self._pending_combo_id = None
        self._pending_combo_display_name = None
        self.reload(select_combo=name)
        self._notify_combination_change()
        _notice(self, "AI 配置", "答案组合已保存。")

    @staticmethod
    def _nonnegative_number(text: str, label: str, *, floating: bool = False):
        raw = text.strip()
        if not raw:
            return 0.0 if floating else 0
        try:
            value = float(raw) if floating else int(raw)
        except ValueError as error:
            raise ValueError(f"{label}必须是有效数字") from error
        if value < 0:
            raise ValueError(f"{label}不能小于 0")
        return value

    def _notify_combination_change(self) -> None:
        window = self.window()
        for page in getattr(window, "pages", []):
            callback = getattr(page, "_reload_answer_combinations", None)
            if callable(callback):
                callback()

    def add_site(self):
        self._edit_site("")

    def edit_site(self):
        name = self._selected_value(self.site_choice, self.endpoint_names)
        if name:
            self._edit_site(name)

    def _edit_site(self, name):
        if not self._allow_discard_combo_edits():
            return
        models = self._models()
        endpoints = models.setdefault("endpoints", {})
        dialog = _EndpointDialog(list(endpoints), name, endpoints.get(name), self)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        new, edited = dialog.value()
        value = {
            **(endpoints.get(name, {}) if isinstance(endpoints.get(name), dict) else {}),
            **edited,
        }
        previous = endpoints.get(name, {})
        value["models"] = previous.get("models", []) if isinstance(previous, dict) else []
        if name and name != new:
            endpoints.pop(name, None)
            self._replace_endpoint_references(models, name, new)
        endpoints[new] = value
        self._save_models(models)
        self.reload(select_site=new)
        self._notify_combination_change()

    @staticmethod
    def _replace_endpoint_references(models: dict[str, Any], old: str, new: str) -> None:
        combinations = models.get("combinations", {})
        if not isinstance(combinations, dict):
            return
        for combination in combinations.values():
            if not isinstance(combination, dict):
                continue
            for route_name in ("timed", "untimed", "challenge"):
                route = combination.get(route_name)
                if not isinstance(route, dict):
                    continue
                for key in ("primary", "fallback"):
                    if route.get(key) == old:
                        route[key] = new
            for condition in combination.get("conditions", []):
                if isinstance(condition, dict):
                    for key in ("primary", "fallback"):
                        if condition.get(key) == old:
                            condition[key] = new

    def delete_site(self):
        name = self._selected_value(self.site_choice, self.endpoint_names)
        if not name:
            _notice(self, "删除站点", "当前没有可删除的站点。")
            return
        if not self._allow_discard_combo_edits():
            return
        if not _confirm(
            self, "删除站点", f"确定删除站点“{name}”吗？组合中引用该站点的位置会同时清空。"
        ):
            return
        models = self._models()
        models.setdefault("endpoints", {}).pop(name, None)
        self._replace_endpoint_references(models, name, "")
        self._save_models(models)
        self.reload()
        self._notify_combination_change()

    def scan_site(self):
        name = self._selected_value(self.site_choice, self.endpoint_names)
        if not name:
            _notice(self, "扫描模型", "请先选择站点。")
            return
        if not self._allow_discard_combo_edits():
            return
        if self._scan_thread is not None and self._scan_thread.isRunning():
            _notice(self, "扫描模型", "已有模型扫描正在运行。")
            return
        self._set_site_actions_enabled(False)
        self._scan_thread = _ScanThread(name, self._endpoints()[name])
        self._scan_thread.done.connect(self._scan_done)
        self._scan_thread.failed.connect(self._scan_failed)
        self._scan_thread.start()

    def _scan_done(self, name, models):
        config_models = self._models()
        config_models.setdefault("endpoints", {}).setdefault(name, {})["models"] = models
        self._save_models(config_models)
        self.reload(select_site=name)
        self._set_site_actions_enabled(True)
        self._notify_combination_change()
        _notice(self, "扫描模型", f"已读取 {len(models)} 个模型。")

    def _scan_failed(self, _name, message):
        self._set_site_actions_enabled(True)
        _notice(self, "扫描模型失败", message)

    def _set_site_actions_enabled(self, enabled: bool):
        self.site_choice.setEnabled(enabled)
        for button in self.site_action_buttons:
            button.setEnabled(enabled)
        if enabled:
            self._update_action_state()

    def new_combo(self):
        if not self._allow_discard_combo_edits():
            return
        self._open_new_combo({})

    def copy_combo(self):
        if not self._allow_discard_combo_edits():
            return
        current_id = self._selected_value(self.combo_choice, self.combination_names) or ""
        current = self._models().setdefault("combinations", {}).get(current_id, {})
        current_display = (current or {}).get("display_name") or {
            "economy": "默认",
            "gpt_only": "高级",
        }.get(current_id, current_id)
        self._open_new_combo(
            deepcopy(current),
            suggested_id=f"{current_id}_copy" if current_id else "new_combo",
            suggested_display_name=f"{current_display} 副本",
        )

    def _open_new_combo(
        self, value, suggested_id: str = "new_combo", suggested_display_name: str = "新组合"
    ):
        names = list(self._models().setdefault("combinations", {}))
        suffix = 2
        base_id = suggested_id
        while suggested_id in names:
            suggested_id = f"{base_id}_{suffix}"
            suffix += 1
        dialog = _CombinationNameDialog(names, suggested_id, suggested_display_name, self)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        display_name, combo_id = dialog.value()
        self._pending_combo = deepcopy(value)
        self._pending_combo_id = combo_id
        self._pending_combo_display_name = display_name
        self.combo_choice.blockSignals(True)
        self.combo_choice.setCurrentIndex(-1)
        self.combo_choice.blockSignals(False)
        self.load_combo()

    def delete_combo(self):
        name = self._selected_value(self.combo_choice, self.combination_names)
        if not name:
            _notice(self, "删除组合", "当前没有可删除的组合。")
            return
        combinations = self._models().setdefault("combinations", {})
        display_name = (combinations.get(name) or {}).get("display_name") or {
            "economy": "默认",
            "gpt_only": "高级",
        }.get(str(name), str(name))
        if not _confirm(self, "删除组合", f"确定删除答案组合“{display_name}”吗？此操作不可撤销。"):
            return
        models = self._models()
        combinations = models.setdefault("combinations", {})
        combinations.pop(name, None)
        if models.get("default") == name:
            models["default"] = next(iter(combinations), "")
        if models.get("gpt_only") == name:
            models["gpt_only"] = ""
        self._save_models(models)
        self.reload()
        self._notify_combination_change()

    def set_default_combo(self) -> None:
        if not self._allow_discard_combo_edits():
            return
        name = self._selected_value(self.combo_choice, self.combination_names)
        if not name:
            _notice(self, "答案组合", "请先选择一个答案组合。")
            return
        models = self._models()
        if name not in models.setdefault("combinations", {}):
            _notice(self, "答案组合", "所选答案组合已经不存在，请刷新后重试。")
            return
        models["default"] = name
        self._save_models(models)
        self.reload(select_combo=name)
        self._notify_combination_change()
        _notice(self, "答案组合", "已设为默认答案组合。")
