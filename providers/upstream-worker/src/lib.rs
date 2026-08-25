//! Thin read-only Provider capabilities backed by an external 0.0.1 worker.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, RwLock},
};

use asterism_domain::{
    AnswerCandidate, AnswerConfidence, AnswerPair, AnswerSource, AssessmentClass, AuthMethod,
    LogLevel, NormalizedAnswer, Question, QuestionAttachment, QuestionAttachmentKind,
    QuestionGroup, QuestionGroupChild, QuestionGroupId, QuestionId, QuestionKind, QuestionOption,
    RemoteState, SessionKind, SourceType, TaskCapability, TaskId, WaitingUserState,
};
use asterism_provider_api::{
    AnswerHistoryCursor, AnswerHistoryHarvestCapability, AnswerHistoryPage,
    AnswerHistoryQuestionEvidence, AnswerHistoryTaskRef, AnswerHistoryTaskRequest,
    AnswerResolveCapability, AuthChallenge, AuthenticationCapability, CourseInventoryCapability,
    CredentialReplacement, CredentialValidation, DurationReadCapability, ExecutionEventSink,
    ExecutionInvocationPreparationRequest, ExecutionOutcome, ExecutionRequest,
    ExternalOauthAuthorization, ExternalOauthCallbackBinding, PreparedProviderExecutionInvocation,
    ProviderAnswerHistoryTaskEvidence, ProviderAuthContext, ProviderCapability, ProviderContext,
    ProviderEntry, ProviderError, ProviderErrorKind, ProviderExecutionLog,
    ProviderExecutionPlanArtifact, ProviderExecutionPrivateInput, ProviderIdentity,
    ProviderMetadata, ProviderProgress, ProviderResult, ProviderRouteContext,
    ProviderRuntimeSettingsSchema, ProviderSettingDefinition, ProviderSettingKind,
    ProviderSettingScope, ProviderSettingValue, ProviderStructuredQuestionParseSet,
    QuestionInventoryCapability, QuestionParseCapability, RemoteCourse, RemoteDuration,
    RemoteQuestionRef, RemoteTask, RemoteTaskDetail, SessionStatus, TaskDetailCapability,
    TaskExecutionCapability, TaskInventoryCapability, VerificationLevel,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, CredentialField, ProviderCredentialRenewal,
    ProviderCredentialRenewer, ProviderCredentialResolution, ProviderCredentialResolver,
    ResolvedProviderCredential, SecretPurpose, SecretStoreError, SecretString, SecretValue,
};
use asterism_uai_worker_client::{UaiWorkerClient, UaiWorkerClientError};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CHAOXING_ANSWERS_INPUT_TYPE: &str = "chaoxing.worker.answers.v1";
const CHAOXING_ANSWERS_PLAN_TYPE: &str = "chaoxing.worker.answers-plan.v1";
const CIDAREN_ANSWERS_INPUT_TYPE: &str = "cidaren.worker.answers.v1";
const CIDAREN_ANSWERS_PLAN_TYPE: &str = "cidaren.worker.answers-plan.v1";
const UAI_GENERATED_TEXT_INPUT_TYPE: &str = "uai.worker.generated-text.v1";
const UAI_GENERATED_TEXT_PLAN_TYPE: &str = "uai.worker.generated-text-plan.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerAuthProfile {
    PasswordAndCookie,
    Password,
    ImportedToken,
    ExternalOauthComposite,
}

#[derive(Clone)]
pub struct UpstreamWorkerProvider {
    metadata: ProviderMetadata,
    client: UaiWorkerClient,
    credentials: Arc<dyn ProviderCredentialResolver>,
    credential_renewer: Option<Arc<dyn ProviderCredentialRenewer>>,
    auth_profile: WorkerAuthProfile,
    authentication_delegate: Option<Arc<dyn AuthenticationCapability>>,
    task_routes: Arc<RwLock<HashMap<(String, String), Value>>>,
    history_questions: Arc<RwLock<HashMap<(String, String), Vec<Value>>>>,
    request_sessions: Arc<RwLock<HashMap<(String, String), Value>>>,
}

impl std::fmt::Debug for UpstreamWorkerProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamWorkerProvider")
            .field("metadata", &self.metadata)
            .field("client", &self.client)
            .field("credentials", &"configured")
            .field("credential_renewer", &self.credential_renewer.is_some())
            .field("auth_profile", &self.auth_profile)
            .field(
                "authentication_delegate",
                &self.authentication_delegate.is_some(),
            )
            .field("task_routes", &"in-memory Provider-private cache")
            .field("request_sessions", &"bounded in-request session cache")
            .finish()
    }
}

impl UpstreamWorkerProvider {
    /// Builds only the read-only slots proven by the current worker contract.
    pub fn entry(
        provider_id: asterism_domain::ProviderId,
        display_name: impl Into<String>,
        client: UaiWorkerClient,
        credentials: Arc<dyn ProviderCredentialResolver>,
        auth_profile: WorkerAuthProfile,
    ) -> ProviderEntry {
        Self::entry_inner(
            provider_id,
            display_name,
            client,
            credentials,
            None,
            auth_profile,
            None,
        )
    }

    /// Builds a worker-backed Provider which can atomically persist a session
    /// refreshed from the account's saved username and password.
    pub fn entry_with_renewal(
        provider_id: asterism_domain::ProviderId,
        display_name: impl Into<String>,
        client: UaiWorkerClient,
        credentials: Arc<dyn ProviderCredentialResolver>,
        credential_renewer: Arc<dyn ProviderCredentialRenewer>,
        auth_profile: WorkerAuthProfile,
    ) -> ProviderEntry {
        Self::entry_inner(
            provider_id,
            display_name,
            client,
            credentials,
            Some(credential_renewer),
            auth_profile,
            None,
        )
    }

    /// Builds a worker-backed Provider while delegating the user-facing login
    /// flow to an existing Provider authentication implementation.
    pub fn entry_with_authentication(
        provider_id: asterism_domain::ProviderId,
        display_name: impl Into<String>,
        client: UaiWorkerClient,
        credentials: Arc<dyn ProviderCredentialResolver>,
        auth_profile: WorkerAuthProfile,
        authentication_delegate: Arc<dyn AuthenticationCapability>,
    ) -> ProviderEntry {
        Self::entry_inner(
            provider_id,
            display_name,
            client,
            credentials,
            None,
            auth_profile,
            Some(authentication_delegate),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the thin Worker registration keeps its capability slots visible in one place"
    )]
    fn entry_inner(
        provider_id: asterism_domain::ProviderId,
        display_name: impl Into<String>,
        client: UaiWorkerClient,
        credentials: Arc<dyn ProviderCredentialResolver>,
        credential_renewer: Option<Arc<dyn ProviderCredentialRenewer>>,
        auth_profile: WorkerAuthProfile,
        authentication_delegate: Option<Arc<dyn AuthenticationCapability>>,
    ) -> ProviderEntry {
        let supports_questions = matches!(provider_id.as_str(), "chaoxing" | "cidaren");
        let supports_execution = matches!(
            provider_id.as_str(),
            "chaoxing" | "welearn" | "uai" | "cidaren"
        );
        let auth_methods = match auth_profile {
            WorkerAuthProfile::PasswordAndCookie => {
                [AuthMethod::Password, AuthMethod::ImportedCookie]
                    .into_iter()
                    .collect()
            }
            WorkerAuthProfile::Password => [AuthMethod::Password].into_iter().collect(),
            WorkerAuthProfile::ImportedToken => [AuthMethod::ImportedToken].into_iter().collect(),
            WorkerAuthProfile::ExternalOauthComposite => {
                [AuthMethod::ExternalBrowserOauth].into_iter().collect()
            }
        };
        let mut capabilities = [
            ProviderCapability::Authentication,
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        if supports_questions {
            capabilities.insert(ProviderCapability::QuestionInventory);
            capabilities.insert(ProviderCapability::QuestionParse);
        }
        if matches!(provider_id.as_str(), "chaoxing" | "cidaren") {
            capabilities.insert(ProviderCapability::AnswerResolve);
        }
        if provider_id.as_str() == "chaoxing" {
            capabilities.insert(ProviderCapability::AnswerHistoryHarvest);
        }
        if supports_execution {
            capabilities.insert(ProviderCapability::ResourceExecution);
        }
        if matches!(provider_id.as_str(), "welearn" | "uai") {
            capabilities.insert(ProviderCapability::DurationReport);
        }
        if provider_id.as_str() == "uai" {
            capabilities.insert(ProviderCapability::TaskDetail);
            capabilities.insert(ProviderCapability::DurationRead);
            capabilities.insert(ProviderCapability::Discussion);
        }
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: display_name.into(),
            implementation_version: "0.0.1-upstream-worker-v1".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: Some(60),
            capture_recipe_version: None,
            capabilities,
            auth_methods,
            session_kinds: match auth_profile {
                WorkerAuthProfile::PasswordAndCookie => [
                    SessionKind::ProviderSpecific,
                    SessionKind::Cookie,
                    SessionKind::Composite,
                ]
                .into_iter()
                .collect(),
                WorkerAuthProfile::Password => {
                    [SessionKind::ProviderSpecific, SessionKind::Composite]
                        .into_iter()
                        .collect()
                }
                WorkerAuthProfile::ImportedToken => [
                    SessionKind::BearerToken,
                    SessionKind::Jwt,
                    SessionKind::Composite,
                ]
                .into_iter()
                .collect(),
                WorkerAuthProfile::ExternalOauthComposite => {
                    [SessionKind::Composite].into_iter().collect()
                }
            },
        };
        let provider = Arc::new(Self {
            metadata: metadata.clone(),
            client,
            credentials,
            credential_renewer,
            auth_profile,
            authentication_delegate,
            task_routes: Arc::new(RwLock::new(HashMap::new())),
            history_questions: Arc::new(RwLock::new(HashMap::new())),
            request_sessions: Arc::new(RwLock::new(HashMap::new())),
        });
        let runtime_settings = worker_runtime_settings_schema(&metadata.id);
        ProviderEntry {
            metadata,
            runtime_settings,
            authentication: Some(provider.clone()),
            course_inventory: Some(provider.clone()),
            course_enrollment: None,
            task_inventory: Some(provider.clone()),
            task_detail: (provider.metadata.id.as_str() == "uai")
                .then(|| provider.clone() as Arc<dyn TaskDetailCapability>),
            task_progress: None,
            duration_read: (provider.metadata.id.as_str() == "uai")
                .then(|| provider.clone() as Arc<dyn DurationReadCapability>),
            question_inventory: supports_questions
                .then(|| provider.clone() as Arc<dyn QuestionInventoryCapability>),
            question_parse: supports_questions
                .then(|| provider.clone() as Arc<dyn QuestionParseCapability>),
            answer_resolve: matches!(provider.metadata.id.as_str(), "chaoxing" | "cidaren")
                .then(|| provider.clone() as Arc<dyn AnswerResolveCapability>),
            submission_build: None,
            submission_execute: None,
            submission_verify: None,
            answer_history_harvest: (provider.metadata.id.as_str() == "chaoxing")
                .then(|| provider.clone() as Arc<dyn AnswerHistoryHarvestCapability>),
            task_execution: supports_execution
                .then_some(provider as Arc<dyn TaskExecutionCapability>),
            browser_bridge: None,
        }
    }

    fn map_client(operation: &str, error: &UaiWorkerClientError) -> ProviderError {
        let kind = match &error {
            UaiWorkerClientError::Remote { code, .. }
                if code == "authentication_failed" || code == "session_invalid" =>
            {
                ProviderErrorKind::Authentication
            }
            UaiWorkerClientError::Remote { code, .. }
                if matches!(
                    code.as_str(),
                    "question_access_denied"
                        | "question_type_unsupported"
                        | "task_unsupported"
                        | "task_not_open"
                ) =>
            {
                ProviderErrorKind::UnsupportedTask
            }
            UaiWorkerClientError::Timeout(_)
            | UaiWorkerClientError::Spawn(_)
            | UaiWorkerClientError::Write(_)
            | UaiWorkerClientError::Read(_)
            | UaiWorkerClientError::Wait(_)
            // A worker process can be terminated by the OS, a supervisor, or
            // an interrupted upstream request (for example while a TLS
            // connection is being established).  This is a transport/runtime
            // failure, not evidence that the Provider protocol changed.  Keep
            // the harvest retryable instead of dead-lettering the account.
            | UaiWorkerClientError::Exited { .. } => ProviderErrorKind::Network,
            UaiWorkerClientError::Remote { .. } => ProviderErrorKind::ProviderUnavailable,
            _ => ProviderErrorKind::InvalidResponse,
        };
        let message = match error {
            UaiWorkerClientError::Remote { code, message } => {
                format!("upstream worker {operation} failed: {code}: {message}")
            }
            _ => format!("upstream worker {operation} failed: {error}"),
        };
        ProviderError::new(kind, message)
    }

    async fn invoke(&self, operation: &str, payload: Value) -> ProviderResult<Value> {
        self.client
            .invoke_result(operation, payload)
            .await
            .map_err(|error| Self::map_client(operation, &error))
    }

    async fn invoke_observed(
        &self,
        operation: &str,
        payload: Value,
    ) -> ProviderResult<asterism_uai_worker_client::WorkerInvocationResult> {
        self.client
            .invoke_observed_result(operation, payload)
            .await
            .map_err(|error| Self::map_client(operation, &error))
    }

    async fn invoke_run_observed(
        &self,
        session: Value,
        worker_remote_id: &str,
        native: Value,
        settings: Value,
        answers: Value,
    ) -> ProviderResult<asterism_uai_worker_client::WorkerInvocationResult> {
        if self.metadata.id.as_str() != "chaoxing"
            || native.get("route_kind").and_then(Value::as_str) != Some("knowledge_point")
        {
            return self
                .invoke_observed(
                    "run",
                    json!({
                        "session": session,
                        "task": {"remote_id": worker_remote_id, "native": native},
                        "settings": settings,
                        "answers": answers,
                    }),
                )
                .await;
        }
        self.invoke_chaoxing_knowledge_jobs(session, worker_remote_id, native, settings, answers)
            .await
    }

