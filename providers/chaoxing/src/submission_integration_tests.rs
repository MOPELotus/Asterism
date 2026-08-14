use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use asterism_domain::{
    AnswerCandidateId, AnswerSource, NormalizedAnswer, ProviderAccountId, ProviderId, QuestionKind,
    QuestionOption, QuestionSnapshotId, SecretId, SelectedAnswer, SubmissionDraft,
    SubmissionDraftId, SubmissionDraftItem, SubmissionQuestionVerificationStatus,
    SubmissionReceipt, SubmissionVerificationStatus, TaskId,
};
use asterism_provider_api::{
    CourseInventoryCapability, ExecutionEventSink, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, ProviderRouteContext, ProviderSubmissionStepOutcome, RemoteCourse,
    ResolvedProviderQuestionSessionContinuation, SubmissionBuildCapability,
    SubmissionExecuteCapability, SubmissionVerifyCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use zeroize::Zeroizing;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingChapterWorkTarget,
    ChaoxingCourseRoute, ChaoxingExamDetailRequest, ChaoxingExamQuestionArtifact,
    ChaoxingExamQuestionRequest, ChaoxingExamStartCommand, ChaoxingExamSubmissionCommand,
    ChaoxingExamSubmissionResponse, ChaoxingExamVerificationDocument, ChaoxingInventoryDocument,
    ChaoxingInventoryTransport, ChaoxingSubmissionBuild, ChaoxingSubmissionExecute,
    ChaoxingSubmissionPlan, ChaoxingSubmissionTransport, ChaoxingSubmissionVerificationTransport,
    ChaoxingSubmissionVerify, ChaoxingWorkDetailRequest, ChaoxingWorkDetailState,
    ChaoxingWorkVerificationDocument, ChaoxingWorkVerificationRoute,
    exam_attempt::{
        CHAOXING_EXAM_QUESTION_ARTIFACT_TYPE, CHAOXING_EXAM_QUESTIONS_READY_PHASE,
        ChaoxingExamAttemptMaterial, parse_exam_cover,
    },
    metadata::development_metadata,
    parse_chapter_work_question_page, parse_exam_question_page, parse_exam_submission_response,
    parse_work_preview_question_page,
    runtime_settings::runtime_settings_schema,
};

const WORK_LIST: &str = include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.html");
const WORK_QUESTIONS: &str =
    include_str!("../../../fixtures/providers/chaoxing/questions/work-preview-mixed.html");
const WORK_VIEW: &str =
    include_str!("../../../fixtures/providers/chaoxing/work/submission-view.html");
const CHAPTER_LIST: &str =
    include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
const CHAPTER_CARDS: &str =
    include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");
const CHAPTER_QUESTIONS: &str =
    include_str!("../../../fixtures/providers/chaoxing/questions/work-mobile-mixed.html");
const EXAM_LIST: &str = include_str!("../../../fixtures/providers/chaoxing/exam/list-mixed.html");
const EXAM_COVER: &str = include_str!("../../../fixtures/providers/chaoxing/exam/cover-ready.html");
const EXAM_QUESTIONS: &str =
    include_str!("../../../fixtures/providers/chaoxing/questions/exam-mobile-mixed.html");
const EXAM_SAVE_1: &str =
    include_str!("../../../fixtures/providers/chaoxing/exam/submit-save-1.json");
const EXAM_SAVE_2: &str =
    include_str!("../../../fixtures/providers/chaoxing/exam/submit-save-2.json");
const EXAM_FINAL: &str =
    include_str!("../../../fixtures/providers/chaoxing/exam/submit-final.json");
const EXAM_RESULT: &str =
    include_str!("../../../fixtures/providers/chaoxing/exam/detail-result.html");

#[derive(Debug)]
struct FixtureCourses {
    metadata: ProviderMetadata,
}

impl FixtureCourses {
    fn new() -> Self {
        Self {
            metadata: development_metadata().unwrap(),
        }
    }
}

