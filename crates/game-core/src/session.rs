use serde::{Deserialize, Serialize};

use crate::{
    AbilityCheckResult, AttemptSocialInteractionCommand, CommitEncounterCommand,
    CommittedEncounterOutcomeDto, D20Roll, ExperienceAwardSummary, GameCoreError, Result,
    RulesetId, Sha256Digest, SocialInteractionOutcomeDto, is_valid_opaque_id,
};

use crate::encounter::SOOT_WIGHT_POLICY_ID;

pub const SESSION_SCHEMA_VERSION: u16 = 1;
const MAX_SESSION_TITLE_CHARS: usize = 200;
const MAX_PARTY_CHARACTERS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDto {
    pub schema_version: u16,
    pub id: String,
    pub ruleset: RulesetId,
    pub title: String,
    pub status: SessionStatus,
    pub character_ids: Vec<String>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    /// Sequence assigned to the most recently committed event.
    pub last_event_sequence: u64,
}

impl SessionDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SESSION_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidSession {
                reason: "schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.id)
            || self.title.trim().is_empty()
            || self.title.chars().count() > MAX_SESSION_TITLE_CHARS
            || self.updated_at_unix_ms < self.created_at_unix_ms
            || self.character_ids.len() > MAX_PARTY_CHARACTERS
            || self.character_ids.iter().any(|id| !is_valid_opaque_id(id))
        {
            return Err(GameCoreError::InvalidSession {
                reason: "identity, title, timestamps, or party roster is invalid",
            });
        }
        let unique = self
            .character_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != self.character_ids.len() {
            return Err(GameCoreError::InvalidSession {
                reason: "party character ids must be unique",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventActor {
    Player { character_id: String },
    AiGameMaster,
    System,
}

/// Immutable provenance for an authoritative encounter mutation.
///
/// `LegacySystem` is only the serde default for events written before command-origin recording
/// existed. New application commits use `Player` or the pinned deterministic policy variant.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EncounterCommandOrigin {
    #[default]
    LegacySystem,
    Player,
    DeterministicPolicy {
        policy_id: String,
    },
}

impl EncounterCommandOrigin {
    fn validate(&self) -> Result<()> {
        match self {
            Self::LegacySystem | Self::Player => Ok(()),
            Self::DeterministicPolicy { policy_id } if policy_id == SOOT_WIGHT_POLICY_ID => Ok(()),
            Self::DeterministicPolicy { .. } => Err(GameCoreError::InvalidSessionEvent {
                reason: "encounter command policy provenance is invalid",
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEventDto {
    pub schema_version: u16,
    pub session_id: String,
    pub sequence: u64,
    pub occurred_at_unix_ms: u64,
    pub actor: EventActor,
    pub payload: SessionEventPayload,
}

impl SessionEventDto {
    /// Validates the self-contained event envelope before repository context
    /// checks such as campaign membership are applied.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SESSION_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidSessionEvent {
                reason: "schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.session_id) || self.sequence == 0 {
            return Err(GameCoreError::InvalidSessionEvent {
                reason: "session id must be present and sequence must start at one",
            });
        }
        if let EventActor::Player { character_id } = &self.actor
            && !is_valid_opaque_id(character_id)
        {
            return Err(GameCoreError::InvalidSessionEvent {
                reason: "player actor character id must not be empty",
            });
        }

        match &self.payload {
            SessionEventPayload::SessionStarted | SessionEventPayload::SessionEnded => {
                if !matches!(self.actor, EventActor::System) {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "session lifecycle events must be committed by the system",
                    });
                }
            }
            SessionEventPayload::PlayerIntent { character_id, text } => {
                if !matches!(
                    &self.actor,
                    EventActor::Player {
                        character_id: actor_id
                    } if actor_id == character_id
                ) || !is_valid_opaque_id(character_id)
                    || !bounded_text(text, 4_000)
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "player intent must match its actor and contain bounded text",
                    });
                }
            }
            SessionEventPayload::DiceResolved {
                purpose,
                roll,
                modifier,
                total,
            } => {
                roll.validate()?;
                if !matches!(self.actor, EventActor::System)
                    || !bounded_text(purpose, 500)
                    || i32::from(roll.selected).checked_add(*modifier) != Some(*total)
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "dice total or purpose is invalid",
                    });
                }
            }
            SessionEventPayload::AbilityCheckResolved {
                character_id,
                action_id,
                result,
            } => {
                if !matches!(self.actor, EventActor::System)
                    || !is_valid_opaque_id(character_id)
                    || !is_valid_opaque_id(action_id)
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "ability-check event actor or identifiers are invalid",
                    });
                }
                result.validate()?;
            }
            SessionEventPayload::ExplorationSocialResolved { command, outcome } => {
                command.validate()?;
                outcome.validate()?;
                if !matches!(self.actor, EventActor::System)
                    || command.campaign_session_id != self.session_id
                    || outcome.campaign_session_id != self.session_id
                    || command.character_id != outcome.character_id
                    || command.action_id != outcome.action_id
                    || command.expected_revision.checked_add(1) != Some(outcome.result_revision)
                    || outcome.event_sequence != self.sequence
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "social event envelope, revisions, or identities do not match",
                    });
                }
            }
            SessionEventPayload::EncounterResolved {
                command,
                outcome,
                command_origin,
            } => {
                command.validate()?;
                outcome.validate()?;
                command_origin.validate()?;
                if !matches!(command_origin, EncounterCommandOrigin::LegacySystem) {
                    outcome.validate_player_action_boundary()?;
                }
                if !matches!(self.actor, EventActor::System)
                    || command.campaign_session_id != self.session_id
                    || outcome.campaign_session_id != self.session_id
                    || outcome.event_sequence != self.sequence
                    || command.expected_campaign_revision.checked_add(1)
                        != Some(outcome.result_campaign_revision)
                    || command.command.encounter_id != outcome.resolution.encounter_id
                    || command.command.expected_revision != outcome.resolution.previous_revision
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "encounter event envelope, revisions, or state do not match",
                    });
                }
            }
            SessionEventPayload::GmNarration {
                text,
                image_prompt,
                source_prompt_id,
            } => {
                if !matches!(self.actor, EventActor::AiGameMaster)
                    || !bounded_text(text, 12_000)
                    || image_prompt
                        .as_ref()
                        .is_some_and(|prompt| !bounded_text(prompt, 4_000))
                    || source_prompt_id
                        .as_ref()
                        .is_some_and(|id| !is_valid_opaque_id(id))
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "GM narration must match its actor and contain bounded text",
                    });
                }
            }
            SessionEventPayload::ExperienceAwarded {
                character_id,
                summary,
            } => {
                if !matches!(self.actor, EventActor::System) || !is_valid_opaque_id(character_id) {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "experience award character id is invalid",
                    });
                }
                summary.validate()?;
            }
            SessionEventPayload::AiProposalAccepted { proposal_id, .. } => {
                if !matches!(self.actor, EventActor::System) || !is_valid_opaque_id(proposal_id) {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "accepted proposal id is invalid",
                    });
                }
            }
            SessionEventPayload::AiProposalRejected {
                proposal_id,
                reason,
                ..
            } => {
                if !matches!(self.actor, EventActor::System)
                    || !is_valid_opaque_id(proposal_id)
                    || !bounded_text(reason, 1_000)
                {
                    return Err(GameCoreError::InvalidSessionEvent {
                        reason: "rejected proposal id or reason is invalid",
                    });
                }
            }
        }
        Ok(())
    }
}

