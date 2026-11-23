//! HTTP surface for stringcan. The engine decides money; this crate authenticates,
//! validates, persists commands, and talks to the payment processor.

pub mod auth;
pub mod error;
pub mod routes;
pub mod store;

use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;
use stringcan_payments::PaymentProcessor;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub processor: Arc<dyn PaymentProcessor>,
    pub admin_hash: [u8; 32],
}

pub fn router(state: AppState) -> Router {
    routes::router().with_state(state)
}
