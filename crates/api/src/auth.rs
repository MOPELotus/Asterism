use std::{collections::BTreeSet, fmt, net::SocketAddr, str::FromStr};

use asterism_auth::{Argon2idPasswordService, OpaqueTokenService, Principal};
use asterism_domain::{
    AuditActor, Permission, RequestSource, ServiceScope, ServiceToken, ServiceTokenId, Timestamp,
    User, UserId, UserStatus, WebSession, WebSessionId,
};
use asterism_secrets::{SecretActor, SecretString};
use asterism_storage::{
    InitialMaster, ServiceTokenQueryRepository, SessionRepository, SqliteSessionRepository,
    SqliteUserRepository, StorageError, UserRepository,
};
use axum::{
    Extension, Json,
    extract::{
        ConnectInfo, Path, Query, Request, State, rejection::JsonRejection,
        rejection::QueryRejection,
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{ApiError, ApiState};

const SESSION_COOKIE: &str = "asterism_session";
const MAX_USERNAME_BYTES: usize = 64;
const MIN_PASSWORD_BYTES: usize = 8;
const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Clone, Debug)]
pub(super) struct AuthContext {
    identity: AuthIdentity,
    target_owner_id: Option<UserId>,
}

#[derive(Clone, Debug)]
enum AuthIdentity {
    Web {
        session_id: WebSessionId,
        principal: Principal,
    },
    Service(ServiceToken),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProviderSettingsAuthority {
    System,
    Owner(UserId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuditAuthority {
    Any,
    Owner(UserId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServiceTokenAuthority {
    System,
    Owner(UserId),
}

impl ProviderSettingsAuthority {
    pub(super) fn permits_owner(self, owner_id: UserId) -> bool {
        match self {
            Self::System => true,
            Self::Owner(required_owner_id) => required_owner_id == owner_id,
        }
    }

    pub(super) const fn is_system(self) -> bool {
        matches!(self, Self::System)
    }
}

impl AuthContext {
    fn require(
        &self,
        user_permission: Permission,
        service_scope: Option<ServiceScope>,
    ) -> Result<(), ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } => principal
                .require(user_permission)
                .map_err(|_| ApiError::forbidden()),
            AuthIdentity::Service(token) => service_scope
                .filter(|scope| token.scopes.contains(scope))
                .map(|_| ())
                .ok_or_else(ApiError::forbidden),
        }
    }

    fn owner_user_id(&self) -> Option<UserId> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } => Some(principal.user_id),
            AuthIdentity::Service(token) => token.owner_user_id,
        }
    }

    pub(super) fn audit_actor(&self) -> AuditActor {
        match &self.identity {
            AuthIdentity::Web { principal, .. } => AuditActor::User(principal.user_id),
            AuthIdentity::Service(token) => AuditActor::ServiceToken(token.id),
        }
    }

    pub(super) fn secret_actor(&self) -> SecretActor {
        match &self.identity {
            AuthIdentity::Web { principal, .. } => SecretActor::User(principal.user_id),
            AuthIdentity::Service(token) => SecretActor::ServiceToken(token.id),
        }
    }

    pub(super) fn require_account_read(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. }
                if principal.has(Permission::ManageOwnAccounts)
                    || principal.has(Permission::ManageProviders) =>
            {
                self.resolve_web_target(principal, Permission::ManageProviders)
            }
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ProviderRead)
                    || token.scopes.contains(&ServiceScope::ProviderManage) =>
            {
                token.owner_user_id.ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_account_manage(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. }
                if principal.has(Permission::ManageOwnAccounts)
                    || principal.has(Permission::ManageProviders) =>
            {
                self.resolve_web_target(principal, Permission::ManageProviders)
            }
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ProviderManage) =>
            {
                token.owner_user_id.ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn resolve_provider_account_owner(
        &self,
        requested_owner_id: Option<UserId>,
    ) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } => {
                let owner_id = requested_owner_id.unwrap_or(principal.user_id);
                if owner_id == principal.user_id
                    && (principal.has(Permission::ManageOwnAccounts)
                        || principal.has(Permission::ManageProviders))
                {
                    return Ok(owner_id);
                }
                if principal.has(Permission::ManageProviders) {
                    return Ok(owner_id);
                }
                Err(ApiError::forbidden())
            }
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ProviderManage) =>
            {
                let owner_id = token.owner_user_id.ok_or_else(ApiError::forbidden)?;
                if requested_owner_id.is_none_or(|requested| requested == owner_id) {
                    Ok(owner_id)
                } else {
                    Err(ApiError::forbidden())
                }
            }
            AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_provider_settings_manage(
        &self,
    ) -> Result<ProviderSettingsAuthority, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ManageSystem) => {
                Ok(ProviderSettingsAuthority::System)
            }
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ProviderManage) =>
            {
                token
                    .owner_user_id
                    .map(ProviderSettingsAuthority::Owner)
                    .ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_scan_schedule_manage(
        &self,
    ) -> Result<ProviderSettingsAuthority, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ManageSystem) => {
                Ok(ProviderSettingsAuthority::System)
            }
            AuthIdentity::Web { principal, .. }
                if principal.has(Permission::ManageOwnAccounts)
                    || principal.has(Permission::ManageProviders) =>
            {
                Ok(ProviderSettingsAuthority::Owner(principal.user_id))
            }
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ProviderManage) =>
            {
                token
                    .owner_user_id
                    .map(ProviderSettingsAuthority::Owner)
                    .ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_task_read(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ReadOwnTasks) => {
                self.resolve_web_target(principal, Permission::ExecuteAnyTask)
            }
            AuthIdentity::Service(token) if token.scopes.contains(&ServiceScope::TaskRead) => {
                token.owner_user_id.ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_credit_read(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ReadOwnCredits) => {
                Ok(principal.user_id)
            }
            AuthIdentity::Service(token) if token.scopes.contains(&ServiceScope::CreditRead) => {
                token.owner_user_id.ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_credit_grant(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::GrantCredits) => {
                Ok(principal.user_id)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_pricing_manage(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ManagePricing) => {
                Ok(principal.user_id)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_task_execute(&self) -> Result<(UserId, RequestSource), ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. }
                if principal.has(Permission::ExecuteOwnTasks)
                    || principal.has(Permission::ExecuteAnyTask) =>
            {
                Ok((
                    self.resolve_web_target(principal, Permission::ExecuteAnyTask)?,
                    RequestSource::WebUi,
                ))
            }
            AuthIdentity::Service(token) if token.scopes.contains(&ServiceScope::TaskExecute) => {
                let owner_id = token.owner_user_id.ok_or_else(ApiError::forbidden)?;
                let source = if token.scopes.contains(&ServiceScope::TaskCommandProxy) {
                    RequestSource::Yunzai
                } else {
                    RequestSource::Cli
                };
                Ok((owner_id, source))
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    fn resolve_web_target(
        &self,
        principal: &Principal,
        delegation_permission: Permission,
    ) -> Result<UserId, ApiError> {
        let Some(target) = self.target_owner_id else {
            return Ok(principal.user_id);
        };
        if target == principal.user_id || principal.has(delegation_permission) {
            Ok(target)
        } else {
            Err(ApiError::forbidden())
        }
    }

    pub(super) fn require_qq_identity_assert(
        &self,
        master_assertion: bool,
    ) -> Result<(), ApiError> {
        match &self.identity {
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::QqIdentityAssert) =>
            {
                if master_assertion && !token.scopes.contains(&ServiceScope::TaskCommandProxy) {
                    Err(ApiError::forbidden())
                } else {
                    Ok(())
                }
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_notification_delivery(&self) -> Result<(), ApiError> {
        match &self.identity {
            AuthIdentity::Service(token)
                if token
                    .scopes
                    .contains(&ServiceScope::NotificationDeliveryReport) =>
            {
                Ok(())
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_user_manage(&self) -> Result<UserId, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ManageUsers) => {
                Ok(principal.user_id)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    pub(super) fn require_audit_read(&self) -> Result<AuditAuthority, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ViewAnyAudit) => {
                Ok(AuditAuthority::Any)
            }
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ViewOwnAudit) => {
                Ok(AuditAuthority::Owner(principal.user_id))
            }
            AuthIdentity::Service(token) if token.scopes.contains(&ServiceScope::AuditRead) => {
                token
                    .owner_user_id
                    .map(AuditAuthority::Owner)
                    .ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    fn require_service_token_creation(
        &self,
        requested_scopes: &BTreeSet<ServiceScope>,
    ) -> Result<(), ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } => principal
                .require(Permission::ManageSystem)
                .map_err(|_| ApiError::forbidden()),
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ServiceTokenManage)
                    && requested_scopes.is_subset(&token.scopes) =>
            {
                Ok(())
            }
            AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }

    fn require_service_token_manage(&self) -> Result<ServiceTokenAuthority, ApiError> {
        match &self.identity {
            AuthIdentity::Web { principal, .. } if principal.has(Permission::ManageSystem) => {
                Ok(ServiceTokenAuthority::System)
            }
            AuthIdentity::Service(token)
                if token.scopes.contains(&ServiceScope::ServiceTokenManage) =>
            {
                token
                    .owner_user_id
                    .map(ServiceTokenAuthority::Owner)
                    .ok_or_else(ApiError::forbidden)
            }
            AuthIdentity::Web { .. } | AuthIdentity::Service(_) => Err(ApiError::forbidden()),
        }
    }
}

pub(super) async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token(request.headers()).or_else(|| session_cookie(request.headers()));
    let token = token.ok_or_else(ApiError::unauthorized)?;
    let now = Utc::now();
    let sessions = SqliteSessionRepository::new(state.database.clone());
    let identity = if token.expose_secret().starts_with("ast_st_") {
        let digest = OpaqueTokenService::new("ast_st")
            .expect("fixed token prefix is valid")
            .digest(&token);
        sessions
            .authenticate_service_token(&digest, now)
            .await
            .map_err(ApiError::internal)?
            .map(AuthIdentity::Service)
            .ok_or_else(ApiError::unauthorized)?
    } else if token.expose_secret().starts_with("ast_ws_") {
        let digest = OpaqueTokenService::new("ast_ws")
            .expect("fixed token prefix is valid")
            .digest(&token);
        let (session, user) = sessions
            .authenticate_web_session(&digest, now)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::unauthorized)?;
        AuthIdentity::Web {
            session_id: session.id,
            principal: Principal::from_roles(user.id, user.roles, user.permissions),
        }
    } else {
        return Err(ApiError::unauthorized());
    };
    let target_owner_id = request
        .headers()
        .get("x-asterism-target-owner")
        .map(|value| {
            let value = value.to_str().map_err(|_| {
                ApiError::bad_request("invalid_target_owner", "target owner header is invalid")
            })?;
            UserId::from_str(value).map_err(|_| {
                ApiError::bad_request("invalid_target_owner", "target owner ID is invalid")
            })
        })
        .transpose()?;
    request.extensions_mut().insert(AuthContext {
        identity,
        target_owner_id,
    });
    Ok(next.run(request).await)
}

pub(super) async fn bootstrap_master(
    State(state): State<ApiState>,
    payload: Result<Json<CredentialsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = api_json(payload)?;
    let username = validate_username(&request.username)?;
    validate_password(&request.password, true)?;
    let users = SqliteUserRepository::new(state.database.clone());
    if users
        .master_initialized()
        .await
        .map_err(ApiError::internal)?
    {
        return Err(master_already_initialized());
    }
    let password_hash = Argon2idPasswordService::default()
        .hash(&request.password)
        .map_err(ApiError::internal)?;
    let now = Utc::now();
    let user = users
        .bootstrap_master(&InitialMaster {
            id: UserId::new(),
            username: username.to_owned(),
            password_hash,
            created_at: now,
        })
        .await
        .map_err(|error| match error {
            StorageError::MasterAlreadyInitialized => master_already_initialized(),
            error => ApiError::internal(error),
        })?;
    create_session_response(&state, user, now).await
}

pub(super) async fn login(
    State(state): State<ApiState>,
    ConnectInfo(remote_address): ConnectInfo<SocketAddr>,
    payload: Result<Json<CredentialsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = api_json(payload)?;
    let username = validate_username(&request.username)?;
    validate_password(&request.password, false)?;
    state
        .login_rate_limiter
        .check_and_record(remote_address.ip(), username)
        .map_err(|error| ApiError::rate_limited(error.retry_after_seconds))?;
    let users = SqliteUserRepository::new(state.database.clone());
    let user = users
        .find_user_by_username(username)
        .await
        .map_err(ApiError::internal)?;
    let password_service = Argon2idPasswordService::default();
    let verified = if let Some(user) = &user {
        password_service
            .verify(&request.password, &user.password_hash)
            .map_err(ApiError::internal)?
    } else {
        password_service
            .hash(&request.password)
            .map_err(ApiError::internal)?;
        false
    };
    let Some(user) = user.filter(|user| verified && user.status == UserStatus::Active) else {
        return Err(ApiError::invalid_credentials());
    };
    create_session_response(&state, user, Utc::now()).await
}

pub(super) struct PasswordChangeRequest {
    current_password: Option<SecretString>,
    new_password: SecretString,
}

impl fmt::Debug for PasswordChangeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PasswordChangeRequest")
            .field("current_password", &self.current_password)
            .field("new_password", &"[REDACTED]")
            .finish()
    }
}

impl<'de> Deserialize<'de> for PasswordChangeRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            current_password: Option<String>,
            new_password: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            current_password: wire.current_password.map(SecretString::new),
            new_password: SecretString::new(wire.new_password),
        })
    }
}

