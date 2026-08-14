use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use asterism_domain::{LogLevel, RemoteState, TaskCapability};
use asterism_provider_api::{
    CourseInventoryCapability, ExecutionEventSink, ExecutionOutcome, ExecutionRequest,
    ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionLog, ProviderIdentity,
    ProviderMetadata, ProviderProgress, ProviderResult, RemoteCourse, RemoteProgress,
    TaskExecutionCapability, TaskProgressCapability,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingCourseRoute,
    ChaoxingInventoryTransport,
    metadata::development_metadata,
    parse_chapter_inventory, parse_chapter_resource_inventory,
    resource_inventory::{
        ChaoxingImmediateResourceKind, ChaoxingImmediateResourceTarget,
        ChaoxingVideoResourceTarget, locate_immediate_resource_target,
        locate_video_resource_target,
    },
    runtime_settings::ChaoxingRuntimeSettings,
    task_inventory::CHAPTER_RESOURCE_CARD_COUNT,
};

#[async_trait]
pub(crate) trait ChaoxingImmediateResourceTransport: Send + Sync {
    async fn complete_immediate_resource(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingImmediateResourceTarget,
    ) -> ProviderResult<()>;
}

pub(crate) struct ChaoxingVideoStatus {
    report_token: asterism_secrets::SecretString,
    duration_seconds: u64,
    play_time_seconds: u64,
}

impl ChaoxingVideoStatus {
    pub(crate) fn try_new(
        report_token: impl Into<String>,
        duration_seconds: u64,
        play_time_seconds: u64,
    ) -> ProviderResult<Self> {
        let report_token = report_token.into();
        if report_token.is_empty()
            || report_token.len() > 4 * 1_024
            || report_token.chars().any(char::is_control)
            || duration_seconds == 0
            || duration_seconds > 24 * 60 * 60
            || play_time_seconds > duration_seconds
        {
            return Err(protocol_drift("Chaoxing Video status is invalid"));
        }
        Ok(Self {
            report_token: asterism_secrets::SecretString::new(report_token),
            duration_seconds,
            play_time_seconds,
        })
    }

    pub(crate) fn report_token(&self) -> &asterism_secrets::SecretString {
        &self.report_token
    }

    pub(crate) const fn duration_seconds(&self) -> u64 {
        self.duration_seconds
    }

    pub(crate) const fn play_time_seconds(&self) -> u64 {
        self.play_time_seconds
    }
}

impl fmt::Debug for ChaoxingVideoStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingVideoStatus")
            .field("report_token", &"[REDACTED]")
            .field("duration_seconds", &self.duration_seconds)
            .field("play_time_seconds", &self.play_time_seconds)
            .finish()
    }
}

#[async_trait]
pub(crate) trait ChaoxingVideoTransport: Send + Sync {
    async fn video_status(
        &self,
        context: &ProviderContext,
        target: &ChaoxingVideoResourceTarget,
    ) -> ProviderResult<ChaoxingVideoStatus>;

    async fn report_video_progress(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        target: &ChaoxingVideoResourceTarget,
        status: &ChaoxingVideoStatus,
        playing_time_seconds: u64,
    ) -> ProviderResult<bool>;
}

#[async_trait]
trait ChaoxingVideoSleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

#[derive(Debug)]
struct TokioVideoSleeper;

#[async_trait]
impl ChaoxingVideoSleeper for TokioVideoSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub struct ChaoxingResourceExecution {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    inventory: Arc<dyn ChaoxingInventoryTransport>,
    immediate: Arc<dyn ChaoxingImmediateResourceTransport>,
    video: Arc<dyn ChaoxingVideoTransport>,
    video_sleeper: Arc<dyn ChaoxingVideoSleeper>,
}

