//! Pure, deterministic rules for the one Slice 1B encounter.
//!
//! This module deliberately has no clock, persistence, network, UI, or operating-system
//! randomness dependency. Callers provide intent-only commands and an injected roll source;
//! successful resolutions return the complete raw roll facts needed to build durable roll
//! records at the application boundary.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{
    ActionEconomy, D20Roll, DiceSource, GameCoreError, RollContext, RollMode,
    hero::{FeatureId, HeroClass, ResourceKind, RestKind, SpellId},
    is_valid_opaque_id,
    rules_matrix::{
        ActionContext, ConditionSet, Cover, DamageDiceResolution, DamageProfile, DeathSaveTally,
        FireBoltTarget, FireBoltTargetKind, HealthState, LightTarget, MageHandActionEffect,
        MageHandActionIntent, MageHandControlTarget, MageHandOperation, MageHandState,
        MageHandTarget, MagicMissileTarget, RULES_MATRIX_SCHEMA_VERSION, RulesMatrixError,
        RuntimeResources, ShieldTrigger, ShortRestRequest, SleepCandidate, SpellAttackResolution,
        SpellComponentAccess, SpellEffect, SpellcastingState, SupportedSpellIntent, VitalStatus,
        action_availability, advance_mage_hand_duration, grant_supported_bonus_action,
        reconcile_mage_hand_distance, resolve_mage_hand_action, resolve_supported_spell,
        spend_hit_die, take_long_rest, take_short_rest, use_action_surge, use_second_wind,
    },
};

pub const LEGACY_ENCOUNTER_SCHEMA_VERSION: u16 = 1;
pub const LIVE_V2_ENCOUNTER_SCHEMA_VERSION: u16 = 2;
pub const ENCOUNTER_SCHEMA_VERSION: u16 = 3;
pub const ENCOUNTER_RULESET_ID: &str = "srd-5.1-cc";
pub const LEGACY_ENCOUNTER_CONTENT_PACK_ID: &str = "manchester-arcana-content:v1";
pub const ENCOUNTER_CONTENT_PACK_ID: &str = "manchester-arcana-content:v2";
pub const SOOT_WIGHT_ENCOUNTER_ID: &str =
    "manchester-arcana-content:v1:encounter:soot-wight-at-viaduct";
pub const CANAL_WARDEN_ID: &str = "manchester-arcana-content:v1:hero:canal-warden";
pub const SOOT_WIGHT_ID: &str = "manchester-arcana-content:v1:creature:soot-wight";
pub const CANAL_WARDEN_ATTACK_ID: &str = "srd-5.1-cc:attack:canal-warden-longsword";
pub const SOOT_WIGHT_ATTACK_ID: &str = "manchester-arcana-content:v1:attack:soot-claw";
pub const SOOT_WIGHT_POLICY_ID: &str = "manchester-arcana-content:v1:policy:soot-wight-closed-v1";
pub const RELEASE_SLUICE_ACTION_ID: &str =
    "manchester-arcana-content:v1:context:release-cleansing-sluice";
pub const SECOND_WIND_ACTION_ID: &str = "srd-5.1-cc:feature:second-wind";
pub const ACTION_SURGE_ACTION_ID: &str = "srd-5.1-cc:feature:action-surge";
pub const DEFEAT_SOOT_WIGHT_OBJECTIVE_ID: &str =
    "manchester-arcana-content:v1:objective:defeat-soot-wight";
pub const RELEASE_SLUICE_OBJECTIVE_ID: &str =
    "manchester-arcana-content:v1:objective:release-cleansing-sluice";
pub const EXPLORATION_DESTINATION_ID: &str = "manchester-arcana-content:v1:scene:viaduct-aftermath";
pub const VIADUCT_RUNE_OBJECT_ID: &str = "manchester-arcana-content:v2:object:viaduct-rune-stone";
pub const SLUICE_LEVER_OBJECT_ID: &str =
    "manchester-arcana-content:v2:object:cleansing-sluice-lever";
pub const MAGE_HAND_ID: &str = "manchester-arcana-content:v2:effect:mage-hand";
pub const POST_ENCOUNTER_REST_BOUNDARY_ID: &str =
    "manchester-arcana-content:v2:boundary:viaduct-aftermath-safe";
pub const SHORT_REST_ACTION_ID: &str = "srd-5.1-cc:rest:short";
pub const HIT_DIE_ACTION_ID: &str = "srd-5.1-cc:rest:spend-hit-die";
pub const ARCANE_RECOVERY_ACTION_ID: &str = "srd-5.1-cc:feature:arcane-recovery";
pub const LONG_REST_ACTION_ID: &str = "srd-5.1-cc:rest:long";

const BATTLEFIELD_MIN_FEET: u16 = 0;
const BATTLEFIELD_MAX_FEET: u16 = 60;
const SLUICE_POSITION_FEET: u16 = 10;
const CONTEXT_RANGE_FEET: u16 = 5;
const HERO_MAXIMUM_HIT_POINTS: u16 = 12;
const CREATURE_MAXIMUM_HIT_POINTS: u16 = 9;
const SHORT_REST_MINUTES: u64 = 60;
const LONG_REST_MINUTES: u64 = 480;

