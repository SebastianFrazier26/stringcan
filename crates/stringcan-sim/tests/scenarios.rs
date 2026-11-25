use stringcan_core::Config;
use stringcan_sim::{joiner_walkthrough, run, zero_fee, Step};

fn shock(cfg: Config) -> stringcan_sim::Run {
    let mut steps = vec![Step::default(); 5];
    steps.push(Step {
        fails: 15,
        ..Step::default()
    });
    steps.push(Step::default());
    run(cfg, 40_000, 40, &steps)
}

#[test]
fn steady_state_splits_exactly_and_keeps_nothing() {
    let r = run(zero_fee(), 20_000, 20, &vec![Step::default(); 4]);
    for row in &r.rows[1..] {
        assert_eq!(
            (row.min_share, row.max_share, row.payout),
            (1_000, 1_000, 20_000)
        );
        assert_eq!((row.reserve, row.deficit_out), (0, 0));
    }
}

#[test]
fn a_growth_burst_lowers_the_price_for_everyone() {
    let steps = [
        Step::default(),
        Step {
            joins_early: 30,
            ..Step::default()
        },
        Step::default(),
    ];
    let r = run(zero_fee(), 20_000, 20, &steps);
    assert_eq!(r.rows[1].max_share, 1_000);
    assert_eq!(r.rows[2].max_share, 400, "200.00 over 50 people");
}

#[test]
fn the_reserve_softens_a_churn_shock_at_the_cost_of_a_premium() {
    let off = shock(zero_fee());
    let on = shock(Config {
        reserve_surcharge_bps: 500,
        ..zero_fee()
    });
    let (off_shock, on_shock) = (&off.rows[6], &on.rows[6]);
    assert_eq!(
        off_shock.deficit_out, 15_000,
        "15 missing payers x 10.00, no cushion"
    );
    assert!(on_shock.deficit_out < off_shock.deficit_out);
    let (off_next, on_next) = (off.rows[7].max_share, on.rows[7].max_share);
    assert!(on_next < off_next, "{on_next} should be below {off_next}");
    assert!(
        on.rows[2].max_share > off.rows[2].max_share,
        "the premium is real"
    );
}

#[test]
fn joiner_before_coverage_equalises_and_after_coverage_prefunds() {
    let early = joiner_walkthrough(Some(4));
    assert!(early
        .payments
        .iter()
        .all(|p| p.funds_period == early.payments[0].funds_period));
    let joiner = early
        .payments
        .iter()
        .find(|p| p.who == "NEW JOINER")
        .unwrap();
    assert_eq!(joiner.charged, 91);
    let (min, max) = (
        early.settled_shares.iter().min().unwrap(),
        early.settled_shares.iter().max().unwrap(),
    );
    assert!(max - min <= 1);
    assert_eq!(early.settled_shares.iter().sum::<i64>(), 1_000);

    let late = joiner_walkthrough(None);
    let joiner = late
        .payments
        .iter()
        .find(|p| p.who == "NEW JOINER")
        .unwrap();
    assert_eq!(joiner.funds_period, late.payments[0].funds_period + 1);
    assert!(late.settled_shares.iter().all(|&s| s == 100));
}
