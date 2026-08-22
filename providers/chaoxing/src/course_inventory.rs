use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, ProviderRouteContext, RemoteCourse,
};
use async_trait::async_trait;
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};

use crate::{ChaoxingCourseScope, ChaoxingInventoryDocument, metadata::development_metadata};

const MAX_COURSE_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_COURSE_TITLE_BYTES: usize = 512;
const MAX_COURSE_TEACHER_BYTES: usize = 256;
const MAX_COURSE_DESCRIPTION_BYTES: usize = 1_024;
const MAX_ROLE_ID_BYTES: usize = 128;
const COURSE_ROUTE_ORIGIN: &str = "https://mooc2-ans.chaoxing.com";
const COURSE_ROUTE_PATH: &str = "/mooc2-ans/mycourse/stu";
const COURSE_MIDDLE_ROUTE_HOST: &str = "mooc1.chaoxing.com";
const COURSE_MIDDLE_ROUTE_PATH: &str = "/visit/stucoursemiddle";

/// Parses one sanitized Chaoxing `courselistdata` HTML response.
///
/// Closed/unopened rows are intentionally excluded. Account-scoped `cpi` is
/// retained only in the non-serialized route context.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for oversized,
/// malformed, duplicate, or unbounded course rows.
pub fn parse_course_inventory(html: &str) -> ProviderResult<Vec<RemoteCourse>> {
    if html.len() > MAX_COURSE_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing course inventory document exceeds the size limit",
        ));
    }
    let document = Html::parse_document(html);
    let course_selector = selector("div.course");
    let unavailable_selector = selector("a.not-open-tip, div.not-open-tip");
    let mut courses = BTreeMap::new();
    for row in document.select(&course_selector) {
        if row.select(&unavailable_selector).next().is_some() {
            continue;
        }
        let course = parse_course_row(row)?;
        let remote_id = course.remote_id.clone();
        if courses.insert(remote_id, course).is_some() {
            return Err(protocol_drift(
                "Chaoxing course inventory contains a duplicate course/class identity",
            ));
        }
    }
    Ok(courses.into_values().collect())
}

fn parse_course_row(row: ElementRef<'_>) -> ProviderResult<RemoteCourse> {
    let course_id = descendant_attr(row, "input.courseId", "value")?;
    let class_id = descendant_attr(row, "input.clazzId", "value")?;
    let href = descendant_attr(row, "a[href]", "href")?;
    let cpi = course_route_cpi(&href, &course_id, &class_id)?;
    let title = validated_optional_text(
        &descendant_attr(row, "span.course-name", "title")?,
        MAX_COURSE_TITLE_BYTES,
        "course title",
    )?;
    let teacher = optional_descendant_attr(row, "p.color3", "title")
        .filter(|value| !value.trim().is_empty())
        .map(|value| validated_optional_text(&value, MAX_COURSE_TEACHER_BYTES, "teacher"))
        .transpose()?;
    let description = optional_descendant_attr(row, "p.margint10", "title")
        .filter(|value| !value.trim().is_empty())
        .map(|value| validated_optional_text(&value, MAX_COURSE_DESCRIPTION_BYTES, "description"))
        .transpose()?;
    let role_id = row
        .value()
        .attr("roleid")
        .filter(|value| !value.trim().is_empty())
        .map(|value| validated_optional_text(value, MAX_ROLE_ID_BYTES, "role ID"))
        .transpose()?;

    let remote_id = format!("course:{course_id}:{class_id}");
    ChaoxingCourseScope::new(&remote_id, &course_id, &class_id)?;
    validate_route_id(&cpi, "cpi")?;
    let route_context = ProviderRouteContext::try_from_pairs([
        ("chaoxing.course_id".to_owned(), course_id),
        ("chaoxing.class_id".to_owned(), class_id),
        ("chaoxing.cpi".to_owned(), cpi),
    ])?;
    Ok(RemoteCourse {
        remote_id,
        title,
        term: None,
        teacher,
        remote_status: None,
        metadata_sanitized: serde_json::json!({
            "schema": "chaoxing.course.v1",
            "description": description,
            "role_id": role_id,
        }),
        route_context,
    })
}

