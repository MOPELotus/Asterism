use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, AuthSession, AuthSessionId, AuthState, ExternalOauthPending,
    ExternalOauthState, ProviderAccountId, ProviderId, Timestamp, UserId, WaitingUserState,
};
use chrono::{Duration, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::auth_session::{
    decode_timestamp, encode_timestamp, fetch_auth_session, mirror_account_state,
    update_auth_session_in_transaction,
};
use crate::{Database, ExternalOauthClaim, StorageError};

const SELECT_PENDING: &str = "SELECT auth_session_id, owner_user_id, provider_account_id, \
    provider_id, state_digest, provider_context_digest, state_json, revision, expires_at, \
    consumed_at, created_at, updated_at FROM external_oauth_pending";
const COMPLETING_RECOVERY_GRACE_SECONDS: i64 = 120;

pub(crate) async fn create_pending(
    database: &Database,
    pending: &ExternalOauthPending,
    waiting_session: &AuthSession,
    expected_auth_revision: u32,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), StorageError> {
    validate_correlation_id(correlation_id)?;
    pending
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if pending.state != ExternalOauthState::Pending
        || pending.auth_session_id != waiting_session.id
        || pending.owner_user_id != waiting_session.owner_user_id
        || pending.provider_account_id != waiting_session.provider_account_id
        || pending.created_at != waiting_session.created_at
        || pending.expires_at != waiting_session.expires_at
        || waiting_session.revision != expected_auth_revision.saturating_add(1)
        || !matches!(
            waiting_session.state,
            AuthState::WaitingUser(WaitingUserState::BrowserCallback)
        )
    {
        return Err(StorageError::InvalidData(
            "external OAuth pending binding does not match its waiting AuthSession".to_owned(),
        ));
    }
    let mut transaction = database.pool().begin_with("BEGIN IMMEDIATE").await?;
    let provider_id: Option<String> = sqlx::query_scalar(
        "SELECT provider_id FROM provider_accounts WHERE id = ? AND owner_user_id = ?",
    )
    .bind(pending.provider_account_id.to_string())
    .bind(pending.owner_user_id.to_string())
    .fetch_optional(&mut *transaction)
    .await?;
    if provider_id.as_deref() != Some(pending.provider_id.as_str()) {
        return Err(StorageError::InvalidData(
            "external OAuth pending Provider binding does not match its account".to_owned(),
        ));
    }
    insert_pending(&mut transaction, pending).await?;
    if !update_auth_session_in_transaction(
        &mut transaction,
        waiting_session,
        expected_auth_revision,
        actor,
        correlation_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Err(StorageError::InvalidData(
            "external OAuth AuthSession changed before pending creation".to_owned(),
        ));
    }
    mirror_account_state(&mut transaction, waiting_session).await?;
    insert_audit(
        &mut transaction,
        actor,
        "external_oauth_pending_created",
        correlation_id,
        pending,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub(crate) async fn find_pending(
    database: &Database,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    auth_session_id: AuthSessionId,
) -> Result<Option<ExternalOauthPending>, StorageError> {
    let query = format!(
        "{SELECT_PENDING} WHERE auth_session_id = ? AND owner_user_id = ? AND provider_account_id = ?"
    );
    sqlx::query(&query)
        .bind(auth_session_id.to_string())
        .bind(owner_user_id.to_string())
        .bind(provider_account_id.to_string())
        .fetch_optional(database.pool())
        .await?
        .as_ref()
        .map(decode_pending)
        .transpose()
}

pub(crate) async fn find_pending_by_state(
    database: &Database,
    owner_user_id: UserId,
    provider_id: &ProviderId,
    state_digest: [u8; 32],
) -> Result<Option<ExternalOauthPending>, StorageError> {
    let query = format!(
        "{SELECT_PENDING} WHERE owner_user_id = ? AND provider_id = ? AND state_digest = ? AND state_json = ?"
    );
    sqlx::query(&query)
        .bind(owner_user_id.to_string())
        .bind(provider_id.as_str())
        .bind(state_digest.to_vec())
        .bind(serde_json::to_string(&ExternalOauthState::Pending)?)
        .fetch_optional(database.pool())
        .await?
        .as_ref()
        .map(decode_pending)
        .transpose()
}

pub(crate) async fn claim_pending(
    database: &Database,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    auth_session_id: AuthSessionId,
    at: Timestamp,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<Option<ExternalOauthClaim>, StorageError> {
    validate_correlation_id(correlation_id)?;
    let mut transaction = database.pool().begin_with("BEGIN IMMEDIATE").await?;
    let query = format!(
        "{SELECT_PENDING} WHERE auth_session_id = ? AND owner_user_id = ? AND provider_account_id = ?"
    );
    let Some(row) = sqlx::query(&query)
        .bind(auth_session_id.to_string())
        .bind(owner_user_id.to_string())
        .bind(provider_account_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?
    else {
        transaction.rollback().await?;
        return Ok(None);
    };
    let mut pending = decode_pending(&row)?;
    if pending.state != ExternalOauthState::Pending || at >= pending.expires_at {
        transaction.rollback().await?;
        return Ok(None);
    }
    let Some(mut session) = fetch_auth_session(&mut transaction, auth_session_id).await? else {
        return Err(StorageError::InvalidData(
            "external OAuth pending AuthSession disappeared".to_owned(),
        ));
    };
    if session.owner_user_id != owner_user_id
        || session.provider_account_id != provider_account_id
        || !matches!(
            session.state,
            AuthState::WaitingUser(WaitingUserState::BrowserCallback)
        )
        || session.expires_at != pending.expires_at
    {
        return Err(StorageError::InvalidData(
            "external OAuth pending and AuthSession state diverged".to_owned(),
        ));
    }
    let expected_pending_revision = pending.revision;
    let expected_auth_revision = session.revision;
    match pending.claim(at) {
        Ok(()) => {}
        Err(
            asterism_domain::ExternalOauthPendingError::Expired
            | asterism_domain::ExternalOauthPendingError::InvalidTransition,
        ) => {
            transaction.rollback().await?;
            return Ok(None);
        }
        Err(error) => return Err(StorageError::InvalidData(error.to_string())),
    }
    session
        .transition(AuthState::ExchangingCredential, at)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if !update_pending_in_transaction(&mut transaction, &pending, expected_pending_revision).await?
        || !update_auth_session_in_transaction(
            &mut transaction,
            &session,
            expected_auth_revision,
            actor,
            correlation_id,
        )
        .await?
    {
        transaction.rollback().await?;
        return Ok(None);
    }
    mirror_account_state(&mut transaction, &session).await?;
    insert_audit(
        &mut transaction,
        actor,
        "external_oauth_callback_claimed",
        correlation_id,
        &pending,
    )
    .await?;
    transaction.commit().await?;
    Ok(Some(ExternalOauthClaim {
        pending,
        auth_session: session,
    }))
}

pub(crate) async fn recover_pending(
    database: &Database,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    auth_session_id: AuthSessionId,
    at: Timestamp,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<Option<ExternalOauthClaim>, StorageError> {
    validate_correlation_id(correlation_id)?;
    let mut transaction = database.pool().begin_with("BEGIN IMMEDIATE").await?;
    let query = format!(
        "{SELECT_PENDING} WHERE auth_session_id = ? AND owner_user_id = ? AND provider_account_id = ?"
    );
    let Some(row) = sqlx::query(&query)
        .bind(auth_session_id.to_string())
        .bind(owner_user_id.to_string())
        .bind(provider_account_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?
    else {
        transaction.rollback().await?;
        return Ok(None);
    };
    let mut pending = decode_pending(&row)?;
    let Some(mut session) = fetch_auth_session(&mut transaction, auth_session_id).await? else {
        return Err(StorageError::InvalidData(
            "external OAuth recovery AuthSession disappeared".to_owned(),
        ));
    };
    if session.owner_user_id != owner_user_id
        || session.provider_account_id != provider_account_id
        || session.expires_at != pending.expires_at
        || at < pending.updated_at
        || at < session.updated_at
    {
        return Err(StorageError::InvalidData(
            "external OAuth recovery binding or timestamp diverged".to_owned(),
        ));
    }

    let previous_pending = pending.clone();
    let previous_session = session.clone();
    reconcile_recovery(&mut pending, &mut session, at)?;
    let pending_changed = pending != previous_pending;
    let session_changed = session != previous_session;
    if !pending_changed && !session_changed {
        transaction.rollback().await?;
        return Ok(Some(ExternalOauthClaim {
            pending,
            auth_session: session,
        }));
    }
    if pending_changed
        && !update_pending_in_transaction(&mut transaction, &pending, previous_pending.revision)
            .await?
    {
        transaction.rollback().await?;
        return Ok(None);
    }
    if session_changed
        && !update_auth_session_in_transaction(
            &mut transaction,
            &session,
            previous_session.revision,
            actor,
            correlation_id,
        )
        .await?
    {
        transaction.rollback().await?;
        return Ok(None);
    }
    if session_changed {
        mirror_account_state(&mut transaction, &session).await?;
    }
    if pending_changed {
        insert_audit(
            &mut transaction,
            actor,
            "external_oauth_pending_recovered",
            correlation_id,
            &pending,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(Some(ExternalOauthClaim {
        pending,
        auth_session: session,
    }))
}

fn reconcile_recovery(
    pending: &mut ExternalOauthPending,
    session: &mut AuthSession,
    at: Timestamp,
) -> Result<(), StorageError> {
    match pending.state {
        ExternalOauthState::Pending => reconcile_unconsumed(pending, session, at),
        ExternalOauthState::Completing => reconcile_completing(pending, session, at),
        ExternalOauthState::Succeeded => {
            require_auth_state(session, |state| matches!(state, AuthState::Authenticated))
        }
        ExternalOauthState::Failed => {
            reconcile_terminal_session(session, at, AuthState::AuthFailed, |state| {
                matches!(
                    state,
                    AuthState::AuthFailed
                        | AuthState::HumanRequired(_)
                        | AuthState::ClientUpdateRequired
                        | AuthState::ProviderUnavailable
                        | AuthState::Expired
                        | AuthState::Cancelled
                )
            })
        }
        ExternalOauthState::Ambiguous => {
            reconcile_terminal_session(session, at, AuthState::ProviderUnavailable, |state| {
                matches!(state, AuthState::ProviderUnavailable | AuthState::Expired)
            })
        }
        ExternalOauthState::Expired => {
            reconcile_waiting_terminal(session, at, AuthState::Expired, |state| {
                matches!(state, AuthState::Expired)
            })
        }
        ExternalOauthState::Cancelled => {
            reconcile_waiting_terminal(session, at, AuthState::Cancelled, |state| {
                matches!(state, AuthState::Cancelled)
            })
        }
    }
}

fn reconcile_unconsumed(
    pending: &mut ExternalOauthPending,
    session: &mut AuthSession,
    at: Timestamp,
) -> Result<(), StorageError> {
    match session.state {
        AuthState::WaitingUser(WaitingUserState::BrowserCallback) if at < pending.expires_at => {
            Ok(())
        }
        AuthState::WaitingUser(WaitingUserState::BrowserCallback) | AuthState::Expired
            if at >= pending.expires_at =>
        {
            pending
                .expire(at)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            if !matches!(session.state, AuthState::Expired) {
                transition_recovered_session(session, AuthState::Expired, at)?;
            }
            Ok(())
        }
        AuthState::Cancelled => pending
            .cancel(at)
            .map_err(|error| StorageError::InvalidData(error.to_string())),
        _ => Err(StorageError::InvalidData(
            "unconsumed external OAuth callback diverged from AuthSession".to_owned(),
        )),
    }
}

fn reconcile_completing(
    pending: &mut ExternalOauthPending,
    session: &mut AuthSession,
    at: Timestamp,
) -> Result<(), StorageError> {
    if matches!(session.state, AuthState::Authenticated) {
        return pending
            .finish(ExternalOauthState::Succeeded, at)
            .map_err(|error| StorageError::InvalidData(error.to_string()));
    }
    if matches!(
        session.state,
        AuthState::ExchangingCredential | AuthState::ValidatingCredential
    ) && at.signed_duration_since(pending.updated_at)
        < Duration::seconds(COMPLETING_RECOVERY_GRACE_SECONDS)
    {
        return Ok(());
    }
    if matches!(session.state, AuthState::WaitingUser(_)) {
        return Err(StorageError::InvalidData(
            "consumed external OAuth callback returned to a waiting AuthSession".to_owned(),
        ));
    }
    pending
        .finish(ExternalOauthState::Ambiguous, at)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if matches!(
        session.state,
        AuthState::ExchangingCredential | AuthState::ValidatingCredential
    ) {
        let target = if session.is_expired_at(at) {
            AuthState::Expired
        } else {
            AuthState::ProviderUnavailable
        };
        transition_recovered_session(session, target, at)?;
    }
    Ok(())
}

fn reconcile_terminal_session(
    session: &mut AuthSession,
    at: Timestamp,
    target: AuthState,
    terminal_matches: impl FnOnce(&AuthState) -> bool,
) -> Result<(), StorageError> {
    if matches!(
        session.state,
        AuthState::ExchangingCredential | AuthState::ValidatingCredential
    ) {
        let target = if session.is_expired_at(at) {
            AuthState::Expired
        } else {
            target
        };
        return transition_recovered_session(session, target, at);
    }
    require_auth_state(session, terminal_matches)
}

fn reconcile_waiting_terminal(
    session: &mut AuthSession,
    at: Timestamp,
    target: AuthState,
    terminal_matches: impl FnOnce(&AuthState) -> bool,
) -> Result<(), StorageError> {
    if matches!(
        session.state,
        AuthState::WaitingUser(WaitingUserState::BrowserCallback)
    ) {
        return transition_recovered_session(session, target, at);
    }
    require_auth_state(session, terminal_matches)
}

fn require_auth_state(
    session: &AuthSession,
    predicate: impl FnOnce(&AuthState) -> bool,
) -> Result<(), StorageError> {
    if predicate(&session.state) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "external OAuth terminal state diverged from AuthSession".to_owned(),
        ))
    }
}

fn transition_recovered_session(
    session: &mut AuthSession,
    target: AuthState,
    at: Timestamp,
) -> Result<(), StorageError> {
    session
        .transition(target, at)
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

pub(crate) async fn update_pending(
    database: &Database,
    pending: &ExternalOauthPending,
    expected_revision: u32,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<bool, StorageError> {
    validate_correlation_id(correlation_id)?;
    pending
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if pending.revision != expected_revision.saturating_add(1)
        || pending.state == ExternalOauthState::Completing
    {
        return Err(StorageError::InvalidData(
            "external OAuth pending revision or update path is invalid".to_owned(),
        ));
    }
    let mut transaction = database.pool().begin_with("BEGIN IMMEDIATE").await?;
    let query = format!("{SELECT_PENDING} WHERE auth_session_id = ?");
    let Some(row) = sqlx::query(&query)
        .bind(pending.auth_session_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?
    else {
        transaction.rollback().await?;
        return Ok(false);
    };
    let current = decode_pending(&row)?;
    if current.revision != expected_revision || !replays_exact_transition(current, pending)? {
        transaction.rollback().await?;
        return Ok(false);
    }
    if !update_pending_in_transaction(&mut transaction, pending, expected_revision).await? {
        transaction.rollback().await?;
        return Ok(false);
    }
    insert_audit(
        &mut transaction,
        actor,
        "external_oauth_pending_finished",
        correlation_id,
        pending,
    )
    .await?;
    transaction.commit().await?;
    Ok(true)
}

fn replays_exact_transition(
    mut current: ExternalOauthPending,
    target: &ExternalOauthPending,
) -> Result<bool, StorageError> {
    match target.state {
        ExternalOauthState::Succeeded
        | ExternalOauthState::Failed
        | ExternalOauthState::Ambiguous => current.finish(target.state, target.updated_at),
        ExternalOauthState::Expired => current.expire(target.updated_at),
        ExternalOauthState::Cancelled => current.cancel(target.updated_at),
        ExternalOauthState::Pending | ExternalOauthState::Completing => {
            return Ok(false);
        }
    }
    .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(&current == target)
}

async fn insert_pending(
    transaction: &mut Transaction<'_, Sqlite>,
    pending: &ExternalOauthPending,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO external_oauth_pending (auth_session_id, owner_user_id, \
         provider_account_id, provider_id, state_digest, provider_context_digest, state_json, \
         revision, expires_at, consumed_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(pending.auth_session_id.to_string())
    .bind(pending.owner_user_id.to_string())
    .bind(pending.provider_account_id.to_string())
    .bind(pending.provider_id.as_str())
    .bind(pending.state_digest.to_vec())
    .bind(pending.provider_context_digest.to_vec())
    .bind(serde_json::to_string(&pending.state)?)
    .bind(i64::from(pending.revision))
    .bind(encode_timestamp(pending.expires_at))
    .bind(pending.consumed_at.map(encode_timestamp))
    .bind(encode_timestamp(pending.created_at))
    .bind(encode_timestamp(pending.updated_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn update_pending_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    pending: &ExternalOauthPending,
    expected_revision: u32,
) -> Result<bool, StorageError> {
    let result = sqlx::query(
        "UPDATE external_oauth_pending SET state_json = ?, revision = ?, consumed_at = ?, \
         updated_at = ? WHERE auth_session_id = ? AND owner_user_id = ? AND \
         provider_account_id = ? AND provider_id = ? AND state_digest = ? AND \
         provider_context_digest = ? AND expires_at = ? AND created_at = ? AND revision = ?",
    )
    .bind(serde_json::to_string(&pending.state)?)
    .bind(i64::from(pending.revision))
    .bind(pending.consumed_at.map(encode_timestamp))
    .bind(encode_timestamp(pending.updated_at))
    .bind(pending.auth_session_id.to_string())
    .bind(pending.owner_user_id.to_string())
    .bind(pending.provider_account_id.to_string())
    .bind(pending.provider_id.as_str())
    .bind(pending.state_digest.to_vec())
    .bind(pending.provider_context_digest.to_vec())
    .bind(encode_timestamp(pending.expires_at))
    .bind(encode_timestamp(pending.created_at))
    .bind(i64::from(expected_revision))
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected() == 1)
}

fn decode_pending(row: &SqliteRow) -> Result<ExternalOauthPending, StorageError> {
    let digest = |column: &str| -> Result<[u8; 32], StorageError> {
        row.try_get::<Vec<u8>, _>(column)?
            .try_into()
            .map_err(|_| StorageError::InvalidData("invalid external OAuth digest".to_owned()))
    };
    let pending = ExternalOauthPending {
        auth_session_id: AuthSessionId::from_str(row.try_get("auth_session_id")?)
            .map_err(|_| StorageError::InvalidData("invalid external OAuth session".to_owned()))?,
        owner_user_id: UserId::from_str(row.try_get("owner_user_id")?)
            .map_err(|_| StorageError::InvalidData("invalid external OAuth owner".to_owned()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|_| StorageError::InvalidData("invalid external OAuth account".to_owned()))?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| StorageError::InvalidData("invalid external OAuth Provider".to_owned()))?,
        state_digest: digest("state_digest")?,
        provider_context_digest: digest("provider_context_digest")?,
        state: serde_json::from_str(row.try_get("state_json")?)?,
        revision: u32::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| StorageError::InvalidData("invalid external OAuth revision".to_owned()))?,
        expires_at: decode_timestamp(row.try_get("expires_at")?)?,
        consumed_at: row
            .try_get::<Option<String>, _>("consumed_at")?
            .as_deref()
            .map(decode_timestamp)
            .transpose()?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    pending
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(pending)
}

async fn insert_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: AuditActor,
    action: &str,
    correlation_id: &str,
    pending: &ExternalOauthPending,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let metadata = serde_json::json!({
        "provider_account_id": pending.provider_account_id,
        "provider_id": pending.provider_id,
        "state": pending.state,
        "revision": pending.revision,
        "expires_at": pending.expires_at,
        "consumed": pending.consumed_at.is_some(),
    });
    sqlx::query(
        "INSERT INTO audit_records (id, occurred_at, actor_type, actor_id, action, \
         resource_type, resource_id, correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'external_oauth_pending', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(Utc::now()))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(pending.auth_session_id.to_string())
    .bind(correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_correlation_id(correlation_id: &str) -> Result<(), StorageError> {
    if correlation_id.is_empty()
        || correlation_id.len() > 128
        || correlation_id.trim() != correlation_id
        || correlation_id.chars().any(char::is_control)
    {
        Err(StorageError::InvalidData(
            "external OAuth correlation ID is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}
