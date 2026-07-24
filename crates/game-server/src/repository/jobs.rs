//! Durable, metadata-only MongoDB generation queue.
//!
//! Prompt bodies, provider bodies, credentials, and raw lease tokens have no
//! storage fields. MongoDB is authoritative; optional queue wakeups happen in
//! the application only after these methods return a committed result.

use std::{future::IntoFuture, str::FromStr, time::Duration};

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use mongodb::{
    ClientSession, Collection,
    bson::{DateTime, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    error::{MongoFailureKind, PersistenceError},
    persistence::{CollectionName, MongoStore},
};

use super::{
    MongoRepository,
    governance::{
        GenerationBudgetDimension, GenerationBudgetScope, NewGenerationGovernanceReceipt,
        ensure_matching_governance_receipt, insert_generation_governance_receipt,
        load_governance_receipt_by_key, preflight_generation_governance,
        record_generation_attempt_usage, release_generation_budget,
        settle_unknown_generation_usage, validate_new_governance,
    },
};

const SCHEMA_VERSION: u32 = 1;
const MAX_ATTEMPTS: u8 = 5;
const MIN_LEASE: Duration = Duration::from_secs(1);
const MAX_LEASE: Duration = Duration::from_secs(5 * 60);
const FAILED_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const UNSELECTED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const CLAIM_SCAN_LIMIT: i64 = 64;
pub const IMAGE_REQUESTS_PER_ROLLING_DAY: u64 = 3;
pub const IMAGE_REQUESTS_PER_CAMPAIGN_LIFETIME: u64 = 10;
pub const IMAGE_REQUESTS_PER_TURN: u64 = 2;

#[derive(Debug, Error)]
pub enum GenerationJobStoreError {
    #[error("invalid generation job request: {0}")]
    InvalidInput(&'static str),
    #[error("generation job {job_id} was not found")]
    NotFound { job_id: String },
    #[error("generation job origin revision conflict: expected {expected}, actual {actual}")]
    OriginRevisionConflict { expected: u64, actual: u64 },
    #[error("generation idempotency key was already used for different metadata")]
    IdempotencyConflict,
    #[error("generation idempotency receipt is closed after operational metadata cleanup")]
    IdempotencyReceiptClosed,
    #[error("generation budget exceeded before provider invocation")]
    BudgetExceeded {
        scope: GenerationBudgetScope,
        dimension: GenerationBudgetDimension,
    },
    #[error("generation job {job_id} cannot transition from {state}")]
    InvalidTransition {
        job_id: String,
        state: GenerationJobState,
    },
    #[error("generation job lease is no longer current")]
    LostLease,
    #[error("stored generation metadata is invalid: {0}")]
    InvalidStoredData(&'static str),
    #[error("generation numeric value is outside MongoDB's signed integer range")]
    NumericRange,
    #[error("generation MongoDB operation failed")]
    Database(#[source] PersistenceError),
}

impl GenerationJobStoreError {
    /// Informational classification. Transaction callbacks are already retried
    /// only for driver-labelled transient/unknown-commit failures.
    pub fn retryable_database_transaction(&self) -> bool {
        let Self::Database(error) = self else {
            return false;
        };
        persistence_retryable(error)
    }
}

fn persistence_retryable(error: &PersistenceError) -> bool {
    match error {
        PersistenceError::Mongo { kind, .. } => matches!(
            kind,
            MongoFailureKind::TransientTransaction | MongoFailureKind::UnknownCommitResult
        ),
        PersistenceError::TransactionRetriesExhausted { last, .. } => persistence_retryable(last),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GenerationPurpose {
    IntentParsing,
    GmPlanning,
    Narration,
    Illustration,
}

impl GenerationPurpose {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::IntentParsing => "intent_parsing",
            Self::GmPlanning => "gm_planning",
            Self::Narration => "narration",
            Self::Illustration => "illustration",
        }
    }

    const fn requires_origin_turn(self) -> bool {
        matches!(self, Self::Narration | Self::Illustration)
    }

    const fn priority(self) -> i32 {
        match self {
            Self::IntentParsing => 400,
            Self::GmPlanning => 300,
            Self::Narration => 200,
            Self::Illustration => 100,
        }
    }

    pub const fn default_max_attempts(self) -> u8 {
        match self {
            Self::IntentParsing => 1,
            Self::GmPlanning => 2,
            Self::Narration => 3,
            Self::Illustration => 5,
        }
    }

    pub fn retry_delay(self, code: GenerationFailureCode, attempt_number: u8) -> Option<Duration> {
        if !code.retryable() || attempt_number >= self.default_max_attempts() {
            return None;
        }
        let base_seconds: u64 = match (self, code) {
            (Self::IntentParsing, _) => return None,
            (Self::GmPlanning, GenerationFailureCode::RateLimited) => 30,
            (Self::GmPlanning, _) => 5,
            (Self::Narration, GenerationFailureCode::RateLimited) => 20,
            (Self::Narration, GenerationFailureCode::LeaseExpired) => 1,
            (Self::Narration, _) => 3,
            (Self::Illustration, GenerationFailureCode::RateLimited) => 60,
            (Self::Illustration, GenerationFailureCode::LeaseExpired) => 5,
            (Self::Illustration, _) => 15,
        };
        let exponent = u32::from(attempt_number.saturating_sub(1)).min(5);
        Some(Duration::from_secs(
            base_seconds.saturating_mul(1_u64 << exponent).min(3_600),
        ))
    }
}

impl FromStr for GenerationPurpose {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "intent_parsing" => Ok(Self::IntentParsing),
            "gm_planning" => Ok(Self::GmPlanning),
            "narration" => Ok(Self::Narration),
            "illustration" => Ok(Self::Illustration),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation purpose",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl GenerationJobState {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for GenerationJobState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for GenerationJobState {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation job state",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationAttemptState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl GenerationAttemptState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl FromStr for GenerationAttemptState {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation attempt state",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuccessRetention {
    CampaignLifetime,
    UnselectedPresentation30Days,
}

impl SuccessRetention {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CampaignLifetime => "campaign_lifetime",
            Self::UnselectedPresentation30Days => "unselected_presentation_30d",
        }
    }
}

impl FromStr for SuccessRetention {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "campaign_lifetime" => Ok(Self::CampaignLifetime),
            "unselected_presentation_30d" => Ok(Self::UnselectedPresentation30Days),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation retention class",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRetentionClass {
    Pending,
    FailedMetadata7Days,
    UnselectedPresentation30Days,
    CampaignLifetime,
}

impl GenerationRetentionClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::FailedMetadata7Days => "failed_metadata_7d",
            Self::UnselectedPresentation30Days => "unselected_presentation_30d",
            Self::CampaignLifetime => "campaign_lifetime",
        }
    }
}

impl FromStr for GenerationRetentionClass {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "failed_metadata_7d" => Ok(Self::FailedMetadata7Days),
            "unselected_presentation_30d" => Ok(Self::UnselectedPresentation30Days),
            "campaign_lifetime" => Ok(Self::CampaignLifetime),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation retention class",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationFailureClass {
    Transient,
    Permanent,
}

impl GenerationFailureClass {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Permanent => "permanent",
        }
    }
}

impl FromStr for GenerationFailureClass {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "transient" => Ok(Self::Transient),
            "permanent" => Ok(Self::Permanent),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation failure class",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationFailureCode {
    Timeout,
    ProviderUnavailable,
    RateLimited,
    ProviderRejected,
    MalformedResponse,
    UnsafeOutput,
    Contradiction,
    InvalidArtifact,
    BudgetExceeded,
    LeaseExpired,
    Cancelled,
}

impl GenerationFailureCode {
    pub const fn class(self) -> GenerationFailureClass {
        match self {
            Self::Timeout | Self::ProviderUnavailable | Self::RateLimited | Self::LeaseExpired => {
                GenerationFailureClass::Transient
            }
            Self::ProviderRejected
            | Self::MalformedResponse
            | Self::UnsafeOutput
            | Self::Contradiction
            | Self::InvalidArtifact
            | Self::BudgetExceeded
            | Self::Cancelled => GenerationFailureClass::Permanent,
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(self.class(), GenerationFailureClass::Transient)
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::RateLimited => "rate_limited",
            Self::ProviderRejected => "provider_rejected",
            Self::MalformedResponse => "malformed_response",
            Self::UnsafeOutput => "unsafe_output",
            Self::Contradiction => "contradiction",
            Self::InvalidArtifact => "invalid_artifact",
            Self::BudgetExceeded => "budget_exceeded",
            Self::LeaseExpired => "lease_expired",
            Self::Cancelled => "cancelled",
        }
    }
}

