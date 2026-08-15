use std::{fmt, str::FromStr, sync::Arc};

use asterism_domain::{
    AuthMethod, AuthSession, AuthSessionId, AuthState, ExternalOauthState, ProviderAccount,
    ProviderAccountId, ProviderId, SessionKind, Timestamp, UserId,
};
use asterism_engine::{
    AuthSessionCredentialRequest, AuthSessionService, AuthSessionServiceError,
    AuthSessionStartRequest, CredentialProvisionError, ExternalOauthCallbackRequest,
    ProviderCredentialService, ProviderScanError, ProviderScanService,
};
use asterism_provider_api::{
    ProviderCapability, ProviderEntry, ProviderError, ProviderErrorKind, SessionStatus,
};
use asterism_scheduler::{ScanSchedule, ScanScheduleError};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, CredentialField, SecretAccess, SecretPurpose,
    SecretStoreError, SecretString, SecretValue,
};
use asterism_storage::{
    AccountHealthRepository, AuthSessionRepository, ProviderAccountRepository,
    ProviderAccountRuntimeRepository, ProviderRuntimeSettingsRepository,
    ProviderRuntimeSettingsTarget, ScanScheduleRepository, SqliteAuthSessionRepository,
    SqliteProtocolObservationRepository, SqliteProviderAccountRepository,
    SqliteProviderRuntimeSettingsRepository, SqliteProviderScanRepository,
    SqliteSchedulerRepository, StorageError,
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, ListResponse, auth::AuthContext};

const AUTH_SESSION_TTL_SECONDS: i64 = 10 * 60;

pub(super) async fn list_provider_accounts(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_read()?;
    let accounts = SqliteProviderAccountRepository::new(state.database)
        .list_provider_accounts(owner_id)
        .await
        .map_err(ApiError::internal)?;
    let items: Vec<_> = accounts.iter().map(ProviderAccountResponse::from).collect();
    Ok(crate::auth::no_store(
        Json(ListResponse {
            total: items.len(),
            items,
        })
        .into_response(),
    ))
}

