use std::{fmt, net::SocketAddr, str::FromStr, sync::Arc};

use asterism_domain::{
    AuthBootstrapClientEvent, AuthBootstrapClientEventKind, AuthBootstrapPurpose,
    AuthBootstrapSession, AuthBootstrapSessionError, AuthBootstrapSessionId, AuthMethod,
    ProviderAccountId, ProviderId, SessionKind, Timestamp,
};
use asterism_engine::{
    AuthBootstrapAccessRequest, AuthBootstrapCancelRequest, AuthBootstrapClaimRequest,
    AuthBootstrapCreateRequest, AuthBootstrapCredentialRequest, AuthBootstrapCredentialService,
    AuthBootstrapCredentialServiceError, AuthBootstrapEventRequest, AuthBootstrapService,
    AuthBootstrapServiceError,
};
use asterism_provider_api::CaptureRecipe;
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, CredentialField, SecretPurpose, SecretString,
    SecretValue,
};
use asterism_storage::{
    AuthBootstrapSessionRepository, ProviderAccountRepository,
    SqliteAuthBootstrapSessionRepository, SqliteProtocolObservationRepository,
    SqliteProviderAccountRepository,
};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize, ser::SerializeStruct};

use crate::{ApiError, ApiState, auth::AuthContext};

const AUTH_BOOTSTRAP_TTL_SECONDS: i64 = 10 * 60;
const MAX_BOOTSTRAP_TOKEN_BYTES: usize = 128;

pub(super) async fn create_auth_bootstrap_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    payload: Result<Json<CreateAuthBootstrapSessionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_account_manage()?;
    let request = api_json(payload)?;
    let provider_id = ProviderId::new(request.provider_id)
        .map_err(|error| ApiError::bad_request("invalid_provider_id", error.to_string()))?;
    let required_recipe_version =
        provider_capture_recipe(&state, &provider_id, request.recipe_version)?.version;
    let provider_account_id = request
        .provider_account_id
        .as_deref()
        .map(parse_provider_account_id)
        .transpose()?;
    if let Some(account_id) = provider_account_id {
        let account = SqliteProviderAccountRepository::new(state.database.clone())
            .find_provider_account(owner_user_id, account_id)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("provider_account_not_found"))?;
        if account.provider_id != provider_id {
            return Err(ApiError::conflict(
                "provider_account_mismatch",
                "the Provider account does not match the requested Provider",
            ));
        }
    }
    let now = Utc::now();
    let created = bootstrap_service(&state)?
        .create(AuthBootstrapCreateRequest {
            owner_user_id,
            provider_id,
            provider_account_id,
            purpose: request.purpose,
            required_recipe_version,
            created_at: now,
            expires_at: now + Duration::seconds(AUTH_BOOTSTRAP_TTL_SECONDS),
            actor: auth.audit_actor(),
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bootstrap_error)?;
    Ok(crate::auth::no_store(
        (
            StatusCode::CREATED,
            Json(CreateAuthBootstrapSessionResponse {
                session: created.session,
                pairing_token: created.pairing_token,
            }),
        )
            .into_response(),
    ))
}

pub(super) async fn get_auth_bootstrap_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(session_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_account_read()?;
    let session_id = parse_session_id(&session_id)?;
    let session = SqliteAuthBootstrapSessionRepository::new(state.database)
        .find_auth_bootstrap_session(owner_user_id, session_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("auth_bootstrap_session_not_found"))?;
    Ok(crate::auth::no_store(Json(session).into_response()))
}

pub(super) async fn cancel_auth_bootstrap_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_account_manage()?;
    let session_id = parse_session_id(&session_id)?;
    let session = bootstrap_service(&state)?
        .cancel(AuthBootstrapCancelRequest {
            owner_user_id,
            session_id,
            cancelled_at: Utc::now(),
            actor: auth.audit_actor(),
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bootstrap_error)?;
    Ok(crate::auth::no_store(Json(session).into_response()))
}

