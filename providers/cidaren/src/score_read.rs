use asterism_domain::{ProtocolObservationKind, ProtocolSurface, SubmissionScore};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, RemoteTaskDetail,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use zeroize::Zeroize;

use crate::{CidarenCryptoContext, decode_response_data};

const CLASS_TASK_INFO_PATH: &str = "ClassTask/Info";
const STUDY_TASK_INFO_PATH: &str = "StudyTask/Info";
const SCORE_REQUEST_VERSION: &str = "2.6.1.240122";
const MAX_SCORE_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_COMPONENT_BYTES: usize = 256;
const SCORE_ALIASES: [&str; 3] = ["score", "task_score", "grade"];

/// Fresh post-run score boundary reconstructed from the reopened public donor.
///
/// The transport receives a Task that was already rediscovered by
/// `TaskDetailCapability`. Implementations must bind the route to that exact
/// class release or study Course/Task identity and must not issue mutations.
#[async_trait]
pub trait CidarenTaskScoreTransport: Send + Sync {
    /// Reads the freshest available score observation for one rebound Task.
    async fn fetch_task_score(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
    ) -> ProviderResult<Option<SubmissionScore>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CidarenTaskScoreRequest {
    pub(crate) path: &'static str,
    pub(crate) query: Vec<(&'static str, String)>,
}

/// Builds the exact donor-observed `Info` read after validating every identity
/// against the freshly rediscovered normalized Task.
///
/// A study row with `task_id=-1` has no unique route in the donor's score
/// request because that request omits `list_id`. It therefore returns `None`
/// so the caller can retain the already fresh list score instead of issuing an
/// ambiguous cross-unit read. Class rows remain uniquely bound by release ID.
pub(crate) fn build_task_score_request(
    remote_task_id: &str,
    detail: &RemoteTaskDetail,
    timestamp: u64,
) -> ProviderResult<Option<CidarenTaskScoreRequest>> {
    if detail.task.remote_id != remote_task_id
        || detail.normalized_detail.get("task") != Some(&detail.task.normalized)
    {
        return Err(remote_changed(
            "Cidaren score read received stale or mismatched fresh Task detail",
        ));
    }
    let schema = detail.task.normalized.get("schema").and_then(Value::as_str);
    let task_id = required_task_id(detail.task.normalized.get("task_id"))?;
    let mut query = vec![("task_id", task_id.to_string())];
    if let Some(release_id) = remote_task_id.strip_prefix("class-task:") {
        let release_id = required_component(release_id, "release ID")?;
        if schema != Some("cidaren.class-task.v1")
            || detail
                .normalized_detail
                .get("schema")
                .and_then(Value::as_str)
                != Some("cidaren.class-task.detail.v1")
            || detail
                .task
                .normalized
                .get("release_id")
                .and_then(Value::as_str)
                != Some(release_id)
            || detail
                .normalized_detail
                .get("release_id")
                .and_then(Value::as_str)
                != Some(release_id)
        {
            return Err(remote_changed(
                "Cidaren class score route no longer matches its fresh release identity",
            ));
        }
        query.push(("release_id", release_id.to_owned()));
        query.extend(common_score_query(timestamp));
        return Ok(Some(CidarenTaskScoreRequest {
            path: CLASS_TASK_INFO_PATH,
            query,
        }));
    }

    let identity = remote_task_id
        .strip_prefix("study-task:")
        .and_then(|identity| identity.split_once(':'))
        .ok_or_else(|| protocol_drift("Cidaren score Task identity is invalid"))?;
    let course_id = required_component(identity.0, "Course ID")?;
    let list_id = required_component(identity.1, "list ID")?;
    if schema != Some("cidaren.study-task.v1")
        || detail
            .normalized_detail
            .get("schema")
            .and_then(Value::as_str)
            != Some("cidaren.study-task.detail.v1")
        || detail
            .task
            .normalized
            .get("course_id")
            .and_then(Value::as_str)
            != Some(course_id)
        || detail
            .task
            .normalized
            .get("list_id")
            .and_then(Value::as_str)
            != Some(list_id)
        || detail
            .normalized_detail
            .get("course_id")
            .and_then(Value::as_str)
            != Some(course_id)
        || detail
            .normalized_detail
            .get("list_id")
            .and_then(Value::as_str)
            != Some(list_id)
    {
        return Err(remote_changed(
            "Cidaren study score route no longer matches its fresh unit identity",
        ));
    }
    if task_id == -1 {
        return Ok(None);
    }
    query.push(("course_id", course_id.to_owned()));
    query.extend(common_score_query(timestamp));
    Ok(Some(CidarenTaskScoreRequest {
        path: STUDY_TASK_INFO_PATH,
        query,
    }))
}

fn common_score_query(timestamp: u64) -> [(&'static str, String); 3] {
    [
        ("timestamp", timestamp.to_string()),
        ("version", SCORE_REQUEST_VERSION.to_owned()),
        ("app_type", "1".to_owned()),
    ]
}

/// Parses plain, legacy or current encrypted success data, then treats donor
/// aliases as one semantic score fact. Conflicting aliases fail closed.
pub(crate) fn parse_task_score_response(
    document: &[u8],
    crypto: Option<&CidarenCryptoContext>,
) -> ProviderResult<Option<SubmissionScore>> {
    if document.is_empty() || document.len() > MAX_SCORE_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Cidaren task-score response is empty or exceeds the size limit",
        ));
    }
    let mut root: Value = serde_json::from_slice(document)
        .map_err(|_| invalid_response("Cidaren task-score response is not valid JSON"))?;
    let decoded = (|| {
        let Some(object) = root.as_object() else {
            return Err(score_envelope_observation(
                protocol_drift("Cidaren task-score response is not an object"),
                &root,
            ));
        };
        if object.get("code").and_then(Value::as_i64) != Some(1) {
            return Err(score_envelope_observation(
                invalid_response("Cidaren task-score endpoint returned a non-success code"),
                &root,
            ));
        }
        let Some(data) = object.get("data").filter(|value| !value.is_null()) else {
            return Err(score_envelope_observation(
                protocol_drift("Cidaren task-score response has no data"),
                &root,
            ));
        };
        let jv = match object.get("jv") {
            None | Some(Value::Null) => "0",
            Some(Value::String(value)) if !value.is_empty() && value.len() <= 64 => value,
            _ => {
                return Err(score_envelope_observation(
                    protocol_drift("Cidaren task-score response has an invalid jv"),
                    &root,
                ));
            }
        };
        decode_response_data(data, jv, crypto)
    })();
    zeroize_json(&mut root);
    let mut decoded = decoded?;
    let score = score_from_object(&decoded);
    zeroize_json(&mut decoded);
    score
}

