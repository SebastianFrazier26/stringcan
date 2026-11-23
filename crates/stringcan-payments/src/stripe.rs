use std::fmt;

use async_trait::async_trait;

use crate::{ChargeRequest, ChargeResult, PaymentProcessor, ProcessorError, RefundRequest};

/// Holds a Stripe secret key. Construction refuses anything but a test-mode key,
/// so a live key can never reach this process by misconfiguration.
#[derive(Clone)]
pub struct StripeConfig {
    #[allow(dead_code)] // read once the HTTP call is wired
    secret_key: String,
}

impl StripeConfig {
    pub fn new(secret_key: &str) -> Result<Self, ProcessorError> {
        if !secret_key.starts_with("sk_test_") || secret_key.len() < 12 {
            return Err(ProcessorError::LiveKeyRefused);
        }
        Ok(StripeConfig {
            secret_key: secret_key.to_owned(),
        })
    }

    pub fn from_env() -> Result<Self, ProcessorError> {
        let key = std::env::var("STRIPE_SECRET_KEY")
            .map_err(|_| ProcessorError::Invalid("STRIPE_SECRET_KEY not set"))?;
        Self::new(&key)
    }
}

impl fmt::Debug for StripeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StripeConfig")
            .field("secret_key", &"sk_test_***")
            .finish()
    }
}

/// Stub: builds the exact `PaymentIntent` form Stripe expects but sends nothing.
/// Wiring HTTP, webhooks and 3-D Secure waits on the owner's Connect decision
/// (docs/DESIGN.md section 15).
#[derive(Debug)]
pub struct StripeTestProcessor {
    #[allow(dead_code)] // read once the HTTP call is wired
    config: StripeConfig,
}

impl StripeTestProcessor {
    pub fn new(config: StripeConfig) -> Self {
        StripeTestProcessor { config }
    }

    pub fn payment_intent_form(
        req: &ChargeRequest,
    ) -> Result<Vec<(String, String)>, ProcessorError> {
        if req.gross <= 0 {
            return Err(ProcessorError::Invalid("amount must be positive"));
        }
        if req.currency.len() != 3 || !req.currency.chars().all(|c| c.is_ascii_lowercase()) {
            return Err(ProcessorError::Invalid(
                "currency must be a 3-letter lowercase code",
            ));
        }
        Ok(vec![
            ("amount".into(), req.gross.to_string()),
            ("currency".into(), req.currency.clone()),
            ("customer".into(), req.customer_ref.clone()),
            ("description".into(), req.description.clone()),
            ("confirm".into(), "true".into()),
            ("automatic_payment_methods[enabled]".into(), "true".into()),
            (
                "automatic_payment_methods[allow_redirects]".into(),
                "never".into(),
            ),
        ])
    }
}

#[async_trait]
impl PaymentProcessor for StripeTestProcessor {
    async fn charge(&self, req: &ChargeRequest) -> Result<ChargeResult, ProcessorError> {
        Self::payment_intent_form(req)?;
        Err(ProcessorError::NotWired("stripe charge"))
    }

    async fn refund(&self, _req: &RefundRequest) -> Result<String, ProcessorError> {
        Err(ProcessorError::NotWired("stripe refund"))
    }
}