impl FromStr for GenerationFailureCode {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "timeout" => Ok(Self::Timeout),
            "provider_unavailable" => Ok(Self::ProviderUnavailable),
            "rate_limited" => Ok(Self::RateLimited),
            "provider_rejected" => Ok(Self::ProviderRejected),
            "malformed_response" => Ok(Self::MalformedResponse),
            "unsafe_output" => Ok(Self::UnsafeOutput),
            "contradiction" => Ok(Self::Contradiction),
            "invalid_artifact" => Ok(Self::InvalidArtifact),
            "budget_exceeded" => Ok(Self::BudgetExceeded),
            "lease_expired" => Ok(Self::LeaseExpired),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation failure code",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGenerationJob {
    pub id: String,
    pub campaign_session_id: String,
    pub origin_turn_id: Option<String>,
    pub origin_campaign_revision: u64,
    pub purpose: GenerationPurpose,
    pub idempotency_key: String,
    pub input_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub config_digest: Sha256Digest,
    pub correlation_id: Option<String>,
    pub max_attempts: u8,
    pub success_retention: SuccessRetention,
    pub governance: Option<NewGenerationGovernanceReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationJob {
    pub id: String,
    pub campaign_session_id: String,
    pub origin_turn_id: Option<String>,
    pub origin_campaign_revision: u64,
    pub purpose: GenerationPurpose,
    pub idempotency_key: String,
    pub state: GenerationJobState,
    pub input_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub config_digest: Sha256Digest,
    pub output_digest: Option<Sha256Digest>,
    pub correlation_id: Option<String>,
    pub attempt_count: u8,
    pub max_attempts: u8,
    pub retry_at: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_token: Option<String>,
    pub lease_expires_at: Option<String>,
    pub last_failure_class: Option<GenerationFailureClass>,
    pub last_failure_code: Option<GenerationFailureCode>,
    pub artifact_id: Option<String>,
    pub success_retention: SuccessRetention,
    pub retention_class: GenerationRetentionClass,
    pub retention_delete_after: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueGenerationJobOutcome {
    Enqueued(GenerationJob),
    Existing(GenerationJob),
}

impl EnqueueGenerationJobOutcome {
    pub const fn job(&self) -> &GenerationJob {
        match self {
            Self::Enqueued(job) | Self::Existing(job) => job,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationClaim {
    pub worker_id: String,
    pub provider: String,
    pub model: String,
    pub lease_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationLease {
    pub job_id: String,
    pub attempt_id: String,
    pub worker_id: String,
    pub lease_token: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenerationUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
    pub latency_milliseconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationAttempt {
    pub id: String,
    pub job_id: String,
    pub attempt_number: u8,
    pub state: GenerationAttemptState,
    pub lease_owner: String,
    pub lease_token: String,
    pub provider: String,
    pub model: String,
    pub usage: GenerationUsage,
    pub failure_class: Option<GenerationFailureClass>,
    pub failure_code: Option<GenerationFailureCode>,
    pub provider_status: Option<u16>,
    pub provider_request_id: Option<String>,
    pub artifact_id: Option<String>,
    pub output_digest: Option<Sha256Digest>,
    pub started_at: String,
    pub heartbeat_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedGenerationJob {
    pub job: GenerationJob,
    pub attempt: GenerationAttempt,
    pub lease: GenerationLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationSuccess {
    pub artifact_id: Option<String>,
    pub output_digest: Sha256Digest,
    pub usage: GenerationUsage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationAttemptFailure {
    pub code: GenerationFailureCode,
    pub provider_status: Option<u16>,
    pub provider_request_id: Option<String>,
    pub usage: GenerationUsage,
    pub output_digest: Option<Sha256Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerationAttemptFinish {
    Succeeded(GenerationSuccess),
    Failed(GenerationAttemptFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationAttemptFinishOutcome {
    Succeeded,
    RetryScheduled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationAttemptDocument {
    #[serde(rename = "_id")]
    pub(super) id: String,
    pub(super) attempt_number: i32,
    pub(super) state: String,
    pub(super) lease_owner: String,
    pub(super) lease_token_digest: String,
    pub(super) provider: String,
    pub(super) model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) total_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cost_microusd: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) latency_milliseconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) failure_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) provider_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) provider_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_digest: Option<String>,
    pub(super) started_at: DateTime,
    pub(super) heartbeat_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) finished_at: Option<DateTime>,
    pub(super) created_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationJobDocument {
    #[serde(rename = "_id")]
    pub(super) id: String,
    pub(super) schema_version: u32,
    pub(super) campaign_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) origin_event_id: Option<String>,
    pub(super) origin_campaign_revision: i64,
    pub(super) purpose: String,
    pub(super) idempotency_key: String,
    pub(super) request_fingerprint: String,
    pub(super) state: String,
    pub(super) priority: i32,
    pub(super) available_at: DateTime,
    pub(super) input_digest: String,
    pub(super) prompt_digest: String,
    pub(super) policy_digest: String,
    pub(super) config_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) correlation_id: Option<String>,
    pub(super) attempt_count: i32,
    pub(super) max_attempts: i32,
    pub(super) campaign_concurrency_limit: i32,
    pub(super) attempts: Vec<GenerationAttemptDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_token_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_expires_at: Option<DateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_failure_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) artifact_id: Option<String>,
    pub(super) success_retention: String,
    pub(super) retention_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) purge_at: Option<DateTime>,
    pub(super) created_at: DateTime,
    pub(super) updated_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) completed_at: Option<DateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pending_artifact_id: Option<String>,
}

impl GenerationJobDocument {
    pub(super) fn to_public(&self) -> Result<GenerationJob, GenerationJobStoreError> {
        let job = GenerationJob {
            id: self.id.clone(),
            campaign_session_id: self.campaign_id.clone(),
            origin_turn_id: self.origin_event_id.clone(),
            origin_campaign_revision: from_i64(self.origin_campaign_revision)?,
            purpose: self.purpose.parse()?,
            idempotency_key: self.idempotency_key.clone(),
            state: self.state.parse()?,
            input_digest: stored_digest(&self.input_digest)?,
            prompt_digest: stored_digest(&self.prompt_digest)?,
            policy_digest: stored_digest(&self.policy_digest)?,
            config_digest: stored_digest(&self.config_digest)?,
            output_digest: self
                .output_digest
                .as_deref()
                .map(stored_digest)
                .transpose()?,
            correlation_id: self.correlation_id.clone(),
            attempt_count: u8::try_from(self.attempt_count)
                .map_err(|_| GenerationJobStoreError::NumericRange)?,
            max_attempts: u8::try_from(self.max_attempts)
                .map_err(|_| GenerationJobStoreError::NumericRange)?,
            retry_at: (self.state == "queued")
                .then(|| date_string(self.available_at))
                .transpose()?,
            lease_owner: self.lease_owner.clone(),
            lease_token: self.lease_token_digest.clone(),
            lease_expires_at: self.lease_expires_at.map(date_string).transpose()?,
            last_failure_class: self
                .last_failure_class
                .as_deref()
                .map(str::parse)
                .transpose()?,
            last_failure_code: self
                .last_failure_code
                .as_deref()
                .map(str::parse)
                .transpose()?,
            artifact_id: self.artifact_id.clone(),
            success_retention: self.success_retention.parse()?,
            retention_class: self.retention_class.parse()?,
            retention_delete_after: self.purge_at.map(date_string).transpose()?,
            created_at: date_string(self.created_at)?,
            updated_at: date_string(self.updated_at)?,
            completed_at: self.completed_at.map(date_string).transpose()?,
        };
        validate_loaded_job(&job)?;
        Ok(job)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnqueueReceiptDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u32,
    scope_kind: String,
    scope_id: String,
    actor_account_id: String,
    command_kind: String,
    idempotency_key: String,
    request_fingerprint: String,
    state: String,
    result_job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin_event_id: Option<String>,
    created_at: DateTime,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignRevisionReference {
    #[serde(rename = "_id")]
    id: String,
    revision: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnReference {
    #[serde(rename = "_id")]
    id: String,
    campaign_id: String,
    sequence: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactReference {
    #[serde(rename = "_id")]
    id: String,
    job_id: String,
    campaign_id: String,
    entity_kind: String,
    state: String,
}

impl MongoRepository {
    pub async fn enqueue_generation_job(
        &self,
        new_job: &NewGenerationJob,
    ) -> Result<EnqueueGenerationJobOutcome, GenerationJobStoreError> {
        validate_new_job(new_job)?;
        if let Some(governance) = new_job.governance.as_ref() {
            validate_new_governance(governance)?;
        }
        let requested = new_job.clone();
        let request_fingerprint = enqueue_fingerprint(&requested);
        let store = self.store().clone();
        let transaction_store = store.clone();
        let jobs = generation_jobs(&store);
        let receipts = enqueue_receipts(&store);
        transaction_store
            .with_transaction(move |session| {
                let requested = requested.clone();
                let request_fingerprint = request_fingerprint.clone();
                let jobs = jobs.clone();
                let receipts = receipts.clone();
                let store = store.clone();
                Box::pin(async move {
                    if let Some(existing) = jobs
                        .find_one(doc! {
                            "campaign_id": &requested.campaign_session_id,
                            "purpose": requested.purpose.as_str(),
                            "idempotency_key": &requested.idempotency_key,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("load generation replay", error))?
                    {
                        let existing_public = match existing.to_public() {
                            Ok(value) => value,
                            Err(error) => return Ok(Err(error)),
                        };
                        if let Err(error) = ensure_matching_replay(&existing_public, &requested) {
                            return Ok(Err(error));
                        }
                        let governance = load_governance_receipt_by_key(
                            &store,
                            session,
                            &requested.campaign_session_id,
                            requested.purpose,
                            &requested.idempotency_key,
                        )
                        .await?;
                        match (governance, requested.governance.as_ref()) {
                            (Some(existing), Some(requested_governance))
                                if existing.job_id == existing_public.id =>
                            {
                                if let Err(error) = ensure_matching_governance_receipt(
                                    &existing,
                                    requested_governance,
                                ) {
                                    return Ok(Err(error));
                                }
                            }
                            (None, None) => {}
                            _ => return Ok(Err(GenerationJobStoreError::IdempotencyConflict)),
                        }
                        return Ok(Ok(EnqueueGenerationJobOutcome::Existing(existing_public)));
                    }

                    let scope_kind = enqueue_scope_kind(requested.purpose);
                    if let Some(receipt) = receipts
                        .find_one(doc! {
                            "scope_kind": scope_kind,
                            "scope_id": &requested.campaign_session_id,
                            "idempotency_key": &requested.idempotency_key,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load closed generation receipt", error)
                        })?
                    {
                        return Ok(Err(
                            if receipt.request_fingerprint == request_fingerprint.as_str() {
                                GenerationJobStoreError::IdempotencyReceiptClosed
                            } else {
                                GenerationJobStoreError::IdempotencyConflict
                            },
                        ));
                    }

                    let campaigns =
                        store.collection::<CampaignRevisionReference>(CollectionName::Campaigns);
                    let Some(campaign) = campaigns
                        .find_one(doc! { "_id": &requested.campaign_session_id })
                        .projection(doc! { "_id": 1, "revision": 1 })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load generation campaign", error)
                        })?
                    else {
                        return Ok(Err(GenerationJobStoreError::NotFound {
                            job_id: requested.id.clone(),
                        }));
                    };
                    store
                        .document_collection(CollectionName::Campaigns)
                        .update_one(
                            doc! { "_id": &campaign.id },
                            doc! { "$inc": { "generation_queue_revision": 1_i64 } },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("serialize generation enqueue", error)
                        })?;

                    if requested.purpose == GenerationPurpose::Illustration {
                        let limits = preflight_illustration_request_limits(
                            &store,
                            session,
                            &requested.campaign_session_id,
                            requested.origin_turn_id.as_deref(),
                        )
                        .await?;
                        if let Err(error) = limits {
                            return Ok(Err(error));
                        }
                    }

                    if let Some(turn_id) = requested.origin_turn_id.as_deref() {
                        let turns = store.collection::<TurnReference>(CollectionName::TurnEvents);
                        let Some(turn) = turns
                            .find_one(doc! {
                                "_id": turn_id,
                                "campaign_id": &requested.campaign_session_id,
                            })
                            .projection(doc! { "_id": 1, "campaign_id": 1, "sequence": 1 })
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("load generation origin turn", error)
                            })?
                        else {
                            return Ok(Err(GenerationJobStoreError::InvalidInput(
                                "origin turn does not belong to the campaign",
                            )));
                        };
                        if turn.id != turn_id || turn.campaign_id != requested.campaign_session_id {
                            return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                "origin turn projection is inconsistent",
                            )));
                        }
                        let committed_revision = match u64::try_from(turn.sequence)
                            .ok()
                            .and_then(|sequence| sequence.checked_add(1))
                        {
                            Some(value) => value,
                            None => return Ok(Err(GenerationJobStoreError::NumericRange)),
                        };
                        let current_revision = match u64::try_from(campaign.revision) {
                            Ok(value) => value,
                            Err(_) => return Ok(Err(GenerationJobStoreError::NumericRange)),
                        };
                        if committed_revision != requested.origin_campaign_revision
                            || current_revision < requested.origin_campaign_revision
                        {
                            return Ok(Err(GenerationJobStoreError::OriginRevisionConflict {
                                expected: requested.origin_campaign_revision,
                                actual: current_revision,
                            }));
                        }
                    } else {
                        let current_revision = match u64::try_from(campaign.revision) {
                            Ok(value) => value,
                            Err(_) => return Ok(Err(GenerationJobStoreError::NumericRange)),
                        };
                        if current_revision != requested.origin_campaign_revision {
                            return Ok(Err(GenerationJobStoreError::OriginRevisionConflict {
                                expected: requested.origin_campaign_revision,
                                actual: current_revision,
                            }));
                        }
                    }

                    if let Some(governance) = requested.governance.as_ref() {
                        match preflight_generation_governance(
                            &store,
                            session,
                            &requested.campaign_session_id,
                            requested.purpose,
                            governance,
                        )
                        .await?
                        {
                            Ok(()) => {}
                            Err(error) => return Ok(Err(error)),
                        }
                    }

                    let now = DateTime::now();
                    let document = GenerationJobDocument {
                        id: requested.id.clone(),
                        schema_version: SCHEMA_VERSION,
                        campaign_id: requested.campaign_session_id.clone(),
                        origin_event_id: requested.origin_turn_id.clone(),
                        origin_campaign_revision: match to_i64(requested.origin_campaign_revision) {
                            Ok(value) => value,
                            Err(error) => return Ok(Err(error)),
                        },
                        purpose: requested.purpose.as_str().to_owned(),
                        idempotency_key: requested.idempotency_key.clone(),
                        request_fingerprint: request_fingerprint.as_str().to_owned(),
                        state: GenerationJobState::Queued.as_str().to_owned(),
                        priority: requested.purpose.priority(),
                        available_at: now,
                        input_digest: requested.input_digest.as_str().to_owned(),
                        prompt_digest: requested.prompt_digest.as_str().to_owned(),
                        policy_digest: requested.policy_digest.as_str().to_owned(),
                        config_digest: requested.config_digest.as_str().to_owned(),
                        output_digest: None,
                        correlation_id: requested.correlation_id.clone(),
                        attempt_count: 0,
                        max_attempts: i32::from(requested.max_attempts),
                        campaign_concurrency_limit: requested
                            .governance
                            .as_ref()
                            .map_or(i32::MAX, |governance| {
                                i32::from(governance.limits.max_campaign_concurrency)
                            }),
                        attempts: Vec::new(),
                        lease_owner: None,
                        lease_token_digest: None,
                        lease_expires_at: None,
                        last_failure_class: None,
                        last_failure_code: None,
                        artifact_id: None,
                        success_retention: requested.success_retention.as_str().to_owned(),
                        retention_class: GenerationRetentionClass::Pending.as_str().to_owned(),
                        purge_at: None,
                        created_at: now,
                        updated_at: now,
                        completed_at: None,
                        pending_artifact_id: None,
                    };
                    jobs.insert_one(document.clone())
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("insert generation job", error))?;
                    if let Some(governance) = requested.governance.as_ref() {
                        insert_generation_governance_receipt(
                            &store,
                            session,
                            &requested.id,
                            &requested.campaign_session_id,
                            requested.origin_turn_id.as_deref(),
                            requested.purpose,
                            &requested.idempotency_key,
                            governance,
                        )
                        .await?;
                    }
                    receipts
                        .insert_one(EnqueueReceiptDocument {
                            id: format!("command-receipt:generation:{}", requested.id),
                            schema_version: SCHEMA_VERSION,
                            scope_kind: scope_kind.to_owned(),
                            scope_id: requested.campaign_session_id.clone(),
                            actor_account_id: "account:system-generation".to_owned(),
                            command_kind: "enqueue_generation_job".to_owned(),
                            idempotency_key: requested.idempotency_key.clone(),
                            request_fingerprint: request_fingerprint.as_str().to_owned(),
                            state: "committed".to_owned(),
                            result_job_id: requested.id.clone(),
                            origin_event_id: requested.origin_turn_id.clone(),
                            created_at: now,
                        })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("insert generation enqueue receipt", error)
                        })?;
                    let public = match document.to_public() {
                        Ok(value) => value,
                        Err(error) => return Ok(Err(error)),
                    };
                    Ok(Ok(EnqueueGenerationJobOutcome::Enqueued(public)))
                })
            })
            .await
            .map_err(map_database)?
    }

    pub async fn load_generation_job(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<Option<GenerationJob>, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        operation(
            self.store(),
            "load generation job",
            generation_jobs(self.store()).find_one(doc! {
                "_id": job_id,
                "campaign_id": campaign_session_id,
            }),
        )
        .await?
        .map(|document| document.to_public())
        .transpose()
    }

    pub async fn load_generation_job_by_key(
        &self,
        campaign_session_id: &str,
        purpose: GenerationPurpose,
        idempotency_key: &str,
    ) -> Result<Option<GenerationJob>, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(idempotency_key, "idempotency key is invalid")?;
        operation(
            self.store(),
            "load generation job by key",
            generation_jobs(self.store()).find_one(doc! {
                "campaign_id": campaign_session_id,
                "purpose": purpose.as_str(),
                "idempotency_key": idempotency_key,
            }),
        )
        .await?
        .map(|document| document.to_public())
        .transpose()
    }

    pub async fn list_generation_attempts(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<Vec<GenerationAttempt>, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        let Some(document) = operation(
            self.store(),
            "load generation attempts",
            generation_jobs(self.store()).find_one(doc! {
                "_id": job_id,
                "campaign_id": campaign_session_id,
            }),
        )
        .await?
        else {
            return Ok(Vec::new());
        };
        document
            .attempts
            .iter()
            .map(|attempt| attempt.to_public(&document.id))
            .collect()
    }

    pub async fn claim_generation_job(
        &self,
        claim: &GenerationClaim,
    ) -> Result<Option<ClaimedGenerationJob>, GenerationJobStoreError> {
        self.claim_generation_job_matching(claim, None, None).await
    }

    pub async fn claim_generation_job_for_purpose(
        &self,
        purpose: GenerationPurpose,
        claim: &GenerationClaim,
    ) -> Result<Option<ClaimedGenerationJob>, GenerationJobStoreError> {
        self.claim_generation_job_matching(claim, None, Some(purpose))
            .await
    }

    pub async fn claim_generation_job_by_id(
        &self,
        campaign_session_id: &str,
        job_id: &str,
        claim: &GenerationClaim,
    ) -> Result<Option<ClaimedGenerationJob>, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        self.claim_generation_job_matching(claim, Some((campaign_session_id, job_id)), None)
            .await
    }

    async fn claim_generation_job_matching(
        &self,
        claim: &GenerationClaim,
        exact_job: Option<(&str, &str)>,
        purpose_filter: Option<GenerationPurpose>,
    ) -> Result<Option<ClaimedGenerationJob>, GenerationJobStoreError> {
        validate_claim(claim)?;
        let claim = claim.clone();
        let exact_job = exact_job.map(|(campaign, job)| (campaign.to_owned(), job.to_owned()));
        let attempt_id = format!("generation-attempt:{}", Uuid::new_v4());
        let lease_token = format!("generation-lease:{}", Uuid::new_v4());
        let lease_digest = opaque_digest("generation-lease-token/v1", &lease_token);
        let store = self.store().clone();
        let transaction_store = store.clone();
        let jobs = generation_jobs(&store);
        transaction_store
            .with_transaction(move |session| {
                let claim = claim.clone();
                let exact_job = exact_job.clone();
                let attempt_id = attempt_id.clone();
                let lease_token = lease_token.clone();
                let lease_digest = lease_digest.clone();
                let store = store.clone();
                let jobs = jobs.clone();
                Box::pin(async move {
                    let now = DateTime::now();
                    let mut filter = doc! {
                        "$or": [
                            {
                                "state": "queued",
                                "available_at": { "$lte": now },
                            },
                            {
                                "state": "running",
                                "lease_expires_at": { "$lte": now },
                            },
                        ],
                    };
                    if let Some((campaign_id, job_id)) = exact_job.as_ref() {
                        filter.insert("_id", job_id);
                        filter.insert("campaign_id", campaign_id);
                    }
                    if let Some(purpose) = purpose_filter {
                        filter.insert("purpose", purpose.as_str());
                    }
                    let mut cursor = jobs
                        .find(filter)
                        .sort(doc! { "priority": -1, "available_at": 1, "_id": 1 })
                        .limit(CLAIM_SCAN_LIMIT)
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("find claimable generation jobs", error)
                        })?;
                    let mut candidates = Vec::new();
                    while cursor.advance(&mut *session).await.map_err(|error| {
                        PersistenceError::mongo("read claimable generation job", error)
                    })? {
                        candidates.push(cursor.deserialize_current().map_err(|error| {
                            PersistenceError::mongo("decode claimable generation job", error)
                        })?);
                    }
                    drop(cursor);

                    for mut candidate in candidates {
                        let purpose = match candidate.purpose.parse::<GenerationPurpose>() {
                            Ok(value) => value,
                            Err(error) => return Ok(Err(error)),
                        };
                        let touched = store
                            .document_collection(CollectionName::Campaigns)
                            .update_one(
                                doc! { "_id": &candidate.campaign_id },
                                doc! { "$inc": { "generation_claim_revision": 1_i64 } },
                            )
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("serialize generation claim", error)
                            })?;
                        if touched.matched_count != 1 {
                            return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                "generation job references a missing campaign",
                            )));
                        }

                        let active = jobs
                            .count_documents(doc! {
                                "campaign_id": &candidate.campaign_id,
                                "_id": { "$ne": &candidate.id },
                                "state": "running",
                                "lease_expires_at": { "$gt": now },
                            })
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo(
                                    "count campaign generation concurrency",
                                    error,
                                )
                            })?;
                        let illustration_active = if purpose == GenerationPurpose::Illustration {
                            jobs.count_documents(doc! {
                                "campaign_id": &candidate.campaign_id,
                                "_id": { "$ne": &candidate.id },
                                "purpose": GenerationPurpose::Illustration.as_str(),
                                "state": "running",
                                "lease_expires_at": { "$gt": now },
                            })
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("count active campaign illustration", error)
                            })?
                        } else {
                            0
                        };
                        let campaign_concurrency_limit =
                            match u64::try_from(candidate.campaign_concurrency_limit) {
                                Ok(0) | Err(_) => {
                                    return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                        "campaign concurrency limit is invalid",
                                    )));
                                }
                                Ok(value) => value,
                            };
                        if active >= campaign_concurrency_limit || illustration_active > 0 {
                            continue;
                        }

                        let expired = candidate.state == GenerationJobState::Running.as_str();
                        if expired {
                            let previous_digest = {
                                let Some(previous) = candidate.attempts.last_mut() else {
                                    return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                        "expired job has no running attempt",
                                    )));
                                };
                                if previous.state != GenerationAttemptState::Running.as_str()
                                    || candidate.lease_token_digest.as_deref()
                                        != Some(previous.lease_token_digest.as_str())
                                {
                                    return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                        "expired job attempt does not match its lease",
                                    )));
                                }
                                previous.state = GenerationAttemptState::Failed.as_str().to_owned();
                                previous.failure_class =
                                    Some(GenerationFailureClass::Transient.as_str().to_owned());
                                previous.failure_code =
                                    Some(GenerationFailureCode::LeaseExpired.as_str().to_owned());
                                previous.heartbeat_at = now;
                                previous.finished_at = Some(now);
                                previous.lease_token_digest.clone()
                            };
                            if candidate.attempt_count >= candidate.max_attempts {
                                candidate.state = GenerationJobState::Failed.as_str().to_owned();
                                candidate.lease_owner = None;
                                candidate.lease_token_digest = None;
                                candidate.lease_expires_at = None;
                                candidate.last_failure_class =
                                    Some(GenerationFailureClass::Transient.as_str().to_owned());
                                candidate.last_failure_code =
                                    Some(GenerationFailureCode::LeaseExpired.as_str().to_owned());
                                candidate.retention_class =
                                    GenerationRetentionClass::FailedMetadata7Days
                                        .as_str()
                                        .to_owned();
                                candidate.purge_at = Some(add_duration(now, FAILED_RETENTION));
                                candidate.completed_at = Some(now);
                                candidate.updated_at = now;
                                retire_pending_artifact(
                                    &store,
                                    session,
                                    &candidate,
                                    add_duration(now, FAILED_RETENTION),
                                )
                                .await?;
                                let replaced = jobs
                                    .replace_one(
                                        doc! {
                                            "_id": &candidate.id,
                                            "state": "running",
                                            "lease_token_digest": &previous_digest,
                                            "lease_expires_at": { "$lte": now },
                                        },
                                        candidate.clone(),
                                    )
                                    .session(&mut *session)
                                    .await
                                    .map_err(|error| {
                                        PersistenceError::mongo(
                                            "terminalize expired generation lease",
                                            error,
                                        )
                                    })?;
                                if replaced.matched_count != 1 {
                                    continue;
                                }
                                match record_generation_attempt_usage(
                                    &store,
                                    session,
                                    &candidate.id,
                                    purpose,
                                    &GenerationUsage::default(),
                                    true,
                                )
                                .await?
                                {
                                    Ok(()) => {}
                                    Err(error) => return Ok(Err(error)),
                                }
                                insert_job_audit(
                                    &store,
                                    session,
                                    &candidate,
                                    "lease_expired",
                                    "failed",
                                )
                                .await?;
                                return Ok(Ok(None));
                            }
                        }

                        let next_attempt = match candidate.attempt_count.checked_add(1) {
                            Some(value) if value <= candidate.max_attempts => value,
                            _ => return Ok(Err(GenerationJobStoreError::NumericRange)),
                        };
                        let expires_at = add_duration(now, claim.lease_duration);
                        let attempt = GenerationAttemptDocument {
                            id: attempt_id.clone(),
                            attempt_number: next_attempt,
                            state: GenerationAttemptState::Running.as_str().to_owned(),
                            lease_owner: claim.worker_id.clone(),
                            lease_token_digest: lease_digest.clone(),
                            provider: claim.provider.clone(),
                            model: claim.model.clone(),
                            prompt_tokens: None,
                            completion_tokens: None,
                            total_tokens: None,
                            cost_microusd: None,
                            latency_milliseconds: None,
                            failure_class: None,
                            failure_code: None,
                            provider_status: None,
                            provider_request_id: None,
                            artifact_id: None,
                            output_digest: None,
                            started_at: now,
                            heartbeat_at: now,
                            finished_at: None,
                            created_at: now,
                        };
                        candidate.attempts.push(attempt.clone());
                        candidate.state = GenerationJobState::Running.as_str().to_owned();
                        candidate.attempt_count = next_attempt;
                        candidate.available_at = now;
                        candidate.lease_owner = Some(claim.worker_id.clone());
                        candidate.lease_token_digest = Some(lease_digest.clone());
                        candidate.lease_expires_at = Some(expires_at);
                        candidate.last_failure_class = None;
                        candidate.last_failure_code = None;
                        candidate.updated_at = now;
                        let state_filter = if expired {
                            doc! {
                                "state": "running",
                                "lease_expires_at": { "$lte": now },
                            }
                        } else {
                            doc! {
                                "state": "queued",
                                "available_at": { "$lte": now },
                            }
                        };
                        let mut update_filter = doc! { "_id": &candidate.id };
                        update_filter.extend(state_filter);
                        let claimed = jobs
                            .find_one_and_replace(update_filter, candidate.clone())
                            .return_document(ReturnDocument::After)
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("claim generation job", error)
                            })?;
                        let Some(claimed) = claimed else {
                            continue;
                        };
                        if expired {
                            match record_generation_attempt_usage(
                                &store,
                                session,
                                &candidate.id,
                                purpose,
                                &GenerationUsage::default(),
                                false,
                            )
                            .await?
                            {
                                Ok(()) => {}
                                Err(error) => return Ok(Err(error)),
                            }
                        }
                        let mut public_job = match claimed.to_public() {
                            Ok(value) => value,
                            Err(error) => return Ok(Err(error)),
                        };
                        public_job.lease_token = Some(lease_token.clone());
                        let mut public_attempt = match attempt.to_public(&candidate.id) {
                            Ok(value) => value,
                            Err(error) => return Ok(Err(error)),
                        };
                        public_attempt.lease_token = lease_token.clone();
                        return Ok(Ok(Some(ClaimedGenerationJob {
                            lease: GenerationLease {
                                job_id: candidate.id,
                                attempt_id,
                                worker_id: claim.worker_id,
                                lease_token,
                            },
                            job: public_job,
                            attempt: public_attempt,
                        })));
                    }
                    Ok(Ok(None))
                })
            })
            .await
            .map_err(map_database)?
    }

    pub async fn heartbeat_generation_job(
        &self,
        lease: &GenerationLease,
        lease_duration: Duration,
    ) -> Result<GenerationJob, GenerationJobStoreError> {
        validate_lease(lease)?;
        validate_lease_duration(lease_duration)?;
        let now = DateTime::now();
        let digest = opaque_digest("generation-lease-token/v1", &lease.lease_token);
        let updated = operation(
            self.store(),
            "heartbeat generation lease",
            generation_jobs(self.store())
                .find_one_and_update(
                    doc! {
                        "_id": &lease.job_id,
                        "state": "running",
                        "lease_owner": &lease.worker_id,
                        "lease_token_digest": &digest,
                        "lease_expires_at": { "$gt": now },
                        "attempts": {
                            "$elemMatch": {
                                "_id": &lease.attempt_id,
                                "state": "running",
                                "lease_owner": &lease.worker_id,
                                "lease_token_digest": &digest,
                            }
                        },
                    },
                    doc! {
                        "$set": {
                            "lease_expires_at": add_duration(now, lease_duration),
                            "attempts.$[attempt].heartbeat_at": now,
                            "updated_at": now,
                        }
                    },
                )
                .array_filters(vec![doc! {
                    "attempt._id": &lease.attempt_id,
                    "attempt.state": "running",
                    "attempt.lease_token_digest": &digest,
                }])
                .return_document(ReturnDocument::After),
        )
        .await?
        .ok_or(GenerationJobStoreError::LostLease)?;
        let mut public = updated.to_public()?;
        public.lease_token = Some(lease.lease_token.clone());
        Ok(public)
    }

    pub async fn finish_generation_attempt(
        &self,
        lease: &GenerationLease,
        finish: GenerationAttemptFinish,
    ) -> Result<GenerationAttemptFinishOutcome, GenerationJobStoreError> {
        match finish {
            GenerationAttemptFinish::Succeeded(success) => {
                self.succeed_generation_job(lease, &success).await?;
                Ok(GenerationAttemptFinishOutcome::Succeeded)
            }
            GenerationAttemptFinish::Failed(failure) => {
                self.fail_generation_attempt(lease, &failure).await
            }
        }
    }

    pub async fn succeed_generation_job(
        &self,
        lease: &GenerationLease,
        success: &GenerationSuccess,
    ) -> Result<GenerationJob, GenerationJobStoreError> {
        validate_lease(lease)?;
        if let Some(artifact_id) = success.artifact_id.as_deref() {
            validate_identifier(artifact_id, "artifact id is invalid")?;
        }
        validate_usage(&success.usage)?;
        let store = self.store().clone();
        let transaction_store = store.clone();
        let lease = lease.clone();
        let success = success.clone();
        let result = transaction_store
            .with_transaction(move |session| {
                let store = store.clone();
                let lease = lease.clone();
                let success = success.clone();
                Box::pin(async move {
                    complete_leased_job_in_transaction(
                        &store,
                        session,
                        &lease,
                        success.artifact_id.as_deref(),
                        &success.output_digest,
                        &success.usage,
                        None,
                        false,
                    )
                    .await
                    .map(|result| result.map(|(document, _)| document))
                })
            })
            .await
            .map_err(map_database)??;
        result.to_public()
    }

    pub async fn fail_generation_attempt(
        &self,
        lease: &GenerationLease,
        failure: &GenerationAttemptFailure,
    ) -> Result<GenerationAttemptFinishOutcome, GenerationJobStoreError> {
        validate_lease(lease)?;
        validate_failure(failure)?;
        let store = self.store().clone();
        let transaction_store = store.clone();
        let lease = lease.clone();
        let failure = failure.clone();
        transaction_store
            .with_transaction(move |session| {
                let store = store.clone();
                let lease = lease.clone();
                let failure = failure.clone();
                Box::pin(async move {
                    let output_digest = failure
                        .output_digest
                        .clone()
                        .unwrap_or_else(|| Sha256Digest::from_bytes([0; 32]));
                    complete_leased_job_in_transaction(
                        &store,
                        session,
                        &lease,
                        None,
                        &output_digest,
                        &failure.usage,
                        Some((&failure, failure.output_digest.as_ref())),
                        true,
                    )
                    .await
                    .map(|result| result.map(|(_, outcome)| outcome))
                })
            })
            .await
            .map_err(map_database)?
    }

    pub async fn cancel_generation_job(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<GenerationJob, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        let store = self.store().clone();
        let transaction_store = store.clone();
        let campaign_id = campaign_session_id.to_owned();
        let job_id = job_id.to_owned();
        let document = transaction_store
            .with_transaction(move |session| {
                let store = store.clone();
                let campaign_id = campaign_id.clone();
                let job_id = job_id.clone();
                Box::pin(async move {
                    let jobs = generation_jobs(&store);
                    let Some(mut job) = jobs
                        .find_one(doc! { "_id": &job_id, "campaign_id": &campaign_id })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load generation job for cancellation", error)
                        })?
                    else {
                        return Ok(Err(GenerationJobStoreError::NotFound { job_id }));
                    };
                    let state = match job.state.parse::<GenerationJobState>() {
                        Ok(value) => value,
                        Err(error) => return Ok(Err(error)),
                    };
                    if state == GenerationJobState::Cancelled {
                        return Ok(Ok(job));
                    }
                    if matches!(
                        state,
                        GenerationJobState::Succeeded | GenerationJobState::Failed
                    ) {
                        return Ok(Err(GenerationJobStoreError::InvalidTransition {
                            job_id,
                            state,
                        }));
                    }
                    match state {
                        GenerationJobState::Queued => {
                            release_generation_budget(&store, session, &job.id).await?;
                        }
                        GenerationJobState::Running => {
                            let Some(attempt) = job.attempts.last_mut() else {
                                return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                    "running job has no matching attempt",
                                )));
                            };
                            if attempt.state != "running"
                                || job.lease_token_digest.as_deref()
                                    != Some(attempt.lease_token_digest.as_str())
                            {
                                return Ok(Err(GenerationJobStoreError::InvalidStoredData(
                                    "running job has no matching attempt",
                                )));
                            }
                            let now = DateTime::now();
                            attempt.state = GenerationAttemptState::Cancelled.as_str().to_owned();
                            attempt.failure_class =
                                Some(GenerationFailureClass::Permanent.as_str().to_owned());
                            attempt.failure_code =
                                Some(GenerationFailureCode::Cancelled.as_str().to_owned());
                            attempt.heartbeat_at = now;
                            attempt.finished_at = Some(now);
                            settle_unknown_generation_usage(&store, session, &job.id).await?;
                        }
                        _ => {}
                    }
                    let now = DateTime::now();
                    let original_state = job.state.clone();
                    let original_digest = job.lease_token_digest.clone();
                    job.state = GenerationJobState::Cancelled.as_str().to_owned();
                    job.lease_owner = None;
                    job.lease_token_digest = None;
                    job.lease_expires_at = None;
                    job.last_failure_class =
                        Some(GenerationFailureClass::Permanent.as_str().to_owned());
                    job.last_failure_code =
                        Some(GenerationFailureCode::Cancelled.as_str().to_owned());
                    job.retention_class = GenerationRetentionClass::FailedMetadata7Days
                        .as_str()
                        .to_owned();
                    job.purge_at = Some(add_duration(now, FAILED_RETENTION));
                    job.completed_at = Some(now);
                    job.updated_at = now;
                    retire_pending_artifact(
                        &store,
                        session,
                        &job,
                        add_duration(now, FAILED_RETENTION),
                    )
                    .await?;
                    let mut filter = doc! { "_id": &job.id, "state": original_state };
                    if let Some(digest) = original_digest {
                        filter.insert("lease_token_digest", digest);
                    }
                    let replaced = jobs
                        .replace_one(filter, job.clone())
                        .session(&mut *session)
                        .await
                        .map_err(|error| PersistenceError::mongo("cancel generation job", error))?;
                    if replaced.matched_count != 1 {
                        return Ok(Err(GenerationJobStoreError::LostLease));
                    }
                    insert_job_audit(&store, session, &job, "cancel", "cancelled").await?;
                    Ok(Ok(job))
                })
            })
            .await
            .map_err(map_database)??;
        document.to_public()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn complete_leased_job_in_transaction(
    store: &MongoStore,
    session: &mut ClientSession,
    lease: &GenerationLease,
    artifact_id: Option<&str>,
    output_digest: &Sha256Digest,
    usage: &GenerationUsage,
    failure: Option<(&GenerationAttemptFailure, Option<&Sha256Digest>)>,
    allow_retry: bool,
) -> Result<
    Result<(GenerationJobDocument, GenerationAttemptFinishOutcome), GenerationJobStoreError>,
    PersistenceError,
