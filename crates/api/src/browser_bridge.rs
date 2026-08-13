use std::{fmt, net::SocketAddr, str::FromStr};

use asterism_domain::{
    BrowserBridgeSession, BrowserBridgeSessionError, BrowserBridgeSessionId,
    BrowserBridgeSessionState, ProviderId, TaskId, Timestamp,
};
use asterism_engine::{
    BrowserBridgeHelperSessionError, BrowserBridgeHelperSessionService,
    BrowserBridgeSessionAccessRequest, BrowserBridgeSessionCancelRequest,
    BrowserBridgeSessionClaimRequest, BrowserBridgeSessionCreateRequest,
    ProviderTaskBrowserSessionService, ReadTaskBrowserSessionCommand,
};
use asterism_provider_api::BrowserSessionSpec;
use asterism_secrets::SecretString;
use asterism_storage::{
    SqliteBrowserBridgeSessionRepository, SqliteProviderAccountRepository,
    SqliteTaskQueryRepository,
};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::{Serialize, ser::SerializeStruct};

use crate::{ApiError, ApiState, auth::AuthContext};

const BROWSER_BRIDGE_TTL_SECONDS: i64 = 10 * 60 * 60;
const MAX_BROWSER_BRIDGE_TOKEN_BYTES: usize = 128;

pub(super) async fn create_browser_bridge_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (owner_user_id, _) = auth.require_task_execute()?;
    let task_id = parse_task_id(&task_id)?;
    let correlation_id = request_id(&headers)?.to_owned();
    let prepared = ProviderTaskBrowserSessionService::new(
        state.providers.clone(),
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
    )
    .read(ReadTaskBrowserSessionCommand {
        owner_id: owner_user_id,
        task_id,
        correlation_id: correlation_id.clone(),
    })
    .await
    .map_err(crate::task::map_task_browser_session_error)?;
    let now = Utc::now();
    let created = bridge_service(&state)?
        .create(BrowserBridgeSessionCreateRequest {
            owner_user_id,
            provider_account_id: prepared.provider_account_id,
            task_id: prepared.task_id,
            provider_id: prepared.provider_id,
            provider_version: prepared.provider_version,
            spec: prepared.spec,
            created_at: now,
            expires_at: now + Duration::seconds(BROWSER_BRIDGE_TTL_SECONDS),
            actor: auth.audit_actor(),
            correlation_id,
        })
        .await
        .map_err(map_bridge_error)?;
    Ok(crate::auth::no_store(
        (
            StatusCode::CREATED,
            Json(CreateBrowserBridgeSessionResponse {
                session: BrowserBridgeSessionResponse::from(&created.session),
                spec: created.spec,
                pairing_token: created.pairing_token,
            }),
        )
            .into_response(),
    ))
}

pub(super) async fn get_browser_bridge_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(session_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_task_read()?;
    let snapshot = bridge_service(&state)?
        .read_owner(owner_user_id, parse_session_id(&session_id)?)
        .await
        .map_err(map_bridge_error)?;
    Ok(crate::auth::no_store(
        Json(BrowserBridgeSessionSnapshotResponse {
            session: BrowserBridgeSessionResponse::from(&snapshot.session),
            spec: snapshot.spec,
        })
        .into_response(),
    ))
}

pub(super) async fn cancel_browser_bridge_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (owner_user_id, _) = auth.require_task_execute()?;
    let snapshot = bridge_service(&state)?
        .cancel(BrowserBridgeSessionCancelRequest {
            owner_user_id,
            session_id: parse_session_id(&session_id)?,
            cancelled_at: Utc::now(),
            actor: auth.audit_actor(),
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bridge_error)?;
    Ok(crate::auth::no_store(
        Json(BrowserBridgeSessionSnapshotResponse {
            session: BrowserBridgeSessionResponse::from(&snapshot.session),
            spec: snapshot.spec,
        })
        .into_response(),
    ))
}

pub(super) async fn claim_browser_bridge_session(
    State(state): State<ApiState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    state
        .browser_bridge_claim_rate_limiter
        .check_and_record(remote.ip(), &session_id.to_string())
        .map_err(|limited| ApiError::rate_limited(limited.retry_after_seconds))?;
    let pairing_token =
        bridge_authorization(&headers).ok_or_else(ApiError::invalid_browser_bridge_token)?;
    let claimed = bridge_service(&state)?
        .claim(BrowserBridgeSessionClaimRequest {
            session_id,
            pairing_token,
            claimed_at: Utc::now(),
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bridge_error)?;
    Ok(crate::auth::no_store(
        Json(ClaimBrowserBridgeSessionResponse {
            session: BrowserBridgeSessionResponse::from(&claimed.session),
            spec: claimed.spec,
            access_token: claimed.access_token,
        })
        .into_response(),
    ))
}

pub(super) async fn get_browser_bridge_snapshot(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let access_token =
        bridge_authorization(&headers).ok_or_else(ApiError::invalid_browser_bridge_token)?;
    let snapshot = bridge_service(&state)?
        .authenticate_access(BrowserBridgeSessionAccessRequest {
            session_id: parse_session_id(&session_id)?,
            access_token,
            authenticated_at: Utc::now(),
        })
        .await
        .map_err(map_bridge_error)?;
    Ok(crate::auth::no_store(
        Json(BrowserBridgeSessionSnapshotResponse {
            session: BrowserBridgeSessionResponse::from(&snapshot.session),
            spec: snapshot.spec,
        })
        .into_response(),
    ))
}

fn bridge_service(
    state: &ApiState,
) -> Result<BrowserBridgeHelperSessionService<SqliteBrowserBridgeSessionRepository>, ApiError> {
    BrowserBridgeHelperSessionService::new(SqliteBrowserBridgeSessionRepository::new(
        state.database.clone(),
    ))
    .map_err(ApiError::internal)
}

fn bridge_authorization(headers: &HeaderMap) -> Option<SecretString> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("browserbridge")
        || parts.next().is_some()
        || token.is_empty()
        || token.len() > MAX_BROWSER_BRIDGE_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        return None;
    }
    Some(SecretString::new(token))
}

