//! Idempotency keys for write requests.
//!
//! A client sends `Idempotency-Key: <unique value>` with every write. The first request with a
//! key runs and its response is stored in the same transaction as its effects. A retry with the
//! same key and the same body gets the stored response back and moves no money. Reusing a key for
//! a different request is rejected.
//!
//! Requests with the same key are serialised with a transaction-scoped advisory lock, so two
//! copies sent at the same moment cannot both run: the second waits, then replays the first.

use crate::error::ApiError;
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;

pub const HEADER: &str = "idempotency-key";

/// The response a write produced: status and JSON body.
pub type Outcome = (StatusCode, Value);

pub fn key_from(headers: &HeaderMap) -> Result<String, ApiError> {
    let key = headers
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or("");
    if key.is_empty() || key.len() > 200 {
        return Err(ApiError::bad_request(
            "an Idempotency-Key header (1 to 200 characters) is required on writes",
        ));
    }
    Ok(key.to_owned())
}

/// What the request is: the route plus the exact body. Same key with a different hash is misuse.
pub fn request_hash(route: &str, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(route.as_bytes());
    h.update([0]);
    h.update(body);
    hex::encode(h.finalize())
}

/// Takes the lock for `key` and returns the stored outcome if this key was already used.
pub async fn begin(
    conn: &mut PgConnection,
    key: &str,
    hash: &str,
) -> Result<Option<Outcome>, ApiError> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut *conn)
        .await?;
    let saved = sqlx::query_as::<_, (String, i32, Value)>(
        "select request_hash, status_code, response from idempotency_keys where key = $1",
    )
    .bind(key)
    .fetch_optional(&mut *conn)
    .await?;
    match saved {
        None => Ok(None),
        Some((saved_hash, _, _)) if saved_hash != hash => Err(ApiError::unprocessable(
            "idempotency_key_reused",
            "this Idempotency-Key was already used for a different request",
        )),
        Some((_, status, body)) => {
            let status = u16::try_from(status)
                .ok()
                .and_then(|s| StatusCode::from_u16(s).ok())
                .ok_or_else(ApiError::internal)?;
            Ok(Some((status, body)))
        }
    }
}

pub async fn save(
    conn: &mut PgConnection,
    key: &str,
    hash: &str,
    outcome: &Outcome,
) -> Result<(), ApiError> {
    sqlx::query(
        "insert into idempotency_keys (key, request_hash, status_code, response)
         values ($1, $2, $3, $4)",
    )
    .bind(key)
    .bind(hash)
    .bind(i32::from(outcome.0.as_u16()))
    .bind(&outcome.1)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