> {
    let now = DateTime::now();
    let lease_digest = opaque_digest("generation-lease-token/v1", &lease.lease_token);
    let jobs = generation_jobs(store);
    let Some(mut job) = jobs
        .find_one(doc! {
            "_id": &lease.job_id,
            "state": "running",
            "lease_owner": &lease.worker_id,
            "lease_token_digest": &lease_digest,
            "lease_expires_at": { "$gt": now },
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load current generation lease", error))?
    else {
        return Ok(Err(GenerationJobStoreError::LostLease));
    };
    let purpose = match job.purpose.parse::<GenerationPurpose>() {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    if purpose == GenerationPurpose::Illustration && failure.is_none() && artifact_id.is_none() {
        return Ok(Err(GenerationJobStoreError::InvalidInput(
            "illustration success requires a validated artifact",
        )));
    }
    if purpose == GenerationPurpose::Illustration
        && artifact_id.is_some()
        && job.pending_artifact_id.as_deref() != artifact_id
    {
        return Ok(Err(GenerationJobStoreError::InvalidInput(
            "illustration artifact does not own the job publication slot",
        )));
    }
    let mut artifact = None;
    if let Some(artifact_id) = artifact_id {
        let assets = store.collection::<ArtifactReference>(CollectionName::GeneratedAssets);
        artifact = assets
            .find_one(doc! {
                "_id": artifact_id,
                "job_id": &job.id,
                "campaign_id": &job.campaign_id,
                "entity_kind": "turn_scene_image",
                "state": "staged",
            })
            .projection(doc! {
                "_id": 1,
                "job_id": 1,
                "campaign_id": 1,
                "entity_kind": 1,
                "state": 1,
            })
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("verify generation artifact ownership", error)
            })?;
        if artifact.is_none() {
            return Ok(Err(GenerationJobStoreError::InvalidInput(
                "artifact does not belong to the generation campaign",
            )));
        }
    }
    let Some(attempt) = job
        .attempts
        .iter_mut()
        .find(|attempt| attempt.id == lease.attempt_id)
    else {
        return Ok(Err(GenerationJobStoreError::LostLease));
    };
    if attempt.state != GenerationAttemptState::Running.as_str()
        || attempt.lease_owner != lease.worker_id
        || attempt.lease_token_digest != lease_digest
    {
        return Ok(Err(GenerationJobStoreError::LostLease));
    }
    let bindings = match usage_bindings(usage) {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    attempt.prompt_tokens = bindings.prompt_tokens;
    attempt.completion_tokens = bindings.completion_tokens;
    attempt.total_tokens = bindings.total_tokens;
    attempt.cost_microusd = bindings.cost_microusd;
    attempt.latency_milliseconds = bindings.latency_milliseconds;
    attempt.heartbeat_at = now;
    attempt.finished_at = Some(now);
    attempt.artifact_id = artifact_id.map(str::to_owned);
    attempt.output_digest = failure.and_then(|(_, digest)| digest).map_or_else(
        || Some(output_digest.as_str().to_owned()),
        |digest| Some(digest.as_str().to_owned()),
    );

    let (terminal, outcome) = if let Some((failure, _)) = failure {
        attempt.state = GenerationAttemptState::Failed.as_str().to_owned();
        attempt.failure_class = Some(failure.code.class().as_str().to_owned());
        attempt.failure_code = Some(failure.code.as_str().to_owned());
        attempt.provider_status = failure.provider_status.map(i32::from);
        attempt.provider_request_id = failure.provider_request_id.clone();
        let attempt_count = match u8::try_from(job.attempt_count) {
            Ok(value) => value,
            Err(_) => return Ok(Err(GenerationJobStoreError::NumericRange)),
        };
        let retry = allow_retry
            .then(|| purpose.retry_delay(failure.code, attempt_count))
            .flatten()
            .filter(|_| job.attempt_count < job.max_attempts);
        if let Some(delay) = retry {
            job.state = GenerationJobState::Queued.as_str().to_owned();
            job.available_at = add_duration(now, delay);
            job.last_failure_class = Some(failure.code.class().as_str().to_owned());
            job.last_failure_code = Some(failure.code.as_str().to_owned());
            job.output_digest = failure
                .output_digest
                .as_ref()
                .map(|digest| digest.as_str().to_owned());
            (false, GenerationAttemptFinishOutcome::RetryScheduled)
        } else {
            job.state = GenerationJobState::Failed.as_str().to_owned();
            job.last_failure_class = Some(failure.code.class().as_str().to_owned());
            job.last_failure_code = Some(failure.code.as_str().to_owned());
            job.output_digest = failure
                .output_digest
                .as_ref()
                .map(|digest| digest.as_str().to_owned());
            job.retention_class = GenerationRetentionClass::FailedMetadata7Days
                .as_str()
                .to_owned();
            job.purge_at = Some(add_duration(now, FAILED_RETENTION));
            job.completed_at = Some(now);
            (true, GenerationAttemptFinishOutcome::Failed)
        }
    } else {
        attempt.state = GenerationAttemptState::Succeeded.as_str().to_owned();
        job.state = GenerationJobState::Succeeded.as_str().to_owned();
        job.output_digest = Some(output_digest.as_str().to_owned());
        job.artifact_id = artifact_id.map(str::to_owned);
        job.completed_at = Some(now);
        let success_retention = match job.success_retention.parse::<SuccessRetention>() {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        match (artifact_id, success_retention) {
            (None, _) | (Some(_), SuccessRetention::UnselectedPresentation30Days) => {
                job.retention_class = GenerationRetentionClass::UnselectedPresentation30Days
                    .as_str()
                    .to_owned();
                job.purge_at = Some(add_duration(now, UNSELECTED_RETENTION));
            }
            (Some(_), SuccessRetention::CampaignLifetime) => {
                job.retention_class = GenerationRetentionClass::CampaignLifetime
                    .as_str()
                    .to_owned();
                job.purge_at = None;
            }
        }
        (true, GenerationAttemptFinishOutcome::Succeeded)
    };
    job.lease_owner = None;
    job.lease_token_digest = None;
    job.lease_expires_at = None;
    job.updated_at = now;
    if failure.is_none() {
        job.pending_artifact_id = None;
    } else if terminal {
        retire_pending_artifact(store, session, &job, add_duration(now, FAILED_RETENTION)).await?;
    }

    match record_generation_attempt_usage(store, session, &job.id, purpose, usage, terminal).await?
    {
        Ok(()) => {}
        Err(error) => return Ok(Err(error)),
    }
    let replaced = jobs
        .replace_one(
            doc! {
                "_id": &job.id,
                "state": "running",
                "lease_owner": &lease.worker_id,
                "lease_token_digest": &lease_digest,
                "lease_expires_at": { "$gt": now },
            },
            job.clone(),
        )
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("finish generation job", error))?;
    if replaced.matched_count != 1 {
        return Ok(Err(GenerationJobStoreError::LostLease));
    }
    if let Some(artifact) = artifact {
        let published = store
            .document_collection(CollectionName::GeneratedAssets)
            .update_one(
                doc! {
                    "_id": &artifact.id,
                    "job_id": &artifact.job_id,
                    "campaign_id": &artifact.campaign_id,
                    "entity_kind": &artifact.entity_kind,
                    "state": &artifact.state,
                },
                doc! { "$set": { "state": "published", "updated_at": now } },
            )
            .session(&mut *session)
            .await
            .map_err(|error| PersistenceError::mongo("publish generated artifact", error))?;
        if published.matched_count != 1 {
            return Err(PersistenceError::SchemaDrift {
                collection: CollectionName::GeneratedAssets.as_str().to_owned(),
                detail: "validated artifact disappeared during generation settlement".to_owned(),
            });
        }
    }
    insert_job_audit(
        store,
        session,
        &job,
        if failure.is_some() {
            "attempt_failed"
        } else {
            "attempt_succeeded"
        },
        job.state.as_str(),
    )
    .await?;
    Ok(Ok((job, outcome)))
}

pub(super) async fn load_leased_job_in_transaction(
    store: &MongoStore,
    session: &mut ClientSession,
    lease: &GenerationLease,
) -> Result<Option<GenerationJobDocument>, PersistenceError> {
    let now = DateTime::now();
    let lease_digest = opaque_digest("generation-lease-token/v1", &lease.lease_token);
    generation_jobs(store)
        .find_one(doc! {
            "_id": &lease.job_id,
            "state": "running",
            "lease_owner": &lease.worker_id,
            "lease_token_digest": &lease_digest,
            "lease_expires_at": { "$gt": now },
            "attempts": {
                "$elemMatch": {
                    "_id": &lease.attempt_id,
                    "state": "running",
                    "lease_owner": &lease.worker_id,
                    "lease_token_digest": &lease_digest,
                }
            },
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load leased generation job", error))
}

async fn retire_pending_artifact(
    store: &MongoStore,
    session: &mut ClientSession,
    job: &GenerationJobDocument,
    purge_at: DateTime,
) -> Result<(), PersistenceError> {
    let Some(artifact_id) = job.pending_artifact_id.as_deref() else {
        return Ok(());
    };
    let retired = store
        .document_collection(CollectionName::GeneratedAssets)
        .update_one(
            doc! {
                "_id": artifact_id,
                "job_id": &job.id,
                "campaign_id": &job.campaign_id,
                "entity_kind": "turn_scene_image",
                "state": "staged",
            },
            doc! {
                "$set": {
                    "state": "superseded",
                    "selected": false,
                    "purge_at": purge_at,
                    "updated_at": DateTime::now(),
                },
            },
        )
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("retire pending generation artifact", error))?;
    if retired.matched_count != 1 {
        return Err(PersistenceError::SchemaDrift {
            collection: CollectionName::GeneratedAssets.as_str().to_owned(),
            detail: "pending generation artifact is missing or no longer staged".to_owned(),
        });
    }
    Ok(())
}

impl GenerationAttemptDocument {
    fn to_public(&self, job_id: &str) -> Result<GenerationAttempt, GenerationJobStoreError> {
        let usage = GenerationUsage {
            prompt_tokens: optional_i64(self.prompt_tokens)?,
            completion_tokens: optional_i64(self.completion_tokens)?,
            total_tokens: optional_i64(self.total_tokens)?,
            cost_microusd: optional_i64(self.cost_microusd)?,
            latency_milliseconds: optional_i64(self.latency_milliseconds)?,
        };
        validate_usage(&usage)?;
        Ok(GenerationAttempt {
            id: self.id.clone(),
            job_id: job_id.to_owned(),
            attempt_number: u8::try_from(self.attempt_number)
                .map_err(|_| GenerationJobStoreError::NumericRange)?,
            state: self.state.parse()?,
            lease_owner: self.lease_owner.clone(),
            lease_token: self.lease_token_digest.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            usage,
            failure_class: self.failure_class.as_deref().map(str::parse).transpose()?,
            failure_code: self.failure_code.as_deref().map(str::parse).transpose()?,
            provider_status: self
                .provider_status
                .map(u16::try_from)
                .transpose()
                .map_err(|_| GenerationJobStoreError::NumericRange)?,
            provider_request_id: self.provider_request_id.clone(),
            artifact_id: self.artifact_id.clone(),
            output_digest: self
                .output_digest
                .as_deref()
                .map(stored_digest)
                .transpose()?,
            started_at: date_string(self.started_at)?,
            heartbeat_at: date_string(self.heartbeat_at)?,
            finished_at: self.finished_at.map(date_string).transpose()?,
            created_at: date_string(self.created_at)?,
        })
    }
}

async fn preflight_illustration_request_limits(
    store: &MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
    origin_turn_id: Option<&str>,
) -> Result<Result<(), GenerationJobStoreError>, PersistenceError> {
    let Some(origin_turn_id) = origin_turn_id else {
        return Ok(Err(GenerationJobStoreError::InvalidInput(
            "illustration jobs require an origin turn",
        )));
    };
    let receipts = store.document_collection(CollectionName::CommandReceipts);
    let base = doc! {
        "scope_kind": enqueue_scope_kind(GenerationPurpose::Illustration),
        "scope_id": campaign_id,
        "command_kind": "enqueue_generation_job",
        "state": "committed",
    };
    let lifetime = receipts
        .count_documents(base.clone())
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("count lifetime illustration requests", error))?;
    let mut rolling_filter = base.clone();
    rolling_filter.insert(
        "created_at",
        doc! { "$gt": DateTime::from_millis(
            DateTime::now().timestamp_millis().saturating_sub(24 * 60 * 60 * 1_000)
        ) },
    );
    let rolling = receipts
        .count_documents(rolling_filter)
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("count rolling illustration requests", error))?;
    let mut turn_filter = base;
    turn_filter.insert("origin_event_id", origin_turn_id);
    let turn = receipts
        .count_documents(turn_filter)
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("count turn illustration requests", error))?;
    if lifetime >= IMAGE_REQUESTS_PER_CAMPAIGN_LIFETIME || rolling >= IMAGE_REQUESTS_PER_ROLLING_DAY
    {
        return Ok(Err(GenerationJobStoreError::BudgetExceeded {
            scope: GenerationBudgetScope::Campaign,
            dimension: GenerationBudgetDimension::Requests,
        }));
    }
    if turn >= IMAGE_REQUESTS_PER_TURN {
        return Ok(Err(GenerationJobStoreError::BudgetExceeded {
            scope: GenerationBudgetScope::Turn,
            dimension: GenerationBudgetDimension::Requests,
        }));
    }
    Ok(Ok(()))
}