pub(super) async fn claim_auth_bootstrap_session(
    State(state): State<ApiState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    state
        .bootstrap_claim_rate_limiter
        .check_and_record(remote.ip(), &session_id.to_string())
        .map_err(|limited| ApiError::rate_limited(limited.retry_after_seconds))?;
    let pairing_token =
        bootstrap_authorization(&headers).ok_or_else(ApiError::invalid_bootstrap_token)?;
    let claimed = bootstrap_service(&state)?
        .claim(AuthBootstrapClaimRequest {
            session_id,
            pairing_token,
            claimed_at: Utc::now(),
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bootstrap_error)?;
    let recipe = provider_capture_recipe(
        &state,
        &claimed.session.provider_id,
        Some(claimed.session.required_recipe_version),
    )?;
    Ok(crate::auth::no_store(
        Json(ClaimAuthBootstrapSessionResponse {
            session: claimed.session,
            access_token: claimed.access_token,
            recipe,
        })
        .into_response(),
    ))
}

pub(super) async fn get_auth_bootstrap_stream_snapshot(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    let access_token =
        bootstrap_authorization(&headers).ok_or_else(ApiError::invalid_bootstrap_token)?;
    let session = bootstrap_service(&state)?
        .authenticate_access(AuthBootstrapAccessRequest {
            session_id,
            access_token,
            authenticated_at: Utc::now(),
        })
        .await
        .map_err(map_bootstrap_error)?;
    Ok(crate::auth::no_store(Json(session).into_response()))
}

pub(super) async fn record_auth_bootstrap_event(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<RecordAuthBootstrapEventRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    let access_token =
        bootstrap_authorization(&headers).ok_or_else(ApiError::invalid_bootstrap_token)?;
    let request = api_json(payload)?;
    let kind = parse_event_kind(&request.kind)?;
    let accepted = bootstrap_service(&state)?
        .record_client_event(AuthBootstrapEventRequest {
            session_id,
            access_token,
            sequence: request.sequence,
            kind,
            received_at: Utc::now(),
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bootstrap_error)?;
    let status = if accepted.duplicate {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(RecordAuthBootstrapEventResponse {
                event: accepted.event,
                duplicate: accepted.duplicate,
            }),
        )
            .into_response(),
    ))
}

pub(super) async fn submit_auth_bootstrap_credential(
    State(state): State<ApiState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<SubmitAuthBootstrapCredentialRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    state
        .bootstrap_credential_rate_limiter
        .check_and_record(remote.ip(), &session_id.to_string())
        .map_err(|limited| ApiError::rate_limited(limited.retry_after_seconds))?;
    let access_token =
        bootstrap_authorization(&headers).ok_or_else(ApiError::invalid_bootstrap_token)?;
    let request = api_json(payload)?;
    let secret_store = state.secret_store.ok_or_else(|| {
        ApiError::service_unavailable(
            "secret_store_unavailable",
            "the encrypted credential store is not configured",
        )
    })?;
    let submitted_at = Utc::now();
    let (display_name, bundle) = request.into_parts(submitted_at)?;
    let accepted = AuthBootstrapCredentialService::new(
        state.providers,
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteAuthBootstrapSessionRepository::new(state.database.clone()),
        secret_store,
    )
    .map_err(ApiError::internal)?
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
    .submit(AuthBootstrapCredentialRequest {
        session_id,
        access_token,
        display_name,
        bundle,
        submitted_at,
        correlation_id: request_id(&headers)?.to_owned(),
    })
    .await
    .map_err(map_bootstrap_credential_error)?;
    Ok(crate::auth::no_store(
        Json(SubmitAuthBootstrapCredentialResponse {
            session: accepted.session,
            provider_account_id: accepted.account.id,
            credential_count: accepted.credentials.len(),
            status: accepted.status,
        })
        .into_response(),
    ))
}

fn bootstrap_service(
    state: &ApiState,
) -> Result<AuthBootstrapService<SqliteAuthBootstrapSessionRepository>, ApiError> {
    AuthBootstrapService::new(SqliteAuthBootstrapSessionRepository::new(
        state.database.clone(),
    ))
    .map_err(ApiError::internal)
}

