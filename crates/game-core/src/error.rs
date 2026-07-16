use thiserror::Error;

pub type Result<T> = std::result::Result<T, GameCoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GameCoreError {
    #[error("ability score must be between 1 and 30, got {score}")]
    InvalidAbilityScore { score: u8 },

    #[error("level must be between 1 and 20, got {level}")]
    InvalidLevel { level: u8 },

    #[error("dice source returned {value} for a d{sides}")]
    InvalidDieRoll { sides: u16, value: u16 },

    #[error("invalid d20 result: {reason}")]
    InvalidD20Roll { reason: &'static str },

    #[error("character field `{field}` cannot be blank")]
    EmptyCharacterField { field: &'static str },

    #[error("field `{field}` is not a valid opaque identifier")]
    InvalidIdentifier { field: &'static str },

    #[error("maximum hit points must be at least 1")]
    InvalidMaximumHitPoints,

    #[error("current hit points ({current}) cannot exceed maximum hit points ({maximum})")]
    CurrentHitPointsExceedMaximum { current: u32, maximum: u32 },

    #[error(
        "level {level} is inconsistent with {experience_points} XP; expected level {expected_level}"
    )]
    LevelExperienceMismatch {
        level: u8,
        experience_points: u32,
        expected_level: u8,
    },

    #[error("experience point total overflowed")]
    ExperienceOverflow,

    #[error("invalid experience award summary: {reason}")]
    InvalidExperienceAwardSummary { reason: &'static str },

    #[error("invalid session event: {reason}")]
    InvalidSessionEvent { reason: &'static str },

    #[error("invalid campaign session: {reason}")]
    InvalidSession { reason: &'static str },

    #[error("invalid campaign provenance pins: {reason}")]
    InvalidCampaignPins { reason: &'static str },

    #[error("invalid exploration-check command: {reason}")]
    InvalidExplorationCheckCommand { reason: &'static str },

    #[error("invalid exploration-check outcome: {reason}")]
    InvalidExplorationCheckOutcome { reason: &'static str },

    #[error("invalid local campaign view: {reason}")]
    InvalidLocalCampaignView { reason: &'static str },

    #[error("invalid ability-check result: {reason}")]
    InvalidAbilityCheckResult { reason: &'static str },

    #[error("field `{field}` exceeds its {maximum}-character limit")]
    TextFieldTooLong { field: &'static str, maximum: usize },

    #[error("SHA-256 digests must use `sha256:` followed by 64 lowercase hexadecimal digits")]
    InvalidSha256Digest,

    #[error("turn resource `{resource}` has already been spent")]
    TurnResourceUnavailable { resource: &'static str },

    #[error("cannot spend {requested} feet of movement; {remaining} feet remain")]
    InsufficientMovement { requested: u16, remaining: u16 },
}
