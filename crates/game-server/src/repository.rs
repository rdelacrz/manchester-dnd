use manchester_dnd_core::{
    Character, EventActor, SESSION_SCHEMA_VERSION, SessionDto, SessionEventDto,
    SessionEventPayload, SessionStatus, Sha256Digest,
    encounter::{EncounterState, EncounterStatus},
    is_valid_opaque_id,
};
use mongodb::{
    ClientSession, Collection,
    bson::{Bson, DateTime, Document, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::{CollectionName, MongoStore, TransactionFuture},
};

pub mod action_points;
mod governance;
mod hero;
mod images;
mod inspiration;
pub mod jobs;
pub(crate) mod lifecycle;
mod memberships;
mod operations;
mod pins;
mod player_characters;
mod presentations;
mod recaps;

pub use action_points::ActionPointRepository;
pub use governance::{
    GENERATION_GOVERNANCE_SCHEMA_VERSION, GenerationBudgetDimension,
    GenerationBudgetRejectionMetric, GenerationBudgetScope, GenerationBudgetStatus,
    GenerationBudgetStatusLine, GenerationBudgetTotals, GenerationCleanupOutcome,
    GenerationGovernanceReceipt, GenerationGovernanceState, GenerationMetricBucket,
    GenerationMetricsSnapshot, NewGenerationGovernanceReceipt,
};
pub(crate) use hero::{
    EncounterHeroUpdate, HeroCharacterMutationCommand, HeroCreationCommitMetadata,
    HeroReceiptScope, NewEncounterRewardClaim, NewHeroCommandReceipt, StoredHeroCommandReceipt,
};
pub use hero::{HeroAuditPayload, StoredHeroAudit};
pub use images::{
    AuthorizedSceneImageVariant, NewSceneImageArtifact, NewSceneImageQuarantine,
    SceneImageArtifact, SceneImageCleanupCandidate, SceneImageRequestCounts, SceneImageVariant,
};
pub use lifecycle::{
    CAMPAIGN_EXPORT_SCHEMA_VERSION, CAMPAIGN_HISTORY_DEFAULT_LIMIT, CAMPAIGN_HISTORY_MAX_LIMIT,
    CAMPAIGN_LIFECYCLE_SCHEMA_VERSION, CampaignLifecycleCommand, CampaignLifecycleOutcome,
    CampaignLifecycleState, CampaignPlaySession, CampaignPrivateExportV1, CampaignSummary,
    CampaignTurnHistoryItem, CampaignTurnHistoryPage, DeleteCampaignCommand, EndPlaySessionCommand,
    PreparedCampaignDeletion, RestoreCampaignExportCommand, StartPlaySessionCommand,
};
pub use memberships::{
    AssignCharacterOutcome, CampaignCharacterInstanceRow, CampaignInvitationRow,
    CampaignMembershipRow, CharacterInstanceState, CreateCampaignWithOwnerOutcome,
    MembershipCampaignSummary, MembershipRole, MembershipState,
};
pub use operations::{
    CompleteRecoveryManifest, DATABASE_OPERATIONS_SNAPSHOT_SCHEMA_VERSION,
    DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION, DatabaseOperationsSnapshot,
    DatabaseRecoveryManifest, GenerationBudgetDenialCount, GenerationQueueStateCount,
    OperationalOutcomeCount, RecoveryArtifactFileEntry, RecoveryCampaignManifestEntry,
    RecoveryManifestError, RecoverySchemaManifestEntry, VerifiedRecoveryFile,
};
pub use pins::StoredCampaignPins;
pub use player_characters::{
    NewPlayerCharacterReceipt, PlayerCharacterDraftSummary, PlayerCharacterSummary,
    StoredPlayerCharacterReceipt,
};
pub use presentations::{
    GeneratedTextPresentation, GeneratedTextPresentationReceipt, GeneratedTextPresentationReplay,
    GeneratedTextPresentationSnapshot, GeneratedTextPresentationSource,
    MAX_TEXT_PRESENTATION_VERSIONS, NewGeneratedTextPresentation, NewTypedIntentCommandReceipt,
    TextPresentationStoreError, TypedIntentCommandReceipt, TypedIntentReceiptState,
};
pub use recaps::{CampaignPrivateRecap, GeneratePrivateRecapCommand, PRIVATE_RECAP_SCHEMA_VERSION};

pub const CHARACTER_SCHEMA_VERSION: u32 = 1;
const STORAGE_SCHEMA_VERSION: u32 = 1;
const MAX_COMMAND_RESPONSE_JSON_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub revision: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CharacterUpdate<'a> {
    pub character: &'a Character,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterCommitOutcome {
    pub character_id: String,
    pub save: SaveOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignCreateOutcome {
    pub session: SaveOutcome,
    pub characters: Vec<CharacterCommitOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventCommitOutcome {
    pub session: SaveOutcome,
    pub characters: Vec<CharacterCommitOutcome>,
    pub hero_character: Option<SaveOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewCommandReceipt {
    pub(crate) actor_account_id: String,
    pub(crate) campaign_session_id: String,
    pub(crate) idempotency_key: String,
    pub(crate) command_kind: String,
    pub(crate) request_fingerprint: Sha256Digest,
    pub(crate) expected_revision: u64,
    pub(crate) result_revision: u64,
    pub(crate) audit_id: String,
    pub(crate) response_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredCommandReceipt {
    pub(crate) actor_account_id: String,
    pub(crate) campaign_session_id: String,
    pub(crate) idempotency_key: String,
    pub(crate) command_kind: String,
    pub(crate) request_fingerprint: Sha256Digest,
    pub(crate) expected_revision: u64,
    pub(crate) result_revision: u64,
    pub(crate) audit_id: String,
    pub(crate) response_json: String,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredDocument<T> {
    pub id: String,
    pub schema_version: u32,
    pub revision: u64,
    pub value: T,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnAudit<T> {
    pub id: String,
    pub campaign_session_id: String,
    pub turn_number: u64,
    pub actor_id: Option<String>,
    pub correlation_id: Option<String>,
    pub schema_version: u32,
    pub payload: T,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewGeneratedAssetAudit {
    pub id: String,
    pub owner_account_id: String,
    pub campaign_session_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub turn_id: Option<String>,
    pub asset_kind: String,
    pub provider: String,
    pub model: String,
    pub location: String,
    pub object_digest: Sha256Digest,
    pub state: String,
    /// Caller-provided digest. Raw prompts are intentionally excluded.
    pub prompt_fingerprint: Option<Sha256Digest>,
    pub metadata: GeneratedAssetMetadata,
}

/// Allowlisted non-sensitive media facts. Raw prompts, credentials, and
/// arbitrary provider JSON are deliberately impossible to represent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneratedAssetMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub media_type: Option<String>,
    pub provider_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedAssetAudit {
    pub id: String,
    pub owner_account_id: String,
    pub campaign_session_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub turn_id: Option<String>,
    pub asset_kind: String,
    pub provider: String,
    pub model: String,
    pub location: String,
    pub object_digest: Sha256Digest,
    pub state: String,
    pub prompt_fingerprint: Option<Sha256Digest>,
    pub metadata: GeneratedAssetMetadata,
    pub created_at: String,
}

/// Cloneable gameplay repository backed only by MongoDB.
#[derive(Clone)]
pub struct MongoRepository {
    store: MongoStore,
}

impl MongoRepository {
    #[must_use]
    pub fn new(store: MongoStore) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn from_store(store: MongoStore) -> Self {
        Self::new(store)
    }

    #[must_use]
    pub fn store(&self) -> &MongoStore {
        &self.store
    }

    /// Delegates to `MongoStore::with_transaction` so repository modules
    /// can run multi-document transactions through the repository handle.
    pub(crate) async fn with_transaction<T, F>(&self, callback: F) -> Result<T, PersistenceError>
    where
        T: Send,
        F: for<'session> FnMut(
                &'session mut mongodb::ClientSession,
            ) -> TransactionFuture<'session, T>
            + Send,
    {
        self.store.with_transaction(callback).await
    }

    #[must_use]
    pub fn into_store(self) -> MongoStore {
        self.store
    }

    pub(crate) async fn health_check(&self) -> Result<(), RepositoryError> {
        self.store.ping().await.map_err(RepositoryError::from)
    }

    /// Creates campaign aggregate and initial bounded runtime roster together.
    pub(crate) async fn create_campaign(
        &self,
        actor_account_id: &str,
        session: &SessionDto,
        characters: &[Character],
    ) -> Result<CampaignCreateOutcome, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_session(session)?;
        if session.last_event_sequence != 0 || session.status != SessionStatus::Active {
            return invalid(
                "campaign session",
                &session.id,
                "a new session must be active and start before its first event",
            );
        }
        validate_initial_roster(session, characters)?;
        if characters.len() > 1 {
            return invalid(
                "campaign session",
                &session.id,
                "campaign creation accepts one player-owned runtime character per account",
            );
        }

        let now = DateTime::now();
        let campaign = CampaignDocument {
            id: session.id.clone(),
            schema_version: i64::from(STORAGE_SCHEMA_VERSION),
            revision: 1,
            gameplay_revision: 1,
            lifecycle_revision: 1,
            owner_account_id: actor_account_id.to_owned(),
            title: session.title.clone(),
            title_normalized: normalize_title(&session.title),
            theme_id: String::new(),
            lifecycle: CampaignLifecycleDocument {
                state: "open".to_owned(),
                archived_at: None,
            },
            members: vec![CampaignMemberDocument {
                account_id: actor_account_id.to_owned(),
                role: "game_master".to_owned(),
                state: "active".to_owned(),
                inviter_account_id: None,
                joined_at: now,
                left_at: None,
                created_at: now,
                updated_at: now,
            }],
            rules_snapshot: doc! { "state": "unsealed" },
            safety_policy_id: "safety:private-v1".to_owned(),
            progression_policy_id: "progression:xp-v1".to_owned(),
            retention_class: "campaign_lifetime".to_owned(),
            retention_delete_after: None,
            current_play_session_id: None,
            session: session.clone(),
            created_at: now,
            updated_at: now,
        };
        let instances = characters
            .iter()
            .map(|character| {
                CoreCharacterInstanceDocument::new(
                    actor_account_id,
                    &session.id,
                    character.clone(),
                    now,
                )
            })
            .collect::<Vec<_>>();
        let campaigns = self.campaigns();
        let character_instances = self.character_instances();
        let campaign_for_write = campaign.clone();
        let instances_for_write = instances.clone();
        let result = self
            .store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let character_instances = character_instances.clone();
                let campaign = campaign_for_write.clone();
                let instances = instances_for_write.clone();
                Box::pin(async move {
                    campaigns
                        .insert_one(campaign)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("create campaign", error))?;
                    if !instances.is_empty() {
                        character_instances
                            .insert_many(instances)
                            .session(&mut *client_session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo(
                                    "create campaign character instances",
                                    error,
                                )
                            })?;
                    }
                    Ok(())
                })
            })
            .await;
        map_write_result(result, "campaign session", &session.id)?;

        Ok(CampaignCreateOutcome {
            session: SaveOutcome {
                revision: 1,
                updated_at: date_string(now),
            },
            characters: instances
                .into_iter()
                .map(|instance| CharacterCommitOutcome {
                    character_id: instance.id,
                    save: SaveOutcome {
                        revision: 1,
                        updated_at: date_string(now),
                    },
                })
                .collect(),
        })
    }

    pub async fn load_campaign_session(
        &self,
        actor_account_id: &str,
        id: &str,
    ) -> Result<Option<StoredDocument<SessionDto>>, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_opaque("campaign session", id)?;
        let stored = self
            .campaigns()
            .find_one(active_campaign_filter(actor_account_id, id))
            .await
            .map_err(|error| mongo_error("load campaign session", error))?;
        stored.map(stored_session).transpose()
    }

    pub async fn load_character(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
        id: &str,
    ) -> Result<Option<StoredDocument<Character>>, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_opaque("campaign session", campaign_session_id)?;
        validate_opaque("character", id)?;
        let campaigns = self.campaigns();
        let characters = self.character_instances();
        let actor = actor_account_id.to_owned();
        let campaign_id = campaign_session_id.to_owned();
        let character_id = id.to_owned();
        self.store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let characters = characters.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let character_id = character_id.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    characters
                        .find_one(doc! {
                            "_id": &character_id,
                            "campaign_id": &campaign_id,
                            "state": "active",
                            "runtime_kind": "core_character",
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load character", error))
                })
            })
            .await
            .map_err(map_persistence)?
            .map(stored_character)
            .transpose()
    }

    pub(crate) async fn load_command_receipt(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<StoredCommandReceipt>, RepositoryError> {
        validate_command_receipt_lookup(actor_account_id, campaign_session_id, idempotency_key)?;
        let campaigns = self.campaigns();
        let receipts = self.receipts();
        let actor = actor_account_id.to_owned();
        let campaign = campaign_session_id.to_owned();
        let key = idempotency_key.to_owned();
        let receipt = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let receipts = receipts.clone();
                let actor = actor.clone();
                let campaign = campaign.clone();
                let key = key.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign,
                    )
                    .await?;
                    receipts
                        .find_one(doc! {
                            "scope_kind": "campaign",
                            "scope_id": &campaign,
                            "actor_account_id": &actor,
                            "idempotency_key": &key,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load command receipt", error))
                })
            })
            .await
            .map_err(map_persistence)?;
        receipt.map(stored_command_receipt).transpose()
    }

    #[cfg(test)]
    pub(crate) async fn commit_session_event(
        &self,
        actor_account_id: &str,
        audit_id: &str,
        session: &SessionDto,
        expected_revision: u64,
        event: &SessionEventDto,
        character_updates: &[CharacterUpdate<'_>],
    ) -> Result<SessionEventCommitOutcome, RepositoryError> {
        self.commit_session_event_internal(
            actor_account_id,
            audit_id,
            session,
            expected_revision,
            event,
            CommitUpdates {
                characters: character_updates,
                hero: None,
            },
            CommitMetadata {
                receipt: None,
                correlation_id: None,
            },
        )
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn commit_session_event_with_receipt(
        &self,
        audit_id: &str,
        session: &SessionDto,
        expected_revision: u64,
        event: &SessionEventDto,
        character_updates: &[CharacterUpdate<'_>],
        receipt: &NewCommandReceipt,
    ) -> Result<SessionEventCommitOutcome, RepositoryError> {
        validate_command_receipt_for_commit(receipt, audit_id, session, expected_revision, event)?;
        self.commit_session_event_internal(
            &receipt.actor_account_id,
            audit_id,
            session,
            expected_revision,
            event,
            CommitUpdates {
                characters: character_updates,
                hero: None,
            },
            CommitMetadata {
                receipt: Some(receipt),
                correlation_id: None,
            },
        )
        .await
    }

    pub(crate) async fn commit_session_event_with_receipt_and_correlation(
        &self,
        session: &SessionDto,
        expected_revision: u64,
        event: &SessionEventDto,
        character_updates: &[CharacterUpdate<'_>],
        receipt: &NewCommandReceipt,
        correlation_id: &str,
    ) -> Result<SessionEventCommitOutcome, RepositoryError> {
        validate_command_receipt_for_commit(
            receipt,
            &receipt.audit_id,
            session,
            expected_revision,
            event,
        )?;
        self.commit_session_event_internal(
            &receipt.actor_account_id,
            &receipt.audit_id,
            session,
            expected_revision,
            event,
            CommitUpdates {
                characters: character_updates,
                hero: None,
            },
            CommitMetadata {
                receipt: Some(receipt),
                correlation_id: Some(correlation_id),
            },
        )
        .await
    }

    pub(crate) async fn commit_encounter_event_with_receipt_and_correlation(
        &self,
        session: &SessionDto,
        expected_revision: u64,
        event: &SessionEventDto,
        hero_update: Option<EncounterHeroUpdate<'_>>,
        receipt: &NewCommandReceipt,
        correlation_id: &str,
    ) -> Result<SessionEventCommitOutcome, RepositoryError> {
        validate_command_receipt_for_commit(
            receipt,
            &receipt.audit_id,
            session,
            expected_revision,
            event,
        )?;
        self.commit_session_event_internal(
            &receipt.actor_account_id,
            &receipt.audit_id,
            session,
            expected_revision,
            event,
            CommitUpdates {
                characters: &[],
                hero: hero_update,
            },
            CommitMetadata {
                receipt: Some(receipt),
                correlation_id: Some(correlation_id),
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_session_event_internal(
        &self,
        actor_account_id: &str,
        audit_id: &str,
        session: &SessionDto,
        expected_revision: u64,
        event: &SessionEventDto,
        updates: CommitUpdates<'_>,
        metadata: CommitMetadata<'_>,
    ) -> Result<SessionEventCommitOutcome, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_session(session)?;
        validate_session_event(event)?;
        validate_opaque("session event", audit_id)?;
        if metadata
            .correlation_id
            .is_some_and(|value| !is_valid_opaque_id(value))
        {
            return invalid(
                "session event",
                audit_id,
                "correlation id must be a valid opaque identifier",
            );
        }
        if session.id != event.session_id || session.last_event_sequence != event.sequence {
            return invalid(
                "session event",
                audit_id,
                "session snapshot and event identity or sequence do not match",
            );
        }
        if event_references_unknown_character(session, event) {
            return invalid(
                "session event",
                audit_id,
                "event references a character outside the campaign session",
            );
        }
        validate_character_update_set(session, event, updates.characters)?;
        let current = self
            .load_campaign_session(actor_account_id, &session.id)
            .await?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "campaign session",
                id: session.id.clone(),
            })?;
        validate_session_successor(&current, session, expected_revision, event, audit_id)?;

        let mut prepared_characters = Vec::with_capacity(updates.characters.len());
        for update in updates.characters {
            let current_character = self
                .load_character(actor_account_id, &session.id, update.character.id())
                .await?
                .ok_or_else(|| RepositoryError::NotFound {
                    entity: "character",
                    id: update.character.id().to_owned(),
                })?;
            validate_character_successor(&current_character, update, event)?;
            prepared_characters.push(PreparedCharacterUpdate {
                id: update.character.id().to_owned(),
                expected_revision: update.expected_revision,
                character_snapshot: update.character.clone(),
                progression: character_progression(update.character),
            });
        }
        let prepared_hero = if let Some(update) = updates.hero {
            Some(
                hero::prepare_encounter_hero_update(
                    self,
                    actor_account_id,
                    &session.id,
                    event,
                    update,
                )
                .await?,
            )
        } else {
            None
        };

        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange { field: "revision" })?;
        let expected_revision_i64 = to_i64(expected_revision, "revision")?;
        let now = DateTime::now();
        let prepared_encounter = prepare_encounter_projection(&session.id, event, now)?;
        let turn = TurnEventDocument {
            id: audit_id.to_owned(),
            schema_version: STORAGE_SCHEMA_VERSION,
            campaign_id: session.id.clone(),
            play_session_id: session.id.clone(),
            sequence: event.sequence,
            correlation_id: metadata.correlation_id.unwrap_or(audit_id).to_owned(),
            actor_account_id: actor_account_id.to_owned(),
            actor_id: match &event.actor {
                EventActor::Player { character_id } => Some(character_id.clone()),
                EventActor::AiGameMaster | EventActor::System => None,
            },
            mode: event_mode(event).to_owned(),
            phase: "committed".to_owned(),
            event: event.clone(),
            created_at: now,
        };
        let audit = AuditEventDocument {
            id: format!("audit:{}", Uuid::new_v4().simple()),
            schema_version: STORAGE_SCHEMA_VERSION,
            category: "gameplay".to_owned(),
            action: event_action(event).to_owned(),
            outcome: "committed".to_owned(),
            actor_account_id: Some(actor_account_id.to_owned()),
            scope_kind: "campaign".to_owned(),
            scope_id: session.id.clone(),
            correlation_id: Some(turn.correlation_id.clone()),
            metadata: doc! {
                "turn_event_id": audit_id,
                "sequence": to_i64(event.sequence, "event sequence")?,
                "result_revision": to_i64(next_revision, "revision")?,
            },
            created_at: now,
        };
        let receipt_document = metadata.receipt.map(command_receipt_document).transpose()?;

        let campaigns = self.campaigns();
        let characters = self.character_instances();
        let hero_characters = self
            .store
            .collection(CollectionName::CampaignCharacterInstances);
        let play_sessions = self.play_sessions();
        let encounters = self.encounters();
        let turn_events = self.turn_events();
        let audits = self.audits();
        let receipts = self.receipts();
        let actor = actor_account_id.to_owned();
        let campaign_id = session.id.clone();
        let successor = session.clone();
        let prepared_characters_for_write = prepared_characters.clone();
        let prepared_hero_for_write = prepared_hero.clone();
        let prepared_encounter_for_write = prepared_encounter.clone();
        let turn_for_write = turn.clone();
        let audit_for_write = audit.clone();
        let receipt_for_write = receipt_document.clone();
        let transaction_result = self
            .store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let characters = characters.clone();
                let hero_characters = hero_characters.clone();
                let play_sessions = play_sessions.clone();
                let encounters = encounters.clone();
                let turn_events = turn_events.clone();
                let audits = audits.clone();
                let receipts = receipts.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let successor = successor.clone();
                let character_updates = prepared_characters_for_write.clone();
                let hero_update = prepared_hero_for_write.clone();
                let encounter_update = prepared_encounter_for_write.clone();
                let mut turn = turn_for_write.clone();
                let audit = audit_for_write.clone();
                let receipt = receipt_for_write.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    let play_session = play_sessions
                        .find_one(doc! {
                            "campaign_id": &campaign_id,
                            "state": "active",
                            "$or": [
                                { "gm_account_id": &actor },
                                {
                                    "participants": {
                                        "$elemMatch": {
                                            "account_id": &actor,
                                            "state": {
                                                "$in": ["active", "human_active", "ai_active"],
                                            },
                                        }
                                    }
                                }
                            ],
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize active play session", error)
                        })?
                        .ok_or_else(|| PersistenceError::NotFound {
                            entity: "active play session",
                            id: campaign_id.clone(),
                        })?;
                    turn.play_session_id = play_session.id.clone();
                    if let Some(receipt) = &receipt {
                        reject_conflicting_receipt(&receipts, client_session, receipt).await?;
                    }
                    let updated = campaigns
                        .find_one_and_update(
                            campaign_revision_filter(
                                &actor,
                                &campaign_id,
                                expected_revision_i64,
                                "open",
                            ),
                            doc! {
                                "$set": {
                                    "session": mongodb::bson::to_bson(&successor)
                                        .map_err(PersistenceError::BsonEncoding)?,
                                    "updated_at": now,
                                },
                                "$inc": {
                                    "revision": 1_i64,
                                    "gameplay_revision": 1_i64,
                                },
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("commit campaign session event", error)
                        })?;
                    let updated = match updated {
                        Some(updated) => updated,
                        None => {
                            let current = campaigns
                                .find_one(active_campaign_filter(&actor, &campaign_id))
                                .session(&mut *client_session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo(
                                        "load conflicting campaign revision",
                                        error,
                                    )
                                })?
                                .ok_or_else(|| PersistenceError::NotFound {
                                    entity: "campaign session",
                                    id: campaign_id.clone(),
                                })?;
                            return Err(PersistenceError::RevisionConflict {
                                entity: "campaign session",
                                id: campaign_id.clone(),
                                expected: expected_revision,
                                actual: u64::try_from(current.gameplay_revision)
                                    .unwrap_or_default(),
                            });
                        }
                    };
                    for update in character_updates {
                        let changed = characters
                            .update_one(
                                doc! {
                                    "_id": &update.id,
                                    "campaign_id": &campaign_id,
                                    "runtime_kind": "core_character",
                                    "state": "active",
                                    "revision": to_i64(
                                        update.expected_revision,
                                        "character revision",
                                    )
                                    .map_err(repository_numeric_as_persistence)?,
                                },
                                doc! {
                                    "$set": {
                                        "runtime.character_snapshot": mongodb::bson::to_bson(
                                            &update.character_snapshot,
                                        )
                                            .map_err(PersistenceError::BsonEncoding)?,
                                        "progression": update.progression,
                                        "updated_at": now,
                                    },
                                    "$inc": { "revision": 1_i64 },
                                },
                            )
                            .session(&mut *client_session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("commit character event update", error)
                            })?;
                        if changed.modified_count != 1 {
                            let current = characters
                                .find_one(doc! {
                                    "_id": &update.id,
                                    "campaign_id": &campaign_id,
                                    "runtime_kind": "core_character",
                                    "state": "active",
                                })
                                .session(&mut *client_session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo(
                                        "load conflicting character revision",
                                        error,
                                    )
                                })?
                                .ok_or_else(|| PersistenceError::NotFound {
                                    entity: "character",
                                    id: update.id.clone(),
                                })?;
                            return Err(PersistenceError::RevisionConflict {
                                entity: "character",
                                id: update.id,
                                expected: update.expected_revision,
                                actual: current.revision,
                            });
                        }
                    }
                    let hero_save = if let Some(update) = hero_update {
                        Some(
                            hero::commit_prepared_encounter_hero_update(
                                &hero_characters,
                                client_session,
                                &campaign_id,
                                now,
                                update,
                            )
                            .await?,
                        )
                    } else {
                        None
                    };
                    if let Some(update) = encounter_update {
                        commit_encounter_projection(
                            &encounters,
                            client_session,
                            &campaign_id,
                            &play_session.id,
                            update,
                        )
                        .await?;
                    }
                    let mut play_session_set = doc! {
                        "mode": event_mode(&turn.event),
                        "turn_state.sequence": to_i64(turn.sequence, "turn sequence")
                            .map_err(repository_numeric_as_persistence)?,
                        "turn_state.based_on_event_sequence": to_i64(
                            turn.sequence,
                            "turn sequence",
                        )
                        .map_err(repository_numeric_as_persistence)?,
                        "updated_at": now,
                    };
                    if let SessionEventPayload::EncounterResolved { outcome, .. } =
                        &turn.event.payload
                    {
                        play_session_set.insert(
                            "turn_state.active_encounter_id",
                            encounter_instance_id(&campaign_id, &outcome.resolution.encounter_id),
                        );
                    }
                    let play_session_changed = play_sessions
                        .update_one(
                            doc! {
                                "_id": &play_session.id,
                                "campaign_id": &campaign_id,
                                "state": "active",
                                "revision": to_i64(
                                    play_session.revision,
                                    "play session revision",
                                )
                                .map_err(repository_numeric_as_persistence)?,
                            },
                            doc! {
                                "$set": play_session_set,
                                "$inc": { "revision": 1_i64 },
                            },
                        )
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("advance active play session", error)
                        })?;
                    if play_session_changed.modified_count != 1 {
                        return Err(PersistenceError::RevisionConflict {
                            entity: "play session",
                            id: play_session.id,
                            expected: play_session.revision,
                            actual: play_session.revision,
                        });
                    }
                    turn_events
                        .insert_one(turn)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert turn event", error))?;
                    audits
                        .insert_one(audit)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert gameplay audit", error))?;
                    if let Some(receipt) = receipt {
                        receipts
                            .insert_one(receipt)
                            .session(&mut *client_session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("insert command receipt", error)
                            })?;
                    }
                    Ok((updated.updated_at, hero_save))
                })
            })
            .await;
        let (updated_at, hero_character) = match transaction_result {
            Ok(value) => value,
            Err(error) => {
                return Err(map_commit_error(
                    self,
                    error,
                    actor_account_id,
                    &session.id,
                    receipt_document.as_ref(),
                )
                .await);
            }
        };
        Ok(SessionEventCommitOutcome {
            session: SaveOutcome {
                revision: next_revision,
                updated_at: date_string(updated_at),
            },
            characters: prepared_characters
                .into_iter()
                .map(|update| CharacterCommitOutcome {
                    character_id: update.id,
                    save: SaveOutcome {
                        revision: update.expected_revision.saturating_add(1),
                        updated_at: date_string(now),
                    },
                })
                .collect(),
            hero_character,
        })
    }

    pub async fn list_session_events(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
    ) -> Result<Vec<TurnAudit<SessionEventDto>>, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_opaque("campaign session", campaign_session_id)?;
        let campaigns = self.campaigns();
        let events = self.turn_events();
        let actor = actor_account_id.to_owned();
        let campaign_id = campaign_session_id.to_owned();
        let stored = self
            .store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let events = events.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    let mut cursor = events
                        .find(doc! { "campaign_id": &campaign_id })
                        .sort(doc! { "sequence": 1_i64 })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("list turn events", error))?;
                    let mut output = Vec::new();
                    while cursor
                        .advance(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("read turn events", error))?
                    {
                        output.push(cursor.deserialize_current().map_err(|error| {
                            PersistenceError::mongo("decode turn event", error)
                        })?);
                    }
                    Ok(output)
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.into_iter().map(stored_turn_event).collect()
    }

    pub async fn record_generated_asset(
        &self,
        asset: &NewGeneratedAssetAudit,
    ) -> Result<(), RepositoryError> {
        validate_generated_asset(asset)?;
        let now = DateTime::now();
        let document = GeneratedAssetDocument {
            id: asset.id.clone(),
            schema_version: STORAGE_SCHEMA_VERSION,
            owner_account_id: asset.owner_account_id.clone(),
            campaign_id: Some(asset.campaign_session_id.clone()),
            entity_kind: asset.entity_kind.clone(),
            entity_id: asset.entity_id.clone(),
            turn_event_id: asset.turn_id.clone(),
            asset_kind: asset.asset_kind.clone(),
            object_key: asset.location.clone(),
            digest: asset.object_digest.as_str().to_owned(),
            provider: asset.provider.clone(),
            model: asset.model.clone(),
            state: asset.state.clone(),
            prompt_fingerprint: asset
                .prompt_fingerprint
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            metadata: asset.metadata.clone(),
            created_at: now,
            updated_at: now,
        };
        let audit = AuditEventDocument {
            id: format!("audit:{}", Uuid::new_v4().simple()),
            schema_version: STORAGE_SCHEMA_VERSION,
            category: "generated_asset".to_owned(),
            action: "asset_recorded".to_owned(),
            outcome: "committed".to_owned(),
            actor_account_id: Some(asset.owner_account_id.clone()),
            scope_kind: "campaign".to_owned(),
            scope_id: asset.campaign_session_id.clone(),
            correlation_id: asset.turn_id.clone(),
            metadata: doc! {
                "asset_id": &asset.id,
                "entity_kind": &asset.entity_kind,
                "entity_id": &asset.entity_id,
            },
            created_at: now,
        };
        let campaigns = self.campaigns();
        let turns = self.turn_events();
        let assets = self.generated_assets();
        let audits = self.audits();
        let actor = asset.owner_account_id.clone();
        let campaign_id = asset.campaign_session_id.clone();
        let turn_id = asset.turn_id.clone();
        let result = self
            .store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let turns = turns.clone();
                let assets = assets.clone();
                let audits = audits.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let turn_id = turn_id.clone();
                let document = document.clone();
                let audit = audit.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    if let Some(turn_id) = turn_id {
                        turns
                            .find_one(doc! { "_id": &turn_id, "campaign_id": &campaign_id })
                            .session(&mut *client_session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("authorize asset turn event", error)
                            })?
                            .ok_or_else(|| PersistenceError::NotFound {
                                entity: "turn event",
                                id: turn_id,
                            })?;
                    }
                    assets
                        .insert_one(document)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("record generated asset", error)
                        })?;
                    audits
                        .insert_one(audit)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("record generated asset audit", error)
                        })?;
                    Ok(())
                })
            })
            .await;
        map_write_result(result, "generated asset", &asset.id)
    }

    pub async fn list_generated_assets(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
    ) -> Result<Vec<GeneratedAssetAudit>, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_opaque("campaign session", campaign_session_id)?;
        let campaigns = self.campaigns();
        let assets = self.generated_assets();
        let actor = actor_account_id.to_owned();
        let campaign_id = campaign_session_id.to_owned();
        let stored = self
            .store
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let assets = assets.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    let mut cursor = assets
                        .find(doc! { "campaign_id": &campaign_id })
                        .sort(doc! { "created_at": 1_i64, "_id": 1_i64 })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("list generated assets", error))?;
                    let mut output = Vec::new();
                    while cursor
                        .advance(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("read generated assets", error))?
                    {
                        output.push(cursor.deserialize_current().map_err(|error| {
                            PersistenceError::mongo("decode generated asset", error)
                        })?);
                    }
                    Ok(output)
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.into_iter().map(stored_generated_asset).collect()
    }

    pub(crate) fn campaigns(&self) -> Collection<CampaignDocument> {
        self.store.collection(CollectionName::Campaigns)
    }

    pub(crate) fn character_instances(&self) -> Collection<CoreCharacterInstanceDocument> {
        self.store
            .collection(CollectionName::CampaignCharacterInstances)
    }

    pub(crate) fn receipts(&self) -> Collection<CommandReceiptDocument> {
        self.store.collection(CollectionName::CommandReceipts)
    }

    pub(crate) fn audits(&self) -> Collection<AuditEventDocument> {
        self.store.collection(CollectionName::AuditEvents)
    }

    fn turn_events(&self) -> Collection<TurnEventDocument> {
        self.store.collection(CollectionName::TurnEvents)
    }

    fn play_sessions(&self) -> Collection<GameplayPlaySessionDocument> {
        self.store.collection(CollectionName::PlaySessions)
    }

    fn encounters(&self) -> Collection<EncounterDocument> {
        self.store.collection(CollectionName::Encounters)
    }

    fn generated_assets(&self) -> Collection<GeneratedAssetDocument> {
        self.store.collection(CollectionName::GeneratedAssets)
    }
}

