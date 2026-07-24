//! MongoDB owner-scoped campaign lifecycle, history, export, restore, and deletion.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use manchester_dnd_core::{SessionEventDto, Sha256Digest, is_valid_opaque_id};
use mongodb::{
    ClientSession, Collection,
    bson::{Bson, DateTime, Document, doc},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::memberships::CampaignCharacterInstanceDocument;
use super::{CampaignDocument, MongoRepository};
use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::CollectionName,
};

pub const CAMPAIGN_LIFECYCLE_SCHEMA_VERSION: u16 = 1;
pub const CAMPAIGN_EXPORT_SCHEMA_VERSION: u16 = 1;
pub const CAMPAIGN_HISTORY_DEFAULT_LIMIT: u16 = 25;
pub const CAMPAIGN_HISTORY_MAX_LIMIT: u16 = 100;

const MAX_PLAYER_EXPORT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXPORTED_DOCUMENTS: usize = 5_000;
const DELETION_PREPARATION_SECONDS: i64 = 60 * 60;
const DELETION_TOMBSTONE_SECONDS: i64 = 35 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignLifecycleState {
    Active,
    Archived,
}

impl CampaignLifecycleState {
    const fn storage_value(self) -> &'static str {
        match self {
            Self::Active => "open",
            Self::Archived => "archived",
        }
    }

    fn from_storage(value: &str, campaign_id: &str) -> Result<Self, RepositoryError> {
        match value {
            "open" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            _ => invalid("campaign lifecycle", campaign_id, "unknown lifecycle state"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignSummary {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub owner_key: String,
    pub title: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub lifecycle_state: CampaignLifecycleState,
    pub archived_at: Option<String>,
    pub safety_policy_id: String,
    pub progression_policy_id: String,
    pub retention_class: String,
    pub retention_delete_after: Option<String>,
    pub open_play_session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLifecycleCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_lifecycle_revision: u64,
    pub idempotency_key: String,
}

impl CampaignLifecycleCommand {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        if self.schema_version != CAMPAIGN_LIFECYCLE_SCHEMA_VERSION {
            return Err(RepositoryError::UnsupportedSchemaVersion {
                entity: "campaign lifecycle command",
                found: u32::from(self.schema_version),
                supported: u32::from(CAMPAIGN_LIFECYCLE_SCHEMA_VERSION),
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_lifecycle_revision == 0
        {
            return invalid(
                "campaign lifecycle command",
                &self.campaign_session_id,
                "ids and expected lifecycle revision must be valid",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartPlaySessionCommand {
    pub lifecycle: CampaignLifecycleCommand,
    pub play_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndPlaySessionCommand {
    pub lifecycle: CampaignLifecycleCommand,
    pub play_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteCampaignCommand {
    pub lifecycle: CampaignLifecycleCommand,
    pub deletion_id: String,
    pub confirm_permanent_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCampaignDeletion {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub deletion_id: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub canonical_export_digest: Sha256Digest,
    pub canonical_export_json: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLifecycleOutcome {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub lifecycle_revision: u64,
    pub lifecycle_state: Option<CampaignLifecycleState>,
    pub play_session_id: Option<String>,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPlaySession {
    pub schema_version: u16,
    pub id: String,
    pub campaign_session_id: String,
    pub owner_key: String,
    pub state: String,
    pub started_campaign_revision: u64,
    pub ended_campaign_revision: Option<u64>,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignTurnHistoryItem {
    pub schema_version: u32,
    pub id: String,
    pub campaign_session_id: String,
    pub turn_number: u64,
    pub actor_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event: SessionEventDto,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignTurnHistoryPage {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub items: Vec<CampaignTurnHistoryItem>,
    pub next_after_turn_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifecycleAuditPayload {
    PlayStarted {
        play_session_id: String,
    },
    PlayEnded {
        play_session_id: String,
    },
    Archived,
    Restored,
    RestoreImported {
        closed_play_session_ids: Vec<String>,
    },
}

impl LifecycleAuditPayload {
    const fn event_kind(&self) -> &'static str {
        match self {
            Self::PlayStarted { .. } => "play_started",
            Self::PlayEnded { .. } => "play_ended",
            Self::Archived => "archived",
            Self::Restored => "restored",
            Self::RestoreImported { .. } => "restore_imported",
        }
    }
}

/// Explicit bounded export. Every vector maps to one allowlisted campaign
/// collection. Authentication, invitations, throttles, jobs, quarantines, and
/// deletion-control documents are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPrivateExportV1 {
    pub schema_version: u16,
    pub owner_key: String,
    pub exported_at: String,
    pub campaign: Document,
    pub character_instances: Vec<Document>,
    pub play_sessions: Vec<Document>,
    pub turn_events: Vec<Document>,
    pub command_receipts: Vec<Document>,
    pub audit_events: Vec<Document>,
    pub campaign_enemy_instances: Vec<Document>,
    pub campaign_events: Vec<Document>,
    pub encounters: Vec<Document>,
    pub bde_ledger: Vec<Document>,
    pub private_inspiration_participants: Vec<Document>,
    pub private_inspiration_sources: Vec<Document>,
    pub private_inspiration_consents: Vec<Document>,
    pub private_inspiration_vetoes: Vec<Document>,
    pub private_inspiration_selections: Vec<Document>,
    pub private_inspiration_work: Vec<Document>,
    pub selected_generated_presentations: Vec<Document>,
    pub selected_generated_assets: Vec<Document>,
}

impl CampaignPrivateExportV1 {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        validate_export(self)
    }

    pub fn canonical_json(&self) -> Result<String, RepositoryError> {
        self.validate()?;
        canonical_json(self)
    }

    pub fn canonical_digest(&self) -> Result<Sha256Digest, RepositoryError> {
        Ok(digest(self.canonical_json()?.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreCampaignExportCommand {
    pub schema_version: u16,
    pub idempotency_key: String,
    pub canonical_export_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaySessionDocument {
    #[serde(rename = "_id")]
    pub(crate) id: String,
    pub(crate) schema_version: i64,
    pub(crate) revision: i64,
    pub(crate) campaign_id: String,
    pub(crate) gm_account_id: String,
    pub(crate) state: String,
    pub(crate) participants: Vec<Document>,
    pub(crate) mode: String,
    pub(crate) turn_state: Document,
    pub(crate) membership_snapshot: Document,
    pub(crate) start_policy: String,
    pub(crate) opened_at: DateTime,
    pub(crate) updated_at: DateTime,
    pub(crate) closed_at: Option<DateTime>,
    pub(crate) close_reason: Option<String>,
    pub(crate) started_campaign_revision: i64,
    pub(crate) ended_campaign_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnEventDocument {
    #[serde(rename = "_id")]
    pub(crate) id: String,
    pub(crate) schema_version: i64,
    pub(crate) campaign_id: String,
    pub(crate) play_session_id: String,
    pub(crate) sequence: i64,
    pub(crate) correlation_id: Option<String>,
    pub(crate) actor_account_id: Option<String>,
    pub(crate) event: SessionEventDto,
    pub(crate) created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleReceiptDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    scope_kind: String,
    scope_id: String,
    campaign_id: String,
    actor_account_id: String,
    command_kind: String,
    idempotency_key: String,
    request_fingerprint: Sha256Digest,
    expected_revision: i64,
    result_revision: i64,
    response: Document,
    state: String,
    retain_after_delete: bool,
    created_at: DateTime,
    purge_at: Option<DateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletionPreparationDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    deletion_id: String,
    owner_account_id: String,
    scope_kind: String,
    scope_id: String,
    campaign_revision: i64,
    lifecycle_revision: i64,
    digest: Sha256Digest,
    canonical_export_json: String,
    expires_at: DateTime,
    purge_at: DateTime,
    created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletionTombstoneDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    entity_kind: String,
    entity_id: String,
    deletion_id: String,
    digest: Sha256Digest,
    owner_account_id: String,
    deleted_lifecycle_revision: i64,
    deleted_at: DateTime,
    purge_at: DateTime,
}

#[derive(Debug)]
struct ExportSnapshot {
    campaign: CampaignDocument,
    documents: BTreeMap<&'static str, Vec<Document>>,
}

impl MongoRepository {
    pub async fn list_owned_campaigns(
        &self,
        owner_key: &str,
    ) -> Result<Vec<CampaignSummary>, RepositoryError> {
        validate_owner(owner_key)?;
        let mut cursor = self
            .campaigns()
            .find(doc! { "owner_account_id": owner_key })
            .sort(doc! { "updated_at": -1, "_id": 1 })
            .await
            .map_err(|error| mongo_error("list owned campaigns", error))?;
        let mut summaries = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read owned campaigns", error))?
        {
            summaries.push(campaign_summary_from_document(
                cursor
                    .deserialize_current()
                    .map_err(|error| mongo_error("decode owned campaign", error))?,
            )?);
        }
        Ok(summaries)
    }

    pub async fn load_owned_campaign_summary(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<Option<CampaignSummary>, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        self.campaigns()
            .find_one(doc! {
                "_id": campaign_session_id,
                "owner_account_id": owner_key,
            })
            .await
            .map_err(|error| mongo_error("load owned campaign", error))?
            .map(campaign_summary_from_document)
            .transpose()
    }

    pub async fn has_campaign_deletion_tombstone(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        Ok(self
            .store()
            .document_collection(CollectionName::DeletionTombstones)
            .find_one(doc! {
                "entity_kind": "campaign",
                "entity_id": campaign_session_id,
                "owner_account_id": owner_key,
                "purge_at": { "$gt": DateTime::now() },
            })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|error| mongo_error("load campaign deletion tombstone", error))?
            .is_some())
    }

    pub(crate) async fn retire_deleted_campaign_receipts_for_recreate(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<(), RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        if self
            .campaigns()
            .find_one(doc! {
                "_id": campaign_session_id,
                "owner_account_id": owner_key,
            })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|error| mongo_error("check campaign before receipt retirement", error))?
            .is_some()
        {
            return invalid(
                "campaign receipt retirement",
                campaign_session_id,
                "campaign still exists",
            );
        }
        self.store()
            .document_collection(CollectionName::CommandReceipts)
            .update_many(
                doc! {
                    "scope_kind": "campaign",
                    "scope_id": campaign_session_id,
                    "actor_account_id": owner_key,
                    "retain_after_delete": true,
                    "state": "committed",
                },
                doc! {
                    "$set": {
                        "state": "retired",
                        "purge_at": DateTime::now(),
                    }
                },
            )
            .await
            .map_err(|error| mongo_error("retire deleted campaign receipts", error))?;
        Ok(())
    }

    pub async fn list_campaign_turn_history(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
        after_turn_number: Option<u64>,
        limit: u16,
    ) -> Result<CampaignTurnHistoryPage, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        if limit == 0 || limit > CAMPAIGN_HISTORY_MAX_LIMIT {
            return invalid(
                "campaign turn history",
                campaign_session_id,
                "limit is outside the supported range",
            );
        }
        require_owned_campaign(self, owner_key, campaign_session_id).await?;
        let after = to_i64(after_turn_number.unwrap_or(0), "turn history cursor")?;
        let mut cursor = self
            .store()
            .collection::<TurnEventDocument>(CollectionName::TurnEvents)
            .find(doc! {
                "campaign_id": campaign_session_id,
                "sequence": { "$gt": after },
            })
            .sort(doc! { "sequence": 1, "_id": 1 })
            .limit(i64::from(limit) + 1)
            .await
            .map_err(|error| mongo_error("list campaign turn history", error))?;
        let mut items = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read campaign turn history", error))?
        {
            let document = cursor
                .deserialize_current()
                .map_err(|error| mongo_error("decode campaign turn history", error))?;
            items.push(turn_history_from_document(document)?);
        }
        let next_after_turn_number = if items.len() > usize::from(limit) {
            items.truncate(usize::from(limit));
            items.last().map(|item| item.turn_number)
        } else {
            None
        };
        Ok(CampaignTurnHistoryPage {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.to_owned(),
            items,
            next_after_turn_number,
        })
    }

    pub async fn list_campaign_play_sessions(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<Vec<CampaignPlaySession>, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        require_owned_campaign(self, owner_key, campaign_session_id).await?;
        let mut cursor = self
            .store()
            .collection::<PlaySessionDocument>(CollectionName::PlaySessions)
            .find(doc! {
                "campaign_id": campaign_session_id,
                "gm_account_id": owner_key,
            })
            .sort(doc! { "opened_at": 1, "_id": 1 })
            .await
            .map_err(|error| mongo_error("list campaign play sessions", error))?;
        let mut output = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| mongo_error("read campaign play sessions", error))?
        {
            output.push(play_session_from_document(
                cursor
                    .deserialize_current()
                    .map_err(|error| mongo_error("decode campaign play session", error))?,
            )?);
        }
        Ok(output)
    }

    pub async fn start_campaign_play_session(
        &self,
        owner_key: &str,
        command: &StartPlaySessionCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        validate_owner(owner_key)?;
        command.lifecycle.validate()?;
        if !is_valid_opaque_id(&command.play_session_id) {
            return invalid(
                "campaign play session",
                &command.play_session_id,
                "play session id is invalid",
            );
        }
        let fingerprint = command_fingerprint("campaign_play_start", command)?;
        let campaigns = self.campaigns();
        let play_sessions = self
            .store()
            .collection::<PlaySessionDocument>(CollectionName::PlaySessions);
        let receipts = self
            .store()
            .collection::<LifecycleReceiptDocument>(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let command_owned = command.clone();
        let owner_owned = owner_key.to_owned();
        let play_id = command.play_session_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let play_sessions = play_sessions.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let command = command_owned.clone();
            let owner = owner_owned.clone();
            let fingerprint = fingerprint.clone();
            Box::pin(async move {
                if let Some(replay) = load_lifecycle_replay(
                    &receipts,
                    session,
                    &owner,
                    &command.lifecycle.campaign_session_id,
                    &command.lifecycle.idempotency_key,
                    "campaign_play_start",
                    &fingerprint,
                )
                .await?
                {
                    return Ok(replay);
                }
                let campaign = load_owned_gm_campaign(
                    &campaigns,
                    session,
                    &owner,
                    &command.lifecycle.campaign_session_id,
                )
                .await?;
                require_lifecycle_revision(
                    &campaign,
                    command.lifecycle.expected_lifecycle_revision,
                )?;
                if campaign.lifecycle.state != "open" || campaign.current_play_session_id.is_some()
                {
                    return Err(PersistenceError::AlreadyExists {
                        entity: "campaign_play_session",
                        id: command.play_session_id.clone(),
                    });
                }
                let new_revision =
                    next_revision(campaign.lifecycle_revision, "lifecycle revision")?;
                let now = DateTime::now();
                let participant_count = i64::try_from(campaign.members.len()).map_err(|_| {
                    PersistenceError::SchemaDrift {
                        collection: "campaigns".to_owned(),
                        detail: "embedded membership count is outside the supported range"
                            .to_owned(),
                    }
                })?;
                let play = PlaySessionDocument {
                    id: command.play_session_id.clone(),
                    schema_version: 1,
                    revision: 1,
                    campaign_id: campaign.id.clone(),
                    gm_account_id: owner.clone(),
                    state: "active".to_owned(),
                    participants: campaign
                        .members
                        .iter()
                        .filter(|member| member.state == "active")
                        .map(|member| {
                            doc! {
                                "account_id": &member.account_id,
                                "role": &member.role,
                                "state": "ready",
                            }
                        })
                        .collect(),
                    mode: "exploration".to_owned(),
                    turn_state: doc! { "phase": "open", "active_account_id": Bson::Null },
                    membership_snapshot: doc! {
                        "campaign_revision": campaign.revision,
                        "member_count": participant_count,
                    },
                    start_policy: "game_master".to_owned(),
                    opened_at: now,
                    updated_at: now,
                    closed_at: None,
                    close_reason: None,
                    started_campaign_revision: campaign.revision,
                    ended_campaign_revision: None,
                };
                play_sessions
                    .insert_one(play)
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("start play session", error))?;
                let updated = campaigns
                    .update_one(
                        doc! {
                            "_id": &campaign.id,
                            "owner_account_id": &owner,
                            "lifecycle_revision": campaign.lifecycle_revision,
                            "current_play_session_id": Bson::Null,
                            "lifecycle.state": "open",
                        },
                        doc! {
                            "$set": {
                                "current_play_session_id": &command.play_session_id,
                                "updated_at": now,
                            },
                            "$inc": { "lifecycle_revision": 1_i64 },
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("advance campaign play state", error)
                    })?;
                if updated.modified_count != 1 {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_lifecycle",
                        id: campaign.id,
                        expected: command.lifecycle.expected_lifecycle_revision,
                        actual: nonnegative_u64(campaign.lifecycle_revision),
                    });
                }
                let outcome = CampaignLifecycleOutcome {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: command.lifecycle.campaign_session_id.clone(),
                    lifecycle_revision: nonnegative_u64(new_revision),
                    lifecycle_state: Some(CampaignLifecycleState::Active),
                    play_session_id: Some(command.play_session_id.clone()),
                    deleted: false,
                };
                insert_lifecycle_audit(
                    &audits,
                    session,
                    &owner,
                    &outcome,
                    LifecycleAuditPayload::PlayStarted {
                        play_session_id: command.play_session_id.clone(),
                    },
                )
                .await?;
                insert_lifecycle_receipt(
                    &receipts,
                    session,
                    &owner,
                    &command.lifecycle,
                    "campaign_play_start",
                    fingerprint,
                    &outcome,
                    false,
                )
                .await?;
                Ok(outcome)
            })
        })
        .await
        .map_err(|error| map_transaction_error(error, "campaign play session", &play_id))
    }

    pub async fn end_campaign_play_session(
        &self,
        owner_key: &str,
        command: &EndPlaySessionCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        validate_owner(owner_key)?;
        command.lifecycle.validate()?;
        if !is_valid_opaque_id(&command.play_session_id) {
            return invalid(
                "campaign play session",
                &command.play_session_id,
                "play session id is invalid",
            );
        }
        let fingerprint = command_fingerprint("campaign_play_end", command)?;
        let campaigns = self.campaigns();
        let play_sessions = self
            .store()
            .collection::<PlaySessionDocument>(CollectionName::PlaySessions);
        let receipts = self
            .store()
            .collection::<LifecycleReceiptDocument>(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let command_owned = command.clone();
        let owner_owned = owner_key.to_owned();
        let play_id = command.play_session_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let play_sessions = play_sessions.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let command = command_owned.clone();
            let owner = owner_owned.clone();
            let fingerprint = fingerprint.clone();
            Box::pin(async move {
                if let Some(replay) = load_lifecycle_replay(
                    &receipts,
                    session,
                    &owner,
                    &command.lifecycle.campaign_session_id,
                    &command.lifecycle.idempotency_key,
                    "campaign_play_end",
                    &fingerprint,
                )
                .await?
                {
                    return Ok(replay);
                }
                let campaign = load_owned_gm_campaign(
                    &campaigns,
                    session,
                    &owner,
                    &command.lifecycle.campaign_session_id,
                )
                .await?;
                require_lifecycle_revision(
                    &campaign,
                    command.lifecycle.expected_lifecycle_revision,
                )?;
                if campaign.current_play_session_id.as_deref()
                    != Some(command.play_session_id.as_str())
                {
                    return Err(PersistenceError::NotFound {
                        entity: "campaign_play_session",
                        id: command.play_session_id.clone(),
                    });
                }
                let now = DateTime::now();
                let closed = play_sessions
                    .update_one(
                        doc! {
                            "_id": &command.play_session_id,
                            "campaign_id": &campaign.id,
                            "gm_account_id": &owner,
                            "state": { "$in": ["waiting", "active"] },
                        },
                        doc! {
                            "$set": {
                                "state": "closed",
                                "closed_at": now,
                                "updated_at": now,
                                "close_reason": "game_master_ended",
                                "ended_campaign_revision": campaign.revision,
                            },
                            "$inc": { "revision": 1_i64 },
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("end play session", error))?;
                if closed.modified_count != 1 {
                    return Err(PersistenceError::NotFound {
                        entity: "campaign_play_session",
                        id: command.play_session_id.clone(),
                    });
                }
                let updated = campaigns
                    .update_one(
                        doc! {
                            "_id": &campaign.id,
                            "owner_account_id": &owner,
                            "lifecycle_revision": campaign.lifecycle_revision,
                            "current_play_session_id": &command.play_session_id,
                        },
                        doc! {
                            "$set": {
                                "current_play_session_id": Bson::Null,
                                "updated_at": now,
                            },
                            "$inc": { "lifecycle_revision": 1_i64 },
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("close campaign play state", error))?;
                if updated.modified_count != 1 {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_lifecycle",
                        id: campaign.id,
                        expected: command.lifecycle.expected_lifecycle_revision,
                        actual: nonnegative_u64(campaign.lifecycle_revision),
                    });
                }
                let result_revision =
                    next_revision(campaign.lifecycle_revision, "lifecycle revision")?;
                let outcome = CampaignLifecycleOutcome {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: command.lifecycle.campaign_session_id.clone(),
                    lifecycle_revision: nonnegative_u64(result_revision),
                    lifecycle_state: Some(CampaignLifecycleState::Active),
                    play_session_id: Some(command.play_session_id.clone()),
                    deleted: false,
                };
                insert_lifecycle_audit(
                    &audits,
                    session,
                    &owner,
                    &outcome,
                    LifecycleAuditPayload::PlayEnded {
                        play_session_id: command.play_session_id.clone(),
                    },
                )
                .await?;
                insert_lifecycle_receipt(
                    &receipts,
                    session,
                    &owner,
                    &command.lifecycle,
                    "campaign_play_end",
                    fingerprint,
                    &outcome,
                    false,
                )
                .await?;
                Ok(outcome)
            })
        })
        .await
        .map_err(|error| map_transaction_error(error, "campaign play session", &play_id))
    }

    pub async fn archive_campaign(
        &self,
        owner_key: &str,
        command: &CampaignLifecycleCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        self.change_campaign_lifecycle(
            owner_key,
            command,
            CampaignLifecycleState::Active,
            CampaignLifecycleState::Archived,
            "campaign_archive",
            LifecycleAuditPayload::Archived,
        )
        .await
    }

    pub async fn restore_archived_campaign(
        &self,
        owner_key: &str,
        command: &CampaignLifecycleCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        self.change_campaign_lifecycle(
            owner_key,
            command,
            CampaignLifecycleState::Archived,
            CampaignLifecycleState::Active,
            "campaign_unarchive",
            LifecycleAuditPayload::Restored,
        )
        .await
    }

    async fn change_campaign_lifecycle(
        &self,
        owner_key: &str,
        command: &CampaignLifecycleCommand,
        required: CampaignLifecycleState,
        target: CampaignLifecycleState,
        command_kind: &'static str,
        payload: LifecycleAuditPayload,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        validate_owner(owner_key)?;
        command.validate()?;
        let fingerprint = command_fingerprint(command_kind, command)?;
        let campaigns = self.campaigns();
        let receipts = self
            .store()
            .collection::<LifecycleReceiptDocument>(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let store = self.store().clone();
        let command_owned = command.clone();
        let owner_owned = owner_key.to_owned();
        let campaign_id = command.campaign_session_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let store = store.clone();
            let command = command_owned.clone();
            let owner = owner_owned.clone();
            let fingerprint = fingerprint.clone();
            let payload = payload.clone();
            Box::pin(async move {
                if let Some(replay) = load_lifecycle_replay(
                    &receipts,
                    session,
                    &owner,
                    &command.campaign_session_id,
                    &command.idempotency_key,
                    command_kind,
                    &fingerprint,
                )
                .await?
                {
                    return Ok(replay);
                }
                let campaign = load_owned_gm_campaign(
                    &campaigns,
                    session,
                    &owner,
                    &command.campaign_session_id,
                )
                .await?;
                require_lifecycle_revision(&campaign, command.expected_lifecycle_revision)?;
                if campaign.lifecycle.state != required.storage_value()
                    || campaign.current_play_session_id.is_some()
                {
                    return Err(PersistenceError::AlreadyExists {
                        entity: "campaign_lifecycle",
                        id: campaign.id,
                    });
                }
                require_no_open_campaign_runtime(&store, session, &campaign.id).await?;
                let now = DateTime::now();
                let archived_at = match target {
                    CampaignLifecycleState::Active => Bson::Null,
                    CampaignLifecycleState::Archived => Bson::DateTime(now),
                };
                let updated = campaigns
                    .update_one(
                        doc! {
                            "_id": &campaign.id,
                            "owner_account_id": &owner,
                            "lifecycle_revision": campaign.lifecycle_revision,
                            "lifecycle.state": required.storage_value(),
                            "current_play_session_id": Bson::Null,
                        },
                        doc! {
                            "$set": {
                                "lifecycle.state": target.storage_value(),
                                "lifecycle.archived_at": archived_at,
                                "updated_at": now,
                            },
                            "$inc": { "lifecycle_revision": 1_i64 },
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("change campaign lifecycle", error))?;
                if updated.modified_count != 1 {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_lifecycle",
                        id: campaign.id,
                        expected: command.expected_lifecycle_revision,
                        actual: nonnegative_u64(campaign.lifecycle_revision),
                    });
                }
                let result_revision =
                    next_revision(campaign.lifecycle_revision, "lifecycle revision")?;
                let outcome = CampaignLifecycleOutcome {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: command.campaign_session_id.clone(),
                    lifecycle_revision: nonnegative_u64(result_revision),
                    lifecycle_state: Some(target),
                    play_session_id: None,
                    deleted: false,
                };
                insert_lifecycle_audit(&audits, session, &owner, &outcome, payload).await?;
                insert_lifecycle_receipt(
                    &receipts,
                    session,
                    &owner,
                    &command,
                    command_kind,
                    fingerprint,
                    &outcome,
                    false,
                )
                .await?;
                Ok(outcome)
            })
        })
        .await
        .map_err(|error| map_transaction_error(error, "campaign lifecycle", &campaign_id))
    }

    pub async fn export_campaign_private(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<CampaignPrivateExportV1, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        let campaigns = self.campaigns();
        let store = self.store().clone();
        let owner_owned = owner_key.to_owned();
        let campaign_id_owned = campaign_session_id.to_owned();
        let snapshot = self
            .with_transaction(move |session| {
                let campaigns = campaigns.clone();
                let store = store.clone();
                let owner = owner_owned.clone();
                let campaign_id = campaign_id_owned.clone();
                Box::pin(async move {
                    let campaign = campaigns
                        .find_one(doc! {
                            "_id": &campaign_id,
                            "owner_account_id": &owner,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load campaign export", error))?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "campaign_session",
                            id: campaign_id.clone(),
                        })?;
                    let documents = load_export_documents(&store, session, &campaign_id).await?;
                    Ok(ExportSnapshot {
                        campaign,
                        documents,
                    })
                })
            })
            .await
            .map_err(|error| {
                map_transaction_error(error, "campaign private export", campaign_session_id)
            })?;
        let export = export_from_snapshot(owner_key, snapshot)?;
        export.validate()?;
        Ok(export)
    }

    pub async fn export_campaign_canonical_json(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<String, RepositoryError> {
        self.export_campaign_private(owner_key, campaign_session_id)
            .await?
            .canonical_json()
    }

    pub async fn export_campaign_player_readable(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<String, RepositoryError> {
        let export = self
            .export_campaign_private(owner_key, campaign_session_id)
            .await?;
        let campaign: CampaignDocument = mongodb::bson::from_document(export.campaign.clone())?;
        let state = CampaignLifecycleState::from_storage(&campaign.lifecycle.state, &campaign.id)?;
        let mut lines = vec![
            format!("# {}", campaign.title),
            String::new(),
            format!("Campaign: {}", campaign.id),
            format!("State: {}", state_label(state)),
            format!("Revision: {}", nonnegative_u64(campaign.revision)),
            format!("Members: {}", campaign.members.len()),
            format!("Assigned characters: {}", export.character_instances.len()),
            format!("Play sessions: {}", export.play_sessions.len()),
            format!("Turns: {}", export.turn_events.len()),
        ];
        for body in export
            .selected_generated_presentations
            .iter()
            .filter(|document| document.get_str("presentation_type") == Ok("private_recap"))
            .filter_map(|document| document.get_str("body").ok())
        {
            lines.push(String::new());
            lines.push("## Private recap".to_owned());
            lines.push(String::new());
            lines.push(body.to_owned());
        }
        let readable = lines.join("\n");
        if readable.len() > MAX_PLAYER_EXPORT_BYTES {
            return invalid(
                "campaign player-readable export",
                campaign_session_id,
                "rendered export exceeds the supported size",
            );
        }
        Ok(readable)
    }

    pub async fn prepare_campaign_deletion(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
        expected_lifecycle_revision: u64,
        deletion_id: &str,
    ) -> Result<PreparedCampaignDeletion, RepositoryError> {
        validate_owner_campaign(owner_key, campaign_session_id)?;
        if expected_lifecycle_revision == 0 || !is_valid_opaque_id(deletion_id) {
            return invalid(
                "campaign deletion preparation",
                deletion_id,
                "deletion id or expected lifecycle revision is invalid",
            );
        }
        let preparations = self
            .store()
            .collection::<DeletionPreparationDocument>(CollectionName::DeletionPreparations);
        if let Some(existing) = preparations
            .find_one(doc! { "deletion_id": deletion_id })
            .await
            .map_err(|error| mongo_error("load campaign deletion preparation replay", error))?
        {
            if existing.owner_account_id != owner_key
                || existing.scope_kind != "campaign"
                || existing.scope_id != campaign_session_id
            {
                return Err(RepositoryError::AlreadyExists {
                    entity: "campaign deletion preparation",
                    id: deletion_id.to_owned(),
                });
            }
            if existing.expires_at > DateTime::now() {
                let current = require_owned_campaign(self, owner_key, campaign_session_id).await?;
                if nonnegative_u64(existing.lifecycle_revision) != expected_lifecycle_revision
                    || existing.campaign_revision != current.revision
                    || existing.lifecycle_revision != current.lifecycle_revision
                    || current.lifecycle.state != "archived"
                    || current.current_play_session_id.is_some()
                    || digest(existing.canonical_export_json.as_bytes()) != existing.digest
                {
                    return Err(RepositoryError::RevisionConflict {
                        entity: "campaign deletion preparation",
                        id: deletion_id.to_owned(),
                        expected: expected_lifecycle_revision,
                        actual: nonnegative_u64(existing.lifecycle_revision),
                    });
                }
                return prepared_deletion_from_document(existing);
            }
        }
        let export = self
            .export_campaign_private(owner_key, campaign_session_id)
            .await?;
        let campaign: CampaignDocument = mongodb::bson::from_document(export.campaign.clone())?;
        if campaign.lifecycle.state != "archived"
            || nonnegative_u64(campaign.lifecycle_revision) != expected_lifecycle_revision
            || campaign.current_play_session_id.is_some()
        {
            return invalid(
                "campaign deletion preparation",
                campaign_session_id,
                "campaign must be archived, closed, and at the expected revision",
            );
        }
        let canonical_export_json = export.canonical_json()?;
        let canonical_export_digest = digest(canonical_export_json.as_bytes());
        let now = DateTime::now();
        let expires_at = add_seconds(now, DELETION_PREPARATION_SECONDS);
        let preparation_key = format!(
            "deletion-preparation:{:x}",
            Sha256::digest(format!("{owner_key}\0{deletion_id}").as_bytes())
        );
        let preparation = DeletionPreparationDocument {
            id: preparation_key.clone(),
            schema_version: 1,
            deletion_id: deletion_id.to_owned(),
            owner_account_id: owner_key.to_owned(),
            scope_kind: "campaign".to_owned(),
            scope_id: campaign_session_id.to_owned(),
            campaign_revision: campaign.revision,
            lifecycle_revision: campaign.lifecycle_revision,
            digest: canonical_export_digest.clone(),
            canonical_export_json: canonical_export_json.clone(),
            expires_at,
            purge_at: expires_at,
            created_at: now,
        };
        let campaigns = self.campaigns();
        let store = self.store().clone();
        let campaign_id = campaign_session_id.to_owned();
        let owner = owner_key.to_owned();
        let preparation_for_write = preparation.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let store = store.clone();
            let preparations = preparations.clone();
            let campaign_id = campaign_id.clone();
            let owner = owner.clone();
            let preparation = preparation_for_write.clone();
            Box::pin(async move {
                let locked =
                    load_owned_gm_campaign(&campaigns, session, &owner, &campaign_id).await?;
                if locked.lifecycle.state != "archived"
                    || locked.current_play_session_id.is_some()
                    || locked.revision != preparation.campaign_revision
                    || locked.lifecycle_revision != preparation.lifecycle_revision
                {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_lifecycle",
                        id: campaign_id,
                        expected: nonnegative_u64(preparation.lifecycle_revision),
                        actual: nonnegative_u64(locked.lifecycle_revision),
                    });
                }
                require_no_open_campaign_runtime(&store, session, &locked.id).await?;
                preparations
                    .replace_one(doc! { "_id": &preparation.id }, preparation)
                    .upsert(true)
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("prepare campaign deletion", error))?;
                Ok(())
            })
        })
        .await
        .map_err(|error| {
            map_transaction_error(error, "campaign deletion preparation", deletion_id)
        })?;
        Ok(PreparedCampaignDeletion {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_session_id.to_owned(),
            deletion_id: deletion_id.to_owned(),
            campaign_revision: nonnegative_u64(campaign.revision),
            lifecycle_revision: nonnegative_u64(campaign.lifecycle_revision),
            canonical_export_digest,
            canonical_export_json,
            expires_at: date_string(expires_at, "deletion_preparations")?,
        })
    }

    pub async fn delete_archived_campaign(
        &self,
        owner_key: &str,
        command: &DeleteCampaignCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        validate_owner(owner_key)?;
        command.lifecycle.validate()?;
        if !command.confirm_permanent_delete || !is_valid_opaque_id(&command.deletion_id) {
            return invalid(
                "campaign deletion",
                &command.deletion_id,
                "explicit confirmation and a valid deletion id are required",
            );
        }
        let fingerprint = command_fingerprint("campaign_delete", command)?;
        if let Some(replay) = self
            .load_retained_lifecycle_replay(
                owner_key,
                &command.lifecycle.campaign_session_id,
                &command.lifecycle.idempotency_key,
                "campaign_delete",
                &fingerprint,
            )
            .await?
        {
            return Ok(replay);
        }
        let campaigns = self.campaigns();
        let store = self.store().clone();
        let receipts = self
            .store()
            .collection::<LifecycleReceiptDocument>(CollectionName::CommandReceipts);
        let preparations = self
            .store()
            .collection::<DeletionPreparationDocument>(CollectionName::DeletionPreparations);
        let tombstones = self
            .store()
            .collection::<DeletionTombstoneDocument>(CollectionName::DeletionTombstones);
        let command_owned = command.clone();
        let owner_owned = owner_key.to_owned();
        let campaign_id = command.lifecycle.campaign_session_id.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let store = store.clone();
            let receipts = receipts.clone();
            let preparations = preparations.clone();
            let tombstones = tombstones.clone();
            let command = command_owned.clone();
            let owner = owner_owned.clone();
            let fingerprint = fingerprint.clone();
            Box::pin(async move {
                if let Some(replay) = load_lifecycle_replay(
                    &receipts,
                    session,
                    &owner,
                    &command.lifecycle.campaign_session_id,
                    &command.lifecycle.idempotency_key,
                    "campaign_delete",
                    &fingerprint,
                )
                .await?
                {
                    return Ok(replay);
                }
                let campaign = load_owned_gm_campaign(
                    &campaigns,
                    session,
                    &owner,
                    &command.lifecycle.campaign_session_id,
                )
                .await?;
                require_lifecycle_revision(
                    &campaign,
                    command.lifecycle.expected_lifecycle_revision,
                )?;
                if campaign.lifecycle.state != "archived"
                    || campaign.current_play_session_id.is_some()
                {
                    return Err(PersistenceError::AlreadyExists {
                        entity: "campaign_deletion",
                        id: campaign.id,
                    });
                }
                require_no_open_campaign_runtime(&store, session, &campaign.id).await?;
                let preparation = preparations
                    .find_one(doc! {
                        "deletion_id": &command.deletion_id,
                        "owner_account_id": &owner,
                        "scope_kind": "campaign",
                        "scope_id": &campaign.id,
                        "expires_at": { "$gt": DateTime::now() },
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load campaign deletion preparation", error)
                    })?
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "campaign_deletion_preparation",
                        id: command.deletion_id.clone(),
                    })?;
                if preparation.campaign_revision != campaign.revision
                    || preparation.lifecycle_revision != campaign.lifecycle_revision
                    || digest(preparation.canonical_export_json.as_bytes()) != preparation.digest
                {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_deletion_preparation",
                        id: command.deletion_id.clone(),
                        expected: nonnegative_u64(preparation.lifecycle_revision),
                        actual: nonnegative_u64(campaign.lifecycle_revision),
                    });
                }
                require_external_cleanup_complete(&store, session, &campaign.id).await?;
                let prepared_export: CampaignPrivateExportV1 =
                    serde_json::from_str(&preparation.canonical_export_json).map_err(|_| {
                        PersistenceError::SchemaDrift {
                            collection: "deletion_preparations".to_owned(),
                            detail: "stored canonical campaign export is invalid".to_owned(),
                        }
                    })?;
                let current_documents =
                    load_export_documents(&store, session, &campaign.id).await?;
                let mut current_export = export_from_snapshot(
                    &owner,
                    ExportSnapshot {
                        campaign: campaign.clone(),
                        documents: current_documents,
                    },
                )
                .map_err(repository_to_persistence)?;
                current_export.exported_at = prepared_export.exported_at;
                let current_digest = current_export
                    .canonical_digest()
                    .map_err(repository_to_persistence)?;
                if current_digest != preparation.digest {
                    return Err(PersistenceError::AlreadyExists {
                        entity: "stale_campaign_deletion_preparation",
                        id: command.deletion_id.clone(),
                    });
                }
                let result_revision =
                    next_revision(campaign.lifecycle_revision, "lifecycle revision")?;
                let outcome = CampaignLifecycleOutcome {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: campaign.id.clone(),
                    lifecycle_revision: nonnegative_u64(result_revision),
                    lifecycle_state: None,
                    play_session_id: None,
                    deleted: true,
                };
                insert_lifecycle_receipt(
                    &receipts,
                    session,
                    &owner,
                    &command.lifecycle,
                    "campaign_delete",
                    fingerprint,
                    &outcome,
                    true,
                )
                .await?;
                let now = DateTime::now();
                tombstones
                    .insert_one(DeletionTombstoneDocument {
                        id: format!("deletion-tombstone:{}", Uuid::new_v4()),
                        schema_version: 1,
                        entity_kind: "campaign".to_owned(),
                        entity_id: campaign.id.clone(),
                        deletion_id: command.deletion_id.clone(),
                        digest: preparation.digest,
                        owner_account_id: owner.clone(),
                        deleted_lifecycle_revision: result_revision,
                        deleted_at: now,
                        purge_at: add_seconds(now, DELETION_TOMBSTONE_SECONDS),
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("write campaign deletion tombstone", error)
                    })?;
                cascade_campaign_documents(
                    &store,
                    session,
                    &campaign.id,
                    &command.lifecycle.idempotency_key,
                )
                .await?;
                let deleted = campaigns
                    .delete_one(doc! {
                        "_id": &campaign.id,
                        "owner_account_id": &owner,
                        "lifecycle_revision": campaign.lifecycle_revision,
                        "lifecycle.state": "archived",
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("delete campaign root", error))?;
                if deleted.deleted_count != 1 {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "campaign_lifecycle",
                        id: campaign.id,
                        expected: command.lifecycle.expected_lifecycle_revision,
                        actual: nonnegative_u64(campaign.lifecycle_revision),
                    });
                }
                Ok(outcome)
            })
        })
        .await
        .map_err(|error| map_transaction_error(error, "campaign deletion", &campaign_id))
    }

    pub async fn delete_expired_campaign_lifecycle_metadata(
        &self,
        limit: u32,
    ) -> Result<u64, RepositoryError> {
        if limit == 0 || limit > 1_000 {
            return invalid(
                "campaign lifecycle cleanup",
                &limit.to_string(),
                "limit is outside the supported range",
            );
        }
        let now = DateTime::now();
        let mut deleted = 0_u64;
        for collection in [
            CollectionName::DeletionPreparations,
            CollectionName::DeletionTombstones,
        ] {
            let documents = self.store().document_collection(collection);
            let mut cursor = documents
                .find(doc! { "purge_at": { "$lte": now } })
                .projection(doc! { "_id": 1 })
                .sort(doc! { "purge_at": 1, "_id": 1 })
                .limit(i64::from(limit))
                .await
                .map_err(|error| mongo_error("find expired lifecycle metadata", error))?;
            let mut ids = Vec::new();
            while cursor
                .advance()
                .await
                .map_err(|error| mongo_error("read expired lifecycle metadata", error))?
            {
                let document = cursor
                    .deserialize_current()
                    .map_err(|error| mongo_error("decode expired lifecycle metadata", error))?;
                if let Ok(id) = document.get_str("_id") {
                    ids.push(id.to_owned());
                }
            }
            if !ids.is_empty() {
                deleted = deleted.saturating_add(
                    documents
                        .delete_many(doc! { "_id": { "$in": ids } })
                        .await
                        .map_err(|error| mongo_error("delete expired lifecycle metadata", error))?
                        .deleted_count,
                );
            }
        }
        Ok(deleted)
    }

    pub async fn restore_campaign_export(
        &self,
        owner_key: &str,
        command: &RestoreCampaignExportCommand,
    ) -> Result<CampaignLifecycleOutcome, RepositoryError> {
        validate_owner(owner_key)?;
        if command.schema_version != CAMPAIGN_LIFECYCLE_SCHEMA_VERSION
            || !is_valid_opaque_id(&command.idempotency_key)
            || command.canonical_export_json.is_empty()
            || command.canonical_export_json.len() > MAX_PLAYER_EXPORT_BYTES
        {
            return invalid(
                "campaign export restore",
                &command.idempotency_key,
                "restore command is invalid or exceeds the size limit",
            );
        }
        let export: CampaignPrivateExportV1 = serde_json::from_str(&command.canonical_export_json)
            .map_err(|source| RepositoryError::InvalidStoredData {
                entity: "campaign private export",
                id: command.idempotency_key.clone(),
                source,
            })?;
        export.validate()?;
        if export.owner_key != owner_key
            || export.canonical_json()? != command.canonical_export_json
        {
            return invalid(
                "campaign export restore",
                &command.idempotency_key,
                "export owner or canonical representation does not match",
            );
        }
        let campaign: CampaignDocument = mongodb::bson::from_document(export.campaign.clone())?;
        let campaign_id = campaign.id.clone();
        let fingerprint = command_fingerprint("campaign_restore_import", command)?;
        if let Some(replay) = self
            .load_retained_lifecycle_replay(
                owner_key,
                &campaign_id,
                &command.idempotency_key,
                "campaign_restore_import",
                &fingerprint,
            )
            .await?
        {
            return Ok(replay);
        }
        if self
            .campaigns()
            .find_one(doc! { "_id": &campaign_id })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|error| mongo_error("check campaign restore target", error))?
            .is_some()
        {
            return Err(RepositoryError::AlreadyExists {
                entity: "campaign_session",
                id: campaign_id,
            });
        }
        let store = self.store().clone();
        let receipts = self
            .store()
            .collection::<LifecycleReceiptDocument>(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let campaign_collection = self.campaigns();
        let owner = owner_key.to_owned();
        let command_owned = command.clone();
        let export_owned = export.clone();
        let campaign_for_insert = campaign.clone();
        let campaign_id_for_error = campaign.id.clone();
        let closed_play_session_ids = export
            .play_sessions
            .iter()
            .filter(|document| matches!(document.get_str("state"), Ok("waiting" | "active")))
            .filter_map(|document| document.get_str("_id").ok().map(str::to_owned))
            .collect::<Vec<_>>();
        self.with_transaction(move |session| {
            let store = store.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let campaign_collection = campaign_collection.clone();
            let owner = owner.clone();
            let command = command_owned.clone();
            let export = export_owned.clone();
            let campaign = campaign_for_insert.clone();
            let fingerprint = fingerprint.clone();
            let closed_play_session_ids = closed_play_session_ids.clone();
            Box::pin(async move {
                if let Some(replay) = load_lifecycle_replay(
                    &receipts,
                    session,
                    &owner,
                    &campaign.id,
                    &command.idempotency_key,
                    "campaign_restore_import",
                    &fingerprint,
                )
                .await?
                {
                    return Ok(replay);
                }
                if campaign_collection
                    .find_one(doc! { "_id": &campaign.id })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check campaign restore transaction", error)
                    })?
                    .is_some()
                {
                    return Err(PersistenceError::AlreadyExists {
                        entity: "campaign_session",
                        id: campaign.id,
                    });
                }
                restore_export_documents(&store, session, &export).await?;
                let mut restored_campaign = campaign;
                restored_campaign.current_play_session_id = None;
                restored_campaign.lifecycle_revision =
                    next_revision(restored_campaign.lifecycle_revision, "lifecycle revision")?;
                restored_campaign.updated_at = DateTime::now();
                campaign_collection
                    .insert_one(restored_campaign.clone())
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("restore campaign root", error))?;
                let restored_state = match restored_campaign.lifecycle.state.as_str() {
                    "open" => CampaignLifecycleState::Active,
                    "archived" => CampaignLifecycleState::Archived,
                    _ => {
                        return Err(PersistenceError::SchemaDrift {
                            collection: "campaigns".to_owned(),
                            detail: "restored campaign lifecycle is invalid".to_owned(),
                        });
                    }
                };
                let outcome = CampaignLifecycleOutcome {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: restored_campaign.id.clone(),
                    lifecycle_revision: nonnegative_u64(restored_campaign.lifecycle_revision),
                    lifecycle_state: Some(restored_state),
                    play_session_id: None,
                    deleted: false,
                };
                insert_lifecycle_audit(
                    &audits,
                    session,
                    &owner,
                    &outcome,
                    LifecycleAuditPayload::RestoreImported {
                        closed_play_session_ids,
                    },
                )
                .await?;
                insert_lifecycle_receipt(
                    &receipts,
                    session,
                    &owner,
                    &CampaignLifecycleCommand {
                        schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                        campaign_session_id: restored_campaign.id,
                        expected_lifecycle_revision: nonnegative_u64(
                            restored_campaign.lifecycle_revision.saturating_sub(1),
                        ),
                        idempotency_key: command.idempotency_key,
                    },
                    "campaign_restore_import",
                    fingerprint,
                    &outcome,
                    true,
                )
                .await?;
                Ok(outcome)
            })
        })
        .await
        .map_err(|error| {
            map_transaction_error(error, "campaign export restore", &campaign_id_for_error)
        })
    }

    async fn load_retained_lifecycle_replay(
        &self,
        owner_key: &str,
        campaign_id: &str,
        idempotency_key: &str,
        command_kind: &'static str,
        fingerprint: &Sha256Digest,
    ) -> Result<Option<CampaignLifecycleOutcome>, RepositoryError> {
        let receipt = self
            .store()
            .collection::<LifecycleReceiptDocument>(CollectionName::CommandReceipts)
            .find_one(doc! {
                "scope_kind": "campaign",
                "scope_id": campaign_id,
                "actor_account_id": owner_key,
                "idempotency_key": idempotency_key,
                "state": "committed",
            })
            .await
            .map_err(|error| mongo_error("load retained lifecycle receipt", error))?;
        receipt
            .map(|receipt| decode_lifecycle_replay(receipt, command_kind, fingerprint))
            .transpose()
    }
}

