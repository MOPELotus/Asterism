use std::{fmt, str::FromStr};

use asterism_domain::SubmissionDraftId;
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    UaiCompoundOralSubmission, UaiCompoundOralSubmissionRequest, UaiCompoundOralSubmissionSequence,
    UaiSubmissionPlan, compound_oral::OralChildEvidence,
};

pub const UAI_COMPOUND_ORAL_PLAN_STATE_TYPE: &str = "uai.compound-oral.plan-state.v1";

const MAX_COMPOUND_ORAL_PLAN_STATE_BYTES: usize = 4 * 1_024 * 1_024;

/// Bounded secret bytes for one complete executable compound-oral plan.
///
/// The caller must place this value in the encrypted Provider execution-state
/// store. Core receives only the compact artifact and hash-only identities.
pub struct EncodedUaiCompoundOralPlanState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiCompoundOralPlanState {
    /// Encodes all ordinary answer and oral evidence while binding it to the
    /// independently materialized request and compact Core sequence.
    ///
    /// # Errors
    ///
    /// Rejects foreign request material or an unrepresentable bounded state.
    pub fn try_new(
        submission: &UaiCompoundOralSubmission,
        request: &UaiCompoundOralSubmissionRequest,
    ) -> ProviderResult<Self> {
        let sequence = UaiCompoundOralSubmissionSequence::try_new(submission)?;
        let plan_binding_digest = submission.plan_binding_digest()?;
        if request.plan_binding_digest() != plan_binding_digest
            || request.request_digest() == [0; 32]
        {
            return Err(foreign_plan_state());
        }
        let ordinary = submission.ordinary_question()?;
        let mut oral_children = Vec::with_capacity(submission.oral_children().len());
        for child in submission.oral_children() {
            oral_children.push(CompoundOralChildWire {
                value_json: serde_json::to_string(child.value())
                    .map_err(|_| invalid_plan_state())?,
                extra_json: child
                    .extra()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|_| invalid_plan_state())?,
                judge_value: child.judge_value().to_owned(),
            });
        }
        let judges = ordinary
            .judges()
            .iter()
            .map(|judge| CompoundOralJudgeWire {
                question_type: judge.question_type().to_owned(),
                reply_type: judge.reply_type().to_owned(),
            })
            .collect();
        let mut wire = CompoundOralPlanWire {
            schema: UAI_COMPOUND_ORAL_PLAN_STATE_TYPE.to_owned(),
            ordinary_draft_id: submission.ordinary_draft_id().to_string(),
            remote_task_id: submission.remote_task_id().to_owned(),
            course_resource_id: submission.course_resource_id().to_owned(),
            unit_id: submission.unit_id().to_owned(),
            group_id: submission.group_id().to_owned(),
            task_fingerprint: submission.task_fingerprint().to_owned(),
            course_publish_version: submission.course_publish_version(),
            ordinary_question: CompoundOralQuestionWire {
                remote_question_id: ordinary.remote_question_id().to_owned(),
                task_type: ordinary.task_type().to_owned(),
                answer_children: ordinary.answer_children().to_vec(),
                judges,
            },
            protocol_course_version: submission.ordinary_plan().protocol_versions().course(),
            protocol_answer_version: submission.ordinary_plan().protocol_versions().answer(),
            oral_instance_id: submission.oral_instance_id().to_owned(),
            oral_children,
            fingerprint: submission.fingerprint().to_owned(),
            plan_binding_digest,
            request_digest: request.request_digest(),
            sequence_artifact_digest: sequence.artifact().artifact_digest(),
            sequence_plan_digest: sequence.plan().plan_digest(),
        };
        let mut encoded =
            Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| invalid_plan_state())?);
        wire.zeroize();
        if encoded.is_empty() || encoded.len() > MAX_COMPOUND_ORAL_PLAN_STATE_BYTES {
            return Err(invalid_plan_state());
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(Self { value, digest })
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiCompoundOralPlanState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiCompoundOralPlanState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Complete decoded Provider plan rebound to Core's compact artifact,
/// sequence and independently persisted exact request identity.
pub struct UaiCompoundOralPlanState {
    submission: UaiCompoundOralSubmission,
    request_digest: [u8; 32],
}

impl UaiCompoundOralPlanState {
    /// Decodes one exact private plan only when every persisted identity agrees.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, digest-mismatched, answer-substituted,
    /// oral-substituted or request-substituted state.
    pub fn decode_bound(
        value: &SecretValue,
        expected_state_digest: [u8; 32],
        expected_sequence: &UaiCompoundOralSubmissionSequence,
        expected_request_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty()
            || bytes.len() > MAX_COMPOUND_ORAL_PLAN_STATE_BYTES
            || expected_request_digest == [0; 32]
            || <[u8; 32]>::from(Sha256::digest(bytes)) != expected_state_digest
        {
            return Err(foreign_plan_state());
        }
        let mut wire: CompoundOralPlanWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_plan_state())?;
        if wire.schema != UAI_COMPOUND_ORAL_PLAN_STATE_TYPE
            || wire.request_digest != expected_request_digest
            || wire.sequence_artifact_digest != expected_sequence.artifact().artifact_digest()
            || wire.sequence_plan_digest != expected_sequence.plan().plan_digest()
            || [
                wire.plan_binding_digest,
                wire.request_digest,
                wire.sequence_artifact_digest,
                wire.sequence_plan_digest,
            ]
            .contains(&[0; 32])
            || wire.protocol_course_version != wire.course_publish_version
            || wire.protocol_answer_version != 3
        {
            return Err(foreign_plan_state());
        }
        let ordinary_draft_id = SubmissionDraftId::from_str(&wire.ordinary_draft_id)
            .map_err(|_| foreign_plan_state())?;
        let judges = std::mem::take(&mut wire.ordinary_question.judges)
            .into_iter()
            .map(|mut judge| {
                (
                    std::mem::take(&mut judge.question_type),
                    std::mem::take(&mut judge.reply_type),
                )
            })
            .collect();
        let ordinary_plan = UaiSubmissionPlan::restore_compound_oral(
            std::mem::take(&mut wire.ordinary_question.remote_question_id),
            std::mem::take(&mut wire.ordinary_question.task_type),
            std::mem::take(&mut wire.ordinary_question.answer_children),
            judges,
            wire.course_publish_version,
        )?;
        let oral_children = std::mem::take(&mut wire.oral_children)
            .into_iter()
            .map(|mut child| {
                let value_json = Zeroizing::new(std::mem::take(&mut child.value_json));
                let extra_json = child.extra_json.take().map(Zeroizing::new);
                OralChildEvidence::restore(
                    value_json.as_str(),
                    extra_json.as_deref().map(String::as_str),
                    std::mem::take(&mut child.judge_value),
                )
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        let stored_plan_binding_digest = wire.plan_binding_digest;
        let request_digest = wire.request_digest;
        let submission = UaiCompoundOralSubmission::restore_plan_state(
            ordinary_draft_id,
            std::mem::take(&mut wire.remote_task_id),
            std::mem::take(&mut wire.course_resource_id),
            std::mem::take(&mut wire.unit_id),
            std::mem::take(&mut wire.group_id),
            std::mem::take(&mut wire.task_fingerprint),
            wire.course_publish_version,
            ordinary_plan,
            std::mem::take(&mut wire.oral_instance_id),
            oral_children,
            std::mem::take(&mut wire.fingerprint),
        )?;
        let actual_sequence = UaiCompoundOralSubmissionSequence::try_new(&submission)?;
        if submission.plan_binding_digest()? != stored_plan_binding_digest
            || actual_sequence.artifact().artifact_digest()
                != expected_sequence.artifact().artifact_digest()
            || actual_sequence.plan().plan_digest() != expected_sequence.plan().plan_digest()
        {
            return Err(foreign_plan_state());
        }
        Ok(Self {
            submission,
            request_digest,
        })
    }

    pub const fn submission(&self) -> &UaiCompoundOralSubmission {
        &self.submission
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    /// Rebinds a freshly materialized issue-time request to this recovered
    /// private plan without exposing its body or account identity.
    ///
    /// # Errors
    ///
    /// Rejects a changed Course/account request or a request built from other
    /// semantic plan material.
    pub fn validate_request(
        &self,
        request: &UaiCompoundOralSubmissionRequest,
    ) -> ProviderResult<()> {
        if request.request_digest() != self.request_digest
            || request.plan_binding_digest() != self.submission.plan_binding_digest()?
        {
            Err(foreign_plan_state())
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for UaiCompoundOralPlanState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralPlanState")
            .field("content", &"[REDACTED]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundOralPlanWire {
    schema: String,
    ordinary_draft_id: String,
    remote_task_id: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    task_fingerprint: String,
    course_publish_version: u64,
    ordinary_question: CompoundOralQuestionWire,
    protocol_course_version: u64,
    protocol_answer_version: u64,
    oral_instance_id: String,
    oral_children: Vec<CompoundOralChildWire>,
    fingerprint: String,
    plan_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    sequence_artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundOralQuestionWire {
    remote_question_id: String,
    task_type: String,
    answer_children: Vec<Vec<String>>,
    judges: Vec<CompoundOralJudgeWire>,
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundOralJudgeWire {
    question_type: String,
    reply_type: String,
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundOralChildWire {
    value_json: String,
    extra_json: Option<String>,
    judge_value: String,
}

fn invalid_plan_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI compound oral private plan state is invalid",
    )
}

fn foreign_plan_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI compound oral private plan state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::SubmissionDraftId;
    use serde_json::json;

    use super::*;
    use crate::build_compound_oral_submission_request;

    #[test]
    fn private_plan_round_trips_only_against_exact_sequence_and_request() {
        let submission = UaiCompoundOralSubmission::fixture(
            SubmissionDraftId::new(),
            "right",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission).unwrap();
        let request =
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let encoded = EncodedUaiCompoundOralPlanState::try_new(&submission, &request).unwrap();
        let debug = format!("{encoded:?}");
        assert!(!debug.contains("right"));
        assert!(!debug.contains("spoken"));
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let restored = UaiCompoundOralPlanState::decode_bound(
            &value,
            digest,
            &sequence,
            request.request_digest(),
        )
        .unwrap();
        assert_eq!(restored.request_digest(), request.request_digest());
        assert_eq!(
            restored.submission().plan_binding_digest().unwrap(),
            submission.plan_binding_digest().unwrap()
        );
        restored.validate_request(&request).unwrap();
        assert!(!format!("{restored:?}").contains("spoken"));

        let foreign_request =
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-2")
                .unwrap();
        assert!(restored.validate_request(&foreign_request).is_err());
        assert!(
            UaiCompoundOralPlanState::decode_bound(
                &value,
                digest,
                &sequence,
                foreign_request.request_digest(),
            )
            .is_err()
        );
        assert!(
            UaiCompoundOralPlanState::decode_bound(
                &value,
                [7; 32],
                &sequence,
                request.request_digest(),
            )
            .is_err()
        );

        let tampered = String::from_utf8(value.expose_secret().to_vec())
            .unwrap()
            .replace("spoken", "edited");
        let tampered = SecretValue::new(tampered.into_bytes());
        let tampered_digest = Sha256::digest(tampered.expose_secret()).into();
        assert!(
            UaiCompoundOralPlanState::decode_bound(
                &tampered,
                tampered_digest,
                &sequence,
                request.request_digest(),
            )
            .is_err()
        );
    }

    #[test]
    fn answer_or_oral_substitution_cannot_rebind_private_state() {
        let draft_id = SubmissionDraftId::new();
        let original = UaiCompoundOralSubmission::fixture(
            draft_id,
            "right",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        let request =
            build_compound_oral_submission_request(&original, "course-instance-1", "openid-1")
                .unwrap();
        let encoded = EncodedUaiCompoundOralPlanState::try_new(&original, &request).unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();

        for foreign in [
            UaiCompoundOralSubmission::fixture(
                draft_id,
                "changed",
                json!(["spoken"]),
                Some(json!({"slot": 1})),
            ),
            UaiCompoundOralSubmission::fixture(
                draft_id,
                "right",
                json!(["changed"]),
                Some(json!({"slot": 1})),
            ),
            UaiCompoundOralSubmission::fixture(
                draft_id,
                "right",
                json!(["spoken"]),
                Some(json!({"slot": 2})),
            ),
        ] {
            let foreign_sequence = UaiCompoundOralSubmissionSequence::try_new(&foreign).unwrap();
            assert!(
                UaiCompoundOralPlanState::decode_bound(
                    &value,
                    digest,
                    &foreign_sequence,
                    request.request_digest(),
                )
                .is_err()
            );
        }
    }
}
