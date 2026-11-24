//! Owner bank link through Plaid (sandbox only), used solely to check that the
//! account receiving the tenant's Stripe payouts holds the required float
//! (DESIGN 20.3, 20.4). Only pass/fail ever leaves this module; balances do not.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use stringcan_payments::{verify_float, AccessToken, BankError, BankLink, FloatCheck, LinkProduct};
use uuid::Uuid;

use crate::auth::Owner;
use crate::crypto::{Keyring, Sealed};
use crate::error::ApiError;
use crate::{store, AppState};

/// Owner decision 2026-11-23: `auth` only. Balance reads come with it.
const PRODUCTS: &[LinkProduct] = &[LinkProduct::Auth];

#[derive(Clone)]
pub struct BankState {
    pub link: Arc<dyn BankLink>,
    pub keys: Arc<Keyring>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/bank/link-token", post(link_token))
        .route("/v1/bank/link", post(exchange).delete(unlink))
        .route("/v1/bank/float", get(float_status))
}

fn bank(st: &AppState) -> Result<&BankState, ApiError> {
    st.bank.as_ref().ok_or(ApiError::NotFound)
}

impl From<BankError> for ApiError {
    fn from(e: BankError) -> Self {
        match e {
            BankError::Invalid(m) => ApiError::BadRequest(m.into()),
            BankError::ReauthRequired => {
                ApiError::Conflict("bank link needs the owner to log in again".into())
            }
            BankError::Rejected(_)
            | BankError::Transient(_)
            | BankError::Protocol(_)
            | BankError::NonSandboxRefused => ApiError::ProcessorUnavailable,
        }
    }
}

fn opaque(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Serialize)]
struct LinkTokenBody {
    link_token: String,
}

async fn link_token(
    Owner(tenant): Owner,
    State(st): State<AppState>,
) -> Result<Json<LinkTokenBody>, ApiError> {
    let b = bank(&st)?;
    let link_token = b
        .link
        .create_link_token(&format!("tenant-{tenant}"), PRODUCTS)
        .await?;
    Ok(Json(LinkTokenBody { link_token }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeBody {
    public_token: String,
    /// The Plaid account id of the account that receives Stripe payouts, as the
    /// owner picked it in Link.
    account_id: String,
}

#[derive(Serialize)]
struct LinkedBody {
    account_id: String,
}

struct StoredLink {
    item_id: String,
    account_id: String,
    sealed: Sealed,
}

#[derive(sqlx::FromRow)]
struct LinkRow {
    item_id: String,
    account_id: String,
    token_key_id: String,
    token_nonce: Vec<u8>,
    token_ciphertext: Vec<u8>,
}

async fn stored_link(pool: &sqlx::PgPool, tenant: Uuid) -> Result<Option<StoredLink>, ApiError> {
    let row: Option<LinkRow> = sqlx::query_as(
        "SELECT item_id, account_id, token_key_id, token_nonce, token_ciphertext
         FROM bank_links WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| StoredLink {
        item_id: r.item_id,
        account_id: r.account_id,
        sealed: Sealed {
            key_id: r.token_key_id,
            nonce: r.token_nonce,
            ciphertext: r.token_ciphertext,
        },
    }))
}

fn open_token(keys: &Keyring, tenant: Uuid, sealed: &Sealed) -> Result<AccessToken, ApiError> {
    let plain = keys
        .open(tenant, sealed)
        .map_err(|e| ApiError::Internal(format!("bank token for {tenant}: {e}")))?;
    AccessToken::from_stored(plain).map_err(|_| ApiError::Internal("stored bank token".into()))
}

async fn revoke_quietly(link: &dyn BankLink, token: &AccessToken, tenant: Uuid) {
    if let Err(e) = link.remove_item(token).await {
        tracing::warn!(%tenant, error = %e, "could not revoke Plaid item; revoke it in the Plaid dashboard");
    }
}

/// Replaces any existing link; the previous Plaid item is revoked after the new
/// one is stored.
async fn exchange(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Json(body): Json<ExchangeBody>,
) -> Result<Json<LinkedBody>, ApiError> {
    let b = bank(&st)?;
    if !opaque(&body.public_token, 256) || !opaque(&body.account_id, 128) {
        return Err(ApiError::BadRequest(
            "malformed public_token or account_id".into(),
        ));
    }
    let item = b.link.exchange_public_token(&body.public_token).await?;
    let result = async {
        let accounts = b
            .link
            .balances(&item.access_token, std::slice::from_ref(&body.account_id))
            .await?;
        let chosen = accounts.iter().find(|a| a.account_id == body.account_id);
        match chosen {
            Some(a) if a.depository => {}
            Some(_) => {
                return Err(ApiError::BadRequest(
                    "the payout account must be a deposit account".into(),
                ))
            }
            None => return Err(ApiError::BadRequest("account_id is not on this bank link".into())),
        }
        let sealed = b.keys.seal(tenant, item.access_token.expose_secret());
        let mut tx = st.pool.begin().await?;
        let previous: Option<(String, Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT token_key_id, token_nonce, token_ciphertext FROM bank_links WHERE tenant_id = $1 FOR UPDATE",
        )
        .bind(tenant)
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO bank_links (tenant_id, item_id, account_id, token_key_id, token_nonce, token_ciphertext)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id) DO UPDATE SET item_id = $2, account_id = $3, token_key_id = $4,
               token_nonce = $5, token_ciphertext = $6, linked_at = now(), last_checked_at = NULL, last_result = NULL",
        )
        .bind(tenant)
        .bind(&item.item_id)
        .bind(&body.account_id)
        .bind(&sealed.key_id)
        .bind(&sealed.nonce)
        .bind(&sealed.ciphertext)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(previous)
    }
    .await;
    match result {
        Ok(previous) => {
            if let Some((key_id, nonce, ciphertext)) = previous {
                let sealed = Sealed {
                    key_id,
                    nonce,
                    ciphertext,
                };
                if let Ok(old) = open_token(&b.keys, tenant, &sealed) {
                    revoke_quietly(b.link.as_ref(), &old, tenant).await;
                }
            }
            Ok(Json(LinkedBody {
                account_id: body.account_id,
            }))
        }
        Err(e) => {
            revoke_quietly(b.link.as_ref(), &item.access_token, tenant).await;
            Err(e)
        }
    }
}

