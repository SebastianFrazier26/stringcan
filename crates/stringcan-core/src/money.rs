use serde::{Deserialize, Serialize};

use crate::error::EngineError;

pub type Cents = i64;

/// Upper bound on any single amount accepted at the boundary; keeps every
/// intermediate product in i64 (largest is `net * 10_000`, about 1e16).
pub const MAX_CENTS: Cents = 1_000_000_000_000;

pub(crate) fn ceil_div(a: Cents, b: Cents) -> Cents {
    debug_assert!(a >= 0 && b > 0);
    (a + b - 1) / b
}

/// Processor fee estimate: `ceil(gross * bps / 10_000) + fixed` per charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeModel {
    pub bps: u32,
    pub fixed: Cents,
}

impl FeeModel {
    pub const ZERO: FeeModel = FeeModel { bps: 0, fixed: 0 };
    pub const STRIPE_US_CARD: FeeModel = FeeModel {
        bps: 290,
        fixed: 30,
    };

    pub fn validate(&self) -> Result<(), EngineError> {
        if self.bps >= 10_000 || !(0..=MAX_CENTS).contains(&self.fixed) {
            return Err(EngineError::InvalidConfig("fee model out of range"));
        }
        Ok(())
    }

    pub fn fee(&self, gross: Cents) -> Cents {
        if gross <= 0 {
            return 0;
        }
        ceil_div(gross * Cents::from(self.bps), 10_000) + self.fixed
    }

    pub fn net(&self, gross: Cents) -> Cents {
        gross - self.fee(gross)
    }

    /// Smallest charge whose net-of-fee amount is at least `net`.
    pub fn gross_for_net(&self, net: Cents) -> Cents {
        if net <= 0 {
            return 0;
        }
        let denom = 10_000 - Cents::from(self.bps);
        let mut g = ceil_div((net + self.fixed) * 10_000, denom);
        // `net(g)` is non-decreasing in g (g grows by 1, the fee by at most 1),
        // so nudging to the exact boundary terminates in a couple of steps.
        while self.net(g) < net {
            g += 1;
        }
        while g > 1 && self.net(g - 1) >= net {
            g -= 1;
        }
        g
    }
}
