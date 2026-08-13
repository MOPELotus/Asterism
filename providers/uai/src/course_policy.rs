use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use asterism_domain::Timestamp;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use async_trait::async_trait;
use serde_json::Value;
use zeroize::Zeroize;

use crate::course_inventory::{required_remote_component, required_text};

const MAX_POLICY_BYTES: usize = 1_024 * 1_024;
const MAX_POLICY_UNITS: usize = 2_048;
const MAX_REQUIRED_TASKS_PER_UNIT: usize = 4_096;

/// One bounded required-Course policy response tied to its fresh route facts.
pub struct UaiCoursePolicyDocument {
    course_resource_id: String,
    strategy_id: u64,
    document: String,
}

impl UaiCoursePolicyDocument {
    /// Binds a policy body to the exact `CourseResource` and fresh strategy ID.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid identities or an empty/oversized body.
    pub fn try_new(
        course_resource_id: impl Into<String>,
        strategy_id: u64,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        let course_resource_id = required_remote_component(
            Some(&Value::String(course_resource_id.into())),
            "Course-policy Course-resource ID",
        )?;
        if strategy_id == 0 {
            return Err(protocol_drift(
                "UAI Course policy has an invalid strategy ID",
            ));
        }
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_POLICY_BYTES {
            document.zeroize();
            return Err(invalid_response(
                "UAI Course-policy document is empty or exceeds the size limit",
            ));
        }
        Ok(Self {
            course_resource_id,
            strategy_id,
            document,
        })
    }
}

impl fmt::Debug for UaiCoursePolicyDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiCoursePolicyDocument([REDACTED])")
    }
}

impl Drop for UaiCoursePolicyDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// One exact Unit's required-task and scoring policy.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiUnitCoursePolicy {
    unit_id: String,
    caption: String,
    name: String,
    sort: u32,
    pass_score_percent: f64,
    score_type: u32,
    required_task_ids: Vec<String>,
    opens_at: Option<Timestamp>,
    closes_at: Option<Timestamp>,
}

impl UaiUnitCoursePolicy {
    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn caption(&self) -> &str {
        &self.caption
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn sort(&self) -> u32 {
        self.sort
    }

    pub const fn pass_score_percent(&self) -> f64 {
        self.pass_score_percent
    }

    pub const fn score_type(&self) -> u32 {
        self.score_type
    }

    pub fn required_task_ids(&self) -> &[String] {
        &self.required_task_ids
    }

    pub const fn opens_at(&self) -> Option<Timestamp> {
        self.opens_at
    }

    pub const fn closes_at(&self) -> Option<Timestamp> {
        self.closes_at
    }
}

/// Course policy plus its unique ordered Unit rows.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiCoursePolicy {
    course_resource_id: String,
    strategy_id: u64,
    unit_unlock: bool,
    unit_inner_unlock: bool,
    scoring_mode: u32,
    opens_at: Option<Timestamp>,
    closes_at: Option<Timestamp>,
    units: Vec<UaiUnitCoursePolicy>,
}

impl UaiCoursePolicy {
    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub const fn strategy_id(&self) -> u64 {
        self.strategy_id
    }

    pub const fn unit_unlock(&self) -> bool {
        self.unit_unlock
    }

    pub const fn unit_inner_unlock(&self) -> bool {
        self.unit_inner_unlock
    }

    pub const fn scoring_mode(&self) -> u32 {
        self.scoring_mode
    }

    pub const fn opens_at(&self) -> Option<Timestamp> {
        self.opens_at
    }

    pub const fn closes_at(&self) -> Option<Timestamp> {
        self.closes_at
    }

    pub fn units(&self) -> &[UaiUnitCoursePolicy] {
        &self.units
    }
}

/// Provider-private native boundary for the donor required-Course policy.
#[async_trait]
pub trait UaiCoursePolicyTransport: Send + Sync {
    async fn fetch_course_policy(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
    ) -> ProviderResult<UaiCoursePolicyDocument>;
}

/// Parses one identity-bound donor Course strategy response.
///
/// # Errors
///
/// Returns a typed error for envelope/identity drift, duplicate Unit/Task
/// identities, unsupported node semantics or malformed score/time fields.
pub fn parse_course_policy(document: &UaiCoursePolicyDocument) -> ProviderResult<UaiCoursePolicy> {
    let root: Value = serde_json::from_str(&document.document)
        .map_err(|_| invalid_response("UAI Course policy is not valid JSON"))?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Course policy is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(1)
        || root.get("success").and_then(Value::as_bool) != Some(true)
    {
        return Err(protocol_drift("UAI Course policy did not succeed"));
    }
    let value = root
        .get("value")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI Course policy has no value object"))?;
    let strategy = value
        .get("courseStudyStrategy")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI Course policy has no Course strategy"))?;
    if required_positive_u64(strategy.get("id"), "Course strategy ID")? != document.strategy_id {
        return Err(protocol_drift(
            "UAI Course policy strategy does not match its fresh request",
        ));
    }
    let actual_resource = required_remote_component(
        strategy.get("courseResourceId"),
        "Course-policy Course-resource ID",
    )?;
    if actual_resource != document.course_resource_id {
        return Err(protocol_drift(
            "UAI Course policy belongs to another CourseResource",
        ));
    }
    let (opens_at, closes_at) = availability_window(
        strategy.get("studyStartTime"),
        strategy.get("studyEndTime"),
        "Course policy",
    )?;
    let units = parse_unit_policies(value, document.strategy_id)?;
    Ok(UaiCoursePolicy {
        course_resource_id: document.course_resource_id.clone(),
        strategy_id: document.strategy_id,
        unit_unlock: binary_mode(strategy.get("unitUnlock"), "unit unlock")?,
        unit_inner_unlock: binary_mode(strategy.get("unitInnerUnlock"), "inner-Unit unlock")?,
        scoring_mode: required_u32(strategy.get("scoringMode"), "scoring mode")?,
        opens_at,
        closes_at,
        units,
    })
}

