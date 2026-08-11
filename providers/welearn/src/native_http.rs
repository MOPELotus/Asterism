use std::{fmt, sync::Arc, time::Duration};

use asterism_domain::{HumanRequiredReason, LogLevel};
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    ExecutionEventSink, ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionLog,
    ProviderProgress, ProviderResult, RemoteCourse,
};
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{CONTENT_TYPE, COOKIE, HeaderMap, LOCATION, REFERER, RETRY_AFTER},
};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    WellearnCmiDocument, WellearnCmiTransport, WellearnCourseInventoryTransport,
    WellearnDurationReportDocuments, WellearnDurationReportTransport, WellearnInventoryDocument,
    WellearnResourceExecutionDocuments, WellearnResourceExecutionTransport,
    WellearnScoLeavesDocument, WellearnSessionResolver, WellearnTaskInventoryDocuments,
    WellearnTaskInventoryTransport,
    cmi::{WellearnCmiSnapshot, parse_cmi_snapshot},
    course_context::{parse_course_context, parse_course_context_for_id},
    course_inventory::course_id_from_remote,
    task_inventory::unit_count,
};

const COURSE_LIST_URL: &str = "https://welearn.sflep.com/ajax/authCourse.aspx?action=gmc";
const COURSE_INDEX_REFERER: &str = "https://welearn.sflep.com/student/index.aspx";
const COURSE_INFO_ORIGIN: &str = "https://welearn.sflep.com";
const COURSE_INFO_PATH: &str = "/student/course_info.aspx";
const STUDY_STAT_URL: &str = "https://welearn.sflep.com/ajax/StudyStat.aspx";
const SCO_URL: &str = "https://welearn.sflep.com/Ajax/SCO.aspx";
const STUDY_COURSE_REFERER: &str = "https://welearn.sflep.com/student/StudyCourse.aspx";
const MAX_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;

/// Native, non-redirecting `WELearn` Course/Unit/SCO inventory transport.
pub struct NativeWellearnInventoryTransport {
    client: Client,
    sessions: Arc<dyn WellearnSessionResolver>,
}

