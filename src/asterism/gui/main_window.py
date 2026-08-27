from __future__ import annotations

import os
from pathlib import Path

from PyQt6.QtCore import QTimer
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import (
    QDialog,
    QHBoxLayout,
    QListWidget,
    QListWidgetItem,
    QMainWindow,
    QStackedWidget,
    QWidget,
)

from ..constants import PROVIDER_IDS
from .ai_settings_v2 import AISettingsPage as ModelAISettingsPage
from .common import display_provider, show_notice
from .controller import DesktopController
from .draft_page import DraftPage
from .fluent import (
    FLUENT_AVAILABLE,
    BodyLabel,
    FluentDialogBase,
    FluentIcon,
    FluentWindow,
    NavigationItemPosition,
    ThemeMode,
    apply_theme,
    configure_page_surface,
)
from .home_page import HomePage
from .provider_page import ProviderPage
from .settings_page import SettingsPage


class MainWindow(FluentWindow if FLUENT_AVAILABLE else QMainWindow):
    def __init__(self, data_root: Path | None = None, source_root: Path | None = None):
        super().__init__()
        self.controller = DesktopController.create(data_root, source_root)
        self.language_code = "zh-CN"
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
        if available is not None:
            self.move(
                available.x() + max(0, (available.width() - self.width()) // 2),
                available.y() + max(0, (available.height() - self.height()) // 2),
            )
        self.pages: list[QWidget] = []
        if FLUENT_AVAILABLE:
            self._init_fluent_shell()
        else:
            self._init_classic_shell()
        self.home_page = HomePage(self.controller)
        self.add_page("home", self.home_page)
        self.provider_pages: dict[str, ProviderPage] = {}
        for provider in PROVIDER_IDS:
            page = ProviderPage(self.controller, provider)
            self.provider_pages[provider] = page
            self.add_page(provider, page)
        self.draft_page = DraftPage(self.controller)
        self.ai_page = ModelAISettingsPage(self.controller)
        self.settings_page = SettingsPage(self.controller)
        self.add_page("drafts", self.draft_page)
        self.add_page("ai", self.ai_page)
        self.add_page("settings", self.settings_page)
        if not FLUENT_AVAILABLE:
            self.navigation.currentRowChanged.connect(self.stack.setCurrentIndex)
            self.navigation.setCurrentRow(0)
        QTimer.singleShot(0, self._show_first_run_wizard)

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
        self.navigation = None
        self.stack = None
        self.setMinimumSize(960, 640)

    def _show_first_run_wizard(self) -> None:
        """Give a local operator a safe first-run orientation without credentials."""
        if os.environ.get("ASTERISM_NONINTERACTIVE") == "1" or os.environ.get(
            "QT_QPA_PLATFORM", ""
        ).casefold() in {"offscreen", "minimal"}:
            return
        config = self.controller.config.ensure()
        if config.get("onboarding_completed") is True:
            return
        dialog = FluentDialogBase(
            "Asterism 首次启动",
            self,
            confirm_text="开始使用",
            cancel_text="稍后设置",
        )
        dialog.setModal(True)
        for text in (
            "这是单一可信操作者工具。平台账号、会话状态、日志、草稿和全局题库都保存在本机；"
            "不会创建 Asterism 用户，也不会把平台凭据上传到云端。",
            f"当前数据目录：{self.controller.paths.root}",
            "下一步：在左侧平台页新建账号，填写对应平台凭据；在 AI 配置页设置可选模型端点，"
            "然后先执行连接检查、登录和课程只读刷新。正式作业/考试仍需在草稿页人工确认。",
        ):
            label = BodyLabel(text)
            label.setWordWrap(True)
            dialog.content_layout.addWidget(label)
        dialog.set_content_size(660, 400)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            config["onboarding_completed"] = True
            self.controller.config.save(config)

    def add_page(self, name: str, page: QWidget) -> None:
        configure_page_surface(page)
        labels = {
            "home": "主页",
            "drafts": "草稿",
            "ai": "AI 配置",
            "settings": "设置",
        }
        label = labels.get(name, display_provider(name) if name in PROVIDER_IDS else name)
        if FLUENT_AVAILABLE:
            page.setObjectName(name.replace("-", "_"))
            icon = {
                "home": FluentIcon.HOME,
                "chaoxing": FluentIcon.LIBRARY,
                "welearn": FluentIcon.EDUCATION,
                "uai": FluentIcon.LANGUAGE,
                "cidaren": FluentIcon.CHAT,
                "drafts": FluentIcon.EDIT,
                "ai": FluentIcon.ROBOT,
                "settings": FluentIcon.SETTING,
            }.get(name, FluentIcon.APPLICATION)
            position = (
                NavigationItemPosition.BOTTOM if name == "settings" else NavigationItemPosition.TOP
            )
            self.addSubInterface(page, icon, label, position)
        else:
            self.navigation.addItem(QListWidgetItem(label))
            self.stack.addWidget(page)
        self.pages.append(page)

    @staticmethod
    def _thread_is_running(thread: object | None) -> bool:
        try:
            return bool(thread is not None and thread.isRunning())
        except (AttributeError, RuntimeError):
            return False

    def has_running_operations(self) -> bool:
        if any(
            self._thread_is_running(page.worker_thread)
            for page in self.provider_pages.values()
        ):
            return True
        if self._thread_is_running(self.draft_page.worker_thread):
            return True
        return self._thread_is_running(getattr(self.ai_page, "_scan_thread", None))

    def closeEvent(self, event) -> None:  # noqa: N802 - Qt API name
        if self.has_running_operations():
            show_notice(
                self,
                "Asterism",
                "仍有平台操作、草稿提交或模型扫描正在运行。请先等待完成，或在平台页取消后再退出。",
                "warning",
            )
            event.ignore()
            return
        super().closeEvent(event)
