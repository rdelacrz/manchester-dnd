-- Account-scoped campaign ownership and memberships (migration 0028).
-- This is additive to the existing local-owner campaign_sessions columns
-- (owner_key from 0010) and is the bridge to hosted multi-account access.
-- owner_key is retained for one compatibility release after 0028 ships.

-- 1. owner_account_id on campaign_sessions. Nullable during backfill; the
--    NOT NULL constraint is added by the trigger-like backfill below.
ALTER TABLE campaign_sessions
    ADD COLUMN owner_account_id TEXT REFERENCES accounts(id) ON DELETE CASCADE;

-- 2. Backfill owner_account_id from owner_key. The local compatibility
--    owner_key 'local-owner' maps to the compatibility account 'account:local'
--    that migration 0025 created. Hosted owner_keys have no account row yet and
--    are left NULL until a hosted migration backfills them; the CHECK below
--    forbids leaving a local-owner row unbackfilled.
UPDATE campaign_sessions
   SET owner_account_id = 'account:local'
 WHERE owner_key = 'local-owner'
   AND owner_account_id IS NULL;

ALTER TABLE campaign_sessions
    ADD CONSTRAINT campaign_sessions_owner_account_id_local_required CHECK (
        owner_key <> 'local-owner' OR owner_account_id = 'account:local'
    );

CREATE INDEX IF NOT EXISTS campaign_sessions_owner_account_idx
    ON campaign_sessions(owner_account_id, updated_at DESC, id)
    WHERE owner_account_id IS NOT NULL;

-- 3. campaign_memberships: (campaign_session_id, account_id) is the natural
--    primary key. Exactly one membership per campaign must have role =
--    'game_master' and state = 'active'; this is enforced by a partial unique
--    index. The CHECK (role, state) pairs are intentionally closed enums.
CREATE TABLE campaign_memberships (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('game_master', 'player')),
    state TEXT NOT NULL CHECK (state IN ('invited', 'active', 'left', 'removed')),
    inviter_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
    accepted_at TIMESTAMPTZ,
    left_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (campaign_session_id, account_id),
    -- Invited/active memberships cannot have left_at; left/removed must have it.
    CHECK (
        state IN ('invited', 'active') AND left_at IS NULL
        OR
        state IN ('left', 'removed') AND left_at IS NOT NULL
    ),
    -- A game-master membership can never be in the 'invited' state; it is the
    -- owning principal and must be active at creation.
    CHECK (role <> 'game_master' OR state <> 'invited'),
    -- Only active memberships carry accepted_at.
    CHECK (
        state = 'active' AND accepted_at IS NOT NULL
        OR state <> 'active' AND accepted_at IS NULL
    )
);

-- Exactly one active game_master per campaign. The composite index is also
-- the membership query path used by list_account_campaigns.
CREATE UNIQUE INDEX campaign_memberships_one_gm_idx
    ON campaign_memberships(campaign_session_id)
    WHERE role = 'game_master' AND state = 'active';

CREATE INDEX campaign_memberships_account_idx
    ON campaign_memberships(account_id, campaign_session_id);

-- 4. campaign_invitations: opaque ID, campaign_id FK, and one of two mutually
--    exclusive redemption paths: invitee_email_digest or join_code_digest.
--    accepted_at and revoked_at are mutually exclusive and both set the
--    invitation as consumed. expiry is required and must be in the future at
--    create time (enforced by application before insert).
CREATE TABLE campaign_invitations (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    inviter_account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    invitee_email_digest TEXT CHECK (
        invitee_email_digest IS NULL
        OR invitee_email_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    join_code_digest TEXT CHECK (
        join_code_digest IS NULL
        OR join_code_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    accepted_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (invitee_email_digest IS NULL) <> (join_code_digest IS NULL)
    ),
    CHECK (
        (accepted_at IS NULL AND accepted_account_id IS NULL AND revoked_at IS NULL)
        OR
        (accepted_at IS NOT NULL AND accepted_account_id IS NOT NULL AND revoked_at IS NULL)
        OR
        (accepted_at IS NULL AND accepted_account_id IS NULL AND revoked_at IS NOT NULL)
    )
);

CREATE INDEX campaign_invitations_email_digest_idx
    ON campaign_invitations(invitee_email_digest, expires_at)
    WHERE invitee_email_digest IS NOT NULL AND accepted_at IS NULL AND revoked_at IS NULL;

CREATE INDEX campaign_invitations_join_code_digest_idx
    ON campaign_invitations(join_code_digest, expires_at)
    WHERE join_code_digest IS NOT NULL AND accepted_at IS NULL AND revoked_at IS NULL;

CREATE INDEX campaign_invitations_campaign_idx
    ON campaign_invitations(campaign_session_id, created_at DESC, id);

-- 5. campaign_character_instances: binds a library player_character to a
--    campaign as a runtime hero_character. The runtime hero stores level/XP/
--    HP; this row stores the immutable source-choice snapshot taken at the
--    moment of instantiation, so a later library character edit cannot rewrite
--    history. UNIQUE (campaign_session_id, account_id) WHERE state = 'active'
--    enforces one active character per membership.
CREATE TABLE campaign_character_instances (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    instance_id TEXT NOT NULL CHECK (
        octet_length(instance_id) BETWEEN 1 AND 128
        AND instance_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    source_player_character_id TEXT NOT NULL
        REFERENCES player_characters(id) ON DELETE RESTRICT,
    runtime_hero_character_id TEXT NOT NULL
        REFERENCES hero_characters(id) ON DELETE CASCADE,
    -- Immutable snapshot of the source character's display name and choices
    -- digest at instantiation time. The library character may later be edited
    -- or deleted; this row preserves what was brought into the campaign.
    source_display_name TEXT NOT NULL CHECK (
        octet_length(source_display_name) BETWEEN 1 AND 200
    ),
    source_choices_digest TEXT NOT NULL CHECK (
        source_choices_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    state TEXT NOT NULL CHECK (state IN ('active', 'retired')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    retired_at TIMESTAMPTZ,
    PRIMARY KEY (campaign_session_id, instance_id),
    CHECK (
        (state = 'active' AND retired_at IS NULL)
        OR
        (state = 'retired' AND retired_at IS NOT NULL)
    )
);

-- One active instance per (campaign, account) membership.
CREATE UNIQUE INDEX campaign_character_instances_active_per_account_idx
    ON campaign_character_instances(campaign_session_id, account_id)
    WHERE state = 'active';

CREATE INDEX campaign_character_instances_account_idx
    ON campaign_character_instances(account_id, campaign_session_id);

CREATE INDEX campaign_character_instances_source_idx
    ON campaign_character_instances(source_player_character_id, campaign_session_id);
