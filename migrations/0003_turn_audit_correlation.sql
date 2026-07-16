ALTER TABLE turn_audits
    ADD COLUMN correlation_id TEXT CHECK (
        correlation_id IS NULL
        OR (
            octet_length(correlation_id) BETWEEN 1 AND 128
            AND correlation_id ~ '^[A-Za-z0-9_.:-]+$'
        )
    );

CREATE INDEX turn_audits_correlation_idx
    ON turn_audits(correlation_id)
    WHERE correlation_id IS NOT NULL;
