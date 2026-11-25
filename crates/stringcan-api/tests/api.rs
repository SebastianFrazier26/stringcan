#![allow(clippy::many_single_char_names)]
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
use stringcan_api::bank::BankState;
use stringcan_api::crypto::Keyring;
use stringcan_api::notify::RecordingNotifier;
use stringcan_api::{router, AppState, MIGRATOR};
use stringcan_core::FeeModel;
use stringcan_payments::{FakeProcessor, JsonFixtureTransport, Plaid};
use tower::ServiceExt;

const ADMIN: &str = "test-admin-token-0123456789abcdef";

async fn app() -> Option<Router> {
    app_with_pool().await.map(|(r, _)| r)
}

async fn app_with_pool() -> Option<(Router, sqlx::PgPool)> {
    build(false, Some(WEBHOOK_SECRET)).await
}

const WEBHOOK_SECRET: &str = "whsec_test_secret_for_tests";

async fn build(
    require_connect: bool,
    webhook_secret: Option<&str>,
) -> Option<(Router, sqlx::PgPool)> {
    build_with_bank(require_connect, webhook_secret, None).await
}

async fn build_with_bank(
    require_connect: bool,
    webhook_secret: Option<&str>,
    bank: Option<BankState>,
) -> Option<(Router, sqlx::PgPool)> {
    build_with(
        require_connect,
        webhook_secret,
        bank,
        Arc::new(RecordingNotifier::default()),
    )
    .await
}

async fn build_with(
    require_connect: bool,
    webhook_secret: Option<&str>,
    bank: Option<BankState>,
    notifier: Arc<RecordingNotifier>,
) -> Option<(Router, sqlx::PgPool)> {
    let fake = Arc::new(FakeProcessor::new(FeeModel::STRIPE_US_CARD));
    build_full(require_connect, webhook_secret, bank, notifier, fake).await
}