pub(super) async fn get_provider_account(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_read()?;
    let account_id = parse_account_id(&account_id)?;
    let account = SqliteProviderAccountRepository::new(state.database)
        .find_provider_account(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    Ok(crate::auth::no_store(
        Json(ProviderAccountResponse::from(&account)).into_response(),
    ))
}

pub(super) async fn get_provider_account_health(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_read()?;
    let account_id = parse_account_id(&account_id)?;
    let health = SqliteProviderAccountRepository::new(state.database)
        .find_owned_account_health(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    Ok(crate::auth::no_store(Json(health).into_response()))
}

pub(super) async fn create_provider_account(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<CreateProviderAccountRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let request = api_json(payload)?;
    let provider_id = ProviderId::new(request.provider_id)
        .map_err(|error| ApiError::bad_request("invalid_provider_id", error.to_string()))?;
    let (display_name, tenant) = validate_mutable_fields(&request.display_name, request.tenant)?;
    let now = Utc::now();
    let account = ProviderAccount {
        id: ProviderAccountId::new(),
        owner_id,
        provider_id,
        display_name,
        tenant,
        auth_state: AuthState::Idle,
        network_profile_id: None,
        credential_refs: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    SqliteProviderAccountRepository::new(state.database)
        .create_provider_account(&account, auth.audit_actor())
        .await
        .map_err(map_storage_error)?;
    Ok(crate::auth::no_store(
        (
            StatusCode::CREATED,
            Json(ProviderAccountResponse::from(&account)),
        )
            .into_response(),
    ))
}

pub(super) async fn update_provider_account(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    payload: Result<Json<UpdateProviderAccountRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let request = api_json(payload)?;
    let (display_name, tenant) = validate_mutable_fields(&request.display_name, request.tenant)?;
    let repository = SqliteProviderAccountRepository::new(state.database);
    let mut account = repository
        .find_provider_account(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    account.display_name = display_name;
    account.tenant = tenant;
    account.updated_at = Utc::now();
    let updated = repository
        .update_provider_account(&account, auth.audit_actor())
        .await
        .map_err(map_storage_error)?;
    if !updated {
        return Err(ApiError::not_found("provider_account_not_found"));
    }
    Ok(crate::auth::no_store(
        Json(ProviderAccountResponse::from(&account)).into_response(),
    ))
}

pub(super) async fn delete_provider_account(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let deleted = SqliteProviderAccountRepository::new(state.database)
        .delete_provider_account(owner_id, account_id, Utc::now(), auth.audit_actor())
        .await
        .map_err(ApiError::internal)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::not_found("provider_account_not_found"))
    }
}

pub(super) async fn put_provider_credentials(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<PutProviderCredentialsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let request = api_json(payload)?;
    let accounts = SqliteProviderAccountRepository::new(state.database.clone());
    let account = accounts
        .find_provider_account(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    let secret_store = state.secret_store.ok_or_else(|| {
        ApiError::service_unavailable(
            "secret_store_unavailable",
            "the encrypted credential store is not configured",
        )
    })?;
    let correlation_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))?;
    let access = SecretAccess {
        actor: auth.secret_actor(),
        correlation_id: correlation_id.to_owned(),
        reason: "replace Provider account credentials".to_owned(),
    };
    let bundle = credential_bundle(account, request);
    let committed = ProviderCredentialService::new(state.providers, accounts, secret_store)
        .validate_and_store(owner_id, account_id, bundle, &access)
        .await
        .map_err(map_credential_error)?;
    Ok(crate::auth::no_store(
        Json(PutProviderCredentialsResponse {
            credential_count: committed.credentials.len(),
            status: committed.status,
        })
        .into_response(),
    ))
}

pub(super) async fn put_auth_session_credentials(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<PutProviderCredentialsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let session_id = parse_auth_session_id(&session_id)?;
    let request = api_json(payload)?;
    let accounts = SqliteProviderAccountRepository::new(state.database.clone());
    let account = accounts
        .find_provider_account(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    let secret_store = state.secret_store.ok_or_else(|| {
        ApiError::service_unavailable(
            "secret_store_unavailable",
            "the encrypted credential store is not configured",
        )
    })?;
    let access = SecretAccess {
        actor: auth.secret_actor(),
        correlation_id: request_id(&headers)?.to_owned(),
        reason: "complete Provider authentication session".to_owned(),
    };
    let committed = AuthSessionService::new(
        state.providers,
        accounts,
        SqliteAuthSessionRepository::new(state.database),
    )
    .submit_credentials(
        &secret_store,
        AuthSessionCredentialRequest {
            owner_user_id: owner_id,
            provider_account_id: account_id,
            session_id,
            bundle: credential_bundle(account, request),
            access,
        },
    )
    .await
    .map_err(map_auth_session_error)?;
    Ok(crate::auth::no_store(
        Json(PutAuthSessionCredentialsResponse {
            session: committed.session,
            credential_count: committed.credentials.len(),
            status: committed.status,
        })
        .into_response(),
    ))
}

pub(super) async fn begin_auth_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<BeginAuthSessionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let request = api_json(payload)?;
    let correlation_id = request_id(&headers)?;
    let now = Utc::now();
    let started = AuthSessionService::new(
        state.providers,
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteAuthSessionRepository::new(state.database),
    )
    .begin(AuthSessionStartRequest {
        owner_user_id: owner_id,
        provider_account_id: account_id,
        method: request.method,
        created_at: now,
        expires_at: now + Duration::seconds(AUTH_SESSION_TTL_SECONDS),
        actor: auth.audit_actor(),
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_auth_session_error)?;
    Ok(crate::auth::no_store(
        (
            StatusCode::CREATED,
            Json(AuthSessionBeginResponse {
                session: started.session,
                challenge: started.challenge,
            }),
        )
            .into_response(),
    ))
}

pub(super) async fn get_latest_auth_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_read()?;
    let account_id = parse_account_id(&account_id)?;
    let session = SqliteAuthSessionRepository::new(state.database)
        .find_latest_account_auth_session(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("auth_session_not_found"))?;
    Ok(crate::auth::no_store(Json(session).into_response()))
}

pub(super) async fn get_auth_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, session_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_read()?;
    let account_id = parse_account_id(&account_id)?;
    let session_id = parse_auth_session_id(&session_id)?;
    let session = SqliteAuthSessionRepository::new(state.database)
        .find_auth_session(owner_id, session_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|session| session.provider_account_id == account_id)
        .ok_or_else(|| ApiError::not_found("auth_session_not_found"))?;
    Ok(crate::auth::no_store(Json(session).into_response()))
}

