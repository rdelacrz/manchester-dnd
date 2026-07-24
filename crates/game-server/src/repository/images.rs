//! Protected scene-image publication metadata.
//!
//! MongoDB stores only bounded metadata, digests, and relative protected-storage
//! keys. The image service validates and writes bytes before calling this layer.

use std::{future::IntoFuture, time::Duration};

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use mongodb::{
    ClientSession, Collection,
    bson::{DateTime, doc},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    error::PersistenceError,
    persistence::{CollectionName, MongoStore},
};

use super::{
    MongoRepository,
    jobs::{GenerationJobDocument, GenerationJobState, GenerationJobStoreError, GenerationPurpose},
};

const SCENE_IMAGE_SCHEMA_VERSION: u32 = 1;
const IMAGE_MEDIA_TYPE: &str = "image/png";
const SAFE_MODERATION_RESULT: &str = "provider_and_application_safe";
const SUPERSEDED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const QUARANTINE_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_QUARANTINE_BYTES: u64 = 32 * 1024 * 1024;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AssetVariantDocument {
    object_key: String,
    digest: String,
    media_type: String,
    width: i64,
    height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedAssetDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    owner_account_id: String,
    campaign_id: String,
    entity_kind: String,
    entity_id: String,
    job_id: String,
    generation_attempt_id: String,
    source_turn_id: String,
    object_key: String,
    digest: String,
    state: String,
    provider: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_request_id: Option<String>,
    brief_fingerprint: String,
    prompt_policy_fingerprint: String,
    config_fingerprint: String,
    original: AssetVariantDocument,
    web: AssetVariantDocument,
    thumbnail: AssetVariantDocument,
    alt_text: String,
    moderation_result: String,
    selected: bool,
    estimated_cost_microusd: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actual_cost_microusd: Option<i64>,
    license_id: String,
    provenance_summary: String,
    created_at: DateTime,
    updated_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    purge_at: Option<DateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarantinedAssetDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    job_id: String,
    attempt_id: String,
    campaign_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    byte_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    object_key: Option<String>,
    reason_code: String,
    created_at: DateTime,
    purge_at: DateTime,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct CampaignOwnerReference {
    #[serde(rename = "_id")]
    id: String,
    owner_account_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct TurnReference {
    #[serde(rename = "_id")]
    id: String,
    campaign_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdReference {
    #[serde(rename = "_id")]
    id: String,
}

impl MongoRepository {
    pub async fn scene_image_request_counts(
        &self,
        campaign_session_id: &str,
        source_turn_id: Option<&str>,
    ) -> Result<SceneImageRequestCounts, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        if let Some(turn_id) = source_turn_id {
            validate_id(turn_id, "source turn id is invalid")?;
        }
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let base = doc! {
            "scope_kind": "generation_illustration",
            "scope_id": campaign_session_id,
            "command_kind": "enqueue_generation_job",
            "state": "committed",
        };
        let campaign_lifetime = operation(
            self.store(),
            "count campaign illustration requests",
            receipts.count_documents(base.clone()),
        )
        .await?;
        let mut rolling = base.clone();
        rolling.insert(
            "created_at",
            doc! {
                "$gt": DateTime::from_millis(
                    DateTime::now()
                        .timestamp_millis()
                        .saturating_sub(24 * 60 * 60 * 1_000),
                ),
            },
        );
        let rolling_day = operation(
            self.store(),
            "count rolling illustration requests",
            receipts.count_documents(rolling),
        )
        .await?;
        let source_turn = if let Some(turn_id) = source_turn_id {
            let mut turn = base.clone();
            turn.insert("origin_event_id", turn_id);
            operation(
                self.store(),
                "count source-turn illustration requests",
                receipts.count_documents(turn),
            )
            .await?
        } else {
            0
        };
        let active = doc! {
            "campaign_id": campaign_session_id,
            "purpose": GenerationPurpose::Illustration.as_str(),
            "dimension": "requests",
            "state": "reserved",
        };
        let active = operation(
            self.store(),
            "count active illustration requests",
            self.store()
                .document_collection(CollectionName::GenerationBudgetReservations)
                .count_documents(active),
        )
        .await?;
        Ok(SceneImageRequestCounts {
            rolling_day,
            campaign_lifetime,
            source_turn,
            active,
        })
    }

    pub async fn latest_scene_image_job_id(
        &self,
        campaign_session_id: &str,
        source_turn_id: &str,
    ) -> Result<Option<String>, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        validate_id(source_turn_id, "source turn id is invalid")?;
        Ok(operation(
            self.store(),
            "load latest scene image job",
            self.store()
                .collection::<IdReference>(CollectionName::GenerationJobs)
                .find_one(doc! {
                    "campaign_id": campaign_session_id,
                    "origin_event_id": source_turn_id,
                    "purpose": GenerationPurpose::Illustration.as_str(),
                })
                .sort(doc! { "created_at": -1, "_id": -1 })
                .projection(doc! { "_id": 1 }),
        )
        .await?
        .map(|document| document.id))
    }

    /// Publishes the one protected artifact slot owned by a running job. The
    /// job remains authoritative; delivery also requires its succeeded state.
    pub async fn upsert_scene_image_artifact(
        &self,
        artifact: &NewSceneImageArtifact,
    ) -> Result<(), GenerationJobStoreError> {
        validate_new_artifact(artifact)?;
        let requested = artifact.clone();
        let store = self.store().clone();
        let transaction_store = store.clone();
        transaction_store
            .with_transaction(move |session| {
                let requested = requested.clone();
                let store = store.clone();
                Box::pin(async move {
                    let jobs = generation_jobs(&store);
                    let assets = generated_assets(&store);
                    let Some(job) = jobs
                        .find_one(doc! {
                            "_id": &requested.job_id,
                            "campaign_id": &requested.campaign_session_id,
                            "origin_event_id": &requested.source_turn_id,
                            "purpose": GenerationPurpose::Illustration.as_str(),
                            "state": GenerationJobState::Running.as_str(),
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load running illustration job", error)
                        })?
                    else {
                        return Ok(Err(GenerationJobStoreError::LostLease));
                    };
                    if job
                        .pending_artifact_id
                        .as_deref()
                        .is_some_and(|id| id != requested.artifact_id)
                    {
                        return Ok(Err(GenerationJobStoreError::IdempotencyConflict));
                    }
                    let Some(running_attempt) = job.attempts.last() else {
                        return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                            "running illustration job has no attempt",
                        )));
                    };
                    if running_attempt.state != "running" {
                        return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                            "illustration artifact has no running attempt",
                        )));
                    }
                    if running_attempt.provider != requested.provider
                        || running_attempt.model != requested.model
                        || job.input_digest != requested.brief_fingerprint.as_str()
                        || job.policy_digest != requested.prompt_policy_fingerprint.as_str()
                        || job.config_digest != requested.config_fingerprint.as_str()
                    {
                        return Ok(Err(GenerationJobStoreError::InvalidInput(
                            "illustration artifact provenance does not match the leased job",
                        )));
                    }
                    let campaign =
                        match load_campaign_owner(&store, session, &requested.campaign_session_id)
                            .await?
                        {
                            Some(value) => value,
                            None => {
                                return Ok(Err(GenerationJobStoreError::InvalidInput(
                                    "campaign was not found",
                                )));
                            }
                        };
                    if !turn_belongs_to_campaign(
                        &store,
                        session,
                        &requested.source_turn_id,
                        &requested.campaign_session_id,
                    )
                    .await?
                    {
                        return Ok(Err(GenerationJobStoreError::InvalidInput(
                            "source turn was not found in the campaign",
                        )));
                    }
                    let now = DateTime::now();
                    let desired = match GeneratedAssetDocument::new(
                        &requested,
                        &campaign.owner_account_id,
                        &running_attempt.id,
                        now,
                    ) {
                        Ok(value) => value,
                        Err(error) => return Ok(Err(error)),
                    };
                    if let Some(existing) = assets
                        .find_one(doc! { "_id": &requested.artifact_id })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load scene image artifact replay", error)
                        })?
                    {
                        if existing.same_payload(&desired) {
                            return Ok(Ok(()));
                        }
                        if existing.job_id != desired.job_id
                            || existing.campaign_id != desired.campaign_id
                            || existing.source_turn_id != desired.source_turn_id
                            || existing.state != "staged"
                        {
                            return Ok(Err(GenerationJobStoreError::IdempotencyConflict));
                        }
                        let replaced = assets
                            .replace_one(
                                doc! {
                                    "_id": &existing.id,
                                    "job_id": &existing.job_id,
                                    "state": "staged",
                                },
                                desired,
                            )
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo(
                                    "replace retried scene image artifact",
                                    error,
                                )
                            })?;
                        if replaced.matched_count != 1 {
                            return Ok(Err(GenerationJobStoreError::LostLease));
                        }
                        return Ok(Ok(()));
                    }
                    let claimed = jobs
                        .update_one(
                            doc! {
                                "_id": &requested.job_id,
                                "state": GenerationJobState::Running.as_str(),
                                "$or": [
                                    { "pending_artifact_id": { "$exists": false } },
                                    { "pending_artifact_id": &requested.artifact_id },
                                ],
                            },
                            doc! {
                                "$set": {
                                    "pending_artifact_id": &requested.artifact_id,
                                    "updated_at": now,
                                },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("reserve illustration artifact slot", error)
                        })?;
                    if claimed.matched_count != 1 {
                        return Ok(Err(GenerationJobStoreError::LostLease));
                    }
                    assets
                        .insert_one(desired)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("insert scene image artifact", error)
                        })?;
                    Ok(Ok(()))
                })
            })
            .await
            .map_err(map_database)?
    }

    /// Selects a completed artifact and gives prior versions thirty-day
    /// retention. Touching the authoritative turn serializes concurrent picks.
    pub async fn select_scene_image_artifact(
        &self,
        campaign_session_id: &str,
        source_turn_id: &str,
        artifact_id: &str,
    ) -> Result<(), GenerationJobStoreError> {
        for value in [campaign_session_id, source_turn_id, artifact_id] {
            validate_id(value, "scene image identifier is invalid")?;
        }
        let store = self.store().clone();
        let transaction_store = store.clone();
        let campaign_id = campaign_session_id.to_owned();
        let turn_id = source_turn_id.to_owned();
        let artifact_id = artifact_id.to_owned();
        transaction_store
            .with_transaction(move |session| {
                let store = store.clone();
                let campaign_id = campaign_id.clone();
                let turn_id = turn_id.clone();
                let artifact_id = artifact_id.clone();
                Box::pin(async move {
                    let assets = generated_assets(&store);
                    let jobs = generation_jobs(&store);
                    let Some(asset) = assets
                        .find_one(doc! {
                            "_id": &artifact_id,
                            "campaign_id": &campaign_id,
                            "source_turn_id": &turn_id,
                            "entity_kind": "turn_scene_image",
                            "moderation_result": SAFE_MODERATION_RESULT,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load selectable scene image", error)
                        })?
                    else {
                        return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                            "completed scene image artifact was not found",
                        )));
                    };
                    let eligible = jobs
                        .find_one(doc! {
                            "_id": &asset.job_id,
                            "campaign_id": &campaign_id,
                            "state": GenerationJobState::Succeeded.as_str(),
                            "artifact_id": &artifact_id,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("verify completed illustration job", error)
                        })?
                        .is_some();
                    if !eligible {
                        return Ok(Err(GenerationJobStoreError::InvalidTransition {
                            job_id: asset.job_id,
                            state: GenerationJobState::Running,
                        }));
                    }
                    let touched = store
                        .document_collection(CollectionName::TurnEvents)
                        .update_one(
                            doc! { "_id": &turn_id, "campaign_id": &campaign_id },
                            doc! {
                                "$set": {
                                    "selected_scene_asset_id": &artifact_id,
                                    "updated_at": DateTime::now(),
                                },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("serialize scene image selection", error)
                        })?;
                    if touched.matched_count != 1 {
                        return Ok(Err(GenerationJobStoreError::InvalidInput(
                            "source turn was not found in the campaign",
                        )));
                    }
                    let now = DateTime::now();
                    let purge_at = add_duration(now, SUPERSEDED_RETENTION);
                    let mut previous = assets
                        .find(doc! {
                            "campaign_id": &campaign_id,
                            "source_turn_id": &turn_id,
                            "entity_kind": "turn_scene_image",
                            "selected": true,
                            "_id": { "$ne": &artifact_id },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("list superseded scene images", error)
                        })?;
                    let mut prior_job_ids = Vec::new();
                    while previous.advance(&mut *session).await.map_err(|error| {
                        PersistenceError::mongo("read superseded scene image", error)
                    })? {
                        prior_job_ids.push(
                            previous
                                .deserialize_current()
                                .map_err(|error| {
                                    PersistenceError::mongo("decode superseded scene image", error)
                                })?
                                .job_id,
                        );
                    }
                    drop(previous);
                    assets
                        .update_many(
                            doc! {
                                "campaign_id": &campaign_id,
                                "source_turn_id": &turn_id,
                                "entity_kind": "turn_scene_image",
                                "_id": { "$ne": &artifact_id },
                                "selected": true,
                            },
                            doc! {
                                "$set": {
                                    "selected": false,
                                    "state": "superseded",
                                    "purge_at": purge_at,
                                    "updated_at": now,
                                },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("supersede prior scene images", error)
                        })?;
                    if !prior_job_ids.is_empty() {
                        jobs.update_many(
                            doc! {
                                "_id": { "$in": prior_job_ids },
                                "state": GenerationJobState::Succeeded.as_str(),
                            },
                            doc! {
                                "$set": {
                                    "success_retention": "unselected_presentation_30d",
                                    "retention_class": "unselected_presentation_30d",
                                    "purge_at": purge_at,
                                    "updated_at": now,
                                },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("retain superseded illustration jobs", error)
                        })?;
                    }
                    let selected = assets
                        .update_one(
                            doc! {
                                "_id": &artifact_id,
                                "campaign_id": &campaign_id,
                                "source_turn_id": &turn_id,
                                "entity_kind": "turn_scene_image",
                            },
                            doc! {
                                "$set": {
                                    "selected": true,
                                    "state": "published",
                                    "updated_at": now,
                                },
                                "$unset": { "purge_at": "" },
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("select scene image", error))?;
                    if selected.matched_count != 1 {
                        return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                            "completed scene image artifact was not found",
                        )));
                    }
                    Ok(Ok(()))
                })
            })
            .await
            .map_err(map_database)?
    }

    pub async fn scene_image_artifact_for_job(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<Option<SceneImageArtifact>, GenerationJobStoreError> {
        validate_id(campaign_session_id, "campaign id is invalid")?;
        validate_id(job_id, "job id is invalid")?;
        let Some(job) = operation(
            self.store(),
            "load completed illustration job",
            generation_jobs(self.store()).find_one(doc! {
                "_id": job_id,
                "campaign_id": campaign_session_id,
                "state": GenerationJobState::Succeeded.as_str(),
            }),
        )
        .await?
        else {
            return Ok(None);
        };
        let Some(artifact_id) = job.artifact_id else {
            return Ok(None);
        };
        operation(
            self.store(),
            "load completed scene image artifact",
            generated_assets(self.store()).find_one(doc! {
                "_id": &artifact_id,
                "job_id": job_id,
                "campaign_id": campaign_session_id,
                "entity_kind": "turn_scene_image",
            }),
        )
        .await?
        .map(|document| document.to_public())
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
        let campaign_id = campaign_session_id.to_owned();
        let artifact_id = artifact_id.to_owned();
        let store = self.store().clone();
        let transaction_store = store.clone();
        transaction_store
            .with_transaction(move |session| {
                let campaign_id = campaign_id.clone();
                let artifact_id = artifact_id.clone();
                let store = store.clone();
                Box::pin(async move {
                    let campaign = campaign_owners(&store)
                        .find_one(doc! { "_id": &campaign_id })
                        .projection(doc! { "_id": 1, "owner_account_id": 1 })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load scene image campaign owner", error)
                        })?;
                    let Some(campaign) = campaign else {
                        return Ok(Ok(None));
                    };
                    let asset = generated_assets(&store)
                        .find_one(doc! {
                            "_id": &artifact_id,
                            "campaign_id": &campaign_id,
                            "entity_kind": "turn_scene_image",
                            "owner_account_id": &campaign.owner_account_id,
                            "moderation_result": SAFE_MODERATION_RESULT,
                            "state": { "$in": ["staged", "published"] },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load authorized scene image", error)
                        })?;
                    let Some(asset) = asset else {
                        return Ok(Ok(None));
                    };
                    let eligible = generation_jobs(&store)
                        .find_one(doc! {
                            "_id": &asset.job_id,
                            "campaign_id": &campaign_id,
                            "state": GenerationJobState::Succeeded.as_str(),
                            "artifact_id": &artifact_id,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("verify authorized illustration job", error)
                        })?
                        .is_some();
                    if !eligible {
                        return Ok(Ok(None));
                    }
                    let selected = match variant {
                        SceneImageVariant::Web => &asset.web,
                        SceneImageVariant::Thumbnail => &asset.thumbnail,
                    };
                    let authorized = (|| {
                        Ok(AuthorizedSceneImageVariant {
                            storage_key: validated_stored_key(&selected.object_key)?,
                            digest: stored_digest(&selected.digest)?,
                            media_type: validated_media_type(&selected.media_type)?,
                            alt_text: validated_stored_text(
                                &asset.alt_text,
                                500,
                                "invalid image alt text",
                            )?,
                        })
                    })();
                    Ok(authorized.map(Some))
                })
            })
            .await
            .map_err(map_database)?
    }

    pub async fn record_scene_image_quarantine(
        &self,
        quarantine: &NewSceneImageQuarantine,
    ) -> Result<(), GenerationJobStoreError> {
        validate_new_quarantine(quarantine)?;
        let requested = quarantine.clone();
        let store = self.store().clone();
        let transaction_store = store.clone();
        transaction_store
            .with_transaction(move |session| {
                let requested = requested.clone();
                let store = store.clone();
                Box::pin(async move {
                    let jobs = generation_jobs(&store);
                    let quarantines = quarantined_assets(&store);
                    let owns_attempt = jobs
                        .find_one(doc! {
                            "_id": &requested.job_id,
                            "campaign_id": &requested.campaign_session_id,
                            "purpose": GenerationPurpose::Illustration.as_str(),
                            "attempts": {
                                "$elemMatch": { "_id": &requested.attempt_id },
                            },
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("verify quarantined image attempt", error)
                        })?
                        .is_some();
                    if !owns_attempt {
                        return Ok(Err(GenerationJobStoreError::InvalidInput(
                            "quarantine does not belong to the illustration attempt",
                        )));
                    }
                    let document = QuarantinedAssetDocument {
                        id: requested.id.clone(),
                        schema_version: SCENE_IMAGE_SCHEMA_VERSION,
                        job_id: requested.job_id,
                        attempt_id: requested.attempt_id,
                        campaign_id: requested.campaign_session_id,
                        digest: requested.byte_digest.map(|value| value.as_str().to_owned()),
                        byte_length: match requested.byte_length {
                            Some(value) => match i64::try_from(value) {
                                Ok(value) => Some(value),
                                Err(_) => {
                                    return Ok(Err(GenerationJobStoreError::NumericRange));
                                }
                            },
                            None => None,
                        },
                        object_key: requested.storage_key,
                        reason_code: requested.reason_code.to_owned(),
                        created_at: DateTime::now(),
                        purge_at: add_duration(DateTime::now(), QUARANTINE_RETENTION),
                    };
                    if let Some(existing) = quarantines
                        .find_one(doc! { "_id": &document.id })
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load quarantine replay", error))?
                    {
                        if quarantine_same_payload(&existing, &document) {
                            return Ok(Ok(()));
                        }
                        return Ok(Err(GenerationJobStoreError::IdempotencyConflict));
                    }
                    if let Some(existing) = quarantines
                        .find_one(doc! {
                            "job_id": &document.job_id,
                            "attempt_id": &document.attempt_id,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load attempt quarantine replay", error)
                        })?
                    {
                        if quarantine_same_payload(&existing, &document) {
                            return Ok(Ok(()));
                        }
                        return Ok(Err(GenerationJobStoreError::IdempotencyConflict));
                    }
                    quarantines
                        .insert_one(document)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("insert quarantined image", error)
                        })?;
                    Ok(Ok(()))
                })
            })
            .await
            .map_err(map_database)?
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
        let now = DateTime::now();
        let mut asset_cursor = operation(
            self.store(),
            "list expired scene images",
            generated_assets(self.store())
                .find(doc! {
                    "entity_kind": "turn_scene_image",
                    "selected": false,
                    "purge_at": { "$lte": now },
                })
                .sort(doc! { "purge_at": 1, "_id": 1 })
                .limit(i64::from(limit)),
        )
        .await?;
        let mut candidates = Vec::new();
        while asset_cursor
            .advance()
            .await
            .map_err(|error| database("read expired scene image", error))?
        {
            let asset = asset_cursor
                .deserialize_current()
                .map_err(|error| database("decode expired scene image", error))?;
            candidates.push(SceneImageCleanupCandidate {
                artifact_id: Some(asset.id),
                job_id: Some(asset.job_id),
                quarantine_id: None,
                storage_keys: vec![
                    asset.original.object_key,
                    asset.web.object_key,
                    asset.thumbnail.object_key,
                ],
            });
        }
        let remaining = usize::from(limit).saturating_sub(candidates.len());
        if remaining > 0 {
            let mut quarantine_cursor = operation(
                self.store(),
                "list expired image quarantines",
                quarantined_assets(self.store())
                    .find(doc! { "purge_at": { "$lte": now } })
                    .sort(doc! { "purge_at": 1, "_id": 1 })
                    .limit(
                        i64::try_from(remaining)
                            .map_err(|_| GenerationJobStoreError::NumericRange)?,
                    ),
            )
            .await?;
            while quarantine_cursor
                .advance()
                .await
                .map_err(|error| database("read expired image quarantine", error))?
            {
                let quarantine = quarantine_cursor
                    .deserialize_current()
                    .map_err(|error| database("decode expired image quarantine", error))?;
                candidates.push(SceneImageCleanupCandidate {
                    artifact_id: None,
                    job_id: None,
                    quarantine_id: Some(quarantine.id),
                    storage_keys: quarantine.object_key.into_iter().collect(),
                });
            }
        }
        Ok(candidates)
    }

    pub async fn delete_scene_image_cleanup_candidate(
        &self,
        candidate: &SceneImageCleanupCandidate,
    ) -> Result<bool, GenerationJobStoreError> {
        validate_cleanup_candidate(candidate)?;
        let candidate = candidate.clone();
        let store = self.store().clone();
        let transaction_store = store.clone();
        transaction_store
            .with_transaction(move |session| {
                let candidate = candidate.clone();
                let store = store.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    match (
                        candidate.artifact_id.as_deref(),
                        candidate.job_id.as_deref(),
                        candidate.quarantine_id.as_deref(),
                    ) {
                        (Some(artifact_id), Some(job_id), None) => {
                            let assets = generated_assets(&store);
                            let jobs = generation_jobs(&store);
                            let Some(asset) = assets
                                .find_one(doc! {
                                    "_id": artifact_id,
                                    "job_id": job_id,
                                    "entity_kind": "turn_scene_image",
                                    "selected": false,
                                    "purge_at": { "$lte": now },
                                })
                                .session(&mut *session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo("verify expired scene image", error)
                                })?
                            else {
                                return Ok(Ok(false));
                            };
                            let persisted_keys = vec![
                                asset.original.object_key.clone(),
                                asset.web.object_key.clone(),
                                asset.thumbnail.object_key.clone(),
                            ];
                            if candidate.storage_keys != persisted_keys {
                                return Ok(Err(GenerationJobStoreError::InvalidInput(
                                    "image cleanup candidate does not match durable metadata",
                                )));
                            }
                            let deleted_job = jobs
                                .delete_one(doc! {
                                    "_id": job_id,
                                    "state": { "$in": ["succeeded", "failed", "cancelled"] },
                                    "$or": [
                                        { "artifact_id": artifact_id },
                                        { "pending_artifact_id": artifact_id },
                                    ],
                                    "purge_at": { "$lte": now },
                                })
                                .session(&mut *session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo(
                                        "delete expired illustration job",
                                        error,
                                    )
                                })?;
                            if deleted_job.deleted_count != 1 {
                                return Ok(Ok(false));
                            }
                            let deleted_asset = assets
                                .delete_one(doc! {
                                    "_id": &asset.id,
                                    "entity_kind": "turn_scene_image",
                                    "selected": false,
                                })
                                .session(&mut *session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo("delete expired scene image", error)
                                })?;
                            Ok(Ok(deleted_asset.deleted_count == 1))
                        }
                        (None, None, Some(quarantine_id)) => {
                            let quarantines = quarantined_assets(&store);
                            let Some(quarantine) = quarantines
                                .find_one(doc! {
                                    "_id": quarantine_id,
                                    "purge_at": { "$lte": now },
                                })
                                .session(&mut *session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo(
                                        "verify expired image quarantine",
                                        error,
                                    )
                                })?
                            else {
                                return Ok(Ok(false));
                            };
                            let persisted_keys = quarantine
                                .object_key
                                .clone()
                                .into_iter()
                                .collect::<Vec<_>>();
                            if candidate.storage_keys != persisted_keys {
                                return Ok(Err(GenerationJobStoreError::InvalidInput(
                                    "quarantine cleanup candidate does not match durable metadata",
                                )));
                            }
                            let deleted = quarantines
                                .delete_one(doc! {
                                    "_id": quarantine_id,
                                    "purge_at": { "$lte": now },
                                })
                                .session(&mut *session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo(
                                        "delete expired image quarantine",
                                        error,
                                    )
                                })?;
                            Ok(Ok(deleted.deleted_count == 1))
                        }
                        _ => Ok(Err(GenerationJobStoreError::InvalidInput(
                            "image cleanup candidate shape is invalid",
                        ))),
                    }
                })
            })
            .await
            .map_err(map_database)?
    }
}

