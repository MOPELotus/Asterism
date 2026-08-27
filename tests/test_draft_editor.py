from __future__ import annotations

import os
import unittest

try:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtWidgets import QApplication

    from asterism.gui.draft_editor import FormalDraftEditor
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


if __name__ == "__main__":
    unittest.main()