async fn build_full(
    require_connect: bool,
    webhook_secret: Option<&str>,
    bank: Option<BankState>,
    notifier: Arc<RecordingNotifier>,
    fake: Arc<FakeProcessor>,
) -> Option<(Router, sqlx::PgPool)> {
    if std::env::var("STRINGCAN_TEST_DB").as_deref() != Ok("1") {
        eprintln!("skipping: STRINGCAN_TEST_DB is not 1");
        return None;
    }
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL with STRINGCAN_TEST_DB=1");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let app = router(AppState {
        pool: pool.clone(),
        processor: fake.clone(),
        connect: fake,
        webhook_secret: webhook_secret.map(str::to_owned),
        require_connect,
        admin_hash: hash_key(ADMIN),
        snapshot_every: 3,
        bank,
        notifier,
    });
    Some((app, pool))
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
    let notifier = Arc::new(RecordingNotifier::default());
    let Some((app, pool)) = build_with(false, None, None, notifier.clone()).await else {
        return;
    };
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
    // Period 1 is covered by the first payer, so a second payment renews period 2,
    // which needs a price notice and the subscriber's consent first (rules v2).
    let pay_alice = json!({"external_ref": "alice@example.com"});
    let (s, v) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(pay_alice.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, n) = call(&app, "POST", "/v1/periods/2/notices", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK, "{n}");
    assert_eq!(
        (n["noticed"].as_i64(), n["consent_needed"].as_i64()),
        (Some(1), Some(1))
    );
    let sent = notifier.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (
            sent[0].external_ref.as_str(),
            sent[0].ceiling_gross,
            sent[0].lead_days
        ),
        ("alice@example.com", 1061, 10)
    );
    let (s, _) = call(&app, "POST", "/v1/payments", Some(&key), Some(pay_alice)).await;
    assert_eq!(s, StatusCode::CONFLICT, "no consent on file yet");
    let (s, v) = call(
        &app,
        "PUT",
        "/v1/subscribers/alice@example.com/renewal-cap",
        Some(&key),
        Some(json!({"max_charge_cents": 1061})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, early) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(json!({"external_ref": "alice@example.com"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "notice is not 10 days old yet");
    assert!(
        early["error"].as_str().unwrap().contains("10 days"),
        "{early}"
    );
    backdate_notices(&pool, &tenant, 10).await;
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

#[tokio::test]
async fn snapshots_are_written_used_and_distrusted_when_invalid() {
    let Some((app, pool)) = app_with_pool().await else {
        return;
    };
    let (tenant, key) = new_tenant(&app, 1000).await;
    for i in 0..7 {
        let (s, _) = call(
            &app,
            "POST",
            "/v1/payments",
            Some(&key),
            Some(json!({"external_ref": format!("u{i}")})),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
    let tid: uuid::Uuid = tenant.parse().unwrap();
    let seqs: Vec<i64> = sqlx::query_scalar("SELECT next_seq FROM snapshots WHERE tenant_id = $1")
        .bind(tid)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(seqs, vec![6], "every third command, older snapshots pruned");

    let url = format!("/v1/public/{tenant}/ledger");
    let (_, with_snapshot) = call(&app, "GET", &url, None, None).await;

    sqlx::query("UPDATE snapshots SET state = '{\"garbage\": true}' WHERE tenant_id = $1")
        .bind(tid)
        .execute(&pool)
        .await
        .unwrap();
    let (_, with_bad_snapshot) = call(&app, "GET", &url, None, None).await;
    sqlx::query("DELETE FROM snapshots WHERE tenant_id = $1")
        .bind(tid)
        .execute(&pool)
        .await
        .unwrap();
    let (_, full_replay) = call(&app, "GET", &url, None, None).await;

    assert_eq!(with_snapshot, full_replay);
    assert_eq!(with_bad_snapshot, full_replay);
    assert_eq!(full_replay["entries"].as_array().unwrap().len(), 7);
}

async fn webhook(app: &Router, body: &str, signature: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::builder().method("POST").uri("/v1/webhooks/stripe");
    if let Some(sig) = signature {
        req = req.header("stripe-signature", sig);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Event ids are the webhook idempotency key and the test database persists
/// across runs, so a fixed id would be a duplicate on every run after the first.
fn event_id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}

fn account_updated(event_id: &str, account: &str, charges: bool) -> String {
    json!({"id": event_id, "type": "account.updated", "account": account,
           "data": {"object": {"charges_enabled": charges, "payouts_enabled": charges, "details_submitted": true}}})
    .to_string()
}

#[tokio::test]
async fn connect_onboarding_webhook_and_charge_flow() {
    let Some((app, _)) = build(true, Some(WEBHOOK_SECRET)).await else {
        return;
    };
    let (_, key) = new_tenant(&app, 1000).await;
    let pay = |r: &str| json!({"external_ref": r});

    let (s, e) = call(&app, "POST", "/v1/payments", Some(&key), Some(pay("alice"))).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "no payments before onboarding: {e}"
    );

    let (s, acct) = call(&app, "POST", "/v1/connect/account", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK, "{acct}");
    let account = acct["account_id"].as_str().unwrap().to_owned();
    let (_, again) = call(&app, "POST", "/v1/connect/account", Some(&key), None).await;
    assert_eq!(
        again["account_id"],
        account.as_str(),
        "creating twice returns the same account"
    );

    let link = json!({"refresh_url": "https://app.example/refresh", "return_url": "https://app.example/done"});
    let (s, l) = call(
        &app,
        "POST",
        "/v1/connect/onboarding-link",
        Some(&key),
        Some(link),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(l["url"].as_str().unwrap().contains(&account));

    let (_, st) = call(&app, "GET", "/v1/connect/status", Some(&key), None).await;
    assert_eq!(st["charges_enabled"], false);

    // Webhook authenticity: wrong secret, tampered body, stale timestamp, missing header.
    let body = account_updated(&event_id("evt_flow"), &account, true);
    let good = stringcan_payments::sign_header(body.as_bytes(), WEBHOOK_SECRET, now());
    let forged = stringcan_payments::sign_header(body.as_bytes(), "whsec_wrong", now());
    let stale = stringcan_payments::sign_header(body.as_bytes(), WEBHOOK_SECRET, now() - 3600);
    for sig in [Some(forged), Some(stale), None] {
        assert_eq!(webhook(&app, &body, sig).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        webhook(&app, &body.replace("true", "false"), Some(good.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, st) = call(&app, "GET", "/v1/connect/status", Some(&key), None).await;
    assert_eq!(
        st["charges_enabled"], false,
        "rejected webhooks change nothing"
    );

    let (s, v) = webhook(&app, &body, Some(good.clone())).await;
    assert_eq!((s, v["received"].as_bool()), (StatusCode::OK, Some(true)));
    let (s, v) = webhook(&app, &body, Some(good)).await;
    assert_eq!(
        (s, v["duplicate"].as_bool()),
        (StatusCode::OK, Some(true)),
        "Stripe retries are idempotent"
    );
    let (_, st) = call(&app, "GET", "/v1/connect/status", Some(&key), None).await;
    assert_eq!(
        (
            st["charges_enabled"].as_bool(),
            st["details_submitted"].as_bool()
        ),
        (Some(true), Some(true))
    );

    // An event for someone else's account must not touch this tenant.
    let other = account_updated(&event_id("evt_other"), "acct_someoneelse", false);
    let sig = stringcan_payments::sign_header(other.as_bytes(), WEBHOOK_SECRET, now());
    assert_eq!(webhook(&app, &other, Some(sig)).await.0, StatusCode::OK);
    let (_, st) = call(&app, "GET", "/v1/connect/status", Some(&key), None).await;
    assert_eq!(st["charges_enabled"], true);

    let (s, _) = call(
        &app,
        "PUT",
        "/v1/subscribers/alice/payment-method",
        Some(&key),
        Some(json!({"customer": "cus_Alice123", "payment_method": "pm_Card123"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for bad in [
        json!({"customer": "cus_x", "payment_method": "pm_Card123"}),
        json!({"customer": "cus_Alice123", "payment_method": "card_123"}),
        json!({"customer": "cus_Alice123", "payment_method": "pm_a/../b"}),
    ] {
        let (s, _) = call(
            &app,
            "PUT",
            "/v1/subscribers/alice/payment-method",
            Some(&key),
            Some(bad),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    }

    let (s, paid) = call(&app, "POST", "/v1/payments", Some(&key), Some(pay("alice"))).await;
    assert_eq!(s, StatusCode::OK, "{paid}");

    // Credit refunds go back against that charge.
    let (s, _) = call(
        &app,
        "POST",
        "/v1/periods/settle",
        Some(&key),
        Some(json!({"actual_cents": 900})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, r) = call(
        &app,
        "POST",
        "/v1/subscribers/alice/refund-credit",
        Some(&key),
        None,
    )
    .await;
    assert_eq!(
        (s, r["refunded_cents"].as_i64()),
        (StatusCode::OK, Some(100))
    );
}

#[tokio::test]
async fn webhook_endpoint_is_off_without_a_secret_and_tenants_cannot_share_onboarding() {
    let Some((app, _)) = build(false, None).await else {
        return;
    };
    let body = account_updated(&event_id("evt_off"), "acct_x1234", true);
    let sig = stringcan_payments::sign_header(body.as_bytes(), WEBHOOK_SECRET, now());
    assert_eq!(
        webhook(&app, &body, Some(sig)).await.0,
        StatusCode::NOT_FOUND
    );

    let (_, key_a) = new_tenant(&app, 1000).await;
    let (_, key_b) = new_tenant(&app, 1000).await;
    let (_, a) = call(&app, "POST", "/v1/connect/account", Some(&key_a), None).await;
    let (_, b) = call(&app, "POST", "/v1/connect/account", Some(&key_b), None).await;
    assert_ne!(a["account_id"], b["account_id"]);
    assert_eq!(
        call(&app, "POST", "/v1/connect/account", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

const TOKEN_KEY: &str = "k1:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const ACCESS_TOKEN: &str = "access-sandbox-de3ce8ef-33f8-452c-a685-8671031fc0f6";

fn plaid_ok(body: &str) -> stringcan_payments::JsonResponse {
    JsonFixtureTransport::ok(body)
}

fn balance(account: &str, kind: &str, available: f64) -> stringcan_payments::JsonResponse {
    plaid_ok(&format!(
        r#"{{"accounts":[{{"account_id":"{account}","type":"{kind}","balances":{{"available":{available},"current":{available},"iso_currency_code":"USD"}}}}],"request_id":"r"}}"#
    ))
}

fn exchanged(item: &str) -> stringcan_payments::JsonResponse {
    plaid_ok(&format!(
        r#"{{"access_token":"{ACCESS_TOKEN}","item_id":"{item}","request_id":"r"}}"#
    ))
}

#[tokio::test]
async fn bank_routes_are_off_without_plaid() {
    let Some(app) = app().await else { return };
    let (_, key) = new_tenant(&app, 1000).await;
    for (m, uri) in [
        ("POST", "/v1/bank/link-token"),
        ("GET", "/v1/bank/float"),
        ("DELETE", "/v1/bank/link"),
    ] {
        assert_eq!(
            call(&app, m, uri, Some(&key), None).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn bank_link_stores_an_encrypted_token_and_checks_only_the_payout_account() {
    let transport = Arc::new(JsonFixtureTransport::new([
        plaid_ok(r#"{"link_token":"link-sandbox-1","request_id":"r"}"#),
        // Linking a credit account is refused and the new item revoked.
        exchanged("item-credit"),
        balance("acct-card", "credit", 900.0),
        plaid_ok(r#"{"request_id":"r"}"#),
        // Linking the payout checking account.
        exchanged("item-1"),
        balance("acct-payout", "depository", 5.0),
        // Float checks: short, verified, then the bank wants a fresh login.
        balance("acct-payout", "depository", 5.0),
        balance("acct-payout", "depository", 20.0),
        JsonFixtureTransport::status(
            400,
            r#"{"error_type":"ITEM_ERROR","error_code":"ITEM_LOGIN_REQUIRED","error_message":"free text"}"#,
        ),
        // Unlink.
        plaid_ok(r#"{"request_id":"r"}"#),
    ]));
    let bank = BankState {
        link: Arc::new(Plaid::new(transport.clone(), "stringcan").unwrap()),
        keys: Arc::new(Keyring::new(TOKEN_KEY, "").unwrap()),
    };
    let Some((app, pool)) = build_with_bank(false, None, Some(bank)).await else {
        return;
    };
    let (tenant, key) = new_tenant(&app, 1000).await;
    let (_, other_key) = new_tenant(&app, 1000).await;

    let (s, v) = call(&app, "POST", "/v1/bank/link-token", Some(&key), None).await;
    assert_eq!(
        (s, v["link_token"].as_str()),
        (StatusCode::OK, Some("link-sandbox-1"))
    );
    let sent = transport.requests();
    assert_eq!(sent[0].body["products"], json!(["auth"]));
    assert_eq!(
        sent[0].body["user"]["client_user_id"],
        format!("tenant-{tenant}")
    );

    let (s, _) = call(
        &app,
        "POST",
        "/v1/bank/link",
        Some(&key),
        Some(json!({"public_token": "public-sandbox-x", "account_id": "acct-card"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(transport.requests()[3].path, "/item/remove");

    let (s, v) = call(
        &app,
        "POST",
        "/v1/bank/link",
        Some(&key),
        Some(json!({"public_token": "public-sandbox-y", "account_id": "acct-payout"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let sent = transport.requests();
    assert_eq!(
        sent[5].body["options"]["account_ids"],
        json!(["acct-payout"])
    );

    let (key_id, ciphertext): (String, Vec<u8>) = sqlx::query_as(
        "SELECT token_key_id, token_ciphertext FROM bank_links WHERE tenant_id = $1::uuid",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(key_id, "k1");
    assert!(!String::from_utf8_lossy(&ciphertext).contains("access-sandbox"));

    assert_eq!(
        call(&app, "GET", "/v1/bank/float", Some(&other_key), None)
            .await
            .0,
        StatusCode::CONFLICT
    );

    // One payment of 1000 net leaves 1000 in the open pool: $5 is short, $20 is enough.
    let (s, _) = call(
        &app,
        "POST",
        "/v1/payments",
        Some(&key),
        Some(json!({"external_ref": "u1"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = call(&app, "GET", "/v1/bank/float", Some(&key), None).await;
    assert_eq!(v, json!({"status": "short"}));
    let (_, v) = call(&app, "GET", "/v1/bank/float", Some(&key), None).await;
    assert_eq!(v, json!({"status": "verified"}));
    let sent = transport.requests();
    assert_eq!(sent[7].body["access_token"], ACCESS_TOKEN);
    assert_eq!(
        sent[7].body["options"]["account_ids"],
        json!(["acct-payout"])
    );

    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(ledger["float_check"]["status"], "verified");
    assert!(!ledger.to_string().contains("acct-payout"));

    let (s, v) = call(&app, "GET", "/v1/bank/float", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "unverifiable");
    assert!(!v.to_string().contains("free text"));

    let (s, _) = call(&app, "DELETE", "/v1/bank/link", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(transport.requests().last().unwrap().path, "/item/remove");
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert!(ledger["float_check"].is_null());
}

#[tokio::test]
async fn bank_token_is_resealed_under_the_current_key_after_rotation() {
    const NEW_KEY: &str = "k2:1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";
    let old = Arc::new(JsonFixtureTransport::new([
        exchanged("item-rot"),
        balance("acct-payout", "depository", 1.0),
    ]));
    let Some((app_old, pool)) = build_with_bank(
        false,
        None,
        Some(BankState {
            link: Arc::new(Plaid::new(old, "stringcan").unwrap()),
            keys: Arc::new(Keyring::new(TOKEN_KEY, "").unwrap()),
        }),
    )
    .await
    else {
        return;
    };
    let (tenant, key) = new_tenant(&app_old, 1000).await;
    let (s, _) = call(
        &app_old,
        "POST",
        "/v1/bank/link",
        Some(&key),
        Some(json!({"public_token": "public-sandbox-z", "account_id": "acct-payout"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let rotated = Arc::new(JsonFixtureTransport::new([balance(
        "acct-payout",
        "depository",
        1.0,
    )]));
    let app_new = build_with_bank(
        false,
        None,
        Some(BankState {
            link: Arc::new(Plaid::new(rotated.clone(), "stringcan").unwrap()),
            keys: Arc::new(Keyring::new(NEW_KEY, TOKEN_KEY).unwrap()),
        }),
    )
    .await
    .unwrap()
    .0;
    let (_, v) = call(&app_new, "GET", "/v1/bank/float", Some(&key), None).await;
    assert_eq!(v["status"], "verified");
    assert_eq!(rotated.requests()[0].body["access_token"], ACCESS_TOKEN);
    let key_id: String =
        sqlx::query_scalar("SELECT token_key_id FROM bank_links WHERE tenant_id = $1::uuid")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(key_id, "k2");
}

/// Moves a tenant's notice times back, standing in for the wait before billing.
async fn backdate_notices(pool: &sqlx::PgPool, tenant: &str, days: i32) {
    sqlx::query(
        "UPDATE price_notices SET issued_at = issued_at - make_interval(days => $2)
         WHERE tenant_id = $1::uuid",
    )
    .bind(tenant)
    .bind(days)
    .execute(pool)
    .await
    .unwrap();
}

async fn pay_ok(app: &Router, key: &str, who: &str) -> Value {
    let (s, v) = call(
        app,
        "POST",
        "/v1/payments",
        Some(key),
        Some(json!({ "external_ref": who })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{who}: {v}");
    v
}

#[tokio::test]
async fn a_failed_notice_delivery_records_nothing() {
    let notifier = Arc::new(RecordingNotifier::default());
    let Some((app, _)) = build_with(false, None, None, notifier.clone()).await else {
        return;
    };
    let (tenant, key) = new_tenant(&app, 1000).await;
    pay_ok(&app, &key, "u1").await;
    notifier.fail_next(true);
    let (s, _) = call(&app, "POST", "/v1/periods/2/notices", Some(&key), None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert!(!ledger.to_string().contains("PriceNotice"));
    notifier.fail_next(false);
    let (_, n) = call(&app, "POST", "/v1/periods/2/notices", Some(&key), None).await;
    assert_eq!(n["noticed"], 1);
    let (_, n) = call(&app, "POST", "/v1/periods/2/notices", Some(&key), None).await;
    assert_eq!(n["noticed"], 0, "nobody is noticed twice");
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert!(ledger.to_string().contains("PriceNotice"));
}

#[tokio::test]
async fn credit_over_the_cap_is_refunded_to_the_payers_own_charge_at_settlement() {
    let Some((app, pool)) = app_with_pool().await else {
        return;
    };
    let (tenant, key) = new_tenant(&app, 100_000).await;
    pay_ok(&app, &key, "first").await;
    // "early" prefunds period 2 at half the upkeep; ten more joiners drive the share down.
    let early = pay_ok(&app, &key, "early").await;
    assert_eq!(early["period"], 2);
    for i in 0..10 {
        pay_ok(&app, &key, &format!("j{i}")).await;
    }
    let settle = |actual: i64| json!({ "actual_cents": actual });
    let (s, _) = call(
        &app,
        "POST",
        "/v1/periods/settle",
        Some(&key),
        Some(settle(100_000)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, p2) = call(
        &app,
        "POST",
        "/v1/periods/settle",
        Some(&key),
        Some(settle(100_000)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p2}");
    // "early" paid 500.00 for a share of about 91.75, so its credit is far above
    // its cap of two shares; the excess goes back to the card it paid with.
    let refunds = p2["refunds"].as_array().unwrap();
    assert!(refunds.iter().all(|r| r["reason"] == "OverCap"), "{p2}");
    let leg = refunds
        .iter()
        .find(|r| r["user"] == 2)
        .expect("early is refunded");
    let share = p2["shares"][0]["share"].as_i64().unwrap();
    let issued = p2["shares"][0]["credit_issued"].as_i64().unwrap();
    assert_eq!(leg["amount"].as_i64().unwrap(), issued - 2 * share);
    let early_charge: String = sqlx::query_scalar(
        "SELECT c.processor_ref FROM commands c JOIN subscribers s
           ON s.tenant_id = c.tenant_id AND (c.command->'RecordPayment'->>'user')::bigint = s.user_id
         WHERE c.tenant_id = $1::uuid AND s.external_ref = 'early'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leg["charge_ref"], early_charge.as_str());
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(ledger["break_even"], true);
}

#[tokio::test]
async fn tenants_created_before_rules_v2_keep_v1_behaviour() {
    let Some((app, pool)) = app_with_pool().await else {
        return;
    };
    let key = generate_api_key();
    let v1_config = json!({"fee": {"bps": 290, "fixed": 30}, "reserve_cap_bps": 5000, "reserve_surcharge_bps": 0});
    sqlx::query(
        "INSERT INTO tenants (name, api_key_hash, engine_config, first_upkeep_cents) VALUES ('old', $1, $2, 1000)",
    )
    .bind(hash_key(&key).as_slice())
    .bind(sqlx::types::Json(v1_config))
    .execute(&pool)
    .await
    .unwrap();
    pay_ok(&app, &key, "alice").await;
    let again = pay_ok(&app, &key, "alice").await;
    assert_eq!(again["period"], 2, "v1 renews without a notice");
    let (s, _) = call(&app, "POST", "/v1/periods/2/notices", Some(&key), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let versions: Vec<i32> =
        sqlx::query_scalar("SELECT DISTINCT c.rules_version FROM commands c JOIN tenants t ON t.id = c.tenant_id WHERE t.name = 'old' AND t.api_key_hash = $1")
            .bind(hash_key(&key).as_slice())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, vec![1]);
}

#[tokio::test]
async fn renewals_wait_for_the_ten_day_notice_lead() {
    let Some((app, pool)) = app_with_pool().await else {
        return;
    };
    let (tenant, key) = new_tenant(&app, 1000).await;
    pay_ok(&app, &key, "bob").await;
    let (s, _) = call(&app, "POST", "/v1/periods/2/notices", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "PUT",
        "/v1/subscribers/bob/renewal-cap",
        Some(&key),
        Some(json!({"max_charge_cents": 5000})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let renew = || {
        call(
            &app,
            "POST",
            "/v1/payments",
            Some(&key),
            Some(json!({"external_ref": "bob"})),
        )
    };

    backdate_notices(&pool, &tenant, 9).await;
    let (s, v) = renew().await;
    assert_eq!(s, StatusCode::CONFLICT, "9 days is too early: {v}");

    // A notice logged before notice times were recorded falls back to the latest
    // notices run for the period, which is now: still too early.
    sqlx::query("DELETE FROM price_notices WHERE tenant_id = $1::uuid")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = renew().await;
    assert_eq!(s, StatusCode::CONFLICT);
    sqlx::query(
        "UPDATE commands SET created_at = created_at - interval '10 days'
         WHERE tenant_id = $1::uuid AND command ? 'IssueNotices'",
    )
    .bind(&tenant)
    .execute(&pool)
    .await
    .unwrap();
    let (s, v) = renew().await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["period"], 2);
}

#[tokio::test]
async fn new_tenants_get_rules_v3_with_the_absolute_credit_ceiling() {
    let Some((app, pool)) = app_with_pool().await else {
        return;
    };
    let (tenant, _) = new_tenant(&app, 1000).await;
    let cfg: Value = sqlx::query_scalar("SELECT engine_config FROM tenants WHERE id = $1::uuid")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cfg["rules_version"], 3);
    assert_eq!(cfg["credit_cap_ceiling"], 50_000);
}

fn auth_numbers(account: &str, number: &str, routing: &str) -> stringcan_payments::JsonResponse {
    plaid_ok(&format!(
        r#"{{"accounts":[],"numbers":{{"ach":[{{"account_id":"{account}","account":"{number}","routing":"{routing}","wire_routing":null}}]}},"request_id":"r"}}"#
    ))
}

fn stripe_bank(routing: &str, last4: &str) -> stringcan_payments::PayoutBank {
    stringcan_payments::PayoutBank {
        routing_number: routing.into(),
        last4: last4.into(),
        currency: "usd".into(),
        default_for_currency: true,
    }
}

#[tokio::test]
async fn the_linked_account_must_be_the_stripe_payout_account() {
    let transport = Arc::new(JsonFixtureTransport::new([
        // Without a Connect account the link stays owner-declared (no auth call).
        exchanged("item-nc"),
        balance("acct-chk", "depository", 50.0),
        // A different account than Stripe pays out to: refused and revoked.
        exchanged("item-wrong"),
        balance("acct-chk", "depository", 50.0),
        auth_numbers("acct-chk", "000099998888", "011401533"),
        plaid_ok(r#"{"request_id":"r"}"#),
        // The payout account: matched.
        exchanged("item-right"),
        balance("acct-chk", "depository", 50.0),
        auth_numbers("acct-chk", "000011110000", "011401533"),
        plaid_ok(r#"{"request_id":"r"}"#),
        balance("acct-chk", "depository", 50.0),
    ]));
    let fake = Arc::new(FakeProcessor::new(FeeModel::STRIPE_US_CARD));
    let bank = BankState {
        link: Arc::new(Plaid::new(transport.clone(), "stringcan").unwrap()),
        keys: Arc::new(Keyring::new(TOKEN_KEY, "").unwrap()),
    };
    let Some((app, pool)) = build_full(
        false,
        None,
        Some(bank),
        Arc::new(RecordingNotifier::default()),
        fake.clone(),
    )
    .await
    else {
        return;
    };
    let (tenant, key) = new_tenant(&app, 1000).await;
    let link = |token: &'static str| {
        call(
            &app,
            "POST",
            "/v1/bank/link",
            Some(&key),
            Some(json!({"public_token": token, "account_id": "acct-chk"})),
        )
    };

    let (s, v) = link("public-sandbox-nc").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["payout_account"], "owner_declared");

    let (s, _) = call(&app, "POST", "/v1/connect/account", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK);
    let account: String =
        sqlx::query_scalar("SELECT stripe_account_id FROM tenants WHERE id = $1::uuid")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    fake.set_payout_banks(&account, vec![stripe_bank("011401533", "0000")]);

    let (s, v) = link("public-sandbox-wrong").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(transport.requests()[5].path, "/item/remove");
    let item: String =
        sqlx::query_scalar("SELECT item_id FROM bank_links WHERE tenant_id = $1::uuid")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        item, "item-nc",
        "a refused link leaves the earlier one in place"
    );

    let (s, v) = link("public-sandbox-right").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["payout_account"], "matched");
    assert_eq!(transport.requests()[8].path, "/auth/get");
    // The earlier item is revoked once the new one is stored.
    assert_eq!(transport.requests()[9].path, "/item/remove");

    let (s, _) = call(&app, "GET", "/v1/bank/float", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, ledger) = call(
        &app,
        "GET",
        &format!("/v1/public/{tenant}/ledger"),
        None,
        None,
    )
    .await;
    assert_eq!(ledger["float_check"]["payout_account"], "matched");
    let text = ledger.to_string();
    assert!(!text.contains("011401533") && !text.contains("0000111"));
    let stored: String = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM bank_links b WHERE tenant_id = $1::uuid",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!stored.contains("011401533") && !stored.contains("000011110000"));
}

/// Funds every account it is asked about; lets the re-check run over whatever
/// links other tests left in the shared database.
struct AlwaysFunded;

#[async_trait::async_trait]
impl stringcan_payments::BankLink for AlwaysFunded {
    async fn create_link_token(
        &self,
        _: &str,
        _: &[stringcan_payments::LinkProduct],
    ) -> Result<String, stringcan_payments::BankError> {
        Ok("link-sandbox-x".into())
    }
    async fn exchange_public_token(
        &self,
        _: &str,
    ) -> Result<stringcan_payments::LinkedItem, stringcan_payments::BankError> {
        Ok(stringcan_payments::LinkedItem {
            item_id: "item-z".into(),
            access_token: stringcan_payments::AccessToken::from_stored(ACCESS_TOKEN.into())?,
        })
    }
    async fn balances(
        &self,
        _: &stringcan_payments::AccessToken,
        ids: &[String],
    ) -> Result<Vec<stringcan_payments::BankBalance>, stringcan_payments::BankError> {
        Ok(ids
            .iter()
            .map(|id| stringcan_payments::BankBalance {
                account_id: id.clone(),
                depository: true,
                available: Some(100_000_000),
                current: Some(100_000_000),
                currency: Some("usd".into()),
            })
            .collect())
    }
    async fn remove_item(
        &self,
        _: &stringcan_payments::AccessToken,
    ) -> Result<(), stringcan_payments::BankError> {
        Ok(())
    }
    async fn ach_numbers(
        &self,
        _: &stringcan_payments::AccessToken,
        _: &[String],
    ) -> Result<Vec<stringcan_payments::AchNumbers>, stringcan_payments::BankError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn daily_recheck_checks_only_links_not_checked_recently() {
    let bank = BankState {
        link: Arc::new(AlwaysFunded),
        keys: Arc::new(Keyring::new(TOKEN_KEY, "").unwrap()),
    };
    let Some((app, pool)) = build_with_bank(false, None, Some(bank)).await else {
        return;
    };
    let (due, due_key) = new_tenant(&app, 1000).await;
    let (fresh, fresh_key) = new_tenant(&app, 1000).await;
    for k in [&due_key, &fresh_key] {
        let (s, v) = call(
            &app,
            "POST",
            "/v1/bank/link",
            Some(k),
            Some(json!({"public_token": "public-sandbox-z", "account_id": "acct-z"})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (_, v) = call(&app, "GET", "/v1/bank/float", Some(&fresh_key), None).await;
    assert_eq!(v["status"], "verified");
    sqlx::query("UPDATE bank_links SET last_result = 'short' WHERE tenant_id = $1::uuid")
        .bind(&fresh)
        .execute(&pool)
        .await
        .unwrap();

    let (s, _) = call(&app, "POST", "/v1/admin/bank/recheck", Some(&due_key), None).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "owners cannot trigger the sweep"
    );
    let (s, sum) = call(&app, "POST", "/v1/admin/bank/recheck", Some(ADMIN), None).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert!(sum["checked"].as_u64().unwrap() >= 1);

    let result = |t: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT last_result FROM bank_links WHERE tenant_id = $1::uuid",
            )
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(result(due).await.as_deref(), Some("verified"));
    assert_eq!(
        result(fresh).await.as_deref(),
        Some("short"),
        "checked within the day, so skipped"
    );
}