fn provider_capture_recipe(
    state: &ApiState,
    provider_id: &ProviderId,
    expected_version: Option<u32>,
) -> Result<CaptureRecipe, ApiError> {
    let recipes = state
        .providers
        .get(provider_id)
        .and_then(|entry| entry.authentication.as_ref())
        .map_or_else(Vec::new, |authentication| authentication.capture_recipes());
    let recipe = expected_version
        .map_or_else(
            || recipes.first(),
            |version| recipes.iter().find(|recipe| recipe.version == version),
        )
        .filter(|recipe| recipe.validate().is_ok())
        .cloned()
        .ok_or_else(|| {
            ApiError::conflict(
                "capture_recipe_unavailable",
                "the Provider does not expose the Capture recipe required by this session",
            )
        })?;
    Ok(recipe)
}

fn bootstrap_authorization(headers: &HeaderMap) -> Option<SecretString> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bootstrap")
        || parts.next().is_some()
        || token.is_empty()
        || token.len() > MAX_BOOTSTRAP_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        return None;
    }
    Some(SecretString::new(token))
}

fn parse_session_id(value: &str) -> Result<AuthBootstrapSessionId, ApiError> {
    AuthBootstrapSessionId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_auth_bootstrap_session_id",
            "authentication bootstrap session ID is invalid",
        )
    })
}

fn parse_provider_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "Provider account ID is invalid",
        )
    })
}

fn request_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))
}

fn api_json<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_json",
            "the request body must be valid JSON with the expected fields",
        )
    })
}

fn parse_event_kind(value: &serde_json::Value) -> Result<AuthBootstrapClientEventKind, ApiError> {
    let kind = serde_json::from_value(value.clone()).map_err(|_| {
        ApiError::bad_request(
            "invalid_auth_bootstrap_event",
            "the event kind must use a supported shape",
        )
    })?;
    let canonical = serde_json::to_value(&kind).map_err(ApiError::internal)?;
    if canonical != *value {
        return Err(ApiError::bad_request(
            "invalid_auth_bootstrap_event",
            "the event kind contains fields not allowed for its type",
        ));
    }
    Ok(kind)
}

fn map_bootstrap_error(error: AuthBootstrapServiceError) -> ApiError {
    match error {
        AuthBootstrapServiceError::SessionNotFound(_) => {
            ApiError::not_found("auth_bootstrap_session_not_found")
        }
        AuthBootstrapServiceError::PairingRejected | AuthBootstrapServiceError::AccessRejected => {
            ApiError::invalid_bootstrap_token()
        }
        AuthBootstrapServiceError::RevisionConflict(_) => ApiError::conflict(
            "auth_bootstrap_session_conflict",
            "the authentication bootstrap session changed concurrently",
        ),
        AuthBootstrapServiceError::EventSequenceConflict { .. } => ApiError::conflict(
            "auth_bootstrap_event_sequence_conflict",
            "the authentication bootstrap event sequence conflicts with stored events",
        ),
        AuthBootstrapServiceError::Domain(
            error @ (AuthBootstrapSessionError::InvalidTransition
            | AuthBootstrapSessionError::SessionExpired
            | AuthBootstrapSessionError::SessionNotExpired
            | AuthBootstrapSessionError::RevisionExhausted),
        ) => ApiError::conflict("auth_bootstrap_session_conflict", error.to_string()),
        AuthBootstrapServiceError::Domain(error) => {
            ApiError::bad_request("invalid_auth_bootstrap_session", error.to_string())
        }
        AuthBootstrapServiceError::EventDomain(error) => {
            ApiError::bad_request("invalid_auth_bootstrap_event", error.to_string())
        }
        AuthBootstrapServiceError::Storage(error) => ApiError::internal(error),
    }
}