/// Changes an existing password or initializes the password for a QQ-created
/// user whose authenticated Web session came from a QQ login ticket.
pub(super) async fn change_or_set_password(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<PasswordChangeRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let AuthIdentity::Web { principal, .. } = &auth.identity else {
        return Err(ApiError::forbidden());
    };
    let request = api_json(payload)?;
    validate_password(&request.new_password, true)?;
    let user = SqliteUserRepository::new(state.database.clone())
        .find_user(principal.user_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::unauthorized)?;
    let password_service = Argon2idPasswordService::default();
    let initialized: bool =
        sqlx::query_scalar("SELECT password_initialized = 1 FROM users WHERE id = ?")
            .bind(user.id.to_string())
            .fetch_one(state.database.pool())
            .await
            .map_err(ApiError::internal)?;
    if let Some(current) = request.current_password {
        validate_password(&current, false)?;
        if !password_service
            .verify(&current, &user.password_hash)
            .map_err(ApiError::internal)?
        {
            return Err(ApiError::invalid_credentials());
        }
    } else if initialized {
        return Err(ApiError::bad_request(
            "current_password_required",
            "current password is required",
        ));
    }
    let password_hash = password_service
        .hash(&request.new_password)
        .map_err(ApiError::internal)?;
    let now = Utc::now();
    let mut transaction = state
        .database
        .pool()
        .begin()
        .await
        .map_err(ApiError::internal)?;
    sqlx::query(
        "UPDATE users SET password_hash = ?, password_initialized = 1, updated_at = ? WHERE id = ?",
    )
    .bind(password_hash)
    .bind(now)
    .bind(user.id.to_string())
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    sqlx::query("UPDATE web_sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
        .bind(now)
        .bind(user.id.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO audit_records (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, outcome, metadata_sanitized_json) VALUES (?, ?, 'user', ?, 'password_changed', 'user', ?, 'succeeded', '{}')",
    )
    .bind(asterism_domain::AuditRecordId::new().to_string())
    .bind(now)
    .bind(user.id.to_string())
    .bind(user.id.to_string())
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        clear_session_cookie(state.secure_cookies),
    );
    Ok(no_store(response))
}