impl ChaoxingResourceExecution {
    /// Creates the native immediate-resource execution capability.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the compile-time Provider metadata is
    /// invalid.
    pub(crate) fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        inventory: Arc<dyn ChaoxingInventoryTransport>,
        immediate: Arc<dyn ChaoxingImmediateResourceTransport>,
        video: Arc<dyn ChaoxingVideoTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            courses,
            inventory,
            immediate,
            video,
            video_sleeper: Arc::new(TokioVideoSleeper),
        })
    }

    async fn resolve_course(
        &self,
        context: &ProviderContext,
        identity: &ResourceIdentity<'_>,
    ) -> ProviderResult<RemoteCourse> {
        let expected = format!("course:{}:{}", identity.course, identity.class);
        let mut matches = self
            .courses
            .list_courses(context)
            .await?
            .into_iter()
            .filter(|course| course.remote_id == expected);
        let course = matches.next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing resource course is no longer available",
            )
        })?;
        if matches.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing course discovery returned a duplicate execution scope",
            ));
        }
        Ok(course)
    }

    async fn resolve_resource_request(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        identity: &ResourceIdentity<'_>,
    ) -> ProviderResult<ChaoxingChapterResourceRequest> {
        let document = self
            .inventory
            .fetch_chapter_inventory(context, route)
            .await?;
        let scope = route.parser_scope()?;
        let chapters = parse_chapter_inventory(document.as_str(), &scope)?;
        let mut matching = chapters.iter().filter(|chapter| {
            chapter
                .normalized
                .get("knowledge_id")
                .and_then(serde_json::Value::as_str)
                == Some(identity.knowledge)
        });
        let chapter = matching.next().ok_or_else(remote_resource_changed)?;
        if matching.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter inventory returned a duplicate execution scope",
            ));
        }
        let request = ChaoxingChapterResourceRequest::try_from_available_chapter(chapter)?
            .ok_or_else(remote_resource_changed)?;
        if !request.belongs_to(route) || request.knowledge_id() != identity.knowledge {
            return Err(protocol_drift(
                "Chaoxing chapter execution request lost its route binding",
            ));
        }
        Ok(request)
    }

    async fn fetch_target(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        remote_task_id: &str,
    ) -> ProviderResult<ChaoxingImmediateResourceTarget> {
        let documents = self
            .fetch_resource_documents(context, route, request)
            .await?;
        locate_target(documents, route, request, remote_task_id)
    }

    async fn fetch_video_target(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        remote_task_id: &str,
    ) -> ProviderResult<ChaoxingVideoResourceTarget> {
        let documents = self
            .fetch_resource_documents(context, route, request)
            .await?;
        locate_video_target(documents, route, request, remote_task_id)
    }

    async fn fetch_resource_documents(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
    ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
        self.inventory
            .fetch_chapter_resource_inventories(context, route, std::slice::from_ref(request))
            .await
    }

    async fn resolve_resource_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress> {
        validate_context(context, &self.metadata)?;
        let identity = ResourceIdentity::parse(remote_task_id)?;
        let course = self.resolve_course(context, &identity).await?;
        let route = ChaoxingCourseRoute::from_remote_course(&course)?;
        let resource_request = self
            .resolve_resource_request(context, route, &identity)
            .await?;
        let documents = self
            .fetch_resource_documents(context, route, &resource_request)
            .await?;
        let (remote_state, kind) =
            locate_resource_fact(&documents, route, &resource_request, remote_task_id)?;
        if !matches!(
            kind,
            ExecutableResourceKind::Immediate
                | ExecutableResourceKind::Video
                | ExecutableResourceKind::Live
        ) {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing resource kind does not expose progress",
            ));
        }
        let completed = remote_state == RemoteState::Completed;
        Ok(RemoteProgress {
            remote_state,
            percent: Some(if completed { 100 } else { 0 }),
            duration_seconds: None,
            updated_at: Utc::now(),
        })
    }

    async fn execute_immediate(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        resource_request: &ChaoxingChapterResourceRequest,
        remote_task_id: &str,
        documents: Vec<ChaoxingChapterResourceDocument>,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        let target = locate_target(documents, route, resource_request, remote_task_id)?;
        if target.remote_state() == RemoteState::Completed {
            events
                .log(execution_log(
                    "resource_finalize",
                    "资源已在远端完成，跳过重复提交",
                    None,
                ))
                .await?;
            return Ok(completed_outcome(target.kind(), true));
        }
        events
            .report(ProviderProgress {
                percent: Some(0),
                stage: "resource_execute".to_owned(),
                status_text: Some("正在提交资源完成请求".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        events
            .log(execution_log(
                "resource_execute",
                "开始提交资源完成请求",
                None,
            ))
            .await?;
        self.immediate
            .complete_immediate_resource(context, route, resource_request.knowledge_id(), &target)
            .await?;
        events
            .report(ProviderProgress {
                percent: Some(90),
                stage: "resource_verify".to_owned(),
                status_text: Some("正在复核远端完成状态".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        events
            .log(execution_log(
                "resource_verify",
                "资源完成请求已返回，开始复核远端状态",
                None,
            ))
            .await?;
        let verified = self
            .fetch_target(context, route, resource_request, remote_task_id)
            .await?;
        if verified.kind() != target.kind() || verified.remote_state() != RemoteState::Completed {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing resource did not become completed after execution",
            ));
        }
        events
            .report(ProviderProgress {
                percent: Some(100),
                stage: "resource_verified".to_owned(),
                status_text: Some("远端完成状态已复核".to_owned()),
                completed_items: Some(1),
                total_items: Some(1),
            })
            .await?;
        events
            .log(execution_log(
                "resource_verified",
                "远端资源完成状态已复核",
                Some(serde_json::json!({
                    "resource_kind": resource_kind_name(target.kind()),
                    "verified": true,
                })),
            ))
            .await?;
        Ok(completed_outcome(target.kind(), false))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the video call keeps its Execution, route, fresh target, settings and event boundaries explicit"
    )]
    async fn execute_video(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        resource_request: &ChaoxingChapterResourceRequest,
        remote_task_id: &str,
        target: &ChaoxingVideoResourceTarget,
        settings: ChaoxingRuntimeSettings,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if target.remote_state() == RemoteState::Completed {
            events
                .log(execution_log(
                    "video_finalize",
                    "视频已在远端完成，跳过重复上报",
                    None,
                ))
                .await?;
            return Ok(video_outcome(true));
        }
        let status = self.video.video_status(context, target).await?;
        let duration = status.duration_seconds();
        let mut playing_time = status
            .play_time_seconds()
            .max(target.initial_play_time_millis() / 1_000)
            .min(duration);
        events
            .log(execution_log(
                "video_execute",
                "开始按冻结运行参数上报视频进度",
                Some(serde_json::json!({
                    "playback_rate_millis": settings.video_playback_rate_millis,
                    "progress_interval_seconds": settings.video_progress_interval_seconds,
                })),
            ))
            .await?;
        loop {
            if playing_time < duration {
                let next = playing_time
                    .saturating_add(settings.video_progress_interval_seconds)
                    .min(duration);
                self.video_sleeper
                    .sleep(real_playback_duration(
                        next - playing_time,
                        settings.video_playback_rate_millis,
                    )?)
                    .await;
                playing_time = next;
            }
            let passed = self
                .video
                .report_video_progress(context, route, target, &status, playing_time)
                .await?;
            let percent = video_percent(playing_time, duration);
            events
                .report(ProviderProgress {
                    percent: Some(percent),
                    stage: "video_execute".to_owned(),
                    status_text: Some("视频进度已上报".to_owned()),
                    completed_items: Some(u32::from(passed)),
                    total_items: Some(1),
                })
                .await?;
            if passed || playing_time == duration {
                break;
            }
        }
        events
            .log(execution_log(
                "video_verify",
                "视频进度上报结束，开始复核远端任务状态",
                None,
            ))
            .await?;
        let verified = self
            .fetch_video_target(context, route, resource_request, remote_task_id)
            .await?;
        if verified.remote_state() != RemoteState::Completed {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing Video did not become completed after progress reporting",
            ));
        }
        events
            .report(ProviderProgress {
                percent: Some(100),
                stage: "video_verified".to_owned(),
                status_text: Some("视频远端完成状态已复核".to_owned()),
                completed_items: Some(1),
                total_items: Some(1),
            })
            .await?;
        events
            .log(execution_log(
                "video_verified",
                "视频远端完成状态已复核",
                Some(serde_json::json!({"resource_kind": "video", "verified": true})),
            ))
            .await?;
        Ok(video_outcome(false))
    }
}

