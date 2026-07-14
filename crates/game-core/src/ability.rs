use serde::{Deserialize, Serialize};

use crate::{GameCoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ability {
    Strength,
    Dexterity,
    Constitution,
    Intelligence,
    Wisdom,
    Charisma,
}

impl Ability {
    pub const ALL: [Self; 6] = [
        Self::Strength,
        Self::Dexterity,
        Self::Constitution,
        Self::Intelligence,
        Self::Wisdom,
        Self::Charisma,
    ];
}

/// A rules-valid ability score for creatures (1 through 30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct AbilityScore(u8);

impl AbilityScore {
    pub fn new(score: u8) -> Result<Self> {
        if (1..=30).contains(&score) {
            Ok(Self(score))
        } else {
            Err(GameCoreError::InvalidAbilityScore { score })
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }

    /// Computes `(score - 10) / 2`, rounding toward negative infinity.
    pub fn modifier(self) -> i8 {
        (i16::from(self.0) - 10).div_euclid(2) as i8
    }
}

impl TryFrom<u8> for AbilityScore {
    type Error = GameCoreError;

    fn try_from(value: u8) -> Result<Self> {
        Self::new(value)
    }
}

impl From<AbilityScore> for u8 {
    fn from(value: AbilityScore) -> Self {
        value.value()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbilityScores {
    strength: AbilityScore,
    dexterity: AbilityScore,
    constitution: AbilityScore,
    intelligence: AbilityScore,
    wisdom: AbilityScore,
    charisma: AbilityScore,
}

impl AbilityScores {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        strength: u8,
        dexterity: u8,
        constitution: u8,
        intelligence: u8,
        wisdom: u8,
        charisma: u8,
    ) -> Result<Self> {
        Ok(Self {
            strength: AbilityScore::new(strength)?,
            dexterity: AbilityScore::new(dexterity)?,
            constitution: AbilityScore::new(constitution)?,
            intelligence: AbilityScore::new(intelligence)?,
            wisdom: AbilityScore::new(wisdom)?,
            charisma: AbilityScore::new(charisma)?,
        })
    }

    pub fn get(&self, ability: Ability) -> AbilityScore {
        match ability {
            Ability::Strength => self.strength,
            Ability::Dexterity => self.dexterity,
            Ability::Constitution => self.constitution,
            Ability::Intelligence => self.intelligence,
            Ability::Wisdom => self.wisdom,
            Ability::Charisma => self.charisma,
        }
    }

    pub fn validate(&self) -> Result<()> {
        for ability in Ability::ALL {
            AbilityScore::new(self.get(ability).value())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_math_rounds_down_for_odd_negative_values() {
        let cases = [(1, -5), (2, -4), (9, -1), (10, 0), (11, 0), (30, 10)];

        for (score, expected) in cases {
            assert_eq!(AbilityScore::new(score).unwrap().modifier(), expected);
        }
    }

    #[test]
    fn rejects_scores_outside_creature_range() {
        assert_eq!(
            AbilityScore::new(0),
            Err(GameCoreError::InvalidAbilityScore { score: 0 })
        );
        assert_eq!(
            AbilityScore::new(31),
            Err(GameCoreError::InvalidAbilityScore { score: 31 })
        );
    }

    #[test]
    fn all_six_scores_are_addressable() {
        let scores = AbilityScores::new(8, 9, 10, 11, 12, 13).unwrap();
        let values: Vec<_> = Ability::ALL
            .into_iter()
            .map(|ability| scores.get(ability).value())
            .collect();

        assert_eq!(values, vec![8, 9, 10, 11, 12, 13]);
    }

    #[test]
    fn deserialization_enforces_score_validation() {
        assert!(serde_json::from_str::<AbilityScore>("0").is_err());
        assert_eq!(
            serde_json::from_str::<AbilityScore>("20").unwrap(),
            AbilityScore::new(20).unwrap()
        );
    }
}
