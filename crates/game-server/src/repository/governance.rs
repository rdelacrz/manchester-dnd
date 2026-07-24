//! Durable, body-free generation budget governance and bounded telemetry.

use std::{collections::BTreeMap, future::IntoFuture, str::FromStr, time::Duration};

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use mongodb::{
    ClientSession, Collection,
    bson::{DateTime, doc},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::{GenerationBudgetAllowance, GenerationGovernanceConfig},
    error::PersistenceError,
    persistence::{CollectionName, MongoStore},
};

use super::{
    MongoRepository,
    jobs::{
        GenerationFailureCode, GenerationJobDocument, GenerationJobState, GenerationJobStoreError,
        GenerationPurpose, GenerationUsage,
    },
};

pub const GENERATION_GOVERNANCE_SCHEMA_VERSION: u16 = 1;
const RESERVATION_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const ACTIVE_RESERVATION_HORIZON: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const DIAGNOSTIC_RETENTION: Duration = Duration::from_secs(14 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

impl GenerationGovernanceState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Settled => "settled",
            Self::Released => "released",
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationBudgetReservationDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: u16,
    job_id: String,
    campaign_id: String,
    purpose: String,
    idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin_turn_id: Option<String>,
    turn_scope_key: String,
    scope_kind: String,
    scope_id: String,
    dimension: String,
    state: String,
    request_fingerprint: String,
    policy_fingerprint: String,
    config_fingerprint: String,
    governance_fingerprint: String,
    reserved_value: i64,
    spent_value: i64,
    overage: bool,
    expires_at: DateTime,
    purge_at: DateTime,
    created_at: DateTime,
    updated_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settled_at: Option<DateTime>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetDiagnosticDocument {
    metadata: BudgetDiagnosticMetadata,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetDiagnosticMetadata {
    purpose: String,
    budget_scope: String,
    budget_dimension: String,
}

pub(super) fn validate_new_governance(
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
        || requested.limits.worker_batch_size == 0
        || requested.limits.worker_batch_size > 1_000
        || requested.governance_fingerprint != requested.limits.non_secret_fingerprint()
    {
        return Err(GenerationJobStoreError::InvalidInput(
            "generation governance bounds are invalid",
        ));
    }
    Ok(())
}

pub(super) async fn load_governance_receipt_by_key(
    store: &MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
    purpose: GenerationPurpose,
    idempotency_key: &str,
) -> Result<Option<GenerationGovernanceReceipt>, PersistenceError> {
    let documents = load_reservations(
        store,
        session,
        doc! {
            "campaign_id": campaign_id,
            "purpose": purpose.as_str(),
            "idempotency_key": idempotency_key,
        },
    )
    .await?;
    receipt_from_reservations(documents)
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
    store: &MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
    purpose: GenerationPurpose,
    requested: &NewGenerationGovernanceReceipt,
) -> Result<Result<(), GenerationJobStoreError>, PersistenceError> {
    let campaign = load_totals(store, session, campaign_id, None).await?;
    let turn = load_totals(store, session, campaign_id, Some(&requested.turn_scope_key)).await?;
    let active_provider_jobs = reservations(store)
        .count_documents(doc! {
            "campaign_id": campaign_id,
            "dimension": GenerationBudgetDimension::Requests.as_str(),
            "state": GenerationGovernanceState::Reserved.as_str(),
            "reserved_value": { "$gt": 0_i64 },
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("count active generation reservations", error))?;
    if requested.reserved_requests > 0
        && active_provider_jobs >= u64::from(requested.limits.max_campaign_concurrency)
    {
        record_budget_diagnostic(
            store,
            session,
            campaign_id,
            purpose,
            GenerationBudgetScope::Concurrency,
            GenerationBudgetDimension::Concurrency,
        )
        .await?;
        return Ok(Err(GenerationJobStoreError::BudgetExceeded {
            scope: GenerationBudgetScope::Concurrency,
            dimension: GenerationBudgetDimension::Concurrency,
        }));
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
        let exceeded = match exceeds(current, addition, limits) {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if let Some(dimension) = exceeded {
            record_budget_diagnostic(store, session, campaign_id, purpose, scope, dimension)
                .await?;
            return Ok(Err(GenerationJobStoreError::BudgetExceeded {
                scope,
                dimension,
            }));
        }
    }
    Ok(Ok(()))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_generation_governance_receipt(
    store: &MongoStore,
    session: &mut ClientSession,
    job_id: &str,
    campaign_id: &str,
    origin_turn_id: Option<&str>,
    purpose: GenerationPurpose,
    idempotency_key: &str,
    requested: &NewGenerationGovernanceReceipt,
) -> Result<(), PersistenceError> {
    let now = DateTime::now();
    let purge_at = add_duration(now, ACTIVE_RESERVATION_HORIZON);
    let values = [
        (
            GenerationBudgetDimension::Requests,
            i64::from(requested.reserved_requests),
        ),
        (
            GenerationBudgetDimension::Tokens,
            to_i64(requested.reserved_tokens).map_err(domain_as_persistence)?,
        ),
        (
            GenerationBudgetDimension::Latency,
            to_i64(requested.reserved_latency_milliseconds).map_err(domain_as_persistence)?,
        ),
        (
            GenerationBudgetDimension::Cost,
            to_i64(requested.reserved_cost_microusd).map_err(domain_as_persistence)?,
        ),
    ];
    let documents = values
        .into_iter()
        .map(
            |(dimension, reserved_value)| GenerationBudgetReservationDocument {
                id: format!("budget-reservation:{job_id}:{}", dimension.as_str()),
                schema_version: GENERATION_GOVERNANCE_SCHEMA_VERSION,
                job_id: job_id.to_owned(),
                campaign_id: campaign_id.to_owned(),
                purpose: purpose.as_str().to_owned(),
                idempotency_key: idempotency_key.to_owned(),
                origin_turn_id: origin_turn_id.map(str::to_owned),
                turn_scope_key: requested.turn_scope_key.clone(),
                scope_kind: GenerationBudgetScope::Campaign.as_str().to_owned(),
                scope_id: campaign_id.to_owned(),
                dimension: dimension.as_str().to_owned(),
                state: GenerationGovernanceState::Reserved.as_str().to_owned(),
                request_fingerprint: requested.request_fingerprint.as_str().to_owned(),
                policy_fingerprint: requested.policy_fingerprint.as_str().to_owned(),
                config_fingerprint: requested.config_fingerprint.as_str().to_owned(),
                governance_fingerprint: requested.governance_fingerprint.as_str().to_owned(),
                reserved_value,
                spent_value: 0,
                overage: false,
                expires_at: purge_at,
                purge_at,
                created_at: now,
                updated_at: now,
                settled_at: None,
            },
        )
        .collect::<Vec<_>>();
    reservations(store)
        .insert_many(documents)
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("insert generation budget reservations", error))?;
    Ok(())
}

pub(super) async fn record_generation_attempt_usage(
    store: &MongoStore,
    session: &mut ClientSession,
    job_id: &str,
    purpose: GenerationPurpose,
    usage: &GenerationUsage,
    terminal: bool,
) -> Result<Result<(), GenerationJobStoreError>, PersistenceError> {
    let documents = load_reservations(store, session, doc! { "job_id": job_id }).await?;
    if documents.is_empty() {
        return Ok(Ok(()));
    }
    if documents
        .iter()
        .any(|document| document.state != GenerationGovernanceState::Reserved.as_str())
    {
        return Ok(Err(GenerationJobStoreError::InvalidStoredData(
            "generation governance receipt was already settled",
        )));
    }
    let campaign_id = documents[0].campaign_id.clone();
    let usage_by_dimension = [
        (
            GenerationBudgetDimension::Requests,
            Some(u64::from(documents[0].reserved_value > 0)),
        ),
        (GenerationBudgetDimension::Tokens, usage.total_tokens),
        (
            GenerationBudgetDimension::Latency,
            usage.latency_milliseconds,
        ),
        (GenerationBudgetDimension::Cost, usage.cost_microusd),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let now = DateTime::now();
    for document in documents {
        let dimension = match document.dimension.parse::<GenerationBudgetDimension>() {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        let increment = usage_by_dimension.get(&dimension).copied().flatten();
        let spent = match increment {
            Some(value) => match checked_add(
                u64::try_from(document.spent_value).map_err(|_| PersistenceError::SchemaDrift {
                    collection: CollectionName::GenerationBudgetReservations
                        .as_str()
                        .to_owned(),
                    detail: "stored generation spent value is negative".to_owned(),
                })?,
                value,
            ) {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            },
            None => {
                u64::try_from(document.spent_value.max(document.reserved_value)).map_err(|_| {
                    PersistenceError::SchemaDrift {
                        collection: CollectionName::GenerationBudgetReservations
                            .as_str()
                            .to_owned(),
                        detail: "stored generation reservation is negative".to_owned(),
                    }
                })?
            }
        };
        let spent = match to_i64(spent) {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        let overage = document.overage || spent > document.reserved_value;
        let state = if terminal {
            GenerationGovernanceState::Settled
        } else {
            GenerationGovernanceState::Reserved
        };
        let mut set = doc! {
            "spent_value": spent,
            "overage": overage,
            "state": state.as_str(),
            "updated_at": now,
        };
        if terminal {
            set.insert("settled_at", now);
            set.insert("expires_at", add_duration(now, RESERVATION_RETENTION));
            set.insert("purge_at", add_duration(now, RESERVATION_RETENTION));
        }
        reservations(store)
            .update_one(
                doc! {
                    "_id": &document.id,
                    "state": GenerationGovernanceState::Reserved.as_str(),
                },
                doc! { "$set": set },
            )
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("settle generation budget dimension", error)
            })?;
        if overage && !document.overage {
            record_budget_diagnostic(
                store,
                session,
                &campaign_id,
                purpose,
                GenerationBudgetScope::Campaign,
                dimension,
            )
            .await?;
        }
    }
    Ok(Ok(()))
}

pub(super) async fn settle_unknown_generation_usage(
    store: &MongoStore,
    session: &mut ClientSession,
    job_id: &str,
) -> Result<(), PersistenceError> {
    let now = DateTime::now();
    let documents = load_reservations(
        store,
        session,
        doc! { "job_id": job_id, "state": "reserved" },
    )
    .await?;
    for document in documents {
        reservations(store)
            .update_one(
                doc! { "_id": &document.id, "state": "reserved" },
                doc! {
                    "$set": {
                        "state": "settled",
                        "spent_value": document.spent_value.max(document.reserved_value),
                        "updated_at": now,
                        "settled_at": now,
                        "expires_at": add_duration(now, RESERVATION_RETENTION),
                        "purge_at": add_duration(now, RESERVATION_RETENTION),
                    }
                },
            )
            .session(&mut *session)
            .await
            .map_err(|error| PersistenceError::mongo("settle unknown generation usage", error))?;
    }
    Ok(())
}

pub(super) async fn release_generation_budget(
    store: &MongoStore,
    session: &mut ClientSession,
    job_id: &str,
) -> Result<(), PersistenceError> {
    let now = DateTime::now();
    reservations(store)
        .update_many(
            doc! { "job_id": job_id, "state": "reserved" },
            doc! {
                "$set": {
                    "state": "released",
                    "updated_at": now,
                    "settled_at": now,
                    "expires_at": add_duration(now, RESERVATION_RETENTION),
                    "purge_at": add_duration(now, RESERVATION_RETENTION),
                }
            },
        )
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("release generation budget", error))?;
    Ok(())
}

impl MongoRepository {
    pub async fn generation_budget_status(
        &self,
        campaign_session_id: &str,
        config: &GenerationGovernanceConfig,
    ) -> Result<GenerationBudgetStatus, GenerationJobStoreError> {
        validate_identifier(campaign_session_id, "campaign id is invalid")?;
        let totals = load_totals_without_session(self.store(), campaign_session_id, None).await?;
        let active = operation(
            self.store(),
            "count active generation reservations",
            reservations(self.store()).count_documents(doc! {
                "campaign_id": campaign_session_id,
                "dimension": "requests",
                "state": "reserved",
                "reserved_value": { "$gt": 0_i64 },
            }),
        )
        .await?;
        let overage_detected = operation(
            self.store(),
            "find generation budget overage",
            reservations(self.store()).find_one(doc! {
                "campaign_id": campaign_session_id,
                "overage": true,
            }),
        )
        .await?
        .is_some();
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
        let mut cursor = operation(
            self.store(),
            "list generation jobs for metrics",
            self.store()
                .collection::<GenerationJobDocument>(CollectionName::GenerationJobs)
                .find(doc! {}),
        )
        .await?;
        let mut buckets =
            BTreeMap::<(String, String, Option<String>), GenerationMetricBucket>::new();
        while cursor
            .advance()
            .await
            .map_err(|error| database("read generation metrics", error))?
        {
            let document = cursor
                .deserialize_current()
                .map_err(|error| database("decode generation metrics", error))?;
            let job = document.to_public()?;
            let key = (
                job.purpose.as_str().to_owned(),
                job.state.as_str().to_owned(),
                job.last_failure_code.map(|code| code.as_str().to_owned()),
            );
            let bucket = buckets.entry(key).or_insert(GenerationMetricBucket {
                purpose: job.purpose,
                state: job.state,
                failure_code: job.last_failure_code,
                count: 0,
                tokens: 0,
                latency_milliseconds: 0,
                cost_microusd: 0,
            });
            bucket.count = bucket.count.saturating_add(1);
            for attempt in document.attempts {
                bucket.tokens = bucket
                    .tokens
                    .saturating_add(stored_nonnegative(attempt.total_tokens)?);
                bucket.latency_milliseconds = bucket
                    .latency_milliseconds
                    .saturating_add(stored_nonnegative(attempt.latency_milliseconds)?);
                bucket.cost_microusd = bucket
                    .cost_microusd
                    .saturating_add(stored_nonnegative(attempt.cost_microusd)?);
            }
        }

        let audits = self
            .store()
            .collection::<BudgetDiagnosticDocument>(CollectionName::AuditEvents);
        let mut rejection_cursor = operation(
            self.store(),
            "list generation budget diagnostics",
            audits
                .find(doc! {
                    "category": "generation_governance",
                    "action": "budget_rejected",
                })
                .projection(doc! {
                    "_id": 0,
                    "metadata.purpose": 1,
                    "metadata.budget_scope": 1,
                    "metadata.budget_dimension": 1,
                }),
        )
        .await?;
        let mut rejections = BTreeMap::<
            (
                GenerationPurpose,
                GenerationBudgetScope,
                GenerationBudgetDimension,
            ),
            u64,
        >::new();
        while rejection_cursor
            .advance()
            .await
            .map_err(|error| database("read generation budget diagnostics", error))?
        {
            let document: BudgetDiagnosticDocument = rejection_cursor
                .deserialize_current()
                .map_err(|error| database("decode generation budget diagnostic", error))?;
            *rejections
                .entry((
                    document.metadata.purpose.parse()?,
                    document.metadata.budget_scope.parse()?,
                    document.metadata.budget_dimension.parse()?,
                ))
                .or_default() += 1;
        }
        Ok(GenerationMetricsSnapshot {
            schema_version: GENERATION_GOVERNANCE_SCHEMA_VERSION,
            jobs: buckets.into_values().collect(),
            budget_rejections: rejections
                .into_iter()
                .map(
                    |((purpose, scope, dimension), count)| GenerationBudgetRejectionMetric {
                        purpose,
                        scope,
                        dimension,
                        count,
                    },
                )
                .collect(),
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
        let now = DateTime::now();
        let jobs = self
            .store()
            .collection::<GenerationJobDocument>(CollectionName::GenerationJobs);
        let mut cursor = operation(
            self.store(),
            "find expired generation jobs",
            jobs.find(doc! {
                "state": { "$in": ["succeeded", "failed", "cancelled"] },
                "purge_at": { "$lte": now },
                "artifact_id": { "$exists": false },
                "pending_artifact_id": { "$exists": false },
            })
            .sort(doc! { "purge_at": 1, "_id": 1 })
            .limit(i64::from(limit)),
        )
        .await?;
        let mut ids = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| database("read expired generation jobs", error))?
        {
            ids.push(
                cursor
                    .deserialize_current()
                    .map_err(|error| database("decode expired generation job", error))?
                    .id,
            );
        }
        let operational_jobs_deleted = if ids.is_empty() {
            0
        } else {
            operation(
                self.store(),
                "delete expired generation jobs",
                jobs.delete_many(doc! { "_id": { "$in": ids } }),
            )
            .await?
            .deleted_count
        };
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let mut diagnostic_cursor = operation(
            self.store(),
            "find expired generation diagnostics",
            audits
                .find(doc! {
                    "category": "generation_governance",
                    "purge_at": { "$lte": now },
                })
                .projection(doc! { "_id": 1 })
                .sort(doc! { "purge_at": 1, "_id": 1 })
                .limit(i64::from(limit)),
        )
        .await?;
        let mut diagnostic_ids = Vec::new();
        while diagnostic_cursor
            .advance()
            .await
            .map_err(|error| database("read expired generation diagnostic", error))?
        {
            let document = diagnostic_cursor
                .deserialize_current()
                .map_err(|error| database("decode expired generation diagnostic", error))?;
            diagnostic_ids.push(
                document
                    .get_str("_id")
                    .map_err(|_| {
                        GenerationJobStoreError::InvalidStoredData(
                            "generation diagnostic id is invalid",
                        )
                    })?
                    .to_owned(),
            );
        }
        let diagnostics_deleted = if diagnostic_ids.is_empty() {
            0
        } else {
            operation(
                self.store(),
                "delete expired generation diagnostics",
                audits.delete_many(doc! {
                    "_id": { "$in": diagnostic_ids },
                    "category": "generation_governance",
                    "purge_at": { "$lte": now },
                }),
            )
            .await?
            .deleted_count
        };
        Ok(GenerationCleanupOutcome {
            operational_jobs_deleted,
            diagnostics_deleted,
        })
    }
}

async fn load_totals(
    store: &MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
    turn_scope_key: Option<&str>,
) -> Result<GenerationBudgetTotals, PersistenceError> {
    let mut filter = doc! {
        "campaign_id": campaign_id,
        "state": { "$in": ["reserved", "settled"] },
    };
    if let Some(turn_scope_key) = turn_scope_key {
        filter.insert("turn_scope_key", turn_scope_key);
    }
    totals_from_documents(load_reservations(store, session, filter).await?)
}

async fn load_totals_without_session(
    store: &MongoStore,
    campaign_id: &str,
    turn_scope_key: Option<&str>,
) -> Result<GenerationBudgetTotals, GenerationJobStoreError> {
    let mut filter = doc! {
        "campaign_id": campaign_id,
        "state": { "$in": ["reserved", "settled"] },
    };
    if let Some(turn_scope_key) = turn_scope_key {
        filter.insert("turn_scope_key", turn_scope_key);
    }
    let mut cursor = operation(
        store,
        "load generation budget totals",
        reservations(store).find(filter),
    )
    .await?;
    let mut documents = Vec::new();
    while cursor
        .advance()
        .await
        .map_err(|error| database("read generation budget totals", error))?
    {
        documents.push(
            cursor
                .deserialize_current()
                .map_err(|error| database("decode generation budget totals", error))?,
        );
    }
    totals_from_documents(documents).map_err(GenerationJobStoreError::Database)
}

fn totals_from_documents(
    documents: Vec<GenerationBudgetReservationDocument>,
) -> Result<GenerationBudgetTotals, PersistenceError> {
    let mut totals = GenerationBudgetTotals::default();
    for document in documents {
        let value = match document.state.as_str() {
            "reserved" => document.reserved_value.max(document.spent_value),
            "settled" => document.spent_value,
            "released" => 0,
            _ => {
                return Err(PersistenceError::SchemaDrift {
                    collection: CollectionName::GenerationBudgetReservations
                        .as_str()
                        .to_owned(),
                    detail: "stored generation reservation state is invalid".to_owned(),
                });
            }
        };
        let value = u64::try_from(value).map_err(|_| PersistenceError::SchemaDrift {
            collection: CollectionName::GenerationBudgetReservations
                .as_str()
                .to_owned(),
            detail: "stored generation budget value is negative".to_owned(),
        })?;
        match document.dimension.parse::<GenerationBudgetDimension>() {
            Ok(GenerationBudgetDimension::Requests) => {
                totals.requests = totals.requests.saturating_add(value)
            }
            Ok(GenerationBudgetDimension::Tokens) => {
                totals.tokens = totals.tokens.saturating_add(value)
            }
            Ok(GenerationBudgetDimension::Latency) => {
                totals.latency_milliseconds = totals.latency_milliseconds.saturating_add(value)
            }
            Ok(GenerationBudgetDimension::Cost) => {
                totals.cost_microusd = totals.cost_microusd.saturating_add(value)
            }
            Ok(GenerationBudgetDimension::Concurrency) => {}
            Err(_) => {
                return Err(PersistenceError::SchemaDrift {
                    collection: CollectionName::GenerationBudgetReservations
                        .as_str()
                        .to_owned(),
                    detail: "stored generation budget dimension is invalid".to_owned(),
                });
            }
        }
    }
    Ok(totals)
}

async fn load_reservations(
    store: &MongoStore,
    session: &mut ClientSession,
    filter: mongodb::bson::Document,
) -> Result<Vec<GenerationBudgetReservationDocument>, PersistenceError> {
    let mut cursor = reservations(store)
        .find(filter)
        .sort(doc! { "dimension": 1 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load generation reservations", error))?;
    let mut documents = Vec::new();
    while cursor
        .advance(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("read generation reservations", error))?
    {
        documents.push(
            cursor
                .deserialize_current()
                .map_err(|error| PersistenceError::mongo("decode generation reservation", error))?,
        );
    }
    Ok(documents)
}

fn receipt_from_reservations(
    documents: Vec<GenerationBudgetReservationDocument>,
) -> Result<Option<GenerationGovernanceReceipt>, PersistenceError> {
    let Some(first) = documents.first() else {
        return Ok(None);
    };
    if documents.len() != 4
        || documents.iter().any(|document| {
            document.job_id != first.job_id
                || document.campaign_id != first.campaign_id
                || document.purpose != first.purpose
                || document.idempotency_key != first.idempotency_key
                || document.turn_scope_key != first.turn_scope_key
                || document.request_fingerprint != first.request_fingerprint
                || document.policy_fingerprint != first.policy_fingerprint
                || document.config_fingerprint != first.config_fingerprint
                || document.governance_fingerprint != first.governance_fingerprint
                || document.state != first.state
        })
    {
        return Err(PersistenceError::SchemaDrift {
            collection: CollectionName::GenerationBudgetReservations
                .as_str()
                .to_owned(),
            detail: "generation reservation dimensions do not form one receipt".to_owned(),
        });
    }
    let mut reserved = BTreeMap::new();
    let mut spent = BTreeMap::new();
    for document in &documents {
        let dimension = document
            .dimension
            .parse()
            .map_err(|_| PersistenceError::SchemaDrift {
                collection: CollectionName::GenerationBudgetReservations
                    .as_str()
                    .to_owned(),
                detail: "generation reservation dimension is invalid".to_owned(),
            })?;
        reserved.insert(dimension, document.reserved_value);
        spent.insert(dimension, document.spent_value);
    }
    let dimension_value = |values: &BTreeMap<GenerationBudgetDimension, i64>, dimension| {
        values
            .get(&dimension)
            .copied()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| PersistenceError::SchemaDrift {
                collection: CollectionName::GenerationBudgetReservations
                    .as_str()
                    .to_owned(),
                detail: "generation reservation value is invalid".to_owned(),
            })
    };
    Ok(Some(GenerationGovernanceReceipt {
        campaign_session_id: first.campaign_id.clone(),
        purpose: first.purpose.parse().map_err(domain_as_persistence)?,
        idempotency_key: first.idempotency_key.clone(),
        job_id: first.job_id.clone(),
        origin_turn_id: first.origin_turn_id.clone(),
        turn_scope_key: first.turn_scope_key.clone(),
        request_fingerprint: digest(&first.request_fingerprint)?,
        policy_fingerprint: digest(&first.policy_fingerprint)?,
        config_fingerprint: digest(&first.config_fingerprint)?,
        governance_fingerprint: digest(&first.governance_fingerprint)?,
        state: first.state.parse().map_err(domain_as_persistence)?,
        reserved_requests: u8::try_from(dimension_value(
            &reserved,
            GenerationBudgetDimension::Requests,
        )?)
        .map_err(|_| schema_error("reserved request value is invalid"))?,
        reserved_tokens: dimension_value(&reserved, GenerationBudgetDimension::Tokens)?,
        reserved_latency_milliseconds: dimension_value(
            &reserved,
            GenerationBudgetDimension::Latency,
        )?,
        reserved_cost_microusd: dimension_value(&reserved, GenerationBudgetDimension::Cost)?,
        spent_requests: u8::try_from(dimension_value(
            &spent,
            GenerationBudgetDimension::Requests,
        )?)
        .map_err(|_| schema_error("spent request value is invalid"))?,
        spent_tokens: dimension_value(&spent, GenerationBudgetDimension::Tokens)?,
        spent_latency_milliseconds: dimension_value(&spent, GenerationBudgetDimension::Latency)?,
        spent_cost_microusd: dimension_value(&spent, GenerationBudgetDimension::Cost)?,
        overage: documents.iter().any(|document| document.overage),
        created_at: date_string(first.created_at)?,
        updated_at: date_string(first.updated_at)?,
        settled_at: first.settled_at.map(date_string).transpose()?,
    }))
}

async fn record_budget_diagnostic(
    store: &MongoStore,
    session: &mut ClientSession,
    campaign_id: &str,
    purpose: GenerationPurpose,
    scope: GenerationBudgetScope,
    dimension: GenerationBudgetDimension,
) -> Result<(), PersistenceError> {
    let now = DateTime::now();
    store
        .document_collection(CollectionName::AuditEvents)
        .insert_one(doc! {
            "_id": format!("audit:{}", Uuid::new_v4()),
            "schema_version": 1_i32,
            "category": "generation_governance",
            "action": "budget_rejected",
            "outcome": "denied",
            "scope_kind": "campaign",
            "scope_id": campaign_id,
            "correlation_id": format!("correlation:{}", Uuid::new_v4()),
            "metadata": {
                "purpose": purpose.as_str(),
                "budget_scope": scope.as_str(),
                "budget_dimension": dimension.as_str(),
            },
            "created_at": now,
            "purge_at": add_duration(now, DIAGNOSTIC_RETENTION),
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("record generation budget diagnostic", error))?;
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

fn reservations(store: &MongoStore) -> Collection<GenerationBudgetReservationDocument> {
    store.collection(CollectionName::GenerationBudgetReservations)
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
        .map_err(|error| database(operation, error))
}

fn database(operation: &'static str, error: mongodb::error::Error) -> GenerationJobStoreError {
    GenerationJobStoreError::Database(PersistenceError::mongo(operation, error))
}

fn domain_as_persistence(error: GenerationJobStoreError) -> PersistenceError {
    schema_error(match error {
        GenerationJobStoreError::NumericRange => "generation numeric value is outside BSON range",
        _ => "stored generation governance metadata is invalid",
    })
}

fn schema_error(detail: &str) -> PersistenceError {
    PersistenceError::SchemaDrift {
        collection: CollectionName::GenerationBudgetReservations
            .as_str()
            .to_owned(),
        detail: detail.to_owned(),
    }
}

fn digest(value: &str) -> Result<Sha256Digest, PersistenceError> {
    Sha256Digest::new(value.to_owned())
        .map_err(|_| schema_error("stored generation governance digest is invalid"))
}

fn status_line(used: u64, limit: u64) -> GenerationBudgetStatusLine {
    GenerationBudgetStatusLine { used, limit }
}

fn checked_add(left: u64, right: u64) -> Result<u64, GenerationJobStoreError> {
    left.checked_add(right)
        .ok_or(GenerationJobStoreError::NumericRange)
}

fn stored_nonnegative(value: Option<i64>) -> Result<u64, GenerationJobStoreError> {
    u64::try_from(value.unwrap_or(0)).map_err(|_| {
        GenerationJobStoreError::InvalidStoredData("stored generation metric value is negative")
    })
}

fn to_i64(value: u64) -> Result<i64, GenerationJobStoreError> {
    i64::try_from(value).map_err(|_| GenerationJobStoreError::NumericRange)
}

fn validate_identifier(value: &str, reason: &'static str) -> Result<(), GenerationJobStoreError> {
    if is_valid_opaque_id(value) {
        Ok(())
    } else {
        Err(GenerationJobStoreError::InvalidInput(reason))
    }
}

fn date_string(value: DateTime) -> Result<String, PersistenceError> {
    value
        .try_to_rfc3339_string()
        .map_err(|_| schema_error("stored BSON date is outside RFC 3339 range"))
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

    #[test]
    fn budget_math_is_conservative_in_every_dimension() {
        let current = GenerationBudgetTotals {
            requests: 1,
            tokens: 90,
            latency_milliseconds: 900,
            cost_microusd: 9,
        };
        let addition = GenerationBudgetTotals {
            requests: 1,
            tokens: 20,
            latency_milliseconds: 50,
            cost_microusd: 1,
        };
        let limits = GenerationBudgetAllowance {
            requests: 2,
            tokens: 100,
            latency_milliseconds: 1_000,
            cost_microusd: 10,
        };
        assert_eq!(
            exceeds(current, addition, limits).unwrap(),
            Some(GenerationBudgetDimension::Tokens)
        );
    }

    #[test]
    fn governance_replay_binds_all_reserved_values_and_fingerprints() {
        let digest = Sha256Digest::from_bytes([7; 32]);
        let requested = NewGenerationGovernanceReceipt {
            turn_scope_key: "turn:scope".to_owned(),
            request_fingerprint: digest.clone(),
            policy_fingerprint: digest.clone(),
            config_fingerprint: digest.clone(),
            governance_fingerprint: digest.clone(),
            reserved_requests: 1,
            reserved_tokens: 10,
            reserved_latency_milliseconds: 20,
            reserved_cost_microusd: 30,
            limits: GenerationGovernanceConfig {
                campaign: GenerationBudgetAllowance {
                    requests: 5,
                    tokens: 100,
                    latency_milliseconds: 100,
                    cost_microusd: 100,
                },
                turn: GenerationBudgetAllowance {
                    requests: 5,
                    tokens: 100,
                    latency_milliseconds: 100,
                    cost_microusd: 100,
                },
                max_campaign_concurrency: 1,
                worker_batch_size: 1,
            },
        };
        let existing = GenerationGovernanceReceipt {
            campaign_session_id: "campaign:test".to_owned(),
            purpose: GenerationPurpose::Narration,
            idempotency_key: "key:test".to_owned(),
            job_id: "generation-job:test".to_owned(),
            origin_turn_id: Some("turn:test".to_owned()),
            turn_scope_key: requested.turn_scope_key.clone(),
            request_fingerprint: digest.clone(),
            policy_fingerprint: digest.clone(),
            config_fingerprint: digest.clone(),
            governance_fingerprint: digest,
            state: GenerationGovernanceState::Reserved,
            reserved_requests: 1,
            reserved_tokens: 10,
            reserved_latency_milliseconds: 20,
            reserved_cost_microusd: 30,
            spent_requests: 0,
            spent_tokens: 0,
            spent_latency_milliseconds: 0,
            spent_cost_microusd: 0,
            overage: false,
            created_at: "created".to_owned(),
            updated_at: "updated".to_owned(),
            settled_at: None,
        };
        assert!(ensure_matching_governance_receipt(&existing, &requested).is_ok());
        let mut drift = requested;
        drift.reserved_tokens += 1;
        assert!(matches!(
            ensure_matching_governance_receipt(&existing, &drift),
            Err(GenerationJobStoreError::IdempotencyConflict)
        ));
    }
}