impl NativeWellearnInventoryTransport {
    /// Builds the transport from the shared network policy and account-scoped
    /// stored-session resolver.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error if the shared HTTP client cannot be
    /// initialized.
    pub fn try_new(
        network: &ResolvedNetworkProfile,
        sessions: Arc<dyn WellearnSessionResolver>,
    ) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn inventory HTTP client initialization failed",
            )
        })?;
        Ok(Self { client, sessions })
    }

    async fn send_get_with_session(
        &self,
        session: &crate::WellearnCookieSession,
        url: Url,
        referer: &str,
        expected_content: ResponseContent,
    ) -> ProviderResult<WellearnInventoryDocument> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, referer)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_inventory_response(response, expected_content).await
    }

    async fn session_for_operation(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<(crate::WellearnCookieSession, bool)> {
        match self.sessions.resolve_session(context).await {
            Ok(session) => Ok((session, false)),
            Err(error) if error.kind == ProviderErrorKind::Authentication => {
                Ok((self.sessions.renew_session(context).await?, true))
            }
            Err(error) => Err(error),
        }
    }

    async fn fetch_tasks_once(
        &self,
        session: &crate::WellearnCookieSession,
        course: &RemoteCourse,
    ) -> ProviderResult<WellearnTaskInventoryDocuments> {
        let course_id = course_id_from_remote(course)?;
        let course_url = course_info_url(&course_id)?;
        let course_page = self
            .send_get_with_session(
                session,
                course_url.clone(),
                COURSE_INDEX_REFERER,
                ResponseContent::Html,
            )
            .await?;
        let route = parse_course_context(course, course_page.as_str())?;
        let unit_response = self
            .client
            .post(static_url(STUDY_STAT_URL)?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, course_url.as_str())
            .form(&[
                ("action", "courseunits"),
                ("cid", route.course_id()),
                ("uid", route.user_id()),
            ])
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let units = read_inventory_response(unit_response, ResponseContent::Json).await?;
        let count = unit_count(units.as_str())?;
        let mut leaves = Vec::with_capacity(count);
        for index in 0..count {
            let unit_index = u32::try_from(index).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "WELearn Unit index exceeds the supported range",
                )
            })?;
            let leaves_url = sco_leaves_url(&route, unit_index)?;
            let document = self
                .send_get_with_session(
                    session,
                    leaves_url,
                    course_url.as_str(),
                    ResponseContent::Json,
                )
                .await?;
            leaves.push(WellearnScoLeavesDocument::try_new(
                unit_index,
                document.as_str(),
            )?);
        }
        Ok(WellearnTaskInventoryDocuments::new(units, leaves))
    }

    async fn fetch_cmi_once(
        &self,
        session: &crate::WellearnCookieSession,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument> {
        let route = self.resolve_course_route(session, course_id).await?;
        self.fetch_cmi_for_route(session, &route, sco_id).await
    }

    async fn resolve_course_route(
        &self,
        session: &crate::WellearnCookieSession,
        course_id: &str,
    ) -> ProviderResult<crate::WellearnCourseContext> {
        let course_page = self
            .send_get_with_session(
                session,
                course_info_url(course_id)?,
                COURSE_INDEX_REFERER,
                ResponseContent::Html,
            )
            .await?;
        parse_course_context_for_id(course_id.to_owned(), course_page.as_str())
    }

    async fn fetch_cmi_for_route(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument> {
        let response = self
            .client
            .post(sco_url(route.user_id())?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, STUDY_COURSE_REFERER)
            .form(&[
                ("action", "getscoinfo_v7"),
                ("uid", route.user_id()),
                ("cid", route.course_id()),
                ("scoid", sco_id),
            ])
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let document = read_inventory_response(response, ResponseContent::Json).await?;
        WellearnCmiDocument::try_new(document.as_str().to_owned())
    }

    async fn send_sco_form(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        fields: &[(&str, &str)],
    ) -> ProviderResult<WellearnInventoryDocument> {
        let response = self
            .client
            .post(sco_url(route.user_id())?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, STUDY_COURSE_REFERER)
            .form(fields)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_inventory_response(response, ResponseContent::Json).await
    }

    async fn send_sco_form_with_referer(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        referer: &Url,
        fields: &[(&str, &str)],
    ) -> ProviderResult<WellearnInventoryDocument> {
        let response = self
            .client
            .post(sco_url(route.user_id())?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, referer.as_str())
            .form(fields)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_inventory_response(response, ResponseContent::Json).await
    }
}

impl fmt::Debug for NativeWellearnInventoryTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeWellearnInventoryTransport")
            .field("client", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

#[async_trait]
impl WellearnCourseInventoryTransport for NativeWellearnInventoryTransport {
    async fn fetch_courses(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnInventoryDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match fetch_course_inventory_document(&self.client, &session).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                fetch_course_inventory_document(&self.client, &session).await
            }
            result => result,
        }
    }
}

pub(crate) async fn fetch_course_inventory_document(
    client: &Client,
    session: &crate::WellearnCookieSession,
) -> ProviderResult<WellearnInventoryDocument> {
    let response = client
        .get(static_url(COURSE_LIST_URL)?)
        .header(COOKIE, session.expose_secret())
        .header(REFERER, COURSE_INDEX_REFERER)
        .send()
        .await
        .map_err(|error| classify_reqwest_error(&error))?;
    read_inventory_response(response, ResponseContent::Json).await
}

#[async_trait]
impl WellearnTaskInventoryTransport for NativeWellearnInventoryTransport {
    async fn fetch_tasks(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<WellearnTaskInventoryDocuments> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_tasks_once(&session, course).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_tasks_once(&session, course).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl WellearnCmiTransport for NativeWellearnInventoryTransport {
    async fn fetch_cmi(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_cmi_once(&session, course_id, sco_id).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_cmi_once(&session, course_id, sco_id).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl WellearnResourceExecutionTransport for NativeWellearnInventoryTransport {
    #[allow(
        clippy::too_many_lines,
        reason = "the non-idempotent mutation sequence keeps every no-replay boundary explicit"
    )]
    async fn complete_resource(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        score_percent: u8,
    ) -> ProviderResult<WellearnResourceExecutionDocuments> {
        if score_percent > 100 {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn resource transport received an out-of-range score",
            ));
        }
        let (mut session, mut renewed) = self.session_for_operation(context).await?;
        let (route, before) = match self
            .read_duration_baseline(&session, course_id, sco_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                renewed = true;
                self.read_duration_baseline(&session, course_id, sco_id)
                    .await?
            }
            result => result?,
        };
        let baseline = parse_cmi_snapshot(before.as_str())?;
        let expected_score = score_percent.to_string();
        if baseline.remote_state() == asterism_domain::RemoteState::Completed
            && baseline.percent() == Some(100)
            && baseline.score_scaled_raw() == Some(expected_score.as_str())
        {
            return Ok(WellearnResourceExecutionDocuments::already_completed(
                before,
            ));
        }

