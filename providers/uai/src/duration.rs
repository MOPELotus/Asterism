use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_domain::Timestamp;
use asterism_provider_api::{
    DurationReadCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteDuration,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use zeroize::Zeroize;

use crate::{
    UaiBrowserDurationReadback, UaiBrowserResidenceCheckpoint, UaiBrowserResidencePlan,
    UaiCourseResidenceBatchPlan,
    course_inventory::{protocol_drift, required_remote_component},
    metadata::development_metadata,
};

const MAX_DURATION_DOCUMENT_BYTES: usize = 1_024 * 1_024;
const MAX_DURATION_NODES: usize = 8_192;
const MAX_DURATION_DEPTH: usize = 32;
const MAX_TASK_TOTAL_SCORE: u64 = 1_000_000;

/// One bounded per-Unit study-record response, redacted and zeroized on drop.
pub struct UaiDurationDocument(String);

impl UaiDurationDocument {
    /// Wraps one complete native duration response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized body.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_DURATION_DOCUMENT_BYTES {
            document.zeroize();
            return Err(invalid_duration_response(
                "UAI duration response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiDurationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiDurationDocument([REDACTED])")
    }
}

impl Drop for UaiDurationDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Native boundary for one fresh per-Unit study-record read.
#[async_trait]
pub trait UaiDurationTransport: Send + Sync {
    async fn fetch_duration(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiDurationDocument>;
}

/// Exact Task-level study-record facts from the independent MIT-donor route.
/// Duration remains the only field projected into the shared `DurationRead`
/// capability; the other facts keep their Provider-specific semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiTaskStudyRecord {
    unit_id: String,
    group_id: String,
    finish_progress_percent: Option<f64>,
    duration_seconds: u64,
    required: Option<bool>,
    score_task: Option<bool>,
    question_total_score: Option<u64>,
    observed_at: Timestamp,
}

impl UaiTaskStudyRecord {
    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub const fn finish_progress_percent(&self) -> Option<f64> {
        self.finish_progress_percent
    }

    pub const fn duration_seconds(&self) -> u64 {
        self.duration_seconds
    }

    pub const fn required(&self) -> Option<bool> {
        self.required
    }

    pub const fn score_task(&self) -> Option<bool> {
        self.score_task
    }

    pub const fn question_total_score(&self) -> Option<u64> {
        self.question_total_score
    }

    pub const fn observed_at(&self) -> Timestamp {
        self.observed_at
    }
}

/// Fresh read-only duration for one normalized UAI Group Task.
pub struct UaiTaskDuration {
    metadata: ProviderMetadata,
    transport: Arc<dyn UaiDurationTransport>,
}

impl UaiTaskDuration {
    /// Creates the capability around one injected duration transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn UaiDurationTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }

    /// Reads the complete Provider-specific Task study record from one fresh,
    /// identity-bound per-Unit response.
    ///
    /// # Errors
    ///
    /// Returns typed authentication, network, identity or protocol errors.
    pub async fn read_study_record(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<UaiTaskStudyRecord> {
        validate_context(context, &self.metadata)?;
        let identity = parse_group_identity(remote_task_id)?;
        let document = self
            .transport
            .fetch_duration(context, &identity.course_resource, &identity.unit)
            .await?;
        parse_task_study_record(document.as_str(), &identity.unit, &identity.group)
    }

    /// Reads and binds the independent fresh Task study record required after
    /// one terminal `BrowserBridge` residence observation.
    ///
    /// The returned owner preserves observed duration and richer donor facts;
    /// it does not infer that any particular duration delta is success.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign checkpoint/plan/Task or a read
    /// observed before the completed residence exchange.
    pub async fn read_browser_residence_readback(
        &self,
        context: &ProviderContext,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        checkpoint: &UaiBrowserResidenceCheckpoint,
    ) -> ProviderResult<UaiBrowserDurationReadback> {
        if checkpoint.remote_task_id() != plan.target_remote_task_id {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI duration readback Task is foreign to its BrowserBridge plan",
            ));
        }
        let record = self
            .read_study_record(context, checkpoint.remote_task_id())
            .await?;
        checkpoint.bind_fresh_duration_readback(batch, plan, record)
    }
}

impl fmt::Debug for UaiTaskDuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiTaskDuration")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiTaskDuration {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl DurationReadCapability for UaiTaskDuration {
    async fn read_duration(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteDuration> {
        let record = self.read_study_record(context, remote_task_id).await?;
        Ok(RemoteDuration {
            duration_seconds: record.duration_seconds(),
            updated_at: record.observed_at(),
        })
    }
}