impl ProviderIdentity for FixtureCourses {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl CourseInventoryCapability for FixtureCourses {
    async fn list_courses(&self, _context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>> {
        Ok(vec![RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "Fixture course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])?,
        }])
    }
}

#[derive(Debug, Default)]
struct FixturePlatform {
    inventories: AtomicUsize,
    submissions: AtomicUsize,
    verifications: AtomicUsize,
    exam_steps: AtomicUsize,
    exam_completed: AtomicBool,
}

#[async_trait]
impl ChaoxingInventoryTransport for FixturePlatform {
    async fn fetch_chapter_inventory(
        &self,
        _context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        ChaoxingInventoryDocument::try_new(CHAPTER_LIST)
    }

    async fn fetch_work_inventory(
        &self,
        _context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        self.inventories.fetch_add(1, Ordering::Relaxed);
        ChaoxingInventoryDocument::try_new(WORK_LIST)
    }

    async fn fetch_chapter_resource_inventories(
        &self,
        _context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingChapterResourceRequest],
    ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
        let request = requests.first().ok_or_else(unexpected_call)?;
        if requests.len() != 1 || request.knowledge_id() != "4001" {
            return Err(unexpected_call());
        }
        let completed = self.submissions.load(Ordering::Relaxed) > 0;
        (0..crate::task_inventory::CHAPTER_RESOURCE_CARD_COUNT)
            .map(|card_index| {
                let document = if card_index == 0 {
                    if completed {
                        CHAPTER_CARDS.replace(
                            "\"jobid\":\"job-work\",\"isPassed\":false",
                            "\"jobid\":\"job-work\",\"isPassed\":true",
                        )
                    } else {
                        CHAPTER_CARDS.to_owned()
                    }
                } else {
                    "<script>mArg={\"defaults\":{},\"attachments\":[]};</script>".to_owned()
                };
                ChaoxingChapterResourceDocument::for_request(request, card_index, document)
            })
            .collect()
    }

    async fn fetch_exam_inventory(
        &self,
        _context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let document = if self.exam_completed.load(Ordering::Relaxed) {
            EXAM_LIST
                .replacen("待做", "已完成", 1)
                .replacen(
                    "onclick=\"goTest('100','exam-1',0,'SAFE_TIME','paper-1',false,'SAFE_ENC')\"",
                    "data-exam-id=\"exam-1\" data=\"/exam-ans/mooc2/exam/preview?courseId=100&amp;classId=200&amp;examId=exam-1\"",
                    1,
                )
        } else {
            EXAM_LIST.to_owned()
        };
        ChaoxingInventoryDocument::try_new(document)
    }

