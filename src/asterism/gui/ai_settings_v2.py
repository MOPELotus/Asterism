# ruff: noqa
from __future__ import annotations

import re
from copy import deepcopy
from typing import Any

import httpx
from PyQt6.QtCore import QThread, pyqtSignal
from PyQt6.QtWidgets import QDialog, QFormLayout, QHBoxLayout, QVBoxLayout, QWidget

from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    ComboBox,
    LineEdit,
    MessageBox,
    PrimaryPushButton,
    PushButton,
    ScrollArea,
    StrongBodyLabel,
    TitleLabel,
    configure_scroll_area,
)


def _title(text: str) -> TitleLabel:
    label = TitleLabel(text)
    label.setMinimumHeight(34)
    return label


def _notice(parent: QWidget, title: str, text: str) -> None:
    MessageBox(title, text, parent).exec()


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
            if self.endpoint.get("api_key"):
                headers["Authorization"] = f"Bearer {self.endpoint['api_key']}"
            response = httpx.get(_models_url(str(self.endpoint.get("base_url") or "")), headers=headers, timeout=15)
            response.raise_for_status()
            body = response.json()
            rows = body.get("data", body.get("models", [])) if isinstance(body, dict) else []
            values = sorted({str(row.get("id") or row.get("name")) for row in rows if isinstance(row, dict) and (row.get("id") or row.get("name"))})
            if not values:
                raise RuntimeError("站点没有返回模型")
            self.done.emit(self.name, values)
        except Exception as error:
            self.failed.emit(self.name, str(error))


class _EndpointDialog(QDialog):
    def __init__(self, names: list[str], name: str, value: dict[str, Any] | None, parent=None):
        super().__init__(parent)
        value = value or {}
        self.original = name
        self.setWindowTitle("AI 站点")
        form = QFormLayout(self)
        form.setContentsMargins(24, 20, 24, 20)
        self.name = LineEdit(); self.name.setText(name); self.name.setPlaceholderText("例如 router_backup")
        self.url = LineEdit(); self.url.setText(str(value.get("base_url") or "")); self.url.setPlaceholderText("https://example.com/v1")
        self.key = LineEdit(); self.key.setText(str(value.get("api_key") or "")); self.key.setEchoMode(LineEdit.EchoMode.Password)
        self.protocol = ComboBox(); self.protocol.addItem("Responses", "responses"); self.protocol.addItem("Chat Completions", "chat_completions")
        self.protocol.setCurrentIndex(max(0, self.protocol.findData(value.get("protocol", "responses"))))
        form.addRow("站点标识", self.name); form.addRow("API 地址", self.url); form.addRow("API Key", self.key); form.addRow("协议", self.protocol)
        buttons = QHBoxLayout(); cancel = PushButton("取消"); save = PrimaryPushButton("保存")
        cancel.clicked.connect(self.reject); save.clicked.connect(self._accept); buttons.addStretch(1); buttons.addWidget(cancel); buttons.addWidget(save); form.addRow(buttons)
        self.names = set(names); self.resize(620, 300)

    def _accept(self) -> None:
        name = self.name.text().strip()
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", name):
            _notice(self, "AI 站点", "站点标识只能使用小写字母、数字、下划线和连字符。"); return
        if name != self.original and name in self.names:
            _notice(self, "AI 站点", "该站点标识已经存在。"); return
        if not self.url.text().strip():
            _notice(self, "AI 站点", "请填写 API 地址。"); return
        self.accept()

    def value(self) -> tuple[str, dict[str, Any]]:
        return self.name.text().strip(), {"base_url": self.url.text().strip(), "api_key": self.key.text(), "protocol": self.protocol.currentData()}


