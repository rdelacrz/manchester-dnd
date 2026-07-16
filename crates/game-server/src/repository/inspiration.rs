//! Transactional persistence for consented private inspiration.
//!
//! The repository accepts only the body-free DTOs from `crate::inspiration`.
//! Every mutating operation locks the campaign row first, checks an exact
//! idempotency receipt, and writes its state, receipt, and redacted audit in a
//! single PostgreSQL transaction.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use manchester_dnd_core::{
    CampaignContentPins, Character, RollAlgorithm, SessionDto, SessionEventDto,
    SessionEventPayload, SessionStatus, Sha256Digest, encounter::EncounterStatus,
    hero::HeroCharacter,
};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use crate::{
    error::{PrivateInspirationError, RepositoryError},
    events::{
        DeterministicEventRandom, EventEligibility, EventNoSelectionReason, EventPrompt,
        EventPromptLoader, EventSelectionAudit, RuntimeEventPrompt,
    },
    inspiration::{
        ApplyInspirationVetoCommand, ApplyPresentationPrivacyCommand,
        CampaignInspirationRedactedExportV1, CampaignInspirationSettingsProjection,
        CampaignInspirationTone, ConfigureCampaignInspirationCommand, ConsentGrantProjection,
        ConsentGrantState, DeleteParticipantPrivateDataCommand, DeletionTombstonePurgeOutcome,
        DerivedArtifactPolicy, DerivedWorkProjection, DisableCampaignInspirationCommand,
        DurableNoSelectionReason, GlobalInspirationControlProjection, GrantConsentCommand,
        InspirationAudience, InspirationMedia, InspirationTransformation, InspirationVetoScope,
        OpaqueInspirationId, OpaqueOperatorId, PARTICIPANT_DELETION_TOMBSTONE_SECONDS,
        PRIVATE_INSPIRATION_EXPORT_SCHEMA_VERSION, PRIVATE_INSPIRATION_SCHEMA_VERSION,
        ParticipantDeletionOutcome, ParticipantVerificationMethod,
        ParticipantVerificationProjection, PresentationPrivacyAction, PresentationPrivacyOutcome,
        PrivacyTransitionOutcome, PrivateInspirationSelection,
        PurgeExpiredParticipantDeletionTombstonesCommand, Q11_CONSERVATIVE_POLICY_ID,
        RecordRestrictedDiagnosticAccessCommand, RegisterDerivedWorkCommand,
        RegisterSourceVersionCommand, RequestInspirationSelectionCommand,
        ResolvedInspirationSelectionAuthority, RestrictedDiagnosticAccessKind,
        RestrictedDiagnosticAccessProjection, RestrictedDiagnosticDecision,
        RestrictedDiagnosticPurpose, ReviewSourceVersionCommand, RevokeConsentCommand,
        SafetySetupEvidence, SetCampaignInspirationPauseCommand,
        SetGlobalInspirationControlCommand, SourceReviewState, SourceVersionProjection,
        VerifyParticipantCommand, VetoProjection, fingerprint, internal_id, invalid,
        to_i64 as inspiration_to_i64, to_u64,
    },
};

use super::{PostgresRepository, presentations::PRIVATE_INSPIRATION_REDACTION_BODY};

const SETTINGS_COLUMNS: &str =
    "campaign_session_id, schema_version, revision, enabled, generation_paused, safety_setup_complete,
     adults_only, fictional_distance, audience, media, q11_policy_id, tone, updated_at_epoch,
     (SELECT COUNT(*) FROM campaign_inspiration_lines AS line
       WHERE line.campaign_session_id = campaign_inspiration_settings.campaign_session_id) AS line_count,
     (SELECT COUNT(*) FROM campaign_inspiration_veils AS veil
       WHERE veil.campaign_session_id = campaign_inspiration_settings.campaign_session_id) AS veil_count,
     (SELECT COUNT(*) FROM campaign_inspiration_excluded_topics AS topic
       WHERE topic.campaign_session_id = campaign_inspiration_settings.campaign_session_id) AS excluded_topic_count,
     (SELECT COUNT(*) FROM campaign_inspiration_excluded_participants AS participant
       WHERE participant.campaign_session_id = campaign_inspiration_settings.campaign_session_id) AS excluded_participant_count";

#[derive(Debug)]
struct LockedCampaign {
    session: SessionDto,
    revision: u64,
}

#[derive(Debug)]
struct StoredSource {
    projection: SourceVersionProjection,
    participants: BTreeSet<String>,
    sensitivities: BTreeSet<String>,
    theme_pack_ids: BTreeSet<String>,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VetoReceipt {
    veto: VetoProjection,
    transition: PrivacyTransitionOutcome,
}

impl PostgresRepository {
    /// Loads only the minimized, integrity-bound runtime projections produced
    /// by the offline source-review process. The ordinary game/image process
    /// never needs the protected Markdown root or its decryption key.
    pub(crate) async fn load_private_inspiration_runtime_prompts(
        &self,
    ) -> Result<Vec<EventPrompt>, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let rows = sqlx::query(
            "SELECT runtime.source_id, runtime.source_version,
                    runtime.source_digest, runtime.schema_version,
                    runtime.selection_weight_nanounits, runtime.minimum_level,
                    runtime.maximum_level, runtime.cooldown_turns,
                    runtime.enabled, runtime.projection_digest
             FROM private_inspiration_runtime_prompts AS runtime
             ORDER BY runtime.source_id, runtime.source_version",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;
        let mut prompts = Vec::with_capacity(rows.len());
        for row in rows {
            let source_id: String = row.try_get("source_id").map_err(db)?;
            let source_version = to_u64(row.try_get("source_version").map_err(db)?)?;
            let source_digest = stored_digest(row.try_get("source_digest").map_err(db)?)?;
            let schema_version = u16::try_from(to_u64(row.try_get("schema_version").map_err(db)?)?)
                .map_err(|_| invalid("runtime_prompt_schema_range"))?;
            let minimum_level = u8::try_from(row.try_get::<i16, _>("minimum_level").map_err(db)?)
                .map_err(|_| invalid("runtime_prompt_minimum_level"))?;
            let maximum_level = row
                .try_get::<Option<i16>, _>("maximum_level")
                .map_err(db)?
                .map(u8::try_from)
                .transpose()
                .map_err(|_| invalid("runtime_prompt_maximum_level"))?;
            let neutral_facts = sqlx::query_scalar::<_, String>(
                "SELECT neutral_fact FROM private_inspiration_runtime_facts
                 WHERE source_id = $1 AND source_version = $2
                 ORDER BY fact_index",
            )
            .bind(&source_id)
            .bind(inspiration_to_i64(source_version)?)
            .fetch_all(&mut *transaction)
            .await
            .map_err(db)?;
            let participant_aliases = sqlx::query_scalar::<_, String>(
                "SELECT participant_id FROM private_inspiration_source_participants
                 WHERE source_id = $1 AND source_version = $2
                 ORDER BY participant_id",
            )
            .bind(&source_id)
            .bind(inspiration_to_i64(source_version)?)
            .fetch_all(&mut *transaction)
            .await
            .map_err(db)?;
            let sensitivity_tags = sqlx::query_scalar::<_, String>(
                "SELECT sensitivity_code FROM private_inspiration_source_sensitivities
                 WHERE source_id = $1 AND source_version = $2
                 ORDER BY sensitivity_code",
            )
            .bind(&source_id)
            .bind(inspiration_to_i64(source_version)?)
            .fetch_all(&mut *transaction)
            .await
            .map_err(db)?;
            let projection = RuntimeEventPrompt {
                schema_version,
                selection_weight_nanounits: to_u64(
                    row.try_get("selection_weight_nanounits").map_err(db)?,
                )?,
                minimum_level,
                maximum_level,
                cooldown_turns: to_u64(row.try_get("cooldown_turns").map_err(db)?)?,
                enabled: row.try_get("enabled").map_err(db)?,
                neutral_facts,
            };
            let stored_projection_digest =
                stored_digest(row.try_get("projection_digest").map_err(db)?)?;
            if fingerprint(&projection)? != stored_projection_digest {
                return Err(invalid("runtime_prompt_projection_digest"));
            }
            prompts.push(
                EventPrompt::from_runtime_projection(
                    &source_id,
                    source_digest,
                    participant_aliases,
                    sensitivity_tags,
                    projection,
                )
                .map_err(|_| invalid("stored_runtime_prompt"))?,
            );
        }
        transaction.commit().await.map_err(db)?;
        Ok(prompts)
    }