pub(crate) fn normalized_task_score(
    detail: &RemoteTaskDetail,
) -> ProviderResult<Option<SubmissionScore>> {
    let value =
        detail.task.normalized.get("score").ok_or_else(|| {
            protocol_drift("Cidaren fresh Task omitted its normalized score fact")
        })?;
    if value.is_null() {
        Ok(None)
    } else {
        score_value(value).map(Some)
    }
}

fn score_from_object(value: &Value) -> ProviderResult<Option<SubmissionScore>> {
    let Some(object) = value.as_object() else {
        return Err(score_data_observation(
            protocol_drift("Cidaren task-score data is not an object"),
            value,
        ));
    };
    let mut accepted: Option<SubmissionScore> = None;
    for alias in SCORE_ALIASES {
        let Some(score_value_raw) = object.get(alias).filter(|value| !value.is_null()) else {
            continue;
        };
        let candidate =
            score_value(score_value_raw).map_err(|error| score_data_observation(error, value))?;
        if accepted
            .as_ref()
            .is_some_and(|accepted| accepted != &candidate)
        {
            return Err(score_data_observation(
                protocol_drift("Cidaren task-score aliases disagree on the observed score"),
                value,
            ));
        }
        accepted = Some(candidate);
    }
    Ok(accepted)
}

fn score_value(value: &Value) -> ProviderResult<SubmissionScore> {
    let encoded = match value {
        Value::Number(value) => value.to_string(),
        Value::String(value)
            if !value.is_empty()
                && value.len() <= 32
                && value.trim() == value
                && value.is_ascii() =>
        {
            value.clone()
        }
        _ => return Err(protocol_drift("Cidaren task score is invalid")),
    };
    let earned_milli_points = decimal_milli_points(&encoded)?;
    let score = SubmissionScore {
        earned_milli_points,
        possible_milli_points: 100_000,
    };
    score
        .validate()
        .map_err(|_| protocol_drift("Cidaren task score is out of range"))?;
    Ok(score)
}

