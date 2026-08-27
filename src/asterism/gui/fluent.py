from __future__ import annotations

import os
from enum import StrEnum

from PyQt6.QtCore import QSize, Qt
from PyQt6.QtGui import QColor, QPalette
from PyQt6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDialog,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QPushButton,
    QRadioButton,
    QScrollArea,
    QTableWidget,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)


class ThemeMode(StrEnum):
    SYSTEM = "system"
    LIGHT = "light"
    DARK = "dark"


_DIALOG_FALLBACK_PARENT: QWidget | None = None


_HEADLESS_PLATFORM = os.environ.get("QT_QPA_PLATFORM", "").casefold() in {
    "offscreen",
    "minimal",
}

try:  # qfluentwidgets is optional during headless development and CI.
    if _HEADLESS_PLATFORM:
        raise ImportError("use native Qt widgets with a headless platform plugin")
    from qfluentwidgets import (
        BodyLabel,
        CaptionLabel,
        CardWidget,
        CheckBox,
        EditableComboBox,
        FluentIcon,
        FluentWindow,
        InfoBar,
        ListWidget,
        MessageBox,
        MessageBoxBase,
        MSFluentWindow,
        NavigationInterface,
        NavigationItemPosition,
        PrimaryPushButton,
        PushButton,
        RadioButton,
        StrongBodyLabel,
        SubtitleLabel,
        TitleLabel,
    )
    from qfluentwidgets.common.icon import toQIcon

    try:
        from qfluentwidgets import ComboBox, LineEdit, ScrollArea, TableWidget, TextEdit
    except ImportError:  # pragma: no cover - old qfluentwidgets releases
        ComboBox, LineEdit, TableWidget, TextEdit = QComboBox, QLineEdit, QTableWidget, QTextEdit
        EditableComboBox = QComboBox
        ScrollArea = QScrollArea
    FLUENT_AVAILABLE = True
except ImportError:  # pragma: no cover - exercised on minimal dev environments
    BodyLabel = QLabel
    CaptionLabel = QLabel
    CheckBox = QCheckBox
    StrongBodyLabel = QLabel
    TitleLabel = QLabel
    SubtitleLabel = QLabel
    PrimaryPushButton = QPushButton
    PushButton = QPushButton
    RadioButton = QRadioButton
    ComboBox, LineEdit, TableWidget, TextEdit = QComboBox, QLineEdit, QTableWidget, QTextEdit
    EditableComboBox = QComboBox
    ListWidget = QListWidget
    ScrollArea = QScrollArea
    CardWidget = QWidget
    FluentIcon = None
    toQIcon = None
    FluentWindow = object
    MSFluentWindow = object
    InfoBar = MessageBox = NavigationInterface = NavigationItemPosition = None
    MessageBoxBase = QDialog
    FLUENT_AVAILABLE = False


def _dark_palette() -> QPalette:
    palette = QPalette()
    palette.setColor(QPalette.ColorRole.Window, QColor("#202020"))
    palette.setColor(QPalette.ColorRole.WindowText, QColor("#f3f3f3"))
    palette.setColor(QPalette.ColorRole.Base, QColor("#171717"))
    palette.setColor(QPalette.ColorRole.AlternateBase, QColor("#242424"))
    palette.setColor(QPalette.ColorRole.Text, QColor("#f3f3f3"))
    palette.setColor(QPalette.ColorRole.Button, QColor("#2d2d2d"))
    palette.setColor(QPalette.ColorRole.ButtonText, QColor("#f3f3f3"))
    palette.setColor(QPalette.ColorRole.Highlight, QColor("#4cc2ff"))
    palette.setColor(QPalette.ColorRole.HighlightedText, QColor("#101010"))
    return palette


def _light_palette() -> QPalette:
    palette = QPalette()
    palette.setColor(QPalette.ColorRole.Window, QColor("#f3f3f3"))
    palette.setColor(QPalette.ColorRole.WindowText, QColor("#1b1b1b"))
    palette.setColor(QPalette.ColorRole.Base, QColor("#ffffff"))
    palette.setColor(QPalette.ColorRole.AlternateBase, QColor("#f7f7f7"))
    palette.setColor(QPalette.ColorRole.Text, QColor("#1b1b1b"))
    palette.setColor(QPalette.ColorRole.Button, QColor("#fbfbfb"))
    palette.setColor(QPalette.ColorRole.ButtonText, QColor("#1b1b1b"))
    palette.setColor(QPalette.ColorRole.Highlight, QColor("#009faa"))
    palette.setColor(QPalette.ColorRole.HighlightedText, QColor("#ffffff"))
    return palette


def _system_is_dark(app) -> bool:
    try:
        scheme = app.styleHints().colorScheme()
        if scheme is Qt.ColorScheme.Dark:
            return True
        if scheme is Qt.ColorScheme.Light:
            return False
    except (AttributeError, RuntimeError):
        pass
    return app.palette().color(QPalette.ColorRole.Window).lightness() < 128


