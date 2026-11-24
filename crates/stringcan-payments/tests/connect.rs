//! Stripe Connect against fixtures. The JSON bodies are hand-written to match
//! Stripe's documented response shapes (no test key was available to record
//! live test-mode traffic); replace with recordings when one exists.

use stringcan_payments::{
    parse_event, sign_header, verify_signature, ChargeRequest, ConnectAccounts, FixtureTransport,
    Method, PaymentProcessor, ProcessorError, RefundRequest, StripeConnect, WebhookError,
};

const SUCCEEDED: &str = include_str!("fixtures/payment_intent_succeeded.json");
const DECLINED: &str = include_str!("fixtures/payment_intent_declined.json");
const REQUIRES_ACTION: &str = include_str!("fixtures/payment_intent_requires_action.json");
const ACCOUNT: &str = include_str!("fixtures/account_created.json");
const LINK: &str = include_str!("fixtures/account_link.json");
const ACCOUNT_READY: &str = include_str!("fixtures/account_ready.json");
const REFUND: &str = include_str!("fixtures/refund_succeeded.json");
const EVENT_ACCOUNT_UPDATED: &str = include_str!("fixtures/event_account_updated.json");

fn charge_req() -> ChargeRequest {
    ChargeRequest {
        idempotency_key: "t1:7:2".into(),
        gross: 1061,
        currency: "usd".into(),
        customer_ref: "alice".into(),
        description: "stringcan period 2".into(),
        connected_account: Some("acct_1TestOwner".into()),
        customer: Some("cus_TestSub".into()),
        payment_method: Some("pm_TestCard".into()),
    }
}

#[tokio::test]
async fn direct_charge_is_made_on_the_connected_account_and_records_the_real_fee() {
    let c = StripeConnect::new(FixtureTransport::new([FixtureTransport::ok(SUCCEEDED)]));
    let res = c.charge(&charge_req()).await.unwrap();
    assert_eq!(
        (res.processor_ref.as_str(), res.fee),
        ("pi_3TestSucceeded", 61)
    );
}

#[tokio::test]
async fn charge_request_shape_is_idempotent_direct_and_expands_the_fee() {
    let t = std::sync::Arc::new(FixtureTransport::new([FixtureTransport::ok(SUCCEEDED)]));
    let c = StripeConnect::new(t.clone());
    c.charge(&charge_req()).await.unwrap();
    let sent = &t.requests()[0];
    assert_eq!(
        (sent.method, sent.path.as_str()),
        (Method::Post, "/v1/payment_intents")
    );
    assert_eq!(sent.header("Stripe-Account"), Some("acct_1TestOwner"));
    assert_eq!(sent.header("Idempotency-Key"), Some("t1:7:2"));
    assert_eq!(sent.form_value("amount"), Some("1061"));
    assert_eq!(sent.form_value("confirm"), Some("true"));
    assert_eq!(sent.form_value("off_session"), Some("true"));
    assert_eq!(
        sent.form_value("expand[]"),
        Some("latest_charge.balance_transaction")
    );
    assert!(
        sent.form_value("application_fee_amount").is_none(),
        "no platform cut, ever"
    );
    assert!(
        sent.headers.iter().all(|(_, v)| !v.contains("sk_")),
        "credentials stay inside the transport"
    );
}

