use std::{fmt, sync::Arc};

use asterism_domain::{
    CompletionDiagnosis, RemoteState, SubmissionDraft, SubmissionQuestionVerification,
    SubmissionQuestionVerificationStatus, SubmissionReceipt, SubmissionVerificationSnapshot,
    SubmissionVerificationStatus, TaskCapability,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, SubmissionBuildCapability, SubmissionVerifyCapability, TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use crate::{
    CidarenSubmissionBuild, CidarenTaskScoreTransport, metadata::development_metadata,
    submission_build::has_complete_answer_coverage,
};

/// Fresh task-level verification for a completed Cidaren answer lifecycle.
///
/// Cidaren exposes no audited answer-history readback. The verifier therefore
/// confirms only the goal-bound Task completion observation and deliberately
/// leaves the current Question unverified. It never treats a localized receipt
/// as success and never issues an assessment mutation.
pub struct CidarenSubmissionVerify {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    scores: Arc<dyn CidarenTaskScoreTransport>,
    preview: CidarenSubmissionBuild,
}

impl CidarenSubmissionVerify {
    /// Builds a read-only verifier around fresh Task rediscovery.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        scores: Arc<dyn CidarenTaskScoreTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            scores,
            preview: CidarenSubmissionBuild::try_new()?,
        })
    }
}

impl fmt::Debug for CidarenSubmissionVerify {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenSubmissionVerify")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("scores", &"configured")
            .field("preview", &self.preview)
            .finish()
    }
}

impl ProviderIdentity for CidarenSubmissionVerify {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionVerifyCapability for CidarenSubmissionVerify {
    async fn verify_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        validate_context(context, &self.metadata)?;
        validate_draft(draft, &self.metadata, remote_task_id)?;
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = draft
            .items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        let expected_preview = self
            .preview
            .build_submission_preview(context, remote_task_id, &questions, &selected)
            .await?;
        if expected_preview != draft.payload_preview {
            return Err(remote_changed(
                "Cidaren submission verification Draft preview is stale or foreign",
            ));
        }
        if let Some(receipt) = receipt {
            receipt
                .validate()
                .map_err(|_| invalid_response("Cidaren verification receipt is invalid"))?;
            if !matches!(receipt.remote_status.as_str(), "accepted" | "completed") {
                return Err(invalid_response(
                    "Cidaren verification receipt has an unknown acknowledgement status",
                ));
            }
        }

        let detail = self.details.task_detail(context, remote_task_id).await?;
        validate_fresh_detail(&detail, remote_task_id)?;
        let progress = detail
            .task
            .normalized
            .get("progress")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= 100)
            .ok_or_else(|| protocol_drift("Cidaren verification progress is invalid"))?;
        let status = match (detail.task.remote_state, progress) {
            (RemoteState::Completed, 100) => SubmissionVerificationStatus::Confirmed,
            (RemoteState::NotOpen | RemoteState::Pending | RemoteState::InProgress, 0..=99) => {
                SubmissionVerificationStatus::Pending
            }
            (RemoteState::Expired | RemoteState::Unknown, 0..=99) => {
                SubmissionVerificationStatus::Inconclusive
            }
            _ => {
                return Err(protocol_drift(
                    "Cidaren verification state and progress disagree",
                ));
            }
        };
        let score = self
            .scores
            .fetch_task_score(context, remote_task_id, &detail)
            .await?;
        let snapshot = SubmissionVerificationSnapshot {
            status,
            remote_state: Some(detail.task.remote_state),
            score,
            progress_percent: Some(progress),
            questions: draft
                .items
                .iter()
                .map(|item| SubmissionQuestionVerification {
                    question_id: item.question.id,
                    status: SubmissionQuestionVerificationStatus::Unverified,
                })
                .collect(),
            verified_at: Utc::now(),
        };
        snapshot
            .validate()
            .map_err(|_| invalid_response("Cidaren verification snapshot is invalid"))?;
        Ok(snapshot)
    }

    fn completion_diagnosis(
        &self,
        verification: &SubmissionVerificationSnapshot,
    ) -> Option<CompletionDiagnosis> {
        (verification.status == SubmissionVerificationStatus::Inconclusive
            && verification.remote_state == Some(RemoteState::Expired)
            && verification
                .progress_percent
                .is_some_and(|progress| progress < 100))
        .then_some(CompletionDiagnosis::WindowClosed)
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren submission verification received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren submission verification requires an authenticated session",
        ));
    }
    Ok(())
}

