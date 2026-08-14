use std::{collections::BTreeSet, fmt, str::FromStr};

use asterism_domain::{Question, QuestionAttachmentKind, QuestionContentFingerprint};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    ParsedUaiQuestion,
    question::{
        canonical_media_url, media_attachment_id, valid_question_identity,
        validate_remote_task_identity,
    },
};

pub const UAI_QUESTION_ARTIFACT_TYPE: &str = "uai.question-attempt.v1";
pub const UAI_QUESTION_ARTIFACT_PHASE: &str = "answer-media";

const MAX_ARTIFACT_BYTES: usize = 768 * 1_024;
const MAX_MEDIA_SOURCES: usize = 64;
const MAX_ATTACHMENT_ID_BYTES: usize = 128;
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_POSITION: u32 = 100_000;

/// Encoded Provider continuation intended for Core's encrypted
/// `QuestionSession` artifact store.
pub struct EncodedUaiQuestionArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiQuestionArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiQuestionArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

/// Encrypted-at-rest UAI continuation containing media routes that are
/// deliberately absent from the persisted Domain Question.
pub struct UaiQuestionArtifact {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    remote_question_id: Zeroizing<String>,
    position: u32,
    question_fingerprint: Zeroizing<String>,
    media_sources: Vec<UaiQuestionArtifactMediaSource>,
}

impl UaiQuestionArtifact {
    /// Binds freshly parsed Provider-private media routes to the exact
    /// normalized Question that Core will snapshot.
    ///
    /// # Errors
    ///
    /// Rejects Questions without media routes and any identity, attachment or
    /// semantic mismatch between the parser output and normalized Question.
    pub fn from_parsed(parsed: &ParsedUaiQuestion, question: &Question) -> ProviderResult<Self> {
        question
            .validate()
            .map_err(|_| invalid_response("UAI artifact Question is invalid"))?;
        let expected = parsed.to_question(question.task_id)?;
        let fingerprint = question
            .content_fingerprint()
            .map_err(|_| invalid_response("UAI artifact Question fingerprint is invalid"))?;
        let expected_fingerprint = expected
            .content_fingerprint()
            .map_err(|_| invalid_response("UAI parsed Question fingerprint is invalid"))?;
        let remote_task_id = question
            .metadata_sanitized
            .get("remote_task_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| protocol_drift("UAI Question has no remote Task binding"))?;
        validate_remote_task_identity(remote_task_id)?;
        if fingerprint != expected_fingerprint
            || question.remote_question_id.as_deref() != Some(parsed.remote_id())
            || question.position != parsed.position()
            || parsed.media_sources().is_empty()
            || parsed.media_sources().len() > MAX_MEDIA_SOURCES
        {
            return Err(protocol_drift(
                "UAI media artifact does not match the freshly parsed Question",
            ));
        }

        let media_sources = parsed
            .media_sources()
            .iter()
            .map(|source| {
                if source.remote_task_id() != remote_task_id
                    || source.remote_question_id() != parsed.remote_id()
                    || !question_has_source(
                        question,
                        source.attachment_id(),
                        source.kind(),
                        source.is_subtitle(),
                    )
                {
                    return Err(protocol_drift(
                        "UAI media artifact source is stale or foreign",
                    ));
                }
                UaiQuestionArtifactMediaSource::try_new(
                    source.attachment_id().to_owned(),
                    source.kind(),
                    source.expose_url(),
                    source.is_subtitle(),
                    remote_task_id,
                    parsed.remote_id(),
                )
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        validate_source_set(&media_sources)?;
        validate_question_source_ids(question, &media_sources)?;

        Ok(Self {
            task_id: Zeroizing::new(question.task_id.to_string()),
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            remote_question_id: Zeroizing::new(parsed.remote_id().to_owned()),
            position: parsed.position(),
            question_fingerprint: Zeroizing::new(fingerprint.to_string()),
            media_sources,
        })
    }

    /// Encodes a deterministic, bounded continuation and returns its digest.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` when serialization fails or exceeds Core's
    /// bounded secret-store limit.
    pub fn encode(&self) -> ProviderResult<EncodedUaiQuestionArtifact> {
        let sources = self
            .media_sources
            .iter()
            .map(UaiQuestionArtifactMediaSource::wire_ref)
            .collect::<Vec<_>>();
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ArtifactWireRef {
                schema: UAI_QUESTION_ARTIFACT_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                remote_question_id: &self.remote_question_id,
                position: self.position,
                question_fingerprint: &self.question_fingerprint,
                media_sources: &sources,
            })
            .map_err(|_| invalid_response("UAI Question artifact could not be encoded"))?,
        );
        if encoded.is_empty() || encoded.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "UAI Question artifact exceeds the encoded bound",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedUaiQuestionArtifact { value, digest })
    }

