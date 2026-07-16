use std::{fmt, str::FromStr};

use rand_chacha::{
    ChaCha20Rng,
    rand_core::{RngCore, SeedableRng},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{RollContext, RollMode, RulesetId, identifier::is_valid_opaque_id};

/// Stable identifier for the only deterministic random stream supported by the MVP.
pub const CHACHA20_V1_ALGORITHM_ID: &str = "chacha20-v1";

/// A seed is supplied by trusted application code and is never part of a roll record.
pub type RollSeed = [u8; 32];

pub const MAX_DICE_EXPRESSION_LEN: usize = 64;
pub const MAX_DICE_COUNT: u16 = 100;
pub const MAX_DIE_SIDES: u32 = 10_000;
pub const MAX_DICE_CONSTANT_ABS: u32 = 100_000;
pub const MAX_ROLL_ABSOLUTE_TOTAL: i64 = 1_000_000;
pub const MAX_MODIFIER_COMPONENTS: usize = 16;

pub type RollResult<T> = std::result::Result<T, RollError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RollError {
    #[error("dice expressions must contain only ASCII characters")]
    NonAsciiExpression,

    #[error("dice expression exceeds its {maximum}-byte limit")]
    ExpressionTooLong { maximum: usize },

    #[error("dice expression must use canonical NdS, NdS+C, or NdS-C syntax")]
    InvalidExpressionSyntax,

    #[error("a numeric component in the dice expression overflowed")]
    ExpressionArithmeticOverflow,

    #[error("dice count must be between 1 and {maximum}, got {count}")]
    DiceCountOutOfRange { count: u64, maximum: u16 },

    #[error("die sides must be between 1 and {maximum}, got {sides}")]
    DieSidesOutOfRange { sides: u64, maximum: u32 },

    #[error("dice constant magnitude must not exceed {maximum}, got {magnitude}")]
    DiceConstantOutOfRange { magnitude: u64, maximum: u32 },

    #[error("dice expression can produce a total outside +/-{maximum}")]
    RollTotalOutOfRange { maximum: i64 },

    #[error("advantage or disadvantage is supported only for one d20")]
    UnsupportedRollMode,

    #[error("the deterministic RNG cursor is exhausted")]
    CursorExhausted,

    #[error("invalid roll record: {reason}")]
    InvalidRollRecord { reason: &'static str },
}

/// Bounded, canonical dice notation: `NdS` with an optional signed constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiceExpression {
    count: u16,
    sides: u32,
    constant: i32,
}

impl DiceExpression {
    pub fn new(count: u16, sides: u32, constant: i32) -> RollResult<Self> {
        validate_expression_parts(u64::from(count), u64::from(sides), i64::from(constant))?;
        Ok(Self {
            count,
            sides,
            constant,
        })
    }

    pub const fn count(self) -> u16 {
        self.count
    }

    pub const fn sides(self) -> u32 {
        self.sides
    }

    pub const fn constant(self) -> i32 {
        self.constant
    }
}

impl fmt::Display for DiceExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}d{}", self.count, self.sides)?;
        match self.constant.cmp(&0) {
            std::cmp::Ordering::Greater => write!(formatter, "+{}", self.constant),
            std::cmp::Ordering::Less => write!(formatter, "-{}", self.constant.unsigned_abs()),
            std::cmp::Ordering::Equal => Ok(()),
        }
    }
}

impl FromStr for DiceExpression {
    type Err = RollError;

