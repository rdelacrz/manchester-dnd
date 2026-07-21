-- Custom-action point ledger (Task 18)
--
-- Append-only ledger for custom-action points. Points are spent atomically
-- in the same transaction as the authoritative game turn commit.

CREATE TABLE IF NOT EXISTS custom_action_point_ledger (
    id              TEXT PRIMARY KEY,
    account_id      TEXT NOT NULL REFERENCES accounts(id),
    campaign_id     TEXT NOT NULL REFERENCES campaign_sessions(id),
    runtime_character_id TEXT NOT NULL REFERENCES hero_characters(id),
    play_session_id TEXT NOT NULL,
    turn_revision   BIGINT NOT NULL,
    amount          INTEGER NOT NULL,
    reason          TEXT NOT NULL CHECK (reason IN ('initial_grant', 'earned', 'custom_action_spent', 'administrative_refund')),
    idempotency_key TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Prevent duplicate charges for the same idempotent spend
    UNIQUE (idempotency_key, reason)
);

-- Materialized balance locked with SELECT FOR UPDATE
CREATE TABLE IF NOT EXISTS custom_action_point_balances (
    account_id           TEXT NOT NULL,
    campaign_id          TEXT NOT NULL,
    runtime_character_id TEXT NOT NULL,
    play_session_id      TEXT NOT NULL,
    balance              INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (account_id, campaign_id, runtime_character_id)
);

-- Index for balance lookups
CREATE INDEX IF NOT EXISTS idx_cap_balances_account_campaign
    ON custom_action_point_balances (account_id, campaign_id);

-- Index for ledger queries by play session
CREATE INDEX IF NOT EXISTS idx_cap_ledger_session
    ON custom_action_point_ledger (play_session_id, created_at DESC);
