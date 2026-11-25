CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    -- SHA-256 of the owner API key; the key itself is shown once and never stored.
    api_key_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(api_key_hash) = 32),
    currency TEXT NOT NULL DEFAULT 'usd' CHECK (currency ~ '^[a-z]{3}$'),
    engine_config JSONB NOT NULL,
    first_upkeep_cents BIGINT NOT NULL CHECK (first_upkeep_cents >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE subscribers (
    tenant_id UUID NOT NULL REFERENCES tenants (id),
    user_id BIGINT NOT NULL CHECK (user_id > 0),
    external_ref TEXT NOT NULL CHECK (char_length(external_ref) BETWEEN 1 AND 128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id),
    UNIQUE (tenant_id, external_ref)
);

-- Append-only: the engine state of a tenant is the replay of these rows in seq order.
CREATE TABLE commands (
    tenant_id UUID NOT NULL REFERENCES tenants (id),
    seq BIGINT NOT NULL CHECK (seq >= 0),
    rules_version INT NOT NULL,
    command JSONB NOT NULL,
    processor_ref TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, seq)
);