fn decimal_milli_points(encoded: &str) -> ProviderResult<u64> {
    let (whole, fractional) = encoded.split_once('.').map_or((encoded, ""), |parts| parts);
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift("Cidaren task score is invalid"));
    }
    let whole = whole
        .parse::<u64>()
        .ok()
        .filter(|whole| *whole <= 100)
        .ok_or_else(|| protocol_drift("Cidaren task score is out of range"))?;
    if whole == 100 && fractional.bytes().any(|byte| byte != b'0') {
        return Err(protocol_drift("Cidaren task score is out of range"));
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
        .ok_or_else(|| protocol_drift("Cidaren task score is out of range"))
}

fn score_envelope_observation(error: ProviderError, root: &Value) -> ProviderError {
    let object = root.as_object();
    let code = object.and_then(|object| object.get("code"));
    let fallback = error.clone();
    error
        .try_with_protocol_observation(
            ProtocolSurface::SubmissionVerify,
            ProtocolObservationKind::UnknownResultShape,
            json!({
                "schema": "cidaren.task-score-observation.v1",
                "stage": "envelope",
                "root_kind": json_value_kind(Some(root)),
                "code_kind": json_value_kind(code),
                "code_value": code.and_then(Value::as_i64),
                "data_kind": json_value_kind(object.and_then(|object| object.get("data"))),
                "jv_kind": json_value_kind(object.and_then(|object| object.get("jv"))),
            }),
        )
        .unwrap_or(fallback)
}

fn score_data_observation(error: ProviderError, value: &Value) -> ProviderError {
    let object = value.as_object();
    let fallback = error.clone();
    error
        .try_with_protocol_observation(
            ProtocolSurface::SubmissionVerify,
            ProtocolObservationKind::UnknownResultShape,
            json!({
                "schema": "cidaren.task-score-observation.v1",
                "stage": "decoded_score",
                "root_kind": json_value_kind(Some(value)),
                "score_kind": json_value_kind(object.and_then(|object| object.get("score"))),
                "task_score_kind": json_value_kind(
                    object.and_then(|object| object.get("task_score"))
                ),
                "grade_kind": json_value_kind(object.and_then(|object| object.get("grade"))),
                "alias_count": object.map_or(0, |object| {
                    SCORE_ALIASES
                        .iter()
                        .filter(|alias| object.get(**alias).is_some_and(|value| !value.is_null()))
                        .count()
                }),
            }),
        )
        .unwrap_or(fallback)
}

const fn json_value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn required_task_id(value: Option<&Value>) -> ProviderResult<i64> {
    value
        .and_then(Value::as_i64)
        .filter(|value| *value == -1 || *value > 0)
        .ok_or_else(|| protocol_drift("Cidaren score Task contains an invalid task ID"))
}