    async fn invoke_chaoxing_knowledge_jobs(
        &self,
        session: Value,
        worker_remote_id: &str,
        mut native: Value,
        settings: Value,
        answers: Value,
    ) -> ProviderResult<asterism_uai_worker_client::WorkerInvocationResult> {
        let children = native
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "Chaoxing knowledge point jobs are missing",
                )
            })?;
        if children.is_empty() {
            return self
                .invoke_observed("run", json!({"session": session, "task": {"remote_id": worker_remote_id, "native": native}, "settings": settings, "answers": answers}))
                .await;
        }
        let supplied = answers.as_array().cloned().unwrap_or_default();
        let mut video = Vec::new();
        let mut answer = Vec::new();
        let mut other = Vec::new();
        for (index, child) in children.iter().enumerate() {
            let Some(job) = child.get("job").and_then(Value::as_object) else {
                continue;
            };
            if job
                .get("_asterism_is_passed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            let kind = job.get("type").and_then(Value::as_str).unwrap_or_default();
            if !matches!(kind, "video" | "document" | "read" | "workid" | "live") {
                continue;
            }
            let identity = job
                .get("_asterism_card_identity")
                .or_else(|| job.get("jobid"))
                .or_else(|| job.get("id"))
                .map(value_text)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("job-{}", index + 1));
            let prefix = format!("{identity}:");
            let child_answers = supplied
                .iter()
                .filter_map(|row| {
                    let remote_id = row.get("remote_id")?.as_str()?;
                    remote_id.strip_prefix(&prefix).map(|stripped| json!({"remote_id": stripped, "value": row.get("value").cloned().unwrap_or(Value::Null)}))
                })
                .collect::<Vec<_>>();
            if kind == "workid" && child_answers.is_empty() {
                continue;
            }
            let child_native = json!({
                "course": native.get("course").cloned().unwrap_or(Value::Null),
                "point": native.get("point").cloned().unwrap_or(Value::Null),
                "job": child.get("job").cloned().unwrap_or(Value::Null),
                "job_info": child.get("job_info").cloned().unwrap_or(Value::Null),
            });
            let payload = json!({
                "session": session,
                "task": {"remote_id": worker_remote_id, "native": child_native},
                "settings": settings,
                "answers": child_answers,
            });
            match kind {
                "video" => video.push(payload),
                "workid" => answer.push(payload),
                _ => other.push(payload),
            }
        }
        let limit = |key: &str, fallback: usize| {
            settings
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .unwrap_or(fallback)
        };
        let run_group = |payloads: Vec<Value>, concurrency: usize| async move {
            stream::iter(
                payloads
                    .into_iter()
                    .map(|payload| async move { self.invoke_observed("run", payload).await }),
            )
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await
        };
        let (video_results, answer_results, other_results) = tokio::join!(
            run_group(video, limit("video_threads", 4)),
            run_group(answer, limit("answer_threads", 1)),
            run_group(other, limit("other_threads", 4)),
        );
        let mut logs = Vec::new();
        let mut progress = Vec::new();
        for result in video_results
            .into_iter()
            .chain(answer_results)
            .chain(other_results)
        {
            let observed = result?;
            logs.extend(observed.logs);
            progress.extend(observed.progress);
        }
        if let Some(children) = native.get_mut("jobs").and_then(Value::as_array_mut) {
            for child in children {
                if let Some(job) = child.get_mut("job").and_then(Value::as_object_mut) {
                    job.insert("_asterism_is_passed".to_owned(), Value::Bool(true));
                }
            }
        }
        let verified = self
            .invoke_observed("run", json!({"session": session, "task": {"remote_id": worker_remote_id, "native": native}, "settings": settings, "answers": []}))
            .await?;
        logs.extend(verified.logs);
        progress.extend(verified.progress);
        Ok(asterism_uai_worker_client::WorkerInvocationResult {
            data: verified.data,
            logs,
            progress,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "credential renewal must remain adjacent to the one retried Worker request"
    )]
    async fn session(&self, context: &ProviderContext) -> ProviderResult<Value> {
        if let Some(session) = self.cached_request_session(context)? {
            return Ok(session);
        }
        let authenticated = match self.auth_profile {
            WorkerAuthProfile::PasswordAndCookie => {
                let cookie = self
                    .resolve(context, vec![SecretPurpose::ProviderCookie])
                    .await;
                let had_cookie = cookie.is_ok();
                if let Ok(cookie) = cookie {
                    let credentials = json!({
                        "cookie": resolved_text(&cookie, SecretPurpose::ProviderCookie)?
                    });
                    if let Ok(session) = self.authenticate_session(credentials).await {
                        self.cache_request_session(context, &session)?;
                        return Ok(session);
                    }
                }
                let purposes = if had_cookie {
                    vec![
                        SecretPurpose::ProviderUsername,
                        SecretPurpose::ProviderPassword,
                        SecretPurpose::ProviderCookie,
                    ]
                } else {
                    vec![
                        SecretPurpose::ProviderUsername,
                        SecretPurpose::ProviderPassword,
                    ]
                };
                let fields = self.resolve(context, purposes).await?;
                let username = resolved_text(&fields, SecretPurpose::ProviderUsername)?.to_owned();
                let password = resolved_text(&fields, SecretPurpose::ProviderPassword)?.to_owned();
                let session = self
                    .authenticate_session(json!({
                        "username": username,
                        "password": password,
                    }))
                    .await?;
                self.persist_password_session(context, &fields, &username, &password, &session)
                    .await?;
                Ok(session)
            }
            WorkerAuthProfile::Password => {
                let fields = match self
                    .resolve(
                        context,
                        vec![
                            SecretPurpose::ProviderUsername,
                            SecretPurpose::ProviderPassword,
                            SecretPurpose::ProviderCompositeSession,
                        ],
                    )
                    .await
                {
                    Ok(fields) => fields,
                    Err(_) => {
                        self.resolve(
                            context,
                            vec![
                                SecretPurpose::ProviderUsername,
                                SecretPurpose::ProviderPassword,
                            ],
                        )
                        .await?
                    }
                };
                let username = resolved_text(&fields, SecretPurpose::ProviderUsername)?.to_owned();
                let password = resolved_text(&fields, SecretPurpose::ProviderPassword)?.to_owned();
                let session = self
                    .authenticate_session(json!({
                        "username": username,
                        "password": password,
                    }))
                    .await?;
                self.persist_password_session(context, &fields, &username, &password, &session)
                    .await?;
                Ok(session)
            }
            WorkerAuthProfile::ImportedToken => {
                let fields = self
                    .resolve(context, vec![SecretPurpose::ProviderAccessToken])
                    .await?;
                self.authenticate_session(json!({
                    "token": resolved_text(&fields, SecretPurpose::ProviderAccessToken)?,
                }))
                .await
            }
            WorkerAuthProfile::ExternalOauthComposite => {
                let fields = self
                    .resolve(
                        context,
                        vec![
                            SecretPurpose::ProviderAccessToken,
                            SecretPurpose::ProviderCompositeSession,
                        ],
                    )
                    .await?;
                self.authenticate_session(json!({
                    "token": resolved_text(&fields, SecretPurpose::ProviderAccessToken)?,
                    "crypto_document": resolved_json(
                        &fields,
                        SecretPurpose::ProviderCompositeSession,
                    )?,
                }))
                .await
            }
        };
        let authenticated = authenticated?;
        self.cache_request_session(context, &authenticated)?;
        Ok(authenticated)
    }

    fn cached_request_session(&self, context: &ProviderContext) -> ProviderResult<Option<Value>> {
        self.request_sessions
            .read()
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "worker request-session cache is unavailable",
                )
            })
            .map(|sessions| {
                sessions
                    .get(&(
                        context.account_id.to_string(),
                        context.correlation_id.clone(),
                    ))
                    .cloned()
            })
    }

    fn cache_request_session(
        &self,
        context: &ProviderContext,
        session: &Value,
    ) -> ProviderResult<()> {
        let mut sessions = self.request_sessions.write().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "worker request-session cache is unavailable",
            )
        })?;
        if sessions.len() >= 128 {
            sessions.clear();
        }
        sessions.insert(
            (
                context.account_id.to_string(),
                context.correlation_id.clone(),
            ),
            session.clone(),
        );
        Ok(())
    }

    async fn persist_password_session(
        &self,
        context: &ProviderContext,
        resolved: &[ResolvedProviderCredential],
        username: &str,
        password: &str,
        session: &Value,
    ) -> ProviderResult<()> {
        let Some(renewer) = &self.credential_renewer else {
            return Ok(());
        };
        let session_field = match self.auth_profile {
            WorkerAuthProfile::PasswordAndCookie => CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(cookie_header_from_session(session)?.into_bytes()),
            },
            WorkerAuthProfile::Password => CredentialField {
                purpose: SecretPurpose::ProviderCompositeSession,
                value: SecretValue::new(serde_json::to_vec(session).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "worker session could not be persisted",
                    )
                })?),
            },
            WorkerAuthProfile::ImportedToken | WorkerAuthProfile::ExternalOauthComposite => {
                return Ok(());
            }
        };
        let bundle = CredentialBundle {
            provider_id: self.metadata.id.clone(),
            tenant: None,
            auth_method: AuthMethod::Password,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: chrono::Utc::now(),
            expires_at: None,
            session_kind: SessionKind::Composite,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderUsername,
                    value: SecretValue::new(username.as_bytes().to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderPassword,
                    value: SecretValue::new(password.as_bytes().to_vec()),
                },
                session_field,
            ],
            user_id_hint: None,
        };
        renewer
            .renew_provider_credentials(ProviderCredentialRenewal {
                provider_account_id: context.account_id,
                expected_credentials: resolved
                    .iter()
                    .map(|field| field.credential.clone())
                    .collect(),
                bundle,
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| {
                let kind = match error {
                    SecretStoreError::NotFound
                    | SecretStoreError::Unauthorized
                    | SecretStoreError::AccountMismatch => ProviderErrorKind::Authentication,
                    _ => ProviderErrorKind::Internal,
                };
                ProviderError::new(kind, format!("worker credential renewal failed: {error}"))
            })?;
        Ok(())
    }

    async fn resolve(
        &self,
        context: &ProviderContext,
        purposes: Vec<SecretPurpose>,
    ) -> ProviderResult<Vec<ResolvedProviderCredential>> {
        self.credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes,
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_secret_error(&error))
    }

    async fn authenticate_session(&self, credentials: Value) -> ProviderResult<Value> {
        self.invoke("authenticate", json!({"credentials": credentials}))
            .await?
            .get("session")
            .cloned()
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker login returned no session",
                )
            })
    }

    async fn all_tasks(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Vec<(RemoteTask, Value)>> {
        let session = self.session(context).await?;
        let courses = self.worker_courses(session.clone()).await?;
        let mut tasks = Vec::new();
        for course in courses {
            let native = course
                .route_context
                .get("worker.native")
                .and_then(|value| serde_json::from_str(value).ok())
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "worker course route is missing",
                    )
                })?;
            tasks.extend(
                self.worker_tasks(
                    session.clone(),
                    &context.account_id.to_string(),
                    &course.remote_id,
                    native,
                )
                .await?,
            );
        }
        Ok(tasks)
    }

    async fn worker_courses(&self, session: Value) -> ProviderResult<Vec<RemoteCourse>> {
        let result = self.invoke("courses", json!({"session": session})).await?;
        let rows = result
            .get("courses")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker courses result is invalid",
                )
            })?;
        rows.iter()
            .map(|row| {
                let native = row.get("native").cloned().unwrap_or_else(|| json!({}));
                let route = serde_json::to_string(&native).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "worker course route is invalid",
                    )
                })?;
                Ok(RemoteCourse {
                    remote_id: required_string(row, "remote_id")?,
                    title: required_string(row, "title")?,
                    term: optional_string(row, "term"),
                    teacher: optional_string(row, "teacher"),
                    remote_status: optional_string(row, "remote_status"),
                    metadata_sanitized: json!({
                        "worker_backed": true,
                        "read_only": row.get("read_only").and_then(Value::as_bool).unwrap_or(false),
                        "provider_summary": row.get("provider_summary").cloned().unwrap_or(Value::Null),
                    }),
                    route_context: ProviderRouteContext::try_from_pairs([(
                        "worker.native".to_owned(),
                        route,
                    )])?,
                })
            })
            .collect()
    }

    async fn worker_tasks(
        &self,
        session: Value,
        account_id: &str,
        course_id: &str,
        native: Value,
    ) -> ProviderResult<Vec<(RemoteTask, Value)>> {
        let result = self
            .invoke("tasks", json!({"session": session, "course": native}))
            .await?;
        let rows = result
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker tasks result is invalid",
                )
            })?;
        let tasks = rows
            .iter()
            .map(|row| {
                let worker_remote_id = required_string(row, "remote_id")?;
                let remote_id = worker_task_remote_id(
                    course_id,
                    &worker_remote_id,
                    row.get("global_remote_id")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
                let mut native = row.get("native").cloned().unwrap_or_else(|| json!({}));
                if let Some(object) = native.as_object_mut() {
                    object.insert(
                        "_asterism_worker_remote_id".to_owned(),
                        Value::String(worker_remote_id),
                    );
                }
                let worker_capabilities = row
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut capabilities = Vec::new();
                if worker_capabilities.iter().any(|value| value == "questions") {
                    capabilities.extend([
                        TaskCapability::QuestionInventory,
                        TaskCapability::QuestionParse,
                    ]);
                    if matches!(self.metadata.id.as_str(), "chaoxing" | "cidaren") {
                        capabilities.push(TaskCapability::AnswerResolve);
                    }
                }
                if worker_capabilities.iter().any(|value| value == "run") {
                    capabilities.push(TaskCapability::ResourceExecution);
                }
                if self.metadata.id.as_str() == "uai"
                    && native.get("category").and_then(Value::as_str) == Some("discussion")
                {
                    capabilities.push(TaskCapability::Discussion);
                }
                if worker_capabilities.iter().any(|value| value == "duration") {
                    capabilities.push(TaskCapability::DurationReport);
                }
                if worker_capabilities
                    .iter()
                    .any(|value| value == "duration_read")
                {
                    capabilities.push(TaskCapability::DurationRead);
                }
                let task = RemoteTask {
                    remote_id: remote_id.clone(),
                    course_remote_id: Some(course_id.to_owned()),
                    title: required_string(row, "title")?,
                    source_type: map_source_type(row.get("source_type")),
                    assessment_class: map_assessment_class(row.get("assessment_class")),
                    remote_state: map_state(row.get("state")),
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities,
                    fingerprint: fingerprint(&self.metadata.id, &remote_id, &native),
                    normalized: json!({}),
                    raw_sanitized: json!({
                        "worker_backed": true,
                        "read_only": row.get("read_only").and_then(Value::as_bool).unwrap_or(false),
                        "provider_summary": if self.metadata.id.as_str() == "uai" { json!({
                            "category": native.get("category").cloned().unwrap_or(Value::Null),
                            "required": native.get("required").cloned().unwrap_or(Value::Null),
                            "score_task": native.get("score_task").cloned().unwrap_or(Value::Null),
                            "finish_progress": native.get("finish_progress").cloned().unwrap_or(Value::Null),
                            "task_score": native.get("task_score").cloned().unwrap_or(Value::Null),
                            "question_num": native.get("question_num").cloned().unwrap_or(Value::Null),
                        }) } else if self.metadata.id.as_str() == "chaoxing" { json!({
                            "position": native.get("provider_position").cloned().unwrap_or(Value::Null),
                            "locked": native.pointer("/point/need_unlock").cloned().unwrap_or(Value::Null),
                            "job_count": native.get("jobs").and_then(Value::as_array).map_or(Value::Null, |jobs| json!(jobs.len())),
                        }) } else { Value::Null },
                    }),
                };
                Ok((task, native))
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        if let Ok(mut routes) = self.task_routes.write() {
            for (task, native) in &tasks {
                routes.insert(
                    (account_id.to_owned(), task.remote_id.clone()),
                    native.clone(),
                );
            }
        }
        Ok(tasks)
    }

    async fn question_rows(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<Value>> {
        let session = self.session(context).await?;
        let cache_key = (context.account_id.to_string(), remote_task_id.to_owned());
        let cached = self
            .task_routes
            .read()
            .ok()
            .and_then(|routes| routes.get(&cache_key).cloned());
        let native = match cached {
            Some(native) => native,
            None => self
                .all_tasks(context)
                .await?
                .into_iter()
                .find(|(task, _)| task.remote_id == remote_task_id)
                .map(|(_, native)| native)
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::RemoteChanged,
                        "worker task disappeared during fresh scan",
                    )
                })?,
        };
        let worker_remote_id = native
            .get("_asterism_worker_remote_id")
            .and_then(Value::as_str)
            .unwrap_or(remote_task_id);
        let result = self
            .invoke(
                "questions",
                json!({
                    "session": session,
                    "task": {"remote_id": worker_remote_id, "native": native},
                    // Cidaren has no side-effect-free question endpoint: the
                    // donor's StartAnswer call is the audited discovery path.
                    // This method is reached only through an explicit question
                    // inventory request, never during account/course scanning.
                    "allow_read_that_starts_attempt": self.metadata.id.as_str() == "cidaren",
                }),
            )
            .await?;
        Ok(result
            .get("questions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn task_native(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Value> {
        let cache_key = (context.account_id.to_string(), remote_task_id.to_owned());
        if let Some(native) = self
            .task_routes
            .read()
            .ok()
            .and_then(|routes| routes.get(&cache_key).cloned())
        {
            return Ok(native);
        }
        self.all_tasks(context)
            .await?
            .into_iter()
            .find_map(|(task, native)| (task.remote_id == remote_task_id).then_some(native))
            .ok_or_else(|| {
                ProviderError::new(ProviderErrorKind::RemoteChanged, "worker task disappeared")
            })
    }
}

fn cidaren_answer_bridge_ticket() -> Option<String> {
    std::env::var("ASTERISM_CIDAREN_ANSWER_BRIDGE_TICKET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let deployment_secret = std::env::var("ASTERISM_SECRET_KEYS").ok()?;
            let digest = Sha256::digest(
                format!("asterism.cidaren.answer-bridge.v1\0{deployment_secret}").as_bytes(),
            );
            Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
        })
}

