//! MongoDB recovery manifests and bounded operational snapshots.
//!
//! Output excludes campaign prose, prompts, credentials, provider responses,
//! and private source bodies.

use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

use manchester_dnd_core::Sha256Digest;
use mongodb::bson::{Bson, DateTime, Document, doc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    CampaignLifecycleState, MongoRepository, date_string, mongo_error, validate_account_id,
};
use crate::{
    error::RepositoryError,
    persistence::{CollectionName, SCHEMA_BUNDLE_VERSION, schema_bundle_digest},
};

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
    "bde_change",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySchemaManifestEntry {
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
    pub schema_bundles: Vec<RecoverySchemaManifestEntry>,
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

impl MongoRepository {
    pub async fn database_recovery_manifest(
        &self,
        owner_key: &str,
    ) -> Result<DatabaseRecoveryManifest, RepositoryError> {
        validate_account_id(owner_key)?;
        let bundle_digest = schema_bundle_digest().map_err(super::map_persistence)?;
        let schema_bundles = vec![RecoverySchemaManifestEntry {
            version: SCHEMA_BUNDLE_VERSION,
            description: format!("mongodb_schema_bundle:{bundle_digest}"),
            success: true,
        }];

        let campaigns_collection = self.store().document_collection(CollectionName::Campaigns);
        let characters = self
            .store()
            .document_collection(CollectionName::CampaignCharacterInstances);
        let turns = self.store().document_collection(CollectionName::TurnEvents);
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let assets = self
            .store()
            .document_collection(CollectionName::GeneratedAssets);

        let mut cursor = campaigns_collection
            .find(doc! { "owner_account_id": owner_key })
            .sort(doc! { "_id": 1_i64 })
            .await
            .map_err(|error| mongo_error("list owned recovery campaigns", error))?;
        let mut campaigns = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read owned recovery campaigns", error))?
        {
            let campaign = cursor
                .deserialize_current()
                .map_err(|error| mongo_error("decode recovery campaign", error))?;
            let campaign_id = required_string(&campaign, "_id", "campaign id")?;
            let campaign_revision = required_u64(&campaign, "revision", "campaign revision")?;
            let lifecycle_state = match nested_string(&campaign, "lifecycle.state").as_deref() {
                Some("archived") => CampaignLifecycleState::Archived,
                Some("open" | "active") => CampaignLifecycleState::Active,
                _ => {
                    return invalid(
                        "database recovery manifest",
                        &campaign_id,
                        "campaign lifecycle state is invalid",
                    );
                }
            };
            let stable_bytes = mongodb::bson::to_vec(&campaign).map_err(|_| {
                RepositoryError::InvalidDomainState {
                    entity: "database recovery manifest",
                    id: campaign_id.clone(),
                    reason: "campaign document could not be encoded for recovery digest",
                }
            })?;
            let hero = characters
                .find_one(doc! {
                    "campaign_id": &campaign_id,
                    "runtime_kind": "hero_character",
                    "state": "active",
                })
                .await
                .map_err(|error| mongo_error("load recovery hero revision", error))?;
            let latest_turn = turns
                .find_one(doc! { "campaign_id": &campaign_id })
                .sort(doc! { "sequence": -1_i64 })
                .await
                .map_err(|error| mongo_error("load latest recovery turn", error))?;
            campaigns.push(RecoveryCampaignManifestEntry {
                campaign_session_id: campaign_id.clone(),
                campaign_revision,
                lifecycle_revision: campaign_revision,
                lifecycle_state,
                stable_state_digest: digest(&stable_bytes),
                character_count: characters
                    .count_documents(doc! { "campaign_id": &campaign_id })
                    .await
                    .map_err(|error| mongo_error("count recovery characters", error))?,
                hero_revision: hero
                    .as_ref()
                    .map(|document| required_u64(document, "revision", "hero revision"))
                    .transpose()?,
                turn_count: turns
                    .count_documents(doc! { "campaign_id": &campaign_id })
                    .await
                    .map_err(|error| mongo_error("count recovery turns", error))?,
                latest_turn_number: latest_turn
                    .as_ref()
                    .map(|document| required_u64(document, "sequence", "turn sequence"))
                    .transpose()?,
                command_receipt_count: receipts
                    .count_documents(doc! {
                        "$or": [
                            { "scope_kind": "campaign", "scope_id": &campaign_id },
                            { "campaign_id": &campaign_id },
                        ]
                    })
                    .await
                    .map_err(|error| mongo_error("count recovery receipts", error))?,
                hero_audit_count: audits
                    .count_documents(doc! {
                        "category": "hero",
                        "metadata.campaign_session_id": &campaign_id,
                    })
                    .await
                    .map_err(|error| mongo_error("count recovery hero audits", error))?,
                lifecycle_audit_count: audits
                    .count_documents(doc! {
                        "category": "campaign_lifecycle",
                        "scope_id": &campaign_id,
                    })
                    .await
                    .map_err(|error| mongo_error("count recovery lifecycle audits", error))?,
                private_recap_count: presentations
                    .count_documents(doc! {
                        "campaign_id": &campaign_id,
                        "presentation_type": "private_recap",
                    })
                    .await
                    .map_err(|error| mongo_error("count recovery recaps", error))?,
                selected_text_presentation_count: presentations
                    .count_documents(doc! {
                        "campaign_id": &campaign_id,
                        "selected": true,
                    })
                    .await
                    .map_err(|error| mongo_error("count selected recovery presentations", error))?,
                selected_generated_asset_count: assets
                    .count_documents(doc! {
                        "campaign_id": &campaign_id,
                        "state": "selected",
                    })
                    .await
                    .map_err(|error| mongo_error("count selected recovery assets", error))?,
                has_validated_content_pins: nested_value(&campaign, "rules_snapshot.campaign_pins")
                    .is_some(),
            });
        }

        let mut asset_cursor = assets
            .find(doc! {
                "owner_account_id": owner_key,
                "state": "selected",
                "campaign_id": { "$type": "string" },
            })
            .sort(doc! { "campaign_id": 1_i64, "_id": 1_i64 })
            .await
            .map_err(|error| mongo_error("list recovery assets", error))?;
        let mut selected_artifact_files = Vec::new();
        while asset_cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read recovery assets", error))?
        {
            let asset = asset_cursor
                .deserialize_current()
                .map_err(|error| mongo_error("decode recovery asset", error))?;
            let artifact_id = required_string(&asset, "_id", "artifact id")?;
            selected_artifact_files.push(RecoveryArtifactFileEntry {
                campaign_session_id: required_string(&asset, "campaign_id", "artifact campaign")?,
                artifact_id: artifact_id.clone(),
                variant: "original".to_owned(),
                storage_key: required_string(&asset, "object_key", "artifact object key")?,
                expected_digest: required_digest(&asset, "digest", &artifact_id)?,
            });
        }

        Ok(DatabaseRecoveryManifest {
            schema_version: DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION,
            schema_bundles,
            campaigns,
            selected_artifact_files,
        })
    }

    pub async fn database_operations_snapshot(
        &self,
    ) -> Result<DatabaseOperationsSnapshot, RepositoryError> {
        let captured_at = DateTime::now();
        let db_stats = self
            .store()
            .database()
            .run_command(doc! { "dbStats": 1, "scale": 1 })
            .await
            .map_err(|error| mongo_error("load MongoDB database statistics", error))?;
        let turn_event_outcomes = aggregate_outcomes(
            self.store().document_collection(CollectionName::TurnEvents),
            doc! {},
            "$event.payload.type",
            TURN_EVENT_OUTCOME_LABELS,
            "turn event outcome",
        )
        .await?;
        let hero_event_outcomes = aggregate_outcomes(
            self.store()
                .document_collection(CollectionName::AuditEvents),
            doc! { "category": "hero" },
            "$action",
            &["creation_transition", "reward_awarded", "level_up"],
            "hero event outcome",
        )
        .await?;
        let lifecycle_event_outcomes = aggregate_outcomes(
            self.store()
                .document_collection(CollectionName::AuditEvents),
            doc! { "category": "campaign_lifecycle" },
            "$action",
            &[
                "play_started",
                "play_ended",
                "archived",
                "restored",
                "restore_imported",
            ],
            "lifecycle event outcome",
        )
        .await?;
        let generation_queue_outcomes = aggregate_outcomes(
            self.store()
                .document_collection(CollectionName::GenerationJobs),
            doc! {},
            "$state",
            &["queued", "running", "succeeded", "failed", "cancelled"],
            "generation queue state",
        )
        .await?;
        let generation_queue = generation_queue_outcomes
            .into_iter()
            .map(|entry| GenerationQueueStateCount {
                state: entry.outcome,
                count: entry.count,
            })
            .collect();
        let generation_attempt_outcomes = aggregate_attempt_outcomes(self).await?;
        let usage = aggregate_generation_usage(self).await?;
        let queue_health = generation_queue_health(self).await?;
        let recovery = recovery_status(self).await?;

        Ok(DatabaseOperationsSnapshot {
            schema_version: DATABASE_OPERATIONS_SNAPSHOT_SCHEMA_VERSION,
            captured_at: date_string(captured_at),
            latest_migration_version: Some(SCHEMA_BUNDLE_VERSION),
            database_bytes: numeric_u64(&db_stats, "dataSize", "database size")?,
            index_bytes: numeric_u64(&db_stats, "indexSize", "index size")?,
            wal_bytes: None,
            configured_max_connections: 0,
            database_connections: 0,
            active_connections: 0,
            waiting_connections: 0,
            long_transaction_count: 0,
            oldest_transaction_seconds: None,
            row_lock_wait_count: 0,
            deadlock_count: 0,
            cumulative_block_read_milliseconds: 0.0,
            cumulative_block_write_milliseconds: 0.0,
            never_analyzed_table_count: 0,
            maximum_dead_tuple_count: 0,
            replication_client_count: None,
            maximum_replication_lag_bytes: None,
            turn_event_outcomes,
            hero_event_outcomes,
            lifecycle_event_outcomes,
            generation_queue,
            generation_attempt_outcomes,
            generation_attempt_count: usage.attempt_count,
            generation_total_tokens: usage.total_tokens,
            generation_total_latency_milliseconds: usage.total_latency_milliseconds,
            generation_maximum_latency_milliseconds: usage.maximum_latency_milliseconds,
            generation_total_cost_microusd: usage.total_cost_microusd,
            generation_budget_denials: Vec::new(),
            oldest_queued_job_seconds: queue_health.oldest_queued_job_seconds,
            expired_generation_lease_count: queue_health.expired_lease_count,
            last_backup_completed_at: recovery.last_backup_completed_at,
            last_backup_vault_digest: recovery.last_backup_vault_digest,
            last_restore_test_completed_at: recovery.last_restore_test_completed_at,
            last_restore_test_result: recovery.last_restore_test_result,
            last_restore_source_digest: recovery.last_restore_source_digest,
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

#[derive(Default)]
struct GenerationUsage {
    attempt_count: u64,
    total_tokens: u64,
    total_latency_milliseconds: u64,
    maximum_latency_milliseconds: Option<u64>,
    total_cost_microusd: u64,
}

#[derive(Default)]
struct GenerationQueueHealth {
    oldest_queued_job_seconds: Option<f64>,
    expired_lease_count: u64,
}

#[derive(Default)]
struct RecoveryStatus {
    last_backup_completed_at: Option<String>,
    last_backup_vault_digest: Option<Sha256Digest>,
    last_restore_test_completed_at: Option<String>,
    last_restore_test_result: Option<String>,
    last_restore_source_digest: Option<Sha256Digest>,
}

async fn aggregate_outcomes(
    collection: mongodb::Collection<Document>,
    filter: Document,
    field_expression: &str,
    allowed: &[&str],
    entity: &'static str,
) -> Result<Vec<OperationalOutcomeCount>, RepositoryError> {
    let mut cursor = collection
        .aggregate(vec![
            doc! { "$match": filter },
            doc! { "$group": { "_id": field_expression, "count": { "$sum": 1_i64 } } },
            doc! { "$sort": { "_id": 1_i64 } },
        ])
        .await
        .map_err(|error| mongo_error("aggregate operational outcomes", error))?;
    let mut output = Vec::new();
    while cursor
        .advance()
        .await
        .map_err(|error| mongo_error("read operational outcomes", error))?
    {
        let row = cursor
            .deserialize_current()
            .map_err(|error| mongo_error("decode operational outcomes", error))?;
        let outcome = required_string(&row, "_id", entity)?;
        if !allowed.contains(&outcome.as_str()) {
            return invalid(entity, "aggregate", "stored operational label is invalid");
        }
        output.push(OperationalOutcomeCount {
            outcome,
            count: required_u64(&row, "count", "operational outcome count")?,
        });
    }
    Ok(output)
}

async fn aggregate_attempt_outcomes(
    repository: &MongoRepository,
) -> Result<Vec<OperationalOutcomeCount>, RepositoryError> {
    let jobs = repository
        .store()
        .document_collection(CollectionName::GenerationJobs);
    let mut cursor = jobs
        .aggregate(vec![
            doc! { "$unwind": "$attempts" },
            doc! {
                "$group": {
                    "_id": { "$ifNull": ["$attempts.failure_code", "$attempts.state"] },
                    "count": { "$sum": 1_i64 },
                }
            },
            doc! { "$sort": { "_id": 1_i64 } },
        ])
        .await
        .map_err(|error| mongo_error("aggregate generation attempts", error))?;
    let mut output = Vec::new();
    while cursor
        .advance()
        .await
        .map_err(|error| mongo_error("read generation attempt outcomes", error))?
    {
        let row = cursor
            .deserialize_current()
            .map_err(|error| mongo_error("decode generation attempt outcomes", error))?;
        output.push(OperationalOutcomeCount {
            outcome: required_string(&row, "_id", "generation attempt outcome")?,
            count: required_u64(&row, "count", "generation attempt count")?,
        });
    }
    Ok(output)
}

async fn aggregate_generation_usage(
    repository: &MongoRepository,
) -> Result<GenerationUsage, RepositoryError> {
    let jobs = repository
        .store()
        .document_collection(CollectionName::GenerationJobs);
    let mut cursor = jobs
        .aggregate(vec![
            doc! { "$unwind": "$attempts" },
            doc! {
                "$group": {
                    "_id": Bson::Null,
                    "attempt_count": { "$sum": 1_i64 },
                    "total_tokens": { "$sum": { "$ifNull": ["$attempts.total_tokens", 0_i64] } },
                    "total_latency_milliseconds": {
                        "$sum": { "$ifNull": ["$attempts.latency_milliseconds", 0_i64] }
                    },
                    "maximum_latency_milliseconds": { "$max": "$attempts.latency_milliseconds" },
                    "total_cost_microusd": {
                        "$sum": { "$ifNull": ["$attempts.cost_microusd", 0_i64] }
                    },
                }
            },
        ])
        .await
        .map_err(|error| mongo_error("aggregate generation usage", error))?;
    if !cursor
        .advance()
        .await
        .map_err(|error| mongo_error("read generation usage", error))?
    {
        return Ok(GenerationUsage::default());
    }
    let row = cursor
        .deserialize_current()
        .map_err(|error| mongo_error("decode generation usage", error))?;
    Ok(GenerationUsage {
        attempt_count: required_u64(&row, "attempt_count", "generation attempt count")?,
        total_tokens: required_u64(&row, "total_tokens", "generation token count")?,
        total_latency_milliseconds: required_u64(
            &row,
            "total_latency_milliseconds",
            "generation latency",
        )?,
        maximum_latency_milliseconds: optional_u64(
            &row,
            "maximum_latency_milliseconds",
            "maximum generation latency",
        )?,
        total_cost_microusd: required_u64(&row, "total_cost_microusd", "generation cost")?,
    })
}

async fn generation_queue_health(
    repository: &MongoRepository,
) -> Result<GenerationQueueHealth, RepositoryError> {
    let jobs = repository
        .store()
        .document_collection(CollectionName::GenerationJobs);
    let now = DateTime::now();
    let expired_lease_count = jobs
        .count_documents(doc! {
            "state": "running",
            "lease_expires_at": { "$lte": now },
        })
        .await
        .map_err(|error| mongo_error("count expired generation leases", error))?;
    let oldest = jobs
        .find_one(doc! { "state": "queued" })
        .sort(doc! { "created_at": 1_i64 })
        .await
        .map_err(|error| mongo_error("load oldest generation job", error))?;
    let oldest_queued_job_seconds = oldest
        .as_ref()
        .and_then(|document| document.get_datetime("created_at").ok())
        .map(|created| {
            let elapsed = now
                .timestamp_millis()
                .saturating_sub(created.timestamp_millis());
            elapsed as f64 / 1_000.0
        });
    Ok(GenerationQueueHealth {
        oldest_queued_job_seconds,
        expired_lease_count,
    })
}

async fn recovery_status(repository: &MongoRepository) -> Result<RecoveryStatus, RepositoryError> {
    let settings = repository
        .store()
        .document_collection(CollectionName::SystemSettings)
        .find_one(doc! { "_id": "system:settings" })
        .await
        .map_err(|error| mongo_error("load recovery status", error))?;
    let Some(settings) = settings else {
        return Ok(RecoveryStatus::default());
    };
    let Some(recovery) = settings.get_document("recovery").ok() else {
        return Ok(RecoveryStatus::default());
    };
    Ok(RecoveryStatus {
        last_backup_completed_at: optional_date_string(recovery, "last_backup_completed_at")?,
        last_backup_vault_digest: optional_digest(recovery, "last_backup_vault_digest")?,
        last_restore_test_completed_at: optional_date_string(
            recovery,
            "last_restore_test_completed_at",
        )?,
        last_restore_test_result: optional_string(recovery, "last_restore_test_result")?,
        last_restore_source_digest: optional_digest(recovery, "last_restore_source_digest")?,
    })
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

fn required_string(
    document: &Document,
    field: &str,
    entity: &'static str,
) -> Result<String, RepositoryError> {
    document
        .get_str(field)
        .map(str::to_owned)
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity,
            id: field.to_owned(),
            reason: "stored string field is missing or invalid",
        })
}

fn optional_string(document: &Document, field: &str) -> Result<Option<String>, RepositoryError> {
    match document.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value.clone())),
        Some(_) => invalid(
            "operator recovery status",
            field,
            "stored recovery string is invalid",
        ),
    }
}

fn optional_date_string(
    document: &Document,
    field: &str,
) -> Result<Option<String>, RepositoryError> {
    match document.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::DateTime(value)) => Ok(Some(date_string(*value))),
        Some(_) => invalid(
            "operator recovery status",
            field,
            "stored recovery date is invalid",
        ),
    }
}