    fn from_str(value: &str) -> RollResult<Self> {
        if !value.is_ascii() {
            return Err(RollError::NonAsciiExpression);
        }
        if value.len() > MAX_DICE_EXPRESSION_LEN {
            return Err(RollError::ExpressionTooLong {
                maximum: MAX_DICE_EXPRESSION_LEN,
            });
        }

        let bytes = value.as_bytes();
        let Some(d_index) = bytes.iter().position(|byte| *byte == b'd') else {
            return Err(RollError::InvalidExpressionSyntax);
        };
        let count_bytes = &bytes[..d_index];
        let tail = &bytes[d_index + 1..];
        if count_bytes.is_empty() || tail.is_empty() {
            return Err(RollError::InvalidExpressionSyntax);
        }

        let constant_index = tail.iter().position(|byte| matches!(*byte, b'+' | b'-'));
        let (sides_bytes, constant) = match constant_index {
            None => (tail, 0_i64),
            Some(index) => {
                let sides = &tail[..index];
                let signed = &tail[index..];
                if sides.is_empty() || signed.len() < 2 {
                    return Err(RollError::InvalidExpressionSyntax);
                }
                let magnitude = parse_ascii_u64(&signed[1..])?;
                let magnitude = i64::try_from(magnitude)
                    .map_err(|_| RollError::ExpressionArithmeticOverflow)?;
                let value = if signed[0] == b'-' {
                    magnitude
                        .checked_neg()
                        .ok_or(RollError::ExpressionArithmeticOverflow)?
                } else {
                    magnitude
                };
                (sides, value)
            }
        };

        let count = parse_ascii_u64(count_bytes)?;
        let sides = parse_ascii_u64(sides_bytes)?;
        validate_expression_parts(count, sides, constant)?;

        Ok(Self {
            count: count as u16,
            sides: sides as u32,
            constant: constant as i32,
        })
    }
}

impl Serialize for DiceExpression {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DiceExpression {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

fn parse_ascii_u64(bytes: &[u8]) -> RollResult<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(RollError::InvalidExpressionSyntax);
    }

    bytes.iter().try_fold(0_u64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or(RollError::ExpressionArithmeticOverflow)
    })
}

fn validate_expression_parts(count: u64, sides: u64, constant: i64) -> RollResult<()> {
    if !(1..=u64::from(MAX_DICE_COUNT)).contains(&count) {
        return Err(RollError::DiceCountOutOfRange {
            count,
            maximum: MAX_DICE_COUNT,
        });
    }
    if !(1..=u64::from(MAX_DIE_SIDES)).contains(&sides) {
        return Err(RollError::DieSidesOutOfRange {
            sides,
            maximum: MAX_DIE_SIDES,
        });
    }

    let constant_magnitude = constant.unsigned_abs();
    if constant_magnitude > u64::from(MAX_DICE_CONSTANT_ABS) {
        return Err(RollError::DiceConstantOutOfRange {
            magnitude: constant_magnitude,
            maximum: MAX_DICE_CONSTANT_ABS,
        });
    }

    let maximum_dice = count
        .checked_mul(sides)
        .ok_or(RollError::ExpressionArithmeticOverflow)?;
    let minimum_total = i128::from(count) + i128::from(constant);
    let maximum_total = i128::from(maximum_dice) + i128::from(constant);
    let bound = i128::from(MAX_ROLL_ABSOLUTE_TOTAL);
    if minimum_total < -bound || maximum_total > bound {
        return Err(RollError::RollTotalOutOfRange {
            maximum: MAX_ROLL_ABSOLUTE_TOTAL,
        });
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RollAlgorithm {
    #[default]
    #[serde(rename = "chacha20-v1")]
    ChaCha20V1,
}

impl RollAlgorithm {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChaCha20V1 => CHACHA20_V1_ALGORITHM_ID,
        }
    }
}

impl fmt::Display for RollAlgorithm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Dice facts produced before durable metadata is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiceRoll {
    pub expression: DiceExpression,
    pub rolled_dice: Vec<u32>,
    pub kept_dice: Vec<u32>,
    pub total: i32,
    pub roll_mode: RollMode,
    pub cursor_before: u64,
    pub cursor_after: u64,
}

/// ChaCha20 with a 32-byte seed, stream zero, and a cursor measured in raw u32 words.
///
/// Die mapping uses rejection sampling over those words. This definition, together
/// with [`CHACHA20_V1_ALGORITHM_ID`], is the persisted `chacha20-v1` contract.
#[derive(Clone)]
pub struct DeterministicRng {
    inner: ChaCha20Rng,
    cursor: u64,
}