    pub(crate) async fn load_global_inspiration_control(
        &self,
    ) -> Result<GlobalInspirationControlProjection, PrivateInspirationError> {
        let row = sqlx::query(
            "SELECT schema_version, revision, generation_disabled, updated_at_epoch
             FROM private_inspiration_global_control WHERE singleton",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        global_control_from_row(row)
    }

    pub(crate) async fn record_private_inspiration_restricted_access(
        &self,
        command: &RecordRestrictedDiagnosticAccessCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<RestrictedDiagnosticAccessProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        if let Some(row) = sqlx::query(
            "SELECT audit_id, schema_version, request_fingerprint,
                    campaign_session_id, operator_id, access_kind, purpose_code,
                    subject_id, evidence_digest, result_code, occurred_at_epoch
             FROM private_inspiration_restricted_access_audits
             WHERE idempotency_key = $1",
        )
        .bind(command.idempotency_key.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        {
            let stored_fingerprint =
                stored_digest(row.try_get("request_fingerprint").map_err(db)?)?;
            if &stored_fingerprint != request_fingerprint {
                return Err(PrivateInspirationError::ScopeDenied);
            }
            let projection = restricted_access_projection_from_row(row)?;
            transaction.commit().await.map_err(db)?;
            return Ok(projection);
        }
        let audit_id = internal_id("restricted-access")?;
        sqlx::query(
            "INSERT INTO private_inspiration_restricted_access_audits
             (audit_id, schema_version, idempotency_key, request_fingerprint,
              campaign_session_id, operator_id, access_kind, purpose_code,
              subject_id, evidence_digest, result_code, occurred_at_epoch)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(audit_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.idempotency_key.as_str())
        .bind(request_fingerprint.as_str())
        .bind(
            command
                .campaign_session_id
                .as_ref()
                .map(OpaqueInspirationId::as_str),
        )
        .bind(command.operator_id.as_str())
        .bind(command.access_kind.as_str())
        .bind(command.purpose.as_str())
        .bind(command.subject_id.as_str())
        .bind(command.evidence_digest.as_str())
        .bind(command.decision.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        insert_privacy_audit(
            &mut transaction,
            command
                .campaign_session_id
                .as_ref()
                .map(OpaqueInspirationId::as_str),
            "restricted_diagnostic_access",
            "restricted_diagnostic",
            command.subject_id.as_str(),
            Some(command.access_kind.as_str()),
            if command.decision == RestrictedDiagnosticDecision::Allowed {
                "applied"
            } else {
                "denied"
            },
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(RestrictedDiagnosticAccessProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            audit_id,
            campaign_session_id: command.campaign_session_id.clone(),
            operator_id: command.operator_id.clone(),
            access_kind: command.access_kind,
            purpose: command.purpose,
            subject_id: command.subject_id.clone(),
            evidence_digest: command.evidence_digest.clone(),
            decision: command.decision,
            occurred_at_epoch: now,
        })
    }

    pub(crate) async fn set_global_inspiration_control(
        &self,
        command: &SetGlobalInspirationControlCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<GlobalInspirationControlProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let current = sqlx::query(
            "SELECT schema_version, revision, generation_disabled, updated_at_epoch
             FROM private_inspiration_global_control WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)
        .and_then(global_control_from_row)?;
        if let Some(row) = sqlx::query(
            "SELECT request_fingerprint, response_json
             FROM private_inspiration_global_command_receipts
             WHERE idempotency_key = $1",
        )
        .bind(command.idempotency_key.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        {
            let stored_fingerprint: String = row.try_get("request_fingerprint").map_err(db)?;
            if stored_fingerprint != request_fingerprint.as_str() {
                return Err(PrivateInspirationError::IdempotencyConflict);
            }
            let body: String = row.try_get("response_json").map_err(db)?;
            return serde_json::from_str(&body)
                .map_err(|_| invalid("stored_global_control_receipt"));
        }
        if current.revision != command.expected_revision {
            return Err(PrivateInspirationError::RevisionConflict {
                expected: command.expected_revision,
                current: current.revision,
            });
        }
        let projection = GlobalInspirationControlProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            revision: current
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("global_control_revision_overflow"))?,
            generation_disabled: command.generation_disabled,
            updated_at_epoch: now,
        };
        sqlx::query(
            "UPDATE private_inspiration_global_control
             SET revision = $1, generation_disabled = $2, operator_id = $3,
                 evidence_digest = $4, updated_at_epoch = $5
             WHERE singleton",
        )
        .bind(inspiration_to_i64(projection.revision)?)
        .bind(projection.generation_disabled)
        .bind(command.operator_id.as_str())
        .bind(command.evidence_digest.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if command.generation_disabled {
            quarantine_all_private_inspiration_work(&mut transaction, now).await?;
        }
        insert_privacy_audit(
            &mut transaction,
            None,
            "global_kill_switch",
            "campaign",
            "global:private-inspiration",
            None,
            "applied",
            now,
        )
        .await?;
        let response_json =
            serde_json::to_string(&projection).map_err(PrivateInspirationError::Serialization)?;
        sqlx::query(
            "INSERT INTO private_inspiration_global_command_receipts
             (idempotency_key, request_fingerprint, response_json,
              created_at_epoch) VALUES ($1, $2, $3, $4)",
        )
        .bind(command.idempotency_key.as_str())
        .bind(request_fingerprint.as_str())
        .bind(response_json)
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }

    pub(crate) async fn purge_expired_private_inspiration_deletion_tombstones(
        &self,
        command: &PurgeExpiredParticipantDeletionTombstonesCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<DeletionTombstonePurgeOutcome, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        sqlx::query(
            "SELECT revision FROM private_inspiration_global_control
             WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        if let Some(row) = sqlx::query(
            "SELECT request_fingerprint, response_json
             FROM private_inspiration_global_command_receipts
             WHERE idempotency_key = $1",
        )
        .bind(command.idempotency_key.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        {
            let stored_fingerprint: String = row.try_get("request_fingerprint").map_err(db)?;
            if stored_fingerprint != request_fingerprint.as_str() {
                return Err(PrivateInspirationError::IdempotencyConflict);
            }
            let body: String = row.try_get("response_json").map_err(db)?;
            return serde_json::from_str(&body)
                .map_err(|_| invalid("stored_tombstone_purge_receipt"));
        }
        let rows = sqlx::query(
            "DELETE FROM private_inspiration_deletion_tombstones
             WHERE delete_after_epoch <= $1
             RETURNING participant_id",
        )
        .bind(inspiration_to_i64(command.delete_after_epoch_inclusive)?)
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;
        for row in &rows {
            let participant_id: String = row.try_get("participant_id").map_err(db)?;
            insert_privacy_audit(
                &mut transaction,
                None,
                "deletion_tombstone_expired",
                "participant",
                &participant_id,
                Some(command.operator_id.as_str()),
                "applied",
                now,
            )
            .await?;
        }
        let outcome = DeletionTombstonePurgeOutcome {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            delete_after_epoch_inclusive: command.delete_after_epoch_inclusive,
            purged_count: u32::try_from(rows.len())
                .map_err(|_| invalid("purged_tombstone_count_range"))?,
            applied_at_epoch: now,
        };
        let response_json =
            serde_json::to_string(&outcome).map_err(PrivateInspirationError::Serialization)?;
        sqlx::query(
            "INSERT INTO private_inspiration_global_command_receipts
             (idempotency_key, request_fingerprint, response_json,
              created_at_epoch) VALUES ($1, $2, $3, $4)",
        )
        .bind(command.idempotency_key.as_str())
        .bind(request_fingerprint.as_str())
        .bind(response_json)
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(outcome)
    }

    pub(crate) async fn load_private_inspiration_campaign_settings(
        &self,
        campaign_session_id: &OpaqueInspirationId,
    ) -> Result<Option<CampaignInspirationSettingsProjection>, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, campaign_session_id.as_str()).await?;
        let settings =
            load_settings_for_update(&mut transaction, campaign_session_id.as_str()).await?;
        transaction.commit().await.map_err(db)?;
        Ok(settings)
    }

    pub(crate) async fn set_private_inspiration_campaign_pause(
        &self,
        command: &SetCampaignInspirationPauseCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "settings_pause",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let mut settings =
            load_settings_for_update(&mut transaction, command.campaign_session_id.as_str())
                .await?
                .ok_or(PrivateInspirationError::NotFound)?;
        if settings.revision != command.expected_revision {
            return Err(PrivateInspirationError::RevisionConflict {
                expected: command.expected_revision,
                current: settings.revision,
            });
        }
        settings.revision = settings
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("settings_revision_overflow"))?;
        settings.generation_paused = command.paused;
        settings.updated_at_epoch = now;
        sqlx::query(
            "UPDATE campaign_inspiration_settings
             SET revision = $2, generation_paused = $3, updated_at_epoch = $4
             WHERE campaign_session_id = $1",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(inspiration_to_i64(settings.revision)?)
        .bind(command.paused)
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "settings_pause",
            request_fingerprint,
            &settings,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "settings_changed",
            "campaign",
            command.campaign_session_id.as_str(),
            None,
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(settings)
    }

    pub(crate) async fn disable_private_inspiration_campaign(
        &self,
        command: &DisableCampaignInspirationCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "settings_disable",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let mut settings =
            load_settings_for_update(&mut transaction, command.campaign_session_id.as_str())
                .await?
                .ok_or(PrivateInspirationError::NotFound)?;
        if settings.revision != command.expected_revision {
            return Err(PrivateInspirationError::RevisionConflict {
                expected: command.expected_revision,
                current: settings.revision,
            });
        }
        settings.revision = settings
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("settings_revision_overflow"))?;
        settings.enabled = false;
        settings.generation_paused = false;
        settings.updated_at_epoch = now;
        sqlx::query(
            "UPDATE campaign_inspiration_settings
             SET revision = $2, enabled = FALSE, generation_paused = FALSE,
                 updated_at_epoch = $3
             WHERE campaign_session_id = $1",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(inspiration_to_i64(settings.revision)?)
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "UPDATE private_inspiration_consent_grants
             SET state = 'revoked', revoked_at_epoch = $2,
                 revocation_code = 'campaign_disabled'
             WHERE campaign_session_id = $1 AND state = 'active'",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        cancel_campaign_pending_work(&mut transaction, command.campaign_session_id.as_str(), now)
            .await?;
        apply_campaign_completed_work_policy(
            &mut transaction,
            command.campaign_session_id.as_str(),
            now,
        )
        .await?;
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "settings_disable",
            request_fingerprint,
            &settings,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "settings_changed",
            "campaign",
            command.campaign_session_id.as_str(),
            None,
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(settings)
    }

    pub(crate) async fn apply_private_inspiration_presentation_control(
        &self,
        command: &ApplyPresentationPrivacyCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<PresentationPrivacyOutcome, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "presentation_control",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let row = sqlx::query(
            "SELECT presentation.privacy_state, work.work_id, work.state,
                    work.source_id, work.source_version, work.source_digest,
                    source.category_id
             FROM generated_text_presentations AS presentation
             JOIN private_inspiration_derived_work AS work
               ON work.work_id = presentation.private_inspiration_work_id
             JOIN private_inspiration_sources AS source
               ON source.source_id = work.source_id
              AND source.source_version = work.source_version
             WHERE presentation.id = $1
               AND presentation.campaign_session_id = $2
             FOR UPDATE OF presentation, work",
        )
        .bind(command.presentation_id.as_str())
        .bind(command.campaign_session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(PrivateInspirationError::NotFound)?;
        let work_id = OpaqueInspirationId::new(row.try_get::<String, _>("work_id").map_err(db)?)?;
        let work_state: String = row.try_get("state").map_err(db)?;
        if !matches!(work_state.as_str(), "completed" | "redacted") {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        let source_id =
            OpaqueInspirationId::new(row.try_get::<String, _>("source_id").map_err(db)?)?;
        let source_version = to_u64(row.try_get("source_version").map_err(db)?)?;
        let source_digest = stored_digest(row.try_get("source_digest").map_err(db)?)?;
        let category_id =
            OpaqueInspirationId::new(row.try_get::<String, _>("category_id").map_err(db)?)?;

        let mut settings_revision = None;
        match command.action {
            PresentationPrivacyAction::Veil | PresentationPrivacyAction::Report => {
                sqlx::query(
                    "UPDATE generated_text_presentations
                     SET body = $2, privacy_state = 'redacted',
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = $1",
                )
                .bind(command.presentation_id.as_str())
                .bind(PRIVATE_INSPIRATION_REDACTION_BODY)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
                sqlx::query(
                    "UPDATE private_inspiration_derived_work
                     SET state = 'redacted'
                     WHERE work_id = $1 AND state IN ('completed', 'redacted')",
                )
                .bind(work_id.as_str())
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
                let operation = if command.action == PresentationPrivacyAction::Report {
                    let revision: i64 = sqlx::query_scalar(
                        "UPDATE campaign_inspiration_settings
                         SET generation_paused = TRUE, revision = revision + 1,
                             updated_at_epoch = $2
                         WHERE campaign_session_id = $1
                         RETURNING revision",
                    )
                    .bind(command.campaign_session_id.as_str())
                    .bind(inspiration_to_i64(now)?)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(db)?;
                    settings_revision = Some(to_u64(revision)?);
                    "privacy_reported"
                } else {
                    "presentation_veiled"
                };
                insert_privacy_audit(
                    &mut transaction,
                    Some(command.campaign_session_id.as_str()),
                    operation,
                    "derived_work",
                    work_id.as_str(),
                    Some(command.presentation_id.as_str()),
                    "applied",
                    now,
                )
                .await?;
            }
            PresentationPrivacyAction::VetoSource | PresentationPrivacyAction::VetoCategory => {
                let scope = if command.action == PresentationPrivacyAction::VetoSource {
                    InspirationVetoScope::SourceVersion {
                        source_id,
                        source_version,
                        source_digest,
                    }
                } else {
                    InspirationVetoScope::Category { category_id }
                };
                insert_owner_veto(
                    &mut transaction,
                    command.campaign_session_id.as_str(),
                    &scope,
                    command.presentation_id.as_str(),
                    now,
                )
                .await?;
                cancel_vetoed_pending_work(
                    &mut transaction,
                    command.campaign_session_id.as_str(),
                    &scope,
                    now,
                )
                .await?;
                apply_vetoed_completed_work_policy(
                    &mut transaction,
                    command.campaign_session_id.as_str(),
                    &scope,
                    now,
                )
                .await?;
            }
        }
        let outcome = PresentationPrivacyOutcome {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            presentation_id: command.presentation_id.clone(),
            action: command.action,
            presentation_hidden: true,
            settings_revision,
            effective_at_epoch: now,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "presentation_control",
            request_fingerprint,
            &outcome,
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(outcome)
    }

    pub(crate) async fn configure_private_inspiration_campaign(
        &self,
        command: &ConfigureCampaignInspirationCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "settings_change",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }

        let current =
            load_settings_for_update(&mut transaction, command.campaign_session_id.as_str())
                .await?;
        let current_revision = current.as_ref().map_or(0, |settings| settings.revision);
        if current_revision != command.expected_revision {
            return Err(PrivateInspirationError::RevisionConflict {
                expected: command.expected_revision,
                current: current_revision,
            });
        }
        let revision = current_revision
            .checked_add(1)
            .ok_or_else(|| invalid("settings_revision_overflow"))?;
        let safety_setup_complete = command.safety_setup.is_some();
        let tone = command
            .safety_setup
            .as_ref()
            .map_or(CampaignInspirationTone::GothicAdventure, |setup| setup.tone);
        let (evidence, reviewer, reviewed_at) =
            command
                .safety_setup
                .as_ref()
                .map_or((None, None, None), |setup| {
                    (
                        Some(setup.evidence_digest.as_str()),
                        Some(setup.reviewer_id.as_str()),
                        Some(now),
                    )
                });

        sqlx::query(
            "INSERT INTO campaign_inspiration_settings
             (campaign_session_id, schema_version, revision, enabled,
              generation_paused, safety_setup_complete, safety_setup_evidence_digest,
              safety_reviewer_id, safety_reviewed_at_epoch, tone, updated_at_epoch)
             VALUES ($1, $2, $3, $4, FALSE, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (campaign_session_id) DO UPDATE SET
               schema_version = EXCLUDED.schema_version,
               revision = EXCLUDED.revision,
               enabled = EXCLUDED.enabled,
               generation_paused = FALSE,
               safety_setup_complete = EXCLUDED.safety_setup_complete,
               safety_setup_evidence_digest = EXCLUDED.safety_setup_evidence_digest,
               safety_reviewer_id = EXCLUDED.safety_reviewer_id,
               safety_reviewed_at_epoch = EXCLUDED.safety_reviewed_at_epoch,
               tone = EXCLUDED.tone,
               updated_at_epoch = EXCLUDED.updated_at_epoch",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(inspiration_to_i64(revision)?)
        .bind(command.enabled)
        .bind(safety_setup_complete)
        .bind(evidence)
        .bind(reviewer)
        .bind(reviewed_at.map(inspiration_to_i64).transpose()?)
        .bind(tone.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        replace_campaign_safety_setup(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.safety_setup.as_ref(),
        )
        .await?;

        if !command.enabled {
            sqlx::query(
                "UPDATE private_inspiration_consent_grants
                 SET state = 'revoked', revoked_at_epoch = $2,
                     revocation_code = 'campaign_disabled'
                 WHERE campaign_session_id = $1 AND state = 'active'",
            )
            .bind(command.campaign_session_id.as_str())
            .bind(inspiration_to_i64(now)?)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
            cancel_campaign_pending_work(
                &mut transaction,
                command.campaign_session_id.as_str(),
                now,
            )
            .await?;
            apply_campaign_completed_work_policy(
                &mut transaction,
                command.campaign_session_id.as_str(),
                now,
            )
            .await?;
        }

        let projection = CampaignInspirationSettingsProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: command.campaign_session_id.clone(),
            revision,
            enabled: command.enabled,
            generation_paused: false,
            safety_setup_complete,
            adults_only: true,
            fictional_distance_locked_high: true,
            tone,
            line_count: command
                .safety_setup
                .as_ref()
                .map_or(0, |setup| setup.line_codes.len() as u32),
            veil_count: command
                .safety_setup
                .as_ref()
                .map_or(0, |setup| setup.veil_codes.len() as u32),
            excluded_topic_count: command
                .safety_setup
                .as_ref()
                .map_or(0, |setup| setup.excluded_topic_codes.len() as u32),
            excluded_participant_count: command
                .safety_setup
                .as_ref()
                .map_or(0, |setup| setup.excluded_participant_ids.len() as u32),
            audience: InspirationAudience::PrivateCampaign,
            media: InspirationMedia::Text,
            q11_policy_id: Q11_CONSERVATIVE_POLICY_ID.to_owned(),
            updated_at_epoch: now,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "settings_change",
            request_fingerprint,
            &projection,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "settings_changed",
            "campaign",
            command.campaign_session_id.as_str(),
            None,
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }

    pub(crate) async fn verify_private_inspiration_participant(
        &self,
        command: &VerifyParticipantCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<ParticipantVerificationProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "participant_verify",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }

        sqlx::query(
            "DELETE FROM private_inspiration_deletion_tombstones
             WHERE participant_id = $1 AND delete_after_epoch <= $2",
        )
        .bind(command.participant_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let deletion_pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM private_inspiration_deletion_tombstones
               WHERE participant_id = $1
             )",
        )
        .bind(command.participant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        if deletion_pending {
            return Err(PrivateInspirationError::ScopeDenied);
        }

        sqlx::query(
            "INSERT INTO private_inspiration_participants
             (participant_id, schema_version, verification_state,
              verification_method, verification_evidence_digest, verifier_id,
              verified_at_epoch, revoked_at_epoch)
             VALUES ($1, $2, 'verified', $3, $4, $5, $6, NULL)
             ON CONFLICT (participant_id) DO UPDATE SET
               schema_version = EXCLUDED.schema_version,
               verification_state = 'verified',
               verification_method = EXCLUDED.verification_method,
               verification_evidence_digest = EXCLUDED.verification_evidence_digest,
               verifier_id = EXCLUDED.verifier_id,
               verified_at_epoch = EXCLUDED.verified_at_epoch,
               revoked_at_epoch = NULL",
        )
        .bind(command.participant_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.method.as_str())
        .bind(command.evidence_digest.as_str())
        .bind(command.verifier_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        let projection = ParticipantVerificationProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            participant_id: command.participant_id.clone(),
            method: command.method,
            verified_at_epoch: now,
            revoked: false,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "participant_verify",
            request_fingerprint,
            &projection,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "participant_verified",
            "participant",
            command.participant_id.as_str(),
            None,
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }

    pub(crate) async fn delete_private_inspiration_participant_data(
        &self,
        command: &DeleteParticipantPrivateDataCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<ParticipantDeletionOutcome, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "participant_delete",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }

        let participant_state = sqlx::query_scalar::<_, String>(
            "SELECT verification_state FROM private_inspiration_participants
             WHERE participant_id = $1 FOR UPDATE",
        )
        .bind(command.participant_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(PrivateInspirationError::NotFound)?;
        if !matches!(participant_state.as_str(), "verified" | "revoked") {
            return Err(invalid("stored_participant_verification_state"));
        }
        let tombstone_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM private_inspiration_deletion_tombstones
               WHERE participant_id = $1
             )",
        )
        .bind(command.participant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        if tombstone_exists {
            return Err(PrivateInspirationError::ScopeDenied);
        }

        let source_rows = sqlx::query(
            "SELECT source.source_id, source.source_version
             FROM private_inspiration_sources AS source
             JOIN private_inspiration_source_participants AS participant
               ON participant.source_id = source.source_id
              AND participant.source_version = source.source_version
             WHERE participant.participant_id = $1
             ORDER BY source.source_id, source.source_version
             FOR UPDATE OF source",
        )
        .bind(command.participant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;

        sqlx::query(
            "UPDATE private_inspiration_participants
             SET verification_state = 'revoked',
                 verification_evidence_digest = $2,
                 verifier_id = $3,
                 revoked_at_epoch = GREATEST(verified_at_epoch, $4)
             WHERE participant_id = $1",
        )
        .bind(command.participant_id.as_str())
        .bind(command.deletion_evidence_digest.as_str())
        .bind(command.operator_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        let revoked_grants = sqlx::query(
            "UPDATE private_inspiration_consent_grants
             SET state = 'revoked',
                 revoked_at_epoch = GREATEST(granted_at_epoch, $2),
                 revocation_code = 'privacy_request'
             WHERE participant_id = $1 AND state IN ('active', 'expired')",
        )
        .bind(command.participant_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?
        .rows_affected();

        for row in &source_rows {
            let source_id: String = row.try_get("source_id").map_err(db)?;
            let source_version: i64 = row.try_get("source_version").map_err(db)?;
            let source_version_text = source_version.to_string();
            sqlx::query(
                "UPDATE private_inspiration_sources
                 SET review_state = 'quarantined', q11_screened = FALSE,
                     review_evidence_digest = $3, reviewer_id = $4,
                     reviewed_at_epoch = $5
                 WHERE source_id = $1 AND source_version = $2",
            )
            .bind(&source_id)
            .bind(source_version)
            .bind(command.deletion_evidence_digest.as_str())
            .bind(command.operator_id.as_str())
            .bind(inspiration_to_i64(now)?)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
            insert_privacy_audit(
                &mut transaction,
                Some(command.campaign_session_id.as_str()),
                "source_quarantined",
                "source_version",
                &source_id,
                Some(&source_version_text),
                "applied",
                now,
            )
            .await?;
        }

        let pending_rows = sqlx::query(
            "UPDATE private_inspiration_derived_work AS work
             SET state = 'cancellation_requested',
                 cancellation_requested_at_epoch = GREATEST(work.created_at_epoch, $2)
             FROM private_inspiration_source_participants AS participant
             WHERE participant.participant_id = $1
               AND participant.source_id = work.source_id
               AND participant.source_version = work.source_version
               AND work.state = 'pending'
             RETURNING work.campaign_session_id, work.work_id",
        )
        .bind(command.participant_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;
        let mut pending_work_cancellation_ids = Vec::with_capacity(pending_rows.len());
        for row in pending_rows {
            let campaign_id: String = row.try_get("campaign_session_id").map_err(db)?;
            let work_id =
                OpaqueInspirationId::new(row.try_get::<String, _>("work_id").map_err(db)?)?;
            insert_privacy_audit(
                &mut transaction,
                Some(&campaign_id),
                "derived_work_cancel_requested",
                "derived_work",
                work_id.as_str(),
                None,
                "cancel_requested",
                now,
            )
            .await?;
            pending_work_cancellation_ids.push(work_id);
        }
        pending_work_cancellation_ids.sort();

        let completed_rows = sqlx::query(
            "SELECT work.campaign_session_id, work.work_id,
                    work.artifact_policy, work.completed_artifact_id
             FROM private_inspiration_derived_work AS work
             JOIN private_inspiration_source_participants AS participant
               ON participant.source_id = work.source_id
              AND participant.source_version = work.source_version
             WHERE participant.participant_id = $1
               AND work.state IN ('completed', 'redacted')
             ORDER BY work.campaign_session_id, work.work_id
             FOR UPDATE OF work",
        )
        .bind(command.participant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;
        let affected_completed_artifact_count = u32::try_from(completed_rows.len())
            .map_err(|_| invalid("completed_artifact_count_range"))?;
        let mut completed_by_campaign = BTreeMap::<String, Vec<PgRow>>::new();
        for row in completed_rows {
            let campaign_id: String = row.try_get("campaign_session_id").map_err(db)?;
            completed_by_campaign
                .entry(campaign_id)
                .or_default()
                .push(row);
        }
        for (campaign_id, rows) in completed_by_campaign {
            apply_completed_work_policy(&mut transaction, &campaign_id, rows, now).await?;
        }

        let tombstone_delete_after_epoch = now
            .checked_add(PARTICIPANT_DELETION_TOMBSTONE_SECONDS)
            .ok_or_else(|| invalid("deletion_tombstone_expiry_overflow"))?;
        sqlx::query(
            "INSERT INTO private_inspiration_deletion_tombstones
             (participant_id, schema_version, requested_by_operator_id,
              deletion_evidence_digest, requested_at_epoch, delete_after_epoch)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(command.participant_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.operator_id.as_str())
        .bind(command.deletion_evidence_digest.as_str())
        .bind(inspiration_to_i64(now)?)
        .bind(inspiration_to_i64(tombstone_delete_after_epoch)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        let outcome = ParticipantDeletionOutcome {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            participant_id: command.participant_id.clone(),
            revoked_grant_count: u32::try_from(revoked_grants)
                .map_err(|_| invalid("revoked_grant_count_range"))?,
            quarantined_source_count: u32::try_from(source_rows.len())
                .map_err(|_| invalid("quarantined_source_count_range"))?,
            pending_work_cancellation_ids,
            affected_completed_artifact_count,
            effective_at_epoch: now,
            tombstone_delete_after_epoch,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "participant_delete",
            request_fingerprint,
            &outcome,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "participant_deletion_requested",
            "participant",
            command.participant_id.as_str(),
            None,
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(outcome)
    }

    pub(crate) async fn register_private_inspiration_source(
        &self,
        command: &RegisterSourceVersionCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<SourceVersionProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "source_register",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        for participant in &command.participant_ids {
            ensure_verified_participant(&mut transaction, participant.as_str()).await?;
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM private_inspiration_sources
               WHERE source_id = $1 AND source_version = $2
             )",
        )
        .bind(command.source_id.as_str())
        .bind(inspiration_to_i64(command.source_version)?)
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        if exists {
            return Err(PrivateInspirationError::ScopeDenied);
        }

        sqlx::query(
            "INSERT INTO private_inspiration_sources
             (source_id, source_version, source_digest, schema_version,
              category_id, owner_participant_id, review_state, q11_screened,
              audience, transformation, provenance_digest, expires_at_epoch,
              registered_at_epoch)
             VALUES ($1, $2, $3, $4, $5, $6, 'pending', FALSE,
                     'private_campaign', 'high_fiction_distance_v1', $7, $8, $9)",
        )
        .bind(command.source_id.as_str())
        .bind(inspiration_to_i64(command.source_version)?)
        .bind(command.source_digest.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.category_id.as_str())
        .bind(command.owner_participant_id.as_str())
        .bind(command.provenance_digest.as_str())
        .bind(
            command
                .expires_at_epoch
                .map(inspiration_to_i64)
                .transpose()?,
        )
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let runtime_projection_digest = fingerprint(&command.runtime_prompt)?;
        sqlx::query(
            "INSERT INTO private_inspiration_runtime_prompts
             (source_id, source_version, source_digest, schema_version,
              selection_weight_nanounits, minimum_level, maximum_level,
              cooldown_turns, enabled, projection_digest)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(command.source_id.as_str())
        .bind(inspiration_to_i64(command.source_version)?)
        .bind(command.source_digest.as_str())
        .bind(i64::from(command.runtime_prompt.schema_version))
        .bind(inspiration_to_i64(
            command.runtime_prompt.selection_weight_nanounits,
        )?)
        .bind(i16::from(command.runtime_prompt.minimum_level))
        .bind(command.runtime_prompt.maximum_level.map(i16::from))
        .bind(inspiration_to_i64(command.runtime_prompt.cooldown_turns)?)
        .bind(command.runtime_prompt.enabled)
        .bind(runtime_projection_digest.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        for (index, fact) in command.runtime_prompt.neutral_facts.iter().enumerate() {
            sqlx::query(
                "INSERT INTO private_inspiration_runtime_facts
                 (source_id, source_version, fact_index, neutral_fact)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(command.source_id.as_str())
            .bind(inspiration_to_i64(command.source_version)?)
            .bind(i16::try_from(index + 1).map_err(|_| invalid("runtime_fact_index"))?)
            .bind(fact)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for participant in &command.participant_ids {
            sqlx::query(
                "INSERT INTO private_inspiration_source_participants
                 (source_id, source_version, participant_id) VALUES ($1, $2, $3)",
            )
            .bind(command.source_id.as_str())
            .bind(inspiration_to_i64(command.source_version)?)
            .bind(participant.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for sensitivity in &command.sensitivity_codes {
            sqlx::query(
                "INSERT INTO private_inspiration_source_sensitivities
                 (source_id, source_version, sensitivity_code) VALUES ($1, $2, $3)",
            )
            .bind(command.source_id.as_str())
            .bind(inspiration_to_i64(command.source_version)?)
            .bind(sensitivity.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for media in &command.eligible_media {
            sqlx::query(
                "INSERT INTO private_inspiration_source_media
                 (source_id, source_version, media) VALUES ($1, $2, $3)",
            )
            .bind(command.source_id.as_str())
            .bind(inspiration_to_i64(command.source_version)?)
            .bind(media.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for theme_pack_id in &command.eligible_theme_pack_ids {
            sqlx::query(
                "INSERT INTO private_inspiration_source_themes
                 (source_id, source_version, theme_pack_id) VALUES ($1, $2, $3)",
            )
            .bind(command.source_id.as_str())
            .bind(inspiration_to_i64(command.source_version)?)
            .bind(theme_pack_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }

        let projection = SourceVersionProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            source_id: command.source_id.clone(),
            source_version: command.source_version,
            source_digest: command.source_digest.clone(),
            category_id: command.category_id.clone(),
            review_state: SourceReviewState::Pending,
            q11_screened: false,
            participant_count: u32::try_from(command.participant_ids.len())
                .map_err(|_| invalid("participant_count_range"))?,
            sensitivity_count: u32::try_from(command.sensitivity_codes.len())
                .map_err(|_| invalid("sensitivity_count_range"))?,
            eligible_media: command.eligible_media.clone(),
            eligible_theme_pack_ids: command.eligible_theme_pack_ids.clone(),
            expires_at_epoch: command.expires_at_epoch,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "source_register",
            request_fingerprint,
            &projection,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "source_registered",
            "source_version",
            command.source_id.as_str(),
            Some(&command.source_version.to_string()),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }

    pub(crate) async fn review_private_inspiration_source(
        &self,
        command: &ReviewSourceVersionCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<SourceVersionProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "source_review",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let source = load_source_for_update(
            &mut transaction,
            command.source_id.as_str(),
            command.source_version,
        )
        .await?
        .ok_or(PrivateInspirationError::NotFound)?;
        if source.projection.source_digest != command.source_digest
            || source.projection.review_state != SourceReviewState::Pending
        {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        sqlx::query(
            "UPDATE private_inspiration_sources
             SET review_state = $3, q11_screened = $4,
                 review_evidence_digest = $5, reviewer_id = $6,
                 reviewed_at_epoch = $7
             WHERE source_id = $1 AND source_version = $2",
        )
        .bind(command.source_id.as_str())
        .bind(inspiration_to_i64(command.source_version)?)
        .bind(command.decision.as_str())
        .bind(command.q11_screened)
        .bind(command.review_evidence_digest.as_str())
        .bind(command.reviewer_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        let mut projection = source.projection;
        projection.review_state = command.decision;
        projection.q11_screened = command.q11_screened;
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "source_review",
            request_fingerprint,
            &projection,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "source_reviewed",
            "source_version",
            command.source_id.as_str(),
            Some(&command.source_version.to_string()),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }

    pub(crate) async fn abandon_private_inspiration_derived_work(
        &self,
        campaign_session_id: &OpaqueInspirationId,
        work_id: &OpaqueInspirationId,
        now: u64,
    ) -> Result<(), PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, campaign_session_id.as_str()).await?;
        let state: String = sqlx::query_scalar(
            "SELECT state FROM private_inspiration_derived_work
             WHERE work_id = $1 AND campaign_session_id = $2 FOR UPDATE",
        )
        .bind(work_id.as_str())
        .bind(campaign_session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(PrivateInspirationError::NotFound)?;
        match state.as_str() {
            "pending" => {
                sqlx::query(
                    "UPDATE private_inspiration_derived_work
                     SET state = 'cancellation_requested',
                         cancellation_requested_at_epoch = $2
                     WHERE work_id = $1",
                )
                .bind(work_id.as_str())
                .bind(inspiration_to_i64(now)?)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
                insert_privacy_audit(
                    &mut transaction,
                    Some(campaign_session_id.as_str()),
                    "derived_work_cancel_requested",
                    "derived_work",
                    work_id.as_str(),
                    None,
                    "cancel_requested",
                    now,
                )
                .await?;
            }
            "cancellation_requested" | "redacted" | "deleted" => {}
            "completed" => return Err(PrivateInspirationError::ScopeDenied),
            _ => return Err(invalid("stored_derived_work_state")),
        }
        transaction.commit().await.map_err(db)
    }
}

impl PostgresRepository {
    pub(crate) async fn reserve_private_inspiration_selection(
        &self,
        deployment_enabled: bool,
        command: &RequestInspirationSelectionCommand,
        authority: &ResolvedInspirationSelectionAuthority,
        prompts: &[EventPrompt],
        now: u64,
    ) -> Result<PrivateInspirationSelection, PrivateInspirationError> {
        let request_fingerprint = fingerprint(command)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let campaign =
            lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_selection_replay(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            &request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        if campaign.revision != command.expected_campaign_revision {
            return Err(PrivateInspirationError::RevisionConflict {
                expected: command.expected_campaign_revision,
                current: campaign.revision,
            });
        }
        let settings =
            load_settings_for_update(&mut transaction, command.campaign_session_id.as_str())
                .await?
                .ok_or(PrivateInspirationError::NotFound)?;
        if settings.revision != command.expected_settings_revision {
            return Err(PrivateInspirationError::RevisionConflict {
                expected: command.expected_settings_revision,
                current: settings.revision,
            });
        }
        let global_generation_disabled: bool = sqlx::query_scalar(
            "SELECT generation_disabled FROM private_inspiration_global_control
             WHERE singleton FOR SHARE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let (turn_number, trigger_window_id) =
            trusted_trigger_window(&mut transaction, &campaign).await?;
        let cursor: u64 = to_u64(
            sqlx::query_scalar::<_, i64>(
                "SELECT rng_cursor FROM campaign_inspiration_settings
                 WHERE campaign_session_id = $1",
            )
            .bind(command.campaign_session_id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?,
        )?;

        let early_reason = if !deployment_enabled {
            Some(DurableNoSelectionReason::DeploymentDisabled)
        } else if global_generation_disabled {
            Some(DurableNoSelectionReason::GlobalKillSwitch)
        } else if !settings.enabled {
            Some(DurableNoSelectionReason::CampaignDisabled)
        } else if settings.generation_paused {
            Some(DurableNoSelectionReason::CampaignPaused)
        } else if !settings.safety_setup_complete {
            Some(DurableNoSelectionReason::SafetyIncomplete)
        } else {
            None
        };

        let mut selected_source_version = None;
        let (audit, durable_no_selection_reason, selected_cooldown) = if let Some(reason) =
            early_reason
        {
            let audit = empty_selection_audit(authority.seed, cursor)?;
            (audit, Some(reason), None)
        } else {
            let party_level =
                trusted_party_level(&mut transaction, command.campaign_session_id.as_str()).await?;
            let theme_pack_id =
                trusted_theme_pack_id(&mut transaction, command.campaign_session_id.as_str())
                    .await?;
            let allowed_sensitivities =
                load_allowed_sensitivities(&mut transaction, command.campaign_session_id.as_str())
                    .await?;
            let (excluded_safety_codes, excluded_participants) = load_campaign_safety_exclusions(
                &mut transaction,
                command.campaign_session_id.as_str(),
            )
            .await?;
            let source_rows = sqlx::query(
                "SELECT DISTINCT source.source_id, source.source_version
                     FROM private_inspiration_sources AS source
                     JOIN private_inspiration_source_media AS media
                       ON media.source_id = source.source_id
                      AND media.source_version = source.source_version
                     WHERE source.review_state = 'approved' AND source.q11_screened
                       AND media.media = $1
                       AND (source.expires_at_epoch IS NULL OR source.expires_at_epoch > $2)
                     ORDER BY source.source_id, source.source_version",
            )
            .bind(command.media.as_str())
            .bind(inspiration_to_i64(now)?)
            .fetch_all(&mut *transaction)
            .await
            .map_err(db)?;
            let mut authenticated_prompts = Vec::new();
            let mut source_versions = BTreeMap::<(String, String), u64>::new();
            let mut consenting_participants = BTreeSet::new();
            for row in source_rows {
                let source_id: String = row.try_get("source_id").map_err(db)?;
                let source_version = to_u64(row.try_get("source_version").map_err(db)?)?;
                let source = load_source_for_update(&mut transaction, &source_id, source_version)
                    .await?
                    .ok_or_else(|| invalid("source_disappeared_during_selection"))?;
                if !source.sensitivities.is_subset(&allowed_sensitivities)
                    || !source.sensitivities.is_disjoint(&excluded_safety_codes)
                    || !source.participants.is_disjoint(&excluded_participants)
                    || !source.theme_pack_ids.contains(&theme_pack_id)
                    || source_is_vetoed(
                        &mut transaction,
                        command.campaign_session_id.as_str(),
                        &source,
                    )
                    .await?
                    || !all_source_participants_verified(&mut transaction, &source).await?
                    || !source_has_complete_consent(&mut transaction, command, &source, now).await?
                {
                    continue;
                }
                let Some(prompt) = prompts.iter().find(|prompt| {
                    prompt.privacy_source_id() == source.projection.source_id.as_str()
                        && prompt.source_digest() == &source.projection.source_digest
                        && prompt.metadata.enabled
                        && normalized_set(&prompt.metadata.sensitivity_tags)
                            == normalized_stored_set(&source.sensitivities)
                        && normalized_set(&prompt.metadata.participant_aliases)
                            == normalized_stored_set(&source.participants)
                }) else {
                    continue;
                };
                source_versions.insert(
                    (
                        source.projection.source_id.as_str().to_owned(),
                        source.projection.source_digest.as_str().to_owned(),
                    ),
                    source.projection.source_version,
                );
                consenting_participants.extend(normalized_stored_set(&source.participants));
                authenticated_prompts.push(prompt.clone());
            }
            let last_triggered_turn =
                load_last_triggered_turns(&mut transaction, command.campaign_session_id.as_str())
                    .await?;
            let normalized_allowed = normalized_stored_set(&allowed_sensitivities);
            let context = EventEligibility {
                inspiration_enabled: true,
                party_level,
                current_turn: turn_number,
                allowed_sensitivity_tags: &normalized_allowed,
                consenting_participant_aliases: &consenting_participants,
                last_triggered_turn: &last_triggered_turn,
            };
            let mut random = DeterministicEventRandom::new(authority.seed, cursor);
            let selected = EventPromptLoader
                .select_with_audit(&authenticated_prompts, &context, &mut random)
                .map_err(PrivateInspirationError::Selection)?;
            let selected_cooldown = selected.prompt.map(|prompt| prompt.metadata.cooldown_turns);
            if let (Some(source_id), Some(source_digest)) = (
                selected.audit.selected_source_id.as_ref(),
                selected.audit.selected_source_digest.as_ref(),
            ) {
                selected_source_version = source_versions
                    .get(&(source_id.clone(), source_digest.as_str().to_owned()))
                    .copied();
                if selected_source_version.is_none() {
                    return Err(invalid("selected_source_version_missing"));
                }
            }
            let reason = selected
                .prompt
                .is_none()
                .then_some(DurableNoSelectionReason::NoEligibleSources);
            (selected.audit, reason, selected_cooldown)
        };

        let selection_id = internal_id("inspiration-selection")?;
        persist_selection(
            &mut transaction,
            &selection_id,
            command,
            &request_fingerprint,
            authority,
            &trigger_window_id,
            turn_number,
            selected_source_version,
            durable_no_selection_reason,
            &audit,
            now,
        )
        .await?;
        if let (Some(source_id), Some(source_version), Some(source_digest), Some(cooldown)) = (
            audit.selected_source_id.as_deref(),
            selected_source_version,
            audit.selected_source_digest.as_ref(),
            selected_cooldown,
        ) {
            let next_eligible_turn = turn_number
                .checked_add(cooldown)
                .ok_or_else(|| invalid("cooldown_turn_overflow"))?;
            if next_eligible_turn <= turn_number {
                return Err(invalid("selected_source_cooldown"));
            }
            sqlx::query(
                "INSERT INTO private_inspiration_source_usage
                 (selection_id, campaign_session_id, source_id, source_version,
                  source_digest, turn_number, next_eligible_turn, created_at_epoch)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(selection_id.as_str())
            .bind(command.campaign_session_id.as_str())
            .bind(source_id)
            .bind(inspiration_to_i64(source_version)?)
            .bind(source_digest.as_str())
            .bind(inspiration_to_i64(turn_number)?)
            .bind(inspiration_to_i64(next_eligible_turn)?)
            .bind(inspiration_to_i64(now)?)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        sqlx::query(
            "UPDATE campaign_inspiration_settings SET rng_cursor = $2
             WHERE campaign_session_id = $1",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(inspiration_to_i64(audit.cursor_after)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "selection_reserved",
            "selection",
            selection_id.as_str(),
            audit.selected_source_id.as_deref(),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(PrivateInspirationSelection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            selection_id,
            campaign_session_id: command.campaign_session_id.clone(),
            source_version: selected_source_version,
            durable_no_selection_reason,
            audit,
            created_at_epoch: now,
        })
    }

    pub(crate) async fn load_private_inspiration_redacted_export(
        &self,
        campaign_session_id: &OpaqueInspirationId,
        requesting_participant_id: &OpaqueInspirationId,
    ) -> Result<CampaignInspirationRedactedExportV1, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, campaign_session_id.as_str()).await?;
        ensure_verified_participant(&mut transaction, requesting_participant_id.as_str()).await?;
        let settings = load_settings_for_update(&mut transaction, campaign_session_id.as_str())
            .await?
            .ok_or(PrivateInspirationError::NotFound)?;
        let grant_rows = sqlx::query(
            "SELECT grant_id, schema_version, source_id, source_version,
                    source_digest, participant_id, audience, media, transformation,
                    artifact_policy, expires_at_epoch, state
             FROM private_inspiration_consent_grants
             WHERE campaign_session_id = $1 AND participant_id = $2
             ORDER BY source_id, source_version, grant_id",
        )
        .bind(campaign_session_id.as_str())
        .bind(requesting_participant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;
        if grant_rows.is_empty() {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        let mut requester_grants = Vec::with_capacity(grant_rows.len());
        let mut source_keys = BTreeSet::new();
        for row in grant_rows {
            let projection = consent_projection_from_row(row)?;
            source_keys.insert((
                projection.source_id.as_str().to_owned(),
                projection.source_version,
            ));
            requester_grants.push(projection);
        }
        let mut sources = Vec::with_capacity(source_keys.len());
        for (source_id, source_version) in source_keys {
            let source = load_source_for_update(&mut transaction, &source_id, source_version)
                .await?
                .ok_or_else(|| invalid("export_source_missing"))?;
            if !source
                .participants
                .contains(requesting_participant_id.as_str())
            {
                return Err(invalid("export_participant_scope"));
            }
            sources.push(source.projection);
        }
        transaction.commit().await.map_err(db)?;
        Ok(CampaignInspirationRedactedExportV1 {
            schema_version: PRIVATE_INSPIRATION_EXPORT_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.clone(),
            requesting_participant_id: requesting_participant_id.clone(),
            settings,
            sources,
            requester_grants,
        })
    }
}

impl PostgresRepository {
    pub(crate) async fn grant_private_inspiration_consent(
        &self,
        command: &GrantConsentCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<ConsentGrantProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "consent_grant",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let settings =
            load_settings_for_update(&mut transaction, command.campaign_session_id.as_str())
                .await?
                .ok_or(PrivateInspirationError::ScopeDenied)?;
        if !settings.enabled || !settings.safety_setup_complete {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        ensure_verified_participant(&mut transaction, command.participant_id.as_str()).await?;
        let source = load_source_for_update(
            &mut transaction,
            command.source_id.as_str(),
            command.source_version,
        )
        .await?
        .ok_or(PrivateInspirationError::NotFound)?;
        let command_sensitivities = command
            .sensitivity_codes
            .iter()
            .map(|code| code.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let allowed_sensitivities =
            load_allowed_sensitivities(&mut transaction, command.campaign_session_id.as_str())
                .await?;
        if source.projection.source_digest != command.source_digest
            || source.projection.review_state != SourceReviewState::Approved
            || !source.projection.q11_screened
            || source
                .projection
                .expires_at_epoch
                .is_some_and(|expiry| expiry <= now)
            || !source
                .participants
                .contains(command.participant_id.as_str())
            || !source.projection.eligible_media.contains(&command.media)
            || source.sensitivities != command_sensitivities
            || !source.sensitivities.is_subset(&allowed_sensitivities)
            || source_is_vetoed(
                &mut transaction,
                command.campaign_session_id.as_str(),
                &source,
            )
            .await?
        {
            return Err(PrivateInspirationError::ScopeDenied);
        }

        sqlx::query(
            "UPDATE private_inspiration_consent_grants
             SET state = 'expired'
             WHERE campaign_session_id = $1 AND state = 'active'
               AND expires_at_epoch <= $2",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let active_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM private_inspiration_consent_grants
               WHERE campaign_session_id = $1 AND source_id = $2
                 AND source_version = $3 AND participant_id = $4
                 AND audience = $5 AND media = $6 AND transformation = $7
                 AND state = 'active'
             )",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(command.source_id.as_str())
        .bind(inspiration_to_i64(command.source_version)?)
        .bind(command.participant_id.as_str())
        .bind(command.audience.as_str())
        .bind(command.media.as_str())
        .bind(command.transformation.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        if active_exists {
            return Err(PrivateInspirationError::ScopeDenied);
        }

        let grant_id = internal_id("consent-grant")?;
        sqlx::query(
            "INSERT INTO private_inspiration_consent_grants
             (grant_id, schema_version, campaign_session_id, source_id,
              source_version, source_digest, participant_id, audience, media,
              transformation, artifact_policy, reviewer_id,
              participant_confirmation_digest, review_evidence_digest, state,
              granted_at_epoch, expires_at_epoch)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                     $12, $13, $14, 'active', $15, $16)",
        )
        .bind(grant_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.campaign_session_id.as_str())
        .bind(command.source_id.as_str())
        .bind(inspiration_to_i64(command.source_version)?)
        .bind(command.source_digest.as_str())
        .bind(command.participant_id.as_str())
        .bind(command.audience.as_str())
        .bind(command.media.as_str())
        .bind(command.transformation.as_str())
        .bind(command.artifact_policy.as_str())
        .bind(command.reviewer_id.as_str())
        .bind(command.participant_confirmation_digest.as_str())
        .bind(command.review_evidence_digest.as_str())
        .bind(inspiration_to_i64(now)?)
        .bind(inspiration_to_i64(command.expires_at_epoch)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        for sensitivity in &command.sensitivity_codes {
            sqlx::query(
                "INSERT INTO private_inspiration_consent_sensitivities
                 (grant_id, sensitivity_code) VALUES ($1, $2)",
            )
            .bind(grant_id.as_str())
            .bind(sensitivity.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }

        let projection = ConsentGrantProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            grant_id,
            source_id: command.source_id.clone(),
            source_version: command.source_version,
            source_digest: command.source_digest.clone(),
            participant_id: command.participant_id.clone(),
            audience: command.audience,
            media: command.media,
            transformation: command.transformation,
            artifact_policy: command.artifact_policy,
            expires_at_epoch: command.expires_at_epoch,
            state: ConsentGrantState::Active,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "consent_grant",
            request_fingerprint,
            &projection,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "consent_granted",
            "consent_grant",
            projection.grant_id.as_str(),
            Some(command.source_id.as_str()),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }

    pub(crate) async fn revoke_private_inspiration_consent(
        &self,
        command: &RevokeConsentCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<PrivacyTransitionOutcome, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "consent_revoke",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let row = sqlx::query(
            "SELECT campaign_session_id, source_id, source_version,
                    participant_id, state
             FROM private_inspiration_consent_grants
             WHERE grant_id = $1 FOR UPDATE",
        )
        .bind(command.grant_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(PrivateInspirationError::NotFound)?;
        let campaign: String = row.try_get("campaign_session_id").map_err(db)?;
        let participant: String = row.try_get("participant_id").map_err(db)?;
        let state: String = row.try_get("state").map_err(db)?;
        if campaign != command.campaign_session_id.as_str()
            || participant != command.requester_participant_id.as_str()
            || state == "revoked"
        {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        let source_id: String = row.try_get("source_id").map_err(db)?;
        let source_version = to_u64(row.try_get("source_version").map_err(db)?)?;
        sqlx::query(
            "UPDATE private_inspiration_consent_grants
             SET state = 'revoked', revoked_at_epoch = $2, revocation_code = $3
             WHERE grant_id = $1",
        )
        .bind(command.grant_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .bind(command.reason.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let pending_work_cancellation_ids = cancel_source_pending_work(
            &mut transaction,
            command.campaign_session_id.as_str(),
            &source_id,
            source_version,
            now,
        )
        .await?;
        apply_source_completed_work_policy(
            &mut transaction,
            command.campaign_session_id.as_str(),
            &source_id,
            source_version,
            now,
        )
        .await?;
        let outcome = PrivacyTransitionOutcome {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            subject_id: command.grant_id.clone(),
            pending_work_cancellation_ids,
            effective_at_epoch: now,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "consent_revoke",
            request_fingerprint,
            &outcome,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "consent_revoked",
            "consent_grant",
            command.grant_id.as_str(),
            Some(command.requester_participant_id.as_str()),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(outcome)
    }

    pub(crate) async fn apply_private_inspiration_veto(
        &self,
        command: &ApplyInspirationVetoCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<(VetoProjection, PrivacyTransitionOutcome), PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt::<VetoReceipt>(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "veto_apply",
            request_fingerprint,
        )
        .await?
        {
            return Ok((replay.veto, replay.transition));
        }
        ensure_verified_participant(&mut transaction, command.participant_id.as_str()).await?;
        if let InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            source_digest,
        } = &command.scope
        {
            let source =
                load_source_for_update(&mut transaction, source_id.as_str(), *source_version)
                    .await?
                    .ok_or(PrivateInspirationError::NotFound)?;
            if source.projection.source_digest != *source_digest
                || !source
                    .participants
                    .contains(command.participant_id.as_str())
            {
                return Err(PrivateInspirationError::ScopeDenied);
            }
        }
        let veto_id = internal_id("inspiration-veto")?;
        let (scope_kind, category_id, source_id, source_version, source_digest) =
            match &command.scope {
                InspirationVetoScope::Campaign => ("campaign", None, None, None, None),
                InspirationVetoScope::Category { category_id } => {
                    ("category", Some(category_id.as_str()), None, None, None)
                }
                InspirationVetoScope::SourceVersion {
                    source_id,
                    source_version,
                    source_digest,
                } => (
                    "source_version",
                    None,
                    Some(source_id.as_str()),
                    Some(inspiration_to_i64(*source_version)?),
                    Some(source_digest.as_str()),
                ),
            };
        sqlx::query(
            "INSERT INTO private_inspiration_vetoes
             (veto_id, schema_version, campaign_session_id, participant_id,
              scope_kind, category_id, source_id, source_version, source_digest,
              veto_code, created_at_epoch)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(veto_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.campaign_session_id.as_str())
        .bind(command.participant_id.as_str())
        .bind(scope_kind)
        .bind(category_id)
        .bind(source_id)
        .bind(source_version)
        .bind(source_digest)
        .bind(command.code.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let pending_work_cancellation_ids = cancel_vetoed_pending_work(
            &mut transaction,
            command.campaign_session_id.as_str(),
            &command.scope,
            now,
        )
        .await?;
        apply_vetoed_completed_work_policy(
            &mut transaction,
            command.campaign_session_id.as_str(),
            &command.scope,
            now,
        )
        .await?;
        let veto = VetoProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            veto_id: veto_id.clone(),
            campaign_session_id: command.campaign_session_id.clone(),
            participant_id: command.participant_id.clone(),
            scope: command.scope.clone(),
            code: command.code,
            created_at_epoch: now,
        };
        let transition = PrivacyTransitionOutcome {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            subject_id: veto_id,
            pending_work_cancellation_ids,
            effective_at_epoch: now,
        };
        let receipt = VetoReceipt {
            veto: veto.clone(),
            transition: transition.clone(),
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "veto_apply",
            request_fingerprint,
            &receipt,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "veto_applied",
            "veto",
            veto.veto_id.as_str(),
            Some(command.participant_id.as_str()),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok((veto, transition))
    }

    pub(crate) async fn register_private_inspiration_derived_work(
        &self,
        command: &RegisterDerivedWorkCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<DerivedWorkProjection, PrivateInspirationError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        lock_campaign(&mut transaction, command.campaign_session_id.as_str()).await?;
        if let Some(replay) = load_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "derived_work_register",
            request_fingerprint,
        )
        .await?
        {
            return Ok(replay);
        }
        let row = sqlx::query(
            "SELECT selected_source_id AS source_id, selected_source_version AS source_version,
                    selected_source_digest AS source_digest, media
             FROM private_inspiration_selection_audits
             WHERE selection_id = $1 AND campaign_session_id = $2
               AND selected_source_id IS NOT NULL
             FOR UPDATE",
        )
        .bind(command.selection_id.as_str())
        .bind(command.campaign_session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(PrivateInspirationError::NotFound)?;
        let source_id: String = row.try_get("source_id").map_err(db)?;
        let source_version = to_u64(row.try_get("source_version").map_err(db)?)?;
        let source_digest = stored_digest(row.try_get("source_digest").map_err(db)?)?;
        let media: String = row.try_get("media").map_err(db)?;
        if media != command.kind.as_str() {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        let source = load_source_for_update(&mut transaction, &source_id, source_version)
            .await?
            .ok_or(PrivateInspirationError::NotFound)?;
        if source.projection.source_digest != source_digest
            || source_is_vetoed(
                &mut transaction,
                command.campaign_session_id.as_str(),
                &source,
            )
            .await?
        {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        let policies = sqlx::query_scalar::<_, String>(
            "SELECT artifact_policy FROM private_inspiration_consent_grants
             WHERE campaign_session_id = $1 AND source_id = $2
               AND source_version = $3 AND source_digest = $4
               AND media = $5 AND state = 'active' AND expires_at_epoch > $6",
        )
        .bind(command.campaign_session_id.as_str())
        .bind(&source_id)
        .bind(inspiration_to_i64(source_version)?)
        .bind(source_digest.as_str())
        .bind(&media)
        .bind(inspiration_to_i64(now)?)
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;
        if policies.len() != source.participants.len()
            || policies.iter().any(|policy| {
                DerivedArtifactPolicy::parse(policy).map_or(true, |stored| {
                    artifact_policy_rank(command.artifact_policy) < artifact_policy_rank(stored)
                })
            })
        {
            return Err(PrivateInspirationError::ScopeDenied);
        }
        sqlx::query(
            "INSERT INTO private_inspiration_derived_work
             (work_id, schema_version, campaign_session_id, selection_id,
              source_id, source_version, source_digest, work_kind, state,
              artifact_policy, created_at_epoch)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending', $9, $10)",
        )
        .bind(command.work_id.as_str())
        .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
        .bind(command.campaign_session_id.as_str())
        .bind(command.selection_id.as_str())
        .bind(&source_id)
        .bind(inspiration_to_i64(source_version)?)
        .bind(source_digest.as_str())
        .bind(command.kind.as_str())
        .bind(command.artifact_policy.as_str())
        .bind(inspiration_to_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let projection = DerivedWorkProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            work_id: command.work_id.clone(),
            selection_id: command.selection_id.clone(),
            source_id: OpaqueInspirationId::new(source_id)?,
            source_version,
            source_digest,
            kind: command.kind,
            artifact_policy: command.artifact_policy,
        };
        insert_receipt(
            &mut transaction,
            command.campaign_session_id.as_str(),
            command.idempotency_key.as_str(),
            "derived_work_register",
            request_fingerprint,
            &projection,
            now,
        )
        .await?;
        insert_privacy_audit(
            &mut transaction,
            Some(command.campaign_session_id.as_str()),
            "derived_work_registered",
            "derived_work",
            command.work_id.as_str(),
            Some(command.selection_id.as_str()),
            "applied",
            now,
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(projection)
    }
}

fn global_control_from_row(
    row: PgRow,
) -> Result<GlobalInspirationControlProjection, PrivateInspirationError> {
    let schema = to_u64(row.try_get("schema_version").map_err(db)?)?;
    if schema != u64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION) {
        return Err(invalid("stored_global_control_schema"));
    }
    Ok(GlobalInspirationControlProjection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        revision: to_u64(row.try_get("revision").map_err(db)?)?,
        generation_disabled: row.try_get("generation_disabled").map_err(db)?,
        updated_at_epoch: to_u64(row.try_get("updated_at_epoch").map_err(db)?)?,
    })
}

fn restricted_access_projection_from_row(
    row: PgRow,
) -> Result<RestrictedDiagnosticAccessProjection, PrivateInspirationError> {
    let schema = to_u64(row.try_get("schema_version").map_err(db)?)?;
    if schema != u64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION) {
        return Err(invalid("stored_restricted_access_schema"));
    }
    Ok(RestrictedDiagnosticAccessProjection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        audit_id: OpaqueInspirationId::new(row.try_get::<String, _>("audit_id").map_err(db)?)?,
        campaign_session_id: row
            .try_get::<Option<String>, _>("campaign_session_id")
            .map_err(db)?
            .map(OpaqueInspirationId::new)
            .transpose()?,
        operator_id: OpaqueOperatorId::new(row.try_get::<String, _>("operator_id").map_err(db)?)?,
        access_kind: RestrictedDiagnosticAccessKind::parse(
            &row.try_get::<String, _>("access_kind").map_err(db)?,
        )?,
        purpose: RestrictedDiagnosticPurpose::parse(
            &row.try_get::<String, _>("purpose_code").map_err(db)?,
        )?,
        subject_id: OpaqueInspirationId::new(row.try_get::<String, _>("subject_id").map_err(db)?)?,
        evidence_digest: stored_digest(row.try_get("evidence_digest").map_err(db)?)?,
        decision: RestrictedDiagnosticDecision::parse(
            &row.try_get::<String, _>("result_code").map_err(db)?,
        )?,
        occurred_at_epoch: to_u64(row.try_get("occurred_at_epoch").map_err(db)?)?,
    })
}

async fn quarantine_all_private_inspiration_work(
    transaction: &mut Transaction<'_, Postgres>,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let pending = sqlx::query(
        "UPDATE private_inspiration_derived_work
         SET state = 'cancellation_requested',
             cancellation_requested_at_epoch = GREATEST(created_at_epoch, $1)
         WHERE state = 'pending'
         RETURNING campaign_session_id, work_id",
    )
    .bind(inspiration_to_i64(now)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    for row in pending {
        let campaign_id: String = row.try_get("campaign_session_id").map_err(db)?;
        let work_id: String = row.try_get("work_id").map_err(db)?;
        insert_privacy_audit(
            transaction,
            Some(&campaign_id),
            "derived_work_cancel_requested",
            "derived_work",
            &work_id,
            None,
            "cancel_requested",
            now,
        )
        .await?;
    }
    let completed = sqlx::query(
        "UPDATE generated_text_presentations AS presentation
         SET body = $1, privacy_state = 'redacted', updated_at = CURRENT_TIMESTAMP
         FROM private_inspiration_derived_work AS work
         WHERE presentation.private_inspiration_work_id = work.work_id
           AND work.state IN ('completed', 'redacted')
         RETURNING work.campaign_session_id, work.work_id, presentation.id",
    )
    .bind(PRIVATE_INSPIRATION_REDACTION_BODY)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    for row in completed {
        let campaign_id: String = row.try_get("campaign_session_id").map_err(db)?;
        let work_id: String = row.try_get("work_id").map_err(db)?;
        let presentation_id: String = row.try_get("id").map_err(db)?;
        sqlx::query(
            "UPDATE private_inspiration_derived_work
             SET state = 'redacted' WHERE work_id = $1",
        )
        .bind(&work_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
        insert_privacy_audit(
            transaction,
            Some(&campaign_id),
            "derived_work_redacted",
            "derived_work",
            &work_id,
            Some(&presentation_id),
            "applied",
            now,
        )
        .await?;
    }
    Ok(())
}

async fn lock_campaign(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<LockedCampaign, PrivateInspirationError> {
    let row = sqlx::query(
        "SELECT revision, payload_json::text AS payload_json
         FROM campaign_sessions WHERE id = $1 FOR UPDATE",
    )
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    .ok_or(PrivateInspirationError::NotFound)?;
    let revision = to_u64(row.try_get("revision").map_err(db)?)?;
    let payload: String = row.try_get("payload_json").map_err(db)?;
    let session: SessionDto =
        serde_json::from_str(&payload).map_err(|_| invalid("stored_campaign_payload"))?;
    session
        .validate()
        .map_err(|_| invalid("stored_campaign_validation"))?;
    if session.id != campaign_session_id
        || session.last_event_sequence.checked_add(1) != Some(revision)
    {
        return Err(invalid("stored_campaign_identity_or_revision"));
    }
    Ok(LockedCampaign { session, revision })
}

async fn trusted_trigger_window(
    transaction: &mut Transaction<'_, Postgres>,
    campaign: &LockedCampaign,
) -> Result<(u64, OpaqueInspirationId), PrivateInspirationError> {
    if campaign.session.status != SessionStatus::Active || campaign.session.last_event_sequence == 0
    {
        return Err(PrivateInspirationError::ScopeDenied);
    }
    let turn_number = campaign.session.last_event_sequence;
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json::text FROM turn_audits
         WHERE campaign_session_id = $1 AND turn_number = $2",
    )
    .bind(&campaign.session.id)
    .bind(inspiration_to_i64(turn_number)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    .ok_or_else(|| invalid("trigger_event_missing"))?;
    let event: SessionEventDto =
        serde_json::from_str(&payload).map_err(|_| invalid("stored_trigger_event"))?;
    event
        .validate()
        .map_err(|_| invalid("stored_trigger_event_validation"))?;
    if event.session_id != campaign.session.id || event.sequence != turn_number {
        return Err(invalid("stored_trigger_event_identity"));
    }
    let safe = match event.payload {
        SessionEventPayload::AbilityCheckResolved { .. }
        | SessionEventPayload::ExplorationSocialResolved { .. }
        | SessionEventPayload::GmNarration { .. }
        | SessionEventPayload::ExperienceAwarded { .. }
        | SessionEventPayload::AiProposalAccepted { .. }
        | SessionEventPayload::AiProposalRejected { .. } => true,
        SessionEventPayload::EncounterResolved { outcome, .. } => {
            matches!(outcome.resolution.state.status, EncounterStatus::Victory)
                || (matches!(outcome.resolution.state.status, EncounterStatus::Defeat)
                    && outcome
                        .resolution
                        .state
                        .transition
                        .as_ref()
                        .is_some_and(|transition| transition.story_recovery_applied))
        }
        SessionEventPayload::SessionStarted
        | SessionEventPayload::PlayerIntent { .. }
        | SessionEventPayload::DiceResolved { .. }
        | SessionEventPayload::SessionEnded => false,
    };
    if !safe {
        return Err(PrivateInspirationError::ScopeDenied);
    }
    Ok((
        turn_number,
        OpaqueInspirationId::new(format!("trigger-window:{turn_number}"))?,
    ))
}

async fn trusted_party_level(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<u8, PrivateInspirationError> {
    let hero_payload: Option<String> = sqlx::query_scalar(
        "SELECT payload_json::text FROM hero_characters
         WHERE campaign_session_id = $1 ORDER BY id LIMIT 1",
    )
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    if let Some(payload) = hero_payload {
        let hero: HeroCharacter =
            serde_json::from_str(&payload).map_err(|_| invalid("stored_hero_payload"))?;
        hero.validate()
            .map_err(|_| invalid("stored_hero_validation"))?;
        if hero.campaign_id != campaign_session_id {
            return Err(invalid("stored_hero_campaign"));
        }
        return Ok(hero.level.value());
    }
    let character_payloads = sqlx::query_scalar::<_, String>(
        "SELECT payload_json::text FROM characters
         WHERE campaign_session_id = $1 ORDER BY id",
    )
    .bind(campaign_session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    let mut party_level = None;
    for payload in character_payloads {
        let character: Character =
            serde_json::from_str(&payload).map_err(|_| invalid("stored_character_payload"))?;
        character
            .validate()
            .map_err(|_| invalid("stored_character_validation"))?;
        party_level = Some(
            party_level.map_or(character.level().value(), |current: u8| {
                current.max(character.level().value())
            }),
        );
    }
    party_level.ok_or_else(|| invalid("campaign_party_missing"))
}

async fn trusted_theme_pack_id(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<String, PrivateInspirationError> {
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json::text FROM campaign_content_pins
         WHERE campaign_session_id = $1",
    )
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    .ok_or_else(|| invalid("campaign_theme_pins_missing"))?;
    let pins: CampaignContentPins =
        serde_json::from_str(&payload).map_err(|_| invalid("stored_campaign_pins"))?;
    pins.validate()
        .map_err(|_| invalid("stored_campaign_pins_validation"))?;
    Ok(pins.hero.theme_id.pack_id().to_owned())
}

fn empty_selection_audit(
    seed: [u8; 32],
    cursor: u64,
) -> Result<EventSelectionAudit, PrivateInspirationError> {
    let allowed = BTreeSet::new();
    let participants = BTreeSet::new();
    let history = HashMap::new();
    let context = EventEligibility {
        inspiration_enabled: false,
        party_level: 1,
        current_turn: 0,
        allowed_sensitivity_tags: &allowed,
        consenting_participant_aliases: &participants,
        last_triggered_turn: &history,
    };
    let mut random = DeterministicEventRandom::new(seed, cursor);
    EventPromptLoader
        .select_with_audit(&[], &context, &mut random)
        .map(|selected| selected.audit)
        .map_err(PrivateInspirationError::Selection)
}

async fn all_source_participants_verified(
    transaction: &mut Transaction<'_, Postgres>,
    source: &StoredSource,
) -> Result<bool, PrivateInspirationError> {
    for participant in &source.participants {
        let verified: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM private_inspiration_participants
               WHERE participant_id = $1 AND verification_state = 'verified'
             )",
        )
        .bind(participant)
        .fetch_one(&mut **transaction)
        .await
        .map_err(db)?;
        if !verified {
            return Ok(false);
        }
    }
    Ok(!source.participants.is_empty())
}

async fn source_has_complete_consent(
    transaction: &mut Transaction<'_, Postgres>,
    command: &RequestInspirationSelectionCommand,
    source: &StoredSource,
    now: u64,
) -> Result<bool, PrivateInspirationError> {
    let rows = sqlx::query(
        "SELECT grant_id, participant_id
         FROM private_inspiration_consent_grants
         WHERE campaign_session_id = $1 AND source_id = $2
           AND source_version = $3 AND source_digest = $4
           AND audience = $5 AND media = $6
           AND transformation = 'high_fiction_distance_v1'
           AND state = 'active' AND expires_at_epoch > $7
         ORDER BY participant_id",
    )
    .bind(command.campaign_session_id.as_str())
    .bind(source.projection.source_id.as_str())
    .bind(inspiration_to_i64(source.projection.source_version)?)
    .bind(source.projection.source_digest.as_str())
    .bind(command.audience.as_str())
    .bind(command.media.as_str())
    .bind(inspiration_to_i64(now)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    if rows.len() != source.participants.len() {
        return Ok(false);
    }
    let mut granted_participants = BTreeSet::new();
    for row in rows {
        let grant_id: String = row.try_get("grant_id").map_err(db)?;
        let participant_id: String = row.try_get("participant_id").map_err(db)?;
        let sensitivities = sqlx::query_scalar::<_, String>(
            "SELECT sensitivity_code FROM private_inspiration_consent_sensitivities
             WHERE grant_id = $1 ORDER BY sensitivity_code",
        )
        .bind(&grant_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?
        .into_iter()
        .collect::<BTreeSet<_>>();
        if sensitivities != source.sensitivities {
            return Ok(false);
        }
        granted_participants.insert(participant_id);
    }
    Ok(granted_participants == source.participants)
}

async fn load_last_triggered_turns(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<HashMap<String, u64>, PrivateInspirationError> {
    let rows = sqlx::query(
        "SELECT source_id, MAX(turn_number) AS turn_number
         FROM private_inspiration_source_usage
         WHERE campaign_session_id = $1 GROUP BY source_id",
    )
    .bind(campaign_session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("source_id").map_err(db)?,
                to_u64(row.try_get("turn_number").map_err(db)?)?,
            ))
        })
        .collect()
}

fn normalized_set(values: &[String]) -> BTreeSet<String> {
    values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect()
}

fn normalized_stored_set(values: &BTreeSet<String>) -> BTreeSet<String> {
    values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn persist_selection(
    transaction: &mut Transaction<'_, Postgres>,
    selection_id: &OpaqueInspirationId,
    command: &RequestInspirationSelectionCommand,
    request_fingerprint: &Sha256Digest,
    authority: &ResolvedInspirationSelectionAuthority,
    trigger_window_id: &OpaqueInspirationId,
    turn_number: u64,
    selected_source_version: Option<u64>,
    durable_no_selection_reason: Option<DurableNoSelectionReason>,
    audit: &EventSelectionAudit,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    sqlx::query(
        "INSERT INTO private_inspiration_selection_audits
         (selection_id, schema_version, campaign_session_id, idempotency_key,
          request_fingerprint, trigger_window_id, campaign_revision, turn_number,
          audience, media, seed_reference, eligible_set_digest,
          eligible_source_count, selected_source_id, selected_source_version,
          selected_source_digest, no_selection_reason, sample_numerator,
          sample_denominator, algorithm, cursor_before, cursor_after,
          created_at_epoch)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23)",
    )
    .bind(selection_id.as_str())
    .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
    .bind(command.campaign_session_id.as_str())
    .bind(command.idempotency_key.as_str())
    .bind(request_fingerprint.as_str())
    .bind(trigger_window_id.as_str())
    .bind(inspiration_to_i64(command.expected_campaign_revision)?)
    .bind(inspiration_to_i64(turn_number)?)
    .bind(command.audience.as_str())
    .bind(command.media.as_str())
    .bind(authority.seed_reference.as_str())
    .bind(audit.eligible_set_digest.as_str())
    .bind(i64::from(audit.eligible_source_count))
    .bind(audit.selected_source_id.as_deref())
    .bind(
        selected_source_version
            .map(inspiration_to_i64)
            .transpose()?,
    )
    .bind(
        audit
            .selected_source_digest
            .as_ref()
            .map(Sha256Digest::as_str),
    )
    .bind(durable_no_selection_reason.map(DurableNoSelectionReason::as_str))
    .bind(audit.sample_numerator.map(inspiration_to_i64).transpose()?)
    .bind(
        audit
            .sample_denominator
            .map(inspiration_to_i64)
            .transpose()?,
    )
    .bind(audit.algorithm.as_str())
    .bind(inspiration_to_i64(audit.cursor_before)?)
    .bind(inspiration_to_i64(audit.cursor_after)?)
    .bind(inspiration_to_i64(now)?)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(())
}

async fn load_selection_replay(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    idempotency_key: &str,
    request_fingerprint: &Sha256Digest,
) -> Result<Option<PrivateInspirationSelection>, PrivateInspirationError> {
    let row = sqlx::query(
        "SELECT selection_id, schema_version, campaign_session_id,
                request_fingerprint, eligible_set_digest, eligible_source_count,
                selected_source_id, selected_source_version, selected_source_digest,
                no_selection_reason, sample_numerator, sample_denominator,
                algorithm, cursor_before, cursor_after, created_at_epoch
         FROM private_inspiration_selection_audits
         WHERE campaign_session_id = $1 AND idempotency_key = $2",
    )
    .bind(campaign_session_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_fingerprint: String = row.try_get("request_fingerprint").map_err(db)?;
    if stored_fingerprint != request_fingerprint.as_str() {
        return Err(PrivateInspirationError::IdempotencyConflict);
    }
    let schema = to_u64(row.try_get("schema_version").map_err(db)?)?;
    let algorithm: String = row.try_get("algorithm").map_err(db)?;
    if schema != u64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
        || algorithm != RollAlgorithm::ChaCha20V1.as_str()
    {
        return Err(invalid("stored_selection_schema_or_algorithm"));
    }
    let durable_reason = row
        .try_get::<Option<String>, _>("no_selection_reason")
        .map_err(db)?
        .map(|reason| DurableNoSelectionReason::parse(&reason))
        .transpose()?;
    let selected_source_id: Option<String> = row.try_get("selected_source_id").map_err(db)?;
    let selected_source_digest = row
        .try_get::<Option<String>, _>("selected_source_digest")
        .map_err(db)?
        .map(stored_digest)
        .transpose()?;
    let no_selection_reason = durable_reason.map(|reason| match reason {
        DurableNoSelectionReason::NoEligibleSources => EventNoSelectionReason::NoEligibleSources,
        DurableNoSelectionReason::DeploymentDisabled
        | DurableNoSelectionReason::GlobalKillSwitch
        | DurableNoSelectionReason::CampaignDisabled
        | DurableNoSelectionReason::CampaignPaused
        | DurableNoSelectionReason::SafetyIncomplete => EventNoSelectionReason::CampaignDisabled,
    });
    Ok(Some(PrivateInspirationSelection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        selection_id: OpaqueInspirationId::new(
            row.try_get::<String, _>("selection_id").map_err(db)?,
        )?,
        campaign_session_id: OpaqueInspirationId::new(
            row.try_get::<String, _>("campaign_session_id")
                .map_err(db)?,
        )?,
        source_version: row
            .try_get::<Option<i64>, _>("selected_source_version")
            .map_err(db)?
            .map(to_u64)
            .transpose()?,
        durable_no_selection_reason: durable_reason,
        audit: EventSelectionAudit {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            eligible_set_digest: stored_digest(row.try_get("eligible_set_digest").map_err(db)?)?,
            eligible_source_count: u32::try_from(to_u64(
                row.try_get("eligible_source_count").map_err(db)?,
            )?)
            .map_err(|_| invalid("stored_eligible_count"))?,
            selected_source_id,
            selected_source_digest,
            no_selection_reason,
            sample_numerator: row
                .try_get::<Option<i64>, _>("sample_numerator")
                .map_err(db)?
                .map(to_u64)
                .transpose()?,
            sample_denominator: row
                .try_get::<Option<i64>, _>("sample_denominator")
                .map_err(db)?
                .map(to_u64)
                .transpose()?,
            algorithm: RollAlgorithm::ChaCha20V1,
            cursor_before: to_u64(row.try_get("cursor_before").map_err(db)?)?,
            cursor_after: to_u64(row.try_get("cursor_after").map_err(db)?)?,
        },
        created_at_epoch: to_u64(row.try_get("created_at_epoch").map_err(db)?)?,
    }))
}

fn consent_projection_from_row(
    row: PgRow,
) -> Result<ConsentGrantProjection, PrivateInspirationError> {
    let schema = to_u64(row.try_get("schema_version").map_err(db)?)?;
    let audience: String = row.try_get("audience").map_err(db)?;
    let transformation: String = row.try_get("transformation").map_err(db)?;
    if schema != u64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
        || audience != "private_campaign"
        || transformation != "high_fiction_distance_v1"
    {
        return Err(invalid("stored_consent_policy"));
    }
    Ok(ConsentGrantProjection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        grant_id: OpaqueInspirationId::new(row.try_get::<String, _>("grant_id").map_err(db)?)?,
        source_id: OpaqueInspirationId::new(row.try_get::<String, _>("source_id").map_err(db)?)?,
        source_version: to_u64(row.try_get("source_version").map_err(db)?)?,
        source_digest: stored_digest(row.try_get("source_digest").map_err(db)?)?,
        participant_id: OpaqueInspirationId::new(
            row.try_get::<String, _>("participant_id").map_err(db)?,
        )?,
        audience: InspirationAudience::PrivateCampaign,
        media: InspirationMedia::parse(&row.try_get::<String, _>("media").map_err(db)?)?,
        transformation: InspirationTransformation::HighFictionDistanceV1,
        artifact_policy: DerivedArtifactPolicy::parse(
            &row.try_get::<String, _>("artifact_policy").map_err(db)?,
        )?,
        expires_at_epoch: to_u64(row.try_get("expires_at_epoch").map_err(db)?)?,
        state: ConsentGrantState::parse(&row.try_get::<String, _>("state").map_err(db)?)?,
    })
}

async fn load_settings_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<Option<CampaignInspirationSettingsProjection>, PrivateInspirationError> {
    let row = sqlx::query(&format!(
        "SELECT {SETTINGS_COLUMNS} FROM campaign_inspiration_settings
         WHERE campaign_session_id = $1 FOR UPDATE"
    ))
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    row.map(settings_from_row).transpose()
}

fn settings_from_row(
    row: PgRow,
) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
    let schema = to_u64(row.try_get("schema_version").map_err(db)?)?;
    let fictional_distance: String = row.try_get("fictional_distance").map_err(db)?;
    let audience: String = row.try_get("audience").map_err(db)?;
    let media: String = row.try_get("media").map_err(db)?;
    let q11_policy_id: String = row.try_get("q11_policy_id").map_err(db)?;
    let tone = CampaignInspirationTone::parse(&row.try_get::<String, _>("tone").map_err(db)?)?;
    let adults_only: bool = row.try_get("adults_only").map_err(db)?;
    if schema != u64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
        || fictional_distance != "high_locked"
        || audience != "private_campaign"
        || media != "text"
        || q11_policy_id != Q11_CONSERVATIVE_POLICY_ID
        || !adults_only
    {
        return Err(invalid("stored_settings_policy"));
    }
    Ok(CampaignInspirationSettingsProjection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        campaign_session_id: OpaqueInspirationId::new(
            row.try_get::<String, _>("campaign_session_id")
                .map_err(db)?,
        )?,
        revision: to_u64(row.try_get("revision").map_err(db)?)?,
        enabled: row.try_get("enabled").map_err(db)?,
        generation_paused: row.try_get("generation_paused").map_err(db)?,
        safety_setup_complete: row.try_get("safety_setup_complete").map_err(db)?,
        adults_only,
        fictional_distance_locked_high: true,
        tone,
        line_count: u32::try_from(to_u64(row.try_get("line_count").map_err(db)?)?)
            .map_err(|_| invalid("stored_line_count"))?,
        veil_count: u32::try_from(to_u64(row.try_get("veil_count").map_err(db)?)?)
            .map_err(|_| invalid("stored_veil_count"))?,
        excluded_topic_count: u32::try_from(to_u64(
            row.try_get("excluded_topic_count").map_err(db)?,
        )?)
        .map_err(|_| invalid("stored_excluded_topic_count"))?,
        excluded_participant_count: u32::try_from(to_u64(
            row.try_get("excluded_participant_count").map_err(db)?,
        )?)
        .map_err(|_| invalid("stored_excluded_participant_count"))?,
        audience: InspirationAudience::PrivateCampaign,
        media: InspirationMedia::Text,
        q11_policy_id,
        updated_at_epoch: to_u64(row.try_get("updated_at_epoch").map_err(db)?)?,
    })
}

async fn ensure_verified_participant(
    transaction: &mut Transaction<'_, Postgres>,
    participant_id: &str,
) -> Result<(), PrivateInspirationError> {
    let row = sqlx::query(
        "SELECT verification_state, verification_method
         FROM private_inspiration_participants
         WHERE participant_id = $1 FOR UPDATE",
    )
    .bind(participant_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    let Some(row) = row else {
        return Err(PrivateInspirationError::ScopeDenied);
    };
    let state: String = row.try_get("verification_state").map_err(db)?;
    ParticipantVerificationMethod::parse(
        &row.try_get::<String, _>("verification_method")
            .map_err(db)?,
    )?;
    if state != "verified" {
        return Err(PrivateInspirationError::ScopeDenied);
    }
    Ok(())
}

async fn replace_campaign_safety_setup(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    setup: Option<&SafetySetupEvidence>,
) -> Result<(), PrivateInspirationError> {
    for table in [
        "campaign_inspiration_allowed_sensitivities",
        "campaign_inspiration_lines",
        "campaign_inspiration_veils",
        "campaign_inspiration_excluded_topics",
        "campaign_inspiration_excluded_participants",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE campaign_session_id = $1"
        ))
        .bind(campaign_session_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    let Some(setup) = setup else {
        return Ok(());
    };
    for (table, column, codes) in [
        (
            "campaign_inspiration_allowed_sensitivities",
            "sensitivity_code",
            &setup.allowed_sensitivity_codes,
        ),
        (
            "campaign_inspiration_lines",
            "safety_code",
            &setup.line_codes,
        ),
        (
            "campaign_inspiration_veils",
            "safety_code",
            &setup.veil_codes,
        ),
        (
            "campaign_inspiration_excluded_topics",
            "safety_code",
            &setup.excluded_topic_codes,
        ),
        (
            "campaign_inspiration_excluded_participants",
            "participant_id",
            &setup.excluded_participant_ids,
        ),
    ] {
        for code in codes {
            sqlx::query(&format!(
                "INSERT INTO {table} (campaign_session_id, {column}) VALUES ($1, $2)"
            ))
            .bind(campaign_session_id)
            .bind(code.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
        }
    }
    Ok(())
}

async fn load_allowed_sensitivities(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<BTreeSet<String>, PrivateInspirationError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT sensitivity_code
         FROM campaign_inspiration_allowed_sensitivities
         WHERE campaign_session_id = $1 ORDER BY sensitivity_code",
    )
    .bind(campaign_session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .collect())
}

async fn load_campaign_safety_exclusions(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
) -> Result<(BTreeSet<String>, BTreeSet<String>), PrivateInspirationError> {
    let safety_codes = sqlx::query_scalar::<_, String>(
        "SELECT safety_code FROM campaign_inspiration_lines
         WHERE campaign_session_id = $1
         UNION
         SELECT safety_code FROM campaign_inspiration_veils
         WHERE campaign_session_id = $1
         UNION
         SELECT safety_code FROM campaign_inspiration_excluded_topics
         WHERE campaign_session_id = $1
         ORDER BY 1",
    )
    .bind(campaign_session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .collect();
    let participants = sqlx::query_scalar::<_, String>(
        "SELECT participant_id FROM campaign_inspiration_excluded_participants
         WHERE campaign_session_id = $1 ORDER BY participant_id",
    )
    .bind(campaign_session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .collect();
    Ok((safety_codes, participants))
}

async fn source_is_vetoed(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    source: &StoredSource,
) -> Result<bool, PrivateInspirationError> {
    sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM private_inspiration_vetoes
           WHERE campaign_session_id = $1 AND state = 'active'
             AND (
               scope_kind = 'campaign'
               OR (scope_kind = 'category' AND category_id = $2)
               OR (scope_kind = 'source_version' AND source_id = $3
                   AND source_version = $4 AND source_digest = $5)
             )
         )",
    )
    .bind(campaign_session_id)
    .bind(source.projection.category_id.as_str())
    .bind(source.projection.source_id.as_str())
    .bind(inspiration_to_i64(source.projection.source_version)?)
    .bind(source.projection.source_digest.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(db)
}

async fn insert_owner_veto(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    scope: &InspirationVetoScope,
    presentation_id: &str,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let veto_id = internal_id("inspiration-veto")?;
    let (scope_kind, category_id, source_id, source_version, source_digest) = match scope {
        InspirationVetoScope::Campaign => ("campaign", None, None, None, None),
        InspirationVetoScope::Category { category_id } => {
            ("category", Some(category_id.as_str()), None, None, None)
        }
        InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            source_digest,
        } => (
            "source_version",
            None,
            Some(source_id.as_str()),
            Some(inspiration_to_i64(*source_version)?),
            Some(source_digest.as_str()),
        ),
    };
    sqlx::query(
        "INSERT INTO private_inspiration_vetoes
         (veto_id, schema_version, campaign_session_id, participant_id,
          actor_kind, scope_kind, category_id, source_id, source_version,
          source_digest, veto_code, created_at_epoch)
         VALUES ($1, $2, $3, NULL, 'campaign_owner', $4, $5, $6, $7, $8,
                 'safety_veto', $9)",
    )
    .bind(veto_id.as_str())
    .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
    .bind(campaign_session_id)
    .bind(scope_kind)
    .bind(category_id)
    .bind(source_id)
    .bind(source_version)
    .bind(source_digest)
    .bind(inspiration_to_i64(now)?)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    insert_privacy_audit(
        transaction,
        Some(campaign_session_id),
        "owner_veto_applied",
        "veto",
        veto_id.as_str(),
        Some(presentation_id),
        "applied",
        now,
    )
    .await
}

async fn cancel_source_pending_work(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    source_id: &str,
    source_version: u64,
    now: u64,
) -> Result<Vec<OpaqueInspirationId>, PrivateInspirationError> {
    let rows = sqlx::query(
        "UPDATE private_inspiration_derived_work
         SET state = 'cancellation_requested', cancellation_requested_at_epoch = $4
         WHERE campaign_session_id = $1 AND source_id = $2
           AND source_version = $3 AND state = 'pending'
         RETURNING work_id",
    )
    .bind(campaign_session_id)
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .bind(inspiration_to_i64(now)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    audit_cancelled_work(transaction, campaign_session_id, rows, now).await
}

async fn cancel_vetoed_pending_work(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    scope: &InspirationVetoScope,
    now: u64,
) -> Result<Vec<OpaqueInspirationId>, PrivateInspirationError> {
    let rows = match scope {
        InspirationVetoScope::Campaign => sqlx::query(
            "UPDATE private_inspiration_derived_work
                 SET state = 'cancellation_requested', cancellation_requested_at_epoch = $2
                 WHERE campaign_session_id = $1 AND state = 'pending'
                 RETURNING work_id",
        )
        .bind(campaign_session_id)
        .bind(inspiration_to_i64(now)?)
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?,
        InspirationVetoScope::Category { category_id } => sqlx::query(
            "UPDATE private_inspiration_derived_work AS work
                 SET state = 'cancellation_requested', cancellation_requested_at_epoch = $3
                 FROM private_inspiration_sources AS source
                 WHERE work.campaign_session_id = $1 AND work.state = 'pending'
                   AND source.source_id = work.source_id
                   AND source.source_version = work.source_version
                   AND source.category_id = $2
                 RETURNING work.work_id",
        )
        .bind(campaign_session_id)
        .bind(category_id.as_str())
        .bind(inspiration_to_i64(now)?)
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?,
        InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            ..
        } => sqlx::query(
            "UPDATE private_inspiration_derived_work
                 SET state = 'cancellation_requested', cancellation_requested_at_epoch = $4
                 WHERE campaign_session_id = $1 AND source_id = $2
                   AND source_version = $3 AND state = 'pending'
                 RETURNING work_id",
        )
        .bind(campaign_session_id)
        .bind(source_id.as_str())
        .bind(inspiration_to_i64(*source_version)?)
        .bind(inspiration_to_i64(now)?)
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?,
    };
    audit_cancelled_work(transaction, campaign_session_id, rows, now).await
}

async fn audit_cancelled_work(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    rows: Vec<PgRow>,
    now: u64,
) -> Result<Vec<OpaqueInspirationId>, PrivateInspirationError> {
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        let id = OpaqueInspirationId::new(row.try_get::<String, _>("work_id").map_err(db)?)?;
        insert_privacy_audit(
            transaction,
            Some(campaign_session_id),
            "derived_work_cancel_requested",
            "derived_work",
            id.as_str(),
            None,
            "cancel_requested",
            now,
        )
        .await?;
        ids.push(id);
    }
    ids.sort();
    Ok(ids)
}

async fn apply_source_completed_work_policy(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    source_id: &str,
    source_version: u64,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let rows = sqlx::query(
        "SELECT work_id, artifact_policy, completed_artifact_id
         FROM private_inspiration_derived_work
         WHERE campaign_session_id = $1 AND source_id = $2
           AND source_version = $3 AND state IN ('completed', 'redacted')
         ORDER BY work_id FOR UPDATE",
    )
    .bind(campaign_session_id)
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    apply_completed_work_policy(transaction, campaign_session_id, rows, now).await
}

async fn apply_campaign_completed_work_policy(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let rows = sqlx::query(
        "SELECT work_id, artifact_policy, completed_artifact_id
         FROM private_inspiration_derived_work
         WHERE campaign_session_id = $1 AND state IN ('completed', 'redacted')
         ORDER BY work_id FOR UPDATE",
    )
    .bind(campaign_session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    apply_completed_work_policy(transaction, campaign_session_id, rows, now).await
}

async fn apply_vetoed_completed_work_policy(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    scope: &InspirationVetoScope,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let rows = match scope {
        InspirationVetoScope::Campaign => sqlx::query(
            "SELECT work_id, artifact_policy, completed_artifact_id
                 FROM private_inspiration_derived_work
                 WHERE campaign_session_id = $1
                   AND state IN ('completed', 'redacted')
                 ORDER BY work_id FOR UPDATE",
        )
        .bind(campaign_session_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?,
        InspirationVetoScope::Category { category_id } => sqlx::query(
            "SELECT work.work_id, work.artifact_policy,
                        work.completed_artifact_id
                 FROM private_inspiration_derived_work AS work
                 JOIN private_inspiration_sources AS source
                   ON source.source_id = work.source_id
                  AND source.source_version = work.source_version
                 WHERE work.campaign_session_id = $1
                   AND work.state IN ('completed', 'redacted')
                   AND source.category_id = $2
                 ORDER BY work.work_id FOR UPDATE OF work",
        )
        .bind(campaign_session_id)
        .bind(category_id.as_str())
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?,
        InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            ..
        } => sqlx::query(
            "SELECT work_id, artifact_policy, completed_artifact_id
                 FROM private_inspiration_derived_work
                 WHERE campaign_session_id = $1 AND source_id = $2
                   AND source_version = $3
                   AND state IN ('completed', 'redacted')
                 ORDER BY work_id FOR UPDATE",
        )
        .bind(campaign_session_id)
        .bind(source_id.as_str())
        .bind(inspiration_to_i64(*source_version)?)
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)?,
    };
    apply_completed_work_policy(transaction, campaign_session_id, rows, now).await
}

async fn apply_completed_work_policy(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    rows: Vec<PgRow>,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    for row in rows {
        let work_id = OpaqueInspirationId::new(row.try_get::<String, _>("work_id").map_err(db)?)?;
        let policy = DerivedArtifactPolicy::parse(
            &row.try_get::<String, _>("artifact_policy").map_err(db)?,
        )?;
        let artifact_id: String = row
            .try_get::<Option<String>, _>("completed_artifact_id")
            .map_err(db)?
            .ok_or_else(|| invalid("completed_work_artifact_missing"))?;
        if !manchester_dnd_core::is_valid_opaque_id(&artifact_id) {
            return Err(invalid("completed_work_artifact_invalid"));
        }

        let (state, retained_artifact_id, operation) = match policy {
            DerivedArtifactPolicy::RedactDerived => {
                let redacted = sqlx::query(
                    "UPDATE generated_text_presentations
                     SET body = $3, privacy_state = 'redacted',
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = $1 AND private_inspiration_work_id = $2",
                )
                .bind(&artifact_id)
                .bind(work_id.as_str())
                .bind(PRIVATE_INSPIRATION_REDACTION_BODY)
                .execute(&mut **transaction)
                .await
                .map_err(db)?;
                if redacted.rows_affected() == 1 {
                    (
                        "redacted",
                        Some(artifact_id.as_str()),
                        "derived_work_redacted",
                    )
                } else {
                    ("deleted", None, "derived_work_deleted")
                }
            }
            DerivedArtifactPolicy::DeleteDerived | DerivedArtifactPolicy::RetainMinimalAudit => {
                sqlx::query(
                    "DELETE FROM generated_text_presentations
                     WHERE id = $1 AND private_inspiration_work_id = $2",
                )
                .bind(&artifact_id)
                .bind(work_id.as_str())
                .execute(&mut **transaction)
                .await
                .map_err(db)?;
                ("deleted", None, "derived_work_deleted")
            }
        };
        sqlx::query(
            "UPDATE private_inspiration_derived_work
             SET state = $2, completed_artifact_id = $3
             WHERE work_id = $1 AND state IN ('completed', 'redacted')",
        )
        .bind(work_id.as_str())
        .bind(state)
        .bind(retained_artifact_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
        insert_privacy_audit(
            transaction,
            Some(campaign_session_id),
            operation,
            "derived_work",
            work_id.as_str(),
            Some(&artifact_id),
            "applied",
            now,
        )
        .await?;
    }
    Ok(())
}

const fn artifact_policy_rank(policy: DerivedArtifactPolicy) -> u8 {
    match policy {
        DerivedArtifactPolicy::DeleteDerived => 3,
        DerivedArtifactPolicy::RedactDerived => 2,
        DerivedArtifactPolicy::RetainMinimalAudit => 1,
    }
}

async fn load_source_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    source_id: &str,
    source_version: u64,
) -> Result<Option<StoredSource>, PrivateInspirationError> {
    let row = sqlx::query(
        "SELECT source_id, source_version, source_digest, schema_version,
                category_id, review_state, q11_screened, expires_at_epoch
         FROM private_inspiration_sources
         WHERE source_id = $1 AND source_version = $2 FOR UPDATE",
    )
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let participants: BTreeSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT participant_id FROM private_inspiration_source_participants
         WHERE source_id = $1 AND source_version = $2 ORDER BY participant_id",
    )
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .collect();
    let sensitivities: BTreeSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT sensitivity_code FROM private_inspiration_source_sensitivities
         WHERE source_id = $1 AND source_version = $2 ORDER BY sensitivity_code",
    )
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .collect();
    let media = sqlx::query_scalar::<_, String>(
        "SELECT media FROM private_inspiration_source_media
         WHERE source_id = $1 AND source_version = $2 ORDER BY media",
    )
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .map(|value| InspirationMedia::parse(&value))
    .collect::<Result<BTreeSet<_>, _>>()?;
    let theme_pack_ids = sqlx::query_scalar::<_, String>(
        "SELECT theme_pack_id FROM private_inspiration_source_themes
         WHERE source_id = $1 AND source_version = $2 ORDER BY theme_pack_id",
    )
    .bind(source_id)
    .bind(inspiration_to_i64(source_version)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let projection = source_projection_from_row(
        row,
        participants.len(),
        sensitivities.len(),
        media,
        &theme_pack_ids,
    )?;
    Ok(Some(StoredSource {
        projection,
        participants,
        sensitivities,
        theme_pack_ids,
    }))
}

fn source_projection_from_row(
    row: PgRow,
    participant_count: usize,
    sensitivity_count: usize,
    eligible_media: BTreeSet<InspirationMedia>,
    theme_pack_ids: &BTreeSet<String>,
) -> Result<SourceVersionProjection, PrivateInspirationError> {
    let schema = to_u64(row.try_get("schema_version").map_err(db)?)?;
    if schema != u64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION) {
        return Err(invalid("stored_source_schema"));
    }
    Ok(SourceVersionProjection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        source_id: OpaqueInspirationId::new(row.try_get::<String, _>("source_id").map_err(db)?)?,
        source_version: to_u64(row.try_get("source_version").map_err(db)?)?,
        source_digest: stored_digest(row.try_get("source_digest").map_err(db)?)?,
        category_id: OpaqueInspirationId::new(
            row.try_get::<String, _>("category_id").map_err(db)?,
        )?,
        review_state: SourceReviewState::parse(
            &row.try_get::<String, _>("review_state").map_err(db)?,
        )?,
        q11_screened: row.try_get("q11_screened").map_err(db)?,
        participant_count: u32::try_from(participant_count)
            .map_err(|_| invalid("stored_participant_count"))?,
        sensitivity_count: u32::try_from(sensitivity_count)
            .map_err(|_| invalid("stored_sensitivity_count"))?,
        eligible_media,
        eligible_theme_pack_ids: theme_pack_ids
            .iter()
            .map(|theme_pack_id| OpaqueInspirationId::new(theme_pack_id.clone()))
            .collect::<Result<_, _>>()?,
        expires_at_epoch: row
            .try_get::<Option<i64>, _>("expires_at_epoch")
            .map_err(db)?
            .map(to_u64)
            .transpose()?,
    })
}

async fn load_receipt<T: DeserializeOwned>(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    idempotency_key: &str,
    operation_code: &str,
    request_fingerprint: &Sha256Digest,
) -> Result<Option<T>, PrivateInspirationError> {
    let row = sqlx::query(
        "SELECT operation_code, request_fingerprint, response_json
         FROM private_inspiration_command_receipts
         WHERE campaign_session_id = $1 AND idempotency_key = $2",
    )
    .bind(campaign_session_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_operation: String = row.try_get("operation_code").map_err(db)?;
    let stored_fingerprint: String = row.try_get("request_fingerprint").map_err(db)?;
    if stored_operation != operation_code || stored_fingerprint != request_fingerprint.as_str() {
        return Err(PrivateInspirationError::IdempotencyConflict);
    }
    let response: String = row.try_get("response_json").map_err(db)?;
    serde_json::from_str(&response)
        .map(Some)
        .map_err(|_| invalid("stored_receipt_response"))
}

async fn insert_receipt<T: Serialize>(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    idempotency_key: &str,
    operation_code: &str,
    request_fingerprint: &Sha256Digest,
    response: &T,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let response_json =
        serde_json::to_string(response).map_err(PrivateInspirationError::Serialization)?;
    sqlx::query(
        "INSERT INTO private_inspiration_command_receipts
         (campaign_session_id, idempotency_key, operation_code,
          request_fingerprint, response_json, created_at_epoch)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(campaign_session_id)
    .bind(idempotency_key)
    .bind(operation_code)
    .bind(request_fingerprint.as_str())
    .bind(response_json)
    .bind(inspiration_to_i64(now)?)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_privacy_audit(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: Option<&str>,
    operation_code: &str,
    subject_kind: &str,
    subject_id: &str,
    secondary_id: Option<&str>,
    result_code: &str,
    now: u64,
) -> Result<(), PrivateInspirationError> {
    let audit_id = internal_id("privacy-audit")?;
    sqlx::query(
        "INSERT INTO private_inspiration_privacy_audits
         (audit_id, schema_version, campaign_session_id, operation_code,
          subject_kind, subject_id, secondary_id, result_code, occurred_at_epoch)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(audit_id.as_str())
    .bind(i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION))
    .bind(campaign_session_id)
    .bind(operation_code)
    .bind(subject_kind)
    .bind(subject_id)
    .bind(secondary_id)
    .bind(result_code)
    .bind(inspiration_to_i64(now)?)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(())
}

async fn cancel_campaign_pending_work(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    now: u64,
) -> Result<Vec<OpaqueInspirationId>, PrivateInspirationError> {
    let rows = sqlx::query(
        "UPDATE private_inspiration_derived_work
         SET state = 'cancellation_requested', cancellation_requested_at_epoch = $2
         WHERE campaign_session_id = $1 AND state = 'pending'
         RETURNING work_id",
    )
    .bind(campaign_session_id)
    .bind(inspiration_to_i64(now)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(db)?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        let id = OpaqueInspirationId::new(row.try_get::<String, _>("work_id").map_err(db)?)?;
        insert_privacy_audit(
            transaction,
            Some(campaign_session_id),
            "derived_work_cancel_requested",
            "derived_work",
            id.as_str(),
            None,
            "cancel_requested",
            now,
        )
        .await?;
        ids.push(id);
    }
    ids.sort();
    Ok(ids)
}

fn stored_digest(value: String) -> Result<Sha256Digest, PrivateInspirationError> {
    Sha256Digest::new(value).map_err(|_| invalid("stored_digest"))
}

fn db(error: sqlx::Error) -> PrivateInspirationError {
    RepositoryError::Database(error).into()
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use manchester_dnd_core::{
        AbilityScores, CharacterDraft, EventActor, RULESET, SESSION_SCHEMA_VERSION,
        hero::{EMBERLINE_THEME_PACK_ID, RAINBOUND_THEME_PACK_ID, ThemeId},
    };
    use sqlx::PgPool;
    use tempfile::tempdir;

    use crate::{
        campaign_pins::CampaignPinRuntime,
        events::EventPromptLoader,
        inspiration::{
            ApplyInspirationVetoCommand, ConfigureCampaignInspirationCommand,
            ConsentRevocationCode, DeleteParticipantPrivateDataCommand, DerivedArtifactPolicy,
            DerivedWorkKind, GrantConsentCommand, InspirationAudience, InspirationMedia,
            InspirationTransformation, InspirationVetoCode, InspirationVetoScope, OpaqueOperatorId,
            ParticipantVerificationMethod, PrivateInspirationApplicationService,
            PurgeExpiredParticipantDeletionTombstonesCommand, RegisterDerivedWorkCommand,
            RegisterSourceVersionCommand, RequestInspirationSelectionCommand,
            ReviewSourceVersionCommand, RevokeConsentCommand, SafetySetupEvidence,
            SourceReviewState, VerifyParticipantCommand,
        },
        repository::{
            GeneratedTextPresentationSource, MIGRATOR, NewGeneratedTextPresentation,
            jobs::{
                GenerationClaim, GenerationPurpose, GenerationUsage, NewGenerationJob,
                SuccessRetention,
            },
        },
        seed::SeedVault,
    };

    use super::*;

    const CAMPAIGN_ID: &str = "campaign:private-inspiration-test";
    const CHARACTER_ID: &str = "character:private-inspiration-test";
    const PARTICIPANT_ID: &str = "participant:11111111111111111111111111111111";
    const OPERATOR_ID: &str = "operator:22222222222222222222222222222222";
    const NOW: u64 = 1_000;

    fn opaque(value: &str) -> OpaqueInspirationId {
        OpaqueInspirationId::new(value).expect("valid opaque id")
    }

    fn operator() -> OpaqueOperatorId {
        OpaqueOperatorId::new(OPERATOR_ID).expect("valid operator id")
    }

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; 32])
    }

    async fn seed_safe_campaign(pool: &PgPool) {
        let session = SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: CAMPAIGN_ID.to_owned(),
            ruleset: RULESET,
            title: "Private rain over Manchester".to_owned(),
            status: SessionStatus::Active,
            character_ids: vec![CHARACTER_ID.to_owned()],
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            last_event_sequence: 1,
        };
        session.validate().expect("valid session");
        let character = CharacterDraft {
            id: CHARACTER_ID.to_owned(),
            name: "The Canal Warden".to_owned(),
            theme: "rainbound occultist".to_owned(),
            ability_scores: AbilityScores::new(12, 14, 10, 16, 13, 8).expect("valid scores"),
            experience_points: 0,
            current_hit_points: 8,
            maximum_hit_points: 8,
        }
        .build()
        .expect("valid character");
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: CAMPAIGN_ID.to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 2,
            actor: EventActor::AiGameMaster,
            payload: SessionEventPayload::GmNarration {
                text: "The rain stills at a safe scene boundary.".to_owned(),
                image_prompt: None,
                source_prompt_id: None,
            },
        };
        event.validate().expect("valid event");
        sqlx::query(
            "INSERT INTO campaign_sessions
             (id, schema_version, revision, payload_json)
             VALUES ($1, $2, 2, $3::jsonb)",
        )
        .bind(CAMPAIGN_ID)
        .bind(i64::from(SESSION_SCHEMA_VERSION))
        .bind(serde_json::to_string(&session).unwrap())
        .execute(pool)
        .await
        .unwrap();
        let pins = CampaignPinRuntime::bundled_for_tests()
            .pins_for_theme(ThemeId::RainboundBorough)
            .expect("bundled rainbound pins");
        sqlx::query(
            "INSERT INTO campaign_content_pins
             (campaign_session_id, schema_version, seal_reason, payload_json)
             VALUES ($1, 1, 'selected_theme', $2::jsonb)",
        )
        .bind(CAMPAIGN_ID)
        .bind(serde_json::to_string(&pins).unwrap())
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO characters
             (id, campaign_session_id, schema_version, revision, payload_json)
             VALUES ($1, $2, 1, 1, $3::jsonb)",
        )
        .bind(CHARACTER_ID)
        .bind(CAMPAIGN_ID)
        .bind(serde_json::to_string(&character).unwrap())
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, actor_id, schema_version, payload_json)
             VALUES ('turn:private-inspiration-test', $1, 1, NULL, $2, $3::jsonb)",
        )
        .bind(CAMPAIGN_ID)
        .bind(i64::from(SESSION_SCHEMA_VERSION))
        .bind(serde_json::to_string(&event).unwrap())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn complete_text_work(
        repository: &PostgresRepository,
        work_id: &str,
        suffix: &str,
    ) -> String {
        let job_id = format!("private-presentation-job:{suffix}");
        repository
            .enqueue_generation_job(&NewGenerationJob {
                id: job_id.clone(),
                campaign_session_id: CAMPAIGN_ID.to_owned(),
                origin_turn_id: Some("turn:private-inspiration-test".to_owned()),
                origin_campaign_revision: 2,
                purpose: GenerationPurpose::Narration,
                idempotency_key: format!("private-presentation-job-key:{suffix}"),
                input_digest: digest(10),
                prompt_digest: digest(11),
                policy_digest: digest(12),
                config_digest: digest(13),
                correlation_id: Some(format!("correlation:{suffix}")),
                max_attempts: 1,
                success_retention: SuccessRetention::UnselectedPresentation30Days,
                governance: None,
            })
            .await
            .expect("private narration job should enqueue");
        let claimed = repository
            .claim_generation_job_by_id(
                CAMPAIGN_ID,
                &job_id,
                &GenerationClaim {
                    worker_id: format!("worker:{suffix}"),
                    provider: "deterministic-fake".to_owned(),
                    model: "fake-v1".to_owned(),
                    lease_duration: Duration::from_secs(60),
                },
            )
            .await
            .expect("private narration claim should succeed")
            .expect("private narration job should be ready");
        let presentation_id = format!("private-presentation:{suffix}");
        repository
            .finish_generation_with_text_presentation(
                &claimed.lease,
                &NewGeneratedTextPresentation {
                    id: presentation_id.clone(),
                    campaign_session_id: CAMPAIGN_ID.to_owned(),
                    origin_turn_id: "turn:private-inspiration-test".to_owned(),
                    generation_job_id: claimed.job.id,
                    generation_attempt_id: claimed.attempt.id,
                    client_idempotency_key: format!("private-presentation-client:{suffix}"),
                    source: GeneratedTextPresentationSource::Provider,
                    body: format!("A high-distance fictional scene for {suffix}."),
                    config_digest: digest(13),
                    prompt_digest: digest(11),
                    policy_digest: digest(12),
                    output_digest: digest(14),
                    private_inspiration_work_id: Some(work_id.to_owned()),
                },
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("private presentation and work should complete atomically");
        presentation_id
    }

    fn load_prompt() -> EventPrompt {
        let root = tempdir().expect("temporary prompt root");
        std::fs::write(
            root.path().join("quiet-delay.md"),
            format!(
                r#"---
{{
  "id": "quiet-delay",
  "title": "The Clockwork Carriage Pauses",
  "weight": 1,
  "minimum_level": 1,
  "cooldown_turns": 2,
  "sensitivity_tags": ["general"],
  "participant_aliases": ["{PARTICIPANT_ID}"],
  "enabled": true
}}
---

## Inspiration

A harmless delay changed the rhythm of a journey.
"#,
            ),
        )
        .expect("write prompt");
        let review = EventPromptLoader
            .load_dir_reviewed(root.path())
            .expect("review prompt tree");
        assert!(review.quarantined_sources.is_empty());
        review
            .approved_prompts
            .into_iter()
            .next()
            .expect("one approved prompt")
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn consent_selection_revocation_veto_and_export_are_atomic_and_redacted(pool: PgPool) {
        seed_safe_campaign(&pool).await;
        let repository = PostgresRepository::from_pool(pool.clone());
        let service = PrivateInspirationApplicationService::with_clock(
            repository.clone(),
            true,
            Arc::new(SeedVault::from_key([7; 32])),
            || NOW,
        );
        let campaign = opaque(CAMPAIGN_ID);
        let participant = opaque(PARTICIPANT_ID);
        let sensitivity = opaque("general");
        let safety = SafetySetupEvidence {
            evidence_digest: digest(1),
            reviewer_id: operator(),
            tone: CampaignInspirationTone::GothicAdventure,
            allowed_sensitivity_codes: BTreeSet::from([sensitivity.clone()]),
            line_codes: BTreeSet::new(),
            veil_codes: BTreeSet::new(),
            excluded_topic_codes: BTreeSet::new(),
            excluded_participant_ids: BTreeSet::new(),
        };
        let disabled = service
            .configure_campaign(ConfigureCampaignInspirationCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("settings:prepare"),
                expected_revision: 0,
                enabled: false,
                safety_setup: Some(safety.clone()),
            })
            .await
            .expect("prepare settings");
        assert_eq!(disabled.revision, 1);
        let enabled_command = ConfigureCampaignInspirationCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: campaign.clone(),
            idempotency_key: opaque("settings:enable"),
            expected_revision: 1,
            enabled: true,
            safety_setup: Some(safety.clone()),
        };
        let enabled = service
            .configure_campaign(enabled_command.clone())
            .await
            .expect("enable settings");
        assert_eq!(enabled.revision, 2);
        let diagnostic_command = RecordRestrictedDiagnosticAccessCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            idempotency_key: opaque("restricted-access:restore-drill"),
            campaign_session_id: Some(campaign.clone()),
            operator_id: operator(),
            access_kind: RestrictedDiagnosticAccessKind::SourceBackup,
            purpose: RestrictedDiagnosticPurpose::RestoreDrill,
            subject_id: opaque("source-vault:restore-drill"),
            evidence_digest: digest(18),
            decision: RestrictedDiagnosticDecision::Allowed,
        };
        let diagnostic = service
            .record_restricted_diagnostic_access(diagnostic_command.clone())
            .await
            .expect("restricted access is explicitly audited");
        assert_eq!(
            service
                .record_restricted_diagnostic_access(diagnostic_command)
                .await
                .expect("restricted access audit replays exactly"),
            diagnostic
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM private_inspiration_restricted_access_audits
                 WHERE idempotency_key = 'restricted-access:restore-drill'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            service
                .configure_campaign(enabled_command)
                .await
                .expect("exact setting replay"),
            enabled
        );
        assert_eq!(
            service
                .campaign_status(&campaign)
                .await
                .expect("load inspiration status")
                .settings,
            Some(enabled.clone())
        );

        service
            .verify_participant(VerifyParticipantCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("participant:verify-command"),
                participant_id: participant.clone(),
                method: ParticipantVerificationMethod::ParticipantSignedConfirmation,
                evidence_digest: digest(2),
                verifier_id: operator(),
            })
            .await
            .expect("verify participant");
        let prompt = load_prompt();
        let source_id = opaque(prompt.privacy_source_id());
        service
            .register_source_version(RegisterSourceVersionCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("source:register"),
                source_id: source_id.clone(),
                source_version: 1,
                source_digest: prompt.source_digest().clone(),
                category_id: opaque("category:journey"),
                owner_participant_id: participant.clone(),
                participant_ids: BTreeSet::from([participant.clone()]),
                sensitivity_codes: BTreeSet::from([sensitivity.clone()]),
                eligible_media: BTreeSet::from([InspirationMedia::Text]),
                eligible_theme_pack_ids: BTreeSet::from([opaque(RAINBOUND_THEME_PACK_ID)]),
                provenance_digest: digest(3),
                expires_at_epoch: Some(NOW + 10_000),
                runtime_prompt: prompt.runtime_projection(),
            })
            .await
            .expect("register source");
        service
            .review_source_version(ReviewSourceVersionCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("source:review"),
                source_id: source_id.clone(),
                source_version: 1,
                source_digest: prompt.source_digest().clone(),
                decision: SourceReviewState::Approved,
                q11_screened: true,
                reviewer_id: operator(),
                review_evidence_digest: digest(4),
            })
            .await
            .expect("approve source");

        let grant = |key: &str| GrantConsentCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: campaign.clone(),
            idempotency_key: opaque(key),
            source_id: source_id.clone(),
            source_version: 1,
            source_digest: prompt.source_digest().clone(),
            participant_id: participant.clone(),
            audience: InspirationAudience::PrivateCampaign,
            media: InspirationMedia::Text,
            transformation: InspirationTransformation::HighFictionDistanceV1,
            sensitivity_codes: BTreeSet::from([sensitivity.clone()]),
            expires_at_epoch: NOW + 5_000,
            reviewer_id: operator(),
            participant_confirmation_digest: digest(5),
            review_evidence_digest: digest(6),
            artifact_policy: DerivedArtifactPolicy::RedactDerived,
        };
        let first_grant = service
            .grant_consent(grant("consent:first"))
            .await
            .expect("grant consent");
        let paused = service
            .set_campaign_pause(SetCampaignInspirationPauseCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("settings:pause"),
                expected_revision: 2,
                paused: true,
            })
            .await
            .expect("pause private generation");
        assert!(paused.generation_paused);
        let paused_selection = service
            .request_selection(RequestInspirationSelectionCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("selection:paused"),
                expected_campaign_revision: 2,
                expected_settings_revision: 3,
                audience: InspirationAudience::PrivateCampaign,
                media: InspirationMedia::Text,
            })
            .await
            .expect("pause returns an audited no-selection");
        assert_eq!(
            paused_selection.outcome.durable_no_selection_reason,
            Some(DurableNoSelectionReason::CampaignPaused)
        );
        assert_eq!(paused_selection.outcome.audit.cursor_after, 0);
        let resumed = service
            .set_campaign_pause(SetCampaignInspirationPauseCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("settings:resume"),
                expected_revision: 3,
                paused: false,
            })
            .await
            .expect("resume private generation");
        assert_eq!(resumed.revision, 4);
        assert!(!resumed.generation_paused);
        let mut excluded_safety = safety.clone();
        excluded_safety
            .excluded_participant_ids
            .insert(participant.clone());
        let excluded = service
            .configure_campaign(ConfigureCampaignInspirationCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("settings:exclude-participant"),
                expected_revision: 4,
                enabled: true,
                safety_setup: Some(excluded_safety),
            })
            .await
            .expect("participant exclusion is persisted");
        assert_eq!(excluded.excluded_participant_count, 1);
        let excluded_selection = service
            .request_selection(RequestInspirationSelectionCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("selection:participant-excluded"),
                expected_campaign_revision: 2,
                expected_settings_revision: 5,
                audience: InspirationAudience::PrivateCampaign,
                media: InspirationMedia::Text,
            })
            .await
            .expect("excluded participant produces no selection");
        assert!(excluded_selection.prompt.is_none());
        assert_eq!(excluded_selection.outcome.audit.cursor_after, 0);
        let restored = service
            .configure_campaign(ConfigureCampaignInspirationCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("settings:restore-safety"),
                expected_revision: 5,
                enabled: true,
                safety_setup: Some(safety),
            })
            .await
            .expect("restore eligible safety setup");
        assert_eq!(restored.revision, 6);
        assert_eq!(restored.excluded_participant_count, 0);
        sqlx::query(
            "UPDATE private_inspiration_source_themes
             SET theme_pack_id = $3 WHERE source_id = $1 AND source_version = $2",
        )
        .bind(source_id.as_str())
        .bind(1_i64)
        .bind(EMBERLINE_THEME_PACK_ID)
        .execute(&pool)
        .await
        .unwrap();
        let theme_mismatch = service
            .request_selection(RequestInspirationSelectionCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("selection:theme-mismatch"),
                expected_campaign_revision: 2,
                expected_settings_revision: 6,
                audience: InspirationAudience::PrivateCampaign,
                media: InspirationMedia::Text,
            })
            .await
            .expect("wrong-theme source produces no selection");
        assert!(theme_mismatch.prompt.is_none());
        assert_eq!(theme_mismatch.outcome.audit.cursor_after, 0);
        sqlx::query(
            "UPDATE private_inspiration_source_themes
             SET theme_pack_id = $3 WHERE source_id = $1 AND source_version = $2",
        )
        .bind(source_id.as_str())
        .bind(1_i64)
        .bind(RAINBOUND_THEME_PACK_ID)
        .execute(&pool)
        .await
        .unwrap();
        let selection_command = RequestInspirationSelectionCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: campaign.clone(),
            idempotency_key: opaque("selection:first"),
            expected_campaign_revision: 2,
            expected_settings_revision: 6,
            audience: InspirationAudience::PrivateCampaign,
            media: InspirationMedia::Text,
        };
        let selected = service
            .request_selection(selection_command.clone())
            .await
            .expect("reserve selection");
        assert_eq!(
            selected
                .prompt
                .as_ref()
                .map(|value| value.privacy_source_id()),
            Some(source_id.as_str())
        );
        assert_eq!(selected.outcome.source_version, Some(1));
        assert_eq!(selected.outcome.audit.cursor_after, 1);
        assert_eq!(
            service
                .request_selection(selection_command.clone())
                .await
                .expect("exact selection replay")
                .outcome,
            selected.outcome
        );

        assert!(matches!(
            service
                .register_derived_work(RegisterDerivedWorkCommand {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    campaign_session_id: campaign.clone(),
                    idempotency_key: opaque("derived:image-forbidden-command"),
                    work_id: opaque("derived:image-forbidden"),
                    selection_id: selected.outcome.selection_id.clone(),
                    kind: DerivedWorkKind::Image,
                    artifact_policy: DerivedArtifactPolicy::DeleteDerived,
                })
                .await,
            Err(PrivateInspirationError::ScopeDenied)
        ));

        service
            .register_derived_work(RegisterDerivedWorkCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("derived:first-command"),
                work_id: opaque("derived:first"),
                selection_id: selected.outcome.selection_id.clone(),
                kind: DerivedWorkKind::Text,
                artifact_policy: DerivedArtifactPolicy::DeleteDerived,
            })
            .await
            .expect("register derived work");
        service
            .register_derived_work(RegisterDerivedWorkCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("derived:pending-command"),
                work_id: opaque("derived:pending"),
                selection_id: selected.outcome.selection_id.clone(),
                kind: DerivedWorkKind::Text,
                artifact_policy: DerivedArtifactPolicy::DeleteDerived,
            })
            .await
            .expect("register pending derived work");
        let deleted_presentation =
            complete_text_work(&repository, "derived:first", "delete-on-revoke").await;
        let revoked = service
            .revoke_consent(RevokeConsentCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("consent:revoke-command"),
                grant_id: first_grant.grant_id.clone(),
                requester_participant_id: participant.clone(),
                reason: ConsentRevocationCode::ParticipantRevoked,
            })
            .await
            .expect("revoke consent");
        assert_eq!(
            revoked.pending_work_cancellation_ids,
            [opaque("derived:pending")]
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM private_inspiration_derived_work WHERE work_id = 'derived:first'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "deleted"
        );
        assert!(
            !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM generated_text_presentations WHERE id = $1)",
            )
            .bind(&deleted_presentation)
            .fetch_one(&pool)
            .await
            .unwrap()
        );
        assert!(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM generated_text_presentation_receipts WHERE presentation_id = $1)",
        )
        .bind(&deleted_presentation)
        .fetch_one(&pool)
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM private_inspiration_derived_work WHERE work_id = 'derived:pending'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "cancellation_requested"
        );

        service
            .grant_consent(grant("consent:second"))
            .await
            .expect("grant consent again after revocation");
        service
            .register_derived_work(RegisterDerivedWorkCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("derived:second-command"),
                work_id: opaque("derived:second"),
                selection_id: selected.outcome.selection_id.clone(),
                kind: DerivedWorkKind::Text,
                artifact_policy: DerivedArtifactPolicy::RedactDerived,
            })
            .await
            .expect("register second derived work");
        service
            .register_derived_work(RegisterDerivedWorkCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("derived:redacted-command"),
                work_id: opaque("derived:redacted"),
                selection_id: selected.outcome.selection_id.clone(),
                kind: DerivedWorkKind::Text,
                artifact_policy: DerivedArtifactPolicy::RedactDerived,
            })
            .await
            .expect("register redaction-policy work");
        let redacted_presentation =
            complete_text_work(&repository, "derived:redacted", "redact-on-veto").await;
        let killed = service
            .set_global_control(SetGlobalInspirationControlCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                idempotency_key: opaque("global-control:disable"),
                expected_revision: 1,
                generation_disabled: true,
                operator_id: operator(),
                evidence_digest: digest(15),
            })
            .await
            .expect("global kill switch applies without restart");
        assert_eq!(killed.revision, 2);
        assert!(killed.generation_disabled);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM private_inspiration_derived_work WHERE work_id = 'derived:second'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "cancellation_requested"
        );
        let globally_blocked = service
            .request_selection(RequestInspirationSelectionCommand {
                idempotency_key: opaque("selection:global-kill-switch"),
                ..selection_command.clone()
            })
            .await
            .expect("global kill switch returns durable no-selection");
        assert_eq!(
            globally_blocked.outcome.durable_no_selection_reason,
            Some(DurableNoSelectionReason::GlobalKillSwitch)
        );
        assert_eq!(globally_blocked.outcome.audit.cursor_after, 1);
        let restored_global = service
            .set_global_control(SetGlobalInspirationControlCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                idempotency_key: opaque("global-control:enable"),
                expected_revision: 2,
                generation_disabled: false,
                operator_id: operator(),
                evidence_digest: digest(16),
            })
            .await
            .expect("operator can restore generation behind existing gates");
        assert_eq!(restored_global.revision, 3);
        assert!(!restored_global.generation_disabled);
        let (_, veto_transition) = service
            .apply_veto(ApplyInspirationVetoCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("veto:campaign-command"),
                participant_id: participant.clone(),
                scope: InspirationVetoScope::Campaign,
                code: InspirationVetoCode::ParticipantVeto,
            })
            .await
            .expect("apply immediate veto");
        assert_eq!(veto_transition.pending_work_cancellation_ids, []);
        let redacted = sqlx::query(
            "SELECT body, privacy_state FROM generated_text_presentations WHERE id = $1",
        )
        .bind(&redacted_presentation)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            redacted.try_get::<String, _>("body").unwrap(),
            PRIVATE_INSPIRATION_REDACTION_BODY
        );
        assert_eq!(
            redacted.try_get::<String, _>("privacy_state").unwrap(),
            "redacted"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM private_inspiration_derived_work WHERE work_id = 'derived:redacted'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "redacted"
        );
        let no_selection = service
            .request_selection(RequestInspirationSelectionCommand {
                idempotency_key: opaque("selection:after-veto"),
                ..selection_command
            })
            .await
            .expect("veto produces an audited no-selection");
        assert!(no_selection.prompt.is_none());
        assert_eq!(
            no_selection.outcome.durable_no_selection_reason,
            Some(DurableNoSelectionReason::NoEligibleSources)
        );
        assert_eq!(no_selection.outcome.audit.cursor_before, 1);
        assert_eq!(no_selection.outcome.audit.cursor_after, 1);

        let export = service
            .redacted_export(&campaign, &participant)
            .await
            .expect("load participant-scoped export");
        assert_eq!(export.requester_grants.len(), 2);
        assert_eq!(export.sources.len(), 1);
        let json = export.canonical_json().expect("serialize export");
        assert!(!json.contains("A harmless delay"));
        assert!(!json.contains(OPERATOR_ID));
        assert!(!json.contains("confirmation_digest"));
        assert!(!json.contains("evidence_digest"));
        let metrics_debug = format!(
            "{:?}",
            repository
                .generation_metrics_snapshot()
                .await
                .expect("load bounded operational metrics")
        );
        assert!(!metrics_debug.contains("A harmless delay"));
        assert!(!metrics_debug.contains(PARTICIPANT_ID));
        assert!(!metrics_debug.contains(source_id.as_str()));

        let veiled = service
            .apply_presentation_privacy_control(ApplyPresentationPrivacyCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("presentation-control:veil"),
                presentation_id: opaque(&redacted_presentation),
                action: PresentationPrivacyAction::Veil,
            })
            .await
            .expect("veil needs no justification");
        assert!(veiled.presentation_hidden);
        service
            .apply_presentation_privacy_control(ApplyPresentationPrivacyCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("presentation-control:veto-source"),
                presentation_id: opaque(&redacted_presentation),
                action: PresentationPrivacyAction::VetoSource,
            })
            .await
            .expect("owner source veto applies immediately");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM private_inspiration_vetoes
                 WHERE campaign_session_id = $1 AND actor_kind = 'campaign_owner'
                   AND participant_id IS NULL",
            )
            .bind(CAMPAIGN_ID)
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        let report_command = ApplyPresentationPrivacyCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: campaign.clone(),
            idempotency_key: opaque("presentation-control:report"),
            presentation_id: opaque(&redacted_presentation),
            action: PresentationPrivacyAction::Report,
        };
        let reported = service
            .apply_presentation_privacy_control(report_command.clone())
            .await
            .expect("privacy report hides and pauses without report prose");
        assert_eq!(reported.settings_revision, Some(7));
        assert_eq!(
            service
                .apply_presentation_privacy_control(report_command)
                .await
                .expect("exact report replay must not increment twice"),
            reported
        );

        let deletion_command = DeleteParticipantPrivateDataCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: campaign.clone(),
            idempotency_key: opaque("participant:delete-command"),
            participant_id: participant.clone(),
            operator_id: operator(),
            deletion_evidence_digest: digest(17),
            protected_sources_removed: true,
        };
        let deletion = service
            .delete_participant_private_data(deletion_command.clone())
            .await
            .expect("participant deletion quarantines all private data atomically");
        assert_eq!(deletion.revoked_grant_count, 1);
        assert_eq!(deletion.quarantined_source_count, 1);
        assert!(deletion.pending_work_cancellation_ids.is_empty());
        assert_eq!(deletion.affected_completed_artifact_count, 1);
        assert_eq!(
            deletion.tombstone_delete_after_epoch,
            NOW + PARTICIPANT_DELETION_TOMBSTONE_SECONDS
        );
        assert_eq!(
            service
                .delete_participant_private_data(deletion_command)
                .await
                .expect("exact participant deletion replay"),
            deletion
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT verification_state FROM private_inspiration_participants
                 WHERE participant_id = $1",
            )
            .bind(PARTICIPANT_ID)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "revoked"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT review_state FROM private_inspiration_sources
                 WHERE source_id = $1 AND source_version = 1",
            )
            .bind(source_id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap(),
            "quarantined"
        );
        let reverify = service
            .verify_participant(VerifyParticipantCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("participant:reverify-during-tombstone"),
                participant_id: participant.clone(),
                method: ParticipantVerificationMethod::ParticipantSignedConfirmation,
                evidence_digest: digest(18),
                verifier_id: operator(),
            })
            .await;
        assert!(matches!(
            reverify,
            Err(PrivateInspirationError::ScopeDenied)
        ));

