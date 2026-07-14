//! Framework-independent rules and persistence types for Manchester Arcana.
//!
//! Randomness is always supplied by the caller through [`DiceSource`]. AI game
//! master output is represented by declarative [`AiGmProposal`] values; this
//! crate deliberately does not apply those proposals to game state.

mod ability;
mod action;
mod ai;
mod character;
mod check;
mod dice;
mod digest;
mod error;
mod exploration;
mod identifier;
mod proficiency;
mod progression;
mod ruleset;
mod session;

pub use ability::{Ability, AbilityScore, AbilityScores};
pub use action::{ActionEconomy, ActionKind, TurnResource};
pub use ai::{
    AI_PROPOSAL_SCHEMA_VERSION, AiGmProposal, CheckDifficulty, GeneratedNarrative, ProposedEffect,
    RewardTier,
};
pub use character::{Character, CharacterDraft, ExperienceAwardSummary};
pub use check::{AbilityCheck, AbilityCheckResult, AttackOutcome, AttackRoll, AttackRollResult};
pub use dice::{D20Roll, DiceSource, RollContext, RollMode, resolve_d20};
pub use digest::Sha256Digest;
pub use error::{GameCoreError, Result};
pub use exploration::{
    AttemptExplorationCheckCommand, EXPLORATION_CHECK_SCHEMA_VERSION, ExplorationCheckOutcomeDto,
    LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION, LocalCampaignViewDto,
};
pub use identifier::{MAX_OPAQUE_ID_LEN, is_valid_opaque_id};
pub use proficiency::Proficiency;
pub use progression::{Level, XP_THRESHOLDS};
pub use ruleset::{RULESET, RulesetId};
pub use session::{
    EventActor, SESSION_SCHEMA_VERSION, SessionDto, SessionEventDto, SessionEventPayload,
    SessionStatus,
};