def apply_theme(app, mode: str | ThemeMode) -> ThemeMode:
    """Apply a theme without making the desktop depend on qfluentwidgets."""
    values = {item.value for item in ThemeMode}
    selected = ThemeMode(str(mode)) if str(mode) in values else ThemeMode.SYSTEM
    effective_dark = (
        _system_is_dark(app) if selected is ThemeMode.SYSTEM else selected is ThemeMode.DARK
    )
    if FLUENT_AVAILABLE:
        try:
            from qfluentwidgets import Theme, setTheme

            setTheme(
                Theme.AUTO
                if selected is ThemeMode.SYSTEM
                else Theme.DARK
                if effective_dark
                else Theme.LIGHT
            )
        except (ImportError, AttributeError):
            pass
    # qfluentwidgets themes its own controls but intentionally leaves ordinary
    # QWidget page surfaces to the application palette.  Without a matching
    # palette, switching a running app to dark mode produces white cards with
    # white Fluent text.  Use fixed Fluent-compatible palettes instead of the
    # Windows platform palette, whose accent colours can leak into pages.
    palette = _dark_palette() if effective_dark else _light_palette()
    app.setPalette(palette)
    if not FLUENT_AVAILABLE:
        app.setStyleSheet(
            """
            QTableWidget { gridline-color: #555; }
            QHeaderView::section { padding: 6px; }
            QToolTip { color: #f3f3f3; background: #303030; border: 1px solid #666; }
            """
            if effective_dark
            else ""
        )
    # qfluentwidgets' stacked window keeps the palette it had when each page
    # was inserted.  Refresh only Asterism's page surfaces after all global
    # style changes so transparent scroll areas and cards compose against the
    # new theme.  A fresh palette also clears old explicit colour roles.
    for widget in app.allWidgets():
        if widget.property("asterism_page_surface") is True:
            widget.setPalette(_dark_palette() if effective_dark else _light_palette())
            widget.update()
    app.setProperty("asterism_theme", selected.value)
    return selected


def configure_page_surface(page: QWidget) -> None:
    """Give a navigation page an opaque, theme-aware Fluent base surface."""
    from PyQt6.QtWidgets import QApplication

    page.setProperty("asterism_page_surface", True)
    page.setAutoFillBackground(True)
    app = QApplication.instance()
    if app is not None:
        mode = str(app.property("asterism_theme") or ThemeMode.SYSTEM.value)
        effective_dark = _system_is_dark(app) if mode == ThemeMode.SYSTEM.value else mode == "dark"
        page.setPalette(_dark_palette() if effective_dark else _light_palette())


def configure_high_dpi() -> None:
    """Enable Qt high-DPI behavior before QApplication is constructed."""
    from PyQt6.QtWidgets import QApplication

    os.environ.setdefault("QT_ENABLE_HIGHDPI_SCALING", "1")
    for name in ("AA_EnableHighDpiScaling", "AA_UseHighDpiPixmaps"):
        attribute = getattr(Qt.ApplicationAttribute, name, None)
        if attribute is not None:
            QApplication.setAttribute(attribute, True)
    policy = getattr(Qt.HighDpiScaleFactorRoundingPolicy, "PassThrough", None)
    if policy is not None:
        QApplication.setHighDpiScaleFactorRoundingPolicy(policy)


def configure_table(table: QTableWidget, *, minimum_row_height: int = 30) -> None:
    table.setAlternatingRowColors(True)
    table.setWordWrap(False)
    table.setTextElideMode(Qt.TextElideMode.ElideRight)
    table.verticalHeader().setDefaultSectionSize(minimum_row_height)
    table.horizontalHeader().setStretchLastSection(True)
    table.horizontalHeader().setSectionsMovable(False)


def form_label(text: str) -> BodyLabel:
    """Create a Fluent label for ``QFormLayout`` instead of its native QLabel shortcut."""
    label = BodyLabel(text)
    label.setMinimumWidth(112)
    return label


def wrapped_caption(text: str) -> CaptionLabel:
    label = CaptionLabel(text)
    label.setWordWrap(True)
    return label


def configure_scroll_area(scroll: QScrollArea) -> None:
    """Make a page scroll surface inherit the Fluent window background.

    QScrollArea's native viewport is opaque by default, which produces the
    grey Qt panel visible on every page after the first one.  qfluentwidgets
    exposes the same setting as ``enableTransparentBackground``; keep a
    compatible fallback for headless/native Qt runs.
    """
    if hasattr(scroll, "enableTransparentBackground"):
        scroll.enableTransparentBackground()
    else:
        scroll.setFrameShape(QScrollArea.Shape.NoFrame)
        scroll.setStyleSheet("QScrollArea { border: none; background: transparent; }")
        scroll.viewport().setStyleSheet("background: transparent;")
    scroll.viewport().setAutoFillBackground(False)


