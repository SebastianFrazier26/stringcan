#![allow(dead_code)]
use stringcan_core::{Command, Config, Engine, EngineError, FeeModel, Quote, UserId};

pub fn cfg_zero_fee() -> Config {
    Config {
        fee: FeeModel::ZERO,
        reserve_cap_bps: 5_000,
        reserve_surcharge_bps: 0,
    }
}

pub fn pay(e: &mut Engine, user: UserId) -> Result<Quote, EngineError> {
    let q = e.quote(user)?;
    let fee = e.fee_model().fee(q.gross_due);
    e.execute(&Command::RecordPayment {
        user,
        gross: q.gross_due,
        fee,
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
        stringcan_core::Outcome::Settled(s) => *s,
        o => panic!("unexpected {o:?}"),
    }
}

pub fn share_of(s: &stringcan_core::Settlement, user: UserId) -> i64 {
    s.shares.iter().find(|r| r.user == user).unwrap().share
}
