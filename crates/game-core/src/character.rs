use serde::{Deserialize, Serialize};

use crate::{AbilityScores, GameCoreError, Level, Result, is_valid_opaque_id};

const MAX_CHARACTER_TEXT_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterDraft {
    pub id: String,
    pub name: String,
    /// Free-form setting or creation theme selected by the player.
    pub theme: String,
    pub ability_scores: AbilityScores,
    pub experience_points: u32,
    pub current_hit_points: u32,
    pub maximum_hit_points: u32,
}

impl CharacterDraft {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("id", &self.id)?;
        validate_text_field("name", &self.name, MAX_CHARACTER_TEXT_CHARS)?;
        validate_text_field("theme", &self.theme, MAX_CHARACTER_TEXT_CHARS)?;
        self.ability_scores.validate()?;
        validate_hit_points(self.current_hit_points, self.maximum_hit_points)
    }

    pub fn build(self) -> Result<Character> {
        self.validate()?;
        let level = Level::from_experience(self.experience_points);
        Ok(Character {
            id: self.id,
            name: self.name,
            theme: self.theme,
            ability_scores: self.ability_scores,
            experience_points: self.experience_points,
            level,
            current_hit_points: self.current_hit_points,
            maximum_hit_points: self.maximum_hit_points,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Character {
    id: String,
    name: String,
    theme: String,
    ability_scores: AbilityScores,
    experience_points: u32,
    level: Level,
    current_hit_points: u32,
    maximum_hit_points: u32,
}

impl Character {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn theme(&self) -> &str {
        &self.theme
    }

    pub fn ability_scores(&self) -> &AbilityScores {
        &self.ability_scores
    }

    pub const fn experience_points(&self) -> u32 {
        self.experience_points
    }

    pub const fn level(&self) -> Level {
        self.level
    }

    pub const fn current_hit_points(&self) -> u32 {
        self.current_hit_points
    }

    pub const fn maximum_hit_points(&self) -> u32 {
        self.maximum_hit_points
    }

    /// Validates state loaded from persistence before it is used by the engine.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("id", &self.id)?;
        validate_text_field("name", &self.name, MAX_CHARACTER_TEXT_CHARS)?;
        validate_text_field("theme", &self.theme, MAX_CHARACTER_TEXT_CHARS)?;
        self.ability_scores.validate()?;
        validate_hit_points(self.current_hit_points, self.maximum_hit_points)?;

        let expected_level = Level::from_experience(self.experience_points);
        if self.level != expected_level {
            return Err(GameCoreError::LevelExperienceMismatch {
                level: self.level.value(),
                experience_points: self.experience_points,
                expected_level: expected_level.value(),
            });
        }
        Ok(())
    }

    /// Awards cumulative XP atomically and reports every newly attained level.
    pub fn award_experience(&mut self, amount: u32) -> Result<ExperienceAwardSummary> {
        self.validate()?;
        let previous_experience_points = self.experience_points;
        let previous_level = self.level;
        let experience_points = previous_experience_points
            .checked_add(amount)
            .ok_or(GameCoreError::ExperienceOverflow)?;
        let level = Level::from_experience(experience_points);
        let levels_gained = ((previous_level.value() + 1)..=level.value())
            .map(|value| Level::new(value).expect("bounded by two valid levels"))
            .collect();

        self.experience_points = experience_points;
        self.level = level;

        Ok(ExperienceAwardSummary {
            awarded: amount,
            previous_experience_points,
            experience_points,
            previous_level,
            level,
            levels_gained,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceAwardSummary {
    pub awarded: u32,
    pub previous_experience_points: u32,
    pub experience_points: u32,
    pub previous_level: Level,
    pub level: Level,
    pub levels_gained: Vec<Level>,
}

impl ExperienceAwardSummary {
    pub fn leveled_up(&self) -> bool {
        !self.levels_gained.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        if self.previous_experience_points.checked_add(self.awarded) != Some(self.experience_points)
        {
            return Err(GameCoreError::InvalidExperienceAwardSummary {
                reason: "the awarded amount does not produce the reported XP total",
            });
        }
        if self.previous_level != Level::from_experience(self.previous_experience_points)
            || self.level != Level::from_experience(self.experience_points)
        {
            return Err(GameCoreError::InvalidExperienceAwardSummary {
                reason: "reported levels do not match the XP totals",
            });
        }
        let expected_levels = ((self.previous_level.value() + 1)..=self.level.value())
            .map(|value| Level::new(value).expect("bounded by valid derived levels"))
            .collect::<Vec<_>>();
        if self.levels_gained != expected_levels {
            return Err(GameCoreError::InvalidExperienceAwardSummary {
                reason: "levels gained do not match the crossed thresholds",
            });
        }
        Ok(())
    }
}

fn validate_text_field(field: &'static str, value: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() {
        Err(GameCoreError::EmptyCharacterField { field })
    } else if value.chars().count() > maximum {
        Err(GameCoreError::TextFieldTooLong { field, maximum })
    } else {
        Ok(())
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<()> {
    if is_valid_opaque_id(value) {
        Ok(())
    } else {
        Err(GameCoreError::InvalidIdentifier { field })
    }
}

fn validate_hit_points(current: u32, maximum: u32) -> Result<()> {
    if maximum == 0 {
        Err(GameCoreError::InvalidMaximumHitPoints)
    } else if current > maximum {
        Err(GameCoreError::CurrentHitPointsExceedMaximum { current, maximum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(experience_points: u32) -> CharacterDraft {
        CharacterDraft {
            id: "character-1".into(),
            name: "Mara".into(),
            theme: "canal-side mystery".into(),
            ability_scores: AbilityScores::new(15, 14, 13, 12, 10, 8).unwrap(),
            experience_points,
            current_hit_points: 10,
            maximum_hit_points: 10,
        }
    }

    #[test]
    fn draft_validation_rejects_blank_fields_and_impossible_hit_points() {
        let mut blank = draft(0);
        blank.name = "  ".into();
        assert_eq!(
            blank.build(),
            Err(GameCoreError::EmptyCharacterField { field: "name" })
        );

        let mut impossible = draft(0);
        impossible.current_hit_points = 11;
        assert_eq!(
            impossible.build(),
            Err(GameCoreError::CurrentHitPointsExceedMaximum {
                current: 11,
                maximum: 10
            })
        );
    }

    #[test]
    fn draft_build_derives_level_from_experience() {
        let character = draft(6_500).build().unwrap();
        assert_eq!(character.level().value(), 5);
        character.validate().unwrap();
    }

    #[test]
    fn xp_award_reports_multiple_levels_crossed() {
        let mut character = draft(0).build().unwrap();
        let summary = character.award_experience(2_700).unwrap();
        let levels: Vec<_> = summary
            .levels_gained
            .iter()
            .map(|level| level.value())
            .collect();

        assert_eq!(levels, vec![2, 3, 4]);
        assert_eq!(summary.level.value(), 4);
        assert!(summary.leveled_up());
        assert_eq!(character.experience_points(), 2_700);
    }

    #[test]
    fn xp_overflow_does_not_partially_mutate_character() {
        let mut character = draft(u32::MAX).build().unwrap();
        let before = character.clone();

        assert_eq!(
            character.award_experience(1),
            Err(GameCoreError::ExperienceOverflow)
        );
        assert_eq!(character, before);
    }

    #[test]
    fn durable_character_json_rejects_unknown_fields() {
        let character = draft(0).build().unwrap();
        let mut json = serde_json::to_value(character).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("future_mechanic".to_owned(), serde_json::json!(true));

        assert!(serde_json::from_value::<Character>(json).is_err());
    }
}
