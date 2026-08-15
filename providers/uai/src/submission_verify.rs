use std::{fmt, sync::Arc};

use asterism_domain::{
    ProtocolObservationKind, ProtocolSurface, SubmissionDraft, SubmissionQuestionVerification,
    SubmissionQuestionVerificationStatus, SubmissionReceipt, SubmissionScore,
    SubmissionVerificationSnapshot, SubmissionVerificationStatus, TaskCapability, Timestamp,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, ResolvedProviderQuestionSessionContinuation, SubmissionBuildCapability,
    SubmissionVerifyCapability, TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UAI_QUESTION_SET_ARTIFACT_PHASE, UAI_QUESTION_SET_ARTIFACT_TYPE, UaiQuestionArtifactSet,
    UaiSubmissionBuild, UaiSubmissionPlan, encrypted::ZeroizingJsonValue,
    metadata::development_metadata, submission_execute::valid_submission_version,
    task_type::supports_audited_question_type,
};

const MAX_VERIFICATION_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NESTED_QUESTION_DATA_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_NESTED_ANSWER_BYTES: usize = 1_024 * 1_024;
const MAX_NESTED_CONTEXT_BYTES: usize = 64 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_VERIFICATION_SCORE_ENTRIES: usize = 5_000;

/// Receipt-bound Task scoring and recording facts from one exact user-module.
///
/// The recording flags remain observations only. Neither flag authorizes a
/// completed-Task retake or selects a shared score policy.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiSubmissionPolicyEvidence {
    group_id: String,
    submission_version: String,
    result_digest: [u8; 32],
    strategy_id: u64,
    required: bool,
    recording: UaiSubmissionRecordingPolicy,
    task_minimum_score_milli_percent: u32,
    opens_at: Option<Timestamp>,
    closes_at: Option<Timestamp>,
    submit_state: UaiSubmissionStateEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UaiSubmissionRecordingPolicy {
    every_submit: bool,
    maximum_submit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UaiSubmissionStateEvidence {
    expired: bool,
    not_started: bool,
    last_submit_at: Option<Timestamp>,
}

impl UaiSubmissionPolicyEvidence {
    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub fn submission_version(&self) -> &str {
        &self.submission_version
    }

    pub const fn result_digest(&self) -> [u8; 32] {
        self.result_digest
    }

    pub const fn strategy_id(&self) -> u64 {
        self.strategy_id
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub const fn record_every_submit(&self) -> bool {
        self.recording.every_submit
    }

    pub const fn record_max_submit(&self) -> bool {
        self.recording.maximum_submit
    }

    pub const fn task_minimum_score_milli_percent(&self) -> u32 {
        self.task_minimum_score_milli_percent
    }

    pub const fn opens_at(&self) -> Option<Timestamp> {
        self.opens_at
    }

    pub const fn closes_at(&self) -> Option<Timestamp> {
        self.closes_at
    }

    pub const fn submit_expired(&self) -> bool {
        self.submit_state.expired
    }

    pub const fn submit_not_started(&self) -> bool {
        self.submit_state.not_started
    }

    pub const fn last_submit_at(&self) -> Option<Timestamp> {
        self.submit_state.last_submit_at
    }
}

impl fmt::Debug for UaiSubmissionPolicyEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionPolicyEvidence")
            .field("binding", &"[REDACTED]")
            .field("strategy_id", &self.strategy_id)
            .field("required", &self.required)
            .field("recording", &self.recording)
            .field(
                "task_minimum_score_milli_percent",
                &self.task_minimum_score_milli_percent,
            )
            .field("opens_at", &self.opens_at)
            .field("closes_at", &self.closes_at)
            .field("submit_state", &self.submit_state)
            .finish_non_exhaustive()
    }
}

impl Drop for UaiSubmissionPolicyEvidence {
    fn drop(&mut self) {
        self.group_id.zeroize();
        self.submission_version.zeroize();
        self.result_digest.zeroize();
    }
}

/// Shared verification plus the independently preserved Provider policy facts.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiVerificationEvidenceSnapshot {
    verification: SubmissionVerificationSnapshot,
    policy: Option<UaiSubmissionPolicyEvidence>,
}

impl UaiVerificationEvidenceSnapshot {
    pub const fn verification(&self) -> &SubmissionVerificationSnapshot {
        &self.verification
    }

    pub const fn policy(&self) -> Option<&UaiSubmissionPolicyEvidence> {
        self.policy.as_ref()
    }

    pub fn into_verification(self) -> SubmissionVerificationSnapshot {
        self.verification
    }
}

/// Redacted ownership wrapper for one bounded fresh UAI user-module response.
pub struct UaiVerificationDocument(String);

impl UaiVerificationDocument {
    /// Owns one bounded JSON response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized documents.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_VERIFICATION_DOCUMENT_BYTES {
            return Err(invalid_response(
                "UAI submission verification response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiVerificationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiVerificationDocument")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiVerificationDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Read-only boundary for one exact receipt-versioned user-module snapshot.
#[async_trait]
pub trait UaiVerificationTransport: Send + Sync {
    async fn fetch_verification(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
        submission_version: &str,
    ) -> ProviderResult<UaiVerificationDocument>;
}

/// Distinct UAI post-submit verification capability. It never issues the
/// submission mutation and does not infer Task completion from a receipt.
pub struct UaiSubmissionVerify {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    preview: UaiSubmissionBuild,
    transport: Arc<dyn UaiVerificationTransport>,
}

impl UaiSubmissionVerify {
    /// Builds verification around fresh Task detail and user-module reads.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn UaiVerificationTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            preview: UaiSubmissionBuild::try_new()?,
            transport,
        })
    }
}

