-- Typed campaign-safety policy. Values are closed codes or opaque participant
-- IDs; free-form safety disclosures and participant names are never stored.

ALTER TABLE campaign_inspiration_settings
    ADD COLUMN tone TEXT NOT NULL DEFAULT 'gothic_adventure' CHECK (
        tone IN ('gothic_adventure', 'hopeful_adventure', 'lighthearted_adventure')
    );

CREATE TABLE campaign_inspiration_lines (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_inspiration_settings(campaign_session_id)
        ON DELETE CASCADE,
    safety_code TEXT NOT NULL CHECK (
        octet_length(safety_code) BETWEEN 1 AND 128
        AND safety_code ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (campaign_session_id, safety_code)
);

CREATE TABLE campaign_inspiration_veils (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_inspiration_settings(campaign_session_id)
        ON DELETE CASCADE,
    safety_code TEXT NOT NULL CHECK (
        octet_length(safety_code) BETWEEN 1 AND 128
        AND safety_code ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (campaign_session_id, safety_code)
);

CREATE TABLE campaign_inspiration_excluded_topics (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_inspiration_settings(campaign_session_id)
        ON DELETE CASCADE,
    safety_code TEXT NOT NULL CHECK (
        octet_length(safety_code) BETWEEN 1 AND 128
        AND safety_code ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (campaign_session_id, safety_code)
);

CREATE TABLE campaign_inspiration_excluded_participants (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_inspiration_settings(campaign_session_id)
        ON DELETE CASCADE,
    participant_id TEXT NOT NULL CHECK (
        participant_id ~ '^participant:[0-9a-f]{32}$'
    ),
    PRIMARY KEY (campaign_session_id, participant_id)
);