pub(crate) fn campaign_summary_from_document(
    campaign: CampaignDocument,
) -> Result<CampaignSummary, RepositoryError> {
    if campaign.schema_version != 1
        || campaign.revision <= 0
        || campaign.lifecycle_revision <= 0
        || campaign.session.id != campaign.id
        || campaign.session.title != campaign.title
    {
        return invalid(
            "campaign session",
            &campaign.id,
            "stored campaign envelope is inconsistent",
        );
    }
    campaign
        .session
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "campaign session",
            id: campaign.id.clone(),
            source,
        })?;
    Ok(CampaignSummary {
        schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
        campaign_session_id: campaign.id.clone(),
        owner_key: campaign.owner_account_id.clone(),
        title: campaign.title,
        campaign_revision: nonnegative_u64(campaign.revision),
        lifecycle_revision: nonnegative_u64(campaign.lifecycle_revision),
        lifecycle_state: CampaignLifecycleState::from_storage(
            &campaign.lifecycle.state,
            &campaign.id,
        )?,
        archived_at: campaign
            .lifecycle
            .archived_at
            .map(|value| date_string(value, "campaigns"))
            .transpose()?,
        safety_policy_id: campaign.safety_policy_id,
        progression_policy_id: campaign.progression_policy_id,
        retention_class: campaign.retention_class,
        retention_delete_after: campaign
            .retention_delete_after
            .map(|value| date_string(value, "campaigns"))
            .transpose()?,
        open_play_session_id: campaign.current_play_session_id,
        created_at: date_string(campaign.created_at, "campaigns")?,
        updated_at: date_string(campaign.updated_at, "campaigns")?,
    })
}

