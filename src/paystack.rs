//! Paystack webhook signature and payload.
//!
//! Paystack signs each webhook with an HMAC-SHA512 of the raw request body, keyed with the
//! account's secret key, and sends the hex digest in the `x-paystack-signature` header. The
//! signature is checked on the raw bytes before any JSON parsing, because re-serialising the JSON
//! could change the bytes.

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha512;

pub const SIGNATURE_HEADER: &str = "x-paystack-signature";
pub const CHARGE_SUCCESS: &str = "charge.success";

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

/// Hex signature for `body`, exactly as Paystack would send it. A public helper so integration
/// tests (and anyone replaying a webhook locally) can sign payloads.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha512>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Every Paystack event has a name and a `data` object whose shape depends on the event, so
/// `data` is kept as raw JSON until the name says how to read it.
#[derive(Debug, Deserialize)]
pub struct Event {
    pub event: String,
    #[serde(default)]
    pub data: Value,
}

impl Event {
    /// The `data.reference` of any event, when it has one, for the audit log.
    pub fn reference(&self) -> Option<&str> {
        self.data.get("reference").and_then(Value::as_str)
    }

    /// The charge details of a `charge.success`, or `None` when the payload lacks them.
    pub fn charge(&self) -> Option<ChargeData> {
        serde_json::from_value(self.data.clone()).ok()
    }
}

/// The fields of a charge this service reads. Everything else in the payload is ignored.
#[derive(Debug, Deserialize)]
pub struct ChargeData {
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

    #[test]
    fn events_of_any_shape_parse_and_only_complete_charges_yield_charge_data() {
        let sparse: Event =
            serde_json::from_str(r#"{"event":"subscription.create","data":{"plan":{}}}"#).unwrap();
        assert_eq!(sparse.reference(), None);
        assert!(sparse.charge().is_none());
        let no_data: Event =
            serde_json::from_str(r#"{"event":"customeridentification.failed"}"#).unwrap();
        assert!(no_data.charge().is_none());
        let charge: Event = serde_json::from_str(
            r#"{"event":"charge.success","data":{"reference":"r","amount":5,"currency":"NGN","status":"success","id":9}}"#,
        )
        .unwrap();
        assert_eq!(charge.charge().unwrap().amount, 5);
    }
}
