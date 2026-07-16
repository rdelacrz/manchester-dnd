-- Campaign-lifetime, body-free aliases preserve response idempotency after a
-- superseded presentation body reaches its Q10/Q13 deletion deadline. Jobs,
-- attempts, and presentation bodies may expire independently, so their opaque
-- identifiers are provenance snapshots rather than foreign keys.
CREATE TABLE generated_text_presentation_receipts (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    origin_turn_id TEXT NOT NULL,
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    client_idempotency_key TEXT NOT NULL CHECK (
        octet_length(client_idempotency_key) BETWEEN 1 AND 128
        AND client_idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    presentation_id TEXT NOT NULL CHECK (
        octet_length(presentation_id) BETWEEN 1 AND 128
        AND presentation_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    generation_job_id TEXT NOT NULL CHECK (
        octet_length(generation_job_id) BETWEEN 1 AND 128
        AND generation_job_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    generation_attempt_id TEXT NOT NULL CHECK (
        octet_length(generation_attempt_id) BETWEEN 1 AND 128
        AND generation_attempt_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    version SMALLINT NOT NULL CHECK (version BETWEEN 1 AND 3),
    source TEXT NOT NULL CHECK (
        source IN ('provider', 'authored_fallback', 'engine_authored')
    ),
    config_digest TEXT NOT NULL CHECK (config_digest ~ '^sha256:[0-9a-f]{64}$'),
    prompt_digest TEXT NOT NULL CHECK (prompt_digest ~ '^sha256:[0-9a-f]{64}$'),
    policy_digest TEXT NOT NULL CHECK (policy_digest ~ '^sha256:[0-9a-f]{64}$'),
    output_digest TEXT NOT NULL CHECK (output_digest ~ '^sha256:[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (campaign_session_id, origin_turn_id, client_idempotency_key),
    UNIQUE (presentation_id),
    UNIQUE (generation_job_id),
    UNIQUE (generation_attempt_id),
    UNIQUE (campaign_session_id, origin_turn_id, version),
    FOREIGN KEY (origin_turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE
);

INSERT INTO generated_text_presentation_receipts
    (campaign_session_id, origin_turn_id, schema_version, client_idempotency_key,
     presentation_id, generation_job_id, generation_attempt_id, version, source,
     config_digest, prompt_digest, policy_digest, output_digest, created_at)
SELECT campaign_session_id, origin_turn_id, 1, client_idempotency_key,
       id, generation_job_id, generation_attempt_id, version, source,
       config_digest, prompt_digest, policy_digest, output_digest, created_at
FROM generated_text_presentations;

CREATE INDEX generated_text_presentation_receipts_turn_idx
    ON generated_text_presentation_receipts(campaign_session_id, origin_turn_id, version);

-- The free-form command body is never retained. A pending receipt records only
-- its digest plus the already validated closed EncounterIntent. If a response
-- is lost around the mechanics commit, the exact command can resume through the
-- canonical command receipt without invoking the model or resolving dice again.
CREATE TABLE typed_intent_command_receipts (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    client_idempotency_key TEXT NOT NULL CHECK (
        octet_length(client_idempotency_key) BETWEEN 1 AND 128
        AND client_idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    player_intent_digest TEXT NOT NULL CHECK (
        player_intent_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    expected_campaign_revision BIGINT NOT NULL CHECK (expected_campaign_revision > 0),
    expected_encounter_revision BIGINT NOT NULL CHECK (expected_encounter_revision > 0),
    resolved_intent_json JSONB NOT NULL CHECK (
        jsonb_typeof(resolved_intent_json) = 'object'
        AND octet_length(resolved_intent_json::text) BETWEEN 2 AND 8192
    ),
    interpretation_label TEXT NOT NULL CHECK (
        interpretation_label = btrim(interpretation_label)
        AND char_length(interpretation_label) BETWEEN 1 AND 512
        AND octet_length(interpretation_label) <= 2048
    ),
    interpretation_evidence_json JSONB NOT NULL CHECK (
        jsonb_typeof(interpretation_evidence_json) = 'object'
        AND octet_length(interpretation_evidence_json::text) BETWEEN 2 AND 32768
    ),
    state TEXT NOT NULL CHECK (state IN ('pending', 'committed')),
    origin_turn_id TEXT,
    event_sequence BIGINT CHECK (event_sequence > 0),
    result_campaign_revision BIGINT CHECK (result_campaign_revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (campaign_session_id, client_idempotency_key),
    UNIQUE (origin_turn_id, campaign_session_id),
    FOREIGN KEY (origin_turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE,
    CHECK (
        (state = 'pending'
            AND origin_turn_id IS NULL
            AND event_sequence IS NULL
            AND result_campaign_revision IS NULL)
        OR
        (state = 'committed'
            AND origin_turn_id IS NOT NULL
            AND event_sequence IS NOT NULL
            AND result_campaign_revision = expected_campaign_revision + 1)
    )
);

CREATE INDEX typed_intent_command_receipts_turn_idx
    ON typed_intent_command_receipts(campaign_session_id, event_sequence)
    WHERE state = 'committed';
