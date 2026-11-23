-- Stripe Connect (docs/DESIGN.md section 20). Standard connected accounts with
-- direct charges: only opaque ids are stored, never card data.
ALTER TABLE tenants
    ADD COLUMN stripe_account_id TEXT UNIQUE CHECK (stripe_account_id ~ '^acct_[A-Za-z0-9_]{4,64}$'),
    ADD COLUMN stripe_charges_enabled BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN stripe_payouts_enabled BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN stripe_details_submitted BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE subscribers
    ADD COLUMN stripe_customer TEXT CHECK (stripe_customer ~ '^cus_[A-Za-z0-9_]{4,64}$'),
    ADD COLUMN stripe_payment_method TEXT CHECK (stripe_payment_method ~ '^pm_[A-Za-z0-9_]{4,64}$');

-- The primary key is the idempotency guard: Stripe retries, we process once.
CREATE TABLE webhook_events (
    event_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    stripe_account_id TEXT,
    payload JSONB NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