impl GeneratedAssetDocument {
    fn new(
        artifact: &NewSceneImageArtifact,
        owner_account_id: &str,
        generation_attempt_id: &str,
        now: DateTime,
    ) -> Result<Self, GenerationJobStoreError> {
        let variant = |object_key: &str,
                       digest: &Sha256Digest,
                       width: u32,
                       height: u32|
         -> AssetVariantDocument {
            AssetVariantDocument {
                object_key: object_key.to_owned(),
                digest: digest.as_str().to_owned(),
                media_type: IMAGE_MEDIA_TYPE.to_owned(),
                width: i64::from(width),
                height: i64::from(height),
            }
        };
        Ok(Self {
            id: artifact.artifact_id.clone(),
            schema_version: SCENE_IMAGE_SCHEMA_VERSION,
            owner_account_id: owner_account_id.to_owned(),
            campaign_id: artifact.campaign_session_id.clone(),
            entity_kind: "turn_scene_image".to_owned(),
            entity_id: artifact.source_turn_id.clone(),
            job_id: artifact.job_id.clone(),
            generation_attempt_id: generation_attempt_id.to_owned(),
            source_turn_id: artifact.source_turn_id.clone(),
            object_key: artifact.web_storage_key.clone(),
            digest: artifact.web_digest.as_str().to_owned(),
            state: "staged".to_owned(),
            provider: artifact.provider.clone(),
            model: artifact.model.clone(),
            provider_request_id: artifact.provider_request_id.clone(),
            brief_fingerprint: artifact.brief_fingerprint.as_str().to_owned(),
            prompt_policy_fingerprint: artifact.prompt_policy_fingerprint.as_str().to_owned(),
            config_fingerprint: artifact.config_fingerprint.as_str().to_owned(),
            original: variant(
                &artifact.original_storage_key,
                &artifact.original_digest,
                artifact.original_width,
                artifact.original_height,
            ),
            web: variant(
                &artifact.web_storage_key,
                &artifact.web_digest,
                artifact.web_width,
                artifact.web_height,
            ),
            thumbnail: variant(
                &artifact.thumbnail_storage_key,
                &artifact.thumbnail_digest,
                artifact.thumbnail_width,
                artifact.thumbnail_height,
            ),
            alt_text: artifact.alt_text.clone(),
            moderation_result: SAFE_MODERATION_RESULT.to_owned(),
            selected: false,
            estimated_cost_microusd: to_i64(artifact.estimated_cost_microusd)?,
            actual_cost_microusd: artifact.actual_cost_microusd.map(to_i64).transpose()?,
            license_id: artifact.license_id.clone(),
            provenance_summary: artifact.provenance_summary.clone(),
            created_at: now,
            updated_at: now,
            purge_at: None,
        })
    }

