from __future__ import annotations

import importlib.util
import pathlib
import unittest


WORKER_PATH = pathlib.Path(__file__).resolve().parents[1] / "worker.py"
SPEC = importlib.util.spec_from_file_location("asterism_chaoxing_worker", WORKER_PATH)
assert SPEC and SPEC.loader
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


class WorkReadParamsTests(unittest.TestCase):
    def test_inventory_text_is_trimmed_control_free_and_utf8_bounded(self):
        cleaned = WORKER.clean_inventory_text("  第一行\n第二行\t" + "课" * 300, 32, "fallback")

        self.assertFalse(any(character.isspace() and character != " " for character in cleaned))
        self.assertLessEqual(len(cleaned.encode("utf-8")), 32)
        self.assertTrue(cleaned.startswith("第一行 第二行"))

    def test_inventory_text_uses_nonempty_fallback(self):
        self.assertEqual(WORKER.clean_inventory_text("\n\t", 512, "fallback"), "fallback")

    def test_reviewed_ordering_question_uses_existing_shared_kind(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="singleQuesId"><div class="Zy_TItle">【排序题】Arrange</div>'
            '<ul><li>first</li><li>second</li></ul></div>'
        )

        self.assertEqual(rows[0]["kind"], "ordering")

    def test_lazy_exam_question_uses_page_type_heading(self):
        rows = WORKER.parse_completed_work_result(
            '<h3 class="tepytitH3">单选题</h3>'
            '<div class="singleQuesId" data="q1"><div class="tit">Choose one</div>'
            '<div class="optionCon">A. alpha</div></div>'
        )

        self.assertEqual(rows[0]["kind"], "single_choice")
        self.assertEqual(rows[0]["native_shape"]["native_type"], "单选题")

    def test_course_homework_question_li_preserves_shared_options(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="questionLi" data="q20"><div class="tit">【共用选项题】Choose</div>'
            '<div class="answerBg"><p class="answer_p">A. alpha</p></div></div>'
        )

        self.assertEqual(rows[0]["remote_id"], "q20")
        self.assertEqual(rows[0]["kind"], "provider_native_shared_options")
        self.assertEqual(rows[0]["options"], ["A. alpha"])

    def test_shared_parent_question_ids_get_stable_child_identities(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="questionLi" data="parent-1"><div class="tit">【共用选项题】One</div></div>'
            '<div class="questionLi" data="parent-1"><div class="tit">【共用选项题】Two</div></div>'
        )

        self.assertEqual([row["remote_id"] for row in rows],
                         ["parent-1", "parent-1:child:2"])
        self.assertEqual(rows[1]["native_shape"]["provider_remote_id"], "parent-1")
        self.assertEqual(rows[1]["native_shape"]["provider_remote_id_occurrence"], 2)

    def test_asterism_tiku_refuses_missing_answers(self):
        source = WORKER.AsterismTiku({"q1": "A"})

        with self.assertRaises(WORKER.WorkerFailure):
            source.query_all([{"id": "q1"}, {"id": "q2"}])

    def test_asterism_tiku_returns_reviewed_answers_in_donor_order(self):
        source = WORKER.AsterismTiku({"q2": ["B", "C"], "q1": "A"})

        self.assertEqual(source.query_all([{"id": "q1"}, {"id": "q2"}]), ["A", ["B", "C"]])

    def test_recovered_completed_card_keeps_request_jobid_empty(self):
        params = WORKER.work_read_params(
            {"courseId": "course", "clazzId": "class", "cpi": "course-cpi"},
            {"point": {"id": "point"}},
            {
                "jobid": "work-abc",
                "_asterism_request_jobid": "",
                "enc": "enc",
            },
            {"ktoken": "ktoken", "utenc": "utenc"},
        )

        self.assertEqual(params["workId"], "abc")
        self.assertEqual(params["jobid"], "")
        self.assertEqual(params["originJobId"], "work-abc")
        self.assertEqual(params["knowledgeid"], "point")

    def test_native_donor_job_uses_existing_jobid_for_both_fields(self):
        params = WORKER.work_read_params(
            {"courseId": "course", "clazzId": "class", "cpi": "course-cpi"},
            {"point": {"id": "point"}},
            {"jobid": "work-abc", "enc": "enc"},
            {"knowledgeid": "native-point", "cpi": "native-cpi"},
        )

        self.assertEqual(params["jobid"], "work-abc")
        self.assertEqual(params["originJobId"], "work-abc")
        self.assertEqual(params["knowledgeid"], "native-point")
        self.assertEqual(params["cpi"], "native-cpi")


if __name__ == "__main__":
    unittest.main()
