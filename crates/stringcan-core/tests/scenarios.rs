mod common;

use common::{cfg_zero_fee, pay, settle, share_of};
use stringcan_core::{Command, Config, Engine, EngineError, EntryKind, FeeModel, Status};

fn stripe_fee_v1() -> Config {
    Config::v1()
}

/// Ten subscribers join a fresh U=1000 app, then period 2 settles.
fn ten_subscriber_engine() -> Engine {
    let mut e = Engine::new(cfg_zero_fee(), 1000).unwrap();
    for u in 1..=10 {
        pay(&mut e, u).unwrap();
    }
    settle(&mut e, 1000);
    pay(&mut e, 1).unwrap();
    settle(&mut e, 1000);
    for u in 1..=10 {
        if e.subscriber(u).unwrap().credit > 0 {
            e.execute(&Command::RefundCredit { user: u }).unwrap();
        }
    }
    e.audit().unwrap();
    e
}

#[test]
fn bootstrap_converges_to_an_even_split() {
    let mut e = Engine::new(cfg_zero_fee(), 1000).unwrap();
    let first = pay(&mut e, 1).unwrap();
    assert_eq!((first.period, first.share), (1, 1000));
    for u in 2..=10 {
        assert_eq!(
            pay(&mut e, u).unwrap().period,
            2,
            "period 1 is covered by the first payer"
        );
    }
    pay(&mut e, 1).unwrap();
    let s1 = settle(&mut e, 1000);
    assert_eq!(s1.shares.len(), 1);
    let s2 = settle(&mut e, 1000);
    assert_eq!(s2.shares.len(), 10);
    for u in 1..=10 {
        assert_eq!(share_of(&s2, u), 100, "user {u}");
    }
    assert_eq!(s2.payout, 1000);
    e.audit().unwrap();
}

#[test]
fn the_ten_dollar_example_each_pays_a_dollar() {
    let mut e = ten_subscriber_engine();
    let quotes: Vec<_> = (1..=10).map(|u| pay(&mut e, u).unwrap()).collect();
    assert!(quotes.iter().all(|q| q.share == 100 && q.period == 3));
    let s = settle(&mut e, 1000);
    assert!((1..=10).all(|u| share_of(&s, u) == 100));
    assert!(s.shares.iter().all(|r| r.credit_issued == 0));
}

#[test]
fn early_joiner_lowers_the_price_and_early_payers_get_credit() {
    let mut e = ten_subscriber_engine();
    for u in 1..=4 {
        assert_eq!(pay(&mut e, u).unwrap().share, 100);
    }
    let joiner = pay(&mut e, 11).unwrap();
    assert_eq!((joiner.period, joiner.share), (3, 91));
    for u in 5..=10 {
        assert_eq!(
            pay(&mut e, u).unwrap().share,
            91,
            "price dropped for the rest"
        );
    }
    let s = settle(&mut e, 1000);
    assert_eq!(s.shares.iter().map(|r| r.share).sum::<i64>(), 1000);
    let (lo, hi) = s
        .shares
        .iter()
        .fold((i64::MAX, 0), |(l, h), r| (l.min(r.share), h.max(r.share)));
    assert!(hi - lo <= 1, "even to within a cent: {lo}..{hi}");
    assert_eq!(
        s.shares.iter().map(|r| r.credit_issued).sum::<i64>(),
        4 * 100 + 7 * 91 - 1000
    );
    for u in 1..=4 {
        assert!(e.subscriber(u).unwrap().credit >= 9);
    }
    e.audit().unwrap();
}

#[test]
fn joiner_after_coverage_prefunds_the_next_period() {
    let mut e = ten_subscriber_engine();
    for u in 1..=10 {
        pay(&mut e, u).unwrap();
    }
    let q = e.quote(11).unwrap();
    assert_eq!((q.period, q.committed, q.share), (4, 11, 91));
    pay(&mut e, 11).unwrap();
    assert_eq!(e.subscriber(11).unwrap().paid_through, 4);
    settle(&mut e, 1000);
    assert_eq!(
        e.subscriber(11).unwrap().status,
        Status::Active,
        "prepaid joiner survives period 3 settling"
    );
}