impl fmt::Debug for UaiSubmissionVerify {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionVerify")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("preview", &self.preview)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiSubmissionVerify {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionVerifyCapability for UaiSubmissionVerify {
    async fn verify_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        validate_context(context, &self.metadata)?;
        let identity = GroupIdentity::parse(remote_task_id)?;
        validate_draft(draft, &self.metadata)?;
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = draft
            .items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        let expected_preview = self
            .preview
            .build_submission_preview(context, remote_task_id, &questions, &selected)
            .await?;
        if expected_preview != draft.payload_preview {
            return Err(invalid_response(
                "UAI submission verification draft preview is stale or foreign",
            ));
        }
        let Some(receipt) = receipt else {
            return inconclusive_snapshot(draft);
        };
        receipt
            .validate()
            .map_err(|_| invalid_response("UAI submission verification receipt is invalid"))?;
        let version = receipt
            .provider_trace_id
            .as_deref()
            .filter(|value| valid_submission_version(value))
            .filter(|_| receipt.remote_status == "accepted")
            .ok_or_else(|| {
                invalid_response("UAI submission verification requires an accepted version receipt")
            })?;

        let detail = self.details.task_detail(context, remote_task_id).await?;
        let task_types =
            validate_fresh_detail(&detail, &identity, remote_task_id, draft.items.len())?;
        let plan = UaiSubmissionPlan::from_draft(draft, &task_types)?;
        let document = self
            .transport
            .fetch_verification(context, &identity.course_resource, &identity.group, version)
            .await?;
        parse_verification_snapshot(document.as_str(), &identity.group, version, &plan, draft)
    }

    async fn verify_submission_with_session(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        validate_context(context, &self.metadata)?;
        if continuation.continuation_type != UAI_QUESTION_SET_ARTIFACT_TYPE
            || continuation.phase != UAI_QUESTION_SET_ARTIFACT_PHASE
            || continuation.revision == 0
        {
            return Err(protocol_drift(
                "UAI verification continuation metadata is stale or foreign",
            ));
        }
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        UaiQuestionArtifactSet::decode_bound(
            continuation.value,
            continuation.continuation_digest,
            remote_task_id,
            &questions,
        )?;
        self.verify_submission(context, remote_task_id, draft, receipt)
            .await
    }
}

/// Parses and strictly binds one fresh user-module response to the exact draft
/// answer and receipt version.
///
/// # Errors
///
/// Returns invalid-response, protocol-drift, or remote-changed errors for
/// malformed nesting, duplicate identities, route drift, or changed answers.
pub fn parse_verification_snapshot(
    document: &str,
    expected_group_id: &str,
    expected_version: &str,
    plan: &UaiSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    parse_verification_evidence_snapshot(document, expected_group_id, expected_version, plan, draft)
        .map(UaiVerificationEvidenceSnapshot::into_verification)
}

/// Parses one exact user-module into shared verification and independently
/// bound Provider-private scoring/recording policy evidence.
///
/// # Errors
///
/// Returns invalid-response, protocol-drift, or remote-changed errors for
/// malformed nesting, partial policy material, route drift or changed answers.
pub fn parse_verification_evidence_snapshot(
    document: &str,
    expected_group_id: &str,
    expected_version: &str,
    plan: &UaiSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<UaiVerificationEvidenceSnapshot> {
    if document.is_empty() || document.len() > MAX_VERIFICATION_DOCUMENT_BYTES {
        return Err(invalid_response(
            "UAI submission verification response is empty or exceeds the size limit",
        ));
    }
    let response =
        ZeroizingJsonValue::new(serde_json::from_str(document).map_err(|_| {
            invalid_response("UAI submission verification response is not valid JSON")
        })?);
    let state = bound_verification_state(response.as_value(), expected_group_id, expected_version)?;
    parse_remote_questions(state, plan, draft.items.len())?;
    let score = verified_submission_score(state)?;
    let policy = verified_submission_policy(
        state,
        expected_group_id,
        expected_version,
        Sha256::digest(document.as_bytes()).into(),
    )?;

    let snapshot = SubmissionVerificationSnapshot {
        status: SubmissionVerificationStatus::Confirmed,
        remote_state: None,
        score,
        progress_percent: None,
        questions: draft
            .items
            .iter()
            .map(|item| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: SubmissionQuestionVerificationStatus::Confirmed,
            })
            .collect(),
        verified_at: Utc::now(),
    };
    snapshot
        .validate()
        .map_err(|_| invalid_response("UAI submission verification snapshot is invalid"))?;
    Ok(UaiVerificationEvidenceSnapshot {
        verification: snapshot,
        policy,
    })
}

pub(crate) fn bound_verification_state<'a>(
    response: &'a Value,
    expected_group_id: &str,
    expected_version: &str,
) -> ProviderResult<&'a serde_json::Map<String, Value>> {
    let response = response
        .as_object()
        .ok_or_else(|| protocol_drift("UAI submission verification response is not an object"))?;
    if response.get("success").and_then(Value::as_bool) != Some(true)
        || response.get("code").and_then(Value::as_i64) != Some(0)
    {
        return Err(invalid_response(
            "UAI submission verification read did not succeed",
        ));
    }
    let data = response
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI submission verification has no data object"))?;
    let expected_module = format!("{expected_group_id}-{expected_version}");
    if data.get("module").and_then(Value::as_str) != Some(expected_module.as_str()) {
        return Err(remote_changed(
            "UAI user-module identity does not match the receipt binding",
        ));
    }
    let state = data
        .get("state")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI submission verification has no state object"))?;
    if state.get("version").and_then(Value::as_str) != Some(expected_version) {
        return Err(remote_changed(
            "UAI user-module version does not match the submission receipt",
        ));
    }
    let submit_info = state
        .get("__EXTEND_DATA__")
        .and_then(Value::as_object)
        .and_then(|value| value.get("__SUBMIT_INFO__"))
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI user-module has no submit-info binding"))?;
    if submit_info.get("group_id").and_then(Value::as_str) != Some(expected_group_id)
        || submit_info.get("version").and_then(Value::as_str) != Some(expected_version)
    {
        return Err(remote_changed(
            "UAI user-module submit-info does not match the receipt binding",
        ));
    }
    Ok(state)
}

