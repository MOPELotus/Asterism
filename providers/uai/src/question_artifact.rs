use std::{collections::BTreeSet, fmt, str::FromStr};

use asterism_domain::{Question, QuestionAttachmentKind, QuestionContentFingerprint};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{SecretString, SecretValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    ParsedUaiQuestion,
    question::{
        canonical_media_url, media_attachment_id, parse_subtitle_transcript,
        valid_question_identity, validate_remote_task_identity,
    },
};

pub const UAI_QUESTION_ARTIFACT_TYPE: &str = "uai.question-attempt.v1";
pub const UAI_QUESTION_ARTIFACT_PHASE: &str = "answer-media";
pub const UAI_QUESTION_SET_ARTIFACT_TYPE: &str = "uai.question-set.v1";
pub const UAI_QUESTION_SET_ARTIFACT_PHASE: &str = "uai.answer-media";
pub const UAI_QUESTION_SET_ARTIFACT_TTL_SECONDS: u64 = 5 * 60;

const MAX_ARTIFACT_BYTES: usize = 768 * 1_024;
const MAX_ARTIFACT_QUESTIONS: usize = 5_000;
const MAX_MEDIA_SOURCES: usize = 64;
const MAX_ATTACHMENT_ID_BYTES: usize = 128;
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_POSITION: u32 = 100_000;
const MAX_SUBTITLE_RESPONSE_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_AUDIO_RESPONSE_BYTES: u64 = 128 * 1_024 * 1_024;
const MAX_VIDEO_RESPONSE_BYTES: u64 = 512 * 1_024 * 1_024;
const UCONTENT_MEDIA_HOST: &str = "ucontent.unipus.cn";

/// Encoded Provider continuation intended for Core's encrypted
/// `QuestionSession` artifact store.
pub struct EncodedUaiQuestionArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

/// Encoded complete-snapshot continuation consumed by Core's ordinary
/// read-only Question artifact contract.
pub struct EncodedUaiQuestionArtifactSet {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiQuestionArtifactSet {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiQuestionArtifactSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiQuestionArtifactSet")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
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

/// One complete immutable Question snapshot plus the media routes deliberately
/// omitted from Domain Questions. Entries without media remain in the manifest
/// so a partial or reordered snapshot cannot consume another session's routes.
pub struct UaiQuestionArtifactSet {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    questions: Vec<UaiQuestionArtifactEntry>,
}

impl UaiQuestionArtifactSet {
    /// Aggregates the existing per-Question media binding into one complete
    /// snapshot continuation. An artifact-free snapshot returns `None`.
    pub(crate) fn from_parsed_questions(
        parsed: &[ParsedUaiQuestion],
        questions: &[Question],
        expected_remote_task_id: &str,
    ) -> ProviderResult<Option<Self>> {
        validate_remote_task_identity(expected_remote_task_id)?;
        if parsed.is_empty()
            || parsed.len() != questions.len()
            || parsed.len() > MAX_ARTIFACT_QUESTIONS
        {
            return Err(protocol_drift(
                "UAI Question artifact set has invalid snapshot cardinality",
            ));
        }
        let task_id = questions[0].task_id;
        let mut entries = Vec::with_capacity(questions.len());
        let mut has_media = false;
        for (index, (parsed, question)) in parsed.iter().zip(questions).enumerate() {
            let expected_position = u32::try_from(index + 1)
                .map_err(|_| invalid_response("UAI artifact Question position exceeds bounds"))?;
            let expected = parsed.to_question(task_id)?;
            let fingerprint = question
                .content_fingerprint()
                .map_err(|_| invalid_response("UAI artifact Question fingerprint is invalid"))?;
            let expected_fingerprint = expected.content_fingerprint().map_err(|_| {
                invalid_response("UAI parsed artifact Question fingerprint is invalid")
            })?;
            let remote_question_id = question
                .remote_question_id
                .as_deref()
                .ok_or_else(|| protocol_drift("UAI artifact Question has no remote identity"))?;
            let remote_task_id = question
                .metadata_sanitized
                .get("remote_task_id")
                .and_then(serde_json::Value::as_str);
            if question.task_id != task_id
                || question.position != expected_position
                || parsed.position() != expected_position
                || parsed.remote_id() != remote_question_id
                || remote_task_id != Some(expected_remote_task_id)
                || fingerprint != expected_fingerprint
                || question.validate().is_err()
            {
                return Err(protocol_drift(
                    "UAI Question artifact set does not match its parsed snapshot",
                ));
            }
            valid_question_identity(remote_question_id)?;
            let media_sources = if parsed.media_sources().is_empty() {
                validate_question_source_ids(question, &[])?;
                Vec::new()
            } else {
                has_media = true;
                let artifact = UaiQuestionArtifact::from_parsed(parsed, question)?;
                artifact.media_sources
            };
            entries.push(UaiQuestionArtifactEntry {
                remote_question_id: Zeroizing::new(remote_question_id.to_owned()),
                position: expected_position,
                question_fingerprint: Zeroizing::new(fingerprint.to_string()),
                media_sources,
            });
        }
        if !has_media {
            return Ok(None);
        }
        Ok(Some(Self {
            task_id: Zeroizing::new(task_id.to_string()),
            remote_task_id: Zeroizing::new(expected_remote_task_id.to_owned()),
            questions: entries,
        }))
    }

