//! Framework-independent rules and persistence types for Manchester Arcana.
//!
//! Randomness is always supplied by the caller through [`DiceSource`]. AI game
//! master output is represented by declarative [`AiGmProposal`] values; this
//! crate deliberately does not apply those proposals to game state.

mod ability;
mod action;
pub mod action_points;
mod ai;
pub mod ai_turn;
pub mod campaign_pins;
mod character;
mod check;
mod dice;
mod digest;
pub mod encounter;
mod error;
mod exploration;
pub mod hero;
mod identifier;
pub mod player_character;
mod proficiency;
mod progression;
mod roll;
pub mod rules_matrix;
mod ruleset;
mod session;

pub use ability::{Ability, AbilityScore, AbilityScores};
pub use action::{ActionEconomy, ActionKind, TurnResource};
pub use ai::{
    AI_PROPOSAL_SCHEMA_VERSION, AiGmProposal, CheckDifficulty, GeneratedNarrative, ProposedEffect,
    RewardTier,
};
pub use campaign_pins::{
    CAMPAIGN_PINS_SCHEMA_VERSION, CAMPAIGN_PROMPT_POLICY_ID, CAMPAIGN_PROMPT_TEMPLATE_ID,
    CONTENT_PACK_SCHEMA_ID, CampaignContentPins, CampaignPinSealReason, CampaignPinStatusDto,
    CampaignPromptPin, CampaignSchemaPins, SealedCampaignPins, TYPED_GM_REQUEST_SCHEMA_ID,
};
pub use character::{Character, CharacterDraft, ExperienceAwardSummary};
pub use check::{AbilityCheck, AbilityCheckResult, AttackOutcome, AttackRoll, AttackRollResult};
pub use dice::{D20Roll, DiceSource, RollContext, RollMode, resolve_d20};
pub use digest::Sha256Digest;
pub use error::{GameCoreError, Result};
pub use exploration::{
    ADVANCE_NPC_TURN_SCHEMA_VERSION, AdvanceNpcTurnCommand, AttemptExplorationCheckCommand,
    AttemptSocialInteractionCommand, CommitEncounterCommand, CommittedEncounterOutcomeDto,
    ENCOUNTER_COMMIT_SCHEMA_VERSION, EXPLORATION_CHECK_SCHEMA_VERSION, EncounterViewDto,
    ExplorationCheckOutcomeDto, LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION, LocalCampaignViewDto,
    SOCIAL_INTERACTION_SCHEMA_VERSION, SocialInteractionOutcomeDto, SocialSceneViewDto,
};
pub use identifier::{MAX_OPAQUE_ID_LEN, is_valid_opaque_id};
pub use player_character::{
    MAX_DISPLAY_NAME_LEN as MAX_PLAYER_CHARACTER_DISPLAY_NAME_LEN, PLAYER_CHARACTER_SCHEMA_VERSION,
    PlayerCharacter,
};
pub use proficiency::Proficiency;
pub use progression::{Level, XP_THRESHOLDS};
pub use roll::{
    CHACHA20_V1_ALGORITHM_ID, DeterministicRng, DiceExpression, DiceRoll, MAX_DICE_CONSTANT_ABS,
    MAX_DICE_COUNT, MAX_DICE_EXPRESSION_LEN, MAX_DIE_SIDES, MAX_MODIFIER_COMPONENTS,
    MAX_ROLL_ABSOLUTE_TOTAL, ModifierComponent, RollAlgorithm, RollError, RollMetadata, RollRecord,
    RollResult, RollSeed,
};
pub use ruleset::{RULESET, RulesetId};
pub use session::{
    EncounterCommandOrigin, EventActor, SESSION_SCHEMA_VERSION, SessionDto, SessionEventDto,
    SessionEventPayload, SessionStatus,
};
