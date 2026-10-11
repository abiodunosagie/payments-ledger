//! HTTP handlers. Request bodies are read as raw bytes and parsed here, so malformed input gets
//! the same JSON error shape as every other failure, and the exact bytes feed the idempotency hash.

use crate::AppState;
use crate::error::ApiError;
use crate::idempotency::{self, Outcome};
use crate::ledger::{self, Leg};
use crate::paystack;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Acquire, FromRow, PgConnection};
use uuid::Uuid;

/// Header set on a response that was replayed from an earlier request with the same key.
pub const REPLAYED_HEADER: &str = "idempotent-replayed";

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))
}

fn id_from(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, ApiError> {
    path.map(|Path(id)| id)
        .map_err(|_| ApiError::bad_request("the id in the path must be a UUID"))
}

/// Runs a write once per idempotency key.
///
/// The work runs in a savepoint inside the request's transaction. A client error (4xx) rolls the
/// savepoint back, so nothing it wrote survives, and the error is stored as the key's answer. A
/// server error rolls everything back and stores nothing, so the client can retry.
async fn idempotent<F>(
    state: &AppState,
    headers: &HeaderMap,
    route: &str,
    body: &Bytes,
    work: F,
) -> Result<Response, ApiError>
where
    F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<Outcome, ApiError>>,
{
    let key = idempotency::key_from(headers)?;
    let hash = idempotency::request_hash(route, body);
    let mut tx = state.pool.begin().await?;
    if let Some((status, saved)) = idempotency::begin(&mut tx, &key, &hash).await? {
        let mut response = (status, Json(saved)).into_response();
        response
            .headers_mut()
            .insert(REPLAYED_HEADER, HeaderValue::from_static("true"));
        return Ok(response);
    }
    let mut savepoint = (&mut *tx).begin().await?;
    let outcome = match work(&mut savepoint).await {
        Ok(outcome) => {
            savepoint.commit().await?;
            outcome
        }
        Err(e) if e.is_client_error() => {
            savepoint.rollback().await?;
            (e.status, e.body())
        }
        Err(e) => return Err(e),
    };
    idempotency::save(&mut tx, &key, &hash, &outcome).await?;
    tx.commit().await?;
    Ok((outcome.0, Json(outcome.1)).into_response())
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

// ---- accounts

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAccount {
    name: String,
    currency: String,
}

pub async fn create_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let req: NewAccount = parse(&body)?;
    idempotent(&state, &headers, "POST /accounts", &body, move |conn| {
        Box::pin(async move {
            let account = ledger::create_account(conn, &req.name, &req.currency).await?;
            Ok((StatusCode::CREATED, json!(account)))
        })
    })
    .await
}

pub async fn get_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ledger::Account>, ApiError> {
    let id = id_from(path)?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(ledger::find_account(&mut conn, id).await?))
}

#[derive(Deserialize)]
pub struct StatementQuery {
    limit: Option<i64>,
    before: Option<i64>,
}

pub async fn list_postings(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<StatementQuery>, QueryRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = id_from(path)?;
    let Query(q) =
        query.map_err(|_| ApiError::bad_request("limit and before must be whole numbers"))?;
    let limit = q.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 100"));
    }
    let mut conn = state.pool.acquire().await?;
    ledger::find_account(&mut conn, id).await?;
    let postings = ledger::statement(&mut conn, id, q.before, limit).await?;
    let next_before = if postings.len() as i64 == limit {
        postings.last().map(|p| p.id)
    } else {
        None
    };
    Ok(Json(
        json!({ "postings": postings, "next_before": next_before }),
    ))
}

// ---- transfers

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTransfer {
    from_account: Uuid,
    to_account: Uuid,
    amount: i64,
    description: Option<String>,
}

pub async fn create_transfer(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let req: NewTransfer = parse(&body)?;
    idempotent(&state, &headers, "POST /transfers", &body, move |conn| {
        Box::pin(async move {
            let entry_id = ledger::transfer(
                conn,
                req.from_account,
                req.to_account,
                req.amount,
                req.description.as_deref(),
            )
            .await?;
            Ok((
                StatusCode::CREATED,
                json!({
                    "entry_id": entry_id,
                    "from_account": req.from_account,
                    "to_account": req.to_account,
                    "amount": req.amount,
                }),
            ))
        })
    })
    .await
}

// ---- deposits

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDeposit {
    account_id: Uuid,
    amount: i64,
}

#[derive(Serialize, FromRow)]
pub struct Deposit {
    id: Uuid,
    account_id: Uuid,
    reference: String,
    amount: i64,
    currency: String,
    status: String,
    entry_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    settled_at: Option<DateTime<Utc>>,
}

