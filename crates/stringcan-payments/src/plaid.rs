//! Plaid, used only to read the balance of the owner's own bank account so the
//! required float (DESIGN section 20.3) can be published as verified or not.
//! Nothing here moves money. See DESIGN section 20.4.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Value};
use stringcan_core::{Cents, MAX_CENTS};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BankError {
    /// The owner must re-authenticate with their bank through Link update mode.
    #[error("bank link needs the owner to log in again")]
    ReauthRequired,
    #[error("bank data provider rejected the request: {0}")]
    Rejected(String),
    #[error("bank data provider temporarily unavailable: {0}")]
    Transient(String),
    #[error("unexpected bank data response: {0}")]
    Protocol(&'static str),
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("refusing non-sandbox Plaid environment")]
    NonSandboxRefused,
}

/// Plaid credentials. Only the sandbox environment is accepted; the secret
/// alone cannot tell environments apart, so the environment is checked
/// explicitly and the live transport is hard-wired to the sandbox host.
#[derive(Clone)]
pub struct PlaidConfig {
    client_id: String,
    secret: String,
}

fn opaque(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl PlaidConfig {
    pub fn new(environment: &str, client_id: &str, secret: &str) -> Result<Self, BankError> {
        if environment != "sandbox" {
            return Err(BankError::NonSandboxRefused);
        }
        if !opaque(client_id, 64) || !opaque(secret, 128) {
            return Err(BankError::Invalid("malformed Plaid credentials"));
        }
        Ok(PlaidConfig {
            client_id: client_id.to_owned(),
            secret: secret.to_owned(),
        })
    }

    pub fn from_env() -> Result<Self, BankError> {
        let var = |k: &'static str| std::env::var(k).map_err(|_| BankError::Invalid(k));
        Self::new(
            &var("PLAID_ENV")?,
            &var("PLAID_CLIENT_ID")?,
            &var("PLAID_SECRET")?,
        )
    }

    #[cfg_attr(not(feature = "http"), allow(dead_code))]
    pub(crate) fn credentials(&self) -> (&str, &str) {
        (&self.client_id, &self.secret)
    }
}

impl fmt::Debug for PlaidConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlaidConfig")
            .field("client_id", &"***")
            .field("secret", &"***")
            .finish()
    }
}

/// A Plaid access token is a long-lived bearer credential to the owner's bank
/// data, not an opaque id: no `Display`, redacted `Debug`, and reading it out
/// is an explicit call so every place that persists it is greppable.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn from_stored(token: String) -> Result<Self, BankError> {
        if opaque(&token, 256) {
            Ok(AccessToken(token))
        } else {
            Err(BankError::Invalid("malformed access token"))
        }
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(***)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedItem {
    pub item_id: String,
    pub access_token: AccessToken,
}

/// Plaid rejects `balance` as a Link product; balance comes with whichever
/// product is initialised. Which one stringcan uses is an open owner question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkProduct {
    Auth,
    Transactions,
}