async fn insert_job_audit(
    store: &MongoStore,
    session: &mut ClientSession,
    job: &GenerationJobDocument,
    action: &str,
    outcome: &str,
) -> Result<(), PersistenceError> {
    store
        .document_collection(CollectionName::AuditEvents)
        .insert_one(doc! {
            "_id": format!("audit:{}", Uuid::new_v4()),
            "schema_version": SCHEMA_VERSION,
            "category": "generation",
            "action": action,
            "outcome": outcome,
            "scope_kind": "generation_job",
            "scope_id": &job.id,
            "correlation_id": job.correlation_id.clone()
                .unwrap_or_else(|| format!("correlation:{}", Uuid::new_v4())),
            "metadata": {
                "campaign_id": &job.campaign_id,
                "purpose": &job.purpose,
                "attempt_count": job.attempt_count,
            },
            "created_at": DateTime::now(),
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("insert generation audit", error))?;
    Ok(())
}

fn validate_new_job(job: &NewGenerationJob) -> Result<(), GenerationJobStoreError> {
    validate_identifier(&job.id, "job id is invalid")?;
    validate_identifier(&job.campaign_session_id, "campaign id is invalid")?;
    validate_identifier(&job.idempotency_key, "idempotency key is invalid")?;
    if let Some(turn_id) = job.origin_turn_id.as_deref() {
        validate_identifier(turn_id, "origin turn id is invalid")?;
    }
    if job.purpose.requires_origin_turn() && job.origin_turn_id.is_none() {
        return Err(GenerationJobStoreError::InvalidInput(
            "narration and illustration jobs require an origin turn",
        ));
    }
    if job.purpose == GenerationPurpose::Illustration && job.governance.is_none() {
        return Err(GenerationJobStoreError::InvalidInput(
            "illustration jobs require durable budget governance",
        ));
    }
    if job.origin_campaign_revision == 0 {
        return Err(GenerationJobStoreError::InvalidInput(
            "origin campaign revision must be positive",
        ));
    }
    if !(1..=MAX_ATTEMPTS).contains(&job.max_attempts) {
        return Err(GenerationJobStoreError::InvalidInput(
            "max attempts must be between one and five",
        ));
    }
    if let Some(correlation_id) = job.correlation_id.as_deref() {
        validate_identifier(correlation_id, "correlation id is invalid")?;
    }
    Ok(())
}

fn validate_claim(claim: &GenerationClaim) -> Result<(), GenerationJobStoreError> {
    validate_identifier(&claim.worker_id, "worker id is invalid")?;
    validate_identifier(&claim.provider, "provider id is invalid")?;
    if claim.model.trim() != claim.model
        || claim.model.is_empty()
        || claim.model.len() > 256
        || claim.model.chars().any(char::is_control)
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "model name must be bounded and control-free",
        ));
    }
    validate_lease_duration(claim.lease_duration)
}

