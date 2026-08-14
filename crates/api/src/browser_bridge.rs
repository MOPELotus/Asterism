use std::{fmt, net::SocketAddr, str::FromStr};

use asterism_domain::{
    BrowserBridgeResultArtifactMetadata, BrowserBridgeRuntimeBinding, BrowserBridgeSession,
    BrowserBridgeSessionError, BrowserBridgeSessionId, BrowserBridgeSessionState, ProviderId,
    TaskId, Timestamp,
};
use asterism_engine::{
    BrowserBridgeCommandDispatchRequest, BrowserBridgeCommandDispatchService,
    BrowserBridgeHelperSessionError, BrowserBridgeHelperSessionService,
    BrowserBridgeResultArtifactService, BrowserBridgeResultReceiveRequest,
    BrowserBridgeRuntimeBindRequest, BrowserBridgeSessionAccessRequest,
    BrowserBridgeSessionCancelRequest, BrowserBridgeSessionClaimRequest,
    BrowserBridgeSessionCreateRequest, ProviderTaskBrowserSessionService,
    ReadTaskBrowserSessionCommand,
};
use asterism_provider_api::BrowserSessionSpec;
use asterism_secrets::{SecretAccess, SecretActor, SecretString, SecretValue};
use asterism_storage::{
    BrowserBridgeCommandDispatchRecord, BrowserBridgeResultArtifactRecord,
    BrowserBridgeRuntimeBindingRecord, SqliteBrowserBridgeSessionRepository,
    SqliteProviderAccountRepository, SqliteTaskQueryRepository,
};
use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes as OwnedBytes;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{ApiError, ApiState, auth::AuthContext};

const BROWSER_BRIDGE_TTL_SECONDS: i64 = 10 * 60 * 60;
const MAX_BROWSER_BRIDGE_TOKEN_BYTES: usize = 128;
pub(super) const MAX_BROWSER_BRIDGE_ARTIFACT_BYTES: usize = 256 * 1_024;
const X_BROWSER_COMMAND_TYPE: &str = "x-asterism-browser-command-type";
const X_BROWSER_COMMAND_DIGEST: &str = "x-asterism-browser-command-digest";
const X_BROWSER_RESULT_TYPE: &str = "x-asterism-browser-result-type";

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

pub(super) async fn bind_browser_bridge_runtime(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<BrowserBridgeRuntimeBindingRequest>,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    let access_token =
        bridge_authorization(&headers).ok_or_else(ApiError::invalid_browser_bridge_token)?;
    let record = bridge_service(&state)?
        .bind_runtime(BrowserBridgeRuntimeBindRequest {
            binding: BrowserBridgeRuntimeBinding {
                session_id,
                observed_origin: request.observed_origin,
                frame_id: request.frame_id,
                bound_at: Utc::now(),
            },
            access_token,
            correlation_id: request_id(&headers)?.to_owned(),
        })
        .await
        .map_err(map_bridge_error)?;
    let (binding, duplicate) = match record {
        BrowserBridgeRuntimeBindingRecord::Bound(binding) => (binding, false),
        BrowserBridgeRuntimeBindingRecord::Duplicate(binding) => (binding, true),
        BrowserBridgeRuntimeBindingRecord::AccessRejected => {
            return Err(ApiError::invalid_browser_bridge_token());
        }
        BrowserBridgeRuntimeBindingRecord::Conflict => {
            return Err(ApiError::conflict(
                "browser_bridge_runtime_binding_conflict",
                "the BrowserBridge runtime origin or frame conflicts with durable session state",
            ));
        }
    };
    Ok(crate::auth::no_store(
        Json(BrowserBridgeRuntimeBindingResponse {
            session_id: binding.session_id,
            observed_origin: binding.observed_origin,
            frame_id: binding.frame_id,
            bound_at: binding.bound_at,
            duplicate,
        })
        .into_response(),
    ))
}

