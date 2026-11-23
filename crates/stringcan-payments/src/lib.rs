//! The engine never talks to a processor; the API does, through these traits, so
//! Stripe is swappable and tests never touch a network.

mod fake;
mod stripe;
mod transport;
mod webhook;

use async_trait::async_trait;
use stringcan_core::Cents;
use thiserror::Error;

pub use fake::FakeProcessor;
pub use stripe::{StripeConfig, StripeConnect};
#[cfg(feature = "http")]
pub use transport::ReqwestTransport;
pub use transport::{FixtureTransport, HttpRequest, HttpResponse, Method, Transport};
pub use webhook::{parse_event, sign_header, verify_signature, Event, WebhookError};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChargeRequest {
    /// Retrying with the same key must not charge twice.
    pub idempotency_key: String,
    pub gross: Cents,
    pub currency: String,
    /// stringcan's own subscriber reference, kept as metadata.
    pub customer_ref: String,
    pub description: String,
    /// Stripe connected account the charge is made on (direct charge).
    pub connected_account: Option<String>,
    /// Stripe customer and saved payment method, both on the connected account.
    pub customer: Option<String>,
    pub payment_method: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeResult {
    /// Processor id of the payment, used to refund it later.
    pub processor_ref: String,
    /// Fee the processor actually took, from its balance transaction.
    pub fee: Cents,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefundRequest {
    pub idempotency_key: String,
    pub amount: Cents,
    pub customer_ref: String,
    pub connected_account: Option<String>,
    /// `processor_ref` of the charge being refunded.
    pub charge_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountStatus {
    pub charges_enabled: bool,
    pub payouts_enabled: bool,
    pub details_submitted: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessorError {
    #[error("payment declined: {0}")]
    Declined(String),
    #[error("processor rejected the request: {0}")]
    Rejected(String),
    #[error("processor temporarily unavailable: {0}")]
    Transient(String),
    #[error("unexpected processor response: {0}")]
    Protocol(&'static str),
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

/// Connected-account lifecycle for tenants. Separate from charging so a tenant's
/// onboarding can be tested and swapped without touching payments.
#[async_trait]
pub trait ConnectAccounts: Send + Sync {
    /// Idempotent per key. Returns the new account id (`acct_...`).
    async fn create_account(
        &self,
        idempotency_key: &str,
        tenant_ref: &str,
    ) -> Result<String, ProcessorError>;
    /// A single-use onboarding URL for the account owner.
    async fn onboarding_link(
        &self,
        account: &str,
        refresh_url: &str,
        return_url: &str,
    ) -> Result<String, ProcessorError>;
    async fn account_status(&self, account: &str) -> Result<AccountStatus, ProcessorError>;
}
