-- Participant deletion is a privacy transition, not a rewrite of immutable
-- game mechanics. Raw protected files must already have been removed. The
-- database keeps only opaque/digest audit material and a short-lived tombstone
-- so the deletion can be carried through encrypted backups under Q13.

CREATE TABLE private_inspiration_deletion_tombstones (
    participant_id TEXT PRIMARY KEY CHECK (
        participant_id ~ '^participant:[0-9a-f]{32}$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    requested_by_operator_id TEXT NOT NULL CHECK (
        requested_by_operator_id ~ '^operator:[0-9a-f]{32}$'
    ),
    deletion_evidence_digest TEXT NOT NULL CHECK (
        deletion_evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    requested_at_epoch BIGINT NOT NULL CHECK (requested_at_epoch >= 0),
    delete_after_epoch BIGINT NOT NULL CHECK (
        delete_after_epoch = requested_at_epoch + 3024000
    )
);

CREATE INDEX private_inspiration_deletion_tombstone_expiry_idx
    ON private_inspiration_deletion_tombstones(delete_after_epoch, participant_id);

ALTER TABLE private_inspiration_command_receipts
    DROP CONSTRAINT private_inspiration_command_receipts_operation_code_check,
    ADD CONSTRAINT private_inspiration_command_receipts_operation_code_check CHECK (
        operation_code IN (
            'settings_change', 'settings_pause', 'settings_disable',
            'source_register', 'source_review', 'participant_verify',
            'participant_revoke', 'participant_delete', 'consent_grant',
            'consent_revoke', 'veto_apply', 'derived_work_register',
            'presentation_control'
        )
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
            'global_kill_switch'
        )
    );
