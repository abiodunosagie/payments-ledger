//! # payments-ledger
//!
//! A double-entry payments ledger: accounts, transfers and Paystack deposits, with balances that
//! cannot go negative, journal entries that must balance, idempotent writes and verified webhooks.
//! Postgres enforces the money rules; this crate is the HTTP API in front of it.

pub mod auth;
pub mod error;
pub mod idempotency;
pub mod ledger;
pub mod paystack;
pub mod routes;

use axum::routing::{get, post};
use axum::{Router, middleware};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// SHA-256 of the API key; the key itself is not kept in memory after startup.
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
    let api = Router::new()
        .route("/accounts", post(routes::create_account))
        .route("/accounts/{id}", get(routes::get_account))
        .route("/accounts/{id}/postings", get(routes::list_postings))
        .route("/transfers", post(routes::create_transfer))
        .route("/deposits", post(routes::create_deposit))
        .route("/deposits/{id}", get(routes::get_deposit))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));
    Router::new()
        .merge(api)
        .route("/webhooks/paystack", post(routes::paystack_webhook))
        .route("/health", get(routes::health))
        .with_state(state)
}

/// Embedded migrations, applied at startup and by the test harness.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