        let referer = study_course_url(&route, sco_id)?;
        let score = score_percent.to_string();
        let cmi = resource_completion_cmi(score_percent)?;
        let start = self
            .send_sco_form_with_referer(
                &session,
                &route,
                &referer,
                &[
                    ("action", "startsco160928"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("classid", route.class_id()),
                    ("tid", "-1"),
                ],
            )
            .await
            .map_err(resource_mutation_error)?;
        parse_mutation_response(start.as_str(), MutationResponseKind::StrictSuccess)
            .map_err(resource_mutation_error)?;

        let set = self
            .send_sco_form_with_referer(
                &session,
                &route,
                &referer,
                &[
                    ("action", "setscoinfo"),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("uid", route.user_id()),
                    ("data", cmi.as_str()),
                    ("isend", "False"),
                ],
            )
            .await
            .map_err(resource_mutation_error)?;
        parse_mutation_response(set.as_str(), MutationResponseKind::StrictSuccess)
            .map_err(resource_mutation_error)?;

        let save = self
            .send_sco_form_with_referer(
                &session,
                &route,
                &referer,
                &[
                    ("action", "savescoinfo160928"),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("uid", route.user_id()),
                    ("progress", "100"),
                    ("crate", &score),
                    ("status", "unknown"),
                    ("cstatus", "completed"),
                    ("trycount", "0"),
                ],
            )
            .await
            .map_err(resource_mutation_error)?;
        parse_mutation_response(save.as_str(), MutationResponseKind::StrictSuccess)
            .map_err(resource_mutation_error)?;

        let after = match self.fetch_cmi_for_route(&session, &route, sco_id).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self
                    .sessions
                    .renew_session(context)
                    .await
                    .map_err(resource_mutation_error)?;
                let route = self
                    .resolve_course_route(&session, course_id)
                    .await
                    .map_err(resource_mutation_error)?;
                self.fetch_cmi_for_route(&session, &route, sco_id)
                    .await
                    .map_err(resource_mutation_error)?
            }
            result => result.map_err(resource_mutation_error)?,
        };
        Ok(WellearnResourceExecutionDocuments::submitted(
            before, after, true,
        ))
    }

    async fn verify_resource(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument> {
        WellearnCmiTransport::fetch_cmi(self, context, course_id, sco_id).await
    }
}

fn resource_mutation_error(error: ProviderError) -> ProviderError {
    match error.kind {
        ProviderErrorKind::Authentication => ProviderError::human_required(
            "WELearn session expired after resource mutation began; execution was not replayed",
            HumanRequiredReason::SessionExpired,
        ),
        ProviderErrorKind::RateLimited
        | ProviderErrorKind::Network
        | ProviderErrorKind::ProviderUnavailable
        | ProviderErrorKind::ProtocolDrift
        | ProviderErrorKind::InvalidResponse => ProviderError::human_required(
            "WELearn resource mutation outcome is uncertain and requires fresh manual review",
            HumanRequiredReason::ManualIntervention,
        ),
        _ => error,
    }
}

