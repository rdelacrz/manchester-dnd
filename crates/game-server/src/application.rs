use std::{
    sync::{Arc, Mutex as StdMutex},
    time::{SystemTime, UNIX_EPOCH},
};

use manchester_dnd_core::{
    Ability, AbilityCheck, AbilityScores, AdvanceNpcTurnCommand, AttemptExplorationCheckCommand,
    AttemptSocialInteractionCommand, CampaignPinSealReason, CampaignPinStatusDto, Character,
    CharacterDraft, CheckDifficulty, CommitEncounterCommand, CommittedEncounterOutcomeDto,
    DeterministicRng, DiceExpression, DiceRoll, DiceSource, ENCOUNTER_COMMIT_SCHEMA_VERSION,
    EXPLORATION_CHECK_SCHEMA_VERSION, EncounterCommandOrigin, EncounterViewDto, EventActor,
    ExplorationCheckOutcomeDto, LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION, Level, LocalCampaignViewDto,
    ModifierComponent, Proficiency, RULESET, RollContext, RollError, RollMetadata, RollMode,
    RollRecord, SESSION_SCHEMA_VERSION, SOCIAL_INTERACTION_SCHEMA_VERSION, SealedCampaignPins,
    SessionDto, SessionEventDto, SessionEventPayload, SessionStatus, Sha256Digest,
    SocialInteractionOutcomeDto, SocialSceneViewDto,
    encounter::{
        CANAL_WARDEN_ID, DamageType as EncounterDamageType, EncounterAttack, EncounterCommand,
        EncounterError, EncounterHeroProfile, EncounterHeroRulesProfile, EncounterIntent,
        EncounterResolution, EncounterRollMode, EncounterRollPurpose, EncounterRollSource,
        EncounterState, LethalityPolicy, OpeningConsequence, RollModifierFact,
        SOOT_WIGHT_ENCOUNTER_ID, SOOT_WIGHT_POLICY_ID, player_legal_actions,
        require_player_control, resolve_encounter, select_soot_wight_policy_intent,
    },
    hero::{
        DamageType as HeroDamageType, HeroCharacter, HeroClass, ResourceKind, SkillId, ThemeId,
    },
    is_valid_opaque_id,
    rules_matrix::{
        AttitudeShift, ClockKind, D20TestOutcome, ExplorationSocialCommand, ExplorationSocialFact,
        ExplorationSocialState, HighStakesKind, NpcAttitude, NpcSocialState, ObjectiveProgress,
        ProgressStatus, RULES_MATRIX_SCHEMA_VERSION, RuntimeResources, SceneClock,
        SpellcastingState, TrustedCheckRequest, TrustedCheckResolution,
        apply_exploration_social_command, resolve_trusted_check,
    },
};
use rand::Rng as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{
    campaign_pins::CampaignPinRuntime,
    config::AccessMode,
    error::{ApplicationError, RepositoryError},
    repository::{
        EncounterHeroUpdate, NewCommandReceipt, PostgresRepository, StoredDocument, TurnAudit,
    },
    seed::SeedVault,
};

mod hero;
mod lifecycle;
mod player_characters;

pub use hero::{
    ClaimEncounterRewardCommand, EncounterRewardClaimOutcomeDto, HERO_APPLICATION_SCHEMA_VERSION,
    HERO_DRAFT_RETENTION_SECONDS, HERO_DRAFT_TTL_SECONDS, HeroLevelUpChoicesDto,
    HeroLevelUpOutcomeDto, HeroRewardOutcomeDto, LOCAL_HERO_OWNER_KEY, LocalHeroWorkspaceDto,
};
pub(crate) use lifecycle::map_lifecycle_repository_error;
pub use player_characters::{
    PLAYER_CHARACTER_DRAFT_RETENTION_SECONDS, PLAYER_CHARACTER_DRAFT_TTL_SECONDS,
};

pub const LOCAL_CAMPAIGN_SESSION_ID: &str = "local-campaign";
pub const LOCAL_CHARACTER_ID: &str = "local-hero";
pub const LOCAL_EXPLORATION_ACTION_ID: &str = "inspect-viaduct-runes";
pub const LOCAL_SOCIAL_ACTION_ID: &str = "parley-lockkeeper";

const LOCAL_CAMPAIGN_TITLE: &str = "The Runes Beneath the Viaduct";
const LOCAL_CHARACTER_NAME: &str = "Mara";
const LOCAL_CHARACTER_THEME: &str = "canal warden";
const EXPLORATION_COMMAND_KIND: &str = "attempt-exploration-check";
const SOCIAL_COMMAND_KIND: &str = "attempt-social-interaction";
const ENCOUNTER_COMMAND_KIND: &str = "commit-encounter-command";
const NPC_ADVANCE_COMMAND_KIND: &str = "advance-npc-turn";
const SOCIAL_OBJECTIVE_ID: &str = "earn-lockkeepers-trust";
const SOCIAL_CLOCK_ID: &str = "soot-tide-rises";
const SOCIAL_NPC_ID: &str = "lockkeeper-elin";

enum EncounterMutation {
    Player(CommitEncounterCommand),
    DeterministicNpc(AdvanceNpcTurnCommand),
}

impl EncounterMutation {
    fn validate(&self) -> Result<(), ApplicationError> {
        match self {
            Self::Player(command) => command.validate(),
            Self::DeterministicNpc(command) => command.validate(),
        }
        .map_err(ApplicationError::InvalidCommand)
    }

    fn campaign_session_id(&self) -> &str {
        match self {
            Self::Player(command) => &command.campaign_session_id,
            Self::DeterministicNpc(command) => &command.campaign_session_id,
        }
    }

    const fn expected_campaign_revision(&self) -> u64 {
        match self {
            Self::Player(command) => command.expected_campaign_revision,
            Self::DeterministicNpc(command) => command.expected_campaign_revision,
        }
    }

    const fn expected_encounter_revision(&self) -> u64 {
        match self {
            Self::Player(command) => command.command.expected_revision,
            Self::DeterministicNpc(command) => command.expected_encounter_revision,
        }
    }

    fn idempotency_key(&self) -> &str {
        match self {
            Self::Player(command) => &command.command.idempotency_key,
            Self::DeterministicNpc(command) => &command.idempotency_key,
        }
    }

    const fn command_kind(&self) -> &'static str {
        match self {
            Self::Player(_) => ENCOUNTER_COMMAND_KIND,
            Self::DeterministicNpc(_) => NPC_ADVANCE_COMMAND_KIND,
        }
    }

    fn fingerprint(&self) -> Result<Sha256Digest, ApplicationError> {
        match self {
            Self::Player(command) => fingerprint_encounter_command(command),
            Self::DeterministicNpc(command) => fingerprint_npc_advance_command(command),
        }
    }

    fn derive_command(
        &self,
        session: &StoredDocument<SessionDto>,
        encounter: &EncounterViewDto,
    ) -> Result<(CommitEncounterCommand, EncounterCommandOrigin), ApplicationError> {
        match self {
            Self::Player(command) => {
                validate_local_encounter_command(command, session, encounter)?;
                require_player_control(&encounter.state).map_err(map_encounter_error)?;
                Ok((command.clone(), EncounterCommandOrigin::Player))
            }
            Self::DeterministicNpc(command) => {
                validate_local_npc_advance_command(command, session, encounter)?;
                let intent = select_soot_wight_policy_intent(&encounter.state)
                    .map_err(map_encounter_error)?;
                Ok((
                    CommitEncounterCommand {
                        schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
                        campaign_session_id: command.campaign_session_id.clone(),
                        expected_campaign_revision: command.expected_campaign_revision,
                        command: EncounterCommand::new(
                            command.expected_encounter_revision,
                            command.idempotency_key.clone(),
                            intent,
                        ),
                    },
                    EncounterCommandOrigin::DeterministicPolicy {
                        policy_id: SOOT_WIGHT_POLICY_ID.to_owned(),
                    },
                ))
            }
        }
    }
}

/// Supplies wall-clock timestamps while keeping application tests deterministic.
pub trait UnixTimeSource: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

impl<F> UnixTimeSource for F
where
    F: Fn() -> u64 + Send + Sync,
{
    fn now_unix_ms(&self) -> u64 {
        self()
    }
}

#[derive(Clone)]
pub struct GameApplicationService {
    access_mode: AccessMode,
    repository: PostgresRepository,
    seed_vault: Arc<SeedVault>,
    campaign_pins: Arc<CampaignPinRuntime>,
    dice: Arc<StdMutex<Box<dyn DiceSource + Send>>>,
    clock: Arc<dyn UnixTimeSource>,
    command_gate: Arc<AsyncMutex<()>>,
}

impl GameApplicationService {
    pub fn new(
        access_mode: AccessMode,
        repository: PostgresRepository,
        seed_vault: Arc<SeedVault>,
        campaign_pins: Arc<CampaignPinRuntime>,
    ) -> Self {
        Self::with_sources_and_pins(
            access_mode,
            repository,
            seed_vault,
            campaign_pins,
            SystemDice,
            SystemClock,
        )
    }

    /// Read-only access to the underlying repository for scoped queries that
    /// do not belong to the game application service (e.g. player character
    /// library operations).
    #[must_use]
    pub fn repository(&self) -> &PostgresRepository {
        &self.repository
    }