    fn same_payload(&self, other: &Self) -> bool {
        self.id == other.id
            && self.owner_account_id == other.owner_account_id
            && self.campaign_id == other.campaign_id
            && self.entity_kind == other.entity_kind
            && self.entity_id == other.entity_id
            && self.job_id == other.job_id
            && self.generation_attempt_id == other.generation_attempt_id
            && self.source_turn_id == other.source_turn_id
            && self.object_key == other.object_key
            && self.digest == other.digest
            && self.provider == other.provider
            && self.model == other.model
            && self.provider_request_id == other.provider_request_id
            && self.brief_fingerprint == other.brief_fingerprint
            && self.prompt_policy_fingerprint == other.prompt_policy_fingerprint
            && self.config_fingerprint == other.config_fingerprint
            && self.original == other.original
            && self.web == other.web
            && self.thumbnail == other.thumbnail
            && self.alt_text == other.alt_text
            && self.moderation_result == other.moderation_result
            && self.estimated_cost_microusd == other.estimated_cost_microusd
            && self.actual_cost_microusd == other.actual_cost_microusd
            && self.license_id == other.license_id
            && self.provenance_summary == other.provenance_summary
    }

    fn to_public(&self) -> Result<SceneImageArtifact, GenerationJobStoreError> {
        if self.schema_version != SCENE_IMAGE_SCHEMA_VERSION
            || self.entity_kind != "turn_scene_image"
            || self.moderation_result != SAFE_MODERATION_RESULT
        {
            return Err(GenerationJobStoreError::InvalidStoredData(
                "invalid scene image artifact schema",
            ));
        }
        Ok(SceneImageArtifact {
            artifact_id: validated_stored_id(&self.id)?,
            job_id: validated_stored_id(&self.job_id)?,
            campaign_session_id: validated_stored_id(&self.campaign_id)?,
            source_turn_id: validated_stored_id(&self.source_turn_id)?,
            web_storage_key: validated_stored_key(&self.web.object_key)?,
            thumbnail_storage_key: validated_stored_key(&self.thumbnail.object_key)?,
            web_digest: stored_digest(&self.web.digest)?,
            thumbnail_digest: stored_digest(&self.thumbnail.digest)?,
            alt_text: validated_stored_text(&self.alt_text, 500, "invalid image alt text")?,
            selected: self.selected,
            created_at: date_string(self.created_at)?,
        })
    }
}

