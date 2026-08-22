use std::{fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTaskDetail, TaskDetailCapability,
    TaskInventoryCapability,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::{ChaoxingCourseRoute, metadata::development_metadata};

const MAX_REMOTE_TASK_ID_BYTES: usize = 640;

/// Fresh, authenticated Chaoxing detail lookup built on the same atomic Course
/// and Task inventory contracts used by scans.
pub struct ChaoxingTaskDetail {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    tasks: Arc<dyn TaskInventoryCapability>,
}

impl ChaoxingTaskDetail {
    /// Builds the detail capability from registered inventory capabilities.
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

impl fmt::Debug for ChaoxingTaskDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingTaskDetail")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("tasks", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingTaskDetail {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskDetailCapability for ChaoxingTaskDetail {
    async fn task_detail(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteTaskDetail> {
        if context.provider_id != self.metadata.id {
            return Err(internal(
                "Chaoxing detail received a mismatched Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing detail requires an authenticated session",
            ));
        }
        let identity = ChaoxingTaskIdentity::parse(remote_task_id)?;
        let courses = self.courses.list_courses(context).await?;
        let course = matching_course(&courses, &identity)?;
        let tasks = self.tasks.list_tasks(context, Some(course)).await?;
        let mut matches = tasks
            .into_iter()
            .filter(|task| task.remote_id == remote_task_id);
        let task = matches.next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing task is no longer present in fresh inventory",
            )
        })?;
        if matches.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing fresh inventory contains duplicate task identity",
            ));
        }
        let normalized_detail = normalized_detail(&task.normalized)?;
        Ok(RemoteTaskDetail {
            task,
            normalized_detail,
        })
    }
}

#[derive(Debug)]
struct ChaoxingTaskIdentity<'a> {
    course_id: &'a str,
    class_id: &'a str,
}

impl<'a> ChaoxingTaskIdentity<'a> {
    fn parse(remote_task_id: &'a str) -> ProviderResult<Self> {
        if remote_task_id.is_empty()
            || remote_task_id.len() > MAX_REMOTE_TASK_ID_BYTES
            || remote_task_id.chars().any(char::is_control)
        {
            return Err(protocol_drift("Chaoxing remote task identity is invalid"));
        }
        let mut fields = remote_task_id.splitn(4, ':');
        let module = fields.next().unwrap_or_default();
        let course_id = fields.next().unwrap_or_default();
        let class_id = fields.next().unwrap_or_default();
        let task_component = fields.next().unwrap_or_default();
        if !matches!(module, "chapter" | "resource" | "work" | "exam" | "sign")
            || !valid_component(course_id)
            || !valid_component(class_id)
            || task_component.is_empty()
        {
            return Err(protocol_drift("Chaoxing remote task identity is invalid"));
        }
        Ok(Self {
            course_id,
            class_id,
        })
    }
}

fn matching_course<'a>(
    courses: &'a [RemoteCourse],
    identity: &ChaoxingTaskIdentity<'_>,
) -> ProviderResult<&'a RemoteCourse> {
    let mut matches = courses.iter().filter(|course| {
        ChaoxingCourseRoute::from_remote_course(course).is_ok_and(|route| {
            route.course_id() == identity.course_id && route.class_id() == identity.class_id
        })
    });
    let course = matches.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing task course is no longer present in fresh inventory",
        )
    })?;
    if matches.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing fresh inventory contains duplicate course identity",
        ));
    }
    Ok(course)
}

