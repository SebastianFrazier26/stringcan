use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use stringcan_api::auth::hash_key;
use stringcan_api::{router, AppState, MIGRATOR};
use stringcan_core::FeeModel;
use stringcan_payments::{FakeProcessor, PaymentProcessor, StripeConfig, StripeTestProcessor};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set (see .env.example)"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let admin_token = required("STRINGCAN_ADMIN_TOKEN");
    assert!(
        admin_token.len() >= 24,
        "STRINGCAN_ADMIN_TOKEN must be at least 24 characters"
    );

    let processor: Arc<dyn PaymentProcessor> = match std::env::var("STRINGCAN_PROCESSOR").as_deref()
    {
        Ok("stripe_test") => Arc::new(StripeTestProcessor::new(
            StripeConfig::from_env().expect("STRIPE_SECRET_KEY must be a test-mode key"),
        )),
        _ => Arc::new(FakeProcessor::new(FeeModel::STRIPE_US_CARD)),
    };

    let options = stringcan_api::db::connect_options(
        &required("DATABASE_URL"),
        std::env::var("STRINGCAN_ALLOW_INSECURE_DB").as_deref() == Ok("1"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
        .expect("database connection");
    MIGRATOR.run(&pool).await.expect("migrations");

    let state = AppState {
        pool,
        processor,
        admin_hash: hash_key(&admin_token),
        snapshot_every: 200,
    };
    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    tracing::info!(%addr, "listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}
