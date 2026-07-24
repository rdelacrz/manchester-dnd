use std::{fmt, path::PathBuf, time::Duration};

use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to load environment file {path}: {reason}")]
    Dotenv {
        path: PathBuf,
        /// Deliberately sanitized: dotenv parse errors can contain the raw
        /// malformed line, which may itself contain a credential.
        reason: &'static str,
    },
    #[error("{name} must be set when {profile} uses the openai-compatible backend")]
    MissingProfileValue {
        profile: &'static str,
        name: &'static str,
    },
    #[error("invalid value for {name}: {reason}")]
    InvalidValue { name: &'static str, reason: String },
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("{capability} generation is disabled")]
    Disabled { capability: &'static str },
    #[error("invalid generation provider configuration: {0}")]
    InvalidConfiguration(String),
    #[error("generation request timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("generation transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("generation provider returned HTTP {status}{request_suffix}", request_suffix = request_id.as_ref().map(|id| format!(" (request id {id})")).unwrap_or_default())]
    HttpStatus {
        status: StatusCode,
        request_id: Option<String>,
    },
    #[error("generation provider returned an invalid {endpoint} response: {reason}")]
    InvalidResponse {
        endpoint: &'static str,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MongoFailureKind {
    DuplicateKey,
    DocumentValidation,
    Timeout,
    Authentication,
    Topology,
    TransactionUnsupported,
    TransientTransaction,
    UnknownCommitResult,
    Database,
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("MongoDB database name failed the configured allowlist")]
    InvalidDatabaseName,
    #[error("MongoDB connection URL could not be parsed")]
    InvalidMongoUri,
    #[error("MongoDB {operation} failed ({kind:?})")]
    Mongo {
        operation: &'static str,
        kind: MongoFailureKind,
        #[source]
        source: mongodb::error::Error,
    },
    #[error("MongoDB schema drift in {collection}: {detail}")]
    SchemaDrift { collection: String, detail: String },
    #[error("MongoDB schema bundle metadata is missing or incompatible")]
    SchemaBundleMismatch,
    #[error("MongoDB operation {operation} exceeded its configured deadline")]
    OperationTimeout { operation: &'static str },
    #[error("MongoDB transaction exceeded its configured deadline")]
    TransactionDeadline,
    #[error("MongoDB transaction retry budget exhausted after {attempts} attempt(s)")]
    TransactionRetriesExhausted {
        attempts: u32,
        #[source]
        last: Box<PersistenceError>,
    },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("{entity} {id} already exists")]
    AlreadyExists { entity: &'static str, id: String },
    #[error("{entity} {id} revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict {
        entity: &'static str,
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("idempotency key {idempotency_key} conflicts in {scope_kind} {scope_id}")]
    IdempotencyConflict {
        scope_kind: String,
        scope_id: String,
        idempotency_key: String,
    },
    #[error("could not encode the MongoDB schema bundle")]
    BsonEncoding(#[source] mongodb::bson::ser::Error),
}

impl PersistenceError {
    pub fn mongo(operation: &'static str, source: mongodb::error::Error) -> Self {
        let kind = classify_mongo_error(&source);
        Self::Mongo {
            operation,
            kind,
            source,
        }
    }

    pub const fn mongo_failure_kind(&self) -> Option<MongoFailureKind> {
        match self {
            Self::Mongo { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

pub fn classify_mongo_error(error: &mongodb::error::Error) -> MongoFailureKind {
    use mongodb::error::{
        ErrorKind, TRANSIENT_TRANSACTION_ERROR, UNKNOWN_TRANSACTION_COMMIT_RESULT, WriteFailure,
    };

    if error.contains_label(UNKNOWN_TRANSACTION_COMMIT_RESULT) {
        return MongoFailureKind::UnknownCommitResult;
    }
    if error.contains_label(TRANSIENT_TRANSACTION_ERROR) {
        return MongoFailureKind::TransientTransaction;
    }

    let code = match error.kind.as_ref() {
        ErrorKind::Command(error) => Some(error.code),
        ErrorKind::Write(WriteFailure::WriteError(error)) => Some(error.code),
        ErrorKind::Write(WriteFailure::WriteConcernError(error)) => Some(error.code),
        ErrorKind::InsertMany(error) => error
            .write_errors
            .as_ref()
            .and_then(|errors| errors.first())
            .map(|error| error.code)
            .or_else(|| error.write_concern_error.as_ref().map(|error| error.code)),
        _ => None,
    };
    match code {
        Some(11000 | 11001 | 12582) => MongoFailureKind::DuplicateKey,
        Some(121) => MongoFailureKind::DocumentValidation,
        Some(18) => MongoFailureKind::Authentication,
        Some(20 | 263 | 303) => MongoFailureKind::TransactionUnsupported,
        Some(50 | 262) => MongoFailureKind::Timeout,
        _ => match error.kind.as_ref() {
            ErrorKind::Authentication { .. } => MongoFailureKind::Authentication,
            ErrorKind::ServerSelection { .. }
            | ErrorKind::DnsResolve { .. }
            | ErrorKind::ConnectionPoolCleared { .. } => MongoFailureKind::Topology,
            ErrorKind::Io(source) if source.kind() == std::io::ErrorKind::TimedOut => {
                MongoFailureKind::Timeout
            }
            ErrorKind::Transaction { message, .. }
                if message.contains("not supported") || message.contains("not support") =>
            {
                MongoFailureKind::TransactionUnsupported
            }
            _ => MongoFailureKind::Database,
        },
    }
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("DragonflyDB cache key material is invalid")]
    InvalidKey,
    #[error("DragonflyDB pool configuration is invalid")]
    InvalidPoolConfiguration,
    #[error("DragonflyDB connection pool is unavailable")]
    Pool(#[source] deadpool_redis::PoolError),
    #[error("DragonflyDB command failed")]
    Command(#[source] redis::RedisError),
    #[error("DragonflyDB command timed out")]
    Timeout,
    #[error("DragonflyDB cache value serialization failed")]
    Serialization(#[source] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("MongoDB persistence operation failed")]
    Persistence(#[from] PersistenceError),
    #[error("could not serialize {entity}")]
    Serialize {
        entity: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not encode MongoDB BSON document")]
    BsonEncoding(#[from] mongodb::bson::ser::Error),
    #[error("could not decode MongoDB BSON document")]
    BsonDecoding(#[from] mongodb::bson::de::Error),
    #[error("stored {entity} {id} contains invalid JSON")]
    InvalidStoredData {
        entity: &'static str,
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("{entity} {id} already exists")]
    AlreadyExists { entity: &'static str, id: String },
    #[error("{entity} {id} revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict {
        entity: &'static str,
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("idempotency key {idempotency_key} conflicts in {scope_kind} {scope_id}")]
    IdempotencyConflict {
        scope_kind: String,
        scope_id: String,
        idempotency_key: String,
    },
    #[error("numeric value for {field} is outside the persistence format's supported range")]
    NumericRange { field: &'static str },
    #[error("unsupported {entity} schema version {found}; this server supports {supported}")]
    UnsupportedSchemaVersion {
        entity: &'static str,
        found: u32,
        supported: u32,
    },
    #[error("stored {entity} identity mismatch: row {row_id}, payload {payload_id}")]
    IdentityMismatch {
        entity: &'static str,
        row_id: String,
        payload_id: String,
    },
    #[error("{entity} {id} failed domain validation")]
    CoreValidation {
        entity: &'static str,
        id: String,
        #[source]
        source: manchester_dnd_core::GameCoreError,
    },
    #[error("{entity} {id} failed hero-domain validation")]
    HeroValidation {
        entity: &'static str,
        id: String,
        #[source]
        source: manchester_dnd_core::hero::HeroError,
    },
    #[error("{entity} {id} failed validation: {reason}")]
    InvalidDomainState {
        entity: &'static str,
        id: String,
        reason: &'static str,
    },
}

/// Errors returned by `AuthService` and the HTTP authentication boundary.
///
/// Variants are deliberately coarse to prevent account enumeration: unknown
/// account, disabled account, and wrong password all map to
/// `InvalidCredentials`.
#[derive(Debug, Error)]
pub enum AuthenticationError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("account is unavailable")]
    AccountUnavailable,
    #[error("signup access token is invalid or expired")]
    InvalidSignupAccess,
    #[error("signup session is invalid or expired")]
    InvalidSignupSession,
    #[error("authentication attempt is throttled")]
    Throttled,
    #[error("session is invalid or expired")]
    InvalidSession,
    #[error("password hash operation failed")]
    PasswordHash,
    #[error("cryptographic randomness source failed")]
    Randomness,
    #[error("authentication input failed validation")]
    InvalidInput(#[from] crate::auth::AuthenticationInputError),
    #[error("repository error during authentication")]
    Repository(#[from] RepositoryError),
    #[error("MongoDB persistence failed during authentication")]
    Persistence(#[from] PersistenceError),
    #[error("email protection failed")]
    EmailCrypto(#[from] crate::persistence::EmailCryptoError),
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("hosted gameplay access is not enabled")]
    HostedAccessDenied,
    #[error("game command failed validation")]
    InvalidCommand(#[source] manchester_dnd_core::GameCoreError),
    #[error("game outcome failed validation")]
    InvalidOutcome(#[source] manchester_dnd_core::GameCoreError),
    #[error("exploration rules resolution failed")]
    Rules(#[source] manchester_dnd_core::GameCoreError),
    #[error("encounter is unavailable until exploration is resolved")]
    EncounterUnavailable,
    #[error("the encounter reward is unavailable until a bound hero wins the encounter")]
    EncounterRewardUnavailable,
    #[error("the encounter reward has already been claimed")]
    EncounterRewardAlreadyClaimed,
    #[error("encounter command is not legal")]
    InvalidEncounterCommand(#[source] manchester_dnd_core::encounter::EncounterError),
    #[error("the current encounter actor is not controlled by the player")]
    NotPlayerTurn,
    #[error("the deterministic NPC policy cannot advance the current encounter actor")]
    NpcTurnUnavailable,
    #[error("encounter rules resolution failed")]
    EncounterRules(#[source] manchester_dnd_core::encounter::EncounterError),
    #[error("hero command or state failed validation")]
    Hero(#[source] manchester_dnd_core::hero::HeroError),
    #[error("hero mechanic is outside the supported MVP matrix")]
    UnsupportedHeroMechanic(manchester_dnd_core::hero::UnsupportedMechanic),
    #[error("the hero creation draft has expired")]
    HeroDraftExpired,
    #[error("the requested hero draft or character was not found")]
    HeroNotFound,
    #[error("deterministic roll resolution failed")]
    Roll(#[source] manchester_dnd_core::RollError),
    #[error("campaign RNG seed access failed")]
    SeedVault(#[source] crate::seed::SeedVaultError),
    #[error("the requested campaign is not the local campaign")]
    WrongCampaign,
    #[error("the requested character is not the local hero")]
    WrongCharacter,
    #[error("exploration action is not available")]
    UnknownAction(String),
    #[error("the campaign is already completed")]
    CampaignCompleted,
    #[error("campaign provenance is not sealed yet")]
    CampaignPinsUnsealed,
    #[error("campaign provenance does not match the active validated catalog")]
    CampaignPinsQuarantined,
    #[error("the campaign is archived")]
    CampaignArchived,
    #[error("the campaign is not archived")]
    CampaignNotArchived,
    #[error("the campaign play-session boundary conflicts with the requested operation")]
    CampaignPlaySessionConflict,
    #[error("the campaign lifecycle command is invalid")]
    InvalidCampaignLifecycle,
    #[error("the private campaign export is invalid or incompatible")]
    InvalidCampaignExport,
    #[error(
        "campaign lifecycle revision conflict: expected {expected}, current revision {current_revision}"
    )]
    LifecycleRevisionConflict {
        expected: u64,
        current_revision: u64,
    },
    #[error("campaign revision conflict: expected {expected}, current revision {current_revision}")]
    RevisionConflict {
        expected: u64,
        current_revision: u64,
    },
    #[error(
        "encounter revision conflict: expected {expected}, current revision {current_revision}"
    )]
    EncounterRevisionConflict {
        expected: u64,
        current_revision: u64,
    },
    #[error("hero revision conflict: expected {expected}, current revision {current_revision}")]
    HeroRevisionConflict {
        expected: u64,
        current_revision: u64,
    },
    #[error("idempotency key was already used for a different command")]
    IdempotencyConflict,
    #[error("could not serialize the public command response")]
    Serialization(#[source] serde_json::Error),
    #[error("stored command response is invalid")]
    StoredResponse(#[source] serde_json::Error),
    #[error("stored local campaign state is inconsistent")]
    InvalidStoredState,
    #[error("application persistence operation failed")]
    Repository(#[source] RepositoryError),
}

impl ApplicationError {
    /// Stable code safe to expose to an untrusted browser.
    pub const fn public_code(&self) -> &'static str {
        match self {
            Self::HostedAccessDenied => "hosted_access_denied",
            Self::InvalidCommand(_) => "invalid_command",
            Self::EncounterUnavailable => "encounter_unavailable",
            Self::EncounterRewardUnavailable => "encounter_reward_unavailable",
            Self::EncounterRewardAlreadyClaimed => "encounter_reward_already_claimed",
            Self::InvalidEncounterCommand(_) => "invalid_encounter_command",
            Self::NotPlayerTurn => "not_player_turn",
            Self::NpcTurnUnavailable => "npc_turn_unavailable",
            Self::Hero(_) => "invalid_hero_command",
            Self::UnsupportedHeroMechanic(_) => "unsupported_mechanic",
            Self::HeroDraftExpired => "hero_draft_expired",
            Self::HeroNotFound => "hero_not_found",
            Self::WrongCampaign => "campaign_not_found",
            Self::WrongCharacter => "character_not_found",
            Self::UnknownAction(_) => "unknown_action",
            Self::CampaignCompleted => "campaign_completed",
            Self::CampaignPinsUnsealed => "campaign_setup_incomplete",
            Self::CampaignPinsQuarantined => "campaign_content_quarantined",
            Self::CampaignArchived => "campaign_archived",
            Self::CampaignNotArchived => "campaign_not_archived",
            Self::CampaignPlaySessionConflict => "play_session_conflict",
            Self::InvalidCampaignLifecycle => "invalid_campaign_lifecycle",
            Self::InvalidCampaignExport => "invalid_campaign_export",
            Self::LifecycleRevisionConflict { .. } => "lifecycle_revision_conflict",
            Self::RevisionConflict { .. } => "revision_conflict",
            Self::EncounterRevisionConflict { .. } => "encounter_revision_conflict",
            Self::HeroRevisionConflict { .. } => "hero_revision_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::InvalidOutcome(_)
            | Self::Rules(_)
            | Self::EncounterRules(_)
            | Self::Roll(_)
            | Self::SeedVault(_)
            | Self::Serialization(_)
            | Self::StoredResponse(_)
            | Self::InvalidStoredState
            | Self::Repository(_) => "internal_error",
        }
    }

    /// Deliberately omits repository, JSON, and rules-engine source details.
    pub const fn safe_message(&self) -> &'static str {
        match self {
            Self::HostedAccessDenied => {
                "Hosted gameplay is unavailable until authentication is configured."
            }
            Self::InvalidCommand(_) => "The game command is invalid.",
            Self::EncounterUnavailable => {
                "Resolve the viaduct exploration before starting this encounter."
            }
            Self::EncounterRewardUnavailable => {
                "Win this encounter with the selected hero before claiming its reward."
            }
            Self::EncounterRewardAlreadyClaimed => {
                "This encounter reward has already been claimed."
            }
            Self::InvalidEncounterCommand(_) => "That encounter action is not available now.",
            Self::NotPlayerTurn => {
                "The current creature turn can only be advanced by the server policy."
            }
            Self::NpcTurnUnavailable => "The Soot Wight is not ready for a policy-controlled step.",
            Self::Hero(_) => "That hero choice is not available.",
            Self::UnsupportedHeroMechanic(_) => {
                "That mechanic is unavailable; choose one of the authored alternatives."
            }
            Self::HeroDraftExpired => "This character-creation draft has expired.",
            Self::HeroNotFound => "The selected hero or creation draft is not available.",
            Self::WrongCampaign => "The local campaign could not be found.",
            Self::WrongCharacter => "The selected character is not available.",
            Self::UnknownAction(_) => "That exploration action is not available.",
            Self::CampaignCompleted => "This campaign has already ended.",
            Self::CampaignPinsUnsealed => "Choose and save a campaign theme before starting play.",
            Self::CampaignPinsQuarantined => {
                "This campaign cannot resume because its saved content provenance is unavailable."
            }
            Self::CampaignArchived => "Restore this archived campaign before resuming play.",
            Self::CampaignNotArchived => "Archive this campaign before permanently deleting it.",
            Self::CampaignPlaySessionConflict => {
                "Finish the current play session before changing campaign lifecycle state."
            }
            Self::InvalidCampaignLifecycle => {
                "That campaign lifecycle action is not available now."
            }
            Self::InvalidCampaignExport => {
                "That private campaign export is invalid or incompatible."
            }
            Self::LifecycleRevisionConflict { .. } => {
                "The campaign list changed; reload it before trying again."
            }
            Self::RevisionConflict { .. } => {
                "The campaign changed; reload it before trying another action."
            }
            Self::EncounterRevisionConflict { .. } => {
                "The encounter changed; reload it before trying another action."
            }
            Self::HeroRevisionConflict { .. } => {
                "The hero changed; reload it before trying another choice."
            }
            Self::IdempotencyConflict => {
                "This request key was already used for a different action."
            }
            Self::InvalidOutcome(_)
            | Self::Rules(_)
            | Self::EncounterRules(_)
            | Self::Roll(_)
            | Self::SeedVault(_)
            | Self::Serialization(_)
            | Self::StoredResponse(_)
            | Self::InvalidStoredState
            | Self::Repository(_) => "The game service is temporarily unavailable.",
        }
    }

    pub const fn retryable(&self) -> bool {
        // Revision conflicts have a defined recovery path: reload the current
        // view. Internal validation/corruption failures are deterministic, and
        // repository errors are not retryable until the MongoDB driver has
        // explicitly classified them as transient.
        matches!(
            self,
            Self::LifecycleRevisionConflict { .. }
                | Self::RevisionConflict { .. }
                | Self::EncounterRevisionConflict { .. }
                | Self::HeroRevisionConflict { .. }
        )
    }

    pub const fn current_revision(&self) -> Option<u64> {
        match self {
            Self::LifecycleRevisionConflict {
                current_revision, ..
            }
            | Self::RevisionConflict {
                current_revision, ..
            } => Some(*current_revision),
            Self::HeroRevisionConflict {
                current_revision, ..
            } => Some(*current_revision),
            _ => None,
        }
    }

    pub const fn current_encounter_revision(&self) -> Option<u64> {
        match self {
            Self::EncounterRevisionConflict {
                current_revision, ..
            } => Some(*current_revision),
            _ => None,
        }
    }

    /// Structured, bounded alternatives safe for a browser to render.
    pub const fn unsupported_hero_mechanic(
        &self,
    ) -> Option<&manchester_dnd_core::hero::UnsupportedMechanic> {
        match self {
            Self::UnsupportedHeroMechanic(unsupported) => Some(unsupported),
            _ => None,
        }
    }
}

#[cfg(test)]
mod application_error_tests {
    use super::*;

    #[test]
    fn public_mapping_never_exposes_persistence_details() {
        let error = ApplicationError::Repository(RepositoryError::NotFound {
            entity: "campaign session",
            id: "private-campaign-id".to_owned(),
        });

        assert_eq!(error.public_code(), "internal_error");
        assert_eq!(
            error.safe_message(),
            "The game service is temporarily unavailable."
        );
        assert!(!error.safe_message().contains("private-campaign-id"));
        assert!(!error.retryable());
        assert_eq!(error.current_revision(), None);
    }

    #[test]
    fn revision_conflict_exposes_only_the_current_revision() {
        let error = ApplicationError::RevisionConflict {
            expected: 3,
            current_revision: 4,
        };

        assert_eq!(error.public_code(), "revision_conflict");
        assert_eq!(error.current_revision(), Some(4));
        assert_eq!(error.current_encounter_revision(), None);
        assert!(error.retryable());
    }

    #[test]
    fn encounter_revision_conflict_is_distinct_and_safe() {
        let error = ApplicationError::EncounterRevisionConflict {
            expected: 8,
            current_revision: 9,
        };

        assert_eq!(error.public_code(), "encounter_revision_conflict");
        assert_eq!(error.current_revision(), None);
        assert_eq!(error.current_encounter_revision(), Some(9));
        assert_eq!(
            error.safe_message(),
            "The encounter changed; reload it before trying another action."
        );
        assert!(error.retryable());
    }

    #[test]
    fn turn_controller_errors_are_distinct_and_non_retryable() {
        let player = ApplicationError::NotPlayerTurn;
        assert_eq!(player.public_code(), "not_player_turn");
        assert!(player.safe_message().contains("server policy"));
        assert!(!player.retryable());

        let npc = ApplicationError::NpcTurnUnavailable;
        assert_eq!(npc.public_code(), "npc_turn_unavailable");
        assert!(!npc.retryable());
    }

    #[test]
    fn seed_and_roll_failures_never_expose_internal_details() {
        let error = ApplicationError::Roll(manchester_dnd_core::RollError::CursorExhausted);
        assert_eq!(error.public_code(), "internal_error");
        assert_eq!(
            error.safe_message(),
            "The game service is temporarily unavailable."
        );
        assert!(!error.to_string().contains("seed"));
    }
}

#[derive(Error)]
pub enum EventPromptError {
    #[error("could not read the configured private event source tree")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the private event source root must be a real directory")]
    RootNotDirectory { path: PathBuf },
    #[error("symbolic links are not allowed in the private event source tree")]
    SymlinkNotAllowed { path: PathBuf },
    #[error("the private event source tree contains an unsupported filesystem entry")]
    UnsupportedEntry { path: PathBuf },
    #[error("a private event source resolves outside its configured root")]
    PathOutsideRoot { path: PathBuf, root: PathBuf },
    #[error("private event source tree exceeds its {maximum}-entry limit")]
    TooManyEntries { maximum: usize },
    #[error("private event source tree exceeds its {maximum}-directory depth")]
    DirectoryTooDeep { path: PathBuf, maximum: usize },
    #[error("a private event source must be strict UTF-8")]
    InvalidUtf8 { path: PathBuf },
    #[error("a private event source exceeds the {maximum_bytes}-byte limit")]
    TooLarge { path: PathBuf, maximum_bytes: u64 },
    #[error("a private event source is missing required JSON frontmatter delimiters")]
    MissingFrontmatter { path: PathBuf },
    #[error("a private event source contains invalid JSON frontmatter")]
    InvalidFrontmatter {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("a private event source has invalid metadata: {reason}")]
    InvalidMetadata { path: PathBuf, reason: String },
    #[error("duplicate private event source id {id}")]
    DuplicateId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error(
        "a private event source was quarantined by deterministic pre-screening ({finding_codes})"
    )]
    QuarantinedCandidate { finding_codes: String },
    #[error("{count} private event source candidate(s) were quarantined")]
    QuarantinedCandidates { count: usize },
    #[error("an in-memory private event source has invalid metadata: {reason}")]
    InvalidRuntimeMetadata { reason: String },
    #[error("event prompt collection contains {found} files; maximum is {maximum}")]
    TooManyPrompts { found: usize, maximum: usize },
    #[error("eligible event prompt weights did not produce a finite positive total")]
    InvalidTotalWeight,
    #[error(
        "random source returned invalid rational sample {numerator}/{denominator}; expected 0 <= numerator < denominator"
    )]
    InvalidRandomSample { numerator: u64, denominator: u64 },
    #[error("deterministic private-event random selection failed")]
    DeterministicRandom(#[from] manchester_dnd_core::RollError),
}

impl fmt::Debug for EventPromptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EventPromptError")
            .field(&self.to_string())
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum PrivateInspirationError {
    #[error("private inspiration command failed closed validation ({code})")]
    InvalidCommand { code: &'static str },
    #[error("private inspiration is disabled for this deployment")]
    DeploymentDisabled,
    #[error("private inspiration persistence has not been migrated to MongoDB")]
    NotImplemented,
    #[error("private inspiration authorization or scope validation failed")]
    ScopeDenied,
    #[error("private inspiration state was not found")]
    NotFound,
    #[error("private inspiration revision conflict: expected {expected}, current {current}")]
    RevisionConflict { expected: u64, current: u64 },
    #[error("private inspiration idempotency key was reused for different intent")]
    IdempotencyConflict,
    #[error("private inspiration selection failed")]
    Selection(#[source] EventPromptError),
    #[error("private inspiration response serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("private inspiration persistence failed")]
    Repository(#[from] RepositoryError),
}

impl PrivateInspirationError {
    pub const fn public_code(&self) -> &'static str {
        match self {
            Self::InvalidCommand { .. } => "invalid_inspiration_command",
            Self::DeploymentDisabled => "inspiration_deployment_disabled",
            Self::NotImplemented => "internal_error",
            Self::ScopeDenied => "inspiration_scope_denied",
            Self::NotFound => "inspiration_not_configured",
            Self::RevisionConflict { .. } => "inspiration_revision_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Selection(_) | Self::Serialization(_) | Self::Repository(_) => "internal_error",
        }
    }

    pub const fn safe_message(&self) -> &'static str {
        match self {
            Self::InvalidCommand { .. } => "That inspiration control is invalid.",
            Self::DeploymentDisabled => "Private inspiration is disabled for this installation.",
            Self::NotImplemented => "The private inspiration service is temporarily unavailable.",
            Self::ScopeDenied => "That private inspiration action is not available.",
            Self::NotFound => "Private inspiration has not been configured for this campaign.",
            Self::RevisionConflict { .. } => {
                "The inspiration settings changed; reload before trying again."
            }
            Self::IdempotencyConflict => {
                "That request key is already bound to a different inspiration action."
            }
            Self::Selection(_) | Self::Serialization(_) | Self::Repository(_) => {
                "The private inspiration service is temporarily unavailable."
            }
        }
    }

    pub const fn current_revision(&self) -> Option<u64> {
        match self {
            Self::RevisionConflict { current, .. } => Some(*current),
            _ => None,
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(self, Self::RevisionConflict { .. })
    }
}

#[derive(Debug, Error)]
pub enum GameMasterError {
    #[error(transparent)]
    Generation(#[from] GenerationError),
    #[error("could not serialize the structured game-master request")]
    RequestSerialization(#[source] serde_json::Error),
    #[error("could not fingerprint the validated game-master proposal")]
    ProposalSerialization(#[source] serde_json::Error),
    #[error("game-master response was not valid structured JSON")]
    InvalidJson(#[source] serde_json::Error),
    #[error("game-master draft failed validation: {0}")]
    InvalidDraft(String),
}

#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Content(#[from] crate::content::ContentCatalogError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Generation(#[from] GenerationError),
    #[error(transparent)]
    EventPrompt(#[from] EventPromptError),
    #[error(transparent)]
    SeedVault(#[from] crate::seed::SeedVaultError),
    #[error(transparent)]
    SceneImage(#[from] crate::scene_images::SceneImageError),
}