async fn load_campaign_owner(
    store: &MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<Option<CampaignOwnerReference>, PersistenceError> {
    campaign_owners(store)
        .find_one(doc! { "_id": campaign_id })
        .projection(doc! { "_id": 1, "owner_account_id": 1 })
        .session(session)
        .await
        .map_err(|error| PersistenceError::mongo("load campaign asset owner", error))
}

async fn turn_belongs_to_campaign(
    store: &MongoStore,
    session: &mut ClientSession,
    turn_id: &str,
    campaign_id: &str,
) -> Result<bool, PersistenceError> {
    Ok(turn_references(store)
        .find_one(doc! { "_id": turn_id, "campaign_id": campaign_id })
        .projection(doc! { "_id": 1, "campaign_id": 1 })
        .session(session)
        .await
        .map_err(|error| PersistenceError::mongo("load source turn reference", error))?
        .is_some())
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
    let expected_keys = expected_scene_storage_keys(&artifact.job_id);
    if artifact.model.trim() != artifact.model
        || artifact.model.is_empty()
        || artifact.model.chars().count() > 256
        || artifact.model.chars().any(char::is_control)
        || artifact
            .provider_request_id
            .as_deref()
            .is_some_and(|id| !is_valid_opaque_id(id) || looks_like_url(id))
        || [
            artifact.original_storage_key.as_str(),
            artifact.web_storage_key.as_str(),
            artifact.thumbnail_storage_key.as_str(),
        ]
        .into_iter()
        .any(|key| !valid_storage_key(key) || looks_like_url(key))
        || artifact.original_storage_key != expected_keys[0]
        || artifact.web_storage_key != expected_keys[1]
        || artifact.thumbnail_storage_key != expected_keys[2]
        || artifact.original_storage_key == artifact.web_storage_key
        || artifact.original_storage_key == artifact.thumbnail_storage_key
        || artifact.web_storage_key == artifact.thumbnail_storage_key
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

fn validate_new_quarantine(
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
    let expected_key = expected_quarantine_storage_key(&quarantine.attempt_id);
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
        .is_some_and(|key| !valid_storage_key(key) || looks_like_url(key) || key != expected_key)
        || quarantine
            .byte_length
            .is_some_and(|length| length > MAX_QUARANTINE_BYTES)
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "scene image quarantine metadata is invalid",
        ));
    }
    Ok(())
}

