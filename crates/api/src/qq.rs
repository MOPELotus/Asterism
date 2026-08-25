use std::collections::BTreeSet;

use asterism_auth::{Argon2idPasswordService, OpaqueTokenService};
use asterism_domain::{
    Permission, Role, Timestamp, User, UserId, UserStatus, WebSession, WebSessionId,
};
use asterism_secrets::SecretString;
use asterism_storage::{
    SessionRepository, SqliteAdminRepository, SqliteSessionRepository, SqliteUserRepository,
    UserAdminCreate, UserAdminCreateOutcome, UserAdminRepository, UserRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{ApiError, ApiState, auth::AuthContext};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssertQqIdentityRequest {
    qq: String,
    #[serde(default = "default_create_if_missing")]
    create_if_missing: bool,
    #[serde(default)]
    return_to: Option<String>,
}

const fn default_create_if_missing() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub(super) struct AssertQqIdentityResponse {
    user_id: UserId,
    username: String,
    qq: String,
    created: bool,
    access_token: String,
    expires_at: Timestamp,
    web_login_path: String,
    web_login_expires_at: Timestamp,
}

impl Drop for AssertQqIdentityResponse {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.web_login_path.zeroize();
    }
}

pub(super) async fn assert_qq_identity(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<AssertQqIdentityRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    auth.require_qq_identity_assert()?;
    let request = payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_qq_identity_assertion",
            "QQ identity assertion body is invalid",
        )
    })?;
    let qq = parse_qq(&request.qq)?;
    let return_to = validate_return_to(request.return_to.as_deref().unwrap_or("/"))?;
    let now = Utc::now();
    let (user, created) =
        find_or_create_user(&state, &auth, qq, request.create_if_missing, now).await?;
    if user.status != UserStatus::Active {
        return Err(ApiError::forbidden());
    }

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
            auth.audit_actor(),
        )
        .await
        .map_err(ApiError::internal)?;
    let ticket_service =
        OpaqueTokenService::new("ast_qq_login").expect("fixed token prefix is valid");
    let (ticket, ticket_digest) = ticket_service.generate();
    let web_login_expires_at = now + Duration::minutes(10);
    sqlx::query(
        "INSERT INTO qq_web_login_tickets \
         (id, user_id, token_hash, return_to, created_at, expires_at, consumed_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(user.id.to_string())
    .bind(ticket_digest.as_bytes().as_slice())
    .bind(return_to)
    .bind(now.to_rfc3339())
    .bind(web_login_expires_at.to_rfc3339())
    .execute(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let web_login_path = format!("/api/v1/auth/qq-login/{}", ticket.expose_secret());
    Ok(crate::auth::no_store(
        Json(AssertQqIdentityResponse {
            user_id: user.id,
            username: user.username,
            qq: qq.to_string(),
            created,
            access_token: plaintext.expose_secret().to_owned(),
            expires_at,
            web_login_path,
            web_login_expires_at,
        })
        .into_response(),
    ))
}

pub(super) async fn consume_qq_web_login(
    State(state): State<ApiState>,
    Path(ticket): Path<String>,
) -> Result<Response, ApiError> {
    let ticket = SecretString::new(ticket);
    let digest = OpaqueTokenService::new("ast_qq_login")
        .expect("fixed token prefix is valid")
        .digest(&ticket);
    let now = Utc::now();
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE qq_web_login_tickets SET consumed_at = ? \
         WHERE token_hash = ? AND consumed_at IS NULL AND expires_at > ? \
         RETURNING user_id, return_to",
    )
    .bind(now.to_rfc3339())
    .bind(digest.as_bytes().as_slice())
    .bind(now.to_rfc3339())
    .fetch_optional(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let (user_id, return_to) = row.ok_or_else(|| {
        ApiError::bad_request(
            "qq_web_login_invalid",
            "QQ Web login link is invalid, expired, or already used",
        )
    })?;
    let user_id = user_id
        .parse()
        .map_err(|_| ApiError::internal("stored QQ web login user ID is invalid"))?;
    let user = SqliteUserRepository::new(state.database.clone())
        .find_user(user_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|user| user.status == UserStatus::Active)
        .ok_or_else(ApiError::forbidden)?;
    crate::auth::create_session_redirect_response(&state, user, now, &return_to).await
}

async fn find_or_create_user(
    state: &ApiState,
    auth: &AuthContext,
    qq: u64,
    create_if_missing: bool,
    now: Timestamp,
) -> Result<(User, bool), ApiError> {
    if let Some(user) = find_qq_user(state, qq).await? {
        return Ok((user, false));
    }
    if !create_if_missing {
        return Err(ApiError::not_found("qq_identity_not_found"));
    }

    let username = qq.to_string();
    let users = SqliteUserRepository::new(state.database.clone());
    let user = if let Some(existing) = users
        .find_user_by_username(&username)
        .await
        .map_err(ApiError::internal)?
    {
        existing
    } else {
        let password_seed = OpaqueTokenService::new("ast_qq_seed")
            .expect("fixed token prefix is valid")
            .generate()
            .0;
        let password_hash = Argon2idPasswordService::default()
            .hash(&SecretString::new(password_seed.expose_secret().to_owned()))
            .map_err(ApiError::internal)?;
        let user = User {
            id: UserId::new(),
            username: username.clone(),
            password_hash,
            status: UserStatus::Active,
            roles: vec![Role::User],
            permissions: BTreeSet::from([
                Permission::ManageOwnAccounts,
                Permission::ReadOwnTasks,
                Permission::ReadOwnCredits,
                Permission::ExecuteOwnTasks,
                Permission::ViewOwnAudit,
            ])
            .into_iter()
            .collect(),
            created_at: now,
            updated_at: now,
        };
        match SqliteAdminRepository::new(state.database.clone())
            .create_user(UserAdminCreate {
                user: &user,
                actor: auth.audit_actor(),
                correlation_id: "qq-auto-registration",
            })
            .await
            .map_err(ApiError::internal)?
        {
            UserAdminCreateOutcome::Created(_) => user,
            UserAdminCreateOutcome::UsernameConflict => users
                .find_user_by_username(&username)
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::internal("QQ user conflict could not be resolved"))?,
        }
    };

    let qq_i64 = i64::try_from(qq).map_err(|_| {
        ApiError::bad_request("invalid_qq", "QQ number is outside the supported range")
    })?;
    let result = sqlx::query(
        "INSERT INTO qq_identities (user_id, qq, verified_at, is_primary) VALUES (?, ?, ?, 1) \
         ON CONFLICT(qq) DO NOTHING",
    )
    .bind(user.id.to_string())
    .bind(qq_i64)
    .bind(now.to_rfc3339())
    .execute(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    if result.rows_affected() == 0 {
        let winner = find_qq_user(state, qq)
            .await?
            .ok_or_else(|| ApiError::internal("QQ binding conflict could not be resolved"))?;
        return Ok((winner, false));
    }
    Ok((user, true))
}

async fn find_qq_user(state: &ApiState, qq: u64) -> Result<Option<User>, ApiError> {
    let qq = i64::try_from(qq).map_err(|_| {
        ApiError::bad_request("invalid_qq", "QQ number is outside the supported range")
    })?;
    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM qq_identities WHERE qq = ?")
            .bind(qq)
            .fetch_optional(state.database.pool())
            .await
            .map_err(ApiError::internal)?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    let user_id = user_id
        .parse()
        .map_err(|_| ApiError::internal("stored QQ user ID is invalid"))?;
    SqliteUserRepository::new(state.database.clone())
        .find_user(user_id)
        .await
        .map_err(ApiError::internal)
}

fn parse_qq(value: &str) -> Result<u64, ApiError> {
    let value = value.trim();
    if !(5..=20).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiError::bad_request(
            "invalid_qq",
            "QQ number must contain 5-20 decimal digits",
        ));
    }
    value
        .parse()
        .map_err(|_| ApiError::bad_request("invalid_qq", "QQ number is invalid"))
}

fn validate_return_to(value: &str) -> Result<&str, ApiError> {
    if value.starts_with('/')
        && !value.starts_with("//")
        && value.len() <= 2048
        && value.is_ascii()
        && !value.chars().any(char::is_control)
    {
        Ok(value)
    } else {
        Err(ApiError::bad_request(
            "invalid_return_to",
            "return_to must be a safe relative WebUI path",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qq_assertion_accepts_only_bounded_decimal_identity() {
        assert_eq!(parse_qq(" 123456 ").unwrap(), 123_456);
        for invalid in ["1234", "12345a", "-12345", "18446744073709551616"] {
            assert!(parse_qq(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn qq_login_return_to_is_relative_and_never_external() {
        assert_eq!(validate_return_to("/").unwrap(), "/");
        assert_eq!(
            validate_return_to("/tasks/task-1?confirm=1").unwrap(),
            "/tasks/task-1?confirm=1"
        );
        for invalid in [
            "https://evil.example/steal",
            "//evil.example/steal",
            "tasks/task-1",
            "/tasks/\nnext",
            "/tasks/🚀",
        ] {
            assert!(validate_return_to(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(validate_return_to(&format!("/{}", "x".repeat(2048))).is_err());
    }
}
