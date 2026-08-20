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
    UAI_DISCUSSION_COMPLETION_OPERATION_TYPE, UaiDiscussionCompletionPlan,
    UaiDiscussionMutationSequence, UaiDiscussionReplyState,
    discussion::{discussion_completion_request_digest, discussion_reply_digest},
    metadata::PROVIDER_ID,
};

pub const UAI_DISCUSSION_COMPLETION_STATE_TYPE: &str = "uai.discussion.completion-state.v1";

const MAX_DISCUSSION_COMPLETION_STATE_BYTES: usize = 64 * 1_024;

/// Bounded encrypted continuation for the dynamically derived second
/// discussion mutation. It contains no reply text and cannot replace the
/// independently recovered reply Draft or Core sequence evidence.
pub struct EncodedUaiDiscussionCompletionState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiDiscussionCompletionState {
    /// Encodes the exact completion plan only after rebinding it to the
    /// recovered reply state and independently projected sequence.
    ///
    /// # Errors
    ///
    /// Rejects any foreign reply, task or sequence material.
    pub fn try_new(
        completion: &UaiDiscussionCompletionPlan,
        reply: &UaiDiscussionReplyState,
        sequence: &UaiDiscussionMutationSequence,
    ) -> ProviderResult<Self> {
        validate_completion(completion, reply, sequence)?;
        let wire = DiscussionCompletionStateWireRef {
            schema: UAI_DISCUSSION_COMPLETION_STATE_TYPE,
            remote_task_id: completion.remote_task_id(),
            task_fingerprint: completion.task_fingerprint(),
            course_resource_id: completion.course_resource_id(),
            unit_id: completion.unit_id(),
            group_id: completion.group_id(),
            topic_id: completion.topic_id(),
            reply_request_digest: completion.reply_request_digest(),
            reply_digest: completion.reply_digest(),
            request_digest: completion.request_digest(),
            fingerprint: completion.fingerprint(),
            reply_state_request_digest: reply.draft().request_digest(),
            artifact_digest: sequence.artifact().artifact_digest(),
            sequence_plan_digest: sequence.plan().plan_digest(),
        };
        let mut encoded =
            Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| invalid_completion_state())?);
        if encoded.is_empty() || encoded.len() > MAX_DISCUSSION_COMPLETION_STATE_BYTES {
            return Err(invalid_completion_state());
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
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_completion_state())?,
            2,
            UAI_DISCUSSION_COMPLETION_STATE_TYPE,
            self.digest,
            self.value,
        )
    }
}

impl fmt::Debug for EncodedUaiDiscussionCompletionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiDiscussionCompletionState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Decoded second-mutation plan rebound to the exact reply state and Core
/// sequence.
pub struct UaiDiscussionCompletionState {
    completion: UaiDiscussionCompletionPlan,
}

impl UaiDiscussionCompletionState {
    /// Restores the accepted completion continuation only from Core's atomic
    /// receipt-plus-stage-output record.
    ///
    /// # Errors
    ///
    /// Rejects missing output and every Provider, type, ordinal, operation,
    /// request, response, reply or sequence substitution.
    pub fn decode_recovery_record(
        sequence: &UaiDiscussionMutationSequence,
        reply: &UaiDiscussionReplyState,
        record: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<Self> {
        let receipt = record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_completion_state)?;
        let output = record.stage_output().ok_or_else(foreign_completion_state)?;
        if record.issue().ordinal() != 2
            || receipt.ordinal() != 2
            || record.issue().operation_type() != UAI_DISCUSSION_COMPLETION_OPERATION_TYPE
            || record.verification().is_some()
            || output.provider_id().as_str() != PROVIDER_ID
            || output.ordinal() != 2
            || output.output_type() != UAI_DISCUSSION_COMPLETION_STATE_TYPE
        {
            return Err(foreign_completion_state());
        }
        let state = Self::decode_bound(output.value(), output.output_digest(), reply, sequence)?;
        if state.completion().request_digest() != record.issue().request_digest()
            || receipt.response_digest() == [0; 32]
        {
            return Err(foreign_completion_state());
        }
        Ok(state)
    }