fn validate_cleanup_candidate(
    candidate: &SceneImageCleanupCandidate,
) -> Result<(), GenerationJobStoreError> {
    match (
        candidate.artifact_id.as_deref(),
        candidate.job_id.as_deref(),
        candidate.quarantine_id.as_deref(),
    ) {
        (Some(artifact), Some(job), None) => {
            validate_id(artifact, "cleanup artifact id is invalid")?;
            validate_id(job, "cleanup job id is invalid")?;
        }
        (None, None, Some(quarantine)) => {
            validate_id(quarantine, "cleanup quarantine id is invalid")?;
        }
        _ => {
            return Err(GenerationJobStoreError::InvalidInput(
                "image cleanup candidate shape is invalid",
            ));
        }
    }
    if candidate
        .storage_keys
        .iter()
        .any(|key| !valid_storage_key(key))
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "cleanup storage key is invalid",
        ));
    }
    Ok(())
}

fn quarantine_same_payload(
    left: &QuarantinedAssetDocument,
    right: &QuarantinedAssetDocument,
) -> bool {
    left.job_id == right.job_id
        && left.attempt_id == right.attempt_id
        && left.campaign_id == right.campaign_id
        && left.digest == right.digest
        && left.byte_length == right.byte_length
        && left.object_key == right.object_key
        && left.reason_code == right.reason_code
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
        && !value.contains('\\')
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn looks_like_url(value: &str) -> bool {
    value.contains("://") || value.starts_with("data:")
}

fn expected_scene_storage_keys(job_id: &str) -> [String; 3] {
    let directory = format!("{:x}", Sha256::digest(job_id.as_bytes()));
    [
        format!("artifacts/{directory}/original.png"),
        format!("artifacts/{directory}/web.png"),
        format!("artifacts/{directory}/thumbnail.png"),
    ]
}

fn expected_quarantine_storage_key(attempt_id: &str) -> String {
    format!("quarantine/{:x}.bin", Sha256::digest(attempt_id.as_bytes()))
}

fn stored_digest(value: &str) -> Result<Sha256Digest, GenerationJobStoreError> {
    Sha256Digest::new(value.to_owned())
        .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid image digest"))
}

fn validated_stored_id(value: &str) -> Result<String, GenerationJobStoreError> {
    if is_valid_opaque_id(value) {
        Ok(value.to_owned())
    } else {
        Err(GenerationJobStoreError::InvalidStoredData(
            "invalid scene image identifier",
        ))
    }
}

fn validated_stored_key(value: &str) -> Result<String, GenerationJobStoreError> {
    if valid_storage_key(value) && !looks_like_url(value) {
        Ok(value.to_owned())
    } else {
        Err(GenerationJobStoreError::InvalidStoredData(
            "invalid scene image object key",
        ))
    }
}

fn validated_media_type(value: &str) -> Result<String, GenerationJobStoreError> {
    if value == IMAGE_MEDIA_TYPE {
        Ok(value.to_owned())
    } else {
        Err(GenerationJobStoreError::InvalidStoredData(
            "invalid scene image media type",
        ))
    }
}

fn validated_stored_text(
    value: &str,
    max_chars: usize,
    reason: &'static str,
) -> Result<String, GenerationJobStoreError> {
    if value.trim() == value
        && !value.is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
    {
        Ok(value.to_owned())
    } else {
        Err(GenerationJobStoreError::InvalidStoredData(reason))
    }
}

fn to_i64(value: u64) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn date_string(value: DateTime) -> Result<String, GenerationJobStoreError> {
    value
        .try_to_rfc3339_string()
        .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid image timestamp"))
}

fn add_duration(now: DateTime, duration: Duration) -> DateTime {
    DateTime::from_millis(
        now.timestamp_millis()
            .saturating_add(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)),
    )
}

