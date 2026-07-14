use std::{path::PathBuf, time::Duration};

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

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("invalid SQLite database URL")]
    InvalidDatabaseUrl(#[source] sqlx::Error),
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
    #[error("database migration failed")]
    Migration(#[source] sqlx::migrate::MigrateError),
    #[error("could not serialize {entity}")]
    Serialize {
        entity: &'static str,
        #[source]
        source: serde_json::Error,
    },
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
    #[error("numeric value for {field} is outside SQLite's supported range")]
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
    #[error("{entity} {id} failed validation: {reason}")]
    InvalidDomainState {
        entity: &'static str,
        id: String,
        reason: &'static str,
    },
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("hosted gameplay access is not enabled")]
    HostedAccessDenied,
    #[error("exploration command failed validation")]
    InvalidCommand(#[source] manchester_dnd_core::GameCoreError),
    #[error("exploration outcome failed validation")]
    InvalidOutcome(#[source] manchester_dnd_core::GameCoreError),
    #[error("rules resolution failed")]
    Rules(#[source] manchester_dnd_core::GameCoreError),
    #[error("the requested campaign is not the local campaign")]
    WrongCampaign,
    #[error("the requested character is not the local hero")]
    WrongCharacter,
    #[error("exploration action is not available")]
    UnknownAction(String),
    #[error("the campaign is already completed")]
    CampaignCompleted,
    #[error("campaign revision conflict: expected {expected}, current revision {current_revision}")]
    RevisionConflict {
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
            Self::WrongCampaign => "campaign_not_found",
            Self::WrongCharacter => "character_not_found",
            Self::UnknownAction(_) => "unknown_action",
            Self::CampaignCompleted => "campaign_completed",
            Self::RevisionConflict { .. } => "revision_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::InvalidOutcome(_)
            | Self::Rules(_)
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
            Self::InvalidCommand(_) => "The exploration command is invalid.",
            Self::WrongCampaign => "The local campaign could not be found.",
            Self::WrongCharacter => "The selected character is not available.",
            Self::UnknownAction(_) => "That exploration action is not available.",
            Self::CampaignCompleted => "This campaign has already ended.",
            Self::RevisionConflict { .. } => {
                "The campaign changed; reload it before trying another action."
            }
            Self::IdempotencyConflict => {
                "This request key was already used for a different action."
            }
            Self::InvalidOutcome(_)
            | Self::Rules(_)
            | Self::Serialization(_)
            | Self::StoredResponse(_)
            | Self::InvalidStoredState
            | Self::Repository(_) => "The game service is temporarily unavailable.",
        }
    }

    pub const fn retryable(&self) -> bool {
        // Revision conflicts have a defined recovery path: reload the current
        // view. Internal validation/corruption failures are deterministic, and
        // repository errors are not retryable until their SQLite code has been
        // explicitly classified as transient.
        matches!(self, Self::RevisionConflict { .. })
    }

    pub const fn current_revision(&self) -> Option<u64> {
        match self {
            Self::RevisionConflict {
                current_revision, ..
            } => Some(*current_revision),
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
        assert!(error.retryable());
    }
}

#[derive(Debug, Error)]
pub enum EventPromptError {
    #[error("could not read event prompt path {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("event prompt {path} exceeds the {maximum_bytes}-byte limit")]
    TooLarge { path: PathBuf, maximum_bytes: u64 },
    #[error("event prompt {path} must begin with JSON frontmatter between `---` delimiters")]
    MissingFrontmatter { path: PathBuf },
    #[error("event prompt {path} contains invalid JSON frontmatter")]
    InvalidFrontmatter {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("event prompt {path} has invalid metadata: {reason}")]
    InvalidMetadata { path: PathBuf, reason: String },
    #[error("duplicate event prompt id {id} in {first} and {second}")]
    DuplicateId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("event prompt collection contains {found} files; maximum is {maximum}")]
    TooManyPrompts { found: usize, maximum: usize },
    #[error("eligible event prompt weights did not produce a finite positive total")]
    InvalidTotalWeight,
    #[error("random source returned {sample}; expected a finite value in [0, 1)")]
    InvalidRandomSample { sample: f64 },
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
    Repository(#[from] RepositoryError),
    #[error(transparent)]
    Generation(#[from] GenerationError),
    #[error(transparent)]
    EventPrompt(#[from] EventPromptError),
}
