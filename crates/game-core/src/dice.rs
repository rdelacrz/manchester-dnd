use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{GameCoreError, Result};

/// Supplies die results to the rules engine. Implementations own all randomness.
pub trait DiceSource {
    fn roll(&mut self, sides: u16) -> u16;
}

impl<F> DiceSource for F
where
    F: FnMut(u16) -> u16,
{
    fn roll(&mut self, sides: u16) -> u16 {
        self(sides)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollMode {
    Normal,
    Advantage,
    Disadvantage,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollContext {
    pub advantage_sources: u8,
    pub disadvantage_sources: u8,
}

impl RollContext {
    pub const fn normal() -> Self {
        Self {
            advantage_sources: 0,
            disadvantage_sources: 0,
        }
    }

    pub const fn with_advantage() -> Self {
        Self {
            advantage_sources: 1,
            disadvantage_sources: 0,
        }
    }

    pub const fn with_disadvantage() -> Self {
        Self {
            advantage_sources: 0,
            disadvantage_sources: 1,
        }
    }

    /// Any amount of advantage and disadvantage together cancels to one roll.
    pub const fn effective_mode(self) -> RollMode {
        match (self.advantage_sources > 0, self.disadvantage_sources > 0) {
            (true, false) => RollMode::Advantage,
            (false, true) => RollMode::Disadvantage,
            _ => RollMode::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct D20Roll {
    pub mode: RollMode,
    pub first: u8,
    pub second: Option<u8>,
    pub selected: u8,
}

impl D20Roll {
    /// Validates a result reconstructed from an untrusted or persisted source.
    pub fn validate(&self) -> Result<()> {
        if !(1..=20).contains(&self.first)
            || self
                .second
                .is_some_and(|second| !(1..=20).contains(&second))
        {
            return Err(GameCoreError::InvalidD20Roll {
                reason: "die values must be between 1 and 20",
            });
        }
        let expected = match (self.mode, self.second) {
            (RollMode::Normal, None) => self.first,
            (RollMode::Advantage, Some(second)) => self.first.max(second),
            (RollMode::Disadvantage, Some(second)) => self.first.min(second),
            (RollMode::Normal, Some(_)) => {
                return Err(GameCoreError::InvalidD20Roll {
                    reason: "a normal roll must contain exactly one die",
                });
            }
            (RollMode::Advantage | RollMode::Disadvantage, None) => {
                return Err(GameCoreError::InvalidD20Roll {
                    reason: "advantage and disadvantage require two dice",
                });
            }
        };
        if self.selected != expected {
            return Err(GameCoreError::InvalidD20Roll {
                reason: "selected die does not match the roll mode",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for D20Roll {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRoll {
            mode: RollMode,
            first: u8,
            second: Option<u8>,
            selected: u8,
        }

        let wire = WireRoll::deserialize(deserializer)?;
        let roll = Self {
            mode: wire.mode,
            first: wire.first,
            second: wire.second,
            selected: wire.selected,
        };
        roll.validate().map_err(D::Error::custom)?;
        Ok(roll)
    }
}

pub fn resolve_d20(source: &mut impl DiceSource, context: RollContext) -> Result<D20Roll> {
    let mode = context.effective_mode();
    let first = roll_validated(source, 20)? as u8;
    let second = match mode {
        RollMode::Normal => None,
        RollMode::Advantage | RollMode::Disadvantage => Some(roll_validated(source, 20)? as u8),
    };
    let selected = match (mode, second) {
        (RollMode::Advantage, Some(other)) => first.max(other),
        (RollMode::Disadvantage, Some(other)) => first.min(other),
        _ => first,
    };

    Ok(D20Roll {
        mode,
        first,
        second,
        selected,
    })
}

fn roll_validated(source: &mut impl DiceSource, sides: u16) -> Result<u16> {
    let value = source.roll(sides);
    if (1..=sides).contains(&value) {
        Ok(value)
    } else {
        Err(GameCoreError::InvalidDieRoll { sides, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SequenceDice {
        values: std::collections::VecDeque<u16>,
    }

    impl SequenceDice {
        fn new(values: impl IntoIterator<Item = u16>) -> Self {
            Self {
                values: values.into_iter().collect(),
            }
        }
    }

    impl DiceSource for SequenceDice {
        fn roll(&mut self, sides: u16) -> u16 {
            assert_eq!(sides, 20);
            self.values.pop_front().expect("test die value")
        }
    }

    #[test]
    fn advantage_keeps_the_higher_roll() {
        let mut dice = SequenceDice::new([3, 17]);
        let roll = resolve_d20(&mut dice, RollContext::with_advantage()).unwrap();

        assert_eq!(roll.selected, 17);
        assert_eq!(roll.second, Some(17));
    }

    #[test]
    fn disadvantage_keeps_the_lower_roll() {
        let mut dice = SequenceDice::new([19, 4]);
        let roll = resolve_d20(&mut dice, RollContext::with_disadvantage()).unwrap();

        assert_eq!(roll.selected, 4);
    }

    #[test]
    fn any_opposed_sources_cancel_and_consume_one_roll() {
        let context = RollContext {
            advantage_sources: 3,
            disadvantage_sources: 1,
        };
        let mut dice = SequenceDice::new([12]);
        let roll = resolve_d20(&mut dice, context).unwrap();

        assert_eq!(roll.mode, RollMode::Normal);
        assert_eq!(roll.second, None);
        assert_eq!(roll.selected, 12);
    }

    #[test]
    fn rejects_out_of_range_values_from_a_source() {
        let mut dice = SequenceDice::new([21]);
        assert_eq!(
            resolve_d20(&mut dice, RollContext::normal()),
            Err(GameCoreError::InvalidDieRoll {
                sides: 20,
                value: 21
            })
        );
    }

    #[test]
    fn deserialization_rejects_an_impossible_selected_die() {
        let json = r#"{
            "mode":"advantage",
            "first":18,
            "second":4,
            "selected":4
        }"#;

        assert!(serde_json::from_str::<D20Roll>(json).is_err());
    }
}
