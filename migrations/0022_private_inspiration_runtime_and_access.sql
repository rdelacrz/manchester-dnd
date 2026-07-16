-- The ordinary game/image process consumes only this minimized projection.
-- Raw Markdown, source paths/titles, review prose, and decrypted backup bytes
-- have no columns. The offline inspiration-admin process derives these rows
-- after the strict source review.
CREATE TABLE private_inspiration_runtime_prompts (
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    source_digest TEXT NOT NULL,
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    selection_weight_nanounits BIGINT NOT NULL CHECK (
        selection_weight_nanounits BETWEEN 1 AND 1000000000000000
    ),
    minimum_level SMALLINT NOT NULL CHECK (minimum_level BETWEEN 1 AND 20),
    maximum_level SMALLINT CHECK (
        maximum_level IS NULL
        OR maximum_level BETWEEN minimum_level AND 20
    ),
    cooldown_turns BIGINT NOT NULL CHECK (
        cooldown_turns BETWEEN 0 AND 1000000
    ),
    enabled BOOLEAN NOT NULL,
    projection_digest TEXT NOT NULL CHECK (
        projection_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    PRIMARY KEY (source_id, source_version),
    UNIQUE (source_id, source_version, source_digest),
    FOREIGN KEY (source_id, source_version, source_digest)
        REFERENCES private_inspiration_sources(
            source_id, source_version, source_digest
        ) ON DELETE CASCADE,
    CHECK (NOT enabled OR cooldown_turns > 0)
);

CREATE TABLE private_inspiration_runtime_facts (
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    fact_index SMALLINT NOT NULL CHECK (fact_index BETWEEN 1 AND 4),
    neutral_fact TEXT NOT NULL CHECK (
        neutral_fact = btrim(neutral_fact)
        AND char_length(neutral_fact) BETWEEN 1 AND 240
        AND neutral_fact !~ '[[:cntrl:]]'
    ),
    PRIMARY KEY (source_id, source_version, fact_index),
    FOREIGN KEY (source_id, source_version)
        REFERENCES private_inspiration_runtime_prompts(source_id, source_version)
        ON DELETE CASCADE
);

-- Restricted diagnostic access is always an explicit operator action. This
-- table contains only opaque subjects, closed purpose/access codes, and an
-- evidence digest; it cannot retain a diagnostic body or filesystem path.
CREATE TABLE private_inspiration_restricted_access_audits (
    audit_id TEXT PRIMARY KEY CHECK (
        octet_length(audit_id) BETWEEN 1 AND 128
        AND audit_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    idempotency_key TEXT NOT NULL UNIQUE CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    campaign_session_id TEXT
        REFERENCES campaign_sessions(id) ON DELETE SET NULL,
    operator_id TEXT NOT NULL CHECK (
        operator_id ~ '^operator:[0-9a-f]{32}$'
    ),
    access_kind TEXT NOT NULL CHECK (
        access_kind IN (
            'source_plaintext', 'source_backup', 'image_quarantine',
            'generation_diagnostic'
        )
    ),
    purpose_code TEXT NOT NULL CHECK (
        purpose_code IN (
            'source_review', 'data_rights_request', 'incident_response',
            'restore_drill', 'security_validation'
        )
    ),
    subject_id TEXT NOT NULL CHECK (
        octet_length(subject_id) BETWEEN 1 AND 128
        AND subject_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    evidence_digest TEXT NOT NULL CHECK (
        evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    result_code TEXT NOT NULL CHECK (result_code IN ('allowed', 'denied')),
    occurred_at_epoch BIGINT NOT NULL CHECK (occurred_at_epoch >= 0)
);

CREATE INDEX private_inspiration_restricted_access_time_idx
    ON private_inspiration_restricted_access_audits(
        occurred_at_epoch DESC, audit_id
    );

ALTER TABLE private_inspiration_privacy_audits
    DROP CONSTRAINT private_inspiration_privacy_audits_operation_code_check,
    ADD CONSTRAINT private_inspiration_privacy_audits_operation_code_check CHECK (
        operation_code IN (
            'settings_changed', 'source_registered', 'source_reviewed',
            'source_quarantined', 'participant_verified',
            'participant_revoked', 'participant_deletion_requested',
            'deletion_tombstone_expired',
            'consent_granted', 'consent_revoked', 'veto_applied',
            'selection_reserved', 'derived_work_registered',
            'derived_work_completed', 'derived_work_cancel_requested',
            'derived_work_redacted', 'derived_work_deleted',
            'presentation_veiled', 'owner_veto_applied', 'privacy_reported',
            'global_kill_switch', 'restricted_diagnostic_access'
        )
    ),
    DROP CONSTRAINT private_inspiration_privacy_audits_subject_kind_check,
    ADD CONSTRAINT private_inspiration_privacy_audits_subject_kind_check CHECK (
        subject_kind IN (
            'campaign', 'source_version', 'participant', 'consent_grant',
            'veto', 'selection', 'derived_work', 'restricted_diagnostic'
        )
    );