pub(super) async fn dispatch_browser_bridge_command(
    State(state): State<ApiState>,
    Path((session_id, sequence)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    let sequence = parse_sequence(&sequence)?;
    let access_token =
        bridge_authorization(&headers).ok_or_else(ApiError::invalid_browser_bridge_token)?;
    let dispatch_token = SecretString::new(access_token.expose_secret().to_owned());
    let snapshot = bridge_service(&state)?
        .authenticate_access(BrowserBridgeSessionAccessRequest {
            session_id,
            access_token,
            authenticated_at: Utc::now(),
        })
        .await
        .map_err(map_bridge_error)?;
    let repository = bridge_artifact_repository(&state, snapshot.session.provider_id.clone())?;
    let record = BrowserBridgeCommandDispatchService::new(repository)
        .map_err(ApiError::internal)?
        .dispatch(BrowserBridgeCommandDispatchRequest {
            session_id,
            sequence,
            access_token: dispatch_token,
            dispatched_at: Utc::now(),
            access: bridge_secret_access(&headers, "dispatch BrowserBridge command")?,
        })
        .await
        .map_err(map_command_service_error)?;
    let BrowserBridgeCommandDispatchRecord::Dispatched(command) = record else {
        return Err(map_dispatch_record(&record));
    };
    let command_type = axum::http::HeaderValue::from_str(&command.exchange.command_type)
        .map_err(ApiError::internal)?;
    let command_digest =
        axum::http::HeaderValue::from_str(&encode_digest(command.exchange.command_digest))
            .map_err(ApiError::internal)?;
    let mut response = Response::new(Body::from(OwnedBytes::from_owner(SecretResponseBody(
        command.command_artifact,
    ))));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(X_BROWSER_COMMAND_TYPE, command_type);
    response
        .headers_mut()
        .insert(X_BROWSER_COMMAND_DIGEST, command_digest);
    Ok(crate::auth::no_store(response))
}

pub(super) async fn receive_browser_bridge_result(
    State(state): State<ApiState>,
    Path((session_id, sequence)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let session_id = parse_session_id(&session_id)?;
    let sequence = parse_sequence(&sequence)?;
    require_octet_stream(&headers)?;
    if body.is_empty() || body.len() > MAX_BROWSER_BRIDGE_ARTIFACT_BYTES {
        return Err(ApiError::bad_request(
            "invalid_browser_bridge_result",
            "BrowserBridge result is empty or oversized",
        ));
    }
    let result_type = headers
        .get(X_BROWSER_RESULT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_browser_bridge_result_type",
                "BrowserBridge result type is missing or invalid",
            )
        })?
        .to_owned();
    let access_token =
        bridge_authorization(&headers).ok_or_else(ApiError::invalid_browser_bridge_token)?;
    let receive_token = SecretString::new(access_token.expose_secret().to_owned());
    let snapshot = bridge_service(&state)?
        .authenticate_access(BrowserBridgeSessionAccessRequest {
            session_id,
            access_token,
            authenticated_at: Utc::now(),
        })
        .await
        .map_err(map_bridge_error)?;
    let received_at = Utc::now();
    let result_digest = Sha256::digest(body.as_ref()).into();
    let metadata = BrowserBridgeResultArtifactMetadata {
        session_id,
        sequence,
        result_type,
        result_digest,
        received_at,
    };
    metadata.validate().map_err(|_| {
        ApiError::bad_request(
            "invalid_browser_bridge_result_type",
            "BrowserBridge result type is invalid",
        )
    })?;
    let repository = bridge_artifact_repository(&state, snapshot.session.provider_id.clone())?;
    let record = BrowserBridgeResultArtifactService::new(repository)
        .map_err(ApiError::internal)?
        .receive(BrowserBridgeResultReceiveRequest {
            metadata,
            result_artifact: secret_request_body(body),
            access_token: receive_token,
            access: bridge_secret_access(&headers, "receive BrowserBridge result")?,
        })
        .await
        .map_err(map_command_service_error)?;
    let (status, metadata, duplicate) = match record {
        BrowserBridgeResultArtifactRecord::Inserted(metadata) => {
            (StatusCode::ACCEPTED, metadata, false)
        }
        BrowserBridgeResultArtifactRecord::Duplicate(metadata) => (StatusCode::OK, metadata, true),
        BrowserBridgeResultArtifactRecord::AccessRejected => {
            return Err(ApiError::invalid_browser_bridge_token());
        }
        BrowserBridgeResultArtifactRecord::SequenceConflict => {
            return Err(ApiError::conflict(
                "browser_bridge_result_conflict",
                "the BrowserBridge result conflicts with durable command state",
            ));
        }
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(BrowserBridgeResultReceiptResponse {
                session_id: metadata.session_id,
                sequence: metadata.sequence,
                result_type: metadata.result_type,
                result_digest: encode_digest(metadata.result_digest),
                received_at: metadata.received_at,
                duplicate,
            }),
        )
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

fn bridge_artifact_repository(
    state: &ApiState,
    provider_id: ProviderId,
) -> Result<asterism_storage::SqliteBrowserBridgeCommandArtifactRepository, ApiError> {
    state
        .secret_store
        .as_ref()
        .map(|store| store.browser_bridge_commands(provider_id))
        .ok_or_else(|| {
            ApiError::service_unavailable(
                "secret_store_unavailable",
                "the encrypted BrowserBridge artifact store is not configured",
            )
        })
}

fn bridge_secret_access(
    headers: &HeaderMap,
    reason: &'static str,
) -> Result<SecretAccess, ApiError> {
    Ok(SecretAccess {
        actor: SecretActor::CoreService("browser-bridge-transport"),
        correlation_id: request_id(headers)?.to_owned(),
        reason: reason.to_owned(),
    })
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

fn parse_sequence(value: &str) -> Result<u64, ApiError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0 && i64::try_from(*value).is_ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_browser_bridge_sequence",
                "BrowserBridge sequence is invalid",
            )
        })
}

