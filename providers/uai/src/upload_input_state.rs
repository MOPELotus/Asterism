use std::{fmt, str::FromStr};

use asterism_domain::{NormalizedAnswer, QuestionKind, SubmissionDraft, SubmissionDraftId};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{UaiUploadArtifact, upload::MAX_UPLOAD_ARTIFACT_BYTES};

pub const UAI_UPLOAD_INPUT_STATE_TYPE: &str = "uai.upload.input.v1";

const INPUT_STATE_MAGIC: &[u8] = b"uai.upload.input.v1\0";
const SINGLE_INPUT_KIND: u8 = 1;
const COMPOUND_INPUT_KIND: u8 = 2;
const MAX_UPLOAD_INPUT_STATE_BYTES: usize = MAX_UPLOAD_ARTIFACT_BYTES + 2 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_FILENAME_BYTES: usize = 128;
const MAX_MEDIA_TYPE_BYTES: usize = 64;
const MAX_ARTIFACT_DIGEST_BYTES: usize = 80;
const MAX_DRAFT_ID_BYTES: usize = 64;

/// Bounded zeroizing bytes for one exact upload artifact and its immutable
/// single/compound scheduling binding.
pub struct EncodedUaiUploadInputState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiUploadInputState {
    /// Encodes an exact artifact for one single-upload remote Task.
    ///
    /// # Errors
    ///
    /// Rejects malformed Task identity or state exceeding the input bound.
    pub fn for_single(remote_task_id: &str, artifact: &UaiUploadArtifact) -> ProviderResult<Self> {
        encode_input(remote_task_id, artifact, None)
    }

