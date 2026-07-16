//! Durable, body-free generation budget governance and bounded telemetry.

use std::str::FromStr;

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::config::{GenerationBudgetAllowance, GenerationGovernanceConfig};

use super::{
    PostgresRepository,
    jobs::{
        GenerationFailureCode, GenerationJobState, GenerationJobStoreError, GenerationPurpose,
        GenerationUsage,
    },
};

pub const GENERATION_GOVERNANCE_SCHEMA_VERSION: u16 = 1;
const DIAGNOSTIC_RETENTION_SQL: &str = "INTERVAL '14 days'";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationBudgetScope {
    Turn,
    Campaign,
    Concurrency,
}

impl GenerationBudgetScope {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Campaign => "campaign",
            Self::Concurrency => "concurrency",
        }
    }
}

impl FromStr for GenerationBudgetScope {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "turn" => Ok(Self::Turn),
            "campaign" => Ok(Self::Campaign),
            "concurrency" => Ok(Self::Concurrency),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation budget scope",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationBudgetDimension {
    Requests,
    Tokens,
    Latency,
    Cost,
    Concurrency,
}

impl GenerationBudgetDimension {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Requests => "requests",
            Self::Tokens => "tokens",
            Self::Latency => "latency",
            Self::Cost => "cost",
            Self::Concurrency => "concurrency",
        }
    }
}

