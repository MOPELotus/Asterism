use std::{collections::BTreeSet, fmt};

use asterism_domain::{Question, SubmissionDraft, TaskId, Timestamp};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{SecretString, SecretValue};
use reqwest::Url;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::ChaoxingCourseRoute;

const MAX_EXAM_ENTRY_BYTES: usize = 8 * 1_024;
const MAX_EXAM_ID_BYTES: usize = 128;
const MAX_ENC_TASK_BYTES: usize = 2_048;
const MAX_ATTEMPT_ENC_BYTES: usize = 4_096;
const MAX_TITLE_BYTES: usize = 512;
const MAX_EXAM_STATE_BYTES: usize = 128 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 640;

pub const CHAOXING_EXAM_PRE_QUESTION_TYPE: &str = "chaoxing.exam-pre-question.v1";
pub const CHAOXING_EXAM_READY_TO_START_PHASE: &str = "chaoxing.exam-ready-to-start";
pub const CHAOXING_EXAM_QUESTION_ARTIFACT_TYPE: &str = "chaoxing.exam-question-attempt.v3";
pub const CHAOXING_EXAM_QUESTIONS_READY_PHASE: &str = "chaoxing.exam-questions-ready";
pub const CHAOXING_EXAM_START_OPERATION: &str = "chaoxing.exam-start.v1";
pub(crate) const CHAOXING_EXAM_CONTINUATION_TTL_SECONDS: u64 = 30 * 60;

/// Fresh Exam entry facts carried from one task inventory into the mutating
/// start chain. The entry and dynamic `enc_task` remain provider-private.
pub struct ChaoxingExamQuestionRequest<'a> {
    exam_id: String,
    enc_task: SecretString,
    route: ChaoxingCourseRoute<'a>,
}

impl<'a> ChaoxingExamQuestionRequest<'a> {
    pub(crate) fn try_new(
        route: ChaoxingCourseRoute<'a>,
        remote_task_id: &'a str,
        entry: &str,
    ) -> ProviderResult<Self> {
        if entry.is_empty()
            || entry.len() > MAX_EXAM_ENTRY_BYTES
            || entry.chars().any(char::is_control)
        {
            return Err(protocol_drift("Chaoxing Exam entry is empty or unbounded"));
        }
        let exam_id = remote_task_id
            .strip_prefix("exam:")
            .and_then(|value| value.strip_prefix(route.course_id()))
            .and_then(|value| value.strip_prefix(':'))
            .and_then(|value| value.strip_prefix(route.class_id()))
            .and_then(|value| value.strip_prefix(':'))
            .filter(|value| valid_component(value))
            .ok_or_else(|| protocol_drift("Chaoxing Exam task identity is not route-bound"))?;
        let enc_task = extract_enc_task(entry).ok_or_else(|| {
            protocol_drift("Chaoxing Exam entry has no bounded enc_task material")
        })?;
        if !entry
            .to_ascii_lowercase()
            .contains(&exam_id.to_ascii_lowercase())
        {
            return Err(protocol_drift(
                "Chaoxing Exam entry does not contain the current exam identity",
            ));
        }
        Ok(Self {
            exam_id: exam_id.to_owned(),
            enc_task: SecretString::new(enc_task),
            route,
        })
    }

    pub(crate) fn exam_id(&self) -> &str {
        &self.exam_id
    }

    pub(crate) fn enc_task(&self) -> &str {
        self.enc_task.expose_secret()
    }

    pub(crate) const fn route(&self) -> ChaoxingCourseRoute<'a> {
        self.route
    }
}

