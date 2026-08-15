use std::{collections::BTreeSet, fmt};

use asterism_domain::{
    NormalizedAnswer, Question, QuestionKind, SubmissionDraft, SubmissionReceipt, Timestamp,
};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::ChaoxingExamQuestionArtifact;

const EXAM_SUBMISSION_ENDPOINT: &str = "/exam-ans/exam/test/reVersionSubmitTestNew";
const MAX_FORM_FIELDS: usize = 512;
const MAX_FIELD_BYTES: usize = 64 * 1_024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const EXAM_SAVE_OPERATION: &str = "chaoxing.exam-answer-save.v1";
const EXAM_FINAL_OPERATION: &str = "chaoxing.exam-final-submit.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExamSubmissionKind {
    Save,
    Final,
}

/// Exact zeroizing request frozen before Core records its digest. Randomized
/// donor signature fields are generated once during preparation and are never
/// regenerated for dispatch or recovery.
pub struct ChaoxingExamSubmissionCommand {
    artifact: Option<ChaoxingExamQuestionArtifact>,
    kind: ExamSubmissionKind,
    user_id: Zeroizing<String>,
    query: Vec<(String, String)>,
    body: Vec<(String, String)>,
    request_digest: [u8; 32],
}

impl ChaoxingExamSubmissionCommand {
    /// Freezes one exact donor save/final command from the current encrypted
    /// attempt state, immutable Draft, account identity and caller entropy.
    ///
    /// # Errors
    ///
    /// Rejects stale bindings, unsupported answer shapes, unbounded form
    /// values or invalid signature inputs.
    pub fn try_new(
        artifact: ChaoxingExamQuestionArtifact,
        draft: &SubmissionDraft,
        user_id: &str,
        entropy: [u8; 32],
        prepared_at: Timestamp,
    ) -> ProviderResult<Self> {
        if !valid_component(user_id) || prepared_at.timestamp_millis() <= 0 {
            return Err(protocol_drift(
                "Chaoxing Exam submission identity or timestamp is invalid",
            ));
        }
        let index = usize::try_from(artifact.next_answer_index())
            .map_err(|_| protocol_drift("Chaoxing Exam answer index is invalid"))?;
        let count = usize::try_from(artifact.submission_count())
            .map_err(|_| protocol_drift("Chaoxing Exam submission Question count is invalid"))?;
        if count != draft.items.len() || index > count {
            return Err(protocol_drift(
                "Chaoxing Exam submission cursor no longer matches its Draft",
            ));
        }
        let (kind, question) = if index == count {
            (ExamSubmissionKind::Final, None)
        } else {
            (ExamSubmissionKind::Save, Some(&draft.items[index].question))
        };
        let question_id = question
            .and_then(|question| question.remote_question_id.as_deref())
            .unwrap_or_default();
        let final_submit = kind == ExamSubmissionKind::Final;
        let start_index = if let Some(question) = question {
            let position = artifact.current_question_position().ok_or_else(|| {
                protocol_drift("Chaoxing Exam submission cursor has no original Question position")
            })?;
            if position != question.position {
                return Err(protocol_drift(
                    "Chaoxing Exam submission cursor changed its original Question position",
                ));
            }
            usize::try_from(position.checked_sub(1).ok_or_else(|| {
                protocol_drift("Chaoxing Exam submission Question position is invalid")
            })?)
            .map_err(|_| protocol_drift("Chaoxing Exam Question position is invalid"))?
        } else {
            0
        };
        let signature = ExamSignature::derive(
            user_id,
            question_id,
            entropy,
            u64::try_from(prepared_at.timestamp_millis())
                .map_err(|_| protocol_drift("Chaoxing Exam timestamp is invalid"))?,
        )?;
        let temp_save = if final_submit { "false" } else { "true" };
        let query = vec![
            ("classId".to_owned(), artifact.class_id().to_owned()),
            ("courseId".to_owned(), artifact.course_id().to_owned()),
            ("cpi".to_owned(), artifact.cpi().to_owned()),
            ("testPaperId".to_owned(), artifact.exam_id().to_owned()),
            (
                "testUserRelationId".to_owned(),
                artifact.exam_answer_id().to_owned(),
            ),
            ("tempSave".to_owned(), temp_save.to_owned()),
            ("pos".to_owned(), signature.pos),
            ("rd".to_owned(), signature.rd),
            ("value".to_owned(), signature.value),
            ("_edt".to_owned(), signature.edt),
            ("qid".to_owned(), question_id.to_owned()),
            ("version".to_owned(), "1".to_owned()),
        ];
        let mut body =
            base_submission_body(&artifact, user_id, start_index, temp_save, final_submit);
        if let Some(question) = question {
            append_question_fields(&mut body, question, &draft.items[index].selected.answer)?;
        }
        validate_fields(&query, &body)?;
        let request_digest = derive_request_digest(&query, &body)?;
        Ok(Self {
            artifact: Some(artifact),
            kind,
            user_id: Zeroizing::new(user_id.to_owned()),
            query,
            body,
            request_digest,
        })
    }