    /// Decodes only the exact dynamic completion plan derived from the
    /// independently recovered immutable reply.
    ///
    /// # Errors
    ///
    /// Rejects malformed, digest-mismatched, reply-substituted, task-
    /// substituted or sequence-substituted state.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        reply: &UaiDiscussionReplyState,
        sequence: &UaiDiscussionMutationSequence,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty()
            || bytes.len() > MAX_DISCUSSION_COMPLETION_STATE_BYTES
            || <[u8; 32]>::from(Sha256::digest(bytes)) != expected_digest
        {
            return Err(foreign_completion_state());
        }
        let mut wire: DiscussionCompletionStateWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_completion_state())?;
        if wire.schema != UAI_DISCUSSION_COMPLETION_STATE_TYPE
            || wire.reply_state_request_digest != reply.draft().request_digest()
            || wire.artifact_digest != sequence.artifact().artifact_digest()
            || wire.sequence_plan_digest != sequence.plan().plan_digest()
            || [
                wire.reply_state_request_digest,
                wire.artifact_digest,
                wire.sequence_plan_digest,
            ]
            .contains(&[0; 32])
        {
            return Err(foreign_completion_state());
        }
        let completion = UaiDiscussionCompletionPlan::restore_state(
            std::mem::take(&mut wire.remote_task_id),
            std::mem::take(&mut wire.task_fingerprint),
            std::mem::take(&mut wire.course_resource_id),
            std::mem::take(&mut wire.unit_id),
            std::mem::take(&mut wire.group_id),
            wire.topic_id,
            wire.reply_request_digest,
            wire.reply_digest,
            wire.request_digest,
            std::mem::take(&mut wire.fingerprint),
        )?;
        validate_completion(&completion, reply, sequence)?;
        Ok(Self { completion })
    }

    pub const fn completion(&self) -> &UaiDiscussionCompletionPlan {
        &self.completion
    }

    pub(crate) fn same_recovery_authority(&self, other: &Self) -> bool {
        let left = self.completion();
        let right = other.completion();
        left.remote_task_id() == right.remote_task_id()
            && left.task_fingerprint() == right.task_fingerprint()
            && left.course_resource_id() == right.course_resource_id()
            && left.unit_id() == right.unit_id()
            && left.group_id() == right.group_id()
            && left.topic_id() == right.topic_id()
            && left.reply_request_digest() == right.reply_request_digest()
            && left.reply_digest() == right.reply_digest()
            && left.request_digest() == right.request_digest()
            && left.fingerprint() == right.fingerprint()
    }
}

impl fmt::Debug for UaiDiscussionCompletionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionCompletionState")
            .field("completion", &"[REDACTED]")
            .finish()
    }
}

fn validate_completion(
    completion: &UaiDiscussionCompletionPlan,
    reply: &UaiDiscussionReplyState,
    sequence: &UaiDiscussionMutationSequence,
) -> ProviderResult<()> {
    let draft = reply.draft();
    let expected_sequence = UaiDiscussionMutationSequence::try_new(draft)?;
    let expected_reply_digest = discussion_reply_digest(draft);
    let expected_request_digest = discussion_completion_request_digest(
        completion.remote_task_id(),
        completion.task_fingerprint(),
        completion.course_resource_id(),
        completion.unit_id(),
        completion.group_id(),
        completion.topic_id(),
        draft.request_digest(),
        expected_reply_digest,
    );
    if expected_sequence.artifact().artifact_digest() != sequence.artifact().artifact_digest()
        || expected_sequence.plan().plan_digest() != sequence.plan().plan_digest()
        || completion.course_resource_id() != draft.binding().course_resource_id()
        || completion.group_id() != draft.binding().group_id()
        || completion.topic_id() != draft.topic_id()
        || completion.reply_request_digest() != draft.request_digest()
        || completion.reply_digest() != expected_reply_digest
        || completion.request_digest() != expected_request_digest
    {
        Err(foreign_completion_state())
    } else {
        Ok(())
    }
}