pub(super) async fn get_external_oauth_pending(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_read()?;
    let account_id = parse_account_id(&account_id)?;
    let session_id = parse_auth_session_id(&session_id)?;
    let pending = AuthSessionService::new(
        state.providers,
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteAuthSessionRepository::new(state.database),
    )
    .recover_external_oauth_pending(
        owner_id,
        account_id,
        session_id,
        auth.audit_actor(),
        request_id(&headers)?,
        Utc::now(),
    )
    .await
    .map_err(map_auth_session_error)?
    .ok_or_else(|| ApiError::not_found("external_oauth_pending_not_found"))?;
    Ok(crate::auth::no_store(
        Json(ExternalOauthPendingResponse {
            auth_session_id: pending.auth_session_id,
            state: pending.state,
            expires_at: pending.expires_at,
            consumed_at: pending.consumed_at,
            revision: pending.revision,
        })
        .into_response(),
    ))
}

pub(super) async fn submit_external_oauth_callback(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<SubmitExternalOauthCallbackRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let session_id = parse_auth_session_id(&session_id)?;
    let request = api_json(payload)?;
    let secret_store = state.secret_store.ok_or_else(|| {
        ApiError::service_unavailable(
            "secret_store_unavailable",
            "the encrypted credential store is not configured",
        )
    })?;
    let access = SecretAccess {
        actor: auth.secret_actor(),
        correlation_id: request_id(&headers)?.to_owned(),
        reason: "complete external OAuth authentication session".to_owned(),
    };
    let committed = AuthSessionService::new(
        state.providers,
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteAuthSessionRepository::new(state.database),
    )
    .submit_external_oauth_callback(
        &secret_store,
        ExternalOauthCallbackRequest {
            owner_user_id: owner_id,
            provider_account_id: account_id,
            session_id,
            callback_url: request.into_secret(),
            access,
        },
    )
    .await
    .map_err(map_auth_session_error)?;
    Ok(crate::auth::no_store(
        Json(PutAuthSessionCredentialsResponse {
            session: committed.session,
            credential_count: committed.credentials.len(),
            status: committed.status,
        })
        .into_response(),
    ))
}

pub(super) async fn cancel_auth_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let session_id = parse_auth_session_id(&session_id)?;
    let repository = SqliteAuthSessionRepository::new(state.database.clone());
    let session = repository
        .find_auth_session(owner_id, session_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|session| session.provider_account_id == account_id)
        .ok_or_else(|| ApiError::not_found("auth_session_not_found"))?;
    let cancelled = AuthSessionService::new(
        state.providers,
        SqliteProviderAccountRepository::new(state.database),
        repository,
    )
    .cancel(
        owner_id,
        session.id,
        auth.audit_actor(),
        request_id(&headers)?,
        Utc::now(),
    )
    .await
    .map_err(map_auth_session_error)?;
    Ok(crate::auth::no_store(Json(cancelled).into_response()))
}

