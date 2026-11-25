use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as SqlJson;
use stringcan_core::{
    Cents, Command, Config, EngineError, EntryKind, JournalEntry, Outcome, PeriodId, Quote,
    RefundLeg, Settlement, Status, MAX_CENTS,
};
use stringcan_payments::{ChargeRequest, RefundRequest};
use uuid::Uuid;

use crate::auth::{generate_api_key, hash_key, Admin, Owner};
use crate::error::ApiError;
use crate::notify::{self, PriceNotice, NOTICE_LEAD_DAYS};
use crate::{store, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/tenants", post(create_tenant))
        .route("/v1/quote", get(quote))
        .route("/v1/payments", post(pay))
        .route("/v1/subscribers/{external_ref}/cancel", post(cancel))
        .route("/v1/subscribers/{external_ref}/lapse", post(lapse))
        .route(
            "/v1/subscribers/{external_ref}/refund-credit",
            post(refund_credit),
        )
        .route(
            "/v1/subscribers/{external_ref}/renewal-cap",
            put(set_renewal_cap),
        )
        .route("/v1/periods/{id}/upkeep", put(set_upkeep))
        .route("/v1/periods/{id}/notices", post(issue_notices))
        .route("/v1/periods/settle", post(settle))
        .route("/v1/public/{tenant_id}/ledger", get(public_ledger))
}

fn valid_ref(s: &str) -> Result<(), ApiError> {
    let ok = (1..=128).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '@' | '-'));
    if ok {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "external_ref must be 1-128 chars of [A-Za-z0-9_.:@-]".into(),
        ))
    }
}

