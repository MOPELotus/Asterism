use std::str::FromStr;

use asterism_domain::{
    AuditRecordId, CreditAccount, CreditAmount, CreditGrantReceiptId, CreditReservation,
    CreditReservationId, CreditReservationState, CreditTransaction, CreditTransactionId,
    CreditTransactionType, ExecutionId, ExecutionState, PriceQuote, PriceQuoteId, TaskId,
    Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction};

use crate::{
    CreditQueryRepository, CreditRepository, CreditReservationDetail, CreditReservationPage,
    CreditTransactionPage, Database, StorageError, outbox::enqueue_in_transaction,
};

const MAX_CREDIT_PAGE_SIZE: u32 = 200;
const MAX_CREDIT_OFFSET: u64 = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditGrant {
    pub receipt_id: CreditGrantReceiptId,
    pub transaction_id: CreditTransactionId,
    pub user_id: UserId,
    pub operator_id: UserId,
    pub amount: CreditAmount,
    pub reason: String,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditGrantResult {
    pub account: CreditAccount,
    pub transaction: CreditTransaction,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CreditGrantOutcome {
    Applied(CreditGrantResult),
    IdempotencyConflict,
    UserNotFound,
}

#[derive(Clone, Debug)]
pub struct SqliteCreditRepository {
    database: Database,
}

impl SqliteCreditRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl CreditRepository for SqliteCreditRepository {
    async fn account(&self, user_id: UserId) -> Result<Option<CreditAccount>, StorageError> {
        let row = sqlx::query(
            "SELECT user_id, available, reserved FROM credit_accounts WHERE user_id = ?",
        )
        .bind(user_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.map(|row| decode_account(&row)).transpose()
    }

    // The immediate transaction intentionally keeps idempotency replay, balance
    // mutation, receipt, Audit and Outbox writes in one visible atomic flow.
    #[allow(clippy::too_many_lines)]
    async fn grant(&self, grant: &CreditGrant) -> Result<CreditGrantOutcome, StorageError> {
        validate_credit_grant(grant)?;
        let amount = encode_amount(grant.amount)?;
        let timestamp = encode_timestamp(grant.created_at);
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(existing) =
            find_credit_grant_receipt(&mut transaction, grant.operator_id, &grant.idempotency_key)
                .await?
        {
            if existing.user_id != grant.user_id
                || existing.amount != grant.amount
                || existing.reason != grant.reason
            {
                transaction.rollback().await?;
                return Ok(CreditGrantOutcome::IdempotencyConflict);
            }
            let credit_transaction =
                fetch_credit_transaction(&mut transaction, existing.transaction_id).await?;
            transaction.rollback().await?;
            return Ok(CreditGrantOutcome::Applied(CreditGrantResult {
                account: CreditAccount {
                    user_id: existing.user_id,
                    available: existing.result_available,
                    reserved: existing.result_reserved,
                },
                transaction: credit_transaction,
                created: false,
            }));
        }
        let user_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = ?)")
                .bind(grant.user_id.to_string())
                .fetch_one(&mut *transaction)
                .await?;
        if !user_exists {
            transaction.rollback().await?;
            return Ok(CreditGrantOutcome::UserNotFound);
        }
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, 0, 0, ?) ON CONFLICT(user_id) DO NOTHING",
        )
        .bind(grant.user_id.to_string())
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await?;
        let changed = sqlx::query(
            "UPDATE credit_accounts SET available = available + ?, updated_at = ? \
             WHERE user_id = ? AND available <= ?",
        )
        .bind(amount)
        .bind(&timestamp)
        .bind(grant.user_id.to_string())
        .bind(i64::MAX - amount)
        .execute(&mut *transaction)
        .await?;
        if changed.rows_affected() != 1 {
            transaction.rollback().await?;
            return Err(StorageError::CreditAmountOutOfRange);
        }
        let credit_transaction = CreditTransaction {
            id: grant.transaction_id,
            user_id: grant.user_id,
            amount,
            transaction_type: CreditTransactionType::MasterGrant,
            task_id: None,
            execution_id: None,
            operator_id: Some(grant.operator_id),
            reason: grant.reason.clone(),
            created_at: grant.created_at,
        };
        sqlx::query(
            "INSERT INTO credit_transactions \
             (id, user_id, amount, transaction_type, operator_id, reason, created_at) \
             VALUES (?, ?, ?, 'master_grant', ?, ?, ?)",
        )
        .bind(grant.transaction_id.to_string())
        .bind(grant.user_id.to_string())
        .bind(amount)
        .bind(grant.operator_id.to_string())
        .bind(&grant.reason)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await?;
        let account = fetch_account(&mut transaction, grant.user_id).await?;
        sqlx::query(
            "INSERT INTO credit_grant_receipts \
             (id, operator_id, idempotency_key, user_id, amount, reason, transaction_id, \
              result_available, result_reserved, correlation_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(grant.receipt_id.to_string())
        .bind(grant.operator_id.to_string())
        .bind(&grant.idempotency_key)
        .bind(grant.user_id.to_string())
        .bind(amount)
        .bind(&grant.reason)
        .bind(grant.transaction_id.to_string())
        .bind(encode_amount(account.available)?)
        .bind(encode_amount(account.reserved)?)
        .bind(&grant.correlation_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await?;
        insert_credit_grant_audit(&mut transaction, grant).await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                &grant.correlation_id,
                DomainEvent::CreditGranted {
                    user_id: grant.user_id,
                    operator_id: grant.operator_id,
                    amount: grant.amount,
                },
                grant.created_at,
            ),
        )
        .await?;
        transaction.commit().await?;
        Ok(CreditGrantOutcome::Applied(CreditGrantResult {
            account,
            transaction: credit_transaction,
            created: true,
        }))
    }

    async fn reserve(
        &self,
        reservation: &CreditReservation,
    ) -> Result<CreditAccount, StorageError> {
        if reservation.state != CreditReservationState::Reserved {
            return Err(StorageError::ReservationNotActive);
        }
        let amount = encode_amount(reservation.amount)?;
        let timestamp = encode_timestamp(reservation.created_at);
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        validate_reservation_binding(&mut transaction, reservation).await?;
        let balance_update = sqlx::query(
            "UPDATE credit_accounts SET \
                 available = available - ?, reserved = reserved + ?, updated_at = ? \
             WHERE user_id = ? AND available >= ?",
        )
        .bind(amount)
        .bind(amount)
        .bind(&timestamp)
        .bind(reservation.user_id.to_string())
        .bind(amount)
        .execute(&mut *transaction)
        .await?;
        if balance_update.rows_affected() != 1 {
            return Err(StorageError::InsufficientCredits);
        }
        sqlx::query(
            "INSERT INTO credit_reservations \
             (id, user_id, quote_id, execution_id, amount, state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'reserved', ?, ?)",
        )
        .bind(reservation.id.to_string())
        .bind(reservation.user_id.to_string())
        .bind(reservation.quote_id.to_string())
        .bind(reservation.execution_id.to_string())
        .bind(amount)
        .bind(&timestamp)
        .bind(encode_timestamp(reservation.updated_at))
        .execute(&mut *transaction)
        .await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                format!("credit-reserve:{}", reservation.id),
                DomainEvent::CreditReserved {
                    user_id: reservation.user_id,
                    execution_id: reservation.execution_id,
                    amount: reservation.amount,
                },
                reservation.created_at,
            ),
        )
        .await?;
        let account = fetch_account(&mut transaction, reservation.user_id).await?;
        transaction.commit().await?;
        Ok(account)
    }

    async fn commit(
        &self,
        reservation_id: CreditReservationId,
        transaction_id: CreditTransactionId,
        at: Timestamp,
    ) -> Result<CreditAccount, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let reservation = active_reservation(&mut transaction, reservation_id).await?;
        if reservation.execution_state != "succeeded" {
            return Err(StorageError::CreditInvariant);
        }
        let account = commit_active_reservation(
            &mut transaction,
            reservation_id,
            &reservation,
            transaction_id,
            at,
            &format!("credit-commit:{reservation_id}"),
        )
        .await?;
        transaction.commit().await?;
        Ok(account)
    }

    async fn release(
        &self,
        reservation_id: CreditReservationId,
        at: Timestamp,
    ) -> Result<CreditAccount, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let reservation = active_reservation(&mut transaction, reservation_id).await?;
        if reservation.execution_state == "succeeded" {
            return Err(StorageError::CreditInvariant);
        }
        let account = release_active_reservation(
            &mut transaction,
            reservation_id,
            &reservation,
            at,
            &format!("credit-release:{reservation_id}"),
        )
        .await?;
        transaction.commit().await?;
        Ok(account)
    }
}

