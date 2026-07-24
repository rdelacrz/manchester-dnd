use manchester_dnd_core::{
    RewardTier, SealedCampaignPins, SessionEventDto, SessionEventPayload, Sha256Digest,
    encounter::ENCOUNTER_SCHEMA_VERSION,
    hero::{
        CharacterCreatedAuditDto, HERO_AUDIT_SCHEMA_VERSION, HERO_CHARACTER_SCHEMA_VERSION,
        HERO_DRAFT_SCHEMA_VERSION, HeroCharacter, HeroCreationCommand, HeroCreationDraft,
        HeroCreationTransitionAuditDto, LevelUpAuditDto, LevelUpCommand, RewardAwardAuditDto,
        RewardAwardCommand, SupportedLevel, TrustedMutationContext, TrustedRewardPolicy,
    },
    is_valid_opaque_id,
    rules_matrix::RuntimeResources,
};
use mongodb::{
    ClientSession, Collection,
    bson::{Bson, DateTime, Document, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    AuditEventDocument, BdeRuntimeDocument, CommandReceiptDocument, MongoRepository, SaveOutcome,
    StoredDocument, active_campaign_filter, date_string, ensure_campaign_access_in_session,
    map_persistence, map_write_result, mongo_error,
    pins::{seal_campaign_pins_in_transaction, validate_seal},
    validate_account_id, validate_opaque,
};
use crate::{
    error::{MongoFailureKind, PersistenceError, RepositoryError},
    persistence::CollectionName,
};

const STORAGE_SCHEMA_VERSION: u32 = 1;
const HERO_RECEIPT_RESPONSE_MAX_BYTES: usize = 128 * 1024;
const CREATION_COMMAND_KIND: &str = "hero_creation_transition";
const REWARD_COMMAND_KIND: &str = "hero_reward";
const LEVEL_UP_COMMAND_KIND: &str = "hero_level_up";
const ENCOUNTER_REWARD_COMMAND_KIND: &str = "encounter_reward_claim";

#[derive(Debug, Clone, Copy)]
pub(crate) struct EncounterHeroUpdate<'a> {
    pub(crate) character: &'a HeroCharacter,
    pub(crate) expected_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeroReceiptScope {
    Draft,
    Character,
}

impl HeroReceiptScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "hero_draft",
            Self::Character => "campaign_character_instance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewHeroCommandReceipt {
    pub(crate) actor_account_id: String,
    pub(crate) scope: HeroReceiptScope,
    pub(crate) scope_id: String,
    pub(crate) campaign_session_id: String,
    pub(crate) idempotency_key: String,
    pub(crate) command_kind: String,
    pub(crate) request_fingerprint: Sha256Digest,
    pub(crate) expected_revision: u64,
    pub(crate) result_revision: u64,
    pub(crate) audit_id: String,
    pub(crate) response_json: String,
}

pub(crate) struct HeroCreationCommitMetadata<'a> {
    pub(crate) receipt: &'a NewHeroCommandReceipt,
    pub(crate) campaign_pins: Option<&'a SealedCampaignPins>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredHeroCommandReceipt {
    pub(crate) actor_account_id: String,
    pub(crate) scope: HeroReceiptScope,
    pub(crate) scope_id: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HeroAuditPayload {
    CreationTransition {
        transition: Box<HeroCreationTransitionAuditDto>,
        character_created: Option<Box<CharacterCreatedAuditDto>>,
    },
    RewardAwarded {
        reward: RewardAwardAuditDto,
    },
    LevelUp {
        level_up: LevelUpAuditDto,
    },
}