    pub const fn operation_type(&self) -> &'static str {
        match self.kind {
            ExamSubmissionKind::Save => EXAM_SAVE_OPERATION,
            ExamSubmissionKind::Final => EXAM_FINAL_OPERATION,
        }
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn is_final(&self) -> bool {
        matches!(self.kind, ExamSubmissionKind::Final)
    }

    pub fn belongs_to_user(&self, user_id: &str) -> bool {
        self.user_id.as_str() == user_id
    }

    /// Exposes the frozen query only to the authorized mutation transport.
    pub fn query(&self) -> &[(String, String)] {
        &self.query
    }

    /// Exposes the frozen answer-bearing body only to the authorized mutation
    /// transport. Debug output remains redacted and drop zeroizes all fields.
    pub fn body(&self) -> &[(String, String)] {
        &self.body
    }

    pub(crate) fn accept_saved(
        mut self,
        last_update_time: u64,
        enc_remain_time: u64,
        enc: &SecretString,
    ) -> ProviderResult<ChaoxingExamQuestionArtifact> {
        if self.kind != ExamSubmissionKind::Save {
            return Err(protocol_drift(
                "Chaoxing Exam final submission returned save state",
            ));
        }
        self.artifact
            .take()
            .ok_or_else(|| protocol_drift("Chaoxing Exam command lost its attempt state"))?
            .apply_saved_answer(
                last_update_time,
                enc_remain_time,
                enc.expose_secret().to_owned(),
            )
    }
}

fn base_submission_body(
    artifact: &ChaoxingExamQuestionArtifact,
    user_id: &str,
    start_index: usize,
    temp_save: &str,
    final_submit: bool,
) -> Vec<(String, String)> {
    vec![
        ("courseId".to_owned(), artifact.course_id().to_owned()),
        ("testPaperId".to_owned(), artifact.exam_id().to_owned()),
        (
            "testUserRelationId".to_owned(),
            artifact.exam_answer_id().to_owned(),
        ),
        ("classId".to_owned(), artifact.class_id().to_owned()),
        ("type".to_owned(), "0".to_owned()),
        ("isphone".to_owned(), "true".to_owned()),
        ("imei".to_owned(), "asterism-native".to_owned()),
        ("subCount".to_owned(), String::new()),
        ("remainTime".to_owned(), artifact.remain_time().to_string()),
        ("tempSave".to_owned(), temp_save.to_owned()),
        ("timeOver".to_owned(), "false".to_owned()),
        (
            "encRemainTime".to_owned(),
            artifact.enc_remain_time().to_string(),
        ),
        (
            "encLastUpdateTime".to_owned(),
            artifact.last_update_time().to_string(),
        ),
        ("enc".to_owned(), artifact.enc().to_owned()),
        ("userId".to_owned(), user_id.to_owned()),
        ("source".to_owned(), "0".to_owned()),
        (
            "start".to_owned(),
            if final_submit {
                "0".to_owned()
            } else {
                start_index.to_string()
            },
        ),
        (
            "enterPageTime".to_owned(),
            artifact.last_update_time().to_string(),
        ),
        ("monitorforcesubmit".to_owned(), "0".to_owned()),
        ("answeredView".to_owned(), "0".to_owned()),
        ("exitdtime".to_owned(), "0".to_owned()),
    ]
}

