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
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use super::{
    PostgresRepository, SaveOutcome, StoredDocument, map_insert_error,
    pins::{SealAuthority, seal_campaign_pins_in_transaction},
};
use crate::error::RepositoryError;

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
            Self::Draft => "draft",
            Self::Character => "character",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewHeroCommandReceipt {
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl PostgresRepository {
    pub(crate) async fn create_hero_draft(
        &self,
        draft: &HeroCreationDraft,
        retention_delete_after_epoch_seconds: u64,
    ) -> Result<SaveOutcome, RepositoryError> {
        validate_draft(draft)?;
        if retention_delete_after_epoch_seconds < draft.expires_at_epoch_seconds {
            return invalid(
                "hero creation draft",
                &draft.draft_id,
                "retention deadline cannot precede expiry",
            );
        }
        let payload = serialize("hero creation draft", draft)?;
        let row = sqlx::query(
            "INSERT INTO hero_creation_drafts
             (id, campaign_session_id, owner_key, schema_version, revision,
              expires_at_epoch_seconds, retention_delete_after_epoch_seconds, payload_json)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb)
             RETURNING updated_at::text AS updated_at",
        )
        .bind(&draft.draft_id)
        .bind(&draft.campaign_id)
        .bind(&draft.owner_id)
        .bind(i64::from(HERO_DRAFT_SCHEMA_VERSION))
        .bind(to_i64(
            durable_revision(draft.revision, "hero draft revision")?,
            "hero draft revision",
        )?)
        .bind(to_i64(draft.expires_at_epoch_seconds, "hero draft expiry")?)
        .bind(to_i64(
            retention_delete_after_epoch_seconds,
            "hero draft retention",
        )?)
        .bind(payload)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_insert_error(error, "hero creation draft", &draft.draft_id))?;
        Ok(SaveOutcome {
            revision: durable_revision(draft.revision, "hero draft revision")?,
            updated_at: row
                .try_get("updated_at")
                .map_err(RepositoryError::Database)?,
        })
    }

    pub async fn load_hero_draft(
        &self,
        id: &str,
    ) -> Result<Option<StoredDocument<HeroCreationDraft>>, RepositoryError> {
        if !is_valid_opaque_id(id) {
            return invalid(
                "hero creation draft",
                id,
                "draft id must be a valid opaque identifier",
            );
        }
        let row = sqlx::query(
            "SELECT id, campaign_session_id, owner_key, expires_at_epoch_seconds,
                    schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_creation_drafts WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_draft_from_row).transpose()
    }

    pub async fn load_hero_character(
        &self,
        id: &str,
    ) -> Result<Option<StoredDocument<HeroCharacter>>, RepositoryError> {
        if !is_valid_opaque_id(id) {
            return invalid(
                "hero character",
                id,
                "character id must be a valid opaque identifier",
            );
        }
        let row = sqlx::query(
            "SELECT id, campaign_session_id, owner_key,
                    schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_characters WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_hero_from_row).transpose()
    }

    pub async fn load_latest_hero_draft_for_owner(
        &self,
        campaign_session_id: &str,
        owner_key: &str,
        now_epoch_seconds: u64,
    ) -> Result<Option<StoredDocument<HeroCreationDraft>>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, owner_key)?;
        let row = sqlx::query(
            "SELECT id, campaign_session_id, owner_key, expires_at_epoch_seconds,
                    schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_creation_drafts
             WHERE campaign_session_id = $1 AND owner_key = $2
               AND expires_at_epoch_seconds >= $3
             ORDER BY updated_at DESC, id DESC
             LIMIT 1",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .bind(to_i64(now_epoch_seconds, "hero draft lookup time")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_draft_from_row).transpose()
    }

    /// Loads retained theme evidence even when a newer replacement draft has
    /// not selected a theme yet. Campaign pins are immutable for the campaign,
    /// not scoped to whichever draft happens to be newest.
    pub(crate) async fn load_latest_pinned_hero_draft_for_owner(
        &self,
        campaign_session_id: &str,
        owner_key: &str,
    ) -> Result<Option<StoredDocument<HeroCreationDraft>>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, owner_key)?;
        let row = sqlx::query(
            "SELECT id, campaign_session_id, owner_key, expires_at_epoch_seconds,
                    schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_creation_drafts
             WHERE campaign_session_id = $1 AND owner_key = $2
               AND payload_json->'pins' IS NOT NULL
               AND payload_json->'pins' <> 'null'::jsonb
             ORDER BY updated_at DESC, id DESC
             LIMIT 1",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_draft_from_row).transpose()
    }

    pub async fn load_hero_character_for_owner(
        &self,
        campaign_session_id: &str,
        owner_key: &str,
    ) -> Result<Option<StoredDocument<HeroCharacter>>, RepositoryError> {
        validate_owner_lookup(campaign_session_id, owner_key)?;
        let row = sqlx::query(
            "SELECT id, campaign_session_id, owner_key,
                    schema_version, revision, payload_json::text AS payload_json,
                    created_at::text AS created_at, updated_at::text AS updated_at
             FROM hero_characters
             WHERE campaign_session_id = $1 AND owner_key = $2",
        )
        .bind(campaign_session_id)
        .bind(owner_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_hero_from_row).transpose()
    }

    pub(crate) async fn delete_retired_hero_drafts(
        &self,
        now_epoch_seconds: u64,
    ) -> Result<u64, RepositoryError> {
        let result = sqlx::query(
            "DELETE FROM hero_creation_drafts
             WHERE retention_delete_after_epoch_seconds <= $1",
        )
        .bind(to_i64(now_epoch_seconds, "hero draft cleanup time")?)
        .execute(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        Ok(result.rows_affected())
    }

    pub(crate) async fn load_hero_command_receipt(
        &self,
        scope: HeroReceiptScope,
        scope_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<StoredHeroCommandReceipt>, RepositoryError> {
        validate_receipt_lookup(scope_id, idempotency_key)?;
        let row = sqlx::query(
            "SELECT scope_kind, scope_id, campaign_session_id, idempotency_key,
                    command_kind, request_fingerprint, expected_revision, result_revision,
                    audit_id, response_json, created_at::text AS created_at
             FROM hero_command_receipts
             WHERE scope_kind = $1 AND scope_id = $2 AND idempotency_key = $3",
        )
        .bind(scope.as_str())
        .bind(scope_id)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_receipt_from_row).transpose()
    }

    pub(crate) async fn load_encounter_reward_claim(
        &self,
        campaign_session_id: &str,
        encounter_id: &str,
    ) -> Result<Option<StoredEncounterRewardClaim>, RepositoryError> {
        if !is_valid_opaque_id(campaign_session_id) || !is_valid_opaque_id(encounter_id) {
            return invalid(
                "encounter reward claim",
                encounter_id,
                "campaign and encounter ids must be valid opaque identifiers",
            );
        }
        let row = sqlx::query(
            "SELECT campaign_session_id, encounter_id, character_id,
                    encounter_revision, victory_event_sequence, reward_tier,
                    experience_awarded, hero_audit_id, created_at::text AS created_at
             FROM encounter_reward_claims
             WHERE campaign_session_id = $1 AND encounter_id = $2",
        )
        .bind(campaign_session_id)
        .bind(encounter_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_encounter_reward_claim_from_row).transpose()
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
        let HeroCreationCommitMetadata {
            receipt,
            campaign_pins,
        } = metadata;
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
        match (&command.intent, campaign_pins) {
            (
                manchester_dnd_core::hero::HeroCreationIntent::SelectCampaignTheme { pins },
                Some(evidence),
            ) if evidence.pins.hero == *pins => {}
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
                    "campaign pins must match the selected hero theme exactly",
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

        let draft_payload = serialize("hero creation draft", draft)?;
        let character_payload = created_character
            .map(|character| serialize("hero character", character))
            .transpose()?;
        let audit_payload = serialize("hero audit", audit)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let stored = load_locked_draft(&mut transaction, &draft.draft_id).await?;
        validate_draft_successor(
            &stored,
            draft,
            expected_revision,
            command,
            audit,
            created_character,
        )?;
        if let Some(evidence) = campaign_pins {
            seal_campaign_pins_in_transaction(
                &mut transaction,
                &draft.campaign_id,
                evidence,
                SealAuthority::ThemeSelection,
            )
            .await?;
        }

        let row = sqlx::query(
            "UPDATE hero_creation_drafts
             SET schema_version = $1, revision = $2, payload_json = $3::jsonb,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $4 AND revision = $5
             RETURNING updated_at::text AS updated_at",
        )
        .bind(i64::from(HERO_DRAFT_SCHEMA_VERSION))
        .bind(to_i64(
            durable_revision(draft.revision, "hero draft revision")?,
            "hero draft revision",
        )?)
        .bind(draft_payload)
        .bind(&draft.draft_id)
        .bind(to_i64(
            durable_revision(expected_revision, "expected hero draft revision")?,
            "expected hero draft revision",
        )?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::RevisionConflict {
            entity: "hero creation draft",
            id: draft.draft_id.clone(),
            expected: expected_revision,
            actual: stored.value.revision,
        })?;
        let draft_save = SaveOutcome {
            revision: durable_revision(draft.revision, "hero draft revision")?,
            updated_at: row
                .try_get("updated_at")
                .map_err(RepositoryError::Database)?,
        };

        let character_save = if let (Some(character), Some(payload)) =
            (created_character, character_payload)
        {
            let row = sqlx::query(
                "INSERT INTO hero_characters
                 (id, campaign_session_id, owner_key, schema_version, revision, payload_json)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb)
                 RETURNING updated_at::text AS updated_at",
            )
            .bind(&character.character_id)
            .bind(&character.campaign_id)
            .bind(&character.owner_id)
            .bind(i64::from(HERO_CHARACTER_SCHEMA_VERSION))
            .bind(to_i64(
                durable_revision(character.revision, "hero character revision")?,
                "hero character revision",
            )?)
            .bind(payload)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| map_insert_error(error, "hero character", &character.character_id))?;
            Some(SaveOutcome {
                revision: durable_revision(character.revision, "hero character revision")?,
                updated_at: row
                    .try_get("updated_at")
                    .map_err(RepositoryError::Database)?,
            })
        } else {
            None
        };

        insert_hero_audit(&mut transaction, &draft.campaign_id, audit, audit_payload).await?;
        insert_hero_receipt(&mut transaction, receipt).await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(HeroMutationCommitOutcome {
            subject: draft_save,
            created_character: character_save,
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
        if let HeroCharacterMutationCommand::EncounterReward { reward, claim } = command {
            validate_encounter_reward_claim(character, reward, claim, audit, receipt)?;
        }
        let character_payload = serialize("hero character", character)?;
        let audit_payload = serialize("hero audit", audit)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let stored = load_locked_hero(&mut transaction, &character.character_id).await?;
        validate_character_successor(
            &stored,
            character,
            expected_revision,
            command,
            audit,
            receipt,
        )?;

        let row = sqlx::query(
            "UPDATE hero_characters
             SET schema_version = $1, revision = $2, payload_json = $3::jsonb,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $4 AND revision = $5
             RETURNING updated_at::text AS updated_at",
        )
        .bind(i64::from(HERO_CHARACTER_SCHEMA_VERSION))
        .bind(to_i64(
            durable_revision(character.revision, "hero character revision")?,
            "hero character revision",
        )?)
        .bind(character_payload)
        .bind(&character.character_id)
        .bind(to_i64(
            durable_revision(expected_revision, "expected hero character revision")?,
            "expected hero character revision",
        )?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::RevisionConflict {
            entity: "hero character",
            id: character.character_id.clone(),
            expected: expected_revision,
            actual: stored.value.revision,
        })?;
        let save = SaveOutcome {
            revision: durable_revision(character.revision, "hero character revision")?,
            updated_at: row
                .try_get("updated_at")
                .map_err(RepositoryError::Database)?,
        };
        insert_hero_audit(
            &mut transaction,
            &character.campaign_id,
            audit,
            audit_payload,
        )
        .await?;
        if let HeroCharacterMutationCommand::EncounterReward { claim, .. } = command {
            insert_encounter_reward_claim(&mut transaction, claim).await?;
        }
        insert_hero_receipt(&mut transaction, receipt).await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(HeroMutationCommitOutcome {
            subject: save,
            created_character: None,
        })
    }

    pub async fn list_hero_audits(
        &self,
        campaign_session_id: &str,
        subject_id: &str,
    ) -> Result<Vec<StoredHeroAudit>, RepositoryError> {
        if !is_valid_opaque_id(campaign_session_id) || !is_valid_opaque_id(subject_id) {
            return invalid(
                "hero audit",
                subject_id,
                "campaign and subject ids must be valid opaque identifiers",
            );
        }
        let rows = sqlx::query(
            "SELECT id, campaign_session_id, subject_kind, subject_id, subject_revision,
                    audit_kind, schema_version,
                    occurred_at_epoch_seconds, payload_json::text AS payload_json,
                    created_at::text AS created_at
             FROM hero_audits
             WHERE campaign_session_id = $1 AND subject_id = $2
             ORDER BY subject_revision, id",
        )
        .bind(campaign_session_id)
        .bind(subject_id)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        rows.into_iter().map(stored_audit_from_row).collect()
    }
}

fn validate_owner_lookup(
    campaign_session_id: &str,
    owner_key: &str,
) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(campaign_session_id) || !is_valid_opaque_id(owner_key) {
        return invalid(
            "hero owner workspace",
            owner_key,
            "campaign and owner keys must be valid opaque identifiers",
        );
    }
    Ok(())
}

