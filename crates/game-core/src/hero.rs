//! Bounded hero creation and level-one-to-two advancement for the private MVP.
//!
//! This module deliberately models only the choices frozen by Q03/Q04.  Theme,
//! concept, and identity fields are retained for presentation but are never read
//! by [`derive_sheet`].  Durable draft and character documents validate again on
//! deserialization so a browser or stale save cannot smuggle derived mechanics.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{Ability, AbilityScores, RewardTier, RulesetId, Sha256Digest, is_valid_opaque_id};

pub const HERO_DRAFT_SCHEMA_VERSION: u16 = 1;
pub const HERO_CHARACTER_SCHEMA_VERSION: u16 = 1;
pub const HERO_DERIVED_SHEET_SCHEMA_VERSION: u16 = 1;
pub const HERO_COMMAND_SCHEMA_VERSION: u16 = 1;
pub const HERO_AUDIT_SCHEMA_VERSION: u16 = 1;
pub const HERO_UNSUPPORTED_SCHEMA_VERSION: u16 = 1;
pub const HERO_DERIVATION_ID: &str = "manchester-arcana:hero-derivation:v1";
pub const CORE_CONTENT_PACK_ID: &str = "dev.manchester-arcana.core-mvp";
pub const RAINBOUND_THEME_PACK_ID: &str = "dev.manchester-arcana.rainbound-borough";
pub const EMBERLINE_THEME_PACK_ID: &str = "dev.manchester-arcana.emberline-archive";
pub const MVP_PACK_VERSION: &str = "1.0.0";
pub const CORE_CONTENT_PACK_DIGEST: &str =
    "sha256:c62468355cc42f51b433fc19b8b32869b35cd308e9a937a6b8bea6df7dcfb7ca";
pub const RAINBOUND_THEME_PACK_DIGEST: &str =
    "sha256:9b92fc08979605e05d2f3213e1c8e2df62cc24a55ee294f292418b00a94f7a61";
pub const EMBERLINE_THEME_PACK_DIGEST: &str =
    "sha256:ce755fdef1521fbb4821cddc2d16ee0c1f7797bb58d9153bfe3a2e9afa695ac3";
/// The creator-only pin set immediately preceding live encounter rules. It
/// remains readable so existing local saves never silently change content.
pub const LEGACY_PRE_LIVE_RULES_CORE_CONTENT_PACK_DIGEST: &str =
    "sha256:f50b1f745125e4ce20f33242c28a415f912b627e1e0f8c9472342e0ec3dd4c8b";
pub const LEGACY_PRE_LIVE_RULES_RAINBOUND_THEME_PACK_DIGEST: &str =
    "sha256:8fca395feb4bae6cbb243ce3663f1501f728797f241df21930548f7232a4d636";
pub const LEGACY_PRE_LIVE_RULES_EMBERLINE_THEME_PACK_DIGEST: &str =
    "sha256:a48cd02c1fcd2aa192c4bd88ad21f63c9ea7fcfee7348e4ad26ee43257b9a641";
/// Pre-release development pins retained only so already-created local saves
/// can be identified and migrated with explicit provenance. New creation
/// always emits the current digests above.
pub const LEGACY_DEV_CORE_CONTENT_PACK_DIGEST: &str =
    "sha256:0fd02e550b122dba3e327ab05123f3ca3ce859293adda3e0666d0601ccda6296";
pub const LEGACY_DEV_RAINBOUND_THEME_PACK_DIGEST: &str =
    "sha256:aaf09a3c878e2fe8d0c7e348efa4c5167d073b8e42f215cbaff345ba9adf23c8";
pub const LEGACY_DEV_EMBERLINE_THEME_PACK_DIGEST: &str =
    "sha256:9980929a5790b9ca25d7e5cf58623f889c6476d5f50b4ba6af80b78c4f1876b2";
pub const LEVEL_TWO_XP: u32 = 300;

const MAX_NAME_CHARS: usize = 80;
const MAX_PRONOUN_CHARS: usize = 60;
const MAX_PRESENTATION_CHARS: usize = 500;
const MAX_TONE_LIMIT_CHARS: usize = 120;
const MAX_TONE_LIMITS: usize = 8;
const MAX_UNSUPPORTED_ALTERNATIVES: usize = 8;

