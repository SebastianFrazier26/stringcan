//! Renewal price notices (DESIGN 20.8, option N3). Nothing here sends email yet:
//! `LogNotifier` only records that a notice was due, and `RecordingNotifier` is
//! the test double. A real sender implements `Notifier`.

use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::PgConnection;
use stringcan_core::{Cents, PeriodId, UserId};
use uuid::Uuid;

use crate::error::ApiError;

/// Owner decision 2026-11-23: notices go out 10 days before the billing date
/// (California requires 7 to 30). Enforced from the other side, approved
/// 2026-11-23: a renewal is not charged until its notice is this old, so the
/// caller still runs the notices job on time but cannot charge early.
pub const NOTICE_LEAD_DAYS: u32 = 10;

/// Records when each notice went out; call in the transaction that appends the
/// `IssueNotices` command.
pub async fn record_issued(
    conn: &mut PgConnection,
    tenant: Uuid,
    period: PeriodId,
    users: &[UserId],
) -> Result<(), ApiError> {
    let ids: Vec<i64> = users.iter().map(|&u| u as i64).collect();
    sqlx::query(
        "INSERT INTO price_notices (tenant_id, user_id, period)
         SELECT $1, u, $3 FROM unnest($2::bigint[]) AS u
         ON CONFLICT DO NOTHING",
    )
    .bind(tenant)
    .bind(&ids)
    .bind(period as i32)
    .execute(conn)
    .await?;
    Ok(())
}

/// `None` when the renewal may be charged now, else the unix time from which it
/// may. A notice logged before `price_notices` existed falls back to the latest
/// `IssueNotices` run for the period, which is never earlier than the real
/// notice, so the fallback can only delay a charge, never allow an early one.
pub async fn lead_wait(
    conn: &mut PgConnection,
    tenant: Uuid,
    user: UserId,
    period: PeriodId,
    lead_days: u32,
) -> Result<Option<i64>, ApiError> {
    let row: (Option<i64>, Option<bool>) = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM ready)::BIGINT, ready <= now()
         FROM (SELECT COALESCE(
             (SELECT issued_at FROM price_notices
              WHERE tenant_id = $1 AND user_id = $2 AND period = $3),
             (SELECT max(created_at) FROM commands
              WHERE tenant_id = $1
                AND command @> jsonb_build_object('IssueNotices', jsonb_build_object('period', $3)))
         ) + make_interval(days => $4) AS ready) t",
    )
    .bind(tenant)
    .bind(user as i64)
    .bind(period as i32)
    .bind(lead_days as i32)
    .fetch_one(conn)
    .await?;
    match row {
        (Some(_), Some(true)) => Ok(None),
        (Some(at), _) => Ok(Some(at)),
        (None, _) => Err(ApiError::Internal(format!(
            "notice for period {period} has no issue time"
        ))),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceNotice {
    pub tenant: Uuid,
    /// The owner app's id for the subscriber; the app maps it to a contact.
    pub external_ref: String,
    pub period: PeriodId,
    /// The most this renewal will charge, fees included, before any credit.
    pub ceiling_gross: Cents,
    pub renewal_cap: Option<Cents>,
    /// The ceiling is above the consented cap: the message must ask for fresh
    /// consent, and without it the subscriber is not charged.
    pub consent_needed: bool,
    pub lead_days: u32,
}

#[derive(Debug, thiserror::Error)]
#[error("notice could not be delivered: {0}")]
pub struct NotifyError(pub String);

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn price_notice(&self, notice: &PriceNotice) -> Result<(), NotifyError>;
}

/// Logs without the subscriber ref, which may be an email address.
pub struct LogNotifier;

#[async_trait]
impl Notifier for LogNotifier {
    async fn price_notice(&self, n: &PriceNotice) -> Result<(), NotifyError> {
        tracing::info!(
            tenant = %n.tenant,
            period = n.period,
            consent_needed = n.consent_needed,
            "price notice due (no sender configured)"
        );
        Ok(())
    }
}

/// Test double: keeps every notice, and can be told to fail.
#[derive(Default)]
pub struct RecordingNotifier {
    sent: Mutex<Vec<PriceNotice>>,
    fail: Mutex<bool>,
}

impl RecordingNotifier {
    pub fn sent(&self) -> Vec<PriceNotice> {
        self.sent.lock().expect("notifier lock").clone()
    }

    pub fn fail_next(&self, fail: bool) {
        *self.fail.lock().expect("notifier lock") = fail;
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn price_notice(&self, n: &PriceNotice) -> Result<(), NotifyError> {
        if *self.fail.lock().expect("notifier lock") {
            return Err(NotifyError("test double told to fail".into()));
        }
        self.sent.lock().expect("notifier lock").push(n.clone());
        Ok(())
    }
}