    /// Encodes an exact artifact together with the independently durable
    /// ordinary sub-Draft required by the audited compound upload.
    ///
    /// # Errors
    ///
    /// Rejects malformed Task identity, a foreign/non-choice Draft or state
    /// exceeding the input bound.
    pub fn for_compound(
        remote_task_id: &str,
        artifact: &UaiUploadArtifact,
        ordinary_draft: &SubmissionDraft,
    ) -> ProviderResult<Self> {
        let draft_digest = compound_draft_digest(ordinary_draft)?;
        encode_input(
            remote_task_id,
            artifact,
            Some((ordinary_draft.id, draft_digest)),
        )
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiUploadInputState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiUploadInputState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Exact decoded input artifact, rebound to a single Task or immutable
/// compound ordinary Draft before any fresh upload-intent preparation.
pub struct UaiUploadInputState {
    remote_task_id: String,
    artifact: UaiUploadArtifact,
    ordinary_draft_id: Option<SubmissionDraftId>,
    ordinary_draft_digest: Option<[u8; 32]>,
}

impl UaiUploadInputState {
    /// Decodes a single-upload input against independently persisted state,
    /// remote Task and artifact digests.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, compound, digest-mismatched or foreign
    /// input before exposing artifact bytes.
    pub fn decode_single_bound(
        value: &SecretValue,
        expected_state_digest: [u8; 32],
        expected_remote_task_id: &str,
        expected_artifact_digest: &str,
    ) -> ProviderResult<Self> {
        decode_input(
            value,
            expected_state_digest,
            expected_remote_task_id,
            expected_artifact_digest,
            None,
        )
    }

    /// Decodes a compound-upload input against the independently persisted
    /// immutable ordinary sub-Draft as well as state/Task/artifact digests.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, single, digest-mismatched or foreign
    /// input before exposing artifact bytes.
    pub fn decode_compound_bound(
        value: &SecretValue,
        expected_state_digest: [u8; 32],
        expected_remote_task_id: &str,
        expected_artifact_digest: &str,
        expected_ordinary_draft: &SubmissionDraft,
    ) -> ProviderResult<Self> {
        let expected_draft_digest = compound_draft_digest(expected_ordinary_draft)?;
        decode_input(
            value,
            expected_state_digest,
            expected_remote_task_id,
            expected_artifact_digest,
            Some((expected_ordinary_draft.id, expected_draft_digest)),
        )
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub const fn artifact(&self) -> &UaiUploadArtifact {
        &self.artifact
    }

    pub const fn ordinary_draft_id(&self) -> Option<SubmissionDraftId> {
        self.ordinary_draft_id
    }

    pub const fn ordinary_draft_digest(&self) -> Option<[u8; 32]> {
        self.ordinary_draft_digest
    }

    pub(crate) const fn is_compound(&self) -> bool {
        self.ordinary_draft_id.is_some()
    }

    pub(crate) fn validate_compound_draft(
        &self,
        expected_ordinary_draft: &SubmissionDraft,
    ) -> ProviderResult<()> {
        let expected_digest = compound_draft_digest(expected_ordinary_draft)?;
        if self.ordinary_draft_id == Some(expected_ordinary_draft.id)
            && self.ordinary_draft_digest == Some(expected_digest)
        {
            Ok(())
        } else {
            Err(foreign_input_state())
        }
    }
}

impl fmt::Debug for UaiUploadInputState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadInputState")
            .field("remote_task_id", &"[BOUND]")
            .field("artifact", &"[REDACTED]")
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("ordinary_draft_digest", &"[HASHED]")
            .finish()
    }
}

fn encode_input(
    remote_task_id: &str,
    artifact: &UaiUploadArtifact,
    ordinary_draft: Option<(SubmissionDraftId, [u8; 32])>,
) -> ProviderResult<EncodedUaiUploadInputState> {
    if !valid_remote_task_id(remote_task_id) {
        return Err(invalid_input_state());
    }
    let artifact_digest = Zeroizing::new(artifact.digest());
    let mut encoded = Zeroizing::new(Vec::with_capacity(
        artifact.expose_bytes().len().saturating_add(512),
    ));
    encoded.extend_from_slice(INPUT_STATE_MAGIC);
    encoded.push(if ordinary_draft.is_some() {
        COMPOUND_INPUT_KIND
    } else {
        SINGLE_INPUT_KIND
    });
    push_string(&mut encoded, remote_task_id)?;
    push_string(&mut encoded, artifact.filename())?;
    push_string(&mut encoded, artifact.media_type())?;
    push_string(&mut encoded, artifact_digest.as_str())?;
    if let Some((draft_id, draft_digest)) = ordinary_draft {
        let draft_id = Zeroizing::new(draft_id.to_string());
        push_string(&mut encoded, draft_id.as_str())?;
        encoded.extend_from_slice(&draft_digest);
    }
    let artifact_length =
        u32::try_from(artifact.expose_bytes().len()).map_err(|_| invalid_input_state())?;
    encoded.extend_from_slice(&artifact_length.to_be_bytes());
    encoded.extend_from_slice(artifact.expose_bytes());
    if encoded.len() > MAX_UPLOAD_INPUT_STATE_BYTES {
        return Err(invalid_input_state());
    }
    let digest = Sha256::digest(encoded.as_slice()).into();
    let value = SecretValue::new(std::mem::take(&mut *encoded));
    Ok(EncodedUaiUploadInputState { value, digest })
}

fn decode_input(
    value: &SecretValue,
    expected_state_digest: [u8; 32],
    expected_remote_task_id: &str,
    expected_artifact_digest: &str,
    expected_ordinary_draft: Option<(SubmissionDraftId, [u8; 32])>,
) -> ProviderResult<UaiUploadInputState> {
    let bytes = value.expose_secret();
    if bytes.len() <= INPUT_STATE_MAGIC.len() || bytes.len() > MAX_UPLOAD_INPUT_STATE_BYTES {
        return Err(foreign_input_state());
    }
    if <[u8; 32]>::from(Sha256::digest(bytes)) != expected_state_digest
        || !valid_remote_task_id(expected_remote_task_id)
        || !valid_artifact_digest(expected_artifact_digest)
    {
        return Err(foreign_input_state());
    }
    let mut reader = InputStateReader::new(bytes);
    if reader.take(INPUT_STATE_MAGIC.len())? != INPUT_STATE_MAGIC {
        return Err(foreign_input_state());
    }
    let kind = reader.read_u8()?;
    let remote_task_id = reader.read_string(MAX_REMOTE_TASK_ID_BYTES)?;
    if remote_task_id != expected_remote_task_id || !valid_remote_task_id(&remote_task_id) {
        return Err(foreign_input_state());
    }
    let mut filename = Zeroizing::new(reader.read_string(MAX_FILENAME_BYTES)?);
    let mut media_type = Zeroizing::new(reader.read_string(MAX_MEDIA_TYPE_BYTES)?);
    let artifact_digest = Zeroizing::new(reader.read_string(MAX_ARTIFACT_DIGEST_BYTES)?);
    if artifact_digest.as_str() != expected_artifact_digest
        || !valid_artifact_digest(artifact_digest.as_str())
    {
        return Err(foreign_input_state());
    }
    let (ordinary_draft_id, ordinary_draft_digest) = match (kind, expected_ordinary_draft) {
        (SINGLE_INPUT_KIND, None) => (None, None),
        (COMPOUND_INPUT_KIND, Some((expected_draft_id, expected_draft_digest))) => {
            let draft_id = reader.read_string(MAX_DRAFT_ID_BYTES)?;
            let draft_id =
                SubmissionDraftId::from_str(&draft_id).map_err(|_| foreign_input_state())?;
            let draft_digest = reader.read_array_32()?;
            if draft_id != expected_draft_id || draft_digest != expected_draft_digest {
                return Err(foreign_input_state());
            }
            (Some(draft_id), Some(draft_digest))
        }
        _ => return Err(foreign_input_state()),
    };
    let artifact_length = usize::try_from(reader.read_u32()?).map_err(|_| foreign_input_state())?;
    if artifact_length == 0 || artifact_length > MAX_UPLOAD_ARTIFACT_BYTES {
        return Err(foreign_input_state());
    }
    let artifact_bytes = Zeroizing::new(reader.take(artifact_length)?.to_vec());
    if !reader.is_finished() {
        return Err(foreign_input_state());
    }
    let artifact = UaiUploadArtifact::try_new_zeroizing(
        std::mem::take(&mut *filename),
        std::mem::take(&mut *media_type),
        artifact_bytes,
    )
    .map_err(|_| foreign_input_state())?;
    if artifact.digest() != expected_artifact_digest {
        return Err(foreign_input_state());
    }
    Ok(UaiUploadInputState {
        remote_task_id,
        artifact,
        ordinary_draft_id,
        ordinary_draft_digest,
    })
}

fn compound_draft_digest(draft: &SubmissionDraft) -> ProviderResult<[u8; 32]> {
    if draft.validate().is_err()
        || draft.provider_id.as_str() != "uai"
        || draft.provider_version != env!("CARGO_PKG_VERSION")
        || draft.items.len() != 1
        || draft.items[0].question.kind != QuestionKind::MultipleChoice
        || !matches!(
            &draft.items[0].selected.answer,
            NormalizedAnswer::Selections(_)
        )
    {
        return Err(invalid_input_state());
    }
    let encoded = Zeroizing::new(serde_json::to_vec(draft).map_err(|_| invalid_input_state())?);
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-input-compound-draft:v1\0");
    digest.update(encoded.as_slice());
    Ok(digest.finalize().into())
}

fn push_string(encoded: &mut Vec<u8>, value: &str) -> ProviderResult<()> {
    let length = u16::try_from(value.len()).map_err(|_| invalid_input_state())?;
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(value.as_bytes());
    Ok(())
}

struct InputStateReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> InputStateReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> ProviderResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(foreign_input_state)?;
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn read_u8(&mut self) -> ProviderResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn read_u16(&mut self) -> ProviderResult<u16> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| foreign_input_state())?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> ProviderResult<u32> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| foreign_input_state())?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_array_32(&mut self) -> ProviderResult<[u8; 32]> {
        self.take(32)?.try_into().map_err(|_| foreign_input_state())
    }

    fn read_string(&mut self, maximum: usize) -> ProviderResult<String> {
        let length = usize::from(self.read_u16()?);
        if length == 0 || length > maximum {
            return Err(foreign_input_state());
        }
        std::str::from_utf8(self.take(length)?)
            .map(str::to_owned)
            .map_err(|_| foreign_input_state())
    }

    const fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }
}

