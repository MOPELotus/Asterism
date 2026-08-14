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
    cmi::{
        UNINITIALIZED_CMI_MARKER, WellearnCmiSnapshot, parse_cmi_snapshot,
        parse_mutation_cmi_baseline,
    },
    course_context::{parse_course_context, parse_course_context_for_id},
    course_inventory::course_id_from_remote,
    runtime_settings::LEGACY_DURATION_REQUEST_INTERVAL_SECONDS,
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
        let mut unit_response = self
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
        if matches!(
            unit_response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            unit_response = self
                .client
                .get(course_units_url(&route)?)
                .header(COOKIE, session.expose_secret())
                .header(REFERER, course_url.as_str())
                .send()
                .await
                .map_err(|error| classify_reqwest_error(&error))?;
        }
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
        self.fetch_cmi_for_route_at_endpoint(session, route, sco_id, ScoEndpoint::QueryUserId)
            .await
    }

    async fn fetch_cmi_for_route_at_endpoint(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        sco_id: &str,
        endpoint: ScoEndpoint,
    ) -> ProviderResult<WellearnCmiDocument> {
        let response = self
            .client
            .post(sco_endpoint_url(route, endpoint)?)
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
        let document = read_inventory_response(response, ResponseContent::Cmi).await?;
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

    async fn send_plain_sco_form_with_referer(
        &self,
        session: &crate::WellearnCookieSession,
        referer: &Url,
        fields: &[(&str, &str)],
    ) -> ProviderResult<WellearnInventoryDocument> {
        let response = self
            .client
            .post(static_url(SCO_URL)?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, referer.as_str())
            .form(fields)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_inventory_response(response, ResponseContent::Json).await
    }

    async fn send_resource_mutation_form(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        task_referer: &Url,
        profile: crate::WellearnResourceMutationProfile,
        fields: &[(&str, &str)],
    ) -> ProviderResult<WellearnInventoryDocument> {
        match profile {
            crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer => {
                self.send_sco_form(session, route, fields).await
            }
            crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer => {
                self.send_plain_sco_form_with_referer(session, task_referer, fields)
                    .await
            }
        }
    }

    async fn send_sco_form_at_endpoint(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        endpoint: ScoEndpoint,
        fields: &[(&str, &str)],
    ) -> ProviderResult<WellearnInventoryDocument> {
        let response = self
            .client
            .post(sco_endpoint_url(route, endpoint)?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, STUDY_COURSE_REFERER)
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
        plan: crate::WellearnResourceExecutionPlan,
    ) -> ProviderResult<WellearnResourceExecutionDocuments> {
        plan.validate()?;
        let crate::WellearnResourceExecutionPlan {
            score_percent,
            sequence,
            time_mode,
            cmi_format,
            write_mode,
            mutation_profile,
        } = plan;
        if score_percent > 100 {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn resource transport received an out-of-range score",
            ));
        }
        let endpoint = resource_endpoint(mutation_profile);
        let (mut session, mut renewed) = self.session_for_operation(context).await?;
        let (route, before) = match self
            .read_duration_baseline(&session, course_id, sco_id, endpoint)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                renewed = true;
                self.read_duration_baseline(&session, course_id, sco_id, endpoint)
                    .await?
            }
            result => result?,
        };
        let baseline = parse_mutation_cmi_baseline(before.as_str())?;
        let expected_score = sequence.final_score(score_percent).to_string();
        if baseline.as_ref().is_some_and(|baseline| {
            baseline.remote_state() == asterism_domain::RemoteState::Completed
                && baseline.percent() == Some(100)
                && baseline.score_scaled_raw() == Some(expected_score.as_str())
        }) {
            return Ok(WellearnResourceExecutionDocuments::already_completed(
                before,
            ));
        }

        let referer = study_course_url(&route, sco_id)?;
        let cmi = if resource_requires_set(write_mode) {
            Some(resource_completion_cmi(
                score_percent,
                time_mode,
                cmi_format,
                baseline.as_ref(),
            )?)
        } else {
            None
        };
        let start_fields = sco_start_fields(
            &route,
            sco_id,
            match mutation_profile {
                crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer => {
                    ScoStartPayload::FullRoute
                }
                crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer => {
                    ScoStartPayload::MinimalIdentity
                }
            },
        );
        let start = self
            .send_resource_mutation_form(
                &session,
                &route,
                &referer,
                mutation_profile,
                &start_fields,
            )
            .await
            .map_err(resource_mutation_error)?;
        let start_accepted = mutation_accepted(start.as_str(), MutationResponseKind::StrictSuccess)
            .map_err(resource_mutation_error)?;

        let set_accepted = if let Some(cmi) = cmi.as_ref() {
            let set = self
                .send_resource_mutation_form(
                    &session,
                    &route,
                    &referer,
                    mutation_profile,
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
            Some(
                mutation_accepted(set.as_str(), MutationResponseKind::StrictSuccess)
                    .map_err(resource_mutation_error)?,
            )
        } else {
            None
        };

        let mut save_acceptances = Vec::new();
        for save_score in resource_save_scores(sequence, score_percent) {
            let save_score = save_score.to_string();
            save_acceptances.push(
                self.save_resource_completion(
                    &session,
                    &route,
                    &referer,
                    mutation_profile,
                    sco_id,
                    &save_score,
                )
                .await
                .map_err(resource_mutation_error)?,
            );
        }

        let after = match self
            .fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
            .await
        {
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
                self.fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
                    .await
                    .map_err(resource_mutation_error)?
            }
            result => result.map_err(resource_mutation_error)?,
        };
        Ok(WellearnResourceExecutionDocuments::submitted(
            before,
            after,
            true,
            start_accepted,
            set_accepted,
            save_acceptances,
        ))
    }

    async fn verify_resource(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        mutation_profile: crate::WellearnResourceMutationProfile,
    ) -> ProviderResult<WellearnCmiDocument> {
        let (mut session, renewed) = self.session_for_operation(context).await?;
        let endpoint = resource_endpoint(mutation_profile);
        let first = async {
            let route = self.resolve_course_route(&session, course_id).await?;
            self.fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
                .await
        }
        .await;
        match first {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                let route = self.resolve_course_route(&session, course_id).await?;
                self.fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
                    .await
            }
            result => result,
        }
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
        plan: crate::WellearnDurationReportPlan,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<WellearnDurationReportDocuments> {
        plan.validate()?;
        let crate::WellearnDurationReportPlan {
            duration_seconds,
            heartbeat_interval_seconds,
            protocol_mode,
        } = plan;
        let endpoint = duration_endpoint(protocol_mode);
        let (mut session, mut renewed) = self.session_for_operation(context).await?;
        if protocol_mode == crate::WellearnDurationProtocolMode::PreserveFresh {
            tokio::time::sleep(Duration::from_secs(
                LEGACY_DURATION_REQUEST_INTERVAL_SECONDS,
            ))
            .await;
        }
        let (mut route, mut before) = match self
            .read_duration_baseline(&session, course_id, sco_id, endpoint)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                renewed = true;
                self.read_duration_baseline(&session, course_id, sco_id, endpoint)
                    .await?
            }
            result => result?,
        };
        let mut snapshot = parse_mutation_cmi_baseline(before.as_str())?;
        let mut started = false;
        let mut start_accepted = None;
        let must_start = duration_requires_start(protocol_mode, snapshot.is_none());
        if must_start {
            events
                .log(duration_log(
                    "duration_start",
                    "远端尚无 CMI，正在启动学习会话",
                    None,
                ))
                .await?;
            if protocol_mode == crate::WellearnDurationProtocolMode::PreserveFresh {
                tokio::time::sleep(Duration::from_secs(
                    LEGACY_DURATION_REQUEST_INTERVAL_SECONDS,
                ))
                .await;
            }
            let start_fields = sco_start_fields(
                &route,
                sco_id,
                if protocol_mode == crate::WellearnDurationProtocolMode::ClientCounter {
                    ScoStartPayload::FullRoute
                } else {
                    ScoStartPayload::MinimalIdentity
                },
            );
            let start = self
                .send_sco_form_at_endpoint(&session, &route, endpoint, &start_fields)
                .await?;
            let accepted = mutation_accepted(start.as_str(), MutationResponseKind::StrictSuccess)?;
            if protocol_mode == crate::WellearnDurationProtocolMode::ClientCounter && !accepted {
                return Err(ProviderError::new(
                    ProviderErrorKind::RemoteChanged,
                    "WELearn current-donor duration start was not accepted",
                ));
            }
            start_accepted = Some(accepted);
            started = true;
            before = self
                .fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
                .await?;
            snapshot = Some(parse_cmi_snapshot(before.as_str())?);
        }

        let snapshot = snapshot.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn duration baseline remained uninitialized after start",
            )
        })?;

        if protocol_mode == crate::WellearnDurationProtocolMode::PreserveFresh
            && snapshot.success_status_raw() != Some("unknown")
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn historical duration donor requires unknown success status",
            ));
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
                    "duration_protocol_mode": protocol_mode.as_str(),
                })),
            ))
            .await?;

        let mut heartbeat_count = 0_u32;
        let mut heartbeat_accepted = 0_u32;
        let mut heartbeat_rejected = 0_u32;
        let mut final_accepted = None;
        match protocol_mode {
            crate::WellearnDurationProtocolMode::PreserveFresh => {
                tokio::time::sleep(Duration::from_secs(
                    LEGACY_DURATION_REQUEST_INTERVAL_SECONDS,
                ))
                .await;
                let accepted = self
                    .keep_duration_preserved(&session, &route, endpoint, sco_id, &state)
                    .await?;
                record_duration_receipt(accepted, &mut heartbeat_accepted, &mut heartbeat_rejected);
                heartbeat_count = heartbeat_count.saturating_add(1);
                let (complete_intervals, trailing_seconds) =
                    duration_heartbeat_plan(duration_seconds, heartbeat_interval_seconds);
                for completed in 1..=complete_intervals {
                    tokio::time::sleep(Duration::from_secs(heartbeat_interval_seconds)).await;
                    let accepted = self
                        .keep_duration_preserved(&session, &route, endpoint, sco_id, &state)
                        .await?;
                    record_duration_receipt(
                        accepted,
                        &mut heartbeat_accepted,
                        &mut heartbeat_rejected,
                    );
                    heartbeat_count = heartbeat_count.saturating_add(1);
                    report_duration_heartbeat(
                        events,
                        completed.saturating_mul(heartbeat_interval_seconds),
                        duration_seconds,
                    )
                    .await?;
                }
                if trailing_seconds != 0 {
                    tokio::time::sleep(Duration::from_secs(trailing_seconds)).await;
                    report_duration_tail(events).await?;
                }
                tokio::time::sleep(Duration::from_secs(
                    LEGACY_DURATION_REQUEST_INTERVAL_SECONDS,
                ))
                .await;
                final_accepted = Some(
                    self.finalize_duration_preserved(&session, &route, endpoint, sco_id, &state)
                        .await?,
                );
            }
            crate::WellearnDurationProtocolMode::ClientCounter => {
                for elapsed in 0..duration_seconds {
                    let accepted = self
                        .keep_duration_counter(&session, &route, endpoint, sco_id, elapsed)
                        .await?;
                    record_duration_receipt(
                        accepted,
                        &mut heartbeat_accepted,
                        &mut heartbeat_rejected,
                    );
                    heartbeat_count = heartbeat_count.saturating_add(1);
                    if !accepted {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    report_duration_heartbeat(events, elapsed.saturating_add(1), duration_seconds)
                        .await?;
                }
            }
            crate::WellearnDurationProtocolMode::ImplicitServer => {
                let (complete_intervals, trailing_seconds) =
                    duration_heartbeat_plan(duration_seconds, heartbeat_interval_seconds);
                for completed in 1..=complete_intervals {
                    tokio::time::sleep(Duration::from_secs(heartbeat_interval_seconds)).await;
                    let accepted = self
                        .keep_duration_implicit(&session, &route, endpoint, sco_id)
                        .await?;
                    record_duration_receipt(
                        accepted,
                        &mut heartbeat_accepted,
                        &mut heartbeat_rejected,
                    );
                    heartbeat_count = heartbeat_count.saturating_add(1);
                    report_duration_heartbeat(
                        events,
                        completed.saturating_mul(heartbeat_interval_seconds),
                        duration_seconds,
                    )
                    .await?;
                }
                if trailing_seconds != 0 {
                    tokio::time::sleep(Duration::from_secs(trailing_seconds)).await;
                    report_duration_tail(events).await?;
                }
                final_accepted = Some(
                    self.finalize_duration_implicit(&session, &route, endpoint, sco_id)
                        .await?,
                );
            }
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
        let after = match self
            .fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                session = self.sessions.renew_session(context).await?;
                route = self.resolve_course_route(&session, course_id).await?;
                self.fetch_cmi_for_route_at_endpoint(&session, &route, sco_id, endpoint)
                    .await?
            }
            result => result?,
        };
        Ok(WellearnDurationReportDocuments::with_receipts(
            before,
            after,
            started,
            heartbeat_count,
            crate::duration_report::WellearnDurationReportReceipts {
                start_accepted,
                heartbeat_accepted,
                heartbeat_rejected,
                final_accepted,
            },
        ))
    }
}

