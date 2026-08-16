use std::{fmt, str::FromStr};

use asterism_domain::SubmissionDraftId;
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    UaiCompoundUploadSubmission, UaiCompoundUploadSubmissionRequest, UaiSubmissionPlan,
    UaiUploadFinalSubmissionKind, UaiUploadFinalSubmissionSequence, UaiUploadSubmission,
    UaiUploadSubmissionRequest,
};

pub const UAI_UPLOAD_FINAL_PLAN_STATE_TYPE: &str = "uai.upload.final-plan-state.v1";

const MAX_UPLOAD_FINAL_PLAN_STATE_BYTES: usize = 4 * 1_024 * 1_024;

/// Bounded zeroizing bytes for one complete single/compound upload final plan.
pub struct EncodedUaiUploadFinalPlanState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiUploadFinalPlanState {
    /// Encodes the complete single-upload plan without exposing the object key
    /// through ordinary execution metadata.
    ///
    /// # Errors
    ///
    /// Rejects a plan that cannot be represented by the bounded strict state
    /// schema or no longer reproduces its compact sequence projection.
    pub fn for_single(
        submission: &UaiUploadSubmission,
        request: &UaiUploadSubmissionRequest,
    ) -> ProviderResult<Self> {
        let sequence = UaiUploadFinalSubmissionSequence::for_single(submission)?;
        if request.sequence_binding_digest() != submission.final_sequence_binding_digest()
            || request.request_digest() == [0; 32]
        {
            return Err(foreign_plan_state());
        }
        encode_wire(&UploadFinalPlanWireRef {
            schema: UAI_UPLOAD_FINAL_PLAN_STATE_TYPE,
            submission_kind: "single",
            single: Some(SingleUploadFinalPlanWireRef {
                remote_task_id: submission.remote_task_id(),
                course_resource_id: submission.course_resource_id(),
                unit_id: submission.unit_id(),
                group_id: submission.group_id(),
                file_key: submission.expose_file_key(),
                artifact_digest: submission.artifact_digest(),
                upload_intent_fingerprint: submission.upload_intent_fingerprint(),
                course_publish_version: submission.course_publish_version(),
                fingerprint: submission.fingerprint(),
            }),
            compound: None,
            request_digest: request.request_digest(),
            sequence_artifact_digest: sequence.artifact().artifact_digest(),
            sequence_plan_digest: sequence.plan().plan_digest(),
        })
    }