fn map_bootstrap_credential_error(error: AuthBootstrapCredentialServiceError) -> ApiError {
    match error {
        AuthBootstrapCredentialServiceError::AccessRejected => ApiError::invalid_bootstrap_token(),
        AuthBootstrapCredentialServiceError::InvalidAccountMetadata => ApiError::bad_request(
            "invalid_provider_account",
            "display_name is required only for a new Provider account",
        ),
        AuthBootstrapCredentialServiceError::AccountBindingConflict => ApiError::conflict(
            "auth_bootstrap_account_conflict",
            "the Provider account binding changed during credential validation",
        ),
        AuthBootstrapCredentialServiceError::RecipeMismatch => ApiError::conflict(
            "capture_recipe_mismatch",
            "the captured credential does not match the Provider recipe frozen for this session",
        ),
        AuthBootstrapCredentialServiceError::InvalidProtocolObservation => ApiError::bad_gateway(
            "provider_authentication_invalid",
            "the Provider returned inconsistent authentication data",
        ),
        AuthBootstrapCredentialServiceError::Credential(error) => {
            crate::account::map_credential_error(error)
        }
        AuthBootstrapCredentialServiceError::Storage(error) => ApiError::internal(error),
        AuthBootstrapCredentialServiceError::SecretStore(error) => {
            crate::account::map_secret_store_error(error)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateAuthBootstrapSessionRequest {
    provider_id: String,
    provider_account_id: Option<String>,
    purpose: AuthBootstrapPurpose,
    recipe_version: Option<u32>,
}

pub(super) struct CreateAuthBootstrapSessionResponse {
    session: AuthBootstrapSession,
    pairing_token: SecretString,
}

impl fmt::Debug for CreateAuthBootstrapSessionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateAuthBootstrapSessionResponse")
            .field("session", &self.session)
            .field("pairing_token", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CreateAuthBootstrapSessionResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut response = serializer.serialize_struct("CreateAuthBootstrapSessionResponse", 2)?;
        response.serialize_field("session", &self.session)?;
        response.serialize_field("pairing_token", self.pairing_token.expose_secret())?;
        response.end()
    }
}

pub(super) struct ClaimAuthBootstrapSessionResponse {
    session: AuthBootstrapSession,
    access_token: SecretString,
    recipe: CaptureRecipe,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordAuthBootstrapEventRequest {
    sequence: u64,
    kind: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct RecordAuthBootstrapEventResponse {
    event: AuthBootstrapClientEvent,
    duplicate: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubmitAuthBootstrapCredentialRequest {
    display_name: Option<String>,
    provider_id: String,
    tenant: Option<String>,
    auth_method: AuthMethod,
    session_kind: SessionKind,
    expires_at: Option<Timestamp>,
    fields: Vec<SubmitAuthBootstrapCredentialField>,
}

impl SubmitAuthBootstrapCredentialRequest {
    fn into_parts(
        self,
        captured_at: Timestamp,
    ) -> Result<(Option<String>, CredentialBundle), ApiError> {
        let bundle = CredentialBundle {
            provider_id: ProviderId::new(self.provider_id)
                .map_err(|error| ApiError::bad_request("invalid_provider_id", error.to_string()))?,
            tenant: self.tenant,
            auth_method: self.auth_method,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at,
            expires_at: self.expires_at,
            session_kind: self.session_kind,
            fields: self
                .fields
                .into_iter()
                .map(|field| CredentialField {
                    purpose: field.purpose,
                    value: field.into_secret_value(),
                })
                .collect(),
            user_id_hint: None,
        };
        Ok((self.display_name, bundle))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitAuthBootstrapCredentialField {
    purpose: SecretPurpose,
    value: String,
}

impl SubmitAuthBootstrapCredentialField {
    fn into_secret_value(mut self) -> SecretValue {
        SecretValue::new(std::mem::take(&mut self.value).into_bytes())
    }
}

impl Drop for SubmitAuthBootstrapCredentialField {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.value);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct SubmitAuthBootstrapCredentialResponse {
    session: AuthBootstrapSession,
    provider_account_id: ProviderAccountId,
    credential_count: usize,
    status: asterism_provider_api::SessionStatus,
}

impl fmt::Debug for ClaimAuthBootstrapSessionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimAuthBootstrapSessionResponse")
            .field("session", &self.session)
            .field("access_token", &"[REDACTED]")
            .field("recipe", &self.recipe)
            .finish()
    }
}

impl Serialize for ClaimAuthBootstrapSessionResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut response = serializer.serialize_struct("ClaimAuthBootstrapSessionResponse", 3)?;
        response.serialize_field("session", &self.session)?;
        response.serialize_field("access_token", self.access_token.expose_secret())?;
        response.serialize_field("recipe", &self.recipe)?;
        response.end()
    }
}