    /// Verifies that `account_id` is an active member of `campaign_id`.
    /// Returns `NotFound` for non-members and non-existent campaigns alike
    /// (anti-enumeration). This is the guard every hosted-mode server function
    /// must call before returning campaign data.
    pub async fn assert_member_access(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<(), ApplicationError> {
        let is_member = self
            .repository
            .is_active_member(account_id, campaign_id)
            .await
            .map_err(map_lifecycle_repository_error)?;
        if !is_member {
            return Err(ApplicationError::WrongCampaign);
        }
        Ok(())
    }

    /// Loads a campaign summary after verifying the caller is an active member.
    /// Returns `None` for non-members and non-existent campaigns (anti-enumeration).
    pub async fn load_member_campaign(
        &self,
        account_id: &str,
        campaign_id: &str,
    ) -> Result<Option<crate::repository::lifecycle::CampaignSummary>, ApplicationError> {
        self.repository
            .load_member_campaign_summary(account_id, campaign_id)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    #[cfg(test)]
    pub fn with_sources(
        access_mode: AccessMode,
        repository: PostgresRepository,
        seed_vault: Arc<SeedVault>,
        dice: impl DiceSource + Send + 'static,
        clock: impl UnixTimeSource + 'static,
    ) -> Self {
        Self::with_sources_and_pins(
            access_mode,
            repository,
            seed_vault,
            Arc::new(CampaignPinRuntime::bundled_for_tests()),
            dice,
            clock,
        )
    }

    fn with_sources_and_pins(
        access_mode: AccessMode,
        repository: PostgresRepository,
        seed_vault: Arc<SeedVault>,
        campaign_pins: Arc<CampaignPinRuntime>,
        dice: impl DiceSource + Send + 'static,
        clock: impl UnixTimeSource + 'static,
    ) -> Self {
        Self {
            access_mode,
            repository,
            seed_vault,
            campaign_pins,
            dice: Arc::new(StdMutex::new(Box::new(dice))),
            clock: Arc::new(clock),
            command_gate: Arc::new(AsyncMutex::new(())),
        }
    }

    pub const fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    pub async fn health_check(&self) -> Result<(), ApplicationError> {
        self.repository
            .health_check()
            .await
            .map_err(ApplicationError::Repository)
    }

    /// Loads the one local campaign, creating its fixed level-one hero on the
    /// first request. Hosted access remains unavailable until authentication
    /// and campaign authorization are implemented.
    pub async fn load_local_campaign(&self) -> Result<LocalCampaignViewDto, ApplicationError> {
        self.require_local_mode()?;
        let _guard = self.command_gate.lock().await;
        let (session, character) = self.load_or_create_local_campaign().await?;
        let content_pins = self.resolve_campaign_pin_status(&session).await?;
        self.build_local_view(&session, &character, content_pins)
            .await
    }

    /// Resolves the authored lockkeeper conversation through the trusted
    /// difficulty map, then commits the roll, objective, clock, attitude, and
    /// idempotency receipt as one revision.
    pub async fn attempt_social_interaction(
        &self,
        command: AttemptSocialInteractionCommand,
    ) -> Result<SocialInteractionOutcomeDto, ApplicationError> {
        let correlation_id = format!("internal:{}", Uuid::new_v4().simple());
        self.attempt_social_interaction_with_correlation(command, &correlation_id)
            .await
    }

    pub async fn attempt_social_interaction_with_correlation(
        &self,
        command: AttemptSocialInteractionCommand,
        correlation_id: &str,
    ) -> Result<SocialInteractionOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        if !is_valid_opaque_id(correlation_id) {
            return Err(ApplicationError::InvalidStoredState);
        }
        command
            .validate()
            .map_err(ApplicationError::InvalidCommand)?;
        let fingerprint = fingerprint_social_command(&command)?;

        let _guard = self.command_gate.lock().await;
        let (stored_session, stored_character) = self.load_or_create_local_campaign().await?;
        self.require_sealed_campaign_pins(&stored_session).await?;
        let authoritative_hero = self.load_local_authoritative_hero().await?;

        if let Some(receipt) = self
            .repository
            .load_command_receipt(&command.campaign_session_id, &command.idempotency_key)
            .await
            .map_err(ApplicationError::Repository)?
        {
            return social_outcome_from_receipt(&command, &fingerprint, &receipt);
        }

        validate_social_command(&command, &stored_session, &stored_character)?;

        let existing_events = self
            .repository
            .list_session_events(&stored_session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        validate_event_stream(&stored_session, &existing_events)?;
        if existing_events.iter().any(|audit| {
            matches!(
                &audit.payload.payload,
                SessionEventPayload::ExplorationSocialResolved { .. }
                    | SessionEventPayload::AbilityCheckResolved { .. }
                    | SessionEventPayload::EncounterResolved { .. }
            )
        }) {
            return Err(ApplicationError::UnknownAction(command.action_id.clone()));
        }

        let (ability_scores, level, proficiency) = social_actor(
            &stored_character.value,
            authoritative_hero.as_ref().map(|stored| &stored.value),
        )?;
        let request = TrustedCheckRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: Ability::Charisma,
            skill: Some(SkillId::Persuasion),
            proficiency,
            difficulty: CheckDifficulty::Moderate,
            stakes: HighStakesKind::None,
            player_confirmed: false,
            roll_context: RollContext::normal(),
            situational_modifiers: Vec::new(),
        };
        let check = {
            let mut dice = self
                .dice
                .lock()
                .map_err(|_| ApplicationError::InvalidStoredState)?;
            let mut dice = DynamicDice(&mut **dice);
            resolve_trusted_check(&ability_scores, level, &request, &mut dice)
                .map_err(|_| ApplicationError::InvalidStoredState)?
        };
        let (resulting_state, facts) = resolve_authored_social_state(&check)?;
        let next_revision = stored_session
            .revision
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let next_sequence = stored_session
            .value
            .last_event_sequence
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let outcome = SocialInteractionOutcomeDto {
            schema_version: SOCIAL_INTERACTION_SCHEMA_VERSION,
            campaign_session_id: command.campaign_session_id.clone(),
            character_id: command.character_id.clone(),
            action_id: command.action_id.clone(),
            result_revision: next_revision,
            event_sequence: next_sequence,
            check,
            facts,
            resulting_state,
        };
        outcome
            .validate()
            .map_err(ApplicationError::InvalidOutcome)?;
        validate_authored_social_outcome(&outcome)?;

        let occurred_at_unix_ms = self
            .clock
            .now_unix_ms()
            .max(stored_session.value.updated_at_unix_ms);
        let mut post_session = stored_session.value.clone();
        post_session.updated_at_unix_ms = occurred_at_unix_ms;
        post_session.last_event_sequence = next_sequence;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: command.campaign_session_id.clone(),
            sequence: next_sequence,
            occurred_at_unix_ms,
            actor: EventActor::System,
            payload: SessionEventPayload::ExplorationSocialResolved {
                command: command.clone(),
                outcome: Box::new(outcome.clone()),
            },
        };
        event.validate().map_err(ApplicationError::InvalidOutcome)?;

        let audit_id = format!("turn:{}", Uuid::new_v4().simple());
        let response_json =
            serde_json::to_string(&outcome).map_err(ApplicationError::Serialization)?;
        let receipt = NewCommandReceipt {
            campaign_session_id: command.campaign_session_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: SOCIAL_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint,
            expected_revision: command.expected_revision,
            result_revision: next_revision,
            audit_id,
            response_json,
        };

        match self
            .repository
            .commit_session_event_with_receipt_and_correlation(
                &post_session,
                command.expected_revision,
                &event,
                &[],
                &receipt,
                correlation_id,
            )
            .await
        {
            Ok(committed) if committed.session.revision == next_revision => Ok(outcome),
            Ok(_) => Err(ApplicationError::InvalidStoredState),
            Err(RepositoryError::RevisionConflict { actual, .. }) => {
                Err(ApplicationError::RevisionConflict {
                    expected: command.expected_revision,
                    current_revision: actual,
                })
            }
            Err(RepositoryError::AlreadyExists {
                entity: "command receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_command_receipt(&command.campaign_session_id, &command.idempotency_key)
                    .await
                    .map_err(ApplicationError::Repository)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                social_outcome_from_receipt(&command, &receipt.request_fingerprint, &stored)
            }
            Err(error) => Err(ApplicationError::Repository(error)),
        }
    }

    /// Resolves the sole authored exploration action with server-owned rules,
    /// dice, timestamps, audit identity, and persistence.
    pub async fn attempt_exploration_check(
        &self,
        command: AttemptExplorationCheckCommand,
    ) -> Result<ExplorationCheckOutcomeDto, ApplicationError> {
        let correlation_id = format!("internal:{}", Uuid::new_v4().simple());
        self.attempt_exploration_check_with_correlation(command, &correlation_id)
            .await
    }

    pub async fn attempt_exploration_check_with_correlation(
        &self,
        command: AttemptExplorationCheckCommand,
        correlation_id: &str,
    ) -> Result<ExplorationCheckOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        if !is_valid_opaque_id(correlation_id) {
            return Err(ApplicationError::InvalidStoredState);
        }
        command
            .validate()
            .map_err(ApplicationError::InvalidCommand)?;
        let fingerprint = fingerprint_command(&command)?;

        // Serializing local mutations ensures two duplicate requests cannot
        // both pass the receipt lookup and consume dice in this process.
        let _guard = self.command_gate.lock().await;
        let (stored_session, stored_character) = self.load_or_create_local_campaign().await?;
        self.require_sealed_campaign_pins(&stored_session).await?;
        let authoritative_hero = self.load_local_authoritative_hero().await?;

        if let Some(receipt) = self
            .repository
            .load_command_receipt(&command.campaign_session_id, &command.idempotency_key)
            .await
            .map_err(ApplicationError::Repository)?
        {
            return outcome_from_receipt(&command, &fingerprint, &receipt);
        }

        validate_local_command(&command, &stored_session, &stored_character)?;
        let existing_events = self
            .repository
            .list_session_events(&stored_session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        validate_event_stream(&stored_session, &existing_events)?;
        if existing_events.iter().any(|audit| {
            matches!(
                &audit.payload.payload,
                SessionEventPayload::EncounterResolved { .. }
            )
        }) {
            return Err(ApplicationError::UnknownAction(command.action_id.clone()));
        }

        let next_revision = stored_session
            .revision
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let next_sequence = stored_session
            .value
            .last_event_sequence
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let occurred_at_unix_ms = self
            .clock
            .now_unix_ms()
            .max(stored_session.value.updated_at_unix_ms);

        let (ability_scores, level, perception_proficiency) = exploration_actor(
            &stored_character.value,
            authoritative_hero.as_ref().map(|stored| &stored.value),
        )?;
        let check = authored_exploration_check(&command.action_id, perception_proficiency)
            .ok_or_else(|| ApplicationError::UnknownAction(command.action_id.clone()))?;
        let result = {
            let mut dice = self
                .dice
                .lock()
                .map_err(|_| ApplicationError::InvalidStoredState)?;
            let mut dice = DynamicDice(&mut **dice);
            check
                .resolve(&ability_scores, level, &mut dice)
                .map_err(ApplicationError::Rules)?
        };

        let outcome = ExplorationCheckOutcomeDto {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: command.campaign_session_id.clone(),
            character_id: command.character_id.clone(),
            action_id: command.action_id.clone(),
            result_revision: next_revision,
            event_sequence: next_sequence,
            result: result.clone(),
        };
        outcome
            .validate()
            .map_err(ApplicationError::InvalidOutcome)?;

        let mut post_session = stored_session.value.clone();
        post_session.updated_at_unix_ms = occurred_at_unix_ms;
        post_session.last_event_sequence = next_sequence;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: command.campaign_session_id.clone(),
            sequence: next_sequence,
            occurred_at_unix_ms,
            actor: EventActor::System,
            payload: SessionEventPayload::AbilityCheckResolved {
                character_id: command.character_id.clone(),
                action_id: command.action_id.clone(),
                result,
            },
        };
        event.validate().map_err(ApplicationError::InvalidOutcome)?;

        let audit_id = format!("turn:{}", Uuid::new_v4().simple());
        let response_json =
            serde_json::to_string(&outcome).map_err(ApplicationError::Serialization)?;
        let receipt = NewCommandReceipt {
            campaign_session_id: command.campaign_session_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: EXPLORATION_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint,
            expected_revision: command.expected_revision,
            result_revision: next_revision,
            audit_id: audit_id.clone(),
            response_json,
        };

        match self
            .repository
            .commit_session_event_with_receipt_and_correlation(
                &post_session,
                command.expected_revision,
                &event,
                &[],
                &receipt,
                correlation_id,
            )
            .await
        {
            Ok(committed) if committed.session.revision == next_revision => Ok(outcome),
            Ok(_) => Err(ApplicationError::InvalidStoredState),
            Err(RepositoryError::RevisionConflict { actual, .. }) => {
                Err(ApplicationError::RevisionConflict {
                    expected: command.expected_revision,
                    current_revision: actual,
                })
            }
            Err(RepositoryError::AlreadyExists {
                entity: "command receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_command_receipt(&command.campaign_session_id, &command.idempotency_key)
                    .await
                    .map_err(ApplicationError::Repository)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                outcome_from_receipt(&command, &receipt.request_fingerprint, &stored)
            }
            Err(error) => Err(ApplicationError::Repository(error)),
        }
    }

    /// Resolves and atomically commits one encounter command, its canonical roll records,
    /// successor state, append-only event, and idempotency receipt.
    pub async fn commit_encounter_command(
        &self,
        command: CommitEncounterCommand,
    ) -> Result<CommittedEncounterOutcomeDto, ApplicationError> {
        let correlation_id = format!("internal:{}", Uuid::new_v4().simple());
        self.commit_encounter_command_with_correlation(command, &correlation_id)
            .await
    }

    pub async fn commit_encounter_command_with_correlation(
        &self,
        command: CommitEncounterCommand,
        correlation_id: &str,
    ) -> Result<CommittedEncounterOutcomeDto, ApplicationError> {
        self.commit_encounter_mutation(EncounterMutation::Player(command), correlation_id)
            .await
    }

    pub async fn advance_npc_turn(
        &self,
        command: AdvanceNpcTurnCommand,
    ) -> Result<CommittedEncounterOutcomeDto, ApplicationError> {
        let correlation_id = format!("internal:{}", Uuid::new_v4().simple());
        self.advance_npc_turn_with_correlation(command, &correlation_id)
            .await
    }

    pub async fn advance_npc_turn_with_correlation(
        &self,
        command: AdvanceNpcTurnCommand,
        correlation_id: &str,
    ) -> Result<CommittedEncounterOutcomeDto, ApplicationError> {
        self.commit_encounter_mutation(EncounterMutation::DeterministicNpc(command), correlation_id)
            .await
    }

    async fn commit_encounter_mutation(
        &self,
        mutation: EncounterMutation,
        correlation_id: &str,
    ) -> Result<CommittedEncounterOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        if !is_valid_opaque_id(correlation_id) {
            return Err(ApplicationError::InvalidStoredState);
        }
        mutation.validate()?;
        let fingerprint = mutation.fingerprint()?;

        // The gate prevents duplicate local requests from both resolving the same cursor. The
        // database revision and receipt uniqueness remain the cross-process authority.
        let _guard = self.command_gate.lock().await;
        let (stored_session, _stored_character) = self.load_or_create_local_campaign().await?;
        self.require_sealed_campaign_pins(&stored_session).await?;
        let authoritative_hero = self.load_local_authoritative_hero().await?;
        let current_hero_profile = authoritative_hero
            .as_ref()
            .map(|stored| encounter_profile_from_hero(&stored.value))
            .transpose()?;

        if let Some(receipt) = self
            .repository
            .load_command_receipt(mutation.campaign_session_id(), mutation.idempotency_key())
            .await
            .map_err(ApplicationError::Repository)?
        {
            return encounter_outcome_from_mutation_receipt(&mutation, &fingerprint, &receipt);
        }

        let events = self
            .repository
            .list_session_events(&stored_session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        validate_event_stream(&stored_session, &events)?;
        let latest_check =
            latest_exploration_check(&events)?.ok_or(ApplicationError::EncounterUnavailable)?;
        let campaign_seed = self
            .seed_vault
            .derive_campaign_seed(&stored_session.id)
            .map_err(ApplicationError::SeedVault)?;
        let projected = project_encounter(
            &stored_session,
            &latest_check,
            &events,
            campaign_seed.expose_to_engine(),
            campaign_seed.reference(),
            current_hero_profile.as_ref(),
        )?;
        if projected.view.state.schema_version
            != manchester_dnd_core::encounter::ENCOUNTER_SCHEMA_VERSION
        {
            return Err(ApplicationError::EncounterUnavailable);
        }
        if let Some(stored) = authoritative_hero.as_ref() {
            ensure_hero_runtime_matches_encounter(&stored.value, &projected.view.state)?;
        } else if projected.view.state.hero.source_character_id.is_some() {
            return Err(ApplicationError::InvalidStoredState);
        }
        let (command, command_origin) =
            mutation.derive_command(&stored_session, &projected.view)?;

        let next_campaign_revision = stored_session
            .revision
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let next_sequence = stored_session
            .value
            .last_event_sequence
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let occurred_at_unix_ms = self
            .clock
            .now_unix_ms()
            .max(stored_session.value.updated_at_unix_ms);

        let mut roll_source = EncounterRngAdapter::new(DeterministicRng::at_cursor(
            campaign_seed.expose_to_engine(),
            projected.next_cursor,
        ));
        let resolution =
            match resolve_encounter(&projected.view.state, &command.command, &mut roll_source) {
                Ok(resolution) if roll_source.error.is_none() => resolution,
                Ok(_) => {
                    return Err(ApplicationError::Roll(
                        roll_source
                            .error
                            .take()
                            .expect("error guard established above"),
                    ));
                }
                Err(_) if roll_source.error.is_some() => {
                    return Err(ApplicationError::Roll(
                        roll_source
                            .error
                            .take()
                            .expect("error guard established above"),
                    ));
                }
                Err(error) => return Err(map_encounter_error(error)),
            };
        let roll_records = canonical_roll_records(
            &stored_session.id,
            next_sequence,
            campaign_seed.reference(),
            &resolution,
            &roll_source.spans,
        )?;
        let legal_actions = player_legal_actions(&resolution.state).map_err(map_encounter_error)?;
        let (hero_candidate, result_hero_revision) =
            if let Some(stored) = authoritative_hero.as_ref() {
                synchronize_hero_after_encounter(stored, &resolution.state)?
            } else {
                (None, None)
            };
        let outcome = CommittedEncounterOutcomeDto {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: stored_session.id.clone(),
            result_campaign_revision: next_campaign_revision,
            event_sequence: next_sequence,
            result_hero_revision,
            resolution,
            roll_records,
            legal_actions,
        };
        outcome
            .validate_player_action_boundary()
            .map_err(ApplicationError::InvalidOutcome)?;

        let mut post_session = stored_session.value.clone();
        post_session.updated_at_unix_ms = occurred_at_unix_ms;
        post_session.last_event_sequence = next_sequence;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: stored_session.id.clone(),
            sequence: next_sequence,
            occurred_at_unix_ms,
            actor: EventActor::System,
            payload: SessionEventPayload::EncounterResolved {
                command: command.clone(),
                outcome: Box::new(outcome.clone()),
                command_origin,
            },
        };
        event.validate().map_err(ApplicationError::InvalidOutcome)?;

        let audit_id = format!("turn:{}", Uuid::new_v4().simple());
        let response_json =
            serde_json::to_string(&outcome).map_err(ApplicationError::Serialization)?;
        let receipt = NewCommandReceipt {
            campaign_session_id: stored_session.id.clone(),
            idempotency_key: mutation.idempotency_key().to_owned(),
            command_kind: mutation.command_kind().to_owned(),
            request_fingerprint: fingerprint,
            expected_revision: mutation.expected_campaign_revision(),
            result_revision: next_campaign_revision,
            audit_id,
            response_json,
        };

        let hero_update = hero_candidate
            .as_ref()
            .map(|character| EncounterHeroUpdate {
                character,
                expected_revision: authoritative_hero
                    .as_ref()
                    .expect("candidate requires an authoritative hero")
                    .value
                    .revision,
            });
        match self
            .repository
            .commit_encounter_event_with_receipt_and_correlation(
                &post_session,
                mutation.expected_campaign_revision(),
                &event,
                hero_update,
                &receipt,
                correlation_id,
            )
            .await
        {
            Ok(committed)
                if committed.session.revision == next_campaign_revision
                    && match (&hero_candidate, &committed.hero_character) {
                        (Some(candidate), Some(save)) => {
                            candidate.revision.checked_add(1) == Some(save.revision)
                        }
                        (None, None) => true,
                        _ => false,
                    } =>
            {
                Ok(outcome)
            }
            Ok(_) => Err(ApplicationError::InvalidStoredState),
            Err(RepositoryError::RevisionConflict { actual, .. }) => {
                if let Some(stored) = self
                    .repository
                    .load_command_receipt(&command.campaign_session_id, mutation.idempotency_key())
                    .await
                    .map_err(ApplicationError::Repository)?
                {
                    encounter_outcome_from_mutation_receipt(
                        &mutation,
                        &receipt.request_fingerprint,
                        &stored,
                    )
                } else {
                    Err(ApplicationError::RevisionConflict {
                        expected: mutation.expected_campaign_revision(),
                        current_revision: actual,
                    })
                }
            }
            Err(RepositoryError::AlreadyExists {
                entity: "command receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_command_receipt(&command.campaign_session_id, mutation.idempotency_key())
                    .await
                    .map_err(ApplicationError::Repository)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                encounter_outcome_from_mutation_receipt(
                    &mutation,
                    &receipt.request_fingerprint,
                    &stored,
                )
            }
            Err(error) => Err(ApplicationError::Repository(error)),
        }
    }

    fn require_local_mode(&self) -> Result<(), ApplicationError> {
        match self.access_mode {
            AccessMode::LocalSingleUser => Ok(()),
            AccessMode::Hosted => Err(ApplicationError::HostedAccessDenied),
        }
    }

    async fn load_or_create_local_campaign(
        &self,
    ) -> Result<(StoredDocument<SessionDto>, StoredDocument<Character>), ApplicationError> {
        if self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .is_none()
        {
            self.explicit_delete_prevents_implicit_recreation().await?;
            let now = self.clock.now_unix_ms();
            let session = local_session(now);
            let character = local_character()?;
            match self
                .repository
                .create_campaign(&session, std::slice::from_ref(&character))
                .await
            {
                Ok(_) | Err(RepositoryError::AlreadyExists { .. }) => {}
                Err(error) => return Err(ApplicationError::Repository(error)),
            }
        }

        self.require_local_campaign_active().await?;

        let session = self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::InvalidStoredState)?;
        let character = self
            .repository
            .load_character(LOCAL_CHARACTER_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::InvalidStoredState)?;
        validate_local_documents(&session, &character)?;
        Ok((session, character))
    }

