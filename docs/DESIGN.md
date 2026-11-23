# stringcan design

Status: draft v0, 2026-11-23. Every rule marked **PROPOSED DEFAULT (owner to confirm)** was chosen by the author, not the product owner, and has a stated alternative. Rules without that mark follow directly from the product idea.

## 1. The idea in one paragraph

stringcan is a subscription billing layer that never keeps a margin. An app owner declares what it costs to run the app for a period (the *upkeep*). Subscribers split that cost evenly: with 10 subscribers and 10 USD of upkeep, each pays 1 USD. When people join, everyone's share falls; when people leave, it rises. Any money collected beyond what is needed is returned to the payers as credit, or held in a capped reserve that absorbs churn. A public ledger lets any subscriber check that the books break even.

Break-even is the product promise, so it is enforced as a set of invariants (section 14) that are property-tested, not as a convention.

## 2. Vocabulary

| Term | Meaning |
|---|---|
| tenant | an app owner using stringcan; owns one pricing pool |
| subscriber | an end user of the tenant's app who pays a share |
| period | one billing cycle (monthly by default), numbered 1, 2, 3 ... |
| upkeep `U` | cost of running the app for a period, in cents |
| target `T` | what subscribers must fund in a period: `U + carried deficit + reserve top-up` (section 8) |
| committed set | subscribers who will be charged for a period: those who already paid plus active renewers |
| quote | the price one subscriber would pay right now |
| contribution | money (cash net of fees, plus any credit used) a subscriber put into a period |
| share | a subscriber's final, fair portion of the period target, fixed when the period settles |
| credit | an overpayment returned to a subscriber as a balance, applied to their next bill |
| reserve | money held back to cover shortfalls when subscribers leave |

All money is integer cents (`i64`). There is no floating point anywhere in the engine.

## 3. Periods and billing dates

- A tenant has a sequence of periods. Period `p` is the funding for service in `p`. Periods are monthly by default and each has a **billing date**, the date renewals for that period are charged. The engine only knows period numbers; the API layer maps numbers to calendar dates.
- At any time at most one period is *funding* (the earliest period that is not yet covered). A period is **covered** when its contributions reach its target.
- Upkeep for a period is an estimate when the period opens. The owner may revise it until the period settles. At settlement the owner supplies the actual figure (section 10).
- **PROPOSED DEFAULT (owner to confirm): monthly periods, billing date = first day of the period, charged in UTC.** Alternative: per-tenant cadence (weekly, annual) or per-subscriber anniversary billing. Anniversary billing breaks the shared-pool model because subscribers would not be funding the same period, so it is rejected for v0.

## 4. Price per subscriber and when it is computed

For the funding period with subscriber target `T` (section 8) and a committed set of size `n` that includes the person paying:

```
quote_share = ceil(T / n)          (n >= 1)
```

Rounding up guarantees `n` payers at the quote cover `T`. The price is recomputed on demand, never stored: every quote reads the live target and committed set, so it falls when people join and rises when people lapse.

The **committed set** is everyone who has contributed to the period plus every active renewer who still owes it. A joiner counts themselves.

A subscriber's actual charge is `quote_share` minus any credit they hold (applied first), grossed up for processor fees (section 9). If their credit covers the whole share, nothing is charged and no fee is incurred.

Worked example used in the tests. Upkeep 1000 cents, 10 renewers, so the quote is 100 each. Four renewers pay 100 (400 collected).

## 5. A joiner before the period is covered

Product rule: a joiner pays the current price, and the price then drops for everyone who has not yet paid.

In the example a joiner arrives. Counting themselves, the committed set is 11, so they pay `ceil(1000/11)` = 91, and the 6 renewers still to pay are quoted 91 instead of 100. Collections are `4*100 + 91 + 6*91` = 1037, which is 37 over the target.

**How earlier payers are made whole: settlement credits (water-fill).** When the period settles, every participant's final share is the same level `s`, chosen so the shares sum to exactly `T`. Here 1000 over 11 participants is 90 remainder 10, so ten pay 91 and one pays 90. Anyone who paid more than their share gets the difference as credit. The four early payers paid 100 and receive 9 or 10 cents back. Net cost per person is identical no matter when in the period they paid.

