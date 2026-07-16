//! Deterministic runtime rules for the exact level 1-2 MVP content matrix.
//!
//! This module deliberately owns no persistence, clock, randomness, content
//! loading, or narration. Callers validate campaign authority and inject dice;
//! the functions below validate mechanical inputs and return typed facts/state.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Ability, AbilityScores, ActionEconomy, CheckDifficulty, D20Roll, DiceSource, GameCoreError,
    Level, Proficiency, RollContext, TurnResource,
    encounter::LethalityPolicy,
    hero::{
        ActionCapability, AttackSummary, AuthoredAlternative, ConditionId, DamageInteraction,
        DamageType, DerivedHeroSheet, EquipmentId, EquipmentState, FeatureAvailability, FeatureId,
        HERO_UNSUPPORTED_SCHEMA_VERSION, HeroClass, ResourceKind, SimpleWeaponId, SkillId, SpellId,
        SupportedLevel, UnsupportedMechanic, UnsupportedMechanicCode, WeaponChoice,
    },
    is_valid_opaque_id, resolve_d20,
};

pub const RULES_MATRIX_SCHEMA_VERSION: u16 = 1;
pub const RULES_MATRIX_PROFILE_ID: &str = "manchester-arcana:rules-matrix:v1";
pub const AUTHORED_CAPACITY_POLICY_ID: &str =
    "manchester-arcana:capacity:authored-starting-loadout-v1";
pub const MAX_SITUATIONAL_MODIFIERS: usize = 16;
pub const MAX_ACTIVE_CONDITIONS: usize = 32;
pub const MAX_CLOCK_SEGMENTS: u8 = 12;
pub const MAX_CURRENCY_PIECES: u32 = 1_000_000;
pub const MAX_MVP_DASHES_PER_TURN: u16 = 2;

