use std::{fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTaskDetail, TaskDetailCapability,
    TaskInventoryCapability,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::metadata::development_metadata;

const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;

/// Fresh UAI Group detail rebuilt from the current `CourseResource` and Task
/// inventories instead of persisted route facts.
pub struct UaiTaskDetail {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    tasks: Arc<dyn TaskInventoryCapability>,
}

impl UaiTaskDetail {
    /// Builds the detail capability from the same complete inventory
    /// capabilities used by scans.
    ///
    /// # Errors
    ///
    /// Returns a sanitized internal error if compile-time metadata is invalid.
    pub fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        tasks: Arc<dyn TaskInventoryCapability>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            courses,
            tasks,
        })
    }
}

impl fmt::Debug for UaiTaskDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiTaskDetail")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("tasks", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiTaskDetail {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskDetailCapability for UaiTaskDetail {
    async fn task_detail(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteTaskDetail> {
        validate_context(context, &self.metadata)?;
        let identity = GroupIdentity::parse(remote_task_id)?;
        let courses = self.courses.list_courses(context).await?;
        let course = matching_course(&courses, &identity.course_resource)?;
        let tasks = self.tasks.list_tasks(context, Some(course)).await?;
        let mut matches = tasks
            .into_iter()
            .filter(|task| task.remote_id == remote_task_id);
        let task = matches.next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI Group Task is no longer present in fresh inventory",
            )
        })?;
        if matches.next().is_some() {
            return Err(protocol_drift(
                "UAI fresh inventory contains a duplicate Group identity",
            ));
        }
        let normalized_detail = normalized_detail(&task.normalized, &identity)?;
        Ok(RemoteTaskDetail {
            task,
            normalized_detail,
        })
    }
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
            return Err(protocol_drift("UAI Group Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("group") {
            return Err(protocol_drift("UAI Group Task identity is invalid"));
        }
        let course_resource = valid_component(components.next())?;
        let unit = valid_component(components.next())?;
        let group = valid_component(components.next())?;
        if components.next().is_some() {
            return Err(protocol_drift("UAI Group Task identity is invalid"));
        }
        Ok(Self {
            course_resource,
            unit,
            group,
        })
    }
}

fn matching_course<'a>(
    courses: &'a [RemoteCourse],
    course_resource: &str,
) -> ProviderResult<&'a RemoteCourse> {
    let expected = format!("course-resource:{course_resource}");
    let mut matches = courses.iter().filter(|course| course.remote_id == expected);
    let course = matches.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI Group Task CourseResource is no longer present in fresh inventory",
        )
    })?;
    if matches.next().is_some() {
        return Err(protocol_drift(
            "UAI fresh inventory contains a duplicate CourseResource identity",
        ));
    }
    Ok(course)
}

fn normalized_detail(task: &Value, identity: &GroupIdentity) -> ProviderResult<Value> {
    let task = task
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Group Task has invalid normalized detail"))?;
    if task.get("course_resource_id").and_then(Value::as_str)
        != Some(identity.course_resource.as_str())
        || task
            .get("unit")
            .and_then(Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(Value::as_str)
            != Some(identity.unit.as_str())
        || task.get("group_id").and_then(Value::as_str) != Some(identity.group.as_str())
    {
        return Err(protocol_drift(
            "UAI normalized Group detail does not match its remote identity",
        ));
    }
    let mut detail = Map::new();
    detail.insert("schema".to_owned(), json!("uai.group-task-detail.v1"));
    detail.insert("task".to_owned(), Value::Object(task.clone()));
    Ok(Value::Object(detail))
}

fn valid_component(value: Option<&str>) -> ProviderResult<String> {
    let value = value.ok_or_else(|| protocol_drift("UAI Group Task identity is invalid"))?;
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(protocol_drift("UAI Group Task identity is invalid"));
    }
    Ok(value.to_owned())
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI detail received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI detail requires an authenticated session",
        ));
    }
    Ok(())
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};
    use asterism_provider_api::RemoteTask;

    use super::*;
    use crate::{parse_course_context, parse_course_inventory, parse_task_inventory};

    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str = include_str!("../../../fixtures/providers/uai/tasks/tree-mixed.json");

    #[derive(Debug)]
    struct FixtureInventory {
        metadata: ProviderMetadata,
        task_calls: AtomicUsize,
    }

    impl FixtureInventory {
        fn new() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                task_calls: AtomicUsize::new(0),
            }
        }
    }

    impl ProviderIdentity for FixtureInventory {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for FixtureInventory {
        async fn list_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            parse_course_inventory(COURSES)
        }
    }

    #[async_trait]
    impl TaskInventoryCapability for FixtureInventory {
        async fn list_tasks(
            &self,
            _context: &ProviderContext,
            course: Option<&RemoteCourse>,
        ) -> ProviderResult<Vec<RemoteTask>> {
            self.task_calls.fetch_add(1, Ordering::Relaxed);
            let course = course.expect("detail must select one fresh CourseResource");
            let context = parse_course_context(course, DETAIL)?;
            parse_task_inventory(course, &context, TREE)
        }
    }

    #[tokio::test]
    async fn detail_rediscovers_and_binds_the_exact_group() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = UaiTaskDetail::try_new(inventory.clone(), inventory.clone()).unwrap();
        let detail = capability
            .task_detail(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap();

        assert_eq!(detail.task.remote_id, "group:2001:unit-1:group-1");
        assert_eq!(
            detail.normalized_detail["schema"],
            "uai.group-task-detail.v1"
        );
        assert_eq!(detail.normalized_detail["task"]["question_count"], 1);
        assert_eq!(inventory.task_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn detail_rejects_malformed_or_disappeared_identity_before_success() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = UaiTaskDetail::try_new(inventory.clone(), inventory.clone()).unwrap();
        assert_eq!(
            capability
                .task_detail(&provider_context(), "link:2001:unit-1:group-1")
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        assert_eq!(inventory.task_calls.load(Ordering::Relaxed), 0);

        assert_eq!(
            capability
                .task_detail(&provider_context(), "group:9999:unit-1:group-1")
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(inventory.task_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn normalized_detail_rejects_identity_drift() {
        let identity = GroupIdentity::parse("group:2001:unit-1:group-1").unwrap();
        assert!(
            normalized_detail(
                &json!({
                    "course_resource_id": "2001",
                    "unit": {"id": "other-unit"},
                    "group_id": "group-1"
                }),
                &identity,
            )
            .is_err()
        );
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-detail-test".to_owned(),
        }
    }
}
