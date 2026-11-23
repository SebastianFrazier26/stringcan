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
    Command, Config, Contribution, Engine, EntryKind, JournalEntry, Outcome, Period, PeriodId,
    Quote, Settlement, ShareRecord, Status, Subscriber, UserId,
};
pub use error::EngineError;
pub use money::{Cents, FeeModel, MAX_CENTS};
pub use waterfill::water_fill;
