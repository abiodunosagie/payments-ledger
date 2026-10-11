//! HTTP handlers, kept thin: parse, call the ledger or deposits module, shape the response.
//! Request bodies are read as raw bytes and parsed here, so malformed input gets the same JSON
//! error shape as every other failure, and the exact bytes feed the idempotency hash.

use crate::AppState;
use crate::deposits::{self, Deposit, Outcome as WebhookOutcome};
use crate::error::ApiError;
use crate::idempotency::{self, Outcome};
use crate::ledger::{self, Posting};
use crate::paystack;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Acquire, PgConnection};
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

/// Paths, shared by the router and the idempotency hash so the two cannot drift.
pub const ACCOUNTS: &str = "/accounts";
pub const ACCOUNT: &str = "/accounts/{id}";
pub const ACCOUNT_POSTINGS: &str = "/accounts/{id}/postings";
pub const TRANSFERS: &str = "/transfers";
pub const DEPOSITS: &str = "/deposits";
pub const DEPOSIT: &str = "/deposits/{id}";
pub const PAYSTACK_WEBHOOK: &str = "/webhooks/paystack";
pub const HEALTH: &str = "/health";

/// Statement page size: default and maximum.
const DEFAULT_PAGE: i64 = 50;
const MAX_PAGE: i64 = 100;

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))
}

fn id_from(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, ApiError> {
    path.map(|Path(id)| id)
        .map_err(|_| ApiError::bad_request("the id in the path must be a UUID"))
}

type Work<'c> = Pin<Box<dyn Future<Output = Result<Outcome, ApiError>> + Send + 'c>>;

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
    F: for<'c> FnOnce(&'c mut PgConnection) -> Work<'c>,
{
    let key = idempotency::key_from(headers)?;
    let hash = idempotency::request_hash(route, body);
    let mut tx = state.pool.begin().await?;
    if let Some((status, saved)) = idempotency::begin(&mut tx, &key, &hash).await? {
        let mut response = (status, Json(saved)).into_response();
        response.headers_mut().insert(
            idempotency::REPLAYED_HEADER,
            HeaderValue::from_static("true"),
        );
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
    idempotent(&state, &headers, ACCOUNTS, &body, move |conn| {
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

#[derive(Serialize)]
pub struct StatementPage {
    postings: Vec<Posting>,
    /// Pass as `before` to get the next page; `null` on the last page.
    next_before: Option<i64>,
}

pub async fn list_postings(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<StatementQuery>, QueryRejection>,
) -> Result<Json<StatementPage>, ApiError> {
    let id = id_from(path)?;
    let Query(q) =
        query.map_err(|_| ApiError::bad_request("limit and before must be whole numbers"))?;
    let limit = q.limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_PAGE}"
        )));
    }
    let mut conn = state.pool.acquire().await?;
    ledger::find_account(&mut conn, id).await?;
    let postings = ledger::statement(&mut conn, id, q.before, limit).await?;
    let next_before = if postings.len() as i64 == limit {
        postings.last().map(|p| p.id)
    } else {
        None
    };
    Ok(Json(StatementPage {
        postings,
        next_before,
    }))
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
    idempotent(&state, &headers, TRANSFERS, &body, move |conn| {
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

/// Opens a pending deposit. The returned `reference` is passed to Paystack when the payment is
/// initialised; the webhook later credits the account against it.
pub async fn create_deposit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let req: NewDeposit = parse(&body)?;
    idempotent(&state, &headers, DEPOSITS, &body, move |conn| {
        Box::pin(async move {
            let deposit = deposits::open(conn, req.account_id, req.amount).await?;
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
    let mut conn = state.pool.acquire().await?;
    Ok(Json(deposits::find(&mut conn, id).await?))
}

// ---- Paystack webhook

/// Receives Paystack events.
///
/// Answers 200 for every authentic delivery, whatever its event or outcome, so Paystack stops
/// retrying; what happened is recorded in `webhook_events`. Only a bad signature (401) or a body
/// that is not JSON at all (400) gets an error status.
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
    let outcome = if event.event != paystack::CHARGE_SUCCESS {
        WebhookOutcome::Ignored
    } else if let Some(charge) = event.charge() {
        deposits::settle(&mut tx, &charge).await?
    } else {
        WebhookOutcome::Malformed
    };
    deposits::record_webhook(&mut tx, &event.event, event.reference(), outcome).await?;
    tx.commit().await?;
    tracing::info!(event = %event.event, reference = event.reference(), outcome = outcome.as_str(), "paystack webhook");
    Ok(StatusCode::OK)
}