fn valid_cents(c: Cents) -> Result<(), ApiError> {
    if (0..=MAX_CENTS).contains(&c) {
        Ok(())
    } else {
        Err(ApiError::BadRequest("amount out of range".into()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTenant {
    name: String,
    first_upkeep_cents: Cents,
    #[serde(default)]
    reserve_cap_bps: Option<u32>,
    #[serde(default)]
    reserve_surcharge_bps: Option<u32>,
}

#[derive(Serialize)]
struct TenantCreated {
    tenant_id: Uuid,
    /// Shown once; only its hash is stored.
    api_key: String,
}

async fn create_tenant(
    _: Admin,
    State(st): State<AppState>,
    Json(body): Json<NewTenant>,
) -> Result<Json<TenantCreated>, ApiError> {
    if !(1..=100).contains(&body.name.chars().count()) {
        return Err(ApiError::BadRequest("name must be 1-100 characters".into()));
    }
    valid_cents(body.first_upkeep_cents)?;
    let defaults = Config::default();
    let cfg = Config {
        reserve_cap_bps: body.reserve_cap_bps.unwrap_or(defaults.reserve_cap_bps),
        reserve_surcharge_bps: body
            .reserve_surcharge_bps
            .unwrap_or(defaults.reserve_surcharge_bps),
        ..defaults
    };
    cfg.validate()?;
    let key = generate_api_key();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO tenants (name, api_key_hash, engine_config, first_upkeep_cents)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(&body.name)
    .bind(hash_key(&key).as_slice())
    .bind(SqlJson(cfg))
    .bind(body.first_upkeep_cents)
    .fetch_one(&st.pool)
    .await?;
    Ok(Json(TenantCreated {
        tenant_id: id,
        api_key: key,
    }))
}

#[derive(Deserialize)]
struct QuoteParams {
    external_ref: String,
}

async fn quote(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Query(p): Query<QuoteParams>,
) -> Result<Json<Quote>, ApiError> {
    valid_ref(&p.external_ref)?;
    let mut tx = store::begin_read(&st.pool).await?;
    let loaded = store::load_snapshot(&mut tx, tenant).await?;
    // An unseen ref quotes as a stranger: no state, hence an id no one holds.
    let user = store::find_user(&mut tx, tenant, &p.external_ref)
        .await?
        .unwrap_or(u64::MAX);
    Ok(Json(loaded.engine.quote(user)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PayBody {
    external_ref: String,
}

#[derive(Serialize)]
struct Paid {
    period: PeriodId,
    charged_gross: Cents,
    processor_fee: Cents,
    credit_applied: Cents,
    contribution: Cents,
}

async fn pay(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Json(body): Json<PayBody>,
) -> Result<Json<Paid>, ApiError> {
    valid_ref(&body.external_ref)?;
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let user = store::ensure_user(&mut tx, tenant, &body.external_ref).await?;
    let q = loaded.engine.quote(user)?;
    match q.blocked {
        Some(stringcan_core::Blocked::NoticeRequired) => {
            return Err(EngineError::NoticeRequired.into())
        }
        Some(stringcan_core::Blocked::ConsentRequired) => {
            return Err(EngineError::ConsentRequired.into())
        }
        None => {}
    }
    if q.ceiling.is_some() {
        let wait = notify::lead_wait(&mut tx, tenant, user, q.period, NOTICE_LEAD_DAYS).await?;
        if let Some(ready_at) = wait {
            return Err(ApiError::Conflict(format!(
                "the price notice for period {} is less than {NOTICE_LEAD_DAYS} days old; \
                 the renewal can be charged from unix time {ready_at}",
                q.period
            )));
        }
    }

    // The lock is held across the processor call: simple and correct, at the cost
    // of serialising a tenant's payments. Revisit with a reservation scheme if needed.
    let (fee, processor_ref) = if q.gross_due > 0 {
        let (account, charges_enabled): (Option<String>, bool) = sqlx::query_as(
            "SELECT stripe_account_id, stripe_charges_enabled FROM tenants WHERE id = $1",
        )
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await?;
        if st.require_connect && !(account.is_some() && charges_enabled) {
            return Err(ApiError::Conflict(
                "complete Stripe Connect onboarding before taking payments".into(),
            ));
        }
        let (customer, payment_method): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT stripe_customer, stripe_payment_method FROM subscribers WHERE tenant_id = $1 AND user_id = $2",
        )
        .bind(tenant)
        .bind(user as i64)
        .fetch_one(&mut *tx)
        .await?;
        let res = st
            .processor
            .charge(&ChargeRequest {
                idempotency_key: format!("{tenant}:{user}:{}", q.period),
                gross: q.gross_due,
                currency: loaded.currency.clone(),
                customer_ref: body.external_ref.clone(),
                description: format!("stringcan period {}", q.period),
                connected_account: account,
                customer,
                payment_method,
            })
            .await?;
        (res.fee, Some(res.processor_ref))
    } else {
        (0, None)
    };
    let cmd = Command::RecordPayment {
        user,
        gross: q.gross_due,
        fee,
        charge_ref: processor_ref.clone(),
    };
    let Outcome::Paid {
        period,
        contribution,
        credit_applied,
    } = loaded.engine.execute(&cmd)?
    else {
        return Err(ApiError::Internal("unexpected outcome".into()));
    };
    store::append(
        &mut tx,
        tenant,
        &mut loaded,
        &cmd,
        processor_ref.as_deref(),
        st.snapshot_every,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(Paid {
        period,
        charged_gross: q.gross_due,
        processor_fee: fee,
        credit_applied,
        contribution,
    }))
}

#[derive(Serialize)]
struct StatusBody {
    status: Status,
}

async fn status_change(
    tenant: Uuid,
    st: &AppState,
    external_ref: &str,
    make: fn(u64) -> Command,
) -> Result<Json<StatusBody>, ApiError> {
    valid_ref(external_ref)?;
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let user = store::find_user(&mut tx, tenant, external_ref)
        .await?
        .ok_or(ApiError::NotFound)?;
    let cmd = make(user);
    let Outcome::StatusChanged(status) = loaded.engine.execute(&cmd)? else {
        return Err(ApiError::Internal("unexpected outcome".into()));
    };
    store::append(&mut tx, tenant, &mut loaded, &cmd, None, st.snapshot_every).await?;
    tx.commit().await?;
    Ok(Json(StatusBody { status }))
}

async fn cancel(
    Owner(t): Owner,
    State(st): State<AppState>,
    Path(r): Path<String>,
) -> Result<Json<StatusBody>, ApiError> {
    status_change(t, &st, &r, |user| Command::Cancel { user }).await
}

async fn lapse(
    Owner(t): Owner,
    State(st): State<AppState>,
    Path(r): Path<String>,
) -> Result<Json<StatusBody>, ApiError> {
    status_change(t, &st, &r, |user| Command::Lapse { user }).await
}

#[derive(Serialize)]
struct Refunded {
    refunded_cents: Cents,
}

async fn refund_credit(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Path(r): Path<String>,
) -> Result<Json<Refunded>, ApiError> {
    valid_ref(&r)?;
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let user = store::find_user(&mut tx, tenant, &r)
        .await?
        .ok_or(ApiError::NotFound)?;
    let cmd = Command::RefundCredit { user };
    let outcome = loaded.engine.execute(&cmd).map_err(|e| match e {
        EngineError::NoRefundableCharge => ApiError::Conflict(
            "no earlier charge left to refund this credit to; settle it with the owner".into(),
        ),
        other => other.into(),
    })?;
    let (amount, refund_ref) = match outcome {
        Outcome::RefundedTo(legs) => {
            let key = format!("{tenant}:refund:{}", loaded.next_seq);
            let refs = refund_legs(&st, &mut tx, tenant, &legs, &key).await?;
            (legs.iter().map(|l| l.amount).sum(), refs.join(","))
        }
        Outcome::Refunded(amount) => (
            amount,
            refund_v1(&st, &mut tx, tenant, user, amount, &r, loaded.next_seq).await?,
        ),
        _ => return Err(ApiError::Internal("unexpected outcome".into())),
    };
    store::append(
        &mut tx,
        tenant,
        &mut loaded,
        &cmd,
        Some(&refund_ref),
        st.snapshot_every,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(Refunded {
        refunded_cents: amount,
    }))
}

/// Rules v1: refund the whole credit against the subscriber's latest charge,
/// refusing when the credit exceeds it.
async fn refund_v1(
    st: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: Uuid,
    user: u64,
    amount: Cents,
    external_ref: &str,
    seq: i64,
) -> Result<String, ApiError> {
    let charge: Option<(String, Option<i64>)> = sqlx::query_as(
        "SELECT processor_ref, (command->'RecordPayment'->>'gross')::bigint FROM commands
         WHERE tenant_id = $1 AND processor_ref IS NOT NULL AND command @> $2
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(tenant)
    .bind(SqlJson(
        serde_json::json!({ "RecordPayment": { "user": user } }),
    ))
    .fetch_optional(&mut **tx)
    .await?;
    let Some((charge_ref, Some(charge_gross))) = charge else {
        return Err(ApiError::Conflict(
            "no charge to refund this credit against; settle it with the owner".into(),
        ));
    };
    if amount > charge_gross {
        return Err(ApiError::Conflict(
            "credit exceeds the latest charge; refund it manually".into(),
        ));
    }
    let account: Option<String> =
        sqlx::query_scalar("SELECT stripe_account_id FROM tenants WHERE id = $1")
            .bind(tenant)
            .fetch_one(&mut **tx)
            .await?;
    Ok(st
        .processor
        .refund(&RefundRequest {
            idempotency_key: format!("{tenant}:{user}:refund:{seq}"),
            amount,
            customer_ref: external_ref.to_owned(),
            connected_account: account,
            charge_ref: Some(charge_ref),
        })
        .await?)
}

/// Rules v2: one processor refund per leg, each to a charge the subscriber made
/// on the owner's own account. Runs before the command is appended, so a failed
/// refund appends nothing; the per-leg idempotency key makes a retry safe.
async fn refund_legs(
    st: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: Uuid,
    legs: &[RefundLeg],
    key: &str,
) -> Result<Vec<String>, ApiError> {
    if legs.is_empty() {
        return Ok(Vec::new());
    }
    let account: Option<String> =
        sqlx::query_scalar("SELECT stripe_account_id FROM tenants WHERE id = $1")
            .bind(tenant)
            .fetch_one(&mut **tx)
            .await?;
    let mut refs = Vec::with_capacity(legs.len());
    for (i, leg) in legs.iter().enumerate() {
        let customer_ref = store::external_ref(tx, tenant, leg.user).await?;
        refs.push(
            st.processor
                .refund(&RefundRequest {
                    idempotency_key: format!("{key}:{i}"),
                    amount: leg.amount,
                    customer_ref,
                    connected_account: account.clone(),
                    charge_ref: Some(leg.charge_ref.clone()),
                })
                .await?,
        );
    }
    Ok(refs)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapBody {
    /// Most the subscriber agreed to be charged per renewal, fees included.
    max_charge_cents: Cents,
}

/// Records the subscriber's express consent to renew up to a cap. The command
/// row (with its timestamp) is the consent record.
async fn set_renewal_cap(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Path(r): Path<String>,
    Json(b): Json<CapBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    valid_ref(&r)?;
    valid_cents(b.max_charge_cents)?;
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let user = store::find_user(&mut tx, tenant, &r)
        .await?
        .ok_or(ApiError::NotFound)?;
    let cmd = Command::SetRenewalCap {
        user,
        cents: b.max_charge_cents,
    };
    loaded.engine.execute(&cmd)?;
    store::append(&mut tx, tenant, &mut loaded, &cmd, None, st.snapshot_every).await?;
    tx.commit().await?;
    Ok(Json(
        serde_json::json!({ "max_charge_cents": b.max_charge_cents }),
    ))
}

#[derive(Serialize)]
struct NoticesBody {
    period: PeriodId,
    noticed: usize,
    consent_needed: usize,
    lead_days: u32,
}

/// The notifications job: call it `NOTICE_LEAD_DAYS` before the period's billing
/// date (no scheduler yet); renewals are refused until their notice is that old. Notices are delivered before the command commits, so
/// a delivery failure records nothing and a rerun notices everyone again.
async fn issue_notices(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Path(id): Path<PeriodId>,
) -> Result<Json<NoticesBody>, ApiError> {
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let cmd = Command::IssueNotices { period: id };
    let Outcome::Noticed(notices) = loaded.engine.execute(&cmd)? else {
        return Err(ApiError::Internal("unexpected outcome".into()));
    };
    for n in &notices {
        let external_ref = store::external_ref(&mut tx, tenant, n.user).await?;
        st.notifier
            .price_notice(&PriceNotice {
                tenant,
                external_ref,
                period: n.period,
                ceiling_gross: n.ceiling_gross,
                renewal_cap: loaded.engine.subscriber(n.user).and_then(|s| s.renewal_cap),
                consent_needed: n.consent_needed,
                lead_days: NOTICE_LEAD_DAYS,
            })
            .await
            .map_err(|_| ApiError::ProcessorUnavailable)?;
    }
    if !notices.is_empty() {
        store::append(&mut tx, tenant, &mut loaded, &cmd, None, st.snapshot_every).await?;
        let users: Vec<_> = notices.iter().map(|n| n.user).collect();
        notify::record_issued(&mut tx, tenant, id, &users).await?;
    }
    tx.commit().await?;
    Ok(Json(NoticesBody {
        period: id,
        noticed: notices.len(),
        consent_needed: notices.iter().filter(|n| n.consent_needed).count(),
        lead_days: NOTICE_LEAD_DAYS,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpkeepBody {
    cents: Cents,
}

async fn set_upkeep(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Path(id): Path<PeriodId>,
    Json(b): Json<UpkeepBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    valid_cents(b.cents)?;
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let cmd = Command::SetUpkeep {
        period: id,
        cents: b.cents,
    };
    loaded.engine.execute(&cmd)?;
    store::append(&mut tx, tenant, &mut loaded, &cmd, None, st.snapshot_every).await?;
    tx.commit().await?;
    Ok(Json(
        serde_json::json!({ "period": id, "upkeep_cents": b.cents }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettleBody {
    actual_cents: Cents,
    evidence: Option<String>,
}

async fn settle(
    Owner(tenant): Owner,
    State(st): State<AppState>,
    Json(b): Json<SettleBody>,
) -> Result<Json<Settlement>, ApiError> {
    valid_cents(b.actual_cents)?;
    let mut tx = st.pool.begin().await?;
    let mut loaded = store::load_locked(&mut tx, tenant).await?;
    let cmd = Command::Settle {
        actual: b.actual_cents,
        evidence: b.evidence,
    };
    let Outcome::Settled(s) = loaded.engine.execute(&cmd).map_err(|e| match e {
        EngineError::AmountOutOfRange => {
            ApiError::BadRequest("evidence too long (max 512 bytes)".into())
        }
        other => other.into(),
    })?
    else {
        return Err(ApiError::Internal("unexpected outcome".into()));
    };
    loaded.engine.audit()?;
    let key = format!("{tenant}:settle:{}", loaded.next_seq);
    let refs = refund_legs(&st, &mut tx, tenant, &s.refunds, &key).await?;
    let refs = (!refs.is_empty()).then(|| refs.join(","));
    store::append(
        &mut tx,
        tenant,
        &mut loaded,
        &cmd,
        refs.as_deref(),
        st.snapshot_every,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(*s))
}

#[derive(Serialize)]
struct LedgerEntry {
    seq: u64,
    kind: EntryKind,
    period: Option<PeriodId>,
    subscriber: Option<String>,
    cents: Cents,
}

#[derive(Serialize)]
struct PeriodView {
    id: PeriodId,
    upkeep_cents: Cents,
    carried_deficit_cents: Cents,
    reserve_topup_cents: Cents,
    reserve_prefund_cents: Cents,
    target_cents: Cents,
    collected_cents: Cents,
    settled: Option<SettledView>,
}

#[derive(Serialize)]
struct SettledView {
    actual_upkeep_cents: Cents,
    evidence: Option<String>,
    payout_cents: Cents,
    participants: usize,
    min_share_cents: Cents,
    max_share_cents: Cents,
    credits_issued_cents: Cents,
}

#[derive(Serialize)]
struct Ledger {
    /// True when the engine's conservation audit passes over the whole history.
    break_even: bool,
    reserve_cents: Cents,
    credits_outstanding_cents: Cents,
    /// Last pass/fail check of the owner's payout account against the required float.
    float_check: Option<crate::bank::PublicFloat>,
    periods: Vec<PeriodView>,
    entries: Vec<LedgerEntry>,
}

fn entry_view(e: &JournalEntry) -> LedgerEntry {
    LedgerEntry {
        seq: e.seq,
        kind: e.kind,
        period: e.period,
        subscriber: e.user.map(|u| format!("s-{u}")),
        cents: e.amount,
    }
}

/// Unauthenticated by design (docs/DESIGN.md section 12). Subscribers appear only
/// as per-tenant ordinals.
async fn public_ledger(
    State(st): State<AppState>,
    Path(tenant): Path<Uuid>,
) -> Result<Json<Ledger>, ApiError> {
    let mut tx = store::begin_read(&st.pool).await?;
    let loaded = store::load_snapshot(&mut tx, tenant).await?;
    let e = &loaded.engine;
    let periods = e
        .periods()
        .map(|p| PeriodView {
            id: p.id,
            upkeep_cents: p.upkeep,
            carried_deficit_cents: p.deficit,
            reserve_topup_cents: p.topup,
            reserve_prefund_cents: p.prefund,
            target_cents: p.target(),
            collected_cents: p.collected(),
            settled: p.settled.as_ref().map(|s| SettledView {
                actual_upkeep_cents: s.actual_upkeep,
                evidence: s.evidence.clone(),
                payout_cents: s.payout,
                participants: s.shares.len(),
                min_share_cents: s.shares.iter().map(|r| r.share).min().unwrap_or(0),
                max_share_cents: s.shares.iter().map(|r| r.share).max().unwrap_or(0),
                credits_issued_cents: s.shares.iter().map(|r| r.credit_issued).sum(),
            }),
        })
        .collect();
    let float_check = crate::bank::last_public_check(&mut tx, tenant).await?;
    Ok(Json(Ledger {
        break_even: e.audit().is_ok(),
        reserve_cents: e.reserve(),
        credits_outstanding_cents: e.credits_outstanding(),
        float_check,
        periods,
        entries: e.journal().iter().map(entry_view).collect(),
    }))
}