impl fmt::Debug for DeterministicRng {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeterministicRng")
            .field("algorithm", &RollAlgorithm::ChaCha20V1)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

impl DeterministicRng {
    pub fn new(seed: RollSeed) -> Self {
        Self::at_cursor(seed, 0)
    }

    pub fn at_cursor(seed: RollSeed, cursor: u64) -> Self {
        let mut inner = ChaCha20Rng::from_seed(seed);
        inner.set_word_pos(u128::from(cursor));
        Self { inner, cursor }
    }

    pub const fn algorithm(&self) -> RollAlgorithm {
        RollAlgorithm::ChaCha20V1
    }

    pub const fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Rolls one unbiased die. On error, the cursor remains unchanged.
    pub fn roll_die(&mut self, sides: u32) -> RollResult<u32> {
        if !(1..=MAX_DIE_SIDES).contains(&sides) {
            return Err(RollError::DieSidesOutOfRange {
                sides: u64::from(sides),
                maximum: MAX_DIE_SIDES,
            });
        }

        let checkpoint = self.clone();
        match self.roll_die_inner(sides) {
            Ok(value) => Ok(value),
            Err(error) => {
                *self = checkpoint;
                Err(error)
            }
        }
    }

    /// Resolves normal dice or d20 advantage/disadvantage. On error, no cursor is spent.
    pub fn roll(
        &mut self,
        expression: DiceExpression,
        context: RollContext,
    ) -> RollResult<DiceRoll> {
        let mode = context.effective_mode();
        if mode != RollMode::Normal && (expression.count != 1 || expression.sides != 20) {
            return Err(RollError::UnsupportedRollMode);
        }

        let checkpoint = self.clone();
        let result = self.roll_inner(expression, mode);
        if result.is_err() {
            *self = checkpoint;
        }
        result
    }

    /// Resolves and validates a complete durable record atomically with respect to the cursor.
    pub fn roll_record(
        &mut self,
        expression: DiceExpression,
        context: RollContext,
        metadata: RollMetadata,
        modifiers: Vec<ModifierComponent>,
    ) -> RollResult<RollRecord> {
        let checkpoint = self.clone();
        let result = self
            .roll(expression, context)
            .and_then(|roll| RollRecord::from_roll(roll, metadata, modifiers));
        if result.is_err() {
            *self = checkpoint;
        }
        result
    }

    fn roll_inner(&mut self, expression: DiceExpression, mode: RollMode) -> RollResult<DiceRoll> {
        let cursor_before = self.cursor;
        let (rolled_dice, kept_dice) = match mode {
            RollMode::Normal => {
                let mut rolled = Vec::with_capacity(usize::from(expression.count));
                for _ in 0..expression.count {
                    rolled.push(self.roll_die_inner(expression.sides)?);
                }
                (rolled.clone(), rolled)
            }
            RollMode::Advantage | RollMode::Disadvantage => {
                let first = self.roll_die_inner(20)?;
                let second = self.roll_die_inner(20)?;
                let selected = if mode == RollMode::Advantage {
                    first.max(second)
                } else {
                    first.min(second)
                };
                (vec![first, second], vec![selected])
            }
        };

        let total = kept_dice
            .iter()
            .try_fold(i64::from(expression.constant), |total, die| {
                total.checked_add(i64::from(*die))
            })
            .ok_or(RollError::ExpressionArithmeticOverflow)?;
        let total = i32::try_from(total).map_err(|_| RollError::RollTotalOutOfRange {
            maximum: MAX_ROLL_ABSOLUTE_TOTAL,
        })?;

        Ok(DiceRoll {
            expression,
            rolled_dice,
            kept_dice,
            total,
            roll_mode: mode,
            cursor_before,
            cursor_after: self.cursor,
        })
    }