impl HeroAuditPayload {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        match self {
            Self::CreationTransition {
                transition,
                character_created,
            } => {
                transition.validate().map_err(|source| {
                    hero_validation("hero audit", &transition.audit_id, source)
                })?;
                if let Some(created) = character_created {
                    created.validate().map_err(|source| {
                        hero_validation("hero audit", &transition.audit_id, source)
                    })?;
                    if created.audit_id != transition.audit_id
                        || created.actor_id != transition.actor_id
                        || created.draft_id != transition.draft_id
                        || created.draft_revision != transition.revision_after
                    {
                        return invalid(
                            "hero audit",
                            &transition.audit_id,
                            "created-character facts must match the creation transition",
                        );
                    }
                }
                Ok(())
            }
            Self::RewardAwarded { reward } => reward
                .validate()
                .map_err(|source| hero_validation("hero audit", &reward.audit_id, source)),
            Self::LevelUp { level_up } => level_up
                .validate()
                .map_err(|source| hero_validation("hero audit", &level_up.audit_id, source)),
        }
    }

    pub fn audit_id(&self) -> &str {
        match self {
            Self::CreationTransition { transition, .. } => &transition.audit_id,
            Self::RewardAwarded { reward } => &reward.audit_id,
            Self::LevelUp { level_up } => &level_up.audit_id,
        }
    }

    pub fn subject_id(&self) -> &str {
        match self {
            Self::CreationTransition { transition, .. } => &transition.draft_id,
            Self::RewardAwarded { reward } => &reward.character_id,
            Self::LevelUp { level_up } => &level_up.character_id,
        }
    }

    const fn subject_kind(&self) -> HeroReceiptScope {
        match self {
            Self::CreationTransition { .. } => HeroReceiptScope::Draft,
            Self::RewardAwarded { .. } | Self::LevelUp { .. } => HeroReceiptScope::Character,
        }
    }

    fn subject_revision(&self) -> u64 {
        match self {
            Self::CreationTransition { transition, .. } => transition.revision_after,
            Self::RewardAwarded { reward } => reward.revision_after,
            Self::LevelUp { level_up } => level_up.revision_after,
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::CreationTransition { .. } => "creation_transition",
            Self::RewardAwarded { .. } => "reward_awarded",
            Self::LevelUp { .. } => "level_up",
        }
    }

    fn occurred_at_epoch_seconds(&self) -> u64 {
        match self {
            Self::CreationTransition { transition, .. } => transition.occurred_at_epoch_seconds,
            Self::RewardAwarded { reward } => reward.occurred_at_epoch_seconds,
            Self::LevelUp { level_up } => level_up.occurred_at_epoch_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHeroAudit {
    pub id: String,
    pub campaign_session_id: String,
    pub subject_id: String,
    pub subject_revision: u64,
    pub schema_version: u32,
    pub payload: HeroAuditPayload,
    pub occurred_at_epoch_seconds: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeroMutationCommitOutcome {
    pub(crate) subject: SaveOutcome,
    pub(crate) created_character: Option<SaveOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewEncounterRewardClaim {
    pub(crate) campaign_session_id: String,
    pub(crate) encounter_id: String,
    pub(crate) character_id: String,
    pub(crate) encounter_revision: u64,
    pub(crate) victory_event_sequence: u64,
    pub(crate) reward_tier: RewardTier,
    pub(crate) experience_awarded: u32,
    pub(crate) hero_audit_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredEncounterRewardClaim {
    pub(crate) campaign_session_id: String,
    pub(crate) encounter_id: String,
    pub(crate) character_id: String,
    pub(crate) encounter_revision: u64,
    pub(crate) victory_event_sequence: u64,
    pub(crate) reward_tier: RewardTier,
    pub(crate) experience_awarded: u32,
    pub(crate) hero_audit_id: String,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HeroCharacterMutationCommand<'a> {
    Reward(&'a RewardAwardCommand),
    EncounterReward {
        reward: &'a RewardAwardCommand,
        claim: &'a NewEncounterRewardClaim,
    },
    LevelUp(&'a LevelUpCommand),
}

impl HeroCharacterMutationCommand<'_> {
    const fn command_kind(self) -> &'static str {
        match self {
            Self::Reward(_) => REWARD_COMMAND_KIND,
            Self::EncounterReward { .. } => ENCOUNTER_REWARD_COMMAND_KIND,
            Self::LevelUp(_) => LEVEL_UP_COMMAND_KIND,
        }
    }
}

impl MongoRepository {
    pub(crate) async fn create_hero_draft(
        &self,
        actor_account_id: &str,
        draft: &HeroCreationDraft,
        retention_delete_after_epoch_seconds: u64,
    ) -> Result<SaveOutcome, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_draft(draft)?;
        if draft.owner_id != actor_account_id
            || retention_delete_after_epoch_seconds < draft.expires_at_epoch_seconds
        {
            return invalid(
                "hero creation draft",
                &draft.draft_id,
                "actor ownership and retention deadline must be valid",
            );
        }
        let now = DateTime::now();
        let document = HeroDraftDocument {
            id: draft.draft_id.clone(),
            schema_version: STORAGE_SCHEMA_VERSION,
            revision: durable_revision(draft.revision, "hero draft revision")?,
            owner_account_id: actor_account_id.to_owned(),
            campaign_id: draft.campaign_id.clone(),
            step: draft.revision,
            state: draft_state(draft).to_owned(),
            expires_at: date_from_epoch_seconds(
                draft.expires_at_epoch_seconds,
                "hero draft expiry",
            )?,
            purge_at: date_from_epoch_seconds(
                retention_delete_after_epoch_seconds,
                "hero draft retention",
            )?,
            draft: draft.clone(),
            created_at: now,
            updated_at: now,
        };
        let campaigns = self.campaigns();
        let drafts = self.hero_drafts();
        let actor = actor_account_id.to_owned();
        let campaign_id = draft.campaign_id.clone();
        let result = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let drafts = drafts.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let document = document.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    drafts
                        .insert_one(document)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("create hero draft", error))?;
                    Ok(())
                })
            })
            .await;
        map_write_result(result, "hero creation draft", &draft.draft_id)?;
        Ok(SaveOutcome {
            revision: durable_revision(draft.revision, "hero draft revision")?,
            updated_at: date_string(now),
        })
    }

    pub async fn load_hero_draft(
        &self,
        actor_account_id: &str,
        id: &str,
    ) -> Result<Option<StoredDocument<HeroCreationDraft>>, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_opaque("hero creation draft", id)?;
        let campaigns = self.campaigns();
        let drafts = self.hero_drafts();
        let actor = actor_account_id.to_owned();
        let draft_id = id.to_owned();
        self.store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let drafts = drafts.clone();
                let actor = actor.clone();
                let draft_id = draft_id.clone();
                Box::pin(async move {
                    let Some(stored) = drafts
                        .find_one(doc! { "_id": &draft_id, "owner_account_id": &actor })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load hero draft", error))?
                    else {
                        return Ok(None);
                    };
                    let authorized = campaigns
                        .find_one(active_campaign_filter(&actor, &stored.campaign_id))
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("authorize hero draft", error))?
                        .is_some();
                    Ok(authorized.then_some(stored))
                })
            })
            .await
            .map_err(map_persistence)?
            .map(stored_draft)
            .transpose()
    }

    pub async fn load_hero_character(
        &self,
        actor_account_id: &str,
        id: &str,
    ) -> Result<Option<StoredDocument<HeroCharacter>>, RepositoryError> {
        validate_account_id(actor_account_id)?;
        validate_opaque("hero character", id)?;
        let campaigns = self.campaigns();
        let heroes = self.hero_instances();
        let actor = actor_account_id.to_owned();
        let character_id = id.to_owned();
        self.store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let heroes = heroes.clone();
                let actor = actor.clone();
                let character_id = character_id.clone();
                Box::pin(async move {
                    let Some(stored) = heroes
                        .find_one(doc! {
                            "_id": &character_id,
                            "account_id": &actor,
                            "state": "active",
                            "runtime_kind": "hero_character",
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load hero character", error))?
                    else {
                        return Ok(None);
                    };
                    let authorized = campaigns
                        .find_one(active_campaign_filter(&actor, &stored.campaign_id))
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("authorize hero character", error)
                        })?
                        .is_some();
                    Ok(authorized.then_some(stored))
                })
            })
            .await
            .map_err(map_persistence)?
            .map(stored_hero)
            .transpose()
    }

    pub async fn load_latest_hero_draft_for_owner(
        &self,
        campaign_session_id: &str,
        owner_key: &str,
        now_epoch_seconds: u64,
    ) -> Result<Option<StoredDocument<HeroCreationDraft>>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, owner_key)?;
        let now = date_from_epoch_seconds(now_epoch_seconds, "hero draft lookup time")?;
        let campaigns = self.campaigns();
        let drafts = self.hero_drafts();
        let campaign = campaign_session_id.to_owned();
        let owner = owner_key.to_owned();
        let stored = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let drafts = drafts.clone();
                let campaign = campaign.clone();
                let owner = owner.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &owner,
                        &campaign,
                    )
                    .await?;
                    drafts
                        .find_one(doc! {
                            "campaign_id": &campaign,
                            "owner_account_id": &owner,
                            "expires_at": { "$gte": now },
                        })
                        .sort(doc! { "updated_at": -1_i64, "_id": -1_i64 })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load latest hero draft", error))
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.map(stored_draft).transpose()
    }

    pub(crate) async fn load_latest_pinned_hero_draft_for_owner(
        &self,
        campaign_session_id: &str,
        owner_key: &str,
    ) -> Result<Option<StoredDocument<HeroCreationDraft>>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, owner_key)?;
        let campaigns = self.campaigns();
        let drafts = self.hero_drafts();
        let campaign = campaign_session_id.to_owned();
        let owner = owner_key.to_owned();
        let stored = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let drafts = drafts.clone();
                let campaign = campaign.clone();
                let owner = owner.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &owner,
                        &campaign,
                    )
                    .await?;
                    drafts
                        .find_one(doc! {
                            "campaign_id": &campaign,
                            "owner_account_id": &owner,
                            "draft.pins": { "$ne": Bson::Null },
                        })
                        .sort(doc! { "updated_at": -1_i64, "_id": -1_i64 })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load pinned hero draft", error))
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.map(stored_draft).transpose()
    }

    pub async fn load_hero_character_for_owner(
        &self,
        campaign_session_id: &str,
        owner_key: &str,
    ) -> Result<Option<StoredDocument<HeroCharacter>>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, owner_key)?;
        let campaigns = self.campaigns();
        let heroes = self.hero_instances();
        let campaign = campaign_session_id.to_owned();
        let owner = owner_key.to_owned();
        let stored = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let heroes = heroes.clone();
                let campaign = campaign.clone();
                let owner = owner.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &owner,
                        &campaign,
                    )
                    .await?;
                    heroes
                        .find_one(doc! {
                            "campaign_id": &campaign,
                            "account_id": &owner,
                            "state": "active",
                            "runtime_kind": "hero_character",
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load hero for owner", error))
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.map(stored_hero).transpose()
    }

    pub(crate) async fn delete_retired_hero_drafts(
        &self,
        now_epoch_seconds: u64,
    ) -> Result<u64, RepositoryError> {
        let now = date_from_epoch_seconds(now_epoch_seconds, "hero draft cleanup time")?;
        self.hero_drafts()
            .delete_many(doc! { "purge_at": { "$lte": now } })
            .await
            .map(|result| result.deleted_count)
            .map_err(|error| mongo_error("delete retired hero drafts", error))
    }

    pub(crate) async fn load_hero_command_receipt(
        &self,
        actor_account_id: &str,
        scope: HeroReceiptScope,
        scope_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<StoredHeroCommandReceipt>, RepositoryError> {
        validate_receipt_lookup(actor_account_id, scope_id, idempotency_key)?;
        let campaigns = self.campaigns();
        let receipts = self.receipts();
        let actor = actor_account_id.to_owned();
        let scope_id = scope_id.to_owned();
        let key = idempotency_key.to_owned();
        let stored = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let receipts = receipts.clone();
                let actor = actor.clone();
                let scope_id = scope_id.clone();
                let key = key.clone();
                Box::pin(async move {
                    let Some(receipt) = receipts
                        .find_one(doc! {
                            "scope_kind": scope.as_str(),
                            "scope_id": &scope_id,
                            "actor_account_id": &actor,
                            "idempotency_key": &key,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load hero receipt", error))?
                    else {
                        return Ok(None);
                    };
                    let Some(campaign_id) = receipt.campaign_id.as_deref() else {
                        return Err(PersistenceError::SchemaDrift {
                            collection: CollectionName::CommandReceipts.as_str().to_owned(),
                            detail: "hero receipt is missing campaign scope".to_owned(),
                        });
                    };
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        campaign_id,
                    )
                    .await?;
                    Ok(Some(receipt))
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.map(stored_receipt).transpose()
    }

    pub(crate) async fn load_encounter_reward_claim(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
        encounter_id: &str,
    ) -> Result<Option<StoredEncounterRewardClaim>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, actor_account_id)?;
        validate_opaque("encounter", encounter_id)?;
        let campaigns = self.campaigns();
        let audits = self.audits();
        let actor = actor_account_id.to_owned();
        let campaign = campaign_session_id.to_owned();
        let encounter = encounter_id.to_owned();
        let stored = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let audits = audits.clone();
                let actor = actor.clone();
                let campaign = campaign.clone();
                let encounter = encounter.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign,
                    )
                    .await?;
                    audits
                        .find_one(doc! {
                            "category": "encounter_reward_claim",
                            "scope_kind": "encounter",
                            "scope_id": &encounter,
                            "metadata.campaign_session_id": &campaign,
                        })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load encounter reward claim", error)
                        })
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.map(stored_encounter_claim).transpose()
    }

    pub(crate) async fn commit_hero_creation_transition(
        &self,
        draft: &HeroCreationDraft,
        expected_revision: u64,
        command: &HeroCreationCommand,
        audit: &HeroAuditPayload,
        created_character: Option<&HeroCharacter>,
        metadata: HeroCreationCommitMetadata<'_>,
    ) -> Result<HeroMutationCommitOutcome, RepositoryError> {
        let receipt = metadata.receipt;
        validate_draft(draft)?;
        audit.validate()?;
        validate_receipt(receipt, audit)?;
        validate_creation_commit(
            draft,
            expected_revision,
            command,
            audit,
            created_character,
            receipt,
        )?;
        if receipt.actor_account_id != draft.owner_id {
            return invalid(
                "hero creation draft",
                &draft.draft_id,
                "receipt actor must own the draft",
            );
        }
        match (&command.intent, metadata.campaign_pins) {
            (
                manchester_dnd_core::hero::HeroCreationIntent::SelectCampaignTheme { pins },
                Some(evidence),
            ) if evidence.pins.hero == *pins => {
                validate_seal(&receipt.actor_account_id, &draft.campaign_id, evidence)?;
            }
            (manchester_dnd_core::hero::HeroCreationIntent::SelectCampaignTheme { .. }, None) => {
                return invalid(
                    "campaign content pins",
                    &draft.campaign_id,
                    "theme selection must atomically seal campaign pins",
                );
            }
            (
                manchester_dnd_core::hero::HeroCreationIntent::SelectCampaignTheme { .. },
                Some(_),
            ) => {
                return invalid(
                    "campaign content pins",
                    &draft.campaign_id,
                    "campaign pins must match the selected hero theme",
                );
            }
            (_, Some(_)) => {
                return invalid(
                    "campaign content pins",
                    &draft.campaign_id,
                    "campaign pins may be sealed only by theme selection",
                );
            }
            (_, None) => {}
        }

        let stored = self
            .load_hero_draft(&receipt.actor_account_id, &draft.draft_id)
            .await?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "hero creation draft",
                id: draft.draft_id.clone(),
            })?;
        validate_draft_successor(
            &stored,
            draft,
            expected_revision,
            command,
            audit,
            created_character,
        )?;
        let now = DateTime::now();
        let next_durable_revision = durable_revision(draft.revision, "hero draft revision")?;
        let expected_durable_revision =
            durable_revision(expected_revision, "expected hero draft revision")?;
        let created_document = created_character
            .map(|character| HeroInstanceDocument::new(character.clone(), now))
            .transpose()?;
        let audit_document = hero_audit_document(&draft.campaign_id, audit, now)?;
        let receipt_document = hero_receipt_document(receipt, now)?;

        let campaigns = self.campaigns();
        let drafts = self.hero_drafts();
        let heroes = self.hero_instances();
        let audits = self.audits();
        let receipts = self.receipts();
        let actor = receipt.actor_account_id.clone();
        let campaign_id = draft.campaign_id.clone();
        let draft_id = draft.draft_id.clone();
        let successor = draft.clone();
        let pins = metadata.campaign_pins.cloned();
        let result = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let drafts = drafts.clone();
                let heroes = heroes.clone();
                let audits = audits.clone();
                let receipts = receipts.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let draft_id = draft_id.clone();
                let successor = successor.clone();
                let created = created_document.clone();
                let audit = audit_document.clone();
                let receipt = receipt_document.clone();
                let pins = pins.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    reject_conflicting_hero_receipt(&receipts, client_session, &receipt).await?;
                    let updated = drafts
                        .find_one_and_update(
                            doc! {
                                "_id": &draft_id,
                                "owner_account_id": &actor,
                                "campaign_id": &campaign_id,
                                "revision": i64::try_from(expected_durable_revision).map_err(
                                    |_| PersistenceError::SchemaDrift {
                                        collection: CollectionName::PlayerCharacterDrafts
                                            .as_str()
                                            .to_owned(),
                                        detail: "hero draft revision exceeds BSON range"
                                            .to_owned(),
                                    },
                                )?,
                            },
                            doc! {
                                "$set": {
                                    "revision": i64::try_from(next_durable_revision).map_err(
                                        |_| PersistenceError::SchemaDrift {
                                            collection: CollectionName::PlayerCharacterDrafts
                                                .as_str()
                                                .to_owned(),
                                            detail: "hero draft revision exceeds BSON range"
                                                .to_owned(),
                                        },
                                    )?,
                                    "step": i64::try_from(successor.revision).map_err(
                                        |_| PersistenceError::SchemaDrift {
                                            collection: CollectionName::PlayerCharacterDrafts
                                                .as_str()
                                                .to_owned(),
                                            detail: "hero draft step exceeds BSON range".to_owned(),
                                        },
                                    )?,
                                    "state": draft_state(&successor),
                                    "draft": mongodb::bson::to_bson(&successor)
                                        .map_err(PersistenceError::BsonEncoding)?,
                                    "updated_at": now,
                                }
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("commit hero draft transition", error)
                        })?;
                    let updated = match updated {
                        Some(updated) => updated,
                        None => {
                            let current = drafts
                                .find_one(doc! {
                                    "_id": &draft_id,
                                    "owner_account_id": &actor,
                                    "campaign_id": &campaign_id,
                                })
                                .session(&mut *client_session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo(
                                        "load conflicting hero draft revision",
                                        error,
                                    )
                                })?
                                .ok_or_else(|| PersistenceError::NotFound {
                                    entity: "hero creation draft",
                                    id: draft_id.clone(),
                                })?;
                            return Err(PersistenceError::RevisionConflict {
                                entity: "hero creation draft",
                                id: draft_id,
                                expected: expected_revision,
                                actual: current.draft.revision,
                            });
                        }
                    };
                    if let Some(character) = created {
                        heroes
                            .insert_one(character)
                            .session(&mut *client_session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("create campaign hero instance", error)
                            })?;
                    }
                    if let Some(pins) = pins {
                        seal_campaign_pins_in_transaction(
                            &campaigns,
                            client_session,
                            &actor,
                            &campaign_id,
                            &pins,
                            now,
                        )
                        .await?;
                    }
                    audits
                        .insert_one(audit)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert hero audit", error))?;
                    receipts
                        .insert_one(receipt)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert hero receipt", error))?;
                    Ok(updated.updated_at)
                })
            })
            .await;
        let updated_at = map_hero_commit_result(self, result, receipt).await?;
        Ok(HeroMutationCommitOutcome {
            subject: SaveOutcome {
                revision: next_durable_revision,
                updated_at: date_string(updated_at),
            },
            created_character: created_character
                .map(|character| {
                    Ok::<SaveOutcome, RepositoryError>(SaveOutcome {
                        revision: durable_revision(character.revision, "hero character revision")?,
                        updated_at: date_string(now),
                    })
                })
                .transpose()?,
        })
    }

    pub(crate) async fn commit_hero_character_mutation(
        &self,
        character: &HeroCharacter,
        expected_revision: u64,
        command: HeroCharacterMutationCommand<'_>,
        audit: &HeroAuditPayload,
        receipt: &NewHeroCommandReceipt,
    ) -> Result<HeroMutationCommitOutcome, RepositoryError> {
        validate_character(character)?;
        audit.validate()?;
        validate_receipt(receipt, audit)?;
        if receipt.actor_account_id != character.owner_id {
            return invalid(
                "hero character",
                &character.character_id,
                "receipt actor must own the campaign character",
            );
        }
        if let HeroCharacterMutationCommand::EncounterReward { reward, claim } = command {
            validate_encounter_reward_claim(character, reward, claim, audit, receipt)?;
        }
        let stored = self
            .load_hero_character(&receipt.actor_account_id, &character.character_id)
            .await?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "hero character",
                id: character.character_id.clone(),
            })?;
        validate_character_successor(
            &stored,
            character,
            expected_revision,
            command,
            audit,
            receipt,
        )?;

        let now = DateTime::now();
        let next_durable_revision =
            durable_revision(character.revision, "hero character revision")?;
        let expected_durable_revision =
            durable_revision(expected_revision, "expected hero character revision")?;
        let progression = hero_progression(character)?;
        let audit_document = hero_audit_document(&character.campaign_id, audit, now)?;
        let claim_document = match command {
            HeroCharacterMutationCommand::EncounterReward { claim, .. } => Some(
                encounter_claim_document(&receipt.actor_account_id, claim, now)?,
            ),
            HeroCharacterMutationCommand::Reward(_) | HeroCharacterMutationCommand::LevelUp(_) => {
                None
            }
        };
        let receipt_document = hero_receipt_document(receipt, now)?;
        let campaigns = self.campaigns();
        let heroes = self.hero_instances();
        let audits = self.audits();
        let receipts = self.receipts();
        let actor = receipt.actor_account_id.clone();
        let campaign_id = character.campaign_id.clone();
        let character_id = character.character_id.clone();
        let successor = character.clone();
        let result = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let heroes = heroes.clone();
                let audits = audits.clone();
                let receipts = receipts.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let character_id = character_id.clone();
                let successor = successor.clone();
                let progression = progression.clone();
                let audit = audit_document.clone();
                let claim = claim_document.clone();
                let receipt = receipt_document.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                    )
                    .await?;
                    reject_conflicting_hero_receipt(&receipts, client_session, &receipt).await?;
                    let updated = heroes
                        .find_one_and_update(
                            doc! {
                                "_id": &character_id,
                                "campaign_id": &campaign_id,
                                "account_id": &actor,
                                "runtime_kind": "hero_character",
                                "state": "active",
                                "revision": i64::try_from(expected_durable_revision).map_err(
                                    |_| PersistenceError::SchemaDrift {
                                        collection: CollectionName::CampaignCharacterInstances
                                            .as_str()
                                            .to_owned(),
                                        detail: "hero revision exceeds BSON range".to_owned(),
                                    },
                                )?,
                            },
                            doc! {
                                "$set": {
                                    "revision": i64::try_from(next_durable_revision).map_err(
                                        |_| PersistenceError::SchemaDrift {
                                            collection: CollectionName::CampaignCharacterInstances
                                                .as_str()
                                                .to_owned(),
                                            detail: "hero revision exceeds BSON range".to_owned(),
                                        },
                                    )?,
                                    "progression": progression,
                                    "runtime.hero_character": mongodb::bson::to_bson(&successor)
                                        .map_err(PersistenceError::BsonEncoding)?,
                                    "updated_at": now,
                                }
                            },
                        )
                        .return_document(ReturnDocument::After)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("commit hero character mutation", error)
                        })?;
                    let updated = match updated {
                        Some(updated) => updated,
                        None => {
                            let current = heroes
                                .find_one(doc! {
                                    "_id": &character_id,
                                    "campaign_id": &campaign_id,
                                    "account_id": &actor,
                                    "runtime_kind": "hero_character",
                                    "state": "active",
                                })
                                .session(&mut *client_session)
                                .await
                                .map_err(|error| {
                                    PersistenceError::mongo("load conflicting hero revision", error)
                                })?
                                .ok_or_else(|| PersistenceError::NotFound {
                                    entity: "hero character",
                                    id: character_id.clone(),
                                })?;
                            return Err(PersistenceError::RevisionConflict {
                                entity: "hero character",
                                id: character_id,
                                expected: expected_revision,
                                actual: current.runtime.hero_character.revision,
                            });
                        }
                    };
                    audits
                        .insert_one(audit)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert hero audit", error))?;
                    if let Some(claim) = claim {
                        audits
                            .insert_one(claim)
                            .session(&mut *client_session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("insert encounter reward claim", error)
                            })?;
                    }
                    receipts
                        .insert_one(receipt)
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert hero receipt", error))?;
                    Ok(updated.updated_at)
                })
            })
            .await;
        let updated_at = map_hero_commit_result(self, result, receipt).await?;
        Ok(HeroMutationCommitOutcome {
            subject: SaveOutcome {
                revision: next_durable_revision,
                updated_at: date_string(updated_at),
            },
            created_character: None,
        })
    }

    pub async fn list_hero_audits(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
        subject_id: &str,
    ) -> Result<Vec<StoredHeroAudit>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, actor_account_id)?;
        validate_opaque("hero audit subject", subject_id)?;
        let campaigns = self.campaigns();
        let audits = self.audits();
        let actor = actor_account_id.to_owned();
        let campaign = campaign_session_id.to_owned();
        let subject = subject_id.to_owned();
        let stored = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let audits = audits.clone();
                let actor = actor.clone();
                let campaign = campaign.clone();
                let subject = subject.clone();
                Box::pin(async move {
                    ensure_campaign_access_in_session(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign,
                    )
                    .await?;
                    let mut cursor = audits
                        .find(doc! {
                            "category": "hero",
                            "scope_id": &subject,
                            "metadata.campaign_session_id": &campaign,
                        })
                        .sort(doc! { "metadata.subject_revision": 1_i64, "_id": 1_i64 })
                        .session(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("list hero audits", error))?;
                    let mut output = Vec::new();
                    while cursor
                        .advance(&mut *client_session)
                        .await
                        .map_err(|error| PersistenceError::mongo("read hero audits", error))?
                    {
                        output.push(cursor.deserialize_current().map_err(|error| {
                            PersistenceError::mongo("decode hero audit", error)
                        })?);
                    }
                    Ok(output)
                })
            })
            .await
            .map_err(map_persistence)?;
        stored.into_iter().map(stored_audit).collect()
    }

    fn hero_drafts(&self) -> Collection<HeroDraftDocument> {
        self.store()
            .collection(CollectionName::PlayerCharacterDrafts)
    }

    fn hero_instances(&self) -> Collection<HeroInstanceDocument> {
        self.store()
            .collection(CollectionName::CampaignCharacterInstances)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeroDraftDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    revision: u64,
    owner_account_id: String,
    campaign_id: String,
    step: u64,
    state: String,
    expires_at: DateTime,
    purge_at: DateTime,
    draft: HeroCreationDraft,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeroRuntimeDocument {
    hero_character: HeroCharacter,
    bde: BdeRuntimeDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HeroInstanceDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    revision: u64,
    campaign_id: String,
    account_id: String,
    source_player_character_id: String,
    runtime_kind: String,
    state: String,
    source_snapshot: Document,
    progression: Document,
    runtime: HeroRuntimeDocument,
    created_at: DateTime,
    updated_at: DateTime,
}

impl HeroInstanceDocument {
    fn new(character: HeroCharacter, now: DateTime) -> Result<Self, RepositoryError> {
        validate_character(&character)?;
        let progression = hero_progression(&character)?;
        Ok(Self {
            id: character.character_id.clone(),
            schema_version: STORAGE_SCHEMA_VERSION,
            revision: durable_revision(character.revision, "hero character revision")?,
            campaign_id: character.campaign_id.clone(),
            account_id: character.owner_id.clone(),
            source_player_character_id: character.character_id.clone(),
            runtime_kind: "hero_character".to_owned(),
            state: "active".to_owned(),
            source_snapshot: doc! {
                "source_kind": "hero_creation",
                "source_id": &character.character_id,
                "source_revision": 1_i64,
            },
            progression,
            runtime: HeroRuntimeDocument {
                hero_character: character,
                bde: BdeRuntimeDocument::default(),
            },
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct EncounterClaimEvidence {
    claim: NewEncounterRewardClaim,
}

#[derive(Debug, Clone)]
#[allow(dead_code, clippy::large_enum_variant)]
pub(super) enum PreparedEncounterHeroUpdate {
    Unchanged(SaveOutcome),
    Update {
        character_id: String,
        expected_revision: u64,
        result_revision: u64,
        character: HeroCharacter,
        progression: Document,
    },
}

pub(super) async fn prepare_encounter_hero_update(
    repository: &MongoRepository,
    actor_account_id: &str,
    campaign_session_id: &str,
    event: &SessionEventDto,
    update: EncounterHeroUpdate<'_>,
) -> Result<PreparedEncounterHeroUpdate, RepositoryError> {
    validate_character(update.character)?;
    let stored = repository
        .load_hero_character(actor_account_id, &update.character.character_id)
        .await?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "hero character",
            id: update.character.character_id.clone(),
        })?;
    if stored.value.campaign_id != campaign_session_id {
        return invalid(
            "hero character",
            &stored.id,
            "encounter hero is not linked to the campaign",
        );
    }
    if stored.value.revision != update.expected_revision {
        return Err(RepositoryError::RevisionConflict {
            entity: "hero character",
            id: stored.id,
            expected: update.expected_revision,
            actual: stored.value.revision,
        });
    }
    let outcome = match &event.payload {
        SessionEventPayload::EncounterResolved { outcome, .. } => outcome,
        _ => {
            return invalid(
                "hero character",
                &update.character.character_id,
                "encounter hero update requires an encounter resolution event",
            );
        }
    };
    let state = &outcome.resolution.state;
    let submitted_runtime = RuntimeResources::from_derived_sheet(
        update.character.choices.class.class(),
        &update.character.sheet,
    )
    .map_err(|_| RepositoryError::InvalidDomainState {
        entity: "hero character",
        id: update.character.character_id.clone(),
        reason: "hero resources cannot be projected into encounter runtime",
    })?;
    if state.schema_version != ENCOUNTER_SCHEMA_VERSION
        || state.hero.source_character_id.as_deref() != Some(update.character.character_id.as_str())
        || state.hero.hit_points.current != update.character.sheet.current_hit_points
        || state.hero.hit_points.maximum != update.character.sheet.maximum_hit_points
        || outcome.result_hero_revision != Some(update.character.revision)
        || state
            .hero_rules
            .as_ref()
            .is_none_or(|rules| rules.runtime_resources != submitted_runtime)
    {
        return invalid(
            "hero character",
            &update.character.character_id,
            "encounter outcome and authoritative hero runtime do not match",
        );
    }
    if stored.value == *update.character {
        return Ok(PreparedEncounterHeroUpdate::Unchanged(SaveOutcome {
            revision: stored.revision,
            updated_at: stored.updated_at,
        }));
    }
    let resource_currents = update
        .character
        .sheet
        .resources
        .iter()
        .map(|pool| (pool.resource, pool.current))
        .collect::<Vec<_>>();
    let mut expected = stored.value;
    expected
        .synchronize_encounter_runtime(
            update.character.sheet.current_hit_points,
            &resource_currents,
        )
        .map_err(|source| RepositoryError::HeroValidation {
            entity: "hero character",
            id: update.character.character_id.clone(),
            source,
        })?;
    if &expected != update.character {
        return invalid(
            "hero character",
            &update.character.character_id,
            "encounter commits may only advance current HP and resource counters",
        );
    }
    Ok(PreparedEncounterHeroUpdate::Update {
        character_id: update.character.character_id.clone(),
        expected_revision: durable_revision(
            update.expected_revision,
            "expected hero character revision",
        )?,
        result_revision: durable_revision(update.character.revision, "hero character revision")?,
        character: update.character.clone(),
        progression: hero_progression(update.character)?,
    })
}

pub(super) async fn commit_prepared_encounter_hero_update(
    characters: &Collection<HeroInstanceDocument>,
    client_session: &mut ClientSession,
    campaign_session_id: &str,
    now: DateTime,
    update: PreparedEncounterHeroUpdate,
) -> Result<SaveOutcome, PersistenceError> {
    match update {
        PreparedEncounterHeroUpdate::Unchanged(save) => Ok(save),
        PreparedEncounterHeroUpdate::Update {
            character_id,
            expected_revision,
            result_revision,
            character,
            progression,
        } => {
            let result = characters
                .update_one(
                    doc! {
                        "_id": &character_id,
                        "campaign_id": campaign_session_id,
                        "runtime_kind": "hero_character",
                        "state": "active",
                        "revision": i64::try_from(expected_revision).map_err(|_| {
                            PersistenceError::SchemaDrift {
                                collection: CollectionName::CampaignCharacterInstances
                                    .as_str()
                                    .to_owned(),
                                detail: "hero revision exceeds BSON range".to_owned(),
                            }
                        })?,
                    },
                    doc! {
                        "$set": {
                            "revision": i64::try_from(result_revision).map_err(|_| {
                                PersistenceError::SchemaDrift {
                                    collection: CollectionName::CampaignCharacterInstances
                                        .as_str()
                                        .to_owned(),
                                    detail: "hero revision exceeds BSON range".to_owned(),
                                }
                            })?,
                            "progression": progression,
                            "runtime.hero_character": mongodb::bson::to_bson(&character)
                                .map_err(PersistenceError::BsonEncoding)?,
                            "updated_at": now,
                        }
                    },
                )
                .session(&mut *client_session)
                .await
                .map_err(|error| PersistenceError::mongo("commit encounter hero update", error))?;
            if result.modified_count != 1 {
                let current = characters
                    .find_one(doc! {
                        "_id": &character_id,
                        "campaign_id": campaign_session_id,
                        "runtime_kind": "hero_character",
                        "state": "active",
                    })
                    .session(&mut *client_session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load conflicting encounter hero revision", error)
                    })?
                    .ok_or_else(|| PersistenceError::NotFound {
                        entity: "hero character",
                        id: character_id.clone(),
                    })?;
                return Err(PersistenceError::RevisionConflict {
                    entity: "hero character",
                    id: character_id,
                    expected: expected_revision.saturating_sub(1),
                    actual: current.runtime.hero_character.revision,
                });
            }
            Ok(SaveOutcome {
                revision: result_revision,
                updated_at: date_string(now),
            })
        }
    }
}

fn stored_draft(
    stored: HeroDraftDocument,
) -> Result<StoredDocument<HeroCreationDraft>, RepositoryError> {
    validate_draft(&stored.draft)?;
    if stored.schema_version != STORAGE_SCHEMA_VERSION
        || stored.id != stored.draft.draft_id
        || stored.revision != durable_revision(stored.draft.revision, "hero draft revision")?
        || stored.campaign_id != stored.draft.campaign_id
        || stored.owner_account_id != stored.draft.owner_id
    {
        return invalid(
            "hero creation draft",
            &stored.id,
            "stored metadata and validated draft do not match",
        );
    }
    Ok(StoredDocument {
        id: stored.id,
        schema_version: u32::from(HERO_DRAFT_SCHEMA_VERSION),
        revision: stored.revision,
        value: stored.draft,
        created_at: date_string(stored.created_at),
        updated_at: date_string(stored.updated_at),
    })
}

fn stored_hero(
    stored: HeroInstanceDocument,
) -> Result<StoredDocument<HeroCharacter>, RepositoryError> {
    validate_character(&stored.runtime.hero_character)?;
    if stored.schema_version != STORAGE_SCHEMA_VERSION
        || stored.id != stored.runtime.hero_character.character_id
        || stored.revision
            != durable_revision(
                stored.runtime.hero_character.revision,
                "hero character revision",
            )?
        || stored.campaign_id != stored.runtime.hero_character.campaign_id
        || stored.account_id != stored.runtime.hero_character.owner_id
    {
        return invalid(
            "hero character",
            &stored.id,
            "stored metadata and validated hero do not match",
        );
    }
    Ok(StoredDocument {
        id: stored.id,
        schema_version: u32::from(HERO_CHARACTER_SCHEMA_VERSION),
        revision: stored.revision,
        value: stored.runtime.hero_character,
        created_at: date_string(stored.created_at),
        updated_at: date_string(stored.updated_at),
    })
}

fn hero_audit_document(
    campaign_session_id: &str,
    audit: &HeroAuditPayload,
    created_at: DateTime,
) -> Result<AuditEventDocument, RepositoryError> {
    audit.validate()?;
    let payload =
        mongodb::bson::to_bson(audit).map_err(|_| RepositoryError::InvalidDomainState {
            entity: "hero audit",
            id: audit.audit_id().to_owned(),
            reason: "hero audit could not be encoded as BSON",
        })?;
    Ok(AuditEventDocument {
        id: audit.audit_id().to_owned(),
        schema_version: STORAGE_SCHEMA_VERSION,
        category: "hero".to_owned(),
        action: audit.kind().to_owned(),
        outcome: "committed".to_owned(),
        actor_account_id: Some(audit_actor_id(audit).to_owned()),
        scope_kind: audit.subject_kind().as_str().to_owned(),
        scope_id: audit.subject_id().to_owned(),
        correlation_id: Some(audit.audit_id().to_owned()),
        metadata: doc! {
            "campaign_session_id": campaign_session_id,
            "subject_revision": i64::try_from(audit.subject_revision()).map_err(|_| {
                RepositoryError::NumericRange {
                    field: "hero audit revision",
                }
            })?,
            "occurred_at_epoch_seconds": i64::try_from(
                audit.occurred_at_epoch_seconds(),
            )
            .map_err(|_| RepositoryError::NumericRange {
                field: "hero audit time",
            })?,
            "hero_audit": payload,
        },
        created_at,
    })
}

fn encounter_claim_document(
    actor_account_id: &str,
    claim: &NewEncounterRewardClaim,
    created_at: DateTime,
) -> Result<AuditEventDocument, RepositoryError> {
    let claim_bson =
        mongodb::bson::to_bson(claim).map_err(|_| RepositoryError::InvalidDomainState {
            entity: "encounter reward claim",
            id: claim.encounter_id.clone(),
            reason: "encounter reward claim could not be encoded as BSON",
        })?;
    Ok(AuditEventDocument {
        id: format!(
            "audit:{}",
            Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!(
                    "encounter-reward:{}:{}",
                    claim.campaign_session_id, claim.encounter_id
                )
                .as_bytes(),
            )
            .simple()
        ),
        schema_version: STORAGE_SCHEMA_VERSION,
        category: "encounter_reward_claim".to_owned(),
        action: "reward_claimed".to_owned(),
        outcome: "committed".to_owned(),
        actor_account_id: Some(actor_account_id.to_owned()),
        scope_kind: "encounter".to_owned(),
        scope_id: claim.encounter_id.clone(),
        correlation_id: Some(claim.hero_audit_id.clone()),
        metadata: doc! {
            "campaign_session_id": &claim.campaign_session_id,
            "claim": claim_bson,
        },
        created_at,
    })
}

fn stored_audit(stored: AuditEventDocument) -> Result<StoredHeroAudit, RepositoryError> {
    let payload: HeroAuditPayload =
        mongodb::bson::from_bson(stored.metadata.get("hero_audit").cloned().ok_or_else(|| {
            RepositoryError::InvalidDomainState {
                entity: "hero audit",
                id: stored.id.clone(),
                reason: "stored hero audit payload is missing",
            }
        })?)
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "hero audit",
            id: stored.id.clone(),
            reason: "stored hero audit payload is invalid",
        })?;
    payload.validate()?;
    let campaign_session_id = stored
        .metadata
        .get_str("campaign_session_id")
        .map(str::to_owned)
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "hero audit",
            id: stored.id.clone(),
            reason: "stored hero audit campaign is invalid",
        })?;
    if stored.id != payload.audit_id()
        || stored.scope_id != payload.subject_id()
        || stored.scope_kind != payload.subject_kind().as_str()
        || stored.action != payload.kind()
    {
        return invalid(
            "hero audit",
            &stored.id,
            "stored hero audit metadata does not match its payload",
        );
    }
    Ok(StoredHeroAudit {
        id: stored.id,
        campaign_session_id,
        subject_id: payload.subject_id().to_owned(),
        subject_revision: payload.subject_revision(),
        schema_version: u32::from(HERO_AUDIT_SCHEMA_VERSION),
        occurred_at_epoch_seconds: payload.occurred_at_epoch_seconds(),
        payload,
        created_at: date_string(stored.created_at),
    })
}