        let tombstone_expiry = deletion.tombstone_delete_after_epoch;
        let retention_service = PrivateInspirationApplicationService::with_clock(
            repository.clone(),
            true,
            Arc::new(SeedVault::from_key([7; 32])),
            move || tombstone_expiry,
        );
        let purge_command = PurgeExpiredParticipantDeletionTombstonesCommand {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            idempotency_key: opaque("participant:purge-expired-tombstone"),
            delete_after_epoch_inclusive: tombstone_expiry,
            operator_id: operator(),
            evidence_digest: digest(19),
        };
        let purged = retention_service
            .purge_expired_deletion_tombstones(purge_command.clone())
            .await
            .expect("expired deletion tombstones are purged deterministically");
        assert_eq!(purged.purged_count, 1);
        assert_eq!(
            retention_service
                .purge_expired_deletion_tombstones(purge_command)
                .await
                .expect("exact tombstone purge replay"),
            purged
        );
        retention_service
            .verify_participant(VerifyParticipantCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("participant:reverify-after-retention"),
                participant_id: participant,
                method: ParticipantVerificationMethod::ParticipantSignedConfirmation,
                evidence_digest: digest(20),
                verifier_id: operator(),
            })
            .await
            .expect("fresh verification is possible only after tombstone expiry");

        let disabled = service
            .disable_campaign(DisableCampaignInspirationCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign.clone(),
                idempotency_key: opaque("settings:disable"),
                expected_revision: 7,
            })
            .await
            .expect("disable campaign inspiration");
        assert_eq!(disabled.revision, 8);
        assert!(!disabled.enabled);
    }
}
