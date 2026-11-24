-- When each renewal price notice was issued (DESIGN 20.8). The engine has no
-- clock, so the 10-day lead is enforced here: a renewal is charged only once its
-- notice is old enough. Written in the same transaction as the IssueNotices command.
CREATE TABLE price_notices (
    tenant_id UUID NOT NULL REFERENCES tenants (id),
    user_id BIGINT NOT NULL CHECK (user_id > 0),
    period INT NOT NULL CHECK (period > 0),
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id, period)
);

-- Whether the linked account was matched to the Stripe payout account (DESIGN
-- 20.4). 'owner_declared' when it could not be checked; a mismatch is refused, so
-- it is never stored. No account or routing numbers are stored.
ALTER TABLE bank_links
    ADD COLUMN payout_account_check TEXT NOT NULL DEFAULT 'owner_declared'
        CHECK (payout_account_check IN ('matched', 'owner_declared'));
