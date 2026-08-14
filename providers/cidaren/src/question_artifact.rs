use std::{fmt, str::FromStr};

use asterism_domain::{Question, QuestionContentFingerprint};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::ParsedCidarenAttemptQuestion;

pub const CIDAREN_QUESTION_ARTIFACT_TYPE: &str = "cidaren.question-attempt.v2";
pub const CIDAREN_QUESTION_ARTIFACT_PHASE: &str = "cidaren.current-question";
pub const CIDAREN_READY_TO_VERIFY_PHASE: &str = "cidaren.ready-to-verify";
pub const CIDAREN_READY_TO_ADVANCE_PHASE: &str = "cidaren.ready-to-advance";

const MAX_ARTIFACT_BYTES: usize = 16 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 768;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_TOPIC_CODE_BYTES: usize = 4_096;
const MAX_POSITION: u32 = 100_000;
const MAX_VERIFIED_STEPS: u32 = 256;

/// Encrypted-at-rest Provider continuation attached to one immutable Core
/// `QuestionSession`. Only the digest and type enter the Domain session.
pub struct EncodedCidarenQuestionArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedCidarenQuestionArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedCidarenQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedCidarenQuestionArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

/// Strictly decoded Cidaren continuation for the current remote Question.
/// The one-time topic code never enters Debug output and is zeroized on drop.
pub struct CidarenQuestionArtifact {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    remote_question_id: Zeroizing<String>,
    position: u32,
    question_fingerprint: Zeroizing<String>,
    topic_code: Zeroizing<String>,
    verified_steps: u32,
}

impl CidarenQuestionArtifact {
    /// Binds one freshly parsed remote step to the exact normalized Question
    /// that Core will snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the parsed step and Question do not describe
    /// the same Task, identity, position and semantic content.
    pub fn from_parsed(
        parsed: &ParsedCidarenAttemptQuestion,
        question: &Question,
    ) -> ProviderResult<Self> {
        question
            .validate()
            .map_err(|_| invalid_response("Cidaren artifact Question is invalid"))?;
        let expected = parsed.to_question(question.task_id)?;
        let fingerprint = question
            .content_fingerprint()
            .map_err(|_| invalid_response("Cidaren artifact Question fingerprint is invalid"))?;
        let expected_fingerprint = expected
            .content_fingerprint()
            .map_err(|_| invalid_response("Cidaren parsed Question fingerprint is invalid"))?;
        if question.remote_question_id.as_deref() != Some(parsed.remote_id())
            || question.position != parsed.position()
            || fingerprint != expected_fingerprint
            || !valid_remote_task_id(parsed.remote_task_id())
            || !valid_topic_code(parsed.topic_code())
        {
            return Err(protocol_drift(
                "Cidaren artifact does not match the freshly parsed Question",
            ));
        }

        Ok(Self {
            task_id: Zeroizing::new(question.task_id.to_string()),
            remote_task_id: Zeroizing::new(parsed.remote_task_id().to_owned()),
            remote_question_id: Zeroizing::new(parsed.remote_id().to_owned()),
            position: parsed.position(),
            question_fingerprint: Zeroizing::new(fingerprint.to_string()),
            topic_code: Zeroizing::new(parsed.topic_code().to_owned()),
            verified_steps: 0,
        })
    }