fn stored_encounter_claim(
    stored: AuditEventDocument,
) -> Result<StoredEncounterRewardClaim, RepositoryError> {
    let claim: NewEncounterRewardClaim =
        mongodb::bson::from_bson(stored.metadata.get("claim").cloned().ok_or_else(|| {
            RepositoryError::InvalidDomainState {
                entity: "encounter reward claim",
                id: stored.scope_id.clone(),
                reason: "stored encounter reward claim is missing",
            }
        })?)
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "encounter reward claim",
            id: stored.scope_id.clone(),
            reason: "stored encounter reward claim is invalid",
        })?;
    validate_stored_encounter_claim(&claim)?;
    Ok(StoredEncounterRewardClaim {
        campaign_session_id: claim.campaign_session_id,
        encounter_id: claim.encounter_id,
        character_id: claim.character_id,
        encounter_revision: claim.encounter_revision,
        victory_event_sequence: claim.victory_event_sequence,
        reward_tier: claim.reward_tier,
        experience_awarded: claim.experience_awarded,
        hero_audit_id: claim.hero_audit_id,
        created_at: date_string(stored.created_at),
    })
}

fn hero_receipt_document(
    receipt: &NewHeroCommandReceipt,
    created_at: DateTime,
) -> Result<CommandReceiptDocument, RepositoryError> {
    validate_receipt_fields(receipt)?;
    Ok(CommandReceiptDocument {
        id: format!("receipt:{}", Uuid::new_v4().simple()),
        schema_version: STORAGE_SCHEMA_VERSION,
        scope_kind: receipt.scope.as_str().to_owned(),
        scope_id: receipt.scope_id.clone(),
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
        created_at,
    })
}