impl ProviderIdentity for UpstreamWorkerProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the four Worker authentication profiles intentionally share one thin boundary"
)]
#[async_trait]
impl AuthenticationCapability for UpstreamWorkerProvider {
    #[allow(
        clippy::too_many_lines,
        reason = "the four Worker authentication profiles intentionally share one thin boundary"
    )]
    async fn begin_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<AuthChallenge> {
        if !self.metadata.auth_methods.contains(&method) {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "worker authentication method is unsupported",
            ));
        }
        if let Some(delegate) = &self.authentication_delegate {
            return delegate.begin_authentication(context, method).await;
        }
        if self.auth_profile == WorkerAuthProfile::ExternalOauthComposite {
            let result = self.invoke("oauth_begin", json!({})).await?;
            let binding = ExternalOauthCallbackBinding::from_digests(
                decode_digest(&required_string(&result, "state_digest")?)?,
                decode_digest(&required_string(&result, "marker_digest")?)?,
            );
            let external_oauth = ExternalOauthAuthorization {
                authorization_url: required_string(&result, "authorization_url")?,
                callback_binding: binding,
            };
            if !external_oauth.validate() {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker OAuth authorization is invalid",
                ));
            }
            return Ok(AuthChallenge {
                session_id: context.auth_session_id.unwrap_or_default(),
                method,
                waiting_for: WaitingUserState::BrowserCallback,
                user_action: Some(
                    "Copy the authorization URL to WeChat, then paste the final callback URL"
                        .to_owned(),
                ),
                expires_at: None,
                external_oauth: Some(external_oauth),
            });
        }
        Ok(AuthChallenge {
            session_id: context.auth_session_id.unwrap_or_default(),
            method,
            waiting_for: if method == AuthMethod::Password {
                WaitingUserState::CredentialInput
            } else {
                WaitingUserState::SessionImport
            },
            user_action: None,
            expires_at: None,
            external_oauth: None,
        })
    }

    async fn validate_credential(
        &self,
        context: &ProviderAuthContext,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if credential.provider_id != self.metadata.id
            || !self.metadata.auth_methods.contains(&credential.auth_method)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "worker credential shape is invalid",
            ));
        }
        if let Some(delegate) = &self.authentication_delegate {
            return delegate.validate_credential(context, credential).await;
        }
        if self.auth_profile == WorkerAuthProfile::ExternalOauthComposite {
            let credentials = json!({
                "token": field_text(credential, SecretPurpose::ProviderAccessToken)?,
                "crypto_document": field_json(
                    credential,
                    SecretPurpose::ProviderCompositeSession,
                )?,
            });
            let result = self
                .invoke("authenticate", json!({"credentials": credentials}))
                .await?;
            return Ok(CredentialValidation::accepted(SessionStatus {
                valid: true,
                kind: SessionKind::Composite,
                expires_at: None,
                account_hint: result
                    .pointer("/account/display_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }));
        }
        let credentials = match credential.auth_method {
            AuthMethod::Password => {
                json!({"username": field_text(credential, SecretPurpose::ProviderUsername)?,
                                           "password": field_text(credential, SecretPurpose::ProviderPassword)?})
            }
            AuthMethod::ImportedCookie => {
                json!({"cookie": field_text(credential, SecretPurpose::ProviderCookie)?})
            }
            AuthMethod::ImportedToken => {
                json!({"token": field_text(credential, SecretPurpose::ProviderAccessToken)?})
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "worker authentication method is unsupported",
                ));
            }
        };
        let result = self
            .invoke("authenticate", json!({"credentials": credentials}))
            .await?;
        let session = result.get("session").cloned().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker login returned no session",
            )
        })?;
        let account_hint = result
            .pointer("/account/display_name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let replacement = match credential.auth_method {
            AuthMethod::Password => {
                let session_field = match self.auth_profile {
                    WorkerAuthProfile::PasswordAndCookie => CredentialField {
                        purpose: SecretPurpose::ProviderCookie,
                        value: SecretValue::new(cookie_header_from_session(&session)?.into_bytes()),
                    },
                    WorkerAuthProfile::Password => CredentialField {
                        purpose: SecretPurpose::ProviderCompositeSession,
                        value: SecretValue::new(serde_json::to_vec(&session).map_err(|_| {
                            ProviderError::new(
                                ProviderErrorKind::InvalidResponse,
                                "worker session is invalid",
                            )
                        })?),
                    },
                    WorkerAuthProfile::ImportedToken
                    | WorkerAuthProfile::ExternalOauthComposite => {
                        return Err(ProviderError::new(
                            ProviderErrorKind::Authentication,
                            "worker password authentication profile is invalid",
                        ));
                    }
                };
                CredentialReplacement {
                    session_kind: SessionKind::Composite,
                    fields: vec![
                        CredentialField {
                            purpose: SecretPurpose::ProviderUsername,
                            value: SecretValue::new(
                                field_text(credential, SecretPurpose::ProviderUsername)?
                                    .as_bytes()
                                    .to_vec(),
                            ),
                        },
                        CredentialField {
                            purpose: SecretPurpose::ProviderPassword,
                            value: SecretValue::new(
                                field_text(credential, SecretPurpose::ProviderPassword)?
                                    .as_bytes()
                                    .to_vec(),
                            ),
                        },
                        session_field,
                    ],
                }
            }
            AuthMethod::ImportedCookie => CredentialReplacement {
                session_kind: SessionKind::Cookie,
                fields: vec![CredentialField {
                    purpose: SecretPurpose::ProviderCookie,
                    value: SecretValue::new(
                        field_text(credential, SecretPurpose::ProviderCookie)?
                            .as_bytes()
                            .to_vec(),
                    ),
                }],
            },
            AuthMethod::ImportedToken => CredentialReplacement {
                session_kind: SessionKind::BearerToken,
                fields: vec![CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(
                        field_text(credential, SecretPurpose::ProviderAccessToken)?
                            .as_bytes()
                            .to_vec(),
                    ),
                }],
            },
            _ => unreachable!("unsupported authentication methods returned above"),
        };
        let _ = context;
        Ok(CredentialValidation {
            status: SessionStatus {
                valid: true,
                kind: replacement.session_kind,
                expires_at: None,
                account_hint,
            },
            replacement: Some(replacement),
        })
    }

    async fn exchange_external_oauth_callback(
        &self,
        context: &ProviderAuthContext,
        callback_url: SecretString,
        binding: ExternalOauthCallbackBinding,
    ) -> ProviderResult<CredentialReplacement> {
        if let Some(delegate) = &self.authentication_delegate {
            return delegate
                .exchange_external_oauth_callback(context, callback_url, binding)
                .await;
        }
        if self.auth_profile != WorkerAuthProfile::ExternalOauthComposite || !binding.validate() {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "worker authentication does not support external OAuth",
            ));
        }
        let result = self
            .invoke(
                "oauth_exchange",
                json!({
                    "callback_url": callback_url.expose_secret(),
                    "binding": {
                        "state_digest": encode_digest(binding.state_digest()),
                        "marker_digest": encode_digest(binding.provider_context_digest()),
                    }
                }),
            )
            .await?;
        let session = result.get("session").ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker OAuth exchange returned no session",
            )
        })?;
        let token = required_string(session, "token")?;
        let crypto = session.get("crypto_document").ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker OAuth exchange returned no crypto context",
            )
        })?;
        let crypto = serde_json::to_vec(crypto).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker OAuth crypto context is invalid",
            )
        })?;
        let _ = context;
        Ok(CredentialReplacement {
            session_kind: SessionKind::Composite,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(token.into_bytes()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderCompositeSession,
                    value: SecretValue::new(crypto),
                },
            ],
        })
    }

    async fn validate_session(&self, context: &ProviderContext) -> ProviderResult<SessionStatus> {
        let session = self.session(context).await?;
        self.worker_courses(session).await?;
        Ok(SessionStatus {
            valid: true,
            kind: SessionKind::Composite,
            expires_at: None,
            account_hint: None,
        })
    }
}

#[async_trait]
impl CourseInventoryCapability for UpstreamWorkerProvider {
    async fn list_courses(&self, context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>> {
        self.worker_courses(self.session(context).await?).await
    }
}

#[async_trait]
impl TaskInventoryCapability for UpstreamWorkerProvider {
    async fn list_tasks(
        &self,
        context: &ProviderContext,
        course: Option<&RemoteCourse>,
    ) -> ProviderResult<Vec<RemoteTask>> {
        let session = self.session(context).await?;
        if let Some(course) = course {
            let native = course
                .route_context
                .get("worker.native")
                .and_then(|value| serde_json::from_str(value).ok())
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "worker course route is missing",
                    )
                })?;
            return Ok(self
                .worker_tasks(
                    session,
                    &context.account_id.to_string(),
                    &course.remote_id,
                    native,
                )
                .await?
                .into_iter()
                .map(|(task, _)| task)
                .collect());
        }
        Ok(self
            .all_tasks(context)
            .await?
            .into_iter()
            .map(|(task, _)| task)
            .collect())
    }
}

#[async_trait]
impl TaskDetailCapability for UpstreamWorkerProvider {
    async fn task_detail(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteTaskDetail> {
        if self.metadata.id.as_str() != "uai" {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "worker task detail is not enabled for this Provider",
            ));
        }
        let session = self.session(context).await?;
        let tasks = self.all_tasks(context).await?;
        let (task, native) = tasks
            .into_iter()
            .find(|(task, _)| task.remote_id == remote_task_id)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::RemoteChanged,
                    "worker task disappeared during fresh detail inspection",
                )
            })?;
        let worker_remote_id = native
            .get("_asterism_worker_remote_id")
            .and_then(Value::as_str)
            .unwrap_or(remote_task_id);
        let result = self
            .invoke(
                "inspect",
                json!({
                    "session": session,
                    "task": {"remote_id": worker_remote_id, "native": native},
                }),
            )
            .await?;
        let worker_task = result
            .get("task")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker task detail result is invalid",
                )
            })?;
        let detail_native = worker_task
            .get("native")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker task detail native payload is invalid",
                )
            })?;
        let category = detail_native
            .get("category")
            .cloned()
            .unwrap_or(Value::Null);
        let content = detail_native.get("content").cloned().unwrap_or(Value::Null);
        Ok(RemoteTaskDetail {
            task,
            normalized_detail: json!({
                "schema": "uai.worker-task-detail.v1",
                "task": {
                    "category": category,
                    "required": detail_native.get("required").cloned().unwrap_or(Value::Null),
                    "score_task": detail_native.get("score_task").cloned().unwrap_or(Value::Null),
                    "finish_progress": detail_native.get("finish_progress").cloned().unwrap_or(Value::Null),
                    "task_score": detail_native.get("task_score").cloned().unwrap_or(Value::Null),
                    "question_num": detail_native.get("question_num").cloned().unwrap_or(Value::Null),
                    "content": content,
                },
            }),
        })
    }
}

#[async_trait]
impl QuestionInventoryCapability for UpstreamWorkerProvider {
    async fn list_question_refs(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<RemoteQuestionRef>> {
        self.question_rows(context, remote_task_id)
            .await?
            .into_iter()
            .map(|row| {
                let remote_id = required_string(&row, "remote_id")?;
                let position = row
                    .get("position")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(1);
                let metadata_sanitized = json!({
                    "remote_id": remote_id,
                    "position": position,
                    "kind": row.get("kind").cloned().unwrap_or(Value::Null),
                    "prompt": row.get("prompt").cloned().unwrap_or(Value::Null),
                    "options": row.get("options").cloned().unwrap_or_else(|| json!([])),
                    "native_shape": row.get("native_shape").cloned().unwrap_or(Value::Null),
                    "worker_backed": true
                });
                Ok(RemoteQuestionRef {
                    remote_id,
                    position,
                    kind_hint: map_kind(row.get("kind")),
                    metadata_sanitized,
                    route_context: ProviderRouteContext::default(),
                })
            })
            .collect()
    }
}

#[async_trait]
impl QuestionParseCapability for UpstreamWorkerProvider {
    async fn parse_question(
        &self,
        _context: &ProviderContext,
        task_id: TaskId,
        _remote_task_id: &str,
        reference: &RemoteQuestionRef,
    ) -> ProviderResult<Question> {
        let row = &reference.metadata_sanitized;
        let options = row
            .get("options")
            .and_then(Value::as_array)
            .map(|values| worker_question_options(values))
            .unwrap_or_default();
        let prompt = row
            .get("prompt")
            .map(value_text)
            .and_then(|value| clean_text(&value, 64 * 1024))
            .unwrap_or_else(|| format!("Provider question {}", reference.remote_id));
        let attachments = rich_attachments(&prompt);
        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some(reference.remote_id.clone()),
            kind: reference.kind_hint,
            stem: prompt,
            options,
            attachments,
            metadata_sanitized: json!({
                "worker_backed": true,
                "provider_kind": row.get("kind").cloned().unwrap_or(Value::Null),
                "provider_private_shape": row.get("native_shape").cloned().unwrap_or(Value::Null),
            }),
            position: reference.position,
        };
        question.validate().map_err(|error| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!("worker question is invalid: {error}"),
            )
        })?;
        Ok(question)
    }

    async fn parse_structured_question_set(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        references: &[RemoteQuestionRef],
    ) -> ProviderResult<ProviderStructuredQuestionParseSet> {
        let mut questions = Vec::with_capacity(references.len());
        for reference in references {
            questions.push(
                self.parse_question(context, task_id, remote_task_id, reference)
                    .await?,
            );
        }
        if !matches!(self.metadata.id.as_str(), "chaoxing" | "cidaren") {
            return ProviderStructuredQuestionParseSet::try_new(questions, Vec::new(), None);
        }

        let mut groups = Vec::<QuestionGroup>::new();
        let mut group_by_options = BTreeMap::<String, usize>::new();
        for (reference, question) in references.iter().zip(&questions) {
            let Some(shared) = reference
                .metadata_sanitized
                .pointer("/native_shape/shared_options")
                .and_then(Value::as_array)
            else {
                continue;
            };
            let values = shared
                .iter()
                .filter_map(|value| value.as_str())
                .filter_map(|value| clean_text(value, 32 * 1024))
                .collect::<Vec<_>>();
            if values.is_empty() {
                continue;
            }
            let encoded = serde_json::to_string(&values).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "Chaoxing shared options are invalid",
                )
            })?;
            let digest = format!("{:x}", Sha256::digest(encoded.as_bytes()));
            let group_index = if let Some(index) = group_by_options.get(&digest) {
                *index
            } else {
                let options = values
                    .iter()
                    .enumerate()
                    .map(|(index, content)| QuestionOption {
                        id: format!("shared-option-{}", index + 1),
                        content: Some(content.clone()),
                        attachments: rich_attachments(content),
                        metadata_sanitized: json!({"provider_shared_option": true}),
                    })
                    .collect::<Vec<_>>();
                let index = groups.len();
                groups.push(QuestionGroup {
                    id: QuestionGroupId::new(),
                    task_id,
                    remote_group_id: Some(format!("shared-options:{}", &digest[..16])),
                    stem: None,
                    options,
                    attachments: Vec::new(),
                    metadata_sanitized: json!({
                        "worker_backed": true,
                        "provider_shape": "shared_options",
                    }),
                    children: Vec::new(),
                });
                group_by_options.insert(digest, index);
                index
            };
            groups[group_index]
                .children
                .push(QuestionGroupChild::Question(question.id));
        }
        ProviderStructuredQuestionParseSet::try_new(questions, groups, None)
    }
}

