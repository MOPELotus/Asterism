use std::fmt;

use asterism_domain::TaskId;
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{CidarenAttemptProgress, ParsedCidarenReadingCard};

pub const CIDAREN_PRE_QUESTION_ARTIFACT_TYPE: &str = "cidaren.pre-question-attempt.v1";
pub const CIDAREN_READY_TO_SELECT_WORDS_PHASE: &str = "cidaren.ready-to-select-words";
pub const CIDAREN_READY_TO_START_PHASE: &str = "cidaren.ready-to-start";
pub const CIDAREN_READING_CARD_PHASE: &str = "cidaren.reading-card";

const MAX_ARTIFACT_BYTES: usize = 128 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 768;

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
    ReadyToSelectWords,
    ReadyToStart,
    ReadingCard(ParsedCidarenReadingCard),
}

impl CidarenPreQuestionArtifact {
    pub(crate) fn ready_to_select_words(task_id: TaskId, remote_task_id: &str) -> Self {
        Self::new(
            task_id,
            remote_task_id,
            CidarenPreQuestionState::ReadyToSelectWords,
        )
    }

    pub(crate) fn ready_to_start(task_id: TaskId, remote_task_id: &str) -> Self {
        Self::new(
            task_id,
            remote_task_id,
            CidarenPreQuestionState::ReadyToStart,
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
            CidarenPreQuestionState::ReadyToSelectWords => CIDAREN_READY_TO_SELECT_WORDS_PHASE,
            CidarenPreQuestionState::ReadyToStart => CIDAREN_READY_TO_START_PHASE,
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
        let (topic_code, reading_card_id, stem_sanitized, position, progress) = match &self.state {
            CidarenPreQuestionState::ReadyToSelectWords | CidarenPreQuestionState::ReadyToStart => {
                (None, None, None, None, None)
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
        let wire: ArtifactWire = serde_json::from_slice(bytes)
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
            CIDAREN_READY_TO_SELECT_WORDS_PHASE if wire.has_no_reading_card_fields() => {
                CidarenPreQuestionState::ReadyToSelectWords
            }
            CIDAREN_READY_TO_START_PHASE if wire.has_no_reading_card_fields() => {
                CidarenPreQuestionState::ReadyToStart
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
                    wire.topic_code.clone().ok_or_else(|| {
                        protocol_drift("Cidaren reading-card artifact has no topic code")
                    })?,
                    wire.reading_card_id.clone().ok_or_else(|| {
                        protocol_drift("Cidaren reading-card artifact has no identity")
                    })?,
                    wire.stem_sanitized.clone().ok_or_else(|| {
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
    fn has_no_reading_card_fields(&self) -> bool {
        self.topic_code.is_none()
            && self.reading_card_id.is_none()
            && self.stem_sanitized.is_none()
            && self.position.is_none()
            && self.progress_completed.is_none()
            && self.progress_total.is_none()
    }
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