fn required_u64(
    document: &Document,
    field: &str,
    name: &'static str,
) -> Result<u64, RepositoryError> {
    optional_u64(document, field, name)?.ok_or_else(|| RepositoryError::InvalidDomainState {
        entity: name,
        id: field.to_owned(),
        reason: "stored numeric field is missing",
    })
}

fn optional_u64(
    document: &Document,
    field: &str,
    name: &'static str,
) -> Result<Option<u64>, RepositoryError> {
    let value = match document.get(field) {
        None | Some(Bson::Null) => return Ok(None),
        Some(Bson::Int32(value)) => i64::from(*value),
        Some(Bson::Int64(value)) => *value,
        Some(Bson::Double(value)) if value.is_finite() && value.fract() == 0.0 => *value as i64,
        Some(_) => {
            return invalid(name, field, "stored numeric field has an invalid BSON type");
        }
    };
    u64::try_from(value)
        .map(Some)
        .map_err(|_| RepositoryError::NumericRange { field: name })
}

fn numeric_u64(
    document: &Document,
    field: &str,
    name: &'static str,
) -> Result<u64, RepositoryError> {
    required_u64(document, field, name)
}

fn nested_value<'a>(document: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut current = document;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        let value = current.get(part)?;
        if parts.peek().is_none() {
            return Some(value);
        }
        current = value.as_document()?;
    }
    None
}

