use serde::{Deserialize, Serialize};

use crate::{GameCoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Attack,
    CastSpell,
    Dash,
    Disengage,
    Dodge,
    Help,
    Hide,
    Ready,
    Search,
    UseObject,
    Improvised,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnResource {
    Action,
    BonusAction,
    Reaction,
    ObjectInteraction,
}

impl TurnResource {
    const fn name(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::BonusAction => "bonus_action",
            Self::Reaction => "reaction",
            Self::ObjectInteraction => "object_interaction",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionEconomy {
    pub action_available: bool,
    pub bonus_action_available: bool,
    pub reaction_available: bool,
    pub object_interaction_available: bool,
    pub movement_remaining_feet: u16,
}

impl ActionEconomy {
    pub const fn new(speed_feet: u16) -> Self {
        Self {
            action_available: true,
            // A bonus action only exists when a spell, feature, or other rule
            // grants one; merely starting a turn does not grant it.
            bonus_action_available: false,
            reaction_available: true,
            object_interaction_available: true,
            movement_remaining_feet: speed_feet,
        }
    }

    pub fn grant_bonus_action(&mut self) {
        self.bonus_action_available = true;
    }

    pub fn spend(&mut self, resource: TurnResource) -> Result<()> {
        let available = match resource {
            TurnResource::Action => &mut self.action_available,
            TurnResource::BonusAction => &mut self.bonus_action_available,
            TurnResource::Reaction => &mut self.reaction_available,
            TurnResource::ObjectInteraction => &mut self.object_interaction_available,
        };

        if !*available {
            return Err(GameCoreError::TurnResourceUnavailable {
                resource: resource.name(),
            });
        }
        *available = false;
        Ok(())
    }

    pub fn spend_movement(&mut self, feet: u16) -> Result<()> {
        if feet > self.movement_remaining_feet {
            return Err(GameCoreError::InsufficientMovement {
                requested: feet,
                remaining: self.movement_remaining_feet,
            });
        }
        self.movement_remaining_feet -= feet;
        Ok(())
    }

    pub fn reset_for_turn(&mut self, speed_feet: u16) {
        *self = Self::new(speed_feet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_resource_can_only_be_spent_once() {
        let mut economy = ActionEconomy::new(30);
        economy.spend(TurnResource::Action).unwrap();

        assert_eq!(
            economy.spend(TurnResource::Action),
            Err(GameCoreError::TurnResourceUnavailable { resource: "action" })
        );
    }

    #[test]
    fn bonus_action_requires_an_explicit_grant() {
        let mut economy = ActionEconomy::new(30);
        assert_eq!(
            economy.spend(TurnResource::BonusAction),
            Err(GameCoreError::TurnResourceUnavailable {
                resource: "bonus_action"
            })
        );

        economy.grant_bonus_action();
        economy.spend(TurnResource::BonusAction).unwrap();
        assert!(!economy.bonus_action_available);
    }

    #[test]
    fn movement_cannot_exceed_remaining_speed() {
        let mut economy = ActionEconomy::new(30);
        economy.spend_movement(20).unwrap();

        assert_eq!(economy.movement_remaining_feet, 10);
        assert_eq!(
            economy.spend_movement(15),
            Err(GameCoreError::InsufficientMovement {
                requested: 15,
                remaining: 10
            })
        );
    }

    #[test]
    fn reset_restores_all_turn_resources() {
        let mut economy = ActionEconomy::new(30);
        economy.spend(TurnResource::Reaction).unwrap();
        economy.spend_movement(30).unwrap();
        economy.reset_for_turn(25);

        assert!(economy.reaction_available);
        assert_eq!(economy.movement_remaining_feet, 25);
    }
}
