from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

try:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtWidgets import QApplication, QWidget

    from asterism.gui.app import acquire_instance_lock
    from asterism.gui.fluent import (
        LineEdit,
        PushButton,
        TableWidget,
        TextEdit,
        ThemeMode,
        apply_theme,
        configure_high_dpi,
    )
    from asterism.gui.main_window import DraftPage, MainWindow, ProviderPage
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
            self.assertEqual(chaoxing_page.question_table.item(0, 1).text(), "matching")
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

    def test_provider_progress_is_visible_on_home_activity_card(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                page
                for page in window.pages
                if isinstance(page, ProviderPage) and page.provider == "chaoxing"
            )
            page.profile_combo.setCurrentIndex(0)
            page._event_received(
                "run",
                {"type": "progress", "current": 2, "total": 7, "message": "章节 2"},
            )
            self.assertIn("chaoxing", window.home_page.activity.text())
            self.assertIn("2/7", window.home_page.activity.text())
            self.assertIn("章节 2", window.home_page.activity.text())
            window.close()

    def test_data_root_allows_only_one_desktop_instance(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            first = acquire_instance_lock(temporary)
            self.assertIsNotNone(first)
            second = acquire_instance_lock(temporary)
            self.assertIsNone(second)
            first.unlock()
            third = acquire_instance_lock(temporary)
            self.assertIsNotNone(third)
            third.unlock()

    def test_noninteractive_first_run_skips_modal_wizard(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            old = os.environ.get("ASTERISM_NONINTERACTIVE")
            os.environ["ASTERISM_NONINTERACTIVE"] = "1"
            try:
                window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
                self.assertFalse(window.controller.config.ensure().get("onboarding_completed"))
                window.close()
            finally:
                if old is None:
                    os.environ.pop("ASTERISM_NONINTERACTIVE", None)
                else:
                    os.environ["ASTERISM_NONINTERACTIVE"] = old

    def test_english_preference_localizes_navigation_labels(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config_path = Path(temporary) / "config.local.json"
            config_path.write_text(
                '{"version": 1, "onboarding_completed": true, '
                '"ui": {"theme": "system", "language": "en-US"}, '
                '"notifications": {"enabled": false, "command": ""}, '
                '"models": {}, "providers": {}}',
                encoding="utf-8",
            )
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            self.assertEqual(window.language_code, "en-US")
            if window.navigation is not None:
                labels = [
                    window.navigation.item(i).text()
                    for i in range(window.navigation.count())
                ]
                self.assertIn("Home", labels)
            window.close()

    def test_draft_page_reports_busy_while_submit_thread_is_running(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(page for page in window.pages if isinstance(page, DraftPage))
            page.worker_thread = SimpleNamespace(isRunning=lambda: True)
            self.assertTrue(page._operation_running())
            page.worker_thread = SimpleNamespace(isRunning=lambda: False)
            self.assertFalse(page._operation_running())
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

    def test_only_chaoxing_exposes_batch_concurrency(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            pages = {
                page.provider: page
                for page in window.pages
                if isinstance(page, ProviderPage)
            }
            pages["chaoxing"]._ask_concurrency = lambda: 8
            self.assertEqual(pages["chaoxing"]._batch_concurrency(), 8)
            for provider in ("welearn", "uai", "cidaren"):
                pages[provider]._ask_concurrency = lambda provider=provider: self.fail(
                    f"{provider} must not ask for cross-task concurrency"
                )
                self.assertEqual(pages[provider]._batch_concurrency(), 1)
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

    def test_oauth_authorization_url_is_redacted_from_log_preview(self) -> None:
        url = "https://example.test/oauth?state=temporary-secret"
        safe = ProviderPage._safe_preview({"authorization_url": url})
        self.assertNotIn("temporary-secret", str(safe))
        self.assertEqual(safe["authorization_url"], "<redacted authorization url>")

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

    def test_scan_status_dialog_exposes_progress_fields_and_retry_action(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                page
                for page in window.pages
                if isinstance(page, ProviderPage) and page.provider == "chaoxing"
            )
            profile = page.controller.profiles.create("chaoxing", "scan-status")
            page.reload_profiles()
            page.profile_combo.setCurrentIndex(page.profile_combo.findData(profile.id))
            page.controller.states.save(
                profile,
                "scan",
                {
                    "state": "failed",
                    "phase": "questions:task-1",
                    "course_count": 2,
                    "task_count": 4,
                    "question_count": 7,
                    "completed_tasks": 3,
                    "retries": 2,
                    "cursor": "task-3",
                    "last_error": "network",
                },
            )
            page.scan_status()
            dialog = next(
                child
                for child in page.findChildren(QWidget)
                if child.windowTitle() == "chaoxing scan status"
            )
            self.assertIn("题目数", dialog.findChildren(TableWidget)[0].item(4, 0).text())
            self.assertTrue(dialog.findChildren(PushButton))
            dialog.close()
            window.close()
