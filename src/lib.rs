//! # payments-ledger
//!
//! A double-entry payments ledger: accounts, transfers and Paystack deposits, with balances that
//! cannot go negative, journal entries that must balance, idempotent writes and verified webhooks.
//! Postgres enforces the money rules; this crate is the HTTP API in front of it.

pub mod auth;
pub mod deposits;
pub mod error;
pub mod idempotency;
pub mod ledger;
pub mod paystack;
pub mod routes;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Router, middleware};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tower_http::timeout::TimeoutLayer;

/// Largest request body accepted. Every request this API takes is a small JSON object.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// A request that has not finished in this time is answered 408 and its work dropped.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// SHA-256 of the API key. Requests are checked against this digest, never the key itself.
    pub api_key_digest: [u8; 32],
    pub paystack_secret: Arc<[u8]>,
}

impl AppState {
    pub fn new(pool: PgPool, api_key: &str, paystack_secret: &str) -> Self {
        Self {
            pool,
            api_key_digest: auth::digest(api_key),
            paystack_secret: Arc::from(paystack_secret.as_bytes()),
        }
    }
}

/// The full HTTP API. Everything except `/health` and the webhook needs the API key.
pub fn app(state: AppState) -> Router {
    use routes::*;
    let api = Router::new()
        .route(ACCOUNTS, post(create_account))
        .route(ACCOUNT, get(get_account))
        .route(ACCOUNT_POSTINGS, get(list_postings))
        .route(TRANSFERS, post(create_transfer))
        .route(DEPOSITS, post(create_deposit))
        .route(DEPOSIT, get(get_deposit))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));
    Router::new()
        .merge(api)
        .route(PAYSTACK_WEBHOOK, post(paystack_webhook))
        .route(HEALTH, get(health))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .with_state(state)
}

/// Embedded migrations, applied at startup and by the test harness.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