#[async_trait]
impl AnswerResolveCapability for UpstreamWorkerProvider {
    async fn resolve_answers(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Vec<AnswerCandidate>> {
        if !matches!(self.metadata.id.as_str(), "chaoxing" | "cidaren") {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "this upstream worker does not expose Provider-native answers",
            ));
        }
        let rows = self.question_rows(context, remote_task_id).await?;
        let mut by_remote_id = HashMap::new();
        for row in rows {
            let remote_id = required_string(&row, "remote_id")?;
            if by_remote_id.insert(remote_id, row).is_some() {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker returned duplicate answer evidence identity",
                ));
            }
        }
        let confidence = AnswerConfidence::try_new(if self.metadata.id.as_str() == "cidaren" {
            // The donor answer_lib is useful historical evidence, but it
            // is not an audited platform standard answer.
            3_000
        } else {
            10_000
        })
        .map_err(|_| {
            ProviderError::new(ProviderErrorKind::Internal, "answer confidence is invalid")
        })?;
        let mut candidates = Vec::new();
        for question in questions {
            let Some(remote_id) = question.remote_question_id.as_deref() else {
                continue;
            };
            let Some(row) = by_remote_id.get(remote_id) else {
                continue;
            };
            let Some(evidence) = row.get("answer_evidence").and_then(Value::as_object) else {
                continue;
            };
            let source = evidence.get("source").and_then(Value::as_str);
            if !matches!(
                source,
                Some("chaoxing_reviewed_result") | Some("cidaren_answer_lib")
            ) {
                continue;
            }
            let Some(value) = evidence.get("value").and_then(Value::as_str) else {
                continue;
            };
            let Some(answer) = normalize_reviewed_answer(question.kind, value) else {
                continue;
            };
            let candidate = AnswerCandidate {
                question_id: question.id,
                source: AnswerSource::ProviderNative,
                answer,
                confidence: Some(confidence),
                explanation: None,
                provenance_sanitized: json!({
                    "origin": source.unwrap_or("provider_native"),
                    "worker_backed": true,
                }),
            };
            candidate.validate().map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker reviewed answer candidate is invalid",
                )
            })?;
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}

#[async_trait]
impl AnswerHistoryHarvestCapability for UpstreamWorkerProvider {
    async fn list_answer_history(
        &self,
        context: &ProviderContext,
        cursor: Option<&AnswerHistoryCursor>,
    ) -> ProviderResult<AnswerHistoryPage> {
        if self.metadata.id.as_str() != "chaoxing" {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "answer history bootstrap is only exposed by the Chaoxing worker",
            ));
        }
        let after = parse_worker_history_cursor(cursor, &self.metadata.id)?;
        let session = self.session(context).await?;
        let mut courses = self.worker_courses(session.clone()).await?;
        courses.sort_by(|left, right| left.remote_id.cmp(&right.remote_id));
        let mut references = Vec::new();
        for course in courses {
            if after
                .as_ref()
                .is_some_and(|value| course.remote_id < value.course_remote_id)
            {
                continue;
            }
            let native = course
                .route_context
                .get("worker.native")
                .and_then(|value| serde_json::from_str(value).ok())
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "worker course route is missing",
                    )
                })?;
            let mut tasks = self
                .worker_tasks(
                    session.clone(),
                    &context.account_id.to_string(),
                    &course.remote_id,
                    native,
                )
                .await?;
            tasks.sort_by(|(left, _), (right, _)| left.remote_id.cmp(&right.remote_id));
            for (task, _) in tasks {
                if after.as_ref().is_some_and(|value| {
                    course.remote_id == value.course_remote_id
                        && task.remote_id <= value.task_remote_id
                }) {
                    continue;
                }
                if task.remote_state != RemoteState::Completed
                    || !task
                        .capabilities
                        .contains(&TaskCapability::QuestionInventory)
                {
                    continue;
                }
                let provider_attempt_digest =
                    history_digest(&["chaoxing.worker-history-attempt.v2", &task.remote_id]);
                references.push(AnswerHistoryTaskRef {
                    remote_task_id: task.remote_id.clone(),
                    course_remote_id: task.course_remote_id.clone(),
                    provider_attempt_digest,
                    completed_at: None,
                    metadata_sanitized: json!({
                        "schema": "chaoxing.worker-history-reference.v1",
                        "source_family": "completed_worker_task",
                        "read_only": true,
                    }),
                    route_context: ProviderRouteContext::default(),
                });
                if references.len() == 20 {
                    let next_cursor = AnswerHistoryCursor {
                        version: 2,
                        cursor_type: "chaoxing.worker-history.v2".to_owned(),
                        value_sanitized: json!({
                            "course_remote_id": course.remote_id,
                            "task_remote_id": task.remote_id,
                        }),
                    };
                    return AnswerHistoryPage::try_new(
                        &self.metadata.id,
                        references,
                        Some(next_cursor),
                        false,
                    )
                    .map_err(|_| {
                        ProviderError::new(
                            ProviderErrorKind::InvalidResponse,
                            "Chaoxing worker history page is invalid",
                        )
                    });
                }
            }
        }
        AnswerHistoryPage::try_new(&self.metadata.id, references, None, true).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing worker history terminal page is invalid",
            )
        })
    }

    async fn read_answer_history_task(
        &self,
        context: &ProviderContext,
        request: &AnswerHistoryTaskRequest,
    ) -> ProviderResult<ProviderAnswerHistoryTaskEvidence> {
        if self.metadata.id.as_str() != "chaoxing" {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "answer history bootstrap is only exposed by the Chaoxing worker",
            ));
        }
        let cache_key = (
            context.account_id.to_string(),
            request.reference.remote_task_id.clone(),
        );
        let cached = self
            .history_questions
            .read()
            .ok()
            .and_then(|cache| cache.get(&cache_key).cloned());
        let rows = match cached {
            Some(rows) => rows,
            None => {
                self.question_rows(context, &request.reference.remote_task_id)
                    .await?
            }
        };
        let mut questions = Vec::new();
        let mut evidence = Vec::new();
        for row in rows.iter().filter(|row| usable_history_row(row)) {
            let reference = worker_question_reference(row)?;
            let question = self
                .parse_question(
                    context,
                    request.task_id,
                    &request.reference.remote_task_id,
                    &reference,
                )
                .await?;
            let facts = row.get("answer_evidence").and_then(Value::as_object);
            let official_answer = facts
                .and_then(|value| value.get("official_value"))
                .and_then(Value::as_str)
                .and_then(|value| normalize_reviewed_answer_for_question(&question, value))
                .filter(|answer| history_answer_matches_question(&question, answer));
            let submitted_answer = facts
                .and_then(|value| value.get("submitted_value"))
                .and_then(Value::as_str)
                .and_then(|value| normalize_reviewed_answer_for_question(&question, value))
                .filter(|answer| history_answer_matches_question(&question, answer));
            let submitted_answer_correct = submitted_answer.as_ref().and_then(|_| {
                facts
                    .and_then(|value| value.get("submitted_correct"))
                    .and_then(Value::as_bool)
            });
            if official_answer.is_some() || submitted_answer.is_some() {
                evidence.push(AnswerHistoryQuestionEvidence {
                    question_id: question.id,
                    submitted_answer,
                    official_answer,
                    submitted_answer_correct,
                    provenance_sanitized: json!({
                        "schema": "chaoxing.worker-history-question.v1",
                        "source": "reviewed_result_page",
                        "read_only": true,
                    }),
                });
            }
            questions.push(question);
        }
        if questions.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing reviewed task no longer exposes usable answer evidence",
            ));
        }
        let encoded_rows = serde_json::to_string(&rows).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing reviewed evidence could not be digested",
            )
        })?;
        let result = ProviderAnswerHistoryTaskEvidence {
            task_id: request.task_id,
            provider_attempt_digest: request.reference.provider_attempt_digest,
            result_digest: history_digest(&[
                "chaoxing.worker-history-result.v2",
                &request.reference.remote_task_id,
                &encoded_rows,
            ]),
            questions,
            question_evidence: evidence,
            score: None,
            retake: None,
            provenance_sanitized: json!({
                "schema": "chaoxing.worker-history-task.v1",
                "source_family": "completed_worker_task",
                "read_only": true,
            }),
            observed_at: chrono::Utc::now(),
        };
        result.validate(request).map_err(|error| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!("Chaoxing worker history evidence violates the shared contract: {error}"),
            )
        })?;
        Ok(result)
    }
}

#[async_trait]
impl DurationReadCapability for UpstreamWorkerProvider {
    async fn read_duration(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteDuration> {
        if self.metadata.id.as_str() != "uai" {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "this upstream worker does not expose duration reads",
            ));
        }
        let native = self.task_native(context, remote_task_id).await?;
        let worker_remote_id = required_string(&native, "_asterism_worker_remote_id")?;
        let result = self
            .invoke(
                "duration",
                json!({
                    "session": self.session(context).await?,
                    "task": {"remote_id": worker_remote_id, "native": native},
                }),
            )
            .await?;
        let duration_seconds = result
            .get("duration_seconds")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "worker duration result did not contain nonnegative seconds",
                )
            })?;
        Ok(RemoteDuration {
            duration_seconds,
            updated_at: chrono::Utc::now(),
        })
    }
}

