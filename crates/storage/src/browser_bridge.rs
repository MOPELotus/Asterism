use std::str::FromStr;

use asterism_auth::TokenDigest;
use asterism_domain::{
    AuditActor, AuditRecordId, BrowserBridgeExchange, BrowserBridgeExchangeState,
    BrowserBridgeSession, BrowserBridgeSessionId, BrowserBridgeSessionState, ProviderAccountId,
    ProviderId, TaskId, Timestamp, UserId,
};
use asterism_provider_api::BrowserSessionSpec;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{BrowserBridgeExchangeRecord, BrowserBridgeSessionRepository, Database, StorageError};

const SESSION_SELECT: &str = "SELECT id, owner_user_id, provider_account_id, task_id, provider_id, \
    provider_version, spec_version, spec_digest, spec_json, state_json, revision, expires_at, \
    claimed_at, created_at, updated_at FROM browser_bridge_sessions";

#[derive(Clone, Debug)]
pub struct SqliteBrowserBridgeSessionRepository {
    database: Database,
}

impl SqliteBrowserBridgeSessionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl BrowserBridgeSessionRepository for SqliteBrowserBridgeSessionRepository {
    async fn create_browser_bridge_session(
        &self,
        session: &BrowserBridgeSession,
        spec: &BrowserSessionSpec,
        pairing_token_digest: &TokenDigest,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError> {
        validate_initial_session(session, spec, correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        ensure_binding(&mut transaction, session).await?;
        sqlx::query(
            "INSERT INTO browser_bridge_sessions \
             (id, owner_user_id, provider_account_id, task_id, provider_id, provider_version, \
              spec_version, spec_digest, spec_json, state_json, pairing_token_hash, \
              access_token_hash, revision, expires_at, claimed_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(session.owner_user_id.to_string())
        .bind(session.provider_account_id.to_string())
        .bind(session.task_id.to_string())
        .bind(session.provider_id.as_str())
        .bind(&session.provider_version)
        .bind(i64::from(session.spec_version))
        .bind(session.spec_digest.as_slice())
        .bind(serde_json::to_string(spec)?)
        .bind(serde_json::to_string(&session.state)?)
        .bind(pairing_token_digest.as_bytes().as_slice())
        .bind(i64::from(session.revision))
        .bind(encode_timestamp(session.expires_at))
        .bind(encode_timestamp(session.created_at))
        .bind(encode_timestamp(session.updated_at))
        .execute(&mut *transaction)
        .await?;
        insert_audit(
            &mut transaction,
            actor_type(actor),
            "browser_bridge_session_created",
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_browser_bridge_session(
        &self,
        owner_user_id: UserId,
        session_id: BrowserBridgeSessionId,
    ) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError> {
        let query = format!("{SESSION_SELECT} WHERE id = ? AND owner_user_id = ?");
        sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_session)
            .transpose()
    }

    async fn claim_browser_bridge_session(
        &self,
        session_id: BrowserBridgeSessionId,
        pairing_token_digest: &TokenDigest,
        access_token_digest: &TokenDigest,
        claimed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError> {
        validate_correlation_id(correlation_id)?;
        if pairing_token_digest == access_token_digest {
            return Err(StorageError::InvalidData(
                "BrowserBridge pairing and access tokens must be independent".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let query = format!(
            "{SESSION_SELECT} WHERE id = ? AND pairing_token_hash = ? \
             AND access_token_hash IS NULL AND state_json = ? AND expires_at > ?"
        );
        let Some(row) = sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(pairing_token_digest.as_bytes().as_slice())
            .bind(serde_json::to_string(
                &BrowserBridgeSessionState::AwaitingClaim,
            )?)
            .bind(encode_timestamp(claimed_at))
            .fetch_optional(&mut *transaction)
            .await?
        else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let (mut session, spec) = decode_session(&row)?;
        ensure_binding(&mut transaction, &session).await?;
        let expected_revision = session.revision;
        session
            .claim(claimed_at)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let updated = sqlx::query(
            "UPDATE browser_bridge_sessions SET state_json = ?, pairing_token_hash = NULL, \
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
        if updated.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(None);
        }
        insert_audit(
            &mut transaction,
            ("browser_bridge_session", Some(session.id.to_string())),
            "browser_bridge_session_claimed",
            correlation_id,
            &session,
        )
        .await?;
        transaction.commit().await?;
        Ok(Some((session, spec)))
    }

    async fn authenticate_browser_bridge_access(
        &self,
        session_id: BrowserBridgeSessionId,
        access_token_digest: &TokenDigest,
        authenticated_at: Timestamp,
    ) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let query = format!(
            "{SESSION_SELECT} WHERE id = ? AND access_token_hash = ? \
             AND pairing_token_hash IS NULL AND state_json = ? AND expires_at > ?"
        );
        let record = sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(access_token_digest.as_bytes().as_slice())
            .bind(serde_json::to_string(&BrowserBridgeSessionState::Claimed)?)
            .bind(encode_timestamp(authenticated_at))
            .fetch_optional(&mut *transaction)
            .await?
            .as_ref()
            .map(decode_session)
            .transpose()?;
        let Some((session, spec)) = record else {
            transaction.rollback().await?;
            return Ok(None);
        };
        if !binding_is_valid(&mut transaction, &session).await? {
            transaction.rollback().await?;
            return Ok(None);
        }
        transaction.commit().await?;
        Ok(Some((session, spec)))
    }

    async fn update_browser_bridge_session_for_owner(
        &self,
        session: &BrowserBridgeSession,
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
                StorageError::InvalidData("BrowserBridge session revision is exhausted".to_owned())
            })?
        {
            return Err(StorageError::InvalidData(
                "BrowserBridge session revision is invalid".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some((current, _)) = fetch_session(&mut transaction, session.id).await? else {
            transaction.rollback().await?;
            return Ok(false);
        };
        if current.owner_user_id != session.owner_user_id || current.revision != expected_revision {
            transaction.rollback().await?;
            return Ok(false);
        }
        let mut expected = current;
        match session.state {
            BrowserBridgeSessionState::Cancelled => expected.cancel(session.updated_at),
            BrowserBridgeSessionState::Expired => expected.expire(session.updated_at),
            _ => Err(asterism_domain::BrowserBridgeSessionError::InvalidTransition),
        }
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if &expected != session {
            return Err(StorageError::InvalidData(
                "BrowserBridge session changed immutable fields".to_owned(),
            ));
        }
        let updated = sqlx::query(
            "UPDATE browser_bridge_sessions SET state_json = ?, pairing_token_hash = NULL, \
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
        if updated.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(false);
        }
        insert_audit(
            &mut transaction,
            actor_type(actor),
            if session.state == BrowserBridgeSessionState::Expired {
                "browser_bridge_session_expired"
            } else {
                "browser_bridge_session_cancelled"
            },
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn complete_browser_bridge_exchange(
        &self,
        exchange: &BrowserBridgeExchange,
        access_token_digest: &TokenDigest,
        correlation_id: &str,
    ) -> Result<BrowserBridgeExchangeRecord, StorageError> {
        exchange
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if !matches!(
            exchange.state,
            BrowserBridgeExchangeState::Completed | BrowserBridgeExchangeState::Rejected
        ) {
            return Err(StorageError::InvalidData(
                "completed BrowserBridge exchange must have a terminal result".to_owned(),
            ));
        }
        validate_correlation_id(correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(session) = authenticate_session_for_exchange(
            &mut transaction,
            exchange.session_id,
            access_token_digest,
            exchange.completed_at.unwrap_or(exchange.issued_at),
        )
        .await?
        else {
            transaction.rollback().await?;
            return Ok(BrowserBridgeExchangeRecord::AccessRejected);
        };
        let sequence = i64::try_from(exchange.sequence)
            .map_err(|_| StorageError::InvalidData("invalid exchange sequence".to_owned()))?;
        let Some(existing) =
            fetch_exchange(&mut transaction, exchange.session_id, sequence).await?
        else {
            transaction.rollback().await?;
            return Ok(BrowserBridgeExchangeRecord::SequenceConflict);
        };
        if existing.command_type != exchange.command_type
            || existing.command_digest != exchange.command_digest
            || existing.issued_at != exchange.issued_at
        {
            transaction.rollback().await?;
            return Ok(BrowserBridgeExchangeRecord::SequenceConflict);
        }
        if existing.state != BrowserBridgeExchangeState::Issued {
            let same_result = existing.state == exchange.state
                && existing.result_type == exchange.result_type
                && existing.result_digest == exchange.result_digest;
            transaction.rollback().await?;
            return Ok(if same_result {
                BrowserBridgeExchangeRecord::Duplicate(existing)
            } else {
                BrowserBridgeExchangeRecord::SequenceConflict
            });
        }
        sqlx::query(
            "UPDATE browser_bridge_exchanges SET result_type = ?, result_digest = ?, \
             state = ?, completed_at = ? WHERE session_id = ? AND sequence = ? AND state = 'issued'",
        )
        .bind(exchange.result_type.as_deref())
        .bind(exchange.result_digest.map(|digest| digest.to_vec()))
        .bind(match exchange.state {
            BrowserBridgeExchangeState::Completed => "completed",
            BrowserBridgeExchangeState::Rejected => "rejected",
            BrowserBridgeExchangeState::Issued => unreachable!(),
        })
        .bind(exchange.completed_at.map(encode_timestamp))
        .bind(exchange.session_id.to_string())
        .bind(sequence)
        .execute(&mut *transaction)
        .await?;
        insert_exchange_audit(
            &mut transaction,
            correlation_id,
            &session,
            exchange,
            "completed",
        )
        .await?;
        transaction.commit().await?;
        Ok(BrowserBridgeExchangeRecord::Inserted(exchange.clone()))
    }
}

pub(crate) async fn authenticate_session_for_exchange(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: BrowserBridgeSessionId,
    access_token_digest: &TokenDigest,
    at: Timestamp,
) -> Result<Option<BrowserBridgeSession>, StorageError> {
    let query = format!(
        "{SESSION_SELECT} WHERE id = ? AND access_token_hash = ? \
         AND pairing_token_hash IS NULL AND state_json = ? AND expires_at > ?"
    );
    let Some(row) = sqlx::query(&query)
        .bind(session_id.to_string())
        .bind(access_token_digest.as_bytes().as_slice())
        .bind(serde_json::to_string(&BrowserBridgeSessionState::Claimed)?)
        .bind(encode_timestamp(at))
        .fetch_optional(&mut **transaction)
        .await?
    else {
        return Ok(None);
    };
    let (session, _) = decode_session(&row)?;
    if binding_is_valid(transaction, &session).await? {
        Ok(Some(session))
    } else {
        Ok(None)
    }
}

pub(crate) async fn find_claimed_session_for_exchange(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: BrowserBridgeSessionId,
    at: Timestamp,
) -> Result<Option<BrowserBridgeSession>, StorageError> {
    let query = format!(
        "{SESSION_SELECT} WHERE id = ? AND pairing_token_hash IS NULL \
         AND access_token_hash IS NOT NULL AND state_json = ? AND expires_at > ?"
    );
    let Some(row) = sqlx::query(&query)
        .bind(session_id.to_string())
        .bind(serde_json::to_string(&BrowserBridgeSessionState::Claimed)?)
        .bind(encode_timestamp(at))
        .fetch_optional(&mut **transaction)
        .await?
    else {
        return Ok(None);
    };
    let (session, _) = decode_session(&row)?;
    if binding_is_valid(transaction, &session).await? {
        Ok(Some(session))
    } else {
        Ok(None)
    }
}

pub(crate) async fn fetch_exchange(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: BrowserBridgeSessionId,
    sequence: i64,
) -> Result<Option<BrowserBridgeExchange>, StorageError> {
    sqlx::query(
        "SELECT command_type, command_digest, result_type, result_digest, state, issued_at, completed_at \
         FROM browser_bridge_exchanges WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .fetch_optional(&mut **transaction)
    .await?
    .map(|row| {
        let command_digest: [u8; 32] = row
            .get::<Vec<u8>, _>("command_digest")
            .try_into()
            .map_err(|_| StorageError::InvalidData("invalid exchange command digest".to_owned()))?;
        let result_digest = row
            .get::<Option<Vec<u8>>, _>("result_digest")
            .map(|value| {
                value.try_into().map_err(|_| {
                    StorageError::InvalidData("invalid exchange result digest".to_owned())
                })
            })
            .transpose()?;
        let state = match row.get::<String, _>("state").as_str() {
            "issued" => BrowserBridgeExchangeState::Issued,
            "completed" => BrowserBridgeExchangeState::Completed,
            "rejected" => BrowserBridgeExchangeState::Rejected,
            _ => return Err(StorageError::InvalidData("invalid exchange state".to_owned())),
        };
        let exchange = BrowserBridgeExchange {
            session_id,
            sequence: u64::try_from(sequence)
                .map_err(|_| StorageError::InvalidData("invalid exchange sequence".to_owned()))?,
            command_type: row.get("command_type"),
            command_digest,
            result_type: row.get("result_type"),
            result_digest,
            state,
            issued_at: decode_timestamp(&row.get::<String, _>("issued_at"))?,
            completed_at: row
                .get::<Option<String>, _>("completed_at")
                .as_deref()
                .map(decode_timestamp)
                .transpose()?,
        };
        exchange
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        Ok(exchange)
    })
    .transpose()
}

pub(crate) async fn insert_exchange_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    correlation_id: &str,
    session: &BrowserBridgeSession,
    exchange: &BrowserBridgeExchange,
    action: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) VALUES (?, ?, 'browser_bridge', ?, ?, \
          'browser_bridge_exchange', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(
        exchange.completed_at.unwrap_or(exchange.issued_at),
    ))
    .bind(session.id.to_string())
    .bind(format!("browser_bridge_exchange_{action}"))
    .bind(format!("{}:{}", session.id, exchange.sequence))
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "sequence": exchange.sequence,
            "command_type": exchange.command_type,
            "command_digest": "[HASHED]",
            "result_type": exchange.result_type,
            "result_digest": exchange.result_digest.map(|_| "[HASHED]"),
            "state": exchange.state,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn fetch_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: BrowserBridgeSessionId,
) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError> {
    let query = format!("{SESSION_SELECT} WHERE id = ?");
    sqlx::query(&query)
        .bind(session_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_session)
        .transpose()
}

fn validate_initial_session(
    session: &BrowserBridgeSession,
    spec: &BrowserSessionSpec,
    correlation_id: &str,
) -> Result<(), StorageError> {
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    validate_spec_binding(session, spec)?;
    validate_correlation_id(correlation_id)?;
    if session.state != BrowserBridgeSessionState::AwaitingClaim
        || session.revision != 1
        || session.claimed_at.is_some()
    {
        return Err(StorageError::InvalidData(
            "new BrowserBridge session is not awaiting claim".to_owned(),
        ));
    }
    Ok(())
}

fn validate_spec_binding(
    session: &BrowserBridgeSession,
    spec: &BrowserSessionSpec,
) -> Result<(), StorageError> {
    let digest = spec
        .digest()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if session.spec_version != spec.version || session.spec_digest != digest {
        return Err(StorageError::InvalidData(
            "BrowserBridge session specification binding is inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn decode_session(
    row: &SqliteRow,
) -> Result<(BrowserBridgeSession, BrowserSessionSpec), StorageError> {
    let spec_digest = row.get::<Vec<u8>, _>("spec_digest");
    let spec_digest: [u8; 32] = spec_digest
        .try_into()
        .map_err(|_| StorageError::InvalidData("invalid BrowserBridge spec digest".to_owned()))?;
    let session = BrowserBridgeSession {
        id: BrowserBridgeSessionId::from_str(&row.get::<String, _>("id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        owner_user_id: UserId::from_str(&row.get::<String, _>("owner_user_id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: ProviderAccountId::from_str(
            &row.get::<String, _>("provider_account_id"),
        )
        .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        task_id: TaskId::from_str(&row.get::<String, _>("task_id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_id: ProviderId::new(row.get::<String, _>("provider_id"))
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_version: row.get("provider_version"),
        spec_version: u32::try_from(row.get::<i64, _>("spec_version")).map_err(|_| {
            StorageError::InvalidData("invalid BrowserBridge spec version".to_owned())
        })?,
        spec_digest,
        state: serde_json::from_str(&row.get::<String, _>("state_json"))?,
        revision: u32::try_from(row.get::<i64, _>("revision"))
            .map_err(|_| StorageError::InvalidData("invalid BrowserBridge revision".to_owned()))?,
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
    let spec: BrowserSessionSpec = serde_json::from_str(&row.get::<String, _>("spec_json"))?;
    validate_spec_binding(&session, &spec)?;
    Ok((session, spec))
}

async fn ensure_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &BrowserBridgeSession,
) -> Result<(), StorageError> {
    if binding_is_valid(transaction, session).await? {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "BrowserBridge owner, account, Task, and Provider binding is invalid".to_owned(),
        ))
    }
}

pub(crate) async fn binding_is_valid(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &BrowserBridgeSession,
) -> Result<bool, StorageError> {
    let valid: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users \
         JOIN provider_accounts ON provider_accounts.owner_user_id = users.id \
         JOIN tasks ON tasks.provider_account_id = provider_accounts.id \
         WHERE users.id = ? AND users.status = 'active' \
           AND provider_accounts.id = ? AND provider_accounts.provider_id = ? \
           AND tasks.id = ?)",
    )
    .bind(session.owner_user_id.to_string())
    .bind(session.provider_account_id.to_string())
    .bind(session.provider_id.as_str())
    .bind(session.task_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    Ok(valid == 1)
}

fn validate_correlation_id(value: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(StorageError::InvalidData(
            "BrowserBridge correlation ID is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn actor_type(actor: AuditActor) -> (&'static str, Option<String>) {
    match actor {
        AuditActor::User(id) => ("user", Some(id.to_string())),
        AuditActor::ServiceToken(id) => ("service_token", Some(id.to_string())),
    }
}

async fn insert_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: (&str, Option<String>),
    action: &str,
    correlation_id: &str,
    session: &BrowserBridgeSession,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'browser_bridge_session', ?, ?, 'succeeded', ?)",
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
            "revision": session.revision,
            "provider_id": session.provider_id,
            "provider_version": session.provider_version,
            "spec_version": session.spec_version,
            "task_id": session.task_id,
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
    use asterism_domain::{BrowserBridgeSessionCreate, Role};
    use chrono::{Duration, Utc};

    use super::*;

    #[tokio::test]
    async fn pairing_rotates_once_and_access_is_session_bound() {
        let fixture = fixture().await;
        let now = Utc::now();
        let spec = spec();
        let session = session(&fixture, &spec, now);
        let pairing_tokens = OpaqueTokenService::new("ast_bridge_pair").unwrap();
        let (pairing, pairing_digest) = pairing_tokens.generate();
        let access_tokens = OpaqueTokenService::new("ast_bridge").unwrap();
        let (access, access_digest) = access_tokens.generate();
        fixture
            .repository
            .create_browser_bridge_session(
                &session,
                &spec,
                &pairing_digest,
                AuditActor::User(fixture.owner),
                "bridge-create",
            )
            .await
            .unwrap();

        let (claimed, frozen) = fixture
            .repository
            .claim_browser_bridge_session(
                session.id,
                &pairing_tokens.digest(&pairing),
                &access_digest,
                now + Duration::seconds(1),
                "bridge-claim",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.state, BrowserBridgeSessionState::Claimed);
        assert_eq!(frozen, spec);
        assert!(
            fixture
                .repository
                .claim_browser_bridge_session(
                    session.id,
                    &pairing_digest,
                    &access_digest,
                    now + Duration::seconds(2),
                    "bridge-replay",
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .repository
                .authenticate_browser_bridge_access(
                    session.id,
                    &access_tokens.digest(&access),
                    now + Duration::seconds(2),
                )
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn wrong_spec_binding_and_foreign_owner_fail_closed() {
        let fixture = fixture().await;
        let now = Utc::now();
        let spec = spec();
        let mut invalid_session = session(&fixture, &spec, now);
        invalid_session.spec_digest = [9; 32];
        let (_, pairing_digest) = OpaqueTokenService::new("ast_bridge_pair")
            .unwrap()
            .generate();
        assert!(
            fixture
                .repository
                .create_browser_bridge_session(
                    &invalid_session,
                    &spec,
                    &pairing_digest,
                    AuditActor::User(fixture.owner),
                    "bridge-invalid-spec",
                )
                .await
                .is_err()
        );
        let valid = session(&fixture, &spec, now);
        fixture
            .repository
            .create_browser_bridge_session(
                &valid,
                &spec,
                &pairing_digest,
                AuditActor::User(fixture.owner),
                "bridge-valid",
            )
            .await
            .unwrap();
        assert!(
            fixture
                .repository
                .find_browser_bridge_session(UserId::new(), valid.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn exchange_completion_is_idempotent_and_result_bound() {
        let fixture = fixture().await;
        let now = Utc::now();
        let spec = spec();
        let session = session(&fixture, &spec, now);
        let pairing_tokens = OpaqueTokenService::new("ast_bridge_pair").unwrap();
        let (pairing, pairing_digest) = pairing_tokens.generate();
        let access_tokens = OpaqueTokenService::new("ast_bridge").unwrap();
        let (access, access_digest) = access_tokens.generate();
        fixture
            .repository
            .create_browser_bridge_session(
                &session,
                &spec,
                &pairing_digest,
                AuditActor::User(fixture.owner),
                "exchange-create",
            )
            .await
            .unwrap();
        fixture
            .repository
            .claim_browser_bridge_session(
                session.id,
                &pairing_tokens.digest(&pairing),
                &access_digest,
                now + Duration::seconds(1),
                "exchange-claim",
            )
            .await
            .unwrap()
            .unwrap();

        let issued = BrowserBridgeExchange::issue(
            session.id,
            1,
            "cidaren.capture.snapshot".to_owned(),
            [1; 32],
            now + Duration::seconds(2),
        )
        .unwrap();
        sqlx::query(
            "INSERT INTO browser_bridge_exchanges \
             (session_id, sequence, command_type, command_digest, state, issued_at) \
             VALUES (?, 1, ?, ?, 'issued', ?)",
        )
        .bind(session.id.to_string())
        .bind(&issued.command_type)
        .bind(issued.command_digest.as_slice())
        .bind(encode_timestamp(issued.issued_at))
        .execute(fixture.repository.database.pool())
        .await
        .unwrap();
        let mut completed = issued.clone();
        completed
            .complete(
                "cidaren.capture.snapshot.result".to_owned(),
                [2; 32],
                now + Duration::seconds(3),
            )
            .unwrap();
        assert!(matches!(
            fixture
                .repository
                .complete_browser_bridge_exchange(
                    &completed,
                    &access_tokens.digest(&access),
                    "exchange-complete",
                )
                .await
                .unwrap(),
            BrowserBridgeExchangeRecord::Inserted(_)
        ));
        assert!(matches!(
            fixture
                .repository
                .complete_browser_bridge_exchange(
                    &completed,
                    &access_tokens.digest(&access),
                    "exchange-complete-duplicate",
                )
                .await
                .unwrap(),
            BrowserBridgeExchangeRecord::Duplicate(_)
        ));
        let mut conflicting = completed.clone();
        conflicting.result_digest = Some([3; 32]);
        assert!(matches!(
            fixture
                .repository
                .complete_browser_bridge_exchange(
                    &conflicting,
                    &access_tokens.digest(&access),
                    "exchange-conflict",
                )
                .await
                .unwrap(),
            BrowserBridgeExchangeRecord::SequenceConflict
        ));
    }

    struct Fixture {
        repository: SqliteBrowserBridgeSessionRepository,
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        provider: ProviderId,
    }

    async fn fixture() -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let account = ProviderAccountId::new();
        let task = TaskId::new();
        let provider = ProviderId::new("provider-alpha").unwrap();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'bridge-owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'primary', '\"authenticated\"', ?, ?)",
        )
        .bind(account.to_string())
        .bind(owner.to_string())
        .bind(provider.as_str())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'remote-task', 'fingerprint', 'work', 'unknown', 'Task', \
                     'pending', 'ready', ?, ?, '[\"browser_bridge\"]')",
        )
        .bind(task.to_string())
        .bind(account.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        Fixture {
            repository: SqliteBrowserBridgeSessionRepository::new(database),
            owner,
            account,
            task,
            provider,
        }
    }

    fn spec() -> BrowserSessionSpec {
        BrowserSessionSpec {
            version: 1,
            isolation_key: "provider-task-a1".to_owned(),
            allowed_origins: vec!["https://provider.example".to_owned()],
            headless: false,
        }
    }

    fn session(
        fixture: &Fixture,
        spec: &BrowserSessionSpec,
        now: Timestamp,
    ) -> BrowserBridgeSession {
        BrowserBridgeSession::awaiting_claim(BrowserBridgeSessionCreate {
            owner_user_id: fixture.owner,
            provider_account_id: fixture.account,
            task_id: fixture.task,
            provider_id: fixture.provider.clone(),
            provider_version: "0.1.0".to_owned(),
            spec_version: spec.version,
            spec_digest: spec.digest().unwrap(),
            created_at: now,
            expires_at: now + Duration::hours(1),
        })
        .unwrap()
    }
}