const fn duration_heartbeat_plan(
    duration_seconds: u64,
    heartbeat_interval_seconds: u64,
) -> (u64, u64) {
    (
        duration_seconds / heartbeat_interval_seconds,
        duration_seconds % heartbeat_interval_seconds,
    )
}

fn record_duration_receipt(accepted: bool, accepted_count: &mut u32, rejected_count: &mut u32) {
    if accepted {
        *accepted_count = accepted_count.saturating_add(1);
    } else {
        *rejected_count = rejected_count.saturating_add(1);
    }
}

const fn duration_requires_start(
    mode: crate::WellearnDurationProtocolMode,
    explicitly_uninitialized: bool,
) -> bool {
    match mode {
        crate::WellearnDurationProtocolMode::PreserveFresh => explicitly_uninitialized,
        crate::WellearnDurationProtocolMode::ClientCounter
        | crate::WellearnDurationProtocolMode::ImplicitServer => true,
    }
}

fn duration_counter_fields(elapsed_seconds: u64) -> (String, String) {
    let elapsed = elapsed_seconds.to_string();
    (elapsed.clone(), elapsed)
}

async fn report_duration_heartbeat(
    events: &(dyn ExecutionEventSink + Send + Sync),
    elapsed: u64,
    duration_seconds: u64,
) -> ProviderResult<()> {
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
        .await
}