impl UpstreamWorkerProvider {
    #[allow(
        clippy::cast_precision_loss,
        clippy::too_many_lines,
        reason = "bounded decimal-millis settings and Worker event mapping stay next to donor dispatch"
    )]
    async fn execute_worker(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        answers: Value,
        assessment_mode: Option<&str>,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        let combined_resource_duration = matches!(
            request.requested_capabilities.as_slice(),
            [
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport
            ]
        ) && self.metadata.id.as_str() == "welearn";
        let capability = match request.requested_capabilities.as_slice() {
            [TaskCapability::ResourceExecution] => TaskCapability::ResourceExecution,
            [TaskCapability::DurationReport] if self.metadata.id.as_str() == "welearn" => {
                TaskCapability::DurationReport
            }
            [
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ] if self.metadata.id.as_str() == "welearn" => TaskCapability::ResourceExecution,
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "upstream worker execution accepts one run or duration action",
                ));
            }
        };
        let native = self.task_native(context, &request.remote_task_id).await?;
        let worker_remote_id = required_string(&native, "_asterism_worker_remote_id")?;
        let mut settings = match capability {
            TaskCapability::DurationReport => json!({
                "action": "duration",
                "duration_seconds": request.runtime_settings
                    .duration_seconds("worker.duration_seconds")
                    .unwrap_or(60),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "welearn" => json!({
                "action": "complete",
                "correctness": request.runtime_settings
                    .integer("worker.correctness")
                    .unwrap_or(100),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "chaoxing" => json!({
                "speed": request.runtime_settings
                    .decimal_millis("worker.playback_rate")
                    .unwrap_or(2_000) as f64 / 1_000.0,
                "minimum_answer_coverage": request.runtime_settings
                    .decimal_millis("worker.minimum_answer_coverage")
                    .unwrap_or(900) as f64 / 1_000.0,
                "video_threads": request.runtime_settings
                    .integer("worker.video_threads").unwrap_or(4),
                "answer_threads": request.runtime_settings
                    .integer("worker.answer_threads").unwrap_or(1),
                "other_threads": request.runtime_settings
                    .integer("worker.other_threads").unwrap_or(4),
                "challenge_retry_attempts": request.runtime_settings
                    .integer("worker.challenge_retry_attempts").unwrap_or(3),
                "challenge_escalation_route": request.runtime_settings
                    .choice("worker.challenge_escalation_route").unwrap_or("sol_xhigh"),
                "verification_attempt_budget": request.runtime_settings
                    .integer("worker.verification_attempt_budget").unwrap_or(3),
                "verification_time_budget_seconds": request.runtime_settings
                    .duration_seconds("worker.verification_time_budget_seconds").unwrap_or(90),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "uai" => json!({
                "cooldown_count": request.runtime_settings
                    .integer("worker.cooldown_count").unwrap_or(5),
                "cooldown_seconds": request.runtime_settings
                    .duration_seconds("worker.cooldown_seconds").unwrap_or(120),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "cidaren" => json!({
                "spend_min_time": request.runtime_settings
                    .duration_seconds("worker.spend_min_time").unwrap_or(1),
                "spend_max_time": request.runtime_settings
                    .duration_seconds("worker.spend_max_time").unwrap_or(2),
                "answer_route": request.runtime_settings
                    .choice("worker.answer_route").unwrap_or("timed"),
                "instant_timeout_seconds": request.runtime_settings
                    .duration_seconds("worker.instant_timeout_seconds").unwrap_or(8),
                "instant_fallback_grace_seconds": request.runtime_settings
                    .duration_seconds("worker.instant_fallback_grace_seconds").unwrap_or(2),
                "answer_bridge_url": Some(std::env::var("ASTERISM_CIDAREN_ANSWER_BRIDGE_URL").unwrap_or_else(|_| "http://127.0.0.1:8068/api/v1/internal/cidaren/answer-bridge".to_owned())),
                "answer_bridge_ticket": cidaren_answer_bridge_ticket(),
                "answer_bridge_expires_at": chrono::Utc::now().timestamp().saturating_add(300),
                "execution_id": request.execution_id.to_string(),
                "task_id": request.task_id.to_string(),
                "remote_task_id": request.remote_task_id,
            }),
            TaskCapability::ResourceExecution => json!({}),
            _ => unreachable!("capability was validated above"),
        };
        if self.metadata.id.as_str() == "chaoxing" {
            if let (Some(mode), Some(settings)) = (assessment_mode, settings.as_object_mut()) {
                settings.insert("assessment_mode".to_owned(), Value::String(mode.to_owned()));
            }
        } else if self.metadata.id.as_str() == "uai" {
            if let (Some(text), Some(settings)) = (assessment_mode, settings.as_object_mut()) {
                settings.insert("generated_text".to_owned(), Value::String(text.to_owned()));
            }
        }
        events
            .report(ProviderProgress {
                percent: Some(0),
                stage: "upstream_worker".to_owned(),
                status_text: Some("正在调用上游执行逻辑".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        events
            .log(ProviderExecutionLog {
                level: LogLevel::Info,
                stage: "upstream_worker".to_owned(),
                message: "已启动 Provider 原生任务流程".to_owned(),
                provider_trace_id: None,
                metadata_sanitized: Some(json!({"worker_backed": true})),
            })
            .await?;
        let mut observed = self
            .invoke_run_observed(
                self.session(context).await?,
                &worker_remote_id,
                native,
                settings,
                answers,
            )
            .await?;
        if combined_resource_duration {
            let duration_observed = self
                .invoke_observed(
                    "run",
                    json!({
                        "session": self.session(context).await?,
                        "task": {"remote_id": worker_remote_id, "native": self.task_native(context, &request.remote_task_id).await?},
                        "settings": {
                            "action": "duration",
                            "duration_seconds": request.runtime_settings
                                .duration_seconds("worker.duration_seconds")
                                .unwrap_or(60),
                        },
                        "answers": [],
                    }),
                )
                .await?;
            observed.logs.extend(duration_observed.logs);
            observed.progress.extend(duration_observed.progress);
            if let (Some(base), Some(duration)) = (
                observed.data.as_object_mut(),
                duration_observed.data.as_object(),
            ) {
                for (key, value) in duration {
                    base.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
        for log in observed.logs {
            let Some(message) = clean_text(&log.message, 8 * 1024) else {
                continue;
            };
            let level = match log.level.as_str() {
                "error" => LogLevel::Error,
                "warning" | "warn" => LogLevel::Warn,
                "debug" => LogLevel::Debug,
                _ => LogLevel::Info,
            };
            events
                .log(ProviderExecutionLog {
                    level,
                    stage: "upstream_worker".to_owned(),
                    message,
                    provider_trace_id: None,
                    metadata_sanitized: Some(json!({"worker_backed": true})),
                })
                .await?;
        }
        for progress in observed.progress {
            let completed_items = u32::try_from(progress.current).unwrap_or(u32::MAX);
            let total_items = progress
                .total
                .map(|total| u32::try_from(total).unwrap_or(u32::MAX));
            let completed_items =
                total_items.map_or(completed_items, |total| completed_items.min(total));
            let percent = progress.total.and_then(|total| {
                (total > 0).then(|| progress.current.saturating_mul(100) / total)
            });
            events
                .report(ProviderProgress {
                    percent: percent.map(|value| value.min(100) as u8),
                    stage: "upstream_worker".to_owned(),
                    status_text: Some("Provider 原生流程正在执行".to_owned()),
                    completed_items: Some(completed_items),
                    total_items,
                })
                .await?;
        }
        let result = observed.data;
        let remote_state = map_state(result.get("remote_state"));
        let verified = result
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let result_sanitized = result.get("result").cloned().unwrap_or_else(|| json!({}));
        let outcome = ExecutionOutcome {
            remote_state,
            verified,
            result_sanitized,
        };
        outcome.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker execution result was not sanitized",
            )
        })?;
        events
            .report(ProviderProgress {
                percent: Some(100),
                stage: "upstream_worker".to_owned(),
                status_text: Some(if verified {
                    "上游已执行并重新读取完成状态".to_owned()
                } else {
                    "上游已接受执行，等待后续扫描确认".to_owned()
                }),
                completed_items: Some(1),
                total_items: Some(1),
            })
            .await?;
        Ok(outcome)
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    reason = "the thin Worker execution boundary keeps private validation and donor dispatch together"
)]
#[async_trait]
impl TaskExecutionCapability for UpstreamWorkerProvider {
    fn requires_execution_verification(&self, _requested_capabilities: &[TaskCapability]) -> bool {
        false
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Chaoxing answer binding validates one complete private invocation boundary"
    )]
    async fn prepare_private_invocation(
        &self,
        context: &ProviderContext,
        request: &ExecutionInvocationPreparationRequest<'_>,
    ) -> ProviderResult<PreparedProviderExecutionInvocation> {
        if self.metadata.id.as_str() == "uai" {
            if request.input_type != UAI_GENERATED_TEXT_INPUT_TYPE
                || request.requested_capabilities != [TaskCapability::ResourceExecution]
                || request.submission_draft.is_some()
            {
                return Err(invalid_uai_generated_text(
                    "UAI generated-text invocation is invalid",
                ));
            }
            let generated_text = decode_uai_generated_text(request.raw_input)?;
            let native = self.task_native(context, request.remote_task_id).await?;
            if native.get("category").and_then(Value::as_str) != Some("discussion") {
                return Err(invalid_uai_generated_text(
                    "UAI generated text is only accepted for a freshly bound discussion task",
                ));
            }
            let private_input = ProviderExecutionPrivateInput::try_new(
                self.metadata.id.clone(),
                UAI_GENERATED_TEXT_INPUT_TYPE,
                SecretValue::new(request.raw_input.expose_secret().to_vec()),
            )?;
            let artifact = ProviderExecutionPlanArtifact::try_new(
                self.metadata.id.clone(),
                UAI_GENERATED_TEXT_PLAN_TYPE,
                json!({
                    "schema": UAI_GENERATED_TEXT_PLAN_TYPE,
                    "task_id": request.task_id.to_string(),
                    "remote_task_id": request.remote_task_id,
                    "character_count": generated_text.chars().count(),
                    "private_input_digest": encode_digest(private_input.input_digest()),
                }),
            )?;
            return PreparedProviderExecutionInvocation::try_new(artifact, private_input);
        }
        let is_chaoxing = self.metadata.id.as_str() == "chaoxing";
        let is_cidaren = self.metadata.id.as_str() == "cidaren";
        let expected_input = if is_chaoxing {
            CHAOXING_ANSWERS_INPUT_TYPE
        } else if is_cidaren {
            CIDAREN_ANSWERS_INPUT_TYPE
        } else {
            ""
        };
        if (!is_chaoxing && !is_cidaren)
            || request.input_type != expected_input
            || request.requested_capabilities != [TaskCapability::ResourceExecution]
            || request.submission_draft.is_some()
        {
            return Err(invalid_worker_answers(
                "Chaoxing reviewed-answer invocation is invalid",
            ));
        }
        let invocation = decode_worker_answers(request.raw_input)?;
        if is_cidaren && invocation.mode == "save" {
            return Err(invalid_worker_answers(
                "Cidaren donor does not expose a save-only assessment operation",
            ));
        }
        let supplied = invocation
            .answers
            .iter()
            .filter_map(|row| row.get("remote_id").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        let questions = self.question_rows(context, request.remote_task_id).await?;
        let expected = questions
            .iter()
            .map(|row| required_string(row, "remote_id"))
            .collect::<ProviderResult<BTreeSet<_>>>()?;
        let coverage_millis = supplied.len().saturating_mul(1_000) / expected.len().max(1);
        let minimum_coverage_millis = request
            .runtime_settings
            .decimal_millis("worker.minimum_answer_coverage")
            .unwrap_or(900)
            .clamp(0, 1_000) as usize;
        let save_only = invocation.mode == "save";
        if expected.is_empty()
            || supplied.is_empty()
            || supplied.len() > expected.len()
            || !supplied
                .iter()
                .all(|remote_id| expected.contains(*remote_id))
            || (!save_only && coverage_millis < minimum_coverage_millis)
        {
            return Err(invalid_worker_answers(
                "Chaoxing reviewed answers do not meet the fresh question coverage policy",
            ));
        }
        let private_input = ProviderExecutionPrivateInput::try_new(
            self.metadata.id.clone(),
            expected_input,
            SecretValue::new(request.raw_input.expose_secret().to_vec()),
        )?;
        let input_digest = encode_digest(private_input.input_digest());
        let artifact = ProviderExecutionPlanArtifact::try_new(
            self.metadata.id.clone(),
            if is_chaoxing {
                CHAOXING_ANSWERS_PLAN_TYPE
            } else {
                CIDAREN_ANSWERS_PLAN_TYPE
            },
            json!({
                "schema": if is_chaoxing { CHAOXING_ANSWERS_PLAN_TYPE } else { CIDAREN_ANSWERS_PLAN_TYPE },
                "task_id": request.task_id.to_string(),
                "remote_task_id": request.remote_task_id,
                "question_count": expected.len(),
                "answered_question_count": supplied.len(),
                "answer_coverage_millis": coverage_millis,
                "minimum_answer_coverage_millis": minimum_coverage_millis,
                "assessment_mode": invocation.mode,
                "private_input_digest": input_digest,
            }),
        )?;
        PreparedProviderExecutionInvocation::try_new(artifact, private_input)
    }

    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        let capability = match request.requested_capabilities.as_slice() {
            [TaskCapability::ResourceExecution] => TaskCapability::ResourceExecution,
            [TaskCapability::DurationReport]
                if matches!(self.metadata.id.as_str(), "welearn" | "uai") =>
            {
                TaskCapability::DurationReport
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "upstream worker execution accepts one run or duration action",
                ));
            }
        };
        let native = self.task_native(context, &request.remote_task_id).await?;
        let worker_remote_id = required_string(&native, "_asterism_worker_remote_id")?;
        let settings = match capability {
            TaskCapability::DurationReport => json!({
                "action": "duration",
                "duration_seconds": request.runtime_settings
                    .duration_seconds("worker.duration_seconds")
                    .unwrap_or(60),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "welearn" => json!({
                "action": "complete",
                "correctness": request.runtime_settings
                    .integer("worker.correctness")
                    .unwrap_or(100),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "chaoxing" => json!({
                "speed": request.runtime_settings
                    .decimal_millis("worker.playback_rate")
                    .unwrap_or(2_000) as f64 / 1_000.0,
                "minimum_answer_coverage": request.runtime_settings
                    .decimal_millis("worker.minimum_answer_coverage")
                    .unwrap_or(900) as f64 / 1_000.0,
                "video_threads": request.runtime_settings
                    .integer("worker.video_threads").unwrap_or(4),
                "answer_threads": request.runtime_settings
                    .integer("worker.answer_threads").unwrap_or(1),
                "other_threads": request.runtime_settings
                    .integer("worker.other_threads").unwrap_or(4),
                "challenge_retry_attempts": request.runtime_settings
                    .integer("worker.challenge_retry_attempts").unwrap_or(3),
                "challenge_escalation_route": request.runtime_settings
                    .choice("worker.challenge_escalation_route").unwrap_or("sol_xhigh"),
            }),
            TaskCapability::ResourceExecution if self.metadata.id.as_str() == "cidaren" => json!({
                "spend_min_time": request.runtime_settings
                    .duration_seconds("worker.spend_min_time").unwrap_or(1),
                "spend_max_time": request.runtime_settings
                    .duration_seconds("worker.spend_max_time").unwrap_or(2),
                "answer_route": request.runtime_settings
                    .choice("worker.answer_route").unwrap_or("timed"),
                "instant_timeout_seconds": request.runtime_settings
                    .duration_seconds("worker.instant_timeout_seconds").unwrap_or(8),
                "instant_fallback_grace_seconds": request.runtime_settings
                    .duration_seconds("worker.instant_fallback_grace_seconds").unwrap_or(2),
                "answer_bridge_url": Some(std::env::var("ASTERISM_CIDAREN_ANSWER_BRIDGE_URL").unwrap_or_else(|_| "http://127.0.0.1:8068/api/v1/internal/cidaren/answer-bridge".to_owned())),
                "answer_bridge_ticket": cidaren_answer_bridge_ticket(),
                "answer_bridge_expires_at": chrono::Utc::now().timestamp().saturating_add(300),
                "execution_id": request.execution_id.to_string(),
                "task_id": request.task_id.to_string(),
                "remote_task_id": request.remote_task_id,
            }),
            TaskCapability::ResourceExecution => json!({}),
            _ => unreachable!("capability was validated above"),
        };
        events
            .report(ProviderProgress {
                percent: Some(0),
                stage: "upstream_worker".to_owned(),
                status_text: Some("正在调用上游执行逻辑".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        events
            .log(ProviderExecutionLog {
                level: LogLevel::Info,
                stage: "upstream_worker".to_owned(),
                message: "已启动 Provider 原生任务流程".to_owned(),
                provider_trace_id: None,
                metadata_sanitized: Some(json!({"worker_backed": true})),
            })
            .await?;
        let observed = self
            .invoke_run_observed(
                self.session(context).await?,
                &worker_remote_id,
                native,
                settings,
                json!([]),
            )
            .await?;
        for log in observed.logs {
            let Some(message) = clean_text(&log.message, 8 * 1024) else {
                continue;
            };
            let level = match log.level.as_str() {
                "error" => LogLevel::Error,
                "warning" | "warn" => LogLevel::Warn,
                "debug" => LogLevel::Debug,
                _ => LogLevel::Info,
            };
            events
                .log(ProviderExecutionLog {
                    level,
                    stage: "upstream_worker".to_owned(),
                    message,
                    provider_trace_id: None,
                    metadata_sanitized: Some(json!({"worker_backed": true})),
                })
                .await?;
        }
        for progress in observed.progress {
            let completed_items = u32::try_from(progress.current).unwrap_or(u32::MAX);
            let total_items = progress
                .total
                .map(|total| u32::try_from(total).unwrap_or(u32::MAX));
            let completed_items =
                total_items.map_or(completed_items, |total| completed_items.min(total));
            let percent = progress.total.and_then(|total| {
                (total > 0).then(|| progress.current.saturating_mul(100) / total)
            });
            events
                .report(ProviderProgress {
                    percent: percent.map(|value| value.min(100) as u8),
                    stage: "upstream_worker".to_owned(),
                    status_text: Some("Provider 原生流程正在执行".to_owned()),
                    completed_items: Some(completed_items),
                    total_items,
                })
                .await?;
        }
        let result = observed.data;
        let remote_state = map_state(result.get("remote_state"));
        let verified = result
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let result_sanitized = result.get("result").cloned().unwrap_or_else(|| json!({}));
        let outcome = ExecutionOutcome {
            remote_state,
            verified,
            result_sanitized,
        };
        outcome.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker execution result was not sanitized",
            )
        })?;
        events
            .report(ProviderProgress {
                percent: Some(100),
                stage: "upstream_worker".to_owned(),
                status_text: Some(if verified {
                    "上游已执行并重新读取完成状态".to_owned()
                } else {
                    "上游已接受执行，等待后续扫描确认".to_owned()
                }),
                completed_items: Some(1),
                total_items: Some(1),
            })
            .await?;
        Ok(outcome)
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::too_many_lines,
        reason = "private answer validation and the original Worker execution remain one boundary"
    )]
    async fn execute_private_invocation(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if self.metadata.id.as_str() == "uai" {
            if private_input.provider_id() != &self.metadata.id
                || private_input.input_type() != UAI_GENERATED_TEXT_INPUT_TYPE
                || request.requested_capabilities != [TaskCapability::ResourceExecution]
            {
                return Err(invalid_uai_generated_text(
                    "UAI generated-text execution binding is invalid",
                ));
            }
            let artifact = request.provider_plan_artifact.as_ref().ok_or_else(|| {
                invalid_uai_generated_text("UAI generated-text plan artifact is missing")
            })?;
            let task_id = request.task_id.to_string();
            let digest = encode_digest(private_input.input_digest());
            if artifact.provider_id() != &self.metadata.id
                || artifact.artifact_type() != UAI_GENERATED_TEXT_PLAN_TYPE
                || artifact
                    .payload_sanitized()
                    .get("task_id")
                    .and_then(Value::as_str)
                    != Some(task_id.as_str())
                || artifact
                    .payload_sanitized()
                    .get("remote_task_id")
                    .and_then(Value::as_str)
                    != Some(request.remote_task_id.as_str())
                || artifact
                    .payload_sanitized()
                    .get("private_input_digest")
                    .and_then(Value::as_str)
                    != Some(digest.as_str())
            {
                return Err(invalid_uai_generated_text(
                    "UAI generated-text plan binding changed",
                ));
            }
            let generated_text = decode_uai_generated_text(private_input.value())?;
            return self
                .execute_worker(context, request, json!([]), Some(&generated_text), events)
                .await;
        }
        let is_chaoxing = self.metadata.id.as_str() == "chaoxing";
        let is_cidaren = self.metadata.id.as_str() == "cidaren";
        let expected_input = if is_chaoxing {
            CHAOXING_ANSWERS_INPUT_TYPE
        } else if is_cidaren {
            CIDAREN_ANSWERS_INPUT_TYPE
        } else {
            ""
        };
        if (!is_chaoxing && !is_cidaren)
            || private_input.provider_id() != &self.metadata.id
            || private_input.input_type() != expected_input
            || request.requested_capabilities != [TaskCapability::ResourceExecution]
        {
            return Err(invalid_worker_answers(
                "Chaoxing reviewed-answer execution binding is invalid",
            ));
        }
        let artifact = request.provider_plan_artifact.as_ref().ok_or_else(|| {
            invalid_worker_answers("Chaoxing reviewed-answer plan artifact is missing")
        })?;
        let digest = encode_digest(private_input.input_digest());
        if artifact.provider_id() != &self.metadata.id
            || artifact.artifact_type()
                != if is_chaoxing {
                    CHAOXING_ANSWERS_PLAN_TYPE
                } else {
                    CIDAREN_ANSWERS_PLAN_TYPE
                }
            || artifact
                .payload_sanitized()
                .get("task_id")
                .and_then(Value::as_str)
                != Some(request.task_id.to_string().as_str())
            || artifact
                .payload_sanitized()
                .get("remote_task_id")
                .and_then(Value::as_str)
                != Some(request.remote_task_id.as_str())
            || artifact
                .payload_sanitized()
                .get("private_input_digest")
                .and_then(Value::as_str)
                != Some(digest.as_str())
        {
            return Err(invalid_worker_answers(
                "Chaoxing reviewed-answer plan binding changed",
            ));
        }
        let invocation = decode_worker_answers(private_input.value())?;
        self.execute_worker(
            context,
            request,
            Value::Array(invocation.answers),
            is_chaoxing.then_some(invocation.mode.as_str()),
            events,
        )
        .await
    }
}

fn invalid_worker_answers(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

fn invalid_uai_generated_text(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

fn decode_uai_generated_text(value: &SecretValue) -> ProviderResult<String> {
    let text = std::str::from_utf8(value.expose_secret())
        .map_err(|_| invalid_uai_generated_text("UAI generated text is not UTF-8"))?
        .trim();
    if text.is_empty()
        || text.len() > 16 * 1024
        || text.starts_with("```")
        || text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(invalid_uai_generated_text(
            "UAI generated text is empty, formatted, or oversized",
        ));
    }
    Ok(text.to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChaoxingWorkerAnswerInvocation {
    answers: Vec<Value>,
    mode: String,
}

fn decode_worker_answers(value: &SecretValue) -> ProviderResult<ChaoxingWorkerAnswerInvocation> {
    let root: Value = serde_json::from_slice(value.expose_secret())
        .map_err(|_| invalid_worker_answers("Chaoxing reviewed answers are not valid JSON"))?;
    let object = root
        .as_object()
        .ok_or_else(|| invalid_worker_answers("Chaoxing reviewed answers must be an object"))?;
    if !(object.len() == 1 || object.len() == 2)
        || !object.contains_key("answers")
        || object.keys().any(|key| key != "answers" && key != "mode")
    {
        return Err(invalid_worker_answers(
            "Chaoxing reviewed answers contain unknown fields",
        ));
    }
    let rows = object
        .get("answers")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty() && rows.len() <= 4096)
        .ok_or_else(|| {
            invalid_worker_answers("Chaoxing reviewed answers are empty or oversized")
        })?;
    let mut identities = BTreeSet::new();
    for row in rows {
        let row = row
            .as_object()
            .filter(|row| {
                row.len() == 2 && row.contains_key("remote_id") && row.contains_key("value")
            })
            .ok_or_else(|| invalid_worker_answers("Chaoxing reviewed answer row is invalid"))?;
        let remote_id = row
            .get("remote_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && value.len() <= 512)
            .ok_or_else(|| invalid_worker_answers("Chaoxing question identity is invalid"))?;
        if !identities.insert(remote_id) || !valid_worker_answer_value(&row["value"], 0) {
            return Err(invalid_worker_answers(
                "Chaoxing reviewed answer value or identity is invalid",
            ));
        }
    }
    let mode = object
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("submit");
    if !matches!(mode, "save" | "submit") {
        return Err(invalid_worker_answers(
            "Chaoxing assessment mode must be save or submit",
        ));
    }
    Ok(ChaoxingWorkerAnswerInvocation {
        answers: rows.clone(),
        mode: mode.to_owned(),
    })
}

fn valid_worker_answer_value(value: &Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(value) => !value.trim().is_empty() && value.len() <= 64 * 1024,
        Value::Array(values) => {
            !values.is_empty()
                && values.len() <= 512
                && values
                    .iter()
                    .all(|value| valid_worker_answer_value(value, depth + 1))
        }
        Value::Object(values) => {
            !values.is_empty()
                && values.len() <= 512
                && values.iter().all(|(key, value)| {
                    !key.trim().is_empty()
                        && key.len() <= 512
                        && valid_worker_answer_value(value, depth + 1)
                })
        }
    }
}

fn field_text(bundle: &CredentialBundle, purpose: SecretPurpose) -> ProviderResult<&str> {
    let field = bundle
        .fields
        .iter()
        .find(|field| field.purpose == purpose)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "worker credential field is missing",
            )
        })?;
    std::str::from_utf8(field.value.expose_secret()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "worker credential field is invalid",
        )
    })
}

fn field_json(bundle: &CredentialBundle, purpose: SecretPurpose) -> ProviderResult<Value> {
    let field = bundle
        .fields
        .iter()
        .find(|field| field.purpose == purpose)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "worker credential field is missing",
            )
        })?;
    serde_json::from_slice(field.value.expose_secret()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "worker credential field is invalid",
        )
    })
}

fn encode_digest(value: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in value {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_digest(value: &str) -> ProviderResult<[u8; 32]> {
    if value.len() != 64 || !value.is_ascii() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "worker OAuth binding digest is invalid",
        ));
    }
    let mut result = [0_u8; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker OAuth binding digest is invalid",
            )
        })?;
    }
    Ok(result)
}

