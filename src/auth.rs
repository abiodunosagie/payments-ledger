//! Bearer API key on every route except the Paystack webhook, which proves itself with a signature.

use crate::AppState;
use crate::error::ApiError;
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// SHA-256 of a key. Comparing fixed-length digests in constant time hides both the key's
/// contents and its length from timing measurements.
pub fn digest(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?;
    if digest(presented).ct_eq(&state.api_key_digest).into() {
        Ok(next.run(request).await)
    } else {
        Err(ApiError::unauthorized())
    }
}
