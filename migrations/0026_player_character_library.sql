-- Account-owned, campaign-independent character library storage.
-- A player character stores identity and reusable creation choices only.
-- It has no campaign_id, level, XP, HP, or any campaign-derived runtime state.
-- Level-dependent sheet derivation occurs only after a campaign instance is
-- created from a library character (migration 0027).

CREATE TABLE player_characters (
    id TEXT PRIMARY KEY CHECK (
        id ~ '^character:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
    ),
    owner_account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL DEFAULT 0,
    display_name TEXT NOT NULL CHECK (
        octet_length(display_name) BETWEEN 1 AND 200
        AND display_name = btrim(display_name)
    ),
    choices_json JSONB NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (owner_account_id != 'account:local' OR id LIKE 'character:local-%')
);

-- Index for listing a player's characters sorted by most recently updated.
CREATE INDEX idx_player_characters_owner_updated
    ON player_characters (owner_account_id, updated_at DESC, id);

-- Ensure one character per owner per display name to prevent confusion.
CREATE UNIQUE INDEX idx_player_characters_owner_display_name
    ON player_characters (owner_account_id, lower(display_name));

-- Drafts are temporary creation-state documents with a TTL.
CREATE TABLE player_character_drafts (
    id TEXT PRIMARY KEY CHECK (
        id ~ '^draft:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
    ),
    owner_account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    step TEXT NOT NULL DEFAULT 'campaign_theme',
    choices_json JSONB,
    reviewed BOOLEAN NOT NULL DEFAULT FALSE,
    committed_character_id TEXT REFERENCES player_characters(id) ON DELETE SET NULL,
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        committed_character_id IS NULL
        OR (reviewed = TRUE AND choices_json IS NOT NULL)
    )
);

CREATE INDEX idx_player_character_drafts_owner
    ON player_character_drafts (owner_account_id, updated_at DESC);

-- Append-only audit trail for character library mutations.
CREATE TABLE player_character_audits (
    id BIGSERIAL PRIMARY KEY,
    character_id TEXT NOT NULL REFERENCES player_characters(id) ON DELETE CASCADE,
    owner_account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    revision BIGINT NOT NULL,
    audit_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (octet_length(action) BETWEEN 1 AND 64)
);

CREATE INDEX idx_player_character_audits_character
    ON player_character_audits (character_id, created_at DESC);

-- Idempotency receipts prevent duplicate mutations.
CREATE TABLE player_character_command_receipts (
    id BIGSERIAL PRIMARY KEY,
    owner_account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    character_id TEXT NOT NULL REFERENCES player_characters(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL CHECK (
        idempotency_key ~ '^[a-zA-Z0-9_-]{1,128}$'
    ),
    command_kind TEXT NOT NULL CHECK (octet_length(command_kind) BETWEEN 1 AND 64),
    request_fingerprint TEXT NOT NULL CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    result_revision BIGINT NOT NULL,
    response_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (character_id, idempotency_key)
);

CREATE INDEX idx_player_character_receipts_owner
    ON player_character_command_receipts (owner_account_id, character_id);
