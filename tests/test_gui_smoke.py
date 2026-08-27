from __future__ import annotations

import ast
import json
import os
import tempfile
import threading
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

try:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtCore import Qt
    from PyQt6.QtWidgets import QApplication, QWidget

    from asterism.gui.ai_settings_v2 import AISettingsPage
    from asterism.gui.app import acquire_instance_lock
    from asterism.gui.fluent import (
        FluentDialogBase,
        LineEdit,
        PushButton,
        TableWidget,
        ThemeMode,
        apply_theme,
        configure_high_dpi,
    )
    from asterism.gui.main_window import DraftPage, MainWindow, ProviderPage
    from asterism.runner import RunnerError
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
            self.assertEqual(chaoxing_page.question_table.item(0, 1).text(), "连线题")
            chaoxing_page._event_received(
                "run", {"type": "progress", "current": 2, "total": 5, "message": "working"}
            )
            self.assertIn("进度 2/5 working", chaoxing_page.log.toPlainText())
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
            apply_theme(self.application, ThemeMode.DARK)
            self.assertLess(
                window.home_page.palette().color(window.home_page.backgroundRole()).lightness(),
                128,
            )
            apply_theme(self.application, ThemeMode.LIGHT)
            self.assertGreater(
                window.home_page.palette().color(window.home_page.backgroundRole()).lightness(),
                128,
            )
            self.assertGreater(window.width(), 0)
            self.assertGreater(window.height(), 0)
            window.close()

    def test_every_page_button_has_an_interaction_handler(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            disconnected = []
            for page in window.pages:
                for button in page.findChildren(PushButton):
                    if button.receivers(button.clicked) < 1:
                        disconnected.append(
                            f"{page.objectName() or type(page).__name__}:{button.text()}"
                        )
            self.assertEqual(disconnected, [])
            window.close()

    def test_ai_page_exposes_custom_sites_and_combinations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(item for item in window.pages if isinstance(item, AISettingsPage))
            self.assertGreater(page.site_choice.count(), 0)
            self.assertGreater(page.combo_choice.count(), 0)
            self.assertIn("economy", page.combination_names)
            self.assertIn("gpt_only", page.combination_names)
            model_editor = page.route_cards["timed"]["model"]
            if hasattr(model_editor, "setText"):
                model_editor.setText("manual-model-id")
            else:
                model_editor.setEditText("manual-model-id")
            self.assertEqual(model_editor.currentText(), "manual-model-id")
            window.close()

    def test_fluent_combo_items_never_pass_user_data_as_the_icon_argument(self) -> None:
        gui_root = Path(__file__).resolve().parents[1] / "src" / "asterism" / "gui"
        failures = []
        for path in gui_root.glob("*.py"):
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                if (
                    isinstance(node, ast.Call)
                    and isinstance(node.func, ast.Attribute)
                    and node.func.attr == "addItem"
                    and len(node.args) > 1
                ):
                    failures.append(f"{path.name}:{node.lineno}")
        self.assertEqual(failures, [], "Fluent ComboBox requires userData= for stored values")

    def test_fluent_input_controls_do_not_receive_text_as_a_parent_argument(self) -> None:
        gui_root = Path(__file__).resolve().parents[1] / "src" / "asterism" / "gui"
        failures = []
        for path in gui_root.glob("*.py"):
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                if (
                    isinstance(node, ast.Call)
                    and isinstance(node.func, ast.Name)
                    and node.func.id
                    in {"ComboBox", "EditableComboBox", "LineEdit", "TextEdit"}
                    and node.args
                ):
                    failures.append(f"{path.name}:{node.lineno}:{node.func.id}")
        self.assertEqual(failures, [], "Fluent input constructors accept a parent, not text")

    def test_provider_actions_require_a_profile_and_lock_selection_while_running(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                item
                for item in window.pages
                if isinstance(item, ProviderPage) and item.provider == "chaoxing"
            )
            self.assertTrue(page.operation_buttons[0].isEnabled())
            self.assertFalse(page.login_button.isEnabled())
            self.assertFalse(page.operation_buttons[1].isEnabled())
            profile = page.controller.profiles.create("chaoxing", "账号")
            page.reload_profiles(profile.id)
            self.assertTrue(page.login_button.isEnabled())
            self.assertTrue(page.operation_buttons[1].isEnabled())
            page._set_operation_running(True)
            self.assertFalse(page.profile_combo.isEnabled())
            self.assertFalse(page.course_table.isEnabled())
            self.assertFalse(page.execution_combination.isEnabled())
            page.cancel_event = threading.Event()
            page.cancel_current()
            self.assertEqual(page.cancel_button.text(), "已请求取消")
            page._set_operation_running(False)
            window.close()

    def test_home_metrics_count_cached_inventory_and_question_bank(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            profile = window.controller.profiles.create("chaoxing", "账号")
            window.controller.inventory.save_courses(
                profile, [{"remote_id": "course-1", "title": "课程"}]
            )
            window.controller.inventory.save_tasks(
                profile, "course-1", [{"remote_id": "task-1"}, {"remote_id": "task-2"}]
            )
            window.controller.bank.upsert_question(
                "chaoxing", "question-hash", "single_choice", {"prompt": "题目"}
            )
            window.home_page.update_summary()
            self.assertEqual(window.home_page.metric_labels["profiles"].text(), "1")
            self.assertEqual(window.home_page.metric_labels["courses"].text(), "1")
            self.assertEqual(window.home_page.metric_labels["tasks"].text(), "2")
            self.assertEqual(window.home_page.metric_labels["bank"].text(), "1")
            window.close()

    def test_theme_switch_preserves_unsaved_advanced_editor_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            settings = window.settings_page
            edited = json.loads(settings.editor.toPlainText())
            edited["custom_unsaved"] = {"keep": True}
            settings.editor.setPlainText(json.dumps(edited, ensure_ascii=False))
            settings.theme.setCurrentIndex(settings.theme.findData(ThemeMode.DARK.value))
            settings.apply_selected_theme()
            remaining = json.loads(settings.editor.toPlainText())
            self.assertEqual(remaining["custom_unsaved"], {"keep": True})
            self.assertNotIn("ui", remaining)
            self.assertEqual(
                window.controller.config.load()["ui"]["theme"], ThemeMode.DARK.value
            )
            window.close()

    def test_untrusted_worker_text_is_redacted_before_display(self) -> None:
        source = (
            "Authorization: Bearer abc.def password=hunter2 "
            "https://example.test/?token=query-secret"
        )
        visible = ProviderPage._redact_text(source)
        for secret in ("abc.def", "hunter2", "query-secret"):
            self.assertNotIn(secret, visible)

    def test_ai_delete_actions_persist_after_config_reload(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(item for item in window.pages if isinstance(item, AISettingsPage))
            removed_combo = str(page.combo_choice.currentData())
            removed_site = str(page.site_choice.currentData())
            self.assertTrue(page.combo_action_buttons[-1].isEnabled())
            self.assertTrue(page.site_action_buttons[2].isEnabled())
            with patch("asterism.gui.ai_settings_v2._confirm", return_value=True):
                page.combo_action_buttons[-1].click()
                page.site_action_buttons[2].click()
                self.application.processEvents()
            loaded = window.controller.config.load()
            self.assertNotIn(removed_combo, loaded["models"]["combinations"])
            self.assertNotIn(removed_site, loaded["models"]["endpoints"])
            window.close()

    def test_ai_default_action_updates_every_provider_selector(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(item for item in window.pages if isinstance(item, AISettingsPage))
            value = window.controller.config.ensure()
            value["models"]["combinations"]["custom"] = {
                **value["models"]["combinations"]["economy"],
                "display_name": "自定义组合",
            }
            window.controller.config.save(value)
            page.reload(select_combo="custom")
            with patch("asterism.gui.ai_settings_v2._notice"):
                page.combo_action_buttons[2].click()
                self.application.processEvents()
            self.assertEqual(window.controller.config.load()["models"]["default"], "custom")
            for provider_page in (item for item in window.pages if isinstance(item, ProviderPage)):
                labels = [
                    provider_page.execution_combination.itemText(index)
                    for index in range(provider_page.execution_combination.count())
                ]
                self.assertIn("自定义组合（默认）", labels)
            window.close()

    def test_ai_missing_primary_does_not_silently_select_another_site(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.ai_page
            config = window.controller.config.ensure()
            models = config["models"]
            models["combinations"]["economy"]["timed"]["primary"] = "missing_site"
            window.controller.config.save(config)
            page.reload(select_combo="economy")
            self.assertIn(
                page.route_cards["timed"]["primary"].currentData(),
                (None, ""),
            )
            self.assertEqual(
                page.route_cards["timed"]["primary"].currentText(), "请选择站点"
            )
            window.close()

    def test_ai_editing_first_condition_preserves_additional_conditions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.ai_page
            value = window.controller.config.ensure()
            value["models"]["combinations"]["economy"]["conditions"] = [
                {
                    "kind": "matching",
                    "primary": "gpt_router",
                    "model": "gpt-5.6-terra",
                    "reasoning_effort": "medium",
                    "timeout_seconds": 30,
                    "retry_attempts": 1,
                },
                {
                    "kind": "fill_blank",
                    "primary": "gpt_router",
                    "model": "gpt-5.6-sol",
                    "reasoning_effort": "high",
                    "timeout_seconds": 60,
                    "retry_attempts": 2,
                },
            ]
            window.controller.config.save(value)
            page.reload(select_combo="economy")
            page.kind_timeout.setText("31")
            with patch("asterism.gui.ai_settings_v2._notice"):
                page.save_combo()
            conditions = window.controller.config.load()["models"]["combinations"]["economy"][
                "conditions"
            ]
            self.assertEqual(len(conditions), 2)
            self.assertEqual(conditions[0]["timeout_seconds"], 31.0)
            self.assertEqual(conditions[1]["kind"], "fill_blank")
            window.close()

    def test_ai_condition_rules_expose_fallback_and_can_be_added_or_removed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.ai_page
            page.reload(select_combo="economy")
            initial = len(page._condition_rules)
            page._add_condition()
            self.assertEqual(len(page._condition_rules), initial + 1)
            page.kind.setText("matching")
            page.kind_site.setCurrentIndex(page.kind_site.findData("gpt_router"))
            page.kind_fallback.setCurrentIndex(
                page.kind_fallback.findData("domestic_backup")
            )
            page._commit_condition_editor()
            current = page._condition_rules[-1]
            self.assertEqual(current["kind"], "matching")
            self.assertEqual(current["fallback"], "domestic_backup")
            page._delete_condition()
            self.assertEqual(len(page._condition_rules), initial)
            window.close()

    def test_ai_cancelled_combo_switch_keeps_unsaved_editor_values(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.ai_page
            page.reload(select_combo="economy")
            page.route_cards["timed"]["timeout"].setText("19")
            self.assertTrue(page._combo_dirty)
            with patch("asterism.gui.ai_settings_v2._confirm", return_value=False):
                page.combo_choice.setCurrentIndex(page.combo_choice.findData("gpt_only"))
            self.assertEqual(page.combo_choice.currentData(), "economy")
            self.assertEqual(page.route_cards["timed"]["timeout"].text(), "19")
            window.close()

    def test_notification_cannot_be_enabled_without_a_command(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            settings = window.settings_page
            settings.notifications_enabled.setChecked(True)
            settings.notification_command.clear()
            with patch("asterism.gui.settings_page.show_notice") as notice:
                settings.save()
            notice.assert_called_once()
            self.assertIn("通知命令", notice.call_args.args[2])
            self.assertFalse(
                window.controller.config.load()["notifications"]["enabled"]
            )
            window.close()

    def test_platform_settings_does_not_discard_unsaved_ai_combination(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.ai_page
            page.route_cards["timed"]["timeout"].setText("17")
            self.assertTrue(page._combo_dirty)
            settings = json.loads(window.settings_page.editor.toPlainText())
            settings["chaoxing"]["speed"] = 3.0
            window.settings_page.editor.setPlainText(json.dumps(settings))
            with patch("asterism.gui.settings_page.show_notice"):
                window.settings_page.save()
            self.assertEqual(
                window.controller.config.load()["providers"]["chaoxing"]["speed"], 3.0
            )
            self.assertEqual(page.route_cards["timed"]["timeout"].text(), "17")
            self.assertTrue(page._combo_dirty)
            window.close()

    def test_ai_changes_do_not_overwrite_unsaved_platform_parameters(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            settings = json.loads(window.settings_page.editor.toPlainText())
            settings["local_unsaved"] = {"keep": True}
            expected = json.dumps(settings, ensure_ascii=False)
            window.settings_page.editor.setPlainText(expected)
            window.ai_page._notify_combination_change()
            self.assertEqual(
                json.loads(window.settings_page.editor.toPlainText()), settings
            )
            window.close()

    def test_provider_progress_is_visible_on_home_activity_card(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                page
                for page in window.pages
                if isinstance(page, ProviderPage) and page.provider == "chaoxing"
            )
            page.current_courses = [{"title": "大学英语"}]
            page.course_table.setRowCount(1)
            page.course_table.selectRow(0)
            page.current_routine_tasks = [{"title": "第一章视频", "type": "video"}]
            page.task_table.setRowCount(1)
            page.task_table.selectRow(0)
            page.profile_combo.setCurrentIndex(0)
            page._event_received(
                "run",
                {"type": "progress", "current": 2, "total": 7, "message": "章节 2"},
            )
            self.assertIn("Chaoxing", window.home_page.activity.text())
            self.assertIn("2/7", window.home_page.activity.text())
            self.assertIn("章节 2", window.home_page.activity.text())
            self.assertIn("第一章视频", window.home_page.activity.text())
            window.close()

    def test_provider_and_draft_empty_states_follow_loaded_rows(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.provider_pages["chaoxing"]
            self.assertFalse(page.course_empty.isHidden())
            self.assertFalse(page.task_empty.isHidden())
            self.assertFalse(page.formal_empty.isHidden())
            self.assertFalse(page.question_empty.isHidden())

            page._success("courses", [{"title": "课程", "state": "active"}])
            self.assertTrue(page.course_empty.isHidden())
            page._success(
                "tasks",
                [
                    {"title": "视频", "type": "video", "state": "available"},
                    {
                        "title": "作业",
                        "type": "homework",
                        "state": "available",
                        "assessment_class": "formal",
                    },
                ],
            )
            self.assertTrue(page.task_empty.isHidden())
            self.assertTrue(page.formal_empty.isHidden())
            page._success("questions", [{"kind": "single_choice", "prompt": "题目"}])
            self.assertTrue(page.question_empty.isHidden())

            draft_page = window.draft_page
            draft_page.current_rows = []
            draft_page.table.setRowCount(0)
            draft_page.empty_state.setVisible(True)
            self.assertFalse(draft_page.empty_state.isHidden())
            window.close()

    def test_worker_error_code_is_translated_without_exposing_internal_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.provider_pages["chaoxing"]
            page._event_received(
                "authenticate",
                {
                    "type": "error",
                    "code": "authentication_failed",
                    "message": "密码或验证码无效",
                },
            )
            visible = page.log.toPlainText()
            self.assertIn("平台认证失败", visible)
            self.assertIn("密码或验证码无效", visible)
            self.assertNotIn("authentication_failed", visible)
            page._failure("authenticate", RunnerError("timeout", "worker exceeded 120 seconds"))
            visible = page.log.toPlainText()
            self.assertIn("操作超时", visible)
            self.assertNotIn("timeout", visible)
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

    def test_legacy_english_preference_does_not_create_a_partially_translated_ui(self) -> None:
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
            self.assertEqual(window.language_code, "zh-CN")
            if window.navigation is not None:
                labels = [
                    window.navigation.item(i).text() for i in range(window.navigation.count())
                ]
                self.assertIn("主页", labels)
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

    def test_window_detects_provider_draft_and_model_background_work(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            running = SimpleNamespace(isRunning=lambda: True)
            stopped = SimpleNamespace(isRunning=lambda: False)
            self.assertFalse(window.has_running_operations())
            window.provider_pages["chaoxing"].worker_thread = running
            self.assertTrue(window.has_running_operations())
            window.provider_pages["chaoxing"].worker_thread = stopped
            window.draft_page.worker_thread = running
            self.assertTrue(window.has_running_operations())
            window.draft_page.worker_thread = stopped
            window.ai_page._scan_thread = running
            self.assertTrue(window.has_running_operations())
            window.ai_page._scan_thread = stopped
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
            self.assertNotIn("draft-1", text)
            self.assertIn("已生成 1 份待确认草稿", text)
            self.assertIn('"failed":1', text)
            window.close()

    def test_only_chaoxing_exposes_batch_concurrency(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            pages = {page.provider: page for page in window.pages if isinstance(page, ProviderPage)}
            pages["chaoxing"]._ask_concurrency = lambda: 8
            self.assertEqual(pages["chaoxing"]._batch_concurrency(), 8)
            for provider in ("welearn", "uai", "cidaren"):
                pages[provider]._ask_concurrency = lambda provider=provider: self.fail(
                    f"{provider} must not ask for cross-task concurrency"
                )
                self.assertEqual(pages[provider]._batch_concurrency(), 1)
            window.close()

    def test_clearing_selection_cannot_run_a_stale_task_or_course(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                item
                for item in window.pages
                if isinstance(item, ProviderPage) and item.provider == "chaoxing"
            )
            page.current_courses = [{"title": "课程"}]
            page.course_table.setRowCount(1)
            page.course_table.selectRow(0)
            page.current_routine_tasks = [{"title": "任务"}]
            page.task_table.setRowCount(1)
            page.task_table.selectRow(0)
            self.assertIsNotNone(page._selected_course())
            self.assertIsNotNone(page._selected_task())

            page.course_table.clearSelection()
            page._clear_task_selection()

            self.assertIsNone(page._selected_course())
            self.assertIsNone(page._selected_task())
            window.close()

    def test_batch_selection_can_include_routine_and_formal_tasks(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.provider_pages["chaoxing"]
            page.current_routine_tasks = [{"title": "视频"}]
            page.current_formal_tasks = [{"title": "作业"}]
            page.task_table.setRowCount(1)
            page.formal_table.setRowCount(1)
            page.task_table.selectRow(0)
            page.formal_table.selectRow(0)
            self.assertEqual(len(page.task_table.selectionModel().selectedRows()), 1)
            self.assertEqual(len(page.formal_table.selectionModel().selectedRows()), 1)
            self.assertIsNone(page._selected_task())
            window.close()

    def test_cancelled_attempt_warning_stops_cidaren_question_read(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                item
                for item in window.pages
                if isinstance(item, ProviderPage) and item.provider == "cidaren"
            )
            profile = page.controller.profiles.create("cidaren", "本地账号")
            page.reload_profiles(profile.id)
            page.current_routine_tasks = [{"title": "限时任务"}]
            page.task_table.setRowCount(1)
            page.task_table.selectRow(0)

            with (
                patch("asterism.gui.provider_page.ask_confirmation", return_value=False),
                patch.object(page, "_call") as call,
            ):
                page.scan_questions()

            call.assert_not_called()
            window.close()

    def test_switching_provider_account_clears_previous_account_data(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = next(
                item
                for item in window.pages
                if isinstance(item, ProviderPage) and item.provider == "chaoxing"
            )
            first = page.controller.profiles.create("chaoxing", "账号一")
            second = page.controller.profiles.create("chaoxing", "账号二")
            page.reload_profiles(first.id)
            page.current_courses = [{"title": "账号一课程"}]
            page.course_table.setRowCount(1)

            page.profile_combo.setCurrentIndex(page.profile_combo.findData(second.id))

            self.assertEqual(page.current_courses, [])
            self.assertEqual(page.course_table.rowCount(), 0)
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
                for child in window.findChildren(QWidget)
                if child.windowTitle() == "Cidaren 授权"
            )
            self.assertIsInstance(dialog, FluentDialogBase)
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
                for child in window.findChildren(QWidget)
                if child.windowTitle() == "Chaoxing 任务详情"
            )
            self.assertIsInstance(dialog, FluentDialogBase)
            table = dialog.findChildren(TableWidget)[0]
            visible = " ".join(
                table.item(row, column).text()
                for row in range(table.rowCount())
                for column in range(table.columnCount())
                if table.item(row, column) is not None
            )
            self.assertNotIn("secret", visible)
            self.assertNotIn("remote_id", visible)
            dialog.close()
            window.close()

    def test_course_detail_presents_grade_components_without_internal_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            window = MainWindow(Path(temporary), Path(__file__).resolve().parents[1])
            page = window.provider_pages["chaoxing"]
            page.current_courses = [
                {
                    "remote_id": "course-secret-id",
                    "title": "大学英语",
                    "state": "active",
                    "provider_summary": {
                        "grade": {
                            "overall_score": 92.5,
                            "components": [
                                {
                                    "type": "reading",
                                    "weight_percent": 20,
                                    "required_minutes": 120,
                                    "observed_minutes": 87,
                                    "remaining_gap": 33,
                                }
                            ],
                        }
                    },
                }
            ]
            page.course_table.setRowCount(1)
            page.course_table.selectRow(0)
            page.show_course_detail()
            dialog = next(
                child
                for child in window.findChildren(QWidget)
                if child.windowTitle() == "Chaoxing 课程详情"
            )
            table = dialog.findChildren(TableWidget)[0]
            visible = " ".join(
                table.item(row, column).text()
                for row in range(table.rowCount())
                for column in range(table.columnCount())
                if table.item(row, column) is not None
            )
            self.assertIn("总成绩", visible)
            self.assertIn("阅读", visible)
            self.assertIn("剩余 33", visible)
            self.assertNotIn("course-secret-id", visible)
            self.assertNotIn("overall_score", visible)
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
                for child in window.findChildren(QWidget)
                if child.windowTitle() == "Chaoxing 扫描状态"
            )
            self.assertIsInstance(dialog, FluentDialogBase)
            self.assertIn("题目数", dialog.findChildren(TableWidget)[0].item(4, 0).text())
            self.assertNotIn(
                "task-3",
                " ".join(
                    item.text()
                    for item in dialog.findChildren(TableWidget)[0].findItems(
                        "*", Qt.MatchFlag.MatchWildcard
                    )
                ),
            )
            self.assertTrue(dialog.findChildren(PushButton))
            dialog.close()
            window.close()
