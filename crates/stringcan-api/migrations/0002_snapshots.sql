-- Replay accelerator only. The command log stays the source of truth: a snapshot
-- is ignored (and the log replayed in full) if its rules_version differs, it fails
-- to deserialize, or it fails the engine audit.
CREATE TABLE snapshots (
    tenant_id UUID NOT NULL REFERENCES tenants (id),
    next_seq BIGINT NOT NULL CHECK (next_seq > 0),
    rules_version INT NOT NULL,
    state JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, next_seq)
);
