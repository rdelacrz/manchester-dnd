//! Request-scoped bridge from typed generation to the durable metadata queue.
//!
//! The bridge accepts only digests and bounded identifiers. Prompt bodies,
//! player intent, provider responses, and credentials stay in memory. An exact
//! job claim prevents an inline request from leasing unrelated queued work.

use std::time::{Duration, Instant};

use manchester_dnd_core::{
    CommittedEncounterOutcomeDto, SessionEventPayload, Sha256Digest, ai_turn::TypedGmProposal,
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    application::LOCAL_HERO_OWNER_KEY,
    config::{GenerationGovernanceConfig, LlmBackend, LlmProfile},
    repository::{
        GeneratedTextPresentation, GeneratedTextPresentationReplay,
        GeneratedTextPresentationSource, MongoRepository, NewGeneratedTextPresentation,
        NewGenerationGovernanceReceipt, NewTypedIntentCommandReceipt, TextPresentationStoreError,
        TypedIntentCommandReceipt,
        jobs::{
            GenerationAttemptFailure, GenerationAttemptFinish, GenerationFailureCode,
            GenerationJobState, GenerationPurpose, GenerationSuccess, GenerationUsage,
            NewGenerationJob, SuccessRetention,
        },
    },
    typed_gm::{
        GenerationFailureClass as TypedFailureClass, TypedGmTurnResult, TypedProposalSource,
    },
};

