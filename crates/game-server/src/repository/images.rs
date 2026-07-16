//! Protected scene-image publication metadata.
//!
//! This module stores digests and relative protected-storage keys only. Image
//! bytes are validated and written by `scene_images`; they never enter SQL.

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use serde_json::json;
use sqlx::Row;

use super::{PostgresRepository, jobs::GenerationJobStoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneImageVariant {
    Web,
    Thumbnail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSceneImageArtifact {
    pub artifact_id: String,
    pub job_id: String,
    pub campaign_session_id: String,
    pub source_turn_id: String,
    pub brief_fingerprint: Sha256Digest,
    pub prompt_policy_fingerprint: Sha256Digest,
    pub config_fingerprint: Sha256Digest,
    pub provider: String,
    pub model: String,
    pub provider_request_id: Option<String>,
    pub original_storage_key: String,
    pub web_storage_key: String,
    pub thumbnail_storage_key: String,
    pub original_digest: Sha256Digest,
    pub web_digest: Sha256Digest,
    pub thumbnail_digest: Sha256Digest,
    pub original_width: u32,
    pub original_height: u32,
    pub web_width: u32,
    pub web_height: u32,
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
    pub alt_text: String,
    pub estimated_cost_microusd: u64,
    pub actual_cost_microusd: Option<u64>,
    pub license_id: String,
    pub provenance_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneImageArtifact {
    pub artifact_id: String,
    pub job_id: String,
    pub campaign_session_id: String,
    pub source_turn_id: String,
    pub web_storage_key: String,
    pub thumbnail_storage_key: String,
    pub web_digest: Sha256Digest,
    pub thumbnail_digest: Sha256Digest,
    pub alt_text: String,
    pub selected: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedSceneImageVariant {
    pub storage_key: String,
    pub digest: Sha256Digest,
    pub media_type: String,
    pub alt_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SceneImageRequestCounts {
    pub rolling_day: u64,
    pub campaign_lifetime: u64,
    pub source_turn: u64,
    pub active: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSceneImageQuarantine {
    pub id: String,
    pub job_id: String,
    pub attempt_id: String,
    pub campaign_session_id: String,
    pub byte_digest: Option<Sha256Digest>,
    pub byte_length: Option<u64>,
    pub storage_key: Option<String>,
    pub reason_code: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneImageCleanupCandidate {
    pub artifact_id: Option<String>,
    pub job_id: Option<String>,
    pub quarantine_id: Option<String>,
    pub storage_keys: Vec<String>,
}

impl PostgresRepository {
    pub async fn scene_image_request_counts(
        &self,
        campaign_session_id: &str,
        source_turn_id: Option<&str>,
    ) -> Result<SceneImageRequestCounts, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        if let Some(turn_id) = source_turn_id {
            validate_id(turn_id, "source turn id is invalid")?;
        }
        let row = sqlx::query(
            "SELECT
                COUNT(*)::BIGINT AS lifetime_count,
                COUNT(*) FILTER (
                    WHERE created_at > CURRENT_TIMESTAMP - INTERVAL '24 hours'
                )::BIGINT AS rolling_count,
                COUNT(*) FILTER (WHERE origin_turn_id = $2)::BIGINT AS turn_count,
                COUNT(*) FILTER (WHERE state = 'reserved')::BIGINT AS active_count
             FROM generation_governance_receipts
             WHERE campaign_session_id = $1 AND purpose = 'illustration'",
        )
        .bind(campaign_session_id)
        .bind(source_turn_id)
        .fetch_one(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        Ok(SceneImageRequestCounts {
            rolling_day: row_u64(&row, "rolling_count")?,
            campaign_lifetime: row_u64(&row, "lifetime_count")?,
            source_turn: row_u64(&row, "turn_count")?,
            active: row_u64(&row, "active_count")?,
        })
    }

    pub async fn latest_scene_image_job_id(
        &self,
        campaign_session_id: &str,
        source_turn_id: &str,
    ) -> Result<Option<String>, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        validate_id(source_turn_id, "source turn id is invalid")?;
        sqlx::query_scalar(
            "SELECT id FROM generation_jobs
             WHERE campaign_session_id = $1 AND origin_turn_id = $2
               AND purpose = 'illustration'
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(campaign_session_id)
        .bind(source_turn_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)
    }

    /// Upserts the one protected artifact slot owned by a running job. Delivery
    /// still requires the job to reach `succeeded`, so a crash here cannot make
    /// partially published bytes visible.
    pub async fn upsert_scene_image_artifact(
        &self,
        artifact: &NewSceneImageArtifact,
    ) -> Result<(), GenerationJobStoreError> {
        validate_new_artifact(artifact)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let owns_running_job: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM generation_jobs
                WHERE id = $1 AND campaign_session_id = $2
                  AND origin_turn_id = $3 AND purpose = 'illustration'
                  AND state = 'running'
             )",
        )
        .bind(&artifact.job_id)
        .bind(&artifact.campaign_session_id)
        .bind(&artifact.source_turn_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if !owns_running_job {
            return Err(GenerationJobStoreError::LostLease);
        }

        let metadata = json!({
            "width": artifact.web_width,
            "height": artifact.web_height,
            "media_type": "image/png",
            "provider_request_id": artifact.provider_request_id,
        });
        sqlx::query(
            "INSERT INTO generated_assets
             (id, campaign_session_id, turn_id, asset_kind, provider, model,
              location, prompt_fingerprint, metadata_json)
             VALUES ($1, $2, $3, 'scene-image', $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                campaign_session_id = EXCLUDED.campaign_session_id,
                turn_id = EXCLUDED.turn_id,
                asset_kind = EXCLUDED.asset_kind,
                provider = EXCLUDED.provider,
                model = EXCLUDED.model,
                location = EXCLUDED.location,
                prompt_fingerprint = EXCLUDED.prompt_fingerprint,
                metadata_json = EXCLUDED.metadata_json",
        )
        .bind(&artifact.artifact_id)
        .bind(&artifact.campaign_session_id)
        .bind(&artifact.source_turn_id)
        .bind(&artifact.provider)
        .bind(&artifact.model)
        .bind(&artifact.web_storage_key)
        .bind(artifact.brief_fingerprint.as_str())
        .bind(metadata)
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;

        let inserted = sqlx::query(
            "INSERT INTO scene_image_artifacts
             (artifact_id, job_id, campaign_session_id, source_turn_id,
              schema_version, brief_fingerprint, prompt_policy_fingerprint,
              config_fingerprint, original_storage_key, web_storage_key,
              thumbnail_storage_key, original_digest, web_digest,
              thumbnail_digest, media_type, original_width, original_height,
              web_width, web_height, thumbnail_width, thumbnail_height,
              alt_text, moderation_result, selection_state,
              estimated_cost_microusd, actual_cost_microusd, license_id,
              provenance_summary)
             VALUES
             ($1, $2, $3, $4, 1, $5, $6, $7, $8, $9, $10, $11, $12,
              $13, 'image/png', $14, $15, $16, $17, $18, $19, $20,
              'provider_and_application_safe', 'superseded', $21, $22, $23, $24)
             ON CONFLICT (artifact_id) DO UPDATE SET
              brief_fingerprint = EXCLUDED.brief_fingerprint,
              prompt_policy_fingerprint = EXCLUDED.prompt_policy_fingerprint,
              config_fingerprint = EXCLUDED.config_fingerprint,
              original_storage_key = EXCLUDED.original_storage_key,
              web_storage_key = EXCLUDED.web_storage_key,
              thumbnail_storage_key = EXCLUDED.thumbnail_storage_key,
              original_digest = EXCLUDED.original_digest,
              web_digest = EXCLUDED.web_digest,
              thumbnail_digest = EXCLUDED.thumbnail_digest,
              original_width = EXCLUDED.original_width,
              original_height = EXCLUDED.original_height,
              web_width = EXCLUDED.web_width,
              web_height = EXCLUDED.web_height,
              thumbnail_width = EXCLUDED.thumbnail_width,
              thumbnail_height = EXCLUDED.thumbnail_height,
              alt_text = EXCLUDED.alt_text,
              estimated_cost_microusd = EXCLUDED.estimated_cost_microusd,
              actual_cost_microusd = EXCLUDED.actual_cost_microusd,
              license_id = EXCLUDED.license_id,
              provenance_summary = EXCLUDED.provenance_summary
             WHERE scene_image_artifacts.job_id = EXCLUDED.job_id",
        )
        .bind(&artifact.artifact_id)
        .bind(&artifact.job_id)
        .bind(&artifact.campaign_session_id)
        .bind(&artifact.source_turn_id)
        .bind(artifact.brief_fingerprint.as_str())
        .bind(artifact.prompt_policy_fingerprint.as_str())
        .bind(artifact.config_fingerprint.as_str())
        .bind(&artifact.original_storage_key)
        .bind(&artifact.web_storage_key)
        .bind(&artifact.thumbnail_storage_key)
        .bind(artifact.original_digest.as_str())
        .bind(artifact.web_digest.as_str())
        .bind(artifact.thumbnail_digest.as_str())
        .bind(i64::from(artifact.original_width))
        .bind(i64::from(artifact.original_height))
        .bind(i64::from(artifact.web_width))
        .bind(i64::from(artifact.web_height))
        .bind(i64::from(artifact.thumbnail_width))
        .bind(i64::from(artifact.thumbnail_height))
        .bind(&artifact.alt_text)
        .bind(to_i64(artifact.estimated_cost_microusd)?)
        .bind(artifact.actual_cost_microusd.map(to_i64).transpose()?)
        .bind(&artifact.license_id)
        .bind(&artifact.provenance_summary)
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if inserted.rows_affected() != 1 {
            return Err(GenerationJobStoreError::IdempotencyConflict);
        }
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)
    }

    /// Selects a completed artifact and moves the prior version to Q10's
    /// thirty-day unselected retention class in one transaction.
    pub async fn select_scene_image_artifact(
        &self,
        campaign_session_id: &str,
        source_turn_id: &str,
        artifact_id: &str,
    ) -> Result<(), GenerationJobStoreError> {
        for value in [campaign_session_id, source_turn_id, artifact_id] {
            validate_id(value, "scene image identifier is invalid")?;
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let eligible: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM scene_image_artifacts AS artifact
                JOIN generation_jobs AS job ON job.id = artifact.job_id
                WHERE artifact.artifact_id = $1
                  AND artifact.campaign_session_id = $2
                  AND artifact.source_turn_id = $3
                  AND job.state = 'succeeded' AND job.artifact_id = artifact.artifact_id
             )",
        )
        .bind(artifact_id)
        .bind(campaign_session_id)
        .bind(source_turn_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if !eligible {
            return Err(GenerationJobStoreError::InvalidTransition {
                job_id: artifact_id.to_owned(),
                state: super::jobs::GenerationJobState::Running,
            });
        }
        sqlx::query(
            "UPDATE scene_image_artifacts
             SET selection_state = 'superseded'
             WHERE campaign_session_id = $1 AND source_turn_id = $2
               AND selection_state = 'selected' AND artifact_id <> $3",
        )
        .bind(campaign_session_id)
        .bind(source_turn_id)
        .bind(artifact_id)
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        sqlx::query(
            "UPDATE generation_jobs AS job
             SET success_retention_class = 'unselected_presentation_30d',
                 retention_class = 'unselected_presentation_30d',
                 retention_delete_after = COALESCE(
                    retention_delete_after,
                    CURRENT_TIMESTAMP + INTERVAL '30 days'
                 ),
                 updated_at = CURRENT_TIMESTAMP
             FROM scene_image_artifacts AS artifact
             WHERE artifact.job_id = job.id
               AND artifact.campaign_session_id = $1
               AND artifact.source_turn_id = $2
               AND artifact.artifact_id <> $3
               AND job.state = 'succeeded'",
        )
        .bind(campaign_session_id)
        .bind(source_turn_id)
        .bind(artifact_id)
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        let selected = sqlx::query(
            "UPDATE scene_image_artifacts SET selection_state = 'selected'
             WHERE artifact_id = $1 AND campaign_session_id = $2 AND source_turn_id = $3",
        )
        .bind(artifact_id)
        .bind(campaign_session_id)
        .bind(source_turn_id)
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if selected.rows_affected() != 1 {
            return Err(GenerationJobStoreError::InvalidStoredData(
                "completed scene image artifact was not found",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)
    }

    pub async fn scene_image_artifact_for_job(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<Option<SceneImageArtifact>, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        validate_id(job_id, "job id is invalid")?;
        sqlx::query(
            "SELECT artifact.artifact_id, artifact.job_id,
                    artifact.campaign_session_id, artifact.source_turn_id,
                    artifact.web_storage_key, artifact.thumbnail_storage_key,
                    artifact.web_digest, artifact.thumbnail_digest,
                    artifact.alt_text, artifact.selection_state,
                    artifact.created_at::TEXT AS created_at
             FROM scene_image_artifacts AS artifact
             JOIN generation_jobs AS job ON job.id = artifact.job_id
             WHERE artifact.job_id = $1 AND artifact.campaign_session_id = $2
               AND job.state = 'succeeded' AND job.artifact_id = artifact.artifact_id",
        )
        .bind(job_id)
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?
        .map(artifact_from_row)
        .transpose()
    }

    pub async fn authorized_scene_image_variant(
        &self,
        campaign_session_id: &str,
        artifact_id: &str,
        variant: SceneImageVariant,
    ) -> Result<Option<AuthorizedSceneImageVariant>, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        validate_id(artifact_id, "artifact id is invalid")?;
        let (key_column, digest_column) = match variant {
            SceneImageVariant::Web => ("web_storage_key", "web_digest"),
            SceneImageVariant::Thumbnail => ("thumbnail_storage_key", "thumbnail_digest"),
        };
        let row = sqlx::query(&format!(
            "SELECT artifact.{key_column} AS storage_key,
                    artifact.{digest_column} AS digest,
                    artifact.media_type, artifact.alt_text
             FROM scene_image_artifacts AS artifact
             JOIN generation_jobs AS job ON job.id = artifact.job_id
             WHERE artifact.artifact_id = $1 AND artifact.campaign_session_id = $2
               AND job.state = 'succeeded' AND job.artifact_id = artifact.artifact_id
               AND artifact.moderation_result = 'provider_and_application_safe'"
        ))
        .bind(artifact_id)
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        row.map(|row| {
            Ok(AuthorizedSceneImageVariant {
                storage_key: row
                    .try_get("storage_key")
                    .map_err(GenerationJobStoreError::Database)?,
                digest: digest_from_row(&row, "digest")?,
                media_type: row
                    .try_get("media_type")
                    .map_err(GenerationJobStoreError::Database)?,
                alt_text: row
                    .try_get("alt_text")
                    .map_err(GenerationJobStoreError::Database)?,
            })
        })
        .transpose()
    }

    pub async fn record_scene_image_quarantine(
        &self,
        quarantine: &NewSceneImageQuarantine,
    ) -> Result<(), GenerationJobStoreError> {
        for value in [
            quarantine.id.as_str(),
            quarantine.job_id.as_str(),
            quarantine.attempt_id.as_str(),
            quarantine.campaign_session_id.as_str(),
        ] {
            validate_id(value, "quarantine identifier is invalid")?;
        }
        if !matches!(
            quarantine.reason_code,
            "provider_url_rejected"
                | "base64_invalid"
                | "byte_limit"
                | "format_invalid"
                | "dimensions_invalid"
                | "pixel_limit"
                | "decode_failed"
                | "safety_rejected"
        ) || quarantine
            .storage_key
            .as_deref()
            .is_some_and(|key| !valid_storage_key(key))
            || quarantine
                .byte_length
                .is_some_and(|length| length > 32 * 1024 * 1024)
        {
            return Err(GenerationJobStoreError::InvalidInput(
                "scene image quarantine metadata is invalid",
            ));
        }
        sqlx::query(
            "INSERT INTO scene_image_quarantines
             (id, job_id, attempt_id, campaign_session_id, byte_digest,
              byte_length, storage_key, reason_code)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (job_id, attempt_id) DO NOTHING",
        )
        .bind(&quarantine.id)
        .bind(&quarantine.job_id)
        .bind(&quarantine.attempt_id)
        .bind(&quarantine.campaign_session_id)
        .bind(quarantine.byte_digest.as_ref().map(Sha256Digest::as_str))
        .bind(quarantine.byte_length.map(to_i64).transpose()?)
        .bind(quarantine.storage_key.as_deref())
        .bind(quarantine.reason_code)
        .execute(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        Ok(())
    }

    pub async fn expired_scene_image_cleanup_candidates(
        &self,
        limit: u16,
    ) -> Result<Vec<SceneImageCleanupCandidate>, GenerationJobStoreError> {
        if limit == 0 || limit > 1_000 {
            return Err(GenerationJobStoreError::InvalidInput(
                "image cleanup limit must be between one and one thousand",
            ));
        }
        let artifact_rows = sqlx::query(
            "SELECT artifact.artifact_id, artifact.job_id,
                    artifact.original_storage_key, artifact.web_storage_key,
                    artifact.thumbnail_storage_key
             FROM scene_image_artifacts AS artifact
             JOIN generation_jobs AS job ON job.id = artifact.job_id
             WHERE job.state = 'succeeded'
               AND job.retention_delete_after <= CURRENT_TIMESTAMP
             ORDER BY job.retention_delete_after, artifact.artifact_id
             LIMIT $1",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        let mut candidates = artifact_rows
            .into_iter()
            .map(|row| {
                Ok(SceneImageCleanupCandidate {
                    artifact_id: Some(
                        row.try_get("artifact_id")
                            .map_err(GenerationJobStoreError::Database)?,
                    ),
                    job_id: Some(
                        row.try_get("job_id")
                            .map_err(GenerationJobStoreError::Database)?,
                    ),
                    quarantine_id: None,
                    storage_keys: vec![
                        row.try_get("original_storage_key")
                            .map_err(GenerationJobStoreError::Database)?,
                        row.try_get("web_storage_key")
                            .map_err(GenerationJobStoreError::Database)?,
                        row.try_get("thumbnail_storage_key")
                            .map_err(GenerationJobStoreError::Database)?,
                    ],
                })
            })
            .collect::<Result<Vec<_>, GenerationJobStoreError>>()?;
        let remaining = usize::from(limit).saturating_sub(candidates.len());
        if remaining > 0 {
            let quarantine_rows = sqlx::query(
                "SELECT id, storage_key FROM scene_image_quarantines
                 WHERE delete_after <= CURRENT_TIMESTAMP
                 ORDER BY delete_after, id LIMIT $1",
            )
            .bind(i64::try_from(remaining).map_err(|_| GenerationJobStoreError::NumericRange)?)
            .fetch_all(&self.pool)
            .await
            .map_err(GenerationJobStoreError::Database)?;
            for row in quarantine_rows {
                let key: Option<String> = row
                    .try_get("storage_key")
                    .map_err(GenerationJobStoreError::Database)?;
                candidates.push(SceneImageCleanupCandidate {
                    artifact_id: None,
                    job_id: None,
                    quarantine_id: Some(
                        row.try_get("id")
                            .map_err(GenerationJobStoreError::Database)?,
                    ),
                    storage_keys: key.into_iter().collect(),
                });
            }
        }
        Ok(candidates)
    }

    pub async fn delete_scene_image_cleanup_candidate(
        &self,
        candidate: &SceneImageCleanupCandidate,
    ) -> Result<bool, GenerationJobStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let deleted = match (
            candidate.artifact_id.as_deref(),
            candidate.job_id.as_deref(),
            candidate.quarantine_id.as_deref(),
        ) {
            (Some(artifact_id), Some(job_id), None) => {
                let job = sqlx::query(
                    "DELETE FROM generation_jobs
                     WHERE id = $1 AND state = 'succeeded'
                       AND retention_delete_after <= CURRENT_TIMESTAMP",
                )
                .bind(job_id)
                .execute(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?;
                if job.rows_affected() == 1 {
                    sqlx::query("DELETE FROM generated_assets WHERE id = $1")
                        .bind(artifact_id)
                        .execute(&mut *transaction)
                        .await
                        .map_err(GenerationJobStoreError::Database)?;
                    true
                } else {
                    false
                }
            }
            (None, None, Some(quarantine_id)) => {
                sqlx::query(
                    "DELETE FROM scene_image_quarantines
                 WHERE id = $1 AND delete_after <= CURRENT_TIMESTAMP",
                )
                .bind(quarantine_id)
                .execute(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?
                .rows_affected()
                    == 1
            }
            _ => {
                return Err(GenerationJobStoreError::InvalidInput(
                    "image cleanup candidate shape is invalid",
                ));
            }
        };
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(deleted)
    }
}

fn validate_new_artifact(artifact: &NewSceneImageArtifact) -> Result<(), GenerationJobStoreError> {
    for value in [
        artifact.artifact_id.as_str(),
        artifact.job_id.as_str(),
        artifact.campaign_session_id.as_str(),
        artifact.source_turn_id.as_str(),
        artifact.provider.as_str(),
    ] {
        validate_id(value, "scene image identifier is invalid")?;
    }
    if artifact.model.trim() != artifact.model
        || artifact.model.is_empty()
        || artifact.model.chars().count() > 256
        || artifact.model.chars().any(char::is_control)
        || artifact
            .provider_request_id
            .as_deref()
            .is_some_and(|id| !is_valid_opaque_id(id))
        || [
            artifact.original_storage_key.as_str(),
            artifact.web_storage_key.as_str(),
            artifact.thumbnail_storage_key.as_str(),
        ]
        .into_iter()
        .any(|key| !valid_storage_key(key))
        || artifact.original_width == 0
        || artifact.original_width > 4_096
        || artifact.original_height == 0
        || artifact.original_height > 4_096
        || artifact.web_width == 0
        || artifact.web_width > 1_600
        || artifact.web_height == 0
        || artifact.web_height > 1_600
        || artifact.thumbnail_width == 0
        || artifact.thumbnail_width > 512
        || artifact.thumbnail_height == 0
        || artifact.thumbnail_height > 512
        || artifact.alt_text.trim() != artifact.alt_text
        || artifact.alt_text.is_empty()
        || artifact.alt_text.chars().count() > 500
        || artifact.alt_text.chars().any(char::is_control)
        || !matches!(
            artifact.license_id.as_str(),
            "provider-output-operator-terms" | "deterministic-fake-fixture"
        )
        || !matches!(
            artifact.provenance_summary.as_str(),
            "generated-from-committed-public-fictional-facts"
                | "deterministic-network-free-test-fixture"
        )
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "scene image artifact metadata is invalid",
        ));
    }
    Ok(())
}

fn artifact_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<SceneImageArtifact, GenerationJobStoreError> {
    Ok(SceneImageArtifact {
        artifact_id: row
            .try_get("artifact_id")
            .map_err(GenerationJobStoreError::Database)?,
        job_id: row
            .try_get("job_id")
            .map_err(GenerationJobStoreError::Database)?,
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(GenerationJobStoreError::Database)?,
        source_turn_id: row
            .try_get("source_turn_id")
            .map_err(GenerationJobStoreError::Database)?,
        web_storage_key: row
            .try_get("web_storage_key")
            .map_err(GenerationJobStoreError::Database)?,
        thumbnail_storage_key: row
            .try_get("thumbnail_storage_key")
            .map_err(GenerationJobStoreError::Database)?,
        web_digest: digest_from_row(&row, "web_digest")?,
        thumbnail_digest: digest_from_row(&row, "thumbnail_digest")?,
        alt_text: row
            .try_get("alt_text")
            .map_err(GenerationJobStoreError::Database)?,
        selected: row
            .try_get::<String, _>("selection_state")
            .map_err(GenerationJobStoreError::Database)?
            == "selected",
        created_at: row
            .try_get("created_at")
            .map_err(GenerationJobStoreError::Database)?,
    })
}

fn digest_from_row(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> Result<Sha256Digest, GenerationJobStoreError> {
    Sha256Digest::new(
        row.try_get::<String, _>(column)
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid image digest"))
}

fn row_u64(row: &sqlx::postgres::PgRow, column: &str) -> Result<u64, GenerationJobStoreError> {
    u64::try_from(
        row.try_get::<i64, _>(column)
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::NumericRange)
}

fn to_i64(value: u64) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn validate_id(value: &str, reason: &'static str) -> Result<(), GenerationJobStoreError> {
    if is_valid_opaque_id(value) {
        Ok(())
    } else {
        Err(GenerationJobStoreError::InvalidInput(reason))
    }
}

fn valid_storage_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.starts_with('/')
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}
