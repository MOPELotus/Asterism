use std::{fmt, net::SocketAddr, str::FromStr};

use asterism_domain::{
    AuthBootstrapPurpose, AuthBootstrapSession, AuthBootstrapSessionError, AuthBootstrapSessionId,
    ProviderAccountId, ProviderId,
};
use asterism_engine::{
    AuthBootstrapAccessRequest, AuthBootstrapCancelRequest, AuthBootstrapClaimRequest,
    AuthBootstrapCreateRequest, AuthBootstrapService, AuthBootstrapServiceError,
};
use asterism_secrets::SecretString;
use asterism_storage::{
    AuthBootstrapSessionRepository, ProviderAccountRepository,
    SqliteAuthBootstrapSessionRepository, SqliteProviderAccountRepository,
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
    let required_recipe_version = state
        .providers
        .get(&provider_id)
        .and_then(|entry| entry.metadata.capture_recipe_version)
        .ok_or_else(|| {
            ApiError::conflict(
                "capture_recipe_unavailable",
                "the Provider does not expose a bundled Capture recipe",
            )
        })?;
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
    Ok(crate::auth::no_store(
        Json(ClaimAuthBootstrapSessionResponse {
            session: claimed.session,
            access_token: claimed.access_token,
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

fn bootstrap_service(
    state: &ApiState,
) -> Result<AuthBootstrapService<SqliteAuthBootstrapSessionRepository>, ApiError> {
    AuthBootstrapService::new(SqliteAuthBootstrapSessionRepository::new(
        state.database.clone(),
    ))
    .map_err(ApiError::internal)
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
        AuthBootstrapServiceError::Domain(
            error @ (AuthBootstrapSessionError::InvalidTransition
            | AuthBootstrapSessionError::SessionExpired
            | AuthBootstrapSessionError::SessionNotExpired
            | AuthBootstrapSessionError::RevisionExhausted),
        ) => ApiError::conflict("auth_bootstrap_session_conflict", error.to_string()),
        AuthBootstrapServiceError::Domain(error) => {
            ApiError::bad_request("invalid_auth_bootstrap_session", error.to_string())
        }
        AuthBootstrapServiceError::Storage(error) => ApiError::internal(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateAuthBootstrapSessionRequest {
    provider_id: String,
    provider_account_id: Option<String>,
    purpose: AuthBootstrapPurpose,
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
}

impl fmt::Debug for ClaimAuthBootstrapSessionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimAuthBootstrapSessionResponse")
            .field("session", &self.session)
            .field("access_token", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for ClaimAuthBootstrapSessionResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut response = serializer.serialize_struct("ClaimAuthBootstrapSessionResponse", 2)?;
        response.serialize_field("session", &self.session)?;
        response.serialize_field("access_token", self.access_token.expose_secret())?;
        response.end()
    }
}