#[derive(Serialize)]
struct DiscussionCompletionStateWireRef<'a> {
    schema: &'static str,
    remote_task_id: &'a str,
    task_fingerprint: &'a str,
    course_resource_id: &'a str,
    unit_id: &'a str,
    group_id: &'a str,
    topic_id: u64,
    reply_request_digest: [u8; 32],
    reply_digest: [u8; 32],
    request_digest: [u8; 32],
    fingerprint: &'a str,
    reply_state_request_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct DiscussionCompletionStateWire {
    schema: String,
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    topic_id: u64,
    reply_request_digest: [u8; 32],
    reply_digest: [u8; 32],
    request_digest: [u8; 32],
    fingerprint: String,
    reply_state_request_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
}

fn invalid_completion_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI discussion completion state is invalid",
    )
}

fn foreign_completion_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI discussion completion state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EncodedUaiDiscussionReplyState, UaiDiscussionBinding, UaiDiscussionReplyDraft,
        UaiDiscussionReplyState,
    };

    #[test]
    fn completion_state_round_trips_only_with_its_exact_reply_and_sequence() {
        let (reply, sequence) = reply_state("A bounded synthetic reply");
        let completion = completion_plan(reply.draft());
        let encoded =
            EncodedUaiDiscussionCompletionState::try_new(&completion, &reply, &sequence).unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let decoded =
            UaiDiscussionCompletionState::decode_bound(&value, digest, &reply, &sequence).unwrap();
        assert_eq!(
            decoded.completion().request_digest(),
            completion.request_digest()
        );
        assert!(!format!("{decoded:?}").contains("synthetic reply"));
        assert!(
            UaiDiscussionCompletionState::decode_bound(&value, [7; 32], &reply, &sequence).is_err()
        );

        let (foreign_reply, foreign_sequence) = reply_state("A different bounded reply");
        assert!(
            UaiDiscussionCompletionState::decode_bound(
                &value,
                digest,
                &foreign_reply,
                &foreign_sequence,
            )
            .is_err()
        );
        assert!(
            EncodedUaiDiscussionCompletionState::try_new(
                &completion,
                &foreign_reply,
                &foreign_sequence,
            )
            .is_err()
        );
    }

    fn reply_state(content: &str) -> (UaiDiscussionReplyState, UaiDiscussionMutationSequence) {
        let draft = UaiDiscussionReplyDraft::try_new(
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
        .unwrap();
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let encoded = EncodedUaiDiscussionReplyState::try_new(&draft, &sequence).unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let reply = UaiDiscussionReplyState::decode_bound(&value, digest, &sequence).unwrap();
        (reply, sequence)
    }

    fn completion_plan(draft: &UaiDiscussionReplyDraft) -> UaiDiscussionCompletionPlan {
        let remote_task_id = "group:2001:unit-1:group-discussion";
        let task_fingerprint = "v1:discussion";
        let reply_digest = discussion_reply_digest(draft);
        let request_digest = discussion_completion_request_digest(
            remote_task_id,
            task_fingerprint,
            "2001",
            "unit-1",
            "group-discussion",
            draft.topic_id(),
            draft.request_digest(),
            reply_digest,
        );
        UaiDiscussionCompletionPlan::restore_state(
            remote_task_id.to_owned(),
            task_fingerprint.to_owned(),
            "2001".to_owned(),
            "unit-1".to_owned(),
            "group-discussion".to_owned(),
            draft.topic_id(),
            draft.request_digest(),
            reply_digest,
            request_digest,
            format!("uai-discussion-complete-v1:{}", hex_digest(request_digest)),
        )
        .unwrap()
    }

    fn hex_digest(digest: [u8; 32]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(64);
        for byte in digest {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}