impl fmt::Debug for ChaoxingExamQuestionRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamQuestionRequest")
            .field("exam_id", &self.exam_id)
            .field("enc_task", &"[REDACTED]")
            .field("route", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Exact, encrypted-at-rest command for the donor-observed one-shot Exam
/// start request. Core persists its request digest before the transport sends
/// anything, so an ambiguous response cannot cause an automatic replay.
pub struct ChaoxingExamStartCommand {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    remote_course_id: Zeroizing<String>,
    course_id: Zeroizing<String>,
    class_id: Zeroizing<String>,
    cpi: Zeroizing<String>,
    exam_id: Zeroizing<String>,
    enc_task: Zeroizing<String>,
    exam_answer_id: Zeroizing<String>,
    request_digest: [u8; 32],
}

impl ChaoxingExamStartCommand {
    pub(crate) fn from_cover(
        task_id: TaskId,
        remote_task_id: &str,
        request: &ChaoxingExamQuestionRequest<'_>,
        cover: ChaoxingExamCover,
    ) -> ProviderResult<Self> {
        let command = Self {
            task_id: Zeroizing::new(task_id.to_string()),
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            remote_course_id: Zeroizing::new(request.route().remote_course_id().to_owned()),
            course_id: Zeroizing::new(request.route().course_id().to_owned()),
            class_id: Zeroizing::new(request.route().class_id().to_owned()),
            cpi: Zeroizing::new(request.route().cpi().to_owned()),
            exam_id: Zeroizing::new(request.exam_id().to_owned()),
            enc_task: Zeroizing::new(request.enc_task().to_owned()),
            exam_answer_id: cover.exam_answer_id,
            request_digest: [0; 32],
        };
        command.validate_bound(task_id, remote_task_id)?;
        let request_digest = command.derive_request_digest()?;
        Ok(Self {
            request_digest,
            ..command
        })
    }

    pub(crate) fn encode(&self) -> ProviderResult<EncodedChaoxingExamState> {
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ExamStartCommandWireRef {
                schema: CHAOXING_EXAM_PRE_QUESTION_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                remote_course_id: &self.remote_course_id,
                course_id: &self.course_id,
                class_id: &self.class_id,
                cpi: &self.cpi,
                exam_id: &self.exam_id,
                enc_task: &self.enc_task,
                exam_answer_id: &self.exam_answer_id,
            })
            .map_err(|_| invalid_response("Chaoxing Exam start command could not be encoded"))?,
        );
        encoded_state(&mut encoded)
    }

    pub(crate) fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        task_id: TaskId,
        remote_task_id: &str,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        validate_encoded_state(bytes, expected_digest)?;
        let mut wire: ExamStartCommandWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Chaoxing Exam start command schema is invalid"))?;
        if wire.schema != CHAOXING_EXAM_PRE_QUESTION_TYPE {
            return Err(protocol_drift(
                "Chaoxing Exam start command type is invalid",
            ));
        }
        let mut command = Self {
            task_id: Zeroizing::new(std::mem::take(&mut wire.task_id)),
            remote_task_id: Zeroizing::new(std::mem::take(&mut wire.remote_task_id)),
            remote_course_id: Zeroizing::new(std::mem::take(&mut wire.remote_course_id)),
            course_id: Zeroizing::new(std::mem::take(&mut wire.course_id)),
            class_id: Zeroizing::new(std::mem::take(&mut wire.class_id)),
            cpi: Zeroizing::new(std::mem::take(&mut wire.cpi)),
            exam_id: Zeroizing::new(std::mem::take(&mut wire.exam_id)),
            enc_task: Zeroizing::new(std::mem::take(&mut wire.enc_task)),
            exam_answer_id: Zeroizing::new(std::mem::take(&mut wire.exam_answer_id)),
            request_digest: [0; 32],
        };
        command.validate_bound(task_id, remote_task_id)?;
        command.request_digest = command.derive_request_digest()?;
        Ok(command)
    }

    fn validate_bound(&self, task_id: TaskId, remote_task_id: &str) -> ProviderResult<()> {
        if self.task_id.as_str() != task_id.to_string()
            || self.remote_task_id.as_str() != remote_task_id
            || !valid_remote_task_id(remote_task_id)
            || !valid_component(&self.course_id)
            || !valid_component(&self.class_id)
            || !valid_component(&self.cpi)
            || !valid_component(&self.exam_id)
            || !valid_component(&self.exam_answer_id)
            || self.enc_task.is_empty()
            || self.enc_task.len() > MAX_ENC_TASK_BYTES
            || self.enc_task.chars().any(char::is_control)
            || remote_task_id
                != format!(
                    "exam:{}:{}:{}",
                    self.course_id(),
                    self.class_id(),
                    self.exam_id()
                )
            || self.remote_course_id.as_str()
                != format!("course:{}:{}", self.course_id(), self.class_id())
        {
            return Err(protocol_drift(
                "Chaoxing Exam start command is stale or foreign",
            ));
        }
        Ok(())
    }

    fn derive_request_digest(&self) -> ProviderResult<[u8; 32]> {
        let encoded = serde_json::to_vec(&ExamStartRequestIdentity {
            method: "GET",
            endpoint: "/exam-ans/exam/phone/start",
            requested_with: "com.chaoxing.mobile",
            course_id: &self.course_id,
            class_id: &self.class_id,
            exam_id: &self.exam_id,
            source: "0",
            exam_answer_id: &self.exam_answer_id,
            cpi: &self.cpi,
            keyboard_display_requires_user_action: "1",
            imei: "asterism-native",
            face_detection: "0",
            facekey: "",
            face_detection_result: "",
            captcha_validate: "",
            jt: "0",
            code: "",
        })
        .map_err(|_| invalid_response("Chaoxing Exam start identity could not be encoded"))?;
        Ok(Sha256::digest(encoded).into())
    }

    pub(crate) const fn operation_type() -> &'static str {
        CHAOXING_EXAM_START_OPERATION
    }

    pub(crate) const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub(crate) fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub(crate) fn remote_course_id(&self) -> &str {
        &self.remote_course_id
    }

    pub(crate) fn course_id(&self) -> &str {
        &self.course_id
    }

    pub(crate) fn class_id(&self) -> &str {
        &self.class_id
    }

    pub(crate) fn cpi(&self) -> &str {
        &self.cpi
    }

    pub(crate) fn exam_id(&self) -> &str {
        &self.exam_id
    }

    pub(crate) fn exam_answer_id(&self) -> &str {
        &self.exam_answer_id
    }

    pub(crate) fn valid_start_redirect(&self, url: &Url) -> bool {
        valid_exam_start_redirect_binding(url, self.course_id(), self.class_id(), self.exam_id())
    }

    pub(crate) fn valid_question_url(&self, url: &Url, exam_answer_id: &str) -> bool {
        valid_exam_question_url_binding(
            url,
            self.course_id(),
            self.class_id(),
            self.exam_id(),
            exam_answer_id,
        )
    }
}

impl fmt::Debug for ChaoxingExamStartCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamStartCommand")
            .field("binding", &"configured")
            .field("request_digest", &self.request_digest)
            .finish_non_exhaustive()
    }
}

pub struct ChaoxingExamStartOutcome {
    pub(crate) document: crate::ChaoxingInventoryDocument,
    pub(crate) material: ChaoxingExamAttemptMaterial,
    pub(crate) response_digest: [u8; 32],
    pub(crate) received_at: Timestamp,
}