fn stored_receipt(
    stored: CommandReceiptDocument,
) -> Result<StoredHeroCommandReceipt, RepositoryError> {
    let scope = match stored.scope_kind.as_str() {
        "hero_draft" => HeroReceiptScope::Draft,
        "campaign_character_instance" => HeroReceiptScope::Character,
        _ => {
            return invalid(
                "hero command receipt",
                &stored.id,
                "stored hero receipt scope is unsupported",
            );
        }
    };
    let request_fingerprint = Sha256Digest::new(stored.request_fingerprint).map_err(|source| {
        RepositoryError::CoreValidation {
            entity: "hero command receipt",
            id: stored.id.clone(),
            source,
        }
    })?;
    let campaign_session_id =
        stored
            .campaign_id
            .clone()
            .ok_or_else(|| RepositoryError::InvalidDomainState {
                entity: "hero command receipt",
                id: stored.id.clone(),
                reason: "stored hero receipt is missing its campaign scope",
            })?;
    let receipt = StoredHeroCommandReceipt {
        actor_account_id: stored.actor_account_id,
        scope,
        scope_id: stored.scope_id,
        campaign_session_id,
        idempotency_key: stored.idempotency_key,
        command_kind: stored.command_kind,
        request_fingerprint,
        expected_revision: stored.expected_revision,
        result_revision: stored.result_revision,
        audit_id: stored.audit_id,
        response_json: stored.response_json,
        created_at: date_string(stored.created_at),
    };
    validate_stored_receipt(&receipt)?;
    Ok(receipt)
}

