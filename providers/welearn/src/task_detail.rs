use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, SourceType, TaskCapability};
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

/// Fresh `WELearn` SCO detail rebuilt from the current Course, Unit and SCO
/// inventories instead of stale persisted route facts.
pub struct WellearnTaskDetail {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    tasks: Arc<dyn TaskInventoryCapability>,
}

pub(crate) fn validate_fresh_execution_detail(
    detail: &RemoteTaskDetail,
    remote_task_id: &str,
    course_id: &str,
    sco_id: &str,
    required_capabilities: &[TaskCapability],
) -> ProviderResult<()> {
    if detail.task.remote_id != remote_task_id
        || detail.task.course_remote_id.as_deref() != Some(format!("course:{course_id}").as_str())
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn SCO identity changed before execution",
        ));
    }
    if detail.task.source_type != SourceType::Resource
        || required_capabilities
            .iter()
            .any(|capability| !detail.task.capabilities.contains(capability))
        || detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionExecute)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "WELearn fresh SCO does not advertise the requested execution contract",
        ));
    }
    if matches!(
        detail.task.remote_state,
        RemoteState::Expired | RemoteState::Removed
    ) {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "WELearn fresh SCO is no longer executable",
        ));
    }
    if detail
        .normalized_detail
        .get("schema")
        .and_then(Value::as_str)
        != Some("welearn.sco-task-detail.v1")
    {
        return Err(protocol_drift(
            "WELearn fresh SCO detail has an unknown schema",
        ));
    }
    let normalized = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("WELearn fresh SCO detail has no normalized Task"))?;
    if normalized.get("schema").and_then(Value::as_str) != Some("welearn.sco.v1")
        || normalized.get("course_id").and_then(Value::as_str) != Some(course_id)
        || normalized.get("sco_id").and_then(Value::as_str) != Some(sco_id)
        || normalized.get("visible").and_then(Value::as_bool).is_none()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn fresh SCO route or visibility observation changed before execution",
        ));
    }
    Ok(())
}

impl WellearnTaskDetail {
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

impl fmt::Debug for WellearnTaskDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnTaskDetail")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("tasks", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnTaskDetail {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskDetailCapability for WellearnTaskDetail {
    async fn task_detail(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteTaskDetail> {
        validate_context(context, &self.metadata)?;
        let identity = ScoIdentity::parse(remote_task_id)?;
        let courses = self.courses.list_courses(context).await?;
        let course = matching_course(&courses, &identity.course_id)?;
        let tasks = self.tasks.list_tasks(context, Some(course)).await?;
        let mut matches = tasks
            .into_iter()
            .filter(|task| task.remote_id == remote_task_id);
        let task = matches.next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "WELearn SCO is no longer present in fresh inventory",
            )
        })?;
        if matches.next().is_some() {
            return Err(protocol_drift(
                "WELearn fresh inventory contains a duplicate SCO identity",
            ));
        }
        let normalized_detail = normalized_detail(&task, &identity)?;
        Ok(RemoteTaskDetail {
            task,
            normalized_detail,
        })
    }
}

struct ScoIdentity {
    course_id: String,
    sco_id: String,
}

impl ScoIdentity {
    fn parse(value: &str) -> ProviderResult<Self> {
        if value.is_empty()
            || value.len() > MAX_REMOTE_TASK_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(protocol_drift("WELearn SCO Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("sco") {
            return Err(protocol_drift("WELearn SCO Task identity is invalid"));
        }
        let course_id = valid_component(components.next())?;
        let sco_id = valid_component(components.next())?;
        if components.next().is_some() {
            return Err(protocol_drift("WELearn SCO Task identity is invalid"));
        }
        Ok(Self { course_id, sco_id })
    }
}

fn matching_course<'a>(
    courses: &'a [RemoteCourse],
    course_id: &str,
) -> ProviderResult<&'a RemoteCourse> {
    let expected = format!("course:{course_id}");
    let mut matches = courses.iter().filter(|course| course.remote_id == expected);
    let course = matches.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn SCO Course is no longer present in fresh inventory",
        )
    })?;
    if matches.next().is_some() {
        return Err(protocol_drift(
            "WELearn fresh inventory contains a duplicate Course identity",
        ));
    }
    Ok(course)
}

fn normalized_detail(
    task: &asterism_provider_api::RemoteTask,
    identity: &ScoIdentity,
) -> ProviderResult<Value> {
    if task.course_remote_id.as_deref() != Some(format!("course:{}", identity.course_id).as_str()) {
        return Err(protocol_drift(
            "WELearn normalized SCO detail does not match its Course identity",
        ));
    }
    let normalized = task
        .normalized
        .as_object()
        .ok_or_else(|| protocol_drift("WELearn SCO has invalid normalized detail"))?;
    if normalized.get("schema").and_then(Value::as_str) != Some("welearn.sco.v1")
        || normalized.get("course_id").and_then(Value::as_str) != Some(identity.course_id.as_str())
        || normalized.get("sco_id").and_then(Value::as_str) != Some(identity.sco_id.as_str())
    {
        return Err(protocol_drift(
            "WELearn normalized SCO detail does not match its remote identity",
        ));
    }
    let mut detail = Map::new();
    detail.insert("schema".to_owned(), json!("welearn.sco-task-detail.v1"));
    detail.insert("task".to_owned(), Value::Object(normalized.clone()));
    Ok(Value::Object(detail))
}

fn valid_component(value: Option<&str>) -> ProviderResult<String> {
    let value = value.ok_or_else(|| protocol_drift("WELearn SCO Task identity is invalid"))?;
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(protocol_drift("WELearn SCO Task identity is invalid"));
    }
    Ok(value.to_owned())
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn detail received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn detail requires an authenticated session",
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
    use crate::{WellearnScoLeavesDocument, parse_course_inventory, parse_task_inventory};

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");

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
            let course = course.expect("detail selects one fresh Course");
            let documents = [
                WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap(),
                WellearnScoLeavesDocument::try_new(1, UNIT_ONE).unwrap(),
            ];
            parse_task_inventory(course, UNITS, &documents)
        }
    }

    #[tokio::test]
    async fn detail_rediscovers_exact_course_and_sco() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = WellearnTaskDetail::try_new(inventory.clone(), inventory.clone()).unwrap();
        let detail = capability
            .task_detail(&context(), "sco:1001:301")
            .await
            .unwrap();

        assert_eq!(detail.task.remote_id, "sco:1001:301");
        assert_eq!(
            detail.normalized_detail["schema"],
            "welearn.sco-task-detail.v1"
        );
        assert_eq!(detail.normalized_detail["task"], detail.task.normalized);
        assert!(detail.task.fingerprint.starts_with("v1:"));
        assert_eq!(inventory.task_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn detail_rejects_malformed_or_disappeared_identity() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = WellearnTaskDetail::try_new(inventory.clone(), inventory).unwrap();

        let malformed = capability
            .task_detail(&context(), "sco:1001:301:extra")
            .await;
        assert_eq!(
            malformed.unwrap_err().kind,
            ProviderErrorKind::ProtocolDrift
        );

        let missing = capability.task_detail(&context(), "sco:9999:301").await;
        assert_eq!(missing.unwrap_err().kind, ProviderErrorKind::RemoteChanged);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-detail-test".to_owned(),
        }
    }
}