fn prepared_deletion_from_document(
    document: DeletionPreparationDocument,
) -> Result<PreparedCampaignDeletion, RepositoryError> {
    if document.schema_version != 1
        || document.scope_kind != "campaign"
        || document.campaign_revision <= 0
        || document.lifecycle_revision <= 0
        || document.canonical_export_json.len() > MAX_PLAYER_EXPORT_BYTES
        || digest(document.canonical_export_json.as_bytes()) != document.digest
    {
        return invalid(
            "campaign deletion preparation",
            &document.deletion_id,
            "stored deletion preparation is invalid",
        );
    }
    Ok(PreparedCampaignDeletion {
        schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
        campaign_session_id: document.scope_id,
        deletion_id: document.deletion_id,
        campaign_revision: nonnegative_u64(document.campaign_revision),
        lifecycle_revision: nonnegative_u64(document.lifecycle_revision),
        canonical_export_digest: document.digest,
        canonical_export_json: document.canonical_export_json,
        expires_at: date_string(document.expires_at, "deletion_preparations")?,
    })
}

fn export_collection_specs(
    campaign_id: &str,
) -> Vec<(&'static str, CollectionName, Document, Document)> {
    vec![
        (
            "character_instances",
            CollectionName::CampaignCharacterInstances,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "play_sessions",
            CollectionName::PlaySessions,
            doc! { "campaign_id": campaign_id },
            doc! { "opened_at": 1, "_id": 1 },
        ),
        (
            "turn_events",
            CollectionName::TurnEvents,
            doc! { "campaign_id": campaign_id },
            doc! { "sequence": 1, "_id": 1 },
        ),
        (
            "command_receipts",
            CollectionName::CommandReceipts,
            doc! {
                "state": "committed",
                "$or": [
                    { "campaign_id": campaign_id },
                    { "scope_kind": "campaign", "scope_id": campaign_id },
                ],
            },
            doc! { "created_at": 1, "_id": 1 },
        ),
        (
            "audit_events",
            CollectionName::AuditEvents,
            doc! { "$or": [{ "campaign_id": campaign_id }, { "scope_kind": "campaign", "scope_id": campaign_id }] },
            doc! { "created_at": 1, "_id": 1 },
        ),
        (
            "campaign_enemy_instances",
            CollectionName::CampaignEnemyInstances,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "campaign_events",
            CollectionName::CampaignEvents,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "encounters",
            CollectionName::Encounters,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "bde_ledger",
            CollectionName::BdeLedger,
            doc! { "campaign_id": campaign_id },
            doc! { "created_at": 1, "_id": 1 },
        ),
        (
            "private_inspiration_participants",
            CollectionName::PrivateInspirationParticipants,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "private_inspiration_sources",
            CollectionName::PrivateInspirationSources,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "private_inspiration_consents",
            CollectionName::PrivateInspirationConsents,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "private_inspiration_vetoes",
            CollectionName::PrivateInspirationVetoes,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "private_inspiration_selections",
            CollectionName::PrivateInspirationSelections,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "private_inspiration_work",
            CollectionName::PrivateInspirationWork,
            doc! { "campaign_id": campaign_id },
            doc! { "_id": 1 },
        ),
        (
            "selected_generated_presentations",
            CollectionName::GeneratedPresentations,
            doc! { "campaign_id": campaign_id, "selected": true },
            doc! { "created_at": 1, "_id": 1 },
        ),
        (
            "selected_generated_assets",
            CollectionName::GeneratedAssets,
            doc! { "campaign_id": campaign_id, "state": { "$in": ["published", "selected", "ready"] } },
            doc! { "created_at": 1, "_id": 1 },
        ),
    ]
}

