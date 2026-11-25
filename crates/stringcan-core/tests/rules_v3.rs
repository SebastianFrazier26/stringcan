//! Rules v3 (2026-11-23): an absolute credit ceiling at or under the US $2,000
//! closed-loop prepaid-access limit, on top of the v2 cap (DESIGN 20.7).
mod common;

use common::{credit, pay_as_is, settle};
use stringcan_core::{
    Config, Engine, FeeModel, RefundReason, Settlement, CLOSED_LOOP_LIMIT, CURRENT_RULES,
};

/// User 1 bootstraps period 1; user 2 joins period 2 at half the upkeep, then
/// nine more join, so user 2's credit at settlement is far above two shares
/// when upkeep is large.
fn big_overpayer(cfg: Config) -> (Engine, Settlement) {
    let mut e = Engine::new(cfg, 2_000_000).unwrap();
    pay_as_is(&mut e, 1).unwrap();
    pay_as_is(&mut e, 2).unwrap();
    for u in 3..=11 {
        pay_as_is(&mut e, u).unwrap();
    }
    settle(&mut e, 2_000_000);
    let s = settle(&mut e, 2_000_000);
    (e, s)
}

fn zero_fee(cfg: Config) -> Config {
    Config {
        fee: FeeModel::ZERO,
        ..cfg
    }
}

#[test]
fn new_tenants_get_v3_with_a_five_hundred_dollar_ceiling() {
    let c = Config::default();
    assert_eq!(c.rules_version, CURRENT_RULES);
    assert_eq!(CURRENT_RULES, 3);
    assert_eq!(c.credit_cap_ceiling, 50_000);
    assert!(c.credit_cap_floor <= c.credit_cap_ceiling);
    c.validate().unwrap();
    assert!(c.credit_cap_ceiling <= CLOSED_LOOP_LIMIT);
}

#[test]
fn credit_never_stays_above_the_ceiling_even_when_two_shares_exceed_it() {
    let (e, s) = big_overpayer(zero_fee(Config::default()));
    let rec = s.shares.iter().find(|r| r.user == 2).unwrap();
    assert!(
        2 * rec.share > CLOSED_LOOP_LIMIT,
        "scenario must exceed $2,000"
    );
    let legs: Vec<_> = s.refunds.iter().filter(|l| l.user == 2).collect();
    assert_eq!(legs.len(), 1);
    assert_eq!(legs[0].reason, RefundReason::OverCap);
    assert_eq!(legs[0].amount, rec.credit_issued - 50_000);
    assert_eq!(credit(&e, 2), 50_000);
    assert!(e.subscribers().all(|(_, s)| s.credit <= 50_000));
    e.audit().unwrap();
}

#[test]
fn v2_tenants_replay_without_the_ceiling() {
    let (e, s) = big_overpayer(zero_fee(Config::v2()));
    let rec = s.shares.iter().find(|r| r.user == 2).unwrap();
    assert_eq!(credit(&e, 2), 2 * rec.share);
    assert!(credit(&e, 2) > CLOSED_LOOP_LIMIT);
    e.audit().unwrap();
}

#[test]
fn the_ceiling_must_sit_between_the_floor_and_the_closed_loop_limit() {
    let above = Config {
        credit_cap_ceiling: CLOSED_LOOP_LIMIT + 1,
        ..Config::default()
    };
    assert!(above.validate().is_err());
    let below_floor = Config {
        credit_cap_ceiling: 9_999,
        ..Config::default()
    };
    assert!(below_floor.validate().is_err());
    // The floor may not rise above the $500 default ceiling unless the ceiling rises too.
    let floor_above_ceiling = Config {
        credit_cap_floor: 50_001,
        ..Config::default()
    };
    assert!(floor_above_ceiling.validate().is_err());
    let floor_at_ceiling = Config {
        credit_cap_floor: 50_000,
        ..Config::default()
    };
    floor_at_ceiling.validate().unwrap();
    let at_limit = Config {
        credit_cap_ceiling: CLOSED_LOOP_LIMIT,
        ..Config::default()
    };
    at_limit.validate().unwrap();
    // A v2 config ignores the field, so a stored value cannot break replay.
    let v2 = Config {
        credit_cap_ceiling: CLOSED_LOOP_LIMIT + 1,
        ..Config::v2()
    };
    v2.validate().unwrap();
}