pub(super) async fn scan_provider_account(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_account_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let account = SqliteProviderAccountRepository::new(state.database.clone())
        .find_provider_account(owner_id, account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    let correlation_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))?;
    let report = ProviderScanService::new(
        state.providers,
        SqliteProviderScanRepository::new(state.database.clone()),
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
    .scan_account(
        &account,
        correlation_id,
        Some(auth.audit_actor()),
        Utc::now(),
    )
    .await
    .map_err(map_scan_error)?;
    Ok(crate::auth::no_store(Json(report).into_response()))
}

pub(super) async fn get_scan_schedule(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let authority = auth.require_provider_settings_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let accounts = SqliteProviderAccountRepository::new(state.database.clone());
    let account = accounts
        .find_runtime_provider_account(account_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|account| authority.permits_owner(account.owner_id))
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    let schedule = SqliteSchedulerRepository::new(state.database.clone())
        .find_scan_schedule(account_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("scan_schedule_not_found"))?;
    let provider_min_interval_seconds = state
        .providers
        .get(&account.provider_id)
        .and_then(|entry| entry.metadata.scan_min_interval_seconds);
    Ok(crate::auth::no_store(
        Json(ScanScheduleResponse::from_schedule(
            &schedule,
            provider_min_interval_seconds,
        ))
        .into_response(),
    ))
}

pub(super) async fn configure_scan_schedule(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<ConfigureScanScheduleRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let authority = auth.require_provider_settings_manage()?;
    let account_id = parse_account_id(&account_id)?;
    let request = api_json(payload)?;
    let accounts = SqliteProviderAccountRepository::new(state.database.clone());
    let account = accounts
        .find_runtime_provider_account(account_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|account| authority.permits_owner(account.owner_id))
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    let provider = state.providers.get(&account.provider_id).ok_or_else(|| {
        ApiError::conflict(
            "provider_not_registered",
            "the account Provider is not registered",
        )
    })?;
    if request.enabled && !matches!(account.auth_state, AuthState::Authenticated) {
        return Err(ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before enabling scans",
        ));
    }
    if !provider
        .metadata
        .advertises(ProviderCapability::CourseInventory)
        && !provider
            .metadata
            .advertises(ProviderCapability::TaskInventory)
    {
        return Err(ApiError::conflict(
            "provider_inventory_unavailable",
            "the Provider exposes no inventory capability",
        ));
    }
    let provider_min_interval_seconds = provider.metadata.scan_min_interval_seconds;
    let desired_interval_seconds = match request.desired_interval_seconds {
        Some(interval) => interval,
        None => resolve_account_scan_default(&state, provider, account.id).await?,
    };
    let repository = SqliteSchedulerRepository::new(state.database);
    let existing = repository
        .find_scan_schedule(account_id)
        .await
        .map_err(ApiError::internal)?;
    let now = Utc::now();
    let schedule = ScanSchedule::configured(
        account_id,
        desired_interval_seconds,
        provider_min_interval_seconds,
        request.enabled,
        now,
        existing.as_ref(),
    )
    .map_err(map_scan_schedule_error)?;
    let correlation_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))?;
    let stored = repository
        .upsert_scan_schedule_for_owner(
            account.owner_id,
            &schedule,
            auth.audit_actor(),
            correlation_id,
        )
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
    Ok(crate::auth::no_store(
        Json(ScanScheduleResponse::from_schedule(
            &stored,
            provider_min_interval_seconds,
        ))
        .into_response(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateProviderAccountRequest {
    provider_id: String,
    display_name: String,
    tenant: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateProviderAccountRequest {
    display_name: String,
    tenant: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BeginAuthSessionRequest {
    method: AuthMethod,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct AuthSessionBeginResponse {
    session: AuthSession,
    challenge: asterism_provider_api::AuthChallenge,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct ExternalOauthPendingResponse {
    auth_session_id: AuthSessionId,
    state: ExternalOauthState,
    expires_at: Timestamp,
    consumed_at: Option<Timestamp>,
    revision: u32,
}

pub(super) struct SubmitExternalOauthCallbackRequest {
    callback_url: String,
}

impl SubmitExternalOauthCallbackRequest {
    fn into_secret(mut self) -> SecretString {
        SecretString::new(std::mem::take(&mut self.callback_url))
    }
}

impl fmt::Debug for SubmitExternalOauthCallbackRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubmitExternalOauthCallbackRequest")
            .field("callback_url", &"[REDACTED]")
            .finish()
    }
}

impl<'de> Deserialize<'de> for SubmitExternalOauthCallbackRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            callback_url: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.callback_url.is_empty()
            || wire.callback_url.len() > 8 * 1_024
            || wire.callback_url.trim() != wire.callback_url
            || wire.callback_url.chars().any(char::is_control)
        {
            return Err(serde::de::Error::custom("callback_url is invalid"));
        }
        Ok(Self {
            callback_url: wire.callback_url,
        })
    }
}

impl Drop for SubmitExternalOauthCallbackRequest {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.callback_url);
    }
}

pub(super) struct PutProviderCredentialsRequest {
    auth_method: AuthMethod,
    acquired_via: CredentialAcquisition,
    session_kind: SessionKind,
    expires_at: Option<Timestamp>,
    fields: Vec<CredentialFieldRequest>,
}

struct CredentialFieldRequest {
    purpose: SecretPurpose,
    value: String,
}

impl CredentialFieldRequest {
    fn into_secret_value(mut self) -> SecretValue {
        SecretValue::new(std::mem::take(&mut self.value).into_bytes())
    }
}

impl fmt::Debug for PutProviderCredentialsRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PutProviderCredentialsRequest")
            .field("auth_method", &self.auth_method)
            .field("acquired_via", &self.acquired_via)
            .field("session_kind", &self.session_kind)
            .field("expires_at", &self.expires_at)
            .field(
                "field_purposes",
                &self
                    .fields
                    .iter()
                    .map(|field| field.purpose)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl<'de> Deserialize<'de> for PutProviderCredentialsRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            auth_method: AuthMethod,
            acquired_via: CredentialAcquisition,
            session_kind: SessionKind,
            expires_at: Option<Timestamp>,
            fields: Vec<WireField>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireField {
            purpose: SecretPurpose,
            value: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            auth_method: wire.auth_method,
            acquired_via: wire.acquired_via,
            session_kind: wire.session_kind,
            expires_at: wire.expires_at,
            fields: wire
                .fields
                .into_iter()
                .map(|field| CredentialFieldRequest {
                    purpose: field.purpose,
                    value: field.value,
                })
                .collect(),
        })
    }
}

