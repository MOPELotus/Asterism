from __future__ import annotations

import os
from enum import StrEnum

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QColor, QPalette
from PyQt6.QtWidgets import QComboBox, QLabel, QLineEdit, QPushButton, QTableWidget, QTextEdit, QWidget


class ThemeMode(StrEnum):
    SYSTEM = "system"
    LIGHT = "light"
    DARK = "dark"


_HEADLESS_PLATFORM = os.environ.get("QT_QPA_PLATFORM", "").casefold() in {
    "offscreen",
    "minimal",
}

try:  # qfluentwidgets is optional during headless development and CI.
    if _HEADLESS_PLATFORM:
        raise ImportError("use native Qt widgets with a headless platform plugin")
    from qfluentwidgets import (
        BodyLabel,
        CardWidget,
        CaptionLabel,
        FluentIcon,
        FluentWindow,
        MSFluentWindow,
        InfoBar,
        MessageBox,
        NavigationItemPosition,
        NavigationInterface,
        PrimaryPushButton,
        PushButton,
        StrongBodyLabel,
        SubtitleLabel,
        TitleLabel,
    )

    try:
        from qfluentwidgets import ComboBox, LineEdit, TableWidget, TextEdit
    except ImportError:  # pragma: no cover - old qfluentwidgets releases
        ComboBox, LineEdit, TableWidget, TextEdit = QComboBox, QLineEdit, QTableWidget, QTextEdit
    FLUENT_AVAILABLE = True
except ImportError:  # pragma: no cover - exercised on minimal dev environments
    BodyLabel = QLabel
    CaptionLabel = QLabel
    StrongBodyLabel = QLabel
    TitleLabel = QLabel
    SubtitleLabel = QLabel
    PrimaryPushButton = QPushButton
    PushButton = QPushButton
    ComboBox, LineEdit, TableWidget, TextEdit = QComboBox, QLineEdit, QTableWidget, QTextEdit
    CardWidget = QWidget
    FluentIcon = None
    FluentWindow = object
    MSFluentWindow = object
    InfoBar = MessageBox = NavigationInterface = NavigationItemPosition = None
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


def _system_is_dark(app) -> bool:
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
    app.setPalette(_dark_palette() if effective_dark else app.style().standardPalette())
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
    app.setProperty("asterism_theme", selected.value)
    return selected


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