impl From<MongoStore> for MongoRepository {
    fn from(store: MongoStore) -> Self {
        Self::new(store)
    }
}

#[derive(Default)]
struct CommitMetadata<'a> {
    receipt: Option<&'a NewCommandReceipt>,
    correlation_id: Option<&'a str>,
}

struct CommitUpdates<'a> {
    characters: &'a [CharacterUpdate<'a>],
    hero: Option<EncounterHeroUpdate<'a>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CampaignLifecycleDocument {
    pub(crate) state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) archived_at: Option<DateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CampaignMemberDocument {
    pub(crate) account_id: String,
    pub(crate) role: String,
    pub(crate) state: String,
    pub(crate) inviter_account_id: Option<String>,
    pub(crate) joined_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) left_at: Option<DateTime>,
    pub(crate) created_at: DateTime,
    pub(crate) updated_at: DateTime,
}

impl CampaignMemberDocument {
    #[allow(dead_code)]
    pub(crate) fn active_member(&self) -> bool {
        self.state == "active"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CampaignDocument {
    #[serde(rename = "_id")]
    pub(crate) id: String,
    pub(crate) schema_version: i64,
    pub(crate) revision: i64,
    pub(crate) gameplay_revision: i64,
    pub(crate) lifecycle_revision: i64,
    pub(crate) owner_account_id: String,
    pub(crate) title: String,
    pub(crate) title_normalized: String,
    #[serde(default)]
    pub(crate) theme_id: String,
    pub(crate) lifecycle: CampaignLifecycleDocument,
    pub(crate) members: Vec<CampaignMemberDocument>,
    pub(crate) rules_snapshot: Document,
    #[serde(default)]
    pub(crate) safety_policy_id: String,
    #[serde(default)]
    pub(crate) progression_policy_id: String,
    #[serde(default)]
    pub(crate) retention_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retention_delete_after: Option<DateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current_play_session_id: Option<String>,
    pub(crate) session: SessionDto,
    pub(crate) created_at: DateTime,
    pub(crate) updated_at: DateTime,
}

impl CampaignDocument {
    pub(crate) fn active_member(&self, account_id: &str) -> Option<&CampaignMemberDocument> {
        self.members
            .iter()
            .find(|member| member.account_id == account_id && member.state == "active")
    }

    pub(crate) fn active_game_master(&self, account_id: &str) -> Option<&CampaignMemberDocument> {
        self.active_member(account_id)
            .filter(|member| member.role == "game_master")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct BdeRuntimeDocument {
    pub(super) balance: i32,
    pub(super) lifetime_earned: u64,
    pub(super) lifetime_spent: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CoreCharacterRuntimeDocument {
    character_snapshot: Character,
    bde: BdeRuntimeDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CoreCharacterInstanceDocument {
    #[serde(rename = "_id")]
    pub(super) id: String,
    schema_version: u32,
    revision: u64,
    campaign_id: String,
    account_id: String,
    source_player_character_id: String,
    runtime_kind: String,
    state: String,
    source_snapshot: Document,
    progression: Document,
    runtime: CoreCharacterRuntimeDocument,
    created_at: DateTime,
    updated_at: DateTime,
}

impl CoreCharacterInstanceDocument {
    fn new(account_id: &str, campaign_id: &str, character: Character, now: DateTime) -> Self {
        let id = character.id().to_owned();
        Self {
            id: id.clone(),
            schema_version: STORAGE_SCHEMA_VERSION,
            revision: 1,
            campaign_id: campaign_id.to_owned(),
            account_id: account_id.to_owned(),
            source_player_character_id: id.clone(),
            runtime_kind: "core_character".to_owned(),
            state: "active".to_owned(),
            source_snapshot: doc! {
                "source_kind": "campaign_creation",
                "source_id": &id,
                "source_revision": 1_i64,
            },
            progression: character_progression(&character),
            runtime: CoreCharacterRuntimeDocument {
                character_snapshot: character,
                bde: BdeRuntimeDocument::default(),
            },
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedCharacterUpdate {
    id: String,
    expected_revision: u64,
    character_snapshot: Character,
    progression: Document,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CommandReceiptDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    scope_kind: String,
    scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    campaign_id: Option<String>,
    actor_account_id: String,
    command_kind: String,
    idempotency_key: String,
    request_fingerprint: String,
    state: String,
    expected_revision: u64,
    result_revision: u64,
    audit_id: String,
    response_json: String,
    created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AuditEventDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    category: String,
    action: String,
    outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor_account_id: Option<String>,
    scope_kind: String,
    scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    metadata: Document,
    created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TurnEventDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    campaign_id: String,
    play_session_id: String,
    sequence: u64,
    correlation_id: String,
    actor_account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor_id: Option<String>,
    mode: String,
    phase: String,
    event: SessionEventDto,
    created_at: DateTime,
}

#[derive(Debug, Clone, Deserialize)]
struct GameplayPlaySessionDocument {
    #[serde(rename = "_id")]
    id: String,
    revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncounterDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    campaign_id: String,
    play_session_id: String,
    logical_encounter_id: String,
    revision: u64,
    status: String,
    combatants: Vec<Bson>,
    initiative: Document,
    round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_actor_id: Option<String>,
    state_snapshot: EncounterState,
    created_at: DateTime,
    started_at: DateTime,
    updated_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended_at: Option<DateTime>,
}

#[derive(Debug, Clone)]
struct PreparedEncounterProjection {
    id: String,
    logical_encounter_id: String,
    expected_revision: u64,
    result_revision: u64,
    status: String,
    combatants: Vec<Bson>,
    initiative: Document,
    state: EncounterState,
    created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeneratedAssetDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    owner_account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    campaign_id: Option<String>,
    entity_kind: String,
    entity_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_event_id: Option<String>,
    asset_kind: String,
    object_key: String,
    digest: String,
    provider: String,
    model: String,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_fingerprint: Option<String>,
    metadata: GeneratedAssetMetadata,
    created_at: DateTime,
    updated_at: DateTime,
}

pub(super) fn active_campaign_filter(account_id: &str, campaign_id: &str) -> Document {
    doc! {
        "_id": campaign_id,
        "lifecycle.state": { "$ne": "deleted" },
        "$or": [
            { "owner_account_id": account_id },
            {
                "members": {
                    "$elemMatch": {
                        "account_id": account_id,
                        "state": "active",
                    }
                }
            }
        ],
    }
}

fn campaign_revision_filter(
    account_id: &str,
    campaign_id: &str,
    revision: i64,
    lifecycle_state: &str,
) -> Document {
    let mut filter = active_campaign_filter(account_id, campaign_id);
    filter.insert("gameplay_revision", revision);
    filter.insert("lifecycle.state", lifecycle_state);
    filter
}

pub(super) async fn ensure_campaign_access_in_session(
    campaigns: &Collection<CampaignDocument>,
    client_session: &mut ClientSession,
    actor_account_id: &str,
    campaign_id: &str,
) -> Result<CampaignDocument, PersistenceError> {
    campaigns
        .find_one(active_campaign_filter(actor_account_id, campaign_id))
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("authorize campaign access", error))?
        .ok_or_else(|| PersistenceError::NotFound {
            entity: "campaign",
            id: campaign_id.to_owned(),
        })
}

pub(super) fn map_persistence(error: PersistenceError) -> RepositoryError {
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
        PersistenceError::IdempotencyConflict {
            scope_kind,
            scope_id,
            idempotency_key,
        } => RepositoryError::IdempotencyConflict {
            scope_kind,
            scope_id,
            idempotency_key,
        },
        other => RepositoryError::Persistence(other),
    }
}

pub(super) fn mongo_error(
    operation: &'static str,
    error: mongodb::error::Error,
) -> RepositoryError {
    map_persistence(PersistenceError::mongo(operation, error))
}

pub(super) fn map_write_result<T>(
    result: Result<T, PersistenceError>,
    entity: &'static str,
    id: &str,
) -> Result<T, RepositoryError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if error.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey) => {
            Err(RepositoryError::AlreadyExists {
                entity,
                id: id.to_owned(),
            })
        }
        Err(error) => Err(map_persistence(error)),
    }
}

async fn map_commit_error(
    repository: &MongoRepository,
    error: PersistenceError,
    actor_account_id: &str,
    campaign_id: &str,
    expected_receipt: Option<&CommandReceiptDocument>,
) -> RepositoryError {
    if error.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey)
        && let Some(expected) = expected_receipt
    {
        match repository
            .load_command_receipt(actor_account_id, campaign_id, &expected.idempotency_key)
            .await
        {
            Ok(Some(stored))
                if stored.command_kind == expected.command_kind
                    && stored.request_fingerprint.as_str() == expected.request_fingerprint =>
            {
                return RepositoryError::AlreadyExists {
                    entity: "command receipt",
                    id: expected.id.clone(),
                };
            }
            Ok(Some(_)) => {
                return RepositoryError::IdempotencyConflict {
                    scope_kind: expected.scope_kind.clone(),
                    scope_id: expected.scope_id.clone(),
                    idempotency_key: expected.idempotency_key.clone(),
                };
            }
            Ok(None) => {}
            Err(repository_error) => return repository_error,
        }
    }
    map_persistence(error)
}

fn repository_numeric_as_persistence(error: RepositoryError) -> PersistenceError {
    PersistenceError::SchemaDrift {
        collection: "repository".to_owned(),
        detail: error.to_string(),
    }
}

async fn reject_conflicting_receipt(
    receipts: &Collection<CommandReceiptDocument>,
    client_session: &mut ClientSession,
    expected: &CommandReceiptDocument,
) -> Result<(), PersistenceError> {
    let existing = receipts
        .find_one(doc! {
            "scope_kind": &expected.scope_kind,
            "scope_id": &expected.scope_id,
            "idempotency_key": &expected.idempotency_key,
        })
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("check command receipt", error))?;
    let Some(existing) = existing else {
        return Ok(());
    };
    if existing.actor_account_id != expected.actor_account_id
        || existing.command_kind != expected.command_kind
        || existing.request_fingerprint != expected.request_fingerprint
    {
        return Err(PersistenceError::IdempotencyConflict {
            scope_kind: expected.scope_kind.clone(),
            scope_id: expected.scope_id.clone(),
            idempotency_key: expected.idempotency_key.clone(),
        });
    }
    Err(PersistenceError::AlreadyExists {
        entity: "command receipt",
        id: expected.id.clone(),
    })
}

fn command_receipt_document(
    receipt: &NewCommandReceipt,
) -> Result<CommandReceiptDocument, RepositoryError> {
    validate_command_receipt_fields(receipt)?;
    Ok(CommandReceiptDocument {
        id: format!("receipt:{}", Uuid::new_v4().simple()),
        schema_version: STORAGE_SCHEMA_VERSION,
        scope_kind: "campaign".to_owned(),
        scope_id: receipt.campaign_session_id.clone(),
        campaign_id: Some(receipt.campaign_session_id.clone()),
        actor_account_id: receipt.actor_account_id.clone(),
        command_kind: receipt.command_kind.clone(),
        idempotency_key: receipt.idempotency_key.clone(),
        request_fingerprint: receipt.request_fingerprint.as_str().to_owned(),
        state: "committed".to_owned(),
        expected_revision: receipt.expected_revision,
        result_revision: receipt.result_revision,
        audit_id: receipt.audit_id.clone(),
        response_json: receipt.response_json.clone(),
        created_at: DateTime::now(),
    })
}

fn stored_command_receipt(
    receipt: CommandReceiptDocument,
) -> Result<StoredCommandReceipt, RepositoryError> {
    let request_fingerprint = Sha256Digest::new(receipt.request_fingerprint).map_err(|source| {
        RepositoryError::CoreValidation {
            entity: "command receipt",
            id: receipt.id.clone(),
            source,
        }
    })?;
    let stored = StoredCommandReceipt {
        actor_account_id: receipt.actor_account_id,
        campaign_session_id: receipt.scope_id,
        idempotency_key: receipt.idempotency_key,
        command_kind: receipt.command_kind,
        request_fingerprint,
        expected_revision: receipt.expected_revision,
        result_revision: receipt.result_revision,
        audit_id: receipt.audit_id,
        response_json: receipt.response_json,
        created_at: date_string(receipt.created_at),
    };
    validate_stored_command_receipt(&stored)?;
    Ok(stored)
}

fn stored_session(stored: CampaignDocument) -> Result<StoredDocument<SessionDto>, RepositoryError> {
    let document = StoredDocument {
        id: stored.id,
        schema_version: u32::from(stored.session.schema_version),
        revision: u64::try_from(stored.gameplay_revision).unwrap_or_default(),
        value: stored.session,
        created_at: date_string(stored.created_at),
        updated_at: date_string(stored.updated_at),
    };
    validate_stored_session(document)
}

fn stored_character(
    stored: CoreCharacterInstanceDocument,
) -> Result<StoredDocument<Character>, RepositoryError> {
    let document = StoredDocument {
        id: stored.id,
        schema_version: CHARACTER_SCHEMA_VERSION,
        revision: stored.revision,
        value: stored.runtime.character_snapshot,
        created_at: date_string(stored.created_at),
        updated_at: date_string(stored.updated_at),
    };
    validate_stored_character(document)
}

fn stored_turn_event(
    stored: TurnEventDocument,
) -> Result<TurnAudit<SessionEventDto>, RepositoryError> {
    validate_session_event(&stored.event)?;
    if stored.campaign_id != stored.event.session_id || stored.sequence != stored.event.sequence {
        return invalid(
            "session event",
            &stored.id,
            "turn-event metadata does not match its payload",
        );
    }
    Ok(TurnAudit {
        id: stored.id,
        campaign_session_id: stored.campaign_id,
        turn_number: stored.sequence,
        actor_id: stored.actor_id,
        correlation_id: Some(stored.correlation_id),
        schema_version: u32::from(stored.event.schema_version),
        payload: stored.event,
        created_at: date_string(stored.created_at),
    })
}

fn stored_generated_asset(
    stored: GeneratedAssetDocument,
) -> Result<GeneratedAssetAudit, RepositoryError> {
    let campaign_id = stored
        .campaign_id
        .ok_or_else(|| RepositoryError::InvalidDomainState {
            entity: "generated asset",
            id: stored.id.clone(),
            reason: "campaign asset is missing its campaign partition",
        })?;
    let object_digest =
        Sha256Digest::new(stored.digest).map_err(|source| RepositoryError::CoreValidation {
            entity: "generated asset",
            id: stored.id.clone(),
            source,
        })?;
    let prompt_fingerprint = stored
        .prompt_fingerprint
        .map(Sha256Digest::new)
        .transpose()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "generated asset",
            id: stored.id.clone(),
            source,
        })?;
    let asset = GeneratedAssetAudit {
        id: stored.id,
        owner_account_id: stored.owner_account_id,
        campaign_session_id: campaign_id,
        entity_kind: stored.entity_kind,
        entity_id: stored.entity_id,
        turn_id: stored.turn_event_id,
        asset_kind: stored.asset_kind,
        provider: stored.provider,
        model: stored.model,
        location: stored.object_key,
        object_digest,
        state: stored.state,
        prompt_fingerprint,
        metadata: stored.metadata,
        created_at: date_string(stored.created_at),
    };
    validate_generated_asset_fields(
        &asset.id,
        &asset.owner_account_id,
        &asset.campaign_session_id,
        &asset.entity_kind,
        &asset.entity_id,
        asset.turn_id.as_deref(),
        &asset.asset_kind,
        &asset.provider,
        &asset.model,
        &asset.location,
        &asset.state,
        &asset.metadata,
    )?;
    Ok(asset)
}

