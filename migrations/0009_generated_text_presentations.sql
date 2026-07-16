-- Canonical output provenance is metadata-only. Generation inputs, prompts,
-- player intent, and raw provider response bodies remain intentionally absent.
ALTER TABLE generation_jobs
    ADD COLUMN output_digest TEXT CHECK (
        output_digest IS NULL OR output_digest ~ '^sha256:[0-9a-f]{64}$'
    );

ALTER TABLE generation_attempts
    ADD COLUMN output_digest TEXT CHECK (
        output_digest IS NULL OR output_digest ~ '^sha256:[0-9a-f]{64}$'
    );

-- The body is the bounded, safety-validated player-visible presentation, not a
-- raw provider response. Job/attempt identifiers and digests are snapshots so
-- Q13 cleanup may remove failed operational metadata without deleting a
-- selected campaign-lifetime narration.
CREATE TABLE generated_text_presentations (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    origin_turn_id TEXT NOT NULL,
    generation_job_id TEXT NOT NULL CHECK (
        octet_length(generation_job_id) BETWEEN 1 AND 128
        AND generation_job_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    generation_attempt_id TEXT NOT NULL CHECK (
        octet_length(generation_attempt_id) BETWEEN 1 AND 128
        AND generation_attempt_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    client_idempotency_key TEXT NOT NULL CHECK (
        octet_length(client_idempotency_key) BETWEEN 1 AND 128
        AND client_idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    version SMALLINT NOT NULL CHECK (version BETWEEN 1 AND 3),
    source TEXT NOT NULL CHECK (
        source IN ('provider', 'authored_fallback', 'engine_authored')
    ),
    body TEXT NOT NULL CHECK (
        body = btrim(body)
        AND char_length(body) BETWEEN 1 AND 12000
        AND octet_length(body) <= 49152
    ),
    config_digest TEXT NOT NULL CHECK (config_digest ~ '^sha256:[0-9a-f]{64}$'),
    prompt_digest TEXT NOT NULL CHECK (prompt_digest ~ '^sha256:[0-9a-f]{64}$'),
    policy_digest TEXT NOT NULL CHECK (policy_digest ~ '^sha256:[0-9a-f]{64}$'),
    output_digest TEXT NOT NULL CHECK (output_digest ~ '^sha256:[0-9a-f]{64}$'),
    selected BOOLEAN NOT NULL,
    retention_delete_after TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (generation_job_id),
    UNIQUE (generation_attempt_id),
    UNIQUE (campaign_session_id, origin_turn_id, client_idempotency_key),
    UNIQUE (campaign_session_id, origin_turn_id, version),
    FOREIGN KEY (origin_turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE,
    CHECK (
        (selected AND retention_delete_after IS NULL)
        OR (NOT selected AND retention_delete_after IS NOT NULL)
    )
);

CREATE UNIQUE INDEX generated_text_presentations_selected_idx
    ON generated_text_presentations(campaign_session_id, origin_turn_id)
    WHERE selected;

CREATE INDEX generated_text_presentations_turn_idx
    ON generated_text_presentations(campaign_session_id, origin_turn_id, version);

CREATE INDEX generated_text_presentations_retention_idx
    ON generated_text_presentations(retention_delete_after)
    WHERE retention_delete_after IS NOT NULL;