async fn reject_conflicting_hero_receipt(
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
        .map_err(|error| PersistenceError::mongo("check hero receipt", error))?;
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
        entity: "hero command receipt",
        id: expected.id.clone(),
    })
}

async fn map_hero_commit_result<T>(
    repository: &MongoRepository,
    result: Result<T, PersistenceError>,
    expected: &NewHeroCommandReceipt,
) -> Result<T, RepositoryError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if error.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey) => {
            match repository
                .load_hero_command_receipt(
                    &expected.actor_account_id,
                    expected.scope,
                    &expected.scope_id,
                    &expected.idempotency_key,
                )
                .await?
            {
                Some(stored)
                    if stored.command_kind == expected.command_kind
                        && stored.request_fingerprint == expected.request_fingerprint =>
                {
                    Err(RepositoryError::AlreadyExists {
                        entity: "hero command receipt",
                        id: expected.idempotency_key.clone(),
                    })
                }
                Some(_) => Err(RepositoryError::IdempotencyConflict {
                    scope_kind: expected.scope.as_str().to_owned(),
                    scope_id: expected.scope_id.clone(),
                    idempotency_key: expected.idempotency_key.clone(),
                }),
                None => Err(map_persistence(error)),
            }
        }
        Err(error) => Err(map_persistence(error)),
    }
}