pub(crate) fn verified_submission_score(
    state: &serde_json::Map<String, Value>,
) -> ProviderResult<Option<SubmissionScore>> {
    let extend = state
        .get("__EXTEND_DATA__")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI user-module has no extended verification state"))?;
    let Some(summary) = extend.get("__SUMMARY__") else {
        return Ok(None);
    };
    if summary.is_null() {
        return Ok(None);
    }
    let answer_list_value = summary
        .as_object()
        .and_then(|summary| summary.get("answerList"));
    let answer_list = answer_list_value
        .and_then(Value::as_object)
        .filter(|answers| answers.len() <= MAX_VERIFICATION_SCORE_ENTRIES)
        .ok_or_else(|| unknown_score_summary(summary, answer_list_value))?;
    let mut counted = false;
    for answer in answer_list.values() {
        let question_type = answer
            .as_object()
            .and_then(|answer| answer.get("questionType"))
            .and_then(Value::as_u64)
            .filter(|value| *value <= 32)
            .ok_or_else(|| protocol_drift("UAI user-module score entry has no bounded type"))?;
        counted |= matches!(question_type, 1 | 3);
    }
    if !counted {
        return Ok(None);
    }
    let score = extend
        .get("__SUBMIT_INFO__")
        .and_then(Value::as_object)
        .and_then(|submit| submit.get("state"))
        .and_then(Value::as_object)
        .and_then(|state| state.get("score_avg"))
        .ok_or_else(|| protocol_drift("UAI scored user-module has no average score"))?;
    let encoded = match score {
        Value::Number(value) => value.to_string(),
        Value::String(value)
            if !value.is_empty()
                && value.len() <= 32
                && value.trim() == value
                && value.is_ascii() =>
        {
            value.clone()
        }
        _ => return Err(protocol_drift("UAI user-module average score is invalid")),
    };
    let score = SubmissionScore {
        earned_milli_points: decimal_milli_points(&encoded)?,
        possible_milli_points: 100_000,
    };
    score
        .validate()
        .map_err(|_| protocol_drift("UAI user-module average score is out of range"))?;
    Ok(Some(score))
}

fn unknown_score_summary(summary: &Value, answer_list: Option<&Value>) -> ProviderError {
    let error = ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI user-module score summary is invalid or oversized",
    );
    error
        .try_with_protocol_observation(
            ProtocolSurface::SubmissionVerify,
            ProtocolObservationKind::UnknownResultShape,
            serde_json::json!({
                "schema": "uai.score-summary-observation.v1",
                "summary_kind": json_value_kind(summary),
                "answer_list_present": answer_list.is_some(),
                "answer_list_kind": answer_list.map(json_value_kind),
                "answer_list_entries": answer_list.and_then(collection_len),
            }),
        )
        .unwrap_or_else(|_| protocol_drift("UAI score-summary observation could not be sanitized"))
}

const fn json_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn collection_len(value: &Value) -> Option<usize> {
    match value {
        Value::Array(values) => Some(values.len()),
        Value::Object(values) => Some(values.len()),
        _ => None,
    }
}

pub(crate) fn verified_submission_policy(
    state: &serde_json::Map<String, Value>,
    expected_group_id: &str,
    expected_version: &str,
    result_digest: [u8; 32],
) -> ProviderResult<Option<UaiSubmissionPolicyEvidence>> {
    let submit = state
        .get("__EXTEND_DATA__")
        .and_then(Value::as_object)
        .and_then(|extend| extend.get("__SUBMIT_INFO__"))
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI user-module has no submit-info binding"))?;
    let strategy_id = submit.get("strategyId");
    let strategy = submit.get("strategy");
    if strategy_id.is_none() && strategy.is_none() {
        return Ok(None);
    }
    let strategy_id = strategy_id
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| protocol_drift("UAI user-module strategy identity is invalid"))?;
    let strategy = strategy
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI user-module strategy is missing or invalid"))?;
    let submit_state = submit
        .get("state")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI user-module policy has no submit state"))?;

    let start = required_policy_epoch(strategy.get("startTime"), "start time")?;
    let end = required_policy_epoch(strategy.get("endTime"), "end time")?;
    let (opens_at, closes_at) = policy_window(start, end)?;
    let last_submit = required_policy_epoch(submit_state.get("lastSubmit"), "last submit")?;
    let last_submit_at = if last_submit == 0 {
        None
    } else {
        Some(policy_timestamp(last_submit, "last submit")?)
    };

    Ok(Some(UaiSubmissionPolicyEvidence {
        group_id: expected_group_id.to_owned(),
        submission_version: expected_version.to_owned(),
        result_digest,
        strategy_id,
        required: required_policy_bool(strategy.get("required"), "required")?,
        recording: UaiSubmissionRecordingPolicy {
            every_submit: required_policy_bool(
                strategy.get("record_every_submit"),
                "record-every-submit",
            )?,
            maximum_submit: required_policy_bool(
                strategy.get("record_max_submit"),
                "record-max-submit",
            )?,
        },
        task_minimum_score_milli_percent: policy_milli_percent(
            strategy.get("task_mini_score_pct"),
        )?,
        opens_at,
        closes_at,
        submit_state: UaiSubmissionStateEvidence {
            expired: required_policy_bool(submit_state.get("expired"), "expired")?,
            not_started: required_policy_bool(submit_state.get("not_start"), "not-started")?,
            last_submit_at,
        },
    }))
}

