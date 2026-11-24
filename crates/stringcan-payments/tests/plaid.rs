//! Plaid against fixtures. The JSON bodies are hand-written to Plaid's
//! documented sandbox shapes (no sandbox keys were available); replace with
//! recorded sandbox responses once the owner provides them.

use std::sync::Arc;

use serde_json::json;
use stringcan_payments::{
    verify_float, AccessToken, BankBalance, BankError, BankLink, FloatCheck, JsonFixtureTransport,
    LinkProduct, Plaid, PlaidConfig,
};

const LINK_TOKEN: &str = include_str!("fixtures/plaid_link_token.json");
const EXCHANGE: &str = include_str!("fixtures/plaid_exchange.json");
const BALANCES: &str = include_str!("fixtures/plaid_balances.json");
const REMOVE: &str = include_str!("fixtures/plaid_item_remove.json");
const LOGIN_REQUIRED: &str = include_str!("fixtures/plaid_error_login_required.json");
const INVALID_TOKEN: &str = include_str!("fixtures/plaid_error_invalid_token.json");
const RATE_LIMIT: &str = include_str!("fixtures/plaid_error_rate_limit.json");

fn plaid(
    responses: Vec<stringcan_payments::JsonResponse>,
) -> (Plaid<Arc<JsonFixtureTransport>>, Arc<JsonFixtureTransport>) {
    let t = Arc::new(JsonFixtureTransport::new(responses));
    (Plaid::new(t.clone(), "stringcan").unwrap(), t)
}

fn token() -> AccessToken {
    AccessToken::from_stored("access-sandbox-de3ce8ef-33f8-452c-a685-8671031fc0f6".into()).unwrap()
}

#[test]
fn config_refuses_anything_but_sandbox() {
    for env in ["production", "development", "Sandbox", ""] {
        assert_eq!(
            PlaidConfig::new(env, "abc123", "def456").unwrap_err(),
            BankError::NonSandboxRefused
        );
    }
    assert!(PlaidConfig::new("sandbox", "abc123", "def456").is_ok());
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let c = PlaidConfig::new("sandbox", "client123", "secret456").unwrap();
    let dbg = format!("{c:?}");
    assert!(!dbg.contains("client123") && !dbg.contains("secret456"));
    assert!(!format!("{:?}", token()).contains("de3ce8ef"));
}

#[tokio::test]
async fn link_token_request_carries_no_credentials_and_an_opaque_user() {
    let (p, t) = plaid(vec![JsonFixtureTransport::ok(LINK_TOKEN)]);
    let lt = p
        .create_link_token("tenant_7", &[LinkProduct::Auth])
        .await
        .unwrap();
    assert!(lt.starts_with("link-sandbox-"));
    let sent = &t.requests()[0];
    assert_eq!(sent.path, "/link/token/create");
    assert_eq!(sent.body["user"]["client_user_id"], "tenant_7");
    assert_eq!(sent.body["products"], json!(["auth"]));
    assert!(sent.body.get("client_id").is_none() && sent.body.get("secret").is_none());
}

#[tokio::test]
async fn link_token_rejects_personal_data_as_user_id_and_empty_products() {
    let (p, t) = plaid(vec![]);
    assert!(matches!(
        p.create_link_token("owner@example.com", &[LinkProduct::Auth])
            .await,
        Err(BankError::Invalid(_))
    ));
    assert!(matches!(
        p.create_link_token("tenant_7", &[]).await,
        Err(BankError::Invalid(_))
    ));
    assert_eq!(t.requests().len(), 0);
}

#[tokio::test]
async fn exchange_returns_item_and_wrapped_token() {
    let (p, _) = plaid(vec![JsonFixtureTransport::ok(EXCHANGE)]);
    let item = p
        .exchange_public_token("public-sandbox-5c224a01-8314-4491-a06f-39e193d5cddc")
        .await
        .unwrap();
    assert_eq!(item.item_id, "M5eVJqLnv3tbzdngLDp9FL5OlDNxlNhlE55op");
    assert_eq!(item.access_token, token());
}

#[tokio::test]
async fn invalid_public_token_is_rejected_with_code_only() {
    let (p, _) = plaid(vec![JsonFixtureTransport::status(400, INVALID_TOKEN)]);
    let err = p
        .exchange_public_token("public-sandbox-bad")
        .await
        .unwrap_err();
    assert_eq!(err, BankError::Rejected("INVALID_PUBLIC_TOKEN".into()));
    assert!(!err.to_string().contains("expected format"));
}

#[tokio::test]
async fn balances_parse_to_cents_and_mark_depository_accounts() {
    let (p, t) = plaid(vec![JsonFixtureTransport::ok(BALANCES)]);
    let b = p.balances(&token(), &[]).await.unwrap();
    assert_eq!(b.len(), 3);
    assert_eq!((b[0].available, b[0].current), (Some(10_000), Some(11_000)));
    assert_eq!((b[1].available, b[1].current), (None, Some(21_047)));
    assert!(b[0].depository && b[1].depository && !b[2].depository);
    assert_eq!(b[0].currency.as_deref(), Some("usd"));
    assert!(t.requests()[0].body.get("options").is_none());
}