fn master_already_initialized() -> ApiError {
    ApiError::conflict(
        "master_already_initialized",
        "the initial master has already been created",
    )
}

pub(super) async fn current_identity(Extension(auth): Extension<AuthContext>) -> Response {
    let response = match auth.identity {
        AuthIdentity::Web { principal, .. } => IdentityResponse {
            identity_type: IdentityType::WebSession,
            user_id: Some(principal.user_id),
            service_token_id: None,
            scopes: BTreeSet::new(),
            permissions: principal.permissions().clone(),
        },
        AuthIdentity::Service(token) => IdentityResponse {
            identity_type: IdentityType::ServiceToken,
            user_id: token.owner_user_id,
            service_token_id: Some(token.id),
            scopes: token.scopes,
            permissions: BTreeSet::new(),
        },
    };
    no_store(Json(response).into_response())
}

pub(super) async fn logout(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    let actor = auth.audit_actor();
    let AuthIdentity::Web { session_id, .. } = auth.identity else {
        return Err(ApiError::bad_request(
            "not_web_session",
            "service tokens must be revoked instead of logged out",
        ));
    };
    SqliteSessionRepository::new(state.database)
        .revoke_web_session(session_id, Utc::now(), actor)
        .await
        .map_err(ApiError::internal)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        clear_session_cookie(state.secure_cookies),
    );
    Ok(no_store(response))
}

