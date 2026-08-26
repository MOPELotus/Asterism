from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .fluent import configure_high_dpi


def main(argv: list[str] | None = None) -> int:
    from PyQt6.QtWidgets import QApplication

    from .main_window import MainWindow

    configure_high_dpi()
    parser = argparse.ArgumentParser(prog="asterism-gui")
    parser.add_argument("--data-root", type=Path)
    parser.add_argument("--source-root", type=Path)
    args = parser.parse_args(argv)
    application = QApplication([sys.argv[0]])
    application.setApplicationName("Asterism")
    application.setOrganizationName("Asterism")
    window = MainWindow(args.data_root, args.source_root)
    window.show()
    return application.exec()
