use std::{collections::BTreeSet, fmt};

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTaskDetail};
use serde_json::{Number, Value, json};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::CidarenAttemptResponseIdentity;

const START_VERSION: &str = "2.6.1.240122";
const START_OPTION_FONT_COLOR: &str = "%23000000";
const VERIFY_VERSION: &str = "2.6.1.231204";
const ADVANCE_VERSION: &str = "2.6.2.24031302";
const SIGNING_SUFFIX: &str = "ajfajfamsnfaflfasakljdlalkflak";
const MAX_TOPIC_CODE_BYTES: usize = 4_096;
const MAX_ANSWER_BYTES: usize = 32 * 1_024;
const MAX_WORDS: usize = 100_000;
const MAX_WORD_BYTES: usize = 1_024;
const MAX_WORD_MAP_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_TIME_SPENT_MILLIS: u64 = 24 * 60 * 60 * 1_000;

/// Fresh Task binding used only while constructing one Cidaren assessment
/// request. Stable Asterism identity is checked against the complete fresh
/// Task detail before its mutable `task_id` observation is accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CidarenAssessmentBinding {
    family: CidarenTaskFamily,
    task_id: i64,
    // The decoded payload echoes the inventory row family (class learning 1,
    // class test 2, ordinary study 3), which is distinct from StartAnswer's
    // donor-fixed request selector for every class Task.
    response_task_type: u8,
    stable_binding: String,
}

impl CidarenAssessmentBinding {
    /// Rebinds one class/study Task to a freshly read detail.
    ///
    /// # Errors
    ///
    /// Returns `RemoteChanged` or `ProtocolDrift` if any stable identity,
    /// family, route observation or mutable task ID disagrees.
    pub fn from_fresh_detail(
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
    ) -> ProviderResult<Self> {
        if detail.task.remote_id != remote_task_id {
            return Err(remote_changed(
                "Cidaren fresh Task detail changed the requested identity",
            ));
        }
        let task = detail
            .normalized_detail
            .get("task")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("Cidaren fresh Task detail has no task object"))?;
        let task_id = task
            .get("task_id")
            .and_then(Value::as_i64)
            .filter(|value| *value == -1 || *value > 0)
            .ok_or_else(|| protocol_drift("Cidaren fresh Task detail has an invalid task ID"))?;

        if let Some(release_id) = remote_task_id.strip_prefix("class-task:") {
            let response_task_type = match task.get("task_type").and_then(Value::as_str) {
                Some("learning") => 1,
                Some("test") => 2,
                _ => {
                    return Err(protocol_drift(
                        "Cidaren fresh class Task binding has an unknown task type",
                    ));
                }
            };
            if detail
                .normalized_detail
                .get("schema")
                .and_then(Value::as_str)
                != Some("cidaren.class-task.detail.v1")
                || detail
                    .normalized_detail
                    .get("release_id")
                    .and_then(Value::as_str)
                    != Some(release_id)
                || task.get("release_id").and_then(Value::as_str) != Some(release_id)
                || !valid_positive_decimal(release_id)
            {
                return Err(protocol_drift(
                    "Cidaren fresh class Task binding is inconsistent",
                ));
            }
            return Ok(Self {
                family: CidarenTaskFamily::Class,
                task_id,
                response_task_type,
                stable_binding: release_id.to_owned(),
            });
        }