    async fn fetch_work_detail_states(
        &self,
        _context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
        _requests: &[ChaoxingWorkDetailRequest<'_>],
    ) -> ProviderResult<Vec<ChaoxingWorkDetailState>> {
        Err(unexpected_call())
    }
}

#[async_trait]
impl ChaoxingSubmissionTransport for FixturePlatform {
    async fn submit_work(
        &self,
        _context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt> {
        self.submissions.fetch_add(1, Ordering::Relaxed);
        assert_eq!(request.remote_task_id(), "work:100:200:work-1");
        assert_eq!(
            plan.answers().collect::<Vec<_>>(),
            vec![
                ("work-preview-q-1", "0", "B"),
                ("work-preview-q-2", "1", "AC"),
            ]
        );
        Ok(receipt())
    }

    async fn submit_chapter_work(
        &self,
        _context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt> {
        self.submissions.fetch_add(1, Ordering::Relaxed);
        assert_eq!(request.knowledge_id(), "4001");
        assert_eq!(target.job_id(), "job-work");
        assert_eq!(
            plan.answers().collect::<Vec<_>>(),
            vec![
                ("work-q-1", "0", "B"),
                ("work-q-2", "1", "AC"),
                ("work-q-3", "2", "bounded answer"),
                ("work-q-4", "3", "true"),
            ]
        );
        Ok(receipt())
    }

    async fn prepare_exam_submission(
        &self,
        _context: &ProviderContext,
        artifact: ChaoxingExamQuestionArtifact,
        draft: &SubmissionDraft,
    ) -> ProviderResult<ChaoxingExamSubmissionCommand> {
        ChaoxingExamSubmissionCommand::try_new(artifact, draft, "9001", [11; 32], Utc::now())
    }

    async fn submit_exam(
        &self,
        _context: &ProviderContext,
        command: &ChaoxingExamSubmissionCommand,
    ) -> ProviderResult<ChaoxingExamSubmissionResponse> {
        let step = self.exam_steps.fetch_add(1, Ordering::Relaxed);
        let document = match (step, command.is_final()) {
            (0, false) => EXAM_SAVE_1,
            (1, false) => EXAM_SAVE_2,
            (2, true) => {
                self.exam_completed.store(true, Ordering::Relaxed);
                EXAM_FINAL
            }
            _ => return Err(unexpected_call()),
        };
        parse_exam_submission_response(document, command.is_final(), Utc::now())
    }
}

#[async_trait]
impl ChaoxingSubmissionVerificationTransport for FixturePlatform {
    async fn fetch_work_verification(
        &self,
        _context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingWorkVerificationDocument> {
        self.verifications.fetch_add(1, Ordering::Relaxed);
        assert_eq!(request.remote_task_id(), "work:100:200:work-1");
        ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::View,
            WORK_VIEW.to_owned(),
        )
    }

    async fn fetch_exam_verification(
        &self,
        _context: &ProviderContext,
        request: ChaoxingExamDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingExamVerificationDocument> {
        self.verifications.fetch_add(1, Ordering::Relaxed);
        assert_eq!(request.remote_task_id(), "exam:100:200:exam-1");
        assert_eq!(request.exam_id(), "exam-1");
        ChaoxingExamVerificationDocument::try_new(EXAM_RESULT.to_owned())
    }
}

#[derive(Debug)]
struct NoopEvents;

#[async_trait]
impl ExecutionEventSink for NoopEvents {
    async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
        Ok(())
    }

    async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn execute_and_verify_use_independent_native_slots() {
    let courses = Arc::new(FixtureCourses::new());
    let platform = Arc::new(FixturePlatform::default());
    let execute =
        ChaoxingSubmissionExecute::try_new(courses.clone(), platform.clone(), platform.clone())
            .unwrap();
    let verify =
        ChaoxingSubmissionVerify::try_new(courses, platform.clone(), platform.clone()).unwrap();
    let draft = draft().await;

    let acknowledgement = execute
        .execute_submission(
            &context(),
            "work:100:200:work-1",
            &draft,
            &runtime_settings_schema().resolve(None, None, None).unwrap(),
            &NoopEvents,
        )
        .await
        .unwrap();
    assert_eq!(acknowledgement.remote_status, "accepted");
    assert_eq!(platform.submissions.load(Ordering::Relaxed), 1);
    assert_eq!(platform.verifications.load(Ordering::Relaxed), 0);

    let snapshot = verify
        .verify_submission(
            &context(),
            "work:100:200:work-1",
            &draft,
            Some(&acknowledgement),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
    assert!(
        snapshot
            .questions
            .iter()
            .all(|question| { question.status == SubmissionQuestionVerificationStatus::Confirmed })
    );
    assert_eq!(platform.submissions.load(Ordering::Relaxed), 1);
    assert_eq!(platform.verifications.load(Ordering::Relaxed), 1);

    let recovered = verify
        .verify_submission(&context(), "work:100:200:work-1", &draft, None)
        .await
        .unwrap();
    assert_eq!(recovered.status, SubmissionVerificationStatus::Confirmed);
    assert_eq!(platform.submissions.load(Ordering::Relaxed), 1);
    assert_eq!(platform.verifications.load(Ordering::Relaxed), 2);
    assert_eq!(platform.inventories.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn chapter_work_execute_refetches_target_and_verifies_fresh_card_state() {
    let courses = Arc::new(FixtureCourses::new());
    let platform = Arc::new(FixturePlatform::default());
    let execute =
        ChaoxingSubmissionExecute::try_new(courses.clone(), platform.clone(), platform.clone())
            .unwrap();
    let verify =
        ChaoxingSubmissionVerify::try_new(courses, platform.clone(), platform.clone()).unwrap();
    let draft = chapter_draft().await;
    let remote_task_id = "resource:100:200:4001:job-work";

    let acknowledgement = execute
        .execute_submission(
            &context(),
            remote_task_id,
            &draft,
            &runtime_settings_schema().resolve(None, None, None).unwrap(),
            &NoopEvents,
        )
        .await
        .unwrap();
    assert_eq!(acknowledgement.remote_status, "accepted");
    assert_eq!(platform.submissions.load(Ordering::Relaxed), 1);
    assert_eq!(platform.verifications.load(Ordering::Relaxed), 0);

    let snapshot = verify
        .verify_submission(&context(), remote_task_id, &draft, Some(&acknowledgement))
        .await
        .unwrap();
    assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
    assert_eq!(
        snapshot.remote_state,
        Some(asterism_domain::RemoteState::Completed)
    );
    assert_eq!(snapshot.progress_percent, Some(100));
    assert!(
        snapshot.questions.iter().all(|question| {
            question.status == SubmissionQuestionVerificationStatus::Unverified
        })
    );
    assert_eq!(platform.submissions.load(Ordering::Relaxed), 1);
    assert_eq!(platform.verifications.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn stale_preview_fails_before_the_mutation_slot() {
    let platform = Arc::new(FixturePlatform::default());
    let execute = ChaoxingSubmissionExecute::try_new(
        Arc::new(FixtureCourses::new()),
        platform.clone(),
        platform.clone(),
    )
    .unwrap();
    let mut draft = draft().await;
    draft.payload_preview.format = "chaoxing.changed.v1".to_owned();
    let error = execute
        .execute_submission(
            &context(),
            "work:100:200:work-1",
            &draft,
            &runtime_settings_schema().resolve(None, None, None).unwrap(),
            &NoopEvents,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert_eq!(platform.submissions.load(Ordering::Relaxed), 0);
    assert_eq!(platform.inventories.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn exam_session_rotates_saves_then_verifies_the_terminal_inventory() {
    let context = context();
    let courses = Arc::new(FixtureCourses::new());
    let platform = Arc::new(FixturePlatform::default());
    let execute =
        ChaoxingSubmissionExecute::try_new(courses.clone(), platform.clone(), platform.clone())
            .unwrap();
    let verify =
        ChaoxingSubmissionVerify::try_new(courses, platform.clone(), platform.clone()).unwrap();
    let (draft, artifact) = exam_draft_and_artifact().await;
    let encoded = artifact.encode().unwrap();
    let first_digest = encoded.digest();
    let first_value = encoded.into_secret_value();
    let (_, first) =
        execute_exam_step(&execute, &context, &draft, first_value, first_digest, 1).await;
    let ProviderSubmissionStepOutcome::Continue {
        continuation,
        response_digest,
        ..
    } = first
    else {
        panic!("first Exam save did not rotate its continuation");
    };
    assert_ne!(response_digest, [0; 32]);
    let (_, second_digest, _, second_value, _) = continuation.into_parts();
    let (_, second) =
        execute_exam_step(&execute, &context, &draft, second_value, second_digest, 2).await;
    let ProviderSubmissionStepOutcome::Continue { continuation, .. } = second else {
        panic!("second Exam save did not rotate its continuation");
    };
    let (_, final_digest, _, final_value, _) = continuation.into_parts();
    let (final_value, terminal) =
        execute_exam_step(&execute, &context, &draft, final_value, final_digest, 3).await;
    let ProviderSubmissionStepOutcome::Submitted { receipt, .. } = terminal else {
        panic!("Exam final submit did not return a terminal receipt");
    };
    assert_eq!(platform.exam_steps.load(Ordering::Relaxed), 3);
    assert!(platform.exam_completed.load(Ordering::Relaxed));

    let snapshot = verify
        .verify_submission_with_session(
            &context,
            "exam:100:200:exam-1",
            &draft,
            Some(&receipt),
            ResolvedProviderQuestionSessionContinuation {
                continuation_type: CHAOXING_EXAM_QUESTION_ARTIFACT_TYPE,
                continuation_digest: final_digest,
                phase: CHAOXING_EXAM_QUESTIONS_READY_PHASE,
                revision: 3,
                value: &final_value,
            },
        )
        .await
        .unwrap();
    assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
    assert_eq!(
        snapshot.remote_state,
        Some(asterism_domain::RemoteState::Completed)
    );
    assert!(
        snapshot
            .questions
            .iter()
            .all(|question| { question.status == SubmissionQuestionVerificationStatus::Confirmed })
    );
    assert_eq!(platform.verifications.load(Ordering::Relaxed), 1);
}

async fn execute_exam_step(
    execute: &ChaoxingSubmissionExecute,
    context: &ProviderContext,
    draft: &SubmissionDraft,
    value: asterism_secrets::SecretValue,
    continuation_digest: [u8; 32],
    revision: u32,
) -> (asterism_secrets::SecretValue, ProviderSubmissionStepOutcome) {
    let operation = execute
        .prepare_submission_operation(
            context,
            "exam:100:200:exam-1",
            draft,
            ResolvedProviderQuestionSessionContinuation {
                continuation_type: CHAOXING_EXAM_QUESTION_ARTIFACT_TYPE,
                continuation_digest,
                phase: CHAOXING_EXAM_QUESTIONS_READY_PHASE,
                revision,
                value: &value,
            },
            &runtime_settings_schema().resolve(None, None, None).unwrap(),
        )
        .await
        .unwrap()
        .expect("Exam session must prepare one operation");
    let outcome = operation.execute(context, &NoopEvents).await.unwrap();
    (value, outcome)
}

async fn draft() -> SubmissionDraft {
    let task_id = TaskId::new();
    let mut questions = parse_work_preview_question_page(WORK_QUESTIONS)
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
    questions[1].kind = QuestionKind::MultipleChoice;
    questions[1].options = vec![
        QuestionOption {
            id: "A".to_owned(),
            content: Some("First".to_owned()),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
        },
        QuestionOption {
            id: "C".to_owned(),
            content: Some("Third".to_owned()),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
        },
    ];
    questions[1].metadata_sanitized["provider_type_code"] = serde_json::json!(1);
    let selected = vec![
        SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: questions[0].id,
            answer: NormalizedAnswer::Selections(vec!["B".to_owned()]),
            source: AnswerSource::Manual,
            confidence: None,
        },
        SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: questions[1].id,
            answer: NormalizedAnswer::Selections(vec!["C".to_owned(), "A".to_owned()]),
            source: AnswerSource::Manual,
            confidence: None,
        },
    ];
    let preview = ChaoxingSubmissionBuild::try_new()
        .unwrap()
        .build_submission_preview(&context(), "work:100:200:work-1", &questions, &selected)
        .await
        .unwrap();
    SubmissionDraft {
        id: SubmissionDraftId::new(),
        task_id,
        question_snapshot_id: QuestionSnapshotId::new(),
        provider_id: ProviderId::new("chaoxing").unwrap(),
        provider_version: development_metadata().unwrap().implementation_version,
        items: questions
            .into_iter()
            .zip(selected)
            .map(|(question, selected)| SubmissionDraftItem { question, selected })
            .collect(),
        payload_preview: preview,
        created_at: Utc::now(),
    }
}

async fn chapter_draft() -> SubmissionDraft {
    let task_id = TaskId::new();
    let questions = parse_chapter_work_question_page(CHAPTER_QUESTIONS)
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
    let selected = vec![
        selected(
            questions[0].id,
            NormalizedAnswer::Selections(vec!["B".to_owned()]),
        ),
        selected(
            questions[1].id,
            NormalizedAnswer::Selections(vec!["C".to_owned(), "A".to_owned()]),
        ),
        selected(
            questions[2].id,
            NormalizedAnswer::Texts(vec!["bounded".to_owned(), " answer".to_owned()]),
        ),
        selected(questions[3].id, NormalizedAnswer::Boolean(true)),
    ];
    let preview = ChaoxingSubmissionBuild::try_new()
        .unwrap()
        .build_submission_preview(
            &context(),
            "resource:100:200:4001:job-work",
            &questions,
            &selected,
        )
        .await
        .unwrap();
    SubmissionDraft {
        id: SubmissionDraftId::new(),
        task_id,
        question_snapshot_id: QuestionSnapshotId::new(),
        provider_id: ProviderId::new("chaoxing").unwrap(),
        provider_version: development_metadata().unwrap().implementation_version,
        items: questions
            .into_iter()
            .zip(selected)
            .map(|(question, selected)| SubmissionDraftItem { question, selected })
            .collect(),
        payload_preview: preview,
        created_at: Utc::now(),
    }
}

async fn exam_draft_and_artifact() -> (SubmissionDraft, ChaoxingExamQuestionArtifact) {
    let task_id = TaskId::new();
    let questions = parse_exam_question_page(EXAM_QUESTIONS)
        .unwrap()
        .into_iter()
        .take(2)
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
    let selected_answers = vec![
        selected(
            questions[0].id,
            NormalizedAnswer::Selections(vec!["A".to_owned()]),
        ),
        selected(questions[1].id, NormalizedAnswer::Boolean(true)),
    ];
    let preview = ChaoxingSubmissionBuild::try_new()
        .unwrap()
        .build_submission_preview(
            &context(),
            "exam:100:200:exam-1",
            &questions,
            &selected_answers,
        )
        .await
        .unwrap();
    let draft = SubmissionDraft {
        id: SubmissionDraftId::new(),
        task_id,
        question_snapshot_id: QuestionSnapshotId::new(),
        provider_id: ProviderId::new("chaoxing").unwrap(),
        provider_version: development_metadata().unwrap().implementation_version,
        items: questions
            .into_iter()
            .zip(selected_answers)
            .map(|(question, selected)| SubmissionDraftItem { question, selected })
            .collect(),
        payload_preview: preview,
        created_at: Utc::now(),
    };
    let course = FixtureCourses::new()
        .list_courses(&context())
        .await
        .unwrap()
        .remove(0);
    let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
    let request = ChaoxingExamQuestionRequest::try_new(
        route,
        "exam:100:200:exam-1",
        "goTest('100','exam-1',0,'SAFE_TIME','paper-1',false,'SAFE_ENC_TASK')",
    )
    .unwrap();
    let start = ChaoxingExamStartCommand::from_cover(
        task_id,
        "exam:100:200:exam-1",
        &request,
        parse_exam_cover(EXAM_COVER).unwrap(),
    )
    .unwrap();
    let material = ChaoxingExamAttemptMaterial {
        exam_answer_id: Zeroizing::new("answer-1".to_owned()),
        enc: Zeroizing::new("SAFE_PREVIEW_ENC".to_owned()),
        enc_remain_time: 3_588,
        remain_time: 3_600,
        last_update_time: 1_700_000_000_500,
    };
    let questions = draft
        .items
        .iter()
        .map(|item| item.question.clone())
        .collect::<Vec<_>>();
    let artifact =
        ChaoxingExamQuestionArtifact::from_materialization(task_id, &start, material, &questions)
            .unwrap();
    (draft, artifact)
}

fn selected(question_id: asterism_domain::QuestionId, answer: NormalizedAnswer) -> SelectedAnswer {
    SelectedAnswer {
        candidate_id: AnswerCandidateId::new(),
        question_id,
        answer,
        source: AnswerSource::Manual,
        confidence: None,
    }
}

fn receipt() -> SubmissionReceipt {
    SubmissionReceipt {
        remote_status: "accepted".to_owned(),
        message_sanitized: Some(
            "Chaoxing accepted the Work submission for independent verification".to_owned(),
        ),
        provider_trace_id: None,
        received_at: Utc::now(),
    }
}

fn context() -> ProviderContext {
    ProviderContext {
        provider_id: ProviderId::new("chaoxing").unwrap(),
        account_id: ProviderAccountId::new(),
        credential_refs: vec![SecretId::new()],
        correlation_id: "chaoxing-submission-integration".to_owned(),
    }
}

fn unexpected_call() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "unexpected Chaoxing fixture transport call",
    )
}