pub(super) fn validate_lease(lease: &GenerationLease) -> Result<(), GenerationJobStoreError> {
    validate_identifier(&lease.job_id, "lease job id is invalid")?;
    validate_identifier(&lease.attempt_id, "lease attempt id is invalid")?;
    validate_identifier(&lease.worker_id, "lease worker id is invalid")?;
    validate_identifier(&lease.lease_token, "lease token is invalid")
}

fn validate_lease_duration(duration: Duration) -> Result<(), GenerationJobStoreError> {
    if !(MIN_LEASE..=MAX_LEASE).contains(&duration) {
        return Err(GenerationJobStoreError::InvalidInput(
            "lease duration must be between one second and five minutes",
        ));
    }
    Ok(())
}

fn validate_failure(failure: &GenerationAttemptFailure) -> Result<(), GenerationJobStoreError> {
    validate_usage(&failure.usage)?;
    if failure
        .provider_status
        .is_some_and(|status| !(100..=599).contains(&status))
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "provider status is invalid",
        ));
    }
    if let Some(request_id) = failure.provider_request_id.as_deref() {
        validate_identifier(request_id, "provider request id is invalid")?;
    }
    Ok(())
}

fn validate_usage(usage: &GenerationUsage) -> Result<(), GenerationJobStoreError> {
    for value in [
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.total_tokens,
        usage.cost_microusd,
        usage.latency_milliseconds,
    ]
    .into_iter()
    .flatten()
    {
        i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)?;
    }
    if let (Some(prompt), Some(completion), Some(total)) = (
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.total_tokens,
    ) {
        let minimum = prompt
            .checked_add(completion)
            .ok_or(GenerationJobStoreError::NumericRange)?;
        if total < minimum {
            return Err(GenerationJobStoreError::InvalidInput(
                "total tokens cannot be less than prompt plus completion tokens",
            ));
        }
    }
    Ok(())
}