impl LinkProduct {
    fn as_str(self) -> &'static str {
        match self {
            LinkProduct::Auth => "auth",
            LinkProduct::Transactions => "transactions",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankBalance {
    pub account_id: String,
    pub depository: bool,
    pub available: Option<Cents>,
    pub current: Option<Cents>,
    /// ISO code, lowercased to match `ChargeRequest::currency`. `None` when
    /// Plaid reports only an unofficial currency.
    pub currency: Option<String>,
}

#[async_trait]
pub trait BankLink: Send + Sync {
    /// Short-lived token the owner's browser hands to Plaid Link.
    async fn create_link_token(
        &self,
        client_user_id: &str,
        products: &[LinkProduct],
    ) -> Result<String, BankError>;
    async fn exchange_public_token(&self, public_token: &str) -> Result<LinkedItem, BankError>;
    /// Real-time balances. An empty `account_ids` means every account on the item.
    async fn balances(
        &self,
        token: &AccessToken,
        account_ids: &[String],
    ) -> Result<Vec<BankBalance>, BankError>;
    /// Revokes the access token at Plaid. Call when an owner unlinks.
    async fn remove_item(&self, token: &AccessToken) -> Result<(), BankError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct JsonRequest {
    pub path: String,
    /// Never carries `client_id` or `secret`; the transport adds them as headers.
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonResponse {
    pub status: u16,
    pub body: String,
}

#[async_trait]
pub trait JsonTransport: Send + Sync {
    async fn send(&self, req: JsonRequest) -> Result<JsonResponse, BankError>;
}

#[async_trait]
impl<T: JsonTransport + ?Sized> JsonTransport for std::sync::Arc<T> {
    async fn send(&self, req: JsonRequest) -> Result<JsonResponse, BankError> {
        (**self).send(req).await
    }
}

#[derive(Default)]
pub struct JsonFixtureTransport {
    responses: Mutex<VecDeque<JsonResponse>>,
    sent: Mutex<Vec<JsonRequest>>,
}

impl JsonFixtureTransport {
    pub fn new(responses: impl IntoIterator<Item = JsonResponse>) -> Self {
        JsonFixtureTransport {
            responses: Mutex::new(responses.into_iter().collect()),
            sent: Mutex::default(),
        }
    }

    pub fn ok(body: &str) -> JsonResponse {
        Self::status(200, body)
    }

    pub fn status(status: u16, body: &str) -> JsonResponse {
        JsonResponse {
            status,
            body: body.to_owned(),
        }
    }

    pub fn requests(&self) -> Vec<JsonRequest> {
        self.sent.lock().expect("fixture lock").clone()
    }
}

#[async_trait]
impl JsonTransport for JsonFixtureTransport {
    async fn send(&self, req: JsonRequest) -> Result<JsonResponse, BankError> {
        self.sent.lock().expect("fixture lock").push(req);
        self.responses
            .lock()
            .expect("fixture lock")
            .pop_front()
            .ok_or(BankError::Invalid("no fixture response queued"))
    }
}

pub struct Plaid<T: JsonTransport> {
    transport: T,
    client_name: String,
}

/// Only `error_type` and `error_code` are kept: Plaid's message fields are
/// free text that can echo request details, so they never reach logs.
fn parse_body(resp: &JsonResponse) -> Result<Value, BankError> {
    let body: Value = serde_json::from_str(&resp.body)
        .map_err(|_| BankError::Protocol("response is not JSON"))?;
    if (200..300).contains(&resp.status) {
        return Ok(body);
    }
    let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("unknown");
    let (kind, code) = (text("error_type"), text("error_code"));
    match (kind, code) {
        (_, "ITEM_LOGIN_REQUIRED") => Err(BankError::ReauthRequired),
        ("RATE_LIMIT_EXCEEDED" | "API_ERROR" | "INSTITUTION_ERROR", _) => {
            Err(BankError::Transient(code.to_owned()))
        }
        _ if resp.status == 429 || resp.status >= 500 => Err(BankError::Transient(code.to_owned())),
        _ => Err(BankError::Rejected(code.to_owned())),
    }
}

fn str_field(v: &Value, key: &str) -> Result<String, BankError> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(BankError::Protocol("missing field"))
}

/// Plaid reports balances as JSON numbers in major units.
fn to_cents(v: Option<&Value>) -> Result<Option<Cents>, BankError> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let major = v
        .as_f64()
        .ok_or(BankError::Protocol("balance is not a number"))?;
    let cents = (major * 100.0).round();
    if !cents.is_finite() || cents.abs() > MAX_CENTS as f64 {
        return Err(BankError::Protocol("balance out of range"));
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(Some(cents as Cents))
}

impl<T: JsonTransport> Plaid<T> {
    pub fn new(transport: T, client_name: &str) -> Result<Self, BankError> {
        // Plaid's documented limit for `client_name`.
        if client_name.is_empty() || client_name.chars().count() > 30 {
            return Err(BankError::Invalid("client name must be 1 to 30 characters"));
        }
        Ok(Plaid {
            transport,
            client_name: client_name.to_owned(),
        })
    }

    async fn call(&self, path: &str, body: Value) -> Result<Value, BankError> {
        parse_body(
            &self
                .transport
                .send(JsonRequest {
                    path: path.to_owned(),
                    body,
                })
                .await?,
        )
    }
}

#[async_trait]
impl<T: JsonTransport> BankLink for Plaid<T> {
    async fn create_link_token(
        &self,
        client_user_id: &str,
        products: &[LinkProduct],
    ) -> Result<String, BankError> {
        // Plaid asks that this id carry no personal data; stringcan passes a tenant ref.
        if !opaque(client_user_id, 64) {
            return Err(BankError::Invalid("client_user_id must be an opaque ref"));
        }
        if products.is_empty() {
            return Err(BankError::Invalid("at least one product is required"));
        }
        let products: Vec<&str> = products.iter().map(|p| p.as_str()).collect();
        let v = self
            .call(
                "/link/token/create",
                json!({
                    "client_name": self.client_name,
                    "language": "en",
                    "country_codes": ["US"],
                    "user": { "client_user_id": client_user_id },
                    "products": products,
                }),
            )
            .await?;
        str_field(&v, "link_token")
    }

    async fn exchange_public_token(&self, public_token: &str) -> Result<LinkedItem, BankError> {
        if !opaque(public_token, 256) {
            return Err(BankError::Invalid("malformed public token"));
        }
        let v = self
            .call(
                "/item/public_token/exchange",
                json!({ "public_token": public_token }),
            )
            .await?;
        let item_id = str_field(&v, "item_id")?;
        if !opaque(&item_id, 128) {
            return Err(BankError::Protocol("unexpected item id"));
        }
        let access_token = AccessToken::from_stored(str_field(&v, "access_token")?)
            .map_err(|_| BankError::Protocol("unexpected access token"))?;
        Ok(LinkedItem {
            item_id,
            access_token,
        })
    }

    async fn balances(
        &self,
        token: &AccessToken,
        account_ids: &[String],
    ) -> Result<Vec<BankBalance>, BankError> {
        if account_ids.iter().any(|a| !opaque(a, 128)) {
            return Err(BankError::Invalid("malformed account id"));
        }
        let mut body = json!({ "access_token": token.expose_secret() });
        if !account_ids.is_empty() {
            body["options"] = json!({ "account_ids": account_ids });
        }
        let v = self.call("/accounts/balance/get", body).await?;
        let accounts = v
            .get("accounts")
            .and_then(Value::as_array)
            .ok_or(BankError::Protocol("missing accounts"))?;
        accounts
            .iter()
            .map(|a| {
                let b = a
                    .get("balances")
                    .ok_or(BankError::Protocol("missing balances"))?;
                Ok(BankBalance {
                    account_id: str_field(a, "account_id")?,
                    depository: a.get("type").and_then(Value::as_str) == Some("depository"),
                    available: to_cents(b.get("available"))?,
                    current: to_cents(b.get("current"))?,
                    currency: b
                        .get("iso_currency_code")
                        .and_then(Value::as_str)
                        .map(str::to_ascii_lowercase),
                })
            })
            .collect()
    }

    async fn remove_item(&self, token: &AccessToken) -> Result<(), BankError> {
        self.call(
            "/item/remove",
            json!({ "access_token": token.expose_secret() }),
        )
        .await
        .map(|_| ())
    }
}

/// Deliberately carries no amounts: only pass/fail is ever published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatCheck {
    Verified,
    Short,
    Unverifiable(&'static str),
}

/// Whether the given accounts together hold at least `required`.
/// PROPOSED DEFAULT (DESIGN 20.4): each account counts its `available`
/// balance, falling back to `current` only when the bank reports no
/// `available`; non-depository accounts (credit, loan, investment) never count.
pub fn verify_float(balances: &[BankBalance], required: Cents, currency: &str) -> FloatCheck {
    let counted: Vec<&BankBalance> = balances.iter().filter(|b| b.depository).collect();
    if counted.is_empty() {
        return FloatCheck::Unverifiable("no deposit account linked");
    }
    let mut total: Cents = 0;
    for b in counted {
        if b.currency.as_deref() != Some(currency) {
            return FloatCheck::Unverifiable("account currency differs from tenant currency");
        }
        let Some(amount) = b.available.or(b.current) else {
            return FloatCheck::Unverifiable("bank reported no balance");
        };
        total = total.saturating_add(amount);
    }
    if total >= required {
        FloatCheck::Verified
    } else {
        FloatCheck::Short
    }
}

#[cfg(feature = "http")]
mod live {
    use std::time::Duration;

    use super::{async_trait, BankError, JsonRequest, JsonResponse, JsonTransport, PlaidConfig};

    // Hard-wired: switching to production is a code change the owner reviews,
    // not a config value.
    const BASE: &str = "https://sandbox.plaid.com";

    pub struct PlaidSandboxTransport {
        client: reqwest::Client,
        config: PlaidConfig,
    }

    impl PlaidSandboxTransport {
        pub fn new(config: PlaidConfig) -> Result<Self, BankError> {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| BankError::Invalid("http client"))?;
            Ok(PlaidSandboxTransport { client, config })
        }
    }

    #[async_trait]
    impl JsonTransport for PlaidSandboxTransport {
        async fn send(&self, req: JsonRequest) -> Result<JsonResponse, BankError> {
            let (id, secret) = self.config.credentials();
            let resp = self
                .client
                .post(format!("{BASE}{}", req.path))
                .header("PLAID-CLIENT-ID", id)
                .header("PLAID-SECRET", secret)
                .header("Content-Type", "application/json")
                .body(req.body.to_string())
                .send()
                .await
                .map_err(|e| BankError::Transient(e.without_url().to_string()))?;
            let status = resp.status().as_u16();
            let body = resp
                .text()
                .await
                .map_err(|e| BankError::Transient(e.without_url().to_string()))?;
            Ok(JsonResponse { status, body })
        }
    }
}

#[cfg(feature = "http")]
pub use live::PlaidSandboxTransport;
