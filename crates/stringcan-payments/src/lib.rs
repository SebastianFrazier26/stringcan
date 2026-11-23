//! The engine never talks to a processor; the API does, through this trait, so
//! Stripe is swappable and tests never touch a network.

mod fake;
mod stripe;

use async_trait::async_trait;
use stringcan_core::Cents;
use thiserror::Error;

pub use fake::FakeProcessor;
pub use stripe::{StripeConfig, StripeTestProcessor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeRequest {
    /// Retrying with the same key must not charge twice.
    pub idempotency_key: String,
    pub gross: Cents,
    pub currency: String,
    pub customer_ref: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeResult {
    pub processor_ref: String,
    /// Fee the processor actually took, from its balance transaction.
    pub fee: Cents,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefundRequest {
    pub idempotency_key: String,
    pub amount: Cents,
    pub customer_ref: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessorError {
    #[error("payment declined: {0}")]
    Declined(String),
    #[error("processor integration not wired: {0}")]
    NotWired(&'static str),
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("refusing live credentials: only test-mode keys are accepted")]
    LiveKeyRefused,
}

#[async_trait]
pub trait PaymentProcessor: Send + Sync {
    async fn charge(&self, req: &ChargeRequest) -> Result<ChargeResult, ProcessorError>;
    async fn refund(&self, req: &RefundRequest) -> Result<String, ProcessorError>;
}
