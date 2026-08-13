use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_domain::RemoteState;
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, RemoteProgress, TaskProgressCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use zeroize::Zeroize;

use crate::{
    course_inventory::{protocol_drift, required_remote_component},
    metadata::development_metadata,
};

const MAX_PROGRESS_DOCUMENT_BYTES: usize = 1_024 * 1_024;
const MAX_PROGRESS_ROUTE_IDENTITY_BYTES: usize = 512;
const MAX_SIGNED_64_BIT_VALUE: u64 = 9_223_372_036_854_775_807;
const MAX_PROGRESS_MICROS: usize = 4_096;
const MAX_MICRO_PATH_COMPONENTS: usize = 8;

/// One bounded per-Unit progress response, redacted and zeroized on drop.
pub struct UaiProgressDocument(String);

impl UaiProgressDocument {
    /// Wraps one complete native progress response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized body.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_PROGRESS_DOCUMENT_BYTES {
            document.zeroize();
            return Err(invalid_progress_response(
                "UAI progress response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiProgressDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiProgressDocument([REDACTED])")
    }
}

impl Drop for UaiProgressDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Sanitized raw Group-state facts. `duration_raw` intentionally has no unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiGroupProgressSnapshot {
    pass: u8,
    pass2: u8,
    perm: u8,
    duration_raw: Option<u64>,
    tab_type: Option<String>,
    required: bool,
    min_score_percent: u8,
    opens_at: Option<asterism_domain::Timestamp>,
    closes_at: Option<asterism_domain::Timestamp>,
    statistic_mode_out: bool,
    publish_version: Option<u64>,
}

impl UaiGroupProgressSnapshot {
    pub const fn pass(&self) -> u8 {
        self.pass
    }

    pub const fn pass2(&self) -> u8 {
        self.pass2
    }

    pub const fn perm(&self) -> u8 {
        self.perm
    }

    pub const fn duration_raw(&self) -> Option<u64> {
        self.duration_raw
    }

    pub fn tab_type(&self) -> Option<&str> {
        self.tab_type.as_deref()
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub const fn min_score_percent(&self) -> u8 {
        self.min_score_percent
    }

    pub const fn opens_at(&self) -> Option<asterism_domain::Timestamp> {
        self.opens_at
    }

    pub const fn closes_at(&self) -> Option<asterism_domain::Timestamp> {
        self.closes_at
    }

    pub const fn statistic_mode_out(&self) -> bool {
        self.statistic_mode_out
    }

    pub const fn publish_version(&self) -> Option<u64> {
        self.publish_version
    }

    pub fn mutation_available_at(&self, now: asterism_domain::Timestamp) -> bool {
        match (self.opens_at, self.closes_at) {
            (Some(opens_at), Some(closes_at)) => opens_at < now && now < closes_at,
            _ => true,
        }
    }

    pub const fn is_completed(&self) -> bool {
        self.pass == 1 && self.pass2 == 1 && self.perm == 1
    }

    fn into_remote_progress(self) -> RemoteProgress {
        let completed = self.is_completed();
        RemoteProgress {
            remote_state: if completed {
                RemoteState::Completed
            } else {
                RemoteState::Unknown
            },
            percent: completed.then_some(100),
            duration_seconds: None,
            updated_at: Utc::now(),
        }
    }
}

/// Sanitized Micro-level progress and strategy facts from one per-Unit read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiMicroProgressSnapshot {
    pass: u8,
    pass2: u8,
    perm: u8,
    required: bool,
    min_score_percent: u8,
    opens_at: Option<asterism_domain::Timestamp>,
    closes_at: Option<asterism_domain::Timestamp>,
    statistic_mode_out: bool,
}

impl UaiMicroProgressSnapshot {
    pub const fn pass(&self) -> u8 {
        self.pass
    }

    pub const fn pass2(&self) -> u8 {
        self.pass2
    }