#[async_trait]
impl WellearnDurationReportTransport for NativeWellearnInventoryTransport {
    #[allow(
        clippy::too_many_lines,
        reason = "the duration operation keeps session renewal, mutation ordering and event emission explicit"
    )]
    async fn report_duration(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        duration_seconds: u64,
        heartbeat_interval_seconds: u64,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<WellearnDurationReportDocuments> {
        if !(60..=7_200).contains(&duration_seconds)
            || !(30..=90).contains(&heartbeat_interval_seconds)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn duration transport received out-of-range runtime settings",
            ));
        }
        let (mut session, mut renewed) = self.session_for_operation(context).await?;
        let (mut route, mut before) = match self
            .read_duration_baseline(&session, course_id, sco_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                renewed = true;
                self.read_duration_baseline(&session, course_id, sco_id)
                    .await?
            }
            result => result?,
        };
        let mut snapshot = parse_cmi_snapshot(before.as_str())?;
        let mut started = false;
        if !snapshot.cmi_present() {
            events
                .log(duration_log(
                    "duration_start",
                    "远端尚无 CMI，正在启动学习会话",
                    None,
                ))
                .await?;
            let start = self
                .send_sco_form(
                    &session,
                    &route,
                    &[
                        ("action", "startsco160928"),
                        ("uid", route.user_id()),
                        ("cid", route.course_id()),
                        ("scoid", sco_id),
                        ("classid", route.class_id()),
                        ("tid", "-1"),
                    ],
                )
                .await?;
            parse_mutation_response(start.as_str(), MutationResponseKind::StrictSuccess)?;
            started = true;
            before = self.fetch_cmi_for_route(&session, &route, sco_id).await?;
            snapshot = parse_cmi_snapshot(before.as_str())?;
        }

        let state = PreservedCmiState::try_from_snapshot(&snapshot)?;
        events
            .report(ProviderProgress {
                percent: Some(0),
                stage: "duration_execute".to_owned(),
                status_text: Some("学习时长会话已开始".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        events
            .log(duration_log(
                "duration_execute",
                "开始按冻结运行参数执行时长心跳",
                Some(serde_json::json!({
                    "duration_report_seconds": duration_seconds,
                    "heartbeat_interval_seconds": heartbeat_interval_seconds,
                })),
            ))
            .await?;

        let mut heartbeat_count = 0_u32;
        self.keep_duration(&session, &route, sco_id, &state).await?;
        heartbeat_count = heartbeat_count.saturating_add(1);
        let mut elapsed = 0_u64;
        while elapsed < duration_seconds {
            let wait = heartbeat_interval_seconds.min(duration_seconds - elapsed);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            elapsed = elapsed.saturating_add(wait);
            self.keep_duration(&session, &route, sco_id, &state).await?;
            heartbeat_count = heartbeat_count.saturating_add(1);
            let percent = u8::try_from(elapsed.saturating_mul(90) / duration_seconds)
                .unwrap_or(90)
                .min(90);
            events
                .report(ProviderProgress {
                    percent: Some(percent),
                    stage: "duration_execute".to_owned(),
                    status_text: Some("学习时长心跳已上报".to_owned()),
                    completed_items: Some(0),
                    total_items: Some(1),
                })
                .await?;
        }

        events
            .report(ProviderProgress {
                percent: Some(95),
                stage: "duration_finalize".to_owned(),
                status_text: Some("正在结束学习会话".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        self.finalize_duration(&session, &route, sco_id, &state)
            .await?;

        let after = match self.fetch_cmi_for_route(&session, &route, sco_id).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                route = self.resolve_course_route(&session, course_id).await?;
                self.fetch_cmi_for_route(&session, &route, sco_id).await?
            }
            result => result?,
        };
        Ok(WellearnDurationReportDocuments::new(
            before,
            after,
            started,
            heartbeat_count,
        ))
    }
}

impl NativeWellearnInventoryTransport {
    async fn read_duration_baseline(
        &self,
        session: &crate::WellearnCookieSession,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<(crate::WellearnCourseContext, WellearnCmiDocument)> {
        let route = self.resolve_course_route(session, course_id).await?;
        let document = self.fetch_cmi_for_route(session, &route, sco_id).await?;
        Ok((route, document))
    }

    async fn keep_duration(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        sco_id: &str,
        state: &PreservedCmiState,
    ) -> ProviderResult<()> {
        let response = self
            .send_sco_form(
                session,
                route,
                &[
                    ("action", "keepsco_with_getticket_with_updatecmitime"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("session_time", &state.session_time),
                    ("total_time", &state.total_time),
                    ("timelimitsec", "0"),
                    ("endcaltime", "false"),
                ],
            )
            .await?;
        parse_mutation_response(response.as_str(), MutationResponseKind::Heartbeat)
    }

    async fn finalize_duration(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        sco_id: &str,
        state: &PreservedCmiState,
    ) -> ProviderResult<()> {
        let response = self
            .send_sco_form(
                session,
                route,
                &[
                    ("action", "savescoinfo160928"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("progress", &state.progress),
                    ("crate", &state.score_scaled),
                    ("status", &state.success_status),
                    ("cstatus", &state.completion_status),
                    ("trycount", "0"),
                ],
            )
            .await?;
        parse_mutation_response(response.as_str(), MutationResponseKind::StrictSuccess)
    }
}

struct PreservedCmiState {
    completion_status: String,
    progress: String,
    score_scaled: String,
    success_status: String,
    session_time: String,
    total_time: String,
}

impl PreservedCmiState {
    fn try_from_snapshot(snapshot: &WellearnCmiSnapshot) -> ProviderResult<Self> {
        let required = |value: Option<&str>, field: &'static str| {
            value.map(str::to_owned).ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    format!("WELearn duration baseline has no {field}"),
                )
            })
        };
        if !snapshot.cmi_present() {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn duration baseline still has no CMI after start",
            ));
        }
        Ok(Self {
            completion_status: required(snapshot.completion_raw(), "completion status")?,
            progress: required(snapshot.progress_raw(), "progress measure")?,
            score_scaled: required(snapshot.score_scaled_raw(), "scaled score")?,
            success_status: required(snapshot.success_status_raw(), "success status")?,
            session_time: required(snapshot.session_time_raw(), "session time")?,
            total_time: required(snapshot.total_time_raw(), "total time")?,
        })
    }
}