        let identity = remote_task_id
            .strip_prefix("study-task:")
            .and_then(|value| value.split_once(':'))
            .filter(|(course_id, list_id)| valid_component(course_id) && valid_component(list_id))
            .ok_or_else(|| protocol_drift("Cidaren study Task identity is invalid"))?;
        if detail
            .normalized_detail
            .get("schema")
            .and_then(Value::as_str)
            != Some("cidaren.study-task.detail.v1")
            || detail
                .normalized_detail
                .get("course_id")
                .and_then(Value::as_str)
                != Some(identity.0)
            || detail
                .normalized_detail
                .get("list_id")
                .and_then(Value::as_str)
                != Some(identity.1)
            || task.get("course_id").and_then(Value::as_str) != Some(identity.0)
            || task.get("list_id").and_then(Value::as_str) != Some(identity.1)
            || task.get("task_type").and_then(Value::as_str) != Some("study")
        {
            return Err(protocol_drift(
                "Cidaren fresh study Task binding is inconsistent",
            ));
        }
        Ok(Self {
            family: CidarenTaskFamily::Study,
            task_id,
            response_task_type: 3,
            stable_binding: identity.0.to_owned(),
        })
    }

    pub(crate) fn validate_attempt_response_identity(
        &self,
        identity: CidarenAttemptResponseIdentity,
    ) -> ProviderResult<()> {
        if identity.task_type() != self.response_task_type
            || (self.task_id != -1 && identity.task_id() != self.task_id)
        {
            return Err(remote_changed(
                "Cidaren attempt response belongs to another Task",
            ));
        }
        Ok(())
    }

    pub(crate) fn rebind_remote_attempt_task_id(
        &self,
        remote_attempt_task_id: Option<i64>,
    ) -> ProviderResult<Option<i64>> {
        if remote_attempt_task_id.is_some_and(|task_id| task_id <= 0) {
            return Err(protocol_drift(
                "Cidaren remote attempt Task allocation is invalid",
            ));
        }
        if self.task_id > 0 {
            if remote_attempt_task_id.is_some_and(|task_id| task_id != self.task_id) {
                return Err(remote_changed(
                    "Cidaren fresh Task row changed the remote attempt allocation",
                ));
            }
            return Ok(Some(self.task_id));
        }
        Ok(remote_attempt_task_id)
    }

    fn endpoint(&self, operation: &str) -> String {
        format!("{}/{operation}", self.family.endpoint_family())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CidarenTaskFamily {
    Class,
    Study,
}

impl CidarenTaskFamily {
    const fn endpoint_family(self) -> &'static str {
        match self {
            Self::Class => "ClassTask",
            Self::Study => "StudyTask",
        }
    }

    const fn start_request_task_type(self) -> u8 {
        match self {
            Self::Class => 2,
            Self::Study => 3,
        }
    }
}

/// Exact donor-observed `StartAnswer` request facts without credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CidarenStartAnswerRequest {
    pub path: String,
    pub query: Vec<(String, String)>,
}

impl CidarenStartAnswerRequest {
    /// Hashes the exact credential-free GET request semantics in query order.
    pub fn request_digest(&self) -> [u8; 32] {
        let mut hasher = request_digest_prefix(b"GET", self.path.as_bytes(), b"read");
        for (key, value) in &self.query {
            digest_frame(&mut hasher, key.as_bytes());
            digest_frame(&mut hasher, value.as_bytes());
        }
        hasher.finalize().into()
    }
}

/// Builds the non-replayable `StartAnswer` request after fresh Task rebinding.
/// The caller must persist an attempt before issuing this request and must not
/// automatically replay it after an ambiguous network outcome.
pub fn build_start_answer_request(
    binding: &CidarenAssessmentBinding,
    timestamp_millis: u64,
) -> CidarenStartAnswerRequest {
    let mut query = vec![
        ("task_id".to_owned(), binding.task_id.to_string()),
        (
            "task_type".to_owned(),
            binding.family.start_request_task_type().to_string(),
        ),
        ("opt_img_w".to_owned(), "684".to_owned()),
        ("opt_font_size".to_owned(), "37".to_owned()),
        ("opt_font_c".to_owned(), START_OPTION_FONT_COLOR.to_owned()),
        ("it_img_w".to_owned(), "804".to_owned()),
        ("it_font_size".to_owned(), "42".to_owned()),
        ("timestamp".to_owned(), timestamp_millis.to_string()),
        ("version".to_owned(), START_VERSION.to_owned()),
        ("app_type".to_owned(), "1".to_owned()),
    ];
    query.push((
        match binding.family {
            CidarenTaskFamily::Class => "release_id",
            CidarenTaskFamily::Study => "course_id",
        }
        .to_owned(),
        binding.stable_binding.clone(),
    ));
    CidarenStartAnswerRequest {
        path: binding.endpoint("StartAnswer"),
        query,
    }
}