    /// Encodes the minimal Provider-private continuation using a stable schema.
    /// The returned plaintext wrapper zeroizes its value on drop and is intended
    /// to cross directly into Core's encrypted `QuestionSession` artifact store.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` if canonical encoding fails or exceeds bounds.
    pub fn encode(&self) -> ProviderResult<EncodedCidarenQuestionArtifact> {
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ArtifactWireRef {
                schema: CIDAREN_QUESTION_ARTIFACT_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                remote_question_id: &self.remote_question_id,
                position: self.position,
                question_fingerprint: &self.question_fingerprint,
                topic_code: &self.topic_code,
                verified_steps: self.verified_steps,
            })
            .map_err(|_| invalid_response("Cidaren Question artifact could not be encoded"))?,
        );
        if encoded.is_empty() || encoded.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "Cidaren Question artifact exceeds the encoded bound",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedCidarenQuestionArtifact { value, digest })
    }

    /// Records a rotated topic code after an exact number of immutable Draft
    /// relation steps have been accepted. The count is encrypted Provider
    /// state and never enters the Draft.
    ///
    /// # Errors
    ///
    /// Returns `ProtocolDrift` for an invalid token or impossible checkpoint.
    pub fn checkpoint_after_verify(
        mut self,
        next_topic_code: &str,
        verified_steps: u32,
    ) -> ProviderResult<Self> {
        if !valid_topic_code(next_topic_code)
            || verified_steps == 0
            || verified_steps > MAX_VERIFIED_STEPS
            || verified_steps <= self.verified_steps
        {
            return Err(protocol_drift(
                "Cidaren verified Question checkpoint is invalid",
            ));
        }
        self.topic_code = Zeroizing::new(next_topic_code.to_owned());
        self.verified_steps = verified_steps;
        Ok(self)
    }

    /// Decodes an encrypted continuation only after checking the immutable
    /// `QuestionSession` digest and every available Task/Question binding.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest mismatch, malformed/unknown fields,
    /// exceeded bounds, or a foreign Task/Question artifact.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_remote_task_id: &str,
        expected_question: &Question,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "Cidaren Question artifact exceeds the encoded bound",
            ));
        }
        if Sha256::digest(bytes).as_slice() != expected_digest {
            return Err(protocol_drift(
                "Cidaren Question artifact digest does not match its session",
            ));
        }
        expected_question
            .validate()
            .map_err(|_| invalid_response("Cidaren artifact Question is invalid"))?;
        let mut wire: ArtifactWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Cidaren Question artifact schema is invalid"))?;
        let expected_fingerprint = expected_question
            .content_fingerprint()
            .map_err(|_| invalid_response("Cidaren artifact Question fingerprint is invalid"))?;
        let parsed_fingerprint = QuestionContentFingerprint::from_str(&wire.question_fingerprint)
            .map_err(|_| {
            protocol_drift("Cidaren Question artifact fingerprint is malformed")
        })?;
        if wire.schema != CIDAREN_QUESTION_ARTIFACT_TYPE
            || !valid_remote_task_id(&wire.remote_task_id)
            || wire.remote_task_id != expected_remote_task_id
            || !valid_text(&wire.remote_question_id, MAX_REMOTE_QUESTION_ID_BYTES)
            || expected_question.remote_question_id.as_deref()
                != Some(wire.remote_question_id.as_str())
            || wire.position == 0
            || wire.position > MAX_POSITION
            || wire.position != expected_question.position
            || wire.task_id != expected_question.task_id.to_string()
            || parsed_fingerprint != expected_fingerprint
            || !valid_topic_code(&wire.topic_code)
            || wire.verified_steps > MAX_VERIFIED_STEPS
        {
            return Err(protocol_drift(
                "Cidaren Question artifact binding is stale or foreign",
            ));
        }

        Ok(Self {
            task_id: Zeroizing::new(std::mem::take(&mut wire.task_id)),
            remote_task_id: Zeroizing::new(std::mem::take(&mut wire.remote_task_id)),
            remote_question_id: Zeroizing::new(std::mem::take(&mut wire.remote_question_id)),
            position: wire.position,
            question_fingerprint: Zeroizing::new(std::mem::take(&mut wire.question_fingerprint)),
            topic_code: Zeroizing::new(std::mem::take(&mut wire.topic_code)),
            verified_steps: wire.verified_steps,
        })
    }

    pub(crate) fn topic_code(&self) -> &str {
        &self.topic_code
    }

    pub(crate) const fn verified_steps(&self) -> u32 {
        self.verified_steps
    }
}

impl fmt::Debug for CidarenQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenQuestionArtifact")
            .field("binding", &"configured")
            .field("position", &self.position)
            .field("question_fingerprint", &self.question_fingerprint)
            .field("topic_code", &"[REDACTED]")
            .field("verified_steps", &self.verified_steps)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct ArtifactWireRef<'a> {
    schema: &'static str,
    task_id: &'a str,
    remote_task_id: &'a str,
    remote_question_id: &'a str,
    position: u32,
    question_fingerprint: &'a str,
    topic_code: &'a str,
    verified_steps: u32,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ArtifactWire {
    schema: String,
    task_id: String,
    remote_task_id: String,
    remote_question_id: String,
    position: u32,
    question_fingerprint: String,
    topic_code: String,
    verified_steps: u32,
}

fn valid_remote_task_id(value: &str) -> bool {
    valid_text(value, MAX_REMOTE_TASK_ID_BYTES)
        && (value.starts_with("class-task:") || value.starts_with("study-task:"))
}

