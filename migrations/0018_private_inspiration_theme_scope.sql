-- Theme compatibility is registry policy, not source-authored model
-- instruction. Every source version must be explicitly allowlisted for one or
-- more immutable campaign theme packs before it can enter the weighted set.

CREATE TABLE private_inspiration_source_themes (
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    theme_pack_id TEXT NOT NULL CHECK (
        octet_length(theme_pack_id) BETWEEN 1 AND 128
        AND theme_pack_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (source_id, source_version, theme_pack_id),
    FOREIGN KEY (source_id, source_version)
        REFERENCES private_inspiration_sources(source_id, source_version)
        ON DELETE CASCADE
);