async fn find_documents(
    collection: &Collection<Document>,
    session: &mut ClientSession,
    filter: Document,
    sort: Document,
    operation: &'static str,
) -> Result<Vec<Document>, PersistenceError> {
    let mut cursor = collection
        .find(filter)
        .sort(sort)
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo(operation, error))?;
    let mut documents = Vec::new();
    while let Some(document) = cursor
        .next(&mut *session)
        .await
        .transpose()
        .map_err(|error| PersistenceError::mongo(operation, error))?
    {
        documents.push(document);
    }
    Ok(documents)
}

async fn load_export_documents(
    store: &crate::persistence::MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<BTreeMap<&'static str, Vec<Document>>, PersistenceError> {
    let mut documents = BTreeMap::new();
    for (key, collection, filter, sort) in export_collection_specs(campaign_id) {
        documents.insert(
            key,
            find_documents(
                &store.document_collection(collection),
                session,
                filter,
                sort,
                "read campaign export collection",
            )
            .await?,
        );
    }
    Ok(documents)
}

fn export_from_snapshot(
    owner_key: &str,
    snapshot: ExportSnapshot,
) -> Result<CampaignPrivateExportV1, RepositoryError> {
    let campaign = mongodb::bson::to_document(&snapshot.campaign)?;
    let mut documents = snapshot.documents;
    Ok(CampaignPrivateExportV1 {
        schema_version: CAMPAIGN_EXPORT_SCHEMA_VERSION,
        owner_key: owner_key.to_owned(),
        exported_at: date_string(DateTime::now(), "campaign export")?,
        campaign,
        character_instances: documents.remove("character_instances").unwrap_or_default(),
        play_sessions: documents.remove("play_sessions").unwrap_or_default(),
        turn_events: documents.remove("turn_events").unwrap_or_default(),
        command_receipts: documents.remove("command_receipts").unwrap_or_default(),
        audit_events: documents.remove("audit_events").unwrap_or_default(),
        campaign_enemy_instances: documents
            .remove("campaign_enemy_instances")
            .unwrap_or_default(),
        campaign_events: documents.remove("campaign_events").unwrap_or_default(),
        encounters: documents.remove("encounters").unwrap_or_default(),
        bde_ledger: documents.remove("bde_ledger").unwrap_or_default(),
        private_inspiration_participants: documents
            .remove("private_inspiration_participants")
            .unwrap_or_default(),
        private_inspiration_sources: documents
            .remove("private_inspiration_sources")
            .unwrap_or_default(),
        private_inspiration_consents: documents
            .remove("private_inspiration_consents")
            .unwrap_or_default(),
        private_inspiration_vetoes: documents
            .remove("private_inspiration_vetoes")
            .unwrap_or_default(),
        private_inspiration_selections: documents
            .remove("private_inspiration_selections")
            .unwrap_or_default(),
        private_inspiration_work: documents
            .remove("private_inspiration_work")
            .unwrap_or_default(),
        selected_generated_presentations: documents
            .remove("selected_generated_presentations")
            .unwrap_or_default(),
        selected_generated_assets: documents
            .remove("selected_generated_assets")
            .unwrap_or_default(),
    })
}