fn normalized_detail(task: &Value) -> ProviderResult<Value> {
    let task = task
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing task has invalid normalized detail"))?;
    let mut detail = Map::new();
    detail.insert("schema".to_owned(), json!("chaoxing.task-detail.v1"));
    detail.insert("task".to_owned(), Value::Object(task.clone()));
    Ok(Value::Object(detail))
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::{ProviderRouteContext, RemoteTask};

    use super::*;

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
            Ok(vec![course("100", "200"), course("100", "201")])
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
            assert_eq!(course.unwrap().remote_id, "course:100:200");
            Ok(vec![task(), exam_task()])
        }
    }

    #[tokio::test]
    async fn detail_rediscovers_the_exact_course_and_task() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = ChaoxingTaskDetail::try_new(inventory.clone(), inventory.clone()).unwrap();

        let detail = capability
            .task_detail(&context(), "work:100:200:work-1")
            .await
            .unwrap();

        assert_eq!(detail.task, task());
        assert_eq!(
            detail.normalized_detail["schema"],
            "chaoxing.task-detail.v1"
        );
        assert_eq!(detail.normalized_detail["task"], task().normalized);
        assert_eq!(inventory.task_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn detail_rejects_malformed_or_disappeared_remote_identity() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = ChaoxingTaskDetail::try_new(inventory.clone(), inventory).unwrap();
        let malformed = capability
            .task_detail(&context(), "foreign:100:200:1")
            .await;
        assert_eq!(
            malformed.unwrap_err().kind,
            ProviderErrorKind::ProtocolDrift
        );

        let missing = capability.task_detail(&context(), "work:999:200:1").await;
        assert_eq!(missing.unwrap_err().kind, ProviderErrorKind::RemoteChanged);
    }

    #[tokio::test]
    async fn fresh_exam_detail_exposes_score_and_retake_facts() {
        let inventory = Arc::new(FixtureInventory::new());
        let capability = ChaoxingTaskDetail::try_new(inventory.clone(), inventory).unwrap();

        let detail = capability
            .task_detail(&context(), "exam:100:200:exam-1")
            .await
            .unwrap();

        assert_eq!(detail.task, exam_task());
        assert_eq!(detail.normalized_detail["task"]["score"], 82.5);
        assert_eq!(detail.normalized_detail["task"]["retake_available"], true);
        assert_eq!(detail.normalized_detail["task"]["detail_score"], 82.5);
        assert_eq!(
            detail.normalized_detail["task"]["detail_retake_available"],
            true
        );
        assert_eq!(detail.task.remote_state, RemoteState::Completed);
        assert_eq!(detail.task.capabilities, Vec::new());
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-detail-test".to_owned(),
        }
    }

    fn course(course_id: &str, class_id: &str) -> RemoteCourse {
        let route_context = ProviderRouteContext::try_from_pairs([
            ("chaoxing.course_id".to_owned(), course_id.to_owned()),
            ("chaoxing.class_id".to_owned(), class_id.to_owned()),
            ("chaoxing.cpi".to_owned(), "9300".to_owned()),
        ])
        .unwrap();
        RemoteCourse {
            remote_id: format!("course:{course_id}:{class_id}"),
            title: format!("Course {class_id}"),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: json!({"schema": "chaoxing.course.v1"}),
            route_context,
        }
    }

    fn task() -> RemoteTask {
        RemoteTask {
            remote_id: "work:100:200:work-1".to_owned(),
            course_remote_id: Some("course:100:200".to_owned()),
            title: "Work one".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Unknown,
            remote_state: RemoteState::Pending,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities: Vec::new(),
            fingerprint: "v1:work-detail".to_owned(),
            normalized: json!({
                "schema": "chaoxing.inventory.v1",
                "module": "work",
                "detail_remote_state": "pending",
            }),
            raw_sanitized: json!({"detail_remote_state": "pending"}),
        }
    }

    fn exam_task() -> RemoteTask {
        RemoteTask {
            remote_id: "exam:100:200:exam-1".to_owned(),
            course_remote_id: Some("course:100:200".to_owned()),
            title: "Exam one".to_owned(),
            source_type: SourceType::Exam,
            assessment_class: AssessmentClass::Unknown,
            remote_state: RemoteState::Completed,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities: Vec::new(),
            fingerprint: "v1:exam-detail".to_owned(),
            normalized: json!({
                "schema": "chaoxing.inventory.v1",
                "module": "exam",
                "remote_state": "completed",
                "score": 82.5,
                "retake_available": true,
                "detail_score": 82.5,
                "detail_retake_available": true,
            }),
            raw_sanitized: json!({
                "score": 82.5,
                "retake_available": true,
            }),
        }
    }
}