impl fmt::Debug for ChaoxingExamStartOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamStartOutcome")
            .field("document", &"[REDACTED]")
            .field("material", &self.material)
            .field("response_digest", &self.response_digest)
            .field("received_at", &self.received_at)
            .finish()
    }
}

/// Minimal encrypted attempt material attached to the immutable Core Question
/// snapshot. It retains no response HTML or answer content.
pub struct ChaoxingExamQuestionArtifact {
    task_id: Zeroizing<String>,
    remote_task_id: Zeroizing<String>,
    remote_course_id: Zeroizing<String>,
    course_id: Zeroizing<String>,
    class_id: Zeroizing<String>,
    cpi: Zeroizing<String>,
    exam_id: Zeroizing<String>,
    exam_answer_id: Zeroizing<String>,
    enc: Zeroizing<String>,
    enc_remain_time: u64,
    remain_time: u64,
    last_update_time: u64,
    question_count: u32,
    question_set_digest: Zeroizing<String>,
    question_bindings: Vec<ExamQuestionBinding>,
    submission_positions: Vec<u32>,
    next_answer_index: u32,
}

impl fmt::Debug for ChaoxingExamQuestionArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamQuestionArtifact")
            .field("binding", &"[REDACTED]")
            .field("question_count", &self.question_count)
            .field("next_answer_index", &self.next_answer_index)
            .finish_non_exhaustive()
    }
}

impl ChaoxingExamQuestionArtifact {
    pub(crate) fn from_materialization(
        task_id: TaskId,
        command: &ChaoxingExamStartCommand,
        material: ChaoxingExamAttemptMaterial,
        questions: &[Question],
    ) -> ProviderResult<Self> {
        if material.exam_answer_id.as_str() != command.exam_answer_id() || questions.is_empty() {
            return Err(protocol_drift(
                "Chaoxing Exam materialization changed its attempt binding",
            ));
        }
        let (question_count, question_set_digest, question_bindings) =
            derive_question_set_binding(task_id, questions.iter())?;
        Ok(Self {
            task_id: Zeroizing::new(task_id.to_string()),
            remote_task_id: Zeroizing::new(command.remote_task_id().to_owned()),
            remote_course_id: Zeroizing::new(command.remote_course_id().to_owned()),
            course_id: Zeroizing::new(command.course_id().to_owned()),
            class_id: Zeroizing::new(command.class_id().to_owned()),
            cpi: Zeroizing::new(command.cpi().to_owned()),
            exam_id: Zeroizing::new(command.exam_id().to_owned()),
            exam_answer_id: material.exam_answer_id,
            enc: material.enc,
            enc_remain_time: material.enc_remain_time,
            remain_time: material.remain_time,
            last_update_time: material.last_update_time,
            question_count,
            question_set_digest: Zeroizing::new(question_set_digest),
            question_bindings,
            submission_positions: Vec::new(),
            next_answer_index: 0,
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
        let mut wire: ExamQuestionArtifactWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Chaoxing Exam artifact schema is invalid"))?;
        if wire.schema != CHAOXING_EXAM_QUESTION_ARTIFACT_TYPE
            || draft.validate().is_err()
            || wire.task_id != draft.task_id.to_string()
            || wire.remote_task_id != remote_task_id
            || remote_task_id
                != format!("exam:{}:{}:{}", wire.course_id, wire.class_id, wire.exam_id)
            || wire.remote_course_id != format!("course:{}:{}", wire.course_id, wire.class_id)
        {
            return Err(protocol_drift(
                "Chaoxing Exam artifact is stale, foreign, or malformed",
            ));
        }
        let question_count = validate_question_binding_wire(&wire.question_bindings)?;
        let question_set_digest = derive_wire_question_set_digest(&wire.question_bindings)?;
        if wire.question_count != question_count
            || wire.question_set_digest != question_set_digest
            || wire.question_count != draft.answer_coverage.total_question_count
            || !valid_component(&wire.course_id)
            || !valid_component(&wire.class_id)
            || !valid_component(&wire.cpi)
            || !valid_component(&wire.exam_id)
            || !valid_component(&wire.exam_answer_id)
            || wire.enc.is_empty()
            || wire.enc.len() > MAX_ATTEMPT_ENC_BYTES
            || wire.enc.chars().any(char::is_control)
        {
            return Err(protocol_drift(
                "Chaoxing Exam artifact no longer matches the immutable Draft",
            ));
        }
        let submission_positions = derive_submission_positions(draft, &wire.question_bindings)?;
        if (wire.submission_positions.is_empty() && wire.next_answer_index != 0)
            || (!wire.submission_positions.is_empty()
                && wire.submission_positions != submission_positions)
            || wire.next_answer_index
                > u32::try_from(submission_positions.len()).map_err(|_| {
                    invalid_response("Chaoxing Exam submission Question count is unbounded")
                })?
        {
            return Err(protocol_drift(
                "Chaoxing Exam submission selection or cursor has changed",
            ));
        }
        let question_bindings = std::mem::take(&mut wire.question_bindings)
            .into_iter()
            .map(ExamQuestionBinding::from)
            .collect();
        Ok(Self {
            task_id: Zeroizing::new(std::mem::take(&mut wire.task_id)),
            remote_task_id: Zeroizing::new(std::mem::take(&mut wire.remote_task_id)),
            remote_course_id: Zeroizing::new(std::mem::take(&mut wire.remote_course_id)),
            course_id: Zeroizing::new(std::mem::take(&mut wire.course_id)),
            class_id: Zeroizing::new(std::mem::take(&mut wire.class_id)),
            cpi: Zeroizing::new(std::mem::take(&mut wire.cpi)),
            exam_id: Zeroizing::new(std::mem::take(&mut wire.exam_id)),
            exam_answer_id: Zeroizing::new(std::mem::take(&mut wire.exam_answer_id)),
            enc: Zeroizing::new(std::mem::take(&mut wire.enc)),
            enc_remain_time: wire.enc_remain_time,
            remain_time: wire.remain_time,
            last_update_time: wire.last_update_time,
            question_count,
            question_set_digest: Zeroizing::new(question_set_digest),
            question_bindings,
            submission_positions,
            next_answer_index: wire.next_answer_index,
        })
    }

    pub(crate) fn encode(&self) -> ProviderResult<EncodedChaoxingExamState> {
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ExamQuestionArtifactWireRef {
                schema: CHAOXING_EXAM_QUESTION_ARTIFACT_TYPE,
                task_id: &self.task_id,
                remote_task_id: &self.remote_task_id,
                remote_course_id: &self.remote_course_id,
                course_id: &self.course_id,
                class_id: &self.class_id,
                cpi: &self.cpi,
                exam_id: &self.exam_id,
                exam_answer_id: &self.exam_answer_id,
                enc: &self.enc,
                enc_remain_time: self.enc_remain_time,
                remain_time: self.remain_time,
                last_update_time: self.last_update_time,
                question_count: self.question_count,
                question_set_digest: &self.question_set_digest,
                question_bindings: self
                    .question_bindings
                    .iter()
                    .map(ExamQuestionBindingWireRef::from)
                    .collect(),
                submission_positions: &self.submission_positions,
                next_answer_index: self.next_answer_index,
            })
            .map_err(|_| invalid_response("Chaoxing Exam artifact could not be encoded"))?,
        );
        encoded_state(&mut encoded)
    }

