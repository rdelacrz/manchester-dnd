//! Durable, metadata-only generation queue storage.
//!
//! The queue never accepts or persists prompt bodies, minimized input bodies,
//! provider response bodies, or credentials. Callers supply canonical digests
//! plus bounded operational metadata. The private-MVP defaults are deliberately
//! conservative: at most five attempts, leases between one second and five
//! minutes, failed metadata retained seven days, and unselected successful
//! presentation artifacts retained thirty days.

use std::{str::FromStr, time::Duration};

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use thiserror::Error;
use uuid::Uuid;

use super::{
    PostgresRepository,
    governance::{
        GenerationBudgetDimension, GenerationBudgetScope, NewGenerationGovernanceReceipt,
        ensure_matching_governance_receipt, insert_generation_governance_receipt,
        load_governance_receipt_by_key, preflight_generation_governance,
        record_generation_attempt_usage, release_generation_budget,
        settle_unknown_generation_usage,
    },
};

const MAX_ATTEMPTS: u8 = 5;
const MIN_LEASE: Duration = Duration::from_secs(1);
const MAX_LEASE: Duration = Duration::from_secs(5 * 60);
const FAILED_RETENTION_SQL: &str = "INTERVAL '7 days'";
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
    #[error("generation numeric value is outside PostgreSQL BIGINT's supported range")]
    NumericRange,
    #[error("generation database operation failed")]
    Database(#[source] sqlx::Error),
}

impl GenerationJobStoreError {
    /// Classification only: any retry must preserve the original job metadata
    /// and idempotency key. Connection failures and unknown SQLSTATEs fail
    /// closed; PostgreSQL documents `40001` and `40P01` as transaction-level
    /// serialization/deadlock failures that may be retried as a whole.
    pub fn retryable_database_transaction(&self) -> bool {
        let Self::Database(error) = self else {
            return false;
        };
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .is_some_and(|code| transient_postgres_sqlstate(code.as_ref()))
    }
}

