-- Immediate, owner-visible controls for the private-inspiration boundary.
-- Pausing preserves reviewed setup and consent while preventing any draw.
-- Disabling is stronger: application code also revokes active grants and
-- requests cancellation of every pending derived work in the transaction.

ALTER TABLE campaign_inspiration_settings
    ADD COLUMN generation_paused BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE private_inspiration_selection_audits
    DROP CONSTRAINT private_inspiration_selection_audits_no_selection_reason_check,
    ADD CONSTRAINT private_inspiration_selection_audits_no_selection_reason_check CHECK (
        no_selection_reason IS NULL
        OR no_selection_reason IN (
            'deployment_disabled', 'campaign_disabled', 'campaign_paused',
            'safety_incomplete', 'no_eligible_sources'
        )
    );

ALTER TABLE private_inspiration_command_receipts
    DROP CONSTRAINT private_inspiration_command_receipts_operation_code_check,
    ADD CONSTRAINT private_inspiration_command_receipts_operation_code_check CHECK (
        operation_code IN (
            'settings_change', 'settings_pause', 'settings_disable',
            'source_register', 'source_review', 'participant_verify',
            'participant_revoke', 'consent_grant', 'consent_revoke',
            'veto_apply', 'derived_work_register'
        )
    );