/// Parses and identity-binds one per-Unit study-record response.
///
/// The donor-audited `duration` field on an exact study-record Task node is in
/// seconds. Progress-route duration facts are intentionally not accepted here.
///
/// # Errors
///
/// Returns an invalid-response or protocol-drift error for malformed,
/// unsupported, duplicate, unbound or missing duration state.
pub fn parse_task_duration(
    document: &str,
    expected_unit_id: &str,
    expected_group_id: &str,
) -> ProviderResult<RemoteDuration> {
    let record = parse_task_study_record(document, expected_unit_id, expected_group_id)?;
    Ok(RemoteDuration {
        duration_seconds: record.duration_seconds(),
        updated_at: record.observed_at(),
    })
}

/// Parses the complete exact Task study-record facts without collapsing them
/// into Duration or progress semantics.
///
/// # Errors
///
/// Returns an invalid-response or protocol-drift error for malformed,
/// unsupported, duplicate, unbound or out-of-range state.
pub fn parse_task_study_record(
    document: &str,
    expected_unit_id: &str,
    expected_group_id: &str,
) -> ProviderResult<UaiTaskStudyRecord> {
    if document.is_empty() || document.len() > MAX_DURATION_DOCUMENT_BYTES {
        return Err(invalid_duration_response(
            "UAI duration response is empty or exceeds the size limit",
        ));
    }
    let expected_unit_id = validated_component(expected_unit_id, "expected duration Unit ID")?;
    let expected_group_id = validated_component(expected_group_id, "expected duration Group ID")?;
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_duration_response("UAI duration response is not valid JSON"))?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("UAI duration response is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(1)
        || root.get("success").and_then(Value::as_bool) != Some(true)
    {
        return Err(protocol_drift("UAI duration read did not succeed"));
    }
    let list = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("list"))
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI duration response has no value list"))?;

    let mut unit = None;
    for node in list {
        let object = node
            .as_object()
            .ok_or_else(|| protocol_drift("UAI duration list contains a non-object Unit"))?;
        let role = node_role(object)?;
        let node_id = node_identity(object)?;
        if role == "unit"
            && node_id.as_deref() == Some(expected_unit_id.as_str())
            && unit.replace(object).is_some()
        {
            return Err(protocol_drift(
                "UAI duration response contains a duplicate matching Unit",
            ));
        }
    }
    let unit = unit.ok_or_else(|| protocol_drift("UAI duration response has no matching Unit"))?;

    let mut state = DurationTraversal::new(&expected_group_id);
    visit_duration_node(unit, 1, &mut state)?;
    match (state.matches, state.facts) {
        (1, Some(facts)) => Ok(UaiTaskStudyRecord {
            unit_id: expected_unit_id.clone(),
            group_id: expected_group_id.clone(),
            finish_progress_percent: facts.finish_progress_percent,
            duration_seconds: facts.duration_seconds,
            required: facts.required,
            score_task: facts.score_task,
            question_total_score: facts.question_total_score,
            observed_at: Utc::now(),
        }),
        (0, _) => Err(protocol_drift("UAI duration response has no matching Task")),
        _ => Err(protocol_drift(
            "UAI duration response contains a duplicate matching Task",
        )),
    }
}

struct DurationTraversal<'a> {
    expected_group_id: &'a str,
    node_count: usize,
    seen_node_ids: BTreeSet<String>,
    matches: usize,
    facts: Option<TaskStudyFacts>,
}

#[derive(Clone, Copy)]
struct TaskStudyFacts {
    finish_progress_percent: Option<f64>,
    duration_seconds: u64,
    required: Option<bool>,
    score_task: Option<bool>,
    question_total_score: Option<u64>,
}

impl<'a> DurationTraversal<'a> {
    fn new(expected_group_id: &'a str) -> Self {
        Self {
            expected_group_id,
            node_count: 0,
            seen_node_ids: BTreeSet::new(),
            matches: 0,
            facts: None,
        }
    }
}

