-- Authentication storage is additive. Hosted access remains fail-closed until
-- the HTTP session, CSRF, and object-authorization boundaries are complete.
CREATE TABLE accounts (
    id TEXT PRIMARY KEY CHECK (
        id = 'account:local'
        OR id ~ '^account:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
    ),
    normalized_email TEXT UNIQUE CHECK (
        normalized_email IS NULL
        OR (
            octet_length(normalized_email) BETWEEN 3 AND 320
            AND normalized_email = lower(btrim(normalized_email))
            AND normalized_email !~ '[[:space:]]'
            AND normalized_email ~ '^[^@]+@[^@]+$'
        )
    ),
    display_name TEXT NOT NULL CHECK (
        octet_length(display_name) BETWEEN 1 AND 200
        AND display_name = btrim(display_name)
    ),
    password_phc TEXT CHECK (
        password_phc IS NULL
        OR (
            octet_length(password_phc) BETWEEN 32 AND 1024
            AND password_phc LIKE '$argon2id$%'
        )
    ),
    login_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    password_changed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (login_enabled AND normalized_email IS NOT NULL AND password_phc IS NOT NULL
            AND password_changed_at IS NOT NULL)
        OR
        (NOT login_enabled AND normalized_email IS NULL AND password_phc IS NULL
            AND password_changed_at IS NULL)
    )
);

-- The compatibility row is not a login identity. Existing owner_key values
-- remain untouched until campaign membership migration 0027.
INSERT INTO accounts (id, display_name, login_enabled)
VALUES ('account:local', 'Local player', FALSE);

CREATE TABLE account_sessions (
    id TEXT PRIMARY KEY CHECK (
        id ~ '^session:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
    ),
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    token_digest TEXT NOT NULL UNIQUE CHECK (token_digest ~ '^sha256:[0-9a-f]{64}$'),
    csrf_digest TEXT NOT NULL UNIQUE CHECK (csrf_digest ~ '^sha256:[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    idle_expires_at TIMESTAMPTZ NOT NULL,
    absolute_expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    CHECK (last_seen_at >= created_at),
    CHECK (idle_expires_at > created_at),
    CHECK (absolute_expires_at > created_at),
    CHECK (idle_expires_at <= absolute_expires_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at),
    UNIQUE (id, account_id)
);

CREATE INDEX account_sessions_active_lookup_idx
    ON account_sessions(token_digest, idle_expires_at, absolute_expires_at)
    WHERE revoked_at IS NULL;
CREATE INDEX account_sessions_account_active_idx
    ON account_sessions(account_id, created_at DESC, id)
    WHERE revoked_at IS NULL;
CREATE INDEX account_sessions_expiry_cleanup_idx
    ON account_sessions(absolute_expires_at, idle_expires_at);

CREATE TABLE auth_throttle_buckets (
    key_digest TEXT NOT NULL CHECK (key_digest ~ '^hmac-sha256:[0-9a-f]{64}$'),
    action_kind TEXT NOT NULL CHECK (action_kind IN ('login', 'signup')),
    window_started_at TIMESTAMPTZ NOT NULL,
    attempt_count INTEGER NOT NULL CHECK (attempt_count BETWEEN 0 AND 1000000),
    blocked_until TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (key_digest, action_kind),
    CHECK (blocked_until IS NULL OR blocked_until >= window_started_at)
);
CREATE INDEX auth_throttle_buckets_cleanup_idx
    ON auth_throttle_buckets(updated_at, blocked_until);

CREATE TABLE authentication_audits (
    id TEXT PRIMARY KEY CHECK (
        id ~ '^auth-audit:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
    ),
    account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
    event_kind TEXT NOT NULL CHECK (
        event_kind IN ('signup', 'login', 'logout', 'session_expired', 'password_rehashed')
    ),
    outcome_class TEXT NOT NULL CHECK (
        outcome_class IN ('success', 'invalid_credentials', 'throttled', 'invalid_request', 'internal_failure')
    ),
    correlation_id TEXT NOT NULL CHECK (
        octet_length(correlation_id) BETWEEN 1 AND 128
        AND correlation_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX authentication_audits_account_time_idx
    ON authentication_audits(account_id, created_at DESC, id);
CREATE INDEX authentication_audits_time_idx
    ON authentication_audits(created_at DESC, id);