def _dialog_parent(parent: QWidget | None) -> QWidget | None:
    global _DIALOG_FALLBACK_PARENT
    if parent is not None:
        return parent.window() if hasattr(parent, "window") else parent
    try:
        from PyQt6.QtWidgets import QApplication

        active = QApplication.activeWindow()
        if active is not None:
            return active
        if not FLUENT_AVAILABLE:
            return None
        if _DIALOG_FALLBACK_PARENT is None:
            _DIALOG_FALLBACK_PARENT = QWidget()
            _DIALOG_FALLBACK_PARENT.setWindowTitle("Asterism")
            _DIALOG_FALLBACK_PARENT.resize(720, 480)
            screen = QApplication.primaryScreen()
            if screen is not None:
                available = screen.availableGeometry()
                _DIALOG_FALLBACK_PARENT.move(
                    available.x() + (available.width() - 720) // 2,
                    available.y() + (available.height() - 480) // 2,
                )
            # qfluentwidgets' mask dialogs require a visible parent window.
            # This owner is used only for startup errors that occur before the
            # real main window exists (for example, a duplicate data-root).
            _DIALOG_FALLBACK_PARENT.show()
        return _DIALOG_FALLBACK_PARENT
    except (ImportError, RuntimeError):
        return None


class FluentDialogBase(MessageBoxBase):
    """Common Fluent dialog shell with a consistent content area and actions.

    qfluentwidgets' ``MessageBoxBase`` supplies the themed window surface and
    standard Fluent buttons.  The native QDialog fallback is retained solely
    for headless tests where qfluentwidgets is intentionally not loaded.
    """

    def __init__(
        self,
        title: str,
        parent: QWidget | None = None,
        *,
        confirm_text: str = "确定",
        cancel_text: str = "取消",
        show_confirm: bool = True,
        show_cancel: bool = True,
    ) -> None:
        if FLUENT_AVAILABLE:
            super().__init__(_dialog_parent(parent))
            self.setWindowTitle(title)
            self.yesButton.setText(confirm_text)
            self.cancelButton.setText(cancel_text)
            if not show_confirm:
                self.hideYesButton()
            if not show_cancel:
                self.hideCancelButton()
            self.content_layout = self.viewLayout
        else:
            super().__init__(_dialog_parent(parent))
            self.setWindowTitle(title)
            outer = QVBoxLayout(self)
            outer.setContentsMargins(24, 20, 24, 22)
            self.content_layout = QVBoxLayout()
            outer.addLayout(self.content_layout, 1)
            self.yesButton = PrimaryPushButton(confirm_text)
            self.cancelButton = PushButton(cancel_text)
            self.yesButton.setVisible(show_confirm)
            self.cancelButton.setVisible(show_cancel)
            buttons = QHBoxLayout()
            buttons.addStretch(1)
            buttons.addWidget(self.cancelButton)
            buttons.addWidget(self.yesButton)
            outer.addLayout(buttons)
            self.yesButton.clicked.connect(self._accept_fallback)
            self.cancelButton.clicked.connect(self.reject)
        self.heading = SubtitleLabel(title)
        self.heading.setMinimumHeight(30)
        self.content_layout.addWidget(self.heading)
        self._validator = None
        self._requested_content_size = QSize(420, 0)
        if FLUENT_AVAILABLE:
            self.widget.setMinimumWidth(420)

    def set_validator(self, validator) -> None:
        self._validator = validator

    def validate(self) -> bool:
        return True if self._validator is None else bool(self._validator())

    def _accept_fallback(self) -> None:
        if self.validate():
            self.accept()

    def set_content_size(self, width: int, height: int = 0) -> None:
        """Size the centered Fluent card without shrinking its full-window mask."""
        self._requested_content_size = QSize(max(320, width), max(0, height))
        if not FLUENT_AVAILABLE:
            self.resize(width, height)
            return
        self.widget.setMinimumWidth(self._requested_content_size.width())
        if height > 0:
            self.widget.setMinimumHeight(height)

    def showEvent(self, event) -> None:  # noqa: N802 - Qt API name
        if FLUENT_AVAILABLE:
            parent = self.parentWidget()
            if parent is not None:
                # MaskDialogBase is a child overlay.  It must always cover the
                # entire parent; sizing the QDialog itself leaves the card and
                # its shadow stuck at the screen's upper-left corner.
                self.setGeometry(0, 0, parent.width(), parent.height())
            available_width = max(320, self.width() - 64)
            self.widget.setMaximumWidth(available_width)
            self.widget.setMinimumWidth(
                min(self._requested_content_size.width(), available_width)
            )
            if self._requested_content_size.height() > 0:
                self.widget.setMinimumHeight(
                    min(self._requested_content_size.height(), max(200, self.height() - 64))
                )
            for label in self.widget.findChildren(QLabel):
                if label is not self.heading:
                    label.setWordWrap(True)
        super().showEvent(event)