fn validate_session_successor(
    current: &StoredDocument<SessionDto>,
    submitted: &SessionDto,
    expected_revision: u64,
    event: &SessionEventDto,
    audit_id: &str,
) -> Result<(), RepositoryError> {
    if current.revision != expected_revision {
        return Err(RepositoryError::RevisionConflict {
            entity: "campaign session",
            id: submitted.id.clone(),
            expected: expected_revision,
            actual: current.revision,
        });
    }
    let expected_sequence =
        current
            .value
            .last_event_sequence
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange {
                field: "event sequence",
            })?;
    if event.sequence != expected_sequence {
        return invalid(
            "session event",
            audit_id,
            "event sequence must immediately follow the stored session sequence",
        );
    }
    if submitted.ruleset != current.value.ruleset
        || submitted.created_at_unix_ms != current.value.created_at_unix_ms
        || submitted.title != current.value.title
        || submitted.character_ids != current.value.character_ids
    {
        return invalid(
            "campaign session",
            &submitted.id,
            "turn events cannot rewrite campaign identity, rules, or roster",
        );
    }
    let valid_status_transition = match event.payload {
        SessionEventPayload::SessionEnded => {
            current.value.status == SessionStatus::Active
                && submitted.status == SessionStatus::Completed
        }
        _ => {
            current.value.status == SessionStatus::Active
                && submitted.status == current.value.status
        }
    };
    if !valid_status_transition {
        return invalid(
            "campaign session",
            &submitted.id,
            "session status transition is invalid for this event",
        );
    }
    if event.occurred_at_unix_ms < current.value.updated_at_unix_ms
        || submitted.updated_at_unix_ms < current.value.updated_at_unix_ms
        || submitted.updated_at_unix_ms < event.occurred_at_unix_ms
    {
        return invalid(
            "campaign session",
            &submitted.id,
            "event and session timestamps must advance monotonically",
        );
    }
    Ok(())
}