fn valid_remote_task_id(value: &str) -> bool {
    let mut parts = value.split(':');
    matches!(
        (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ),
        (Some("group"), Some(course), Some(unit), Some(group), None)
            if valid_remote_component(course)
                && valid_remote_component(unit)
                && valid_remote_component(group)
    )
}

fn valid_remote_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_artifact_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn invalid_input_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI upload input state is invalid",
    )
}

fn foreign_input_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI upload input state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderId, Question, QuestionId, QuestionOption,
        QuestionSnapshotId, SelectedAnswer, SubmissionAnswerCoverage, SubmissionDraftItem,
        SubmissionPayloadEncoding, SubmissionPayloadFieldPreview, SubmissionPayloadPreview, TaskId,
    };
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    const REMOTE_TASK_ID: &str = "group:2001:unit-1:group-upload";

    #[test]
    fn single_input_round_trips_only_against_exact_task_and_artifact() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let artifact_digest = artifact.digest();
        let encoded = EncodedUaiUploadInputState::for_single(REMOTE_TASK_ID, &artifact).unwrap();
        assert!(!format!("{encoded:?}").contains("nothing.mp3"));
        let state_digest = encoded.digest();
        let value = encoded.into_secret_value();
        let state = UaiUploadInputState::decode_single_bound(
            &value,
            state_digest,
            REMOTE_TASK_ID,
            &artifact_digest,
        )
        .unwrap();
        assert_eq!(state.remote_task_id(), REMOTE_TASK_ID);
        assert_eq!(state.artifact().digest(), artifact_digest);
        assert_eq!(state.artifact().expose_bytes(), artifact.expose_bytes());
        assert_eq!(state.ordinary_draft_id(), None);
        assert!(!format!("{state:?}").contains("nothing.mp3"));

        assert!(
            UaiUploadInputState::decode_single_bound(
                &value,
                [7; 32],
                REMOTE_TASK_ID,
                &artifact_digest,
            )
            .is_err()
        );
        assert!(
            UaiUploadInputState::decode_single_bound(
                &value,
                state_digest,
                "group:2001:unit-1:other",
                &artifact_digest,
            )
            .is_err()
        );
        let foreign_artifact =
            UaiUploadArtifact::try_new("other.mp3", "audio/mpeg", vec![1; 4_096]).unwrap();
        assert!(
            UaiUploadInputState::decode_single_bound(
                &value,
                state_digest,
                REMOTE_TASK_ID,
                &foreign_artifact.digest(),
            )
            .is_err()
        );
        assert!(
            UaiUploadInputState::decode_compound_bound(
                &value,
                state_digest,
                REMOTE_TASK_ID,
                &artifact_digest,
                &ordinary_draft(),
            )
            .is_err()
        );
    }

    #[test]
    fn compound_input_rejects_same_id_draft_answer_substitution() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let artifact_digest = artifact.digest();
        let draft = ordinary_draft();
        let encoded =
            EncodedUaiUploadInputState::for_compound(REMOTE_TASK_ID, &artifact, &draft).unwrap();
        let state_digest = encoded.digest();
        let value = encoded.into_secret_value();
        let state = UaiUploadInputState::decode_compound_bound(
            &value,
            state_digest,
            REMOTE_TASK_ID,
            &artifact_digest,
            &draft,
        )
        .unwrap();
        assert_eq!(state.ordinary_draft_id(), Some(draft.id));
        assert!(state.ordinary_draft_digest().is_some());

        let mut changed = draft.clone();
        changed.items[0].selected.answer = NormalizedAnswer::Selections(vec!["B".to_owned()]);
        changed.validate().unwrap();
        assert_eq!(changed.id, draft.id);
        assert!(
            UaiUploadInputState::decode_compound_bound(
                &value,
                state_digest,
                REMOTE_TASK_ID,
                &artifact_digest,
                &changed,
            )
            .is_err()
        );
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
            created_at: Utc::now(),
        };
        draft.validate().unwrap();
        draft
    }
}
