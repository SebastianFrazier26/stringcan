# stringcan

Subscription billing that breaks exactly even.

An app owner says what it costs to run the app this month. Subscribers split that cost evenly: 10 subscribers and 10 USD of upkeep means each pays 1 USD. When more people join, everyone's share drops; when people leave, it rises. stringcan takes no cut. Overpayments come back as credit, and an optional capped reserve cushions churn. A public ledger lets anyone check the books.

Status: early. The pricing engine, a simulator, and an API scaffold exist; payments run against a fake processor and a Stripe test-mode stub. Do not use with real money. See [`docs/DESIGN.md`](docs/DESIGN.md) for the rules, the open questions, and the legal flags.

See the rules work: [`docs/demo/REPORT.md`](docs/demo/REPORT.md) (steady state, growth, churn shock with and without a reserve, joining before vs after coverage).

## Try it

```sh
cargo test --workspace
cargo run -p stringcan-sim -- --seed 7 --periods 12
```

Run the whole service locally: put `STRINGCAN_ADMIN_TOKEN=<24+ random chars>` in `.env`, then `docker compose up --build` (API on 127.0.0.1:8080, Postgres alongside).

The simulator drives the real engine with random joins, cancellations and failed payments and prints a per-period break-even table; the engine audits money conservation after every settlement.

## How the price works, briefly

- Price = upkeep divided by the people committed this period, rounded up to the cent.
- Joining early in a period lowers the price for everyone still to pay; earlier payers get the difference back as credit at settlement, so everyone's net cost is the same to the cent.
- Joining after the period is covered prefunds the next period.
- Processor fees are part of the cost: the charge is grossed up so the pool receives the share.

## Layout

| crate | what |
|---|---|
| `stringcan-core` | deterministic pricing and ledger engine, property-tested |
| `stringcan-sim` | seeded simulator |
| `stringcan-payments` | processor trait, fake processor, Stripe test-mode stub |
| `stringcan-api` | axum + Postgres service |

## License

MIT, see [LICENSE](LICENSE).