pub type EncounterResult<T> = std::result::Result<T, EncounterError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EncounterError {
    #[error("invalid encounter command: {reason}")]
    InvalidCommand { reason: &'static str },
    #[error("invalid encounter state: {reason}")]
    InvalidState { reason: &'static str },
    #[error("command belongs to encounter `{actual}`, expected `{expected}`")]
    WrongEncounter { expected: String, actual: String },
    #[error("encounter revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("encounter revision overflowed")]
    RevisionOverflow,
    #[error("intent is not legal now: {reason}")]
    IllegalIntent { reason: &'static str },
    #[error("current actor `{current_actor_id}` is not controlled by the player")]
    PlayerControlUnavailable { current_actor_id: String },
    #[error("the deterministic Soot Wight policy is unavailable: {reason}")]
    DeterministicPolicyUnavailable { reason: &'static str },
    #[error("attack `{attack_id}` is not available to current actor `{actor_id}`")]
    AttackUnavailable { actor_id: String, attack_id: String },
    #[error("target `{target_id}` is invalid for current actor `{actor_id}`")]
    InvalidTarget { actor_id: String, target_id: String },
    #[error("target is {distance_feet} feet away; attack range is {range_feet} feet")]
    TargetOutOfRange { distance_feet: u16, range_feet: u16 },
    #[error("destination {destination_feet} is not a legal battlefield position")]
    InvalidDestination { destination_feet: u16 },
    #[error("move needs {requested_feet} feet; {remaining_feet} feet remain")]
    InsufficientMovement {
        requested_feet: u16,
        remaining_feet: u16,
    },
    #[error("roll source returned {value} for a d{sides}")]
    InvalidRoll { sides: u16, value: u16 },
    #[error("round counter overflowed")]
    RoundOverflow,
    #[error("invalid correction event: {reason}")]
    InvalidCorrection { reason: &'static str },
}

/// The application owns the deterministic stream, algorithm pin, seed reference, and cursor.
/// The rules engine asks only for bounded die values and validates every returned value.
pub trait EncounterRollSource {
    fn roll_die(&mut self, sides: u16) -> u16;
}

impl<F> EncounterRollSource for F
where
    F: FnMut(u16) -> u16,
{
    fn roll_die(&mut self, sides: u16) -> u16 {
        self(sides)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterCommand {
    pub schema_version: u16,
    pub encounter_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub intent: EncounterIntent,
}

impl EncounterCommand {
    pub fn new(
        expected_revision: u64,
        idempotency_key: impl Into<String>,
        intent: EncounterIntent,
    ) -> Self {
        Self {
            schema_version: ENCOUNTER_SCHEMA_VERSION,
            encounter_id: SOOT_WIGHT_ENCOUNTER_ID.to_owned(),
            expected_revision,
            idempotency_key: idempotency_key.into(),
            intent,
        }
    }

    pub fn validate(&self) -> EncounterResult<()> {
        if !matches!(
            self.schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION
                | LIVE_V2_ENCOUNTER_SCHEMA_VERSION
                | ENCOUNTER_SCHEMA_VERSION
        ) {
            return Err(EncounterError::InvalidCommand {
                reason: "schema version is unsupported",
            });
        }
        if self.schema_version == LEGACY_ENCOUNTER_SCHEMA_VERSION
            && matches!(
                &self.intent,
                EncounterIntent::CastSpell { .. }
                    | EncounterIntent::SecondWind
                    | EncounterIntent::ActionSurge
            )
        {
            return Err(EncounterError::InvalidCommand {
                reason: "Slice 2 encounter intents require schema version 2",
            });
        }
        if self.schema_version < ENCOUNTER_SCHEMA_VERSION
            && encounter_intent_requires_v3(&self.intent)
        {
            return Err(EncounterError::InvalidCommand {
                reason: "full Q04 encounter intents require schema version 3",
            });
        }
        if !is_valid_opaque_id(&self.encounter_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_revision == 0
        {
            return Err(EncounterError::InvalidCommand {
                reason: "identifiers must be valid and expected revision must be positive",
            });
        }
        match &self.intent {
            EncounterIntent::Attack {
                attack_id,
                target_id,
            } => {
                if !is_valid_opaque_id(attack_id) || !is_valid_opaque_id(target_id) {
                    return Err(EncounterError::InvalidCommand {
                        reason: "attack and target identifiers must be valid",
                    });
                }
            }
            EncounterIntent::ContextAction { action_id } => {
                if !is_valid_opaque_id(action_id) {
                    return Err(EncounterError::InvalidCommand {
                        reason: "context action identifier must be valid",
                    });
                }
            }
            EncounterIntent::CastSpell { target_id, .. } => {
                if !is_valid_opaque_id(target_id) {
                    return Err(EncounterError::InvalidCommand {
                        reason: "spell target identifier must be valid",
                    });
                }
            }
            EncounterIntent::CastLight { object_id }
            | EncounterIntent::CastMageHand {
                anchor_object_id: object_id,
            }
            | EncounterIntent::ControlMageHand { object_id } => {
                if !is_valid_opaque_id(object_id) {
                    return Err(EncounterError::InvalidCommand {
                        reason: "authored object identifier must be valid",
                    });
                }
            }
            EncounterIntent::StartEncounter
            | EncounterIntent::Move { .. }
            | EncounterIntent::SecondWind
            | EncounterIntent::ActionSurge
            | EncounterIntent::DismissMageHand
            | EncounterIntent::CastSleep
            | EncounterIntent::CastShield
            | EncounterIntent::DeclineReaction
            | EncounterIntent::BeginShortRest
            | EncounterIntent::SpendHitDie
            | EncounterIntent::UseArcaneRecovery
            | EncounterIntent::FinishShortRest
            | EncounterIntent::TakeLongRest
            | EncounterIntent::EndTurn
            | EncounterIntent::RollDeathSave => {}
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for EncounterCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCommand {
            schema_version: u16,
            encounter_id: String,
            expected_revision: u64,
            idempotency_key: String,
            intent: EncounterIntent,
        }

        let wire = WireCommand::deserialize(deserializer)?;
        let command = Self {
            schema_version: wire.schema_version,
            encounter_id: wire.encounter_id,
            expected_revision: wire.expected_revision,
            idempotency_key: wire.idempotency_key,
            intent: wire.intent,
        };
        command.validate().map_err(D::Error::custom)?;
        Ok(command)
    }
}

/// Player/system intent only. Actor, dice, AC, modifiers, damage, HP, rewards, and time are
/// intentionally absent and are always derived from the canonical state and fixed content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EncounterIntent {
    StartEncounter,
    Move {
        destination_feet: u16,
    },
    Attack {
        attack_id: String,
        target_id: String,
    },
    ContextAction {
        action_id: String,
    },
    CastSpell {
        spell: SpellId,
        target_id: String,
    },
    CastLight {
        object_id: String,
    },
    CastMageHand {
        anchor_object_id: String,
    },
    ControlMageHand {
        object_id: String,
    },
    DismissMageHand,
    CastSleep,
    CastShield,
    DeclineReaction,
    SecondWind,
    ActionSurge,
    BeginShortRest,
    SpendHitDie,
    UseArcaneRecovery,
    FinishShortRest,
    TakeLongRest,
    EndTurn,
    RollDeathSave,
}

fn encounter_intent_requires_v3(intent: &EncounterIntent) -> bool {
    matches!(
        intent,
        EncounterIntent::CastLight { .. }
            | EncounterIntent::CastMageHand { .. }
            | EncounterIntent::ControlMageHand { .. }
            | EncounterIntent::DismissMageHand
            | EncounterIntent::CastSleep
            | EncounterIntent::CastShield
            | EncounterIntent::DeclineReaction
            | EncounterIntent::BeginShortRest
            | EncounterIntent::SpendHitDie
            | EncounterIntent::UseArcaneRecovery
            | EncounterIntent::FinishShortRest
            | EncounterIntent::TakeLongRest
    )
}

impl<'de> Deserialize<'de> for EncounterIntent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Empty struct variants, rather than unit variants, make serde enforce
        // `deny_unknown_fields` for no-argument intents as well.
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum WireIntent {
            StartEncounter {},
            Move {
                #[serde(deserialize_with = "deserialize_form_compatible_u16")]
                destination_feet: u16,
            },
            Attack {
                attack_id: String,
                target_id: String,
            },
            ContextAction {
                action_id: String,
            },
            CastSpell {
                spell: SpellId,
                target_id: String,
            },
            CastLight {
                object_id: String,
            },
            CastMageHand {
                anchor_object_id: String,
            },
            ControlMageHand {
                object_id: String,
            },
            DismissMageHand {},
            CastSleep {},
            CastShield {},
            DeclineReaction {},
            SecondWind {},
            ActionSurge {},
            BeginShortRest {},
            SpendHitDie {},
            UseArcaneRecovery {},
            FinishShortRest {},
            TakeLongRest {},
            EndTurn {},
            RollDeathSave {},
        }

        Ok(match WireIntent::deserialize(deserializer)? {
            WireIntent::StartEncounter {} => Self::StartEncounter,
            WireIntent::Move { destination_feet } => Self::Move { destination_feet },
            WireIntent::Attack {
                attack_id,
                target_id,
            } => Self::Attack {
                attack_id,
                target_id,
            },
            WireIntent::ContextAction { action_id } => Self::ContextAction { action_id },
            WireIntent::CastSpell { spell, target_id } => Self::CastSpell { spell, target_id },
            WireIntent::CastLight { object_id } => Self::CastLight { object_id },
            WireIntent::CastMageHand { anchor_object_id } => {
                Self::CastMageHand { anchor_object_id }
            }
            WireIntent::ControlMageHand { object_id } => Self::ControlMageHand { object_id },
            WireIntent::DismissMageHand {} => Self::DismissMageHand,
            WireIntent::CastSleep {} => Self::CastSleep,
            WireIntent::CastShield {} => Self::CastShield,
            WireIntent::DeclineReaction {} => Self::DeclineReaction,
            WireIntent::SecondWind {} => Self::SecondWind,
            WireIntent::ActionSurge {} => Self::ActionSurge,
            WireIntent::BeginShortRest {} => Self::BeginShortRest,
            WireIntent::SpendHitDie {} => Self::SpendHitDie,
            WireIntent::UseArcaneRecovery {} => Self::UseArcaneRecovery,
            WireIntent::FinishShortRest {} => Self::FinishShortRest,
            WireIntent::TakeLongRest {} => Self::TakeLongRest,
            WireIntent::EndTurn {} => Self::EndTurn,
            WireIntent::RollDeathSave {} => Self::RollDeathSave,
        })
    }
}

/// HTML form transports encode scalar values as text. Keep the domain command
/// numeric while accepting the canonical decimal representation emitted by
/// Leptos' URL-encoded server-function client.
fn deserialize_form_compatible_u16<'de, D>(deserializer: D) -> std::result::Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    struct U16Visitor;

    impl<'de> serde::de::Visitor<'de> for U16Visitor {
        type Value = u16;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a u16 or its canonical unsigned decimal form")
        }

        fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            u16::try_from(value).map_err(E::custom)
        }

        fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            u16::try_from(value).map_err(E::custom)
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
                || (value.len() > 1 && value.starts_with('0'))
            {
                return Err(E::custom("expected canonical unsigned decimal u16"));
            }
            value.parse::<u16>().map_err(E::custom)
        }

        fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(U16Visitor)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LethalityPolicy {
    #[default]
    StoryRecovery,
    RulesAsWritten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpeningConsequence {
    RunesUnderstood,
    RunesMisread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatantStatusEffect {
    RuneWard,
    SootVeil,
    MagicallyAsleep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatantKind {
    Hero,
    Creature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifeStatus {
    Conscious,
    Unconscious,
    Stable,
    Dead,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeathSaves {
    pub successes: u8,
    pub failures: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitPoints {
    pub current: u16,
    pub maximum: u16,
    pub temporary: u16,
    pub death_saves: DeathSaves,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombatantState {
    pub id: String,
    pub name: String,
    pub kind: CombatantKind,
    /// Authoritative hero document that supplied this combat snapshot. Legacy
    /// Slice 1 saves omit it and retain the fixed Canal Warden profile.
    #[serde(default)]
    pub source_character_id: Option<String>,
    pub armor_class: u16,
    pub speed_feet: u16,
    pub initiative_modifier: i8,
    pub position_feet: u16,
    pub hit_points: HitPoints,
    pub life_status: LifeStatus,
    pub status_effects: Vec<CombatantStatusEffect>,
    /// Closed weapon attacks captured when the encounter first starts. An
    /// empty hero list is the backward-compatible legacy longsword profile.
    #[serde(default)]
    pub attacks: Vec<EncounterAttack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterAttack {
    pub attack_id: String,
    pub range_feet: u16,
    pub attack_modifiers: Vec<RollModifierFact>,
    pub damage_die_sides: u16,
    pub damage_modifier: RollModifierFact,
    pub damage_type: DamageType,
}

impl EncounterAttack {
    fn validate(&self) -> EncounterResult<()> {
        let modifier_ids = self
            .attack_modifiers
            .iter()
            .map(|modifier| modifier.source_id.as_str())
            .collect::<BTreeSet<_>>();
        let modifier_total = self
            .attack_modifiers
            .iter()
            .try_fold(0_i16, |total, modifier| total.checked_add(modifier.value));
        if !is_valid_opaque_id(&self.attack_id)
            || self.range_feet == 0
            || self.range_feet > 320
            || self.attack_modifiers.is_empty()
            || self.attack_modifiers.len() > 4
            || modifier_ids.len() != self.attack_modifiers.len()
            || self
                .attack_modifiers
                .iter()
                .any(|modifier| !is_valid_opaque_id(&modifier.source_id))
            || modifier_total.is_none()
            || !is_valid_opaque_id(&self.damage_modifier.source_id)
            || !matches!(self.damage_die_sides, 4 | 6 | 8 | 10 | 12)
        {
            return Err(EncounterError::InvalidState {
                reason: "encounter attack identity, range, modifiers, or damage die is invalid",
            });
        }
        Ok(())
    }
}

/// Persisted Slice 2 rules snapshot derived from the authoritative hero at the
/// moment the encounter begins. A snapshot keeps later advancement from
/// rewriting an encounter already in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterHeroRulesProfile {
    pub runtime_resources: RuntimeResources,
    pub spellcasting: Option<SpellcastingState>,
    /// Present for schema-v3 runtime recovery. Historical v2 snapshots omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constitution_modifier: Option<i8>,
}

impl EncounterHeroRulesProfile {
    pub fn validate(&self) -> EncounterResult<()> {
        self.runtime_resources
            .validate()
            .map_err(|_| EncounterError::InvalidState {
                reason: "hero runtime resources are invalid",
            })?;
        if self
            .constitution_modifier
            .is_some_and(|modifier| !(-5..=10).contains(&modifier))
        {
            return Err(EncounterError::InvalidState {
                reason: "hero constitution modifier is outside the supported range",
            });
        }
        match (self.runtime_resources.class, &self.spellcasting) {
            (HeroClass::Fighter, None) => Ok(()),
            (HeroClass::Wizard, Some(spellcasting))
                if spellcasting.caster_id == CANAL_WARDEN_ID =>
            {
                spellcasting
                    .validate()
                    .map_err(|_| EncounterError::InvalidState {
                        reason: "hero spellcasting snapshot is invalid",
                    })
            }
            _ => Err(EncounterError::InvalidState {
                reason: "hero class and spellcasting snapshot do not match",
            }),
        }
    }
}

/// Validated combat and rules snapshot derived from an authoritative created
/// hero. `rules` remains optional solely so pre-Slice-2 encounter events can be
/// replayed exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncounterHeroProfile {
    pub source_character_id: String,
    pub name: String,
    pub armor_class: u16,
    pub speed_feet: u16,
    pub initiative_modifier: i8,
    pub current_hit_points: u16,
    pub maximum_hit_points: u16,
    pub attacks: Vec<EncounterAttack>,
    pub rules: Option<EncounterHeroRulesProfile>,
}

impl EncounterHeroProfile {
    pub fn validate(&self) -> EncounterResult<()> {
        let attack_ids = self
            .attacks
            .iter()
            .map(|attack| attack.attack_id.as_str())
            .collect::<BTreeSet<_>>();
        if !is_valid_opaque_id(&self.source_character_id)
            || self.name.trim().is_empty()
            || self.name.chars().count() > 120
            || self.armor_class == 0
            || self.speed_feet == 0
            || !self.speed_feet.is_multiple_of(5)
            || self.current_hit_points == 0
            || self.current_hit_points > self.maximum_hit_points
            || self.attacks.is_empty()
            || self.attacks.len() > 8
            || attack_ids.len() != self.attacks.len()
        {
            return Err(EncounterError::InvalidState {
                reason: "authoritative hero combat profile is invalid",
            });
        }
        for attack in &self.attacks {
            attack.validate()?;
        }
        if let Some(rules) = &self.rules {
            rules.validate()?;
        }
        Ok(())
    }
}

impl CombatantState {
    fn hero_profile_fields_valid(&self) -> bool {
        self.kind == CombatantKind::Hero
            && self.source_character_id.is_some()
            && !self.attacks.is_empty()
    }

    fn validate(&self, map: &EncounterMap) -> EncounterResult<()> {
        if !is_valid_opaque_id(&self.id)
            || self.name.trim().is_empty()
            || self.name.chars().count() > 120
            || self
                .source_character_id
                .as_ref()
                .is_some_and(|id| !is_valid_opaque_id(id))
            || self.armor_class == 0
            || self.speed_feet == 0
            || self.position_feet < map.minimum_position_feet
            || self.position_feet > map.maximum_position_feet
            || !self.position_feet.is_multiple_of(5)
            || self.hit_points.maximum == 0
            || self.hit_points.current > self.hit_points.maximum
            || self.hit_points.temporary > self.hit_points.maximum
            || self.hit_points.death_saves.successes > 3
            || self.hit_points.death_saves.failures > 3
        {
            return Err(EncounterError::InvalidState {
                reason: "combatant identity, statistics, position, or hit points are invalid",
            });
        }
        let attack_ids = self
            .attacks
            .iter()
            .map(|attack| attack.attack_id.as_str())
            .collect::<BTreeSet<_>>();
        if self.attacks.len() > 8 || attack_ids.len() != self.attacks.len() {
            return Err(EncounterError::InvalidState {
                reason: "combatant attacks must be bounded and unique",
            });
        }
        for attack in &self.attacks {
            attack.validate()?;
        }
        let unique_effects = self.status_effects.iter().collect::<BTreeSet<_>>();
        if unique_effects.len() != self.status_effects.len() {
            return Err(EncounterError::InvalidState {
                reason: "combatant status effects must be unique",
            });
        }
        match self.life_status {
            LifeStatus::Conscious if self.hit_points.current == 0 => {
                return Err(EncounterError::InvalidState {
                    reason: "a conscious combatant must have positive hit points",
                });
            }
            LifeStatus::Unconscious | LifeStatus::Stable | LifeStatus::Dead
                if self.hit_points.current != 0 =>
            {
                return Err(EncounterError::InvalidState {
                    reason: "a non-conscious combatant must have zero hit points",
                });
            }
            _ => {}
        }
        if self.life_status == LifeStatus::Stable && self.hit_points.death_saves.successes != 3 {
            return Err(EncounterError::InvalidState {
                reason: "a stable hero must have three successful death saves",
            });
        }
        if self.life_status == LifeStatus::Dead
            && self.kind == CombatantKind::Hero
            && self.hit_points.death_saves.failures != 3
        {
            return Err(EncounterError::InvalidState {
                reason: "a dead hero must record three failed death saves",
            });
        }
        if self.kind == CombatantKind::Creature
            && (self.source_character_id.is_some()
                || !self.attacks.is_empty()
                || self.hit_points.death_saves != DeathSaves::default())
        {
            return Err(EncounterError::InvalidState {
                reason: "the fixed creature cannot have a hero source, custom attacks, or death saves",
            });
        }
        if self.kind == CombatantKind::Creature
            && !matches!(self.life_status, LifeStatus::Conscious | LifeStatus::Dead)
        {
            return Err(EncounterError::InvalidState {
                reason: "the fixed creature is either conscious or dead",
            });
        }
        if self.kind == CombatantKind::Hero {
            match self.life_status {
                LifeStatus::Conscious if self.hit_points.death_saves != DeathSaves::default() => {
                    return Err(EncounterError::InvalidState {
                        reason: "a conscious hero must have cleared death saves",
                    });
                }
                LifeStatus::Unconscious
                    if self.hit_points.death_saves.successes >= 3
                        || self.hit_points.death_saves.failures >= 3 =>
                {
                    return Err(EncounterError::InvalidState {
                        reason: "an unconscious hero cannot have a terminal death-save count",
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterMap {
    pub minimum_position_feet: u16,
    pub maximum_position_feet: u16,
    pub sluice_position_feet: u16,
    pub context_range_feet: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnResources {
    pub action_available: bool,
    pub movement_remaining_feet: u16,
    pub bonus_action_available: bool,
    pub reaction_available: bool,
    pub object_interaction_available: bool,
}

impl TurnResources {
    fn fresh(speed_feet: u16, rules: Option<&EncounterHeroRulesProfile>) -> EncounterResult<Self> {
        let mut economy = ActionEconomy::new(speed_feet);
        if let Some(rules) = rules {
            grant_supported_bonus_action(&rules.runtime_resources, &mut economy).map_err(|_| {
                EncounterError::InvalidState {
                    reason: "hero bonus-action resources are invalid",
                }
            })?;
        }
        Ok(Self::from_action_economy(&economy))
    }

    fn action_economy(&self) -> ActionEconomy {
        ActionEconomy {
            action_available: self.action_available,
            bonus_action_available: self.bonus_action_available,
            reaction_available: self.reaction_available,
            object_interaction_available: self.object_interaction_available,
            movement_remaining_feet: self.movement_remaining_feet,
        }
    }

    fn from_action_economy(economy: &ActionEconomy) -> Self {
        Self {
            action_available: economy.action_available,
            movement_remaining_feet: economy.movement_remaining_feet,
            bonus_action_available: economy.bonus_action_available,
            reaction_available: economy.reaction_available,
            object_interaction_available: economy.object_interaction_available,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeEntry {
    pub participant_id: String,
    pub natural_roll: u8,
    pub modifier: i8,
    pub total: i16,
    pub tie_break_rank: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitiativeTieBreaker {
    HigherModifierThenStableId,
    StableId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeTie {
    pub total: i16,
    pub participant_ids: Vec<String>,
    pub resolved_by: InitiativeTieBreaker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeState {
    pub entries: Vec<InitiativeEntry>,
    pub order: Vec<String>,
    pub ties: Vec<InitiativeTie>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveStatus {
    Pending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterObjective {
    pub objective_id: String,
    pub status: ObjectiveStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterObjectives {
    pub primary: EncounterObjective,
    pub contextual: EncounterObjective,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncounterStatus {
    Ready,
    Active,
    Victory,
    Defeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncounterRewardTier {
    /// Legacy private-save value retained for exact read compatibility.
    Minor,
    /// The one-shot MVP encounter advances a level-1 hero to level 2.
    Major,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RewardEligibility {
    Pending,
    Eligible { tier: EncounterRewardTier },
    Ineligible { reason: RewardIneligibilityReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardIneligibilityReason {
    EncounterDefeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncounterOutcome {
    Victory,
    Defeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefeatReason {
    HeroUnconscious,
    HeroStable,
    HeroDead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationTransition {
    pub destination_id: String,
    pub outcome: EncounterOutcome,
    pub defeat_reason: Option<DefeatReason>,
    pub hero_current_hit_points: u16,
    pub hero_life_status: LifeStatus,
    pub story_recovery_applied: bool,
}

/// Closed authored objects available to v3 utility spells. Mechanical shape,
/// position, weight, and magic-item status are state-owned and never accepted
/// from a browser command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterObjectState {
    pub object_id: String,
    pub position_feet: u16,
    pub maximum_dimension_feet: u8,
    pub weight_pounds: u8,
    pub is_magic_item: bool,
    pub light_remaining_rounds: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterSleepState {
    pub target_id: String,
    pub remaining_rounds: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAttackReaction {
    pub actor_id: String,
    pub target_id: String,
    pub attack_id: String,
    pub natural_roll: u8,
    pub attack_total: i32,
    pub armor_class: u16,
    pub critical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShieldExpiry {
    StartOfCasterTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShieldWardState {
    pub caster_id: String,
    pub armor_class_bonus: u8,
    pub immune_to_magic_missile: bool,
    pub expiry: ShieldExpiry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortRestState {
    pub boundary_id: String,
    pub started_at_campaign_minute: u64,
    pub completes_at_campaign_minute: u64,
    pub hit_dice_spent: u8,
    pub arcane_recovery_used: bool,
}

/// Schema-v3-only runtime state. `campaign_time_minutes` is trusted abstract
/// campaign time advanced by fixed server-authored policies; it is never wall
/// time and is never supplied by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterLiveQ04State {
    pub objects: Vec<EncounterObjectState>,
    pub mage_hand: Option<MageHandState>,
    pub mage_hand_position_feet: Option<u16>,
    pub sleep: Option<EncounterSleepState>,
    pub pending_attack_reaction: Option<PendingAttackReaction>,
    pub shield_ward: Option<ShieldWardState>,
    pub hero_reaction_available: bool,
    pub campaign_time_minutes: u64,
    pub active_short_rest: Option<ShortRestState>,
    pub last_long_rest_completed_at_campaign_minute: Option<u64>,
}

impl EncounterLiveQ04State {
    fn new() -> Self {
        Self {
            objects: vec![
                EncounterObjectState {
                    object_id: VIADUCT_RUNE_OBJECT_ID.to_owned(),
                    position_feet: 0,
                    maximum_dimension_feet: 5,
                    weight_pounds: 10,
                    is_magic_item: false,
                    light_remaining_rounds: None,
                },
                EncounterObjectState {
                    object_id: SLUICE_LEVER_OBJECT_ID.to_owned(),
                    position_feet: SLUICE_POSITION_FEET,
                    maximum_dimension_feet: 5,
                    weight_pounds: 10,
                    is_magic_item: false,
                    light_remaining_rounds: None,
                },
            ],
            mage_hand: None,
            mage_hand_position_feet: None,
            sleep: None,
            pending_attack_reaction: None,
            shield_ward: None,
            hero_reaction_available: true,
            campaign_time_minutes: 0,
            active_short_rest: None,
            last_long_rest_completed_at_campaign_minute: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterState {
    pub schema_version: u16,
    pub encounter_id: String,
    pub ruleset_id: String,
    pub content_pack_id: String,
    pub revision: u64,
    pub lethality_policy: LethalityPolicy,
    pub opening_consequence: OpeningConsequence,
    pub map: EncounterMap,
    pub hero: CombatantState,
    /// Absent only for legacy fixed-hero and pre-Slice-2 persisted encounters.
    #[serde(default)]
    pub hero_rules: Option<EncounterHeroRulesProfile>,
    pub creature: CombatantState,
    pub initiative: Option<InitiativeState>,
    pub round: u32,
    pub current_actor_index: Option<u8>,
    pub current_actor_id: Option<String>,
    pub turn_resources: Option<TurnResources>,
    pub objectives: EncounterObjectives,
    pub status: EncounterStatus,
    pub reward_eligibility: RewardEligibility,
    pub transition: Option<ExplorationTransition>,
    /// Present exactly for schema v3. Historical v1/v2 events remain byte-shape
    /// compatible and are never assigned default live mechanics on read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_q04: Option<EncounterLiveQ04State>,
}

impl EncounterState {
    /// Builds the only authored MVP encounter. The prior rune check has a real, persisted
    /// consequence: success grants the hero a 2 HP ward; failure doubles the wight's opening
    /// soot veil from 2 to 4 temporary HP.
    pub fn new(lethality_policy: LethalityPolicy, opening: OpeningConsequence) -> Self {
        let (hero_temporary, creature_temporary, hero_effects) = match opening {
            OpeningConsequence::RunesUnderstood => (2, 2, vec![CombatantStatusEffect::RuneWard]),
            OpeningConsequence::RunesMisread => (0, 4, Vec::new()),
        };
        Self {
            schema_version: ENCOUNTER_SCHEMA_VERSION,
            encounter_id: SOOT_WIGHT_ENCOUNTER_ID.to_owned(),
            ruleset_id: ENCOUNTER_RULESET_ID.to_owned(),
            content_pack_id: ENCOUNTER_CONTENT_PACK_ID.to_owned(),
            revision: 1,
            lethality_policy,
            opening_consequence: opening,
            map: EncounterMap {
                minimum_position_feet: BATTLEFIELD_MIN_FEET,
                maximum_position_feet: BATTLEFIELD_MAX_FEET,
                sluice_position_feet: SLUICE_POSITION_FEET,
                context_range_feet: CONTEXT_RANGE_FEET,
            },
            hero: CombatantState {
                id: CANAL_WARDEN_ID.to_owned(),
                name: "Canal Warden".to_owned(),
                kind: CombatantKind::Hero,
                source_character_id: None,
                armor_class: 18,
                speed_feet: 30,
                initiative_modifier: 2,
                position_feet: 0,
                hit_points: HitPoints {
                    current: HERO_MAXIMUM_HIT_POINTS,
                    maximum: HERO_MAXIMUM_HIT_POINTS,
                    temporary: hero_temporary,
                    death_saves: DeathSaves::default(),
                },
                life_status: LifeStatus::Conscious,
                status_effects: hero_effects,
                attacks: Vec::new(),
            },
            hero_rules: None,
            creature: CombatantState {
                id: SOOT_WIGHT_ID.to_owned(),
                name: "Soot Wight".to_owned(),
                kind: CombatantKind::Creature,
                source_character_id: None,
                armor_class: 12,
                speed_feet: 25,
                initiative_modifier: 1,
                position_feet: 30,
                hit_points: HitPoints {
                    current: CREATURE_MAXIMUM_HIT_POINTS,
                    maximum: CREATURE_MAXIMUM_HIT_POINTS,
                    temporary: creature_temporary,
                    death_saves: DeathSaves::default(),
                },
                life_status: LifeStatus::Conscious,
                status_effects: vec![CombatantStatusEffect::SootVeil],
                attacks: Vec::new(),
            },
            initiative: None,
            round: 0,
            current_actor_index: None,
            current_actor_id: None,
            turn_resources: None,
            objectives: EncounterObjectives {
                primary: EncounterObjective {
                    objective_id: DEFEAT_SOOT_WIGHT_OBJECTIVE_ID.to_owned(),
                    status: ObjectiveStatus::Pending,
                },
                contextual: EncounterObjective {
                    objective_id: RELEASE_SLUICE_OBJECTIVE_ID.to_owned(),
                    status: ObjectiveStatus::Pending,
                },
            },
            status: EncounterStatus::Ready,
            reward_eligibility: RewardEligibility::Pending,
            transition: None,
            live_q04: Some(EncounterLiveQ04State::new()),
        }
    }

    /// Reconstructs the exact historical envelope before replay. This does not
    /// migrate state: v1/v2 keep their original content pin and omit every v3
    /// field. Callers must still replay only commands of the same schema.
    pub fn pin_historical_schema_for_replay(&mut self, schema_version: u16) -> EncounterResult<()> {
        if !matches!(
            schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION | LIVE_V2_ENCOUNTER_SCHEMA_VERSION
        ) {
            return Err(EncounterError::InvalidState {
                reason: "historical replay schema must be version 1 or 2",
            });
        }
        self.schema_version = schema_version;
        self.content_pack_id = LEGACY_ENCOUNTER_CONTENT_PACK_ID.to_owned();
        self.live_q04 = None;
        self.validate()
    }

    /// Builds the authored encounter with a validated combat snapshot from a
    /// created hero. The snapshot is then carried by every successor state, so
    /// later hero advancement cannot rewrite an encounter already in progress.
    pub fn new_for_hero(
        lethality_policy: LethalityPolicy,
        opening: OpeningConsequence,
        profile: EncounterHeroProfile,
    ) -> EncounterResult<Self> {
        Self::new_for_hero_at_schema(lethality_policy, opening, profile, ENCOUNTER_SCHEMA_VERSION)
    }

    pub fn new_for_hero_for_historical_replay(
        lethality_policy: LethalityPolicy,
        opening: OpeningConsequence,
        profile: EncounterHeroProfile,
        schema_version: u16,
    ) -> EncounterResult<Self> {
        if !matches!(
            schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION | LIVE_V2_ENCOUNTER_SCHEMA_VERSION
        ) {
            return Err(EncounterError::InvalidState {
                reason: "historical hero replay schema must be version 1 or 2",
            });
        }
        Self::new_for_hero_at_schema(lethality_policy, opening, profile, schema_version)
    }

    fn new_for_hero_at_schema(
        lethality_policy: LethalityPolicy,
        opening: OpeningConsequence,
        profile: EncounterHeroProfile,
        schema_version: u16,
    ) -> EncounterResult<Self> {
        profile.validate()?;
        let mut state = Self::new(lethality_policy, opening);
        if schema_version != ENCOUNTER_SCHEMA_VERSION {
            state.pin_historical_schema_for_replay(schema_version)?;
        }
        state.hero.source_character_id = Some(profile.source_character_id);
        state.hero.name = profile.name;
        state.hero.armor_class = profile.armor_class;
        state.hero.speed_feet = profile.speed_feet;
        state.hero.initiative_modifier = profile.initiative_modifier;
        state.hero.hit_points.current = profile.current_hit_points;
        state.hero.hit_points.maximum = profile.maximum_hit_points;
        state.hero.attacks = profile.attacks;
        state.hero_rules = profile.rules;
        state.validate()?;
        Ok(state)
    }

    /// Returns the authoritative combat snapshot, or `None` for legacy Canal
    /// Warden encounters created before hero bridging existed.
    pub fn hero_profile(&self) -> Option<EncounterHeroProfile> {
        Some(EncounterHeroProfile {
            source_character_id: self.hero.source_character_id.clone()?,
            name: self.hero.name.clone(),
            armor_class: self.hero.armor_class,
            speed_feet: self.hero.speed_feet,
            initiative_modifier: self.hero.initiative_modifier,
            current_hit_points: self.hero.hit_points.current,
            maximum_hit_points: self.hero.hit_points.maximum,
            attacks: self.hero.attacks.clone(),
            rules: self.hero_rules.clone(),
        })
    }

    pub fn current_actor(&self) -> Option<&CombatantState> {
        match self.current_actor_id.as_deref() {
            Some(CANAL_WARDEN_ID) => Some(&self.hero),
            Some(SOOT_WIGHT_ID) => Some(&self.creature),
            _ => None,
        }
    }

    pub fn validate(&self) -> EncounterResult<()> {
        if !matches!(
            self.schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION
                | LIVE_V2_ENCOUNTER_SCHEMA_VERSION
                | ENCOUNTER_SCHEMA_VERSION
        ) || self.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
            || self.ruleset_id != ENCOUNTER_RULESET_ID
            || self.content_pack_id
                != if self.schema_version == ENCOUNTER_SCHEMA_VERSION {
                    ENCOUNTER_CONTENT_PACK_ID
                } else {
                    LEGACY_ENCOUNTER_CONTENT_PACK_ID
                }
            || self.revision == 0
            || self.map.minimum_position_feet != BATTLEFIELD_MIN_FEET
            || self.map.maximum_position_feet != BATTLEFIELD_MAX_FEET
            || self.map.sluice_position_feet != SLUICE_POSITION_FEET
            || self.map.context_range_feet != CONTEXT_RANGE_FEET
        {
            return Err(EncounterError::InvalidState {
                reason: "schema, fixed content pins, revision, or map is invalid",
            });
        }
        if self.schema_version == LEGACY_ENCOUNTER_SCHEMA_VERSION && self.hero_rules.is_some() {
            return Err(EncounterError::InvalidState {
                reason: "legacy encounter state cannot contain a Slice 2 hero rules snapshot",
            });
        }
        if self.schema_version == ENCOUNTER_SCHEMA_VERSION {
            self.validate_live_q04()?;
        } else if self.live_q04.is_some() {
            return Err(EncounterError::InvalidState {
                reason: "historical encounter state cannot contain schema-v3 live state",
            });
        }
        if self.hero.id != CANAL_WARDEN_ID
            || self.hero.kind != CombatantKind::Hero
            || self.creature.id != SOOT_WIGHT_ID
            || self.creature.kind != CombatantKind::Creature
            || self.creature.name != "Soot Wight"
            || self.creature.source_character_id.is_some()
            || !self.creature.attacks.is_empty()
            || self.creature.armor_class != 12
            || self.creature.speed_feet != 25
            || self.creature.initiative_modifier != 1
            || self.creature.hit_points.maximum != CREATURE_MAXIMUM_HIT_POINTS
            || self.objectives.primary.objective_id != DEFEAT_SOOT_WIGHT_OBJECTIVE_ID
            || self.objectives.contextual.objective_id != RELEASE_SLUICE_OBJECTIVE_ID
        {
            return Err(EncounterError::InvalidState {
                reason: "fixed participants or objectives are invalid",
            });
        }
        match self.hero.source_character_id.as_deref() {
            None if self.hero.name == "Canal Warden"
                && self.hero.armor_class == 18
                && self.hero.speed_feet == 30
                && self.hero.initiative_modifier == 2
                && self.hero.hit_points.maximum == HERO_MAXIMUM_HIT_POINTS
                && self.hero.attacks.is_empty() => {}
            Some(_) if self.hero.hero_profile_fields_valid() => {}
            _ => {
                return Err(EncounterError::InvalidState {
                    reason: "hero must be the legacy Canal Warden or an authoritative combat snapshot",
                });
            }
        }
        if let Some(rules) = &self.hero_rules {
            if self.hero.source_character_id.is_none() {
                return Err(EncounterError::InvalidState {
                    reason: "legacy fixed heroes cannot carry a created-hero rules snapshot",
                });
            }
            rules.validate()?;
            if self.schema_version == ENCOUNTER_SCHEMA_VERSION
                && rules.constitution_modifier.is_none()
            {
                return Err(EncounterError::InvalidState {
                    reason: "schema-v3 hero rules require a constitution modifier",
                });
            }
        }
        self.hero.validate(&self.map)?;
        self.creature.validate(&self.map)?;
        let has_rune_ward = self
            .hero
            .status_effects
            .contains(&CombatantStatusEffect::RuneWard);
        if has_rune_ward != (self.opening_consequence == OpeningConsequence::RunesUnderstood)
            || self.objectives.contextual.status == ObjectiveStatus::Failed
        {
            return Err(EncounterError::InvalidState {
                reason: "opening consequence or contextual objective state is invalid",
            });
        }
        let has_soot_veil = self
            .creature
            .status_effects
            .contains(&CombatantStatusEffect::SootVeil);
        match self.objectives.contextual.status {
            ObjectiveStatus::Pending if !has_soot_veil => {
                return Err(EncounterError::InvalidState {
                    reason: "a pending sluice objective requires the soot veil",
                });
            }
            ObjectiveStatus::Completed
                if has_soot_veil || self.creature.hit_points.temporary != 0 =>
            {
                return Err(EncounterError::InvalidState {
                    reason: "a completed sluice objective must remove the soot veil",
                });
            }
            _ => {}
        }
        self.validate_initiative()?;

        match self.status {
            EncounterStatus::Ready => {
                let (expected_hero_temporary, expected_creature_temporary) =
                    match self.opening_consequence {
                        OpeningConsequence::RunesUnderstood => (2, 2),
                        OpeningConsequence::RunesMisread => (0, 4),
                    };
                if self.initiative.is_some()
                    || self.round != 0
                    || self.current_actor_index.is_some()
                    || self.current_actor_id.is_some()
                    || self.turn_resources.is_some()
                    || self.objectives.primary.status != ObjectiveStatus::Pending
                    || self.objectives.contextual.status != ObjectiveStatus::Pending
                    || self.reward_eligibility != RewardEligibility::Pending
                    || self.transition.is_some()
                    || self.hero.life_status != LifeStatus::Conscious
                    || self.creature.life_status != LifeStatus::Conscious
                    || self.hero.hit_points.current != self.hero.hit_points.maximum
                    || self.creature.hit_points.current != self.creature.hit_points.maximum
                    || self.hero.hit_points.temporary != expected_hero_temporary
                    || self.creature.hit_points.temporary != expected_creature_temporary
                {
                    return Err(EncounterError::InvalidState {
                        reason: "a ready encounter contains active or completed state",
                    });
                }
            }
            EncounterStatus::Active => {
                let initiative = self
                    .initiative
                    .as_ref()
                    .ok_or(EncounterError::InvalidState {
                        reason: "an active encounter requires initiative",
                    })?;
                let index = usize::from(self.current_actor_index.ok_or(
                    EncounterError::InvalidState {
                        reason: "an active encounter requires a current actor index",
                    },
                )?);
                if self.round == 0
                    || initiative.order.get(index) != self.current_actor_id.as_ref()
                    || self.current_actor().is_none()
                    || self.creature.life_status != LifeStatus::Conscious
                    || !matches!(
                        self.hero.life_status,
                        LifeStatus::Conscious | LifeStatus::Unconscious
                    )
                    || self.objectives.primary.status != ObjectiveStatus::Pending
                    || self.reward_eligibility != RewardEligibility::Pending
                    || self.transition.is_some()
                {
                    return Err(EncounterError::InvalidState {
                        reason: "active round, actor, objective, reward, or transition is invalid",
                    });
                }
                let resources =
                    self.turn_resources
                        .as_ref()
                        .ok_or(EncounterError::InvalidState {
                            reason: "an active encounter requires turn resources",
                        })?;
                if resources.movement_remaining_feet > self.current_actor().unwrap().speed_feet {
                    return Err(EncounterError::InvalidState {
                        reason: "remaining movement exceeds the current actor's speed",
                    });
                }
                let bonus_action_reachable = self.current_actor_id.as_deref()
                    == Some(CANAL_WARDEN_ID)
                    && self.hero_rules.as_ref().is_some_and(|rules| {
                        rules
                            .runtime_resources
                            .second_wind
                            .is_some_and(|resource| resource.current > 0)
                    });
                if !resources.movement_remaining_feet.is_multiple_of(5)
                    || (resources.bonus_action_available && !bonus_action_reachable)
                    || !resources.reaction_available
                    || (!resources.object_interaction_available
                        && (self.current_actor_id.as_deref() != Some(CANAL_WARDEN_ID)
                            || self.objectives.contextual.status != ObjectiveStatus::Completed))
                {
                    return Err(EncounterError::InvalidState {
                        reason: "turn resources are not reachable in the fixed encounter",
                    });
                }
            }
            EncounterStatus::Victory => self.validate_victory()?,
            EncounterStatus::Defeat => self.validate_defeat()?,
        }
        Ok(())
    }

    fn validate_live_q04(&self) -> EncounterResult<()> {
        let live = self.live_q04.as_ref().ok_or(EncounterError::InvalidState {
            reason: "schema-v3 encounter state requires live Q04 state",
        })?;
        if live.objects.len() != 2 {
            return Err(EncounterError::InvalidState {
                reason: "live Q04 state requires the two authored objects",
            });
        }
        for (object, expected_id, expected_position) in [
            (&live.objects[0], VIADUCT_RUNE_OBJECT_ID, 0),
            (
                &live.objects[1],
                SLUICE_LEVER_OBJECT_ID,
                SLUICE_POSITION_FEET,
            ),
        ] {
            if object.object_id != expected_id
                || object.position_feet != expected_position
                || object.maximum_dimension_feet != 5
                || object.weight_pounds != 10
                || object.is_magic_item
                || object
                    .light_remaining_rounds
                    .is_some_and(|rounds| !(1..=600).contains(&rounds))
            {
                return Err(EncounterError::InvalidState {
                    reason: "authored utility-spell object state is invalid",
                });
            }
        }
        if let Some(hand) = &live.mage_hand {
            hand.validate().map_err(|_| EncounterError::InvalidState {
                reason: "persisted Mage Hand state is invalid",
            })?;
            if hand.hand_id != MAGE_HAND_ID || hand.caster_id != CANAL_WARDEN_ID {
                return Err(EncounterError::InvalidState {
                    reason: "persisted Mage Hand identity is invalid",
                });
            }
            let position = live
                .mage_hand_position_feet
                .ok_or(EncounterError::InvalidState {
                    reason: "persisted Mage Hand is missing its authored position",
                })?;
            if position < self.map.minimum_position_feet
                || position > self.map.maximum_position_feet
                || !position.is_multiple_of(5)
                || distance(self.hero.position_feet, position) != hand.distance_from_caster_feet
            {
                return Err(EncounterError::InvalidState {
                    reason: "persisted Mage Hand position and range are inconsistent",
                });
            }
        } else if live.mage_hand_position_feet.is_some() {
            return Err(EncounterError::InvalidState {
                reason: "Mage Hand position cannot outlive the hand",
            });
        }
        let creature_asleep = self
            .creature
            .status_effects
            .contains(&CombatantStatusEffect::MagicallyAsleep);
        match &live.sleep {
            Some(sleep)
                if sleep.target_id == SOOT_WIGHT_ID
                    && (1..=10).contains(&sleep.remaining_rounds)
                    && creature_asleep
                    && self.creature.life_status == LifeStatus::Conscious => {}
            None if !creature_asleep => {}
            _ => {
                return Err(EncounterError::InvalidState {
                    reason: "Sleep duration and creature condition are inconsistent",
                });
            }
        }
        if let Some(pending) = &live.pending_attack_reaction {
            let resources = self
                .turn_resources
                .as_ref()
                .ok_or(EncounterError::InvalidState {
                    reason: "a pending reaction requires active turn resources",
                })?;
            if self.status != EncounterStatus::Active
                || self.current_actor_id.as_deref() != Some(SOOT_WIGHT_ID)
                || pending.actor_id != SOOT_WIGHT_ID
                || pending.target_id != CANAL_WARDEN_ID
                || pending.attack_id != SOOT_WIGHT_ATTACK_ID
                || !(1..=20).contains(&pending.natural_roll)
                || pending.natural_roll == 1
                || (pending.natural_roll != 20
                    && pending.attack_total < i32::from(pending.armor_class))
                || pending.armor_class != self.hero.armor_class
                || pending.critical != (pending.natural_roll == 20)
                || resources.action_available
                || live.shield_ward.is_some()
            {
                return Err(EncounterError::InvalidState {
                    reason: "pending Shield reaction is not a real unresolved creature hit",
                });
            }
        }
        if let Some(ward) = &live.shield_ward
            && (ward.caster_id != CANAL_WARDEN_ID
                || ward.armor_class_bonus != 5
                || !ward.immune_to_magic_missile
                || ward.expiry != ShieldExpiry::StartOfCasterTurn
                || self.status != EncounterStatus::Active
                || self.current_actor_id.as_deref() != Some(SOOT_WIGHT_ID))
        {
            return Err(EncounterError::InvalidState {
                reason: "Shield ward duration state is invalid",
            });
        }
        if let Some(rest) = &live.active_short_rest
            && (!self.is_safe_rest_boundary()
                || rest.boundary_id != POST_ENCOUNTER_REST_BOUNDARY_ID
                || rest
                    .started_at_campaign_minute
                    .checked_add(SHORT_REST_MINUTES)
                    != Some(rest.completes_at_campaign_minute)
                || rest.completes_at_campaign_minute != live.campaign_time_minutes)
        {
            return Err(EncounterError::InvalidState {
                reason: "short rest is outside the trusted safe-boundary campaign clock",
            });
        }
        if live
            .last_long_rest_completed_at_campaign_minute
            .is_some_and(|minute| minute > live.campaign_time_minutes)
        {
            return Err(EncounterError::InvalidState {
                reason: "long-rest completion is ahead of trusted campaign time",
            });
        }
        if self.status == EncounterStatus::Ready
            && (live.mage_hand.is_some()
                || live.mage_hand_position_feet.is_some()
                || live.sleep.is_some()
                || live.pending_attack_reaction.is_some()
                || live.shield_ward.is_some()
                || live.active_short_rest.is_some()
                || live.campaign_time_minutes != 0
                || live.last_long_rest_completed_at_campaign_minute.is_some()
                || live
                    .objects
                    .iter()
                    .any(|object| object.light_remaining_rounds.is_some()))
        {
            return Err(EncounterError::InvalidState {
                reason: "a ready v3 encounter contains progressed live Q04 state",
            });
        }
        Ok(())
    }

    fn is_safe_rest_boundary(&self) -> bool {
        self.status == EncounterStatus::Victory
            || (self.status == EncounterStatus::Defeat
                && self.lethality_policy == LethalityPolicy::StoryRecovery
                && self
                    .transition
                    .as_ref()
                    .is_some_and(|transition| transition.story_recovery_applied))
    }

    fn validate_initiative(&self) -> EncounterResult<()> {
        let Some(initiative) = &self.initiative else {
            return Ok(());
        };
        if initiative.entries.len() != 2 || initiative.order.len() != 2 {
            return Err(EncounterError::InvalidState {
                reason: "initiative must contain exactly the hero and creature",
            });
        }
        for entry in &initiative.entries {
            let expected_modifier = match entry.participant_id.as_str() {
                CANAL_WARDEN_ID => self.hero.initiative_modifier,
                SOOT_WIGHT_ID => self.creature.initiative_modifier,
                _ => {
                    return Err(EncounterError::InvalidState {
                        reason: "initiative contains an unknown participant",
                    });
                }
            };
            if !matches!(entry.natural_roll, 1..=20)
                || entry.modifier != expected_modifier
                || entry.total != i16::from(entry.natural_roll) + i16::from(entry.modifier)
            {
                return Err(EncounterError::InvalidState {
                    reason: "initiative entry roll or total is invalid",
                });
            }
        }
        let mut expected_entries = initiative.entries.clone();
        sort_initiative_entries(&mut expected_entries);
        for (rank, entry) in expected_entries.iter_mut().enumerate() {
            entry.tie_break_rank = rank as u8;
        }
        let expected_order = expected_entries
            .iter()
            .map(|entry| entry.participant_id.clone())
            .collect::<Vec<_>>();
        let unique = expected_order.iter().collect::<BTreeSet<_>>();
        if unique.len() != 2
            || !unique.contains(&CANAL_WARDEN_ID.to_owned())
            || !unique.contains(&SOOT_WIGHT_ID.to_owned())
            || initiative.entries != expected_entries
            || initiative.order != expected_order
            || initiative.ties != initiative_ties(&initiative.entries)
        {
            return Err(EncounterError::InvalidState {
                reason: "initiative order or deterministic tie resolution is invalid",
            });
        }
        Ok(())
    }

    fn validate_victory(&self) -> EncounterResult<()> {
        let transition = self
            .transition
            .as_ref()
            .ok_or(EncounterError::InvalidState {
                reason: "victory requires an exploration transition",
            })?;
        if self.initiative.is_none()
            || self.current_actor_index.is_some()
            || self.current_actor_id.is_some()
            || self.turn_resources.is_some()
            || self.creature.life_status != LifeStatus::Dead
            || self.creature.hit_points.current != 0
            || self.objectives.primary.status != ObjectiveStatus::Completed
            || !matches!(
                self.reward_eligibility,
                RewardEligibility::Eligible {
                    tier: EncounterRewardTier::Minor | EncounterRewardTier::Major
                }
            )
            || transition.outcome != EncounterOutcome::Victory
            || transition.defeat_reason.is_some()
            || transition.hero_current_hit_points != self.hero.hit_points.current
            || transition.hero_life_status != self.hero.life_status
            || transition.story_recovery_applied
            || self.hero.life_status != LifeStatus::Conscious
        {
            return Err(EncounterError::InvalidState {
                reason: "victory completion, reward, or transition is invalid",
            });
        }
        validate_transition_destination(transition)
    }

    fn validate_defeat(&self) -> EncounterResult<()> {
        let transition = self
            .transition
            .as_ref()
            .ok_or(EncounterError::InvalidState {
                reason: "defeat requires an exploration transition",
            })?;
        if self.initiative.is_none()
            || self.current_actor_index.is_some()
            || self.current_actor_id.is_some()
            || self.turn_resources.is_some()
            || self.objectives.primary.status != ObjectiveStatus::Failed
            || !matches!(
                self.reward_eligibility,
                RewardEligibility::Ineligible {
                    reason: RewardIneligibilityReason::EncounterDefeat
                }
            )
            || transition.outcome != EncounterOutcome::Defeat
            || transition.defeat_reason.is_none()
        {
            return Err(EncounterError::InvalidState {
                reason: "defeat completion, reward, or transition is invalid",
            });
        }
        match self.lethality_policy {
            LethalityPolicy::StoryRecovery => {
                let recovery_entered = self.schema_version == ENCOUNTER_SCHEMA_VERSION
                    && self
                        .live_q04
                        .as_ref()
                        .is_some_and(|live| live.campaign_time_minutes > 0);
                if recovery_entered {
                    if self.hero.life_status != LifeStatus::Conscious
                        || self.hero.hit_points.current == 0
                        || transition.hero_current_hit_points != self.hero.hit_points.current
                        || transition.hero_life_status != self.hero.life_status
                        || !transition.story_recovery_applied
                    {
                        return Err(EncounterError::InvalidState {
                            reason: "post-defeat story recovery runtime is invalid",
                        });
                    }
                } else if self.hero.life_status != LifeStatus::Unconscious
                    || self.hero.hit_points.current != 0
                    || transition.hero_current_hit_points != 1
                    || transition.hero_life_status != LifeStatus::Conscious
                    || !transition.story_recovery_applied
                {
                    return Err(EncounterError::InvalidState {
                        reason: "story recovery must preserve defeat and prepare a 1 HP hero",
                    });
                }
            }
            LethalityPolicy::RulesAsWritten => {
                if !matches!(self.hero.life_status, LifeStatus::Stable | LifeStatus::Dead)
                    || transition.hero_current_hit_points != self.hero.hit_points.current
                    || transition.hero_life_status != self.hero.life_status
                    || transition.story_recovery_applied
                {
                    return Err(EncounterError::InvalidState {
                        reason: "rules-as-written defeat must preserve the terminal health state",
                    });
                }
            }
        }
        validate_transition_destination(transition)
    }
}

fn validate_transition_destination(transition: &ExplorationTransition) -> EncounterResult<()> {
    if transition.destination_id != EXPLORATION_DESTINATION_ID {
        Err(EncounterError::InvalidState {
            reason: "exploration transition destination is invalid",
        })
    } else {
        Ok(())
    }
}

fn sort_initiative_entries(entries: &mut [InitiativeEntry]) {
    entries.sort_by(|left, right| {
        right
            .total
            .cmp(&left.total)
            .then_with(|| right.modifier.cmp(&left.modifier))
            .then_with(|| left.participant_id.cmp(&right.participant_id))
    });
}

fn initiative_ties(entries: &[InitiativeEntry]) -> Vec<InitiativeTie> {
    if entries.len() == 2 && entries[0].total == entries[1].total {
        let resolved_by = if entries[0].modifier == entries[1].modifier {
            InitiativeTieBreaker::StableId
        } else {
            InitiativeTieBreaker::HigherModifierThenStableId
        };
        vec![InitiativeTie {
            total: entries[0].total,
            participant_ids: entries
                .iter()
                .map(|entry| entry.participant_id.clone())
                .collect(),
            resolved_by,
        }]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LegalEncounterAction {
    StartEncounter,
    Move {
        minimum_destination_feet: u16,
        maximum_destination_feet: u16,
        movement_remaining_feet: u16,
    },
    Attack {
        attack_id: String,
        target_id: String,
        range_feet: u16,
    },
    ContextAction {
        action_id: String,
    },
    CastSpell {
        spell: SpellId,
        target_id: String,
        range_feet: u16,
    },
    CastLight {
        object_id: String,
    },
    CastMageHand {
        anchor_object_id: String,
    },
    ControlMageHand {
        object_id: String,
    },
    DismissMageHand,
    CastSleep,
    CastShield,
    DeclineReaction,
    SecondWind,
    ActionSurge,
    BeginShortRest,
    SpendHitDie,
    UseArcaneRecovery,
    FinishShortRest,
    TakeLongRest,
    EndTurn,
    RollDeathSave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncounterRollMode {
    Normal,
    Advantage,
    Disadvantage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncounterRollPurpose {
    Initiative,
    Attack,
    Damage,
    Healing,
    SleepHitPoints,
    HitDie,
    DeathSave,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDieFact {
    pub sides: u16,
    pub value: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollModifierFact {
    pub source_id: String,
    pub value: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollComparisonKind {
    ArmorClass,
    DeathSaveDifficultyClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollComparison {
    pub kind: RollComparisonKind,
    pub value: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackOutcome {
    AutomaticMiss,
    Miss,
    Hit,
    CriticalHit,
}

impl AttackOutcome {
    pub const fn is_hit(self) -> bool {
        matches!(self, Self::Hit | Self::CriticalHit)
    }

    pub const fn is_critical(self) -> bool {
        matches!(self, Self::CriticalHit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawRollOutcome {
    Total,
    AutomaticMiss,
    Miss,
    Hit,
    CriticalHit,
    Success,
    Failure,
    NaturalOneFailure,
    NaturalTwentyRecovery,
}

/// Mechanical roll data before the application adds algorithm, seed-reference, and cursor
/// metadata. Every die, kept index, modifier component, comparison, and result is explicit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRollFacts {
    pub sequence: u16,
    pub purpose: EncounterRollPurpose,
    pub actor_id: String,
    pub target_id: Option<String>,
    pub action_id: Option<String>,
    pub expression: String,
    pub mode: EncounterRollMode,
    pub individual_dice: Vec<RawDieFact>,
    pub kept_die_indices: Vec<u16>,
    pub modifiers: Vec<RollModifierFact>,
    pub natural_d20: Option<u8>,
    pub total: i32,
    pub comparison: Option<RollComparison>,
    pub outcome: RawRollOutcome,
}

impl RawRollFacts {
    pub fn validate(&self) -> EncounterResult<()> {
        if self.sequence == 0
            || !is_valid_opaque_id(&self.actor_id)
            || self
                .target_id
                .as_ref()
                .is_some_and(|id| !is_valid_opaque_id(id))
            || self
                .action_id
                .as_ref()
                .is_some_and(|id| !is_valid_opaque_id(id))
            || self.expression.trim().is_empty()
            || self.individual_dice.is_empty()
            || self
                .individual_dice
                .iter()
                .any(|die| die.sides == 0 || die.value == 0 || die.value > die.sides)
        {
            return Err(EncounterError::InvalidState {
                reason: "raw roll identity, expression, or die values are invalid",
            });
        }
        let unique_kept = self.kept_die_indices.iter().collect::<BTreeSet<_>>();
        let dice_count = u16::try_from(self.individual_dice.len()).map_err(|_| {
            EncounterError::InvalidState {
                reason: "raw roll has too many dice",
            }
        })?;
        let expected_normal_kept = (0..dice_count).collect::<Vec<_>>();
        if unique_kept.len() != self.kept_die_indices.len()
            || self
                .kept_die_indices
                .iter()
                .any(|index| usize::from(*index) >= self.individual_dice.len())
            || self
                .individual_dice
                .iter()
                .any(|die| die.sides != self.individual_dice[0].sides)
        {
            return Err(EncounterError::InvalidState {
                reason: "raw roll kept-die indices or die sizes are invalid",
            });
        }
        match self.mode {
            EncounterRollMode::Normal if self.kept_die_indices == expected_normal_kept => {}
            EncounterRollMode::Advantage | EncounterRollMode::Disadvantage
                if self.individual_dice.len() == 2
                    && self.individual_dice[0].sides == 20
                    && self.kept_die_indices.len() == 1 =>
            {
                let first = self.individual_dice[0].value;
                let second = self.individual_dice[1].value;
                let expected_value = match self.mode {
                    EncounterRollMode::Advantage => first.max(second),
                    EncounterRollMode::Disadvantage => first.min(second),
                    EncounterRollMode::Normal => unreachable!(),
                };
                let kept_value = self.individual_dice[usize::from(self.kept_die_indices[0])].value;
                if kept_value != expected_value {
                    return Err(EncounterError::InvalidState {
                        reason: "advantage or disadvantage kept the wrong d20",
                    });
                }
            }
            _ => {
                return Err(EncounterError::InvalidState {
                    reason: "raw roll mode and kept dice are inconsistent",
                });
            }
        }
        let kept_total = self
            .kept_die_indices
            .iter()
            .try_fold(0_i32, |total, index| {
                total.checked_add(i32::from(self.individual_dice[usize::from(*index)].value))
            });
        let modifier_total = self.modifiers.iter().try_fold(0_i32, |total, modifier| {
            total.checked_add(i32::from(modifier.value))
        });
        let modifier_ids = self
            .modifiers
            .iter()
            .map(|modifier| &modifier.source_id)
            .collect::<BTreeSet<_>>();
        if modifier_ids.len() != self.modifiers.len()
            || self
                .modifiers
                .iter()
                .any(|modifier| !is_valid_opaque_id(&modifier.source_id))
        {
            return Err(EncounterError::InvalidState {
                reason: "raw roll modifier identifiers must be valid and unique",
            });
        }
        if kept_total.and_then(|dice| modifier_total.and_then(|mods| dice.checked_add(mods)))
            != Some(self.total)
        {
            return Err(EncounterError::InvalidState {
                reason: "raw roll total does not match kept dice and modifiers",
            });
        }
        let modifier_total = modifier_total.ok_or(EncounterError::InvalidState {
            reason: "raw roll modifiers overflowed",
        })?;
        if self.expression
            != dice_expression(dice_count, self.individual_dice[0].sides, modifier_total)
        {
            return Err(EncounterError::InvalidState {
                reason: "raw roll expression does not match dice and modifiers",
            });
        }
        if let Some(natural) = self.natural_d20 {
            let kept = self
                .kept_die_indices
                .first()
                .and_then(|index| self.individual_dice.get(usize::from(*index)));
            if kept
                != Some(&RawDieFact {
                    sides: 20,
                    value: u16::from(natural),
                })
            {
                return Err(EncounterError::InvalidState {
                    reason: "natural d20 does not match the kept d20 value",
                });
            }
        }
        match self.purpose {
            EncounterRollPurpose::Initiative
                if self.target_id.is_none()
                    && self.action_id.is_none()
                    && self.individual_dice.len() == 1
                    && self.natural_d20.is_some() => {}
            EncounterRollPurpose::Attack
                if self.target_id.is_some()
                    && self.action_id.is_some()
                    && matches!(self.individual_dice.len(), 1 | 2)
                    && self.natural_d20.is_some() => {}
            EncounterRollPurpose::Damage
                if self.target_id.is_some()
                    && self.action_id.is_some()
                    && matches!(self.individual_dice.len(), 1 | 2)
                    && self.natural_d20.is_none() => {}
            EncounterRollPurpose::Healing
                if self.target_id.as_deref() == Some(self.actor_id.as_str())
                    && self.action_id.is_some()
                    && self.individual_dice.len() == 1
                    && self.natural_d20.is_none() => {}
            EncounterRollPurpose::SleepHitPoints
                if self.target_id.is_some()
                    && self.action_id.as_deref() == Some(SpellId::Sleep.mechanic_id())
                    && self.individual_dice.len() == 5
                    && self.natural_d20.is_none() => {}
            EncounterRollPurpose::HitDie
                if self.target_id.as_deref() == Some(self.actor_id.as_str())
                    && self.action_id.as_deref() == Some(HIT_DIE_ACTION_ID)
                    && self.individual_dice.len() == 1
                    && self.natural_d20.is_none() => {}
            EncounterRollPurpose::DeathSave
                if self.target_id.as_deref() == Some(self.actor_id.as_str())
                    && self.action_id.is_none()
                    && self.individual_dice.len() == 1
                    && self.natural_d20.is_some() => {}
            _ => {
                return Err(EncounterError::InvalidState {
                    reason: "raw roll actor, target, action, or dice shape is invalid",
                });
            }
        }
        self.validate_outcome()
    }

    fn validate_outcome(&self) -> EncounterResult<()> {
        match (self.purpose, self.comparison.as_ref(), self.natural_d20) {
            (EncounterRollPurpose::Attack, Some(comparison), Some(natural))
                if comparison.kind == RollComparisonKind::ArmorClass =>
            {
                let expected = match natural {
                    1 => RawRollOutcome::AutomaticMiss,
                    20 => RawRollOutcome::CriticalHit,
                    _ if self.total >= i32::from(comparison.value) => RawRollOutcome::Hit,
                    _ => RawRollOutcome::Miss,
                };
                if self.outcome != expected {
                    return Err(EncounterError::InvalidState {
                        reason: "raw attack roll outcome is inconsistent",
                    });
                }
            }
            (EncounterRollPurpose::DeathSave, Some(comparison), Some(natural))
                if comparison.kind == RollComparisonKind::DeathSaveDifficultyClass =>
            {
                let expected = match natural {
                    1 => RawRollOutcome::NaturalOneFailure,
                    20 => RawRollOutcome::NaturalTwentyRecovery,
                    _ if self.total >= i32::from(comparison.value) => RawRollOutcome::Success,
                    _ => RawRollOutcome::Failure,
                };
                if self.outcome != expected {
                    return Err(EncounterError::InvalidState {
                        reason: "raw death-save outcome is inconsistent",
                    });
                }
            }
            (
                EncounterRollPurpose::Initiative
                | EncounterRollPurpose::Damage
                | EncounterRollPurpose::Healing
                | EncounterRollPurpose::SleepHitPoints
                | EncounterRollPurpose::HitDie,
                None,
                _,
            ) if self.outcome == RawRollOutcome::Total => {}
            _ => {
                return Err(EncounterError::InvalidState {
                    reason: "raw roll purpose, comparison, or outcome is inconsistent",
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageType {
    Bludgeoning,
    Piercing,
    Slashing,
    Necrotic,
    Fire,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepEndReason {
    Damaged,
    DurationExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EncounterFact {
    EncounterStarted {
        round: u32,
        initiative_order: Vec<String>,
        current_actor_id: String,
    },
    Moved {
        actor_id: String,
        from_feet: u16,
        to_feet: u16,
        movement_spent_feet: u16,
        movement_remaining_feet: u16,
    },
    AttackResolved {
        actor_id: String,
        target_id: String,
        attack_id: String,
        distance_feet: u16,
        range_feet: u16,
        armor_class: u16,
        attack_total: i32,
        outcome: AttackOutcome,
    },
    DamageApplied {
        actor_id: String,
        target_id: String,
        attack_id: String,
        damage_type: DamageType,
        critical: bool,
        amount: u16,
        temporary_hit_points_before: u16,
        temporary_hit_points_absorbed: u16,
        temporary_hit_points_after: u16,
        current_hit_points_before: u16,
        current_hit_points_after: u16,
    },
    SpellCastResolved {
        actor_id: String,
        target_id: String,
        spell: SpellId,
        level_one_spell_slots_before: Option<u8>,
        level_one_spell_slots_after: Option<u8>,
        damage_applied: u16,
    },
    LightApplied {
        actor_id: String,
        object_id: String,
        duration_rounds: u16,
    },
    LightExpired {
        object_id: String,
    },
    MageHandCreated {
        actor_id: String,
        hand_id: String,
        anchor_object_id: String,
        distance_from_caster_feet: u16,
        duration_rounds: u16,
    },
    MageHandControlled {
        actor_id: String,
        hand_id: String,
        object_id: String,
        operation: MageHandOperation,
        resulting_distance_from_caster_feet: u16,
    },
    MageHandDismissed {
        actor_id: String,
        hand_id: String,
    },
    MageHandExpired {
        hand_id: String,
    },
    SleepResolved {
        actor_id: String,
        hit_point_pool: u16,
        ordered_target_ids: Vec<String>,
        affected_target_ids: Vec<String>,
        duration_rounds: u16,
    },
    SleepEnded {
        target_id: String,
        reason: SleepEndReason,
    },
    AttackReactionOpened {
        actor_id: String,
        target_id: String,
        attack_id: String,
        natural_roll: u8,
        attack_total: i32,
        armor_class: u16,
    },
    ShieldResolved {
        actor_id: String,
        triggering_attack_id: String,
        triggering_natural_roll: u8,
        triggering_attack_total: i32,
        armor_class_before: u16,
        armor_class_with_shield: u16,
        negated_trigger: bool,
        level_one_spell_slots_before: u8,
        level_one_spell_slots_after: u8,
    },
    AttackReactionDeclined {
        actor_id: String,
        triggering_attack_id: String,
    },
    ClassFeatureResolved {
        actor_id: String,
        feature: FeatureId,
        resource: ResourceKind,
        resource_before: u8,
        resource_after: u8,
        action_available_after: bool,
        bonus_action_available_after: bool,
    },
    HealingApplied {
        actor_id: String,
        feature: FeatureId,
        requested_healing: u16,
        effective_healing: u16,
        current_hit_points_before: u16,
        current_hit_points_after: u16,
    },
    RestStarted {
        actor_id: String,
        kind: RestKind,
        boundary_id: String,
        started_at_campaign_minute: u64,
        completes_at_campaign_minute: u64,
    },
    HitDieSpent {
        actor_id: String,
        hit_die: ResourceKind,
        roll: u8,
        constitution_modifier: i8,
        healing_total: u16,
        effective_healing: u16,
        hit_dice_before: u8,
        hit_dice_after: u8,
    },
    ArcaneRecoveryApplied {
        actor_id: String,
        spell_slots_before: u8,
        spell_slots_after: u8,
        resource_before: u8,
        resource_after: u8,
    },
    RestCompleted {
        actor_id: String,
        kind: RestKind,
        boundary_id: String,
        completed_at_campaign_minute: u64,
        hit_points_recovered: u16,
        hit_dice_recovered: u8,
        spell_slots_recovered: u8,
    },
    LifeStatusChanged {
        participant_id: String,
        from: LifeStatus,
        to: LifeStatus,
        death_save_successes: u8,
        death_save_failures: u8,
    },
    ContextActionResolved {
        actor_id: String,
        action_id: String,
        removed_temporary_hit_points: u16,
        objective_id: String,
    },
    DeathSaveResolved {
        actor_id: String,
        natural_roll: u8,
        successes_before: u8,
        successes_after: u8,
        failures_before: u8,
        failures_after: u8,
        life_status_after: LifeStatus,
    },
    TurnEnded {
        actor_id: String,
        next_actor_id: String,
        round: u32,
    },
    EncounterCompleted {
        outcome: EncounterOutcome,
        defeat_reason: Option<DefeatReason>,
        reward_eligible: bool,
        story_recovery_applied: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterministicNarration {
    pub narration_id: String,
    pub authored_text: String,
    pub fallback_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterResolution {
    pub schema_version: u16,
    pub encounter_id: String,
    pub previous_revision: u64,
    pub result_revision: u64,
    pub state: EncounterState,
    pub rolls: Vec<RawRollFacts>,
    pub facts: Vec<EncounterFact>,
    pub narration: DeterministicNarration,
}

impl EncounterResolution {
    pub fn validate(&self) -> EncounterResult<()> {
        if !matches!(
            self.schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION
                | LIVE_V2_ENCOUNTER_SCHEMA_VERSION
                | ENCOUNTER_SCHEMA_VERSION
        ) || self.state.schema_version != self.schema_version
            || self.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
            || self.previous_revision == 0
            || self.previous_revision.checked_add(1) != Some(self.result_revision)
            || self.state.encounter_id != self.encounter_id
            || self.state.revision != self.result_revision
            || self.facts.is_empty()
            || !is_valid_opaque_id(&self.narration.narration_id)
            || self.narration.authored_text.trim().is_empty()
            || self.narration.fallback_text.trim().is_empty()
            || self.narration.authored_text.chars().count() > 4_000
            || self.narration.fallback_text.chars().count() > 1_000
        {
            return Err(EncounterError::InvalidState {
                reason: "encounter resolution envelope or narration is invalid",
            });
        }
        if self.schema_version == LEGACY_ENCOUNTER_SCHEMA_VERSION
            && (self.rolls.iter().any(|roll| {
                roll.purpose == EncounterRollPurpose::Healing
                    || roll.mode != EncounterRollMode::Normal
            }) || self.facts.iter().any(encounter_fact_requires_v2))
        {
            return Err(EncounterError::InvalidState {
                reason: "legacy encounter resolution contains Slice 2 mechanics",
            });
        }
        if self.schema_version < ENCOUNTER_SCHEMA_VERSION
            && (self.rolls.iter().any(|roll| {
                matches!(
                    roll.purpose,
                    EncounterRollPurpose::SleepHitPoints | EncounterRollPurpose::HitDie
                )
            }) || self.facts.iter().any(encounter_fact_requires_v3))
        {
            return Err(EncounterError::InvalidState {
                reason: "historical encounter resolution contains schema-v3 mechanics",
            });
        }
        self.state.validate()?;
        for (index, roll) in self.rolls.iter().enumerate() {
            if roll.sequence
                != u16::try_from(index + 1).map_err(|_| EncounterError::InvalidState {
                    reason: "too many raw rolls in one resolution",
                })?
            {
                return Err(EncounterError::InvalidState {
                    reason: "raw rolls are not contiguously sequenced",
                });
            }
            roll.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EncounterCorrectionEvent {
    pub schema_version: u16,
    pub correction_id: String,
    pub encounter_id: String,
    pub previous_revision: u64,
    pub result_revision: u64,
    pub reason: String,
    pub corrected_state: EncounterState,
}

impl EncounterCorrectionEvent {
    pub fn validate(&self) -> EncounterResult<()> {
        if !matches!(
            self.schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION
                | LIVE_V2_ENCOUNTER_SCHEMA_VERSION
                | ENCOUNTER_SCHEMA_VERSION
        ) || self.corrected_state.schema_version != self.schema_version
            || !is_valid_opaque_id(&self.correction_id)
            || self.encounter_id != SOOT_WIGHT_ENCOUNTER_ID
            || self.previous_revision == 0
            || self.previous_revision.checked_add(1) != Some(self.result_revision)
            || self.reason.trim().is_empty()
            || self.reason.chars().count() > 1_000
            || self.corrected_state.encounter_id != self.encounter_id
            || self.corrected_state.revision != self.result_revision
        {
            return Err(EncounterError::InvalidCorrection {
                reason: "schema, identity, revisions, reason, or corrected state is invalid",
            });
        }
        self.corrected_state.validate()
    }
}

fn encounter_fact_requires_v2(fact: &EncounterFact) -> bool {
    matches!(
        fact,
        EncounterFact::SpellCastResolved { .. }
            | EncounterFact::ClassFeatureResolved { .. }
            | EncounterFact::HealingApplied { .. }
            | EncounterFact::DamageApplied {
                damage_type: DamageType::Fire | DamageType::Force,
                ..
            }
    )
}

fn encounter_fact_requires_v3(fact: &EncounterFact) -> bool {
    matches!(
        fact,
        EncounterFact::LightApplied { .. }
            | EncounterFact::LightExpired { .. }
            | EncounterFact::MageHandCreated { .. }
            | EncounterFact::MageHandControlled { .. }
            | EncounterFact::MageHandDismissed { .. }
            | EncounterFact::MageHandExpired { .. }
            | EncounterFact::SleepResolved { .. }
            | EncounterFact::SleepEnded { .. }
            | EncounterFact::AttackReactionOpened { .. }
            | EncounterFact::ShieldResolved { .. }
            | EncounterFact::AttackReactionDeclined { .. }
            | EncounterFact::RestStarted { .. }
            | EncounterFact::HitDieSpent { .. }
            | EncounterFact::ArcaneRecoveryApplied { .. }
            | EncounterFact::RestCompleted { .. }
    )
}

impl<'de> Deserialize<'de> for EncounterCorrectionEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCorrection {
            schema_version: u16,
            correction_id: String,
            encounter_id: String,
            previous_revision: u64,
            result_revision: u64,
            reason: String,
            corrected_state: EncounterState,
        }

        let wire = WireCorrection::deserialize(deserializer)?;
        let correction = EncounterCorrectionEvent {
            schema_version: wire.schema_version,
            correction_id: wire.correction_id,
            encounter_id: wire.encounter_id,
            previous_revision: wire.previous_revision,
            result_revision: wire.result_revision,
            reason: wire.reason,
            corrected_state: wire.corrected_state,
        };
        correction.validate().map_err(D::Error::custom)?;
        Ok(correction)
    }
}

#[derive(Debug, Clone, Copy)]
struct ModifierDefinition {
    source_id: &'static str,
    value: i16,
}

#[derive(Debug, Clone, Copy)]
struct AttackDefinition {
    attack_id: &'static str,
    range_feet: u16,
    attack_modifiers: &'static [ModifierDefinition],
    damage_die_sides: u16,
    damage_modifier: ModifierDefinition,
    damage_type: DamageType,
}

const HERO_ATTACK_MODIFIERS: [ModifierDefinition; 2] = [
    ModifierDefinition {
        source_id: "srd-5.1-cc:modifier:strength",
        value: 3,
    },
    ModifierDefinition {
        source_id: "srd-5.1-cc:modifier:proficiency",
        value: 2,
    },
];
const CREATURE_ATTACK_MODIFIERS: [ModifierDefinition; 2] = [
    ModifierDefinition {
        source_id: "manchester-arcana-content:v1:modifier:soot-wight-dexterity",
        value: 1,
    },
    ModifierDefinition {
        source_id: "srd-5.1-cc:modifier:proficiency",
        value: 2,
    },
];
const HERO_ATTACK: AttackDefinition = AttackDefinition {
    attack_id: CANAL_WARDEN_ATTACK_ID,
    range_feet: 5,
    attack_modifiers: &HERO_ATTACK_MODIFIERS,
    damage_die_sides: 8,
    damage_modifier: ModifierDefinition {
        source_id: "srd-5.1-cc:modifier:strength-damage",
        value: 3,
    },
    damage_type: DamageType::Slashing,
};
const CREATURE_ATTACK: AttackDefinition = AttackDefinition {
    attack_id: SOOT_WIGHT_ATTACK_ID,
    range_feet: 5,
    attack_modifiers: &CREATURE_ATTACK_MODIFIERS,
    damage_die_sides: 6,
    damage_modifier: ModifierDefinition {
        source_id: "manchester-arcana-content:v1:modifier:soot-wight-damage",
        value: 1,
    },
    damage_type: DamageType::Necrotic,
};

#[derive(Debug, Clone, Copy)]
enum ResolvedAttack<'a> {
    Fixed(AttackDefinition),
    Snapshot(&'a EncounterAttack),
}

impl<'a> ResolvedAttack<'a> {
    fn attack_id(self) -> &'a str {
        match self {
            Self::Fixed(attack) => attack.attack_id,
            Self::Snapshot(attack) => &attack.attack_id,
        }
    }

    const fn range_feet(self) -> u16 {
        match self {
            Self::Fixed(attack) => attack.range_feet,
            Self::Snapshot(attack) => attack.range_feet,
        }
    }

    fn attack_modifiers(self) -> Vec<RollModifierFact> {
        match self {
            Self::Fixed(attack) => attack.attack_modifiers.iter().map(modifier_fact).collect(),
            Self::Snapshot(attack) => attack.attack_modifiers.clone(),
        }
    }

    const fn damage_die_sides(self) -> u16 {
        match self {
            Self::Fixed(attack) => attack.damage_die_sides,
            Self::Snapshot(attack) => attack.damage_die_sides,
        }
    }

    fn damage_modifier(self) -> RollModifierFact {
        match self {
            Self::Fixed(attack) => modifier_fact(&attack.damage_modifier),
            Self::Snapshot(attack) => attack.damage_modifier.clone(),
        }
    }

    const fn damage_type(self) -> DamageType {
        match self {
            Self::Fixed(attack) => attack.damage_type,
            Self::Snapshot(attack) => attack.damage_type,
        }
    }
}

pub fn legal_actions(state: &EncounterState) -> EncounterResult<Vec<LegalEncounterAction>> {
    state.validate()?;
    if state.schema_version == ENCOUNTER_SCHEMA_VERSION
        && state
            .live_q04
            .as_ref()
            .is_some_and(|live| live.pending_attack_reaction.is_some())
    {
        let mut reactions = Vec::new();
        if shield_reaction_available(state) {
            reactions.push(LegalEncounterAction::CastShield);
        }
        reactions.push(LegalEncounterAction::DeclineReaction);
        return Ok(reactions);
    }
    match state.status {
        EncounterStatus::Ready => return Ok(vec![LegalEncounterAction::StartEncounter]),
        EncounterStatus::Victory | EncounterStatus::Defeat => {
            return rest_legal_actions(state);
        }
        EncounterStatus::Active => {}
    }

    let actor = state.current_actor().ok_or(EncounterError::InvalidState {
        reason: "active state has no current actor",
    })?;
    if actor.life_status == LifeStatus::Unconscious {
        return Ok(vec![LegalEncounterAction::RollDeathSave]);
    }
    if actor.life_status != LifeStatus::Conscious {
        return Err(EncounterError::InvalidState {
            reason: "an active current actor is neither conscious nor making death saves",
        });
    }
    if actor.id == SOOT_WIGHT_ID
        && state
            .live_q04
            .as_ref()
            .is_some_and(|live| live.sleep.is_some())
    {
        return Ok(vec![LegalEncounterAction::EndTurn]);
    }
    let resources = state
        .turn_resources
        .as_ref()
        .ok_or(EncounterError::InvalidState {
            reason: "active state has no turn resources",
        })?;
    let mut actions = Vec::new();
    if resources.movement_remaining_feet > 0 {
        actions.push(LegalEncounterAction::Move {
            minimum_destination_feet: actor
                .position_feet
                .saturating_sub(resources.movement_remaining_feet)
                .max(state.map.minimum_position_feet),
            maximum_destination_feet: actor
                .position_feet
                .saturating_add(resources.movement_remaining_feet)
                .min(state.map.maximum_position_feet),
            movement_remaining_feet: resources.movement_remaining_feet,
        });
    }
    let attacks = attacks_for_actor(state, &actor.id).ok_or(EncounterError::InvalidState {
        reason: "current actor has no fixed encounter attack",
    })?;
    let target = opposing_combatant(state, &actor.id).ok_or(EncounterError::InvalidState {
        reason: "current actor has no opposing target",
    })?;
    if resources.action_available && target.life_status != LifeStatus::Dead {
        for attack in attacks {
            if distance(actor.position_feet, target.position_feet) <= attack.range_feet() {
                actions.push(LegalEncounterAction::Attack {
                    attack_id: attack.attack_id().to_owned(),
                    target_id: target.id.clone(),
                    range_feet: attack.range_feet(),
                });
            }
        }
    }
    if actor.id == CANAL_WARDEN_ID
        && target.life_status != LifeStatus::Dead
        && let Some(rules) = &state.hero_rules
    {
        let economy = resources.action_economy();
        let conditions = ConditionSet::empty();
        if let Some(spellcasting) = &rules.spellcasting {
            for spell in [SpellId::FireBolt, SpellId::MagicMissile] {
                let range_feet = 120;
                let prepared = if spell.level() == 0 {
                    spellcasting.cantrips.contains(&spell)
                } else {
                    spellcasting.prepared.contains(&spell)
                };
                let availability = action_availability(
                    &economy,
                    &conditions,
                    &ActionContext::CastSpell {
                        spell,
                        target_is_valid: distance(actor.position_feet, target.position_feet)
                            <= range_feet,
                        prepared,
                        slot_available: rules.runtime_resources.has_spell_slot(),
                    },
                )
                .map_err(|_| EncounterError::InvalidState {
                    reason: "spell action availability could not be derived",
                })?;
                if availability.is_available() {
                    actions.push(LegalEncounterAction::CastSpell {
                        spell,
                        target_id: target.id.clone(),
                        range_feet,
                    });
                }
            }
            if state.schema_version == ENCOUNTER_SCHEMA_VERSION {
                let live = state
                    .live_q04
                    .as_ref()
                    .expect("schema-v3 state has live Q04 state");
                for object in &live.objects {
                    let object_distance = distance(actor.position_feet, object.position_feet);
                    if spellcasting.cantrips.contains(&SpellId::Light)
                        && object_distance <= 5
                        && action_availability(
                            &economy,
                            &conditions,
                            &ActionContext::CastSpell {
                                spell: SpellId::Light,
                                target_is_valid: true,
                                prepared: true,
                                slot_available: true,
                            },
                        )
                        .map_err(|_| EncounterError::InvalidState {
                            reason: "Light availability could not be derived",
                        })?
                        .is_available()
                    {
                        actions.push(LegalEncounterAction::CastLight {
                            object_id: object.object_id.clone(),
                        });
                    }
                }
                if spellcasting.cantrips.contains(&SpellId::MageHand)
                    && action_availability(
                        &economy,
                        &conditions,
                        &ActionContext::CastSpell {
                            spell: SpellId::MageHand,
                            target_is_valid: true,
                            prepared: true,
                            slot_available: true,
                        },
                    )
                    .map_err(|_| EncounterError::InvalidState {
                        reason: "Mage Hand availability could not be derived",
                    })?
                    .is_available()
                {
                    for object in &live.objects {
                        if distance(actor.position_feet, object.position_feet) <= 30 {
                            actions.push(LegalEncounterAction::CastMageHand {
                                anchor_object_id: object.object_id.clone(),
                            });
                        }
                    }
                }
                if live.mage_hand.is_some() && economy.action_available {
                    if state.objectives.contextual.status == ObjectiveStatus::Pending
                        && distance(actor.position_feet, SLUICE_POSITION_FEET) <= 30
                    {
                        actions.push(LegalEncounterAction::ControlMageHand {
                            object_id: SLUICE_LEVER_OBJECT_ID.to_owned(),
                        });
                    }
                    actions.push(LegalEncounterAction::DismissMageHand);
                }
                if spellcasting.prepared.contains(&SpellId::Sleep)
                    && state.creature.life_status == LifeStatus::Conscious
                    && live.sleep.is_none()
                    && action_availability(
                        &economy,
                        &conditions,
                        &ActionContext::CastSpell {
                            spell: SpellId::Sleep,
                            target_is_valid: distance(
                                actor.position_feet,
                                state.creature.position_feet,
                            ) <= 90,
                            prepared: true,
                            slot_available: rules.runtime_resources.has_spell_slot(),
                        },
                    )
                    .map_err(|_| EncounterError::InvalidState {
                        reason: "Sleep availability could not be derived",
                    })?
                    .is_available()
                {
                    actions.push(LegalEncounterAction::CastSleep);
                }
            }
        }
        if rules.runtime_resources.class == HeroClass::Fighter {
            let second_wind_available = rules
                .runtime_resources
                .second_wind
                .is_some_and(|resource| resource.current > 0);
            if action_availability(
                &economy,
                &conditions,
                &ActionContext::SecondWind {
                    resource_available: second_wind_available,
                },
            )
            .map_err(|_| EncounterError::InvalidState {
                reason: "Second Wind availability could not be derived",
            })?
            .is_available()
            {
                actions.push(LegalEncounterAction::SecondWind);
            }
            let action_surge_available = !economy.action_available
                && rules
                    .runtime_resources
                    .action_surge
                    .is_some_and(|resource| resource.current > 0);
            if action_availability(
                &economy,
                &conditions,
                &ActionContext::ActionSurge {
                    resource_available: action_surge_available,
                },
            )
            .map_err(|_| EncounterError::InvalidState {
                reason: "Action Surge availability could not be derived",
            })?
            .is_available()
            {
                actions.push(LegalEncounterAction::ActionSurge);
            }
        }
    }
    if actor.id == CANAL_WARDEN_ID
        && resources.object_interaction_available
        && state.objectives.contextual.status == ObjectiveStatus::Pending
        && distance(actor.position_feet, state.map.sluice_position_feet)
            <= state.map.context_range_feet
    {
        actions.push(LegalEncounterAction::ContextAction {
            action_id: RELEASE_SLUICE_ACTION_ID.to_owned(),
        });
    }
    actions.push(LegalEncounterAction::EndTurn);
    Ok(actions)
}

fn shield_reaction_available(state: &EncounterState) -> bool {
    let Some(live) = &state.live_q04 else {
        return false;
    };
    live.pending_attack_reaction.is_some() && shield_spell_ready(state)
}

fn shield_spell_ready(state: &EncounterState) -> bool {
    let Some(live) = &state.live_q04 else {
        return false;
    };
    let Some(rules) = &state.hero_rules else {
        return false;
    };
    live.hero_reaction_available
        && live.shield_ward.is_none()
        && state.hero.life_status == LifeStatus::Conscious
        && rules.runtime_resources.class == HeroClass::Wizard
        && rules.runtime_resources.has_spell_slot()
        && rules
            .spellcasting
            .as_ref()
            .is_some_and(|spellcasting| spellcasting.prepared.contains(&SpellId::Shield))
}

fn rest_legal_actions(state: &EncounterState) -> EncounterResult<Vec<LegalEncounterAction>> {
    if state.schema_version != ENCOUNTER_SCHEMA_VERSION || !state.is_safe_rest_boundary() {
        return Ok(Vec::new());
    }
    let Some(live) = &state.live_q04 else {
        return Ok(Vec::new());
    };
    let Some(rules) = &state.hero_rules else {
        return Ok(Vec::new());
    };
    if rules.constitution_modifier.is_none() {
        return Ok(Vec::new());
    }
    if let Some(rest) = &live.active_short_rest {
        let mut actions = Vec::new();
        if state.hero.hit_points.current < state.hero.hit_points.maximum
            && rules.runtime_resources.hit_dice.current > 0
        {
            actions.push(LegalEncounterAction::SpendHitDie);
        }
        if !rest.arcane_recovery_used
            && rules
                .runtime_resources
                .arcane_recovery
                .is_some_and(|resource| resource.current > 0)
            && rules
                .runtime_resources
                .level_one_spell_slots
                .is_some_and(|slots| slots.current < slots.maximum)
        {
            actions.push(LegalEncounterAction::UseArcaneRecovery);
        }
        actions.push(LegalEncounterAction::FinishShortRest);
        return Ok(actions);
    }

    let mut actions = vec![LegalEncounterAction::BeginShortRest];
    let long_rest_available = live
        .last_long_rest_completed_at_campaign_minute
        .is_none_or(|last| live.campaign_time_minutes.saturating_sub(last) >= 24 * 60);
    if long_rest_available {
        actions.push(LegalEncounterAction::TakeLongRest);
    }
    Ok(actions)
}

/// Returns only actions a player is authorized to select. Full action derivation remains
/// available to trusted server policy code through [`legal_actions`], but a creature turn never
/// exposes attack, target, movement, or end-turn choices to a client.
pub fn player_legal_actions(state: &EncounterState) -> EncounterResult<Vec<LegalEncounterAction>> {
    state.validate()?;
    if state.schema_version < ENCOUNTER_SCHEMA_VERSION {
        // Versions 1 and 2 remain replayable but are deliberately read-only. New
        // mutations require an explicit, separately audited migration.
        return Ok(Vec::new());
    }
    if state
        .live_q04
        .as_ref()
        .is_some_and(|live| live.pending_attack_reaction.is_some())
    {
        return legal_actions(state);
    }
    match state.status {
        EncounterStatus::Ready => Ok(vec![LegalEncounterAction::StartEncounter]),
        EncounterStatus::Active
            if state.current_actor_id.as_deref() == Some(state.hero.id.as_str()) =>
        {
            legal_actions(state)
        }
        EncounterStatus::Victory | EncounterStatus::Defeat => legal_actions(state),
        EncounterStatus::Active => Ok(Vec::new()),
    }
}

/// Enforces the server-side player/controller boundary before resolving a client-selected intent.
pub fn require_player_control(state: &EncounterState) -> EncounterResult<()> {
    state.validate()?;
    if state.schema_version == ENCOUNTER_SCHEMA_VERSION
        && (state
            .live_q04
            .as_ref()
            .is_some_and(|live| live.pending_attack_reaction.is_some())
            || state.is_safe_rest_boundary())
    {
        return Ok(());
    }
    if state.status == EncounterStatus::Active
        && state.current_actor_id.as_deref() != Some(state.hero.id.as_str())
    {
        return Err(EncounterError::PlayerControlUnavailable {
            current_actor_id: state.current_actor_id.clone().ok_or(
                EncounterError::InvalidState {
                    reason: "active state has no current actor",
                },
            )?,
        });
    }
    Ok(())
}

/// Selects exactly one intent from the closed, deterministic Soot Wight policy.
///
/// Priority is fixed: attack the authoritative hero when the claw is legal; otherwise move as
/// close as possible while stopping five feet away; otherwise end the turn. No client-provided
/// action, target, or destination participates in this decision.
pub fn select_soot_wight_policy_intent(state: &EncounterState) -> EncounterResult<EncounterIntent> {
    state.validate()?;
    if state.status != EncounterStatus::Active
        || state.current_actor_id.as_deref() != Some(state.creature.id.as_str())
        || state.creature.id != SOOT_WIGHT_ID
    {
        return Err(EncounterError::DeterministicPolicyUnavailable {
            reason: "the Soot Wight is not the active current actor",
        });
    }

    let actions = legal_actions(state)?;
    if actions.iter().any(|action| {
        matches!(
            action,
            LegalEncounterAction::Attack {
                attack_id,
                target_id,
                ..
            } if attack_id == SOOT_WIGHT_ATTACK_ID && target_id == &state.hero.id
        )
    }) {
        return Ok(EncounterIntent::Attack {
            attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
            target_id: state.hero.id.clone(),
        });
    }

    let move_bounds = actions.iter().find_map(|action| match action {
        LegalEncounterAction::Move {
            minimum_destination_feet,
            maximum_destination_feet,
            ..
        } => Some((*minimum_destination_feet, *maximum_destination_feet)),
        _ => None,
    });
    if let Some((minimum, maximum)) = move_bounds {
        let current = state.creature.position_feet;
        let hero = state.hero.position_feet;
        let desired = if current < hero {
            hero.saturating_sub(5)
        } else if current > hero {
            hero.saturating_add(5).min(state.map.maximum_position_feet)
        } else {
            current
        };
        let destination = desired.clamp(minimum, maximum);
        if destination != current && distance(destination, hero) < distance(current, hero) {
            return Ok(EncounterIntent::Move {
                destination_feet: destination,
            });
        }
    }

    if actions.contains(&LegalEncounterAction::EndTurn) {
        return Ok(EncounterIntent::EndTurn);
    }
    Err(EncounterError::DeterministicPolicyUnavailable {
        reason: "the closed policy has no legal action",
    })
}

/// Resolves one validated intent against an immutable input state. The input is never mutated;
/// failed resolution returns no successor state. Applications persist the returned state, raw
/// rolls, and facts atomically and own retry/idempotency handling around this boundary.
pub fn resolve_encounter(
    state: &EncounterState,
    command: &EncounterCommand,
    roll_source: &mut impl EncounterRollSource,
) -> EncounterResult<EncounterResolution> {
    state.validate()?;
    command.validate()?;
    if command.encounter_id != state.encounter_id {
        return Err(EncounterError::WrongEncounter {
            expected: state.encounter_id.clone(),
            actual: command.encounter_id.clone(),
        });
    }
    if command.expected_revision != state.revision {
        return Err(EncounterError::RevisionConflict {
            expected: command.expected_revision,
            actual: state.revision,
        });
    }
    if command.schema_version != state.schema_version {
        return Err(EncounterError::InvalidCommand {
            reason: "command and encounter state schema versions must match",
        });
    }

    let mut next = state.clone();
    let mut rolls = Vec::new();
    let mut facts = Vec::new();
    let narration = match &command.intent {
        EncounterIntent::StartEncounter => {
            resolve_start(&mut next, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::Move { destination_feet } => {
            resolve_move(&mut next, *destination_feet, &mut facts)?
        }
        EncounterIntent::Attack {
            attack_id,
            target_id,
        } => resolve_attack(
            &mut next,
            attack_id,
            target_id,
            roll_source,
            &mut rolls,
            &mut facts,
        )?,
        EncounterIntent::ContextAction { action_id } => {
            resolve_context_action(&mut next, action_id, &mut facts)?
        }
        EncounterIntent::CastSpell { spell, target_id } => resolve_cast_spell(
            &mut next,
            *spell,
            target_id,
            roll_source,
            &mut rolls,
            &mut facts,
        )?,
        EncounterIntent::CastLight { object_id } => {
            resolve_cast_light(&mut next, object_id, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::CastMageHand { anchor_object_id } => {
            resolve_cast_mage_hand(&mut next, anchor_object_id, roll_source, &mut facts)?
        }
        EncounterIntent::ControlMageHand { object_id } => {
            resolve_control_mage_hand(&mut next, object_id, &mut facts)?
        }
        EncounterIntent::DismissMageHand => resolve_dismiss_mage_hand(&mut next, &mut facts)?,
        EncounterIntent::CastSleep => {
            resolve_cast_sleep(&mut next, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::CastShield => {
            resolve_shield_reaction(&mut next, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::DeclineReaction => {
            resolve_decline_reaction(&mut next, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::SecondWind => {
            resolve_second_wind(&mut next, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::ActionSurge => resolve_action_surge(&mut next, &mut facts)?,
        EncounterIntent::BeginShortRest => resolve_begin_short_rest(&mut next, &mut facts)?,
        EncounterIntent::SpendHitDie => {
            resolve_spend_hit_die(&mut next, roll_source, &mut rolls, &mut facts)?
        }
        EncounterIntent::UseArcaneRecovery => {
            resolve_arcane_recovery(&mut next, roll_source, &mut facts)?
        }
        EncounterIntent::FinishShortRest => {
            resolve_finish_short_rest(&mut next, roll_source, &mut facts)?
        }
        EncounterIntent::TakeLongRest => resolve_long_rest(&mut next, &mut facts)?,
        EncounterIntent::EndTurn => resolve_end_turn(&mut next, &mut facts)?,
        EncounterIntent::RollDeathSave => {
            resolve_death_save(&mut next, roll_source, &mut rolls, &mut facts)?
        }
    };

    next.schema_version = state.schema_version;
    next.revision = state
        .revision
        .checked_add(1)
        .ok_or(EncounterError::RevisionOverflow)?;
    for roll in &rolls {
        roll.validate()?;
    }
    next.validate()?;
    let resolution = EncounterResolution {
        schema_version: next.schema_version,
        encounter_id: state.encounter_id.clone(),
        previous_revision: state.revision,
        result_revision: next.revision,
        state: next,
        rolls,
        facts,
        narration,
    };
    resolution.validate()?;
    Ok(resolution)
}

fn resolve_start(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if state.status != EncounterStatus::Ready {
        return Err(EncounterError::IllegalIntent {
            reason: "the encounter has already started",
        });
    }
    let hero_roll = perform_roll(
        roll_source,
        rolls,
        EncounterRollPurpose::Initiative,
        CANAL_WARDEN_ID,
        None,
        None,
        1,
        20,
        vec![RollModifierFact {
            source_id: "srd-5.1-cc:modifier:dexterity-initiative".to_owned(),
            value: i16::from(state.hero.initiative_modifier),
        }],
        None,
    )?;
    let creature_roll = perform_roll(
        roll_source,
        rolls,
        EncounterRollPurpose::Initiative,
        SOOT_WIGHT_ID,
        None,
        None,
        1,
        20,
        vec![RollModifierFact {
            source_id: "manchester-arcana-content:v1:modifier:soot-wight-initiative".to_owned(),
            value: i16::from(state.creature.initiative_modifier),
        }],
        None,
    )?;
    let mut entries = vec![
        InitiativeEntry {
            participant_id: CANAL_WARDEN_ID.to_owned(),
            natural_roll: hero_roll.natural_d20.expect("initiative is a d20"),
            modifier: state.hero.initiative_modifier,
            total: i16::try_from(hero_roll.total).expect("bounded initiative total"),
            tie_break_rank: 0,
        },
        InitiativeEntry {
            participant_id: SOOT_WIGHT_ID.to_owned(),
            natural_roll: creature_roll.natural_d20.expect("initiative is a d20"),
            modifier: state.creature.initiative_modifier,
            total: i16::try_from(creature_roll.total).expect("bounded initiative total"),
            tie_break_rank: 0,
        },
    ];
    sort_initiative_entries(&mut entries);
    for (rank, entry) in entries.iter_mut().enumerate() {
        entry.tie_break_rank = rank as u8;
    }
    let order = entries
        .iter()
        .map(|entry| entry.participant_id.clone())
        .collect::<Vec<_>>();
    let ties = initiative_ties(&entries);
    state.initiative = Some(InitiativeState {
        entries,
        order: order.clone(),
        ties,
    });
    state.status = EncounterStatus::Active;
    state.round = 1;
    set_current_turn(state, 0)?;
    let current_actor_id = state
        .current_actor_id
        .clone()
        .expect("the first initiative entry exists");
    facts.push(EncounterFact::EncounterStarted {
        round: 1,
        initiative_order: order,
        current_actor_id: current_actor_id.clone(),
    });
    if current_actor_id == CANAL_WARDEN_ID {
        let hero_name = hero_narrative_name(state);
        Ok(narration(
            "encounter:start:canal-warden-first",
            format!(
                "{hero_name} reads the soot's motion and acts before the Soot Wight can close."
            ),
            format!("The encounter begins. {hero_name} acts first."),
        ))
    } else {
        Ok(narration(
            "encounter:start:soot-wight-first",
            "The Soot Wight spills from the viaduct shadow and seizes the first move.",
            "The encounter begins. The Soot Wight acts first.",
        ))
    }
}

fn resolve_move(
    state: &mut EncounterState,
    destination_feet: u16,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    let actor_id = require_conscious_active_actor(state)?;
    if destination_feet < state.map.minimum_position_feet
        || destination_feet > state.map.maximum_position_feet
        || !destination_feet.is_multiple_of(5)
    {
        return Err(EncounterError::InvalidDestination { destination_feet });
    }
    let from_feet = combatant(state, &actor_id)
        .expect("validated current actor")
        .position_feet;
    if destination_feet == from_feet {
        return Err(EncounterError::IllegalIntent {
            reason: "movement must change position",
        });
    }
    let requested_feet = distance(from_feet, destination_feet);
    let remaining_feet = state
        .turn_resources
        .as_ref()
        .expect("active state has resources")
        .movement_remaining_feet;
    if requested_feet > remaining_feet {
        return Err(EncounterError::InsufficientMovement {
            requested_feet,
            remaining_feet,
        });
    }
    state
        .turn_resources
        .as_mut()
        .expect("active state has resources")
        .movement_remaining_feet -= requested_feet;
    combatant_mut(state, &actor_id)
        .expect("validated current actor")
        .position_feet = destination_feet;
    if actor_id == CANAL_WARDEN_ID
        && state.schema_version == ENCOUNTER_SCHEMA_VERSION
        && let Some(hand_position) = state
            .live_q04
            .as_ref()
            .and_then(|live| live.mage_hand_position_feet)
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        let expired = reconcile_mage_hand_distance(
            &mut live.mage_hand,
            distance(destination_feet, hand_position),
        )
        .map_err(map_rules_resolution_error)?;
        if expired {
            live.mage_hand_position_feet = None;
            facts.push(EncounterFact::MageHandExpired {
                hand_id: MAGE_HAND_ID.to_owned(),
            });
        }
    }
    let movement_remaining_feet = state
        .turn_resources
        .as_ref()
        .expect("active state has resources")
        .movement_remaining_feet;
    facts.push(EncounterFact::Moved {
        actor_id: actor_id.clone(),
        from_feet,
        to_feet: destination_feet,
        movement_spent_feet: requested_feet,
        movement_remaining_feet,
    });
    Ok(narration(
        "encounter:move",
        format!(
            "{} crosses {} feet of the rain-slick towpath.",
            combatant(state, &actor_id)
                .expect("actor remains present")
                .name,
            requested_feet
        ),
        format!("The current actor moves to {} feet.", destination_feet),
    ))
}

fn resolve_attack(
    state: &mut EncounterState,
    attack_id: &str,
    target_id: &str,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    let actor_id = require_conscious_active_actor(state)?;
    let attack = attack_for_actor(state, &actor_id, attack_id).ok_or_else(|| {
        EncounterError::AttackUnavailable {
            actor_id: actor_id.clone(),
            attack_id: attack_id.to_owned(),
        }
    })?;
    let resolved_attack_id = attack.attack_id().to_owned();
    let attack_range_feet = attack.range_feet();
    let attack_modifiers = attack.attack_modifiers();
    let damage_die_sides = attack.damage_die_sides();
    let damage_modifier = attack.damage_modifier();
    let damage_type = attack.damage_type();
    let expected_target_id = if actor_id == CANAL_WARDEN_ID {
        SOOT_WIGHT_ID
    } else {
        CANAL_WARDEN_ID
    };
    if target_id != expected_target_id {
        return Err(EncounterError::InvalidTarget {
            actor_id,
            target_id: target_id.to_owned(),
        });
    }
    let target = combatant(state, target_id).expect("the fixed target exists");
    if target.life_status == LifeStatus::Dead {
        return Err(EncounterError::IllegalIntent {
            reason: "a dead target cannot be attacked",
        });
    }
    let actor_position = combatant(state, &actor_id)
        .expect("the fixed actor exists")
        .position_feet;
    let target_position = target.position_feet;
    let distance_feet = distance(actor_position, target_position);
    if distance_feet > attack_range_feet {
        return Err(EncounterError::TargetOutOfRange {
            distance_feet,
            range_feet: attack_range_feet,
        });
    }
    if !state
        .turn_resources
        .as_ref()
        .expect("active state has resources")
        .action_available
    {
        return Err(EncounterError::IllegalIntent {
            reason: "the current turn's action has already been spent",
        });
    }
    let armor_class = target.armor_class;
    state
        .turn_resources
        .as_mut()
        .expect("active state has resources")
        .action_available = false;

    let attack_roll = perform_roll(
        roll_source,
        rolls,
        EncounterRollPurpose::Attack,
        &actor_id,
        Some(target_id),
        Some(&resolved_attack_id),
        1,
        20,
        attack_modifiers,
        Some(RollComparison {
            kind: RollComparisonKind::ArmorClass,
            value: i16::try_from(armor_class).expect("fixed armor class fits i16"),
        }),
    )?;
    let outcome = match attack_roll.outcome {
        RawRollOutcome::AutomaticMiss => AttackOutcome::AutomaticMiss,
        RawRollOutcome::Miss => AttackOutcome::Miss,
        RawRollOutcome::Hit => AttackOutcome::Hit,
        RawRollOutcome::CriticalHit => AttackOutcome::CriticalHit,
        _ => unreachable!("attack roll helper returns an attack outcome"),
    };
    facts.push(EncounterFact::AttackResolved {
        actor_id: actor_id.clone(),
        target_id: target_id.to_owned(),
        attack_id: resolved_attack_id.clone(),
        distance_feet,
        range_feet: attack_range_feet,
        armor_class,
        attack_total: attack_roll.total,
        outcome,
    });

    let actor_name = combatant(state, &actor_id)
        .expect("actor remains present")
        .name
        .clone();
    let target_name = combatant(state, target_id)
        .expect("target remains present")
        .name
        .clone();
    if !outcome.is_hit() {
        return Ok(match outcome {
            AttackOutcome::AutomaticMiss => narration(
                "encounter:attack:natural-one",
                format!(
                    "{} overcommits, and {} slips clear.",
                    actor_name, target_name
                ),
                format!("{} misses {} on a natural 1.", actor_name, target_name),
            ),
            AttackOutcome::Miss => narration(
                "encounter:attack:miss",
                format!("{}'s strike cuts rain, not {}.", actor_name, target_name),
                format!("{} misses {}.", actor_name, target_name),
            ),
            _ => unreachable!(),
        });
    }

    if state.schema_version == ENCOUNTER_SCHEMA_VERSION
        && actor_id == SOOT_WIGHT_ID
        && shield_spell_ready(state)
    {
        let natural_roll = attack_roll
            .natural_d20
            .expect("weapon attack roll has a natural d20");
        let pending = PendingAttackReaction {
            actor_id: actor_id.clone(),
            target_id: target_id.to_owned(),
            attack_id: resolved_attack_id.clone(),
            natural_roll,
            attack_total: attack_roll.total,
            armor_class,
            critical: outcome.is_critical(),
        };
        state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state")
            .pending_attack_reaction = Some(pending);
        facts.push(EncounterFact::AttackReactionOpened {
            actor_id,
            target_id: target_id.to_owned(),
            attack_id: resolved_attack_id,
            natural_roll,
            attack_total: attack_roll.total,
            armor_class,
        });
        return Ok(narration(
            "encounter:reaction:shield-window",
            format!(
                "{}'s claw breaks through {}'s guard; a heartbeat remains to raise a Shield.",
                actor_name, target_name
            ),
            "The Soot Wight has hit. Shield or decline the reaction before damage is rolled.",
        ));
    }

    let damage_roll = perform_roll(
        roll_source,
        rolls,
        EncounterRollPurpose::Damage,
        &actor_id,
        Some(target_id),
        Some(&resolved_attack_id),
        if outcome.is_critical() { 2 } else { 1 },
        damage_die_sides,
        vec![damage_modifier],
        None,
    )?;
    let damage =
        u16::try_from(damage_roll.total.max(0)).map_err(|_| EncounterError::InvalidState {
            reason: "weapon damage total is outside the encounter range",
        })?;
    let application = {
        let policy = state.lethality_policy;
        let target = combatant_mut(state, target_id).expect("the fixed target exists");
        apply_damage(target, damage, outcome.is_critical(), policy)
    };
    facts.push(EncounterFact::DamageApplied {
        actor_id: actor_id.clone(),
        target_id: target_id.to_owned(),
        attack_id: resolved_attack_id,
        damage_type,
        critical: outcome.is_critical(),
        amount: damage,
        temporary_hit_points_before: application.temporary_before,
        temporary_hit_points_absorbed: application.temporary_absorbed,
        temporary_hit_points_after: application.temporary_after,
        current_hit_points_before: application.current_before,
        current_hit_points_after: application.current_after,
    });
    wake_sleep_after_damage(state, target_id, damage, facts);
    if application.life_before != application.life_after {
        let health = &combatant(state, target_id)
            .expect("target remains present")
            .hit_points;
        facts.push(EncounterFact::LifeStatusChanged {
            participant_id: target_id.to_owned(),
            from: application.life_before,
            to: application.life_after,
            death_save_successes: health.death_saves.successes,
            death_save_failures: health.death_saves.failures,
        });
    }

    let completion = finish_for_health_transition(state, target_id, facts)?;
    if let Some(outcome) = completion {
        return Ok(match outcome {
            EncounterOutcome::Victory => narration(
                "encounter:complete:victory",
                format!(
                    "{}'s blow breaks the Soot Wight apart; cleansing water carries the ash away.",
                    actor_name
                ),
                "The Soot Wight is defeated. The encounter ends in victory.",
            ),
            EncounterOutcome::Defeat => {
                let hero_name = hero_narrative_name(state);
                narration(
                    "encounter:complete:defeat",
                    format!(
                        "{hero_name} falls beneath the viaduct as the encounter closes around them."
                    ),
                    format!("{hero_name} is defeated. The encounter ends with no reward."),
                )
            }
        });
    }
    Ok(if outcome.is_critical() {
        narration(
            "encounter:attack:critical-hit",
            format!(
                "{} finds the opening and lands a devastating blow on {} for {} damage.",
                actor_name, target_name, damage
            ),
            format!("Critical hit: {} takes {} damage.", target_name, damage),
        )
    } else {
        narration(
            "encounter:attack:hit",
            format!(
                "{} strikes {} for {} damage.",
                actor_name, target_name, damage
            ),
            format!("Hit: {} takes {} damage.", target_name, damage),
        )
    })
}

struct RulesDiceAdapter<'a, R>(&'a mut R);

impl<R: EncounterRollSource> DiceSource for RulesDiceAdapter<'_, R> {
    fn roll(&mut self, sides: u16) -> u16 {
        self.0.roll_die(sides)
    }
}

fn map_rules_resolution_error(error: RulesMatrixError) -> EncounterError {
    match error {
        RulesMatrixError::Core(GameCoreError::InvalidDieRoll { sides, value }) => {
            EncounterError::InvalidRoll { sides, value }
        }
        _ => EncounterError::IllegalIntent {
            reason: "the selected spell or class feature is unavailable in authoritative state",
        },
    }
}

fn authored_object(
    state: &EncounterState,
    object_id: &str,
) -> EncounterResult<EncounterObjectState> {
    state
        .live_q04
        .as_ref()
        .and_then(|live| {
            live.objects
                .iter()
                .find(|object| object.object_id == object_id)
        })
        .cloned()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the authored object is not available in this encounter",
        })
}

fn resolve_cast_light(
    state: &mut EncounterState,
    object_id: &str,
    roll_source: &mut impl EncounterRollSource,
    _rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::CastLight {
        object_id: object_id.to_owned(),
    }) {
        return Err(EncounterError::IllegalIntent {
            reason: "Light is not a server-derived legal action for that authored object",
        });
    }
    let object = authored_object(state, object_id)?;
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let spellcasting = rules
        .spellcasting
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no spellcasting snapshot",
        })?;
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        resolve_supported_spell(
            &spellcasting,
            &mut rules.runtime_resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Light {
                target: LightTarget {
                    object_id: object.object_id.clone(),
                    distance_feet: distance(state.hero.position_feet, object.position_feet),
                    object_maximum_dimension_feet: object.maximum_dimension_feet,
                    hostile_carrier: None,
                },
            },
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let duration = resolution
        .effects
        .iter()
        .find_map(|effect| match effect {
            SpellEffect::IlluminateObject {
                object_id: effect_object,
                duration_rounds,
                ..
            } if effect_object == object_id => Some(*duration_rounds),
            _ => None,
        })
        .ok_or(EncounterError::InvalidState {
            reason: "Light resolver omitted its authored object effect",
        })?;
    state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state")
        .objects
        .iter_mut()
        .find(|candidate| candidate.object_id == object_id)
        .expect("validated authored object remains present")
        .light_remaining_rounds = Some(duration);
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::SpellCastResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: object_id.to_owned(),
        spell: SpellId::Light,
        level_one_spell_slots_before: None,
        level_one_spell_slots_after: None,
        damage_applied: 0,
    });
    facts.push(EncounterFact::LightApplied {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        object_id: object_id.to_owned(),
        duration_rounds: duration,
    });
    Ok(narration(
        "encounter:spell:light",
        "Arcane light gathers on the old viaduct stone and holds against the soot-dark.",
        format!("Light illuminates {object_id} for 600 rounds."),
    ))
}

fn resolve_cast_mage_hand(
    state: &mut EncounterState,
    anchor_object_id: &str,
    roll_source: &mut impl EncounterRollSource,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::CastMageHand {
        anchor_object_id: anchor_object_id.to_owned(),
    }) {
        return Err(EncounterError::IllegalIntent {
            reason: "Mage Hand is not a server-derived legal action for that authored anchor",
        });
    }
    let object = authored_object(state, anchor_object_id)?;
    let distance_from_caster = distance(state.hero.position_feet, object.position_feet);
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let spellcasting = rules
        .spellcasting
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no spellcasting snapshot",
        })?;
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        resolve_supported_spell(
            &spellcasting,
            &mut rules.runtime_resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::MageHand {
                target: MageHandTarget {
                    hand_id: MAGE_HAND_ID.to_owned(),
                    distance_feet: distance_from_caster,
                },
            },
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let hand = resolution
        .effects
        .iter()
        .find_map(|effect| match effect {
            SpellEffect::CreateMageHand { hand } => Some(hand.clone()),
            _ => None,
        })
        .ok_or(EncounterError::InvalidState {
            reason: "Mage Hand resolver omitted its duration state",
        })?;
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        live.mage_hand = Some(hand.clone());
        live.mage_hand_position_feet = Some(object.position_feet);
    }
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::SpellCastResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: anchor_object_id.to_owned(),
        spell: SpellId::MageHand,
        level_one_spell_slots_before: None,
        level_one_spell_slots_after: None,
        damage_applied: 0,
    });
    facts.push(EncounterFact::MageHandCreated {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        hand_id: hand.hand_id,
        anchor_object_id: anchor_object_id.to_owned(),
        distance_from_caster_feet: distance_from_caster,
        duration_rounds: hand.remaining_rounds,
    });
    Ok(narration(
        "encounter:spell:mage-hand",
        "A spectral hand takes shape among the dripping ironwork.",
        "Mage Hand appears for 10 rounds.",
    ))
}

fn resolve_control_mage_hand(
    state: &mut EncounterState,
    object_id: &str,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::ControlMageHand {
        object_id: object_id.to_owned(),
    }) {
        return Err(EncounterError::IllegalIntent {
            reason: "that Mage Hand control is not a server-derived legal action",
        });
    }
    let object = authored_object(state, object_id)?;
    let resulting_distance = distance(state.hero.position_feet, object.position_feet);
    let current_distance = state
        .live_q04
        .as_ref()
        .and_then(|live| live.mage_hand.as_ref())
        .expect("legal control has an active hand")
        .distance_from_caster_feet;
    let mut hand = state
        .live_q04
        .as_ref()
        .expect("schema-v3 state has live Q04 state")
        .mage_hand
        .clone();
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let resolution = resolve_mage_hand_action(
        &mut hand,
        &mut economy,
        &ConditionSet::empty(),
        &MageHandActionIntent::Control {
            target: MageHandControlTarget {
                object_id: object.object_id.clone(),
                hand_movement_feet: current_distance.abs_diff(resulting_distance),
                resulting_distance_from_caster_feet: resulting_distance,
                object_weight_pounds: object.weight_pounds,
                is_magic_item: object.is_magic_item,
                operation: MageHandOperation::ManipulateObject,
            },
        },
    )
    .map_err(map_rules_resolution_error)?;
    let MageHandActionEffect::Controlled {
        hand_id,
        operation,
        resulting_distance_from_caster_feet,
        ..
    } = resolution.effect
    else {
        return Err(EncounterError::InvalidState {
            reason: "Mage Hand control returned the wrong effect",
        });
    };
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        live.mage_hand = resolution.resulting_hand;
        live.mage_hand_position_feet = Some(object.position_feet);
    }
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    facts.push(EncounterFact::MageHandControlled {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        hand_id,
        object_id: object_id.to_owned(),
        operation,
        resulting_distance_from_caster_feet,
    });
    if object_id == SLUICE_LEVER_OBJECT_ID
        && state.objectives.contextual.status == ObjectiveStatus::Pending
    {
        let removed_temporary_hit_points = state.creature.hit_points.temporary;
        state.creature.hit_points.temporary = 0;
        state
            .creature
            .status_effects
            .retain(|effect| *effect != CombatantStatusEffect::SootVeil);
        state.objectives.contextual.status = ObjectiveStatus::Completed;
        facts.push(EncounterFact::ContextActionResolved {
            actor_id: CANAL_WARDEN_ID.to_owned(),
            action_id: SLUICE_LEVER_OBJECT_ID.to_owned(),
            removed_temporary_hit_points,
            objective_id: RELEASE_SLUICE_OBJECTIVE_ID.to_owned(),
        });
    }
    Ok(narration(
        "encounter:spell:mage-hand-control",
        "The spectral hand closes around the cleansing lever and drags it down; canal water answers.",
        "Mage Hand manipulates the authored sluice lever.",
    ))
}

fn resolve_dismiss_mage_hand(
    state: &mut EncounterState,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::DismissMageHand) {
        return Err(EncounterError::IllegalIntent {
            reason: "Mage Hand cannot be dismissed now",
        });
    }
    let mut hand = state
        .live_q04
        .as_ref()
        .expect("schema-v3 state has live Q04 state")
        .mage_hand
        .clone();
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let resolution = resolve_mage_hand_action(
        &mut hand,
        &mut economy,
        &ConditionSet::empty(),
        &MageHandActionIntent::Dismiss,
    )
    .map_err(map_rules_resolution_error)?;
    let MageHandActionEffect::Dismissed { hand_id } = resolution.effect else {
        return Err(EncounterError::InvalidState {
            reason: "Mage Hand dismissal returned the wrong effect",
        });
    };
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        live.mage_hand = None;
        live.mage_hand_position_feet = None;
    }
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    facts.push(EncounterFact::MageHandDismissed {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        hand_id,
    });
    Ok(narration(
        "encounter:spell:mage-hand-dismiss",
        "The spectral hand thins into rain and is gone.",
        "Mage Hand is dismissed.",
    ))
}

fn resolve_cast_sleep(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::CastSleep) {
        return Err(EncounterError::IllegalIntent {
            reason: "Sleep is not a server-derived legal action",
        });
    }
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let spellcasting = rules
        .spellcasting
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no spellcasting snapshot",
        })?;
    let slots_before =
        spell_slot_count(&rules.runtime_resources).ok_or(EncounterError::InvalidState {
            reason: "wizard spell slots are absent",
        })?;
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let ordered_target_ids = vec![SOOT_WIGHT_ID.to_owned()];
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        resolve_supported_spell(
            &spellcasting,
            &mut rules.runtime_resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Sleep {
                center_distance_feet: distance(
                    state.hero.position_feet,
                    state.creature.position_feet,
                ),
                candidates: vec![SleepCandidate {
                    target_id: SOOT_WIGHT_ID.to_owned(),
                    distance_from_point_feet: 0,
                    current_hit_points: state.creature.hit_points.current,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                }],
            },
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let pool = resolution
        .sleep_hit_point_pool
        .ok_or(EncounterError::InvalidState {
            reason: "Sleep resolver omitted its hit-point pool",
        })?;
    let pool_roll = resolution
        .damage_rolls
        .first()
        .ok_or(EncounterError::InvalidState {
            reason: "Sleep resolver omitted its canonical 5d8 roll",
        })?;
    push_sleep_roll(rolls, pool_roll);
    let affected = resolution
        .effects
        .iter()
        .filter_map(|effect| match effect {
            SpellEffect::ApplyCondition { target_id, .. } => Some(target_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if affected.iter().any(|target| target == SOOT_WIGHT_ID) {
        state
            .creature
            .status_effects
            .push(CombatantStatusEffect::MagicallyAsleep);
        state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state")
            .sleep = Some(EncounterSleepState {
            target_id: SOOT_WIGHT_ID.to_owned(),
            remaining_rounds: 10,
        });
    }
    let slots_after =
        spell_slot_count(&rules.runtime_resources).expect("wizard slots remain present");
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::SpellCastResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: SOOT_WIGHT_ID.to_owned(),
        spell: SpellId::Sleep,
        level_one_spell_slots_before: Some(slots_before),
        level_one_spell_slots_after: Some(slots_after),
        damage_applied: 0,
    });
    facts.push(EncounterFact::SleepResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        hit_point_pool: pool,
        ordered_target_ids,
        affected_target_ids: affected.clone(),
        duration_rounds: 10,
    });
    Ok(if affected.is_empty() {
        narration(
            "encounter:spell:sleep-resisted",
            "Drowsing magic rolls through the soot, but the wight remains upright.",
            format!("Sleep rolled a {pool} hit-point pool; no target was affected."),
        )
    } else {
        narration(
            "encounter:spell:sleep",
            "The Soot Wight folds into the wet stones under the weight of sudden slumber.",
            format!("Sleep rolled a {pool} hit-point pool and affected the Soot Wight."),
        )
    })
}

fn resolve_shield_reaction(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::CastShield) {
        return Err(EncounterError::IllegalIntent {
            reason: "Shield is not available for a real pending hit",
        });
    }
    let pending = state
        .live_q04
        .as_ref()
        .and_then(|live| live.pending_attack_reaction.clone())
        .expect("legal Shield has a pending hit");
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let spellcasting = rules
        .spellcasting
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no spellcasting snapshot",
        })?;
    let slots_before =
        spell_slot_count(&rules.runtime_resources).ok_or(EncounterError::InvalidState {
            reason: "wizard spell slots are absent",
        })?;
    let armor_class =
        u8::try_from(pending.armor_class).map_err(|_| EncounterError::InvalidState {
            reason: "pending Shield armor class is outside the rules range",
        })?;
    let attack_total =
        i16::try_from(pending.attack_total).map_err(|_| EncounterError::InvalidState {
            reason: "pending Shield attack total is outside the rules range",
        })?;
    let mut economy = ActionEconomy::new(0);
    economy.reaction_available = state
        .live_q04
        .as_ref()
        .expect("schema-v3 state has live Q04 state")
        .hero_reaction_available;
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        resolve_supported_spell(
            &spellcasting,
            &mut rules.runtime_resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Shield {
                trigger: ShieldTrigger::AttackHit {
                    natural_roll: pending.natural_roll,
                    attack_total,
                    armor_class,
                },
            },
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let (armor_class_bonus, negated_trigger, immune_to_magic_missile) = resolution
        .effects
        .iter()
        .find_map(|effect| match effect {
            SpellEffect::ShieldWard {
                armor_class_bonus,
                negates_triggering_attack,
                immune_to_magic_missile,
                until_start_of_caster_turn: true,
            } => Some((
                *armor_class_bonus,
                *negates_triggering_attack,
                *immune_to_magic_missile,
            )),
            _ => None,
        })
        .ok_or(EncounterError::InvalidState {
            reason: "Shield resolver omitted its ward effect",
        })?;
    let slots_after =
        spell_slot_count(&rules.runtime_resources).expect("wizard slots remain present");
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        live.pending_attack_reaction = None;
        live.hero_reaction_available = false;
        live.shield_ward = Some(ShieldWardState {
            caster_id: CANAL_WARDEN_ID.to_owned(),
            armor_class_bonus,
            immune_to_magic_missile,
            expiry: ShieldExpiry::StartOfCasterTurn,
        });
    }
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::SpellCastResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: CANAL_WARDEN_ID.to_owned(),
        spell: SpellId::Shield,
        level_one_spell_slots_before: Some(slots_before),
        level_one_spell_slots_after: Some(slots_after),
        damage_applied: 0,
    });
    facts.push(EncounterFact::ShieldResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        triggering_attack_id: pending.attack_id.clone(),
        triggering_natural_roll: pending.natural_roll,
        triggering_attack_total: pending.attack_total,
        armor_class_before: pending.armor_class,
        armor_class_with_shield: pending.armor_class + u16::from(armor_class_bonus),
        negated_trigger,
        level_one_spell_slots_before: slots_before,
        level_one_spell_slots_after: slots_after,
    });
    if negated_trigger {
        return Ok(narration(
            "encounter:spell:shield-negates-hit",
            "A pane of force snaps into being and turns the claw aside before it can bite.",
            "Shield negates the pending hit; no damage roll is made.",
        ));
    }
    let completion = resolve_pending_attack_damage(state, &pending, roll_source, rolls, facts)?;
    if completion.is_some() {
        state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state")
            .shield_ward = None;
    }
    Ok(if pending.critical {
        narration(
            "encounter:spell:shield-natural-twenty",
            "The ward flares, but the wight's perfect strike punches through it.",
            "Shield cannot negate the natural-20 hit; damage is applied.",
        )
    } else {
        narration(
            "encounter:spell:shield-hit-persists",
            "The ward rises, but the claw was already driven too deep to turn aside.",
            "Shield is active, but the triggering hit still deals damage.",
        )
    })
}

fn resolve_decline_reaction(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::DeclineReaction) {
        return Err(EncounterError::IllegalIntent {
            reason: "there is no pending reaction to decline",
        });
    }
    let pending = state
        .live_q04
        .as_ref()
        .and_then(|live| live.pending_attack_reaction.clone())
        .expect("legal decline has a pending hit");
    state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state")
        .pending_attack_reaction = None;
    facts.push(EncounterFact::AttackReactionDeclined {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        triggering_attack_id: pending.attack_id.clone(),
    });
    let completion = resolve_pending_attack_damage(state, &pending, roll_source, rolls, facts)?;
    Ok(if completion == Some(EncounterOutcome::Defeat) {
        narration(
            "encounter:reaction:decline-defeat",
            "The claw lands without a ward between it and the hero; the viaduct goes dark.",
            "Shield was declined. The pending hit defeats the hero.",
        )
    } else {
        narration(
            "encounter:reaction:decline",
            "The moment for a ward passes, and the soot-claw lands.",
            "Shield was declined. The pending hit deals damage.",
        )
    })
}

fn resolve_pending_attack_damage(
    state: &mut EncounterState,
    pending: &PendingAttackReaction,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<Option<EncounterOutcome>> {
    let damage_roll = perform_roll(
        roll_source,
        rolls,
        EncounterRollPurpose::Damage,
        &pending.actor_id,
        Some(&pending.target_id),
        Some(&pending.attack_id),
        if pending.critical { 2 } else { 1 },
        CREATURE_ATTACK.damage_die_sides,
        vec![modifier_fact(&CREATURE_ATTACK.damage_modifier)],
        None,
    )?;
    let damage =
        u16::try_from(damage_roll.total.max(0)).map_err(|_| EncounterError::InvalidState {
            reason: "pending attack damage total is outside the encounter range",
        })?;
    let application = {
        let policy = state.lethality_policy;
        apply_damage(&mut state.hero, damage, pending.critical, policy)
    };
    facts.push(EncounterFact::DamageApplied {
        actor_id: pending.actor_id.clone(),
        target_id: pending.target_id.clone(),
        attack_id: pending.attack_id.clone(),
        damage_type: CREATURE_ATTACK.damage_type,
        critical: pending.critical,
        amount: damage,
        temporary_hit_points_before: application.temporary_before,
        temporary_hit_points_absorbed: application.temporary_absorbed,
        temporary_hit_points_after: application.temporary_after,
        current_hit_points_before: application.current_before,
        current_hit_points_after: application.current_after,
    });
    if application.life_before != application.life_after {
        facts.push(EncounterFact::LifeStatusChanged {
            participant_id: CANAL_WARDEN_ID.to_owned(),
            from: application.life_before,
            to: application.life_after,
            death_save_successes: state.hero.hit_points.death_saves.successes,
            death_save_failures: state.hero.hit_points.death_saves.failures,
        });
    }
    finish_for_health_transition(state, CANAL_WARDEN_ID, facts)
}

fn wake_sleep_after_damage(
    state: &mut EncounterState,
    target_id: &str,
    damage: u16,
    facts: &mut Vec<EncounterFact>,
) {
    if damage == 0 || target_id != SOOT_WIGHT_ID || state.schema_version != ENCOUNTER_SCHEMA_VERSION
    {
        return;
    }
    let live = state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state");
    if let Some(sleep) = live.sleep.take() {
        state
            .creature
            .status_effects
            .retain(|effect| *effect != CombatantStatusEffect::MagicallyAsleep);
        facts.push(EncounterFact::SleepEnded {
            target_id: sleep.target_id,
            reason: SleepEndReason::Damaged,
        });
    }
}

fn recovery_health(state: &EncounterState) -> HealthState {
    HealthState {
        schema_version: RULES_MATRIX_SCHEMA_VERSION,
        maximum: state.hero.hit_points.maximum,
        current: state.hero.hit_points.current,
        temporary: state.hero.hit_points.temporary,
        vital_status: if state.hero.life_status == LifeStatus::Dead {
            VitalStatus::Dead
        } else if state.hero.hit_points.current == 0 {
            VitalStatus::Dying
        } else {
            VitalStatus::Active
        },
        death_saves: DeathSaveTally {
            successes: state.hero.hit_points.death_saves.successes,
            failures: state.hero.hit_points.death_saves.failures,
        },
    }
}

fn apply_recovery_health(state: &mut EncounterState, health: &HealthState) {
    state.hero.hit_points.current = health.current;
    state.hero.hit_points.temporary = health.temporary;
    state.hero.hit_points.death_saves = DeathSaves {
        successes: health.death_saves.successes,
        failures: health.death_saves.failures,
    };
    state.hero.life_status = match health.vital_status {
        VitalStatus::Active => LifeStatus::Conscious,
        VitalStatus::Dying => LifeStatus::Unconscious,
        VitalStatus::Stable => LifeStatus::Stable,
        VitalStatus::Dead => LifeStatus::Dead,
    };
    if matches!(
        state.status,
        EncounterStatus::Victory | EncounterStatus::Defeat
    ) && let Some(transition) = &mut state.transition
    {
        transition.hero_current_hit_points = state.hero.hit_points.current;
        transition.hero_life_status = state.hero.life_status;
    }
}

fn apply_story_recovery_boundary(state: &mut EncounterState) -> EncounterResult<()> {
    if state.status == EncounterStatus::Defeat
        && state.lethality_policy == LethalityPolicy::StoryRecovery
        && state.hero.life_status == LifeStatus::Unconscious
    {
        let transition = state
            .transition
            .as_ref()
            .ok_or(EncounterError::InvalidState {
                reason: "story recovery defeat is missing its transition",
            })?;
        state.hero.hit_points.current = transition.hero_current_hit_points;
        state.hero.hit_points.death_saves = DeathSaves::default();
        state.hero.life_status = transition.hero_life_status;
    }
    Ok(())
}

fn expire_effects_for_rest(state: &mut EncounterState, facts: &mut Vec<EncounterFact>) {
    let live = state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state");
    for object in &mut live.objects {
        if object.light_remaining_rounds.take().is_some() {
            facts.push(EncounterFact::LightExpired {
                object_id: object.object_id.clone(),
            });
        }
    }
    if let Some(hand) = live.mage_hand.take() {
        live.mage_hand_position_feet = None;
        facts.push(EncounterFact::MageHandExpired {
            hand_id: hand.hand_id,
        });
    }
    if let Some(sleep) = live.sleep.take() {
        state
            .creature
            .status_effects
            .retain(|effect| *effect != CombatantStatusEffect::MagicallyAsleep);
        facts.push(EncounterFact::SleepEnded {
            target_id: sleep.target_id,
            reason: SleepEndReason::DurationExpired,
        });
    }
}

fn resolve_begin_short_rest(
    state: &mut EncounterState,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::BeginShortRest) {
        return Err(EncounterError::IllegalIntent {
            reason: "a short rest is not legal at this boundary",
        });
    }
    apply_story_recovery_boundary(state)?;
    let started_at = state
        .live_q04
        .as_ref()
        .expect("schema-v3 state has live Q04 state")
        .campaign_time_minutes;
    let completes_at =
        started_at
            .checked_add(SHORT_REST_MINUTES)
            .ok_or(EncounterError::InvalidState {
                reason: "trusted campaign time overflowed",
            })?;
    expire_effects_for_rest(state, facts);
    let live = state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state");
    live.campaign_time_minutes = completes_at;
    live.active_short_rest = Some(ShortRestState {
        boundary_id: POST_ENCOUNTER_REST_BOUNDARY_ID.to_owned(),
        started_at_campaign_minute: started_at,
        completes_at_campaign_minute: completes_at,
        hit_dice_spent: 0,
        arcane_recovery_used: false,
    });
    facts.push(EncounterFact::RestStarted {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        kind: RestKind::Short,
        boundary_id: POST_ENCOUNTER_REST_BOUNDARY_ID.to_owned(),
        started_at_campaign_minute: started_at,
        completes_at_campaign_minute: completes_at,
    });
    Ok(narration(
        "encounter:rest:short-start",
        "The aftermath is secured. An hour passes beneath the viaduct while the hero binds their wounds.",
        "A trusted 60-minute short-rest boundary is active; confirm each hit die separately.",
    ))
}

fn resolve_spend_hit_die(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::SpendHitDie) {
        return Err(EncounterError::IllegalIntent {
            reason: "one hit die cannot be spent now",
        });
    }
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let constitution_modifier =
        rules
            .constitution_modifier
            .ok_or(EncounterError::InvalidState {
                reason: "schema-v3 rest snapshot lacks constitution",
            })?;
    let hit_dice_before = rules.runtime_resources.hit_dice.current;
    let mut health = recovery_health(state);
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        spend_hit_die(
            &mut rules.runtime_resources,
            &mut health,
            constitution_modifier,
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    push_hit_die_roll(rolls, &resolution);
    apply_recovery_health(state, &health);
    state.hero_rules = Some(rules);
    state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state")
        .active_short_rest
        .as_mut()
        .expect("legal hit-die spend has an active rest")
        .hit_dice_spent += 1;
    facts.push(EncounterFact::HitDieSpent {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        hit_die: resolution.hit_die,
        roll: resolution.roll,
        constitution_modifier,
        healing_total: resolution.healing_total,
        effective_healing: resolution.hit_points_recovered,
        hit_dice_before,
        hit_dice_after: resolution.resulting_resources.hit_dice.current,
    });
    Ok(narration(
        "encounter:rest:hit-die",
        format!(
            "The hero commits one hit die and recovers {} hit points.",
            resolution.hit_points_recovered
        ),
        format!(
            "Hit die {} plus Constitution {} recovered {} hit points.",
            resolution.roll, constitution_modifier, resolution.hit_points_recovered
        ),
    ))
}

fn resolve_arcane_recovery(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::UseArcaneRecovery) {
        return Err(EncounterError::IllegalIntent {
            reason: "Arcane Recovery is not legal during this short rest",
        });
    }
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let slots_before = rules
        .runtime_resources
        .level_one_spell_slots
        .expect("legal Arcane Recovery has spell slots")
        .current;
    let recovery_before = rules
        .runtime_resources
        .arcane_recovery
        .expect("legal Arcane Recovery has its resource")
        .current;
    let mut health = recovery_health(state);
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        take_short_rest(
            &mut rules.runtime_resources,
            &mut health,
            rules
                .constitution_modifier
                .expect("v3 rules include constitution"),
            &ShortRestRequest {
                hit_dice_to_spend: 0,
                use_arcane_recovery: true,
            },
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let slots_after = resolution
        .resulting_resources
        .level_one_spell_slots
        .expect("wizard rest preserves spell slots")
        .current;
    let recovery_after = resolution
        .resulting_resources
        .arcane_recovery
        .expect("wizard rest preserves Arcane Recovery")
        .current;
    state.hero_rules = Some(rules);
    state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state")
        .active_short_rest
        .as_mut()
        .expect("legal Arcane Recovery has an active rest")
        .arcane_recovery_used = true;
    facts.push(EncounterFact::ArcaneRecoveryApplied {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        spell_slots_before: slots_before,
        spell_slots_after: slots_after,
        resource_before: recovery_before,
        resource_after: recovery_after,
    });
    Ok(narration(
        "encounter:rest:arcane-recovery",
        "The wizard reconstructs one spent pattern from the quiet geometry of the runes.",
        "Arcane Recovery restores one level-one spell slot.",
    ))
}

fn resolve_finish_short_rest(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::FinishShortRest) {
        return Err(EncounterError::IllegalIntent {
            reason: "there is no active short rest to finish",
        });
    }
    let rest = state
        .live_q04
        .as_ref()
        .and_then(|live| live.active_short_rest.clone())
        .expect("legal finish has an active rest");
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let mut health = recovery_health(state);
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        take_short_rest(
            &mut rules.runtime_resources,
            &mut health,
            rules
                .constitution_modifier
                .expect("v3 rules include constitution"),
            &ShortRestRequest {
                hit_dice_to_spend: 0,
                use_arcane_recovery: false,
            },
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    apply_recovery_health(state, &health);
    state.hero_rules = Some(rules);
    state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state")
        .active_short_rest = None;
    facts.push(EncounterFact::RestCompleted {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        kind: RestKind::Short,
        boundary_id: rest.boundary_id,
        completed_at_campaign_minute: rest.completes_at_campaign_minute,
        hit_points_recovered: resolution.hit_points_recovered,
        hit_dice_recovered: 0,
        spell_slots_recovered: 0,
    });
    Ok(narration(
        "encounter:rest:short-complete",
        "The hour closes with gear reset and breath steadied; the hero chooses to move on.",
        "Short rest completed. No additional hit die is spent automatically.",
    ))
}

fn resolve_long_rest(
    state: &mut EncounterState,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !legal_actions(state)?.contains(&LegalEncounterAction::TakeLongRest) {
        return Err(EncounterError::IllegalIntent {
            reason: "a long rest is unavailable under the once-per-24-hours boundary policy",
        });
    }
    apply_story_recovery_boundary(state)?;
    let started_at = state
        .live_q04
        .as_ref()
        .expect("schema-v3 state has live Q04 state")
        .campaign_time_minutes;
    let completes_at =
        started_at
            .checked_add(LONG_REST_MINUTES)
            .ok_or(EncounterError::InvalidState {
                reason: "trusted campaign time overflowed",
            })?;
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let mut health = recovery_health(state);
    let resolution = take_long_rest(&mut rules.runtime_resources, &mut health)
        .map_err(map_rules_resolution_error)?;
    expire_effects_for_rest(state, facts);
    apply_recovery_health(state, &health);
    state.hero_rules = Some(rules);
    let live = state
        .live_q04
        .as_mut()
        .expect("schema-v3 state has live Q04 state");
    live.campaign_time_minutes = completes_at;
    live.last_long_rest_completed_at_campaign_minute = Some(completes_at);
    facts.push(EncounterFact::RestStarted {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        kind: RestKind::Long,
        boundary_id: POST_ENCOUNTER_REST_BOUNDARY_ID.to_owned(),
        started_at_campaign_minute: started_at,
        completes_at_campaign_minute: completes_at,
    });
    facts.push(EncounterFact::RestCompleted {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        kind: RestKind::Long,
        boundary_id: POST_ENCOUNTER_REST_BOUNDARY_ID.to_owned(),
        completed_at_campaign_minute: completes_at,
        hit_points_recovered: resolution.hit_points_recovered,
        hit_dice_recovered: resolution.hit_dice_recovered,
        spell_slots_recovered: resolution.spell_slots_recovered,
    });
    Ok(narration(
        "encounter:rest:long-complete",
        "Eight guarded hours pass. At their end, the hero rises fully restored.",
        "Long rest completed after 480 trusted campaign minutes; another requires 24 hours.",
    ))
}

fn resolve_cast_spell(
    state: &mut EncounterState,
    spell: SpellId,
    target_id: &str,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if !matches!(spell, SpellId::FireBolt | SpellId::MagicMissile) {
        return Err(EncounterError::IllegalIntent {
            reason: "that allowlisted spell is not exposed by this encounter",
        });
    }
    let actor_id = require_conscious_active_actor(state)?;
    if actor_id != CANAL_WARDEN_ID
        || !legal_actions(state)?.iter().any(|action| {
            matches!(
                action,
                LegalEncounterAction::CastSpell {
                    spell: legal_spell,
                    target_id: legal_target,
                    ..
                } if *legal_spell == spell && legal_target == target_id
            )
        })
    {
        return Err(EncounterError::IllegalIntent {
            reason: "the spell is not a server-derived legal action",
        });
    }

    let distance_feet = distance(state.hero.position_feet, state.creature.position_feet);
    let armor_class =
        u8::try_from(state.creature.armor_class).map_err(|_| EncounterError::InvalidState {
            reason: "spell target armor class is outside the rules-matrix range",
        })?;
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let spellcasting = rules
        .spellcasting
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no spellcasting snapshot",
        })?;
    let slots_before = spell_slot_count(&rules.runtime_resources);
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let intent = match spell {
        SpellId::FireBolt => SupportedSpellIntent::FireBolt {
            target: FireBoltTarget {
                target_id: target_id.to_owned(),
                distance_feet,
                visible: true,
                armor_class,
                cover: Cover::None,
                threatening_hostile_within_five_feet: distance_feet <= 5,
                damage_profile: DamageProfile::normal(),
                target_kind: FireBoltTargetKind::Creature,
                conditions: ConditionSet::empty(),
            },
            roll_context: RollContext::normal(),
        },
        SpellId::MagicMissile => {
            let target = MagicMissileTarget {
                target_id: target_id.to_owned(),
                distance_feet,
                visible: true,
                shielded: false,
                damage_profile: DamageProfile::normal(),
            };
            SupportedSpellIntent::MagicMissile {
                darts: Box::new([target.clone(), target.clone(), target]),
            }
        }
        _ => unreachable!("unsupported spells returned before intent assembly"),
    };
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        resolve_supported_spell(
            &spellcasting,
            &mut rules.runtime_resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &intent,
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let slots_after = spell_slot_count(&rules.runtime_resources);

    let attack_outcome = resolution.attack.as_ref().map(|attack| {
        push_spell_attack_roll(rolls, attack, target_id, spell);
        let outcome = spell_attack_outcome(attack);
        facts.push(EncounterFact::AttackResolved {
            actor_id: actor_id.clone(),
            target_id: target_id.to_owned(),
            attack_id: spell.mechanic_id().to_owned(),
            distance_feet,
            range_feet: 120,
            armor_class: u16::from(attack.target_armor_class),
            attack_total: i32::from(attack.total),
            outcome,
        });
        outcome
    });
    for damage_roll in &resolution.damage_rolls {
        push_spell_damage_roll(rolls, damage_roll, target_id, spell);
    }

    let mut damage_applied = 0_u16;
    let mut damage_type = None;
    for effect in &resolution.effects {
        if let SpellEffect::Damage {
            target_id: effect_target,
            damage_type: effect_type,
            effective_damage,
            ..
        } = effect
        {
            if effect_target != target_id {
                return Err(EncounterError::InvalidState {
                    reason: "spell resolver emitted damage for an unexpected target",
                });
            }
            let mapped_type = match effect_type {
                crate::hero::DamageType::Fire => DamageType::Fire,
                crate::hero::DamageType::Force => DamageType::Force,
                _ => {
                    return Err(EncounterError::InvalidState {
                        reason: "spell resolver emitted an unexpected damage type",
                    });
                }
            };
            if damage_type.is_some_and(|existing| existing != mapped_type) {
                return Err(EncounterError::InvalidState {
                    reason: "one spell resolution mixed damage types",
                });
            }
            damage_type = Some(mapped_type);
            damage_applied = damage_applied.checked_add(*effective_damage).ok_or(
                EncounterError::InvalidState {
                    reason: "spell damage overflowed the encounter range",
                },
            )?;
        }
    }

    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::SpellCastResolved {
        actor_id: actor_id.clone(),
        target_id: target_id.to_owned(),
        spell,
        level_one_spell_slots_before: slots_before,
        level_one_spell_slots_after: slots_after,
        damage_applied,
    });

    if damage_applied > 0 {
        let critical = attack_outcome.is_some_and(AttackOutcome::is_critical);
        let application = {
            let policy = state.lethality_policy;
            let target = combatant_mut(state, target_id).expect("the fixed spell target exists");
            apply_damage(target, damage_applied, critical, policy)
        };
        facts.push(EncounterFact::DamageApplied {
            actor_id: actor_id.clone(),
            target_id: target_id.to_owned(),
            attack_id: spell.mechanic_id().to_owned(),
            damage_type: damage_type.expect("positive spell damage has a type"),
            critical,
            amount: damage_applied,
            temporary_hit_points_before: application.temporary_before,
            temporary_hit_points_absorbed: application.temporary_absorbed,
            temporary_hit_points_after: application.temporary_after,
            current_hit_points_before: application.current_before,
            current_hit_points_after: application.current_after,
        });
        wake_sleep_after_damage(state, target_id, damage_applied, facts);
        if application.life_before != application.life_after {
            let health = &state.creature.hit_points;
            facts.push(EncounterFact::LifeStatusChanged {
                participant_id: target_id.to_owned(),
                from: application.life_before,
                to: application.life_after,
                death_save_successes: health.death_saves.successes,
                death_save_failures: health.death_saves.failures,
            });
        }
        if finish_for_health_transition(state, target_id, facts)? == Some(EncounterOutcome::Victory)
        {
            return Ok(narration(
                "encounter:complete:spell-victory",
                format!(
                    "{}'s magic tears the Soot Wight apart; cleansing water carries the ash away.",
                    hero_narrative_name(state)
                ),
                "The Soot Wight is defeated by a spell. The encounter ends in victory.",
            ));
        }
    }

    if attack_outcome.is_some_and(|outcome| !outcome.is_hit()) {
        Ok(narration(
            "encounter:spell:miss",
            format!(
                "{} casts Fire Bolt, but the flame gutters past the Soot Wight.",
                hero_narrative_name(state)
            ),
            "Fire Bolt misses the Soot Wight.",
        ))
    } else {
        Ok(narration(
            "encounter:spell:damage",
            format!(
                "{} casts {}, dealing {} damage to the Soot Wight.",
                hero_narrative_name(state),
                live_spell_label(spell),
                damage_applied
            ),
            format!(
                "{} deals {damage_applied} damage to the Soot Wight.",
                live_spell_label(spell)
            ),
        ))
    }
}

fn resolve_second_wind(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    let actor_id = require_conscious_active_actor(state)?;
    if actor_id != CANAL_WARDEN_ID
        || !legal_actions(state)?.contains(&LegalEncounterAction::SecondWind)
    {
        return Err(EncounterError::IllegalIntent {
            reason: "Second Wind is not a server-derived legal action",
        });
    }
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let resource_before = rules
        .runtime_resources
        .second_wind
        .as_ref()
        .map_or(0, |resource| resource.current);
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    let current_before = state.hero.hit_points.current;
    let mut health = HealthState {
        schema_version: RULES_MATRIX_SCHEMA_VERSION,
        maximum: state.hero.hit_points.maximum,
        current: state.hero.hit_points.current,
        temporary: state.hero.hit_points.temporary,
        vital_status: VitalStatus::Active,
        death_saves: DeathSaveTally::default(),
    };
    let resolution = {
        let mut dice = RulesDiceAdapter(roll_source);
        use_second_wind(
            &mut rules.runtime_resources,
            &mut economy,
            &mut health,
            &mut dice,
        )
        .map_err(map_rules_resolution_error)?
    };
    let resource_after = rules
        .runtime_resources
        .second_wind
        .as_ref()
        .map_or(0, |resource| resource.current);
    push_second_wind_roll(rolls, &resolution, &actor_id);
    state.hero.hit_points.current = health.current;
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::ClassFeatureResolved {
        actor_id: actor_id.clone(),
        feature: FeatureId::SecondWind,
        resource: ResourceKind::SecondWind,
        resource_before,
        resource_after,
        action_available_after: economy.action_available,
        bonus_action_available_after: economy.bonus_action_available,
    });
    facts.push(EncounterFact::HealingApplied {
        actor_id,
        feature: FeatureId::SecondWind,
        requested_healing: resolution.healing.requested_healing,
        effective_healing: resolution.healing.effective_healing,
        current_hit_points_before: current_before,
        current_hit_points_after: health.current,
    });
    Ok(narration(
        "encounter:feature:second-wind",
        format!(
            "{} catches a second wind and recovers {} hit points.",
            hero_narrative_name(state),
            resolution.healing.effective_healing
        ),
        format!(
            "Second Wind restores {} hit points.",
            resolution.healing.effective_healing
        ),
    ))
}

fn resolve_action_surge(
    state: &mut EncounterState,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    let actor_id = require_conscious_active_actor(state)?;
    if actor_id != CANAL_WARDEN_ID
        || !legal_actions(state)?.contains(&LegalEncounterAction::ActionSurge)
    {
        return Err(EncounterError::IllegalIntent {
            reason: "Action Surge is not a server-derived legal action",
        });
    }
    let mut rules = state
        .hero_rules
        .clone()
        .ok_or(EncounterError::IllegalIntent {
            reason: "the hero has no live rules snapshot",
        })?;
    let resource_before = rules
        .runtime_resources
        .action_surge
        .as_ref()
        .map_or(0, |resource| resource.current);
    let mut economy = state
        .turn_resources
        .as_ref()
        .expect("active state has turn resources")
        .action_economy();
    use_action_surge(&mut rules.runtime_resources, &mut economy)
        .map_err(map_rules_resolution_error)?;
    let resource_after = rules
        .runtime_resources
        .action_surge
        .as_ref()
        .map_or(0, |resource| resource.current);
    state.turn_resources = Some(TurnResources::from_action_economy(&economy));
    state.hero_rules = Some(rules);
    facts.push(EncounterFact::ClassFeatureResolved {
        actor_id,
        feature: FeatureId::ActionSurge,
        resource: ResourceKind::ActionSurge,
        resource_before,
        resource_after,
        action_available_after: economy.action_available,
        bonus_action_available_after: economy.bonus_action_available,
    });
    Ok(narration(
        "encounter:feature:action-surge",
        format!(
            "{} surges through the opening and regains an action.",
            hero_narrative_name(state)
        ),
        "Action Surge restores the hero's action for this turn.",
    ))
}

fn spell_slot_count(resources: &RuntimeResources) -> Option<u8> {
    resources
        .level_one_spell_slots
        .as_ref()
        .map(|resource| resource.current)
}

const fn live_spell_label(spell: SpellId) -> &'static str {
    match spell {
        SpellId::FireBolt => "Fire Bolt",
        SpellId::MagicMissile => "Magic Missile",
        SpellId::Light => "Light",
        SpellId::MageHand => "Mage Hand",
        SpellId::Shield => "Shield",
        SpellId::Sleep => "Sleep",
    }
}

fn spell_attack_outcome(attack: &SpellAttackResolution) -> AttackOutcome {
    match attack.outcome {
        crate::rules_matrix::D20TestOutcome::AutomaticMiss => AttackOutcome::AutomaticMiss,
        crate::rules_matrix::D20TestOutcome::Failure => AttackOutcome::Miss,
        crate::rules_matrix::D20TestOutcome::Success => AttackOutcome::Hit,
        crate::rules_matrix::D20TestOutcome::CriticalHit => AttackOutcome::CriticalHit,
    }
}

fn push_spell_attack_roll(
    rolls: &mut Vec<RawRollFacts>,
    attack: &SpellAttackResolution,
    target_id: &str,
    spell: SpellId,
) {
    let (mode, individual_dice, kept_die_indices) = raw_d20(&attack.roll);
    let outcome = match spell_attack_outcome(attack) {
        AttackOutcome::AutomaticMiss => RawRollOutcome::AutomaticMiss,
        AttackOutcome::Miss => RawRollOutcome::Miss,
        AttackOutcome::Hit => RawRollOutcome::Hit,
        AttackOutcome::CriticalHit => RawRollOutcome::CriticalHit,
    };
    rolls.push(RawRollFacts {
        sequence: u16::try_from(rolls.len() + 1).expect("encounter rolls are bounded"),
        purpose: EncounterRollPurpose::Attack,
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: Some(target_id.to_owned()),
        action_id: Some(spell.mechanic_id().to_owned()),
        expression: dice_expression(
            u16::try_from(individual_dice.len()).expect("one or two d20s"),
            20,
            i32::from(attack.spell_attack_bonus),
        ),
        mode,
        individual_dice,
        kept_die_indices,
        modifiers: vec![RollModifierFact {
            source_id: "srd-5.1-cc:modifier:spell-attack".to_owned(),
            value: i16::from(attack.spell_attack_bonus),
        }],
        natural_d20: Some(attack.roll.selected),
        total: i32::from(attack.total),
        comparison: Some(RollComparison {
            kind: RollComparisonKind::ArmorClass,
            value: i16::from(attack.target_armor_class),
        }),
        outcome,
    });
}

fn raw_d20(roll: &D20Roll) -> (EncounterRollMode, Vec<RawDieFact>, Vec<u16>) {
    let mode = match roll.mode {
        RollMode::Normal => EncounterRollMode::Normal,
        RollMode::Advantage => EncounterRollMode::Advantage,
        RollMode::Disadvantage => EncounterRollMode::Disadvantage,
    };
    let mut dice = vec![RawDieFact {
        sides: 20,
        value: u16::from(roll.first),
    }];
    if let Some(second) = roll.second {
        dice.push(RawDieFact {
            sides: 20,
            value: u16::from(second),
        });
    }
    let kept = if roll.first == roll.selected { 0 } else { 1 };
    (mode, dice, vec![kept])
}

fn push_spell_damage_roll(
    rolls: &mut Vec<RawRollFacts>,
    damage: &DamageDiceResolution,
    target_id: &str,
    spell: SpellId,
) {
    let individual_dice = damage
        .dice
        .iter()
        .map(|value| RawDieFact {
            sides: u16::from(damage.sides),
            value: u16::from(*value),
        })
        .collect::<Vec<_>>();
    let kept_die_indices = (0..individual_dice.len())
        .map(|index| u16::try_from(index).expect("spell damage dice are bounded"))
        .collect::<Vec<_>>();
    let modifiers = (damage.constant != 0)
        .then(|| RollModifierFact {
            source_id: "srd-5.1-cc:modifier:spell-damage".to_owned(),
            value: i16::from(damage.constant),
        })
        .into_iter()
        .collect();
    rolls.push(RawRollFacts {
        sequence: u16::try_from(rolls.len() + 1).expect("encounter rolls are bounded"),
        purpose: EncounterRollPurpose::Damage,
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: Some(target_id.to_owned()),
        action_id: Some(spell.mechanic_id().to_owned()),
        expression: dice_expression(
            u16::try_from(individual_dice.len()).expect("spell damage dice are bounded"),
            u16::from(damage.sides),
            i32::from(damage.constant),
        ),
        mode: EncounterRollMode::Normal,
        individual_dice,
        kept_die_indices,
        modifiers,
        natural_d20: None,
        total: i32::from(damage.total),
        comparison: None,
        outcome: RawRollOutcome::Total,
    });
}

fn push_sleep_roll(rolls: &mut Vec<RawRollFacts>, damage: &DamageDiceResolution) {
    let individual_dice = damage
        .dice
        .iter()
        .map(|value| RawDieFact {
            sides: u16::from(damage.sides),
            value: u16::from(*value),
        })
        .collect::<Vec<_>>();
    rolls.push(RawRollFacts {
        sequence: u16::try_from(rolls.len() + 1).expect("encounter rolls are bounded"),
        purpose: EncounterRollPurpose::SleepHitPoints,
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: Some(SOOT_WIGHT_ID.to_owned()),
        action_id: Some(SpellId::Sleep.mechanic_id().to_owned()),
        expression: dice_expression(5, 8, 0),
        mode: EncounterRollMode::Normal,
        individual_dice,
        kept_die_indices: (0..5).collect(),
        modifiers: Vec::new(),
        natural_d20: None,
        total: i32::from(damage.total),
        comparison: None,
        outcome: RawRollOutcome::Total,
    });
}

fn push_hit_die_roll(
    rolls: &mut Vec<RawRollFacts>,
    resolution: &crate::rules_matrix::HitDieSpendResolution,
) {
    let sides = match resolution.hit_die {
        ResourceKind::HitDiceD6 => 6,
        ResourceKind::HitDiceD10 => 10,
        _ => unreachable!("validated hit-die resolution has a hit-die resource"),
    };
    let modifier = i16::from(resolution.constitution_modifier);
    rolls.push(RawRollFacts {
        sequence: u16::try_from(rolls.len() + 1).expect("encounter rolls are bounded"),
        purpose: EncounterRollPurpose::HitDie,
        actor_id: CANAL_WARDEN_ID.to_owned(),
        target_id: Some(CANAL_WARDEN_ID.to_owned()),
        action_id: Some(HIT_DIE_ACTION_ID.to_owned()),
        expression: dice_expression(1, sides, i32::from(modifier)),
        mode: EncounterRollMode::Normal,
        individual_dice: vec![RawDieFact {
            sides,
            value: u16::from(resolution.roll),
        }],
        kept_die_indices: vec![0],
        modifiers: vec![RollModifierFact {
            source_id: "srd-5.1-cc:modifier:constitution".to_owned(),
            value: modifier,
        }],
        natural_d20: None,
        total: i32::from(resolution.roll) + i32::from(modifier),
        comparison: None,
        outcome: RawRollOutcome::Total,
    });
}

fn push_second_wind_roll(
    rolls: &mut Vec<RawRollFacts>,
    resolution: &crate::rules_matrix::SecondWindResolution,
    actor_id: &str,
) {
    rolls.push(RawRollFacts {
        sequence: u16::try_from(rolls.len() + 1).expect("encounter rolls are bounded"),
        purpose: EncounterRollPurpose::Healing,
        actor_id: actor_id.to_owned(),
        target_id: Some(actor_id.to_owned()),
        action_id: Some(SECOND_WIND_ACTION_ID.to_owned()),
        expression: dice_expression(1, 10, i32::from(resolution.level_bonus)),
        mode: EncounterRollMode::Normal,
        individual_dice: vec![RawDieFact {
            sides: 10,
            value: u16::from(resolution.healing_roll),
        }],
        kept_die_indices: vec![0],
        modifiers: vec![RollModifierFact {
            source_id: "srd-5.1-cc:modifier:class-level".to_owned(),
            value: i16::from(resolution.level_bonus),
        }],
        natural_d20: None,
        total: i32::from(resolution.healing.requested_healing),
        comparison: None,
        outcome: RawRollOutcome::Total,
    });
}

fn resolve_context_action(
    state: &mut EncounterState,
    action_id: &str,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    let actor_id = require_conscious_active_actor(state)?;
    if actor_id != CANAL_WARDEN_ID || action_id != RELEASE_SLUICE_ACTION_ID {
        return Err(EncounterError::IllegalIntent {
            reason: "the context action is unavailable to the current actor",
        });
    }
    if state.objectives.contextual.status != ObjectiveStatus::Pending {
        return Err(EncounterError::IllegalIntent {
            reason: "the cleansing sluice has already been released",
        });
    }
    let actor_position = state.hero.position_feet;
    if distance(actor_position, state.map.sluice_position_feet) > state.map.context_range_feet {
        return Err(EncounterError::IllegalIntent {
            reason: "the Canal Warden is too far from the sluice control",
        });
    }
    if !state
        .turn_resources
        .as_ref()
        .expect("active state has resources")
        .object_interaction_available
    {
        return Err(EncounterError::IllegalIntent {
            reason: "the current turn's object interaction has already been spent",
        });
    }
    state
        .turn_resources
        .as_mut()
        .expect("active state has resources")
        .object_interaction_available = false;
    let removed_temporary_hit_points = state.creature.hit_points.temporary;
    state.creature.hit_points.temporary = 0;
    state
        .creature
        .status_effects
        .retain(|effect| *effect != CombatantStatusEffect::SootVeil);
    state.objectives.contextual.status = ObjectiveStatus::Completed;
    facts.push(EncounterFact::ContextActionResolved {
        actor_id,
        action_id: RELEASE_SLUICE_ACTION_ID.to_owned(),
        removed_temporary_hit_points,
        objective_id: RELEASE_SLUICE_OBJECTIVE_ID.to_owned(),
    });
    Ok(narration(
        "encounter:context:release-sluice",
        "The Warden throws the old lever. Canal water lashes through the chamber and tears away the wight's soot veil.",
        "The cleansing sluice is released. The Soot Wight loses its temporary hit points.",
    ))
}

fn resolve_end_turn(
    state: &mut EncounterState,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    require_conscious_active_actor(state)?;
    let ended_actor = state
        .current_actor_id
        .clone()
        .expect("active state has current actor");
    advance_turn(state, facts)?;
    let next_actor = state
        .current_actor_id
        .clone()
        .expect("active state has next actor");
    Ok(narration(
        "encounter:turn:end",
        format!(
            "{} yields the moment; {} takes the next turn.",
            combatant(state, &ended_actor)
                .expect("ended actor remains present")
                .name,
            combatant(state, &next_actor)
                .expect("next actor remains present")
                .name
        ),
        format!("Turn ended. {} acts next.", next_actor),
    ))
}

fn resolve_death_save(
    state: &mut EncounterState,
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<DeterministicNarration> {
    if state.status != EncounterStatus::Active
        || state.current_actor_id.as_deref() != Some(CANAL_WARDEN_ID)
        || state.hero.life_status != LifeStatus::Unconscious
        || state.lethality_policy != LethalityPolicy::RulesAsWritten
    {
        return Err(EncounterError::IllegalIntent {
            reason: "a death save is only legal for the unconscious hero on their turn",
        });
    }
    let death_roll = perform_roll(
        roll_source,
        rolls,
        EncounterRollPurpose::DeathSave,
        CANAL_WARDEN_ID,
        Some(CANAL_WARDEN_ID),
        None,
        1,
        20,
        Vec::new(),
        Some(RollComparison {
            kind: RollComparisonKind::DeathSaveDifficultyClass,
            value: 10,
        }),
    )?;
    let natural = death_roll.natural_d20.expect("death save is a d20");
    let before = state.hero.hit_points.death_saves;
    let life_before = state.hero.life_status;
    match natural {
        20 => {
            state.hero.hit_points.current = 1;
            state.hero.hit_points.death_saves = DeathSaves::default();
            state.hero.life_status = LifeStatus::Conscious;
        }
        1 => {
            state.hero.hit_points.death_saves.failures = state
                .hero
                .hit_points
                .death_saves
                .failures
                .saturating_add(2)
                .min(3);
        }
        10..=19 => {
            state.hero.hit_points.death_saves.successes = state
                .hero
                .hit_points
                .death_saves
                .successes
                .saturating_add(1)
                .min(3);
        }
        _ => {
            state.hero.hit_points.death_saves.failures = state
                .hero
                .hit_points
                .death_saves
                .failures
                .saturating_add(1)
                .min(3);
        }
    }
    if state.hero.hit_points.death_saves.failures == 3 {
        state.hero.life_status = LifeStatus::Dead;
    } else if state.hero.hit_points.death_saves.successes == 3 {
        state.hero.life_status = LifeStatus::Stable;
    }
    let after = state.hero.hit_points.death_saves;
    facts.push(EncounterFact::DeathSaveResolved {
        actor_id: CANAL_WARDEN_ID.to_owned(),
        natural_roll: natural,
        successes_before: before.successes,
        successes_after: after.successes,
        failures_before: before.failures,
        failures_after: after.failures,
        life_status_after: state.hero.life_status,
    });
    if life_before != state.hero.life_status {
        facts.push(EncounterFact::LifeStatusChanged {
            participant_id: CANAL_WARDEN_ID.to_owned(),
            from: life_before,
            to: state.hero.life_status,
            death_save_successes: after.successes,
            death_save_failures: after.failures,
        });
    }

    match state.hero.life_status {
        LifeStatus::Conscious => {
            let hero_name = hero_narrative_name(state);
            let authored = if state.hero.source_character_id.is_none() {
                "The Warden drags in a breath and rises with one last chance to act.".to_owned()
            } else {
                format!("{hero_name} drags in a breath and rises with one last chance to act.")
            };
            Ok(narration(
                "encounter:death-save:natural-twenty",
                authored,
                format!("Natural 20: {hero_name} regains 1 hit point."),
            ))
        }
        LifeStatus::Stable => {
            complete_defeat(state, DefeatReason::HeroStable, facts);
            Ok(narration(
                "encounter:complete:stable-defeat",
                "The Warden's breathing steadies, but the viaduct is lost for now.",
                "Three successful death saves stabilize the hero. The encounter ends with no reward.",
            ))
        }
        LifeStatus::Dead => {
            complete_defeat(state, DefeatReason::HeroDead, facts);
            Ok(narration(
                "encounter:complete:raw-death",
                "The last echo under the viaduct fades. The Warden does not rise.",
                "Three failed death saves kill the hero. The encounter ends with no reward.",
            ))
        }
        LifeStatus::Unconscious => {
            advance_turn(state, facts)?;
            Ok(if natural == 1 {
                narration(
                    "encounter:death-save:natural-one",
                    "The Warden's breath catches; the darkness draws much closer.",
                    "Natural 1: the hero records two failed death saves.",
                )
            } else if natural >= 10 {
                narration(
                    "encounter:death-save:success",
                    "The Warden holds to a thin, stubborn rhythm of breath.",
                    "The hero succeeds on a death save.",
                )
            } else {
                narration(
                    "encounter:death-save:failure",
                    "The Warden lies still as soot settles over the stones.",
                    "The hero fails a death save.",
                )
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_roll(
    roll_source: &mut impl EncounterRollSource,
    rolls: &mut Vec<RawRollFacts>,
    purpose: EncounterRollPurpose,
    actor_id: &str,
    target_id: Option<&str>,
    action_id: Option<&str>,
    dice_count: u16,
    sides: u16,
    modifiers: Vec<RollModifierFact>,
    comparison: Option<RollComparison>,
) -> EncounterResult<RawRollFacts> {
    let mut individual_dice = Vec::with_capacity(usize::from(dice_count));
    for _ in 0..dice_count {
        let value = roll_source.roll_die(sides);
        if value == 0 || value > sides {
            return Err(EncounterError::InvalidRoll { sides, value });
        }
        individual_dice.push(RawDieFact { sides, value });
    }
    let kept_die_indices = (0..dice_count).collect::<Vec<_>>();
    let dice_total = individual_dice
        .iter()
        .fold(0_i32, |total, die| total + i32::from(die.value));
    let modifier_total = modifiers
        .iter()
        .fold(0_i32, |total, modifier| total + i32::from(modifier.value));
    let total = dice_total + modifier_total;
    let natural_d20 = if dice_count == 1 && sides == 20 {
        Some(u8::try_from(individual_dice[0].value).expect("validated d20 fits u8"))
    } else {
        None
    };
    let outcome = match (purpose, comparison.as_ref(), natural_d20) {
        (EncounterRollPurpose::Attack, Some(target), Some(natural)) => match natural {
            1 => RawRollOutcome::AutomaticMiss,
            20 => RawRollOutcome::CriticalHit,
            _ if total >= i32::from(target.value) => RawRollOutcome::Hit,
            _ => RawRollOutcome::Miss,
        },
        (EncounterRollPurpose::DeathSave, Some(target), Some(natural)) => match natural {
            1 => RawRollOutcome::NaturalOneFailure,
            20 => RawRollOutcome::NaturalTwentyRecovery,
            _ if total >= i32::from(target.value) => RawRollOutcome::Success,
            _ => RawRollOutcome::Failure,
        },
        (EncounterRollPurpose::Initiative | EncounterRollPurpose::Damage, None, _) => {
            RawRollOutcome::Total
        }
        _ => {
            return Err(EncounterError::InvalidState {
                reason: "internal roll request has an invalid purpose or comparison",
            });
        }
    };
    let sequence = u16::try_from(rolls.len())
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(EncounterError::InvalidState {
            reason: "too many rolls in one encounter resolution",
        })?;
    let roll = RawRollFacts {
        sequence,
        purpose,
        actor_id: actor_id.to_owned(),
        target_id: target_id.map(str::to_owned),
        action_id: action_id.map(str::to_owned),
        expression: dice_expression(dice_count, sides, modifier_total),
        mode: EncounterRollMode::Normal,
        individual_dice,
        kept_die_indices,
        modifiers,
        natural_d20,
        total,
        comparison,
        outcome,
    };
    roll.validate()?;
    rolls.push(roll.clone());
    Ok(roll)
}

fn dice_expression(dice_count: u16, sides: u16, modifier: i32) -> String {
    match modifier.cmp(&0) {
        std::cmp::Ordering::Greater => format!("{dice_count}d{sides}+{modifier}"),
        std::cmp::Ordering::Less => format!("{dice_count}d{sides}{modifier}"),
        std::cmp::Ordering::Equal => format!("{dice_count}d{sides}"),
    }
}

fn modifier_fact(definition: &ModifierDefinition) -> RollModifierFact {
    RollModifierFact {
        source_id: definition.source_id.to_owned(),
        value: definition.value,
    }
}

fn attacks_for_actor<'a>(
    state: &'a EncounterState,
    actor_id: &str,
) -> Option<Vec<ResolvedAttack<'a>>> {
    match actor_id {
        CANAL_WARDEN_ID if state.hero.attacks.is_empty() => {
            Some(vec![ResolvedAttack::Fixed(HERO_ATTACK)])
        }
        CANAL_WARDEN_ID => Some(
            state
                .hero
                .attacks
                .iter()
                .map(ResolvedAttack::Snapshot)
                .collect(),
        ),
        SOOT_WIGHT_ID => Some(vec![ResolvedAttack::Fixed(CREATURE_ATTACK)]),
        _ => None,
    }
}

fn attack_for_actor<'a>(
    state: &'a EncounterState,
    actor_id: &str,
    attack_id: &str,
) -> Option<ResolvedAttack<'a>> {
    attacks_for_actor(state, actor_id)?
        .into_iter()
        .find(|attack| attack.attack_id() == attack_id)
}

fn opposing_combatant<'a>(state: &'a EncounterState, actor_id: &str) -> Option<&'a CombatantState> {
    match actor_id {
        CANAL_WARDEN_ID => Some(&state.creature),
        SOOT_WIGHT_ID => Some(&state.hero),
        _ => None,
    }
}

fn combatant<'a>(state: &'a EncounterState, id: &str) -> Option<&'a CombatantState> {
    match id {
        CANAL_WARDEN_ID => Some(&state.hero),
        SOOT_WIGHT_ID => Some(&state.creature),
        _ => None,
    }
}

fn combatant_mut<'a>(state: &'a mut EncounterState, id: &str) -> Option<&'a mut CombatantState> {
    match id {
        CANAL_WARDEN_ID => Some(&mut state.hero),
        SOOT_WIGHT_ID => Some(&mut state.creature),
        _ => None,
    }
}

fn require_conscious_active_actor(state: &EncounterState) -> EncounterResult<String> {
    if state.status != EncounterStatus::Active {
        return Err(EncounterError::IllegalIntent {
            reason: "the encounter is not active",
        });
    }
    let actor = state.current_actor().ok_or(EncounterError::InvalidState {
        reason: "active state has no current actor",
    })?;
    if actor.life_status != LifeStatus::Conscious {
        return Err(EncounterError::IllegalIntent {
            reason: "the current actor cannot take ordinary actions",
        });
    }
    Ok(actor.id.clone())
}

fn set_current_turn(state: &mut EncounterState, index: usize) -> EncounterResult<()> {
    let actor_id = state
        .initiative
        .as_ref()
        .and_then(|initiative| initiative.order.get(index))
        .cloned()
        .ok_or(EncounterError::InvalidState {
            reason: "current actor index is outside initiative order",
        })?;
    let speed = combatant(state, &actor_id)
        .ok_or(EncounterError::InvalidState {
            reason: "initiative actor is not a participant",
        })?
        .speed_feet;
    state.current_actor_index = Some(u8::try_from(index).expect("two initiative entries fit u8"));
    let rules = (actor_id == CANAL_WARDEN_ID)
        .then_some(state.hero_rules.as_ref())
        .flatten();
    let resources = TurnResources::fresh(speed, rules)?;
    state.current_actor_id = Some(actor_id);
    state.turn_resources = Some(resources);
    if state.schema_version == ENCOUNTER_SCHEMA_VERSION
        && state.current_actor_id.as_deref() == Some(CANAL_WARDEN_ID)
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        live.hero_reaction_available = true;
        live.shield_ward = None;
    }
    Ok(())
}

fn advance_live_q04_round(
    state: &mut EncounterState,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<()> {
    if state.schema_version != ENCOUNTER_SCHEMA_VERSION {
        return Ok(());
    }
    let mut expired_lights = Vec::new();
    {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        for object in &mut live.objects {
            if let Some(rounds) = &mut object.light_remaining_rounds {
                if *rounds == 1 {
                    object.light_remaining_rounds = None;
                    expired_lights.push(object.object_id.clone());
                } else {
                    *rounds -= 1;
                }
            }
        }
        if live.mage_hand.is_some()
            && advance_mage_hand_duration(&mut live.mage_hand)
                .map_err(map_rules_resolution_error)?
        {
            live.mage_hand_position_feet = None;
            facts.push(EncounterFact::MageHandExpired {
                hand_id: MAGE_HAND_ID.to_owned(),
            });
        }
    }
    for object_id in expired_lights {
        facts.push(EncounterFact::LightExpired { object_id });
    }
    let expired_sleep = {
        let live = state
            .live_q04
            .as_mut()
            .expect("schema-v3 state has live Q04 state");
        match &mut live.sleep {
            Some(sleep) if sleep.remaining_rounds == 1 => live.sleep.take(),
            Some(sleep) => {
                sleep.remaining_rounds -= 1;
                None
            }
            None => None,
        }
    };
    if let Some(sleep) = expired_sleep {
        state
            .creature
            .status_effects
            .retain(|effect| *effect != CombatantStatusEffect::MagicallyAsleep);
        facts.push(EncounterFact::SleepEnded {
            target_id: sleep.target_id,
            reason: SleepEndReason::DurationExpired,
        });
    }
    Ok(())
}

fn advance_turn(state: &mut EncounterState, facts: &mut Vec<EncounterFact>) -> EncounterResult<()> {
    let initiative_len = state
        .initiative
        .as_ref()
        .ok_or(EncounterError::InvalidState {
            reason: "cannot advance a turn without initiative",
        })?
        .order
        .len();
    let old_index = usize::from(
        state
            .current_actor_index
            .ok_or(EncounterError::InvalidState {
                reason: "cannot advance a turn without a current actor",
            })?,
    );
    let ended_actor_id = state
        .current_actor_id
        .clone()
        .ok_or(EncounterError::InvalidState {
            reason: "cannot advance a turn without a current actor",
        })?;
    let next_index = (old_index + 1) % initiative_len;
    if next_index == 0 {
        state.round = state
            .round
            .checked_add(1)
            .ok_or(EncounterError::RoundOverflow)?;
        advance_live_q04_round(state, facts)?;
    }
    set_current_turn(state, next_index)?;
    facts.push(EncounterFact::TurnEnded {
        actor_id: ended_actor_id,
        next_actor_id: state
            .current_actor_id
            .clone()
            .expect("new current actor was set"),
        round: state.round,
    });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DamageApplication {
    temporary_before: u16,
    temporary_absorbed: u16,
    temporary_after: u16,
    current_before: u16,
    current_after: u16,
    life_before: LifeStatus,
    life_after: LifeStatus,
}

fn apply_damage(
    target: &mut CombatantState,
    amount: u16,
    critical: bool,
    policy: LethalityPolicy,
) -> DamageApplication {
    let temporary_before = target.hit_points.temporary;
    let current_before = target.hit_points.current;
    let life_before = target.life_status;
    let temporary_absorbed = amount.min(target.hit_points.temporary);
    target.hit_points.temporary -= temporary_absorbed;
    let hit_point_damage = amount - temporary_absorbed;

    if target.kind == CombatantKind::Creature {
        target.hit_points.current = target.hit_points.current.saturating_sub(hit_point_damage);
        if target.hit_points.current == 0 {
            target.life_status = LifeStatus::Dead;
        }
    } else if policy == LethalityPolicy::StoryRecovery {
        target.hit_points.current = target.hit_points.current.saturating_sub(hit_point_damage);
        if target.hit_points.current == 0 {
            target.life_status = LifeStatus::Unconscious;
            target.hit_points.death_saves = DeathSaves::default();
        }
    } else if target.hit_points.current > 0 {
        if hit_point_damage >= target.hit_points.current {
            let remaining_damage = hit_point_damage - target.hit_points.current;
            target.hit_points.current = 0;
            target.hit_points.death_saves = DeathSaves::default();
            if remaining_damage >= target.hit_points.maximum {
                target.life_status = LifeStatus::Dead;
                target.hit_points.death_saves.failures = 3;
            } else {
                target.life_status = LifeStatus::Unconscious;
            }
        } else {
            target.hit_points.current -= hit_point_damage;
        }
    } else if hit_point_damage > 0 {
        if hit_point_damage >= target.hit_points.maximum {
            target.life_status = LifeStatus::Dead;
            target.hit_points.death_saves.failures = 3;
        } else {
            if target.life_status == LifeStatus::Stable {
                target.life_status = LifeStatus::Unconscious;
                target.hit_points.death_saves.successes = 0;
            }
            target.hit_points.death_saves.failures = target
                .hit_points
                .death_saves
                .failures
                .saturating_add(if critical { 2 } else { 1 })
                .min(3);
            if target.hit_points.death_saves.failures == 3 {
                target.life_status = LifeStatus::Dead;
            }
        }
    }

    DamageApplication {
        temporary_before,
        temporary_absorbed,
        temporary_after: target.hit_points.temporary,
        current_before,
        current_after: target.hit_points.current,
        life_before,
        life_after: target.life_status,
    }
}

fn finish_for_health_transition(
    state: &mut EncounterState,
    target_id: &str,
    facts: &mut Vec<EncounterFact>,
) -> EncounterResult<Option<EncounterOutcome>> {
    if target_id == SOOT_WIGHT_ID && state.creature.life_status == LifeStatus::Dead {
        complete_victory(state, facts);
        return Ok(Some(EncounterOutcome::Victory));
    }
    if target_id == CANAL_WARDEN_ID {
        match (state.lethality_policy, state.hero.life_status) {
            (LethalityPolicy::StoryRecovery, LifeStatus::Unconscious) => {
                complete_defeat(state, DefeatReason::HeroUnconscious, facts);
                return Ok(Some(EncounterOutcome::Defeat));
            }
            (LethalityPolicy::RulesAsWritten, LifeStatus::Dead) => {
                complete_defeat(state, DefeatReason::HeroDead, facts);
                return Ok(Some(EncounterOutcome::Defeat));
            }
            _ => {}
        }
    }
    Ok(None)
}

fn complete_victory(state: &mut EncounterState, facts: &mut Vec<EncounterFact>) {
    state.status = EncounterStatus::Victory;
    state.current_actor_index = None;
    state.current_actor_id = None;
    state.turn_resources = None;
    state.objectives.primary.status = ObjectiveStatus::Completed;
    state.reward_eligibility = RewardEligibility::Eligible {
        tier: EncounterRewardTier::Major,
    };
    state.transition = Some(ExplorationTransition {
        destination_id: EXPLORATION_DESTINATION_ID.to_owned(),
        outcome: EncounterOutcome::Victory,
        defeat_reason: None,
        hero_current_hit_points: state.hero.hit_points.current,
        hero_life_status: state.hero.life_status,
        story_recovery_applied: false,
    });
    facts.push(EncounterFact::EncounterCompleted {
        outcome: EncounterOutcome::Victory,
        defeat_reason: None,
        reward_eligible: true,
        story_recovery_applied: false,
    });
}

fn complete_defeat(
    state: &mut EncounterState,
    reason: DefeatReason,
    facts: &mut Vec<EncounterFact>,
) {
    let story_recovery = state.lethality_policy == LethalityPolicy::StoryRecovery;
    state.status = EncounterStatus::Defeat;
    state.current_actor_index = None;
    state.current_actor_id = None;
    state.turn_resources = None;
    state.objectives.primary.status = ObjectiveStatus::Failed;
    state.reward_eligibility = RewardEligibility::Ineligible {
        reason: RewardIneligibilityReason::EncounterDefeat,
    };
    state.transition = Some(ExplorationTransition {
        destination_id: EXPLORATION_DESTINATION_ID.to_owned(),
        outcome: EncounterOutcome::Defeat,
        defeat_reason: Some(reason),
        hero_current_hit_points: if story_recovery {
            1
        } else {
            state.hero.hit_points.current
        },
        hero_life_status: if story_recovery {
            LifeStatus::Conscious
        } else {
            state.hero.life_status
        },
        story_recovery_applied: story_recovery,
    });
    facts.push(EncounterFact::EncounterCompleted {
        outcome: EncounterOutcome::Defeat,
        defeat_reason: Some(reason),
        reward_eligible: false,
        story_recovery_applied: story_recovery,
    });
}

fn hero_narrative_name(state: &EncounterState) -> String {
    if state.hero.source_character_id.is_none() {
        format!("The {}", state.hero.name)
    } else {
        state.hero.name.clone()
    }
}

fn distance(left: u16, right: u16) -> u16 {
    left.abs_diff(right)
}

fn narration(
    narration_id: impl Into<String>,
    authored_text: impl Into<String>,
    fallback_text: impl Into<String>,
) -> DeterministicNarration {
    DeterministicNarration {
        narration_id: narration_id.into(),
        authored_text: authored_text.into(),
        fallback_text: fallback_text.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    struct SequenceRolls {
        values: VecDeque<u16>,
        requested_sides: Vec<u16>,
    }

    impl SequenceRolls {
        fn new(values: impl IntoIterator<Item = u16>) -> Self {
            Self {
                values: values.into_iter().collect(),
                requested_sides: Vec::new(),
            }
        }

        fn assert_exhausted(&self) {
            assert!(self.values.is_empty(), "unused rolls: {:?}", self.values);
        }
    }

    impl EncounterRollSource for SequenceRolls {
        fn roll_die(&mut self, sides: u16) -> u16 {
            self.requested_sides.push(sides);
            self.values.pop_front().expect("test roll value")
        }
    }

    struct PanicRolls;

    impl EncounterRollSource for PanicRolls {
        fn roll_die(&mut self, _: u16) -> u16 {
            panic!("illegal intent must not consume a roll")
        }
    }

    fn command(state: &EncounterState, key: &str, intent: EncounterIntent) -> EncounterCommand {
        EncounterCommand::new(state.revision, key, intent)
    }

    fn started(
        policy: LethalityPolicy,
        opening: OpeningConsequence,
        hero_initiative: u16,
        creature_initiative: u16,
    ) -> EncounterState {
        let state = EncounterState::new(policy, opening);
        let command = command(&state, "start-command", EncounterIntent::StartEncounter);
        let mut rolls = SequenceRolls::new([hero_initiative, creature_initiative]);
        let resolution = resolve_encounter(&state, &command, &mut rolls).unwrap();
        rolls.assert_exhausted();
        resolution.state
    }

    fn created_hero_ready(class: HeroClass, level: crate::hero::SupportedLevel) -> EncounterState {
        let rules = EncounterHeroRulesProfile {
            runtime_resources: RuntimeResources::new(class, level),
            spellcasting: (class == HeroClass::Wizard).then(|| SpellcastingState {
                schema_version: RULES_MATRIX_SCHEMA_VERSION,
                caster_id: CANAL_WARDEN_ID.to_owned(),
                spell_attack_bonus: 5,
                spell_save_dc: 13,
                cantrips: SpellId::CANTRIPS.to_vec(),
                prepared: SpellId::LEVEL_ONE.to_vec(),
            }),
            constitution_modifier: Some(2),
        };
        EncounterState::new_for_hero(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            EncounterHeroProfile {
                source_character_id: format!("created-{class:?}-{level:?}").to_lowercase(),
                name: "Mara Vale".to_owned(),
                armor_class: 16,
                speed_feet: 30,
                initiative_modifier: 3,
                current_hit_points: 12,
                maximum_hit_points: 12,
                attacks: vec![EncounterAttack {
                    attack_id: "srd-5.1-cc:attack:test-crossbow".to_owned(),
                    range_feet: 80,
                    attack_modifiers: vec![RollModifierFact {
                        source_id: "srd-5.1-cc:modifier:test-attack".to_owned(),
                        value: 5,
                    }],
                    damage_die_sides: 8,
                    damage_modifier: RollModifierFact {
                        source_id: "srd-5.1-cc:modifier:test-damage".to_owned(),
                        value: 3,
                    },
                    damage_type: DamageType::Piercing,
                }],
                rules: Some(rules),
            },
        )
        .unwrap()
    }

    fn start_created_hero(class: HeroClass, level: crate::hero::SupportedLevel) -> EncounterState {
        let ready = created_hero_ready(class, level);
        let mut rolls = SequenceRolls::new([20, 1]);
        resolve_encounter(
            &ready,
            &command(
                &ready,
                "created-rules-start",
                EncounterIntent::StartEncounter,
            ),
            &mut rolls,
        )
        .unwrap()
        .state
    }

    fn moved(state: &EncounterState, destination_feet: u16) -> EncounterState {
        let command = command(
            state,
            "move-command",
            EncounterIntent::Move { destination_feet },
        );
        resolve_encounter(state, &command, &mut PanicRolls)
            .unwrap()
            .state
    }

    fn unconscious_hero_turn() -> EncounterState {
        let mut state = started(
            LethalityPolicy::RulesAsWritten,
            OpeningConsequence::RunesUnderstood,
            20,
            1,
        );
        assert_eq!(state.current_actor_id.as_deref(), Some(CANAL_WARDEN_ID));
        state.hero.hit_points.current = 0;
        state.hero.hit_points.temporary = 0;
        state.hero.hit_points.death_saves = DeathSaves::default();
        state.hero.life_status = LifeStatus::Unconscious;
        state.validate().unwrap();
        state
    }

    #[test]
    fn intent_schema_is_strict_and_carries_no_client_mechanics() {
        let base = json!({
            "schema_version": ENCOUNTER_SCHEMA_VERSION,
            "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
            "expected_revision": 1,
            "idempotency_key": "intent-1",
            "intent": {
                "type": "attack",
                "attack_id": CANAL_WARDEN_ATTACK_ID,
                "target_id": SOOT_WIGHT_ID
            }
        });
        let decoded: EncounterCommand = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(decoded.expected_revision, 1);
        let encoded = serde_json::to_value(decoded).unwrap();
        for forbidden in [
            "actor",
            "roll",
            "armor_class",
            "modifier",
            "damage",
            "hit_points",
            "experience_points",
            "timestamp",
        ] {
            assert!(encoded.get(forbidden).is_none());

            let mut forged_outer = base.clone();
            forged_outer
                .as_object_mut()
                .unwrap()
                .insert(forbidden.to_owned(), json!(99));
            assert!(serde_json::from_value::<EncounterCommand>(forged_outer).is_err());

            let mut forged_intent = base.clone();
            forged_intent["intent"]
                .as_object_mut()
                .unwrap()
                .insert(forbidden.to_owned(), json!(99));
            assert!(serde_json::from_value::<EncounterCommand>(forged_intent).is_err());
        }

        let mut future = base;
        future["schema_version"] = json!(ENCOUNTER_SCHEMA_VERSION + 1);
        assert!(serde_json::from_value::<EncounterCommand>(future).is_err());
    }

    #[test]
    fn move_accepts_canonical_form_scalar_but_rejects_ambiguous_numbers() {
        let command: EncounterCommand = serde_json::from_value(json!({
            "schema_version": ENCOUNTER_SCHEMA_VERSION,
            "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
            "expected_revision": 1,
            "idempotency_key": "move-form-scalar",
            "intent": {
                "type": "move",
                "destination_feet": "5"
            }
        }))
        .unwrap();
        assert_eq!(
            command.intent,
            EncounterIntent::Move {
                destination_feet: 5
            }
        );

        for invalid in ["", "05", "+5", "-1", "5.0", "65536"] {
            let value = json!({
                "schema_version": ENCOUNTER_SCHEMA_VERSION,
                "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
                "expected_revision": 1,
                "idempotency_key": "move-form-invalid",
                "intent": {
                    "type": "move",
                    "destination_feet": invalid
                }
            });
            assert!(serde_json::from_value::<EncounterCommand>(value).is_err());
        }
    }

    #[test]
    fn fixed_content_ids_are_namespaced_and_opaque_id_safe() {
        for id in [
            SOOT_WIGHT_ENCOUNTER_ID,
            CANAL_WARDEN_ID,
            SOOT_WIGHT_ID,
            CANAL_WARDEN_ATTACK_ID,
            SOOT_WIGHT_ATTACK_ID,
            RELEASE_SLUICE_ACTION_ID,
            DEFEAT_SOOT_WIGHT_OBJECTIVE_ID,
            RELEASE_SLUICE_OBJECTIVE_ID,
        ] {
            assert!(id.contains(':'));
            assert!(is_valid_opaque_id(id), "invalid fixed ID: {id}");
        }
    }

    #[test]
    fn opening_check_success_and_failure_change_canonical_health() {
        let success = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        let failure = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesMisread,
        );

        assert_eq!(success.hero.hit_points.temporary, 2);
        assert_eq!(success.creature.hit_points.temporary, 2);
        assert_eq!(failure.hero.hit_points.temporary, 0);
        assert_eq!(failure.creature.hit_points.temporary, 4);
        assert!(
            success
                .hero
                .status_effects
                .contains(&CombatantStatusEffect::RuneWard)
        );
        success.validate().unwrap();
        failure.validate().unwrap();
    }

    #[test]
    fn authoritative_hero_profile_drives_stats_and_weapon_actions() {
        let profile = EncounterHeroProfile {
            source_character_id: "created-wizard".to_owned(),
            name: "Iris Quill".to_owned(),
            armor_class: 13,
            speed_feet: 30,
            initiative_modifier: 3,
            current_hit_points: 7,
            maximum_hit_points: 7,
            attacks: vec![
                EncounterAttack {
                    attack_id: "attack:simple:dagger".to_owned(),
                    range_feet: 5,
                    attack_modifiers: vec![RollModifierFact {
                        source_id: "srd-5.1-cc:modifier:dexterity".to_owned(),
                        value: 5,
                    }],
                    damage_die_sides: 4,
                    damage_modifier: RollModifierFact {
                        source_id: "manchester-arcana:modifier:derived-weapon-damage".to_owned(),
                        value: 3,
                    },
                    damage_type: DamageType::Piercing,
                },
                EncounterAttack {
                    attack_id: "attack:light-crossbow".to_owned(),
                    range_feet: 80,
                    attack_modifiers: vec![RollModifierFact {
                        source_id: "srd-5.1-cc:modifier:dexterity".to_owned(),
                        value: 5,
                    }],
                    damage_die_sides: 8,
                    damage_modifier: RollModifierFact {
                        source_id: "manchester-arcana:modifier:derived-weapon-damage".to_owned(),
                        value: 3,
                    },
                    damage_type: DamageType::Piercing,
                },
            ],
            rules: None,
        };
        let ready = EncounterState::new_for_hero(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            profile.clone(),
        )
        .unwrap();
        assert_eq!(ready.hero_profile(), Some(profile));

        let mut rolls = SequenceRolls::new([20, 1]);
        let active = resolve_encounter(
            &ready,
            &command(&ready, "profile-start", EncounterIntent::StartEncounter),
            &mut rolls,
        )
        .unwrap()
        .state;
        let actions = legal_actions(&active).unwrap();
        assert!(actions.contains(&LegalEncounterAction::Attack {
            attack_id: "attack:light-crossbow".to_owned(),
            target_id: SOOT_WIGHT_ID.to_owned(),
            range_feet: 80,
        }));
        assert!(!actions.iter().any(|action| matches!(
            action,
            LegalEncounterAction::Attack { attack_id, .. }
                if attack_id == "attack:simple:dagger"
        )));
    }

    #[test]
    fn initiative_tie_uses_modifier_then_records_the_tie() {
        // 10 + 2 and 11 + 1 both total 12; the Warden's higher modifier wins.
        let initial = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        let mut rolls = SequenceRolls::new([10, 11]);
        let resolution = resolve_encounter(
            &initial,
            &command(&initial, "start-tie", EncounterIntent::StartEncounter),
            &mut rolls,
        )
        .unwrap();
        let initiative = resolution.state.initiative.as_ref().unwrap();

        assert_eq!(
            initiative.order,
            vec![CANAL_WARDEN_ID.to_owned(), SOOT_WIGHT_ID.to_owned()]
        );
        assert_eq!(initiative.ties.len(), 1);
        assert_eq!(initiative.ties[0].total, 12);
        assert_eq!(
            initiative.ties[0].resolved_by,
            InitiativeTieBreaker::HigherModifierThenStableId
        );
        assert_eq!(resolution.rolls[0].expression, "1d20+2");
        assert_eq!(resolution.rolls[1].expression, "1d20+1");
    }

    #[test]
    fn exact_initiative_tie_uses_stable_identifier_order() {
        let mut entries = vec![
            InitiativeEntry {
                participant_id: CANAL_WARDEN_ID.to_owned(),
                natural_roll: 10,
                modifier: 2,
                total: 12,
                tie_break_rank: 0,
            },
            InitiativeEntry {
                participant_id: SOOT_WIGHT_ID.to_owned(),
                natural_roll: 10,
                modifier: 2,
                total: 12,
                tie_break_rank: 0,
            },
        ];
        sort_initiative_entries(&mut entries);

        assert_eq!(entries[0].participant_id, SOOT_WIGHT_ID);
        assert_eq!(
            initiative_ties(&entries)[0].resolved_by,
            InitiativeTieBreaker::StableId
        );
    }

    #[test]
    fn current_actor_and_target_are_derived_and_enforced() {
        let state = started(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            1,
            20,
        );
        assert_eq!(state.current_actor_id.as_deref(), Some(SOOT_WIGHT_ID));

        let wrong_attack = command(
            &state,
            "forged-hero-action",
            EncounterIntent::Attack {
                attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                target_id: CANAL_WARDEN_ID.to_owned(),
            },
        );
        assert!(matches!(
            resolve_encounter(&state, &wrong_attack, &mut PanicRolls),
            Err(EncounterError::AttackUnavailable { actor_id, .. }) if actor_id == SOOT_WIGHT_ID
        ));

        let wrong_target = command(
            &state,
            "forged-target",
            EncounterIntent::Attack {
                attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
                target_id: SOOT_WIGHT_ID.to_owned(),
            },
        );
        assert!(matches!(
            resolve_encounter(&state, &wrong_target, &mut PanicRolls),
            Err(EncounterError::InvalidTarget { actor_id, target_id })
                if actor_id == SOOT_WIGHT_ID && target_id == SOOT_WIGHT_ID
        ));
    }

    #[test]
    fn creature_turn_exposes_no_player_actions_and_rejects_player_control() {
        let state = started(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            1,
            20,
        );

        assert_eq!(state.current_actor_id.as_deref(), Some(SOOT_WIGHT_ID));
        assert!(player_legal_actions(&state).unwrap().is_empty());
        assert!(matches!(
            require_player_control(&state),
            Err(EncounterError::PlayerControlUnavailable { current_actor_id })
                if current_actor_id == SOOT_WIGHT_ID
        ));
        assert!(legal_actions(&state).unwrap().iter().any(|action| matches!(
            action,
            LegalEncounterAction::Move { .. } | LegalEncounterAction::EndTurn
        )));
    }

    #[test]
    fn closed_soot_wight_policy_moves_attacks_then_ends_without_client_choices() {
        let state = started(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            1,
            20,
        );
        assert_eq!(
            select_soot_wight_policy_intent(&state).unwrap(),
            EncounterIntent::Move {
                destination_feet: 5
            }
        );

        let adjacent = moved(&state, 5);
        assert_eq!(
            select_soot_wight_policy_intent(&adjacent).unwrap(),
            EncounterIntent::Attack {
                attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
                target_id: adjacent.hero.id.clone(),
            }
        );

        let attack = command(
            &adjacent,
            "policy-attack",
            select_soot_wight_policy_intent(&adjacent).unwrap(),
        );
        let mut miss = SequenceRolls::new([1]);
        let after_attack = resolve_encounter(&adjacent, &attack, &mut miss)
            .unwrap()
            .state;
        assert_eq!(
            select_soot_wight_policy_intent(&after_attack).unwrap(),
            EncounterIntent::EndTurn
        );
    }

    #[test]
    fn player_boundary_uses_the_authoritative_hero_actor_with_a_created_hero_snapshot() {
        let profile = EncounterHeroProfile {
            source_character_id: "created-hero-42".to_owned(),
            name: "Mara Vale".to_owned(),
            armor_class: 16,
            speed_feet: 30,
            initiative_modifier: 2,
            current_hit_points: 11,
            maximum_hit_points: 11,
            attacks: vec![EncounterAttack {
                attack_id: "attack:created-hero-longsword".to_owned(),
                range_feet: 5,
                attack_modifiers: vec![RollModifierFact {
                    source_id: "modifier:created-hero-strength".to_owned(),
                    value: 5,
                }],
                damage_die_sides: 8,
                damage_modifier: RollModifierFact {
                    source_id: "modifier:created-hero-damage".to_owned(),
                    value: 3,
                },
                damage_type: DamageType::Slashing,
            }],
            rules: None,
        };
        let ready = EncounterState::new_for_hero(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            profile,
        )
        .unwrap();
        let mut rolls = SequenceRolls::new([1, 20]);
        let creature_turn = resolve_encounter(
            &ready,
            &command(
                &ready,
                "created-hero-start",
                EncounterIntent::StartEncounter,
            ),
            &mut rolls,
        )
        .unwrap()
        .state;

        assert_eq!(
            creature_turn.hero.source_character_id.as_deref(),
            Some("created-hero-42")
        );
        assert!(player_legal_actions(&creature_turn).unwrap().is_empty());
        assert_eq!(
            select_soot_wight_policy_intent(&moved(&creature_turn, 5)).unwrap(),
            EncounterIntent::Attack {
                attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
                target_id: creature_turn.hero.id,
            }
        );
    }

    #[test]
    fn movement_and_attack_range_are_enforced_before_rolling() {
        let state = started(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            20,
            1,
        );
        let attack = command(
            &state,
            "out-of-range",
            EncounterIntent::Attack {
                attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                target_id: SOOT_WIGHT_ID.to_owned(),
            },
        );
        assert_eq!(
            resolve_encounter(&state, &attack, &mut PanicRolls),
            Err(EncounterError::TargetOutOfRange {
                distance_feet: 30,
                range_feet: 5,
            })
        );
        assert!(
            !legal_actions(&state)
                .unwrap()
                .iter()
                .any(|action| matches!(action, LegalEncounterAction::Attack { .. }))
        );

        let too_far = command(
            &state,
            "move-too-far",
            EncounterIntent::Move {
                destination_feet: 35,
            },
        );
        assert_eq!(
            resolve_encounter(&state, &too_far, &mut PanicRolls),
            Err(EncounterError::InsufficientMovement {
                requested_feet: 35,
                remaining_feet: 30,
            })
        );

        let adjacent = moved(&state, 25);
        assert_eq!(
            adjacent
                .turn_resources
                .as_ref()
                .unwrap()
                .movement_remaining_feet,
            5
        );
        assert!(
            legal_actions(&adjacent)
                .unwrap()
                .iter()
                .any(|action| matches!(
                    action,
                    LegalEncounterAction::Attack { target_id, .. } if target_id == SOOT_WIGHT_ID
                ))
        );
    }

    #[test]
    fn action_cannot_be_overspent_and_turn_resources_reset() {
        let state = moved(
            &started(
                LethalityPolicy::StoryRecovery,
                OpeningConsequence::RunesUnderstood,
                20,
                1,
            ),
            25,
        );
        let attack = command(
            &state,
            "first-attack",
            EncounterIntent::Attack {
                attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                target_id: SOOT_WIGHT_ID.to_owned(),
            },
        );
        let mut miss = SequenceRolls::new([1]);
        let after_attack = resolve_encounter(&state, &attack, &mut miss).unwrap().state;
        assert!(
            !after_attack
                .turn_resources
                .as_ref()
                .unwrap()
                .action_available
        );

        let second = command(
            &after_attack,
            "second-attack",
            EncounterIntent::Attack {
                attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                target_id: SOOT_WIGHT_ID.to_owned(),
            },
        );
        assert!(matches!(
            resolve_encounter(&after_attack, &second, &mut PanicRolls),
            Err(EncounterError::IllegalIntent { reason })
                if reason.contains("action has already been spent")
        ));

        let ended = resolve_encounter(
            &after_attack,
            &command(&after_attack, "end-hero", EncounterIntent::EndTurn),
            &mut PanicRolls,
        )
        .unwrap()
        .state;
        assert_eq!(ended.current_actor_id.as_deref(), Some(SOOT_WIGHT_ID));
        let resources = ended.turn_resources.as_ref().unwrap();
        assert!(resources.action_available);
        assert!(!resources.bonus_action_available);
        assert!(resources.reaction_available);
        assert!(resources.object_interaction_available);
        assert_eq!(resources.movement_remaining_feet, 25);

        let round_two = resolve_encounter(
            &ended,
            &command(&ended, "end-creature", EncounterIntent::EndTurn),
            &mut PanicRolls,
        )
        .unwrap()
        .state;
        assert_eq!(round_two.round, 2);
        assert_eq!(round_two.current_actor_id.as_deref(), Some(CANAL_WARDEN_ID));
        assert_eq!(
            round_two
                .turn_resources
                .as_ref()
                .unwrap()
                .movement_remaining_feet,
            30
        );
    }

    #[test]
    fn contextual_action_spends_object_interaction_and_removes_soot_veil() {
        let state = moved(
            &started(
                LethalityPolicy::StoryRecovery,
                OpeningConsequence::RunesMisread,
                20,
                1,
            ),
            5,
        );
        let action = command(
            &state,
            "release-sluice",
            EncounterIntent::ContextAction {
                action_id: RELEASE_SLUICE_ACTION_ID.to_owned(),
            },
        );
        let resolution = resolve_encounter(&state, &action, &mut PanicRolls).unwrap();

        assert_eq!(resolution.state.creature.hit_points.temporary, 0);
        assert!(
            !resolution
                .state
                .creature
                .status_effects
                .contains(&CombatantStatusEffect::SootVeil)
        );
        assert_eq!(
            resolution.state.objectives.contextual.status,
            ObjectiveStatus::Completed
        );
        assert!(
            !resolution
                .state
                .turn_resources
                .as_ref()
                .unwrap()
                .object_interaction_available
        );
        assert!(matches!(
            resolution.facts.as_slice(),
            [EncounterFact::ContextActionResolved {
                removed_temporary_hit_points: 4,
                ..
            }]
        ));
    }

    #[test]
    fn natural_one_misses_without_damage_and_natural_twenty_doubles_damage_dice() {
        let adjacent = moved(
            &started(
                LethalityPolicy::StoryRecovery,
                OpeningConsequence::RunesUnderstood,
                20,
                1,
            ),
            25,
        );
        let attack_command = |state: &EncounterState, key: &str| {
            command(
                state,
                key,
                EncounterIntent::Attack {
                    attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            )
        };

        let mut natural_one = SequenceRolls::new([1]);
        let miss = resolve_encounter(
            &adjacent,
            &attack_command(&adjacent, "natural-one"),
            &mut natural_one,
        )
        .unwrap();
        assert_eq!(miss.rolls.len(), 1);
        assert_eq!(miss.rolls[0].outcome, RawRollOutcome::AutomaticMiss);
        assert_eq!(miss.state.creature.hit_points, adjacent.creature.hit_points);

        let mut natural_twenty = SequenceRolls::new([20, 4, 5]);
        let critical = resolve_encounter(
            &adjacent,
            &attack_command(&adjacent, "natural-twenty"),
            &mut natural_twenty,
        )
        .unwrap();
        assert_eq!(critical.rolls.len(), 2);
        assert_eq!(critical.rolls[0].outcome, RawRollOutcome::CriticalHit);
        assert_eq!(critical.rolls[1].expression, "2d8+3");
        assert_eq!(
            critical.rolls[1].individual_dice,
            vec![
                RawDieFact { sides: 8, value: 4 },
                RawDieFact { sides: 8, value: 5 }
            ]
        );
        assert_eq!(critical.rolls[1].total, 12);
        assert_eq!(critical.state.status, EncounterStatus::Victory);
        assert!(matches!(
            critical.state.reward_eligibility,
            RewardEligibility::Eligible {
                tier: EncounterRewardTier::Major
            }
        ));
        assert_eq!(
            critical.narration.fallback_text,
            "The Soot Wight is defeated. The encounter ends in victory."
        );
    }

    #[test]
    fn temporary_hit_points_absorb_before_current_hit_points() {
        let adjacent = moved(
            &started(
                LethalityPolicy::StoryRecovery,
                OpeningConsequence::RunesMisread,
                20,
                1,
            ),
            25,
        );
        let attack = command(
            &adjacent,
            "ordinary-hit",
            EncounterIntent::Attack {
                attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                target_id: SOOT_WIGHT_ID.to_owned(),
            },
        );
        let mut rolls = SequenceRolls::new([7, 2]); // 7 + 5 hits AC 12; 2 + 3 = 5.
        let resolution = resolve_encounter(&adjacent, &attack, &mut rolls).unwrap();

        assert_eq!(resolution.state.creature.hit_points.temporary, 0);
        assert_eq!(resolution.state.creature.hit_points.current, 8);
        assert!(resolution.facts.iter().any(|fact| matches!(
            fact,
            EncounterFact::DamageApplied {
                amount: 5,
                temporary_hit_points_before: 4,
                temporary_hit_points_absorbed: 4,
                current_hit_points_before: 9,
                current_hit_points_after: 8,
                ..
            }
        )));
    }

    #[test]
    fn natural_twenty_death_save_restores_one_hp_without_ending_turn() {
        let state = unconscious_hero_turn();
        assert_eq!(
            legal_actions(&state).unwrap(),
            vec![LegalEncounterAction::RollDeathSave]
        );
        let mut rolls = SequenceRolls::new([20]);
        let resolution = resolve_encounter(
            &state,
            &command(&state, "death-save-20", EncounterIntent::RollDeathSave),
            &mut rolls,
        )
        .unwrap();

        assert_eq!(resolution.state.hero.hit_points.current, 1);
        assert_eq!(resolution.state.hero.life_status, LifeStatus::Conscious);
        assert_eq!(
            resolution.state.hero.hit_points.death_saves,
            DeathSaves::default()
        );
        assert_eq!(
            resolution.state.current_actor_id.as_deref(),
            Some(CANAL_WARDEN_ID)
        );
        assert_eq!(resolution.state.status, EncounterStatus::Active);
        assert_eq!(
            resolution.rolls[0].outcome,
            RawRollOutcome::NaturalTwentyRecovery
        );
    }

    #[test]
    fn three_death_save_successes_stabilize_and_end_raw_encounter() {
        let mut state = unconscious_hero_turn();
        for attempt in 0..3 {
            let mut rolls = SequenceRolls::new([10]);
            state = resolve_encounter(
                &state,
                &command(
                    &state,
                    &format!("death-success-{attempt}"),
                    EncounterIntent::RollDeathSave,
                ),
                &mut rolls,
            )
            .unwrap()
            .state;
            if attempt < 2 {
                assert_eq!(state.current_actor_id.as_deref(), Some(SOOT_WIGHT_ID));
                state = resolve_encounter(
                    &state,
                    &command(
                        &state,
                        &format!("wight-yields-{attempt}"),
                        EncounterIntent::EndTurn,
                    ),
                    &mut PanicRolls,
                )
                .unwrap()
                .state;
            }
        }

        assert_eq!(state.status, EncounterStatus::Defeat);
        assert_eq!(state.hero.life_status, LifeStatus::Stable);
        assert_eq!(state.hero.hit_points.death_saves.successes, 3);
        assert_eq!(
            state.transition.as_ref().unwrap().defeat_reason,
            Some(DefeatReason::HeroStable)
        );
        assert!(!state.transition.as_ref().unwrap().story_recovery_applied);
    }

    #[test]
    fn natural_one_and_later_damage_reach_three_failures_and_death() {
        let state = unconscious_hero_turn();
        let mut death_roll = SequenceRolls::new([1]);
        let after_save = resolve_encounter(
            &state,
            &command(&state, "death-natural-one", EncounterIntent::RollDeathSave),
            &mut death_roll,
        )
        .unwrap()
        .state;
        assert_eq!(after_save.hero.hit_points.death_saves.failures, 2);
        assert_eq!(after_save.current_actor_id.as_deref(), Some(SOOT_WIGHT_ID));

        let adjacent = moved(&after_save, 5);
        let attack = command(
            &adjacent,
            "damage-unconscious-hero",
            EncounterIntent::Attack {
                attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
                target_id: CANAL_WARDEN_ID.to_owned(),
            },
        );
        let mut rolls = SequenceRolls::new([17, 1]);
        let resolution = resolve_encounter(&adjacent, &attack, &mut rolls).unwrap();

        assert_eq!(resolution.state.hero.hit_points.death_saves.failures, 3);
        assert_eq!(resolution.state.hero.life_status, LifeStatus::Dead);
        assert_eq!(resolution.state.status, EncounterStatus::Defeat);
        assert_eq!(
            resolution.state.transition.as_ref().unwrap().defeat_reason,
            Some(DefeatReason::HeroDead)
        );
    }

    #[test]
    fn critical_damage_at_zero_counts_as_two_failures() {
        let mut hero = unconscious_hero_turn().hero;
        let application = apply_damage(&mut hero, 1, true, LethalityPolicy::RulesAsWritten);

        assert_eq!(application.current_before, 0);
        assert_eq!(hero.hit_points.death_saves.failures, 2);
        assert_eq!(hero.life_status, LifeStatus::Unconscious);
    }

    #[test]
    fn story_recovery_defeat_prepares_one_hp_and_never_awards_reward() {
        let mut state = started(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesMisread,
            1,
            20,
        );
        state.hero.hit_points.current = 1;
        state.hero.hit_points.temporary = 0;
        state.validate().unwrap();
        let adjacent = moved(&state, 5);
        let attack = command(
            &adjacent,
            "story-defeat",
            EncounterIntent::Attack {
                attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
                target_id: CANAL_WARDEN_ID.to_owned(),
            },
        );
        let mut rolls = SequenceRolls::new([17, 1]);
        let resolution = resolve_encounter(&adjacent, &attack, &mut rolls).unwrap();
        let transition = resolution.state.transition.as_ref().unwrap();

        assert_eq!(resolution.state.status, EncounterStatus::Defeat);
        assert_eq!(resolution.state.hero.hit_points.current, 0);
        assert_eq!(resolution.state.hero.life_status, LifeStatus::Unconscious);
        assert_eq!(transition.hero_current_hit_points, 1);
        assert_eq!(transition.hero_life_status, LifeStatus::Conscious);
        assert!(transition.story_recovery_applied);
        assert!(matches!(
            resolution.state.reward_eligibility,
            RewardEligibility::Ineligible {
                reason: RewardIneligibilityReason::EncounterDefeat
            }
        ));
    }

    #[test]
    fn repeated_state_commands_and_rolls_produce_identical_resolutions() {
        fn replay() -> Vec<EncounterResolution> {
            let mut source = SequenceRolls::new([15, 5, 20, 8, 8]);
            let mut state = EncounterState::new(
                LethalityPolicy::StoryRecovery,
                OpeningConsequence::RunesUnderstood,
            );
            let intents = [
                EncounterIntent::StartEncounter,
                EncounterIntent::Move {
                    destination_feet: 25,
                },
                EncounterIntent::Attack {
                    attack_id: CANAL_WARDEN_ATTACK_ID.to_owned(),
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ];
            let mut resolutions = Vec::new();
            for (index, intent) in intents.into_iter().enumerate() {
                let resolution = resolve_encounter(
                    &state,
                    &command(&state, &format!("replay-{index}"), intent),
                    &mut source,
                )
                .unwrap();
                state = resolution.state.clone();
                resolutions.push(resolution);
            }
            source.assert_exhausted();
            resolutions
        }

        let first = replay();
        let second = replay();
        assert_eq!(first, second);
        assert_eq!(first.last().unwrap().state.status, EncounterStatus::Victory);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
    }

    #[test]
    fn attack_outcome_invariant_holds_for_every_d20_face() {
        for natural in 1..=20_u16 {
            let mut source = SequenceRolls::new([natural]);
            let mut rolls = Vec::new();
            let roll = perform_roll(
                &mut source,
                &mut rolls,
                EncounterRollPurpose::Attack,
                CANAL_WARDEN_ID,
                Some(SOOT_WIGHT_ID),
                Some(CANAL_WARDEN_ATTACK_ID),
                1,
                20,
                HERO_ATTACK_MODIFIERS.iter().map(modifier_fact).collect(),
                Some(RollComparison {
                    kind: RollComparisonKind::ArmorClass,
                    value: 12,
                }),
            )
            .unwrap();
            roll.validate().unwrap();

            let expected = match natural {
                1 => RawRollOutcome::AutomaticMiss,
                20 => RawRollOutcome::CriticalHit,
                7..=19 => RawRollOutcome::Hit,
                _ => RawRollOutcome::Miss,
            };
            assert_eq!(roll.outcome, expected, "natural roll {natural}");
        }
    }

    #[test]
    fn damage_invariants_hold_across_current_temp_and_damage_ranges() {
        for current in 1..=HERO_MAXIMUM_HIT_POINTS {
            for temporary in 0..=5 {
                for amount in 1..=24 {
                    let mut hero = EncounterState::new(
                        LethalityPolicy::RulesAsWritten,
                        OpeningConsequence::RunesMisread,
                    )
                    .hero;
                    hero.hit_points.current = current;
                    hero.hit_points.temporary = temporary;
                    let application = apply_damage(
                        &mut hero,
                        amount,
                        amount % 2 == 0,
                        LethalityPolicy::RulesAsWritten,
                    );

                    assert!(hero.hit_points.current <= hero.hit_points.maximum);
                    assert!(hero.hit_points.temporary <= temporary);
                    assert_eq!(
                        application.temporary_absorbed + application.temporary_after,
                        temporary
                    );
                    assert_eq!(application.current_after, hero.hit_points.current);
                    assert_eq!(application.life_after, hero.life_status);
                    if hero.hit_points.current > 0 {
                        assert_eq!(hero.life_status, LifeStatus::Conscious);
                    } else {
                        assert_ne!(hero.life_status, LifeStatus::Conscious);
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_roll_returns_no_successor_and_input_state_is_unchanged() {
        let state = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        let before = state.clone();
        let mut rolls = SequenceRolls::new([10, 0]);
        assert_eq!(
            resolve_encounter(
                &state,
                &command(&state, "invalid-roll", EncounterIntent::StartEncounter),
                &mut rolls,
            ),
            Err(EncounterError::InvalidRoll {
                sides: 20,
                value: 0
            })
        );
        assert_eq!(state, before);
    }

    #[test]
    fn correction_is_a_new_validated_revision_and_never_an_in_place_edit() {
        let original = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        let mut corrected = original.clone();
        corrected.revision = 2;
        let event = EncounterCorrectionEvent {
            schema_version: ENCOUNTER_SCHEMA_VERSION,
            correction_id: "correction-1".to_owned(),
            encounter_id: SOOT_WIGHT_ENCOUNTER_ID.to_owned(),
            previous_revision: 1,
            result_revision: 2,
            reason: "Restore the canonical opening consequence after an operator review."
                .to_owned(),
            corrected_state: corrected,
        };
        event.validate().unwrap();
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            serde_json::from_value::<EncounterCorrectionEvent>(value.clone()).unwrap(),
            event
        );

        let mut unknown = value;
        unknown["rewrites_history"] = json!(true);
        assert!(serde_json::from_value::<EncounterCorrectionEvent>(unknown).is_err());

        let mut invalid = event;
        invalid.result_revision = 1;
        assert!(matches!(
            invalid.validate(),
            Err(EncounterError::InvalidCorrection { .. })
        ));
    }

    #[test]
    fn wizard_spells_are_live_server_derived_actions_with_persisted_slots_and_rolls() {
        let state = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        let actions = legal_actions(&state).unwrap();
        assert!(actions.contains(&LegalEncounterAction::CastSpell {
            spell: SpellId::FireBolt,
            target_id: SOOT_WIGHT_ID.to_owned(),
            range_feet: 120,
        }));
        assert!(actions.contains(&LegalEncounterAction::CastSpell {
            spell: SpellId::MagicMissile,
            target_id: SOOT_WIGHT_ID.to_owned(),
            range_feet: 120,
        }));
        assert!(!actions.iter().any(|action| matches!(
            action,
            LegalEncounterAction::CastSpell {
                spell: SpellId::Light | SpellId::MageHand | SpellId::Shield | SpellId::Sleep,
                ..
            }
        )));

        let mut dice = SequenceRolls::new([1, 2, 3]);
        let resolution = resolve_encounter(
            &state,
            &command(
                &state,
                "cast-magic-missile",
                EncounterIntent::CastSpell {
                    spell: SpellId::MagicMissile,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ),
            &mut dice,
        )
        .unwrap();
        assert_eq!(
            resolution
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
        assert_eq!(resolution.rolls.len(), 3);
        assert!(
            resolution
                .rolls
                .iter()
                .all(|roll| roll.purpose == EncounterRollPurpose::Damage)
        );
        assert!(
            resolution
                .facts
                .contains(&EncounterFact::SpellCastResolved {
                    actor_id: CANAL_WARDEN_ID.to_owned(),
                    target_id: SOOT_WIGHT_ID.to_owned(),
                    spell: SpellId::MagicMissile,
                    level_one_spell_slots_before: Some(2),
                    level_one_spell_slots_after: Some(1),
                    damage_applied: 9,
                })
        );
        let encoded = serde_json::to_value(&resolution).unwrap();
        let decoded: EncounterResolution = serde_json::from_value(encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, resolution);
    }

    #[test]
    fn fire_bolt_uses_disadvantage_at_melee_range_and_hidden_spells_fail_before_rolling() {
        let state = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        let adjacent = moved(&state, 25);
        let mut dice = SequenceRolls::new([18, 2]);
        let resolution = resolve_encounter(
            &adjacent,
            &command(
                &adjacent,
                "cast-fire-bolt-adjacent",
                EncounterIntent::CastSpell {
                    spell: SpellId::FireBolt,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ),
            &mut dice,
        )
        .unwrap();
        assert_eq!(resolution.rolls.len(), 1);
        assert_eq!(resolution.rolls[0].mode, EncounterRollMode::Disadvantage);
        assert_eq!(resolution.rolls[0].kept_die_indices, vec![1]);
        assert_eq!(resolution.rolls[0].natural_d20, Some(2));
        assert_eq!(resolution.rolls[0].outcome, RawRollOutcome::Miss);

        let mut colocated = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        colocated.hero.position_feet = colocated.creature.position_feet;
        colocated.validate().unwrap();
        assert!(
            legal_actions(&colocated)
                .unwrap()
                .contains(&LegalEncounterAction::CastSpell {
                    spell: SpellId::FireBolt,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                    range_feet: 120,
                })
        );
        let mut colocated_dice = SequenceRolls::new([2, 1]);
        let colocated_resolution = resolve_encounter(
            &colocated,
            &command(
                &colocated,
                "cast-fire-bolt-colocated",
                EncounterIntent::CastSpell {
                    spell: SpellId::FireBolt,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ),
            &mut colocated_dice,
        )
        .unwrap();
        assert_eq!(
            colocated_resolution.rolls[0].mode,
            EncounterRollMode::Disadvantage
        );

        let fresh = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        assert_eq!(
            resolve_encounter(
                &fresh,
                &command(
                    &fresh,
                    "forged-light",
                    EncounterIntent::CastSpell {
                        spell: SpellId::Light,
                        target_id: SOOT_WIGHT_ID.to_owned(),
                    },
                ),
                &mut PanicRolls,
            ),
            Err(EncounterError::IllegalIntent {
                reason: "that allowlisted spell is not exposed by this encounter",
            })
        );
    }

    #[test]
    fn fighter_second_wind_and_action_surge_spend_exact_live_resources() {
        let mut state = start_created_hero(HeroClass::Fighter, crate::hero::SupportedLevel::Two);
        state.hero.hit_points.current = 7;
        state.validate().unwrap();
        assert!(
            legal_actions(&state)
                .unwrap()
                .contains(&LegalEncounterAction::SecondWind)
        );
        let mut healing_die = SequenceRolls::new([4]);
        let healed = resolve_encounter(
            &state,
            &command(&state, "second-wind", EncounterIntent::SecondWind),
            &mut healing_die,
        )
        .unwrap();
        assert_eq!(healed.state.hero.hit_points.current, 12);
        assert_eq!(healed.rolls[0].purpose, EncounterRollPurpose::Healing);
        assert_eq!(healed.rolls[0].total, 6);
        assert_eq!(
            healed
                .state
                .hero_rules
                .as_ref()
                .unwrap()
                .runtime_resources
                .second_wind
                .as_ref()
                .unwrap()
                .current,
            0
        );
        assert!(
            !legal_actions(&healed.state)
                .unwrap()
                .contains(&LegalEncounterAction::SecondWind)
        );

        let mut miss = SequenceRolls::new([1]);
        let attacked = resolve_encounter(
            &healed.state,
            &command(
                &healed.state,
                "fighter-spends-action",
                EncounterIntent::Attack {
                    attack_id: "srd-5.1-cc:attack:test-crossbow".to_owned(),
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ),
            &mut miss,
        )
        .unwrap()
        .state;
        assert!(
            legal_actions(&attacked)
                .unwrap()
                .contains(&LegalEncounterAction::ActionSurge)
        );
        let surged = resolve_encounter(
            &attacked,
            &command(&attacked, "action-surge", EncounterIntent::ActionSurge),
            &mut PanicRolls,
        )
        .unwrap();
        assert!(
            surged
                .state
                .turn_resources
                .as_ref()
                .unwrap()
                .action_available
        );
        assert_eq!(
            surged
                .state
                .hero_rules
                .as_ref()
                .unwrap()
                .runtime_resources
                .action_surge
                .as_ref()
                .unwrap()
                .current,
            0
        );
        assert!(
            !legal_actions(&surged.state)
                .unwrap()
                .contains(&LegalEncounterAction::ActionSurge)
        );
    }

    #[test]
    fn light_and_mage_hand_use_only_authored_objects_and_persist_durations() {
        let light_state = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        assert!(
            legal_actions(&light_state)
                .unwrap()
                .contains(&LegalEncounterAction::CastLight {
                    object_id: VIADUCT_RUNE_OBJECT_ID.to_owned(),
                })
        );
        let light = resolve_encounter(
            &light_state,
            &command(
                &light_state,
                "cast-light-runes",
                EncounterIntent::CastLight {
                    object_id: VIADUCT_RUNE_OBJECT_ID.to_owned(),
                },
            ),
            &mut PanicRolls,
        )
        .unwrap();
        assert!(light.rolls.is_empty());
        assert_eq!(
            light.state.live_q04.as_ref().unwrap().objects[0].light_remaining_rounds,
            Some(600)
        );

        let mage_state = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        assert!(legal_actions(&mage_state).unwrap().contains(
            &LegalEncounterAction::CastMageHand {
                anchor_object_id: SLUICE_LEVER_OBJECT_ID.to_owned(),
            }
        ));
        let mut mage = resolve_encounter(
            &mage_state,
            &command(
                &mage_state,
                "cast-mage-hand-lever",
                EncounterIntent::CastMageHand {
                    anchor_object_id: SLUICE_LEVER_OBJECT_ID.to_owned(),
                },
            ),
            &mut PanicRolls,
        )
        .unwrap()
        .state;
        assert_eq!(
            mage.live_q04
                .as_ref()
                .unwrap()
                .mage_hand
                .as_ref()
                .unwrap()
                .remaining_rounds,
            10
        );
        mage.turn_resources.as_mut().unwrap().action_available = true;
        let controlled = resolve_encounter(
            &mage,
            &command(
                &mage,
                "mage-hand-controls-lever",
                EncounterIntent::ControlMageHand {
                    object_id: SLUICE_LEVER_OBJECT_ID.to_owned(),
                },
            ),
            &mut PanicRolls,
        )
        .unwrap();
        assert_eq!(
            controlled.state.objectives.contextual.status,
            ObjectiveStatus::Completed
        );
        assert!(controlled.facts.iter().any(|fact| matches!(
            fact,
            EncounterFact::MageHandControlled { object_id, .. }
                if object_id == SLUICE_LEVER_OBJECT_ID
        )));
        assert_eq!(
            resolve_encounter(
                &mage_state,
                &command(
                    &mage_state,
                    "forged-mage-hand-object",
                    EncounterIntent::CastMageHand {
                        anchor_object_id: "attacker-authored-object".to_owned(),
                    },
                ),
                &mut PanicRolls,
            ),
            Err(EncounterError::IllegalIntent {
                reason: "Mage Hand is not a server-derived legal action for that authored anchor",
            })
        );
    }

    #[test]
    fn sleep_uses_canonical_five_d8_stable_order_and_wakes_on_damage() {
        let state = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        let mut sleep_dice = SequenceRolls::new([2, 2, 2, 2, 2]);
        let mut sleeping = resolve_encounter(
            &state,
            &command(&state, "cast-sleep", EncounterIntent::CastSleep),
            &mut sleep_dice,
        )
        .unwrap();
        assert_eq!(sleeping.rolls.len(), 1);
        assert_eq!(
            sleeping.rolls[0].purpose,
            EncounterRollPurpose::SleepHitPoints
        );
        assert_eq!(sleeping.rolls[0].expression, "5d8");
        assert!(sleeping.facts.contains(&EncounterFact::SleepResolved {
            actor_id: CANAL_WARDEN_ID.to_owned(),
            hit_point_pool: 10,
            ordered_target_ids: vec![SOOT_WIGHT_ID.to_owned()],
            affected_target_ids: vec![SOOT_WIGHT_ID.to_owned()],
            duration_rounds: 10,
        }));
        sleeping
            .state
            .turn_resources
            .as_mut()
            .unwrap()
            .action_available = true;
        let mut attack_dice = SequenceRolls::new([10, 1]);
        let woke = resolve_encounter(
            &sleeping.state,
            &command(
                &sleeping.state,
                "wake-with-damage",
                EncounterIntent::Attack {
                    attack_id: "srd-5.1-cc:attack:test-crossbow".to_owned(),
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ),
            &mut attack_dice,
        )
        .unwrap();
        assert!(woke.state.live_q04.as_ref().unwrap().sleep.is_none());
        assert!(woke.facts.contains(&EncounterFact::SleepEnded {
            target_id: SOOT_WIGHT_ID.to_owned(),
            reason: SleepEndReason::Damaged,
        }));
    }

    #[test]
    fn shield_is_only_a_pending_real_hit_reaction_and_splits_damage_cursor() {
        let ready = created_hero_ready(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        let mut initiative = SequenceRolls::new([1, 20]);
        let mut creature_turn = resolve_encounter(
            &ready,
            &command(&ready, "shield-test-start", EncounterIntent::StartEncounter),
            &mut initiative,
        )
        .unwrap()
        .state;
        creature_turn.creature.position_feet = 5;
        let mut attack_die = SequenceRolls::new([14]);
        let pending = resolve_encounter(
            &creature_turn,
            &command(
                &creature_turn,
                "real-pending-hit",
                EncounterIntent::Attack {
                    attack_id: SOOT_WIGHT_ATTACK_ID.to_owned(),
                    target_id: CANAL_WARDEN_ID.to_owned(),
                },
            ),
            &mut attack_die,
        )
        .unwrap();
        assert_eq!(pending.rolls.len(), 1);
        assert_eq!(pending.rolls[0].purpose, EncounterRollPurpose::Attack);
        assert_eq!(pending.state.hero.hit_points.current, 12);
        assert_eq!(
            player_legal_actions(&pending.state).unwrap(),
            vec![
                LegalEncounterAction::CastShield,
                LegalEncounterAction::DeclineReaction,
            ]
        );
        let shielded = resolve_encounter(
            &pending.state,
            &command(
                &pending.state,
                "cast-pending-shield",
                EncounterIntent::CastShield,
            ),
            &mut PanicRolls,
        )
        .unwrap();
        assert!(shielded.rolls.is_empty());
        assert_eq!(shielded.state.hero.hit_points.current, 12);
        assert!(
            shielded
                .state
                .live_q04
                .as_ref()
                .unwrap()
                .shield_ward
                .is_some()
        );

        let fresh = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        assert_eq!(
            resolve_encounter(
                &fresh,
                &command(&fresh, "free-shield-rejected", EncounterIntent::CastShield),
                &mut PanicRolls,
            ),
            Err(EncounterError::IllegalIntent {
                reason: "Shield is not available for a real pending hit",
            })
        );
    }

    #[test]
    fn rests_confirm_each_hit_die_use_trusted_campaign_time_and_enforce_long_rest_day() {
        let mut state = start_created_hero(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        state.hero.hit_points.current = 5;
        state.validate().unwrap();
        let mut missile_dice = SequenceRolls::new([4, 4, 4]);
        let victory = resolve_encounter(
            &state,
            &command(
                &state,
                "rest-boundary-victory",
                EncounterIntent::CastSpell {
                    spell: SpellId::MagicMissile,
                    target_id: SOOT_WIGHT_ID.to_owned(),
                },
            ),
            &mut missile_dice,
        )
        .unwrap();
        assert_eq!(victory.state.status, EncounterStatus::Victory);
        let begun = resolve_encounter(
            &victory.state,
            &command(
                &victory.state,
                "begin-short-rest",
                EncounterIntent::BeginShortRest,
            ),
            &mut PanicRolls,
        )
        .unwrap();
        assert!(begun.rolls.is_empty());
        assert_eq!(begun.state.hero.hit_points.current, 5);
        assert_eq!(
            begun.state.live_q04.as_ref().unwrap().campaign_time_minutes,
            60
        );
        let mut hit_die = SequenceRolls::new([3]);
        let spent = resolve_encounter(
            &begun.state,
            &command(
                &begun.state,
                "confirm-one-hit-die",
                EncounterIntent::SpendHitDie,
            ),
            &mut hit_die,
        )
        .unwrap();
        assert_eq!(spent.rolls.len(), 1);
        assert_eq!(spent.rolls[0].purpose, EncounterRollPurpose::HitDie);
        assert_eq!(spent.state.hero.hit_points.current, 10);
        let recovered = resolve_encounter(
            &spent.state,
            &command(
                &spent.state,
                "arcane-recovery",
                EncounterIntent::UseArcaneRecovery,
            ),
            &mut PanicRolls,
        )
        .unwrap();
        let finished = resolve_encounter(
            &recovered.state,
            &command(
                &recovered.state,
                "finish-short-rest",
                EncounterIntent::FinishShortRest,
            ),
            &mut PanicRolls,
        )
        .unwrap();
        let long = resolve_encounter(
            &finished.state,
            &command(
                &finished.state,
                "take-long-rest",
                EncounterIntent::TakeLongRest,
            ),
            &mut PanicRolls,
        )
        .unwrap();
        assert_eq!(long.state.hero.hit_points.current, 12);
        assert_eq!(
            long.state.live_q04.as_ref().unwrap().campaign_time_minutes,
            540
        );
        assert!(
            !legal_actions(&long.state)
                .unwrap()
                .contains(&LegalEncounterAction::TakeLongRest)
        );
    }

    #[test]
    fn schema_v2_fixture_replays_exactly_but_is_publicly_read_only() {
        let mut v2 = created_hero_ready(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        v2.hero_rules.as_mut().unwrap().constitution_modifier = None;
        v2.pin_historical_schema_for_replay(LIVE_V2_ENCOUNTER_SCHEMA_VERSION)
            .unwrap();
        let encoded = serde_json::to_value(&v2).unwrap();
        assert!(encoded.get("live_q04").is_none());
        let decoded: EncounterState = serde_json::from_value(encoded).unwrap();
        assert!(player_legal_actions(&decoded).unwrap().is_empty());
        let command: EncounterCommand = serde_json::from_value(json!({
            "schema_version": LIVE_V2_ENCOUNTER_SCHEMA_VERSION,
            "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
            "expected_revision": 1,
            "idempotency_key": "v2-start",
            "intent": { "type": "start_encounter" }
        }))
        .unwrap();
        let mut dice = SequenceRolls::new([20, 1]);
        let replayed = resolve_encounter(&decoded, &command, &mut dice).unwrap();
        assert_eq!(replayed.schema_version, LIVE_V2_ENCOUNTER_SCHEMA_VERSION);
        assert_eq!(
            serde_json::from_value::<EncounterResolution>(serde_json::to_value(&replayed).unwrap())
                .unwrap(),
            replayed
        );
        assert!(
            serde_json::from_value::<EncounterCommand>(json!({
                "schema_version": LIVE_V2_ENCOUNTER_SCHEMA_VERSION,
                "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
                "expected_revision": 1,
                "idempotency_key": "v2-forged-sleep",
                "intent": { "type": "cast_sleep" }
            }))
            .is_err()
        );
    }

    #[test]
    fn schema_v1_fixture_replays_exactly_and_rejects_v2_semantics() {
        let mut legacy_state = EncounterState::new(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
        );
        legacy_state
            .pin_historical_schema_for_replay(LEGACY_ENCOUNTER_SCHEMA_VERSION)
            .unwrap();
        let mut fixture = serde_json::to_value(&legacy_state).unwrap();
        fixture.as_object_mut().unwrap().remove("hero_rules");
        let decoded: EncounterState = serde_json::from_value(fixture).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.schema_version, LEGACY_ENCOUNTER_SCHEMA_VERSION);
        assert!(decoded.hero_rules.is_none());
        assert!(player_legal_actions(&decoded).unwrap().is_empty());
        assert_eq!(
            resolve_encounter(
                &decoded,
                &EncounterCommand::new(
                    decoded.revision,
                    "implicit-upgrade-rejected",
                    EncounterIntent::StartEncounter,
                ),
                &mut PanicRolls,
            ),
            Err(EncounterError::InvalidCommand {
                reason: "command and encounter state schema versions must match",
            })
        );

        let command: EncounterCommand = serde_json::from_value(json!({
            "schema_version": LEGACY_ENCOUNTER_SCHEMA_VERSION,
            "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
            "expected_revision": 1,
            "idempotency_key": "legacy-start",
            "intent": { "type": "start_encounter" }
        }))
        .unwrap();
        let mut dice = SequenceRolls::new([20, 1]);
        let resolution = resolve_encounter(&decoded, &command, &mut dice).unwrap();
        assert_eq!(resolution.schema_version, LEGACY_ENCOUNTER_SCHEMA_VERSION);
        assert_eq!(
            resolution.state.schema_version,
            LEGACY_ENCOUNTER_SCHEMA_VERSION
        );
        let round_trip: EncounterResolution =
            serde_json::from_value(serde_json::to_value(&resolution).unwrap()).unwrap();
        assert_eq!(round_trip, resolution);

        let legacy_cast = json!({
            "schema_version": LEGACY_ENCOUNTER_SCHEMA_VERSION,
            "encounter_id": SOOT_WIGHT_ENCOUNTER_ID,
            "expected_revision": 1,
            "idempotency_key": "legacy-forged-cast",
            "intent": {
                "type": "cast_spell",
                "spell": "fire_bolt",
                "target_id": SOOT_WIGHT_ID
            }
        });
        assert!(serde_json::from_value::<EncounterCommand>(legacy_cast).is_err());

        let mut invalid_legacy_state =
            created_hero_ready(HeroClass::Wizard, crate::hero::SupportedLevel::One);
        invalid_legacy_state.schema_version = LEGACY_ENCOUNTER_SCHEMA_VERSION;
        invalid_legacy_state.content_pack_id = LEGACY_ENCOUNTER_CONTENT_PACK_ID.to_owned();
        invalid_legacy_state.live_q04 = None;
        assert!(matches!(
            invalid_legacy_state.validate(),
            Err(EncounterError::InvalidState {
                reason: "legacy encounter state cannot contain a Slice 2 hero rules snapshot"
            })
        ));
    }

    #[test]
    fn canonical_state_round_trips_and_rejects_unknown_fields() {
        let state = started(
            LethalityPolicy::StoryRecovery,
            OpeningConsequence::RunesUnderstood,
            20,
            1,
        );
        let value = serde_json::to_value(&state).unwrap();
        let decoded: EncounterState = serde_json::from_value(value.clone()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, state);

        let mut legacy = value.clone();
        for participant in ["hero", "creature"] {
            legacy[participant]
                .as_object_mut()
                .unwrap()
                .remove("source_character_id");
            legacy[participant]
                .as_object_mut()
                .unwrap()
                .remove("attacks");
        }
        let legacy_decoded: EncounterState = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy_decoded, state);

        let mut future = value;
        future["future_mechanic"] = json!(true);
        assert!(serde_json::from_value::<EncounterState>(future).is_err());
    }
}
