use std::fmt;

use asterism_domain::TaskId;
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{CidarenAttemptProgress, ParsedCidarenReadingCard};

pub const CIDAREN_PRE_QUESTION_ARTIFACT_TYPE: &str = "cidaren.pre-question-attempt.v2";
pub const CIDAREN_READY_TO_SELECT_WORDS_PHASE: &str = "cidaren.ready-to-select-words";
pub const CIDAREN_READY_TO_START_PHASE: &str = "cidaren.ready-to-start";
pub const CIDAREN_READING_CARD_PHASE: &str = "cidaren.reading-card";

const MAX_ARTIFACT_BYTES: usize = 128 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 768;
const MAX_POSITION: u32 = 100_000;

pub struct EncodedCidarenPreQuestionArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedCidarenPreQuestionArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedCidarenPreQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedCidarenPreQuestionArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

pub struct CidarenPreQuestionArtifact {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    state: CidarenPreQuestionState,
}

pub(crate) enum CidarenPreQuestionState {
    ReadyToSelectWords(u32),
    ReadyToStart(u32),
    ReadingCard(ParsedCidarenReadingCard),
}

impl CidarenPreQuestionArtifact {
    pub(crate) fn ready_to_select_words(
        task_id: TaskId,
        remote_task_id: &str,
        position: u32,
    ) -> Self {
        Self::new(
            task_id,
            remote_task_id,
            CidarenPreQuestionState::ReadyToSelectWords(position),
        )
    }

    pub(crate) fn ready_to_start(task_id: TaskId, remote_task_id: &str, position: u32) -> Self {
        Self::new(
            task_id,
            remote_task_id,
            CidarenPreQuestionState::ReadyToStart(position),
        )
    }

    pub(crate) fn reading_card(
        task_id: TaskId,
        remote_task_id: &str,
        card: &ParsedCidarenReadingCard,
    ) -> ProviderResult<Self> {
        let restored = ParsedCidarenReadingCard::from_artifact(
            card.topic_code().to_owned(),
            card.remote_id().to_owned(),
            card.stem_sanitized().to_owned(),
            card.position(),
            card.remote_progress(),
        )?;
        Ok(Self::new(
            task_id,
            remote_task_id,
            CidarenPreQuestionState::ReadingCard(restored),
        ))
    }

    fn new(task_id: TaskId, remote_task_id: &str, state: CidarenPreQuestionState) -> Self {
        Self {
            task_id: Zeroizing::new(task_id.to_string()),
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            state,
        }
    }

