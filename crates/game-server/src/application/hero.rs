use manchester_dnd_core::{
    RewardTier, Sha256Digest,
    encounter::{EncounterRewardTier, EncounterStatus, RewardEligibility, SOOT_WIGHT_ENCOUNTER_ID},
    hero::{
        HeroCharacter, HeroCreationCommand, HeroCreationDraft, HeroCreationIntent,
        HeroCreationOutcome, HeroError, LevelUpAuditDto, LevelUpChoice, LevelUpCommand,
        RewardAwardAuditDto, RewardAwardCommand, TrustedMutationContext, TrustedRewardPolicy,
    },
    is_valid_opaque_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    GameApplicationService, LOCAL_CAMPAIGN_SESSION_ID, encounter_profile_from_hero,
    latest_exploration_check, map_campaign_pin_repository_error, project_encounter,
    validate_event_stream,
};
use crate::{
    error::{ApplicationError, RepositoryError},
    repository::{
        HeroAuditPayload, HeroCharacterMutationCommand, HeroCreationCommitMetadata,
        HeroReceiptScope, NewEncounterRewardClaim, NewHeroCommandReceipt, StoredHeroCommandReceipt,
    },
};

pub const HERO_APPLICATION_SCHEMA_VERSION: u16 = 1;
pub const LOCAL_HERO_OWNER_KEY: &str = "local-owner";
pub const HERO_DRAFT_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const HERO_DRAFT_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;

const CREATION_COMMAND_KIND: &str = "hero_creation_transition";
const REWARD_COMMAND_KIND: &str = "hero_reward";
const LEVEL_UP_COMMAND_KIND: &str = "hero_level_up";
const ENCOUNTER_REWARD_COMMAND_KIND: &str = "encounter_reward_claim";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimEncounterRewardCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub expected_campaign_revision: u64,
    pub expected_character_revision: u64,
    pub idempotency_key: String,
}