impl Drop for CredentialFieldRequest {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.value);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct PutProviderCredentialsResponse {
    credential_count: usize,
    status: SessionStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct PutAuthSessionCredentialsResponse {
    session: AuthSession,
    credential_count: usize,
    status: SessionStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfigureScanScheduleRequest {
    desired_interval_seconds: Option<u64>,
    enabled: bool,
}

async fn resolve_account_scan_default(
    state: &ApiState,
    provider: &ProviderEntry,
    account_id: ProviderAccountId,
) -> Result<u64, ApiError> {
    let repository = SqliteProviderRuntimeSettingsRepository::new(state.database.clone());
    let provider_settings = repository
        .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Provider {
            provider_id: provider.metadata.id.clone(),
        })
        .await
        .map_err(ApiError::internal)?;
    let account_settings = repository
        .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::ProviderAccount {
            provider_id: provider.metadata.id.clone(),
            provider_account_id: account_id,
        })
        .await
        .map_err(ApiError::internal)?;
    let resolved = provider
        .runtime_settings
        .resolve(
            provider_settings.as_ref().map(|record| &record.patch),
            account_settings.as_ref().map(|record| &record.patch),
            None,
        )
        .map_err(|_| {
            ApiError::conflict(
                "runtime_settings_schema_mismatch",
                "stored Provider settings no longer match the registered schema",
            )
        })?;
    provider
        .runtime_settings
        .account_scan_interval(&resolved)
        .map_err(|_| {
            ApiError::conflict(
                "runtime_settings_schema_mismatch",
                "the Provider scan interval setting is incompatible",
            )
        })?
        .ok_or_else(|| {
            ApiError::bad_request(
                "scan_default_unavailable",
                "desired_interval_seconds is required because this Provider declares no scan default",
            )
        })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct ScanScheduleResponse {
    id: asterism_domain::ScheduleId,
    provider_account_id: ProviderAccountId,
    desired_interval_seconds: u64,
    effective_interval_seconds: u64,
    provider_min_interval_seconds: Option<u64>,
    next_run_at: Timestamp,
    enabled: bool,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl ScanScheduleResponse {
    fn from_schedule(schedule: &ScanSchedule, provider_min_interval_seconds: Option<u64>) -> Self {
        Self {
            id: schedule.id,
            provider_account_id: schedule.provider_account_id,
            desired_interval_seconds: schedule.desired_interval_seconds,
            effective_interval_seconds: schedule.interval_seconds,
            provider_min_interval_seconds,
            next_run_at: schedule.next_run_at,
            enabled: schedule.enabled,
            created_at: schedule.created_at,
            updated_at: schedule.updated_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct ProviderAccountResponse {
    id: ProviderAccountId,
    owner_id: UserId,
    provider_id: ProviderId,
    display_name: String,
    tenant: Option<String>,
    auth_state: AuthState,
    network_profile_id: Option<String>,
    credential_count: usize,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl From<&ProviderAccount> for ProviderAccountResponse {
    fn from(account: &ProviderAccount) -> Self {
        Self {
            id: account.id,
            owner_id: account.owner_id,
            provider_id: account.provider_id.clone(),
            display_name: account.display_name.clone(),
            tenant: account.tenant.clone(),
            auth_state: account.auth_state.clone(),
            network_profile_id: account.network_profile_id.clone(),
            credential_count: account.credential_refs.len(),
            created_at: account.created_at,
            updated_at: account.updated_at,
        }
    }
}

fn api_json<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_json",
            "the request body must be valid JSON with the expected fields",
        )
    })
}

fn parse_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "provider account ID is invalid",
        )
    })
}

