from __future__ import annotations

import json
from typing import Any

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import QHBoxLayout, QVBoxLayout, QWidget

from .common import make_title, show_notice
from .controller import DesktopController
from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    CheckBox,
    ComboBox,
    LineEdit,
    PrimaryPushButton,
    ScrollArea,
    StrongBodyLabel,
    TextEdit,
    ThemeMode,
    apply_theme,
    configure_scroll_area,
    wrapped_caption,
)


class SettingsPage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
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
        intro_layout.addWidget(make_title("设置"))
        intro_layout.addWidget(BodyLabel("主题、通知和平台默认值保存在本机配置。"))
        intro_layout.addWidget(
            CaptionLabel("配置内容不会自动上传；凭据和会话状态保存在本地数据目录。")
        )
        root.addWidget(intro)
        appearance = CardWidget()
        appearance_layout = QHBoxLayout(appearance)
        appearance_layout.setContentsMargins(20, 14, 20, 14)
        appearance_layout.addWidget(StrongBodyLabel("外观主题"))
        self.theme = ComboBox()
        self.theme.addItem("跟随系统", userData=ThemeMode.SYSTEM.value)
        self.theme.addItem("浅色", userData=ThemeMode.LIGHT.value)
        self.theme.addItem("深色", userData=ThemeMode.DARK.value)
        current_theme = str(
            controller.config.ensure().get("ui", {}).get("theme", ThemeMode.SYSTEM.value)
        )
        self.theme.setCurrentIndex(max(0, self.theme.findData(current_theme)))
        self.theme.currentIndexChanged.connect(self.apply_selected_theme)
        appearance_layout.addWidget(self.theme)
        appearance_layout.addStretch(1)
        root.addWidget(appearance)
        notification = CardWidget()
        notification_layout = QVBoxLayout(notification)
        notification_layout.setContentsMargins(20, 16, 20, 16)
        notification_layout.setSpacing(8)
        notification_layout.addWidget(StrongBodyLabel("终态通知"))
        notification_layout.addWidget(
            wrapped_caption(
                "可选；仅在手动执行成功或失败后调用本机命令，不包含定时巡检或后台调度。"
            )
        )
        self.notifications_enabled = CheckBox("启用执行结果通知")
        notification_layout.addWidget(self.notifications_enabled)
        self.notification_command = LineEdit()
        self.notification_command.setPlaceholderText(
            "通知程序及固定参数；执行时会在末尾自动附加一段结果 JSON"
        )
        notification_layout.addWidget(self.notification_command)
        root.addWidget(notification)
        editor_card = CardWidget()
        editor_layout = QVBoxLayout(editor_card)
        editor_layout.setContentsMargins(20, 16, 20, 16)
        editor_layout.setSpacing(8)
        editor_layout.addWidget(StrongBodyLabel("平台高级参数"))
        editor_layout.addWidget(
            CaptionLabel(
                "只编辑 providers 对象；答案组合、主题和通知分别由对应页面或控件管理。"
            )
        )
        self.editor = TextEdit()
        self.editor.setPlainText(
            json.dumps(
                controller.config.ensure().get("providers", {}),
                ensure_ascii=False,
                indent=2,
            )
        )
        editor_layout.addWidget(self.editor)
        save = PrimaryPushButton("保存配置")
        save.clicked.connect(self.save)
        editor_layout.addWidget(save, 0, Qt.AlignmentFlag.AlignLeft)
        root.addWidget(editor_card, 1)
        self._load_notification_controls(controller.config.ensure())

    def _load_notification_controls(self, value: dict[str, Any]) -> None:
        notifications = value.get("notifications")
        notifications = notifications if isinstance(notifications, dict) else {}
        self.notifications_enabled.setChecked(bool(notifications.get("enabled", False)))
        self.notification_command.setText(str(notifications.get("command") or ""))

    def reload_config(self) -> None:
        value = self.controller.config.ensure()
        current_theme = str(value.get("ui", {}).get("theme", ThemeMode.SYSTEM.value))
        self.theme.blockSignals(True)
        self.theme.setCurrentIndex(max(0, self.theme.findData(current_theme)))
        self.theme.blockSignals(False)
        self._load_notification_controls(value)
        self.editor.setPlainText(
            json.dumps(value.get("providers", {}), ensure_ascii=False, indent=2)
        )

    def save(self) -> None:
        try:
            providers = json.loads(self.editor.toPlainText())
            if not isinstance(providers, dict):
                raise ValueError("平台参数必须是 JSON 对象")
            if any(not isinstance(item, dict) for item in providers.values()):
                raise ValueError("每个平台的参数必须是 JSON 对象")
            notification_command = self.notification_command.text().strip()
            if self.notifications_enabled.isChecked() and not notification_command:
                raise ValueError("启用终态通知前必须填写本机通知命令")
            value = self.controller.config.ensure()
            value["providers"] = providers
            value.setdefault("notifications", {})["enabled"] = (
                self.notifications_enabled.isChecked()
            )
            value.setdefault("notifications", {})["command"] = (
                notification_command
            )
            self.controller.config.save(value)
            app = QGuiApplication.instance()
            if app is not None:
                apply_theme(app, str(value.get("ui", {}).get("theme", "system")))
            show_notice(self, "设置", "配置已保存并应用。")
        except (ValueError, OSError, TypeError) as error:
            show_notice(self, "设置", str(error), "error")

    def apply_selected_theme(self) -> None:
        try:
            persisted = self.controller.config.ensure()
            persisted.setdefault("ui", {})["theme"] = self.theme.currentData()
            persisted.setdefault("ui", {})["language"] = "zh-CN"
            self.controller.config.save(persisted)
            app = QGuiApplication.instance()
            if app is not None:
                apply_theme(app, str(self.theme.currentData()))
        except (TypeError, ValueError, json.JSONDecodeError) as error:
            self.log_theme_error(str(error))

    def log_theme_error(self, message: str) -> None:
        # Keep theme selection non-destructive if the JSON editor is temporarily invalid.
        self.theme.setToolTip(f"配置暂不可解析：{message}")