async fn report_duration_tail(
    events: &(dyn ExecutionEventSink + Send + Sync),
) -> ProviderResult<()> {
    events
        .report(ProviderProgress {
            percent: Some(90),
            stage: "duration_execute".to_owned(),
            status_text: Some("学习时长尾段已完成".to_owned()),
            completed_items: Some(0),
            total_items: Some(1),
        })
        .await
}

impl NativeWellearnInventoryTransport {
    async fn save_resource_completion(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        referer: &Url,
        mutation_profile: crate::WellearnResourceMutationProfile,
        sco_id: &str,
        score: &str,
    ) -> ProviderResult<bool> {
        let response = self
            .send_resource_mutation_form(
                session,
                route,
                referer,
                mutation_profile,
                &[
                    ("action", "savescoinfo160928"),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("uid", route.user_id()),
                    ("progress", "100"),
                    ("crate", score),
                    ("status", "unknown"),
                    ("cstatus", "completed"),
                    ("trycount", "0"),
                ],
            )
            .await?;
        mutation_accepted(response.as_str(), MutationResponseKind::StrictSuccess)
    }

    async fn read_duration_baseline(
        &self,
        session: &crate::WellearnCookieSession,
        course_id: &str,
        sco_id: &str,
        endpoint: ScoEndpoint,
    ) -> ProviderResult<(crate::WellearnCourseContext, WellearnCmiDocument)> {
        let route = self.resolve_course_route(session, course_id).await?;
        let document = self
            .fetch_cmi_for_route_at_endpoint(session, &route, sco_id, endpoint)
            .await?;
        Ok((route, document))
    }