    async fn load_local_authoritative_hero(
        &self,
    ) -> Result<Option<StoredDocument<HeroCharacter>>, ApplicationError> {
        let hero = self
            .repository
            .load_hero_character_for_owner(LOCAL_CAMPAIGN_SESSION_ID, LOCAL_HERO_OWNER_KEY)
            .await
            .map_err(ApplicationError::Repository)?;
        if let Some(stored) = &hero {
            validate_local_authoritative_hero(&stored.value)?;
        }
        Ok(hero)
    }

    async fn resolve_campaign_pin_status(
        &self,
        session: &StoredDocument<SessionDto>,
    ) -> Result<CampaignPinStatusDto, ApplicationError> {
        let hero = self.load_local_authoritative_hero().await?;
        let draft = self
            .repository
            .load_latest_pinned_hero_draft_for_owner(&session.id, LOCAL_HERO_OWNER_KEY)
            .await
            .map_err(ApplicationError::Repository)?;
        let selected_hero_pins = hero
            .as_ref()
            .map(|stored| &stored.value.choices.pins)
            .or_else(|| draft.as_ref().and_then(|stored| stored.value.pins.as_ref()));

        if let Some(stored) = self
            .repository
            .load_campaign_pins(&session.id)
            .await
            .map_err(map_campaign_pin_repository_error)?
        {
            self.campaign_pins
                .validate(&stored.evidence.pins)
                .map_err(|_| ApplicationError::CampaignPinsQuarantined)?;
            if session.value.ruleset != stored.evidence.pins.hero.ruleset_id
                || selected_hero_pins.is_some_and(|hero_pins| {
                    hero_pins != &stored.evidence.pins.hero
                        && stored.evidence.legacy_source.as_ref() != Some(hero_pins)
                })
            {
                return Err(ApplicationError::CampaignPinsQuarantined);
            }
            return Ok(CampaignPinStatusDto::Sealed {
                evidence: Box::new(stored.evidence),
            });
        }

        let legacy_eligible = self
            .repository
            .campaign_pin_legacy_eligible(&session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        if legacy_eligible {
            let (theme_id, mut seal_reason) = selected_hero_pins.map_or(
                (
                    ThemeId::RainboundBorough,
                    CampaignPinSealReason::LegacyDefaultRainbound,
                ),
                |pins| (pins.theme_id, CampaignPinSealReason::LegacySelectedTheme),
            );
            let pins = self
                .campaign_pins
                .pins_for_theme(theme_id)
                .map_err(|_| ApplicationError::CampaignPinsQuarantined)?;
            let legacy_source = selected_hero_pins
                .filter(|selected| selected.is_legacy_dev_alias())
                .cloned();
            if legacy_source.is_some() {
                seal_reason = CampaignPinSealReason::LegacyDigestAlias;
            }
            if selected_hero_pins
                .is_some_and(|selected| selected != &pins.hero && !selected.is_legacy_dev_alias())
                || session.value.ruleset != pins.hero.ruleset_id
            {
                return Err(ApplicationError::CampaignPinsQuarantined);
            }
            let stored = self
                .repository
                .seal_legacy_campaign_pins(
                    &session.id,
                    &SealedCampaignPins {
                        seal_reason,
                        pins,
                        legacy_source,
                    },
                )
                .await
                .map_err(ApplicationError::Repository)?;
            return Ok(CampaignPinStatusDto::Sealed {
                evidence: Box::new(stored.evidence),
            });
        }

        if selected_hero_pins.is_some() || session.value.last_event_sequence != 0 {
            return Err(ApplicationError::CampaignPinsQuarantined);
        }
        Ok(CampaignPinStatusDto::UnsealedCreatorScaffold)
    }

    async fn require_sealed_campaign_pins(
        &self,
        session: &StoredDocument<SessionDto>,
    ) -> Result<SealedCampaignPins, ApplicationError> {
        match self.resolve_campaign_pin_status(session).await? {
            CampaignPinStatusDto::Sealed { evidence } => Ok(*evidence),
            CampaignPinStatusDto::UnsealedCreatorScaffold => {
                Err(ApplicationError::CampaignPinsUnsealed)
            }
        }
    }

    async fn pins_for_theme_selection(
        &self,
        session: &StoredDocument<SessionDto>,
        selected: &manchester_dnd_core::hero::HeroPins,
    ) -> Result<SealedCampaignPins, ApplicationError> {
        let pins = self
            .campaign_pins
            .pins_for_theme(selected.theme_id)
            .map_err(|_| ApplicationError::CampaignPinsQuarantined)?;
        if &pins.hero != selected || session.value.ruleset != pins.hero.ruleset_id {
            return Err(ApplicationError::CampaignPinsQuarantined);
        }
        if let CampaignPinStatusDto::Sealed { evidence } =
            self.resolve_campaign_pin_status(session).await?
            && evidence.pins != pins
        {
            return Err(ApplicationError::CampaignPinsQuarantined);
        }
        Ok(SealedCampaignPins {
            seal_reason: CampaignPinSealReason::SelectedTheme,
            pins,
            legacy_source: None,
        })
    }

    async fn build_local_view(
        &self,
        session: &StoredDocument<SessionDto>,
        character: &StoredDocument<Character>,
        content_pins: CampaignPinStatusDto,
    ) -> Result<LocalCampaignViewDto, ApplicationError> {
        let events = self
            .repository
            .list_session_events(&session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        validate_event_stream(session, &events)?;
        let authoritative_hero = self.load_local_authoritative_hero().await?;
        let current_hero_profile = authoritative_hero
            .as_ref()
            .map(|stored| encounter_profile_from_hero(&stored.value))
            .transpose()?;
        let latest_check = latest_exploration_check(&events)?;
        let social = if content_pins.sealed().is_some() {
            Some(project_social_scene(session, &events)?)
        } else {
            None
        };
        let encounter = if let Some(check) = &latest_check {
            let campaign_seed = self
                .seed_vault
                .derive_campaign_seed(&session.id)
                .map_err(ApplicationError::SeedVault)?;
            Some(
                project_encounter(
                    session,
                    check,
                    &events,
                    campaign_seed.expose_to_engine(),
                    campaign_seed.reference(),
                    current_hero_profile.as_ref(),
                )?
                .view,
            )
        } else {
            None
        };

        let view = LocalCampaignViewDto {
            schema_version: LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION,
            campaign_session_id: session.id.clone(),
            character_id: character.id.clone(),
            campaign_title: session.value.title.clone(),
            character_name: authoritative_hero.as_ref().map_or_else(
                || character.value.name().to_owned(),
                |stored| stored.value.choices.presentation.name.clone(),
            ),
            revision: session.revision,
            last_event_sequence: session.value.last_event_sequence,
            content_pins,
            social,
            latest_check,
            encounter,
        };
        view.validate().map_err(ApplicationError::InvalidOutcome)?;
        Ok(view)
    }
}

fn map_campaign_pin_repository_error(error: RepositoryError) -> ApplicationError {
    match &error {
        RepositoryError::InvalidStoredData {
            entity: "campaign content pins",
            ..
        }
        | RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            ..
        }
        | RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign content pins",
            ..
        } => ApplicationError::CampaignPinsQuarantined,
        _ => ApplicationError::Repository(error),
    }
}

#[derive(Serialize)]
struct NormalizedExplorationCommand<'a> {
    schema_version: u16,
    campaign_session_id: &'a str,
    character_id: &'a str,
    action_id: &'a str,
    expected_revision: u64,
}

#[derive(Serialize)]
struct NormalizedSocialCommand<'a> {
    schema_version: u16,
    campaign_session_id: &'a str,
    character_id: &'a str,
    action_id: &'a str,
    expected_revision: u64,
}

