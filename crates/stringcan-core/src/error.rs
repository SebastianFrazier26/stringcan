use thiserror::Error;

use crate::engine::{PeriodId, UserId};

#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum EngineError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("amount out of range")]
    AmountOutOfRange,
    #[error("unknown subscriber {0}")]
    UnknownSubscriber(UserId),
    #[error("subscriber {user} already contributed to period {period}")]
    AlreadyPaid { user: UserId, period: PeriodId },
    #[error("payment below the quoted charge")]
    Underpayment,
    #[error("payment would contribute nothing")]
    ZeroPayment,
    #[error("period {0} is not open for revision")]
    PeriodNotOpen(PeriodId),
    #[error("subscriber is not in a state that allows this")]
    InvalidState,
    #[error("no credit to refund")]
    NoCredit,
    #[error("a card payment must carry its processor charge id")]
    MissingChargeRef,
    #[error("renewal needs a price notice for this period first")]
    NoticeRequired,
    #[error("renewal price is above the subscriber's consented cap; fresh consent needed")]
    ConsentRequired,
    #[error("no earlier charge left to refund this credit to")]
    NoRefundableCharge,
    #[error("audit failed: {0}")]
    Audit(String),
}