pub(super) async fn create_service_token(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<CreateServiceTokenRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = api_json(payload)?;
    auth.require_service_token_creation(&request.scopes)?;
    let name = request.name.trim();
    if name.is_empty() || name.len() > 128 || request.scopes.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_service_token",
            "service token name (1-128 bytes) and scopes are required",
        ));
    }
    let now = Utc::now();
    let expires_at = request
        .expires_in_seconds
        .map(|seconds| checked_expiry(now, seconds))
        .transpose()?;
    let token = ServiceToken {
        id: ServiceTokenId::new(),
        owner_user_id: auth.owner_user_id(),
        name: name.to_owned(),
        scopes: request.scopes,
        created_at: now,
        expires_at,
        revoked_at: None,
        last_used_at: None,
    };
    let token_service = OpaqueTokenService::new("ast_st").expect("fixed token prefix is valid");
    let (plaintext, digest) = token_service.generate();
    let actor = auth.audit_actor();
    SqliteSessionRepository::new(state.database)
        .create_service_token(&token, &digest, actor)
        .await
        .map_err(ApiError::internal)?;
    Ok(no_store(
        Json(CreateServiceTokenResponse {
            token: plaintext.expose_secret().to_owned(),
            metadata: token,
        })
        .into_response(),
    ))
}