class AISettingsPage(QWidget):
    EFFORTS = ("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")

    def __init__(self, controller):
        super().__init__()
        self.controller = controller
        self._scan_thread = None
        outer = QVBoxLayout(self); outer.setContentsMargins(0, 0, 0, 0)
        scroll = ScrollArea(); scroll.setWidgetResizable(True); content = QWidget(); scroll.setWidget(content); configure_scroll_area(scroll); outer.addWidget(scroll)
        root = QVBoxLayout(content); root.setContentsMargins(28, 24, 28, 28); root.setSpacing(14)
        intro = CardWidget(); il = QVBoxLayout(intro); il.setContentsMargins(20, 16, 20, 16)
        il.addWidget(_title("AI 配置")); il.addWidget(BodyLabel("选择一个答案组合，再按任务条件设置站点、模型、思考等级、超时和灾备。")); il.addWidget(CaptionLabel("内置组合可以修改；也可以复制或新建任意组合。API Key 只保存在本机。")); root.addWidget(intro)
        site_card = CardWidget(); sl = QVBoxLayout(site_card); sl.setContentsMargins(20, 16, 20, 16); sl.addWidget(StrongBodyLabel("AI 站点与模型")); sl.addWidget(CaptionLabel("先添加站点并扫描模型，下面的条件规则会使用扫描到的模型。"))
        self.site_choice = ComboBox(); sl.addWidget(self.site_choice)
        site_buttons = QHBoxLayout()
        for text, callback in (("新增站点", self.add_site), ("编辑站点", self.edit_site), ("删除站点", self.delete_site), ("扫描模型", self.scan_site)):
            button = PrimaryPushButton(text) if text == "新增站点" else PushButton(text); button.clicked.connect(callback); site_buttons.addWidget(button)
        site_buttons.addStretch(1); sl.addLayout(site_buttons); root.addWidget(site_card)
        combo_card = CardWidget(); cl = QVBoxLayout(combo_card); cl.setContentsMargins(20, 16, 20, 16); cl.addWidget(StrongBodyLabel("答案组合")); self.combo_choice = ComboBox(); cl.addWidget(self.combo_choice)
        combo_buttons = QHBoxLayout()
        for text, callback in (("新建组合", self.new_combo), ("复制当前", self.copy_combo), ("删除当前", self.delete_combo)):
            button = PrimaryPushButton(text) if text == "新建组合" else PushButton(text); button.clicked.connect(callback); combo_buttons.addWidget(button)
        combo_buttons.addStretch(1); cl.addLayout(combo_buttons); root.addWidget(combo_card)
        self.name = LineEdit(); self.name.setPlaceholderText("组合标识，例如 daily")
        name_card = CardWidget(); nl = QFormLayout(name_card); nl.setContentsMargins(20, 16, 20, 16); nl.addRow("当前组合标识", self.name); root.addWidget(name_card)
        self.route_cards: dict[str, dict[str, QWidget]] = {}
        for route, label in (("timed", "当任务为限时任务时"), ("untimed", "当任务为一般任务时")):
            root.addWidget(self._build_route(route, label))
        root.addWidget(self._build_condition_card())
        root.addWidget(self._build_challenge_card())
        save = PrimaryPushButton("保存当前组合"); save.clicked.connect(self.save_combo); root.addWidget(save, 0)
        root.addStretch(1)
        self.combo_choice.currentIndexChanged.connect(self.load_combo); self.site_choice.currentIndexChanged.connect(self._refresh_route_endpoints)
        self.reload()

    def _build_route(self, route: str, label: str) -> CardWidget:
        card = CardWidget(); form = QFormLayout(card); form.setContentsMargins(20, 16, 20, 16); form.setSpacing(10); form.addRow(StrongBodyLabel(label), CaptionLabel("主站点失败后按重试次数切换灾备"))
        primary = ComboBox(); model = ComboBox(); effort = ComboBox(); timeout = LineEdit(); retries = LineEdit(); fallback = ComboBox(); fallback_model = ComboBox(); fallback_effort = ComboBox()
        for item in self.EFFORTS: effort.addItem(item, item); fallback_effort.addItem(item, item)
        fallback.addItem("不切换灾备", "")
        form.addRow("优先站点", primary); form.addRow("优先模型", model); form.addRow("思考等级", effort); form.addRow("超时（秒）", timeout); form.addRow("失败重试次数", retries); form.addRow("重试后站点", fallback); form.addRow("灾备模型", fallback_model); form.addRow("灾备思考等级", fallback_effort)
        self.route_cards[route] = {"primary": primary, "model": model, "effort": effort, "timeout": timeout, "retries": retries, "fallback": fallback, "fallback_model": fallback_model, "fallback_effort": fallback_effort}
        primary.currentIndexChanged.connect(lambda _=0, r=route: self._refresh_models(r, False)); fallback.currentIndexChanged.connect(lambda _=0, r=route: self._refresh_models(r, True)); return card

    def _build_condition_card(self) -> CardWidget:
        card = CardWidget(); layout = QVBoxLayout(card); layout.setContentsMargins(20, 16, 20, 16); layout.addWidget(StrongBodyLabel("题型条件")); layout.addWidget(CaptionLabel("可选；例如题型为 matching、fill_blank 时覆盖该组合的通用规则。"))
        self.kind = LineEdit(); self.kind.setPlaceholderText("题型标识，例如 matching")
        self.kind_site = ComboBox(); self.kind_model = ComboBox(); self.kind_effort = ComboBox(); self.kind_timeout = LineEdit(); self.kind_retries = LineEdit()
        for item in self.EFFORTS: self.kind_effort.addItem(item, item)
        form = QFormLayout(); form.addRow("题型", self.kind); form.addRow("使用站点", self.kind_site); form.addRow("使用模型", self.kind_model); form.addRow("思考等级", self.kind_effort); form.addRow("超时（秒）", self.kind_timeout); form.addRow("失败重试次数", self.kind_retries); layout.addLayout(form); self.kind_site.currentIndexChanged.connect(lambda: self._refresh_kind_models()); return card

    def _build_challenge_card(self) -> CardWidget:
        card = CardWidget(); form = QFormLayout(card); form.setContentsMargins(20, 16, 20, 16); form.addRow(StrongBodyLabel("Chaoxing 挑战模式"), CaptionLabel("连续失败后切换专用模型；不影响普通限时/一般任务规则。"))
        self.challenge_attempts = LineEdit(); self.challenge_site = ComboBox(); self.challenge_model = ComboBox(); self.challenge_effort = ComboBox(); self.challenge_retries = LineEdit()
        for item in self.EFFORTS: self.challenge_effort.addItem(item, item)
        form.addRow("普通模型最多失败次数", self.challenge_attempts); form.addRow("升级后站点", self.challenge_site); form.addRow("升级后模型", self.challenge_model); form.addRow("升级后思考等级", self.challenge_effort); form.addRow("升级后再尝试次数", self.challenge_retries)
        self.challenge_site.currentIndexChanged.connect(self._refresh_challenge_models); return card

    def _models(self): return self.controller.config.ensure().setdefault("models", {})
    def _endpoints(self): return self._models().setdefault("endpoints", {})
    def reload(self):
        self.site_choice.blockSignals(True); self.site_choice.clear()
        for name in self._endpoints(): self.site_choice.addItem(name, name)
        self.site_choice.blockSignals(False)
        self.combo_choice.blockSignals(True); self.combo_choice.clear(); combinations = self._models().setdefault("combinations", {})
        for name in combinations: self.combo_choice.addItem(name, name)
        self.combo_choice.blockSignals(False); self._refresh_route_endpoints(); self._refresh_condition_endpoints(); self._refresh_challenge_endpoints(); self.load_combo()

    def _refresh_route_endpoints(self):
        for route, fields in self.route_cards.items():
            for key in ("primary", "fallback"):
                combo = fields[key]; current = combo.currentData(); combo.blockSignals(True); combo.clear()
                if key == "fallback": combo.addItem("不切换灾备", "")
                for name in self._endpoints(): combo.addItem(name, name)
                combo.setCurrentIndex(max(0, combo.findData(current))); combo.blockSignals(False)
            self._refresh_models(route, False); self._refresh_models(route, True)

    def _refresh_models(self, route: str, backup: bool):
        fields = self.route_cards[route]; endpoint = fields["fallback"] if backup else fields["primary"]; target = fields["fallback_model"] if backup else fields["model"]; self._fill_models(target, str(endpoint.currentData() or ""))

    def _fill_models(self, target: ComboBox, endpoint: str, current: str = ""):
        target.blockSignals(True); target.clear(); value = self._endpoints().get(endpoint, {}); options = [current, value.get("model", ""), *value.get("models", [])]
        for item in dict.fromkeys(str(x) for x in options if x): target.addItem(item, item)
        target.blockSignals(False)

    def _refresh_condition_endpoints(self):
        self._fill_endpoint_combo(self.kind_site)
        self._refresh_kind_models()

    def _refresh_kind_models(self): self._fill_models(self.kind_model, str(self.kind_site.currentData() or ""))
    def _refresh_challenge_endpoints(self): self._fill_endpoint_combo(self.challenge_site); self._refresh_challenge_models()
    def _fill_endpoint_combo(self, combo: ComboBox):
        current = combo.currentData(); combo.blockSignals(True); combo.clear()
        for name in self._endpoints(): combo.addItem(name, name)
        combo.setCurrentIndex(max(0, combo.findData(current))); combo.blockSignals(False)
    def _refresh_challenge_models(self): self._fill_models(self.challenge_model, str(self.challenge_site.currentData() or ""))

    def load_combo(self):
        name = self.combo_choice.currentData(); combinations = self._models().setdefault("combinations", {}); value = combinations.get(name, {}) if name else {}; self.name.setText(str(name or ""))
        for route, fields in self.route_cards.items():
            row = value.get(route, {}) if isinstance(value.get(route), dict) else {}
            for key, field in (("primary", "primary"), ("fallback", "fallback"), ("effort", "reasoning_effort")):
                fields[key].setCurrentIndex(max(0, fields[key].findData(row.get(field, ""))))
            fields["timeout"].setText(str(row.get("timeout_seconds", ""))); fields["retries"].setText(str(row.get("retry_attempts", 0))); self._refresh_models(route, False); self._refresh_models(route, True); fields["model"].setCurrentIndex(max(0, fields["model"].findData(row.get("model", "")))); fields["fallback_model"].setCurrentIndex(max(0, fields["fallback_model"].findData(row.get("fallback_model", "")))); fields["fallback_effort"].setCurrentIndex(max(0, fields["fallback_effort"].findData(row.get("fallback_reasoning_effort", "medium"))))
        condition = (value.get("conditions") or [{}])[0]; self.kind.setText(str(condition.get("kind", ""))); self.kind_site.setCurrentIndex(max(0, self.kind_site.findData(condition.get("primary", "")))); self._refresh_kind_models(); self.kind_model.setCurrentIndex(max(0, self.kind_model.findData(condition.get("model", "")))); self.kind_effort.setCurrentIndex(max(0, self.kind_effort.findData(condition.get("reasoning_effort", "medium")))); self.kind_timeout.setText(str(condition.get("timeout_seconds", ""))); self.kind_retries.setText(str(condition.get("retry_attempts", 0)))
        challenge = self._models().get("challenge", {}); self.challenge_attempts.setText(str(challenge.get("retry_attempts", 3))); self.challenge_site.setCurrentIndex(max(0, self.challenge_site.findData(challenge.get("escalation_endpoint", "")))); self._refresh_challenge_models(); self.challenge_model.setCurrentIndex(max(0, self.challenge_model.findData(challenge.get("escalation_model", "")))); self.challenge_effort.setCurrentIndex(max(0, self.challenge_effort.findData(challenge.get("reasoning_effort", "xhigh")))); self.challenge_retries.setText(str(challenge.get("escalation_retries", 1)))

    def save_combo(self):
        name = self.name.text().strip(); models = self._models(); combinations = models.setdefault("combinations", {})
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", name): _notice(self, "答案组合", "组合标识格式不正确。"); return
        old = self.combo_choice.currentData(); value: dict[str, Any] = {}
        for route, fields in self.route_cards.items():
            row = {"primary": fields["primary"].currentData(), "model": fields["model"].currentData() or fields["model"].currentText(), "reasoning_effort": fields["effort"].currentData(), "timeout_seconds": int(fields["timeout"].text() or 0), "retry_attempts": int(fields["retries"].text() or 0)}
            if fields["fallback"].currentData(): row.update({"fallback": fields["fallback"].currentData(), "fallback_model": fields["fallback_model"].currentData() or fields["fallback_model"].currentText(), "fallback_reasoning_effort": fields["fallback_effort"].currentData()})
            value[route] = row
        if self.kind.text().strip(): value["conditions"] = [{"kind": self.kind.text().strip(), "primary": self.kind_site.currentData(), "model": self.kind_model.currentData() or self.kind_model.currentText(), "reasoning_effort": self.kind_effort.currentData(), "timeout_seconds": int(self.kind_timeout.text() or 0), "retry_attempts": int(self.kind_retries.text() or 0)}]
        if old and old != name: combinations.pop(old, None)
        combinations[name] = value; models["default"] = models.get("default") or name; models["challenge"] = {"retry_attempts": int(self.challenge_attempts.text() or 3), "escalation_endpoint": self.challenge_site.currentData(), "escalation_model": self.challenge_model.currentData() or self.challenge_model.currentText(), "reasoning_effort": self.challenge_effort.currentData(), "escalation_retries": int(self.challenge_retries.text() or 1)}
        self.controller.config.save({**self.controller.config.ensure(), "models": models}); self.reload(); self.combo_choice.setCurrentText(name); _notice(self, "AI 配置", "答案组合已保存。")

    def add_site(self): self._edit_site("")
    def edit_site(self):
        name = self.site_choice.currentData()
        if name: self._edit_site(name)
    def _edit_site(self, name):
        endpoints = self._endpoints(); dialog = _EndpointDialog(list(endpoints), name, endpoints.get(name), self)
        if dialog.exec() != QDialog.DialogCode.Accepted: return
        new, value = dialog.value(); value["models"] = endpoints.get(name, {}).get("models", []); endpoints.pop(name, None) if name and name != new else None; endpoints[new] = value; self.controller.config.save({**self.controller.config.ensure(), "models": self._models()}); self.reload(); self.site_choice.setCurrentText(new)
    def delete_site(self):
        name = self.site_choice.currentData()
        if name: self._endpoints().pop(name, None); self.controller.config.save({**self.controller.config.ensure(), "models": self._models()}); self.reload()
    def scan_site(self):
        name = self.site_choice.currentData()
        if not name: _notice(self, "扫描模型", "请先选择站点。"); return
        self._scan_thread = _ScanThread(name, self._endpoints()[name]); self._scan_thread.done.connect(self._scan_done); self._scan_thread.failed.connect(lambda _, msg: _notice(self, "扫描模型失败", msg)); self._scan_thread.start()
    def _scan_done(self, name, models): self._endpoints()[name]["models"] = models; self.controller.config.save({**self.controller.config.ensure(), "models": self._models()}); self.reload(); _notice(self, "扫描模型", f"已读取 {len(models)} 个模型。")
    def new_combo(self): self._open_new_combo({})
    def copy_combo(self): self._open_new_combo(deepcopy(self._models().setdefault("combinations", {}).get(self.combo_choice.currentData(), {})))
    def _open_new_combo(self, value): self.name.clear(); self.combo_choice.setCurrentIndex(-1); self.load_combo()
    def delete_combo(self):
        name = self.combo_choice.currentData()
        if name: self._models().setdefault("combinations", {}).pop(name, None); self.controller.config.save({**self.controller.config.ensure(), "models": self._models()}); self.reload()
