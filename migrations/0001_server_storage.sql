CREATE TABLE campaign_sessions (
    id TEXT PRIMARY KEY,
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    revision BIGINT NOT NULL CHECK (revision > 0),
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE characters (
    id TEXT PRIMARY KEY,
    campaign_session_id TEXT REFERENCES campaign_sessions(id) ON DELETE SET NULL,
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    revision BIGINT NOT NULL CHECK (revision > 0),
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX characters_campaign_session_idx
    ON characters(campaign_session_id);

CREATE TABLE turn_audits (
    id TEXT PRIMARY KEY,
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    turn_number BIGINT NOT NULL CHECK (turn_number >= 0),
    actor_id TEXT,
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (id, campaign_session_id),
    UNIQUE (campaign_session_id, turn_number)
);

CREATE INDEX turn_audits_campaign_session_idx
    ON turn_audits(campaign_session_id, turn_number);

CREATE TABLE generated_assets (
    id TEXT PRIMARY KEY,
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    turn_id TEXT,
    asset_kind TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    location TEXT NOT NULL,
    prompt_fingerprint TEXT CHECK (
        prompt_fingerprint IS NULL
        OR prompt_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    metadata_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE
);

CREATE INDEX generated_assets_campaign_session_idx
    ON generated_assets(campaign_session_id, created_at);

CREATE INDEX generated_assets_turn_idx
    ON generated_assets(turn_id);