    /// Encodes one bounded deterministic snapshot continuation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` when serialization fails or the complete
    /// manifest exceeds the encrypted continuation bound.
    pub fn encode(&self) -> ProviderResult<EncodedUaiQuestionArtifactSet> {
        let questions = self
            .questions
            .iter()
            .map(UaiQuestionArtifactEntry::wire_ref)
            .collect::<Vec<_>>();
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ArtifactSetWireRef {
                schema: UAI_QUESTION_SET_ARTIFACT_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                questions: &questions,
            })
            .map_err(|_| invalid_response("UAI Question artifact set could not be encoded"))?,
        );
        if encoded.is_empty() || encoded.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "UAI Question artifact set exceeds the encoded bound",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedUaiQuestionArtifactSet { value, digest })
    }

    /// Decodes and rebinds the complete manifest before exposing any media
    /// source to `AnswerResolve` or a future external resolver.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, digest-mismatched, partial, reordered or
    /// foreign manifests and any changed Question or media binding.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_remote_task_id: &str,
        expected_questions: &[Question],
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "UAI Question artifact set exceeds the encoded bound",
            ));
        }
        if Sha256::digest(bytes).as_slice() != expected_digest {
            return Err(protocol_drift(
                "UAI Question artifact set digest does not match its session",
            ));
        }
        validate_remote_task_identity(expected_remote_task_id)?;
        if expected_questions.is_empty() || expected_questions.len() > MAX_ARTIFACT_QUESTIONS {
            return Err(protocol_drift(
                "UAI Question artifact set has invalid expected cardinality",
            ));
        }
        let wire: ArtifactSetWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("UAI Question artifact set schema is invalid"))?;
        let task_id = expected_questions[0].task_id;
        if wire.schema != UAI_QUESTION_SET_ARTIFACT_TYPE
            || wire.task_id != task_id.to_string()
            || wire.remote_task_id != expected_remote_task_id
            || wire.questions.len() != expected_questions.len()
        {
            return Err(protocol_drift(
                "UAI Question artifact set binding is stale or foreign",
            ));
        }
        let mut entries = Vec::with_capacity(wire.questions.len());
        let mut has_media = false;
        for (index, (wire, question)) in wire.questions.iter().zip(expected_questions).enumerate() {
            let expected_position = u32::try_from(index + 1)
                .map_err(|_| invalid_response("UAI artifact Question position exceeds bounds"))?;
            let remote_question_id = question
                .remote_question_id
                .as_deref()
                .ok_or_else(|| protocol_drift("UAI artifact Question has no remote identity"))?;
            let expected_fingerprint = question
                .content_fingerprint()
                .map_err(|_| invalid_response("UAI artifact Question fingerprint is invalid"))?;
            let parsed_fingerprint =
                QuestionContentFingerprint::from_str(&wire.question_fingerprint).map_err(|_| {
                    protocol_drift("UAI artifact Question fingerprint is malformed")
                })?;
            if question.task_id != task_id
                || question.position != expected_position
                || question.validate().is_err()
                || wire.remote_question_id != remote_question_id
                || wire.position != expected_position
                || parsed_fingerprint != expected_fingerprint
                || question
                    .metadata_sanitized
                    .get("remote_task_id")
                    .and_then(serde_json::Value::as_str)
                    != Some(expected_remote_task_id)
            {
                return Err(protocol_drift(
                    "UAI Question artifact entry is stale or foreign",
                ));
            }
            valid_question_identity(remote_question_id)?;
            let media_sources = wire
                .media_sources
                .iter()
                .map(|source| {
                    UaiQuestionArtifactMediaSource::try_new(
                        source.attachment_id.clone(),
                        parse_attachment_kind(&source.kind)?,
                        &source.url,
                        source.subtitle,
                        expected_remote_task_id,
                        remote_question_id,
                    )
                })
                .collect::<ProviderResult<Vec<_>>>()?;
            validate_source_set(&media_sources)?;
            validate_question_source_ids(question, &media_sources)?;
            has_media |= !media_sources.is_empty();
            entries.push(UaiQuestionArtifactEntry {
                remote_question_id: Zeroizing::new(wire.remote_question_id.clone()),
                position: wire.position,
                question_fingerprint: Zeroizing::new(wire.question_fingerprint.clone()),
                media_sources,
            });
        }
        if !has_media {
            return Err(protocol_drift(
                "UAI Question artifact set contains no media continuation",
            ));
        }
        Ok(Self {
            task_id: Zeroizing::new(wire.task_id.clone()),
            remote_task_id: Zeroizing::new(wire.remote_task_id.clone()),
            questions: entries,
        })
    }

    pub fn media_sources_for_question(
        &self,
        remote_question_id: &str,
    ) -> Option<&[UaiQuestionArtifactMediaSource]> {
        self.questions
            .iter()
            .find(|question| question.remote_question_id.as_str() == remote_question_id)
            .map(|question| question.media_sources.as_slice())
    }

    pub fn question_count(&self) -> usize {
        self.questions.len()
    }

    /// Freezes one exact media fetch request from this already rebound
    /// complete Question continuation.
    ///
    /// # Errors
    ///
    /// Rejects missing, duplicate or foreign Question/attachment identities
    /// and any source that no longer satisfies its canonical route binding.
    pub fn prepare_media_fetch(
        &self,
        remote_question_id: &str,
        attachment_id: &str,
    ) -> ProviderResult<UaiMediaFetchPlan> {
        valid_question_identity(remote_question_id)?;
        if !valid_text(attachment_id, MAX_ATTACHMENT_ID_BYTES) {
            return Err(protocol_drift(
                "UAI media fetch attachment identity is invalid",
            ));
        }
        let mut entries = self
            .questions
            .iter()
            .filter(|question| question.remote_question_id.as_str() == remote_question_id);
        let question = entries
            .next()
            .ok_or_else(|| protocol_drift("UAI media fetch Question is missing or foreign"))?;
        if entries.next().is_some() {
            return Err(protocol_drift(
                "UAI media fetch Question identity is ambiguous",
            ));
        }
        let mut sources = question
            .media_sources
            .iter()
            .filter(|source| source.attachment_id.as_str() == attachment_id);
        let source = sources
            .next()
            .ok_or_else(|| protocol_drift("UAI media fetch attachment is missing or foreign"))?;
        if sources.next().is_some() {
            return Err(protocol_drift(
                "UAI media fetch attachment identity is ambiguous",
            ));
        }
        UaiMediaFetchPlan::try_new(
            &self.task_id,
            &self.remote_task_id,
            &question.remote_question_id,
            question.position,
            &question.question_fingerprint,
            source,
        )
    }

    /// Freezes every media request for one exact Question in donor-observed
    /// source order.
    ///
    /// # Errors
    ///
    /// Rejects a missing, duplicate or media-free Question and any source
    /// that cannot independently reproduce its immutable request authority.
    pub fn prepare_media_fetch_batch(
        &self,
        remote_question_id: &str,
    ) -> ProviderResult<UaiMediaFetchBatchPlan> {
        valid_question_identity(remote_question_id)?;
        let mut entries = self
            .questions
            .iter()
            .filter(|question| question.remote_question_id.as_str() == remote_question_id);
        let question = entries.next().ok_or_else(|| {
            protocol_drift("UAI media fetch batch Question is missing or foreign")
        })?;
        if entries.next().is_some() || question.media_sources.is_empty() {
            return Err(protocol_drift(
                "UAI media fetch batch Question is ambiguous or media-free",
            ));
        }
        let requests = question
            .media_sources
            .iter()
            .map(|source| {
                UaiMediaFetchPlan::try_new(
                    &self.task_id,
                    &self.remote_task_id,
                    &question.remote_question_id,
                    question.position,
                    &question.question_fingerprint,
                    source,
                )
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        UaiMediaFetchBatchPlan::try_new(requests)
    }
}

impl fmt::Debug for UaiQuestionArtifactSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiQuestionArtifactSet")
            .field("binding", &"configured")
            .field("question_count", &self.questions.len())
            .field(
                "media_question_count",
                &self
                    .questions
                    .iter()
                    .filter(|question| !question.media_sources.is_empty())
                    .count(),
            )
            .finish_non_exhaustive()
    }
}