impl fmt::Debug for ChaoxingResourceExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingResourceExecution")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("inventory", &"configured")
            .field("immediate", &"configured")
            .field("video", &"configured")
            .field("video_sleeper", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingResourceExecution {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskExecutionCapability for ChaoxingResourceExecution {
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing resource execution accepts only ResourceExecution",
            ));
        }
        let settings = ChaoxingRuntimeSettings::resolve(&request.runtime_settings)?;
        let identity = ResourceIdentity::parse(&request.remote_task_id)?;
        let course = self.resolve_course(context, &identity).await?;
        let route = ChaoxingCourseRoute::from_remote_course(&course)?;
        let resource_request = self
            .resolve_resource_request(context, route, &identity)
            .await?;
        let documents = self
            .fetch_resource_documents(context, route, &resource_request)
            .await?;
        let (_, kind) = locate_resource_fact(
            &documents,
            route,
            &resource_request,
            &request.remote_task_id,
        )?;
        match kind {
            ExecutableResourceKind::Immediate => {
                self.execute_immediate(
                    context,
                    route,
                    &resource_request,
                    &request.remote_task_id,
                    documents,
                    events,
                )
                .await
            }
            ExecutableResourceKind::Video => {
                let target = locate_video_target(
                    documents,
                    route,
                    &resource_request,
                    &request.remote_task_id,
                )?;
                self.execute_video(
                    context,
                    route,
                    &resource_request,
                    &request.remote_task_id,
                    &target,
                    settings,
                    events,
                )
                .await
            }
            ExecutableResourceKind::Live | ExecutableResourceKind::Unsupported => {
                Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "Chaoxing resource kind has no execution path",
                ))
            }
        }
    }
}