fn parse_unit_policies(
    value: &serde_json::Map<String, Value>,
    expected_strategy_id: u64,
) -> ProviderResult<Vec<UaiUnitCoursePolicy>> {
    let rows = value
        .get("courseUnitStrategyList")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI Course policy has no Unit strategy list"))?;
    if rows.len() > MAX_POLICY_UNITS {
        return Err(invalid_response("UAI Course policy exceeds the Unit limit"));
    }
    let mut units = BTreeMap::new();
    let mut sorts = BTreeMap::new();
    for row in rows {
        let row = row
            .as_object()
            .ok_or_else(|| protocol_drift("UAI Course policy has a non-object Unit"))?;
        if required_positive_u64(row.get("strategyId"), "Unit strategy ID")? != expected_strategy_id
        {
            return Err(protocol_drift(
                "UAI Unit policy belongs to another strategy",
            ));
        }
        if row.get("requireNodeType").and_then(Value::as_str) != Some("task") {
            return Err(protocol_drift(
                "UAI Unit policy has an unsupported required-node type",
            ));
        }
        let unit_id = required_remote_component(row.get("unitId"), "Course-policy Unit ID")?;
        let sort = required_u32(row.get("sort"), "Unit sort")?;
        if sort == 0 || sorts.insert(sort, unit_id.clone()).is_some() {
            return Err(protocol_drift(
                "UAI Course policy has an invalid or duplicate Unit sort",
            ));
        }
        let required_tasks = row
            .get("requiredTask")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI Unit policy has no required-Task list"))?;
        if required_tasks.len() > MAX_REQUIRED_TASKS_PER_UNIT {
            return Err(invalid_response(
                "UAI Unit policy exceeds the required-Task limit",
            ));
        }
        let mut required_task_ids = Vec::with_capacity(required_tasks.len());
        let mut required_task_set = BTreeSet::new();
        for task_id in required_tasks {
            let task_id = required_remote_component(Some(task_id), "required Task ID")?;
            if !required_task_set.insert(task_id.clone()) {
                return Err(protocol_drift(
                    "UAI Unit policy contains a duplicate required Task",
                ));
            }
            required_task_ids.push(task_id);
        }
        let (unit_opens_at, unit_closes_at) = availability_window(
            row.get("studyStartTime"),
            row.get("studyEndTime"),
            "Unit policy",
        )?;
        let unit = UaiUnitCoursePolicy {
            unit_id: unit_id.clone(),
            caption: required_text(row.get("caption"), "Course-policy Unit caption")?,
            name: required_text(row.get("unitName"), "Course-policy Unit name")?,
            sort,
            pass_score_percent: numeric_string_percent(row.get("passScore"), "pass score")?,
            score_type: numeric_string_u32(row.get("scoreType"), "score type")?,
            required_task_ids,
            opens_at: unit_opens_at,
            closes_at: unit_closes_at,
        };
        if units.insert(unit_id, unit).is_some() {
            return Err(protocol_drift(
                "UAI Course policy contains a duplicate Unit identity",
            ));
        }
    }
    let mut units = units.into_values().collect::<Vec<_>>();
    units.sort_by_key(UaiUnitCoursePolicy::sort);
    Ok(units)
}

fn availability_window(
    start: Option<&Value>,
    end: Option<&Value>,
    label: &'static str,
) -> ProviderResult<(Option<Timestamp>, Option<Timestamp>)> {
    let start = required_nonnegative_i64(start, label, "start time")?;
    let end = required_nonnegative_i64(end, label, "end time")?;
    if start == 0 || end == 0 {
        return Ok((None, None));
    }
    if start >= end {
        return Err(protocol_drift(format!(
            "UAI {label} has an invalid availability window"
        )));
    }
    Ok((
        Some(
            chrono::DateTime::from_timestamp_millis(start)
                .ok_or_else(|| protocol_drift(format!("UAI {label} start time is invalid")))?,
        ),
        Some(
            chrono::DateTime::from_timestamp_millis(end)
                .ok_or_else(|| protocol_drift(format!("UAI {label} end time is invalid")))?,
        ),
    ))
}