fn required_policy_bool(value: Option<&Value>, label: &'static str) -> ProviderResult<bool> {
    value
        .as_ref()
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                format!("UAI user-module policy {label} is invalid"),
            )
        })
}

fn required_policy_epoch(value: Option<&Value>, label: &'static str) -> ProviderResult<i64> {
    value
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                format!("UAI user-module policy {label} is invalid"),
            )
        })
}

fn policy_window(start: i64, end: i64) -> ProviderResult<(Option<Timestamp>, Option<Timestamp>)> {
    if start == 0 || end == 0 {
        return Ok((None, None));
    }
    if start >= end {
        return Err(protocol_drift(
            "UAI user-module policy availability window is invalid",
        ));
    }
    Ok((
        Some(policy_timestamp(start, "start time")?),
        Some(policy_timestamp(end, "end time")?),
    ))
}

fn policy_timestamp(value: i64, label: &'static str) -> ProviderResult<Timestamp> {
    chrono::DateTime::from_timestamp(value, 0).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("UAI user-module policy {label} is out of range"),
        )
    })
}

fn policy_milli_percent(value: Option<&Value>) -> ProviderResult<u32> {
    let encoded = match value {
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::String(value))
            if !value.is_empty()
                && value.len() <= 32
                && value.trim() == value
                && value.is_ascii() =>
        {
            value.clone()
        }
        _ => {
            return Err(protocol_drift(
                "UAI user-module Task minimum score is invalid",
            ));
        }
    };
    decimal_milli_points(&encoded)
        .and_then(|value| {
            u32::try_from(value)
                .map_err(|_| protocol_drift("UAI user-module Task minimum score is out of range"))
        })
        .map_err(|_| protocol_drift("UAI user-module Task minimum score is invalid"))
}

fn decimal_milli_points(encoded: &str) -> ProviderResult<u64> {
    let (whole, fractional) = encoded.split_once('.').map_or((encoded, ""), |parts| parts);
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift("UAI user-module average score is invalid"));
    }
    let whole = whole
        .parse::<u64>()
        .ok()
        .filter(|whole| *whole <= 100)
        .ok_or_else(|| protocol_drift("UAI user-module average score is out of range"))?;
    if whole == 100 && fractional.bytes().any(|byte| byte != b'0') {
        return Err(protocol_drift(
            "UAI user-module average score is out of range",
        ));
    }
    let mut digits = fractional.bytes();
    let hundreds = digits.next().map_or(0, |byte| u64::from(byte - b'0'));
    let tens = digits.next().map_or(0, |byte| u64::from(byte - b'0'));
    let units = digits.next().map_or(0, |byte| u64::from(byte - b'0'));
    let rounds_up = digits.next().is_some_and(|byte| byte >= b'5');
    let fractional_milli_points = hundreds * 100 + tens * 10 + units + u64::from(rounds_up);
    whole
        .checked_mul(1_000)
        .and_then(|whole| whole.checked_add(fractional_milli_points))
        .filter(|score| *score <= 100_000)
        .ok_or_else(|| protocol_drift("UAI user-module average score is out of range"))
}

fn parse_remote_questions(
    state: &serde_json::Map<String, Value>,
    plan: &UaiSubmissionPlan,
    expected_count: usize,
) -> ProviderResult<()> {
    let question_data = Zeroizing::new(
        state
            .get("quesData")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_QUESTION_DATA_BYTES)
            .ok_or_else(|| protocol_drift("UAI user-module has no bounded Question data"))?
            .to_owned(),
    );
    let question_data_value = ZeroizingJsonValue::new(
        serde_json::from_str(&question_data)
            .map_err(|_| invalid_response("UAI user-module Question data is not valid JSON"))?,
    );
    let entries = question_data_value
        .as_value()
        .as_array()
        .ok_or_else(|| protocol_drift("UAI user-module Question data is not an array"))?;
    if entries.len() != expected_count || plan.questions().len() != expected_count {
        return Err(remote_changed(
            "UAI user-module Question count does not match the submission draft",
        ));
    }
    for (entry, planned) in entries.iter().zip(plan.questions()) {
        parse_remote_question(entry, planned)?;
    }
    Ok(())
}

