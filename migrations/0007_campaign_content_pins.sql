-- Rows that existed before immutable campaign provenance was introduced are
-- eligible for one explicit lazy migration. New rows default to false and may
-- remain unsealed only while they are pre-game character-creator scaffolds.
ALTER TABLE campaign_sessions
    ADD COLUMN content_pin_legacy_eligible BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE campaign_sessions
    ALTER COLUMN content_pin_legacy_eligible SET DEFAULT FALSE;

CREATE TABLE campaign_content_pins (
    campaign_session_id TEXT PRIMARY KEY
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    seal_reason TEXT NOT NULL CHECK (
        seal_reason IN (
            'selected_theme',
            'legacy_selected_theme',
            'legacy_digest_alias',
            'legacy_default_rainbound'
        )
    ),
    payload_json JSONB NOT NULL CHECK (
        jsonb_typeof(payload_json) = 'object'
        AND octet_length(payload_json::text) BETWEEN 2 AND 32768
    ),
    legacy_source_json JSONB CHECK (
        legacy_source_json IS NULL
        OR (
            jsonb_typeof(legacy_source_json) = 'object'
            AND octet_length(legacy_source_json::text) BETWEEN 2 AND 8192
        )
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (seal_reason = 'legacy_digest_alias')
        = (legacy_source_json IS NOT NULL)
    )
);

COMMENT ON TABLE campaign_content_pins IS
    'Insert-only exact campaign rules/content/theme/prompt/schema/catalog pins';

CREATE INDEX campaign_content_pins_created_idx
    ON campaign_content_pins(created_at, campaign_session_id);