    /// Encodes the complete atomic ordinary-answer plus object-key final plan.
    ///
    /// # Errors
    ///
    /// Rejects a non-single ordinary Question plan, malformed judge shape or
    /// state that cannot reproduce the compact compound sequence projection.
    pub fn for_compound(
        submission: &UaiCompoundUploadSubmission,
        request: &UaiCompoundUploadSubmissionRequest,
    ) -> ProviderResult<Self> {
        let sequence = UaiUploadFinalSubmissionSequence::for_compound(submission)?;
        if request.sequence_binding_digest() != submission.final_sequence_binding_digest()
            || request.request_digest() == [0; 32]
        {
            return Err(foreign_plan_state());
        }
        let question = submission
            .ordinary_plan()
            .questions()
            .first()
            .filter(|_| submission.ordinary_plan().questions().len() == 1)
            .ok_or_else(invalid_plan_state)?;
        let judges = question
            .judges()
            .iter()
            .map(|judge| CompoundJudgeWireRef {
                question_type: judge.question_type(),
                reply_type: judge.reply_type(),
            })
            .collect();
        let ordinary_draft_id = Zeroizing::new(submission.ordinary_draft_id().to_string());
        encode_wire(&UploadFinalPlanWireRef {
            schema: UAI_UPLOAD_FINAL_PLAN_STATE_TYPE,
            submission_kind: "compound",
            single: None,
            compound: Some(CompoundUploadFinalPlanWireRef {
                ordinary_draft_id: ordinary_draft_id.as_str(),
                remote_task_id: submission.remote_task_id(),
                course_resource_id: submission.course_resource_id(),
                unit_id: submission.unit_id(),
                group_id: submission.group_id(),
                task_fingerprint: submission.task_fingerprint(),
                file_key: submission.expose_file_key(),
                artifact_digest: submission.artifact_digest(),
                upload_intent_fingerprint: submission.upload_intent_fingerprint(),
                course_publish_version: submission.course_publish_version(),
                ordinary_question: CompoundQuestionWireRef {
                    remote_question_id: question.remote_question_id(),
                    task_type: question.task_type(),
                    answer_children: question.answer_children(),
                    judges,
                },
                protocol_course_version: submission.ordinary_plan().protocol_versions().course(),
                protocol_answer_version: submission.ordinary_plan().protocol_versions().answer(),
                fingerprint: submission.fingerprint(),
            }),
            request_digest: request.request_digest(),
            sequence_artifact_digest: sequence.artifact().artifact_digest(),
            sequence_plan_digest: sequence.plan().plan_digest(),
        })
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiUploadFinalPlanState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiUploadFinalPlanState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Complete decoded Provider plan rebound to Core's compact sequence identity.
pub enum UaiUploadFinalPlanState {
    Single {
        submission: UaiUploadSubmission,
        request_digest: [u8; 32],
    },
    Compound {
        submission: UaiCompoundUploadSubmission,
        request_digest: [u8; 32],
    },
}

impl UaiUploadFinalPlanState {
    /// Decodes one exact private plan and requires it to reproduce the
    /// independently persisted compact artifact and sequence plan.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, digest-mismatched or foreign single/
    /// compound state before exposing object keys or selected answers.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_sequence: &UaiUploadFinalSubmissionSequence,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_UPLOAD_FINAL_PLAN_STATE_BYTES {
            return Err(invalid_plan_state());
        }
        if <[u8; 32]>::from(Sha256::digest(bytes)) != expected_digest {
            return Err(foreign_plan_state());
        }
        let mut wire: UploadFinalPlanWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_plan_state())?;
        if wire.schema != UAI_UPLOAD_FINAL_PLAN_STATE_TYPE
            || wire.sequence_artifact_digest != expected_sequence.artifact().artifact_digest()
            || wire.sequence_plan_digest != expected_sequence.plan().plan_digest()
            || [
                wire.request_digest,
                wire.sequence_artifact_digest,
                wire.sequence_plan_digest,
            ]
            .contains(&[0; 32])
        {
            return Err(foreign_plan_state());
        }
        let request_digest = wire.request_digest;
        match wire.submission_kind.as_str() {
            "single" if wire.compound.is_none() => {
                let single = wire.single.take().ok_or_else(foreign_plan_state)?;
                let submission = restore_single(single)?;
                validate_sequence(
                    &UaiUploadFinalSubmissionSequence::for_single(&submission)?,
                    expected_sequence,
                )?;
                Ok(Self::Single {
                    submission,
                    request_digest,
                })
            }
            "compound" if wire.single.is_none() => {
                let compound = wire.compound.take().ok_or_else(foreign_plan_state)?;
                let submission = restore_compound(compound)?;
                validate_sequence(
                    &UaiUploadFinalSubmissionSequence::for_compound(&submission)?,
                    expected_sequence,
                )?;
                Ok(Self::Compound {
                    submission,
                    request_digest,
                })
            }
            _ => Err(foreign_plan_state()),
        }
    }

    pub const fn kind(&self) -> UaiUploadFinalSubmissionKind {
        match self {
            Self::Single { .. } => UaiUploadFinalSubmissionKind::Single,
            Self::Compound { .. } => UaiUploadFinalSubmissionKind::Compound,
        }
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        match self {
            Self::Single { request_digest, .. } | Self::Compound { request_digest, .. } => {
                *request_digest
            }
        }
    }

    pub const fn as_single(&self) -> Option<&UaiUploadSubmission> {
        match self {
            Self::Single { submission, .. } => Some(submission),
            Self::Compound { .. } => None,
        }
    }

    pub const fn as_compound(&self) -> Option<&UaiCompoundUploadSubmission> {
        match self {
            Self::Compound { submission, .. } => Some(submission),
            Self::Single { .. } => None,
        }
    }
}

impl fmt::Debug for UaiUploadFinalPlanState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadFinalPlanState")
            .field("kind", &self.kind())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