pub type HeroResult<T> = std::result::Result<T, HeroError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HeroError {
    #[error("unsupported schema version {actual}; expected {expected}")]
    InvalidSchemaVersion { expected: u16, actual: u16 },
    #[error("invalid hero field `{field}`: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    #[error("draft step mismatch: expected {expected:?}, found {actual:?}")]
    DraftStepMismatch {
        expected: CreationStep,
        actual: CreationStep,
    },
    #[error("stale draft revision: expected {expected}, found {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("unsupported mechanic")]
    UnsupportedMechanic(UnsupportedMechanic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ThemeId {
    #[serde(rename = "dev.manchester-arcana.rainbound-borough")]
    RainboundBorough,
    #[serde(rename = "dev.manchester-arcana.emberline-archive")]
    EmberlineArchive,
}

impl ThemeId {
    pub const ALL: [Self; 2] = [Self::RainboundBorough, Self::EmberlineArchive];

    pub const fn pack_id(self) -> &'static str {
        match self {
            Self::RainboundBorough => RAINBOUND_THEME_PACK_ID,
            Self::EmberlineArchive => EMBERLINE_THEME_PACK_ID,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackPin {
    pub pack_id: String,
    pub version: String,
    pub digest: Sha256Digest,
}

impl PackPin {
    fn validate_exact(&self, field: &'static str, expected_id: &str) -> HeroResult<()> {
        validate_id(field, &self.pack_id)?;
        if self.pack_id != expected_id || self.version != MVP_PACK_VERSION {
            return invalid(
                field,
                "pack id or immutable version is outside the MVP pin set",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroPins {
    pub ruleset_id: RulesetId,
    pub core_content: PackPin,
    pub theme_id: ThemeId,
    pub theme: PackPin,
}

impl HeroPins {
    pub fn mvp(theme_id: ThemeId) -> Self {
        let theme_digest = match theme_id {
            ThemeId::RainboundBorough => RAINBOUND_THEME_PACK_DIGEST,
            ThemeId::EmberlineArchive => EMBERLINE_THEME_PACK_DIGEST,
        };
        Self {
            ruleset_id: RulesetId::Srd5_1,
            core_content: PackPin {
                pack_id: CORE_CONTENT_PACK_ID.to_owned(),
                version: MVP_PACK_VERSION.to_owned(),
                digest: Sha256Digest::new(CORE_CONTENT_PACK_DIGEST)
                    .expect("the compiled core pack digest is canonical"),
            },
            theme_id,
            theme: PackPin {
                pack_id: theme_id.pack_id().to_owned(),
                version: MVP_PACK_VERSION.to_owned(),
                digest: Sha256Digest::new(theme_digest)
                    .expect("the compiled theme pack digest is canonical"),
            },
        }
    }

    pub fn validate(&self) -> HeroResult<()> {
        if self.ruleset_id != RulesetId::Srd5_1 {
            return invalid("ruleset_id", "only srd-5.1-cc is supported");
        }
        self.core_content
            .validate_exact("core_content", CORE_CONTENT_PACK_ID)?;
        self.theme
            .validate_exact("theme", self.theme_id.pack_id())?;
        let expected_theme_digest = match self.theme_id {
            ThemeId::RainboundBorough => RAINBOUND_THEME_PACK_DIGEST,
            ThemeId::EmberlineArchive => EMBERLINE_THEME_PACK_DIGEST,
        };
        let current = self.core_content.digest.as_str() == CORE_CONTENT_PACK_DIGEST
            && self.theme.digest.as_str() == expected_theme_digest;
        if !current && !self.is_legacy_dev_alias() {
            return invalid(
                "pins.digest",
                "content and theme digests must match a retained immutable pin set",
            );
        }
        Ok(())
    }

    pub fn is_legacy_dev_alias(&self) -> bool {
        let legacy_dev_theme_digest = match self.theme_id {
            ThemeId::RainboundBorough => LEGACY_DEV_RAINBOUND_THEME_PACK_DIGEST,
            ThemeId::EmberlineArchive => LEGACY_DEV_EMBERLINE_THEME_PACK_DIGEST,
        };
        let pre_live_rules_theme_digest = match self.theme_id {
            ThemeId::RainboundBorough => LEGACY_PRE_LIVE_RULES_RAINBOUND_THEME_PACK_DIGEST,
            ThemeId::EmberlineArchive => LEGACY_PRE_LIVE_RULES_EMBERLINE_THEME_PACK_DIGEST,
        };
        (self.core_content.digest.as_str() == LEGACY_DEV_CORE_CONTENT_PACK_DIGEST
            && self.theme.digest.as_str() == legacy_dev_theme_digest)
            || (self.core_content.digest.as_str() == LEGACY_PRE_LIVE_RULES_CORE_CONTENT_PACK_DIGEST
                && self.theme.digest.as_str() == pre_live_rules_theme_digest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeroConceptId {
    CanalGuardian,
    ArchiveSeeker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AncestryId {
    Human,
}

impl AncestryId {
    pub const fn mechanic_id(self) -> &'static str {
        "srd-5.1-cc:ancestry:human"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeroClass {
    Fighter,
    Wizard,
}

impl HeroClass {
    pub const ALL: [Self; 2] = [Self::Fighter, Self::Wizard];

    pub const fn mechanic_id(self) -> &'static str {
        match self {
            Self::Fighter => "srd-5.1-cc:class:fighter-levels-1-2",
            Self::Wizard => "srd-5.1-cc:class:wizard-levels-1-2",
        }
    }

    const fn hit_die_sides(self) -> u8 {
        match self {
            Self::Fighter => 10,
            Self::Wizard => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundId {
    Soldier,
    Sage,
}

impl BackgroundId {
    pub const ALL: [Self; 2] = [Self::Soldier, Self::Sage];

    pub const fn mechanic_id(self) -> &'static str {
        match self {
            Self::Soldier => "srd-5.1-cc:background:soldier",
            Self::Sage => "srd-5.1-cc:background:sage",
        }
    }

    pub const fn skill_proficiencies(self) -> [SkillId; 2] {
        match self {
            Self::Soldier => [SkillId::Athletics, SkillId::Intimidation],
            Self::Sage => [SkillId::Arcana, SkillId::History],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillId {
    Acrobatics,
    AnimalHandling,
    Arcana,
    Athletics,
    Deception,
    History,
    Insight,
    Intimidation,
    Investigation,
    Medicine,
    Nature,
    Perception,
    Performance,
    Persuasion,
    Religion,
    SleightOfHand,
    Stealth,
    Survival,
}

impl SkillId {
    pub const ALL: [Self; 18] = [
        Self::Acrobatics,
        Self::AnimalHandling,
        Self::Arcana,
        Self::Athletics,
        Self::Deception,
        Self::History,
        Self::Insight,
        Self::Intimidation,
        Self::Investigation,
        Self::Medicine,
        Self::Nature,
        Self::Perception,
        Self::Performance,
        Self::Persuasion,
        Self::Religion,
        Self::SleightOfHand,
        Self::Stealth,
        Self::Survival,
    ];

    pub const fn ability(self) -> Ability {
        match self {
            Self::Athletics => Ability::Strength,
            Self::Acrobatics | Self::SleightOfHand | Self::Stealth => Ability::Dexterity,
            Self::Arcana | Self::History | Self::Investigation | Self::Nature | Self::Religion => {
                Ability::Intelligence
            }
            Self::AnimalHandling
            | Self::Insight
            | Self::Medicine
            | Self::Perception
            | Self::Survival => Ability::Wisdom,
            Self::Deception | Self::Intimidation | Self::Performance | Self::Persuasion => {
                Ability::Charisma
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FightingStyleId {
    Defense,
    Dueling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClassSelection {
    Fighter { fighting_style: FightingStyleId },
    Wizard,
}

impl ClassSelection {
    pub const fn class(&self) -> HeroClass {
        match self {
            Self::Fighter { .. } => HeroClass::Fighter,
            Self::Wizard => HeroClass::Wizard,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StandardArrayAssignment {
    pub strength: u8,
    pub dexterity: u8,
    pub constitution: u8,
    pub intelligence: u8,
    pub wisdom: u8,
    pub charisma: u8,
}

impl StandardArrayAssignment {
    pub const METHOD_ID: &'static str = "srd-5.1-cc:rule:standard-array";
    pub const VALUES: [u8; 6] = [8, 10, 12, 13, 14, 15];

    pub fn validate(&self) -> HeroResult<()> {
        let mut values = self.values();
        values.sort_unstable();
        if values != Self::VALUES {
            return invalid(
                "ability_assignment",
                "scores must use each of 15, 14, 13, 12, 10, and 8 exactly once",
            );
        }
        Ok(())
    }

    pub const fn values(&self) -> [u8; 6] {
        [
            self.strength,
            self.dexterity,
            self.constitution,
            self.intelligence,
            self.wisdom,
            self.charisma,
        ]
    }

    pub fn base_scores(&self) -> HeroResult<AbilityScores> {
        self.validate()?;
        AbilityScores::new(
            self.strength,
            self.dexterity,
            self.constitution,
            self.intelligence,
            self.wisdom,
            self.charisma,
        )
        .map_err(|_| HeroError::InvalidField {
            field: "ability_assignment",
            reason: "a standard-array value was invalid",
        })
    }

    pub fn human_scores(&self) -> HeroResult<AbilityScores> {
        self.validate()?;
        AbilityScores::new(
            self.strength + 1,
            self.dexterity + 1,
            self.constitution + 1,
            self.intelligence + 1,
            self.wisdom + 1,
            self.charisma + 1,
        )
        .map_err(|_| HeroError::InvalidField {
            field: "ability_assignment",
            reason: "the supported human ancestry adjustment was invalid",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentId {
    SimpleWeapons,
    Longsword,
    LightCrossbow,
    Shield,
    ChainMail,
    LeatherArmor,
    ExplorersPack,
    ScholarsPack,
    Spellbook,
    ArcaneFocus,
}

impl EquipmentId {
    pub const ALL: [Self; 10] = [
        Self::SimpleWeapons,
        Self::Longsword,
        Self::LightCrossbow,
        Self::Shield,
        Self::ChainMail,
        Self::LeatherArmor,
        Self::ExplorersPack,
        Self::ScholarsPack,
        Self::Spellbook,
        Self::ArcaneFocus,
    ];

    pub const fn mechanic_id(self) -> &'static str {
        match self {
            Self::SimpleWeapons => "srd-5.1-cc:equipment:simple-weapons",
            Self::Longsword => "srd-5.1-cc:equipment:longsword",
            Self::LightCrossbow => "srd-5.1-cc:equipment:light-crossbow",
            Self::Shield => "srd-5.1-cc:equipment:shield",
            Self::ChainMail => "srd-5.1-cc:equipment:chain-mail",
            Self::LeatherArmor => "srd-5.1-cc:equipment:leather-armor",
            Self::ExplorersPack => "srd-5.1-cc:equipment:explorers-pack",
            Self::ScholarsPack => "srd-5.1-cc:equipment:scholars-pack",
            Self::Spellbook => "srd-5.1-cc:equipment:spellbook",
            Self::ArcaneFocus => "srd-5.1-cc:equipment:arcane-focus",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimpleWeaponId {
    Club,
    Dagger,
    Greatclub,
    Handaxe,
    Javelin,
    LightHammer,
    Mace,
    Quarterstaff,
    Sickle,
    Spear,
}

impl SimpleWeaponId {
    pub const ALL: [Self; 10] = [
        Self::Club,
        Self::Dagger,
        Self::Greatclub,
        Self::Handaxe,
        Self::Javelin,
        Self::LightHammer,
        Self::Mace,
        Self::Quarterstaff,
        Self::Sickle,
        Self::Spear,
    ];

    const fn wizard_allowed(self) -> bool {
        matches!(self, Self::Dagger | Self::Quarterstaff)
    }

    const fn is_two_handed(self) -> bool {
        matches!(self, Self::Greatclub)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentSelection {
    /// Canonically sorted, duplicate-free content IDs.
    pub carried: Vec<EquipmentId>,
    /// Concrete choice represented by the `simple-weapons` content entry.
    pub simple_weapon: Option<SimpleWeaponId>,
    pub equipped_armor: Option<EquipmentId>,
    pub shield_equipped: bool,
}

impl EquipmentSelection {
    pub fn validate_for(&self, class: &ClassSelection) -> HeroResult<()> {
        validate_sorted_unique("equipment.carried", &self.carried)?;
        if self.carried.len() > EquipmentId::ALL.len() {
            return invalid("equipment.carried", "too many carried equipment entries");
        }

        let has_simple = self.carried.contains(&EquipmentId::SimpleWeapons);
        if has_simple != self.simple_weapon.is_some() {
            return invalid(
                "equipment.simple_weapon",
                "a concrete simple weapon is required exactly when the category is carried",
            );
        }
        if self.carried.contains(&EquipmentId::ChainMail)
            && self.carried.contains(&EquipmentId::LeatherArmor)
        {
            return invalid(
                "equipment.carried",
                "chain mail and leather armor are mutually exclusive starting choices",
            );
        }
        if self.shield_equipped != self.carried.contains(&EquipmentId::Shield) {
            return invalid(
                "equipment.shield_equipped",
                "the starting shield must be carried and equipped together",
            );
        }
        match self.equipped_armor {
            Some(armor @ (EquipmentId::ChainMail | EquipmentId::LeatherArmor))
                if self.carried.contains(&armor) => {}
            None if !self.carried.contains(&EquipmentId::ChainMail)
                && !self.carried.contains(&EquipmentId::LeatherArmor) => {}
            _ => {
                return invalid(
                    "equipment.equipped_armor",
                    "equipped armor must match the one carried armor choice",
                );
            }
        }

        match class {
            ClassSelection::Fighter { fighting_style } => {
                let armor_count = [EquipmentId::ChainMail, EquipmentId::LeatherArmor]
                    .into_iter()
                    .filter(|item| self.carried.contains(item))
                    .count();
                let melee_count = [EquipmentId::SimpleWeapons, EquipmentId::Longsword]
                    .into_iter()
                    .filter(|item| self.carried.contains(item))
                    .count();
                if armor_count != 1
                    || melee_count != 1
                    || !self.carried.contains(&EquipmentId::LightCrossbow)
                    || !self.carried.contains(&EquipmentId::ExplorersPack)
                    || self.carried.contains(&EquipmentId::ScholarsPack)
                    || self.carried.contains(&EquipmentId::Spellbook)
                    || self.carried.contains(&EquipmentId::ArcaneFocus)
                {
                    return invalid(
                        "equipment",
                        "fighter equipment must contain one supported armor, one supported melee choice, a light crossbow, and an explorer's pack",
                    );
                }
                if self
                    .simple_weapon
                    .is_some_and(SimpleWeaponId::is_two_handed)
                    && self.shield_equipped
                {
                    return invalid(
                        "equipment",
                        "a two-handed simple weapon and shield cannot be equipped together",
                    );
                }
                if *fighting_style == FightingStyleId::Dueling
                    && self
                        .simple_weapon
                        .is_some_and(SimpleWeaponId::is_two_handed)
                {
                    return invalid(
                        "class.fighting_style",
                        "dueling requires a supported one-handed melee weapon",
                    );
                }
            }
            ClassSelection::Wizard => {
                let required = [
                    EquipmentId::SimpleWeapons,
                    EquipmentId::ScholarsPack,
                    EquipmentId::Spellbook,
                    EquipmentId::ArcaneFocus,
                ];
                if self.carried.as_slice() != required
                    || self.equipped_armor.is_some()
                    || self.shield_equipped
                    || !self
                        .simple_weapon
                        .is_some_and(SimpleWeaponId::wizard_allowed)
                {
                    return invalid(
                        "equipment",
                        "wizard equipment is one supported wizard simple weapon, a scholar's pack, spellbook, and arcane focus",
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellId {
    FireBolt,
    Light,
    MageHand,
    MagicMissile,
    Shield,
    Sleep,
}

impl SpellId {
    pub const ALL: [Self; 6] = [
        Self::FireBolt,
        Self::Light,
        Self::MageHand,
        Self::MagicMissile,
        Self::Shield,
        Self::Sleep,
    ];
    pub const CANTRIPS: [Self; 3] = [Self::FireBolt, Self::Light, Self::MageHand];
    pub const LEVEL_ONE: [Self; 3] = [Self::MagicMissile, Self::Shield, Self::Sleep];

    pub const fn mechanic_id(self) -> &'static str {
        match self {
            Self::FireBolt => "srd-5.1-cc:spell:fire-bolt",
            Self::Light => "srd-5.1-cc:spell:light",
            Self::MageHand => "srd-5.1-cc:spell:mage-hand",
            Self::MagicMissile => "srd-5.1-cc:spell:magic-missile",
            Self::Shield => "srd-5.1-cc:spell:shield",
            Self::Sleep => "srd-5.1-cc:spell:sleep",
        }
    }

    pub const fn level(self) -> u8 {
        match self {
            Self::FireBolt | Self::Light | Self::MageHand => 0,
            Self::MagicMissile | Self::Shield | Self::Sleep => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WizardSpellSelection {
    pub cantrips: Vec<SpellId>,
    pub spellbook: Vec<SpellId>,
    pub prepared: Vec<SpellId>,
}

impl WizardSpellSelection {
    fn validate_creation(&self, intelligence_modifier: i8) -> HeroResult<()> {
        validate_sorted_unique("wizard_spells.cantrips", &self.cantrips)?;
        validate_sorted_unique("wizard_spells.spellbook", &self.spellbook)?;
        validate_sorted_unique("wizard_spells.prepared", &self.prepared)?;
        if self.cantrips.as_slice() != SpellId::CANTRIPS
            || self.spellbook.as_slice() != SpellId::LEVEL_ONE
        {
            return invalid(
                "wizard_spells",
                "the MVP wizard must record the complete fixed cantrip and spellbook allowlists",
            );
        }
        let prepared_capacity = prepared_spell_capacity(1, intelligence_modifier);
        if self.prepared.len() != usize::from(prepared_capacity)
            || self
                .prepared
                .iter()
                .any(|spell| !self.spellbook.contains(spell))
        {
            return invalid(
                "wizard_spells.prepared",
                "prepared spells must be a unique spellbook subset at the level-one capacity",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundSelection {
    pub background: BackgroundId,
    /// Exactly two canonical class-skill choices after background filtering.
    pub class_skills: Vec<SkillId>,
}

impl BackgroundSelection {
    fn validate_for(&self, class: HeroClass) -> HeroResult<()> {
        validate_sorted_unique("background.class_skills", &self.class_skills)?;
        if self.class_skills.len() != 2 {
            return invalid(
                "background.class_skills",
                "exactly two class skills are required",
            );
        }
        let background_skills = self.background.skill_proficiencies();
        if self.class_skills.iter().any(|skill| {
            !class_skill_choices(class).contains(skill) || background_skills.contains(skill)
        }) {
            return invalid(
                "background.class_skills",
                "class skills must meet class prerequisites and cannot duplicate background skills",
            );
        }
        Ok(())
    }

    fn all_skills(&self) -> Vec<SkillId> {
        let mut skills = self.class_skills.clone();
        skills.extend(self.background.skill_proficiencies());
        skills.sort_unstable();
        skills
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroPresentation {
    pub name: String,
    pub pronouns: String,
    pub appearance: String,
    pub ideal: String,
    pub bond: String,
    pub flaw: String,
    #[serde(default)]
    pub tone_limits: Vec<String>,
}

impl HeroPresentation {
    pub fn validate(&self) -> HeroResult<()> {
        validate_text("presentation.name", &self.name, MAX_NAME_CHARS, true)?;
        validate_text(
            "presentation.pronouns",
            &self.pronouns,
            MAX_PRONOUN_CHARS,
            true,
        )?;
        for (field, value) in [
            ("presentation.appearance", self.appearance.as_str()),
            ("presentation.ideal", self.ideal.as_str()),
            ("presentation.bond", self.bond.as_str()),
            ("presentation.flaw", self.flaw.as_str()),
        ] {
            validate_text(field, value, MAX_PRESENTATION_CHARS, true)?;
        }
        if self.tone_limits.len() > MAX_TONE_LIMITS {
            return invalid("presentation.tone_limits", "too many tone limits");
        }
        let mut unique = BTreeSet::new();
        for limit in &self.tone_limits {
            validate_text(
                "presentation.tone_limits",
                limit,
                MAX_TONE_LIMIT_CHARS,
                true,
            )?;
            if !unique.insert(limit) {
                return invalid("presentation.tone_limits", "tone limits must be unique");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedLevel {
    One,
    Two,
}

impl SupportedLevel {
    pub const fn value(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionCapability {
    Attack,
    CastSupportedSpell,
    Dash,
    Disengage,
    Dodge,
    Help,
    Hide,
    Ready,
    Search,
    UseObject,
    SecondWind,
    ActionSurge,
}

impl ActionCapability {
    pub const CORE: [Self; 10] = [
        Self::Attack,
        Self::CastSupportedSpell,
        Self::Dash,
        Self::Disengage,
        Self::Dodge,
        Self::Help,
        Self::Hide,
        Self::Ready,
        Self::Search,
        Self::UseObject,
    ];

    pub const fn mechanic_id(self) -> &'static str {
        match self {
            Self::Attack => "action.attack",
            Self::CastSupportedSpell => "action.cast-supported-spell",
            Self::Dash => "action.dash",
            Self::Disengage => "action.disengage",
            Self::Dodge => "action.dodge",
            Self::Help => "action.help",
            Self::Hide => "action.hide",
            Self::Ready => "action.ready",
            Self::Search => "action.search",
            Self::UseObject => "action.use-object",
            Self::SecondWind => "action.fighter.second-wind",
            Self::ActionSurge => "action.fighter.action-surge",
        }
    }

    pub fn from_mechanic_id(value: &str) -> std::result::Result<Self, UnsupportedMechanic> {
        let capability = [
            Self::Attack,
            Self::CastSupportedSpell,
            Self::Dash,
            Self::Disengage,
            Self::Dodge,
            Self::Help,
            Self::Hide,
            Self::Ready,
            Self::Search,
            Self::UseObject,
            Self::SecondWind,
            Self::ActionSurge,
        ]
        .into_iter()
        .find(|capability| capability.mechanic_id() == value);
        capability.ok_or_else(|| UnsupportedMechanic::authored_action_alternatives(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionId {
    Prone,
    Restrained,
    Grappled,
    Incapacitated,
    Unconscious,
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageType {
    Bludgeoning,
    Piercing,
    Slashing,
    Fire,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageInteraction {
    Normal,
    Resistance,
    Vulnerability,
    Immunity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestKind {
    Short,
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    HitDiceD6,
    HitDiceD10,
    SecondWind,
    ActionSurge,
    LevelOneSpellSlots,
    ArcaneRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureId {
    FightingStyleDefense,
    FightingStyleDueling,
    SecondWind,
    ActionSurge,
    WizardSpellcasting,
    ArcaneRecovery,
    EvocationTradition,
    SculptSpells,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureAvailability {
    Active,
    /// The feature is recorded for SRD level fidelity, but none of the six
    /// allowlisted spells can trigger it. It must not be offered as an action.
    DormantNoQualifyingSupportedSpell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureSummary {
    pub feature: FeatureId,
    pub availability: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredAlternative {
    pub action: ActionCapability,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedMechanic {
    pub schema_version: u16,
    pub code: UnsupportedMechanicCode,
    pub requested_id: String,
    pub alternatives: Vec<AuthoredAlternative>,
}

impl UnsupportedMechanic {
    fn authored_action_alternatives(requested_id: &str) -> Self {
        let requested_id = if is_valid_opaque_id(requested_id) {
            requested_id.to_owned()
        } else {
            "invalid-requested-id".to_owned()
        };
        Self {
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
        }
    }

    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_UNSUPPORTED_SCHEMA_VERSION)?;
        validate_id("unsupported.requested_id", &self.requested_id)?;
        if self.alternatives.is_empty() || self.alternatives.len() > MAX_UNSUPPORTED_ALTERNATIVES {
            return invalid(
                "unsupported.alternatives",
                "one to eight authored alternatives are required",
            );
        }
        let mut actions = BTreeSet::new();
        for alternative in &self.alternatives {
            if !actions.insert(alternative.action) {
                return invalid(
                    "unsupported.alternatives",
                    "alternative actions must be unique",
                );
            }
            validate_text(
                "unsupported.alternatives.label",
                &alternative.label,
                MAX_TONE_LIMIT_CHARS,
                true,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedMechanicCode {
    OutsideMvpMatrix,
    NotAvailableForHero,
    DormantFeature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbilityModifiers {
    pub strength: i8,
    pub dexterity: i8,
    pub constitution: i8,
    pub intelligence: i8,
    pub wisdom: i8,
    pub charisma: i8,
}

impl AbilityModifiers {
    fn from_scores(scores: &AbilityScores) -> Self {
        Self {
            strength: scores.get(Ability::Strength).modifier(),
            dexterity: scores.get(Ability::Dexterity).modifier(),
            constitution: scores.get(Ability::Constitution).modifier(),
            intelligence: scores.get(Ability::Intelligence).modifier(),
            wisdom: scores.get(Ability::Wisdom).modifier(),
            charisma: scores.get(Ability::Charisma).modifier(),
        }
    }

    pub const fn get(&self, ability: Ability) -> i8 {
        match ability {
            Ability::Strength => self.strength,
            Ability::Dexterity => self.dexterity,
            Ability::Constitution => self.constitution,
            Ability::Intelligence => self.intelligence,
            Ability::Wisdom => self.wisdom,
            Ability::Charisma => self.charisma,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavingThrowSummary {
    pub ability: Ability,
    pub proficient: bool,
    pub modifier: i8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSummary {
    pub skill: SkillId,
    pub proficient: bool,
    pub modifier: i8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassiveValues {
    pub perception: i8,
    pub investigation: i8,
    pub insight: i8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamageDice {
    pub count: u8,
    pub sides: u8,
    pub constant: i8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "weapon", rename_all = "snake_case", deny_unknown_fields)]
pub enum WeaponChoice {
    Simple { kind: SimpleWeaponId },
    Longsword,
    LightCrossbow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttackSummary {
    pub attack_id: String,
    pub weapon: WeaponChoice,
    pub ability: Ability,
    pub attack_bonus: i8,
    pub damage: DamageDice,
    pub damage_type: DamageType,
    pub normal_range_feet: u16,
    pub long_range_feet: Option<u16>,
    pub versatile_damage_sides: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePool {
    pub resource: ResourceKind,
    pub current: u8,
    pub maximum: u8,
    pub recovers_on: RestKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentState {
    pub carried: Vec<EquipmentId>,
    pub simple_weapon: Option<SimpleWeaponId>,
    pub equipped_armor: Option<EquipmentId>,
    pub shield_equipped: bool,
    /// MVP uses a bounded authored loadout rather than a weight simulation.
    pub capacity_policy_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellEffectCapability {
    pub spell: SpellId,
    pub action: SpellActionKind,
    pub target: SpellTargetKind,
    pub range_feet: u16,
    pub duration: SpellDuration,
    pub damage_type: Option<DamageType>,
    pub applies_condition: Option<ConditionId>,
    pub spends_level_one_slot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellActionKind {
    Action,
    Reaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellTargetKind {
    RangedSpellAttack,
    TouchedObject,
    UtilityHand,
    ChosenCreatures,
    SelfOnHitTrigger,
    PointAreaHitPointPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellDuration {
    Instantaneous,
    OneRound,
    OneMinute,
    OneHour,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellcastingSummary {
    pub ability: Ability,
    pub spell_attack_bonus: i8,
    pub spell_save_dc: u8,
    pub cantrips: Vec<SpellId>,
    pub spellbook: Vec<SpellId>,
    pub prepared: Vec<SpellId>,
    pub prepared_capacity: u8,
    pub effects: Vec<SpellEffectCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedHeroSheet {
    pub schema_version: u16,
    pub derivation_id: String,
    pub level: SupportedLevel,
    pub proficiency_bonus: u8,
    pub base_scores: AbilityScores,
    pub ability_scores: AbilityScores,
    pub ability_modifiers: AbilityModifiers,
    pub armor_class: u8,
    pub speed_feet: u8,
    pub maximum_hit_points: u16,
    pub current_hit_points: u16,
    pub saving_throws: Vec<SavingThrowSummary>,
    pub skills: Vec<SkillSummary>,
    pub passive_values: PassiveValues,
    pub attacks: Vec<AttackSummary>,
    pub resources: Vec<ResourcePool>,
    pub equipment: EquipmentState,
    pub spellcasting: Option<SpellcastingSummary>,
    pub features: Vec<FeatureSummary>,
    pub legal_action_capabilities: Vec<ActionCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroChoices {
    pub pins: HeroPins,
    pub concept: HeroConceptId,
    pub ancestry: AncestryId,
    pub class: ClassSelection,
    pub ability_assignment: StandardArrayAssignment,
    pub background: BackgroundSelection,
    pub equipment: EquipmentSelection,
    pub wizard_spells: Option<WizardSpellSelection>,
    pub presentation: HeroPresentation,
}

impl HeroChoices {
    pub fn validate(&self) -> HeroResult<()> {
        self.pins.validate()?;
        if self.ancestry != AncestryId::Human {
            return invalid("ancestry", "only the supported human ancestry is available");
        }
        self.ability_assignment.validate()?;
        self.background.validate_for(self.class.class())?;
        self.equipment.validate_for(&self.class)?;
        self.presentation.validate()?;

        let intelligence_modifier = self
            .ability_assignment
            .human_scores()?
            .get(Ability::Intelligence)
            .modifier();
        match (&self.class, &self.wizard_spells) {
            (ClassSelection::Wizard, Some(spells)) => {
                spells.validate_creation(intelligence_modifier)?;
            }
            (ClassSelection::Wizard, None) => {
                return invalid(
                    "wizard_spells",
                    "wizard creation requires the fixed supported spell selection",
                );
            }
            (ClassSelection::Fighter { .. }, None) => {}
            (ClassSelection::Fighter { .. }, Some(_)) => {
                return invalid(
                    "wizard_spells",
                    "fighter creation cannot carry a wizard spell selection",
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitPointGrowthChoice {
    FixedAverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArcaneTraditionId {
    Evocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum LevelUpChoice {
    Fighter {
        hit_points: HitPointGrowthChoice,
    },
    Wizard {
        hit_points: HitPointGrowthChoice,
        arcane_tradition: ArcaneTraditionId,
    },
}

impl LevelUpChoice {
    pub const fn class(&self) -> HeroClass {
        match self {
            Self::Fighter { .. } => HeroClass::Fighter,
            Self::Wizard { .. } => HeroClass::Wizard,
        }
    }
}

fn derive_sheet(
    choices: &HeroChoices,
    level: SupportedLevel,
    advancement_choices: &[LevelUpChoice],
) -> HeroResult<DerivedHeroSheet> {
    choices.validate()?;
    match level {
        SupportedLevel::One if !advancement_choices.is_empty() => {
            return invalid(
                "advancement_choices",
                "a level-one hero cannot contain level-up choices",
            );
        }
        SupportedLevel::Two => {
            if advancement_choices.len() != 1
                || advancement_choices[0].class() != choices.class.class()
            {
                return invalid(
                    "advancement_choices",
                    "level two requires one explicit class-matching level-up choice",
                );
            }
        }
        SupportedLevel::One => {}
    }

    let base_scores = choices.ability_assignment.base_scores()?;
    let ability_scores = choices.ability_assignment.human_scores()?;
    let modifiers = AbilityModifiers::from_scores(&ability_scores);
    let proficiency_bonus = 2;
    let class = choices.class.class();
    let armor_class = derive_armor_class(&choices.class, &choices.equipment, &modifiers)?;
    let speed_feet = if choices.equipment.equipped_armor == Some(EquipmentId::ChainMail)
        && ability_scores.get(Ability::Strength).value() < 13
    {
        20
    } else {
        30
    };
    let maximum_hit_points = derive_maximum_hit_points(class, level, &modifiers);
    let saving_throws = derive_saving_throws(class, &modifiers, proficiency_bonus);
    let proficient_skills = choices.background.all_skills();
    let skills = SkillId::ALL
        .into_iter()
        .map(|skill| {
            let proficient = proficient_skills.contains(&skill);
            SkillSummary {
                skill,
                proficient,
                modifier: modifiers.get(skill.ability())
                    + if proficient {
                        proficiency_bonus as i8
                    } else {
                        0
                    },
            }
        })
        .collect::<Vec<_>>();
    let passive_values = PassiveValues {
        perception: 10 + skill_modifier(&skills, SkillId::Perception),
        investigation: 10 + skill_modifier(&skills, SkillId::Investigation),
        insight: 10 + skill_modifier(&skills, SkillId::Insight),
    };
    let attacks = derive_attacks(&choices.class, &choices.equipment, &modifiers)?;
    let resources = derive_resources(class, level);
    let equipment = EquipmentState {
        carried: choices.equipment.carried.clone(),
        simple_weapon: choices.equipment.simple_weapon,
        equipped_armor: choices.equipment.equipped_armor,
        shield_equipped: choices.equipment.shield_equipped,
        capacity_policy_id: "manchester-arcana:capacity:authored-starting-loadout-v1".to_owned(),
    };
    let spellcasting = derive_spellcasting(choices, level, &modifiers, proficiency_bonus)?;
    let features = derive_features(&choices.class, level);
    let mut legal_action_capabilities = ActionCapability::CORE.to_vec();
    if class == HeroClass::Fighter {
        // Fighters do not receive a spell action from the Q04 class path.
        legal_action_capabilities.retain(|action| *action != ActionCapability::CastSupportedSpell);
        legal_action_capabilities.push(ActionCapability::SecondWind);
        if level == SupportedLevel::Two {
            legal_action_capabilities.push(ActionCapability::ActionSurge);
        }
    }

    Ok(DerivedHeroSheet {
        schema_version: HERO_DERIVED_SHEET_SCHEMA_VERSION,
        derivation_id: HERO_DERIVATION_ID.to_owned(),
        level,
        proficiency_bonus,
        base_scores,
        ability_scores,
        ability_modifiers: modifiers,
        armor_class,
        speed_feet,
        maximum_hit_points,
        current_hit_points: maximum_hit_points,
        saving_throws,
        skills,
        passive_values,
        attacks,
        resources,
        equipment,
        spellcasting,
        features,
        legal_action_capabilities,
    })
}

fn derive_armor_class(
    class: &ClassSelection,
    equipment: &EquipmentSelection,
    modifiers: &AbilityModifiers,
) -> HeroResult<u8> {
    equipment.validate_for(class)?;
    let mut armor_class = match equipment.equipped_armor {
        Some(EquipmentId::ChainMail) => 16,
        Some(EquipmentId::LeatherArmor) => 11_i16 + i16::from(modifiers.dexterity),
        None => 10_i16 + i16::from(modifiers.dexterity),
        Some(_) => {
            return invalid(
                "equipment.equipped_armor",
                "the selected item is not supported armor",
            );
        }
    };
    if equipment.shield_equipped {
        armor_class += 2;
    }
    if matches!(
        class,
        ClassSelection::Fighter {
            fighting_style: FightingStyleId::Defense
        }
    ) {
        armor_class += 1;
    }
    u8::try_from(armor_class).map_err(|_| HeroError::InvalidField {
        field: "armor_class",
        reason: "derived armor class was outside the supported range",
    })
}

fn derive_maximum_hit_points(
    class: HeroClass,
    level: SupportedLevel,
    modifiers: &AbilityModifiers,
) -> u16 {
    let constitution = i16::from(modifiers.constitution);
    let level_one = (i16::from(class.hit_die_sides()) + constitution).max(1);
    let level_two = match class {
        HeroClass::Fighter => 6 + constitution,
        HeroClass::Wizard => 4 + constitution,
    }
    .max(1);
    u16::try_from(
        level_one
            + if level == SupportedLevel::Two {
                level_two
            } else {
                0
            },
    )
    .expect("bounded level-one-to-two hit points fit u16")
}

fn derive_saving_throws(
    class: HeroClass,
    modifiers: &AbilityModifiers,
    proficiency_bonus: u8,
) -> Vec<SavingThrowSummary> {
    let proficient = match class {
        HeroClass::Fighter => [Ability::Strength, Ability::Constitution],
        HeroClass::Wizard => [Ability::Intelligence, Ability::Wisdom],
    };
    Ability::ALL
        .into_iter()
        .map(|ability| {
            let is_proficient = proficient.contains(&ability);
            SavingThrowSummary {
                ability,
                proficient: is_proficient,
                modifier: modifiers.get(ability)
                    + if is_proficient {
                        proficiency_bonus as i8
                    } else {
                        0
                    },
            }
        })
        .collect()
}

fn derive_attacks(
    class: &ClassSelection,
    equipment: &EquipmentSelection,
    modifiers: &AbilityModifiers,
) -> HeroResult<Vec<AttackSummary>> {
    let proficiency_bonus = 2_i8;
    let dueling = matches!(
        class,
        ClassSelection::Fighter {
            fighting_style: FightingStyleId::Dueling
        }
    );
    let mut attacks = Vec::new();
    if let Some(simple_weapon) = equipment.simple_weapon {
        attacks.push(simple_weapon_attack(
            simple_weapon,
            modifiers,
            proficiency_bonus,
            dueling,
        ));
    }
    if equipment.carried.contains(&EquipmentId::Longsword) {
        let use_two_hands = !equipment.shield_equipped && !dueling;
        attacks.push(AttackSummary {
            attack_id: "attack:longsword".to_owned(),
            weapon: WeaponChoice::Longsword,
            ability: Ability::Strength,
            attack_bonus: modifiers.strength + proficiency_bonus,
            damage: DamageDice {
                count: 1,
                sides: if use_two_hands { 10 } else { 8 },
                constant: modifiers.strength + if dueling { 2 } else { 0 },
            },
            damage_type: DamageType::Slashing,
            normal_range_feet: 5,
            long_range_feet: None,
            versatile_damage_sides: Some(10),
        });
    }
    if equipment.carried.contains(&EquipmentId::LightCrossbow) {
        attacks.push(AttackSummary {
            attack_id: "attack:light-crossbow".to_owned(),
            weapon: WeaponChoice::LightCrossbow,
            ability: Ability::Dexterity,
            attack_bonus: modifiers.dexterity + proficiency_bonus,
            damage: DamageDice {
                count: 1,
                sides: 8,
                constant: modifiers.dexterity,
            },
            damage_type: DamageType::Piercing,
            normal_range_feet: 80,
            long_range_feet: Some(320),
            versatile_damage_sides: None,
        });
    }
    if attacks.is_empty() {
        return invalid(
            "attacks",
            "the supported loadout must derive at least one attack",
        );
    }
    Ok(attacks)
}

fn simple_weapon_attack(
    weapon: SimpleWeaponId,
    modifiers: &AbilityModifiers,
    proficiency_bonus: i8,
    dueling: bool,
) -> AttackSummary {
    let (sides, damage_type, finesse, normal_range, long_range, versatile) = match weapon {
        SimpleWeaponId::Club => (4, DamageType::Bludgeoning, false, 5, None, None),
        SimpleWeaponId::Dagger => (4, DamageType::Piercing, true, 5, Some(60), None),
        SimpleWeaponId::Greatclub => (8, DamageType::Bludgeoning, false, 5, None, None),
        SimpleWeaponId::Handaxe => (6, DamageType::Slashing, false, 5, Some(60), None),
        SimpleWeaponId::Javelin => (6, DamageType::Piercing, false, 5, Some(120), None),
        SimpleWeaponId::LightHammer => (4, DamageType::Bludgeoning, false, 5, Some(60), None),
        SimpleWeaponId::Mace => (6, DamageType::Bludgeoning, false, 5, None, None),
        SimpleWeaponId::Quarterstaff => (6, DamageType::Bludgeoning, false, 5, None, Some(8)),
        SimpleWeaponId::Sickle => (4, DamageType::Slashing, false, 5, None, None),
        SimpleWeaponId::Spear => (6, DamageType::Piercing, false, 5, Some(60), Some(8)),
    };
    let ability = if finesse && modifiers.dexterity > modifiers.strength {
        Ability::Dexterity
    } else {
        Ability::Strength
    };
    let ability_modifier = modifiers.get(ability);
    AttackSummary {
        attack_id: format!("attack:simple:{weapon:?}").to_ascii_lowercase(),
        weapon: WeaponChoice::Simple { kind: weapon },
        ability,
        attack_bonus: ability_modifier + proficiency_bonus,
        damage: DamageDice {
            count: 1,
            sides,
            constant: ability_modifier
                + if dueling && !weapon.is_two_handed() {
                    2
                } else {
                    0
                },
        },
        damage_type,
        normal_range_feet: normal_range,
        long_range_feet: long_range,
        versatile_damage_sides: versatile,
    }
}

fn derive_resources(class: HeroClass, level: SupportedLevel) -> Vec<ResourcePool> {
    let hit_dice = ResourcePool {
        resource: match class {
            HeroClass::Fighter => ResourceKind::HitDiceD10,
            HeroClass::Wizard => ResourceKind::HitDiceD6,
        },
        current: level.value(),
        maximum: level.value(),
        recovers_on: RestKind::Long,
    };
    match class {
        HeroClass::Fighter => {
            let mut resources = vec![
                hit_dice,
                ResourcePool {
                    resource: ResourceKind::SecondWind,
                    current: 1,
                    maximum: 1,
                    recovers_on: RestKind::Short,
                },
            ];
            if level == SupportedLevel::Two {
                resources.push(ResourcePool {
                    resource: ResourceKind::ActionSurge,
                    current: 1,
                    maximum: 1,
                    recovers_on: RestKind::Short,
                });
            }
            resources
        }
        HeroClass::Wizard => vec![
            hit_dice,
            ResourcePool {
                resource: ResourceKind::LevelOneSpellSlots,
                current: if level == SupportedLevel::One { 2 } else { 3 },
                maximum: if level == SupportedLevel::One { 2 } else { 3 },
                recovers_on: RestKind::Long,
            },
            ResourcePool {
                resource: ResourceKind::ArcaneRecovery,
                current: 1,
                maximum: 1,
                recovers_on: RestKind::Long,
            },
        ],
    }
}

fn derive_spellcasting(
    choices: &HeroChoices,
    level: SupportedLevel,
    modifiers: &AbilityModifiers,
    proficiency_bonus: u8,
) -> HeroResult<Option<SpellcastingSummary>> {
    let Some(selection) = &choices.wizard_spells else {
        return Ok(None);
    };
    if choices.class.class() != HeroClass::Wizard {
        return invalid("wizard_spells", "only a wizard can derive spellcasting");
    }
    let prepared_capacity = prepared_spell_capacity(level.value(), modifiers.intelligence);
    if selection.prepared.len() > usize::from(prepared_capacity) {
        return invalid(
            "wizard_spells.prepared",
            "stored prepared spells exceed the current capacity",
        );
    }
    let spell_attack_bonus = modifiers.intelligence + proficiency_bonus as i8;
    let save_dc = 8_i16 + i16::from(modifiers.intelligence) + i16::from(proficiency_bonus);
    let spell_save_dc = u8::try_from(save_dc).map_err(|_| HeroError::InvalidField {
        field: "spell_save_dc",
        reason: "derived spell save DC was outside the supported range",
    })?;
    Ok(Some(SpellcastingSummary {
        ability: Ability::Intelligence,
        spell_attack_bonus,
        spell_save_dc,
        cantrips: selection.cantrips.clone(),
        spellbook: selection.spellbook.clone(),
        prepared: selection.prepared.clone(),
        prepared_capacity,
        effects: SpellId::ALL
            .into_iter()
            .map(spell_effect_capability)
            .collect(),
    }))
}

fn spell_effect_capability(spell: SpellId) -> SpellEffectCapability {
    match spell {
        SpellId::FireBolt => SpellEffectCapability {
            spell,
            action: SpellActionKind::Action,
            target: SpellTargetKind::RangedSpellAttack,
            range_feet: 120,
            duration: SpellDuration::Instantaneous,
            damage_type: Some(DamageType::Fire),
            applies_condition: None,
            spends_level_one_slot: false,
        },
        SpellId::Light => SpellEffectCapability {
            spell,
            action: SpellActionKind::Action,
            target: SpellTargetKind::TouchedObject,
            range_feet: 0,
            duration: SpellDuration::OneHour,
            damage_type: None,
            applies_condition: None,
            spends_level_one_slot: false,
        },
        SpellId::MageHand => SpellEffectCapability {
            spell,
            action: SpellActionKind::Action,
            target: SpellTargetKind::UtilityHand,
            range_feet: 30,
            duration: SpellDuration::OneMinute,
            damage_type: None,
            applies_condition: None,
            spends_level_one_slot: false,
        },
        SpellId::MagicMissile => SpellEffectCapability {
            spell,
            action: SpellActionKind::Action,
            target: SpellTargetKind::ChosenCreatures,
            range_feet: 120,
            duration: SpellDuration::Instantaneous,
            damage_type: Some(DamageType::Force),
            applies_condition: None,
            spends_level_one_slot: true,
        },
        SpellId::Shield => SpellEffectCapability {
            spell,
            action: SpellActionKind::Reaction,
            target: SpellTargetKind::SelfOnHitTrigger,
            range_feet: 0,
            duration: SpellDuration::OneRound,
            damage_type: None,
            applies_condition: None,
            spends_level_one_slot: true,
        },
        SpellId::Sleep => SpellEffectCapability {
            spell,
            action: SpellActionKind::Action,
            target: SpellTargetKind::PointAreaHitPointPool,
            range_feet: 90,
            duration: SpellDuration::OneMinute,
            damage_type: None,
            applies_condition: Some(ConditionId::Unconscious),
            spends_level_one_slot: true,
        },
    }
}

fn derive_features(class: &ClassSelection, level: SupportedLevel) -> Vec<FeatureSummary> {
    match class {
        ClassSelection::Fighter { fighting_style } => {
            let mut features = vec![
                FeatureSummary {
                    feature: match fighting_style {
                        FightingStyleId::Defense => FeatureId::FightingStyleDefense,
                        FightingStyleId::Dueling => FeatureId::FightingStyleDueling,
                    },
                    availability: FeatureAvailability::Active,
                },
                FeatureSummary {
                    feature: FeatureId::SecondWind,
                    availability: FeatureAvailability::Active,
                },
            ];
            if level == SupportedLevel::Two {
                features.push(FeatureSummary {
                    feature: FeatureId::ActionSurge,
                    availability: FeatureAvailability::Active,
                });
            }
            features
        }
        ClassSelection::Wizard => {
            let mut features = vec![
                FeatureSummary {
                    feature: FeatureId::WizardSpellcasting,
                    availability: FeatureAvailability::Active,
                },
                FeatureSummary {
                    feature: FeatureId::ArcaneRecovery,
                    availability: FeatureAvailability::Active,
                },
            ];
            if level == SupportedLevel::Two {
                features.extend([
                    FeatureSummary {
                        feature: FeatureId::EvocationTradition,
                        availability: FeatureAvailability::Active,
                    },
                    FeatureSummary {
                        feature: FeatureId::SculptSpells,
                        availability: FeatureAvailability::DormantNoQualifyingSupportedSpell,
                    },
                ]);
            }
            features
        }
    }
}

fn prepared_spell_capacity(level: u8, intelligence_modifier: i8) -> u8 {
    let rules_capacity = (i16::from(level) + i16::from(intelligence_modifier)).max(1);
    u8::try_from(rules_capacity.min(SpellId::LEVEL_ONE.len() as i16))
        .expect("bounded prepared capacity fits u8")
}

fn skill_modifier(skills: &[SkillSummary], wanted: SkillId) -> i8 {
    skills
        .iter()
        .find(|skill| skill.skill == wanted)
        .map_or(0, |skill| skill.modifier)
}

fn class_skill_choices(class: HeroClass) -> &'static [SkillId] {
    match class {
        HeroClass::Fighter => &[
            SkillId::Acrobatics,
            SkillId::AnimalHandling,
            SkillId::Athletics,
            SkillId::History,
            SkillId::Insight,
            SkillId::Intimidation,
            SkillId::Perception,
            SkillId::Survival,
        ],
        HeroClass::Wizard => &[
            SkillId::Arcana,
            SkillId::History,
            SkillId::Insight,
            SkillId::Investigation,
            SkillId::Medicine,
            SkillId::Religion,
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedRewardPolicy {
    MvpXpV1,
}

impl TrustedRewardPolicy {
    pub const fn experience_for(self, tier: RewardTier) -> u32 {
        match (self, tier) {
            (Self::MvpXpV1, RewardTier::Minor) => 50,
            (Self::MvpXpV1, RewardTier::Significant) => 150,
            (Self::MvpXpV1, RewardTier::Major) => LEVEL_TWO_XP,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedMutationContext {
    pub audit_id: String,
    pub actor_id: String,
    pub occurred_at_epoch_seconds: u64,
}

impl TrustedMutationContext {
    fn validate(&self) -> HeroResult<()> {
        validate_id("audit_id", &self.audit_id)?;
        validate_id("actor_id", &self.actor_id)?;
        if self.occurred_at_epoch_seconds == 0 {
            return invalid("occurred_at", "trusted audit time must be non-zero");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewardAwardCommand {
    pub schema_version: u16,
    pub character_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    /// Closed narrative tier only; no client/model XP amount exists in this API.
    pub tier: RewardTier,
}

impl RewardAwardCommand {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_COMMAND_SCHEMA_VERSION)?;
        validate_id("character_id", &self.character_id)?;
        validate_id("idempotency_key", &self.idempotency_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelUpCommand {
    pub schema_version: u16,
    pub character_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub choice: LevelUpChoice,
}

impl LevelUpCommand {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_COMMAND_SCHEMA_VERSION)?;
        validate_id("character_id", &self.character_id)?;
        validate_id("idempotency_key", &self.idempotency_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeroCharacter {
    pub schema_version: u16,
    pub character_id: String,
    pub campaign_id: String,
    pub owner_id: String,
    pub revision: u64,
    pub level: SupportedLevel,
    pub experience_points: u32,
    pub choices: HeroChoices,
    pub advancement_choices: Vec<LevelUpChoice>,
    pub sheet: DerivedHeroSheet,
}

impl HeroCharacter {
    pub fn create(
        character_id: String,
        campaign_id: String,
        owner_id: String,
        choices: HeroChoices,
    ) -> HeroResult<Self> {
        validate_id("character_id", &character_id)?;
        validate_id("campaign_id", &campaign_id)?;
        validate_id("owner_id", &owner_id)?;
        choices.validate()?;
        let sheet = derive_sheet(&choices, SupportedLevel::One, &[])?;
        let character = Self {
            schema_version: HERO_CHARACTER_SCHEMA_VERSION,
            character_id,
            campaign_id,
            owner_id,
            revision: 0,
            level: SupportedLevel::One,
            experience_points: 0,
            choices,
            advancement_choices: Vec::new(),
            sheet,
        };
        character.validate()?;
        Ok(character)
    }

    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_CHARACTER_SCHEMA_VERSION)?;
        validate_id("character_id", &self.character_id)?;
        validate_id("campaign_id", &self.campaign_id)?;
        validate_id("owner_id", &self.owner_id)?;
        self.choices.validate()?;
        match self.level {
            SupportedLevel::One if !self.advancement_choices.is_empty() => {
                return invalid(
                    "advancement_choices",
                    "level-one state cannot contain advancement choices",
                );
            }
            SupportedLevel::Two
                if self.experience_points < LEVEL_TWO_XP
                    || self.advancement_choices.len() != 1
                    || self.advancement_choices[0].class() != self.choices.class.class() =>
            {
                return invalid(
                    "advancement_choices",
                    "level-two state requires eligibility and one matching explicit choice",
                );
            }
            _ => {}
        }
        if self.experience_points >= 900 {
            return invalid(
                "experience_points",
                "level-three eligibility is outside the level-one-to-two MVP",
            );
        }
        let expected = derive_sheet(&self.choices, self.level, &self.advancement_choices)?;
        let mut normalized = self.sheet.clone();
        if normalized.current_hit_points > normalized.maximum_hit_points {
            return invalid(
                "sheet.current_hit_points",
                "current hit points cannot exceed the derived maximum",
            );
        }
        normalized.current_hit_points = expected.current_hit_points;
        if normalized.resources.len() != expected.resources.len() {
            return invalid(
                "sheet.resources",
                "runtime resources do not match the pinned class and level",
            );
        }
        for resource in &mut normalized.resources {
            if resource.current > resource.maximum {
                return invalid(
                    "sheet.resources.current",
                    "a resource current value cannot exceed its derived maximum",
                );
            }
            let Some(expected_resource) = expected
                .resources
                .iter()
                .find(|candidate| candidate.resource == resource.resource)
            else {
                return invalid(
                    "sheet.resources",
                    "runtime resources do not match the pinned class and level",
                );
            };
            resource.current = expected_resource.current;
        }
        if normalized != expected {
            return invalid(
                "sheet",
                "stored derived state outside current HP/resources does not match the pinned explicit choices",
            );
        }
        Ok(())
    }

    /// Applies encounter-owned current HP and resource counters without allowing
    /// the caller to rewrite maxima, class features, choices, or other derived state.
    pub fn synchronize_encounter_runtime(
        &mut self,
        current_hit_points: u16,
        resource_currents: &[(ResourceKind, u8)],
    ) -> HeroResult<()> {
        self.validate()?;
        if current_hit_points > self.sheet.maximum_hit_points {
            return invalid(
                "current_hit_points",
                "encounter hit points cannot exceed the hero maximum",
            );
        }
        if resource_currents.len() != self.sheet.resources.len() {
            return invalid(
                "resource_currents",
                "encounter resources must contain every authoritative hero pool exactly once",
            );
        }
        let mut seen = BTreeSet::new();
        for (kind, current) in resource_currents {
            if !seen.insert(*kind) {
                return invalid(
                    "resource_currents",
                    "encounter resources cannot contain duplicate pools",
                );
            }
            let Some(pool) = self
                .sheet
                .resources
                .iter_mut()
                .find(|pool| pool.resource == *kind)
            else {
                return invalid(
                    "resource_currents",
                    "encounter resources cannot introduce a new hero pool",
                );
            };
            if *current > pool.maximum {
                return invalid(
                    "resource_currents",
                    "encounter resource current cannot exceed the derived maximum",
                );
            }
            pool.current = *current;
        }
        self.sheet.current_hit_points = current_hit_points;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(HeroError::InvalidField {
                field: "revision",
                reason: "revision overflowed",
            })?;
        self.validate()
    }

    pub fn level_up_eligible(&self) -> bool {
        self.level == SupportedLevel::One && self.experience_points >= LEVEL_TWO_XP
    }

    pub fn valid_level_up_choices(&self) -> HeroResult<Vec<LevelUpChoice>> {
        self.validate()?;
        if !self.level_up_eligible() {
            return Ok(Vec::new());
        }
        Ok(vec![match self.choices.class.class() {
            HeroClass::Fighter => LevelUpChoice::Fighter {
                hit_points: HitPointGrowthChoice::FixedAverage,
            },
            HeroClass::Wizard => LevelUpChoice::Wizard {
                hit_points: HitPointGrowthChoice::FixedAverage,
                arcane_tradition: ArcaneTraditionId::Evocation,
            },
        }])
    }

    pub fn apply_reward(
        &mut self,
        command: &RewardAwardCommand,
        policy: TrustedRewardPolicy,
        context: &TrustedMutationContext,
    ) -> HeroResult<RewardAwardAuditDto> {
        self.validate()?;
        command.validate()?;
        context.validate()?;
        if command.character_id != self.character_id {
            return invalid("character_id", "command targets a different character");
        }
        check_revision(self.revision, command.expected_revision)?;
        let awarded = policy.experience_for(command.tier);
        let experience_after =
            self.experience_points
                .checked_add(awarded)
                .ok_or(HeroError::InvalidField {
                    field: "experience_points",
                    reason: "experience point total overflowed",
                })?;
        if experience_after >= 900 {
            let unsupported = UnsupportedMechanic {
                schema_version: HERO_UNSUPPORTED_SCHEMA_VERSION,
                code: UnsupportedMechanicCode::NotAvailableForHero,
                requested_id: "advancement.level-three".to_owned(),
                alternatives: vec![AuthoredAlternative {
                    action: ActionCapability::Search,
                    label: "Continue the story without an unsupported level gain".to_owned(),
                }],
            };
            unsupported.validate()?;
            return Err(HeroError::UnsupportedMechanic(unsupported));
        }

        let revision_before = self.revision;
        let experience_before = self.experience_points;
        let mut candidate = self.clone();
        candidate.experience_points = experience_after;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(HeroError::InvalidField {
                field: "revision",
                reason: "revision overflowed",
            })?;
        candidate.validate()?;
        let audit = RewardAwardAuditDto {
            schema_version: HERO_AUDIT_SCHEMA_VERSION,
            audit_id: context.audit_id.clone(),
            actor_id: context.actor_id.clone(),
            character_id: self.character_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            revision_before,
            revision_after: candidate.revision,
            policy,
            tier: command.tier,
            experience_awarded: awarded,
            experience_before,
            experience_after,
            level: candidate.level,
            level_up_eligible: candidate.level_up_eligible(),
            occurred_at_epoch_seconds: context.occurred_at_epoch_seconds,
        };
        audit.validate()?;
        *self = candidate;
        Ok(audit)
    }

    pub fn level_up(
        &mut self,
        command: &LevelUpCommand,
        context: &TrustedMutationContext,
    ) -> HeroResult<LevelUpAuditDto> {
        self.validate()?;
        command.validate()?;
        context.validate()?;
        if command.character_id != self.character_id {
            return invalid("character_id", "command targets a different character");
        }
        check_revision(self.revision, command.expected_revision)?;
        if !self.level_up_eligible() {
            return invalid(
                "level",
                "hero is not eligible for the supported level-one-to-two transition",
            );
        }
        let valid_choices = self.valid_level_up_choices()?;
        if !valid_choices.contains(&command.choice) {
            return invalid(
                "level_up.choice",
                "choice does not match the pinned class and supported path",
            );
        }

        let revision_before = self.revision;
        let hit_points_before = self.sheet.maximum_hit_points;
        let resources_before = self.sheet.resources.clone();
        let mut candidate = self.clone();
        candidate.level = SupportedLevel::Two;
        candidate.advancement_choices = vec![command.choice.clone()];
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(HeroError::InvalidField {
                field: "revision",
                reason: "revision overflowed",
            })?;
        let mut advanced_sheet = derive_sheet(
            &candidate.choices,
            candidate.level,
            &candidate.advancement_choices,
        )?;
        let hit_point_increase = advanced_sheet
            .maximum_hit_points
            .checked_sub(self.sheet.maximum_hit_points)
            .ok_or(HeroError::InvalidField {
                field: "sheet.maximum_hit_points",
                reason: "level-up cannot reduce maximum hit points",
            })?;
        advanced_sheet.current_hit_points = self
            .sheet
            .current_hit_points
            .checked_add(hit_point_increase)
            .ok_or(HeroError::InvalidField {
                field: "sheet.current_hit_points",
                reason: "level-up hit points overflowed",
            })?
            .min(advanced_sheet.maximum_hit_points);
        for advanced in &mut advanced_sheet.resources {
            if let Some(previous) = self
                .sheet
                .resources
                .iter()
                .find(|pool| pool.resource == advanced.resource)
            {
                let capacity_increase = advanced.maximum.saturating_sub(previous.maximum);
                advanced.current = previous
                    .current
                    .saturating_add(capacity_increase)
                    .min(advanced.maximum);
            }
        }
        candidate.sheet = advanced_sheet;
        candidate.validate()?;
        let audit = LevelUpAuditDto {
            schema_version: HERO_AUDIT_SCHEMA_VERSION,
            audit_id: context.audit_id.clone(),
            actor_id: context.actor_id.clone(),
            character_id: self.character_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            revision_before,
            revision_after: candidate.revision,
            level_before: SupportedLevel::One,
            level_after: SupportedLevel::Two,
            choice: command.choice.clone(),
            maximum_hit_points_before: hit_points_before,
            maximum_hit_points_after: candidate.sheet.maximum_hit_points,
            resources_before,
            resources_after: candidate.sheet.resources.clone(),
            occurred_at_epoch_seconds: context.occurred_at_epoch_seconds,
        };
        audit.validate()?;
        *self = candidate;
        Ok(audit)
    }
}

impl<'de> Deserialize<'de> for HeroCharacter {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: u16,
            character_id: String,
            campaign_id: String,
            owner_id: String,
            revision: u64,
            level: SupportedLevel,
            experience_points: u32,
            choices: HeroChoices,
            advancement_choices: Vec<LevelUpChoice>,
            sheet: DerivedHeroSheet,
        }
        let wire = Wire::deserialize(deserializer)?;
        let character = Self {
            schema_version: wire.schema_version,
            character_id: wire.character_id,
            campaign_id: wire.campaign_id,
            owner_id: wire.owner_id,
            revision: wire.revision,
            level: wire.level,
            experience_points: wire.experience_points,
            choices: wire.choices,
            advancement_choices: wire.advancement_choices,
            sheet: wire.sheet,
        };
        character.validate().map_err(D::Error::custom)?;
        Ok(character)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewardAwardAuditDto {
    pub schema_version: u16,
    pub audit_id: String,
    pub actor_id: String,
    pub character_id: String,
    pub idempotency_key: String,
    pub revision_before: u64,
    pub revision_after: u64,
    pub policy: TrustedRewardPolicy,
    pub tier: RewardTier,
    pub experience_awarded: u32,
    pub experience_before: u32,
    pub experience_after: u32,
    pub level: SupportedLevel,
    pub level_up_eligible: bool,
    pub occurred_at_epoch_seconds: u64,
}

impl RewardAwardAuditDto {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_AUDIT_SCHEMA_VERSION)?;
        validate_id("audit_id", &self.audit_id)?;
        validate_id("actor_id", &self.actor_id)?;
        validate_id("character_id", &self.character_id)?;
        validate_id("idempotency_key", &self.idempotency_key)?;
        if self.revision_before.checked_add(1) != Some(self.revision_after)
            || self.experience_before.checked_add(self.experience_awarded)
                != Some(self.experience_after)
            || self.experience_awarded != self.policy.experience_for(self.tier)
            || self.level_up_eligible
                != (self.level == SupportedLevel::One && self.experience_after >= LEVEL_TWO_XP)
            || self.occurred_at_epoch_seconds == 0
        {
            return invalid(
                "reward_audit",
                "immutable reward facts do not recompute under the trusted policy",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelUpAuditDto {
    pub schema_version: u16,
    pub audit_id: String,
    pub actor_id: String,
    pub character_id: String,
    pub idempotency_key: String,
    pub revision_before: u64,
    pub revision_after: u64,
    pub level_before: SupportedLevel,
    pub level_after: SupportedLevel,
    pub choice: LevelUpChoice,
    pub maximum_hit_points_before: u16,
    pub maximum_hit_points_after: u16,
    pub resources_before: Vec<ResourcePool>,
    pub resources_after: Vec<ResourcePool>,
    pub occurred_at_epoch_seconds: u64,
}

impl LevelUpAuditDto {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_AUDIT_SCHEMA_VERSION)?;
        validate_id("audit_id", &self.audit_id)?;
        validate_id("actor_id", &self.actor_id)?;
        validate_id("character_id", &self.character_id)?;
        validate_id("idempotency_key", &self.idempotency_key)?;
        if self.revision_before.checked_add(1) != Some(self.revision_after)
            || self.level_before != SupportedLevel::One
            || self.level_after != SupportedLevel::Two
            || self.maximum_hit_points_after <= self.maximum_hit_points_before
            || self.resources_before == self.resources_after
            || self.occurred_at_epoch_seconds == 0
        {
            return invalid(
                "level_up_audit",
                "immutable level-up facts do not describe one valid transition",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterCreatedAuditDto {
    pub schema_version: u16,
    pub audit_id: String,
    pub actor_id: String,
    pub draft_id: String,
    pub character_id: String,
    pub campaign_id: String,
    pub draft_revision: u64,
    pub choices: HeroChoices,
    pub derived_sheet: DerivedHeroSheet,
    pub occurred_at_epoch_seconds: u64,
}

impl CharacterCreatedAuditDto {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_AUDIT_SCHEMA_VERSION)?;
        validate_id("audit_id", &self.audit_id)?;
        validate_id("actor_id", &self.actor_id)?;
        validate_id("draft_id", &self.draft_id)?;
        validate_id("character_id", &self.character_id)?;
        validate_id("campaign_id", &self.campaign_id)?;
        self.choices.validate()?;
        if self.derived_sheet != derive_sheet(&self.choices, SupportedLevel::One, &[])?
            || self.occurred_at_epoch_seconds == 0
        {
            return invalid(
                "character_created_audit",
                "created-character choices and derived sheet do not recompute",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationStep {
    CampaignTheme,
    Concept,
    Rules,
    AbilityScores,
    Background,
    EquipmentAndSpells,
    Presentation,
    Review,
    Commit,
    Committed,
}

impl CreationStep {
    const fn ordinal(self) -> u8 {
        match self {
            Self::CampaignTheme => 0,
            Self::Concept => 1,
            Self::Rules => 2,
            Self::AbilityScores => 3,
            Self::Background => 4,
            Self::EquipmentAndSpells => 5,
            Self::Presentation => 6,
            Self::Review => 7,
            Self::Commit => 8,
            Self::Committed => 9,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HeroCreationIntent {
    SelectCampaignTheme {
        pins: HeroPins,
    },
    SelectConcept {
        concept: HeroConceptId,
    },
    SelectRules {
        ancestry: AncestryId,
        class: ClassSelection,
    },
    AssignAbilities {
        assignment: StandardArrayAssignment,
    },
    SelectBackground {
        selection: BackgroundSelection,
    },
    SelectEquipmentAndSpells {
        equipment: EquipmentSelection,
        wizard_spells: Option<WizardSpellSelection>,
    },
    SetPresentation {
        presentation: HeroPresentation,
    },
    Review,
    Commit {
        character_id: String,
    },
}

impl HeroCreationIntent {
    const fn required_step(&self) -> CreationStep {
        match self {
            Self::SelectCampaignTheme { .. } => CreationStep::CampaignTheme,
            Self::SelectConcept { .. } => CreationStep::Concept,
            Self::SelectRules { .. } => CreationStep::Rules,
            Self::AssignAbilities { .. } => CreationStep::AbilityScores,
            Self::SelectBackground { .. } => CreationStep::Background,
            Self::SelectEquipmentAndSpells { .. } => CreationStep::EquipmentAndSpells,
            Self::SetPresentation { .. } => CreationStep::Presentation,
            Self::Review => CreationStep::Review,
            Self::Commit { .. } => CreationStep::Commit,
        }
    }

    const fn action(&self) -> HeroCreationAction {
        match self {
            Self::SelectCampaignTheme { .. } => HeroCreationAction::CampaignThemeSelected,
            Self::SelectConcept { .. } => HeroCreationAction::ConceptSelected,
            Self::SelectRules { .. } => HeroCreationAction::RulesSelected,
            Self::AssignAbilities { .. } => HeroCreationAction::AbilitiesAssigned,
            Self::SelectBackground { .. } => HeroCreationAction::BackgroundSelected,
            Self::SelectEquipmentAndSpells { .. } => HeroCreationAction::EquipmentAndSpellsSelected,
            Self::SetPresentation { .. } => HeroCreationAction::PresentationSet,
            Self::Review => HeroCreationAction::Reviewed,
            Self::Commit { .. } => HeroCreationAction::Committed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeroCreationCommand {
    pub schema_version: u16,
    pub draft_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub intent: HeroCreationIntent,
}

impl HeroCreationCommand {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_COMMAND_SCHEMA_VERSION)?;
        validate_id("draft_id", &self.draft_id)?;
        validate_id("idempotency_key", &self.idempotency_key)?;
        match &self.intent {
            HeroCreationIntent::SelectCampaignTheme { pins } => pins.validate(),
            HeroCreationIntent::AssignAbilities { assignment } => assignment.validate(),
            HeroCreationIntent::SetPresentation { presentation } => presentation.validate(),
            HeroCreationIntent::Commit { character_id } => {
                validate_id("character_id", character_id)
            }
            _ => Ok(()),
        }
    }
}

impl<'de> Deserialize<'de> for HeroCreationCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: u16,
            draft_id: String,
            expected_revision: u64,
            idempotency_key: String,
            intent: HeroCreationIntent,
        }
        let wire = Wire::deserialize(deserializer)?;
        let command = Self {
            schema_version: wire.schema_version,
            draft_id: wire.draft_id,
            expected_revision: wire.expected_revision,
            idempotency_key: wire.idempotency_key,
            intent: wire.intent,
        };
        command.validate().map_err(D::Error::custom)?;
        Ok(command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeroCreationDraft {
    pub schema_version: u16,
    pub draft_id: String,
    pub campaign_id: String,
    pub owner_id: String,
    pub revision: u64,
    pub expires_at_epoch_seconds: u64,
    pub step: CreationStep,
    pub pins: Option<HeroPins>,
    pub concept: Option<HeroConceptId>,
    pub ancestry: Option<AncestryId>,
    pub class: Option<ClassSelection>,
    pub ability_assignment: Option<StandardArrayAssignment>,
    pub background: Option<BackgroundSelection>,
    pub equipment: Option<EquipmentSelection>,
    pub wizard_spells: Option<WizardSpellSelection>,
    pub presentation: Option<HeroPresentation>,
    pub reviewed: bool,
    pub committed_character_id: Option<String>,
}

impl HeroCreationDraft {
    pub fn new(
        draft_id: String,
        campaign_id: String,
        owner_id: String,
        expires_at_epoch_seconds: u64,
    ) -> HeroResult<Self> {
        let draft = Self {
            schema_version: HERO_DRAFT_SCHEMA_VERSION,
            draft_id,
            campaign_id,
            owner_id,
            revision: 0,
            expires_at_epoch_seconds,
            step: CreationStep::CampaignTheme,
            pins: None,
            concept: None,
            ancestry: None,
            class: None,
            ability_assignment: None,
            background: None,
            equipment: None,
            wizard_spells: None,
            presentation: None,
            reviewed: false,
            committed_character_id: None,
        };
        draft.validate()?;
        Ok(draft)
    }

    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_DRAFT_SCHEMA_VERSION)?;
        validate_id("draft_id", &self.draft_id)?;
        validate_id("campaign_id", &self.campaign_id)?;
        validate_id("owner_id", &self.owner_id)?;
        if self.expires_at_epoch_seconds == 0 {
            return invalid("expires_at", "draft expiry must be non-zero");
        }
        let ordinal = self.step.ordinal();
        validate_step_option("pins", self.pins.as_ref(), ordinal >= 1)?;
        validate_step_option("concept", self.concept.as_ref(), ordinal >= 2)?;
        validate_step_option("ancestry", self.ancestry.as_ref(), ordinal >= 3)?;
        validate_step_option("class", self.class.as_ref(), ordinal >= 3)?;
        validate_step_option(
            "ability_assignment",
            self.ability_assignment.as_ref(),
            ordinal >= 4,
        )?;
        validate_step_option("background", self.background.as_ref(), ordinal >= 5)?;
        validate_step_option("equipment", self.equipment.as_ref(), ordinal >= 6)?;
        validate_step_option("presentation", self.presentation.as_ref(), ordinal >= 7)?;
        if self.reviewed != (ordinal >= 8) {
            return invalid(
                "reviewed",
                "review marker must match the authoritative draft step",
            );
        }
        validate_step_option(
            "committed_character_id",
            self.committed_character_id.as_ref(),
            ordinal == 9,
        )?;

        if let Some(pins) = &self.pins {
            pins.validate()?;
        }
        if self
            .ancestry
            .is_some_and(|ancestry| ancestry != AncestryId::Human)
        {
            return invalid("ancestry", "only human is offered");
        }
        if let Some(assignment) = &self.ability_assignment {
            assignment.validate()?;
        }
        if let (Some(class), Some(background)) = (&self.class, &self.background) {
            background.validate_for(class.class())?;
        }
        if let (Some(class), Some(equipment)) = (&self.class, &self.equipment) {
            equipment.validate_for(class)?;
        }
        if ordinal < 6 && self.wizard_spells.is_some() {
            return invalid(
                "wizard_spells",
                "spells cannot be selected before the equipment-and-spells step",
            );
        }
        if ordinal >= 6 {
            let class = self.class.as_ref().expect("validated class prerequisite");
            let assignment = self
                .ability_assignment
                .as_ref()
                .expect("validated ability prerequisite");
            let intelligence_modifier = assignment
                .human_scores()?
                .get(Ability::Intelligence)
                .modifier();
            match (class, &self.wizard_spells) {
                (ClassSelection::Wizard, Some(spells)) => {
                    spells.validate_creation(intelligence_modifier)?;
                }
                (ClassSelection::Wizard, None) => {
                    return invalid("wizard_spells", "wizard draft requires supported spells");
                }
                (ClassSelection::Fighter { .. }, None) => {}
                (ClassSelection::Fighter { .. }, Some(_)) => {
                    return invalid("wizard_spells", "fighter draft cannot include spells");
                }
            }
        }
        if let Some(presentation) = &self.presentation {
            presentation.validate()?;
        }
        if let Some(character_id) = &self.committed_character_id {
            validate_id("committed_character_id", character_id)?;
        }
        if ordinal >= 7 {
            self.to_choices()?.validate()?;
        }
        Ok(())
    }

    pub fn is_expired(&self, now_epoch_seconds: u64) -> bool {
        now_epoch_seconds > self.expires_at_epoch_seconds
    }

    pub fn apply_trusted(
        &mut self,
        command: &HeroCreationCommand,
        context: &TrustedMutationContext,
    ) -> HeroResult<HeroCreationOutcome> {
        self.validate()?;
        command.validate()?;
        context.validate()?;
        if command.draft_id != self.draft_id {
            return invalid("draft_id", "command targets a different draft");
        }
        if context.actor_id != self.owner_id {
            return invalid("actor_id", "only the draft owner can advance creation");
        }
        if self.is_expired(context.occurred_at_epoch_seconds) {
            return invalid("expires_at", "creation draft has expired");
        }
        check_revision(self.revision, command.expected_revision)?;
        let expected_step = command.intent.required_step();
        if self.step != expected_step {
            return Err(HeroError::DraftStepMismatch {
                expected: expected_step,
                actual: self.step,
            });
        }

        let revision_before = self.revision;
        let mut candidate = self.clone();
        let mut character = None;
        match &command.intent {
            HeroCreationIntent::SelectCampaignTheme { pins } => {
                candidate.pins = Some(pins.clone());
                candidate.step = CreationStep::Concept;
            }
            HeroCreationIntent::SelectConcept { concept } => {
                candidate.concept = Some(*concept);
                candidate.step = CreationStep::Rules;
            }
            HeroCreationIntent::SelectRules { ancestry, class } => {
                if *ancestry != AncestryId::Human {
                    return invalid("ancestry", "only human is offered");
                }
                candidate.ancestry = Some(*ancestry);
                candidate.class = Some(class.clone());
                candidate.step = CreationStep::AbilityScores;
            }
            HeroCreationIntent::AssignAbilities { assignment } => {
                assignment.validate()?;
                candidate.ability_assignment = Some(assignment.clone());
                candidate.step = CreationStep::Background;
            }
            HeroCreationIntent::SelectBackground { selection } => {
                selection.validate_for(
                    candidate
                        .class
                        .as_ref()
                        .expect("step validation requires class")
                        .class(),
                )?;
                candidate.background = Some(selection.clone());
                candidate.step = CreationStep::EquipmentAndSpells;
            }
            HeroCreationIntent::SelectEquipmentAndSpells {
                equipment,
                wizard_spells,
            } => {
                let class = candidate
                    .class
                    .as_ref()
                    .expect("step validation requires class");
                equipment.validate_for(class)?;
                let intelligence_modifier = candidate
                    .ability_assignment
                    .as_ref()
                    .expect("step validation requires abilities")
                    .human_scores()?
                    .get(Ability::Intelligence)
                    .modifier();
                match (class, wizard_spells) {
                    (ClassSelection::Wizard, Some(spells)) => {
                        spells.validate_creation(intelligence_modifier)?;
                    }
                    (ClassSelection::Wizard, None) => {
                        return invalid("wizard_spells", "wizard spells are required");
                    }
                    (ClassSelection::Fighter { .. }, None) => {}
                    (ClassSelection::Fighter { .. }, Some(_)) => {
                        return invalid("wizard_spells", "fighter cannot select wizard spells");
                    }
                }
                candidate.equipment = Some(equipment.clone());
                candidate.wizard_spells = wizard_spells.clone();
                candidate.step = CreationStep::Presentation;
            }
            HeroCreationIntent::SetPresentation { presentation } => {
                presentation.validate()?;
                candidate.presentation = Some(presentation.clone());
                candidate.step = CreationStep::Review;
            }
            HeroCreationIntent::Review => {
                candidate.to_choices()?.validate()?;
                candidate.reviewed = true;
                candidate.step = CreationStep::Commit;
            }
            HeroCreationIntent::Commit { character_id } => {
                let created = HeroCharacter::create(
                    character_id.clone(),
                    candidate.campaign_id.clone(),
                    candidate.owner_id.clone(),
                    candidate.to_choices()?,
                )?;
                candidate.committed_character_id = Some(character_id.clone());
                candidate.step = CreationStep::Committed;
                character = Some(created);
            }
        }
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(HeroError::InvalidField {
                field: "revision",
                reason: "draft revision overflowed",
            })?;
        candidate.validate()?;

        let transition_audit = HeroCreationTransitionAuditDto {
            schema_version: HERO_AUDIT_SCHEMA_VERSION,
            audit_id: context.audit_id.clone(),
            actor_id: context.actor_id.clone(),
            draft_id: self.draft_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            revision_before,
            revision_after: candidate.revision,
            step_before: self.step,
            step_after: candidate.step,
            action: command.intent.action(),
            occurred_at_epoch_seconds: context.occurred_at_epoch_seconds,
        };
        transition_audit.validate()?;
        let created_audit = character.as_ref().map(|created| CharacterCreatedAuditDto {
            schema_version: HERO_AUDIT_SCHEMA_VERSION,
            audit_id: context.audit_id.clone(),
            actor_id: context.actor_id.clone(),
            draft_id: self.draft_id.clone(),
            character_id: created.character_id.clone(),
            campaign_id: created.campaign_id.clone(),
            draft_revision: candidate.revision,
            choices: created.choices.clone(),
            derived_sheet: created.sheet.clone(),
            occurred_at_epoch_seconds: context.occurred_at_epoch_seconds,
        });
        if let Some(audit) = &created_audit {
            audit.validate()?;
        }
        *self = candidate;
        Ok(HeroCreationOutcome {
            transition_audit,
            character,
            created_audit,
        })
    }

    fn to_choices(&self) -> HeroResult<HeroChoices> {
        Ok(HeroChoices {
            pins: self.pins.clone().ok_or(HeroError::InvalidField {
                field: "pins",
                reason: "campaign/theme pins are incomplete",
            })?,
            concept: self.concept.ok_or(HeroError::InvalidField {
                field: "concept",
                reason: "concept is incomplete",
            })?,
            ancestry: self.ancestry.ok_or(HeroError::InvalidField {
                field: "ancestry",
                reason: "ancestry is incomplete",
            })?,
            class: self.class.clone().ok_or(HeroError::InvalidField {
                field: "class",
                reason: "class is incomplete",
            })?,
            ability_assignment: self
                .ability_assignment
                .clone()
                .ok_or(HeroError::InvalidField {
                    field: "ability_assignment",
                    reason: "ability assignment is incomplete",
                })?,
            background: self.background.clone().ok_or(HeroError::InvalidField {
                field: "background",
                reason: "background is incomplete",
            })?,
            equipment: self.equipment.clone().ok_or(HeroError::InvalidField {
                field: "equipment",
                reason: "equipment is incomplete",
            })?,
            wizard_spells: self.wizard_spells.clone(),
            presentation: self.presentation.clone().ok_or(HeroError::InvalidField {
                field: "presentation",
                reason: "presentation is incomplete",
            })?,
        })
    }
}

impl<'de> Deserialize<'de> for HeroCreationDraft {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: u16,
            draft_id: String,
            campaign_id: String,
            owner_id: String,
            revision: u64,
            expires_at_epoch_seconds: u64,
            step: CreationStep,
            pins: Option<HeroPins>,
            concept: Option<HeroConceptId>,
            ancestry: Option<AncestryId>,
            class: Option<ClassSelection>,
            ability_assignment: Option<StandardArrayAssignment>,
            background: Option<BackgroundSelection>,
            equipment: Option<EquipmentSelection>,
            wizard_spells: Option<WizardSpellSelection>,
            presentation: Option<HeroPresentation>,
            reviewed: bool,
            committed_character_id: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let draft = Self {
            schema_version: wire.schema_version,
            draft_id: wire.draft_id,
            campaign_id: wire.campaign_id,
            owner_id: wire.owner_id,
            revision: wire.revision,
            expires_at_epoch_seconds: wire.expires_at_epoch_seconds,
            step: wire.step,
            pins: wire.pins,
            concept: wire.concept,
            ancestry: wire.ancestry,
            class: wire.class,
            ability_assignment: wire.ability_assignment,
            background: wire.background,
            equipment: wire.equipment,
            wizard_spells: wire.wizard_spells,
            presentation: wire.presentation,
            reviewed: wire.reviewed,
            committed_character_id: wire.committed_character_id,
        };
        draft.validate().map_err(D::Error::custom)?;
        Ok(draft)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeroCreationAction {
    CampaignThemeSelected,
    ConceptSelected,
    RulesSelected,
    AbilitiesAssigned,
    BackgroundSelected,
    EquipmentAndSpellsSelected,
    PresentationSet,
    Reviewed,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroCreationTransitionAuditDto {
    pub schema_version: u16,
    pub audit_id: String,
    pub actor_id: String,
    pub draft_id: String,
    pub idempotency_key: String,
    pub revision_before: u64,
    pub revision_after: u64,
    pub step_before: CreationStep,
    pub step_after: CreationStep,
    pub action: HeroCreationAction,
    pub occurred_at_epoch_seconds: u64,
}

impl HeroCreationTransitionAuditDto {
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, HERO_AUDIT_SCHEMA_VERSION)?;
        validate_id("audit_id", &self.audit_id)?;
        validate_id("actor_id", &self.actor_id)?;
        validate_id("draft_id", &self.draft_id)?;
        validate_id("idempotency_key", &self.idempotency_key)?;
        if self.revision_before.checked_add(1) != Some(self.revision_after)
            || self.step_after.ordinal() != self.step_before.ordinal() + 1
            || self.occurred_at_epoch_seconds == 0
        {
            return invalid(
                "creation_audit",
                "transition audit must describe one adjacent revision and step",
            );
        }
        let expected_action = match self.step_before {
            CreationStep::CampaignTheme => HeroCreationAction::CampaignThemeSelected,
            CreationStep::Concept => HeroCreationAction::ConceptSelected,
            CreationStep::Rules => HeroCreationAction::RulesSelected,
            CreationStep::AbilityScores => HeroCreationAction::AbilitiesAssigned,
            CreationStep::Background => HeroCreationAction::BackgroundSelected,
            CreationStep::EquipmentAndSpells => HeroCreationAction::EquipmentAndSpellsSelected,
            CreationStep::Presentation => HeroCreationAction::PresentationSet,
            CreationStep::Review => HeroCreationAction::Reviewed,
            CreationStep::Commit => HeroCreationAction::Committed,
            CreationStep::Committed => {
                return invalid(
                    "creation_audit.step_before",
                    "committed drafts have no next transition",
                );
            }
        };
        if self.action != expected_action {
            return invalid(
                "creation_audit.action",
                "action does not match the recorded step transition",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeroCreationOutcome {
    pub transition_audit: HeroCreationTransitionAuditDto,
    pub character: Option<HeroCharacter>,
    pub created_audit: Option<CharacterCreatedAuditDto>,
}

fn validate_step_option<T>(
    field: &'static str,
    value: Option<&T>,
    should_be_present: bool,
) -> HeroResult<()> {
    if value.is_some() == should_be_present {
        Ok(())
    } else if should_be_present {
        invalid(field, "required prerequisite is missing at this draft step")
    } else {
        invalid(field, "a future-step value was supplied early")
    }
}

fn validate_sorted_unique<T>(field: &'static str, values: &[T]) -> HeroResult<()>
where
    T: Ord,
{
    if values.windows(2).any(|window| window[0] >= window[1]) {
        invalid(
            field,
            "values must be canonical, strictly sorted, and unique",
        )
    } else {
        Ok(())
    }
}

pub(crate) fn require_schema(actual: u16, expected: u16) -> HeroResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(HeroError::InvalidSchemaVersion { expected, actual })
    }
}

pub(crate) fn validate_id(field: &'static str, value: &str) -> HeroResult<()> {
    if is_valid_opaque_id(value) {
        Ok(())
    } else {
        invalid(field, "identifier must satisfy the shared opaque-id bounds")
    }
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum_chars: usize,
    required: bool,
) -> HeroResult<()> {
    let trimmed = value.trim();
    if (required && trimmed.is_empty())
        || value.chars().count() > maximum_chars
        || value.chars().any(char::is_control)
    {
        invalid(
            field,
            "text is blank, too long, or contains control characters",
        )
    } else {
        Ok(())
    }
}

fn invalid<T>(field: &'static str, reason: &'static str) -> HeroResult<T> {
    Err(HeroError::InvalidField { field, reason })
}

fn check_revision(actual: u64, expected: u64) -> HeroResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(HeroError::StaleRevision { expected, actual })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presentation(name: &str) -> HeroPresentation {
        HeroPresentation {
            name: name.to_owned(),
            pronouns: "they/them".to_owned(),
            appearance: "A weathered coat and a careful gaze.".to_owned(),
            ideal: "Leave every street safer.".to_owned(),
            bond: "The canal ward gave them a home.".to_owned(),
            flaw: "They shoulder every burden alone.".to_owned(),
            tone_limits: vec!["No graphic horror".to_owned()],
        }
    }

    fn assignment(values: [u8; 6]) -> StandardArrayAssignment {
        StandardArrayAssignment {
            strength: values[0],
            dexterity: values[1],
            constitution: values[2],
            intelligence: values[3],
            wisdom: values[4],
            charisma: values[5],
        }
    }

    fn default_assignment() -> StandardArrayAssignment {
        assignment([15, 14, 13, 12, 10, 8])
    }

    fn background_for(class: HeroClass, background: BackgroundId) -> BackgroundSelection {
        let blocked = background.skill_proficiencies();
        let mut class_skills = class_skill_choices(class)
            .iter()
            .copied()
            .filter(|skill| !blocked.contains(skill))
            .take(2)
            .collect::<Vec<_>>();
        class_skills.sort_unstable();
        BackgroundSelection {
            background,
            class_skills,
        }
    }

    fn fighter_equipment(
        armor: EquipmentId,
        simple_weapon: Option<SimpleWeaponId>,
        shield: bool,
    ) -> EquipmentSelection {
        let mut carried = vec![
            armor,
            EquipmentId::LightCrossbow,
            EquipmentId::ExplorersPack,
        ];
        carried.push(if simple_weapon.is_some() {
            EquipmentId::SimpleWeapons
        } else {
            EquipmentId::Longsword
        });
        if shield {
            carried.push(EquipmentId::Shield);
        }
        carried.sort_unstable();
        EquipmentSelection {
            carried,
            simple_weapon,
            equipped_armor: Some(armor),
            shield_equipped: shield,
        }
    }

    fn wizard_equipment(weapon: SimpleWeaponId) -> EquipmentSelection {
        EquipmentSelection {
            carried: vec![
                EquipmentId::SimpleWeapons,
                EquipmentId::ScholarsPack,
                EquipmentId::Spellbook,
                EquipmentId::ArcaneFocus,
            ],
            simple_weapon: Some(weapon),
            equipped_armor: None,
            shield_equipped: false,
        }
    }

    fn wizard_spells(assignment: &StandardArrayAssignment) -> WizardSpellSelection {
        let intelligence_modifier = assignment
            .human_scores()
            .unwrap()
            .get(Ability::Intelligence)
            .modifier();
        let capacity = usize::from(prepared_spell_capacity(1, intelligence_modifier));
        WizardSpellSelection {
            cantrips: SpellId::CANTRIPS.to_vec(),
            spellbook: SpellId::LEVEL_ONE.to_vec(),
            prepared: SpellId::LEVEL_ONE[..capacity].to_vec(),
        }
    }

    fn fighter_choices(theme: ThemeId) -> HeroChoices {
        let abilities = default_assignment();
        HeroChoices {
            pins: HeroPins::mvp(theme),
            concept: HeroConceptId::CanalGuardian,
            ancestry: AncestryId::Human,
            class: ClassSelection::Fighter {
                fighting_style: FightingStyleId::Defense,
            },
            ability_assignment: abilities,
            background: background_for(HeroClass::Fighter, BackgroundId::Soldier),
            equipment: fighter_equipment(EquipmentId::ChainMail, None, true),
            wizard_spells: None,
            presentation: presentation("Mara Vale"),
        }
    }

    fn wizard_choices(theme: ThemeId) -> HeroChoices {
        let abilities = default_assignment();
        HeroChoices {
            pins: HeroPins::mvp(theme),
            concept: HeroConceptId::ArchiveSeeker,
            ancestry: AncestryId::Human,
            class: ClassSelection::Wizard,
            ability_assignment: abilities.clone(),
            background: background_for(HeroClass::Wizard, BackgroundId::Sage),
            equipment: wizard_equipment(SimpleWeaponId::Quarterstaff),
            wizard_spells: Some(wizard_spells(&abilities)),
            presentation: presentation("Eli Ward"),
        }
    }

    fn permutations() -> Vec<[u8; 6]> {
        fn visit(index: usize, values: &mut [u8; 6], output: &mut Vec<[u8; 6]>) {
            if index == values.len() {
                output.push(*values);
                return;
            }
            for selected in index..values.len() {
                values.swap(index, selected);
                visit(index + 1, values, output);
                values.swap(index, selected);
            }
        }
        let mut values = StandardArrayAssignment::VALUES;
        let mut output = Vec::new();
        visit(0, &mut values, &mut output);
        output
    }

    #[test]
    fn standard_array_has_exactly_720_valid_assignments_and_no_reuse() {
        let assignments = permutations();
        assert_eq!(assignments.len(), 720);
        assert_eq!(
            assignments.iter().copied().collect::<BTreeSet<_>>().len(),
            720
        );
        for values in assignments {
            assignment(values).validate().unwrap();
        }

        assert!(assignment([15, 15, 13, 12, 10, 8]).validate().is_err());
        assert!(assignment([16, 14, 13, 12, 10, 8]).validate().is_err());
    }

    #[test]
    fn all_ability_class_background_theme_and_loadout_combinations_derive() {
        let mut checked = 0_u32;
        for values in permutations() {
            let ability_assignment = assignment(values);
            for theme in ThemeId::ALL {
                for background in BackgroundId::ALL {
                    for style in [FightingStyleId::Defense, FightingStyleId::Dueling] {
                        for armor in [EquipmentId::ChainMail, EquipmentId::LeatherArmor] {
                            let mut loadouts = vec![(None, false), (None, true)];
                            loadouts.extend(SimpleWeaponId::ALL.into_iter().flat_map(|weapon| {
                                if weapon.is_two_handed() {
                                    vec![(Some(weapon), false)]
                                } else {
                                    vec![(Some(weapon), false), (Some(weapon), true)]
                                }
                            }));
                            for (simple_weapon, shield) in loadouts {
                                if style == FightingStyleId::Dueling
                                    && simple_weapon.is_some_and(SimpleWeaponId::is_two_handed)
                                {
                                    continue;
                                }
                                let choices = HeroChoices {
                                    pins: HeroPins::mvp(theme),
                                    concept: HeroConceptId::CanalGuardian,
                                    ancestry: AncestryId::Human,
                                    class: ClassSelection::Fighter {
                                        fighting_style: style,
                                    },
                                    ability_assignment: ability_assignment.clone(),
                                    background: background_for(HeroClass::Fighter, background),
                                    equipment: fighter_equipment(armor, simple_weapon, shield),
                                    wizard_spells: None,
                                    presentation: presentation("Valid Fighter"),
                                };
                                let hero = HeroCharacter::create(
                                    "hero-matrix".to_owned(),
                                    "campaign-matrix".to_owned(),
                                    "owner-matrix".to_owned(),
                                    choices,
                                )
                                .unwrap();
                                hero.validate().unwrap();
                                checked += 1;
                            }
                        }
                    }

                    for weapon in [SimpleWeaponId::Dagger, SimpleWeaponId::Quarterstaff] {
                        let choices = HeroChoices {
                            pins: HeroPins::mvp(theme),
                            concept: HeroConceptId::ArchiveSeeker,
                            ancestry: AncestryId::Human,
                            class: ClassSelection::Wizard,
                            ability_assignment: ability_assignment.clone(),
                            background: background_for(HeroClass::Wizard, background),
                            equipment: wizard_equipment(weapon),
                            wizard_spells: Some(wizard_spells(&ability_assignment)),
                            presentation: presentation("Valid Wizard"),
                        };
                        let hero = HeroCharacter::create(
                            "hero-matrix".to_owned(),
                            "campaign-matrix".to_owned(),
                            "owner-matrix".to_owned(),
                            choices,
                        )
                        .unwrap();
                        hero.validate().unwrap();
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 241_920);
    }

    #[test]
    fn every_filtered_class_skill_pair_is_valid_and_duplicates_are_rejected() {
        for class in HeroClass::ALL {
            for background in BackgroundId::ALL {
                let blocked = background.skill_proficiencies();
                let available = class_skill_choices(class)
                    .iter()
                    .copied()
                    .filter(|skill| !blocked.contains(skill))
                    .collect::<Vec<_>>();
                for left in 0..available.len() {
                    for right in (left + 1)..available.len() {
                        let mut class_skills = vec![available[left], available[right]];
                        class_skills.sort_unstable();
                        BackgroundSelection {
                            background,
                            class_skills,
                        }
                        .validate_for(class)
                        .unwrap();
                    }
                }
            }
        }

        let duplicate = BackgroundSelection {
            background: BackgroundId::Soldier,
            class_skills: vec![SkillId::Athletics, SkillId::Athletics],
        };
        assert!(duplicate.validate_for(HeroClass::Fighter).is_err());
        let overlaps_background = BackgroundSelection {
            background: BackgroundId::Sage,
            class_skills: vec![SkillId::Arcana, SkillId::Insight],
        };
        assert!(overlaps_background.validate_for(HeroClass::Wizard).is_err());
    }

    #[test]
    fn forged_class_equipment_spells_and_pins_fail_closed() {
        let mut wizard = wizard_choices(ThemeId::RainboundBorough);
        wizard.equipment = fighter_equipment(EquipmentId::ChainMail, None, true);
        assert!(wizard.validate().is_err());

        let mut fighter = fighter_choices(ThemeId::RainboundBorough);
        fighter.wizard_spells = Some(wizard_spells(&fighter.ability_assignment));
        assert!(fighter.validate().is_err());

        let mut duplicate_equipment = fighter_choices(ThemeId::RainboundBorough);
        duplicate_equipment
            .equipment
            .carried
            .insert(0, EquipmentId::Longsword);
        assert!(duplicate_equipment.validate().is_err());

        let mut forged_pin = fighter_choices(ThemeId::RainboundBorough);
        forged_pin.pins.core_content.digest =
            Sha256Digest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(forged_pin.validate().is_err());
    }

    #[test]
    fn themes_and_presentation_are_mechanically_independent() {
        let left = fighter_choices(ThemeId::RainboundBorough);
        let mut right = left.clone();
        right.pins = HeroPins::mvp(ThemeId::EmberlineArchive);
        right.concept = HeroConceptId::ArchiveSeeker;
        right.presentation = presentation("A Completely Different Hero");

        let left_sheet = derive_sheet(&left, SupportedLevel::One, &[]).unwrap();
        let right_sheet = derive_sheet(&right, SupportedLevel::One, &[]).unwrap();
        assert_eq!(left_sheet, right_sheet);
    }

    fn apply_creation(
        draft: &mut HeroCreationDraft,
        intent: HeroCreationIntent,
        sequence: u64,
    ) -> HeroCreationOutcome {
        let command = HeroCreationCommand {
            schema_version: HERO_COMMAND_SCHEMA_VERSION,
            draft_id: draft.draft_id.clone(),
            expected_revision: draft.revision,
            idempotency_key: format!("creation-command-{sequence}"),
            intent,
        };
        draft
            .apply_trusted(
                &command,
                &TrustedMutationContext {
                    audit_id: format!("creation-audit-{sequence}"),
                    actor_id: draft.owner_id.clone(),
                    occurred_at_epoch_seconds: 100 + sequence,
                },
            )
            .unwrap()
    }

    #[test]
    fn draft_transitions_are_resumable_strict_and_commit_atomically() {
        let mut draft = HeroCreationDraft::new(
            "draft-1".to_owned(),
            "campaign-1".to_owned(),
            "owner-1".to_owned(),
            10_000,
        )
        .unwrap();
        apply_creation(
            &mut draft,
            HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
            },
            1,
        );
        apply_creation(
            &mut draft,
            HeroCreationIntent::SelectConcept {
                concept: HeroConceptId::CanalGuardian,
            },
            2,
        );

        let json = serde_json::to_string(&draft).unwrap();
        let mut draft: HeroCreationDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(draft.step, CreationStep::Rules);
        assert_eq!(draft.revision, 2);

        apply_creation(
            &mut draft,
            HeroCreationIntent::SelectRules {
                ancestry: AncestryId::Human,
                class: ClassSelection::Fighter {
                    fighting_style: FightingStyleId::Defense,
                },
            },
            3,
        );
        apply_creation(
            &mut draft,
            HeroCreationIntent::AssignAbilities {
                assignment: default_assignment(),
            },
            4,
        );
        apply_creation(
            &mut draft,
            HeroCreationIntent::SelectBackground {
                selection: background_for(HeroClass::Fighter, BackgroundId::Soldier),
            },
            5,
        );
        apply_creation(
            &mut draft,
            HeroCreationIntent::SelectEquipmentAndSpells {
                equipment: fighter_equipment(EquipmentId::ChainMail, None, true),
                wizard_spells: None,
            },
            6,
        );
        apply_creation(
            &mut draft,
            HeroCreationIntent::SetPresentation {
                presentation: presentation("Mara Vale"),
            },
            7,
        );
        apply_creation(&mut draft, HeroCreationIntent::Review, 8);
        let outcome = apply_creation(
            &mut draft,
            HeroCreationIntent::Commit {
                character_id: "hero-1".to_owned(),
            },
            9,
        );

        assert_eq!(draft.step, CreationStep::Committed);
        assert_eq!(draft.revision, 9);
        let character = outcome.character.unwrap();
        character.validate().unwrap();
        assert_eq!(character.level, SupportedLevel::One);
        let created_audit = outcome.created_audit.unwrap();
        created_audit.validate().unwrap();
        assert_eq!(created_audit.choices, character.choices);
        assert_eq!(created_audit.derived_sheet, character.sheet);
    }

    #[test]
    fn stale_or_out_of_order_creation_never_partially_mutates() {
        let mut draft = HeroCreationDraft::new(
            "draft-2".to_owned(),
            "campaign-2".to_owned(),
            "owner-2".to_owned(),
            10_000,
        )
        .unwrap();
        let before = draft.clone();
        let stale = HeroCreationCommand {
            schema_version: HERO_COMMAND_SCHEMA_VERSION,
            draft_id: draft.draft_id.clone(),
            expected_revision: 4,
            idempotency_key: "stale-command".to_owned(),
            intent: HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
            },
        };
        assert!(matches!(
            draft.apply_trusted(
                &stale,
                &TrustedMutationContext {
                    audit_id: "audit-stale".to_owned(),
                    actor_id: "owner-2".to_owned(),
                    occurred_at_epoch_seconds: 100,
                }
            ),
            Err(HeroError::StaleRevision { .. })
        ));
        assert_eq!(draft, before);

        let out_of_order = HeroCreationCommand {
            schema_version: HERO_COMMAND_SCHEMA_VERSION,
            draft_id: draft.draft_id.clone(),
            expected_revision: 0,
            idempotency_key: "early-commit".to_owned(),
            intent: HeroCreationIntent::Commit {
                character_id: "hero-forged".to_owned(),
            },
        };
        assert!(matches!(
            draft.apply_trusted(
                &out_of_order,
                &TrustedMutationContext {
                    audit_id: "audit-early".to_owned(),
                    actor_id: "owner-2".to_owned(),
                    occurred_at_epoch_seconds: 101,
                }
            ),
            Err(HeroError::DraftStepMismatch { .. })
        ));
        assert_eq!(draft, before);
    }

    #[test]
    fn reward_tiers_and_level_up_are_trusted_atomic_and_reload_stable() {
        for (class, choices) in [
            (
                HeroClass::Fighter,
                fighter_choices(ThemeId::RainboundBorough),
            ),
            (HeroClass::Wizard, wizard_choices(ThemeId::EmberlineArchive)),
        ] {
            let mut hero = HeroCharacter::create(
                format!("hero-{class:?}"),
                "campaign-level".to_owned(),
                "owner-level".to_owned(),
                choices,
            )
            .unwrap();
            let reward = hero
                .apply_reward(
                    &RewardAwardCommand {
                        schema_version: HERO_COMMAND_SCHEMA_VERSION,
                        character_id: hero.character_id.clone(),
                        expected_revision: 0,
                        idempotency_key: format!("reward-{class:?}"),
                        tier: RewardTier::Major,
                    },
                    TrustedRewardPolicy::MvpXpV1,
                    &TrustedMutationContext {
                        audit_id: format!("reward-audit-{class:?}"),
                        actor_id: "owner-level".to_owned(),
                        occurred_at_epoch_seconds: 500,
                    },
                )
                .unwrap();
            reward.validate().unwrap();
            assert_eq!(reward.experience_awarded, LEVEL_TWO_XP);
            assert!(hero.level_up_eligible());
            assert_eq!(hero.valid_level_up_choices().unwrap().len(), 1);

            let hp_before = hero.sheet.maximum_hit_points;
            let choice = hero.valid_level_up_choices().unwrap().remove(0);
            let audit = hero
                .level_up(
                    &LevelUpCommand {
                        schema_version: HERO_COMMAND_SCHEMA_VERSION,
                        character_id: hero.character_id.clone(),
                        expected_revision: 1,
                        idempotency_key: format!("level-up-{class:?}"),
                        choice,
                    },
                    &TrustedMutationContext {
                        audit_id: format!("level-audit-{class:?}"),
                        actor_id: "owner-level".to_owned(),
                        occurred_at_epoch_seconds: 501,
                    },
                )
                .unwrap();
            audit.validate().unwrap();
            assert_eq!(hero.level, SupportedLevel::Two);
            assert!(hero.sheet.maximum_hit_points > hp_before);

            let canonical = serde_json::to_vec(&hero).unwrap();
            let reloaded: HeroCharacter = serde_json::from_slice(&canonical).unwrap();
            assert_eq!(reloaded, hero);
            assert_eq!(serde_json::to_vec(&reloaded).unwrap(), canonical);
        }
    }

    #[test]
    fn forged_level_up_choice_and_derived_sheet_are_rejected() {
        let mut wizard = HeroCharacter::create(
            "hero-wizard".to_owned(),
            "campaign-wizard".to_owned(),
            "owner-wizard".to_owned(),
            wizard_choices(ThemeId::RainboundBorough),
        )
        .unwrap();
        wizard
            .apply_reward(
                &RewardAwardCommand {
                    schema_version: HERO_COMMAND_SCHEMA_VERSION,
                    character_id: "hero-wizard".to_owned(),
                    expected_revision: 0,
                    idempotency_key: "major-reward".to_owned(),
                    tier: RewardTier::Major,
                },
                TrustedRewardPolicy::MvpXpV1,
                &TrustedMutationContext {
                    audit_id: "reward-audit".to_owned(),
                    actor_id: "owner-wizard".to_owned(),
                    occurred_at_epoch_seconds: 600,
                },
            )
            .unwrap();
        let before = wizard.clone();
        assert!(
            wizard
                .level_up(
                    &LevelUpCommand {
                        schema_version: HERO_COMMAND_SCHEMA_VERSION,
                        character_id: "hero-wizard".to_owned(),
                        expected_revision: 1,
                        idempotency_key: "forged-fighter-level".to_owned(),
                        choice: LevelUpChoice::Fighter {
                            hit_points: HitPointGrowthChoice::FixedAverage,
                        },
                    },
                    &TrustedMutationContext {
                        audit_id: "level-audit".to_owned(),
                        actor_id: "owner-wizard".to_owned(),
                        occurred_at_epoch_seconds: 601,
                    },
                )
                .is_err()
        );
        assert_eq!(wizard, before);

        let mut forged = serde_json::to_value(&wizard).unwrap();
        forged["sheet"]["armor_class"] = serde_json::json!(99);
        assert!(serde_json::from_value::<HeroCharacter>(forged).is_err());
    }

    #[test]
    fn strict_boundary_rejects_unknown_fields_and_bad_schema_versions() {
        let command = HeroCreationCommand {
            schema_version: HERO_COMMAND_SCHEMA_VERSION,
            draft_id: "draft-strict".to_owned(),
            expected_revision: 0,
            idempotency_key: "command-strict".to_owned(),
            intent: HeroCreationIntent::SelectCampaignTheme {
                pins: HeroPins::mvp(ThemeId::RainboundBorough),
            },
        };
        let mut unknown = serde_json::to_value(&command).unwrap();
        unknown["forged_xp"] = serde_json::json!(300);
        assert!(serde_json::from_value::<HeroCreationCommand>(unknown).is_err());

        let mut wrong_version = serde_json::to_value(&command).unwrap();
        wrong_version["schema_version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<HeroCreationCommand>(wrong_version).is_err());

        let mut hero_json = serde_json::to_value(
            HeroCharacter::create(
                "hero-strict".to_owned(),
                "campaign-strict".to_owned(),
                "owner-strict".to_owned(),
                fighter_choices(ThemeId::RainboundBorough),
            )
            .unwrap(),
        )
        .unwrap();
        hero_json["unknown_mechanic"] = serde_json::json!(true);
        assert!(serde_json::from_value::<HeroCharacter>(hero_json).is_err());
    }

    #[test]
    fn unsupported_mechanics_return_typed_authored_alternatives() {
        let response = ActionCapability::from_mechanic_id("action.teleport-anywhere").unwrap_err();
        response.validate().unwrap();
        assert_eq!(response.code, UnsupportedMechanicCode::OutsideMvpMatrix);
        assert_eq!(response.requested_id, "action.teleport-anywhere");
        assert!(
            response
                .alternatives
                .iter()
                .any(|alternative| alternative.action == ActionCapability::Attack)
        );
    }

    #[test]
    fn wizard_spell_capabilities_are_complete_and_do_not_activate_sculpt_spells() {
        let hero = HeroCharacter::create(
            "hero-spells".to_owned(),
            "campaign-spells".to_owned(),
            "owner-spells".to_owned(),
            wizard_choices(ThemeId::RainboundBorough),
        )
        .unwrap();
        let spellcasting = hero.sheet.spellcasting.unwrap();
        assert_eq!(spellcasting.effects.len(), SpellId::ALL.len());
        assert_eq!(
            spellcasting
                .effects
                .iter()
                .map(|effect| effect.spell)
                .collect::<BTreeSet<_>>(),
            SpellId::ALL.into_iter().collect()
        );
        assert!(!hero.sheet.features.iter().any(|feature| {
            feature.feature == FeatureId::SculptSpells
                && feature.availability == FeatureAvailability::Active
        }));
    }
}