#[tokio::test]
async fn declines_actions_and_outages_map_to_distinct_errors() {
    let cases = [
        (
            FixtureTransport::status(402, DECLINED),
            ProcessorError::Declined("insufficient_funds".into()),
        ),
        (
            FixtureTransport::ok(REQUIRES_ACTION),
            ProcessorError::Declined("requires_action".into()),
        ),
        (
            FixtureTransport::status(429, r#"{"error":{"type":"rate_limit_error"}}"#),
            ProcessorError::Transient("status 429".into()),
        ),
        (
            FixtureTransport::status(503, "{}"),
            ProcessorError::Transient("status 503".into()),
        ),
        (
            FixtureTransport::status(
                403,
                r#"{"error":{"type":"invalid_request_error","code":"account_invalid"}}"#,
            ),
            ProcessorError::Rejected("account_invalid".into()),
        ),
        (
            FixtureTransport::ok("not json"),
            ProcessorError::Protocol("response is not JSON"),
        ),
    ];
    for (resp, want) in cases {
        let c = StripeConnect::new(FixtureTransport::new([resp]));
        assert_eq!(c.charge(&charge_req()).await.unwrap_err(), want);
    }
}

#[tokio::test]
async fn charge_inputs_are_validated_before_anything_is_sent() {
    let c = StripeConnect::new(FixtureTransport::new([]));
    for mutate in [
        (|r: &mut ChargeRequest| r.connected_account = None) as fn(&mut ChargeRequest),
        |r| r.connected_account = Some("acct_x/../y".into()),
        |r| r.customer = None,
        |r| r.payment_method = Some("card_123".into()),
        |r| r.currency = "USD".into(),
        |r| r.gross = 0,
    ] {
        let mut r = charge_req();
        mutate(&mut r);
        assert!(matches!(
            c.charge(&r).await,
            Err(ProcessorError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn onboarding_creates_a_standard_account_then_a_link() {
    let c = StripeConnect::new(FixtureTransport::new([
        FixtureTransport::ok(ACCOUNT),
        FixtureTransport::ok(LINK),
        FixtureTransport::ok(ACCOUNT_READY),
    ]));
    let acct = c.create_account("tenant-1", "t1").await.unwrap();
    assert_eq!(acct, "acct_1TestOwner");
    let url = c
        .onboarding_link(
            &acct,
            "https://app.example/refresh",
            "https://app.example/done",
        )
        .await
        .unwrap();
    assert!(url.starts_with("https://connect.stripe.com/"));
    let s = c.account_status(&acct).await.unwrap();
    assert!(s.charges_enabled && s.payouts_enabled && s.details_submitted);

    assert!(c
        .onboarding_link(&acct, "javascript:alert(1)", "https://x.example")
        .await
        .is_err());
    assert!(c.account_status("acct_../../v1/charges").await.is_err());
}

#[tokio::test]
async fn refunds_target_the_original_payment_on_the_connected_account() {
    let c = StripeConnect::new(FixtureTransport::new([FixtureTransport::ok(REFUND)]));
    let id = c
        .refund(&RefundRequest {
            idempotency_key: "r1".into(),
            amount: 300,
            customer_ref: "alice".into(),
            connected_account: Some("acct_1TestOwner".into()),
            charge_ref: Some("pi_3TestSucceeded".into()),
        })
        .await
        .unwrap();
    assert_eq!(id, "re_3TestRefund");
    assert!(c
        .refund(&RefundRequest {
            amount: 300,
            ..RefundRequest::default()
        })
        .await
        .is_err());
}

#[test]
fn webhook_signatures_accept_good_and_reject_bad_ones() {
    let body = EVENT_ACCOUNT_UPDATED.as_bytes();
    let secret = "whsec_testsecret";
    let now = 1_700_000_000;
    let good = sign_header(body, secret, now);
    assert_eq!(verify_signature(body, &good, secret, now + 10, 300), Ok(()));

    assert_eq!(
        verify_signature(body, &good, "whsec_other", now, 300),
        Err(WebhookError::NoValidSignature)
    );
    assert_eq!(
        verify_signature(b"{\"tampered\":1}", &good, secret, now, 300),
        Err(WebhookError::NoValidSignature)
    );
    assert_eq!(
        verify_signature(body, &good, secret, now + 301, 300),
        Err(WebhookError::TimestampOutOfTolerance)
    );
    assert_eq!(
        verify_signature(body, "garbage", secret, now, 300),
        Err(WebhookError::MalformedHeader)
    );
    assert_eq!(
        verify_signature(body, &format!("t={now}"), secret, now, 300),
        Err(WebhookError::MalformedHeader)
    );
    assert_eq!(
        verify_signature(body, &format!("t={now},v1=zz"), secret, now, 300),
        Err(WebhookError::NoValidSignature)
    );

    // Stripe may send several v1 values during secret rotation; any valid one is enough.
    let valid = good.split("v1=").nth(1).unwrap();
    let rotated = format!("t={now},v1={},v1={valid}", "00".repeat(32));
    assert_eq!(verify_signature(body, &rotated, secret, now, 300), Ok(()));
}

#[test]
fn events_parse_with_their_connected_account() {
    let e = parse_event(EVENT_ACCOUNT_UPDATED.as_bytes()).unwrap();
    assert_eq!(
        (e.id.as_str(), e.kind.as_str()),
        ("evt_1TestAccountUpdated", "account.updated")
    );
    assert_eq!(e.account.as_deref(), Some("acct_1TestOwner"));
    assert_eq!(e.object["charges_enabled"], true);
    assert_eq!(
        parse_event(b"{}").unwrap_err(),
        WebhookError::MalformedPayload
    );
}

const EXTERNAL: &str = include_str!("fixtures/external_accounts.json");
const EXTERNAL_FORBIDDEN: &str = include_str!("fixtures/external_accounts_forbidden.json");

#[tokio::test]
async fn payout_banks_list_the_connected_accounts_bank_accounts() {
    let t = std::sync::Arc::new(FixtureTransport::new([FixtureTransport::ok(EXTERNAL)]));
    let c = StripeConnect::new(t.clone());
    let banks = c.payout_banks("acct_1TestOwner").await.unwrap();
    assert_eq!(banks.len(), 2);
    assert!(banks[1].default_for_currency);
    assert_eq!(
        (banks[1].routing_number.as_str(), banks[1].last4.as_str()),
        ("011401533", "0000")
    );
    let sent = &t.requests()[0];
    assert_eq!(sent.method, Method::Get);
    assert!(sent
        .path
        .starts_with("/v1/accounts/acct_1TestOwner/external_accounts?object=bank_account"));
    assert!(c.payout_banks("not-an-account").await.is_err());
}

#[tokio::test]
async fn a_refused_external_account_listing_is_a_rejection_not_a_mismatch() {
    let c = StripeConnect::new(FixtureTransport::new([FixtureTransport::status(
        403,
        EXTERNAL_FORBIDDEN,
    )]));
    assert!(matches!(
        c.payout_banks("acct_1TestOwner").await,
        Err(ProcessorError::Rejected(_))
    ));
}

mod payout_match {
    use stringcan_payments::{match_payout_account, AchNumbers, PayoutBank, PayoutMatch};

    fn bank(routing: &str, last4: &str, currency: &str, default: bool) -> PayoutBank {
        PayoutBank {
            routing_number: routing.into(),
            last4: last4.into(),
            currency: currency.into(),
            default_for_currency: default,
        }
    }

    fn plaid(tokenized: bool) -> AchNumbers {
        AchNumbers {
            account_id: "acc".into(),
            routing: "011401533".into(),
            last4: "0000".into(),
            tokenized,
        }
    }

    #[test]
    fn matches_the_default_payout_account_in_the_tenant_currency() {
        let banks = [
            bank("011401533", "0000", "usd", true),
            bank("110000000", "6789", "usd", false),
        ];
        assert_eq!(
            match_payout_account(Some(&plaid(false)), &banks, "usd"),
            PayoutMatch::Matched
        );
    }

    #[test]
    fn a_non_default_account_does_not_count_when_a_default_exists() {
        let banks = [
            bank("110000000", "6789", "usd", true),
            bank("011401533", "0000", "usd", false),
        ];
        assert_eq!(
            match_payout_account(Some(&plaid(false)), &banks, "usd"),
            PayoutMatch::Mismatch
        );
    }

    #[test]
    fn without_a_flagged_default_any_account_in_the_currency_counts() {
        let banks = [bank("011401533", "0000", "usd", false)];
        assert_eq!(
            match_payout_account(Some(&plaid(false)), &banks, "usd"),
            PayoutMatch::Matched
        );
    }

    #[test]
    fn unknowable_cases_are_unknown_not_mismatch() {
        let banks = [bank("011401533", "0000", "usd", true)];
        assert!(matches!(
            match_payout_account(Some(&plaid(true)), &banks, "usd"),
            PayoutMatch::Unknown(_)
        ));
        assert!(matches!(
            match_payout_account(None, &banks, "usd"),
            PayoutMatch::Unknown(_)
        ));
        assert!(matches!(
            match_payout_account(Some(&plaid(false)), &banks, "eur"),
            PayoutMatch::Unknown(_)
        ));
        assert!(matches!(
            match_payout_account(Some(&plaid(false)), &[], "usd"),
            PayoutMatch::Unknown(_)
        ));
    }
}