- **Joiner pays the price including themselves (T/(n+1)), not the old price (T/n).** PROPOSED DEFAULT (owner to confirm). The literal reading, "pay the old price, then lower it", behaves badly at the start: with one subscriber committed, the next joiner would pay `T/1`, the entire target, and cover the period alone, then the next joiner does the same for the following period, so each period is paid by one rotating person. Because settlement credits equalise net cost, the two readings differ only in cash flow. Alternative: the literal rule plus a cap on any single payment at `T/2`.
- **Overpayment returned as per-person credit applied to the next bill, refundable in cash on request.** PROPOSED DEFAULT (owner to confirm). Alternative A: roll the reduction forward as an equal discount for everyone next period (simpler, but early payers subsidise late payers, which breaks "evenly"). Alternative B: immediate cash refund (exact, but every refund costs a processor fee and defeats the point).
- **Water-fill rather than a naive equal split.** If some subscribers paid less than the final level (the price rose after they paid because renewers lapsed), they cannot be charged retroactively. A share is `min(contribution, s)` for the largest level `s` whose shares do not exceed `T`; leftover cents go to participants who paid above `s`. If contributions fall below `T` the period is short (section 7).

### When a period counts as covered

A period is **covered** when contributions reach its target **and** every active renewer has either paid it or been lapsed. Cash alone is not enough: early payers of a falling price overpay, and treating that overshoot as coverage would send the remaining renewers and later joiners to a different period than the early payers. PROPOSED DEFAULT (owner to confirm). Alternative: cash-only coverage, which in simulation lets a handful of early payers fund a whole period while the other renewers get a free ride until the next one.

Consequence for the first period: the first subscriber is the only committed person, so they pay the whole target and the period is covered. Joiners after that prefund period 2, where their shares fall as more people join. By the end of period 2 everyone has the same net share. Period 1 is a bootstrap quirk: whoever subscribes first funds it alone. PROPOSED DEFAULT (owner to confirm). Alternative: the owner fronts period 1 and recovers it as a carried deficit (section 7), which spreads it over period 2's subscribers.

### Anti-gaming

1. **Waiting does not help.** Net cost for a period is the final level `s`, the same for everyone who contributes, whenever they pay. A joiner who waits for the price to fall pays the lower quote later, but an earlier payer's overpayment is credited back to the same level. Waiting only costs the waiter access.
2. **Join-and-leave.** Contributions are not refundable mid-period (section 11), so joining to see a price and leaving does not extract money. Only credit (overpayment) is refundable.
3. **Late-period joining.** Once the period is covered, joiners fund the next period (section 6). They get no discount for joining late; they do get immediate access, which costs nobody anything because upkeep is fixed (assumption A1).
4. **Quote front-running.** A quote is only a quote. The engine attributes a payment to the funding period when the payment is *recorded*, and settles all shares together, so seeing a quote and paying later changes nothing about the final share.
5. **Skipping a renewal.** A renewer who does not pay is lapsed at settlement and must re-subscribe at the then-current quote. The tenant app grants access only when the subscriber is paid through the current period. If others' payments cover a period without them, the others' level `T/n` is unchanged because the missing renewer was replaced by a joiner, so nobody is worse off.
6. **Sybil joiners** (one person, many accounts) pay one share per account. Detecting duplicates is the tenant app's job.

Assumption A1: upkeep is independent of the number of subscribers within a period. If marginal cost per user matters (e.g. per-seat LLM tokens), the owner models it as a separate per-user cost line in a later version. PROPOSED DEFAULT (owner to confirm): fixed upkeep per period; no per-seat variable cost in v0.

## 6. A joiner after the period is covered

Once a period is covered, the next payment goes to the next period (created automatically, with an estimate equal to the latest upkeep). The joiner:

- gets access immediately, including the rest of the covered period, at no cost to anyone;
- pays the quote for the next period: `ceil(T_next / n_next)` where `n_next` counts the active renewers (those not cancelling), people who have prepaid, and themselves;
- is a participant of the next period and shares its credits at its settlement.

