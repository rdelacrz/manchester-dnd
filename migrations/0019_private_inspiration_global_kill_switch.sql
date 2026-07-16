-- Restart-independent incident switch. The deployment environment flag still
-- controls whether source files are loaded; this durable switch immediately
-- blocks new reservations and lets the operator quarantine active artifacts.

CREATE TABLE private_inspiration_global_control (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    revision BIGINT NOT NULL CHECK (revision > 0),
    generation_disabled BOOLEAN NOT NULL DEFAULT FALSE,
    operator_id TEXT CHECK (
        operator_id IS NULL OR operator_id ~ '^operator:[0-9a-f]{32}$'
    ),
    evidence_digest TEXT CHECK (
        evidence_digest IS NULL OR evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    updated_at_epoch BIGINT NOT NULL CHECK (updated_at_epoch >= 0),
    CHECK (
        (generation_disabled AND operator_id IS NOT NULL AND evidence_digest IS NOT NULL)
        OR (NOT generation_disabled)
    )
);

INSERT INTO private_inspiration_global_control
    (singleton, schema_version, revision, generation_disabled, updated_at_epoch)
VALUES (TRUE, 1, 1, FALSE, 0);

CREATE TABLE private_inspiration_global_command_receipts (
    idempotency_key TEXT PRIMARY KEY CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    response_json TEXT NOT NULL CHECK (
        octet_length(response_json) BETWEEN 2 AND 65536
        AND jsonb_typeof(response_json::jsonb) = 'object'
    ),
    created_at_epoch BIGINT NOT NULL CHECK (created_at_epoch >= 0)
);

ALTER TABLE private_inspiration_selection_audits
    DROP CONSTRAINT private_inspiration_selection_audits_no_selection_reason_check,
    ADD CONSTRAINT private_inspiration_selection_audits_no_selection_reason_check CHECK (
        no_selection_reason IS NULL
        OR no_selection_reason IN (
            'deployment_disabled', 'global_kill_switch', 'campaign_disabled',
            'campaign_paused', 'safety_incomplete', 'no_eligible_sources'
        )
    );

ALTER TABLE private_inspiration_privacy_audits
    DROP CONSTRAINT private_inspiration_privacy_audits_operation_code_check,
    ADD CONSTRAINT private_inspiration_privacy_audits_operation_code_check CHECK (
        operation_code IN (
            'settings_changed', 'source_registered', 'source_reviewed',
            'participant_verified', 'participant_revoked', 'consent_granted',
            'consent_revoked', 'veto_applied', 'selection_reserved',
            'derived_work_registered', 'derived_work_completed',
            'derived_work_cancel_requested', 'derived_work_redacted',
            'derived_work_deleted', 'presentation_veiled',
            'owner_veto_applied', 'privacy_reported', 'global_kill_switch'
        )
    );

