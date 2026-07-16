-- Scene-image publication records contain only bounded public presentation
-- metadata. Provider bytes live beneath IMAGE_ARTIFACT_ROOT, never in SQL or a
-- web-served directory. A job may publish exactly one validated artifact.
CREATE TABLE scene_image_artifacts (
    artifact_id TEXT PRIMARY KEY
        REFERENCES generated_assets(id) ON DELETE CASCADE,
    job_id TEXT NOT NULL UNIQUE
        REFERENCES generation_jobs(id) ON DELETE CASCADE,
    campaign_session_id TEXT NOT NULL,
    source_turn_id TEXT NOT NULL,
    schema_version SMALLINT NOT NULL CHECK (schema_version = 1),
    brief_fingerprint TEXT NOT NULL CHECK (
        brief_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    prompt_policy_fingerprint TEXT NOT NULL CHECK (
        prompt_policy_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    config_fingerprint TEXT NOT NULL CHECK (
        config_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    original_storage_key TEXT NOT NULL CHECK (
        octet_length(original_storage_key) BETWEEN 1 AND 512
        AND original_storage_key ~ '^[A-Za-z0-9._/-]+$'
        AND original_storage_key !~ '(^|/)\.\.(/|$)'
    ),
    web_storage_key TEXT NOT NULL CHECK (
        octet_length(web_storage_key) BETWEEN 1 AND 512
        AND web_storage_key ~ '^[A-Za-z0-9._/-]+$'
        AND web_storage_key !~ '(^|/)\.\.(/|$)'
    ),
    thumbnail_storage_key TEXT NOT NULL CHECK (
        octet_length(thumbnail_storage_key) BETWEEN 1 AND 512
        AND thumbnail_storage_key ~ '^[A-Za-z0-9._/-]+$'
        AND thumbnail_storage_key !~ '(^|/)\.\.(/|$)'
    ),
    original_digest TEXT NOT NULL CHECK (original_digest ~ '^sha256:[0-9a-f]{64}$'),
    web_digest TEXT NOT NULL CHECK (web_digest ~ '^sha256:[0-9a-f]{64}$'),
    thumbnail_digest TEXT NOT NULL CHECK (thumbnail_digest ~ '^sha256:[0-9a-f]{64}$'),
    media_type TEXT NOT NULL CHECK (media_type = 'image/png'),
    original_width INTEGER NOT NULL CHECK (original_width BETWEEN 1 AND 4096),
    original_height INTEGER NOT NULL CHECK (original_height BETWEEN 1 AND 4096),
    web_width INTEGER NOT NULL CHECK (web_width BETWEEN 1 AND 1600),
    web_height INTEGER NOT NULL CHECK (web_height BETWEEN 1 AND 1600),
    thumbnail_width INTEGER NOT NULL CHECK (thumbnail_width BETWEEN 1 AND 512),
    thumbnail_height INTEGER NOT NULL CHECK (thumbnail_height BETWEEN 1 AND 512),
    alt_text TEXT NOT NULL CHECK (
        alt_text = btrim(alt_text)
        AND char_length(alt_text) BETWEEN 1 AND 500
        AND alt_text !~ '[[:cntrl:]]'
    ),
    moderation_result TEXT NOT NULL CHECK (
        moderation_result = 'provider_and_application_safe'
    ),
    selection_state TEXT NOT NULL CHECK (
        selection_state IN ('selected', 'superseded')
    ),
    estimated_cost_microusd BIGINT NOT NULL CHECK (estimated_cost_microusd >= 0),
    actual_cost_microusd BIGINT CHECK (actual_cost_microusd IS NULL OR actual_cost_microusd >= 0),
    license_id TEXT NOT NULL CHECK (
        license_id IN ('provider-output-operator-terms', 'deterministic-fake-fixture')
    ),
    provenance_summary TEXT NOT NULL CHECK (
        provenance_summary IN (
            'generated-from-committed-public-fictional-facts',
            'deterministic-network-free-test-fixture'
        )
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    published_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (job_id, campaign_session_id)
        REFERENCES generation_jobs(id, campaign_session_id) ON DELETE CASCADE,
    FOREIGN KEY (source_turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE,
    UNIQUE (artifact_id, campaign_session_id)
);

CREATE UNIQUE INDEX scene_image_one_selected_turn_idx
    ON scene_image_artifacts(campaign_session_id, source_turn_id)
    WHERE selection_state = 'selected';

CREATE INDEX scene_image_campaign_created_idx
    ON scene_image_artifacts(campaign_session_id, created_at DESC, artifact_id);

-- Invalid provider bytes are never publishable and are not joined to
-- generated_assets. The worker keeps only a digest, bounded byte count, and a
-- protected relative quarantine key for the Q13 fourteen-day diagnostic window.
CREATE TABLE scene_image_quarantines (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    job_id TEXT NOT NULL CHECK (
        octet_length(job_id) BETWEEN 1 AND 128
        AND job_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    attempt_id TEXT NOT NULL CHECK (
        octet_length(attempt_id) BETWEEN 1 AND 128
        AND attempt_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL CHECK (
        octet_length(campaign_session_id) BETWEEN 1 AND 128
        AND campaign_session_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    byte_digest TEXT CHECK (byte_digest IS NULL OR byte_digest ~ '^sha256:[0-9a-f]{64}$'),
    byte_length BIGINT CHECK (byte_length IS NULL OR byte_length BETWEEN 0 AND 33554432),
    storage_key TEXT CHECK (
        storage_key IS NULL
        OR (
            octet_length(storage_key) BETWEEN 1 AND 512
            AND storage_key ~ '^[A-Za-z0-9._/-]+$'
            AND storage_key !~ '(^|/)\.\.(/|$)'
        )
    ),
    reason_code TEXT NOT NULL CHECK (
        reason_code IN (
            'provider_url_rejected', 'base64_invalid', 'byte_limit',
            'format_invalid', 'dimensions_invalid', 'pixel_limit',
            'decode_failed', 'safety_rejected'
        )
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    delete_after TIMESTAMPTZ NOT NULL DEFAULT (CURRENT_TIMESTAMP + INTERVAL '14 days'),
    UNIQUE (job_id, attempt_id)
);

CREATE INDEX scene_image_quarantine_expiry_idx
    ON scene_image_quarantines(delete_after, id);