So money paid after coverage prefunds the next period, and next period's price drops accordingly. This is the "goes to next month's upkeep" rule. Until the previous period settles, the next period's target is provisional (no carried deficit, reserve top-up or release yet); those are fixed at settlement of the previous period and water-fill absorbs the difference.

## 7. Churn and shortfalls

Lapsed or cancelling subscribers shrink the committed set, raising the price for the next period. Within a period the shortfall arises when contributions end below target (a price was quoted for a large committed set, and some committed subscribers then failed to pay).

At settlement, with contributions `C` (including prefund) below the upkeep plus carried deficit:

1. The **reserve** pays the shortfall first, up to the reserve balance.
2. Whatever the reserve cannot cover becomes the **carried deficit**, added to the next period's target (so costs rise for those remaining). The owner is assumed to have fronted that money from their own pocket, and the carried amount is how they are repaid.
3. Subscribers who already paid are never charged retroactively.

- **PROPOSED DEFAULT (owner to confirm): the reserve is drawn before any deficit is carried; the unrecovered remainder is carried as a deficit.** Alternative: carry the shortfall immediately and keep the reserve only for catastrophic loss (more volatile prices, bigger reserve).
- **Non-payers lapse.** At settlement of `p`, any active subscriber whose paid-through period is before `p` is lapsed (access ends). They can re-subscribe at the then-current quote.

## 8. The reserve

- Sources: (a) the optional top-up built into the period target, (b) nothing else. Overpayment never goes to the reserve; it returns to payers as credit (section 5).
- **Top-up (reserve build).** The target is `T = U + D + top_up`, where `top_up = min(cap - reserve, ceil(U * surcharge_bps / 10_000))`.
- **Cap.** `cap = ceil(U * reserve_cap_bps / 10_000)`.
- **PROPOSED DEFAULT (owner to confirm): `surcharge_bps = 0` and `reserve_cap_bps = 5_000` (half a period of upkeep).** With surcharge 0, the reserve stays at zero unless the owner opts in, which keeps the pure "exactly break even" promise literally true. Alternative: a small build surcharge (e.g. 300 bps = 3 percent) until the reserve reaches the cap, trading a slightly higher price for price stability when people leave.
- **Excess above the cap** (the cap shrank because upkeep fell): the excess is *released* as prefund into the next period's pool, which lowers the amount subscribers must pay. Nobody is refunded in cash. **PROPOSED DEFAULT (owner to confirm).** Alternative: pro-rata credits to current subscribers.
- The reserve is the tenant pool's money, never the owner's. The owner cannot withdraw from it; it is released only by the rules above.

## 9. Rounding and processor fees

**Rounding.** Shares are integer cents. When `T` does not divide evenly among the participants at the final level, the leftover cents (fewer than the number of participants) are assigned one each to the earliest contributors (by contribution order), who by construction have paid the most. No remainder is retained by anyone; it is a share, not a profit. **PROPOSED DEFAULT (owner to confirm).** Alternative: rotate who pays the extra cent each period (fairer over a long horizon, more state).

**Processor fees are upkeep, and the price is grossed up for them.** The ledger is kept in *net* cents (money actually received after fees). A fee model `fee(g) = ceil(g * bps / 10_000) + fixed` (Stripe-like defaults: 290 bps + 30 cents; configurable) estimates fees. To receive `net` cents the subscriber is charged the smallest `g` with `g - fee(g) >= net`. The subscriber sees the fee as a separate line on the quote.

