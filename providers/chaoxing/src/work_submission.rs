use std::{collections::BTreeSet, fmt};

use asterism_domain::{Question, SubmissionDraft, TaskId, Timestamp};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::submission_support::ChaoxingSubmissionForm;

const MAX_WORK_STATE_BYTES: usize = 128 * 1_024;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_REMOTE_TASK_ID_BYTES: usize = 640;
const MAX_FORM_FIELDS: usize = 2_048;
const MAX_FORM_FIELD_BYTES: usize = 32 * 1_024;

pub(crate) const CHAOXING_WORK_QUESTION_ARTIFACT_TYPE: &str = "chaoxing.work-question-snapshot.v1";
pub(crate) const CHAOXING_WORK_QUESTIONS_READY_PHASE: &str = "chaoxing.work-questions-ready";
pub(crate) const CHAOXING_WORK_FINAL_OPERATION: &str = "chaoxing.work-final-submit.v1";
pub(crate) const CHAOXING_WORK_CONTINUATION_TTL_SECONDS: u64 = 30 * 60;

/// Minimal encrypted binding from one read-only Work Question snapshot to its
/// later durable final-submit operation. No answer or remote editor material is
/// retained; the editor is freshly rediscovered before the command is frozen.
pub(crate) struct ChaoxingWorkQuestionArtifact {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    question_count: u32,
    question_set_digest: Zeroizing<String>,
    question_bindings: Vec<WorkQuestionBinding>,
}

impl ChaoxingWorkQuestionArtifact {
    pub(crate) fn from_questions(
        task_id: TaskId,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Self> {
        validate_work_task_id(remote_task_id)?;
        let (question_count, question_set_digest, question_bindings) =
            derive_question_set_binding(task_id, questions)?;
        Ok(Self {
            task_id: Zeroizing::new(task_id.to_string()),
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            question_count,
            question_set_digest: Zeroizing::new(question_set_digest),
            question_bindings,
        })
    }

    pub(crate) fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        draft: &SubmissionDraft,
        remote_task_id: &str,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        validate_encoded_state(bytes, expected_digest)?;
        let mut wire: WorkQuestionArtifactWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Chaoxing Work artifact schema is invalid"))?;
        validate_work_task_id(remote_task_id)?;
        if wire.schema != CHAOXING_WORK_QUESTION_ARTIFACT_TYPE
            || draft.validate().is_err()
            || wire.task_id != draft.task_id.to_string()
            || wire.remote_task_id != remote_task_id
        {
            return Err(protocol_drift(
                "Chaoxing Work artifact is stale, foreign, or malformed",
            ));
        }
        let question_count = validate_question_binding_wire(&wire.question_bindings)?;
        let question_set_digest = derive_wire_question_set_digest(&wire.question_bindings)?;
        if wire.question_count != question_count
            || wire.question_set_digest != question_set_digest
            || wire.question_count != draft.answer_coverage.total_question_count
        {
            return Err(protocol_drift(
                "Chaoxing Work artifact no longer matches the immutable Draft",
            ));
        }
        validate_draft_subset(draft, &wire.question_bindings)?;
        let question_bindings = std::mem::take(&mut wire.question_bindings)
            .into_iter()
            .map(WorkQuestionBinding::from)
            .collect();
        Ok(Self {
            task_id: Zeroizing::new(std::mem::take(&mut wire.task_id)),
            remote_task_id: Zeroizing::new(std::mem::take(&mut wire.remote_task_id)),
            question_count,
            question_set_digest: Zeroizing::new(question_set_digest),
            question_bindings,
        })
    }

    pub(crate) fn encode(&self) -> ProviderResult<EncodedChaoxingWorkState> {
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&WorkQuestionArtifactWireRef {
                schema: CHAOXING_WORK_QUESTION_ARTIFACT_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                question_count: self.question_count,
                question_set_digest: &self.question_set_digest,
                question_bindings: self
                    .question_bindings
                    .iter()
                    .map(WorkQuestionBindingWireRef::from)
                    .collect(),
            })
            .map_err(|_| invalid_response("Chaoxing Work artifact could not be encoded"))?,
        );
        encoded_state(&mut encoded)
    }

    pub(crate) fn bind_submission_plan(
        &self,
        plan: &mut crate::ChaoxingSubmissionPlan,
    ) -> ProviderResult<()> {
        plan.bind_full_question_partition(
            self.question_bindings
                .iter()
                .map(|binding| {
                    (
                        binding.remote_id.as_str().to_owned(),
                        binding.type_code.as_str().to_owned(),
                    )
                })
                .collect(),
        )
    }
}

impl fmt::Debug for ChaoxingWorkQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingWorkQuestionArtifact")
            .field("binding", &"[REDACTED]")
            .field("question_count", &self.question_count)
            .finish_non_exhaustive()
    }
}