fn resolved_text(
    fields: &[ResolvedProviderCredential],
    purpose: SecretPurpose,
) -> ProviderResult<&str> {
    let field = fields
        .iter()
        .find(|field| field.credential.secret.purpose == purpose)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "worker credential field is missing",
            )
        })?;
    std::str::from_utf8(field.value.expose_secret()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "worker credential field is invalid",
        )
    })
}

fn resolved_json(
    fields: &[ResolvedProviderCredential],
    purpose: SecretPurpose,
) -> ProviderResult<Value> {
    let field = fields
        .iter()
        .find(|field| field.credential.secret.purpose == purpose)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "worker credential field is missing",
            )
        })?;
    serde_json::from_slice(field.value.expose_secret()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "worker composite credential is invalid",
        )
    })
}

fn cookie_header_from_session(session: &Value) -> ProviderResult<String> {
    let cookies = session
        .get("cookies")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker password login returned no Cookie jar",
            )
        })?;
    let mut pairs = cookies
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .filter(|value| !name.is_empty() && !value.is_empty())
                .map(|value| format!("{name}={value}"))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker password login returned an invalid Cookie jar",
            )
        })?;
    if pairs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "worker password login returned an empty Cookie jar",
        ));
    }
    pairs.sort_unstable();
    Ok(pairs.join("; "))
}

fn required_string(value: &Value, key: &str) -> ProviderResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned())
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "worker result is missing an identity field",
            )
        })
}
fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}
fn clean_text(value: &str, maximum_bytes: usize) -> Option<String> {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= maximum_bytes {
        return Some(trimmed.to_owned());
    }
    let mut boundary = maximum_bytes;
    while !trimmed.is_char_boundary(boundary) {
        boundary -= 1;
    }
    Some(trimmed[..boundary].to_owned())
}

fn option_identity_and_content(raw: &str, index: usize) -> (String, Option<String>) {
    let trimmed = raw.trim();
    let mut parts = trimmed.splitn(2, |character: char| {
        matches!(character, '.' | '．' | '、' | ':' | '：' | ')' | '）')
    });
    let prefix = parts.next().unwrap_or_default().trim();
    let remainder = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let provider_id = if remainder.is_some()
        && !prefix.is_empty()
        && prefix.len() <= 8
        && prefix
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        prefix.to_ascii_uppercase()
    } else {
        (index + 1).to_string()
    };
    (
        provider_id,
        clean_text(remainder.unwrap_or(trimmed), 32 * 1024),
    )
}

fn worker_question_options(values: &[Value]) -> Vec<QuestionOption> {
    let mut used_ids = BTreeSet::new();
    values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let (raw_provider_id, content) = if let Some(object) = value.as_object() {
                (
                    object
                        .get("id")
                        .or_else(|| object.get("key"))
                        .map_or_else(|| (index + 1).to_string(), value_text),
                    object
                        .get("content")
                        .or_else(|| object.get("text"))
                        .or_else(|| object.get("value"))
                        .map(value_text),
                )
            } else {
                let raw = value_text(value);
                option_identity_and_content(&raw, index)
            };
            let content = content.and_then(|value| clean_text(&value, 32 * 1024))?;
            let provider_id = clean_text(&raw_provider_id, 512);
            let base_id = provider_id
                .clone()
                .unwrap_or_else(|| format!("option-{}", index + 1));
            let mut internal_id = base_id.clone();
            let mut duplicate = 2usize;
            while !used_ids.insert(internal_id.clone()) {
                let suffix = format!("#{duplicate}");
                let keep = 512usize.saturating_sub(suffix.len());
                let truncated =
                    clean_text(&base_id, keep).unwrap_or_else(|| format!("option-{}", index + 1));
                internal_id = format!("{truncated}{suffix}");
                duplicate += 1;
            }
            Some(QuestionOption {
                id: internal_id,
                attachments: rich_attachments(&content),
                metadata_sanitized: json!({"provider_option_id": provider_id}),
                content: Some(content),
            })
        })
        .collect()
}

fn rich_attachments(text: &str) -> Vec<QuestionAttachment> {
    const MARKERS: [(&str, QuestionAttachmentKind, &str); 5] = [
        (
            "[QUESTION_IMAGE:",
            QuestionAttachmentKind::Image,
            "question image",
        ),
        (
            "[QUESTION_AUDIO:",
            QuestionAttachmentKind::Audio,
            "question audio",
        ),
        (
            "[QUESTION_VIDEO:",
            QuestionAttachmentKind::Video,
            "question video",
        ),
        (
            "[QUESTION_FILE:",
            QuestionAttachmentKind::File,
            "question file",
        ),
        (
            "[QUESTION_FORMULA:",
            QuestionAttachmentKind::Formula,
            "question formula",
        ),
    ];
    let mut attachments = Vec::new();
    let mut remaining = text;
    while attachments.len() < 64 {
        let Some((start, prefix, kind, fallback_label)) = MARKERS
            .iter()
            .filter_map(|(prefix, kind, label)| {
                remaining
                    .find(prefix)
                    .map(|start| (start, *prefix, *kind, *label))
            })
            .min_by_key(|(start, _, _, _)| *start)
        else {
            break;
        };
        let value = &remaining[start + prefix.len()..];
        let Some(end) = value.find(']') else {
            break;
        };
        let encoded = value[..end].trim();
        let (reference, visible_label) = encoded
            .split_once('|')
            .map_or((encoded, fallback_label), |(reference, visible)| {
                (reference.trim(), visible.trim())
            });
        let remote_id =
            (reference != "embedded" && reference.len() <= 512).then(|| reference.to_owned());
        let label =
            clean_text(visible_label, 32 * 1024).or_else(|| Some(fallback_label.to_owned()));
        if !encoded.is_empty()
            && !attachments.iter().any(|item: &QuestionAttachment| {
                item.kind == kind && item.remote_id == remote_id && item.label == label
            })
        {
            attachments.push(QuestionAttachment {
                kind,
                remote_id,
                label,
                metadata_sanitized: json!({"source": "worker_question_dom"}),
            });
        }
        remaining = &value[end + 1..];
    }
    attachments
}

fn worker_question_reference(row: &Value) -> ProviderResult<RemoteQuestionRef> {
    let remote_id = required_string(row, "remote_id")?;
    let position = row
        .get("position")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);
    Ok(RemoteQuestionRef {
        remote_id: remote_id.clone(),
        position,
        kind_hint: map_kind(row.get("kind")),
        metadata_sanitized: json!({
            "remote_id": remote_id,
            "position": position,
            "kind": row.get("kind").cloned().unwrap_or(Value::Null),
            "prompt": row.get("prompt").cloned().unwrap_or(Value::Null),
            "options": row.get("options").cloned().unwrap_or_else(|| json!([])),
            "native_shape": row.get("native_shape").cloned().unwrap_or(Value::Null),
            "worker_backed": true,
        }),
        route_context: ProviderRouteContext::default(),
    })
}

fn usable_history_row(row: &Value) -> bool {
    let kind = map_kind(row.get("kind"));
    let Some(facts) = row.get("answer_evidence").and_then(Value::as_object) else {
        return false;
    };
    ["official_value", "submitted_value", "value"]
        .into_iter()
        .filter_map(|field| facts.get(field).and_then(Value::as_str))
        .any(|value| normalize_reviewed_answer(kind, value).is_some())
}

struct WorkerHistoryCursor {
    course_remote_id: String,
    task_remote_id: String,
}

fn parse_worker_history_cursor(
    cursor: Option<&AnswerHistoryCursor>,
    provider_id: &asterism_domain::ProviderId,
) -> ProviderResult<Option<WorkerHistoryCursor>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    cursor.validate(provider_id).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing worker history cursor is invalid",
        )
    })?;
    let value = cursor.value_sanitized.as_object();
    let course_remote_id = value
        .and_then(|value| value.get("course_remote_id"))
        .and_then(Value::as_str)
        .and_then(|value| clean_text(value, 512));
    let task_remote_id = value
        .and_then(|value| value.get("task_remote_id"))
        .and_then(Value::as_str)
        .and_then(|value| clean_text(value, 512));
    if cursor.version != 2
        || cursor.cursor_type != "chaoxing.worker-history.v2"
        || value.is_none_or(|value| value.len() != 2)
        || course_remote_id.is_none()
        || task_remote_id.is_none()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing worker history cursor changed",
        ));
    }
    Ok(Some(WorkerHistoryCursor {
        course_remote_id: course_remote_id.expect("validated above"),
        task_remote_id: task_remote_id.expect("validated above"),
    }))
}

fn history_digest(parts: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(part.as_bytes());
    }
    digest.finalize().into()
}

fn map_state(value: Option<&Value>) -> RemoteState {
    match value.and_then(Value::as_str) {
        Some("completed") => RemoteState::Completed,
        Some("pending") => RemoteState::Pending,
        Some("in_progress" | "submitted") => RemoteState::InProgress,
        Some("not_open") => RemoteState::NotOpen,
        Some("expired") => RemoteState::Expired,
        Some("removed") => RemoteState::Removed,
        _ => RemoteState::Unknown,
    }
}
fn map_source_type(value: Option<&Value>) -> SourceType {
    match value.and_then(Value::as_str) {
        Some("chapter") => SourceType::Chapter,
        Some("work") => SourceType::Work,
        Some("exam") => SourceType::Exam,
        Some("resource") => SourceType::Resource,
        Some("practice") => SourceType::Practice,
        Some("discussion") => SourceType::Discussion,
        _ => SourceType::Other,
    }
}
fn map_assessment_class(value: Option<&Value>) -> AssessmentClass {
    match value.and_then(Value::as_str) {
        Some("routine") => AssessmentClass::Routine,
        Some("formal") => AssessmentClass::Formal,
        _ => AssessmentClass::Unknown,
    }
}
fn map_kind(value: Option<&Value>) -> QuestionKind {
    match value.and_then(Value::as_str) {
        Some("single_choice") => QuestionKind::SingleChoice,
        Some("multiple_choice") => QuestionKind::MultipleChoice,
        Some("true_false") => QuestionKind::TrueFalse,
        Some("fill_blank") => QuestionKind::FillBlank,
        Some("short_answer") => QuestionKind::ShortAnswer,
        Some("matching") => QuestionKind::Matching,
        Some("ordering") => QuestionKind::Ordering,
        _ => QuestionKind::Unknown,
    }
}