fn fingerprint_social_command(
    command: &AttemptSocialInteractionCommand,
) -> Result<Sha256Digest, ApplicationError> {
    let normalized = NormalizedSocialCommand {
        schema_version: command.schema_version,
        campaign_session_id: &command.campaign_session_id,
        character_id: &command.character_id,
        action_id: &command.action_id,
        expected_revision: command.expected_revision,
    };
    let serialized = serde_json::to_vec(&normalized).map_err(ApplicationError::Serialization)?;
    Ok(Sha256Digest::from_bytes(Sha256::digest(serialized).into()))
}

fn social_outcome_from_receipt(
    command: &AttemptSocialInteractionCommand,
    fingerprint: &Sha256Digest,
    receipt: &crate::repository::StoredCommandReceipt,
) -> Result<SocialInteractionOutcomeDto, ApplicationError> {
    if &receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.command_kind != SOCIAL_COMMAND_KIND
        || receipt.campaign_session_id != command.campaign_session_id
        || receipt.idempotency_key != command.idempotency_key
        || receipt.expected_revision != command.expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    let outcome: SocialInteractionOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    if outcome.campaign_session_id != command.campaign_session_id
        || outcome.character_id != command.character_id
        || outcome.action_id != command.action_id
        || outcome.result_revision != receipt.result_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    validate_authored_social_outcome(&outcome)?;
    Ok(outcome)
}

fn fingerprint_command(
    command: &AttemptExplorationCheckCommand,
) -> Result<Sha256Digest, ApplicationError> {
    let normalized = NormalizedExplorationCommand {
        schema_version: command.schema_version,
        campaign_session_id: &command.campaign_session_id,
        character_id: &command.character_id,
        action_id: &command.action_id,
        expected_revision: command.expected_revision,
    };
    let serialized = serde_json::to_vec(&normalized).map_err(ApplicationError::Serialization)?;
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    Ok(Sha256Digest::from_bytes(digest))
}

fn outcome_from_receipt(
    command: &AttemptExplorationCheckCommand,
    fingerprint: &Sha256Digest,
    receipt: &crate::repository::StoredCommandReceipt,
) -> Result<ExplorationCheckOutcomeDto, ApplicationError> {
    if &receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.command_kind != EXPLORATION_COMMAND_KIND
        || receipt.campaign_session_id != command.campaign_session_id
        || receipt.idempotency_key != command.idempotency_key
        || receipt.expected_revision != command.expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    let outcome: ExplorationCheckOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    if outcome.campaign_session_id != command.campaign_session_id
        || outcome.character_id != command.character_id
        || outcome.action_id != command.action_id
        || outcome.result_revision != receipt.result_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(outcome)
}

#[derive(Serialize)]
struct NormalizedEncounterCommand<'a> {
    schema_version: u16,
    campaign_session_id: &'a str,
    expected_campaign_revision: u64,
    encounter_schema_version: u16,
    encounter_id: &'a str,
    expected_encounter_revision: u64,
    intent: &'a EncounterIntent,
}

fn fingerprint_encounter_command(
    command: &CommitEncounterCommand,
) -> Result<Sha256Digest, ApplicationError> {
    let normalized = NormalizedEncounterCommand {
        schema_version: command.schema_version,
        campaign_session_id: &command.campaign_session_id,
        expected_campaign_revision: command.expected_campaign_revision,
        encounter_schema_version: command.command.schema_version,
        encounter_id: &command.command.encounter_id,
        expected_encounter_revision: command.command.expected_revision,
        intent: &command.command.intent,
    };
    let serialized = serde_json::to_vec(&normalized).map_err(ApplicationError::Serialization)?;
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    Ok(Sha256Digest::from_bytes(digest))
}

#[derive(Serialize)]
struct NormalizedNpcAdvanceCommand<'a> {
    schema_version: u16,
    campaign_session_id: &'a str,
    expected_campaign_revision: u64,
    expected_encounter_revision: u64,
}

fn fingerprint_npc_advance_command(
    command: &AdvanceNpcTurnCommand,
) -> Result<Sha256Digest, ApplicationError> {
    let normalized = NormalizedNpcAdvanceCommand {
        schema_version: command.schema_version,
        campaign_session_id: &command.campaign_session_id,
        expected_campaign_revision: command.expected_campaign_revision,
        expected_encounter_revision: command.expected_encounter_revision,
    };
    let serialized = serde_json::to_vec(&normalized).map_err(ApplicationError::Serialization)?;
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    Ok(Sha256Digest::from_bytes(digest))
}

fn encounter_outcome_from_mutation_receipt(
    mutation: &EncounterMutation,
    fingerprint: &Sha256Digest,
    receipt: &crate::repository::StoredCommandReceipt,
) -> Result<CommittedEncounterOutcomeDto, ApplicationError> {
    if &receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.command_kind != mutation.command_kind()
        || receipt.campaign_session_id != mutation.campaign_session_id()
        || receipt.idempotency_key != mutation.idempotency_key()
        || receipt.expected_revision != mutation.expected_campaign_revision()
        || receipt.expected_revision.checked_add(1) != Some(receipt.result_revision)
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    let mut outcome: CommittedEncounterOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    if outcome.campaign_session_id != mutation.campaign_session_id()
        || outcome.result_campaign_revision != receipt.result_revision
        || outcome.resolution.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
        || outcome.resolution.previous_revision != mutation.expected_encounter_revision()
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    // A receipt written before the player/controller boundary may contain the engine's creature
    // actions. Preserve replay compatibility without ever returning those choices to a client.
    outcome.legal_actions = player_legal_actions(&outcome.resolution.state)
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    outcome
        .validate_player_action_boundary()
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    Ok(outcome)
}

fn validate_event_stream(
    session: &StoredDocument<SessionDto>,
    events: &[TurnAudit<SessionEventDto>],
) -> Result<(), ApplicationError> {
    let expected_len = usize::try_from(session.value.last_event_sequence)
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    if events.len() != expected_len {
        return Err(ApplicationError::InvalidStoredState);
    }
    for (index, audit) in events.iter().enumerate() {
        let expected_sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or(ApplicationError::InvalidStoredState)?;
        if audit.campaign_session_id != session.id
            || audit.turn_number != expected_sequence
            || audit.payload.session_id != session.id
            || audit.payload.sequence != expected_sequence
        {
            return Err(ApplicationError::InvalidStoredState);
        }
    }
    Ok(())
}

fn latest_exploration_check(
    events: &[TurnAudit<SessionEventDto>],
) -> Result<Option<ExplorationCheckOutcomeDto>, ApplicationError> {
    for audit in events.iter().rev() {
        let SessionEventPayload::AbilityCheckResolved {
            character_id,
            action_id,
            result,
        } = &audit.payload.payload
        else {
            continue;
        };
        let outcome = ExplorationCheckOutcomeDto {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: audit.campaign_session_id.clone(),
            character_id: character_id.clone(),
            action_id: action_id.clone(),
            // Campaign creation is revision one and every subsequent revision has exactly one
            // corresponding ordered event.
            result_revision: audit
                .turn_number
                .checked_add(1)
                .ok_or(ApplicationError::InvalidStoredState)?,
            event_sequence: audit.turn_number,
            result: result.clone(),
        };
        outcome
            .validate()
            .map_err(|_| ApplicationError::InvalidStoredState)?;
        return Ok(Some(outcome));
    }
    Ok(None)
}

fn project_social_scene(
    session: &StoredDocument<SessionDto>,
    events: &[TurnAudit<SessionEventDto>],
) -> Result<SocialSceneViewDto, ApplicationError> {
    let latest_outcome = events.iter().rev().find_map(|audit| {
        let SessionEventPayload::ExplorationSocialResolved { outcome, .. } = &audit.payload.payload
        else {
            return None;
        };
        Some((**outcome).clone())
    });
    if let Some(outcome) = &latest_outcome {
        validate_authored_social_outcome(outcome)?;
    }
    let state = latest_outcome
        .as_ref()
        .map_or_else(initial_social_state, |outcome| {
            outcome.resulting_state.clone()
        });
    let view = SocialSceneViewDto {
        schema_version: SOCIAL_INTERACTION_SCHEMA_VERSION,
        campaign_session_id: session.id.clone(),
        campaign_revision: session.revision,
        last_event_sequence: session.value.last_event_sequence,
        state,
        latest_outcome,
    };
    view.validate()
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    Ok(view)
}

#[derive(Debug)]
struct ProjectedEncounter {
    view: EncounterViewDto,
    next_cursor: u64,
}

fn project_encounter(
    session: &StoredDocument<SessionDto>,
    latest_check: &ExplorationCheckOutcomeDto,
    events: &[TurnAudit<SessionEventDto>],
    seed: [u8; 32],
    seed_reference: &str,
    current_hero_profile: Option<&EncounterHeroProfile>,
) -> Result<ProjectedEncounter, ApplicationError> {
    if latest_check.campaign_session_id != session.id
        || latest_check.character_id != LOCAL_CHARACTER_ID
        || latest_check.action_id != LOCAL_EXPLORATION_ACTION_ID
        || latest_check.event_sequence > session.value.last_event_sequence
    {
        return Err(ApplicationError::InvalidStoredState);
    }

    let opening = if latest_check.result.success {
        OpeningConsequence::RunesUnderstood
    } else {
        OpeningConsequence::RunesMisread
    };
    let first_encounter_profile = events.iter().find_map(|audit| {
        let SessionEventPayload::EncounterResolved { outcome, .. } = &audit.payload.payload else {
            return None;
        };
        Some((
            outcome.resolution.state.schema_version,
            outcome.resolution.state.hero_profile(),
        ))
    });
    let mut state = match first_encounter_profile {
        Some((schema_version, Some(profile))) => {
            if schema_version == manchester_dnd_core::encounter::ENCOUNTER_SCHEMA_VERSION {
                EncounterState::new_for_hero(LethalityPolicy::StoryRecovery, opening, profile)
                    .map_err(|_| ApplicationError::InvalidStoredState)?
            } else {
                EncounterState::new_for_hero_for_historical_replay(
                    LethalityPolicy::StoryRecovery,
                    opening,
                    profile,
                    schema_version,
                )
                .map_err(|_| ApplicationError::InvalidStoredState)?
            }
        }
        Some((schema_version, None)) => {
            let mut state = EncounterState::new(LethalityPolicy::StoryRecovery, opening);
            if schema_version != manchester_dnd_core::encounter::ENCOUNTER_SCHEMA_VERSION {
                state
                    .pin_historical_schema_for_replay(schema_version)
                    .map_err(|_| ApplicationError::InvalidStoredState)?;
            }
            state
        }
        None => match current_hero_profile {
            Some(profile) => EncounterState::new_for_hero(
                LethalityPolicy::StoryRecovery,
                opening,
                profile.clone(),
            )
            .map_err(|_| ApplicationError::InvalidStoredState)?,
            None => EncounterState::new(LethalityPolicy::StoryRecovery, opening),
        },
    };
    let mut latest_outcome = None;
    let mut verification_rng = DeterministicRng::new(seed);
    let mut encounter_started = false;

    for audit in events {
        match &audit.payload.payload {
            SessionEventPayload::AbilityCheckResolved { .. } if encounter_started => {
                // The opening consequence is fixed when the first encounter command commits.
                return Err(ApplicationError::InvalidStoredState);
            }
            SessionEventPayload::EncounterResolved {
                command,
                outcome,
                command_origin,
            } => {
                if audit.turn_number <= latest_check.event_sequence
                    || command.expected_campaign_revision != audit.turn_number
                    || command.command.expected_revision != state.revision
                    || command.command.encounter_id != state.encounter_id
                    || outcome.resolution.previous_revision != state.revision
                {
                    return Err(ApplicationError::InvalidStoredState);
                }
                encounter_started = true;

                match command_origin {
                    EncounterCommandOrigin::LegacySystem => {}
                    EncounterCommandOrigin::Player => {
                        require_player_control(&state)
                            .map_err(|_| ApplicationError::InvalidStoredState)?;
                    }
                    EncounterCommandOrigin::DeterministicPolicy { policy_id }
                        if policy_id == SOOT_WIGHT_POLICY_ID =>
                    {
                        let selected = select_soot_wight_policy_intent(&state)
                            .map_err(|_| ApplicationError::InvalidStoredState)?;
                        if selected != command.command.intent {
                            return Err(ApplicationError::InvalidStoredState);
                        }
                    }
                    EncounterCommandOrigin::DeterministicPolicy { .. } => {
                        return Err(ApplicationError::InvalidStoredState);
                    }
                }

                for (raw, record) in outcome.resolution.rolls.iter().zip(&outcome.roll_records) {
                    if record.seed_reference != seed_reference
                        || record.cursor_before != verification_rng.cursor()
                    {
                        return Err(ApplicationError::InvalidStoredState);
                    }
                    for die in &raw.individual_dice {
                        let regenerated = verification_rng
                            .roll_die(u32::from(die.sides))
                            .map_err(|_| ApplicationError::InvalidStoredState)?;
                        if regenerated != u32::from(die.value) {
                            return Err(ApplicationError::InvalidStoredState);
                        }
                    }
                    if record.cursor_after != verification_rng.cursor() {
                        return Err(ApplicationError::InvalidStoredState);
                    }
                }

                let mut replay_rolls = RecordedEncounterRolls::from_resolution(&outcome.resolution);
                let replayed = resolve_encounter(&state, &command.command, &mut replay_rolls)
                    .map_err(|_| ApplicationError::InvalidStoredState)?;
                if replay_rolls.invalid
                    || replay_rolls.index != replay_rolls.dice.len()
                    || replayed != outcome.resolution
                {
                    return Err(ApplicationError::InvalidStoredState);
                }
                state = replayed.state.clone();
                let mut public_outcome = outcome.as_ref().clone();
                public_outcome.legal_actions = player_legal_actions(&state)
                    .map_err(|_| ApplicationError::InvalidStoredState)?;
                public_outcome
                    .validate_player_action_boundary()
                    .map_err(|_| ApplicationError::InvalidStoredState)?;
                latest_outcome = Some(public_outcome);
            }
            _ => {}
        }
    }

    let actions = player_legal_actions(&state).map_err(|_| ApplicationError::InvalidStoredState)?;
    let view = EncounterViewDto {
        schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
        campaign_session_id: session.id.clone(),
        campaign_revision: session.revision,
        last_event_sequence: session.value.last_event_sequence,
        state,
        legal_actions: actions,
        latest_outcome,
    };
    view.validate()
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    Ok(ProjectedEncounter {
        view,
        next_cursor: verification_rng.cursor(),
    })
}

