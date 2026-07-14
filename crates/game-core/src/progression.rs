use serde::{Deserialize, Serialize};

use crate::{GameCoreError, Result};

pub const XP_THRESHOLDS: [u32; 20] = [
    0, 300, 900, 2_700, 6_500, 14_000, 23_000, 34_000, 48_000, 64_000, 85_000, 100_000, 120_000,
    140_000, 165_000, 195_000, 225_000, 265_000, 305_000, 355_000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Level(u8);

impl Level {
    pub fn new(value: u8) -> Result<Self> {
        if (1..=20).contains(&value) {
            Ok(Self(value))
        } else {
            Err(GameCoreError::InvalidLevel { level: value })
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }

    pub const fn proficiency_bonus(self) -> u8 {
        2 + (self.0 - 1) / 4
    }

    pub fn minimum_experience(self) -> u32 {
        XP_THRESHOLDS[usize::from(self.0 - 1)]
    }

    pub fn from_experience(experience_points: u32) -> Self {
        let attained = XP_THRESHOLDS.partition_point(|threshold| *threshold <= experience_points);
        // The first threshold is zero, so at least one level is always attained.
        Self(attained.min(20) as u8)
    }
}

impl TryFrom<u8> for Level {
    type Error = GameCoreError;

    fn try_from(value: u8) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Level> for u8 {
    fn from(value: Level) -> Self {
        value.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_boundaries_map_to_levels() {
        assert_eq!(Level::from_experience(0).value(), 1);
        assert_eq!(Level::from_experience(299).value(), 1);
        assert_eq!(Level::from_experience(300).value(), 2);
        assert_eq!(Level::from_experience(354_999).value(), 19);
        assert_eq!(Level::from_experience(355_000).value(), 20);
        assert_eq!(Level::from_experience(u32::MAX).value(), 20);
    }

    #[test]
    fn proficiency_bonus_changes_at_expected_boundaries() {
        let cases = [
            (1, 2),
            (4, 2),
            (5, 3),
            (8, 3),
            (9, 4),
            (13, 5),
            (17, 6),
            (20, 6),
        ];

        for (level, expected) in cases {
            assert_eq!(Level::new(level).unwrap().proficiency_bonus(), expected);
        }
    }

    #[test]
    fn rejects_levels_outside_one_through_twenty() {
        assert!(Level::new(0).is_err());
        assert!(Level::new(21).is_err());
    }
}
