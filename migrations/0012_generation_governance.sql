-- Durable generation governance is metadata-only. Reservation receipts bind
-- one exact generation request to conservative pre-provider estimates and
-- retain aggregate spend after Q13 cleanup removes operational jobs/attempts.
-- No prompt, player-intent, provider-response, or credential body is stored.
ALTER TABLE generation_attempts
    ADD COLUMN latency_milliseconds BIGINT CHECK (
        latency_milliseconds IS NULL OR latency_milliseconds >= 0
    );

CREATE TABLE generation_governance_receipts (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (
        purpose IN ('intent_parsing', 'gm_planning', 'narration', 'illustration')
    ),
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    job_id TEXT NOT NULL UNIQUE CHECK (
        octet_length(job_id) BETWEEN 1 AND 128
        AND job_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    origin_turn_id TEXT,
    turn_scope_key TEXT NOT NULL CHECK (
        octet_length(turn_scope_key) BETWEEN 1 AND 128
        AND turn_scope_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    policy_fingerprint TEXT NOT NULL CHECK (
        policy_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    config_fingerprint TEXT NOT NULL CHECK (
        config_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    governance_fingerprint TEXT NOT NULL CHECK (
        governance_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    state TEXT NOT NULL CHECK (state IN ('reserved', 'settled', 'released')),
    reserved_requests SMALLINT NOT NULL CHECK (reserved_requests BETWEEN 0 AND 5),
    reserved_tokens BIGINT NOT NULL CHECK (reserved_tokens >= 0),
    reserved_latency_milliseconds BIGINT NOT NULL CHECK (
        reserved_latency_milliseconds >= 0
    ),
    reserved_cost_microusd BIGINT NOT NULL CHECK (reserved_cost_microusd >= 0),
    spent_requests SMALLINT NOT NULL DEFAULT 0 CHECK (spent_requests BETWEEN 0 AND 5),
    spent_tokens BIGINT NOT NULL DEFAULT 0 CHECK (spent_tokens >= 0),
    spent_latency_milliseconds BIGINT NOT NULL DEFAULT 0 CHECK (
        spent_latency_milliseconds >= 0
    ),
    spent_cost_microusd BIGINT NOT NULL DEFAULT 0 CHECK (spent_cost_microusd >= 0),
    overage BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    settled_at TIMESTAMPTZ,
    PRIMARY KEY (campaign_session_id, purpose, idempotency_key),
    CHECK (
        (state = 'reserved' AND settled_at IS NULL)
        OR (state IN ('settled', 'released') AND settled_at IS NOT NULL)
    )
);

CREATE INDEX generation_governance_campaign_idx
    ON generation_governance_receipts(campaign_session_id, state, created_at);

CREATE INDEX generation_governance_turn_idx
    ON generation_governance_receipts(campaign_session_id, turn_scope_key, state);

-- Rejections are deliberately bounded enum-only diagnostics. They support
-- operator metrics without retaining request bodies or unbounded labels and
-- expire independently after fourteen days.
CREATE TABLE generation_governance_diagnostics (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    purpose TEXT NOT NULL CHECK (
        purpose IN ('intent_parsing', 'gm_planning', 'narration', 'illustration')
    ),
    failure_code TEXT NOT NULL CHECK (failure_code = 'budget_exceeded'),
    budget_scope TEXT NOT NULL CHECK (budget_scope IN ('turn', 'campaign', 'concurrency')),
    budget_dimension TEXT NOT NULL CHECK (
        budget_dimension IN ('requests', 'tokens', 'latency', 'cost', 'concurrency')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    retention_delete_after TIMESTAMPTZ NOT NULL
        DEFAULT (CURRENT_TIMESTAMP + INTERVAL '14 days')
);

CREATE INDEX generation_governance_diagnostics_retention_idx
    ON generation_governance_diagnostics(retention_delete_after, id);

CREATE INDEX generation_governance_diagnostics_metrics_idx
    ON generation_governance_diagnostics(
        purpose, budget_scope, budget_dimension, created_at
    );
