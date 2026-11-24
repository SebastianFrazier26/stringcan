//! HTTP surface for stringcan. The engine decides money; this crate authenticates,
//! validates, persists commands, and talks to the payment processor.

pub mod auth;
pub mod bank;
pub mod connect;
pub mod crypto;
pub mod db;
pub mod error;
pub mod routes;
pub mod store;

use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;
use stringcan_payments::{ConnectAccounts, PaymentProcessor};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub processor: Arc<dyn PaymentProcessor>,
    pub connect: Arc<dyn ConnectAccounts>,
    /// Signing secret for `/v1/webhooks/stripe`; the endpoint is disabled without it.
    pub webhook_secret: Option<String>,
    /// Refuse to charge until the tenant has finished Stripe Connect onboarding.
    pub require_connect: bool,
    pub admin_hash: [u8; 32],
    /// Write a snapshot every N commands per tenant (0 disables).
    pub snapshot_every: i64,
    /// Plaid bank link and the keyring for its stored token; `/v1/bank/*` is off without it.
    pub bank: Option<bank::BankState>,
}

pub fn router(state: AppState) -> Router {
    routes::router()
        .merge(connect::router())
        .merge(bank::router())
        .with_state(state)
}
