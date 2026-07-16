CREATE TABLE command_receipts (
    campaign_session_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    expected_revision BIGINT NOT NULL CHECK (expected_revision > 0),
    result_revision BIGINT NOT NULL CHECK (result_revision = expected_revision + 1),
    audit_id TEXT NOT NULL,
    response_json TEXT NOT NULL CHECK (
        octet_length(response_json) BETWEEN 1 AND 65536
        AND jsonb_typeof(response_json::jsonb) IS NOT NULL
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (campaign_session_id, idempotency_key),
    FOREIGN KEY (audit_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE
);

CREATE INDEX command_receipts_audit_idx
    ON command_receipts(audit_id, campaign_session_id);