/// Exact in-memory Work POST command whose digest is persisted by Core before
/// Native HTTP dispatch. Dynamic editor fields and answer values are redacted
/// and zeroized when the command leaves scope.
pub struct ChaoxingWorkSubmissionCommand {
    remote_task_id: Zeroizing<String>,
    user_id: Zeroizing<String>,
    referer: Zeroizing<String>,
    fields: Vec<(String, String)>,
    request_digest: [u8; 32],
}

impl ChaoxingWorkSubmissionCommand {
    pub(crate) fn try_new(
        remote_task_id: &str,
        user_id: &str,
        referer: &Url,
        form: &ChaoxingSubmissionForm,
    ) -> ProviderResult<Self> {
        validate_work_task_id(remote_task_id)?;
        if !valid_component(user_id)
            || referer.as_str().len() > 16 * 1_024
            || referer.scheme() != "https"
            || referer.host_str() != Some("mooc1.chaoxing.com")
            || !referer.path().eq_ignore_ascii_case("/mooc-ans/work/dowork")
            || !referer.username().is_empty()
            || referer.password().is_some()
            || referer.port().is_some()
            || referer.fragment().is_some()
            || form.fields().is_empty()
            || form.fields().len() > MAX_FORM_FIELDS
        {
            return Err(protocol_drift(
                "Chaoxing Work submission command is malformed",
            ));
        }
        let mut names = BTreeSet::new();
        if form.fields().iter().any(|(name, value)| {
            name.is_empty()
                || name.len() > MAX_REMOTE_COMPONENT_BYTES
                || !names.insert(name.as_str())
                || value.len() > MAX_FORM_FIELD_BYTES
        }) {
            return Err(protocol_drift(
                "Chaoxing Work submission command fields are malformed",
            ));
        }
        let mut command = Self {
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            user_id: Zeroizing::new(user_id.to_owned()),
            referer: Zeroizing::new(referer.as_str().to_owned()),
            fields: form.fields().to_vec(),
            request_digest: [0; 32],
        };
        command.request_digest = command.derive_request_digest()?;
        Ok(command)
    }

    fn derive_request_digest(&self) -> ProviderResult<[u8; 32]> {
        let mut hasher = Sha256::new();
        hasher.update(b"asterism.chaoxing.work-final-submit.v1\0");
        for value in [
            "POST",
            "https://mooc1.chaoxing.com/mooc-ans/work/addStudentWorkNew",
            "https://mooc1.chaoxing.com",
            "XMLHttpRequest",
            "application/x-www-form-urlencoded; charset=UTF-8",
            self.remote_task_id.as_str(),
            self.user_id.as_str(),
            self.referer.as_str(),
        ] {
            hash_bounded_component(&mut hasher, value)?;
        }
        let field_count = u64::try_from(self.fields.len())
            .map_err(|_| invalid_response("Chaoxing Work field count is unbounded"))?;
        hasher.update(field_count.to_be_bytes());
        for (name, value) in &self.fields {
            hash_bounded_component(&mut hasher, name)?;
            hash_bounded_component(&mut hasher, value)?;
        }
        Ok(hasher.finalize().into())
    }

    pub(crate) const fn operation_type() -> &'static str {
        CHAOXING_WORK_FINAL_OPERATION
    }

    pub(crate) const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub(crate) fn belongs_to_user(&self, user_id: &str) -> bool {
        self.user_id.as_str() == user_id
    }

    pub(crate) fn referer(&self) -> ProviderResult<Url> {
        Url::parse(&self.referer)
            .map_err(|_| protocol_drift("Chaoxing Work command Referer is invalid"))
    }

    pub(crate) fn fields(&self) -> &[(String, String)] {
        &self.fields
    }
}

impl fmt::Debug for ChaoxingWorkSubmissionCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingWorkSubmissionCommand")
            .field("binding", &"[REDACTED]")
            .field("field_count", &self.fields.len())
            .field("request_digest", &self.request_digest)
            .finish_non_exhaustive()
    }
}

impl Drop for ChaoxingWorkSubmissionCommand {
    fn drop(&mut self) {
        for (name, value) in &mut self.fields {
            name.zeroize();
            value.zeroize();
        }
        self.fields.clear();
    }
}

pub struct ChaoxingWorkSubmissionResponse {
    pub(crate) receipt: asterism_domain::SubmissionReceipt,
    pub(crate) response_digest: [u8; 32],
    pub(crate) received_at: Timestamp,
}

impl fmt::Debug for ChaoxingWorkSubmissionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingWorkSubmissionResponse")
            .field("receipt", &self.receipt)
            .field("response_digest", &self.response_digest)
            .field("received_at", &self.received_at)
            .finish()
    }
}

