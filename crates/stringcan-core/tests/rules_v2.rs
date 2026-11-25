//! Rules v2 (2026-11-23): noticed price ceilings with a consented renewal cap
//! (DESIGN 20.8, option N3) and credit safeguards P3 to P6 (DESIGN 20.7).
mod common;

use common::{cfg_zero_fee_v2, credit, pay, pay_as_is, settle};
use stringcan_core::{
    Blocked, Command, Config, Engine, EngineError, EntryKind, Outcome, RefundReason, Status,
    MAX_REMEMBERED_CHARGES,
};

fn notices(e: &mut Engine, period: u32) -> Vec<stringcan_core::Notice> {
    match e.execute(&Command::IssueNotices { period }).unwrap() {
        Outcome::Noticed(n) => n,
        o => panic!("unexpected {o:?}"),
    }
}

fn cap(e: &mut Engine, user: u64, cents: i64) {
    e.execute(&Command::SetRenewalCap { user, cents }).unwrap();
}

/// Period 1 bootstrapped by user 1 and settled; users 1..=n are renewers of period 2
/// who have each prefunded nothing yet (only user 1 has paid anything).
fn renewers(n: u64, cfg: Config) -> Engine {
    let mut e = Engine::new(cfg, 1000).unwrap();
    pay(&mut e, 1).unwrap();
    for u in 2..=n {
        pay(&mut e, u).unwrap();
    }
    settle(&mut e, 1000);
    e
}

#[test]
fn a_renewal_needs_a_notice_and_consent_but_a_joiner_does_not() {
    let mut e = Engine::new(cfg_zero_fee_v2(), 1000).unwrap();
    pay_as_is(&mut e, 1).unwrap();
    settle(&mut e, 1000);

    let before = e.clone();
    assert_eq!(e.quote(1).unwrap().blocked, Some(Blocked::NoticeRequired));
    assert_eq!(pay_as_is(&mut e, 1), Err(EngineError::NoticeRequired));
    assert_eq!(e, before, "a blocked renewal changes nothing");
    assert_eq!(
        e.quote(2).unwrap().blocked,
        None,
        "a new joiner is on-session"
    );

    let n = notices(&mut e, 2);
    assert_eq!(n.len(), 1);
    assert!(n[0].consent_needed, "no cap on file yet");
    assert_eq!(pay_as_is(&mut e, 1), Err(EngineError::ConsentRequired));
    cap(&mut e, 1, 999);
    assert_eq!(pay_as_is(&mut e, 1), Err(EngineError::ConsentRequired));
    cap(&mut e, 1, 1000);
    assert_eq!(pay_as_is(&mut e, 1).unwrap().gross_due, 1000);
    assert!(e
        .journal()
        .iter()
        .any(|j| j.kind == EntryKind::PriceNotice && j.user == Some(1) && j.amount == 1000));
}

#[test]
fn the_charge_is_capped_at_the_ceiling_and_the_gap_is_carried() {
    // Period 1: user 1 alone. Users 2 to 4 prefund period 2; user 1 renews it.
    let mut e = Engine::new(cfg_zero_fee_v2(), 1000).unwrap();
    for u in 1..=4 {
        pay_as_is(&mut e, u).unwrap();
    }
    settle(&mut e, 1000);
    pay(&mut e, 1).unwrap();
    settle(&mut e, 1000);

    let n = notices(&mut e, 3);
    assert_eq!(n.len(), 4);
    assert!(n.iter().all(|x| x.ceiling == 250));
    for u in 1..=4 {
        cap(&mut e, u, 250);
    }
    // Two renewers leave after the notice: the live share doubles, the charge does not.
    e.execute(&Command::Cancel { user: 3 }).unwrap();
    e.execute(&Command::Cancel { user: 4 }).unwrap();
    for u in [1, 2] {
        let q = pay_as_is(&mut e, u).unwrap();
        assert_eq!((q.share, q.ceiling), (250, Some(250)));
    }
    let s = settle(&mut e, 1000);
    assert_eq!(s.deficit_out, 500, "the shortfall is carried, not charged");
    assert_eq!(e.period(4).unwrap().deficit, 500);
    e.audit().unwrap();
}

#[test]
fn a_ceiling_once_noticed_is_never_raised() {
    let mut e = renewers(2, cfg_zero_fee_v2());
    let first = notices(&mut e, 2);
    assert!(
        notices(&mut e, 2).is_empty(),
        "re-running notices nobody twice"
    );
    e.execute(&Command::SetUpkeep {
        period: 2,
        cents: 5000,
    })
    .unwrap();
    for x in &first {
        assert_eq!(e.notice(2, x.user), Some(x.ceiling));
    }
}

