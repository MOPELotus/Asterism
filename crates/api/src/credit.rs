use std::str::FromStr;

use asterism_domain::{
    CreditAccount, CreditAmount, CreditGrantReceiptId, CreditReservation, CreditTransaction,
    CreditTransactionId, PriceQuote, UserId,
};
use asterism_storage::{
    CreditGrant, CreditGrantOutcome, CreditQueryRepository, CreditRepository,
    SqliteCreditRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;
const IDEMPOTENCY_KEY: &str = "idempotency-key";

pub(super) async fn grant_user_credits(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<CreditGrantRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let operator_id = auth.require_credit_grant()?;
    let user_id = UserId::from_str(&user_id)
        .map_err(|_| ApiError::bad_request("invalid_user_id", "user ID is invalid"))?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request("invalid_credit_grant", "credit grant body is invalid")
    })?;
    if request.amount == CreditAmount::ZERO
        || i64::try_from(request.amount.value()).is_err()
        || invalid_reason(&request.reason)
    {
        return Err(ApiError::bad_request(
            "invalid_credit_grant",
            "amount must be a positive SQLite-safe integer and reason must be 1-256 safe bytes",
        ));
    }
    let idempotency_key = required_header(&headers, IDEMPOTENCY_KEY, 256)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let outcome = SqliteCreditRepository::new(state.database)
        .grant(&CreditGrant {
            receipt_id: CreditGrantReceiptId::new(),
            transaction_id: CreditTransactionId::new(),
            user_id,
            operator_id,
            amount: request.amount,
            reason: request.reason,
            idempotency_key: idempotency_key.to_owned(),
            correlation_id: correlation_id.to_owned(),
            created_at: Utc::now(),
        })
        .await
        .map_err(ApiError::internal)?;
    match outcome {
        CreditGrantOutcome::Applied(result) => {
            let status = if result.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            Ok(crate::auth::no_store(
                (
                    status,
                    Json(CreditGrantResponse {
                        account: result.account,
                        transaction: result.transaction,
                        created: result.created,
                    }),
                )
                    .into_response(),
            ))
        }
        CreditGrantOutcome::IdempotencyConflict => Err(ApiError::conflict(
            "idempotency_conflict",
            "the idempotency key is already bound to another credit grant",
        )),
        CreditGrantOutcome::UserNotFound => Err(ApiError::not_found("user_not_found")),
    }
}

pub(super) async fn get_credit_account(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_credit_read()?;
    let account = SqliteCreditRepository::new(state.database)
        .account(owner_id)
        .await
        .map_err(ApiError::internal)?
        .unwrap_or(CreditAccount {
            user_id: owner_id,
            available: CreditAmount::ZERO,
            reserved: CreditAmount::ZERO,
        });
    Ok(crate::auth::no_store(Json(account).into_response()))
}

pub(super) async fn list_credit_transactions(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<CreditPageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_credit_read()?;
    let (limit, offset) = credit_pagination(query)?;
    let page = SqliteCreditRepository::new(state.database)
        .list_owned_credit_transactions(owner_id, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(CreditTransactionPageResponse {
            total: page.total,
            limit,
            offset,
            items: page.items,
        })
        .into_response(),
    ))
}

pub(super) async fn list_credit_reservations(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<CreditPageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_credit_read()?;
    let (limit, offset) = credit_pagination(query)?;
    let page = SqliteCreditRepository::new(state.database)
        .list_owned_credit_reservations(owner_id, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(CreditReservationPageResponse {
            total: page.total,
            limit,
            offset,
            items: page
                .items
                .into_iter()
                .map(|detail| CreditReservationDetailResponse {
                    reservation: detail.reservation,
                    quote: detail.quote,
                })
                .collect(),
        })
        .into_response(),
    ))
}

fn credit_pagination(
    query: Result<Query<CreditPageQuery>, QueryRejection>,
) -> Result<(u32, u64), ApiError> {
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_credit_query",
            "credit query parameters have an invalid format",
        )
    })?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > MAX_OFFSET {
        return Err(ApiError::bad_request(
            "invalid_credit_pagination",
            "credit limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    Ok((limit, offset))
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    max_bytes: usize,
) -> Result<&'a str, ApiError> {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "missing_required_header",
                format!("the {name} header is required"),
            )
        })?;
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "invalid_required_header",
            format!("the {name} header is invalid"),
        ));
    }
    Ok(value)
}

fn invalid_reason(reason: &str) -> bool {
    reason.is_empty()
        || reason.len() > 256
        || reason.trim() != reason
        || reason.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct CreditGrantRequest {
    amount: CreditAmount,
    reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CreditGrantResponse {
    account: CreditAccount,
    transaction: CreditTransaction,
    created: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct CreditPageQuery {
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CreditTransactionPageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<CreditTransaction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CreditReservationPageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<CreditReservationDetailResponse>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CreditReservationDetailResponse {
    reservation: CreditReservation,
    quote: PriceQuote,
}
