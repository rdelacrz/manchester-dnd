ALTER TABLE hero_command_receipts
    DROP CONSTRAINT hero_command_receipts_command_kind_check;

ALTER TABLE hero_command_receipts
    ADD CONSTRAINT hero_command_receipts_command_kind_check CHECK (
        command_kind IN (
            'hero_creation_transition',
            'hero_reward',
            'hero_level_up',
            'encounter_reward_claim'
        )
    );

-- One immutable claim binds a terminal encounter victory to the exact created
-- hero that supplied its combat snapshot. The hero update, audit, receipt, and
-- this uniqueness record are committed by one database transaction.
CREATE TABLE encounter_reward_claims (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    encounter_id TEXT NOT NULL CHECK (
        octet_length(encounter_id) BETWEEN 1 AND 128
        AND encounter_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    character_id TEXT NOT NULL REFERENCES hero_characters(id) ON DELETE CASCADE,
    encounter_revision BIGINT NOT NULL CHECK (encounter_revision > 0),
    victory_event_sequence BIGINT NOT NULL CHECK (victory_event_sequence > 0),
    reward_tier TEXT NOT NULL CHECK (reward_tier = 'minor'),
    experience_awarded BIGINT NOT NULL CHECK (experience_awarded > 0),
    hero_audit_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (campaign_session_id, encounter_id),
    UNIQUE (hero_audit_id, campaign_session_id),
    FOREIGN KEY (hero_audit_id, campaign_session_id)
        REFERENCES hero_audits(id, campaign_session_id) ON DELETE CASCADE
);

CREATE INDEX encounter_reward_claims_character_idx
    ON encounter_reward_claims(character_id, created_at DESC);
