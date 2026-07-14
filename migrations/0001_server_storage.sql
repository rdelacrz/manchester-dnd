PRAGMA foreign_keys = ON;

CREATE TABLE campaign_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE characters (
    id TEXT PRIMARY KEY NOT NULL,
    campaign_session_id TEXT REFERENCES campaign_sessions(id) ON DELETE SET NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX characters_campaign_session_idx
    ON characters(campaign_session_id);

CREATE TABLE turn_audits (
    id TEXT PRIMARY KEY NOT NULL,
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    turn_number INTEGER NOT NULL CHECK (turn_number >= 0),
    actor_id TEXT,
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (id, campaign_session_id),
    UNIQUE (campaign_session_id, turn_number)
);

CREATE INDEX turn_audits_campaign_session_idx
    ON turn_audits(campaign_session_id, turn_number);

CREATE TABLE generated_assets (
    id TEXT PRIMARY KEY NOT NULL,
    campaign_session_id TEXT NOT NULL REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    turn_id TEXT,
    asset_kind TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    location TEXT NOT NULL,
    prompt_fingerprint TEXT CHECK (
        prompt_fingerprint IS NULL OR (
            length(prompt_fingerprint) = 71
            AND substr(prompt_fingerprint, 1, 7) = 'sha256:'
            AND substr(prompt_fingerprint, 8) NOT GLOB '*[^0-9a-f]*'
        )
    ),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    FOREIGN KEY (turn_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE
);

CREATE INDEX generated_assets_campaign_session_idx
    ON generated_assets(campaign_session_id, created_at);

CREATE INDEX generated_assets_turn_idx
    ON generated_assets(turn_id);
