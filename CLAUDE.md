# stringcan

Break-even subscription billing: subscribers split an app's upkeep evenly, no margin. Read `docs/DESIGN.md` first; rules marked "PROPOSED DEFAULT" are awaiting the owner.

## Commands

- Test everything: `cargo test --workspace` (DB tests skip unless `STRINGCAN_TEST_DB=1`; start Postgres with `docker compose up -d db`)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`
- Simulator: `cargo run -p stringcan-sim -- --seed 7 --periods 12`
- Dependency audit: `cargo audit`

## Layout

- `crates/stringcan-core`: pure engine. No I/O, clock, or randomness; deterministic; all money is `i64` cents. Changing a rule changes replayed history, so bump `RULES_VERSION` in the API crate when engine behaviour changes.
- `crates/stringcan-sim`: seeded simulator over the engine.
- `crates/stringcan-payments`: `PaymentProcessor` and `ConnectAccounts` traits, `FakeProcessor`, `StripeConnect<Transport>` (Standard accounts, direct charges, webhooks). Tests use `FixtureTransport`; only `ReqwestTransport` (feature `http`) touches the network. Never accept a non-`sk_test_` key. Never add an application fee or a platform balance (DESIGN section 20).
- `crates/stringcan-api`: axum + sqlx service; persists engine commands, rebuilds by replay under a tenant row lock.

## Conventions

- Parameterised SQL only. Validate input at the HTTP boundary into typed structs.
- No secrets in the repo; `.env` is gitignored, `.env.example` carries dummies.
- Comments only where they carry a decision, constraint or surprise. Date-stamp user-visible changes in `CHANGELOG.md`.
- Feature branches off `main`; never commit to `main`; never push without the owner.
- Design decisions go to the owner: add them to `docs/DESIGN.md` as PROPOSED DEFAULT with the alternative.
