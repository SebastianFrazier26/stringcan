# Owner agreement: limited agent of the payee (template)

> **DRAFT FOR LAWYER REVIEW. NOT LEGAL ADVICE.** Drafted by an engineer on 2026-11-23 from
> DESIGN.md section 20.7, item P1 (owner-approved as a design direction on 2026-11-23). It has
> not been reviewed by a lawyer, is not fit to sign, and must be reviewed for each target state
> and country before live mode. Bracketed text is to be completed or decided by counsel.

## Why this exists

stringcan never receives, holds or transmits funds: subscribers pay the app owner's own Stripe
account directly (DESIGN 20.1, 20.2). This agreement is a fallback for US states whose money
transmission laws include an agent-of-the-payee exemption (for example, states that adopted the
CSBS Money Transmission Modernization Act). As summarised in public sources, such exemptions
typically need (1) a written contract between payee and agent, (2) the payee holding the agent out
to the public, and (3) payment to the agent discharging the payer's obligation. Clauses 2, 4 and 5
are aimed at those conditions. Counsel to confirm the wording per state.

## Parties

- **Owner**: [legal name], the operator of [app name] and the merchant of record for its
  subscriptions ("Owner").
- **stringcan**: [legal name of the stringcan operator] ("stringcan").

## 1. Service

stringcan provides software that computes each subscriber's share of the Owner's declared
upkeep, keeps a public ledger, and sends instructions to the Owner's payment processor (Stripe) to
create charges and refunds on the Owner's own Stripe account.

## 2. Limited appointment

The Owner appoints stringcan as its limited agent solely to:

1. compute subscription prices under the published stringcan rules; and
2. instruct Stripe to create charges and refunds on the Owner's Stripe connected account.

stringcan acts only for the Owner. It does not act for any subscriber (see the companion
[no-subscriber-agency rule](no-subscriber-agency.md)).

## 3. No possession of funds

stringcan never receives, holds, controls or transmits the Owner's or any subscriber's funds.
All payments are made directly to the Owner's Stripe account and paid out by Stripe to the Owner's
bank account on the Owner's own schedule. stringcan has no platform balance, receives no
transfers, and takes no application fee or other share of any payment.

## 4. Discharge

A subscriber's payment that succeeds on the Owner's Stripe account fully discharges that
subscriber's obligation to the Owner for the amount paid, whether or not stringcan's ledger has
been updated.

## 5. Holding out

The Owner will name stringcan as its billing service provider, acting for the Owner, in its
subscriber terms and on receipts. [Counsel: exact wording required per state.]

## 6. Owner obligations

The Owner:

- is the merchant of record, and is responsible for tax, disputes, refunds and its own Stripe
  relationship;
- holds the required float (credits, reserve and open pool) as published on the ledger. The
  credits and the reserve are liabilities of the Owner to its subscribers, released only by the
  published rules. stringcan publishes them but cannot lock them;
- honours credit refunds that stringcan instructs under the published rules, which go only to the
  subscriber's own earlier card charges.

## 7. Limits of the authority

stringcan may not move funds between subscribers, between tenants, or to itself, and may not
create a payout or transfer. Refunds are limited to the published credit rules.

## 8. Term, termination, liability, governing law

[Counsel to draft.]