    pub const fn perm(&self) -> u8 {
        self.perm
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub const fn min_score_percent(&self) -> u8 {
        self.min_score_percent
    }

    pub const fn opens_at(&self) -> Option<asterism_domain::Timestamp> {
        self.opens_at
    }

    pub const fn closes_at(&self) -> Option<asterism_domain::Timestamp> {
        self.closes_at
    }

    pub const fn statistic_mode_out(&self) -> bool {
        self.statistic_mode_out
    }

    pub const fn is_completed(&self) -> bool {
        self.pass == 1 && self.pass2 == 1 && self.perm == 1
    }
}

/// Native boundary for one fresh per-Unit progress read.
#[async_trait]
pub trait UaiProgressTransport: Send + Sync {
    async fn fetch_progress(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiProgressDocument>;
}

/// Fresh read-only progress for one normalized UAI Group Task.
pub struct UaiTaskProgress {
    metadata: ProviderMetadata,
    transport: Arc<dyn UaiProgressTransport>,
}

impl UaiTaskProgress {
    /// Creates the capability around one injected progress transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn UaiProgressTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for UaiTaskProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiTaskProgress")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiTaskProgress {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskProgressCapability for UaiTaskProgress {
    async fn read_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress> {
        validate_context(context, &self.metadata)?;
        let identity = parse_group_identity(remote_task_id)?;
        let document = self
            .transport
            .fetch_progress(context, &identity.course_resource, &identity.unit)
            .await?;
        parse_group_progress(document.as_str(), &identity.unit, &identity.group)
            .map(UaiGroupProgressSnapshot::into_remote_progress)
    }
}

/// Parses and identity-binds one per-Unit Group progress response.
///
/// # Errors
///
/// Returns an invalid-response or protocol-drift error for malformed,
/// unsupported, unbound or missing Group state.
pub fn parse_group_progress(
    document: &str,
    expected_unit_id: &str,
    expected_group_id: &str,
) -> ProviderResult<UaiGroupProgressSnapshot> {
    if document.is_empty() || document.len() > MAX_PROGRESS_DOCUMENT_BYTES {
        return Err(invalid_progress_response(
            "UAI progress response is empty or exceeds the size limit",
        ));
    }
    let expected_unit_id = required_remote_component(
        Some(&Value::String(expected_unit_id.to_owned())),
        "expected progress Unit ID",
    )?;
    let expected_group_id = required_remote_component(
        Some(&Value::String(expected_group_id.to_owned())),
        "expected progress Group ID",
    )?;
    let result = progress_result(document)?;
    let actual_unit = required_remote_component(result.get("unit_id"), "progress Unit ID")?;
    if actual_unit != expected_unit_id {
        return Err(protocol_drift(
            "UAI progress response does not match its Unit identity",
        ));
    }
    let leaves = result
        .get("leafs")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI progress response has no leafs object"))?;
    let leaf = leaves
        .get(&expected_group_id)
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI progress response has no matching Group"))?;
    let state = leaf
        .get("state")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI Group progress has no state object"))?;
    let strategy = progress_strategy(leaf.get("strategies"))?;
    Ok(UaiGroupProgressSnapshot {
        pass: state_flag(state.get("pass"), "pass")?,
        pass2: state_flag(state.get("pass2"), "pass2")?,
        perm: state_flag(state.get("perm"), "perm")?,
        duration_raw: leaf
            .get("duration")
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| protocol_drift("UAI Group duration is not unsigned"))
            })
            .transpose()?,
        tab_type: leaf
            .get("tab_type")
            .map(|value| {
                let value = value
                    .as_str()
                    .ok_or_else(|| protocol_drift("UAI progress tab type is not text"))?;
                required_remote_component(
                    Some(&Value::String(value.to_owned())),
                    "progress tab type",
                )
            })
            .transpose()?,
        required: strategy.required,
        min_score_percent: strategy.min_score_percent,
        opens_at: strategy.opens_at,
        closes_at: strategy.closes_at,
        statistic_mode_out: strategy.statistic_mode_out,
        publish_version: optional_progress_publish_version(result.get("publish_version"))?,
    })
}