pub(super) async fn list_service_tokens(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<ServiceTokenListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let authority = auth.require_service_token_manage()?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_service_token_query",
            "service token query parameters are invalid",
        )
    })?;
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > 200 || offset > 1_000_000 {
        return Err(ApiError::bad_request(
            "invalid_service_token_pagination",
            "limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    let owner_scope = match authority {
        ServiceTokenAuthority::System => None,
        ServiceTokenAuthority::Owner(user_id) => Some(user_id),
    };
    let page = SqliteSessionRepository::new(state.database)
        .list_service_tokens(owner_scope, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(no_store(
        Json(ServiceTokenPageResponse {
            items: page.items,
            total: page.total,
            limit,
            offset,
        })
        .into_response(),
    ))
}

pub(super) async fn revoke_service_token(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(token_id): Path<String>,
) -> Result<Response, ApiError> {
    let authority = auth.require_service_token_manage()?;
    let token_id = ServiceTokenId::from_str(&token_id).map_err(|_| {
        ApiError::bad_request("invalid_service_token_id", "service token ID is invalid")
    })?;
    let repository = SqliteSessionRepository::new(state.database);
    let token = repository
        .find_service_token(token_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|token| match authority {
            ServiceTokenAuthority::System => true,
            ServiceTokenAuthority::Owner(owner_id) => token.owner_user_id == Some(owner_id),
        })
        .ok_or_else(|| ApiError::not_found("service_token_not_found"))?;
    if token.revoked_at.is_some() {
        return Err(ApiError::not_found("service_token_not_found"));
    }
    let actor = auth.audit_actor();
    let revoked = repository
        .revoke_service_token(token_id, Utc::now(), actor)
        .await
        .map_err(ApiError::internal)?;
    if revoked {
        Ok(no_store(StatusCode::NO_CONTENT.into_response()))
    } else {
        Err(ApiError::not_found("service_token_not_found"))
    }
}

async fn create_session_response(
    state: &ApiState,
    user: User,
    now: Timestamp,
) -> Result<Response, ApiError> {
    let ttl = i64::try_from(state.session_ttl_seconds)
        .map_err(|_| ApiError::internal("session TTL does not fit i64"))?;
    let expires_at = now
        .checked_add_signed(
            Duration::try_seconds(ttl)
                .ok_or_else(|| ApiError::internal("session TTL is outside the timestamp range"))?,
        )
        .ok_or_else(|| ApiError::internal("session expiry is outside the timestamp range"))?;
    let token_service = OpaqueTokenService::new("ast_ws").expect("fixed token prefix is valid");
    let (plaintext, digest) = token_service.generate();
    SqliteSessionRepository::new(state.database.clone())
        .create_web_session(
            &WebSession {
                id: WebSessionId::new(),
                user_id: user.id,
                created_at: now,
                expires_at,
                revoked_at: None,
                last_used_at: None,
            },
            &digest,
            AuditActor::User(user.id),
        )
        .await
        .map_err(ApiError::internal)?;
    let secure = if state.secure_cookies { "; Secure" } else { "" };
    let mut cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        plaintext.expose_secret(),
        state.session_ttl_seconds,
        secure
    );
    let cookie_header = HeaderValue::from_str(&cookie);
    cookie.zeroize();
    let cookie_header = cookie_header.map_err(ApiError::internal)?;
    let mut response = Json(LoginResponse {
        user: UserSummary::from(user),
        expires_at,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_header);
    Ok(no_store(response))
}

pub(super) async fn create_session_redirect_response(
    state: &ApiState,
    user: User,
    now: Timestamp,
    return_to: &str,
) -> Result<Response, ApiError> {
    let response = create_session_response(state, user, now).await?;
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .cloned()
        .ok_or_else(|| ApiError::internal("created web session did not contain a cookie"))?;
    let location = HeaderValue::from_str(return_to).map_err(ApiError::internal)?;
    let mut redirect = StatusCode::SEE_OTHER.into_response();
    redirect.headers_mut().insert(header::SET_COOKIE, cookie);
    redirect.headers_mut().insert(header::LOCATION, location);
    Ok(no_store(redirect))
}

fn api_json<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_json",
            "the request body must be valid JSON with the expected fields",
        )
    })
}