fn generation_jobs(store: &MongoStore) -> Collection<GenerationJobDocument> {
    store.collection(CollectionName::GenerationJobs)
}

fn generated_assets(store: &MongoStore) -> Collection<GeneratedAssetDocument> {
    store.collection(CollectionName::GeneratedAssets)
}

fn quarantined_assets(store: &MongoStore) -> Collection<QuarantinedAssetDocument> {
    store.collection(CollectionName::QuarantinedAssets)
}

fn campaign_owners(store: &MongoStore) -> Collection<CampaignOwnerReference> {
    store.collection(CollectionName::Campaigns)
}

fn turn_references(store: &MongoStore) -> Collection<TurnReference> {
    store.collection(CollectionName::TurnEvents)
}

async fn operation<T, A>(
    store: &MongoStore,
    operation_name: &'static str,
    action: A,
) -> Result<T, GenerationJobStoreError>
where
    A: IntoFuture<Output = mongodb::error::Result<T>>,
{
    tokio::time::timeout(store.operation_timeout(), action.into_future())
        .await
        .map_err(|_| {
            GenerationJobStoreError::Database(PersistenceError::OperationTimeout {
                operation: operation_name,
            })
        })?
        .map_err(|error| database(operation_name, error))
}

fn database(operation_name: &'static str, error: mongodb::error::Error) -> GenerationJobStoreError {
    GenerationJobStoreError::Database(PersistenceError::mongo(operation_name, error))
}