impl fmt::Debug for ChaoxingExamSubmissionCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamSubmissionCommand")
            .field("operation_type", &self.operation_type())
            .field("request_digest", &self.request_digest)
            .field("query", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl Drop for ChaoxingExamSubmissionCommand {
    fn drop(&mut self) {
        for (name, value) in &mut self.query {
            name.zeroize();
            value.zeroize();
        }
        for (name, value) in &mut self.body {
            name.zeroize();
            value.zeroize();
        }
        self.query.clear();
        self.body.clear();
    }
}

pub enum ChaoxingExamSubmissionResponse {
    Saved {
        last_update_time: u64,
        enc_remain_time: u64,
        enc: SecretString,
        response_digest: [u8; 32],
        received_at: Timestamp,
    },
    Submitted {
        receipt: SubmissionReceipt,
        response_digest: [u8; 32],
        received_at: Timestamp,
    },
}

impl fmt::Debug for ChaoxingExamSubmissionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Saved {
                last_update_time,
                enc_remain_time,
                response_digest,
                received_at,
                ..
            } => formatter
                .debug_struct("ChaoxingExamSubmissionResponse::Saved")
                .field("last_update_time", last_update_time)
                .field("enc_remain_time", enc_remain_time)
                .field("enc", &"[REDACTED]")
                .field("response_digest", response_digest)
                .field("received_at", received_at)
                .finish(),
            Self::Submitted {
                receipt,
                response_digest,
                received_at,
            } => formatter
                .debug_struct("ChaoxingExamSubmissionResponse::Submitted")
                .field("receipt", receipt)
                .field("response_digest", response_digest)
                .field("received_at", received_at)
                .finish(),
        }
    }
}