- The actual fee reported by the processor is what the ledger records. Differences between estimated and actual fees flow into the normal settlement (extra net becomes credit, missing net becomes shortfall).
- Credits are denominated in net cents; spending a credit that covers the whole share avoids a charge and therefore a fee.
- Fees are **not** charged on the portion of overpayment returned as credit (the fee on that overpayment is a sunk cost; it is small and is absorbed into the period's pool as part of the quote rounding). **PROPOSED DEFAULT (owner to confirm).** Alternative: allocate fees on overpayments back to the overpayers.
- Processors typically do not return their original fee on a refund. **PROPOSED DEFAULT (owner to confirm): credit refunds are paid in full, and the unrecovered fee is added to the next period's upkeep as a cost (not modelled in v0).** Alternative: deduct it from the refund.

## 10. Variable or unknown upkeep

- At period open the owner states an **estimate** (default: last period's actual). The estimate is the target the price is computed from.
- Before settlement the owner may revise the estimate (new invoice arrived); later payers see the new price; earlier payers' overpayment or underpayment is handled by water-fill at settlement.
- At settlement the owner supplies the **actual** upkeep, optionally with an evidence reference (invoice id or URL) that appears in the public ledger. The settlement runs against the actual:
  - actual below the total contributed: the difference is credited to payers (true-up down);
  - actual above: shortfall rules (section 7), including drawing the reserve.
- **PROPOSED DEFAULT (owner to confirm): the owner self-attests upkeep and evidence is published but not verified.** Alternatives: connect cloud-provider billing APIs to verify; require an invoice upload; a subscriber-vote or audit mechanism. Without verification the "break-even" claim is only as honest as the owner, which is the main trust gap (section 12).
- **Owner pay-out.** On settlement the engine records a payout of `min(contributions, actual + carried deficit)` to the owner. The owner's pay-out can never exceed the declared upkeep plus carried deficit plus reserve top-up; the engine refuses to produce a larger one.

## 11. Cancellations, refunds, failed payments

- **Cancel**: takes effect at the end of the period the subscriber has paid for. They keep access, are excluded from the next period's committed set, and are marked ended at settlement. There is no cash refund of contributions mid-period (they already fund a shared cost that the others are counting on). **PROPOSED DEFAULT (owner to confirm).** Alternative: pro-rata refund for a cancellation in the first N days (kills the pool's predictability).
- **Credit refund**: the subscriber's credit balance is refundable at any time, in cash through the processor.
- **Failed payment**: the processor's failure leaves no contribution. The subscriber stays committed (counted in the price) while dunning is active; the tenant app retries (default 3 attempts over 7 days). When dunning is exhausted, or at settlement if the subscriber has not paid, they are lapsed and drop out of the committed set, which raises the price for others. **PROPOSED DEFAULT (owner to confirm): 7-day grace; the period settles at its end regardless.** Alternative: settlement waits for dunning to end (delays owner pay-out).
- **Chargebacks** (disputes after settlement): modelled as a negative contribution adjustment applied to the current funding period's shortfall and the processor's dispute fee, i.e. it is upkeep. Not implemented in v0; flagged.

## 12. Trust and transparency

The break-even promise is only credible if verifiable.

- A **public ledger** is served without authentication per tenant: every journal entry (contribution, credit use, credit issue, settlement, payout, reserve movement, refund), per-period target, collected, payout, reserve. Subscribers appear as stable per-tenant ordinal pseudonyms (`s-17`), not as external ids or emails; a subscriber can ask the tenant app for their own ordinal.
- Invariants from section 14 are asserted by the engine's `audit()` and served as a `break_even: true/false` flag with the numbers behind it.
- The ledger is derived from an append-only command log. Replaying the log reproduces the state byte-for-byte (the engine is deterministic and has no clock or randomness).
- **PROPOSED DEFAULT (owner to confirm): the ledger is public per tenant.** Alternative: ledger visible only to the tenant's subscribers.
- Residual trust: upkeep is self-attested (section 10). Publishing evidence and a third-party audit hook are the roadmap for narrowing it.

## 13. Multi-tenant model

- One stringcan deployment hosts many tenants. A tenant is a fully separate pool: its own periods, subscribers, reserve and ledger. Nothing is shared or pooled across tenants.
- Postgres: every row carries `tenant_id`; the command log is per tenant, and writes for a tenant serialise on a row lock on the tenant row (`SELECT ... FOR UPDATE`) so replay-then-append is race free.
- Subscribers are identified by the tenant's `external_ref` (opaque string); stringcan stores no email or name in v0.
- stringcan takes **no platform fee**. Hosting costs of stringcan itself can be a tenant of stringcan (dogfooding) and is not built into the engine. **PROPOSED DEFAULT (owner to confirm).** Alternative: an explicit platform line item in the target.
- Money custody is the open question (section 15): v0 assumes each tenant brings its own Stripe account, so funds never sit with stringcan.

## 14. Invariants (property-tested)

Let a period settle with target `T`, participants `i` with contributions `c_i`, shares `s_i`, credits `k_i`.

1. **Share bounds**: `0 <= s_i <= c_i` and `k_i = c_i - s_i >= 0`.
2. **Exact cover**: if `sum(c_i) + prefund >= T`, `sum(s_i) + prefund = T` exactly; otherwise `s_i = c_i` for all i (the period is short by `T - sum(c_i) - prefund`).
3. **Evenness**: among participants whose share was not capped by their own contribution, shares differ by at most 1 cent.
4. **Money conservation**: at all times, `cash_in_net = payouts + refunds + reserve + credits_outstanding + open_pool`, where `open_pool` is everything contributed (cash, credit used, prefund) to unsettled periods. Credit use and prefund are internal transfers.
5. **No over-collection**: the owner's total payout for a period never exceeds `actual upkeep + carried deficit`; the reserve receives at most `top_up`; the rest of any overpayment is credit, not retained.
6. **Reserve bounds**: the reserve is never negative, and a top-up never pushes it above the cap.
7. **Even price**: a quote is `ceil(T/n)` and `n` payers at the quote cover `T`.
8. **Fee gross-up**: for any net `x > 0`, `gross_for_net(x) - fee(gross_for_net(x)) >= x` and `gross_for_net` is minimal.
9. **Determinism**: replaying the same command sequence yields identical state and journal.
10. **Quote is monotone**: with no payments in between, a joiner's presence never raises anyone else's quote.

## 15. Legal and compliance notes (flagged, not decided)

Not legal advice; none of the following is decided. The owner should take advice before taking real money.

- **Money transmission / holding funds.** If stringcan (the operator) holds subscriber money in a reserve, or pays tenants out, that may constitute money transmission or stored-value activity needing licensing (in the US, FinCEN registration and state money transmitter licences; elsewhere, e-money or payment-institution rules). The safest v0 design avoids it: each tenant connects its own Stripe account (or uses Stripe Connect) and the reserve and credit balances are *ledger entries* the tenant is responsible for honouring. Even then, a reserve held by the tenant is a customer-liability. Decide before real money: Stripe Connect model, who is merchant of record.
- **Credits and refunds.** Stored credit may be regulated as gift-card or stored value in some jurisdictions and may be subject to unclaimed-property (escheat) rules.
- **Tax.** Sales tax / VAT on subscriptions depends on the subscriber's location; the engine models only the pre-tax share. Tax is charged on top by the processor (Stripe Tax) and kept out of the pool. "At-cost" sharing does not obviously escape income or sales tax; get advice.
- **Consumer protection.** Variable monthly prices need clear disclosure and consent: price notices before renewal, the right to cancel, and in some regions (EU, California auto-renewal law) specific renewal reminders and consent to price changes.
- **Pricing claims.** "We take no cut" must be literally true and auditable, or it may be an unfair-practice claim. This is why the ledger is public.
- **Data protection.** v0 stores opaque external refs only; adding emails or payment methods moves it into GDPR/CCPA territory. Card data stays at Stripe (PCI scope SAQ-A).
- **Processor terms.** Stripe's terms on pooled funds, delayed settlement and variable pricing need to be checked.

## 16. Authentication and authorisation

Decided at design time:

| Actor | Credential | Can do |
|---|---|---|
| platform admin | one bootstrap token from env (`STRINGCAN_ADMIN_TOKEN`), compared in constant time | create tenants. Nothing else |
| tenant owner | per-tenant API key, `sc_` + 32 random bytes, hex; only the SHA-256 hash is stored; shown once at creation | everything inside their own tenant: set upkeep, open and settle periods, quote and record payments for their subscribers, cancel, refund credit |
| subscriber | none in v0. The tenant's app calls the API server-to-server with its owner key on the subscriber's behalf | n/a |
| anyone | none | read the public ledger of a tenant |

- Every owner endpoint resolves the key to exactly one tenant and cannot name another tenant: the tenant id is *derived from the key*, never taken from the request path, so there is no cross-tenant path to forget to check.
- Least privilege: the API's DB role needs only DML on its own tables (migrations run with a separate role in deployment). **PROPOSED DEFAULT (owner to confirm).** Alternative: subscriber-facing tokens (JWT signed by the tenant) for a hosted checkout and billing portal, a later version.
- Stripe: test keys only. The code refuses any key not starting with `sk_test_`.
- All request bodies are deserialised into typed structs with bounds checks (amount limits, string length, charset on `external_ref`) before reaching the engine.

## 17. Architecture and stack

- **Rust workspace**, three crates plus a payments crate:
  - `stringcan-core`: the pure pricing and ledger engine. No I/O, no clock, no randomness; deterministic. Heavily tested.
  - `stringcan-sim`: simulator binary that drives the engine with seeded random subscribers and prints a break-even report.
  - `stringcan-payments`: the `PaymentProcessor` trait, a deterministic `FakeProcessor` for tests, and a `StripeTestProcessor` stub (builds requests, never calls the network in v0; refuses live keys).
  - `stringcan-api`: axum service on Postgres.
- **Web framework: axum on tokio.** Typed extractors give input validation at the boundary, tower middleware fits auth, and it is the mainstream choice (alternatives: actix-web, similar speed with a different runtime story; poem).
- **DB layer: sqlx (postgres), parameterised queries, embedded migrations.** No ORM: the data model is small, the engine owns the business logic, and SQL stays visible and reviewable. Queries are plain `sqlx::query` with bound parameters (never string-built). Moving to the compile-checked `query!` macros needs a DB at build time or committed `.sqlx` metadata; deferred (section 18).
- **Event/command sourcing.** The engine is deterministic, so the API persists accepted *commands* in an append-only table and rebuilds a tenant's engine by replay under the tenant row lock. This makes the ledger auditable and the engine independent of SQL. Costs: replay time grows (add snapshots later), and changing engine rules changes history, so each command row carries `rules_version`, and a rules change must keep old versions replayable. **PROPOSED DEFAULT (owner to confirm).** Alternative: normalised relational tables mirroring engine state (simpler queries, harder to audit).
- **Snapshots.** Every N commands (default 200) the API stores the serialised engine beside the log. Loading uses the latest snapshot plus the log tail, and ignores a snapshot whose `rules_version` differs, that fails to deserialize, or that fails the engine audit, falling back to a full replay. The log stays the source of truth; a property test checks snapshot plus tail equals full replay.
- **Postgres TLS.** sqlx is built with rustls. Any non-local database host must use `sslmode=verify-full` or the service refuses to start; `STRINGCAN_ALLOW_INSECURE_DB=1` is an explicit opt-in for a sibling container in compose.
- **Processor behind a trait** so Stripe is swappable and tests never touch the network.

## 18. Deferred and open

- Stripe webhooks, SCA / 3-D Secure flows, real PaymentIntents (needs the owner's decision on Connect, section 15).
- Compile-checked SQL (`cargo sqlx prepare` in CI).
- Chargebacks, per-seat variable cost, annual plans, subscriber tokens and portal, email notices of price changes.
- Dunning scheduler (the engine exposes lapse; a job triggers it).
- Rate limiting and request tracing.

## 19. Assumptions made while brainstorming (author answered, owner to confirm)

- A1 fixed upkeep per period (section 5).
- A2 prices only fall within a period as people join; they can rise only by lapses and upkeep revisions.
- A3 one currency per tenant (default USD); no FX.
- A4 a minimum charge exists: processors cannot charge below ~50 cents; if the share is smaller than the minimum, the tenant batches via credit (the subscriber pre-funds credit) or the share is rounded up to the minimum and the excess becomes credit. Handled by credits already; not special-cased in v0.
- A5 no free trials in v0 (a trial would be a participant who does not contribute and distorts the even split).