pub type RulesMatrixResult<T> = std::result::Result<T, RulesMatrixError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RulesMatrixError {
    #[error("invalid rules state: {reason}")]
    InvalidState { reason: &'static str },
    #[error("invalid rules intent: {reason}")]
    InvalidIntent { reason: &'static str },
    #[error("rules arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("unsupported mechanic: {0:?}")]
    Unsupported(UnsupportedMechanic),
    #[error(transparent)]
    Core(#[from] GameCoreError),
}

/// Produces the same structured failure for UI, authored actions, and free-form
/// intent translation. Unknown mechanics never fall through to narration.
pub fn unsupported_mechanic(requested_id: &str) -> RulesMatrixError {
    let requested_id = if is_valid_opaque_id(requested_id) {
        requested_id.to_owned()
    } else {
        "invalid-requested-id".to_owned()
    };
    RulesMatrixError::Unsupported(UnsupportedMechanic {
        schema_version: HERO_UNSUPPORTED_SCHEMA_VERSION,
        code: UnsupportedMechanicCode::OutsideMvpMatrix,
        requested_id,
        alternatives: vec![
            AuthoredAlternative {
                action: ActionCapability::Attack,
                label: "Make a supported weapon attack".to_owned(),
            },
            AuthoredAlternative {
                action: ActionCapability::Search,
                label: "Search the current scene".to_owned(),
            },
            AuthoredAlternative {
                action: ActionCapability::UseObject,
                label: "Use a supported carried object".to_owned(),
            },
        ],
    })
}

fn invalid_state(reason: &'static str) -> RulesMatrixError {
    RulesMatrixError::InvalidState { reason }
}

fn invalid_intent(reason: &'static str) -> RulesMatrixError {
    RulesMatrixError::InvalidIntent { reason }
}

fn require_id(value: &str, reason: &'static str) -> RulesMatrixResult<()> {
    if is_valid_opaque_id(value) {
        Ok(())
    } else {
        Err(invalid_intent(reason))
    }
}

fn require_supported_level(level: Level) -> RulesMatrixResult<()> {
    if matches!(level.value(), 1 | 2) {
        Ok(())
    } else {
        Err(unsupported_mechanic("level.outside-mvp-1-2"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cover {
    None,
    Half,
    ThreeQuarters,
}

impl Cover {
    pub const fn armor_class_bonus(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Half => 2,
            Self::ThreeQuarters => 5,
        }
    }

    pub const fn dexterity_save_bonus(self) -> i8 {
        self.armor_class_bonus() as i8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SituationalModifier {
    pub source_id: String,
    pub value: i8,
}

impl SituationalModifier {
    fn validate_all(values: &[Self]) -> RulesMatrixResult<()> {
        if values.len() > MAX_SITUATIONAL_MODIFIERS {
            return Err(invalid_intent("too many situational modifiers"));
        }
        let mut ids = BTreeSet::new();
        for value in values {
            require_id(&value.source_id, "situational modifier source is invalid")?;
            if !(-30..=30).contains(&value.value) {
                return Err(invalid_intent(
                    "situational modifier is outside -30 through 30",
                ));
            }
            if !ids.insert(&value.source_id) {
                return Err(invalid_intent(
                    "situational modifier sources must be unique",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum D20Target {
    AbilityCheck { difficulty_class: u8 },
    SavingThrow { difficulty_class: u8, cover: Cover },
    Attack { armor_class: u8, cover: Cover },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct D20TestRequest {
    pub schema_version: u16,
    pub ability: Ability,
    pub proficiency: Proficiency,
    pub roll_context: RollContext,
    #[serde(default)]
    pub situational_modifiers: Vec<SituationalModifier>,
    pub target: D20Target,
}

impl D20TestRequest {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION {
            return Err(invalid_intent("d20 test schema version is unsupported"));
        }
        SituationalModifier::validate_all(&self.situational_modifiers)?;
        let target = match self.target {
            D20Target::AbilityCheck { difficulty_class }
            | D20Target::SavingThrow {
                difficulty_class, ..
            } => difficulty_class,
            D20Target::Attack { armor_class, .. } => armor_class,
        };
        if target == 0 || target > 60 {
            return Err(invalid_intent("DC or AC must be between 1 and 60"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum D20TestOutcome {
    Success,
    Failure,
    CriticalHit,
    AutomaticMiss,
}

impl D20TestOutcome {
    pub const fn succeeds(self) -> bool {
        matches!(self, Self::Success | Self::CriticalHit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct D20TestResolution {
    pub schema_version: u16,
    pub roll: D20Roll,
    pub ability: Ability,
    pub ability_modifier: i8,
    pub proficiency_modifier: u8,
    pub situational_modifiers: Vec<SituationalModifier>,
    pub situational_total: i16,
    pub total: i16,
    pub target_number: u8,
    pub cover_bonus: u8,
    pub outcome: D20TestOutcome,
}

/// Resolves checks, saves, and attacks through one auditable d20 path. Natural
/// 1/20 special handling applies only to attacks.
pub fn resolve_d20_test(
    ability_scores: &AbilityScores,
    level: Level,
    request: &D20TestRequest,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<D20TestResolution> {
    request.validate()?;
    ability_scores.validate()?;
    require_supported_level(level)?;

    let roll = resolve_d20(dice, request.roll_context)?;
    let ability_modifier = ability_scores.get(request.ability).modifier();
    let proficiency_modifier = request.proficiency.bonus(level.proficiency_bonus());
    let situational_total =
        request
            .situational_modifiers
            .iter()
            .try_fold(0_i16, |sum, modifier| {
                sum.checked_add(i16::from(modifier.value))
                    .ok_or(RulesMatrixError::ArithmeticOverflow)
            })?;
    let (base_target, cover_bonus) = match request.target {
        D20Target::AbilityCheck { difficulty_class } => (difficulty_class, 0),
        D20Target::SavingThrow {
            difficulty_class,
            cover,
        } => (
            difficulty_class,
            if request.ability == Ability::Dexterity {
                cover.dexterity_save_bonus() as u8
            } else {
                0
            },
        ),
        D20Target::Attack { armor_class, cover } => (armor_class, cover.armor_class_bonus()),
    };
    let target_number = base_target
        .checked_add(cover_bonus)
        .ok_or(RulesMatrixError::ArithmeticOverflow)?;
    let total = i16::from(roll.selected)
        .checked_add(i16::from(ability_modifier))
        .and_then(|value| value.checked_add(i16::from(proficiency_modifier)))
        .and_then(|value| value.checked_add(situational_total))
        .ok_or(RulesMatrixError::ArithmeticOverflow)?;
    let outcome = match request.target {
        D20Target::Attack { .. } if roll.selected == 20 => D20TestOutcome::CriticalHit,
        D20Target::Attack { .. } if roll.selected == 1 => D20TestOutcome::AutomaticMiss,
        _ if total >= i16::from(target_number) => D20TestOutcome::Success,
        _ => D20TestOutcome::Failure,
    };

    Ok(D20TestResolution {
        schema_version: RULES_MATRIX_SCHEMA_VERSION,
        roll,
        ability: request.ability,
        ability_modifier,
        proficiency_modifier,
        situational_modifiers: request.situational_modifiers.clone(),
        situational_total,
        total,
        target_number,
        cover_bonus,
        outcome,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConditionedD20Resolution {
    Rolled {
        result: D20TestResolution,
    },
    AutomaticSaveFailure {
        ability: Ability,
        difficulty_class: u8,
    },
}

/// Applies actor-side condition rules before a check, save, or attack roll.
/// Automatic Strength/Dexterity save failures consume no die.
pub fn resolve_conditioned_d20_test(
    ability_scores: &AbilityScores,
    level: Level,
    request: &D20TestRequest,
    conditions: &ConditionSet,
    situation: RollSituation,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<ConditionedD20Resolution> {
    let situation_matches = match (request.target, situation) {
        (D20Target::AbilityCheck { .. }, RollSituation::AbilityCheck)
        | (D20Target::Attack { .. }, RollSituation::OwnAttack) => true,
        (D20Target::SavingThrow { .. }, RollSituation::SavingThrow { ability }) => {
            ability == request.ability
        }
        _ => false,
    };
    if !situation_matches {
        return Err(invalid_intent(
            "d20 target and condition situation do not match",
        ));
    }
    let effects = condition_effects(conditions, situation)?;
    if effects.actions_blocked && matches!(request.target, D20Target::Attack { .. }) {
        return Err(invalid_intent(
            "current conditions prevent the attack action",
        ));
    }
    if effects.automatic_save_failure {
        let D20Target::SavingThrow {
            difficulty_class, ..
        } = request.target
        else {
            return Err(invalid_state(
                "automatic failure appeared outside a saving throw",
            ));
        };
        return Ok(ConditionedD20Resolution::AutomaticSaveFailure {
            ability: request.ability,
            difficulty_class,
        });
    }
    let mut conditioned = request.clone();
    conditioned.roll_context = combine_roll_context(request.roll_context, effects.roll_context);
    Ok(ConditionedD20Resolution::Rolled {
        result: resolve_d20_test(ability_scores, level, &conditioned, dice)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackMode {
    Melee,
    Ranged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeBand {
    MeleeReach,
    Normal,
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeResolution {
    pub band: RangeBand,
    pub disadvantage_sources: u8,
}

fn weapon_ranges(weapon: &WeaponChoice, mode: AttackMode) -> Option<(u16, Option<u16>)> {
    match (weapon, mode) {
        (WeaponChoice::Longsword, AttackMode::Melee)
        | (WeaponChoice::Simple { .. }, AttackMode::Melee) => Some((5, None)),
        (WeaponChoice::LightCrossbow, AttackMode::Ranged) => Some((80, Some(320))),
        (WeaponChoice::Simple { kind }, AttackMode::Ranged) => match kind {
            SimpleWeaponId::Dagger | SimpleWeaponId::Handaxe | SimpleWeaponId::LightHammer => {
                Some((20, Some(60)))
            }
            SimpleWeaponId::Javelin => Some((30, Some(120))),
            SimpleWeaponId::Spear => Some((20, Some(60))),
            SimpleWeaponId::Club
            | SimpleWeaponId::Greatclub
            | SimpleWeaponId::Mace
            | SimpleWeaponId::Quarterstaff
            | SimpleWeaponId::Sickle => None,
        },
        (WeaponChoice::Longsword | WeaponChoice::LightCrossbow, _) => None,
    }
}

pub fn resolve_weapon_range(
    weapon: &WeaponChoice,
    mode: AttackMode,
    distance_feet: u16,
    threatening_hostile_within_five_feet: bool,
) -> RulesMatrixResult<RangeResolution> {
    if distance_feet == 0 || !distance_feet.is_multiple_of(5) {
        return Err(invalid_intent(
            "attack distance must be a positive five-foot increment",
        ));
    }
    let Some((normal, long)) = weapon_ranges(weapon, mode) else {
        return Err(unsupported_mechanic("attack.weapon-mode"));
    };
    let (band, mut disadvantage_sources) = if distance_feet <= normal {
        (
            if mode == AttackMode::Melee {
                RangeBand::MeleeReach
            } else {
                RangeBand::Normal
            },
            0_u8,
        )
    } else if long.is_some_and(|long| distance_feet <= long) {
        (RangeBand::Long, 1)
    } else {
        return Err(invalid_intent(
            "target is outside the supported weapon range",
        ));
    };
    if mode == AttackMode::Ranged && threatening_hostile_within_five_feet {
        disadvantage_sources = disadvantage_sources.saturating_add(1);
    }
    Ok(RangeResolution {
        band,
        disadvantage_sources,
    })
}

pub fn combine_roll_context(base: RollContext, other: RollContext) -> RollContext {
    RollContext {
        advantage_sources: base
            .advantage_sources
            .saturating_add(other.advantage_sources),
        disadvantage_sources: base
            .disadvantage_sources
            .saturating_add(other.disadvantage_sources),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnBoundary {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectDuration {
    Permanent,
    Rounds {
        remaining: u16,
        boundary: TurnBoundary,
        actor_id: String,
    },
    UntilDamagedOrAwakened {
        remaining_rounds: u16,
        actor_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConditionSource {
    Spell { spell: SpellId, caster_id: String },
    Mechanic { mechanic_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveCondition {
    pub condition: ConditionId,
    pub source: ConditionSource,
    pub duration: EffectDuration,
}

impl ActiveCondition {
    fn validate(&self) -> RulesMatrixResult<()> {
        match &self.source {
            ConditionSource::Spell { caster_id, .. } => {
                require_id(caster_id, "condition caster ID is invalid")?;
            }
            ConditionSource::Mechanic { mechanic_id } => {
                require_id(mechanic_id, "condition mechanic ID is invalid")?;
            }
        }
        match &self.duration {
            EffectDuration::Permanent => {}
            EffectDuration::Rounds {
                remaining,
                actor_id,
                ..
            } => {
                if *remaining == 0 || *remaining > 600 {
                    return Err(invalid_state(
                        "condition duration is outside one round to one hour",
                    ));
                }
                require_id(actor_id, "condition duration actor ID is invalid")?;
            }
            EffectDuration::UntilDamagedOrAwakened {
                remaining_rounds,
                actor_id,
            } => {
                if *remaining_rounds == 0 || *remaining_rounds > 10 {
                    return Err(invalid_state("sleep duration is outside one to ten rounds"));
                }
                require_id(actor_id, "sleep duration actor ID is invalid")?;
                if self.condition != ConditionId::Unconscious
                    || !matches!(
                        self.source,
                        ConditionSource::Spell {
                            spell: SpellId::Sleep,
                            ..
                        }
                    )
                {
                    return Err(invalid_state(
                        "damage-or-awakening duration is reserved for sleep",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionSet {
    pub schema_version: u16,
    pub active: Vec<ActiveCondition>,
}

impl ConditionSet {
    pub fn empty() -> Self {
        Self {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            active: Vec::new(),
        }
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION
            || self.active.len() > MAX_ACTIVE_CONDITIONS
        {
            return Err(invalid_state("condition set schema or size is invalid"));
        }
        let mut identities = BTreeSet::new();
        for condition in &self.active {
            condition.validate()?;
            let source = match &condition.source {
                ConditionSource::Spell { spell, caster_id } => {
                    format!("spell:{spell:?}:{caster_id}")
                }
                ConditionSource::Mechanic { mechanic_id } => format!("mechanic:{mechanic_id}"),
            };
            if !identities.insert((condition.condition, source)) {
                return Err(invalid_state("duplicate condition source"));
            }
        }
        Ok(())
    }

    pub fn contains(&self, condition: ConditionId) -> bool {
        self.active
            .iter()
            .any(|active| active.condition == condition)
    }

    pub fn apply(&mut self, condition: ActiveCondition) -> RulesMatrixResult<()> {
        self.validate()?;
        condition.validate()?;
        if self.active.len() == MAX_ACTIVE_CONDITIONS {
            return Err(invalid_state("condition set is full"));
        }
        let duplicate = self.active.iter().any(|active| {
            active.condition == condition.condition && active.source == condition.source
        });
        if duplicate {
            return Err(invalid_intent(
                "the same condition source is already active",
            ));
        }
        self.active.push(condition);
        self.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurationEvent {
    TurnBoundary {
        actor_id: String,
        boundary: TurnBoundary,
    },
    Damaged {
        actor_id: String,
    },
    AwakenedByAction {
        actor_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurationResolution {
    pub expired: Vec<ActiveCondition>,
}

pub fn process_durations(
    conditions: &mut ConditionSet,
    event: &DurationEvent,
) -> RulesMatrixResult<DurationResolution> {
    conditions.validate()?;
    let event_actor = match event {
        DurationEvent::TurnBoundary { actor_id, .. }
        | DurationEvent::Damaged { actor_id }
        | DurationEvent::AwakenedByAction { actor_id } => actor_id,
    };
    require_id(event_actor, "duration event actor ID is invalid")?;

    let mut expired = Vec::new();
    let mut retained = Vec::with_capacity(conditions.active.len());
    for mut condition in conditions.active.drain(..) {
        let remove = match (&mut condition.duration, event) {
            (
                EffectDuration::Rounds {
                    remaining,
                    boundary,
                    actor_id,
                },
                DurationEvent::TurnBoundary {
                    actor_id: event_actor,
                    boundary: event_boundary,
                },
            ) if actor_id == event_actor && boundary == event_boundary => {
                *remaining -= 1;
                *remaining == 0
            }
            (
                EffectDuration::UntilDamagedOrAwakened { actor_id, .. },
                DurationEvent::Damaged {
                    actor_id: event_actor,
                }
                | DurationEvent::AwakenedByAction {
                    actor_id: event_actor,
                },
            ) if actor_id == event_actor => true,
            (
                EffectDuration::UntilDamagedOrAwakened {
                    remaining_rounds,
                    actor_id,
                },
                DurationEvent::TurnBoundary {
                    actor_id: event_actor,
                    boundary: TurnBoundary::End,
                },
            ) if actor_id == event_actor => {
                *remaining_rounds -= 1;
                *remaining_rounds == 0
            }
            _ => false,
        };
        if remove {
            expired.push(condition);
        } else {
            retained.push(condition);
        }
    }
    conditions.active = retained;
    conditions.validate()?;
    Ok(DurationResolution { expired })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RollSituation {
    AbilityCheck,
    SavingThrow { ability: Ability },
    OwnAttack,
    IncomingAttack { attacker_distance_feet: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionEffects {
    pub roll_context: RollContext,
    pub automatic_save_failure: bool,
    pub speed_is_zero: bool,
    pub actions_blocked: bool,
    pub reactions_blocked: bool,
    pub incoming_hit_is_critical: bool,
}

pub fn condition_effects(
    conditions: &ConditionSet,
    situation: RollSituation,
) -> RulesMatrixResult<ConditionEffects> {
    conditions.validate()?;
    let mut effects = ConditionEffects {
        roll_context: RollContext::normal(),
        automatic_save_failure: false,
        speed_is_zero: false,
        actions_blocked: false,
        reactions_blocked: false,
        incoming_hit_is_critical: false,
    };
    for condition in &conditions.active {
        match condition.condition {
            ConditionId::Prone => match situation {
                RollSituation::OwnAttack => {
                    effects.roll_context.disadvantage_sources =
                        effects.roll_context.disadvantage_sources.saturating_add(1);
                }
                RollSituation::IncomingAttack {
                    attacker_distance_feet,
                } if attacker_distance_feet <= 5 => {
                    effects.roll_context.advantage_sources =
                        effects.roll_context.advantage_sources.saturating_add(1);
                }
                RollSituation::IncomingAttack { .. } => {
                    effects.roll_context.disadvantage_sources =
                        effects.roll_context.disadvantage_sources.saturating_add(1);
                }
                _ => {}
            },
            ConditionId::Restrained => match situation {
                RollSituation::OwnAttack => {
                    effects.roll_context.disadvantage_sources =
                        effects.roll_context.disadvantage_sources.saturating_add(1);
                }
                RollSituation::IncomingAttack { .. } => {
                    effects.roll_context.advantage_sources =
                        effects.roll_context.advantage_sources.saturating_add(1);
                }
                RollSituation::SavingThrow {
                    ability: Ability::Dexterity,
                } => {
                    effects.roll_context.disadvantage_sources =
                        effects.roll_context.disadvantage_sources.saturating_add(1);
                }
                _ => {}
            },
            ConditionId::Grappled => effects.speed_is_zero = true,
            ConditionId::Incapacitated => {
                effects.actions_blocked = true;
                effects.reactions_blocked = true;
            }
            ConditionId::Unconscious => {
                effects.actions_blocked = true;
                effects.reactions_blocked = true;
                effects.speed_is_zero = true;
                match situation {
                    RollSituation::SavingThrow {
                        ability: Ability::Strength | Ability::Dexterity,
                    } => effects.automatic_save_failure = true,
                    RollSituation::IncomingAttack {
                        attacker_distance_feet,
                    } => {
                        effects.roll_context.advantage_sources =
                            effects.roll_context.advantage_sources.saturating_add(1);
                        if attacker_distance_feet <= 5 {
                            effects.incoming_hit_is_critical = true;
                        }
                    }
                    _ => {}
                }
            }
            ConditionId::Poisoned => match situation {
                RollSituation::AbilityCheck | RollSituation::OwnAttack => {
                    effects.roll_context.disadvantage_sources =
                        effects.roll_context.disadvantage_sources.saturating_add(1);
                }
                _ => {}
            },
        }
    }
    if conditions.contains(ConditionId::Restrained) {
        effects.speed_is_zero = true;
    }
    Ok(effects)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovementState {
    pub base_speed_feet: u16,
    pub remaining_feet: u16,
    pub position_feet: u16,
}

impl MovementState {
    pub fn new(speed_feet: u16, position_feet: u16) -> RulesMatrixResult<Self> {
        if speed_feet == 0 || !speed_feet.is_multiple_of(5) || !position_feet.is_multiple_of(5) {
            return Err(invalid_state(
                "speed and position must use positive five-foot movement",
            ));
        }
        Ok(Self {
            base_speed_feet: speed_feet,
            remaining_feet: speed_feet,
            position_feet,
        })
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.base_speed_feet == 0
            || !self.base_speed_feet.is_multiple_of(5)
            || !self.remaining_feet.is_multiple_of(5)
            || !self.position_feet.is_multiple_of(5)
            || self.remaining_feet
                > self
                    .base_speed_feet
                    .saturating_mul(1 + MAX_MVP_DASHES_PER_TURN)
        {
            return Err(invalid_state(
                "movement state exceeds the level-two Fighter Dash profile",
            ));
        }
        Ok(())
    }

    pub fn reset(&mut self) {
        self.remaining_feet = self.base_speed_feet;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovementContext {
    pub difficult_terrain: bool,
    pub crawling: bool,
}

pub fn move_to(
    movement: &mut MovementState,
    destination_feet: u16,
    context: MovementContext,
    conditions: &ConditionSet,
) -> RulesMatrixResult<u16> {
    movement.validate()?;
    if !destination_feet.is_multiple_of(5) {
        return Err(invalid_intent(
            "movement destination must use five-foot increments",
        ));
    }
    let effects = condition_effects(conditions, RollSituation::AbilityCheck)?;
    if effects.speed_is_zero {
        return Err(invalid_intent("current conditions reduce speed to zero"));
    }
    let distance = movement.position_feet.abs_diff(destination_feet);
    let multiplier = 1_u16
        .checked_add(u16::from(context.difficult_terrain))
        .and_then(|value| value.checked_add(u16::from(context.crawling)))
        .ok_or(RulesMatrixError::ArithmeticOverflow)?;
    let cost = distance
        .checked_mul(multiplier)
        .ok_or(RulesMatrixError::ArithmeticOverflow)?;
    if cost > movement.remaining_feet {
        return Err(invalid_intent(
            "movement cost exceeds the remaining movement budget",
        ));
    }
    movement.remaining_feet -= cost;
    movement.position_feet = destination_feet;
    Ok(cost)
}

pub fn stand_from_prone(
    movement: &mut MovementState,
    conditions: &mut ConditionSet,
) -> RulesMatrixResult<u16> {
    movement.validate()?;
    conditions.validate()?;
    if !conditions.contains(ConditionId::Prone) {
        return Err(invalid_intent("actor is not prone"));
    }
    let blocked = condition_effects(conditions, RollSituation::AbilityCheck)?.speed_is_zero;
    if blocked {
        return Err(invalid_intent("an actor with zero speed cannot stand"));
    }
    let cost = movement.base_speed_feet / 2;
    if movement.remaining_feet < cost {
        return Err(invalid_intent("standing costs more movement than remains"));
    }
    movement.remaining_feet -= cost;
    conditions
        .active
        .retain(|condition| condition.condition != ConditionId::Prone);
    Ok(cost)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTurnState {
    pub schema_version: u16,
    pub actor_id: String,
    pub round: u32,
    pub active: bool,
    pub action_economy: ActionEconomy,
    pub movement: MovementState,
    pub conditions: ConditionSet,
}

impl RuntimeTurnState {
    pub fn new(
        actor_id: impl Into<String>,
        round: u32,
        speed_feet: u16,
        position_feet: u16,
        conditions: ConditionSet,
    ) -> RulesMatrixResult<Self> {
        let actor_id = actor_id.into();
        require_id(&actor_id, "turn actor ID is invalid")?;
        if round == 0 {
            return Err(invalid_state("combat round must be positive"));
        }
        conditions.validate()?;
        let result = Self {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            actor_id,
            round,
            active: true,
            action_economy: ActionEconomy::new(speed_feet),
            movement: MovementState::new(speed_feet, position_feet)?,
            conditions,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION || self.round == 0 {
            return Err(invalid_state("runtime turn schema or round is invalid"));
        }
        require_id(&self.actor_id, "turn actor ID is invalid")?;
        self.conditions.validate()?;
        self.movement.validate()?;
        if self.movement.remaining_feet != self.action_economy.movement_remaining_feet {
            return Err(invalid_state(
                "turn movement views disagree or exceed the Dash profile",
            ));
        }
        Ok(())
    }

    pub fn synchronize_movement(&mut self) {
        self.action_economy.movement_remaining_feet = self.movement.remaining_feet;
    }

    pub fn begin_next_turn(
        &mut self,
        round: u32,
        resources: &RuntimeResources,
    ) -> RulesMatrixResult<DurationResolution> {
        self.validate()?;
        if self.active || round < self.round || round > self.round.saturating_add(1) {
            return Err(invalid_intent("next turn round or active state is invalid"));
        }
        resources.validate()?;
        let mut next = self.clone();
        let duration = process_durations(
            &mut next.conditions,
            &DurationEvent::TurnBoundary {
                actor_id: next.actor_id.clone(),
                boundary: TurnBoundary::Start,
            },
        )?;
        next.round = round;
        next.active = true;
        next.action_economy
            .reset_for_turn(next.movement.base_speed_feet);
        next.movement.reset();
        grant_supported_bonus_action(resources, &mut next.action_economy)?;
        next.validate()?;
        *self = next;
        Ok(duration)
    }

    pub fn end_turn(&mut self) -> RulesMatrixResult<DurationResolution> {
        self.validate()?;
        if !self.active {
            return Err(invalid_intent("turn has already ended"));
        }
        let mut next = self.clone();
        let duration = process_durations(
            &mut next.conditions,
            &DurationEvent::TurnBoundary {
                actor_id: next.actor_id.clone(),
                boundary: TurnBoundary::End,
            },
        )?;
        next.active = false;
        next.validate()?;
        *self = next;
        Ok(duration)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyTrigger {
    CreatureEntersReach,
    CreatureEntersNormalRange,
    DoorOpens,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionContext {
    Attack {
        target_is_valid: bool,
        in_range: bool,
    },
    CastSpell {
        spell: SpellId,
        target_is_valid: bool,
        prepared: bool,
        slot_available: bool,
    },
    Dash,
    Disengage,
    Dodge,
    Help {
        target_id: String,
        target_is_valid: bool,
        target_within_five_feet: bool,
    },
    Hide {
        obscured_from_observers: bool,
    },
    Ready {
        trigger: ReadyTrigger,
        reaction_available: bool,
    },
    Search,
    UseObject {
        item: EquipmentId,
        item_is_carried: bool,
        authored_use_available: bool,
    },
    SecondWind {
        resource_available: bool,
    },
    ActionSurge {
        resource_available: bool,
    },
}

impl ActionContext {
    pub const fn capability(&self) -> ActionCapability {
        match self {
            Self::Attack { .. } => ActionCapability::Attack,
            Self::CastSpell { .. } => ActionCapability::CastSupportedSpell,
            Self::Dash => ActionCapability::Dash,
            Self::Disengage => ActionCapability::Disengage,
            Self::Dodge => ActionCapability::Dodge,
            Self::Help { .. } => ActionCapability::Help,
            Self::Hide { .. } => ActionCapability::Hide,
            Self::Ready { .. } => ActionCapability::Ready,
            Self::Search => ActionCapability::Search,
            Self::UseObject { .. } => ActionCapability::UseObject,
            Self::SecondWind { .. } => ActionCapability::SecondWind,
            Self::ActionSurge { .. } => ActionCapability::ActionSurge,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionBlocker {
    Incapacitated,
    ActionSpent,
    BonusActionUnavailable,
    ReactionUnavailable,
    InvalidTarget,
    OutOfRange,
    SpellNotPrepared,
    SpellSlotUnavailable,
    NotObscured,
    NoAuthoredObjectUse,
    ResourceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionAvailability {
    pub capability: ActionCapability,
    pub resource: Option<TurnResource>,
    pub blockers: Vec<ActionBlocker>,
}

impl ActionAvailability {
    pub fn is_available(&self) -> bool {
        self.blockers.is_empty()
    }
}

pub fn action_availability(
    economy: &ActionEconomy,
    conditions: &ConditionSet,
    context: &ActionContext,
) -> RulesMatrixResult<ActionAvailability> {
    conditions.validate()?;
    let effects = condition_effects(conditions, RollSituation::AbilityCheck)?;
    let capability = context.capability();
    let resource = match context {
        ActionContext::SecondWind { .. } => Some(TurnResource::BonusAction),
        ActionContext::ActionSurge { .. } => None,
        ActionContext::CastSpell {
            spell: SpellId::Shield,
            ..
        } => Some(TurnResource::Reaction),
        _ => Some(TurnResource::Action),
    };
    let mut blockers = Vec::new();
    if effects.actions_blocked {
        blockers.push(ActionBlocker::Incapacitated);
    }
    match resource {
        Some(TurnResource::Action) if !economy.action_available => {
            blockers.push(ActionBlocker::ActionSpent);
        }
        Some(TurnResource::BonusAction) if !economy.bonus_action_available => {
            blockers.push(ActionBlocker::BonusActionUnavailable);
        }
        Some(TurnResource::Reaction)
            if !economy.reaction_available || effects.reactions_blocked =>
        {
            blockers.push(ActionBlocker::ReactionUnavailable);
        }
        _ => {}
    }
    match context {
        ActionContext::Attack {
            target_is_valid,
            in_range,
        } => {
            if !target_is_valid {
                blockers.push(ActionBlocker::InvalidTarget);
            }
            if !in_range {
                blockers.push(ActionBlocker::OutOfRange);
            }
        }
        ActionContext::CastSpell {
            spell,
            target_is_valid,
            prepared,
            slot_available,
        } => {
            if !target_is_valid {
                blockers.push(ActionBlocker::InvalidTarget);
            }
            if spell.level() == 1 && !prepared {
                blockers.push(ActionBlocker::SpellNotPrepared);
            }
            if spell.level() == 1 && !slot_available {
                blockers.push(ActionBlocker::SpellSlotUnavailable);
            }
        }
        ActionContext::Help {
            target_id,
            target_is_valid,
            target_within_five_feet,
        } => {
            require_id(target_id, "help target ID is invalid")?;
            if !target_is_valid {
                blockers.push(ActionBlocker::InvalidTarget);
            }
            if !target_within_five_feet {
                blockers.push(ActionBlocker::OutOfRange);
            }
        }
        ActionContext::Hide {
            obscured_from_observers,
        } if !obscured_from_observers => blockers.push(ActionBlocker::NotObscured),
        ActionContext::Ready {
            reaction_available, ..
        } if !reaction_available => blockers.push(ActionBlocker::ReactionUnavailable),
        ActionContext::UseObject {
            item: _,
            item_is_carried,
            authored_use_available,
        } => {
            if !item_is_carried {
                blockers.push(ActionBlocker::InvalidTarget);
            }
            if !authored_use_available {
                blockers.push(ActionBlocker::NoAuthoredObjectUse);
            }
        }
        ActionContext::SecondWind { resource_available } => {
            if !resource_available {
                blockers.push(ActionBlocker::ResourceUnavailable);
            }
        }
        ActionContext::ActionSurge { resource_available } if !resource_available => {
            blockers.push(ActionBlocker::ResourceUnavailable);
        }
        _ => {}
    }
    blockers.sort_by_key(|blocker| *blocker as u8);
    blockers.dedup();
    Ok(ActionAvailability {
        capability,
        resource,
        blockers,
    })
}

pub fn spend_action(
    economy: &mut ActionEconomy,
    availability: &ActionAvailability,
) -> RulesMatrixResult<()> {
    if !availability.is_available() {
        return Err(invalid_intent("blocked action cannot be spent"));
    }
    if let Some(resource) = availability.resource {
        economy.spend(resource)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreActionEffect {
    AttackCommitted,
    SpellCastCommitted { spell: SpellId },
    Dashed { movement_gained_feet: u16 },
    DisengagedUntilTurnEnd,
    DodgingUntilNextTurn { benefit_active: bool },
    HelpGranted { target_id: String },
    HideCheckRequested,
    Readied { trigger: ReadyTrigger },
    SearchCheckRequested,
    ObjectUsed { item: EquipmentId },
}

/// Spends a validated core action and applies the deterministic turn-local
/// portion. Attack, spell, check, and object-specific effects use their typed
/// resolvers after this gate.
pub fn apply_core_action(
    economy: &mut ActionEconomy,
    movement: &mut MovementState,
    conditions: &ConditionSet,
    context: &ActionContext,
) -> RulesMatrixResult<CoreActionEffect> {
    let availability = action_availability(economy, conditions, context)?;
    if matches!(
        context,
        ActionContext::SecondWind { .. } | ActionContext::ActionSurge { .. }
    ) {
        return Err(invalid_intent(
            "class feature uses its dedicated transition",
        ));
    }
    movement.validate()?;
    let mut next_economy = economy.clone();
    let mut next_movement = movement.clone();
    let condition_effects = condition_effects(conditions, RollSituation::AbilityCheck)?;
    spend_action(&mut next_economy, &availability)?;
    let effect = match context {
        ActionContext::Attack { .. } => CoreActionEffect::AttackCommitted,
        ActionContext::CastSpell { spell, .. } => {
            CoreActionEffect::SpellCastCommitted { spell: *spell }
        }
        ActionContext::Dash => {
            let movement_gained_feet = if condition_effects.speed_is_zero {
                0
            } else {
                next_movement.base_speed_feet
            };
            next_movement.remaining_feet = next_movement
                .remaining_feet
                .checked_add(movement_gained_feet)
                .ok_or(RulesMatrixError::ArithmeticOverflow)?;
            next_economy.movement_remaining_feet = next_movement.remaining_feet;
            CoreActionEffect::Dashed {
                movement_gained_feet,
            }
        }
        ActionContext::Disengage => CoreActionEffect::DisengagedUntilTurnEnd,
        ActionContext::Dodge => CoreActionEffect::DodgingUntilNextTurn {
            benefit_active: !condition_effects.speed_is_zero,
        },
        ActionContext::Help { target_id, .. } => CoreActionEffect::HelpGranted {
            target_id: target_id.clone(),
        },
        ActionContext::Hide { .. } => CoreActionEffect::HideCheckRequested,
        ActionContext::Ready { trigger, .. } => CoreActionEffect::Readied { trigger: *trigger },
        ActionContext::Search => CoreActionEffect::SearchCheckRequested,
        ActionContext::UseObject { item, .. } => CoreActionEffect::ObjectUsed { item: *item },
        ActionContext::SecondWind { .. } | ActionContext::ActionSurge { .. } => {
            unreachable!("class features returned before spending")
        }
    };
    next_movement.validate()?;
    *economy = next_economy;
    *movement = next_movement;
    Ok(effect)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamageProfile {
    #[serde(default)]
    pub resistances: Vec<DamageType>,
    #[serde(default)]
    pub vulnerabilities: Vec<DamageType>,
    #[serde(default)]
    pub immunities: Vec<DamageType>,
}

impl DamageProfile {
    pub fn normal() -> Self {
        Self {
            resistances: Vec::new(),
            vulnerabilities: Vec::new(),
            immunities: Vec::new(),
        }
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        for list in [
            self.resistances.as_slice(),
            self.vulnerabilities.as_slice(),
            self.immunities.as_slice(),
        ] {
            if !list.windows(2).all(|pair| pair[0] < pair[1]) {
                return Err(invalid_state(
                    "damage interaction lists must be sorted and unique",
                ));
            }
        }
        if self
            .immunities
            .iter()
            .any(|kind| self.resistances.contains(kind) || self.vulnerabilities.contains(kind))
        {
            return Err(invalid_state(
                "immunity cannot overlap another damage interaction",
            ));
        }
        Ok(())
    }

    pub fn interaction(&self, damage_type: DamageType) -> RulesMatrixResult<DamageInteraction> {
        self.validate()?;
        Ok(if self.immunities.contains(&damage_type) {
            DamageInteraction::Immunity
        } else {
            match (
                self.resistances.contains(&damage_type),
                self.vulnerabilities.contains(&damage_type),
            ) {
                (true, true) | (false, false) => DamageInteraction::Normal,
                (true, false) => DamageInteraction::Resistance,
                (false, true) => DamageInteraction::Vulnerability,
            }
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeathSaveTally {
    pub successes: u8,
    pub failures: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VitalStatus {
    Active,
    Dying,
    Stable,
    Dead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthState {
    pub schema_version: u16,
    pub maximum: u16,
    pub current: u16,
    pub temporary: u16,
    pub vital_status: VitalStatus,
    pub death_saves: DeathSaveTally,
}

impl HealthState {
    pub fn new(maximum: u16) -> RulesMatrixResult<Self> {
        if maximum == 0 {
            return Err(invalid_state("maximum hit points must be positive"));
        }
        Ok(Self {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            maximum,
            current: maximum,
            temporary: 0,
            vital_status: VitalStatus::Active,
            death_saves: DeathSaveTally::default(),
        })
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION
            || self.maximum == 0
            || self.current > self.maximum
            || self.death_saves.successes > 3
            || self.death_saves.failures > 3
        {
            return Err(invalid_state(
                "health schema, bounds, or death-save counts are invalid",
            ));
        }
        match self.vital_status {
            VitalStatus::Active
                if self.current == 0 || self.death_saves != DeathSaveTally::default() =>
            {
                Err(invalid_state(
                    "active health requires hit points and cleared death saves",
                ))
            }
            VitalStatus::Dying
                if self.current != 0
                    || self.death_saves.successes >= 3
                    || self.death_saves.failures >= 3 =>
            {
                Err(invalid_state(
                    "dying health has a terminal or positive-HP state",
                ))
            }
            VitalStatus::Stable
                if self.current != 0
                    || self.death_saves.successes != 3
                    || self.death_saves.failures >= 3 =>
            {
                Err(invalid_state(
                    "stable health requires exactly three successes",
                ))
            }
            VitalStatus::Dead if self.current != 0 || self.death_saves.failures != 3 => {
                Err(invalid_state("dead health requires exactly three failures"))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamageRequest {
    pub amount: u16,
    pub damage_type: DamageType,
    pub critical_hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthConditionChange {
    ApplyZeroHitPointUnconscious,
    ApplyProne,
    RemoveZeroHitPointConditions,
    WakeFromSleep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamageResolution {
    pub requested_damage: u16,
    pub interaction: DamageInteraction,
    pub effective_damage: u16,
    pub temporary_hit_points_lost: u16,
    pub current_hit_points_lost: u16,
    pub excess_damage: u16,
    pub death_save_failures_added: u8,
    pub condition_changes: Vec<HealthConditionChange>,
    pub resulting_health: HealthState,
}

fn interacted_damage(amount: u16, interaction: DamageInteraction) -> RulesMatrixResult<u16> {
    match interaction {
        DamageInteraction::Normal => Ok(amount),
        DamageInteraction::Resistance => Ok(amount / 2),
        DamageInteraction::Vulnerability => amount
            .checked_mul(2)
            .ok_or(RulesMatrixError::ArithmeticOverflow),
        DamageInteraction::Immunity => Ok(0),
    }
}

pub fn apply_damage(
    health: &mut HealthState,
    profile: &DamageProfile,
    request: &DamageRequest,
) -> RulesMatrixResult<DamageResolution> {
    health.validate()?;
    profile.validate()?;
    if request.amount == 0 {
        return Err(invalid_intent("damage must be positive"));
    }
    if health.vital_status == VitalStatus::Dead {
        return Err(invalid_intent(
            "dead targets cannot receive this MVP damage transition",
        ));
    }

    let interaction = profile.interaction(request.damage_type)?;
    let effective_damage = interacted_damage(request.amount, interaction)?;
    let temporary_hit_points_lost = health.temporary.min(effective_damage);
    health.temporary -= temporary_hit_points_lost;
    let remaining_damage = effective_damage - temporary_hit_points_lost;
    let current_before = health.current;
    let current_hit_points_lost = current_before.min(remaining_damage);
    health.current -= current_hit_points_lost;
    let excess_damage = remaining_damage - current_hit_points_lost;
    let mut condition_changes = Vec::new();
    let mut death_save_failures_added = 0;

    if effective_damage > 0 {
        condition_changes.push(HealthConditionChange::WakeFromSleep);
    }
    if current_before > 0 && health.current == 0 {
        if excess_damage >= health.maximum {
            health.vital_status = VitalStatus::Dead;
            health.death_saves = DeathSaveTally {
                successes: 0,
                failures: 3,
            };
        } else {
            health.vital_status = VitalStatus::Dying;
            health.death_saves = DeathSaveTally::default();
            condition_changes.extend([
                HealthConditionChange::ApplyZeroHitPointUnconscious,
                HealthConditionChange::ApplyProne,
            ]);
        }
    } else if current_before == 0 && effective_damage > 0 {
        if health.vital_status == VitalStatus::Stable {
            health.vital_status = VitalStatus::Dying;
            health.death_saves = DeathSaveTally::default();
        }
        if health.vital_status == VitalStatus::Dying {
            death_save_failures_added = if request.critical_hit { 2 } else { 1 };
            health.death_saves.failures = health
                .death_saves
                .failures
                .saturating_add(death_save_failures_added)
                .min(3);
            if health.death_saves.failures == 3 {
                health.vital_status = VitalStatus::Dead;
            }
        }
    }
    health.validate()?;
    Ok(DamageResolution {
        requested_damage: request.amount,
        interaction,
        effective_damage,
        temporary_hit_points_lost,
        current_hit_points_lost,
        excess_damage,
        death_save_failures_added,
        condition_changes,
        resulting_health: health.clone(),
    })
}

/// Applies the condition facts emitted by health transitions without guessing
/// at their source. This keeps hit-point state and condition state consistent.
pub fn apply_health_condition_changes(
    actor_id: &str,
    conditions: &mut ConditionSet,
    changes: &[HealthConditionChange],
) -> RulesMatrixResult<()> {
    require_id(actor_id, "health-condition actor ID is invalid")?;
    conditions.validate()?;
    for change in changes {
        match change {
            HealthConditionChange::ApplyZeroHitPointUnconscious => {
                if !conditions.active.iter().any(|active| {
                    active.condition == ConditionId::Unconscious
                        && matches!(
                            &active.source,
                            ConditionSource::Mechanic { mechanic_id }
                                if mechanic_id == "health.zero-hit-points"
                        )
                }) {
                    conditions.apply(ActiveCondition {
                        condition: ConditionId::Unconscious,
                        source: ConditionSource::Mechanic {
                            mechanic_id: "health.zero-hit-points".to_owned(),
                        },
                        duration: EffectDuration::Permanent,
                    })?;
                }
            }
            HealthConditionChange::ApplyProne => {
                if !conditions.active.iter().any(|active| {
                    active.condition == ConditionId::Prone
                        && matches!(
                            &active.source,
                            ConditionSource::Mechanic { mechanic_id }
                                if mechanic_id == "health.zero-hit-points"
                        )
                }) {
                    conditions.apply(ActiveCondition {
                        condition: ConditionId::Prone,
                        source: ConditionSource::Mechanic {
                            mechanic_id: "health.zero-hit-points".to_owned(),
                        },
                        duration: EffectDuration::Permanent,
                    })?;
                }
            }
            HealthConditionChange::RemoveZeroHitPointConditions => {
                conditions.active.retain(|active| {
                    !matches!(
                        &active.source,
                        ConditionSource::Mechanic { mechanic_id }
                            if mechanic_id == "health.zero-hit-points"
                    )
                });
            }
            HealthConditionChange::WakeFromSleep => {
                let _ = process_durations(
                    conditions,
                    &DurationEvent::Damaged {
                        actor_id: actor_id.to_owned(),
                    },
                )?;
            }
        }
    }
    conditions.validate()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealingResolution {
    pub requested_healing: u16,
    pub effective_healing: u16,
    pub condition_changes: Vec<HealthConditionChange>,
    pub resulting_health: HealthState,
}

pub fn apply_healing(
    health: &mut HealthState,
    amount: u16,
) -> RulesMatrixResult<HealingResolution> {
    health.validate()?;
    if amount == 0 {
        return Err(invalid_intent("healing must be positive"));
    }
    if health.vital_status == VitalStatus::Dead {
        return Err(unsupported_mechanic("health.restore-dead"));
    }
    let missing = health.maximum - health.current;
    let effective_healing = missing.min(amount);
    health.current += effective_healing;
    let condition_changes = if effective_healing > 0 && health.vital_status != VitalStatus::Active {
        health.vital_status = VitalStatus::Active;
        health.death_saves = DeathSaveTally::default();
        vec![HealthConditionChange::RemoveZeroHitPointConditions]
    } else {
        Vec::new()
    };
    health.validate()?;
    Ok(HealingResolution {
        requested_healing: amount,
        effective_healing,
        condition_changes,
        resulting_health: health.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefeatRecoveryResolution {
    pub policy: LethalityPolicy,
    pub story_recovery_applied: bool,
    pub condition_changes: Vec<HealthConditionChange>,
    pub resulting_health: HealthState,
}

pub fn apply_defeat_recovery(
    policy: LethalityPolicy,
    health: &mut HealthState,
) -> RulesMatrixResult<DefeatRecoveryResolution> {
    health.validate()?;
    let mut condition_changes = Vec::new();
    let story_recovery_applied =
        policy == LethalityPolicy::StoryRecovery && health.vital_status != VitalStatus::Active;
    if story_recovery_applied {
        health.current = 1;
        health.vital_status = VitalStatus::Active;
        health.death_saves = DeathSaveTally::default();
        condition_changes.push(HealthConditionChange::RemoveZeroHitPointConditions);
    }
    health.validate()?;
    Ok(DefeatRecoveryResolution {
        policy,
        story_recovery_applied,
        condition_changes,
        resulting_health: health.clone(),
    })
}

pub fn grant_temporary_hit_points(
    health: &mut HealthState,
    offered: u16,
) -> RulesMatrixResult<u16> {
    health.validate()?;
    if offered == 0 {
        return Err(invalid_intent("temporary hit point offer must be positive"));
    }
    let previous = health.temporary;
    if offered > previous {
        health.temporary = offered;
    }
    Ok(health.temporary - previous)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathSaveOutcome {
    Success,
    Failure,
    CriticalFailure,
    Revived,
    Stabilized,
    Died,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeathSaveResolution {
    pub roll: D20Roll,
    pub outcome: DeathSaveOutcome,
    pub condition_changes: Vec<HealthConditionChange>,
    pub resulting_health: HealthState,
}

pub fn resolve_death_save(
    health: &mut HealthState,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<DeathSaveResolution> {
    health.validate()?;
    if health.vital_status != VitalStatus::Dying {
        return Err(invalid_intent("only a dying hero makes death saves"));
    }
    let roll = resolve_d20(dice, RollContext::normal())?;
    let mut condition_changes = Vec::new();
    let outcome = match roll.selected {
        20 => {
            health.current = 1;
            health.vital_status = VitalStatus::Active;
            health.death_saves = DeathSaveTally::default();
            condition_changes.push(HealthConditionChange::RemoveZeroHitPointConditions);
            DeathSaveOutcome::Revived
        }
        1 => {
            health.death_saves.failures = health.death_saves.failures.saturating_add(2).min(3);
            if health.death_saves.failures == 3 {
                health.vital_status = VitalStatus::Dead;
                DeathSaveOutcome::Died
            } else {
                DeathSaveOutcome::CriticalFailure
            }
        }
        10..=19 => {
            health.death_saves.successes += 1;
            if health.death_saves.successes == 3 {
                health.vital_status = VitalStatus::Stable;
                DeathSaveOutcome::Stabilized
            } else {
                DeathSaveOutcome::Success
            }
        }
        _ => {
            health.death_saves.failures += 1;
            if health.death_saves.failures == 3 {
                health.vital_status = VitalStatus::Dead;
                DeathSaveOutcome::Died
            } else {
                DeathSaveOutcome::Failure
            }
        }
    };
    health.validate()?;
    Ok(DeathSaveResolution {
        roll,
        outcome,
        condition_changes,
        resulting_health: health.clone(),
    })
}

pub fn stabilize(health: &mut HealthState) -> RulesMatrixResult<()> {
    health.validate()?;
    if health.vital_status != VitalStatus::Dying {
        return Err(invalid_intent("only a dying target can be stabilized"));
    }
    health.vital_status = VitalStatus::Stable;
    health.death_saves = DeathSaveTally {
        successes: 3,
        failures: health.death_saves.failures,
    };
    health.validate()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamageDiceResolution {
    pub sides: u8,
    pub dice: Vec<u8>,
    pub constant: i8,
    pub critical: bool,
    pub total: u16,
}

pub fn resolve_damage_dice(
    count: u8,
    sides: u8,
    constant: i8,
    critical: bool,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<DamageDiceResolution> {
    if !(1..=20).contains(&count) || !(2..=100).contains(&sides) {
        return Err(invalid_intent(
            "damage dice are outside the bounded MVP grammar",
        ));
    }
    let rolled_count = count
        .checked_mul(if critical { 2 } else { 1 })
        .ok_or(RulesMatrixError::ArithmeticOverflow)?;
    let mut rolled = Vec::with_capacity(usize::from(rolled_count));
    for _ in 0..rolled_count {
        let value = dice.roll(u16::from(sides));
        let value =
            u8::try_from(value).map_err(|_| invalid_state("dice source value overflowed"))?;
        if !(1..=sides).contains(&value) {
            return Err(RulesMatrixError::Core(GameCoreError::InvalidDieRoll {
                sides: u16::from(sides),
                value: u16::from(value),
            }));
        }
        rolled.push(value);
    }
    let rolled_total: i32 = rolled.iter().map(|value| i32::from(*value)).sum();
    let total = (rolled_total + i32::from(constant)).max(0);
    let total = u16::try_from(total).map_err(|_| RulesMatrixError::ArithmeticOverflow)?;
    Ok(DamageDiceResolution {
        sides,
        dice: rolled,
        constant,
        critical,
        total,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponAttackTarget {
    pub target_id: String,
    pub distance_feet: u16,
    pub armor_class: u8,
    pub cover: Cover,
    pub threatening_hostile_within_five_feet: bool,
    pub damage_profile: DamageProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponAttackResolution {
    pub attack_id: String,
    pub range: RangeResolution,
    pub attack: D20TestResolution,
    pub damage: Option<DamageDiceResolution>,
    pub damage_interaction: Option<DamageInteraction>,
    pub effective_damage: u16,
    pub critical_damage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponAttackConditionContext {
    pub base_roll_context: RollContext,
    #[serde(default)]
    pub situational_modifiers: Vec<SituationalModifier>,
    pub attacker_conditions: ConditionSet,
    pub target_conditions: ConditionSet,
}

pub fn resolve_weapon_attack(
    sheet: &DerivedHeroSheet,
    attack_id: &str,
    mode: AttackMode,
    target: &WeaponAttackTarget,
    base_roll_context: RollContext,
    situational_modifiers: Vec<SituationalModifier>,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<WeaponAttackResolution> {
    resolve_weapon_attack_internal(
        sheet,
        attack_id,
        mode,
        target,
        base_roll_context,
        situational_modifiers,
        false,
        dice,
    )
}

pub fn resolve_conditioned_weapon_attack(
    sheet: &DerivedHeroSheet,
    attack_id: &str,
    mode: AttackMode,
    target: &WeaponAttackTarget,
    context: &WeaponAttackConditionContext,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<WeaponAttackResolution> {
    let attacker = condition_effects(&context.attacker_conditions, RollSituation::OwnAttack)?;
    if attacker.actions_blocked {
        return Err(invalid_intent(
            "current conditions prevent the attack action",
        ));
    }
    let incoming = condition_effects(
        &context.target_conditions,
        RollSituation::IncomingAttack {
            attacker_distance_feet: target.distance_feet,
        },
    )?;
    let conditioned = combine_roll_context(
        combine_roll_context(context.base_roll_context, attacker.roll_context),
        incoming.roll_context,
    );
    resolve_weapon_attack_internal(
        sheet,
        attack_id,
        mode,
        target,
        conditioned,
        context.situational_modifiers.clone(),
        incoming.incoming_hit_is_critical,
        dice,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_weapon_attack_internal(
    sheet: &DerivedHeroSheet,
    attack_id: &str,
    mode: AttackMode,
    target: &WeaponAttackTarget,
    base_roll_context: RollContext,
    situational_modifiers: Vec<SituationalModifier>,
    critical_hit_override: bool,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<WeaponAttackResolution> {
    require_id(attack_id, "attack ID is invalid")?;
    require_id(&target.target_id, "attack target ID is invalid")?;
    target.damage_profile.validate()?;
    let attack_summary: &AttackSummary = sheet
        .attacks
        .iter()
        .find(|attack| attack.attack_id == attack_id)
        .ok_or_else(|| unsupported_mechanic(attack_id))?;
    let range = resolve_weapon_range(
        &attack_summary.weapon,
        mode,
        target.distance_feet,
        target.threatening_hostile_within_five_feet,
    )?;
    let range_context = RollContext {
        advantage_sources: 0,
        disadvantage_sources: range.disadvantage_sources,
    };
    let roll_context = combine_roll_context(base_roll_context, range_context);
    let expected_attack_bonus =
        sheet.ability_scores.get(attack_summary.ability).modifier() + sheet.proficiency_bonus as i8;
    if attack_summary.attack_bonus != expected_attack_bonus {
        return Err(invalid_state(
            "derived attack bonus does not match ability and proficiency",
        ));
    }
    if !(1..=20).contains(&attack_summary.damage.count)
        || !(2..=100).contains(&attack_summary.damage.sides)
    {
        return Err(invalid_state(
            "derived attack damage dice are outside the MVP grammar",
        ));
    }
    let attack = resolve_d20_test(
        &sheet.ability_scores,
        Level::new(sheet.level.value())?,
        &D20TestRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: attack_summary.ability,
            proficiency: Proficiency::Proficient,
            roll_context,
            situational_modifiers,
            target: D20Target::Attack {
                armor_class: target.armor_class,
                cover: target.cover,
            },
        },
        dice,
    )?;
    let critical_damage = attack.outcome.succeeds()
        && (attack.outcome == D20TestOutcome::CriticalHit || critical_hit_override);
    let (damage, damage_interaction, effective_damage) = if attack.outcome.succeeds() {
        let damage = resolve_damage_dice(
            attack_summary.damage.count,
            attack_summary.damage.sides,
            attack_summary.damage.constant,
            critical_damage,
            dice,
        )?;
        let interaction = target
            .damage_profile
            .interaction(attack_summary.damage_type)?;
        let effective_damage = interacted_damage(damage.total, interaction)?;
        (Some(damage), Some(interaction), effective_damage)
    } else {
        (None, None, 0)
    };
    Ok(WeaponAttackResolution {
        attack_id: attack_id.to_owned(),
        range,
        attack,
        damage,
        damage_interaction,
        effective_damage,
        critical_damage,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCounter {
    pub kind: ResourceKind,
    pub current: u8,
    pub maximum: u8,
}

impl ResourceCounter {
    fn validate(&self) -> RulesMatrixResult<()> {
        if self.maximum == 0 || self.current > self.maximum {
            Err(invalid_state("resource counter is outside its maximum"))
        } else {
            Ok(())
        }
    }

    fn spend(&mut self) -> RulesMatrixResult<()> {
        self.validate()?;
        if self.current == 0 {
            return Err(invalid_intent("class resource is unavailable"));
        }
        self.current -= 1;
        Ok(())
    }

    fn restore_all(&mut self) {
        self.current = self.maximum;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeResources {
    pub schema_version: u16,
    pub class: HeroClass,
    pub level: SupportedLevel,
    pub hit_dice: ResourceCounter,
    pub second_wind: Option<ResourceCounter>,
    pub action_surge: Option<ResourceCounter>,
    pub level_one_spell_slots: Option<ResourceCounter>,
    pub arcane_recovery: Option<ResourceCounter>,
}

impl RuntimeResources {
    pub fn new(class: HeroClass, level: SupportedLevel) -> Self {
        let hit_dice = ResourceCounter {
            kind: match class {
                HeroClass::Fighter => ResourceKind::HitDiceD10,
                HeroClass::Wizard => ResourceKind::HitDiceD6,
            },
            current: level.value(),
            maximum: level.value(),
        };
        match class {
            HeroClass::Fighter => Self {
                schema_version: RULES_MATRIX_SCHEMA_VERSION,
                class,
                level,
                hit_dice,
                second_wind: Some(ResourceCounter {
                    kind: ResourceKind::SecondWind,
                    current: 1,
                    maximum: 1,
                }),
                action_surge: (level == SupportedLevel::Two).then_some(ResourceCounter {
                    kind: ResourceKind::ActionSurge,
                    current: 1,
                    maximum: 1,
                }),
                level_one_spell_slots: None,
                arcane_recovery: None,
            },
            HeroClass::Wizard => Self {
                schema_version: RULES_MATRIX_SCHEMA_VERSION,
                class,
                level,
                hit_dice,
                second_wind: None,
                action_surge: None,
                level_one_spell_slots: Some(ResourceCounter {
                    kind: ResourceKind::LevelOneSpellSlots,
                    current: if level == SupportedLevel::One { 2 } else { 3 },
                    maximum: if level == SupportedLevel::One { 2 } else { 3 },
                }),
                arcane_recovery: Some(ResourceCounter {
                    kind: ResourceKind::ArcaneRecovery,
                    current: 1,
                    maximum: 1,
                }),
            },
        }
    }

    pub fn from_derived_sheet(
        class: HeroClass,
        sheet: &DerivedHeroSheet,
    ) -> RulesMatrixResult<Self> {
        let mut expected = Self::new(class, sheet.level);
        let expected_pools = [
            Some(&expected.hit_dice),
            expected.second_wind.as_ref(),
            expected.action_surge.as_ref(),
            expected.level_one_spell_slots.as_ref(),
            expected.arcane_recovery.as_ref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if sheet.resources.len() != expected_pools.len()
            || expected_pools.iter().any(|expected| {
                !sheet
                    .resources
                    .iter()
                    .any(|pool| pool.resource == expected.kind && pool.maximum == expected.maximum)
            })
        {
            return Err(invalid_state(
                "derived sheet resources do not match the runtime profile",
            ));
        }
        for counter in [
            Some(&mut expected.hit_dice),
            expected.second_wind.as_mut(),
            expected.action_surge.as_mut(),
            expected.level_one_spell_slots.as_mut(),
            expected.arcane_recovery.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            counter.current = sheet
                .resources
                .iter()
                .find(|pool| pool.resource == counter.kind)
                .ok_or_else(|| invalid_state("derived sheet resource is missing"))?
                .current;
        }
        expected.validate()?;
        Ok(expected)
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION {
            return Err(invalid_state(
                "runtime resource schema version is unsupported",
            ));
        }
        self.hit_dice.validate()?;
        for pool in [
            self.second_wind.as_ref(),
            self.action_surge.as_ref(),
            self.level_one_spell_slots.as_ref(),
            self.arcane_recovery.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            pool.validate()?;
        }
        let expected = Self::new(self.class, self.level);
        let shape_matches = self.hit_dice.kind == expected.hit_dice.kind
            && self.hit_dice.maximum == expected.hit_dice.maximum
            && option_resource_shape(&self.second_wind, &expected.second_wind)
            && option_resource_shape(&self.action_surge, &expected.action_surge)
            && option_resource_shape(&self.level_one_spell_slots, &expected.level_one_spell_slots)
            && option_resource_shape(&self.arcane_recovery, &expected.arcane_recovery);
        if !shape_matches {
            return Err(invalid_state(
                "runtime resources do not match class and level",
            ));
        }
        Ok(())
    }

    pub fn has_spell_slot(&self) -> bool {
        self.level_one_spell_slots
            .is_some_and(|pool| pool.current > 0)
    }

    fn spend_spell_slot(&mut self) -> RulesMatrixResult<()> {
        self.level_one_spell_slots
            .as_mut()
            .ok_or_else(|| unsupported_mechanic("resource.level-one-spell-slot"))?
            .spend()
    }
}

fn option_resource_shape(
    actual: &Option<ResourceCounter>,
    expected: &Option<ResourceCounter>,
) -> bool {
    match (actual, expected) {
        (None, None) => true,
        (Some(actual), Some(expected)) => {
            actual.kind == expected.kind && actual.maximum == expected.maximum
        }
        _ => false,
    }
}

/// Grants the one supported fighter bonus-action option for the current turn.
/// `ActionEconomy` intentionally starts with no generic bonus action.
pub fn grant_supported_bonus_action(
    resources: &RuntimeResources,
    economy: &mut ActionEconomy,
) -> RulesMatrixResult<()> {
    resources.validate()?;
    if resources
        .second_wind
        .is_some_and(|resource| resource.current > 0)
    {
        economy.grant_bonus_action();
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecondWindResolution {
    pub healing_roll: u8,
    pub level_bonus: u8,
    pub healing: HealingResolution,
}

pub fn use_second_wind(
    resources: &mut RuntimeResources,
    economy: &mut ActionEconomy,
    health: &mut HealthState,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<SecondWindResolution> {
    resources.validate()?;
    health.validate()?;
    if resources.class != HeroClass::Fighter {
        return Err(unsupported_mechanic("feature.second-wind"));
    }
    if !economy.bonus_action_available {
        return Err(invalid_intent(
            "second wind requires the available bonus action",
        ));
    }
    if resources
        .second_wind
        .is_none_or(|resource| resource.current == 0)
    {
        return Err(invalid_intent("second wind resource is unavailable"));
    }
    let roll = dice.roll(10);
    let healing_roll =
        u8::try_from(roll).map_err(|_| invalid_state("dice source value overflowed"))?;
    if !(1..=10).contains(&healing_roll) {
        return Err(RulesMatrixError::Core(GameCoreError::InvalidDieRoll {
            sides: 10,
            value: roll,
        }));
    }
    let healing_amount = u16::from(healing_roll) + u16::from(resources.level.value());
    let mut next_resources = resources.clone();
    next_resources
        .second_wind
        .as_mut()
        .expect("validated fighter resources contain second wind")
        .spend()?;
    let mut next_health = health.clone();
    let healing = apply_healing(&mut next_health, healing_amount)?;
    economy.spend(TurnResource::BonusAction)?;
    *resources = next_resources;
    *health = next_health;
    Ok(SecondWindResolution {
        healing_roll,
        level_bonus: resources.level.value(),
        healing,
    })
}

pub fn use_action_surge(
    resources: &mut RuntimeResources,
    economy: &mut ActionEconomy,
) -> RulesMatrixResult<()> {
    resources.validate()?;
    if economy.action_available {
        return Err(invalid_intent(
            "action surge is only needed after spending the action",
        ));
    }
    let pool = resources
        .action_surge
        .as_mut()
        .ok_or_else(|| unsupported_mechanic("feature.action-surge"))?;
    pool.spend()?;
    economy.action_available = true;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortRestRequest {
    pub hit_dice_to_spend: u8,
    pub use_arcane_recovery: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestResolution {
    pub hit_die_rolls: Vec<u8>,
    pub hit_points_recovered: u16,
    pub hit_dice_recovered: u8,
    pub spell_slots_recovered: u8,
    pub resulting_health: HealthState,
    pub resulting_resources: RuntimeResources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitDieSpendResolution {
    pub hit_die: ResourceKind,
    pub roll: u8,
    pub constitution_modifier: i8,
    pub healing_total: u16,
    pub hit_points_recovered: u16,
    pub condition_changes: Vec<HealthConditionChange>,
    pub resulting_health: HealthState,
    pub resulting_resources: RuntimeResources,
}

fn hit_die_sides(resources: &RuntimeResources) -> RulesMatrixResult<u16> {
    match resources.hit_dice.kind {
        ResourceKind::HitDiceD6 => Ok(6),
        ResourceKind::HitDiceD10 => Ok(10),
        _ => Err(invalid_state("runtime hit-die resource has the wrong kind")),
    }
}

fn validate_hit_die_spend(
    resources: &RuntimeResources,
    health: &HealthState,
    constitution_modifier: i8,
) -> RulesMatrixResult<u16> {
    resources.validate()?;
    health.validate()?;
    if health.vital_status == VitalStatus::Dead || resources.hit_dice.current == 0 {
        return Err(invalid_intent(
            "a living hero with an available hit die is required",
        ));
    }
    if !(-5..=10).contains(&constitution_modifier) {
        return Err(invalid_intent(
            "constitution modifier is outside the creature range",
        ));
    }
    hit_die_sides(resources)
}

/// Spends exactly one hit die after a validated short rest. Calling this
/// transition repeatedly lets the player inspect each result before deciding
/// whether to spend another die, as required by the pinned rules profile.
pub fn spend_hit_die(
    resources: &mut RuntimeResources,
    health: &mut HealthState,
    constitution_modifier: i8,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<HitDieSpendResolution> {
    let die_sides = validate_hit_die_spend(resources, health, constitution_modifier)?;
    let value = dice.roll(die_sides);
    if !(1..=die_sides).contains(&value) {
        return Err(RulesMatrixError::Core(GameCoreError::InvalidDieRoll {
            sides: die_sides,
            value,
        }));
    }

    let healing_total = (i16::try_from(value).expect("d10 fits i16")
        + i16::from(constitution_modifier))
    .max(0) as u16;
    let mut next_resources = resources.clone();
    next_resources.hit_dice.current -= 1;
    let mut next_health = health.clone();
    let (hit_points_recovered, condition_changes) = if healing_total == 0 {
        (0, Vec::new())
    } else {
        let healing = apply_healing(&mut next_health, healing_total)?;
        (healing.effective_healing, healing.condition_changes)
    };
    next_resources.validate()?;
    next_health.validate()?;
    *resources = next_resources.clone();
    *health = next_health.clone();
    Ok(HitDieSpendResolution {
        hit_die: resources.hit_dice.kind,
        roll: value as u8,
        constitution_modifier,
        healing_total,
        hit_points_recovered,
        condition_changes,
        resulting_health: next_health,
        resulting_resources: next_resources,
    })
}

fn validate_arcane_recovery_request(
    resources: &RuntimeResources,
    requested: bool,
) -> RulesMatrixResult<()> {
    if !requested {
        return Ok(());
    }
    let recovery = resources
        .arcane_recovery
        .as_ref()
        .ok_or_else(|| unsupported_mechanic("feature.arcane-recovery"))?;
    let slots = resources
        .level_one_spell_slots
        .as_ref()
        .ok_or_else(|| unsupported_mechanic("resource.level-one-spell-slot"))?;
    if recovery.current == 0 || slots.current == slots.maximum {
        return Err(invalid_intent(
            "arcane recovery or a missing spell slot is unavailable",
        ));
    }
    Ok(())
}

/// Applies benefits for a trusted, completed short rest. Interactive callers
/// may first invoke [`spend_hit_die`] one die at a time and then finish with a
/// zero-die request; the batched count remains an atomic convenience.
pub fn take_short_rest(
    resources: &mut RuntimeResources,
    health: &mut HealthState,
    constitution_modifier: i8,
    request: &ShortRestRequest,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<RestResolution> {
    resources.validate()?;
    health.validate()?;
    if health.vital_status == VitalStatus::Dead
        || request.hit_dice_to_spend > resources.hit_dice.current
    {
        return Err(invalid_intent(
            "short-rest hit dice or health state are invalid",
        ));
    }
    if !(-5..=10).contains(&constitution_modifier) {
        return Err(invalid_intent(
            "constitution modifier is outside the creature range",
        ));
    }
    let _ = hit_die_sides(resources)?;
    validate_arcane_recovery_request(resources, request.use_arcane_recovery)?;

    let mut next_resources = resources.clone();
    let mut next_health = health.clone();
    let mut rolls = Vec::with_capacity(usize::from(request.hit_dice_to_spend));
    let mut recovered = 0_u16;
    for _ in 0..request.hit_dice_to_spend {
        let spend = spend_hit_die(
            &mut next_resources,
            &mut next_health,
            constitution_modifier,
            dice,
        )?;
        rolls.push(spend.roll);
        recovered = recovered
            .checked_add(spend.hit_points_recovered)
            .ok_or(RulesMatrixError::ArithmeticOverflow)?;
    }
    if let Some(pool) = &mut next_resources.second_wind {
        pool.restore_all();
    }
    if let Some(pool) = &mut next_resources.action_surge {
        pool.restore_all();
    }
    let mut spell_slots_recovered = 0;
    if request.use_arcane_recovery {
        let recovery = next_resources
            .arcane_recovery
            .as_mut()
            .expect("validated Arcane Recovery request has its resource");
        let slots = next_resources
            .level_one_spell_slots
            .as_mut()
            .expect("validated wizard resources contain spell slots");
        recovery.spend()?;
        slots.current += 1;
        spell_slots_recovered = 1;
    }
    next_resources.validate()?;
    next_health.validate()?;
    *resources = next_resources.clone();
    *health = next_health.clone();
    Ok(RestResolution {
        hit_die_rolls: rolls,
        hit_points_recovered: recovered,
        hit_dice_recovered: 0,
        spell_slots_recovered,
        resulting_health: next_health,
        resulting_resources: next_resources,
    })
}

/// Applies benefits after trusted campaign time and interruption policy has
/// established that a long rest completed. This pure transition intentionally
/// does not accept a client-authored clock or waive the positive-HP rule.
pub fn take_long_rest(
    resources: &mut RuntimeResources,
    health: &mut HealthState,
) -> RulesMatrixResult<RestResolution> {
    resources.validate()?;
    health.validate()?;
    if health.vital_status != VitalStatus::Active {
        return Err(invalid_intent("the MVP long rest requires an active hero"));
    }
    let mut next_resources = resources.clone();
    let regain_limit = (next_resources.hit_dice.maximum / 2).max(1);
    let missing_hit_dice = next_resources.hit_dice.maximum - next_resources.hit_dice.current;
    let hit_dice_recovered = missing_hit_dice.min(regain_limit);
    next_resources.hit_dice.current += hit_dice_recovered;
    let spell_slots_recovered = next_resources
        .level_one_spell_slots
        .map_or(0, |slots| slots.maximum - slots.current);
    for pool in [
        next_resources.second_wind.as_mut(),
        next_resources.action_surge.as_mut(),
        next_resources.level_one_spell_slots.as_mut(),
        next_resources.arcane_recovery.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        pool.restore_all();
    }
    let mut next_health = health.clone();
    let hit_points_recovered = next_health.maximum - next_health.current;
    next_health.current = next_health.maximum;
    next_health.temporary = 0;
    next_health.death_saves = DeathSaveTally::default();
    next_health.vital_status = VitalStatus::Active;
    next_resources.validate()?;
    next_health.validate()?;
    *resources = next_resources.clone();
    *health = next_health.clone();
    Ok(RestResolution {
        hit_die_rolls: Vec::new(),
        hit_points_recovered,
        hit_dice_recovered,
        spell_slots_recovered,
        resulting_health: next_health,
        resulting_resources: next_resources,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Currency {
    pub copper: u32,
    pub silver: u32,
    pub gold: u32,
}

impl Currency {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        if [self.copper, self.silver, self.gold]
            .into_iter()
            .any(|pieces| pieces > MAX_CURRENCY_PIECES)
        {
            return Err(invalid_state("currency exceeds the bounded MVP wallet"));
        }
        self.total_copper()?;
        Ok(())
    }

    pub fn total_copper(&self) -> RulesMatrixResult<u64> {
        u64::from(self.gold)
            .checked_mul(100)
            .and_then(|value| value.checked_add(u64::from(self.silver) * 10))
            .and_then(|value| value.checked_add(u64::from(self.copper)))
            .ok_or(RulesMatrixError::ArithmeticOverflow)
    }

    fn from_total_copper(total: u64) -> RulesMatrixResult<Self> {
        let gold = u32::try_from(total / 100).map_err(|_| RulesMatrixError::ArithmeticOverflow)?;
        let silver =
            u32::try_from((total % 100) / 10).map_err(|_| RulesMatrixError::ArithmeticOverflow)?;
        let copper = u32::try_from(total % 10).map_err(|_| RulesMatrixError::ArithmeticOverflow)?;
        let result = Self {
            copper,
            silver,
            gold,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn credit(&mut self, amount: Currency) -> RulesMatrixResult<()> {
        self.validate()?;
        amount.validate()?;
        let total = self
            .total_copper()?
            .checked_add(amount.total_copper()?)
            .ok_or(RulesMatrixError::ArithmeticOverflow)?;
        *self = Self::from_total_copper(total)?;
        Ok(())
    }

    pub fn debit(&mut self, amount: Currency) -> RulesMatrixResult<()> {
        self.validate()?;
        amount.validate()?;
        let total = self
            .total_copper()?
            .checked_sub(amount.total_copper()?)
            .ok_or_else(|| invalid_intent("wallet cannot cover the requested currency amount"))?;
        *self = Self::from_total_copper(total)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryEntry {
    pub item: EquipmentId,
    pub quantity: u8,
    pub equipped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryState {
    pub schema_version: u16,
    pub capacity_policy_id: String,
    pub entries: Vec<InventoryEntry>,
    pub simple_weapon: Option<SimpleWeaponId>,
    pub held_weapon: WeaponChoice,
    pub currency: Currency,
}

impl InventoryState {
    pub fn from_equipment(equipment: &EquipmentState) -> RulesMatrixResult<Self> {
        if equipment.capacity_policy_id != AUTHORED_CAPACITY_POLICY_ID {
            return Err(unsupported_mechanic("capacity.policy"));
        }
        let held_weapon = if equipment.carried.contains(&EquipmentId::Longsword) {
            WeaponChoice::Longsword
        } else if let Some(kind) = equipment.simple_weapon {
            WeaponChoice::Simple { kind }
        } else if equipment.carried.contains(&EquipmentId::LightCrossbow) {
            WeaponChoice::LightCrossbow
        } else {
            return Err(invalid_state("authored inventory has no supported weapon"));
        };
        let result = Self {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            capacity_policy_id: equipment.capacity_policy_id.clone(),
            entries: equipment
                .carried
                .iter()
                .map(|item| InventoryEntry {
                    item: *item,
                    quantity: 1,
                    equipped: equipment.equipped_armor == Some(*item)
                        || (*item == EquipmentId::Shield && equipment.shield_equipped),
                })
                .collect(),
            simple_weapon: equipment.simple_weapon,
            held_weapon,
            currency: Currency::default(),
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION
            || self.capacity_policy_id != AUTHORED_CAPACITY_POLICY_ID
            || self.entries.is_empty()
            || self.entries.len() > EquipmentId::ALL.len()
        {
            return Err(invalid_state(
                "inventory schema, capacity policy, or size is invalid",
            ));
        }
        self.currency.validate()?;
        let mut previous = None;
        for entry in &self.entries {
            if entry.quantity != 1 || previous.is_some_and(|item| item >= entry.item) {
                return Err(invalid_state(
                    "authored inventory must be sorted, unique, and quantity one",
                ));
            }
            if entry.equipped
                && !matches!(
                    entry.item,
                    EquipmentId::ChainMail | EquipmentId::LeatherArmor | EquipmentId::Shield
                )
            {
                return Err(invalid_state(
                    "only supported armor and shield use inventory equip state",
                ));
            }
            previous = Some(entry.item);
        }
        let carries_simple = self
            .entries
            .iter()
            .any(|entry| entry.item == EquipmentId::SimpleWeapons);
        if carries_simple != self.simple_weapon.is_some() {
            return Err(invalid_state(
                "simple weapon category and concrete weapon must agree",
            ));
        }
        if !self.weapon_is_carried(&self.held_weapon) {
            return Err(invalid_state(
                "held weapon is not present in carried equipment",
            ));
        }
        Ok(())
    }

    pub fn weapon_is_carried(&self, weapon: &WeaponChoice) -> bool {
        match weapon {
            WeaponChoice::Simple { kind } => {
                self.simple_weapon == Some(*kind)
                    && self
                        .entries
                        .iter()
                        .any(|entry| entry.item == EquipmentId::SimpleWeapons)
            }
            WeaponChoice::Longsword => self
                .entries
                .iter()
                .any(|entry| entry.item == EquipmentId::Longsword),
            WeaponChoice::LightCrossbow => self
                .entries
                .iter()
                .any(|entry| entry.item == EquipmentId::LightCrossbow),
        }
    }

    pub fn use_object_interaction_to_ready_weapon(
        &mut self,
        weapon: WeaponChoice,
        economy: &mut ActionEconomy,
    ) -> RulesMatrixResult<()> {
        self.validate()?;
        if !self.weapon_is_carried(&weapon) {
            return Err(invalid_intent(
                "weapon must be carried before it can be readied",
            ));
        }
        let mut next_economy = economy.clone();
        next_economy.spend(TurnResource::ObjectInteraction)?;
        self.held_weapon = weapon;
        *economy = next_economy;
        self.validate()
    }

    pub fn validate_readied_attack(&self, weapon: &WeaponChoice) -> RulesMatrixResult<()> {
        self.validate()?;
        if &self.held_weapon != weapon {
            return Err(invalid_intent("selected attack weapon is not readied"));
        }
        let shield_equipped = self
            .entries
            .iter()
            .any(|entry| entry.item == EquipmentId::Shield && entry.equipped);
        let needs_two_hands = matches!(weapon, WeaponChoice::LightCrossbow)
            || matches!(
                weapon,
                WeaponChoice::Simple {
                    kind: SimpleWeaponId::Greatclub
                }
            );
        if shield_equipped && needs_two_hands {
            return Err(invalid_intent(
                "two-handed attack is unavailable while a shield is equipped",
            ));
        }
        Ok(())
    }

    /// Changes armor state only at a trusted between-scene boundary. Combat
    /// armor donning/doffing durations are outside the shipped encounters.
    pub fn set_equipped_between_scenes(
        &mut self,
        item: EquipmentId,
        equipped: bool,
    ) -> RulesMatrixResult<()> {
        self.validate()?;
        if !matches!(
            item,
            EquipmentId::ChainMail | EquipmentId::LeatherArmor | EquipmentId::Shield
        ) {
            return Err(unsupported_mechanic("inventory.equip-item"));
        }
        let Some(index) = self.entries.iter().position(|entry| entry.item == item) else {
            return Err(invalid_intent(
                "item must be carried before it can be equipped",
            ));
        };
        if equipped && matches!(item, EquipmentId::ChainMail | EquipmentId::LeatherArmor) {
            for entry in &mut self.entries {
                if matches!(
                    entry.item,
                    EquipmentId::ChainMail | EquipmentId::LeatherArmor
                ) {
                    entry.equipped = false;
                }
            }
        }
        self.entries[index].equipped = equipped;
        self.validate()
    }

    /// Donning or doffing the supported shield during combat spends an action.
    pub fn use_action_to_change_shield(
        &mut self,
        equipped: bool,
        economy: &mut ActionEconomy,
    ) -> RulesMatrixResult<()> {
        self.validate()?;
        if !self
            .entries
            .iter()
            .any(|entry| entry.item == EquipmentId::Shield)
        {
            return Err(invalid_intent(
                "shield must be carried before changing its equip state",
            ));
        }
        let mut next_inventory = self.clone();
        let mut next_economy = economy.clone();
        next_economy.spend(TurnResource::Action)?;
        next_inventory.set_equipped_between_scenes(EquipmentId::Shield, equipped)?;
        *self = next_inventory;
        *economy = next_economy;
        Ok(())
    }

    /// No individually selectable consumable is present in Q04. Pack contents
    /// stay opaque until a future immutable pack names one with a typed effect.
    pub fn use_consumable(&mut self, requested_id: &str) -> RulesMatrixResult<()> {
        Err(unsupported_mechanic(requested_id))
    }
}

pub fn runtime_armor_class(
    sheet: &DerivedHeroSheet,
    inventory: &InventoryState,
) -> RulesMatrixResult<u8> {
    inventory.validate()?;
    let equipped_armor = inventory
        .entries
        .iter()
        .filter(|entry| {
            entry.equipped
                && matches!(
                    entry.item,
                    EquipmentId::ChainMail | EquipmentId::LeatherArmor
                )
        })
        .map(|entry| entry.item)
        .collect::<Vec<_>>();
    if equipped_armor.len() > 1 {
        return Err(invalid_state("only one supported armor can be equipped"));
    }
    let dexterity = i16::from(sheet.ability_scores.get(Ability::Dexterity).modifier());
    let mut armor_class = match equipped_armor.first() {
        Some(EquipmentId::ChainMail) => 16_i16,
        Some(EquipmentId::LeatherArmor) => 11 + dexterity,
        None => 10 + dexterity,
        Some(_) => unreachable!("equipped armor filter is closed"),
    };
    let wearing_armor = !equipped_armor.is_empty();
    if inventory
        .entries
        .iter()
        .any(|entry| entry.item == EquipmentId::Shield && entry.equipped)
    {
        armor_class += 2;
    }
    if wearing_armor
        && sheet.features.iter().any(|feature| {
            feature.feature == FeatureId::FightingStyleDefense
                && feature.availability == FeatureAvailability::Active
        })
    {
        armor_class += 1;
    }
    u8::try_from(armor_class).map_err(|_| RulesMatrixError::ArithmeticOverflow)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellcastingState {
    pub schema_version: u16,
    pub caster_id: String,
    pub spell_attack_bonus: i8,
    pub spell_save_dc: u8,
    pub cantrips: Vec<SpellId>,
    pub prepared: Vec<SpellId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellComponentAccess {
    pub verbal_available: bool,
    pub somatic_available: bool,
    pub material_focus_available: bool,
}

impl SpellComponentAccess {
    pub const fn available() -> Self {
        Self {
            verbal_available: true,
            somatic_available: true,
            material_focus_available: true,
        }
    }

    fn validate_for(self, spell: SpellId) -> RulesMatrixResult<()> {
        let needs_somatic = spell != SpellId::Light;
        let needs_material = matches!(spell, SpellId::Light | SpellId::Sleep);
        if !self.verbal_available
            || (needs_somatic && !self.somatic_available)
            || (needs_material && !self.material_focus_available)
        {
            return Err(invalid_intent(
                "required verbal, somatic, or material spell component is unavailable",
            ));
        }
        Ok(())
    }
}

impl SpellcastingState {
    pub fn from_derived_sheet(
        caster_id: impl Into<String>,
        sheet: &DerivedHeroSheet,
    ) -> RulesMatrixResult<Self> {
        let spellcasting = sheet
            .spellcasting
            .as_ref()
            .ok_or_else(|| unsupported_mechanic("feature.wizard-spellcasting"))?;
        let result = Self {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            caster_id: caster_id.into(),
            spell_attack_bonus: spellcasting.spell_attack_bonus,
            spell_save_dc: spellcasting.spell_save_dc,
            cantrips: spellcasting.cantrips.clone(),
            prepared: spellcasting.prepared.clone(),
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION {
            return Err(invalid_state(
                "spellcasting state schema version is unsupported",
            ));
        }
        require_id(&self.caster_id, "caster ID is invalid")?;
        if !(-3..=12).contains(&self.spell_attack_bonus)
            || !(5..=30).contains(&self.spell_save_dc)
            || self.cantrips.as_slice() != SpellId::CANTRIPS
            || self.prepared.is_empty()
            || self.prepared.len() > SpellId::LEVEL_ONE.len()
            || !self.prepared.windows(2).all(|pair| pair[0] < pair[1])
            || self
                .prepared
                .iter()
                .any(|spell| !SpellId::LEVEL_ONE.contains(spell))
        {
            return Err(invalid_state(
                "spellcasting attack, DC, cantrips, or prepared list is invalid",
            ));
        }
        Ok(())
    }

    fn can_cast(&self, spell: SpellId) -> bool {
        if spell.level() == 0 {
            self.cantrips.contains(&spell)
        } else {
            self.prepared.contains(&spell)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FireBoltTarget {
    pub target_id: String,
    pub distance_feet: u16,
    pub visible: bool,
    pub armor_class: u8,
    pub cover: Cover,
    pub threatening_hostile_within_five_feet: bool,
    pub damage_profile: DamageProfile,
    pub target_kind: FireBoltTargetKind,
    pub conditions: ConditionSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireBoltTargetKind {
    Creature,
    UnattendedFlammableObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightCarrierSave {
    pub carrier_id: String,
    pub dexterity_save_modifier: i8,
    pub roll_context: RollContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightTarget {
    pub object_id: String,
    pub distance_feet: u16,
    pub object_maximum_dimension_feet: u8,
    pub hostile_carrier: Option<LightCarrierSave>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MageHandOperation {
    ManipulateObject,
    OpenUnlockedDoorOrContainer,
    StowObject,
    RetrieveObject,
    PourContents,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MageHandTarget {
    pub hand_id: String,
    pub distance_feet: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MageHandState {
    pub schema_version: u16,
    pub hand_id: String,
    pub caster_id: String,
    pub distance_from_caster_feet: u16,
    pub remaining_rounds: u16,
}

impl MageHandState {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION
            || self.distance_from_caster_feet > 30
            || !self.distance_from_caster_feet.is_multiple_of(5)
            || !(1..=10).contains(&self.remaining_rounds)
        {
            return Err(invalid_state(
                "mage hand schema, range, or duration is invalid",
            ));
        }
        require_id(&self.hand_id, "mage hand ID is invalid")?;
        require_id(&self.caster_id, "mage hand caster ID is invalid")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MageHandControlTarget {
    pub object_id: String,
    pub hand_movement_feet: u16,
    pub resulting_distance_from_caster_feet: u16,
    pub object_weight_pounds: u8,
    pub is_magic_item: bool,
    pub operation: MageHandOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MageHandActionIntent {
    Control { target: MageHandControlTarget },
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MageHandActionEffect {
    Controlled {
        hand_id: String,
        object_id: String,
        operation: MageHandOperation,
        previous_distance_from_caster_feet: u16,
        hand_movement_feet: u16,
        resulting_distance_from_caster_feet: u16,
    },
    Dismissed {
        hand_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MageHandActionResolution {
    pub effect: MageHandActionEffect,
    pub resulting_hand: Option<MageHandState>,
    pub resulting_action_economy: ActionEconomy,
}

pub fn resolve_mage_hand_action(
    hand: &mut Option<MageHandState>,
    economy: &mut ActionEconomy,
    caster_conditions: &ConditionSet,
    intent: &MageHandActionIntent,
) -> RulesMatrixResult<MageHandActionResolution> {
    let current = hand
        .as_ref()
        .ok_or_else(|| invalid_intent("no active mage hand is available"))?;
    current.validate()?;
    caster_conditions.validate()?;
    if condition_effects(caster_conditions, RollSituation::AbilityCheck)?.actions_blocked {
        return Err(invalid_intent(
            "current conditions prevent controlling mage hand",
        ));
    }

    let mut next_economy = economy.clone();
    next_economy.spend(TurnResource::Action)?;
    let (effect, resulting_hand) = match intent {
        MageHandActionIntent::Control { target } => {
            require_id(&target.object_id, "mage hand object ID is invalid")?;
            if target.hand_movement_feet > 30
                || !target.hand_movement_feet.is_multiple_of(5)
                || target.resulting_distance_from_caster_feet > 30
                || !target.resulting_distance_from_caster_feet.is_multiple_of(5)
                || target.object_weight_pounds > 10
                || target.is_magic_item
                || target.hand_movement_feet
                    < current
                        .distance_from_caster_feet
                        .abs_diff(target.resulting_distance_from_caster_feet)
            {
                return Err(invalid_intent(
                    "mage hand control exceeds its movement, range, weight, or object limits",
                ));
            }
            let mut next_hand = current.clone();
            let previous_distance = next_hand.distance_from_caster_feet;
            next_hand.distance_from_caster_feet = target.resulting_distance_from_caster_feet;
            next_hand.validate()?;
            (
                MageHandActionEffect::Controlled {
                    hand_id: next_hand.hand_id.clone(),
                    object_id: target.object_id.clone(),
                    operation: target.operation,
                    previous_distance_from_caster_feet: previous_distance,
                    hand_movement_feet: target.hand_movement_feet,
                    resulting_distance_from_caster_feet: target.resulting_distance_from_caster_feet,
                },
                Some(next_hand),
            )
        }
        MageHandActionIntent::Dismiss => (
            MageHandActionEffect::Dismissed {
                hand_id: current.hand_id.clone(),
            },
            None,
        ),
    };
    *hand = resulting_hand.clone();
    *economy = next_economy.clone();
    Ok(MageHandActionResolution {
        effect,
        resulting_hand,
        resulting_action_economy: next_economy,
    })
}

/// Advances the one-minute duration by one round and removes the hand at zero.
/// Returns `true` only when this transition expires the hand.
pub fn advance_mage_hand_duration(hand: &mut Option<MageHandState>) -> RulesMatrixResult<bool> {
    let mut next = hand
        .as_ref()
        .ok_or_else(|| invalid_intent("no active mage hand is available"))?
        .clone();
    next.validate()?;
    if next.remaining_rounds == 1 {
        *hand = None;
        return Ok(true);
    }
    next.remaining_rounds -= 1;
    next.validate()?;
    *hand = Some(next);
    Ok(false)
}

/// Applies a trusted distance update after either the caster or hand moves.
/// Crossing thirty feet ends the spell instead of retaining an invalid state.
pub fn reconcile_mage_hand_distance(
    hand: &mut Option<MageHandState>,
    distance_from_caster_feet: u16,
) -> RulesMatrixResult<bool> {
    let mut next = hand
        .as_ref()
        .ok_or_else(|| invalid_intent("no active mage hand is available"))?
        .clone();
    next.validate()?;
    if !distance_from_caster_feet.is_multiple_of(5) {
        return Err(invalid_intent(
            "mage hand distance must use five-foot increments",
        ));
    }
    if distance_from_caster_feet > 30 {
        *hand = None;
        return Ok(true);
    }
    next.distance_from_caster_feet = distance_from_caster_feet;
    next.validate()?;
    *hand = Some(next);
    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicMissileTarget {
    pub target_id: String,
    pub distance_feet: u16,
    pub visible: bool,
    pub shielded: bool,
    pub damage_profile: DamageProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ShieldTrigger {
    AttackHit {
        natural_roll: u8,
        attack_total: i16,
        armor_class: u8,
    },
    MagicMissile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SleepCandidate {
    pub target_id: String,
    pub distance_from_point_feet: u16,
    pub current_hit_points: u16,
    pub already_unconscious: bool,
    pub immune_to_magical_sleep: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "spell", rename_all = "snake_case", deny_unknown_fields)]
pub enum SupportedSpellIntent {
    FireBolt {
        target: FireBoltTarget,
        roll_context: RollContext,
    },
    Light {
        target: LightTarget,
    },
    MageHand {
        target: MageHandTarget,
    },
    MagicMissile {
        darts: Box<[MagicMissileTarget; 3]>,
    },
    Shield {
        trigger: ShieldTrigger,
    },
    Sleep {
        center_distance_feet: u16,
        candidates: Vec<SleepCandidate>,
    },
}

impl SupportedSpellIntent {
    pub const fn spell(&self) -> SpellId {
        match self {
            Self::FireBolt { .. } => SpellId::FireBolt,
            Self::Light { .. } => SpellId::Light,
            Self::MageHand { .. } => SpellId::MageHand,
            Self::MagicMissile { .. } => SpellId::MagicMissile,
            Self::Shield { .. } => SpellId::Shield,
            Self::Sleep { .. } => SpellId::Sleep,
        }
    }

    fn validate(&self) -> RulesMatrixResult<()> {
        match self {
            Self::FireBolt { target, .. } => {
                require_id(&target.target_id, "fire bolt target ID is invalid")?;
                target.damage_profile.validate()?;
                target.conditions.validate()?;
                if !target.visible
                    || target.distance_feet > 120
                    || !target.distance_feet.is_multiple_of(5)
                    || target.armor_class == 0
                {
                    return Err(invalid_intent(
                        "fire bolt requires a visible in-range target with AC",
                    ));
                }
            }
            Self::Light { target } => {
                require_id(&target.object_id, "light object ID is invalid")?;
                if target.distance_feet > 5
                    || !target.distance_feet.is_multiple_of(5)
                    || target.object_maximum_dimension_feet == 0
                    || target.object_maximum_dimension_feet > 10
                {
                    return Err(invalid_intent(
                        "light requires a touched object no larger than ten feet",
                    ));
                }
                if let Some(carrier) = &target.hostile_carrier {
                    require_id(&carrier.carrier_id, "light carrier ID is invalid")?;
                    if !(-5..=15).contains(&carrier.dexterity_save_modifier) {
                        return Err(invalid_intent("light carrier save modifier is invalid"));
                    }
                }
            }
            Self::MageHand { target } => {
                require_id(&target.hand_id, "mage hand ID is invalid")?;
                if target.distance_feet > 30 || !target.distance_feet.is_multiple_of(5) {
                    return Err(invalid_intent("mage hand creation point is out of range"));
                }
            }
            Self::MagicMissile { darts } => {
                for target in darts.as_ref() {
                    require_id(&target.target_id, "magic missile target ID is invalid")?;
                    target.damage_profile.validate()?;
                    if !target.visible
                        || target.distance_feet > 120
                        || !target.distance_feet.is_multiple_of(5)
                    {
                        return Err(invalid_intent(
                            "magic missile requires visible in-range targets",
                        ));
                    }
                }
            }
            Self::Shield { trigger } => {
                if let ShieldTrigger::AttackHit {
                    natural_roll,
                    attack_total,
                    armor_class,
                } = trigger
                    && (!(1..=30).contains(armor_class)
                        || !(1..=20).contains(natural_roll)
                        || *natural_roll == 1
                        || (*natural_roll != 20 && *attack_total < i16::from(*armor_class)))
                {
                    return Err(invalid_intent(
                        "shield attack trigger must describe an actual hit",
                    ));
                }
            }
            Self::Sleep {
                center_distance_feet,
                candidates,
            } => {
                if *center_distance_feet > 90
                    || !center_distance_feet.is_multiple_of(5)
                    || candidates.len() > 32
                {
                    return Err(invalid_intent("sleep point or candidate count is invalid"));
                }
                let mut ids = BTreeSet::new();
                for target in candidates {
                    require_id(&target.target_id, "sleep target ID is invalid")?;
                    if !ids.insert(&target.target_id)
                        || target.distance_from_point_feet > 20
                        || !target.distance_from_point_feet.is_multiple_of(5)
                        || target.current_hit_points == 0
                    {
                        return Err(invalid_intent(
                            "sleep candidates must be unique, living, and in the area",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellAttackResolution {
    pub roll: D20Roll,
    pub spell_attack_bonus: i8,
    pub total: i16,
    pub target_armor_class: u8,
    pub outcome: D20TestOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectSaveResolution {
    pub roll: D20Roll,
    pub modifier: i8,
    pub total: i16,
    pub difficulty_class: u8,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpellEffect {
    Damage {
        target_id: String,
        damage_type: DamageType,
        rolled_damage: u16,
        interaction: DamageInteraction,
        effective_damage: u16,
    },
    IgniteUnattendedFlammableObject {
        target_id: String,
    },
    IlluminateObject {
        caster_id: String,
        object_id: String,
        bright_light_feet: u16,
        dim_light_feet: u16,
        duration_rounds: u16,
    },
    CreateMageHand {
        hand: MageHandState,
    },
    MagicMissileNegated {
        target_id: String,
    },
    ShieldWard {
        armor_class_bonus: u8,
        negates_triggering_attack: bool,
        immune_to_magic_missile: bool,
        until_start_of_caster_turn: bool,
    },
    ApplyCondition {
        target_id: String,
        condition: ActiveCondition,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellCastResolution {
    pub schema_version: u16,
    pub spell: SpellId,
    pub attack: Option<SpellAttackResolution>,
    pub saving_throw: Option<DirectSaveResolution>,
    pub damage_rolls: Vec<DamageDiceResolution>,
    pub sleep_hit_point_pool: Option<u16>,
    pub effects: Vec<SpellEffect>,
    pub resulting_resources: RuntimeResources,
    pub resulting_action_economy: ActionEconomy,
}

fn spend_spell_action_and_slot(
    spell: SpellId,
    resources: &mut RuntimeResources,
    economy: &mut ActionEconomy,
) -> RulesMatrixResult<()> {
    let resource = if spell == SpellId::Shield {
        TurnResource::Reaction
    } else {
        TurnResource::Action
    };
    economy.spend(resource)?;
    if spell.level() == 1 {
        resources.spend_spell_slot()?;
    }
    Ok(())
}

pub fn resolve_supported_spell(
    spellcasting: &SpellcastingState,
    resources: &mut RuntimeResources,
    economy: &mut ActionEconomy,
    caster_conditions: &ConditionSet,
    components: SpellComponentAccess,
    intent: &SupportedSpellIntent,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<SpellCastResolution> {
    spellcasting.validate()?;
    resources.validate()?;
    caster_conditions.validate()?;
    intent.validate()?;
    if resources.class != HeroClass::Wizard {
        return Err(unsupported_mechanic("action.cast-supported-spell"));
    }
    let spell = intent.spell();
    components.validate_for(spell)?;
    let caster_effects = condition_effects(
        caster_conditions,
        if spell == SpellId::FireBolt {
            RollSituation::OwnAttack
        } else {
            RollSituation::AbilityCheck
        },
    )?;
    if caster_effects.actions_blocked
        || (spell == SpellId::Shield && caster_effects.reactions_blocked)
    {
        return Err(invalid_intent(
            "current conditions prevent this spell action or reaction",
        ));
    }
    if !spellcasting.can_cast(spell) {
        return Err(RulesMatrixError::Unsupported(UnsupportedMechanic {
            schema_version: HERO_UNSUPPORTED_SCHEMA_VERSION,
            code: UnsupportedMechanicCode::NotAvailableForHero,
            requested_id: spell.mechanic_id().to_owned(),
            alternatives: vec![AuthoredAlternative {
                action: ActionCapability::Attack,
                label: "Make a supported weapon attack".to_owned(),
            }],
        }));
    }
    if spell.level() == 1 && !resources.has_spell_slot() {
        return Err(invalid_intent("no level-one spell slot remains"));
    }
    let mut next_resources = resources.clone();
    let mut next_economy = economy.clone();
    spend_spell_action_and_slot(spell, &mut next_resources, &mut next_economy)?;

    let mut attack = None;
    let mut saving_throw = None;
    let mut damage_rolls = Vec::new();
    let mut sleep_hit_point_pool = None;
    let mut effects = Vec::new();
    match intent {
        SupportedSpellIntent::FireBolt {
            target,
            roll_context,
        } => {
            let target_effects = condition_effects(
                &target.conditions,
                RollSituation::IncomingAttack {
                    attacker_distance_feet: target.distance_feet,
                },
            )?;
            let range_context = RollContext {
                advantage_sources: 0,
                disadvantage_sources: u8::from(target.threatening_hostile_within_five_feet),
            };
            let roll_context = combine_roll_context(
                combine_roll_context(*roll_context, range_context),
                combine_roll_context(caster_effects.roll_context, target_effects.roll_context),
            );
            let roll = resolve_d20(dice, roll_context)?;
            let target_armor_class = target
                .armor_class
                .checked_add(target.cover.armor_class_bonus())
                .ok_or(RulesMatrixError::ArithmeticOverflow)?;
            let total = i16::from(roll.selected) + i16::from(spellcasting.spell_attack_bonus);
            let outcome = match roll.selected {
                20 => D20TestOutcome::CriticalHit,
                1 => D20TestOutcome::AutomaticMiss,
                _ if total >= i16::from(target_armor_class) => D20TestOutcome::Success,
                _ => D20TestOutcome::Failure,
            };
            attack = Some(SpellAttackResolution {
                roll,
                spell_attack_bonus: spellcasting.spell_attack_bonus,
                total,
                target_armor_class,
                outcome,
            });
            if outcome.succeeds() {
                let critical = outcome == D20TestOutcome::CriticalHit
                    || target_effects.incoming_hit_is_critical;
                let damage = resolve_damage_dice(1, 10, 0, critical, dice)?;
                let interaction = target.damage_profile.interaction(DamageType::Fire)?;
                let effective_damage = interacted_damage(damage.total, interaction)?;
                effects.push(SpellEffect::Damage {
                    target_id: target.target_id.clone(),
                    damage_type: DamageType::Fire,
                    rolled_damage: damage.total,
                    interaction,
                    effective_damage,
                });
                if target.target_kind == FireBoltTargetKind::UnattendedFlammableObject {
                    effects.push(SpellEffect::IgniteUnattendedFlammableObject {
                        target_id: target.target_id.clone(),
                    });
                }
                damage_rolls.push(damage);
            }
        }
        SupportedSpellIntent::Light { target } => {
            let save_succeeded = if let Some(carrier) = &target.hostile_carrier {
                let roll = resolve_d20(dice, carrier.roll_context)?;
                let total = i16::from(roll.selected) + i16::from(carrier.dexterity_save_modifier);
                let success = total >= i16::from(spellcasting.spell_save_dc);
                saving_throw = Some(DirectSaveResolution {
                    roll,
                    modifier: carrier.dexterity_save_modifier,
                    total,
                    difficulty_class: spellcasting.spell_save_dc,
                    success,
                });
                success
            } else {
                false
            };
            if !save_succeeded {
                effects.push(SpellEffect::IlluminateObject {
                    caster_id: spellcasting.caster_id.clone(),
                    object_id: target.object_id.clone(),
                    bright_light_feet: 20,
                    dim_light_feet: 20,
                    duration_rounds: 600,
                });
            }
        }
        SupportedSpellIntent::MageHand { target } => {
            effects.push(SpellEffect::CreateMageHand {
                hand: MageHandState {
                    schema_version: RULES_MATRIX_SCHEMA_VERSION,
                    hand_id: target.hand_id.clone(),
                    caster_id: spellcasting.caster_id.clone(),
                    distance_from_caster_feet: target.distance_feet,
                    remaining_rounds: 10,
                },
            });
        }
        SupportedSpellIntent::MagicMissile { darts } => {
            for target in darts.as_ref() {
                let damage = resolve_damage_dice(1, 4, 1, false, dice)?;
                if target.shielded {
                    effects.push(SpellEffect::MagicMissileNegated {
                        target_id: target.target_id.clone(),
                    });
                } else {
                    let interaction = target.damage_profile.interaction(DamageType::Force)?;
                    let effective_damage = interacted_damage(damage.total, interaction)?;
                    effects.push(SpellEffect::Damage {
                        target_id: target.target_id.clone(),
                        damage_type: DamageType::Force,
                        rolled_damage: damage.total,
                        interaction,
                        effective_damage,
                    });
                }
                damage_rolls.push(damage);
            }
        }
        SupportedSpellIntent::Shield { trigger } => {
            let negates_triggering_attack = match trigger {
                ShieldTrigger::AttackHit {
                    natural_roll,
                    attack_total,
                    armor_class,
                } => {
                    *natural_roll != 20 && *attack_total < i16::from(armor_class.saturating_add(5))
                }
                ShieldTrigger::MagicMissile => true,
            };
            effects.push(SpellEffect::ShieldWard {
                armor_class_bonus: 5,
                negates_triggering_attack,
                immune_to_magic_missile: true,
                until_start_of_caster_turn: true,
            });
        }
        SupportedSpellIntent::Sleep { candidates, .. } => {
            let pool_roll = resolve_damage_dice(5, 8, 0, false, dice)?;
            let mut remaining = pool_roll.total;
            let mut ordered = candidates
                .iter()
                .filter(|candidate| {
                    !candidate.already_unconscious && !candidate.immune_to_magical_sleep
                })
                .collect::<Vec<_>>();
            ordered.sort_by_key(|candidate| (candidate.current_hit_points, &candidate.target_id));
            for target in ordered {
                if target.current_hit_points > remaining {
                    break;
                }
                remaining -= target.current_hit_points;
                effects.push(SpellEffect::ApplyCondition {
                    target_id: target.target_id.clone(),
                    condition: ActiveCondition {
                        condition: ConditionId::Unconscious,
                        source: ConditionSource::Spell {
                            spell: SpellId::Sleep,
                            caster_id: spellcasting.caster_id.clone(),
                        },
                        duration: EffectDuration::UntilDamagedOrAwakened {
                            remaining_rounds: 10,
                            actor_id: target.target_id.clone(),
                        },
                    },
                });
                effects.push(SpellEffect::ApplyCondition {
                    target_id: target.target_id.clone(),
                    condition: ActiveCondition {
                        condition: ConditionId::Prone,
                        source: ConditionSource::Spell {
                            spell: SpellId::Sleep,
                            caster_id: spellcasting.caster_id.clone(),
                        },
                        duration: EffectDuration::Permanent,
                    },
                });
            }
            sleep_hit_point_pool = Some(pool_roll.total);
            damage_rolls.push(pool_roll);
        }
    }

    next_resources.validate()?;
    *resources = next_resources.clone();
    *economy = next_economy.clone();
    Ok(SpellCastResolution {
        schema_version: RULES_MATRIX_SCHEMA_VERSION,
        spell,
        attack,
        saving_throw,
        damage_rolls,
        sleep_hit_point_pool,
        effects,
        resulting_resources: next_resources,
        resulting_action_economy: next_economy,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighStakesKind {
    None,
    Death,
    PermanentLoss,
    IrreversibleStoryCommitment,
    ConsentSensitiveInspiration,
    NonRenewableResource,
}

impl HighStakesKind {
    pub const fn requires_confirmation(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedDifficulty {
    pub band: CheckDifficulty,
    pub difficulty_class: u8,
    pub stakes: HighStakesKind,
    pub player_confirmed: bool,
}

pub fn map_trusted_difficulty(
    band: CheckDifficulty,
    stakes: HighStakesKind,
    player_confirmed: bool,
) -> RulesMatrixResult<TrustedDifficulty> {
    if stakes.requires_confirmation() && !player_confirmed {
        return Err(invalid_intent(
            "high-stakes check requires player confirmation before rolling",
        ));
    }
    let difficulty_class =
        u8::try_from(band.baseline_dc()).expect("closed difficulty bands fit u8");
    Ok(TrustedDifficulty {
        band,
        difficulty_class,
        stakes,
        player_confirmed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedCheckRequest {
    pub schema_version: u16,
    pub ability: Ability,
    pub skill: Option<SkillId>,
    pub proficiency: Proficiency,
    pub difficulty: CheckDifficulty,
    pub stakes: HighStakesKind,
    pub player_confirmed: bool,
    pub roll_context: RollContext,
    #[serde(default)]
    pub situational_modifiers: Vec<SituationalModifier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedCheckResolution {
    pub difficulty: TrustedDifficulty,
    pub result: D20TestResolution,
}

impl TrustedCheckResolution {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        let expected_difficulty = map_trusted_difficulty(
            self.difficulty.band,
            self.difficulty.stakes,
            self.difficulty.player_confirmed,
        )?;
        if self.difficulty != expected_difficulty
            || self.result.schema_version != RULES_MATRIX_SCHEMA_VERSION
            || self.result.target_number != self.difficulty.difficulty_class
            || self.result.cover_bonus != 0
            || !matches!(
                self.result.outcome,
                D20TestOutcome::Success | D20TestOutcome::Failure
            )
            || !(-5..=10).contains(&self.result.ability_modifier)
            || !matches!(self.result.proficiency_modifier, 0 | 2..=6 | 8 | 10 | 12)
        {
            return Err(invalid_state("trusted check resolution is inconsistent"));
        }
        self.result.roll.validate()?;
        SituationalModifier::validate_all(&self.result.situational_modifiers)?;
        let situational_total =
            self.result
                .situational_modifiers
                .iter()
                .try_fold(0_i16, |total, modifier| {
                    total
                        .checked_add(i16::from(modifier.value))
                        .ok_or(RulesMatrixError::ArithmeticOverflow)
                })?;
        let total = i16::from(self.result.roll.selected)
            .checked_add(i16::from(self.result.ability_modifier))
            .and_then(|value| value.checked_add(i16::from(self.result.proficiency_modifier)))
            .and_then(|value| value.checked_add(situational_total))
            .ok_or(RulesMatrixError::ArithmeticOverflow)?;
        let expected_outcome = if total >= i16::from(self.result.target_number) {
            D20TestOutcome::Success
        } else {
            D20TestOutcome::Failure
        };
        if self.result.situational_total != situational_total
            || self.result.total != total
            || self.result.outcome != expected_outcome
        {
            return Err(invalid_state("trusted check totals are inconsistent"));
        }
        Ok(())
    }
}

pub fn resolve_trusted_check(
    ability_scores: &AbilityScores,
    level: Level,
    request: &TrustedCheckRequest,
    dice: &mut impl DiceSource,
) -> RulesMatrixResult<TrustedCheckResolution> {
    if request.schema_version != RULES_MATRIX_SCHEMA_VERSION {
        return Err(invalid_intent(
            "trusted check schema version is unsupported",
        ));
    }
    if request
        .skill
        .is_some_and(|skill| skill.ability() != request.ability)
    {
        return Err(invalid_intent("skill and ability do not match"));
    }
    let difficulty =
        map_trusted_difficulty(request.difficulty, request.stakes, request.player_confirmed)?;
    let result = resolve_d20_test(
        ability_scores,
        level,
        &D20TestRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: request.ability,
            proficiency: request.proficiency,
            roll_context: request.roll_context,
            situational_modifiers: request.situational_modifiers.clone(),
            target: D20Target::AbilityCheck {
                difficulty_class: difficulty.difficulty_class,
            },
        },
        dice,
    )?;
    Ok(TrustedCheckResolution { difficulty, result })
}

pub fn passive_skill_value(
    sheet: &DerivedHeroSheet,
    skill: SkillId,
    situational_modifier: i8,
) -> RulesMatrixResult<i16> {
    if !(-30..=30).contains(&situational_modifier) {
        return Err(invalid_intent(
            "passive situational modifier is outside -30 through 30",
        ));
    }
    let skill_modifier = sheet
        .skills
        .iter()
        .find(|summary| summary.skill == skill)
        .ok_or_else(|| invalid_state("derived sheet is missing a skill summary"))?
        .modifier;
    Ok(10 + i16::from(skill_modifier) + i16::from(situational_modifier))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveProgress {
    pub objective_id: String,
    pub progress: u8,
    pub target: u8,
    pub status: ProgressStatus,
}

impl ObjectiveProgress {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        require_id(&self.objective_id, "objective ID is invalid")?;
        if self.target == 0 || self.target > MAX_CLOCK_SEGMENTS || self.progress > self.target {
            return Err(invalid_state("objective progress bounds are invalid"));
        }
        if (self.progress == self.target) != (self.status == ProgressStatus::Completed) {
            return Err(invalid_state("objective completion and progress disagree"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockKind {
    Progress,
    Threat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneClock {
    pub clock_id: String,
    pub kind: ClockKind,
    pub filled: u8,
    pub segments: u8,
}

impl SceneClock {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        require_id(&self.clock_id, "clock ID is invalid")?;
        if self.segments == 0 || self.segments > MAX_CLOCK_SEGMENTS || self.filled > self.segments {
            return Err(invalid_state("clock bounds are invalid"));
        }
        Ok(())
    }

    pub fn advance(&mut self, segments: u8) -> RulesMatrixResult<u8> {
        self.validate()?;
        if segments == 0 {
            return Err(invalid_intent("clock advance must be positive"));
        }
        let applied = segments.min(self.segments - self.filled);
        self.filled += applied;
        Ok(applied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NpcAttitude {
    Hostile,
    Indifferent,
    Friendly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NpcSocialState {
    pub npc_id: String,
    pub attitude: NpcAttitude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttitudeShift {
    OneStepWorse,
    NoChange,
    OneStepBetter,
}

impl NpcSocialState {
    pub fn apply_shift(&mut self, shift: AttitudeShift) -> RulesMatrixResult<NpcAttitude> {
        require_id(&self.npc_id, "NPC ID is invalid")?;
        self.attitude = match (self.attitude, shift) {
            (NpcAttitude::Hostile, AttitudeShift::OneStepBetter) => NpcAttitude::Indifferent,
            (NpcAttitude::Indifferent, AttitudeShift::OneStepBetter) => NpcAttitude::Friendly,
            (NpcAttitude::Friendly, AttitudeShift::OneStepWorse) => NpcAttitude::Indifferent,
            (NpcAttitude::Indifferent, AttitudeShift::OneStepWorse) => NpcAttitude::Hostile,
            (attitude, _) => attitude,
        };
        Ok(self.attitude)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationSocialState {
    pub schema_version: u16,
    pub turn: u32,
    pub objectives: Vec<ObjectiveProgress>,
    pub clocks: Vec<SceneClock>,
    pub npcs: Vec<NpcSocialState>,
}

impl ExplorationSocialState {
    pub fn validate(&self) -> RulesMatrixResult<()> {
        if self.schema_version != RULES_MATRIX_SCHEMA_VERSION || self.turn == 0 {
            return Err(invalid_state("exploration state schema or turn is invalid"));
        }
        let mut ids = BTreeSet::new();
        for objective in &self.objectives {
            objective.validate()?;
            if !ids.insert(format!("objective:{}", objective.objective_id)) {
                return Err(invalid_state("duplicate objective ID"));
            }
        }
        for clock in &self.clocks {
            clock.validate()?;
            if !ids.insert(format!("clock:{}", clock.clock_id)) {
                return Err(invalid_state("duplicate clock ID"));
            }
        }
        for npc in &self.npcs {
            require_id(&npc.npc_id, "NPC ID is invalid")?;
            if !ids.insert(format!("npc:{}", npc.npc_id)) {
                return Err(invalid_state("duplicate NPC ID"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExplorationSocialCommand {
    AdvanceObjective {
        objective_id: String,
        amount: u8,
    },
    FailObjective {
        objective_id: String,
    },
    AdvanceClock {
        clock_id: String,
        amount: u8,
    },
    ShiftNpcAttitude {
        npc_id: String,
        shift: AttitudeShift,
    },
    EndTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExplorationSocialFact {
    ObjectiveAdvanced {
        objective_id: String,
        applied: u8,
        completed: bool,
    },
    ObjectiveFailed {
        objective_id: String,
    },
    ClockAdvanced {
        clock_id: String,
        applied: u8,
        filled: bool,
    },
    NpcAttitudeChanged {
        npc_id: String,
        attitude: NpcAttitude,
    },
    TurnEnded {
        next_turn: u32,
    },
}

pub fn apply_exploration_social_command(
    state: &mut ExplorationSocialState,
    command: &ExplorationSocialCommand,
) -> RulesMatrixResult<ExplorationSocialFact> {
    state.validate()?;
    let mut next = state.clone();
    let fact = match command {
        ExplorationSocialCommand::AdvanceObjective {
            objective_id,
            amount,
        } => {
            require_id(objective_id, "objective command ID is invalid")?;
            if *amount == 0 {
                return Err(invalid_intent("objective advance must be positive"));
            }
            let objective = next
                .objectives
                .iter_mut()
                .find(|objective| objective.objective_id == *objective_id)
                .ok_or_else(|| invalid_intent("objective is not in the current scene"))?;
            if objective.status != ProgressStatus::Active {
                return Err(invalid_intent("only an active objective can advance"));
            }
            let applied = (*amount).min(objective.target - objective.progress);
            objective.progress += applied;
            if objective.progress == objective.target {
                objective.status = ProgressStatus::Completed;
            }
            ExplorationSocialFact::ObjectiveAdvanced {
                objective_id: objective_id.clone(),
                applied,
                completed: objective.status == ProgressStatus::Completed,
            }
        }
        ExplorationSocialCommand::FailObjective { objective_id } => {
            require_id(objective_id, "objective command ID is invalid")?;
            let objective = next
                .objectives
                .iter_mut()
                .find(|objective| objective.objective_id == *objective_id)
                .ok_or_else(|| invalid_intent("objective is not in the current scene"))?;
            if objective.status != ProgressStatus::Active {
                return Err(invalid_intent("only an active objective can fail"));
            }
            objective.status = ProgressStatus::Failed;
            ExplorationSocialFact::ObjectiveFailed {
                objective_id: objective_id.clone(),
            }
        }
        ExplorationSocialCommand::AdvanceClock { clock_id, amount } => {
            require_id(clock_id, "clock command ID is invalid")?;
            let clock = next
                .clocks
                .iter_mut()
                .find(|clock| clock.clock_id == *clock_id)
                .ok_or_else(|| invalid_intent("clock is not in the current scene"))?;
            let applied = clock.advance(*amount)?;
            ExplorationSocialFact::ClockAdvanced {
                clock_id: clock_id.clone(),
                applied,
                filled: clock.filled == clock.segments,
            }
        }
        ExplorationSocialCommand::ShiftNpcAttitude { npc_id, shift } => {
            require_id(npc_id, "NPC command ID is invalid")?;
            let npc = next
                .npcs
                .iter_mut()
                .find(|npc| npc.npc_id == *npc_id)
                .ok_or_else(|| invalid_intent("NPC is not in the current scene"))?;
            let attitude = npc.apply_shift(*shift)?;
            ExplorationSocialFact::NpcAttitudeChanged {
                npc_id: npc_id.clone(),
                attitude,
            }
        }
        ExplorationSocialCommand::EndTurn => {
            next.turn = next
                .turn
                .checked_add(1)
                .ok_or(RulesMatrixError::ArithmeticOverflow)?;
            ExplorationSocialFact::TurnEnded {
                next_turn: next.turn,
            }
        }
    };
    next.validate()?;
    *state = next;
    Ok(fact)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::hero::{
        AncestryId, BackgroundId, BackgroundSelection, ClassSelection, EquipmentSelection,
        FightingStyleId, HeroCharacter, HeroChoices, HeroConceptId, HeroPins, HeroPresentation,
        StandardArrayAssignment, ThemeId, WizardSpellSelection,
    };

    struct SequenceDice {
        values: VecDeque<u16>,
    }

    impl SequenceDice {
        fn new(values: impl IntoIterator<Item = u16>) -> Self {
            Self {
                values: values.into_iter().collect(),
            }
        }
    }

    impl DiceSource for SequenceDice {
        fn roll(&mut self, _sides: u16) -> u16 {
            self.values.pop_front().expect("test die value")
        }
    }

    fn presentation(name: &str) -> HeroPresentation {
        HeroPresentation {
            name: name.to_owned(),
            pronouns: "they/them".to_owned(),
            appearance: "A practical coat.".to_owned(),
            ideal: "Protect the ward.".to_owned(),
            bond: "The canal community.".to_owned(),
            flaw: "Too stubborn.".to_owned(),
            tone_limits: vec!["No graphic horror".to_owned()],
        }
    }

    fn fighter() -> HeroCharacter {
        HeroCharacter::create(
            "fighter-1".to_owned(),
            "campaign-1".to_owned(),
            "owner-1".to_owned(),
            HeroChoices {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
                concept: HeroConceptId::CanalGuardian,
                ancestry: AncestryId::Human,
                class: ClassSelection::Fighter {
                    fighting_style: FightingStyleId::Defense,
                },
                ability_assignment: StandardArrayAssignment {
                    strength: 15,
                    dexterity: 14,
                    constitution: 13,
                    intelligence: 12,
                    wisdom: 10,
                    charisma: 8,
                },
                background: BackgroundSelection {
                    background: BackgroundId::Soldier,
                    class_skills: vec![SkillId::Acrobatics, SkillId::AnimalHandling],
                },
                equipment: EquipmentSelection {
                    carried: vec![
                        EquipmentId::Longsword,
                        EquipmentId::LightCrossbow,
                        EquipmentId::Shield,
                        EquipmentId::ChainMail,
                        EquipmentId::ExplorersPack,
                    ],
                    simple_weapon: None,
                    equipped_armor: Some(EquipmentId::ChainMail),
                    shield_equipped: true,
                },
                wizard_spells: None,
                presentation: presentation("Mara Vale"),
            },
        )
        .unwrap()
    }

    fn wizard() -> HeroCharacter {
        HeroCharacter::create(
            "wizard-1".to_owned(),
            "campaign-1".to_owned(),
            "owner-1".to_owned(),
            HeroChoices {
                pins: HeroPins::mvp(ThemeId::EmberlineArchive),
                concept: HeroConceptId::ArchiveSeeker,
                ancestry: AncestryId::Human,
                class: ClassSelection::Wizard,
                ability_assignment: StandardArrayAssignment {
                    strength: 8,
                    dexterity: 14,
                    constitution: 13,
                    intelligence: 15,
                    wisdom: 12,
                    charisma: 10,
                },
                background: BackgroundSelection {
                    background: BackgroundId::Sage,
                    class_skills: vec![SkillId::Insight, SkillId::Investigation],
                },
                equipment: EquipmentSelection {
                    carried: vec![
                        EquipmentId::SimpleWeapons,
                        EquipmentId::ScholarsPack,
                        EquipmentId::Spellbook,
                        EquipmentId::ArcaneFocus,
                    ],
                    simple_weapon: Some(SimpleWeaponId::Quarterstaff),
                    equipped_armor: None,
                    shield_equipped: false,
                },
                wizard_spells: Some(WizardSpellSelection {
                    cantrips: SpellId::CANTRIPS.to_vec(),
                    spellbook: SpellId::LEVEL_ONE.to_vec(),
                    prepared: SpellId::LEVEL_ONE.to_vec(),
                }),
                presentation: presentation("Eli Ward"),
            },
        )
        .unwrap()
    }

    fn permanent(condition: ConditionId, source: &str) -> ActiveCondition {
        ActiveCondition {
            condition,
            source: ConditionSource::Mechanic {
                mechanic_id: source.to_owned(),
            },
            duration: EffectDuration::Permanent,
        }
    }

    fn spell_fixture() -> (SpellcastingState, RuntimeResources, ActionEconomy) {
        let wizard = wizard();
        (
            SpellcastingState::from_derived_sheet("wizard-1", &wizard.sheet).unwrap(),
            RuntimeResources::from_derived_sheet(HeroClass::Wizard, &wizard.sheet).unwrap(),
            ActionEconomy::new(30),
        )
    }

    fn resolve_spell_fixture(
        intent: &SupportedSpellIntent,
        components: SpellComponentAccess,
        values: impl IntoIterator<Item = u16>,
    ) -> (
        RulesMatrixResult<SpellCastResolution>,
        RuntimeResources,
        ActionEconomy,
        SequenceDice,
    ) {
        let (casting, mut resources, mut economy) = spell_fixture();
        let mut dice = SequenceDice::new(values);
        let resolution = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            components,
            intent,
            &mut dice,
        );
        (resolution, resources, economy, dice)
    }

    #[test]
    fn d20_tests_cancel_opposed_sources_and_apply_cover_only_where_relevant() {
        let scores = AbilityScores::new(16, 14, 12, 10, 8, 6).unwrap();
        let request = D20TestRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: Ability::Dexterity,
            proficiency: Proficiency::Proficient,
            roll_context: RollContext {
                advantage_sources: 2,
                disadvantage_sources: 1,
            },
            situational_modifiers: vec![SituationalModifier {
                source_id: "covering-fire".to_owned(),
                value: 1,
            }],
            target: D20Target::SavingThrow {
                difficulty_class: 15,
                cover: Cover::Half,
            },
        };
        let mut dice = SequenceDice::new([10]);
        let result =
            resolve_d20_test(&scores, Level::new(1).unwrap(), &request, &mut dice).unwrap();
        assert_eq!(result.roll.second, None);
        assert_eq!(result.target_number, 17);
        assert_eq!(result.total, 15); // 10 + Dex 2 + proficiency 2 + situational 1
        assert_eq!(result.outcome, D20TestOutcome::Failure);

        let mut check = request;
        check.ability = Ability::Wisdom;
        check.target = D20Target::AbilityCheck {
            difficulty_class: 30,
        };
        check.roll_context = RollContext::normal();
        check.situational_modifiers.clear();
        let mut dice = SequenceDice::new([20]);
        let result = resolve_d20_test(&scores, Level::new(1).unwrap(), &check, &mut dice).unwrap();
        assert_eq!(result.outcome, D20TestOutcome::Failure);
    }

    #[test]
    fn attack_natural_one_and_twenty_are_attack_only_rules() {
        let scores = AbilityScores::new(16, 14, 12, 10, 8, 6).unwrap();
        let attack = |armor_class| D20TestRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: Ability::Strength,
            proficiency: Proficiency::Proficient,
            roll_context: RollContext::normal(),
            situational_modifiers: Vec::new(),
            target: D20Target::Attack {
                armor_class,
                cover: Cover::None,
            },
        };
        let mut dice = SequenceDice::new([1, 20]);
        assert_eq!(
            resolve_d20_test(&scores, Level::new(1).unwrap(), &attack(1), &mut dice)
                .unwrap()
                .outcome,
            D20TestOutcome::AutomaticMiss
        );
        assert_eq!(
            resolve_d20_test(&scores, Level::new(1).unwrap(), &attack(60), &mut dice)
                .unwrap()
                .outcome,
            D20TestOutcome::CriticalHit
        );
    }

    #[test]
    fn exact_weapon_modes_and_ranges_are_closed() {
        let thrown = [
            (SimpleWeaponId::Dagger, 20, 60),
            (SimpleWeaponId::Handaxe, 20, 60),
            (SimpleWeaponId::Javelin, 30, 120),
            (SimpleWeaponId::LightHammer, 20, 60),
            (SimpleWeaponId::Spear, 20, 60),
        ];
        for (kind, normal, long) in thrown {
            let weapon = WeaponChoice::Simple { kind };
            assert_eq!(
                resolve_weapon_range(&weapon, AttackMode::Ranged, normal, false)
                    .unwrap()
                    .band,
                RangeBand::Normal
            );
            let long_result =
                resolve_weapon_range(&weapon, AttackMode::Ranged, long, false).unwrap();
            assert_eq!(long_result.band, RangeBand::Long);
            assert_eq!(long_result.disadvantage_sources, 1);
            assert!(resolve_weapon_range(&weapon, AttackMode::Ranged, long + 5, false).is_err());
        }
        for kind in [
            SimpleWeaponId::Club,
            SimpleWeaponId::Greatclub,
            SimpleWeaponId::Mace,
            SimpleWeaponId::Quarterstaff,
            SimpleWeaponId::Sickle,
        ] {
            assert!(matches!(
                resolve_weapon_range(&WeaponChoice::Simple { kind }, AttackMode::Ranged, 5, false),
                Err(RulesMatrixError::Unsupported(_))
            ));
        }
        assert_eq!(
            resolve_weapon_range(&WeaponChoice::LightCrossbow, AttackMode::Ranged, 80, true)
                .unwrap()
                .disadvantage_sources,
            1
        );
    }

    #[test]
    fn derived_weapon_attack_resolves_cover_critical_dice_and_interaction() {
        let hero = fighter();
        let target = WeaponAttackTarget {
            target_id: "wight-1".to_owned(),
            distance_feet: 5,
            armor_class: 30,
            cover: Cover::ThreeQuarters,
            threatening_hostile_within_five_feet: false,
            damage_profile: DamageProfile {
                resistances: Vec::new(),
                vulnerabilities: vec![DamageType::Slashing],
                immunities: Vec::new(),
            },
        };
        let mut dice = SequenceDice::new([20, 4, 5]);
        let result = resolve_weapon_attack(
            &hero.sheet,
            "attack:longsword",
            AttackMode::Melee,
            &target,
            RollContext::normal(),
            Vec::new(),
            &mut dice,
        )
        .unwrap();
        assert_eq!(result.attack.target_number, 35);
        assert_eq!(result.attack.outcome, D20TestOutcome::CriticalHit);
        assert_eq!(result.damage.as_ref().unwrap().dice, vec![4, 5]);
        assert_eq!(
            result.damage_interaction,
            Some(DamageInteraction::Vulnerability)
        );
        assert_eq!(result.effective_damage, 24); // (4 + 5 + Str 3) * 2

        let mut target_conditions = ConditionSet::empty();
        target_conditions
            .apply(permanent(ConditionId::Unconscious, "target.unconscious"))
            .unwrap();
        let close_target = WeaponAttackTarget {
            target_id: "wight-2".to_owned(),
            distance_feet: 5,
            armor_class: 1,
            cover: Cover::None,
            threatening_hostile_within_five_feet: false,
            damage_profile: DamageProfile::normal(),
        };
        let mut dice = SequenceDice::new([10, 11, 4, 5]);
        let conditioned = resolve_conditioned_weapon_attack(
            &hero.sheet,
            "attack:longsword",
            AttackMode::Melee,
            &close_target,
            &WeaponAttackConditionContext {
                base_roll_context: RollContext::normal(),
                situational_modifiers: Vec::new(),
                attacker_conditions: ConditionSet::empty(),
                target_conditions,
            },
            &mut dice,
        )
        .unwrap();
        assert_eq!(conditioned.attack.outcome, D20TestOutcome::Success);
        assert!(conditioned.critical_damage);
        assert_eq!(conditioned.damage.unwrap().dice.len(), 2);
    }

    #[test]
    fn reachable_conditions_have_exhaustive_combat_effects() {
        let mut conditions = ConditionSet::empty();
        for (condition, source) in [
            (ConditionId::Prone, "condition.prone"),
            (ConditionId::Restrained, "condition.restrained"),
            (ConditionId::Grappled, "condition.grappled"),
            (ConditionId::Incapacitated, "condition.incapacitated"),
            (ConditionId::Unconscious, "condition.unconscious"),
            (ConditionId::Poisoned, "condition.poisoned"),
        ] {
            conditions.apply(permanent(condition, source)).unwrap();
        }
        let own = condition_effects(&conditions, RollSituation::OwnAttack).unwrap();
        assert_eq!(own.roll_context.disadvantage_sources, 3);
        assert!(own.actions_blocked && own.reactions_blocked && own.speed_is_zero);
        let incoming = condition_effects(
            &conditions,
            RollSituation::IncomingAttack {
                attacker_distance_feet: 5,
            },
        )
        .unwrap();
        assert_eq!(incoming.roll_context.advantage_sources, 3);
        assert!(incoming.incoming_hit_is_critical);
        let save = condition_effects(
            &conditions,
            RollSituation::SavingThrow {
                ability: Ability::Dexterity,
            },
        )
        .unwrap();
        assert!(save.automatic_save_failure);
        assert_eq!(save.roll_context.disadvantage_sources, 1);

        let request = D20TestRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: Ability::Dexterity,
            proficiency: Proficiency::Proficient,
            roll_context: RollContext::normal(),
            situational_modifiers: Vec::new(),
            target: D20Target::SavingThrow {
                difficulty_class: 15,
                cover: Cover::None,
            },
        };
        let mut dice = SequenceDice::new([17]);
        assert!(matches!(
            resolve_conditioned_d20_test(
                &AbilityScores::new(10, 10, 10, 10, 10, 10).unwrap(),
                Level::new(1).unwrap(),
                &request,
                &conditions,
                RollSituation::SavingThrow {
                    ability: Ability::Dexterity
                },
                &mut dice
            )
            .unwrap(),
            ConditionedD20Resolution::AutomaticSaveFailure { .. }
        ));
        assert_eq!(dice.values, VecDeque::from([17]));
    }

    #[test]
    fn duration_processing_expires_only_at_the_named_boundary_or_sleep_wake_event() {
        let mut conditions = ConditionSet::empty();
        conditions
            .apply(ActiveCondition {
                condition: ConditionId::Restrained,
                source: ConditionSource::Mechanic {
                    mechanic_id: "net-restraint".to_owned(),
                },
                duration: EffectDuration::Rounds {
                    remaining: 1,
                    boundary: TurnBoundary::End,
                    actor_id: "hero-1".to_owned(),
                },
            })
            .unwrap();
        conditions
            .apply(ActiveCondition {
                condition: ConditionId::Unconscious,
                source: ConditionSource::Spell {
                    spell: SpellId::Sleep,
                    caster_id: "wizard-1".to_owned(),
                },
                duration: EffectDuration::UntilDamagedOrAwakened {
                    remaining_rounds: 10,
                    actor_id: "hero-1".to_owned(),
                },
            })
            .unwrap();
        assert!(
            process_durations(
                &mut conditions,
                &DurationEvent::TurnBoundary {
                    actor_id: "hero-1".to_owned(),
                    boundary: TurnBoundary::Start
                }
            )
            .unwrap()
            .expired
            .is_empty()
        );
        assert_eq!(
            process_durations(
                &mut conditions,
                &DurationEvent::Damaged {
                    actor_id: "hero-1".to_owned()
                }
            )
            .unwrap()
            .expired
            .len(),
            1
        );
        assert_eq!(
            process_durations(
                &mut conditions,
                &DurationEvent::TurnBoundary {
                    actor_id: "hero-1".to_owned(),
                    boundary: TurnBoundary::End
                }
            )
            .unwrap()
            .expired
            .len(),
            1
        );
    }

    #[test]
    fn movement_and_all_core_actions_are_contextual_and_deterministic() {
        let empty = ConditionSet::empty();
        let contexts = [
            ActionContext::Attack {
                target_is_valid: true,
                in_range: true,
            },
            ActionContext::CastSpell {
                spell: SpellId::FireBolt,
                target_is_valid: true,
                prepared: true,
                slot_available: true,
            },
            ActionContext::Dash,
            ActionContext::Disengage,
            ActionContext::Dodge,
            ActionContext::Help {
                target_id: "ally-1".to_owned(),
                target_is_valid: true,
                target_within_five_feet: true,
            },
            ActionContext::Hide {
                obscured_from_observers: true,
            },
            ActionContext::Ready {
                trigger: ReadyTrigger::DoorOpens,
                reaction_available: true,
            },
            ActionContext::Search,
            ActionContext::UseObject {
                item: EquipmentId::ExplorersPack,
                item_is_carried: true,
                authored_use_available: true,
            },
        ];
        assert_eq!(
            contexts
                .iter()
                .map(ActionContext::capability)
                .collect::<Vec<_>>(),
            ActionCapability::CORE
        );
        for context in contexts {
            let economy = ActionEconomy::new(30);
            assert!(
                action_availability(&economy, &empty, &context)
                    .unwrap()
                    .is_available()
            );
        }

        let mut movement = MovementState::new(30, 0).unwrap();
        assert_eq!(
            move_to(
                &mut movement,
                10,
                MovementContext {
                    difficult_terrain: true,
                    crawling: true
                },
                &empty
            )
            .unwrap(),
            30
        );
        let mut economy = ActionEconomy::new(30);
        movement.reset();
        let effect =
            apply_core_action(&mut economy, &mut movement, &empty, &ActionContext::Dash).unwrap();
        assert_eq!(
            effect,
            CoreActionEffect::Dashed {
                movement_gained_feet: 30
            }
        );
        assert_eq!(movement.remaining_feet, 60);
    }

    #[test]
    fn health_transitions_hold_for_a_wide_damage_range() {
        for amount in 1..=40 {
            let mut health = HealthState::new(10).unwrap();
            health.temporary = 3;
            let result = apply_damage(
                &mut health,
                &DamageProfile::normal(),
                &DamageRequest {
                    amount,
                    damage_type: DamageType::Fire,
                    critical_hit: false,
                },
            )
            .unwrap();
            result.resulting_health.validate().unwrap();
            assert_eq!(result.temporary_hit_points_lost, amount.min(3));
        }
    }

    #[test]
    fn damage_interactions_cancel_or_transform_after_dice() {
        for damage_type in [
            DamageType::Bludgeoning,
            DamageType::Piercing,
            DamageType::Slashing,
            DamageType::Fire,
            DamageType::Force,
        ] {
            let both = DamageProfile {
                resistances: vec![damage_type],
                vulnerabilities: vec![damage_type],
                immunities: Vec::new(),
            };
            assert_eq!(
                both.interaction(damage_type).unwrap(),
                DamageInteraction::Normal
            );
        }
        assert_eq!(
            interacted_damage(5, DamageInteraction::Resistance).unwrap(),
            2
        );
        assert_eq!(
            interacted_damage(5, DamageInteraction::Vulnerability).unwrap(),
            10
        );
        assert_eq!(
            interacted_damage(5, DamageInteraction::Immunity).unwrap(),
            0
        );
    }

    #[test]
    fn death_saves_stabilization_healing_and_story_recovery_follow_q06() {
        let mut health = HealthState::new(10).unwrap();
        let down = apply_damage(
            &mut health,
            &DamageProfile::normal(),
            &DamageRequest {
                amount: 10,
                damage_type: DamageType::Slashing,
                critical_hit: false,
            },
        )
        .unwrap();
        assert_eq!(health.vital_status, VitalStatus::Dying);
        let mut conditions = ConditionSet::empty();
        apply_health_condition_changes("hero-1", &mut conditions, &down.condition_changes).unwrap();
        assert!(conditions.contains(ConditionId::Unconscious));

        let mut dice = SequenceDice::new([1, 20]);
        assert_eq!(
            resolve_death_save(&mut health, &mut dice).unwrap().outcome,
            DeathSaveOutcome::CriticalFailure
        );
        assert_eq!(health.death_saves.failures, 2);
        assert_eq!(
            resolve_death_save(&mut health, &mut dice).unwrap().outcome,
            DeathSaveOutcome::Revived
        );
        let healed = apply_healing(&mut health, 20).unwrap();
        assert_eq!(healed.effective_healing, 9);

        health.current = 0;
        health.vital_status = VitalStatus::Stable;
        health.death_saves = DeathSaveTally {
            successes: 3,
            failures: 0,
        };
        let recovery = apply_defeat_recovery(LethalityPolicy::StoryRecovery, &mut health).unwrap();
        assert!(recovery.story_recovery_applied);
        assert_eq!(health.current, 1);
    }

    #[test]
    fn critical_damage_doubles_dice_but_not_modifier() {
        let mut dice = SequenceDice::new([4, 5]);
        let damage = resolve_damage_dice(1, 8, 3, true, &mut dice).unwrap();
        assert_eq!(damage.dice, vec![4, 5]);
        assert_eq!(damage.total, 12);
    }

    #[test]
    fn fighter_resources_second_wind_action_surge_and_rests_transition() {
        let mut resources = RuntimeResources::new(HeroClass::Fighter, SupportedLevel::Two);
        let mut economy = ActionEconomy::new(30);
        grant_supported_bonus_action(&resources, &mut economy).unwrap();
        let mut health = HealthState::new(20).unwrap();
        health.current = 10;
        let mut dice = SequenceDice::new([5]);
        let wind = use_second_wind(&mut resources, &mut economy, &mut health, &mut dice).unwrap();
        assert_eq!(wind.healing.effective_healing, 7);
        assert_eq!(resources.second_wind.unwrap().current, 0);
        economy.spend(TurnResource::Action).unwrap();
        use_action_surge(&mut resources, &mut economy).unwrap();
        assert!(economy.action_available);
        assert_eq!(resources.action_surge.unwrap().current, 0);

        let mut dice = SequenceDice::new([]);
        take_short_rest(
            &mut resources,
            &mut health,
            2,
            &ShortRestRequest {
                hit_dice_to_spend: 0,
                use_arcane_recovery: false,
            },
            &mut dice,
        )
        .unwrap();
        assert_eq!(resources.second_wind.unwrap().current, 1);
        assert_eq!(resources.action_surge.unwrap().current, 1);
    }

    #[test]
    fn wizard_hit_dice_arcane_recovery_and_long_rest_are_bounded() {
        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::Two);
        resources.hit_dice.current = 1;
        resources.level_one_spell_slots.as_mut().unwrap().current = 1;
        let mut health = HealthState::new(12).unwrap();
        health.current = 4;
        let mut dice = SequenceDice::new([4]);
        let short = take_short_rest(
            &mut resources,
            &mut health,
            1,
            &ShortRestRequest {
                hit_dice_to_spend: 1,
                use_arcane_recovery: true,
            },
            &mut dice,
        )
        .unwrap();
        assert_eq!(short.hit_points_recovered, 5);
        assert_eq!(short.spell_slots_recovered, 1);
        assert_eq!(resources.arcane_recovery.unwrap().current, 0);
        let long = take_long_rest(&mut resources, &mut health).unwrap();
        assert_eq!(long.hit_dice_recovered, 1);
        assert_eq!(long.spell_slots_recovered, 1);
        assert_eq!(health.current, health.maximum);
    }

    #[test]
    fn inventory_enforces_authored_capacity_and_currency_is_value_preserving() {
        let hero = fighter();
        let mut inventory = InventoryState::from_equipment(&hero.sheet.equipment).unwrap();
        assert_eq!(runtime_armor_class(&hero.sheet, &inventory).unwrap(), 19);
        inventory
            .currency
            .credit(Currency {
                copper: 5,
                silver: 2,
                gold: 1,
            })
            .unwrap();
        assert_eq!(inventory.currency.total_copper().unwrap(), 125);
        inventory
            .currency
            .debit(Currency {
                copper: 0,
                silver: 3,
                gold: 0,
            })
            .unwrap();
        assert_eq!(inventory.currency.total_copper().unwrap(), 95);
        let mut economy = ActionEconomy::new(30);
        inventory
            .use_object_interaction_to_ready_weapon(WeaponChoice::LightCrossbow, &mut economy)
            .unwrap();
        assert!(
            inventory
                .validate_readied_attack(&WeaponChoice::LightCrossbow)
                .is_err()
        );
        inventory
            .set_equipped_between_scenes(EquipmentId::Shield, false)
            .unwrap();
        assert_eq!(runtime_armor_class(&hero.sheet, &inventory).unwrap(), 17);
        inventory
            .validate_readied_attack(&WeaponChoice::LightCrossbow)
            .unwrap();
        assert!(!economy.object_interaction_available);
        assert!(matches!(
            inventory.use_consumable("item.healing-potion"),
            Err(RulesMatrixError::Unsupported(_))
        ));
    }

    #[test]
    fn fire_bolt_uses_attack_cover_critical_damage_and_object_ignition() {
        let (casting, mut resources, mut economy) = spell_fixture();
        let intent = SupportedSpellIntent::FireBolt {
            target: FireBoltTarget {
                target_id: "crate-1".to_owned(),
                distance_feet: 120,
                visible: true,
                armor_class: 40,
                cover: Cover::ThreeQuarters,
                threatening_hostile_within_five_feet: false,
                damage_profile: DamageProfile::normal(),
                target_kind: FireBoltTargetKind::UnattendedFlammableObject,
                conditions: ConditionSet::empty(),
            },
            roll_context: RollContext::normal(),
        };
        let mut dice = SequenceDice::new([20, 10, 9]);
        let result = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &intent,
            &mut dice,
        )
        .unwrap();
        assert_eq!(result.attack.unwrap().outcome, D20TestOutcome::CriticalHit);
        assert_eq!(result.damage_rolls[0].total, 19);
        assert!(
            result.effects.iter().any(|effect| matches!(
                effect,
                SpellEffect::IgniteUnattendedFlammableObject { .. }
            ))
        );
        assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
        assert!(!economy.action_available);
    }

    #[test]
    fn light_and_mage_hand_apply_exact_targeting_without_slots() {
        let (casting, mut resources, mut economy) = spell_fixture();
        let mut dice = SequenceDice::new([20]);
        let light = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Light {
                target: LightTarget {
                    object_id: "badge-1".to_owned(),
                    distance_feet: 5,
                    object_maximum_dimension_feet: 1,
                    hostile_carrier: Some(LightCarrierSave {
                        carrier_id: "guard-1".to_owned(),
                        dexterity_save_modifier: 0,
                        roll_context: RollContext::normal(),
                    }),
                },
            },
            &mut dice,
        )
        .unwrap();
        assert!(light.saving_throw.unwrap().success);
        assert!(light.effects.is_empty());
        assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);

        let (casting, mut resources, mut economy) = spell_fixture();
        let mut dice = SequenceDice::new([]);
        let hand = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::MageHand {
                target: MageHandTarget {
                    hand_id: "mage-hand-1".to_owned(),
                    distance_feet: 30,
                },
            },
            &mut dice,
        )
        .unwrap();
        assert!(matches!(
            hand.effects[0],
            SpellEffect::CreateMageHand { .. }
        ));
    }

    #[test]
    fn magic_missile_shield_and_sleep_have_typed_resource_effects() {
        let (casting, mut resources, mut economy) = spell_fixture();
        let target = |id: &str, shielded| MagicMissileTarget {
            target_id: id.to_owned(),
            distance_feet: 120,
            visible: true,
            shielded,
            damage_profile: DamageProfile::normal(),
        };
        let mut dice = SequenceDice::new([1, 2, 3]);
        let missiles = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::MagicMissile {
                darts: Box::new([target("a", false), target("a", false), target("b", true)]),
            },
            &mut dice,
        )
        .unwrap();
        assert_eq!(missiles.damage_rolls.len(), 3);
        assert!(missiles.effects.iter().any(|effect| matches!(
            effect,
            SpellEffect::MagicMissileNegated { target_id } if target_id == "b"
        )));
        assert_eq!(resources.level_one_spell_slots.unwrap().current, 1);

        let (casting, mut resources, mut economy) = spell_fixture();
        let mut dice = SequenceDice::new([]);
        let shield = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Shield {
                trigger: ShieldTrigger::AttackHit {
                    natural_roll: 10,
                    attack_total: 14,
                    armor_class: 12,
                },
            },
            &mut dice,
        )
        .unwrap();
        assert!(matches!(
            shield.effects[0],
            SpellEffect::ShieldWard {
                negates_triggering_attack: true,
                ..
            }
        ));
        assert!(!economy.reaction_available);

        let (casting, mut resources, mut economy) = spell_fixture();
        let mut dice = SequenceDice::new([]);
        let natural_twenty = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Shield {
                trigger: ShieldTrigger::AttackHit {
                    natural_roll: 20,
                    attack_total: 5,
                    armor_class: 30,
                },
            },
            &mut dice,
        )
        .unwrap();
        assert!(matches!(
            natural_twenty.effects[0],
            SpellEffect::ShieldWard {
                negates_triggering_attack: false,
                ..
            }
        ));

        let (casting, mut resources, mut economy) = spell_fixture();
        let mut dice = SequenceDice::new([8, 8, 8, 8, 8]);
        let sleep = resolve_supported_spell(
            &casting,
            &mut resources,
            &mut economy,
            &ConditionSet::empty(),
            SpellComponentAccess::available(),
            &SupportedSpellIntent::Sleep {
                center_distance_feet: 90,
                candidates: vec![
                    SleepCandidate {
                        target_id: "large".to_owned(),
                        distance_from_point_feet: 20,
                        current_hit_points: 30,
                        already_unconscious: false,
                        immune_to_magical_sleep: false,
                    },
                    SleepCandidate {
                        target_id: "small".to_owned(),
                        distance_from_point_feet: 5,
                        current_hit_points: 10,
                        already_unconscious: false,
                        immune_to_magical_sleep: false,
                    },
                    SleepCandidate {
                        target_id: "immune".to_owned(),
                        distance_from_point_feet: 10,
                        current_hit_points: 1,
                        already_unconscious: false,
                        immune_to_magical_sleep: true,
                    },
                    SleepCandidate {
                        target_id: "medium".to_owned(),
                        distance_from_point_feet: 15,
                        current_hit_points: 20,
                        already_unconscious: false,
                        immune_to_magical_sleep: false,
                    },
                ],
            },
            &mut dice,
        )
        .unwrap();
        assert_eq!(sleep.sleep_hit_point_pool, Some(40));
        assert_eq!(
            sleep
                .effects
                .iter()
                .filter(|effect| matches!(
                    effect,
                    SpellEffect::ApplyCondition {
                        condition: ActiveCondition {
                            condition: ConditionId::Unconscious,
                            ..
                        },
                        ..
                    }
                ))
                .count(),
            2
        );
    }

    #[test]
    fn invalid_spell_target_is_atomic_and_unknown_spells_are_structured() {
        let (casting, mut resources, mut economy) = spell_fixture();
        let original_resources = resources.clone();
        let original_economy = economy.clone();
        let mut dice = SequenceDice::new([]);
        let invalid = SupportedSpellIntent::MageHand {
            target: MageHandTarget {
                hand_id: "mage-hand-1".to_owned(),
                distance_feet: 35,
            },
        };
        assert!(
            resolve_supported_spell(
                &casting,
                &mut resources,
                &mut economy,
                &ConditionSet::empty(),
                SpellComponentAccess::available(),
                &invalid,
                &mut dice
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(economy, original_economy);
        let missing_somatic = SupportedSpellIntent::MageHand {
            target: MageHandTarget {
                hand_id: "mage-hand-1".to_owned(),
                distance_feet: 30,
            },
        };
        assert!(
            resolve_supported_spell(
                &casting,
                &mut resources,
                &mut economy,
                &ConditionSet::empty(),
                SpellComponentAccess {
                    verbal_available: true,
                    somatic_available: false,
                    material_focus_available: true,
                },
                &missing_somatic,
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(economy, original_economy);
        assert!(matches!(
            unsupported_mechanic("spell.teleport"),
            RulesMatrixError::Unsupported(_)
        ));
    }

    #[test]
    fn light_exhausts_target_component_save_and_resource_boundaries() {
        for distance_feet in [0, 5] {
            for object_maximum_dimension_feet in [1, 10] {
                let intent = SupportedSpellIntent::Light {
                    target: LightTarget {
                        object_id: "lamp-object".to_owned(),
                        distance_feet,
                        object_maximum_dimension_feet,
                        hostile_carrier: None,
                    },
                };
                let (resolution, resources, economy, dice) = resolve_spell_fixture(
                    &intent,
                    SpellComponentAccess {
                        verbal_available: true,
                        somatic_available: false,
                        material_focus_available: true,
                    },
                    [],
                );
                let resolution = resolution.unwrap();
                assert!(matches!(
                    &resolution.effects[0],
                    SpellEffect::IlluminateObject {
                        caster_id,
                        object_id,
                        bright_light_feet: 20,
                        dim_light_feet: 20,
                        duration_rounds: 600,
                    } if caster_id == "wizard-1" && object_id == "lamp-object"
                ));
                assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
                assert!(!economy.action_available);
                assert!(dice.values.is_empty());
            }
        }

        for (distance_feet, object_maximum_dimension_feet) in [(1, 1), (10, 1), (0, 0), (0, 11)] {
            let intent = SupportedSpellIntent::Light {
                target: LightTarget {
                    object_id: "lamp-object".to_owned(),
                    distance_feet,
                    object_maximum_dimension_feet,
                    hostile_carrier: None,
                },
            };
            let (resolution, resources, economy, dice) =
                resolve_spell_fixture(&intent, SpellComponentAccess::available(), [20]);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.action_available);
            assert_eq!(dice.values.len(), 1);
        }

        for (rolls, context, save_succeeds) in [
            (vec![1], RollContext::normal(), false),
            (vec![20], RollContext::normal(), true),
            (
                vec![2, 18],
                RollContext {
                    advantage_sources: 1,
                    disadvantage_sources: 0,
                },
                true,
            ),
        ] {
            let intent = SupportedSpellIntent::Light {
                target: LightTarget {
                    object_id: "worn-badge".to_owned(),
                    distance_feet: 5,
                    object_maximum_dimension_feet: 1,
                    hostile_carrier: Some(LightCarrierSave {
                        carrier_id: "hostile-carrier".to_owned(),
                        dexterity_save_modifier: 0,
                        roll_context: context,
                    }),
                },
            };
            let (resolution, resources, economy, _) =
                resolve_spell_fixture(&intent, SpellComponentAccess::available(), rolls);
            let resolution = resolution.unwrap();
            assert_eq!(
                resolution.saving_throw.as_ref().unwrap().success,
                save_succeeds
            );
            assert_eq!(resolution.effects.is_empty(), save_succeeds);
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(!economy.action_available);
        }

        let intent = SupportedSpellIntent::Light {
            target: LightTarget {
                object_id: "lamp-object".to_owned(),
                distance_feet: 0,
                object_maximum_dimension_feet: 1,
                hostile_carrier: None,
            },
        };
        for components in [
            SpellComponentAccess {
                verbal_available: false,
                somatic_available: true,
                material_focus_available: true,
            },
            SpellComponentAccess {
                verbal_available: true,
                somatic_available: true,
                material_focus_available: false,
            },
        ] {
            let (resolution, resources, economy, _) =
                resolve_spell_fixture(&intent, components, []);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.action_available);
        }
    }

    #[test]
    fn mage_hand_separates_cast_control_duration_and_closed_limits() {
        for distance_feet in [0, 30] {
            let intent = SupportedSpellIntent::MageHand {
                target: MageHandTarget {
                    hand_id: "mage-hand-1".to_owned(),
                    distance_feet,
                },
            };
            let (resolution, resources, economy, _) = resolve_spell_fixture(
                &intent,
                SpellComponentAccess {
                    verbal_available: true,
                    somatic_available: true,
                    material_focus_available: false,
                },
                [],
            );
            let resolution = resolution.unwrap();
            assert!(matches!(
                &resolution.effects[0],
                SpellEffect::CreateMageHand { hand }
                    if hand.hand_id == "mage-hand-1"
                        && hand.caster_id == "wizard-1"
                        && hand.distance_from_caster_feet == distance_feet
                        && hand.remaining_rounds == 10
            ));
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(!economy.action_available);
        }
        for distance_feet in [1, 31, 35] {
            let intent = SupportedSpellIntent::MageHand {
                target: MageHandTarget {
                    hand_id: "mage-hand-1".to_owned(),
                    distance_feet,
                },
            };
            let (resolution, resources, economy, _) =
                resolve_spell_fixture(&intent, SpellComponentAccess::available(), []);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.action_available);
        }

        let base_hand = MageHandState {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            hand_id: "mage-hand-1".to_owned(),
            caster_id: "wizard-1".to_owned(),
            distance_from_caster_feet: 0,
            remaining_rounds: 10,
        };
        base_hand.validate().unwrap();
        let encoded = serde_json::to_value(&base_hand).unwrap();
        let decoded: MageHandState = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, base_hand);
        let mut unknown_field = encoded;
        unknown_field
            .as_object_mut()
            .unwrap()
            .insert("duration_minutes".to_owned(), serde_json::json!(1));
        assert!(serde_json::from_value::<MageHandState>(unknown_field).is_err());
        let mut future_schema = base_hand.clone();
        future_schema.schema_version += 1;
        assert!(future_schema.validate().is_err());
        for operation in [
            MageHandOperation::ManipulateObject,
            MageHandOperation::OpenUnlockedDoorOrContainer,
            MageHandOperation::StowObject,
            MageHandOperation::RetrieveObject,
            MageHandOperation::PourContents,
        ] {
            let mut hand = Some(base_hand.clone());
            let mut economy = ActionEconomy::new(30);
            let resolution = resolve_mage_hand_action(
                &mut hand,
                &mut economy,
                &ConditionSet::empty(),
                &MageHandActionIntent::Control {
                    target: MageHandControlTarget {
                        object_id: "ordinary-object".to_owned(),
                        hand_movement_feet: 30,
                        resulting_distance_from_caster_feet: 30,
                        object_weight_pounds: 10,
                        is_magic_item: false,
                        operation,
                    },
                },
            )
            .unwrap();
            assert!(matches!(
                resolution.effect,
                MageHandActionEffect::Controlled {
                    operation: actual,
                    hand_movement_feet: 30,
                    resulting_distance_from_caster_feet: 30,
                    ..
                } if actual == operation
            ));
            assert_eq!(hand.as_ref().unwrap().distance_from_caster_feet, 30);
            assert!(!economy.action_available);
        }

        let invalid_targets = [
            MageHandControlTarget {
                object_id: "ordinary-object".to_owned(),
                hand_movement_feet: 0,
                resulting_distance_from_caster_feet: 30,
                object_weight_pounds: 1,
                is_magic_item: false,
                operation: MageHandOperation::ManipulateObject,
            },
            MageHandControlTarget {
                object_id: "ordinary-object".to_owned(),
                hand_movement_feet: 35,
                resulting_distance_from_caster_feet: 30,
                object_weight_pounds: 10,
                is_magic_item: false,
                operation: MageHandOperation::ManipulateObject,
            },
            MageHandControlTarget {
                object_id: "ordinary-object".to_owned(),
                hand_movement_feet: 30,
                resulting_distance_from_caster_feet: 35,
                object_weight_pounds: 10,
                is_magic_item: false,
                operation: MageHandOperation::ManipulateObject,
            },
            MageHandControlTarget {
                object_id: "ordinary-object".to_owned(),
                hand_movement_feet: 0,
                resulting_distance_from_caster_feet: 0,
                object_weight_pounds: 11,
                is_magic_item: false,
                operation: MageHandOperation::ManipulateObject,
            },
            MageHandControlTarget {
                object_id: "magic-object".to_owned(),
                hand_movement_feet: 0,
                resulting_distance_from_caster_feet: 0,
                object_weight_pounds: 1,
                is_magic_item: true,
                operation: MageHandOperation::ManipulateObject,
            },
        ];
        for target in invalid_targets {
            let mut hand = Some(base_hand.clone());
            let original_hand = hand.clone();
            let mut economy = ActionEconomy::new(30);
            let original_economy = economy.clone();
            assert!(
                resolve_mage_hand_action(
                    &mut hand,
                    &mut economy,
                    &ConditionSet::empty(),
                    &MageHandActionIntent::Control { target },
                )
                .is_err()
            );
            assert_eq!(hand, original_hand);
            assert_eq!(economy, original_economy);
        }

        let mut hand = Some(base_hand.clone());
        let mut spent_economy = ActionEconomy::new(30);
        spent_economy.spend(TurnResource::Action).unwrap();
        let original_hand = hand.clone();
        assert!(
            resolve_mage_hand_action(
                &mut hand,
                &mut spent_economy,
                &ConditionSet::empty(),
                &MageHandActionIntent::Dismiss,
            )
            .is_err()
        );
        assert_eq!(hand, original_hand);

        let mut conditions = ConditionSet::empty();
        conditions
            .apply(permanent(
                ConditionId::Incapacitated,
                "condition.incapacitated",
            ))
            .unwrap();
        let mut hand = Some(base_hand.clone());
        let original_hand = hand.clone();
        let mut economy = ActionEconomy::new(30);
        assert!(
            resolve_mage_hand_action(
                &mut hand,
                &mut economy,
                &conditions,
                &MageHandActionIntent::Dismiss,
            )
            .is_err()
        );
        assert_eq!(hand, original_hand);
        assert!(economy.action_available);

        let mut hand = Some(base_hand.clone());
        let mut economy = ActionEconomy::new(30);
        let dismissed = resolve_mage_hand_action(
            &mut hand,
            &mut economy,
            &ConditionSet::empty(),
            &MageHandActionIntent::Dismiss,
        )
        .unwrap();
        assert!(matches!(
            dismissed.effect,
            MageHandActionEffect::Dismissed { .. }
        ));
        assert!(hand.is_none());
        assert!(!economy.action_available);

        let mut ranged_hand = Some(base_hand.clone());
        assert!(!reconcile_mage_hand_distance(&mut ranged_hand, 30).unwrap());
        let before_invalid_distance = ranged_hand.clone();
        assert!(reconcile_mage_hand_distance(&mut ranged_hand, 29).is_err());
        assert_eq!(ranged_hand, before_invalid_distance);
        assert!(reconcile_mage_hand_distance(&mut ranged_hand, 35).unwrap());
        assert!(ranged_hand.is_none());

        let mut hand = Some(base_hand);
        for remaining_after in (1..=9).rev() {
            assert!(!advance_mage_hand_duration(&mut hand).unwrap());
            assert_eq!(hand.as_ref().unwrap().remaining_rounds, remaining_after);
        }
        assert!(advance_mage_hand_duration(&mut hand).unwrap());
        assert!(hand.is_none());
        assert!(advance_mage_hand_duration(&mut hand).is_err());
    }

    #[test]
    fn shield_exhausts_trigger_reaction_slot_and_negation_boundaries() {
        for armor_class in [1_u8, 12, 30] {
            for amount_over_armor_class in 0_i16..=6 {
                let intent = SupportedSpellIntent::Shield {
                    trigger: ShieldTrigger::AttackHit {
                        natural_roll: 10,
                        attack_total: i16::from(armor_class) + amount_over_armor_class,
                        armor_class,
                    },
                };
                let (resolution, resources, economy, _) = resolve_spell_fixture(
                    &intent,
                    SpellComponentAccess {
                        verbal_available: true,
                        somatic_available: true,
                        material_focus_available: false,
                    },
                    [],
                );
                let resolution = resolution.unwrap();
                assert!(matches!(
                    resolution.effects[0],
                    SpellEffect::ShieldWard {
                        armor_class_bonus: 5,
                        negates_triggering_attack,
                        immune_to_magic_missile: true,
                        until_start_of_caster_turn: true,
                    } if negates_triggering_attack == (amount_over_armor_class < 5)
                ));
                assert_eq!(resources.level_one_spell_slots.unwrap().current, 1);
                assert!(economy.action_available);
                assert!(!economy.reaction_available);
            }
        }

        for (trigger, negates) in [
            (ShieldTrigger::MagicMissile, true),
            (
                ShieldTrigger::AttackHit {
                    natural_roll: 20,
                    attack_total: -20,
                    armor_class: 30,
                },
                false,
            ),
        ] {
            let intent = SupportedSpellIntent::Shield { trigger };
            let (resolution, resources, economy, _) =
                resolve_spell_fixture(&intent, SpellComponentAccess::available(), []);
            let resolution = resolution.unwrap();
            assert!(matches!(
                resolution.effects[0],
                SpellEffect::ShieldWard {
                    negates_triggering_attack,
                    ..
                } if negates_triggering_attack == negates
            ));
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 1);
            assert!(!economy.reaction_available);
        }

        for trigger in [
            ShieldTrigger::AttackHit {
                natural_roll: 0,
                attack_total: 12,
                armor_class: 12,
            },
            ShieldTrigger::AttackHit {
                natural_roll: 1,
                attack_total: 30,
                armor_class: 12,
            },
            ShieldTrigger::AttackHit {
                natural_roll: 21,
                attack_total: 30,
                armor_class: 12,
            },
            ShieldTrigger::AttackHit {
                natural_roll: 10,
                attack_total: 12,
                armor_class: 0,
            },
            ShieldTrigger::AttackHit {
                natural_roll: 10,
                attack_total: 31,
                armor_class: 31,
            },
            ShieldTrigger::AttackHit {
                natural_roll: 10,
                attack_total: 11,
                armor_class: 12,
            },
        ] {
            let intent = SupportedSpellIntent::Shield { trigger };
            let (resolution, resources, economy, dice) =
                resolve_spell_fixture(&intent, SpellComponentAccess::available(), [20]);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.reaction_available);
            assert_eq!(dice.values.len(), 1);
        }

        let intent = SupportedSpellIntent::Shield {
            trigger: ShieldTrigger::MagicMissile,
        };
        for components in [
            SpellComponentAccess {
                verbal_available: false,
                somatic_available: true,
                material_focus_available: true,
            },
            SpellComponentAccess {
                verbal_available: true,
                somatic_available: false,
                material_focus_available: true,
            },
        ] {
            let (resolution, resources, economy, _) =
                resolve_spell_fixture(&intent, components, []);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.reaction_available);
        }

        let (casting, mut resources, mut economy) = spell_fixture();
        economy.spend(TurnResource::Reaction).unwrap();
        let original_resources = resources.clone();
        let original_economy = economy.clone();
        let mut dice = SequenceDice::new([]);
        assert!(
            resolve_supported_spell(
                &casting,
                &mut resources,
                &mut economy,
                &ConditionSet::empty(),
                SpellComponentAccess::available(),
                &intent,
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(economy, original_economy);
    }

    #[test]
    fn sleep_exhausts_pool_order_immunity_area_and_atomicity() {
        let intent = SupportedSpellIntent::Sleep {
            center_distance_feet: 90,
            candidates: vec![
                SleepCandidate {
                    target_id: "tie-b".to_owned(),
                    distance_from_point_feet: 20,
                    current_hit_points: 5,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                },
                SleepCandidate {
                    target_id: "immune".to_owned(),
                    distance_from_point_feet: 20,
                    current_hit_points: 1,
                    already_unconscious: false,
                    immune_to_magical_sleep: true,
                },
                SleepCandidate {
                    target_id: "unconscious".to_owned(),
                    distance_from_point_feet: 20,
                    current_hit_points: 1,
                    already_unconscious: true,
                    immune_to_magical_sleep: false,
                },
                SleepCandidate {
                    target_id: "tie-a".to_owned(),
                    distance_from_point_feet: 0,
                    current_hit_points: 5,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                },
                SleepCandidate {
                    target_id: "exact-remainder".to_owned(),
                    distance_from_point_feet: 5,
                    current_hit_points: 30,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                },
            ],
        };
        let (resolution, resources, economy, _) =
            resolve_spell_fixture(&intent, SpellComponentAccess::available(), [8, 8, 8, 8, 8]);
        let resolution = resolution.unwrap();
        assert_eq!(resolution.sleep_hit_point_pool, Some(40));
        let slept = resolution
            .effects
            .iter()
            .filter_map(|effect| match effect {
                SpellEffect::ApplyCondition {
                    target_id,
                    condition:
                        ActiveCondition {
                            condition: ConditionId::Unconscious,
                            ..
                        },
                } => Some(target_id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(slept, ["tie-a", "tie-b", "exact-remainder"]);
        assert_eq!(resources.level_one_spell_slots.unwrap().current, 1);
        assert!(!economy.action_available);

        for face in 1_u16..=8 {
            let pool = face * 5;
            let intent = SupportedSpellIntent::Sleep {
                center_distance_feet: 0,
                candidates: vec![SleepCandidate {
                    target_id: "exact-pool".to_owned(),
                    distance_from_point_feet: 20,
                    current_hit_points: pool,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                }],
            };
            let (resolution, _, _, _) =
                resolve_spell_fixture(&intent, SpellComponentAccess::available(), [face; 5]);
            let resolution = resolution.unwrap();
            assert_eq!(resolution.sleep_hit_point_pool, Some(pool));
            assert!(resolution.effects.iter().any(|effect| matches!(
                effect,
                SpellEffect::ApplyCondition {
                    target_id,
                    condition: ActiveCondition {
                        condition: ConditionId::Unconscious,
                        ..
                    },
                } if target_id == "exact-pool"
            )));
        }

        let thirty_two = (0..32)
            .map(|index| SleepCandidate {
                target_id: format!("candidate-{index:02}"),
                distance_from_point_feet: 20,
                current_hit_points: 1,
                already_unconscious: false,
                immune_to_magical_sleep: false,
            })
            .collect::<Vec<_>>();
        let accepted_maximum = SupportedSpellIntent::Sleep {
            center_distance_feet: 90,
            candidates: thirty_two.clone(),
        };
        let (resolution, _, _, _) =
            resolve_spell_fixture(&accepted_maximum, SpellComponentAccess::available(), [8; 5]);
        assert_eq!(
            resolution
                .unwrap()
                .effects
                .iter()
                .filter(|effect| matches!(
                    effect,
                    SpellEffect::ApplyCondition {
                        condition: ActiveCondition {
                            condition: ConditionId::Unconscious,
                            ..
                        },
                        ..
                    }
                ))
                .count(),
            32
        );

        let mut thirty_three = thirty_two;
        thirty_three.push(SleepCandidate {
            target_id: "candidate-32".to_owned(),
            distance_from_point_feet: 20,
            current_hit_points: 1,
            already_unconscious: false,
            immune_to_magical_sleep: false,
        });
        let invalid_intents = [
            SupportedSpellIntent::Sleep {
                center_distance_feet: 95,
                candidates: Vec::new(),
            },
            SupportedSpellIntent::Sleep {
                center_distance_feet: 1,
                candidates: Vec::new(),
            },
            SupportedSpellIntent::Sleep {
                center_distance_feet: 0,
                candidates: vec![SleepCandidate {
                    target_id: "outside".to_owned(),
                    distance_from_point_feet: 25,
                    current_hit_points: 1,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                }],
            },
            SupportedSpellIntent::Sleep {
                center_distance_feet: 0,
                candidates: vec![SleepCandidate {
                    target_id: "zero-hp".to_owned(),
                    distance_from_point_feet: 0,
                    current_hit_points: 0,
                    already_unconscious: false,
                    immune_to_magical_sleep: false,
                }],
            },
            SupportedSpellIntent::Sleep {
                center_distance_feet: 0,
                candidates: vec![
                    SleepCandidate {
                        target_id: "duplicate".to_owned(),
                        distance_from_point_feet: 0,
                        current_hit_points: 1,
                        already_unconscious: false,
                        immune_to_magical_sleep: false,
                    },
                    SleepCandidate {
                        target_id: "duplicate".to_owned(),
                        distance_from_point_feet: 5,
                        current_hit_points: 2,
                        already_unconscious: false,
                        immune_to_magical_sleep: false,
                    },
                ],
            },
            SupportedSpellIntent::Sleep {
                center_distance_feet: 0,
                candidates: thirty_three,
            },
        ];
        for invalid in invalid_intents {
            let (resolution, resources, economy, dice) =
                resolve_spell_fixture(&invalid, SpellComponentAccess::available(), [1; 5]);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.action_available);
            assert_eq!(dice.values.len(), 5);
        }

        for components in [
            SpellComponentAccess {
                verbal_available: false,
                somatic_available: true,
                material_focus_available: true,
            },
            SpellComponentAccess {
                verbal_available: true,
                somatic_available: false,
                material_focus_available: true,
            },
            SpellComponentAccess {
                verbal_available: true,
                somatic_available: true,
                material_focus_available: false,
            },
        ] {
            let (resolution, resources, economy, dice) =
                resolve_spell_fixture(&intent, components, [8; 5]);
            assert!(resolution.is_err());
            assert_eq!(resources.level_one_spell_slots.unwrap().current, 2);
            assert!(economy.action_available);
            assert_eq!(dice.values.len(), 5);
        }

        let (casting, mut resources, mut economy) = spell_fixture();
        resources.level_one_spell_slots.as_mut().unwrap().current = 0;
        let original_resources = resources.clone();
        let original_economy = economy.clone();
        let mut dice = SequenceDice::new([8; 5]);
        assert!(
            resolve_supported_spell(
                &casting,
                &mut resources,
                &mut economy,
                &ConditionSet::empty(),
                SpellComponentAccess::available(),
                &intent,
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(economy, original_economy);
        assert_eq!(dice.values.len(), 5);

        let (casting, mut resources, mut economy) = spell_fixture();
        let original_resources = resources.clone();
        let original_economy = economy.clone();
        let mut dice = SequenceDice::new([0, 1, 1, 1, 1]);
        assert!(
            resolve_supported_spell(
                &casting,
                &mut resources,
                &mut economy,
                &ConditionSet::empty(),
                SpellComponentAccess::available(),
                &intent,
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(economy, original_economy);
    }

    #[test]
    fn hit_die_spending_exhausts_class_level_modifier_face_and_health_bounds() {
        for class in [HeroClass::Fighter, HeroClass::Wizard] {
            for level in [SupportedLevel::One, SupportedLevel::Two] {
                let sides = match class {
                    HeroClass::Fighter => 10_u16,
                    HeroClass::Wizard => 6_u16,
                };
                for constitution_modifier in -5_i8..=10 {
                    for face in 1_u16..=sides {
                        let mut resources = RuntimeResources::new(class, level);
                        let mut health = HealthState::new(100).unwrap();
                        health.current = 1;
                        let mut dice = SequenceDice::new([face]);
                        let spend = spend_hit_die(
                            &mut resources,
                            &mut health,
                            constitution_modifier,
                            &mut dice,
                        )
                        .unwrap();
                        let expected = (i16::try_from(face).unwrap()
                            + i16::from(constitution_modifier))
                        .max(0) as u16;
                        assert_eq!(spend.roll, face as u8);
                        assert_eq!(spend.healing_total, expected);
                        assert_eq!(spend.hit_points_recovered, expected);
                        assert_eq!(health.current, 1 + expected);
                        assert_eq!(resources.hit_dice.current, level.value() - 1);
                        assert!(dice.values.is_empty());
                    }
                }
            }
        }

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
        let mut health = HealthState::new(6).unwrap();
        let mut dice = SequenceDice::new([6]);
        let spend = spend_hit_die(&mut resources, &mut health, 10, &mut dice).unwrap();
        assert_eq!(spend.healing_total, 16);
        assert_eq!(spend.hit_points_recovered, 0);
        assert_eq!(resources.hit_dice.current, 0);

        let mut resources = RuntimeResources::new(HeroClass::Fighter, SupportedLevel::Two);
        let mut health = HealthState::new(30).unwrap();
        health.current = 1;
        let mut first_die = SequenceDice::new([1]);
        let first = spend_hit_die(&mut resources, &mut health, 0, &mut first_die).unwrap();
        assert_eq!(first.hit_points_recovered, 1);
        assert_eq!(resources.hit_dice.current, 1);
        let mut second_die = SequenceDice::new([10]);
        let second = spend_hit_die(&mut resources, &mut health, 0, &mut second_die).unwrap();
        assert_eq!(second.hit_points_recovered, 10);
        assert_eq!(resources.hit_dice.current, 0);

        let mut stable_health = HealthState::new(10).unwrap();
        stable_health.current = 0;
        stable_health.vital_status = VitalStatus::Stable;
        stable_health.death_saves.successes = 3;
        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
        let mut dice = SequenceDice::new([1]);
        let spend = spend_hit_die(&mut resources, &mut stable_health, 0, &mut dice).unwrap();
        assert_eq!(stable_health.vital_status, VitalStatus::Active);
        assert_eq!(stable_health.current, 1);
        assert_eq!(
            spend.condition_changes,
            [HealthConditionChange::RemoveZeroHitPointConditions]
        );

        for invalid_modifier in [-6, 11] {
            let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
            let mut health = HealthState::new(10).unwrap();
            health.current = 1;
            let original_resources = resources.clone();
            let original_health = health.clone();
            let mut dice = SequenceDice::new([1]);
            assert!(
                spend_hit_die(&mut resources, &mut health, invalid_modifier, &mut dice).is_err()
            );
            assert_eq!(resources, original_resources);
            assert_eq!(health, original_health);
            assert_eq!(dice.values.len(), 1);
        }

        for invalid_roll in [0, 7] {
            let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
            let mut health = HealthState::new(10).unwrap();
            health.current = 1;
            let original_resources = resources.clone();
            let original_health = health.clone();
            let mut dice = SequenceDice::new([invalid_roll]);
            assert!(spend_hit_die(&mut resources, &mut health, 0, &mut dice).is_err());
            assert_eq!(resources, original_resources);
            assert_eq!(health, original_health);
        }

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
        resources.hit_dice.current = 0;
        let mut health = HealthState::new(10).unwrap();
        let original_resources = resources.clone();
        let mut dice = SequenceDice::new([1]);
        assert!(spend_hit_die(&mut resources, &mut health, 0, &mut dice).is_err());
        assert_eq!(resources, original_resources);
        assert_eq!(dice.values.len(), 1);
    }

    #[test]
    fn short_rest_and_arcane_recovery_exhaust_resources_and_fail_atomically() {
        for level in [SupportedLevel::One, SupportedLevel::Two] {
            let maximum = if level == SupportedLevel::One { 2 } else { 3 };
            for slots_before in 0..maximum {
                let mut resources = RuntimeResources::new(HeroClass::Wizard, level);
                resources.level_one_spell_slots.as_mut().unwrap().current = slots_before;
                let mut health = HealthState::new(10).unwrap();
                let mut dice = SequenceDice::new([]);
                let rest = take_short_rest(
                    &mut resources,
                    &mut health,
                    0,
                    &ShortRestRequest {
                        hit_dice_to_spend: 0,
                        use_arcane_recovery: true,
                    },
                    &mut dice,
                )
                .unwrap();
                assert_eq!(rest.spell_slots_recovered, 1);
                assert_eq!(
                    resources.level_one_spell_slots.unwrap().current,
                    slots_before + 1
                );
                assert_eq!(resources.arcane_recovery.unwrap().current, 0);
            }
        }

        for level in [SupportedLevel::One, SupportedLevel::Two] {
            let mut resources = RuntimeResources::new(HeroClass::Fighter, level);
            resources.second_wind.as_mut().unwrap().current = 0;
            if let Some(surge) = resources.action_surge.as_mut() {
                surge.current = 0;
            }
            let mut health = HealthState::new(20).unwrap();
            let mut dice = SequenceDice::new([]);
            take_short_rest(
                &mut resources,
                &mut health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 0,
                    use_arcane_recovery: false,
                },
                &mut dice,
            )
            .unwrap();
            assert_eq!(resources.second_wind.unwrap().current, 1);
            assert!(
                resources
                    .action_surge
                    .is_none_or(|surge| surge.current == 1)
            );
        }

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::Two);
        let maximum = resources.level_one_spell_slots.unwrap().maximum;
        resources.level_one_spell_slots.as_mut().unwrap().current = maximum;
        let mut health = HealthState::new(20).unwrap();
        health.current = 1;
        let original_resources = resources.clone();
        let original_health = health.clone();
        let mut dice = SequenceDice::new([6]);
        assert!(
            take_short_rest(
                &mut resources,
                &mut health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 1,
                    use_arcane_recovery: true,
                },
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(health, original_health);
        assert_eq!(dice.values.len(), 1);

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::Two);
        resources.arcane_recovery.as_mut().unwrap().current = 0;
        resources.level_one_spell_slots.as_mut().unwrap().current = 0;
        let mut health = HealthState::new(20).unwrap();
        let original_resources = resources.clone();
        let mut dice = SequenceDice::new([6]);
        assert!(
            take_short_rest(
                &mut resources,
                &mut health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 1,
                    use_arcane_recovery: true,
                },
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(dice.values.len(), 1);

        let mut resources = RuntimeResources::new(HeroClass::Fighter, SupportedLevel::One);
        let mut health = HealthState::new(20).unwrap();
        let original_resources = resources.clone();
        let mut dice = SequenceDice::new([10]);
        assert!(
            take_short_rest(
                &mut resources,
                &mut health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 1,
                    use_arcane_recovery: true,
                },
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(dice.values.len(), 1);

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::Two);
        resources.level_one_spell_slots.as_mut().unwrap().current = 0;
        let mut health = HealthState::new(20).unwrap();
        health.current = 1;
        let mut dice = SequenceDice::new([1, 6]);
        let rest = take_short_rest(
            &mut resources,
            &mut health,
            -5,
            &ShortRestRequest {
                hit_dice_to_spend: 2,
                use_arcane_recovery: false,
            },
            &mut dice,
        )
        .unwrap();
        assert_eq!(rest.hit_die_rolls, [1, 6]);
        assert_eq!(rest.hit_points_recovered, 1);
        assert_eq!(resources.hit_dice.current, 0);
        assert_eq!(health.current, 2);
        assert_eq!(resources.arcane_recovery.unwrap().current, 1);

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
        let mut health = HealthState::new(20).unwrap();
        health.current = 1;
        let original_resources = resources.clone();
        let original_health = health.clone();
        let mut dice = SequenceDice::new([1, 1]);
        assert!(
            take_short_rest(
                &mut resources,
                &mut health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 2,
                    use_arcane_recovery: false,
                },
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(health, original_health);
        assert_eq!(dice.values.len(), 2);

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::Two);
        let mut health = HealthState::new(20).unwrap();
        health.current = 1;
        let original_resources = resources.clone();
        let original_health = health.clone();
        let mut dice = SequenceDice::new([6, 0]);
        assert!(
            take_short_rest(
                &mut resources,
                &mut health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 2,
                    use_arcane_recovery: false,
                },
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(resources, original_resources);
        assert_eq!(health, original_health);

        let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::One);
        let mut dead_health = HealthState {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            maximum: 10,
            current: 0,
            temporary: 0,
            vital_status: VitalStatus::Dead,
            death_saves: DeathSaveTally {
                successes: 0,
                failures: 3,
            },
        };
        let mut dice = SequenceDice::new([6]);
        assert!(
            take_short_rest(
                &mut resources,
                &mut dead_health,
                0,
                &ShortRestRequest {
                    hit_dice_to_spend: 1,
                    use_arcane_recovery: false,
                },
                &mut dice,
            )
            .is_err()
        );
        assert_eq!(dice.values.len(), 1);
    }

    #[test]
    fn long_rest_exhausts_level_one_two_health_and_resource_recovery() {
        for class in [HeroClass::Fighter, HeroClass::Wizard] {
            for level in [SupportedLevel::One, SupportedLevel::Two] {
                for hit_dice_before in 0..=level.value() {
                    let mut resources = RuntimeResources::new(class, level);
                    resources.hit_dice.current = hit_dice_before;
                    for pool in [
                        resources.second_wind.as_mut(),
                        resources.action_surge.as_mut(),
                        resources.level_one_spell_slots.as_mut(),
                        resources.arcane_recovery.as_mut(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        pool.current = 0;
                    }
                    let spell_slots_before = resources.level_one_spell_slots;
                    let mut health = HealthState::new(20).unwrap();
                    health.current = 1;
                    health.temporary = 7;
                    let rest = take_long_rest(&mut resources, &mut health).unwrap();
                    let regain_limit = (level.value() / 2).max(1);
                    assert_eq!(
                        rest.hit_dice_recovered,
                        (level.value() - hit_dice_before).min(regain_limit)
                    );
                    assert_eq!(
                        resources.hit_dice.current,
                        hit_dice_before + rest.hit_dice_recovered
                    );
                    assert_eq!(
                        rest.spell_slots_recovered,
                        spell_slots_before.map_or(0, |slots| slots.maximum)
                    );
                    for pool in [
                        resources.second_wind,
                        resources.action_surge,
                        resources.level_one_spell_slots,
                        resources.arcane_recovery,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        assert_eq!(pool.current, pool.maximum);
                    }
                    assert_eq!(rest.hit_points_recovered, 19);
                    assert_eq!(health.current, 20);
                    assert_eq!(health.temporary, 0);
                    assert_eq!(health.vital_status, VitalStatus::Active);
                    assert_eq!(health.death_saves, DeathSaveTally::default());
                }
            }
        }

        let invalid_health_states = [
            HealthState {
                schema_version: RULES_MATRIX_SCHEMA_VERSION,
                maximum: 10,
                current: 0,
                temporary: 0,
                vital_status: VitalStatus::Dying,
                death_saves: DeathSaveTally::default(),
            },
            HealthState {
                schema_version: RULES_MATRIX_SCHEMA_VERSION,
                maximum: 10,
                current: 0,
                temporary: 0,
                vital_status: VitalStatus::Stable,
                death_saves: DeathSaveTally {
                    successes: 3,
                    failures: 0,
                },
            },
            HealthState {
                schema_version: RULES_MATRIX_SCHEMA_VERSION,
                maximum: 10,
                current: 0,
                temporary: 0,
                vital_status: VitalStatus::Dead,
                death_saves: DeathSaveTally {
                    successes: 0,
                    failures: 3,
                },
            },
        ];
        for mut health in invalid_health_states {
            health.validate().unwrap();
            let original_health = health.clone();
            let mut resources = RuntimeResources::new(HeroClass::Wizard, SupportedLevel::Two);
            resources.hit_dice.current = 0;
            let original_resources = resources.clone();
            assert!(take_long_rest(&mut resources, &mut health).is_err());
            assert_eq!(health, original_health);
            assert_eq!(resources, original_resources);
        }
    }

    #[test]
    fn difficulty_bands_are_trusted_and_every_high_stakes_kind_requires_confirmation() {
        let bands = [
            (CheckDifficulty::VeryEasy, 5),
            (CheckDifficulty::Easy, 10),
            (CheckDifficulty::Moderate, 15),
            (CheckDifficulty::Hard, 20),
            (CheckDifficulty::VeryHard, 25),
            (CheckDifficulty::NearlyImpossible, 30),
        ];
        for (band, expected) in bands {
            assert_eq!(
                map_trusted_difficulty(band, HighStakesKind::None, false)
                    .unwrap()
                    .difficulty_class,
                expected
            );
        }
        for stakes in [
            HighStakesKind::Death,
            HighStakesKind::PermanentLoss,
            HighStakesKind::IrreversibleStoryCommitment,
            HighStakesKind::ConsentSensitiveInspiration,
            HighStakesKind::NonRenewableResource,
        ] {
            assert!(map_trusted_difficulty(CheckDifficulty::Easy, stakes, false).is_err());
            assert!(map_trusted_difficulty(CheckDifficulty::Easy, stakes, true).is_ok());
        }
    }

    #[test]
    fn trusted_skill_check_and_passive_values_use_derived_skill_ability() {
        let hero = wizard();
        let request = TrustedCheckRequest {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            ability: Ability::Intelligence,
            skill: Some(SkillId::Investigation),
            proficiency: Proficiency::Proficient,
            difficulty: CheckDifficulty::Moderate,
            stakes: HighStakesKind::None,
            player_confirmed: false,
            roll_context: RollContext::normal(),
            situational_modifiers: Vec::new(),
        };
        let mut dice = SequenceDice::new([10]);
        assert!(
            resolve_trusted_check(
                &hero.sheet.ability_scores,
                Level::new(1).unwrap(),
                &request,
                &mut dice
            )
            .unwrap()
            .result
            .outcome
            .succeeds()
        );
        assert_eq!(
            passive_skill_value(&hero.sheet, SkillId::Investigation, 2).unwrap(),
            i16::from(hero.sheet.passive_values.investigation) + 2
        );
    }

    #[test]
    fn exploration_objectives_clocks_attitudes_and_turns_are_closed_transitions() {
        let mut state = ExplorationSocialState {
            schema_version: RULES_MATRIX_SCHEMA_VERSION,
            turn: 1,
            objectives: vec![ObjectiveProgress {
                objective_id: "open-sluice".to_owned(),
                progress: 0,
                target: 2,
                status: ProgressStatus::Active,
            }],
            clocks: vec![SceneClock {
                clock_id: "rising-water".to_owned(),
                kind: ClockKind::Threat,
                filled: 0,
                segments: 4,
            }],
            npcs: vec![NpcSocialState {
                npc_id: "warden".to_owned(),
                attitude: NpcAttitude::Indifferent,
            }],
        };
        state.validate().unwrap();
        apply_exploration_social_command(
            &mut state,
            &ExplorationSocialCommand::AdvanceObjective {
                objective_id: "open-sluice".to_owned(),
                amount: 2,
            },
        )
        .unwrap();
        assert_eq!(state.objectives[0].status, ProgressStatus::Completed);
        apply_exploration_social_command(
            &mut state,
            &ExplorationSocialCommand::AdvanceClock {
                clock_id: "rising-water".to_owned(),
                amount: 9,
            },
        )
        .unwrap();
        assert_eq!(state.clocks[0].filled, 4);
        apply_exploration_social_command(
            &mut state,
            &ExplorationSocialCommand::ShiftNpcAttitude {
                npc_id: "warden".to_owned(),
                shift: AttitudeShift::OneStepBetter,
            },
        )
        .unwrap();
        assert_eq!(state.npcs[0].attitude, NpcAttitude::Friendly);
        apply_exploration_social_command(&mut state, &ExplorationSocialCommand::EndTurn).unwrap();
        assert_eq!(state.turn, 2);
    }

    #[test]
    fn runtime_turn_processes_boundaries_and_resets_exact_resources() {
        let mut conditions = ConditionSet::empty();
        conditions
            .apply(ActiveCondition {
                condition: ConditionId::Poisoned,
                source: ConditionSource::Mechanic {
                    mechanic_id: "brief-poison".to_owned(),
                },
                duration: EffectDuration::Rounds {
                    remaining: 1,
                    boundary: TurnBoundary::End,
                    actor_id: "fighter-1".to_owned(),
                },
            })
            .unwrap();
        let resources = RuntimeResources::new(HeroClass::Fighter, SupportedLevel::One);
        let mut turn = RuntimeTurnState::new("fighter-1", 1, 30, 0, conditions).unwrap();
        assert_eq!(turn.end_turn().unwrap().expired.len(), 1);
        turn.begin_next_turn(2, &resources).unwrap();
        assert!(turn.active);
        assert!(turn.action_economy.action_available);
        assert!(turn.action_economy.bonus_action_available);
    }

    #[test]
    fn strict_wire_types_reject_unknown_fields() {
        let json = r#"{
            "schema_version":1,
            "ability":"strength",
            "proficiency":"none",
            "roll_context":{"advantage_sources":0,"disadvantage_sources":0},
            "situational_modifiers":[],
            "target":{"type":"ability_check","difficulty_class":10},
            "difficulty_class":99
        }"#;
        assert!(serde_json::from_str::<D20TestRequest>(json).is_err());
    }
}