#[test]
fn credit_is_refunded_only_to_the_subscribers_own_charges() {
    let mut e = Engine::new(cfg_zero_fee_v2(), 1000).unwrap();
    pay_as_is(&mut e, 1).unwrap();
    pay_as_is(&mut e, 2).unwrap(); // prefunds period 2 at 500
    for u in 3..=10 {
        pay_as_is(&mut e, u).unwrap();
    }
    settle(&mut e, 1000);
    settle(&mut e, 1000);
    let c = credit(&e, 2);
    assert!(c > 0);
    let Outcome::RefundedTo(legs) = e.execute(&Command::RefundCredit { user: 2 }).unwrap() else {
        panic!("v2 refunds name charges");
    };
    let mine: Vec<String> = e
        .subscriber(2)
        .unwrap()
        .charges
        .iter()
        .map(|c| c.charge_ref.clone())
        .collect();
    assert!(legs
        .iter()
        .all(|l| l.user == 2 && mine.contains(&l.charge_ref)));
    assert_eq!(legs.iter().map(|l| l.amount).sum::<i64>(), c);
    assert!(legs.iter().all(|l| l.reason == RefundReason::Requested));
    assert_eq!(
        e.execute(&Command::RefundCredit { user: 2 }),
        Err(EngineError::NoCredit)
    );
    e.audit().unwrap();
}

#[test]
fn credit_above_the_cap_is_refunded_at_settlement() {
    let cfg = Config {
        credit_cap_floor: 0,
        ..cfg_zero_fee_v2()
    };
    let mut e = Engine::new(cfg, 1000).unwrap();
    pay_as_is(&mut e, 1).unwrap();
    pay_as_is(&mut e, 2).unwrap(); // 500 for period 2
    for u in 3..=11 {
        pay_as_is(&mut e, u).unwrap();
    }
    settle(&mut e, 1000);
    let s = settle(&mut e, 1000);
    let share2 = s.shares.iter().find(|r| r.user == 2).unwrap();
    let issued = share2.credit_issued;
    let cap = 2 * share2.share;
    assert!(issued > cap);
    let over: Vec<_> = s.refunds.iter().filter(|l| l.user == 2).collect();
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].reason, RefundReason::OverCap);
    assert_eq!(over[0].amount, issued - cap);
    assert_eq!(credit(&e, 2), cap);
    e.audit().unwrap();
}

#[test]
fn default_cap_is_a_hundred_dollars_or_two_shares() {
    let e = Engine::new(Config::default(), 1000).unwrap();
    assert_eq!(e.config().credit_cap_floor, 10_000);
}

#[test]
fn dormant_credit_is_swept_back_to_the_card() {
    let cfg = Config {
        dormant_after_periods: 2,
        ..cfg_zero_fee_v2()
    };
    let mut e = Engine::new(cfg, 1000).unwrap();
    pay_as_is(&mut e, 1).unwrap();
    pay_as_is(&mut e, 2).unwrap();
    pay_as_is(&mut e, 3).unwrap();
    settle(&mut e, 1000); // period 1
    e.execute(&Command::Cancel { user: 2 }).unwrap();
    settle(&mut e, 600); // period 2: 2 and 3 overpaid, credit issued
    assert_eq!(e.subscriber(2).unwrap().status, Status::Ended);
    let held = credit(&e, 2);
    assert!(held > 0);
    pay(&mut e, 3).unwrap();
    let s3 = settle(&mut e, 900); // period 3: one period after user 2's last
    assert!(s3.refunds.iter().all(|l| l.user != 2));
    pay(&mut e, 3).unwrap();
    let s4 = settle(&mut e, 900); // period 4 = paid_through 2 + 2
    let swept: Vec<_> = s4.refunds.iter().filter(|l| l.user == 2).collect();
    assert_eq!(swept.iter().map(|l| l.amount).sum::<i64>(), held);
    assert!(swept.iter().all(|l| l.reason == RefundReason::Dormant));
    assert_eq!(credit(&e, 2), 0);
    e.audit().unwrap();
}

#[test]
fn only_recent_charges_are_remembered_and_a_charge_id_is_required() {
    let mut e = Engine::new(cfg_zero_fee_v2(), 100).unwrap();
    for _ in 0..(MAX_REMEMBERED_CHARGES + 3) {
        pay(&mut e, 1).unwrap();
        settle(&mut e, 100);
    }
    assert_eq!(
        e.subscriber(1).unwrap().charges.len(),
        MAX_REMEMBERED_CHARGES
    );
    let mut fresh = Engine::new(cfg_zero_fee_v2(), 100).unwrap();
    assert_eq!(
        fresh.execute(&Command::RecordPayment {
            user: 9,
            gross: 100,
            fee: 0,
            charge_ref: None,
        }),
        Err(EngineError::MissingChargeRef)
    );
}

#[test]
fn v1_tenants_keep_v1_rules() {
    let stored =
        r#"{"fee":{"bps":290,"fixed":30},"reserve_cap_bps":5000,"reserve_surcharge_bps":0}"#;
    let cfg: Config = serde_json::from_str(stored).unwrap();
    assert_eq!(cfg.rules_version, 1);
    let mut e = Engine::new(cfg, 1000).unwrap();
    assert_eq!(
        e.execute(&Command::IssueNotices { period: 1 }),
        Err(EngineError::InvalidState)
    );
    pay(&mut e, 1).unwrap();
    settle(&mut e, 1000);
    assert_eq!(
        e.quote(1).unwrap().blocked,
        None,
        "v1 renews without notice"
    );
    let v1_payment = r#"{"RecordPayment":{"user":1,"gross":5,"fee":0}}"#;
    let cmd: Command = serde_json::from_str(v1_payment).unwrap();
    assert_eq!(serde_json::to_string(&cmd).unwrap(), v1_payment);
}
