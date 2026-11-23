use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WebhookError {
    #[error("malformed Stripe-Signature header")]
    MalformedHeader,
    #[error("no valid signature")]
    NoValidSignature,
    #[error("timestamp outside tolerance")]
    TimestampOutOfTolerance,
    #[error("malformed event payload")]
    MalformedPayload,
}

fn mac_for(secret: &str, timestamp: i64, payload: &[u8]) -> HmacSha256 {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload);
    mac
}

/// Stripe's scheme: header `t=<unix>,v1=<hex>[,v1=<hex>...]`, where each `v1` is
/// HMAC-SHA256 of `"{t}.{raw body}"` keyed by the endpoint secret. The body must
/// be the raw bytes as received. Comparison is constant time; `now` is injected
/// so the check is testable.
pub fn verify_signature(
    payload: &[u8],
    header: &str,
    secret: &str,
    now: i64,
    tolerance_secs: i64,
) -> Result<(), WebhookError> {
    let mut timestamp = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => timestamp = timestamp.or(v.parse::<i64>().ok()),
            Some(("v1", v)) => sigs.push(v),
            Some(_) => {}
            None => return Err(WebhookError::MalformedHeader),
        }
    }
    let timestamp = timestamp.ok_or(WebhookError::MalformedHeader)?;
    if sigs.is_empty() {
        return Err(WebhookError::MalformedHeader);
    }
    let matched = sigs.iter().any(|s| {
        hex::decode(s).is_ok_and(|bytes| {
            mac_for(secret, timestamp, payload)
                .verify_slice(&bytes)
                .is_ok()
        })
    });
    if !matched {
        return Err(WebhookError::NoValidSignature);
    }
    if (now - timestamp).abs() > tolerance_secs {
        return Err(WebhookError::TimestampOutOfTolerance);
    }
    Ok(())
}

/// Builds a valid header; used by tests and local tooling.
pub fn sign_header(payload: &[u8], secret: &str, timestamp: i64) -> String {
    let sig = hex::encode(mac_for(secret, timestamp, payload).finalize().into_bytes());
    format!("t={timestamp},v1={sig}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: String,
    pub kind: String,
    /// Set on events about a connected account (the `acct_...` it happened on).
    pub account: Option<String>,
    pub object: Value,
}

pub fn parse_event(payload: &[u8]) -> Result<Event, WebhookError> {
    let v: Value = serde_json::from_slice(payload).map_err(|_| WebhookError::MalformedPayload)?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(Event {
        id: text("id").ok_or(WebhookError::MalformedPayload)?,
        kind: text("type").ok_or(WebhookError::MalformedPayload)?,
        account: text("account"),
        object: v
            .pointer("/data/object")
            .cloned()
            .ok_or(WebhookError::MalformedPayload)?,
    })
}
