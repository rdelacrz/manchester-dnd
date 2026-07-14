use serde::{Deserialize, Serialize};

use crate::{
    D20Roll, ExperienceAwardSummary, GameCoreError, Result, RulesetId, Sha256Digest,
    is_valid_opaque_id,
};

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
    use crate::RULESET;

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
}
