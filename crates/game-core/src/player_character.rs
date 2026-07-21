//! Account-owned, campaign-independent player character library types.
//!
//! A `PlayerCharacter` stores identity and reusable creation choices only. It
//! has no `campaign_id`, level, XP, current/max HP, resources, conditions,
//! equipment changes, or mutable campaign state. Level-dependent sheet
//! derivation occurs only after a campaign instance is created from a library
//! character.
//!
//! The conversion [`PlayerCharacter::instantiate_for_campaign`] creates a
//! campaign-bound level-one [`HeroCharacter`] from a library character plus
//! campaign-compatible content pins. Two campaign instances from the same library
//! character can advance independently.

use serde::{Deserialize, Serialize};

use crate::hero::{
    HeroCharacter, HeroChoices, HeroError, HeroResult, ThemeId, require_schema, validate_id,
};
use crate::is_valid_opaque_id;

/// Current schema version for player character library documents.
pub const PLAYER_CHARACTER_SCHEMA_VERSION: u16 = 1;

/// Maximum allowed length for a player character display name.
pub const MAX_DISPLAY_NAME_LEN: usize = 200;

/// An account-owned, campaign-independent character stored in the player's
/// library. Contains identity and reusable creation choices only — no level,
/// XP, HP, or any campaign-derived runtime state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerCharacter {
    pub schema_version: u16,
    pub character_id: String,
    pub owner_account_id: String,
    pub revision: u64,
    pub display_name: String,
    pub choices: HeroChoices,
}

impl PlayerCharacter {
    /// Creates a new library character from validated creation choices.
    /// The `owner_account_id` is always server-derived, never browser-provided.
    pub fn new(
        character_id: String,
        owner_account_id: String,
        display_name: String,
        choices: HeroChoices,
    ) -> HeroResult<Self> {
        validate_id("character_id", &character_id)?;
        validate_account_id(&owner_account_id)?;
        validate_display_name(&display_name)?;
        choices.validate()?;
        let character = Self {
            schema_version: PLAYER_CHARACTER_SCHEMA_VERSION,
            character_id,
            owner_account_id,
            revision: 0,
            display_name,
            choices,
        };
        character.validate()?;
        Ok(character)
    }

    /// Validates the library character without checking campaign runtime state.
    /// This intentionally does not validate level, XP, HP, or advancement
    /// choices because those belong to campaign-bound instances.
    pub fn validate(&self) -> HeroResult<()> {
        require_schema(self.schema_version, PLAYER_CHARACTER_SCHEMA_VERSION)?;
        validate_id("character_id", &self.character_id)?;
        validate_account_id(&self.owner_account_id)?;
        validate_display_name(&self.display_name)?;
        self.choices.validate()?;
        Ok(())
    }

    /// Bumps the revision counter. Called when the character is mutated
    /// (e.g., display name change or choices update).
    pub fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    /// Creates a campaign-bound level-one [`HeroCharacter`] from this library
    /// character. The campaign pins must be compatible with the character's
    /// theme pins.
    ///
    /// The resulting `HeroCharacter` is a fully independent runtime instance.
    /// Two instances from the same library character can advance to different
    /// levels and stats without affecting each other or the library character.
    pub fn instantiate_for_campaign(
        &self,
        campaign_id: String,
        runtime_character_id: String,
    ) -> HeroResult<HeroCharacter> {
        self.validate()?;
        validate_id("campaign_id", &campaign_id)?;
        validate_id("runtime_character_id", &runtime_character_id)?;
        HeroCharacter::create(
            runtime_character_id,
            campaign_id,
            self.owner_account_id.clone(),
            self.choices.clone(),
        )
    }

    /// Returns the theme ID from the character's content pins.
    pub fn theme_id(&self) -> ThemeId {
        self.choices.pins.theme_id
    }

    /// Returns true if the character is owned by the given account.
    /// This is a presentation-level check; server-side authorization is mandatory.
    pub fn is_owned_by(&self, account_id: &str) -> bool {
        self.owner_account_id == account_id
    }
}

/// Validates an account ID. Accepts the local compatibility principal
/// or a valid opaque identifier with the `account:` prefix.
fn validate_account_id(account_id: &str) -> HeroResult<()> {
    if account_id == crate::hero::CORE_CONTENT_PACK_ID {
        // Internal pack IDs are not account IDs.
        return invalid(
            "owner_account_id",
            "account identifier must not be a content pack ID",
        );
    }
    if account_id == "account:local" {
        return Ok(());
    }
    if !account_id.starts_with("account:") {
        return invalid(
            "owner_account_id",
            "account identifier must use the account: prefix",
        );
    }
    if !is_valid_opaque_id(account_id) {
        return invalid(
            "owner_account_id",
            "account identifier must satisfy the shared opaque-id bounds",
        );
    }
    Ok(())
}

