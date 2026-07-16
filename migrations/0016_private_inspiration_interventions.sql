-- Local-owner safety interventions do not impersonate a represented
-- participant. They carry no justification text and attach only opaque IDs.

ALTER TABLE private_inspiration_vetoes
    ALTER COLUMN participant_id DROP NOT NULL,
    ADD COLUMN actor_kind TEXT NOT NULL DEFAULT 'participant' CHECK (
        actor_kind IN ('participant', 'campaign_owner')
    ),
    ADD CONSTRAINT private_inspiration_veto_actor_check CHECK (
        (actor_kind = 'participant' AND participant_id IS NOT NULL)
        OR (actor_kind = 'campaign_owner' AND participant_id IS NULL)
    );

ALTER TABLE private_inspiration_command_receipts
    DROP CONSTRAINT private_inspiration_command_receipts_operation_code_check,
    ADD CONSTRAINT private_inspiration_command_receipts_operation_code_check CHECK (
        operation_code IN (
            'settings_change', 'settings_pause', 'settings_disable',
            'source_register', 'source_review', 'participant_verify',
            'participant_revoke', 'consent_grant', 'consent_revoke',
            'veto_apply', 'derived_work_register', 'presentation_control'
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
            'owner_veto_applied', 'privacy_reported'
        )
    );