/// Exact wire value accepted by `VerifyAnswer`. Debug output is redacted and
/// owned text is zeroized on drop.
pub enum CidarenWireAnswer {
    Number(u64),
    Text(Zeroizing<String>),
}

impl CidarenWireAnswer {
    /// Decodes an immutable normalized option ID produced by the Cidaren
    /// Question parser.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for foreign, empty, oversized or malformed IDs.
    pub fn from_option_id(option_id: &str) -> ProviderResult<Self> {
        if let Some(value) = option_id.strip_prefix("n:") {
            return value
                .parse::<u64>()
                .ok()
                .filter(|value| *value <= 1_000_000)
                .map(Self::Number)
                .ok_or_else(|| invalid_input("Cidaren selected option ID is invalid"));
        }
        option_id
            .strip_prefix("s:")
            .filter(|value| valid_text(value, MAX_ANSWER_BYTES))
            .map(|value| Self::Text(Zeroizing::new(value.to_owned())))
            .ok_or_else(|| invalid_input("Cidaren selected option ID is invalid"))
    }

    /// Builds one bounded free-text answer.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for empty, oversized or control-bearing text.
    pub fn from_text(value: &str) -> ProviderResult<Self> {
        valid_text(value, MAX_ANSWER_BYTES)
            .then(|| Self::Text(Zeroizing::new(value.to_owned())))
            .ok_or_else(|| invalid_input("Cidaren text answer is invalid"))
    }

    fn json_value(&self) -> Value {
        match self {
            Self::Number(value) => Value::Number(Number::from(*value)),
            Self::Text(value) => Value::String(value.to_string()),
        }
    }

    fn signature_value(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Text(value) => value.to_string(),
        }
    }
}

impl fmt::Debug for CidarenWireAnswer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => formatter.debug_tuple("Number").field(value).finish(),
            Self::Text(_) => formatter.write_str("Text([REDACTED])"),
        }
    }
}

/// One credential-free, in-memory Cidaren mutation request. The serialized
/// body may contain a one-time topic code or selected answer and is zeroized
/// when dropped. It is never a Submission Draft payload.
pub struct CidarenMutationRequest {
    path: String,
    body: Zeroizing<Vec<u8>>,
    authorization: CidarenMutationAuthorization,
}

impl CidarenMutationRequest {
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exposes the transient body only to the authenticated Provider transport.
    /// Callers must not persist or log these bytes.
    pub fn body_bytes(&self) -> &[u8] {
        self.body.as_slice()
    }

    pub(crate) const fn authorization(&self) -> CidarenMutationAuthorization {
        self.authorization
    }

    /// Hashes the exact credential-free POST path, authorization family and
    /// serialized body which the native transport will send.
    pub fn request_digest(&self) -> [u8; 32] {
        let mut hasher = request_digest_prefix(
            b"POST",
            self.path.as_bytes(),
            self.authorization.digest_label(),
        );
        digest_frame(&mut hasher, self.body.as_slice());
        hasher.finalize().into()
    }
}

impl fmt::Debug for CidarenMutationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenMutationRequest")
            .field("path", &self.path)
            .field("body", &"[REDACTED]")
            .field("authorization", &self.authorization)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CidarenMutationAuthorization {
    Read,
    Submit,
}

impl CidarenMutationAuthorization {
    const fn digest_label(self) -> &'static [u8] {
        match self {
            Self::Read => b"read",
            Self::Submit => b"submit",
        }
    }
}

/// Builds one `VerifyAnswer` mutation. Matching questions call this only for
/// the current relation, then bind the next response topic code before the
/// next call; a batch of stale topic-code mutations is never prebuilt.
///
/// # Errors
///
/// Returns `InvalidResponse` for an invalid topic code or a serialization error.
pub fn build_verify_answer_request(
    binding: &CidarenAssessmentBinding,
    topic_code: &str,
    answer: &CidarenWireAnswer,
    timestamp_millis: u64,
) -> ProviderResult<CidarenMutationRequest> {
    validate_topic_code(topic_code)?;
    let signature_value = Zeroizing::new(answer.signature_value());
    let signature_input = Zeroizing::new(format!(
        "answer={}&timestamp={timestamp_millis}&topic_code={topic_code}&version={VERIFY_VERSION}{SIGNING_SUFFIX}",
        signature_value.as_str()
    ));
    let sign = format!("{:x}", md5::compute(signature_input.as_bytes()));
    mutation_request(
        binding.endpoint("VerifyAnswer"),
        json!({
            "answer": answer.json_value(),
            "topic_code": topic_code,
            "timestamp": timestamp_millis,
            "version": VERIFY_VERSION,
            "sign": sign,
            "app_type": 1,
        }),
        CidarenMutationAuthorization::Submit,
    )
}

