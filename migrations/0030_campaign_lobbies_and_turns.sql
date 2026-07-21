-- Durable lobby transitions and turn-boundary state machine (Task 15).
-- This migration extends the existing campaign_play_sessions table (0010)
-- with multi-account lobby semantics and adds the participants, turn-state,
-- and append-only turn-control audit tables needed for hosted play.
--
-- Design notes:
--   * campaign_play_sessions.state is broadened from ('open','closed') to
--     ('waiting','active','closed') via a DROP+ADD CHECK. Existing rows are
--     backfilled: 'open' → 'waiting' (a never-started local session) and
--     'closed' stays 'closed'.
--   * start_policy captures whether the lobby waits for every member to be
--     ready (wait_for_all) or fills absent humans with AI substitutes
--     (start_with_ai_substitutes).
--   * gm_account_id is the server-derived GM principal that opened the lobby.
--     It is non-null for all rows created after 0030; legacy local-owner rows
--     are backfilled to 'account:local'.
--   * expected_membership_revision / active_turn_revision are optimistic
--     concurrency guards consumed by the repository layer.

-- Drop the old state CHECK constraints (0010 added two: a simple
-- state_check and a compound _check that also validates ended/closed
-- invariants tied to 'open'/'closed' semantics).
DO $$
DECLARE
    constraint_name TEXT;
BEGIN
    SELECT con.conname INTO constraint_name
      FROM pg_constraint con
      JOIN pg_class rel ON rel.oid = con.conrelid
     WHERE rel.relname = 'campaign_play_sessions'
       AND con.contype = 'c'
       AND pg_get_constraintdef(con.oid) LIKE '%state IN (''open'', ''closed'')%';
    IF constraint_name IS NOT NULL THEN
        EXECUTE format('ALTER TABLE campaign_play_sessions DROP CONSTRAINT %I', constraint_name);
    END IF;
    -- Also drop the compound check from 0010 if present.
    IF EXISTS (
        SELECT 1 FROM pg_constraint con
          JOIN pg_class rel ON rel.oid = con.conrelid
         WHERE rel.relname = 'campaign_play_sessions'
           AND con.contype = 'c'
           AND con.conname = 'campaign_play_sessions_check'
    ) THEN
        ALTER TABLE campaign_play_sessions DROP CONSTRAINT campaign_play_sessions_check;
    END IF;
END $$;

ALTER TABLE campaign_play_sessions
    ADD COLUMN gm_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL;

ALTER TABLE campaign_play_sessions
    ADD COLUMN start_policy TEXT NOT NULL DEFAULT 'wait_for_all'
        CHECK (start_policy IN ('wait_for_all', 'start_with_ai_substitutes'));

ALTER TABLE campaign_play_sessions
    ADD COLUMN expected_membership_revision BIGINT NOT NULL DEFAULT 0
        CHECK (expected_membership_revision >= 0);

ALTER TABLE campaign_play_sessions
    ADD COLUMN active_turn_revision BIGINT NOT NULL DEFAULT 0
        CHECK (active_turn_revision >= 0);

-- Backfill gm_account_id for legacy rows using the campaign owner_account_id.
UPDATE campaign_play_sessions ps
   SET gm_account_id = cs.owner_account_id
  FROM campaign_sessions cs
 WHERE ps.campaign_session_id = cs.id
   AND ps.gm_account_id IS NULL;

ALTER TABLE campaign_play_sessions
    DROP CONSTRAINT IF EXISTS campaign_play_sessions_gm_required_for_new;
ALTER TABLE campaign_play_sessions
    ADD CONSTRAINT campaign_play_sessions_gm_required_for_new CHECK (
        state = 'closed'
        OR state = 'waiting'
        OR gm_account_id IS NOT NULL
    );

-- Drop old state constraint before backfill, then add the new one.
ALTER TABLE campaign_play_sessions
    DROP CONSTRAINT IF EXISTS campaign_play_sessions_state_check;

-- Backfill state: 'open' → 'waiting'. Any legacy 'open' row never reached the
-- active play boundary under the new model.
UPDATE campaign_play_sessions SET state = 'waiting' WHERE state = 'open';

ALTER TABLE campaign_play_sessions
    ADD CONSTRAINT campaign_play_sessions_state_check CHECK (
        state IN ('waiting', 'active', 'closed')
    );

-- The one-open-per-campaign unique index from 0010 still applies; rebuild it
-- to cover the new vocabulary (waiting + active replace open).
DROP INDEX IF EXISTS campaign_play_sessions_one_open_idx;
CREATE UNIQUE INDEX campaign_play_sessions_one_open_idx
    ON campaign_play_sessions(campaign_session_id)
    WHERE state IN ('waiting', 'active');

CREATE INDEX campaign_play_sessions_gm_idx
    ON campaign_play_sessions(gm_account_id, state, opened_at DESC, id)
    WHERE gm_account_id IS NOT NULL;