/// Runtime adapter which returns the root and any folder-scoped course-list
/// responses for one authenticated account.
#[async_trait]
pub trait ChaoxingCourseInventoryTransport: Send + Sync {
    async fn fetch_course_inventories(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Vec<ChaoxingInventoryDocument>>;
}

/// Development-level Chaoxing `CourseInventory` capability.
pub struct ChaoxingCourseInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn ChaoxingCourseInventoryTransport>,
}

impl ChaoxingCourseInventory {
    /// Creates the capability around one authenticated runtime transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the compile-time Provider metadata is
    /// invalid.
    pub fn try_new(transport: Arc<dyn ChaoxingCourseInventoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for ChaoxingCourseInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingCourseInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl CourseInventoryCapability for ChaoxingCourseInventory {
    async fn list_courses(&self, context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing course inventory received a mismatched Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing course inventory requires an authenticated session",
            ));
        }
        let documents = self.transport.fetch_course_inventories(context).await?;
        if documents.is_empty() {
            return Err(invalid_response(
                "Chaoxing course transport returned no inventory documents",
            ));
        }
        let mut merged = BTreeMap::new();
        for document in &documents {
            for course in parse_course_inventory(document.as_str())? {
                match merged.get(&course.remote_id) {
                    Some(previous) if previous == &course => {}
                    Some(_) => {
                        return Err(protocol_drift(
                            "Chaoxing folders disagree on one course identity",
                        ));
                    }
                    None => {
                        merged.insert(course.remote_id.clone(), course);
                    }
                }
            }
        }
        Ok(merged.into_values().collect())
    }
}

fn descendant_attr(
    row: ElementRef<'_>,
    selector_text: &str,
    attribute: &str,
) -> ProviderResult<String> {
    optional_descendant_attr(row, selector_text, attribute)
        .ok_or_else(|| protocol_drift("Chaoxing course row is missing a required structural field"))
}

fn optional_descendant_attr(
    row: ElementRef<'_>,
    selector_text: &str,
    attribute: &str,
) -> Option<String> {
    let selector = selector(selector_text);
    row.select(&selector)
        .find_map(|node| node.value().attr(attribute).map(str::to_owned))
}

fn course_route_cpi(value: &str, course_id: &str, class_id: &str) -> ProviderResult<String> {
    let url = Url::parse(value)
        .or_else(|_| Url::parse(COURSE_ROUTE_ORIGIN).and_then(|base| base.join(value)))
        .map_err(|_| protocol_drift("Chaoxing course row contains an invalid route"))?;
    let trusted_entry = matches!(
        (url.host_str(), url.path()),
        (Some("mooc2-ans.chaoxing.com"), COURSE_ROUTE_PATH)
            | (Some(COURSE_MIDDLE_ROUTE_HOST), COURSE_MIDDLE_ROUTE_PATH)
    );
    if url.scheme() != "https"
        || !trusted_entry
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || unique_query_value(&url, "courseid").as_deref() != Some(course_id)
        || unique_query_value(&url, "clazzid").as_deref() != Some(class_id)
    {
        return Err(protocol_drift(
            "Chaoxing course row contains an untrusted or mismatched route",
        ));
    }
    unique_query_value(&url, "cpi")
        .ok_or_else(|| protocol_drift("Chaoxing course row is missing a required route parameter"))
}

fn unique_query_value(url: &Url, key: &str) -> Option<String> {
    let mut values = url
        .query_pairs()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.into_owned());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn validated_optional_text(
    value: &str,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<String> {
    let value = normalize_text(value);
    validate_text(&value, maximum, label)?;
    Ok(value)
}

fn validate_text(value: &str, maximum: usize, label: &'static str) -> ProviderResult<()> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("Chaoxing course inventory contains an invalid {label}"),
        ));
    }
    Ok(())
}

