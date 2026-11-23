use stringcan_core::FeeModel;
use stringcan_payments::{
    ChargeRequest, FakeProcessor, PaymentProcessor, ProcessorError, StripeConfig,
    StripeTestProcessor,
};

fn req(key: &str, customer: &str, gross: i64) -> ChargeRequest {
    ChargeRequest {
        idempotency_key: key.into(),
        gross,
        currency: "usd".into(),
        customer_ref: customer.into(),
        description: "period 1".into(),
    }
}

#[tokio::test]
async fn fake_charges_with_model_fee_and_is_idempotent() {
    let p = FakeProcessor::new(FeeModel::STRIPE_US_CARD);
    let a = p.charge(&req("k1", "cus_1", 1000)).await.unwrap();
    let b = p.charge(&req("k1", "cus_1", 1000)).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(a.fee, FeeModel::STRIPE_US_CARD.fee(1000));
    assert_eq!((p.charges_created(), p.charge_calls()), (1, 2));
}

#[tokio::test]
async fn fake_declines_and_rejects_bad_amounts() {
    let p = FakeProcessor::new(FeeModel::ZERO);
    assert!(matches!(
        p.charge(&req("k", "cus_decline", 500)).await,
        Err(ProcessorError::Declined(_))
    ));
    assert!(matches!(
        p.charge(&req("k2", "cus_1", 0)).await,
        Err(ProcessorError::Invalid(_))
    ));
    assert_eq!(p.charges_created(), 0);
}

#[test]
fn stripe_config_refuses_live_and_malformed_keys() {
    assert_eq!(
        StripeConfig::new("sk_live_abcdefghijkl").unwrap_err(),
        ProcessorError::LiveKeyRefused
    );
    assert_eq!(
        StripeConfig::new("rk_test_abcdefghijkl").unwrap_err(),
        ProcessorError::LiveKeyRefused
    );
    assert_eq!(
        StripeConfig::new("sk_test_").unwrap_err(),
        ProcessorError::LiveKeyRefused
    );
    let ok = StripeConfig::new("sk_test_abcdefghijkl").unwrap();
    assert!(!format!("{ok:?}").contains("abcdefghijkl"));
}

#[tokio::test]
async fn stripe_stub_builds_the_request_but_never_sends() {
    let form = StripeTestProcessor::payment_intent_form(&req("k", "cus_1", 1234)).unwrap();
    assert!(form.contains(&("amount".into(), "1234".into())));
    let p = StripeTestProcessor::new(StripeConfig::new("sk_test_abcdefghijkl").unwrap());
    assert_eq!(
        p.charge(&req("k", "cus_1", 1234)).await,
        Err(ProcessorError::NotWired("stripe charge"))
    );
    let mut bad = req("k", "cus_1", 1234);
    bad.currency = "USD".into();
    assert!(StripeTestProcessor::payment_intent_form(&bad).is_err());
}
