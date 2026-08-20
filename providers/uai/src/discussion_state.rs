use std::fmt;

use asterism_domain::ProviderId;
use asterism_provider_api::{
    ExecutionMutationRecoveryRecord, ExecutionMutationStageOutput, ProviderError,
    ProviderErrorKind, ProviderResult,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    UAI_DISCUSSION_REPLY_OPERATION_TYPE, UaiDiscussionBinding, UaiDiscussionMutationSequence,
    UaiDiscussionReplyDraft, metadata::PROVIDER_ID,
};

pub const UAI_DISCUSSION_REPLY_STATE_TYPE: &str = "uai.discussion.reply-state.v1";

const MAX_DISCUSSION_REPLY_STATE_BYTES: usize = 64 * 1_024;

/// Bounded encrypted Provider continuation for one immutable discussion reply
/// intent. The compact Core artifact continues to carry hashes only.
pub struct EncodedUaiDiscussionReplyState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiDiscussionReplyState {
    /// Encodes the exact reply content and complete fresh route/account
    /// binding against the independently projected Core sequence.
    ///
    /// # Errors
    ///
    /// Rejects a foreign sequence or unrepresentable bounded state.
    pub fn try_new(
        draft: &UaiDiscussionReplyDraft,
        sequence: &UaiDiscussionMutationSequence,
    ) -> ProviderResult<Self> {
        let actual = UaiDiscussionMutationSequence::try_new(draft)?;
        if actual.artifact().artifact_digest() != sequence.artifact().artifact_digest()
            || actual.plan().plan_digest() != sequence.plan().plan_digest()
        {
            return Err(foreign_reply_state());
        }
        let binding = draft.binding();
        let wire = DiscussionReplyStateWireRef {
            schema: UAI_DISCUSSION_REPLY_STATE_TYPE,
            course_resource_id: binding.course_resource_id(),
            course_instance_id: binding.course_instance_id(),
            group_id: binding.group_id(),
            class_id: binding.class_id(),
            curricula_id: binding.curricula_id(),
            current_user_id: binding.current_user_id(),
            topic_id: draft.topic_id(),
            content: draft.expose_content(),
            request_digest: draft.request_digest(),
            artifact_digest: sequence.artifact().artifact_digest(),
            sequence_plan_digest: sequence.plan().plan_digest(),
        };
        let mut encoded =
            Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| invalid_reply_state())?);
        if encoded.is_empty() || encoded.len() > MAX_DISCUSSION_REPLY_STATE_BYTES {
            return Err(invalid_reply_state());
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

    pub(crate) fn into_stage_output(self) -> ProviderResult<ExecutionMutationStageOutput> {
        ExecutionMutationStageOutput::try_new(
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_reply_state())?,
            1,
            UAI_DISCUSSION_REPLY_STATE_TYPE,
            self.digest,
            self.value,
        )
    }
}

impl fmt::Debug for EncodedUaiDiscussionReplyState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiDiscussionReplyState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Decoded immutable reply intent rebound to its compact artifact and exact
/// two-phase sequence.
pub struct UaiDiscussionReplyState {
    draft: UaiDiscussionReplyDraft,
}

impl UaiDiscussionReplyState {
    /// Restores the accepted reply continuation only from Core's atomic
    /// receipt-plus-stage-output record.
    ///
    /// # Errors
    ///
    /// Rejects missing output and every Provider, type, ordinal, operation,
    /// request, response or sequence substitution.
    pub fn decode_recovery_record(
        sequence: &UaiDiscussionMutationSequence,
        record: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<Self> {
        let receipt = record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_reply_state)?;
        let output = record.stage_output().ok_or_else(foreign_reply_state)?;
        if record.issue().ordinal() != 1
            || receipt.ordinal() != 1
            || record.issue().operation_type() != UAI_DISCUSSION_REPLY_OPERATION_TYPE
            || output.provider_id().as_str() != PROVIDER_ID
            || output.ordinal() != 1
            || output.output_type() != UAI_DISCUSSION_REPLY_STATE_TYPE
        {
            return Err(foreign_reply_state());
        }
        let state = Self::decode_bound(output.value(), output.output_digest(), sequence)?;
        if state.draft().request_digest() != record.issue().request_digest()
            || receipt.response_digest() == [0; 32]
        {
            return Err(foreign_reply_state());
        }
        Ok(state)
    }