fn map_database(error: PersistenceError) -> GenerationJobStoreError {
    GenerationJobStoreError::Database(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{
            GenerationBudgetAllowance, GenerationGovernanceConfig, MongoConfig, MongoSchemaPolicy,
            SecretString,
        },
        persistence::SchemaReconciler,
        repository::{
            MongoRepository,
            governance::NewGenerationGovernanceReceipt,
            jobs::{
                EnqueueGenerationJobOutcome, GenerationClaim, GenerationSuccess, GenerationUsage,
                NewGenerationJob, SuccessRetention,
            },
        },
    };

    #[test]
    fn object_keys_reject_traversal_urls_and_ambiguous_separators() {
        for value in [
            "",
            "/absolute/image.png",
            "../image.png",
            "campaign/../image.png",
            "campaign//image.png",
            r"campaign\image.png",
            "https://provider.example/image.png",
            "data:image/png;base64,AAAA",
        ] {
            assert!(
                !valid_storage_key(value) || looks_like_url(value),
                "{value}"
            );
        }
        assert!(valid_storage_key(
            "campaign-id/turn-id/attempt-id/image.web.png"
        ));
    }

    #[test]
    fn quarantine_bounds_untrusted_metadata() {
        let quarantine = NewSceneImageQuarantine {
            id: "quarantine:one".to_owned(),
            job_id: "generation-job:one".to_owned(),
            attempt_id: "generation-attempt:one".to_owned(),
            campaign_session_id: "campaign:one".to_owned(),
            byte_digest: None,
            byte_length: Some(MAX_QUARANTINE_BYTES + 1),
            storage_key: None,
            reason_code: "byte_limit",
        };
        assert!(matches!(
            validate_new_quarantine(&quarantine),
            Err(GenerationJobStoreError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn live_mongo_asset_ownership_authorization_and_quarantine_contract() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        let campaign_id = "campaign:image-contract";
        let other_campaign_id = "campaign:image-other";
        let turn_id = "turn:image-contract";
        seed_campaign(&store, campaign_id, "account:image-owner").await;
        seed_campaign(&store, other_campaign_id, "account:image-other").await;
        store
            .document_collection(CollectionName::TurnEvents)
            .insert_one(doc! {
                "_id": turn_id,
                "schema_version": 1_i32,
                "campaign_id": campaign_id,
                "play_session_id": "play-session:image-contract",
                "sequence": 0_i64,
                "correlation_id": "correlation:image-contract",
                "created_at": DateTime::now(),
            })
            .await
            .unwrap();
        let limits = GenerationGovernanceConfig {
            campaign: GenerationBudgetAllowance {
                requests: 10,
                tokens: 10_000,
                latency_milliseconds: 60_000,
                cost_microusd: 10_000,
            },
            turn: GenerationBudgetAllowance {
                requests: 2,
                tokens: 10_000,
                latency_milliseconds: 60_000,
                cost_microusd: 10_000,
            },
            max_campaign_concurrency: 2,
            worker_batch_size: 2,
        };
        let job = NewGenerationJob {
            id: "generation-job:image-contract".to_owned(),
            campaign_session_id: campaign_id.to_owned(),
            origin_turn_id: Some(turn_id.to_owned()),
            origin_campaign_revision: 1,
            purpose: GenerationPurpose::Illustration,
            idempotency_key: "generation-image:contract".to_owned(),
            input_digest: test_digest(1),
            prompt_digest: test_digest(2),
            policy_digest: test_digest(3),
            config_digest: test_digest(4),
            correlation_id: Some("correlation:image-generation".to_owned()),
            max_attempts: 2,
            success_retention: SuccessRetention::CampaignLifetime,
            governance: Some(NewGenerationGovernanceReceipt {
                turn_scope_key: "turn-scope:image-contract".to_owned(),
                request_fingerprint: test_digest(5),
                policy_fingerprint: test_digest(6),
                config_fingerprint: test_digest(7),
                governance_fingerprint: limits.non_secret_fingerprint(),
                reserved_requests: 1,
                reserved_tokens: 1_000,
                reserved_latency_milliseconds: 30_000,
                reserved_cost_microusd: 1_000,
                limits,
            }),
        };
        assert!(matches!(
            repository.enqueue_generation_job(&job).await.unwrap(),
            EnqueueGenerationJobOutcome::Enqueued(_)
        ));
        let claimed = repository
            .claim_generation_job_by_id(
                campaign_id,
                &job.id,
                &GenerationClaim {
                    worker_id: "worker:image-contract".to_owned(),
                    provider: "provider:test".to_owned(),
                    model: "deterministic-test".to_owned(),
                    lease_duration: Duration::from_secs(30),
                },
            )
            .await
            .unwrap()
            .unwrap();
        let artifact = NewSceneImageArtifact {
            artifact_id: "generated-asset:image-contract".to_owned(),
            job_id: job.id.clone(),
            campaign_session_id: campaign_id.to_owned(),
            source_turn_id: turn_id.to_owned(),
            brief_fingerprint: test_digest(1),
            prompt_policy_fingerprint: test_digest(3),
            config_fingerprint: test_digest(4),
            provider: "provider:test".to_owned(),
            model: "deterministic-test".to_owned(),
            provider_request_id: Some("provider-request:image-contract".to_owned()),
            original_storage_key: expected_scene_storage_keys(&job.id)[0].clone(),
            web_storage_key: expected_scene_storage_keys(&job.id)[1].clone(),
            thumbnail_storage_key: expected_scene_storage_keys(&job.id)[2].clone(),
            original_digest: test_digest(11),
            web_digest: test_digest(12),
            thumbnail_digest: test_digest(13),
            original_width: 1_024,
            original_height: 1_024,
            web_width: 1_024,
            web_height: 1_024,
            thumbnail_width: 256,
            thumbnail_height: 256,
            alt_text: "Rain-lit heroes beside a Manchester canal.".to_owned(),
            estimated_cost_microusd: 500,
            actual_cost_microusd: Some(450),
            license_id: "deterministic-fake-fixture".to_owned(),
            provenance_summary: "deterministic-network-free-test-fixture".to_owned(),
        };
        repository
            .upsert_scene_image_artifact(&artifact)
            .await
            .unwrap();
        repository
            .upsert_scene_image_artifact(&artifact)
            .await
            .unwrap();
        let mut drift = artifact.clone();
        drift.web_digest = test_digest(14);
        repository
            .upsert_scene_image_artifact(&drift)
            .await
            .unwrap();
        repository
            .succeed_generation_job(
                &claimed.lease,
                &GenerationSuccess {
                    artifact_id: Some(artifact.artifact_id.clone()),
                    output_digest: artifact.original_digest.clone(),
                    usage: GenerationUsage {
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cost_microusd: artifact.actual_cost_microusd,
                        latency_milliseconds: Some(1_000),
                    },
                },
            )
            .await
            .unwrap();
        assert!(
            repository
                .authorized_scene_image_variant(
                    campaign_id,
                    &artifact.artifact_id,
                    SceneImageVariant::Web,
                )
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            repository
                .authorized_scene_image_variant(
                    other_campaign_id,
                    &artifact.artifact_id,
                    SceneImageVariant::Web,
                )
                .await
                .unwrap()
                .is_none()
        );
        repository
            .select_scene_image_artifact(campaign_id, turn_id, &artifact.artifact_id)
            .await
            .unwrap();
        assert!(
            repository
                .scene_image_artifact_for_job(campaign_id, &job.id)
                .await
                .unwrap()
                .unwrap()
                .selected
        );
        repository
            .record_scene_image_quarantine(&NewSceneImageQuarantine {
                id: "quarantined-asset:image-contract".to_owned(),
                job_id: job.id,
                attempt_id: claimed.lease.attempt_id,
                campaign_session_id: campaign_id.to_owned(),
                byte_digest: None,
                byte_length: None,
                storage_key: None,
                reason_code: "provider_url_rejected",
            })
            .await
            .unwrap();
        let quarantine = store
            .document_collection(CollectionName::QuarantinedAssets)
            .find_one(doc! { "_id": "quarantined-asset:image-contract" })
            .await
            .unwrap()
            .unwrap();
        assert!(quarantine.get_str("reason_code").is_ok());
        assert!(quarantine.get_datetime("purge_at").is_ok());
        assert!(!format!("{quarantine:?}").contains("https://"));

        assert!(
            database.starts_with("mdnd_image_test_") && database != "manchester_dnd",
            "cleanup safeguard"
        );
        store.database().drop().await.unwrap();
    }

    async fn isolated_mongo_repository() -> Option<(MongoRepository, MongoStore, String)> {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping image MongoDB contract: MONGODB_TEST_URI is not set");
            return None;
        };
        assert!(
            !uri.trim().is_empty(),
            "MONGODB_TEST_URI must not be empty when set"
        );
        let database = format!("mdnd_image_test_{}", uuid::Uuid::new_v4().simple());
        let store = MongoStore::connect(&MongoConfig {
            uri: SecretString::new(uri),
            database: database.clone(),
            max_pool_size: 8,
            min_pool_size: 0,
            connect_timeout: Duration::from_secs(5),
            server_selection_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(15),
            transaction_timeout: Duration::from_secs(10),
            transaction_max_retries: 4,
            schema_policy: MongoSchemaPolicy::ApplyAndVerify,
        })
        .await
        .unwrap();
        SchemaReconciler::new(store.clone()).apply().await.unwrap();
        Some((MongoRepository::new(store.clone()), store, database))
    }

    async fn seed_campaign(store: &MongoStore, id: &str, owner: &str) {
        let now = DateTime::now();
        store
            .document_collection(CollectionName::Campaigns)
            .insert_one(doc! {
                "_id": id,
                "schema_version": 1_i32,
                "owner_account_id": owner,
                "revision": 10_i64,
                "title_normalized": format!("test-{id}"),
                "members": [],
                "rules_snapshot": {},
                "created_at": now,
                "updated_at": now,
            })
            .await
            .unwrap();
    }

    fn test_digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; 32])
    }
}