async fn load_locked_draft(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<StoredDocument<HeroCreationDraft>, RepositoryError> {
    let row = sqlx::query(
        "SELECT id, campaign_session_id, owner_key, expires_at_epoch_seconds,
                schema_version, revision, payload_json::text AS payload_json,
                created_at::text AS created_at, updated_at::text AS updated_at
         FROM hero_creation_drafts WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?
    .ok_or_else(|| RepositoryError::NotFound {
        entity: "hero creation draft",
        id: id.to_owned(),
    })?;
    stored_draft_from_row(row)
}

async fn load_locked_hero(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<StoredDocument<HeroCharacter>, RepositoryError> {
    let row = sqlx::query(
        "SELECT id, campaign_session_id, owner_key,
                schema_version, revision, payload_json::text AS payload_json,
                created_at::text AS created_at, updated_at::text AS updated_at
         FROM hero_characters WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?
    .ok_or_else(|| RepositoryError::NotFound {
        entity: "hero character",
        id: id.to_owned(),
    })?;
    stored_hero_from_row(row)
}

fn stored_draft_from_row(row: PgRow) -> Result<StoredDocument<HeroCreationDraft>, RepositoryError> {
    let campaign_session_id: String = row
        .try_get("campaign_session_id")
        .map_err(RepositoryError::Database)?;
    let owner_key: String = row
        .try_get("owner_key")
        .map_err(RepositoryError::Database)?;
    let expires_at_epoch_seconds = from_i64(
        row.try_get("expires_at_epoch_seconds")
            .map_err(RepositoryError::Database)?,
        "hero draft expiry",
    )?;
    let stored = stored_document_from_row(row, "hero creation draft")?;
    validate_draft(&stored.value)?;
    if stored.schema_version != u32::from(HERO_DRAFT_SCHEMA_VERSION)
        || stored.id != stored.value.draft_id
        || stored.revision != durable_revision(stored.value.revision, "hero draft revision")?
        || campaign_session_id != stored.value.campaign_id
        || owner_key != stored.value.owner_id
        || expires_at_epoch_seconds != stored.value.expires_at_epoch_seconds
    {
        return invalid(
            "hero creation draft",
            &stored.id,
            "row metadata and validated draft payload do not match",
        );
    }
    Ok(stored)
}

fn stored_hero_from_row(row: PgRow) -> Result<StoredDocument<HeroCharacter>, RepositoryError> {
    let campaign_session_id: String = row
        .try_get("campaign_session_id")
        .map_err(RepositoryError::Database)?;
    let owner_key: String = row
        .try_get("owner_key")
        .map_err(RepositoryError::Database)?;
    let stored = stored_document_from_row(row, "hero character")?;
    validate_character(&stored.value)?;
    if stored.schema_version != u32::from(HERO_CHARACTER_SCHEMA_VERSION)
        || stored.id != stored.value.character_id
        || stored.revision != durable_revision(stored.value.revision, "hero character revision")?
        || campaign_session_id != stored.value.campaign_id
        || owner_key != stored.value.owner_id
    {
        return invalid(
            "hero character",
            &stored.id,
            "row metadata and validated character payload do not match",
        );
    }
    Ok(stored)
}

/// Commits the authoritative hero runtime snapshot while the caller still owns
/// the surrounding session-event transaction. The successor is reconstructed
/// from the locked row so this path can change only current HP/resource counts.
pub(super) async fn commit_encounter_hero_update(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    event: &SessionEventDto,
    update: EncounterHeroUpdate<'_>,
) -> Result<SaveOutcome, RepositoryError> {
    validate_character(update.character)?;
    let stored = load_locked_hero(transaction, &update.character.character_id).await?;
    if stored.value.campaign_id != campaign_session_id {
        return invalid(
            "hero character",
            &stored.id,
            "encounter hero is not linked to the campaign session",
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
                "an encounter hero update requires an encounter resolution event",
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
        reason: "hero resources cannot be projected into the encounter runtime",
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
            "encounter outcome and authoritative hero revision/runtime do not match",
        );
    }

    if stored.value == *update.character {
        return Ok(SaveOutcome {
            revision: stored.revision,
            updated_at: stored.updated_at,
        });
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

    let payload = serialize("hero character", update.character)?;
    let next_durable_revision =
        durable_revision(update.character.revision, "hero character revision")?;
    let expected_durable_revision =
        durable_revision(update.expected_revision, "expected hero character revision")?;
    let row = sqlx::query(
        "UPDATE hero_characters
         SET schema_version = $1, revision = $2, payload_json = $3::jsonb,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = $4 AND campaign_session_id = $5 AND revision = $6
         RETURNING updated_at::text AS updated_at",
    )
    .bind(i64::from(HERO_CHARACTER_SCHEMA_VERSION))
    .bind(to_i64(next_durable_revision, "hero character revision")?)
    .bind(payload)
    .bind(&update.character.character_id)
    .bind(campaign_session_id)
    .bind(to_i64(
        expected_durable_revision,
        "expected hero character revision",
    )?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?
    .ok_or_else(|| RepositoryError::RevisionConflict {
        entity: "hero character",
        id: update.character.character_id.clone(),
        expected: update.expected_revision,
        actual: update.expected_revision,
    })?;
    Ok(SaveOutcome {
        revision: next_durable_revision,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn stored_document_from_row<T>(
    row: PgRow,
    entity: &'static str,
) -> Result<StoredDocument<T>, RepositoryError>
where
    T: for<'de> Deserialize<'de>,
{
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let value =
        serde_json::from_str(&payload).map_err(|source| RepositoryError::InvalidStoredData {
            entity,
            id: id.clone(),
            source,
        })?;
    Ok(StoredDocument {
        id,
        schema_version: from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "hero schema version",
        )?,
        revision: from_i64(
            row.try_get("revision").map_err(RepositoryError::Database)?,
            "hero revision",
        )?,
        value,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn stored_audit_from_row(row: PgRow) -> Result<StoredHeroAudit, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let payload: HeroAuditPayload = serde_json::from_str(&payload_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity: "hero audit",
            id: id.clone(),
            source,
        }
    })?;
    payload.validate()?;
    let subject_kind: String = row
        .try_get("subject_kind")
        .map_err(RepositoryError::Database)?;
    let audit_kind: String = row
        .try_get("audit_kind")
        .map_err(RepositoryError::Database)?;
    let audit = StoredHeroAudit {
        id: id.clone(),
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        subject_id: row
            .try_get("subject_id")
            .map_err(RepositoryError::Database)?,
        subject_revision: from_i64(
            row.try_get("subject_revision")
                .map_err(RepositoryError::Database)?,
            "hero audit revision",
        )?,
        schema_version: from_i64_u32(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "hero audit schema version",
        )?,
        occurred_at_epoch_seconds: from_i64(
            row.try_get("occurred_at_epoch_seconds")
                .map_err(RepositoryError::Database)?,
            "hero audit time",
        )?,
        payload,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    };
    if audit.id != audit.payload.audit_id()
        || audit.subject_id != audit.payload.subject_id()
        || subject_kind != audit.payload.subject_kind().as_str()
        || audit_kind != audit.payload.kind()
        || audit.subject_revision != audit.payload.subject_revision()
        || audit.schema_version != u32::from(HERO_AUDIT_SCHEMA_VERSION)
        || audit.occurred_at_epoch_seconds != audit.payload.occurred_at_epoch_seconds()
    {
        return invalid(
            "hero audit",
            &audit.id,
            "row metadata and validated audit payload do not match",
        );
    }
    Ok(audit)
}

fn stored_receipt_from_row(row: PgRow) -> Result<StoredHeroCommandReceipt, RepositoryError> {
    let scope_value: String = row
        .try_get("scope_kind")
        .map_err(RepositoryError::Database)?;
    let scope = match scope_value.as_str() {
        "draft" => HeroReceiptScope::Draft,
        "character" => HeroReceiptScope::Character,
        _ => {
            return invalid(
                "hero command receipt",
                &scope_value,
                "stored receipt scope is unsupported",
            );
        }
    };
    let fingerprint: String = row
        .try_get("request_fingerprint")
        .map_err(RepositoryError::Database)?;
    let receipt = StoredHeroCommandReceipt {
        scope,
        scope_id: row.try_get("scope_id").map_err(RepositoryError::Database)?,
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(RepositoryError::Database)?,
        command_kind: row
            .try_get("command_kind")
            .map_err(RepositoryError::Database)?,
        request_fingerprint: Sha256Digest::new(fingerprint).map_err(|source| {
            RepositoryError::CoreValidation {
                entity: "hero command receipt",
                id: "stored-fingerprint".to_owned(),
                source,
            }
        })?,
        expected_revision: from_i64(
            row.try_get("expected_revision")
                .map_err(RepositoryError::Database)?,
            "hero receipt expected revision",
        )?,
        result_revision: from_i64(
            row.try_get("result_revision")
                .map_err(RepositoryError::Database)?,
            "hero receipt result revision",
        )?,
        audit_id: row.try_get("audit_id").map_err(RepositoryError::Database)?,
        response_json: row
            .try_get("response_json")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    };
    validate_stored_receipt(&receipt)?;
    Ok(receipt)
}

fn stored_encounter_reward_claim_from_row(
    row: PgRow,
) -> Result<StoredEncounterRewardClaim, RepositoryError> {
    let tier: String = row
        .try_get("reward_tier")
        .map_err(RepositoryError::Database)?;
    let reward_tier = match tier.as_str() {
        "minor" => RewardTier::Minor,
        "major" => RewardTier::Major,
        _ => {
            return invalid(
                "encounter reward claim",
                &tier,
                "stored reward tier is unsupported",
            );
        }
    };
    let claim = StoredEncounterRewardClaim {
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        encounter_id: row
            .try_get("encounter_id")
            .map_err(RepositoryError::Database)?,
        character_id: row
            .try_get("character_id")
            .map_err(RepositoryError::Database)?,
        encounter_revision: from_i64(
            row.try_get("encounter_revision")
                .map_err(RepositoryError::Database)?,
            "encounter reward revision",
        )?,
        victory_event_sequence: from_i64(
            row.try_get("victory_event_sequence")
                .map_err(RepositoryError::Database)?,
            "encounter reward event sequence",
        )?,
        reward_tier,
        experience_awarded: from_i64_u32(
            row.try_get("experience_awarded")
                .map_err(RepositoryError::Database)?,
            "encounter reward experience",
        )?,
        hero_audit_id: row
            .try_get("hero_audit_id")
            .map_err(RepositoryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    };
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
    Ok(claim)
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
            "locked draft replay does not equal the submitted state and audit",
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
            "successor, audit scope, and receipt must preserve identity and base choices",
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
            && stored.value.advancement_choices == submitted.advancement_choices =>
        {
            // Exact transition is replayed below.
        }
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
        _ => invalid(
            "hero character",
            &submitted.character_id,
            "character successor does not match its immutable reward or level-up audit",
        )?,
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
                    "trusted reward replay does not equal the immutable audit",
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
                    "level-up replay does not equal the immutable audit",
                );
            }
        }
        _ => unreachable!("command/audit pair was checked above"),
    }
    if replayed != *submitted {
        return invalid(
            "hero character",
            &submitted.character_id,
            "locked character replay does not equal the submitted successor",
        );
    }
    Ok(())
}

fn validate_receipt(
    receipt: &NewHeroCommandReceipt,
    audit: &HeroAuditPayload,
) -> Result<(), RepositoryError> {
    validate_receipt_lookup(&receipt.scope_id, &receipt.idempotency_key)?;
    if !is_valid_opaque_id(&receipt.campaign_session_id)
        || !is_valid_opaque_id(&receipt.command_kind)
        || !is_valid_opaque_id(&receipt.audit_id)
        || receipt.audit_id != audit.audit_id()
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
    validate_receipt_lookup(&receipt.scope_id, &receipt.idempotency_key)?;
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
            "stored receipt is invalid",
        );
    }
    Ok(())
}

fn validate_receipt_lookup(scope_id: &str, idempotency_key: &str) -> Result<(), RepositoryError> {
    if !is_valid_opaque_id(scope_id) || !is_valid_opaque_id(idempotency_key) {
        invalid(
            "hero command receipt",
            scope_id,
            "scope and idempotency ids must be valid opaque identifiers",
        )
    } else {
        Ok(())
    }
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

async fn insert_hero_audit(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    audit: &HeroAuditPayload,
    payload: String,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO hero_audits
         (id, campaign_session_id, subject_kind, subject_id, audit_kind, schema_version,
          subject_revision, occurred_at_epoch_seconds, payload_json)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb)",
    )
    .bind(audit.audit_id())
    .bind(campaign_session_id)
    .bind(audit.subject_kind().as_str())
    .bind(audit.subject_id())
    .bind(audit.kind())
    .bind(i64::from(HERO_AUDIT_SCHEMA_VERSION))
    .bind(to_i64(audit.subject_revision(), "hero audit revision")?)
    .bind(to_i64(
        audit.occurred_at_epoch_seconds(),
        "hero audit time",
    )?)
    .bind(payload)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_insert_error(error, "hero audit", audit.audit_id()))?;
    Ok(())
}

async fn insert_encounter_reward_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &NewEncounterRewardClaim,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO encounter_reward_claims
         (campaign_session_id, encounter_id, character_id, encounter_revision,
          victory_event_sequence, reward_tier, experience_awarded, hero_audit_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&claim.campaign_session_id)
    .bind(&claim.encounter_id)
    .bind(&claim.character_id)
    .bind(to_i64(
        claim.encounter_revision,
        "encounter reward revision",
    )?)
    .bind(to_i64(
        claim.victory_event_sequence,
        "encounter reward event sequence",
    )?)
    .bind(match claim.reward_tier {
        RewardTier::Minor => "minor",
        RewardTier::Major => "major",
        RewardTier::Significant => "significant",
    })
    .bind(i64::from(claim.experience_awarded))
    .bind(&claim.hero_audit_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        map_insert_error(
            error,
            "encounter reward claim",
            &format!("{}:{}", claim.campaign_session_id, claim.encounter_id),
        )
    })?;
    Ok(())
}