    pub const fn phase(&self) -> &'static str {
        match self.state {
            CidarenPreQuestionState::ReadyToSelectWords(_) => CIDAREN_READY_TO_SELECT_WORDS_PHASE,
            CidarenPreQuestionState::ReadyToStart(_) => CIDAREN_READY_TO_START_PHASE,
            CidarenPreQuestionState::ReadingCard(_) => CIDAREN_READING_CARD_PHASE,
        }
    }

    /// Encodes the minimal pre-Question continuation for encrypted Core
    /// storage. Fresh word selection maps are deliberately rediscovered rather
    /// than copied into this artifact.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the artifact is malformed or exceeds bounds.
    pub fn encode(&self) -> ProviderResult<EncodedCidarenPreQuestionArtifact> {
        if !valid_remote_task_id(&self.remote_task_id) {
            return Err(protocol_drift(
                "Cidaren pre-Question artifact Task binding is invalid",
            ));
        }
        if matches!(
            self.state,
            CidarenPreQuestionState::ReadyToSelectWords(position)
                | CidarenPreQuestionState::ReadyToStart(position)
                if !valid_position(position)
        ) {
            return Err(protocol_drift(
                "Cidaren pre-Question artifact position is invalid",
            ));
        }
        let (topic_code, reading_card_id, stem_sanitized, position, progress) = match &self.state {
            CidarenPreQuestionState::ReadyToSelectWords(position)
            | CidarenPreQuestionState::ReadyToStart(position) => {
                (None, None, None, Some(*position), None)
            }
            CidarenPreQuestionState::ReadingCard(card) => (
                Some(card.topic_code()),
                Some(card.remote_id()),
                Some(card.stem_sanitized()),
                Some(card.position()),
                card.remote_progress(),
            ),
        };
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ArtifactWireRef {
                schema: CIDAREN_PRE_QUESTION_ARTIFACT_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                phase: self.phase(),
                topic_code,
                reading_card_id,
                stem_sanitized,
                position,
                progress_completed: progress.map(CidarenAttemptProgress::completed),
                progress_total: progress.map(CidarenAttemptProgress::total),
            })
            .map_err(|_| invalid_response("Cidaren pre-Question artifact could not be encoded"))?,
        );
        if encoded.is_empty() || encoded.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "Cidaren pre-Question artifact exceeds the encoded bound",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedCidarenPreQuestionArtifact { value, digest })
    }

    /// Decodes one encrypted continuation and repeats exact local/remote Task
    /// binding before any operation can be reconstructed.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest/schema/shape drift or a foreign Task.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_task_id: TaskId,
        expected_remote_task_id: &str,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "Cidaren pre-Question artifact exceeds the encoded bound",
            ));
        }
        if Sha256::digest(bytes).as_slice() != expected_digest {
            return Err(protocol_drift(
                "Cidaren pre-Question artifact digest does not match its attempt",
            ));
        }
        let mut wire: ArtifactWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Cidaren pre-Question artifact schema is invalid"))?;
        if wire.schema != CIDAREN_PRE_QUESTION_ARTIFACT_TYPE
            || wire.task_id != expected_task_id.to_string()
            || wire.remote_task_id != expected_remote_task_id
            || !valid_remote_task_id(&wire.remote_task_id)
        {
            return Err(protocol_drift(
                "Cidaren pre-Question artifact binding is stale or foreign",
            ));
        }

        let state = match wire.phase.as_str() {
            CIDAREN_READY_TO_SELECT_WORDS_PHASE if wire.has_no_ready_payload_fields() => {
                CidarenPreQuestionState::ReadyToSelectWords(wire.ready_position()?)
            }
            CIDAREN_READY_TO_START_PHASE if wire.has_no_ready_payload_fields() => {
                CidarenPreQuestionState::ReadyToStart(wire.ready_position()?)
            }
            CIDAREN_READING_CARD_PHASE => {
                let progress = match (wire.progress_completed, wire.progress_total) {
                    (None, None) => None,
                    (Some(completed), Some(total)) => {
                        Some(CidarenAttemptProgress::from_artifact(completed, total)?)
                    }
                    _ => {
                        return Err(protocol_drift(
                            "Cidaren reading-card artifact progress is incomplete",
                        ));
                    }
                };
                CidarenPreQuestionState::ReadingCard(ParsedCidarenReadingCard::from_artifact(
                    wire.topic_code.take().ok_or_else(|| {
                        protocol_drift("Cidaren reading-card artifact has no topic code")
                    })?,
                    wire.reading_card_id.take().ok_or_else(|| {
                        protocol_drift("Cidaren reading-card artifact has no identity")
                    })?,
                    wire.stem_sanitized.take().ok_or_else(|| {
                        protocol_drift("Cidaren reading-card artifact has no stem")
                    })?,
                    wire.position.ok_or_else(|| {
                        protocol_drift("Cidaren reading-card artifact has no position")
                    })?,
                    progress,
                )?)
            }
            _ => {
                return Err(protocol_drift(
                    "Cidaren pre-Question artifact phase shape is invalid",
                ));
            }
        };
        Ok(Self::new(expected_task_id, expected_remote_task_id, state))
    }

    pub(crate) fn into_state(self) -> CidarenPreQuestionState {
        self.state
    }
}

impl fmt::Debug for CidarenPreQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenPreQuestionArtifact")
            .field("binding", &"configured")
            .field("phase", &self.phase())
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize)]
struct ArtifactWireRef<'a> {
    schema: &'static str,
    task_id: &'a str,
    remote_task_id: &'a str,
    phase: &'static str,
    topic_code: Option<&'a str>,
    reading_card_id: Option<&'a str>,
    stem_sanitized: Option<&'a str>,
    position: Option<u32>,
    progress_completed: Option<u32>,
    progress_total: Option<u32>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ArtifactWire {
    schema: String,
    task_id: String,
    remote_task_id: String,
    phase: String,
    topic_code: Option<String>,
    reading_card_id: Option<String>,
    stem_sanitized: Option<String>,
    position: Option<u32>,
    progress_completed: Option<u32>,
    progress_total: Option<u32>,
}

impl ArtifactWire {
    fn has_no_ready_payload_fields(&self) -> bool {
        self.topic_code.is_none()
            && self.reading_card_id.is_none()
            && self.stem_sanitized.is_none()
            && self.progress_completed.is_none()
            && self.progress_total.is_none()
    }

    fn ready_position(&self) -> ProviderResult<u32> {
        let position = self.position.ok_or_else(|| {
            protocol_drift("Cidaren pre-Question artifact has no position binding")
        })?;
        if !valid_position(position) {
            return Err(protocol_drift(
                "Cidaren pre-Question artifact position is invalid",
            ));
        }
        Ok(position)
    }
}

const fn valid_position(position: u32) -> bool {
    position > 0 && position <= MAX_POSITION
}