    async fn keep_duration_preserved(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        endpoint: ScoEndpoint,
        sco_id: &str,
        state: &PreservedCmiState,
    ) -> ProviderResult<bool> {
        let response = self
            .send_sco_form_at_endpoint(
                session,
                route,
                endpoint,
                &[
                    ("action", "keepsco_with_getticket_with_updatecmitime"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("session_time", &state.session_time),
                    ("total_time", &state.total_time),
                ],
            )
            .await?;
        mutation_accepted(response.as_str(), MutationResponseKind::Heartbeat)
    }

    async fn keep_duration_counter(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        endpoint: ScoEndpoint,
        sco_id: &str,
        elapsed_seconds: u64,
    ) -> ProviderResult<bool> {
        let (session_time, total_time) = duration_counter_fields(elapsed_seconds);
        let response = self
            .send_sco_form_at_endpoint(
                session,
                route,
                endpoint,
                &[
                    ("action", "keepsco_with_getticket_with_updatecmitime"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("session_time", session_time.as_str()),
                    ("total_time", total_time.as_str()),
                    ("timelimitsec", "0"),
                    ("endcaltime", "false"),
                ],
            )
            .await?;
        mutation_accepted(response.as_str(), MutationResponseKind::Heartbeat)
    }

    async fn keep_duration_implicit(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        endpoint: ScoEndpoint,
        sco_id: &str,
    ) -> ProviderResult<bool> {
        let response = self
            .send_sco_form_at_endpoint(
                session,
                route,
                endpoint,
                &[
                    ("action", "keepsco_with_getticket_with_updatecmitime"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                ],
            )
            .await?;
        mutation_accepted(response.as_str(), MutationResponseKind::Heartbeat)
    }

    async fn finalize_duration_preserved(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        endpoint: ScoEndpoint,
        sco_id: &str,
        state: &PreservedCmiState,
    ) -> ProviderResult<bool> {
        let response = self
            .send_sco_form_at_endpoint(
                session,
                route,
                endpoint,
                &[
                    ("action", "savescoinfo160928"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                    ("progress", &state.progress),
                    ("crate", &state.score_scaled),
                    ("status", "unknown"),
                    ("cstatus", &state.completion_status),
                    ("trycount", "0"),
                ],
            )
            .await?;
        mutation_accepted(response.as_str(), MutationResponseKind::StrictSuccess)
    }

    async fn finalize_duration_implicit(
        &self,
        session: &crate::WellearnCookieSession,
        route: &crate::WellearnCourseContext,
        endpoint: ScoEndpoint,
        sco_id: &str,
    ) -> ProviderResult<bool> {
        let response = self
            .send_sco_form_at_endpoint(
                session,
                route,
                endpoint,
                &[
                    ("action", "savescoinfo160928"),
                    ("uid", route.user_id()),
                    ("cid", route.course_id()),
                    ("scoid", sco_id),
                ],
            )
            .await?;
        mutation_accepted(response.as_str(), MutationResponseKind::StrictSuccess)
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

fn mutation_accepted(document: &str, kind: MutationResponseKind) -> ProviderResult<bool> {
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
    Ok(result == 0 || matches!(kind, MutationResponseKind::Heartbeat) && result == 1)
}

fn resource_completion_cmi(
    score_percent: u8,
    time_mode: crate::WellearnResourceCompletionTimeMode,
    cmi_format: crate::WellearnResourceCompletionCmiFormat,
    baseline: Option<&WellearnCmiSnapshot>,
) -> ProviderResult<Zeroizing<String>> {
    if score_percent > 100 {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn completion CMI received an out-of-range score",
        ));
    }
    let score = score_percent.to_string();
    let (session_time, total_time) = match time_mode {
        crate::WellearnResourceCompletionTimeMode::Auto => {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn resource transport received an unresolved completion time mode",
            ));
        }
        crate::WellearnResourceCompletionTimeMode::ZeroTime => ("0", "0"),
        crate::WellearnResourceCompletionTimeMode::PreserveFreshTime => {
            let baseline = baseline.ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn cannot preserve completion time from an uninitialized CMI",
                )
            })?;
            if !baseline.cmi_present() {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn cannot preserve completion time without fresh CMI",
                ));
            }
            (
                baseline.session_time_raw().ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "WELearn completion baseline has no session time",
                    )
                })?,
                baseline.total_time_raw().ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "WELearn completion baseline has no total time",
                    )
                })?,
            )
        }
    };
    let mut document = serde_json::to_string(&serde_json::json!({
        "cmi": {
            "completion_status": "completed",
            "interactions": [],
            "launch_data": "",
            "progress_measure": "1",
            "score": {"scaled": score, "raw": "100"},
            "session_time": session_time,
            "success_status": "unknown",
            "total_time": total_time,
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
    .map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn completion CMI serialization failed",
        )
    })?;
    match cmi_format {
        crate::WellearnResourceCompletionCmiFormat::Json => {}
        crate::WellearnResourceCompletionCmiFormat::InteractionInfoSuffix => {
            document.push_str("[INTERACTIONINFO]");
        }
    }
    Ok(Zeroizing::new(document))
}

