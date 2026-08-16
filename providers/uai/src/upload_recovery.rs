use std::{fmt, sync::Arc};

use asterism_domain::SubmissionDraft;
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, TaskDetailCapability,
};

use crate::{
    UaiCompoundUploadPreparation, UaiCompoundUploadSubmission, UaiUploadGrantState,
    UaiUploadInputState, UaiUploadIntent, UaiUploadObjectState, UaiUploadPreparation,
    UaiUploadSubmission, build_upload_multipart,
};

/// Provider-owned read-only adapter that reconnects every recovered upload
/// stage before fresh final-plan preparation. It never dispatches or returns a
/// reconstructed object-store mutation request.
#[derive(Clone)]
pub struct UaiUploadStageRecovery {
    upload: UaiUploadPreparation,
    compound: UaiCompoundUploadPreparation,
}

impl UaiUploadStageRecovery {
    pub fn new(details: Arc<dyn TaskDetailCapability>) -> Self {
        Self {
            upload: UaiUploadPreparation::new(details.clone()),
            compound: UaiCompoundUploadPreparation::new(details),
        }
    }

    /// Freshly rebinds a recovered single-upload chain and prepares only its
    /// final submission plan.
    ///
    /// # Errors
    ///
    /// Rejects compound input, stale Task facts, any cross-state substitution
    /// or an object request digest not reproduced by the exact input/grant.
    pub async fn prepare_single_final(
        &self,
        context: &ProviderContext,
        input: &UaiUploadInputState,
        grant: &UaiUploadGrantState,
        object: &UaiUploadObjectState,
    ) -> ProviderResult<UaiUploadSubmission> {
        if input.is_compound() {
            return Err(foreign_recovery_chain());
        }
        let intent = self
            .upload
            .prepare_intent(context, input.remote_task_id(), input.artifact())
            .await?;
        validate_recovery_chain(input, &intent, grant, object, 1)?;
        self.upload
            .prepare_submission(context, object.uploaded())
            .await
    }

    /// Freshly rebinds a recovered compound-upload chain and its complete
    /// immutable ordinary Draft before preparing only the atomic final plan.
    ///
    /// # Errors
    ///
    /// Rejects single input, stale/changed Draft or Task facts, any cross-state
    /// substitution or an object request digest not reproduced by the exact
    /// input/grant.
    pub async fn prepare_compound_final(
        &self,
        context: &ProviderContext,
        input: &UaiUploadInputState,
        ordinary_draft: &SubmissionDraft,
        grant: &UaiUploadGrantState,
        object: &UaiUploadObjectState,
    ) -> ProviderResult<UaiCompoundUploadSubmission> {
        if !input.is_compound() {
            return Err(foreign_recovery_chain());
        }
        input.validate_compound_draft(ordinary_draft)?;
        let intent = self
            .upload
            .prepare_intent(context, input.remote_task_id(), input.artifact())
            .await?;
        validate_recovery_chain(input, &intent, grant, object, 2)?;
        self.compound
            .prepare_submission(context, ordinary_draft, object.uploaded())
            .await
    }
}

impl fmt::Debug for UaiUploadStageRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadStageRecovery")
            .field("upload", &"configured; object replay forbidden")
            .field("compound", &"configured; object replay forbidden")
            .finish()
    }
}

