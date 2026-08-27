# ruff: noqa: E501
from __future__ import annotations

import re
from typing import Any

import httpx
from PyQt6.QtCore import QThread, pyqtSignal
from PyQt6.QtWidgets import QDialog, QFormLayout, QHBoxLayout, QTableWidgetItem, QVBoxLayout, QWidget

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
    TableWidget,
    TitleLabel,
    configure_scroll_area,
    configure_table,
)


def _title(text: str) -> TitleLabel:
    label = TitleLabel(text)
    label.setMinimumHeight(34)
    return label


def _notice(parent: QWidget, title: str, text: str) -> None:
    MessageBox(title, text, parent).exec()


def _model_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    return base + "/models" if base.endswith("/v1") else base + "/v1/models"


class ModelScanThread(QThread):
    succeeded = pyqtSignal(str, object)
    failed = pyqtSignal(str, str)

    def __init__(self, name: str, endpoint: dict[str, Any]):
        super().__init__()
        self.name = name
        self.endpoint = endpoint

    def run(self) -> None:
        try:
            headers = {"Accept": "application/json"}
            key = str(self.endpoint.get("api_key") or "")
            if key:
                headers["Authorization"] = f"Bearer {key}"
            response = httpx.get(
                _model_url(str(self.endpoint.get("base_url") or "")),
                headers=headers,
                timeout=15,
            )
            response.raise_for_status()
            body = response.json()
            rows = body.get("data", body.get("models", [])) if isinstance(body, dict) else []
            models = sorted(
                {
                    str(row.get("id") or row.get("name") or "")
                    for row in rows
                    if isinstance(row, dict) and (row.get("id") or row.get("name"))
                }
            )
            if not models:
                raise RuntimeError("站点没有返回可用模型")
            self.succeeded.emit(self.name, models)
        except Exception as error:
            self.failed.emit(self.name, str(error))


class EndpointDialog(QDialog):
    def __init__(self, names: list[str], name: str = "", value: dict[str, Any] | None = None, parent=None):
        super().__init__(parent)
        value = value or {}
        self.original_name = name
        self.existing_names = set(names)
        self.setWindowTitle("AI 站点")
        form = QFormLayout(self)
        form.setContentsMargins(24, 20, 24, 20)
        self.name = LineEdit()
        self.name.setText(name)
        self.name.setPlaceholderText("例如 siliconflow")
        self.base_url = LineEdit()
        self.base_url.setText(str(value.get("base_url") or ""))
        self.base_url.setPlaceholderText("https://example.com/v1")
        self.api_key = LineEdit()
        self.api_key.setText(str(value.get("api_key") or ""))
        self.api_key.setEchoMode(LineEdit.EchoMode.Password)
        self.protocol = ComboBox()
        self.protocol.addItem("Responses", "responses")
        self.protocol.addItem("Chat Completions", "chat_completions")
        self.protocol.setCurrentIndex(max(0, self.protocol.findData(value.get("protocol", "responses"))))
        form.addRow("站点标识", self.name)
        form.addRow("API 地址", self.base_url)
        form.addRow("API Key", self.api_key)
        form.addRow("协议", self.protocol)
        actions = QHBoxLayout()
        cancel = PushButton("取消")
        cancel.clicked.connect(self.reject)
        save = PrimaryPushButton("保存")
        save.clicked.connect(self._accept)
        actions.addStretch(1)
        actions.addWidget(cancel)
        actions.addWidget(save)
        form.addRow(actions)
        self.resize(620, 300)

    def _accept(self) -> None:
        name = self.name.text().strip()
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", name):
            _notice(self, "AI 站点", "站点标识只能使用小写字母、数字、下划线和连字符。")
            return
        if name != self.original_name and name in self.existing_names:
            _notice(self, "AI 站点", "该站点标识已经存在。")
            return
        if not self.base_url.text().strip():
            _notice(self, "AI 站点", "请填写 API 地址。")
            return
        self.accept()

    def result_value(self) -> tuple[str, dict[str, Any]]:
        return self.name.text().strip(), {
            "base_url": self.base_url.text().strip(),
            "api_key": self.api_key.text(),
            "protocol": self.protocol.currentData(),
        }