fn normalize_reviewed_answer(kind: QuestionKind, value: &str) -> Option<NormalizedAnswer> {
    let value = clean_text(value, 16 * 1024)?;
    match kind {
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => {
            let compact = value
                .chars()
                .filter(|character| !character.is_whitespace() && !",，;；、#".contains(*character))
                .collect::<String>();
            let selections = if !compact.is_empty()
                && compact.len() <= 26
                && compact.bytes().all(|byte| byte.is_ascii_alphabetic())
            {
                compact
                    .chars()
                    .map(|character| character.to_ascii_uppercase().to_string())
                    .collect::<Vec<_>>()
            } else {
                value
                    .split([',', '，', ';', '；', '、', '#'])
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            (!selections.is_empty()).then_some(NormalizedAnswer::Selections(selections))
        }
        QuestionKind::TrueFalse => {
            let normalized = value.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "正确" | "对" | "true" | "1" | "√") {
                Some(NormalizedAnswer::Boolean(true))
            } else if matches!(normalized.as_str(), "错误" | "错" | "false" | "0" | "×") {
                Some(NormalizedAnswer::Boolean(false))
            } else {
                None
            }
        }
        QuestionKind::FillBlank | QuestionKind::ShortAnswer => {
            let texts = value
                .split('#')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            (!texts.is_empty()).then_some(NormalizedAnswer::Texts(texts))
        }
        QuestionKind::Matching => {
            let pairs = value
                .split_whitespace()
                .filter_map(|part| part.split_once('-'))
                .map(|(left, right)| AnswerPair {
                    left: left.trim().to_owned(),
                    right: right.trim().to_owned(),
                })
                .filter(|pair| !pair.left.is_empty() && !pair.right.is_empty())
                .collect::<Vec<_>>();
            (!pairs.is_empty()).then_some(NormalizedAnswer::Pairs(pairs))
        }
        QuestionKind::Ordering => {
            let ordering = value
                .split([',', '，', ';', '；', '、', ' ', '>'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            (!ordering.is_empty()).then_some(NormalizedAnswer::Ordering(ordering))
        }
        QuestionKind::Composite | QuestionKind::Unknown => None,
    }
}

fn normalize_reviewed_answer_for_question(
    question: &Question,
    value: &str,
) -> Option<NormalizedAnswer> {
    let raw = clean_text(value, 16 * 1024)?;
    let direct_content_matches = question
        .options
        .iter()
        .filter(|option| option.content.as_deref() == Some(raw.as_str()))
        .collect::<Vec<_>>();
    if direct_content_matches.len() == 1 {
        return Some(NormalizedAnswer::Selections(vec![
            direct_content_matches[0].id.clone(),
        ]));
    }
    let normalized = normalize_reviewed_answer(question.kind, value)?;
    let NormalizedAnswer::Selections(values) = normalized else {
        return Some(normalized);
    };
    let mut rebound = Vec::with_capacity(values.len());
    for value in values {
        if let Some(option) = question.options.iter().find(|option| option.id == value) {
            rebound.push(option.id.clone());
            continue;
        }
        if value.len() == 1 && value.as_bytes()[0].is_ascii_uppercase() {
            let index = usize::from(value.as_bytes()[0] - b'A');
            if let Some(option) = question.options.get(index) {
                rebound.push(option.id.clone());
                continue;
            }
        }
        let content_matches = question
            .options
            .iter()
            .filter(|option| option.content.as_deref() == Some(value.as_str()))
            .collect::<Vec<_>>();
        if content_matches.len() != 1 {
            return None;
        }
        rebound.push(content_matches[0].id.clone());
    }
    Some(NormalizedAnswer::Selections(rebound))
}

fn history_answer_matches_question(question: &Question, answer: &NormalizedAnswer) -> bool {
    if answer.validate().is_err()
        || matches!(answer, NormalizedAnswer::Unknown | NormalizedAnswer::Skip)
    {
        return false;
    }
    match (question.kind, answer) {
        (
            QuestionKind::SingleChoice | QuestionKind::MultipleChoice,
            NormalizedAnswer::Selections(selected),
        ) => selected
            .iter()
            .all(|id| question.options.iter().any(|option| option.id == *id)),
        (QuestionKind::TrueFalse, NormalizedAnswer::Boolean(_))
        | (QuestionKind::FillBlank | QuestionKind::ShortAnswer, NormalizedAnswer::Texts(_))
        | (QuestionKind::Matching, NormalizedAnswer::Pairs(_))
        | (QuestionKind::Ordering, NormalizedAnswer::Ordering(_))
        | (QuestionKind::Composite, NormalizedAnswer::Composite(_)) => true,
        _ => false,
    }
}

fn worker_runtime_settings_schema(
    provider_id: &asterism_domain::ProviderId,
) -> ProviderRuntimeSettingsSchema {
    let scopes = BTreeSet::from([
        ProviderSettingScope::Provider,
        ProviderSettingScope::ProviderAccount,
        ProviderSettingScope::Task,
    ]);
    let definitions = match provider_id.as_str() {
        "chaoxing" => vec![
            ProviderSettingDefinition {
                key: "worker.playback_rate".to_owned(),
                display_name: "Video playback rate".to_owned(),
                description:
                    "Video-only multiplier passed to the audited Chaoxing donor; live stays 1x."
                        .to_owned(),
                kind: ProviderSettingKind::DecimalMillis {
                    minimum: 1,
                    maximum: i64::MAX,
                    step: 1,
                },
                default: ProviderSettingValue::DecimalMillis(2_000),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.minimum_answer_coverage".to_owned(),
                display_name: "Minimum answer coverage".to_owned(),
                description: "Minimum reviewed-answer fraction required for Chaoxing final submission; save-only may remain partial.".to_owned(),
                kind: ProviderSettingKind::DecimalMillis {
                    minimum: 0,
                    maximum: 1_000,
                    step: 1,
                },
                default: ProviderSettingValue::DecimalMillis(900),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.video_threads".to_owned(),
                display_name: "Video task threads".to_owned(),
                description: "Concurrent Chaoxing video execution points.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: i64::MAX,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(4),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.answer_threads".to_owned(),
                display_name: "Answer task threads".to_owned(),
                description: "Concurrent Chaoxing chapter-answer execution points.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: i64::MAX,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(1),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.other_threads".to_owned(),
                display_name: "Other task threads".to_owned(),
                description: "Concurrent Chaoxing document/read/other execution points.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: i64::MAX,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(4),
                scopes,
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.challenge_retry_attempts".to_owned(),
                display_name: "Challenge retry attempts".to_owned(),
                description: "Bounded retries for Chaoxing challenge-mode knowledge points before escalation.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 3,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(3),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.challenge_escalation_route".to_owned(),
                display_name: "Challenge escalation route".to_owned(),
                description: "Route marker emitted after bounded challenge retries; the answer service may escalate once to Sol xhigh.".to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from(["sol_xhigh".to_owned()]),
                },
                default: ProviderSettingValue::Choice("sol_xhigh".to_owned()),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.verification_attempt_budget".to_owned(),
                display_name: "Verification attempt budget".to_owned(),
                description: "Maximum automatic attempts for one Chaoxing image verification sequence.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 12,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(3),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.verification_time_budget_seconds".to_owned(),
                display_name: "Verification time budget".to_owned(),
                description: "Maximum elapsed time for one Chaoxing automatic verification sequence before bounded failure.".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 10,
                    maximum: 600,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(90),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            },
        ],
        "welearn" => vec![
            ProviderSettingDefinition {
                key: "worker.correctness".to_owned(),
                display_name: "Completion correctness".to_owned(),
                description: "Correctness percentage passed to the audited WELearn donor."
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 0,
                    maximum: 100,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(100),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.duration_seconds".to_owned(),
                display_name: "Duration seconds".to_owned(),
                description: "Real elapsed duration reported by the audited WELearn donor."
                    .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 1,
                    maximum: 86_400,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(60),
                scopes,
                core_behavior: None,
            },
        ],
        "uai" => vec![
            ProviderSettingDefinition {
                key: "worker.duration_seconds".to_owned(),
                display_name: "Course residence seconds".to_owned(),
                description:
                    "Rendered page-residence budget passed to the audited UAI userscript donor."
                        .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 60,
                    maximum: 3_600,
                    step: 60,
                },
                default: ProviderSettingValue::DurationSeconds(60),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.cooldown_count".to_owned(),
                display_name: "Submissions before cooldown".to_owned(),
                description: "Preserves the donor's per-account submission frequency control."
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: i64::MAX,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(5),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.cooldown_seconds".to_owned(),
                display_name: "Cooldown seconds".to_owned(),
                description: "Delay after each configured UAI submission group.".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 0,
                    maximum: u64::MAX,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(120),
                scopes,
                core_behavior: None,
            },
        ],
        "cidaren" => vec![
            ProviderSettingDefinition {
                key: "worker.answer_route".to_owned(),
                display_name: "Cidaren answer route".to_owned(),
                description: "Selects the Asterism AI route frozen for this Cidaren task; timed is the safe default for per-question deadlines.".to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        "timed".to_owned(), "untimed".to_owned(), "escalation".to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice("timed".to_owned()),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.instant_timeout_seconds".to_owned(),
                display_name: "Cidaren Instant timeout".to_owned(),
                description: "Maximum wait for the configured Instant model on a timed question.".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 1,
                    maximum: 300,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(8),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.instant_fallback_grace_seconds".to_owned(),
                display_name: "Cidaren fallback grace".to_owned(),
                description: "Bounded grace period before the donor fallback must answer to preserve completion.".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 0,
                    maximum: 120,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(2),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.spend_min_time".to_owned(),
                display_name: "Cidaren minimum answer delay".to_owned(),
                description: "Minimum donor delay between timed Cidaren questions.".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 0,
                    maximum: 3_600,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(1),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: "worker.spend_max_time".to_owned(),
                display_name: "Cidaren maximum answer delay".to_owned(),
                description: "Maximum donor delay between timed Cidaren questions.".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 0,
                    maximum: 3_600,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(2),
                scopes,
                core_behavior: None,
            },
        ],
        _ => Vec::new(),
    };
    ProviderRuntimeSettingsSchema {
        version: 1,
        definitions,
    }
}
fn fingerprint(provider: &asterism_domain::ProviderId, remote_id: &str, native: &Value) -> String {
    let digest =
        Sha256::digest(format!("{}\0{}\0{}", provider.as_str(), remote_id, native).as_bytes());
    format!("v1:{digest:x}")
}
fn scoped_task_remote_id(course_id: &str, worker_remote_id: &str) -> String {
    let scoped = format!("{course_id}:{worker_remote_id}");
    if scoped.len() <= 256 {
        scoped
    } else {
        let digest = Sha256::digest(scoped.as_bytes());
        format!("scoped:{digest:x}")
    }
}
fn worker_task_remote_id(course_id: &str, worker_remote_id: &str, global: bool) -> String {
    if global {
        worker_remote_id.to_owned()
    } else {
        scoped_task_remote_id(course_id, worker_remote_id)
    }
}
fn map_secret_error(error: &SecretStoreError) -> ProviderError {
    let kind = match error {
        SecretStoreError::NotFound
        | SecretStoreError::Unauthorized
        | SecretStoreError::AccountMismatch => ProviderErrorKind::Authentication,
        _ => ProviderErrorKind::Internal,
    };
    ProviderError::new(kind, format!("worker credential storage failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use asterism_domain::{
        ProviderAccountId, ProviderId, SecretId, SessionKind, Timestamp, UserId,
    };
    use asterism_provider_api::ProviderRegistry;
    use asterism_secrets::{
        CredentialAcquisition, ProviderCredential, ProviderCredentialRenewal,
        ProviderCredentialRenewer, ProviderCredentialResolution, ResolvedProviderCredential,
        SecretRef, SecretStoreError,
    };

    use super::*;

    #[derive(Debug)]
    struct NoCredentials;

    #[async_trait]
    impl ProviderCredentialResolver for NoCredentials {
        async fn resolve_provider_credentials(
            &self,
            _request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            Err(SecretStoreError::NotFound)
        }
    }

    #[derive(Debug)]
    struct RenewingCredentials {
        account_id: ProviderAccountId,
        owner_id: UserId,
        fields: Vec<(SecretId, SecretPurpose, &'static [u8])>,
        renewal: Mutex<Option<(usize, HashSet<SecretPurpose>)>>,
        renewed: AtomicBool,
    }

    impl RenewingCredentials {
        fn uai(account_id: ProviderAccountId) -> Self {
            Self {
                account_id,
                owner_id: UserId::new(),
                fields: vec![
                    (
                        SecretId::new(),
                        SecretPurpose::ProviderUsername,
                        b"fixture-user",
                    ),
                    (
                        SecretId::new(),
                        SecretPurpose::ProviderPassword,
                        b"fixture-password",
                    ),
                    (
                        SecretId::new(),
                        SecretPurpose::ProviderCompositeSession,
                        br#"{"open_id":"stale","authorization":"stale","cookies":[]}"#,
                    ),
                ],
                renewal: Mutex::new(None),
                renewed: AtomicBool::new(false),
            }
        }

        fn credential_refs(&self) -> Vec<SecretId> {
            self.fields.iter().map(|(id, _, _)| *id).collect()
        }
    }

    #[async_trait]
    impl ProviderCredentialResolver for RenewingCredentials {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            if self.renewed.load(Ordering::SeqCst) {
                return Err(SecretStoreError::AccountMismatch);
            }
            let now = Timestamp::default();
            Ok(self
                .fields
                .iter()
                .filter(|(_, purpose, _)| request.purposes.contains(purpose))
                .map(|(id, purpose, value)| ResolvedProviderCredential {
                    credential: ProviderCredential {
                        provider_account_id: self.account_id,
                        secret: SecretRef {
                            id: *id,
                            owner_user_id: self.owner_id,
                            purpose: *purpose,
                            version: 1,
                            key_id: "fixture-key".to_owned(),
                            created_at: now,
                            updated_at: now,
                        },
                        session_kind: SessionKind::Composite,
                        acquired_via: CredentialAcquisition::NativeProviderLogin,
                        captured_at: now,
                        expires_at: None,
                        updated_at: now,
                    },
                    value: SecretValue::new(value.to_vec()),
                })
                .collect())
        }
    }

    #[async_trait]
    impl ProviderCredentialRenewer for RenewingCredentials {
        async fn renew_provider_credentials(
            &self,
            request: ProviderCredentialRenewal,
        ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
            *self.renewal.lock().unwrap() = Some((
                request.expected_credentials.len(),
                request
                    .bundle
                    .fields
                    .iter()
                    .map(|field| field.purpose)
                    .collect(),
            ));
            self.renewed.store(true, Ordering::SeqCst);
            Ok(request.expected_credentials)
        }
    }

    fn workspace_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    fn fixture_entry() -> ProviderEntry {
        let client = UaiWorkerClient::new(
            "python",
            workspace_path("workers/uai/worker.py"),
            workspace_path("workers/uai/tests/fixtures/fake_upstream.py"),
        )
        .with_source_metadata(workspace_path(
            "workers/uai/tests/fixtures/fake_SOURCE.json",
        ));
        UpstreamWorkerProvider::entry(
            ProviderId::new("uai").unwrap(),
            "UAI fixture",
            client,
            Arc::new(NoCredentials),
            WorkerAuthProfile::Password,
        )
    }

    #[test]
    fn uai_entry_registers_worker_execution_without_submission_slots() {
        let entry = fixture_entry();
        assert!(entry.authentication.is_some());
        assert!(entry.course_inventory.is_some());
        assert!(entry.task_inventory.is_some());
        assert!(entry.task_detail.is_some());
        assert!(entry.question_inventory.is_none());
        assert!(entry.question_parse.is_none());
        assert!(entry.duration_read.is_some());
        assert!(entry.task_execution.is_some());
        assert!(entry.answer_resolve.is_none());
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::Discussion)
        );
        assert!(entry.submission_execute.is_none());
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
    }

    #[test]
    fn cidaren_entry_exposes_only_external_oauth_and_composite_sessions() {
        let client = UaiWorkerClient::new(
            "python",
            workspace_path("workers/cidaren/worker.py"),
            workspace_path("upstreams/cidaren/api/login.py"),
        )
        .with_source_metadata(workspace_path("workers/cidaren/SOURCE.json"));
        let entry = UpstreamWorkerProvider::entry(
            ProviderId::new("cidaren").unwrap(),
            "Cidaren fixture",
            client,
            Arc::new(NoCredentials),
            WorkerAuthProfile::ExternalOauthComposite,
        );
        assert_eq!(
            entry.metadata.auth_methods,
            BTreeSet::from([AuthMethod::ExternalBrowserOauth])
        );
        assert_eq!(
            entry.metadata.session_kinds,
            BTreeSet::from([SessionKind::Composite])
        );
        assert!(entry.task_execution.is_some());
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::ResourceExecution)
        );
        assert!(entry.browser_bridge.is_none());
        ProviderRegistry::default().register(entry).unwrap();
        let digest = [0xabu8; 32];
        assert_eq!(decode_digest(&encode_digest(digest)).unwrap(), digest);
    }

    #[test]
    fn permanent_worker_question_denial_is_not_marked_retryable() {
        let error = UpstreamWorkerProvider::map_client(
            "questions",
            &UaiWorkerClientError::Remote {
                code: "question_access_denied".to_owned(),
                message: "reviewed paper disabled".to_owned(),
            },
        );

        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(!error.is_retryable());
    }

    #[test]
    fn reviewed_answer_normalization_preserves_common_and_native_shapes() {
        assert_eq!(
            normalize_reviewed_answer(QuestionKind::MultipleChoice, "A、C"),
            Some(NormalizedAnswer::Selections(vec![
                "A".to_owned(),
                "C".to_owned()
            ]))
        );
        assert_eq!(
            normalize_reviewed_answer(QuestionKind::TrueFalse, "正确"),
            Some(NormalizedAnswer::Boolean(true))
        );
        assert_eq!(
            normalize_reviewed_answer(QuestionKind::Matching, "1-C 2-D"),
            Some(NormalizedAnswer::Pairs(vec![
                AnswerPair {
                    left: "1".to_owned(),
                    right: "C".to_owned()
                },
                AnswerPair {
                    left: "2".to_owned(),
                    right: "D".to_owned()
                },
            ]))
        );
        assert_eq!(
            normalize_reviewed_answer(QuestionKind::Composite, "opaque"),
            None
        );
    }

    #[test]
    fn reviewed_choice_labels_rebind_to_current_provider_option_ids() {
        let question = Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("history-choice".to_owned()),
            kind: QuestionKind::MultipleChoice,
            stem: "Choose".to_owned(),
            options: vec![
                QuestionOption {
                    id: "remote-17".to_owned(),
                    content: Some("First".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({"provider_option_id": "remote-17"}),
                },
                QuestionOption {
                    id: "remote-42".to_owned(),
                    content: Some("Second".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({"provider_option_id": "remote-42"}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: json!({}),
            position: 1,
        };
        assert_eq!(
            normalize_reviewed_answer_for_question(&question, "B"),
            Some(NormalizedAnswer::Selections(vec!["remote-42".to_owned()]))
        );
        assert_eq!(
            normalize_reviewed_answer_for_question(&question, "First"),
            Some(NormalizedAnswer::Selections(vec!["remote-17".to_owned()]))
        );
        assert!(!history_answer_matches_question(
            &question,
            &NormalizedAnswer::Selections(vec!["remote-17".to_owned(), "remote-17".to_owned()])
        ));
    }

    #[test]
    fn worker_question_options_keep_duplicate_provider_ids_as_private_metadata() {
        let options = worker_question_options(&[
            json!({"id": "A", "content": "First"}),
            json!({"id": "A", "content": "Second"}),
            json!({"id": "", "content": "Third"}),
        ]);

        assert_eq!(options.len(), 3);
        assert_eq!(options[0].id, "A");
        assert_eq!(options[1].id, "A#2");
        assert_eq!(options[2].id, "option-3");
        assert_eq!(options[0].metadata_sanitized["provider_option_id"], "A");
        assert_eq!(options[1].metadata_sanitized["provider_option_id"], "A");
        assert!(options[2].metadata_sanitized["provider_option_id"].is_null());
        let question = Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: None,
            kind: QuestionKind::SingleChoice,
            stem: "Choose".to_owned(),
            options,
            attachments: Vec::new(),
            metadata_sanitized: json!({}),
            position: 1,
        };
        question.validate().unwrap();
    }

    #[test]
    fn rich_worker_markers_become_typed_attachments() {
        let attachments = rich_attachments(
            "A [QUESTION_IMAGE:https://example.test/a.png] B [QUESTION_AUDIO:https://example.test/a.mp3] C [QUESTION_FORMULA:x+y] D [QUESTION_FILE:https://example.test/a.pdf|paper]",
        );
        assert_eq!(attachments.len(), 4);
        assert_eq!(attachments[0].kind, QuestionAttachmentKind::Image);
        assert_eq!(attachments[1].kind, QuestionAttachmentKind::Audio);
        assert_eq!(attachments[2].kind, QuestionAttachmentKind::Formula);
        assert_eq!(attachments[2].remote_id.as_deref(), Some("x+y"));
        assert_eq!(attachments[3].kind, QuestionAttachmentKind::File);
        assert_eq!(attachments[3].label.as_deref(), Some("paper"));
    }

    #[test]
    fn uai_generated_text_accepts_plain_human_content_only() {
        assert_eq!(
            decode_uai_generated_text(&SecretValue::new(
                "A concise discussion reply.".as_bytes().to_vec()
            ))
            .unwrap(),
            "A concise discussion reply."
        );
        assert!(
            decode_uai_generated_text(&SecretValue::new(b"```markdown\nautomated\n```".to_vec()))
                .is_err()
        );
    }

    #[test]
    fn chaoxing_option_labels_remain_submission_identities() {
        assert_eq!(
            option_identity_and_content("A. alpha", 0),
            ("A".to_owned(), Some("alpha".to_owned()))
        );
        assert_eq!(
            option_identity_and_content("plain option", 1),
            ("2".to_owned(), Some("plain option".to_owned()))
        );
    }

    #[test]
    fn reviewed_history_requires_a_normalizable_answer() {
        assert!(usable_history_row(&json!({
            "kind": "single_choice",
            "answer_evidence": {"official_value": "B"}
        })));
        assert!(!usable_history_row(&json!({
            "kind": "provider_native",
            "answer_evidence": {"official_value": "opaque"}
        })));
    }

    #[test]
    fn chaoxing_private_answers_are_bounded_and_exactly_shaped() {
        let input = SecretValue::new(
            serde_json::to_vec(&json!({
                "answers": [
                    {"remote_id": "q-1", "value": "A"},
                    {"remote_id": "q-2", "value": ["B", "D"]},
                    {"remote_id": "q-3", "value": {"1": "C", "2": "A"}},
                ]
            }))
            .unwrap(),
        );
        let decoded = decode_worker_answers(&input).unwrap();
        assert_eq!(decoded.answers.len(), 3);
        assert_eq!(decoded.mode, "submit");
        let save = SecretValue::new(
            br#"{"answers":[{"remote_id":"q-1","value":"A"}],"mode":"save"}"#.to_vec(),
        );
        assert_eq!(decode_worker_answers(&save).unwrap().mode, "save");
        assert!(
            decode_worker_answers(&SecretValue::new(
                br#"{"answers":[{"remote_id":"q-1","value":"A"},{"remote_id":"q-1","value":"B"}]}"#
                    .to_vec(),
            ))
            .is_err()
        );
        assert!(
            decode_worker_answers(&SecretValue::new(
                br#"{"answers":[{"remote_id":"q-1","value":null}],"unsafe":true}"#.to_vec(),
            ))
            .is_err()
        );
    }

    #[test]
    fn chaoxing_runtime_schema_exposes_partial_coverage_policy() {
        let provider = ProviderId::new("chaoxing").unwrap();
        let schema = worker_runtime_settings_schema(&provider);
        let setting = schema
            .definitions
            .iter()
            .find(|definition| definition.key == "worker.minimum_answer_coverage")
            .unwrap();
        assert_eq!(setting.default, ProviderSettingValue::DecimalMillis(900));
        assert_eq!(
            setting.kind,
            ProviderSettingKind::DecimalMillis {
                minimum: 0,
                maximum: 1_000,
                step: 1,
            }
        );
    }

    #[test]
    fn chaoxing_runtime_schema_exposes_bounded_challenge_escalation() {
        let provider = ProviderId::new("chaoxing").unwrap();
        let schema = worker_runtime_settings_schema(&provider);
        let retries = schema
            .definitions
            .iter()
            .find(|definition| definition.key == "worker.challenge_retry_attempts")
            .unwrap();
        assert_eq!(retries.default, ProviderSettingValue::Integer(3));
        assert_eq!(
            retries.kind,
            ProviderSettingKind::Integer {
                minimum: 1,
                maximum: 3,
                step: 1,
            }
        );
        let route = schema
            .definitions
            .iter()
            .find(|definition| definition.key == "worker.challenge_escalation_route")
            .unwrap();
        assert_eq!(
            route.default,
            ProviderSettingValue::Choice("sol_xhigh".to_owned())
        );
    }

    #[test]
    fn chaoxing_runtime_schema_exposes_bounded_verification_policy() {
        let provider = ProviderId::new("chaoxing").unwrap();
        let schema = worker_runtime_settings_schema(&provider);
        let value = |key: &str| {
            schema
                .definitions
                .iter()
                .find(|definition| definition.key == key)
                .unwrap()
        };
        assert_eq!(
            value("worker.verification_attempt_budget").kind,
            ProviderSettingKind::Integer {
                minimum: 1,
                maximum: 12,
                step: 1,
            }
        );
        assert_eq!(
            value("worker.verification_time_budget_seconds").default,
            ProviderSettingValue::DurationSeconds(90)
        );
    }

    #[test]
    fn cidaren_runtime_schema_freezes_delay_and_answer_budgets() {
        let provider = ProviderId::new("cidaren").unwrap();
        let schema = worker_runtime_settings_schema(&provider);
        let value = |key: &str| {
            schema
                .definitions
                .iter()
                .find(|definition| definition.key == key)
                .unwrap()
                .default
                .clone()
        };
        assert_eq!(
            value("worker.answer_route"),
            ProviderSettingValue::Choice("timed".to_owned())
        );
        assert_eq!(
            value("worker.instant_timeout_seconds"),
            ProviderSettingValue::DurationSeconds(8)
        );
        assert_eq!(
            value("worker.instant_fallback_grace_seconds"),
            ProviderSettingValue::DurationSeconds(2)
        );
        schema.validate().unwrap();
    }

    #[test]
    fn worker_inventory_identity_and_text_fit_core_bounds() {
        let provider = ProviderId::new("uai").unwrap();
        let scoped = scoped_task_remote_id("course-a", "task-a");
        assert_eq!(scoped, "course-a:task-a");
        assert_eq!(
            worker_task_remote_id("course-a", "global-task", true),
            "global-task"
        );
        assert!(scoped_task_remote_id(&"课".repeat(200), &"题".repeat(200)).len() <= 256);
        assert!(fingerprint(&provider, &scoped, &json!({"shape": 1})).starts_with("v1:"));
        assert_eq!(
            clean_text("  line\nvalue  ", 64).as_deref(),
            Some("line value")
        );
        assert_eq!(clean_text(" \n\t ", 64), None);
    }

    #[tokio::test]
    async fn password_login_is_delegated_and_returns_opaque_composite_session() {
        let entry = fixture_entry();
        let credential = CredentialBundle {
            provider_id: ProviderId::new("uai").unwrap(),
            tenant: None,
            auth_method: AuthMethod::Password,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: chrono::Utc::now(),
            expires_at: None,
            session_kind: SessionKind::ProviderSpecific,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderUsername,
                    value: SecretValue::new(b"fixture-user".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderPassword,
                    value: SecretValue::new(b"fixture-password".to_vec()),
                },
            ],
            user_id_hint: None,
        };
        let validation = entry
            .authentication
            .unwrap()
            .validate_credential(
                &ProviderAuthContext {
                    provider_id: ProviderId::new("uai").unwrap(),
                    account_id: ProviderAccountId::new(),
                    auth_session_id: None,
                    correlation_id: "fixture-login".to_owned(),
                },
                &credential,
            )
            .await
            .unwrap();
        assert!(validation.status.valid);
        let replacement = validation.replacement.unwrap();
        assert_eq!(replacement.session_kind, SessionKind::Composite);
        assert_eq!(replacement.fields.len(), 3);
        assert_eq!(
            replacement
                .fields
                .iter()
                .map(|field| field.purpose)
                .collect::<HashSet<_>>(),
            HashSet::from([
                SecretPurpose::ProviderUsername,
                SecretPurpose::ProviderPassword,
                SecretPurpose::ProviderCompositeSession,
            ])
        );
        let session_field = replacement
            .fields
            .iter()
            .find(|field| field.purpose == SecretPurpose::ProviderCompositeSession)
            .unwrap();
        let session: Value = serde_json::from_slice(session_field.value.expose_secret()).unwrap();
        assert_eq!(session["open_id"], "open-1");
    }

    #[tokio::test]
    async fn password_runtime_login_atomically_renews_the_complete_session_bundle() {
        let account_id = ProviderAccountId::new();
        let credentials = Arc::new(RenewingCredentials::uai(account_id));
        let context = ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id,
            credential_refs: credentials.credential_refs(),
            correlation_id: "fixture-runtime-renewal".to_owned(),
        };
        let client = UaiWorkerClient::new(
            "python",
            workspace_path("workers/uai/worker.py"),
            workspace_path("workers/uai/tests/fixtures/fake_upstream.py"),
        )
        .with_source_metadata(workspace_path(
            "workers/uai/tests/fixtures/fake_SOURCE.json",
        ));
        let entry = UpstreamWorkerProvider::entry_with_renewal(
            ProviderId::new("uai").unwrap(),
            "UAI renewal fixture",
            client,
            credentials.clone(),
            credentials.clone(),
            WorkerAuthProfile::Password,
        );

        let courses = entry
            .course_inventory
            .as_ref()
            .unwrap()
            .list_courses(&context)
            .await
            .unwrap();
        let cached_courses = entry
            .course_inventory
            .as_ref()
            .unwrap()
            .list_courses(&context)
            .await
            .unwrap();

        assert_eq!(courses.len(), 1);
        assert_eq!(cached_courses.len(), 1);
        assert_eq!(
            *credentials.renewal.lock().unwrap(),
            Some((
                3,
                HashSet::from([
                    SecretPurpose::ProviderUsername,
                    SecretPurpose::ProviderPassword,
                    SecretPurpose::ProviderCompositeSession,
                ]),
            ))
        );
    }
}