/// Parses one bounded donor Exam save/final acknowledgement without treating
/// it as completion verification.
///
/// # Errors
///
/// Returns a typed error for malformed, rejected, or protocol-drifted JSON.
pub fn parse_exam_submission_response(
    document: &str,
    final_submit: bool,
    received_at: Timestamp,
) -> ProviderResult<ChaoxingExamSubmissionResponse> {
    if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
        return Err(invalid_response(
            "Chaoxing Exam submission response is empty or unbounded",
        ));
    }
    let mut response: ExamSubmissionWire = serde_json::from_str(document)
        .map_err(|_| protocol_drift("Chaoxing Exam submission response is not valid JSON"))?;
    if response.status != "success" {
        return Err(classify_rejection(response.message.as_deref()));
    }
    let response_digest = Sha256::digest(document.as_bytes()).into();
    if final_submit {
        return Ok(ChaoxingExamSubmissionResponse::Submitted {
            receipt: SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some(
                    "Chaoxing accepted the Exam final submission for fresh verification".to_owned(),
                ),
                provider_trace_id: None,
                received_at,
            },
            response_digest,
            received_at,
        });
    }
    let data = Zeroizing::new(
        response
            .data
            .take()
            .ok_or_else(|| protocol_drift("Chaoxing Exam save response omitted attempt state"))?,
    );
    let mut fields = data.split('|');
    let last_update_time = parse_u64(fields.next())?;
    let enc_remain_time = parse_u64(fields.next())?;
    let enc = fields
        .next()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_FIELD_BYTES
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| protocol_drift("Chaoxing Exam save response has invalid enc"))?
        .to_owned();
    if fields.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing Exam save response has extra attempt fields",
        ));
    }
    Ok(ChaoxingExamSubmissionResponse::Saved {
        last_update_time,
        enc_remain_time,
        enc: SecretString::new(enc),
        response_digest,
        received_at,
    })
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct ExamSubmissionWire {
    status: String,
    #[serde(default, alias = "msg")]
    message: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

struct ExamSignature {
    pos: String,
    rd: String,
    value: String,
    edt: String,
}

impl ExamSignature {
    fn derive(
        user_id: &str,
        question_id: &str,
        entropy: [u8; 32],
        timestamp_millis: u64,
    ) -> ProviderResult<Self> {
        let timestamp = timestamp_millis.to_string();
        if timestamp.len() < 5 || !question_id.is_empty() && !valid_component(question_id) {
            return Err(protocol_drift("Chaoxing Exam signature input is invalid"));
        }
        let r1 = entropy[16] % 9;
        let r2 = entropy[17] % 9;
        let token = hex(&entropy[..16]);
        let seed = format!("{}{}{}{}{}", token, &timestamp[4..], r1, r2, question_id);
        let hash = seed.bytes().fold(0_i64, |value, byte| {
            value.wrapping_mul(31).wrapping_add(i64::from(byte))
        });
        let salt = format!("{}{}{}", r1, r2, (hash & 0x7fff_ffff) % 10);
        let encoded_value = if question_id.is_empty() {
            format!("{user_id}|{salt}")
        } else {
            format!("{user_id}_{question_id}|{salt}")
        };
        let encoded_digits = encoded_value
            .bytes()
            .map(|byte| byte.to_string())
            .collect::<String>();
        if encoded_digits.len() < 10 {
            return Err(protocol_drift(
                "Chaoxing Exam signature identity is too short",
            ));
        }
        let stride = encoded_digits.len() / 5;
        let digit = |index: usize| -> ProviderResult<u64> {
            encoded_digits
                .as_bytes()
                .get(index)
                .and_then(|byte| char::from(*byte).to_digit(10))
                .map(u64::from)
                .ok_or_else(|| protocol_drift("Chaoxing Exam signature digit is invalid"))
        };
        let c = digit(stride)? * 1_000
            + digit(stride * 2)? * 100
            + digit(stride * 3)? * 10
            + digit(stride * 4)?;
        let first_ten = encoded_digits[..10]
            .parse::<u64>()
            .map_err(|_| protocol_drift("Chaoxing Exam signature prefix is invalid"))?;
        let modulus = 0x7fff_ffff_u64;
        let d = u64::try_from(encoded_value.len() / 2 + 1)
            .map_err(|_| protocol_drift("Chaoxing Exam signature length is invalid"))?;
        let mut state =
            (u128::from(c) * u128::from(first_ten) + u128::from(d)) % u128::from(modulus);
        let x = 100 + u16::from_be_bytes([entropy[18], entropy[19]]) % 901;
        let y = 100 + u16::from_be_bytes([entropy[20], entropy[21]]) % 901;
        let value = format!("({x}|{y})");
        let mut pos = String::with_capacity(value.len() * 2 + 8);
        for byte in value.bytes() {
            let mask = u8::try_from((state * 255) / u128::from(modulus))
                .map_err(|_| protocol_drift("Chaoxing Exam signature mask is invalid"))?;
            push_hex_byte(&mut pos, byte ^ mask);
            state = (u128::from(c) * state + u128::from(d)) % u128::from(modulus);
        }
        pos.push_str(&hex(&entropy[24..28]));
        let rd_raw = u64::from_be_bytes(entropy[0..8].try_into().expect("fixed entropy slice"));
        Ok(Self {
            pos,
            rd: format!("0.{:019}", rd_raw % 10_000_000_000_000_000_000_u64),
            value,
            edt: format!("{timestamp}{salt}"),
        })
    }
}

fn append_question_fields(
    fields: &mut Vec<(String, String)>,
    question: &Question,
    answer: &NormalizedAnswer,
) -> ProviderResult<()> {
    let question_id = question
        .remote_question_id
        .as_deref()
        .filter(|value| valid_component(value))
        .ok_or_else(|| protocol_drift("Chaoxing Exam Question identity is invalid"))?;
    let type_code = question
        .metadata_sanitized
        .get("provider_type_code")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| protocol_drift("Chaoxing Exam Question type is missing"))?;
    let (expected_code, type_name) = match question.kind {
        QuestionKind::SingleChoice => (0, "单选题"),
        QuestionKind::MultipleChoice => (1, "多选题"),
        QuestionKind::FillBlank => (2, "填空题"),
        QuestionKind::TrueFalse => (3, "判断题"),
        _ => {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing mobile Exam donor does not encode this Question kind",
            ));
        }
    };
    if type_code != expected_code {
        return Err(protocol_drift(
            "Chaoxing Exam Question kind/type binding changed",
        ));
    }
    fields.extend([
        (format!("type{question_id}"), type_code.to_string()),
        ("questionId".to_owned(), question_id.to_owned()),
        (format!("typeName{question_id}"), type_name.to_owned()),
        ("hidetext".to_owned(), String::new()),
    ]);
    match (question.kind, answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
            if values.len() == 1 && valid_selections(question, values) =>
        {
            fields.push((format!("answer{question_id}"), values[0].clone()));
        }
        (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values))
            if valid_selections(question, values) =>
        {
            let mut values = values.clone();
            values.sort();
            if values.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(protocol_drift(
                    "Chaoxing Exam multiple-choice answer is duplicated",
                ));
            }
            fields.extend(
                values
                    .into_iter()
                    .map(|value| (format!("answers{question_id}"), value)),
            );
        }
        (QuestionKind::FillBlank, NormalizedAnswer::Texts(values))
            if values.len() == 1
                && question
                    .metadata_sanitized
                    .get("blank_count")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1) =>
        {
            for (index, value) in values.iter().enumerate() {
                fields.push((format!("answer{question_id}{}", index + 1), value.clone()));
            }
            fields.push((
                format!("blankNum{question_id}"),
                format!(
                    "{},",
                    (1..=values.len())
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            ));
        }
        (QuestionKind::TrueFalse, NormalizedAnswer::Boolean(value)) => {
            fields.push((
                format!("answer{question_id}"),
                if *value { "true" } else { "false" }.to_owned(),
            ));
        }
        _ => {
            return Err(protocol_drift(
                "Chaoxing Exam answer does not match its Question kind",
            ));
        }
    }
    Ok(())
}