#[derive(Debug)]
struct RecordedEncounterRolls {
    dice: Vec<(u16, u16)>,
    index: usize,
    invalid: bool,
}

impl RecordedEncounterRolls {
    fn from_resolution(resolution: &EncounterResolution) -> Self {
        let dice = resolution
            .rolls
            .iter()
            .flat_map(|roll| {
                roll.individual_dice
                    .iter()
                    .map(|die| (die.sides, die.value))
            })
            .collect();
        Self {
            dice,
            index: 0,
            invalid: false,
        }
    }
}

impl EncounterRollSource for RecordedEncounterRolls {
    fn roll_die(&mut self, sides: u16) -> u16 {
        let Some(&(expected_sides, value)) = self.dice.get(self.index) else {
            self.invalid = true;
            return 0;
        };
        self.index += 1;
        if expected_sides != sides {
            self.invalid = true;
            return 0;
        }
        value
    }
}

fn validate_local_encounter_command(
    command: &CommitEncounterCommand,
    session: &StoredDocument<SessionDto>,
    encounter: &EncounterViewDto,
) -> Result<(), ApplicationError> {
    if command.campaign_session_id != session.id {
        return Err(ApplicationError::WrongCampaign);
    }
    if session.value.status != SessionStatus::Active {
        return Err(ApplicationError::CampaignCompleted);
    }
    if command.expected_campaign_revision != session.revision {
        return Err(ApplicationError::RevisionConflict {
            expected: command.expected_campaign_revision,
            current_revision: session.revision,
        });
    }
    if command.command.encounter_id != SOOT_WIGHT_ENCOUNTER_ID {
        return Err(ApplicationError::InvalidEncounterCommand(
            EncounterError::WrongEncounter {
                expected: SOOT_WIGHT_ENCOUNTER_ID.to_owned(),
                actual: command.command.encounter_id.clone(),
            },
        ));
    }
    if command.command.expected_revision != encounter.state.revision {
        return Err(ApplicationError::EncounterRevisionConflict {
            expected: command.command.expected_revision,
            current_revision: encounter.state.revision,
        });
    }
    Ok(())
}