fn nested_string(document: &Document, path: &str) -> Option<String> {
    nested_value(document, path)
        .and_then(Bson::as_str)
        .map(str::to_owned)
}

fn required_digest(
    document: &Document,
    field: &str,
    id: &str,
) -> Result<Sha256Digest, RepositoryError> {
    let value = required_string(document, field, "recovery artifact digest")?;
    Sha256Digest::new(value).map_err(|_| RepositoryError::InvalidDomainState {
        entity: "recovery artifact digest",
        id: id.to_owned(),
        reason: "stored digest is invalid",
    })
}

fn optional_digest(
    document: &Document,
    field: &str,
) -> Result<Option<Sha256Digest>, RepositoryError> {
    optional_string(document, field)?
        .map(|value| {
            Sha256Digest::new(value).map_err(|_| RepositoryError::InvalidDomainState {
                entity: "operator recovery status",
                id: field.to_owned(),
                reason: "stored digest is invalid",
            })
        })
        .transpose()
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

fn invalid<T>(entity: &'static str, id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operational_turn_labels_include_social_and_bde_events() {
        assert!(TURN_EVENT_OUTCOME_LABELS.contains(&"exploration_social_resolved"));
        assert!(TURN_EVENT_OUTCOME_LABELS.contains(&"bde_change"));
    }

    #[test]
    fn recovery_storage_key_rejects_traversal() {
        assert!(validate_storage_key("artifacts/asset/web.png").is_ok());
        assert!(validate_storage_key("../outside.png").is_err());
    }
}