    /// Decodes the private reply only when state, artifact, sequence and full
    /// request identity all reproduce independently.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, digest-mismatched or substituted reply
    /// state before exposing its content.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        sequence: &UaiDiscussionMutationSequence,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty()
            || bytes.len() > MAX_DISCUSSION_REPLY_STATE_BYTES
            || <[u8; 32]>::from(Sha256::digest(bytes)) != expected_digest
        {
            return Err(foreign_reply_state());
        }
        let mut wire: DiscussionReplyStateWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_reply_state())?;
        if wire.schema != UAI_DISCUSSION_REPLY_STATE_TYPE
            || wire.artifact_digest != sequence.artifact().artifact_digest()
            || wire.sequence_plan_digest != sequence.plan().plan_digest()
            || [
                wire.request_digest,
                wire.artifact_digest,
                wire.sequence_plan_digest,
            ]
            .contains(&[0; 32])
        {
            return Err(foreign_reply_state());
        }
        let binding = UaiDiscussionBinding::try_new(
            &wire.course_resource_id,
            &wire.course_instance_id,
            &wire.group_id,
            &wire.class_id,
            &wire.curricula_id,
            &wire.current_user_id,
        )?;
        let content = Zeroizing::new(std::mem::take(&mut wire.content));
        let draft = UaiDiscussionReplyDraft::try_new(binding, wire.topic_id, content.as_str())?;
        let actual = UaiDiscussionMutationSequence::try_new(&draft)?;
        if draft.request_digest() != wire.request_digest
            || actual.artifact().artifact_digest() != sequence.artifact().artifact_digest()
            || actual.plan().plan_digest() != sequence.plan().plan_digest()
        {
            return Err(foreign_reply_state());
        }
        Ok(Self { draft })
    }

    pub const fn draft(&self) -> &UaiDiscussionReplyDraft {
        &self.draft
    }

    pub(crate) fn same_recovery_authority(&self, other: &Self) -> bool {
        self.draft.binding() == other.draft.binding()
            && self.draft.topic_id() == other.draft.topic_id()
            && self.draft.request_digest() == other.draft.request_digest()
            && self.draft.expose_content() == other.draft.expose_content()
    }
}

impl fmt::Debug for UaiDiscussionReplyState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionReplyState")
            .field("draft", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize)]
struct DiscussionReplyStateWireRef<'a> {
    schema: &'static str,
    course_resource_id: &'a str,
    course_instance_id: &'a str,
    group_id: &'a str,
    class_id: &'a str,
    curricula_id: &'a str,
    current_user_id: &'a str,
    topic_id: u64,
    content: &'a str,
    request_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct DiscussionReplyStateWire {
    schema: String,
    course_resource_id: String,
    course_instance_id: String,
    group_id: String,
    class_id: String,
    curricula_id: String,
    current_user_id: String,
    topic_id: u64,
    content: String,
    request_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

fn invalid_reply_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI discussion private reply state is invalid",
    )
}

fn foreign_reply_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI discussion private reply state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_reply_round_trips_only_against_exact_sequence() {
        let draft = reply_draft("A bounded synthetic reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let encoded = EncodedUaiDiscussionReplyState::try_new(&draft, &sequence).unwrap();
        let digest = encoded.digest();
        assert!(!format!("{encoded:?}").contains("synthetic reply"));
        let value = encoded.into_secret_value();
        let decoded = UaiDiscussionReplyState::decode_bound(&value, digest, &sequence).unwrap();
        assert_eq!(
            decoded.draft().expose_content(),
            "A bounded synthetic reply"
        );
        assert_eq!(decoded.draft().request_digest(), draft.request_digest());
        assert!(!format!("{decoded:?}").contains("synthetic reply"));
        assert!(UaiDiscussionReplyState::decode_bound(&value, [7; 32], &sequence).is_err());

        let foreign = reply_draft("A different bounded reply");
        let foreign_sequence = UaiDiscussionMutationSequence::try_new(&foreign).unwrap();
        assert!(UaiDiscussionReplyState::decode_bound(&value, digest, &foreign_sequence).is_err());
        assert!(EncodedUaiDiscussionReplyState::try_new(&draft, &foreign_sequence).is_err());

        let record = ExecutionMutationRecoveryRecord::try_new(
            asterism_provider_api::ExecutionMutationIssue::new(
                1,
                UAI_DISCUSSION_REPLY_OPERATION_TYPE,
                draft.request_digest(),
            )
            .unwrap(),
            Some(asterism_provider_api::ExecutionMutationReceipt::new(1, [4; 32], true).unwrap()),
            None,
        )
        .unwrap();
        assert!(UaiDiscussionReplyState::decode_recovery_record(&sequence, &record).is_err());
        let recovered = record
            .clone()
            .try_with_stage_output(
                EncodedUaiDiscussionReplyState::try_new(&draft, &sequence)
                    .unwrap()
                    .into_stage_output()
                    .unwrap(),
            )
            .unwrap();
        assert!(
            UaiDiscussionReplyState::decode_recovery_record(&sequence, &recovered)
                .unwrap()
                .same_recovery_authority(&decoded)
        );
        let foreign_encoded = EncodedUaiDiscussionReplyState::try_new(&draft, &sequence).unwrap();
        let foreign = record
            .try_with_stage_output(
                ExecutionMutationStageOutput::try_new(
                    ProviderId::new("foreign").unwrap(),
                    1,
                    "foreign.discussion.reply-state.v1",
                    foreign_encoded.digest(),
                    foreign_encoded.into_secret_value(),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(UaiDiscussionReplyState::decode_recovery_record(&sequence, &foreign).is_err());
    }

    fn reply_draft(content: &str) -> UaiDiscussionReplyDraft {
        UaiDiscussionReplyDraft::try_new(
            UaiDiscussionBinding::try_new(
                "2001",
                "course-instance-1",
                "group-discussion",
                "class-1",
                "curricula-1",
                "user-1",
            )
            .unwrap(),
            7_001,
            content,
        )
        .unwrap()
    }
}