struct UaiQuestionArtifactEntry {
    remote_question_id: Zeroizing<String>,
    position: u32,
    question_fingerprint: Zeroizing<String>,
    media_sources: Vec<UaiQuestionArtifactMediaSource>,
}

impl UaiQuestionArtifactEntry {
    fn wire_ref(&self) -> ArtifactSetQuestionWireRef<'_> {
        ArtifactSetQuestionWireRef {
            remote_question_id: &self.remote_question_id,
            position: self.position,
            question_fingerprint: &self.question_fingerprint,
            media_sources: self
                .media_sources
                .iter()
                .map(UaiQuestionArtifactMediaSource::wire_ref)
                .collect(),
        }
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

/// Whether the future shared downloader may attach the already scoped UAI
/// session to this exact request. External media hosts remain anonymous even
/// though the donor's generic client would forward its default headers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UaiMediaFetchCredentialScope {
    UcontentSession,
    Anonymous,
}

/// Immutable, request-digest-bound authority for one UAI Question media GET.
///
/// It performs no DNS lookup or network I/O. Shared download execution must
/// still enforce DNS/private-range policy, streaming bounds and no redirects.
pub struct UaiMediaFetchPlan {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    remote_question_id: Zeroizing<String>,
    position: u32,
    question_fingerprint: Zeroizing<String>,
    attachment_id: Zeroizing<String>,
    kind: QuestionAttachmentKind,
    subtitle: bool,
    url: Zeroizing<String>,
    credential_scope: UaiMediaFetchCredentialScope,
    max_response_bytes: u64,
    request_digest: [u8; 32],
}

impl UaiMediaFetchPlan {
    fn try_new(
        task_id: &str,
        remote_task_id: &str,
        remote_question_id: &str,
        position: u32,
        question_fingerprint: &str,
        source: &UaiQuestionArtifactMediaSource,
    ) -> ProviderResult<Self> {
        validate_remote_task_identity(remote_task_id)?;
        valid_question_identity(remote_question_id)?;
        QuestionContentFingerprint::from_str(question_fingerprint)
            .map_err(|_| protocol_drift("UAI media fetch Question fingerprint is invalid"))?;
        let (url, kind) = canonical_media_url(&source.url, source.subtitle)?;
        if url != source.url.as_str()
            || kind != source.kind
            || source.attachment_id.as_str()
                != media_attachment_id(remote_task_id, remote_question_id, &url)
            || position == 0
            || position > MAX_POSITION
            || !valid_text(task_id, 128)
        {
            return Err(protocol_drift(
                "UAI media fetch source is stale, malformed or foreign",
            ));
        }
        let parsed = reqwest::Url::parse(&url)
            .map_err(|_| protocol_drift("UAI media fetch URL is malformed"))?;
        let credential_scope = if parsed.host_str() == Some(UCONTENT_MEDIA_HOST)
            && parsed.port_or_known_default() == Some(443)
        {
            UaiMediaFetchCredentialScope::UcontentSession
        } else {
            UaiMediaFetchCredentialScope::Anonymous
        };
        let max_response_bytes = match (source.kind, source.subtitle) {
            (QuestionAttachmentKind::File, true) => MAX_SUBTITLE_RESPONSE_BYTES,
            (QuestionAttachmentKind::Audio, false) => MAX_AUDIO_RESPONSE_BYTES,
            (QuestionAttachmentKind::Video, false) => MAX_VIDEO_RESPONSE_BYTES,
            _ => {
                return Err(protocol_drift(
                    "UAI media fetch source kind is not donor-audited",
                ));
            }
        };
        let request_digest = media_fetch_request_digest(
            task_id,
            remote_task_id,
            remote_question_id,
            position,
            question_fingerprint,
            &source.attachment_id,
            source.kind,
            source.subtitle,
            &url,
            credential_scope,
            max_response_bytes,
        );
        if request_digest == [0; 32] {
            return Err(invalid_response(
                "UAI media fetch request digest is invalid",
            ));
        }
        Ok(Self {
            task_id: Zeroizing::new(task_id.to_owned()),
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            remote_question_id: Zeroizing::new(remote_question_id.to_owned()),
            position,
            question_fingerprint: Zeroizing::new(question_fingerprint.to_owned()),
            attachment_id: source.attachment_id.clone(),
            kind: source.kind,
            subtitle: source.subtitle,
            url: Zeroizing::new(url),
            credential_scope,
            max_response_bytes,
            request_digest,
        })
    }

    pub const fn method(&self) -> &'static str {
        "GET"
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn remote_question_id(&self) -> &str {
        &self.remote_question_id
    }

    pub const fn position(&self) -> u32 {
        self.position
    }

    pub fn question_fingerprint(&self) -> &str {
        &self.question_fingerprint
    }

    pub fn attachment_id(&self) -> &str {
        &self.attachment_id
    }

    pub fn expose_url(&self) -> &str {
        &self.url
    }

    pub const fn credential_scope(&self) -> UaiMediaFetchCredentialScope {
        self.credential_scope
    }

    pub const fn max_response_bytes(&self) -> u64 {
        self.max_response_bytes
    }

    pub const fn kind(&self) -> QuestionAttachmentKind {
        self.kind
    }

