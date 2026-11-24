# stringcan design

Status: draft v0, 2026-11-23. Rules marked **DEFAULT (owner-confirmed 2026-11-23)** were chosen by the author with a stated alternative, then accepted by the product owner ("they seem ok") on 2026-11-23. Rules marked **PROPOSED DEFAULT (owner to confirm)** are newer (section 20) and still await the owner. Rules without a mark follow directly from the product idea. The owner has also decided: payments go through **Stripe Connect**, and money is **held in the app owner's own bank account** (section 20).

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
- **DEFAULT (owner-confirmed 2026-11-23): monthly periods, billing date = first day of the period, charged in UTC.** Alternative: per-tenant cadence (weekly, annual) or per-subscriber anniversary billing. Anniversary billing breaks the shared-pool model because subscribers would not be funding the same period, so it is rejected for v0.

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

- **Joiner pays the price including themselves (T/(n+1)), not the old price (T/n).** DEFAULT (owner-confirmed 2026-11-23). The literal reading, "pay the old price, then lower it", behaves badly at the start: with one subscriber committed, the next joiner would pay `T/1`, the entire target, and cover the period alone, then the next joiner does the same for the following period, so each period is paid by one rotating person. Because settlement credits equalise net cost, the two readings differ only in cash flow. Alternative: the literal rule plus a cap on any single payment at `T/2`.
- **Overpayment returned as per-person credit applied to the next bill, refundable on request.** DEFAULT (owner-confirmed 2026-11-23). Since rules v2 (2026-11-23) a refund goes only back to the subscriber's own earlier card charges, and credit is capped and swept (section 20.7, P3 to P6). Alternative A: roll the reduction forward as an equal discount for everyone next period (simpler, but early payers subsidise late payers, which breaks "evenly"). Alternative B: immediate cash refund (exact, but every refund costs a processor fee and defeats the point).
- **Water-fill rather than a naive equal split.** If some subscribers paid less than the final level (the price rose after they paid because renewers lapsed), they cannot be charged retroactively. A share is `min(contribution, s)` for the largest level `s` whose shares do not exceed `T`; leftover cents go to participants who paid above `s`. If contributions fall below `T` the period is short (section 7).

### When a period counts as covered

A period is **covered** when contributions reach its target **and** every active renewer has either paid it or been lapsed. Cash alone is not enough: early payers of a falling price overpay, and treating that overshoot as coverage would send the remaining renewers and later joiners to a different period than the early payers. DEFAULT (owner-confirmed 2026-11-23). Alternative: cash-only coverage, which in simulation lets a handful of early payers fund a whole period while the other renewers get a free ride until the next one.

Consequence for the first period: the first subscriber is the only committed person, so they pay the whole target and the period is covered. Joiners after that prefund period 2, where their shares fall as more people join. By the end of period 2 everyone has the same net share. Period 1 is a bootstrap quirk: whoever subscribes first funds it alone. DEFAULT (owner-confirmed 2026-11-23). Alternative: the owner fronts period 1 and recovers it as a carried deficit (section 7), which spreads it over period 2's subscribers.

### Anti-gaming

1. **Waiting does not help.** Net cost for a period is the final level `s`, the same for everyone who contributes, whenever they pay. A joiner who waits for the price to fall pays the lower quote later, but an earlier payer's overpayment is credited back to the same level. Waiting only costs the waiter access.
2. **Join-and-leave.** Contributions are not refundable mid-period (section 11), so joining to see a price and leaving does not extract money. Only credit (overpayment) is refundable.
3. **Late-period joining.** Once the period is covered, joiners fund the next period (section 6). They get no discount for joining late; they do get immediate access, which costs nobody anything because upkeep is fixed (assumption A1).
4. **Quote front-running.** A quote is only a quote. The engine attributes a payment to the funding period when the payment is *recorded*, and settles all shares together, so seeing a quote and paying later changes nothing about the final share.
5. **Skipping a renewal.** A renewer who does not pay is lapsed at settlement and must re-subscribe at the then-current quote. The tenant app grants access only when the subscriber is paid through the current period. If others' payments cover a period without them, the others' level `T/n` is unchanged because the missing renewer was replaced by a joiner, so nobody is worse off.
6. **Sybil joiners** (one person, many accounts) pay one share per account. Detecting duplicates is the tenant app's job.

Assumption A1: upkeep is independent of the number of subscribers within a period. If marginal cost per user matters (e.g. per-seat LLM tokens), the owner models it as a separate per-user cost line in a later version. DEFAULT (owner-confirmed 2026-11-23): fixed upkeep per period; no per-seat variable cost in v0.

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

- **DEFAULT (owner-confirmed 2026-11-23): the reserve is drawn before any deficit is carried; the unrecovered remainder is carried as a deficit.** Alternative: carry the shortfall immediately and keep the reserve only for catastrophic loss (more volatile prices, bigger reserve).
- **Non-payers lapse.** At settlement of `p`, any active subscriber whose paid-through period is before `p` is lapsed (access ends). They can re-subscribe at the then-current quote.

## 8. The reserve

- Sources: (a) the optional top-up built into the period target, (b) nothing else. Overpayment never goes to the reserve; it returns to payers as credit (section 5).
- **Top-up (reserve build).** The target is `T = U + D + top_up`, where `top_up = min(cap - reserve, ceil(U * surcharge_bps / 10_000))`.
- **Cap.** `cap = ceil(U * reserve_cap_bps / 10_000)`.
- **DEFAULT (owner-confirmed 2026-11-23): `surcharge_bps = 0` and `reserve_cap_bps = 5_000` (half a period of upkeep).** With surcharge 0, the reserve stays at zero unless the owner opts in, which keeps the pure "exactly break even" promise literally true. Alternative: a small build surcharge (e.g. 300 bps = 3 percent) until the reserve reaches the cap, trading a slightly higher price for price stability when people leave.
- **Excess above the cap** (the cap shrank because upkeep fell): the excess is *released* as prefund into the next period's pool, which lowers the amount subscribers must pay. Nobody is refunded in cash. **DEFAULT (owner-confirmed 2026-11-23).** Alternative: pro-rata credits to current subscribers.
- The reserve is the tenant pool's money, never the owner's. The owner cannot withdraw from it; it is released only by the rules above.

## 9. Rounding and processor fees