fn validate_route_id(value: &str, label: &'static str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("Chaoxing course inventory contains an invalid {label}"),
        ));
    }
    Ok(())
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static Chaoxing selector must be valid")
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    const COURSES: &str =
        include_str!("../../../fixtures/providers/chaoxing/courses/list-mixed.html");
    const EXPECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/courses/list-mixed.expected.json");

    #[derive(Debug)]
    struct FixtureTransport {
        documents: Vec<&'static str>,
    }

    #[async_trait]
    impl ChaoxingCourseInventoryTransport for FixtureTransport {
        async fn fetch_course_inventories(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<ChaoxingInventoryDocument>> {
            self.documents
                .iter()
                .map(|document| ChaoxingInventoryDocument::try_new(*document))
                .collect()
        }
    }

    #[test]
    fn parser_keeps_class_identity_and_hides_cpi_from_serialization() {
        let courses = parse_course_inventory(COURSES).unwrap();
        let actual = serde_json::to_value(&courses).unwrap();
        let expected: serde_json::Value = serde_json::from_str(EXPECTED).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].route_context.get("chaoxing.cpi"), Some("9300"));
        assert!(!serde_json::to_string(&courses).unwrap().contains("9300"));
    }

    #[test]
    fn malformed_or_duplicate_course_rows_fail_closed() {
        assert!(parse_course_inventory("<div class='course'></div>").is_err());
        let duplicate = COURSES
            .replace("class-201", "class-200")
            .replace("201", "200");
        assert!(parse_course_inventory(&duplicate).is_err());
    }

    #[test]
    fn course_routes_reject_foreign_or_mismatched_identity() {
        for invalid in [
            COURSES.replace("mooc2-ans/mycourse", "//foreign.example/mycourse"),
            COURSES.replace("courseid=100", "courseid=999"),
            COURSES.replace("cpi=9300", "cpi=9300&cpi=other"),
            COURSES.replace(
                "/mooc2-ans/mycourse/stu",
                "https://user@mooc2-ans.chaoxing.com/mooc2-ans/mycourse/stu",
            ),
        ] {
            assert!(parse_course_inventory(&invalid).is_err());
        }
    }

    #[test]
    fn current_middle_route_keeps_exact_course_class_and_cpi_binding() {
        let current = COURSES.to_owned();
        let parsed = parse_course_inventory(&current).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].remote_id, "course:100:200");
        assert_eq!(parsed[0].route_context.get("chaoxing.cpi"), Some("9300"));

        for invalid in [
            current.replace(COURSE_MIDDLE_ROUTE_HOST, "mooc2-ans.chaoxing.com"),
            current.replace(COURSE_MIDDLE_ROUTE_PATH, COURSE_ROUTE_PATH),
            current.replace("clazzid=200", "clazzid=foreign"),
            current.replace("cpi=9300", "cpi=9300&amp;cpi=other"),
        ] {
            assert!(parse_course_inventory(&invalid).is_err());
        }
    }

    #[tokio::test]
    async fn capability_merges_identical_folder_results_deterministically() {
        let transport = Arc::new(FixtureTransport {
            documents: vec![COURSES, COURSES],
        });
        let inventory = ChaoxingCourseInventory::try_new(transport).unwrap();
        let courses = inventory.list_courses(&context()).await.unwrap();
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].remote_id, "course:100:200");
        assert_eq!(courses[1].remote_id, "course:100:201");
        assert_eq!(inventory.metadata(), &development_metadata().unwrap());
    }

    #[tokio::test]
    async fn capability_rejects_empty_transport_output() {
        let transport = Arc::new(FixtureTransport {
            documents: Vec::new(),
        });
        let inventory = ChaoxingCourseInventory::try_new(transport).unwrap();
        let error = inventory.list_courses(&context()).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-course-test".to_owned(),
        }
    }
}