#[tokio::test]
async fn balances_can_be_limited_to_chosen_accounts() {
    let (p, t) = plaid(vec![JsonFixtureTransport::ok(BALANCES)]);
    let ids = vec!["BxBXxLj1m4HMXBm9WZZmCWVbPjX16EHwv99vp".to_owned()];
    p.balances(&token(), &ids).await.unwrap();
    let sent = &t.requests()[0];
    assert_eq!(sent.path, "/accounts/balance/get");
    assert_eq!(sent.body["options"]["account_ids"], json!(ids));
}

#[tokio::test]
async fn expired_login_maps_to_reauth_and_rate_limit_to_transient() {
    let (p, _) = plaid(vec![
        JsonFixtureTransport::status(400, LOGIN_REQUIRED),
        JsonFixtureTransport::status(429, RATE_LIMIT),
        JsonFixtureTransport::status(502, "not json"),
    ]);
    assert_eq!(
        p.balances(&token(), &[]).await,
        Err(BankError::ReauthRequired)
    );
    assert_eq!(
        p.balances(&token(), &[]).await,
        Err(BankError::Transient("BALANCE_LIMIT".into()))
    );
    assert!(matches!(
        p.balances(&token(), &[]).await,
        Err(BankError::Protocol(_))
    ));
}

#[tokio::test]
async fn remove_item_revokes_the_token() {
    let (p, t) = plaid(vec![JsonFixtureTransport::ok(REMOVE)]);
    p.remove_item(&token()).await.unwrap();
    assert_eq!(t.requests()[0].path, "/item/remove");
}

fn acct(available: Option<i64>, current: Option<i64>, depository: bool) -> BankBalance {
    BankBalance {
        account_id: "acc".into(),
        depository,
        available,
        current,
        currency: Some("usd".into()),
    }
}

#[test]
fn float_uses_available_then_current_and_ignores_credit_accounts() {
    let b = [
        acct(Some(10_000), Some(11_000), true),
        acct(None, Some(21_047), true),
        acct(None, Some(1_000_000), false),
    ];
    assert_eq!(verify_float(&b, 31_047, "usd"), FloatCheck::Verified);
    assert_eq!(verify_float(&b, 31_048, "usd"), FloatCheck::Short);
}

#[test]
fn float_is_unverifiable_rather_than_guessed() {
    assert!(matches!(
        verify_float(&[acct(None, Some(5), false)], 1, "usd"),
        FloatCheck::Unverifiable(_)
    ));
    assert!(matches!(
        verify_float(&[acct(None, None, true)], 1, "usd"),
        FloatCheck::Unverifiable(_)
    ));
    assert!(matches!(
        verify_float(&[acct(Some(500), None, true)], 1, "eur"),
        FloatCheck::Unverifiable(_)
    ));
}

#[test]
fn overdrawn_account_counts_against_the_float() {
    let b = [
        acct(Some(-2_000), None, true),
        acct(Some(5_000), None, true),
    ];
    assert_eq!(verify_float(&b, 3_000, "usd"), FloatCheck::Verified);
    assert_eq!(verify_float(&b, 3_001, "usd"), FloatCheck::Short);
}

const AUTH: &str = include_str!("fixtures/plaid_auth.json");
const AUTH_TOKENIZED: &str = include_str!("fixtures/plaid_auth_tokenized.json");

#[tokio::test]
async fn ach_numbers_keep_only_routing_and_last_four() {
    let (p, t) = plaid(vec![JsonFixtureTransport::ok(AUTH)]);
    let acct = "BxBXxLj1m4HMXBm9WZZmCWVbPjX16EHwv99vp".to_owned();
    let nums = p
        .ach_numbers(&token(), std::slice::from_ref(&acct))
        .await
        .unwrap();
    assert_eq!(nums.len(), 1);
    assert_eq!(
        (
            nums[0].routing.as_str(),
            nums[0].last4.as_str(),
            nums[0].tokenized
        ),
        ("011401533", "0000", false)
    );
    let dbg = format!("{nums:?}");
    assert!(!dbg.contains("1111222233330000") && !dbg.contains("011401533"));
    let sent = &t.requests()[0];
    assert_eq!(sent.path, "/auth/get");
    assert_eq!(sent.body["options"]["account_ids"], json!([acct]));
}

#[tokio::test]
async fn tokenized_account_numbers_are_flagged() {
    let (p, _) = plaid(vec![JsonFixtureTransport::ok(AUTH_TOKENIZED)]);
    let nums = p.ach_numbers(&token(), &[]).await.unwrap();
    assert!(nums[0].tokenized);
}
