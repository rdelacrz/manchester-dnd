//! Durable, owner-visible narration presentation versions.
//!
//! Only a bounded, safety-validated body is retained. Prompt text, player
//! intent, raw provider responses, and credentials have no fields in this
//! store. Operational job metadata may expire independently; immutable IDs and
//! digests are copied into each presentation as provenance snapshots.

use manchester_dnd_core::{
    Sha256Digest,
    encounter::{EncounterCommand, EncounterIntent},
    is_valid_opaque_id,
};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use thiserror::Error;

use super::{
    PostgresRepository,
    governance::record_generation_attempt_usage,
    jobs::{
        GenerationFailureCode, GenerationLease, GenerationPurpose, GenerationUsage, usage_bindings,
        validate_lease,
    },
};

pub const MAX_TEXT_PRESENTATION_VERSIONS: u8 = 3;
pub const TEXT_PRESENTATION_RECEIPT_SCHEMA_VERSION: u16 = 1;
pub const TYPED_INTENT_RECEIPT_SCHEMA_VERSION: u16 = 1;
pub const MAX_TEXT_PRESENTATION_CHARS: usize = 12_000;
const MAX_TEXT_PRESENTATION_BYTES: usize = 48 * 1_024;
pub(crate) const PRIVATE_INSPIRATION_REDACTION_BODY: &str = "Private inspiration removed at a participant request. The committed game mechanics are unchanged.";