    pub(crate) fn apply_saved_answer(
        mut self,
        last_update_time: u64,
        enc_remain_time: u64,
        mut enc: String,
    ) -> ProviderResult<Self> {
        if self.next_answer_index >= self.submission_count()
            || last_update_time < self.last_update_time
            || enc_remain_time > self.enc_remain_time
            || enc.is_empty()
            || enc.len() > MAX_ATTEMPT_ENC_BYTES
            || enc.chars().any(char::is_control)
        {
            enc.zeroize();
            return Err(protocol_drift(
                "Chaoxing Exam save response regressed its attempt state",
            ));
        }
        self.next_answer_index = self
            .next_answer_index
            .checked_add(1)
            .ok_or_else(|| protocol_drift("Chaoxing Exam answer index is exhausted"))?;
        self.last_update_time = last_update_time;
        self.enc_remain_time = enc_remain_time;
        self.enc = Zeroizing::new(enc);
        Ok(self)
    }

    pub(crate) const fn next_answer_index(&self) -> u32 {
        self.next_answer_index
    }

    pub(crate) fn submission_count(&self) -> u32 {
        u32::try_from(self.submission_positions.len()).unwrap_or(u32::MAX)
    }

    pub(crate) fn current_question_position(&self) -> Option<u32> {
        usize::try_from(self.next_answer_index)
            .ok()
            .and_then(|index| self.submission_positions.get(index).copied())
    }

    pub(crate) fn question_binding_at(&self, index: usize) -> Option<(&str, u32)> {
        self.question_bindings
            .get(index)
            .map(|binding| (binding.remote_id.as_str(), binding.position))
    }

    pub(crate) fn course_id(&self) -> &str {
        &self.course_id
    }

    pub(crate) fn class_id(&self) -> &str {
        &self.class_id
    }

    pub(crate) fn cpi(&self) -> &str {
        &self.cpi
    }

    pub(crate) fn exam_id(&self) -> &str {
        &self.exam_id
    }

    pub(crate) fn exam_answer_id(&self) -> &str {
        &self.exam_answer_id
    }

    pub(crate) fn enc(&self) -> &str {
        &self.enc
    }

    pub(crate) const fn enc_remain_time(&self) -> u64 {
        self.enc_remain_time
    }

    pub(crate) const fn remain_time(&self) -> u64 {
        self.remain_time
    }

    pub(crate) const fn last_update_time(&self) -> u64 {
        self.last_update_time
    }
}

pub(crate) struct EncodedChaoxingExamState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedChaoxingExamState {
    pub(crate) const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub(crate) fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedChaoxingExamState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedChaoxingExamState")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

#[derive(Serialize)]
struct ExamStartCommandWireRef<'a> {
    schema: &'static str,
    task_id: &'a str,
    remote_task_id: &'a str,
    remote_course_id: &'a str,
    course_id: &'a str,
    class_id: &'a str,
    cpi: &'a str,
    exam_id: &'a str,
    enc_task: &'a str,
    exam_answer_id: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ExamStartCommandWire {
    schema: String,
    task_id: String,
    remote_task_id: String,
    remote_course_id: String,
    course_id: String,
    class_id: String,
    cpi: String,
    exam_id: String,
    enc_task: String,
    exam_answer_id: String,
}

#[derive(Serialize)]
struct ExamStartRequestIdentity<'a> {
    method: &'static str,
    endpoint: &'static str,
    requested_with: &'static str,
    course_id: &'a str,
    class_id: &'a str,
    exam_id: &'a str,
    source: &'static str,
    exam_answer_id: &'a str,
    cpi: &'a str,
    keyboard_display_requires_user_action: &'static str,
    imei: &'static str,
    face_detection: &'static str,
    facekey: &'static str,
    face_detection_result: &'static str,
    captcha_validate: &'static str,
    jt: &'static str,
    code: &'static str,
}