fn validate_draft(
    draft: &SubmissionDraft,
    metadata: &ProviderMetadata,
    remote_task_id: &str,
) -> ProviderResult<()> {
    if draft.validate().is_err()
        || draft.provider_id != metadata.id
        || draft.provider_version != metadata.implementation_version
        || draft.items.len() != 1
        || !has_complete_answer_coverage(&draft.answer_coverage, draft.items.len())
        || draft.items[0].question.task_id != draft.task_id
        || draft.items[0]
            .question
            .metadata_sanitized
            .get("schema")
            .and_then(Value::as_str)
            != Some("cidaren.attempt-question.v1")
        || draft.items[0]
            .question
            .metadata_sanitized
            .get("remote_task_id")
            .and_then(Value::as_str)
            != Some(remote_task_id)
    {
        return Err(remote_changed(
            "Cidaren submission verification Draft binding is stale or foreign",
        ));
    }
    Ok(())
}

fn validate_fresh_detail(
    detail: &asterism_provider_api::RemoteTaskDetail,
    remote_task_id: &str,
) -> ProviderResult<()> {
    if detail.task.remote_id != remote_task_id
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionBuild)
    {
        return Err(remote_changed(
            "Cidaren submission verification Task identity or capability changed",
        ));
    }
    let task = detail
        .task
        .normalized
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren verification Task shape is invalid"))?;
    let course_id = task
        .get("course_id")
        .and_then(Value::as_str)
        .filter(|value| valid_task_component(value))
        .ok_or_else(|| protocol_drift("Cidaren verification Course identity is invalid"))?;
    if detail.task.course_remote_id.as_deref() != Some(format!("course:{course_id}").as_str()) {
        return Err(remote_changed(
            "Cidaren submission verification Task belongs to another Course",
        ));
    }
    if detail.normalized_detail.get("task") != Some(&detail.task.normalized) {
        return Err(remote_changed(
            "Cidaren submission verification detail no longer repeats its fresh Task",
        ));
    }

    let detail_schema = detail
        .normalized_detail
        .get("schema")
        .and_then(Value::as_str);
    if let Some(release_id) = remote_task_id.strip_prefix("class-task:") {
        if !valid_release_id(release_id) {
            return Err(protocol_drift(
                "Cidaren class verification identity is invalid",
            ));
        }
        if task.get("schema").and_then(Value::as_str) != Some("cidaren.class-task.v1")
            || task.get("release_id").and_then(Value::as_str) != Some(release_id)
            || detail_schema != Some("cidaren.class-task.detail.v1")
            || detail
                .normalized_detail
                .get("release_id")
                .and_then(Value::as_str)
                != Some(release_id)
        {
            return Err(remote_changed(
                "Cidaren class verification release identity changed",
            ));
        }
        return Ok(());
    }
    if let Some(identity) = remote_task_id.strip_prefix("study-task:")
        && let Some((remote_course_id, list_id)) = identity.split_once(':')
        && valid_task_component(remote_course_id)
        && valid_task_component(list_id)
    {
        if task.get("schema").and_then(Value::as_str) != Some("cidaren.study-task.v1")
            || course_id != remote_course_id
            || task.get("list_id").and_then(Value::as_str) != Some(list_id)
            || detail_schema != Some("cidaren.study-task.detail.v1")
            || detail
                .normalized_detail
                .get("course_id")
                .and_then(Value::as_str)
                != Some(remote_course_id)
            || detail
                .normalized_detail
                .get("list_id")
                .and_then(Value::as_str)
                != Some(list_id)
        {
            return Err(remote_changed(
                "Cidaren study verification Course/list identity changed",
            ));
        }
        return Ok(());
    }
    Err(protocol_drift(
        "Cidaren submission verification Task identity is invalid",
    ))
}

