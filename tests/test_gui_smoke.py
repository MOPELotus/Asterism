from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

try:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtWidgets import QApplication, QWidget

    from asterism.gui.fluent import (
        LineEdit,
        TextEdit,
        ThemeMode,
        apply_theme,
        configure_high_dpi,
    )
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
            chaoxing_page._event_received(
                "run", {"type": "progress", "current": 2, "total": 5, "message": "working"}
            )
            self.assertIn("progress 2/5 working", chaoxing_page.log.toPlainText())
            safe = ProviderPage._safe_preview(
                [
                    SimpleNamespace(
                        task_remote_id="task-1",
                        error_code=None,
                        result=SimpleNamespace(
                            operation="run", data={"session": {"token": "secret-token"}}
                        ),
                    )
                ]
            )
            self.assertNotIn("secret-token", str(safe))
            for mode in ThemeMode:
                self.assertEqual(apply_theme(self.application, mode), mode)
            self.assertGreater(window.width(), 0)
            self.assertGreater(window.height(), 0)
            window.close()

    def test_mixed_batch_result_counts_routine_failures(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                page
                for page in window.pages
                if isinstance(page, ProviderPage) and page.provider == "chaoxing"
            )
            page._success(
                "batch run",
                {
                    "drafts": [SimpleNamespace(id="draft-1")],
                    "routine_results": [
                        SimpleNamespace(error_code=None),
                        SimpleNamespace(error_code="network"),
                    ],
                },
            )
            text = page.log.toPlainText()
            self.assertIn("[draft] draft-1", text)
            self.assertIn('"failed":1', text)
            window.close()

    def test_cidaren_oauth_result_exposes_copyable_authorization_dialog(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                page
                for page in window.pages
                if isinstance(page, ProviderPage) and page.provider == "cidaren"
            )
            page._show_oauth_authorization(
                "https://open.weixin.qq.com/connect/oauth2/authorize?x=1"
            )
            dialog = next(
                child
                for child in page.findChildren(QWidget)
                if child.windowTitle() == "cidaren OAuth"
            )
            self.assertTrue(dialog.findChildren(LineEdit))
            dialog.close()
            window.close()

    def test_provider_task_detail_dialog_is_read_only_and_sanitized(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                page
                for page in window.pages
                if isinstance(page, ProviderPage) and page.provider == "chaoxing"
            )
            page.current_routine_tasks = [
                {
                    "remote_id": "task-1",
                    "title": "detail",
                    "native": {"route_kind": "knowledge_point", "token": "secret"},
                }
            ]
            page.task_table.setRowCount(1)
            page.task_table.selectRow(0)
            page.show_task_detail()
            dialog = next(
                child
                for child in page.findChildren(QWidget)
                if child.windowTitle() == "chaoxing task detail"
            )
            viewer = dialog.findChildren(TextEdit)[0]
            self.assertTrue(viewer.isReadOnly())
            self.assertNotIn("secret", viewer.toPlainText())
            dialog.close()
            window.close()
