use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::money::{ceil_div, Cents, FeeModel, MAX_CENTS};
use crate::waterfill::water_fill;

pub type UserId = u64;
pub type PeriodId = u32;

/// Rules a tenant was created under. A tenant keeps its version for life so its
/// command log always replays under the rules it was written with.
/// v1: original rules. v2 (2026-11-23): credit safeguards (refund only to the
/// card that paid, per-subscriber cap, dormant sweep) and noticed price ceilings
/// with a subscriber renewal cap (DESIGN 20.7, 20.8). v3 (2026-11-23): an
/// absolute per-subscriber credit ceiling under the US closed-loop limit.
pub const RULES_V1: u32 = 1;
pub const RULES_V2: u32 = 2;
pub const CURRENT_RULES: u32 = 3;

/// The US closed-loop prepaid-access threshold, $2,000 (31 CFR
/// 1010.100(ff)(4)(iii)(A)); a v3 credit ceiling may not exceed it.
pub const CLOSED_LOOP_LIMIT: Cents = 200_000;

/// Charges remembered per subscriber as refund targets. Owner-approved
/// 2026-11-23: six, about six monthly charges, since card refunds get harder
/// with age; older charges stop being refund targets.
pub const MAX_REMEMBERED_CHARGES: usize = 6;

fn rules_v1() -> u32 {
    RULES_V1
}
fn default_credit_cap_floor() -> Cents {
    10_000
}
fn default_dormant_after() -> u32 {
    3
}
/// Owner decision 2026-11-24: $500.
fn default_credit_cap_ceiling() -> Cents {
    50_000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub fee: FeeModel,
    /// Reserve cap as basis points of a period's upkeep.
    pub reserve_cap_bps: u32,
    /// Per-period reserve build, basis points of upkeep, while below the cap.
    pub reserve_surcharge_bps: u32,
    /// Configs stored before versioning carry no field and are v1.
    #[serde(default = "rules_v1")]
    pub rules_version: u32,
    /// v2: a subscriber's credit is capped at max(this, two of their shares).
    #[serde(default = "default_credit_cap_floor")]
    pub credit_cap_floor: Cents,
    /// v2: credit of a lapsed or ended subscriber is refunded to their card once
    /// this many periods have passed since the last period they paid for.
    #[serde(default = "default_dormant_after")]
    pub dormant_after_periods: u32,
    /// v3: no subscriber's credit is held above this, whatever their share.
    #[serde(default = "default_credit_cap_ceiling")]
    pub credit_cap_ceiling: Cents,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            fee: FeeModel::STRIPE_US_CARD,
            reserve_cap_bps: 5_000,
            reserve_surcharge_bps: 0,
            rules_version: CURRENT_RULES,
            credit_cap_floor: default_credit_cap_floor(),
            dormant_after_periods: default_dormant_after(),
            credit_cap_ceiling: default_credit_cap_ceiling(),
        }
    }
}

impl Config {
    /// The original rules, kept so v1 tenants replay unchanged.
    pub fn v1() -> Self {
        Config {
            rules_version: RULES_V1,
            ..Config::default()
        }
    }