fn hero_progression(character: &HeroCharacter) -> Result<Document, RepositoryError> {
    let level = mongodb::bson::to_bson(&character.level).map_err(|_| {
        RepositoryError::InvalidDomainState {
            entity: "hero character",
            id: character.character_id.clone(),
            reason: "hero level could not be encoded as BSON",
        }
    })?;
    Ok(doc! {
        "level": level,
        "experience_points": i64::from(character.experience_points),
        "advancement_choices": mongodb::bson::to_bson(&character.advancement_choices)
            .map_err(|_| RepositoryError::InvalidDomainState {
                entity: "hero character",
                id: character.character_id.clone(),
                reason: "hero advancement choices could not be encoded as BSON",
            })?,
    })
}

fn validate_owner_lookup(
    campaign_session_id: &str,
    owner_key: &str,
) -> Result<(), RepositoryError> {
    validate_account_id(owner_key)?;
    validate_opaque("campaign", campaign_session_id)
}

fn validate_draft(draft: &HeroCreationDraft) -> Result<(), RepositoryError> {
    draft
        .validate()
        .map_err(|source| hero_validation("hero creation draft", &draft.draft_id, source))
}

fn validate_character(character: &HeroCharacter) -> Result<(), RepositoryError> {
    character
        .validate()
        .map_err(|source| hero_validation("hero character", &character.character_id, source))
}

