# Rule: stringcan never acts for subscribers (template)

> **DRAFT FOR LAWYER REVIEW. NOT LEGAL ADVICE.** Drafted by an engineer on 2026-11-23 from
> DESIGN.md section 20.7, item P2 (owner-approved on 2026-11-23). It records a design invariant
> the code already meets and suggests public wording. A lawyer must review the public wording
> before live mode.

## The rule (engineering invariant)

stringcan acts only on the app owner's instructions and never as agent, custodian or wallet for a
subscriber. Concretely, and enforced in code (DESIGN 20.7, rules v2 and v3):

1. stringcan stores no subscriber payment credentials. Card data stays at Stripe; stringcan keeps
   only opaque Stripe ids (`cus_`, `pm_`) that belong to the owner's connected account.
2. stringcan never moves money or credit between subscribers. No command transfers credit, and the
   engine audit reconciles each subscriber's credit with their own journal entries.
3. stringcan has no subscriber wallet or balance that it holds. A credit is the owner's liability
   to one subscriber, usable only with that owner, and refundable only to that subscriber's own
   earlier card charges.
4. Credits are never usable at another tenant and are never pooled across tenants.

A change that breaks any of these is a design change for the owner and counsel. It is not a
refactor.

## Suggested public wording

> stringcan is a billing service used by [app name]. It works for [app name] only, not for you.
> Your payment goes straight to [app name]'s own payment account. stringcan never holds your money
> or your card details. Any credit on your account is owed to you by [app name], can be used only
> for [app name], and is refunded only to the card you paid with.

[Counsel: confirm the wording for consumer-protection and prepaid-access purposes in each target
jurisdiction.]