impl Drop for PreservedCmiState {
    fn drop(&mut self) {
        self.completion_status.zeroize();
        self.progress.zeroize();
        self.score_scaled.zeroize();
        self.success_status.zeroize();
        self.session_time.zeroize();
        self.total_time.zeroize();
    }
}

#[derive(Clone, Copy)]
enum MutationResponseKind {
    StrictSuccess,
    Heartbeat,
}

fn parse_mutation_response(document: &str, kind: MutationResponseKind) -> ProviderResult<()> {
    let value: serde_json::Value = serde_json::from_str(document).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn SCO mutation response is not valid JSON",
        )
    })?;
    let result = value
        .as_object()
        .and_then(|object| object.get("ret"))
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn SCO mutation response has no integer result",
            )
        })?;
    let accepted = result == 0 || matches!(kind, MutationResponseKind::Heartbeat) && result == 1;
    if !accepted {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn SCO mutation was not accepted",
        ));
    }
    Ok(())
}

fn resource_completion_cmi(score_percent: u8) -> ProviderResult<Zeroizing<String>> {
    if score_percent > 100 {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn completion CMI received an out-of-range score",
        ));
    }
    let score = score_percent.to_string();
    serde_json::to_string(&serde_json::json!({
        "cmi": {
            "completion_status": "completed",
            "interactions": [],
            "launch_data": "",
            "progress_measure": "1",
            "score": {"scaled": score, "raw": "100"},
            "session_time": "0",
            "success_status": "unknown",
            "total_time": "0",
            "mode": "normal"
        },
        "adl": {"data": []},
        "cci": {
            "data": [],
            "service": {
                "dictionary": {"headword": "", "short_cuts": ""},
                "new_words": [],
                "notes": [],
                "writing_marking": [],
                "record": {"files": []},
                "play": {"offline_media_id": "9999"}
            },
            "retry_count": "0",
            "submit_time": ""
        }
    }))
    .map(Zeroizing::new)
    .map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn completion CMI serialization failed",
        )
    })
}

fn duration_log(
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

#[derive(Clone, Copy)]
enum ResponseContent {
    Html,
    Json,
}

async fn read_inventory_response(
    mut response: Response,
    expected_content: ResponseContent,
) -> ProviderResult<WellearnInventoryDocument> {
    validate_response_head(&response)?;
    let content_type_result = validate_response_content_type(&response, expected_content);
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bytes.zeroize();
            return Err(oversized_response());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an empty response",
        ));
    }
    let document = match String::from_utf8(bytes) {
        Ok(document) => document,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "WELearn inventory endpoint returned invalid UTF-8",
            ));
        }
    };
    if looks_like_login_document(&document) {
        let mut document = document;
        document.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn returned a login page for the current session",
        ));
    }
    if let Err(error) = content_type_result {
        let mut document = document;
        document.zeroize();
        return Err(error);
    }
    WellearnInventoryDocument::try_new(document)
}