fn validate_draft_successor(
    stored: &StoredDocument<HeroCreationDraft>,
    submitted: &HeroCreationDraft,
    expected_revision: u64,
    command: &HeroCreationCommand,
    audit: &HeroAuditPayload,
    created_character: Option<&HeroCharacter>,
) -> Result<(), RepositoryError> {
    if stored.value.revision != expected_revision {
        return Err(RepositoryError::RevisionConflict {
            entity: "hero creation draft",
            id: stored.id.clone(),
            expected: expected_revision,
            actual: stored.value.revision,
        });
    }
    if submitted.revision
        != expected_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange {
                field: "hero draft revision",
            })?
        || stored.value.draft_id != submitted.draft_id
        || stored.value.campaign_id != submitted.campaign_id
        || stored.value.owner_id != submitted.owner_id
        || stored.value.expires_at_epoch_seconds != submitted.expires_at_epoch_seconds
    {
        return invalid(
            "hero creation draft",
            &submitted.draft_id,
            "successor must advance one revision without rewriting identity or expiry",
        );
    }
    let HeroAuditPayload::CreationTransition {
        transition,
        character_created,
    } = audit
    else {
        return invalid(
            "hero creation draft",
            &submitted.draft_id,
            "draft successor requires a creation-transition audit",
        );
    };
    let mut replayed_draft = stored.value.clone();
    let replayed = replayed_draft
        .apply_trusted(
            command,
            &TrustedMutationContext {
                audit_id: transition.audit_id.clone(),
                actor_id: transition.actor_id.clone(),
                occurred_at_epoch_seconds: transition.occurred_at_epoch_seconds,
            },
        )
        .map_err(|source| hero_validation("hero creation draft", &submitted.draft_id, source))?;
    if replayed_draft != *submitted
        || replayed.transition_audit != **transition
        || replayed.created_audit.as_ref() != character_created.as_deref()
        || replayed.character.as_ref() != created_character
    {
        return invalid(
            "hero creation draft",
            &submitted.draft_id,
            "locked draft replay does not equal submitted state and audit",
        );
    }
    Ok(())
}

fn validate_creation_commit(
    draft: &HeroCreationDraft,
    expected_revision: u64,
    command: &HeroCreationCommand,
    audit: &HeroAuditPayload,
    created_character: Option<&HeroCharacter>,
    receipt: &NewHeroCommandReceipt,
) -> Result<(), RepositoryError> {
    let HeroAuditPayload::CreationTransition {
        transition,
        character_created,
    } = audit
    else {
        return invalid(
            "hero creation draft",
            &draft.draft_id,
            "creation commits require a creation-transition audit",
        );
    };
    if transition.draft_id != draft.draft_id
        || command.draft_id != draft.draft_id
        || command.expected_revision != expected_revision
        || command.idempotency_key != transition.idempotency_key
        || transition.revision_before != expected_revision
        || transition.revision_after != draft.revision
        || receipt.scope != HeroReceiptScope::Draft
        || receipt.command_kind != CREATION_COMMAND_KIND
        || receipt.scope_id != draft.draft_id
        || receipt.campaign_session_id != draft.campaign_id
    {
        return invalid(
            "hero creation draft",
            &draft.draft_id,
            "draft, audit, and receipt identities or revisions do not match",
        );
    }
    match (created_character, character_created) {
        (None, None) if draft.committed_character_id.is_none() => Ok(()),
        (Some(character), Some(created))
            if draft.committed_character_id.as_deref() == Some(character.character_id.as_str())
                && character.campaign_id == draft.campaign_id
                && character.owner_id == draft.owner_id
                && character.revision == 0
                && created.character_id == character.character_id
                && created.choices == character.choices
                && created.derived_sheet == character.sheet =>
        {
            validate_character(character)
        }
        _ => invalid(
            "hero creation draft",
            &draft.draft_id,
            "committed draft, created character, and audit must be present atomically",
        ),
    }
}

fn validate_character_successor(
    stored: &StoredDocument<HeroCharacter>,
    submitted: &HeroCharacter,
    expected_revision: u64,
    command: HeroCharacterMutationCommand<'_>,
    audit: &HeroAuditPayload,
    receipt: &NewHeroCommandReceipt,
) -> Result<(), RepositoryError> {
    if stored.value.revision != expected_revision {
        return Err(RepositoryError::RevisionConflict {
            entity: "hero character",
            id: stored.id.clone(),
            expected: expected_revision,
            actual: stored.value.revision,
        });
    }
    if submitted.revision
        != expected_revision
            .checked_add(1)
            .ok_or(RepositoryError::NumericRange {
                field: "hero character revision",
            })?
        || stored.value.character_id != submitted.character_id
        || stored.value.campaign_id != submitted.campaign_id
        || stored.value.owner_id != submitted.owner_id
        || stored.value.choices != submitted.choices
        || receipt.scope != HeroReceiptScope::Character
        || receipt.scope_id != submitted.character_id
        || receipt.campaign_session_id != submitted.campaign_id
    {
        return invalid(
            "hero character",
            &submitted.character_id,
            "successor, audit scope, and receipt must preserve identity and choices",
        );
    }
    let expected_command_kind = command.command_kind();
    match (command, audit) {
        (
            HeroCharacterMutationCommand::Reward(command)
            | HeroCharacterMutationCommand::EncounterReward {
                reward: command, ..
            },
            HeroAuditPayload::RewardAwarded { reward },
        ) if reward.character_id == submitted.character_id
            && receipt.command_kind == expected_command_kind
            && command.character_id == submitted.character_id
            && command.expected_revision == expected_revision
            && command.idempotency_key == receipt.idempotency_key
            && reward.revision_before == expected_revision
            && reward.revision_after == submitted.revision
            && reward.experience_before == stored.value.experience_points
            && reward.experience_after == submitted.experience_points
            && stored.value.level == submitted.level
            && stored.value.advancement_choices == submitted.advancement_choices => {}
        (
            HeroCharacterMutationCommand::LevelUp(command),
            HeroAuditPayload::LevelUp { level_up },
        ) if level_up.character_id == submitted.character_id
            && receipt.command_kind == LEVEL_UP_COMMAND_KIND
            && command.character_id == submitted.character_id
            && command.expected_revision == expected_revision
            && command.idempotency_key == receipt.idempotency_key
            && command.choice == level_up.choice
            && level_up.revision_before == expected_revision
            && level_up.revision_after == submitted.revision
            && stored.value.experience_points == submitted.experience_points
            && stored.value.level == SupportedLevel::One
            && submitted.level == SupportedLevel::Two
            && submitted.advancement_choices.as_slice()
                == std::slice::from_ref(&level_up.choice) => {}
        _ => {
            return invalid(
                "hero character",
                &submitted.character_id,
                "character successor does not match immutable reward or level-up audit",
            );
        }
    }
    let mut replayed = stored.value.clone();
    match (command, audit) {
        (
            HeroCharacterMutationCommand::Reward(command)
            | HeroCharacterMutationCommand::EncounterReward {
                reward: command, ..
            },
            HeroAuditPayload::RewardAwarded { reward },
        ) => {
            let replayed_audit = replayed
                .apply_reward(
                    command,
                    TrustedRewardPolicy::MvpXpV1,
                    &TrustedMutationContext {
                        audit_id: reward.audit_id.clone(),
                        actor_id: reward.actor_id.clone(),
                        occurred_at_epoch_seconds: reward.occurred_at_epoch_seconds,
                    },
                )
                .map_err(|source| {
                    hero_validation("hero character", &submitted.character_id, source)
                })?;
            if replayed_audit != *reward {
                return invalid(
                    "hero character",
                    &submitted.character_id,
                    "trusted reward replay does not equal immutable audit",
                );
            }
        }
        (
            HeroCharacterMutationCommand::LevelUp(command),
            HeroAuditPayload::LevelUp { level_up },
        ) => {
            let replayed_audit = replayed
                .level_up(
                    command,
                    &TrustedMutationContext {
                        audit_id: level_up.audit_id.clone(),
                        actor_id: level_up.actor_id.clone(),
                        occurred_at_epoch_seconds: level_up.occurred_at_epoch_seconds,
                    },
                )
                .map_err(|source| {
                    hero_validation("hero character", &submitted.character_id, source)
                })?;
            if replayed_audit != *level_up {
                return invalid(
                    "hero character",
                    &submitted.character_id,
                    "level-up replay does not equal immutable audit",
                );
            }
        }
        _ => {
            return invalid(
                "hero character",
                &submitted.character_id,
                "command and audit variants do not match",
            );
        }
    }
    if replayed != *submitted {
        return invalid(
            "hero character",
            &submitted.character_id,
            "locked character replay does not equal submitted successor",
        );
    }
    Ok(())
}

