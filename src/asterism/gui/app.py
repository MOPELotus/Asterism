from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from ..paths import DataPaths
from .fluent import configure_high_dpi


def acquire_instance_lock(data_root: str | Path | None = None) -> Any | None:
    """Own one writable desktop process per local data directory."""
    from PyQt6.QtCore import QLockFile

    paths = DataPaths.resolve(data_root)
    paths.initialize()
    lock = QLockFile(str(paths.root / ".asterism.lock"))
    return lock if lock.tryLock(0) else None


def main(argv: list[str] | None = None) -> int:
    from PyQt6.QtWidgets import QApplication, QMessageBox

    from .main_window import MainWindow

    configure_high_dpi()
    parser = argparse.ArgumentParser(prog="asterism-gui")
    parser.add_argument("--data-root", type=Path)
    parser.add_argument("--source-root", type=Path)
    args = parser.parse_args(argv)
    application = QApplication([sys.argv[0]])
    application.setApplicationName("Asterism")
    application.setOrganizationName("Asterism")
    instance_lock = acquire_instance_lock(args.data_root)
    if instance_lock is None:
        QMessageBox.critical(
            None,
            "Asterism",
            "相同数据目录已有 Asterism 实例正在运行。",
        )
        return 2
    try:
        window = MainWindow(args.data_root, args.source_root)
        window.show()
        return application.exec()
    finally:
        instance_lock.unlock()
