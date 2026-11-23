use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Postgres, Transaction};
use stringcan_payments::{parse_event, verify_signature};
use uuid::Uuid;

use crate::auth::Owner;
use crate::error::ApiError;
use crate::{store, AppState};

const WEBHOOK_TOLERANCE_SECS: i64 = 300;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/connect/account", post(create_account))
        .route("/v1/connect/onboarding-link", post(onboarding_link))
        .route("/v1/connect/status", get(status))
        .route(
            "/v1/subscribers/{external_ref}/payment-method",
            put(set_payment_method),
        )
        .route("/v1/webhooks/stripe", post(stripe_webhook))
}

/// Lock the tenant row (same lock the engine writers use) and return its Stripe account id.
async fn locked_account(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
) -> Result<Option<String>, ApiError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT stripe_account_id FROM tenants WHERE id = $1 FOR UPDATE")
            .bind(tenant)
            .fetch_optional(&mut **tx)
            .await?;
    row.ok_or(ApiError::NotFound)
}

#[derive(Serialize)]
struct AccountBody {
    account_id: String,
}

async fn create_account(
    Owner(tenant): Owner,
    State(st): State<AppState>,
) -> Result<Json<AccountBody>, ApiError> {
    let mut tx = st.pool.begin().await?;
    if let Some(account_id) = locked_account(&mut tx, tenant).await? {
        return Ok(Json(AccountBody { account_id }));
    }
    let account_id = st
        .connect
        .create_account(&format!("tenant-account:{tenant}"), &tenant.to_string())
        .await?;
    sqlx::query("UPDATE tenants SET stripe_account_id = $2 WHERE id = $1")
        .bind(tenant)
        .bind(&account_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(AccountBody { account_id }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkBody {
    refresh_url: String,
    return_url: String,
}

#[derive(Serialize)]
struct LinkResponse {
    url: String,
}

async fn onboarding_link(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Json(b): Json<LinkBody>,
) -> Result<Json<LinkResponse>, ApiError> {
    let account: Option<String> =
        sqlx::query_scalar("SELECT stripe_account_id FROM tenants WHERE id = $1")
            .bind(tenant)
            .fetch_one(&st.pool)
            .await?;
    let account =
        account.ok_or_else(|| ApiError::Conflict("create the connected account first".into()))?;
    let url = st
        .connect
        .onboarding_link(&account, &b.refresh_url, &b.return_url)
        .await?;
    Ok(Json(LinkResponse { url }))
}

#[derive(Serialize)]
pub struct StatusBody {
    account_id: Option<String>,
    charges_enabled: bool,
    payouts_enabled: bool,
    details_submitted: bool,
}

#[derive(Deserialize)]
struct StatusParams {
    #[serde(default)]
    refresh: bool,
}

async fn read_status(conn: &mut PgConnection, tenant: Uuid) -> Result<StatusBody, ApiError> {
    let (account_id, c, p, d): (Option<String>, bool, bool, bool) = sqlx::query_as(
        "SELECT stripe_account_id, stripe_charges_enabled, stripe_payouts_enabled, stripe_details_submitted
         FROM tenants WHERE id = $1",
    )
    .bind(tenant)
    .fetch_one(conn)
    .await?;
    Ok(StatusBody {
        account_id,
        charges_enabled: c,
        payouts_enabled: p,
        details_submitted: d,
    })
}

/// Webhooks are the source of truth; `?refresh=true` pulls from Stripe for setups
/// (local dev) where webhooks are not reachable.
async fn status(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Query(q): Query<StatusParams>,
) -> Result<Json<StatusBody>, ApiError> {
    let mut conn = st.pool.acquire().await?;
    let current = read_status(&mut conn, tenant).await?;
    if let (true, Some(account)) = (q.refresh, current.account_id.as_deref()) {
        let s = st.connect.account_status(account).await?;
        sqlx::query(
            "UPDATE tenants SET stripe_charges_enabled = $2, stripe_payouts_enabled = $3, stripe_details_submitted = $4 WHERE id = $1",
        )
        .bind(tenant)
        .bind(s.charges_enabled)
        .bind(s.payouts_enabled)
        .bind(s.details_submitted)
        .execute(&mut *conn)
        .await?;
        return Ok(Json(read_status(&mut conn, tenant).await?));
    }
    Ok(Json(current))
}

fn valid_stripe_id(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|r| {
        (4..=64).contains(&r.len()) && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaymentMethodBody {
    customer: String,
    payment_method: String,
}

/// The owner's app collects the card on the connected account (Checkout in setup
/// mode) and registers the resulting ids here. Only the ids are stored.
async fn set_payment_method(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Path(external_ref): Path<String>,
    Json(b): Json<PaymentMethodBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !valid_stripe_id(&b.customer, "cus_") || !valid_stripe_id(&b.payment_method, "pm_") {
        return Err(ApiError::BadRequest(
            "expected cus_... and pm_... ids".into(),
        ));
    }
    if external_ref.is_empty() || external_ref.len() > 128 {
        return Err(ApiError::BadRequest("bad external_ref".into()));
    }
    let mut tx = st.pool.begin().await?;
    locked_account(&mut tx, tenant).await?;
    let user = store::ensure_user(&mut tx, tenant, &external_ref).await?;
    sqlx::query("UPDATE subscribers SET stripe_customer = $3, stripe_payment_method = $4 WHERE tenant_id = $1 AND user_id = $2")
        .bind(tenant)
        .bind(user as i64)
        .bind(&b.customer)
        .bind(&b.payment_method)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Unauthenticated: authenticity comes from the signature over the raw body.
async fn stripe_webhook(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let secret = st.webhook_secret.as_deref().ok_or(ApiError::NotFound)?;
    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    verify_signature(&body, sig, secret, now, WEBHOOK_TOLERANCE_SECS)
        .map_err(|_| ApiError::Unauthorized)?;
    let event = parse_event(&body).map_err(|_| ApiError::BadRequest("malformed event".into()))?;
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("malformed event".into()))?;

    let mut tx = st.pool.begin().await?;
    let fresh = sqlx::query(
        "INSERT INTO webhook_events (event_id, kind, stripe_account_id, payload) VALUES ($1, $2, $3, $4)
         ON CONFLICT (event_id) DO NOTHING",
    )
    .bind(&event.id)
    .bind(&event.kind)
    .bind(event.account.as_deref())
    .bind(sqlx::types::Json(&payload))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !fresh {
        return Ok(Json(serde_json::json!({ "duplicate": true })));
    }

    // Other kinds (payment_intent.*, charge.dispute.created, ...) are stored above so
    // they are auditable; the engine does not model them yet (DESIGN section 11).
    if event.kind == "account.updated" {
        if let Some(account) = event
            .account
            .as_deref()
            .filter(|a| valid_stripe_id(a, "acct_"))
        {
            let flag = |k: &str| {
                event
                    .object
                    .get(k)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            };
            sqlx::query(
                "UPDATE tenants SET stripe_charges_enabled = $2, stripe_payouts_enabled = $3, stripe_details_submitted = $4
                 WHERE stripe_account_id = $1",
            )
            .bind(account)
            .bind(flag("charges_enabled"))
            .bind(flag("payouts_enabled"))
            .bind(flag("details_submitted"))
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(serde_json::json!({ "received": true })))
}