fn execution_log(
    stage: &str,
    message: &str,
    metadata_sanitized: Option<serde_json::Value>,
) -> ProviderExecutionLog {
    ProviderExecutionLog {
        level: LogLevel::Info,
        stage: stage.to_owned(),
        message: message.to_owned(),
        provider_trace_id: None,
        metadata_sanitized,
    }
}

#[async_trait]
impl TaskProgressCapability for ChaoxingResourceExecution {
    async fn read_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress> {
        self.resolve_resource_progress(context, remote_task_id)
            .await
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing resource capability received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing resource capability requires an authenticated session",
        ));
    }
    Ok(())
}

fn locate_target(
    documents: Vec<ChaoxingChapterResourceDocument>,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<ChaoxingImmediateResourceTarget> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing immediate execution received an incomplete card set",
        ));
    }
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing immediate execution received a foreign or duplicate card",
            ));
        }
    }
    let scope = route.parser_scope()?;
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed
            .remove(&card_index)
            .ok_or_else(|| protocol_drift("Chaoxing immediate execution omitted a card"))?;
        let Some(target) = locate_immediate_resource_target(
            document.as_str(),
            &scope,
            request.knowledge_id(),
            card_index,
            remote_task_id,
        )?
        else {
            continue;
        };
        if found.replace(target).is_some() {
            return Err(protocol_drift(
                "Chaoxing immediate execution found the task on multiple cards",
            ));
        }
    }
    found.ok_or_else(remote_resource_changed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutableResourceKind {
    Immediate,
    Video,
    Live,
    Unsupported,
}

fn locate_resource_fact(
    documents: &[ChaoxingChapterResourceDocument],
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<(RemoteState, ExecutableResourceKind)> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing resource lookup received an incomplete card set",
        ));
    }
    let scope = route.parser_scope()?;
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing resource lookup received a foreign or duplicate card",
            ));
        }
    }
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed
            .remove(&card_index)
            .ok_or_else(|| protocol_drift("Chaoxing resource lookup omitted a card"))?;
        for task in parse_chapter_resource_inventory(
            document.as_str(),
            &scope,
            request.knowledge_id(),
            card_index,
        )? {
            if task.remote_id != remote_task_id {
                continue;
            }
            let kind = match task
                .normalized
                .get("resource_kind")
                .and_then(serde_json::Value::as_str)
            {
                Some("document" | "read") => ExecutableResourceKind::Immediate,
                Some("video") => ExecutableResourceKind::Video,
                Some("live") => ExecutableResourceKind::Live,
                Some(_) => ExecutableResourceKind::Unsupported,
                None => {
                    return Err(protocol_drift(
                        "Chaoxing resource task has no normalized kind",
                    ));
                }
            };
            if found.replace((task.remote_state, kind)).is_some() {
                return Err(protocol_drift(
                    "Chaoxing resource lookup found the task on multiple cards",
                ));
            }
        }
    }
    found.ok_or_else(remote_resource_changed)
}