fn transient_postgres_sqlstate(code: &str) -> bool {
    matches!(code, "40001" | "40P01")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    const fn as_str(self) -> &'static str {
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
    /// An owner-selected artifact follows the campaign lifetime.
    CampaignLifetime,
    /// Q13's default for an unselected presentation version.
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

/// Stable, body-free failure codes. Only transport availability failures are
/// automatically retryable; safety, fidelity, budget, and provider rejections
/// fail closed.
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
    /// Estimated or provider-reported cost in millionths of one US dollar.
    pub cost_microusd: Option<u64>,
    /// End-to-end provider-attempt latency. Queue time is excluded.
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
    /// Illustration jobs require a campaign-owned validated artifact. Textual
    /// jobs may complete metadata-only after their typed result is committed
    /// elsewhere; no placeholder asset location is manufactured.
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
    /// Present when a validated, safe authored fallback was produced. Transport
    /// failures that yielded no presentation leave this absent.
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

impl PostgresRepository {
    /// Inserts one durable job or returns the original row for a matching
    /// idempotent replay. A replay is checked before the campaign's current
    /// revision so a later retry can still recover the original job ID.
    pub async fn enqueue_generation_job(
        &self,
        new_job: &NewGenerationJob,
    ) -> Result<EnqueueGenerationJobOutcome, GenerationJobStoreError> {
        validate_new_job(new_job)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;

        // The campaign row is the cross-process serialization point for both
        // exact-key replay and aggregate budget/concurrency reservation.
        let current_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM campaign_sessions WHERE id = $1 FOR UPDATE")
                .bind(&new_job.campaign_session_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?;
        let Some(current_revision) = current_revision else {
            return Err(GenerationJobStoreError::NotFound {
                job_id: new_job.id.clone(),
            });
        };
        let current_revision = from_i64(current_revision)?;
        if let Some(existing) = exact_enqueue_replay(&mut transaction, new_job).await? {
            transaction
                .commit()
                .await
                .map_err(GenerationJobStoreError::Database)?;
            return Ok(EnqueueGenerationJobOutcome::Existing(existing));
        }
        if new_job.purpose == GenerationPurpose::Illustration {
            preflight_illustration_request_limits(
                &mut transaction,
                &new_job.campaign_session_id,
                new_job.origin_turn_id.as_deref(),
            )
            .await?;
        }
        if let Some(turn_id) = new_job.origin_turn_id.as_deref() {
            let origin_turn_number: Option<i64> = sqlx::query_scalar(
                "SELECT turn_number FROM turn_audits
                 WHERE id = $1 AND campaign_session_id = $2",
            )
            .bind(turn_id)
            .bind(&new_job.campaign_session_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?;
            let Some(origin_turn_number) = origin_turn_number else {
                return Err(GenerationJobStoreError::InvalidInput(
                    "origin turn does not belong to the campaign",
                ));
            };
            let origin_turn_number = from_i64(origin_turn_number)?;
            let committed_revision = origin_turn_number
                .checked_add(1)
                .ok_or(GenerationJobStoreError::NumericRange)?;
            if committed_revision != new_job.origin_campaign_revision
                || current_revision < new_job.origin_campaign_revision
            {
                return Err(GenerationJobStoreError::OriginRevisionConflict {
                    expected: new_job.origin_campaign_revision,
                    actual: current_revision,
                });
            }
        } else if current_revision != new_job.origin_campaign_revision {
            return Err(GenerationJobStoreError::OriginRevisionConflict {
                expected: new_job.origin_campaign_revision,
                actual: current_revision,
            });
        }

        if let Some(governance) = new_job.governance.as_ref()
            && let Err(error) = preflight_generation_governance(
                &mut transaction,
                &new_job.campaign_session_id,
                new_job.purpose,
                governance,
            )
            .await
        {
            if matches!(error, GenerationJobStoreError::BudgetExceeded { .. }) {
                transaction
                    .commit()
                    .await
                    .map_err(GenerationJobStoreError::Database)?;
            }
            return Err(error);
        }

        let sql = format!(
            "INSERT INTO generation_jobs
             (id, campaign_session_id, origin_turn_id, origin_campaign_revision,
              purpose, idempotency_key, state, input_digest, prompt_digest,
              policy_digest, config_digest, correlation_id, attempt_count,
              max_attempts, retry_at, success_retention_class, retention_class)
             VALUES
             ($1, $2, $3, $4, $5, $6, 'queued', $7, $8, $9, $10, $11,
              0, $12, CURRENT_TIMESTAMP, $13, 'pending')
             ON CONFLICT (campaign_session_id, purpose, idempotency_key) DO NOTHING
             RETURNING {JOB_COLUMNS}"
        );
        let inserted = sqlx::query(&sql)
            .bind(&new_job.id)
            .bind(&new_job.campaign_session_id)
            .bind(new_job.origin_turn_id.as_deref())
            .bind(to_i64(new_job.origin_campaign_revision)?)
            .bind(new_job.purpose.as_str())
            .bind(&new_job.idempotency_key)
            .bind(new_job.input_digest.as_str())
            .bind(new_job.prompt_digest.as_str())
            .bind(new_job.policy_digest.as_str())
            .bind(new_job.config_digest.as_str())
            .bind(new_job.correlation_id.as_deref())
            .bind(i16::from(new_job.max_attempts))
            .bind(new_job.success_retention.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_enqueue_error)?
            .map(job_from_row)
            .transpose()?;

        let outcome = if let Some(inserted) = inserted {
            if let Some(governance) = new_job.governance.as_ref() {
                insert_generation_governance_receipt(
                    &mut transaction,
                    &inserted.id,
                    &inserted.campaign_session_id,
                    inserted.origin_turn_id.as_deref(),
                    inserted.purpose,
                    &inserted.idempotency_key,
                    governance,
                )
                .await?;
            }
            EnqueueGenerationJobOutcome::Enqueued(inserted)
        } else {
            let existing = load_job_by_key(
                &mut transaction,
                &new_job.campaign_session_id,
                new_job.purpose,
                &new_job.idempotency_key,
            )
            .await?
            .ok_or(GenerationJobStoreError::InvalidStoredData(
                "idempotency conflict did not resolve to a job",
            ))?;
            ensure_matching_replay(&existing, new_job)?;
            ensure_job_governance_replay(&mut transaction, &existing, new_job).await?;
            EnqueueGenerationJobOutcome::Existing(existing)
        };
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(outcome)
    }

    pub async fn load_generation_job(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<Option<GenerationJob>, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM generation_jobs
             WHERE id = $1 AND campaign_session_id = $2"
        );
        sqlx::query(&sql)
            .bind(job_id)
            .bind(campaign_session_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(GenerationJobStoreError::Database)?
            .map(job_from_row)
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
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM generation_jobs
             WHERE campaign_session_id = $1 AND purpose = $2 AND idempotency_key = $3"
        );
        sqlx::query(&sql)
            .bind(campaign_session_id)
            .bind(purpose.as_str())
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(GenerationJobStoreError::Database)?
            .map(job_from_row)
            .transpose()
    }

    pub async fn list_generation_attempts(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<Vec<GenerationAttempt>, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        let rows = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS}
             FROM generation_attempts AS generation_attempts
             JOIN generation_jobs ON generation_jobs.id = generation_attempts.job_id
             WHERE generation_attempts.job_id = $1
               AND generation_jobs.campaign_session_id = $2
             ORDER BY generation_attempts.attempt_number"
        ))
        .bind(job_id)
        .bind(campaign_session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        rows.into_iter().map(attempt_from_row).collect()
    }

    /// Claims one ready job. An expired lease is closed as a body-free
    /// `lease_expired` attempt before a new attempt is created in the same
    /// transaction. Concurrent workers skip the locked row.
    pub async fn claim_generation_job(
        &self,
        claim: &GenerationClaim,
    ) -> Result<Option<ClaimedGenerationJob>, GenerationJobStoreError> {
        self.claim_generation_job_matching(claim, None, None).await
    }

    /// Claims only one purpose. A dedicated image worker therefore cannot
    /// consume text work even when both modalities share the durable queue.
    pub async fn claim_generation_job_for_purpose(
        &self,
        purpose: GenerationPurpose,
        claim: &GenerationClaim,
    ) -> Result<Option<ClaimedGenerationJob>, GenerationJobStoreError> {
        self.claim_generation_job_matching(claim, None, Some(purpose))
            .await
    }

    /// Claims one exact campaign-owned job for an inline bounded worker.
    ///
    /// This prevents a request-scoped worker from accidentally leasing an
    /// unrelated queued job while preserving the same lease/reclaim rules as
    /// the general background-worker claim path.
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
        let lease_millis = duration_millis(claim.lease_duration)?;
        let attempt_id = format!("generation-attempt:{}", Uuid::new_v4());
        let lease_token = format!("generation-lease:{}", Uuid::new_v4());
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;

        let (campaign_filter, job_filter) = exact_job
            .map(|(campaign_session_id, job_id)| (Some(campaign_session_id), Some(job_id)))
            .unwrap_or((None, None));
        let candidate_sql = format!(
            "SELECT {JOB_COLUMNS} FROM generation_jobs AS candidate
             WHERE ($1::TEXT IS NULL OR (candidate.campaign_session_id = $1 AND candidate.id = $2))
               AND ($3::TEXT IS NULL OR candidate.purpose = $3)
               AND ((state = 'queued' AND retry_at <= CURRENT_TIMESTAMP)
                 OR (state = 'running' AND lease_expires_at <= CURRENT_TIMESTAMP))
               AND (
                    candidate.purpose <> 'illustration'
                    OR candidate.state = 'running'
                    OR NOT EXISTS (
                        SELECT 1 FROM generation_jobs AS active_image
                        WHERE active_image.campaign_session_id = candidate.campaign_session_id
                          AND active_image.purpose = 'illustration'
                          AND active_image.state = 'running'
                    )
               )
             ORDER BY CASE WHEN state = 'queued' THEN 0 ELSE 1 END,
                      COALESCE(retry_at, lease_expires_at), created_at, id
             FOR UPDATE SKIP LOCKED
             LIMIT 1"
        );
        let Some(candidate) = sqlx::query(&candidate_sql)
            .bind(campaign_filter)
            .bind(job_filter)
            .bind(purpose_filter.map(GenerationPurpose::as_str))
            .fetch_optional(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?
            .map(job_from_row)
            .transpose()?
        else {
            transaction
                .commit()
                .await
                .map_err(GenerationJobStoreError::Database)?;
            return Ok(None);
        };

        if candidate.state == GenerationJobState::Running {
            let closed = sqlx::query(
                "UPDATE generation_attempts
                 SET state = 'failed', failure_class = 'transient',
                     failure_code = 'lease_expired', heartbeat_at = CURRENT_TIMESTAMP,
                     finished_at = CURRENT_TIMESTAMP
                 WHERE job_id = $1 AND lease_token = $2 AND state = 'running'",
            )
            .bind(&candidate.id)
            .bind(candidate.lease_token.as_deref())
            .execute(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?;
            if closed.rows_affected() != 1 {
                return Err(GenerationJobStoreError::InvalidStoredData(
                    "expired job has no matching running attempt",
                ));
            }
            record_generation_attempt_usage(
                &mut transaction,
                &candidate.id,
                candidate.purpose,
                &GenerationUsage::default(),
                candidate.attempt_count >= candidate.max_attempts,
            )
            .await?;
            if candidate.attempt_count >= candidate.max_attempts {
                sqlx::query(&format!(
                    "UPDATE generation_jobs
                     SET state = 'failed', retry_at = NULL, lease_owner = NULL,
                         lease_token = NULL, lease_expires_at = NULL,
                         last_failure_class = 'transient',
                         last_failure_code = 'lease_expired',
                         retention_class = 'failed_metadata_7d',
                         retention_delete_after = CURRENT_TIMESTAMP + {FAILED_RETENTION_SQL},
                         completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                     WHERE id = $1"
                ))
                .bind(&candidate.id)
                .execute(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?;
                transaction
                    .commit()
                    .await
                    .map_err(GenerationJobStoreError::Database)?;
                return Ok(None);
            }
        }

        let next_attempt = candidate
            .attempt_count
            .checked_add(1)
            .ok_or(GenerationJobStoreError::NumericRange)?;
        let update_sql = format!(
            "UPDATE generation_jobs
             SET state = 'running', attempt_count = $2, retry_at = NULL,
                 lease_owner = $3, lease_token = $4,
                 lease_expires_at = CURRENT_TIMESTAMP + ($5 * INTERVAL '1 millisecond'),
                 last_failure_class = NULL, last_failure_code = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $1
             RETURNING {JOB_COLUMNS}"
        );
        let job = job_from_row(
            sqlx::query(&update_sql)
                .bind(&candidate.id)
                .bind(i16::from(next_attempt))
                .bind(&claim.worker_id)
                .bind(&lease_token)
                .bind(lease_millis)
                .fetch_one(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?,
        )?;

        let attempt_sql = format!(
            "INSERT INTO generation_attempts
             (id, job_id, attempt_number, state, lease_owner, lease_token, provider, model)
             VALUES ($1, $2, $3, 'running', $4, $5, $6, $7)
             RETURNING {ATTEMPT_COLUMNS}"
        );
        let attempt = attempt_from_row(
            sqlx::query(&attempt_sql)
                .bind(&attempt_id)
                .bind(&candidate.id)
                .bind(i16::from(next_attempt))
                .bind(&claim.worker_id)
                .bind(&lease_token)
                .bind(&claim.provider)
                .bind(&claim.model)
                .fetch_one(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?,
        )?;
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(Some(ClaimedGenerationJob {
            lease: GenerationLease {
                job_id: job.id.clone(),
                attempt_id: attempt.id.clone(),
                worker_id: claim.worker_id.clone(),
                lease_token,
            },
            job,
            attempt,
        }))
    }

    pub async fn heartbeat_generation_job(
        &self,
        lease: &GenerationLease,
        lease_duration: Duration,
    ) -> Result<GenerationJob, GenerationJobStoreError> {
        validate_lease(lease)?;
        validate_lease_duration(lease_duration)?;
        let lease_millis = duration_millis(lease_duration)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let update_sql = format!(
            "UPDATE generation_jobs
             SET lease_expires_at = CURRENT_TIMESTAMP + ($4 * INTERVAL '1 millisecond'),
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND state = 'running' AND lease_owner = $2
               AND lease_token = $3 AND lease_expires_at > CURRENT_TIMESTAMP
             RETURNING {JOB_COLUMNS}"
        );
        let Some(row) = sqlx::query(&update_sql)
            .bind(&lease.job_id)
            .bind(&lease.worker_id)
            .bind(&lease.lease_token)
            .bind(lease_millis)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?
        else {
            return Err(GenerationJobStoreError::LostLease);
        };
        let touched = sqlx::query(
            "UPDATE generation_attempts
             SET heartbeat_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND job_id = $2 AND state = 'running'
               AND lease_owner = $3 AND lease_token = $4",
        )
        .bind(&lease.attempt_id)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if touched.rows_affected() != 1 {
            return Err(GenerationJobStoreError::LostLease);
        }
        let job = job_from_row(row)?;
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(job)
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let job = lock_current_lease(&mut transaction, lease).await?;
        if job.purpose == GenerationPurpose::Illustration && success.artifact_id.is_none() {
            return Err(GenerationJobStoreError::InvalidInput(
                "illustration success requires a validated artifact",
            ));
        }
        if let Some(artifact_id) = success.artifact_id.as_deref() {
            let artifact_matches: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM generated_assets
                    WHERE id = $1 AND campaign_session_id = $2
                 )",
            )
            .bind(artifact_id)
            .bind(&job.campaign_session_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?;
            if !artifact_matches {
                return Err(GenerationJobStoreError::InvalidInput(
                    "artifact does not belong to the generation campaign",
                ));
            }
        }

        let usage = usage_bindings(&success.usage)?;
        let attempt_updated = sqlx::query(
            "UPDATE generation_attempts
             SET state = 'succeeded', prompt_tokens = $5, completion_tokens = $6,
                 total_tokens = $7, cost_microusd = $8,
                 latency_milliseconds = $9, artifact_id = $10,
                 output_digest = $11,
                 heartbeat_at = CURRENT_TIMESTAMP, finished_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND job_id = $2 AND state = 'running'
               AND lease_owner = $3 AND lease_token = $4",
        )
        .bind(&lease.attempt_id)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .bind(usage.total_tokens)
        .bind(usage.cost_microusd)
        .bind(usage.latency_milliseconds)
        .bind(success.artifact_id.as_deref())
        .bind(success.output_digest.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if attempt_updated.rows_affected() != 1 {
            return Err(GenerationJobStoreError::LostLease);
        }
        record_generation_attempt_usage(
            &mut transaction,
            &lease.job_id,
            job.purpose,
            &success.usage,
            true,
        )
        .await?;

        let (retention_class_sql, retention_delete_after_sql) =
            match (success.artifact_id.is_some(), job.success_retention) {
                (false, _) => (
                    "'unselected_presentation_30d'",
                    "CURRENT_TIMESTAMP + INTERVAL '30 days'",
                ),
                (true, SuccessRetention::CampaignLifetime) => ("success_retention_class", "NULL"),
                (true, SuccessRetention::UnselectedPresentation30Days) => (
                    "success_retention_class",
                    "CURRENT_TIMESTAMP + INTERVAL '30 days'",
                ),
            };
        let update_sql = format!(
            "UPDATE generation_jobs
             SET state = 'succeeded', retry_at = NULL, lease_owner = NULL,
                 lease_token = NULL, lease_expires_at = NULL,
                 last_failure_class = NULL, last_failure_code = NULL,
                 artifact_id = $2, output_digest = $3,
                 retention_class = {retention_class_sql},
                 retention_delete_after = {retention_delete_after_sql},
                 completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1
             RETURNING {JOB_COLUMNS}"
        );
        let completed = job_from_row(
            sqlx::query(&update_sql)
                .bind(&lease.job_id)
                .bind(success.artifact_id.as_deref())
                .bind(success.output_digest.as_str())
                .fetch_one(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?,
        )?;
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(completed)
    }

    pub async fn fail_generation_attempt(
        &self,
        lease: &GenerationLease,
        failure: &GenerationAttemptFailure,
    ) -> Result<GenerationAttemptFinishOutcome, GenerationJobStoreError> {
        validate_lease(lease)?;
        validate_failure(failure)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let job = lock_current_lease(&mut transaction, lease).await?;
        let usage = usage_bindings(&failure.usage)?;
        let attempt_updated = sqlx::query(
            "UPDATE generation_attempts
             SET state = 'failed', prompt_tokens = $5, completion_tokens = $6,
                 total_tokens = $7, cost_microusd = $8,
                 latency_milliseconds = $9,
                 failure_class = $10, failure_code = $11,
                 provider_status = $12, provider_request_id = $13,
                 output_digest = $14,
                 heartbeat_at = CURRENT_TIMESTAMP, finished_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND job_id = $2 AND state = 'running'
               AND lease_owner = $3 AND lease_token = $4",
        )
        .bind(&lease.attempt_id)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .bind(usage.total_tokens)
        .bind(usage.cost_microusd)
        .bind(usage.latency_milliseconds)
        .bind(failure.code.class().as_str())
        .bind(failure.code.as_str())
        .bind(failure.provider_status.map(i32::from))
        .bind(failure.provider_request_id.as_deref())
        .bind(failure.output_digest.as_ref().map(Sha256Digest::as_str))
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        if attempt_updated.rows_affected() != 1 {
            return Err(GenerationJobStoreError::LostLease);
        }

        let retry_delay = job.purpose.retry_delay(failure.code, job.attempt_count);
        let should_retry = retry_delay.is_some() && job.attempt_count < job.max_attempts;
        record_generation_attempt_usage(
            &mut transaction,
            &lease.job_id,
            job.purpose,
            &failure.usage,
            !should_retry,
        )
        .await?;
        let outcome = if should_retry {
            let retry_millis = duration_millis(
                retry_delay.expect("retryable purpose policy supplies a bounded delay"),
            )?;
            sqlx::query(
                "UPDATE generation_jobs
                 SET state = 'queued', retry_at = CURRENT_TIMESTAMP
                        + ($2 * INTERVAL '1 millisecond'),
                     lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                     last_failure_class = $3, last_failure_code = $4,
                     output_digest = $5,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = $1",
            )
            .bind(&lease.job_id)
            .bind(retry_millis)
            .bind(failure.code.class().as_str())
            .bind(failure.code.as_str())
            .bind(failure.output_digest.as_ref().map(Sha256Digest::as_str))
            .execute(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?;
            GenerationAttemptFinishOutcome::RetryScheduled
        } else {
            sqlx::query(&format!(
                "UPDATE generation_jobs
                 SET state = 'failed', retry_at = NULL, lease_owner = NULL,
                     lease_token = NULL, lease_expires_at = NULL,
                     last_failure_class = $2, last_failure_code = $3,
                     output_digest = $4,
                     retention_class = 'failed_metadata_7d',
                     retention_delete_after = CURRENT_TIMESTAMP + {FAILED_RETENTION_SQL},
                     completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE id = $1"
            ))
            .bind(&lease.job_id)
            .bind(failure.code.class().as_str())
            .bind(failure.code.as_str())
            .bind(failure.output_digest.as_ref().map(Sha256Digest::as_str))
            .execute(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?;
            GenerationAttemptFinishOutcome::Failed
        };
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(outcome)
    }

    /// Cancellation is idempotent only after the job is cancelled. A running
    /// attempt is closed atomically; a stale worker can no longer heartbeat or
    /// publish an artifact after this commit.
    pub async fn cancel_generation_job(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<GenerationJob, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        validate_identifier(job_id, "job id is invalid")?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM generation_jobs
             WHERE id = $1 AND campaign_session_id = $2 FOR UPDATE"
        );
        let job = sqlx::query(&sql)
            .bind(job_id)
            .bind(campaign_session_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(GenerationJobStoreError::Database)?
            .map(job_from_row)
            .transpose()?
            .ok_or_else(|| GenerationJobStoreError::NotFound {
                job_id: job_id.to_owned(),
            })?;
        match job.state {
            GenerationJobState::Cancelled => {
                transaction
                    .commit()
                    .await
                    .map_err(GenerationJobStoreError::Database)?;
                return Ok(job);
            }
            GenerationJobState::Queued => {
                release_generation_budget(&mut transaction, job_id).await?;
            }
            GenerationJobState::Running => {
                let closed = sqlx::query(
                    "UPDATE generation_attempts
                     SET state = 'cancelled', failure_class = 'permanent',
                         failure_code = 'cancelled', heartbeat_at = CURRENT_TIMESTAMP,
                         finished_at = CURRENT_TIMESTAMP
                     WHERE job_id = $1 AND lease_token = $2 AND state = 'running'",
                )
                .bind(job_id)
                .bind(job.lease_token.as_deref())
                .execute(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?;
                if closed.rows_affected() != 1 {
                    return Err(GenerationJobStoreError::InvalidStoredData(
                        "running job has no matching attempt",
                    ));
                }
                settle_unknown_generation_usage(&mut transaction, job_id).await?;
            }
            GenerationJobState::Succeeded | GenerationJobState::Failed => {
                return Err(GenerationJobStoreError::InvalidTransition {
                    job_id: job_id.to_owned(),
                    state: job.state,
                });
            }
        }
        let update_sql = format!(
            "UPDATE generation_jobs
             SET state = 'cancelled', retry_at = NULL, lease_owner = NULL,
                 lease_token = NULL, lease_expires_at = NULL,
                 last_failure_class = 'permanent', last_failure_code = 'cancelled',
                 retention_class = 'failed_metadata_7d',
                 retention_delete_after = CURRENT_TIMESTAMP + {FAILED_RETENTION_SQL},
                 completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1
             RETURNING {JOB_COLUMNS}"
        );
        let cancelled = job_from_row(
            sqlx::query(&update_sql)
                .bind(job_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(GenerationJobStoreError::Database)?,
        )?;
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(cancelled)
    }
}

const JOB_COLUMNS: &str = "
    id, campaign_session_id, origin_turn_id, origin_campaign_revision,
    purpose, idempotency_key, state, input_digest, prompt_digest,
    policy_digest, config_digest, output_digest, correlation_id, attempt_count, max_attempts,
    retry_at::text AS retry_at, lease_owner, lease_token,
    lease_expires_at::text AS lease_expires_at, last_failure_class,
    last_failure_code, artifact_id, success_retention_class, retention_class,
    retention_delete_after::text AS retention_delete_after,
    created_at::text AS created_at, updated_at::text AS updated_at,
    completed_at::text AS completed_at";

const ATTEMPT_COLUMNS: &str = "
    generation_attempts.id, generation_attempts.job_id,
    generation_attempts.attempt_number, generation_attempts.state,
    generation_attempts.lease_owner, generation_attempts.lease_token,
    generation_attempts.provider, generation_attempts.model,
    generation_attempts.prompt_tokens, generation_attempts.completion_tokens,
    generation_attempts.total_tokens, generation_attempts.cost_microusd,
    generation_attempts.latency_milliseconds,
    generation_attempts.failure_class, generation_attempts.failure_code,
    generation_attempts.provider_status, generation_attempts.provider_request_id,
    generation_attempts.artifact_id, generation_attempts.output_digest,
    generation_attempts.started_at::text AS started_at,
    generation_attempts.heartbeat_at::text AS heartbeat_at,
    generation_attempts.finished_at::text AS finished_at,
    generation_attempts.created_at::text AS created_at";

async fn load_job_by_key(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    purpose: GenerationPurpose,
    idempotency_key: &str,
) -> Result<Option<GenerationJob>, GenerationJobStoreError> {
    let sql = format!(
        "SELECT {JOB_COLUMNS} FROM generation_jobs
         WHERE campaign_session_id = $1 AND purpose = $2 AND idempotency_key = $3"
    );
    sqlx::query(&sql)
        .bind(campaign_session_id)
        .bind(purpose.as_str())
        .bind(idempotency_key)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?
        .map(job_from_row)
        .transpose()
}

async fn exact_enqueue_replay(
    transaction: &mut Transaction<'_, Postgres>,
    requested: &NewGenerationJob,
) -> Result<Option<GenerationJob>, GenerationJobStoreError> {
    if let Some(existing) = load_job_by_key(
        transaction,
        &requested.campaign_session_id,
        requested.purpose,
        &requested.idempotency_key,
    )
    .await?
    {
        ensure_matching_replay(&existing, requested)?;
        ensure_job_governance_replay(transaction, &existing, requested).await?;
        return Ok(Some(existing));
    }
    if let Some(receipt) = load_governance_receipt_by_key(
        transaction,
        &requested.campaign_session_id,
        requested.purpose,
        &requested.idempotency_key,
    )
    .await?
    {
        let governance = requested
            .governance
            .as_ref()
            .ok_or(GenerationJobStoreError::IdempotencyConflict)?;
        ensure_matching_governance_receipt(&receipt, governance)?;
        return Err(GenerationJobStoreError::IdempotencyReceiptClosed);
    }
    Ok(None)
}

async fn ensure_job_governance_replay(
    transaction: &mut Transaction<'_, Postgres>,
    existing: &GenerationJob,
    requested: &NewGenerationJob,
) -> Result<(), GenerationJobStoreError> {
    match (
        load_governance_receipt_by_key(
            transaction,
            &existing.campaign_session_id,
            existing.purpose,
            &existing.idempotency_key,
        )
        .await?,
        requested.governance.as_ref(),
    ) {
        (Some(receipt), Some(governance)) if receipt.job_id == existing.id => {
            ensure_matching_governance_receipt(&receipt, governance)
        }
        (None, None) => Ok(()),
        _ => Err(GenerationJobStoreError::IdempotencyConflict),
    }
}

async fn lock_current_lease(
    transaction: &mut Transaction<'_, Postgres>,
    lease: &GenerationLease,
) -> Result<GenerationJob, GenerationJobStoreError> {
    let sql = format!(
        "SELECT {JOB_COLUMNS} FROM generation_jobs
         WHERE id = $1 AND state = 'running' AND lease_owner = $2
           AND lease_token = $3 AND lease_expires_at > CURRENT_TIMESTAMP
         FOR UPDATE"
    );
    sqlx::query(&sql)
        .bind(&lease.job_id)
        .bind(&lease.worker_id)
        .bind(&lease.lease_token)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?
        .map(job_from_row)
        .transpose()?
        .ok_or(GenerationJobStoreError::LostLease)
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

async fn preflight_illustration_request_limits(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    origin_turn_id: Option<&str>,
) -> Result<(), GenerationJobStoreError> {
    let Some(origin_turn_id) = origin_turn_id else {
        return Err(GenerationJobStoreError::InvalidInput(
            "illustration jobs require an origin turn",
        ));
    };
    let row = sqlx::query(
        "SELECT
            COUNT(*)::BIGINT AS lifetime_count,
            COUNT(*) FILTER (
                WHERE created_at > CURRENT_TIMESTAMP - INTERVAL '24 hours'
            )::BIGINT AS rolling_count,
            COUNT(*) FILTER (WHERE origin_turn_id = $2)::BIGINT AS turn_count
         FROM generation_governance_receipts
         WHERE campaign_session_id = $1 AND purpose = 'illustration'",
    )
    .bind(campaign_session_id)
    .bind(origin_turn_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    let lifetime = u64::try_from(
        row.try_get::<i64, _>("lifetime_count")
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::NumericRange)?;
    let rolling = u64::try_from(
        row.try_get::<i64, _>("rolling_count")
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::NumericRange)?;
    let turn = u64::try_from(
        row.try_get::<i64, _>("turn_count")
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::NumericRange)?;
    if lifetime >= IMAGE_REQUESTS_PER_CAMPAIGN_LIFETIME || rolling >= IMAGE_REQUESTS_PER_ROLLING_DAY
    {
        return Err(GenerationJobStoreError::BudgetExceeded {
            scope: GenerationBudgetScope::Campaign,
            dimension: GenerationBudgetDimension::Requests,
        });
    }
    if turn >= IMAGE_REQUESTS_PER_TURN {
        return Err(GenerationJobStoreError::BudgetExceeded {
            scope: GenerationBudgetScope::Turn,
            dimension: GenerationBudgetDimension::Requests,
        });
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
    // The proposed row ID and transport correlation ID may differ on a retry;
    // they do not alter the generation intent. Everything that can affect the
    // produced artifact is bound to the idempotency key.
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

fn validate_identifier(value: &str, reason: &'static str) -> Result<(), GenerationJobStoreError> {
    if !is_valid_opaque_id(value) {
        return Err(GenerationJobStoreError::InvalidInput(reason));
    }
    Ok(())
}

fn map_enqueue_error(error: sqlx::Error) -> GenerationJobStoreError {
    if error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
    {
        GenerationJobStoreError::InvalidInput("generation job id already exists")
    } else {
        GenerationJobStoreError::Database(error)
    }
}

fn duration_millis(duration: Duration) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(duration.as_millis()).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn to_i64(value: u64) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn from_i64(value: i64) -> Result<u64, GenerationJobStoreError> {
    u64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn from_i16(value: i16) -> Result<u8, GenerationJobStoreError> {
    u8::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
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

fn digest_from_row(row: &PgRow, column: &str) -> Result<Sha256Digest, GenerationJobStoreError> {
    let value: String = row
        .try_get(column)
        .map_err(GenerationJobStoreError::Database)?;
    Sha256Digest::new(value)
        .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid stored digest"))
}

fn optional_digest_from_row(
    row: &PgRow,
    column: &str,
) -> Result<Option<Sha256Digest>, GenerationJobStoreError> {
    row.try_get::<Option<String>, _>(column)
        .map_err(GenerationJobStoreError::Database)?
        .map(Sha256Digest::new)
        .transpose()
        .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid stored digest"))
}

fn job_from_row(row: PgRow) -> Result<GenerationJob, GenerationJobStoreError> {
    let purpose: String = row
        .try_get("purpose")
        .map_err(GenerationJobStoreError::Database)?;
    let state: String = row
        .try_get("state")
        .map_err(GenerationJobStoreError::Database)?;
    let success_retention: String = row
        .try_get("success_retention_class")
        .map_err(GenerationJobStoreError::Database)?;
    let last_failure_class: Option<String> = row
        .try_get("last_failure_class")
        .map_err(GenerationJobStoreError::Database)?;
    let last_failure_code: Option<String> = row
        .try_get("last_failure_code")
        .map_err(GenerationJobStoreError::Database)?;
    let job = GenerationJob {
        id: row
            .try_get("id")
            .map_err(GenerationJobStoreError::Database)?,
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(GenerationJobStoreError::Database)?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(GenerationJobStoreError::Database)?,
        origin_campaign_revision: from_i64(
            row.try_get("origin_campaign_revision")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        purpose: purpose.parse()?,
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(GenerationJobStoreError::Database)?,
        state: state.parse()?,
        input_digest: digest_from_row(&row, "input_digest")?,
        prompt_digest: digest_from_row(&row, "prompt_digest")?,
        policy_digest: digest_from_row(&row, "policy_digest")?,
        config_digest: digest_from_row(&row, "config_digest")?,
        output_digest: optional_digest_from_row(&row, "output_digest")?,
        correlation_id: row
            .try_get("correlation_id")
            .map_err(GenerationJobStoreError::Database)?,
        attempt_count: from_i16(
            row.try_get("attempt_count")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        max_attempts: from_i16(
            row.try_get("max_attempts")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        retry_at: row
            .try_get("retry_at")
            .map_err(GenerationJobStoreError::Database)?,
        lease_owner: row
            .try_get("lease_owner")
            .map_err(GenerationJobStoreError::Database)?,
        lease_token: row
            .try_get("lease_token")
            .map_err(GenerationJobStoreError::Database)?,
        lease_expires_at: row
            .try_get("lease_expires_at")
            .map_err(GenerationJobStoreError::Database)?,
        last_failure_class: last_failure_class.map(|value| value.parse()).transpose()?,
        last_failure_code: last_failure_code.map(|value| value.parse()).transpose()?,
        artifact_id: row
            .try_get("artifact_id")
            .map_err(GenerationJobStoreError::Database)?,
        success_retention: success_retention.parse()?,
        retention_class: row
            .try_get::<String, _>("retention_class")
            .map_err(GenerationJobStoreError::Database)?
            .parse()?,
        retention_delete_after: row
            .try_get("retention_delete_after")
            .map_err(GenerationJobStoreError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(GenerationJobStoreError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(GenerationJobStoreError::Database)?,
        completed_at: row
            .try_get("completed_at")
            .map_err(GenerationJobStoreError::Database)?,
    };
    validate_loaded_job(&job)?;
    Ok(job)
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

fn attempt_from_row(row: PgRow) -> Result<GenerationAttempt, GenerationJobStoreError> {
    let state: String = row
        .try_get("state")
        .map_err(GenerationJobStoreError::Database)?;
    let failure_class: Option<String> = row
        .try_get("failure_class")
        .map_err(GenerationJobStoreError::Database)?;
    let failure_code: Option<String> = row
        .try_get("failure_code")
        .map_err(GenerationJobStoreError::Database)?;
    let provider_status: Option<i16> = row
        .try_get("provider_status")
        .map_err(GenerationJobStoreError::Database)?;
    let usage = GenerationUsage {
        prompt_tokens: optional_i64(
            row.try_get("prompt_tokens")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        completion_tokens: optional_i64(
            row.try_get("completion_tokens")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        total_tokens: optional_i64(
            row.try_get("total_tokens")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        cost_microusd: optional_i64(
            row.try_get("cost_microusd")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        latency_milliseconds: optional_i64(
            row.try_get("latency_milliseconds")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
    };
    validate_usage(&usage)?;
    let attempt = GenerationAttempt {
        id: row
            .try_get("id")
            .map_err(GenerationJobStoreError::Database)?,
        job_id: row
            .try_get("job_id")
            .map_err(GenerationJobStoreError::Database)?,
        attempt_number: from_i16(
            row.try_get("attempt_number")
                .map_err(GenerationJobStoreError::Database)?,
        )?,
        state: state.parse()?,
        lease_owner: row
            .try_get("lease_owner")
            .map_err(GenerationJobStoreError::Database)?,
        lease_token: row
            .try_get("lease_token")
            .map_err(GenerationJobStoreError::Database)?,
        provider: row
            .try_get("provider")
            .map_err(GenerationJobStoreError::Database)?,
        model: row
            .try_get("model")
            .map_err(GenerationJobStoreError::Database)?,
        usage,
        failure_class: failure_class.map(|value| value.parse()).transpose()?,
        failure_code: failure_code.map(|value| value.parse()).transpose()?,
        provider_status: provider_status
            .map(|value| u16::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange))
            .transpose()?,
        provider_request_id: row
            .try_get("provider_request_id")
            .map_err(GenerationJobStoreError::Database)?,
        artifact_id: row
            .try_get("artifact_id")
            .map_err(GenerationJobStoreError::Database)?,
        output_digest: optional_digest_from_row(&row, "output_digest")?,
        started_at: row
            .try_get("started_at")
            .map_err(GenerationJobStoreError::Database)?,
        heartbeat_at: row
            .try_get("heartbeat_at")
            .map_err(GenerationJobStoreError::Database)?,
        finished_at: row
            .try_get("finished_at")
            .map_err(GenerationJobStoreError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(GenerationJobStoreError::Database)?,
    };
    validate_identifier(&attempt.id, "stored attempt id is invalid")?;
    validate_identifier(&attempt.job_id, "stored attempt job id is invalid")?;
    validate_identifier(&attempt.lease_owner, "stored attempt worker is invalid")?;
    validate_identifier(&attempt.lease_token, "stored attempt lease is invalid")?;
    if attempt.attempt_number == 0
        || attempt.attempt_number > MAX_ATTEMPTS
        || attempt.started_at.is_empty()
        || attempt.heartbeat_at.is_empty()
        || attempt.created_at.is_empty()
    {
        return Err(GenerationJobStoreError::InvalidStoredData(
            "stored attempt bounds are invalid",
        ));
    }
    Ok(attempt)
}

fn optional_i64(value: Option<i64>) -> Result<Option<u64>, GenerationJobStoreError> {
    value.map(from_i64).transpose()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::repository::MIGRATOR;

    fn repository(pool: PgPool) -> PostgresRepository {
        PostgresRepository::from_pool(pool)
    }

    async fn seed_campaign(pool: &PgPool, id: &str, revision: i64) {
        sqlx::query(
            "INSERT INTO campaign_sessions (id, schema_version, revision, payload_json)
             VALUES ($1, 1, $2, '{}'::jsonb)",
        )
        .bind(id)
        .bind(revision)
        .execute(pool)
        .await
        .expect("campaign fixture should insert");
    }

    async fn seed_turn(pool: &PgPool, campaign_id: &str, turn_id: &str) {
        seed_turn_number(pool, campaign_id, turn_id, 1).await;
    }

    async fn seed_turn_number(pool: &PgPool, campaign_id: &str, turn_id: &str, turn_number: i64) {
        sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, schema_version, payload_json)
             VALUES ($1, $2, $3, 1, '{}'::jsonb)",
        )
        .bind(turn_id)
        .bind(campaign_id)
        .bind(turn_number)
        .execute(pool)
        .await
        .expect("turn fixture should insert");
    }

    async fn seed_artifact(pool: &PgPool, campaign_id: &str, artifact_id: &str) {
        sqlx::query(
            "INSERT INTO generated_assets
             (id, campaign_session_id, asset_kind, provider, model, location, metadata_json)
             VALUES ($1, $2, 'narration', 'fake', 'fake-v1',
                     'campaign/artifact.json', '{}'::jsonb)",
        )
        .bind(artifact_id)
        .bind(campaign_id)
        .execute(pool)
        .await
        .expect("artifact fixture should insert");
    }

    fn new_job(id: &str, key: &str) -> NewGenerationJob {
        NewGenerationJob {
            id: id.to_owned(),
            campaign_session_id: "campaign-1".to_owned(),
            origin_turn_id: None,
            origin_campaign_revision: 1,
            purpose: GenerationPurpose::IntentParsing,
            idempotency_key: key.to_owned(),
            input_digest: Sha256Digest::from_bytes([1; 32]),
            prompt_digest: Sha256Digest::from_bytes([2; 32]),
            policy_digest: Sha256Digest::from_bytes([3; 32]),
            config_digest: Sha256Digest::from_bytes([4; 32]),
            correlation_id: Some("correlation-1".to_owned()),
            max_attempts: 3,
            success_retention: SuccessRetention::UnselectedPresentation30Days,
            governance: None,
        }
    }

    fn claim(worker_id: &str) -> GenerationClaim {
        GenerationClaim {
            worker_id: worker_id.to_owned(),
            provider: "fake".to_owned(),
            model: "fake-v1".to_owned(),
            lease_duration: Duration::from_secs(30),
        }
    }

    fn illustration_governance(turn_scope_key: &str) -> NewGenerationGovernanceReceipt {
        let allowance = crate::config::GenerationBudgetAllowance {
            requests: 100,
            tokens: 100_000,
            latency_milliseconds: 100_000,
            cost_microusd: 100_000,
        };
        NewGenerationGovernanceReceipt {
            turn_scope_key: turn_scope_key.to_owned(),
            request_fingerprint: Sha256Digest::from_bytes([1; 32]),
            policy_fingerprint: Sha256Digest::from_bytes([3; 32]),
            config_fingerprint: Sha256Digest::from_bytes([4; 32]),
            governance_fingerprint: Sha256Digest::from_bytes([5; 32]),
            reserved_requests: 1,
            reserved_tokens: 0,
            reserved_latency_milliseconds: 1_000,
            reserved_cost_microusd: 0,
            limits: crate::config::GenerationGovernanceConfig {
                campaign: allowance,
                turn: allowance,
                max_campaign_concurrency: 2,
                worker_batch_size: 2,
            },
        }
    }

    #[test]
    fn transient_classification_is_a_closed_allowlist() {
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
            assert_eq!(permanent.class(), GenerationFailureClass::Permanent);
        }
    }

    #[test]
    fn postgres_transaction_retry_classification_is_narrow() {
        assert!(transient_postgres_sqlstate("40001"));
        assert!(transient_postgres_sqlstate("40P01"));
        for fail_closed in ["08006", "23505", "23503", "57014", "XX000", ""] {
            assert!(!transient_postgres_sqlstate(fail_closed));
        }
        assert!(!GenerationJobStoreError::InvalidInput("fixture").retryable_database_transaction());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn duplicate_enqueue_replays_exact_metadata_and_rejects_changed_intent(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        let original = new_job("job-1", "generation-key-1");
        let created = repository
            .enqueue_generation_job(&original)
            .await
            .expect("first enqueue should succeed");
        assert!(matches!(created, EnqueueGenerationJobOutcome::Enqueued(_)));

        sqlx::query("UPDATE campaign_sessions SET revision = 2 WHERE id = 'campaign-1'")
            .execute(&pool)
            .await
            .expect("fixture revision should advance");
        let mut replay = original.clone();
        replay.id = "job-from-retry".to_owned();
        replay.correlation_id = Some("correlation-from-retry".to_owned());
        let replayed = repository
            .enqueue_generation_job(&replay)
            .await
            .expect("matching retry should return the original before revision checks");
        assert!(matches!(replayed, EnqueueGenerationJobOutcome::Existing(_)));
        assert_eq!(replayed.job().id, "job-1");

        let mut conflict = replay;
        conflict.input_digest = Sha256Digest::from_bytes([9; 32]);
        assert!(matches!(
            repository.enqueue_generation_job(&conflict).await,
            Err(GenerationJobStoreError::IdempotencyConflict)
        ));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM generation_jobs")
            .fetch_one(&pool)
            .await
            .expect("count should load");
        assert_eq!(count, 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn concurrent_workers_claim_a_job_once(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        repository
            .enqueue_generation_job(&new_job("job-1", "generation-key-1"))
            .await
            .expect("job should enqueue");
        let first = repository.clone();
        let second = repository.clone();
        let first_claim = claim("worker-1");
        let second_claim = claim("worker-2");
        let (first_result, second_result) = tokio::join!(
            first.claim_generation_job(&first_claim),
            second.claim_generation_job(&second_claim),
        );
        let results = [
            first_result.expect("first claim should not fail"),
            second_result.expect("second claim should not fail"),
        ];
        assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);

        let stored = repository
            .load_generation_job("campaign-1", "job-1")
            .await
            .expect("job should load")
            .expect("job should exist");
        assert_eq!(stored.state, GenerationJobState::Running);
        assert_eq!(stored.attempt_count, 1);
        assert_eq!(
            repository
                .list_generation_attempts("campaign-1", "job-1")
                .await
                .expect("attempts should load")
                .len(),
            1
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn exact_claim_never_leases_an_unrelated_ready_job(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        seed_campaign(&pool, "campaign-2", 1).await;
        let repository = repository(pool.clone());
        repository
            .enqueue_generation_job(&new_job("job-first", "generation-key-first"))
            .await
            .expect("first job should enqueue");
        repository
            .enqueue_generation_job(&new_job("job-requested", "generation-key-requested"))
            .await
            .expect("requested job should enqueue");

        assert!(
            repository
                .claim_generation_job_by_id(
                    "campaign-2",
                    "job-requested",
                    &claim("wrong-campaign-worker"),
                )
                .await
                .expect("cross-campaign exact claim should be a safe miss")
                .is_none()
        );
        let requested = repository
            .claim_generation_job_by_id("campaign-1", "job-requested", &claim("inline-worker"))
            .await
            .expect("exact claim should work")
            .expect("requested job should be ready");
        assert_eq!(requested.job.id, "job-requested");

        let first = repository
            .load_generation_job("campaign-1", "job-first")
            .await
            .expect("first job should load")
            .expect("first job should exist");
        assert_eq!(first.state, GenerationJobState::Queued);
        let claimed_first = repository
            .claim_generation_job(&claim("background-worker"))
            .await
            .expect("general claim should work")
            .expect("unrelated first job should remain claimable");
        assert_eq!(claimed_first.job.id, "job-first");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn expired_lease_is_closed_and_reclaimed_without_reusing_attempt(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        let mut job = new_job("job-1", "generation-key-1");
        job.max_attempts = 2;
        repository
            .enqueue_generation_job(&job)
            .await
            .expect("job should enqueue");
        let first = repository
            .claim_generation_job(&claim("worker-1"))
            .await
            .expect("first claim should work")
            .expect("job should be ready");
        sqlx::query(
            "UPDATE generation_jobs
             SET lease_expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second'
             WHERE id = 'job-1'",
        )
        .execute(&pool)
        .await
        .expect("lease should expire");

        let second = repository
            .claim_generation_job(&claim("worker-2"))
            .await
            .expect("reclaim should work")
            .expect("expired job should be reclaimed");
        assert_ne!(first.lease.lease_token, second.lease.lease_token);
        assert_ne!(first.attempt.id, second.attempt.id);
        assert_eq!(second.job.attempt_count, 2);
        let attempts = repository
            .list_generation_attempts("campaign-1", "job-1")
            .await
            .expect("attempts should load");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].state, GenerationAttemptState::Failed);
        assert_eq!(
            attempts[0].failure_code,
            Some(GenerationFailureCode::LeaseExpired)
        );
        assert_eq!(attempts[1].state, GenerationAttemptState::Running);
        assert!(matches!(
            repository
                .heartbeat_generation_job(&first.lease, Duration::from_secs(30))
                .await,
            Err(GenerationJobStoreError::LostLease)
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn attempt_cap_terminalizes_an_expired_final_lease(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        let mut job = new_job("job-1", "generation-key-1");
        job.max_attempts = 1;
        repository
            .enqueue_generation_job(&job)
            .await
            .expect("job should enqueue");
        repository
            .claim_generation_job(&claim("worker-1"))
            .await
            .expect("claim should work")
            .expect("job should be ready");
        sqlx::query(
            "UPDATE generation_jobs
             SET lease_expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second'
             WHERE id = 'job-1'",
        )
        .execute(&pool)
        .await
        .expect("lease should expire");
        assert!(
            repository
                .claim_generation_job(&claim("worker-2"))
                .await
                .expect("terminalization should work")
                .is_none()
        );
        let stored = repository
            .load_generation_job("campaign-1", "job-1")
            .await
            .expect("job should load")
            .expect("job should exist");
        assert_eq!(stored.state, GenerationJobState::Failed);
        assert_eq!(
            stored.retention_class,
            GenerationRetentionClass::FailedMetadata7Days
        );
        assert!(stored.retention_delete_after.is_some());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn transient_failure_schedules_backoff_and_a_fresh_attempt(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        let mut job = new_job("job-1", "generation-key-1");
        job.purpose = GenerationPurpose::GmPlanning;
        repository
            .enqueue_generation_job(&job)
            .await
            .expect("job should enqueue");
        let first = repository
            .claim_generation_job(&claim("worker-1"))
            .await
            .expect("claim should work")
            .expect("job should be ready");
        let outcome = repository
            .fail_generation_attempt(
                &first.lease,
                &GenerationAttemptFailure {
                    code: GenerationFailureCode::Timeout,
                    provider_status: None,
                    provider_request_id: None,
                    usage: GenerationUsage::default(),
                    output_digest: None,
                },
            )
            .await
            .expect("transient failure should commit");
        assert_eq!(outcome, GenerationAttemptFinishOutcome::RetryScheduled);
        assert!(
            repository
                .claim_generation_job(&claim("worker-too-early"))
                .await
                .expect("early poll should work")
                .is_none()
        );
        sqlx::query(
            "UPDATE generation_jobs
             SET retry_at = CURRENT_TIMESTAMP - INTERVAL '1 second'
             WHERE id = 'job-1'",
        )
        .execute(&pool)
        .await
        .expect("retry fixture should become ready");
        let second = repository
            .claim_generation_job(&claim("worker-2"))
            .await
            .expect("second claim should work")
            .expect("backoff should now be due");
        assert_eq!(second.attempt.attempt_number, 2);
        assert_ne!(second.lease.lease_token, first.lease.lease_token);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn text_purposes_complete_metadata_only_with_thirty_day_retention(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        seed_turn(&pool, "campaign-1", "turn-1").await;
        let repository = repository(pool.clone());

        for (index, purpose) in [
            GenerationPurpose::IntentParsing,
            GenerationPurpose::GmPlanning,
            GenerationPurpose::Narration,
        ]
        .into_iter()
        .enumerate()
        {
            if purpose == GenerationPurpose::Narration {
                sqlx::query("UPDATE campaign_sessions SET revision = 2 WHERE id = 'campaign-1'")
                    .execute(&pool)
                    .await
                    .expect("narration origin revision should follow turn one");
            }
            let mut job = new_job(
                &format!("metadata-job-{index}"),
                &format!("metadata-key-{index}"),
            );
            job.purpose = purpose;
            job.origin_turn_id =
                (purpose == GenerationPurpose::Narration).then(|| "turn-1".to_owned());
            if purpose == GenerationPurpose::Narration {
                job.origin_campaign_revision = 2;
            }
            // Metadata-only results never inherit campaign-lifetime retention,
            // even if that was the artifact policy selected at enqueue time.
            job.success_retention = SuccessRetention::CampaignLifetime;
            repository
                .enqueue_generation_job(&job)
                .await
                .expect("metadata job should enqueue");
            let claimed = repository
                .claim_generation_job(&claim(&format!("metadata-worker-{index}")))
                .await
                .expect("claim should work")
                .expect("metadata job should be ready");
            assert_eq!(claimed.job.purpose, purpose);
            let completed = repository
                .succeed_generation_job(
                    &claimed.lease,
                    &GenerationSuccess {
                        artifact_id: None,
                        output_digest: Sha256Digest::from_bytes([5; 32]),
                        usage: GenerationUsage {
                            prompt_tokens: Some(4),
                            completion_tokens: Some(2),
                            total_tokens: Some(6),
                            cost_microusd: Some(1),
                            latency_milliseconds: Some(10),
                        },
                    },
                )
                .await
                .expect("text job should complete without a fake artifact");
            assert_eq!(completed.state, GenerationJobState::Succeeded);
            assert!(completed.artifact_id.is_none());
            assert_eq!(
                completed.retention_class,
                GenerationRetentionClass::UnselectedPresentation30Days
            );
            assert!(completed.retention_delete_after.is_some());
            let attempt = repository
                .list_generation_attempts("campaign-1", &job.id)
                .await
                .expect("attempt should load")
                .pop()
                .expect("attempt should exist");
            assert_eq!(attempt.state, GenerationAttemptState::Succeeded);
            assert!(attempt.artifact_id.is_none());
            assert_eq!(attempt.usage.total_tokens, Some(6));
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn illustration_success_requires_a_campaign_owned_artifact(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 2).await;
        seed_campaign(&pool, "campaign-2", 1).await;
        seed_turn(&pool, "campaign-1", "turn-1").await;
        seed_artifact(&pool, "campaign-1", "artifact-owned").await;
        seed_artifact(&pool, "campaign-2", "artifact-other-campaign").await;
        let repository = repository(pool.clone());
        let mut job = new_job("illustration-job", "illustration-key");
        job.origin_campaign_revision = 2;
        job.purpose = GenerationPurpose::Illustration;
        job.origin_turn_id = Some("turn-1".to_owned());
        job.governance = Some(illustration_governance("turn-1"));
        repository
            .enqueue_generation_job(&job)
            .await
            .expect("illustration should enqueue");
        let claimed = repository
            .claim_generation_job(&claim("illustration-worker"))
            .await
            .expect("claim should work")
            .expect("illustration should be ready");

        let database_bypass = sqlx::query(
            "UPDATE generation_jobs
             SET state = 'succeeded', retry_at = NULL, lease_owner = NULL,
                 lease_token = NULL, lease_expires_at = NULL,
                 completed_at = CURRENT_TIMESTAMP,
                 retention_class = 'unselected_presentation_30d',
                 retention_delete_after = CURRENT_TIMESTAMP + INTERVAL '30 days'
             WHERE id = 'illustration-job'",
        )
        .execute(&pool)
        .await;
        assert!(database_bypass.is_err());

        let metadata_only = GenerationSuccess {
            artifact_id: None,
            output_digest: Sha256Digest::from_bytes([5; 32]),
            usage: GenerationUsage::default(),
        };
        assert!(matches!(
            repository
                .succeed_generation_job(&claimed.lease, &metadata_only)
                .await,
            Err(GenerationJobStoreError::InvalidInput(_))
        ));
        let wrong_campaign = GenerationSuccess {
            artifact_id: Some("artifact-other-campaign".to_owned()),
            output_digest: Sha256Digest::from_bytes([5; 32]),
            usage: GenerationUsage::default(),
        };
        assert!(matches!(
            repository
                .succeed_generation_job(&claimed.lease, &wrong_campaign)
                .await,
            Err(GenerationJobStoreError::InvalidInput(_))
        ));
        let completed = repository
            .succeed_generation_job(
                &claimed.lease,
                &GenerationSuccess {
                    artifact_id: Some("artifact-owned".to_owned()),
                    output_digest: Sha256Digest::from_bytes([5; 32]),
                    usage: GenerationUsage::default(),
                },
            )
            .await
            .expect("campaign-owned artifact should complete illustration");
        assert_eq!(completed.state, GenerationJobState::Succeeded);
        assert_eq!(completed.artifact_id.as_deref(), Some("artifact-owned"));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn illustration_limits_enforce_three_per_day_and_ten_per_lifetime(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 20).await;
        for number in 1..=11_i64 {
            seed_turn_number(&pool, "campaign-1", &format!("image-turn-{number}"), number).await;
        }
        let repository = repository(pool.clone());

        for number in 1..=10_u64 {
            if matches!(number, 4 | 7 | 10) {
                let rejected_number = number;
                let turn_id = format!("image-turn-{rejected_number}");
                let mut rejected = new_job(
                    &format!("image-job-{rejected_number}"),
                    &format!("image-key-{rejected_number}"),
                );
                rejected.origin_turn_id = Some(turn_id.clone());
                rejected.origin_campaign_revision = rejected_number + 1;
                rejected.purpose = GenerationPurpose::Illustration;
                rejected.governance = Some(illustration_governance(&turn_id));
                assert!(matches!(
                    repository.enqueue_generation_job(&rejected).await,
                    Err(GenerationJobStoreError::BudgetExceeded {
                        scope: GenerationBudgetScope::Campaign,
                        dimension: GenerationBudgetDimension::Requests,
                    })
                ));
                sqlx::query(
                    "UPDATE generation_governance_receipts
                     SET created_at = CURRENT_TIMESTAMP - INTERVAL '25 hours'
                     WHERE campaign_session_id = 'campaign-1'
                       AND purpose = 'illustration'",
                )
                .execute(&pool)
                .await
                .unwrap();
            }

            let turn_id = format!("image-turn-{number}");
            let mut job = new_job(
                &format!("image-job-{number}"),
                &format!("image-key-{number}"),
            );
            job.origin_turn_id = Some(turn_id.clone());
            job.origin_campaign_revision = number + 1;
            job.purpose = GenerationPurpose::Illustration;
            job.governance = Some(illustration_governance(&turn_id));
            repository
                .enqueue_generation_job(&job)
                .await
                .expect("request inside the rolling and lifetime caps should enqueue");
            repository
                .cancel_generation_job("campaign-1", &job.id)
                .await
                .unwrap();
        }

        sqlx::query(
            "UPDATE generation_governance_receipts
             SET created_at = CURRENT_TIMESTAMP - INTERVAL '25 hours'
             WHERE campaign_session_id = 'campaign-1' AND purpose = 'illustration'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut eleventh = new_job("image-job-11", "image-key-11");
        eleventh.origin_turn_id = Some("image-turn-11".to_owned());
        eleventh.origin_campaign_revision = 12;
        eleventh.purpose = GenerationPurpose::Illustration;
        eleventh.governance = Some(illustration_governance("image-turn-11"));
        assert!(matches!(
            repository.enqueue_generation_job(&eleventh).await,
            Err(GenerationJobStoreError::BudgetExceeded {
                scope: GenerationBudgetScope::Campaign,
                dimension: GenerationBudgetDimension::Requests,
            })
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn illustration_claims_one_running_job_per_campaign_and_one_replacement(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 5).await;
        seed_turn_number(&pool, "campaign-1", "image-turn-1", 1).await;
        let repository = repository(pool);
        for number in 1..=2_u64 {
            let mut job = new_job(
                &format!("image-job-{number}"),
                &format!("image-key-{number}"),
            );
            job.origin_turn_id = Some("image-turn-1".to_owned());
            job.origin_campaign_revision = 2;
            job.purpose = GenerationPurpose::Illustration;
            job.governance = Some(illustration_governance("image-turn-1"));
            repository.enqueue_generation_job(&job).await.unwrap();
        }

        let first = repository
            .claim_generation_job_for_purpose(
                GenerationPurpose::Illustration,
                &claim("image-worker-1"),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            repository
                .claim_generation_job_for_purpose(
                    GenerationPurpose::Illustration,
                    &claim("image-worker-2"),
                )
                .await
                .unwrap()
                .is_none()
        );
        repository
            .cancel_generation_job("campaign-1", &first.job.id)
            .await
            .unwrap();
        let second = repository
            .claim_generation_job_for_purpose(
                GenerationPurpose::Illustration,
                &claim("image-worker-2"),
            )
            .await
            .unwrap()
            .unwrap();
        repository
            .cancel_generation_job("campaign-1", &second.job.id)
            .await
            .unwrap();

        let mut third = new_job("image-job-3", "image-key-3");
        third.origin_turn_id = Some("image-turn-1".to_owned());
        third.origin_campaign_revision = 2;
        third.purpose = GenerationPurpose::Illustration;
        third.governance = Some(illustration_governance("image-turn-1"));
        assert!(matches!(
            repository.enqueue_generation_job(&third).await,
            Err(GenerationJobStoreError::BudgetExceeded {
                scope: GenerationBudgetScope::Turn,
                dimension: GenerationBudgetDimension::Requests,
            })
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn success_is_atomic_and_terminal_transitions_reject_stale_workers(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        seed_artifact(&pool, "campaign-1", "artifact-1").await;
        let repository = repository(pool.clone());
        repository
            .enqueue_generation_job(&new_job("job-1", "generation-key-1"))
            .await
            .expect("job should enqueue");
        let claimed = repository
            .claim_generation_job(&claim("worker-1"))
            .await
            .expect("claim should work")
            .expect("job should be ready");
        repository
            .heartbeat_generation_job(&claimed.lease, Duration::from_secs(30))
            .await
            .expect("current lease should heartbeat");
        let success = GenerationSuccess {
            artifact_id: Some("artifact-1".to_owned()),
            output_digest: Sha256Digest::from_bytes([5; 32]),
            usage: GenerationUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                cost_microusd: Some(25),
                latency_milliseconds: Some(20),
            },
        };
        let completed = repository
            .succeed_generation_job(&claimed.lease, &success)
            .await
            .expect("success should commit");
        assert_eq!(completed.state, GenerationJobState::Succeeded);
        assert_eq!(completed.artifact_id.as_deref(), Some("artifact-1"));
        assert_eq!(
            completed.retention_class,
            GenerationRetentionClass::UnselectedPresentation30Days
        );
        assert!(completed.retention_delete_after.is_some());
        assert!(matches!(
            repository
                .fail_generation_attempt(
                    &claimed.lease,
                    &GenerationAttemptFailure {
                        code: GenerationFailureCode::Timeout,
                        provider_status: None,
                        provider_request_id: None,
                        usage: GenerationUsage::default(),
                        output_digest: None,
                    },
                )
                .await,
            Err(GenerationJobStoreError::LostLease)
        ));
        assert!(matches!(
            repository
                .cancel_generation_job("campaign-1", "job-1")
                .await,
            Err(GenerationJobStoreError::InvalidTransition { .. })
        ));
        let attempt = repository
            .list_generation_attempts("campaign-1", "job-1")
            .await
            .expect("attempt should load")
            .pop()
            .expect("attempt should exist");
        assert_eq!(attempt.state, GenerationAttemptState::Succeeded);
        assert_eq!(attempt.usage, success.usage);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn cancellation_is_idempotent_and_invalidates_a_running_lease(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        repository
            .enqueue_generation_job(&new_job("job-1", "generation-key-1"))
            .await
            .expect("job should enqueue");
        let claimed = repository
            .claim_generation_job(&claim("worker-1"))
            .await
            .expect("claim should work")
            .expect("job should be ready");
        let cancelled = repository
            .cancel_generation_job("campaign-1", "job-1")
            .await
            .expect("cancel should work");
        assert_eq!(cancelled.state, GenerationJobState::Cancelled);
        let replay = repository
            .cancel_generation_job("campaign-1", "job-1")
            .await
            .expect("cancel replay should return the terminal job");
        assert_eq!(replay, cancelled);
        assert!(matches!(
            repository
                .heartbeat_generation_job(&claimed.lease, Duration::from_secs(30))
                .await,
            Err(GenerationJobStoreError::LostLease)
        ));
        let attempt = repository
            .list_generation_attempts("campaign-1", "job-1")
            .await
            .expect("attempt should load")
            .pop()
            .expect("attempt should exist");
        assert_eq!(attempt.state, GenerationAttemptState::Cancelled);
        assert_eq!(attempt.failure_code, Some(GenerationFailureCode::Cancelled));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn attempt_insert_failure_rolls_back_claim_and_preserves_enqueue_receipt(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        repository
            .enqueue_generation_job(&new_job("job-1", "generation-key-1"))
            .await
            .expect("job should enqueue");
        sqlx::query(
            "ALTER TABLE generation_attempts
             ADD CONSTRAINT reject_test_provider CHECK (provider <> 'force-rollback')",
        )
        .execute(&pool)
        .await
        .expect("test constraint should install");
        let mut rejected_claim = claim("worker-1");
        rejected_claim.provider = "force-rollback".to_owned();
        assert!(matches!(
            repository.claim_generation_job(&rejected_claim).await,
            Err(GenerationJobStoreError::Database(_))
        ));

        let stored = repository
            .load_generation_job("campaign-1", "job-1")
            .await
            .expect("job should load")
            .expect("enqueue receipt should remain");
        assert_eq!(stored.state, GenerationJobState::Queued);
        assert_eq!(stored.attempt_count, 0);
        assert!(
            repository
                .list_generation_attempts("campaign-1", "job-1")
                .await
                .expect("attempt lookup should work")
                .is_empty()
        );
        let replay = repository
            .enqueue_generation_job(&new_job("different-job-id", "generation-key-1"))
            .await
            .expect("original idempotency row should still replay");
        assert_eq!(replay.job().id, "job-1");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn failure_storage_has_no_provider_or_prompt_body_channel(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 1).await;
        let repository = repository(pool.clone());
        repository
            .enqueue_generation_job(&new_job("job-1", "generation-key-1"))
            .await
            .expect("job should enqueue");
        let claimed = repository
            .claim_generation_job(&claim("worker-1"))
            .await
            .expect("claim should work")
            .expect("job should be ready");

        let secret_sentinel = "SECRET provider body with spaces";
        let rejected = GenerationAttemptFailure {
            code: GenerationFailureCode::ProviderRejected,
            provider_status: Some(400),
            provider_request_id: Some(secret_sentinel.to_owned()),
            usage: GenerationUsage::default(),
            output_digest: None,
        };
        assert!(matches!(
            repository
                .fail_generation_attempt(&claimed.lease, &rejected)
                .await,
            Err(GenerationJobStoreError::InvalidInput(_))
        ));

        let safe = GenerationAttemptFailure {
            code: GenerationFailureCode::ProviderRejected,
            provider_status: Some(400),
            provider_request_id: Some("provider-request-1".to_owned()),
            usage: GenerationUsage::default(),
            output_digest: Some(Sha256Digest::from_bytes([5; 32])),
        };
        assert_eq!(
            repository
                .finish_generation_attempt(&claimed.lease, GenerationAttemptFinish::Failed(safe),)
                .await
                .expect("redacted failure should commit"),
            GenerationAttemptFinishOutcome::Failed
        );
        let row_json: String = sqlx::query_scalar(
            "SELECT row_to_json(generation_attempts)::text
             FROM generation_attempts WHERE job_id = 'job-1'",
        )
        .fetch_one(&pool)
        .await
        .expect("attempt JSON should load");
        assert!(!row_json.contains(secret_sentinel));
        assert!(!row_json.contains("provider_body"));
        assert!(!row_json.contains("prompt_body"));

        let prohibited_columns: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name IN ('generation_jobs', 'generation_attempts')
               AND column_name IN (
                   'provider_body', 'response_body', 'raw_response', 'error_message',
                   'prompt_body', 'prompt_text', 'input_body', 'input_json'
               )",
        )
        .fetch_one(&pool)
        .await
        .expect("schema should be inspectable");
        assert_eq!(prohibited_columns, 0);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn narration_and_illustration_require_a_campaign_owned_turn(pool: PgPool) {
        seed_campaign(&pool, "campaign-1", 3).await;
        seed_turn(&pool, "campaign-1", "turn-1").await;
        let repository = repository(pool);
        let mut narration = new_job("job-1", "generation-key-1");
        narration.purpose = GenerationPurpose::Narration;
        assert!(matches!(
            repository.enqueue_generation_job(&narration).await,
            Err(GenerationJobStoreError::InvalidInput(_))
        ));
        narration.origin_turn_id = Some("turn-1".to_owned());
        narration.origin_campaign_revision = 2;
        repository
            .enqueue_generation_job(&narration)
            .await
            .expect("historical campaign-owned turn should enqueue after later commits");
    }
}
