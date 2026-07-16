-- Owner-authorized private recaps are deterministic presentation artifacts
-- derived only from immutable committed turn audits. They are campaign-owned,
-- have no public/share token, and disappear with the campaign.
CREATE TABLE campaign_private_recaps (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    owner_key TEXT NOT NULL CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    campaign_revision BIGINT NOT NULL CHECK (campaign_revision > 0),
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    first_turn_number BIGINT CHECK (first_turn_number > 0),
    last_turn_number BIGINT CHECK (last_turn_number > 0),
    source_audit_count BIGINT NOT NULL CHECK (source_audit_count >= 0),
    source_audit_digest TEXT NOT NULL CHECK (
        source_audit_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    template_id TEXT NOT NULL CHECK (template_id = 'private-recap-v1'),
    body TEXT NOT NULL CHECK (octet_length(body) BETWEEN 1 AND 131072),
    body_digest TEXT NOT NULL CHECK (body_digest ~ '^sha256:[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (source_audit_count = 0
            AND first_turn_number IS NULL
            AND last_turn_number IS NULL)
        OR
        (source_audit_count > 0
            AND first_turn_number IS NOT NULL
            AND last_turn_number IS NOT NULL
            AND last_turn_number >= first_turn_number)
    ),
    UNIQUE (campaign_session_id, idempotency_key),
    UNIQUE (campaign_session_id, campaign_revision),
    UNIQUE (id, campaign_session_id)
);

CREATE INDEX campaign_private_recaps_owner_idx
    ON campaign_private_recaps(owner_key, campaign_session_id, campaign_revision DESC);