fn locate_video_target(
    documents: Vec<ChaoxingChapterResourceDocument>,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<ChaoxingVideoResourceTarget> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing Video execution received an incomplete card set",
        ));
    }
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing Video execution received a foreign or duplicate card",
            ));
        }
    }
    let scope = route.parser_scope()?;
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed
            .remove(&card_index)
            .ok_or_else(|| protocol_drift("Chaoxing Video execution omitted a card"))?;
        let Some(target) = locate_video_resource_target(
            document.as_str(),
            &scope,
            request.knowledge_id(),
            card_index,
            remote_task_id,
        )?
        else {
            continue;
        };
        if found.replace(target).is_some() {
            return Err(protocol_drift(
                "Chaoxing Video execution found the task on multiple cards",
            ));
        }
    }
    found.ok_or_else(remote_resource_changed)
}

struct ResourceIdentity<'a> {
    course: &'a str,
    class: &'a str,
    knowledge: &'a str,
}

impl<'a> ResourceIdentity<'a> {
    fn parse(remote_task_id: &'a str) -> ProviderResult<Self> {
        let components = remote_task_id.split(':').collect::<Vec<_>>();
        if components.len() != 5
            || components[0] != "resource"
            || !valid_component(components[1])
            || !valid_component(components[2])
            || !(1..=20).contains(&components[3].len())
            || !components[3].bytes().all(|byte| byte.is_ascii_digit())
            || !valid_component(components[4])
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing execution request is not a valid resource task",
            ));
        }
        Ok(Self {
            course: components[1],
            class: components[2],
            knowledge: components[3],
        })
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

const fn resource_kind_name(kind: ChaoxingImmediateResourceKind) -> &'static str {
    match kind {
        ChaoxingImmediateResourceKind::Document => "document",
        ChaoxingImmediateResourceKind::Read => "read",
    }
}

fn completed_outcome(
    kind: ChaoxingImmediateResourceKind,
    already_completed: bool,
) -> ExecutionOutcome {
    ExecutionOutcome {
        remote_state: RemoteState::Completed,
        verified: true,
        result_sanitized: serde_json::json!({
            "schema": "chaoxing.immediate-resource-result.v1",
            "resource_kind": resource_kind_name(kind),
            "already_completed": already_completed,
            "verification": "fresh_card_state",
        }),
    }
}

fn video_outcome(already_completed: bool) -> ExecutionOutcome {
    ExecutionOutcome {
        remote_state: RemoteState::Completed,
        verified: true,
        result_sanitized: serde_json::json!({
            "schema": "chaoxing.video-result.v1",
            "resource_kind": "video",
            "already_completed": already_completed,
            "verification": "fresh_card_state",
        }),
    }
}

fn real_playback_duration(
    video_seconds: u64,
    playback_rate_millis: u16,
) -> ProviderResult<Duration> {
    let milliseconds = video_seconds
        .checked_mul(1_000_000)
        .map(|value| value.div_ceil(u64::from(playback_rate_millis)))
        .ok_or_else(|| protocol_drift("Chaoxing Video playback duration overflowed"))?;
    Ok(Duration::from_millis(milliseconds))
}

