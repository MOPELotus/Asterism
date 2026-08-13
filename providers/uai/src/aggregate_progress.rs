use std::{collections::BTreeMap, fmt};

use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use async_trait::async_trait;
use serde_json::{Map, Value};
use zeroize::Zeroize;

use crate::course_inventory::{required_remote_component, required_text};

const MAX_AGGREGATE_PROGRESS_BYTES: usize = 1_024 * 1_024;
const MAX_AGGREGATE_UNITS: usize = 2_048;

/// One bounded Course/Unit study-record response tied to its request identity.
pub struct UaiAggregateProgressDocument {
    course_resource_id: String,
    app_user_id: String,
    document: String,
}

impl UaiAggregateProgressDocument {
    /// Binds an authenticated aggregate-progress body to the exact route facts
    /// used to obtain it.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid identities or an empty/oversized body.
    pub fn try_new(
        course_resource_id: impl Into<String>,
        app_user_id: impl Into<String>,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        let course_resource_id = required_remote_component(
            Some(&Value::String(course_resource_id.into())),
            "aggregate-progress Course-resource ID",
        )?;
        let mut app_user_id = required_remote_component(
            Some(&Value::String(app_user_id.into())),
            "aggregate-progress app-user ID",
        )?;
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_AGGREGATE_PROGRESS_BYTES {
            app_user_id.zeroize();
            document.zeroize();
            return Err(invalid_response(
                "UAI aggregate-progress document is empty or exceeds the size limit",
            ));
        }
        Ok(Self {
            course_resource_id,
            app_user_id,
            document,
        })
    }
}

impl fmt::Debug for UaiAggregateProgressDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiAggregateProgressDocument([REDACTED])")
    }
}

impl Drop for UaiAggregateProgressDocument {
    fn drop(&mut self) {
        self.app_user_id.zeroize();
        self.document.zeroize();
    }
}

/// Course- or Unit-level progress metrics from the independent study-record
/// route. Duration is donor-documented seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UaiAggregateProgressMetric {
    finish_progress_percent: f64,
    duration_seconds: u64,
    score_percent: Option<f64>,
}

impl UaiAggregateProgressMetric {
    pub const fn finish_progress_percent(&self) -> f64 {
        self.finish_progress_percent
    }

    pub const fn duration_seconds(&self) -> u64 {
        self.duration_seconds
    }

    pub const fn score_percent(&self) -> Option<f64> {
        self.score_percent
    }
}

/// One Unit row in a Course aggregate-progress snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiUnitAggregateProgress {
    unit_id: String,
    caption: String,
    name: String,
    required: bool,
    metric: UaiAggregateProgressMetric,
}

impl UaiUnitAggregateProgress {
    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn caption(&self) -> &str {
        &self.caption
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub const fn metric(&self) -> UaiAggregateProgressMetric {
        self.metric
    }
}

/// Exact Course total plus its unique Unit summaries.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiCourseAggregateProgress {
    course_resource_id: String,
    total: UaiAggregateProgressMetric,
    units: Vec<UaiUnitAggregateProgress>,
}

impl UaiCourseAggregateProgress {
    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub const fn total(&self) -> UaiAggregateProgressMetric {
        self.total
    }

    pub fn units(&self) -> &[UaiUnitAggregateProgress] {
        &self.units
    }
}

/// Provider-private transport boundary for the donor Course/Unit aggregate
/// study-record read. Shared capability registration requires a Core contract.
#[async_trait]
pub trait UaiAggregateProgressTransport: Send + Sync {
    async fn fetch_aggregate_progress(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
    ) -> ProviderResult<UaiAggregateProgressDocument>;
}

