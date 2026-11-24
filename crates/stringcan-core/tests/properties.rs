#![allow(clippy::single_match_else)]
mod common;

use common::pay;
use proptest::prelude::*;
use stringcan_core::{
    water_fill, Cents, Command, Config, Engine, FeeModel, Outcome, Status, CURRENT_RULES, RULES_V1,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn water_fill_invariants(
        contribs in proptest::collection::vec(0i64..20_000, 0..40),
        target in 0i64..400_000,
    ) {
        let shares = water_fill(&contribs, target);
        prop_assert_eq!(shares.len(), contribs.len());
        let total: Cents = contribs.iter().sum();
        for (s, c) in shares.iter().zip(&contribs) {
            prop_assert!(*s >= 0 && s <= c);
        }
        if total <= target {
            prop_assert_eq!(&shares, &contribs);
        } else {
            prop_assert_eq!(shares.iter().sum::<Cents>(), target);
            let uncapped: Vec<Cents> = shares.iter().zip(&contribs).filter(|(s, c)| s < c).map(|(s, _)| *s).collect();
            if let (Some(lo), Some(hi)) = (uncapped.iter().min(), uncapped.iter().max()) {
                prop_assert!(hi - lo <= 1);
                for (s, c) in shares.iter().zip(&contribs) {
                    if s == c {
                        prop_assert!(*c <= lo + 1);
                    }
                }
            }
        }
    }

    #[test]
    fn gross_for_net_is_sufficient_and_minimal(bps in 0u32..3_000, fixed in 0i64..200, net in 1i64..2_000_000_000) {
        let m = FeeModel { bps, fixed };
        let g = m.gross_for_net(net);
        prop_assert!(m.net(g) >= net);
        prop_assert!(m.net(g - 1) < net);
    }
}

#[derive(Clone, Debug)]
enum Op {
    Join(u64, i64),
    Cancel(u64),
    Lapse(u64),
    Settle(i64),
    SetUpkeep(i64),
    Refund(u64),
    Notice,
    Cap(u64, i64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u64..14, 0i64..4).prop_map(|(u, j)| Op::Join(u, j)),
        1 => (0u64..14).prop_map(Op::Cancel),
        2 => (0u64..14).prop_map(Op::Lapse),
        2 => (0i64..4_000).prop_map(Op::Settle),
        1 => (0i64..4_000).prop_map(Op::SetUpkeep),
        1 => (0u64..14).prop_map(Op::Refund),
        2 => Just(Op::Notice),
        2 => (0u64..14, 0i64..3_000).prop_map(|(u, c)| Op::Cap(u, c)),
    ]
}

fn config() -> impl Strategy<Value = Config> {
    (
        prop_oneof![Just(FeeModel::ZERO), Just(FeeModel::STRIPE_US_CARD)],
        prop_oneof![Just(0u32), Just(1_000), Just(5_000), Just(20_000)],
        prop_oneof![Just(0u32), Just(500), Just(1_500)],
        prop_oneof![Just(RULES_V1), Just(CURRENT_RULES)],
        prop_oneof![Just(0i64), Just(150), Just(10_000)],
        1u32..4,
    )
        .prop_map(|(fee, cap, sur, rules, floor, dormant)| Config {
            fee,
            reserve_cap_bps: cap,
            reserve_surcharge_bps: sur,
            rules_version: rules,
            credit_cap_floor: floor,
            dormant_after_periods: dormant,
        })
}