    /// Decodes a continuation only after its session digest and all available
    /// Task/Question/media bindings have been checked.
    ///
    /// # Errors
    ///
    /// Rejects unknown fields, malformed or non-canonical URLs, attachment
    /// substitutions, digest mismatch and foreign or semantically changed
    /// Questions.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_remote_task_id: &str,
        expected_question: &Question,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "UAI Question artifact exceeds the encoded bound",
            ));
        }
        if Sha256::digest(bytes).as_slice() != expected_digest {
            return Err(protocol_drift(
                "UAI Question artifact digest does not match its session",
            ));
        }
        validate_remote_task_identity(expected_remote_task_id)?;
        expected_question
            .validate()
            .map_err(|_| invalid_response("UAI artifact Question is invalid"))?;
        let wire: ArtifactWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("UAI Question artifact schema is invalid"))?;
        let expected_fingerprint = expected_question
            .content_fingerprint()
            .map_err(|_| invalid_response("UAI artifact Question fingerprint is invalid"))?;
        let parsed_fingerprint =
            QuestionContentFingerprint::from_str(&wire.question_fingerprint)
                .map_err(|_| protocol_drift("UAI Question artifact fingerprint is malformed"))?;
        if wire.schema != UAI_QUESTION_ARTIFACT_TYPE
            || wire.task_id != expected_question.task_id.to_string()
            || wire.remote_task_id != expected_remote_task_id
            || !valid_text(&wire.remote_task_id, MAX_REMOTE_TASK_ID_BYTES)
            || expected_question.remote_question_id.as_deref()
                != Some(wire.remote_question_id.as_str())
            || !valid_text(&wire.remote_question_id, MAX_REMOTE_QUESTION_ID_BYTES)
            || wire.position == 0
            || wire.position > MAX_POSITION
            || wire.position != expected_question.position
            || parsed_fingerprint != expected_fingerprint
            || wire.media_sources.is_empty()
            || wire.media_sources.len() > MAX_MEDIA_SOURCES
        {
            return Err(protocol_drift(
                "UAI Question artifact binding is stale or foreign",
            ));
        }
        valid_question_identity(&wire.remote_question_id)?;

        let media_sources = wire
            .media_sources
            .iter()
            .map(|source| {
                let kind = parse_attachment_kind(&source.kind)?;
                if !question_has_source(
                    expected_question,
                    &source.attachment_id,
                    kind,
                    source.subtitle,
                ) {
                    return Err(protocol_drift(
                        "UAI Question artifact attachment no longer matches its Question",
                    ));
                }
                UaiQuestionArtifactMediaSource::try_new(
                    source.attachment_id.clone(),
                    kind,
                    &source.url,
                    source.subtitle,
                    &wire.remote_task_id,
                    &wire.remote_question_id,
                )
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        validate_source_set(&media_sources)?;
        validate_question_source_ids(expected_question, &media_sources)?;

        Ok(Self {
            task_id: Zeroizing::new(wire.task_id.clone()),
            remote_task_id: Zeroizing::new(wire.remote_task_id.clone()),
            remote_question_id: Zeroizing::new(wire.remote_question_id.clone()),
            position: wire.position,
            question_fingerprint: Zeroizing::new(wire.question_fingerprint.clone()),
            media_sources,
        })
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn remote_question_id(&self) -> &str {
        &self.remote_question_id
    }

    pub fn media_sources(&self) -> &[UaiQuestionArtifactMediaSource] {
        &self.media_sources
    }
}

impl fmt::Debug for UaiQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiQuestionArtifact")
            .field("binding", &"configured")
            .field("position", &self.position)
            .field("question_fingerprint", &self.question_fingerprint)
            .field("media_source_count", &self.media_sources.len())
            .finish_non_exhaustive()
    }
}

pub struct UaiQuestionArtifactMediaSource {
    attachment_id: Zeroizing<String>,
    kind: QuestionAttachmentKind,
    url: Zeroizing<String>,
    subtitle: bool,
}

impl UaiQuestionArtifactMediaSource {
    fn try_new(
        attachment_id: String,
        kind: QuestionAttachmentKind,
        url: &str,
        subtitle: bool,
        remote_task_id: &str,
        remote_question_id: &str,
    ) -> ProviderResult<Self> {
        if !valid_text(&attachment_id, MAX_ATTACHMENT_ID_BYTES) {
            return Err(protocol_drift(
                "UAI Question artifact attachment identity is invalid",
            ));
        }
        let (canonical_url, canonical_kind) = canonical_media_url(url, subtitle)?;
        if canonical_url != url || canonical_kind != kind {
            return Err(protocol_drift(
                "UAI Question artifact media route is non-canonical",
            ));
        }
        let expected_attachment_id =
            media_attachment_id(remote_task_id, remote_question_id, &canonical_url);
        if attachment_id != expected_attachment_id {
            return Err(protocol_drift(
                "UAI Question artifact media route does not match its attachment",
            ));
        }
        Ok(Self {
            attachment_id: Zeroizing::new(attachment_id),
            kind,
            url: Zeroizing::new(canonical_url),
            subtitle,
        })
    }