fn parse_auth_session_id(value: &str) -> Result<AuthSessionId, ApiError> {
    AuthSessionId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_auth_session_id",
            "authentication session ID is invalid",
        )
    })
}

fn request_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))
}

fn credential_bundle(
    account: ProviderAccount,
    request: PutProviderCredentialsRequest,
) -> CredentialBundle {
    CredentialBundle {
        provider_id: account.provider_id,
        tenant: account.tenant,
        auth_method: request.auth_method,
        acquired_via: request.acquired_via,
        captured_at: Utc::now(),
        expires_at: request.expires_at,
        session_kind: request.session_kind,
        fields: request
            .fields
            .into_iter()
            .map(|field| CredentialField {
                purpose: field.purpose,
                value: field.into_secret_value(),
            })
            .collect(),
        user_id_hint: None,
    }
}

fn validate_mutable_fields(
    display_name: &str,
    tenant: Option<String>,
) -> Result<(String, Option<String>), ApiError> {
    let display_name = display_name.trim();
    if display_name.is_empty()
        || display_name.len() > 128
        || display_name.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "invalid_provider_account",
            "display_name must contain 1-128 bytes without control characters",
        ));
    }
    let tenant = tenant
        .map(|tenant| tenant.trim().to_owned())
        .filter(|tenant| !tenant.is_empty());
    if tenant
        .as_ref()
        .is_some_and(|tenant| tenant.len() > 256 || tenant.chars().any(char::is_control))
    {
        return Err(ApiError::bad_request(
            "invalid_provider_account",
            "tenant must contain at most 256 bytes without control characters",
        ));
    }
    Ok((display_name.to_owned(), tenant))
}