#[derive(Serialize)]
struct ExamQuestionArtifactWireRef<'a> {
    schema: &'static str,
    task_id: &'a str,
    remote_task_id: &'a str,
    remote_course_id: &'a str,
    course_id: &'a str,
    class_id: &'a str,
    cpi: &'a str,
    exam_id: &'a str,
    exam_answer_id: &'a str,
    enc: &'a str,
    enc_remain_time: u64,
    remain_time: u64,
    last_update_time: u64,
    question_count: u32,
    question_set_digest: &'a str,
    question_bindings: Vec<ExamQuestionBindingWireRef<'a>>,
    submission_positions: &'a [u32],
    next_answer_index: u32,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ExamQuestionArtifactWire {
    schema: String,
    task_id: String,
    remote_task_id: String,
    remote_course_id: String,
    course_id: String,
    class_id: String,
    cpi: String,
    exam_id: String,
    exam_answer_id: String,
    enc: String,
    enc_remain_time: u64,
    remain_time: u64,
    last_update_time: u64,
    question_count: u32,
    question_set_digest: String,
    question_bindings: Vec<ExamQuestionBindingWire>,
    submission_positions: Vec<u32>,
    next_answer_index: u32,
}

struct ExamQuestionBinding {
    remote_id: Zeroizing<String>,
    position: u32,
    fingerprint: Zeroizing<String>,
}

impl From<ExamQuestionBindingWire> for ExamQuestionBinding {
    fn from(mut value: ExamQuestionBindingWire) -> Self {
        Self {
            remote_id: Zeroizing::new(std::mem::take(&mut value.remote_id)),
            position: value.position,
            fingerprint: Zeroizing::new(std::mem::take(&mut value.fingerprint)),
        }
    }
}

#[derive(Serialize)]
struct ExamQuestionBindingWireRef<'a> {
    remote_id: &'a str,
    position: u32,
    fingerprint: &'a str,
}

impl<'a> From<&'a ExamQuestionBinding> for ExamQuestionBindingWireRef<'a> {
    fn from(value: &'a ExamQuestionBinding) -> Self {
        Self {
            remote_id: &value.remote_id,
            position: value.position,
            fingerprint: &value.fingerprint,
        }
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ExamQuestionBindingWire {
    remote_id: String,
    position: u32,
    fingerprint: String,
}

fn encoded_state(encoded: &mut Zeroizing<Vec<u8>>) -> ProviderResult<EncodedChaoxingExamState> {
    if encoded.is_empty() || encoded.len() > MAX_EXAM_STATE_BYTES {
        return Err(invalid_response("Chaoxing Exam state exceeds its bound"));
    }
    let digest = Sha256::digest(encoded.as_slice()).into();
    Ok(EncodedChaoxingExamState {
        value: SecretValue::new(std::mem::take(&mut **encoded)),
        digest,
    })
}

fn validate_encoded_state(bytes: &[u8], expected_digest: [u8; 32]) -> ProviderResult<()> {
    if bytes.is_empty()
        || bytes.len() > MAX_EXAM_STATE_BYTES
        || Sha256::digest(bytes).as_slice() != expected_digest
    {
        return Err(protocol_drift(
            "Chaoxing Exam state digest or size is invalid",
        ));
    }
    Ok(())
}

fn derive_question_set_binding<'a>(
    task_id: TaskId,
    questions: impl ExactSizeIterator<Item = &'a Question>,
) -> ProviderResult<(u32, String, Vec<ExamQuestionBinding>)> {
    let question_count = u32::try_from(questions.len())
        .map_err(|_| invalid_response("Chaoxing Exam Question count is unbounded"))?;
    if question_count == 0 {
        return Err(protocol_drift("Chaoxing Exam Question set is empty"));
    }
    let mut remote_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    let mut question_set_hasher = Sha256::new();
    let mut question_bindings = Vec::with_capacity(
        usize::try_from(question_count)
            .map_err(|_| invalid_response("Chaoxing Exam Question count is unbounded"))?,
    );
    for (index, question) in questions.enumerate() {
        question
            .validate()
            .map_err(|_| invalid_response("Chaoxing Exam Question is invalid"))?;
        let remote_question_id = question
            .remote_question_id
            .as_deref()
            .filter(|value| valid_component(value))
            .ok_or_else(|| protocol_drift("Chaoxing Exam Question has no remote identity"))?;
        if question.task_id != task_id
            || !remote_ids.insert(remote_question_id)
            || !positions.insert(question.position)
            || usize::try_from(question.position).ok() != index.checked_add(1)
        {
            return Err(protocol_drift(
                "Chaoxing Exam Question set has a stale or duplicate binding",
            ));
        }
        let fingerprint = question
            .content_fingerprint()
            .map_err(|_| invalid_response("Chaoxing Exam Question fingerprint failed"))?
            .to_string();
        hash_bounded_component(&mut question_set_hasher, remote_question_id)?;
        question_set_hasher.update(question.position.to_be_bytes());
        hash_bounded_component(&mut question_set_hasher, &fingerprint)?;
        question_bindings.push(ExamQuestionBinding {
            remote_id: Zeroizing::new(remote_question_id.to_owned()),
            position: question.position,
            fingerprint: Zeroizing::new(fingerprint),
        });
    }
    Ok((
        question_count,
        hex_digest(question_set_hasher.finalize().into()),
        question_bindings,
    ))
}