fn validate_response_head(response: &Response) -> ProviderResult<()> {
    validate_status(response.status(), response.headers())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(oversized_response());
    }
    Ok(())
}

fn validate_response_content_type(
    response: &Response,
    expected_content: ResponseContent,
) -> ProviderResult<()> {
    let Some(content_type) = response.headers().get(CONTENT_TYPE) else {
        return Ok(());
    };
    let content_type = content_type.to_str().map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an invalid Content-Type",
        )
    })?;
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    let valid = match expected_content {
        ResponseContent::Html => {
            media_type.eq_ignore_ascii_case("text/html")
                || media_type.eq_ignore_ascii_case("application/xhtml+xml")
        }
        ResponseContent::Json => {
            media_type.eq_ignore_ascii_case("application/json")
                || media_type.eq_ignore_ascii_case("text/json")
                || media_type.eq_ignore_ascii_case("text/plain")
        }
    };
    if !valid {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an unexpected content type",
        ));
    }
    Ok(())
}

fn validate_status(status: StatusCode, headers: &HeaderMap) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "WELearn rate limited the inventory request",
        );
        error.retry_after_seconds = headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds <= 3_600);
        return Err(error);
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn rejected the current session",
        ));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(ProviderError::new(
            ProviderErrorKind::Authorization,
            "WELearn denied access to the inventory route",
        ));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn inventory route was not found",
        ));
    }
    if status.is_redirection() {
        let kind = if headers
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(looks_like_login_location)
        {
            ProviderErrorKind::Authentication
        } else {
            ProviderErrorKind::ProtocolDrift
        };
        return Err(ProviderError::new(
            kind,
            "WELearn returned an unhandled inventory redirect",
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "WELearn inventory endpoint is temporarily unavailable",
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an unexpected status",
        ));
    }
    Ok(())
}

fn course_info_url(course_id: &str) -> ProviderResult<Url> {
    let mut url = Url::parse(COURSE_INFO_ORIGIN).map_err(|_| static_route_error())?;
    url.set_path(COURSE_INFO_PATH);
    url.query_pairs_mut().append_pair("cid", course_id);
    Ok(url)
}

fn sco_leaves_url(route: &crate::WellearnCourseContext, unit_index: u32) -> ProviderResult<Url> {
    let mut url = static_url(STUDY_STAT_URL)?;
    url.query_pairs_mut()
        .append_pair("action", "scoLeaves")
        .append_pair("cid", route.course_id())
        .append_pair("uid", route.user_id())
        .append_pair("unitidx", &unit_index.to_string())
        .append_pair("classid", route.class_id());
    Ok(url)
}

fn sco_url(user_id: &str) -> ProviderResult<Url> {
    let mut url = static_url(SCO_URL)?;
    url.query_pairs_mut().append_pair("uid", user_id);
    Ok(url)
}

fn study_course_url(route: &crate::WellearnCourseContext, sco_id: &str) -> ProviderResult<Url> {
    let mut url = static_url(STUDY_COURSE_REFERER)?;
    url.query_pairs_mut()
        .append_pair("cid", route.course_id())
        .append_pair("classid", route.class_id())
        .append_pair("sco", sco_id);
    Ok(url)
}

fn static_url(value: &'static str) -> ProviderResult<Url> {
    Url::parse(value).map_err(|_| static_route_error())
}

fn looks_like_login_location(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("sso.sflep.com"))
            || url.path().to_ascii_lowercase().contains("login")
    })
}

fn looks_like_login_document(document: &str) -> bool {
    let lowercase = document.to_ascii_lowercase();
    lowercase.contains("<form")
        && lowercase.contains("login")
        && lowercase.contains("account")
        && lowercase.contains("pwd")
}

