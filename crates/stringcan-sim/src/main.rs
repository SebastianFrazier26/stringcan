//! Seeded simulator: random joins, cancellations and failed payments against the
//! real engine, printing a per-period break-even table.
//!
//! usage: stringcan-sim [--seed N] [--periods N] [--upkeep CENTS] [--joins N]
//!                      [--cancel PCT] [--fail PCT] [--surcharge BPS]

use std::process::ExitCode;

use stringcan_core::{Command, Config, Engine, Outcome, Status, UserId};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn pct(&mut self, p: u64) -> bool {
        self.below(100) < p
    }
}

enum Event {
    Renew(UserId),
    Join(UserId),
}

fn arg(args: &[String], name: &str, default: u64) -> u64 {
    args.windows(2)
        .find(|w| w[0] == name)
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(default)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let seed = arg(&args, "--seed", 1);
    let periods = arg(&args, "--periods", 12);
    let upkeep = arg(&args, "--upkeep", 10_000) as i64;
    let joins = arg(&args, "--joins", 4);
    let cancel = arg(&args, "--cancel", 5);
    let fail = arg(&args, "--fail", 3);
    let cfg = Config {
        reserve_surcharge_bps: arg(&args, "--surcharge", 0) as u32,
        ..Config::default()
    };

    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut engine = Engine::new(cfg, upkeep).expect("valid config");
    let mut next_user: UserId = 1;
    let (mut total_upkeep, mut worst_spread) = (0i64, 0i64);

    println!(
        "seed={seed} upkeep={upkeep} fee=stripe-us-card surcharge_bps={}",
        cfg.reserve_surcharge_bps
    );
    println!(
        "{:>3} {:>5} {:>8} {:>9} {:>9} {:>8} {:>8} {:>8} {:>7}",
        "per", "subs", "min", "max", "payout", "reserve", "deficit", "credits", "spread"
    );

    for _ in 0..periods {
        let current = engine.current_period();
        let mut events: Vec<Event> = engine
            .subscribers()
            .filter(|(_, s)| s.status == Status::Active && s.paid_through < current)
            .map(|(u, _)| Event::Renew(*u))
            .collect();
        for _ in 0..rng.below(joins * 2 + 1) {
            events.push(Event::Join(next_user));
            next_user += 1;
        }
        for i in (1..events.len()).rev() {
            events.swap(i, rng.below(i as u64 + 1) as usize);
        }

        for ev in events {
            match ev {
                Event::Renew(u) if rng.pct(cancel) => {
                    let _ = engine.execute(&Command::Cancel { user: u });
                }
                Event::Renew(u) if rng.pct(fail) => {
                    let _ = engine.execute(&Command::Lapse { user: u });
                }
                Event::Renew(u) | Event::Join(u) => {
                    if let Ok(q) = engine.quote(u) {
                        let fee = engine.fee_model().fee(q.gross_due);
                        let _ = engine.execute(&Command::RecordPayment {
                            user: u,
                            gross: q.gross_due,
                            fee,
                        });
                    }
                }
            }
        }

        // Real upkeep wobbles around the estimate by up to +/-10%.
        let estimate = engine.period(current).map_or(upkeep, |p| p.upkeep);
        let actual = estimate + (rng.below(21) as i64 - 10) * estimate / 100;
        let Ok(Outcome::Settled(s)) = engine.execute(&Command::Settle {
            actual,
            evidence: None,
        }) else {
            eprintln!("settle failed");
            return ExitCode::FAILURE;
        };
        if let Err(e) = engine.audit() {
            eprintln!("AUDIT FAILED: {e}");
            return ExitCode::FAILURE;
        }
        total_upkeep += actual;
        let shares: Vec<i64> = s.shares.iter().map(|r| r.share).collect();
        let (min, max) = (
            shares.iter().min().copied().unwrap_or(0),
            shares.iter().max().copied().unwrap_or(0),
        );
        worst_spread = worst_spread.max(max - min);
        println!(
            "{:>3} {:>5} {:>8} {:>9} {:>9} {:>8} {:>8} {:>8} {:>7}",
            current,
            shares.len(),
            money(min),
            money(max),
            money(s.payout),
            money(engine.reserve()),
            money(s.deficit_out),
            money(engine.credits_outstanding()),
            max - min
        );
    }

    println!("\nupkeep declared over run: {}", money(total_upkeep));
    println!(
        "worst within-period share spread: {worst_spread} cent(s) (shortfall periods can exceed 1)"
    );
    println!("audit: money conserved, no payout above upkeep + carried deficit");
    ExitCode::SUCCESS
}

fn money(c: i64) -> String {
    format!("{}.{:02}", c / 100, (c % 100).abs())
}
