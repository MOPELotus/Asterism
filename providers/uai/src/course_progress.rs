use std::{collections::BTreeMap, fmt};

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use serde_json::Value;
use zeroize::Zeroize;

use crate::course_inventory::{protocol_drift, required_remote_component};

const MAX_COURSE_PROGRESS_DOCUMENT_BYTES: usize = 1_024 * 1_024;
const MAX_COURSE_PROGRESS_UNITS: usize = 2_048;
const MAX_SIGNED_64_BIT_VALUE: u64 = 9_223_372_036_854_775_807;

/// One bounded Course-level progress response, redacted and zeroized on drop.
pub struct UaiCourseProgressDocument(String);

impl UaiCourseProgressDocument {
    /// Wraps one complete native Course-level progress response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized body.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_COURSE_PROGRESS_DOCUMENT_BYTES {
            document.zeroize();
            return Err(invalid_course_progress_response(
                "UAI Course progress response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiCourseProgressDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiCourseProgressDocument([REDACTED])")
    }
}

impl Drop for UaiCourseProgressDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Sanitized Course-level strategy facts for one exact Unit.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiCourseUnitProgressStrategy {
    required: bool,
    minimum_score_percent: f64,
    opens_at: Option<asterism_domain::Timestamp>,
    closes_at: Option<asterism_domain::Timestamp>,
    statistic_mode_out: bool,
}

impl UaiCourseUnitProgressStrategy {
    pub const fn required(&self) -> bool {
        self.required
    }

    pub const fn minimum_score_percent(&self) -> f64 {
        self.minimum_score_percent
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
}

/// Sanitized current Course progress facts used to bind inventory and submit
/// protocol versions independently of the Course tree.
#[derive(Clone, Debug, PartialEq)]
pub struct UaiCourseProgressSnapshot {
    publish_version: u64,
    units: BTreeMap<String, UaiCourseUnitProgressStrategy>,
}

impl UaiCourseProgressSnapshot {
    pub const fn publish_version(&self) -> u64 {
        self.publish_version
    }