fn map_storage_error(error: StorageError) -> ApiError {
    match error {
        StorageError::InvalidData(message) => {
            ApiError::bad_request("invalid_provider_account", message)
        }
        error => ApiError::internal(error),
    }
}

pub(super) fn map_credential_error(error: CredentialProvisionError) -> ApiError {
    match error {
        CredentialProvisionError::Unauthorized => ApiError::forbidden(),
        CredentialProvisionError::InvalidBundle(error) => {
            ApiError::bad_request("invalid_provider_credential", error.to_string())
        }
        CredentialProvisionError::AccountNotFound(_) => {
            ApiError::not_found("provider_account_not_found")
        }
        CredentialProvisionError::AccountMismatch => ApiError::conflict(
            "provider_account_mismatch",
            "the credential does not match the Provider account",
        ),
        CredentialProvisionError::ProviderNotRegistered(_)
        | CredentialProvisionError::AuthenticationUnavailable
        | CredentialProvisionError::UnsupportedAuthMethod(_)
        | CredentialProvisionError::UnsupportedSessionKind(_) => ApiError::conflict(
            "provider_authentication_unavailable",
            "the Provider does not support the requested authentication contract",
        ),
        CredentialProvisionError::CredentialRejected => ApiError::conflict(
            "provider_credential_rejected",
            "the Provider rejected the candidate credential",
        ),
        CredentialProvisionError::InvalidProviderStatus => ApiError::bad_gateway(
            "provider_authentication_invalid",
            "the Provider returned an inconsistent authentication result",
        ),
        CredentialProvisionError::Provider(error) => map_credential_provider_error(error),
        CredentialProvisionError::Storage(error) => ApiError::internal(error),
        CredentialProvisionError::SecretStore(error) => map_secret_store_error(error),
    }
}

fn map_auth_session_error(error: AuthSessionServiceError) -> ApiError {
    match error {
        AuthSessionServiceError::AccountNotFound(_) => {
            ApiError::not_found("provider_account_not_found")
        }
        AuthSessionServiceError::SessionNotFound(_) => {
            ApiError::not_found("auth_session_not_found")
        }
        AuthSessionServiceError::ProviderNotRegistered(_)
        | AuthSessionServiceError::AuthenticationUnavailable
        | AuthSessionServiceError::UnsupportedAuthMethod(_) => ApiError::conflict(
            "provider_authentication_unavailable",
            "the Provider does not support the requested authentication method",
        ),
        AuthSessionServiceError::InvalidChallenge(_) => ApiError::bad_gateway(
            "provider_authentication_invalid",
            "the Provider returned an inconsistent authentication challenge",
        ),
        AuthSessionServiceError::SessionExpired(_)
        | AuthSessionServiceError::InvalidSessionState(_)
        | AuthSessionServiceError::RevisionConflict(_) => ApiError::conflict(
            "auth_session_conflict",
            "the authentication session is expired, terminal, or changed concurrently",
        ),
        AuthSessionServiceError::Provider { source, .. } => map_credential_provider_error(source),
        AuthSessionServiceError::Credential(error) => map_credential_error(error),
        AuthSessionServiceError::CredentialStore(error) => map_secret_store_error(error),
        AuthSessionServiceError::Domain(error) => {
            ApiError::bad_request("invalid_auth_session", error.to_string())
        }
        AuthSessionServiceError::ExternalOauthDomain(error) => {
            ApiError::bad_gateway("provider_external_oauth_invalid", error.to_string())
        }
        AuthSessionServiceError::Storage(error) => ApiError::internal(error),
    }
}