    pub fn attachment_id(&self) -> &str {
        &self.attachment_id
    }

    pub const fn kind(&self) -> QuestionAttachmentKind {
        self.kind
    }

    pub fn expose_url(&self) -> &str {
        &self.url
    }

    pub const fn is_subtitle(&self) -> bool {
        self.subtitle
    }

    fn wire_ref(&self) -> MediaSourceWireRef<'_> {
        MediaSourceWireRef {
            attachment_id: &self.attachment_id,
            kind: attachment_kind_name(self.kind),
            url: &self.url,
            subtitle: self.subtitle,
        }
    }
}

impl fmt::Debug for UaiQuestionArtifactMediaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiQuestionArtifactMediaSource")
            .field("attachment_id", &self.attachment_id)
            .field("kind", &self.kind)
            .field("url", &"[REDACTED]")
            .field("subtitle", &self.subtitle)
            .finish()
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
    media_sources: &'a [MediaSourceWireRef<'a>],
}

#[derive(Serialize)]
struct MediaSourceWireRef<'a> {
    attachment_id: &'a str,
    kind: &'static str,
    url: &'a str,
    subtitle: bool,
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
    media_sources: Vec<MediaSourceWire>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct MediaSourceWire {
    attachment_id: String,
    kind: String,
    url: String,
    subtitle: bool,
}

fn question_has_source(
    question: &Question,
    attachment_id: &str,
    kind: QuestionAttachmentKind,
    subtitle: bool,
) -> bool {
    question.attachments.iter().any(|attachment| {
        attachment.remote_id.as_deref() == Some(attachment_id)
            && attachment.kind == kind
            && attachment
                .metadata_sanitized
                .get("schema")
                .and_then(serde_json::Value::as_str)
                == Some("uai.question-media.v1")
            && attachment
                .metadata_sanitized
                .get("subtitle")
                .and_then(serde_json::Value::as_bool)
                == Some(subtitle)
    })
}

fn validate_source_set(sources: &[UaiQuestionArtifactMediaSource]) -> ProviderResult<()> {
    let mut attachment_ids = BTreeSet::new();
    let mut urls = BTreeSet::new();
    if sources.iter().any(|source| {
        !attachment_ids.insert(source.attachment_id.as_str()) || !urls.insert(source.url.as_str())
    }) {
        return Err(protocol_drift(
            "UAI Question artifact contains duplicate media sources",
        ));
    }
    Ok(())
}

fn validate_question_source_ids(
    question: &Question,
    sources: &[UaiQuestionArtifactMediaSource],
) -> ProviderResult<()> {
    let expected = question
        .metadata_sanitized
        .get("media_attachment_ids")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| protocol_drift("UAI Question has no media attachment binding"))?;
    if expected.len() != sources.len()
        || expected
            .iter()
            .zip(sources)
            .any(|(expected, source)| expected.as_str() != Some(source.attachment_id.as_str()))
    {
        return Err(protocol_drift(
            "UAI Question artifact media set does not match its Question",
        ));
    }
    Ok(())
}

const fn attachment_kind_name(kind: QuestionAttachmentKind) -> &'static str {
    match kind {
        QuestionAttachmentKind::Image => "image",
        QuestionAttachmentKind::Audio => "audio",
        QuestionAttachmentKind::Video => "video",
        QuestionAttachmentKind::File => "file",
        QuestionAttachmentKind::Formula => "formula",
        QuestionAttachmentKind::Other => "other",
    }
}