    pub const fn units(&self) -> &BTreeMap<String, UaiCourseUnitProgressStrategy> {
        &self.units
    }
}

/// Parses the current Rust donor's independent Course progress response.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for malformed,
/// oversized, unsuccessful, unversioned or invalid Unit strategy state.
pub fn parse_course_progress(document: &str) -> ProviderResult<UaiCourseProgressSnapshot> {
    if document.is_empty() || document.len() > MAX_COURSE_PROGRESS_DOCUMENT_BYTES {
        return Err(invalid_course_progress_response(
            "UAI Course progress response is empty or exceeds the size limit",
        ));
    }
    let root: Value = serde_json::from_str(document).map_err(|_| {
        invalid_course_progress_response("UAI Course progress response is not valid JSON")
    })?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Course progress response is not an object"))?;
    if root
        .get("code")
        .is_some_and(|value| value.as_i64() != Some(0))
    {
        return Err(protocol_drift("UAI Course progress read did not succeed"));
    }
    let result = root
        .get("rt")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI Course progress response has no rt object"))?;
    let publish_version = required_publish_version(result.get("publish_version"))?;
    let source_units = result
        .get("units")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI Course progress response has no units object"))?;
    if source_units.len() > MAX_COURSE_PROGRESS_UNITS {
        return Err(invalid_course_progress_response(
            "UAI Course progress Unit count exceeds the limit",
        ));
    }

    let mut units = BTreeMap::new();
    for (unit_id, value) in source_units {
        let unit_id = required_remote_component(
            Some(&Value::String(unit_id.to_owned())),
            "Course-progress Unit ID",
        )?;
        let unit = value
            .as_object()
            .ok_or_else(|| protocol_drift("UAI Course progress Unit is not an object"))?;
        let strategy = parse_unit_strategy(unit.get("strategies"))?;
        if units.insert(unit_id, strategy).is_some() {
            return Err(protocol_drift(
                "UAI Course progress contains a duplicate Unit identity",
            ));
        }
    }

    Ok(UaiCourseProgressSnapshot {
        publish_version,
        units,
    })
}

fn required_publish_version(value: Option<&Value>) -> ProviderResult<u64> {
    let version = match value {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
    .filter(|value| (1..=MAX_SIGNED_64_BIT_VALUE).contains(value))
    .ok_or_else(|| protocol_drift("UAI Course progress has an invalid publish version"))?;
    Ok(version)
}

fn parse_unit_strategy(value: Option<&Value>) -> ProviderResult<UaiCourseUnitProgressStrategy> {
    let strategy = value
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI Course progress Unit has no strategies object"))?;
    let required = optional_boolean(strategy.get("required"), "required")?.unwrap_or(false);
    let minimum_score_percent = optional_score_percent(strategy.get("min_score_pct"))?;
    let start = optional_epoch_seconds(strategy.get("start_time"), "start time")?.unwrap_or(0);
    let end = optional_epoch_seconds(strategy.get("end_time"), "end time")?.unwrap_or(0);
    let (opens_at, closes_at) = if start == 0 || end == 0 {
        (None, None)
    } else {
        if start >= end {
            return Err(protocol_drift(
                "UAI Course progress Unit has an invalid availability window",
            ));
        }
        (
            Some(chrono::DateTime::from_timestamp(start, 0).ok_or_else(|| {
                protocol_drift("UAI Course progress Unit start time is out of range")
            })?),
            Some(chrono::DateTime::from_timestamp(end, 0).ok_or_else(|| {
                protocol_drift("UAI Course progress Unit end time is out of range")
            })?),
        )
    };
    let statistic_mode_out =
        optional_boolean(strategy.get("statistic_mode_out"), "statistic mode")?.unwrap_or(false);
    Ok(UaiCourseUnitProgressStrategy {
        required,
        minimum_score_percent,
        opens_at,
        closes_at,
        statistic_mode_out,
    })
}

fn optional_score_percent(value: Option<&Value>) -> ProviderResult<f64> {
    let parsed = match value {
        None | Some(Value::Null) => return Ok(0.0),
        Some(Value::Number(value)) => number_as_percent(value),
        Some(Value::String(value)) => value.trim().parse::<i32>().ok().map(f64::from),
        _ => None,
    };
    parsed.ok_or_else(|| {
        protocol_drift(format!(
            "UAI Course progress Unit minimum score percent is invalid (shape={})",
            sanitized_json_shape(value)
        ))
    })
}

fn number_as_percent(value: &serde_json::Number) -> Option<f64> {
    if let Some(value) = value.as_i64() {
        return i32::try_from(value).ok().map(f64::from);
    }
    let value = value.as_f64()?;
    if !value.is_finite() {
        return None;
    }
    if value < 0.0 && value.fract() == 0.0 && value >= f64::from(i32::MIN) {
        return Some(value);
    }
    if (0.0..=1.0).contains(&value) {
        return Some(value * 100.0);
    }
    (value > 1.0 && value <= 100.0).then_some(value)
}

fn sanitized_json_shape(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(value)) if value.is_i64() => "signed_integer",
        Some(Value::Number(value)) if value.is_u64() => "unsigned_integer",
        Some(Value::Number(value)) => fractional_number_shape(value),
        Some(Value::String(value)) if value.trim().is_empty() => "empty_string",
        Some(Value::String(_)) => "string_non_i32",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn fractional_number_shape(value: &serde_json::Number) -> &'static str {
    match value.as_f64() {
        Some(value) if value > 0.0 && value < 1.0 => "fraction_0_to_1",
        Some(value) if value > 1.0 && value <= 100.0 => "fraction_1_to_100",
        _ => "fraction_other",
    }
}

fn optional_boolean(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<bool>> {
    value
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                protocol_drift(format!("UAI Course progress Unit {label} is not boolean"))
            })
        })
        .transpose()
}

