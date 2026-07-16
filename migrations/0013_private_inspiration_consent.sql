-- Durable, body-free consent and eligibility state for private inspiration.
-- Raw Markdown, filesystem paths, names, contact details, consent prose, and
-- generated bodies have no columns in this schema.

CREATE TABLE campaign_inspiration_settings (
    campaign_session_id TEXT PRIMARY KEY
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    revision BIGINT NOT NULL CHECK (revision > 0),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    safety_setup_complete BOOLEAN NOT NULL DEFAULT FALSE,
    adults_only BOOLEAN NOT NULL DEFAULT TRUE CHECK (adults_only),
    fictional_distance TEXT NOT NULL DEFAULT 'high_locked'
        CHECK (fictional_distance = 'high_locked'),
    audience TEXT NOT NULL DEFAULT 'private_campaign'
        CHECK (audience = 'private_campaign'),
    media TEXT NOT NULL DEFAULT 'text'
        CHECK (media = 'text'),
    q11_policy_id TEXT NOT NULL DEFAULT 'q11_conservative_v1'
        CHECK (q11_policy_id = 'q11_conservative_v1'),
    safety_setup_evidence_digest TEXT CHECK (
        safety_setup_evidence_digest IS NULL
        OR safety_setup_evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    safety_reviewer_id TEXT CHECK (
        safety_reviewer_id IS NULL
        OR safety_reviewer_id ~ '^operator:[0-9a-f]{32}$'
    ),
    safety_reviewed_at_epoch BIGINT CHECK (safety_reviewed_at_epoch >= 0),
    rng_cursor BIGINT NOT NULL DEFAULT 0 CHECK (rng_cursor >= 0),
    updated_at_epoch BIGINT NOT NULL CHECK (updated_at_epoch >= 0),
    CHECK (
        (safety_setup_complete
            AND safety_setup_evidence_digest IS NOT NULL
            AND safety_reviewer_id IS NOT NULL
            AND safety_reviewed_at_epoch IS NOT NULL)
        OR
        (NOT safety_setup_complete
            AND safety_setup_evidence_digest IS NULL
            AND safety_reviewer_id IS NULL
            AND safety_reviewed_at_epoch IS NULL)
    ),
    CHECK (NOT enabled OR safety_setup_complete)
);

CREATE TABLE campaign_inspiration_allowed_sensitivities (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_inspiration_settings(campaign_session_id)
        ON DELETE CASCADE,
    sensitivity_code TEXT NOT NULL CHECK (
        octet_length(sensitivity_code) BETWEEN 1 AND 128
        AND sensitivity_code ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (campaign_session_id, sensitivity_code)
);

CREATE TABLE private_inspiration_participants (
    participant_id TEXT PRIMARY KEY CHECK (
        participant_id ~ '^participant:[0-9a-f]{32}$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    verification_state TEXT NOT NULL CHECK (
        verification_state IN ('verified', 'revoked')
    ),
    verification_method TEXT NOT NULL CHECK (
        verification_method IN (
            'participant_signed_confirmation',
            'timestamped_two_channel_acknowledgement'
        )
    ),
    verification_evidence_digest TEXT NOT NULL CHECK (
        verification_evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    verifier_id TEXT NOT NULL CHECK (
        verifier_id ~ '^operator:[0-9a-f]{32}$'
    ),
    verified_at_epoch BIGINT NOT NULL CHECK (verified_at_epoch >= 0),
    revoked_at_epoch BIGINT CHECK (revoked_at_epoch >= verified_at_epoch),
    CHECK (
        (verification_state = 'verified' AND revoked_at_epoch IS NULL)
        OR (verification_state = 'revoked' AND revoked_at_epoch IS NOT NULL)
    )
);

CREATE TABLE private_inspiration_sources (
    source_id TEXT NOT NULL CHECK (
        source_id ~ '^event-source-[0-9a-f]{24}$'
    ),
    source_version BIGINT NOT NULL CHECK (source_version > 0),
    source_digest TEXT NOT NULL CHECK (
        source_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    category_id TEXT NOT NULL CHECK (
        octet_length(category_id) BETWEEN 1 AND 128
        AND category_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    owner_participant_id TEXT NOT NULL
        REFERENCES private_inspiration_participants(participant_id),
    review_state TEXT NOT NULL CHECK (
        review_state IN ('pending', 'approved', 'rejected', 'quarantined')
    ),
    q11_screened BOOLEAN NOT NULL DEFAULT FALSE,
    audience TEXT NOT NULL CHECK (audience = 'private_campaign'),
    transformation TEXT NOT NULL CHECK (transformation = 'high_fiction_distance_v1'),
    provenance_digest TEXT NOT NULL CHECK (
        provenance_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    review_evidence_digest TEXT CHECK (
        review_evidence_digest IS NULL
        OR review_evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    reviewer_id TEXT CHECK (
        reviewer_id IS NULL
        OR reviewer_id ~ '^operator:[0-9a-f]{32}$'
    ),
    reviewed_at_epoch BIGINT CHECK (reviewed_at_epoch >= 0),
    expires_at_epoch BIGINT CHECK (expires_at_epoch >= 0),
    registered_at_epoch BIGINT NOT NULL CHECK (registered_at_epoch >= 0),
    PRIMARY KEY (source_id, source_version),
    UNIQUE (source_id, source_digest),
    UNIQUE (source_id, source_version, source_digest),
    CHECK (
        (review_state = 'pending'
            AND NOT q11_screened
            AND review_evidence_digest IS NULL
            AND reviewer_id IS NULL
            AND reviewed_at_epoch IS NULL)
        OR
        (review_state IN ('approved', 'rejected', 'quarantined')
            AND review_evidence_digest IS NOT NULL
            AND reviewer_id IS NOT NULL
            AND reviewed_at_epoch IS NOT NULL)
    ),
    CHECK (review_state <> 'approved' OR q11_screened)
);

CREATE TABLE private_inspiration_source_participants (
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    participant_id TEXT NOT NULL
        REFERENCES private_inspiration_participants(participant_id),
    PRIMARY KEY (source_id, source_version, participant_id),
    FOREIGN KEY (source_id, source_version)
        REFERENCES private_inspiration_sources(source_id, source_version)
        ON DELETE CASCADE
);

CREATE TABLE private_inspiration_source_sensitivities (
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    sensitivity_code TEXT NOT NULL CHECK (
        octet_length(sensitivity_code) BETWEEN 1 AND 128
        AND sensitivity_code ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (source_id, source_version, sensitivity_code),
    FOREIGN KEY (source_id, source_version)
        REFERENCES private_inspiration_sources(source_id, source_version)
        ON DELETE CASCADE
);

CREATE TABLE private_inspiration_source_media (
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    media TEXT NOT NULL CHECK (media IN ('text', 'image', 'recap')),
    PRIMARY KEY (source_id, source_version, media),
    FOREIGN KEY (source_id, source_version)
        REFERENCES private_inspiration_sources(source_id, source_version)
        ON DELETE CASCADE
);

CREATE TABLE private_inspiration_consent_grants (
    grant_id TEXT PRIMARY KEY CHECK (
        octet_length(grant_id) BETWEEN 1 AND 128
        AND grant_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    source_digest TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    audience TEXT NOT NULL CHECK (audience = 'private_campaign'),
    media TEXT NOT NULL CHECK (media IN ('text', 'image', 'recap')),
    transformation TEXT NOT NULL CHECK (
        transformation = 'high_fiction_distance_v1'
    ),
    artifact_policy TEXT NOT NULL CHECK (
        artifact_policy IN (
            'delete_derived', 'redact_derived', 'retain_minimal_audit'
        )
    ),
    reviewer_id TEXT NOT NULL CHECK (
        reviewer_id ~ '^operator:[0-9a-f]{32}$'
    ),
    participant_confirmation_digest TEXT NOT NULL CHECK (
        participant_confirmation_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    review_evidence_digest TEXT NOT NULL CHECK (
        review_evidence_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    state TEXT NOT NULL CHECK (state IN ('active', 'expired', 'revoked')),
    granted_at_epoch BIGINT NOT NULL CHECK (granted_at_epoch >= 0),
    expires_at_epoch BIGINT NOT NULL CHECK (expires_at_epoch > granted_at_epoch),
    revoked_at_epoch BIGINT CHECK (revoked_at_epoch >= granted_at_epoch),
    revocation_code TEXT CHECK (
        revocation_code IS NULL
        OR revocation_code IN (
            'participant_revoked', 'reviewer_revoked', 'source_changed',
            'campaign_disabled', 'privacy_request'
        )
    ),
    FOREIGN KEY (source_id, source_version, source_digest)
        REFERENCES private_inspiration_sources(
            source_id, source_version, source_digest
        ),
    FOREIGN KEY (source_id, source_version, media)
        REFERENCES private_inspiration_source_media(
            source_id, source_version, media
        ),
    FOREIGN KEY (source_id, source_version, participant_id)
        REFERENCES private_inspiration_source_participants(
            source_id, source_version, participant_id
        ),
    CHECK (
        (state IN ('active', 'expired')
            AND revoked_at_epoch IS NULL
            AND revocation_code IS NULL)
        OR
        (state = 'revoked' AND revoked_at_epoch IS NOT NULL AND revocation_code IS NOT NULL)
    )
);

CREATE UNIQUE INDEX private_inspiration_one_active_grant_scope_idx
    ON private_inspiration_consent_grants(
        campaign_session_id, source_id, source_version, participant_id,
        audience, media, transformation
    ) WHERE state = 'active';

CREATE TABLE private_inspiration_consent_sensitivities (
    grant_id TEXT NOT NULL
        REFERENCES private_inspiration_consent_grants(grant_id) ON DELETE CASCADE,
    sensitivity_code TEXT NOT NULL CHECK (
        octet_length(sensitivity_code) BETWEEN 1 AND 128
        AND sensitivity_code ~ '^[A-Za-z0-9_.:-]+$'
    ),
    PRIMARY KEY (grant_id, sensitivity_code)
);

CREATE TABLE private_inspiration_vetoes (
    veto_id TEXT PRIMARY KEY CHECK (
        octet_length(veto_id) BETWEEN 1 AND 128
        AND veto_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    participant_id TEXT NOT NULL
        REFERENCES private_inspiration_participants(participant_id),
    scope_kind TEXT NOT NULL CHECK (
        scope_kind IN ('campaign', 'category', 'source_version')
    ),
    category_id TEXT CHECK (
        category_id IS NULL
        OR (
            octet_length(category_id) BETWEEN 1 AND 128
            AND category_id ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    source_id TEXT,
    source_version BIGINT,
    source_digest TEXT,
    state TEXT NOT NULL DEFAULT 'active' CHECK (state = 'active'),
    veto_code TEXT NOT NULL CHECK (
        veto_code IN ('participant_veto', 'safety_veto', 'privacy_veto')
    ),
    created_at_epoch BIGINT NOT NULL CHECK (created_at_epoch >= 0),
    FOREIGN KEY (source_id, source_version, source_digest)
        REFERENCES private_inspiration_sources(
            source_id, source_version, source_digest
        ),
    CHECK (
        (scope_kind = 'campaign'
            AND category_id IS NULL
            AND source_id IS NULL
            AND source_version IS NULL
            AND source_digest IS NULL)
        OR
        (scope_kind = 'category'
            AND category_id IS NOT NULL
            AND source_id IS NULL
            AND source_version IS NULL
            AND source_digest IS NULL)
        OR
        (scope_kind = 'source_version'
            AND category_id IS NULL
            AND source_id IS NOT NULL
            AND source_version IS NOT NULL
            AND source_digest IS NOT NULL)
    )
);

CREATE INDEX private_inspiration_veto_lookup_idx
    ON private_inspiration_vetoes(campaign_session_id, scope_kind, category_id, source_id);

CREATE TABLE private_inspiration_selection_audits (
    selection_id TEXT PRIMARY KEY CHECK (
        octet_length(selection_id) BETWEEN 1 AND 128
        AND selection_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    trigger_window_id TEXT NOT NULL CHECK (
        octet_length(trigger_window_id) BETWEEN 1 AND 128
        AND trigger_window_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    campaign_revision BIGINT NOT NULL CHECK (campaign_revision > 0),
    turn_number BIGINT NOT NULL CHECK (turn_number >= 0),
    audience TEXT NOT NULL CHECK (audience = 'private_campaign'),
    media TEXT NOT NULL CHECK (media IN ('text', 'image', 'recap')),
    seed_reference TEXT NOT NULL CHECK (
        octet_length(seed_reference) BETWEEN 1 AND 128
        AND seed_reference ~ '^[A-Za-z0-9_.:-]+$'
    ),
    eligible_set_digest TEXT NOT NULL CHECK (
        eligible_set_digest ~ '^sha256:[0-9a-f]{64}$'
    ),
    eligible_source_count BIGINT NOT NULL CHECK (eligible_source_count >= 0),
    selected_source_id TEXT,
    selected_source_version BIGINT,
    selected_source_digest TEXT,
    no_selection_reason TEXT CHECK (
        no_selection_reason IS NULL
        OR no_selection_reason IN (
            'deployment_disabled', 'campaign_disabled',
            'safety_incomplete', 'no_eligible_sources'
        )
    ),
    sample_numerator BIGINT,
    sample_denominator BIGINT,
    algorithm TEXT NOT NULL CHECK (algorithm = 'chacha20-v1'),
    cursor_before BIGINT NOT NULL CHECK (cursor_before >= 0),
    cursor_after BIGINT NOT NULL CHECK (cursor_after >= cursor_before),
    created_at_epoch BIGINT NOT NULL CHECK (created_at_epoch >= 0),
    UNIQUE (campaign_session_id, idempotency_key),
    FOREIGN KEY (selected_source_id, selected_source_version, selected_source_digest)
        REFERENCES private_inspiration_sources(
            source_id, source_version, source_digest
        ),
    FOREIGN KEY (selected_source_id, selected_source_version, media)
        REFERENCES private_inspiration_source_media(
            source_id, source_version, media
        ),
    CHECK (
        (selected_source_id IS NOT NULL
            AND selected_source_version IS NOT NULL
            AND selected_source_digest IS NOT NULL
            AND no_selection_reason IS NULL
            AND sample_numerator IS NOT NULL
            AND sample_denominator IS NOT NULL
            AND sample_denominator > 0
            AND sample_numerator >= 0
            AND sample_numerator < sample_denominator
            AND cursor_after > cursor_before)
        OR
        (selected_source_id IS NULL
            AND selected_source_version IS NULL
            AND selected_source_digest IS NULL
            AND no_selection_reason IS NOT NULL
            AND sample_numerator IS NULL
            AND sample_denominator IS NULL
            AND cursor_after = cursor_before)
    )
);

CREATE INDEX private_inspiration_selection_history_idx
    ON private_inspiration_selection_audits(
        campaign_session_id, turn_number DESC, selection_id
    );

CREATE TABLE private_inspiration_source_usage (
    selection_id TEXT PRIMARY KEY
        REFERENCES private_inspiration_selection_audits(selection_id) ON DELETE CASCADE,
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    source_digest TEXT NOT NULL,
    turn_number BIGINT NOT NULL CHECK (turn_number >= 0),
    next_eligible_turn BIGINT NOT NULL CHECK (next_eligible_turn > turn_number),
    created_at_epoch BIGINT NOT NULL CHECK (created_at_epoch >= 0),
    FOREIGN KEY (source_id, source_version, source_digest)
        REFERENCES private_inspiration_sources(
            source_id, source_version, source_digest
        )
);

CREATE INDEX private_inspiration_usage_lookup_idx
    ON private_inspiration_source_usage(
        campaign_session_id, source_id, turn_number DESC
    );

CREATE TABLE private_inspiration_derived_work (
    work_id TEXT PRIMARY KEY CHECK (
        octet_length(work_id) BETWEEN 1 AND 128
        AND work_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    selection_id TEXT NOT NULL
        REFERENCES private_inspiration_selection_audits(selection_id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    source_digest TEXT NOT NULL,
    work_kind TEXT NOT NULL CHECK (work_kind IN ('text', 'image', 'recap')),
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'cancellation_requested', 'completed', 'redacted', 'deleted')
    ),
    artifact_policy TEXT NOT NULL CHECK (
        artifact_policy IN (
            'delete_derived', 'redact_derived', 'retain_minimal_audit'
        )
    ),
    created_at_epoch BIGINT NOT NULL CHECK (created_at_epoch >= 0),
    cancellation_requested_at_epoch BIGINT CHECK (
        cancellation_requested_at_epoch >= created_at_epoch
    ),
    FOREIGN KEY (source_id, source_version, source_digest)
        REFERENCES private_inspiration_sources(
            source_id, source_version, source_digest
        ),
    FOREIGN KEY (source_id, source_version, work_kind)
        REFERENCES private_inspiration_source_media(
            source_id, source_version, media
        ),
    CHECK (
        (state = 'cancellation_requested' AND cancellation_requested_at_epoch IS NOT NULL)
        OR (state <> 'cancellation_requested' AND cancellation_requested_at_epoch IS NULL)
    )
);

CREATE INDEX private_inspiration_pending_work_idx
    ON private_inspiration_derived_work(
        campaign_session_id, source_id, source_version, state
    ) WHERE state = 'pending';

CREATE TABLE private_inspiration_privacy_audits (
    audit_id TEXT PRIMARY KEY CHECK (
        octet_length(audit_id) BETWEEN 1 AND 128
        AND audit_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    campaign_session_id TEXT
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    operation_code TEXT NOT NULL CHECK (
        operation_code IN (
            'settings_changed', 'source_registered', 'source_reviewed',
            'participant_verified', 'participant_revoked', 'consent_granted',
            'consent_revoked', 'veto_applied', 'selection_reserved',
            'derived_work_registered', 'derived_work_cancel_requested'
        )
    ),
    subject_kind TEXT NOT NULL CHECK (
        subject_kind IN (
            'campaign', 'source_version', 'participant', 'consent_grant',
            'veto', 'selection', 'derived_work'
        )
    ),
    subject_id TEXT NOT NULL CHECK (
        octet_length(subject_id) BETWEEN 1 AND 128
        AND subject_id ~ '^[A-Za-z0-9_.:-]+$'
    ),
    secondary_id TEXT CHECK (
        secondary_id IS NULL
        OR (
            octet_length(secondary_id) BETWEEN 1 AND 128
            AND secondary_id ~ '^[A-Za-z0-9_.:-]+$'
        )
    ),
    result_code TEXT NOT NULL CHECK (
        result_code IN ('applied', 'replayed', 'denied', 'cancel_requested')
    ),
    occurred_at_epoch BIGINT NOT NULL CHECK (occurred_at_epoch >= 0)
);

CREATE INDEX private_inspiration_privacy_audit_idx
    ON private_inspiration_privacy_audits(
        campaign_session_id, occurred_at_epoch DESC, audit_id
    );

-- Redacted response receipts contain only versioned DTOs made from opaque IDs,
-- digests, closed codes, booleans, revisions, counters, and epoch timestamps.
CREATE TABLE private_inspiration_command_receipts (
    campaign_session_id TEXT NOT NULL
        REFERENCES campaign_sessions(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL CHECK (
        octet_length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'
    ),
    operation_code TEXT NOT NULL CHECK (
        operation_code IN (
            'settings_change', 'source_register', 'source_review',
            'participant_verify', 'participant_revoke', 'consent_grant',
            'consent_revoke', 'veto_apply', 'derived_work_register'
        )
    ),
    request_fingerprint TEXT NOT NULL CHECK (
        request_fingerprint ~ '^sha256:[0-9a-f]{64}$'
    ),
    response_json TEXT NOT NULL CHECK (
        octet_length(response_json) BETWEEN 2 AND 65536
        AND jsonb_typeof(response_json::jsonb) = 'object'
    ),
    created_at_epoch BIGINT NOT NULL CHECK (created_at_epoch >= 0),
    PRIMARY KEY (campaign_session_id, idempotency_key)
);
