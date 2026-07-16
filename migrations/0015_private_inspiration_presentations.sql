-- Bind a consented private-inspiration reservation to the one player-visible
-- presentation it produced. The link and completion metadata are opaque and
-- body-free; raw source text remains outside PostgreSQL and client responses.

ALTER TABLE generated_text_presentations
    ADD COLUMN private_inspiration_work_id TEXT UNIQUE
        REFERENCES private_inspiration_derived_work(work_id) ON DELETE SET NULL,
    ADD COLUMN privacy_state TEXT NOT NULL DEFAULT 'visible' CHECK (
        privacy_state IN ('visible', 'redacted')
    ),
    ADD CONSTRAINT generated_text_presentations_privacy_redaction_check CHECK (
        privacy_state = 'visible'
        OR body = 'Private inspiration removed at a participant request. The committed game mechanics are unchanged.'
    );

ALTER TABLE private_inspiration_derived_work
    ADD COLUMN completed_artifact_id TEXT CHECK (
        completed_artifact_id IS NULL
        OR (
            octet_length(completed_artifact_id) BETWEEN 1 AND 128
            AND completed_artifact_id ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    ADD COLUMN completed_output_digest TEXT CHECK (
        completed_output_digest IS NULL
        OR completed_output_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    ADD COLUMN completed_at_epoch BIGINT CHECK (
        completed_at_epoch IS NULL OR completed_at_epoch >= created_at_epoch
    ),
    ADD CONSTRAINT private_inspiration_derived_work_completion_check CHECK (
        (
            state IN ('pending', 'cancellation_requested')
            AND completed_artifact_id IS NULL
            AND completed_output_digest IS NULL
            AND completed_at_epoch IS NULL
        )
        OR (
            state IN ('completed', 'redacted')
            AND completed_artifact_id IS NOT NULL
            AND completed_output_digest IS NOT NULL
            AND completed_at_epoch IS NOT NULL
        )
        OR (
            state = 'deleted'
            AND completed_artifact_id IS NULL
            AND completed_output_digest IS NOT NULL
            AND completed_at_epoch IS NOT NULL
        )
    );

CREATE INDEX private_inspiration_completed_work_idx
    ON private_inspiration_derived_work(
        campaign_session_id, source_id, source_version, state
    ) WHERE state IN ('completed', 'redacted');

ALTER TABLE private_inspiration_privacy_audits
    DROP CONSTRAINT private_inspiration_privacy_audits_operation_code_check,
    ADD CONSTRAINT private_inspiration_privacy_audits_operation_code_check CHECK (
        operation_code IN (
            'settings_changed', 'source_registered', 'source_reviewed',
            'participant_verified', 'participant_revoked', 'consent_granted',
            'consent_revoked', 'veto_applied', 'selection_reserved',
            'derived_work_registered', 'derived_work_completed',
            'derived_work_cancel_requested', 'derived_work_redacted',
            'derived_work_deleted'
        )
    );

