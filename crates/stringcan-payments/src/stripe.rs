use std::fmt;

use async_trait::async_trait;
use serde_json::Value;

use crate::transport::{HttpRequest, HttpResponse, Method, Transport};
use crate::{
    AccountStatus, ChargeRequest, ChargeResult, ConnectAccounts, PaymentProcessor, ProcessorError,
    RefundRequest,
};

/// Holds a Stripe secret key. Construction refuses anything but a test-mode key,
/// so a live key can never reach this process by misconfiguration.
#[derive(Clone)]
pub struct StripeConfig {
    secret_key: String,
}

impl StripeConfig {
    pub fn new(secret_key: &str) -> Result<Self, ProcessorError> {
        if !secret_key.starts_with("sk_test_") || secret_key.len() < 12 {
            return Err(ProcessorError::LiveKeyRefused);
        }
        Ok(StripeConfig {
            secret_key: secret_key.to_owned(),
        })
    }

    pub fn from_env() -> Result<Self, ProcessorError> {
        let key = std::env::var("STRIPE_SECRET_KEY")
            .map_err(|_| ProcessorError::Invalid("STRIPE_SECRET_KEY not set"))?;
        Self::new(&key)
    }

    #[cfg_attr(not(feature = "http"), allow(dead_code))]
    pub(crate) fn secret(&self) -> &str {
        &self.secret_key
    }
}

impl fmt::Debug for StripeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StripeConfig")
            .field("secret_key", &"sk_test_***")
            .finish()
    }
}

/// Stripe Connect client: Standard connected accounts, direct charges.
/// See docs/DESIGN.md section 20.
pub struct StripeConnect<T: Transport> {
    transport: T,
}

fn valid_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        (4..=64).contains(&rest.len())
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn valid_url(u: &str) -> bool {
    let ok_scheme = u.starts_with("https://")
        || u.starts_with("http://localhost")
        || u.starts_with("http://127.0.0.1");
    ok_scheme && u.len() <= 2048 && u.chars().all(|c| c.is_ascii_graphic())
}

fn form(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn header(k: &str, v: &str) -> (String, String) {
    (k.to_owned(), v.to_owned())
}

/// Maps a Stripe error response onto our error type; `Ok` for 2xx.
fn parse_body(resp: &HttpResponse) -> Result<Value, ProcessorError> {
    let body: Value = serde_json::from_str(&resp.body)
        .map_err(|_| ProcessorError::Protocol("response is not JSON"))?;
    if (200..300).contains(&resp.status) {
        return Ok(body);
    }
    let err = body.get("error");
    let text = |k: &str| err.and_then(|e| e.get(k)).and_then(Value::as_str);
    let code = text("decline_code")
        .or_else(|| text("code"))
        .unwrap_or("unknown")
        .to_owned();
    match resp.status {
        402 => Err(ProcessorError::Declined(code)),
        _ if text("type") == Some("card_error") => Err(ProcessorError::Declined(code)),
        429 | 500..=599 => Err(ProcessorError::Transient(format!("status {}", resp.status))),
        _ => Err(ProcessorError::Rejected(code)),
    }
}

fn str_field(v: &Value, key: &str) -> Result<String, ProcessorError> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ProcessorError::Protocol("missing field"))
}

impl<T: Transport> StripeConnect<T> {
    pub fn new(transport: T) -> Self {
        StripeConnect { transport }
    }

    async fn call(&self, req: HttpRequest) -> Result<Value, ProcessorError> {
        parse_body(&self.transport.send(req).await?)
    }
}

#[async_trait]
impl<T: Transport> ConnectAccounts for StripeConnect<T> {
    async fn create_account(
        &self,
        idempotency_key: &str,
        tenant_ref: &str,
    ) -> Result<String, ProcessorError> {
        let v = self
            .call(HttpRequest {
                method: Method::Post,
                path: "/v1/accounts".into(),
                form: form(&[("type", "standard"), ("metadata[tenant_ref]", tenant_ref)]),
                headers: vec![header("Idempotency-Key", idempotency_key)],
            })
            .await?;
        let id = str_field(&v, "id")?;
        if valid_id(&id, "acct_") {
            Ok(id)
        } else {
            Err(ProcessorError::Protocol("unexpected account id"))
        }
    }