fn validate_character_successor(
    current: &StoredDocument<Character>,
    update: &CharacterUpdate<'_>,
    event: &SessionEventDto,
) -> Result<(), RepositoryError> {
    if current.revision != update.expected_revision {
        return Err(RepositoryError::RevisionConflict {
            entity: "character",
            id: update.character.id().to_owned(),
            expected: update.expected_revision,
            actual: current.revision,
        });
    }
    if let SessionEventPayload::ExperienceAwarded {
        character_id,
        summary,
    } = &event.payload
    {
        let mut expected_character = current.value.clone();
        let expected_summary = expected_character
            .award_experience(summary.awarded)
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "character",
                id: update.character.id().to_owned(),
                source,
            })?;
        if character_id != update.character.id()
            || &expected_summary != summary
            || &expected_character != update.character
        {
            return invalid(
                "character",
                update.character.id(),
                "character snapshot does not match the audited XP award",
            );
        }
    }
    Ok(())
}

fn character_progression(character: &Character) -> Document {
    doc! {
        "kind": "core_character",
        "experience_points": i64::from(character.experience_points()),
    }
}

fn validate_command_receipt_lookup(
    actor_account_id: &str,
    campaign_session_id: &str,
    idempotency_key: &str,
) -> Result<(), RepositoryError> {
    validate_account_id(actor_account_id)?;
    if !is_valid_opaque_id(campaign_session_id) || !is_valid_opaque_id(idempotency_key) {
        return invalid(
            "command receipt",
            &format!("{campaign_session_id}:{idempotency_key}"),
            "campaign and idempotency identifiers must be valid",
        );
    }
    Ok(())
}