impl ClaimEncounterRewardCommand {
    pub fn validate(&self) -> Result<(), HeroError> {
        if self.schema_version != HERO_APPLICATION_SCHEMA_VERSION {
            return Err(HeroError::InvalidSchemaVersion {
                expected: HERO_APPLICATION_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.character_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_campaign_revision == 0
        {
            return Err(HeroError::InvalidField {
                field: "encounter_reward_claim",
                reason: "campaign, character, idempotency, and campaign revision are invalid",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterRewardClaimOutcomeDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub encounter_id: String,
    pub character: HeroCharacter,
    pub audit: RewardAwardAuditDto,
    pub eligibility: HeroLevelUpChoicesDto,
}

impl EncounterRewardClaimOutcomeDto {
    pub fn validate(&self) -> Result<(), HeroError> {
        if self.schema_version != HERO_APPLICATION_SCHEMA_VERSION {
            return Err(HeroError::InvalidSchemaVersion {
                expected: HERO_APPLICATION_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        self.character.validate()?;
        self.audit.validate()?;
        if self.campaign_session_id != self.character.campaign_id
            || self.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
            || !matches!(self.audit.tier, RewardTier::Minor | RewardTier::Major)
            || self.audit.character_id != self.character.character_id
            || self.audit.revision_after != self.character.revision
            || self.audit.experience_after != self.character.experience_points
            || self.eligibility.character_id != self.character.character_id
            || self.eligibility.revision != self.character.revision
            || self.eligibility.eligible != self.character.level_up_eligible()
            || self.eligibility.choices != self.character.valid_level_up_choices()?
        {
            return Err(HeroError::InvalidField {
                field: "encounter_reward_claim_outcome",
                reason: "claim identity, derived reward, character, and eligibility do not match",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroRewardOutcomeDto {
    pub schema_version: u16,
    pub character: HeroCharacter,
    pub audit: RewardAwardAuditDto,
}

impl HeroRewardOutcomeDto {
    pub fn validate(&self) -> Result<(), HeroError> {
        if self.schema_version != HERO_APPLICATION_SCHEMA_VERSION {
            return Err(HeroError::InvalidSchemaVersion {
                expected: HERO_APPLICATION_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        self.character.validate()?;
        self.audit.validate()?;
        if self.character.character_id != self.audit.character_id
            || self.character.revision != self.audit.revision_after
            || self.character.experience_points != self.audit.experience_after
            || self.character.level != self.audit.level
        {
            return Err(HeroError::InvalidField {
                field: "reward_outcome",
                reason: "character state does not match the immutable reward audit",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroLevelUpChoicesDto {
    pub schema_version: u16,
    pub character_id: String,
    pub revision: u64,
    pub eligible: bool,
    pub choices: Vec<LevelUpChoice>,
}

impl HeroLevelUpChoicesDto {
    fn from_character(character: &HeroCharacter) -> Result<Self, HeroError> {
        let choices = character.valid_level_up_choices()?;
        Ok(Self {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            character_id: character.character_id.clone(),
            revision: character.revision,
            eligible: character.level_up_eligible(),
            choices,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroLevelUpOutcomeDto {
    pub schema_version: u16,
    pub character: HeroCharacter,
    pub audit: LevelUpAuditDto,
}

/// The durable local-owner hero state needed to resume after a browser refresh
/// without trusting a client-cached draft or character identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalHeroWorkspaceDto {
    pub schema_version: u16,
    pub draft: Option<HeroCreationDraft>,
    pub character: Option<HeroCharacter>,
}

impl LocalHeroWorkspaceDto {
    pub fn validate(&self, now_epoch_seconds: u64) -> Result<(), HeroError> {
        if self.schema_version != HERO_APPLICATION_SCHEMA_VERSION {
            return Err(HeroError::InvalidSchemaVersion {
                expected: HERO_APPLICATION_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if let Some(draft) = &self.draft {
            draft.validate()?;
            if draft.campaign_id != LOCAL_CAMPAIGN_SESSION_ID
                || draft.owner_id != LOCAL_HERO_OWNER_KEY
                || draft.is_expired(now_epoch_seconds)
            {
                return Err(HeroError::InvalidField {
                    field: "workspace.draft",
                    reason: "draft must be an unexpired local-owner document",
                });
            }
        }
        if let Some(character) = &self.character {
            character.validate()?;
            if character.campaign_id != LOCAL_CAMPAIGN_SESSION_ID
                || character.owner_id != LOCAL_HERO_OWNER_KEY
            {
                return Err(HeroError::InvalidField {
                    field: "workspace.character",
                    reason: "character must be a local-owner document",
                });
            }
        }
        Ok(())
    }
}

impl HeroLevelUpOutcomeDto {
    pub fn validate(&self) -> Result<(), HeroError> {
        if self.schema_version != HERO_APPLICATION_SCHEMA_VERSION {
            return Err(HeroError::InvalidSchemaVersion {
                expected: HERO_APPLICATION_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        self.character.validate()?;
        self.audit.validate()?;
        if self.character.character_id != self.audit.character_id
            || self.character.revision != self.audit.revision_after
            || self.character.level != self.audit.level_after
            || self.character.sheet.maximum_hit_points != self.audit.maximum_hit_points_after
            || self.character.sheet.resources != self.audit.resources_after
            || self.character.advancement_choices.as_slice()
                != std::slice::from_ref(&self.audit.choice)
        {
            return Err(HeroError::InvalidField {
                field: "level_up_outcome",
                reason: "character state does not match the immutable level-up audit",
            });
        }
        Ok(())
    }
}

impl GameApplicationService {
    /// Resolves the latest unexpired creation draft and the owner's unique
    /// created hero from server authority. No browser-stored IDs are required.
    pub async fn load_local_hero_workspace(
        &self,
    ) -> Result<LocalHeroWorkspaceDto, ApplicationError> {
        self.require_local_mode()?;
        if let Some(session) = self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
        {
            self.resolve_campaign_pin_status(&session).await?;
        }
        let now = self.hero_now_epoch_seconds();
        self.repository
            .delete_retired_hero_drafts(now)
            .await
            .map_err(ApplicationError::Repository)?;
        let draft = self
            .repository
            .load_latest_hero_draft_for_owner(LOCAL_CAMPAIGN_SESSION_ID, LOCAL_HERO_OWNER_KEY, now)
            .await
            .map_err(ApplicationError::Repository)?
            .map(|stored| stored.value);
        let character = self
            .repository
            .load_hero_character_for_owner(LOCAL_CAMPAIGN_SESSION_ID, LOCAL_HERO_OWNER_KEY)
            .await
            .map_err(ApplicationError::Repository)?
            .map(|stored| stored.value);
        let workspace = LocalHeroWorkspaceDto {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            draft,
            character,
        };
        workspace.validate(now).map_err(ApplicationError::Hero)?;
        Ok(workspace)
    }

    /// Starts a new server-owned, resumable local creation draft. Existing Slice 1
    /// campaign/character documents remain untouched.
    pub async fn start_local_hero_creation(&self) -> Result<HeroCreationDraft, ApplicationError> {
        self.require_local_mode()?;
        let _guard = self.command_gate.lock().await;
        let (session, _) = self.load_or_create_local_campaign().await?;
        self.resolve_campaign_pin_status(&session).await?;
        let now = self.hero_now_epoch_seconds();
        self.repository
            .delete_retired_hero_drafts(now)
            .await
            .map_err(ApplicationError::Repository)?;
        let expires_at = now
            .checked_add(HERO_DRAFT_TTL_SECONDS)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let retention_delete_after = expires_at
            .checked_add(HERO_DRAFT_RETENTION_SECONDS)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let draft = HeroCreationDraft::new(
            format!("hero-draft:{}", Uuid::new_v4().simple()),
            LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            LOCAL_HERO_OWNER_KEY.to_owned(),
            expires_at,
        )
        .map_err(ApplicationError::Hero)?;
        self.repository
            .create_hero_draft(&draft, retention_delete_after)
            .await
            .map_err(ApplicationError::Repository)?;
        Ok(draft)
    }

    pub async fn load_local_hero_creation(
        &self,
        draft_id: &str,
    ) -> Result<HeroCreationDraft, ApplicationError> {
        self.require_local_mode()?;
        let stored = self
            .repository
            .load_hero_draft(draft_id)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::HeroNotFound)?;
        validate_local_draft(&stored.value)?;
        if stored.value.is_expired(self.hero_now_epoch_seconds()) {
            return Err(ApplicationError::HeroDraftExpired);
        }
        Ok(stored.value)
    }

    /// Applies any non-final creation step. A `Commit` intent is also safe here and
    /// uses the same atomic finalization transaction as [`Self::finalize_hero_creation`].
    pub async fn apply_hero_creation_command(
        &self,
        command: HeroCreationCommand,
    ) -> Result<HeroCreationOutcome, ApplicationError> {
        self.require_local_mode()?;
        command.validate().map_err(ApplicationError::Hero)?;
        let fingerprint = fingerprint_creation_command(&command)?;
        let _guard = self.command_gate.lock().await;
        if let Some(receipt) = self
            .repository
            .load_hero_command_receipt(
                HeroReceiptScope::Draft,
                &command.draft_id,
                &command.idempotency_key,
            )
            .await
            .map_err(ApplicationError::Repository)?
        {
            return creation_outcome_from_receipt(&command, &fingerprint, &receipt);
        }

        let session = self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::InvalidStoredState)?;
        let campaign_pins = match &command.intent {
            HeroCreationIntent::SelectCampaignTheme { pins } => {
                Some(self.pins_for_theme_selection(&session, pins).await?)
            }
            _ => {
                self.require_sealed_campaign_pins(&session).await?;
                None
            }
        };

        let stored = self
            .repository
            .load_hero_draft(&command.draft_id)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::HeroNotFound)?;
        validate_local_draft(&stored.value)?;
        let now = self.hero_now_epoch_seconds();
        if stored.value.is_expired(now) {
            return Err(ApplicationError::HeroDraftExpired);
        }
        if command.expected_revision != stored.value.revision {
            return Err(ApplicationError::HeroRevisionConflict {
                expected: command.expected_revision,
                current_revision: stored.value.revision,
            });
        }

        let audit_id = format!("hero-audit:{}", Uuid::new_v4().simple());
        let mut draft = stored.value;
        let outcome = draft
            .apply_trusted(
                &command,
                &TrustedMutationContext {
                    audit_id: audit_id.clone(),
                    actor_id: LOCAL_HERO_OWNER_KEY.to_owned(),
                    occurred_at_epoch_seconds: now,
                },
            )
            .map_err(map_hero_error)?;
        validate_creation_outcome(&command, &outcome)?;
        let audit = HeroAuditPayload::CreationTransition {
            transition: Box::new(outcome.transition_audit.clone()),
            character_created: outcome.created_audit.clone().map(Box::new),
        };
        let response_json =
            serde_json::to_string(&outcome).map_err(ApplicationError::Serialization)?;
        let receipt = NewHeroCommandReceipt {
            scope: HeroReceiptScope::Draft,
            scope_id: command.draft_id.clone(),
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: CREATION_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint.clone(),
            expected_revision: command.expected_revision,
            result_revision: draft.revision,
            audit_id,
            response_json,
        };
        match self
            .repository
            .commit_hero_creation_transition(
                &draft,
                command.expected_revision,
                &command,
                &audit,
                outcome.character.as_ref(),
                HeroCreationCommitMetadata {
                    receipt: &receipt,
                    campaign_pins: campaign_pins.as_ref(),
                },
            )
            .await
        {
            Ok(committed)
                if committed.subject.revision == draft.revision + 1
                    && committed.created_character.is_some() == outcome.character.is_some() =>
            {
                Ok(outcome)
            }
            Ok(_) => Err(ApplicationError::InvalidStoredState),
            Err(RepositoryError::RevisionConflict { actual, .. }) => {
                if let Some(stored_receipt) = self
                    .repository
                    .load_hero_command_receipt(
                        HeroReceiptScope::Draft,
                        &command.draft_id,
                        &command.idempotency_key,
                    )
                    .await
                    .map_err(ApplicationError::Repository)?
                {
                    creation_outcome_from_receipt(&command, &fingerprint, &stored_receipt)
                } else {
                    Err(ApplicationError::HeroRevisionConflict {
                        expected: command.expected_revision,
                        current_revision: actual,
                    })
                }
            }
            Err(RepositoryError::AlreadyExists {
                entity: "hero command receipt",
                ..
            }) => {
                let stored_receipt = self
                    .repository
                    .load_hero_command_receipt(
                        HeroReceiptScope::Draft,
                        &command.draft_id,
                        &command.idempotency_key,
                    )
                    .await
                    .map_err(ApplicationError::Repository)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                creation_outcome_from_receipt(&command, &fingerprint, &stored_receipt)
            }
            Err(error) => Err(map_campaign_pin_repository_error(error)),
        }
    }

    pub async fn finalize_hero_creation(
        &self,
        command: HeroCreationCommand,
    ) -> Result<HeroCreationOutcome, ApplicationError> {
        if !matches!(command.intent, HeroCreationIntent::Commit { .. }) {
            return Err(ApplicationError::Hero(HeroError::InvalidField {
                field: "intent",
                reason: "finalization requires the commit intent",
            }));
        }
        self.apply_hero_creation_command(command).await
    }

    pub async fn load_local_created_hero(
        &self,
        character_id: &str,
    ) -> Result<HeroCharacter, ApplicationError> {
        self.require_local_mode()?;
        let session = self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::InvalidStoredState)?;
        self.require_sealed_campaign_pins(&session).await?;
        let stored = self
            .repository
            .load_hero_character(character_id)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::HeroNotFound)?;
        validate_local_character(&stored.value)?;
        Ok(stored.value)
    }

    /// Applies the one pinned server policy from a closed reward tier. No raw XP,
    /// HP, level, resource, or derived-sheet field crosses this boundary.
    pub async fn apply_hero_reward(
        &self,
        command: RewardAwardCommand,
    ) -> Result<HeroRewardOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        command.validate().map_err(ApplicationError::Hero)?;
        let fingerprint = fingerprint_reward_command(&command)?;
        let _guard = self.command_gate.lock().await;
        let (session, _) = self.load_or_create_local_campaign().await?;
        self.require_sealed_campaign_pins(&session).await?;
        if let Some(receipt) = self
            .repository
            .load_hero_command_receipt(
                HeroReceiptScope::Character,
                &command.character_id,
                &command.idempotency_key,
            )
            .await
            .map_err(ApplicationError::Repository)?
        {
            return reward_outcome_from_receipt(&command, &fingerprint, &receipt);
        }

        let stored = self
            .repository
            .load_hero_character(&command.character_id)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::HeroNotFound)?;
        validate_local_character(&stored.value)?;
        if command.expected_revision != stored.value.revision {
            return Err(ApplicationError::HeroRevisionConflict {
                expected: command.expected_revision,
                current_revision: stored.value.revision,
            });
        }
        let audit_id = format!("hero-audit:{}", Uuid::new_v4().simple());
        let mut character = stored.value;
        let audit = character
            .apply_reward(
                &command,
                TrustedRewardPolicy::MvpXpV1,
                &TrustedMutationContext {
                    audit_id: audit_id.clone(),
                    actor_id: LOCAL_HERO_OWNER_KEY.to_owned(),
                    occurred_at_epoch_seconds: self.hero_now_epoch_seconds(),
                },
            )
            .map_err(map_hero_error)?;
        let outcome = HeroRewardOutcomeDto {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            character,
            audit,
        };
        outcome.validate().map_err(ApplicationError::Hero)?;
        let audit_payload = HeroAuditPayload::RewardAwarded {
            reward: outcome.audit.clone(),
        };
        let receipt = NewHeroCommandReceipt {
            scope: HeroReceiptScope::Character,
            scope_id: command.character_id.clone(),
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: REWARD_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint.clone(),
            expected_revision: command.expected_revision,
            result_revision: outcome.character.revision,
            audit_id,
            response_json: serde_json::to_string(&outcome)
                .map_err(ApplicationError::Serialization)?,
        };
        if let Some(replayed) = commit_character_outcome(
            self,
            CharacterCommitRequest {
                character_id: &command.character_id,
                expected_revision: command.expected_revision,
                idempotency_key: &command.idempotency_key,
                fingerprint: &fingerprint,
                character: &outcome.character,
                command: HeroCharacterMutationCommand::Reward(&command),
                audit: &audit_payload,
                receipt: &receipt,
            },
            |receipt| reward_outcome_from_receipt(&command, &fingerprint, receipt),
        )
        .await?
        {
            return Ok(replayed);
        }
        Ok(outcome)
    }

    /// Claims the fixed encounter's reward from committed victory authority.
    /// The public command carries no tier or XP; both are derived from the
    /// terminal encounter state and the pinned trusted reward policy.
    pub async fn claim_local_encounter_reward(
        &self,
        command: ClaimEncounterRewardCommand,
    ) -> Result<EncounterRewardClaimOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        command.validate().map_err(ApplicationError::Hero)?;
        if command.campaign_session_id != LOCAL_CAMPAIGN_SESSION_ID {
            return Err(ApplicationError::WrongCampaign);
        }
        let fingerprint = fingerprint_encounter_reward_claim(&command)?;
        let _guard = self.command_gate.lock().await;
        let (session, _) = self.load_or_create_local_campaign().await?;
        self.require_sealed_campaign_pins(&session).await?;

        if let Some(receipt) = self
            .repository
            .load_hero_command_receipt(
                HeroReceiptScope::Character,
                &command.character_id,
                &command.idempotency_key,
            )
            .await
            .map_err(ApplicationError::Repository)?
        {
            return encounter_reward_outcome_from_receipt(&command, &fingerprint, &receipt);
        }

        if command.expected_campaign_revision != session.revision {
            return Err(ApplicationError::RevisionConflict {
                expected: command.expected_campaign_revision,
                current_revision: session.revision,
            });
        }
        let stored_hero = self
            .load_local_authoritative_hero()
            .await?
            .ok_or(ApplicationError::HeroNotFound)?;
        if stored_hero.value.character_id != command.character_id {
            return Err(ApplicationError::WrongCharacter);
        }
        let current_profile = encounter_profile_from_hero(&stored_hero.value)?;
        let events = self
            .repository
            .list_session_events(&session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        validate_event_stream(&session, &events)?;
        let latest_check = latest_exploration_check(&events)?
            .ok_or(ApplicationError::EncounterRewardUnavailable)?;
        let campaign_seed = self
            .seed_vault
            .derive_campaign_seed(&session.id)
            .map_err(ApplicationError::SeedVault)?;
        let projected = project_encounter(
            &session,
            &latest_check,
            &events,
            campaign_seed.expose_to_engine(),
            campaign_seed.reference(),
            Some(&current_profile),
        )?;
        let encounter_reward_tier = match projected.view.state.reward_eligibility {
            RewardEligibility::Eligible { tier }
                if projected.view.state.status == EncounterStatus::Victory =>
            {
                tier
            }
            _ => {
                return Err(ApplicationError::EncounterRewardUnavailable);
            }
        };
        let reward_tier = match encounter_reward_tier {
            EncounterRewardTier::Minor => RewardTier::Minor,
            EncounterRewardTier::Major => RewardTier::Major,
        };
        if projected.view.state.hero.source_character_id.as_deref()
            != Some(command.character_id.as_str())
        {
            return Err(ApplicationError::WrongCharacter);
        }
        if self
            .repository
            .load_encounter_reward_claim(&command.campaign_session_id, SOOT_WIGHT_ENCOUNTER_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .is_some()
        {
            if let Some(receipt) = self
                .repository
                .load_hero_command_receipt(
                    HeroReceiptScope::Character,
                    &command.character_id,
                    &command.idempotency_key,
                )
                .await
                .map_err(ApplicationError::Repository)?
            {
                return encounter_reward_outcome_from_receipt(&command, &fingerprint, &receipt);
            }
            return Err(ApplicationError::EncounterRewardAlreadyClaimed);
        }
        if command.expected_character_revision != stored_hero.value.revision {
            return Err(ApplicationError::HeroRevisionConflict {
                expected: command.expected_character_revision,
                current_revision: stored_hero.value.revision,
            });
        }

        let reward_command = RewardAwardCommand {
            schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
            character_id: command.character_id.clone(),
            expected_revision: command.expected_character_revision,
            idempotency_key: command.idempotency_key.clone(),
            tier: reward_tier,
        };
        let audit_id = format!("hero-audit:{}", Uuid::new_v4().simple());
        let mut character = stored_hero.value;
        let audit = character
            .apply_reward(
                &reward_command,
                TrustedRewardPolicy::MvpXpV1,
                &TrustedMutationContext {
                    audit_id: audit_id.clone(),
                    actor_id: LOCAL_HERO_OWNER_KEY.to_owned(),
                    occurred_at_epoch_seconds: self.hero_now_epoch_seconds(),
                },
            )
            .map_err(map_hero_error)?;
        let eligibility =
            HeroLevelUpChoicesDto::from_character(&character).map_err(ApplicationError::Hero)?;
        let outcome = EncounterRewardClaimOutcomeDto {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            campaign_session_id: command.campaign_session_id.clone(),
            encounter_id: SOOT_WIGHT_ENCOUNTER_ID.to_owned(),
            character,
            audit,
            eligibility,
        };
        outcome.validate().map_err(ApplicationError::Hero)?;
        let audit_payload = HeroAuditPayload::RewardAwarded {
            reward: outcome.audit.clone(),
        };
        let victory = projected
            .view
            .latest_outcome
            .as_ref()
            .ok_or(ApplicationError::InvalidStoredState)?;
        let claim = NewEncounterRewardClaim {
            campaign_session_id: command.campaign_session_id.clone(),
            encounter_id: SOOT_WIGHT_ENCOUNTER_ID.to_owned(),
            character_id: command.character_id.clone(),
            encounter_revision: projected.view.state.revision,
            victory_event_sequence: victory.event_sequence,
            reward_tier,
            experience_awarded: outcome.audit.experience_awarded,
            hero_audit_id: audit_id.clone(),
        };
        let receipt = NewHeroCommandReceipt {
            scope: HeroReceiptScope::Character,
            scope_id: command.character_id.clone(),
            campaign_session_id: command.campaign_session_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: ENCOUNTER_REWARD_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint.clone(),
            expected_revision: command.expected_character_revision,
            result_revision: outcome.character.revision,
            audit_id,
            response_json: serde_json::to_string(&outcome)
                .map_err(ApplicationError::Serialization)?,
        };

        match self
            .repository
            .commit_hero_character_mutation(
                &outcome.character,
                command.expected_character_revision,
                HeroCharacterMutationCommand::EncounterReward {
                    reward: &reward_command,
                    claim: &claim,
                },
                &audit_payload,
                &receipt,
            )
            .await
        {
            Ok(committed) if committed.subject.revision == outcome.character.revision + 1 => {
                Ok(outcome)
            }
            Ok(_) => Err(ApplicationError::InvalidStoredState),
            Err(RepositoryError::RevisionConflict { actual, .. }) => {
                if let Some(stored) = self
                    .repository
                    .load_hero_command_receipt(
                        HeroReceiptScope::Character,
                        &command.character_id,
                        &command.idempotency_key,
                    )
                    .await
                    .map_err(ApplicationError::Repository)?
                {
                    encounter_reward_outcome_from_receipt(&command, &fingerprint, &stored)
                } else if self
                    .repository
                    .load_encounter_reward_claim(
                        &command.campaign_session_id,
                        SOOT_WIGHT_ENCOUNTER_ID,
                    )
                    .await
                    .map_err(ApplicationError::Repository)?
                    .is_some()
                {
                    Err(ApplicationError::EncounterRewardAlreadyClaimed)
                } else {
                    Err(ApplicationError::HeroRevisionConflict {
                        expected: command.expected_character_revision,
                        current_revision: actual,
                    })
                }
            }
            Err(RepositoryError::AlreadyExists {
                entity: "encounter reward claim",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_hero_command_receipt(
                        HeroReceiptScope::Character,
                        &command.character_id,
                        &command.idempotency_key,
                    )
                    .await
                    .map_err(ApplicationError::Repository)?;
                match stored {
                    Some(receipt) => {
                        encounter_reward_outcome_from_receipt(&command, &fingerprint, &receipt)
                    }
                    None => Err(ApplicationError::EncounterRewardAlreadyClaimed),
                }
            }
            Err(RepositoryError::AlreadyExists {
                entity: "hero command receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_hero_command_receipt(
                        HeroReceiptScope::Character,
                        &command.character_id,
                        &command.idempotency_key,
                    )
                    .await
                    .map_err(ApplicationError::Repository)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                encounter_reward_outcome_from_receipt(&command, &fingerprint, &stored)
            }
            Err(error) => Err(ApplicationError::Repository(error)),
        }
    }

    pub async fn hero_level_up_choices(
        &self,
        character_id: &str,
    ) -> Result<HeroLevelUpChoicesDto, ApplicationError> {
        let character = self.load_local_created_hero(character_id).await?;
        HeroLevelUpChoicesDto::from_character(&character).map_err(ApplicationError::Hero)
    }

    pub async fn level_up_hero(
        &self,
        command: LevelUpCommand,
    ) -> Result<HeroLevelUpOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        command.validate().map_err(ApplicationError::Hero)?;
        let fingerprint = fingerprint_level_up_command(&command)?;
        let _guard = self.command_gate.lock().await;
        let (session, _) = self.load_or_create_local_campaign().await?;
        self.require_sealed_campaign_pins(&session).await?;
        if let Some(receipt) = self
            .repository
            .load_hero_command_receipt(
                HeroReceiptScope::Character,
                &command.character_id,
                &command.idempotency_key,
            )
            .await
            .map_err(ApplicationError::Repository)?
        {
            return level_up_outcome_from_receipt(&command, &fingerprint, &receipt);
        }
        let stored = self
            .repository
            .load_hero_character(&command.character_id)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::HeroNotFound)?;
        validate_local_character(&stored.value)?;
        if command.expected_revision != stored.value.revision {
            return Err(ApplicationError::HeroRevisionConflict {
                expected: command.expected_revision,
                current_revision: stored.value.revision,
            });
        }
        let audit_id = format!("hero-audit:{}", Uuid::new_v4().simple());
        let mut character = stored.value;
        let audit = character
            .level_up(
                &command,
                &TrustedMutationContext {
                    audit_id: audit_id.clone(),
                    actor_id: LOCAL_HERO_OWNER_KEY.to_owned(),
                    occurred_at_epoch_seconds: self.hero_now_epoch_seconds(),
                },
            )
            .map_err(map_hero_error)?;
        let outcome = HeroLevelUpOutcomeDto {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            character,
            audit,
        };
        outcome.validate().map_err(ApplicationError::Hero)?;
        let audit_payload = HeroAuditPayload::LevelUp {
            level_up: outcome.audit.clone(),
        };
        let receipt = NewHeroCommandReceipt {
            scope: HeroReceiptScope::Character,
            scope_id: command.character_id.clone(),
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: LEVEL_UP_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint.clone(),
            expected_revision: command.expected_revision,
            result_revision: outcome.character.revision,
            audit_id,
            response_json: serde_json::to_string(&outcome)
                .map_err(ApplicationError::Serialization)?,
        };
        if let Some(replayed) = commit_character_outcome(
            self,
            CharacterCommitRequest {
                character_id: &command.character_id,
                expected_revision: command.expected_revision,
                idempotency_key: &command.idempotency_key,
                fingerprint: &fingerprint,
                character: &outcome.character,
                command: HeroCharacterMutationCommand::LevelUp(&command),
                audit: &audit_payload,
                receipt: &receipt,
            },
            |receipt| level_up_outcome_from_receipt(&command, &fingerprint, receipt),
        )
        .await?
        {
            return Ok(replayed);
        }
        Ok(outcome)
    }

    fn hero_now_epoch_seconds(&self) -> u64 {
        (self.clock.now_unix_ms() / 1_000).max(1)
    }
}

struct CharacterCommitRequest<'a> {
    character_id: &'a str,
    expected_revision: u64,
    idempotency_key: &'a str,
    fingerprint: &'a Sha256Digest,
    character: &'a HeroCharacter,
    command: HeroCharacterMutationCommand<'a>,
    audit: &'a HeroAuditPayload,
    receipt: &'a NewHeroCommandReceipt,
}

async fn commit_character_outcome<T, F>(
    service: &GameApplicationService,
    request: CharacterCommitRequest<'_>,
    replay: F,
) -> Result<Option<T>, ApplicationError>
where
    F: Fn(&StoredHeroCommandReceipt) -> Result<T, ApplicationError>,
{
    match service
        .repository
        .commit_hero_character_mutation(
            request.character,
            request.expected_revision,
            request.command,
            request.audit,
            request.receipt,
        )
        .await
    {
        Ok(committed) if committed.subject.revision == request.character.revision + 1 => Ok(None),
        Ok(_) => Err(ApplicationError::InvalidStoredState),
        Err(RepositoryError::RevisionConflict { actual, .. }) => {
            if let Some(stored) = service
                .repository
                .load_hero_command_receipt(
                    HeroReceiptScope::Character,
                    request.character_id,
                    request.idempotency_key,
                )
                .await
                .map_err(ApplicationError::Repository)?
            {
                if stored.request_fingerprint != *request.fingerprint {
                    return Err(ApplicationError::IdempotencyConflict);
                }
                Ok(Some(replay(&stored)?))
            } else {
                Err(ApplicationError::HeroRevisionConflict {
                    expected: request.expected_revision,
                    current_revision: actual,
                })
            }
        }
        Err(RepositoryError::AlreadyExists {
            entity: "hero command receipt",
            ..
        }) => {
            let stored = service
                .repository
                .load_hero_command_receipt(
                    HeroReceiptScope::Character,
                    request.character_id,
                    request.idempotency_key,
                )
                .await
                .map_err(ApplicationError::Repository)?
                .ok_or(ApplicationError::InvalidStoredState)?;
            Ok(Some(replay(&stored)?))
        }
        Err(error) => Err(ApplicationError::Repository(error)),
    }
}

#[derive(Serialize)]
struct NormalizedCreationCommand<'a> {
    schema_version: u16,
    draft_id: &'a str,
    expected_revision: u64,
    intent: &'a HeroCreationIntent,
}

fn fingerprint_creation_command(
    command: &HeroCreationCommand,
) -> Result<Sha256Digest, ApplicationError> {
    fingerprint(&NormalizedCreationCommand {
        schema_version: command.schema_version,
        draft_id: &command.draft_id,
        expected_revision: command.expected_revision,
        intent: &command.intent,
    })
}

#[derive(Serialize)]
struct NormalizedRewardCommand<'a> {
    schema_version: u16,
    character_id: &'a str,
    expected_revision: u64,
    tier: manchester_dnd_core::RewardTier,
}

#[derive(Serialize)]
struct NormalizedEncounterRewardClaim<'a> {
    schema_version: u16,
    campaign_session_id: &'a str,
    character_id: &'a str,
    expected_campaign_revision: u64,
    expected_character_revision: u64,
}

fn fingerprint_encounter_reward_claim(
    command: &ClaimEncounterRewardCommand,
) -> Result<Sha256Digest, ApplicationError> {
    fingerprint(&NormalizedEncounterRewardClaim {
        schema_version: command.schema_version,
        campaign_session_id: &command.campaign_session_id,
        character_id: &command.character_id,
        expected_campaign_revision: command.expected_campaign_revision,
        expected_character_revision: command.expected_character_revision,
    })
}

fn fingerprint_reward_command(
    command: &RewardAwardCommand,
) -> Result<Sha256Digest, ApplicationError> {
    fingerprint(&NormalizedRewardCommand {
        schema_version: command.schema_version,
        character_id: &command.character_id,
        expected_revision: command.expected_revision,
        tier: command.tier,
    })
}

#[derive(Serialize)]
struct NormalizedLevelUpCommand<'a> {
    schema_version: u16,
    character_id: &'a str,
    expected_revision: u64,
    choice: &'a LevelUpChoice,
}

fn fingerprint_level_up_command(
    command: &LevelUpCommand,
) -> Result<Sha256Digest, ApplicationError> {
    fingerprint(&NormalizedLevelUpCommand {
        schema_version: command.schema_version,
        character_id: &command.character_id,
        expected_revision: command.expected_revision,
        choice: &command.choice,
    })
}

fn fingerprint(value: &impl Serialize) -> Result<Sha256Digest, ApplicationError> {
    let serialized = serde_json::to_vec(value).map_err(ApplicationError::Serialization)?;
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    Ok(Sha256Digest::from_bytes(digest))
}

fn validate_local_draft(draft: &HeroCreationDraft) -> Result<(), ApplicationError> {
    draft.validate().map_err(ApplicationError::Hero)?;
    if draft.campaign_id != LOCAL_CAMPAIGN_SESSION_ID || draft.owner_id != LOCAL_HERO_OWNER_KEY {
        return Err(ApplicationError::HeroNotFound);
    }
    Ok(())
}

fn validate_local_character(character: &HeroCharacter) -> Result<(), ApplicationError> {
    character.validate().map_err(ApplicationError::Hero)?;
    if character.campaign_id != LOCAL_CAMPAIGN_SESSION_ID
        || character.owner_id != LOCAL_HERO_OWNER_KEY
    {
        return Err(ApplicationError::HeroNotFound);
    }
    Ok(())
}

fn validate_creation_outcome(
    command: &HeroCreationCommand,
    outcome: &HeroCreationOutcome,
) -> Result<(), ApplicationError> {
    outcome
        .transition_audit
        .validate()
        .map_err(ApplicationError::Hero)?;
    if outcome.transition_audit.draft_id != command.draft_id
        || outcome.transition_audit.idempotency_key != command.idempotency_key
        || outcome.transition_audit.revision_before != command.expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    match (&outcome.character, &outcome.created_audit) {
        (None, None) if !matches!(command.intent, HeroCreationIntent::Commit { .. }) => Ok(()),
        (Some(character), Some(audit))
            if matches!(command.intent, HeroCreationIntent::Commit { .. }) =>
        {
            character.validate().map_err(ApplicationError::Hero)?;
            audit.validate().map_err(ApplicationError::Hero)?;
            if audit.character_id != character.character_id
                || audit.choices != character.choices
                || audit.derived_sheet != character.sheet
            {
                Err(ApplicationError::InvalidStoredState)
            } else {
                Ok(())
            }
        }
        _ => Err(ApplicationError::InvalidStoredState),
    }
}

fn creation_outcome_from_receipt(
    command: &HeroCreationCommand,
    fingerprint: &Sha256Digest,
    receipt: &StoredHeroCommandReceipt,
) -> Result<HeroCreationOutcome, ApplicationError> {
    validate_receipt_identity(
        receipt,
        HeroReceiptScope::Draft,
        &command.draft_id,
        &command.idempotency_key,
        CREATION_COMMAND_KIND,
        command.expected_revision,
        fingerprint,
    )?;
    let outcome: HeroCreationOutcome =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    validate_creation_outcome(command, &outcome)?;
    if outcome.transition_audit.revision_after != receipt.result_revision
        || outcome.transition_audit.audit_id != receipt.audit_id
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(outcome)
}

fn reward_outcome_from_receipt(
    command: &RewardAwardCommand,
    fingerprint: &Sha256Digest,
    receipt: &StoredHeroCommandReceipt,
) -> Result<HeroRewardOutcomeDto, ApplicationError> {
    validate_receipt_identity(
        receipt,
        HeroReceiptScope::Character,
        &command.character_id,
        &command.idempotency_key,
        REWARD_COMMAND_KIND,
        command.expected_revision,
        fingerprint,
    )?;
    let outcome: HeroRewardOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    outcome.validate().map_err(ApplicationError::Hero)?;
    if outcome.character.revision != receipt.result_revision
        || outcome.audit.audit_id != receipt.audit_id
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(outcome)
}

fn encounter_reward_outcome_from_receipt(
    command: &ClaimEncounterRewardCommand,
    fingerprint: &Sha256Digest,
    receipt: &StoredHeroCommandReceipt,
) -> Result<EncounterRewardClaimOutcomeDto, ApplicationError> {
    validate_receipt_identity(
        receipt,
        HeroReceiptScope::Character,
        &command.character_id,
        &command.idempotency_key,
        ENCOUNTER_REWARD_COMMAND_KIND,
        command.expected_character_revision,
        fingerprint,
    )?;
    let outcome: EncounterRewardClaimOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    outcome.validate().map_err(ApplicationError::Hero)?;
    if outcome.campaign_session_id != command.campaign_session_id
        || outcome.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
        || outcome.character.character_id != command.character_id
        || outcome.character.revision != receipt.result_revision
        || outcome.audit.audit_id != receipt.audit_id
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(outcome)
}

fn level_up_outcome_from_receipt(
    command: &LevelUpCommand,
    fingerprint: &Sha256Digest,
    receipt: &StoredHeroCommandReceipt,
) -> Result<HeroLevelUpOutcomeDto, ApplicationError> {
    validate_receipt_identity(
        receipt,
        HeroReceiptScope::Character,
        &command.character_id,
        &command.idempotency_key,
        LEVEL_UP_COMMAND_KIND,
        command.expected_revision,
        fingerprint,
    )?;
    let outcome: HeroLevelUpOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    outcome.validate().map_err(ApplicationError::Hero)?;
    if outcome.character.revision != receipt.result_revision
        || outcome.audit.audit_id != receipt.audit_id
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn validate_receipt_identity(
    receipt: &StoredHeroCommandReceipt,
    scope: HeroReceiptScope,
    scope_id: &str,
    idempotency_key: &str,
    command_kind: &str,
    expected_revision: u64,
    fingerprint: &Sha256Digest,
) -> Result<(), ApplicationError> {
    if &receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.scope != scope
        || receipt.scope_id != scope_id
        || receipt.campaign_session_id != LOCAL_CAMPAIGN_SESSION_ID
        || receipt.idempotency_key != idempotency_key
        || receipt.command_kind != command_kind
        || receipt.expected_revision != expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(())
}

fn map_hero_error(error: HeroError) -> ApplicationError {
    match error {
        HeroError::StaleRevision { expected, actual } => ApplicationError::HeroRevisionConflict {
            expected,
            current_revision: actual,
        },
        HeroError::UnsupportedMechanic(unsupported) => {
            ApplicationError::UnsupportedHeroMechanic(unsupported)
        }
        error => ApplicationError::Hero(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use manchester_dnd_core::{
        ADVANCE_NPC_TURN_SCHEMA_VERSION, AdvanceNpcTurnCommand, CommitEncounterCommand,
        ENCOUNTER_COMMIT_SCHEMA_VERSION, RewardTier,
        encounter::{EncounterCommand, EncounterIntent, EncounterStatus, LegalEncounterAction},
        hero::{
            AncestryId, ArcaneTraditionId, BackgroundId, BackgroundSelection, ClassSelection,
            CreationStep, EquipmentId, EquipmentSelection, FightingStyleId, HeroConceptId,
            HeroPins, HeroPresentation, HitPointGrowthChoice, LevelUpChoice, SimpleWeaponId,
            SkillId, SpellId, StandardArrayAssignment, SupportedLevel, ThemeId,
            WizardSpellSelection,
        },
    };
    use sqlx::PgPool;

    use crate::{config::AccessMode, repository::PostgresRepository, seed::SeedVault};

    use super::*;

    fn test_service(
        pool: PgPool,
        clock: Arc<AtomicU64>,
    ) -> (GameApplicationService, PostgresRepository) {
        let repository = PostgresRepository::from_pool(pool);
        let observed_clock = clock;
        let service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([9; 32])),
            |_| 12,
            move || observed_clock.load(Ordering::SeqCst),
        );
        (service, repository)
    }

    fn creation_command(
        draft: &HeroCreationDraft,
        key: &str,
        intent: HeroCreationIntent,
    ) -> HeroCreationCommand {
        HeroCreationCommand {
            schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
            draft_id: draft.draft_id.clone(),
            expected_revision: draft.revision,
            idempotency_key: key.to_owned(),
            intent,
        }
    }

    fn standard_array() -> StandardArrayAssignment {
        StandardArrayAssignment {
            strength: 15,
            dexterity: 14,
            constitution: 13,
            intelligence: 12,
            wisdom: 10,
            charisma: 8,
        }
    }

    fn fighter_background() -> BackgroundSelection {
        BackgroundSelection {
            background: BackgroundId::Soldier,
            class_skills: vec![SkillId::Acrobatics, SkillId::AnimalHandling],
        }
    }

    fn fighter_equipment() -> EquipmentSelection {
        EquipmentSelection {
            carried: vec![
                EquipmentId::Longsword,
                EquipmentId::LightCrossbow,
                EquipmentId::Shield,
                EquipmentId::ChainMail,
                EquipmentId::ExplorersPack,
            ],
            simple_weapon: None,
            equipped_armor: Some(EquipmentId::ChainMail),
            shield_equipped: true,
        }
    }

    fn wizard_standard_array() -> StandardArrayAssignment {
        StandardArrayAssignment {
            strength: 8,
            dexterity: 14,
            constitution: 13,
            intelligence: 15,
            wisdom: 12,
            charisma: 10,
        }
    }

    fn wizard_background() -> BackgroundSelection {
        BackgroundSelection {
            background: BackgroundId::Sage,
            class_skills: vec![SkillId::Insight, SkillId::Investigation],
        }
    }

    fn wizard_equipment() -> EquipmentSelection {
        EquipmentSelection {
            carried: vec![
                EquipmentId::SimpleWeapons,
                EquipmentId::ScholarsPack,
                EquipmentId::Spellbook,
                EquipmentId::ArcaneFocus,
            ],
            simple_weapon: Some(SimpleWeaponId::Dagger),
            equipped_armor: None,
            shield_equipped: false,
        }
    }

    fn wizard_spells() -> WizardSpellSelection {
        WizardSpellSelection {
            cantrips: SpellId::CANTRIPS.to_vec(),
            spellbook: SpellId::LEVEL_ONE.to_vec(),
            prepared: SpellId::LEVEL_ONE.to_vec(),
        }
    }

    fn presentation() -> HeroPresentation {
        HeroPresentation {
            name: "Mara Vale".to_owned(),
            pronouns: "they/them".to_owned(),
            appearance: "A weathered coat marked with canal-silt.".to_owned(),
            ideal: "No one is abandoned below the arches.".to_owned(),
            bond: "The old lock keepers raised them.".to_owned(),
            flaw: "They refuse help long after they need it.".to_owned(),
            tone_limits: vec!["No graphic horror".to_owned()],
        }
    }

    async fn apply_and_reload(
        service: &GameApplicationService,
        draft: &HeroCreationDraft,
        key: &str,
        intent: HeroCreationIntent,
    ) -> HeroCreationDraft {
        service
            .apply_hero_creation_command(creation_command(draft, key, intent))
            .await
            .unwrap();
        service
            .load_local_hero_creation(&draft.draft_id)
            .await
            .unwrap()
    }

    async fn ready_to_commit(service: &GameApplicationService) -> HeroCreationDraft {
        let mut draft = service.start_local_hero_creation().await.unwrap();
        draft = apply_and_reload(
            service,
            &draft,
            "creation-theme",
            HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "creation-concept",
            HeroCreationIntent::SelectConcept {
                concept: HeroConceptId::CanalGuardian,
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "creation-rules",
            HeroCreationIntent::SelectRules {
                ancestry: AncestryId::Human,
                class: ClassSelection::Fighter {
                    fighting_style: FightingStyleId::Defense,
                },
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "creation-abilities",
            HeroCreationIntent::AssignAbilities {
                assignment: standard_array(),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "creation-background",
            HeroCreationIntent::SelectBackground {
                selection: fighter_background(),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "creation-equipment",
            HeroCreationIntent::SelectEquipmentAndSpells {
                equipment: fighter_equipment(),
                wizard_spells: None,
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "creation-presentation",
            HeroCreationIntent::SetPresentation {
                presentation: presentation(),
            },
        )
        .await;
        apply_and_reload(
            service,
            &draft,
            "creation-review",
            HeroCreationIntent::Review,
        )
        .await
    }

    async fn create_fighter(
        service: &GameApplicationService,
        character_id: &str,
    ) -> (HeroCreationDraft, HeroCreationCommand, HeroCreationOutcome) {
        let draft = ready_to_commit(service).await;
        let command = creation_command(
            &draft,
            "creation-commit",
            HeroCreationIntent::Commit {
                character_id: character_id.to_owned(),
            },
        );
        let outcome = service
            .finalize_hero_creation(command.clone())
            .await
            .unwrap();
        let committed = service
            .load_local_hero_creation(&draft.draft_id)
            .await
            .unwrap();
        (committed, command, outcome)
    }

    async fn create_wizard(service: &GameApplicationService, character_id: &str) -> HeroCharacter {
        let mut draft = service.start_local_hero_creation().await.unwrap();
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-theme",
            HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::EmberlineArchive),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-concept",
            HeroCreationIntent::SelectConcept {
                concept: HeroConceptId::ArchiveSeeker,
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-rules",
            HeroCreationIntent::SelectRules {
                ancestry: AncestryId::Human,
                class: ClassSelection::Wizard,
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-abilities",
            HeroCreationIntent::AssignAbilities {
                assignment: wizard_standard_array(),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-background",
            HeroCreationIntent::SelectBackground {
                selection: wizard_background(),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-equipment",
            HeroCreationIntent::SelectEquipmentAndSpells {
                equipment: wizard_equipment(),
                wizard_spells: Some(wizard_spells()),
            },
        )
        .await;
        draft = apply_and_reload(
            service,
            &draft,
            "wizard-presentation",
            HeroCreationIntent::SetPresentation {
                presentation: HeroPresentation {
                    name: "Iris Quill".to_owned(),
                    ..presentation()
                },
            },
        )
        .await;
        draft =
            apply_and_reload(service, &draft, "wizard-review", HeroCreationIntent::Review).await;
        service
            .finalize_hero_creation(creation_command(
                &draft,
                "wizard-commit",
                HeroCreationIntent::Commit {
                    character_id: character_id.to_owned(),
                },
            ))
            .await
            .unwrap()
            .character
            .unwrap()
    }

    fn exploration_command(
        view: &manchester_dnd_core::LocalCampaignViewDto,
        key: &str,
    ) -> manchester_dnd_core::AttemptExplorationCheckCommand {
        manchester_dnd_core::AttemptExplorationCheckCommand {
            schema_version: manchester_dnd_core::EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            character_id: view.character_id.clone(),
            action_id: super::super::LOCAL_EXPLORATION_ACTION_ID.to_owned(),
            expected_revision: view.revision,
            idempotency_key: key.to_owned(),
        }
    }

    fn encounter_command(
        view: &manchester_dnd_core::LocalCampaignViewDto,
        key: &str,
        intent: EncounterIntent,
    ) -> CommitEncounterCommand {
        let encounter = view.encounter.as_ref().unwrap();
        CommitEncounterCommand {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            expected_campaign_revision: view.revision,
            command: EncounterCommand::new(encounter.state.revision, key, intent),
        }
    }

    async fn ready_encounter(
        service: &GameApplicationService,
        key: &str,
    ) -> manchester_dnd_core::LocalCampaignViewDto {
        let view = service.load_local_campaign().await.unwrap();
        service
            .attempt_exploration_check(exploration_command(&view, key))
            .await
            .unwrap();
        service.load_local_campaign().await.unwrap()
    }

    #[test]
    fn encounter_reward_claim_boundary_has_no_client_tier_or_experience() {
        let forged = serde_json::json!({
            "schema_version": HERO_APPLICATION_SCHEMA_VERSION,
            "campaign_session_id": LOCAL_CAMPAIGN_SESSION_ID,
            "character_id": "created-hero",
            "expected_campaign_revision": 12,
            "expected_character_revision": 0,
            "idempotency_key": "forged-reward",
            "tier": "major",
            "experience_points": 300
        });
        assert!(serde_json::from_value::<ClaimEncounterRewardCommand>(forged).is_err());
    }

    async fn play_encounter_to_completion(
        service: &GameApplicationService,
        mut view: manchester_dnd_core::LocalCampaignViewDto,
        key_prefix: &str,
    ) -> manchester_dnd_core::LocalCampaignViewDto {
        service
            .commit_encounter_command(encounter_command(
                &view,
                &format!("{key_prefix}-start"),
                EncounterIntent::StartEncounter,
            ))
            .await
            .unwrap();
        for step in 0..100_u8 {
            view = service.load_local_campaign().await.unwrap();
            let encounter = view.encounter.as_ref().unwrap();
            if matches!(
                encounter.state.status,
                EncounterStatus::Victory | EncounterStatus::Defeat
            ) {
                return view;
            }
            if encounter
                .legal_actions
                .contains(&LegalEncounterAction::DeclineReaction)
            {
                service
                    .commit_encounter_command(encounter_command(
                        &view,
                        &format!("{key_prefix}-decline-reaction-{step}"),
                        EncounterIntent::DeclineReaction,
                    ))
                    .await
                    .unwrap();
                continue;
            }
            if encounter.state.current_actor_id.as_deref()
                == Some(encounter.state.creature.id.as_str())
            {
                service
                    .advance_npc_turn(AdvanceNpcTurnCommand {
                        schema_version: ADVANCE_NPC_TURN_SCHEMA_VERSION,
                        campaign_session_id: view.campaign_session_id.clone(),
                        expected_campaign_revision: view.revision,
                        expected_encounter_revision: encounter.state.revision,
                        idempotency_key: format!("{key_prefix}-npc-{step}"),
                    })
                    .await
                    .unwrap();
                continue;
            }
            let resources = encounter.state.turn_resources.as_ref().unwrap();
            let intent = if !resources.action_available {
                EncounterIntent::EndTurn
            } else if let Some(LegalEncounterAction::Attack {
                attack_id,
                target_id,
                ..
            }) = encounter
                .legal_actions
                .iter()
                .find(|action| matches!(action, LegalEncounterAction::Attack { .. }))
            {
                EncounterIntent::Attack {
                    attack_id: attack_id.clone(),
                    target_id: target_id.clone(),
                }
            } else if let Some(LegalEncounterAction::Move {
                minimum_destination_feet,
                maximum_destination_feet,
                ..
            }) = encounter
                .legal_actions
                .iter()
                .find(|action| matches!(action, LegalEncounterAction::Move { .. }))
            {
                let target_position = if encounter.state.current_actor_id.as_deref()
                    == Some(manchester_dnd_core::encounter::CANAL_WARDEN_ID)
                {
                    encounter.state.creature.position_feet
                } else {
                    encounter.state.hero.position_feet
                };
                EncounterIntent::Move {
                    destination_feet: target_position
                        .clamp(*minimum_destination_feet, *maximum_destination_feet),
                }
            } else if encounter
                .legal_actions
                .contains(&LegalEncounterAction::RollDeathSave)
            {
                EncounterIntent::RollDeathSave
            } else {
                EncounterIntent::EndTurn
            };
            service
                .commit_encounter_command(encounter_command(
                    &view,
                    &format!("{key_prefix}-{step}"),
                    intent,
                ))
                .await
                .unwrap();
        }
        panic!("encounter did not complete within the bounded test script")
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn pre_creation_campaign_cannot_start_the_legacy_encounter_fallback(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, _) = test_service(pool, clock);
        let view = service.load_local_campaign().await.unwrap();
        assert!(view.content_pins.sealed().is_none());
        assert!(matches!(
            service
                .attempt_exploration_check(exploration_command(&view, "legacy-exploration"))
                .await,
            Err(ApplicationError::CampaignPinsUnsealed)
        ));
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn fighter_sheet_drives_perception_and_the_ready_encounter(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, _) = test_service(pool, clock);
        let (_, _, created) = create_fighter(&service, "encounter-fighter").await;
        let hero = created.character.unwrap();
        let initial = service.load_local_campaign().await.unwrap();
        assert_eq!(initial.character_name, "Mara Vale");
        let check = service
            .attempt_exploration_check(exploration_command(&initial, "fighter-perception"))
            .await
            .unwrap();
        let perception = hero
            .sheet
            .skills
            .iter()
            .find(|skill| skill.skill == SkillId::Perception)
            .unwrap();
        assert_eq!(
            i16::from(check.result.ability_modifier) + i16::from(check.result.proficiency_modifier),
            i16::from(perception.modifier),
        );

        let ready = service.load_local_campaign().await.unwrap();
        let encounter = ready.encounter.unwrap();
        let encounter_hero = &encounter.state.hero;
        assert_eq!(
            encounter_hero.source_character_id.as_deref(),
            Some(hero.character_id.as_str())
        );
        assert_eq!(encounter_hero.name, hero.choices.presentation.name);
        assert_eq!(
            encounter_hero.armor_class,
            u16::from(hero.sheet.armor_class)
        );
        assert_eq!(
            encounter_hero.hit_points.maximum,
            hero.sheet.maximum_hit_points
        );
        assert_eq!(encounter_hero.speed_feet, u16::from(hero.sheet.speed_feet));
        assert_eq!(
            encounter_hero
                .attacks
                .iter()
                .map(|attack| attack.attack_id.as_str())
                .collect::<Vec<_>>(),
            hero.sheet
                .attacks
                .iter()
                .map(|attack| attack.attack_id.as_str())
                .collect::<Vec<_>>()
        );
        let live_resources = &encounter
            .state
            .hero_rules
            .as_ref()
            .unwrap()
            .runtime_resources;
        assert_eq!(
            live_resources.class,
            manchester_dnd_core::hero::HeroClass::Fighter
        );
        assert_eq!(live_resources.second_wind.as_ref().unwrap().current, 1);
        assert!(live_resources.action_surge.is_none());
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn wizard_live_spells_persist_slots_rolls_and_replay_without_client_mechanics(
        pool: PgPool,
    ) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, _) = test_service(pool, clock);
        let hero = create_wizard(&service, "encounter-wizard").await;
        let ready = ready_encounter(&service, "wizard-perception").await;
        let state = &ready.encounter.as_ref().unwrap().state;

        assert_eq!(state.hero.name, "Iris Quill");
        assert_eq!(state.hero.armor_class, u16::from(hero.sheet.armor_class));
        assert_eq!(state.hero.hit_points.maximum, hero.sheet.maximum_hit_points);
        assert_eq!(state.hero.attacks.len(), 1);
        assert_eq!(state.hero.attacks[0].attack_id, "attack:simple:dagger");
        assert!(
            state
                .hero
                .attacks
                .iter()
                .all(|attack| !attack.attack_id.contains("spell"))
        );
        let ready_rules = state.hero_rules.as_ref().unwrap();
        assert!(ready_rules.spellcasting.is_some());
        assert_eq!(
            ready_rules
                .runtime_resources
                .level_one_spell_slots
                .as_ref()
                .unwrap()
                .current,
            2
        );

        service
            .commit_encounter_command(encounter_command(
                &ready,
                "wizard-live-start",
                EncounterIntent::StartEncounter,
            ))
            .await
            .unwrap();
        let mut active = service.load_local_campaign().await.unwrap();
        for step in 0..4_u8 {
            let encounter = active.encounter.as_ref().unwrap();
            if encounter
                .legal_actions
                .contains(&LegalEncounterAction::DeclineReaction)
            {
                service
                    .commit_encounter_command(encounter_command(
                        &active,
                        &format!("wizard-live-decline-reaction-{step}"),
                        EncounterIntent::DeclineReaction,
                    ))
                    .await
                    .unwrap();
                active = service.load_local_campaign().await.unwrap();
                continue;
            }
            if encounter.state.current_actor_id.as_deref() == Some(encounter.state.hero.id.as_str())
            {
                break;
            }
            service
                .advance_npc_turn(AdvanceNpcTurnCommand {
                    schema_version: ADVANCE_NPC_TURN_SCHEMA_VERSION,
                    campaign_session_id: active.campaign_session_id.clone(),
                    expected_campaign_revision: active.revision,
                    expected_encounter_revision: encounter.state.revision,
                    idempotency_key: format!("wizard-live-npc-{step}"),
                })
                .await
                .unwrap();
            active = service.load_local_campaign().await.unwrap();
        }
        let encounter = active.encounter.as_ref().unwrap();
        assert_eq!(
            encounter.state.current_actor_id.as_deref(),
            Some(encounter.state.hero.id.as_str())
        );
        for spell in [SpellId::FireBolt, SpellId::MagicMissile] {
            assert!(encounter.legal_actions.iter().any(|action| matches!(
                action,
                LegalEncounterAction::CastSpell {
                    spell: legal_spell,
                    target_id,
                    ..
                } if *legal_spell == spell && target_id == &encounter.state.creature.id
            )));
        }
        assert!(!encounter.legal_actions.iter().any(|action| matches!(
            action,
            LegalEncounterAction::CastSpell {
                spell: SpellId::Light | SpellId::MageHand | SpellId::Shield | SpellId::Sleep,
                ..
            }
        )));

        let revision_before_forgery = active.revision;
        assert!(matches!(
            service
                .commit_encounter_command(encounter_command(
                    &active,
                    "wizard-forged-sleep",
                    EncounterIntent::CastSpell {
                        spell: SpellId::Sleep,
                        target_id: encounter.state.creature.id.clone(),
                    },
                ))
                .await,
            Err(ApplicationError::InvalidEncounterCommand(_))
        ));
        active = service.load_local_campaign().await.unwrap();
        assert_eq!(active.revision, revision_before_forgery);

        let target_id = active.encounter.as_ref().unwrap().state.creature.id.clone();
        let cast_command = encounter_command(
            &active,
            "wizard-cast-magic-missile",
            EncounterIntent::CastSpell {
                spell: SpellId::MagicMissile,
                target_id,
            },
        );
        let committed = service
            .commit_encounter_command(cast_command.clone())
            .await
            .unwrap();
        assert_eq!(committed.roll_records.len(), 3);
        assert!(
            committed
                .roll_records
                .iter()
                .all(|record| record.purpose == "encounter:damage")
        );
        assert_eq!(
            committed
                .resolution
                .state
                .hero_rules
                .as_ref()
                .unwrap()
                .runtime_resources
                .level_one_spell_slots
                .as_ref()
                .unwrap()
                .current,
            1
        );
        let retried = service
            .commit_encounter_command(cast_command)
            .await
            .unwrap();
        assert_eq!(retried, committed);

        let reloaded = service.load_local_campaign().await.unwrap();
        let reloaded_encounter = reloaded.encounter.unwrap();
        assert_eq!(reloaded_encounter.state, committed.resolution.state);
        assert_eq!(
            reloaded_encounter.latest_outcome.unwrap().resolution,
            committed.resolution
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn started_encounter_keeps_its_original_hero_snapshot_after_advancement(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, _) = test_service(pool, clock);
        let (_, _, created) = create_fighter(&service, "snapshot-fighter").await;
        let hero = created.character.unwrap();
        let ready = ready_encounter(&service, "snapshot-perception").await;
        let started = service
            .commit_encounter_command(encounter_command(
                &ready,
                "snapshot-start",
                EncounterIntent::StartEncounter,
            ))
            .await
            .unwrap();

        service
            .apply_hero_reward(RewardAwardCommand {
                schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
                character_id: hero.character_id.clone(),
                expected_revision: 0,
                idempotency_key: "snapshot-major-reward".to_owned(),
                tier: RewardTier::Major,
            })
            .await
            .unwrap();
        service
            .level_up_hero(LevelUpCommand {
                schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
                character_id: hero.character_id,
                expected_revision: 1,
                idempotency_key: "snapshot-level-up".to_owned(),
                choice: LevelUpChoice::Fighter {
                    hit_points: HitPointGrowthChoice::FixedAverage,
                },
            })
            .await
            .unwrap();

        let reloaded = service.load_local_campaign().await.unwrap();
        assert_eq!(reloaded.encounter.unwrap().state, started.resolution.state,);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn encounter_reward_is_victory_bound_trusted_idempotent_and_reloadable(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, repository) = test_service(pool, clock.clone());
        let (_, _, created) = create_fighter(&service, "reward-fighter").await;
        let hero = created.character.unwrap();
        let initial = service.load_local_campaign().await.unwrap();
        let before_victory = ClaimEncounterRewardCommand {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            campaign_session_id: initial.campaign_session_id.clone(),
            character_id: hero.character_id.clone(),
            expected_campaign_revision: initial.revision,
            expected_character_revision: hero.revision,
            idempotency_key: "reward-before-victory".to_owned(),
        };
        assert!(matches!(
            service.claim_local_encounter_reward(before_victory).await,
            Err(ApplicationError::EncounterRewardUnavailable)
        ));

        let ready = ready_encounter(&service, "reward-perception").await;
        let completed = play_encounter_to_completion(&service, ready, "reward-fight").await;
        assert_eq!(
            completed.encounter.as_ref().unwrap().state.status,
            EncounterStatus::Victory
        );
        let encounter_hero_revision = completed
            .encounter
            .as_ref()
            .and_then(|encounter| encounter.latest_outcome.as_ref())
            .and_then(|outcome| outcome.result_hero_revision)
            .unwrap();
        let command = ClaimEncounterRewardCommand {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            campaign_session_id: completed.campaign_session_id.clone(),
            character_id: hero.character_id.clone(),
            expected_campaign_revision: completed.revision,
            expected_character_revision: encounter_hero_revision,
            idempotency_key: "claim-victory-reward".to_owned(),
        };

        let mut wrong_campaign = command.clone();
        wrong_campaign.campaign_session_id = "another-campaign".to_owned();
        wrong_campaign.idempotency_key = "claim-wrong-campaign".to_owned();
        assert!(matches!(
            service.claim_local_encounter_reward(wrong_campaign).await,
            Err(ApplicationError::WrongCampaign)
        ));
        let mut stale_campaign = command.clone();
        stale_campaign.expected_campaign_revision -= 1;
        stale_campaign.idempotency_key = "claim-stale-campaign".to_owned();
        assert!(matches!(
            service.claim_local_encounter_reward(stale_campaign).await,
            Err(ApplicationError::RevisionConflict { .. })
        ));
        let mut stale = command.clone();
        stale.expected_character_revision = encounter_hero_revision + 1;
        stale.idempotency_key = "claim-stale-hero".to_owned();
        assert!(matches!(
            service.claim_local_encounter_reward(stale).await,
            Err(ApplicationError::HeroRevisionConflict {
                expected,
                current_revision
            }) if expected == encounter_hero_revision + 1
                && current_revision == encounter_hero_revision
        ));
        let mut wrong_character = command.clone();
        wrong_character.character_id = "another-created-hero".to_owned();
        wrong_character.idempotency_key = "claim-wrong-character".to_owned();
        assert!(matches!(
            service.claim_local_encounter_reward(wrong_character).await,
            Err(ApplicationError::WrongCharacter)
        ));

        let claimed = service
            .claim_local_encounter_reward(command.clone())
            .await
            .unwrap();
        assert_eq!(claimed.audit.tier, RewardTier::Major);
        assert_eq!(claimed.audit.experience_awarded, 300);
        assert_eq!(claimed.character.experience_points, 300);
        assert_eq!(claimed.character.revision, encounter_hero_revision + 1);
        assert!(claimed.eligibility.eligible);
        assert_eq!(
            service
                .claim_local_encounter_reward(command.clone())
                .await
                .unwrap(),
            claimed
        );

        let duplicate = ClaimEncounterRewardCommand {
            expected_character_revision: encounter_hero_revision + 1,
            idempotency_key: "claim-victory-again".to_owned(),
            ..command.clone()
        };
        assert!(matches!(
            service.claim_local_encounter_reward(duplicate).await,
            Err(ApplicationError::EncounterRewardAlreadyClaimed)
        ));
        let stored_claim = repository
            .load_encounter_reward_claim(LOCAL_CAMPAIGN_SESSION_ID, SOOT_WIGHT_ENCOUNTER_ID)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_claim.character_id, hero.character_id);
        assert_eq!(stored_claim.experience_awarded, 300);

        let resumed = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository,
            Arc::new(SeedVault::from_key([9; 32])),
            |_| 1,
            move || clock.load(Ordering::SeqCst),
        );
        assert_eq!(
            resumed.claim_local_encounter_reward(command).await.unwrap(),
            claimed
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn concurrent_reward_claims_across_service_instances_commit_once(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (setup, repository) = test_service(pool, clock.clone());
        let (_, _, created) = create_fighter(&setup, "concurrent-reward-fighter").await;
        let hero = created.character.unwrap();
        let ready = ready_encounter(&setup, "concurrent-reward-perception").await;
        let completed =
            play_encounter_to_completion(&setup, ready, "concurrent-reward-fight").await;
        assert_eq!(
            completed.encounter.as_ref().unwrap().state.status,
            EncounterStatus::Victory
        );
        let encounter_hero_revision = completed
            .encounter
            .as_ref()
            .and_then(|encounter| encounter.latest_outcome.as_ref())
            .and_then(|outcome| outcome.result_hero_revision)
            .unwrap();
        let command = ClaimEncounterRewardCommand {
            schema_version: HERO_APPLICATION_SCHEMA_VERSION,
            campaign_session_id: completed.campaign_session_id,
            character_id: hero.character_id.clone(),
            expected_campaign_revision: completed.revision,
            expected_character_revision: encounter_hero_revision,
            idempotency_key: "concurrent-victory-claim".to_owned(),
        };
        let service = |repository: PostgresRepository, clock: Arc<AtomicU64>| {
            GameApplicationService::with_sources(
                AccessMode::LocalSingleUser,
                repository,
                Arc::new(SeedVault::from_key([9; 32])),
                |_| 1,
                move || clock.load(Ordering::SeqCst),
            )
        };
        let first = service(repository.clone(), clock.clone());
        let second = service(repository.clone(), clock);
        let (left, right) = tokio::join!(
            first.claim_local_encounter_reward(command.clone()),
            second.claim_local_encounter_reward(command),
        );

        assert_eq!(left.unwrap(), right.unwrap());
        assert_eq!(
            repository
                .list_hero_audits(LOCAL_CAMPAIGN_SESSION_ID, &hero.character_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repository
                .load_hero_character(&hero.character_id)
                .await
                .unwrap()
                .unwrap()
                .value
                .experience_points,
            300
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn draft_resumes_and_enforces_exact_expiry_and_retention_boundaries(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, repository) = test_service(pool, clock.clone());
        assert_eq!(
            service.load_local_hero_workspace().await.unwrap(),
            LocalHeroWorkspaceDto {
                schema_version: HERO_APPLICATION_SCHEMA_VERSION,
                draft: None,
                character: None,
            }
        );
        let draft = service.start_local_hero_creation().await.unwrap();
        let advanced = apply_and_reload(
            &service,
            &draft,
            "resume-theme",
            HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
            },
        )
        .await;

        let resumed_service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([9; 32])),
            |_| 1,
            {
                let clock = clock.clone();
                move || clock.load(Ordering::SeqCst)
            },
        );
        assert_eq!(
            resumed_service
                .load_local_hero_creation(&draft.draft_id)
                .await
                .unwrap(),
            advanced
        );
        assert_eq!(advanced.step, CreationStep::Concept);
        assert_eq!(
            resumed_service
                .load_local_hero_workspace()
                .await
                .unwrap()
                .draft,
            Some(advanced.clone())
        );

        clock.store(advanced.expires_at_epoch_seconds * 1_000, Ordering::SeqCst);
        assert!(
            resumed_service
                .load_local_hero_creation(&draft.draft_id)
                .await
                .is_ok()
        );
        clock.store(
            (advanced.expires_at_epoch_seconds + 1) * 1_000,
            Ordering::SeqCst,
        );
        assert!(matches!(
            resumed_service
                .load_local_hero_creation(&draft.draft_id)
                .await,
            Err(ApplicationError::HeroDraftExpired)
        ));
        assert!(
            resumed_service
                .load_local_hero_workspace()
                .await
                .unwrap()
                .draft
                .is_none()
        );
        assert!(
            repository
                .load_hero_draft(&draft.draft_id)
                .await
                .unwrap()
                .is_some()
        );

        clock.store(
            (advanced.expires_at_epoch_seconds + HERO_DRAFT_RETENTION_SECONDS) * 1_000,
            Ordering::SeqCst,
        );
        resumed_service.start_local_hero_creation().await.unwrap();
        assert!(
            repository
                .load_hero_draft(&draft.draft_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn duplicate_stale_forged_and_rewritten_creation_transitions_fail_closed(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, repository) = test_service(pool.clone(), clock);
        let draft = service.start_local_hero_creation().await.unwrap();
        let command = creation_command(
            &draft,
            "exact-theme",
            HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
            },
        );
        let first = service
            .apply_hero_creation_command(command.clone())
            .await
            .unwrap();
        let replay = service
            .apply_hero_creation_command(command.clone())
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert_eq!(
            repository
                .list_hero_audits(LOCAL_CAMPAIGN_SESSION_ID, &draft.draft_id)
                .await
                .unwrap()
                .len(),
            1
        );
        let sealed = repository
            .load_campaign_pins(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            sealed.evidence.seal_reason,
            manchester_dnd_core::CampaignPinSealReason::SelectedTheme
        );
        assert_eq!(
            sealed.evidence.pins.hero.theme_id,
            ThemeId::RainboundBorough
        );

        let changed_same_key = HeroCreationCommand {
            intent: HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::EmberlineArchive),
            },
            ..command.clone()
        };
        assert!(matches!(
            service.apply_hero_creation_command(changed_same_key).await,
            Err(ApplicationError::IdempotencyConflict)
        ));

        let stale = HeroCreationCommand {
            idempotency_key: "stale-theme".to_owned(),
            ..command
        };
        assert!(matches!(
            service.apply_hero_creation_command(stale).await,
            Err(ApplicationError::HeroRevisionConflict {
                expected: 0,
                current_revision: 1
            })
        ));

        let fresh = service.start_local_hero_creation().await.unwrap();
        let mut forged_pins = HeroPins::mvp(ThemeId::RainboundBorough);
        forged_pins.core_content.digest =
            manchester_dnd_core::Sha256Digest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(matches!(
            service
                .apply_hero_creation_command(creation_command(
                    &fresh,
                    "forged-pins",
                    HeroCreationIntent::SelectCampaignTheme { pins: forged_pins }
                ))
                .await,
            Err(ApplicationError::Hero(_))
        ));
        assert_eq!(
            service
                .load_local_hero_creation(&fresh.draft_id)
                .await
                .unwrap()
                .revision,
            0
        );

        // Bypass the application to prove the repository cannot rewrite a prior
        // selection while presenting a valid next-step audit.
        let stored = repository
            .load_hero_draft(&draft.draft_id)
            .await
            .unwrap()
            .unwrap();
        let next = creation_command(
            &stored.value,
            "repository-rewrite",
            HeroCreationIntent::SelectConcept {
                concept: HeroConceptId::CanalGuardian,
            },
        );
        let mut submitted = stored.value.clone();
        let transition = submitted
            .apply_trusted(
                &next,
                &TrustedMutationContext {
                    audit_id: "hero-audit:repository-rewrite".to_owned(),
                    actor_id: LOCAL_HERO_OWNER_KEY.to_owned(),
                    occurred_at_epoch_seconds: 1_001,
                },
            )
            .unwrap();
        submitted.pins = Some(HeroPins::mvp(ThemeId::EmberlineArchive));
        submitted.validate().unwrap();
        let response_json = serde_json::to_string(&transition).unwrap();
        let audit = HeroAuditPayload::CreationTransition {
            transition: Box::new(transition.transition_audit),
            character_created: None,
        };
        let receipt = NewHeroCommandReceipt {
            scope: HeroReceiptScope::Draft,
            scope_id: draft.draft_id.clone(),
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            idempotency_key: next.idempotency_key.clone(),
            command_kind: CREATION_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint_creation_command(&next).unwrap(),
            expected_revision: next.expected_revision,
            result_revision: submitted.revision,
            audit_id: audit.audit_id().to_owned(),
            response_json,
        };
        assert!(
            repository
                .commit_hero_creation_transition(
                    &submitted,
                    next.expected_revision,
                    &next,
                    &audit,
                    None,
                    HeroCreationCommitMetadata {
                        receipt: &receipt,
                        campaign_pins: None,
                    },
                )
                .await
                .is_err()
        );
        assert_eq!(
            repository
                .load_hero_draft(&draft.draft_id)
                .await
                .unwrap()
                .unwrap()
                .value
                .revision,
            1
        );

        let replacement = service.start_local_hero_creation().await.unwrap();
        assert!(matches!(
            service
                .apply_hero_creation_command(creation_command(
                    &replacement,
                    "later-theme-mutation",
                    HeroCreationIntent::SelectCampaignTheme {
                        pins: HeroPins::mvp(ThemeId::EmberlineArchive),
                    },
                ))
                .await,
            Err(ApplicationError::CampaignPinsQuarantined)
        ));
        assert_eq!(
            repository
                .load_campaign_pins(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .unwrap()
                .evidence
                .pins
                .hero
                .theme_id,
            ThemeId::RainboundBorough
        );

        sqlx::query("DELETE FROM campaign_content_pins WHERE campaign_session_id = $1")
            .bind(LOCAL_CAMPAIGN_SESSION_ID)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::CampaignPinsQuarantined)
        ));
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn finalize_is_atomic_replayable_and_bridges_the_slice_one_view(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, repository) = test_service(pool, clock);
        let (draft, command, outcome) = create_fighter(&service, "created-hero-1").await;
        assert_eq!(draft.step, CreationStep::Committed);
        assert_eq!(draft.revision, 9);
        let hero = outcome.character.clone().unwrap();
        assert_eq!(hero.revision, 0);
        assert_eq!(
            service
                .load_local_created_hero("created-hero-1")
                .await
                .unwrap(),
            hero
        );
        let replay = service.finalize_hero_creation(command).await.unwrap();
        assert_eq!(replay, outcome);
        let workspace = service.load_local_hero_workspace().await.unwrap();
        assert_eq!(workspace.draft, Some(draft.clone()));
        assert_eq!(workspace.character, Some(hero.clone()));

        let stored_draft = repository
            .load_hero_draft(&draft.draft_id)
            .await
            .unwrap()
            .unwrap();
        let stored_hero = repository
            .load_hero_character("created-hero-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_draft.revision, 10);
        assert_eq!(stored_hero.revision, 1);
        let audits = repository
            .list_hero_audits(LOCAL_CAMPAIGN_SESSION_ID, &draft.draft_id)
            .await
            .unwrap();
        assert_eq!(audits.len(), 9);
        assert_eq!(
            audits
                .iter()
                .map(|audit| audit.subject_revision)
                .collect::<Vec<_>>(),
            (1..=9).collect::<Vec<_>>()
        );

        let slice_one = service.load_local_campaign().await.unwrap();
        assert_eq!(slice_one.character_id, super::super::LOCAL_CHARACTER_ID);
        assert_eq!(slice_one.character_name, "Mara Vale");
        assert!(slice_one.encounter.is_none());
        let pin_evidence = slice_one.content_pins.sealed().unwrap();
        assert_eq!(pin_evidence.pins.hero, hero.choices.pins);
        assert_eq!(
            pin_evidence.seal_reason,
            manchester_dnd_core::CampaignPinSealReason::SelectedTheme
        );
        assert_eq!(
            pin_evidence.pins.prompt.template_id,
            manchester_dnd_core::CAMPAIGN_PROMPT_TEMPLATE_ID
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn trusted_reward_and_level_up_replay_and_reload_exactly(pool: PgPool) {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let (service, repository) = test_service(pool, clock);
        let (_, _, created) = create_fighter(&service, "advancing-hero").await;
        let hero = created.character.unwrap();
        let reward_command = RewardAwardCommand {
            schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
            character_id: hero.character_id.clone(),
            expected_revision: 0,
            idempotency_key: "major-reward".to_owned(),
            tier: RewardTier::Major,
        };
        let reward = service
            .apply_hero_reward(reward_command.clone())
            .await
            .unwrap();
        assert_eq!(reward.character.experience_points, 300);
        assert!(reward.character.level_up_eligible());
        assert_eq!(
            service.apply_hero_reward(reward_command).await.unwrap(),
            reward
        );

        let choices = service
            .hero_level_up_choices("advancing-hero")
            .await
            .unwrap();
        assert!(choices.eligible);
        assert_eq!(choices.revision, 1);
        assert_eq!(choices.choices.len(), 1);

        let forged = LevelUpCommand {
            schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
            character_id: "advancing-hero".to_owned(),
            expected_revision: 1,
            idempotency_key: "forged-wizard-level".to_owned(),
            choice: LevelUpChoice::Wizard {
                hit_points: HitPointGrowthChoice::FixedAverage,
                arcane_tradition: ArcaneTraditionId::Evocation,
            },
        };
        assert!(matches!(
            service.level_up_hero(forged).await,
            Err(ApplicationError::Hero(_))
        ));
        assert_eq!(
            service
                .load_local_created_hero("advancing-hero")
                .await
                .unwrap()
                .revision,
            1
        );

        let level_command = LevelUpCommand {
            schema_version: manchester_dnd_core::hero::HERO_COMMAND_SCHEMA_VERSION,
            character_id: "advancing-hero".to_owned(),
            expected_revision: 1,
            idempotency_key: "fighter-level-two".to_owned(),
            choice: LevelUpChoice::Fighter {
                hit_points: HitPointGrowthChoice::FixedAverage,
            },
        };
        let level_up = service.level_up_hero(level_command.clone()).await.unwrap();
        assert_eq!(level_up.character.level, SupportedLevel::Two);
        assert_eq!(level_up.character.revision, 2);
        assert_eq!(
            service.level_up_hero(level_command).await.unwrap(),
            level_up
        );
        assert_eq!(
            service
                .load_local_created_hero("advancing-hero")
                .await
                .unwrap(),
            level_up.character
        );
        let audits = repository
            .list_hero_audits(LOCAL_CAMPAIGN_SESSION_ID, "advancing-hero")
            .await
            .unwrap();
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].subject_revision, 1);
        assert_eq!(audits[1].subject_revision, 2);
    }

    #[test]
    fn unsupported_mechanics_keep_bounded_authored_alternatives() {
        let unsupported = manchester_dnd_core::hero::ActionCapability::from_mechanic_id(
            "action.teleport-anywhere",
        )
        .unwrap_err();
        let error = map_hero_error(HeroError::UnsupportedMechanic(unsupported.clone()));
        assert_eq!(error.public_code(), "unsupported_mechanic");
        assert_eq!(error.unsupported_hero_mechanic(), Some(&unsupported));
        assert!(!unsupported.alternatives.is_empty());
    }
}
