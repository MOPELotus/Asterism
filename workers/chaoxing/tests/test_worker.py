from __future__ import annotations

import importlib.util
import pathlib
import types
import unittest
from unittest import mock


WORKER_PATH = pathlib.Path(__file__).resolve().parents[1] / "worker.py"
SPEC = importlib.util.spec_from_file_location("asterism_chaoxing_worker", WORKER_PATH)
assert SPEC and SPEC.loader
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


class WorkReadParamsTests(unittest.TestCase):
    def test_verification_policy_is_bounded_and_redacts_material(self):
        class Session:
            def reg_captcha_after(self, callback): self.captcha_after = callback
            def reg_captcha_before(self, callback): self.captcha_before = callback
            def reg_face_after(self, callback): self.face_after = callback
            def reg_face_before(self, callback): self.face_before = callback

        messages = []
        events = types.SimpleNamespace(
            emit=lambda kind, **payload: messages.append((kind, payload))
        )
        session = Session()
        WORKER._configure_verification_policy(
            session,
            {"settings": {
                "verification_attempt_budget": 4,
                "verification_time_budget_seconds": 30,
                "verification_source": "assessment",
            }},
            events,
        )

        self.assertEqual(getattr(session, "_SessionWraper__captcha_max_retry"), 4)
        session.captcha_after(1)
        session.captcha_before(True, "SECRET-CODE")
        session.face_after("https://secret.example/face?token=SECRET")
        session.face_before("SECRET-OBJECT", pathlib.Path("SECRET-FACE.jpg"))
        serialized = repr(messages)
        self.assertIn("source=assessment", serialized)
        self.assertIn("image_captcha succeeded", serialized)
        self.assertIn("face succeeded", serialized)
        self.assertNotIn("SECRET", serialized)

    def test_verification_policy_rejects_unbounded_values(self):
        session = types.SimpleNamespace()
        events = types.SimpleNamespace(emit=lambda *_args, **_kwargs: None)
        with self.assertRaises(WORKER.WorkerFailure):
            WORKER._configure_verification_policy(
                session,
                {"settings": {"verification_attempt_budget": 0}},
                events,
            )

    def test_challenge_marker_accepts_donor_variants(self):
        self.assertTrue(WORKER._is_challenge_point({"need_unlock": True, "challengeMode": True}))
        self.assertTrue(WORKER._is_challenge_point({"mode": "闯关"}))
        self.assertFalse(WORKER._is_challenge_point({"mode": "普通"}))

    def test_inventory_keeps_one_selectable_task_per_knowledge_point(self):
        class Cookies:
            def get_dict(self):
                return {"sid": "redacted"}

        class Session:
            cookies = Cookies()

            def get(self, *_args, **_kwargs):
                raise RuntimeError("homework list unavailable in fixture")

        session = Session()
        module = types.SimpleNamespace(
            SessionManager=types.SimpleNamespace(get_session=lambda: session),
            _asterism_card_diagnostics={},
        )
        bot = types.SimpleNamespace(
            get_course_point=lambda *_args: {
                "points": [{
                    "id": "point-1", "title": "1.1 Knowledge point",
                    "has_finished": False, "need_unlock": False,
                }]
            },
            get_job_list=lambda *_args: ([
                {"type": "video", "jobid": "video-1"},
                {"type": "workid", "jobid": "work-1"},
            ], {"knowledgeid": "point-1"}),
        )
        payload = {
            "session": {"cookies": {"sid": "redacted"}},
            "course": {"courseId": "course-1", "clazzId": "class-1", "cpi": "cpi-1"},
        }

        with mock.patch.object(WORKER, "bot_for", return_value=bot), \
             mock.patch.object(WORKER, "include_completed_cards"), \
             mock.patch.object(WORKER, "cxkitty_for", return_value=None):
            result = WORKER.inventory(
                module,
                payload,
                types.SimpleNamespace(emit=lambda *_args, **_kwargs: None),
                WORKER.Redactor(),
            )

        self.assertEqual(len(result["tasks"]), 1)
        task = result["tasks"][0]
        self.assertEqual(task["remote_id"], "knowledge:point-1")
        self.assertEqual(task["title"], "1.1 Knowledge point")
        self.assertEqual(task["source_type"], "chapter")
        self.assertEqual(task["capabilities"], ["questions", "run"])
        self.assertEqual(task["native"]["route_kind"], "knowledge_point")
        self.assertEqual(len(task["native"]["jobs"]), 2)

    def test_inventory_text_is_trimmed_control_free_and_utf8_bounded(self):
        cleaned = WORKER.clean_inventory_text("  第一行\n第二行\t" + "课" * 300, 32, "fallback")

        self.assertFalse(any(character.isspace() and character != " " for character in cleaned))
        self.assertLessEqual(len(cleaned.encode("utf-8")), 32)
        self.assertTrue(cleaned.startswith("第一行 第二行"))

    def test_inventory_text_uses_nonempty_fallback(self):
        self.assertEqual(WORKER.clean_inventory_text("\n\t", 512, "fallback"), "fallback")

    def test_homework_answer_record_is_learner_side_complete(self):
        self.assertEqual(WORKER.homework_inventory_state("主观题", "answer-42"), "completed")
        self.assertEqual(WORKER.homework_inventory_state("主观题", "0"), "pending")

    def test_reviewed_ordering_question_uses_existing_shared_kind(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="singleQuesId"><div class="Zy_TItle">【排序题】Arrange</div>'
            '<ul><li>first</li><li>second</li></ul></div>'
        )

        self.assertEqual(rows[0]["kind"], "ordering")

    def test_reviewed_result_prefers_official_answer_and_keeps_negative_evidence(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="singleQuesId" data="q1"><div class="Zy_TItle">【单选题】Choose</div>'
            '<div class="mark_answer">我的答案：A 正确答案：B</div></div>'
        )

        evidence = rows[0]["answer_evidence"]
        self.assertEqual(evidence["value"], "B")
        self.assertEqual(evidence["submitted_value"], "A")
        self.assertEqual(evidence["official_value"], "B")
        self.assertFalse(evidence["submitted_correct"])

    def test_lazy_exam_question_uses_page_type_heading(self):
        rows = WORKER.parse_completed_work_result(
            '<h3 class="tepytitH3">单选题</h3>'
            '<div class="singleQuesId" data="q1"><div class="tit">Choose one</div>'
            '<div class="optionCon">A. alpha</div></div>'
        )

        self.assertEqual(rows[0]["kind"], "single_choice")
        self.assertEqual(rows[0]["native_shape"]["native_type"], "单选题")

    def test_reviewed_question_infers_parenthesized_inline_type(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="questionLi"><div class="tit">4. (判断题) 示例</div>'
            '<ul><li>A. 对</li><li>B. 错</li></ul></div>'
        )

        self.assertEqual(rows[0]["kind"], "true_false")
        self.assertEqual(rows[0]["native_shape"]["native_type"], "判断题")

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

    def test_rich_stem_preserves_underlined_target_and_visual_blank(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="singleQuesId" data="q1"><div class="Zy_TItle">'
            'Replace <span style="text-decoration: underline">futuristic</span> and fill '
            '<u>&nbsp;&nbsp;&nbsp;</u> or ____.</div></div>'
        )

        self.assertIn('[UNDERLINE]futuristic[/UNDERLINE]', rows[0]["prompt"])
        self.assertEqual(rows[0]["prompt"].count("[BLANK_"), 2)
        self.assertEqual(rows[0]["native_shape"]["underline_count"], 1)
        self.assertEqual(rows[0]["native_shape"]["blank_count"], 2)

    def test_rich_stem_preserves_input_blanks_in_order(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="singleQuesId" data="q1"><div class="Zy_TItle">A '
            '<input name="answer1"/> B <textarea></textarea></div></div>'
        )

        self.assertIn("A [BLANK_1] B [BLANK_2]", rows[0]["prompt"])

    def test_page_level_word_bank_is_preserved_as_shared_context(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="wordBank"><span class="word">alpha</span>'
            '<span class="word">beta</span></div>'
            '<div class="singleQuesId" data="q1"><div class="Zy_TItle">'
            'Fill <u>&nbsp;</u></div></div>'
        )

        self.assertEqual(rows[0]["native_shape"]["shared_options"], ["alpha", "beta"])

    def test_rich_content_keeps_media_formula_and_file_order(self):
        rows = WORKER.parse_completed_work_result(
            '<div class="singleQuesId" data="q1"><div class="Zy_TItle">Before'
            '<audio><source src="//media.example/a.mp3?token=secret" type="audio/mpeg"></audio>'
            '<span class="katex">x + y</span>'
            '<a href="https://files.example/task.pdf?sign=secret">worksheet</a>'
            'After</div></div>'
        )

        prompt = rows[0]["prompt"]
        self.assertLess(prompt.index("Before"), prompt.index("[QUESTION_AUDIO:"))
        self.assertLess(prompt.index("[QUESTION_AUDIO:"), prompt.index("[QUESTION_FORMULA:x + y]"))
        self.assertLess(prompt.index("[QUESTION_FORMULA:x + y]"), prompt.index("[QUESTION_FILE:"))
        self.assertNotIn("secret", prompt)

    def test_grade_composition_keeps_explicit_scoring_duration_and_discussion_facts(self):
        summary = WORKER.parse_course_grade_summary(
            '<div>综合成绩：92.5分</div><table>'
            '<tr><td>视频</td><td>权重 30%</td><td>完成率 100%</td></tr>'
            '<tr><td>阅读</td><td>要求阅读 120 分钟</td><td>已读 87 分钟</td></tr>'
            '<tr><td>直播</td><td>观看时长 45 分钟</td></tr>'
            '<tr><td>讨论</td><td>占比 10%</td><td>得分 8分</td></tr>'
            '</table>',
            'https://mooc1.chaoxing.com/mooc-ans/statistic/student?token=secret',
        )

        self.assertEqual(summary["overall_score"], 92.5)
        by_type = {component["type"]: component for component in summary["components"]}
        self.assertEqual(by_type["video"]["weight_percent"], 30.0)
        self.assertEqual(by_type["reading"]["required_minutes"], 120.0)
        self.assertEqual(by_type["live"]["observed_minutes"], 45.0)
        self.assertEqual(by_type["discussion"]["score"], 8.0)
        self.assertEqual(summary["source_path"], "/mooc-ans/statistic/student")

    def test_grade_composition_keeps_completion_condition_and_remaining_gap(self):
        summary = WORKER.parse_course_grade_summary(
            '<table><tr><td>阅读</td><td>完成条件：阅读满 120 分钟</td>'
            '<td>要求阅读 120 分钟</td><td>已读 87 分钟</td></tr></table>'
        )
        component = summary["components"][0]
        self.assertEqual(component["completion_condition"], "阅读满 120 分钟")
        self.assertEqual(component["remaining_gap"], 33.0)

    def test_native_matching_routes_to_browser_without_affecting_common_choices(self):
        html = (
            '<div class="singleQuesId" data="q1"><div class="TiMu" data="11"></div></div>'
            '<div class="singleQuesId" data="q2"><div class="TiMu" data="0"></div></div>'
        )

        self.assertTrue(WORKER.chapter_work_requires_browser(html, {"q1": {"1": "C"}}))
        self.assertFalse(WORKER.chapter_work_requires_browser(html, {"q2": "A"}))

    def test_asterism_tiku_preserves_missing_answers_as_empty_slots(self):
        source = WORKER.AsterismTiku({"q1": "A"}, cover_rate=0.75)

        self.assertEqual(source.COVER_RATE, 0.75)
        self.assertEqual(source.query_all([{"id": "q1"}, {"id": "q2"}]), ["A", None])

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

    def test_formal_exam_save_mode_never_calls_final_submit(self):
        question = types.SimpleNamespace(
            id="q1",
            type=types.SimpleNamespace(name="单选题"),
            options={"A": "alpha", "B": "beta"},
            answer=None,
        )
        exam = types.SimpleNamespace(
            need_code=False,
            session=types.SimpleNamespace(ck_dump=lambda: {"sid": "opaque"}),
            get_meta=mock.Mock(),
            start=mock.Mock(),
            fetch_all=mock.Mock(return_value=[question]),
            submit=mock.Mock(),
            final_submit=mock.Mock(),
        )
        current = types.SimpleNamespace(status=types.SimpleNamespace(value="未完成"))
        payload = {
            "answers": [{"remote_id": "q1", "value": "A"}],
            "settings": {"assessment_mode": "save"},
        }

        with mock.patch.object(WORKER, "cxkitty_exam", return_value=(None, None, exam, current)):
            result = WORKER.run_course_exam(
                payload, {}, types.SimpleNamespace(emit=lambda *_args, **_kwargs: None), WORKER.Redactor()
            )

        exam.submit.assert_called_once()
        exam.final_submit.assert_not_called()
        self.assertTrue(result["result"]["answers_saved"])
        self.assertFalse(result["result"]["final_submit"])
        self.assertEqual(result["remote_state"], "in_progress")


if __name__ == "__main__":
    unittest.main()