fn ensure_matching_replay(
    existing: &GenerationJob,
    requested: &NewGenerationJob,
) -> Result<(), GenerationJobStoreError> {
    if existing.campaign_session_id != requested.campaign_session_id
        || existing.origin_turn_id != requested.origin_turn_id
        || existing.origin_campaign_revision != requested.origin_campaign_revision
        || existing.purpose != requested.purpose
        || existing.idempotency_key != requested.idempotency_key
        || existing.input_digest != requested.input_digest
        || existing.prompt_digest != requested.prompt_digest
        || existing.policy_digest != requested.policy_digest
        || existing.config_digest != requested.config_digest
        || existing.max_attempts != requested.max_attempts
        || existing.success_retention != requested.success_retention
    {
        return Err(GenerationJobStoreError::IdempotencyConflict);
    }
    Ok(())
}

fn enqueue_fingerprint(job: &NewGenerationJob) -> Sha256Digest {
    let governance = job.governance.as_ref();
    let revision = job.origin_campaign_revision.to_string();
    let max_attempts = job.max_attempts.to_string();
    let reserved_requests = governance
        .map(|value| value.reserved_requests)
        .unwrap_or_default()
        .to_string();
    let reserved_tokens = governance
        .map(|value| value.reserved_tokens)
        .unwrap_or_default()
        .to_string();
    let reserved_latency = governance
        .map(|value| value.reserved_latency_milliseconds)
        .unwrap_or_default()
        .to_string();
    let reserved_cost = governance
        .map(|value| value.reserved_cost_microusd)
        .unwrap_or_default()
        .to_string();
    let fields = [
        job.campaign_session_id.as_str(),
        job.origin_turn_id.as_deref().unwrap_or(""),
        revision.as_str(),
        job.purpose.as_str(),
        job.idempotency_key.as_str(),
        job.input_digest.as_str(),
        job.prompt_digest.as_str(),
        job.policy_digest.as_str(),
        job.config_digest.as_str(),
        max_attempts.as_str(),
        job.success_retention.as_str(),
        governance
            .map(|value| value.governance_fingerprint.as_str())
            .unwrap_or(""),
        reserved_requests.as_str(),
        reserved_tokens.as_str(),
        reserved_latency.as_str(),
        reserved_cost.as_str(),
    ];
    let mut hasher = Sha256::new();
    let domain = b"generation-enqueue/v1";
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    Sha256Digest::from_bytes(hasher.finalize().into())
}

