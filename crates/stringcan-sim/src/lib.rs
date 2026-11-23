//! Deterministic scenario scripts over the real engine, used for the demo report
//! and for tests that pin the headline behaviours.

use stringcan_core::{Cents, Command, Config, Engine, FeeModel, Outcome, Status, UserId};

#[derive(Clone, Debug, Default)]
pub struct Step {
    /// New subscribers who pay before the renewers do (they join the funding period).
    pub joins_early: usize,
    /// Renewers who cancel instead of paying.
    pub cancels: usize,
    /// Renewers who fail to pay and lapse.
    pub fails: usize,
    /// New subscribers who pay after every renewer (the period is covered, so they prefund the next).
    pub joins_late: usize,
    /// Actual upkeep; `None` means exactly the estimate.
    pub actual: Option<Cents>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub period: u32,
    pub participants: usize,
    pub min_share: Cents,
    pub max_share: Cents,
    pub payout: Cents,
    pub credits_issued: Cents,
    pub reserve: Cents,
    pub deficit_out: Cents,
}

pub struct Run {
    pub rows: Vec<Row>,
    pub engine: Engine,
}

fn pay(e: &mut Engine, user: UserId) {
    if let Ok(q) = e.quote(user) {
        let fee = e.fee_model().fee(q.gross_due);
        let _ = e.execute(&Command::RecordPayment {
            user,
            gross: q.gross_due,
            fee,
        });
    }
}

fn pending(e: &Engine) -> Vec<UserId> {
    let current = e.current_period();
    e.subscribers()
        .filter(|(_, s)| s.status == Status::Active && s.paid_through < current)
        .map(|(u, _)| *u)
        .collect()
}

/// Bootstrap `n` subscribers (first pays period 1 alone, the rest prefund period 2),
/// settle period 1, and let period 2 collect its first renewer.
pub fn seeded_engine(cfg: Config, upkeep: Cents, n: u64) -> (Engine, u64) {
    let mut e = Engine::new(cfg, upkeep).expect("config");
    for u in 1..=n {
        pay(&mut e, u);
    }
    (e, n + 1)
}

pub fn run(cfg: Config, upkeep: Cents, subscribers: u64, steps: &[Step]) -> Run {
    let (mut e, mut next_user) = seeded_engine(cfg, upkeep, subscribers);
    let mut rows = Vec::new();
    // The first settle closes the bootstrap period (period 1), which one person funded alone.
    let mut bootstrap = true;
    let mut step_iter = steps.iter();
    loop {
        let step = if bootstrap {
            Step::default()
        } else {
            match step_iter.next() {
                Some(s) => s.clone(),
                None => break,
            }
        };
        let current = e.current_period();
        if !bootstrap {
            let mut renewers = pending(&e);
            for _ in 0..step.joins_early {
                pay(&mut e, next_user);
                next_user += 1;
            }
            for _ in 0..step.cancels.min(renewers.len()) {
                let u = renewers.pop().expect("non-empty");
                let _ = e.execute(&Command::Cancel { user: u });
            }
            // Failures are quoted the same price as everyone else and only lapse once the
            // renewal window ends, so the others have already paid the pre-churn price.
            let failing: Vec<UserId> = (0..step.fails.min(renewers.len()))
                .map(|_| renewers.pop().expect("non-empty"))
                .collect();
            for u in renewers {
                pay(&mut e, u);
            }
            for u in failing {
                let _ = e.execute(&Command::Lapse { user: u });
            }
            for _ in 0..step.joins_late {
                pay(&mut e, next_user);
                next_user += 1;
            }
        }
        let estimate = e.period(current).map_or(upkeep, |p| p.upkeep);
        let actual = step.actual.unwrap_or(estimate);
        let Ok(Outcome::Settled(s)) = e.execute(&Command::Settle {
            actual,
            evidence: None,
        }) else {
            panic!("settle failed");
        };
        e.audit().expect("audit");
        let shares: Vec<Cents> = s.shares.iter().map(|r| r.share).collect();
        rows.push(Row {
            period: current,
            participants: shares.len(),
            min_share: shares.iter().copied().min().unwrap_or(0),
            max_share: shares.iter().copied().max().unwrap_or(0),
            payout: s.payout,
            credits_issued: s.shares.iter().map(|r| r.credit_issued).sum(),
            reserve: e.reserve(),
            deficit_out: s.deficit_out,
        });
        bootstrap = false;
    }
    Run { rows, engine: e }
}

pub fn zero_fee() -> Config {
    Config {
        fee: FeeModel::ZERO,
        reserve_cap_bps: 5_000,
        reserve_surcharge_bps: 0,
    }
}

/// One line of the joiner walkthrough.
#[derive(Clone, Debug)]
pub struct Payment {
    pub who: String,
    pub funds_period: u32,
    pub charged: Cents,
}

pub struct JoinerWalkthrough {
    pub payments: Vec<Payment>,
    pub settled_shares: Vec<Cents>,
    pub credits: Vec<(String, Cents)>,
    pub joiner_period: u32,
}

/// The product example: ten subscribers, 10.00 upkeep. `early_after` renewers
/// pay, then someone new joins, then the remaining renewers pay.
/// With `early_after = None` the joiner arrives only after all ten have paid.
pub fn joiner_walkthrough(early_after: Option<usize>) -> JoinerWalkthrough {
    let (mut e, _) = seeded_engine(zero_fee(), 1000, 10);
    // Settle the bootstrap period and renew period 2 so we start from ten settled-in subscribers.
    let settle = |e: &mut Engine| {
        let _ = e.execute(&Command::Settle {
            actual: 1000,
            evidence: None,
        });
    };
    settle(&mut e);
    pay(&mut e, 1);
    settle(&mut e);
    for u in 1..=10 {
        let _ = e.execute(&Command::RefundCredit { user: u });
    }
    let mut payments = Vec::new();
    let mut record = |e: &mut Engine, who: String, user: UserId| {
        let q = e.quote(user).expect("quote");
        payments.push(Payment {
            who,
            funds_period: q.period,
            charged: q.gross_due,
        });
        pay(e, user);
        q.period
    };
    let joiner_period;
    if let Some(k) = early_after {
        for u in 1..=k as u64 {
            record(&mut e, format!("renewer {u}"), u);
        }
        joiner_period = record(&mut e, "NEW JOINER".into(), 11);
        for u in (k as u64 + 1)..=10 {
            record(&mut e, format!("renewer {u}"), u);
        }
    } else {
        for u in 1..=10 {
            record(&mut e, format!("renewer {u}"), u);
        }
        joiner_period = record(&mut e, "NEW JOINER".into(), 11);
    }
    let Ok(Outcome::Settled(s)) = e.execute(&Command::Settle {
        actual: 1000,
        evidence: None,
    }) else {
        panic!("settle");
    };
    let name = |u: UserId| {
        if u == 11 {
            "NEW JOINER".to_string()
        } else {
            format!("renewer {u}")
        }
    };
    JoinerWalkthrough {
        payments,
        settled_shares: s.shares.iter().map(|r| r.share).collect(),
        credits: s
            .shares
            .iter()
            .filter(|r| r.credit_issued > 0)
            .map(|r| (name(r.user), r.credit_issued))
            .collect(),
        joiner_period,
    }
}

pub fn money(c: Cents) -> String {
    format!(
        "{}${}.{:02}",
        if c < 0 { "-" } else { "" },
        c.abs() / 100,
        c.abs() % 100
    )
}
