# Changelog

Dated entries, newest first. User-visible and rule changes only.

## 2026-11-23

- Owner accepted design section 20 (Stripe Connect custody plan) as written: Standard accounts, direct charges, owner as merchant of record, credits and reserve as ledger-only claims, no application fee. Section 20 status changed from proposed to accepted. The lawyer questions in 20.5 stay open with the owner.
- Owner approved Plaid (instead of Stripe Financial Connections) for reading the owner's own bank balance to verify the required float.
- Owner confirmed all 19 rules marked "owner-confirmed 2026-11-23" in the design doc, including the coverage rule, first-period bootstrap and event sourcing.
- Plaid client in sandbox mode behind a `BankLink` trait: Link token, public-token exchange, real-time balances, item removal, and a pass/fail float check. Fixture-tested with hand-written fixtures; refuses non-sandbox environments; not yet wired into the API.

- Owner accepted the 16 defaults and chose Stripe Connect with funds held in the owner's own bank account; design doc section 20 covers custody, Standard accounts with direct charges, credits and reserve as ledger-only claims, Plaid vs Financial Connections, and compliance flags for a lawyer.
- Stripe Connect in test mode behind the processor traits: connected-account creation, onboarding links, direct-charge PaymentIntents with real fees, refunds, signature-verified idempotent webhooks. Fixture-tested; hand-written fixtures, no live calls.

- Postgres TLS via rustls; non-local hosts require `sslmode=verify-full`.
- Command-log snapshots (every 200 commands) with fallback to full replay; property test for snapshot plus tail.
- Demo report (`docs/demo/REPORT.md`) generated from the real engine: steady, growth burst, churn shock with the reserve on and off, joiner before and after coverage, fees.
- Dockerfile and `docker compose up` verified end to end (image builds, runs as a non-root user, creates a tenant over HTTP); TLS verified by hand against a self-signed Postgres: verify-full with the CA connects, without it fails with UnknownIssuer, and a remote host with a weaker mode refuses to start.
- compose now runs the API as well as Postgres.

- Core engine: quotes, settlement by water-fill, credits, reserve, fee gross-up, audit; 16 scenario tests and property tests.
- Simulator (`stringcan-sim`).
- API service: tenants, owner API keys (hashed), quote/pay/cancel/lapse/refund-credit/upkeep/settle, public pseudonymous ledger; Postgres via sqlx with command-log replay.
- Dockerfile, compose file, CI image build.
- Payments crate: `PaymentProcessor` trait, `FakeProcessor`, Stripe test-mode stub that refuses non-test keys.
- Project scaffolded: workspace, CI, design doc (`docs/DESIGN.md`), MIT license.