    pub const fn is_subtitle(&self) -> bool {
        self.subtitle
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn permits_redirects(&self) -> bool {
        false
    }

    /// Requires the downloader to report the exact canonical route selected
    /// by this plan. Redirect handling stays fail-closed until a shared
    /// redirect chain contract can preserve DNS and credential scope.
    ///
    /// # Errors
    ///
    /// Rejects malformed, non-canonical or changed final routes.
    pub fn validate_final_url(&self, final_url: &str) -> ProviderResult<()> {
        let (canonical, kind) = canonical_media_url(final_url, self.subtitle)?;
        if canonical == self.url.as_str() && kind == self.kind {
            Ok(())
        } else {
            Err(protocol_drift(
                "UAI media fetch response crossed its frozen route",
            ))
        }
    }

    /// Consumes the exact successful downloader output into a response owner
    /// bound to this immutable request.
    ///
    /// Shared execution must apply the plan's DNS/private-range, credential,
    /// redirect and streaming policy before constructing the secret body.
    /// This boundary independently rechecks the facts observable after the
    /// download and does not infer a media type from unaudited response
    /// headers or file signatures.
    ///
    /// # Errors
    ///
    /// Rejects a non-200 response, any observed redirect, changed final route,
    /// empty body or payload exceeding the request's frozen response ceiling.
    pub fn accept_response(
        self,
        status: u16,
        final_url: &str,
        redirect_count: u32,
        body: SecretValue,
    ) -> ProviderResult<UaiMediaFetchResponse> {
        if status != 200 {
            return Err(invalid_response(
                "UAI media fetch did not return an exact successful response",
            ));
        }
        if redirect_count != 0 {
            return Err(protocol_drift(
                "UAI media fetch response followed a forbidden redirect",
            ));
        }
        self.validate_final_url(final_url)?;
        let body_len = u64::try_from(body.expose_secret().len())
            .map_err(|_| invalid_response("UAI media fetch response size is invalid"))?;
        if body_len == 0 || body_len > self.max_response_bytes {
            return Err(invalid_response(
                "UAI media fetch response is empty or exceeds its frozen bound",
            ));
        }
        let body_digest = Sha256::digest(body.expose_secret()).into();
        let response_digest =
            media_fetch_response_digest(self.request_digest, body_digest, body_len);
        if body_digest == [0; 32] || response_digest == [0; 32] {
            return Err(invalid_response(
                "UAI media fetch response digest is invalid",
            ));
        }
        Ok(UaiMediaFetchResponse {
            plan: self,
            body,
            body_len,
            body_digest,
            response_digest,
        })
    }
}

impl fmt::Debug for UaiMediaFetchPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMediaFetchPlan")
            .field("binding", &"[REDACTED]")
            .field("kind", &self.kind)
            .field("subtitle", &self.subtitle)
            .field("credential_scope", &self.credential_scope)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("request_digest", &self.request_digest)
            .finish_non_exhaustive()
    }
}

/// Complete ordered media-request authority for one exact UAI Question.
///
/// It carries no retry, skip, transcription or prompt policy. Shared
/// orchestration must report each independently bound result without changing
/// this donor-observed order.
pub struct UaiMediaFetchBatchPlan {
    requests: Vec<UaiMediaFetchPlan>,
    batch_digest: [u8; 32],
}

impl UaiMediaFetchBatchPlan {
    fn try_new(requests: Vec<UaiMediaFetchPlan>) -> ProviderResult<Self> {
        let first = requests
            .first()
            .ok_or_else(|| protocol_drift("UAI media fetch batch contains no request authority"))?;
        if requests.len() > MAX_MEDIA_SOURCES {
            return Err(invalid_response(
                "UAI media fetch batch exceeds the source limit",
            ));
        }
        let mut request_digests = BTreeSet::new();
        for request in &requests {
            if request.task_id != first.task_id
                || request.remote_task_id != first.remote_task_id
                || request.remote_question_id != first.remote_question_id
                || request.position != first.position
                || request.question_fingerprint != first.question_fingerprint
                || !request_digests.insert(request.request_digest)
            {
                return Err(protocol_drift(
                    "UAI media fetch batch contains a foreign or duplicate request",
                ));
            }
        }
        let batch_digest = media_fetch_batch_digest(&requests)?;
        if batch_digest == [0; 32] {
            return Err(invalid_response("UAI media fetch batch digest is invalid"));
        }
        Ok(Self {
            requests,
            batch_digest,
        })
    }

    pub fn task_id(&self) -> &str {
        self.requests[0].task_id()
    }

    pub fn remote_task_id(&self) -> &str {
        self.requests[0].remote_task_id()
    }

    pub fn remote_question_id(&self) -> &str {
        self.requests[0].remote_question_id()
    }

    pub const fn requests(&self) -> &[UaiMediaFetchPlan] {
        self.requests.as_slice()
    }

    pub const fn request_count(&self) -> usize {
        self.requests.len()
    }

    pub const fn batch_digest(&self) -> [u8; 32] {
        self.batch_digest
    }

    pub fn into_requests(self) -> Vec<UaiMediaFetchPlan> {
        self.requests
    }

    /// Consumes one complete successful downloader result for every frozen
    /// request, preserving the exact donor-observed order.
    ///
    /// This does not define behavior for failed or skipped sources; those
    /// outcomes require an explicit shared attempt policy and cannot be
    /// represented by silently omitting a slot here.
    ///
    /// # Errors
    ///
    /// Rejects missing, extra, duplicate, reordered or foreign responses.
    pub fn accept_complete_responses(
        self,
        responses: Vec<UaiMediaFetchResponse>,
    ) -> ProviderResult<UaiMediaFetchBatchResponseSet> {
        if responses.len() != self.requests.len()
            || responses
                .iter()
                .zip(&self.requests)
                .any(|(response, request)| response.plan.request_digest != request.request_digest)
        {
            return Err(protocol_drift(
                "UAI media fetch batch responses are missing, reordered or foreign",
            ));
        }
        let response_set_digest =
            media_fetch_batch_response_set_digest(self.batch_digest, &responses)?;
        if response_set_digest == [0; 32] {
            return Err(invalid_response(
                "UAI media fetch batch response-set digest is invalid",
            ));
        }
        Ok(UaiMediaFetchBatchResponseSet {
            batch: self,
            responses,
            response_set_digest,
        })
    }
}

impl fmt::Debug for UaiMediaFetchBatchPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMediaFetchBatchPlan")
            .field("binding", &"[REDACTED]")
            .field("request_count", &self.requests.len())
            .field("batch_digest", &self.batch_digest)
            .finish_non_exhaustive()
    }
}

/// Complete ordered successful response set for one immutable media batch.
pub struct UaiMediaFetchBatchResponseSet {
    batch: UaiMediaFetchBatchPlan,
    responses: Vec<UaiMediaFetchResponse>,
    response_set_digest: [u8; 32],
}

impl UaiMediaFetchBatchResponseSet {
    pub const fn batch(&self) -> &UaiMediaFetchBatchPlan {
        &self.batch
    }

    pub const fn responses(&self) -> &[UaiMediaFetchResponse] {
        self.responses.as_slice()
    }

    pub const fn response_count(&self) -> usize {
        self.responses.len()
    }

    pub const fn response_set_digest(&self) -> [u8; 32] {
        self.response_set_digest
    }

    pub fn into_parts(self) -> (UaiMediaFetchBatchPlan, Vec<UaiMediaFetchResponse>) {
        (self.batch, self.responses)
    }
}

impl fmt::Debug for UaiMediaFetchBatchResponseSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMediaFetchBatchResponseSet")
            .field("binding", &"[REDACTED]")
            .field("response_count", &self.responses.len())
            .field("batch_digest", &self.batch.batch_digest)
            .field("response_set_digest", &self.response_set_digest)
            .finish_non_exhaustive()
    }
}