/// Builds the donor's `SubmitAnswerAndSave` advance mutation after a verified
/// answer. This receipt is not completion verification.
///
/// # Errors
///
/// Returns `InvalidResponse` for invalid topic/time fields.
pub fn build_submit_answer_and_save_request(
    binding: &CidarenAssessmentBinding,
    topic_code: &str,
    time_spent_millis: u64,
    timestamp_millis: u64,
) -> ProviderResult<CidarenMutationRequest> {
    build_advance_request(
        binding,
        "SubmitAnswerAndSave",
        topic_code,
        time_spent_millis,
        timestamp_millis,
    )
}

/// Builds the donor's explicit `SkipAnswer` mutation.
///
/// # Errors
///
/// Returns `InvalidResponse` for invalid topic/time fields.
pub fn build_skip_answer_request(
    binding: &CidarenAssessmentBinding,
    topic_code: &str,
    time_spent_millis: u64,
    timestamp_millis: u64,
) -> ProviderResult<CidarenMutationRequest> {
    build_advance_request(
        binding,
        "SkipAnswer",
        topic_code,
        time_spent_millis,
        timestamp_millis,
    )
}

fn build_advance_request(
    binding: &CidarenAssessmentBinding,
    operation: &str,
    topic_code: &str,
    time_spent_millis: u64,
    timestamp_millis: u64,
) -> ProviderResult<CidarenMutationRequest> {
    validate_topic_code(topic_code)?;
    if time_spent_millis == 0 || time_spent_millis > MAX_TIME_SPENT_MILLIS {
        return Err(invalid_input("Cidaren answer duration is invalid"));
    }
    let signature_input = Zeroizing::new(format!(
        "it_font_size=42&it_img_w=804&opt_font_c=#000000&opt_font_size=37&opt_img_w=684&time_spent={time_spent_millis}&timestamp={timestamp_millis}&topic_code={topic_code}&version={ADVANCE_VERSION}{SIGNING_SUFFIX}"
    ));
    let sign = format!("{:x}", md5::compute(signature_input.as_bytes()));
    mutation_request(
        binding.endpoint(operation),
        json!({
            "it_font_size": 42,
            "it_img_w": 804,
            "opt_font_c": "#000000",
            "opt_font_size": 37,
            "opt_img_w": 684,
            "time_spent": time_spent_millis,
            "timestamp": timestamp_millis,
            "topic_code": topic_code,
            "version": ADVANCE_VERSION,
            "sign": sign,
        }),
        CidarenMutationAuthorization::Submit,
    )
}

/// Builds `SubmitChoseWord` for the exact donor-observed list/object word-map
/// shapes. Word-map content is bounded and zeroized with the request body.
///
/// # Errors
///
/// Returns `InvalidResponse` for malformed word maps or invalid serialized size.
pub fn build_submit_chose_word_request(
    binding: &CidarenAssessmentBinding,
    word_map: &Value,
    timestamp_millis: u64,
) -> ProviderResult<CidarenMutationRequest> {
    validate_word_map(word_map)?;
    let encoded_word_map = Zeroizing::new(
        serde_json::to_string(word_map)
            .map_err(|_| invalid_input("Cidaren word map cannot be encoded"))?,
    );
    if encoded_word_map.len() > MAX_WORD_MAP_BYTES {
        return Err(invalid_input("Cidaren word map exceeds the size limit"));
    }
    let signature_input = Zeroizing::new(format!(
        "chose_err_item=2&task_id={}&timestamp={timestamp_millis}&version={VERIFY_VERSION}&word_map={}{SIGNING_SUFFIX}",
        binding.task_id,
        encoded_word_map.as_str()
    ));
    let sign = format!("{:x}", md5::compute(signature_input.as_bytes()));
    mutation_request(
        binding.endpoint("SubmitChoseWord"),
        json!({
            "task_id": binding.task_id,
            "word_map": word_map,
            "chose_err_item": 2,
            "timestamp": timestamp_millis,
            "version": VERIFY_VERSION,
            "sign": sign,
            "app_type": 1,
        }),
        CidarenMutationAuthorization::Read,
    )
}