fn required_component<'a>(value: &'a str, label: &'static str) -> ProviderResult<&'a str> {
    if !value.is_empty()
        && value.len() <= MAX_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Ok(value)
    } else {
        Err(protocol_drift(format!(
            "Cidaren score Task contains an invalid {label}"
        )))
    }
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(values) => values.values_mut().for_each(zeroize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;

    #[test]
    fn class_and_study_routes_freeze_donor_query_shapes() {
        let class = detail(
            "class-task:2002",
            "cidaren.class-task",
            812,
            &json!({
                "release_id": "2002",
                "course_id": "course-a",
            }),
        );
        let request = build_task_score_request("class-task:2002", &class, 1_730_000_000_000)
            .unwrap()
            .unwrap();
        assert_eq!(request.path, "ClassTask/Info");
        assert_eq!(
            request.query,
            vec![
                ("task_id", "812".to_owned()),
                ("release_id", "2002".to_owned()),
                ("timestamp", "1730000000000".to_owned()),
                ("version", "2.6.1.240122".to_owned()),
                ("app_type", "1".to_owned()),
            ]
        );

        let study = detail(
            "study-task:course-a:unit-4",
            "cidaren.study-task",
            924,
            &json!({
                "course_id": "course-a",
                "list_id": "unit-4",
            }),
        );
        let request =
            build_task_score_request("study-task:course-a:unit-4", &study, 1_730_000_000_001)
                .unwrap()
                .unwrap();
        assert_eq!(request.path, "StudyTask/Info");
        assert_eq!(request.query[0], ("task_id", "924".to_owned()));
        assert_eq!(request.query[1], ("course_id", "course-a".to_owned()));
        assert!(!request.query.iter().any(|(key, _)| *key == "list_id"));
    }

    #[test]
    fn ambiguous_study_minus_one_uses_only_fresh_list_observation() {
        let detail = detail(
            "study-task:course-a:unit-4",
            "cidaren.study-task",
            -1,
            &json!({"course_id": "course-a", "list_id": "unit-4"}),
        );
        assert!(
            build_task_score_request("study-task:course-a:unit-4", &detail, 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            normalized_task_score(&detail)
                .unwrap()
                .unwrap()
                .earned_milli_points,
            96_500
        );
    }

    #[test]
    fn score_fixture_accepts_aliases_and_exact_decimal_rounding() {
        let score = parse_task_score_response(
            include_bytes!("../../../fixtures/providers/cidaren/tasks/task-score-success.json"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(score.earned_milli_points, 96_501);
        assert_eq!(score.possible_milli_points, 100_000);
        assert_eq!(
            parse_task_score_response(br#"{"code":1,"data":{"score":0}}"#, None)
                .unwrap()
                .unwrap()
                .earned_milli_points,
            0
        );
        assert_eq!(
            parse_task_score_response(br#"{"code":1,"data":{"score":null}}"#, None).unwrap(),
            None
        );
    }

    #[test]
    fn conflicting_or_invalid_score_evidence_fails_closed() {
        assert!(
            parse_task_score_response(br#"{"code":1,"data":{"score":"80","grade":81}}"#, None)
                .is_err()
        );
        assert!(
            parse_task_score_response(br#"{"code":1,"data":{"score":100.0001}}"#, None).is_err()
        );
        assert!(parse_task_score_response(br#"{"code":1,"data":{"score":"1e2"}}"#, None).is_err());
    }

    #[test]
    fn score_drift_observations_expose_shape_without_result_values() {
        let envelope = br#"{
            "code":991,
            "data":{"answer":"must-not-cross-answer"},
            "jv":"must-not-cross-jv"
        }"#;
        let error = parse_task_score_response(envelope, None).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::SubmissionVerify);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            observation.shape_sanitized,
            json!({
                "schema": "cidaren.task-score-observation.v1",
                "stage": "envelope",
                "root_kind": "object",
                "code_kind": "number",
                "code_value": 991,
                "data_kind": "object",
                "jv_kind": "string",
            })
        );

        let decoded = br#"{
            "code":1,
            "data":{
                "score":"80",
                "grade":81,
                "answer":"must-not-cross-answer",
                "topic_code":"must-not-cross-topic-code"
            }
        }"#;
        let error = parse_task_score_response(decoded, None).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(
            observation.shape_sanitized,
            json!({
                "schema": "cidaren.task-score-observation.v1",
                "stage": "decoded_score",
                "root_kind": "object",
                "score_kind": "string",
                "task_score_kind": "missing",
                "grade_kind": "number",
                "alias_count": 2,
            })
        );
        let sanitized = serde_json::to_string(&observation.shape_sanitized).unwrap();
        assert!(!sanitized.contains("must-not-cross"));
        assert!(!sanitized.contains("answer"));
        assert!(!sanitized.contains("topic_code"));
        assert!(!sanitized.contains("80"));
        assert!(!sanitized.contains("81"));
    }

    #[test]
    fn stale_identity_and_normalized_detail_mismatch_fail_closed() {
        let mut detail = detail(
            "class-task:2002",
            "cidaren.class-task",
            812,
            &json!({
                "release_id": "2002",
                "course_id": "course-a",
            }),
        );
        detail.normalized_detail["release_id"] = json!("2003");
        assert_eq!(
            build_task_score_request("class-task:2002", &detail, 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    fn detail(
        remote_id: &str,
        schema_prefix: &str,
        task_id: i64,
        identity: &Value,
    ) -> RemoteTaskDetail {
        let mut normalized = identity.as_object().unwrap().clone();
        normalized.insert("schema".to_owned(), json!(format!("{schema_prefix}.v1")));
        normalized.insert("task_id".to_owned(), json!(task_id));
        normalized.insert("progress".to_owned(), json!(100));
        normalized.insert("score".to_owned(), json!(96.5));
        let normalized = Value::Object(normalized);
        let mut detail = identity.as_object().unwrap().clone();
        detail.insert(
            "schema".to_owned(),
            json!(format!("{schema_prefix}.detail.v1")),
        );
        detail.insert("task".to_owned(), normalized.clone());
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: remote_id.to_owned(),
                course_remote_id: Some("course:course-a".to_owned()),
                title: "Synthetic Task".to_owned(),
                source_type: SourceType::Exam,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Completed,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: [TaskCapability::SubmissionBuild].into_iter().collect(),
                fingerprint: "fixture".to_owned(),
                normalized,
                raw_sanitized: json!({}),
            },
            normalized_detail: Value::Object(detail),
        }
    }
}