fn encode_wire(
    wire: &UploadFinalPlanWireRef<'_>,
) -> ProviderResult<EncodedUaiUploadFinalPlanState> {
    let mut encoded = Zeroizing::new(serde_json::to_vec(wire).map_err(|_| invalid_plan_state())?);
    if encoded.is_empty() || encoded.len() > MAX_UPLOAD_FINAL_PLAN_STATE_BYTES {
        return Err(invalid_plan_state());
    }
    let digest = Sha256::digest(encoded.as_slice()).into();
    let value = SecretValue::new(std::mem::take(&mut *encoded));
    Ok(EncodedUaiUploadFinalPlanState { value, digest })
}

fn restore_single(mut wire: SingleUploadFinalPlanWire) -> ProviderResult<UaiUploadSubmission> {
    UaiUploadSubmission::restore_final_plan(
        std::mem::take(&mut wire.remote_task_id),
        std::mem::take(&mut wire.course_resource_id),
        std::mem::take(&mut wire.unit_id),
        std::mem::take(&mut wire.group_id),
        std::mem::take(&mut wire.file_key),
        std::mem::take(&mut wire.artifact_digest),
        std::mem::take(&mut wire.upload_intent_fingerprint),
        wire.course_publish_version,
        std::mem::take(&mut wire.fingerprint),
    )
}

fn restore_compound(
    mut wire: CompoundUploadFinalPlanWire,
) -> ProviderResult<UaiCompoundUploadSubmission> {
    if wire.ordinary_question.task_type != "multichoice"
        || wire.protocol_course_version != wire.course_publish_version
        || wire.protocol_answer_version != 3
    {
        return Err(foreign_plan_state());
    }
    let ordinary_draft_id =
        SubmissionDraftId::from_str(&wire.ordinary_draft_id).map_err(|_| foreign_plan_state())?;
    let judges = std::mem::take(&mut wire.ordinary_question.judges)
        .into_iter()
        .map(|mut judge| {
            (
                std::mem::take(&mut judge.question_type),
                std::mem::take(&mut judge.reply_type),
            )
        })
        .collect();
    let ordinary_plan = UaiSubmissionPlan::restore_compound_upload(
        std::mem::take(&mut wire.ordinary_question.remote_question_id),
        std::mem::take(&mut wire.ordinary_question.answer_children),
        judges,
        wire.course_publish_version,
    )?;
    UaiCompoundUploadSubmission::restore_final_plan(
        ordinary_draft_id,
        std::mem::take(&mut wire.remote_task_id),
        std::mem::take(&mut wire.course_resource_id),
        std::mem::take(&mut wire.unit_id),
        std::mem::take(&mut wire.group_id),
        std::mem::take(&mut wire.task_fingerprint),
        std::mem::take(&mut wire.file_key),
        std::mem::take(&mut wire.artifact_digest),
        std::mem::take(&mut wire.upload_intent_fingerprint),
        wire.course_publish_version,
        ordinary_plan,
        std::mem::take(&mut wire.fingerprint),
    )
}

fn validate_sequence(
    actual: &UaiUploadFinalSubmissionSequence,
    expected: &UaiUploadFinalSubmissionSequence,
) -> ProviderResult<()> {
    if actual.kind() != expected.kind()
        || actual.artifact().artifact_digest() != expected.artifact().artifact_digest()
        || actual.plan().plan_digest() != expected.plan().plan_digest()
    {
        Err(foreign_plan_state())
    } else {
        Ok(())
    }
}

#[derive(Serialize)]
struct UploadFinalPlanWireRef<'a> {
    schema: &'static str,
    submission_kind: &'static str,
    single: Option<SingleUploadFinalPlanWireRef<'a>>,
    compound: Option<CompoundUploadFinalPlanWireRef<'a>>,
    request_digest: [u8; 32],
    sequence_artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

#[derive(Serialize)]
struct SingleUploadFinalPlanWireRef<'a> {
    remote_task_id: &'a str,
    course_resource_id: &'a str,
    unit_id: &'a str,
    group_id: &'a str,
    file_key: &'a str,
    artifact_digest: &'a str,
    upload_intent_fingerprint: &'a str,
    course_publish_version: u64,
    fingerprint: &'a str,
}