fn validate_command_receipt_fields(receipt: &NewCommandReceipt) -> Result<(), RepositoryError> {
    validate_command_receipt_lookup(
        &receipt.actor_account_id,
        &receipt.campaign_session_id,
        &receipt.idempotency_key,
    )?;
    if !is_valid_opaque_id(&receipt.command_kind) || !is_valid_opaque_id(&receipt.audit_id) {
        return invalid(
            "command receipt",
            &receipt.idempotency_key,
            "command kind and audit identifiers must be valid",
        );
    }
    Sha256Digest::new(receipt.request_fingerprint.as_str()).map_err(|source| {
        RepositoryError::CoreValidation {
            entity: "command receipt",
            id: receipt.idempotency_key.clone(),
            source,
        }
    })?;
    let expected_result_revision = receipt
        .expected_revision
        .checked_add(1)
        .ok_or(RepositoryError::NumericRange { field: "revision" })?;
    if receipt.expected_revision == 0 || receipt.result_revision != expected_result_revision {
        return invalid(
            "command receipt",
            &receipt.idempotency_key,
            "result revision must immediately follow a positive expected revision",
        );
    }
    if receipt.response_json.is_empty()
        || receipt.response_json.len() > MAX_COMMAND_RESPONSE_JSON_BYTES
        || serde_json::from_str::<serde_json::Value>(&receipt.response_json).is_err()
    {
        return invalid(
            "command receipt",
            &receipt.idempotency_key,
            "response must be bounded valid JSON",
        );
    }
    Ok(())
}