fn opaque_digest(domain: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn enqueue_scope_kind(purpose: GenerationPurpose) -> &'static str {
    match purpose {
        GenerationPurpose::IntentParsing => "generation_intent_parsing",
        GenerationPurpose::GmPlanning => "generation_gm_planning",
        GenerationPurpose::Narration => "generation_narration",
        GenerationPurpose::Illustration => "generation_illustration",
    }
}

pub(super) struct UsageBindings {
    pub(super) prompt_tokens: Option<i64>,
    pub(super) completion_tokens: Option<i64>,
    pub(super) total_tokens: Option<i64>,
    pub(super) cost_microusd: Option<i64>,
    pub(super) latency_milliseconds: Option<i64>,
}

pub(super) fn usage_bindings(
    usage: &GenerationUsage,
) -> Result<UsageBindings, GenerationJobStoreError> {
    validate_usage(usage)?;
    Ok(UsageBindings {
        prompt_tokens: usage.prompt_tokens.map(to_i64).transpose()?,
        completion_tokens: usage.completion_tokens.map(to_i64).transpose()?,
        total_tokens: usage.total_tokens.map(to_i64).transpose()?,
        cost_microusd: usage.cost_microusd.map(to_i64).transpose()?,
        latency_milliseconds: usage.latency_milliseconds.map(to_i64).transpose()?,
    })
}

fn validate_loaded_job(job: &GenerationJob) -> Result<(), GenerationJobStoreError> {
    validate_identifier(&job.id, "stored job id is invalid")?;
    validate_identifier(&job.campaign_session_id, "stored campaign id is invalid")?;
    validate_identifier(&job.idempotency_key, "stored idempotency key is invalid")?;
    if job.origin_campaign_revision == 0
        || !(1..=MAX_ATTEMPTS).contains(&job.max_attempts)
        || job.attempt_count > job.max_attempts
        || job.created_at.is_empty()
        || job.updated_at.is_empty()
    {
        return Err(GenerationJobStoreError::InvalidStoredData(
            "stored job bounds are invalid",
        ));
    }
    if job.purpose.requires_origin_turn() && job.origin_turn_id.is_none() {
        return Err(GenerationJobStoreError::InvalidStoredData(
            "stored job is missing its required origin turn",
        ));
    }
    Ok(())
}

fn generation_jobs(store: &MongoStore) -> Collection<GenerationJobDocument> {
    store.collection(CollectionName::GenerationJobs)
}

fn enqueue_receipts(store: &MongoStore) -> Collection<EnqueueReceiptDocument> {
    store.collection(CollectionName::CommandReceipts)
}

async fn operation<T>(
    store: &MongoStore,
    operation: &'static str,
    future: impl IntoFuture<Output = mongodb::error::Result<T>>,
) -> Result<T, GenerationJobStoreError> {
    tokio::time::timeout(store.operation_timeout(), future.into_future())
        .await
        .map_err(|_| {
            GenerationJobStoreError::Database(PersistenceError::OperationTimeout { operation })
        })?
        .map_err(|error| {
            GenerationJobStoreError::Database(PersistenceError::mongo(operation, error))
        })
}

fn map_database(error: PersistenceError) -> GenerationJobStoreError {
    if error.mongo_failure_kind() == Some(MongoFailureKind::DuplicateKey) {
        GenerationJobStoreError::IdempotencyConflict
    } else {
        GenerationJobStoreError::Database(error)
    }
}

fn stored_digest(value: &str) -> Result<Sha256Digest, GenerationJobStoreError> {
    Sha256Digest::new(value.to_owned())
        .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid stored digest"))
}

fn validate_identifier(value: &str, reason: &'static str) -> Result<(), GenerationJobStoreError> {
    if !is_valid_opaque_id(value) {
        return Err(GenerationJobStoreError::InvalidInput(reason));
    }
    Ok(())
}