fn optional_epoch_seconds(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<Option<i64>> {
    value
        .map(|value| {
            value.as_i64().filter(|value| *value >= 0).ok_or_else(|| {
                protocol_drift(format!("UAI Course progress Unit {label} is invalid"))
            })
        })
        .transpose()
}

fn invalid_course_progress_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    const COURSE_PROGRESS: &str =
        include_str!("../../../fixtures/providers/uai/progress/course-mixed.json");
    const SCORE_SHAPES: &str =
        include_str!("../../../fixtures/providers/uai/progress/course-score-shapes.json");

    #[test]
    fn parser_binds_publish_version_units_and_strategies() {
        let snapshot = parse_course_progress(COURSE_PROGRESS).unwrap();
        assert_eq!(snapshot.publish_version(), 123_290);
        assert_eq!(snapshot.units().len(), 1);
        let strategy = &snapshot.units()["unit-1"];
        assert!(strategy.required());
        assert_eq!(strategy.minimum_score_percent(), 60.0);
        assert!(!strategy.statistic_mode_out());
        assert_eq!(strategy.opens_at().unwrap().timestamp(), 1_785_542_400);
        assert_eq!(strategy.closes_at().unwrap().timestamp(), 1_790_812_800);
    }

    #[test]
    fn parser_accepts_observed_string_or_numeric_version_and_optional_code() {
        let numeric = COURSE_PROGRESS.replace(r#""123290""#, "123290");
        assert_eq!(
            parse_course_progress(&numeric).unwrap().publish_version(),
            123_290
        );
        let without_code = COURSE_PROGRESS.replacen(r#""code": 0,"#, "", 1);
        assert_eq!(
            parse_course_progress(&without_code)
                .unwrap()
                .publish_version(),
            123_290
        );
    }

    #[test]
    fn parser_accepts_bounded_score_shapes_without_guessing_invalid_values() {
        let snapshot = parse_course_progress(SCORE_SHAPES).unwrap();
        let expected = [
            ("unit-missing", 0.0),
            ("unit-null", 0.0),
            ("unit-string-zero", 0.0),
            ("unit-string-max", 100.0),
            ("unit-string-sentinel", -1.0),
            ("unit-number-sentinel", -1.0),
            ("unit-number-integral-float", 60.0),
            ("unit-number-ratio", 60.0),
            ("unit-number-fractional-percent", 60.5),
            ("unit-number", 60.0),
        ];
        for (unit_id, minimum) in expected {
            assert_eq!(snapshot.units()[unit_id].minimum_score_percent(), minimum);
        }

        for invalid in [
            "2147483648",
            "-2147483649",
            "-1.5",
            "100.5",
            "true",
            "{}",
            r#""2147483648""#,
            r#""60.0""#,
            r#""score""#,
        ] {
            let document = SCORE_SHAPES.replace(
                r#""min_score_pct": 60"#,
                &format!(r#""min_score_pct": {invalid}"#),
            );
            assert!(
                parse_course_progress(&document).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn malformed_versions_units_and_strategies_fail_closed() {
        for invalid in ["0", "-1", "9223372036854775808", r#""invalid""#] {
            let document = COURSE_PROGRESS.replace(r#""123290""#, invalid);
            assert!(parse_course_progress(&document).is_err());
        }
        assert!(
            parse_course_progress(&COURSE_PROGRESS.replace(r#""code": 0"#, r#""code": 1"#))
                .is_err()
        );
        assert!(
            parse_course_progress(
                &COURSE_PROGRESS
                    .replace(r#""min_score_pct": 60"#, r#""min_score_pct": 2147483648"#,)
            )
            .is_err()
        );
        assert!(
            parse_course_progress(
                &COURSE_PROGRESS.replace(r#""required": true"#, r#""required": 1"#)
            )
            .is_err()
        );
        assert!(
            parse_course_progress(
                &COURSE_PROGRESS.replace(r#""end_time": 1790812800"#, r#""end_time": 1"#)
            )
            .is_err()
        );
        assert_eq!(
            parse_course_progress(r#"{"code":0,"rt":{"publish_version":"1","units":{}}}"#)
                .unwrap()
                .units()
                .len(),
            0
        );
    }

    #[test]
    fn documents_are_bounded_and_redacted() {
        let document = UaiCourseProgressDocument::try_new(COURSE_PROGRESS).unwrap();
        assert!(!format!("{document:?}").contains("unit-1"));
        assert!(UaiCourseProgressDocument::try_new("").is_err());
        assert!(
            UaiCourseProgressDocument::try_new("x".repeat(MAX_COURSE_PROGRESS_DOCUMENT_BYTES + 1))
                .is_err()
        );
    }
}
