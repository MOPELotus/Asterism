use asterism_domain::{
    CreditAccount, CreditAmount, CreditReservation, CreditTransaction, PriceQuote,
};
use asterism_storage::{CreditQueryRepository, CreditRepository, SqliteCreditRepository};
use axum::{
    Extension, Json,
    extract::{Query, State, rejection::QueryRejection},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;

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
