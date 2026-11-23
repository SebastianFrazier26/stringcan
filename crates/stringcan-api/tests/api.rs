//! Postgres-backed tests. They skip cleanly unless `STRINGCAN_TEST_DB=1` and
//! `DATABASE_URL` point at a disposable database.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use stringcan_api::auth::{admin_matches, generate_api_key, hash_key};
use stringcan_api::{router, AppState, MIGRATOR};
use stringcan_core::FeeModel;
use stringcan_payments::FakeProcessor;
use tower::ServiceExt;

const ADMIN: &str = "test-admin-token-0123456789abcdef";

async fn app() -> Option<Router> {
    if std::env::var("STRINGCAN_TEST_DB").as_deref() != Ok("1") {
        eprintln!("skipping: STRINGCAN_TEST_DB is not 1");
        return None;
    }
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL with STRINGCAN_TEST_DB=1");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    Some(router(AppState {
        pool,
        processor: Arc::new(FakeProcessor::new(FeeModel::STRIPE_US_CARD)),
        admin_hash: hash_key(ADMIN),
    }))
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn new_tenant(app: &Router, upkeep: i64) -> (String, String) {
    let (s, v) = call(
        app,
        "POST",
        "/v1/tenants",
        Some(ADMIN),
        Some(json!({"name": "t", "first_upkeep_cents": upkeep})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (
        v["tenant_id"].as_str().unwrap().into(),
        v["api_key"].as_str().unwrap().into(),
    )
}

#[test]
fn keys_are_random_prefixed_and_hash_compared() {
    let (a, b) = (generate_api_key(), generate_api_key());
    assert!(a.starts_with("sc_") && a.len() == 3 + 64 && a != b);
    assert!(admin_matches(&hash_key("x"), "x"));
    assert!(!admin_matches(&hash_key("x"), "y"));
}

#[tokio::test]
async fn auth_is_required_and_scoped() {
    let Some(app) = app().await else { return };
    let (_, key_a) = new_tenant(&app, 1000).await;
    let (tenant_b, key_b) = new_tenant(&app, 1000).await;

    assert_eq!(
        call(&app, "GET", "/v1/quote?external_ref=u1", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/v1/quote?external_ref=u1",
            Some("sc_nope"),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/tenants",
            Some(&key_a),
            Some(json!({"name": "x", "first_upkeep_cents": 1}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED,
        "an owner key cannot mint tenants"
    );

    // A pays; B's books must not move.
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/payments",
            Some(&key_a),
            Some(json!({"external_ref": "u1"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, ledger_b) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant_b}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(ledger_b["entries"].as_array().unwrap().len(), 0);
    let (_, q) = call(&app, "GET", "/v1/quote?external_ref=u1", Some(&key_b), None).await;
    assert_eq!(q["share"], 1000);
}

#[tokio::test]
async fn full_cycle_persists_settles_and_publishes_a_pseudonymous_ledger() {
    let Some(app) = app().await else { return };
    let (tenant, key) = new_tenant(&app, 1000).await;

    let (s, paid) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(json!({"external_ref": "alice@example.com"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{paid}");
    assert_eq!(
        (paid["charged_gross"].as_i64(), paid["period"].as_i64()),
        (Some(1061), Some(1))
    );
    // Period 1 is covered by the first payer, so a second payment prefunds period 2.
    let (_, again) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(json!({"external_ref": "alice@example.com"})),
    )
    .await;
    assert_eq!(again["period"], 2);

    let (s, settled) = call(
        &app,
        "POST",
        "/v1/periods/settle",
        Some(&key),
        Some(json!({"actual_cents": 900, "evidence": "inv-42"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{settled}");
    assert_eq!(settled["payout"], 900);

    let (s, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ledger["break_even"], true);
    assert_eq!(ledger["credits_outstanding_cents"], 100);
    assert_eq!(ledger["periods"][0]["settled"]["evidence"], "inv-42");
    let text = ledger.to_string();
    assert!(
        !text.contains("alice") && text.contains("s-1"),
        "ledger leaks an external ref: {text}"
    );

    let (s, refund) = call(
        &app,
        "POST",
        "/v1/subscribers/alice@example.com/refund-credit",
        Some(&key),
        None,
    )
    .await;
    assert_eq!(
        (s, refund["refunded_cents"].as_i64()),
        (StatusCode::OK, Some(100))
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/subscribers/alice@example.com/refund-credit",
            Some(&key),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn declined_card_records_nothing() {
    let Some(app) = app().await else { return };
    let (tenant, key) = new_tenant(&app, 1000).await;
    let (s, _) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(json!({"external_ref": "decline-me"})),
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(ledger["entries"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn bad_input_is_rejected_at_the_boundary() {
    let Some(app) = app().await else { return };
    let (_, key) = new_tenant(&app, 1000).await;
    for bad in ["'; DROP TABLE tenants;--", "has space", ""] {
        let (s, _) = call(
            &app,
            "POST",
            "/v1/payments",
            Some(&key),
            Some(json!({"external_ref": bad})),
        )
        .await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY, "{bad:?} -> {s}");
    }
    let (s, _) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(json!({"external_ref": "u", "extra": 1})),
    )
    .await;
    assert!(s.is_client_error(), "unknown fields rejected");
    let (s, _) = call(
        &app,
        "POST",
        "/v1/periods/settle",
        Some(&key),
        Some(json!({"actual_cents": -5})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(
        &app,
        "POST",
        "/v1/tenants",
        Some(ADMIN),
        Some(json!({"name": "", "first_upkeep_cents": 1})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn concurrent_payers_serialise_per_tenant() {
    let Some(app) = app().await else { return };
    let (tenant, key) = new_tenant(&app, 1200).await;
    let mut handles = Vec::new();
    for i in 0..6 {
        let (app, key) = (app.clone(), key.clone());
        handles.push(tokio::spawn(async move {
            call(
                &app,
                "POST",
                "/v1/payments",
                Some(&key),
                Some(json!({"external_ref": format!("u{i}")})),
            )
            .await
            .0
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(ledger["break_even"], true);
    assert_eq!(ledger["entries"].as_array().unwrap().len(), 6);
}