**Rounding.** Shares are integer cents. When `T` does not divide evenly among the participants at the final level, the leftover cents (fewer than the number of participants) are assigned one each to the earliest contributors (by contribution order), who by construction have paid the most. No remainder is retained by anyone; it is a share, not a profit. **DEFAULT (owner-confirmed 2026-11-23).** Alternative: rotate who pays the extra cent each period (fairer over a long horizon, more state).

**Processor fees are upkeep, and the price is grossed up for them.** The ledger is kept in *net* cents (money actually received after fees). A fee model `fee(g) = ceil(g * bps / 10_000) + fixed` (Stripe-like defaults: 290 bps + 30 cents; configurable) estimates fees. To receive `net` cents the subscriber is charged the smallest `g` with `g - fee(g) >= net`. The subscriber sees the fee as a separate line on the quote.

- The actual fee reported by the processor is what the ledger records. Differences between estimated and actual fees flow into the normal settlement (extra net becomes credit, missing net becomes shortfall).
- Credits are denominated in net cents; spending a credit that covers the whole share avoids a charge and therefore a fee.
- Fees are **not** charged on the portion of overpayment returned as credit (the fee on that overpayment is a sunk cost; it is small and is absorbed into the period's pool as part of the quote rounding). **DEFAULT (owner-confirmed 2026-11-23).** Alternative: allocate fees on overpayments back to the overpayers.
- Processors typically do not return their original fee on a refund. **DEFAULT (owner-confirmed 2026-11-23): credit refunds are paid in full, and the unrecovered fee is added to the next period's upkeep as a cost (not modelled in v0).** Alternative: deduct it from the refund.

## 10. Variable or unknown upkeep

- At period open the owner states an **estimate** (default: last period's actual). The estimate is the target the price is computed from.
- Before settlement the owner may revise the estimate (new invoice arrived); later payers see the new price; earlier payers' overpayment or underpayment is handled by water-fill at settlement.
- At settlement the owner supplies the **actual** upkeep, optionally with an evidence reference (invoice id or URL) that appears in the public ledger. The settlement runs against the actual:
  - actual below the total contributed: the difference is credited to payers (true-up down);
  - actual above: shortfall rules (section 7), including drawing the reserve.
- **DEFAULT (owner-confirmed 2026-11-23): the owner self-attests upkeep and evidence is published but not verified.** Alternatives: connect cloud-provider billing APIs to verify; require an invoice upload; a subscriber-vote or audit mechanism. Without verification the "break-even" claim is only as honest as the owner, which is the main trust gap (section 12).
- **Owner pay-out.** On settlement the engine records a payout of `min(contributions, actual + carried deficit)` to the owner. The owner's pay-out can never exceed the declared upkeep plus carried deficit plus reserve top-up; the engine refuses to produce a larger one.

## 11. Cancellations, refunds, failed payments

- **Cancel**: takes effect at the end of the period the subscriber has paid for. They keep access, are excluded from the next period's committed set, and are marked ended at settlement. There is no cash refund of contributions mid-period (they already fund a shared cost that the others are counting on). **DEFAULT (owner-confirmed 2026-11-23).** Alternative: pro-rata refund for a cancellation in the first N days (kills the pool's predictability).
- **Credit refund**: the subscriber's credit balance is refundable at any time through the processor, as a refund to their own earlier charges (rules v2; section 20.7).
- **Failed payment**: the processor's failure leaves no contribution. The subscriber stays committed (counted in the price) while dunning is active; the tenant app retries (default 3 attempts over 7 days). When dunning is exhausted, or at settlement if the subscriber has not paid, they are lapsed and drop out of the committed set, which raises the price for others. **DEFAULT (owner-confirmed 2026-11-23): 7-day grace; the period settles at its end regardless.** Alternative: settlement waits for dunning to end (delays owner pay-out).
- **Chargebacks** (disputes after settlement): modelled as a negative contribution adjustment applied to the current funding period's shortfall and the processor's dispute fee, i.e. it is upkeep. Not implemented in v0; flagged.

## 12. Trust and transparency

The break-even promise is only credible if verifiable.

- A **public ledger** is served without authentication per tenant: every journal entry (contribution, credit use, credit issue, settlement, payout, reserve movement, refund), per-period target, collected, payout, reserve. Subscribers appear as stable per-tenant ordinal pseudonyms (`s-17`), not as external ids or emails; a subscriber can ask the tenant app for their own ordinal.
- Invariants from section 14 are asserted by the engine's `audit()` and served as a `break_even: true/false` flag with the numbers behind it.
- The ledger is derived from an append-only command log. Replaying the log reproduces the state byte-for-byte (the engine is deterministic and has no clock or randomness).
- **DEFAULT (owner-confirmed 2026-11-23): the ledger is public per tenant.** Alternative: ledger visible only to the tenant's subscribers.
- Residual trust: upkeep is self-attested (section 10). Publishing evidence and a third-party audit hook are the roadmap for narrowing it.

## 13. Multi-tenant model

- One stringcan deployment hosts many tenants. A tenant is a fully separate pool: its own periods, subscribers, reserve and ledger. Nothing is shared or pooled across tenants.
- Postgres: every row carries `tenant_id`; the command log is per tenant, and writes for a tenant serialise on a row lock on the tenant row (`SELECT ... FOR UPDATE`) so replay-then-append is race free.
- Subscribers are identified by the tenant's `external_ref` (opaque string); stringcan stores no email or name in v0.
- stringcan takes **no platform fee**. Hosting costs of stringcan itself can be a tenant of stringcan (dogfooding) and is not built into the engine. **DEFAULT (owner-confirmed 2026-11-23).** Alternative: an explicit platform line item in the target.
- Custody: each tenant is a Stripe Connect connected account and funds settle to the owner's own bank account; stringcan never holds money (section 20).

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
11. **Credit stays with its owner** (all versions): each subscriber's credit equals what was issued to them minus what they spent and had refunded, reconciled from the journal by `audit()`; no command moves credit between subscribers, and each tenant is its own engine.
12. **Rules v2**: a renewer's share never exceeds the noticed ceiling; no charge is refunded beyond its amount; after a settlement no subscriber with refundable charges holds credit above their cap or keeps dormant credit.

## 15. Legal and compliance notes (flagged, not decided)

Not legal advice; none of the following is decided. The owner should take advice before taking real money.

- **Money transmission / holding funds.** If stringcan (the operator) holds subscriber money in a reserve, or pays tenants out, that may constitute money transmission or stored-value activity needing licensing (in the US, FinCEN registration and state money transmitter licences; elsewhere, e-money or payment-institution rules). The safest v0 design avoids it: each tenant connects its own Stripe account (or uses Stripe Connect) and the reserve and credit balances are *ledger entries* the tenant is responsible for honouring. Even then, a reserve held by the tenant is a customer-liability. The Connect model and merchant of record are now chosen (section 20); the lawyer review flagged there is still required before real money.
- **Credits and refunds.** Stored credit may be regulated as gift-card or stored value in some jurisdictions and may be subject to unclaimed-property (escheat) rules.
- **Tax.** Sales tax / VAT on subscriptions depends on the subscriber's location; the engine models only the pre-tax share. Tax is charged on top by the processor (Stripe Tax) and kept out of the pool. "At-cost" sharing does not obviously escape income or sales tax; get advice.
- **Consumer protection.** Variable monthly prices need clear disclosure and consent: price notices before renewal, the right to cancel, and in some regions (EU, California auto-renewal law) specific renewal reminders and consent to price changes. Research and options: section 20.8.
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
- Least privilege: the API's DB role needs only DML on its own tables (migrations run with a separate role in deployment). **DEFAULT (owner-confirmed 2026-11-23).** Alternative: subscriber-facing tokens (JWT signed by the tenant) for a hosted checkout and billing portal, a later version.
- Stripe: test keys only. The code refuses any key not starting with `sk_test_`.
- All request bodies are deserialised into typed structs with bounds checks (amount limits, string length, charset on `external_ref`) before reaching the engine.

## 17. Architecture and stack

- **Rust workspace**, three crates plus a payments crate:
  - `stringcan-core`: the pure pricing and ledger engine. No I/O, no clock, no randomness; deterministic. Heavily tested.
  - `stringcan-sim`: simulator binary that drives the engine with seeded random subscribers and prints a break-even report.
  - `stringcan-payments`: the `PaymentProcessor` and `ConnectAccounts` traits, a deterministic `FakeProcessor`, and the Stripe Connect client (`StripeConnect`, over an HTTP `Transport` trait; test-mode keys only, tested against fixtures, never the network).
  - `stringcan-api`: axum service on Postgres.
- **Web framework: axum on tokio.** Typed extractors give input validation at the boundary, tower middleware fits auth, and it is the mainstream choice (alternatives: actix-web, similar speed with a different runtime story; poem).
- **DB layer: sqlx (postgres), parameterised queries, embedded migrations.** No ORM: the data model is small, the engine owns the business logic, and SQL stays visible and reviewable. Queries are plain `sqlx::query` with bound parameters (never string-built). Moving to the compile-checked `query!` macros needs a DB at build time or committed `.sqlx` metadata; deferred (section 18).
- **Event/command sourcing.** The engine is deterministic, so the API persists accepted *commands* in an append-only table and rebuilds a tenant's engine by replay under the tenant row lock. This makes the ledger auditable and the engine independent of SQL. Costs: replay time grows (add snapshots later), and changing engine rules changes history, so each command row carries `rules_version`, and a rules change must keep old versions replayable. **DEFAULT (owner-confirmed 2026-11-23).** Alternative: normalised relational tables mirroring engine state (simpler queries, harder to audit).
- **Snapshots.** Every N commands (default 200) the API stores the serialised engine beside the log. Loading uses the latest snapshot plus the log tail, and ignores a snapshot whose `rules_version` differs, that fails to deserialize, or that fails the engine audit, falling back to a full replay. The log stays the source of truth; a property test checks snapshot plus tail equals full replay.
- **Postgres TLS.** sqlx is built with rustls. Any non-local database host must use `sslmode=verify-full` or the service refuses to start; `STRINGCAN_ALLOW_INSECURE_DB=1` is an explicit opt-in for a sibling container in compose.
- **Processor behind a trait** so Stripe is swappable and tests never touch the network.

## 18. Deferred and open

- SCA / 3-D Secure flows for new payment methods (hosted Checkout in setup mode on the connected account), a payout-schedule policy, live-mode enablement (needs lawyer review, sections 15 and 20).
- Compile-checked SQL (`cargo sqlx prepare` in CI).
- Chargebacks, per-seat variable cost, annual plans, subscriber tokens and portal, email notices of price changes.
- Dunning scheduler (the engine exposes lapse; a job triggers it).
- Rate limiting and request tracing.

## 19. Assumptions made while brainstorming (author answered, owner-confirmed 2026-11-23)

- A1 fixed upkeep per period (section 5).
- A2 prices only fall within a period as people join; they can rise only by lapses and upkeep revisions.
- A3 one currency per tenant (default USD); no FX.
- A4 a minimum charge exists: processors cannot charge below ~50 cents; if the share is smaller than the minimum, the tenant batches via credit (the subscriber pre-funds credit) or the share is rounded up to the minimum and the excess becomes credit. Handled by credits already; not special-cased in v0.
- A5 no free trials in v0 (a trial would be a participant who does not contribute and distorts the even split).

## 20. Custody, Stripe Connect and money flows

Status: **ACCEPTED** (owner, 2026-11-23). Owner decisions, 2026-11-23: use **Stripe Connect**; money is **held in the app owner's own bank account**; the plan below (Standard accounts, direct charges, owner as merchant of record, credits and reserve as ledger-only claims, no application fee) is approved as written; **Plaid is approved** for reading the owner's bank account (20.4). The owner recorded positions on the 20.5 questions on 2026-11-23 (owner positions, not legal advice); a lawyer still reviews them before live mode, and the analysis and proposed changes are in 20.7 to 20.9. Rules still marked PROPOSED DEFAULT inside this section (20.3 float publication and credit-refund limit, 20.4 balance rule) await the owner. Nothing here is legal advice (section 20.5).

### 20.1 Shape

Each tenant (app owner) is a Stripe **connected account**. Subscribers pay the tenant's connected account directly; Stripe pays out to the owner's bank account on the owner's own payout schedule. stringcan is a software layer that computes prices, asks Stripe to create the charge on the owner's account, and keeps the ledger. **stringcan never has a balance, never receives a transfer, and never takes an application fee** (consistent with "no cut").

```
subscriber card --charge (direct, on the connected account)--> owner's Stripe balance --payout--> owner's bank
        ^                                                          ^
 stringcan computes price, calls Stripe API using its platform key,   stringcan only keeps the ledger of who is owed what
 with the Stripe-Account header set to the owner's acct_...
```

### 20.2 Account type and charge type

| choice | picked | why | alternative |
|---|---|---|---|
| connected account type | **Standard** | The owner has a full Stripe account and dashboard, completes Stripe's own KYC and onboarding, owns disputes, refunds, tax settings and the bank link. Stripe, not stringcan, carries most compliance load for the account. stringcan needs no fee, so Standard's inability to take platform-controlled fees costs nothing. | **Express**: Stripe-hosted onboarding and a lighter dashboard, and the platform can set payout schedules, but the platform takes on more responsibility (losses, support, branding). **Custom**: the platform owns the whole experience and the regulatory exposure; reject for v0 |
| charge type | **Direct charges** on the connected account | The owner is the merchant of record. The charge, the Stripe fee, the dispute and the refund all land on the owner's account, which matches "fees are upkeep" (section 9) and keeps funds off any platform balance. | **Destination charges** / separate charges and transfers: funds pass through the platform's balance before reaching the owner. That makes stringcan the merchant and a holder of funds, which is exactly what section 15 tries to avoid |

Consequences of Standard plus direct:

- Customers and saved payment methods live on the **connected** account (a customer object is per account). The subscriber's first payment method is collected by the owner's app with Stripe Checkout in setup mode (or Elements) on that account; the owner then registers the resulting `cus_` and `pm_` ids with stringcan. Renewals are off-session charges with the stored method.
- Every API call that acts for a tenant carries the `Stripe-Account: acct_...` header. A tenant cannot act on another tenant's account because the account id is read from the tenant row, derived from the owner's API key, never from the request.
- Onboarding uses Stripe **Account Links** (`account_onboarding`). The tenant's capability to charge is only trusted once Stripe reports `charges_enabled`, delivered by the `account.updated` webhook.
- stringcan's platform secret key is test-mode only until the owner approves live mode (the code refuses non-`sk_test_` keys). Because the key can act on all connected accounts, it is the most sensitive secret in the system; it lives only in the deployment secret store.

### 20.3 Where credits and the reserve live

With money sitting in the owner's Stripe balance and bank account, **credits and the reserve are ledger-only claims**, not separate pots of cash:

- A *credit* is a liability of the tenant to one subscriber, recorded in the stringcan ledger.
- The *reserve* is a tenant-level liability to the subscriber pool (it is released only by the rules in section 8).
- The *open pool* (collected for periods not yet settled) is money the owner has received but not yet earned against declared upkeep.

The owner physically holds the cash for all three. The engine already computes the exact amount the owner must hold: the **required float** = `reserve + credits_outstanding + open_pool` (the same terms as invariant 4). The public ledger will show it. This is a promise, not a lock: stringcan cannot stop an owner spending it (that would require stringcan to control funds).

- PROPOSED DEFAULT (owner to confirm): the required float is published, and the owner is asked to keep at least that much in their Stripe balance or linked bank account. Optional verification (20.4) turns that into something a subscriber can trust. Alternative: Stripe payouts set to manual with a holdback, which gives real lock-up but needs Express or Custom accounts (platform-controlled payout schedules), pushing stringcan toward holding funds in effect.
- **Refunding credit.** A cash refund of credit is a Stripe refund created on the connected account against the subscriber's most recent successful charge, up to that charge's amount. It returns the money to the card, debits the owner's balance, and Stripe does not return the original processing fee (consistent with section 9: that fee is a cost). Rules v1 refund credit only up to the latest charge and reject a larger credit. **Rules v2 (owner, 2026-11-23, P3):** the refund is split across the subscriber's own recent charges, newest first, each leg capped at what that charge has not yet had refunded; see 20.7.
- **Chargebacks** hit the owner's account. They are not yet modelled in the engine (section 11); the webhook handler records `charge.dispute.created` events so they are not lost.
- If an owner stops operating or abandons the tenant, credits and reserve are an unsecured claim on the owner. Said plainly in subscriber-facing terms, this is the residual trust cost of "money in the owner's account" and the reason the float is published and optionally verified.

### 20.4 Plaid, Financial Connections, and verifying upkeep

Two different jobs are bundled under "maybe Plaid":

1. **Linking a bank account.** Not needed: Stripe Connect Standard onboarding already collects and verifies the owner's payout bank account. Using Plaid for this would add a second vendor for no gain.
2. **Verifying the owner's claims**: (a) that the owner actually holds the required float, and (b) that declared upkeep is real.
   - For (a), **Stripe Financial Connections** (Stripe's own account-linking product, built on the same Connect relationship) can read a linked bank account's balance with the owner's consent. Plaid does the same job as a separate integration.
   - For (b), neither verifies a bill. Plaid Transactions could match outgoing payments to known hosting vendors (a partial, privacy-heavy signal). Invoice upload or cloud-provider billing APIs prove upkeep more directly.

**Decision, 2026-11-23: the owner approved Plaid** for job 2(a), reading the balance of the owner's own bank account, over Stripe Financial Connections. The original recommendation is kept below for the record; where it says Financial Connections for the verified-float badge, read Plaid.

Implemented (this branch, sandbox only): `stringcan-payments::plaid`, a `BankLink` trait (create a Link token, exchange the public token, read real-time balances via `/accounts/balance/get`, revoke via `/item/remove`) over a `JsonTransport` seam, plus `verify_float`, which returns only Verified / Short / Unverifiable, never amounts.

- Security posture: sandbox only (`PLAID_ENV` must be `sandbox`, and the live transport is hard-wired to `sandbox.plaid.com`); credentials sent as `PLAID-CLIENT-ID` / `PLAID-SECRET` headers by the transport, so request bodies and fixtures never contain them; config and access token redact in `Debug` and the token has no `Display`; Plaid's free-text error messages are dropped, only `error_type` / `error_code` are kept; `client_user_id` must be an opaque ref (no email or name).
- The Plaid **access token is a credential, not an opaque id** (it grants ongoing read access to the owner's bank data), so the "store only opaque ids" rule in 20.5 cannot cover it as is. It is stored encrypted (owner decision below).
- PROPOSED DEFAULT (owner to confirm): an account counts its `available` balance, falling back to `current` only when the bank reports no `available`; only depository accounts count (credit, loan and investment never do); a missing balance or a currency other than the tenant's makes the check Unverifiable rather than guessed. Alternative: count `min(available, current)`, or `current` only (higher, includes pending debits not yet cleared).
- **Owner decisions, 2026-11-23:**
  - **Access token storage:** encrypted in Postgres (`bank_links`), sealed with an AEAD (XChaCha20-Poly1305, RustCrypto `chacha20poly1305`) under a key from the secret store (`STRINGCAN_TOKEN_KEY`, an env var in dev; `.env.example` carries a dummy). The tenant id is associated data, so a ciphertext moved to another tenant's row does not decrypt. A `token_key_id` column names the sealing key; retired keys (`STRINGCAN_TOKEN_KEYS_RETIRED`) still decrypt, and a row is re-sealed under the current key the next time it is read. XChaCha20 was picked over AES-256-GCM because its 24-byte nonce is safe to draw at random.
  - **Link product: `auth` only.**
  - **Only the account that receives Stripe payouts counts toward the float.** The owner names that account (the Plaid `account_id` picked in Link) when exchanging the public token; it must be a depository account on the item, and every balance read and float check is restricted to it.
- Implemented (sandbox only): `POST /v1/bank/link-token`, `POST /v1/bank/link` (exchange plus payout account; replaces and revokes any earlier item), `DELETE /v1/bank/link` (revokes at Plaid, deletes the row), `GET /v1/bank/float` (on-demand check, returns `verified` / `short` / `unverifiable` and never amounts). The latest result and its time are shown on the public ledger as `float_check`. A bank needing re-login, or a revoked link, reads as unverifiable rather than as an error. The routes are off unless `PLAID_CLIENT_ID` is set.
- PROPOSED DEFAULT (owner to confirm): **re-check daily plus on demand.** v0 has only the on-demand route; the daily job is not built (no scheduler yet, section 18). Alternative: on demand only, or before each settlement.
- PROPOSED DEFAULT (owner to confirm): **Plaid webhooks** are JWT-signed (ES256, key fetched from `/webhook_verification_key/get`); proposed: verify the JWT and the body hash, handle only `ITEM` errors and `PENDING_DISCONNECT` / `LOGIN_REPAIRED` to flip the published status to unverifiable, ignore the rest. Not built.
- Not yet enforced: that the named account is in fact the Stripe payout account. It is owner-declared. PROPOSED follow-up: with `auth`, compare the account's routing number and last four digits (`/auth/get`) with the Stripe external account on the connected account, and refuse a mismatch. Alternative: keep it owner-declared and say so on the public ledger.

Original recommendation (superseded on the Plaid-vs-Financial-Connections point):

- v0 (now): no Plaid and no Financial Connections. Publish the required float; publish upkeep evidence references (already supported).
- v1: add **Stripe Financial Connections** as an opt-in "verified float" badge: the owner links the bank account that receives payouts and stringcan periodically reads the balance, publishing only a pass/fail against the required float, never account details.
- v2, only if there is demand: upkeep verification by invoice attachment and cloud-billing APIs (AWS, Railway, Vercel) before considering **Plaid Transactions**, whose cost in consent, data minimisation and US/EU data-protection scope is high for a weak signal.
- Alternative: Plaid for both jobs now. Rejected because it adds vendor and privacy surface before the core flow is proven.

### 20.5 Compliance notes (flagged for a lawyer, not decided)

Which choices help avoid stringcan being treated as a money transmitter or payment institution. None of this is settled; it needs a lawyer in each jurisdiction before live mode.

- **Choices that point away from transmitting:** funds never enter an account stringcan controls (direct charges); no platform balance, no transfers to or from stringcan, no application fee; the owner is merchant of record and holds the Stripe relationship (Standard); stringcan sends instructions to Stripe but cannot itself move owner funds (it can create refunds on the connected account, which is a limited power worth scoping: see below); Stripe is the regulated entity processing payments.
- **Questions for counsel, with the owner's positions (recorded 2026-11-23).** The owner answered each question with their own position. These are **owner positions, not legal advice and not a legal conclusion**; the gate stays: **a lawyer reviews each before live mode.** Consequences and proposed design changes are in 20.7; item 4 research is in 20.8.
  1. *Charges and refunds on an owner's behalf.* Question: whether creating charges and refunds on an owner's behalf, with the platform key, is "acting as agent" for a payment in a way that triggers licensing in any target state or country (direct charges are the standard SaaS-platform pattern, but the pooled, redistributed credit is unusual). **Owner position: yes, stringcan acts as a payment agent (of the owner).** Status: position recorded; counsel review open. See 20.7 for why "agent" needs care.
  2. *Credits.* Question: whether credits (owner-owed value redeemable for cash or future service) are regulated as stored value / gift card / prepaid access, plus unclaimed-property (escheat) and expiry rules. **Owner position: credits are regulated as prepaid balances, and they can be cashed out later (redeemable for cash).** Status: position recorded; counsel review open (prepaid-access exclusions, state gift-card and escheat rules). See 20.7.
  3. *Reserve.* Question: whether the reserve (a held-back customer fund inside the owner's account) creates trust-account or segregation duties for the owner. **Owner position: it should not create trust-account or segregation duties.** Status: position recorded; counsel review open. See 20.7 for wording in this doc that pulls the other way.
  4. *Variable monthly prices.* Question: consumer-law treatment of variable monthly prices (notice, consent, auto-renewal rules in the EU and California). **Owner position: needs research before deciding. OPEN.** Research done 2026-11-23, see 20.8; the notice/consent design awaits the owner.
  5. *Tax.* **Owner confirmed (2026-11-23): the owner is merchant of record and tax sits on the owner; Stripe Tax on the connected account calculates tax on top of the share, outside the pool.** The engine continues to model only the pre-tax share. Counsel still confirms the owner's own registration duties per jurisdiction.
  6. *Platform funding.* Question: the platform's responsibility under Stripe's Connect terms, and how stringcan pays its own upkeep with "no application fee". **Owner position (2026-11-23): stringcan is free; its upkeep is paid by donations. No application fee, no cut.** How donations work without breaking "no cut" is an open owner question, see 20.9. Stripe's Connect platform terms are still to be read by counsel.
- **Scoping the platform key:** the platform key can create charges on all connected accounts. Practical mitigations: keep it test-only until review, restrict it with Stripe restricted keys to the needed resources (accounts, account links, payment intents, refunds), store it only in the secret manager, and never log it (the code redacts it in Debug).
- **Data:** only opaque Stripe ids (`cus_`, `pm_`) are stored; no card data, no emails. The one stored credential is the Plaid access token, encrypted (20.4).

### 20.6 Implementation (this branch)

- `stringcan-payments`: `Transport` trait (HTTP boundary); `StripeConnect<T>` implementing `PaymentProcessor` and the new `ConnectAccounts` trait (create Standard account, Account Link, account status); direct-charge PaymentIntents with `Stripe-Account` and `Idempotency-Key` headers and an expanded balance transaction so the real fee is recorded; refunds; webhook signature verification (`Stripe-Signature`, HMAC-SHA256 over `timestamp.payload`, constant-time compare, 5 minute tolerance) and typed event parsing. A `FixtureTransport` serves canned responses in tests; a `ReqwestTransport` is the only code that touches the network and is never used by tests.
- `stringcan-api`: `POST /v1/connect/account`, `POST /v1/connect/onboarding-link`, `GET /v1/connect/status`, `PUT /v1/subscribers/{ref}/payment-method`, and `POST /v1/webhooks/stripe` (signature-verified, event-id idempotent, handles `account.updated`, records the rest). Payments for a Connect tenant are refused until `charges_enabled`.
- Test fixtures are **hand-written to Stripe's documented shapes**, not recorded from live traffic, because no test key was available. Replace them with recorded test-mode responses once the owner provides a Stripe test account (and re-run the tests).
- Plaid (sandbox, fixture-tested, not wired into the API): see 20.4. Fixtures are hand-written to Plaid's documented sandbox shapes; replace with recorded sandbox responses once the owner provides sandbox keys.

### 20.7 Consequences of the owner positions (analysis, not legal advice)

Researched 2026-11-23. This is an engineer's reading of public sources, **not legal advice**; laws and their status change, so re-check before relying on it. Sources and how strong each is are listed at the end of 20.8. Nothing below is adopted: the "proposed design changes" are for the owner to accept or reject.

**(1) "stringcan acts as a payment agent."** Calling stringcan an agent has real consequences, and the safer framing may be narrower than the owner's.

- *US federal (FinCEN).* A money transmitter does not include a person that only "acts as a payment processor to facilitate the purchase of, or payment of a bill for, a good or service" by agreement with the seller, or that only "accepts and transmits funds only integral to the sale of goods or the provision of services" (31 CFR 1010.100(ff)(5)(ii)(B) and (F)). stringcan never accepts or transmits funds at all, which is a stronger position than relying on either exclusion.
- *US states.* Most states that adopted the CSBS Money Transmission Modernization Act (MTMA; 27 states fully or partly by May 2025, per secondary sources) carry an **agent-of-the-payee exemption**. As summarised for Colorado's 2025 version, it has three conditions: a written contract between payee and agent authorising the agent to collect payments; the payee holds the agent out to the public (terms, receipts); and payment to the agent discharges the payer's obligation to the payee. Not every state has the exemption or words it the same way, and it covers payments for goods and services, not general transmission. Its premise is that the agent *receives* money. stringcan does not, so the exemption is a fallback, not the main argument.
- *EU.* PSD2 Article 3(b) excludes payments "through a commercial agent authorised via an agreement to negotiate or conclude the sale or purchase of goods or services on behalf of only the payer or only the payee". stringcan neither negotiates nor concludes sales, so it fits this poorly, and the PSD3/PSR texts agreed in 2025 and endorsed by Council in April 2026 (not yet in force as far as found) narrow it further and target platforms. The better-fitting EU exclusion is the **technical service provider** one (PSD2 Article 3(j): providers supporting payment services without entering into possession of the funds; wording recalled, not re-fetched this session).
- *Plain reading.* The owner's answer is right that stringcan acts *for the owner only* (never for the subscriber) and that matters. But "payment agent" suggests stringcan is in the money flow. The design keeps it out: direct charges, no platform balance, no transfers. The consistent position is "software and technical service provider acting on the owner's instructions; if a regulator treats it as an agent, it is an agent of the payee only".

**(2) "Credits are prepaid balances, redeemable for cash."** If credits are prepaid value, who the provider is and whether an exclusion applies decide everything.

- *FinCEN prepaid access.* "Prepaid access" is "access to funds or the value of funds that have been paid in advance and can be retrieved or transferred" (31 CFR 1010.100(ww)). An arrangement is not a "prepaid program" if it provides **closed loop** prepaid access (usable only with a defined merchant) of no more than **$2,000** per device on any day ((ff)(4)(iii)(A)), or open-loop access capped at **$1,000** with no international use, no person-to-person transfers and no non-depository loads ((ff)(4)(iii)(D)). Credits usable only for the owner's own service are closed loop. The catch is **cash**: FinCEN's reasoning is that closed loop value is not a medium for later cash withdrawal, and its 2016 FAQ (FIN-2016-G002) treats only state-required cash refunds as de minimis. "Redeemable for cash" in general weakens the closed-loop exclusion. Refunding an overpayment back to the original card, capped at the original charge, is closer to a purchase refund than a cash withdrawal; that distinction is for counsel, and the FAQ does not address it.
- *Who is the provider.* The provider is the participant acting as "principal conduit" with principal oversight and control. The owner issues the credit and owes it, but stringcan runs the ledger and computes balances, which makes it a plausible candidate. That is the main reason to cap credits and keep redemption on the owner's account.
- *EU e-money.* E-money is monetary value "accepted by a natural or legal person other than the electronic money issuer" (EMD2 Article 2(2); recalled, not re-fetched). Credits accepted only by the owner who owes them are not e-money. They would drift toward it if credits became transferable or spendable at other tenants.
- *States.* Gift-card and stored-value statutes and unclaimed-property (escheat) rules apply to the owner as issuer, and vary by state (some require cash redemption of small balances, ban expiry, or require escheat of dormant balances). Not researched per state here.

**(3) Reserve without trust or segregation duties.** No statute found imposes trust accounting on an ordinary merchant for a voluntary price buffer. The risk is self-inflicted wording: section 8 says the reserve "is the tenant pool's money, never the owner's" and "the owner cannot withdraw from it". Language like that, published to subscribers, can be read as a declaration of trust in favour of the pool, which is exactly the duty the owner wants to avoid, and it contradicts 20.3 (the float is "a promise, not a lock"). Consistent wording would call the reserve an **owner liability to the pool, released only by the published rules**.

**Proposed design changes.** Owner decision 2026-11-23: **P3, P4, P5 and P6 ADOPTED** and implemented as rules v2 (below). P1, P2, P7 and P8 are still proposed and unanswered.

- P1. **Agent-of-payee terms with each owner.** The owner agreement appoints stringcan as the owner's limited agent for computing prices and instructing Stripe to create charges and refunds on the owner's account; states that stringcan never receives, holds or transmits funds; and states that a subscriber's payment to the owner's Stripe account discharges the subscriber's obligation. Owner-facing subscriber terms and receipts name stringcan as the owner's service provider. Cheap belt-and-braces for (1) in MTMA states.
- P2. **Never act for subscribers.** No subscriber-side agency: stringcan does not hold subscriber payment credentials, does not move money between subscribers, and has no subscriber wallet. (Already true; P2 makes it a written invariant.)
- P3. **ADOPTED 2026-11-23.** **Credits redeemable only from the owner's own Stripe account, only as a refund to the original payment method**, capped at the original charge (already the 20.3 v0 rule; P3 makes it permanent rather than a v0 limitation). No bank transfers, no cash, no payout to a different person.
- P4. **ADOPTED 2026-11-23, cap = max($100, two of the subscriber's shares).** **Credit cap per subscriber per tenant**, well under the $2,000 closed-loop line: e.g. the larger of $100 or two periods' shares; anything above the cap is refunded automatically to the card instead of being held. Keeps every subscriber's balance small and short-lived.
- P5. **ADOPTED 2026-11-23.** **Credits are non-transferable and tenant-scoped**: never moved between subscribers, never usable at another tenant, never pooled across tenants (keeps them closed loop and out of EU e-money).
- P6. **ADOPTED 2026-11-23.** **Dormant-credit sweep**: credits unused for N periods after a subscriber ends are refunded to the card automatically, which shrinks escheat exposure.
- P7. **Reserve wording**: change section 8 from "the tenant pool's money, never the owner's ... cannot withdraw" to "an owner liability to the pool, released only by these rules; stringcan publishes it but cannot lock it" (consistent with 20.3 and owner position 3).
- P8. **EU framing**: rely on "never in possession of funds" (technical service provider) rather than the commercial-agent exclusion.

**Rules v2: how P3 to P6 are implemented (2026-11-23).** Each tenant carries a rules version in its engine config, fixed at creation; tenants stored before this change are v1 and replay under the v1 rules, new tenants are v2. In v2:

- Every card payment records its processor charge id with the subscriber (a payment without one is refused). Credit refunds, whether requested, over the cap, or dormant, are split across that subscriber's own charges, newest first, never more than a charge's unrefunded amount (P3). The API makes one Stripe refund per leg on the owner's connected account before it records the command, so a failed refund records nothing.
- At each settlement, a subscriber's credit above `max(credit_cap_floor, 2 * last settled share)` is refunded to their charges (P4). `credit_cap_floor` defaults to 10,000 cents ($100).
- No command moves credit between subscribers; `audit()` reconciles each subscriber's credit with their own journal entries, and tenants never share an engine (P5).
- At each settlement, a lapsed or ended subscriber's whole credit is refunded once `dormant_after_periods` have passed since the last period they paid for (P6).
- PROPOSED DEFAULT (owner to confirm): `dormant_after_periods = 3`. Alternative: 1 (sweep at once) or 12.
- PROPOSED DEFAULT (owner to confirm): the engine remembers a subscriber's last **6** charges as refund targets, since card refunds get harder as charges age (Stripe's exact refund window was not checked). Alternative: remember all charges.
- Known gap: credit above the cap, or dormant credit, that no remembered charge can absorb stays as credit and is flagged by nothing yet. Possible fix: list such balances for the owner to settle by hand.
- Note: with a large share, two shares can exceed FinCEN's $2,000 closed-loop line (20.7). Possible fix: also cap at an absolute ceiling below $2,000.

### 20.8 Variable monthly prices: notice and consent (research; OWNER DECISION NEEDED)

Researched 2026-11-23; not legal advice. The issue: a renewer's charge is the live quote on the billing date (section 4), which falls as people join and can rise when people lapse or upkeep is revised (assumption A2). Several regimes require advance notice of a changed amount, or treat unilateral price changes as unfair unless the method is clear.

| regime | status (2026-11-23) | what it requires that touches a variable price |
|---|---|---|
| **EU** Unfair Contract Terms Directive 93/13 (Annex points (j), (l)) and CJEU case law (*Invitel* C-472/10, 2012; *RWE Vertrieb* C-92/11, 2013; recalled, not re-fetched) | in force | A term letting the trader change the price is at risk of unfairness unless the contract states the reason and the **method** of variation transparently enough for the consumer to foresee changes, and the consumer can terminate. National rules add more (e.g. Germany limits terms after the minimum period). |
| **EU** Consumer Rights Directive, new Article 11a withdrawal function (Directive 2023/2673) | applies from 19 June 2026; transposition uneven across member states | Contracts concluded online need an online "withdraw from contract" function during the withdrawal period. |
| **EU** Digital Fairness Act | **proposal only**, published by the Commission 11 November 2026 | Per press coverage: easier cancellation, auto-renewal as a conscious choice, reminders before free trials convert. Price-change provisions not confirmed in what was found. Will change in Parliament and Council. |
| **California** Automatic Renewal Law, Bus. & Prof. Code 17600 ff., as amended by AB 2863 (2024) | in force for contracts entered, amended or extended from 1 July 2025 | **Express affirmative consent** to the renewal terms, separate from the rest; disclosure before billing info is confirmed of the amount or range of charges; **notice of a change in the amount charged between 7 and 30 days before it takes effect**, with how to cancel; **annual reminder**; cancel online in the same medium as sign-up, with a "click to cancel" link where save offers are shown; consent records kept 3 years or 1 year after termination, whichever is longer. |
| **US federal** FTC Negative Option ("Click-to-Cancel") Rule | the 2024 rule was **vacated** by the Eighth Circuit in July 2025; the FTC published an ANPRM to restart rulemaking in March 2026 (comments closed 13 April 2026); no new rule in effect as found | **ROSCA** still applies and is enforced: clear disclosure of material terms before billing, express informed consent, simple cancellation. Other states have their own auto-renewal laws (not surveyed). |

The tightest constraint is California's 7-to-30-day notice of a change in amount. A price that floats until the billing date cannot meet it literally unless the consent covered the formula and range and every charge stays within what was noticed.

**Design options:**

- **N1. Formula consent only.** Consent at sign-up to "your share of declared upkeep divided by subscribers, between $a and $b"; no per-period notice. Simplest; relies on the argument that a charge within a disclosed formula and range is not a "change". Weakest under California if a charge moves, and the range must be honest.
- **N2. Noticed ceiling per period (price lock).** 7 to 30 days before each billing date (default 10), each renewer is sent the period's **ceiling** = the current quote (plus fee gross-up). On the billing date the charge is `min(live quote, noticed ceiling)`. Prices can only fall after notice. If lapses after the notice push the true share above the ceiling, the gap is a shortfall that the reserve, then the carried deficit, absorbs (section 7), so the next period's notice is higher. Water-fill (section 5) already equalises net cost, so no subscriber pays more than their notice.
- **N3. Ceiling plus subscriber cap.** N2, plus each subscriber sets a maximum monthly charge at consent. A ceiling above that cap is not charged: the subscriber gets a notice asking for fresh express consent, and without it they are not renewed (they lapse at settlement, no charge). Covers EU transparency and termination and California consent in one mechanism.
- **N4. No auto-renewal.** Every period is a fresh opt-in payment. Outside auto-renewal law entirely, but renewers become joiners each month, which breaks the committed-set model and the price falls apart.

**Owner decision, 2026-11-23: N3 with a 10-day ceiling notice. Implemented in rules v2:**

- `IssueNotices { period }` (API `POST /v1/periods/{id}/notices`, the notifications job) logs, for every renewer who owes the period, a ceiling equal to the live share at that moment, and journals it as a public `PriceNotice` entry. A ceiling, once noticed, is never raised; re-running notices only reaches renewers who have none.
- A renewal (an active subscriber paying a later period than they last paid) is refused without a notice for that period. Its share is `min(live share, ceiling)`. If people leave after the notice, the gap is a shortfall that the reserve and then the carried deficit absorb (section 7).
- `SetRenewalCap` (API `PUT /v1/subscribers/{ref}/renewal-cap`) records the subscriber's express consent to renew up to a gross amount. The command row and its timestamp are the consent record. A renewal whose ceiling, fees included, is above the cap (or with no cap on file) is refused until fresh consent raises the cap; without it the subscriber lapses at settlement and is not charged.
- New and lapsed subscribers paying are on-session purchases at the live quote and need no notice.
- Delivery: a `Notifier` trait. `LogNotifier` (the default) only logs that a notice was due, without the subscriber ref; `RecordingNotifier` is the test double. Notices are delivered before the command commits, so a delivery failure records nothing. **No real email or other channel is built.**
- Not enforced yet: the 10-day lead. The engine has no clock and there is no scheduler, so the caller must run the job 10 days before the billing date (7 to 30 for California). The annual reminder and cancel-online flow belong to the owner's app and are not built here.

Original recommendation, kept for the record: **N3** (N2 plus a subscriber cap), with a 10-day notice default, an annual reminder, cancel-online in the tenant app, and the formula, range and cap shown at consent. It fits the engine's existing machinery (water-fill and carried deficit absorb the ceiling gap) and needs one new engine rule: the charge is capped at the noticed ceiling, and the notice is an event in the command log so the ceiling is auditable in the public ledger. Cost: a small, bounded owner float risk between notice and billing, and a notifications job (already listed as deferred, section 18). **Owner decision needed**; counsel to review the consent and notice text per jurisdiction.

**Sources (accessed 2026-11-23; primary marked P, secondary S):**
P: 31 CFR 1010.100 (via Cornell LII, https://www.law.cornell.edu/cfr/text/31/1010.100); FinCEN FAQ on prepaid access FIN-2016-G002 (https://www.fincen.gov/resources/statutes-regulations/guidance/frequently-asked-questions-regarding-prepaid-access); PSD2 Article 3(b) (EUR-Lex CELEX 32015L2366).
S: MTMA agent-of-payee conditions (Colorado HB25-1201 summary, bhfs.com; Cooley 2024; Stinson on Massachusetts); PSD3/PSR status and commercial-agent changes (Freshfields, Taylor Wessing, DLA Piper 2026); California AB 2863 (Fenwick, Davis Wright Tremaine, Kilpatrick Townsend; leginfo.legislature.ca.gov refused automated access, so the statute text was not read directly); FTC rule status (Gibson Dunn, Jones Day 2026); CRD Article 11a (Hogan Lovells, Heuking 2026); Digital Fairness Act (Euronews, 11 November 2026).
Recalled, not re-fetched this session: PSD2 Article 3(j), EMD2 Article 2(2), UCTD Annex, CJEU cases.

### 20.9 Donations without a cut (proposal; OWNER QUESTION)

Owner position 6: stringcan is free and funded by donations. The "no cut" claim must stay literally true and auditable (section 15), so a donation must never ride on a subscriber's charge to an owner, never pass through an owner's Stripe account, and never be a condition of using stringcan.

Options (not built):

- **D1. External sponsorship** (GitHub Sponsors or a fiscal host such as an open-source collective): donations go to stringcan's maintainer outside the product. No code in the money path, no Connect involvement, no effect on any tenant ledger. Simplest and cleanest.
- **D2. Dogfooding: stringcan as a tenant of stringcan.** stringcan's own hosting upkeep is a tenant whose supporters subscribe at cost, with its own public ledger. Shows the product working and keeps the platform "exactly break even". These are supporter subscriptions, not donations, and they run on a separate Stripe account the maintainer owns, never the platform account.
- **D3. A separate donation page on the maintainer's own Stripe account** (Payment Links), linked from the docs and footer, never from inside a tenant's checkout.
- **Rejected**: tips or "round up for stringcan" in a tenant's checkout, or any application fee, because that is a cut and puts stringcan in the money flow.

**Owner decision, 2026-11-23: D1 now (GitHub Sponsors); D2 later**, once live mode is approved, with stringcan as its own tenant. A `.github/FUNDING.yml` is proposed but deliberately not committed until the GitHub Sponsors account exists. **Still open:** who receives donations, an individual (possibly taxable income, not tax-deductible for donors) or a fiscal host.
