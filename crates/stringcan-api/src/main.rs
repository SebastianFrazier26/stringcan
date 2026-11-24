use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use stringcan_api::auth::hash_key;
use stringcan_api::bank::BankState;
use stringcan_api::crypto::Keyring;
use stringcan_api::{router, AppState, MIGRATOR};
use stringcan_core::FeeModel;
use stringcan_payments::{
    BankLink, ConnectAccounts, FakeProcessor, PaymentProcessor, Plaid, PlaidConfig,
    PlaidSandboxTransport, ReqwestTransport, StripeConfig, StripeConnect,
};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set (see .env.example)"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let admin_token = required("STRINGCAN_ADMIN_TOKEN");
    assert!(
        admin_token.len() >= 24,
        "STRINGCAN_ADMIN_TOKEN must be at least 24 characters"
    );

    let stripe_mode = std::env::var("STRINGCAN_PROCESSOR").as_deref() == Ok("stripe_test");
    let (processor, connect): (Arc<dyn PaymentProcessor>, Arc<dyn ConnectAccounts>) = if stripe_mode
    {
        let key = StripeConfig::from_env().expect("STRIPE_SECRET_KEY must be a test-mode key");
        let client = Arc::new(StripeConnect::new(
            ReqwestTransport::new(key).expect("http client"),
        ));
        (client.clone(), client)
    } else {
        let fake = Arc::new(FakeProcessor::new(FeeModel::STRIPE_US_CARD));
        (fake.clone(), fake)
    };
    let webhook_secret = std::env::var("STRIPE_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.is_empty());

    // Off unless PLAID_CLIENT_ID is set; once set, misconfiguration is fatal
    // rather than silently running without the float check.
    let bank = std::env::var("PLAID_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|_| {
            let config =
                PlaidConfig::from_env().expect("PLAID_ENV must be sandbox with valid credentials");
            let plaid = Plaid::new(
                PlaidSandboxTransport::new(config).expect("http client"),
                "stringcan",
            )
            .expect("client name");
            let link: Arc<dyn BankLink> = Arc::new(plaid);
            BankState {
                link,
                keys: Arc::new(Keyring::from_env().unwrap_or_else(|e| panic!("{e}"))),
            }
        });

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
        connect,
        webhook_secret,
        require_connect: stripe_mode,
        admin_hash: hash_key(&admin_token),
        snapshot_every: 200,
        bank,
        notifier: Arc::new(stringcan_api::notify::LogNotifier),
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