fn valid_topic_code(value: &str) -> bool {
    valid_text(value, MAX_TOPIC_CODE_BYTES)
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::TaskId;
    use serde_json::{Value, json};

    use super::*;
    use crate::parse_attempt_question;

    fn parsed_question(task_id: TaskId) -> (ParsedCidarenAttemptQuestion, Question) {
        let payload: Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/cidaren/questions/start-answer-single.json"
        ))
        .unwrap();
        let parsed = parse_attempt_question(&payload, "class-task:2002", 1).unwrap();
        let question = parsed.to_question(task_id).unwrap();
        (parsed, question)
    }

    #[test]
    fn artifact_round_trips_with_stable_digest_and_redaction() {
        let (parsed, question) = parsed_question(TaskId::new());
        let artifact = CidarenQuestionArtifact::from_parsed(&parsed, &question).unwrap();
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        assert_eq!(artifact.encode().unwrap().digest(), digest);
        assert!(!format!("{artifact:?}").contains("synthetic-topic-code"));
        assert!(!format!("{encoded:?}").contains("synthetic-topic-code"));

        let value = encoded.into_secret_value();
        let decoded =
            CidarenQuestionArtifact::decode_bound(&value, digest, "class-task:2002", &question)
                .unwrap();
        assert_eq!(decoded.topic_code(), "synthetic-topic-code");
        assert_eq!(decoded.verified_steps(), 0);
        assert!(!format!("{decoded:?}").contains("synthetic-topic-code"));

        let checkpoint = CidarenQuestionArtifact::from_parsed(&parsed, &question)
            .unwrap()
            .checkpoint_after_verify("verified-topic-code", 2)
            .unwrap();
        assert_eq!(checkpoint.topic_code(), "verified-topic-code");
        assert_eq!(checkpoint.verified_steps(), 2);
        assert_ne!(checkpoint.encode().unwrap().digest(), digest);
        assert!(
            checkpoint
                .checkpoint_after_verify("regressed-topic-code", 1)
                .is_err()
        );
        assert!(
            CidarenQuestionArtifact::from_parsed(&parsed, &question)
                .unwrap()
                .checkpoint_after_verify(" invalid", 1)
                .is_err()
        );
    }

    #[test]
    fn artifact_rejects_foreign_task_question_and_digest() {
        let (parsed, question) = parsed_question(TaskId::new());
        let encoded = CidarenQuestionArtifact::from_parsed(&parsed, &question)
            .unwrap()
            .encode()
            .unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        assert!(
            CidarenQuestionArtifact::decode_bound(&value, [7; 32], "class-task:2002", &question,)
                .is_err()
        );
        assert!(
            CidarenQuestionArtifact::decode_bound(&value, digest, "class-task:9999", &question,)
                .is_err()
        );

        let (_, foreign_question) = parsed_question(TaskId::new());
        assert!(
            CidarenQuestionArtifact::decode_bound(
                &value,
                digest,
                "class-task:2002",
                &foreign_question,
            )
            .is_err()
        );
    }

    #[test]
    fn artifact_rejects_semantic_drift_unknown_fields_and_oversized_values() {
        let (parsed, question) = parsed_question(TaskId::new());
        let mut drifted = question.clone();
        drifted.stem.push_str(" changed");
        let encoded = CidarenQuestionArtifact::from_parsed(&parsed, &question)
            .unwrap()
            .encode()
            .unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        assert!(
            CidarenQuestionArtifact::decode_bound(&value, digest, "class-task:2002", &drifted,)
                .is_err()
        );

        let unknown = SecretValue::new(
            serde_json::to_vec(&json!({
                "schema": CIDAREN_QUESTION_ARTIFACT_TYPE,
                "task_id": question.task_id.to_string(),
                "remote_task_id": "class-task:2002",
                "remote_question_id": question.remote_question_id,
                "position": 1,
                "question_fingerprint": question.content_fingerprint().unwrap(),
                "topic_code": "synthetic-topic-code",
                "verified_steps": 0,
                "unexpected": true
            }))
            .unwrap(),
        );
        let unknown_digest = Sha256::digest(unknown.expose_secret()).into();
        assert!(
            CidarenQuestionArtifact::decode_bound(
                &unknown,
                unknown_digest,
                "class-task:2002",
                &question,
            )
            .is_err()
        );

        let invalid_checkpoint = SecretValue::new(
            serde_json::to_vec(&json!({
                "schema": CIDAREN_QUESTION_ARTIFACT_TYPE,
                "task_id": question.task_id.to_string(),
                "remote_task_id": "class-task:2002",
                "remote_question_id": question.remote_question_id,
                "position": 1,
                "question_fingerprint": question.content_fingerprint().unwrap(),
                "topic_code": "synthetic-topic-code",
                "verified_steps": MAX_VERIFIED_STEPS + 1
            }))
            .unwrap(),
        );
        let invalid_checkpoint_digest = Sha256::digest(invalid_checkpoint.expose_secret()).into();
        assert!(
            CidarenQuestionArtifact::decode_bound(
                &invalid_checkpoint,
                invalid_checkpoint_digest,
                "class-task:2002",
                &question,
            )
            .is_err()
        );

        let oversized = SecretValue::new(vec![b'x'; MAX_ARTIFACT_BYTES + 1]);
        assert!(
            CidarenQuestionArtifact::decode_bound(
                &oversized,
                Sha256::digest(oversized.expose_secret()).into(),
                "class-task:2002",
                &question,
            )
            .is_err()
        );
    }
}