fn visit_duration_node(
    object: &Map<String, Value>,
    depth: usize,
    state: &mut DurationTraversal<'_>,
) -> ProviderResult<()> {
    if depth > MAX_DURATION_DEPTH {
        return Err(invalid_duration_response(
            "UAI duration tree exceeds the depth limit",
        ));
    }
    state.node_count = state
        .node_count
        .checked_add(1)
        .ok_or_else(|| invalid_duration_response("UAI duration node count overflowed"))?;
    if state.node_count > MAX_DURATION_NODES {
        return Err(invalid_duration_response(
            "UAI duration tree exceeds the node limit",
        ));
    }

    let role = node_role(object)?;
    let node_id = node_identity(object)?;
    if let Some(node_id) = node_id {
        if !state.seen_node_ids.insert(node_id.clone()) {
            return Err(protocol_drift(
                "UAI duration tree contains a duplicate node identity",
            ));
        }
        if node_id == state.expected_group_id {
            state.matches += 1;
            if !matches!(role.as_str(), "link" | "group") {
                return Err(protocol_drift(
                    "UAI duration Task has an unsupported node role",
                ));
            }
            state.facts = Some(TaskStudyFacts {
                finish_progress_percent: optional_percent(
                    object.get("finishProgress"),
                    "Task finish progress",
                )?,
                duration_seconds: object.get("duration").and_then(Value::as_u64).ok_or_else(
                    || protocol_drift("UAI duration Task has no unsigned duration in seconds"),
                )?,
                required: optional_bool(object.get("required"), "Task required flag")?,
                score_task: optional_bool(object.get("scoreTaskFlag"), "Task score flag")?,
                question_total_score: optional_total_score(object.get("taskQuesTotalScore"))?,
            });
        }
    }

    if let Some(children) = object.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| protocol_drift("UAI duration children field is not an array"))?;
        for child in children {
            let child = child
                .as_object()
                .ok_or_else(|| protocol_drift("UAI duration tree contains a non-object node"))?;
            visit_duration_node(child, depth + 1, state)?;
        }
    }
    Ok(())
}

fn optional_percent(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<f64>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
            .map(Some)
            .ok_or_else(|| protocol_drift(format!("UAI duration {label} is invalid"))),
    }
}

fn optional_bool(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<bool>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(protocol_drift(format!("UAI duration {label} is invalid"))),
    }
}

fn optional_total_score(value: Option<&Value>) -> ProviderResult<Option<u64>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= MAX_TASK_TOTAL_SCORE)
            .map(Some)
            .ok_or_else(|| protocol_drift("UAI duration Task total score is invalid")),
    }
}

fn node_role(object: &Map<String, Value>) -> ProviderResult<String> {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .ok_or_else(|| protocol_drift("UAI duration node has no role"))?;
    if !matches!(
        role.as_str(),
        "unit" | "section" | "node" | "link" | "group"
    ) {
        return Err(protocol_drift(
            "UAI duration tree contains an unknown node role",
        ));
    }
    Ok(role)
}

fn node_identity(object: &Map<String, Value>) -> ProviderResult<Option<String>> {
    match object.get("nodeId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(value) => required_remote_component(Some(value), "duration node ID").map(Some),
    }
}

struct GroupIdentity {
    course_resource: String,
    unit: String,
    group: String,
}

fn parse_group_identity(value: &str) -> ProviderResult<GroupIdentity> {
    let mut components = value.split(':');
    if components.next() != Some("group") {
        return Err(protocol_drift(
            "UAI duration received an unsupported Task identity",
        ));
    }
    let course_resource_id = components
        .next()
        .ok_or_else(|| protocol_drift("UAI duration Task has no Course-resource ID"))?;
    let unit_id = components
        .next()
        .ok_or_else(|| protocol_drift("UAI duration Task has no Unit ID"))?;
    let group_id = components
        .next()
        .ok_or_else(|| protocol_drift("UAI duration Task has no Group ID"))?;
    if components.next().is_some() {
        return Err(protocol_drift(
            "UAI duration Task identity has extra components",
        ));
    }
    Ok(GroupIdentity {
        course_resource: validated_component(course_resource_id, "duration Course-resource ID")?,
        unit: validated_component(unit_id, "duration Unit ID")?,
        group: validated_component(group_id, "duration Group ID")?,
    })
}

fn validated_component(value: &str, label: &'static str) -> ProviderResult<String> {
    required_remote_component(Some(&Value::String(value.to_owned())), label)
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI duration received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI duration requires an authenticated session",
        ));
    }
    Ok(())
}

