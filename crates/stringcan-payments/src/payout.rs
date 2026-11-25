//! Is the bank account the owner linked through Plaid the one Stripe pays the
//! tenant out to? (DESIGN 20.4, owner-approved 2026-11-23.) Compared on routing
//! number and last four digits only; full account numbers are never kept.

/// One bank account on the owner's Stripe connected account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayoutBank {
    pub routing_number: String,
    pub last4: String,
    /// Lowercase ISO code.
    pub currency: String,
    /// Stripe pays out in `currency` to this account.
    pub default_for_currency: bool,
}

/// ACH identifiers of one Plaid account, reduced to what the check compares.
#[derive(Clone, PartialEq, Eq)]
pub struct AchNumbers {
    pub account_id: String,
    pub routing: String,
    pub last4: String,
    /// Some banks give Plaid a tokenized account number that differs from the
    /// real one, so its last four cannot be compared with Stripe's.
    pub tokenized: bool,
}

impl std::fmt::Debug for AchNumbers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AchNumbers")
            .field("account_id", &self.account_id)
            .field("tokenized", &self.tokenized)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayoutMatch {
    Matched,
    /// The linked account is not where Stripe pays out; the link is refused.
    Mismatch,
    /// Cannot tell; the account stays owner-declared and the ledger says so.
    Unknown(&'static str),
}

/// Compares against the account Stripe pays out to in `currency` (its default
/// for that currency, or any account in that currency if none is flagged).
pub fn match_payout_account(
    plaid: Option<&AchNumbers>,
    stripe: &[PayoutBank],
    currency: &str,
) -> PayoutMatch {
    let Some(p) = plaid else {
        return PayoutMatch::Unknown("bank reported no account numbers");
    };
    if p.tokenized {
        return PayoutMatch::Unknown("bank uses tokenized account numbers");
    }
    let in_currency: Vec<&PayoutBank> = stripe.iter().filter(|b| b.currency == currency).collect();
    let defaults: Vec<&PayoutBank> = in_currency
        .iter()
        .copied()
        .filter(|b| b.default_for_currency)
        .collect();
    let candidates = if defaults.is_empty() {
        in_currency
    } else {
        defaults
    };
    if candidates.is_empty() {
        return PayoutMatch::Unknown("no Stripe payout bank account in the tenant currency");
    }
    if candidates
        .iter()
        .any(|b| b.routing_number == p.routing && b.last4 == p.last4)
    {
        PayoutMatch::Matched
    } else {
        PayoutMatch::Mismatch
    }
}