#[derive(Debug, Error)]
pub enum GenerationLedgerError {
    #[error("generation metadata could not be persisted")]
    Store(#[from] crate::repository::jobs::GenerationJobStoreError),
    #[error("generation origin audit could not be loaded")]
    Repository(#[from] crate::error::RepositoryError),
    #[error("generation job is already {state}")]
    AlreadyHandled { state: GenerationJobState },
    #[error("generation job was not available for its exact inline worker")]
    ExactJobUnavailable,
    #[error("generation origin turn is unavailable")]
    OriginTurnUnavailable,
    #[error("the committed encounter outcome for the presentation turn is unavailable")]
    OriginOutcomeUnavailable,
    #[error("generated text presentation could not be persisted")]
    Presentation(#[from] TextPresentationStoreError),
    #[error("generated text presentation does not match the validated narration proposal")]
    InvalidPresentation,
    #[error("typed player intent idempotency key was reused for different text")]
    TypedIntentIdempotencyConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineGenerationRequest {
    pub campaign_session_id: String,
    pub origin_turn_id: Option<String>,
    pub origin_campaign_revision: u64,
    pub purpose: GenerationPurpose,
    pub idempotency_key: String,
    pub input_digest: Sha256Digest,
    pub prompt_digest: Sha256Digest,
    pub policy_digest: Sha256Digest,
    pub config_digest: Sha256Digest,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTypedIntentCommandRequest {
    pub campaign_session_id: String,
    pub client_idempotency_key: String,
    pub player_intent: String,
    pub expected_campaign_revision: u64,
    pub expected_encounter_revision: u64,
    pub resolved_intent: manchester_dnd_core::encounter::EncounterIntent,
    pub interpretation_label: String,
    pub interpretation_evidence_json: String,
}

#[derive(Debug, Clone)]
pub struct InlineGenerationAttempt {
    pub job_id: String,
    pub attempt_id: String,
    campaign_session_id: String,
    origin_turn_id: Option<String>,
    prompt_digest: Sha256Digest,
    policy_digest: Sha256Digest,
    config_digest: Sha256Digest,
    lease: crate::repository::jobs::GenerationLease,
    started_at: Instant,
    metered_provider_request: bool,
    estimated_request_cost_microusd: u64,
}

struct PresentationCompletion<'a> {
    source: GeneratedTextPresentationSource,
    body: &'a str,
    client_idempotency_key: &'a str,
    output_digest: Sha256Digest,
    usage: &'a GenerationUsage,
    failure: Option<GenerationFailureCode>,
    private_inspiration_work_id: Option<&'a str>,
}

#[derive(Clone)]
pub struct InlineGenerationLedger {
    repository: MongoRepository,
    provider: String,
    model: String,
    lease_duration: Duration,
    governance: GenerationGovernanceConfig,
    governance_fingerprint: Sha256Digest,
    reserved_requests: u8,
    reserved_tokens: u64,
    reserved_latency_milliseconds: u64,
    reserved_cost_microusd: u64,
}

impl InlineGenerationLedger {
    pub fn new(
        repository: MongoRepository,
        profile: &LlmProfile,
        governance: &GenerationGovernanceConfig,
    ) -> Self {
        let provider = match profile.backend {
            LlmBackend::Disabled => "disabled",
            LlmBackend::Fake => "deterministic-fake",
            LlmBackend::OpenAiCompatible => "openai-compatible",
        }
        .to_owned();
        let model = profile
            .model
            .clone()
            .unwrap_or_else(|| match profile.backend {
                LlmBackend::Disabled => "disabled".to_owned(),
                LlmBackend::Fake => "deterministic-fake-v1".to_owned(),
                LlmBackend::OpenAiCompatible => "configured-model".to_owned(),
            });
        // The typed service has its own purpose deadline. Keep enough lease
        // slack for validation and the final database transaction even when a
        // profile configures a very short transport timeout.
        let lease_duration = profile
            .timeout
            .saturating_add(Duration::from_secs(15))
            .clamp(Duration::from_secs(30), Duration::from_secs(300));
        Self {
            repository,
            provider,
            model,
            lease_duration,
            governance: governance.clone(),
            governance_fingerprint: governance.non_secret_fingerprint(),
            reserved_requests: u8::try_from(profile.estimated_request_units())
                .expect("a generation profile reserves at most one request"),
            reserved_tokens: profile.estimated_request_tokens(),
            reserved_latency_milliseconds: profile.estimated_request_latency_milliseconds(),
            reserved_cost_microusd: profile.estimated_request_cost_microusd,
        }
    }

    /// Enqueues and leases the exact job for this request. Inline typed turns
    /// are one durable attempt because their body is intentionally not stored;
    /// provider-internal repair attempts remain visible in usage/evidence.
    pub async fn begin(
        &self,
        request: InlineGenerationRequest,
    ) -> Result<InlineGenerationAttempt, GenerationLedgerError> {
        let turn_scope_key = request
            .origin_turn_id
            .clone()
            .unwrap_or_else(|| format!("campaign-revision:{}", request.origin_campaign_revision));
        let new_job = NewGenerationJob {
            id: format!("generation-job:{}", Uuid::new_v4().simple()),
            campaign_session_id: request.campaign_session_id.clone(),
            origin_turn_id: request.origin_turn_id,
            origin_campaign_revision: request.origin_campaign_revision,
            purpose: request.purpose,
            idempotency_key: request.idempotency_key,
            input_digest: request.input_digest.clone(),
            prompt_digest: request.prompt_digest,
            policy_digest: request.policy_digest.clone(),
            config_digest: request.config_digest.clone(),
            correlation_id: Some(request.correlation_id),
            max_attempts: 1,
            success_retention: SuccessRetention::UnselectedPresentation30Days,
            governance: Some(NewGenerationGovernanceReceipt {
                turn_scope_key,
                request_fingerprint: request.input_digest.clone(),
                policy_fingerprint: request.policy_digest.clone(),
                config_fingerprint: request.config_digest.clone(),
                governance_fingerprint: self.governance_fingerprint.clone(),
                reserved_requests: self.reserved_requests,
                reserved_tokens: self.reserved_tokens,
                reserved_latency_milliseconds: self.reserved_latency_milliseconds,
                reserved_cost_microusd: self.reserved_cost_microusd,
                limits: self.governance.clone(),
            }),
        };
        let queued = self.repository.enqueue_generation_job(&new_job).await?;
        let job = queued.job();
        if job.state != GenerationJobState::Queued {
            return Err(GenerationLedgerError::AlreadyHandled { state: job.state });
        }
        let claim = crate::repository::jobs::GenerationClaim {
            worker_id: "worker:inline-typed-gm".to_owned(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            lease_duration: self.lease_duration,
        };
        let claimed = self
            .repository
            .claim_generation_job_by_id(&job.campaign_session_id, &job.id, &claim)
            .await?
            .ok_or(GenerationLedgerError::ExactJobUnavailable)?;
        Ok(InlineGenerationAttempt {
            job_id: claimed.job.id,
            attempt_id: claimed.attempt.id,
            campaign_session_id: claimed.job.campaign_session_id,
            origin_turn_id: claimed.job.origin_turn_id,
            prompt_digest: claimed.job.prompt_digest,
            policy_digest: claimed.job.policy_digest,
            config_digest: claimed.job.config_digest,
            lease: claimed.lease,
            started_at: Instant::now(),
            metered_provider_request: self.reserved_requests > 0,
            estimated_request_cost_microusd: self.reserved_cost_microusd,
        })
    }

    /// Resolves a committed event sequence to its immutable audit identity so
    /// narration and image jobs can retain a real origin link without exposing
    /// repository internals to the UI layer.
    pub async fn origin_turn_id(
        &self,
        campaign_session_id: &str,
        event_sequence: u64,
    ) -> Result<String, GenerationLedgerError> {
        self.repository
            .list_session_events(LOCAL_HERO_OWNER_KEY, campaign_session_id)
            .await?
            .into_iter()
            .find(|audit| audit.turn_number == event_sequence)
            .map(|audit| audit.id)
            .ok_or(GenerationLedgerError::OriginTurnUnavailable)
    }

    pub async fn committed_encounter_outcome(
        &self,
        campaign_session_id: &str,
        event_sequence: u64,
    ) -> Result<CommittedEncounterOutcomeDto, GenerationLedgerError> {
        self.repository
            .list_session_events(LOCAL_HERO_OWNER_KEY, campaign_session_id)
            .await?
            .into_iter()
            .find_map(|audit| {
                if audit.turn_number != event_sequence {
                    return None;
                }
                match audit.payload.payload {
                    SessionEventPayload::EncounterResolved { outcome, .. } => Some(*outcome),
                    _ => None,
                }
            })
            .ok_or(GenerationLedgerError::OriginOutcomeUnavailable)
    }

    pub async fn finish_typed(
        &self,
        attempt: &InlineGenerationAttempt,
        result: &TypedGmTurnResult,
    ) -> Result<(), GenerationLedgerError> {
        let usage = GenerationUsage {
            prompt_tokens: result.usage.prompt_tokens,
            completion_tokens: result.usage.completion_tokens,
            total_tokens: result.usage.total_tokens,
            cost_microusd: attempt
                .metered_provider_request
                .then_some(attempt.estimated_request_cost_microusd),
            latency_milliseconds: attempt_latency(attempt),
        };
        let finish = match result.source {
            TypedProposalSource::Provider => {
                GenerationAttemptFinish::Succeeded(GenerationSuccess {
                    artifact_id: None,
                    output_digest: result.proposal_fingerprint.clone(),
                    usage,
                })
            }
            TypedProposalSource::AuthoredFallback => {
                let code = result
                    .failure
                    .map(map_failure)
                    .unwrap_or(GenerationFailureCode::ProviderUnavailable);
                GenerationAttemptFinish::Failed(GenerationAttemptFailure {
                    code,
                    provider_status: None,
                    provider_request_id: None,
                    usage,
                    output_digest: Some(result.proposal_fingerprint.clone()),
                })
            }
        };
        self.repository
            .finish_generation_attempt(&attempt.lease, finish)
            .await?;
        Ok(())
    }

    /// Atomically completes a validated narration attempt and selects the new
    /// owner-visible presentation version. The proposal body itself is the only
    /// provider-derived text admitted to storage; raw responses remain absent.
    pub async fn finish_typed_presentation(
        &self,
        attempt: &InlineGenerationAttempt,
        result: &TypedGmTurnResult,
        body: &str,
        client_idempotency_key: &str,
        private_inspiration_work_id: Option<&str>,
    ) -> Result<GeneratedTextPresentation, GenerationLedgerError> {
        let TypedGmProposal::Narration(narration) = &result.proposal else {
            return Err(GenerationLedgerError::InvalidPresentation);
        };
        if narration.text != body
            || result.prompt_fingerprint != attempt.prompt_digest
            || result.policy_fingerprint != attempt.policy_digest
            || result.config_fingerprint != attempt.config_digest
        {
            return Err(GenerationLedgerError::InvalidPresentation);
        }
        let source = match result.source {
            TypedProposalSource::Provider => GeneratedTextPresentationSource::Provider,
            TypedProposalSource::AuthoredFallback => {
                GeneratedTextPresentationSource::AuthoredFallback
            }
        };
        let failure = result.failure.map(map_failure);
        let usage = GenerationUsage {
            prompt_tokens: result.usage.prompt_tokens,
            completion_tokens: result.usage.completion_tokens,
            total_tokens: result.usage.total_tokens,
            cost_microusd: attempt
                .metered_provider_request
                .then_some(attempt.estimated_request_cost_microusd),
            latency_milliseconds: attempt_latency(attempt),
        };
        self.finish_presentation(
            attempt,
            PresentationCompletion {
                source,
                body,
                client_idempotency_key,
                output_digest: result.proposal_fingerprint.clone(),
                usage: &usage,
                failure,
                private_inspiration_work_id,
            },
        )
        .await
    }

    /// Records deterministic engine prose when external prose is disabled or
    /// intentionally blocked by the fail-closed deployment policy.
    pub async fn finish_engine_authored_presentation(
        &self,
        attempt: &InlineGenerationAttempt,
        body: &str,
        client_idempotency_key: &str,
        failure: GenerationFailureCode,
    ) -> Result<GeneratedTextPresentation, GenerationLedgerError> {
        let usage = GenerationUsage {
            cost_microusd: attempt
                .metered_provider_request
                .then_some(attempt.estimated_request_cost_microusd),
            latency_milliseconds: attempt_latency(attempt),
            ..GenerationUsage::default()
        };
        self.finish_presentation(
            attempt,
            PresentationCompletion {
                source: GeneratedTextPresentationSource::EngineAuthored,
                body,
                client_idempotency_key,
                output_digest: fingerprint_presentation_body(body),
                usage: &usage,
                failure: Some(failure),
                private_inspiration_work_id: None,
            },
        )
        .await
    }

    pub async fn presentations_for_turn(
        &self,
        campaign_session_id: &str,
        event_sequence: u64,
    ) -> Result<Vec<GeneratedTextPresentation>, GenerationLedgerError> {
        let origin_turn_id = self
            .origin_turn_id(campaign_session_id, event_sequence)
            .await?;
        Ok(self
            .repository
            .list_generated_text_presentations(campaign_session_id, &origin_turn_id)
            .await?)
    }

    pub async fn presentation_version_count(
        &self,
        campaign_session_id: &str,
        event_sequence: u64,
    ) -> Result<u8, GenerationLedgerError> {
        let origin_turn_id = self
            .origin_turn_id(campaign_session_id, event_sequence)
            .await?;
        Ok(self
            .repository
            .generated_text_presentation_version_count(campaign_session_id, &origin_turn_id)
            .await?)
    }

    pub async fn presentation_for_client_key(
        &self,
        campaign_session_id: &str,
        event_sequence: u64,
        client_idempotency_key: &str,
    ) -> Result<Option<GeneratedTextPresentation>, GenerationLedgerError> {
        let origin_turn_id = self
            .origin_turn_id(campaign_session_id, event_sequence)
            .await?;
        Ok(self
            .repository
            .load_generated_text_presentation_by_client_key(
                campaign_session_id,
                &origin_turn_id,
                client_idempotency_key,
            )
            .await?)
    }

    pub async fn presentation_replay_for_client_key(
        &self,
        campaign_session_id: &str,
        event_sequence: u64,
        client_idempotency_key: &str,
    ) -> Result<Option<GeneratedTextPresentationReplay>, GenerationLedgerError> {
        let origin_turn_id = self
            .origin_turn_id(campaign_session_id, event_sequence)
            .await?;
        Ok(self
            .repository
            .load_generated_text_presentation_replay(
                campaign_session_id,
                &origin_turn_id,
                client_idempotency_key,
            )
            .await?)
    }

    /// Loads a body-free typed-command recovery record before any current
    /// revision or provider work. Reusing the key with different normalized
    /// text fails explicitly.
    pub async fn typed_intent_command_receipt(
        &self,
        campaign_session_id: &str,
        client_idempotency_key: &str,
        player_intent: &str,
    ) -> Result<Option<TypedIntentCommandReceipt>, GenerationLedgerError> {
        let expected_digest = fingerprint_player_intent(player_intent);
        let receipt = self
            .repository
            .load_typed_intent_command_receipt(campaign_session_id, client_idempotency_key)
            .await?;
        if receipt
            .as_ref()
            .is_some_and(|receipt| receipt.player_intent_digest != expected_digest)
        {
            return Err(GenerationLedgerError::TypedIntentIdempotencyConflict);
        }
        Ok(receipt)
    }

    pub async fn insert_pending_typed_intent_command_receipt(
        &self,
        request: PendingTypedIntentCommandRequest,
    ) -> Result<TypedIntentCommandReceipt, GenerationLedgerError> {
        Ok(self
            .repository
            .insert_pending_typed_intent_command_receipt(&NewTypedIntentCommandReceipt {
                campaign_session_id: request.campaign_session_id,
                client_idempotency_key: request.client_idempotency_key,
                player_intent_digest: fingerprint_player_intent(&request.player_intent),
                expected_campaign_revision: request.expected_campaign_revision,
                expected_encounter_revision: request.expected_encounter_revision,
                resolved_intent: request.resolved_intent,
                interpretation_label: request.interpretation_label,
                interpretation_evidence_json: request.interpretation_evidence_json,
            })
            .await?)
    }

    pub async fn commit_typed_intent_command_receipt(
        &self,
        receipt: &TypedIntentCommandReceipt,
        player_intent: &str,
        event_sequence: u64,
        result_campaign_revision: u64,
    ) -> Result<TypedIntentCommandReceipt, GenerationLedgerError> {
        let player_intent_digest = fingerprint_player_intent(player_intent);
        if receipt.player_intent_digest != player_intent_digest {
            return Err(GenerationLedgerError::TypedIntentIdempotencyConflict);
        }
        let origin_turn_id = self
            .origin_turn_id(&receipt.campaign_session_id, event_sequence)
            .await?;
        Ok(self
            .repository
            .commit_typed_intent_command_receipt(
                &receipt.campaign_session_id,
                &receipt.client_idempotency_key,
                &player_intent_digest,
                &origin_turn_id,
                event_sequence,
                result_campaign_revision,
            )
            .await?)
    }

    async fn finish_presentation(
        &self,
        attempt: &InlineGenerationAttempt,
        completion: PresentationCompletion<'_>,
    ) -> Result<GeneratedTextPresentation, GenerationLedgerError> {
        let origin_turn_id = attempt
            .origin_turn_id
            .clone()
            .ok_or(GenerationLedgerError::OriginTurnUnavailable)?;
        Ok(self
            .repository
            .finish_generation_with_text_presentation(
                &attempt.lease,
                &NewGeneratedTextPresentation {
                    id: format!("text-presentation:{}", Uuid::new_v4().simple()),
                    campaign_session_id: attempt.campaign_session_id.clone(),
                    origin_turn_id,
                    generation_job_id: attempt.job_id.clone(),
                    generation_attempt_id: attempt.attempt_id.clone(),
                    client_idempotency_key: completion.client_idempotency_key.to_owned(),
                    source: completion.source,
                    body: completion.body.to_owned(),
                    config_digest: attempt.config_digest.clone(),
                    prompt_digest: attempt.prompt_digest.clone(),
                    policy_digest: attempt.policy_digest.clone(),
                    output_digest: completion.output_digest,
                    private_inspiration_work_id: completion
                        .private_inspiration_work_id
                        .map(str::to_owned),
                },
                completion.usage,
                completion.failure,
            )
            .await?)
    }

    /// Closes a leased attempt when the typed service itself cannot produce
    /// even its validated authored fallback. No prompt or response body is
    /// retained with the operational failure.
    pub async fn finish_unavailable(
        &self,
        attempt: &InlineGenerationAttempt,
    ) -> Result<(), GenerationLedgerError> {
        let usage = GenerationUsage {
            cost_microusd: attempt
                .metered_provider_request
                .then_some(attempt.estimated_request_cost_microusd),
            latency_milliseconds: attempt_latency(attempt),
            ..GenerationUsage::default()
        };
        self.repository
            .finish_generation_attempt(
                &attempt.lease,
                GenerationAttemptFinish::Failed(GenerationAttemptFailure {
                    code: GenerationFailureCode::ProviderUnavailable,
                    provider_status: None,
                    provider_request_id: None,
                    usage,
                    output_digest: None,
                }),
            )
            .await?;
        Ok(())
    }
}

fn attempt_latency(attempt: &InlineGenerationAttempt) -> Option<u64> {
    attempt
        .metered_provider_request
        .then(|| u64::try_from(attempt.started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
}

fn fingerprint_presentation_body(body: &str) -> Sha256Digest {
    let mut hasher = Sha256::new();
    let domain = b"generated-text-presentation/v1";
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    hasher.update((body.len() as u64).to_be_bytes());
    hasher.update(body.as_bytes());
    Sha256Digest::from_bytes(hasher.finalize().into())
}

fn fingerprint_player_intent(player_intent: &str) -> Sha256Digest {
    let normalized = player_intent.trim();
    let mut hasher = Sha256::new();
    let domain = b"typed-player-intent/v1";
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    hasher.update((normalized.len() as u64).to_be_bytes());
    hasher.update(normalized.as_bytes());
    Sha256Digest::from_bytes(hasher.finalize().into())
}

const fn map_failure(failure: TypedFailureClass) -> GenerationFailureCode {
    match failure {
        TypedFailureClass::Timeout => GenerationFailureCode::Timeout,
        TypedFailureClass::Unavailable => GenerationFailureCode::ProviderUnavailable,
        TypedFailureClass::RateLimit => GenerationFailureCode::RateLimited,
        TypedFailureClass::Malformed => GenerationFailureCode::MalformedResponse,
        TypedFailureClass::Unsafe => GenerationFailureCode::UnsafeOutput,
        TypedFailureClass::Contradiction => GenerationFailureCode::Contradiction,
    }
}

#[cfg(test)]
mod tests {
    use manchester_dnd_core::{RULESET, SESSION_SCHEMA_VERSION, SessionDto, SessionStatus};
    use mongodb::bson::{DateTime, doc};

    use super::*;
    use crate::{
        config::{MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::{CollectionName, MongoStore, SchemaReconciler},
    };

    fn disabled_profile() -> LlmProfile {
        LlmProfile {
            backend: LlmBackend::Disabled,
            base_url: None,
            api_key: None,
            model: None,
            timeout: Duration::from_secs(20),
            max_output_tokens: Some(2_048),
            temperature: Some(0.0),
            default_image_size: None,
            estimated_request_cost_microusd: 0,
        }
    }

    fn governance_config() -> GenerationGovernanceConfig {
        let allowance = crate::config::GenerationBudgetAllowance {
            requests: 8,
            tokens: 100_000,
            latency_milliseconds: 60_000,
            cost_microusd: 0,
        };
        GenerationGovernanceConfig {
            campaign: allowance,
            turn: allowance,
            max_campaign_concurrency: 2,
            worker_batch_size: 2,
        }
    }

    fn request() -> InlineGenerationRequest {
        InlineGenerationRequest {
            campaign_session_id: "campaign-ledger".to_owned(),
            origin_turn_id: None,
            origin_campaign_revision: 1,
            purpose: GenerationPurpose::IntentParsing,
            idempotency_key: "typed-intent:request-1".to_owned(),
            input_digest: Sha256Digest::from_bytes([1; 32]),
            prompt_digest: Sha256Digest::from_bytes([2; 32]),
            policy_digest: Sha256Digest::from_bytes([3; 32]),
            config_digest: Sha256Digest::from_bytes([4; 32]),
            correlation_id: "correlation:ledger-test".to_owned(),
        }
    }

    async fn isolated_mongo_repository() -> Option<(MongoRepository, MongoStore, String)> {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping generation-ledger MongoDB test: MONGODB_TEST_URI is not set");
            return None;
        };
        assert!(
            uri.starts_with("mongodb://root:") && uri.contains("127.0.0.1"),
            "MONGODB_TEST_URI must be the explicit local root test URI"
        );
        let database = format!("mdnd_generation_ledger_test_{}", Uuid::new_v4().simple());
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

    async fn seed_campaign(repository: &MongoRepository, store: &MongoStore, revision: u64) {
        repository
            .create_campaign(
                LOCAL_HERO_OWNER_KEY,
                &SessionDto {
                    schema_version: SESSION_SCHEMA_VERSION,
                    id: "campaign-ledger".to_owned(),
                    ruleset: RULESET,
                    title: "Generation ledger test".to_owned(),
                    status: SessionStatus::Active,
                    character_ids: Vec::new(),
                    created_at_unix_ms: 1,
                    updated_at_unix_ms: 1,
                    last_event_sequence: 0,
                },
                &[],
            )
            .await
            .unwrap();
        if revision != 1 {
            store
                .document_collection(CollectionName::Campaigns)
                .update_one(
                    doc! { "_id": "campaign-ledger" },
                    doc! { "$set": { "revision": i64::try_from(revision).unwrap() } },
                )
                .await
                .unwrap();
        }
    }

    async fn drop_test_database(store: &MongoStore, database: &str) {
        assert!(
            database.starts_with("mdnd_generation_ledger_test_") && database != "manchester_dnd",
            "cleanup safeguard"
        );
        store.database().drop().await.unwrap();
    }

    #[test]
    fn typed_failures_map_to_the_closed_durable_code_set() {
        assert_eq!(
            map_failure(TypedFailureClass::Timeout),
            GenerationFailureCode::Timeout
        );
        assert_eq!(
            map_failure(TypedFailureClass::Unavailable),
            GenerationFailureCode::ProviderUnavailable
        );
        assert_eq!(
            map_failure(TypedFailureClass::RateLimit),
            GenerationFailureCode::RateLimited
        );
        assert_eq!(
            map_failure(TypedFailureClass::Malformed),
            GenerationFailureCode::MalformedResponse
        );
        assert_eq!(
            map_failure(TypedFailureClass::Unsafe),
            GenerationFailureCode::UnsafeOutput
        );
        assert_eq!(
            map_failure(TypedFailureClass::Contradiction),
            GenerationFailureCode::Contradiction
        );
    }

    #[tokio::test]
    async fn inline_begin_leases_only_its_exact_durable_job() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        seed_campaign(&repository, &store, 1).await;
        let ledger = InlineGenerationLedger::new(
            repository.clone(),
            &disabled_profile(),
            &governance_config(),
        );

        let attempt = ledger
            .begin(request())
            .await
            .expect("first inline request should lease its job");
        let stored = repository
            .load_generation_job("campaign-ledger", &attempt.job_id)
            .await
            .expect("job should load")
            .expect("job should exist");
        assert_eq!(stored.state, GenerationJobState::Running);
        assert_eq!(stored.attempt_count, 1);

        assert!(matches!(
            ledger.begin(request()).await,
            Err(GenerationLedgerError::AlreadyHandled {
                state: GenerationJobState::Running
            })
        ));
        drop_test_database(&store, &database).await;
    }

    #[tokio::test]
    async fn completed_presentation_attempt_replays_despite_fresh_server_uuid() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        seed_campaign(&repository, &store, 2).await;
        store
            .document_collection(CollectionName::TurnEvents)
            .insert_one(doc! {
                "_id": "turn-ledger",
                "schema_version": 1_i32,
                "campaign_id": "campaign-ledger",
                "play_session_id": "play-session:ledger",
                "sequence": 1_i64,
                "correlation_id": "correlation:turn-ledger",
                "created_at": DateTime::now(),
            })
            .await
            .expect("turn fixture should insert");
        let ledger = InlineGenerationLedger::new(
            repository.clone(),
            &disabled_profile(),
            &governance_config(),
        );
        let attempt = ledger
            .begin(InlineGenerationRequest {
                campaign_session_id: "campaign-ledger".to_owned(),
                origin_turn_id: Some("turn-ledger".to_owned()),
                origin_campaign_revision: 2,
                purpose: GenerationPurpose::Narration,
                idempotency_key: "narration:1:replay".to_owned(),
                input_digest: Sha256Digest::from_bytes([11; 32]),
                prompt_digest: Sha256Digest::from_bytes([12; 32]),
                policy_digest: Sha256Digest::from_bytes([13; 32]),
                config_digest: Sha256Digest::from_bytes([14; 32]),
                correlation_id: "correlation:presentation-replay".to_owned(),
            })
            .await
            .expect("narration attempt should begin");
        let first = ledger
            .finish_engine_authored_presentation(
                &attempt,
                "The soot settles without changing the saved roll.",
                "client:presentation-replay",
                GenerationFailureCode::ProviderUnavailable,
            )
            .await
            .expect("first completion should commit");
        let replay = ledger
            .finish_engine_authored_presentation(
                &attempt,
                "The soot settles without changing the saved roll.",
                "client:presentation-replay",
                GenerationFailureCode::ProviderUnavailable,
            )
            .await
            .expect("fresh presentation UUID must resolve to the original attempt row");
        assert_eq!(replay, first);
        let governance_reservations = store
            .document_collection(CollectionName::GenerationBudgetReservations)
            .count_documents(doc! { "job_id": &attempt.job_id })
            .await
            .expect("presentation completion should retain its governance receipt");
        let settled_reservations = store
            .document_collection(CollectionName::GenerationBudgetReservations)
            .count_documents(doc! { "job_id": &attempt.job_id, "state": "settled" })
            .await
            .expect("presentation completion should settle its governance receipt");
        assert_eq!(governance_reservations, 4);
        assert_eq!(settled_reservations, governance_reservations);
        drop_test_database(&store, &database).await;
    }
}