fn validate_recovery_chain(
    input: &UaiUploadInputState,
    intent: &UaiUploadIntent,
    grant_state: &UaiUploadGrantState,
    object_state: &UaiUploadObjectState,
    expected_upload_position: u32,
) -> ProviderResult<()> {
    let grant = grant_state.grant();
    let uploaded = object_state.uploaded();
    let artifact_digest = input.artifact().digest();
    if intent.remote_task_id() != input.remote_task_id()
        || intent.upload_position() != expected_upload_position
        || intent.artifact_digest() != artifact_digest
        || !intent.matches_artifact(input.artifact())
        || grant.remote_task_id() != intent.remote_task_id()
        || grant.task_fingerprint() != intent.task_fingerprint()
        || grant.course_resource_id() != intent.course_resource_id()
        || grant.unit_id() != intent.unit_id()
        || grant.group_id() != intent.group_id()
        || grant.upload_position() != intent.upload_position()
        || grant.artifact_digest() != intent.artifact_digest()
        || grant.intent_fingerprint() != intent.fingerprint()
        || uploaded.remote_task_id() != intent.remote_task_id()
        || uploaded.task_fingerprint() != intent.task_fingerprint()
        || uploaded.course_resource_id() != intent.course_resource_id()
        || uploaded.unit_id() != intent.unit_id()
        || uploaded.group_id() != intent.group_id()
        || uploaded.upload_position() != intent.upload_position()
        || uploaded.artifact_digest() != intent.artifact_digest()
        || uploaded.intent_fingerprint() != intent.fingerprint()
        || uploaded.file_key() != grant.file_key()
    {
        return Err(foreign_recovery_chain());
    }
    let multipart = build_upload_multipart(grant, input.artifact())?;
    if multipart.request_digest() != uploaded.object_request_digest() {
        return Err(foreign_recovery_chain());
    }
    Ok(())
}