#[derive(Serialize)]
struct CompoundUploadFinalPlanWireRef<'a> {
    ordinary_draft_id: &'a str,
    remote_task_id: &'a str,
    course_resource_id: &'a str,
    unit_id: &'a str,
    group_id: &'a str,
    task_fingerprint: &'a str,
    file_key: &'a str,
    artifact_digest: &'a str,
    upload_intent_fingerprint: &'a str,
    course_publish_version: u64,
    ordinary_question: CompoundQuestionWireRef<'a>,
    protocol_course_version: u64,
    protocol_answer_version: u64,
    fingerprint: &'a str,
}

#[derive(Serialize)]
struct CompoundQuestionWireRef<'a> {
    remote_question_id: &'a str,
    task_type: &'a str,
    answer_children: &'a [Vec<String>],
    judges: Vec<CompoundJudgeWireRef<'a>>,
}

#[derive(Serialize)]
struct CompoundJudgeWireRef<'a> {
    question_type: &'a str,
    reply_type: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct UploadFinalPlanWire {
    schema: String,
    submission_kind: String,
    single: Option<SingleUploadFinalPlanWire>,
    compound: Option<CompoundUploadFinalPlanWire>,
    request_digest: [u8; 32],
    sequence_artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct SingleUploadFinalPlanWire {
    remote_task_id: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    file_key: String,
    artifact_digest: String,
    upload_intent_fingerprint: String,
    course_publish_version: u64,
    fingerprint: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundUploadFinalPlanWire {
    ordinary_draft_id: String,
    remote_task_id: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    task_fingerprint: String,
    file_key: String,
    artifact_digest: String,
    upload_intent_fingerprint: String,
    course_publish_version: u64,
    ordinary_question: CompoundQuestionWire,
    protocol_course_version: u64,
    protocol_answer_version: u64,
    fingerprint: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundQuestionWire {
    remote_question_id: String,
    task_type: String,
    answer_children: Vec<Vec<String>>,
    judges: Vec<CompoundJudgeWire>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundJudgeWire {
    question_type: String,
    reply_type: String,
}

fn invalid_plan_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI upload final-plan state is invalid",
    )
}

fn foreign_plan_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI upload final-plan state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_final_plan_round_trips_only_against_exact_sequence() {
        let submission = UaiUploadSubmission::fixture("course/42/nothing.mp3", "fixture-a");
        let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
        let request =
            crate::build_upload_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let encoded = EncodedUaiUploadFinalPlanState::for_single(&submission, &request).unwrap();
        assert!(!format!("{encoded:?}").contains("course/42/nothing.mp3"));
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let restored = UaiUploadFinalPlanState::decode_bound(&value, digest, &sequence).unwrap();
        let restored = restored.as_single().unwrap();
        assert_eq!(restored.remote_task_id(), submission.remote_task_id());
        assert_eq!(restored.expose_file_key(), submission.expose_file_key());
        assert_eq!(restored.fingerprint(), submission.fingerprint());
        assert_eq!(
            UaiUploadFinalPlanState::decode_bound(&value, digest, &sequence)
                .unwrap()
                .request_digest(),
            request.request_digest()
        );
        assert!(!format!("{restored:?}").contains("course/42/nothing.mp3"));

        assert!(UaiUploadFinalPlanState::decode_bound(&value, [7; 32], &sequence).is_err());
        let foreign_submission = UaiUploadSubmission::fixture("course/42/other.mp3", "fixture-b");
        let foreign_sequence =
            UaiUploadFinalSubmissionSequence::for_single(&foreign_submission).unwrap();
        assert!(UaiUploadFinalPlanState::decode_bound(&value, digest, &foreign_sequence).is_err());

        let tampered = String::from_utf8(value.expose_secret().to_vec())
            .unwrap()
            .replace("course/42/nothing.mp3", "course/42/tampered.mp3");
        let tampered = SecretValue::new(tampered.into_bytes());
        let tampered_digest = Sha256::digest(tampered.expose_secret()).into();
        assert!(
            UaiUploadFinalPlanState::decode_bound(&tampered, tampered_digest, &sequence).is_err()
        );
    }
}
