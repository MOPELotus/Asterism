use std::str::FromStr;

use asterism_domain::{
    AuthState, ProviderAccount, ProviderAccountId, ProviderId, Timestamp, UserId,
};
use asterism_storage::{ProviderAccountRepository, SqliteProviderAccountRepository, StorageError};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, ListResponse, auth::AuthContext};

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
