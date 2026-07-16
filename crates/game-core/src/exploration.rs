use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{
    AbilityCheckResult, DiceExpression, GameCoreError, RULESET, Result, RollMode, RollRecord,
    campaign_pins::CampaignPinStatusDto,
    encounter::{
        EncounterCommand, EncounterResolution, EncounterRollMode, EncounterRollPurpose,
        EncounterState, EncounterStatus, LegalEncounterAction, SOOT_WIGHT_ENCOUNTER_ID,
        legal_actions, player_legal_actions,
    },
    is_valid_opaque_id,
    rules_matrix::{ExplorationSocialFact, ExplorationSocialState, TrustedCheckResolution},
};

pub const EXPLORATION_CHECK_SCHEMA_VERSION: u16 = 1;
pub const SOCIAL_INTERACTION_SCHEMA_VERSION: u16 = 1;
pub const ENCOUNTER_COMMIT_SCHEMA_VERSION: u16 = 1;
pub const ADVANCE_NPC_TURN_SCHEMA_VERSION: u16 = 1;
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

/// Intent-only request for the one authored pre-encounter social approach.
/// The server owns the skill, proficiency, difficulty, state transitions, and
/// roll; the browser supplies only identity, revision, and idempotency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptSocialInteractionCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub action_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
}

impl AttemptSocialInteractionCommand {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SOCIAL_INTERACTION_SCHEMA_VERSION
            || !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.character_id)
            || !is_valid_opaque_id(&self.action_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_revision == 0
        {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "social interaction schema, identifiers, or revision are invalid",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AttemptSocialInteractionCommand {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SocialInteractionOutcomeDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub character_id: String,
    pub action_id: String,
    pub result_revision: u64,
    pub event_sequence: u64,
    pub check: TrustedCheckResolution,
    pub facts: Vec<ExplorationSocialFact>,
    pub resulting_state: ExplorationSocialState,
}

impl SocialInteractionOutcomeDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SOCIAL_INTERACTION_SCHEMA_VERSION
            || !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.character_id)
            || !is_valid_opaque_id(&self.action_id)
            || self.result_revision == 0
            || self.event_sequence == 0
            || self.event_sequence.checked_add(1) != Some(self.result_revision)
            || self.facts.is_empty()
            || self.facts.len() > 8
        {
            return Err(GameCoreError::InvalidExplorationCheckOutcome {
                reason: "social interaction outcome envelope is invalid",
            });
        }
        self.check
            .validate()
            .map_err(|_| GameCoreError::InvalidExplorationCheckOutcome {
                reason: "social interaction check is invalid",
            })?;
        self.resulting_state
            .validate()
            .map_err(|_| GameCoreError::InvalidExplorationCheckOutcome {
                reason: "social interaction state is invalid",
            })
    }
}

impl<'de> Deserialize<'de> for SocialInteractionOutcomeDto {
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
            check: TrustedCheckResolution,
            facts: Vec<ExplorationSocialFact>,
            resulting_state: ExplorationSocialState,
        }

        let wire = WireOutcome::deserialize(deserializer)?;
        let outcome = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            character_id: wire.character_id,
            action_id: wire.action_id,
            result_revision: wire.result_revision,
            event_sequence: wire.event_sequence,
            check: wire.check,
            facts: wire.facts,
            resulting_state: wire.resulting_state,
        };
        outcome.validate().map_err(D::Error::custom)?;
        Ok(outcome)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialSceneViewDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub campaign_revision: u64,
    pub last_event_sequence: u64,
    pub state: ExplorationSocialState,
    pub latest_outcome: Option<SocialInteractionOutcomeDto>,
}

impl SocialSceneViewDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SOCIAL_INTERACTION_SCHEMA_VERSION
            || !is_valid_opaque_id(&self.campaign_session_id)
            || self.campaign_revision == 0
            || self.last_event_sequence.checked_add(1) != Some(self.campaign_revision)
        {
            return Err(GameCoreError::InvalidLocalCampaignView {
                reason: "social scene view envelope is invalid",
            });
        }
        self.state
            .validate()
            .map_err(|_| GameCoreError::InvalidLocalCampaignView {
                reason: "social scene state is invalid",
            })?;
        if let Some(outcome) = &self.latest_outcome {
            outcome.validate()?;
            if outcome.campaign_session_id != self.campaign_session_id
                || outcome.result_revision > self.campaign_revision
                || outcome.event_sequence > self.last_event_sequence
                || outcome.resulting_state != self.state
            {
                return Err(GameCoreError::InvalidLocalCampaignView {
                    reason: "social scene outcome does not belong to this view",
                });
            }
        }
        Ok(())
    }
}

