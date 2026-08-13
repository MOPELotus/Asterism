use std::{fmt, sync::Arc};

use asterism_domain::{
    RemoteState, SubmissionDraft, SubmissionQuestionVerification,
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

use crate::{CidarenSubmissionBuild, metadata::development_metadata};

/// Fresh task-level verification for a completed Cidaren answer lifecycle.
///
/// Cidaren exposes no audited answer-history readback. The verifier therefore
/// confirms only the goal-bound Task completion observation and deliberately
/// leaves the current Question unverified. It never treats a localized receipt
/// as success and never issues an assessment mutation.
pub struct CidarenSubmissionVerify {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    preview: CidarenSubmissionBuild,
}

impl CidarenSubmissionVerify {
    /// Builds a read-only verifier around fresh Task rediscovery.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(details: Arc<dyn TaskDetailCapability>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
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
        let snapshot = SubmissionVerificationSnapshot {
            status,
            remote_state: Some(detail.task.remote_state),
            score: None,
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
    let schema = detail
        .normalized_detail
        .get("schema")
        .and_then(Value::as_str);
    if detail.task.remote_id != remote_task_id
        || !matches!(
            schema,
            Some("cidaren.class-task.detail.v1" | "cidaren.study-task.detail.v1")
        )
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionBuild)
    {
        return Err(remote_changed(
            "Cidaren submission verification Task identity or capability changed",
        ));
    }
    Ok(())
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
        QuestionSnapshotId, SecretId, SelectedAnswer, SubmissionDraftId, TaskId,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;

    struct FixtureDetail {
        state: RemoteState,
        progress: u8,
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
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
                    normalized: json!({
                        "schema": "cidaren.class-task.v1",
                        "release_id": "2002",
                        "progress": self.progress,
                    }),
                    raw_sanitized: json!({}),
                },
                normalized_detail: json!({
                    "schema": "cidaren.class-task.detail.v1",
                    "release_id": "2002",
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
        let verifier = CidarenSubmissionVerify::try_new(Arc::new(FixtureDetail {
            state: RemoteState::Completed,
            progress: 100,
        }))
        .unwrap();
        let snapshot = verifier
            .verify_submission(&context(), "class-task:2002", &draft, None)
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.remote_state, Some(RemoteState::Completed));
        assert_eq!(snapshot.progress_percent, Some(100));
        assert_eq!(snapshot.questions.len(), 1);
        assert_eq!(
            snapshot.questions[0].status,
            SubmissionQuestionVerificationStatus::Unverified
        );
    }

    #[tokio::test]
    async fn receipt_is_only_context_and_pending_readback_stays_pending() {
        let draft = draft().await;
        let verifier = CidarenSubmissionVerify::try_new(Arc::new(FixtureDetail {
            state: RemoteState::InProgress,
            progress: 35,
        }))
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
    }

    #[tokio::test]
    async fn stale_preview_unknown_receipt_and_state_drift_fail_closed() {
        let mut stale = draft().await;
        stale.payload_preview.format = "cidaren.foreign.v1".to_owned();
        let verifier = CidarenSubmissionVerify::try_new(Arc::new(FixtureDetail {
            state: RemoteState::InProgress,
            progress: 35,
        }))
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

        let drifted = CidarenSubmissionVerify::try_new(Arc::new(FixtureDetail {
            state: RemoteState::Completed,
            progress: 99,
        }))
        .unwrap();
        assert_eq!(
            drifted
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