fn validate_stored_command_receipt(receipt: &StoredCommandReceipt) -> Result<(), RepositoryError> {
    validate_command_receipt_lookup(
        &receipt.actor_account_id,
        &receipt.campaign_session_id,
        &receipt.idempotency_key,
    )?;
    if receipt.result_revision != receipt.expected_revision.saturating_add(1)
        || receipt.response_json.is_empty()
        || receipt.response_json.len() > MAX_COMMAND_RESPONSE_JSON_BYTES
        || serde_json::from_str::<serde_json::Value>(&receipt.response_json).is_err()
    {
        return invalid(
            "command receipt",
            &receipt.idempotency_key,
            "stored receipt is invalid",
        );
    }
    Ok(())
}

fn validate_command_receipt_for_commit(
    receipt: &NewCommandReceipt,
    audit_id: &str,
    session: &SessionDto,
    expected_revision: u64,
    event: &SessionEventDto,
) -> Result<(), RepositoryError> {
    validate_command_receipt_fields(receipt)?;
    if receipt.campaign_session_id != session.id
        || receipt.campaign_session_id != event.session_id
        || receipt.audit_id != audit_id
        || receipt.expected_revision != expected_revision
    {
        return invalid(
            "command receipt",
            &receipt.idempotency_key,
            "receipt must identify the committed campaign, audit, and revisions",
        );
    }
    Ok(())
}