fn require_octet_stream(headers: &HeaderMap) -> Result<(), ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type == Some("application/octet-stream") {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid_browser_bridge_content_type",
            "BrowserBridge results require application/octet-stream",
        ))
    }
}

fn encode_digest(digest: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn secret_request_body(body: Bytes) -> SecretValue {
    match body.try_into_mut() {
        Ok(mut body) => {
            let secret = SecretValue::new(body.as_ref().to_vec());
            body.as_mut().zeroize();
            secret
        }
        Err(body) => SecretValue::new(body.to_vec()),
    }
}

fn map_dispatch_record(record: &BrowserBridgeCommandDispatchRecord) -> ApiError {
    match record {
        BrowserBridgeCommandDispatchRecord::AccessRejected => {
            ApiError::invalid_browser_bridge_token()
        }
        BrowserBridgeCommandDispatchRecord::NotFound => {
            ApiError::not_found("browser_bridge_command_not_found")
        }
        BrowserBridgeCommandDispatchRecord::AlreadyDispatched => ApiError::conflict(
            "browser_bridge_command_already_dispatched",
            "the BrowserBridge command was already dispatched and cannot be replayed",
        ),
        BrowserBridgeCommandDispatchRecord::SequenceConflict => ApiError::conflict(
            "browser_bridge_command_conflict",
            "the BrowserBridge command conflicts with durable session state",
        ),
        BrowserBridgeCommandDispatchRecord::Dispatched(_) => {
            ApiError::internal("dispatched command was mapped as an error")
        }
    }
}

fn map_command_service_error(error: asterism_engine::BrowserBridgeCommandServiceError) -> ApiError {
    match error {
        asterism_engine::BrowserBridgeCommandServiceError::SecretStore(
            asterism_secrets::SecretStoreError::InvalidValue,
        ) => ApiError::bad_request(
            "invalid_browser_bridge_artifact",
            "the BrowserBridge artifact is invalid",
        ),
        asterism_engine::BrowserBridgeCommandServiceError::SecretStore(
            asterism_secrets::SecretStoreError::KeyUnavailable,
        ) => ApiError::service_unavailable(
            "secret_store_unavailable",
            "the encrypted BrowserBridge artifact store is not configured",
        ),
        error @ asterism_engine::BrowserBridgeCommandServiceError::SecretStore(_) => {
            ApiError::internal(error)
        }
    }
}

struct SecretResponseBody(SecretValue);

impl AsRef<[u8]> for SecretResponseBody {
    fn as_ref(&self) -> &[u8] {
        self.0.expose_secret()
    }
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
        BrowserBridgeHelperSessionError::RuntimeBinding(error) => {
            ApiError::bad_request("invalid_browser_bridge_runtime_binding", error.to_string())
        }
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

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserBridgeRuntimeBindingRequest {
    observed_origin: String,
    frame_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BrowserBridgeRuntimeBindingResponse {
    session_id: BrowserBridgeSessionId,
    observed_origin: String,
    frame_id: String,
    bound_at: Timestamp,
    duplicate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BrowserBridgeResultReceiptResponse {
    session_id: BrowserBridgeSessionId,
    sequence: u64,
    result_type: String,
    result_digest: String,
    received_at: Timestamp,
    duplicate: bool,
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
