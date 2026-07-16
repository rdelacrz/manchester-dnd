-- Durable generation is deliberately metadata-only. Prompt/input/provider
-- bodies do not have columns here; routine provenance is represented by
-- canonical SHA-256 digests and bounded, non-secret operational facts.
CREATE TABLE generation_jobs (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    origin_turn_id TEXT,
    origin_campaign_revision BIGINT NOT NULL CHECK (origin_campaign_revision > 0),
    purpose TEXT NOT NULL CHECK (
        purpose IN ('intent_parsing', 'gm_planning', 'narration', 'illustration')
    ),
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    state TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')
    ),
    input_digest TEXT NOT NULL CHECK (input_digest ~ '^sha256:[0-9a-f]{64}$'),
    prompt_digest TEXT NOT NULL CHECK (prompt_digest ~ '^sha256:[0-9a-f]{64}$'),
    policy_digest TEXT NOT NULL CHECK (policy_digest ~ '^sha256:[0-9a-f]{64}$'),
    config_digest TEXT NOT NULL CHECK (config_digest ~ '^sha256:[0-9a-f]{64}$'),
    correlation_id TEXT CHECK (
        correlation_id IS NULL
        OR (
            octet_length(correlation_id) BETWEEN 1 AND 128
            AND correlation_id ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    attempt_count SMALLINT NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 5),
    max_attempts SMALLINT NOT NULL CHECK (max_attempts BETWEEN 1 AND 5),
    retry_at TIMESTAMPTZ,
    lease_owner TEXT CHECK (
        lease_owner IS NULL
        OR (
            octet_length(lease_owner) BETWEEN 1 AND 128
            AND lease_owner ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    lease_token TEXT UNIQUE CHECK (
        lease_token IS NULL
        OR (
            octet_length(lease_token) BETWEEN 1 AND 128
            AND lease_token ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    lease_expires_at TIMESTAMPTZ,
    last_failure_class TEXT CHECK (
        last_failure_class IS NULL
        OR last_failure_class IN ('transient', 'permanent')
    ),
    last_failure_code TEXT CHECK (
        last_failure_code IS NULL
        OR last_failure_code IN (
            'timeout', 'provider_unavailable', 'rate_limited', 'provider_rejected',
            'malformed_response', 'unsafe_output', 'contradiction',
            'invalid_artifact', 'budget_exceeded', 'lease_expired', 'cancelled'
        )
    ),
    artifact_id TEXT REFERENCES generated_assets(id) ON DELETE RESTRICT,
    success_retention_class TEXT NOT NULL CHECK (
        success_retention_class IN ('unselected_presentation_30d', 'campaign_lifetime')
    ),
    retention_class TEXT NOT NULL DEFAULT 'pending' CHECK (
        retention_class IN (
            'pending', 'failed_metadata_7d', 'unselected_presentation_30d',
            'campaign_lifetime'
        )
    ),
    retention_delete_after TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMPTZ,
    UNIQUE (campaign_session_id, purpose, idempotency_key),
    UNIQUE (id, campaign_session_id),
    FOREIGN KEY (origin_turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE,
    CHECK (
        purpose NOT IN ('narration', 'illustration')
        OR origin_turn_id IS NOT NULL
    ),
    CHECK (
        (last_failure_class IS NULL AND last_failure_code IS NULL)
        OR (last_failure_class IS NOT NULL AND last_failure_code IS NOT NULL)
    ),
    CHECK (
        (state = 'queued'
            AND retry_at IS NOT NULL
            AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL
            AND completed_at IS NULL AND artifact_id IS NULL
            AND retention_class = 'pending' AND retention_delete_after IS NULL
            AND attempt_count < max_attempts)
        OR
        (state = 'running'
            AND retry_at IS NULL
            AND lease_owner IS NOT NULL AND lease_token IS NOT NULL
            AND lease_expires_at IS NOT NULL
            AND completed_at IS NULL AND artifact_id IS NULL
            AND retention_class = 'pending' AND retention_delete_after IS NULL)
        OR
        (state = 'succeeded'
            AND retry_at IS NULL
            AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL
            AND completed_at IS NOT NULL
            AND last_failure_class IS NULL AND last_failure_code IS NULL
            AND (purpose <> 'illustration' OR artifact_id IS NOT NULL)
            AND (
                (artifact_id IS NULL
                    AND retention_class = 'unselected_presentation_30d'
                    AND retention_delete_after IS NOT NULL)
                OR
                (artifact_id IS NOT NULL
                    AND retention_class = success_retention_class
                    AND (
                        (retention_class = 'unselected_presentation_30d'
                            AND retention_delete_after IS NOT NULL)
                        OR
                        (retention_class = 'campaign_lifetime'
                            AND retention_delete_after IS NULL)
                    ))
            ))
        OR
        (state IN ('failed', 'cancelled')
            AND retry_at IS NULL
            AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL
            AND completed_at IS NOT NULL AND artifact_id IS NULL
            AND last_failure_class IS NOT NULL AND last_failure_code IS NOT NULL
            AND retention_class = 'failed_metadata_7d'
            AND retention_delete_after IS NOT NULL)
    )
);

CREATE INDEX generation_jobs_claim_idx
    ON generation_jobs(retry_at, created_at, id)
    WHERE state = 'queued';

CREATE INDEX generation_jobs_expired_lease_idx
    ON generation_jobs(lease_expires_at, created_at, id)
    WHERE state = 'running';

CREATE INDEX generation_jobs_campaign_idx
    ON generation_jobs(campaign_session_id, created_at DESC, id);

CREATE INDEX generation_jobs_retention_idx
    ON generation_jobs(retention_delete_after)
    WHERE retention_delete_after IS NOT NULL;

CREATE TABLE generation_attempts (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    job_id TEXT NOT NULL REFERENCES generation_jobs(id) ON DELETE CASCADE,
    attempt_number SMALLINT NOT NULL CHECK (attempt_number BETWEEN 1 AND 5),
    state TEXT NOT NULL CHECK (
        state IN ('running', 'succeeded', 'failed', 'cancelled')
    ),
    lease_owner TEXT NOT NULL CHECK (
        octet_length(lease_owner) BETWEEN 1 AND 128
        AND lease_owner ~ '^[A-Za-z0-9_.:-]+$'
    ),
    lease_token TEXT NOT NULL UNIQUE CHECK (
        octet_length(lease_token) BETWEEN 1 AND 128
        AND lease_token ~ '^[A-Za-z0-9_.:-]+$'
    ),
    provider TEXT NOT NULL CHECK (
        octet_length(provider) BETWEEN 1 AND 128
        AND provider ~ '^[A-Za-z0-9_.:-]+$'
    ),
    model TEXT NOT NULL CHECK (
        octet_length(model) BETWEEN 1 AND 256
        AND model = btrim(model)
        AND model !~ '[[:cntrl:]]'
    ),
    prompt_tokens BIGINT CHECK (prompt_tokens IS NULL OR prompt_tokens >= 0),
    completion_tokens BIGINT CHECK (completion_tokens IS NULL OR completion_tokens >= 0),
    total_tokens BIGINT CHECK (total_tokens IS NULL OR total_tokens >= 0),
    cost_microusd BIGINT CHECK (cost_microusd IS NULL OR cost_microusd >= 0),
    failure_class TEXT CHECK (
        failure_class IS NULL OR failure_class IN ('transient', 'permanent')
    ),
    failure_code TEXT CHECK (
        failure_code IS NULL
        OR failure_code IN (
            'timeout', 'provider_unavailable', 'rate_limited', 'provider_rejected',
            'malformed_response', 'unsafe_output', 'contradiction',
            'invalid_artifact', 'budget_exceeded', 'lease_expired', 'cancelled'
        )
    ),
    provider_status SMALLINT CHECK (
        provider_status IS NULL OR provider_status BETWEEN 100 AND 599
    ),
    provider_request_id TEXT CHECK (
        provider_request_id IS NULL
        OR (
            octet_length(provider_request_id) BETWEEN 1 AND 128
            AND provider_request_id ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    artifact_id TEXT REFERENCES generated_assets(id) ON DELETE RESTRICT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (job_id, attempt_number),
    CHECK (
        total_tokens IS NULL
        OR prompt_tokens IS NULL
        OR completion_tokens IS NULL
        OR total_tokens >= prompt_tokens + completion_tokens
    ),
    CHECK (
        (failure_class IS NULL AND failure_code IS NULL)
        OR (failure_class IS NOT NULL AND failure_code IS NOT NULL)
    ),
    CHECK (
        (state = 'running'
            AND finished_at IS NULL AND failure_class IS NULL
            AND failure_code IS NULL AND artifact_id IS NULL)
        OR
        (state = 'succeeded'
            AND finished_at IS NOT NULL AND failure_class IS NULL
            AND failure_code IS NULL)
        OR
        (state = 'failed'
            AND finished_at IS NOT NULL AND failure_class IS NOT NULL
            AND failure_code IS NOT NULL AND artifact_id IS NULL)
        OR
        (state = 'cancelled'
            AND finished_at IS NOT NULL AND failure_class = 'permanent'
            AND failure_code = 'cancelled' AND artifact_id IS NULL)
    )
);

CREATE INDEX generation_attempts_job_idx
    ON generation_attempts(job_id, attempt_number);

CREATE INDEX generation_attempts_provider_idx
    ON generation_attempts(provider, model, created_at DESC);
