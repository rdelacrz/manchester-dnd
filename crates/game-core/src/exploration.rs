use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{AbilityCheckResult, GameCoreError, Result, is_valid_opaque_id};

pub const EXPLORATION_CHECK_SCHEMA_VERSION: u16 = 1;
pub const LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION: u16 = 1;
const MAX_LOCAL_CAMPAIGN_TEXT_CHARS: usize = 200;

/// Player intent for one authored exploration check.
///
/// Mechanical inputs are deliberately absent. The authoritative application
/// maps `action_id` to the ability, proficiency, difficulty, modifiers, and
/// roll context before asking the rules engine to resolve it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptExplorationCheckCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub action_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
}

impl AttemptExplorationCheckCommand {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != EXPLORATION_CHECK_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.character_id)
            || !is_valid_opaque_id(&self.action_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_revision == 0
        {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "identifiers must be valid and expected revision must be positive",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AttemptExplorationCheckCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCommand {
            schema_version: u16,
            campaign_session_id: String,
            character_id: String,
            action_id: String,
            expected_revision: u64,
            idempotency_key: String,
        }

        let wire = WireCommand::deserialize(deserializer)?;
        let command = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            character_id: wire.character_id,
            action_id: wire.action_id,
            expected_revision: wire.expected_revision,
            idempotency_key: wire.idempotency_key,
        };
        command.validate().map_err(D::Error::custom)?;
        Ok(command)
    }
}

/// Public result of a committed exploration check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExplorationCheckOutcomeDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub action_id: String,
    pub result_revision: u64,
    pub event_sequence: u64,
    pub result: AbilityCheckResult,
}

impl ExplorationCheckOutcomeDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != EXPLORATION_CHECK_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidExplorationCheckOutcome {
                reason: "schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.character_id)
            || !is_valid_opaque_id(&self.action_id)
            || self.result_revision == 0
            || self.event_sequence == 0
        {
            return Err(GameCoreError::InvalidExplorationCheckOutcome {
                reason: "identifiers, result revision, or event sequence are invalid",
            });
        }
        if self.event_sequence.checked_add(1) != Some(self.result_revision) {
            return Err(GameCoreError::InvalidExplorationCheckOutcome {
                reason: "result revision must immediately follow the event sequence",
            });
        }
        self.result.validate()
    }
}

impl<'de> Deserialize<'de> for ExplorationCheckOutcomeDto {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireOutcome {
            schema_version: u16,
            campaign_session_id: String,
            character_id: String,
            action_id: String,
            result_revision: u64,
            event_sequence: u64,
            result: AbilityCheckResult,
        }

        let wire = WireOutcome::deserialize(deserializer)?;
        let outcome = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            character_id: wire.character_id,
            action_id: wire.action_id,
            result_revision: wire.result_revision,
            event_sequence: wire.event_sequence,
            result: wire.result,
        };
        outcome.validate().map_err(D::Error::custom)?;
        Ok(outcome)
    }
}

/// Public, reloadable projection for the current local campaign and hero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalCampaignViewDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub campaign_title: String,
    pub character_name: String,
    pub revision: u64,
    pub last_event_sequence: u64,
    pub latest_check: Option<ExplorationCheckOutcomeDto>,
}

