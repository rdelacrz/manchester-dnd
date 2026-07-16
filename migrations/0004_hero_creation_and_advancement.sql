CREATE TABLE hero_creation_drafts (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    owner_key TEXT NOT NULL CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    -- Durable document revisions are one-based. The validated domain payload
    -- uses a zero-based command revision, so row revision = payload revision + 1.
    revision BIGINT NOT NULL CHECK (revision > 0),
    expires_at_epoch_seconds BIGINT NOT NULL CHECK (expires_at_epoch_seconds > 0),
    retention_delete_after_epoch_seconds BIGINT NOT NULL CHECK (
        retention_delete_after_epoch_seconds >= expires_at_epoch_seconds
    ),
    payload_json JSONB NOT NULL CHECK (
        jsonb_typeof(payload_json) = 'object'
        AND octet_length(payload_json::text) BETWEEN 2 AND 65536
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (octet_length(campaign_session_id) BETWEEN 1 AND 128)
);

CREATE INDEX hero_creation_drafts_owner_idx
    ON hero_creation_drafts(campaign_session_id, owner_key, updated_at DESC);

CREATE INDEX hero_creation_drafts_retention_idx
    ON hero_creation_drafts(retention_delete_after_epoch_seconds);

CREATE TABLE hero_characters (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    owner_key TEXT NOT NULL CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    -- Durable document revisions are one-based. The validated domain payload
    -- uses a zero-based command revision, so row revision = payload revision + 1.
    revision BIGINT NOT NULL CHECK (revision > 0),
    payload_json JSONB NOT NULL CHECK (
        jsonb_typeof(payload_json) = 'object'
        AND octet_length(payload_json::text) BETWEEN 2 AND 65536
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (octet_length(campaign_session_id) BETWEEN 1 AND 128)
);

ALTER TABLE hero_characters
    ADD CONSTRAINT hero_characters_one_owner_hero
    UNIQUE (campaign_session_id, owner_key);

CREATE INDEX hero_characters_campaign_owner_idx
    ON hero_characters(campaign_session_id, owner_key, updated_at DESC);

CREATE TABLE hero_audits (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('draft', 'character')),
    subject_id TEXT NOT NULL CHECK (
        octet_length(subject_id) BETWEEN 1 AND 128
        AND subject_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    audit_kind TEXT NOT NULL CHECK (
        audit_kind IN ('creation_transition', 'reward_awarded', 'level_up')
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    subject_revision BIGINT NOT NULL CHECK (subject_revision > 0),
    occurred_at_epoch_seconds BIGINT NOT NULL CHECK (occurred_at_epoch_seconds > 0),
    payload_json JSONB NOT NULL CHECK (
        jsonb_typeof(payload_json) = 'object'
        AND octet_length(payload_json::text) BETWEEN 2 AND 131072
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (id, campaign_session_id),
    UNIQUE (campaign_session_id, subject_kind, subject_id, subject_revision)
);

CREATE INDEX hero_audits_subject_idx
    ON hero_audits(campaign_session_id, subject_kind, subject_id, subject_revision, id);

CREATE TABLE hero_command_receipts (
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('draft', 'character')),
    scope_id TEXT NOT NULL CHECK (
        octet_length(scope_id) BETWEEN 1 AND 128
        AND scope_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    command_kind TEXT NOT NULL CHECK (
        command_kind IN ('hero_creation_transition', 'hero_reward', 'hero_level_up')
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    expected_revision BIGINT NOT NULL CHECK (expected_revision >= 0),
    result_revision BIGINT NOT NULL CHECK (result_revision = expected_revision + 1),
    audit_id TEXT NOT NULL,
    response_json TEXT NOT NULL CHECK (
        octet_length(response_json) BETWEEN 1 AND 131072
        AND jsonb_typeof(response_json::jsonb) IS NOT NULL
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (scope_kind, scope_id, idempotency_key),
    FOREIGN KEY (audit_id, campaign_session_id)
        REFERENCES hero_audits(id, campaign_session_id) ON DELETE CASCADE
);

CREATE INDEX hero_command_receipts_audit_idx
    ON hero_command_receipts(audit_id, campaign_session_id);