/// Parses all bounded Micro-level state entries from one per-Unit response.
/// Composite Micro keys are retained as sanitized identity paths rather than
/// flattened into a guessed single ID.
///
/// # Errors
///
/// Returns protocol drift for malformed paths, flags, strategies or windows,
/// and an invalid-response error when the map exceeds its bound.
pub fn parse_micro_progress(
    document: &str,
) -> ProviderResult<BTreeMap<String, UaiMicroProgressSnapshot>> {
    let result = progress_result(document)?;
    let Some(micros) = result.get("micros") else {
        return Ok(BTreeMap::new());
    };
    let micros = micros
        .as_object()
        .ok_or_else(|| protocol_drift("UAI progress micros is not an object"))?;
    if micros.len() > MAX_PROGRESS_MICROS {
        return Err(invalid_progress_response(
            "UAI progress Micro count exceeds the limit",
        ));
    }
    let mut snapshots = BTreeMap::new();
    for (path, value) in micros {
        let path = micro_progress_path(path)?;
        let micro = value
            .as_object()
            .ok_or_else(|| protocol_drift("UAI Micro progress entry is not an object"))?;
        let state = micro
            .get("state")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("UAI Micro progress has no state object"))?;
        let strategy = progress_strategy(micro.get("strategies"))?;
        let snapshot = UaiMicroProgressSnapshot {
            pass: state_flag(state.get("pass"), "Micro pass")?,
            pass2: state_flag(state.get("pass2"), "Micro pass2")?,
            perm: state_flag(state.get("perm"), "Micro perm")?,
            required: strategy.required,
            min_score_percent: strategy.min_score_percent,
            opens_at: strategy.opens_at,
            closes_at: strategy.closes_at,
            statistic_mode_out: strategy.statistic_mode_out,
        };
        if snapshots.insert(path, snapshot).is_some() {
            return Err(protocol_drift(
                "UAI progress contains a duplicate Micro identity path",
            ));
        }
    }
    Ok(snapshots)
}

/// Validates the account/Course/Unit echo on one native per-Unit progress read.
///
/// # Errors
///
/// Returns protocol drift when the bounded response belongs to a different
/// browser account, Course instance or Unit route.
pub(crate) fn validate_progress_route_binding(
    document: &str,
    expected_course_instance_id: &str,
    expected_unit_id: &str,
    expected_open_id: &str,
) -> ProviderResult<()> {
    let result = progress_result(document)?;
    let actual_unit = required_remote_component(result.get("unit_id"), "progress Unit ID")?;
    let actual_course = required_progress_route_identity(result.get("tutorialId"), "Course")?;
    let actual_open_id = required_progress_route_identity(result.get("open_id"), "account")?;
    if actual_unit != expected_unit_id
        || actual_course != expected_course_instance_id
        || actual_open_id != expected_open_id
    {
        return Err(protocol_drift(
            "UAI progress response does not match its account/Course/Unit route",
        ));
    }
    Ok(())
}

fn progress_result(document: &str) -> ProviderResult<serde_json::Map<String, Value>> {
    if document.is_empty() || document.len() > MAX_PROGRESS_DOCUMENT_BYTES {
        return Err(invalid_progress_response(
            "UAI progress response is empty or exceeds the size limit",
        ));
    }
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_progress_response("UAI progress response is not valid JSON"))?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("UAI progress response is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(protocol_drift("UAI progress read did not succeed"));
    }
    root.get("rt")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| protocol_drift("UAI progress response has no rt object"))
}

fn optional_progress_publish_version(value: Option<&Value>) -> ProviderResult<Option<u64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let version = match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
    .filter(|value| (1..=MAX_SIGNED_64_BIT_VALUE).contains(value))
    .ok_or_else(|| protocol_drift("UAI Unit progress has an invalid publish version"))?;
    Ok(Some(version))
}