fn validate_session(session: &SessionDto) -> Result<(), RepositoryError> {
    if session.schema_version != SESSION_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign session",
            found: u32::from(session.schema_version),
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    session
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "campaign session",
            id: session.id.clone(),
            source,
        })
}

fn validate_initial_roster(
    session: &SessionDto,
    characters: &[Character],
) -> Result<(), RepositoryError> {
    let mut supplied_ids = std::collections::BTreeSet::new();
    for character in characters {
        character
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "character",
                id: character.id().to_owned(),
                source,
            })?;
        if !supplied_ids.insert(character.id()) {
            return invalid(
                "campaign session",
                &session.id,
                "initial character snapshots must have unique ids",
            );
        }
    }
    let declared_ids = session
        .character_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if declared_ids != supplied_ids {
        return invalid(
            "campaign session",
            &session.id,
            "declared party ids must exactly match initial character snapshots",
        );
    }
    Ok(())
}

fn validate_stored_session(
    stored: StoredDocument<SessionDto>,
) -> Result<StoredDocument<SessionDto>, RepositoryError> {
    if stored.schema_version != u32::from(SESSION_SCHEMA_VERSION) {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign session",
            found: stored.schema_version,
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    if stored.id != stored.value.id {
        return Err(RepositoryError::IdentityMismatch {
            entity: "campaign session",
            row_id: stored.id,
            payload_id: stored.value.id,
        });
    }
    validate_session(&stored.value)?;
    Ok(stored)
}

fn validate_stored_character(
    stored: StoredDocument<Character>,
) -> Result<StoredDocument<Character>, RepositoryError> {
    if stored.schema_version != CHARACTER_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "character",
            found: stored.schema_version,
            supported: CHARACTER_SCHEMA_VERSION,
        });
    }
    if stored.id != stored.value.id() {
        return Err(RepositoryError::IdentityMismatch {
            entity: "character",
            row_id: stored.id,
            payload_id: stored.value.id().to_owned(),
        });
    }
    stored
        .value
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "character",
            id: stored.id.clone(),
            source,
        })?;
    Ok(stored)
}

fn validate_session_event(event: &SessionEventDto) -> Result<(), RepositoryError> {
    if event.schema_version != SESSION_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "session event",
            found: u32::from(event.schema_version),
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    event
        .validate()
        .map_err(|source| RepositoryError::CoreValidation {
            entity: "session event",
            id: format!("{}:{}", event.session_id, event.sequence),
            source,
        })
}

fn event_references_unknown_character(session: &SessionDto, event: &SessionEventDto) -> bool {
    let known = |id: &str| session.character_ids.iter().any(|known| known == id);
    match &event.actor {
        EventActor::Player { character_id } if !known(character_id) => return true,
        EventActor::Player { .. } | EventActor::AiGameMaster | EventActor::System => {}
    }
    match &event.payload {
        SessionEventPayload::PlayerIntent { character_id, .. }
        | SessionEventPayload::AbilityCheckResolved { character_id, .. }
        | SessionEventPayload::ExplorationSocialResolved {
            command: manchester_dnd_core::AttemptSocialInteractionCommand { character_id, .. },
            ..
        }
        | SessionEventPayload::ExperienceAwarded { character_id, .. } => !known(character_id),
        _ => false,
    }
}

fn validate_character_update_set(
    session: &SessionDto,
    event: &SessionEventDto,
    updates: &[CharacterUpdate<'_>],
) -> Result<(), RepositoryError> {
    let mut ids = std::collections::BTreeSet::new();
    for update in updates {
        update
            .character
            .validate()
            .map_err(|source| RepositoryError::CoreValidation {
                entity: "character",
                id: update.character.id().to_owned(),
                source,
            })?;
        if !ids.insert(update.character.id())
            || !session
                .character_ids
                .iter()
                .any(|id| id == update.character.id())
        {
            return invalid(
                "session event",
                &format!("{}:{}", event.session_id, event.sequence),
                "character updates must be unique members of the campaign",
            );
        }
    }
    match &event.payload {
        SessionEventPayload::ExperienceAwarded { character_id, .. }
            if updates.len() == 1 && updates[0].character.id() == character_id =>
        {
            Ok(())
        }
        SessionEventPayload::ExperienceAwarded { .. } => invalid(
            "session event",
            &format!("{}:{}", event.session_id, event.sequence),
            "an XP event requires exactly one matching character update",
        ),
        _ if updates.is_empty() => Ok(()),
        _ => invalid(
            "session event",
            &format!("{}:{}", event.session_id, event.sequence),
            "this event type cannot mutate a character",
        ),
    }
}

fn validate_generated_asset(asset: &NewGeneratedAssetAudit) -> Result<(), RepositoryError> {
    Sha256Digest::new(asset.object_digest.as_str()).map_err(|source| {
        RepositoryError::CoreValidation {
            entity: "generated asset",
            id: asset.id.clone(),
            source,
        }
    })?;
    validate_generated_asset_fields(
        &asset.id,
        &asset.owner_account_id,
        &asset.campaign_session_id,
        &asset.entity_kind,
        &asset.entity_id,
        asset.turn_id.as_deref(),
        &asset.asset_kind,
        &asset.provider,
        &asset.model,
        &asset.location,
        &asset.state,
        &asset.metadata,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_generated_asset_fields(
    id: &str,
    owner_account_id: &str,
    campaign_session_id: &str,
    entity_kind: &str,
    entity_id: &str,
    turn_id: Option<&str>,
    asset_kind: &str,
    provider: &str,
    model: &str,
    location: &str,
    state: &str,
    metadata: &GeneratedAssetMetadata,
) -> Result<(), RepositoryError> {
    let bounded =
        |value: &str, maximum: usize| !value.trim().is_empty() && value.chars().count() <= maximum;
    if !is_valid_opaque_id(id)
        || !is_valid_opaque_id(owner_account_id)
        || !is_valid_opaque_id(campaign_session_id)
        || !is_valid_opaque_id(entity_kind)
        || !is_valid_opaque_id(entity_id)
        || turn_id.is_some_and(|id| !is_valid_opaque_id(id))
        || !is_valid_opaque_id(asset_kind)
        || !is_valid_opaque_id(provider)
        || !matches!(state, "pending" | "published" | "selected" | "retired")
        || !bounded(model, 256)
        || !valid_asset_key(location)
        || metadata
            .media_type
            .as_ref()
            .is_some_and(|media_type| !valid_media_type(media_type))
        || metadata
            .provider_request_id
            .as_ref()
            .is_some_and(|id| !is_valid_opaque_id(id))
        || !matches!(
            (metadata.width, metadata.height),
            (None, None) | (Some(1..=32_768), Some(1..=32_768))
        )
    {
        return invalid(
            "generated asset",
            id,
            "asset identity, authorization, state, key, or media metadata is invalid",
        );
    }
    Ok(())
}

fn valid_asset_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2_048
        && !value.starts_with('/')
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn valid_media_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.contains('/')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'/' | b'+' | b'-' | b'.')
        })
}

fn prepare_encounter_projection(
    campaign_id: &str,
    event: &SessionEventDto,
    created_at: DateTime,
) -> Result<Option<PreparedEncounterProjection>, RepositoryError> {
    let SessionEventPayload::EncounterResolved { outcome, .. } = &event.payload else {
        return Ok(None);
    };
    let resolution = &outcome.resolution;
    to_i64(resolution.previous_revision, "encounter revision")?;
    to_i64(resolution.result_revision, "encounter revision")?;
    let state = resolution.state.clone();
    let combatants = vec![
        mongodb::bson::to_bson(&state.hero).map_err(PersistenceError::BsonEncoding)?,
        mongodb::bson::to_bson(&state.creature).map_err(PersistenceError::BsonEncoding)?,
    ];
    let initiative = doc! {
        "snapshot": mongodb::bson::to_bson(&state.initiative)
            .map_err(PersistenceError::BsonEncoding)?,
    };
    Ok(Some(PreparedEncounterProjection {
        id: encounter_instance_id(campaign_id, &resolution.encounter_id),
        logical_encounter_id: resolution.encounter_id.clone(),
        expected_revision: resolution.previous_revision,
        result_revision: resolution.result_revision,
        status: encounter_status(state.status).to_owned(),
        combatants,
        initiative,
        state,
        created_at,
    }))
}