fn valid_selections(question: &Question, values: &[String]) -> bool {
    let options = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    !values.is_empty()
        && values.iter().all(|value| {
            value.len() == 1
                && value.bytes().all(|byte| byte.is_ascii_uppercase())
                && options.contains(value.as_str())
        })
}

fn validate_fields(query: &[(String, String)], body: &[(String, String)]) -> ProviderResult<()> {
    if query.len() > MAX_FORM_FIELDS
        || body.len() > MAX_FORM_FIELDS
        || query.iter().chain(body).any(|(name, value)| {
            name.is_empty()
                || name.len() > 256
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || value.len() > MAX_FIELD_BYTES
                || value.chars().any(|character| {
                    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                })
        })
    {
        return Err(invalid_response(
            "Chaoxing Exam frozen submission fields are invalid or unbounded",
        ));
    }
    Ok(())
}

fn derive_request_digest(
    query: &[(String, String)],
    body: &[(String, String)],
) -> ProviderResult<[u8; 32]> {
    #[derive(Serialize)]
    struct Identity<'a> {
        method: &'static str,
        endpoint: &'static str,
        content_type: &'static str,
        requested_with: &'static str,
        query: &'a [(String, String)],
        body: &'a [(String, String)],
    }
    let encoded = serde_json::to_vec(&Identity {
        method: "POST",
        endpoint: EXAM_SUBMISSION_ENDPOINT,
        content_type: "application/x-www-form-urlencoded",
        requested_with: "com.chaoxing.mobile",
        query,
        body,
    })
    .map_err(|_| invalid_response("Chaoxing Exam request identity could not be encoded"))?;
    Ok(Sha256::digest(encoded).into())
}

fn parse_u64(value: Option<&str>) -> ProviderResult<u64> {
    value
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| protocol_drift("Chaoxing Exam save response has invalid timing state"))
}

fn classify_rejection(message: Option<&str>) -> ProviderError {
    let message = message.unwrap_or_default();
    if message.contains("登录") || message.contains("重新登录") {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Exam submission session is no longer authenticated",
        )
    } else if message.contains("时间已用完") || message.contains("不允许提交") {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing Exam is not currently submittable",
        )
    } else {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing rejected the Exam submission request",
        )
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        push_hex_byte(&mut value, *byte);
    }
    value
}