pub(crate) struct EncodedChaoxingWorkState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedChaoxingWorkState {
    pub(crate) const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub(crate) fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedChaoxingWorkState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedChaoxingWorkState")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

#[derive(Serialize)]
struct WorkQuestionArtifactWireRef<'a> {
    schema: &'static str,
    task_id: &'a str,
    remote_task_id: &'a str,
    question_count: u32,
    question_set_digest: &'a str,
    question_bindings: Vec<WorkQuestionBindingWireRef<'a>>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct WorkQuestionArtifactWire {
    schema: String,
    task_id: String,
    remote_task_id: String,
    question_count: u32,
    question_set_digest: String,
    question_bindings: Vec<WorkQuestionBindingWire>,
}

struct WorkQuestionBinding {
    remote_id: Zeroizing<String>,
    position: u32,
    type_code: Zeroizing<String>,
    fingerprint: Zeroizing<String>,
}

impl From<WorkQuestionBindingWire> for WorkQuestionBinding {
    fn from(mut value: WorkQuestionBindingWire) -> Self {
        Self {
            remote_id: Zeroizing::new(std::mem::take(&mut value.remote_id)),
            position: value.position,
            type_code: Zeroizing::new(std::mem::take(&mut value.type_code)),
            fingerprint: Zeroizing::new(std::mem::take(&mut value.fingerprint)),
        }
    }
}

#[derive(Serialize)]
struct WorkQuestionBindingWireRef<'a> {
    remote_id: &'a str,
    position: u32,
    type_code: &'a str,
    fingerprint: &'a str,
}

impl<'a> From<&'a WorkQuestionBinding> for WorkQuestionBindingWireRef<'a> {
    fn from(value: &'a WorkQuestionBinding) -> Self {
        Self {
            remote_id: &value.remote_id,
            position: value.position,
            type_code: &value.type_code,
            fingerprint: &value.fingerprint,
        }
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct WorkQuestionBindingWire {
    remote_id: String,
    position: u32,
    type_code: String,
    fingerprint: String,
}

fn derive_question_set_binding(
    task_id: TaskId,
    questions: &[Question],
) -> ProviderResult<(u32, String, Vec<WorkQuestionBinding>)> {
    let question_count = u32::try_from(questions.len())
        .map_err(|_| invalid_response("Chaoxing Work Question count is unbounded"))?;
    if question_count == 0 {
        return Err(protocol_drift("Chaoxing Work Question set is empty"));
    }
    let mut remote_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    let mut hasher = Sha256::new();
    let mut bindings = Vec::with_capacity(questions.len());
    for (index, question) in questions.iter().enumerate() {
        question
            .validate()
            .map_err(|_| invalid_response("Chaoxing Work Question is invalid"))?;
        let remote_id = question
            .remote_question_id
            .as_deref()
            .filter(|value| valid_component(value))
            .ok_or_else(|| protocol_drift("Chaoxing Work Question has no remote identity"))?;
        if question.task_id != task_id
            || !remote_ids.insert(remote_id)
            || !positions.insert(question.position)
            || usize::try_from(question.position).ok() != index.checked_add(1)
        {
            return Err(protocol_drift(
                "Chaoxing Work Question set has a stale or duplicate binding",
            ));
        }
        let fingerprint = question
            .content_fingerprint()
            .map_err(|_| invalid_response("Chaoxing Work Question fingerprint failed"))?
            .to_string();
        let type_code = question_type_code(question)?;
        hash_bounded_component(&mut hasher, remote_id)?;
        hasher.update(question.position.to_be_bytes());
        hash_bounded_component(&mut hasher, &type_code)?;
        hash_bounded_component(&mut hasher, &fingerprint)?;
        bindings.push(WorkQuestionBinding {
            remote_id: Zeroizing::new(remote_id.to_owned()),
            position: question.position,
            type_code: Zeroizing::new(type_code),
            fingerprint: Zeroizing::new(fingerprint),
        });
    }
    Ok((
        question_count,
        hex_digest(hasher.finalize().into()),
        bindings,
    ))
}

fn validate_question_binding_wire(bindings: &[WorkQuestionBindingWire]) -> ProviderResult<u32> {
    let question_count = u32::try_from(bindings.len())
        .map_err(|_| invalid_response("Chaoxing Work Question binding count is unbounded"))?;
    if question_count == 0 {
        return Err(protocol_drift(
            "Chaoxing Work Question binding set is empty",
        ));
    }
    let mut remote_ids = BTreeSet::new();
    for (index, binding) in bindings.iter().enumerate() {
        if !valid_component(&binding.remote_id)
            || !valid_type_code(&binding.type_code)
            || !valid_question_fingerprint(&binding.fingerprint)
            || !remote_ids.insert(binding.remote_id.as_str())
            || usize::try_from(binding.position).ok() != index.checked_add(1)
        {
            return Err(protocol_drift(
                "Chaoxing Work Question binding set is malformed",
            ));
        }
    }
    Ok(question_count)
}

fn derive_wire_question_set_digest(bindings: &[WorkQuestionBindingWire]) -> ProviderResult<String> {
    let mut hasher = Sha256::new();
    for binding in bindings {
        hash_bounded_component(&mut hasher, &binding.remote_id)?;
        hasher.update(binding.position.to_be_bytes());
        hash_bounded_component(&mut hasher, &binding.type_code)?;
        hash_bounded_component(&mut hasher, &binding.fingerprint)?;
    }
    Ok(hex_digest(hasher.finalize().into()))
}

fn validate_draft_subset(
    draft: &SubmissionDraft,
    bindings: &[WorkQuestionBindingWire],
) -> ProviderResult<()> {
    let mut positions = BTreeSet::new();
    for item in &draft.items {
        let index = item
            .question
            .position
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| protocol_drift("Chaoxing Work Draft position is invalid"))?;
        let binding = bindings.get(index).ok_or_else(|| {
            protocol_drift("Chaoxing Work Draft Question is outside the full snapshot")
        })?;
        let remote_id = item
            .question
            .remote_question_id
            .as_deref()
            .filter(|value| valid_component(value))
            .ok_or_else(|| protocol_drift("Chaoxing Work Draft Question has no remote identity"))?;
        let fingerprint = item
            .question
            .content_fingerprint()
            .map_err(|_| invalid_response("Chaoxing Work Question fingerprint failed"))?
            .to_string();
        if item.question.task_id != draft.task_id
            || binding.remote_id != remote_id
            || binding.position != item.question.position
            || binding.fingerprint != fingerprint
            || !positions.insert(binding.position)
        {
            return Err(protocol_drift(
                "Chaoxing Work Draft no longer matches the full Question set",
            ));
        }
    }
    Ok(())
}