fn command(engine: &Engine, op: &Op, seq: usize) -> Option<Command> {
    Some(match *op {
        Op::Join(u, jitter) => {
            let q = engine.quote(u).ok()?;
            Command::RecordPayment {
                user: u,
                gross: q.gross_due,
                fee: (engine.fee_model().fee(q.gross_due) + jitter).min(q.gross_due),
                charge_ref: Some(format!("pi_{seq}")),
            }
        }
        Op::Cancel(u) => Command::Cancel { user: u },
        Op::Lapse(u) => Command::Lapse { user: u },
        Op::Settle(a) => Command::Settle {
            actual: a,
            evidence: None,
        },
        Op::SetUpkeep(c) => Command::SetUpkeep {
            period: engine.current_period(),
            cents: c,
        },
        Op::Refund(u) => Command::RefundCredit { user: u },
        Op::Notice => Command::IssueNotices {
            period: engine
                .quote(u64::MAX)
                .map_or(engine.current_period(), |q| q.period),
        },
        Op::Cap(u, c) => Command::SetRenewalCap { user: u, cents: c },
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn engine_invariants_hold_under_random_histories(
        cfg in config(),
        first in 0i64..4_000,
        ops in proptest::collection::vec(op(), 1..80),
    ) {
        let mut e = Engine::new(cfg, first).unwrap();
        let mut log: Vec<Command> = Vec::new();
        for (n, o) in ops.into_iter().enumerate() {
            let before = e.clone();
            if let Op::Join(u, _) = o {
                if let Ok(q) = e.quote(u) {
                    if let Some(c) = q.ceiling {
                        prop_assert!(q.share <= c, "share {} above noticed ceiling {c}", q.share);
                    }
                }
            }
            let Some(cmd) = command(&e, &o, n) else { continue };
            match e.execute(&cmd) {
                Ok(out) => {
                    log.push(cmd);
                    if let Outcome::Settled(s) = out {
                        prop_assert!(s.payout <= s.actual_upkeep + s.deficit_in);
                        let credits: Cents = s.shares.iter().map(|r| r.credit_issued).sum();
                        let shares: Cents = s.shares.iter().map(|r| r.share).sum();
                        prop_assert!(shares <= s.target);
                        let contributed: Cents = s.shares.iter().map(|r| r.contributed).sum();
                        prop_assert_eq!(contributed, shares + credits);
                        if cfg.rules_version >= 2 {
                            // After settlement no one holds credit above their cap, or
                            // stays dormant with credit, unless no charge is left to refund to.
                            for (u, sub) in e.subscribers() {
                                let room: Cents = sub.charges.iter().map(|c| c.gross - c.refunded).sum();
                                let cap = cfg.credit_cap_floor.max(2 * sub.last_share);
                                let dormant = matches!(sub.status, Status::Lapsed | Status::Ended)
                                    && e.current_period() > sub.paid_through + cfg.dormant_after_periods;
                                prop_assert!(room == 0 || sub.credit <= cap, "{u} over cap");
                                prop_assert!(room == 0 || !dormant || sub.credit == 0, "{u} dormant credit kept");
                            }
                        }
                    }
                }
                Err(_) => prop_assert_eq!(&e, &before, "rejected command must not mutate"),
            }
            prop_assert_eq!(e.audit(), Ok(()));
            let max_cap = e.periods().map(|p| p.cap).max().unwrap_or(0);
            prop_assert!(e.reserve() <= max_cap, "reserve {} above every cap seen ({max_cap})", e.reserve());
        }

        let mut replay = Engine::new(cfg, first).unwrap();
        for c in &log {
            replay.execute(c).unwrap();
        }
        prop_assert_eq!(replay, e);
    }

    #[test]
    fn snapshot_plus_tail_equals_full_replay(
        cfg in config(),
        first in 0i64..4_000,
        ops in proptest::collection::vec(op(), 1..80),
        cut in 0usize..80,
    ) {
        let mut e = Engine::new(cfg, first).unwrap();
        let mut log: Vec<Command> = Vec::new();
        for (n, o) in ops.into_iter().enumerate() {
            let Some(cmd) = command(&e, &o, n) else { continue };
            if e.execute(&cmd).is_ok() {
                log.push(cmd);
            }
        }

        let cut = cut.min(log.len());
        let mut head = Engine::new(cfg, first).unwrap();
        for c in &log[..cut] {
            head.execute(c).unwrap();
        }
        let json = serde_json::to_string(&head).unwrap();
        let mut restored: Engine = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&restored, &head, "snapshot round trip");
        prop_assert_eq!(restored.audit(), Ok(()));
        for c in &log[cut..] {
            restored.execute(c).unwrap();
        }
        prop_assert_eq!(restored, e);
    }

    #[test]
    fn a_quote_never_rises_when_someone_else_joins(cfg in config(), first in 1i64..4_000, n in 1u64..12) {
        let mut e = Engine::new(cfg, first).unwrap();
        let mut prev: Option<(u32, Cents)> = None;
        for u in 0..n {
            if pay(&mut e, u).is_err() {
                break;
            }
            let q = e.quote(1000).unwrap();
            if let Some((period, share)) = prev {
                if period == q.period {
                    prop_assert!(q.share <= share, "{} > {share}", q.share);
                }
            }
            prev = Some((q.period, q.share));
        }
    }
}