impl FromStr for GenerationBudgetDimension {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "requests" => Ok(Self::Requests),
            "tokens" => Ok(Self::Tokens),
            "latency" => Ok(Self::Latency),
            "cost" => Ok(Self::Cost),
            "concurrency" => Ok(Self::Concurrency),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation budget dimension",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGenerationGovernanceReceipt {
    pub turn_scope_key: String,
    pub request_fingerprint: Sha256Digest,
    pub policy_fingerprint: Sha256Digest,
    pub config_fingerprint: Sha256Digest,
    pub governance_fingerprint: Sha256Digest,
    pub reserved_requests: u8,
    pub reserved_tokens: u64,
    pub reserved_latency_milliseconds: u64,
    pub reserved_cost_microusd: u64,
    pub limits: GenerationGovernanceConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationGovernanceState {
    Reserved,
    Settled,
    Released,
}

impl FromStr for GenerationGovernanceState {
    type Err = GenerationJobStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "settled" => Ok(Self::Settled),
            "released" => Ok(Self::Released),
            _ => Err(GenerationJobStoreError::InvalidStoredData(
                "unknown generation governance state",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationGovernanceReceipt {
    pub campaign_session_id: String,
    pub purpose: GenerationPurpose,
    pub idempotency_key: String,
    pub job_id: String,
    pub origin_turn_id: Option<String>,
    pub turn_scope_key: String,
    pub request_fingerprint: Sha256Digest,
    pub policy_fingerprint: Sha256Digest,
    pub config_fingerprint: Sha256Digest,
    pub governance_fingerprint: Sha256Digest,
    pub state: GenerationGovernanceState,
    pub reserved_requests: u8,
    pub reserved_tokens: u64,
    pub reserved_latency_milliseconds: u64,
    pub reserved_cost_microusd: u64,
    pub spent_requests: u8,
    pub spent_tokens: u64,
    pub spent_latency_milliseconds: u64,
    pub spent_cost_microusd: u64,
    pub overage: bool,
    pub created_at: String,
    pub updated_at: String,
    pub settled_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GenerationBudgetTotals {
    pub requests: u64,
    pub tokens: u64,
    pub latency_milliseconds: u64,
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationBudgetStatusLine {
    pub used: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationBudgetStatus {
    pub schema_version: u16,
    pub campaign_requests: GenerationBudgetStatusLine,
    pub campaign_tokens: GenerationBudgetStatusLine,
    pub campaign_latency_milliseconds: GenerationBudgetStatusLine,
    pub campaign_cost_microusd: GenerationBudgetStatusLine,
    pub active_provider_jobs: u16,
    pub max_active_provider_jobs: u16,
    pub overage_detected: bool,
    pub blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationMetricBucket {
    pub purpose: GenerationPurpose,
    pub state: GenerationJobState,
    pub failure_code: Option<GenerationFailureCode>,
    pub count: u64,
    pub tokens: u64,
    pub latency_milliseconds: u64,
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationBudgetRejectionMetric {
    pub purpose: GenerationPurpose,
    pub scope: GenerationBudgetScope,
    pub dimension: GenerationBudgetDimension,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationMetricsSnapshot {
    pub schema_version: u16,
    pub jobs: Vec<GenerationMetricBucket>,
    pub budget_rejections: Vec<GenerationBudgetRejectionMetric>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationCleanupOutcome {
    pub operational_jobs_deleted: u64,
    pub diagnostics_deleted: u64,
}

pub(super) async fn load_governance_receipt_by_key(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    purpose: GenerationPurpose,
    idempotency_key: &str,
) -> Result<Option<GenerationGovernanceReceipt>, GenerationJobStoreError> {
    sqlx::query(&format!(
        "SELECT {GOVERNANCE_COLUMNS} FROM generation_governance_receipts
         WHERE campaign_session_id = $1 AND purpose = $2 AND idempotency_key = $3"
    ))
    .bind(campaign_session_id)
    .bind(purpose.as_str())
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?
    .map(governance_from_row)
    .transpose()
}

pub(super) fn ensure_matching_governance_receipt(
    existing: &GenerationGovernanceReceipt,
    requested: &NewGenerationGovernanceReceipt,
) -> Result<(), GenerationJobStoreError> {
    if existing.turn_scope_key != requested.turn_scope_key
        || existing.request_fingerprint != requested.request_fingerprint
        || existing.policy_fingerprint != requested.policy_fingerprint
        || existing.config_fingerprint != requested.config_fingerprint
        || existing.governance_fingerprint != requested.governance_fingerprint
        || existing.reserved_requests != requested.reserved_requests
        || existing.reserved_tokens != requested.reserved_tokens
        || existing.reserved_latency_milliseconds != requested.reserved_latency_milliseconds
        || existing.reserved_cost_microusd != requested.reserved_cost_microusd
    {
        return Err(GenerationJobStoreError::IdempotencyConflict);
    }
    Ok(())
}

pub(super) async fn preflight_generation_governance(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    purpose: GenerationPurpose,
    requested: &NewGenerationGovernanceReceipt,
) -> Result<(), GenerationJobStoreError> {
    validate_new_governance(requested)?;
    let campaign = load_totals(transaction, campaign_session_id, None).await?;
    let turn = load_totals(
        transaction,
        campaign_session_id,
        Some(&requested.turn_scope_key),
    )
    .await?;
    let active_provider_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_governance_receipts
         WHERE campaign_session_id = $1 AND state = 'reserved'
           AND reserved_requests > 0",
    )
    .bind(campaign_session_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    let active_provider_jobs =
        u64::try_from(active_provider_jobs).map_err(|_| GenerationJobStoreError::NumericRange)?;
    if requested.reserved_requests > 0
        && active_provider_jobs >= u64::from(requested.limits.max_campaign_concurrency)
    {
        record_budget_diagnostic(
            transaction,
            campaign_session_id,
            purpose,
            GenerationBudgetScope::Concurrency,
            GenerationBudgetDimension::Concurrency,
        )
        .await?;
        return Err(GenerationJobStoreError::BudgetExceeded {
            scope: GenerationBudgetScope::Concurrency,
            dimension: GenerationBudgetDimension::Concurrency,
        });
    }

    let addition = GenerationBudgetTotals {
        requests: u64::from(requested.reserved_requests),
        tokens: requested.reserved_tokens,
        latency_milliseconds: requested.reserved_latency_milliseconds,
        cost_microusd: requested.reserved_cost_microusd,
    };
    for (scope, current, limits) in [
        (
            GenerationBudgetScope::Campaign,
            campaign,
            requested.limits.campaign,
        ),
        (GenerationBudgetScope::Turn, turn, requested.limits.turn),
    ] {
        if let Some(dimension) = exceeds(current, addition, limits)? {
            record_budget_diagnostic(transaction, campaign_session_id, purpose, scope, dimension)
                .await?;
            return Err(GenerationJobStoreError::BudgetExceeded { scope, dimension });
        }
    }
    Ok(())
}

pub(super) async fn insert_generation_governance_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &str,
    campaign_session_id: &str,
    origin_turn_id: Option<&str>,
    purpose: GenerationPurpose,
    idempotency_key: &str,
    requested: &NewGenerationGovernanceReceipt,
) -> Result<(), GenerationJobStoreError> {
    sqlx::query(
        "INSERT INTO generation_governance_receipts
         (campaign_session_id, purpose, idempotency_key, schema_version, job_id,
          origin_turn_id, turn_scope_key, request_fingerprint, policy_fingerprint,
          config_fingerprint, governance_fingerprint, state, reserved_requests,
          reserved_tokens, reserved_latency_milliseconds, reserved_cost_microusd)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'reserved',
                 $12, $13, $14, $15)",
    )
    .bind(campaign_session_id)
    .bind(purpose.as_str())
    .bind(idempotency_key)
    .bind(i64::from(GENERATION_GOVERNANCE_SCHEMA_VERSION))
    .bind(job_id)
    .bind(origin_turn_id)
    .bind(&requested.turn_scope_key)
    .bind(requested.request_fingerprint.as_str())
    .bind(requested.policy_fingerprint.as_str())
    .bind(requested.config_fingerprint.as_str())
    .bind(requested.governance_fingerprint.as_str())
    .bind(i16::from(requested.reserved_requests))
    .bind(to_i64(requested.reserved_tokens)?)
    .bind(to_i64(requested.reserved_latency_milliseconds)?)
    .bind(to_i64(requested.reserved_cost_microusd)?)
    .execute(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    Ok(())
}

pub(super) async fn record_generation_attempt_usage(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &str,
    purpose: GenerationPurpose,
    usage: &GenerationUsage,
    terminal: bool,
) -> Result<(), GenerationJobStoreError> {
    let Some(receipt) = load_governance_receipt_by_job(transaction, job_id, true).await? else {
        return Ok(());
    };
    if receipt.state != GenerationGovernanceState::Reserved {
        return Err(GenerationJobStoreError::InvalidStoredData(
            "generation governance receipt was already settled",
        ));
    }
    let request_increment = u8::from(receipt.reserved_requests > 0);
    let spent_requests = receipt
        .spent_requests
        .checked_add(request_increment)
        .ok_or(GenerationJobStoreError::NumericRange)?;
    let spent_tokens = match usage.total_tokens {
        Some(value) => checked_add(receipt.spent_tokens, value)?,
        None => receipt.spent_tokens.max(receipt.reserved_tokens),
    };
    let spent_latency = match usage.latency_milliseconds {
        Some(value) => checked_add(receipt.spent_latency_milliseconds, value)?,
        None => receipt
            .spent_latency_milliseconds
            .max(receipt.reserved_latency_milliseconds),
    };
    let spent_cost = match usage.cost_microusd {
        Some(value) => checked_add(receipt.spent_cost_microusd, value)?,
        None => receipt
            .spent_cost_microusd
            .max(receipt.reserved_cost_microusd),
    };
    let overages = [
        (
            GenerationBudgetDimension::Requests,
            u64::from(spent_requests) > u64::from(receipt.reserved_requests),
        ),
        (
            GenerationBudgetDimension::Tokens,
            spent_tokens > receipt.reserved_tokens,
        ),
        (
            GenerationBudgetDimension::Latency,
            spent_latency > receipt.reserved_latency_milliseconds,
        ),
        (
            GenerationBudgetDimension::Cost,
            spent_cost > receipt.reserved_cost_microusd,
        ),
    ];
    let overage = receipt.overage || overages.iter().any(|(_, exceeded)| *exceeded);
    sqlx::query(
        "UPDATE generation_governance_receipts
         SET state = CASE WHEN $7 THEN 'settled' ELSE 'reserved' END,
             spent_requests = $2, spent_tokens = $3,
             spent_latency_milliseconds = $4, spent_cost_microusd = $5,
             overage = $6, updated_at = CURRENT_TIMESTAMP,
             settled_at = CASE WHEN $7 THEN CURRENT_TIMESTAMP ELSE NULL END
         WHERE job_id = $1 AND state = 'reserved'",
    )
    .bind(job_id)
    .bind(i16::from(spent_requests))
    .bind(to_i64(spent_tokens)?)
    .bind(to_i64(spent_latency)?)
    .bind(to_i64(spent_cost)?)
    .bind(overage)
    .bind(terminal)
    .execute(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    for (dimension, exceeded) in overages {
        if exceeded {
            record_budget_diagnostic(
                transaction,
                &receipt.campaign_session_id,
                purpose,
                GenerationBudgetScope::Campaign,
                dimension,
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn settle_unknown_generation_usage(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &str,
) -> Result<(), GenerationJobStoreError> {
    sqlx::query(
        "UPDATE generation_governance_receipts
         SET state = 'settled',
             spent_requests = GREATEST(spent_requests, reserved_requests),
             spent_tokens = GREATEST(spent_tokens, reserved_tokens),
             spent_latency_milliseconds = GREATEST(
                 spent_latency_milliseconds, reserved_latency_milliseconds
             ),
             spent_cost_microusd = GREATEST(spent_cost_microusd, reserved_cost_microusd),
             updated_at = CURRENT_TIMESTAMP, settled_at = CURRENT_TIMESTAMP
         WHERE job_id = $1 AND state = 'reserved'",
    )
    .bind(job_id)
    .execute(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    Ok(())
}

pub(super) async fn release_generation_budget(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &str,
) -> Result<(), GenerationJobStoreError> {
    sqlx::query(
        "UPDATE generation_governance_receipts
         SET state = 'released', updated_at = CURRENT_TIMESTAMP,
             settled_at = CURRENT_TIMESTAMP
         WHERE job_id = $1 AND state = 'reserved'",
    )
    .bind(job_id)
    .execute(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    Ok(())
}

impl PostgresRepository {
    pub async fn generation_budget_status(
        &self,
        campaign_session_id: &str,
        config: &GenerationGovernanceConfig,
    ) -> Result<GenerationBudgetStatus, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let totals = load_totals(&mut transaction, campaign_session_id, None).await?;
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM generation_governance_receipts
             WHERE campaign_session_id = $1 AND state = 'reserved'
               AND reserved_requests > 0",
        )
        .bind(campaign_session_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        let overage_detected: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM generation_governance_receipts
                WHERE campaign_session_id = $1 AND overage
             )",
        )
        .bind(campaign_session_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let active = u16::try_from(active).map_err(|_| GenerationJobStoreError::NumericRange)?;
        let blocked = active >= config.max_campaign_concurrency
            || totals.requests >= config.campaign.requests
            || totals.tokens >= config.campaign.tokens
            || totals.latency_milliseconds >= config.campaign.latency_milliseconds
            || totals.cost_microusd >= config.campaign.cost_microusd;
        Ok(GenerationBudgetStatus {
            schema_version: GENERATION_GOVERNANCE_SCHEMA_VERSION,
            campaign_requests: status_line(totals.requests, config.campaign.requests),
            campaign_tokens: status_line(totals.tokens, config.campaign.tokens),
            campaign_latency_milliseconds: status_line(
                totals.latency_milliseconds,
                config.campaign.latency_milliseconds,
            ),
            campaign_cost_microusd: status_line(
                totals.cost_microusd,
                config.campaign.cost_microusd,
            ),
            active_provider_jobs: active,
            max_active_provider_jobs: config.max_campaign_concurrency,
            overage_detected,
            blocked,
        })
    }

    pub async fn generation_metrics_snapshot(
        &self,
    ) -> Result<GenerationMetricsSnapshot, GenerationJobStoreError> {
        let job_rows = sqlx::query(
            "SELECT jobs.purpose, jobs.state, jobs.last_failure_code,
                    COUNT(*) AS count,
                    COALESCE(SUM(attempts.total_tokens), 0)::BIGINT AS tokens,
                    COALESCE(SUM(attempts.latency_milliseconds), 0)::BIGINT AS latency,
                    COALESCE(SUM(attempts.cost_microusd), 0)::BIGINT AS cost
             FROM generation_jobs AS jobs
             LEFT JOIN generation_attempts AS attempts ON attempts.job_id = jobs.id
             GROUP BY jobs.purpose, jobs.state, jobs.last_failure_code
             ORDER BY jobs.purpose, jobs.state, jobs.last_failure_code NULLS FIRST",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        let jobs = job_rows
            .into_iter()
            .map(metric_bucket_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let rejection_rows = sqlx::query(
            "SELECT purpose, budget_scope, budget_dimension, COUNT(*) AS count
             FROM generation_governance_diagnostics
             GROUP BY purpose, budget_scope, budget_dimension
             ORDER BY purpose, budget_scope, budget_dimension",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(GenerationJobStoreError::Database)?;
        let budget_rejections = rejection_rows
            .into_iter()
            .map(rejection_metric_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(GenerationMetricsSnapshot {
            schema_version: GENERATION_GOVERNANCE_SCHEMA_VERSION,
            jobs,
            budget_rejections,
        })
    }

    pub async fn cleanup_generation_metadata(
        &self,
        limit: u16,
    ) -> Result<GenerationCleanupOutcome, GenerationJobStoreError> {
        if limit == 0 || limit > 1_000 {
            return Err(GenerationJobStoreError::InvalidInput(
                "cleanup limit must be between one and one thousand",
            ));
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        let jobs = sqlx::query(
            "WITH expired AS (
                SELECT id FROM generation_jobs
                WHERE state IN ('succeeded', 'failed', 'cancelled')
                  AND retention_delete_after <= CURRENT_TIMESTAMP
                ORDER BY retention_delete_after, id
                LIMIT $1 FOR UPDATE SKIP LOCKED
             )
             DELETE FROM generation_jobs WHERE id IN (SELECT id FROM expired)",
        )
        .bind(i64::from(limit))
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?
        .rows_affected();
        let diagnostics = sqlx::query(
            "WITH expired AS (
                SELECT id FROM generation_governance_diagnostics
                WHERE retention_delete_after <= CURRENT_TIMESTAMP
                ORDER BY retention_delete_after, id
                LIMIT $1 FOR UPDATE SKIP LOCKED
             )
             DELETE FROM generation_governance_diagnostics
             WHERE id IN (SELECT id FROM expired)",
        )
        .bind(i64::from(limit))
        .execute(&mut *transaction)
        .await
        .map_err(GenerationJobStoreError::Database)?
        .rows_affected();
        transaction
            .commit()
            .await
            .map_err(GenerationJobStoreError::Database)?;
        Ok(GenerationCleanupOutcome {
            operational_jobs_deleted: jobs,
            diagnostics_deleted: diagnostics,
        })
    }
}

async fn load_totals(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    turn_scope_key: Option<&str>,
) -> Result<GenerationBudgetTotals, GenerationJobStoreError> {
    let row = sqlx::query(
        "SELECT
            COALESCE(SUM(CASE
                WHEN state = 'reserved' THEN GREATEST(reserved_requests, spent_requests)
                WHEN state = 'settled' THEN spent_requests ELSE 0 END), 0)::BIGINT AS requests,
            COALESCE(SUM(CASE
                WHEN state = 'reserved' THEN GREATEST(reserved_tokens, spent_tokens)
                WHEN state = 'settled' THEN spent_tokens ELSE 0 END), 0)::BIGINT AS tokens,
            COALESCE(SUM(CASE
                WHEN state = 'reserved' THEN GREATEST(
                    reserved_latency_milliseconds, spent_latency_milliseconds)
                WHEN state = 'settled' THEN spent_latency_milliseconds ELSE 0 END), 0)::BIGINT
                AS latency,
            COALESCE(SUM(CASE
                WHEN state = 'reserved' THEN GREATEST(reserved_cost_microusd, spent_cost_microusd)
                WHEN state = 'settled' THEN spent_cost_microusd ELSE 0 END), 0)::BIGINT AS cost
         FROM generation_governance_receipts
         WHERE campaign_session_id = $1
           AND ($2::TEXT IS NULL OR turn_scope_key = $2)",
    )
    .bind(campaign_session_id)
    .bind(turn_scope_key)
    .fetch_one(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    Ok(GenerationBudgetTotals {
        requests: row_u64(&row, "requests")?,
        tokens: row_u64(&row, "tokens")?,
        latency_milliseconds: row_u64(&row, "latency")?,
        cost_microusd: row_u64(&row, "cost")?,
    })
}

async fn load_governance_receipt_by_job(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &str,
    for_update: bool,
) -> Result<Option<GenerationGovernanceReceipt>, GenerationJobStoreError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query(&format!(
        "SELECT {GOVERNANCE_COLUMNS} FROM generation_governance_receipts
         WHERE job_id = $1{lock}"
    ))
    .bind(job_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?
    .map(governance_from_row)
    .transpose()
}

async fn record_budget_diagnostic(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    purpose: GenerationPurpose,
    scope: GenerationBudgetScope,
    dimension: GenerationBudgetDimension,
) -> Result<(), GenerationJobStoreError> {
    sqlx::query(&format!(
        "INSERT INTO generation_governance_diagnostics
         (id, campaign_session_id, schema_version, purpose, failure_code,
          budget_scope, budget_dimension, retention_delete_after)
         VALUES ($1, $2, $3, $4, 'budget_exceeded', $5, $6,
                 CURRENT_TIMESTAMP + {DIAGNOSTIC_RETENTION_SQL})"
    ))
    .bind(format!("generation-diagnostic:{}", Uuid::new_v4().simple()))
    .bind(campaign_session_id)
    .bind(i64::from(GENERATION_GOVERNANCE_SCHEMA_VERSION))
    .bind(purpose.as_str())
    .bind(scope.as_str())
    .bind(dimension.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(GenerationJobStoreError::Database)?;
    Ok(())
}

fn exceeds(
    current: GenerationBudgetTotals,
    addition: GenerationBudgetTotals,
    limits: GenerationBudgetAllowance,
) -> Result<Option<GenerationBudgetDimension>, GenerationJobStoreError> {
    for (dimension, used, added, limit) in [
        (
            GenerationBudgetDimension::Requests,
            current.requests,
            addition.requests,
            limits.requests,
        ),
        (
            GenerationBudgetDimension::Tokens,
            current.tokens,
            addition.tokens,
            limits.tokens,
        ),
        (
            GenerationBudgetDimension::Latency,
            current.latency_milliseconds,
            addition.latency_milliseconds,
            limits.latency_milliseconds,
        ),
        (
            GenerationBudgetDimension::Cost,
            current.cost_microusd,
            addition.cost_microusd,
            limits.cost_microusd,
        ),
    ] {
        if checked_add(used, added)? > limit {
            return Ok(Some(dimension));
        }
    }
    Ok(None)
}

fn validate_new_governance(
    requested: &NewGenerationGovernanceReceipt,
) -> Result<(), GenerationJobStoreError> {
    validate_identifier(&requested.turn_scope_key, "turn scope key is invalid")?;
    for value in [
        requested.reserved_tokens,
        requested.reserved_latency_milliseconds,
        requested.reserved_cost_microusd,
        requested.limits.campaign.requests,
        requested.limits.campaign.tokens,
        requested.limits.campaign.latency_milliseconds,
        requested.limits.campaign.cost_microusd,
        requested.limits.turn.requests,
        requested.limits.turn.tokens,
        requested.limits.turn.latency_milliseconds,
        requested.limits.turn.cost_microusd,
    ] {
        to_i64(value)?;
    }
    if requested.reserved_requests > 5
        || requested.limits.max_campaign_concurrency == 0
        || requested.limits.max_campaign_concurrency > 32
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "generation governance bounds are invalid",
        ));
    }
    Ok(())
}

fn governance_from_row(row: PgRow) -> Result<GenerationGovernanceReceipt, GenerationJobStoreError> {
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(GenerationJobStoreError::Database)?;
    if schema_version != i64::from(GENERATION_GOVERNANCE_SCHEMA_VERSION) {
        return Err(GenerationJobStoreError::InvalidStoredData(
            "unsupported generation governance schema version",
        ));
    }
    let purpose: String = row
        .try_get("purpose")
        .map_err(GenerationJobStoreError::Database)?;
    let state: String = row
        .try_get("state")
        .map_err(GenerationJobStoreError::Database)?;
    let receipt = GenerationGovernanceReceipt {
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(GenerationJobStoreError::Database)?,
        purpose: purpose.parse()?,
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(GenerationJobStoreError::Database)?,
        job_id: row
            .try_get("job_id")
            .map_err(GenerationJobStoreError::Database)?,
        origin_turn_id: row
            .try_get("origin_turn_id")
            .map_err(GenerationJobStoreError::Database)?,
        turn_scope_key: row
            .try_get("turn_scope_key")
            .map_err(GenerationJobStoreError::Database)?,
        request_fingerprint: digest_from_row(&row, "request_fingerprint")?,
        policy_fingerprint: digest_from_row(&row, "policy_fingerprint")?,
        config_fingerprint: digest_from_row(&row, "config_fingerprint")?,
        governance_fingerprint: digest_from_row(&row, "governance_fingerprint")?,
        state: state.parse()?,
        reserved_requests: row_u8(&row, "reserved_requests")?,
        reserved_tokens: row_u64(&row, "reserved_tokens")?,
        reserved_latency_milliseconds: row_u64(&row, "reserved_latency_milliseconds")?,
        reserved_cost_microusd: row_u64(&row, "reserved_cost_microusd")?,
        spent_requests: row_u8(&row, "spent_requests")?,
        spent_tokens: row_u64(&row, "spent_tokens")?,
        spent_latency_milliseconds: row_u64(&row, "spent_latency_milliseconds")?,
        spent_cost_microusd: row_u64(&row, "spent_cost_microusd")?,
        overage: row
            .try_get("overage")
            .map_err(GenerationJobStoreError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(GenerationJobStoreError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(GenerationJobStoreError::Database)?,
        settled_at: row
            .try_get("settled_at")
            .map_err(GenerationJobStoreError::Database)?,
    };
    validate_identifier(
        &receipt.campaign_session_id,
        "stored campaign id is invalid",
    )?;
    validate_identifier(
        &receipt.idempotency_key,
        "stored idempotency key is invalid",
    )?;
    validate_identifier(&receipt.job_id, "stored job id is invalid")?;
    validate_identifier(&receipt.turn_scope_key, "stored turn scope key is invalid")?;
    Ok(receipt)
}

fn metric_bucket_from_row(row: PgRow) -> Result<GenerationMetricBucket, GenerationJobStoreError> {
    let purpose: String = row
        .try_get("purpose")
        .map_err(GenerationJobStoreError::Database)?;
    let state: String = row
        .try_get("state")
        .map_err(GenerationJobStoreError::Database)?;
    let failure: Option<String> = row
        .try_get("last_failure_code")
        .map_err(GenerationJobStoreError::Database)?;
    Ok(GenerationMetricBucket {
        purpose: purpose.parse()?,
        state: state.parse()?,
        failure_code: failure.map(|value| value.parse()).transpose()?,
        count: row_u64(&row, "count")?,
        tokens: row_u64(&row, "tokens")?,
        latency_milliseconds: row_u64(&row, "latency")?,
        cost_microusd: row_u64(&row, "cost")?,
    })
}

fn rejection_metric_from_row(
    row: PgRow,
) -> Result<GenerationBudgetRejectionMetric, GenerationJobStoreError> {
    let purpose: String = row
        .try_get("purpose")
        .map_err(GenerationJobStoreError::Database)?;
    let scope: String = row
        .try_get("budget_scope")
        .map_err(GenerationJobStoreError::Database)?;
    let dimension: String = row
        .try_get("budget_dimension")
        .map_err(GenerationJobStoreError::Database)?;
    Ok(GenerationBudgetRejectionMetric {
        purpose: purpose.parse()?,
        scope: scope.parse()?,
        dimension: dimension.parse()?,
        count: row_u64(&row, "count")?,
    })
}

fn status_line(used: u64, limit: u64) -> GenerationBudgetStatusLine {
    GenerationBudgetStatusLine { used, limit }
}

fn checked_add(left: u64, right: u64) -> Result<u64, GenerationJobStoreError> {
    left.checked_add(right)
        .ok_or(GenerationJobStoreError::NumericRange)
}

fn to_i64(value: u64) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn row_u64(row: &PgRow, column: &str) -> Result<u64, GenerationJobStoreError> {
    u64::try_from(
        row.try_get::<i64, _>(column)
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::NumericRange)
}

fn row_u8(row: &PgRow, column: &str) -> Result<u8, GenerationJobStoreError> {
    u8::try_from(
        row.try_get::<i16, _>(column)
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::NumericRange)
}

fn digest_from_row(row: &PgRow, column: &str) -> Result<Sha256Digest, GenerationJobStoreError> {
    Sha256Digest::new(
        row.try_get::<String, _>(column)
            .map_err(GenerationJobStoreError::Database)?,
    )
    .map_err(|_| GenerationJobStoreError::InvalidStoredData("invalid governance digest"))
}

fn validate_identifier(value: &str, reason: &'static str) -> Result<(), GenerationJobStoreError> {
    if is_valid_opaque_id(value) {
        Ok(())
    } else {
        Err(GenerationJobStoreError::InvalidInput(reason))
    }
}

const GOVERNANCE_COLUMNS: &str = "
    campaign_session_id, purpose, idempotency_key, schema_version, job_id,
    origin_turn_id, turn_scope_key, request_fingerprint, policy_fingerprint,
    config_fingerprint, governance_fingerprint, state, reserved_requests,
    reserved_tokens, reserved_latency_milliseconds, reserved_cost_microusd,
    spent_requests, spent_tokens, spent_latency_milliseconds, spent_cost_microusd,
    overage, created_at::text AS created_at, updated_at::text AS updated_at,
    settled_at::text AS settled_at";

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;

    use super::*;
    use crate::repository::{
        MIGRATOR,
        jobs::{
            EnqueueGenerationJobOutcome, GenerationAttemptFailure, GenerationClaim,
            GenerationPurpose, NewGenerationJob, SuccessRetention,
        },
    };

    async fn seed_campaign(pool: &PgPool) {
        sqlx::query(
            "INSERT INTO campaign_sessions (id, schema_version, revision, payload_json)
             VALUES ('governance-campaign', 1, 1, '{}'::jsonb)",
        )
        .execute(pool)
        .await
        .expect("campaign fixture should insert");
    }

    fn limits() -> GenerationGovernanceConfig {
        GenerationGovernanceConfig {
            campaign: GenerationBudgetAllowance {
                requests: 2,
                tokens: 300,
                latency_milliseconds: 3_000,
                cost_microusd: 150,
            },
            turn: GenerationBudgetAllowance {
                requests: 2,
                tokens: 300,
                latency_milliseconds: 3_000,
                cost_microusd: 150,
            },
            max_campaign_concurrency: 2,
            worker_batch_size: 2,
        }
    }

    fn job(id: &str, key: &str, turn_scope_key: &str) -> NewGenerationJob {
        NewGenerationJob {
            id: id.to_owned(),
            campaign_session_id: "governance-campaign".to_owned(),
            origin_turn_id: None,
            origin_campaign_revision: 1,
            purpose: GenerationPurpose::IntentParsing,
            idempotency_key: key.to_owned(),
            input_digest: Sha256Digest::from_bytes([1; 32]),
            prompt_digest: Sha256Digest::from_bytes([2; 32]),
            policy_digest: Sha256Digest::from_bytes([3; 32]),
            config_digest: Sha256Digest::from_bytes([4; 32]),
            correlation_id: Some(format!("correlation:{id}")),
            max_attempts: 1,
            success_retention: SuccessRetention::UnselectedPresentation30Days,
            governance: Some(NewGenerationGovernanceReceipt {
                turn_scope_key: turn_scope_key.to_owned(),
                request_fingerprint: Sha256Digest::from_bytes([1; 32]),
                policy_fingerprint: Sha256Digest::from_bytes([3; 32]),
                config_fingerprint: Sha256Digest::from_bytes([4; 32]),
                governance_fingerprint: Sha256Digest::from_bytes([5; 32]),
                reserved_requests: 1,
                reserved_tokens: 100,
                reserved_latency_milliseconds: 1_000,
                reserved_cost_microusd: 50,
                limits: limits(),
            }),
        }
    }

    fn claim(worker_id: &str) -> GenerationClaim {
        GenerationClaim {
            worker_id: worker_id.to_owned(),
            provider: "deterministic-fake".to_owned(),
            model: "fake-v1".to_owned(),
            lease_duration: Duration::from_secs(60),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn concurrent_preflight_serializes_and_exact_replay_never_double_reserves(pool: PgPool) {
        seed_campaign(&pool).await;
        let repository = PostgresRepository::from_pool(pool);
        let mut constrained = limits();
        constrained.max_campaign_concurrency = 1;
        constrained.campaign.requests = 10;
        constrained.turn.requests = 10;
        constrained.campaign.tokens = 10_000;
        constrained.turn.tokens = 10_000;
        constrained.campaign.latency_milliseconds = 10_000;
        constrained.turn.latency_milliseconds = 10_000;
        constrained.campaign.cost_microusd = 1_000;
        constrained.turn.cost_microusd = 1_000;
        let mut first_job = job("governance-job-1", "governance-key-1", "turn-scope-1");
        first_job.governance.as_mut().unwrap().limits = constrained.clone();
        let mut second_job = job("governance-job-2", "governance-key-2", "turn-scope-2");
        second_job.governance.as_mut().unwrap().limits = constrained.clone();

        let (first, second) = tokio::join!(
            repository.enqueue_generation_job(&first_job),
            repository.enqueue_generation_job(&second_job),
        );
        assert!(matches!(
            (&first, &second),
            (
                Ok(EnqueueGenerationJobOutcome::Enqueued(_)),
                Err(GenerationJobStoreError::BudgetExceeded {
                    scope: GenerationBudgetScope::Concurrency,
                    dimension: GenerationBudgetDimension::Concurrency,
                })
            ) | (
                Err(GenerationJobStoreError::BudgetExceeded {
                    scope: GenerationBudgetScope::Concurrency,
                    dimension: GenerationBudgetDimension::Concurrency,
                }),
                Ok(EnqueueGenerationJobOutcome::Enqueued(_))
            )
        ));
        let accepted = if first.is_ok() {
            &first_job
        } else {
            &second_job
        };
        assert!(matches!(
            repository.enqueue_generation_job(accepted).await.unwrap(),
            EnqueueGenerationJobOutcome::Existing(_)
        ));
        let status = repository
            .generation_budget_status("governance-campaign", &constrained)
            .await
            .unwrap();
        assert_eq!(status.active_provider_jobs, 1);
        assert_eq!(status.campaign_requests.used, 1);
        assert!(status.blocked);
        let metrics = repository.generation_metrics_snapshot().await.unwrap();
        assert_eq!(metrics.budget_rejections.len(), 1);
        assert_eq!(metrics.budget_rejections[0].count, 1);

        repository
            .cancel_generation_job("governance-campaign", &accepted.id)
            .await
            .unwrap();
        let released = repository
            .generation_budget_status("governance-campaign", &constrained)
            .await
            .unwrap();
        assert_eq!(released.active_provider_jobs, 0);
        assert_eq!(released.campaign_requests.used, 0);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn usage_overage_cancellation_and_cleanup_keep_conservative_lifetime_totals(
        pool: PgPool,
    ) {
        seed_campaign(&pool).await;
        let repository = PostgresRepository::from_pool(pool.clone());

        let overage_job = job(
            "governance-job-overage",
            "governance-key-overage",
            "turn-overage",
        );
        repository
            .enqueue_generation_job(&overage_job)
            .await
            .unwrap();
        let overage_claim = repository
            .claim_generation_job_by_id(
                "governance-campaign",
                &overage_job.id,
                &claim("worker-overage"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repository
                .fail_generation_attempt(
                    &overage_claim.lease,
                    &GenerationAttemptFailure {
                        code: GenerationFailureCode::ProviderUnavailable,
                        provider_status: None,
                        provider_request_id: None,
                        usage: GenerationUsage {
                            prompt_tokens: Some(100),
                            completion_tokens: Some(50),
                            total_tokens: Some(150),
                            cost_microusd: Some(75),
                            latency_milliseconds: Some(1_500),
                        },
                        output_digest: None,
                    },
                )
                .await
                .unwrap(),
            crate::repository::jobs::GenerationAttemptFinishOutcome::Failed
        );

        let queued_job = job(
            "governance-job-queued",
            "governance-key-queued",
            "turn-queued",
        );
        repository
            .enqueue_generation_job(&queued_job)
            .await
            .unwrap();
        repository
            .cancel_generation_job("governance-campaign", &queued_job.id)
            .await
            .unwrap();

        let running_job = job(
            "governance-job-running",
            "governance-key-running",
            "turn-running",
        );
        repository
            .enqueue_generation_job(&running_job)
            .await
            .unwrap();
        repository
            .claim_generation_job_by_id(
                "governance-campaign",
                &running_job.id,
                &claim("worker-running"),
            )
            .await
            .unwrap()
            .unwrap();
        repository
            .cancel_generation_job("governance-campaign", &running_job.id)
            .await
            .unwrap();

        let status = repository
            .generation_budget_status("governance-campaign", &limits())
            .await
            .unwrap();
        assert_eq!(status.campaign_requests.used, 2);
        assert_eq!(status.campaign_tokens.used, 250);
        assert_eq!(status.campaign_latency_milliseconds.used, 2_500);
        assert_eq!(status.campaign_cost_microusd.used, 125);
        assert_eq!(status.active_provider_jobs, 0);
        assert!(status.overage_detected);

        let rejected = job(
            "governance-job-rejected",
            "governance-key-rejected",
            "turn-rejected",
        );
        assert!(matches!(
            repository.enqueue_generation_job(&rejected).await,
            Err(GenerationJobStoreError::BudgetExceeded {
                scope: GenerationBudgetScope::Campaign,
                dimension: GenerationBudgetDimension::Requests,
            })
        ));
        let metrics = repository.generation_metrics_snapshot().await.unwrap();
        assert!(metrics.budget_rejections.iter().any(|metric| {
            metric.scope == GenerationBudgetScope::Campaign
                && metric.dimension == GenerationBudgetDimension::Requests
                && metric.count == 1
        }));

        sqlx::query(
            "UPDATE generation_jobs
             SET retention_delete_after = CURRENT_TIMESTAMP - INTERVAL '1 second'
             WHERE campaign_session_id = 'governance-campaign' AND state IN ('failed', 'cancelled')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let cleanup = repository.cleanup_generation_metadata(100).await.unwrap();
        assert_eq!(cleanup.operational_jobs_deleted, 3);
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM generation_governance_receipts
             WHERE campaign_session_id = 'governance-campaign'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipt_count, 3);
        let retained = repository
            .generation_budget_status("governance-campaign", &limits())
            .await
            .unwrap();
        assert_eq!(retained.campaign_tokens.used, 250);
        assert!(retained.overage_detected);
    }
}