pub(super) fn map_secret_store_error(error: SecretStoreError) -> ApiError {
    match error {
        SecretStoreError::Unauthorized => ApiError::forbidden(),
        SecretStoreError::NotFound => ApiError::not_found("provider_account_not_found"),
        SecretStoreError::InvalidValue => ApiError::bad_request(
            "invalid_provider_credential",
            "the credential payload is invalid",
        ),
        SecretStoreError::AccountMismatch => ApiError::conflict(
            "provider_account_mismatch",
            "the credential does not match the Provider account",
        ),
        SecretStoreError::VersionConflict => ApiError::conflict(
            "provider_credential_conflict",
            "the Provider credentials changed concurrently",
        ),
        SecretStoreError::KeyUnavailable => ApiError::service_unavailable(
            "secret_store_unavailable",
            "the encrypted credential store is not configured",
        ),
        error => ApiError::internal(error),
    }
}

fn map_credential_provider_error(error: ProviderError) -> ApiError {
    match error.kind {
        ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
            error.retry_after_seconds.unwrap_or(60).clamp(1, 86_400),
        ),
        ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
            tracing::warn!(%error, "Provider credential validation is temporarily unavailable");
            ApiError::service_unavailable(
                "provider_unavailable",
                "the Provider is temporarily unavailable",
            )
        }
        ProviderErrorKind::Authentication | ProviderErrorKind::Authorization => ApiError::conflict(
            "provider_credential_rejected",
            "the Provider rejected the candidate credential",
        ),
        ProviderErrorKind::HumanRequired
        | ProviderErrorKind::RemoteChanged
        | ProviderErrorKind::UnsupportedTask => ApiError::conflict(
            "provider_action_required",
            "the Provider requires additional user action",
        ),
        ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
            tracing::warn!(%error, "Provider returned invalid authentication data");
            ApiError::bad_gateway(
                "provider_authentication_invalid",
                "the Provider returned inconsistent authentication data",
            )
        }
        ProviderErrorKind::Internal => ApiError::internal(error),
    }
}

fn map_scan_error(error: ProviderScanError) -> ApiError {
    match error {
        ProviderScanError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the account Provider is not registered",
        ),
        ProviderScanError::NoInventoryCapabilities(_) => ApiError::conflict(
            "provider_inventory_unavailable",
            "the Provider exposes no inventory capability",
        ),
        ProviderScanError::AccountNotAuthenticated(_) => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before scanning",
        ),
        ProviderScanError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider scan is temporarily unavailable");
                ApiError::service_unavailable(
                    "provider_unavailable",
                    "the Provider is temporarily unavailable",
                )
            }
            ProviderErrorKind::Authentication
            | ProviderErrorKind::Authorization
            | ProviderErrorKind::RemoteChanged
            | ProviderErrorKind::UnsupportedTask
            | ProviderErrorKind::HumanRequired => ApiError::conflict(
                "provider_action_required",
                "the Provider requires authentication or user action",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid scan inventory");
                ApiError::bad_gateway(
                    "provider_inventory_invalid",
                    "the Provider returned inconsistent inventory",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderScanError::CourseScopeMismatch { .. }
        | ProviderScanError::UnadvertisedTaskCapability { .. }
        | ProviderScanError::InvalidProtocolObservation => {
            tracing::warn!(%error, "Provider returned inconsistent scan inventory");
            ApiError::bad_gateway(
                "provider_inventory_invalid",
                "the Provider returned inconsistent inventory",
            )
        }
        ProviderScanError::InvalidCorrelationId | ProviderScanError::Storage(_) => {
            ApiError::internal(error)
        }
    }
}

fn map_scan_schedule_error(error: ScanScheduleError) -> ApiError {
    ApiError::bad_request("invalid_scan_schedule", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_request_debug_output_redacts_every_value() {
        let request: PutProviderCredentialsRequest = serde_json::from_str(
            r#"{"auth_method":"imported_cookie","acquired_via":"manual_import","session_kind":"cookie","fields":[{"purpose":"provider_cookie","value":"private-cookie"}]}"#,
        )
        .unwrap();
        let debug = format!("{request:?}");
        assert!(debug.contains("ProviderCookie"));
        assert!(!debug.contains("private-cookie"));
    }
}