/// Zeroizing media bytes accepted only for one exact immutable fetch plan.
///
/// The response digest binds the byte digest and length to the request digest,
/// so downstream transcription cannot substitute bytes fetched for another
/// Task, Question, attachment or route.
pub struct UaiMediaFetchResponse {
    plan: UaiMediaFetchPlan,
    body: SecretValue,
    body_len: u64,
    body_digest: [u8; 32],
    response_digest: [u8; 32],
}

impl UaiMediaFetchResponse {
    pub const fn plan(&self) -> &UaiMediaFetchPlan {
        &self.plan
    }

    pub const fn body_len(&self) -> u64 {
        self.body_len
    }

    pub const fn body_digest(&self) -> [u8; 32] {
        self.body_digest
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    /// Explicitly exposes media bytes to the already authorized bounded
    /// transcription or answer-resolution adapter.
    pub fn expose_body(&self) -> &[u8] {
        self.body.expose_secret()
    }

    pub fn into_parts(self) -> (UaiMediaFetchPlan, SecretValue) {
        (self.plan, self.body)
    }

    /// Parses one donor-evidenced downloaded VTT/SRT response into a
    /// response-bound zeroizing transcript.
    ///
    /// Audio/video transcription remains a shared model-execution boundary;
    /// this method accepts only a source already marked as a subtitle in the
    /// immutable Question manifest.
    ///
    /// # Errors
    ///
    /// Rejects a non-subtitle response, invalid UTF-8, oversized subtitle
    /// source or an empty normalized transcript.
    pub fn parse_subtitle(self) -> ProviderResult<UaiMediaSubtitleTranscript> {
        if !self.plan.subtitle || self.plan.kind != QuestionAttachmentKind::File {
            return Err(protocol_drift(
                "UAI media response is not an audited subtitle source",
            ));
        }
        let document = std::str::from_utf8(self.body.expose_secret())
            .map_err(|_| invalid_response("UAI downloaded subtitle is not valid UTF-8"))?;
        let transcript = parse_subtitle_transcript(document)?;
        if transcript.is_empty() {
            return Err(invalid_response(
                "UAI downloaded subtitle contains no transcript text",
            ));
        }
        let transcript_digest =
            media_subtitle_transcript_digest(self.response_digest, transcript.as_bytes());
        if transcript_digest == [0; 32] {
            return Err(invalid_response(
                "UAI downloaded subtitle transcript digest is invalid",
            ));
        }
        Ok(UaiMediaSubtitleTranscript {
            response: self,
            transcript: SecretString::new(transcript),
            transcript_digest,
        })
    }
}

impl fmt::Debug for UaiMediaFetchResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMediaFetchResponse")
            .field("binding", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .field("body_len", &self.body_len)
            .field("body_digest", &self.body_digest)
            .field("response_digest", &self.response_digest)
            .finish_non_exhaustive()
    }
}

/// Normalized VTT/SRT text bound to one exact accepted subtitle response.
pub struct UaiMediaSubtitleTranscript {
    response: UaiMediaFetchResponse,
    transcript: SecretString,
    transcript_digest: [u8; 32],
}

impl UaiMediaSubtitleTranscript {
    pub const fn response(&self) -> &UaiMediaFetchResponse {
        &self.response
    }

    pub fn expose_transcript(&self) -> &str {
        self.transcript.expose_secret()
    }

    pub const fn transcript_digest(&self) -> [u8; 32] {
        self.transcript_digest
    }

    pub fn into_parts(self) -> (UaiMediaFetchResponse, SecretString) {
        (self.response, self.transcript)
    }
}

impl fmt::Debug for UaiMediaSubtitleTranscript {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMediaSubtitleTranscript")
            .field("binding", &"[REDACTED]")
            .field("transcript", &"[REDACTED]")
            .field("response_digest", &self.response.response_digest)
            .field("transcript_digest", &self.transcript_digest)
            .finish_non_exhaustive()
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "each immutable Question/media/request fact is an independent digest authority"
)]
fn media_fetch_request_digest(
    task_id: &str,
    remote_task_id: &str,
    remote_question_id: &str,
    position: u32,
    question_fingerprint: &str,
    attachment_id: &str,
    kind: QuestionAttachmentKind,
    subtitle: bool,
    url: &str,
    credential_scope: UaiMediaFetchCredentialScope,
    max_response_bytes: u64,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    for field in [
        b"asterism:uai:media-fetch:v1".as_slice(),
        task_id.as_bytes(),
        remote_task_id.as_bytes(),
        remote_question_id.as_bytes(),
        question_fingerprint.as_bytes(),
        attachment_id.as_bytes(),
        attachment_kind_name(kind).as_bytes(),
        url.as_bytes(),
    ] {
        digest.update(field);
        digest.update(b"\0");
    }
    digest.update(position.to_be_bytes());
    digest.update([u8::from(subtitle)]);
    digest.update([match credential_scope {
        UaiMediaFetchCredentialScope::UcontentSession => 1,
        UaiMediaFetchCredentialScope::Anonymous => 0,
    }]);
    digest.update(max_response_bytes.to_be_bytes());
    digest.finalize().into()
}

fn media_fetch_response_digest(
    request_digest: [u8; 32],
    body_digest: [u8; 32],
    body_len: u64,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:media-fetch-response:v1\0");
    digest.update(request_digest);
    digest.update(body_digest);
    digest.update(body_len.to_be_bytes());
    digest.finalize().into()
}

fn media_fetch_batch_digest(requests: &[UaiMediaFetchPlan]) -> ProviderResult<[u8; 32]> {
    let request_count = u64::try_from(requests.len())
        .map_err(|_| invalid_response("UAI media fetch batch count is invalid"))?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:media-fetch-batch:v1\0");
    digest.update(request_count.to_be_bytes());
    for request in requests {
        digest.update(request.request_digest);
    }
    Ok(digest.finalize().into())
}

fn media_fetch_batch_response_set_digest(
    batch_digest: [u8; 32],
    responses: &[UaiMediaFetchResponse],
) -> ProviderResult<[u8; 32]> {
    let response_count = u64::try_from(responses.len())
        .map_err(|_| invalid_response("UAI media fetch response-set count is invalid"))?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:media-fetch-response-set:v1\0");
    digest.update(batch_digest);
    digest.update(response_count.to_be_bytes());
    for response in responses {
        digest.update(response.response_digest);
    }
    Ok(digest.finalize().into())
}

fn media_subtitle_transcript_digest(response_digest: [u8; 32], transcript: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:media-subtitle-transcript:v1\0");
    digest.update(response_digest);
    digest.update(transcript);
    digest.finalize().into()
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
struct ArtifactSetWireRef<'a> {
    schema: &'static str,
    task_id: &'a str,
    remote_task_id: &'a str,
    questions: &'a [ArtifactSetQuestionWireRef<'a>],
}

