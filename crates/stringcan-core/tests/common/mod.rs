#![allow(dead_code)]
use stringcan_core::{
    Blocked, Cents, Command, Config, Engine, EngineError, FeeModel, Outcome, Quote, UserId,
    MAX_CENTS,
};

/// The original (v1) rules; the older scenario tests pin them so v1 tenants
/// keep replaying exactly as they did.
pub fn cfg_zero_fee() -> Config {
    Config {
        fee: FeeModel::ZERO,
        reserve_cap_bps: 5_000,
        reserve_surcharge_bps: 0,
        ..Config::v1()
    }
}

pub fn cfg_zero_fee_v2() -> Config {
    Config {
        fee: FeeModel::ZERO,
        ..Config::default()
    }
}

/// Pays the current quote with a fresh charge id. Under v2 a renewer who has
/// no notice yet is noticed first and consents to any price, which models a
/// subscriber who accepts every renewal; use `pay_as_is` to see the blocks.
pub fn pay(e: &mut Engine, user: UserId) -> Result<Quote, EngineError> {
    let q = e.quote(user)?;
    match q.blocked {
        Some(Blocked::NoticeRequired) => {
            e.execute(&Command::IssueNotices { period: q.period })?;
            return pay(e, user);
        }
        Some(Blocked::ConsentRequired) => {
            e.execute(&Command::SetRenewalCap {
                user,
                cents: MAX_CENTS,
            })?;
            return pay(e, user);
        }
        None => {}
    }
    pay_as_is(e, user)
}

pub fn pay_as_is(e: &mut Engine, user: UserId) -> Result<Quote, EngineError> {
    let q = e.quote(user)?;
    let fee = e.fee_model().fee(q.gross_due);
    e.execute(&Command::RecordPayment {
        user,
        gross: q.gross_due,
        fee,
        charge_ref: Some(format!("pi_{}_{}", user, e.journal().len())),
    })?;
    Ok(q)
}

pub fn settle(e: &mut Engine, actual: i64) -> stringcan_core::Settlement {
    match e
        .execute(&Command::Settle {
            actual,
            evidence: None,
        })
        .unwrap()
    {
        Outcome::Settled(s) => *s,
        o => panic!("unexpected {o:?}"),
    }
}

pub fn share_of(s: &stringcan_core::Settlement, user: UserId) -> i64 {
    s.shares.iter().find(|r| r.user == user).unwrap().share
}

pub fn credit(e: &Engine, user: UserId) -> Cents {
    e.subscriber(user).map_or(0, |s| s.credit)
}