pub(super) fn validate_username(username: &str) -> Result<&str, ApiError> {
    let username = username.trim();
    if username.is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || username.chars().any(char::is_control)
    {
        Err(ApiError::bad_request(
            "invalid_credentials_format",
            "username must contain 1-64 bytes without control characters",
        ))
    } else {
        Ok(username)
    }
}

pub(super) fn validate_password(
    password: &SecretString,
    require_minimum: bool,
) -> Result<(), ApiError> {
    let length = password.expose_secret().len();
    let minimum = if require_minimum {
        MIN_PASSWORD_BYTES
    } else {
        1
    };
    if (minimum..=MAX_PASSWORD_BYTES).contains(&length) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid_credentials_format",
            format!("password must contain {minimum}-{MAX_PASSWORD_BYTES} bytes"),
        ))
    }
}

fn checked_expiry(now: Timestamp, seconds: u64) -> Result<Timestamp, ApiError> {
    if seconds == 0 {
        return Err(ApiError::bad_request(
            "invalid_expiry",
            "expiry must be greater than zero",
        ));
    }
    let seconds = i64::try_from(seconds)
        .map_err(|_| ApiError::bad_request("invalid_expiry", "expiry is too large"))?;
    let duration = Duration::try_seconds(seconds)
        .ok_or_else(|| ApiError::bad_request("invalid_expiry", "expiry is too large"))?;
    now.checked_add_signed(duration)
        .ok_or_else(|| ApiError::bad_request("invalid_expiry", "expiry is too large"))
}

fn bearer_token(headers: &HeaderMap) -> Option<SecretString> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") || parts.next().is_some() {
        return None;
    }
    Some(SecretString::new(token))
}

fn session_cookie(headers: &HeaderMap) -> Option<SecretString> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            (name == SESSION_COOKIE && !value.is_empty()).then(|| SecretString::new(value))
        })
}

pub(super) fn require_provider_read(auth: &AuthContext) -> Result<(), ApiError> {
    auth.require(Permission::ReadProviders, Some(ServiceScope::ProviderRead))
}