fn validate_receipt(
    receipt: &NewHeroCommandReceipt,
    audit: &HeroAuditPayload,
) -> Result<(), RepositoryError> {
    validate_receipt_fields(receipt)?;
    if receipt.audit_id != audit.audit_id() {
        return invalid(
            "hero command receipt",
            &receipt.scope_id,
            "receipt audit id does not match hero audit",
        );
    }
    Ok(())
}

fn validate_receipt_fields(receipt: &NewHeroCommandReceipt) -> Result<(), RepositoryError> {
    validate_receipt_lookup(
        &receipt.actor_account_id,
        &receipt.scope_id,
        &receipt.idempotency_key,
    )?;
    if !is_valid_opaque_id(&receipt.campaign_session_id)
        || !is_valid_opaque_id(&receipt.command_kind)
        || !is_valid_opaque_id(&receipt.audit_id)
        || receipt.result_revision
            != receipt
                .expected_revision
                .checked_add(1)
                .ok_or(RepositoryError::NumericRange {
                    field: "hero receipt revision",
                })?
        || receipt.response_json.is_empty()
        || receipt.response_json.len() > HERO_RECEIPT_RESPONSE_MAX_BYTES
        || serde_json::from_str::<serde_json::Value>(&receipt.response_json).is_err()
    {
        return invalid(
            "hero command receipt",
            &receipt.scope_id,
            "receipt identity, revisions, audit, or bounded response are invalid",
        );
    }
    Ok(())
}

fn validate_stored_receipt(receipt: &StoredHeroCommandReceipt) -> Result<(), RepositoryError> {
    validate_receipt_lookup(
        &receipt.actor_account_id,
        &receipt.scope_id,
        &receipt.idempotency_key,
    )?;
    if !is_valid_opaque_id(&receipt.campaign_session_id)
        || !is_valid_opaque_id(&receipt.command_kind)
        || !is_valid_opaque_id(&receipt.audit_id)
        || receipt.result_revision != receipt.expected_revision.saturating_add(1)
        || receipt.response_json.is_empty()
        || receipt.response_json.len() > HERO_RECEIPT_RESPONSE_MAX_BYTES
        || serde_json::from_str::<serde_json::Value>(&receipt.response_json).is_err()
    {
        return invalid(
            "hero command receipt",
            &receipt.scope_id,
            "stored receipt is invalid",
        );
    }
    Ok(())
}

fn validate_receipt_lookup(
    actor_account_id: &str,
    scope_id: &str,
    idempotency_key: &str,
) -> Result<(), RepositoryError> {
    validate_account_id(actor_account_id)?;
    if !is_valid_opaque_id(scope_id) || !is_valid_opaque_id(idempotency_key) {
        return invalid(
            "hero command receipt",
            scope_id,
            "scope and idempotency ids must be valid opaque identifiers",
        );
    }
    Ok(())
}

fn validate_encounter_reward_claim(
    character: &HeroCharacter,
    command: &RewardAwardCommand,
    claim: &NewEncounterRewardClaim,
    audit: &HeroAuditPayload,
    receipt: &NewHeroCommandReceipt,
) -> Result<(), RepositoryError> {
    let HeroAuditPayload::RewardAwarded { reward } = audit else {
        return invalid(
            "encounter reward claim",
            &claim.encounter_id,
            "encounter rewards require a reward audit",
        );
    };
    if !is_valid_opaque_id(&claim.campaign_session_id)
        || !is_valid_opaque_id(&claim.encounter_id)
        || !is_valid_opaque_id(&claim.character_id)
        || !is_valid_opaque_id(&claim.hero_audit_id)
        || claim.encounter_revision == 0
        || claim.victory_event_sequence == 0
        || !matches!(claim.reward_tier, RewardTier::Minor | RewardTier::Major)
        || claim.campaign_session_id != character.campaign_id
        || claim.character_id != character.character_id
        || claim.hero_audit_id != reward.audit_id
        || claim.reward_tier != command.tier
        || claim.reward_tier != reward.tier
        || claim.experience_awarded != reward.experience_awarded
        || claim.experience_awarded
            != TrustedRewardPolicy::MvpXpV1.experience_for(claim.reward_tier)
        || receipt.command_kind != ENCOUNTER_REWARD_COMMAND_KIND
        || receipt.audit_id != claim.hero_audit_id
    {
        return invalid(
            "encounter reward claim",
            &claim.encounter_id,
            "claim, character, derived reward, audit, and receipt do not match",
        );
    }
    Ok(())
}

fn validate_stored_encounter_claim(claim: &NewEncounterRewardClaim) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(&claim.campaign_session_id)
        || !is_valid_opaque_id(&claim.encounter_id)
        || !is_valid_opaque_id(&claim.character_id)
        || !is_valid_opaque_id(&claim.hero_audit_id)
        || claim.encounter_revision == 0
        || claim.victory_event_sequence == 0
        || claim.experience_awarded
            != TrustedRewardPolicy::MvpXpV1.experience_for(claim.reward_tier)
    {
        return invalid(
            "encounter reward claim",
            &claim.encounter_id,
            "stored claim metadata is invalid",
        );
    }
    Ok(())
}

fn audit_actor_id(audit: &HeroAuditPayload) -> &str {
    match audit {
        HeroAuditPayload::CreationTransition { transition, .. } => &transition.actor_id,
        HeroAuditPayload::RewardAwarded { reward } => &reward.actor_id,
        HeroAuditPayload::LevelUp { level_up } => &level_up.actor_id,
    }
}

fn draft_state(draft: &HeroCreationDraft) -> &'static str {
    if draft.committed_character_id.is_some() {
        "committed"
    } else {
        "active"
    }
}

fn date_from_epoch_seconds(value: u64, field: &'static str) -> Result<DateTime, RepositoryError> {
    let milliseconds = value
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(RepositoryError::NumericRange { field })?;
    Ok(DateTime::from_millis(milliseconds))
}

fn durable_revision(domain_revision: u64, field: &'static str) -> Result<u64, RepositoryError> {
    domain_revision
        .checked_add(1)
        .ok_or(RepositoryError::NumericRange { field })
}

fn hero_validation(
    entity: &'static str,
    id: &str,
    source: manchester_dnd_core::hero::HeroError,
) -> RepositoryError {
    RepositoryError::HeroValidation {
        entity,
        id: id.to_owned(),
        source,
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
    use super::*;

    #[test]
    fn hero_receipt_scopes_match_generic_collection_contract() {
        assert_eq!(HeroReceiptScope::Draft.as_str(), "hero_draft");
        assert_eq!(
            HeroReceiptScope::Character.as_str(),
            "campaign_character_instance"
        );
    }
}