fn video_percent(playing_time: u64, duration: u64) -> u8 {
    u8::try_from(playing_time.saturating_mul(100) / duration)
        .unwrap_or(100)
        .min(100)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_resource_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "Chaoxing resource task is no longer available for execution",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, TaskId};
    use asterism_provider_api::{ProviderRouteContext, RemoteCourse};

    use super::*;
    use crate::{ChaoxingInventoryDocument, ChaoxingWorkDetailRequest, ChaoxingWorkDetailState};

    const CHAPTER_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
    const RESOURCE_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");

    struct FixtureProvider {
        metadata: ProviderMetadata,
        completed_read: AtomicBool,
        completed_video: AtomicBool,
        resource_calls: AtomicUsize,
        execute_calls: AtomicUsize,
        video_reports: Mutex<Vec<u64>>,
        video_sleeps: Mutex<Vec<Duration>>,
    }

    impl FixtureProvider {
        fn new(completed_read: bool) -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                completed_read: AtomicBool::new(completed_read),
                completed_video: AtomicBool::new(false),
                resource_calls: AtomicUsize::new(0),
                execute_calls: AtomicUsize::new(0),
                video_reports: Mutex::new(Vec::new()),
                video_sleeps: Mutex::new(Vec::new()),
            }
        }
    }

    impl fmt::Debug for FixtureProvider {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("FixtureProvider")
        }
    }

    impl ProviderIdentity for FixtureProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for FixtureProvider {
        async fn list_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            Ok(vec![course()])
        }
    }

    #[async_trait]
    impl ChaoxingInventoryTransport for FixtureProvider {
        async fn fetch_chapter_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            ChaoxingInventoryDocument::try_new(CHAPTER_MIXED)
        }

        async fn fetch_work_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            Err(unsupported_fixture_call())
        }

        async fn fetch_chapter_resource_inventories(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            requests: &[ChaoxingChapterResourceRequest],
        ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
            assert_eq!(requests.len(), 1);
            self.resource_calls.fetch_add(1, Ordering::Relaxed);
            let mut first = RESOURCE_MIXED.to_owned();
            if self.completed_read.load(Ordering::Relaxed) {
                first = first.replace(
                    "\"jobid\":\"job-read\",\"isPassed\":false",
                    "\"jobid\":\"job-read\",\"isPassed\":true",
                );
            }
            if self.completed_video.load(Ordering::Relaxed) {
                first = first.replace(
                    "\"jobid\":\"job-video\",\"isPassed\":false",
                    "\"jobid\":\"job-video\",\"isPassed\":true",
                );
            }
            (0..CHAPTER_RESOURCE_CARD_COUNT)
                .map(|card_index| {
                    ChaoxingChapterResourceDocument::for_request(
                        &requests[0],
                        card_index,
                        if card_index == 0 {
                            first.as_str()
                        } else {
                            "<html><body>empty card slot</body></html>"
                        },
                    )
                })
                .collect()
        }

        async fn fetch_exam_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            Err(unsupported_fixture_call())
        }

        async fn fetch_work_detail_states(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            _requests: &[ChaoxingWorkDetailRequest<'_>],
        ) -> ProviderResult<Vec<ChaoxingWorkDetailState>> {
            Err(unsupported_fixture_call())
        }
    }

    #[async_trait]
    impl ChaoxingImmediateResourceTransport for FixtureProvider {
        async fn complete_immediate_resource(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            knowledge_id: &str,
            target: &ChaoxingImmediateResourceTarget,
        ) -> ProviderResult<()> {
            assert_eq!(knowledge_id, "4001");
            assert_eq!(target.kind(), ChaoxingImmediateResourceKind::Read);
            assert_eq!(
                target.token().unwrap().expose_secret(),
                "PRIVATE_READ_TOKEN"
            );
            self.execute_calls.fetch_add(1, Ordering::Relaxed);
            self.completed_read.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    #[async_trait]
    impl ChaoxingVideoTransport for FixtureProvider {
        async fn video_status(
            &self,
            _context: &ProviderContext,
            target: &ChaoxingVideoResourceTarget,
        ) -> ProviderResult<ChaoxingVideoStatus> {
            assert_eq!(target.job_id(), "job-video");
            ChaoxingVideoStatus::try_new("SAFE_REPORT_TOKEN", 135, 15)
        }

        async fn report_video_progress(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            _target: &ChaoxingVideoResourceTarget,
            _status: &ChaoxingVideoStatus,
            playing_time_seconds: u64,
        ) -> ProviderResult<bool> {
            self.video_reports
                .lock()
                .unwrap()
                .push(playing_time_seconds);
            let completed = playing_time_seconds == 135;
            self.completed_video.store(completed, Ordering::Relaxed);
            Ok(completed)
        }
    }

    #[async_trait]
    impl ChaoxingVideoSleeper for FixtureProvider {
        async fn sleep(&self, duration: Duration) {
            self.video_sleeps.lock().unwrap().push(duration);
        }
    }

    #[derive(Debug, Default)]
    struct RecordingEvents {
        progress: AtomicUsize,
        logs: AtomicUsize,
    }

    #[async_trait]
    impl ExecutionEventSink for RecordingEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            self.progress.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn log(&self, event: ProviderExecutionLog) -> ProviderResult<()> {
            event.validate().unwrap();
            self.logs.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[tokio::test]
    async fn immediate_execution_refetches_and_verifies_the_remote_card() {
        let fixture = Arc::new(FixtureProvider::new(false));
        let execution = ChaoxingResourceExecution::try_new(
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
        )
        .unwrap();
        let events = RecordingEvents::default();
        let outcome = execution
            .execute(
                &context(),
                &execution_request("resource:100:200:4001:job-read"),
                &events,
            )
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["resource_kind"], "read");
        assert_eq!(fixture.resource_calls.load(Ordering::Relaxed), 2);
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 1);
        assert_eq!(events.progress.load(Ordering::Relaxed), 3);
        assert_eq!(events.logs.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn progress_read_refetches_remote_state_without_executing() {
        let fixture = Arc::new(FixtureProvider::new(false));
        let execution = ChaoxingResourceExecution::try_new(
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
        )
        .unwrap();
        let pending = execution
            .read_progress(&context(), "resource:100:200:4001:job-read")
            .await
            .unwrap();
        assert_eq!(pending.remote_state, RemoteState::Pending);
        assert_eq!(pending.percent, Some(0));
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 0);

        let pending_video = execution
            .read_progress(&context(), "resource:100:200:4001:job-video")
            .await
            .unwrap();
        assert_eq!(pending_video.remote_state, RemoteState::Pending);
        assert_eq!(pending_video.percent, Some(0));

        let pending_live = execution
            .read_progress(&context(), "resource:100:200:4001:job-live")
            .await
            .unwrap();
        assert_eq!(pending_live.remote_state, RemoteState::Pending);
        assert_eq!(pending_live.percent, Some(0));

        fixture.completed_read.store(true, Ordering::Relaxed);
        let completed = execution
            .read_progress(&context(), "resource:100:200:4001:job-read")
            .await
            .unwrap();
        assert_eq!(completed.remote_state, RemoteState::Completed);
        assert_eq!(completed.percent, Some(100));
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn completed_document_is_idempotent() {
        let fixture = Arc::new(FixtureProvider::new(false));
        let execution = ChaoxingResourceExecution::try_new(
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
        )
        .unwrap();
        let events = RecordingEvents::default();
        let completed = execution
            .execute(
                &context(),
                &execution_request("resource:100:200:4001:job-document"),
                &events,
            )
            .await
            .unwrap();
        assert_eq!(completed.result_sanitized["already_completed"], true);
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 0);
        assert_eq!(events.logs.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn video_execution_uses_frozen_speed_and_verifies_the_fresh_card() {
        let fixture = Arc::new(FixtureProvider::new(false));
        let mut execution = ChaoxingResourceExecution::try_new(
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
            fixture.clone(),
        )
        .unwrap();
        execution.video_sleeper = fixture.clone();
        let events = RecordingEvents::default();
        let outcome = execution
            .execute(
                &context(),
                &execution_request_with_speed("resource:100:200:4001:job-video", 1_500),
                &events,
            )
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["resource_kind"], "video");
        assert_eq!(*fixture.video_reports.lock().unwrap(), [75_u64, 135_u64]);
        assert_eq!(
            *fixture.video_sleeps.lock().unwrap(),
            [Duration::from_secs(40), Duration::from_secs(40)]
        );
        assert_eq!(fixture.resource_calls.load(Ordering::Relaxed), 2);
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 0);
        assert_eq!(events.progress.load(Ordering::Relaxed), 3);
        assert_eq!(events.logs.load(Ordering::Relaxed), 3);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-resource-execution-test".to_owned(),
        }
    }

    fn execution_request(remote_task_id: &str) -> ExecutionRequest {
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: remote_task_id.to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: crate::runtime_settings::runtime_settings_schema()
                .resolve(None, None, None)
                .unwrap(),
            provider_plan_artifact: None,
        }
    }

    fn execution_request_with_speed(remote_task_id: &str, speed_millis: i64) -> ExecutionRequest {
        let schema = crate::runtime_settings::runtime_settings_schema();
        let task = asterism_provider_api::ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                crate::runtime_settings::VIDEO_PLAYBACK_RATE_KEY.to_owned(),
                asterism_provider_api::ProviderSettingValue::DecimalMillis(speed_millis),
            )]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: remote_task_id.to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
            provider_plan_artifact: None,
        }
    }

    fn course() -> RemoteCourse {
        RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({"safe": true}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        }
    }

    fn unsupported_fixture_call() -> ProviderError {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "unexpected fixture transport call",
        )
    }
}
