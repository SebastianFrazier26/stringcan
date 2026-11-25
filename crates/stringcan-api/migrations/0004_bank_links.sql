-- Plaid bank link per tenant (docs/DESIGN.md section 20.4). The access token is a
-- credential, so it is stored only as XChaCha20-Poly1305 ciphertext; key_id names
-- the key that sealed it so keys can rotate without a migration.
CREATE TABLE bank_links (
    tenant_id UUID PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL CHECK (item_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    -- The one account that receives the tenant's Stripe payouts; only it counts.
    account_id TEXT NOT NULL CHECK (account_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    token_key_id TEXT NOT NULL CHECK (token_key_id ~ '^[A-Za-z0-9]{1,16}$'),
    token_nonce BYTEA NOT NULL CHECK (length(token_nonce) = 24),
    token_ciphertext BYTEA NOT NULL,
    linked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_checked_at TIMESTAMPTZ,
    last_result TEXT CHECK (last_result IN ('verified', 'short', 'unverifiable'))
);