/// Campaign-scoped envelope for one pure encounter command.
///
/// Campaign and encounter revisions are intentionally distinct: the campaign revision protects
/// the database aggregate while the nested command revision protects the encounter state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitEncounterCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_campaign_revision: u64,
    pub command: EncounterCommand,
}

impl CommitEncounterCommand {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ENCOUNTER_COMMIT_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "encounter commit schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id) || self.expected_campaign_revision == 0 {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "encounter campaign identity or revision is invalid",
            });
        }
        self.command
            .validate()
            .map_err(|_| GameCoreError::InvalidExplorationCheckCommand {
                reason: "nested encounter command is invalid",
            })
    }
}

impl<'de> Deserialize<'de> for CommitEncounterCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCommand {
            schema_version: u16,
            campaign_session_id: String,
            expected_campaign_revision: u64,
            command: EncounterCommand,
        }

        let wire = WireCommand::deserialize(deserializer)?;
        let command = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            expected_campaign_revision: wire.expected_campaign_revision,
            command: wire.command,
        };
        command.validate().map_err(D::Error::custom)?;
        Ok(command)
    }
}

/// Intent-only request to let the authoritative server advance one deterministic NPC action.
///
/// There is deliberately no actor, action, target, destination, roll, or mechanic field. The
/// server reloads canonical state and applies the closed Soot Wight policy after validating both
/// optimistic-lock revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdvanceNpcTurnCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_campaign_revision: u64,
    pub expected_encounter_revision: u64,
    pub idempotency_key: String,
}

