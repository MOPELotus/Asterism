use std::str::FromStr;

use asterism_auth::TokenDigest;
use asterism_domain::{
    AuditActor, AuditRecordId, AuthBootstrapClientEvent, AuthBootstrapSession,
    AuthBootstrapSessionId, AuthBootstrapState, ProviderId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    AuthBootstrapClientEventRecord, AuthBootstrapSessionRepository, Database, StorageError,
};

const AUTH_BOOTSTRAP_SELECT: &str = "SELECT id, owner_user_id, provider_id, provider_account_id, \
    purpose_json, required_recipe_version, state_json, revision, expires_at, claimed_at, \
    created_at, updated_at FROM auth_bootstrap_sessions";

#[derive(Clone, Debug)]
pub struct SqliteAuthBootstrapSessionRepository {
    database: Database,
}

impl SqliteAuthBootstrapSessionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl AuthBootstrapSessionRepository for SqliteAuthBootstrapSessionRepository {
    async fn create_auth_bootstrap_session(
        &self,
        session: &AuthBootstrapSession,
        pairing_token_digest: &TokenDigest,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError> {
        validate_initial_session(session, correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        ensure_bootstrap_binding(&mut transaction, session).await?;
        sqlx::query(
            "INSERT INTO auth_bootstrap_sessions \
             (id, owner_user_id, provider_id, provider_account_id, purpose_json, \
              required_recipe_version, state_json, pairing_token_hash, access_token_hash, \
              revision, expires_at, claimed_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(session.owner_user_id.to_string())
        .bind(session.provider_id.as_str())
        .bind(session.provider_account_id.map(|id| id.to_string()))
        .bind(serde_json::to_string(&session.purpose)?)
        .bind(i64::from(session.required_recipe_version))
        .bind(serde_json::to_string(&session.state)?)
        .bind(pairing_token_digest.as_bytes().as_slice())
        .bind(i64::from(session.revision))
        .bind(encode_timestamp(session.expires_at))
        .bind(encode_timestamp(session.created_at))
        .bind(encode_timestamp(session.updated_at))
        .execute(&mut *transaction)
        .await?;
        insert_bootstrap_audit(
            &mut transaction,
            actor_type(actor),
            "auth_bootstrap_session_created",
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_auth_bootstrap_session(
        &self,
        owner_user_id: UserId,
        session_id: AuthBootstrapSessionId,
    ) -> Result<Option<AuthBootstrapSession>, StorageError> {
        let query = format!("{AUTH_BOOTSTRAP_SELECT} WHERE id = ? AND owner_user_id = ?");
        sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_auth_bootstrap_session)
            .transpose()
    }

    async fn claim_auth_bootstrap_session(
        &self,
        session_id: AuthBootstrapSessionId,
        pairing_token_digest: &TokenDigest,
        access_token_digest: &TokenDigest,
        claimed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<Option<AuthBootstrapSession>, StorageError> {
        validate_correlation_id(correlation_id)?;
        if pairing_token_digest == access_token_digest {
            return Err(StorageError::InvalidData(
                "pairing and access tokens must be independently generated".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let query = format!(
            "{AUTH_BOOTSTRAP_SELECT} WHERE id = ? AND pairing_token_hash = ? \
             AND access_token_hash IS NULL AND state_json = ? AND expires_at > ?"
        );
        let Some(row) = sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(pairing_token_digest.as_bytes().as_slice())
            .bind(serde_json::to_string(&AuthBootstrapState::AwaitingClaim)?)
            .bind(encode_timestamp(claimed_at))
            .fetch_optional(&mut *transaction)
            .await?
        else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let mut session = decode_auth_bootstrap_session(&row)?;
        ensure_bootstrap_binding(&mut transaction, &session).await?;
        let expected_revision = session.revision;
        session
            .claim(claimed_at)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let result = sqlx::query(
            "UPDATE auth_bootstrap_sessions SET state_json = ?, pairing_token_hash = NULL, \
             access_token_hash = ?, revision = ?, claimed_at = ?, updated_at = ? \
             WHERE id = ? AND pairing_token_hash = ? AND access_token_hash IS NULL \
               AND revision = ?",
        )
        .bind(serde_json::to_string(&session.state)?)
        .bind(access_token_digest.as_bytes().as_slice())
        .bind(i64::from(session.revision))
        .bind(session.claimed_at.map(encode_timestamp))
        .bind(encode_timestamp(session.updated_at))
        .bind(session.id.to_string())
        .bind(pairing_token_digest.as_bytes().as_slice())
        .bind(i64::from(expected_revision))
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(None);
        }
        insert_bootstrap_audit(
            &mut transaction,
            ("auth_bootstrap_session", Some(session.id.to_string())),
            "auth_bootstrap_session_claimed",
            correlation_id,
            &session,
        )
        .await?;
        transaction.commit().await?;
        Ok(Some(session))
    }

    async fn authenticate_auth_bootstrap_access(
        &self,
        session_id: AuthBootstrapSessionId,
        access_token_digest: &TokenDigest,
        authenticated_at: Timestamp,
    ) -> Result<Option<AuthBootstrapSession>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let session = authenticate_access_in_transaction(
            &mut transaction,
            session_id,
            access_token_digest,
            authenticated_at,
        )
        .await?;
        if session.is_none() {
            transaction.rollback().await?;
            return Ok(None);
        }
        transaction.commit().await?;
        Ok(session)
    }

    async fn record_auth_bootstrap_client_event(
        &self,
        event: &AuthBootstrapClientEvent,
        access_token_digest: &TokenDigest,
        correlation_id: &str,
    ) -> Result<AuthBootstrapClientEventRecord, StorageError> {
        event
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        validate_correlation_id(correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(session) = authenticate_access_in_transaction(
            &mut transaction,
            event.session_id,
            access_token_digest,
            event.received_at,
        )
        .await?
        else {
            transaction.rollback().await?;
            return Ok(AuthBootstrapClientEventRecord::AccessRejected);
        };
        if let Some(existing) =
            fetch_client_event(&mut transaction, event.session_id, event.sequence).await?
        {
            transaction.rollback().await?;
            return Ok(if existing.kind == event.kind {
                AuthBootstrapClientEventRecord::Duplicate(existing)
            } else {
                AuthBootstrapClientEventRecord::SequenceConflict
            });
        }
        let last_sequence: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM auth_bootstrap_client_events WHERE session_id = ?",
        )
        .bind(event.session_id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        let expected_sequence = last_sequence.map_or(1, |sequence| sequence.saturating_add(1));
        let event_sequence = i64::try_from(event.sequence)
            .map_err(|_| StorageError::InvalidData("invalid event sequence".to_owned()))?;
        if event_sequence != expected_sequence {
            transaction.rollback().await?;
            return Ok(AuthBootstrapClientEventRecord::SequenceConflict);
        }
        sqlx::query(
            "INSERT INTO auth_bootstrap_client_events \
             (session_id, sequence, kind_json, received_at) VALUES (?, ?, ?, ?)",
        )
        .bind(event.session_id.to_string())
        .bind(event_sequence)
        .bind(serde_json::to_string(&event.kind)?)
        .bind(encode_timestamp(event.received_at))
        .execute(&mut *transaction)
        .await?;
        insert_client_event_audit(&mut transaction, correlation_id, &session, event).await?;
        transaction.commit().await?;
        Ok(AuthBootstrapClientEventRecord::Inserted(event.clone()))
    }

    async fn update_auth_bootstrap_session_for_owner(
        &self,
        session: &AuthBootstrapSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError> {
        validate_correlation_id(correlation_id)?;
        session
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if session.revision
            != expected_revision.checked_add(1).ok_or_else(|| {
                StorageError::InvalidData(
                    "authentication bootstrap revision is exhausted".to_owned(),
                )
            })?
        {
            return Err(StorageError::InvalidData(
                "authentication bootstrap revision is invalid".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(current) = fetch_auth_bootstrap_session(&mut transaction, session.id).await?
        else {
            transaction.rollback().await?;
            return Ok(false);
        };
        if current.owner_user_id != session.owner_user_id || current.revision != expected_revision {
            transaction.rollback().await?;
            return Ok(false);
        }
        let mut expected = current;
        match session.state {
            AuthBootstrapState::Cancelled => expected.cancel(session.updated_at),
            AuthBootstrapState::Expired => expected.expire(session.updated_at),
            _ => Err(asterism_domain::AuthBootstrapSessionError::InvalidTransition),
        }
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if &expected != session {
            return Err(StorageError::InvalidData(
                "authentication bootstrap session changed immutable fields".to_owned(),
            ));
        }
        let result = sqlx::query(
            "UPDATE auth_bootstrap_sessions SET state_json = ?, pairing_token_hash = NULL, \
             access_token_hash = NULL, revision = ?, updated_at = ? \
             WHERE id = ? AND owner_user_id = ? AND revision = ?",
        )
        .bind(serde_json::to_string(&session.state)?)
        .bind(i64::from(session.revision))
        .bind(encode_timestamp(session.updated_at))
        .bind(session.id.to_string())
        .bind(session.owner_user_id.to_string())
        .bind(i64::from(expected_revision))
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(false);
        }
        insert_bootstrap_audit(
            &mut transaction,
            actor_type(actor),
            if session.state == AuthBootstrapState::Expired {
                "auth_bootstrap_session_expired"
            } else {
                "auth_bootstrap_session_cancelled"
            },
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
}

async fn authenticate_access_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthBootstrapSessionId,
    access_token_digest: &TokenDigest,
    authenticated_at: Timestamp,
) -> Result<Option<AuthBootstrapSession>, StorageError> {
    let query = format!(
        "{AUTH_BOOTSTRAP_SELECT} WHERE id = ? AND access_token_hash = ? \
         AND pairing_token_hash IS NULL AND state_json = ? AND expires_at > ?"
    );
    let Some(row) = sqlx::query(&query)
        .bind(session_id.to_string())
        .bind(access_token_digest.as_bytes().as_slice())
        .bind(serde_json::to_string(&AuthBootstrapState::Claimed)?)
        .bind(encode_timestamp(authenticated_at))
        .fetch_optional(&mut **transaction)
        .await?
    else {
        return Ok(None);
    };
    let session = decode_auth_bootstrap_session(&row)?;
    if bootstrap_binding_is_valid(transaction, &session).await? {
        Ok(Some(session))
    } else {
        Ok(None)
    }
}

async fn fetch_client_event(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthBootstrapSessionId,
    sequence: u64,
) -> Result<Option<AuthBootstrapClientEvent>, StorageError> {
    let sequence = i64::try_from(sequence)
        .map_err(|_| StorageError::InvalidData("invalid event sequence".to_owned()))?;
    sqlx::query(
        "SELECT kind_json, received_at FROM auth_bootstrap_client_events \
         WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .fetch_optional(&mut **transaction)
    .await?
    .map(|row| {
        AuthBootstrapClientEvent::new(
            session_id,
            u64::try_from(sequence)
                .map_err(|_| StorageError::InvalidData("invalid event sequence".to_owned()))?,
            serde_json::from_str(&row.get::<String, _>("kind_json"))?,
            decode_timestamp(&row.get::<String, _>("received_at"))?,
        )
        .map_err(|error| StorageError::InvalidData(error.to_string()))
    })
    .transpose()
}

async fn insert_client_event_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    correlation_id: &str,
    session: &AuthBootstrapSession,
    event: &AuthBootstrapClientEvent,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'auth_bootstrap_session', ?, 'auth_bootstrap_client_event_recorded', \
                 'auth_bootstrap_session', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(event.received_at))
    .bind(session.id.to_string())
    .bind(session.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "sequence": event.sequence,
            "kind": event.kind,
            "client_success_is_diagnostic": event.is_client_success_report(),
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn fetch_auth_bootstrap_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthBootstrapSessionId,
) -> Result<Option<AuthBootstrapSession>, StorageError> {
    let query = format!("{AUTH_BOOTSTRAP_SELECT} WHERE id = ?");
    sqlx::query(&query)
        .bind(session_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_auth_bootstrap_session)
        .transpose()
}

fn validate_initial_session(
    session: &AuthBootstrapSession,
    correlation_id: &str,
) -> Result<(), StorageError> {
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    validate_correlation_id(correlation_id)?;
    if session.state != AuthBootstrapState::AwaitingClaim
        || session.revision != 1
        || session.claimed_at.is_some()
    {
        return Err(StorageError::InvalidData(
            "new authentication bootstrap session is not awaiting claim".to_owned(),
        ));
    }
    Ok(())
}

fn validate_correlation_id(correlation_id: &str) -> Result<(), StorageError> {
    if correlation_id.is_empty()
        || correlation_id.len() > 128
        || correlation_id.chars().any(char::is_control)
    {
        Err(StorageError::InvalidData(
            "authentication bootstrap correlation ID is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}

async fn ensure_bootstrap_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &AuthBootstrapSession,
) -> Result<(), StorageError> {
    if bootstrap_binding_is_valid(transaction, session).await? {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "authentication bootstrap owner and Provider account binding is invalid".to_owned(),
        ))
    }
}

async fn bootstrap_binding_is_valid(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &AuthBootstrapSession,
) -> Result<bool, StorageError> {
    let valid: i64 = match session.provider_account_id {
        Some(account_id) => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM users JOIN provider_accounts \
                 ON provider_accounts.owner_user_id = users.id \
                 WHERE users.id = ? AND users.status = 'active' \
                   AND provider_accounts.id = ? AND provider_accounts.provider_id = ?)",
            )
            .bind(session.owner_user_id.to_string())
            .bind(account_id.to_string())
            .bind(session.provider_id.as_str())
            .fetch_one(&mut **transaction)
            .await?
        }
        None => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM users WHERE id = ? AND status = 'active')",
            )
            .bind(session.owner_user_id.to_string())
            .fetch_one(&mut **transaction)
            .await?
        }
    };
    Ok(valid == 1)
}

fn decode_auth_bootstrap_session(row: &SqliteRow) -> Result<AuthBootstrapSession, StorageError> {
    let required_recipe_version = u32::try_from(row.get::<i64, _>("required_recipe_version"))
        .map_err(|_| StorageError::InvalidData("invalid recipe version".to_owned()))?;
    let revision = u32::try_from(row.get::<i64, _>("revision"))
        .map_err(|_| StorageError::InvalidData("invalid bootstrap revision".to_owned()))?;
    let session = AuthBootstrapSession {
        id: AuthBootstrapSessionId::from_str(&row.get::<String, _>("id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        owner_user_id: UserId::from_str(&row.get::<String, _>("owner_user_id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_id: ProviderId::new(row.get::<String, _>("provider_id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: row
            .get::<Option<String>, _>("provider_account_id")
            .map(|id| {
                id.parse()
                    .map_err(|error: uuid::Error| StorageError::InvalidData(error.to_string()))
            })
            .transpose()?,
        purpose: serde_json::from_str(&row.get::<String, _>("purpose_json"))?,
        required_recipe_version,
        state: serde_json::from_str(&row.get::<String, _>("state_json"))?,
        revision,
        expires_at: decode_timestamp(&row.get::<String, _>("expires_at"))?,
        claimed_at: row
            .get::<Option<String>, _>("claimed_at")
            .as_deref()
            .map(decode_timestamp)
            .transpose()?,
        created_at: decode_timestamp(&row.get::<String, _>("created_at"))?,
        updated_at: decode_timestamp(&row.get::<String, _>("updated_at"))?,
    };
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(session)
}

fn actor_type(actor: AuditActor) -> (&'static str, Option<String>) {
    match actor {
        AuditActor::User(id) => ("user", Some(id.to_string())),
        AuditActor::ServiceToken(id) => ("service_token", Some(id.to_string())),
    }
}

async fn insert_bootstrap_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: (&str, Option<String>),
    action: &str,
    correlation_id: &str,
    session: &AuthBootstrapSession,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'auth_bootstrap_session', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(session.updated_at))
    .bind(actor.0)
    .bind(actor.1)
    .bind(action)
    .bind(session.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "state": session.state,
            "purpose": session.purpose,
            "revision": session.revision,
            "provider_id": session.provider_id,
            "has_provider_account": session.provider_account_id.is_some(),
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

#[cfg(test)]
mod tests {
    use asterism_auth::OpaqueTokenService;
    use asterism_domain::{
        AuthBootstrapPurpose, AuthBootstrapState, ProviderAccount, ProviderAccountId, Role,
    };
    use chrono::{Duration, Utc};

    use super::*;
    use crate::{ProviderAccountRepository, SqliteProviderAccountRepository};

    #[tokio::test]
    async fn pairing_digest_rotates_once_into_scoped_access_digest() {
        let fixture = fixture().await;
        let now = Utc::now();
        let session = AuthBootstrapSession::awaiting_claim(
            fixture.owner,
            fixture.provider_id.clone(),
            Some(fixture.account),
            AuthBootstrapPurpose::Reauthenticate,
            3,
            now,
            now + Duration::minutes(10),
        )
        .unwrap();
        let tokens = OpaqueTokenService::new("ast_pair").unwrap();
        let (pairing_token, pairing_digest) = tokens.generate();
        let (_, wrong_digest) = tokens.generate();
        let access_tokens = OpaqueTokenService::new("ast_boot").unwrap();
        let (_, access_digest) = access_tokens.generate();
        fixture
            .repository
            .create_auth_bootstrap_session(
                &session,
                &pairing_digest,
                AuditActor::User(fixture.owner),
                "bootstrap-create-test",
            )
            .await
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .claim_auth_bootstrap_session(
                    session.id,
                    &wrong_digest,
                    &access_digest,
                    now + Duration::seconds(1),
                    "bootstrap-wrong-token",
                )
                .await
                .unwrap(),
            None
        );
        let claimed = fixture
            .repository
            .claim_auth_bootstrap_session(
                session.id,
                &pairing_digest,
                &access_digest,
                now + Duration::seconds(1),
                "bootstrap-claim-test",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.state, AuthBootstrapState::Claimed);
        assert_eq!(claimed.revision, 2);
        assert_eq!(
            fixture
                .repository
                .claim_auth_bootstrap_session(
                    session.id,
                    &pairing_digest,
                    &wrong_digest,
                    now + Duration::seconds(2),
                    "bootstrap-replay-test",
                )
                .await
                .unwrap(),
            None
        );
        let row: (Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT pairing_token_hash, access_token_hash FROM auth_bootstrap_sessions WHERE id = ?",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert!(row.0.is_none());
        assert_eq!(row.1.as_deref(), Some(access_digest.as_bytes().as_slice()));
        let token_occurrences: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM auth_bootstrap_sessions WHERE CAST(pairing_token_hash AS TEXT) = ? \
             OR CAST(access_token_hash AS TEXT) = ?",
        )
        .bind(pairing_token.expose_secret())
        .bind(pairing_token.expose_secret())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(token_occurrences, 0);
    }

    #[tokio::test]
    async fn access_digest_is_session_scoped_and_expires_with_the_claim() {
        let fixture = fixture().await;
        let now = Utc::now();
        let (claimed, pairing_digest, access_digest) =
            claimed_session(&fixture, now, "access-primary").await;
        assert_eq!(
            fixture
                .repository
                .authenticate_auth_bootstrap_access(
                    claimed.id,
                    &access_digest,
                    now + Duration::seconds(2),
                )
                .await
                .unwrap(),
            Some(claimed.clone())
        );
        assert_eq!(
            fixture
                .repository
                .authenticate_auth_bootstrap_access(
                    claimed.id,
                    &pairing_digest,
                    now + Duration::seconds(2),
                )
                .await
                .unwrap(),
            None
        );
        let (other, _, _) = claimed_session(&fixture, now, "access-other").await;
        assert_eq!(
            fixture
                .repository
                .authenticate_auth_bootstrap_access(
                    other.id,
                    &access_digest,
                    now + Duration::seconds(2),
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            fixture
                .repository
                .authenticate_auth_bootstrap_access(claimed.id, &access_digest, claimed.expires_at,)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn client_event_sequence_is_contiguous_idempotent_and_immutable() {
        let fixture = fixture().await;
        let now = Utc::now();
        let (claimed, _, access_digest) = claimed_session(&fixture, now, "event-sequence").await;
        let first = AuthBootstrapClientEvent::new(
            claimed.id,
            1,
            asterism_domain::AuthBootstrapClientEventKind::ClientReady,
            now + Duration::seconds(2),
        )
        .unwrap();
        assert_eq!(
            fixture
                .repository
                .record_auth_bootstrap_client_event(&first, &access_digest, "event-first")
                .await
                .unwrap(),
            AuthBootstrapClientEventRecord::Inserted(first.clone())
        );
        let retry = AuthBootstrapClientEvent {
            received_at: now + Duration::seconds(3),
            ..first.clone()
        };
        assert_eq!(
            fixture
                .repository
                .record_auth_bootstrap_client_event(&retry, &access_digest, "event-retry")
                .await
                .unwrap(),
            AuthBootstrapClientEventRecord::Duplicate(first.clone())
        );
        let changed = AuthBootstrapClientEvent::new(
            claimed.id,
            1,
            asterism_domain::AuthBootstrapClientEventKind::CredentialDetected,
            now + Duration::seconds(3),
        )
        .unwrap();
        assert_eq!(
            fixture
                .repository
                .record_auth_bootstrap_client_event(&changed, &access_digest, "event-changed")
                .await
                .unwrap(),
            AuthBootstrapClientEventRecord::SequenceConflict
        );
        let gap = AuthBootstrapClientEvent::new(
            claimed.id,
            3,
            asterism_domain::AuthBootstrapClientEventKind::Validating,
            now + Duration::seconds(4),
        )
        .unwrap();
        assert_eq!(
            fixture
                .repository
                .record_auth_bootstrap_client_event(&gap, &access_digest, "event-gap")
                .await
                .unwrap(),
            AuthBootstrapClientEventRecord::SequenceConflict
        );
    }

    #[tokio::test]
    async fn client_success_event_is_diagnostic_and_requires_live_access() {
        let fixture = fixture().await;
        let now = Utc::now();
        let (claimed, pairing_digest, access_digest) =
            claimed_session(&fixture, now, "event-success").await;
        let ready = AuthBootstrapClientEvent::new(
            claimed.id,
            1,
            asterism_domain::AuthBootstrapClientEventKind::ClientReady,
            now + Duration::seconds(2),
        )
        .unwrap();
        assert_eq!(
            fixture
                .repository
                .record_auth_bootstrap_client_event(&ready, &pairing_digest, "event-wrong-token")
                .await
                .unwrap(),
            AuthBootstrapClientEventRecord::AccessRejected
        );
        fixture
            .repository
            .record_auth_bootstrap_client_event(&ready, &access_digest, "event-ready")
            .await
            .unwrap();
        let reported = AuthBootstrapClientEvent::new(
            claimed.id,
            2,
            asterism_domain::AuthBootstrapClientEventKind::ClientReportedAuthenticated,
            now + Duration::seconds(3),
        )
        .unwrap();
        fixture
            .repository
            .record_auth_bootstrap_client_event(&reported, &access_digest, "event-reported")
            .await
            .unwrap();
        let stored = fixture
            .repository
            .find_auth_bootstrap_session(fixture.owner, claimed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, AuthBootstrapState::Claimed);
        assert_eq!(stored.revision, 2);
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action = 'auth_bootstrap_client_event_recorded'",
        )
        .bind(claimed.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(audits, 2);
    }

    #[tokio::test]
    async fn expired_pairing_and_invalid_account_binding_cannot_be_claimed() {
        let fixture = fixture().await;
        let now = Utc::now();
        let tokens = OpaqueTokenService::new("ast_pair").unwrap();
        let (_, pairing_digest) = tokens.generate();
        let (_, access_digest) = OpaqueTokenService::new("ast_boot").unwrap().generate();
        let invalid_binding = AuthBootstrapSession::awaiting_claim(
            fixture.owner,
            ProviderId::new("provider-beta").unwrap(),
            Some(fixture.account),
            AuthBootstrapPurpose::Reauthenticate,
            2,
            now,
            now + Duration::minutes(10),
        )
        .unwrap();
        assert!(matches!(
            fixture
                .repository
                .create_auth_bootstrap_session(
                    &invalid_binding,
                    &pairing_digest,
                    AuditActor::User(fixture.owner),
                    "bootstrap-invalid-binding",
                )
                .await,
            Err(StorageError::InvalidData(_))
        ));
        let session = AuthBootstrapSession::awaiting_claim(
            fixture.owner,
            fixture.provider_id,
            Some(fixture.account),
            AuthBootstrapPurpose::RepairSession,
            2,
            now,
            now + Duration::seconds(1),
        )
        .unwrap();
        fixture
            .repository
            .create_auth_bootstrap_session(
                &session,
                &pairing_digest,
                AuditActor::User(fixture.owner),
                "bootstrap-expiry-create",
            )
            .await
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .claim_auth_bootstrap_session(
                    session.id,
                    &pairing_digest,
                    &access_digest,
                    session.expires_at,
                    "bootstrap-expired-claim",
                )
                .await
                .unwrap(),
            None
        );
    }

    struct Fixture {
        database: Database,
        owner: UserId,
        account: ProviderAccountId,
        provider_id: ProviderId,
        repository: SqliteAuthBootstrapSessionRepository,
    }

    async fn claimed_session(
        fixture: &Fixture,
        now: Timestamp,
        correlation_prefix: &str,
    ) -> (AuthBootstrapSession, TokenDigest, TokenDigest) {
        let session = AuthBootstrapSession::awaiting_claim(
            fixture.owner,
            fixture.provider_id.clone(),
            Some(fixture.account),
            AuthBootstrapPurpose::Reauthenticate,
            3,
            now,
            now + Duration::minutes(10),
        )
        .unwrap();
        let (_, pairing_digest) = OpaqueTokenService::new("ast_pair").unwrap().generate();
        let (_, access_digest) = OpaqueTokenService::new("ast_boot").unwrap().generate();
        fixture
            .repository
            .create_auth_bootstrap_session(
                &session,
                &pairing_digest,
                AuditActor::User(fixture.owner),
                &format!("{correlation_prefix}-create"),
            )
            .await
            .unwrap();
        let claimed = fixture
            .repository
            .claim_auth_bootstrap_session(
                session.id,
                &pairing_digest,
                &access_digest,
                now + Duration::seconds(1),
                &format!("{correlation_prefix}-claim"),
            )
            .await
            .unwrap()
            .unwrap();
        (claimed, pairing_digest, access_digest)
    }

    async fn fixture() -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'bootstrap-owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account = ProviderAccountId::new();
        SqliteProviderAccountRepository::new(database.clone())
            .create_provider_account(
                &ProviderAccount {
                    id: account,
                    owner_id: owner,
                    provider_id: provider_id.clone(),
                    display_name: "Primary".to_owned(),
                    tenant: None,
                    auth_state: asterism_domain::AuthState::Idle,
                    network_profile_id: None,
                    credential_refs: Vec::new(),
                    created_at: now,
                    updated_at: now,
                },
                AuditActor::User(owner),
            )
            .await
            .unwrap();
        Fixture {
            repository: SqliteAuthBootstrapSessionRepository::new(database.clone()),
            database,
            owner,
            account,
            provider_id,
        }
    }
}