fn validate_question_binding_wire(bindings: &[ExamQuestionBindingWire]) -> ProviderResult<u32> {
    let question_count = u32::try_from(bindings.len())
        .map_err(|_| invalid_response("Chaoxing Exam Question binding count is unbounded"))?;
    if question_count == 0 {
        return Err(protocol_drift(
            "Chaoxing Exam Question binding set is empty",
        ));
    }
    let mut remote_ids = BTreeSet::new();
    for (index, binding) in bindings.iter().enumerate() {
        if !valid_component(&binding.remote_id)
            || !valid_question_fingerprint(&binding.fingerprint)
            || !remote_ids.insert(binding.remote_id.as_str())
            || usize::try_from(binding.position).ok() != index.checked_add(1)
        {
            return Err(protocol_drift(
                "Chaoxing Exam Question binding set is malformed",
            ));
        }
    }
    Ok(question_count)
}

fn derive_wire_question_set_digest(bindings: &[ExamQuestionBindingWire]) -> ProviderResult<String> {
    let mut question_set_hasher = Sha256::new();
    for binding in bindings {
        hash_bounded_component(&mut question_set_hasher, &binding.remote_id)?;
        question_set_hasher.update(binding.position.to_be_bytes());
        hash_bounded_component(&mut question_set_hasher, &binding.fingerprint)?;
    }
    Ok(hex_digest(question_set_hasher.finalize().into()))
}

fn derive_submission_positions(
    draft: &SubmissionDraft,
    bindings: &[ExamQuestionBindingWire],
) -> ProviderResult<Vec<u32>> {
    let mut submission_positions = Vec::with_capacity(draft.items.len());
    for item in &draft.items {
        let position_index = item
            .question
            .position
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| protocol_drift("Chaoxing Exam Draft Question position is invalid"))?;
        let binding = bindings.get(position_index).ok_or_else(|| {
            protocol_drift("Chaoxing Exam Draft Question is outside the full Question set")
        })?;
        let remote_id = item
            .question
            .remote_question_id
            .as_deref()
            .filter(|value| valid_component(value))
            .ok_or_else(|| protocol_drift("Chaoxing Exam Draft Question has no remote identity"))?;
        let fingerprint = Zeroizing::new(
            item.question
                .content_fingerprint()
                .map_err(|_| invalid_response("Chaoxing Exam Question fingerprint failed"))?
                .to_string(),
        );
        if item.question.task_id != draft.task_id
            || binding.position != item.question.position
            || binding.remote_id != remote_id
            || binding.fingerprint != fingerprint.as_str()
            || submission_positions
                .last()
                .is_some_and(|last| *last >= binding.position)
        {
            return Err(protocol_drift(
                "Chaoxing Exam Draft Question no longer matches the full Question set",
            ));
        }
        submission_positions.push(binding.position);
    }
    Ok(submission_positions)
}

fn valid_question_fingerprint(value: &str) -> bool {
    value.strip_prefix("v1:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn hash_bounded_component(hasher: &mut Sha256, value: &str) -> ProviderResult<()> {
    let length = u64::try_from(value.len())
        .map_err(|_| invalid_response("Chaoxing Exam Question binding is unbounded"))?;
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
    Ok(())
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

/// Non-secret metadata from the Exam cover page. Dynamic attempt material is
/// intentionally kept separate and is only produced after the one-shot start.
pub(crate) struct ChaoxingExamCover {
    pub(crate) title: String,
    pub(crate) exam_answer_id: Zeroizing<String>,
    pub(crate) need_code: bool,
    pub(crate) need_face: bool,
    pub(crate) need_captcha: bool,
    pub(crate) captcha_id: Option<String>,
}

impl fmt::Debug for ChaoxingExamCover {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamCover")
            .field("title", &self.title)
            .field("attempt_material", &"[REDACTED]")
            .field("need_code", &self.need_code)
            .field("need_face", &self.need_face)
            .field("need_captcha", &self.need_captcha)
            .field(
                "captcha_id",
                &self.captcha_id.as_ref().map(|_| "[REDACTED]"),
            )
            .finish_non_exhaustive()
    }
}

/// Dynamic attempt state returned by the start redirect/page.
pub(crate) struct ChaoxingExamAttemptMaterial {
    pub(crate) exam_answer_id: Zeroizing<String>,
    pub(crate) enc: Zeroizing<String>,
    pub(crate) enc_remain_time: u64,
    pub(crate) remain_time: u64,
    pub(crate) last_update_time: u64,
}

impl fmt::Debug for ChaoxingExamAttemptMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamAttemptMaterial")
            .field("exam_answer_id", &self.exam_answer_id)
            .field("enc", &"[REDACTED]")
            .field("enc_remain_time", &self.enc_remain_time)
            .field("remain_time", &self.remain_time)
            .field("last_update_time", &self.last_update_time)
            .finish()
    }
}

