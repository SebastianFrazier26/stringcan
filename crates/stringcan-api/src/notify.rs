//! Renewal price notices (DESIGN 20.8, option N3). Nothing here sends email yet:
//! `LogNotifier` only records that a notice was due, and `RecordingNotifier` is
//! the test double. A real sender implements `Notifier`.

use std::sync::Mutex;

use async_trait::async_trait;
use stringcan_core::{Cents, PeriodId};
use uuid::Uuid;

/// Owner decision 2026-11-23: notices go out 10 days before the billing date
/// (California requires 7 to 30). The engine has no clock, so whoever calls
/// `POST /v1/periods/{id}/notices` is responsible for the timing.
pub const NOTICE_LEAD_DAYS: u32 = 10;

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