fn required_nonnegative_i64(
    value: Option<&Value>,
    label: &'static str,
    field: &'static str,
) -> ProviderResult<i64> {
    value
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| protocol_drift(format!("UAI {label} {field} is invalid")))
}

fn required_positive_u64(value: Option<&Value>, label: &'static str) -> ProviderResult<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| protocol_drift(format!("UAI {label} is invalid")))
}

fn required_u32(value: Option<&Value>, label: &'static str) -> ProviderResult<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| protocol_drift(format!("UAI {label} is invalid")))
}

fn binary_mode(value: Option<&Value>, label: &'static str) -> ProviderResult<bool> {
    match required_u32(value, label)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(protocol_drift(format!("UAI {label} is not binary"))),
    }
}

fn numeric_string_percent(value: Option<&Value>, label: &'static str) -> ProviderResult<f64> {
    let value = value
        .and_then(Value::as_str)
        .filter(|value| value.trim() == *value && value.len() <= 16)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value));
    value.ok_or_else(|| protocol_drift(format!("UAI Unit {label} is invalid")))
}

fn numeric_string_u32(value: Option<&Value>, label: &'static str) -> ProviderResult<u32> {
    value
        .and_then(Value::as_str)
        .filter(|value| value.trim() == *value && value.len() <= 10)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| protocol_drift(format!("UAI Unit {label} is invalid")))
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

    const POLICY: &str =
        include_str!("../../../fixtures/providers/uai/progress/course-required-policy.json");

    #[test]
    fn parses_bound_course_and_required_unit_task_policy() {
        let document = UaiCoursePolicyDocument::try_new("2001", 3001, POLICY).unwrap();
        let policy = parse_course_policy(&document).unwrap();

        assert_eq!(policy.course_resource_id(), "2001");
        assert_eq!(policy.strategy_id(), 3001);
        assert!(policy.unit_unlock());
        assert!(!policy.unit_inner_unlock());
        assert_eq!(policy.scoring_mode(), 0);
        assert_eq!(
            policy.opens_at().unwrap().timestamp_millis(),
            1_782_864_000_000
        );
        assert_eq!(
            policy.closes_at().unwrap().timestamp_millis(),
            1_793_491_200_000
        );
        assert_eq!(policy.units().len(), 2);
        assert_eq!(policy.units()[0].unit_id(), "unit-1");
        assert_eq!(policy.units()[0].caption(), "Unit 1");
        assert_eq!(policy.units()[0].name(), "Language in mission");
        assert_eq!(policy.units()[0].sort(), 1);
        assert!((policy.units()[0].pass_score_percent() - 60.0).abs() < f64::EPSILON);
        assert_eq!(policy.units()[0].score_type(), 0);
        assert_eq!(
            policy.units()[0].required_task_ids(),
            ["group-1", "group-2"]
        );
        assert!(policy.units()[0].opens_at().is_some());
        assert!(policy.units()[1].opens_at().is_none());
        assert!(policy.units()[1].closes_at().is_none());
        assert!(!format!("{document:?}").contains("must-not-be-retained"));
    }

    #[test]
    fn rejects_foreign_duplicate_and_invalid_policy_shapes() {
        for invalid in [
            POLICY.replacen("\"id\": 3001", "\"id\": 3002", 1),
            POLICY.replacen(
                "\"courseResourceId\": 2001",
                "\"courseResourceId\": 2002",
                1,
            ),
            POLICY.replacen("\"strategyId\": 3001", "\"strategyId\": 3002", 1),
            POLICY.replacen("\"unitId\": \"unit-2\"", "\"unitId\": \"unit-1\"", 1),
            POLICY.replacen("\"sort\": 2", "\"sort\": 1", 1),
            POLICY.replacen("\"group-2\"", "\"group-1\"", 1),
            POLICY.replacen("\"passScore\": \"60\"", "\"passScore\": \"101\"", 1),
            POLICY.replacen(
                "\"requireNodeType\": \"task\"",
                "\"requireNodeType\": \"group\"",
                1,
            ),
            POLICY.replacen("\"unitUnlock\": 1", "\"unitUnlock\": 2", 1),
            POLICY.replacen(
                "\"studyEndTime\": 1790812800000",
                "\"studyEndTime\": 1785542399999",
                1,
            ),
        ] {
            let document = UaiCoursePolicyDocument::try_new("2001", 3001, invalid).unwrap();
            assert!(parse_course_policy(&document).is_err());
        }
    }

    #[test]
    fn policy_document_is_bounded_and_redacted() {
        assert!(UaiCoursePolicyDocument::try_new("2001", 0, POLICY).is_err());
        assert!(UaiCoursePolicyDocument::try_new("2001", 3001, "").is_err());
        assert!(
            UaiCoursePolicyDocument::try_new("2001", 3001, "x".repeat(MAX_POLICY_BYTES + 1),)
                .is_err()
        );
    }
}