pub(crate) fn parse_exam_cover(html: &str) -> ProviderResult<ChaoxingExamCover> {
    bounded_html(html)?;
    let document = Html::parse_document(html);
    if let Some(message) = first_text(&document, "h2.color6.fs36.textCenter, p.blankTips, li.msg") {
        let normalized = normalize_text(&message);
        if normalized.contains("尚未开始") || normalized.contains("未开始") {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing Exam has not started",
            ));
        }
        if normalized.contains("验证码") || normalized.contains("人脸") {
            return Err(ProviderError::human_required(
                "Chaoxing Exam requires browser verification before start",
                asterism_domain::HumanRequiredReason::BrowserRequired,
            ));
        }
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing Exam cover returned a non-startable state",
        ));
    }
    let exam_answer_id = input_value(&document, "#testUserRelationId")
        .filter(|value| valid_component(value))
        .ok_or_else(|| protocol_drift("Chaoxing Exam cover has no attempt identity"))?;
    let monitor_enc = input_value(&document, "#monitorEnc").unwrap_or_default();
    if monitor_enc.len() > MAX_ATTEMPT_ENC_BYTES {
        return Err(invalid_response(
            "Chaoxing Exam monitor material is unbounded",
        ));
    }
    let title = first_text(&document, "span.overHidden2")
        .map(|value| normalize_text(&value))
        .filter(|value| !value.is_empty() && value.len() <= MAX_TITLE_BYTES)
        .ok_or_else(|| protocol_drift("Chaoxing Exam cover has no bounded title"))?;
    let need_code = script_numeric_flag(&document, "needcode")?;
    let need_face = input_value(&document, "#faceRecognitionCompare").is_some_and(|v| v != "0");
    let need_captcha = input_value(&document, "#captchaCheck").is_some_and(|v| v != "0");
    let captcha_id = input_value(&document, "#captchaCaptchaId").filter(|v| !v.is_empty());
    Ok(ChaoxingExamCover {
        title,
        exam_answer_id: Zeroizing::new(exam_answer_id),
        need_code,
        need_face,
        need_captcha,
        captcha_id,
    })
}

pub(crate) fn parse_exam_attempt(
    final_url: &Url,
    html: &str,
    expected_answer_id: &str,
) -> ProviderResult<ChaoxingExamAttemptMaterial> {
    bounded_html(html)?;
    let enc = final_url
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("enc"))
        .map(|(_, value)| value.into_owned())
        .or_else(|| {
            let document = Html::parse_document(html);
            input_value(&document, "form#submitTest input#enc")
        })
        .filter(|value| !value.is_empty() && value.len() <= MAX_ATTEMPT_ENC_BYTES)
        .ok_or_else(|| protocol_drift("Chaoxing Exam start returned no bounded enc"))?;
    let document = Html::parse_document(html);
    let exam_answer_id = input_value(&document, "#testUserRelationId")
        .unwrap_or_else(|| expected_answer_id.to_owned());
    if exam_answer_id != expected_answer_id || !valid_component(&exam_answer_id) {
        return Err(protocol_drift(
            "Chaoxing Exam start changed the bound attempt identity",
        ));
    }
    let enc_remain_time = bounded_u64_input(&document, "#encRemainTime")?;
    let remain_time = bounded_u64_input(&document, "#remainTime")?;
    let last_update_time = bounded_u64_input(&document, "#encLastUpdateTime")?;
    Ok(ChaoxingExamAttemptMaterial {
        exam_answer_id: Zeroizing::new(exam_answer_id),
        enc: Zeroizing::new(enc),
        enc_remain_time,
        remain_time,
        last_update_time,
    })
}

fn valid_exam_question_url_binding(
    url: &Url,
    course_id: &str,
    class_id: &str,
    exam_id: &str,
    exam_answer_id: &str,
) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("mooc1-api.chaoxing.com")
        && url.path().contains("/exam-ans/exam/")
        && unique_query(url, "courseId").as_deref() == Some(course_id)
        && unique_query(url, "classId").as_deref() == Some(class_id)
        && unique_query(url, "tId")
            .or_else(|| unique_query(url, "examRelationId"))
            .is_some_and(|value| value == exam_id)
        && unique_query(url, "id")
            .or_else(|| unique_query(url, "examRelationAnswerId"))
            .is_some_and(|value| value == exam_answer_id)
}

#[cfg(test)]
pub(crate) fn valid_exam_start_redirect(
    url: &Url,
    route: ChaoxingCourseRoute<'_>,
    exam_id: &str,
) -> bool {
    valid_exam_start_redirect_binding(url, route.course_id(), route.class_id(), exam_id)
}

fn valid_exam_start_redirect_binding(
    url: &Url,
    course_id: &str,
    class_id: &str,
    exam_id: &str,
) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("mooc1-api.chaoxing.com")
        && url.path().contains("/exam-ans/exam/")
        && unique_query(url, "courseId").as_deref() == Some(course_id)
        && unique_query(url, "classId").as_deref() == Some(class_id)
        && unique_query(url, "tId")
            .or_else(|| unique_query(url, "examRelationId"))
            .is_some_and(|value| value == exam_id)
        && url.fragment().is_none()
}

fn extract_enc_task(entry: &str) -> Option<String> {
    if let Ok(url) = Url::parse(entry) {
        return unique_query(&url, "enc_task")
            .map(std::borrow::Cow::into_owned)
            .filter(|value| !value.is_empty() && value.len() <= MAX_ENC_TASK_BYTES);
    }
    let lower = entry.to_ascii_lowercase();
    let start = lower.find("gotest(")? + "gotest(".len();
    let args = entry.get(start..)?.split(')').next()?;
    args.split(',')
        .nth(6)
        .map(|value| value.trim().trim_matches(['\'', '"']).to_owned())
        .filter(|value| !value.is_empty() && value.len() <= MAX_ENC_TASK_BYTES)
}

