-- Local-single-owner lifecycle metadata. The default backfills the one
-- pre-existing local campaign and keeps the current fixed-local creator path
-- working. Hosted mode remains fail-closed in application configuration.
ALTER TABLE campaign_sessions
    ADD COLUMN owner_key TEXT NOT NULL DEFAULT 'local-owner' CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    ADD COLUMN lifecycle_revision BIGINT NOT NULL DEFAULT 1 CHECK (
        lifecycle_revision > 0
    ),
    ADD COLUMN lifecycle_state TEXT NOT NULL DEFAULT 'active' CHECK (
        lifecycle_state IN ('active', 'archived')
    ),
    ADD COLUMN archived_at TIMESTAMPTZ,
    ADD COLUMN safety_policy_id TEXT NOT NULL DEFAULT 'safety:private-mvp:v1' CHECK (
        octet_length(safety_policy_id) BETWEEN 1 AND 128
        AND safety_policy_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    ADD COLUMN progression_policy_id TEXT NOT NULL DEFAULT 'progression:srd-5.1-mvp:v1' CHECK (
        octet_length(progression_policy_id) BETWEEN 1 AND 128
        AND progression_policy_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    ADD COLUMN retention_class TEXT NOT NULL DEFAULT 'campaign_lifetime' CHECK (
        retention_class IN ('campaign_lifetime', 'archived_owner_managed')
    ),
    ADD COLUMN retention_delete_after TIMESTAMPTZ,
    ADD CONSTRAINT campaign_sessions_lifecycle_shape CHECK (
        (lifecycle_state = 'active'
            AND archived_at IS NULL
            AND retention_class = 'campaign_lifetime'
            AND retention_delete_after IS NULL)
        OR
        (lifecycle_state = 'archived'
            AND archived_at IS NOT NULL
            AND retention_class = 'archived_owner_managed'
            AND retention_delete_after IS NULL)
    );

CREATE INDEX campaign_sessions_owner_lifecycle_idx
    ON campaign_sessions(owner_key, lifecycle_state, updated_at DESC, id);

-- Every row under a campaign is campaign-owned. Baseline characters used to
-- survive as orphans; explicit owner deletion must remove them with the rest.
ALTER TABLE characters
    DROP CONSTRAINT characters_campaign_session_id_fkey;

ALTER TABLE characters
    ADD CONSTRAINT characters_campaign_session_id_fkey
    FOREIGN KEY (campaign_session_id)
    REFERENCES campaign_sessions(id) ON DELETE CASCADE;

CREATE TABLE campaign_play_sessions (
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
    state TEXT NOT NULL CHECK (state IN ('open', 'closed')),
    started_campaign_revision BIGINT NOT NULL CHECK (started_campaign_revision > 0),
    ended_campaign_revision BIGINT CHECK (ended_campaign_revision > 0),
    opened_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    closed_at TIMESTAMPTZ,
    close_reason TEXT CHECK (
        close_reason IS NULL OR close_reason IN ('owner_ended', 'archive', 'restore_import')
    ),
    CHECK (
        (state = 'open'
            AND ended_campaign_revision IS NULL
            AND closed_at IS NULL
            AND close_reason IS NULL)
        OR
        (state = 'closed'
            AND ended_campaign_revision IS NOT NULL
            AND closed_at IS NOT NULL
            AND close_reason IS NOT NULL
            AND ended_campaign_revision >= started_campaign_revision)
    ),
    UNIQUE (id, campaign_session_id)
);

CREATE UNIQUE INDEX campaign_play_sessions_one_open_idx
    ON campaign_play_sessions(campaign_session_id)
    WHERE state = 'open';

CREATE INDEX campaign_play_sessions_history_idx
    ON campaign_play_sessions(campaign_session_id, opened_at DESC, id);

CREATE TABLE campaign_lifecycle_audits (
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
    lifecycle_revision BIGINT NOT NULL CHECK (lifecycle_revision > 1),
    event_kind TEXT NOT NULL CHECK (
        event_kind IN (
            'play_started', 'play_ended', 'archived', 'restored',
            'restore_imported'
        )
    ),
    payload_json JSONB NOT NULL CHECK (
        jsonb_typeof(payload_json) = 'object'
        AND octet_length(payload_json::text) BETWEEN 2 AND 16384
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (campaign_session_id, lifecycle_revision),
    UNIQUE (id, campaign_session_id)
);

CREATE INDEX campaign_lifecycle_audits_history_idx
    ON campaign_lifecycle_audits(campaign_session_id, lifecycle_revision, id);

-- Receipts deliberately have no campaign foreign key: an exact delete replay
-- must remain answerable after all campaign-owned rows have been removed.
CREATE TABLE campaign_lifecycle_receipts (
    owner_key TEXT NOT NULL CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL CHECK (
        octet_length(campaign_session_id) BETWEEN 1 AND 128
        AND campaign_session_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    command_kind TEXT NOT NULL CHECK (
        command_kind IN (
            'play_start', 'play_end', 'archive', 'restore_archive',
            'delete', 'restore_export'
        )
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    expected_lifecycle_revision BIGINT NOT NULL CHECK (
        expected_lifecycle_revision >= 0
    ),
    result_lifecycle_revision BIGINT NOT NULL CHECK (
        result_lifecycle_revision = expected_lifecycle_revision + 1
    ),
    response_json TEXT NOT NULL CHECK (
        octet_length(response_json) BETWEEN 1 AND 65536
        AND jsonb_typeof(response_json::jsonb) IS NOT NULL
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    retention_delete_after TIMESTAMPTZ NOT NULL
        DEFAULT (CURRENT_TIMESTAMP + INTERVAL '30 days'),
    PRIMARY KEY (owner_key, campaign_session_id, idempotency_key)
);

CREATE INDEX campaign_lifecycle_receipts_retention_idx
    ON campaign_lifecycle_receipts(retention_delete_after);

-- A short-lived server-prepared delete snapshot prevents a browser from
-- forging the tombstone digest. Delete rechecks both revisions under lock.
CREATE TABLE campaign_deletion_preparations (
    owner_key TEXT NOT NULL CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    deletion_id TEXT NOT NULL CHECK (
        octet_length(deletion_id) BETWEEN 1 AND 128
        AND deletion_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_revision BIGINT NOT NULL CHECK (campaign_revision > 0),
    lifecycle_revision BIGINT NOT NULL CHECK (lifecycle_revision > 0),
    canonical_export_digest TEXT NOT NULL CHECK (
        canonical_export_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    canonical_export_json TEXT NOT NULL CHECK (
        octet_length(canonical_export_json) BETWEEN 2 AND 2097152
        AND jsonb_typeof(canonical_export_json::jsonb) = 'object'
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMPTZ NOT NULL
        DEFAULT (CURRENT_TIMESTAMP + INTERVAL '1 hour'),
    PRIMARY KEY (owner_key, campaign_session_id, deletion_id)
);

CREATE INDEX campaign_deletion_preparations_expiry_idx
    ON campaign_deletion_preparations(expires_at);

-- A deletion tombstone contains only opaque identifiers, a canonical export
-- digest, revisions, and timestamps. It never stores an export body or title.
CREATE TABLE campaign_deletion_tombstones (
    owner_key TEXT NOT NULL CHECK (
        octet_length(owner_key) BETWEEN 1 AND 128
        AND owner_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL CHECK (
        octet_length(campaign_session_id) BETWEEN 1 AND 128
        AND campaign_session_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    deletion_id TEXT NOT NULL UNIQUE CHECK (
        octet_length(deletion_id) BETWEEN 1 AND 128
        AND deletion_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    deleted_lifecycle_revision BIGINT NOT NULL CHECK (
        deleted_lifecycle_revision > 1
    ),
    canonical_export_digest TEXT NOT NULL CHECK (
        canonical_export_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    deleted_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    retention_delete_after TIMESTAMPTZ NOT NULL
        DEFAULT (CURRENT_TIMESTAMP + INTERVAL '35 days'),
    PRIMARY KEY (owner_key, campaign_session_id, deletion_id)
);

CREATE INDEX campaign_deletion_tombstones_retention_idx
    ON campaign_deletion_tombstones(retention_delete_after);
