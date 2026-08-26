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
use sqlx::Row;
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
    let return_to = if created {
        "/settings/password".to_owned()
    } else {
        return_to.to_owned()
    };
    let (web_login_path, web_login_expires_at) =
        create_web_login_ticket(&state, user.id, &return_to, now).await?;
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

#[derive(Debug, Serialize)]
pub(super) struct QqFormalNotification {
    id: String,
    kind: String,
    qq: String,
    task_id: String,
    title: String,
    closes_at: String,
    message: String,
    web_login_path: String,
}

#[derive(Debug, Serialize)]
pub(super) struct QqFormalNotificationPage {
    items: Vec<QqFormalNotification>,
}

pub(super) async fn claim_qq_formal_notifications(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    auth.require_notification_delivery()?;
    let now = Utc::now();
    let window_end = now + Duration::hours(24);
    let recent_deadline_start = now - Duration::hours(24);
    let stale_claim = now - Duration::minutes(5);
    let mut transaction = state
        .database
        .pool()
        .begin()
        .await
        .map_err(ApiError::internal)?;
    sqlx::query(
        "UPDATE qq_formal_notification_deliveries \
         SET state = 'retry', next_attempt_at = ?, updated_at = ? \
         WHERE state = 'claimed' AND claimed_at <= ?",
    )
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .bind(stale_claim.to_rfc3339())
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    let candidates = sqlx::query(
        "SELECT tasks.id AS task_id, accounts.owner_user_id AS user_id, identities.qq \
         FROM tasks \
         INNER JOIN provider_accounts AS accounts ON accounts.id = tasks.provider_account_id \
         INNER JOIN qq_identities AS identities ON identities.user_id = accounts.owner_user_id AND identities.is_primary = 1 \
         WHERE tasks.assessment_class = 'formal' \
           AND tasks.closes_at IS NOT NULL AND tasks.closes_at > ? AND tasks.closes_at <= ? \
           AND tasks.remote_state NOT IN ('completed', 'removed')",
    )
    .bind(now.to_rfc3339())
    .bind(window_end.to_rfc3339())
    .fetch_all(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    for row in candidates {
        sqlx::query(
            "INSERT OR IGNORE INTO qq_formal_notification_deliveries \
             (id, task_id, user_id, qq, notification_kind, deduplication_key, state, attempts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'confirmation_due', ?, 'pending', 0, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(
            row.try_get::<String, _>("task_id")
                .map_err(ApiError::internal)?,
        )
        .bind(
            row.try_get::<String, _>("user_id")
                .map_err(ApiError::internal)?,
        )
        .bind(row.try_get::<i64, _>("qq").map_err(ApiError::internal)?)
        .bind(row.try_get::<String, _>("task_id").map_err(ApiError::internal)?)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    }
    let missed = sqlx::query(
        "SELECT tasks.id AS task_id, accounts.owner_user_id AS user_id, identities.qq \
         FROM tasks \
         INNER JOIN provider_accounts AS accounts ON accounts.id = tasks.provider_account_id \
         INNER JOIN qq_identities AS identities ON identities.user_id = accounts.owner_user_id AND identities.is_primary = 1 \
         WHERE tasks.assessment_class = 'formal' \
           AND tasks.closes_at IS NOT NULL AND tasks.closes_at > ? AND tasks.closes_at <= ? \
           AND tasks.remote_state NOT IN ('completed', 'removed')",
    )
    .bind(recent_deadline_start.to_rfc3339())
    .bind(now.to_rfc3339())
    .fetch_all(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    for row in missed {
        sqlx::query(
            "INSERT OR IGNORE INTO qq_formal_notification_deliveries \
             (id, task_id, user_id, qq, notification_kind, deduplication_key, state, attempts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'deadline_missed', ?, 'pending', 0, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(row.try_get::<String, _>("task_id").map_err(ApiError::internal)?)
        .bind(row.try_get::<String, _>("user_id").map_err(ApiError::internal)?)
        .bind(row.try_get::<i64, _>("qq").map_err(ApiError::internal)?)
        .bind(row.try_get::<String, _>("task_id").map_err(ApiError::internal)?)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    }
    let running_executions = sqlx::query(
        "SELECT execution.id AS execution_id, tasks.id AS task_id, \
                accounts.owner_user_id AS user_id, identities.qq, progress.percent \
         FROM executions AS execution \
         INNER JOIN execution_progress AS progress ON progress.execution_id = execution.id \
         INNER JOIN tasks ON tasks.id = execution.task_id \
         INNER JOIN provider_accounts AS accounts ON accounts.id = tasks.provider_account_id \
         INNER JOIN qq_identities AS identities ON identities.user_id = accounts.owner_user_id AND identities.is_primary = 1 \
         WHERE execution.state = 'running' AND progress.percent BETWEEN 25 AND 99",
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    for row in running_executions {
        let execution_id: String = row.try_get("execution_id").map_err(ApiError::internal)?;
        let percent = row
            .try_get::<i64, _>("percent")
            .map_err(ApiError::internal)?;
        let bucket = (percent / 25).clamp(1, 3);
        sqlx::query(
            "INSERT INTO qq_formal_notification_deliveries \
             (id, task_id, execution_id, user_id, qq, notification_kind, deduplication_key, state, attempts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'execution_progress', ?, 'pending', 0, ?, ?) \
             ON CONFLICT(user_id, notification_kind, deduplication_key) DO NOTHING",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(row.try_get::<String, _>("task_id").map_err(ApiError::internal)?)
        .bind(&execution_id)
        .bind(row.try_get::<String, _>("user_id").map_err(ApiError::internal)?)
        .bind(row.try_get::<i64, _>("qq").map_err(ApiError::internal)?)
        .bind(format!("{execution_id}:{bucket}"))
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    }
    let terminal_executions = sqlx::query(
        "SELECT execution.id AS execution_id, tasks.id AS task_id, accounts.owner_user_id AS user_id, identities.qq, execution.state \
         FROM executions AS execution \
         INNER JOIN tasks ON tasks.id = execution.task_id \
         INNER JOIN provider_accounts AS accounts ON accounts.id = tasks.provider_account_id \
         INNER JOIN qq_identities AS identities ON identities.user_id = accounts.owner_user_id AND identities.is_primary = 1 \
         WHERE execution.state IN ('succeeded', 'failed') \
           AND execution.finished_at IS NOT NULL AND execution.finished_at > ?",
    )
    .bind(recent_deadline_start.to_rfc3339())
    .fetch_all(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    for row in terminal_executions {
        let execution_id: String = row.try_get("execution_id").map_err(ApiError::internal)?;
        let notification_kind = if row
            .try_get::<String, _>("state")
            .map_err(ApiError::internal)?
            == "succeeded"
        {
            "execution_succeeded"
        } else {
            "execution_failed"
        };
        sqlx::query(
            "INSERT OR IGNORE INTO qq_formal_notification_deliveries \
             (id, task_id, execution_id, user_id, qq, notification_kind, deduplication_key, state, attempts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(row.try_get::<String, _>("task_id").map_err(ApiError::internal)?)
        .bind(&execution_id)
        .bind(row.try_get::<String, _>("user_id").map_err(ApiError::internal)?)
        .bind(row.try_get::<i64, _>("qq").map_err(ApiError::internal)?)
        .bind(notification_kind)
        .bind(&execution_id)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    }
    let rows = sqlx::query(
        "SELECT delivery.id, delivery.notification_kind, delivery.execution_id, delivery.user_id, delivery.qq, tasks.id AS task_id, tasks.title, \
                COALESCE(tasks.closes_at, execution.finished_at, progress.updated_at, delivery.created_at) AS event_at, \
                progress.percent, progress.stage, progress.status_text \
         FROM qq_formal_notification_deliveries AS delivery \
         INNER JOIN tasks ON tasks.id = delivery.task_id \
         LEFT JOIN executions AS execution ON execution.id = delivery.execution_id \
         LEFT JOIN execution_progress AS progress ON progress.execution_id = delivery.execution_id \
         WHERE delivery.state = 'pending' \
            OR (delivery.state = 'retry' AND (delivery.next_attempt_at IS NULL OR delivery.next_attempt_at <= ?)) \
         ORDER BY event_at, delivery.created_at LIMIT 50",
    )
    .bind(now.to_rfc3339())
    .fetch_all(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    let mut claimed = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id").map_err(ApiError::internal)?;
        let updated = sqlx::query(
            "UPDATE qq_formal_notification_deliveries \
             SET state = 'claimed', attempts = attempts + 1, claimed_at = ?, updated_at = ? \
             WHERE id = ? AND state IN ('pending', 'retry')",
        )
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(&id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        if updated.rows_affected() == 1 {
            claimed.push((
                id,
                row.try_get::<String, _>("notification_kind")
                    .map_err(ApiError::internal)?,
                row.try_get::<Option<String>, _>("execution_id")
                    .map_err(ApiError::internal)?,
                row.try_get::<String, _>("user_id")
                    .map_err(ApiError::internal)?,
                row.try_get::<i64, _>("qq").map_err(ApiError::internal)?,
                row.try_get::<String, _>("task_id")
                    .map_err(ApiError::internal)?,
                row.try_get::<String, _>("title")
                    .map_err(ApiError::internal)?,
                row.try_get::<String, _>("event_at")
                    .map_err(ApiError::internal)?,
                row.try_get::<Option<i64>, _>("percent")
                    .map_err(ApiError::internal)?,
                row.try_get::<Option<String>, _>("stage")
                    .map_err(ApiError::internal)?,
                row.try_get::<Option<String>, _>("status_text")
                    .map_err(ApiError::internal)?,
            ));
        }
    }
    transaction.commit().await.map_err(ApiError::internal)?;
    let mut items = Vec::with_capacity(claimed.len());
    for (
        id,
        kind,
        execution_id,
        user_id,
        qq,
        task_id,
        title,
        closes_at,
        percent,
        stage,
        status_text,
    ) in claimed
    {
        let user_id = user_id
            .parse()
            .map_err(|_| ApiError::internal("stored notification user ID is invalid"))?;
        let return_to = match (kind.as_str(), execution_id.as_deref()) {
            ("confirmation_due", _) => format!("/tasks/{task_id}?confirm=1"),
            (
                "execution_progress" | "execution_succeeded" | "execution_failed",
                Some(execution_id),
            ) => {
                format!("/executions/{execution_id}")
            }
            _ => format!("/tasks/{task_id}"),
        };
        let (web_login_path, _) = create_web_login_ticket(&state, user_id, &return_to, now).await?;
        items.push(QqFormalNotification {
            id,
            kind: kind.clone(),
            qq: qq.to_string(),
            task_id,
            title: title.clone(),
            closes_at: closes_at.clone(),
            message: match kind.as_str() {
                "confirmation_due" => format!("独立作业/考试“{title}”尚待提交前确认，截止时间 {closes_at}。截止后不会自动提交。"),
                "deadline_missed" => format!("独立作业/考试“{title}”已于 {closes_at} 截止且未确认提交；Asterism 已保留草稿且没有自动提交。"),
                "execution_progress" => format!(
                    "任务“{title}”当前进度 {}%，阶段：{}{}。",
                    percent.unwrap_or(0),
                    stage.as_deref().unwrap_or("执行中"),
                    status_text.as_deref().map(|value| format!("（{value}）")).unwrap_or_default(),
                ),
                "execution_succeeded" => format!("任务“{title}”已完成，可打开执行详情查看进度和日志。"),
                "execution_failed" => format!("任务“{title}”执行失败，请打开执行详情查看原因和重试状态。"),
                _ => "Asterism 任务状态已更新。".to_owned(),
            },
            web_login_path,
        });
    }
    Ok(crate::auth::no_store(
        Json(QqFormalNotificationPage { items }).into_response(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QqNotificationDeliveryReport {
    items: Vec<QqNotificationDeliveryResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QqNotificationDeliveryResult {
    id: String,
    delivered: bool,
    error: Option<String>,
}

pub(super) async fn report_qq_formal_notifications(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<QqNotificationDeliveryReport>, JsonRejection>,
) -> Result<Response, ApiError> {
    auth.require_notification_delivery()?;
    let report = payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_notification_report",
            "notification report body is invalid",
        )
    })?;
    if report.items.is_empty() || report.items.len() > 100 {
        return Err(ApiError::bad_request(
            "invalid_notification_report",
            "notification report must contain 1-100 items",
        ));
    }
    let now = Utc::now();
    let retry_at = now + Duration::minutes(5);
    let mut transaction = state
        .database
        .pool()
        .begin()
        .await
        .map_err(ApiError::internal)?;
    for item in report.items {
        let error = item.error.as_deref().map(sanitize_delivery_error);
        let updated = if item.delivered {
            sqlx::query(
                "UPDATE qq_formal_notification_deliveries \
                 SET state = 'delivered', delivered_at = ?, last_error = NULL, updated_at = ? \
                 WHERE id = ? AND state = 'claimed'",
            )
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .bind(&item.id)
            .execute(&mut *transaction)
            .await
            .map_err(ApiError::internal)?
        } else {
            sqlx::query(
                "UPDATE qq_formal_notification_deliveries \
                 SET state = 'retry', next_attempt_at = ?, last_error = ?, updated_at = ? \
                 WHERE id = ? AND state = 'claimed'",
            )
            .bind(retry_at.to_rfc3339())
            .bind(error.unwrap_or_else(|| "delivery_failed".to_owned()))
            .bind(now.to_rfc3339())
            .bind(&item.id)
            .execute(&mut *transaction)
            .await
            .map_err(ApiError::internal)?
        };
        if updated.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "notification_delivery_conflict",
                "notification is not currently claimed",
            ));
        }
    }
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        axum::http::StatusCode::NO_CONTENT.into_response(),
    ))
}

async fn create_web_login_ticket(
    state: &ApiState,
    user_id: UserId,
    return_to: &str,
    now: Timestamp,
) -> Result<(String, Timestamp), ApiError> {
    let return_to = validate_return_to(return_to)?;
    let ticket_service =
        OpaqueTokenService::new("ast_qq_login").expect("fixed token prefix is valid");
    let (ticket, ticket_digest) = ticket_service.generate();
    let expires_at = now + Duration::minutes(10);
    sqlx::query(
        "INSERT INTO qq_web_login_tickets \
         (id, user_id, token_hash, return_to, created_at, expires_at, consumed_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(user_id.to_string())
    .bind(ticket_digest.as_bytes().as_slice())
    .bind(return_to)
    .bind(now.to_rfc3339())
    .bind(expires_at.to_rfc3339())
    .execute(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    Ok((
        format!("/api/v1/auth/qq-login/{}", ticket.expose_secret()),
        expires_at,
    ))
}

fn sanitize_delivery_error(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect::<String>()
        .trim()
        .to_owned()
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
    let mut created_user = false;
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
            UserAdminCreateOutcome::Created(_) => {
                created_user = true;
                user
            }
            UserAdminCreateOutcome::UsernameConflict => users
                .find_user_by_username(&username)
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::internal("QQ user conflict could not be resolved"))?,
        }
    };

    if created_user {
        sqlx::query("UPDATE users SET password_initialized = 0 WHERE id = ? AND username = ?")
            .bind(user.id.to_string())
            .bind(&username)
            .execute(state.database.pool())
            .await
            .map_err(ApiError::internal)?;
    }

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

    #[test]
    fn notification_delivery_errors_are_bounded_and_credential_free() {
        let value = sanitize_delivery_error(" failed\nwith secret-like text ");
        assert_eq!(value, "failedwith secret-like text");
        assert!(sanitize_delivery_error(&"x".repeat(600)).len() <= 512);
    }
}