    async fn onboarding_link(
        &self,
        account: &str,
        refresh_url: &str,
        return_url: &str,
    ) -> Result<String, ProcessorError> {
        if !valid_id(account, "acct_") {
            return Err(ProcessorError::Invalid("bad account id"));
        }
        if !valid_url(refresh_url) || !valid_url(return_url) {
            return Err(ProcessorError::Invalid("urls must be https (or localhost)"));
        }
        let v = self
            .call(HttpRequest {
                method: Method::Post,
                path: "/v1/account_links".into(),
                form: form(&[
                    ("account", account),
                    ("refresh_url", refresh_url),
                    ("return_url", return_url),
                    ("type", "account_onboarding"),
                ]),
                headers: vec![],
            })
            .await?;
        str_field(&v, "url")
    }

    async fn account_status(&self, account: &str) -> Result<AccountStatus, ProcessorError> {
        if !valid_id(account, "acct_") {
            return Err(ProcessorError::Invalid("bad account id"));
        }
        let v = self
            .call(HttpRequest {
                method: Method::Get,
                path: format!("/v1/accounts/{account}"),
                form: vec![],
                headers: vec![],
            })
            .await?;
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(AccountStatus {
            charges_enabled: flag("charges_enabled"),
            payouts_enabled: flag("payouts_enabled"),
            details_submitted: flag("details_submitted"),
        })
    }
}

#[async_trait]
impl<T: Transport> PaymentProcessor for StripeConnect<T> {
    async fn charge(&self, req: &ChargeRequest) -> Result<ChargeResult, ProcessorError> {
        if req.gross <= 0 {
            return Err(ProcessorError::Invalid("amount must be positive"));
        }
        if req.currency.len() != 3 || !req.currency.chars().all(|c| c.is_ascii_lowercase()) {
            return Err(ProcessorError::Invalid(
                "currency must be a 3-letter lowercase code",
            ));
        }
        let account = req
            .connected_account
            .as_deref()
            .filter(|a| valid_id(a, "acct_"));
        let account = account.ok_or(ProcessorError::Invalid("tenant has no connected account"))?;
        let customer = req.customer.as_deref().filter(|c| valid_id(c, "cus_"));
        let method = req.payment_method.as_deref().filter(|m| valid_id(m, "pm_"));
        let (Some(customer), Some(method)) = (customer, method) else {
            return Err(ProcessorError::Invalid(
                "subscriber has no payment method on file",
            ));
        };
        let amount = req.gross.to_string();
        let v = self
            .call(HttpRequest {
                method: Method::Post,
                path: "/v1/payment_intents".into(),
                form: form(&[
                    ("amount", &amount),
                    ("currency", &req.currency),
                    ("customer", customer),
                    ("payment_method", method),
                    ("confirm", "true"),
                    ("off_session", "true"),
                    ("description", &req.description),
                    ("metadata[customer_ref]", &req.customer_ref),
                    ("expand[]", "latest_charge.balance_transaction"),
                ]),
                headers: vec![
                    header("Stripe-Account", account),
                    header("Idempotency-Key", &req.idempotency_key),
                ],
            })
            .await?;
        match v.get("status").and_then(Value::as_str) {
            Some("succeeded") => {
                let fee = v
                    .pointer("/latest_charge/balance_transaction/fee")
                    .and_then(Value::as_i64)
                    .ok_or(ProcessorError::Protocol(
                        "fee missing from balance transaction",
                    ))?;
                Ok(ChargeResult {
                    processor_ref: str_field(&v, "id")?,
                    fee,
                })
            }
            Some(other) => Err(ProcessorError::Declined(other.to_owned())),
            None => Err(ProcessorError::Protocol("payment intent has no status")),
        }
    }

    async fn refund(&self, req: &RefundRequest) -> Result<String, ProcessorError> {
        if req.amount <= 0 {
            return Err(ProcessorError::Invalid("amount must be positive"));
        }
        let account = req
            .connected_account
            .as_deref()
            .filter(|a| valid_id(a, "acct_"));
        let account = account.ok_or(ProcessorError::Invalid("tenant has no connected account"))?;
        let charge = req.charge_ref.as_deref().filter(|c| valid_id(c, "pi_"));
        let charge = charge.ok_or(ProcessorError::Invalid("no charge to refund against"))?;
        let amount = req.amount.to_string();
        let v = self
            .call(HttpRequest {
                method: Method::Post,
                path: "/v1/refunds".into(),
                form: form(&[("payment_intent", charge), ("amount", &amount)]),
                headers: vec![
                    header("Stripe-Account", account),
                    header("Idempotency-Key", &req.idempotency_key),
                ],
            })
            .await?;
        match v.get("status").and_then(Value::as_str) {
            Some("succeeded" | "pending") => str_field(&v, "id"),
            _ => Err(ProcessorError::Rejected("refund did not succeed".into())),
        }
    }
}
