# Changelog

Dated entries, newest first. User-visible and rule changes only.

## 2026-11-24

- Owner set the rules v3 absolute credit ceiling to $500 (`credit_cap_ceiling = 50_000` cents), replacing the proposed $1,000. The credit cap floor must stay at or below the ceiling. Tenants keep the ceiling they were created with.

## 2026-11-23

- Owner approved every recommendation and proposed default in the design doc ("all recommendations fly"). Adopted: P1 (agent-of-payee owner terms), P2 (never act for subscribers), P7 (the reserve is an owner liability to the pool), P8 (EU position: never in possession of funds), float publication, the balance-counting rule, a daily plus on-demand float re-check, Plaid webhooks that only mark the status unverifiable, payout-account verification, a dormant sweep after 3 periods, the last 6 charges as refund targets, and consent when no renewal cap is on file. Design doc 20.10 lists them. Still open: the donations recipient, credit that no remembered charge can absorb, California's 30-day notice upper bound, and the lawyer review.
- Rules v3 for new tenants (v1 and v2 tenants replay unchanged): no subscriber's credit is held above an absolute ceiling, `credit_cap_ceiling`, proposed at $1,000 (set to $500 on 2026-11-24), which may not exceed FinCEN's $2,000 closed-loop limit. Any excess is refunded to the subscriber's own charges at settlement.
- Renewals are now refused until their price notice is at least 10 days old. The API records when each notice went out.
- Linking a bank account now checks that it is the Stripe payout account by routing number and last four. A mismatch is refused. When the check is not possible, the account is marked `owner_declared`, and the public ledger shows `float_check.payout_account`. No account numbers are stored.
- `POST /v1/admin/bank/recheck` (admin only) re-checks the float of every bank link not checked in the last 20 hours, for a daily cron.
- `docs/legal/`: draft owner agreement (agent of the payee) and the no-subscriber-agency rule, both marked as drafts for lawyer review, not legal advice.

- Owner adopted credit safeguards P3 to P6 and price-notice option N3 with a 10-day notice; recorded donations as GitHub Sponsors now and stringcan-as-its-own-tenant later (recipient still open). P1, P2, P7 and P8 remain proposed.
- Rules v2 for new tenants (existing tenants keep v1 and replay unchanged): renewals need a logged price notice and the subscriber's consented cap, and are charged at most the noticed ceiling, with any gap carried as a shortfall; credit is refunded only to the subscriber's own recent charges, capped per subscriber at the larger of $100 or two shares (the excess refunded at settlement), never moved between subscribers, and swept back to the card after 3 dormant periods.
- API: `POST /v1/periods/{id}/notices` (notifications job; logs only, no email yet) and `PUT /v1/subscribers/{ref}/renewal-cap`; settlement and credit refunds now issue one processor refund per charge.
- Fixed a webhook test that failed when run twice against the same database.

- Owner decided the Plaid questions: the access token is stored encrypted in Postgres with a key from the secret store; Link uses the `auth` product only; only the account that receives Stripe payouts counts toward the float.
- Bank link API (Plaid sandbox only, off unless configured): link token, public-token exchange naming the payout account, unlink, and an on-demand float check that returns only verified, short or unverifiable. The public ledger shows the latest result as `float_check`. Access tokens are sealed with XChaCha20-Poly1305 bound to the tenant, with a key id for rotation.

- Owner positions recorded on the six lawyer questions (design 20.5), dated 2026-11-23 and marked as owner positions, not legal advice; the lawyer review before live mode stays required. Positions: stringcan acts as the owner's payment agent; credits are prepaid balances redeemable for cash; the reserve should not create trust or segregation duties; variable-price notice and consent open pending research; tax confirmed on the owner as merchant of record with Stripe Tax on top of the share; stringcan is free and funded by donations, no application fee.
- Design 20.7: consequences of those positions (FinCEN exclusions and prepaid-access limits, MTMA agent-of-payee exemption, PSD2 commercial-agent and technical-provider exclusions, EU e-money) and eight proposed design changes for the owner, none adopted.
- Design 20.8: research on notice and consent for variable monthly prices (EU unfair-terms law and withdrawal button, Digital Fairness Act proposal, California auto-renewal law after AB 2863, FTC rule status), four options and a recommendation, awaiting the owner.
- Design 20.9: donation options that keep "no cut" true, as an owner question.

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