class CombinationDialog(QDialog):
    EFFORTS = ("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")

    def __init__(self, endpoints: dict[str, Any], name: str = "", value: dict[str, Any] | None = None, parent=None):
        super().__init__(parent)
        self.endpoints = endpoints
        self.original_name = name
        self.setWindowTitle("答案组合")
        root = QVBoxLayout(self)
        root.setContentsMargins(24, 20, 24, 20)
        name_form = QFormLayout()
        self.name = LineEdit()
        self.name.setText(name)
        self.name.setPlaceholderText("例如 daily 或 high_quality")
        name_form.addRow("组合标识", self.name)
        root.addLayout(name_form)
        self.routes = {}
        for route, label in (("timed", "当任务为限时任务时"), ("untimed", "当任务为一般任务时")):
            route_value = (value or {}).get(route, {})
            card = CardWidget()
            form = QFormLayout(card)
            form.setContentsMargins(18, 14, 18, 14)
            form.addRow(StrongBodyLabel(label), CaptionLabel("选择站点、模型与思考等级"))
            primary = ComboBox()
            fallback = ComboBox()
            fallback.addItem("不使用灾备", "")
            for endpoint_name in endpoints:
                primary.addItem(endpoint_name, endpoint_name)
                fallback.addItem(endpoint_name, endpoint_name)
            primary.setCurrentIndex(max(0, primary.findData(route_value.get("primary", ""))))
            fallback.setCurrentIndex(max(0, fallback.findData(route_value.get("fallback", ""))))
            model = ComboBox()
            fallback_model = ComboBox()
            effort = ComboBox()
            for item in self.EFFORTS:
                effort.addItem(item, item)
            effort.setCurrentIndex(max(0, effort.findData(route_value.get("reasoning_effort", "medium"))))

            def fill_models(target: ComboBox, endpoint_name: str, current: str = "") -> None:
                target.clear()
                options = list(endpoints.get(endpoint_name, {}).get("models", []))
                default = str(endpoints.get(endpoint_name, {}).get("model") or "")
                for item in dict.fromkeys([current, default, *options]):
                    if item:
                        target.addItem(item, item)

            fill_models(model, str(primary.currentData() or ""), str(route_value.get("model") or ""))
            fill_models(fallback_model, str(fallback.currentData() or ""), str(route_value.get("fallback_model") or ""))
            primary.currentIndexChanged.connect(lambda _=0, p=primary, m=model: fill_models(m, str(p.currentData() or "")))
            fallback.currentIndexChanged.connect(lambda _=0, p=fallback, m=fallback_model: fill_models(m, str(p.currentData() or "")))
            form.addRow("使用站点", primary)
            form.addRow("使用模型", model)
            form.addRow("思考等级", effort)
            form.addRow("失败后灾备", fallback)
            form.addRow("灾备模型", fallback_model)
            root.addWidget(card)
            self.routes[route] = (primary, model, effort, fallback, fallback_model)
        actions = QHBoxLayout()
        cancel = PushButton("取消")
        cancel.clicked.connect(self.reject)
        save = PrimaryPushButton("保存组合")
        save.clicked.connect(self.accept)
        actions.addStretch(1)
        actions.addWidget(cancel)
        actions.addWidget(save)
        root.addLayout(actions)
        self.resize(760, 720)

    def result_value(self) -> tuple[str, dict[str, Any]]:
        value = {}
        for route, (primary, model, effort, fallback, fallback_model) in self.routes.items():
            row = {
                "primary": primary.currentData(),
                "model": model.currentData() or model.currentText(),
                "reasoning_effort": effort.currentData(),
            }
            if fallback.currentData():
                row["fallback"] = fallback.currentData()
                row["fallback_model"] = fallback_model.currentData() or fallback_model.currentText()
            value[route] = row
        return self.name.text().strip(), value