#[async_trait]
impl CreditQueryRepository for SqliteCreditRepository {
    async fn list_owned_credit_transactions(
        &self,
        owner_id: UserId,
        limit: u32,
        offset: u64,
    ) -> Result<CreditTransactionPage, StorageError> {
        validate_credit_pagination(limit, offset)?;
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM credit_transactions WHERE user_id = ?")
                .bind(owner_id.to_string())
                .fetch_one(self.database.pool())
                .await?;
        let rows = sqlx::query(
            "SELECT id, user_id, amount, transaction_type, task_id, execution_id, operator_id, \
                    reason, created_at FROM credit_transactions WHERE user_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner_id.to_string())
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated credit offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        Ok(CreditTransactionPage {
            items: rows
                .iter()
                .map(decode_transaction)
                .collect::<Result<_, _>>()?,
            total: decode_count(total, "credit transaction")?,
        })
    }

    async fn list_owned_credit_reservations(
        &self,
        owner_id: UserId,
        limit: u32,
        offset: u64,
    ) -> Result<CreditReservationPage, StorageError> {
        validate_credit_pagination(limit, offset)?;
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM credit_reservations WHERE user_id = ?")
                .bind(owner_id.to_string())
                .fetch_one(self.database.pool())
                .await?;
        let rows = sqlx::query(
            "SELECT reservation.id AS reservation_id, reservation.user_id, \
                    reservation.quote_id, reservation.execution_id, \
                    reservation.amount AS reservation_amount, reservation.state, \
                    reservation.created_at AS reservation_created_at, reservation.updated_at, \
                    quote.task_id AS quote_task_id, quote.amount AS quote_amount, \
                    quote.pricing_revision, quote.reason, quote.created_at AS quote_created_at, \
                    execution.task_id AS execution_task_id, execution.requested_by, \
                    execution.quote_id AS execution_quote_id \
             FROM credit_reservations AS reservation \
             JOIN price_quotes AS quote ON quote.id = reservation.quote_id \
             JOIN executions AS execution ON execution.id = reservation.execution_id \
             WHERE reservation.user_id = ? \
             ORDER BY reservation.created_at DESC, reservation.id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner_id.to_string())
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated credit offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        Ok(CreditReservationPage {
            items: rows
                .iter()
                .map(decode_reservation_detail)
                .collect::<Result<_, _>>()?,
            total: decode_count(total, "credit reservation")?,
        })
    }
}

pub(crate) async fn settle_execution_reservation(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    final_state: ExecutionState,
    at: Timestamp,
    correlation_id: &str,
) -> Result<(), StorageError> {
    if !final_state.is_terminal() {
        return Ok(());
    }
    let execution_quote_id = sqlx::query_scalar::<_, Option<String>>(
        "SELECT quote_id FROM executions WHERE id = ? AND state = ?",
    )
    .bind(execution_id.to_string())
    .bind(match final_state {
        ExecutionState::Succeeded => "succeeded",
        ExecutionState::Failed => "failed",
        ExecutionState::Cancelled => "cancelled",
        _ => unreachable!("non-terminal states returned above"),
    })
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::ExecutionStateConflict)?;
    let reservation_id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM credit_reservations WHERE execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let reservation_id = match (execution_quote_id, reservation_id) {
        (None, None) => return Ok(()),
        (Some(_), Some(id)) => CreditReservationId::from_str(&id)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        _ => return Err(StorageError::CreditInvariant),
    };
    let reservation = active_reservation(transaction, reservation_id)
        .await
        .map_err(|error| match error {
            StorageError::ReservationNotActive => StorageError::CreditInvariant,
            other => other,
        })?;
    if reservation.execution_id != execution_id {
        return Err(StorageError::CreditInvariant);
    }
    match final_state {
        ExecutionState::Succeeded => {
            commit_active_reservation(
                transaction,
                reservation_id,
                &reservation,
                CreditTransactionId::new(),
                at,
                correlation_id,
            )
            .await?;
        }
        ExecutionState::Failed | ExecutionState::Cancelled => {
            release_active_reservation(
                transaction,
                reservation_id,
                &reservation,
                at,
                correlation_id,
            )
            .await?;
        }
        _ => unreachable!("non-terminal states returned above"),
    }
    Ok(())
}

async fn commit_active_reservation(
    transaction: &mut Transaction<'_, Sqlite>,
    reservation_id: CreditReservationId,
    reservation: &ActiveReservation,
    transaction_id: CreditTransactionId,
    at: Timestamp,
    correlation_id: &str,
) -> Result<CreditAccount, StorageError> {
    if reservation.execution_state != "succeeded" {
        return Err(StorageError::CreditInvariant);
    }
    let timestamp = encode_timestamp(at);
    let changed = sqlx::query(
        "UPDATE credit_reservations SET state = 'committed', updated_at = ? \
         WHERE id = ? AND state = 'reserved'",
    )
    .bind(&timestamp)
    .bind(reservation_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(StorageError::ReservationNotActive);
    }
    let balance_update = sqlx::query(
        "UPDATE credit_accounts SET reserved = reserved - ?, updated_at = ? \
         WHERE user_id = ? AND reserved >= ?",
    )
    .bind(reservation.amount)
    .bind(&timestamp)
    .bind(reservation.user_id.to_string())
    .bind(reservation.amount)
    .execute(&mut **transaction)
    .await?;
    if balance_update.rows_affected() != 1 {
        return Err(StorageError::CreditInvariant);
    }
    sqlx::query(
        "INSERT INTO credit_transactions \
         (id, user_id, amount, transaction_type, task_id, execution_id, reason, created_at) \
         VALUES (?, ?, ?, 'task_execution', ?, ?, 'execution succeeded', ?)",
    )
    .bind(transaction_id.to_string())
    .bind(reservation.user_id.to_string())
    .bind(-reservation.amount)
    .bind(reservation.task_id.to_string())
    .bind(reservation.execution_id.to_string())
    .bind(&timestamp)
    .execute(&mut **transaction)
    .await?;
    let amount = CreditAmount::new(decode_amount(reservation.amount)?);
    enqueue_in_transaction(
        transaction,
        &EventEnvelope::at(
            correlation_id,
            DomainEvent::CreditCommitted {
                user_id: reservation.user_id,
                execution_id: reservation.execution_id,
                amount,
            },
            at,
        ),
    )
    .await?;
    fetch_account(transaction, reservation.user_id).await
}

async fn release_active_reservation(
    transaction: &mut Transaction<'_, Sqlite>,
    reservation_id: CreditReservationId,
    reservation: &ActiveReservation,
    at: Timestamp,
    correlation_id: &str,
) -> Result<CreditAccount, StorageError> {
    if reservation.execution_state == "succeeded" {
        return Err(StorageError::CreditInvariant);
    }
    let timestamp = encode_timestamp(at);
    let changed = sqlx::query(
        "UPDATE credit_reservations SET state = 'released', updated_at = ? \
         WHERE id = ? AND state = 'reserved'",
    )
    .bind(&timestamp)
    .bind(reservation_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(StorageError::ReservationNotActive);
    }
    let balance_update = sqlx::query(
        "UPDATE credit_accounts SET reserved = reserved - ?, available = available + ?, \
             updated_at = ? WHERE user_id = ? AND reserved >= ?",
    )
    .bind(reservation.amount)
    .bind(reservation.amount)
    .bind(&timestamp)
    .bind(reservation.user_id.to_string())
    .bind(reservation.amount)
    .execute(&mut **transaction)
    .await?;
    if balance_update.rows_affected() != 1 {
        return Err(StorageError::CreditInvariant);
    }
    let amount = CreditAmount::new(decode_amount(reservation.amount)?);
    enqueue_in_transaction(
        transaction,
        &EventEnvelope::at(
            correlation_id,
            DomainEvent::CreditReleased {
                user_id: reservation.user_id,
                execution_id: reservation.execution_id,
                amount,
            },
            at,
        ),
    )
    .await?;
    fetch_account(transaction, reservation.user_id).await
}

struct StoredCreditGrantReceipt {
    user_id: UserId,
    amount: CreditAmount,
    reason: String,
    transaction_id: CreditTransactionId,
    result_available: CreditAmount,
    result_reserved: CreditAmount,
}

async fn find_credit_grant_receipt(
    transaction: &mut Transaction<'_, Sqlite>,
    operator_id: UserId,
    idempotency_key: &str,
) -> Result<Option<StoredCreditGrantReceipt>, StorageError> {
    let row = sqlx::query(
        "SELECT user_id, amount, reason, transaction_id, result_available, result_reserved \
         FROM credit_grant_receipts WHERE operator_id = ? AND idempotency_key = ?",
    )
    .bind(operator_id.to_string())
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref()
        .map(|row| {
            Ok(StoredCreditGrantReceipt {
                user_id: parse_id(row.try_get("user_id")?)?,
                amount: CreditAmount::new(decode_amount(row.try_get("amount")?)?),
                reason: row.try_get("reason")?,
                transaction_id: parse_id(row.try_get("transaction_id")?)?,
                result_available: CreditAmount::new(decode_amount(
                    row.try_get("result_available")?,
                )?),
                result_reserved: CreditAmount::new(decode_amount(row.try_get("result_reserved")?)?),
            })
        })
        .transpose()
}

async fn fetch_credit_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    transaction_id: CreditTransactionId,
) -> Result<CreditTransaction, StorageError> {
    let row = sqlx::query(
        "SELECT id, user_id, amount, transaction_type, task_id, execution_id, operator_id, \
                reason, created_at FROM credit_transactions WHERE id = ?",
    )
    .bind(transaction_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    decode_transaction(&row)
}

async fn insert_credit_grant_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    grant: &CreditGrant,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'user', ?, 'credit_granted', 'user', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(grant.created_at))
    .bind(grant.operator_id.to_string())
    .bind(grant.user_id.to_string())
    .bind(&grant.correlation_id)
    .bind(serde_json::to_string(&serde_json::json!({
        "amount": grant.amount,
        "reason": grant.reason,
        "transaction_id": grant.transaction_id,
    }))?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_credit_grant(grant: &CreditGrant) -> Result<(), StorageError> {
    if grant.amount == CreditAmount::ZERO
        || invalid_bounded_text(&grant.reason, 256)
        || invalid_bounded_text(&grant.idempotency_key, 256)
        || invalid_bounded_text(&grant.correlation_id, 128)
    {
        return Err(StorageError::InvalidData(
            "credit grant violates persistence invariants".to_owned(),
        ));
    }
    Ok(())
}

fn invalid_bounded_text(value: &str, max_bytes: usize) -> bool {
    value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
}

struct ActiveReservation {
    user_id: UserId,
    amount: i64,
    execution_id: ExecutionId,
    task_id: TaskId,
    execution_state: String,
}

fn validate_credit_pagination(limit: u32, offset: u64) -> Result<(), StorageError> {
    if limit == 0 || limit > MAX_CREDIT_PAGE_SIZE || offset > MAX_CREDIT_OFFSET {
        Err(StorageError::InvalidData(
            "credit pagination is outside the supported range".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn decode_count(value: i64, resource: &str) -> Result<u64, StorageError> {
    u64::try_from(value)
        .map_err(|_| StorageError::InvalidData(format!("{resource} count is invalid")))
}

fn decode_transaction(row: &sqlx::sqlite::SqliteRow) -> Result<CreditTransaction, StorageError> {
    Ok(CreditTransaction {
        id: parse_id(row.try_get("id")?)?,
        user_id: parse_id(row.try_get("user_id")?)?,
        amount: row.try_get("amount")?,
        transaction_type: decode_enum::<CreditTransactionType>(row.try_get("transaction_type")?)?,
        task_id: row
            .try_get::<Option<&str>, _>("task_id")?
            .map(parse_id)
            .transpose()?,
        execution_id: row
            .try_get::<Option<&str>, _>("execution_id")?
            .map(parse_id)
            .transpose()?,
        operator_id: row
            .try_get::<Option<&str>, _>("operator_id")?
            .map(parse_id)
            .transpose()?,
        reason: row.try_get("reason")?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    })
}

fn decode_reservation_detail(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<CreditReservationDetail, StorageError> {
    let quote_id: PriceQuoteId = parse_id(row.try_get("quote_id")?)?;
    let quote_task_id: TaskId = parse_id(row.try_get("quote_task_id")?)?;
    let quote_amount = CreditAmount::new(decode_amount(row.try_get("quote_amount")?)?);
    let reservation = CreditReservation {
        id: parse_id(row.try_get("reservation_id")?)?,
        user_id: parse_id(row.try_get("user_id")?)?,
        quote_id,
        execution_id: parse_id(row.try_get("execution_id")?)?,
        amount: CreditAmount::new(decode_amount(row.try_get("reservation_amount")?)?),
        state: decode_enum(row.try_get("state")?)?,
        created_at: decode_timestamp(row.try_get("reservation_created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    let quote = PriceQuote {
        id: quote_id,
        task_id: quote_task_id,
        amount: quote_amount,
        pricing_revision: row.try_get("pricing_revision")?,
        reason: row.try_get("reason")?,
        created_at: decode_timestamp(row.try_get("quote_created_at")?)?,
    };
    let execution_task_id: TaskId = parse_id(row.try_get("execution_task_id")?)?;
    let requested_by = row
        .try_get::<Option<&str>, _>("requested_by")?
        .map(parse_id)
        .transpose()?;
    let execution_quote_id = row
        .try_get::<Option<&str>, _>("execution_quote_id")?
        .map(parse_id)
        .transpose()?;
    if reservation.amount != quote.amount
        || requested_by != Some(reservation.user_id)
        || execution_quote_id != Some(quote.id)
        || execution_task_id != quote.task_id
    {
        return Err(StorageError::CreditInvariant);
    }
    Ok(CreditReservationDetail { reservation, quote })
}

async fn validate_reservation_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    reservation: &CreditReservation,
) -> Result<(), StorageError> {
    let row = sqlx::query(
        "SELECT quote.task_id AS quote_task_id, quote.amount AS quote_amount, \
                execution.task_id AS execution_task_id, execution.requested_by, \
                execution.quote_id AS execution_quote_id \
         FROM price_quotes AS quote \
         INNER JOIN executions AS execution ON execution.id = ? \
         WHERE quote.id = ?",
    )
    .bind(reservation.execution_id.to_string())
    .bind(reservation.quote_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::CreditInvariant)?;
    let requested_by = row
        .try_get::<Option<&str>, _>("requested_by")?
        .map(parse_id)
        .transpose()?;
    let execution_quote_id = row
        .try_get::<Option<&str>, _>("execution_quote_id")?
        .map(parse_id)
        .transpose()?;
    if parse_id::<TaskId>(row.try_get("quote_task_id")?)?
        != parse_id::<TaskId>(row.try_get("execution_task_id")?)?
        || decode_amount(row.try_get("quote_amount")?)? != reservation.amount.value()
        || requested_by != Some(reservation.user_id)
        || execution_quote_id != Some(reservation.quote_id)
    {
        return Err(StorageError::CreditInvariant);
    }
    Ok(())
}

async fn active_reservation(
    transaction: &mut Transaction<'_, Sqlite>,
    reservation_id: CreditReservationId,
) -> Result<ActiveReservation, StorageError> {
    let row = sqlx::query(
        "SELECT r.user_id, r.amount, r.execution_id, r.quote_id, e.task_id, \
                e.requested_by, e.quote_id AS execution_quote_id, e.state AS execution_state, \
                q.task_id AS quote_task_id, q.amount AS quote_amount \
         FROM credit_reservations r \
         JOIN executions e ON e.id = r.execution_id \
         JOIN price_quotes q ON q.id = r.quote_id \
         WHERE r.id = ? AND r.state = 'reserved'",
    )
    .bind(reservation_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::ReservationNotActive)?;
    let user_id = parse_id(row.try_get("user_id")?)?;
    let amount: i64 = row.try_get("amount")?;
    let execution_id = parse_id(row.try_get("execution_id")?)?;
    let quote_id: asterism_domain::PriceQuoteId = parse_id(row.try_get("quote_id")?)?;
    let task_id = parse_id(row.try_get("task_id")?)?;
    let requested_by = row
        .try_get::<Option<&str>, _>("requested_by")?
        .map(parse_id)
        .transpose()?;
    let execution_quote_id = row
        .try_get::<Option<&str>, _>("execution_quote_id")?
        .map(parse_id)
        .transpose()?;
    let quote_task_id: TaskId = parse_id(row.try_get("quote_task_id")?)?;
    let quote_amount: i64 = row.try_get("quote_amount")?;
    if requested_by != Some(user_id)
        || execution_quote_id != Some(quote_id)
        || quote_task_id != task_id
        || quote_amount != amount
    {
        return Err(StorageError::CreditInvariant);
    }
    Ok(ActiveReservation {
        user_id,
        amount,
        execution_id,
        task_id,
        execution_state: row.try_get("execution_state")?,
    })
}

async fn fetch_account(
    transaction: &mut Transaction<'_, Sqlite>,
    user_id: UserId,
) -> Result<CreditAccount, StorageError> {
    let row =
        sqlx::query("SELECT user_id, available, reserved FROM credit_accounts WHERE user_id = ?")
            .bind(user_id.to_string())
            .fetch_one(&mut **transaction)
            .await?;
    decode_account(&row)
}

fn decode_account(row: &sqlx::sqlite::SqliteRow) -> Result<CreditAccount, StorageError> {
    Ok(CreditAccount {
        user_id: parse_id(row.try_get("user_id")?)?,
        available: CreditAmount::new(decode_amount(row.try_get("available")?)?),
        reserved: CreditAmount::new(decode_amount(row.try_get("reserved")?)?),
    })
}

fn parse_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    T::from_str(value).map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn encode_amount(value: CreditAmount) -> Result<i64, StorageError> {
    i64::try_from(value.value()).map_err(|_| StorageError::CreditAmountOutOfRange)
}

fn decode_amount(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::CreditInvariant)
}

fn decode_enum<T>(value: &str) -> Result<T, StorageError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(Into::into)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{CreditReservationState, PriceQuoteId, ProviderAccountId, TaskId};
    use chrono::Utc;

    use super::*;

    struct Scenario {
        repository: SqliteCreditRepository,
        user_id: UserId,
        operator_id: UserId,
        quote_ids: [PriceQuoteId; 2],
        execution_ids: [ExecutionId; 2],
        now: Timestamp,
    }

    async fn scenario() -> Scenario {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let user_id = UserId::new();
        let operator_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let quote_ids = [PriceQuoteId::new(), PriceQuoteId::new()];
        let execution_ids = [ExecutionId::new(), ExecutionId::new()];
        let now = Utc::now();
        let timestamp = encode_timestamp(now);
        for (id, username) in [(user_id, "user"), (operator_id, "operator")] {
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, ?, 'hash', 'active', '[]', '[]', ?, ?)",
            )
            .bind(id.to_string())
            .bind(username)
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'test', 'Test', '{}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(user_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'remote', 'fingerprint', 'work', 'routine', 'Task', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        for (quote_id, execution_id) in quote_ids.into_iter().zip(execution_ids) {
            sqlx::query(
                "INSERT INTO price_quotes (id, task_id, amount, pricing_revision, reason, created_at) \
                 VALUES (?, ?, 80, 'test', 'test', ?)",
            )
            .bind(quote_id.to_string())
            .bind(task_id.to_string())
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO executions (id, task_id, requested_by, request_source, quote_id, state, created_at) \
                 VALUES (?, ?, ?, 'system', ?, 'requested', ?)",
            )
            .bind(execution_id.to_string())
            .bind(task_id.to_string())
            .bind(user_id.to_string())
            .bind(quote_id.to_string())
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
        }
        Scenario {
            repository: SqliteCreditRepository::new(database),
            user_id,
            operator_id,
            quote_ids,
            execution_ids,
            now,
        }
    }

    impl Scenario {
        fn reservation(&self, index: usize) -> CreditReservation {
            CreditReservation {
                id: CreditReservationId::new(),
                user_id: self.user_id,
                quote_id: self.quote_ids[index],
                execution_id: self.execution_ids[index],
                amount: CreditAmount::new(80),
                state: CreditReservationState::Reserved,
                created_at: self.now,
                updated_at: self.now,
            }
        }
    }

    #[tokio::test]
    async fn credit_grant_is_persistently_idempotent_and_audited() {
        let scenario = scenario().await;
        let original = CreditGrant {
            receipt_id: CreditGrantReceiptId::new(),
            transaction_id: CreditTransactionId::new(),
            user_id: scenario.user_id,
            operator_id: scenario.operator_id,
            amount: CreditAmount::new(100),
            reason: "manual grant".to_owned(),
            idempotency_key: "grant-user-100".to_owned(),
            correlation_id: "grant-request-1".to_owned(),
            created_at: scenario.now,
        };
        let CreditGrantOutcome::Applied(first) =
            scenario.repository.grant(&original).await.unwrap()
        else {
            panic!("first grant must be applied");
        };
        assert!(first.created);
        assert_eq!(first.account.available, CreditAmount::new(100));

        let replay = CreditGrant {
            receipt_id: CreditGrantReceiptId::new(),
            transaction_id: CreditTransactionId::new(),
            correlation_id: "grant-request-2".to_owned(),
            ..original.clone()
        };
        let CreditGrantOutcome::Applied(replayed) =
            scenario.repository.grant(&replay).await.unwrap()
        else {
            panic!("exact grant retry must replay");
        };
        assert!(!replayed.created);
        assert_eq!(replayed.account, first.account);
        assert_eq!(replayed.transaction, first.transaction);

        let conflict = CreditGrant {
            amount: CreditAmount::new(101),
            ..replay
        };
        assert_eq!(
            scenario.repository.grant(&conflict).await.unwrap(),
            CreditGrantOutcome::IdempotencyConflict
        );
        for table in [
            "credit_grant_receipts",
            "credit_transactions",
            "audit_records",
            "event_outbox",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(scenario.repository.database.pool())
                .await
                .unwrap();
            assert_eq!(count, 1, "unexpected {table} mutation count");
        }
    }

    #[tokio::test]
    async fn concurrent_reservations_cannot_overspend() {
        let scenario = scenario().await;
        scenario
            .repository
            .grant(&CreditGrant {
                receipt_id: CreditGrantReceiptId::new(),
                transaction_id: CreditTransactionId::new(),
                user_id: scenario.user_id,
                operator_id: scenario.operator_id,
                amount: CreditAmount::new(100),
                reason: "initial".to_owned(),
                idempotency_key: "initial-grant".to_owned(),
                correlation_id: "initial-grant".to_owned(),
                created_at: scenario.now,
            })
            .await
            .unwrap();
        let first = scenario.reservation(0);
        let second = scenario.reservation(1);
        let (first_result, second_result) = tokio::join!(
            scenario.repository.reserve(&first),
            scenario.repository.reserve(&second)
        );
        assert_eq!(
            [first_result, second_result]
                .iter()
                .filter(|result| result.is_ok())
                .count(),
            1
        );
        let account = scenario
            .repository
            .account(scenario.user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.available.value(), 20);
        assert_eq!(account.reserved.value(), 80);
    }

    #[tokio::test]
    async fn commit_debits_once_and_release_restores_available_credit() {
        let scenario = scenario().await;
        scenario
            .repository
            .grant(&CreditGrant {
                receipt_id: CreditGrantReceiptId::new(),
                transaction_id: CreditTransactionId::new(),
                user_id: scenario.user_id,
                operator_id: scenario.operator_id,
                amount: CreditAmount::new(200),
                reason: "initial".to_owned(),
                idempotency_key: "initial-grant".to_owned(),
                correlation_id: "initial-grant".to_owned(),
                created_at: scenario.now,
            })
            .await
            .unwrap();
        let committed = scenario.reservation(0);
        scenario.repository.reserve(&committed).await.unwrap();
        scenario.set_execution_state(0, "succeeded").await;
        let account = scenario
            .repository
            .commit(committed.id, CreditTransactionId::new(), scenario.now)
            .await
            .unwrap();
        assert_eq!(
            (account.available.value(), account.reserved.value()),
            (120, 0)
        );
        assert!(matches!(
            scenario
                .repository
                .commit(committed.id, CreditTransactionId::new(), scenario.now)
                .await,
            Err(StorageError::ReservationNotActive)
        ));

        let released = scenario.reservation(1);
        scenario.repository.reserve(&released).await.unwrap();
        scenario.set_execution_state(1, "failed").await;
        let account = scenario
            .repository
            .release(released.id, scenario.now)
            .await
            .unwrap();
        assert_eq!(
            (account.available.value(), account.reserved.value()),
            (120, 0)
        );
        let ledger_sum: i64 =
            sqlx::query_scalar("SELECT SUM(amount) FROM credit_transactions WHERE user_id = ?")
                .bind(scenario.user_id.to_string())
                .fetch_one(scenario.repository.database.pool())
                .await
                .unwrap();
        assert_eq!(ledger_sum, 120);
    }

    #[tokio::test]
    async fn reservation_rejects_cross_execution_quote_and_amount_mismatch() {
        let scenario = scenario().await;
        scenario
            .repository
            .grant(&CreditGrant {
                receipt_id: CreditGrantReceiptId::new(),
                transaction_id: CreditTransactionId::new(),
                user_id: scenario.user_id,
                operator_id: scenario.operator_id,
                amount: CreditAmount::new(200),
                reason: "initial".to_owned(),
                idempotency_key: "initial-grant".to_owned(),
                correlation_id: "initial-grant".to_owned(),
                created_at: scenario.now,
            })
            .await
            .unwrap();

        let mut cross_bound = scenario.reservation(0);
        cross_bound.execution_id = scenario.execution_ids[1];
        assert!(matches!(
            scenario.repository.reserve(&cross_bound).await,
            Err(StorageError::CreditInvariant)
        ));

        let mut wrong_amount = scenario.reservation(0);
        wrong_amount.amount = CreditAmount::new(79);
        assert!(matches!(
            scenario.repository.reserve(&wrong_amount).await,
            Err(StorageError::CreditInvariant)
        ));

        let account = scenario
            .repository
            .account(scenario.user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (account.available.value(), account.reserved.value()),
            (200, 0)
        );
    }

    #[tokio::test]
    async fn credit_history_is_owner_scoped_paginated_and_keeps_quote_attribution() {
        let scenario = scenario().await;
        scenario
            .repository
            .grant(&CreditGrant {
                receipt_id: CreditGrantReceiptId::new(),
                transaction_id: CreditTransactionId::new(),
                user_id: scenario.user_id,
                operator_id: scenario.operator_id,
                amount: CreditAmount::new(200),
                reason: "initial".to_owned(),
                idempotency_key: "initial-grant".to_owned(),
                correlation_id: "initial-grant".to_owned(),
                created_at: scenario.now,
            })
            .await
            .unwrap();
        let committed = scenario.reservation(0);
        let active = scenario.reservation(1);
        scenario.repository.reserve(&committed).await.unwrap();
        scenario.repository.reserve(&active).await.unwrap();
        scenario.set_execution_state(0, "succeeded").await;
        scenario
            .repository
            .commit(committed.id, CreditTransactionId::new(), scenario.now)
            .await
            .unwrap();

        let first_transaction = scenario
            .repository
            .list_owned_credit_transactions(scenario.user_id, 1, 0)
            .await
            .unwrap();
        assert_eq!(first_transaction.total, 2);
        assert_eq!(first_transaction.items.len(), 1);
        let all_transactions = scenario
            .repository
            .list_owned_credit_transactions(scenario.user_id, 2, 0)
            .await
            .unwrap();
        assert!(all_transactions.items.iter().any(|transaction| {
            transaction.transaction_type == CreditTransactionType::MasterGrant
                && transaction.amount == 200
        }));
        assert!(all_transactions.items.iter().any(|transaction| {
            transaction.transaction_type == CreditTransactionType::TaskExecution
                && transaction.amount == -80
                && transaction.execution_id == Some(committed.execution_id)
        }));

        let reservations = scenario
            .repository
            .list_owned_credit_reservations(scenario.user_id, 2, 0)
            .await
            .unwrap();
        assert_eq!(reservations.total, 2);
        assert_eq!(reservations.items.len(), 2);
        for detail in &reservations.items {
            assert_eq!(detail.reservation.quote_id, detail.quote.id);
            assert_eq!(detail.reservation.amount, detail.quote.amount);
            assert_eq!(detail.quote.pricing_revision, "test");
        }
        assert!(reservations.items.iter().any(|detail| {
            detail.reservation.id == committed.id
                && detail.reservation.state == CreditReservationState::Committed
        }));
        assert!(reservations.items.iter().any(|detail| {
            detail.reservation.id == active.id
                && detail.reservation.state == CreditReservationState::Reserved
        }));

        let foreign_transactions = scenario
            .repository
            .list_owned_credit_transactions(scenario.operator_id, 50, 0)
            .await
            .unwrap();
        let foreign_reservations = scenario
            .repository
            .list_owned_credit_reservations(scenario.operator_id, 50, 0)
            .await
            .unwrap();
        assert_eq!(foreign_transactions.total, 0);
        assert!(foreign_transactions.items.is_empty());
        assert_eq!(foreign_reservations.total, 0);
        assert!(foreign_reservations.items.is_empty());
    }

    #[tokio::test]
    async fn credit_history_rejects_unbounded_pagination() {
        let scenario = scenario().await;
        for result in [
            scenario
                .repository
                .list_owned_credit_transactions(scenario.user_id, 0, 0)
                .await
                .map(|_| ()),
            scenario
                .repository
                .list_owned_credit_reservations(scenario.user_id, MAX_CREDIT_PAGE_SIZE + 1, 0)
                .await
                .map(|_| ()),
            scenario
                .repository
                .list_owned_credit_transactions(scenario.user_id, 1, MAX_CREDIT_OFFSET + 1)
                .await
                .map(|_| ()),
        ] {
            assert!(matches!(result, Err(StorageError::InvalidData(_))));
        }
    }

    #[tokio::test]
    async fn settlement_requires_the_matching_execution_outcome() {
        let scenario = scenario().await;
        scenario
            .repository
            .grant(&CreditGrant {
                receipt_id: CreditGrantReceiptId::new(),
                transaction_id: CreditTransactionId::new(),
                user_id: scenario.user_id,
                operator_id: scenario.operator_id,
                amount: CreditAmount::new(100),
                reason: "initial".to_owned(),
                idempotency_key: "initial-grant".to_owned(),
                correlation_id: "initial-grant".to_owned(),
                created_at: scenario.now,
            })
            .await
            .unwrap();
        let reservation = scenario.reservation(0);
        scenario.repository.reserve(&reservation).await.unwrap();

        assert!(matches!(
            scenario
                .repository
                .commit(reservation.id, CreditTransactionId::new(), scenario.now)
                .await,
            Err(StorageError::CreditInvariant)
        ));
        scenario.set_execution_state(0, "succeeded").await;
        assert!(matches!(
            scenario
                .repository
                .release(reservation.id, scenario.now)
                .await,
            Err(StorageError::CreditInvariant)
        ));
    }

    impl Scenario {
        async fn set_execution_state(&self, index: usize, state: &str) {
            sqlx::query("UPDATE executions SET state = ? WHERE id = ?")
                .bind(state)
                .bind(self.execution_ids[index].to_string())
                .execute(self.repository.database.pool())
                .await
                .unwrap();
        }
    }
}