fn resource_save_scores(
    sequence: crate::WellearnResourceCompletionSequence,
    selected_score: u8,
) -> Vec<u8> {
    match sequence {
        crate::WellearnResourceCompletionSequence::SelectedScore => vec![selected_score],
        crate::WellearnResourceCompletionSequence::CurrentDonorDualSave100 => {
            vec![selected_score, 100]
        }
    }
}

const fn resource_requires_set(mode: crate::WellearnResourceCompletionWriteMode) -> bool {
    match mode {
        crate::WellearnResourceCompletionWriteMode::SetThenSave => true,
        crate::WellearnResourceCompletionWriteMode::SaveOnly => false,
    }
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum ResponseContent {
    Html,
    Json,
    Cmi,
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
    if let Err(error) = content_type_result
        && (expected_content != ResponseContent::Cmi
            || !document.contains(UNINITIALIZED_CMI_MARKER))
    {
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
        ResponseContent::Json | ResponseContent::Cmi => {
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

fn course_units_url(route: &crate::WellearnCourseContext) -> ProviderResult<Url> {
    let mut url = static_url(STUDY_STAT_URL)?;
    url.query_pairs_mut()
        .append_pair("action", "courseunits")
        .append_pair("cid", route.course_id())
        .append_pair("uid", route.user_id());
    Ok(url)
}

fn sco_url(user_id: &str) -> ProviderResult<Url> {
    let mut url = static_url(SCO_URL)?;
    url.query_pairs_mut().append_pair("uid", user_id);
    Ok(url)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScoEndpoint {
    QueryUserId,
    Plain,
}

fn sco_endpoint_url(
    route: &crate::WellearnCourseContext,
    endpoint: ScoEndpoint,
) -> ProviderResult<Url> {
    match endpoint {
        ScoEndpoint::QueryUserId => sco_url(route.user_id()),
        ScoEndpoint::Plain => static_url(SCO_URL),
    }
}

const fn resource_endpoint(profile: crate::WellearnResourceMutationProfile) -> ScoEndpoint {
    match profile {
        crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer => {
            ScoEndpoint::QueryUserId
        }
        crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer => ScoEndpoint::Plain,
    }
}

const fn duration_endpoint(mode: crate::WellearnDurationProtocolMode) -> ScoEndpoint {
    match mode {
        crate::WellearnDurationProtocolMode::ClientCounter => ScoEndpoint::QueryUserId,
        crate::WellearnDurationProtocolMode::PreserveFresh
        | crate::WellearnDurationProtocolMode::ImplicitServer => ScoEndpoint::Plain,
    }
}

fn study_course_url(route: &crate::WellearnCourseContext, sco_id: &str) -> ProviderResult<Url> {
    let mut url = static_url(STUDY_COURSE_REFERER)?;
    url.query_pairs_mut()
        .append_pair("cid", route.course_id())
        .append_pair("classid", route.class_id())
        .append_pair("sco", sco_id);
    Ok(url)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScoStartPayload {
    FullRoute,
    MinimalIdentity,
}

fn sco_start_fields<'a>(
    route: &'a crate::WellearnCourseContext,
    sco_id: &'a str,
    payload: ScoStartPayload,
) -> Vec<(&'static str, &'a str)> {
    let mut fields = vec![
        ("action", "startsco160928"),
        ("uid", route.user_id()),
        ("cid", route.course_id()),
        ("scoid", sco_id),
    ];
    if payload == ScoStartPayload::FullRoute {
        fields.push(("classid", route.class_id()));
        fields.push(("tid", "-1"));
    }
    fields
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
    let login_form = lowercase.contains("<form")
        && lowercase.contains("login")
        && lowercase.contains("account")
        && lowercase.contains("pwd");
    let prelogin_script = lowercase.contains("<script")
        && lowercase.contains("/user/prelogin.aspx")
        && lowercase.contains("loginret=")
        && lowercase.contains("top.loginsso()");
    login_form || prelogin_script
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
            course_units_url(&context).unwrap().query(),
            Some("action=courseunits&cid=1001&uid=7001")
        );
        assert_eq!(
            sco_url("user value").unwrap().as_str(),
            "https://welearn.sflep.com/Ajax/SCO.aspx?uid=user+value"
        );
        assert_eq!(
            sco_endpoint_url(&context, ScoEndpoint::Plain)
                .unwrap()
                .as_str(),
            "https://welearn.sflep.com/Ajax/SCO.aspx"
        );
        assert_eq!(
            sco_endpoint_url(&context, ScoEndpoint::QueryUserId)
                .unwrap()
                .as_str(),
            "https://welearn.sflep.com/Ajax/SCO.aspx?uid=7001"
        );
        assert_eq!(
            resource_endpoint(crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer),
            ScoEndpoint::Plain
        );
        assert_eq!(
            resource_endpoint(crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer),
            ScoEndpoint::QueryUserId
        );
        assert_eq!(
            duration_endpoint(crate::WellearnDurationProtocolMode::PreserveFresh),
            ScoEndpoint::Plain
        );
        assert_eq!(
            duration_endpoint(crate::WellearnDurationProtocolMode::ImplicitServer),
            ScoEndpoint::Plain
        );
        assert_eq!(
            duration_endpoint(crate::WellearnDurationProtocolMode::ClientCounter),
            ScoEndpoint::QueryUserId
        );
        assert_eq!(
            sco_start_fields(&context, "301", ScoStartPayload::MinimalIdentity),
            vec![
                ("action", "startsco160928"),
                ("uid", "7001"),
                ("cid", "1001"),
                ("scoid", "301"),
            ]
        );
        assert_eq!(
            sco_start_fields(&context, "301", ScoStartPayload::FullRoute),
            vec![
                ("action", "startsco160928"),
                ("uid", "7001"),
                ("cid", "1001"),
                ("scoid", "301"),
                ("classid", "class-8001"),
                ("tid", "-1"),
            ]
        );
    }

    #[test]
    fn duration_mutation_results_are_strictly_classified() {
        assert!(mutation_accepted(r#"{"ret":0}"#, MutationResponseKind::StrictSuccess).unwrap());
        assert!(mutation_accepted(r#"{"ret":1}"#, MutationResponseKind::Heartbeat).unwrap());
        assert!(!mutation_accepted(r#"{"ret":1}"#, MutationResponseKind::StrictSuccess).unwrap());
        assert!(!mutation_accepted(r#"{"ret":2}"#, MutationResponseKind::Heartbeat).unwrap());
        for document in [r#"{"ret":"0"}"#, r#"{"ok":true}"#, "not-json"] {
            assert!(mutation_accepted(document, MutationResponseKind::StrictSuccess).is_err());
        }
        assert!(!mutation_accepted(r#"{"ret":7}"#, MutationResponseKind::StrictSuccess).unwrap());

        let (mut accepted, mut rejected) = (0, 0);
        record_duration_receipt(true, &mut accepted, &mut rejected);
        record_duration_receipt(false, &mut accepted, &mut rejected);
        assert_eq!((accepted, rejected), (1, 1));
    }

    #[test]
    fn duration_heartbeat_plan_does_not_invent_a_trailing_keep() {
        assert_eq!(duration_heartbeat_plan(10, 60), (0, 10));
        assert_eq!(duration_heartbeat_plan(60, 60), (1, 0));
        assert_eq!(duration_heartbeat_plan(61, 60), (1, 1));
        assert_eq!(duration_heartbeat_plan(120, 30), (4, 0));
    }

    #[test]
    fn donor_duration_wire_plans_remain_distinct() {
        use crate::WellearnDurationProtocolMode::{ClientCounter, ImplicitServer, PreserveFresh};

        assert!(!duration_requires_start(PreserveFresh, false));
        assert!(duration_requires_start(PreserveFresh, true));
        assert!(duration_requires_start(ClientCounter, true));
        assert!(duration_requires_start(ImplicitServer, true));

        assert_eq!(
            (0..3).map(duration_counter_fields).collect::<Vec<_>>(),
            [
                ("0".to_owned(), "0".to_owned()),
                ("1".to_owned(), "1".to_owned()),
                ("2".to_owned(), "2".to_owned()),
            ]
        );
        assert_eq!(duration_heartbeat_plan(61, 60), (1, 1));
    }

    #[test]
    fn duration_baseline_recognizes_only_the_donor_uninitialized_marker() {
        assert!(
            parse_mutation_cmi_baseline("学习数据不正确，请先开始学习")
                .unwrap()
                .is_none()
        );
        assert!(parse_mutation_cmi_baseline("not-json").is_err());

        let initialized = parse_mutation_cmi_baseline(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0\",\"session_time\":\"0\",\"total_time\":\"0\",\"score\":{\"scaled\":\"0\"},\"success_status\":\"unknown\"}}"}"#,
        )
        .unwrap();
        assert!(initialized.is_some());

        let valid_no_cmi = parse_mutation_cmi_baseline(r#"{"ret":0,"comment":"{}"}"#)
            .unwrap()
            .unwrap();
        assert!(!valid_no_cmi.cmi_present());
        assert!(!duration_requires_start(
            crate::WellearnDurationProtocolMode::PreserveFresh,
            false,
        ));
    }

    #[test]
    fn resource_completion_cmi_is_bounded_and_matches_the_current_donor_shape() {
        let baseline = parse_cmi_snapshot(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"20\"},\"success_status\":\"unknown\"}}"}"#,
        )
        .unwrap();
        let document = resource_completion_cmi(
            82,
            crate::WellearnResourceCompletionTimeMode::ZeroTime,
            crate::WellearnResourceCompletionCmiFormat::Json,
            Some(&baseline),
        )
        .unwrap();
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
        assert!(
            resource_completion_cmi(
                82,
                crate::WellearnResourceCompletionTimeMode::ZeroTime,
                crate::WellearnResourceCompletionCmiFormat::Json,
                None,
            )
            .is_ok()
        );
        assert!(
            resource_completion_cmi(
                101,
                crate::WellearnResourceCompletionTimeMode::ZeroTime,
                crate::WellearnResourceCompletionCmiFormat::Json,
                Some(&baseline),
            )
            .is_err()
        );
    }

    #[test]
    fn resource_completion_can_preserve_fresh_duration_times() {
        let baseline = parse_cmi_snapshot(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"20\"},\"success_status\":\"unknown\"}}"}"#,
        )
        .unwrap();
        let document = resource_completion_cmi(
            100,
            crate::WellearnResourceCompletionTimeMode::PreserveFreshTime,
            crate::WellearnResourceCompletionCmiFormat::Json,
            Some(&baseline),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(document.as_str()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/welearn/cmi/resource-completion-cmi-preserve-time.expected.json"
        ))
        .unwrap();
        assert_eq!(value, expected);
        assert_eq!(value["cmi"]["session_time"], "15");
        assert_eq!(value["cmi"]["total_time"], "45");

        let missing = parse_cmi_snapshot(r#"{"ret":0,"comment":"{}"}"#).unwrap();
        let error = resource_completion_cmi(
            100,
            crate::WellearnResourceCompletionTimeMode::PreserveFreshTime,
            crate::WellearnResourceCompletionCmiFormat::Json,
            Some(&missing),
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        let error = resource_completion_cmi(
            100,
            crate::WellearnResourceCompletionTimeMode::PreserveFreshTime,
            crate::WellearnResourceCompletionCmiFormat::Json,
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    #[test]
    fn resource_completion_cmi_supports_the_interaction_info_envelope() {
        let baseline = parse_cmi_snapshot(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"20\"},\"success_status\":\"unknown\"}}"}"#,
        )
        .unwrap();
        let document = resource_completion_cmi(
            82,
            crate::WellearnResourceCompletionTimeMode::ZeroTime,
            crate::WellearnResourceCompletionCmiFormat::InteractionInfoSuffix,
            Some(&baseline),
        )
        .unwrap();
        let json = document
            .strip_suffix("[INTERACTIONINFO]")
            .expect("audited literal suffix");
        let actual: serde_json::Value = serde_json::from_str(json).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/welearn/cmi/resource-completion-cmi.expected.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn resource_save_plan_covers_both_donor_sequences_exactly() {
        assert_eq!(
            resource_save_scores(crate::WellearnResourceCompletionSequence::SelectedScore, 82,),
            [82]
        );
        assert_eq!(
            resource_save_scores(
                crate::WellearnResourceCompletionSequence::CurrentDonorDualSave100,
                82,
            ),
            [82, 100]
        );
        assert_eq!(
            resource_save_scores(
                crate::WellearnResourceCompletionSequence::CurrentDonorDualSave100,
                100,
            ),
            [100, 100]
        );
        assert!(resource_requires_set(
            crate::WellearnResourceCompletionWriteMode::SetThenSave
        ));
        assert!(!resource_requires_set(
            crate::WellearnResourceCompletionWriteMode::SaveOnly
        ));
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
        assert!(looks_like_login_document(include_str!(
            "../../../fixtures/providers/welearn/auth/anonymous-course-list-login.html"
        )));
        assert!(!looks_like_login_document(
            "<script>const message='login account pwd';</script>"
        ));
        assert!(!looks_like_login_document(
            "<script>top.loginSSO();</script>"
        ));
    }

    #[tokio::test]
    async fn json_reader_distinguishes_a_login_page_from_unexpected_html() {
        let login = fixture_response(
            "text/html",
            include_str!(
                "../../../fixtures/providers/welearn/auth/anonymous-course-list-login.html"
            ),
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

        let uninitialized = fixture_response("text/html", "学习数据不正确，请先开始学习").await;
        let document = read_inventory_response(uninitialized, ResponseContent::Cmi)
            .await
            .unwrap();
        assert!(document.as_str().contains(UNINITIALIZED_CMI_MARKER));

        let login = fixture_response(
            "text/html",
            include_str!(
                "../../../fixtures/providers/welearn/auth/anonymous-course-list-login.html"
            ),
        )
        .await;
        let error = read_inventory_response(login, ResponseContent::Cmi)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);
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