fn invalid_duration_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    const DURATION: &str = include_str!("../../../fixtures/providers/uai/duration/unit-mixed.json");

    #[derive(Debug)]
    struct FixtureTransport;

    #[async_trait]
    impl UaiDurationTransport for FixtureTransport {
        async fn fetch_duration(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
        ) -> ProviderResult<UaiDurationDocument> {
            assert_eq!(course_resource_id, "2001");
            assert_eq!(unit_id, "unit-1");
            UaiDurationDocument::try_new(DURATION)
        }
    }

    #[test]
    fn parser_reads_only_exact_bound_task_seconds() {
        let record = parse_task_study_record(DURATION, "unit-1", "group-1").unwrap();
        assert_eq!(record.unit_id(), "unit-1");
        assert_eq!(record.group_id(), "group-1");
        assert_eq!(record.finish_progress_percent(), Some(100.0));
        assert_eq!(record.duration_seconds(), 445);
        assert_eq!(record.required(), Some(true));
        assert_eq!(record.score_task(), Some(true));
        assert_eq!(record.question_total_score(), Some(100));
        assert_eq!(
            parse_task_duration(DURATION, "unit-1", "group-1")
                .unwrap()
                .duration_seconds,
            445
        );
        assert_eq!(
            parse_task_duration(DURATION, "unit-1", "group-2")
                .unwrap()
                .duration_seconds,
            0
        );
        assert!(parse_task_duration(DURATION, "other-unit", "group-1").is_err());
        assert!(parse_task_duration(DURATION, "unit-1", "missing").is_err());
        let unscored = parse_task_study_record(DURATION, "unit-1", "group-2").unwrap();
        assert_eq!(unscored.finish_progress_percent(), Some(0.0));
        assert_eq!(unscored.score_task(), Some(false));
        assert_eq!(unscored.question_total_score(), None);
    }

    #[test]
    fn parser_rejects_duplicate_unknown_missing_negative_and_overflow_state() {
        let duplicate = DURATION.replace("\"group-2\"", "\"group-1\"");
        assert!(parse_task_duration(&duplicate, "unit-1", "group-1").is_err());
        assert!(
            parse_task_duration(
                r#"{"code":1,"success":true,"value":{"list":[{"nodeId":"unit-1","role":"unit","children":[{"nodeId":"group-1","role":"future","duration":1}]}]}}"#,
                "unit-1",
                "group-1",
            )
            .is_err()
        );
        for duration in ["null", "-1", "18446744073709551616"] {
            let document = format!(
                r#"{{"code":1,"success":true,"value":{{"list":[{{"nodeId":"unit-1","role":"unit","children":[{{"nodeId":"group-1","role":"link","duration":{duration}}}]}}]}}}}"#
            );
            assert!(parse_task_duration(&document, "unit-1", "group-1").is_err());
        }
        for invalid in [
            DURATION.replacen("\"finishProgress\": 100", "\"finishProgress\": 100.1", 1),
            DURATION.replacen("\"required\": true", "\"required\": \"true\"", 4),
            DURATION.replacen("\"scoreTaskFlag\": true", "\"scoreTaskFlag\": \"true\"", 1),
            DURATION.replacen(
                "\"taskQuesTotalScore\": 100",
                "\"taskQuesTotalScore\": -1",
                1,
            ),
            DURATION.replacen(
                "\"taskQuesTotalScore\": 100",
                "\"taskQuesTotalScore\": 1000001",
                1,
            ),
        ] {
            assert!(parse_task_study_record(&invalid, "unit-1", "group-1").is_err());
        }
    }

    #[tokio::test]
    async fn capability_reads_exact_duration_without_execution_side_effects() {
        let capability = UaiTaskDuration::try_new(Arc::new(FixtureTransport)).unwrap();
        let duration = capability
            .read_duration(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap();
        assert_eq!(duration.duration_seconds, 445);
        let record = capability
            .read_study_record(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap();
        assert_eq!(record.finish_progress_percent(), Some(100.0));
        assert_eq!(record.score_task(), Some(true));
        assert!(
            capability
                .read_duration(&provider_context(), "link:2001:unit-1:group-1")
                .await
                .is_err()
        );
    }

    #[test]
    fn documents_are_bounded_and_redacted() {
        let document = UaiDurationDocument::try_new(DURATION).unwrap();
        assert!(!format!("{document:?}").contains("group-1"));
        assert!(UaiDurationDocument::try_new("").is_err());
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-duration-test".to_owned(),
        }
    }
}
