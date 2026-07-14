CREATE TABLE command_receipts (
    campaign_session_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL CHECK (
        length(request_fingerprint) = 71
        AND substr(request_fingerprint, 1, 7) = 'sha256:'
        AND substr(request_fingerprint, 8) NOT GLOB '*[^0-9a-f]*'
    ),
    expected_revision INTEGER NOT NULL CHECK (expected_revision > 0),
    result_revision INTEGER NOT NULL CHECK (result_revision = expected_revision + 1),
    audit_id TEXT NOT NULL,
    response_json TEXT NOT NULL CHECK (
        json_valid(response_json)
        AND length(CAST(response_json AS BLOB)) BETWEEN 1 AND 65536
    ),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (campaign_session_id, idempotency_key),
    FOREIGN KEY (audit_id, campaign_session_id)
        REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE
);

CREATE INDEX command_receipts_audit_idx
    ON command_receipts(audit_id, campaign_session_id);