fn parse_attachment_kind(value: &str) -> ProviderResult<QuestionAttachmentKind> {
    match value {
        "image" => Ok(QuestionAttachmentKind::Image),
        "audio" => Ok(QuestionAttachmentKind::Audio),
        "video" => Ok(QuestionAttachmentKind::Video),
        "file" => Ok(QuestionAttachmentKind::File),
        "formula" => Ok(QuestionAttachmentKind::Formula),
        "other" => Ok(QuestionAttachmentKind::Other),
        _ => Err(protocol_drift(
            "UAI Question artifact attachment kind is unknown",
        )),
    }
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
    use crate::question::parse_question_entry;

    const REMOTE_TASK_ID: &str = "group:2001:unit-1:group-media";

    fn parsed_question(task_id: TaskId) -> (ParsedUaiQuestion, Question) {
        let parsed = parse_question_entry(
            &json!({
                "id": "9001",
                "content": {
                    "type": "short_answer",
                    "direction": {"text": "Summarize the recording"},
                    "contents": [{
                        "name": "Listening.mp3",
                        "path": "https://media.example.edu/listening.mp3#duration=10",
                        "subtitles": [{
                            "name": "English",
                            "path": "https://media.example.edu/listening.vtt#track=en"
                        }]
                    }],
                    "children": [{
                        "type": "basic",
                        "replyType": "text-area",
                        "quesText": "What happened?"
                    }]
                }
            }),
            1,
            "short_answer",
            REMOTE_TASK_ID,
        )
        .unwrap();
        let question = parsed.to_question(task_id).unwrap();
        (parsed, question)
    }

    #[test]
    fn media_artifact_round_trips_with_stable_digest_and_redaction() {
        let (parsed, question) = parsed_question(TaskId::new());
        let artifact = UaiQuestionArtifact::from_parsed(&parsed, &question).unwrap();
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        assert_eq!(artifact.encode().unwrap().digest(), digest);
        assert!(!format!("{artifact:?}").contains("media.example.edu"));
        assert!(!format!("{encoded:?}").contains("media.example.edu"));

        let value = encoded.into_secret_value();
        let decoded =
            UaiQuestionArtifact::decode_bound(&value, digest, REMOTE_TASK_ID, &question).unwrap();
        assert_eq!(decoded.media_sources().len(), 2);
        assert_eq!(
            decoded.media_sources()[0].expose_url(),
            "https://media.example.edu/listening.mp3"
        );
        assert!(!format!("{decoded:?}").contains("media.example.edu"));
        assert!(!format!("{:?}", decoded.media_sources()[0]).contains("media.example.edu"));
    }

    #[test]
    fn media_artifact_rejects_digest_task_and_question_substitution() {
        let (parsed, question) = parsed_question(TaskId::new());
        let encoded = UaiQuestionArtifact::from_parsed(&parsed, &question)
            .unwrap()
            .encode()
            .unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        assert!(
            UaiQuestionArtifact::decode_bound(&value, [7; 32], REMOTE_TASK_ID, &question).is_err()
        );
        assert!(
            UaiQuestionArtifact::decode_bound(
                &value,
                digest,
                "group:2001:unit-1:group-foreign",
                &question,
            )
            .is_err()
        );
        let (_, foreign_question) = parsed_question(TaskId::new());
        assert!(
            UaiQuestionArtifact::decode_bound(&value, digest, REMOTE_TASK_ID, &foreign_question,)
                .is_err()
        );
    }

    #[test]
    fn media_artifact_rejects_url_attachment_and_schema_drift() {
        let (parsed, question) = parsed_question(TaskId::new());
        let encoded = UaiQuestionArtifact::from_parsed(&parsed, &question)
            .unwrap()
            .encode()
            .unwrap();
        let value = encoded.into_secret_value();
        let mut wire: Value = serde_json::from_slice(value.expose_secret()).unwrap();

        wire["media_sources"][0]["url"] = json!("https://media.example.edu/changed.mp3");
        assert_rejected_wire(&wire, &question);

        wire = serde_json::from_slice(value.expose_secret()).unwrap();
        wire["media_sources"][0]["attachment_id"] = json!("uai-media-v1:foreign");
        assert_rejected_wire(&wire, &question);

        wire = serde_json::from_slice(value.expose_secret()).unwrap();
        wire["unexpected"] = json!(true);
        assert_rejected_wire(&wire, &question);
    }

    #[test]
    fn media_artifact_rejects_question_without_media_and_oversized_value() {
        let parsed = parse_question_entry(
            &json!({
                "id": "9002",
                "content": {
                    "type": "basic",
                    "direction": {"text": "Choose one"},
                    "children": [{
                        "type": "basic",
                        "replyType": "singlechoice",
                        "options": [
                            {"name": "A", "text": "Alpha"},
                            {"name": "B", "text": "Beta"}
                        ]
                    }]
                }
            }),
            1,
            "single-choice",
            "group:2001:unit-1:group-questions",
        )
        .unwrap();
        let question = parsed.to_question(TaskId::new()).unwrap();
        assert!(UaiQuestionArtifact::from_parsed(&parsed, &question).is_err());

        let oversized = SecretValue::new(vec![b'x'; MAX_ARTIFACT_BYTES + 1]);
        assert!(
            UaiQuestionArtifact::decode_bound(
                &oversized,
                Sha256::digest(oversized.expose_secret()).into(),
                REMOTE_TASK_ID,
                &question,
            )
            .is_err()
        );
    }

    fn assert_rejected_wire(wire: &Value, question: &Question) {
        let value = SecretValue::new(serde_json::to_vec(wire).unwrap());
        let digest = Sha256::digest(value.expose_secret()).into();
        assert!(
            UaiQuestionArtifact::decode_bound(&value, digest, REMOTE_TASK_ID, question).is_err()
        );
    }
}