const DEPOSIT_COLUMNS: &str =
    "id, account_id, reference, amount, currency, status, entry_id, created_at, settled_at";

/// Opens a pending deposit. The returned `reference` is passed to Paystack when the payment is
/// initialised; the webhook later credits the account against it.
pub async fn create_deposit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let req: NewDeposit = parse(&body)?;
    idempotent(&state, &headers, "POST /deposits", &body, move |conn| {
        Box::pin(async move {
            ledger::validate_amount(req.amount)?;
            let account = ledger::find_account(conn, req.account_id).await?;
            let reference = format!("dep_{}", Uuid::new_v4().simple());
            let deposit = sqlx::query_as::<_, Deposit>(
                "insert into deposits (id, account_id, reference, amount, currency)
                 values ($1, $2, $3, $4, $5)
                 returning id, account_id, reference, amount, currency, status, entry_id,
                           created_at, settled_at",
            )
            .bind(Uuid::new_v4())
            .bind(account.id)
            .bind(&reference)
            .bind(req.amount)
            .bind(&account.currency)
            .fetch_one(&mut *conn)
            .await?;
            Ok((StatusCode::CREATED, json!(deposit)))
        })
    })
    .await
}

pub async fn get_deposit(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Deposit>, ApiError> {
    let id = id_from(path)?;
    let sql = format!("select {DEPOSIT_COLUMNS} from deposits where id = $1");
    let deposit = sqlx::query_as::<_, Deposit>(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("deposit"))?;
    Ok(Json(deposit))
}

// ---- Paystack webhook

/// Credits a pending deposit when Paystack reports a successful charge.
///
/// Answers 200 for every authentic delivery, including duplicates and ones that are rejected,
/// so Paystack stops retrying; the outcome is recorded in `webhook_events`. Only a bad signature
/// or an unreadable body gets an error status.
pub async fn paystack_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let signature = headers
        .get(paystack::SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !paystack::signature_is_valid(&state.paystack_secret, &body, signature) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_signature",
            "webhook signature check failed",
        ));
    }
    let event: paystack::Event = parse(&body)?;
    let mut tx = state.pool.begin().await?;
    let outcome = if event.event == "charge.success" {
        settle_deposit(&mut tx, &event.data).await?
    } else {
        "ignored"
    };
    sqlx::query(
        "insert into webhook_events (provider, event, reference, outcome)
         values ('paystack', $1, $2, $3)",
    )
    .bind(&event.event)
    .bind(&event.data.reference)
    .bind(outcome)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(event = %event.event, reference = %event.data.reference, outcome, "paystack webhook");
    Ok(StatusCode::OK)
}

/// Returns what happened: `credited`, `duplicate`, `unknown_reference` or `rejected_mismatch`.
async fn settle_deposit(
    conn: &mut PgConnection,
    data: &paystack::EventData,
) -> Result<&'static str, ApiError> {
    // The row lock makes a redelivery that arrives mid-processing wait, then see `completed`.
    let sql = format!("select {DEPOSIT_COLUMNS} from deposits where reference = $1 for update");
    let Some(deposit) = sqlx::query_as::<_, Deposit>(sqlx::AssertSqlSafe(sql))
        .bind(&data.reference)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok("unknown_reference");
    };
    if deposit.status != "pending" {
        return Ok("duplicate");
    }
    if data.status != "success"
        || data.amount != deposit.amount
        || data.currency != deposit.currency
    {
        sqlx::query("update deposits set status = 'rejected', settled_at = now() where id = $1")
            .bind(deposit.id)
            .execute(&mut *conn)
            .await?;
        tracing::warn!(reference = %data.reference, "paystack charge does not match the deposit; not credited");
        return Ok("rejected_mismatch");
    }
    let clearing = ledger::clearing_account(conn, "paystack", &deposit.currency).await?;
    let entry_id = ledger::post_entry(
        conn,
        "deposit",
        Some("Paystack deposit"),
        &deposit.currency,
        &[
            Leg {
                account_id: clearing,
                amount: -deposit.amount,
            },
            Leg {
                account_id: deposit.account_id,
                amount: deposit.amount,
            },
        ],
    )
    .await?;
    sqlx::query(
        "update deposits set status = 'completed', entry_id = $2, settled_at = now() where id = $1",
    )
    .bind(deposit.id)
    .bind(entry_id)
    .execute(&mut *conn)
    .await?;
    Ok("credited")
}