fn encoded_state(encoded: &mut Zeroizing<Vec<u8>>) -> ProviderResult<EncodedChaoxingWorkState> {
    if encoded.is_empty() || encoded.len() > MAX_WORK_STATE_BYTES {
        return Err(invalid_response("Chaoxing Work state exceeds its bound"));
    }
    let digest = Sha256::digest(encoded.as_slice()).into();
    Ok(EncodedChaoxingWorkState {
        value: SecretValue::new(std::mem::take(&mut **encoded)),
        digest,
    })
}

fn validate_encoded_state(bytes: &[u8], expected_digest: [u8; 32]) -> ProviderResult<()> {
    if bytes.is_empty()
        || bytes.len() > MAX_WORK_STATE_BYTES
        || expected_digest == [0; 32]
        || Sha256::digest(bytes).as_slice() != expected_digest
    {
        return Err(protocol_drift(
            "Chaoxing Work state digest or size is invalid",
        ));
    }
    Ok(())
}

fn validate_work_task_id(value: &str) -> ProviderResult<()> {
    let parts = value.split(':').collect::<Vec<_>>();
    if value.is_empty()
        || value.len() > MAX_REMOTE_TASK_ID_BYTES
        || value.chars().any(char::is_control)
        || !matches!(
            parts.as_slice(),
            ["work", course, class, work]
                if [course, class, work].into_iter().all(|component| valid_component(component))
        )
    {
        return Err(protocol_drift("Chaoxing Work task identity is invalid"));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_question_fingerprint(value: &str) -> bool {
    value.strip_prefix("v1:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn question_type_code(question: &Question) -> ProviderResult<String> {
    let value = question
        .metadata_sanitized
        .get("provider_type_code")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value.to_string())
        .filter(|value| valid_type_code(value))
        .ok_or_else(|| protocol_drift("Chaoxing Work Question has no valid Provider type code"))?;
    Ok(value)
}

fn valid_type_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 3
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn hash_bounded_component(hasher: &mut Sha256, value: &str) -> ProviderResult<()> {
    let length = u64::try_from(value.len())
        .map_err(|_| invalid_response("Chaoxing Work binding is unbounded"))?;
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
    Ok(())
}

pub(crate) fn response_digest(status: u16, body: &str) -> ProviderResult<[u8; 32]> {
    if body.is_empty() || body.len() > 4 * 1024 * 1024 {
        return Err(invalid_response(
            "Chaoxing Work response is empty or unbounded",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(b"asterism.chaoxing.work-final-response.v1\0");
    hasher.update(status.to_be_bytes());
    hash_bounded_component(&mut hasher, body)?;
    Ok(hasher.finalize().into())
}

fn hex_digest(value: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in value {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}