fn push_hex_byte(value: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    value.push(char::from(HEX[usize::from(byte >> 4)]));
    value.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, NormalizedAnswer, ProviderId, QuestionSnapshotId,
        SelectedAnswer, SubmissionAnswerCoverage, SubmissionDraftId, SubmissionDraftItem, TaskId,
    };
    use asterism_provider_api::{ProviderRouteContext, RemoteCourse, SubmissionBuildCapability};
    use chrono::{TimeZone, Utc};
    use zeroize::Zeroizing;

    use super::*;
    use crate::{
        ChaoxingCourseRoute, ChaoxingExamQuestionRequest, ChaoxingExamStartCommand,
        ChaoxingSubmissionBuild,
        exam_attempt::{ChaoxingExamAttemptMaterial, parse_exam_cover},
        metadata::development_metadata,
        question_parser::parse_exam_question_page,
    };

    const COVER: &str = include_str!("../../../fixtures/providers/chaoxing/exam/cover-ready.html");
    const QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/exam-mobile-mixed.html");
    const FILL_QUESTION: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/exam-mobile-fill.html");
    const SAVE_1: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/submit-save-1.json");
    const SAVE_2: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/submit-save-2.json");
    const FINAL: &str = include_str!("../../../fixtures/providers/chaoxing/exam/submit-final.json");
    const REJECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/submit-rejected.json");

    #[tokio::test]
    async fn frozen_commands_rotate_each_answer_then_issue_one_final_submit() {
        let (draft, artifact) = exam_draft_and_artifact().await;
        let prepared_at = Utc.timestamp_millis_opt(1_700_000_000_500).unwrap();
        let first =
            ChaoxingExamSubmissionCommand::try_new(artifact, &draft, "9001", [7; 32], prepared_at)
                .unwrap();
        assert_eq!(first.operation_type(), EXAM_SAVE_OPERATION);
        assert!(!first.is_final());
        assert!(
            first
                .query()
                .iter()
                .any(|field| field == &("qid".to_owned(), "exam-q-1".to_owned()))
        );
        assert!(
            first
                .body()
                .iter()
                .any(|field| field == &("answerexam-q-1".to_owned(), "A".to_owned()))
        );
        let first_digest = first.request_digest();
        let artifact = accept_save(first, SAVE_1, 1);

        let second =
            ChaoxingExamSubmissionCommand::try_new(artifact, &draft, "9001", [8; 32], prepared_at)
                .unwrap();
        assert_eq!(second.operation_type(), EXAM_SAVE_OPERATION);
        assert!(
            second
                .query()
                .iter()
                .any(|field| field == &("qid".to_owned(), "exam-q-2".to_owned()))
        );
        assert!(
            second
                .body()
                .iter()
                .any(|field| field == &("answerexam-q-2".to_owned(), "true".to_owned()))
        );
        assert_ne!(second.request_digest(), first_digest);
        let second_digest = second.request_digest();
        let artifact = accept_save(second, SAVE_2, 2);

        let final_command =
            ChaoxingExamSubmissionCommand::try_new(artifact, &draft, "9001", [9; 32], prepared_at)
                .unwrap();
        assert_eq!(final_command.operation_type(), EXAM_FINAL_OPERATION);
        assert!(final_command.is_final());
        assert_ne!(final_command.request_digest(), second_digest);
        assert!(
            final_command
                .query()
                .iter()
                .any(|field| field == &("tempSave".to_owned(), "false".to_owned()))
        );
        assert!(
            final_command
                .query()
                .iter()
                .any(|field| field == &("qid".to_owned(), String::new()))
        );
        assert!(!format!("{final_command:?}").contains("SAFE_NEXT_ENC_2"));

        let response = parse_exam_submission_response(FINAL, true, prepared_at).unwrap();
        assert!(matches!(
            response,
            ChaoxingExamSubmissionResponse::Submitted { receipt, .. }
                if receipt.remote_status == "accepted"
        ));
    }

    #[tokio::test]
    async fn partial_command_preserves_the_selected_questions_original_position() {
        let (draft, artifact) = partial_exam_draft_and_artifact().await;
        let prepared_at = Utc.timestamp_millis_opt(1_700_000_000_500).unwrap();
        let command =
            ChaoxingExamSubmissionCommand::try_new(artifact, &draft, "9001", [4; 32], prepared_at)
                .unwrap();
        assert!(!command.is_final());
        assert!(
            command
                .query()
                .iter()
                .any(|field| field == &("qid".to_owned(), "exam-q-2".to_owned()))
        );
        assert!(
            command
                .body()
                .iter()
                .any(|field| field == &("start".to_owned(), "1".to_owned()))
        );
        let artifact = accept_save(command, SAVE_1, 1);
        let final_command =
            ChaoxingExamSubmissionCommand::try_new(artifact, &draft, "9001", [5; 32], prepared_at)
                .unwrap();
        assert!(final_command.is_final());
    }

    #[test]
    fn single_blank_command_fields_require_the_bound_blank_count() {
        let task_id = TaskId::new();
        let mut question = parse_exam_question_page(FILL_QUESTION)
            .unwrap()
            .remove(0)
            .to_question(task_id)
            .unwrap();
        let answer = NormalizedAnswer::Texts(vec!["bounded answer".to_owned()]);
        let mut fields = Vec::new();
        append_question_fields(&mut fields, &question, &answer).unwrap();
        assert!(fields.iter().any(|field| {
            field
                == &(
                    "answerexam-fill-q-11".to_owned(),
                    "bounded answer".to_owned(),
                )
        }));
        assert!(
            fields
                .iter()
                .any(|field| field == &("blankNumexam-fill-q-1".to_owned(), "1,".to_owned()))
        );

        question.metadata_sanitized["blank_count"] = serde_json::json!(2);
        assert_eq!(
            append_question_fields(&mut Vec::new(), &question, &answer)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[tokio::test]
    async fn saved_attempt_state_cannot_regress_or_cross_its_draft_binding() {
        let (draft, artifact) = exam_draft_and_artifact().await;
        let prepared_at = Utc.timestamp_millis_opt(1_700_000_000_500).unwrap();
        let command =
            ChaoxingExamSubmissionCommand::try_new(artifact, &draft, "9001", [1; 32], prepared_at)
                .unwrap();
        let error = command
            .accept_saved(
                1_699_999_999_999,
                3_601,
                &SecretString::new("REGRESSED".to_owned()),
            )
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);

        let rejected = parse_exam_submission_response(REJECTED, false, prepared_at).unwrap_err();
        assert_eq!(rejected.kind, ProviderErrorKind::RemoteChanged);

        let (mut foreign, artifact) = exam_draft_and_artifact().await;
        foreign.items.swap(0, 1);
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let error = ChaoxingExamQuestionArtifact::decode_bound(
            &value,
            digest,
            &foreign,
            "exam:100:200:exam-1",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    fn accept_save(
        command: ChaoxingExamSubmissionCommand,
        document: &str,
        expected_index: u32,
    ) -> ChaoxingExamQuestionArtifact {
        let response = parse_exam_submission_response(document, false, Utc::now()).unwrap();
        let ChaoxingExamSubmissionResponse::Saved {
            last_update_time,
            enc_remain_time,
            enc,
            ..
        } = response
        else {
            panic!("save fixture returned a terminal response");
        };
        let artifact = command
            .accept_saved(last_update_time, enc_remain_time, &enc)
            .unwrap();
        assert_eq!(artifact.next_answer_index(), expected_index);
        artifact
    }

    async fn exam_draft_and_artifact() -> (SubmissionDraft, ChaoxingExamQuestionArtifact) {
        let task_id = TaskId::new();
        let questions = parse_exam_question_page(QUESTIONS)
            .unwrap()
            .into_iter()
            .take(2)
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        let selected = vec![
            selected(
                questions[0].id,
                NormalizedAnswer::Selections(vec!["A".to_owned()]),
            ),
            selected(questions[1].id, NormalizedAnswer::Boolean(true)),
        ];
        let preview = ChaoxingSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(&context(), "exam:100:200:exam-1", &questions, &selected)
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 2,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: questions
                .into_iter()
                .zip(selected)
                .map(|(question, selected)| SubmissionDraftItem { question, selected })
                .collect(),
            payload_preview: preview,
            created_at: Utc::now(),
        };
        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "Exam fixture".to_owned(),
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
            "goTest('100','exam-1',0,'SAFE_TIME','paper-1',false,'SAFE_ENC_TASK')",
        )
        .unwrap();
        let command = ChaoxingExamStartCommand::from_cover(
            task_id,
            "exam:100:200:exam-1",
            &request,
            parse_exam_cover(COVER).unwrap(),
        )
        .unwrap();
        let material = ChaoxingExamAttemptMaterial {
            exam_answer_id: Zeroizing::new("answer-1".to_owned()),
            enc: Zeroizing::new("SAFE_ATTEMPT_ENC".to_owned()),
            enc_remain_time: 3_600,
            remain_time: 3_600,
            last_update_time: 1_700_000_000_000,
        };
        let artifact = ChaoxingExamQuestionArtifact::from_materialization(
            task_id,
            &command,
            material,
            &draft
                .items
                .iter()
                .map(|item| item.question.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let artifact = ChaoxingExamQuestionArtifact::decode_bound(
            &value,
            digest,
            &draft,
            "exam:100:200:exam-1",
        )
        .unwrap();
        (draft, artifact)
    }

    async fn partial_exam_draft_and_artifact() -> (SubmissionDraft, ChaoxingExamQuestionArtifact) {
        let task_id = TaskId::new();
        let questions = parse_exam_question_page(QUESTIONS)
            .unwrap()
            .into_iter()
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        let selected_question = questions[1].clone();
        let selected = selected(selected_question.id, NormalizedAnswer::Boolean(true));
        let preview = ChaoxingSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "exam:100:200:exam-1",
                std::slice::from_ref(&selected_question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 4,
                minimum_coverage_millis: 250,
                unanswered_question_ids: vec![questions[0].id, questions[2].id, questions[3].id],
            },
            items: vec![SubmissionDraftItem {
                question: selected_question,
                selected,
            }],
            payload_preview: preview,
            created_at: Utc::now(),
        };
        draft.validate().unwrap();
        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "Exam fixture".to_owned(),
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
            "goTest('100','exam-1',0,'SAFE_TIME','paper-1',false,'SAFE_ENC_TASK')",
        )
        .unwrap();
        let command = ChaoxingExamStartCommand::from_cover(
            task_id,
            "exam:100:200:exam-1",
            &request,
            parse_exam_cover(COVER).unwrap(),
        )
        .unwrap();
        let artifact = ChaoxingExamQuestionArtifact::from_materialization(
            task_id,
            &command,
            ChaoxingExamAttemptMaterial {
                exam_answer_id: Zeroizing::new("answer-1".to_owned()),
                enc: Zeroizing::new("SAFE_ATTEMPT_ENC".to_owned()),
                enc_remain_time: 3_600,
                remain_time: 3_600,
                last_update_time: 1_700_000_000_000,
            },
            &questions,
        )
        .unwrap();
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let artifact = ChaoxingExamQuestionArtifact::decode_bound(
            &value,
            digest,
            &draft,
            "exam:100:200:exam-1",
        )
        .unwrap();
        (draft, artifact)
    }

    fn selected(
        question_id: asterism_domain::QuestionId,
        answer: NormalizedAnswer,
    ) -> SelectedAnswer {
        SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id,
            answer,
            source: AnswerSource::Manual,
            confidence: None,
        }
    }

    fn context() -> asterism_provider_api::ProviderContext {
        asterism_provider_api::ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: asterism_domain::ProviderAccountId::new(),
            credential_refs: vec![asterism_domain::SecretId::new()],
            correlation_id: "exam-submission-test".to_owned(),
        }
    }
}