    fn roll_die_inner(&mut self, sides: u32) -> RollResult<u32> {
        let sample_space = u64::from(u32::MAX) + 1;
        let acceptance_limit = sample_space - (sample_space % u64::from(sides));
        loop {
            let sample = u64::from(self.next_word()?);
            if sample < acceptance_limit {
                return Ok((sample % u64::from(sides)) as u32 + 1);
            }
        }
    }

    fn next_word(&mut self) -> RollResult<u32> {
        let next_cursor = self
            .cursor
            .checked_add(1)
            .ok_or(RollError::CursorExhausted)?;
        let word = self.inner.next_u32();
        self.cursor = next_cursor;
        Ok(word)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModifierComponent {
    pub name: String,
    pub value: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollMetadata {
    pub roll_id: String,
    pub purpose: String,
    pub actor_id: String,
    pub target_id: Option<String>,
    pub ruleset: RulesetId,
    pub seed_reference: String,
}

/// Canonical durable facts for a single roll. Raw seed material is deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RollRecord {
    pub roll_id: String,
    pub expression: DiceExpression,
    pub rolled_dice: Vec<u32>,
    pub kept_dice: Vec<u32>,
    pub modifier_components: Vec<ModifierComponent>,
    pub total: i32,
    pub purpose: String,
    pub actor_id: String,
    pub target_id: Option<String>,
    pub roll_mode: RollMode,
    pub ruleset: RulesetId,
    pub algorithm_id: RollAlgorithm,
    pub seed_reference: String,
    pub cursor_before: u64,
    pub cursor_after: u64,
}

impl RollRecord {
    pub fn from_roll(
        roll: DiceRoll,
        metadata: RollMetadata,
        mut modifiers: Vec<ModifierComponent>,
    ) -> RollResult<Self> {
        modifiers.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let record = Self {
            roll_id: metadata.roll_id,
            expression: roll.expression,
            rolled_dice: roll.rolled_dice,
            kept_dice: roll.kept_dice,
            modifier_components: modifiers,
            total: roll.total,
            purpose: metadata.purpose,
            actor_id: metadata.actor_id,
            target_id: metadata.target_id,
            roll_mode: roll.roll_mode,
            ruleset: metadata.ruleset,
            algorithm_id: RollAlgorithm::ChaCha20V1,
            seed_reference: metadata.seed_reference,
            cursor_before: roll.cursor_before,
            cursor_after: roll.cursor_after,
        };
        record.validate()?;
        Ok(record)
    }

    /// Recomputes the record's shape, selection, modifier, total, and cursor invariants.
    pub fn validate(&self) -> RollResult<()> {
        for (field, value) in [
            ("roll_id", self.roll_id.as_str()),
            ("purpose", self.purpose.as_str()),
            ("actor_id", self.actor_id.as_str()),
            ("seed_reference", self.seed_reference.as_str()),
        ] {
            if !is_valid_opaque_id(value) {
                return Err(RollError::InvalidRollRecord {
                    reason: match field {
                        "roll_id" => "roll_id is not a valid opaque identifier",
                        "purpose" => "purpose is not a valid opaque identifier",
                        "actor_id" => "actor_id is not a valid opaque identifier",
                        _ => "seed_reference is not a valid opaque identifier",
                    },
                });
            }
        }
        if self
            .target_id
            .as_deref()
            .is_some_and(|id| !is_valid_opaque_id(id))
        {
            return Err(RollError::InvalidRollRecord {
                reason: "target_id is not a valid opaque identifier",
            });
        }
        if self.algorithm_id != RollAlgorithm::ChaCha20V1 {
            return Err(RollError::InvalidRollRecord {
                reason: "algorithm is not supported",
            });
        }
        if self.modifier_components.len() > MAX_MODIFIER_COMPONENTS {
            return Err(RollError::InvalidRollRecord {
                reason: "too many modifier components",
            });
        }

        let mut previous_name: Option<&str> = None;
        let modifier_total = self.modifier_components.iter().try_fold(
            0_i64,
            |total, component| -> RollResult<i64> {
                if !is_valid_opaque_id(&component.name) {
                    return Err(RollError::InvalidRollRecord {
                        reason: "modifier name is not a valid opaque identifier",
                    });
                }
                if previous_name.is_some_and(|previous| previous >= component.name.as_str()) {
                    return Err(RollError::InvalidRollRecord {
                        reason: "modifier names must be unique and canonically sorted",
                    });
                }
                previous_name = Some(&component.name);
                total
                    .checked_add(i64::from(component.value))
                    .ok_or(RollError::InvalidRollRecord {
                        reason: "modifier total overflowed",
                    })
            },
        )?;
        if modifier_total != i64::from(self.expression.constant) {
            return Err(RollError::InvalidRollRecord {
                reason: "modifier components do not equal the expression constant",
            });
        }

        let minimum_words = match self.roll_mode {
            RollMode::Normal => {
                if self.rolled_dice.len() != usize::from(self.expression.count)
                    || self.kept_dice != self.rolled_dice
                {
                    return Err(RollError::InvalidRollRecord {
                        reason: "normal rolls must roll and keep the expression's dice",
                    });
                }
                u64::from(self.expression.count)
            }
            RollMode::Advantage | RollMode::Disadvantage => {
                if self.expression.count != 1
                    || self.expression.sides != 20
                    || self.rolled_dice.len() != 2
                    || self.kept_dice.len() != 1
                {
                    return Err(RollError::InvalidRollRecord {
                        reason: "advantage and disadvantage require two rolled d20s and one kept die",
                    });
                }
                let expected = if self.roll_mode == RollMode::Advantage {
                    self.rolled_dice[0].max(self.rolled_dice[1])
                } else {
                    self.rolled_dice[0].min(self.rolled_dice[1])
                };
                if self.kept_dice[0] != expected {
                    return Err(RollError::InvalidRollRecord {
                        reason: "kept d20 does not match the roll mode",
                    });
                }
                2
            }
        };

        if self
            .rolled_dice
            .iter()
            .any(|die| !(1..=self.expression.sides).contains(die))
        {
            return Err(RollError::InvalidRollRecord {
                reason: "rolled die is outside the expression's side range",
            });
        }

        let expected_total = self
            .kept_dice
            .iter()
            .try_fold(modifier_total, |total, die| {
                total
                    .checked_add(i64::from(*die))
                    .ok_or(RollError::InvalidRollRecord {
                        reason: "roll total overflowed",
                    })
            })?;
        if expected_total != i64::from(self.total) {
            return Err(RollError::InvalidRollRecord {
                reason: "total does not equal kept dice plus modifiers",
            });
        }
        if expected_total.unsigned_abs() > MAX_ROLL_ABSOLUTE_TOTAL as u64 {
            return Err(RollError::InvalidRollRecord {
                reason: "total is outside the supported range",
            });
        }

        let spent_words = self.cursor_after.checked_sub(self.cursor_before).ok_or(
            RollError::InvalidRollRecord {
                reason: "cursor_after precedes cursor_before",
            },
        )?;
        if spent_words < minimum_words {
            return Err(RollError::InvalidRollRecord {
                reason: "cursor range is too short for the rolled dice",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RollRecord {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRecord {
            roll_id: String,
            expression: DiceExpression,
            rolled_dice: Vec<u32>,
            kept_dice: Vec<u32>,
            modifier_components: Vec<ModifierComponent>,
            total: i32,
            purpose: String,
            actor_id: String,
            target_id: Option<String>,
            roll_mode: RollMode,
            ruleset: RulesetId,
            algorithm_id: RollAlgorithm,
            seed_reference: String,
            cursor_before: u64,
            cursor_after: u64,
        }

        let wire = WireRecord::deserialize(deserializer)?;
        let record = Self {
            roll_id: wire.roll_id,
            expression: wire.expression,
            rolled_dice: wire.rolled_dice,
            kept_dice: wire.kept_dice,
            modifier_components: wire.modifier_components,
            total: wire.total,
            purpose: wire.purpose,
            actor_id: wire.actor_id,
            target_id: wire.target_id,
            roll_mode: wire.roll_mode,
            ruleset: wire.ruleset,
            algorithm_id: wire.algorithm_id,
            seed_reference: wire.seed_reference,
            cursor_before: wire.cursor_before,
            cursor_after: wire.cursor_after,
        };
        record.validate().map_err(D::Error::custom)?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RULESET;

    fn expression(value: &str) -> DiceExpression {
        value.parse().unwrap()
    }

    fn metadata() -> RollMetadata {
        RollMetadata {
            roll_id: "roll:encounter-1:turn-2:attack".into(),
            purpose: "attack:soot-wight".into(),
            actor_id: "character:canal-warden".into(),
            target_id: Some("creature:soot-wight".into()),
            ruleset: RULESET,
            seed_reference: "seed:encounter-1".into(),
        }
    }

    #[test]
    fn parses_and_displays_canonical_expressions() {
        for (source, expected, parts) in [
            ("1d20", "1d20", (1, 20, 0)),
            ("2d6+3", "2d6+3", (2, 6, 3)),
            ("4d8-12", "4d8-12", (4, 8, -12)),
            ("01d006+0003", "1d6+3", (1, 6, 3)),
            ("1d1-0", "1d1", (1, 1, 0)),
        ] {
            let parsed = expression(source);
            assert_eq!(parsed.to_string(), expected);
            assert_eq!((parsed.count(), parsed.sides(), parsed.constant()), parts);
        }
    }

    #[test]
    fn rejects_non_ascii_whitespace_and_malformed_expressions() {
        for invalid in [
            "", "d20", "1d", "1D20", "1 d20", "1d20 ", "1d20+", "1d20--1", "1d20+-1", "+1d20",
            "1dd20", "1d２０", "🎲1d20",
        ] {
            assert!(
                invalid.parse::<DiceExpression>().is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn enforces_each_expression_bound_and_numeric_overflow() {
        for invalid in [
            "0d6",
            "101d6",
            "1d0",
            "1d10001",
            "1d6+100001",
            "1d6-100001",
            "100d10000+1",
            "18446744073709551616d6",
            "1d18446744073709551616",
            "1d6+18446744073709551616",
        ] {
            assert!(
                invalid.parse::<DiceExpression>().is_err(),
                "accepted {invalid}"
            );
        }

        assert!(expression("100d10000").to_string() == "100d10000");
        assert!(expression("1d6+100000").to_string() == "1d6+100000");
    }

    #[test]
    fn expression_roundtrips_across_boundary_combinations() {
        let sides = [1, 2, 6, 20, 100, MAX_DIE_SIDES];
        let constants = [
            -(MAX_DICE_CONSTANT_ABS as i32),
            -1,
            0,
            1,
            MAX_DICE_CONSTANT_ABS as i32,
        ];
        for count in 1..=MAX_DICE_COUNT {
            for side in sides {
                for constant in constants {
                    if let Ok(value) = DiceExpression::new(count, side, constant) {
                        assert_eq!(value.to_string().parse::<DiceExpression>(), Ok(value));
                    }
                }
            }
        }
    }

    #[test]
    fn expression_serde_is_a_validated_canonical_string() {
        let parsed = expression("02d006+03");
        assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""2d6+3""#);
        assert_eq!(
            serde_json::from_str::<DiceExpression>(r#""2d6+3""#).unwrap(),
            parsed
        );
        assert!(serde_json::from_str::<DiceExpression>(r#""0d6""#).is_err());
        assert!(serde_json::from_str::<DiceExpression>("null").is_err());
    }

    #[test]
    fn chacha20_v1_matches_the_pinned_word_vector() {
        let mut rng = DeterministicRng::new([0; 32]);
        let words = [
            rng.next_word().unwrap(),
            rng.next_word().unwrap(),
            rng.next_word().unwrap(),
            rng.next_word().unwrap(),
        ];

        assert_eq!(words, [0xade0_b876, 0x903d_f1a0, 0xe56a_5d40, 0x28bd_8653]);
        assert_eq!(rng.cursor(), 4);
        assert_eq!(rng.algorithm().to_string(), CHACHA20_V1_ALGORITHM_ID);
    }

    #[test]
    fn seeded_rolls_are_replayable_from_any_recorded_cursor() {
        let seed = [42; 32];
        let mut uninterrupted = DeterministicRng::new(seed);
        let first = uninterrupted
            .roll(expression("4d6+2"), RollContext::normal())
            .unwrap();
        let second = uninterrupted
            .roll(expression("1d20-1"), RollContext::with_advantage())
            .unwrap();

        let mut replay_first = DeterministicRng::at_cursor(seed, first.cursor_before);
        assert_eq!(
            replay_first
                .roll(expression("4d6+2"), RollContext::normal())
                .unwrap(),
            first
        );
        let mut replay_second = DeterministicRng::at_cursor(seed, first.cursor_after);
        assert_eq!(
            replay_second
                .roll(expression("1d20-1"), RollContext::with_advantage())
                .unwrap(),
            second
        );
    }

    #[test]
    fn d20_modes_keep_the_correct_die_and_opposed_sources_cancel() {
        let seed = [7; 32];
        let mut advantage_rng = DeterministicRng::new(seed);
        let advantage = advantage_rng
            .roll(expression("1d20+3"), RollContext::with_advantage())
            .unwrap();
        assert_eq!(advantage.roll_mode, RollMode::Advantage);
        assert_eq!(advantage.rolled_dice.len(), 2);
        assert_eq!(
            advantage.kept_dice,
            vec![*advantage.rolled_dice.iter().max().unwrap()]
        );
        assert_eq!(advantage.total, advantage.kept_dice[0] as i32 + 3);

        let mut disadvantage_rng = DeterministicRng::new(seed);
        let disadvantage = disadvantage_rng
            .roll(expression("1d20+3"), RollContext::with_disadvantage())
            .unwrap();
        assert_eq!(
            disadvantage.kept_dice,
            vec![*disadvantage.rolled_dice.iter().min().unwrap()]
        );

        let mut cancelled_rng = DeterministicRng::new(seed);
        let cancelled = cancelled_rng
            .roll(
                expression("1d20+3"),
                RollContext {
                    advantage_sources: u8::MAX,
                    disadvantage_sources: 1,
                },
            )
            .unwrap();
        assert_eq!(cancelled.roll_mode, RollMode::Normal);
        assert_eq!(cancelled.rolled_dice.len(), 1);
        assert_eq!(cancelled.cursor_after - cancelled.cursor_before, 1);
    }

    #[test]
    fn non_d20_modes_fail_without_spending_the_cursor() {
        let mut rng = DeterministicRng::new([9; 32]);
        let before = rng.cursor();
        assert_eq!(
            rng.roll(expression("2d6"), RollContext::with_advantage()),
            Err(RollError::UnsupportedRollMode)
        );
        assert_eq!(rng.cursor(), before);
    }

    #[test]
    fn rolls_stay_in_bounds_and_replay_across_many_seeds_and_cursors() {
        for case in 0_u8..=127 {
            let mut seed = [0_u8; 32];
            for (index, byte) in seed.iter_mut().enumerate() {
                *byte = case.wrapping_mul(31).wrapping_add(index as u8);
            }
            let cursor = u64::from(case) * 17;
            for sides in [1, 2, 3, 6, 20, 100, MAX_DIE_SIDES] {
                let mut first = DeterministicRng::at_cursor(seed, cursor);
                let mut replay = DeterministicRng::at_cursor(seed, cursor);
                let first_value = first.roll_die(sides).unwrap();
                let replay_value = replay.roll_die(sides).unwrap();
                assert!((1..=sides).contains(&first_value));
                assert_eq!(first_value, replay_value);
                assert_eq!(first.cursor(), replay.cursor());
            }
        }
    }

    #[test]
    fn cursor_exhaustion_is_atomic() {
        let mut rng = DeterministicRng::at_cursor([0; 32], u64::MAX);
        assert_eq!(rng.roll_die(20), Err(RollError::CursorExhausted));
        assert_eq!(rng.cursor(), u64::MAX);
    }

    #[test]
    fn roll_record_is_canonical_validated_and_contains_no_seed() {
        let mut rng = DeterministicRng::new([11; 32]);
        let record = rng
            .roll_record(
                expression("1d20+5"),
                RollContext::with_advantage(),
                metadata(),
                vec![
                    ModifierComponent {
                        name: "proficiency".into(),
                        value: 2,
                    },
                    ModifierComponent {
                        name: "ability:strength".into(),
                        value: 3,
                    },
                ],
            )
            .unwrap();

        assert_eq!(record.modifier_components[0].name, "ability:strength");
        record.validate().unwrap();
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("[11,11"));
        assert!(json.contains(r#""algorithm_id":"chacha20-v1""#));
        assert_eq!(serde_json::from_str::<RollRecord>(&json).unwrap(), record);
    }

    #[test]
    fn roll_record_json_has_a_stable_golden_shape() {
        let record = RollRecord::from_roll(
            DiceRoll {
                expression: expression("1d20+5"),
                rolled_dice: vec![4, 17],
                kept_dice: vec![17],
                total: 22,
                roll_mode: RollMode::Advantage,
                cursor_before: 8,
                cursor_after: 10,
            },
            metadata(),
            vec![
                ModifierComponent {
                    name: "proficiency".into(),
                    value: 2,
                },
                ModifierComponent {
                    name: "ability:strength".into(),
                    value: 3,
                },
            ],
        )
        .unwrap();

        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"roll_id":"roll:encounter-1:turn-2:attack","expression":"1d20+5","rolled_dice":[4,17],"kept_dice":[17],"modifier_components":[{"name":"ability:strength","value":3},{"name":"proficiency","value":2}],"total":22,"purpose":"attack:soot-wight","actor_id":"character:canal-warden","target_id":"creature:soot-wight","roll_mode":"advantage","ruleset":"srd-5.1-cc","algorithm_id":"chacha20-v1","seed_reference":"seed:encounter-1","cursor_before":8,"cursor_after":10}"#
        );
    }

    #[test]
    fn record_validation_rejects_tampering_and_unknown_fields() {
        let valid = r#"{"roll_id":"roll:1","expression":"1d20+3","rolled_dice":[4,17],"kept_dice":[17],"modifier_components":[{"name":"ability","value":3}],"total":20,"purpose":"attack","actor_id":"hero:1","target_id":"creature:1","roll_mode":"advantage","ruleset":"srd-5.1-cc","algorithm_id":"chacha20-v1","seed_reference":"seed:1","cursor_before":4,"cursor_after":6}"#;
        assert!(serde_json::from_str::<RollRecord>(valid).is_ok());

        for invalid in [
            valid.replace(r#""total":20"#, r#""total":19"#),
            valid.replace(r#""kept_dice":[17]"#, r#""kept_dice":[4]"#),
            valid.replace(r#""cursor_after":6"#, r#""cursor_after":5"#),
            valid.replace(r#""actor_id":"hero:1""#, r#""actor_id":"bad id""#),
            valid.replace(
                r#""modifier_components":[{"name":"ability","value":3}]"#,
                r#""modifier_components":[]"#,
            ),
            valid.replace(r#""cursor_after":6"#, r#""cursor_after":6,"extra":true"#),
        ] {
            assert!(
                serde_json::from_str::<RollRecord>(&invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn invalid_record_metadata_does_not_spend_randomness() {
        let mut invalid_metadata = metadata();
        invalid_metadata.actor_id = "contains spaces".into();
        let mut rng = DeterministicRng::new([1; 32]);
        let before = rng.cursor();
        assert!(
            rng.roll_record(
                expression("1d20"),
                RollContext::normal(),
                invalid_metadata,
                vec![],
            )
            .is_err()
        );
        assert_eq!(rng.cursor(), before);
    }
}
