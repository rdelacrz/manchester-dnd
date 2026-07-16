//! Purpose-specific, inert AI turn proposals.
//!
//! These values can describe a candidate interpretation or presentation, but
//! they cannot mutate game state. Application code must validate them against a
//! [`ProposalAcceptanceContext`] and convert an accepted proposal into an
//! ordinary authoritative engine command.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CheckDifficulty, RewardTier, Sha256Digest, is_valid_opaque_id};

pub const TYPED_AI_PROPOSAL_SCHEMA_VERSION: u16 = 1;
pub const MAX_CLARIFICATION_CHOICES: usize = 4;
pub const MAX_NARRATION_FACTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypedProposalError {
    #[error("typed AI proposal is invalid: {reason}")]
    Invalid { reason: &'static str },
    #[error("typed AI proposal is stale")]
    Stale,
    #[error("typed AI proposal references an unavailable capability")]
    Unsupported,
    #[error("typed AI narration contradicts authoritative facts")]
    Contradiction,
}

pub type TypedProposalResult<T> = std::result::Result<T, TypedProposalError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalBase {
    pub schema_version: u16,
    pub proposal_id: String,
    pub session_id: String,
    pub based_on_revision: u64,
    pub based_on_event_sequence: u64,
    pub prompt_template_id: String,
    pub policy_id: String,
    /// Fingerprint of non-secret provider/model/prompt configuration.
    pub config_fingerprint: Sha256Digest,
}

impl ProposalBase {
    pub fn validate(&self) -> TypedProposalResult<()> {
        if self.schema_version != TYPED_AI_PROPOSAL_SCHEMA_VERSION {
            return invalid("schema version is unsupported");
        }
        if !is_valid_opaque_id(&self.proposal_id)
            || !is_valid_opaque_id(&self.session_id)
            || !is_valid_opaque_id(&self.prompt_template_id)
            || !is_valid_opaque_id(&self.policy_id)
            || self.based_on_revision == 0
        {
            return invalid("proposal identity, pins, or revision are invalid");
        }
        Ok(())
    }

    fn validate_against(&self, context: &ProposalAcceptanceContext) -> TypedProposalResult<()> {
        self.validate()?;
        context.validate()?;
        if self.session_id != context.session_id
            || self.based_on_revision != context.revision
            || self.based_on_event_sequence != context.event_sequence
        {
            return Err(TypedProposalError::Stale);
        }
        if self.prompt_template_id != context.prompt_template_id
            || self.policy_id != context.policy_id
            || self.config_fingerprint != context.config_fingerprint
        {
            return Err(TypedProposalError::Unsupported);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStakes {
    Reversible,
    ResourceLoss,
    Irreversible,
    CharacterDefeat,
}

impl CheckStakes {
    pub const fn requires_confirmation(self) -> bool {
        matches!(self, Self::Irreversible | Self::CharacterDefeat)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionProposal {
    pub base: ProposalBase,
    /// Must resolve to an action currently offered by the authoritative engine.
    pub action_id: String,
    pub target_id: Option<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckProposal {
    pub base: ProposalBase,
    /// Authored check/action ID; the application owns ability, skill, and modifiers.
    pub check_id: String,
    pub difficulty: CheckDifficulty,
    pub stakes: CheckStakes,
    pub rationale: String,
}

impl CheckProposal {
    pub const fn requires_confirmation(&self) -> bool {
        self.stakes.requires_confirmation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneProposal {
    pub base: ProposalBase,
    pub scene_id: String,
    pub objective_id: Option<String>,
    /// Trusted campaign policy may decline or map this tier; no XP amount is accepted.
    pub reward_tier: Option<RewardTier>,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MechanicalFact {
    Actor {
        actor_id: String,
    },
    Target {
        target_id: String,
    },
    RollTotal {
        roll_id: String,
        total: i32,
    },
    DifficultyClass {
        source_id: String,
        value: i16,
    },
    ArmorClass {
        target_id: String,
        value: i16,
    },
    Damage {
        target_id: String,
        amount: u16,
    },
    HitPoints {
        target_id: String,
        before: u16,
        after: u16,
    },
    Condition {
        target_id: String,
        condition_id: String,
    },
    Resource {
        actor_id: String,
        resource_id: String,
        remaining: u16,
    },
    Outcome {
        outcome_id: String,
    },
    Objective {
        objective_id: String,
        status_id: String,
    },
}

impl MechanicalFact {
    pub fn validate(&self) -> TypedProposalResult<()> {
        let ids: &[&str] = match self {
            Self::Actor { actor_id } => &[actor_id],
            Self::Target { target_id } => &[target_id],
            Self::RollTotal { roll_id, .. } => &[roll_id],
            Self::DifficultyClass { source_id, .. } => &[source_id],
            Self::ArmorClass { target_id, .. }
            | Self::Damage { target_id, .. }
            | Self::HitPoints { target_id, .. } => &[target_id],
            Self::Condition {
                target_id,
                condition_id,
            } => &[target_id, condition_id],
            Self::Resource {
                actor_id,
                resource_id,
                ..
            } => &[actor_id, resource_id],
            Self::Outcome { outcome_id } => &[outcome_id],
            Self::Objective {
                objective_id,
                status_id,
            } => &[objective_id, status_id],
        };
        if ids.iter().any(|id| !is_valid_opaque_id(id)) {
            return invalid("a narration fact identifier is invalid");
        }
        match self {
            Self::DifficultyClass { value, .. } | Self::ArmorClass { value, .. } if *value <= 0 => {
                invalid("AC and DC facts must be positive")
            }
            Self::HitPoints { before, after, .. } if after > before => {
                invalid("a damage narration HP fact cannot increase HP")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrationProposal {
    pub base: ProposalBase,
    pub narration_id: String,
    pub text: String,
    /// Closed structured claims extracted alongside the prose. Acceptance
    /// requires byte-independent set equality with committed facts.
    pub claimed_facts: Vec<MechanicalFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationChoice {
    pub choice_id: String,
    pub label: String,
    pub action_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationProposal {
    pub base: ProposalBase,
    pub question: String,
    pub choices: Vec<ClarificationChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedGmProposal {
    Action(ActionProposal),
    Check(CheckProposal),
    Scene(SceneProposal),
    Narration(NarrationProposal),
    Clarification(ClarificationProposal),
}

impl TypedGmProposal {
    pub fn base(&self) -> &ProposalBase {
        match self {
            Self::Action(value) => &value.base,
            Self::Check(value) => &value.base,
            Self::Scene(value) => &value.base,
            Self::Narration(value) => &value.base,
            Self::Clarification(value) => &value.base,
        }
    }

    pub fn validate_against(
        &self,
        context: &ProposalAcceptanceContext,
    ) -> TypedProposalResult<ProposalDisposition> {
        self.base().validate_against(context)?;
        match self {
            Self::Action(value) => {
                validate_text(&value.rationale, 1_000)?;
                if !is_valid_opaque_id(&value.action_id)
                    || !context.legal_action_ids.contains(&value.action_id)
                    || value.target_id.as_ref().is_some_and(|target| {
                        !is_valid_opaque_id(target) || !context.legal_target_ids.contains(target)
                    })
                {
                    return Err(TypedProposalError::Unsupported);
                }
                Ok(ProposalDisposition::ConvertToEngineCommand)
            }
            Self::Check(value) => {
                validate_text(&value.rationale, 1_000)?;
                if !is_valid_opaque_id(&value.check_id)
                    || !context.legal_check_ids.contains(&value.check_id)
                {
                    return Err(TypedProposalError::Unsupported);
                }
                if value.requires_confirmation() {
                    Ok(ProposalDisposition::RequirePlayerConfirmation)
                } else {
                    Ok(ProposalDisposition::ConvertToEngineCommand)
                }
            }
            Self::Scene(value) => {
                validate_text(&value.rationale, 1_000)?;
                if !is_valid_opaque_id(&value.scene_id)
                    || !context.legal_scene_ids.contains(&value.scene_id)
                    || value.objective_id.as_ref().is_some_and(|objective| {
                        !is_valid_opaque_id(objective)
                            || !context.legal_objective_ids.contains(objective)
                    })
                {
                    return Err(TypedProposalError::Unsupported);
                }
                Ok(ProposalDisposition::ConvertToEngineCommand)
            }
            Self::Narration(value) => {
                validate_text(&value.text, 12_000)?;
                if !is_valid_opaque_id(&value.narration_id)
                    || value.claimed_facts.len() > MAX_NARRATION_FACTS
                    || value
                        .claimed_facts
                        .iter()
                        .any(|fact| fact.validate().is_err())
                {
                    return invalid("narration identity, text, or fact bounds are invalid");
                }
                let claims = value.claimed_facts.iter().collect::<BTreeSet<_>>();
                if claims.len() != value.claimed_facts.len()
                    || claims != context.authoritative_facts.iter().collect()
                {
                    return Err(TypedProposalError::Contradiction);
                }
                Ok(ProposalDisposition::PresentationOnly)
            }
            Self::Clarification(value) => {
                validate_text(&value.question, 500)?;
                if value.choices.is_empty() || value.choices.len() > MAX_CLARIFICATION_CHOICES {
                    return invalid("clarification choice count is invalid");
                }
                let mut ids = BTreeSet::new();
                for choice in &value.choices {
                    validate_text(&choice.label, 300)?;
                    if !is_valid_opaque_id(&choice.choice_id)
                        || !ids.insert(&choice.choice_id)
                        || choice.action_id.as_ref().is_some_and(|action| {
                            !is_valid_opaque_id(action)
                                || !context.legal_action_ids.contains(action)
                        })
                    {
                        return Err(TypedProposalError::Unsupported);
                    }
                }
                Ok(ProposalDisposition::AskClarification)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalDisposition {
    ConvertToEngineCommand,
    RequirePlayerConfirmation,
    PresentationOnly,
    AskClarification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalAcceptanceContext {
    pub session_id: String,
    pub revision: u64,
    pub event_sequence: u64,
    pub prompt_template_id: String,
    pub policy_id: String,
    pub config_fingerprint: Sha256Digest,
    pub legal_action_ids: BTreeSet<String>,
    pub legal_check_ids: BTreeSet<String>,
    pub legal_target_ids: BTreeSet<String>,
    pub legal_scene_ids: BTreeSet<String>,
    pub legal_objective_ids: BTreeSet<String>,
    pub authoritative_facts: Vec<MechanicalFact>,
}

impl ProposalAcceptanceContext {
    pub fn validate(&self) -> TypedProposalResult<()> {
        if !is_valid_opaque_id(&self.session_id)
            || self.revision == 0
            || !is_valid_opaque_id(&self.prompt_template_id)
            || !is_valid_opaque_id(&self.policy_id)
        {
            return invalid("acceptance context identity or revision is invalid");
        }
        for ids in [
            &self.legal_action_ids,
            &self.legal_check_ids,
            &self.legal_target_ids,
            &self.legal_scene_ids,
            &self.legal_objective_ids,
        ] {
            if ids.len() > 256 || ids.iter().any(|id| !is_valid_opaque_id(id)) {
                return invalid("acceptance allowlist is invalid or too large");
            }
        }
        if self.authoritative_facts.len() > MAX_NARRATION_FACTS
            || self
                .authoritative_facts
                .iter()
                .any(|fact| fact.validate().is_err())
            || self
                .authoritative_facts
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.authoritative_facts.len()
        {
            return invalid("authoritative narration facts are invalid or duplicated");
        }
        Ok(())
    }
}

fn validate_text(value: &str, maximum_chars: usize) -> TypedProposalResult<()> {
    if value.trim().is_empty() || value.chars().count() > maximum_chars {
        invalid("proposal text is empty or too long")
    } else {
        Ok(())
    }
}

fn invalid<T>(reason: &'static str) -> TypedProposalResult<T> {
    Err(TypedProposalError::Invalid { reason })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn base() -> ProposalBase {
        ProposalBase {
            schema_version: TYPED_AI_PROPOSAL_SCHEMA_VERSION,
            proposal_id: "proposal:1".to_owned(),
            session_id: "session:1".to_owned(),
            based_on_revision: 3,
            based_on_event_sequence: 2,
            prompt_template_id: "prompt:gm-turn:v1".to_owned(),
            policy_id: "policy:private-mvp:v1".to_owned(),
            config_fingerprint: Sha256Digest::from_bytes([7; 32]),
        }
    }

    fn context() -> ProposalAcceptanceContext {
        ProposalAcceptanceContext {
            session_id: "session:1".to_owned(),
            revision: 3,
            event_sequence: 2,
            prompt_template_id: "prompt:gm-turn:v1".to_owned(),
            policy_id: "policy:private-mvp:v1".to_owned(),
            config_fingerprint: Sha256Digest::from_bytes([7; 32]),
            legal_action_ids: BTreeSet::from(["action:move".to_owned()]),
            legal_check_ids: BTreeSet::from(["check:search".to_owned()]),
            legal_target_ids: BTreeSet::from(["target:door".to_owned()]),
            legal_scene_ids: BTreeSet::from(["scene:viaduct".to_owned()]),
            legal_objective_ids: BTreeSet::new(),
            authoritative_facts: vec![MechanicalFact::Outcome {
                outcome_id: "outcome:success".to_owned(),
            }],
        }
    }

    #[test]
    fn legal_action_is_only_a_candidate_engine_command() {
        let proposal = TypedGmProposal::Action(ActionProposal {
            base: base(),
            action_id: "action:move".to_owned(),
            target_id: Some("target:door".to_owned()),
            rationale: "The player asked to approach the door.".to_owned(),
        });
        assert_eq!(
            proposal.validate_against(&context()).unwrap(),
            ProposalDisposition::ConvertToEngineCommand
        );
    }

    #[test]
    fn high_stakes_checks_require_confirmation() {
        let proposal = TypedGmProposal::Check(CheckProposal {
            base: base(),
            check_id: "check:search".to_owned(),
            difficulty: CheckDifficulty::Hard,
            stakes: CheckStakes::CharacterDefeat,
            rationale: "Failure would expose the hero.".to_owned(),
        });
        assert_eq!(
            proposal.validate_against(&context()).unwrap(),
            ProposalDisposition::RequirePlayerConfirmation
        );
    }

    #[test]
    fn stale_unknown_and_mechanical_fields_fail_closed() {
        let mut stale = base();
        stale.based_on_revision = 2;
        let proposal = TypedGmProposal::Action(ActionProposal {
            base: stale,
            action_id: "action:move".to_owned(),
            target_id: None,
            rationale: "Move.".to_owned(),
        });
        assert_eq!(
            proposal.validate_against(&context()),
            Err(TypedProposalError::Stale)
        );

        let forged = json!({
            "type": "check",
            "base": serde_json::to_value(base()).unwrap(),
            "check_id": "check:search",
            "difficulty": "hard",
            "stakes": "reversible",
            "rationale": "Search.",
            "difficulty_class": 1,
            "roll": 20
        });
        assert!(serde_json::from_value::<TypedGmProposal>(forged).is_err());
    }

    #[test]
    fn narration_claims_must_exactly_equal_committed_facts() {
        let accepted = TypedGmProposal::Narration(NarrationProposal {
            base: base(),
            narration_id: "narration:1".to_owned(),
            text: "The search succeeds.".to_owned(),
            claimed_facts: context().authoritative_facts,
        });
        assert_eq!(
            accepted.validate_against(&context()).unwrap(),
            ProposalDisposition::PresentationOnly
        );

        let contradictory = TypedGmProposal::Narration(NarrationProposal {
            base: base(),
            narration_id: "narration:2".to_owned(),
            text: "The search fails.".to_owned(),
            claimed_facts: vec![MechanicalFact::Outcome {
                outcome_id: "outcome:failure".to_owned(),
            }],
        });
        assert_eq!(
            contradictory.validate_against(&context()),
            Err(TypedProposalError::Contradiction)
        );
    }
}