pub(crate) fn parse_remote_question(
    entry: &Value,
    planned: &crate::UaiSubmissionQuestionPlan,
) -> ProviderResult<()> {
    let entry = entry
        .as_object()
        .ok_or_else(|| protocol_drift("UAI user-module Question entry is not an object"))?;
    if entry.get("instanceId").and_then(remote_identity).as_deref()
        != Some(planned.remote_question_id())
    {
        return Err(remote_changed(
            "UAI user-module Question identity does not match the submission draft",
        ));
    }
    let context = entry
        .get("context")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_CONTEXT_BYTES)
        .ok_or_else(|| protocol_drift("UAI user-module has no bounded submission context"))?;
    let context =
        ZeroizingJsonValue::new(serde_json::from_str(context).map_err(|_| {
            invalid_response("UAI user-module submission context is not valid JSON")
        })?);
    if context.as_value().get("state").and_then(Value::as_str) != Some("submitted") {
        return Err(remote_changed(
            "UAI user-module Question is not in submitted state",
        ));
    }
    let answer = Zeroizing::new(
        entry
            .get("answer")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_ANSWER_BYTES)
            .ok_or_else(|| protocol_drift("UAI user-module has no bounded answer data"))?
            .to_owned(),
    );
    let answer_value = ZeroizingJsonValue::new(
        serde_json::from_str(&answer)
            .map_err(|_| invalid_response("UAI user-module answer data is not valid JSON"))?,
    );
    let children = answer_value
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI user-module answer has no children"))?;
    let mut remote_children = Zeroizing::new(Vec::with_capacity(children.len()));
    for child in children {
        if child.get("isDone").and_then(Value::as_bool) != Some(true) {
            return Err(remote_changed(
                "UAI user-module answer child is not marked submitted",
            ));
        }
        let values = child
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI user-module answer child has no values"))?;
        let mut normalized = Zeroizing::new(Vec::with_capacity(values.len()));
        for value in values {
            let value = value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| protocol_drift("UAI user-module answer contains invalid text"))?;
            normalized.push(value.to_owned());
        }
        remote_children.push(std::mem::take(&mut *normalized));
    }
    if remote_children.as_slice() != planned.answer_children() {
        return Err(remote_changed(
            "UAI user-module answer differs from the immutable submission draft",
        ));
    }
    Ok(())
}

pub(crate) fn validate_verification_course_binding(
    document: &str,
    expected_course_instance_id: &str,
) -> ProviderResult<()> {
    let response =
        ZeroizingJsonValue::new(serde_json::from_str(document).map_err(|_| {
            invalid_response("UAI submission verification response is not valid JSON")
        })?);
    let data = response
        .as_value()
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI submission verification has no data object"))?;
    let course = data
        .get("course")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI submission verification has no Course binding"))?;
    let submit_course = data
        .get("state")
        .and_then(Value::as_object)
        .and_then(|state| state.get("__EXTEND_DATA__"))
        .and_then(Value::as_object)
        .and_then(|value| value.get("__SUBMIT_INFO__"))
        .and_then(Value::as_object)
        .and_then(|submit| submit.get("course_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            protocol_drift("UAI submission verification has no submit-info Course binding")
        })?;
    if course != expected_course_instance_id || submit_course != expected_course_instance_id {
        return Err(remote_changed(
            "UAI submission verification Course does not match its fresh route",
        ));
    }
    Ok(())
}

fn inconclusive_snapshot(
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let snapshot = SubmissionVerificationSnapshot {
        status: SubmissionVerificationStatus::Inconclusive,
        remote_state: None,
        score: None,
        progress_percent: None,
        questions: draft
            .items
            .iter()
            .map(|item| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: SubmissionQuestionVerificationStatus::Unverified,
            })
            .collect(),
        verified_at: Utc::now(),
    };
    snapshot
        .validate()
        .map_err(|_| invalid_response("UAI inconclusive verification snapshot is invalid"))?;
    Ok(snapshot)
}

fn validate_draft(draft: &SubmissionDraft, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if draft.provider_id != metadata.id
        || draft.provider_version != metadata.implementation_version
        || draft.validate().is_err()
    {
        return Err(invalid_response(
            "UAI submission verification received an invalid or stale draft",
        ));
    }
    Ok(())
}