-- 2. campaign_play_session_participants: one row per (play_session, account).
--    Readiness is durable — it is never inferred from presence. A participant
--    that leaves mid-lobby keeps their row (state 'left') so the lobby
--    history is preserved; the start transition treats 'left' as absent.
CREATE TABLE campaign_play_session_participants (
    play_session_id TEXT NOT NULL
        REFERENCES campaign_play_sessions(id) ON DELETE CASCADE,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    runtime_character_id TEXT CHECK (
        runtime_character_id IS NULL
        OR (octet_length(runtime_character_id) BETWEEN 1 AND 128
            AND runtime_character_id ~ '^[A-Za-z0-9_.:-]+$')
    ),
    state TEXT NOT NULL CHECK (
        state IN ('not_ready', 'ready', 'human_active', 'ai_substitute', 'left')
    ),
    ready_at TIMESTAMPTZ,
    handoff_revision BIGINT NOT NULL DEFAULT 0 CHECK (handoff_revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (play_session_id, account_id),
    CHECK (
        (state IN ('not_ready', 'left') AND ready_at IS NULL)
        OR
        (state IN ('ready', 'human_active', 'ai_substitute') AND ready_at IS NOT NULL)
    )
);

CREATE INDEX campaign_play_session_participants_account_idx
    ON campaign_play_session_participants(account_id, play_session_id);

CREATE INDEX campaign_play_session_participants_session_idx
    ON campaign_play_session_participants(play_session_id, state, account_id);

-- 3. campaign_turn_states: exactly one row per active play session. The phase
--    column is the turn-boundary state machine; revision is the optimistic
--    guard for advancing the turn. bounded_json carries the authored scene
--    payload for the current boundary (never raw prompts).
CREATE TABLE campaign_turn_states (
    play_session_id TEXT PRIMARY KEY
        REFERENCES campaign_play_sessions(id) ON DELETE CASCADE,
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    phase TEXT NOT NULL CHECK (
        phase IN ('game_master_generation', 'player_action', 'resolving', 'completed')
    ),
    active_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
    active_character_id TEXT CHECK (
        active_character_id IS NULL
        OR (octet_length(active_character_id) BETWEEN 1 AND 128
            AND active_character_id ~ '^[A-Za-z0-9_.:-]+$')
    ),
    round BIGINT NOT NULL CHECK (round > 0),
    turn_number BIGINT NOT NULL CHECK (turn_number > 0),
    revision BIGINT NOT NULL CHECK (revision > 0),
    bounded_json JSONB NOT NULL CHECK (
        jsonb_typeof(bounded_json) = 'object'
        AND octet_length(bounded_json::text) BETWEEN 2 AND 65536
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (play_session_id <> ''),
    UNIQUE (play_session_id, revision)
);

CREATE INDEX campaign_turn_states_active_character_idx
    ON campaign_turn_states(active_account_id, active_character_id, play_session_id)
    WHERE active_account_id IS NOT NULL;

-- 4. turn_control_audits: append-only ledger of every turn-boundary transition.
--    No UPDATE path is ever issued against this table; it is the durable
--    history of lobby and turn-control decisions.
CREATE TABLE turn_control_audits (
    id TEXT PRIMARY KEY CHECK (
        octet_length(id) BETWEEN 1 AND 128
        AND id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    play_session_id TEXT NOT NULL
        REFERENCES campaign_play_sessions(id) ON DELETE CASCADE,
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    event_kind TEXT NOT NULL CHECK (
        event_kind IN (
            'lobby_created',
            'member_readied',
            'member_unreadied',
            'member_left',
            'lobby_started',
            'lobby_started_replay',
            'lobby_ended',
            'turn_boundary',
            'handoff'
        )
    ),
    actor_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
    from_phase TEXT CHECK (
        from_phase IS NULL
        OR from_phase IN ('game_master_generation', 'player_action', 'resolving', 'completed')
    ),
    to_phase TEXT CHECK (
        to_phase IS NULL
        OR to_phase IN ('game_master_generation', 'player_action', 'resolving', 'completed')
    ),
    from_revision BIGINT CHECK (from_revision IS NULL OR from_revision >= 0),
    to_revision BIGINT CHECK (to_revision IS NULL OR to_revision >= 0),
    payload_json JSONB NOT NULL CHECK (
        jsonb_typeof(payload_json) = 'object'
        AND octet_length(payload_json::text) BETWEEN 2 AND 16384
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX turn_control_audits_session_idx
    ON turn_control_audits(campaign_session_id, play_session_id, created_at DESC, id);

CREATE INDEX turn_control_audits_actor_idx
    ON turn_control_audits(actor_account_id, created_at DESC, id)
    WHERE actor_account_id IS NOT NULL;

-- 5. Lobby command receipts: idempotency for start/end so a duplicate replay
--    returns the stored outcome rather than creating a second active session.
CREATE TABLE lobby_command_receipts (
    play_session_id TEXT NOT NULL
        REFERENCES campaign_play_sessions(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    command_kind TEXT NOT NULL CHECK (
        command_kind IN ('lobby_start', 'lobby_end')
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    expected_revision BIGINT NOT NULL CHECK (expected_revision >= 0),
    result_revision BIGINT NOT NULL CHECK (result_revision > 0),
    response_json TEXT NOT NULL CHECK (
        octet_length(response_json) BETWEEN 1 AND 65536
        AND jsonb_typeof(response_json::jsonb) IS NOT NULL
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (play_session_id, idempotency_key)
);