async fn insert_hero_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    receipt: &NewHeroCommandReceipt,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO hero_command_receipts
         (scope_kind, scope_id, campaign_session_id, idempotency_key, command_kind,
          request_fingerprint, expected_revision, result_revision, audit_id, response_json)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(receipt.scope.as_str())
    .bind(&receipt.scope_id)
    .bind(&receipt.campaign_session_id)
    .bind(&receipt.idempotency_key)
    .bind(&receipt.command_kind)
    .bind(receipt.request_fingerprint.as_str())
    .bind(to_i64(
        receipt.expected_revision,
        "hero receipt expected revision",
    )?)
    .bind(to_i64(
        receipt.result_revision,
        "hero receipt result revision",
    )?)
    .bind(&receipt.audit_id)
    .bind(&receipt.response_json)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        map_insert_error(
            error,
            "hero command receipt",
            &format!(
                "{}:{}:{}",
                receipt.scope.as_str(),
                receipt.scope_id,
                receipt.idempotency_key
            ),
        )
    })?;
    Ok(())
}

fn serialize<T: Serialize>(entity: &'static str, value: &T) -> Result<String, RepositoryError> {
    serde_json::to_string(value).map_err(|source| RepositoryError::Serialize { entity, source })
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn durable_revision(domain_revision: u64, field: &'static str) -> Result<u64, RepositoryError> {
    domain_revision
        .checked_add(1)
        .ok_or(RepositoryError::NumericRange { field })
}

fn from_i64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
}

fn from_i64_u32(value: i64, field: &'static str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|_| RepositoryError::NumericRange { field })
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