pub(super) fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn clear_session_cookie(secure: bool) -> HeaderValue {
    if secure {
        HeaderValue::from_static(
            "asterism_session=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0",
        )
    } else {
        HeaderValue::from_static("asterism_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
    }
}

pub(super) struct CredentialsRequest {
    username: String,
    password: SecretString,
}

impl fmt::Debug for CredentialsRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialsRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl<'de> Deserialize<'de> for CredentialsRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            username: String,
            password: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            username: wire.username,
            password: SecretString::new(wire.password),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LoginResponse {
    pub user: UserSummary,
    pub expires_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserSummary {
    pub id: UserId,
    pub username: String,
    pub roles: Vec<asterism_domain::Role>,
}

impl From<User> for UserSummary {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            username: user.username,
            roles: user.roles,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IdentityResponse {
    pub identity_type: IdentityType,
    pub user_id: Option<UserId>,
    pub service_token_id: Option<ServiceTokenId>,
    pub scopes: BTreeSet<ServiceScope>,
    pub permissions: BTreeSet<Permission>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityType {
    WebSession,
    ServiceToken,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateServiceTokenRequest {
    name: String,
    scopes: BTreeSet<ServiceScope>,
    expires_in_seconds: Option<u64>,
}

#[derive(Serialize)]
struct CreateServiceTokenResponse {
    /// Returned once. Only its digest is stored.
    token: String,
    metadata: ServiceToken,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ServiceTokenListQuery {
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ServiceTokenPageResponse {
    items: Vec<ServiceToken>,
    total: u64,
    limit: u32,
    offset: u64,
}

impl fmt::Debug for CreateServiceTokenResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateServiceTokenResponse")
            .field("token", &"[REDACTED]")
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl Drop for CreateServiceTokenResponse {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use asterism_domain::Role;
    use axum::http::HeaderMap;

    use super::*;

    #[test]
    fn expiry_validation_never_overflows() {
        assert!(checked_expiry(Utc::now(), u64::MAX).is_err());
        assert!(checked_expiry(Utc::now(), i64::MAX as u64).is_err());
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_but_unambiguous() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer ast_st_example"),
        );
        assert_eq!(
            bearer_token(&headers).unwrap().expose_secret(),
            "ast_st_example"
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer ast_st_example trailing"),
        );
        assert!(bearer_token(&headers).is_none());
    }

    #[test]
    fn credential_debug_output_redacts_passwords() {
        let request: CredentialsRequest = serde_json::from_str(
            r#"{"username":"master","password":"correct-horse-battery-staple"}"#,
        )
        .unwrap();
        let debug = format!("{request:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("correct-horse-battery-staple"));
    }

    #[test]
    fn provider_runtime_settings_are_master_only_for_web_users() {
        let owner_id = UserId::new();
        for role in [Role::User, Role::Operator] {
            let auth = AuthContext {
                identity: AuthIdentity::Web {
                    session_id: WebSessionId::new(),
                    principal: Principal::from_roles(owner_id, [role], []),
                },
                target_owner_id: None,
            };
            assert!(auth.require_provider_settings_manage().is_err());
        }
        let master = AuthContext {
            identity: AuthIdentity::Web {
                session_id: WebSessionId::new(),
                principal: Principal::from_roles(owner_id, [Role::Master], []),
            },
            target_owner_id: None,
        };
        assert_eq!(
            master.require_provider_settings_manage().unwrap(),
            ProviderSettingsAuthority::System
        );
    }

    #[test]
    fn provider_manage_service_scope_is_required_for_settings_automation() {
        let owner_id = UserId::new();
        let service = |scopes| AuthContext {
            identity: AuthIdentity::Service(ServiceToken {
                id: ServiceTokenId::new(),
                owner_user_id: Some(owner_id),
                name: "settings-test".to_owned(),
                scopes,
                created_at: Utc::now(),
                expires_at: None,
                revoked_at: None,
                last_used_at: None,
            }),
            target_owner_id: None,
        };
        assert!(
            service(BTreeSet::from([ServiceScope::ProviderRead]))
                .require_provider_settings_manage()
                .is_err()
        );
        assert_eq!(
            service(BTreeSet::from([ServiceScope::ProviderManage]))
                .require_provider_settings_manage()
                .unwrap(),
            ProviderSettingsAuthority::Owner(owner_id)
        );
        let other_owner_id = UserId::new();
        assert!(ProviderSettingsAuthority::System.permits_owner(other_owner_id));
        assert!(!ProviderSettingsAuthority::Owner(owner_id).permits_owner(other_owner_id));
    }

    #[test]
    fn account_owners_can_manage_their_scan_schedule_but_not_global_provider_settings() {
        let owner_id = UserId::new();
        let auth = AuthContext {
            identity: AuthIdentity::Web {
                session_id: WebSessionId::new(),
                principal: Principal::from_roles(owner_id, [Role::User], []),
            },
            target_owner_id: None,
        };
        assert_eq!(
            auth.require_scan_schedule_manage().unwrap(),
            ProviderSettingsAuthority::Owner(owner_id)
        );
        assert!(auth.require_provider_settings_manage().is_err());
    }
}
