-- Body-free singleton used by the offline recovery drill and read-only
-- operational snapshot. It contains no campaign identifiers or content.
CREATE TABLE operator_recovery_status (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    schema_version SMALLINT NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    last_backup_completed_at TIMESTAMPTZ,
    last_backup_vault_digest TEXT CHECK (
        last_backup_vault_digest IS NULL
        OR last_backup_vault_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    last_restore_test_completed_at TIMESTAMPTZ,
    last_restore_test_result TEXT CHECK (
        last_restore_test_result IS NULL
        OR last_restore_test_result IN ('passed', 'failed')
    ),
    last_restore_source_digest TEXT CHECK (
        last_restore_source_digest IS NULL
        OR last_restore_source_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (last_backup_completed_at IS NULL) = (last_backup_vault_digest IS NULL)
    ),
    CHECK (
        (last_restore_test_completed_at IS NULL)
        = (last_restore_test_result IS NULL)
    ),
    CHECK (
        last_restore_source_digest IS NULL
        OR last_restore_test_completed_at IS NOT NULL
    )
);

INSERT INTO operator_recovery_status (singleton) VALUES (TRUE);
