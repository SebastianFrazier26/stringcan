//! Deterministic break-even pricing and ledger engine.
//!
//! No I/O, no clock, no randomness: the same command sequence always produces
//! the same state, which is what lets the API rebuild a tenant by replay and
//! lets anyone audit the ledger. Rules are specified in `docs/DESIGN.md`.

mod engine;
mod error;
mod money;
mod waterfill;

pub use engine::{
    Blocked, Charge, Command, Config, Contribution, Engine, EntryKind, JournalEntry, Notice,
    Outcome, Period, PeriodId, Quote, RefundLeg, RefundReason, Settlement, ShareRecord, Status,
    Subscriber, UserId, CURRENT_RULES, MAX_REMEMBERED_CHARGES, RULES_V1,
};
pub use error::EngineError;
pub use money::{Cents, FeeModel, MAX_CENTS};
pub use waterfill::water_fill;
