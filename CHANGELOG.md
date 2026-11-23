# Changelog

Dated entries, newest first. User-visible and rule changes only.

## 2026-11-23

- Postgres TLS via rustls; non-local hosts require `sslmode=verify-full`.
- Command-log snapshots (every 200 commands) with fallback to full replay; property test for snapshot plus tail.
- Demo report (`docs/demo/REPORT.md`) generated from the real engine: steady, growth burst, churn shock with the reserve on and off, joiner before and after coverage, fees.
- compose now runs the API as well as Postgres.

- Core engine: quotes, settlement by water-fill, credits, reserve, fee gross-up, audit; 16 scenario tests and property tests.
- Simulator (`stringcan-sim`).
- API service: tenants, owner API keys (hashed), quote/pay/cancel/lapse/refund-credit/upkeep/settle, public pseudonymous ledger; Postgres via sqlx with command-log replay.
- Dockerfile, compose file, CI image build.
- Payments crate: `PaymentProcessor` trait, `FakeProcessor`, Stripe test-mode stub that refuses non-test keys.
- Project scaffolded: workspace, CI, design doc (`docs/DESIGN.md`), MIT license.