fn required_progress_route_identity(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_PROGRESS_ROUTE_IDENTITY_BYTES
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| protocol_drift(format!("UAI progress has no valid {label} identity")))
}

fn micro_progress_path(value: &str) -> ProviderResult<String> {
    if value.is_empty() || value.len() > MAX_PROGRESS_ROUTE_IDENTITY_BYTES {
        return Err(protocol_drift("UAI progress Micro path is invalid"));
    }
    let components = value.split('/').collect::<Vec<_>>();
    if components.is_empty() || components.len() > MAX_MICRO_PATH_COMPONENTS {
        return Err(protocol_drift("UAI progress Micro path is invalid"));
    }
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return Err(protocol_drift("UAI progress Micro path is invalid"));
    }
    components
        .iter()
        .map(|component| {
            required_remote_component(
                Some(&Value::String((*component).to_owned())),
                "progress Micro path component",
            )
        })
        .collect::<ProviderResult<Vec<_>>>()
        .map(|components| components.join("/"))
}

struct ProgressStrategy {
    required: bool,
    min_score_percent: u8,
    opens_at: Option<asterism_domain::Timestamp>,
    closes_at: Option<asterism_domain::Timestamp>,
    statistic_mode_out: bool,
}

fn progress_strategy(value: Option<&Value>) -> ProviderResult<ProgressStrategy> {
    let Some(strategy) = value else {
        return Ok(ProgressStrategy {
            required: false,
            min_score_percent: 0,
            opens_at: None,
            closes_at: None,
            statistic_mode_out: false,
        });
    };
    let strategy = strategy
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Group progress strategies is not an object"))?;
    let required = optional_boolean(strategy.get("required"), "required")?.unwrap_or(false);
    let min_score_percent = strategy
        .get("min_score_pct")
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value <= 100)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| protocol_drift("UAI Group minimum score percent is invalid"))
        })
        .transpose()?
        .unwrap_or(0);
    let start = optional_epoch_seconds(strategy.get("start_time"), "start time")?.unwrap_or(0);
    let end = optional_epoch_seconds(strategy.get("end_time"), "end time")?.unwrap_or(0);
    let (opens_at, closes_at) = if start == 0 || end == 0 {
        (None, None)
    } else {
        if start >= end {
            return Err(protocol_drift(
                "UAI Group progress has an invalid availability window",
            ));
        }
        (
            Some(
                chrono::DateTime::from_timestamp(start, 0)
                    .ok_or_else(|| protocol_drift("UAI Group start time is out of range"))?,
            ),
            Some(
                chrono::DateTime::from_timestamp(end, 0)
                    .ok_or_else(|| protocol_drift("UAI Group end time is out of range"))?,
            ),
        )
    };
    let statistic_mode_out =
        optional_boolean(strategy.get("statistic_mode_out"), "statistic mode")?.unwrap_or(false);
    Ok(ProgressStrategy {
        required,
        min_score_percent,
        opens_at,
        closes_at,
        statistic_mode_out,
    })
}

fn optional_boolean(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<bool>> {
    value
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| protocol_drift(format!("UAI Group {label} is not boolean")))
        })
        .transpose()
}

fn optional_epoch_seconds(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<Option<i64>> {
    value
        .map(|value| {
            value
                .as_i64()
                .filter(|value| *value >= 0)
                .ok_or_else(|| protocol_drift(format!("UAI Group {label} is invalid")))
        })
        .transpose()
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
            "UAI progress received an unsupported Task identity",
        ));
    }
    let course_resource_id = components
        .next()
        .ok_or_else(|| protocol_drift("UAI progress Task has no Course-resource ID"))?;
    let unit_id = components
        .next()
        .ok_or_else(|| protocol_drift("UAI progress Task has no Unit ID"))?;
    let group_id = components
        .next()
        .ok_or_else(|| protocol_drift("UAI progress Task has no Group ID"))?;
    if components.next().is_some() {
        return Err(protocol_drift(
            "UAI progress Task identity has extra components",
        ));
    }
    Ok(GroupIdentity {
        course_resource: required_remote_component(
            Some(&Value::String(course_resource_id.to_owned())),
            "progress Course-resource ID",
        )?,
        unit: required_remote_component(
            Some(&Value::String(unit_id.to_owned())),
            "progress Unit ID",
        )?,
        group: required_remote_component(
            Some(&Value::String(group_id.to_owned())),
            "progress Group ID",
        )?,
    })
}