fn mutation_request(
    path: String,
    mut value: Value,
    authorization: CidarenMutationAuthorization,
) -> ProviderResult<CidarenMutationRequest> {
    let body = serde_json::to_vec(&value);
    zeroize_json(&mut value);
    let body = body.map_err(|_| invalid_input("Cidaren mutation body cannot be encoded"))?;
    Ok(CidarenMutationRequest {
        path,
        body: Zeroizing::new(body),
        authorization,
    })
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(values) => values.values_mut().for_each(zeroize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn request_digest_prefix(method: &[u8], path: &[u8], authorization: &[u8]) -> Sha256 {
    let mut hasher = Sha256::new();
    digest_frame(&mut hasher, b"cidaren.assessment-request.v1");
    digest_frame(&mut hasher, method);
    digest_frame(&mut hasher, path);
    digest_frame(&mut hasher, authorization);
    hasher
}

fn digest_frame(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn validate_word_map(value: &Value) -> ProviderResult<()> {
    let mut words = Vec::new();
    match value {
        Value::Array(values) => words.extend(values),
        Value::Object(entries) if !entries.is_empty() && entries.len() <= 1_000 => {
            for (key, value) in entries {
                if !valid_word_map_key(key) {
                    return Err(invalid_input("Cidaren word-map key is invalid"));
                }
                words.extend(
                    value
                        .as_array()
                        .ok_or_else(|| invalid_input("Cidaren word-map value is not an array"))?,
                );
            }
        }
        _ => return Err(invalid_input("Cidaren word map has an unknown shape")),
    }
    if words.is_empty() || words.len() > MAX_WORDS {
        return Err(invalid_input("Cidaren word-map count is invalid"));
    }
    let mut unique = BTreeSet::new();
    for word in words {
        let word = word
            .as_str()
            .filter(|word| valid_text(word, MAX_WORD_BYTES))
            .ok_or_else(|| invalid_input("Cidaren word map contains an invalid word"))?;
        if !unique.insert(word) {
            return Err(invalid_input("Cidaren word map contains duplicate words"));
        }
    }
    Ok(())
}

fn validate_topic_code(value: &str) -> ProviderResult<()> {
    if valid_text(value, MAX_TOPIC_CODE_BYTES) {
        Ok(())
    } else {
        Err(invalid_input("Cidaren topic code is invalid"))
    }
}

fn valid_positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value != "0"
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_word_map_key(value: &str) -> bool {
    valid_component(value)
        || value.split_once(':').is_some_and(|(course_id, list_id)| {
            valid_component(course_id) && valid_component(list_id)
        })
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn remote_changed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_input(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AssessmentClass, RemoteState, SourceType};
    use asterism_provider_api::RemoteTask;
    use serde_json::Map;

    use super::*;

    #[test]
    fn fresh_binding_and_start_request_preserve_exact_family_semantics() {
        let class = class_binding();
        let request = build_start_answer_request(&class, 1_710_000_000_000);
        assert_eq!(request.path, "ClassTask/StartAnswer");
        assert_eq!(query(&request, "task_id"), Some("-1"));
        assert_eq!(query(&request, "task_type"), Some("2"));
        assert_eq!(query(&request, "release_id"), Some("2002"));
        assert_eq!(query(&request, "opt_font_c"), Some(START_OPTION_FONT_COLOR));
        assert_eq!(query(&request, "version"), Some(START_VERSION));

        let study = study_binding();
        let request = build_start_answer_request(&study, 1_710_000_000_000);
        assert_eq!(request.path, "StudyTask/StartAnswer");
        assert_eq!(query(&request, "task_id"), Some("71002"));
        assert_eq!(query(&request, "task_type"), Some("3"));
        assert_eq!(query(&request, "course_id"), Some("course-a"));
        assert!(query(&request, "release_id").is_none());
    }

    #[test]
    fn class_learning_keeps_request_selector_separate_from_response_identity() {
        let mut detail = class_detail();
        detail.task.normalized["task_type"] = json!("learning");
        detail.normalized_detail["task"]["task_type"] = json!("learning");
        let binding =
            CidarenAssessmentBinding::from_fresh_detail("class-task:2002", &detail).unwrap();

        let request = build_start_answer_request(&binding, 1_710_000_000_000);
        assert_eq!(request.path, "ClassTask/StartAnswer");
        assert_eq!(query(&request, "task_type"), Some("2"));
        assert_eq!(query(&request, "release_id"), Some("2002"));

        let mut payload: Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/cidaren/questions/start-answer-fill-blank-73.json"
        ))
        .unwrap();
        payload["task_type"] = json!(1);
        let identity = crate::parse_attempt_question(&payload, "class-task:2002", 1)
            .unwrap()
            .response_identity()
            .unwrap();
        binding
            .validate_attempt_response_identity(identity)
            .unwrap();
        payload["task_type"] = json!(2);
        let wrong_identity = crate::parse_attempt_question(&payload, "class-task:2002", 1)
            .unwrap()
            .response_identity()
            .unwrap();
        assert_eq!(
            binding
                .validate_attempt_response_identity(wrong_identity)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn signed_mutation_vectors_match_frozen_donor_order() {
        let binding = class_binding();
        let verify = build_verify_answer_request(
            &binding,
            "synthetic-topic",
            &CidarenWireAnswer::from_option_id("n:2").unwrap(),
            1_710_000_000_000,
        )
        .unwrap();
        assert_eq!(verify.path(), "ClassTask/VerifyAnswer");
        assert_eq!(verify.authorization(), CidarenMutationAuthorization::Submit);
        let verify_body = body(&verify);
        assert_eq!(verify_body["answer"], 2);
        assert_eq!(
            verify_body["sign"],
            format!(
                "{:x}",
                md5::compute(b"answer=2&timestamp=1710000000000&topic_code=synthetic-topic&version=2.6.1.231204ajfajfamsnfaflfasakljdlalkflak")
            )
        );

        let advance = build_submit_answer_and_save_request(
            &binding,
            "synthetic-topic",
            25_000,
            1_710_000_000_000,
        )
        .unwrap();
        assert_eq!(advance.path(), "ClassTask/SubmitAnswerAndSave");
        let advance_body = body(&advance);
        assert_eq!(advance_body["time_spent"], 25_000);
        assert_eq!(
            advance_body["sign"],
            format!(
                "{:x}",
                md5::compute(b"it_font_size=42&it_img_w=804&opt_font_c=#000000&opt_font_size=37&opt_img_w=684&time_spent=25000&timestamp=1710000000000&topic_code=synthetic-topic&version=2.6.2.24031302ajfajfamsnfaflfasakljdlalkflak")
            )
        );

        let skip =
            build_skip_answer_request(&binding, "synthetic-topic", 20_000, 1_710_000_000_000)
                .unwrap();
        assert_eq!(skip.path(), "ClassTask/SkipAnswer");
        assert!(!format!("{skip:?}").contains("synthetic-topic"));
    }

    #[test]
    fn request_digests_bind_exact_frozen_transport_semantics() {
        let binding = class_binding();
        let start = build_start_answer_request(&binding, 1_710_000_000_000);
        let same_start = build_start_answer_request(&binding, 1_710_000_000_000);
        let later_start = build_start_answer_request(&binding, 1_710_000_000_001);
        assert_eq!(start.request_digest(), same_start.request_digest());
        assert_ne!(start.request_digest(), later_start.request_digest());

        let first = build_verify_answer_request(
            &binding,
            "synthetic-topic",
            &CidarenWireAnswer::from_option_id("n:1").unwrap(),
            1_710_000_000_000,
        )
        .unwrap();
        let same = build_verify_answer_request(
            &binding,
            "synthetic-topic",
            &CidarenWireAnswer::from_option_id("n:1").unwrap(),
            1_710_000_000_000,
        )
        .unwrap();
        let changed_answer = build_verify_answer_request(
            &binding,
            "synthetic-topic",
            &CidarenWireAnswer::from_option_id("n:2").unwrap(),
            1_710_000_000_000,
        )
        .unwrap();
        assert_eq!(first.request_digest(), same.request_digest());
        assert_ne!(first.request_digest(), changed_answer.request_digest());
    }

    #[test]
    fn chose_word_and_answer_values_are_bounded_and_redacted() {
        let binding = study_binding();
        let request = build_submit_chose_word_request(
            &binding,
            &json!({"course-a:list-a": ["alpha", "beta"]}),
            1_710_000_000_000,
        )
        .unwrap();
        assert_eq!(request.path(), "StudyTask/SubmitChoseWord");
        assert_eq!(request.authorization(), CidarenMutationAuthorization::Read);
        assert_eq!(body(&request)["task_id"], 71_002);
        assert!(!format!("{request:?}").contains("alpha"));

        let text = CidarenWireAnswer::from_text("synthetic answer").unwrap();
        assert!(!format!("{text:?}").contains("synthetic"));
        assert!(CidarenWireAnswer::from_option_id("foreign").is_err());
        assert!(CidarenWireAnswer::from_text("").is_err());
        assert!(
            build_submit_chose_word_request(&binding, &json!(["duplicate", "duplicate"]), 1)
                .is_err()
        );
    }

    #[test]
    fn binding_rejects_stale_or_cross_family_detail() {
        let mut detail = class_detail();
        detail.task.remote_id = "class-task:9999".to_owned();
        assert_eq!(
            CidarenAssessmentBinding::from_fresh_detail("class-task:2002", &detail)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );

        let mut detail = class_detail();
        detail.normalized_detail["release_id"] = json!("9999");
        assert_eq!(
            CidarenAssessmentBinding::from_fresh_detail("class-task:2002", &detail)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        assert!(
            CidarenAssessmentBinding::from_fresh_detail("study-task:course-a:list-a", &detail)
                .is_err()
        );
    }

    fn query<'a>(request: &'a CidarenStartAnswerRequest, name: &str) -> Option<&'a str> {
        request
            .query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn body(request: &CidarenMutationRequest) -> Value {
        serde_json::from_slice(request.body_bytes()).unwrap()
    }

    fn class_binding() -> CidarenAssessmentBinding {
        CidarenAssessmentBinding::from_fresh_detail("class-task:2002", &class_detail()).unwrap()
    }

    fn study_binding() -> CidarenAssessmentBinding {
        CidarenAssessmentBinding::from_fresh_detail("study-task:course-a:list-a", &study_detail())
            .unwrap()
    }

    fn class_detail() -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": -1,
            "course_id": "course-a",
            "task_type": "test",
            "progress": 35,
        });
        RemoteTaskDetail {
            task: task("class-task:2002", normalized.clone()),
            normalized_detail: json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": "2002",
                "task": normalized,
            }),
        }
    }

    fn study_detail() -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "cidaren.study-task.v1",
            "task_id": 71002,
            "course_id": "course-a",
            "list_id": "list-a",
            "task_type": "study",
            "progress": 35,
        });
        RemoteTaskDetail {
            task: task("study-task:course-a:list-a", normalized.clone()),
            normalized_detail: json!({
                "schema": "cidaren.study-task.detail.v1",
                "course_id": "course-a",
                "list_id": "list-a",
                "task": normalized,
            }),
        }
    }

    fn task(remote_id: &str, normalized: Value) -> RemoteTask {
        RemoteTask {
            remote_id: remote_id.to_owned(),
            course_remote_id: Some("course:course-a".to_owned()),
            title: "Synthetic Task".to_owned(),
            source_type: SourceType::Exam,
            assessment_class: AssessmentClass::Routine,
            remote_state: RemoteState::InProgress,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities: Vec::new(),
            fingerprint: "synthetic-fingerprint".to_owned(),
            normalized,
            raw_sanitized: Map::new().into(),
        }
    }
}