fn to_i64(value: u64) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn from_i64(value: i64) -> Result<u64, GenerationJobStoreError> {
    u64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn optional_i64(value: Option<i64>) -> Result<Option<u64>, GenerationJobStoreError> {
    value.map(from_i64).transpose()
}

fn date_string(value: DateTime) -> Result<String, GenerationJobStoreError> {
    value.try_to_rfc3339_string().map_err(|_| {
        GenerationJobStoreError::InvalidStoredData("stored BSON date is outside RFC 3339 range")
    })
}

fn add_duration(value: DateTime, duration: Duration) -> DateTime {
    DateTime::from_millis(
        value
            .timestamp_millis()
            .saturating_add(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{
            GenerationBudgetAllowance, GenerationGovernanceConfig, MongoConfig, MongoSchemaPolicy,
            SecretString,
        },
        persistence::SchemaReconciler,
        repository::MongoRepository,
    };

    #[test]
    fn transient_classification_is_closed() {
        assert!(GenerationFailureCode::Timeout.retryable());
        assert!(GenerationFailureCode::ProviderUnavailable.retryable());
        assert!(GenerationFailureCode::RateLimited.retryable());
        assert!(GenerationFailureCode::LeaseExpired.retryable());
        for permanent in [
            GenerationFailureCode::ProviderRejected,
            GenerationFailureCode::MalformedResponse,
            GenerationFailureCode::UnsafeOutput,
            GenerationFailureCode::Contradiction,
            GenerationFailureCode::InvalidArtifact,
            GenerationFailureCode::BudgetExceeded,
            GenerationFailureCode::Cancelled,
        ] {
            assert!(!permanent.retryable());
        }
    }

    #[test]
    fn enqueue_fingerprint_ignores_server_row_id_and_correlation_only() {
        let mut first = NewGenerationJob {
            id: "generation-job:first".to_owned(),
            campaign_session_id: "campaign:test".to_owned(),
            origin_turn_id: None,
            origin_campaign_revision: 1,
            purpose: GenerationPurpose::IntentParsing,
            idempotency_key: "generation-key:test".to_owned(),
            input_digest: Sha256Digest::from_bytes([1; 32]),
            prompt_digest: Sha256Digest::from_bytes([2; 32]),
            policy_digest: Sha256Digest::from_bytes([3; 32]),
            config_digest: Sha256Digest::from_bytes([4; 32]),
            correlation_id: Some("correlation:first".to_owned()),
            max_attempts: 1,
            success_retention: SuccessRetention::UnselectedPresentation30Days,
            governance: None,
        };
        let original = enqueue_fingerprint(&first);
        first.id = "generation-job:retry".to_owned();
        first.correlation_id = Some("correlation:retry".to_owned());
        assert_eq!(enqueue_fingerprint(&first), original);
        first.input_digest = Sha256Digest::from_bytes([9; 32]);
        assert_ne!(enqueue_fingerprint(&first), original);
    }

    #[test]
    fn lease_storage_is_one_way() {
        let raw = "generation-lease:secret";
        let stored = opaque_digest("generation-lease-token/v1", raw);
        assert!(stored.starts_with("sha256:"));
        assert!(!stored.contains(raw));
    }

    #[tokio::test]
    async fn live_mongo_enqueue_lease_budget_and_retention_contract() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        let campaign_id = "campaign:generation-contract";
        let budget_campaign_id = "campaign:budget-contract";
        let turn_id = "turn:generation-contract";
        seed_campaign(&store, campaign_id, 10).await;
        seed_campaign(&store, budget_campaign_id, 10).await;
        seed_turn(&store, campaign_id, turn_id, 0).await;

        let request = live_job(
            "generation-job:lease-contract",
            campaign_id,
            Some(turn_id),
            1,
            GenerationPurpose::Narration,
            "command:lease-contract",
            None,
        );
        assert!(matches!(
            repository.enqueue_generation_job(&request).await.unwrap(),
            EnqueueGenerationJobOutcome::Enqueued(_)
        ));
        assert!(matches!(
            repository.enqueue_generation_job(&request).await.unwrap(),
            EnqueueGenerationJobOutcome::Existing(_)
        ));
        let mut drift = request.clone();
        drift.input_digest = test_digest(99);
        assert!(matches!(
            repository.enqueue_generation_job(&drift).await,
            Err(GenerationJobStoreError::IdempotencyConflict)
        ));

        let first_repository = repository.clone();
        let second_repository = repository.clone();
        let first_claim = live_claim("worker:first");
        let second_claim = live_claim("worker:second");
        let (first, second) = tokio::join!(
            first_repository.claim_generation_job_by_id(campaign_id, &request.id, &first_claim),
            second_repository.claim_generation_job_by_id(campaign_id, &request.id, &second_claim),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        let expired = first.or(second).unwrap();
        store
            .document_collection(CollectionName::GenerationJobs)
            .update_one(
                doc! { "_id": &request.id },
                doc! {
                    "$set": {
                        "lease_expires_at": DateTime::from_millis(
                            DateTime::now().timestamp_millis().saturating_sub(1_000),
                        ),
                    },
                },
            )
            .await
            .unwrap();
        let reclaimed = repository
            .claim_generation_job_by_id(campaign_id, &request.id, &live_claim("worker:reclaimer"))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(expired.lease.attempt_id, reclaimed.lease.attempt_id);
        assert_eq!(reclaimed.attempt.attempt_number, 2);
        assert!(matches!(
            repository
                .heartbeat_generation_job(&expired.lease, Duration::from_secs(30))
                .await,
            Err(GenerationJobStoreError::LostLease)
        ));
        repository
            .succeed_generation_job(
                &reclaimed.lease,
                &GenerationSuccess {
                    artifact_id: None,
                    output_digest: test_digest(40),
                    usage: GenerationUsage::default(),
                },
            )
            .await
            .unwrap();
        store
            .document_collection(CollectionName::GenerationJobs)
            .update_one(
                doc! { "_id": &request.id },
                doc! {
                    "$set": {
                        "purge_at": DateTime::from_millis(
                            DateTime::now().timestamp_millis().saturating_sub(1),
                        ),
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            repository
                .cleanup_generation_metadata(10)
                .await
                .unwrap()
                .operational_jobs_deleted,
            1
        );
        assert!(matches!(
            repository.enqueue_generation_job(&request).await,
            Err(GenerationJobStoreError::IdempotencyReceiptClosed)
        ));

        let governance = live_governance("turn-scope:budget-contract");
        let first_budget = live_job(
            "generation-job:budget-one",
            budget_campaign_id,
            None,
            10,
            GenerationPurpose::IntentParsing,
            "command:budget-one",
            Some(governance.clone()),
        );
        let mut second_budget = first_budget.clone();
        second_budget.id = "generation-job:budget-two".to_owned();
        second_budget.idempotency_key = "command:budget-two".to_owned();
        second_budget.input_digest = test_digest(41);
        let first_repository = repository.clone();
        let second_repository = repository.clone();
        let (first, second) = tokio::join!(
            first_repository.enqueue_generation_job(&first_budget),
            second_repository.enqueue_generation_job(&second_budget),
        );
        let outcomes = [first, second];
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Ok(EnqueueGenerationJobOutcome::Enqueued(_))))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(GenerationJobStoreError::BudgetExceeded {
                        scope: GenerationBudgetScope::Campaign,
                        dimension: GenerationBudgetDimension::Requests,
                    })
                ))
                .count(),
            1
        );
        let accepted = outcomes
            .into_iter()
            .find_map(Result::ok)
            .unwrap()
            .job()
            .clone();
        let claimed = repository
            .claim_generation_job_by_id(
                budget_campaign_id,
                &accepted.id,
                &live_claim("worker:budget"),
            )
            .await
            .unwrap()
            .unwrap();
        repository
            .succeed_generation_job(
                &claimed.lease,
                &GenerationSuccess {
                    artifact_id: None,
                    output_digest: test_digest(42),
                    usage: GenerationUsage {
                        prompt_tokens: Some(50),
                        completion_tokens: Some(100),
                        total_tokens: Some(150),
                        cost_microusd: Some(25),
                        latency_milliseconds: Some(300),
                    },
                },
            )
            .await
            .unwrap();
        let budget = repository
            .generation_budget_status(budget_campaign_id, &governance.limits)
            .await
            .unwrap();
        assert_eq!(budget.campaign_tokens.used, 150);
        let overage = store
            .document_collection(CollectionName::GenerationBudgetReservations)
            .count_documents(doc! {
                "job_id": &accepted.id,
                "dimension": "tokens",
                "overage": true,
            })
            .await
            .unwrap();
        assert_eq!(overage, 1);

        assert!(
            database.starts_with("mdnd_generation_test_") && database != "manchester_dnd",
            "cleanup safeguard"
        );
        store.database().drop().await.unwrap();
    }

    async fn isolated_mongo_repository() -> Option<(MongoRepository, MongoStore, String)> {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping generation MongoDB contract: MONGODB_TEST_URI is not set");
            return None;
        };
        assert!(
            !uri.trim().is_empty(),
            "MONGODB_TEST_URI must not be empty when set"
        );
        let database = format!("mdnd_generation_test_{}", Uuid::new_v4().simple());
        let store = MongoStore::connect(&MongoConfig {
            uri: SecretString::new(uri),
            database: database.clone(),
            max_pool_size: 8,
            min_pool_size: 0,
            connect_timeout: Duration::from_secs(5),
            server_selection_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(15),
            transaction_timeout: Duration::from_secs(10),
            transaction_max_retries: 4,
            schema_policy: MongoSchemaPolicy::ApplyAndVerify,
        })
        .await
        .unwrap();
        SchemaReconciler::new(store.clone()).apply().await.unwrap();
        Some((MongoRepository::new(store.clone()), store, database))
    }

    async fn seed_campaign(store: &MongoStore, id: &str, revision: i64) {
        let now = DateTime::now();
        store
            .document_collection(CollectionName::Campaigns)
            .insert_one(doc! {
                "_id": id,
                "schema_version": 1_i32,
                "owner_account_id": format!("account:owner-{id}"),
                "revision": revision,
                "title_normalized": format!("test-{id}"),
                "members": [],
                "rules_snapshot": {},
                "created_at": now,
                "updated_at": now,
            })
            .await
            .unwrap();
    }

    async fn seed_turn(store: &MongoStore, campaign_id: &str, turn_id: &str, sequence: i64) {
        store
            .document_collection(CollectionName::TurnEvents)
            .insert_one(doc! {
                "_id": turn_id,
                "schema_version": 1_i32,
                "campaign_id": campaign_id,
                "play_session_id": format!("play-session:{campaign_id}"),
                "sequence": sequence,
                "correlation_id": format!("correlation:{turn_id}"),
                "created_at": DateTime::now(),
            })
            .await
            .unwrap();
    }

    fn live_job(
        id: &str,
        campaign_id: &str,
        turn_id: Option<&str>,
        revision: u64,
        purpose: GenerationPurpose,
        key: &str,
        governance: Option<NewGenerationGovernanceReceipt>,
    ) -> NewGenerationJob {
        NewGenerationJob {
            id: id.to_owned(),
            campaign_session_id: campaign_id.to_owned(),
            origin_turn_id: turn_id.map(str::to_owned),
            origin_campaign_revision: revision,
            purpose,
            idempotency_key: key.to_owned(),
            input_digest: test_digest(31),
            prompt_digest: test_digest(32),
            policy_digest: test_digest(33),
            config_digest: test_digest(34),
            correlation_id: Some(format!("correlation:{id}")),
            max_attempts: 2,
            success_retention: SuccessRetention::UnselectedPresentation30Days,
            governance,
        }
    }

    fn live_claim(worker_id: &str) -> GenerationClaim {
        GenerationClaim {
            worker_id: worker_id.to_owned(),
            provider: "provider:test".to_owned(),
            model: "deterministic-test".to_owned(),
            lease_duration: Duration::from_secs(30),
        }
    }

    fn live_governance(turn_scope_key: &str) -> NewGenerationGovernanceReceipt {
        let limits = GenerationGovernanceConfig {
            campaign: GenerationBudgetAllowance {
                requests: 1,
                tokens: 200,
                latency_milliseconds: 1_000,
                cost_microusd: 100,
            },
            turn: GenerationBudgetAllowance {
                requests: 1,
                tokens: 200,
                latency_milliseconds: 1_000,
                cost_microusd: 100,
            },
            max_campaign_concurrency: 2,
            worker_batch_size: 2,
        };
        NewGenerationGovernanceReceipt {
            turn_scope_key: turn_scope_key.to_owned(),
            request_fingerprint: test_digest(50),
            policy_fingerprint: test_digest(51),
            config_fingerprint: test_digest(52),
            governance_fingerprint: limits.non_secret_fingerprint(),
            reserved_requests: 1,
            reserved_tokens: 10,
            reserved_latency_milliseconds: 100,
            reserved_cost_microusd: 10,
            limits,
        }
    }

    fn test_digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; 32])
    }
}