#[test]
fn paying_at_the_start_or_the_end_costs_the_same_net() {
    let mut e = Engine::new(cfg_zero_fee(), 1200).unwrap();
    pay(&mut e, 1).unwrap();
    for u in 2..=6 {
        pay(&mut e, u).unwrap();
    }
    settle(&mut e, 1200);
    // Users 2..=6 prepaid period 2 at falling prices; user 1 pays last.
    pay(&mut e, 1).unwrap();
    let s = settle(&mut e, 1200);
    let shares: Vec<_> = s.shares.iter().map(|r| r.share).collect();
    assert!(
        shares.iter().max().unwrap() - shares.iter().min().unwrap() <= 1,
        "{shares:?}"
    );
}

#[test]
fn nonpayers_lapse_and_the_price_rises_for_the_rest() {
    let mut e = ten_subscriber_engine();
    for u in 1..=7 {
        pay(&mut e, u).unwrap();
    }
    for u in 8..=10 {
        e.execute(&Command::Lapse { user: u }).unwrap();
    }
    assert_eq!(
        e.quote(11).unwrap().share,
        125,
        "1000 / (7 payers + joiner)"
    );
    let s = settle(&mut e, 1000);
    // 7 payers at 100 each fall short of 1000 by 300; no reserve, so it carries.
    assert_eq!(s.deficit_out, 300);
    assert_eq!(e.period(4).unwrap().deficit, 300);
    assert_eq!(e.subscriber(8).unwrap().status, Status::Lapsed);
}

#[test]
fn reserve_pays_a_shortfall_before_any_deficit_is_carried() {
    let cfg = Config {
        reserve_surcharge_bps: 1_000,
        ..cfg_zero_fee()
    };
    let mut e = Engine::new(cfg, 1000).unwrap();
    assert_eq!(e.period(1).unwrap().topup, 100);
    pay(&mut e, 1).unwrap();
    let s1 = settle(&mut e, 1000);
    assert_eq!(s1.reserve_topup, 100);
    assert_eq!(e.reserve(), 100);

    pay(&mut e, 1).unwrap();
    let s2 = settle(&mut e, 1250);
    assert_eq!(s2.reserve_draw, 100);
    assert_eq!(
        s2.deficit_out,
        1250 - 1100 - 100,
        "owed 1250, pool 1100 (incl. top-up target), reserve 100"
    );
    assert_eq!(e.period(3).unwrap().deficit, s2.deficit_out);
    e.audit().unwrap();
}

#[test]
fn reserve_above_the_cap_is_released_as_prefund() {
    let cfg = Config {
        reserve_cap_bps: 1_000,
        reserve_surcharge_bps: 1_000,
        ..cfg_zero_fee()
    };
    let mut e = Engine::new(cfg, 1000).unwrap();
    pay(&mut e, 1).unwrap();
    settle(&mut e, 1000);
    assert_eq!(e.reserve(), 100);
    pay(&mut e, 1).unwrap();
    settle(&mut e, 500);
    let p3 = e.period(3).unwrap();
    assert_eq!((p3.prefund, p3.upkeep), (50, 500));
    assert_eq!(e.reserve(), 50);
    assert!(e
        .journal()
        .iter()
        .any(|j| j.kind == EntryKind::ReserveRelease && j.amount == 50));
    e.audit().unwrap();
}

#[test]
fn actual_below_estimate_is_credited_back_not_kept() {
    let mut e = Engine::new(cfg_zero_fee(), 1000).unwrap();
    pay(&mut e, 1).unwrap();
    let s = settle(&mut e, 800);
    assert_eq!(
        (s.payout, s.shares[0].share, s.shares[0].credit_issued),
        (800, 800, 200)
    );
    assert_eq!(e.subscriber(1).unwrap().credit, 200);
}