async fn unlink(
    Owner(tenant): Owner,
    State(st): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let b = bank(&st)?;
    let link = stored_link(&st.pool, tenant)
        .await?
        .ok_or(ApiError::NotFound)?;
    let token = open_token(&b.keys, tenant, &link.sealed)?;
    b.link.remove_item(&token).await?;
    sqlx::query("DELETE FROM bank_links WHERE tenant_id = $1")
        .bind(tenant)
        .execute(&st.pool)
        .await?;
    Ok(Json(serde_json::json!({ "unlinked": true })))
}

#[derive(Serialize)]
pub struct FloatBody {
    /// `verified`, `short` or `unverifiable`. Never an amount.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

fn label(check: FloatCheck) -> FloatBody {
    match check {
        FloatCheck::Verified => FloatBody {
            status: "verified",
            reason: None,
        },
        FloatCheck::Short => FloatBody {
            status: "short",
            reason: None,
        },
        FloatCheck::Unverifiable(r) => FloatBody {
            status: "unverifiable",
            reason: Some(r),
        },
    }
}

/// On-demand check. The result is recorded and shown (pass/fail only) on the
/// public ledger. A bank that needs re-login, or a revoked link, is reported as
/// unverifiable rather than as an error, because that is what subscribers see.
async fn float_status(
    Owner(tenant): Owner,
    State(st): State<AppState>,
) -> Result<Json<FloatBody>, ApiError> {
    let b = bank(&st)?;
    let link = stored_link(&st.pool, tenant)
        .await?
        .ok_or_else(|| ApiError::Conflict("link the payout bank account first".into()))?;
    let token = open_token(&b.keys, tenant, &link.sealed)?;
    if link.sealed.key_id != b.keys.current_id() {
        let resealed = b.keys.seal(tenant, token.expose_secret());
        sqlx::query(
            "UPDATE bank_links SET token_key_id = $2, token_nonce = $3, token_ciphertext = $4
             WHERE tenant_id = $1 AND item_id = $5",
        )
        .bind(tenant)
        .bind(&resealed.key_id)
        .bind(&resealed.nonce)
        .bind(&resealed.ciphertext)
        .bind(&link.item_id)
        .execute(&st.pool)
        .await?;
    }

    let (required, currency) = {
        let mut tx = store::begin_read(&st.pool).await?;
        let loaded = store::load_snapshot(&mut tx, tenant).await?;
        (loaded.engine.required_float(), loaded.currency)
    };
    let check = match b
        .link
        .balances(&token, std::slice::from_ref(&link.account_id))
        .await
    {
        Ok(balances) => {
            let payout: Vec<_> = balances
                .into_iter()
                .filter(|a| a.account_id == link.account_id)
                .collect();
            if payout.is_empty() {
                FloatCheck::Unverifiable("payout account no longer on the bank link")
            } else {
                verify_float(&payout, required, &currency.to_ascii_lowercase())
            }
        }
        Err(BankError::ReauthRequired) => {
            FloatCheck::Unverifiable("bank link needs the owner to log in again")
        }
        Err(BankError::Rejected(_)) => FloatCheck::Unverifiable("bank link rejected by Plaid"),
        Err(e) => return Err(e.into()),
    };
    let body = label(check);
    sqlx::query(
        "UPDATE bank_links SET last_checked_at = now(), last_result = $2 WHERE tenant_id = $1 AND item_id = $3",
    )
    .bind(tenant)
    .bind(body.status)
    .bind(&link.item_id)
    .execute(&st.pool)
    .await?;
    Ok(Json(body))
}

#[derive(Serialize)]
pub struct PublicFloat {
    pub status: String,
    pub checked_at_unix: i64,
}

/// Last recorded pass/fail for the public ledger; `None` when never checked.
pub async fn last_public_check(
    conn: &mut sqlx::PgConnection,
    tenant: Uuid,
) -> Result<Option<PublicFloat>, ApiError> {
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT last_result, EXTRACT(EPOCH FROM last_checked_at)::BIGINT FROM bank_links
         WHERE tenant_id = $1 AND last_result IS NOT NULL",
    )
    .bind(tenant)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(status, checked_at_unix)| PublicFloat {
        status,
        checked_at_unix,
    }))
}