fn validate_export(export: &CampaignPrivateExportV1) -> Result<(), RepositoryError> {
    if export.schema_version != CAMPAIGN_EXPORT_SCHEMA_VERSION
        || !is_valid_opaque_id(&export.owner_key)
        || DateTime::parse_rfc3339_str(&export.exported_at).is_err()
    {
        return invalid(
            "campaign private export",
            &export.owner_key,
            "schema, owner, or export timestamp is invalid",
        );
    }
    let campaign: CampaignDocument = mongodb::bson::from_document(export.campaign.clone())
        .map_err(RepositoryError::BsonDecoding)?;
    let member_ids = campaign
        .members
        .iter()
        .map(|member| member.account_id.as_str())
        .collect::<BTreeSet<_>>();
    if campaign.owner_account_id != export.owner_key
        || campaign.members.len() > 16
        || member_ids.len() != campaign.members.len()
        || campaign.members.iter().any(|member| {
            !matches!(member.role.as_str(), "game_master" | "player")
                || !matches!(
                    member.state.as_str(),
                    "invited" | "active" | "left" | "removed"
                )
        })
        || campaign.active_game_master(&export.owner_key).is_none()
    {
        return invalid(
            "campaign private export",
            &campaign.id,
            "campaign ownership or bounded membership is invalid",
        );
    }
    campaign_summary_from_document(campaign.clone())?;
    let groups = export_document_groups(export);
    let document_count = groups.iter().fold(0_usize, |total, (_, documents)| {
        total.saturating_add(documents.len())
    });
    if document_count > MAX_EXPORTED_DOCUMENTS {
        return invalid(
            "campaign private export",
            &campaign.id,
            "export document count exceeds the supported bound",
        );
    }
    reject_private_keys(&export.campaign, &campaign.id)?;
    for (name, documents) in groups {
        let mut ids = BTreeSet::new();
        for document in documents {
            let id = document
                .get_str("_id")
                .map_err(|_| RepositoryError::InvalidDomainState {
                    entity: "campaign private export",
                    id: campaign.id.clone(),
                    reason: "exported document is missing a string id",
                })?;
            if !is_valid_opaque_id(id) || !ids.insert(id.to_owned()) {
                return invalid(
                    "campaign private export",
                    id,
                    "exported document id is invalid or duplicated",
                );
            }
            if !matches!(
                document.get("schema_version"),
                Some(Bson::Int32(1) | Bson::Int64(1))
            ) {
                return invalid(
                    "campaign private export",
                    id,
                    "exported document schema version is unsupported",
                );
            }
            let scoped = document.get_str("campaign_id") == Ok(campaign.id.as_str())
                || (document.get_str("scope_kind") == Ok("campaign")
                    && document.get_str("scope_id") == Ok(campaign.id.as_str()));
            if !scoped {
                return invalid(
                    "campaign private export",
                    id,
                    "exported document is outside the campaign scope",
                );
            }
            reject_private_keys(document, id)?;
            if name == "selected_generated_presentations"
                && document.get_bool("selected") != Ok(true)
            {
                return invalid(
                    "campaign private export",
                    id,
                    "only selected presentations may be exported",
                );
            }
            if name == "character_instances" {
                validate_exported_character_instance(document, &campaign.id)?;
            } else if name == "play_sessions" {
                let play: PlaySessionDocument = mongodb::bson::from_document(document.clone())
                    .map_err(RepositoryError::BsonDecoding)?;
                play_session_from_document(play)?;
            } else if name == "turn_events" {
                let turn: TurnEventDocument = mongodb::bson::from_document(document.clone())
                    .map_err(RepositoryError::BsonDecoding)?;
                turn_history_from_document(turn)?;
            }
        }
    }
    if canonical_json_unchecked(export)?.len() > MAX_PLAYER_EXPORT_BYTES {
        return invalid(
            "campaign private export",
            &campaign.id,
            "canonical export exceeds the supported size",
        );
    }
    Ok(())
}