/// Validates a display name: non-empty after trimming, no control characters,
/// and within the maximum length.
fn validate_display_name(name: &str) -> HeroResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || name.chars().count() > MAX_DISPLAY_NAME_LEN
        || name.chars().any(char::is_control)
    {
        return invalid(
            "display_name",
            "display name must be 1–200 non-control characters",
        );
    }
    Ok(())
}

fn invalid<T>(field: &'static str, reason: &'static str) -> HeroResult<T> {
    Err(HeroError::InvalidField { field, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RewardTier;
    use crate::hero::{
        AncestryId, BackgroundId, BackgroundSelection, ClassSelection, EquipmentId,
        EquipmentSelection, FightingStyleId, HERO_CHARACTER_SCHEMA_VERSION,
        HERO_COMMAND_SCHEMA_VERSION, HeroChoices, HeroConceptId, HeroPins, HeroPresentation,
        LEVEL_TWO_XP, LevelUpCommand, RewardAwardCommand, SkillId, StandardArrayAssignment,
        SupportedLevel, ThemeId, TrustedMutationContext, TrustedRewardPolicy,
    };

    fn test_choices(theme: ThemeId) -> HeroChoices {
        HeroChoices {
            pins: HeroPins::mvp(theme),
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
                class_skills: vec![SkillId::Perception, SkillId::Survival],
            },
            equipment: EquipmentSelection {
                carried: vec![
                    EquipmentId::Longsword,
                    EquipmentId::LightCrossbow,
                    EquipmentId::ChainMail,
                    EquipmentId::ExplorersPack,
                ],
                simple_weapon: None,
                equipped_armor: Some(EquipmentId::ChainMail),
                shield_equipped: false,
            },
            wizard_spells: None,
            presentation: HeroPresentation {
                name: "Test Hero".to_owned(),
                pronouns: "they/them".to_owned(),
                appearance: "A weathered adventurer".to_owned(),
                ideal: "Justice for all".to_owned(),
                bond: "Owes a life debt".to_owned(),
                flaw: "Too trusting".to_owned(),
                tone_limits: Vec::new(),
            },
        }
    }

    fn test_character() -> PlayerCharacter {
        PlayerCharacter::new(
            "character:test-1234567890".to_owned(),
            "account:test-player-account".to_owned(),
            "Test Hero".to_owned(),
            test_choices(ThemeId::RainboundBorough),
        )
        .expect("test character should be valid")
    }

    #[test]
    fn library_character_has_no_campaign_or_runtime_fields() {
        let character = test_character();
        let json = serde_json::to_string(&character).unwrap();
        // Must NOT contain campaign runtime fields.
        assert!(!json.contains("campaign_id"));
        assert!(!json.contains("level"));
        assert!(!json.contains("experience_points"));
        assert!(!json.contains("maximum_hit_points"));
        assert!(!json.contains("current_hit_points"));
        assert!(!json.contains("advancement_choices"));
        assert!(!json.contains("sheet"));
        // Must contain library fields.
        assert!(json.contains("character_id"));
        assert!(json.contains("owner_account_id"));
        assert!(json.contains("display_name"));
        assert!(json.contains("choices"));
    }

    #[test]
    fn library_character_denies_unknown_fields() {
        let character = test_character();
        let mut json = serde_json::to_value(&character).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.insert("level".to_owned(), serde_json::json!(5));
        let result: Result<PlayerCharacter, _> = serde_json::from_value(json);
        assert!(
            result.is_err(),
            "deserialization should reject unknown fields"
        );
    }

    #[test]
    fn library_character_validates_owner_identity_and_choices() {
        let character = test_character();
        assert_eq!(character.character_id, "character:test-1234567890");
        assert_eq!(character.owner_account_id, "account:test-player-account");
        assert_eq!(character.display_name, "Test Hero");
        assert_eq!(character.revision, 0);
        assert!(character.validate().is_ok());

        // Invalid character ID (contains spaces).
        assert!(
            PlayerCharacter::new(
                "has spaces".to_owned(),
                "account:test".to_owned(),
                "Test".to_owned(),
                test_choices(ThemeId::RainboundBorough),
            )
            .is_err()
        );

        // Invalid account ID (no prefix).
        assert!(
            PlayerCharacter::new(
                "character:valid-id-1234567".to_owned(),
                "not-an-account".to_owned(),
                "Test".to_owned(),
                test_choices(ThemeId::RainboundBorough),
            )
            .is_err()
        );

        // Empty display name.
        assert!(
            PlayerCharacter::new(
                "character:valid-id-1234567".to_owned(),
                "account:test".to_owned(),
                "  ".to_owned(),
                test_choices(ThemeId::RainboundBorough),
            )
            .is_err()
        );

        // Display name too long.
        assert!(
            PlayerCharacter::new(
                "character:valid-id-1234567".to_owned(),
                "account:test".to_owned(),
                "x".repeat(201),
                test_choices(ThemeId::RainboundBorough),
            )
            .is_err()
        );
    }

    #[test]
    fn local_compatibility_account_id_is_accepted() {
        let character = PlayerCharacter::new(
            "character:local-test-char-0".to_owned(),
            "account:local".to_owned(),
            "Local Hero".to_owned(),
            test_choices(ThemeId::RainboundBorough),
        )
        .expect("local account ID should be accepted");
        assert!(character.is_owned_by("account:local"));
    }

    #[test]
    fn bump_revision_increments_without_overflow() {
        let mut character = test_character();
        assert_eq!(character.revision, 0);
        character.bump_revision();
        assert_eq!(character.revision, 1);
        character.bump_revision();
        assert_eq!(character.revision, 2);
    }

    #[test]
    fn instantiate_for_campaign_creates_independent_level_one_hero() {
        let library_character = test_character();

        let hero_a = library_character
            .instantiate_for_campaign(
                "campaign:aaa-1111111111".to_owned(),
                "character:runtime-aaa-11111".to_owned(),
            )
            .expect("instantiation should succeed");

        let hero_b = library_character
            .instantiate_for_campaign(
                "campaign:bbb-2222222222".to_owned(),
                "character:runtime-bbb-22222".to_owned(),
            )
            .expect("instantiation should succeed");

        // Both heroes start at level 1 with 0 XP.
        assert_eq!(hero_a.level, SupportedLevel::One);
        assert_eq!(hero_a.experience_points, 0);
        assert_eq!(hero_b.level, SupportedLevel::One);
        assert_eq!(hero_b.experience_points, 0);

        // They are bound to different campaigns.
        assert_ne!(hero_a.campaign_id, hero_b.campaign_id);
        assert_ne!(hero_a.character_id, hero_b.character_id);

        // Both share the same source choices but are independent instances.
        assert_eq!(hero_a.choices, hero_b.choices);
        assert_eq!(hero_a.owner_id, hero_b.owner_id);

        // The schema version of the runtime hero is the hero character schema,
        // not the player character library schema.
        assert_eq!(hero_a.schema_version, HERO_CHARACTER_SCHEMA_VERSION);

        // The library character is unchanged after instantiation.
        assert_eq!(library_character.revision, 0);
    }

    #[test]
    fn two_campaign_instances_advance_independently() {
        let library_character = test_character();
        let original_revision = library_character.revision;

        // Two independent campaign instances from the same library character.
        let mut hero_a = library_character
            .instantiate_for_campaign(
                "campaign:aaa-1111111111".to_owned(),
                "character:runtime-aaa-11111".to_owned(),
            )
            .expect("instantiation of hero_a should succeed");
        let mut hero_b = library_character
            .instantiate_for_campaign(
                "campaign:bbb-2222222222".to_owned(),
                "character:runtime-bbb-22222".to_owned(),
            )
            .expect("instantiation of hero_b should succeed");

        // Both heroes start at level 1 with 0 XP and identical sheets.
        assert_eq!(hero_a.level, SupportedLevel::One);
        assert_eq!(hero_b.level, SupportedLevel::One);
        assert_eq!(hero_a.experience_points, 0);
        assert_eq!(hero_b.experience_points, 0);
        assert_eq!(hero_a.sheet, hero_b.sheet);

        // --- Advance hero_a to level 2; leave hero_b untouched. ---
        // Award a Major-tier reward (LEVEL_TWO_XP = 300 XP) to hero_a.
        hero_a
            .apply_reward(
                &RewardAwardCommand {
                    schema_version: HERO_COMMAND_SCHEMA_VERSION,
                    character_id: hero_a.character_id.clone(),
                    expected_revision: 0,
                    idempotency_key: "reward-hero-a-major".to_owned(),
                    tier: RewardTier::Major,
                },
                TrustedRewardPolicy::MvpXpV1,
                &TrustedMutationContext {
                    audit_id: "audit:reward-hero-a-0001".to_owned(),
                    actor_id: "account:test-player-account".to_owned(),
                    occurred_at_epoch_seconds: 1000,
                },
            )
            .expect("reward should apply to hero_a");
        assert_eq!(hero_a.experience_points, LEVEL_TWO_XP);
        assert!(hero_a.level_up_eligible());
        // hero_b is untouched by hero_a's advancement.
        assert_eq!(hero_b.experience_points, 0);
        assert!(!hero_b.level_up_eligible());

        // Apply the level-up to hero_a.
        let choice = hero_a
            .valid_level_up_choices()
            .expect("level-up choices should be available")
            .remove(0);
        let hp_before_a = hero_a.sheet.maximum_hit_points;
        hero_a
            .level_up(
                &LevelUpCommand {
                    schema_version: HERO_COMMAND_SCHEMA_VERSION,
                    character_id: hero_a.character_id.clone(),
                    expected_revision: 1,
                    idempotency_key: "level-up-hero-a-0001".to_owned(),
                    choice,
                },
                &TrustedMutationContext {
                    audit_id: "audit:level-up-hero-a-0001".to_owned(),
                    actor_id: "account:test-player-account".to_owned(),
                    occurred_at_epoch_seconds: 1001,
                },
            )
            .expect("level-up should apply to hero_a");
        assert_eq!(hero_a.level, SupportedLevel::Two);
        assert!(hero_a.sheet.maximum_hit_points > hp_before_a);

        // --- hero_b is still level 1 with 0 XP and the original sheet. ---
        assert_eq!(hero_b.level, SupportedLevel::One);
        assert_eq!(hero_b.experience_points, 0);
        assert_eq!(hero_b.revision, 0);
        assert_eq!(hero_b.sheet.maximum_hit_points, hp_before_a);
        assert!(!hero_b.level_up_eligible());

        // Awarding a Minor-tier reward to hero_b does not affect hero_a.
        hero_b
            .apply_reward(
                &RewardAwardCommand {
                    schema_version: HERO_COMMAND_SCHEMA_VERSION,
                    character_id: hero_b.character_id.clone(),
                    expected_revision: 0,
                    idempotency_key: "reward-hero-b-minor".to_owned(),
                    tier: RewardTier::Minor,
                },
                TrustedRewardPolicy::MvpXpV1,
                &TrustedMutationContext {
                    audit_id: "audit:reward-hero-b-0001".to_owned(),
                    actor_id: "account:test-player-account".to_owned(),
                    occurred_at_epoch_seconds: 2000,
                },
            )
            .expect("reward should apply to hero_b");
        assert_eq!(hero_b.experience_points, 50);
        assert_eq!(hero_a.experience_points, LEVEL_TWO_XP);

        // The two instances now differ in level, XP, revision, and max HP.
        assert_ne!(hero_a.level, hero_b.level);
        assert_ne!(hero_a.experience_points, hero_b.experience_points);
        assert_ne!(hero_a.revision, hero_b.revision);
        assert_ne!(
            hero_a.sheet.maximum_hit_points,
            hero_b.sheet.maximum_hit_points
        );

        // The library character is never mutated by either instance.
        assert_eq!(library_character.revision, original_revision);
    }

    #[test]
    fn library_character_preserves_theme_id() {
        let rainbound = PlayerCharacter::new(
            "character:rainbound-test-00".to_owned(),
            "account:test".to_owned(),
            "Rainbound Hero".to_owned(),
            test_choices(ThemeId::RainboundBorough),
        )
        .unwrap();
        assert_eq!(rainbound.theme_id(), ThemeId::RainboundBorough);

        let emberline = PlayerCharacter::new(
            "character:emberline-test-00".to_owned(),
            "account:test".to_owned(),
            "Emberline Hero".to_owned(),
            test_choices(ThemeId::EmberlineArchive),
        )
        .unwrap();
        assert_eq!(emberline.theme_id(), ThemeId::EmberlineArchive);
    }

    #[test]
    fn serialization_round_trip_preserves_all_fields() {
        let character = test_character();
        let json = serde_json::to_string(&character).unwrap();
        let restored: PlayerCharacter = serde_json::from_str(&json).unwrap();
        assert_eq!(character, restored);
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let mut character = test_character();
        character.schema_version = 999;
        assert!(character.validate().is_err());
    }

    #[test]
    fn control_characters_in_display_name_are_rejected() {
        assert!(
            PlayerCharacter::new(
                "character:valid-id-1234567".to_owned(),
                "account:test".to_owned(),
                "Name\u{0000}WithNull".to_owned(),
                test_choices(ThemeId::RainboundBorough),
            )
            .is_err()
        );
    }

    #[test]
    fn content_pack_id_is_not_accepted_as_account_id() {
        assert!(
            PlayerCharacter::new(
                "character:valid-id-1234567".to_owned(),
                "dev.manchester-arcana.core-mvp".to_owned(),
                "Test".to_owned(),
                test_choices(ThemeId::RainboundBorough),
            )
            .is_err()
        );
    }
}