fn parse_task_id(value: &str) -> Result<TaskId, ApiError> {
    TaskId::from_str(value)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))
}

fn parse_session_id(value: &str) -> Result<BrowserBridgeSessionId, ApiError> {
    BrowserBridgeSessionId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_browser_bridge_session_id",
            "BrowserBridge session ID is invalid",
        )
    })
}

fn request_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))
}

fn map_bridge_error(error: BrowserBridgeHelperSessionError) -> ApiError {
    match error {
        BrowserBridgeHelperSessionError::SessionNotFound(_) => {
            ApiError::not_found("browser_bridge_session_not_found")
        }
        BrowserBridgeHelperSessionError::PairingRejected
        | BrowserBridgeHelperSessionError::AccessRejected => {
            ApiError::invalid_browser_bridge_token()
        }
        BrowserBridgeHelperSessionError::RevisionConflict(_) => ApiError::conflict(
            "browser_bridge_session_conflict",
            "the BrowserBridge session changed concurrently",
        ),
        BrowserBridgeHelperSessionError::Domain(
            error @ (BrowserBridgeSessionError::InvalidTransition
            | BrowserBridgeSessionError::Expired
            | BrowserBridgeSessionError::NotExpired
            | BrowserBridgeSessionError::RevisionExhausted),
        ) => ApiError::conflict("browser_bridge_session_conflict", error.to_string()),
        BrowserBridgeHelperSessionError::Domain(error) => ApiError::internal(error),
        BrowserBridgeHelperSessionError::Spec(error) => ApiError::internal(error),
        BrowserBridgeHelperSessionError::Storage(error) => ApiError::internal(error),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BrowserBridgeSessionResponse {
    id: BrowserBridgeSessionId,
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    spec_version: u32,
    state: BrowserBridgeSessionState,
    created_at: Timestamp,
    updated_at: Timestamp,
    expires_at: Timestamp,
    claimed_at: Option<Timestamp>,
    revision: u32,
}

impl From<&BrowserBridgeSession> for BrowserBridgeSessionResponse {
    fn from(session: &BrowserBridgeSession) -> Self {
        Self {
            id: session.id,
            task_id: session.task_id,
            provider_id: session.provider_id.clone(),
            provider_version: session.provider_version.clone(),
            spec_version: session.spec_version,
            state: session.state,
            created_at: session.created_at,
            updated_at: session.updated_at,
            expires_at: session.expires_at,
            claimed_at: session.claimed_at,
            revision: session.revision,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BrowserBridgeSessionSnapshotResponse {
    session: BrowserBridgeSessionResponse,
    spec: BrowserSessionSpec,
}

struct CreateBrowserBridgeSessionResponse {
    session: BrowserBridgeSessionResponse,
    spec: BrowserSessionSpec,
    pairing_token: SecretString,
}

impl fmt::Debug for CreateBrowserBridgeSessionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateBrowserBridgeSessionResponse")
            .field("session", &self.session)
            .field("spec", &self.spec)
            .field("pairing_token", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CreateBrowserBridgeSessionResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut response = serializer.serialize_struct("CreateBrowserBridgeSessionResponse", 3)?;
        response.serialize_field("session", &self.session)?;
        response.serialize_field("spec", &self.spec)?;
        response.serialize_field("pairing_token", self.pairing_token.expose_secret())?;
        response.end()
    }
}

struct ClaimBrowserBridgeSessionResponse {
    session: BrowserBridgeSessionResponse,
    spec: BrowserSessionSpec,
    access_token: SecretString,
}

impl fmt::Debug for ClaimBrowserBridgeSessionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimBrowserBridgeSessionResponse")
            .field("session", &self.session)
            .field("spec", &self.spec)
            .field("access_token", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for ClaimBrowserBridgeSessionResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut response = serializer.serialize_struct("ClaimBrowserBridgeSessionResponse", 3)?;
        response.serialize_field("session", &self.session)?;
        response.serialize_field("spec", &self.spec)?;
        response.serialize_field("access_token", self.access_token.expose_secret())?;
        response.end()
    }
}