#[derive(Serialize)]
struct ArtifactSetQuestionWireRef<'a> {
    remote_question_id: &'a str,
    position: u32,
    question_fingerprint: &'a str,
    media_sources: Vec<MediaSourceWireRef<'a>>,
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
struct ArtifactSetWire {
    schema: String,
    task_id: String,
    remote_task_id: String,
    questions: Vec<ArtifactSetQuestionWire>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ArtifactSetQuestionWire {
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
        parsed_question_with_media_url(
            task_id,
            "https://media.example.edu/listening.mp3#duration=10",
        )
    }

    fn parsed_question_with_media_url(
        task_id: TaskId,
        audio_url: &str,
    ) -> (ParsedUaiQuestion, Question) {
        let parsed = parse_question_entry(
            &json!({
                "id": "9001",
                "content": {
                    "type": "short_answer",
                    "direction": {"text": "Summarize the recording"},
                    "contents": [{
                        "name": "Listening.mp3",
                        "path": audio_url,
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

    fn plain_question(task_id: TaskId, position: u32) -> (ParsedUaiQuestion, Question) {
        let parsed = parse_question_entry(
            &json!({
                "id": "9002",
                "content": {
                    "type": "short_answer",
                    "direction": {"text": "Summarize the text"},
                    "children": [{
                        "type": "basic",
                        "replyType": "text-area",
                        "quesText": "What happened next?"
                    }]
                }
            }),
            position,
            "short_answer",
            REMOTE_TASK_ID,
        )
        .unwrap();
        let question = parsed.to_question(task_id).unwrap();
        (parsed, question)
    }

    fn accepted_batch_responses(
        artifact: &UaiQuestionArtifactSet,
        changed_last: bool,
    ) -> Vec<UaiMediaFetchResponse> {
        let requests = artifact
            .prepare_media_fetch_batch("9001")
            .unwrap()
            .into_requests();
        let last = requests.len().saturating_sub(1);
        requests
            .into_iter()
            .enumerate()
            .map(|(index, request)| {
                let url = request.expose_url().to_owned();
                let body = if changed_last && index == last {
                    vec![b'z']
                } else {
                    vec![b'a' + u8::try_from(index).unwrap()]
                };
                request
                    .accept_response(200, &url, 0, SecretValue::new(body))
                    .unwrap()
            })
            .collect()
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

    #[test]
    fn media_artifact_set_binds_every_question_and_only_exposes_bound_routes() {
        let task_id = TaskId::new();
        let (media_parsed, media_question) = parsed_question(task_id);
        let (plain_parsed, plain_question) = plain_question(task_id, 2);
        let parsed = vec![media_parsed, plain_parsed];
        let questions = vec![media_question, plain_question];
        let artifact =
            UaiQuestionArtifactSet::from_parsed_questions(&parsed, &questions, REMOTE_TASK_ID)
                .unwrap()
                .unwrap();
        assert_eq!(artifact.question_count(), 2);
        assert_eq!(
            artifact.media_sources_for_question("9001").unwrap().len(),
            2
        );
        assert!(
            artifact
                .media_sources_for_question("9002")
                .unwrap()
                .is_empty()
        );
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let decoded =
            UaiQuestionArtifactSet::decode_bound(&value, digest, REMOTE_TASK_ID, &questions)
                .unwrap();
        assert_eq!(decoded.question_count(), 2);
        assert!(!format!("{decoded:?}").contains("media.example.edu"));
        assert!(
            UaiQuestionArtifactSet::decode_bound(&value, [8; 32], REMOTE_TASK_ID, &questions,)
                .is_err()
        );
        let mut reordered = questions.clone();
        reordered.swap(0, 1);
        assert!(
            UaiQuestionArtifactSet::decode_bound(&value, digest, REMOTE_TASK_ID, &reordered,)
                .is_err()
        );
    }

    #[test]
    fn media_fetch_plan_binds_external_route_without_credentials_or_redirects() {
        let task_id = TaskId::new();
        let task_id_text = task_id.to_string();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let sources = artifact.media_sources_for_question("9001").unwrap();
        let audio_id = sources[0].attachment_id().to_owned();
        let subtitle_id = sources[1].attachment_id().to_owned();

        let audio = artifact.prepare_media_fetch("9001", &audio_id).unwrap();
        assert_eq!(audio.method(), "GET");
        assert_eq!(audio.task_id(), task_id_text);
        assert_eq!(audio.remote_task_id(), REMOTE_TASK_ID);
        assert_eq!(audio.remote_question_id(), "9001");
        assert_eq!(audio.position(), 1);
        assert_eq!(
            audio.question_fingerprint(),
            question.content_fingerprint().unwrap().to_string()
        );
        assert_eq!(audio.attachment_id(), audio_id);
        assert_eq!(audio.kind(), QuestionAttachmentKind::Audio);
        assert!(!audio.is_subtitle());
        assert_eq!(
            audio.credential_scope(),
            UaiMediaFetchCredentialScope::Anonymous
        );
        assert_eq!(audio.max_response_bytes(), MAX_AUDIO_RESPONSE_BYTES);
        assert_ne!(audio.request_digest(), [0; 32]);
        assert!(!audio.permits_redirects());
        audio.validate_final_url(audio.expose_url()).unwrap();
        assert!(
            audio
                .validate_final_url("https://cdn.example.edu/listening.mp3")
                .is_err()
        );
        let debug = format!("{audio:?}");
        assert!(!debug.contains("media.example.edu"));
        assert!(!debug.contains(&task_id_text));
        assert!(!debug.contains(&audio_id));

        let subtitle = artifact.prepare_media_fetch("9001", &subtitle_id).unwrap();
        assert_eq!(subtitle.kind(), QuestionAttachmentKind::File);
        assert!(subtitle.is_subtitle());
        assert_eq!(subtitle.max_response_bytes(), MAX_SUBTITLE_RESPONSE_BYTES);
        assert_ne!(subtitle.request_digest(), audio.request_digest());
        assert!(artifact.prepare_media_fetch("9002", &audio_id).is_err());
        assert!(artifact.prepare_media_fetch("9001", "foreign").is_err());

        let (video_parsed, video_question) = parsed_question_with_media_url(
            TaskId::new(),
            "https://media.example.edu/listening.mp4",
        );
        let video_artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[video_parsed],
            std::slice::from_ref(&video_question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let video_source = &video_artifact.media_sources_for_question("9001").unwrap()[0];
        let video = video_artifact
            .prepare_media_fetch("9001", video_source.attachment_id())
            .unwrap();
        assert_eq!(video.kind(), QuestionAttachmentKind::Video);
        assert_eq!(video.max_response_bytes(), MAX_VIDEO_RESPONSE_BYTES);
    }

    #[test]
    fn media_fetch_plan_scopes_session_only_to_exact_ucontent_https_origin() {
        let task_id = TaskId::new();
        let (parsed, question) = parsed_question_with_media_url(
            task_id,
            "https://ucontent.unipus.cn/media/listening.mp3",
        );
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let source = &artifact.media_sources_for_question("9001").unwrap()[0];
        let plan = artifact
            .prepare_media_fetch("9001", source.attachment_id())
            .unwrap();

        assert_eq!(
            plan.credential_scope(),
            UaiMediaFetchCredentialScope::UcontentSession
        );
        assert_eq!(plan.max_response_bytes(), MAX_AUDIO_RESPONSE_BYTES);
        assert!(
            plan.validate_final_url("https://ucontent.unipus.cn.evil.example/media/listening.mp3")
                .is_err()
        );

        let (custom_port_parsed, custom_port_question) = parsed_question_with_media_url(
            TaskId::new(),
            "https://ucontent.unipus.cn:444/media/listening.mp3",
        );
        let custom_port_artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[custom_port_parsed],
            std::slice::from_ref(&custom_port_question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let custom_port_source = &custom_port_artifact
            .media_sources_for_question("9001")
            .unwrap()[0];
        let custom_port_plan = custom_port_artifact
            .prepare_media_fetch("9001", custom_port_source.attachment_id())
            .unwrap();
        assert_eq!(
            custom_port_plan.credential_scope(),
            UaiMediaFetchCredentialScope::Anonymous
        );
    }

    #[test]
    fn media_fetch_batch_freezes_complete_donor_source_order() {
        let task_id = TaskId::new();
        let task_id_text = task_id.to_string();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let batch = artifact.prepare_media_fetch_batch("9001").unwrap();

        assert_eq!(batch.task_id(), task_id_text);
        assert_eq!(batch.remote_task_id(), REMOTE_TASK_ID);
        assert_eq!(batch.remote_question_id(), "9001");
        assert_eq!(batch.request_count(), 2);
        assert_eq!(batch.requests()[0].kind(), QuestionAttachmentKind::Audio);
        assert_eq!(batch.requests()[1].kind(), QuestionAttachmentKind::File);
        assert!(batch.requests()[1].is_subtitle());
        assert_ne!(batch.batch_digest(), [0; 32]);
        assert_eq!(
            batch.batch_digest(),
            artifact
                .prepare_media_fetch_batch("9001")
                .unwrap()
                .batch_digest()
        );
        let debug = format!("{batch:?}");
        assert!(!debug.contains(&task_id_text));
        assert!(!debug.contains("media.example.edu"));

        let mut reversed = artifact
            .prepare_media_fetch_batch("9001")
            .unwrap()
            .into_requests();
        reversed.reverse();
        let reversed = UaiMediaFetchBatchPlan::try_new(reversed).unwrap();
        assert_ne!(batch.batch_digest(), reversed.batch_digest());

        let duplicate_id = artifact.media_sources_for_question("9001").unwrap()[0]
            .attachment_id()
            .to_owned();
        assert!(
            UaiMediaFetchBatchPlan::try_new(vec![
                artifact.prepare_media_fetch("9001", &duplicate_id).unwrap(),
                artifact.prepare_media_fetch("9001", &duplicate_id).unwrap(),
            ])
            .is_err()
        );
        assert!(artifact.prepare_media_fetch_batch("9002").is_err());
    }

    #[test]
    fn media_fetch_batch_response_set_rejects_missing_duplicate_and_reordered_slots() {
        let task_id = TaskId::new();
        let task_id_text = task_id.to_string();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();

        let response_set = artifact
            .prepare_media_fetch_batch("9001")
            .unwrap()
            .accept_complete_responses(accepted_batch_responses(&artifact, false))
            .unwrap();
        assert_eq!(response_set.response_count(), 2);
        assert_eq!(
            response_set.batch().request_count(),
            response_set.responses().len()
        );
        assert_ne!(response_set.response_set_digest(), [0; 32]);
        let stable_digest = response_set.response_set_digest();
        assert_eq!(
            stable_digest,
            artifact
                .prepare_media_fetch_batch("9001")
                .unwrap()
                .accept_complete_responses(accepted_batch_responses(&artifact, false))
                .unwrap()
                .response_set_digest()
        );
        assert_ne!(
            stable_digest,
            artifact
                .prepare_media_fetch_batch("9001")
                .unwrap()
                .accept_complete_responses(accepted_batch_responses(&artifact, true))
                .unwrap()
                .response_set_digest()
        );
        let debug = format!("{response_set:?}");
        assert!(!debug.contains(&task_id_text));
        assert!(!debug.contains("media.example.edu"));

        let mut missing = accepted_batch_responses(&artifact, false);
        missing.pop();
        assert!(
            artifact
                .prepare_media_fetch_batch("9001")
                .unwrap()
                .accept_complete_responses(missing)
                .is_err()
        );
        let mut reordered = accepted_batch_responses(&artifact, false);
        reordered.reverse();
        assert!(
            artifact
                .prepare_media_fetch_batch("9001")
                .unwrap()
                .accept_complete_responses(reordered)
                .is_err()
        );
        let first = artifact.media_sources_for_question("9001").unwrap()[0]
            .attachment_id()
            .to_owned();
        let duplicate = (0..2)
            .map(|_| {
                let request = artifact.prepare_media_fetch("9001", &first).unwrap();
                let url = request.expose_url().to_owned();
                request
                    .accept_response(200, &url, 0, SecretValue::new(vec![b'a']))
                    .unwrap()
            })
            .collect();
        assert!(
            artifact
                .prepare_media_fetch_batch("9001")
                .unwrap()
                .accept_complete_responses(duplicate)
                .is_err()
        );
    }

    #[test]
    fn media_fetch_response_binds_secret_bytes_to_the_exact_request() {
        let task_id = TaskId::new();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let audio_id = artifact.media_sources_for_question("9001").unwrap()[0]
            .attachment_id()
            .to_owned();
        let plan = artifact.prepare_media_fetch("9001", &audio_id).unwrap();
        let request_digest = plan.request_digest();
        let url = plan.expose_url().to_owned();
        let response = plan
            .accept_response(200, &url, 0, SecretValue::new(b"synthetic audio".to_vec()))
            .unwrap();

        assert_eq!(response.plan().request_digest(), request_digest);
        assert_eq!(response.body_len(), 15);
        assert_eq!(response.expose_body(), b"synthetic audio");
        assert_ne!(response.body_digest(), [0; 32]);
        assert_ne!(response.response_digest(), [0; 32]);
        let debug = format!("{response:?}");
        assert!(!debug.contains("synthetic audio"));
        assert!(!debug.contains("media.example.edu"));
        assert!(!debug.contains(&audio_id));

        let duplicate = artifact
            .prepare_media_fetch("9001", &audio_id)
            .unwrap()
            .accept_response(200, &url, 0, SecretValue::new(b"synthetic audio".to_vec()))
            .unwrap();
        let changed = artifact
            .prepare_media_fetch("9001", &audio_id)
            .unwrap()
            .accept_response(200, &url, 0, SecretValue::new(b"different audio".to_vec()))
            .unwrap();
        assert_eq!(response.response_digest(), duplicate.response_digest());
        assert_ne!(response.response_digest(), changed.response_digest());

        let (accepted_plan, accepted_body) = response.into_parts();
        assert_eq!(accepted_plan.request_digest(), request_digest);
        assert_eq!(accepted_body.expose_secret(), b"synthetic audio");
    }

    #[test]
    fn media_fetch_response_rejects_status_route_empty_and_size_drift() {
        let task_id = TaskId::new();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let sources = artifact.media_sources_for_question("9001").unwrap();
        let audio_id = sources[0].attachment_id().to_owned();
        let subtitle_id = sources[1].attachment_id().to_owned();
        let audio_url = sources[0].expose_url().to_owned();
        let subtitle_url = sources[1].expose_url().to_owned();

        assert!(
            artifact
                .prepare_media_fetch("9001", &audio_id)
                .unwrap()
                .accept_response(206, &audio_url, 0, SecretValue::new(vec![1]))
                .is_err()
        );
        assert!(
            artifact
                .prepare_media_fetch("9001", &audio_id)
                .unwrap()
                .accept_response(200, &audio_url, 1, SecretValue::new(vec![1]))
                .is_err()
        );
        assert!(
            artifact
                .prepare_media_fetch("9001", &audio_id)
                .unwrap()
                .accept_response(
                    200,
                    "https://cdn.example.edu/listening.mp3",
                    0,
                    SecretValue::new(vec![1]),
                )
                .is_err()
        );
        assert!(
            artifact
                .prepare_media_fetch("9001", &audio_id)
                .unwrap()
                .accept_response(200, &audio_url, 0, SecretValue::new(Vec::new()))
                .is_err()
        );
        assert!(
            artifact
                .prepare_media_fetch("9001", &subtitle_id)
                .unwrap()
                .accept_response(
                    200,
                    &subtitle_url,
                    0,
                    SecretValue::new(vec![
                        b'x';
                        usize::try_from(MAX_SUBTITLE_RESPONSE_BYTES).unwrap()
                            + 1
                    ]),
                )
                .is_err()
        );
    }

    #[test]
    fn downloaded_subtitle_normalizes_into_a_response_bound_secret_transcript() {
        let task_id = TaskId::new();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let source = &artifact.media_sources_for_question("9001").unwrap()[1];
        let plan = artifact
            .prepare_media_fetch("9001", source.attachment_id())
            .unwrap();
        let url = plan.expose_url().to_owned();
        let response = plan
            .accept_response(
                200,
                &url,
                0,
                SecretValue::new(
                    b"WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\n<p>Hello</p>\n\n2\n00:00:03.000 --> 00:00:04.000\nWorld"
                        .to_vec(),
                ),
            )
            .unwrap();
        let response_digest = response.response_digest();
        let transcript = response.parse_subtitle().unwrap();

        assert_eq!(transcript.expose_transcript(), "Hello\nWorld");
        assert_eq!(transcript.response().response_digest(), response_digest);
        assert_ne!(transcript.transcript_digest(), [0; 32]);
        let debug = format!("{transcript:?}");
        assert!(!debug.contains("Hello"));
        assert!(!debug.contains("media.example.edu"));
        let (response, text) = transcript.into_parts();
        assert!(response.plan().is_subtitle());
        assert_eq!(text.expose_secret(), "Hello\nWorld");
    }

    #[test]
    fn downloaded_subtitle_rejects_foreign_kind_encoding_empty_and_source_overflow() {
        let task_id = TaskId::new();
        let (parsed, question) = parsed_question(task_id);
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap();
        let sources = artifact.media_sources_for_question("9001").unwrap();
        let audio_id = sources[0].attachment_id().to_owned();
        let audio_url = sources[0].expose_url().to_owned();
        let subtitle_id = sources[1].attachment_id().to_owned();
        let subtitle_url = sources[1].expose_url().to_owned();

        assert!(
            artifact
                .prepare_media_fetch("9001", &audio_id)
                .unwrap()
                .accept_response(200, &audio_url, 0, SecretValue::new(b"audio".to_vec()))
                .unwrap()
                .parse_subtitle()
                .is_err()
        );
        for body in [
            SecretValue::new(vec![0xff, 0xfe]),
            SecretValue::new(b"WEBVTT\n1\n00:00:01.000 --> 00:00:02.000\n".to_vec()),
            SecretValue::new("text\n".repeat(300_000).into_bytes()),
        ] {
            assert!(
                artifact
                    .prepare_media_fetch("9001", &subtitle_id)
                    .unwrap()
                    .accept_response(200, &subtitle_url, 0, body)
                    .unwrap()
                    .parse_subtitle()
                    .is_err()
            );
        }
    }

    #[test]
    fn artifact_free_set_returns_none_and_unknown_set_fields_fail_closed() {
        let task_id = TaskId::new();
        let (parsed, question) = plain_question(task_id, 1);
        assert!(
            UaiQuestionArtifactSet::from_parsed_questions(&[parsed], &[question], REMOTE_TASK_ID,)
                .unwrap()
                .is_none()
        );

        let (parsed, question) = parsed_question(task_id);
        let encoded = UaiQuestionArtifactSet::from_parsed_questions(
            &[parsed],
            std::slice::from_ref(&question),
            REMOTE_TASK_ID,
        )
        .unwrap()
        .unwrap()
        .encode()
        .unwrap();
        let value = encoded.into_secret_value();
        let mut wire: Value = serde_json::from_slice(value.expose_secret()).unwrap();
        wire["questions"][0]["unexpected"] = json!(true);
        let changed = SecretValue::new(serde_json::to_vec(&wire).unwrap());
        assert!(
            UaiQuestionArtifactSet::decode_bound(
                &changed,
                Sha256::digest(changed.expose_secret()).into(),
                REMOTE_TASK_ID,
                &[question],
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
