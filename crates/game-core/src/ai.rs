use serde::{Deserialize, Serialize};

use crate::Ability;

pub const AI_PROPOSAL_SCHEMA_VERSION: u16 = 1;

/// Declarative output from an AI game master.
///
/// Consuming applications must validate known identifiers and explicitly
/// commit proposed effects. Constructing or deserializing this value never
/// changes a character or session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiGmProposal {
    pub schema_version: u16,
    pub proposal_id: String,
    pub session_id: String,
    /// Event sequence on which the model based this proposal.
    pub based_on_event_sequence: u64,
    pub narrative: Option<GeneratedNarrative>,
    pub effects: Vec<ProposedEffect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedNarrative {
    pub text: String,
    /// A separate prompt that an image-generation adapter may consume.
    pub image_prompt: Option<String>,
    /// Player-facing options, never executable commands.
    #[serde(default)]
    pub choices: Vec<String>,
}

/// A bounded narrative assessment. Trusted rules policy maps this to a DC and
/// can reject or adjust it; the model cannot supply a raw difficulty class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckDifficulty {
    VeryEasy,
    Easy,
    Moderate,
    Hard,
    VeryHard,
    NearlyImpossible,
}

impl CheckDifficulty {
    pub const fn baseline_dc(self) -> u16 {
        match self {
            Self::VeryEasy => 5,
            Self::Easy => 10,
            Self::Moderate => 15,
            Self::Hard => 20,
            Self::VeryHard => 25,
            Self::NearlyImpossible => 30,
        }
    }
}

/// A narrative reward signal. Encounter and campaign policy decides whether
/// this becomes XP, milestone progress, treasure, or no mechanical reward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardTier {
    Minor,
    Significant,
    Major,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposedEffect {
    RequestAbilityCheck {
        character_id: String,
        ability: Ability,
        /// A stable rules-content identifier, validated against the character.
        skill_id: Option<String>,
        difficulty: CheckDifficulty,
        reason: String,
    },
    RequestAttack {
        character_id: String,
        target_id: String,
        /// A stable identifier from the character's legal attack list.
        attack_id: String,
        reason: String,
    },
    ProposeReward {
        character_id: String,
        tier: RewardTier,
        reason: String,
    },
    IntroduceEvent {
        title: String,
        description: String,
        /// Optional identifier of the consent-filtered prompt that inspired it.
        source_prompt_id: Option<String>,
    },
    EndSession {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbilityScores, CharacterDraft, SessionEventPayload, Sha256Digest};

    #[test]
    fn a_reward_proposal_does_not_mutate_character_state() {
        let character = CharacterDraft {
            id: "character-1".into(),
            name: "Mara".into(),
            theme: "urban folklore".into(),
            ability_scores: AbilityScores::new(10, 10, 10, 10, 10, 10).unwrap(),
            experience_points: 0,
            current_hit_points: 8,
            maximum_hit_points: 8,
        }
        .build()
        .unwrap();

        let proposal = AiGmProposal {
            schema_version: AI_PROPOSAL_SCHEMA_VERSION,
            proposal_id: "proposal-1".into(),
            session_id: "session-1".into(),
            based_on_event_sequence: 7,
            narrative: None,
            effects: vec![ProposedEffect::ProposeReward {
                character_id: character.id().into(),
                tier: RewardTier::Significant,
                reason: "Resolved the encounter".into(),
            }],
        };

        assert_eq!(character.experience_points(), 0);
        assert_eq!(proposal.effects.len(), 1);

        let accepted = SessionEventPayload::AiProposalAccepted {
            proposal_id: proposal.proposal_id,
            proposal_fingerprint: Sha256Digest::from_bytes([0; 32]),
        };
        assert!(matches!(
            accepted,
            SessionEventPayload::AiProposalAccepted { .. }
        ));
    }

    #[test]
    fn proposal_decoding_rejects_unknown_mechanical_fields() {
        let json = r#"{
            "schema_version": 1,
            "proposal_id": "proposal-1",
            "session_id": "session-1",
            "based_on_event_sequence": 0,
            "narrative": null,
            "effects": [{
                "type": "request_ability_check",
                "character_id": "character-1",
                "ability": "wisdom",
                "skill_id": "perception",
                "difficulty": "moderate",
                "difficulty_class": 99,
                "reason": "Look for the hidden door"
            }]
        }"#;

        assert!(serde_json::from_str::<AiGmProposal>(json).is_err());
    }

    #[test]
    fn difficulty_bands_have_stable_baselines() {
        assert_eq!(CheckDifficulty::VeryEasy.baseline_dc(), 5);
        assert_eq!(CheckDifficulty::Moderate.baseline_dc(), 15);
        assert_eq!(CheckDifficulty::NearlyImpossible.baseline_dc(), 30);
    }
}