fn validate_fresh_detail(
    detail: &asterism_provider_api::RemoteTaskDetail,
    identity: &GroupIdentity,
    remote_task_id: &str,
    expected_question_count: usize,
) -> ProviderResult<Vec<String>> {
    if detail.task.remote_id != remote_task_id
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionVerify)
    {
        return Err(unsupported(
            "UAI fresh Group does not advertise submission verification",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI fresh Group detail has no normalized Task"))?;
    if task.get("course_resource_id").and_then(Value::as_str)
        != Some(identity.course_resource.as_str())
        || task
            .get("unit")
            .and_then(Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(Value::as_str)
            != Some(identity.unit.as_str())
        || task.get("group_id").and_then(Value::as_str) != Some(identity.group.as_str())
        || task
            .get("question_count")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            != Some(expected_question_count)
    {
        return Err(remote_changed(
            "UAI Group identity or Question count changed before verification",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI fresh Group detail has no task types"))?;
    if task_types.is_empty()
        || (task_types.len() != 1 && task_types.len() != expected_question_count)
    {
        return Err(unsupported(
            "UAI submission verification requires one shared or one-per-Question task type",
        ));
    }
    task_types
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| supports_audited_question_type(value))
                .map(str::to_owned)
                .ok_or_else(|| unsupported("UAI fresh Group task type is not verifiable"))
        })
        .collect()
}

struct GroupIdentity {
    course_resource: String,
    unit: String,
    group: String,
}

impl GroupIdentity {
    fn parse(value: &str) -> ProviderResult<Self> {
        if value.is_empty()
            || value.len() > MAX_REMOTE_TASK_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(invalid_response("UAI Group Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("group") {
            return Err(unsupported(
                "UAI submission verification supports Group Tasks only",
            ));
        }
        let course_resource = valid_component(components.next())?;
        let unit = valid_component(components.next())?;
        let group = valid_component(components.next())?;
        if components.next().is_some() {
            return Err(invalid_response("UAI Group Task identity is invalid"));
        }
        Ok(Self {
            course_resource,
            unit,
            group,
        })
    }
}

fn valid_component(value: Option<&str>) -> ProviderResult<String> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_REMOTE_COMPONENT_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        .map(str::to_owned)
        .ok_or_else(|| invalid_response("UAI Group Task identity is invalid"))
}

fn remote_identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI submission verification received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI submission verification requires an authenticated session",
        ));
    }
    Ok(())
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderAccountId, ProviderId, QuestionSnapshotId,
        SecretId, SelectedAnswer, SubmissionAnswerCoverage, SubmissionDraftId, SubmissionDraftItem,
        TaskId,
    };
    use asterism_provider_api::RemoteTaskDetail;

    use super::*;
    use crate::{
        parse_course_context, parse_course_inventory, parse_question_content, parse_task_inventory,
    };

    const VERIFIED: &str =
        include_str!("../../../fixtures/providers/uai/submissions/verified.json");
    const SCORED_VERIFIED: &str =
        include_str!("../../../fixtures/providers/uai/submissions/verified-scored.json");
    const MIXED_VERIFIED: &str =
        include_str!("../../../fixtures/providers/uai/submissions/verified-mixed-simple.json");
    const CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-multiple-choice.json");
    const MIXED_CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-mixed-simple.json");
    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str = include_str!("../../../fixtures/providers/uai/tasks/tree-mixed.json");

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        multiple: bool,
    }

    impl FixtureDetail {
        fn single() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                multiple: false,
            }
        }

        fn multiple() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                multiple: true,
            }
        }
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let course = parse_course_inventory(COURSES)?.remove(0);
            let context = parse_course_context(&course, DETAIL)?;
            let tree = if self.multiple {
                TREE.replace(
                    r#"\"base\":\"rich-text-read\",\"question_num\":1"#,
                    r#"\"base\":\"multichoice,short_answer\",\"question_num\":2"#,
                )
            } else {
                TREE.replace("rich-text-read", "multichoice")
            };
            let task = parse_task_inventory(&course, &context, &tree)?
                .into_iter()
                .find(|task| task.remote_id == remote_task_id)
                .ok_or_else(|| protocol_drift("synthetic UAI Group is missing"))?;
            Ok(RemoteTaskDetail {
                normalized_detail: serde_json::json!({
                    "schema": "uai.group-task-detail.v1",
                    "task": task.normalized.clone(),
                }),
                task,
            })
        }
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<(String, String, String)>>,
        document: Option<String>,
    }

    #[async_trait]
    impl UaiVerificationTransport for FixtureTransport {
        async fn fetch_verification(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            group_id: &str,
            submission_version: &str,
        ) -> ProviderResult<UaiVerificationDocument> {
            self.calls.lock().unwrap().push((
                course_resource_id.to_owned(),
                group_id.to_owned(),
                submission_version.to_owned(),
            ));
            UaiVerificationDocument::try_new(
                self.document.clone().unwrap_or_else(|| VERIFIED.to_owned()),
            )
        }
    }

    #[tokio::test]
    async fn exact_user_module_snapshot_confirms_the_submitted_question_only() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();
        let snapshot =
            parse_verification_snapshot(VERIFIED, "group-1", "submit-version-42", &plan, &draft)
                .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.remote_state, None);
        assert_eq!(snapshot.score, None);
        assert_eq!(snapshot.questions.len(), 1);
        assert_eq!(
            snapshot.questions[0].status,
            SubmissionQuestionVerificationStatus::Confirmed
        );
        assert!(!format!("{snapshot:?}").contains("must_be_dropped"));
        validate_verification_course_binding(VERIFIED, "course-v2:synthetic+rw").unwrap();
        assert!(validate_verification_course_binding(VERIFIED, "changed").is_err());
        let changed_module = VERIFIED.replace(
            r#""module":"group-1-submit-version-42""#,
            r#""module":"group-1-changed""#,
        );
        assert_eq!(
            parse_verification_snapshot(
                &changed_module,
                "group-1",
                "submit-version-42",
                &plan,
                &draft,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
        let changed_course = VERIFIED.replace(
            r#""course_id":"course-v2:synthetic+rw""#,
            r#""course_id":"changed""#,
        );
        assert_eq!(
            validate_verification_course_binding(&changed_course, "course-v2:synthetic+rw")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn fresh_counted_user_module_exposes_fixed_point_score() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();
        let snapshot = parse_verification_snapshot(
            SCORED_VERIFIED,
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap();
        assert_eq!(
            snapshot.score,
            Some(SubmissionScore {
                earned_milli_points: 37_500,
                possible_milli_points: 100_000,
            })
        );

        let mut zero_score: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        zero_score["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["state"]["score_avg"] =
            serde_json::json!(0);
        let snapshot = parse_verification_snapshot(
            &serde_json::to_string(&zero_score).unwrap(),
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap();
        assert_eq!(snapshot.score.unwrap().earned_milli_points, 0);

        let mut uncounted = zero_score.clone();
        uncounted["data"]["state"]["__EXTEND_DATA__"]["__SUMMARY__"]["answerList"]["0"]["questionType"] =
            serde_json::json!(2);
        let snapshot = parse_verification_snapshot(
            &serde_json::to_string(&uncounted).unwrap(),
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap();
        assert_eq!(snapshot.score, None);

        let mut invalid = zero_score;
        invalid["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["state"]["score_avg"] =
            serde_json::json!(100.001);
        assert!(
            parse_verification_snapshot(
                &serde_json::to_string(&invalid).unwrap(),
                "group-1",
                "submit-version-42",
                &plan,
                &draft,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn invalid_score_summary_emits_shape_only_observation() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();
        let mut document: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        document["data"]["state"]["__EXTEND_DATA__"]["__SUMMARY__"]["answerList"] = serde_json::json!([{
            "student_answer": "must-not-cross",
            "authorization": "must-not-cross",
        }]);
        let error = parse_verification_snapshot(
            &serde_json::to_string(&document).unwrap(),
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::SubmissionVerify);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            observation.shape_sanitized,
            serde_json::json!({
                "schema": "uai.score-summary-observation.v1",
                "summary_kind": "object",
                "answer_list_present": true,
                "answer_list_kind": "array",
                "answer_list_entries": 1,
            })
        );
        let encoded = serde_json::to_string(&observation.shape_sanitized).unwrap();
        assert!(!encoded.contains("student_answer"));
        assert!(!encoded.contains("must-not-cross"));
        assert!(!encoded.contains("authorization"));
    }

    #[tokio::test]
    async fn receipt_bound_user_module_preserves_task_policy_without_retaking() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();
        let evidence = parse_verification_evidence_snapshot(
            SCORED_VERIFIED,
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap();
        assert_eq!(
            evidence.verification().score,
            Some(SubmissionScore {
                earned_milli_points: 37_500,
                possible_milli_points: 100_000,
            })
        );
        let policy = evidence.policy().unwrap();
        assert_eq!(policy.group_id(), "group-1");
        assert_eq!(policy.submission_version(), "submit-version-42");
        assert_eq!(policy.strategy_id(), 3001);
        assert!(policy.required());
        assert!(!policy.record_every_submit());
        assert!(policy.record_max_submit());
        assert_eq!(policy.task_minimum_score_milli_percent(), 60_000);
        assert_eq!(policy.opens_at().unwrap().timestamp(), 1_785_542_400);
        assert_eq!(policy.closes_at().unwrap().timestamp(), 1_790_812_800);
        assert_eq!(policy.last_submit_at().unwrap().timestamp(), 1_786_752_000);
        assert!(!policy.submit_expired());
        assert!(!policy.submit_not_started());
        assert_eq!(
            policy.result_digest(),
            <[u8; 32]>::from(Sha256::digest(SCORED_VERIFIED.as_bytes()))
        );
        let debug = format!("{policy:?}");
        assert!(!debug.contains("group-1"));
        assert!(!debug.contains("submit-version-42"));

        let legacy = parse_verification_evidence_snapshot(
            VERIFIED,
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap();
        assert!(legacy.policy().is_none());
    }

    #[tokio::test]
    async fn partial_or_invalid_user_module_policy_fails_closed() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();
        let mut invalid_documents = Vec::new();

        let mut missing_strategy: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        missing_strategy["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]
            .as_object_mut()
            .unwrap()
            .remove("strategy");
        invalid_documents.push(missing_strategy);

        let mut zero_strategy: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        zero_strategy["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["strategyId"] =
            serde_json::json!(0);
        invalid_documents.push(zero_strategy);

        let mut invalid_boolean: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        invalid_boolean["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["strategy"]["record_max_submit"] =
            serde_json::json!(1);
        invalid_documents.push(invalid_boolean);

        let mut invalid_threshold: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        invalid_threshold["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["strategy"]["task_mini_score_pct"] =
            serde_json::json!(100.001);
        invalid_documents.push(invalid_threshold);

        let mut invalid_window: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        invalid_window["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["strategy"]["startTime"] =
            serde_json::json!(1_800_000_000);
        invalid_documents.push(invalid_window);

        let mut invalid_last_submit: Value = serde_json::from_str(SCORED_VERIFIED).unwrap();
        invalid_last_submit["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]["state"]["lastSubmit"] =
            serde_json::json!(-1);
        invalid_documents.push(invalid_last_submit);

        for document in invalid_documents {
            assert!(
                parse_verification_evidence_snapshot(
                    &serde_json::to_string(&document).unwrap(),
                    "group-1",
                    "submit-version-42",
                    &plan,
                    &draft,
                )
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn exact_multi_question_readback_confirms_every_draft_item() {
        let draft = multiple_draft().await;
        let plan = UaiSubmissionPlan::from_draft(
            &draft,
            &["multichoice".to_owned(), "short_answer".to_owned()],
        )
        .unwrap();
        let snapshot = parse_verification_snapshot(
            MIXED_VERIFIED,
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.questions.len(), 2);
        assert!(snapshot.questions.iter().all(|question| {
            question.status == SubmissionQuestionVerificationStatus::Confirmed
        }));

        let transport = Arc::new(FixtureTransport {
            calls: Mutex::new(Vec::new()),
            document: Some(MIXED_VERIFIED.to_owned()),
        });
        let capability =
            UaiSubmissionVerify::try_new(Arc::new(FixtureDetail::multiple()), transport.clone())
                .unwrap();
        let snapshot = capability
            .verify_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                Some(&receipt()),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.questions.len(), 2);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reordered_multi_question_readback_fails_closed() {
        let draft = multiple_draft().await;
        let plan = UaiSubmissionPlan::from_draft(
            &draft,
            &["multichoice".to_owned(), "short_answer".to_owned()],
        )
        .unwrap();
        let mut document: Value = serde_json::from_str(MIXED_VERIFIED).unwrap();
        let mut question_data: Value =
            serde_json::from_str(document["data"]["state"]["quesData"].as_str().unwrap()).unwrap();
        question_data.as_array_mut().unwrap().swap(0, 1);
        document["data"]["state"]["quesData"] =
            Value::String(serde_json::to_string(&question_data).unwrap());
        let error = parse_verification_snapshot(
            &serde_json::to_string(&document).unwrap(),
            "group-1",
            "submit-version-42",
            &plan,
            &draft,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    #[tokio::test]
    async fn verifier_reads_the_exact_receipt_version_without_mutating() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionVerify::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let draft = draft().await;
        let snapshot = capability
            .verify_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                Some(&receipt()),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[(
                "2001".to_owned(),
                "group-1".to_owned(),
                "submit-version-42".to_owned(),
            )]
        );
    }

    #[tokio::test]
    async fn media_session_verification_rebinds_artifact_before_readback() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionVerify::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let (draft, value, digest) = media_draft_and_artifact().await;
        let snapshot = capability
            .verify_submission_with_session(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                Some(&receipt()),
                ResolvedProviderQuestionSessionContinuation {
                    continuation_type: UAI_QUESTION_SET_ARTIFACT_TYPE,
                    continuation_digest: digest,
                    phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
                    revision: 1,
                    value: &value,
                },
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);

        let error = capability
            .verify_submission_with_session(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                Some(&receipt()),
                ResolvedProviderQuestionSessionContinuation {
                    continuation_type: "uai.foreign-artifact.v1",
                    continuation_digest: digest,
                    phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
                    revision: 1,
                    value: &value,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_receipt_is_inconclusive_without_guessing_a_route() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionVerify::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let snapshot = capability
            .verify_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft().await,
                None,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);
        assert_eq!(
            snapshot.questions[0].status,
            SubmissionQuestionVerificationStatus::Unverified
        );
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_remote_answer_fails_closed() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();
        let mut changed: Value = serde_json::from_str(VERIFIED).unwrap();
        let mut question_data: Value =
            serde_json::from_str(changed["data"]["state"]["quesData"].as_str().unwrap()).unwrap();
        let mut answer: Value =
            serde_json::from_str(question_data[0]["answer"].as_str().unwrap()).unwrap();
        answer["children"][0]["value"][1] = Value::String("C".to_owned());
        question_data[0]["answer"] = Value::String(serde_json::to_string(&answer).unwrap());
        changed["data"]["state"]["quesData"] =
            Value::String(serde_json::to_string(&question_data).unwrap());
        let changed = serde_json::to_string(&changed).unwrap();
        assert_eq!(
            parse_verification_snapshot(&changed, "group-1", "submit-version-42", &plan, &draft,)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    async fn draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let mut question = parse_question_content(
            CONTENT,
            "group:2001:unit-1:group-1",
            &["multichoice".to_owned()],
            Some(1),
        )
        .unwrap()
        .remove(0)
        .to_question(task_id)
        .unwrap();
        question.metadata_sanitized["judge_types"] = serde_json::json!([
            {"question_type": "multichoice", "reply_type": "multichoice"}
        ]);
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: asterism_domain::NormalizedAnswer::Selections(vec![
                "A".to_owned(),
                "B".to_owned(),
            ]),
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: asterism_domain::ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    async fn media_draft_and_artifact() -> (SubmissionDraft, asterism_secrets::SecretValue, [u8; 32])
    {
        let remote_task_id = "group:2001:unit-1:group-1";
        let task_id = TaskId::new();
        let parsed = crate::question::parse_question_entry(
            &serde_json::json!({
                "id": "1001",
                "content": {
                    "type": "basic",
                    "direction": {"text": "Choose every matching answer"},
                    "contents": [{
                        "name": "Listening.mp3",
                        "path": "https://media.example.edu/listening.mp3"
                    }],
                    "children": [{
                        "type": "basic",
                        "replyType": "multichoice",
                        "options": [
                            {"name": "A", "text": "Alpha"},
                            {"name": "B", "text": "Beta"}
                        ]
                    }]
                }
            }),
            1,
            "multichoice",
            remote_task_id,
        )
        .unwrap();
        let question = parsed.to_question(task_id).unwrap();
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: asterism_domain::NormalizedAnswer::Selections(vec![
                "A".to_owned(),
                "B".to_owned(),
            ]),
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                remote_task_id,
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            std::slice::from_ref(&parsed),
            std::slice::from_ref(&question),
            remote_task_id,
        )
        .unwrap()
        .unwrap()
        .encode()
        .unwrap();
        let digest = artifact.digest();
        let value = artifact.into_secret_value();
        (
            SubmissionDraft {
                id: SubmissionDraftId::new(),
                task_id,
                question_snapshot_id: QuestionSnapshotId::new(),
                provider_id: ProviderId::new("uai").unwrap(),
                provider_version: development_metadata().unwrap().implementation_version,
                answer_coverage: SubmissionAnswerCoverage {
                    total_question_count: 1,
                    minimum_coverage_millis: 1_000,
                    unanswered_question_ids: Vec::new(),
                },
                items: vec![SubmissionDraftItem { question, selected }],
                payload_preview: preview,
                created_at: Utc::now(),
            },
            value,
            digest,
        )
    }

    async fn multiple_draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let questions = parse_question_content(
            MIXED_CONTENT,
            "group:2001:unit-1:group-1",
            &["multichoice".to_owned(), "short_answer".to_owned()],
            Some(2),
        )
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
        let answers = [
            asterism_domain::NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
            asterism_domain::NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()]),
        ];
        let items = questions
            .into_iter()
            .zip(answers)
            .map(|(question, answer)| SubmissionDraftItem {
                selected: SelectedAnswer {
                    candidate_id: AnswerCandidateId::new(),
                    question_id: question.id,
                    answer,
                    source: AnswerSource::Manual,
                    confidence: None,
                },
                question,
            })
            .collect::<Vec<_>>();
        let questions = items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                &questions,
                &selected,
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id: items[0].question.task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: u32::try_from(items.len()).unwrap(),
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items,
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    fn receipt() -> SubmissionReceipt {
        SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some(
                "UAI accepted the submission for later verification".to_owned(),
            ),
            provider_trace_id: Some("submit-version-42".to_owned()),
            received_at: Utc::now(),
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            correlation_id: "uai-submission-verify".to_owned(),
            credential_refs: vec![SecretId::new()],
        }
    }
}