/// Parses one identity-bound aggregate study-record response.
///
/// # Errors
///
/// Returns a typed error for a rejected envelope, identity mismatch, duplicate
/// Unit or invalid metric/role shape.
pub fn parse_course_aggregate_progress(
    document: &UaiAggregateProgressDocument,
) -> ProviderResult<UaiCourseAggregateProgress> {
    let root: Value = serde_json::from_str(&document.document)
        .map_err(|_| invalid_response("UAI aggregate progress is not valid JSON"))?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("UAI aggregate progress is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(1)
        || root.get("success").and_then(Value::as_bool) != Some(true)
    {
        return Err(protocol_drift("UAI aggregate progress did not succeed"));
    }
    let value = root
        .get("value")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI aggregate progress has no value object"))?;
    let actual_user = value
        .get("user")
        .and_then(Value::as_object)
        .and_then(|user| user.get("appUserId"));
    let actual_user = required_remote_component(actual_user, "aggregate-progress app-user ID")?;
    if actual_user != document.app_user_id {
        return Err(protocol_drift(
            "UAI aggregate progress belongs to another app-user identity",
        ));
    }
    let total = progress_metric(
        value
            .get("totalDetail")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("UAI aggregate progress has no total detail"))?,
        "Course total",
    )?;
    let rows = value
        .get("unitList")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI aggregate progress has no Unit list"))?;
    if rows.len() > MAX_AGGREGATE_UNITS {
        return Err(invalid_response(
            "UAI aggregate progress exceeds the Unit limit",
        ));
    }
    let mut units = BTreeMap::new();
    for row in rows {
        let row = row
            .as_object()
            .ok_or_else(|| protocol_drift("UAI aggregate progress has a non-object Unit"))?;
        if row.get("role").and_then(Value::as_str) != Some("unit") {
            return Err(protocol_drift(
                "UAI aggregate progress contains a non-Unit role",
            ));
        }
        let unit_id = required_remote_component(row.get("nodeId"), "aggregate-progress Unit ID")?;
        let unit = UaiUnitAggregateProgress {
            unit_id: unit_id.clone(),
            caption: required_text(row.get("caption"), "aggregate-progress Unit caption")?,
            name: required_text(row.get("name"), "aggregate-progress Unit name")?,
            required: row
                .get("required")
                .and_then(Value::as_bool)
                .ok_or_else(|| protocol_drift("UAI aggregate-progress required flag is invalid"))?,
            metric: progress_metric(row, "Unit")?,
        };
        if units.insert(unit_id, unit).is_some() {
            return Err(protocol_drift(
                "UAI aggregate progress contains a duplicate Unit identity",
            ));
        }
    }
    Ok(UaiCourseAggregateProgress {
        course_resource_id: document.course_resource_id.clone(),
        total,
        units: units.into_values().collect(),
    })
}

fn progress_metric(
    object: &Map<String, Value>,
    label: &'static str,
) -> ProviderResult<UaiAggregateProgressMetric> {
    Ok(UaiAggregateProgressMetric {
        finish_progress_percent: bounded_percent(
            object.get("finishProgress"),
            label,
            "finish progress",
        )?
        .ok_or_else(|| protocol_drift(format!("UAI {label} has no finish progress")))?,
        duration_seconds: object
            .get("duration")
            .and_then(Value::as_u64)
            .ok_or_else(|| protocol_drift(format!("UAI {label} duration is invalid")))?,
        score_percent: bounded_percent(object.get("score"), label, "score")?,
    })
}

fn bounded_percent(
    value: Option<&Value>,
    label: &'static str,
    field: &'static str,
) -> ProviderResult<Option<f64>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
            .map(Some)
            .ok_or_else(|| protocol_drift(format!("UAI {label} {field} is invalid"))),
    }
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMMARY: &str =
        include_str!("../../../fixtures/providers/uai/progress/course-unit-summary.json");

    #[test]
    fn parses_exact_course_and_unit_progress_without_retaining_user_noise() {
        let document = UaiAggregateProgressDocument::try_new("2001", "42", SUMMARY).unwrap();
        let snapshot = parse_course_aggregate_progress(&document).unwrap();

        assert_eq!(snapshot.course_resource_id(), "2001");
        assert!((snapshot.total().finish_progress_percent() - 62.5).abs() < f64::EPSILON);
        assert_eq!(snapshot.total().duration_seconds(), 140_000);
        assert_eq!(snapshot.total().score_percent(), Some(72.3));
        assert_eq!(snapshot.units().len(), 2);
        assert_eq!(snapshot.units()[0].unit_id(), "unit-1");
        assert_eq!(snapshot.units()[0].caption(), "Unit 1");
        assert_eq!(snapshot.units()[0].name(), "Language in mission");
        assert!(snapshot.units()[0].required());
        assert_eq!(snapshot.units()[0].metric().score_percent(), Some(85.5));
        assert_eq!(snapshot.units()[1].metric().score_percent(), None);
        assert!(!format!("{document:?}").contains("must-not-be-retained"));
    }

    #[test]
    fn rejects_identity_metric_role_and_duplicate_drift() {
        let wrong_user = UaiAggregateProgressDocument::try_new("2001", "other", SUMMARY).unwrap();
        assert!(parse_course_aggregate_progress(&wrong_user).is_err());

        for invalid in [
            SUMMARY.replacen("62.5", "100.1", 1),
            SUMMARY.replacen("140000", "-1", 1),
            SUMMARY.replacen("\"role\": \"unit\"", "\"role\": \"group\"", 1),
            SUMMARY.replacen("\"nodeId\": \"unit-2\"", "\"nodeId\": \"unit-1\"", 1),
            SUMMARY.replacen("\"required\": true", "\"required\": \"true\"", 1),
        ] {
            let document = UaiAggregateProgressDocument::try_new("2001", "42", invalid).unwrap();
            assert!(parse_course_aggregate_progress(&document).is_err());
        }
    }

    #[test]
    fn document_is_bounded_and_redacted() {
        assert!(UaiAggregateProgressDocument::try_new("bad/id", "42", SUMMARY).is_err());
        assert!(UaiAggregateProgressDocument::try_new("2001", "42", "").is_err());
        assert!(
            UaiAggregateProgressDocument::try_new(
                "2001",
                "42",
                "x".repeat(MAX_AGGREGATE_PROGRESS_BYTES + 1),
            )
            .is_err()
        );
    }
}