impl AdvanceNpcTurnCommand {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ADVANCE_NPC_TURN_SCHEMA_VERSION {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "NPC advance schema version is unsupported",
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_campaign_revision == 0
            || self.expected_encounter_revision == 0
        {
            return Err(GameCoreError::InvalidExplorationCheckCommand {
                reason: "NPC advance identity and revisions are invalid",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AdvanceNpcTurnCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCommand {
            schema_version: u16,
            campaign_session_id: String,
            expected_campaign_revision: u64,
            expected_encounter_revision: u64,
            idempotency_key: String,
        }

        let wire = WireCommand::deserialize(deserializer)?;
        let command = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            expected_campaign_revision: wire.expected_campaign_revision,
            expected_encounter_revision: wire.expected_encounter_revision,
            idempotency_key: wire.idempotency_key,
        };
        command.validate().map_err(D::Error::custom)?;
        Ok(command)
    }
}

/// Public response for one encounter command after state, audit, rolls, and receipt commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedEncounterOutcomeDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub result_campaign_revision: u64,
    pub event_sequence: u64,
    /// The zero-based authoritative hero document revision committed in the
    /// same transaction. Historical and fixed-hero encounters have no value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_hero_revision: Option<u64>,
    pub resolution: EncounterResolution,
    pub roll_records: Vec<RollRecord>,
    pub legal_actions: Vec<LegalEncounterAction>,
}

impl CommittedEncounterOutcomeDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ENCOUNTER_COMMIT_SCHEMA_VERSION
            || !is_valid_opaque_id(&self.campaign_session_id)
            || self.result_campaign_revision == 0
            || self.event_sequence == 0
            || self.event_sequence.checked_add(1) != Some(self.result_campaign_revision)
            || self.resolution.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
        {
            return Err(invalid_encounter_outcome(
                "encounter outcome schema, identity, campaign revision, or sequence is invalid",
            ));
        }
        self.resolution
            .validate()
            .map_err(|_| invalid_encounter_outcome("encounter resolution is invalid"))?;
        let created_hero = self.resolution.state.hero.source_character_id.is_some();
        match (
            self.resolution.state.schema_version,
            created_hero,
            self.result_hero_revision,
        ) {
            (crate::encounter::ENCOUNTER_SCHEMA_VERSION, true, Some(_)) => {}
            (crate::encounter::ENCOUNTER_SCHEMA_VERSION, false, None)
            | (crate::encounter::LEGACY_ENCOUNTER_SCHEMA_VERSION, _, None)
            | (crate::encounter::LIVE_V2_ENCOUNTER_SCHEMA_VERSION, _, None) => {}
            _ => {
                return Err(invalid_encounter_outcome(
                    "authoritative hero revision does not match the encounter schema and hero source",
                ));
            }
        }
        if self.roll_records.len() != self.resolution.rolls.len() {
            return Err(invalid_encounter_outcome(
                "canonical roll count does not match raw resolution rolls",
            ));
        }
        let mut previous_cursor_after = None;
        let mut seed_reference: Option<&str> = None;
        for (raw, record) in self.resolution.rolls.iter().zip(&self.roll_records) {
            record
                .validate()
                .map_err(|_| invalid_encounter_outcome("canonical roll record is invalid"))?;
            validate_roll_record_pair(&self.campaign_session_id, self.event_sequence, raw, record)?;
            if previous_cursor_after.is_some_and(|cursor| cursor != record.cursor_before) {
                return Err(invalid_encounter_outcome(
                    "canonical roll cursor ranges are not contiguous",
                ));
            }
            if seed_reference.is_some_and(|reference| reference != record.seed_reference) {
                return Err(invalid_encounter_outcome(
                    "canonical rolls do not share one seed reference",
                ));
            }
            previous_cursor_after = Some(record.cursor_after);
            seed_reference = Some(&record.seed_reference);
        }
        let expected_player_legal = player_legal_actions(&self.resolution.state)
            .map_err(|_| invalid_encounter_outcome("legal encounter actions cannot be derived"))?;
        let legacy_legal = legal_actions(&self.resolution.state).map_err(|_| {
            invalid_encounter_outcome("legacy legal encounter actions cannot be derived")
        })?;
        if self.legal_actions != expected_player_legal && self.legal_actions != legacy_legal {
            return Err(invalid_encounter_outcome(
                "legal actions do not match the committed encounter state",
            ));
        }
        Ok(())
    }

    /// Enforces the current public player/controller boundary. The broader [`Self::validate`]
    /// still accepts pre-boundary persisted outcomes so legacy audit events remain replayable.
    pub fn validate_player_action_boundary(&self) -> Result<()> {
        self.validate()?;
        let expected = player_legal_actions(&self.resolution.state)
            .map_err(|_| invalid_encounter_outcome("player legal actions cannot be derived"))?;
        if self.legal_actions != expected {
            return Err(invalid_encounter_outcome(
                "public legal actions expose a non-player actor",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CommittedEncounterOutcomeDto {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireOutcome {
            schema_version: u16,
            campaign_session_id: String,
            result_campaign_revision: u64,
            event_sequence: u64,
            #[serde(default)]
            result_hero_revision: Option<u64>,
            resolution: EncounterResolution,
            roll_records: Vec<RollRecord>,
            legal_actions: Vec<LegalEncounterAction>,
        }

        let wire = WireOutcome::deserialize(deserializer)?;
        let outcome = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            result_campaign_revision: wire.result_campaign_revision,
            event_sequence: wire.event_sequence,
            result_hero_revision: wire.result_hero_revision,
            resolution: wire.resolution,
            roll_records: wire.roll_records,
            legal_actions: wire.legal_actions,
        };
        outcome.validate().map_err(D::Error::custom)?;
        Ok(outcome)
    }
}

/// Reloadable encounter projection. Before the first encounter command, `latest_outcome` is
/// absent and `state` is the deterministic ready state derived from the exploration result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterViewDto {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub campaign_revision: u64,
    pub last_event_sequence: u64,
    pub state: EncounterState,
    pub legal_actions: Vec<LegalEncounterAction>,
    pub latest_outcome: Option<CommittedEncounterOutcomeDto>,
}

impl EncounterViewDto {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ENCOUNTER_COMMIT_SCHEMA_VERSION
            || !is_valid_opaque_id(&self.campaign_session_id)
            || self.campaign_revision == 0
            || self.last_event_sequence.checked_add(1) != Some(self.campaign_revision)
        {
            return Err(invalid_encounter_outcome(
                "encounter view schema, identity, or campaign revision is invalid",
            ));
        }
        self.state
            .validate()
            .map_err(|_| invalid_encounter_outcome("encounter view state is invalid"))?;
        let expected_legal = player_legal_actions(&self.state).map_err(|_| {
            invalid_encounter_outcome("encounter view legal actions cannot be derived")
        })?;
        if self.legal_actions != expected_legal {
            return Err(invalid_encounter_outcome(
                "encounter view legal actions do not match its state",
            ));
        }
        match &self.latest_outcome {
            Some(outcome) => {
                outcome.validate()?;
                if outcome.campaign_session_id != self.campaign_session_id
                    || outcome.result_campaign_revision > self.campaign_revision
                    || outcome.event_sequence > self.last_event_sequence
                    || outcome.resolution.state != self.state
                    || outcome.legal_actions != self.legal_actions
                {
                    return Err(invalid_encounter_outcome(
                        "latest encounter outcome does not belong to this view",
                    ));
                }
            }
            None if self.state.status == EncounterStatus::Ready && self.state.revision == 1 => {}
            None => {
                return Err(invalid_encounter_outcome(
                    "a progressed encounter view requires its latest committed outcome",
                ));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for EncounterViewDto {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireView {
            schema_version: u16,
            campaign_session_id: String,
            campaign_revision: u64,
            last_event_sequence: u64,
            state: EncounterState,
            legal_actions: Vec<LegalEncounterAction>,
            latest_outcome: Option<CommittedEncounterOutcomeDto>,
        }

        let wire = WireView::deserialize(deserializer)?;
        let view = Self {
            schema_version: wire.schema_version,
            campaign_session_id: wire.campaign_session_id,
            campaign_revision: wire.campaign_revision,
            last_event_sequence: wire.last_event_sequence,
            state: wire.state,
            legal_actions: wire.legal_actions,
            latest_outcome: wire.latest_outcome,
        };
        view.validate().map_err(D::Error::custom)?;
        Ok(view)
    }
}

fn validate_roll_record_pair(
    campaign_session_id: &str,
    event_sequence: u64,
    raw: &crate::encounter::RawRollFacts,
    record: &RollRecord,
) -> Result<()> {
    let expected_roll_id = format!(
        "roll:{campaign_session_id}:{event_sequence}:{}",
        raw.sequence
    );
    let expected_expression = raw
        .expression
        .parse::<DiceExpression>()
        .map_err(|_| invalid_encounter_outcome("raw roll expression is not canonical"))?;
    let expected_rolled = raw
        .individual_dice
        .iter()
        .map(|die| u32::from(die.value))
        .collect::<Vec<_>>();
    let expected_kept = raw
        .kept_die_indices
        .iter()
        .map(|index| u32::from(raw.individual_dice[usize::from(*index)].value))
        .collect::<Vec<_>>();
    let mut expected_modifiers = raw
        .modifiers
        .iter()
        .map(|modifier| (modifier.source_id.as_str(), i32::from(modifier.value)))
        .collect::<Vec<_>>();
    expected_modifiers.sort_unstable_by_key(|(name, _)| *name);
    let actual_modifiers = record
        .modifier_components
        .iter()
        .map(|modifier| (modifier.name.as_str(), modifier.value))
        .collect::<Vec<_>>();

    if record.roll_id != expected_roll_id
        || record.expression != expected_expression
        || record.rolled_dice != expected_rolled
        || record.kept_dice != expected_kept
        || actual_modifiers != expected_modifiers
        || record.total != raw.total
        || record.purpose != roll_purpose_id(raw.purpose)
        || record.actor_id != raw.actor_id
        || record.target_id != raw.target_id
        || record.roll_mode != canonical_roll_mode(raw.mode)
        || record.ruleset != RULESET
    {
        return Err(invalid_encounter_outcome(
            "canonical roll record does not match raw resolution facts",
        ));
    }
    Ok(())
}

const fn canonical_roll_mode(mode: EncounterRollMode) -> RollMode {
    match mode {
        EncounterRollMode::Normal => RollMode::Normal,
        EncounterRollMode::Advantage => RollMode::Advantage,
        EncounterRollMode::Disadvantage => RollMode::Disadvantage,
    }
}

fn roll_purpose_id(purpose: EncounterRollPurpose) -> &'static str {
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

fn invalid_encounter_outcome(reason: &'static str) -> GameCoreError {
    GameCoreError::InvalidLocalCampaignView { reason }
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
    /// Immutable campaign provenance, or the only permitted pre-game state.
    pub content_pins: CampaignPinStatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social: Option<SocialSceneViewDto>,
    pub latest_check: Option<ExplorationCheckOutcomeDto>,
    pub encounter: Option<EncounterViewDto>,
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
            || self.last_event_sequence.checked_add(1) != Some(self.revision)
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
        self.content_pins.validate()?;
        if self.content_pins.sealed().is_none()
            && (self.last_event_sequence != 0
                || self.social.is_some()
                || self.latest_check.is_some()
                || self.encounter.is_some())
        {
            return Err(GameCoreError::InvalidLocalCampaignView {
                reason: "an unsealed creator scaffold cannot contain gameplay history",
            });
        }
        if let Some(social) = &self.social {
            social.validate()?;
            if social.campaign_session_id != self.campaign_session_id
                || social.campaign_revision != self.revision
                || social.last_event_sequence != self.last_event_sequence
            {
                return Err(GameCoreError::InvalidLocalCampaignView {
                    reason: "social scene does not belong to this campaign view",
                });
            }
        }
        if self.encounter.is_some() != self.latest_check.is_some() {
            return Err(GameCoreError::InvalidLocalCampaignView {
                reason: "encounter must appear exactly when the exploration check has resolved",
            });
        }
        if let Some(encounter) = &self.encounter {
            encounter.validate()?;
            if encounter.campaign_session_id != self.campaign_session_id
                || encounter.campaign_revision != self.revision
                || encounter.last_event_sequence != self.last_event_sequence
            {
                return Err(GameCoreError::InvalidLocalCampaignView {
                    reason: "encounter does not belong to this campaign view",
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
            #[serde(default)]
            content_pins: CampaignPinStatusDto,
            #[serde(default)]
            social: Option<SocialSceneViewDto>,
            latest_check: Option<ExplorationCheckOutcomeDto>,
            encounter: Option<EncounterViewDto>,
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
            content_pins: wire.content_pins,
            social: wire.social,
            latest_check: wire.latest_check,
            encounter: wire.encounter,
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
    use crate::{
        Ability, AbilityCheck, AbilityScores, Level, Proficiency, RollContext, Sha256Digest,
        campaign_pins::{
            CAMPAIGN_PINS_SCHEMA_VERSION, CAMPAIGN_PROMPT_POLICY_ID, CAMPAIGN_PROMPT_TEMPLATE_ID,
            CampaignContentPins, CampaignPinSealReason, CampaignPromptPin, CampaignSchemaPins,
            SealedCampaignPins,
        },
        encounter::{EncounterIntent, LethalityPolicy, OpeningConsequence},
        hero::{HeroPins, ThemeId},
    };

    fn sealed_pins() -> SealedCampaignPins {
        SealedCampaignPins {
            seal_reason: CampaignPinSealReason::SelectedTheme,
            pins: CampaignContentPins {
                schema_version: CAMPAIGN_PINS_SCHEMA_VERSION,
                hero: HeroPins::mvp(ThemeId::RainboundBorough),
                prompt: CampaignPromptPin {
                    template_id: CAMPAIGN_PROMPT_TEMPLATE_ID.to_owned(),
                    template_digest: Sha256Digest::from_bytes([3; 32]),
                    policy_id: CAMPAIGN_PROMPT_POLICY_ID.to_owned(),
                },
                schemas: CampaignSchemaPins::current(),
                active_catalog_fingerprint: Sha256Digest::from_bytes([4; 32]),
            },
            legacy_source: None,
        }
    }

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

    fn social_command_json() -> serde_json::Value {
        json!({
            "schema_version": SOCIAL_INTERACTION_SCHEMA_VERSION,
            "campaign_session_id": "session-1",
            "character_id": "character-1",
            "action_id": "parley-lockkeeper",
            "expected_revision": 3,
            "idempotency_key": "social-command-1"
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

    fn ready_encounter_view(campaign_revision: u64, last_event_sequence: u64) -> EncounterViewDto {
        let state = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        EncounterViewDto {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            campaign_revision,
            last_event_sequence,
            legal_actions: player_legal_actions(&state).unwrap(),
            state,
            latest_outcome: None,
        }
    }

    fn encounter_command_json() -> serde_json::Value {
        serde_json::to_value(CommitEncounterCommand {
            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
            campaign_session_id: "session-1".to_owned(),
            expected_campaign_revision: 7,
            command: EncounterCommand::new(
                3,
                "encounter-command-1",
                EncounterIntent::StartEncounter,
            ),
        })
        .unwrap()
    }

    fn advance_npc_command_json() -> serde_json::Value {
        json!({
            "schema_version": ADVANCE_NPC_TURN_SCHEMA_VERSION,
            "campaign_session_id": "session-1",
            "expected_campaign_revision": 7,
            "expected_encounter_revision": 3,
            "idempotency_key": "npc-command-1"
        })
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
    fn social_command_round_trips_without_client_selected_mechanics() {
        let command =
            serde_json::from_value::<AttemptSocialInteractionCommand>(social_command_json())
                .expect("valid authored social intent should decode");
        command.validate().unwrap();

        let encoded = serde_json::to_value(command).unwrap();
        for forbidden in [
            "roll",
            "difficulty",
            "difficulty_class",
            "ability",
            "skill",
            "proficiency",
            "objective",
            "clock",
            "attitude",
        ] {
            assert!(encoded.get(forbidden).is_none());
        }
    }

    #[test]
    fn social_command_rejects_forged_mechanical_and_unknown_fields() {
        for (field, value) in [
            ("roll", json!(20)),
            ("difficulty_class", json!(1)),
            ("proficiency", json!("expertise")),
            ("objective", json!("auto-complete")),
            ("attitude", json!("friendly")),
            ("future_field", json!(true)),
        ] {
            let mut forged = social_command_json();
            forged
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), value);
            assert!(
                serde_json::from_value::<AttemptSocialInteractionCommand>(forged).is_err(),
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
    fn encounter_command_keeps_campaign_and_encounter_revisions_distinct() {
        let command = serde_json::from_value::<CommitEncounterCommand>(encounter_command_json())
            .expect("valid encounter intent should decode");

        assert_eq!(command.expected_campaign_revision, 7);
        assert_eq!(command.command.expected_revision, 3);
        assert!(serde_json::to_value(command).unwrap().get("roll").is_none());
    }

    #[test]
    fn npc_advance_command_is_strict_and_contains_no_selectable_mechanics() {
        let command = serde_json::from_value::<AdvanceNpcTurnCommand>(advance_npc_command_json())
            .expect("valid NPC advance should decode");
        command.validate().unwrap();
        assert_eq!(command.expected_campaign_revision, 7);
        assert_eq!(command.expected_encounter_revision, 3);

        let encoded = serde_json::to_value(command).unwrap();
        for forbidden in [
            "actor_id",
            "action_id",
            "attack_id",
            "target_id",
            "destination_feet",
            "intent",
            "roll",
            "damage",
        ] {
            assert!(encoded.get(forbidden).is_none());

            let mut forged = advance_npc_command_json();
            forged
                .as_object_mut()
                .unwrap()
                .insert(forbidden.to_owned(), json!("forged"));
            assert!(serde_json::from_value::<AdvanceNpcTurnCommand>(forged).is_err());
        }
    }

    #[test]
    fn npc_advance_command_validates_both_revisions_and_identity() {
        for (field, value) in [
            ("schema_version", json!(99)),
            ("campaign_session_id", json!("../session")),
            ("expected_campaign_revision", json!(0)),
            ("expected_encounter_revision", json!(0)),
            ("idempotency_key", json!("")),
        ] {
            let mut invalid = advance_npc_command_json();
            invalid
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), value);
            assert!(serde_json::from_value::<AdvanceNpcTurnCommand>(invalid).is_err());
        }
    }

    #[test]
    fn encounter_command_rejects_forged_mechanics_at_every_boundary() {
        for (pointer, field, value) in [
            ("", "roll", json!(20)),
            ("", "actor", json!("system")),
            ("/command", "damage", json!(999)),
            ("/command", "armor_class", json!(1)),
            ("/command/intent", "target_id", json!("forged-target")),
        ] {
            let mut forged = encounter_command_json();
            forged
                .pointer_mut(pointer)
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .insert(field.to_owned(), value);
            assert!(
                serde_json::from_value::<CommitEncounterCommand>(forged).is_err(),
                "field {field} at {pointer:?} must be rejected"
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
            content_pins: CampaignPinStatusDto::Sealed {
                evidence: Box::new(sealed_pins()),
            },
            social: None,
            latest_check: Some(outcome()),
            encounter: Some(ready_encounter_view(6, 5)),
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
            content_pins: CampaignPinStatusDto::Sealed {
                evidence: Box::new(sealed_pins()),
            },
            social: None,
            latest_check: Some(outcome()),
            encounter: Some(ready_encounter_view(4, 3)),
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
            content_pins: CampaignPinStatusDto::UnsealedCreatorScaffold,
            social: None,
            latest_check: None,
            encounter: None,
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
            content_pins: CampaignPinStatusDto::UnsealedCreatorScaffold,
            social: None,
            latest_check: None,
            encounter: None,
        })
        .unwrap();
        invalid_identity
            .as_object_mut()
            .unwrap()
            .insert("character_id".to_owned(), json!("../character"));
        assert!(serde_json::from_value::<LocalCampaignViewDto>(invalid_identity).is_err());
    }
}