fn input_value(document: &Html, selector_text: &str) -> Option<String> {
    let selector = Selector::parse(selector_text).ok()?;
    document
        .select(&selector)
        .next()?
        .value()
        .attr("value")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn bounded_u64_input(document: &Html, selector_text: &str) -> ProviderResult<u64> {
    input_value(document, selector_text)
        .ok_or_else(|| protocol_drift("Chaoxing Exam attempt omitted a timing field"))?
        .parse::<u64>()
        .map_err(|_| protocol_drift("Chaoxing Exam attempt has an invalid timing field"))
}

fn first_text(document: &Html, selector_text: &str) -> Option<String> {
    let selector = Selector::parse(selector_text).ok()?;
    document
        .select(&selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
}

fn script_numeric_flag(document: &Html, name: &str) -> ProviderResult<bool> {
    let mut values = Vec::new();
    for script in document
        .select(&Selector::parse("script").expect("static script selector"))
        .flat_map(|node| node.text())
    {
        let mut offset = 0_usize;
        while let Some(relative_start) = script[offset..].find("var") {
            let start = offset + relative_start;
            let tail = &script[start + "var".len()..];
            let tail = tail.trim_start_matches(' ');
            let Some(after_name) = tail.strip_prefix(name) else {
                offset = start + "var".len();
                continue;
            };
            if after_name
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
            {
                offset = start + "var".len();
                continue;
            }
            let after_name = after_name.trim_start_matches(' ');
            let Some(value) = after_name.strip_prefix('=') else {
                return Err(protocol_drift(
                    "Chaoxing Exam cover has a malformed script flag declaration",
                ));
            };
            let value = value.trim_start_matches(' ');
            let end = value
                .bytes()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(value.len());
            let digits = &value[..end];
            if digits.is_empty() || !value[end..].starts_with(';') {
                return Err(protocol_drift(
                    "Chaoxing Exam cover has a malformed script flag value",
                ));
            }
            values.push(
                digits
                    .parse::<u64>()
                    .map_err(|_| protocol_drift("Chaoxing Exam cover script flag is unbounded"))?,
            );
            offset = start + "var".len();
        }
    }
    let Some(value) = values.first().copied() else {
        return Err(protocol_drift(
            "Chaoxing Exam cover omitted a required script flag",
        ));
    };
    if values.len() != 1 {
        return Err(protocol_drift(
            "Chaoxing Exam cover duplicated a script flag",
        ));
    }
    Ok(value != 0)
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded_html(value: &str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > 4 * 1_024 * 1_024
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(invalid_response(
            "Chaoxing Exam document is empty, unbounded, or contains controls",
        ));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EXAM_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_remote_task_id(value: &str) -> bool {
    value.starts_with("exam:")
        && value.len() <= MAX_REMOTE_TASK_ID_BYTES
        && !value.chars().any(char::is_control)
}

fn unique_query<'a>(url: &'a Url, key: &str) -> Option<std::borrow::Cow<'a, str>> {
    let mut values = url
        .query_pairs()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asterism_provider_api::ProviderRouteContext;
    use asterism_provider_api::RemoteCourse;

    const COVER: &str = include_str!("../../../fixtures/providers/chaoxing/exam/cover-ready.html");
    const START: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/start-question.html");

    #[test]
    fn cover_and_start_bind_dynamic_attempt_material() {
        let cover = parse_exam_cover(COVER).unwrap();
        assert_eq!(cover.exam_answer_id.as_str(), "answer-1");
        assert!(cover.need_code);
        let url = Url::parse(
            "https://mooc1-api.chaoxing.com/exam-ans/exam/test/reVersionTestStartNew?courseId=100&classId=200&tId=exam-1&id=answer-1&enc=SAFE_ATTEMPT_ENC",
        )
        .unwrap();
        let attempt = parse_exam_attempt(&url, START, &cover.exam_answer_id).unwrap();
        assert_eq!(attempt.exam_answer_id.as_str(), "answer-1");
        assert_eq!(attempt.enc_remain_time, 3600);
        assert_eq!(attempt.remain_time, 3600);
    }

    #[test]
    fn cover_requires_one_exact_numeric_exam_code_flag() {
        for (declaration, expected) in [
            ("var needcode = 0;", false),
            ("var needcode = 1;", true),
            ("var needcode=2;", true),
            ("varneedcode=10;", true),
        ] {
            let cover = COVER.replace("var needcode = 1;", declaration);
            assert_eq!(parse_exam_cover(&cover).unwrap().need_code, expected);
        }

        for declaration in [
            "var needcodeBackup = 1;",
            "let needcode = 1;",
            "var needcode = '1';",
            "var needcode = 1.0;",
            "var needcode = 1; var needcode = 0;",
        ] {
            let cover = COVER.replace("var needcode = 1;", declaration);
            assert_eq!(
                parse_exam_cover(&cover).unwrap_err().kind,
                ProviderErrorKind::ProtocolDrift
            );
        }
    }

    #[test]
    fn request_requires_route_bound_enc_task() {
        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "fixture".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        };
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let request = ChaoxingExamQuestionRequest::try_new(
            route,
            "exam:100:200:exam-1",
            "goTest('100','exam-1',0,'SAFE_TIME','paper-1',false,'SAFE_ENC')",
        )
        .unwrap();
        assert_eq!(request.exam_id(), "exam-1");
        assert_eq!(request.enc_task(), "SAFE_ENC");
        assert!(!format!("{request:?}").contains("SAFE_ENC"));
    }

    #[test]
    fn start_redirect_is_limited_to_the_attempt_host_and_identity() {
        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "fixture".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        };
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let valid = Url::parse(
            "https://mooc1-api.chaoxing.com/exam-ans/exam/test/reVersionTestStartNew?courseId=100&classId=200&tId=exam-1&id=answer-1",
        )
        .unwrap();
        assert!(valid_exam_start_redirect(&valid, route, "exam-1"));
        let foreign = Url::parse(
            "https://evil.invalid/exam-ans/exam/test/reVersionTestStartNew?courseId=100&classId=200&tId=exam-1",
        )
        .unwrap();
        assert!(!valid_exam_start_redirect(&foreign, route, "exam-1"));
    }
}