fn valid_remote_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_TASK_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && (value.starts_with("class-task:") || value.starts_with("study-task:"))
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const REMOTE_TASK_ID: &str = "class-task:2002";

    #[test]
    fn pre_question_artifact_roundtrips_and_redacts_bindings() {
        let task_id = TaskId::new();
        let artifact =
            CidarenPreQuestionArtifact::ready_to_select_words(task_id, REMOTE_TASK_ID, 9);
        assert_eq!(artifact.phase(), CIDAREN_READY_TO_SELECT_WORDS_PHASE);
        assert!(!format!("{artifact:?}").contains(REMOTE_TASK_ID));

        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        assert!(!format!("{encoded:?}").contains(REMOTE_TASK_ID));
        let value = encoded.into_secret_value();
        let decoded =
            CidarenPreQuestionArtifact::decode_bound(&value, digest, task_id, REMOTE_TASK_ID)
                .unwrap();

        assert_eq!(decoded.phase(), CIDAREN_READY_TO_SELECT_WORDS_PHASE);
        assert!(!format!("{decoded:?}").contains(REMOTE_TASK_ID));
        assert!(matches!(
            decoded.into_state(),
            CidarenPreQuestionState::ReadyToSelectWords(9)
        ));
    }

    #[test]
    fn pre_question_artifact_rejects_unknown_and_phase_foreign_fields() {
        let task_id = TaskId::new();
        assert!(
            CidarenPreQuestionArtifact::ready_to_start(task_id, REMOTE_TASK_ID, 0)
                .encode()
                .is_err()
        );
        let mut unknown = ready_wire(task_id, CIDAREN_READY_TO_START_PHASE);
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), json!(true));
        assert_decode_rejected(&unknown, task_id);

        let mut foreign = ready_wire(task_id, CIDAREN_READY_TO_START_PHASE);
        foreign.as_object_mut().unwrap().insert(
            "topic_code".to_owned(),
            json!("must-not-survive-ready-state"),
        );
        assert_decode_rejected(&foreign, task_id);

        let mut zero_position = ready_wire(task_id, CIDAREN_READY_TO_START_PHASE);
        zero_position
            .as_object_mut()
            .unwrap()
            .insert("position".to_owned(), json!(0));
        assert_decode_rejected(&zero_position, task_id);

        assert_decode_rejected(&ready_wire(task_id, CIDAREN_READY_TO_START_PHASE), task_id);

        assert_decode_rejected(&ready_wire(task_id, "cidaren.unknown-phase"), task_id);
    }

    #[test]
    fn reading_card_artifact_rejects_incomplete_and_invalid_progress() {
        let task_id = TaskId::new();
        let mut incomplete = reading_wire(task_id);
        incomplete
            .as_object_mut()
            .unwrap()
            .insert("progress_completed".to_owned(), json!(1));
        assert_decode_rejected(&incomplete, task_id);

        let mut reversed = reading_wire(task_id);
        reversed
            .as_object_mut()
            .unwrap()
            .insert("progress_completed".to_owned(), json!(2));
        reversed
            .as_object_mut()
            .unwrap()
            .insert("progress_total".to_owned(), json!(1));
        assert_decode_rejected(&reversed, task_id);

        let mut zero_total = reading_wire(task_id);
        zero_total
            .as_object_mut()
            .unwrap()
            .insert("progress_completed".to_owned(), json!(0));
        zero_total
            .as_object_mut()
            .unwrap()
            .insert("progress_total".to_owned(), json!(0));
        assert_decode_rejected(&zero_total, task_id);
    }

    #[test]
    fn pre_question_artifact_rejects_invalid_reading_shape_and_oversized_input() {
        let task_id = TaskId::new();
        let mut invalid_card = reading_wire(task_id);
        invalid_card
            .as_object_mut()
            .unwrap()
            .insert("reading_card_id".to_owned(), json!("question:foreign"));
        assert_decode_rejected(&invalid_card, task_id);

        let oversized = SecretValue::new(vec![b'x'; MAX_ARTIFACT_BYTES + 1]);
        assert!(
            CidarenPreQuestionArtifact::decode_bound(
                &oversized,
                Sha256::digest(oversized.expose_secret()).into(),
                task_id,
                REMOTE_TASK_ID,
            )
            .is_err()
        );
    }

    fn ready_wire(task_id: TaskId, phase: &str) -> Value {
        json!({
            "schema": CIDAREN_PRE_QUESTION_ARTIFACT_TYPE,
            "task_id": task_id.to_string(),
            "remote_task_id": REMOTE_TASK_ID,
            "phase": phase,
            "topic_code": null,
            "reading_card_id": null,
            "stem_sanitized": null,
            "position": null,
            "progress_completed": null,
            "progress_total": null
        })
    }

    fn reading_wire(task_id: TaskId) -> Value {
        json!({
            "schema": CIDAREN_PRE_QUESTION_ARTIFACT_TYPE,
            "task_id": task_id.to_string(),
            "remote_task_id": REMOTE_TASK_ID,
            "phase": CIDAREN_READING_CARD_PHASE,
            "topic_code": "synthetic-topic-code",
            "reading_card_id": "reading-card:0123456789abcdef",
            "stem_sanitized": "Synthetic reading card",
            "position": 1,
            "progress_completed": null,
            "progress_total": null
        })
    }

    fn assert_decode_rejected(wire: &Value, task_id: TaskId) {
        let value = SecretValue::new(serde_json::to_vec(&wire).unwrap());
        let digest = Sha256::digest(value.expose_secret()).into();
        assert!(
            CidarenPreQuestionArtifact::decode_bound(&value, digest, task_id, REMOTE_TASK_ID,)
                .is_err()
        );
    }
}