fn foreign_recovery_chain() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI recovered upload stage chain is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, AssessmentClass, NormalizedAnswer, ProviderAccountId,
        ProviderId, Question, QuestionId, QuestionKind, QuestionOption, QuestionSnapshotId,
        RemoteState, SecretId, SelectedAnswer, SourceType, SubmissionAnswerCoverage,
        SubmissionDraftId, SubmissionDraftItem, SubmissionPayloadEncoding,
        SubmissionPayloadFieldPreview, SubmissionPayloadPreview, TaskCapability, TaskId,
    };
    use asterism_provider_api::{ProviderIdentity, ProviderMetadata, RemoteTask, RemoteTaskDetail};
    use async_trait::async_trait;
    use serde_json::json;

    use crate::{
        EncodedUaiUploadGrantState, EncodedUaiUploadInputState, EncodedUaiUploadObjectState,
        UaiUploadArtifact, UaiUploadedArtifact, build_upload_grant_request,
        parse_upload_grant_bound, parse_upload_object_result,
    };

    use super::*;

    const REMOTE_TASK_ID: &str = "group:2001:unit-1:group-upload";

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        detail: RemoteTaskDetail,
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            if remote_task_id != REMOTE_TASK_ID {
                return Err(foreign_recovery_chain());
            }
            Ok(self.detail.clone())
        }
    }

    #[tokio::test]
    async fn recovered_single_chain_rebinds_every_stage_without_object_replay() {
        let details: Arc<dyn TaskDetailCapability> = Arc::new(FixtureDetail {
            metadata: crate::development_metadata().unwrap(),
            detail: upload_detail("v1:upload", &["multiFileUpload"]),
        });
        let context = context();
        let preparation = UaiUploadPreparation::new(details.clone());
        let recovery = UaiUploadStageRecovery::new(details);
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let artifact_digest = artifact.digest();

        let encoded_input =
            EncodedUaiUploadInputState::for_single(REMOTE_TASK_ID, &artifact).unwrap();
        let input_digest = encoded_input.digest();
        let input_value = encoded_input.into_secret_value();
        let input = UaiUploadInputState::decode_single_bound(
            &input_value,
            input_digest,
            REMOTE_TASK_ID,
            &artifact_digest,
        )
        .unwrap();

        let intent = preparation
            .prepare_intent(&context, REMOTE_TASK_ID, input.artifact())
            .await
            .unwrap();
        let grant_request = build_upload_grant_request(
            &intent,
            input.artifact(),
            "course-instance-1",
            "app-user-1",
        )
        .unwrap();
        let grant_document =
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#;
        let grant = parse_upload_grant_bound(grant_document, &intent, &grant_request).unwrap();
        let encoded_grant = EncodedUaiUploadGrantState::try_new(&grant).unwrap();
        let grant_digest = encoded_grant.digest();
        let grant_value = encoded_grant.into_secret_value();
        let grant_state = UaiUploadGrantState::decode_bound(
            &grant_value,
            grant_digest,
            grant.grant_request_digest(),
            grant.grant_response_digest(),
        )
        .unwrap();

        let multipart = build_upload_multipart(grant_state.grant(), input.artifact()).unwrap();
        let object_document = r#"{"hash":"synthetic-qiniu-etag","key":"course/42/nothing.mp3"}"#;
        let object_result =
            parse_upload_object_result(object_document, grant_state.grant().file_key()).unwrap();
        let uploaded = UaiUploadedArtifact::from_object_result(
            grant_state.grant(),
            &multipart,
            &object_result,
        )
        .unwrap();
        let encoded_object = EncodedUaiUploadObjectState::try_new(&uploaded).unwrap();
        let object_digest = encoded_object.digest();
        let object_value = encoded_object.into_secret_value();
        let object_state = UaiUploadObjectState::decode_bound(
            &object_value,
            object_digest,
            uploaded.object_request_digest(),
            uploaded.object_response_digest(),
        )
        .unwrap();

        let final_plan = recovery
            .prepare_single_final(&context, &input, &grant_state, &object_state)
            .await
            .unwrap();
        assert_eq!(final_plan.remote_task_id(), REMOTE_TASK_ID);
        assert_eq!(final_plan.artifact_digest(), artifact_digest);

        let rotated_document =
            r#"{"code":200,"upToken":"rotated-upload-token","fileKey":"course/42/nothing.mp3"}"#;
        let rotated = parse_upload_grant_bound(rotated_document, &intent, &grant_request).unwrap();
        let rotated_encoded = EncodedUaiUploadGrantState::try_new(&rotated).unwrap();
        let rotated_digest = rotated_encoded.digest();
        let rotated_value = rotated_encoded.into_secret_value();
        let rotated_state = UaiUploadGrantState::decode_bound(
            &rotated_value,
            rotated_digest,
            rotated.grant_request_digest(),
            rotated.grant_response_digest(),
        )
        .unwrap();
        assert!(
            recovery
                .prepare_single_final(&context, &input, &rotated_state, &object_state)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn recovered_compound_chain_rebinds_the_complete_ordinary_draft() {
        let details: Arc<dyn TaskDetailCapability> = Arc::new(FixtureDetail {
            metadata: crate::development_metadata().unwrap(),
            detail: upload_detail("v1:compound-upload", &["multichoice", "multiFileUpload"]),
        });
        let context = context();
        let preparation = UaiUploadPreparation::new(details.clone());
        let recovery = UaiUploadStageRecovery::new(details);
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let artifact_digest = artifact.digest();
        let draft = ordinary_draft();

        let encoded_input =
            EncodedUaiUploadInputState::for_compound(REMOTE_TASK_ID, &artifact, &draft).unwrap();
        let input_digest = encoded_input.digest();
        let input_value = encoded_input.into_secret_value();
        let input = UaiUploadInputState::decode_compound_bound(
            &input_value,
            input_digest,
            REMOTE_TASK_ID,
            &artifact_digest,
            &draft,
        )
        .unwrap();

        let intent = preparation
            .prepare_intent(&context, REMOTE_TASK_ID, input.artifact())
            .await
            .unwrap();
        let grant_request = build_upload_grant_request(
            &intent,
            input.artifact(),
            "course-instance-1",
            "app-user-1",
        )
        .unwrap();
        let grant_document =
            r#"{"code":200,"upToken":"compound-upload-token","fileKey":"course/42/nothing.mp3"}"#;
        let grant = parse_upload_grant_bound(grant_document, &intent, &grant_request).unwrap();
        let encoded_grant = EncodedUaiUploadGrantState::try_new(&grant).unwrap();
        let grant_digest = encoded_grant.digest();
        let grant_value = encoded_grant.into_secret_value();
        let grant_state = UaiUploadGrantState::decode_bound(
            &grant_value,
            grant_digest,
            grant.grant_request_digest(),
            grant.grant_response_digest(),
        )
        .unwrap();

        let multipart = build_upload_multipart(grant_state.grant(), input.artifact()).unwrap();
        let object_document = r#"{"key":"course/42/nothing.mp3"}"#;
        let object_result =
            parse_upload_object_result(object_document, grant_state.grant().file_key()).unwrap();
        let uploaded = UaiUploadedArtifact::from_object_result(
            grant_state.grant(),
            &multipart,
            &object_result,
        )
        .unwrap();
        let encoded_object = EncodedUaiUploadObjectState::try_new(&uploaded).unwrap();
        let object_digest = encoded_object.digest();
        let object_value = encoded_object.into_secret_value();
        let object_state = UaiUploadObjectState::decode_bound(
            &object_value,
            object_digest,
            uploaded.object_request_digest(),
            uploaded.object_response_digest(),
        )
        .unwrap();

        let final_plan = recovery
            .prepare_compound_final(&context, &input, &draft, &grant_state, &object_state)
            .await
            .unwrap();
        assert_eq!(final_plan.ordinary_draft_id(), draft.id);
        assert_eq!(final_plan.artifact_digest(), artifact_digest);
    }

    fn upload_detail(task_fingerprint: &str, task_types: &[&str]) -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": "unit-1", "title": "Unit 1"},
            "section": {"id": "section-1", "title": "Section 1"},
            "micro": {"id": "micro-1", "title": "Speaking"},
            "group_id": "group-upload",
            "course_publish_version": 123_290,
            "task_types": task_types,
            "question_count": task_types.len(),
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: REMOTE_TASK_ID.to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Upload recording".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: vec![TaskCapability::BrowserBridge],
                fingerprint: task_fingerprint.to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: json!({}),
            },
            normalized_detail: json!({
                "schema": "uai.group-task-detail.v1",
                "task": normalized,
            }),
        }
    }

    fn ordinary_draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some("1001".to_owned()),
            kind: QuestionKind::MultipleChoice,
            stem: "Choose all valid answers".to_owned(),
            options: ["A", "B"]
                .into_iter()
                .map(|id| QuestionOption {
                    id: id.to_owned(),
                    content: Some(format!("Option {id}")),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                })
                .collect(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "multichoice",
                "remote_task_id": REMOTE_TASK_ID,
                "judge_types": [{"question_type":"basic","reply_type":"multichoice"}],
                "composite_children": null,
                "media_attachment_ids": [],
                "embedded_transcript": null,
                "matching_lefts": null
            }),
            position: 1,
        };
        let fields = [
            "quesDatas[].instanceId",
            "quesDatas[].answer",
            "quesDatas[].context",
            "quesDatas[].contextVersion",
            "quesDatas[].answerVersion",
        ]
        .into_iter()
        .map(|field_name| SubmissionPayloadFieldPreview {
            question_id: question.id,
            field_name: field_name.to_owned(),
        })
        .collect();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: env!("CARGO_PKG_VERSION").to_owned(),
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem {
                selected: SelectedAnswer {
                    candidate_id: AnswerCandidateId::new(),
                    question_id: question.id,
                    answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                    source: AnswerSource::Manual,
                    confidence: None,
                },
                question,
            }],
            payload_preview: SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::Json,
                format: "uai.new-exploration.json.v1".to_owned(),
                fields,
            },
            created_at: chrono::Utc::now(),
        };
        draft.validate().unwrap();
        draft
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-upload-stage-recovery".to_owned(),
        }
    }
}
