//! `payments-ledger` server. Configuration comes from the environment only:
//!
//! - `DATABASE_URL`: Postgres connection string.
//! - `LEDGER_API_KEY`: bearer key for the API, at least 32 characters.
//! - `PAYSTACK_SECRET_KEY`: Paystack secret key, used to verify webhook signatures.
//! - `BIND_ADDR`: listen address, default `127.0.0.1:8080`.

use payments_ledger::{AppState, MIGRATOR, app};
use sqlx::postgres::PgPoolOptions;
use std::process::ExitCode;

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("{name} is not set"))
}

async fn run() -> Result<(), String> {
    let database_url = required("DATABASE_URL")?;
    let api_key = required("LEDGER_API_KEY")?;
    if api_key.len() < 32 {
        return Err("LEDGER_API_KEY must be at least 32 characters".into());
    }
    let paystack_secret = required("PAYSTACK_SECRET_KEY")?;
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    // Error messages name the step, never the connection string or a key.
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .map_err(|e| format!("could not connect to the database: {e}"))?;
    MIGRATOR
        .run(&pool)
        .await
        .map_err(|e| format!("could not apply migrations: {e}"))?;

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("could not listen on {bind}: {e}"))?;
    tracing::info!(%bind, "payments-ledger listening");
    axum::serve(
        listener,
        app(AppState::new(pool, &api_key, &paystack_secret)),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|e| format!("server error: {e}"))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "payments_ledger=info".into()),
        )
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("payments-ledger: {e}");
            ExitCode::FAILURE
        }
    }
}