fn validate_exported_character_instance(
    document: &Document,
    campaign_id: &str,
) -> Result<(), RepositoryError> {
    let instance: CampaignCharacterInstanceDocument =
        mongodb::bson::from_document(document.clone()).map_err(RepositoryError::BsonDecoding)?;
    let source = &instance.source_snapshot.player_character;
    let hero = &instance.runtime.hero;
    let encoded = serde_json::to_vec(source).map_err(|source| RepositoryError::Serialize {
        entity: "campaign character source snapshot",
        source,
    })?;
    let expected_digest = format!("sha256:{:x}", Sha256::digest(encoded));
    if instance.schema_version != 1
        || instance.revision <= 0
        || !matches!(instance.state.as_str(), "active" | "retired")
        || instance.campaign_id != campaign_id
        || instance.account_id != source.owner_account_id
        || instance.source_player_character_id != source.character_id
        || instance.source_snapshot.source_revision
            != i64::try_from(source.revision).map_err(|_| RepositoryError::NumericRange {
                field: "source character revision",
            })?
        || instance.source_snapshot.source_schema_version != i64::from(source.schema_version)
        || instance.source_snapshot.source_digest != expected_digest
        || instance.source_snapshot.display_name != source.display_name
        || hero.campaign_id != campaign_id
        || hero.owner_id != instance.account_id
        || hero.character_id != instance.runtime_hero_character_id
        || instance.progression.level != i64::from(hero.level.value())
        || instance.progression.experience_points != i64::from(hero.experience_points)
        || instance.progression.milestone_count < 0
        || instance.runtime.current_hit_points != i64::from(hero.sheet.current_hit_points)
        || instance.runtime.maximum_hit_points != i64::from(hero.sheet.maximum_hit_points)
        || instance.runtime.current_hit_points < 0
        || instance.runtime.current_hit_points > instance.runtime.maximum_hit_points
        || instance.runtime.temporary_hit_points < 0
    {
        return invalid(
            "campaign private export character instance",
            &instance.id,
            "source snapshot, progression, or runtime boundary is inconsistent",
        );
    }
    source
        .validate()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign private export character instance",
            id: instance.id.clone(),
            reason: "source player character failed validation",
        })?;
    hero.validate()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign private export character instance",
            id: instance.id,
            reason: "runtime hero character failed validation",
        })?;
    Ok(())
}

fn export_document_groups(export: &CampaignPrivateExportV1) -> Vec<(&'static str, &[Document])> {
    vec![
        ("character_instances", &export.character_instances),
        ("play_sessions", &export.play_sessions),
        ("turn_events", &export.turn_events),
        ("command_receipts", &export.command_receipts),
        ("audit_events", &export.audit_events),
        ("campaign_enemy_instances", &export.campaign_enemy_instances),
        ("campaign_events", &export.campaign_events),
        ("encounters", &export.encounters),
        ("bde_ledger", &export.bde_ledger),
        (
            "private_inspiration_participants",
            &export.private_inspiration_participants,
        ),
        (
            "private_inspiration_sources",
            &export.private_inspiration_sources,
        ),
        (
            "private_inspiration_consents",
            &export.private_inspiration_consents,
        ),
        (
            "private_inspiration_vetoes",
            &export.private_inspiration_vetoes,
        ),
        (
            "private_inspiration_selections",
            &export.private_inspiration_selections,
        ),
        ("private_inspiration_work", &export.private_inspiration_work),
        (
            "selected_generated_presentations",
            &export.selected_generated_presentations,
        ),
        (
            "selected_generated_assets",
            &export.selected_generated_assets,
        ),
    ]
}

fn reject_private_keys(document: &Document, id: &str) -> Result<(), RepositoryError> {
    for (key, value) in document {
        let normalized = key.to_ascii_lowercase();
        if normalized.contains("password")
            || normalized.contains("email_cipher")
            || normalized.contains("session_token")
            || normalized.contains("access_token")
            || normalized.contains("refresh_token")
            || normalized.contains("throttle")
            || normalized.contains("private_key")
            || normalized.contains("secret")
            || normalized.contains("prompt_text")
        {
            return invalid(
                "campaign private export",
                id,
                "export contains a prohibited private field",
            );
        }
        reject_private_value(value, id)?;
    }
    Ok(())
}