impl LocalCampaignViewDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidLocalCampaignView {
                reason: "schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.character_id)
            || !bounded_text(&self.campaign_title, MAX_LOCAL_CAMPAIGN_TEXT_CHARS)
            || !bounded_text(&self.character_name, MAX_LOCAL_CAMPAIGN_TEXT_CHARS)
            || self.revision == 0
        {
            return Err(GameCoreError::InvalidLocalCampaignView {
                reason: "identifiers, display text, or revision are invalid",
            });
        }
        if let Some(latest) = &self.latest_check {
            latest.validate()?;
            if latest.campaign_session_id != self.campaign_session_id
                || latest.character_id != self.character_id
                || latest.result_revision > self.revision
                || latest.event_sequence > self.last_event_sequence
            {
                return Err(GameCoreError::InvalidLocalCampaignView {
                    reason: "latest check does not belong to this campaign view or is from the future",
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for LocalCampaignViewDto {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireView {
            schema_version: u16,
            campaign_session_id: String,
            character_id: String,
            campaign_title: String,
            character_name: String,
            revision: u64,
            last_event_sequence: u64,
            latest_check: Option<ExplorationCheckOutcomeDto>,
        }

        let wire = WireView::deserialize(deserializer)?;
        let view = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            character_id: wire.character_id,
            campaign_title: wire.campaign_title,
            character_name: wire.character_name,
            revision: wire.revision,
            last_event_sequence: wire.last_event_sequence,
            latest_check: wire.latest_check,
        };
        view.validate().map_err(D::Error::custom)?;
        Ok(view)
    }
}

fn bounded_text(value: &str, maximum_chars: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= maximum_chars
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Ability, AbilityCheck, AbilityScores, Level, Proficiency, RollContext};

    fn command_json() -> serde_json::Value {
        json!({
            "schema_version": EXPLORATION_CHECK_SCHEMA_VERSION,
            "campaign_session_id": "session-1",
            "character_id": "character-1",
            "action_id": "inspect-rune",
            "expected_revision": 3,
            "idempotency_key": "command-1"
        })
    }

    fn result() -> AbilityCheckResult {
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

    fn outcome() -> ExplorationCheckOutcomeDto {
        ExplorationCheckOutcomeDto {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            character_id: "character-1".to_owned(),
            action_id: "inspect-rune".to_owned(),
            result_revision: 4,
            event_sequence: 3,
            result: result(),
        }
    }

    #[test]
    fn command_round_trips_without_mechanical_inputs() {
        let command = serde_json::from_value::<AttemptExplorationCheckCommand>(command_json())
            .expect("valid authored intent should decode");
        command.validate().unwrap();

        let encoded = serde_json::to_value(command).unwrap();
        assert!(encoded.get("roll").is_none());
        assert!(encoded.get("difficulty_class").is_none());
        assert!(encoded.get("modifier").is_none());
        assert!(encoded.get("actor").is_none());
    }

    #[test]
    fn command_rejects_forged_mechanical_and_unknown_fields() {
        for (field, value) in [
            ("roll", json!(20)),
            ("difficulty_class", json!(1)),
            ("modifier", json!(99)),
            ("actor", json!("system")),
            ("future_field", json!(true)),
        ] {
            let mut forged = command_json();
            forged
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), value);
            assert!(
                serde_json::from_value::<AttemptExplorationCheckCommand>(forged).is_err(),
                "field {field} must be rejected"
            );
        }
    }

    #[test]
    fn command_deserialization_enforces_schema_ids_and_revision() {
        for (field, value) in [
            ("schema_version", json!(99)),
            ("campaign_session_id", json!("../session")),
            ("character_id", json!("")),
            ("action_id", json!("contains spaces")),
            ("expected_revision", json!(0)),
            ("idempotency_key", json!("")),
        ] {
            let mut invalid = command_json();
            invalid
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), value);
            assert!(
                serde_json::from_value::<AttemptExplorationCheckCommand>(invalid).is_err(),
                "field {field} must be validated"
            );
        }
    }

    #[test]
    fn outcome_round_trips_with_a_validated_result() {
        let outcome = outcome();

        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(
            serde_json::from_str::<ExplorationCheckOutcomeDto>(&json).unwrap(),
            outcome
        );
    }

    #[test]
    fn outcome_rejects_a_revision_that_does_not_match_its_event() {
        let mut outcome = outcome();
        outcome.event_sequence = 2;

        assert!(matches!(
            outcome.validate(),
            Err(GameCoreError::InvalidExplorationCheckOutcome { .. })
        ));
    }

    #[test]
    fn local_campaign_view_round_trips_with_a_prior_latest_check() {
        let view = LocalCampaignViewDto {
            schema_version: LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            character_id: "character-1".to_owned(),
            campaign_title: "Rain over Ancoats".to_owned(),
            character_name: "Mara".to_owned(),
            revision: 6,
            last_event_sequence: 5,
            latest_check: Some(outcome()),
        };

        let json = serde_json::to_string(&view).unwrap();
        assert_eq!(
            serde_json::from_str::<LocalCampaignViewDto>(&json).unwrap(),
            view
        );
    }

    #[test]
    fn local_campaign_view_rejects_mismatched_or_future_latest_checks() {
        let base = LocalCampaignViewDto {
            schema_version: LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            character_id: "character-1".to_owned(),
            campaign_title: "Rain over Ancoats".to_owned(),
            character_name: "Mara".to_owned(),
            revision: 4,
            last_event_sequence: 3,
            latest_check: Some(outcome()),
        };
        base.validate().unwrap();

        let mut wrong_campaign = base.clone();
        wrong_campaign
            .latest_check
            .as_mut()
            .unwrap()
            .campaign_session_id = "session-2".to_owned();
        assert!(wrong_campaign.validate().is_err());

        let mut wrong_character = base.clone();
        wrong_character.latest_check.as_mut().unwrap().character_id = "character-2".to_owned();
        assert!(wrong_character.validate().is_err());

        let mut future_revision = base.clone();
        future_revision
            .latest_check
            .as_mut()
            .unwrap()
            .result_revision = 5;
        assert!(future_revision.validate().is_err());

        let mut future_sequence = base;
        future_sequence
            .latest_check
            .as_mut()
            .unwrap()
            .event_sequence = 4;
        assert!(future_sequence.validate().is_err());
    }

    #[test]
    fn local_campaign_view_deserialization_bounds_text_and_unknown_fields() {
        let mut json = serde_json::to_value(LocalCampaignViewDto {
            schema_version: LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            character_id: "character-1".to_owned(),
            campaign_title: "Rain over Ancoats".to_owned(),
            character_name: "Mara".to_owned(),
            revision: 1,
            last_event_sequence: 0,
            latest_check: None,
        })
        .unwrap();
        json.as_object_mut().unwrap().insert(
            "campaign_title".to_owned(),
            json!("x".repeat(MAX_LOCAL_CAMPAIGN_TEXT_CHARS + 1)),
        );
        assert!(serde_json::from_value::<LocalCampaignViewDto>(json.clone()).is_err());

        json.as_object_mut()
            .unwrap()
            .insert("campaign_title".to_owned(), json!("Rain over Ancoats"));
        json.as_object_mut()
            .unwrap()
            .insert("hidden_state".to_owned(), json!(true));
        assert!(serde_json::from_value::<LocalCampaignViewDto>(json).is_err());

        let mut invalid_identity = serde_json::to_value(LocalCampaignViewDto {
            schema_version: LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            character_id: "character-1".to_owned(),
            campaign_title: "Rain over Ancoats".to_owned(),
            character_name: "Mara".to_owned(),
            revision: 1,
            last_event_sequence: 0,
            latest_check: None,
        })
        .unwrap();
        invalid_identity
            .as_object_mut()
            .unwrap()
            .insert("character_id".to_owned(), json!("../character"));
        assert!(serde_json::from_value::<LocalCampaignViewDto>(invalid_identity).is_err());
    }
}