fn valid_release_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value != "0"
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_task_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderAccountId, ProviderId, Question,
        QuestionSnapshotId, SecretId, SelectedAnswer, SubmissionAnswerCoverage, SubmissionDraftId,
        SubmissionScore, TaskId,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;

    struct FixtureDetail {
        state: RemoteState,
        progress: u8,
    }

    struct FixtureScore {
        earned_milli_points: Option<u64>,
        fail: bool,
    }

    #[async_trait]
    impl CidarenTaskScoreTransport for FixtureScore {
        async fn fetch_task_score(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _detail: &RemoteTaskDetail,
        ) -> ProviderResult<Option<SubmissionScore>> {
            if self.fail {
                return Err(protocol_drift("synthetic invalid score response"));
            }
            Ok(self
                .earned_milli_points
                .map(|earned_milli_points| SubmissionScore {
                    earned_milli_points,
                    possible_milli_points: 100_000,
                }))
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let normalized = json!({
                "schema": "cidaren.class-task.v1",
                "release_id": "2002",
                "course_id": "course-a",
                "task_id": 812,
                "progress": self.progress,
                "score": null,
            });
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:course-a".to_owned()),
                    title: "Synthetic Cidaren Task".to_owned(),
                    source_type: asterism_domain::SourceType::Exam,
                    assessment_class: asterism_domain::AssessmentClass::Routine,
                    remote_state: self.state,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: [TaskCapability::SubmissionBuild].into_iter().collect(),
                    fingerprint: "fixture".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: json!({}),
                },
                normalized_detail: json!({
                    "schema": "cidaren.class-task.detail.v1",
                    "release_id": "2002",
                    "task": normalized,
                }),
            })
        }
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            unreachable!("fixture identity is not queried")
        }
    }

    #[tokio::test]
    async fn fresh_completion_confirms_task_but_not_unreadable_answer_history() {
        let draft = draft().await;
        let verifier = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::Completed,
                progress: 100,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: Some(96_500),
                fail: false,
            }),
        )
        .unwrap();
        let snapshot = verifier
            .verify_submission(&context(), "class-task:2002", &draft, None)
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.remote_state, Some(RemoteState::Completed));
        assert_eq!(snapshot.progress_percent, Some(100));
        assert_eq!(
            snapshot.score,
            Some(SubmissionScore {
                earned_milli_points: 96_500,
                possible_milli_points: 100_000,
            })
        );
        assert_eq!(snapshot.questions.len(), 1);
        assert_eq!(
            snapshot.questions[0].status,
            SubmissionQuestionVerificationStatus::Unverified
        );
        assert_eq!(verifier.completion_diagnosis(&snapshot), None);
    }

    #[tokio::test]
    async fn receipt_is_only_context_and_pending_readback_stays_pending() {
        let draft = draft().await;
        let verifier = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::InProgress,
                progress: 35,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: None,
                fail: false,
            }),
        )
        .unwrap();
        let receipt = SubmissionReceipt {
            remote_status: "completed".to_owned(),
            message_sanitized: Some("synthetic terminal acknowledgement".to_owned()),
            provider_trace_id: None,
            received_at: Utc::now(),
        };
        let snapshot = verifier
            .verify_submission(&context(), "class-task:2002", &draft, Some(&receipt))
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Pending);
        assert_eq!(snapshot.progress_percent, Some(35));
        for state in [
            RemoteState::NotOpen,
            RemoteState::Pending,
            RemoteState::InProgress,
        ] {
            let mut unfinished = snapshot.clone();
            unfinished.remote_state = Some(state);
            assert_eq!(verifier.completion_diagnosis(&unfinished), None);
        }
    }

    #[tokio::test]
    async fn fresh_verification_rebinds_class_course_release_and_exact_detail_copy() {
        let detail = FixtureDetail {
            state: RemoteState::Completed,
            progress: 100,
        }
        .task_detail(&context(), "class-task:2002")
        .await
        .unwrap();
        validate_fresh_detail(&detail, "class-task:2002").unwrap();

        let mut drifted = detail.clone();
        drifted.task.course_remote_id = Some("course:course-b".to_owned());
        assert_eq!(
            validate_fresh_detail(&drifted, "class-task:2002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        let mut drifted = detail.clone();
        drifted.task.normalized["release_id"] = json!("2003");
        assert_eq!(
            validate_fresh_detail(&drifted, "class-task:2002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        let mut drifted = detail.clone();
        drifted.normalized_detail["release_id"] = json!("2003");
        assert_eq!(
            validate_fresh_detail(&drifted, "class-task:2002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        let mut drifted = detail.clone();
        drifted.normalized_detail["task"]["progress"] = json!(99);
        assert_eq!(
            validate_fresh_detail(&drifted, "class-task:2002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(
            validate_fresh_detail(&detail, "class-task:02002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn fresh_verification_rebinds_study_course_list_and_exact_detail_copy() {
        let normalized = json!({
            "schema": "cidaren.study-task.v1",
            "course_id": "course-a",
            "list_id": "course-a_02",
            "task_id": 71002,
            "progress": 100,
        });
        let detail = RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "study-task:course-a:course-a_02".to_owned(),
                course_remote_id: Some("course:course-a".to_owned()),
                title: "Synthetic Study Task".to_owned(),
                source_type: asterism_domain::SourceType::Practice,
                assessment_class: asterism_domain::AssessmentClass::Routine,
                remote_state: RemoteState::Completed,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: [TaskCapability::SubmissionBuild].into_iter().collect(),
                fingerprint: "fixture-study".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: json!({}),
            },
            normalized_detail: json!({
                "schema": "cidaren.study-task.detail.v1",
                "course_id": "course-a",
                "list_id": "course-a_02",
                "task": normalized,
            }),
        };
        validate_fresh_detail(&detail, "study-task:course-a:course-a_02").unwrap();

        for drifted in [
            {
                let mut value = detail.clone();
                value.task.course_remote_id = Some("course:course-b".to_owned());
                value
            },
            {
                let mut value = detail.clone();
                value.task.normalized["list_id"] = json!("course-a_03");
                value
            },
            {
                let mut value = detail.clone();
                value.normalized_detail["course_id"] = json!("course-b");
                value
            },
            {
                let mut value = detail.clone();
                value.normalized_detail["list_id"] = json!("course-a_03");
                value
            },
            {
                let mut value = detail.clone();
                value.normalized_detail["task"]["progress"] = json!(99);
                value
            },
        ] {
            assert_eq!(
                validate_fresh_detail(&drifted, "study-task:course-a:course-a_02")
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::RemoteChanged
            );
        }
    }

    #[tokio::test]
    async fn expired_task_reports_window_closed_without_guessing_unknown_state() {
        let draft = draft().await;
        let expired = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::Expired,
                progress: 35,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: None,
                fail: false,
            }),
        )
        .unwrap();
        let snapshot = expired
            .verify_submission(&context(), "class-task:2002", &draft, None)
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);
        assert_eq!(
            expired.completion_diagnosis(&snapshot),
            Some(CompletionDiagnosis::WindowClosed)
        );

        let mut incomplete_fact = snapshot.clone();
        incomplete_fact.progress_percent = None;
        assert_eq!(expired.completion_diagnosis(&incomplete_fact), None);
        incomplete_fact = snapshot.clone();
        incomplete_fact.status = SubmissionVerificationStatus::Pending;
        assert_eq!(expired.completion_diagnosis(&incomplete_fact), None);

        let unknown = SubmissionVerificationSnapshot {
            remote_state: Some(RemoteState::Unknown),
            ..snapshot
        };
        assert_eq!(expired.completion_diagnosis(&unknown), None);
    }

    #[tokio::test]
    async fn partial_coverage_is_rejected_before_remote_verification() {
        let mut draft = draft().await;
        draft.answer_coverage = SubmissionAnswerCoverage {
            total_question_count: 2,
            minimum_coverage_millis: 500,
            unanswered_question_ids: vec![asterism_domain::QuestionId::new()],
        };
        draft.validate().unwrap();
        let verifier = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::Completed,
                progress: 100,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: None,
                fail: false,
            }),
        )
        .unwrap();
        assert_eq!(
            verifier
                .verify_submission(&context(), "class-task:2002", &draft, None)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn stale_preview_unknown_receipt_and_state_drift_fail_closed() {
        let mut stale = draft().await;
        stale.payload_preview.format = "cidaren.foreign.v1".to_owned();
        let verifier = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::InProgress,
                progress: 35,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: None,
                fail: false,
            }),
        )
        .unwrap();
        assert_eq!(
            verifier
                .verify_submission(&context(), "class-task:2002", &stale, None)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );

        let draft = draft().await;
        let receipt = SubmissionReceipt {
            remote_status: "unknown".to_owned(),
            message_sanitized: None,
            provider_trace_id: None,
            received_at: Utc::now(),
        };
        assert_eq!(
            verifier
                .verify_submission(&context(), "class-task:2002", &draft, Some(&receipt),)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );

        let drifted = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::Completed,
                progress: 99,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: None,
                fail: false,
            }),
        )
        .unwrap();
        assert_eq!(
            drifted
                .verify_submission(&context(), "class-task:2002", &draft, None)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let invalid_score = CidarenSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                state: RemoteState::Completed,
                progress: 100,
            }),
            Arc::new(FixtureScore {
                earned_milli_points: None,
                fail: true,
            }),
        )
        .unwrap();
        assert_eq!(
            invalid_score
                .verify_submission(&context(), "class-task:2002", &draft, None)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    async fn draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id,
            remote_question_id: Some("question:synthetic".to_owned()),
            kind: asterism_domain::QuestionKind::SingleChoice,
            stem: "Synthetic Cidaren Question".to_owned(),
            options: vec![asterism_domain::QuestionOption {
                id: "n:0".to_owned(),
                content: Some("first".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
            }],
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "cidaren.attempt-question.v1",
                "remote_task_id": "class-task:2002",
                "topic_mode": 17,
            }),
            position: 1,
        };
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: asterism_domain::NormalizedAnswer::Selections(vec!["n:0".to_owned()]),
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let preview = CidarenSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "class-task:2002",
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("cidaren").unwrap(),
            provider_version: env!("CARGO_PKG_VERSION").to_owned(),
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![asterism_domain::SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-submission-verify-test".to_owned(),
        }
    }
}