fn state_flag(value: Option<&Value>, label: &'static str) -> ProviderResult<u8> {
    match value.and_then(Value::as_u64) {
        Some(value @ 0..=1) => Ok(u8::try_from(value).expect("bounded UAI state flag")),
        _ => Err(protocol_drift(format!(
            "UAI Group progress contains an invalid {label} flag"
        ))),
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI progress received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI progress requires an authenticated session",
        ));
    }
    Ok(())
}

fn invalid_progress_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    const PROGRESS: &str = include_str!("../../../fixtures/providers/uai/progress/unit-mixed.json");

    #[derive(Debug)]
    struct FixtureTransport;

    #[async_trait]
    impl UaiProgressTransport for FixtureTransport {
        async fn fetch_progress(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
        ) -> ProviderResult<UaiProgressDocument> {
            assert_eq!(course_resource_id, "2001");
            assert_eq!(unit_id, "unit-1");
            UaiProgressDocument::try_new(PROGRESS)
        }
    }

    #[test]
    fn parser_requires_all_three_flags_for_completion() {
        let completed = parse_group_progress(PROGRESS, "unit-1", "group-1").unwrap();
        assert!(completed.is_completed());
        assert_eq!(completed.duration_raw(), Some(17));
        assert_eq!(completed.tab_type(), Some("text"));
        assert!(completed.required());
        assert_eq!(completed.min_score_percent(), 60);
        assert!(!completed.statistic_mode_out());
        assert_eq!(completed.publish_version(), Some(123_290));
        assert_eq!(completed.opens_at().unwrap().timestamp(), 1_785_542_400);
        assert_eq!(completed.closes_at().unwrap().timestamp(), 1_790_812_800);
        assert!(
            completed
                .mutation_available_at(chrono::DateTime::from_timestamp(1_786_752_000, 0).unwrap())
        );
        assert!(
            !completed
                .mutation_available_at(chrono::DateTime::from_timestamp(1_784_678_400, 0).unwrap())
        );

        let incomplete = parse_group_progress(PROGRESS, "unit-1", "group-2").unwrap();
        assert!(!incomplete.is_completed());
        assert_eq!(incomplete.pass(), 0);
        assert_eq!(incomplete.pass2(), 0);
        assert_eq!(incomplete.perm(), 1);
        assert_eq!(incomplete.duration_raw(), Some(9));
        assert_eq!(incomplete.tab_type(), Some("video"));
        assert!(!incomplete.required());
        assert!(incomplete.statistic_mode_out());
        assert!(incomplete.opens_at().is_none());
        assert!(incomplete.closes_at().is_none());
    }

    #[test]
    fn parser_rejects_unbound_missing_or_invalid_state() {
        assert!(
            parse_group_progress(
                &PROGRESS.replacen("\"code\": 0,", "\"message\": \"success\",", 1),
                "unit-1",
                "group-1",
            )
            .is_err()
        );
        assert!(
            parse_group_progress(
                &PROGRESS.replacen("\"min_score_pct\": 60", "\"min_score_pct\": 101", 1),
                "unit-1",
                "group-1",
            )
            .is_err()
        );
        assert!(
            parse_group_progress(
                &PROGRESS.replacen("\"end_time\": 1790812800", "\"end_time\": 1785542399", 1,),
                "unit-1",
                "group-1",
            )
            .is_err()
        );
        assert!(
            parse_group_progress(
                &PROGRESS.replacen("\"tab_type\": \"text\"", "\"tab_type\": 1", 1),
                "unit-1",
                "group-1",
            )
            .is_err()
        );
        assert!(parse_group_progress(PROGRESS, "other-unit", "group-1").is_err());
        assert!(parse_group_progress(PROGRESS, "unit-1", "missing").is_err());
        assert!(
            parse_group_progress(
                &PROGRESS.replace(r#""publish_version": "123290""#, r#""publish_version": 0"#),
                "unit-1",
                "group-1",
            )
            .is_err()
        );
        assert!(
            parse_group_progress(
                r#"{"code":0,"rt":{"unit_id":"unit-1","leafs":{"group-1":{"state":{"pass":2,"pass2":1,"perm":1}}}}}"#,
                "unit-1",
                "group-1",
            )
            .is_err()
        );
    }

    #[test]
    fn native_route_binding_requires_exact_account_course_and_unit_echoes() {
        validate_progress_route_binding(
            PROGRESS,
            "course-v2:synthetic+rw+20260809",
            "unit-1",
            "synthetic-open-id",
        )
        .unwrap();
        assert!(
            validate_progress_route_binding(
                PROGRESS,
                "course-v2:foreign+rw+20260809",
                "unit-1",
                "synthetic-open-id",
            )
            .is_err()
        );
        assert!(
            validate_progress_route_binding(
                PROGRESS,
                "course-v2:synthetic+rw+20260809",
                "unit-1",
                "foreign-open-id",
            )
            .is_err()
        );
    }

    #[test]
    fn micro_progress_preserves_composite_identity_state_and_strategy() {
        let micros = parse_micro_progress(PROGRESS).unwrap();
        assert_eq!(micros.len(), 1);
        let micro = &micros["section-1/micro-1"];
        assert_eq!(micro.pass(), 0);
        assert_eq!(micro.pass2(), 1);
        assert_eq!(micro.perm(), 1);
        assert!(!micro.is_completed());
        assert!(micro.required());
        assert_eq!(micro.min_score_percent(), 60);
        assert!(!micro.statistic_mode_out());
        assert_eq!(micro.opens_at().unwrap().timestamp(), 1_785_542_400);
        assert_eq!(micro.closes_at().unwrap().timestamp(), 1_790_812_800);

        assert!(
            parse_micro_progress(&PROGRESS.replace(
                "section-1/micro-1",
                "section-1/../micro-1"
            ))
            .is_err()
        );
        assert!(
            parse_micro_progress(&PROGRESS.replace(r#""pass2": 1"#, r#""pass2": 2"#))
                .is_err()
        );
    }

    #[tokio::test]
    async fn capability_reads_bound_progress_without_duration_inference() {
        let capability = UaiTaskProgress::try_new(Arc::new(FixtureTransport)).unwrap();
        let completed = capability
            .read_progress(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap();
        assert_eq!(completed.remote_state, RemoteState::Completed);
        assert_eq!(completed.percent, Some(100));
        assert_eq!(completed.duration_seconds, None);

        let incomplete = capability
            .read_progress(&provider_context(), "group:2001:unit-1:group-2")
            .await
            .unwrap();
        assert_eq!(incomplete.remote_state, RemoteState::Unknown);
        assert_eq!(incomplete.percent, None);
        assert_eq!(incomplete.duration_seconds, None);
        assert!(
            capability
                .read_progress(&provider_context(), "group:2001:group-1")
                .await
                .is_err()
        );
    }

    #[test]
    fn documents_are_bounded_and_redacted() {
        let document = UaiProgressDocument::try_new(PROGRESS).unwrap();
        assert!(!format!("{document:?}").contains("group-1"));
        assert!(UaiProgressDocument::try_new("").is_err());
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-progress-test".to_owned(),
        }
    }
}