fn validate_local_npc_advance_command(
    command: &AdvanceNpcTurnCommand,
    session: &StoredDocument<SessionDto>,
    encounter: &EncounterViewDto,
) -> Result<(), ApplicationError> {
    if command.campaign_session_id != session.id {
        return Err(ApplicationError::WrongCampaign);
    }
    if session.value.status != SessionStatus::Active {
        return Err(ApplicationError::CampaignCompleted);
    }
    if command.expected_campaign_revision != session.revision {
        return Err(ApplicationError::RevisionConflict {
            expected: command.expected_campaign_revision,
            current_revision: session.revision,
        });
    }
    if command.expected_encounter_revision != encounter.state.revision {
        return Err(ApplicationError::EncounterRevisionConflict {
            expected: command.expected_encounter_revision,
            current_revision: encounter.state.revision,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DieCursorSpan {
    sides: u16,
    value: u16,
    cursor_before: u64,
    cursor_after: u64,
}

struct EncounterRngAdapter {
    rng: DeterministicRng,
    spans: Vec<DieCursorSpan>,
    error: Option<RollError>,
}

impl EncounterRngAdapter {
    fn new(rng: DeterministicRng) -> Self {
        Self {
            rng,
            spans: Vec::new(),
            error: None,
        }
    }
}

impl EncounterRollSource for EncounterRngAdapter {
    fn roll_die(&mut self, sides: u16) -> u16 {
        if self.error.is_some() {
            return 0;
        }
        let cursor_before = self.rng.cursor();
        match self.rng.roll_die(u32::from(sides)) {
            Ok(value) => {
                let value = match u16::try_from(value) {
                    Ok(value) => value,
                    Err(_) => {
                        self.error = Some(RollError::InvalidRollRecord {
                            reason: "encounter die value does not fit its durable representation",
                        });
                        return 0;
                    }
                };
                self.spans.push(DieCursorSpan {
                    sides,
                    value,
                    cursor_before,
                    cursor_after: self.rng.cursor(),
                });
                value
            }
            Err(error) => {
                self.error = Some(error);
                0
            }
        }
    }
}

fn canonical_roll_records(
    campaign_session_id: &str,
    event_sequence: u64,
    seed_reference: &str,
    resolution: &EncounterResolution,
    spans: &[DieCursorSpan],
) -> Result<Vec<RollRecord>, ApplicationError> {
    let mut next_span = 0_usize;
    let mut records = Vec::with_capacity(resolution.rolls.len());
    for raw in &resolution.rolls {
        let end_span = next_span
            .checked_add(raw.individual_dice.len())
            .ok_or(ApplicationError::InvalidStoredState)?;
        let roll_spans = spans
            .get(next_span..end_span)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let Some(first_span) = roll_spans.first() else {
            return Err(ApplicationError::InvalidStoredState);
        };
        let Some(last_span) = roll_spans.last() else {
            return Err(ApplicationError::InvalidStoredState);
        };
        for (die, span) in raw.individual_dice.iter().zip(roll_spans) {
            if die.sides != span.sides || die.value != span.value {
                return Err(ApplicationError::InvalidStoredState);
            }
        }

        let expression = raw
            .expression
            .parse::<DiceExpression>()
            .map_err(ApplicationError::Roll)?;
        let rolled_dice = raw
            .individual_dice
            .iter()
            .map(|die| u32::from(die.value))
            .collect::<Vec<_>>();
        let kept_dice = raw
            .kept_die_indices
            .iter()
            .map(|index| u32::from(raw.individual_dice[usize::from(*index)].value))
            .collect::<Vec<_>>();
        let roll = DiceRoll {
            expression,
            rolled_dice,
            kept_dice,
            total: raw.total,
            roll_mode: match raw.mode {
                EncounterRollMode::Normal => RollMode::Normal,
                EncounterRollMode::Advantage => RollMode::Advantage,
                EncounterRollMode::Disadvantage => RollMode::Disadvantage,
            },
            cursor_before: first_span.cursor_before,
            cursor_after: last_span.cursor_after,
        };
        let metadata = RollMetadata {
            roll_id: format!(
                "roll:{campaign_session_id}:{event_sequence}:{}",
                raw.sequence
            ),
            purpose: encounter_roll_purpose_id(raw.purpose).to_owned(),
            actor_id: raw.actor_id.clone(),
            target_id: raw.target_id.clone(),
            ruleset: RULESET,
            seed_reference: seed_reference.to_owned(),
        };
        let modifiers = raw
            .modifiers
            .iter()
            .map(|modifier| ModifierComponent {
                name: modifier.source_id.clone(),
                value: i32::from(modifier.value),
            })
            .collect();
        records.push(
            RollRecord::from_roll(roll, metadata, modifiers).map_err(ApplicationError::Roll)?,
        );
        next_span = end_span;
    }
    if next_span != spans.len() {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(records)
}

const fn encounter_roll_purpose_id(purpose: EncounterRollPurpose) -> &'static str {
    match purpose {
        EncounterRollPurpose::Initiative => "encounter:initiative",
        EncounterRollPurpose::Attack => "encounter:attack",
        EncounterRollPurpose::Damage => "encounter:damage",
        EncounterRollPurpose::Healing => "encounter:healing",
        EncounterRollPurpose::SleepHitPoints => "encounter:sleep-hit-points",
        EncounterRollPurpose::HitDie => "encounter:hit-die",
        EncounterRollPurpose::DeathSave => "encounter:death-save",
    }
}

fn map_encounter_error(error: EncounterError) -> ApplicationError {
    match error {
        EncounterError::PlayerControlUnavailable { .. } => ApplicationError::NotPlayerTurn,
        EncounterError::DeterministicPolicyUnavailable { .. } => {
            ApplicationError::NpcTurnUnavailable
        }
        EncounterError::RevisionConflict { expected, actual } => {
            ApplicationError::EncounterRevisionConflict {
                expected,
                current_revision: actual,
            }
        }
        error @ (EncounterError::InvalidCommand { .. }
        | EncounterError::WrongEncounter { .. }
        | EncounterError::IllegalIntent { .. }
        | EncounterError::AttackUnavailable { .. }
        | EncounterError::InvalidTarget { .. }
        | EncounterError::TargetOutOfRange { .. }
        | EncounterError::InvalidDestination { .. }
        | EncounterError::InsufficientMovement { .. }) => {
            ApplicationError::InvalidEncounterCommand(error)
        }
        error @ (EncounterError::InvalidState { .. }
        | EncounterError::RevisionOverflow
        | EncounterError::InvalidRoll { .. }
        | EncounterError::RoundOverflow
        | EncounterError::InvalidCorrection { .. }) => ApplicationError::EncounterRules(error),
    }
}

fn validate_local_command(
    command: &AttemptExplorationCheckCommand,
    session: &StoredDocument<SessionDto>,
    character: &StoredDocument<Character>,
) -> Result<(), ApplicationError> {
    if command.campaign_session_id != session.id {
        return Err(ApplicationError::WrongCampaign);
    }
    if command.character_id != character.id {
        return Err(ApplicationError::WrongCharacter);
    }
    if authored_exploration_check(&command.action_id, Proficiency::Proficient).is_none() {
        return Err(ApplicationError::UnknownAction(command.action_id.clone()));
    }
    if session.value.status != SessionStatus::Active {
        return Err(ApplicationError::CampaignCompleted);
    }
    if command.expected_revision != session.revision {
        return Err(ApplicationError::RevisionConflict {
            expected: command.expected_revision,
            current_revision: session.revision,
        });
    }
    Ok(())
}

fn validate_social_command(
    command: &AttemptSocialInteractionCommand,
    session: &StoredDocument<SessionDto>,
    character: &StoredDocument<Character>,
) -> Result<(), ApplicationError> {
    if command.campaign_session_id != session.id {
        return Err(ApplicationError::WrongCampaign);
    }
    if command.character_id != character.id {
        return Err(ApplicationError::WrongCharacter);
    }
    if command.action_id != LOCAL_SOCIAL_ACTION_ID {
        return Err(ApplicationError::UnknownAction(command.action_id.clone()));
    }
    if session.value.status != SessionStatus::Active {
        return Err(ApplicationError::CampaignCompleted);
    }
    if command.expected_revision != session.revision {
        return Err(ApplicationError::RevisionConflict {
            expected: command.expected_revision,
            current_revision: session.revision,
        });
    }
    Ok(())
}

fn validate_local_documents(
    session: &StoredDocument<SessionDto>,
    character: &StoredDocument<Character>,
) -> Result<(), ApplicationError> {
    let expected_revision = session
        .value
        .last_event_sequence
        .checked_add(1)
        .ok_or(ApplicationError::InvalidStoredState)?;
    if session.id != LOCAL_CAMPAIGN_SESSION_ID
        || character.id != LOCAL_CHARACTER_ID
        || session.value.character_ids.as_slice() != [LOCAL_CHARACTER_ID]
        || session.revision != expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(())
}

fn authored_exploration_check(
    action_id: &str,
    perception_proficiency: Proficiency,
) -> Option<AbilityCheck> {
    match action_id {
        LOCAL_EXPLORATION_ACTION_ID => Some(AbilityCheck {
            ability: Ability::Wisdom,
            proficiency: perception_proficiency,
            difficulty_class: 13,
            situational_modifier: 0,
            roll_context: RollContext::normal(),
        }),
        _ => None,
    }
}

fn initial_social_state() -> ExplorationSocialState {
    ExplorationSocialState {
        schema_version: RULES_MATRIX_SCHEMA_VERSION,
        turn: 1,
        objectives: vec![ObjectiveProgress {
            objective_id: SOCIAL_OBJECTIVE_ID.to_owned(),
            progress: 0,
            target: 1,
            status: ProgressStatus::Active,
        }],
        clocks: vec![SceneClock {
            clock_id: SOCIAL_CLOCK_ID.to_owned(),
            kind: ClockKind::Threat,
            filled: 0,
            segments: 4,
        }],
        npcs: vec![NpcSocialState {
            npc_id: SOCIAL_NPC_ID.to_owned(),
            attitude: NpcAttitude::Indifferent,
        }],
    }
}

fn resolve_authored_social_state(
    check: &TrustedCheckResolution,
) -> Result<(ExplorationSocialState, Vec<ExplorationSocialFact>), ApplicationError> {
    check
        .validate()
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    if check.difficulty.band != CheckDifficulty::Moderate
        || check.difficulty.stakes != HighStakesKind::None
        || check.result.ability != Ability::Charisma
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    let mut state = initial_social_state();
    let mut facts = Vec::with_capacity(4);
    facts.push(
        apply_exploration_social_command(
            &mut state,
            &ExplorationSocialCommand::AdvanceClock {
                clock_id: SOCIAL_CLOCK_ID.to_owned(),
                amount: 1,
            },
        )
        .map_err(|_| ApplicationError::InvalidStoredState)?,
    );
    if check.result.outcome == D20TestOutcome::Success {
        facts.push(
            apply_exploration_social_command(
                &mut state,
                &ExplorationSocialCommand::AdvanceObjective {
                    objective_id: SOCIAL_OBJECTIVE_ID.to_owned(),
                    amount: 1,
                },
            )
            .map_err(|_| ApplicationError::InvalidStoredState)?,
        );
        facts.push(
            apply_exploration_social_command(
                &mut state,
                &ExplorationSocialCommand::ShiftNpcAttitude {
                    npc_id: SOCIAL_NPC_ID.to_owned(),
                    shift: AttitudeShift::OneStepBetter,
                },
            )
            .map_err(|_| ApplicationError::InvalidStoredState)?,
        );
    } else {
        facts.push(
            apply_exploration_social_command(
                &mut state,
                &ExplorationSocialCommand::FailObjective {
                    objective_id: SOCIAL_OBJECTIVE_ID.to_owned(),
                },
            )
            .map_err(|_| ApplicationError::InvalidStoredState)?,
        );
        facts.push(
            apply_exploration_social_command(
                &mut state,
                &ExplorationSocialCommand::ShiftNpcAttitude {
                    npc_id: SOCIAL_NPC_ID.to_owned(),
                    shift: AttitudeShift::OneStepWorse,
                },
            )
            .map_err(|_| ApplicationError::InvalidStoredState)?,
        );
    }
    facts.push(
        apply_exploration_social_command(&mut state, &ExplorationSocialCommand::EndTurn)
            .map_err(|_| ApplicationError::InvalidStoredState)?,
    );
    Ok((state, facts))
}

fn validate_authored_social_outcome(
    outcome: &SocialInteractionOutcomeDto,
) -> Result<(), ApplicationError> {
    outcome
        .validate()
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    if outcome.action_id != LOCAL_SOCIAL_ACTION_ID {
        return Err(ApplicationError::InvalidStoredState);
    }
    let (state, facts) = resolve_authored_social_state(&outcome.check)?;
    if outcome.resulting_state != state || outcome.facts != facts {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(())
}

fn social_actor(
    legacy: &Character,
    authoritative: Option<&HeroCharacter>,
) -> Result<(AbilityScores, Level, Proficiency), ApplicationError> {
    let (scores, level, _) = exploration_actor(legacy, authoritative)?;
    let proficiency = authoritative
        .and_then(|hero| {
            hero.sheet
                .skills
                .iter()
                .find(|skill| skill.skill == SkillId::Persuasion)
        })
        .map_or(Proficiency::Proficient, |skill| {
            if skill.proficient {
                Proficiency::Proficient
            } else {
                Proficiency::None
            }
        });
    Ok((scores, level, proficiency))
}

fn exploration_actor(
    legacy: &Character,
    authoritative: Option<&HeroCharacter>,
) -> Result<(AbilityScores, Level, Proficiency), ApplicationError> {
    let Some(hero) = authoritative else {
        return Ok((
            legacy.ability_scores().clone(),
            legacy.level(),
            Proficiency::Proficient,
        ));
    };
    validate_local_authoritative_hero(hero)?;
    let perception = hero
        .sheet
        .skills
        .iter()
        .find(|skill| skill.skill == SkillId::Perception)
        .ok_or(ApplicationError::InvalidStoredState)?;
    let proficiency = if perception.proficient {
        Proficiency::Proficient
    } else {
        Proficiency::None
    };
    let level = Level::new(hero.level.value()).map_err(ApplicationError::InvalidOutcome)?;
    Ok((hero.sheet.ability_scores.clone(), level, proficiency))
}

fn encounter_profile_from_hero(
    hero: &HeroCharacter,
) -> Result<EncounterHeroProfile, ApplicationError> {
    validate_local_authoritative_hero(hero)?;
    let proficiency = i16::from(hero.sheet.proficiency_bonus);
    let attacks = hero
        .sheet
        .attacks
        .iter()
        .map(|attack| {
            if attack.damage.count != 1 {
                return Err(ApplicationError::InvalidStoredState);
            }
            let ability_modifier = i16::from(hero.sheet.ability_modifiers.get(attack.ability));
            if ability_modifier.checked_add(proficiency) != Some(i16::from(attack.attack_bonus)) {
                return Err(ApplicationError::InvalidStoredState);
            }
            let damage_type = match attack.damage_type {
                HeroDamageType::Bludgeoning => EncounterDamageType::Bludgeoning,
                HeroDamageType::Piercing => EncounterDamageType::Piercing,
                HeroDamageType::Slashing => EncounterDamageType::Slashing,
                HeroDamageType::Fire | HeroDamageType::Force => {
                    return Err(ApplicationError::InvalidStoredState);
                }
            };
            Ok(EncounterAttack {
                attack_id: attack.attack_id.clone(),
                range_feet: attack.normal_range_feet,
                attack_modifiers: vec![
                    RollModifierFact {
                        source_id: hero_ability_modifier_id(attack.ability).to_owned(),
                        value: ability_modifier,
                    },
                    RollModifierFact {
                        source_id: "srd-5.1-cc:modifier:proficiency".to_owned(),
                        value: proficiency,
                    },
                ],
                damage_die_sides: u16::from(attack.damage.sides),
                damage_modifier: RollModifierFact {
                    source_id: "manchester-arcana:modifier:derived-weapon-damage".to_owned(),
                    value: i16::from(attack.damage.constant),
                },
                damage_type,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let class = hero.choices.class.class();
    let runtime_resources = RuntimeResources::from_derived_sheet(class, &hero.sheet)
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    let spellcasting = match class {
        HeroClass::Fighter => None,
        HeroClass::Wizard => Some(
            SpellcastingState::from_derived_sheet(CANAL_WARDEN_ID, &hero.sheet)
                .map_err(|_| ApplicationError::InvalidStoredState)?,
        ),
    };
    let profile = EncounterHeroProfile {
        source_character_id: hero.character_id.clone(),
        name: hero.choices.presentation.name.clone(),
        armor_class: u16::from(hero.sheet.armor_class),
        speed_feet: u16::from(hero.sheet.speed_feet),
        initiative_modifier: hero.sheet.ability_modifiers.dexterity,
        current_hit_points: hero.sheet.current_hit_points,
        maximum_hit_points: hero.sheet.maximum_hit_points,
        attacks,
        rules: Some(EncounterHeroRulesProfile {
            runtime_resources,
            spellcasting,
            constitution_modifier: Some(hero.sheet.ability_modifiers.constitution),
        }),
    };
    profile
        .validate()
        .map_err(|_| ApplicationError::InvalidStoredState)?;
    Ok(profile)
}

fn ensure_hero_runtime_matches_encounter(
    hero: &HeroCharacter,
    encounter: &EncounterState,
) -> Result<(), ApplicationError> {
    let profile = encounter_profile_from_hero(hero)?;
    let projected = encounter
        .hero_profile()
        .ok_or(ApplicationError::InvalidStoredState)?;
    if projected != profile {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(())
}

fn synchronize_hero_after_encounter(
    stored: &StoredDocument<HeroCharacter>,
    encounter: &EncounterState,
) -> Result<(Option<HeroCharacter>, Option<u64>), ApplicationError> {
    if encounter.hero.source_character_id.as_deref() != Some(stored.value.character_id.as_str()) {
        return Err(ApplicationError::InvalidStoredState);
    }
    let runtime = encounter
        .hero_rules
        .as_ref()
        .ok_or(ApplicationError::InvalidStoredState)?
        .runtime_resources
        .clone();
    let resource_currents = runtime_resource_currents(&runtime);
    let unchanged = stored.value.sheet.current_hit_points == encounter.hero.hit_points.current
        && stored.value.sheet.resources.len() == resource_currents.len()
        && resource_currents.iter().all(|(kind, current)| {
            stored
                .value
                .sheet
                .resources
                .iter()
                .any(|pool| pool.resource == *kind && pool.current == *current)
        });
    let mut candidate = stored.value.clone();
    if !unchanged {
        candidate
            .synchronize_encounter_runtime(encounter.hero.hit_points.current, &resource_currents)
            .map_err(ApplicationError::Hero)?;
    }
    let revision = candidate.revision;
    Ok((Some(candidate), Some(revision)))
}

fn runtime_resource_currents(runtime: &RuntimeResources) -> Vec<(ResourceKind, u8)> {
    [
        Some(&runtime.hit_dice),
        runtime.second_wind.as_ref(),
        runtime.action_surge.as_ref(),
        runtime.level_one_spell_slots.as_ref(),
        runtime.arcane_recovery.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|counter| (counter.kind, counter.current))
    .collect()
}

const fn hero_ability_modifier_id(ability: Ability) -> &'static str {
    match ability {
        Ability::Strength => "srd-5.1-cc:modifier:strength",
        Ability::Dexterity => "srd-5.1-cc:modifier:dexterity",
        Ability::Constitution => "srd-5.1-cc:modifier:constitution",
        Ability::Intelligence => "srd-5.1-cc:modifier:intelligence",
        Ability::Wisdom => "srd-5.1-cc:modifier:wisdom",
        Ability::Charisma => "srd-5.1-cc:modifier:charisma",
    }
}

fn validate_local_authoritative_hero(hero: &HeroCharacter) -> Result<(), ApplicationError> {
    hero.validate().map_err(ApplicationError::Hero)?;
    if hero.campaign_id != LOCAL_CAMPAIGN_SESSION_ID || hero.owner_id != LOCAL_HERO_OWNER_KEY {
        return Err(ApplicationError::HeroNotFound);
    }
    Ok(())
}

fn local_session(now_unix_ms: u64) -> SessionDto {
    SessionDto {
        schema_version: SESSION_SCHEMA_VERSION,
        id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
        ruleset: RULESET,
        title: LOCAL_CAMPAIGN_TITLE.to_owned(),
        status: SessionStatus::Active,
        character_ids: vec![LOCAL_CHARACTER_ID.to_owned()],
        created_at_unix_ms: now_unix_ms,
        updated_at_unix_ms: now_unix_ms,
        last_event_sequence: 0,
    }
}

fn local_character() -> Result<Character, ApplicationError> {
    CharacterDraft {
        id: LOCAL_CHARACTER_ID.to_owned(),
        name: LOCAL_CHARACTER_NAME.to_owned(),
        theme: LOCAL_CHARACTER_THEME.to_owned(),
        ability_scores: AbilityScores::new(10, 12, 10, 10, 14, 10)
            .map_err(ApplicationError::InvalidOutcome)?,
        experience_points: 0,
        current_hit_points: 10,
        maximum_hit_points: 10,
    }
    .build()
    .map_err(ApplicationError::InvalidOutcome)
}

struct SystemDice;

impl DiceSource for SystemDice {
    fn roll(&mut self, sides: u16) -> u16 {
        rand::rng().random_range(1..=sides)
    }
}

struct DynamicDice<'a>(&'a mut (dyn DiceSource + Send));

impl DiceSource for DynamicDice<'_> {
    fn roll(&mut self, sides: u16) -> u16 {
        self.0.roll(sides)
    }
}

struct SystemClock;

impl UnixTimeSource for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use manchester_dnd_core::encounter::{EncounterCommand, EncounterIntent, EncounterStatus};
    use sqlx::PgPool;

    use super::*;

    async fn test_service(
        access_mode: AccessMode,
        pool: PgPool,
    ) -> (GameApplicationService, PostgresRepository, Arc<AtomicUsize>) {
        test_service_with_roll(access_mode, pool, 12).await
    }

    async fn test_service_with_roll(
        access_mode: AccessMode,
        pool: PgPool,
        roll: u16,
    ) -> (GameApplicationService, PostgresRepository, Arc<AtomicUsize>) {
        let repository = PostgresRepository::from_pool(pool);
        let roll_count = Arc::new(AtomicUsize::new(0));
        let observed_rolls = roll_count.clone();
        let dice = move |sides| {
            assert_eq!(sides, 20);
            observed_rolls.fetch_add(1, Ordering::SeqCst);
            roll
        };
        let service = GameApplicationService::with_sources(
            access_mode,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            dice,
            || 1_000,
        );
        if access_mode == AccessMode::LocalSingleUser {
            service.load_local_campaign().await.unwrap();
            let evidence = SealedCampaignPins {
                seal_reason: CampaignPinSealReason::SelectedTheme,
                pins: service
                    .campaign_pins
                    .pins_for_theme(ThemeId::RainboundBorough)
                    .unwrap(),
                legacy_source: None,
            };
            repository
                .seal_campaign_pins_for_test(LOCAL_CAMPAIGN_SESSION_ID, &evidence)
                .await
                .unwrap();
        }
        (service, repository, roll_count)
    }

    fn command(view: &LocalCampaignViewDto, key: &str) -> AttemptExplorationCheckCommand {
        AttemptExplorationCheckCommand {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            character_id: view.character_id.clone(),
            action_id: LOCAL_EXPLORATION_ACTION_ID.to_owned(),
            expected_revision: view.revision,
            idempotency_key: key.to_owned(),
        }
    }

    fn social_command(view: &LocalCampaignViewDto, key: &str) -> AttemptSocialInteractionCommand {
        AttemptSocialInteractionCommand {
            schema_version: SOCIAL_INTERACTION_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            character_id: view.character_id.clone(),
            action_id: LOCAL_SOCIAL_ACTION_ID.to_owned(),
            expected_revision: view.revision,
            idempotency_key: key.to_owned(),
        }
    }

    fn encounter_command(
        view: &LocalCampaignViewDto,
        key: &str,
        intent: EncounterIntent,
    ) -> CommitEncounterCommand {
        let encounter = view
            .encounter
            .as_ref()
            .expect("exploration should expose the encounter");
        CommitEncounterCommand {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            expected_campaign_revision: view.revision,
            command: EncounterCommand::new(encounter.state.revision, key, intent),
        }
    }

    fn npc_advance_command(view: &LocalCampaignViewDto, key: &str) -> AdvanceNpcTurnCommand {
        let encounter = view
            .encounter
            .as_ref()
            .expect("exploration should expose the encounter");
        AdvanceNpcTurnCommand {
            schema_version: manchester_dnd_core::ADVANCE_NPC_TURN_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            expected_campaign_revision: view.revision,
            expected_encounter_revision: encounter.state.revision,
            idempotency_key: key.to_owned(),
        }
    }

    async fn advance_to_npc_turn(
        service: &GameApplicationService,
        ready: &LocalCampaignViewDto,
        key_prefix: &str,
    ) -> LocalCampaignViewDto {
        service
            .commit_encounter_command(encounter_command(
                ready,
                &format!("{key_prefix}-start"),
                EncounterIntent::StartEncounter,
            ))
            .await
            .unwrap();
        let mut view = service.load_local_campaign().await.unwrap();
        let encounter = view.encounter.as_ref().unwrap();
        if encounter.state.current_actor_id.as_deref() == Some(encounter.state.hero.id.as_str()) {
            service
                .commit_encounter_command(encounter_command(
                    &view,
                    &format!("{key_prefix}-end-hero"),
                    EncounterIntent::EndTurn,
                ))
                .await
                .unwrap();
            view = service.load_local_campaign().await.unwrap();
        }
        let encounter = view.encounter.as_ref().unwrap();
        assert_eq!(
            encounter.state.current_actor_id.as_deref(),
            Some(encounter.state.creature.id.as_str())
        );
        assert!(encounter.legal_actions.is_empty());
        view
    }

    async fn resolve_exploration(service: &GameApplicationService) -> LocalCampaignViewDto {
        let initial = service.load_local_campaign().await.unwrap();
        assert!(initial.encounter.is_none());
        service
            .attempt_exploration_check(command(&initial, "check-before-encounter"))
            .await
            .unwrap();
        service.load_local_campaign().await.unwrap()
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn local_campaign_is_created_once_and_resumed(pool: PgPool) {
        let (service, repository, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let first = service.load_local_campaign().await.unwrap();
        let resumed = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |_| 20,
            || 2_000,
        )
        .load_local_campaign()
        .await
        .unwrap();

        assert_eq!(first, resumed);
        assert_eq!(first.revision, 1);
        assert_eq!(first.last_event_sequence, 0);
        let character = repository
            .load_character(LOCAL_CHARACTER_ID)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(character.value.level().value(), 1);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn unsealed_creator_scaffold_exposes_status_but_blocks_gameplay(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool);
        let service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |_| 12,
            || 1_000,
        );
        let view = service.load_local_campaign().await.unwrap();
        assert_eq!(
            view.content_pins,
            CampaignPinStatusDto::UnsealedCreatorScaffold
        );
        assert!(matches!(
            service
                .attempt_exploration_check(command(&view, "blocked-before-theme"))
                .await,
            Err(ApplicationError::CampaignPinsUnsealed)
        ));
        assert!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn legacy_save_without_theme_evidence_seals_recorded_rainbound_default(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool.clone());
        let service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |_| 12,
            || 1_000,
        );
        let unsealed = service.load_local_campaign().await.unwrap();
        assert!(unsealed.content_pins.sealed().is_none());
        sqlx::query(
            "UPDATE campaign_sessions
             SET content_pin_legacy_eligible = TRUE
             WHERE id = $1",
        )
        .bind(LOCAL_CAMPAIGN_SESSION_ID)
        .execute(&pool)
        .await
        .unwrap();

        let migrated = service.load_local_campaign().await.unwrap();
        let evidence = migrated.content_pins.sealed().unwrap();
        assert_eq!(
            evidence.seal_reason,
            CampaignPinSealReason::LegacyDefaultRainbound
        );
        assert_eq!(evidence.pins.hero.theme_id, ThemeId::RainboundBorough);
        assert_eq!(
            repository
                .load_campaign_pins(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .unwrap()
                .evidence,
            *evidence
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn legacy_pre_release_digests_are_aliased_with_original_evidence(pool: PgPool) {
        use manchester_dnd_core::hero::{
            HeroPins, LEGACY_DEV_CORE_CONTENT_PACK_DIGEST, LEGACY_DEV_RAINBOUND_THEME_PACK_DIGEST,
        };

        let repository = PostgresRepository::from_pool(pool.clone());
        let service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository,
            Arc::new(SeedVault::from_key([7; 32])),
            |_| 12,
            || 1_000,
        );
        service.load_local_campaign().await.unwrap();
        let draft = service.start_local_hero_creation().await.unwrap();
        let mut legacy = HeroPins::mvp(ThemeId::RainboundBorough);
        legacy.core_content.digest =
            Sha256Digest::new(LEGACY_DEV_CORE_CONTENT_PACK_DIGEST).unwrap();
        legacy.theme.digest = Sha256Digest::new(LEGACY_DEV_RAINBOUND_THEME_PACK_DIGEST).unwrap();
        legacy.validate().unwrap();
        sqlx::query(
            "UPDATE hero_creation_drafts
             SET payload_json = jsonb_set(
                     jsonb_set(
                         jsonb_set(payload_json, '{pins}', $2::jsonb),
                         '{step}', to_jsonb('concept'::text)
                     ),
                     '{revision}', '1'::jsonb
                 ),
                 revision = 2
             WHERE id = $1",
        )
        .bind(&draft.draft_id)
        .bind(serde_json::to_string(&legacy).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE campaign_sessions
             SET content_pin_legacy_eligible = TRUE WHERE id = $1",
        )
        .bind(LOCAL_CAMPAIGN_SESSION_ID)
        .execute(&pool)
        .await
        .unwrap();

        let view = service.load_local_campaign().await.unwrap();
        let evidence = view.content_pins.sealed().unwrap();
        assert_eq!(
            evidence.seal_reason,
            CampaignPinSealReason::LegacyDigestAlias
        );
        assert_eq!(evidence.legacy_source.as_ref(), Some(&legacy));
        assert_eq!(
            evidence.pins.hero,
            manchester_dnd_core::hero::HeroPins::mvp(ThemeId::RainboundBorough)
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn active_catalog_fingerprint_drift_quarantines_resume(pool: PgPool) {
        let (service, _, _) = test_service(AccessMode::LocalSingleUser, pool.clone()).await;
        sqlx::query(
            "UPDATE campaign_content_pins
             SET payload_json = jsonb_set(
                 payload_json,
                 '{active_catalog_fingerprint}',
                 to_jsonb($2::text)
             )
             WHERE campaign_session_id = $1",
        )
        .bind(LOCAL_CAMPAIGN_SESSION_ID)
        .bind(format!("sha256:{}", "9".repeat(64)))
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::CampaignPinsQuarantined)
        ));
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn saved_pack_version_and_digest_drift_quarantine_resume(pool: PgPool) {
        use manchester_dnd_core::hero::{CORE_CONTENT_PACK_DIGEST, MVP_PACK_VERSION};

        let (service, _, _) = test_service(AccessMode::LocalSingleUser, pool.clone()).await;
        sqlx::query(
            "UPDATE campaign_content_pins
             SET payload_json = jsonb_set(
                 payload_json,
                 '{hero,core_content,version}',
                 to_jsonb('1.0.1'::text)
             )
             WHERE campaign_session_id = $1",
        )
        .bind(LOCAL_CAMPAIGN_SESSION_ID)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::CampaignPinsQuarantined)
        ));

        sqlx::query(
            "UPDATE campaign_content_pins
             SET payload_json = jsonb_set(
                 jsonb_set(
                     payload_json,
                     '{hero,core_content,version}',
                     to_jsonb($2::text)
                 ),
                 '{hero,core_content,digest}',
                 to_jsonb($3::text)
             )
             WHERE campaign_session_id = $1",
        )
        .bind(LOCAL_CAMPAIGN_SESSION_ID)
        .bind(MVP_PACK_VERSION)
        .bind(format!("sha256:{}", "8".repeat(64)))
        .execute(&pool)
        .await
        .unwrap();
        assert_ne!(
            CORE_CONTENT_PACK_DIGEST,
            format!("sha256:{}", "8".repeat(64))
        );
        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::CampaignPinsQuarantined)
        ));
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn social_failure_commits_trusted_state_and_resumes_without_rerolling(pool: PgPool) {
        let (service, repository, roll_count) =
            test_service(AccessMode::LocalSingleUser, pool).await;
        let initial = service.load_local_campaign().await.unwrap();
        let initial_social = initial.social.as_ref().unwrap();
        assert_eq!(initial_social.state.turn, 1);
        assert_eq!(initial_social.state.clocks[0].filled, 0);
        assert_eq!(
            initial_social.state.npcs[0].attitude,
            NpcAttitude::Indifferent
        );

        let outcome = service
            .attempt_social_interaction_with_correlation(
                social_command(&initial, "social-failure"),
                "request:social-failure",
            )
            .await
            .unwrap();

        assert_eq!(outcome.check.difficulty.band, CheckDifficulty::Moderate);
        assert_eq!(outcome.check.difficulty.difficulty_class, 15);
        assert_eq!(outcome.check.result.ability, Ability::Charisma);
        assert_eq!(outcome.check.result.roll.selected, 12);
        assert_eq!(outcome.check.result.total, 14);
        assert_eq!(outcome.check.result.outcome, D20TestOutcome::Failure);
        assert_eq!(outcome.result_revision, 2);
        assert_eq!(outcome.event_sequence, 1);
        assert_eq!(outcome.resulting_state.turn, 2);
        assert_eq!(outcome.resulting_state.clocks[0].filled, 1);
        assert_eq!(
            outcome.resulting_state.objectives[0].status,
            ProgressStatus::Failed
        );
        assert_eq!(
            outcome.resulting_state.npcs[0].attitude,
            NpcAttitude::Hostile
        );

        let resumed_service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |_| panic!("a saved social outcome must never reroll"),
            || 9_000,
        );
        let resumed = resumed_service.load_local_campaign().await.unwrap();
        let resumed_social = resumed.social.unwrap();
        assert_eq!(resumed_social.latest_outcome, Some(outcome.clone()));
        assert_eq!(resumed_social.state, outcome.resulting_state);
        assert!(resumed.latest_check.is_none());
        assert!(resumed.encounter.is_none());
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);

        let audits = repository
            .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap();
        assert_eq!(audits.len(), 1);
        assert_eq!(
            audits[0].correlation_id.as_deref(),
            Some("request:social-failure")
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn social_success_completes_objective_and_improves_attitude(pool: PgPool) {
        let (service, _, roll_count) =
            test_service_with_roll(AccessMode::LocalSingleUser, pool, 20).await;
        let initial = service.load_local_campaign().await.unwrap();

        let outcome = service
            .attempt_social_interaction(social_command(&initial, "social-success"))
            .await
            .unwrap();

        assert_eq!(outcome.check.result.total, 22);
        assert_eq!(outcome.check.result.outcome, D20TestOutcome::Success);
        assert_eq!(
            outcome.resulting_state.objectives[0].status,
            ProgressStatus::Completed
        );
        assert_eq!(outcome.resulting_state.objectives[0].progress, 1);
        assert_eq!(
            outcome.resulting_state.npcs[0].attitude,
            NpcAttitude::Friendly
        );
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn concurrent_social_duplicates_share_one_roll_event_and_receipt(pool: PgPool) {
        let (service, repository, roll_count) =
            test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let request = social_command(&view, "social-concurrent");

        let (first, second) = tokio::join!(
            service.attempt_social_interaction(request.clone()),
            service.attempt_social_interaction(request.clone()),
        );

        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            repository
                .load_command_receipt(LOCAL_CAMPAIGN_SESSION_ID, &request.idempotency_key)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn invalid_and_stale_social_intents_are_rejected_before_dice(pool: PgPool) {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();

        let mut wrong_action = social_command(&view, "social-wrong-action");
        wrong_action.action_id = "choose-the-difficulty".to_owned();
        assert!(matches!(
            service.attempt_social_interaction(wrong_action).await,
            Err(ApplicationError::UnknownAction(_))
        ));
        let mut wrong_character = social_command(&view, "social-wrong-character");
        wrong_character.character_id = "other-hero".to_owned();
        assert!(matches!(
            service.attempt_social_interaction(wrong_character).await,
            Err(ApplicationError::WrongCharacter)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 0);

        service
            .attempt_social_interaction(social_command(&view, "social-first"))
            .await
            .unwrap();
        let error = service
            .attempt_social_interaction(social_command(&view, "social-stale"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ApplicationError::RevisionConflict {
                expected: 1,
                current_revision: 2
            }
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn deterministic_check_commits_and_reloads_without_rerolling(pool: PgPool) {
        let (service, repository, roll_count) =
            test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let outcome = service
            .attempt_exploration_check_with_correlation(
                command(&view, "check-1"),
                "request:test-correlation",
            )
            .await
            .unwrap();

        assert_eq!(outcome.result.roll.selected, 12);
        assert_eq!(outcome.result.ability, Ability::Wisdom);
        assert_eq!(outcome.result.ability_modifier, 2);
        assert_eq!(outcome.result.proficiency_modifier, 2);
        assert_eq!(outcome.result.difficulty_class, 13);
        assert_eq!(outcome.result.total, 16);
        assert!(outcome.result.success);
        assert_eq!(outcome.result_revision, 2);
        assert_eq!(outcome.event_sequence, 1);

        let reloaded = service.load_local_campaign().await.unwrap();
        assert_eq!(reloaded.latest_check, Some(outcome));
        assert_eq!(
            reloaded
                .encounter
                .as_ref()
                .unwrap()
                .state
                .opening_consequence,
            OpeningConsequence::RunesUnderstood
        );
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
        let audits = repository
            .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap();
        assert_eq!(audits.len(), 1);
        assert_eq!(
            audits[0].correlation_id.as_deref(),
            Some("request:test-correlation")
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn failed_latest_exploration_check_selects_the_misread_opening(pool: PgPool) {
        let repository = PostgresRepository::from_pool(pool);
        let service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |sides| {
                assert_eq!(sides, 20);
                1
            },
            || 1_000,
        );
        service.load_local_campaign().await.unwrap();
        repository
            .seal_campaign_pins_for_test(
                LOCAL_CAMPAIGN_SESSION_ID,
                &SealedCampaignPins {
                    seal_reason: CampaignPinSealReason::SelectedTheme,
                    pins: service
                        .campaign_pins
                        .pins_for_theme(ThemeId::RainboundBorough)
                        .unwrap(),
                    legacy_source: None,
                },
            )
            .await
            .unwrap();
        let initial = service.load_local_campaign().await.unwrap();
        let outcome = service
            .attempt_exploration_check(command(&initial, "failed-rune-check"))
            .await
            .unwrap();
        assert!(!outcome.result.success);

        let encounter = service
            .load_local_campaign()
            .await
            .unwrap()
            .encounter
            .unwrap();
        assert_eq!(
            encounter.state.opening_consequence,
            OpeningConsequence::RunesMisread
        );
        assert_eq!(encounter.state.hero.hit_points.temporary, 0);
        assert_eq!(encounter.state.creature.hit_points.temporary, 4);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn matching_receipt_replays_before_revision_validation_without_rerolling(pool: PgPool) {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let request = command(&view, "check-replay");

        let first = service
            .attempt_exploration_check(request.clone())
            .await
            .unwrap();
        let replay = service.attempt_exploration_check(request).await.unwrap();

        assert_eq!(replay, first);
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn encounter_start_commits_canonical_rolls_and_reloads_equivalently(pool: PgPool) {
        let (service, repository, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let ready = resolve_exploration(&service).await;
        let request = encounter_command(&ready, "encounter-start", EncounterIntent::StartEncounter);

        let outcome = service
            .commit_encounter_command_with_correlation(
                request,
                "request:encounter-start-correlation",
            )
            .await
            .unwrap();

        assert_eq!(outcome.result_campaign_revision, 3);
        assert_eq!(outcome.event_sequence, 2);
        assert_eq!(outcome.resolution.previous_revision, 1);
        assert_eq!(outcome.resolution.result_revision, 2);
        assert_eq!(outcome.resolution.state.status, EncounterStatus::Active);
        assert_eq!(outcome.roll_records.len(), 2);
        assert_eq!(outcome.roll_records[0].cursor_before, 0);
        assert!(
            outcome
                .roll_records
                .windows(2)
                .all(|rolls| rolls[0].cursor_after == rolls[1].cursor_before)
        );
        for record in &outcome.roll_records {
            record.validate().unwrap();
        }
        let public_json = serde_json::to_value(&outcome).unwrap();
        assert!(public_json.pointer("/roll_records/0/seed").is_none());

        let resumed_service = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |_| 1,
            || 9_000,
        );
        let reloaded = resumed_service.load_local_campaign().await.unwrap();
        let encounter = reloaded.encounter.unwrap();
        assert_eq!(encounter.state, outcome.resolution.state);
        assert_eq!(encounter.legal_actions, outcome.legal_actions);
        assert_eq!(encounter.latest_outcome, Some(outcome));

        let audits = repository
            .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap();
        assert_eq!(audits.len(), 2);
        assert_eq!(
            audits[1].correlation_id.as_deref(),
            Some("request:encounter-start-correlation")
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn encounter_receipt_retry_is_exact_and_does_not_append_or_advance(pool: PgPool) {
        let (service, repository, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let ready = resolve_exploration(&service).await;
        let request = encounter_command(&ready, "encounter-retry", EncounterIntent::StartEncounter);

        let first = service
            .commit_encounter_command(request.clone())
            .await
            .unwrap();
        let replay = service.commit_encounter_command(request).await.unwrap();

        assert_eq!(replay, first);
        assert_eq!(replay.roll_records[0].cursor_before, 0);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn player_endpoint_rejects_every_client_selected_intent_on_the_creature_turn(
        pool: PgPool,
    ) {
        let (service, repository, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let ready = resolve_exploration(&service).await;
        let npc_turn = advance_to_npc_turn(&service, &ready, "controller-boundary").await;
        let before = npc_turn.encounter.as_ref().unwrap().state.clone();
        let event_count = repository
            .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap()
            .len();

        // End turn is legal for the active creature in the engine, which makes this a direct
        // controller-boundary check rather than an incidental invalid-action rejection.
        let error = service
            .commit_encounter_command(encounter_command(
                &npc_turn,
                "forged-client-creature-end-turn",
                EncounterIntent::EndTurn,
            ))
            .await
            .unwrap_err();
        assert!(matches!(error, ApplicationError::NotPlayerTurn));

        let reloaded = service.load_local_campaign().await.unwrap();
        assert_eq!(reloaded.encounter.unwrap().state, before);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            event_count
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn npc_policy_receipts_are_atomic_replayable_and_record_the_pinned_origin(pool: PgPool) {
        let (service, repository, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let ready = resolve_exploration(&service).await;
        let npc_turn = advance_to_npc_turn(&service, &ready, "npc-policy").await;
        let first_request = npc_advance_command(&npc_turn, "npc-policy-move");
        let expected_move =
            select_soot_wight_policy_intent(&npc_turn.encounter.as_ref().unwrap().state).unwrap();
        assert!(matches!(expected_move, EncounterIntent::Move { .. }));

        let moved = service
            .advance_npc_turn(first_request)
            .await
            .expect("the policy should move toward the hero");
        assert!(moved.legal_actions.is_empty());
        let adjacent = service.load_local_campaign().await.unwrap();
        let expected_attack =
            select_soot_wight_policy_intent(&adjacent.encounter.as_ref().unwrap().state).unwrap();
        assert!(matches!(expected_attack, EncounterIntent::Attack { .. }));

        let request = npc_advance_command(&adjacent, "npc-policy-concurrent-attack");
        let competing = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([7; 32])),
            |_| 1,
            || 2_000,
        );
        let before_count = repository
            .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap()
            .len();
        let (left, right) = tokio::join!(
            service.advance_npc_turn(request.clone()),
            competing.advance_npc_turn(request.clone()),
        );
        let committed = left.unwrap();
        assert_eq!(right.unwrap(), committed);
        assert!(!committed.roll_records.is_empty());
        assert!(committed.legal_actions.is_empty());
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            before_count + 1
        );

        let replay = service.advance_npc_turn(request.clone()).await.unwrap();
        assert_eq!(replay, committed);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            before_count + 1
        );
        let receipt = repository
            .load_command_receipt(LOCAL_CAMPAIGN_SESSION_ID, &request.idempotency_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.command_kind, NPC_ADVANCE_COMMAND_KIND);

        let audits = repository
            .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap();
        let SessionEventPayload::EncounterResolved {
            command,
            command_origin,
            ..
        } = &audits.last().unwrap().payload.payload
        else {
            panic!("last audit must be the policy attack")
        };
        assert_eq!(command.command.intent, expected_attack);
        assert_eq!(
            command_origin,
            &EncounterCommandOrigin::DeterministicPolicy {
                policy_id: SOOT_WIGHT_POLICY_ID.to_owned(),
            }
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn stale_campaign_revision_does_not_spend_the_first_encounter_cursor(pool: PgPool) {
        let (service, _, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let ready = resolve_exploration(&service).await;
        let mut stale =
            encounter_command(&ready, "encounter-stale", EncounterIntent::StartEncounter);
        stale.expected_campaign_revision = 1;

        let error = service.commit_encounter_command(stale).await.unwrap_err();
        assert!(matches!(
            error,
            ApplicationError::RevisionConflict {
                expected: 1,
                current_revision: 2
            }
        ));

        let committed = service
            .commit_encounter_command(encounter_command(
                &ready,
                "encounter-after-stale",
                EncounterIntent::StartEncounter,
            ))
            .await
            .unwrap();
        assert_eq!(committed.roll_records[0].cursor_before, 0);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn stale_encounter_revision_is_separate_from_campaign_revision(pool: PgPool) {
        let (service, _, _) = test_service(AccessMode::LocalSingleUser, pool).await;
        let ready = resolve_exploration(&service).await;
        let mut stale = encounter_command(
            &ready,
            "encounter-state-stale",
            EncounterIntent::StartEncounter,
        );
        stale.command.expected_revision = 2;

        let error = service.commit_encounter_command(stale).await.unwrap_err();
        assert!(matches!(
            error,
            ApplicationError::EncounterRevisionConflict {
                expected: 2,
                current_revision: 1
            }
        ));
        assert_eq!(error.current_revision(), None);
        assert_eq!(error.current_encounter_revision(), Some(1));
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn concurrent_duplicate_requests_share_one_roll_and_commit(pool: PgPool) {
        let (service, repository, roll_count) =
            test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let request = command(&view, "check-concurrent");
        let first_service = service.clone();
        let second_service = service.clone();
        let first_request = request.clone();

        let (first, second) = tokio::join!(
            first_service.attempt_exploration_check(first_request),
            second_service.attempt_exploration_check(request),
        );

        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn stale_revision_is_rejected_before_dice_are_consumed(pool: PgPool) {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        service
            .attempt_exploration_check(command(&view, "check-first"))
            .await
            .unwrap();
        let error = service
            .attempt_exploration_check(command(&view, "check-stale"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ApplicationError::RevisionConflict {
                expected: 1,
                current_revision: 2
            }
        ));
        assert_eq!(error.current_revision(), Some(2));
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn idempotency_key_cannot_be_reused_for_a_different_command(pool: PgPool) {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let original = command(&view, "same-key");
        service
            .attempt_exploration_check(original.clone())
            .await
            .unwrap();
        let mut changed = original;
        changed.action_id = "another-action".to_owned();

        assert!(matches!(
            service.attempt_exploration_check(changed).await,
            Err(ApplicationError::IdempotencyConflict)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn unknown_action_and_wrong_character_are_rejected(pool: PgPool) {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let mut wrong_action = command(&view, "wrong-action");
        wrong_action.action_id = "search-somewhere-else".to_owned();
        assert!(matches!(
            service.attempt_exploration_check(wrong_action).await,
            Err(ApplicationError::UnknownAction(_))
        ));

        let mut wrong_character = command(&view, "wrong-character");
        wrong_character.character_id = "another-character".to_owned();
        assert!(matches!(
            service.attempt_exploration_check(wrong_character).await,
            Err(ApplicationError::WrongCharacter)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 0);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn completed_campaign_rejects_new_checks(pool: PgPool) {
        let (service, repository, roll_count) =
            test_service(AccessMode::LocalSingleUser, pool).await;
        let view = service.load_local_campaign().await.unwrap();
        let stored = repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap()
            .unwrap();
        let mut completed = stored.value;
        completed.status = SessionStatus::Completed;
        completed.last_event_sequence = 1;
        completed.updated_at_unix_ms = 1_001;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 1_001,
            actor: EventActor::System,
            payload: SessionEventPayload::SessionEnded,
        };
        repository
            .commit_session_event("test-end", &completed, 1, &event, &[])
            .await
            .unwrap();
        let mut request = command(&view, "after-end");
        request.expected_revision = 2;

        assert!(matches!(
            service.attempt_exploration_check(request).await,
            Err(ApplicationError::CampaignCompleted)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 0);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn hosted_mode_fails_closed_for_loads_and_mutations(pool: PgPool) {
        let (service, repository, _) = test_service(AccessMode::Hosted, pool).await;

        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        let request = AttemptExplorationCheckCommand {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            character_id: LOCAL_CHARACTER_ID.to_owned(),
            action_id: LOCAL_EXPLORATION_ACTION_ID.to_owned(),
            expected_revision: 1,
            idempotency_key: "hosted-denied".to_owned(),
        };
        assert!(matches!(
            service.attempt_exploration_check(request).await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        assert!(
            repository
                .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .is_none()
        );
    }
}