#[derive(Debug, Error)]
pub enum TextPresentationStoreError {
    #[error("invalid generated text presentation: {0}")]
    InvalidInput(&'static str),
    #[error("generation job lease is no longer current")]
    LostLease,
    #[error("the committed turn already has its initial narration and two regenerations")]
    VersionLimitReached,
    #[error("generated text presentation idempotency metadata conflicts")]
    IdempotencyConflict,
    #[error("the exact generated text presentation body has expired")]
    ReplayExpired,
    #[error("stored generated text presentation is invalid: {0}")]
    InvalidStoredData(&'static str),
    #[error("generated text presentation numeric value is outside PostgreSQL BIGINT's range")]
    NumericRange,
    #[error("generated text presentation database operation failed")]
    Database(#[source] sqlx::Error),
    #[error("invalid generation completion metadata")]
    Generation(#[from] super::jobs::GenerationJobStoreError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedTextPresentationSource {
    Provider,
    AuthoredFallback,
    EngineAuthored,
}

impl GeneratedTextPresentationSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::AuthoredFallback => "authored_fallback",
            Self::EngineAuthored => "engine_authored",
        }
    }
}

impl std::str::FromStr for GeneratedTextPresentationSource {
    type Err = TextPresentationStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "provider" => Ok(Self::Provider),
            "authored_fallback" => Ok(Self::AuthoredFallback),
            "engine_authored" => Ok(Self::EngineAuthored),
            _ => Err(TextPresentationStoreError::InvalidStoredData(
                "unknown presentation source",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGeneratedTextPresentation {
    pub id: String,
    pub campaign_session_id: String,
    pub origin_turn_id: String,
    pub generation_job_id: String,
    pub generation_attempt_id: String,
    pub client_idempotency_key: String,
    pub source: GeneratedTextPresentationSource,
    pub body: String,
    pub config_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub output_digest: Sha256Digest,
    /// Opaque reservation identity only. No source ID, participant identity,
    /// consent detail, or private body crosses this persistence boundary.
    pub private_inspiration_work_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTextPresentation {
    pub id: String,
    pub campaign_session_id: String,
    pub origin_turn_id: String,
    pub generation_job_id: String,
    pub generation_attempt_id: String,
    pub client_idempotency_key: String,
    pub version: u8,
    pub source: GeneratedTextPresentationSource,
    pub body: String,
    pub config_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub output_digest: Sha256Digest,
    pub private_inspiration_work_id: Option<String>,
    pub privacy_redacted: bool,
    pub selected: bool,
    pub retention_delete_after: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTextPresentationReceipt {
    pub campaign_session_id: String,
    pub origin_turn_id: String,
    pub client_idempotency_key: String,
    pub presentation_id: String,
    pub generation_job_id: String,
    pub generation_attempt_id: String,
    pub version: u8,
    pub source: GeneratedTextPresentationSource,
    pub config_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub output_digest: Sha256Digest,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTextPresentationSnapshot {
    /// The exact presentation bound to the caller's client idempotency key.
    pub requested: GeneratedTextPresentation,
    /// One turn-row-locked snapshot of every currently retained body.
    pub retained_versions: Vec<GeneratedTextPresentation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedTextPresentationReplay {
    Available(GeneratedTextPresentationSnapshot),
    Expired {
        receipt: GeneratedTextPresentationReceipt,
        retained_versions: Vec<GeneratedTextPresentation>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedIntentReceiptState {
    Pending,
    Committed,
}

impl std::str::FromStr for TypedIntentReceiptState {
    type Err = TextPresentationStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "committed" => Ok(Self::Committed),
            _ => Err(TextPresentationStoreError::InvalidStoredData(
                "unknown typed intent receipt state",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTypedIntentCommandReceipt {
    pub campaign_session_id: String,
    pub client_idempotency_key: String,
    pub player_intent_digest: Sha256Digest,
    pub expected_campaign_revision: u64,
    pub expected_encounter_revision: u64,
    pub resolved_intent: EncounterIntent,
    pub interpretation_label: String,
    pub interpretation_evidence_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedIntentCommandReceipt {
    pub campaign_session_id: String,
    pub client_idempotency_key: String,
    pub player_intent_digest: Sha256Digest,
    pub expected_campaign_revision: u64,
    pub expected_encounter_revision: u64,
    pub resolved_intent: EncounterIntent,
    pub interpretation_label: String,
    pub interpretation_evidence_json: String,
    pub state: TypedIntentReceiptState,
    pub origin_turn_id: Option<String>,
    pub event_sequence: Option<u64>,
    pub result_campaign_revision: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

impl PostgresRepository {
    /// Completes one leased narration attempt and selects its safe presentation
    /// in one transaction. A completed exact replay returns the original row;
    /// conflicting metadata fails closed.
    pub async fn finish_generation_with_text_presentation(
        &self,
        lease: &GenerationLease,
        presentation: &NewGeneratedTextPresentation,
        usage: &GenerationUsage,
        failure: Option<GenerationFailureCode>,
    ) -> Result<GeneratedTextPresentation, TextPresentationStoreError> {
        validate_lease(lease)?;
        validate_new_presentation(presentation, failure)?;
        let usage_values = usage_bindings(usage)?;
        if lease.job_id != presentation.generation_job_id
            || lease.attempt_id != presentation.generation_attempt_id
        {
            return Err(TextPresentationStoreError::InvalidInput(
                "presentation does not match the leased attempt",
            ));
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        if let Some(existing) = load_by_generation_attempt(
            &mut transaction,
            &presentation.generation_job_id,
            &presentation.generation_attempt_id,
        )
        .await?
        {
            ensure_matching_replay(&existing, presentation)?;
            transaction
                .commit()
                .await
                .map_err(TextPresentationStoreError::Database)?;
            return Ok(existing);
        }

        let job = sqlx::query(
            "SELECT campaign_session_id, origin_turn_id, purpose, config_digest,
                    prompt_digest, policy_digest
             FROM generation_jobs
             WHERE id = $1 AND state = 'running' AND lease_owner = $2
               AND lease_token = $3 AND lease_expires_at > CURRENT_TIMESTAMP
             FOR UPDATE",
        )
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
        .ok_or(TextPresentationStoreError::LostLease)?;
        let campaign_session_id: String = job
            .try_get("campaign_session_id")
            .map_err(TextPresentationStoreError::Database)?;
        let origin_turn_id: Option<String> = job
            .try_get("origin_turn_id")
            .map_err(TextPresentationStoreError::Database)?;
        let purpose: String = job
            .try_get("purpose")
            .map_err(TextPresentationStoreError::Database)?;
        if campaign_session_id != presentation.campaign_session_id
            || origin_turn_id.as_deref() != Some(presentation.origin_turn_id.as_str())
            || purpose != GenerationPurpose::Narration.as_str()
        {
            return Err(TextPresentationStoreError::InvalidInput(
                "presentation origin does not match the narration job",
            ));
        }
        ensure_job_digest(&job, "config_digest", &presentation.config_digest)?;
        ensure_job_digest(&job, "prompt_digest", &presentation.prompt_digest)?;
        ensure_job_digest(&job, "policy_digest", &presentation.policy_digest)?;

        let attempt_matches: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM generation_attempts
                WHERE id = $1 AND job_id = $2 AND state = 'running'
                  AND lease_owner = $3 AND lease_token = $4
             )",
        )
        .bind(&lease.attempt_id)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .fetch_one(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?;
        if !attempt_matches {
            return Err(TextPresentationStoreError::LostLease);
        }

        // Locking the immutable audit row serializes version allocation for one
        // turn across workers and process restarts.
        let origin_exists = sqlx::query(
            "SELECT id, turn_number FROM turn_audits
             WHERE id = $1 AND campaign_session_id = $2 FOR UPDATE",
        )
        .bind(&presentation.origin_turn_id)
        .bind(&presentation.campaign_session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?;
        let Some(origin) = origin_exists else {
            return Err(TextPresentationStoreError::InvalidInput(
                "presentation turn does not belong to the campaign",
            ));
        };
        if let Some(work_id) = &presentation.private_inspiration_work_id {
            let generation_disabled: bool = sqlx::query_scalar(
                "SELECT generation_disabled FROM private_inspiration_global_control
                 WHERE singleton FOR SHARE",
            )
            .fetch_one(&mut *transaction)
            .await
            .map_err(TextPresentationStoreError::Database)?;
            if generation_disabled {
                return Err(TextPresentationStoreError::InvalidInput(
                    "private inspiration generation is globally disabled",
                ));
            }
            let turn_number: i64 = origin
                .try_get("turn_number")
                .map_err(TextPresentationStoreError::Database)?;
            let work = sqlx::query(
                "SELECT work.state, work.work_kind, selection.turn_number
                 FROM private_inspiration_derived_work AS work
                 JOIN private_inspiration_selection_audits AS selection
                   ON selection.selection_id = work.selection_id
                 WHERE work.work_id = $1 AND work.campaign_session_id = $2
                 FOR UPDATE OF work",
            )
            .bind(work_id)
            .bind(&presentation.campaign_session_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(TextPresentationStoreError::Database)?
            .ok_or(TextPresentationStoreError::InvalidInput(
                "private inspiration work is unavailable",
            ))?;
            let state: String = work
                .try_get("state")
                .map_err(TextPresentationStoreError::Database)?;
            let kind: String = work
                .try_get("work_kind")
                .map_err(TextPresentationStoreError::Database)?;
            let selected_turn: i64 = work
                .try_get("turn_number")
                .map_err(TextPresentationStoreError::Database)?;
            if state != "pending" || kind != "text" || selected_turn != turn_number {
                return Err(TextPresentationStoreError::InvalidInput(
                    "private inspiration work is not current for this turn",
                ));
            }
        }
        let current_version: i16 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0)::SMALLINT
             FROM generated_text_presentation_receipts
             WHERE campaign_session_id = $1 AND origin_turn_id = $2",
        )
        .bind(&presentation.campaign_session_id)
        .bind(&presentation.origin_turn_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?;
        let next_version = current_version
            .checked_add(1)
            .ok_or(TextPresentationStoreError::NumericRange)?;
        if next_version > i16::from(MAX_TEXT_PRESENTATION_VERSIONS) {
            return Err(TextPresentationStoreError::VersionLimitReached);
        }

        complete_attempt_and_job(
            &mut transaction,
            lease,
            &presentation.output_digest,
            &usage_values,
            failure,
        )
        .await?;
        record_generation_attempt_usage(
            &mut transaction,
            &lease.job_id,
            GenerationPurpose::Narration,
            usage,
            true,
        )
        .await?;
        sqlx::query(
            "UPDATE generated_text_presentations
             SET selected = FALSE,
                 retention_delete_after = CURRENT_TIMESTAMP + INTERVAL '30 days',
                 updated_at = CURRENT_TIMESTAMP
             WHERE campaign_session_id = $1 AND origin_turn_id = $2 AND selected",
        )
        .bind(&presentation.campaign_session_id)
        .bind(&presentation.origin_turn_id)
        .execute(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?;

        let inserted = sqlx::query(&format!(
            "INSERT INTO generated_text_presentations
             (id, campaign_session_id, origin_turn_id, generation_job_id,
              generation_attempt_id, client_idempotency_key, version, source,
              body, config_digest, prompt_digest, policy_digest, output_digest,
              private_inspiration_work_id, selected)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, TRUE)
             RETURNING {PRESENTATION_COLUMNS}"
        ))
        .bind(&presentation.id)
        .bind(&presentation.campaign_session_id)
        .bind(&presentation.origin_turn_id)
        .bind(&presentation.generation_job_id)
        .bind(&presentation.generation_attempt_id)
        .bind(&presentation.client_idempotency_key)
        .bind(next_version)
        .bind(presentation.source.as_str())
        .bind(&presentation.body)
        .bind(presentation.config_digest.as_str())
        .bind(presentation.prompt_digest.as_str())
        .bind(presentation.policy_digest.as_str())
        .bind(presentation.output_digest.as_str())
        .bind(&presentation.private_inspiration_work_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_insert_error)
        .and_then(presentation_from_row)?;
        sqlx::query(
            "INSERT INTO generated_text_presentation_receipts
             (campaign_session_id, origin_turn_id, schema_version,
              client_idempotency_key, presentation_id, generation_job_id,
              generation_attempt_id, version, source, config_digest,
              prompt_digest, policy_digest, output_digest, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                     $14::TIMESTAMPTZ)",
        )
        .bind(&inserted.campaign_session_id)
        .bind(&inserted.origin_turn_id)
        .bind(i64::from(TEXT_PRESENTATION_RECEIPT_SCHEMA_VERSION))
        .bind(&inserted.client_idempotency_key)
        .bind(&inserted.id)
        .bind(&inserted.generation_job_id)
        .bind(&inserted.generation_attempt_id)
        .bind(i16::from(inserted.version))
        .bind(inserted.source.as_str())
        .bind(inserted.config_digest.as_str())
        .bind(inserted.prompt_digest.as_str())
        .bind(inserted.policy_digest.as_str())
        .bind(inserted.output_digest.as_str())
        .bind(&inserted.created_at)
        .execute(&mut *transaction)
        .await
        .map_err(map_insert_error)?;
        if let Some(work_id) = &presentation.private_inspiration_work_id {
            let completed = sqlx::query(
                "UPDATE private_inspiration_derived_work
                 SET state = 'completed', completed_artifact_id = $2,
                     completed_output_digest = $3,
                     completed_at_epoch = GREATEST(
                         created_at_epoch,
                         FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT
                     )
                 WHERE work_id = $1 AND campaign_session_id = $4
                   AND state = 'pending'",
            )
            .bind(work_id)
            .bind(&inserted.id)
            .bind(inserted.output_digest.as_str())
            .bind(&inserted.campaign_session_id)
            .execute(&mut *transaction)
            .await
            .map_err(TextPresentationStoreError::Database)?;
            if completed.rows_affected() != 1 {
                return Err(TextPresentationStoreError::InvalidInput(
                    "private inspiration work completion lost authorization",
                ));
            }
            sqlx::query(
                "INSERT INTO private_inspiration_privacy_audits
                 (audit_id, schema_version, campaign_session_id, operation_code,
                  subject_kind, subject_id, secondary_id, result_code,
                  occurred_at_epoch)
                 VALUES ($1, 1, $2, 'derived_work_completed', 'derived_work',
                         $3, $4, 'applied',
                         FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT)",
            )
            .bind(format!("privacy-audit:{}", uuid::Uuid::new_v4().simple()))
            .bind(&inserted.campaign_session_id)
            .bind(work_id)
            .bind(&inserted.id)
            .execute(&mut *transaction)
            .await
            .map_err(TextPresentationStoreError::Database)?;
        }
        transaction
            .commit()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        Ok(inserted)
    }

    /// Lists every still-retained owner-visible version for one campaign turn.
    pub async fn list_generated_text_presentations(
        &self,
        campaign_session_id: &str,
        origin_turn_id: &str,
    ) -> Result<Vec<GeneratedTextPresentation>, TextPresentationStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(origin_turn_id, "turn id is invalid")?;
        let rows = sqlx::query(&format!(
            "SELECT {PRESENTATION_COLUMNS}
             FROM generated_text_presentations
             WHERE campaign_session_id = $1 AND origin_turn_id = $2
               AND (retention_delete_after IS NULL
                    OR retention_delete_after > CURRENT_TIMESTAMP)
             ORDER BY version"
        ))
        .bind(campaign_session_id)
        .bind(origin_turn_id)
        .fetch_all(&self.pool)
        .await
        .map_err(TextPresentationStoreError::Database)?;
        rows.into_iter().map(presentation_from_row).collect()
    }

    pub async fn generated_text_presentation_version_count(
        &self,
        campaign_session_id: &str,
        origin_turn_id: &str,
    ) -> Result<u8, TextPresentationStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(origin_turn_id, "turn id is invalid")?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM generated_text_presentation_receipts
             WHERE campaign_session_id = $1 AND origin_turn_id = $2",
        )
        .bind(campaign_session_id)
        .bind(origin_turn_id)
        .fetch_one(&self.pool)
        .await
        .map_err(TextPresentationStoreError::Database)?;
        let count = u8::try_from(count).map_err(|_| TextPresentationStoreError::NumericRange)?;
        if count > MAX_TEXT_PRESENTATION_VERSIONS {
            return Err(TextPresentationStoreError::InvalidStoredData(
                "presentation receipt count exceeds the version cap",
            ));
        }
        Ok(count)
    }

    /// Resolves an exact client-command replay to its retained presentation
    /// without requiring a live lease or rebuilding a request with the current
    /// generation policy. This is used only after a response interruption; the
    /// opaque client key cannot create another version.
    pub async fn load_generated_text_presentation_by_client_key(
        &self,
        campaign_session_id: &str,
        origin_turn_id: &str,
        client_idempotency_key: &str,
    ) -> Result<Option<GeneratedTextPresentation>, TextPresentationStoreError> {
        Ok(
            match self
                .load_generated_text_presentation_replay(
                    campaign_session_id,
                    origin_turn_id,
                    client_idempotency_key,
                )
                .await?
            {
                Some(GeneratedTextPresentationReplay::Available(snapshot)) => {
                    Some(snapshot.requested)
                }
                Some(GeneratedTextPresentationReplay::Expired { .. }) | None => None,
            },
        )
    }

    /// Loads the exact client-key-bound presentation and retained history under
    /// a shared turn lock. A superseded body may expire, but its campaign-
    /// lifetime receipt remains an explicit replay result rather than allowing
    /// the old key to enqueue a new generation.
    pub async fn load_generated_text_presentation_replay(
        &self,
        campaign_session_id: &str,
        origin_turn_id: &str,
        client_idempotency_key: &str,
    ) -> Result<Option<GeneratedTextPresentationReplay>, TextPresentationStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(origin_turn_id, "turn id is invalid")?;
        validate_identifier(client_idempotency_key, "client idempotency key is invalid")?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        let origin_exists = sqlx::query_scalar::<_, String>(
            "SELECT id FROM turn_audits
             WHERE id = $1 AND campaign_session_id = $2 FOR SHARE",
        )
        .bind(origin_turn_id)
        .bind(campaign_session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
        .is_some();
        if !origin_exists {
            return Err(TextPresentationStoreError::InvalidInput(
                "presentation turn does not belong to the campaign",
            ));
        }
        let receipt = load_presentation_receipt_by_client_key(
            &mut transaction,
            campaign_session_id,
            origin_turn_id,
            client_idempotency_key,
        )
        .await?;
        let Some(receipt) = receipt else {
            transaction
                .commit()
                .await
                .map_err(TextPresentationStoreError::Database)?;
            return Ok(None);
        };
        let requested = sqlx::query(&format!(
            "SELECT {PRESENTATION_COLUMNS}
             FROM generated_text_presentations
             WHERE id = $1 AND campaign_session_id = $2 AND origin_turn_id = $3
               AND (retention_delete_after IS NULL
                    OR retention_delete_after > CURRENT_TIMESTAMP)"
        ))
        .bind(&receipt.presentation_id)
        .bind(campaign_session_id)
        .bind(origin_turn_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
        .map(presentation_from_row)
        .transpose()?;
        if let Some(requested) = requested.as_ref() {
            ensure_receipt_matches_presentation(&receipt, requested)?;
        }
        let retained_versions = list_presentations_in_transaction(
            &mut transaction,
            campaign_session_id,
            origin_turn_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        Ok(Some(match requested {
            Some(requested) => {
                GeneratedTextPresentationReplay::Available(GeneratedTextPresentationSnapshot {
                    requested,
                    retained_versions,
                })
            }
            None => GeneratedTextPresentationReplay::Expired {
                receipt,
                retained_versions,
            },
        }))
    }

    /// Inserts the body-free recovery record after a closed typed intent has
    /// passed validation and before its mechanics mutation runs. Matching
    /// retries return the original record; any metadata drift fails closed.
    pub async fn insert_pending_typed_intent_command_receipt(
        &self,
        requested: &NewTypedIntentCommandReceipt,
    ) -> Result<TypedIntentCommandReceipt, TextPresentationStoreError> {
        validate_new_typed_intent_receipt(requested)?;
        let expected_campaign_revision = i64::try_from(requested.expected_campaign_revision)
            .map_err(|_| TextPresentationStoreError::NumericRange)?;
        let expected_encounter_revision = i64::try_from(requested.expected_encounter_revision)
            .map_err(|_| TextPresentationStoreError::NumericRange)?;
        let resolved_intent_json = serde_json::to_string(&requested.resolved_intent)
            .map_err(|_| TextPresentationStoreError::InvalidInput("intent serialization failed"))?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        let inserted = sqlx::query(&format!(
            "INSERT INTO typed_intent_command_receipts
             (campaign_session_id, client_idempotency_key, schema_version,
              player_intent_digest, expected_campaign_revision,
              expected_encounter_revision, resolved_intent_json,
              interpretation_label, interpretation_evidence_json, state)
             VALUES ($1, $2, $3, $4, $5, $6, $7::JSONB, $8, $9::JSONB, 'pending')
             ON CONFLICT (campaign_session_id, client_idempotency_key) DO NOTHING
             RETURNING {TYPED_INTENT_RECEIPT_COLUMNS}"
        ))
        .bind(&requested.campaign_session_id)
        .bind(&requested.client_idempotency_key)
        .bind(i64::from(TYPED_INTENT_RECEIPT_SCHEMA_VERSION))
        .bind(requested.player_intent_digest.as_str())
        .bind(expected_campaign_revision)
        .bind(expected_encounter_revision)
        .bind(&resolved_intent_json)
        .bind(&requested.interpretation_label)
        .bind(&requested.interpretation_evidence_json)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
        .map(typed_intent_receipt_from_row)
        .transpose()?;
        let stored = if let Some(inserted) = inserted {
            inserted
        } else {
            load_typed_intent_receipt_in_transaction(
                &mut transaction,
                &requested.campaign_session_id,
                &requested.client_idempotency_key,
                false,
            )
            .await?
            .ok_or(TextPresentationStoreError::InvalidStoredData(
                "typed intent idempotency conflict did not resolve to a receipt",
            ))?
        };
        ensure_matching_typed_intent_receipt(&stored, requested)?;
        transaction
            .commit()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        Ok(stored)
    }

    pub async fn load_typed_intent_command_receipt(
        &self,
        campaign_session_id: &str,
        client_idempotency_key: &str,
    ) -> Result<Option<TypedIntentCommandReceipt>, TextPresentationStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(client_idempotency_key, "client idempotency key is invalid")?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        let receipt = load_typed_intent_receipt_in_transaction(
            &mut transaction,
            campaign_session_id,
            client_idempotency_key,
            false,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        Ok(receipt)
    }

    pub async fn commit_typed_intent_command_receipt(
        &self,
        campaign_session_id: &str,
        client_idempotency_key: &str,
        player_intent_digest: &Sha256Digest,
        origin_turn_id: &str,
        event_sequence: u64,
        result_campaign_revision: u64,
    ) -> Result<TypedIntentCommandReceipt, TextPresentationStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(client_idempotency_key, "client idempotency key is invalid")?;
        validate_identifier(origin_turn_id, "turn id is invalid")?;
        let event_sequence =
            i64::try_from(event_sequence).map_err(|_| TextPresentationStoreError::NumericRange)?;
        let result_campaign_revision = i64::try_from(result_campaign_revision)
            .map_err(|_| TextPresentationStoreError::NumericRange)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        let stored = load_typed_intent_receipt_in_transaction(
            &mut transaction,
            campaign_session_id,
            client_idempotency_key,
            true,
        )
        .await?
        .ok_or(TextPresentationStoreError::InvalidInput(
            "typed intent receipt is unavailable",
        ))?;
        if &stored.player_intent_digest != player_intent_digest {
            return Err(TextPresentationStoreError::IdempotencyConflict);
        }
        if stored.state == TypedIntentReceiptState::Committed {
            if stored.origin_turn_id.as_deref() != Some(origin_turn_id)
                || stored.event_sequence != u64::try_from(event_sequence).ok()
                || stored.result_campaign_revision != u64::try_from(result_campaign_revision).ok()
            {
                return Err(TextPresentationStoreError::IdempotencyConflict);
            }
            transaction
                .commit()
                .await
                .map_err(TextPresentationStoreError::Database)?;
            return Ok(stored);
        }
        let committed = sqlx::query(&format!(
            "UPDATE typed_intent_command_receipts
             SET state = 'committed', origin_turn_id = $3, event_sequence = $4,
                 result_campaign_revision = $5, updated_at = CURRENT_TIMESTAMP
             WHERE campaign_session_id = $1 AND client_idempotency_key = $2
               AND state = 'pending'
             RETURNING {TYPED_INTENT_RECEIPT_COLUMNS}"
        ))
        .bind(campaign_session_id)
        .bind(client_idempotency_key)
        .bind(origin_turn_id)
        .bind(event_sequence)
        .bind(result_campaign_revision)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
        .map(typed_intent_receipt_from_row)
        .transpose()?
        .ok_or(TextPresentationStoreError::IdempotencyConflict)?;
        transaction
            .commit()
            .await
            .map_err(TextPresentationStoreError::Database)?;
        Ok(committed)
    }

    /// Idempotent bounded retention cleanup. Selected rows cannot match the
    /// predicate because the schema requires a null deletion timestamp.
    pub async fn delete_expired_generated_text_presentations(
        &self,
        limit: u16,
    ) -> Result<u64, TextPresentationStoreError> {
        if limit == 0 || limit > 1_000 {
            return Err(TextPresentationStoreError::InvalidInput(
                "cleanup limit must be between one and one thousand",
            ));
        }
        let deleted = sqlx::query(
            "WITH expired AS (
                SELECT id FROM generated_text_presentations
                WHERE NOT selected
                  AND retention_delete_after <= CURRENT_TIMESTAMP
                ORDER BY retention_delete_after, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
             )
             DELETE FROM generated_text_presentations
             WHERE id IN (SELECT id FROM expired)",
        )
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await
        .map_err(TextPresentationStoreError::Database)?;
        Ok(deleted.rows_affected())
    }
}

async fn complete_attempt_and_job(
    transaction: &mut Transaction<'_, Postgres>,
    lease: &GenerationLease,
    output_digest: &Sha256Digest,
    usage: &super::jobs::UsageBindings,
    failure: Option<GenerationFailureCode>,
) -> Result<(), TextPresentationStoreError> {
    let attempt_updated = if let Some(code) = failure {
        sqlx::query(
            "UPDATE generation_attempts
             SET state = 'failed', prompt_tokens = $5, completion_tokens = $6,
                 total_tokens = $7, cost_microusd = $8,
                 latency_milliseconds = $9,
                 failure_class = $10, failure_code = $11, output_digest = $12,
                 heartbeat_at = CURRENT_TIMESTAMP, finished_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND job_id = $2 AND state = 'running'
               AND lease_owner = $3 AND lease_token = $4",
        )
        .bind(&lease.attempt_id)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .bind(usage.total_tokens)
        .bind(usage.cost_microusd)
        .bind(usage.latency_milliseconds)
        .bind(code.class().as_str())
        .bind(code.as_str())
        .bind(output_digest.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
    } else {
        sqlx::query(
            "UPDATE generation_attempts
             SET state = 'succeeded', prompt_tokens = $5, completion_tokens = $6,
                 total_tokens = $7, cost_microusd = $8,
                 latency_milliseconds = $9, output_digest = $10,
                 heartbeat_at = CURRENT_TIMESTAMP, finished_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND job_id = $2 AND state = 'running'
               AND lease_owner = $3 AND lease_token = $4",
        )
        .bind(&lease.attempt_id)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .bind(usage.total_tokens)
        .bind(usage.cost_microusd)
        .bind(usage.latency_milliseconds)
        .bind(output_digest.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?
    };
    if attempt_updated.rows_affected() != 1 {
        return Err(TextPresentationStoreError::LostLease);
    }

    if let Some(code) = failure {
        sqlx::query(
            "UPDATE generation_jobs
             SET state = 'failed', retry_at = NULL, lease_owner = NULL,
                 lease_token = NULL, lease_expires_at = NULL,
                 last_failure_class = $2, last_failure_code = $3,
                 output_digest = $4, retention_class = 'failed_metadata_7d',
                 retention_delete_after = CURRENT_TIMESTAMP + INTERVAL '7 days',
                 completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1",
        )
        .bind(&lease.job_id)
        .bind(code.class().as_str())
        .bind(code.as_str())
        .bind(output_digest.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?;
    } else {
        sqlx::query(
            "UPDATE generation_jobs
             SET state = 'succeeded', retry_at = NULL, lease_owner = NULL,
                 lease_token = NULL, lease_expires_at = NULL,
                 last_failure_class = NULL, last_failure_code = NULL,
                 output_digest = $2,
                 retention_class = 'unselected_presentation_30d',
                 retention_delete_after = CURRENT_TIMESTAMP + INTERVAL '30 days',
                 completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1",
        )
        .bind(&lease.job_id)
        .bind(output_digest.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(TextPresentationStoreError::Database)?;
    }
    Ok(())
}

async fn load_by_generation_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &str,
    attempt_id: &str,
) -> Result<Option<GeneratedTextPresentation>, TextPresentationStoreError> {
    sqlx::query(&format!(
        "SELECT {PRESENTATION_COLUMNS}
         FROM generated_text_presentations
         WHERE generation_job_id = $1 AND generation_attempt_id = $2"
    ))
    .bind(job_id)
    .bind(attempt_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(TextPresentationStoreError::Database)?
    .map(presentation_from_row)
    .transpose()
}

async fn list_presentations_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    origin_turn_id: &str,
) -> Result<Vec<GeneratedTextPresentation>, TextPresentationStoreError> {
    let rows = sqlx::query(&format!(
        "SELECT {PRESENTATION_COLUMNS}
         FROM generated_text_presentations
         WHERE campaign_session_id = $1 AND origin_turn_id = $2
           AND (retention_delete_after IS NULL
                OR retention_delete_after > CURRENT_TIMESTAMP)
         ORDER BY version"
    ))
    .bind(campaign_session_id)
    .bind(origin_turn_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(TextPresentationStoreError::Database)?;
    rows.into_iter().map(presentation_from_row).collect()
}

async fn load_presentation_receipt_by_client_key(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    origin_turn_id: &str,
    client_idempotency_key: &str,
) -> Result<Option<GeneratedTextPresentationReceipt>, TextPresentationStoreError> {
    sqlx::query(&format!(
        "SELECT {PRESENTATION_RECEIPT_COLUMNS}
         FROM generated_text_presentation_receipts
         WHERE campaign_session_id = $1 AND origin_turn_id = $2
           AND client_idempotency_key = $3"
    ))
    .bind(campaign_session_id)
    .bind(origin_turn_id)
    .bind(client_idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(TextPresentationStoreError::Database)?
    .map(presentation_receipt_from_row)
    .transpose()
}

fn presentation_receipt_from_row(
    row: PgRow,
) -> Result<GeneratedTextPresentationReceipt, TextPresentationStoreError> {
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(TextPresentationStoreError::Database)?;
    let version: i16 = row
        .try_get("version")
        .map_err(TextPresentationStoreError::Database)?;
    let source: String = row
        .try_get("source")
        .map_err(TextPresentationStoreError::Database)?;
    let receipt = GeneratedTextPresentationReceipt {
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(TextPresentationStoreError::Database)?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(TextPresentationStoreError::Database)?,
        client_idempotency_key: row
            .try_get("client_idempotency_key")
            .map_err(TextPresentationStoreError::Database)?,
        presentation_id: row
            .try_get("presentation_id")
            .map_err(TextPresentationStoreError::Database)?,
        generation_job_id: row
            .try_get("generation_job_id")
            .map_err(TextPresentationStoreError::Database)?,
        generation_attempt_id: row
            .try_get("generation_attempt_id")
            .map_err(TextPresentationStoreError::Database)?,
        version: u8::try_from(version).map_err(|_| TextPresentationStoreError::NumericRange)?,
        source: source.parse()?,
        config_digest: digest_from_row(&row, "config_digest")?,
        prompt_digest: digest_from_row(&row, "prompt_digest")?,
        policy_digest: digest_from_row(&row, "policy_digest")?,
        output_digest: digest_from_row(&row, "output_digest")?,
        created_at: row
            .try_get("created_at")
            .map_err(TextPresentationStoreError::Database)?,
    };
    if schema_version != i64::from(TEXT_PRESENTATION_RECEIPT_SCHEMA_VERSION) {
        return Err(TextPresentationStoreError::InvalidStoredData(
            "unsupported presentation receipt schema version",
        ));
    }
    validate_identifier(
        &receipt.campaign_session_id,
        "stored receipt campaign id is invalid",
    )?;
    validate_identifier(&receipt.origin_turn_id, "stored receipt turn id is invalid")?;
    for (value, reason) in [
        (
            receipt.client_idempotency_key.as_str(),
            "stored receipt client key is invalid",
        ),
        (
            receipt.presentation_id.as_str(),
            "stored receipt presentation id is invalid",
        ),
        (
            receipt.generation_job_id.as_str(),
            "stored receipt job id is invalid",
        ),
        (
            receipt.generation_attempt_id.as_str(),
            "stored receipt attempt id is invalid",
        ),
    ] {
        validate_identifier(value, reason)?;
    }
    if !(1..=MAX_TEXT_PRESENTATION_VERSIONS).contains(&receipt.version)
        || receipt.created_at.is_empty()
    {
        return Err(TextPresentationStoreError::InvalidStoredData(
            "stored presentation receipt bounds are invalid",
        ));
    }
    Ok(receipt)
}

fn ensure_receipt_matches_presentation(
    receipt: &GeneratedTextPresentationReceipt,
    presentation: &GeneratedTextPresentation,
) -> Result<(), TextPresentationStoreError> {
    if receipt.campaign_session_id != presentation.campaign_session_id
        || receipt.origin_turn_id != presentation.origin_turn_id
        || receipt.client_idempotency_key != presentation.client_idempotency_key
        || receipt.presentation_id != presentation.id
        || receipt.generation_job_id != presentation.generation_job_id
        || receipt.generation_attempt_id != presentation.generation_attempt_id
        || receipt.version != presentation.version
        || receipt.source != presentation.source
        || receipt.config_digest != presentation.config_digest
        || receipt.prompt_digest != presentation.prompt_digest
        || receipt.policy_digest != presentation.policy_digest
        || receipt.output_digest != presentation.output_digest
        || receipt.created_at != presentation.created_at
    {
        return Err(TextPresentationStoreError::InvalidStoredData(
            "presentation receipt does not match retained presentation",
        ));
    }
    Ok(())
}

async fn load_typed_intent_receipt_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    client_idempotency_key: &str,
    for_update: bool,
) -> Result<Option<TypedIntentCommandReceipt>, TextPresentationStoreError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query(&format!(
        "SELECT {TYPED_INTENT_RECEIPT_COLUMNS}
         FROM typed_intent_command_receipts
         WHERE campaign_session_id = $1 AND client_idempotency_key = $2{lock}"
    ))
    .bind(campaign_session_id)
    .bind(client_idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(TextPresentationStoreError::Database)?
    .map(typed_intent_receipt_from_row)
    .transpose()
}

fn typed_intent_receipt_from_row(
    row: PgRow,
) -> Result<TypedIntentCommandReceipt, TextPresentationStoreError> {
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(TextPresentationStoreError::Database)?;
    if schema_version != i64::from(TYPED_INTENT_RECEIPT_SCHEMA_VERSION) {
        return Err(TextPresentationStoreError::InvalidStoredData(
            "unsupported typed intent receipt schema version",
        ));
    }
    let player_intent_digest = digest_from_row(&row, "player_intent_digest")?;
    let expected_campaign_revision: i64 = row
        .try_get("expected_campaign_revision")
        .map_err(TextPresentationStoreError::Database)?;
    let expected_encounter_revision: i64 = row
        .try_get("expected_encounter_revision")
        .map_err(TextPresentationStoreError::Database)?;
    let resolved_intent_json: String = row
        .try_get("resolved_intent_json")
        .map_err(TextPresentationStoreError::Database)?;
    let state: String = row
        .try_get("state")
        .map_err(TextPresentationStoreError::Database)?;
    let event_sequence: Option<i64> = row
        .try_get("event_sequence")
        .map_err(TextPresentationStoreError::Database)?;
    let result_campaign_revision: Option<i64> = row
        .try_get("result_campaign_revision")
        .map_err(TextPresentationStoreError::Database)?;
    let receipt = TypedIntentCommandReceipt {
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(TextPresentationStoreError::Database)?,
        client_idempotency_key: row
            .try_get("client_idempotency_key")
            .map_err(TextPresentationStoreError::Database)?,
        player_intent_digest,
        expected_campaign_revision: u64::try_from(expected_campaign_revision)
            .map_err(|_| TextPresentationStoreError::NumericRange)?,
        expected_encounter_revision: u64::try_from(expected_encounter_revision)
            .map_err(|_| TextPresentationStoreError::NumericRange)?,
        resolved_intent: serde_json::from_str(&resolved_intent_json).map_err(|_| {
            TextPresentationStoreError::InvalidStoredData("invalid stored resolved intent")
        })?,
        interpretation_label: row
            .try_get("interpretation_label")
            .map_err(TextPresentationStoreError::Database)?,
        interpretation_evidence_json: row
            .try_get("interpretation_evidence_json")
            .map_err(TextPresentationStoreError::Database)?,
        state: state.parse()?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(TextPresentationStoreError::Database)?,
        event_sequence: event_sequence
            .map(u64::try_from)
            .transpose()
            .map_err(|_| TextPresentationStoreError::NumericRange)?,
        result_campaign_revision: result_campaign_revision
            .map(u64::try_from)
            .transpose()
            .map_err(|_| TextPresentationStoreError::NumericRange)?,
        created_at: row
            .try_get("created_at")
            .map_err(TextPresentationStoreError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(TextPresentationStoreError::Database)?,
    };
    validate_loaded_typed_intent_receipt(&receipt)?;
    Ok(receipt)
}

fn validate_new_typed_intent_receipt(
    receipt: &NewTypedIntentCommandReceipt,
) -> Result<(), TextPresentationStoreError> {
    validate_identifier(&receipt.campaign_session_id, "campaign id is invalid")?;
    validate_identifier(
        &receipt.client_idempotency_key,
        "client idempotency key is invalid",
    )?;
    if receipt.expected_campaign_revision == 0 || receipt.expected_encounter_revision == 0 {
        return Err(TextPresentationStoreError::InvalidInput(
            "typed intent receipt revisions must be positive",
        ));
    }
    validate_interpretation_label(&receipt.interpretation_label)?;
    validate_metadata_json(
        &receipt.interpretation_evidence_json,
        32_768,
        "interpretation evidence is invalid",
    )?;
    let intent_json = serde_json::to_string(&receipt.resolved_intent)
        .map_err(|_| TextPresentationStoreError::InvalidInput("intent serialization failed"))?;
    validate_metadata_json(&intent_json, 8_192, "resolved intent is invalid")?;
    EncounterCommand::new(
        receipt.expected_encounter_revision,
        receipt.client_idempotency_key.clone(),
        receipt.resolved_intent.clone(),
    )
    .validate()
    .map_err(|_| TextPresentationStoreError::InvalidInput("resolved intent is invalid"))?;
    Ok(())
}

fn validate_loaded_typed_intent_receipt(
    receipt: &TypedIntentCommandReceipt,
) -> Result<(), TextPresentationStoreError> {
    validate_identifier(
        &receipt.campaign_session_id,
        "stored typed intent campaign id is invalid",
    )?;
    validate_identifier(
        &receipt.client_idempotency_key,
        "stored typed intent client key is invalid",
    )?;
    if receipt.expected_campaign_revision == 0
        || receipt.expected_encounter_revision == 0
        || receipt.created_at.is_empty()
        || receipt.updated_at.is_empty()
    {
        return Err(TextPresentationStoreError::InvalidStoredData(
            "stored typed intent receipt bounds are invalid",
        ));
    }
    EncounterCommand::new(
        receipt.expected_encounter_revision,
        receipt.client_idempotency_key.clone(),
        receipt.resolved_intent.clone(),
    )
    .validate()
    .map_err(|_| {
        TextPresentationStoreError::InvalidStoredData("stored resolved intent is invalid")
    })?;
    validate_interpretation_label(&receipt.interpretation_label)?;
    validate_metadata_json(
        &receipt.interpretation_evidence_json,
        32_768,
        "stored interpretation evidence is invalid",
    )?;
    match receipt.state {
        TypedIntentReceiptState::Pending
            if receipt.origin_turn_id.is_none()
                && receipt.event_sequence.is_none()
                && receipt.result_campaign_revision.is_none() => {}
        TypedIntentReceiptState::Committed
            if receipt.origin_turn_id.is_some()
                && receipt.event_sequence.is_some()
                && receipt.result_campaign_revision
                    == receipt.expected_campaign_revision.checked_add(1) =>
        {
            validate_identifier(
                receipt
                    .origin_turn_id
                    .as_deref()
                    .expect("committed receipt has a turn id"),
                "stored typed intent turn id is invalid",
            )?;
        }
        _ => {
            return Err(TextPresentationStoreError::InvalidStoredData(
                "stored typed intent receipt state is invalid",
            ));
        }
    }
    Ok(())
}

fn ensure_matching_typed_intent_receipt(
    existing: &TypedIntentCommandReceipt,
    requested: &NewTypedIntentCommandReceipt,
) -> Result<(), TextPresentationStoreError> {
    let existing_evidence: serde_json::Value =
        serde_json::from_str(&existing.interpretation_evidence_json).map_err(|_| {
            TextPresentationStoreError::InvalidStoredData(
                "stored interpretation evidence is invalid",
            )
        })?;
    let requested_evidence: serde_json::Value =
        serde_json::from_str(&requested.interpretation_evidence_json).map_err(|_| {
            TextPresentationStoreError::InvalidInput("interpretation evidence is invalid")
        })?;
    if existing.campaign_session_id != requested.campaign_session_id
        || existing.client_idempotency_key != requested.client_idempotency_key
        || existing.player_intent_digest != requested.player_intent_digest
        || existing.expected_campaign_revision != requested.expected_campaign_revision
        || existing.expected_encounter_revision != requested.expected_encounter_revision
        || existing.resolved_intent != requested.resolved_intent
        || existing.interpretation_label != requested.interpretation_label
        || existing_evidence != requested_evidence
    {
        return Err(TextPresentationStoreError::IdempotencyConflict);
    }
    Ok(())
}

fn validate_interpretation_label(value: &str) -> Result<(), TextPresentationStoreError> {
    if value.trim() != value
        || value.is_empty()
        || value.chars().count() > 512
        || value.len() > 2_048
        || value.chars().any(char::is_control)
    {
        return Err(TextPresentationStoreError::InvalidInput(
            "interpretation label is invalid",
        ));
    }
    Ok(())
}

fn validate_metadata_json(
    value: &str,
    max_bytes: usize,
    reason: &'static str,
) -> Result<(), TextPresentationStoreError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(TextPresentationStoreError::InvalidInput(reason));
    }
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|_| TextPresentationStoreError::InvalidInput(reason))?;
    if !parsed.is_object() {
        return Err(TextPresentationStoreError::InvalidInput(reason));
    }
    Ok(())
}

fn validate_new_presentation(
    presentation: &NewGeneratedTextPresentation,
    failure: Option<GenerationFailureCode>,
) -> Result<(), TextPresentationStoreError> {
    for (value, reason) in [
        (presentation.id.as_str(), "presentation id is invalid"),
        (
            presentation.campaign_session_id.as_str(),
            "campaign id is invalid",
        ),
        (presentation.origin_turn_id.as_str(), "turn id is invalid"),
        (
            presentation.generation_job_id.as_str(),
            "generation job id is invalid",
        ),
        (
            presentation.generation_attempt_id.as_str(),
            "generation attempt id is invalid",
        ),
        (
            presentation.client_idempotency_key.as_str(),
            "client idempotency key is invalid",
        ),
    ] {
        validate_identifier(value, reason)?;
    }
    if let Some(work_id) = &presentation.private_inspiration_work_id {
        validate_identifier(work_id, "private inspiration work id is invalid")?;
    }
    validate_safe_body(&presentation.body)?;
    match (presentation.source, failure) {
        (GeneratedTextPresentationSource::Provider, None)
        | (
            GeneratedTextPresentationSource::AuthoredFallback
            | GeneratedTextPresentationSource::EngineAuthored,
            Some(_),
        ) => Ok(()),
        _ => Err(TextPresentationStoreError::InvalidInput(
            "presentation source does not match generation completion",
        )),
    }
}

fn validate_safe_body(body: &str) -> Result<(), TextPresentationStoreError> {
    if body.trim() != body
        || body.is_empty()
        || body.chars().count() > MAX_TEXT_PRESENTATION_CHARS
        || body.len() > MAX_TEXT_PRESENTATION_BYTES
        || body
            .chars()
            .any(|character| character.is_control() && character != '\n')
    {
        return Err(TextPresentationStoreError::InvalidInput(
            "presentation body must be trimmed, bounded, and control-free",
        ));
    }
    let lower = body.to_ascii_lowercase();
    const REJECTED_MARKERS: &[&str] = &[
        "<script",
        "<iframe",
        "javascript:",
        "authorization: bearer",
        "api_key",
        "api key",
        "system prompt",
        "developer message",
        "ignore previous instructions",
        "ignore all previous",
    ];
    if REJECTED_MARKERS.iter().any(|marker| lower.contains(marker)) || contains_html_tag(&lower) {
        return Err(TextPresentationStoreError::InvalidInput(
            "presentation body failed the deterministic safety filter",
        ));
    }
    Ok(())
}

fn contains_html_tag(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'<'
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || matches!(*next, b'/' | b'!'))
            && bytes[index + 1..]
                .iter()
                .take(128)
                .any(|next| *next == b'>')
    })
}

fn ensure_matching_replay(
    existing: &GeneratedTextPresentation,
    requested: &NewGeneratedTextPresentation,
) -> Result<(), TextPresentationStoreError> {
    // `id` is server-assigned presentation identity, not generation intent.
    // A caller recovering an ambiguous commit may propose a fresh UUID; the
    // immutable job/attempt pair remains the exact replay authority.
    if existing.campaign_session_id != requested.campaign_session_id
        || existing.origin_turn_id != requested.origin_turn_id
        || existing.generation_job_id != requested.generation_job_id
        || existing.generation_attempt_id != requested.generation_attempt_id
        || existing.client_idempotency_key != requested.client_idempotency_key
        || existing.source != requested.source
        || existing.body != requested.body
        || existing.config_digest != requested.config_digest
        || existing.prompt_digest != requested.prompt_digest
        || existing.policy_digest != requested.policy_digest
        || existing.output_digest != requested.output_digest
        || existing.private_inspiration_work_id != requested.private_inspiration_work_id
    {
        return Err(TextPresentationStoreError::IdempotencyConflict);
    }
    Ok(())
}

fn ensure_job_digest(
    row: &PgRow,
    column: &str,
    expected: &Sha256Digest,
) -> Result<(), TextPresentationStoreError> {
    let actual: String = row
        .try_get(column)
        .map_err(TextPresentationStoreError::Database)?;
    if actual != expected.as_str() {
        return Err(TextPresentationStoreError::InvalidInput(
            "presentation digest does not match the generation job",
        ));
    }
    Ok(())
}

fn validate_identifier(
    value: &str,
    reason: &'static str,
) -> Result<(), TextPresentationStoreError> {
    if !is_valid_opaque_id(value) {
        return Err(TextPresentationStoreError::InvalidInput(reason));
    }
    Ok(())
}

fn map_insert_error(error: sqlx::Error) -> TextPresentationStoreError {
    if error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
    {
        TextPresentationStoreError::IdempotencyConflict
    } else {
        TextPresentationStoreError::Database(error)
    }
}

fn presentation_from_row(
    row: PgRow,
) -> Result<GeneratedTextPresentation, TextPresentationStoreError> {
    let source: String = row
        .try_get("source")
        .map_err(TextPresentationStoreError::Database)?;
    let version: i16 = row
        .try_get("version")
        .map_err(TextPresentationStoreError::Database)?;
    let presentation = GeneratedTextPresentation {
        id: row
            .try_get("id")
            .map_err(TextPresentationStoreError::Database)?,
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(TextPresentationStoreError::Database)?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(TextPresentationStoreError::Database)?,
        generation_job_id: row
            .try_get("generation_job_id")
            .map_err(TextPresentationStoreError::Database)?,
        generation_attempt_id: row
            .try_get("generation_attempt_id")
            .map_err(TextPresentationStoreError::Database)?,
        client_idempotency_key: row
            .try_get("client_idempotency_key")
            .map_err(TextPresentationStoreError::Database)?,
        version: u8::try_from(version).map_err(|_| TextPresentationStoreError::NumericRange)?,
        source: source.parse()?,
        body: row
            .try_get("body")
            .map_err(TextPresentationStoreError::Database)?,
        config_digest: digest_from_row(&row, "config_digest")?,
        prompt_digest: digest_from_row(&row, "prompt_digest")?,
        policy_digest: digest_from_row(&row, "policy_digest")?,
        output_digest: digest_from_row(&row, "output_digest")?,
        private_inspiration_work_id: row
            .try_get("private_inspiration_work_id")
            .map_err(TextPresentationStoreError::Database)?,
        privacy_redacted: row
            .try_get::<String, _>("privacy_state")
            .map_err(TextPresentationStoreError::Database)?
            == "redacted",
        selected: row
            .try_get("selected")
            .map_err(TextPresentationStoreError::Database)?,
        retention_delete_after: row
            .try_get("retention_delete_after")
            .map_err(TextPresentationStoreError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(TextPresentationStoreError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(TextPresentationStoreError::Database)?,
    };
    validate_loaded_presentation(&presentation)?;
    Ok(presentation)
}

fn digest_from_row(row: &PgRow, column: &str) -> Result<Sha256Digest, TextPresentationStoreError> {
    Sha256Digest::new(
        row.try_get::<String, _>(column)
            .map_err(TextPresentationStoreError::Database)?,
    )
    .map_err(|_| TextPresentationStoreError::InvalidStoredData("invalid digest"))
}

fn validate_loaded_presentation(
    presentation: &GeneratedTextPresentation,
) -> Result<(), TextPresentationStoreError> {
    validate_identifier(&presentation.id, "stored presentation id is invalid")?;
    validate_identifier(
        &presentation.campaign_session_id,
        "stored campaign id is invalid",
    )?;
    validate_identifier(&presentation.origin_turn_id, "stored turn id is invalid")?;
    validate_identifier(
        &presentation.client_idempotency_key,
        "stored client idempotency key is invalid",
    )?;
    if let Some(work_id) = &presentation.private_inspiration_work_id {
        validate_identifier(work_id, "stored private inspiration work id is invalid")?;
    }
    if presentation.privacy_redacted {
        if presentation.body != PRIVATE_INSPIRATION_REDACTION_BODY {
            return Err(TextPresentationStoreError::InvalidStoredData(
                "stored privacy redaction marker is invalid",
            ));
        }
    } else {
        validate_safe_body(&presentation.body)?;
    }
    if !(1..=MAX_TEXT_PRESENTATION_VERSIONS).contains(&presentation.version)
        || presentation.created_at.is_empty()
        || presentation.updated_at.is_empty()
        || presentation.selected != presentation.retention_delete_after.is_none()
    {
        return Err(TextPresentationStoreError::InvalidStoredData(
            "stored presentation bounds are invalid",
        ));
    }
    Ok(())
}

const PRESENTATION_COLUMNS: &str = "
    id, campaign_session_id, origin_turn_id, generation_job_id,
    generation_attempt_id, client_idempotency_key, version, source, body,
    config_digest, prompt_digest, policy_digest, output_digest,
    private_inspiration_work_id, privacy_state, selected,
    retention_delete_after::text AS retention_delete_after,
    created_at::text AS created_at, updated_at::text AS updated_at";

const PRESENTATION_RECEIPT_COLUMNS: &str = "
    campaign_session_id, origin_turn_id, schema_version,
    client_idempotency_key, presentation_id, generation_job_id,
    generation_attempt_id, version, source, config_digest, prompt_digest,
    policy_digest, output_digest, created_at::text AS created_at";

const TYPED_INTENT_RECEIPT_COLUMNS: &str = "
    campaign_session_id, client_idempotency_key, schema_version,
    player_intent_digest, expected_campaign_revision,
    expected_encounter_revision, resolved_intent_json::text AS resolved_intent_json,
    interpretation_label,
    interpretation_evidence_json::text AS interpretation_evidence_json,
    state, origin_turn_id, event_sequence, result_campaign_revision,
    created_at::text AS created_at, updated_at::text AS updated_at";

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;

    use super::*;
    use crate::repository::{
        MIGRATOR,
        jobs::{GenerationClaim, GenerationJobState, NewGenerationJob, SuccessRetention},
    };

    const CAMPAIGN_ID: &str = "campaign-presentations";
    const TURN_ID: &str = "turn-presentation-1";

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; 32])
    }

    async fn seed_origin(pool: &PgPool) {
        sqlx::query(
            "INSERT INTO campaign_sessions (id, schema_version, revision, payload_json)
             VALUES ($1, 1, 2, '{}'::jsonb)",
        )
        .bind(CAMPAIGN_ID)
        .execute(pool)
        .await
        .expect("campaign fixture should insert");
        sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, schema_version, payload_json)
             VALUES ($1, $2, 1, 1, '{}'::jsonb)",
        )
        .bind(TURN_ID)
        .bind(CAMPAIGN_ID)
        .execute(pool)
        .await
        .expect("turn fixture should insert");
    }

    async fn claim_narration(
        repository: &PostgresRepository,
        suffix: &str,
    ) -> crate::repository::jobs::ClaimedGenerationJob {
        let job_id = format!("presentation-job:{suffix}");
        repository
            .enqueue_generation_job(&NewGenerationJob {
                id: job_id.clone(),
                campaign_session_id: CAMPAIGN_ID.to_owned(),
                origin_turn_id: Some(TURN_ID.to_owned()),
                origin_campaign_revision: 2,
                purpose: GenerationPurpose::Narration,
                idempotency_key: format!("presentation-key:{suffix}"),
                input_digest: digest(1),
                prompt_digest: digest(2),
                policy_digest: digest(3),
                config_digest: digest(4),
                correlation_id: Some(format!("correlation:{suffix}")),
                max_attempts: 1,
                success_retention: SuccessRetention::UnselectedPresentation30Days,
                governance: None,
            })
            .await
            .expect("narration job should enqueue");
        repository
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
            .expect("claim should succeed")
            .expect("exact narration job should be ready")
    }

    fn presentation(
        claimed: &crate::repository::jobs::ClaimedGenerationJob,
        suffix: &str,
        source: GeneratedTextPresentationSource,
    ) -> NewGeneratedTextPresentation {
        NewGeneratedTextPresentation {
            id: format!("text-presentation:{suffix}"),
            campaign_session_id: CAMPAIGN_ID.to_owned(),
            origin_turn_id: TURN_ID.to_owned(),
            generation_job_id: claimed.job.id.clone(),
            generation_attempt_id: claimed.attempt.id.clone(),
            client_idempotency_key: format!("client-key:{suffix}"),
            source,
            body: format!("Safe narration version {suffix}."),
            config_digest: digest(4),
            prompt_digest: digest(2),
            policy_digest: digest(3),
            output_digest: digest(5),
            private_inspiration_work_id: None,
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn safe_fallback_completion_is_atomic_and_snapshots_exact_provenance(pool: PgPool) {
        seed_origin(&pool).await;
        let repository = PostgresRepository::from_pool(pool.clone());
        let claimed = claim_narration(&repository, "fallback").await;
        let requested = presentation(
            &claimed,
            "fallback",
            GeneratedTextPresentationSource::AuthoredFallback,
        );
        let stored = repository
            .finish_generation_with_text_presentation(
                &claimed.lease,
                &requested,
                &GenerationUsage::default(),
                Some(GenerationFailureCode::UnsafeOutput),
            )
            .await
            .expect("safe fallback and failure metadata should commit together");
        assert_eq!(stored.version, 1);
        assert!(stored.selected);
        assert_eq!(stored.output_digest, digest(5));

        let job = repository
            .load_generation_job(CAMPAIGN_ID, &claimed.job.id)
            .await
            .expect("job load should work")
            .expect("job should remain");
        assert_eq!(job.state, GenerationJobState::Failed);
        assert_eq!(job.output_digest, Some(digest(5)));
        let attempts = repository
            .list_generation_attempts(CAMPAIGN_ID, &claimed.job.id)
            .await
            .expect("attempt load should work");
        assert_eq!(attempts[0].output_digest, Some(digest(5)));

        let job_output: String =
            sqlx::query_scalar("SELECT output_digest FROM generation_jobs WHERE id = $1")
                .bind(&claimed.job.id)
                .fetch_one(&pool)
                .await
                .expect("job output digest should be queryable");
        let presentation_output: String = sqlx::query_scalar(
            "SELECT output_digest FROM generated_text_presentations WHERE id = $1",
        )
        .bind(&stored.id)
        .fetch_one(&pool)
        .await
        .expect("presentation output digest should be queryable");
        assert_eq!(job_output, presentation_output);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn client_replay_survives_policy_config_drift_without_spending_a_version(pool: PgPool) {
        seed_origin(&pool).await;
        let repository = PostgresRepository::from_pool(pool);
        let claimed = claim_narration(&repository, "replay").await;
        let requested = presentation(
            &claimed,
            "replay",
            GeneratedTextPresentationSource::Provider,
        );
        let first = repository
            .finish_generation_with_text_presentation(
                &claimed.lease,
                &requested,
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("first completion should commit");
        let replay = repository
            .finish_generation_with_text_presentation(
                &claimed.lease,
                &requested,
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("exact stale-lease replay should return the original row");
        assert_eq!(replay, first);
        let mut ambiguous_commit_retry = requested.clone();
        ambiguous_commit_retry.id = "text-presentation:fresh-recovery-uuid".to_owned();
        let recovered_after_ambiguous_commit = repository
            .finish_generation_with_text_presentation(
                &claimed.lease,
                &ambiguous_commit_retry,
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("a fresh proposed UUID must not break exact attempt replay");
        assert_eq!(recovered_after_ambiguous_commit, first);

        // Model a restarted deployment that would prepare a different full
        // generation key and digests for the same opaque client command. The
        // replay lookup must not depend on any of this current configuration.
        repository
            .enqueue_generation_job(&NewGenerationJob {
                id: "presentation-job:drifted-policy".to_owned(),
                campaign_session_id: CAMPAIGN_ID.to_owned(),
                origin_turn_id: Some(TURN_ID.to_owned()),
                origin_campaign_revision: 2,
                purpose: GenerationPurpose::Narration,
                idempotency_key: "narration:1:drifted-policy:client-key:replay".to_owned(),
                input_digest: digest(11),
                prompt_digest: digest(12),
                policy_digest: digest(13),
                config_digest: digest(14),
                correlation_id: Some("correlation:drifted-policy".to_owned()),
                max_attempts: 1,
                success_retention: SuccessRetention::UnselectedPresentation30Days,
                governance: None,
            })
            .await
            .expect("the simulated current-policy job should enqueue");
        let restarted = PostgresRepository::from_pool(repository.pool.clone());
        let recovered = restarted
            .load_generated_text_presentation_by_client_key(
                CAMPAIGN_ID,
                TURN_ID,
                "client-key:replay",
            )
            .await
            .expect("response-loss recovery query should work")
            .expect("the client key should recover its original presentation");
        assert_eq!(recovered, first);
        assert_eq!(recovered.policy_digest, digest(3));
        assert_eq!(recovered.config_digest, digest(4));
        let version_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM generated_text_presentations
             WHERE campaign_session_id = $1 AND origin_turn_id = $2",
        )
        .bind(CAMPAIGN_ID)
        .bind(TURN_ID)
        .fetch_one(&restarted.pool)
        .await
        .expect("version count should be queryable");
        assert_eq!(version_count, 1, "response replay must not spend a version");

        let mut conflict = requested;
        conflict.body = "Different body under the same generation attempt.".to_owned();
        assert!(matches!(
            repository
                .finish_generation_with_text_presentation(
                    &claimed.lease,
                    &conflict,
                    &GenerationUsage::default(),
                    None,
                )
                .await,
            Err(TextPresentationStoreError::IdempotencyConflict)
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn concurrent_regenerations_survive_restart_and_stop_after_three_versions(pool: PgPool) {
        seed_origin(&pool).await;
        let repository = PostgresRepository::from_pool(pool.clone());
        let initial = claim_narration(&repository, "initial").await;
        repository
            .finish_generation_with_text_presentation(
                &initial.lease,
                &presentation(
                    &initial,
                    "initial",
                    GeneratedTextPresentationSource::Provider,
                ),
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("initial version should commit");

        // Reconstructing the repository exercises the same query path used
        // after a process restart; no in-memory version counter exists.
        let restarted = PostgresRepository::from_pool(pool.clone());
        let second = claim_narration(&restarted, "second").await;
        let third = claim_narration(&restarted, "third").await;
        let second_request =
            presentation(&second, "second", GeneratedTextPresentationSource::Provider);
        let third_request =
            presentation(&third, "third", GeneratedTextPresentationSource::Provider);
        let second_usage = GenerationUsage::default();
        let third_usage = GenerationUsage::default();
        let (second_result, third_result) = tokio::join!(
            restarted.finish_generation_with_text_presentation(
                &second.lease,
                &second_request,
                &second_usage,
                None,
            ),
            restarted.finish_generation_with_text_presentation(
                &third.lease,
                &third_request,
                &third_usage,
                None,
            )
        );
        second_result.expect("one concurrent regeneration should commit");
        third_result.expect("the other concurrent regeneration should serialize and commit");

        let versions = restarted
            .list_generated_text_presentations(CAMPAIGN_ID, TURN_ID)
            .await
            .expect("versions should load after restart");
        assert_eq!(
            versions
                .iter()
                .map(|value| value.version)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(versions.iter().filter(|value| value.selected).count(), 1);
        assert!(
            versions
                .iter()
                .filter(|value| !value.selected)
                .all(|value| value.retention_delete_after.is_some())
        );
        let version_two = versions
            .iter()
            .find(|value| value.version == 2)
            .expect("version two should exist");
        let exact_snapshot = restarted
            .load_generated_text_presentation_replay(
                CAMPAIGN_ID,
                TURN_ID,
                &version_two.client_idempotency_key,
            )
            .await
            .expect("snapshot should load")
            .expect("version two receipt should exist");
        let GeneratedTextPresentationReplay::Available(exact_snapshot) = exact_snapshot else {
            panic!("retained version two should remain available");
        };
        assert_eq!(exact_snapshot.requested.version, 2);
        assert_eq!(exact_snapshot.requested.body, version_two.body);
        assert_eq!(
            exact_snapshot
                .retained_versions
                .iter()
                .find(|value| value.selected)
                .map(|value| value.version),
            Some(3)
        );

        let fourth = claim_narration(&restarted, "fourth").await;
        assert!(matches!(
            restarted
                .finish_generation_with_text_presentation(
                    &fourth.lease,
                    &presentation(&fourth, "fourth", GeneratedTextPresentationSource::Provider,),
                    &GenerationUsage::default(),
                    None,
                )
                .await,
            Err(TextPresentationStoreError::VersionLimitReached)
        ));
        let fourth_job = restarted
            .load_generation_job(CAMPAIGN_ID, &fourth.job.id)
            .await
            .expect("fourth job load should work")
            .expect("fourth job should remain");
        assert_eq!(fourth_job.state, GenerationJobState::Running);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn unsafe_body_is_rejected_and_expired_unselected_cleanup_is_idempotent(pool: PgPool) {
        seed_origin(&pool).await;
        let repository = PostgresRepository::from_pool(pool.clone());
        let unsafe_claim = claim_narration(&repository, "unsafe").await;
        let mut unsafe_presentation = presentation(
            &unsafe_claim,
            "unsafe",
            GeneratedTextPresentationSource::Provider,
        );
        unsafe_presentation.body = "<script>alert('unsafe')</script>".to_owned();
        assert!(matches!(
            repository
                .finish_generation_with_text_presentation(
                    &unsafe_claim.lease,
                    &unsafe_presentation,
                    &GenerationUsage::default(),
                    None,
                )
                .await,
            Err(TextPresentationStoreError::InvalidInput(_))
        ));

        let first = claim_narration(&repository, "cleanup-first").await;
        repository
            .finish_generation_with_text_presentation(
                &first.lease,
                &presentation(
                    &first,
                    "cleanup-first",
                    GeneratedTextPresentationSource::Provider,
                ),
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("first cleanup version should commit");
        let second = claim_narration(&repository, "cleanup-second").await;
        repository
            .finish_generation_with_text_presentation(
                &second.lease,
                &presentation(
                    &second,
                    "cleanup-second",
                    GeneratedTextPresentationSource::Provider,
                ),
                &GenerationUsage::default(),
                None,
            )
            .await
            .expect("second cleanup version should commit");
        sqlx::query(
            "UPDATE generated_text_presentations
             SET retention_delete_after = CURRENT_TIMESTAMP - INTERVAL '1 second'
             WHERE campaign_session_id = $1 AND origin_turn_id = $2 AND NOT selected",
        )
        .bind(CAMPAIGN_ID)
        .bind(TURN_ID)
        .execute(&pool)
        .await
        .expect("test expiry should update");
        assert_eq!(
            repository
                .delete_expired_generated_text_presentations(10)
                .await
                .expect("cleanup should succeed"),
            1
        );
        assert_eq!(
            repository
                .delete_expired_generated_text_presentations(10)
                .await
                .expect("cleanup replay should succeed"),
            0
        );
        let retained = repository
            .list_generated_text_presentations(CAMPAIGN_ID, TURN_ID)
            .await
            .expect("selected presentation should remain");
        assert_eq!(retained.len(), 1);
        assert!(retained[0].selected);

        let expired_replay = repository
            .load_generated_text_presentation_replay(
                CAMPAIGN_ID,
                TURN_ID,
                "client-key:cleanup-first",
            )
            .await
            .expect("expired alias lookup should work")
            .expect("campaign-lifetime alias should remain");
        let GeneratedTextPresentationReplay::Expired {
            receipt,
            retained_versions,
        } = expired_replay
        else {
            panic!("the deleted superseded body must return an expired receipt");
        };
        assert_eq!(receipt.version, 1);
        assert_eq!(retained_versions.len(), 1);
        assert_eq!(
            repository
                .generated_text_presentation_version_count(CAMPAIGN_ID, TURN_ID)
                .await
                .expect("body-free receipt count should load"),
            2
        );

        let stale = claim_narration(&repository, "cleanup-stale-replay").await;
        let mut stale_request = presentation(
            &stale,
            "cleanup-stale-replay",
            GeneratedTextPresentationSource::Provider,
        );
        stale_request.client_idempotency_key = "client-key:cleanup-first".to_owned();
        assert!(matches!(
            repository
                .finish_generation_with_text_presentation(
                    &stale.lease,
                    &stale_request,
                    &GenerationUsage::default(),
                    None,
                )
                .await,
            Err(TextPresentationStoreError::IdempotencyConflict)
        ));
        assert_eq!(
            repository
                .generated_text_presentation_version_count(CAMPAIGN_ID, TURN_ID)
                .await
                .expect("failed stale replay must not spend a receipt"),
            2
        );
        let selected_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM generated_text_presentations
             WHERE campaign_session_id = $1 AND origin_turn_id = $2 AND selected",
        )
        .bind(CAMPAIGN_ID)
        .bind(TURN_ID)
        .fetch_one(&pool)
        .await
        .expect("selection should remain queryable");
        assert_eq!(selected_count, 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn typed_intent_receipt_recovers_pending_commit_and_rejects_text_drift(pool: PgPool) {
        seed_origin(&pool).await;
        let repository = PostgresRepository::from_pool(pool.clone());
        let requested = NewTypedIntentCommandReceipt {
            campaign_session_id: CAMPAIGN_ID.to_owned(),
            client_idempotency_key: "typed-client:recover".to_owned(),
            player_intent_digest: digest(21),
            expected_campaign_revision: 1,
            expected_encounter_revision: 1,
            resolved_intent: EncounterIntent::StartEncounter,
            interpretation_label: "Begin initiative".to_owned(),
            interpretation_evidence_json: serde_json::json!({
                "source": "deterministic-fake",
                "proposal_fingerprint": digest(22).as_str(),
            })
            .to_string(),
        };
        let pending = repository
            .insert_pending_typed_intent_command_receipt(&requested)
            .await
            .expect("validated intent should be recoverable before mechanics commit");
        assert_eq!(pending.state, TypedIntentReceiptState::Pending);

        let restarted = PostgresRepository::from_pool(pool);
        let recovered_pending = restarted
            .load_typed_intent_command_receipt(CAMPAIGN_ID, "typed-client:recover")
            .await
            .expect("pending receipt should load after repository reconstruction")
            .expect("pending receipt should remain");
        assert_eq!(recovered_pending, pending);

        let mut drifted = requested.clone();
        drifted.player_intent_digest = digest(23);
        assert!(matches!(
            restarted
                .insert_pending_typed_intent_command_receipt(&drifted)
                .await,
            Err(TextPresentationStoreError::IdempotencyConflict)
        ));

        let committed = restarted
            .commit_typed_intent_command_receipt(
                CAMPAIGN_ID,
                "typed-client:recover",
                &digest(21),
                TURN_ID,
                1,
                2,
            )
            .await
            .expect("pending receipt should bind to the committed immutable turn");
        assert_eq!(committed.state, TypedIntentReceiptState::Committed);
        assert_eq!(committed.origin_turn_id.as_deref(), Some(TURN_ID));
        assert_eq!(committed.event_sequence, Some(1));
        assert_eq!(committed.result_campaign_revision, Some(2));
        assert_eq!(
            restarted
                .commit_typed_intent_command_receipt(
                    CAMPAIGN_ID,
                    "typed-client:recover",
                    &digest(21),
                    TURN_ID,
                    1,
                    2,
                )
                .await
                .expect("completion replay should return the exact committed receipt"),
            committed
        );
    }
}
