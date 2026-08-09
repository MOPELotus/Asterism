use std::{collections::BTreeSet, fmt};

use asterism_provider_api::{ProviderResult, RemoteCourse};

use crate::course_inventory::{course_id_from_remote, protocol_drift};

const MAX_COURSE_PAGE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ROUTE_COMPONENT_BYTES: usize = 128;

/// Account-scoped Course routing facts parsed from one fresh Course page.
/// Diagnostics redact all values and the structure is never serialized.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnCourseContext {
    course: String,
    user: String,
    class: String,
}

impl WellearnCourseContext {
    /// Exposes the stable Course ID only to an authorized inventory transport.
    pub fn course_id(&self) -> &str {
        &self.course
    }

    /// Exposes the account-scoped user route value only to an authorized
    /// inventory transport.
    pub fn user_id(&self) -> &str {
        &self.user
    }

    /// Exposes the account-scoped class route value only to an authorized
    /// inventory transport.
    pub fn class_id(&self) -> &str {
        &self.class
    }
}

impl fmt::Debug for WellearnCourseContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnCourseContext")
            .field("values", &"[REDACTED]")
            .finish()
    }
}

/// Extracts fresh `uid` and `classid` route values from a sanitized Course page.
///
/// Multiple identical observations are accepted, while conflicting values fail
/// closed. The Course's scan-local and stable identities are revalidated first.
///
/// # Errors
///
/// Returns a protocol-drift error for an oversized page, missing/ambiguous route
/// fields, a foreign Course or unsafe route characters.
pub fn parse_course_context(
    course: &RemoteCourse,
    document: &str,
) -> ProviderResult<WellearnCourseContext> {
    if document.is_empty() || document.len() > MAX_COURSE_PAGE_BYTES {
        return Err(protocol_drift(
            "WELearn Course page is empty or exceeds the size limit",
        ));
    }
    let course_id = course_id_from_remote(course)?;
    let user_id = unique_json_like_scalar(document, "uid")?;
    let class_id = unique_json_like_scalar(document, "classid")?;
    validate_route_component(&user_id, "user ID")?;
    validate_route_component(&class_id, "class ID")?;
    Ok(WellearnCourseContext {
        course: course_id,
        user: user_id,
        class: class_id,
    })
}

fn unique_json_like_scalar(document: &str, key: &'static str) -> ProviderResult<String> {
    let marker = format!("\"{key}\"");
    let mut values = BTreeSet::new();
    let mut remaining = document;
    while let Some(position) = remaining.find(&marker) {
        let after_marker = &remaining[position + marker.len()..];
        remaining = after_marker;
        let after_marker = after_marker.trim_start();
        let Some(after_colon) = after_marker.strip_prefix(':') else {
            continue;
        };
        if let Some(value) = parse_json_like_scalar(after_colon.trim_start()) {
            values.insert(value);
        }
    }
    if values.len() != 1 {
        return Err(protocol_drift(format!(
            "WELearn Course page has a missing or ambiguous {key} field"
        )));
    }
    Ok(values.pop_first().expect("one checked route value"))
}

fn parse_json_like_scalar(value: &str) -> Option<String> {
    if let Some(quoted) = value.strip_prefix('"') {
        let end = quoted.find('"')?;
        let value = &quoted[..end];
        if value.contains('\\') {
            return None;
        }
        return Some(value.to_owned());
    }
    let end = value
        .find(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.')
        })
        .unwrap_or(value.len());
    (end > 0 && value[..end].bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value[..end].to_owned())
}

fn validate_route_component(value: &str, label: &'static str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_ROUTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(protocol_drift(format!(
            "WELearn Course page contains an invalid {label}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_course_inventory;

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const CONTEXT: &str =
        include_str!("../../../fixtures/providers/welearn/courses/course-context.html");

    #[test]
    fn parser_binds_fresh_route_values_without_exposing_them() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let context = parse_course_context(course, CONTEXT).unwrap();
        assert_eq!(context.course_id(), "1001");
        assert_eq!(context.user_id(), "7001");
        assert_eq!(context.class_id(), "class-8001");
        assert!(!format!("{context:?}").contains("7001"));
    }

    #[test]
    fn missing_conflicting_or_unsafe_values_fail_closed() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        assert!(parse_course_context(course, "<html></html>").is_err());
        assert!(
            parse_course_context(
                course,
                r#"<script>const a={"uid":1,"classid":"a"};const b={"uid":2};</script>"#,
            )
            .is_err()
        );
        assert!(
            parse_course_context(
                course,
                r#"<script>const a={"uid":"bad/id","classid":"a"};</script>"#,
            )
            .is_err()
        );
        assert!(
            parse_course_context(
                course,
                r#"<script>const a={"uid":true,"classid":"a"};</script>"#,
            )
            .is_err()
        );
    }
}
