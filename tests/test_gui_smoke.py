from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

try:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtWidgets import QApplication

    from asterism.gui.fluent import ThemeMode, apply_theme, configure_high_dpi
    from asterism.gui.main_window import MainWindow, ProviderPage
except ImportError:  # pragma: no cover - desktop extra is optional in CI
    QApplication = None


@unittest.skipUnless(QApplication is not None, "desktop dependencies are not installed")
class GuiSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        configure_high_dpi()
        cls.application = QApplication.instance() or QApplication(["asterism-gui-test"])

    def test_window_builds_all_provider_pages_and_switches_theme(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            apply_theme(self.application, ThemeMode.LIGHT)
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            window.show()
            pages = [page for page in window.pages if isinstance(page, ProviderPage)]
            self.assertEqual(
                [page.provider for page in pages], ["chaoxing", "welearn", "uai", "cidaren"]
            )
            chaoxing_page = pages[0]
            chaoxing_page._success(
                "questions",
                [
                    {
                        "kind": "matching",
                        "prompt": "图文混编题",
                        "options": [{"text": "A", "image": "https://example.test/a.png"}],
                        "remote_id": "remote-1",
                    }
                ],
            )
            self.assertEqual(chaoxing_page.question_table.rowCount(), 1)
            self.assertEqual(chaoxing_page.question_table.item(0, 0).text(), "matching")
            for mode in ThemeMode:
                self.assertEqual(apply_theme(self.application, mode), mode)
            self.assertGreater(window.width(), 0)
            self.assertGreater(window.height(), 0)
            window.close()
