from __future__ import annotations

import os
import unittest

try:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtWidgets import QApplication

    from asterism.gui.draft_editor import AnswerValueEditor, FormalDraftEditor
except ImportError:  # pragma: no cover
    QApplication = None


@unittest.skipUnless(QApplication is not None, "desktop dependencies are not installed")
class DraftEditorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.application = QApplication.instance() or QApplication(["draft-editor-test"])

    def test_editor_recomputes_answer_rows_and_unresolved_questions(self) -> None:
        editor = FormalDraftEditor(
            {
                "task": {"remote_id": "formal-1"},
                "questions": [
                    {"remote_id": "q1", "kind": "single_choice", "prompt": "one"},
                    {"remote_id": "q2", "kind": "short_answer", "prompt": "two"},
                ],
                "answers": [{"remote_id": "q1", "value": "A"}],
                "unresolved_question_ids": ["stale"],
            }
        )
        editor.answers["q2"] = "plain text"
        value = editor.current_payload()
        self.assertEqual(
            value["answers"],
            [
                {"remote_id": "q1", "value": "A"},
                {"remote_id": "q2", "value": "plain text"},
            ],
        )
        self.assertEqual(value["unresolved_question_ids"], [])
        self.assertEqual(editor.answers["q2"], "plain text")
        editor.answers.pop("q1")
        self.assertEqual(editor.current_payload()["unresolved_question_ids"], ["q1"])
        editor.close()

    def test_subjective_plain_text_is_never_coerced_to_json_scalar(self) -> None:
        self.assertEqual(FormalDraftEditor._parse_answer("123", "short_answer"), "123")
        self.assertEqual(FormalDraftEditor._parse_answer("true", "discussion"), "true")
        self.assertEqual(
            FormalDraftEditor._parse_answer('["A", "B"]', "multiple_choice"), ["A", "B"]
        )

    def test_choice_editor_rebinds_visible_option_content_to_current_key(self) -> None:
        editor = AnswerValueEditor(
            {
                "kind": "single_choice",
                "prompt": "请选择",
                "options": ["苹果", "香蕉"],
            },
            "香蕉",
        )
        self.assertEqual(editor.value(), "B")
        editor.close()

    def test_multiple_and_true_false_editors_return_worker_values(self) -> None:
        multiple = AnswerValueEditor(
            {
                "kind": "multiple_choice",
                "prompt": "多选",
                "options": [
                    {"key": "A", "text": "甲"},
                    {"key": "B", "text": "乙"},
                ],
            },
            ["A", "B"],
        )
        self.assertEqual(multiple.value(), ["A", "B"])
        judgement = AnswerValueEditor(
            {"kind": "true_false", "prompt": "判断"},
            True,
        )
        self.assertEqual(judgement.value(), "true")
        multiple.close()
        judgement.close()

    def test_fill_matching_and_ordering_editors_keep_native_shapes(self) -> None:
        blanks = AnswerValueEditor(
            {
                "kind": "fill_blank",
                "prompt": "两个空",
                "native_shape": {"blank_count": 2},
            },
            ["one", "two"],
        )
        self.assertEqual(blanks.value(), ["one", "two"])
        matching = AnswerValueEditor(
            {
                "kind": "matching",
                "prompt": "配对",
                "native": {
                    "matching_groups": {
                        "left": ["左一", "左二"],
                        "right": ["右一", "右二"],
                    }
                },
            },
            {"左一": "右二", "左二": "右一"},
        )
        self.assertEqual(matching.value(), {"左一": "B", "左二": "A"})
        ordering = AnswerValueEditor(
            {"kind": "ordering", "prompt": "排序", "options": ["甲", "乙"]},
            ["乙", "甲"],
        )
        self.assertEqual(ordering.value(), ["B", "A"])
        ordering.ordering_list.setCurrentRow(1)
        ordering._move_ordering_item(-1)
        self.assertEqual(ordering.value(), ["A", "B"])
        blanks.close()
        matching.close()
        ordering.close()


if __name__ == "__main__":
    unittest.main()
