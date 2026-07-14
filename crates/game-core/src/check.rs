use serde::{Deserialize, Serialize};

use crate::{
    Ability, AbilityScores, D20Roll, DiceSource, Level, Proficiency, Result, RollContext,
    resolve_d20,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbilityCheck {
    pub ability: Ability,
    pub proficiency: Proficiency,
    pub difficulty_class: u16,
    pub situational_modifier: i16,
    pub roll_context: RollContext,
}

impl AbilityCheck {
    pub fn resolve(
        &self,
        ability_scores: &AbilityScores,
        level: Level,
        dice: &mut impl DiceSource,
    ) -> Result<AbilityCheckResult> {
        let roll = resolve_d20(dice, self.roll_context)?;
        let ability_modifier = ability_scores.get(self.ability).modifier();
        let proficiency_modifier = self.proficiency.bonus(level.proficiency_bonus());
        let total = i32::from(roll.selected)
            + i32::from(ability_modifier)
            + i32::from(proficiency_modifier)
            + i32::from(self.situational_modifier);

        Ok(AbilityCheckResult {
            roll,
            ability: self.ability,
            ability_modifier,
            proficiency_modifier,
            situational_modifier: self.situational_modifier,
            total,
            difficulty_class: self.difficulty_class,
            success: total >= i32::from(self.difficulty_class),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbilityCheckResult {
    pub roll: D20Roll,
    pub ability: Ability,
    pub ability_modifier: i8,
    pub proficiency_modifier: u8,
    pub situational_modifier: i16,
    pub total: i32,
    pub difficulty_class: u16,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackRoll {
    pub ability: Ability,
    pub proficiency: Proficiency,
    pub armor_class: u16,
    pub situational_modifier: i16,
    pub roll_context: RollContext,
}

impl AttackRoll {
    pub fn resolve(
        &self,
        ability_scores: &AbilityScores,
        level: Level,
        dice: &mut impl DiceSource,
    ) -> Result<AttackRollResult> {
        let roll = resolve_d20(dice, self.roll_context)?;
        let ability_modifier = ability_scores.get(self.ability).modifier();
        let proficiency_modifier = self.proficiency.bonus(level.proficiency_bonus());
        let total = i32::from(roll.selected)
            + i32::from(ability_modifier)
            + i32::from(proficiency_modifier)
            + i32::from(self.situational_modifier);
        let outcome = match roll.selected {
            20 => AttackOutcome::CriticalHit,
            1 => AttackOutcome::AutomaticMiss,
            _ if total >= i32::from(self.armor_class) => AttackOutcome::Hit,
            _ => AttackOutcome::Miss,
        };

        Ok(AttackRollResult {
            roll,
            ability: self.ability,
            ability_modifier,
            proficiency_modifier,
            situational_modifier: self.situational_modifier,
            total,
            armor_class: self.armor_class,
            outcome,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackOutcome {
    CriticalHit,
    Hit,
    Miss,
    AutomaticMiss,
}

impl AttackOutcome {
    pub const fn is_hit(self) -> bool {
        matches!(self, Self::CriticalHit | Self::Hit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackRollResult {
    pub roll: D20Roll,
    pub ability: Ability,
    pub ability_modifier: i8,
    pub proficiency_modifier: u8,
    pub situational_modifier: i16,
    pub total: i32,
    pub armor_class: u16,
    pub outcome: AttackOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scores() -> AbilityScores {
        AbilityScores::new(16, 14, 12, 10, 8, 6).unwrap()
    }

    #[test]
    fn ability_check_combines_score_proficiency_and_situational_modifiers() {
        let check = AbilityCheck {
            ability: Ability::Strength,
            proficiency: Proficiency::Expertise,
            difficulty_class: 20,
            situational_modifier: -1,
            roll_context: RollContext::normal(),
        };
        let mut dice = |_| 12;
        let result = check
            .resolve(&scores(), Level::new(5).unwrap(), &mut dice)
            .unwrap();

        assert_eq!(result.total, 20); // 12 + 3 + (2 * 3) - 1
        assert!(result.success);
    }

    #[test]
    fn natural_twenty_does_not_automatically_pass_an_ability_check() {
        let check = AbilityCheck {
            ability: Ability::Charisma,
            proficiency: Proficiency::None,
            difficulty_class: 30,
            situational_modifier: 0,
            roll_context: RollContext::normal(),
        };
        let mut dice = |_| 20;
        let result = check
            .resolve(&scores(), Level::new(1).unwrap(), &mut dice)
            .unwrap();

        assert_eq!(result.total, 18);
        assert!(!result.success);
    }

    #[test]
    fn natural_twenty_is_a_critical_hit_regardless_of_total() {
        let attack = AttackRoll {
            ability: Ability::Charisma,
            proficiency: Proficiency::None,
            armor_class: 99,
            situational_modifier: -100,
            roll_context: RollContext::normal(),
        };
        let mut dice = |_| 20;
        let result = attack
            .resolve(&scores(), Level::new(1).unwrap(), &mut dice)
            .unwrap();

        assert_eq!(result.outcome, AttackOutcome::CriticalHit);
        assert!(result.outcome.is_hit());
    }

    #[test]
    fn natural_one_is_an_automatic_miss_regardless_of_total() {
        let attack = AttackRoll {
            ability: Ability::Strength,
            proficiency: Proficiency::Expertise,
            armor_class: 1,
            situational_modifier: 100,
            roll_context: RollContext::normal(),
        };
        let mut dice = |_| 1;
        let result = attack
            .resolve(&scores(), Level::new(20).unwrap(), &mut dice)
            .unwrap();

        assert_eq!(result.outcome, AttackOutcome::AutomaticMiss);
        assert!(!result.outcome.is_hit());
    }
}