class AISettingsPage(QWidget):
    def __init__(self, controller):
        super().__init__()
        self.controller = controller
        self.scan_thread = None
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
        layout = QVBoxLayout(intro)
        layout.addWidget(_title("AI 配置"))
        layout.addWidget(BodyLabel("先添加站点并扫描模型，再按任务条件建立任意答案组合。"))
        layout.addWidget(CaptionLabel("省钱组合和 GPT-only 组合是内置模板，也可以复制、修改或另建组合。"))
        root.addWidget(intro)
        self.endpoint_table = TableWidget()
        self.endpoint_table.setColumnCount(4)
        self.endpoint_table.setHorizontalHeaderLabels(["站点", "API 地址", "协议", "模型数量"])
        configure_table(self.endpoint_table)
        root.addWidget(StrongBodyLabel("AI 站点"))
        root.addWidget(self.endpoint_table)
        endpoint_actions = QHBoxLayout()
        for text, callback in (("新增站点", self.add_endpoint), ("编辑", self.edit_endpoint), ("删除", self.delete_endpoint), ("扫描模型", self.scan_models)):
            button = PrimaryPushButton(text) if text == "新增站点" else PushButton(text)
            button.clicked.connect(callback)
            endpoint_actions.addWidget(button)
        endpoint_actions.addStretch(1)
        root.addLayout(endpoint_actions)
        self.combo_table = TableWidget()
        self.combo_table.setColumnCount(3)
        self.combo_table.setHorizontalHeaderLabels(["答案组合", "限时任务", "一般任务"])
        configure_table(self.combo_table)
        root.addWidget(StrongBodyLabel("条件式答案组合"))
        root.addWidget(self.combo_table)
        combo_actions = QHBoxLayout()
        for text, callback in (("新增组合", self.add_combination), ("编辑", self.edit_combination), ("复制", self.copy_combination), ("删除", self.delete_combination), ("设为默认", self.set_default)):
            button = PrimaryPushButton(text) if text == "新增组合" else PushButton(text)
            button.clicked.connect(callback)
            combo_actions.addWidget(button)
        combo_actions.addStretch(1)
        root.addLayout(combo_actions)
        root.addStretch(1)
        self.reload()

    def _models(self) -> dict[str, Any]:
        return self.controller.config.ensure().setdefault("models", {})

    def _save(self, models: dict[str, Any]) -> None:
        config = self.controller.config.ensure()
        config["models"] = models
        self.controller.config.save(config)
        self.reload()

    def reload(self) -> None:
        models = self._models()
        endpoints = models.setdefault("endpoints", {})
        self.endpoint_names = list(endpoints)
        self.endpoint_table.setRowCount(len(self.endpoint_names))
        for row, name in enumerate(self.endpoint_names):
            value = endpoints[name]
            visible = (name, value.get("base_url", ""), value.get("protocol", "responses"), len(value.get("models", [])))
            for column, item in enumerate(visible):
                self.endpoint_table.setItem(row, column, QTableWidgetItem(str(item)))
        combinations = models.setdefault("combinations", {})
        self.combination_names = list(combinations)
        self.combo_table.setRowCount(len(self.combination_names))
        for row, name in enumerate(self.combination_names):
            value = combinations[name]
            mark = "（默认）" if name == models.get("default") else ""
            visible = (name + mark, self._route_text(value.get("timed", {})), self._route_text(value.get("untimed", {})))
            for column, item in enumerate(visible):
                self.combo_table.setItem(row, column, QTableWidgetItem(str(item)))

    @staticmethod
    def _route_text(value: dict[str, Any]) -> str:
        return f"{value.get('primary', '-')} / {value.get('model', '-')} / {value.get('reasoning_effort', '-')}"

    def _selected(self, table: TableWidget, names: list[str]) -> str | None:
        row = table.currentRow()
        return names[row] if 0 <= row < len(names) else None

    def add_endpoint(self) -> None:
        self._edit_endpoint("")

    def edit_endpoint(self) -> None:
        name = self._selected(self.endpoint_table, self.endpoint_names)
        if name:
            self._edit_endpoint(name)

    def _edit_endpoint(self, name: str) -> None:
        models = self._models()
        endpoints = models.setdefault("endpoints", {})
        dialog = EndpointDialog(list(endpoints), name, endpoints.get(name), self)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        new_name, value = dialog.result_value()
        if name and name != new_name:
            endpoints.pop(name, None)
        value["models"] = endpoints.get(name, {}).get("models", [])
        endpoints[new_name] = value
        self._save(models)

    def delete_endpoint(self) -> None:
        name = self._selected(self.endpoint_table, self.endpoint_names)
        if not name:
            return
        models = self._models()
        models.setdefault("endpoints", {}).pop(name, None)
        self._save(models)

    def scan_models(self) -> None:
        name = self._selected(self.endpoint_table, self.endpoint_names)
        if not name:
            _notice(self, "扫描模型", "请先选择一个站点。")
            return
        endpoint = self._models().setdefault("endpoints", {}).get(name, {})
        self.scan_thread = ModelScanThread(name, endpoint)
        self.scan_thread.succeeded.connect(self._scan_succeeded)
        self.scan_thread.failed.connect(lambda _, message: _notice(self, "扫描模型失败", message))
        self.scan_thread.start()

    def _scan_succeeded(self, name: str, models_found: list[str]) -> None:
        models = self._models()
        models.setdefault("endpoints", {}).setdefault(name, {})["models"] = models_found
        self._save(models)
        _notice(self, "扫描模型", f"已读取 {len(models_found)} 个模型。")

    def add_combination(self) -> None:
        self._edit_combination("")

    def edit_combination(self) -> None:
        name = self._selected(self.combo_table, self.combination_names)
        if name:
            self._edit_combination(name)

    def copy_combination(self) -> None:
        name = self._selected(self.combo_table, self.combination_names)
        if name:
            self._edit_combination("", self._models()["combinations"][name])

    def _edit_combination(self, name: str, source: dict[str, Any] | None = None) -> None:
        models = self._models()
        combinations = models.setdefault("combinations", {})
        dialog = CombinationDialog(models.setdefault("endpoints", {}), name, source or combinations.get(name), self)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        new_name, value = dialog.result_value()
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", new_name):
            _notice(self, "答案组合", "组合标识格式不正确。")
            return
        if name and name != new_name:
            combinations.pop(name, None)
        combinations[new_name] = value
        self._save(models)

    def delete_combination(self) -> None:
        name = self._selected(self.combo_table, self.combination_names)
        if not name:
            return
        models = self._models()
        models.setdefault("combinations", {}).pop(name, None)
        if models.get("default") == name:
            models["default"] = next(iter(models["combinations"]), "")
        self._save(models)

    def set_default(self) -> None:
        name = self._selected(self.combo_table, self.combination_names)
        if name:
            models = self._models()
            models["default"] = name
            self._save(models)
