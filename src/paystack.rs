//! Paystack webhook signature and payload.
//!
//! Paystack signs each webhook with an HMAC-SHA512 of the raw request body, keyed with the
//! account's secret key, and sends the hex digest in the `x-paystack-signature` header. The
//! signature is checked on the raw bytes before any JSON parsing, because re-serialising the JSON
//! could change the bytes.

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha512;

pub const SIGNATURE_HEADER: &str = "x-paystack-signature";

/// True when `signature_hex` is the HMAC-SHA512 of `body` under `secret`. The comparison runs in
/// constant time (`verify_slice`), so a forger learns nothing from response timing.
pub fn signature_is_valid(secret: &[u8], body: &[u8], signature_hex: &str) -> bool {
    let Ok(signature) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha512>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

/// Hex signature for `body`, as Paystack would send it. Used by tests and local tooling.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha512>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// The fields of a webhook this service reads. Everything else in the payload is ignored.
#[derive(Debug, Deserialize)]
pub struct Event {
    pub event: String,
    pub data: EventData,
}

#[derive(Debug, Deserialize)]
pub struct EventData {
    pub reference: String,
    /// Amount in the currency's minor unit (kobo for NGN), as Paystack reports it.
    pub amount: i64,
    pub currency: String,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_only_for_the_same_body_and_secret() {
        let body = br#"{"event":"charge.success"}"#;
        let sig = sign(b"sk_test_secret", body);
        assert!(signature_is_valid(b"sk_test_secret", body, &sig));
        assert!(!signature_is_valid(b"sk_test_other", body, &sig));
        assert!(!signature_is_valid(b"sk_test_secret", b"{}", &sig));
        assert!(!signature_is_valid(b"sk_test_secret", body, "not-hex"));
        assert!(!signature_is_valid(b"sk_test_secret", body, ""));
        assert!(!signature_is_valid(b"sk_test_secret", body, &sig[..64]));
    }
}