fn bounded_text(value: &str, maximum_chars: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= maximum_chars
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionEventPayload {
    SessionStarted,
    PlayerIntent {
        character_id: String,
        text: String,
    },
    DiceResolved {
        purpose: String,
        roll: D20Roll,
        modifier: i32,
        total: i32,
    },
    AbilityCheckResolved {
        character_id: String,
        /// Authored action whose trusted rules definition produced this check.
        action_id: String,
        result: AbilityCheckResult,
    },
    ExplorationSocialResolved {
        command: AttemptSocialInteractionCommand,
        outcome: Box<SocialInteractionOutcomeDto>,
    },
    EncounterResolved {
        command: CommitEncounterCommand,
        outcome: Box<CommittedEncounterOutcomeDto>,
        #[serde(default)]
        command_origin: EncounterCommandOrigin,
    },
    GmNarration {
        text: String,
        image_prompt: Option<String>,
        /// Opaque ID of the consent-filtered private inspiration, when used.
        /// Raw prompt content never belongs in a session event.
        source_prompt_id: Option<String>,
    },
    ExperienceAwarded {
        character_id: String,
        summary: ExperienceAwardSummary,
    },
    AiProposalAccepted {
        proposal_id: String,
        proposal_fingerprint: Sha256Digest,
    },
    AiProposalRejected {
        proposal_id: String,
        proposal_fingerprint: Sha256Digest,
        reason: String,
    },
    SessionEnded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Ability, AbilityCheck, AbilityScores, DiceExpression, DiceRoll,
        ENCOUNTER_COMMIT_SCHEMA_VERSION, Level, ModifierComponent, Proficiency, RULESET,
        RollContext, RollMetadata, RollMode, RollRecord,
        encounter::{
            EncounterCommand, EncounterIntent, EncounterRollMode, EncounterRollPurpose,
            EncounterState, LethalityPolicy, OpeningConsequence, legal_actions, resolve_encounter,
        },
    };

    fn ability_check_result() -> AbilityCheckResult {
        let check = AbilityCheck {
            ability: Ability::Wisdom,
            proficiency: Proficiency::Proficient,
            difficulty_class: 13,
            situational_modifier: 0,
            roll_context: RollContext::normal(),
        };
        let mut dice = |_| 12;
        check
            .resolve(
                &AbilityScores::new(10, 10, 10, 10, 14, 10).unwrap(),
                Level::new(3).unwrap(),
                &mut dice,
            )
            .unwrap()
    }

    fn encounter_event() -> SessionEventDto {
        let state = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        let nested = EncounterCommand::new(1, "encounter-start", EncounterIntent::StartEncounter);
        let mut values = [20_u16, 1].into_iter();
        let resolution =
            resolve_encounter(&state, &nested, &mut |_| values.next().unwrap()).unwrap();
        let mut cursor = 0_u64;
        let roll_records = resolution
            .rolls
            .iter()
            .map(|raw| {
                let cursor_before = cursor;
                cursor += u64::try_from(raw.individual_dice.len()).unwrap();
                let expression = raw.expression.parse::<DiceExpression>().unwrap();
                let roll = DiceRoll {
                    expression,
                    rolled_dice: raw
                        .individual_dice
                        .iter()
                        .map(|die| u32::from(die.value))
                        .collect(),
                    kept_dice: raw
                        .kept_die_indices
                        .iter()
                        .map(|index| u32::from(raw.individual_dice[usize::from(*index)].value))
                        .collect(),
                    total: raw.total,
                    roll_mode: match raw.mode {
                        EncounterRollMode::Normal => RollMode::Normal,
                        EncounterRollMode::Advantage => RollMode::Advantage,
                        EncounterRollMode::Disadvantage => RollMode::Disadvantage,
                    },
                    cursor_before,
                    cursor_after: cursor,
                };
                let purpose = match raw.purpose {
                    EncounterRollPurpose::Initiative => "encounter:initiative",
                    EncounterRollPurpose::Attack => "encounter:attack",
                    EncounterRollPurpose::Damage => "encounter:damage",
                    EncounterRollPurpose::Healing => "encounter:healing",
                    EncounterRollPurpose::SleepHitPoints => "encounter:sleep-hit-points",
                    EncounterRollPurpose::HitDie => "encounter:hit-die",
                    EncounterRollPurpose::DeathSave => "encounter:death-save",
                };
                RollRecord::from_roll(
                    roll,
                    RollMetadata {
                        roll_id: format!("roll:session-1:2:{}", raw.sequence),
                        purpose: purpose.to_owned(),
                        actor_id: raw.actor_id.clone(),
                        target_id: raw.target_id.clone(),
                        ruleset: RULESET,
                        seed_reference: "seed:test".to_owned(),
                    },
                    raw.modifiers
                        .iter()
                        .map(|modifier| ModifierComponent {
                            name: modifier.source_id.clone(),
                            value: i32::from(modifier.value),
                        })
                        .collect(),
                )
                .unwrap()
            })
            .collect();
        let command = CommitEncounterCommand {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            expected_campaign_revision: 2,
            command: nested,
        };
        let outcome = CommittedEncounterOutcomeDto {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            result_campaign_revision: 3,
            event_sequence: 2,
            result_hero_revision: None,
            legal_actions: legal_actions(&resolution.state).unwrap(),
            resolution,
            roll_records,
        };
        SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            sequence: 2,
            occurred_at_unix_ms: 200,
            actor: EventActor::System,
            payload: SessionEventPayload::EncounterResolved {
                command,
                outcome: Box::new(outcome),
                command_origin: EncounterCommandOrigin::Player,
            },
        }
    }

    #[test]
    fn session_dto_round_trips_as_json() {
        let session = SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: "session-1".into(),
            ruleset: RULESET,
            title: "The Rainy Gate".into(),
            status: SessionStatus::Active,
            character_ids: vec!["character-1".into()],
            created_at_unix_ms: 100,
            updated_at_unix_ms: 200,
            last_event_sequence: 4,
        };

        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<SessionDto>(&json).unwrap(), session);
    }

    #[test]
    fn event_payload_is_explicitly_tagged() {
        let payload = SessionEventPayload::PlayerIntent {
            character_id: "character-1".into(),
            text: "Open the gate".into(),
        };
        let json = serde_json::to_value(payload).unwrap();

        assert_eq!(json["type"], "player_intent");
    }

    #[test]
    fn durable_event_json_rejects_unknown_nested_fields() {
        let json = r#"{
            "schema_version":1,
            "session_id":"session-1",
            "sequence":1,
            "occurred_at_unix_ms":100,
            "actor":{"type":"player","character_id":"character-1"},
            "payload":{
                "type":"player_intent",
                "character_id":"character-1",
                "text":"Open the gate",
                "future_mechanic":true
            }
        }"#;

        assert!(serde_json::from_str::<SessionEventDto>(json).is_err());
    }

    #[test]
    fn authoritative_events_require_the_system_actor() {
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 100,
            actor: EventActor::AiGameMaster,
            payload: SessionEventPayload::SessionEnded,
        };

        assert!(event.validate().is_err());
    }

    #[test]
    fn ability_check_event_requires_system_actor_and_valid_authored_ids() {
        let mut event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 100,
            actor: EventActor::System,
            payload: SessionEventPayload::AbilityCheckResolved {
                character_id: "character-1".to_owned(),
                action_id: "inspect-rune".to_owned(),
                result: ability_check_result(),
            },
        };
        event.validate().unwrap();

        event.actor = EventActor::AiGameMaster;
        assert!(event.validate().is_err());

        event.actor = EventActor::System;
        let SessionEventPayload::AbilityCheckResolved { action_id, .. } = &mut event.payload else {
            unreachable!("test event has the expected payload")
        };
        *action_id = "forged action".to_owned();
        assert!(event.validate().is_err());
    }

    #[test]
    fn ability_check_event_rejects_a_tampered_result() {
        let mut result = ability_check_result();
        result.total += 1;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 100,
            actor: EventActor::System,
            payload: SessionEventPayload::AbilityCheckResolved {
                character_id: "character-1".to_owned(),
                action_id: "inspect-rune".to_owned(),
                result,
            },
        };

        assert!(matches!(
            event.validate(),
            Err(GameCoreError::InvalidAbilityCheckResult { .. })
        ));
    }

    #[test]
    fn encounter_event_round_trips_with_a_strict_validated_envelope() {
        let event = encounter_event();
        event.validate().unwrap();

        let encoded = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serde_json::from_str::<SessionEventDto>(&encoded).unwrap(),
            event
        );
    }

    #[test]
    fn legacy_encounter_event_without_command_origin_still_decodes() {
        let mut json = serde_json::to_value(encounter_event()).unwrap();
        json.pointer_mut("/payload")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("command_origin");

        let decoded = serde_json::from_value::<SessionEventDto>(json).unwrap();
        let SessionEventPayload::EncounterResolved { command_origin, .. } = &decoded.payload else {
            unreachable!("fixture is an encounter event")
        };
        assert_eq!(command_origin, &EncounterCommandOrigin::LegacySystem);
        decoded.validate().unwrap();
    }

    #[test]
    fn encounter_event_rejects_unpinned_policy_origin() {
        let mut event = encounter_event();
        let SessionEventPayload::EncounterResolved { command_origin, .. } = &mut event.payload
        else {
            unreachable!("fixture is an encounter event")
        };
        *command_origin = EncounterCommandOrigin::DeterministicPolicy {
            policy_id: "policy:forged".to_owned(),
        };
        assert!(event.validate().is_err());
    }

    #[test]
    fn encounter_event_rejects_envelope_state_and_roll_mismatches() {
        let mut wrong_session = encounter_event();
        wrong_session.session_id = "session-2".to_owned();
        assert!(wrong_session.validate().is_err());

        let mut wrong_sequence = encounter_event();
        wrong_sequence.sequence = 3;
        assert!(wrong_sequence.validate().is_err());

        let mut wrong_encounter_revision = encounter_event();
        let SessionEventPayload::EncounterResolved { command, .. } =
            &mut wrong_encounter_revision.payload
        else {
            unreachable!()
        };
        command.command.expected_revision = 2;
        assert!(wrong_encounter_revision.validate().is_err());

        let mut forged_state = encounter_event();
        let SessionEventPayload::EncounterResolved { outcome, .. } = &mut forged_state.payload
        else {
            unreachable!()
        };
        outcome.resolution.state.revision = 99;
        assert!(forged_state.validate().is_err());

        let mut forged_roll = encounter_event();
        let SessionEventPayload::EncounterResolved { outcome, .. } = &mut forged_roll.payload
        else {
            unreachable!()
        };
        outcome.roll_records[0].total += 1;
        assert!(forged_roll.validate().is_err());
    }
}