pub(crate) fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::InvalidResponse
    };
    ProviderError::new(kind, "WELearn inventory HTTP request failed")
}

fn oversized_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "WELearn inventory response exceeds the size limit",
    )
}

fn static_route_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn compile-time inventory route is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_networking::NetworkProfile;

    use super::*;

    #[derive(Debug)]
    struct UnusedSessions;

    #[derive(Debug, Default)]
    struct RenewingSessions {
        renewals: AtomicUsize,
    }

    #[async_trait]
    impl WellearnSessionResolver for UnusedSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<crate::WellearnCookieSession> {
            Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "fixture session must not be resolved",
            ))
        }
    }

    #[async_trait]
    impl WellearnSessionResolver for RenewingSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<crate::WellearnCookieSession> {
            Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "fixture stored session expired",
            ))
        }

        async fn renew_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<crate::WellearnCookieSession> {
            self.renewals.fetch_add(1, Ordering::SeqCst);
            crate::WellearnCookieSession::try_new("session=RENEWED")
        }
    }

    #[test]
    fn native_transport_uses_the_shared_non_redirecting_client() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let transport =
            NativeWellearnInventoryTransport::try_new(&network, Arc::new(UnusedSessions)).unwrap();
        assert!(format!("{transport:?}").contains("configured"));
    }

    #[tokio::test]
    async fn operation_session_renews_only_an_authentication_failure() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let sessions = Arc::new(RenewingSessions::default());
        let transport =
            NativeWellearnInventoryTransport::try_new(&network, sessions.clone()).unwrap();
        let (session, renewed) = transport
            .session_for_operation(&ProviderContext {
                provider_id: asterism_domain::ProviderId::new("welearn").unwrap(),
                account_id: asterism_domain::ProviderAccountId::new(),
                credential_refs: vec![asterism_domain::SecretId::new()],
                correlation_id: "welearn-renewal-test".to_owned(),
            })
            .await
            .unwrap();
        assert!(renewed);
        assert_eq!(session.expose_secret(), "session=RENEWED");
        assert_eq!(sessions.renewals.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn inventory_routes_are_exact_and_encoded() {
        let course = course_info_url("course value").unwrap();
        assert_eq!(course.scheme(), "https");
        assert_eq!(course.host_str(), Some("welearn.sflep.com"));
        assert_eq!(course.path(), COURSE_INFO_PATH);
        assert_eq!(course.query(), Some("cid=course+value"));

        let courses = crate::parse_course_inventory(include_str!(
            "../../../fixtures/providers/welearn/courses/list-mixed.json"
        ))
        .unwrap();
        let context = parse_course_context(
            &courses[0],
            include_str!("../../../fixtures/providers/welearn/courses/course-context.html"),
        )
        .unwrap();
        let leaves = sco_leaves_url(&context, 3).unwrap();
        assert_eq!(
            leaves.query(),
            Some("action=scoLeaves&cid=1001&uid=7001&unitidx=3&classid=class-8001")
        );
        assert_eq!(
            sco_url("user value").unwrap().as_str(),
            "https://welearn.sflep.com/Ajax/SCO.aspx?uid=user+value"
        );
    }

    #[test]
    fn duration_mutation_results_are_strictly_classified() {
        assert!(
            parse_mutation_response(r#"{"ret":0}"#, MutationResponseKind::StrictSuccess).is_ok()
        );
        assert!(parse_mutation_response(r#"{"ret":1}"#, MutationResponseKind::Heartbeat).is_ok());
        for document in [
            r#"{"ret":1}"#,
            r#"{"ret":"0"}"#,
            r#"{"ok":true}"#,
            "not-json",
        ] {
            assert!(
                parse_mutation_response(document, MutationResponseKind::StrictSuccess).is_err()
            );
        }
        assert!(parse_mutation_response(r#"{"ret":2}"#, MutationResponseKind::Heartbeat).is_err());
    }

    #[test]
    fn resource_completion_cmi_is_bounded_and_matches_the_current_donor_shape() {
        let document = resource_completion_cmi(82).unwrap();
        let value: serde_json::Value = serde_json::from_str(document.as_str()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/welearn/cmi/resource-completion-cmi.expected.json"
        ))
        .unwrap();
        assert_eq!(value, expected);
        assert_eq!(value["cmi"]["completion_status"], "completed");
        assert_eq!(value["cmi"]["progress_measure"], "1");
        assert_eq!(value["cmi"]["score"]["scaled"], "82");
        assert_eq!(value["cmi"]["score"]["raw"], "100");
        assert_eq!(value["cmi"]["session_time"], "0");
        assert_eq!(value["cmi"]["total_time"], "0");
        assert!(resource_completion_cmi(101).is_err());
    }

    #[test]
    fn ambiguous_post_mutation_errors_require_human_review() {
        for kind in [
            ProviderErrorKind::Authentication,
            ProviderErrorKind::Network,
            ProviderErrorKind::RateLimited,
            ProviderErrorKind::ProviderUnavailable,
            ProviderErrorKind::ProtocolDrift,
            ProviderErrorKind::InvalidResponse,
        ] {
            let mapped = resource_mutation_error(ProviderError::new(kind, "fixture"));
            assert_eq!(mapped.kind, ProviderErrorKind::HumanRequired);
            assert!(mapped.human_required_reason.is_some());
        }
        let rejected = resource_mutation_error(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "fixture",
        ));
        assert_eq!(rejected.kind, ProviderErrorKind::RemoteChanged);
    }

    #[test]
    fn duration_state_preserves_only_audited_cmi_scalars() {
        let snapshot = parse_cmi_snapshot(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"0.8\"},\"success_status\":\"unknown\"}}"}"#,
        )
        .unwrap();
        let state = PreservedCmiState::try_from_snapshot(&snapshot).unwrap();
        assert_eq!(state.completion_status, "incomplete");
        assert_eq!(state.progress, "0.25");
        assert_eq!(state.score_scaled, "0.8");
        assert_eq!(state.success_status, "unknown");
        assert_eq!(state.session_time, "15");
        assert_eq!(state.total_time, "45");
    }

    #[test]
    fn duration_state_rejects_absent_or_incomplete_cmi() {
        for document in [
            r#"{"ret":0,"comment":"{}"}"#,
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"0.8\"}}}"}"#,
        ] {
            let snapshot = parse_cmi_snapshot(document).unwrap();
            let Err(error) = PreservedCmiState::try_from_snapshot(&snapshot) else {
                panic!("incomplete CMI must not produce mutation state");
            };
            assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        }
    }

    #[test]
    fn response_statuses_are_typed_without_body_access() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "12".parse().unwrap());
        let limited = validate_status(StatusCode::TOO_MANY_REQUESTS, &headers).unwrap_err();
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(12));

        headers.insert(
            LOCATION,
            "https://sso.sflep.com/idsvr/login".parse().unwrap(),
        );
        assert_eq!(
            validate_status(StatusCode::FOUND, &headers)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            validate_status(StatusCode::SERVICE_UNAVAILABLE, &HeaderMap::new())
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProviderUnavailable
        );
        assert!(validate_status(StatusCode::OK, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn login_document_detection_requires_structural_markers() {
        assert!(looks_like_login_document(
            "<form action='/login'><input name='account'><input name='pwd'></form>"
        ));
        assert!(!looks_like_login_document(
            "<script>const message='login account pwd';</script>"
        ));
    }

    #[tokio::test]
    async fn json_reader_distinguishes_a_login_page_from_unexpected_html() {
        let login = fixture_response(
            "text/html",
            "<form action='/login'><input name='account'><input name='pwd'></form>",
        )
        .await;
        let error = read_inventory_response(login, ResponseContent::Json)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);

        let unrelated = fixture_response("text/html", "<html><p>maintenance</p></html>").await;
        let error = read_inventory_response(unrelated, ResponseContent::Json)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    async fn fixture_response(content_type: &'static str, body: &'static str) -> Response {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut request = [0_u8; 1_024];
            let mut received = Vec::new();
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut request).unwrap();
                assert!(count > 0, "fixture client closed before request headers");
                received.extend_from_slice(&request[..count]);
                assert!(received.len() <= 16 * 1_024, "fixture request is bounded");
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let response = Client::new()
            .get(format!("http://{address}/fixture"))
            .send()
            .await
            .unwrap();
        server.join().unwrap();
        response
    }
}
