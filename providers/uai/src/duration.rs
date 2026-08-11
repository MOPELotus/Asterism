use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_provider_api::{
    DurationReadCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteDuration,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use zeroize::Zeroize;

use crate::{
    course_inventory::{protocol_drift, required_remote_component},
    metadata::development_metadata,
};

const MAX_DURATION_DOCUMENT_BYTES: usize = 1_024 * 1_024;
const MAX_DURATION_NODES: usize = 8_192;
const MAX_DURATION_DEPTH: usize = 32;

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
        validate_context(context, &self.metadata)?;
        let identity = parse_group_identity(remote_task_id)?;
        let document = self
            .transport
            .fetch_duration(context, &identity.course_resource, &identity.unit)
            .await?;
        parse_task_duration(document.as_str(), &identity.unit, &identity.group)
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
    match (state.matches, state.duration_seconds) {
        (1, Some(duration_seconds)) => Ok(RemoteDuration {
            duration_seconds,
            updated_at: Utc::now(),
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
    duration_seconds: Option<u64>,
}

impl<'a> DurationTraversal<'a> {
    fn new(expected_group_id: &'a str) -> Self {
        Self {
            expected_group_id,
            node_count: 0,
            seen_node_ids: BTreeSet::new(),
            matches: 0,
            duration_seconds: None,
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
            state.duration_seconds = Some(
                object
                    .get("duration")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        protocol_drift("UAI duration Task has no unsigned duration in seconds")
                    })?,
            );
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
    }

    #[tokio::test]
    async fn capability_reads_exact_duration_without_execution_side_effects() {
        let capability = UaiTaskDuration::try_new(Arc::new(FixtureTransport)).unwrap();
        let duration = capability
            .read_duration(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap();
        assert_eq!(duration.duration_seconds, 445);
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
