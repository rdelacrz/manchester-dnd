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