#[test]
fn credit_is_spent_before_cash_and_can_cover_a_whole_share() {
    let mut e = Engine::new(stripe_fee_v1(), 1000).unwrap();
    pay(&mut e, 1).unwrap();
    settle(&mut e, 400);
    let credit = e.subscriber(1).unwrap().credit;
    assert!(credit > 0);
    let q = e.quote(1).unwrap();
    assert_eq!(q.credit_applied, credit.min(q.share));
    pay(&mut e, 1).unwrap();
    e.audit().unwrap();
}

#[test]
fn fees_are_grossed_up_so_the_pool_receives_the_share() {
    let mut e = Engine::new(stripe_fee_v1(), 1000).unwrap();
    let q = e.quote(1).unwrap();
    assert_eq!(q.share, 1000);
    assert!(q.gross_due > 1000);
    assert_eq!(FeeModel::STRIPE_US_CARD.net(q.gross_due), 1000);
    pay(&mut e, 1).unwrap();
    let s = settle(&mut e, 1000);
    assert_eq!((s.payout, s.shares[0].credit_issued), (1000, 0));
}

#[test]
fn cancelling_keeps_access_then_ends_and_leaves_the_price() {
    let mut e = ten_subscriber_engine();
    for u in 1..=10 {
        pay(&mut e, u).unwrap();
    }
    e.execute(&Command::Cancel { user: 10 }).unwrap();
    assert_eq!(e.subscriber(10).unwrap().status, Status::Cancelling);
    settle(&mut e, 1000);
    assert_eq!(e.subscriber(10).unwrap().status, Status::Ended);
    assert_eq!(
        e.quote(1).unwrap().committed,
        9,
        "next period is split nine ways"
    );
}

#[test]
fn credit_refund_pays_out_once() {
    let mut e = Engine::new(cfg_zero_fee(), 1000).unwrap();
    pay(&mut e, 1).unwrap();
    settle(&mut e, 700);
    assert!(matches!(
        e.execute(&Command::RefundCredit { user: 1 }),
        Ok(stringcan_core::Outcome::Refunded(300))
    ));
    assert_eq!(
        e.execute(&Command::RefundCredit { user: 1 }),
        Err(EngineError::NoCredit)
    );
    e.audit().unwrap();
}

#[test]
fn paying_twice_in_one_period_is_rejected() {
    let mut e = ten_subscriber_engine();
    pay(&mut e, 1).unwrap();
    assert!(matches!(e.quote(1), Err(EngineError::AlreadyPaid { .. })));
}

#[test]
fn underpayment_and_bad_amounts_are_rejected_without_side_effects() {
    let mut e = Engine::new(cfg_zero_fee(), 1000).unwrap();
    let before = e.clone();
    assert_eq!(
        e.execute(&Command::RecordPayment {
            user: 1,
            gross: 999,
            fee: 0,
            charge_ref: None,
        }),
        Err(EngineError::Underpayment)
    );
    assert_eq!(
        e.execute(&Command::RecordPayment {
            user: 1,
            gross: -1,
            fee: 0,
            charge_ref: None,
        }),
        Err(EngineError::AmountOutOfRange)
    );
    assert_eq!(
        e.execute(&Command::Cancel { user: 9 }),
        Err(EngineError::UnknownSubscriber(9))
    );
    assert_eq!(e, before);
}

#[test]
fn replaying_commands_reproduces_the_state() {
    let cmds = vec![
        Command::RecordPayment {
            user: 1,
            gross: 1000,
            fee: 0,
            charge_ref: None,
        },
        Command::RecordPayment {
            user: 2,
            gross: 500,
            fee: 0,
            charge_ref: None,
        },
        Command::Settle {
            actual: 900,
            evidence: Some("inv-1".into()),
        },
        Command::RecordPayment {
            user: 1,
            gross: 500,
            fee: 0,
            charge_ref: None,
        },
        Command::Settle {
            actual: 1200,
            evidence: None,
        },
    ];
    let run = || {
        let mut e = Engine::new(cfg_zero_fee(), 1000).unwrap();
        for c in &cmds {
            let _ = e.execute(c);
        }
        e
    };
    assert_eq!(run(), run());
    let json = serde_json::to_string(&cmds).unwrap();
    assert_eq!(serde_json::from_str::<Vec<Command>>(&json).unwrap(), cmds);
}