    /// Rules v2, kept so v2 tenants replay unchanged.
    pub fn v2() -> Self {
        Config {
            rules_version: RULES_V2,
            ..Config::default()
        }
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        self.fee.validate()?;
        if self.reserve_cap_bps > 100_000 || self.reserve_surcharge_bps > 100_000 {
            return Err(EngineError::InvalidConfig("reserve bps out of range"));
        }
        if !(RULES_V1..=CURRENT_RULES).contains(&self.rules_version) {
            return Err(EngineError::InvalidConfig("unknown rules version"));
        }
        if !(0..=MAX_CENTS).contains(&self.credit_cap_floor) {
            return Err(EngineError::InvalidConfig("credit cap floor out of range"));
        }
        if !(1..=120).contains(&self.dormant_after_periods) {
            return Err(EngineError::InvalidConfig(
                "dormant period count out of range",
            ));
        }
        if self.rules_version >= 3
            && !(self.credit_cap_floor..=CLOSED_LOOP_LIMIT).contains(&self.credit_cap_ceiling)
        {
            return Err(EngineError::InvalidConfig(
                "credit cap ceiling must be between the floor and $2,000",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Active,
    /// Paid for the current period, will not renew.
    Cancelling,
    Lapsed,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscriber {
    pub status: Status,
    /// Latest period this subscriber has contributed to (0 = never).
    pub paid_through: PeriodId,
    pub credit: Cents,
    /// v2: recent card charges, oldest first; credit is refunded only to these.
    #[serde(default)]
    pub charges: Vec<Charge>,
    /// v2: final share in the last settled period they took part in.
    #[serde(default)]
    pub last_share: Cents,
    /// v2: the most this subscriber consented to be charged per renewal (gross).
    #[serde(default)]
    pub renewal_cap: Option<Cents>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Charge {
    /// Processor id of the payment (e.g. a `PaymentIntent`), refunded against.
    pub charge_ref: String,
    pub gross: Cents,
    pub refunded: Cents,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundReason {
    Requested,
    /// Credit above the per-subscriber cap.
    OverCap,
    /// Credit of a subscriber who left and stayed away.
    Dormant,
}

/// One refund to one earlier charge. Several legs may serve one refund.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundLeg {
    pub user: UserId,
    pub charge_ref: String,
    pub amount: Cents,
    pub reason: RefundReason,
}

/// A logged advance notice of the most a renewer will be charged for a period.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub user: UserId,
    pub period: PeriodId,
    /// Share ceiling in net cents; the charge never exceeds its gross-up.
    pub ceiling: Cents,
    pub ceiling_gross: Cents,
    /// The ceiling is above the subscriber's renewal cap (or no cap is on file),
    /// so they will not be charged without fresh consent.
    pub consent_needed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    pub user: UserId,
    pub cash_net: Cents,
    pub credit_used: Cents,
}

impl Contribution {
    pub fn amount(&self) -> Cents {
        self.cash_net + self.credit_used
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRecord {
    pub user: UserId,
    pub contributed: Cents,
    pub share: Cents,
    pub credit_issued: Cents,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub actual_upkeep: Cents,
    pub deficit_in: Cents,
    pub evidence: Option<String>,
    /// What users plus prefund had to fund: actual + carried deficit + top-up.
    pub target: Cents,
    pub shares: Vec<ShareRecord>,
    pub payout: Cents,
    pub reserve_draw: Cents,
    pub reserve_topup: Cents,
    pub deficit_out: Cents,
    /// v2: refunds made at settlement (credit above the cap, dormant credit).
    #[serde(default)]
    pub refunds: Vec<RefundLeg>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    pub id: PeriodId,
    pub upkeep: Cents,
    /// True until the owner states an estimate; settlement of the previous
    /// period then replaces it with that period's actual upkeep.
    pub upkeep_is_default: bool,
    pub deficit: Cents,
    pub topup: Cents,
    pub prefund: Cents,
    /// Reserve cap fixed when the period opened.
    pub cap: Cents,
    pub contributions: Vec<Contribution>,
    contributors: BTreeSet<UserId>,
    pub settled: Option<Settlement>,
}

impl Period {
    fn new(id: PeriodId, upkeep: Cents, upkeep_is_default: bool) -> Self {
        Period {
            id,
            upkeep,
            upkeep_is_default,
            deficit: 0,
            topup: 0,
            prefund: 0,
            cap: 0,
            contributions: Vec::new(),
            contributors: BTreeSet::new(),
            settled: None,
        }
    }

    pub fn target(&self) -> Cents {
        self.upkeep + self.deficit + self.topup
    }

    /// What subscribers (not the reserve prefund) must contribute.
    pub fn user_target(&self) -> Cents {
        (self.target() - self.prefund).max(0)
    }

    pub fn collected(&self) -> Cents {
        self.contributions.iter().map(Contribution::amount).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    ContributionCash,
    CreditApplied,
    CreditIssued,
    CreditRefunded,
    Payout,
    ReserveDraw,
    ReserveTopup,
    ReserveRelease,
    DeficitCarried,
    /// v2: a renewal price ceiling was noticed to a subscriber (amount = ceiling).
    PriceNotice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: u64,
    pub kind: EntryKind,
    pub period: Option<PeriodId>,
    pub user: Option<UserId>,
    pub amount: Cents,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub period: PeriodId,
    pub committed: usize,
    pub share: Cents,
    pub credit_applied: Cents,
    pub net_due: Cents,
    pub fee_estimate: Cents,
    pub gross_due: Cents,
    /// v2: the noticed ceiling for this renewal, if any; `share` never exceeds it.
    #[serde(default)]
    pub ceiling: Option<Cents>,
    /// v2: why this renewal cannot be charged yet.
    #[serde(default)]
    pub blocked: Option<Blocked>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blocked {
    /// No price notice has been issued to this renewer for the period.
    NoticeRequired,
    /// The noticed ceiling is above the renewer's consented cap.
    ConsentRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    SetUpkeep {
        period: PeriodId,
        cents: Cents,
    },
    RecordPayment {
        user: UserId,
        gross: Cents,
        fee: Cents,
        /// v2: processor id of the charge, required when `gross > 0`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        charge_ref: Option<String>,
    },
    Cancel {
        user: UserId,
    },
    Lapse {
        user: UserId,
    },
    Settle {
        actual: Cents,
        evidence: Option<String>,
    },
    RefundCredit {
        user: UserId,
    },
    /// v2: notice every renewer who owes `period` of their price ceiling.
    IssueNotices {
        period: PeriodId,
    },
    /// v2: the subscriber's express consent to renew up to `cents` gross per period.
    SetRenewalCap {
        user: UserId,
        cents: Cents,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    UpkeepSet,
    Paid {
        period: PeriodId,
        contribution: Cents,
        credit_applied: Cents,
    },
    StatusChanged(Status),
    Settled(Box<Settlement>),
    /// v1 refund of the whole credit; the caller picks the charge.
    Refunded(Cents),
    /// v2 refund, split across the subscriber's own earlier charges.
    RefundedTo(Vec<RefundLeg>),
    Noticed(Vec<Notice>),
    CapSet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Engine {
    cfg: Config,
    periods: BTreeMap<PeriodId, Period>,
    subs: BTreeMap<UserId, Subscriber>,
    reserve: Cents,
    cash_in: Cents,
    payouts: Cents,
    refunds: Cents,
    journal: Vec<JournalEntry>,
    /// v2: noticed ceilings per open period and renewer.
    #[serde(default)]
    notices: BTreeMap<PeriodId, BTreeMap<UserId, Cents>>,
}

/// Where the next payment lands, computed without mutating anything.
struct Funding {
    id: PeriodId,
    user_target: Cents,
}

impl Engine {
    pub fn new(cfg: Config, first_upkeep: Cents) -> Result<Self, EngineError> {
        cfg.validate()?;
        check_amount(first_upkeep)?;
        let mut e = Engine {
            cfg,
            periods: BTreeMap::new(),
            subs: BTreeMap::new(),
            reserve: 0,
            cash_in: 0,
            payouts: 0,
            refunds: 0,
            journal: Vec::new(),
            notices: BTreeMap::new(),
        };
        e.periods.insert(1, Period::new(1, first_upkeep, false));
        e.open_period(1, 0);
        Ok(e)
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }
    pub fn reserve(&self) -> Cents {
        self.reserve
    }
    pub fn journal(&self) -> &[JournalEntry] {
        &self.journal
    }
    pub fn periods(&self) -> impl Iterator<Item = &Period> {
        self.periods.values()
    }
    pub fn period(&self, id: PeriodId) -> Option<&Period> {
        self.periods.get(&id)
    }
    pub fn subscriber(&self, user: UserId) -> Option<&Subscriber> {
        self.subs.get(&user)
    }
    pub fn subscribers(&self) -> impl Iterator<Item = (&UserId, &Subscriber)> {
        self.subs.iter()
    }
    pub fn credits_outstanding(&self) -> Cents {
        self.subs.values().map(|s| s.credit).sum()
    }

    /// Collected (and prefunded) for periods that have not settled yet.
    pub fn open_pool(&self) -> Cents {
        self.periods
            .values()
            .filter(|p| p.settled.is_none())
            .map(|p| p.collected() + p.prefund)
            .sum()
    }

    /// What the owner must still hold for subscribers (DESIGN 20.3).
    pub fn required_float(&self) -> Cents {
        self.reserve + self.credits_outstanding() + self.open_pool()
    }

    /// Earliest period that has not settled.
    pub fn current_period(&self) -> PeriodId {
        self.periods
            .values()
            .find(|p| p.settled.is_none())
            .map_or(1, |p| p.id)
    }

    fn v2(&self) -> bool {
        self.cfg.rules_version >= 2
    }

    /// Noticed ceiling for `user` in `period`, if one was issued.
    pub fn notice(&self, period: PeriodId, user: UserId) -> Option<Cents> {
        self.notices.get(&period)?.get(&user).copied()
    }

    pub fn execute(&mut self, cmd: &Command) -> Result<Outcome, EngineError> {
        match cmd {
            Command::SetUpkeep { period, cents } => self.set_upkeep(*period, *cents),
            Command::RecordPayment {
                user,
                gross,
                fee,
                charge_ref,
            } => self.record_payment(*user, *gross, *fee, charge_ref.as_deref()),
            Command::Cancel { user } => self.cancel(*user),
            Command::Lapse { user } => self.lapse(*user),
            Command::Settle { actual, evidence } => self.settle(*actual, evidence.clone()),
            Command::RefundCredit { user } => self.refund_credit(*user),
            Command::IssueNotices { period } => self.issue_notices(*period),
            Command::SetRenewalCap { user, cents } => self.set_renewal_cap(*user, *cents),
        }
    }

    /// Price the next payment by `user` (new or existing) would carry.
    pub fn quote(&self, user: UserId) -> Result<Quote, EngineError> {
        let f = self.funding();
        let contributors = self.periods.get(&f.id).map(|p| &p.contributors);
        if contributors.is_some_and(|c| c.contains(&user)) {
            return Err(EngineError::AlreadyPaid { user, period: f.id });
        }
        // The quote counts the payer. The literal "pay the old price, then drop it"
        // makes the first joiner after a bootstrap pay the whole target and cover a
        // period alone; settlement credits make the net cost identical either way.
        let committed = self.committed(f.id, contributors)
            + usize::from(
                self.subs
                    .get(&user)
                    .is_none_or(|s| s.status != Status::Active),
            );
        let live = ceil_div(f.user_target, committed.max(1) as Cents);
        let sub = self.subs.get(&user);
        // A renewal is an off-session charge of someone already subscribed, which is
        // what notice-of-change and auto-renewal consent rules govern. A lapsed or
        // new subscriber paying is a fresh, on-session purchase at the live quote.
        let renewal =
            self.v2() && sub.is_some_and(|s| s.status == Status::Active && s.paid_through < f.id);
        let (share, ceiling, blocked) = if renewal {
            match self.notice(f.id, user) {
                None => (live, None, Some(Blocked::NoticeRequired)),
                Some(c) => {
                    let cap = sub.and_then(|s| s.renewal_cap);
                    let consented = cap.is_some_and(|cap| cap >= self.cfg.fee.gross_for_net(c));
                    (
                        live.min(c),
                        Some(c),
                        (!consented).then_some(Blocked::ConsentRequired),
                    )
                }
            }
        } else {
            (live, None, None)
        };
        let credit = sub.map_or(0, |s| s.credit);
        let credit_applied = credit.min(share);
        let net_due = share - credit_applied;
        let gross_due = self.cfg.fee.gross_for_net(net_due);
        Ok(Quote {
            period: f.id,
            committed,
            share,
            credit_applied,
            net_due,
            fee_estimate: gross_due - net_due,
            gross_due,
            ceiling,
            blocked,
        })
    }

    /// Checks every conservation invariant; callable at any time.
    pub fn audit(&self) -> Result<(), EngineError> {
        let fail = |m: String| Err(EngineError::Audit(m));
        if self.reserve < 0 {
            return fail(format!("negative reserve {}", self.reserve));
        }
        if let Some((u, s)) = self.subs.iter().find(|(_, s)| s.credit < 0) {
            return fail(format!("negative credit {} for {u}", s.credit));
        }
        let held = self.payouts + self.refunds + self.required_float();
        if held != self.cash_in {
            return fail(format!("cash_in {} != accounted {held}", self.cash_in));
        }
        let sum_kind = |k: EntryKind| -> Cents {
            self.journal
                .iter()
                .filter(|e| e.kind == k)
                .map(|e| e.amount)
                .sum()
        };
        if sum_kind(EntryKind::ContributionCash) != self.cash_in
            || sum_kind(EntryKind::Payout) != self.payouts
            || sum_kind(EntryKind::CreditRefunded) != self.refunds
        {
            return fail("journal totals disagree with account totals".into());
        }
        // Credit only moves between a subscriber and their own pool: what each holds
        // must equal what was issued to them, less what they spent or had refunded.
        let mut per_user: BTreeMap<UserId, Cents> = BTreeMap::new();
        for e in &self.journal {
            let Some(u) = e.user else { continue };
            let delta = match e.kind {
                EntryKind::CreditIssued => e.amount,
                EntryKind::CreditApplied | EntryKind::CreditRefunded => -e.amount,
                _ => continue,
            };
            *per_user.entry(u).or_default() += delta;
        }
        for (u, s) in &self.subs {
            if per_user.get(u).copied().unwrap_or(0) != s.credit {
                return fail(format!("credit of {u} does not reconcile with its journal"));
            }
            if s.charges
                .iter()
                .any(|c| c.refunded < 0 || c.refunded > c.gross)
            {
                return fail(format!("charge of {u} refunded beyond its amount"));
            }
        }
        for p in self.periods.values() {
            let Some(s) = &p.settled else { continue };
            if s.payout > s.actual_upkeep + s.deficit_in {
                return fail(format!(
                    "period {} paid out more than upkeep plus deficit",
                    p.id
                ));
            }
            for r in &s.shares {
                if r.share < 0
                    || r.share > r.contributed
                    || r.credit_issued != r.contributed - r.share
                {
                    return fail(format!("period {} bad share for {}", p.id, r.user));
                }
            }
        }
        Ok(())
    }

    fn funding(&self) -> Funding {
        for p in self.periods.values() {
            if p.settled.is_none() && !self.is_covered(p) {
                return Funding {
                    id: p.id,
                    user_target: p.user_target(),
                };
            }
        }
        // Everything open is covered: payments prefund a not-yet-created period
        // whose estimate defaults to the latest one.
        let last = self
            .periods
            .values()
            .next_back()
            .expect("period 1 always exists");
        Funding {
            id: last.id + 1,
            user_target: last.upkeep,
        }
    }

    /// Active renewers who still owe `id`. Someone who prepaid a later period is
    /// not pending for an earlier one, or every late joiner would reopen it.
    fn pending(&self, id: PeriodId, contributors: Option<&BTreeSet<UserId>>) -> usize {
        self.subs
            .iter()
            .filter(|(u, s)| {
                s.status == Status::Active
                    && s.paid_through < id
                    && !contributors.is_some_and(|c| c.contains(u))
            })
            .count()
    }

    fn committed(&self, id: PeriodId, contributors: Option<&BTreeSet<UserId>>) -> usize {
        contributors.map_or(0, BTreeSet::len) + self.pending(id, contributors)
    }

    /// Covered means the money is in AND every renewer has paid or been lapsed.
    /// Cash alone is not enough: early payers of a falling price overpay, and
    /// treating that overshoot as coverage would push the remaining renewers
    /// and later joiners into a different period than the early payers.
    fn is_covered(&self, p: &Period) -> bool {
        p.collected() >= p.user_target() && self.pending(p.id, Some(&p.contributors)) == 0
    }

    /// The live share for `period` and the renewers who still owe it.
    fn notice_terms(&self, period: PeriodId) -> Result<(Cents, Vec<UserId>), EngineError> {
        let last = *self
            .periods
            .keys()
            .next_back()
            .expect("period 1 always exists");
        let (user_target, contributors) = match self.periods.get(&period) {
            Some(p) if p.settled.is_none() => (p.user_target(), Some(&p.contributors)),
            None if period == last + 1 => (self.periods[&last].upkeep, None),
            _ => return Err(EngineError::PeriodNotOpen(period)),
        };
        let share = ceil_div(
            user_target,
            self.committed(period, contributors).max(1) as Cents,
        );
        let owing = self
            .subs
            .iter()
            .filter(|(u, s)| {
                s.status == Status::Active
                    && s.paid_through < period
                    && !contributors.is_some_and(|c| c.contains(u))
            })
            .map(|(u, _)| *u)
            .collect();
        Ok((share, owing))
    }

    fn issue_notices(&mut self, period: PeriodId) -> Result<Outcome, EngineError> {
        if !self.v2() {
            return Err(EngineError::InvalidState);
        }
        let (share, owing) = self.notice_terms(period)?;
        let mut issued = Vec::new();
        for user in owing {
            // A ceiling, once noticed, is never raised: the subscriber was told it.
            if self.notice(period, user).is_some() {
                continue;
            }
            self.notices.entry(period).or_default().insert(user, share);
            self.push(EntryKind::PriceNotice, Some(period), Some(user), share);
            let ceiling_gross = self.cfg.fee.gross_for_net(share);
            let cap = self.subs[&user].renewal_cap;
            issued.push(Notice {
                user,
                period,
                ceiling: share,
                ceiling_gross,
                consent_needed: cap.is_none_or(|c| c < ceiling_gross),
            });
        }
        Ok(Outcome::Noticed(issued))
    }

    fn set_renewal_cap(&mut self, user: UserId, cents: Cents) -> Result<Outcome, EngineError> {
        if !self.v2() {
            return Err(EngineError::InvalidState);
        }
        check_amount(cents)?;
        let sub = self
            .subs
            .get_mut(&user)
            .ok_or(EngineError::UnknownSubscriber(user))?;
        sub.renewal_cap = Some(cents);
        Ok(Outcome::CapSet)
    }

    /// Refund up to `amount` of `user`'s credit to their own charges, newest
    /// first, never more than any charge's unrefunded amount.
    fn refund_to_charges(
        &mut self,
        user: UserId,
        amount: Cents,
        reason: RefundReason,
        period: Option<PeriodId>,
    ) -> Vec<RefundLeg> {
        let sub = self.subs.get_mut(&user).expect("subscriber exists");
        let mut left = amount.min(sub.credit);
        let mut legs = Vec::new();
        for c in sub.charges.iter_mut().rev() {
            if left == 0 {
                break;
            }
            let take = left.min(c.gross - c.refunded);
            if take > 0 {
                c.refunded += take;
                left -= take;
                legs.push(RefundLeg {
                    user,
                    charge_ref: c.charge_ref.clone(),
                    amount: take,
                    reason,
                });
            }
        }
        let total: Cents = legs.iter().map(|l| l.amount).sum();
        sub.credit -= total;
        self.refunds += total;
        for l in &legs {
            self.push(EntryKind::CreditRefunded, period, Some(user), l.amount);
        }
        legs
    }

    fn push(
        &mut self,
        kind: EntryKind,
        period: Option<PeriodId>,
        user: Option<UserId>,
        amount: Cents,
    ) {
        let seq = self.journal.len() as u64;
        self.journal.push(JournalEntry {
            seq,
            kind,
            period,
            user,
            amount,
        });
    }

    /// Fix a period's deficit, reserve top-up and reserve release. Run once, when its
    /// predecessor settles (or at creation for period 1).
    fn open_period(&mut self, id: PeriodId, deficit: Cents) {
        let upkeep = self.periods[&id].upkeep;
        let cap = ceil_div(upkeep * Cents::from(self.cfg.reserve_cap_bps), 10_000);
        let (topup, prefund) = if self.reserve > cap {
            (0, (self.reserve - cap).min(upkeep + deficit))
        } else {
            let build = ceil_div(upkeep * Cents::from(self.cfg.reserve_surcharge_bps), 10_000);
            ((cap - self.reserve).min(build), 0)
        };
        self.reserve -= prefund;
        if prefund > 0 {
            self.push(EntryKind::ReserveRelease, Some(id), None, prefund);
        }
        if deficit > 0 {
            self.push(EntryKind::DeficitCarried, Some(id), None, deficit);
        }
        let p = self.periods.get_mut(&id).expect("period exists");
        p.deficit = deficit;
        p.topup = topup;
        p.prefund = prefund;
        p.cap = cap;
    }

    fn set_upkeep(&mut self, period: PeriodId, cents: Cents) -> Result<Outcome, EngineError> {
        check_amount(cents)?;
        match self.periods.get_mut(&period) {
            Some(p) if p.settled.is_none() => {
                p.upkeep = cents;
                p.upkeep_is_default = false;
                Ok(Outcome::UpkeepSet)
            }
            _ => Err(EngineError::PeriodNotOpen(period)),
        }
    }

    fn record_payment(
        &mut self,
        user: UserId,
        gross: Cents,
        fee: Cents,
        charge_ref: Option<&str>,
    ) -> Result<Outcome, EngineError> {
        check_amount(gross)?;
        if !(0..=gross).contains(&fee) {
            return Err(EngineError::AmountOutOfRange);
        }
        let charge_ref = if self.v2() && gross > 0 {
            let r = charge_ref.ok_or(EngineError::MissingChargeRef)?;
            if !(1..=255).contains(&r.len()) || r.chars().any(char::is_control) {
                return Err(EngineError::MissingChargeRef);
            }
            Some(r.to_owned())
        } else {
            None
        };
        let q = self.quote(user)?;
        match q.blocked {
            Some(Blocked::NoticeRequired) => return Err(EngineError::NoticeRequired),
            Some(Blocked::ConsentRequired) => return Err(EngineError::ConsentRequired),
            None => {}
        }
        if gross < q.gross_due {
            return Err(EngineError::Underpayment);
        }
        let net = gross - fee;
        let amount = net + q.credit_applied;
        if amount <= 0 {
            return Err(EngineError::ZeroPayment);
        }

        let id = q.period;
        if !self.periods.contains_key(&id) {
            let upkeep = self.periods[&(id - 1)].upkeep;
            self.periods.insert(id, Period::new(id, upkeep, true));
            // Predecessor still open, so deficit/top-up/release arrive when it settles.
        }
        let sub = self.subs.entry(user).or_insert(Subscriber {
            status: Status::Active,
            paid_through: id,
            credit: 0,
            charges: Vec::new(),
            last_share: 0,
            renewal_cap: None,
        });
        if let Some(charge_ref) = charge_ref {
            sub.charges.push(Charge {
                charge_ref,
                gross,
                refunded: 0,
            });
            if sub.charges.len() > MAX_REMEMBERED_CHARGES {
                sub.charges.remove(0);
            }
        }
        sub.credit -= q.credit_applied;
        sub.status = Status::Active;
        sub.paid_through = sub.paid_through.max(id);
        let p = self.periods.get_mut(&id).expect("just ensured");
        p.contributions.push(Contribution {
            user,
            cash_net: net,
            credit_used: q.credit_applied,
        });
        p.contributors.insert(user);
        self.cash_in += net;
        self.push(EntryKind::ContributionCash, Some(id), Some(user), net);
        if q.credit_applied > 0 {
            self.push(
                EntryKind::CreditApplied,
                Some(id),
                Some(user),
                q.credit_applied,
            );
        }
        Ok(Outcome::Paid {
            period: id,
            contribution: amount,
            credit_applied: q.credit_applied,
        })
    }

    fn cancel(&mut self, user: UserId) -> Result<Outcome, EngineError> {
        let current = self.current_period();
        let sub = self
            .subs
            .get_mut(&user)
            .ok_or(EngineError::UnknownSubscriber(user))?;
        sub.status = match sub.status {
            Status::Active if sub.paid_through >= current => Status::Cancelling,
            Status::Active => Status::Ended,
            Status::Cancelling => Status::Cancelling,
            Status::Lapsed | Status::Ended => return Err(EngineError::InvalidState),
        };
        Ok(Outcome::StatusChanged(sub.status))
    }

    fn lapse(&mut self, user: UserId) -> Result<Outcome, EngineError> {
        let current = self.current_period();
        let sub = self
            .subs
            .get_mut(&user)
            .ok_or(EngineError::UnknownSubscriber(user))?;
        if sub.status != Status::Active || sub.paid_through >= current {
            return Err(EngineError::InvalidState);
        }
        sub.status = Status::Lapsed;
        Ok(Outcome::StatusChanged(Status::Lapsed))
    }

    fn refund_credit(&mut self, user: UserId) -> Result<Outcome, EngineError> {
        let amount = self
            .subs
            .get(&user)
            .ok_or(EngineError::UnknownSubscriber(user))?
            .credit;
        if amount == 0 {
            return Err(EngineError::NoCredit);
        }
        if self.v2() {
            // Credit goes back only to the cards that paid it in (DESIGN 20.7 P3).
            let legs = self.refund_to_charges(user, amount, RefundReason::Requested, None);
            if legs.is_empty() {
                return Err(EngineError::NoRefundableCharge);
            }
            return Ok(Outcome::RefundedTo(legs));
        }
        self.subs.get_mut(&user).expect("checked above").credit = 0;
        self.refunds += amount;
        self.push(EntryKind::CreditRefunded, None, Some(user), amount);
        Ok(Outcome::Refunded(amount))
    }

    fn settle(&mut self, actual: Cents, evidence: Option<String>) -> Result<Outcome, EngineError> {
        check_amount(actual)?;
        if evidence.as_ref().is_some_and(|e| e.len() > 512) {
            return Err(EngineError::AmountOutOfRange);
        }
        let id = self.current_period();
        let (deficit, topup, prefund, contributions) = {
            let p = &self.periods[&id];
            (p.deficit, p.topup, p.prefund, p.contributions.clone())
        };

        let owed = actual + deficit;
        let target = owed + topup;
        // Prefund beyond what the settled period needs goes back to the reserve.
        let prefund_used = prefund.min(target);
        let prefund_excess = prefund - prefund_used;
        let amounts: Vec<Cents> = contributions.iter().map(Contribution::amount).collect();
        let shares = water_fill(&amounts, target - prefund_used);

        let pool = shares.iter().sum::<Cents>() + prefund_used;
        let payout_from_pool = pool.min(owed);
        let reserve_topup = pool - payout_from_pool;
        let shortfall = owed - payout_from_pool;
        let reserve_draw = shortfall.min(self.reserve);
        let deficit_out = shortfall - reserve_draw;

        self.reserve = self.reserve - reserve_draw + reserve_topup + prefund_excess;
        self.payouts += payout_from_pool + reserve_draw;

        let mut records = Vec::with_capacity(contributions.len());
        for (c, &share) in contributions.iter().zip(&shares) {
            let credit_issued = c.amount() - share;
            if credit_issued > 0 {
                self.subs
                    .get_mut(&c.user)
                    .expect("contributor exists")
                    .credit += credit_issued;
                self.push(
                    EntryKind::CreditIssued,
                    Some(id),
                    Some(c.user),
                    credit_issued,
                );
            }
            records.push(ShareRecord {
                user: c.user,
                contributed: c.amount(),
                share,
                credit_issued,
            });
        }
        self.push(EntryKind::Payout, Some(id), None, payout_from_pool);
        if reserve_draw > 0 {
            self.push(EntryKind::ReserveDraw, Some(id), None, reserve_draw);
            self.push(EntryKind::Payout, Some(id), None, reserve_draw);
        }
        if reserve_topup + prefund_excess > 0 {
            self.push(
                EntryKind::ReserveTopup,
                Some(id),
                None,
                reserve_topup + prefund_excess,
            );
        }

        for sub in self.subs.values_mut() {
            match sub.status {
                Status::Active if sub.paid_through < id => sub.status = Status::Lapsed,
                Status::Cancelling if sub.paid_through <= id => sub.status = Status::Ended,
                _ => {}
            }
        }

        let mut refunds = Vec::new();
        if self.v2() {
            for r in &records {
                self.subs
                    .get_mut(&r.user)
                    .expect("contributor exists")
                    .last_share = r.share;
            }
            let users: Vec<UserId> = self.subs.keys().copied().collect();
            for u in users {
                let s = &self.subs[&u];
                let mut cap = self
                    .cfg
                    .credit_cap_floor
                    .max(s.last_share.saturating_mul(2));
                if self.cfg.rules_version >= 3 {
                    cap = cap.min(self.cfg.credit_cap_ceiling);
                }
                let dormant = matches!(s.status, Status::Lapsed | Status::Ended)
                    && id >= s.paid_through + self.cfg.dormant_after_periods;
                let (amount, reason) = if dormant {
                    (s.credit, RefundReason::Dormant)
                } else {
                    (s.credit - cap, RefundReason::OverCap)
                };
                if amount > 0 {
                    refunds.extend(self.refund_to_charges(u, amount, reason, Some(id)));
                }
            }
            self.notices.retain(|p, _| *p > id);
        }

        let settlement = Settlement {
            actual_upkeep: actual,
            deficit_in: deficit,
            evidence,
            target,
            shares: records,
            payout: payout_from_pool + reserve_draw,
            reserve_draw,
            reserve_topup,
            deficit_out,
            refunds,
        };
        self.periods.get_mut(&id).expect("current exists").settled = Some(settlement.clone());

        let next = id + 1;
        match self.periods.get_mut(&next) {
            Some(p) if p.upkeep_is_default => p.upkeep = actual,
            Some(_) => {}
            None => {
                self.periods.insert(next, Period::new(next, actual, true));
            }
        }
        self.open_period(next, deficit_out);
        Ok(Outcome::Settled(Box::new(settlement)))
    }
}

fn check_amount(c: Cents) -> Result<(), EngineError> {
    if (0..=MAX_CENTS).contains(&c) {
        Ok(())
    } else {
        Err(EngineError::AmountOutOfRange)
    }
}

impl Engine {
    /// Fee model shortcut for callers that need to price a fee for a given gross.
    pub fn fee_model(&self) -> &FeeModel {
        &self.cfg.fee
    }
}
