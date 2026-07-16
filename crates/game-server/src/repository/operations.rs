//! Read-only database recovery manifests and bounded operational snapshots.
//!
//! These queries are for an offline backup/operator process. They expose no
//! campaign prose, prompts, provider responses, credentials, or source text.

use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

use manchester_dnd_core::Sha256Digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use thiserror::Error;

use super::{CampaignLifecycleState, PostgresRepository};
use crate::error::RepositoryError;

pub const DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const DATABASE_OPERATIONS_SNAPSHOT_SCHEMA_VERSION: u16 = 2;
const MAX_RECOVERY_FILE_BYTES: u64 = 64 * 1024 * 1024;
const TURN_EVENT_OUTCOME_LABELS: &[&str] = &[
    "session_started",
    "player_intent",
    "dice_resolved",
    "ability_check_resolved",
    "exploration_social_resolved",
    "encounter_resolved",
    "gm_narration",
    "experience_awarded",
    "ai_proposal_accepted",
    "ai_proposal_rejected",
    "session_ended",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryMigrationManifestEntry {
    pub version: i64,
    pub description: String,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCampaignManifestEntry {
    pub campaign_session_id: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub lifecycle_state: CampaignLifecycleState,
    pub stable_state_digest: Sha256Digest,
    pub character_count: u64,
    pub hero_revision: Option<u64>,
    pub turn_count: u64,
    pub latest_turn_number: Option<u64>,
    pub command_receipt_count: u64,
    pub hero_audit_count: u64,
    pub lifecycle_audit_count: u64,
    pub private_recap_count: u64,
    pub selected_text_presentation_count: u64,
    pub selected_generated_asset_count: u64,
    pub has_validated_content_pins: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryArtifactFileEntry {
    pub campaign_session_id: String,
    pub artifact_id: String,
    pub variant: String,
    pub storage_key: String,
    pub expected_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseRecoveryManifest {
    pub schema_version: u16,
    pub migrations: Vec<RecoveryMigrationManifestEntry>,
    pub campaigns: Vec<RecoveryCampaignManifestEntry>,
    pub selected_artifact_files: Vec<RecoveryArtifactFileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedRecoveryFile {
    pub kind: String,
    pub storage_key: String,
    pub byte_count: u64,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRecoveryManifest {
    pub schema_version: u16,
    pub database: DatabaseRecoveryManifest,
    pub rng_master_key: VerifiedRecoveryFile,
    pub selected_artifact_files: Vec<VerifiedRecoveryFile>,
}

#[derive(Debug, Error)]
pub enum RecoveryManifestError {
    #[error("database recovery manifest failed")]
    Repository(#[from] RepositoryError),
    #[error("protected recovery file storage failed")]
    Io(#[source] std::io::Error),
    #[error("protected recovery file validation failed: {0}")]
    Invalid(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationQueueStateCount {
    pub state: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalOutcomeCount {
    pub outcome: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationBudgetDenialCount {
    pub purpose: String,
    pub scope: String,
    pub dimension: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseOperationsSnapshot {
    pub schema_version: u16,
    pub captured_at: String,
    pub latest_migration_version: Option<i64>,
    pub database_bytes: u64,
    pub index_bytes: u64,
    pub wal_bytes: Option<u64>,
    pub configured_max_connections: u64,
    pub database_connections: u64,
    pub active_connections: u64,
    pub waiting_connections: u64,
    pub long_transaction_count: u64,
    pub oldest_transaction_seconds: Option<f64>,
    pub row_lock_wait_count: u64,
    pub deadlock_count: u64,
    pub cumulative_block_read_milliseconds: f64,
    pub cumulative_block_write_milliseconds: f64,
    pub never_analyzed_table_count: u64,
    pub maximum_dead_tuple_count: u64,
    pub replication_client_count: Option<u64>,
    pub maximum_replication_lag_bytes: Option<u64>,
    pub turn_event_outcomes: Vec<OperationalOutcomeCount>,
    pub hero_event_outcomes: Vec<OperationalOutcomeCount>,
    pub lifecycle_event_outcomes: Vec<OperationalOutcomeCount>,
    pub generation_queue: Vec<GenerationQueueStateCount>,
    pub generation_attempt_outcomes: Vec<OperationalOutcomeCount>,
    pub generation_attempt_count: u64,
    pub generation_total_tokens: u64,
    pub generation_total_latency_milliseconds: u64,
    pub generation_maximum_latency_milliseconds: Option<u64>,
    pub generation_total_cost_microusd: u64,
    pub generation_budget_denials: Vec<GenerationBudgetDenialCount>,
    pub oldest_queued_job_seconds: Option<f64>,
    pub expired_generation_lease_count: u64,
    pub last_backup_completed_at: Option<String>,
    pub last_backup_vault_digest: Option<Sha256Digest>,
    pub last_restore_test_completed_at: Option<String>,
    pub last_restore_test_result: Option<String>,
    pub last_restore_source_digest: Option<Sha256Digest>,
}

impl PostgresRepository {
    pub async fn database_recovery_manifest(
        &self,
        owner_key: &str,
    ) -> Result<DatabaseRecoveryManifest, RepositoryError> {
        let migration_rows = sqlx::query(
            "SELECT version, description, success
             FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let migrations = migration_rows
            .into_iter()
            .map(|row| {
                Ok(RecoveryMigrationManifestEntry {
                    version: row.try_get("version").map_err(RepositoryError::Database)?,
                    description: row
                        .try_get("description")
                        .map_err(RepositoryError::Database)?,
                    success: row.try_get("success").map_err(RepositoryError::Database)?,
                })
            })
            .collect::<Result<Vec<_>, RepositoryError>>()?;

        let mut summaries = self.list_owned_campaigns(owner_key).await?;
        summaries.sort_by(|left, right| left.campaign_session_id.cmp(&right.campaign_session_id));
        let mut campaigns = Vec::with_capacity(summaries.len());
        for summary in summaries {
            let exported = self
                .export_campaign_private(owner_key, &summary.campaign_session_id)
                .await?;
            let mut stable_value =
                serde_json::to_value(&exported).map_err(|source| RepositoryError::Serialize {
                    entity: "database recovery state",
                    source,
                })?;
            stable_value
                .as_object_mut()
                .ok_or_else(|| RepositoryError::InvalidDomainState {
                    entity: "database recovery state",
                    id: summary.campaign_session_id.clone(),
                    reason: "campaign export did not serialize as an object",
                })?
                .remove("exported_at");
            let stable_bytes =
                serde_json::to_vec(&stable_value).map_err(|source| RepositoryError::Serialize {
                    entity: "database recovery state",
                    source,
                })?;
            let hero_revision = exported.hero_character.as_ref().map(|hero| hero.revision);
            campaigns.push(RecoveryCampaignManifestEntry {
                campaign_session_id: summary.campaign_session_id,
                campaign_revision: summary.campaign_revision,
                lifecycle_revision: summary.lifecycle_revision,
                lifecycle_state: summary.lifecycle_state,
                stable_state_digest: digest(&stable_bytes),
                character_count: usize_to_u64(exported.characters.len())?,
                hero_revision,
                turn_count: usize_to_u64(exported.turns.len())?,
                latest_turn_number: exported.turns.last().map(|turn| turn.turn_number),
                command_receipt_count: usize_to_u64(exported.command_receipts.len())?,
                hero_audit_count: usize_to_u64(exported.hero_audits.len())?,
                lifecycle_audit_count: usize_to_u64(exported.lifecycle_audits.len())?,
                private_recap_count: usize_to_u64(exported.private_recaps.len())?,
                selected_text_presentation_count: usize_to_u64(
                    exported.selected_text_presentations.len(),
                )?,
                selected_generated_asset_count: usize_to_u64(
                    exported.selected_generated_assets.len(),
                )?,
                has_validated_content_pins: exported.content_pins.is_some(),
            });
        }

        let artifact_rows = sqlx::query(
            "SELECT campaign_session_id, artifact_id,
                    original_storage_key, original_digest,
                    web_storage_key, web_digest,
                    thumbnail_storage_key, thumbnail_digest
             FROM scene_image_artifacts
             WHERE selection_state = 'selected'
               AND campaign_session_id IN (
                   SELECT id FROM campaign_sessions WHERE owner_key = $1
               )
             ORDER BY campaign_session_id, artifact_id",
        )
        .bind(owner_key)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let mut selected_artifact_files = Vec::with_capacity(artifact_rows.len() * 3);
        for row in artifact_rows {
            let campaign_session_id: String = row
                .try_get("campaign_session_id")
                .map_err(RepositoryError::Database)?;
            let artifact_id: String = row
                .try_get("artifact_id")
                .map_err(RepositoryError::Database)?;
            for (variant, key_column, digest_column) in [
                ("original", "original_storage_key", "original_digest"),
                ("web", "web_storage_key", "web_digest"),
                ("thumbnail", "thumbnail_storage_key", "thumbnail_digest"),
            ] {
                let storage_key: String =
                    row.try_get(key_column).map_err(RepositoryError::Database)?;
                let digest_text: String = row
                    .try_get(digest_column)
                    .map_err(RepositoryError::Database)?;
                let expected_digest = Sha256Digest::new(digest_text).map_err(|_| {
                    RepositoryError::InvalidDomainState {
                        entity: "scene image recovery digest",
                        id: artifact_id.clone(),
                        reason: "stored digest is invalid",
                    }
                })?;
                selected_artifact_files.push(RecoveryArtifactFileEntry {
                    campaign_session_id: campaign_session_id.clone(),
                    artifact_id: artifact_id.clone(),
                    variant: variant.to_owned(),
                    storage_key,
                    expected_digest,
                });
            }
        }
        Ok(DatabaseRecoveryManifest {
            schema_version: DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION,
            migrations,
            campaigns,
            selected_artifact_files,
        })
    }

    pub async fn database_operations_snapshot(
        &self,
    ) -> Result<DatabaseOperationsSnapshot, RepositoryError> {
        let storage = sqlx::query(
            "SELECT CURRENT_TIMESTAMP::text AS captured_at,
                    pg_database_size(current_database())::bigint AS database_bytes,
                    COALESCE((SELECT SUM(pg_relation_size(indexrelid))::bigint
                              FROM pg_stat_user_indexes), 0) AS index_bytes,
                    current_setting('max_connections')::bigint AS max_connections,
                    (SELECT MAX(version) FROM _sqlx_migrations) AS migration_version",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let activity = sqlx::query(
            "SELECT COUNT(*)::bigint AS total,
                    COUNT(*) FILTER (WHERE state = 'active')::bigint AS active,
                    COUNT(*) FILTER (WHERE wait_event IS NOT NULL)::bigint AS waiting,
                    COUNT(*) FILTER (
                        WHERE xact_start < CURRENT_TIMESTAMP - INTERVAL '30 seconds'
                    )::bigint AS long_transactions,
                    (MAX(EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - xact_start)))
                        FILTER (WHERE xact_start IS NOT NULL))::float8
                        AS oldest_transaction_seconds,
                    COUNT(*) FILTER (WHERE wait_event_type = 'Lock')::bigint AS lock_waits
             FROM pg_stat_activity WHERE datname = current_database()",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let database_stats = sqlx::query(
            "SELECT deadlocks::bigint AS deadlocks,
                    blk_read_time::float8 AS block_read_ms,
                    blk_write_time::float8 AS block_write_ms
             FROM pg_stat_database WHERE datname = current_database()",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let table_stats = sqlx::query(
            "SELECT COUNT(*) FILTER (
                        WHERE last_analyze IS NULL AND last_autoanalyze IS NULL
                    )::bigint AS never_analyzed,
                    COALESCE(MAX(n_dead_tup), 0)::bigint AS max_dead_tuples
             FROM pg_stat_user_tables",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let queue_rows = sqlx::query(
            "SELECT state, COUNT(*)::bigint AS count
             FROM generation_jobs GROUP BY state ORDER BY state",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let generation_queue = queue_rows
            .into_iter()
            .map(|row| {
                let count: i64 = row.try_get("count").map_err(RepositoryError::Database)?;
                Ok(GenerationQueueStateCount {
                    state: row.try_get("state").map_err(RepositoryError::Database)?,
                    count: non_negative_i64(count, "generation queue count")?,
                })
            })
            .collect::<Result<Vec<_>, RepositoryError>>()?;
        let turn_event_rows = sqlx::query(
            "SELECT payload_json->'payload'->>'type' AS outcome,
                    COUNT(*)::bigint AS count
             FROM turn_audits
             GROUP BY payload_json->'payload'->>'type' ORDER BY outcome",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let turn_event_outcomes = closed_outcome_counts(
            turn_event_rows,
            TURN_EVENT_OUTCOME_LABELS,
            "turn event outcome",
        )?;
        let hero_event_rows = sqlx::query(
            "SELECT audit_kind AS outcome, COUNT(*)::bigint AS count
             FROM hero_audits GROUP BY audit_kind ORDER BY audit_kind",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let hero_event_outcomes = closed_outcome_counts(
            hero_event_rows,
            &["creation_transition", "reward_awarded", "level_up"],
            "hero event outcome",
        )?;
        let lifecycle_event_rows = sqlx::query(
            "SELECT event_kind AS outcome, COUNT(*)::bigint AS count
             FROM campaign_lifecycle_audits GROUP BY event_kind ORDER BY event_kind",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let lifecycle_event_outcomes = closed_outcome_counts(
            lifecycle_event_rows,
            &[
                "play_started",
                "play_ended",
                "archived",
                "restored",
                "restore_imported",
            ],
            "lifecycle event outcome",
        )?;
        let generation_attempt_rows = sqlx::query(
            "SELECT COALESCE(failure_code, state) AS outcome, COUNT(*)::bigint AS count
             FROM generation_attempts
             GROUP BY COALESCE(failure_code, state) ORDER BY outcome",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let generation_attempt_outcomes = closed_outcome_counts(
            generation_attempt_rows,
            &[
                "running",
                "succeeded",
                "failed",
                "cancelled",
                "timeout",
                "provider_unavailable",
                "rate_limited",
                "provider_rejected",
                "malformed_response",
                "unsafe_output",
                "contradiction",
                "invalid_artifact",
                "budget_exceeded",
                "lease_expired",
            ],
            "generation attempt outcome",
        )?;
        let generation_usage = sqlx::query(
            "SELECT COUNT(*)::bigint AS attempt_count,
                    COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                    COALESCE(SUM(latency_milliseconds), 0)::bigint
                        AS total_latency_milliseconds,
                    MAX(latency_milliseconds)::bigint AS maximum_latency_milliseconds,
                    COALESCE(SUM(cost_microusd), 0)::bigint AS total_cost_microusd
             FROM generation_attempts",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let generation_budget_rows = sqlx::query(
            "SELECT purpose, budget_scope, budget_dimension,
                    COUNT(*)::bigint AS count
             FROM generation_governance_diagnostics
             GROUP BY purpose, budget_scope, budget_dimension
             ORDER BY purpose, budget_scope, budget_dimension",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let generation_budget_denials = generation_budget_rows
            .into_iter()
            .map(|row| {
                let purpose: String = row.try_get("purpose").map_err(RepositoryError::Database)?;
                let scope: String = row
                    .try_get("budget_scope")
                    .map_err(RepositoryError::Database)?;
                let dimension: String = row
                    .try_get("budget_dimension")
                    .map_err(RepositoryError::Database)?;
                if !matches!(
                    purpose.as_str(),
                    "intent_parsing" | "gm_planning" | "narration" | "illustration"
                ) || !matches!(scope.as_str(), "turn" | "campaign" | "concurrency")
                    || !matches!(
                        dimension.as_str(),
                        "requests" | "tokens" | "latency" | "cost" | "concurrency"
                    )
                {
                    return Err(RepositoryError::InvalidDomainState {
                        entity: "generation budget metric",
                        id: "aggregate".to_owned(),
                        reason: "stored operational label is invalid",
                    });
                }
                let count: i64 = row.try_get("count").map_err(RepositoryError::Database)?;
                Ok(GenerationBudgetDenialCount {
                    purpose,
                    scope,
                    dimension,
                    count: non_negative_i64(count, "generation budget denial count")?,
                })
            })
            .collect::<Result<Vec<_>, RepositoryError>>()?;
        let queue_health = sqlx::query(
            "SELECT (MAX(EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - created_at)))
                        FILTER (WHERE state = 'queued'))::float8 AS oldest_queued_seconds,
                    COUNT(*) FILTER (
                        WHERE state = 'running' AND lease_expires_at <= CURRENT_TIMESTAMP
                    )::bigint AS expired_leases
             FROM generation_jobs",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let recovery = sqlx::query(
            "SELECT last_backup_completed_at::text AS last_backup_completed_at,
                    last_backup_vault_digest,
                    last_restore_test_completed_at::text AS last_restore_test_completed_at,
                    last_restore_test_result, last_restore_source_digest
             FROM operator_recovery_status WHERE singleton",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;

        let wal_bytes = optional_wal_bytes(&self.pool).await;
        let (replication_client_count, maximum_replication_lag_bytes) =
            optional_replication_health(&self.pool).await;
        let database_bytes: i64 = storage
            .try_get("database_bytes")
            .map_err(RepositoryError::Database)?;
        let index_bytes: i64 = storage
            .try_get("index_bytes")
            .map_err(RepositoryError::Database)?;
        let configured_max_connections: i64 = storage
            .try_get("max_connections")
            .map_err(RepositoryError::Database)?;
        let total_connections: i64 = activity
            .try_get("total")
            .map_err(RepositoryError::Database)?;
        let active_connections: i64 = activity
            .try_get("active")
            .map_err(RepositoryError::Database)?;
        let waiting_connections: i64 = activity
            .try_get("waiting")
            .map_err(RepositoryError::Database)?;
        let long_transactions: i64 = activity
            .try_get("long_transactions")
            .map_err(RepositoryError::Database)?;
        let lock_waits: i64 = activity
            .try_get("lock_waits")
            .map_err(RepositoryError::Database)?;
        let deadlocks: i64 = database_stats
            .try_get("deadlocks")
            .map_err(RepositoryError::Database)?;
        let never_analyzed: i64 = table_stats
            .try_get("never_analyzed")
            .map_err(RepositoryError::Database)?;
        let max_dead_tuples: i64 = table_stats
            .try_get("max_dead_tuples")
            .map_err(RepositoryError::Database)?;
        let expired_leases: i64 = queue_health
            .try_get("expired_leases")
            .map_err(RepositoryError::Database)?;
        let recovery_fields = recovery.as_ref().map(recovery_status_fields).transpose()?;
        Ok(DatabaseOperationsSnapshot {
            schema_version: DATABASE_OPERATIONS_SNAPSHOT_SCHEMA_VERSION,
            captured_at: storage
                .try_get("captured_at")
                .map_err(RepositoryError::Database)?,
            latest_migration_version: storage
                .try_get("migration_version")
                .map_err(RepositoryError::Database)?,
            database_bytes: non_negative_i64(database_bytes, "database size")?,
            index_bytes: non_negative_i64(index_bytes, "index size")?,
            wal_bytes,
            configured_max_connections: non_negative_i64(
                configured_max_connections,
                "maximum connections",
            )?,
            database_connections: non_negative_i64(total_connections, "database connections")?,
            active_connections: non_negative_i64(active_connections, "active connections")?,
            waiting_connections: non_negative_i64(waiting_connections, "waiting connections")?,
            long_transaction_count: non_negative_i64(long_transactions, "long transactions")?,
            oldest_transaction_seconds: activity
                .try_get("oldest_transaction_seconds")
                .map_err(RepositoryError::Database)?,
            row_lock_wait_count: non_negative_i64(lock_waits, "row lock waits")?,
            deadlock_count: non_negative_i64(deadlocks, "deadlocks")?,
            cumulative_block_read_milliseconds: database_stats
                .try_get("block_read_ms")
                .map_err(RepositoryError::Database)?,
            cumulative_block_write_milliseconds: database_stats
                .try_get("block_write_ms")
                .map_err(RepositoryError::Database)?,
            never_analyzed_table_count: non_negative_i64(never_analyzed, "unanalyzed tables")?,
            maximum_dead_tuple_count: non_negative_i64(max_dead_tuples, "dead tuples")?,
            replication_client_count,
            maximum_replication_lag_bytes,
            turn_event_outcomes,
            hero_event_outcomes,
            lifecycle_event_outcomes,
            generation_queue,
            generation_attempt_outcomes,
            generation_attempt_count: non_negative_row_i64(
                &generation_usage,
                "attempt_count",
                "generation attempt count",
            )?,
            generation_total_tokens: non_negative_row_i64(
                &generation_usage,
                "total_tokens",
                "generation token count",
            )?,
            generation_total_latency_milliseconds: non_negative_row_i64(
                &generation_usage,
                "total_latency_milliseconds",
                "generation latency",
            )?,
            generation_maximum_latency_milliseconds: generation_usage
                .try_get::<Option<i64>, _>("maximum_latency_milliseconds")
                .map_err(RepositoryError::Database)?
                .map(|value| non_negative_i64(value, "maximum generation latency"))
                .transpose()?,
            generation_total_cost_microusd: non_negative_row_i64(
                &generation_usage,
                "total_cost_microusd",
                "generation cost",
            )?,
            generation_budget_denials,
            oldest_queued_job_seconds: queue_health
                .try_get("oldest_queued_seconds")
                .map_err(RepositoryError::Database)?,
            expired_generation_lease_count: non_negative_i64(
                expired_leases,
                "expired generation leases",
            )?,
            last_backup_completed_at: recovery_fields.as_ref().and_then(|fields| fields.0.clone()),
            last_backup_vault_digest: recovery_fields.as_ref().and_then(|fields| fields.1.clone()),
            last_restore_test_completed_at: recovery_fields
                .as_ref()
                .and_then(|fields| fields.2.clone()),
            last_restore_test_result: recovery_fields.as_ref().and_then(|fields| fields.3.clone()),
            last_restore_source_digest: recovery_fields.and_then(|fields| fields.4),
        })
    }
}

impl CompleteRecoveryManifest {
    pub fn build(
        database: DatabaseRecoveryManifest,
        rng_master_key_path: &Path,
        image_artifact_root: &Path,
    ) -> Result<Self, RecoveryManifestError> {
        let rng_master_key = verify_file(
            rng_master_key_path,
            "rng_master_key",
            "rng-master.key",
            None,
            Some(32),
        )?;
        let mut selected_artifact_files =
            Vec::with_capacity(database.selected_artifact_files.len());
        for expected in &database.selected_artifact_files {
            validate_storage_key(&expected.storage_key)?;
            let path = image_artifact_root.join(&expected.storage_key);
            selected_artifact_files.push(verify_file(
                &path,
                "selected_scene_image",
                &expected.storage_key,
                Some(&expected.expected_digest),
                None,
            )?);
        }
        selected_artifact_files.sort_by(|left, right| left.storage_key.cmp(&right.storage_key));
        Ok(Self {
            schema_version: DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION,
            database,
            rng_master_key,
            selected_artifact_files,
        })
    }
}

fn verify_file(
    path: &Path,
    kind: &str,
    storage_key: &str,
    expected_digest: Option<&Sha256Digest>,
    expected_length: Option<u64>,
) -> Result<VerifiedRecoveryFile, RecoveryManifestError> {
    let metadata = std::fs::symlink_metadata(path).map_err(RecoveryManifestError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_RECOVERY_FILE_BYTES
        || expected_length.is_some_and(|length| length != metadata.len())
    {
        return Err(RecoveryManifestError::Invalid("protected file shape"));
    }
    #[cfg(unix)]
    if kind == "rng_master_key" {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RecoveryManifestError::Invalid("RNG master key permissions"));
        }
    }
    let mut file = File::open(path).map_err(RecoveryManifestError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut byte_count = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(RecoveryManifestError::Io)?;
        if read == 0 {
            break;
        }
        byte_count = byte_count
            .checked_add(read as u64)
            .ok_or(RecoveryManifestError::Invalid("protected file size"))?;
        if byte_count > MAX_RECOVERY_FILE_BYTES {
            return Err(RecoveryManifestError::Invalid("protected file size"));
        }
        hasher.update(&buffer[..read]);
    }
    if byte_count != metadata.len() {
        return Err(RecoveryManifestError::Invalid("protected file size"));
    }
    let digest = Sha256Digest::from_bytes(hasher.finalize().into());
    if expected_digest.is_some_and(|expected| expected != &digest) {
        return Err(RecoveryManifestError::Invalid("protected artifact digest"));
    }
    Ok(VerifiedRecoveryFile {
        kind: kind.to_owned(),
        storage_key: storage_key.to_owned(),
        byte_count,
        digest,
    })
}

fn validate_storage_key(value: &str) -> Result<(), RecoveryManifestError> {
    if value.is_empty()
        || value.len() > 512
        || Path::new(value).is_absolute()
        || Path::new(value)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || value.split('/').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(RecoveryManifestError::Invalid("artifact storage key"));
    }
    Ok(())
}

async fn optional_wal_bytes(pool: &sqlx::PgPool) -> Option<u64> {
    let value: Result<Option<i64>, sqlx::Error> =
        sqlx::query_scalar("SELECT SUM(size)::bigint FROM pg_ls_waldir()")
            .fetch_one(pool)
            .await;
    value.ok().flatten().and_then(|value| value.try_into().ok())
}

async fn optional_replication_health(pool: &sqlx::PgPool) -> (Option<u64>, Option<u64>) {
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS clients,
                MAX(pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn))::bigint AS lag_bytes
         FROM pg_stat_replication",
    )
    .fetch_one(pool)
    .await;
    let Ok(row) = row else {
        return (None, None);
    };
    let clients: Option<i64> = row.try_get("clients").ok();
    let lag: Option<i64> = row.try_get("lag_bytes").ok();
    (
        clients.and_then(|value| value.try_into().ok()),
        lag.and_then(|value| value.try_into().ok()),
    )
}

type RecoveryStatusFields = (
    Option<String>,
    Option<Sha256Digest>,
    Option<String>,
    Option<String>,
    Option<Sha256Digest>,
);

fn recovery_status_fields(
    row: &sqlx::postgres::PgRow,
) -> Result<RecoveryStatusFields, RepositoryError> {
    Ok((
        row.try_get("last_backup_completed_at")
            .map_err(RepositoryError::Database)?,
        optional_digest(row, "last_backup_vault_digest")?,
        row.try_get("last_restore_test_completed_at")
            .map_err(RepositoryError::Database)?,
        row.try_get("last_restore_test_result")
            .map_err(RepositoryError::Database)?,
        optional_digest(row, "last_restore_source_digest")?,
    ))
}

fn optional_digest(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> Result<Option<Sha256Digest>, RepositoryError> {
    let value: Option<String> = row.try_get(column).map_err(RepositoryError::Database)?;
    value
        .map(|value| {
            Sha256Digest::new(value).map_err(|_| RepositoryError::InvalidDomainState {
                entity: "operator recovery status",
                id: "singleton".to_owned(),
                reason: "stored digest is invalid",
            })
        })
        .transpose()
}

fn usize_to_u64(value: usize) -> Result<u64, RepositoryError> {
    value.try_into().map_err(|_| RepositoryError::NumericRange {
        field: "recovery manifest count",
    })
}

fn closed_outcome_counts(
    rows: Vec<sqlx::postgres::PgRow>,
    allowed: &[&str],
    entity: &'static str,
) -> Result<Vec<OperationalOutcomeCount>, RepositoryError> {
    rows.into_iter()
        .map(|row| {
            let outcome: Option<String> =
                row.try_get("outcome").map_err(RepositoryError::Database)?;
            let Some(outcome) = outcome else {
                return Err(RepositoryError::InvalidDomainState {
                    entity,
                    id: "aggregate".to_owned(),
                    reason: "stored operational label is missing",
                });
            };
            if !allowed.contains(&outcome.as_str()) {
                return Err(RepositoryError::InvalidDomainState {
                    entity,
                    id: "aggregate".to_owned(),
                    reason: "stored operational label is invalid",
                });
            }
            let count: i64 = row.try_get("count").map_err(RepositoryError::Database)?;
            Ok(OperationalOutcomeCount {
                outcome,
                count: non_negative_i64(count, "operational outcome count")?,
            })
        })
        .collect()
}

fn non_negative_row_i64(
    row: &sqlx::postgres::PgRow,
    column: &str,
    field: &'static str,
) -> Result<u64, RepositoryError> {
    let value: i64 = row.try_get(column).map_err(RepositoryError::Database)?;
    non_negative_i64(value, field)
}

fn non_negative_i64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    value
        .try_into()
        .map_err(|_| RepositoryError::NumericRange { field })
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operational_turn_labels_include_the_durable_social_event() {
        assert!(TURN_EVENT_OUTCOME_LABELS.contains(&"exploration_social_resolved"));
    }

    #[test]
    fn recovery_file_manifest_verifies_key_artifact_digest_and_path_boundary() {
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("rng-master.key");
        std::fs::write(&key, [0x2a; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let artifacts = root.path().join("artifacts");
        let stored = artifacts.join("artifacts/asset/web.png");
        std::fs::create_dir_all(stored.parent().unwrap()).unwrap();
        let image = b"bounded validated png fixture";
        std::fs::write(&stored, image).unwrap();
        let database = DatabaseRecoveryManifest {
            schema_version: DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION,
            migrations: Vec::new(),
            campaigns: Vec::new(),
            selected_artifact_files: vec![RecoveryArtifactFileEntry {
                campaign_session_id: "local-campaign".to_owned(),
                artifact_id: "asset".to_owned(),
                variant: "web".to_owned(),
                storage_key: "artifacts/asset/web.png".to_owned(),
                expected_digest: digest(image),
            }],
        };
        let complete = CompleteRecoveryManifest::build(database.clone(), &key, &artifacts).unwrap();
        assert_eq!(complete.rng_master_key.byte_count, 32);
        assert_eq!(complete.selected_artifact_files.len(), 1);
        assert_eq!(complete.selected_artifact_files[0].digest, digest(image));

        std::fs::write(&stored, b"tampered").unwrap();
        assert!(matches!(
            CompleteRecoveryManifest::build(database, &key, &artifacts),
            Err(RecoveryManifestError::Invalid("protected artifact digest"))
        ));

        let traversal = DatabaseRecoveryManifest {
            schema_version: DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION,
            migrations: Vec::new(),
            campaigns: Vec::new(),
            selected_artifact_files: vec![RecoveryArtifactFileEntry {
                campaign_session_id: "local-campaign".to_owned(),
                artifact_id: "asset".to_owned(),
                variant: "web".to_owned(),
                storage_key: "../outside.png".to_owned(),
                expected_digest: digest(image),
            }],
        };
        assert!(matches!(
            CompleteRecoveryManifest::build(traversal, &key, &artifacts),
            Err(RecoveryManifestError::Invalid("artifact storage key"))
        ));
    }
}