fn reject_private_value(value: &Bson, id: &str) -> Result<(), RepositoryError> {
    match value {
        Bson::Document(nested) => reject_private_keys(nested, id),
        Bson::Array(values) => {
            for value in values {
                reject_private_value(value, id)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

async fn restore_export_documents(
    store: &crate::persistence::MongoStore,
    session: &mut ClientSession,
    export: &CampaignPrivateExportV1,
) -> Result<(), PersistenceError> {
    for (collection, documents) in restore_groups(export) {
        if documents.is_empty() {
            continue;
        }
        let mut restored = documents.to_vec();
        if collection == CollectionName::PlaySessions {
            let now = DateTime::now();
            for document in &mut restored {
                if matches!(document.get_str("state"), Ok("waiting" | "active")) {
                    document.insert("state", "closed");
                    document.insert("closed_at", now);
                    document.insert("updated_at", now);
                    document.insert("close_reason", "restore_import");
                }
            }
        }
        if collection == CollectionName::CommandReceipts {
            let receipts = store.document_collection(collection);
            for document in restored {
                let id = document
                    .get_str("_id")
                    .map_err(|_| PersistenceError::SchemaDrift {
                        collection: "command_receipts".to_owned(),
                        detail: "restored receipt is missing its string id".to_owned(),
                    })?;
                receipts
                    .replace_one(doc! { "_id": id }, document.clone())
                    .upsert(true)
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("restore campaign receipt", error))?;
            }
            continue;
        }
        store
            .document_collection(collection)
            .insert_many(restored)
            .session(&mut *session)
            .await
            .map_err(|error| PersistenceError::mongo("restore campaign collection", error))?;
    }
    Ok(())
}

fn restore_groups(export: &CampaignPrivateExportV1) -> Vec<(CollectionName, &[Document])> {
    vec![
        (
            CollectionName::CampaignCharacterInstances,
            &export.character_instances,
        ),
        (CollectionName::PlaySessions, &export.play_sessions),
        (CollectionName::TurnEvents, &export.turn_events),
        (CollectionName::CommandReceipts, &export.command_receipts),
        (CollectionName::AuditEvents, &export.audit_events),
        (
            CollectionName::CampaignEnemyInstances,
            &export.campaign_enemy_instances,
        ),
        (CollectionName::CampaignEvents, &export.campaign_events),
        (CollectionName::Encounters, &export.encounters),
        (CollectionName::BdeLedger, &export.bde_ledger),
        (
            CollectionName::PrivateInspirationParticipants,
            &export.private_inspiration_participants,
        ),
        (
            CollectionName::PrivateInspirationSources,
            &export.private_inspiration_sources,
        ),
        (
            CollectionName::PrivateInspirationConsents,
            &export.private_inspiration_consents,
        ),
        (
            CollectionName::PrivateInspirationVetoes,
            &export.private_inspiration_vetoes,
        ),
        (
            CollectionName::PrivateInspirationSelections,
            &export.private_inspiration_selections,
        ),
        (
            CollectionName::PrivateInspirationWork,
            &export.private_inspiration_work,
        ),
        (
            CollectionName::GeneratedPresentations,
            &export.selected_generated_presentations,
        ),
        (
            CollectionName::GeneratedAssets,
            &export.selected_generated_assets,
        ),
    ]
}

async fn cascade_campaign_documents(
    store: &crate::persistence::MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
    retained_receipt_key: &str,
) -> Result<(), PersistenceError> {
    let generation_jobs = store.document_collection(CollectionName::GenerationJobs);
    let mut job_cursor = generation_jobs
        .find(doc! { "campaign_id": campaign_id })
        .projection(doc! { "_id": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("load campaign generation jobs for deletion", error)
        })?;
    let mut job_ids = Vec::new();
    while let Some(document) =
        job_cursor
            .next(&mut *session)
            .await
            .transpose()
            .map_err(|error| {
                PersistenceError::mongo("read campaign generation jobs for deletion", error)
            })?
    {
        if let Ok(id) = document.get_str("_id") {
            job_ids.push(id.to_owned());
        }
    }
    if !job_ids.is_empty() {
        store
            .document_collection(CollectionName::QuarantinedAssets)
            .delete_many(doc! { "job_id": { "$in": &job_ids } })
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("delete campaign quarantined assets", error)
            })?;
    }
    for collection in [
        CollectionName::CampaignInvitations,
        CollectionName::CampaignCharacterInstances,
        CollectionName::CampaignEnemyInstances,
        CollectionName::CampaignEvents,
        CollectionName::PlaySessions,
        CollectionName::Encounters,
        CollectionName::TurnEvents,
        CollectionName::BdeLedger,
        CollectionName::GenerationJobs,
        CollectionName::GeneratedPresentations,
        CollectionName::GeneratedAssets,
        CollectionName::PrivateInspirationParticipants,
        CollectionName::PrivateInspirationSources,
        CollectionName::PrivateInspirationConsents,
        CollectionName::PrivateInspirationVetoes,
        CollectionName::PrivateInspirationSelections,
        CollectionName::PrivateInspirationWork,
    ] {
        store
            .document_collection(collection)
            .delete_many(doc! { "campaign_id": campaign_id })
            .session(&mut *session)
            .await
            .map_err(|error| PersistenceError::mongo("cascade campaign deletion", error))?;
    }
    store
        .document_collection(CollectionName::GenerationBudgetReservations)
        .delete_many(doc! {
            "$or": [
                { "scope_kind": "campaign", "scope_id": campaign_id },
                { "job_id": { "$in": &job_ids } },
            ]
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("delete campaign reservations", error))?;
    store
        .document_collection(CollectionName::DeletionPreparations)
        .delete_many(doc! {
            "scope_kind": "campaign",
            "scope_id": campaign_id,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("delete campaign deletion preparations", error))?;
    store
        .document_collection(CollectionName::AuditEvents)
        .delete_many(doc! {
            "$or": [
                { "campaign_id": campaign_id },
                { "scope_kind": "campaign", "scope_id": campaign_id },
            ]
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("delete campaign audits", error))?;
    store
        .document_collection(CollectionName::CommandReceipts)
        .delete_many(doc! {
            "scope_kind": "campaign",
            "scope_id": campaign_id,
            "idempotency_key": { "$ne": retained_receipt_key },
            "retain_after_delete": { "$ne": true },
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("delete campaign receipts", error))?;
    Ok(())
}

async fn load_owned_gm_campaign(
    campaigns: &Collection<CampaignDocument>,
    session: &mut ClientSession,
    owner_key: &str,
    campaign_id: &str,
) -> Result<CampaignDocument, PersistenceError> {
    campaigns
        .find_one(doc! {
            "_id": campaign_id,
            "owner_account_id": owner_key,
            "members": {
                "$elemMatch": {
                    "account_id": owner_key,
                    "role": "game_master",
                    "state": "active",
                }
            },
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("authorize campaign lifecycle", error))?
        .ok_or_else(|| PersistenceError::NotFound {
            entity: "campaign_session",
            id: campaign_id.to_owned(),
        })
}

async fn require_owned_campaign(
    repository: &MongoRepository,
    owner_key: &str,
    campaign_id: &str,
) -> Result<CampaignDocument, RepositoryError> {
    repository
        .campaigns()
        .find_one(doc! {
            "_id": campaign_id,
            "owner_account_id": owner_key,
            "members": {
                "$elemMatch": {
                    "account_id": owner_key,
                    "role": "game_master",
                    "state": "active",
                }
            },
        })
        .await
        .map_err(|error| mongo_error("authorize owned campaign", error))?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign_session",
            id: campaign_id.to_owned(),
        })
}

async fn require_no_open_campaign_runtime(
    store: &crate::persistence::MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<(), PersistenceError> {
    let open_play = store
        .document_collection(CollectionName::PlaySessions)
        .find_one(doc! {
            "campaign_id": campaign_id,
            "state": { "$in": ["waiting", "active"] },
        })
        .projection(doc! { "_id": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("check open campaign play sessions", error))?;
    if open_play.is_some() {
        return Err(PersistenceError::AlreadyExists {
            entity: "campaign_play_session",
            id: campaign_id.to_owned(),
        });
    }
    let active_encounter = store
        .document_collection(CollectionName::Encounters)
        .find_one(doc! {
            "campaign_id": campaign_id,
            "status": "active",
        })
        .projection(doc! { "_id": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("check active campaign encounters", error))?;
    if active_encounter.is_some() {
        return Err(PersistenceError::AlreadyExists {
            entity: "campaign_encounter",
            id: campaign_id.to_owned(),
        });
    }
    Ok(())
}

async fn require_external_cleanup_complete(
    store: &crate::persistence::MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<(), PersistenceError> {
    let pending_asset = store
        .document_collection(CollectionName::GeneratedAssets)
        .find_one(doc! {
            "campaign_id": campaign_id,
            "object_key": { "$type": "string" },
            "state": { "$nin": ["erased", "deleted"] },
        })
        .projection(doc! { "_id": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("check campaign external asset cleanup", error))?;
    if pending_asset.is_some() {
        return Err(PersistenceError::AlreadyExists {
            entity: "campaign_external_asset_cleanup",
            id: campaign_id.to_owned(),
        });
    }

    let generation_jobs = store.document_collection(CollectionName::GenerationJobs);
    let mut job_cursor = generation_jobs
        .find(doc! { "campaign_id": campaign_id })
        .projection(doc! { "_id": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("load campaign jobs for external cleanup check", error)
        })?;
    let mut job_ids = Vec::new();
    while let Some(document) =
        job_cursor
            .next(&mut *session)
            .await
            .transpose()
            .map_err(|error| {
                PersistenceError::mongo("read campaign jobs for external cleanup check", error)
            })?
    {
        if let Ok(id) = document.get_str("_id") {
            job_ids.push(id.to_owned());
        }
    }
    if job_ids.is_empty() {
        return Ok(());
    }
    let pending_quarantine = store
        .document_collection(CollectionName::QuarantinedAssets)
        .find_one(doc! {
            "job_id": { "$in": job_ids },
            "$or": [
                { "object_key": { "$type": "string" } },
                { "storage_key": { "$type": "string" } },
            ],
        })
        .projection(doc! { "_id": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("check campaign quarantined asset cleanup", error)
        })?;
    if pending_quarantine.is_some() {
        return Err(PersistenceError::AlreadyExists {
            entity: "campaign_external_asset_cleanup",
            id: campaign_id.to_owned(),
        });
    }
    Ok(())
}

fn require_lifecycle_revision(
    campaign: &CampaignDocument,
    expected: u64,
) -> Result<(), PersistenceError> {
    let actual = nonnegative_u64(campaign.lifecycle_revision);
    if actual != expected {
        return Err(PersistenceError::RevisionConflict {
            entity: "campaign_lifecycle",
            id: campaign.id.clone(),
            expected,
            actual,
        });
    }
    Ok(())
}

async fn load_lifecycle_replay(
    receipts: &Collection<LifecycleReceiptDocument>,
    session: &mut ClientSession,
    owner_key: &str,
    campaign_id: &str,
    idempotency_key: &str,
    command_kind: &'static str,
    fingerprint: &Sha256Digest,
) -> Result<Option<CampaignLifecycleOutcome>, PersistenceError> {
    receipts
        .find_one(doc! {
            "scope_kind": "campaign",
            "scope_id": campaign_id,
            "actor_account_id": owner_key,
            "idempotency_key": idempotency_key,
            "state": "committed",
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load lifecycle receipt", error))?
        .map(|receipt| {
            decode_lifecycle_replay(receipt, command_kind, fingerprint)
                .map_err(repository_to_persistence)
        })
        .transpose()
}

fn decode_lifecycle_replay(
    receipt: LifecycleReceiptDocument,
    command_kind: &'static str,
    fingerprint: &Sha256Digest,
) -> Result<CampaignLifecycleOutcome, RepositoryError> {
    if receipt.command_kind != command_kind || receipt.request_fingerprint != *fingerprint {
        return Err(RepositoryError::AlreadyExists {
            entity: "campaign lifecycle idempotency key",
            id: receipt.idempotency_key,
        });
    }
    mongodb::bson::from_document(receipt.response).map_err(RepositoryError::BsonDecoding)
}

#[allow(clippy::too_many_arguments)]
async fn insert_lifecycle_receipt(
    receipts: &Collection<LifecycleReceiptDocument>,
    session: &mut ClientSession,
    owner_key: &str,
    command: &CampaignLifecycleCommand,
    command_kind: &'static str,
    fingerprint: Sha256Digest,
    outcome: &CampaignLifecycleOutcome,
    retain_after_delete: bool,
) -> Result<(), PersistenceError> {
    let response = mongodb::bson::to_document(outcome).map_err(PersistenceError::BsonEncoding)?;
    receipts
        .insert_one(LifecycleReceiptDocument {
            id: format!("command-receipt:{}", Uuid::new_v4()),
            schema_version: 1,
            scope_kind: "campaign".to_owned(),
            scope_id: command.campaign_session_id.clone(),
            campaign_id: command.campaign_session_id.clone(),
            actor_account_id: owner_key.to_owned(),
            command_kind: command_kind.to_owned(),
            idempotency_key: command.idempotency_key.clone(),
            request_fingerprint: fingerprint,
            expected_revision: to_i64_persistence(command.expected_lifecycle_revision)?,
            result_revision: to_i64_persistence(outcome.lifecycle_revision)?,
            response,
            state: "committed".to_owned(),
            retain_after_delete,
            created_at: DateTime::now(),
            purge_at: None,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("write lifecycle receipt", error))?;
    Ok(())
}

async fn insert_lifecycle_audit(
    audits: &Collection<Document>,
    session: &mut ClientSession,
    owner_key: &str,
    outcome: &CampaignLifecycleOutcome,
    payload: LifecycleAuditPayload,
) -> Result<(), PersistenceError> {
    let metadata = mongodb::bson::to_document(&payload).map_err(PersistenceError::BsonEncoding)?;
    audits
        .insert_one(doc! {
            "_id": format!("audit:{}", Uuid::new_v4()),
            "schema_version": 1_i64,
            "category": "campaign_lifecycle",
            "action": payload.event_kind(),
            "outcome": "committed",
            "scope_kind": "campaign",
            "scope_id": &outcome.campaign_session_id,
            "campaign_id": &outcome.campaign_session_id,
            "actor_account_id": owner_key,
            "revision": to_i64_persistence(outcome.lifecycle_revision)?,
            "metadata": metadata,
            "created_at": DateTime::now(),
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("write lifecycle audit", error))?;
    Ok(())
}

fn turn_history_from_document(
    document: TurnEventDocument,
) -> Result<CampaignTurnHistoryItem, RepositoryError> {
    if document.sequence <= 0
        || document.event.session_id != document.campaign_id
        || document.event.sequence != nonnegative_u64(document.sequence)
    {
        return invalid(
            "campaign turn event",
            &document.id,
            "event envelope does not match stored campaign sequence",
        );
    }
    document
        .event
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "campaign turn event",
            id: document.id.clone(),
            source,
        })?;
    Ok(CampaignTurnHistoryItem {
        schema_version: u32::try_from(document.schema_version).map_err(|_| {
            RepositoryError::NumericRange {
                field: "turn event schema version",
            }
        })?,
        id: document.id,
        campaign_session_id: document.campaign_id,
        turn_number: nonnegative_u64(document.sequence),
        actor_id: document.actor_account_id,
        correlation_id: document.correlation_id,
        event: document.event,
        created_at: date_string(document.created_at, "turn_events")?,
    })
}

fn play_session_from_document(
    document: PlaySessionDocument,
) -> Result<CampaignPlaySession, RepositoryError> {
    if !matches!(document.state.as_str(), "waiting" | "active" | "closed") {
        return invalid(
            "campaign play session",
            &document.id,
            "unknown play session state",
        );
    }
    Ok(CampaignPlaySession {
        schema_version: u16::try_from(document.schema_version).map_err(|_| {
            RepositoryError::NumericRange {
                field: "play session schema version",
            }
        })?,
        id: document.id,
        campaign_session_id: document.campaign_id,
        owner_key: document.gm_account_id,
        state: document.state,
        started_campaign_revision: nonnegative_u64(document.started_campaign_revision),
        ended_campaign_revision: document.ended_campaign_revision.map(nonnegative_u64),
        opened_at: date_string(document.opened_at, "play_sessions")?,
        closed_at: document
            .closed_at
            .map(|value| date_string(value, "play_sessions"))
            .transpose()?,
        close_reason: document.close_reason,
    })
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String, RepositoryError> {
    let encoded = canonical_json_unchecked(value)?;
    if encoded.len() > MAX_PLAYER_EXPORT_BYTES {
        return invalid(
            "canonical JSON",
            "campaign-export",
            "canonical value exceeds the supported size",
        );
    }
    Ok(encoded)
}

fn canonical_json_unchecked<T: Serialize>(value: &T) -> Result<String, RepositoryError> {
    let value = serde_json::to_value(value).map_err(|source| RepositoryError::Serialize {
        entity: "canonical JSON",
        source,
    })?;
    serde_json::to_string(&canonicalize_value(value)).map_err(|source| RepositoryError::Serialize {
        entity: "canonical JSON",
        source,
    })
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_value).collect()),
        scalar => scalar,
    }
}

fn command_fingerprint<T: Serialize>(
    command_kind: &str,
    command: &T,
) -> Result<Sha256Digest, RepositoryError> {
    let payload = canonical_json(command)?;
    Ok(digest(format!("{command_kind}\n{payload}").as_bytes()))
}

pub(crate) fn digest(bytes: &[u8]) -> Sha256Digest {
    let bytes: [u8; 32] = Sha256::digest(bytes).into();
    Sha256Digest::from_bytes(bytes)
}

fn validate_owner(owner_key: &str) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(owner_key) {
        return invalid(
            "campaign owner",
            owner_key,
            "owner account identifier is invalid",
        );
    }
    Ok(())
}

fn validate_owner_campaign(owner_key: &str, campaign_id: &str) -> Result<(), RepositoryError> {
    validate_owner(owner_key)?;
    if !is_valid_opaque_id(campaign_id) {
        return invalid(
            "campaign session",
            campaign_id,
            "campaign identifier is invalid",
        );
    }
    Ok(())
}

fn date_string(value: DateTime, collection: &str) -> Result<String, RepositoryError> {
    value.try_to_rfc3339_string().map_err(|_| {
        RepositoryError::Persistence(PersistenceError::SchemaDrift {
            collection: collection.to_owned(),
            detail: "stored BSON date is outside RFC 3339 range".to_owned(),
        })
    })
}

fn add_seconds(value: DateTime, seconds: i64) -> DateTime {
    DateTime::from_millis(
        value
            .timestamp_millis()
            .saturating_add(seconds.saturating_mul(1_000)),
    )
}

fn next_revision(value: i64, field: &'static str) -> Result<i64, PersistenceError> {
    value
        .checked_add(1)
        .filter(|value| *value > 0)
        .ok_or_else(|| PersistenceError::SchemaDrift {
            collection: "campaigns".to_owned(),
            detail: format!("{field} overflowed"),
        })
}

fn nonnegative_u64(value: i64) -> u64 {
    if value < 0 { 0 } else { value as u64 }
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn to_i64_persistence(value: u64) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| PersistenceError::SchemaDrift {
        collection: "command_receipts".to_owned(),
        detail: "revision is outside the supported range".to_owned(),
    })
}

fn repository_to_persistence(error: RepositoryError) -> PersistenceError {
    match error {
        RepositoryError::NotFound { entity, id } => PersistenceError::NotFound { entity, id },
        RepositoryError::AlreadyExists { entity, id } => {
            PersistenceError::AlreadyExists { entity, id }
        }
        RepositoryError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        } => PersistenceError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        },
        _ => PersistenceError::SchemaDrift {
            collection: "campaigns".to_owned(),
            detail: "stored campaign failed lifecycle validation".to_owned(),
        },
    }
}

fn mongo_error(operation: &'static str, error: mongodb::error::Error) -> RepositoryError {
    RepositoryError::Persistence(PersistenceError::mongo(operation, error))
}

fn map_transaction_error(
    error: PersistenceError,
    entity: &'static str,
    id: &str,
) -> RepositoryError {
    match error {
        PersistenceError::NotFound { entity, id } => RepositoryError::NotFound { entity, id },
        PersistenceError::AlreadyExists { entity, id } => {
            RepositoryError::AlreadyExists { entity, id }
        }
        PersistenceError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        } => RepositoryError::RevisionConflict {
            entity,
            id,
            expected,
            actual,
        },
        other if other.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey) => {
            RepositoryError::AlreadyExists {
                entity,
                id: id.to_owned(),
            }
        }
        other if other.mongo_failure_kind() == Some(MongoFailureKind::DocumentValidation) => {
            RepositoryError::InvalidDomainState {
                entity,
                id: id.to_owned(),
                reason: "document failed MongoDB schema validation",
            }
        }
        other => RepositoryError::Persistence(other),
    }
}

fn state_label(state: CampaignLifecycleState) -> &'static str {
    match state {
        CampaignLifecycleState::Active => "active",
        CampaignLifecycleState::Archived => "archived",
    }
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
    use std::time::Duration;

    use super::*;
    use crate::{
        config::{MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::{MongoStore, SchemaReconciler},
    };

    async fn test_repository() -> Option<(MongoRepository, String)> {
        let uri = std::env::var("MONGODB_TEST_URI").ok()?;
        if uri.trim().is_empty() {
            return None;
        }
        let database = format!("mdnd_test_lifecycle_{}", Uuid::new_v4().simple());
        let config = MongoConfig {
            uri: SecretString::new(uri),
            database: database.clone(),
            max_pool_size: 5,
            min_pool_size: 0,
            connect_timeout: Duration::from_secs(5),
            server_selection_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
            transaction_timeout: Duration::from_secs(10),
            transaction_max_retries: 3,
            schema_policy: MongoSchemaPolicy::ApplyAndVerify,
        };
        let store = MongoStore::connect(&config)
            .await
            .expect("test MongoDB must connect");
        SchemaReconciler::new(store.clone())
            .apply()
            .await
            .expect("schema must apply");
        Some((MongoRepository::new(store), database))
    }

    async fn insert_account(repository: &MongoRepository, account_id: &str) {
        repository
            .store()
            .document_collection(CollectionName::Accounts)
            .insert_one(doc! {
                "_id": account_id,
                "schema_version": 1_i64,
                "revision": 1_i64,
                "role": "user",
                "username_normalized": format!("user-{}", Uuid::new_v4()),
                "email_lookup_hmac": format!("hmac-sha256:{}", Uuid::new_v4().simple()),
                "password_phc": "$argon2id$test",
                "login_enabled": false,
                "created_at": DateTime::now(),
                "updated_at": DateTime::now(),
            })
            .await
            .expect("account fixture must insert");
    }

    fn lifecycle_command(campaign_id: &str, revision: u64, key: &str) -> CampaignLifecycleCommand {
        CampaignLifecycleCommand {
            schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: campaign_id.to_owned(),
            expected_lifecycle_revision: revision,
            idempotency_key: key.to_owned(),
        }
    }

    #[tokio::test]
    async fn mongo_lifecycle_replay_export_delete_restore_contract() {
        let Some((repository, database)) = test_repository().await else {
            return;
        };
        let owner = format!("account:{}", Uuid::new_v4());
        insert_account(&repository, &owner).await;
        let campaign = repository
            .create_campaign_with_owner(
                &owner,
                "Lifecycle Test",
                "dev.manchester-arcana.rainbound-borough",
            )
            .await
            .expect("campaign fixture must create");
        let start = StartPlaySessionCommand {
            lifecycle: lifecycle_command(&campaign.campaign_id, 1, "command:start-once"),
            play_session_id: format!("play-session:{}", Uuid::new_v4()),
        };
        let started = repository
            .start_campaign_play_session(&owner, &start)
            .await
            .expect("start must work");
        assert_eq!(
            repository
                .start_campaign_play_session(&owner, &start)
                .await
                .expect("exact replay must work"),
            started
        );
        let mut changed = start.clone();
        changed.play_session_id = format!("play-session:{}", Uuid::new_v4());
        assert!(
            repository
                .start_campaign_play_session(&owner, &changed)
                .await
                .is_err()
        );
        repository
            .end_campaign_play_session(
                &owner,
                &EndPlaySessionCommand {
                    lifecycle: lifecycle_command(&campaign.campaign_id, 2, "command:end-once"),
                    play_session_id: start.play_session_id,
                },
            )
            .await
            .expect("end must work");
        repository
            .archive_campaign(
                &owner,
                &lifecycle_command(&campaign.campaign_id, 3, "command:archive-once"),
            )
            .await
            .expect("archive must work");
        let export = repository
            .export_campaign_private(&owner, &campaign.campaign_id)
            .await
            .expect("export must work");
        let canonical = export.canonical_json().expect("export must canonicalize");
        assert!(!canonical.contains("password_hash"));
        assert!(!canonical.contains("email_ciphertext"));
        let asset_id = format!("asset:{}", Uuid::new_v4());
        repository
            .store()
            .document_collection(CollectionName::GeneratedAssets)
            .insert_one(doc! {
                "_id": &asset_id,
                "schema_version": 1_i64,
                "owner_account_id": &owner,
                "campaign_id": &campaign.campaign_id,
                "entity_kind": "campaign",
                "entity_id": &campaign.campaign_id,
                "object_key": format!("campaign-assets/{asset_id}.png"),
                "digest": format!("sha256:{}", "a".repeat(64)),
                "state": "published",
                "created_at": DateTime::now(),
            })
            .await
            .expect("asset fixture must insert");
        let pending_cleanup = repository
            .prepare_campaign_deletion(
                &owner,
                &campaign.campaign_id,
                4,
                "deletion:external-cleanup",
            )
            .await
            .expect("pending-cleanup deletion preparation must work");
        assert!(matches!(
            repository
                .delete_archived_campaign(
                    &owner,
                    &DeleteCampaignCommand {
                        lifecycle: lifecycle_command(
                            &campaign.campaign_id,
                            4,
                            "command:delete-before-cleanup",
                        ),
                        deletion_id: pending_cleanup.deletion_id,
                        confirm_permanent_delete: true,
                    },
                )
                .await,
            Err(RepositoryError::AlreadyExists {
                entity: "campaign_external_asset_cleanup",
                ..
            })
        ));
        repository
            .store()
            .document_collection(CollectionName::GeneratedAssets)
            .update_one(
                doc! { "_id": &asset_id },
                doc! { "$set": { "state": "erased" } },
            )
            .await
            .expect("asset erasure evidence must update");
        let prepared = repository
            .prepare_campaign_deletion(&owner, &campaign.campaign_id, 4, "deletion:lifecycle-test")
            .await
            .expect("deletion preparation must work");
        assert_eq!(
            repository
                .prepare_campaign_deletion(
                    &owner,
                    &campaign.campaign_id,
                    4,
                    "deletion:lifecycle-test",
                )
                .await
                .expect("deletion preparation replay must work"),
            prepared
        );
        let delete = DeleteCampaignCommand {
            lifecycle: lifecycle_command(&campaign.campaign_id, 4, "command:delete-once"),
            deletion_id: prepared.deletion_id,
            confirm_permanent_delete: true,
        };
        let late_audit_id = format!("audit:{}", Uuid::new_v4());
        repository
            .store()
            .document_collection(CollectionName::AuditEvents)
            .insert_one(doc! {
                "_id": &late_audit_id,
                "schema_version": 1_i64,
                "category": "lifecycle",
                "action": "test_child_mutation",
                "outcome": "success",
                "scope_kind": "campaign",
                "scope_id": &campaign.campaign_id,
                "campaign_id": &campaign.campaign_id,
                "created_at": DateTime::now(),
            })
            .await
            .expect("late audit fixture must insert");
        assert!(matches!(
            repository.delete_archived_campaign(&owner, &delete).await,
            Err(RepositoryError::AlreadyExists {
                entity: "stale_campaign_deletion_preparation",
                ..
            })
        ));
        repository
            .store()
            .document_collection(CollectionName::AuditEvents)
            .delete_one(doc! { "_id": late_audit_id })
            .await
            .expect("late audit fixture must delete");
        let deleted = repository
            .delete_archived_campaign(&owner, &delete)
            .await
            .expect("delete must work");
        assert!(deleted.deleted);
        assert_eq!(
            repository
                .delete_archived_campaign(&owner, &delete)
                .await
                .expect("delete replay must work"),
            deleted
        );
        assert!(
            repository
                .has_campaign_deletion_tombstone(&owner, &campaign.campaign_id)
                .await
                .expect("tombstone check must work")
        );
        let tombstone = repository
            .store()
            .document_collection(CollectionName::DeletionTombstones)
            .find_one(doc! {
                "entity_kind": "campaign",
                "entity_id": &campaign.campaign_id,
                "owner_account_id": &owner,
            })
            .await
            .expect("tombstone read must work")
            .expect("tombstone must exist");
        let deleted_at = tombstone
            .get_datetime("deleted_at")
            .expect("deleted timestamp must exist");
        let purge_at = tombstone
            .get_datetime("purge_at")
            .expect("purge timestamp must exist");
        assert_eq!(
            purge_at
                .timestamp_millis()
                .saturating_sub(deleted_at.timestamp_millis()),
            DELETION_TOMBSTONE_SECONDS.saturating_mul(1_000)
        );
        assert!(
            repository
                .store()
                .document_collection(CollectionName::PlaySessions)
                .find_one(doc! { "campaign_id": &campaign.campaign_id })
                .await
                .expect("cascade read must work")
                .is_none()
        );
        assert!(
            repository
                .store()
                .document_collection(CollectionName::DeletionPreparations)
                .find_one(doc! { "scope_id": &campaign.campaign_id })
                .await
                .expect("preparation cascade read must work")
                .is_none()
        );
        let restored = repository
            .restore_campaign_export(
                &owner,
                &RestoreCampaignExportCommand {
                    schema_version: CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    idempotency_key: "command:restore-once".to_owned(),
                    canonical_export_json: canonical,
                },
            )
            .await
            .expect("restore must work");
        assert_eq!(restored.campaign_session_id, campaign.campaign_id);
        assert!(
            repository
                .list_campaign_play_sessions(&owner, &restored.campaign_session_id)
                .await
                .expect("restored play history must load")
                .iter()
                .all(|play| play.state == "closed")
        );

        assert!(
            database.starts_with("mdnd_test_lifecycle_"),
            "cleanup safeguard"
        );
        repository
            .store()
            .database()
            .drop()
            .await
            .expect("test database must drop");
    }
}