async fn commit_encounter_projection(
    encounters: &Collection<EncounterDocument>,
    client_session: &mut ClientSession,
    campaign_id: &str,
    play_session_id: &str,
    update: PreparedEncounterProjection,
) -> Result<(), PersistenceError> {
    let existing = encounters
        .find_one(doc! {
            "_id": &update.id,
            "campaign_id": campaign_id,
            "play_session_id": play_session_id,
        })
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("load encounter projection", error))?;
    if let Some(existing) = existing {
        if existing.logical_encounter_id != update.logical_encounter_id
            || existing.revision != update.expected_revision
        {
            return Err(PersistenceError::RevisionConflict {
                entity: "encounter",
                id: update.logical_encounter_id,
                expected: update.expected_revision,
                actual: existing.revision,
            });
        }
        let mut fields = doc! {
            "revision": i64::try_from(update.result_revision).map_err(|_| {
                PersistenceError::SchemaDrift {
                    collection: CollectionName::Encounters.as_str().to_owned(),
                    detail: "encounter revision exceeds BSON range".to_owned(),
                }
            })?,
            "status": &update.status,
            "combatants": update.combatants,
            "initiative": update.initiative,
            "round": i64::from(update.state.round),
            "state_snapshot": mongodb::bson::to_bson(&update.state)
                .map_err(PersistenceError::BsonEncoding)?,
            "updated_at": update.created_at,
        };
        match &update.state.current_actor_id {
            Some(actor_id) => {
                fields.insert("current_actor_id", actor_id);
            }
            None => {
                fields.insert("current_actor_id", Bson::Null);
            }
        }
        if matches!(
            update.state.status,
            EncounterStatus::Victory | EncounterStatus::Defeat
        ) {
            fields.insert("ended_at", update.created_at);
        }
        let result = encounters
            .update_one(
                doc! {
                    "_id": &update.id,
                    "campaign_id": campaign_id,
                    "play_session_id": play_session_id,
                    "revision": i64::try_from(update.expected_revision).map_err(|_| {
                        PersistenceError::SchemaDrift {
                            collection: CollectionName::Encounters.as_str().to_owned(),
                            detail: "encounter revision exceeds BSON range".to_owned(),
                        }
                    })?,
                },
                doc! { "$set": fields },
            )
            .session(&mut *client_session)
            .await
            .map_err(|error| PersistenceError::mongo("advance encounter projection", error))?;
        if result.modified_count != 1 {
            return Err(PersistenceError::RevisionConflict {
                entity: "encounter",
                id: update.logical_encounter_id,
                expected: update.expected_revision,
                actual: existing.revision,
            });
        }
        return Ok(());
    }
    if update.expected_revision != 1 {
        return Err(PersistenceError::NotFound {
            entity: "encounter",
            id: update.logical_encounter_id,
        });
    }
    let terminal = matches!(
        update.state.status,
        EncounterStatus::Victory | EncounterStatus::Defeat
    );
    encounters
        .insert_one(EncounterDocument {
            id: update.id,
            schema_version: STORAGE_SCHEMA_VERSION,
            campaign_id: campaign_id.to_owned(),
            play_session_id: play_session_id.to_owned(),
            logical_encounter_id: update.logical_encounter_id,
            revision: update.result_revision,
            status: update.status,
            combatants: update.combatants,
            initiative: update.initiative,
            round: update.state.round,
            current_actor_id: update.state.current_actor_id.clone(),
            state_snapshot: update.state,
            created_at: update.created_at,
            started_at: update.created_at,
            updated_at: update.created_at,
            ended_at: terminal.then_some(update.created_at),
        })
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("create encounter projection", error))?;
    Ok(())
}

fn encounter_instance_id(campaign_id: &str, logical_encounter_id: &str) -> String {
    let identity = format!("{campaign_id}:{logical_encounter_id}");
    format!(
        "encounter:{}",
        Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes()).simple()
    )
}

const fn encounter_status(status: EncounterStatus) -> &'static str {
    match status {
        EncounterStatus::Ready => "ready",
        EncounterStatus::Active => "active",
        EncounterStatus::Victory => "victory",
        EncounterStatus::Defeat => "defeat",
    }
}

fn event_action(event: &SessionEventDto) -> &'static str {
    match event.payload {
        SessionEventPayload::SessionStarted => "session_started",
        SessionEventPayload::PlayerIntent { .. } => "player_intent",
        SessionEventPayload::DiceResolved { .. } => "dice_resolved",
        SessionEventPayload::AbilityCheckResolved { .. } => "ability_check_resolved",
        SessionEventPayload::ExplorationSocialResolved { .. } => "exploration_social_resolved",
        SessionEventPayload::EncounterResolved { .. } => "encounter_resolved",
        SessionEventPayload::GmNarration { .. } => "gm_narration",
        SessionEventPayload::ExperienceAwarded { .. } => "experience_awarded",
        SessionEventPayload::AiProposalAccepted { .. } => "ai_proposal_accepted",
        SessionEventPayload::AiProposalRejected { .. } => "ai_proposal_rejected",
        SessionEventPayload::SessionEnded => "session_ended",
    }
}

fn event_mode(event: &SessionEventDto) -> &'static str {
    match event.payload {
        SessionEventPayload::EncounterResolved { .. } => "battle",
        _ => "exploration",
    }
}

pub(super) fn date_string(value: DateTime) -> String {
    value.to_string()
}

pub(super) fn to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

pub(super) fn validate_account_id(account_id: &str) -> Result<(), RepositoryError> {
    validate_opaque("account", account_id)
}

pub(super) fn validate_opaque(entity: &'static str, id: &str) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(id) {
        return invalid(entity, id, "identifier must be a valid opaque identifier");
    }
    Ok(())
}

fn normalize_title(value: &str) -> String {
    value.trim().to_lowercase()
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

    use uuid::Uuid;

    use super::*;
    use crate::{
        config::{MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::SchemaReconciler,
    };

    #[test]
    fn generated_asset_key_rejects_traversal_and_absolute_paths() {
        assert!(valid_asset_key("campaign/asset.webp"));
        assert!(!valid_asset_key("../asset.webp"));
        assert!(!valid_asset_key("/campaign/asset.webp"));
    }

    #[test]
    fn campaign_scope_filter_binds_active_membership() {
        let filter = active_campaign_filter("account:test", "campaign:test");
        assert_eq!(filter.get_str("_id"), Ok("campaign:test"));
        assert!(filter.contains_key("$or"));
    }

    #[tokio::test]
    async fn live_campaign_create_is_transactional_and_tenant_scoped() {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping Mongo repository contract: MONGODB_TEST_URI is not set");
            return;
        };
        assert!(
            !uri.trim().is_empty(),
            "MONGODB_TEST_URI must not be empty when set"
        );
        let suffix = Uuid::new_v4().simple().to_string();
        let database = format!("mdnd_repository_test_{suffix}");
        let store = MongoStore::connect(&MongoConfig {
            uri: SecretString::new(uri),
            database: database.clone(),
            max_pool_size: 4,
            min_pool_size: 0,
            connect_timeout: Duration::from_secs(5),
            server_selection_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(15),
            transaction_timeout: Duration::from_secs(10),
            transaction_max_retries: 2,
            schema_policy: MongoSchemaPolicy::ApplyAndVerify,
        })
        .await
        .unwrap();
        let repository = MongoRepository::new(store.clone());
        let owner = format!("account:{suffix}");
        let foreign = format!("account:foreign-{suffix}");
        let campaign_id = format!("campaign:{suffix}");
        let session = SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: campaign_id.clone(),
            ruleset: manchester_dnd_core::RULESET,
            title: "Mongo contract campaign".to_owned(),
            status: SessionStatus::Active,
            character_ids: Vec::new(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
            last_event_sequence: 0,
        };
        let outcome = async {
            SchemaReconciler::new(store.clone())
                .apply()
                .await
                .map_err(|error| error.to_string())?;
            repository
                .create_campaign(&owner, &session, &[])
                .await
                .map_err(|error| error.to_string())?;
            let owned = repository
                .load_campaign_session(&owner, &campaign_id)
                .await
                .map_err(|error| error.to_string())?;
            if owned.is_none() {
                return Err("owner could not reload committed campaign".to_owned());
            }
            let hidden = repository
                .load_campaign_session(&foreign, &campaign_id)
                .await
                .map_err(|error| error.to_string())?;
            if hidden.is_some() {
                return Err("foreign account could read campaign".to_owned());
            }
            Ok::<(), String>(())
        }
        .await;

        assert!(
            database.starts_with("mdnd_repository_test_") && database != "manchester_dnd",
            "cleanup safeguard"
        );
        store.database().drop().await.unwrap();
        outcome.unwrap();
    }
}
