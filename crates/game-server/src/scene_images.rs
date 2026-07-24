//! Typed, asynchronous scene-image boundary.
//!
//! Briefs are reconstructed from immutable encounter events after restart.
//! They contain only closed engine-authored fictional facts; raw narration,
//! player text, private inspiration, identities, paths, and provider
//! instructions have no field through which to enter the provider request.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, imageops::FilterType};
use manchester_dnd_core::{
    SessionEventPayload, Sha256Digest, encounter::EncounterStatus, is_valid_opaque_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, time};
use uuid::Uuid;

use crate::{
    application::LOCAL_HERO_OWNER_KEY,
    config::{GenerationGovernanceConfig, LlmBackend, LlmProfile},
    error::{GenerationError, RepositoryError},
    generation::{ImageGenerationRequest, ImageGenerator},
    repository::{
        AuthorizedSceneImageVariant, MongoRepository, NewGenerationGovernanceReceipt,
        NewSceneImageArtifact, NewSceneImageQuarantine, SceneImageArtifact,
        SceneImageRequestCounts, SceneImageVariant,
        jobs::{
            ClaimedGenerationJob, EnqueueGenerationJobOutcome, GenerationAttemptFailure,
            GenerationAttemptFinishOutcome, GenerationClaim, GenerationFailureCode, GenerationJob,
            GenerationJobState, GenerationJobStoreError, GenerationPurpose, GenerationSuccess,
            GenerationUsage, IMAGE_REQUESTS_PER_TURN, NewGenerationJob, SuccessRetention,
        },
    },
};

pub const IMAGE_BRIEF_SCHEMA_VERSION: u16 = 1;
const IMAGE_POLICY_VERSION: &str = "scene-image-policy/v1";
const MAX_PROVIDER_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4_096;
const MAX_IMAGE_PIXELS: u64 = 16_777_216;
const MAX_PUBLISHED_PNG_BYTES: usize = 32 * 1024 * 1024;
const WEB_MAX_DIMENSION: u32 = 1_600;
const THUMBNAIL_MAX_DIMENSION: u32 = 512;
const WORKER_LEASE: Duration = Duration::from_secs(60);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const CIRCUIT_FAILURE_THRESHOLD: u8 = 3;
const CIRCUIT_OPEN_DURATION: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibleImageFact {
    RainyFantasyCanalUnderStoneViaduct,
    UnnamedCanalWardenAndSootSpirit,
    BrassRunesAndOldSluice,
    RunesGlowWithProtectiveLight,
    RunesReleaseACloudOfSoot,
    EncounterReady,
    EncounterInProgress,
    CanalWardenVictorious,
    StoryRecoveryAfterDefeat,
}

impl VisibleImageFact {
    const fn prompt_text(self) -> &'static str {
        match self {
            Self::RainyFantasyCanalUnderStoneViaduct => {
                "A rain-washed fantasy canal passes beneath an old stone viaduct."
            }
            Self::UnnamedCanalWardenAndSootSpirit => {
                "An unnamed fantasy canal warden faces an invented soot spirit."
            }
            Self::BrassRunesAndOldSluice => {
                "Original brass runes and an old sluice mechanism frame the scene."
            }
            Self::RunesGlowWithProtectiveLight => {
                "The fictional runes glow with a gentle protective light."
            }
            Self::RunesReleaseACloudOfSoot => {
                "The fictional runes release a harmless dramatic curl of soot."
            }
            Self::EncounterReady => "The two fantasy figures regard one another before action.",
            Self::EncounterInProgress => {
                "The scene shows energetic fantasy action without injury detail."
            }
            Self::CanalWardenVictorious => {
                "The canal warden stands victorious as the soot spirit disperses."
            }
            Self::StoryRecoveryAfterDefeat => {
                "The canal warden rests safely while the threatening spirit recedes."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageArtDirection {
    PainterlyGothicFantasy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageComposition {
    WideEstablishingScene,
    DynamicTwoFigureScene,
    CalmAftermathScene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSafetyRating {
    TeenFantasyNonGraphic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFictionalizationPolicy {
    OriginalFantasyNoRealPersonLikeness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageBrief {
    pub schema_version: u16,
    pub visible_facts: Vec<VisibleImageFact>,
    pub art_direction: ImageArtDirection,
    pub composition: ImageComposition,
    pub exclusions: Vec<String>,
    pub safety_rating: ImageSafetyRating,
    pub fictionalization_policy: ImageFictionalizationPolicy,
    pub alt_text_context: String,
}

impl ImageBrief {
    fn from_encounter(
        status: EncounterStatus,
        runes_understood: bool,
    ) -> Result<Self, SceneImageError> {
        let status_fact = match status {
            EncounterStatus::Ready => VisibleImageFact::EncounterReady,
            EncounterStatus::Active => VisibleImageFact::EncounterInProgress,
            EncounterStatus::Victory => VisibleImageFact::CanalWardenVictorious,
            EncounterStatus::Defeat => VisibleImageFact::StoryRecoveryAfterDefeat,
        };
        let composition = match status {
            EncounterStatus::Ready => ImageComposition::WideEstablishingScene,
            EncounterStatus::Active => ImageComposition::DynamicTwoFigureScene,
            EncounterStatus::Victory | EncounterStatus::Defeat => {
                ImageComposition::CalmAftermathScene
            }
        };
        let brief = Self {
            schema_version: IMAGE_BRIEF_SCHEMA_VERSION,
            visible_facts: vec![
                VisibleImageFact::RainyFantasyCanalUnderStoneViaduct,
                VisibleImageFact::UnnamedCanalWardenAndSootSpirit,
                VisibleImageFact::BrassRunesAndOldSluice,
                if runes_understood {
                    VisibleImageFact::RunesGlowWithProtectiveLight
                } else {
                    VisibleImageFact::RunesReleaseACloudOfSoot
                },
                status_fact,
            ],
            art_direction: ImageArtDirection::PainterlyGothicFantasy,
            composition,
            exclusions: vec![
                "real people or recognizable likenesses".to_owned(),
                "names, writing, captions, logos, trademarks, or contact details".to_owned(),
                "photographs, private documents, screens, maps, or source material".to_owned(),
                "modern uniforms, real institutions, exact Manchester landmarks".to_owned(),
                "blood, gore, sexual content, hate symbols, or frightening injury".to_owned(),
            ],
            safety_rating: ImageSafetyRating::TeenFantasyNonGraphic,
            fictionalization_policy:
                ImageFictionalizationPolicy::OriginalFantasyNoRealPersonLikeness,
            alt_text_context: match status {
                EncounterStatus::Ready => {
                    "An unnamed canal warden and a soot spirit meet beneath a rainy fantasy viaduct."
                }
                EncounterStatus::Active => {
                    "An unnamed canal warden confronts a soot spirit beside glowing runes and a sluice."
                }
                EncounterStatus::Victory => {
                    "An unnamed canal warden stands beneath the viaduct as the defeated soot spirit disperses."
                }
                EncounterStatus::Defeat => {
                    "An unnamed canal warden rests safely by the canal as the soot spirit recedes."
                }
            }
            .to_owned(),
        };
        brief.validate()?;
        Ok(brief)
    }

    pub fn validate(&self) -> Result<(), SceneImageError> {
        if self.schema_version != IMAGE_BRIEF_SCHEMA_VERSION
            || self.visible_facts.len() != 5
            || self.exclusions.len() != 5
            || self
                .exclusions
                .iter()
                .any(|value| value.trim() != value || value.is_empty() || value.len() > 200)
            || self.alt_text_context.trim() != self.alt_text_context
            || self.alt_text_context.is_empty()
            || self.alt_text_context.chars().count() > 500
            || self.alt_text_context.chars().any(char::is_control)
        {
            return Err(SceneImageError::PolicyRejected);
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<Sha256Digest, SceneImageError> {
        let bytes = serde_json::to_vec(self).map_err(SceneImageError::BriefSerialization)?;
        Ok(digest(&bytes))
    }

    pub fn provider_prompt(&self) -> Result<String, SceneImageError> {
        self.validate()?;
        let facts = self
            .visible_facts
            .iter()
            .map(|fact| fact.prompt_text())
            .collect::<Vec<_>>()
            .join(" ");
        let exclusions = self.exclusions.join("; ");
        let composition = match self.composition {
            ImageComposition::WideEstablishingScene => "wide establishing composition",
            ImageComposition::DynamicTwoFigureScene => "dynamic two-figure composition",
            ImageComposition::CalmAftermathScene => "calm aftermath composition",
        };
        Ok(format!(
            "Create an original painterly gothic-fantasy scene with a {composition}. {facts} Keep it suitable for a teen fantasy adventure and clearly illustrated rather than photographic. Exclude: {exclusions}. Do not add facts, identities, text, symbols, or likenesses not stated here."
        ))
    }
}

#[derive(Debug, Error)]
pub enum SceneImageError {
    #[error("scene-image generation is disabled")]
    Disabled,
    #[error("the scene-image command is invalid")]
    InvalidCommand,
    #[error("the requested campaign is unavailable")]
    WrongCampaign,
    #[error("the campaign changed before the image request")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("no committed encounter scene is available")]
    NoCommittedScene,
    #[error("the scene-image request failed policy validation")]
    PolicyRejected,
    #[error("the scene-image request budget is exhausted")]
    BudgetExceeded,
    #[error("the scene-image replacement limit is exhausted")]
    ReplacementLimit,
    #[error("the scene-image job was not found")]
    NotFound,
    #[error("the scene-image provider circuit is temporarily open")]
    CircuitOpen,
    #[error("could not serialize the bounded scene-image brief")]
    BriefSerialization(#[source] serde_json::Error),
    #[error("scene-image persistence failed")]
    Store(#[from] GenerationJobStoreError),
    #[error("campaign event storage failed")]
    Repository(#[from] RepositoryError),
    #[error("scene-image provider failed")]
    Generation(#[from] GenerationError),
    #[error("protected image storage failed")]
    Storage(#[source] std::io::Error),
    #[error("scene-image bytes failed validation: {0}")]
    InvalidArtifact(&'static str),
    #[error("scene-image codec failed")]
    Codec(#[source] image::ImageError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneImageEnqueueOutcome {
    pub job: GenerationJob,
    pub existing: bool,
    pub counts: SceneImageRequestCounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneImageServiceStatus {
    pub provider_enabled: bool,
    pub provider_temporarily_unavailable: bool,
    pub estimated_cost_microusd: u64,
    pub campaign_cost_used_microusd: u64,
    pub campaign_cost_limit_microusd: u64,
    pub counts: SceneImageRequestCounts,
    pub latest_job: Option<GenerationJob>,
    pub artifact: Option<SceneImageArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredSceneImage {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub digest: Sha256Digest,
    pub alt_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneImageWorkerOutcome {
    Idle,
    Succeeded,
    RetryScheduled,
    Failed,
    LostLease,
    CircuitOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SceneImageCleanupOutcome {
    pub records_deleted: u64,
    pub files_deleted: u64,
}

#[derive(Debug, Default)]
struct CircuitState {
    consecutive_failures: u8,
    open_until: Option<Instant>,
}

#[derive(Clone)]
pub struct SceneImageService {
    repository: MongoRepository,
    provider: Arc<dyn ImageGenerator>,
    profile: LlmProfile,
    governance: GenerationGovernanceConfig,
    config_fingerprint: Sha256Digest,
    policy_fingerprint: Sha256Digest,
    storage: Arc<ProtectedImageStorage>,
    circuit: Arc<Mutex<CircuitState>>,
}

impl SceneImageService {
    pub fn new(
        repository: MongoRepository,
        provider: Arc<dyn ImageGenerator>,
        profile: &LlmProfile,
        governance: &GenerationGovernanceConfig,
        artifact_root: &Path,
    ) -> Result<Self, SceneImageError> {
        let storage = ProtectedImageStorage::initialize(artifact_root)?;
        Ok(Self {
            repository,
            provider,
            profile: profile.clone(),
            governance: governance.clone(),
            config_fingerprint: profile.non_secret_fingerprint("scene-image"),
            policy_fingerprint: digest(IMAGE_POLICY_VERSION.as_bytes()),
            storage: Arc::new(storage),
            circuit: Arc::new(Mutex::new(CircuitState::default())),
        })
    }

    pub async fn request(
        &self,
        campaign_session_id: &str,
        expected_revision: u64,
        idempotency_key: &str,
        replacement: bool,
        correlation_id: Option<&str>,
    ) -> Result<SceneImageEnqueueOutcome, SceneImageError> {
        if self.profile.backend == LlmBackend::Disabled {
            return Err(SceneImageError::Disabled);
        }
        if !is_valid_opaque_id(campaign_session_id)
            || expected_revision == 0
            || !is_valid_opaque_id(idempotency_key)
            || correlation_id.is_some_and(|value| !is_valid_opaque_id(value))
        {
            return Err(SceneImageError::InvalidCommand);
        }
        let campaign = self
            .repository
            .load_campaign_session(LOCAL_HERO_OWNER_KEY, campaign_session_id)
            .await?
            .ok_or(SceneImageError::WrongCampaign)?;
        if campaign.revision != expected_revision {
            return Err(SceneImageError::RevisionConflict {
                expected: expected_revision,
                actual: campaign.revision,
            });
        }
        let (turn_id, turn_number, brief) = self.latest_brief(campaign_session_id).await?;
        let exact_retry = self
            .repository
            .load_generation_job_by_key(
                campaign_session_id,
                GenerationPurpose::Illustration,
                idempotency_key,
            )
            .await?
            .is_some_and(|job| job.origin_turn_id.as_deref() == Some(turn_id.as_str()));
        let before = self
            .repository
            .scene_image_request_counts(campaign_session_id, Some(&turn_id))
            .await?;
        if !exact_retry && before.source_turn >= IMAGE_REQUESTS_PER_TURN {
            return Err(SceneImageError::ReplacementLimit);
        }
        if !exact_retry && replacement != (before.source_turn == 1) {
            return Err(SceneImageError::InvalidCommand);
        }
        let brief_fingerprint = brief.fingerprint()?;
        let prompt = brief.provider_prompt()?;
        let prompt_fingerprint = digest(prompt.as_bytes());
        let origin_revision = turn_number
            .checked_add(1)
            .ok_or(SceneImageError::InvalidCommand)?;
        let request = NewGenerationJob {
            id: format!("image-job:{}", Uuid::new_v4()),
            campaign_session_id: campaign_session_id.to_owned(),
            origin_turn_id: Some(turn_id.clone()),
            origin_campaign_revision: origin_revision,
            purpose: GenerationPurpose::Illustration,
            idempotency_key: idempotency_key.to_owned(),
            input_digest: brief_fingerprint.clone(),
            prompt_digest: prompt_fingerprint,
            policy_digest: self.policy_fingerprint.clone(),
            config_digest: self.config_fingerprint.clone(),
            correlation_id: correlation_id.map(ToOwned::to_owned),
            max_attempts: GenerationPurpose::Illustration.default_max_attempts(),
            success_retention: SuccessRetention::CampaignLifetime,
            governance: Some(NewGenerationGovernanceReceipt {
                turn_scope_key: turn_id.clone(),
                request_fingerprint: brief_fingerprint,
                policy_fingerprint: self.policy_fingerprint.clone(),
                config_fingerprint: self.config_fingerprint.clone(),
                governance_fingerprint: self.governance.non_secret_fingerprint(),
                reserved_requests: 1,
                reserved_tokens: 0,
                reserved_latency_milliseconds: self
                    .profile
                    .estimated_request_latency_milliseconds(),
                reserved_cost_microusd: self.profile.estimated_request_cost_microusd,
                limits: self.governance.clone(),
            }),
        };
        let (job, existing) = match self.repository.enqueue_generation_job(&request).await {
            Ok(EnqueueGenerationJobOutcome::Enqueued(job)) => (job, false),
            Ok(EnqueueGenerationJobOutcome::Existing(job)) => (job, true),
            Err(GenerationJobStoreError::BudgetExceeded { scope, .. }) => {
                return Err(if scope == crate::repository::GenerationBudgetScope::Turn {
                    SceneImageError::ReplacementLimit
                } else {
                    SceneImageError::BudgetExceeded
                });
            }
            Err(error) => return Err(error.into()),
        };
        let counts = self
            .repository
            .scene_image_request_counts(campaign_session_id, Some(&turn_id))
            .await?;
        Ok(SceneImageEnqueueOutcome {
            job,
            existing,
            counts,
        })
    }

    pub async fn status(
        &self,
        campaign_session_id: &str,
    ) -> Result<SceneImageServiceStatus, SceneImageError> {
        if !is_valid_opaque_id(campaign_session_id) {
            return Err(SceneImageError::InvalidCommand);
        }
        let latest = self.latest_brief(campaign_session_id).await.ok();
        let turn_id = latest.as_ref().map(|(id, _, _)| id.as_str());
        let counts = self
            .repository
            .scene_image_request_counts(campaign_session_id, turn_id)
            .await?;
        let latest_job = match turn_id {
            Some(turn_id) => self
                .repository
                .latest_scene_image_job_id(campaign_session_id, turn_id)
                .await?
                .map(|job_id| async move {
                    self.repository
                        .load_generation_job(campaign_session_id, &job_id)
                        .await
                }),
            None => None,
        };
        let latest_job = match latest_job {
            Some(future) => future.await?,
            None => None,
        };
        let artifact = match latest_job.as_ref() {
            Some(job) if job.state == GenerationJobState::Succeeded => {
                self.repository
                    .scene_image_artifact_for_job(campaign_session_id, &job.id)
                    .await?
            }
            _ => None,
        };
        let budget = self
            .repository
            .generation_budget_status(campaign_session_id, &self.governance)
            .await?;
        Ok(SceneImageServiceStatus {
            provider_enabled: self.profile.backend != LlmBackend::Disabled,
            provider_temporarily_unavailable: self.circuit_is_open(),
            estimated_cost_microusd: self.profile.estimated_request_cost_microusd,
            campaign_cost_used_microusd: budget.campaign_cost_microusd.used,
            campaign_cost_limit_microusd: budget.campaign_cost_microusd.limit,
            counts,
            latest_job,
            artifact,
        })
    }

    pub async fn cancel(
        &self,
        campaign_session_id: &str,
        job_id: &str,
    ) -> Result<GenerationJob, SceneImageError> {
        let job = self
            .repository
            .load_generation_job(campaign_session_id, job_id)
            .await?
            .ok_or(SceneImageError::NotFound)?;
        if job.purpose != GenerationPurpose::Illustration {
            return Err(SceneImageError::NotFound);
        }
        Ok(self
            .repository
            .cancel_generation_job(campaign_session_id, job_id)
            .await?)
    }

    pub async fn deliver(
        &self,
        campaign_session_id: &str,
        artifact_id: &str,
        variant: SceneImageVariant,
    ) -> Result<Option<DeliveredSceneImage>, SceneImageError> {
        let Some(authorized) = self
            .repository
            .authorized_scene_image_variant(campaign_session_id, artifact_id, variant)
            .await?
        else {
            return Ok(None);
        };
        let bytes = self.storage.read(&authorized).await?;
        if digest(&bytes) != authorized.digest {
            return Err(SceneImageError::InvalidArtifact(
                "stored variant digest mismatch",
            ));
        }
        Ok(Some(DeliveredSceneImage {
            bytes,
            media_type: authorized.media_type,
            digest: authorized.digest,
            alt_text: authorized.alt_text,
        }))
    }

    pub async fn process_next(
        &self,
        worker_id: &str,
    ) -> Result<SceneImageWorkerOutcome, SceneImageError> {
        if self.profile.backend == LlmBackend::Disabled {
            return Ok(SceneImageWorkerOutcome::Idle);
        }
        if self.circuit_is_open() {
            return Ok(SceneImageWorkerOutcome::CircuitOpen);
        }
        let claim = GenerationClaim {
            worker_id: worker_id.to_owned(),
            provider: self.provider_id().to_owned(),
            model: self.model_id(),
            lease_duration: WORKER_LEASE,
        };
        let Some(claimed) = self
            .repository
            .claim_generation_job_for_purpose(GenerationPurpose::Illustration, &claim)
            .await?
        else {
            return Ok(SceneImageWorkerOutcome::Idle);
        };
        self.process_claimed(claimed).await
    }

    pub async fn cleanup_expired(
        &self,
        limit: u16,
    ) -> Result<SceneImageCleanupOutcome, SceneImageError> {
        let candidates = self
            .repository
            .expired_scene_image_cleanup_candidates(limit)
            .await?;
        let mut outcome = SceneImageCleanupOutcome::default();
        for candidate in candidates {
            for key in &candidate.storage_keys {
                if self.storage.delete(key).await? {
                    outcome.files_deleted = outcome.files_deleted.saturating_add(1);
                }
            }
            if self
                .repository
                .delete_scene_image_cleanup_candidate(&candidate)
                .await?
            {
                outcome.records_deleted = outcome.records_deleted.saturating_add(1);
            }
        }
        Ok(outcome)
    }

    async fn process_claimed(
        &self,
        claimed: ClaimedGenerationJob,
    ) -> Result<SceneImageWorkerOutcome, SceneImageError> {
        let brief = match self.brief_for_claim(&claimed).await {
            Ok(brief) => brief,
            Err(_) => {
                return self
                    .finish_failure(&claimed, GenerationFailureCode::Contradiction, None, None)
                    .await;
            }
        };
        let prompt = brief.provider_prompt()?;
        let generation_request = ImageGenerationRequest::one(prompt);
        let started = Instant::now();
        let response = match self
            .generate_with_heartbeat(&claimed, generation_request)
            .await
        {
            Ok(response) => response,
            Err(WorkerGenerationError::LostLease) => {
                return Ok(SceneImageWorkerOutcome::LostLease);
            }
            Err(WorkerGenerationError::Provider(error)) => {
                let code = generation_failure_code(&error);
                self.record_circuit_failure(code);
                return self
                    .finish_failure(
                        &claimed,
                        code,
                        provider_status(&error),
                        Some(started.elapsed()),
                    )
                    .await;
            }
        };
        self.reset_circuit();
        let image = match response.images.as_slice() {
            [image] if image.url.is_none() => image,
            [image] if image.url.is_some() => {
                self.quarantine(&claimed, None, "provider_url_rejected")
                    .await?;
                return self
                    .finish_failure(
                        &claimed,
                        GenerationFailureCode::InvalidArtifact,
                        None,
                        Some(started.elapsed()),
                    )
                    .await;
            }
            _ => {
                return self
                    .finish_failure(
                        &claimed,
                        GenerationFailureCode::MalformedResponse,
                        None,
                        Some(started.elapsed()),
                    )
                    .await;
            }
        };
        let raw = match decode_provider_base64(image.base64_data.as_deref()) {
            Ok(bytes) => bytes,
            Err(reason) => {
                self.quarantine(&claimed, None, reason).await?;
                return self
                    .finish_failure(
                        &claimed,
                        GenerationFailureCode::InvalidArtifact,
                        None,
                        Some(started.elapsed()),
                    )
                    .await;
            }
        };
        let processed = match process_image(&raw) {
            Ok(processed) => processed,
            Err(reason) => {
                self.quarantine(&claimed, Some(&raw), reason).await?;
                return self
                    .finish_failure(
                        &claimed,
                        if reason == "safety_rejected" {
                            GenerationFailureCode::UnsafeOutput
                        } else {
                            GenerationFailureCode::InvalidArtifact
                        },
                        None,
                        Some(started.elapsed()),
                    )
                    .await;
            }
        };
        // A cancellation or lease takeover must win before any publishable
        // metadata is registered.
        if matches!(
            self.repository
                .heartbeat_generation_job(&claimed.lease, WORKER_LEASE)
                .await,
            Err(GenerationJobStoreError::LostLease)
        ) {
            return Ok(SceneImageWorkerOutcome::LostLease);
        }
        let artifact_id = format!("scene-image:{}", claimed.job.id);
        let stored = self.storage.publish(&claimed.job.id, &processed).await?;
        let artifact = NewSceneImageArtifact {
            artifact_id: artifact_id.clone(),
            job_id: claimed.job.id.clone(),
            campaign_session_id: claimed.job.campaign_session_id.clone(),
            source_turn_id: claimed
                .job
                .origin_turn_id
                .clone()
                .ok_or(SceneImageError::InvalidCommand)?,
            brief_fingerprint: brief.fingerprint()?,
            prompt_policy_fingerprint: self.policy_fingerprint.clone(),
            config_fingerprint: self.config_fingerprint.clone(),
            provider: self.provider_id().to_owned(),
            model: self.model_id(),
            provider_request_id: None,
            original_storage_key: stored.original_key,
            web_storage_key: stored.web_key,
            thumbnail_storage_key: stored.thumbnail_key,
            original_digest: digest(&processed.original_png),
            web_digest: digest(&processed.web_png),
            thumbnail_digest: digest(&processed.thumbnail_png),
            original_width: processed.original_dimensions.0,
            original_height: processed.original_dimensions.1,
            web_width: processed.web_dimensions.0,
            web_height: processed.web_dimensions.1,
            thumbnail_width: processed.thumbnail_dimensions.0,
            thumbnail_height: processed.thumbnail_dimensions.1,
            alt_text: brief.alt_text_context.clone(),
            estimated_cost_microusd: self.profile.estimated_request_cost_microusd,
            actual_cost_microusd: Some(self.profile.estimated_request_cost_microusd),
            license_id: if self.profile.backend == LlmBackend::Fake {
                "deterministic-fake-fixture"
            } else {
                "provider-output-operator-terms"
            }
            .to_owned(),
            provenance_summary: if self.profile.backend == LlmBackend::Fake {
                "deterministic-network-free-test-fixture"
            } else {
                "generated-from-committed-public-fictional-facts"
            }
            .to_owned(),
        };
        match self.repository.upsert_scene_image_artifact(&artifact).await {
            Ok(()) => {}
            Err(GenerationJobStoreError::LostLease) => {
                return Ok(SceneImageWorkerOutcome::LostLease);
            }
            Err(error) => return Err(error.into()),
        }
        let latency = duration_millis(started.elapsed());
        match self
            .repository
            .succeed_generation_job(
                &claimed.lease,
                &GenerationSuccess {
                    artifact_id: Some(artifact_id.clone()),
                    output_digest: artifact.web_digest.clone(),
                    usage: GenerationUsage {
                        cost_microusd: Some(self.profile.estimated_request_cost_microusd),
                        latency_milliseconds: Some(latency),
                        ..GenerationUsage::default()
                    },
                },
            )
            .await
        {
            Ok(_) => {
                self.repository
                    .select_scene_image_artifact(
                        &artifact.campaign_session_id,
                        &artifact.source_turn_id,
                        &artifact.artifact_id,
                    )
                    .await?;
                Ok(SceneImageWorkerOutcome::Succeeded)
            }
            Err(GenerationJobStoreError::LostLease) => Ok(SceneImageWorkerOutcome::LostLease),
            Err(error) => Err(error.into()),
        }
    }

    async fn latest_brief(
        &self,
        campaign_session_id: &str,
    ) -> Result<(String, u64, ImageBrief), SceneImageError> {
        let events = self
            .repository
            .list_session_events(LOCAL_HERO_OWNER_KEY, campaign_session_id)
            .await?;
        let event = events
            .into_iter()
            .rev()
            .find(|event| {
                matches!(
                    event.payload.payload,
                    SessionEventPayload::EncounterResolved { .. }
                )
            })
            .ok_or(SceneImageError::NoCommittedScene)?;
        let SessionEventPayload::EncounterResolved { outcome, .. } = &event.payload.payload else {
            return Err(SceneImageError::NoCommittedScene);
        };
        let brief = ImageBrief::from_encounter(
            outcome.resolution.state.status,
            matches!(
                outcome.resolution.state.opening_consequence,
                manchester_dnd_core::encounter::OpeningConsequence::RunesUnderstood
            ),
        )?;
        Ok((event.id, event.turn_number, brief))
    }

    async fn brief_for_claim(
        &self,
        claimed: &ClaimedGenerationJob,
    ) -> Result<ImageBrief, SceneImageError> {
        let source_turn_id = claimed
            .job
            .origin_turn_id
            .as_deref()
            .ok_or(SceneImageError::NoCommittedScene)?;
        let events = self
            .repository
            .list_session_events(LOCAL_HERO_OWNER_KEY, &claimed.job.campaign_session_id)
            .await?;
        let event = events
            .into_iter()
            .find(|event| event.id == source_turn_id)
            .ok_or(SceneImageError::NoCommittedScene)?;
        let SessionEventPayload::EncounterResolved { outcome, .. } = &event.payload.payload else {
            return Err(SceneImageError::NoCommittedScene);
        };
        let brief = ImageBrief::from_encounter(
            outcome.resolution.state.status,
            matches!(
                outcome.resolution.state.opening_consequence,
                manchester_dnd_core::encounter::OpeningConsequence::RunesUnderstood
            ),
        )?;
        let prompt = brief.provider_prompt()?;
        if brief.fingerprint()? != claimed.job.input_digest
            || digest(prompt.as_bytes()) != claimed.job.prompt_digest
            || self.policy_fingerprint != claimed.job.policy_digest
            || self.config_fingerprint != claimed.job.config_digest
        {
            return Err(SceneImageError::PolicyRejected);
        }
        Ok(brief)
    }

    async fn generate_with_heartbeat(
        &self,
        claimed: &ClaimedGenerationJob,
        request: ImageGenerationRequest,
    ) -> Result<crate::generation::ImageGenerationResponse, WorkerGenerationError> {
        let generation = self.provider.generate_image(request);
        tokio::pin!(generation);
        let deadline = time::sleep(self.profile.timeout);
        tokio::pin!(deadline);
        let mut heartbeat = time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                result = &mut generation => return result.map_err(WorkerGenerationError::Provider),
                _ = &mut deadline => {
                    return Err(WorkerGenerationError::Provider(GenerationError::Timeout {
                        timeout: self.profile.timeout,
                    }));
                }
                _ = heartbeat.tick() => {
                    match self.repository
                        .heartbeat_generation_job(&claimed.lease, WORKER_LEASE)
                        .await
                    {
                        Ok(_) => {}
                        Err(GenerationJobStoreError::LostLease) => {
                            return Err(WorkerGenerationError::LostLease);
                        }
                        Err(_) => return Err(WorkerGenerationError::Provider(
                            GenerationError::InvalidResponse {
                                endpoint: "image generation",
                                reason: "lease heartbeat failed",
                            }
                        )),
                    }
                }
            }
        }
    }

    async fn finish_failure(
        &self,
        claimed: &ClaimedGenerationJob,
        code: GenerationFailureCode,
        status: Option<u16>,
        elapsed: Option<Duration>,
    ) -> Result<SceneImageWorkerOutcome, SceneImageError> {
        let result = self
            .repository
            .fail_generation_attempt(
                &claimed.lease,
                &GenerationAttemptFailure {
                    code,
                    provider_status: status,
                    provider_request_id: None,
                    usage: GenerationUsage {
                        latency_milliseconds: elapsed.map(duration_millis),
                        cost_microusd: Some(0),
                        ..GenerationUsage::default()
                    },
                    output_digest: None,
                },
            )
            .await;
        match result {
            Ok(GenerationAttemptFinishOutcome::RetryScheduled) => {
                Ok(SceneImageWorkerOutcome::RetryScheduled)
            }
            Ok(GenerationAttemptFinishOutcome::Failed) => Ok(SceneImageWorkerOutcome::Failed),
            Ok(GenerationAttemptFinishOutcome::Succeeded) => {
                Err(SceneImageError::InvalidArtifact("invalid failure outcome"))
            }
            Err(GenerationJobStoreError::LostLease) => Ok(SceneImageWorkerOutcome::LostLease),
            Err(error) => Err(error.into()),
        }
    }

    async fn quarantine(
        &self,
        claimed: &ClaimedGenerationJob,
        bytes: Option<&[u8]>,
        reason: &'static str,
    ) -> Result<(), SceneImageError> {
        let stored = match bytes {
            Some(bytes) if bytes.len() <= MAX_PROVIDER_IMAGE_BYTES => {
                Some(self.storage.quarantine(&claimed.attempt.id, bytes).await?)
            }
            _ => None,
        };
        self.repository
            .record_scene_image_quarantine(&NewSceneImageQuarantine {
                id: format!("image-quarantine:{}", Uuid::new_v4()),
                job_id: claimed.job.id.clone(),
                attempt_id: claimed.attempt.id.clone(),
                campaign_session_id: claimed.job.campaign_session_id.clone(),
                byte_digest: bytes.map(digest),
                byte_length: bytes.and_then(|bytes| u64::try_from(bytes.len()).ok()),
                storage_key: stored,
                reason_code: reason,
            })
            .await?;
        Ok(())
    }

    fn provider_id(&self) -> &'static str {
        match self.profile.backend {
            LlmBackend::Disabled => "disabled",
            LlmBackend::Fake => "deterministic-fake",
            LlmBackend::OpenAiCompatible => "openai-compatible",
        }
    }

    fn model_id(&self) -> String {
        match self.profile.backend {
            LlmBackend::Disabled => "disabled".to_owned(),
            LlmBackend::Fake => "deterministic-image-fake-v1".to_owned(),
            LlmBackend::OpenAiCompatible => self
                .profile
                .model
                .clone()
                .unwrap_or_else(|| "invalid-profile".to_owned()),
        }
    }

    fn circuit_is_open(&self) -> bool {
        let mut circuit = self.circuit.lock().expect("image circuit mutex poisoned");
        if circuit
            .open_until
            .is_some_and(|until| until <= Instant::now())
        {
            *circuit = CircuitState::default();
        }
        circuit.open_until.is_some()
    }

    fn record_circuit_failure(&self, code: GenerationFailureCode) {
        if !code.retryable() {
            return;
        }
        let mut circuit = self.circuit.lock().expect("image circuit mutex poisoned");
        circuit.consecutive_failures = circuit.consecutive_failures.saturating_add(1);
        if circuit.consecutive_failures >= CIRCUIT_FAILURE_THRESHOLD {
            circuit.open_until = Some(Instant::now() + CIRCUIT_OPEN_DURATION);
        }
    }

    fn reset_circuit(&self) {
        *self.circuit.lock().expect("image circuit mutex poisoned") = CircuitState::default();
    }
}

enum WorkerGenerationError {
    Provider(GenerationError),
    LostLease,
}

#[derive(Debug)]
struct ProcessedImage {
    original_png: Vec<u8>,
    web_png: Vec<u8>,
    thumbnail_png: Vec<u8>,
    original_dimensions: (u32, u32),
    web_dimensions: (u32, u32),
    thumbnail_dimensions: (u32, u32),
}

struct StoredImageKeys {
    original_key: String,
    web_key: String,
    thumbnail_key: String,
}

struct ProtectedImageStorage {
    root: PathBuf,
}

impl ProtectedImageStorage {
    fn initialize(configured_root: &Path) -> Result<Self, SceneImageError> {
        if configured_root.exists() {
            let metadata =
                std::fs::symlink_metadata(configured_root).map_err(SceneImageError::Storage)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(SceneImageError::InvalidArtifact(
                    "artifact root must be a real directory",
                ));
            }
        } else {
            std::fs::create_dir_all(configured_root).map_err(SceneImageError::Storage)?;
        }
        let root = std::fs::canonicalize(configured_root).map_err(SceneImageError::Storage)?;
        if root
            .components()
            .any(|component| matches!(component.as_os_str().to_str(), Some("public" | "target")))
        {
            return Err(SceneImageError::InvalidArtifact(
                "artifact root resolved into a public directory",
            ));
        }
        for directory in [root.join("artifacts"), root.join("quarantine")] {
            std::fs::create_dir_all(&directory).map_err(SceneImageError::Storage)?;
            set_private_directory_permissions(&directory)?;
        }
        set_private_directory_permissions(&root)?;
        Ok(Self { root })
    }

    async fn publish(
        &self,
        job_id: &str,
        image: &ProcessedImage,
    ) -> Result<StoredImageKeys, SceneImageError> {
        let directory_key = digest(job_id.as_bytes())
            .as_str()
            .strip_prefix("sha256:")
            .expect("digest format is stable")
            .to_owned();
        let base = format!("artifacts/{directory_key}");
        let directory = self.root.join(&base);
        fs::create_dir_all(&directory)
            .await
            .map_err(SceneImageError::Storage)?;
        set_private_directory_permissions(&directory)?;
        let original_key = format!("{base}/original.png");
        let web_key = format!("{base}/web.png");
        let thumbnail_key = format!("{base}/thumbnail.png");
        self.write_atomic(&original_key, &image.original_png)
            .await?;
        self.write_atomic(&web_key, &image.web_png).await?;
        self.write_atomic(&thumbnail_key, &image.thumbnail_png)
            .await?;
        Ok(StoredImageKeys {
            original_key,
            web_key,
            thumbnail_key,
        })
    }

    async fn quarantine(&self, attempt_id: &str, bytes: &[u8]) -> Result<String, SceneImageError> {
        let key = format!(
            "quarantine/{}.bin",
            digest(attempt_id.as_bytes())
                .as_str()
                .strip_prefix("sha256:")
                .expect("digest format is stable")
        );
        self.write_atomic(&key, bytes).await?;
        Ok(key)
    }

    async fn read(
        &self,
        authorized: &AuthorizedSceneImageVariant,
    ) -> Result<Vec<u8>, SceneImageError> {
        if !valid_storage_key(&authorized.storage_key) {
            return Err(SceneImageError::InvalidArtifact("invalid storage key"));
        }
        let path = self.root.join(&authorized.storage_key);
        let canonical = fs::canonicalize(&path)
            .await
            .map_err(SceneImageError::Storage)?;
        if !canonical.starts_with(&self.root) {
            return Err(SceneImageError::InvalidArtifact(
                "stored variant escaped protected root",
            ));
        }
        let metadata = fs::symlink_metadata(&canonical)
            .await
            .map_err(SceneImageError::Storage)?;
        if !metadata.is_file() || metadata.len() > MAX_PUBLISHED_PNG_BYTES as u64 {
            return Err(SceneImageError::InvalidArtifact(
                "stored variant is not a bounded regular file",
            ));
        }
        fs::read(canonical).await.map_err(SceneImageError::Storage)
    }

    async fn write_atomic(&self, key: &str, bytes: &[u8]) -> Result<(), SceneImageError> {
        if !valid_storage_key(key) || bytes.len() > MAX_PROVIDER_IMAGE_BYTES {
            return Err(SceneImageError::InvalidArtifact(
                "protected write exceeded bounds",
            ));
        }
        let destination = self.root.join(key);
        let parent = destination
            .parent()
            .ok_or(SceneImageError::InvalidArtifact(
                "storage key has no parent",
            ))?;
        let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temp)
            .await
            .map_err(SceneImageError::Storage)?;
        file.write_all(bytes)
            .await
            .map_err(SceneImageError::Storage)?;
        file.sync_all().await.map_err(SceneImageError::Storage)?;
        drop(file);
        fs::rename(&temp, &destination)
            .await
            .map_err(SceneImageError::Storage)?;
        set_private_file_permissions(&destination)?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, SceneImageError> {
        if !valid_storage_key(key) {
            return Err(SceneImageError::InvalidArtifact(
                "invalid cleanup storage key",
            ));
        }
        let path = self.root.join(key);
        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(SceneImageError::Storage(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SceneImageError::InvalidArtifact(
                "cleanup target is not a regular file",
            ));
        }
        fs::remove_file(&path)
            .await
            .map_err(SceneImageError::Storage)?;
        if let Some(parent) = path.parent()
            && parent != self.root
        {
            let _ = fs::remove_dir(parent).await;
        }
        Ok(true)
    }
}

fn decode_provider_base64(value: Option<&str>) -> Result<Vec<u8>, &'static str> {
    let value = value.ok_or("base64_invalid")?;
    if value.len() > (MAX_PROVIDER_IMAGE_BYTES * 4 / 3).saturating_add(8) {
        return Err("byte_limit");
    }
    let bytes = BASE64_STANDARD
        .decode(value)
        .map_err(|_| "base64_invalid")?;
    if bytes.is_empty() || bytes.len() > MAX_PROVIDER_IMAGE_BYTES {
        return Err("byte_limit");
    }
    Ok(bytes)
}

fn process_image(bytes: &[u8]) -> Result<ProcessedImage, &'static str> {
    let format = image::guess_format(bytes).map_err(|_| "format_invalid")?;
    if !matches!(format, ImageFormat::Png | ImageFormat::WebP) {
        return Err("format_invalid");
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some((MAX_IMAGE_PIXELS * 8).min(u64::from(u32::MAX)));
    reader.limits(limits);
    let image = reader.decode().map_err(|_| "decode_failed")?;
    let dimensions = image.dimensions();
    if dimensions.0 == 0
        || dimensions.1 == 0
        || dimensions.0 > MAX_IMAGE_DIMENSION
        || dimensions.1 > MAX_IMAGE_DIMENSION
    {
        return Err("dimensions_invalid");
    }
    if u64::from(dimensions.0) * u64::from(dimensions.1) > MAX_IMAGE_PIXELS {
        return Err("pixel_limit");
    }
    let rgba = image.to_rgba8();
    if rgba.pixels().all(|pixel| pixel.0[3] == 0) {
        return Err("safety_rejected");
    }
    let image = DynamicImage::ImageRgba8(rgba);
    let web = resize(&image, WEB_MAX_DIMENSION);
    let thumbnail = resize(&image, THUMBNAIL_MAX_DIMENSION);
    let original_png = encode_png(&image)?;
    let web_png = encode_png(&web)?;
    let thumbnail_png = encode_png(&thumbnail)?;
    if [original_png.len(), web_png.len(), thumbnail_png.len()]
        .into_iter()
        .any(|length| length > MAX_PUBLISHED_PNG_BYTES)
    {
        return Err("byte_limit");
    }
    Ok(ProcessedImage {
        original_png,
        web_png,
        thumbnail_png,
        original_dimensions: dimensions,
        web_dimensions: web.dimensions(),
        thumbnail_dimensions: thumbnail.dimensions(),
    })
}

/// Exercises provider base64 and bounded PNG/WebP decode/resize boundaries.
#[cfg(feature = "fuzzing")]
pub fn fuzz_image_boundaries(bytes: &[u8]) {
    if bytes.len() > MAX_PROVIDER_IMAGE_BYTES {
        return;
    }
    if let Ok(decoded) = decode_provider_base64(std::str::from_utf8(bytes).ok()) {
        let _ = process_image(&decoded);
    }
    let _ = process_image(bytes);
}

fn resize(image: &DynamicImage, maximum: u32) -> DynamicImage {
    if image.width() <= maximum && image.height() <= maximum {
        image.clone()
    } else {
        image.resize(maximum, maximum, FilterType::Lanczos3)
    }
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|_| "decode_failed")?;
    Ok(bytes.into_inner())
}

fn generation_failure_code(error: &GenerationError) -> GenerationFailureCode {
    match error {
        GenerationError::Disabled { .. } => GenerationFailureCode::ProviderRejected,
        GenerationError::Timeout { .. } => GenerationFailureCode::Timeout,
        GenerationError::Transport(_) => GenerationFailureCode::ProviderUnavailable,
        GenerationError::HttpStatus { status, .. } if status.as_u16() == 429 => {
            GenerationFailureCode::RateLimited
        }
        GenerationError::HttpStatus { status, .. } if status.is_server_error() => {
            GenerationFailureCode::ProviderUnavailable
        }
        GenerationError::HttpStatus { .. } => GenerationFailureCode::ProviderRejected,
        GenerationError::InvalidConfiguration(_) => GenerationFailureCode::ProviderRejected,
        GenerationError::InvalidResponse { .. } => GenerationFailureCode::MalformedResponse,
    }
}

fn provider_status(error: &GenerationError) -> Option<u16> {
    match error {
        GenerationError::HttpStatus { status, .. } => Some(status.as_u16()),
        _ => None,
    }
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn valid_storage_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.starts_with('/')
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn set_private_directory_permissions(path: &Path) -> Result<(), SceneImageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(SceneImageError::Storage)?;
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<(), SceneImageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(SceneImageError::Storage)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use manchester_dnd_core::{
        CampaignPinSealReason, CommitEncounterCommand, ENCOUNTER_COMMIT_SCHEMA_VERSION,
        EXPLORATION_CHECK_SCHEMA_VERSION, SealedCampaignPins,
        encounter::{EncounterCommand, EncounterIntent},
        hero::ThemeId,
    };
    use mongodb::bson::{DateTime, doc};
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        config::{MongoConfig, MongoSchemaPolicy, SecretString},
        persistence::{CollectionName, MongoStore, SchemaReconciler},
    };

    fn fake_profile() -> LlmProfile {
        LlmProfile {
            backend: LlmBackend::Fake,
            base_url: None,
            api_key: None,
            model: None,
            timeout: Duration::from_secs(5),
            max_output_tokens: None,
            temperature: None,
            default_image_size: Some("1024x1024".to_owned()),
            estimated_request_cost_microusd: 0,
        }
    }

    fn image_governance() -> GenerationGovernanceConfig {
        let allowance = crate::config::GenerationBudgetAllowance {
            requests: 32,
            tokens: 0,
            latency_milliseconds: 120_000,
            cost_microusd: 0,
        };
        GenerationGovernanceConfig {
            campaign: allowance,
            turn: allowance,
            max_campaign_concurrency: 1,
            worker_batch_size: 1,
        }
    }

    async fn isolated_mongo_repository() -> Option<(MongoRepository, MongoStore, String)> {
        let Ok(uri) = std::env::var("MONGODB_TEST_URI") else {
            eprintln!("skipping scene-image MongoDB test: MONGODB_TEST_URI is not set");
            return None;
        };
        assert!(
            uri.starts_with("mongodb://root:") && uri.contains("127.0.0.1"),
            "MONGODB_TEST_URI must be the explicit local root test URI"
        );
        let database = format!("mdnd_scene_images_test_{}", Uuid::new_v4().simple());
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

    async fn drop_test_database(store: &MongoStore, database: &str) {
        assert!(
            database.starts_with("mdnd_scene_images_test_") && database != "manchester_dnd",
            "cleanup safeguard"
        );
        store.database().drop().await.unwrap();
    }

    async fn committed_scene(
        repository: MongoRepository,
    ) -> manchester_dnd_core::LocalCampaignViewDto {
        use crate::{
            application::{
                GameApplicationService, LOCAL_CAMPAIGN_SESSION_ID, LOCAL_EXPLORATION_ACTION_ID,
            },
            campaign_pins::CampaignPinRuntime,
            config::AccessMode,
            seed::SeedVault,
        };

        let pins = Arc::new(CampaignPinRuntime::bundled_for_tests());
        let application = GameApplicationService::new(
            AccessMode::LocalSingleUser,
            repository.clone(),
            Arc::new(SeedVault::from_key([0x44; 32])),
            pins.clone(),
        );
        application.load_local_campaign().await.unwrap();
        repository
            .seal_campaign_pins_for_test(
                LOCAL_HERO_OWNER_KEY,
                LOCAL_CAMPAIGN_SESSION_ID,
                &SealedCampaignPins {
                    seal_reason: CampaignPinSealReason::SelectedTheme,
                    pins: pins.pins_for_theme(ThemeId::RainboundBorough).unwrap(),
                    legacy_source: None,
                },
            )
            .await
            .unwrap();
        let summary = application
            .list_local_campaigns()
            .await
            .unwrap()
            .into_iter()
            .find(|campaign| campaign.campaign_session_id == LOCAL_CAMPAIGN_SESSION_ID)
            .expect("local campaign fixture should exist");
        application
            .start_local_play_session(crate::repository::StartPlaySessionCommand {
                lifecycle: crate::repository::CampaignLifecycleCommand {
                    schema_version: crate::repository::CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
                    campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
                    expected_lifecycle_revision: summary.lifecycle_revision,
                    idempotency_key: "scene-image-play-start".to_owned(),
                },
                play_session_id: "play-session:scene-image".to_owned(),
            })
            .await
            .unwrap();
        let initial = application.load_local_campaign().await.unwrap();
        application
            .attempt_exploration_check(manchester_dnd_core::AttemptExplorationCheckCommand {
                schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
                campaign_session_id: initial.campaign_session_id,
                character_id: initial.character_id,
                action_id: LOCAL_EXPLORATION_ACTION_ID.to_owned(),
                expected_revision: initial.revision,
                idempotency_key: "scene-image-exploration".to_owned(),
            })
            .await
            .unwrap();
        let ready = application.load_local_campaign().await.unwrap();
        let encounter = ready.encounter.as_ref().unwrap();
        application
            .commit_encounter_command(CommitEncounterCommand {
                schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
                campaign_session_id: ready.campaign_session_id.clone(),
                expected_campaign_revision: ready.revision,
                command: EncounterCommand::new(
                    encounter.state.revision,
                    "scene-image-start",
                    EncounterIntent::StartEncounter,
                ),
            })
            .await
            .unwrap();
        application.load_local_campaign().await.unwrap()
    }

    #[test]
    fn image_brief_is_closed_bounded_and_contains_no_private_input_channel() {
        let brief = ImageBrief::from_encounter(EncounterStatus::Victory, true).unwrap();
        let encoded = serde_json::to_string(&brief).unwrap();
        let prompt = brief.provider_prompt().unwrap();
        for forbidden in [
            "participant:",
            "source_id",
            "filesystem",
            "provider instruction",
            "SYNTHETIC_RAW_SOURCE_CANARY",
            "Mara",
        ] {
            assert!(!encoded.contains(forbidden));
            assert!(!prompt.contains(forbidden));
        }
        assert!(prompt.chars().count() < 4_000);
        assert!(brief.alt_text_context.chars().count() <= 500);
    }

    #[test]
    fn image_processing_rejects_spoofed_and_oversized_inputs_and_strips_metadata() {
        assert_eq!(
            process_image(b"not an image").unwrap_err(),
            "format_invalid"
        );
        assert_eq!(
            process_image(&[0xff, 0xd8, 0xff, 0x00]).unwrap_err(),
            "format_invalid"
        );
        let oversized = encode_png(&DynamicImage::new_rgba8(MAX_IMAGE_DIMENSION + 1, 1)).unwrap();
        assert!(matches!(
            process_image(&oversized),
            Err("dimensions_invalid" | "decode_failed")
        ));
        let transparent = encode_png(&DynamicImage::new_rgba8(1, 1)).unwrap();
        assert_eq!(process_image(&transparent).unwrap_err(), "safety_rejected");
        let fake = crate::generation::FakeImageGenerator;
        let response = tokio_test_image(fake);
        let raw = decode_provider_base64(response.images[0].base64_data.as_deref()).unwrap();
        let processed = process_image(&raw).unwrap();
        assert_eq!(&processed.original_png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(processed.original_png.len() <= MAX_PUBLISHED_PNG_BYTES);
    }

    fn tokio_test_image(
        fake: crate::generation::FakeImageGenerator,
    ) -> crate::generation::ImageGenerationResponse {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime
            .block_on(fake.generate_image(ImageGenerationRequest::one("safe fantasy scene")))
            .unwrap()
    }

    #[test]
    fn storage_keys_reject_traversal_and_absolute_paths() {
        assert!(valid_storage_key("artifacts/abcd/web.png"));
        assert!(!valid_storage_key("../secret"));
        assert!(!valid_storage_key("artifacts//web.png"));
        assert!(!valid_storage_key("/absolute.png"));
    }

    #[test]
    fn q09_limits_are_exact_public_constants() {
        assert_eq!(crate::repository::jobs::IMAGE_REQUESTS_PER_ROLLING_DAY, 3);
        assert_eq!(
            crate::repository::jobs::IMAGE_REQUESTS_PER_CAMPAIGN_LIFETIME,
            10
        );
        assert_eq!(IMAGE_REQUESTS_PER_TURN, 2);
    }

    #[tokio::test]
    async fn durable_image_request_replays_publishes_authorizes_and_replaces_once() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        let campaign = committed_scene(repository.clone()).await;
        let storage = tempfile::tempdir().unwrap();
        let profile = fake_profile();
        let service = SceneImageService::new(
            repository.clone(),
            Arc::new(crate::generation::FakeImageGenerator),
            &profile,
            &image_governance(),
            storage.path(),
        )
        .unwrap();

        let first = service
            .request(
                &campaign.campaign_session_id,
                campaign.revision,
                "scene-image-request-1",
                false,
                Some("correlation:image-request-1"),
            )
            .await
            .unwrap();
        assert!(!first.existing);
        assert_eq!(first.job.state, GenerationJobState::Queued);
        let replay = service
            .request(
                &campaign.campaign_session_id,
                campaign.revision,
                "scene-image-request-1",
                false,
                Some("correlation:image-request-replay"),
            )
            .await
            .unwrap();
        assert!(replay.existing);
        assert_eq!(replay.job.id, first.job.id);
        assert_eq!(replay.counts.source_turn, 1);

        assert_eq!(
            service.process_next("image-worker:test-1").await.unwrap(),
            SceneImageWorkerOutcome::Succeeded
        );
        let ready = service.status(&campaign.campaign_session_id).await.unwrap();
        assert_eq!(
            ready.latest_job.as_ref().map(|job| job.state),
            Some(GenerationJobState::Succeeded)
        );
        let artifact = ready.artifact.as_ref().unwrap();
        assert!(artifact.selected);
        let delivered = service
            .deliver(
                &campaign.campaign_session_id,
                &artifact.artifact_id,
                SceneImageVariant::Web,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.media_type, "image/png");
        assert_eq!(&delivered.bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert!(!delivered.alt_text.is_empty());
        assert!(
            service
                .deliver(
                    "another-campaign",
                    &artifact.artifact_id,
                    SceneImageVariant::Web
                )
                .await
                .unwrap()
                .is_none()
        );

        // A fresh service reconstructs the brief and status from MongoDB and
        // the protected root; no in-memory request body is required after restart.
        let restarted = SceneImageService::new(
            repository.clone(),
            Arc::new(crate::generation::FakeImageGenerator),
            &profile,
            &image_governance(),
            storage.path(),
        )
        .unwrap();
        assert!(
            restarted
                .status(&campaign.campaign_session_id)
                .await
                .unwrap()
                .artifact
                .is_some()
        );

        let replacement = restarted
            .request(
                &campaign.campaign_session_id,
                campaign.revision,
                "scene-image-request-2",
                true,
                Some("correlation:image-request-2"),
            )
            .await
            .unwrap();
        assert!(!replacement.existing);
        assert_eq!(
            restarted.process_next("image-worker:test-2").await.unwrap(),
            SceneImageWorkerOutcome::Succeeded
        );
        let replaced = restarted
            .status(&campaign.campaign_session_id)
            .await
            .unwrap();
        assert_eq!(replaced.counts.source_turn, 2);
        assert_ne!(
            replaced.artifact.as_ref().unwrap().artifact_id,
            artifact.artifact_id
        );
        let superseded = repository
            .load_generation_job(&campaign.campaign_session_id, &first.job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            superseded.retention_class,
            crate::repository::jobs::GenerationRetentionClass::UnselectedPresentation30Days
        );
        assert!(superseded.retention_delete_after.is_some());
        assert!(matches!(
            restarted
                .request(
                    &campaign.campaign_session_id,
                    campaign.revision,
                    "scene-image-request-3",
                    true,
                    Some("correlation:image-request-3"),
                )
                .await,
            Err(SceneImageError::ReplacementLimit)
        ));

        let expired = DateTime::from_millis(1);
        store
            .document_collection(CollectionName::GenerationJobs)
            .update_one(
                doc! { "_id": &first.job.id },
                doc! { "$set": { "purge_at": expired } },
            )
            .await
            .unwrap();
        store
            .document_collection(CollectionName::GeneratedAssets)
            .update_one(
                doc! { "_id": &artifact.artifact_id },
                doc! { "$set": { "purge_at": expired } },
            )
            .await
            .unwrap();
        let first_attempt = repository
            .list_generation_attempts(&campaign.campaign_session_id, &first.job.id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("completed image job should retain its attempt");
        let quarantine_key = restarted
            .storage
            .quarantine(&first_attempt.id, b"invalid-provider-bytes")
            .await
            .unwrap();
        repository
            .record_scene_image_quarantine(&NewSceneImageQuarantine {
                id: "image-quarantine:expired".to_owned(),
                job_id: first.job.id.clone(),
                attempt_id: first_attempt.id,
                campaign_session_id: campaign.campaign_session_id.clone(),
                byte_digest: Some(digest(b"invalid-provider-bytes")),
                byte_length: Some(u64::try_from(b"invalid-provider-bytes".len()).unwrap()),
                storage_key: Some(quarantine_key),
                reason_code: "format_invalid",
            })
            .await
            .unwrap();
        store
            .document_collection(CollectionName::QuarantinedAssets)
            .update_one(
                doc! { "_id": "image-quarantine:expired" },
                doc! { "$set": { "purge_at": expired } },
            )
            .await
            .unwrap();
        let cleaned = restarted.cleanup_expired(10).await.unwrap();
        assert_eq!(cleaned.records_deleted, 2);
        assert_eq!(cleaned.files_deleted, 4);
        assert!(
            restarted
                .deliver(
                    &campaign.campaign_session_id,
                    &artifact.artifact_id,
                    SceneImageVariant::Web,
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            restarted
                .deliver(
                    &campaign.campaign_session_id,
                    &replaced.artifact.as_ref().unwrap().artifact_id,
                    SceneImageVariant::Web,
                )
                .await
                .unwrap()
                .is_some()
        );
        drop_test_database(&store, &database).await;
    }

    #[derive(Debug)]
    struct UrlOnlyImageGenerator;

    #[async_trait::async_trait]
    impl ImageGenerator for UrlOnlyImageGenerator {
        async fn generate_image(
            &self,
            _request: ImageGenerationRequest,
        ) -> Result<crate::generation::ImageGenerationResponse, GenerationError> {
            Ok(crate::generation::ImageGenerationResponse {
                images: vec![crate::generation::GeneratedImage {
                    url: Some("https://169.254.169.254/latest/meta-data".to_owned()),
                    base64_data: None,
                    revised_prompt: None,
                }],
            })
        }
    }

    #[tokio::test]
    async fn provider_urls_are_never_fetched_and_are_quarantined() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        let campaign = committed_scene(repository.clone()).await;
        let storage = tempfile::tempdir().unwrap();
        let service = SceneImageService::new(
            repository,
            Arc::new(UrlOnlyImageGenerator),
            &fake_profile(),
            &image_governance(),
            storage.path(),
        )
        .unwrap();
        service
            .request(
                &campaign.campaign_session_id,
                campaign.revision,
                "scene-image-url-request",
                false,
                Some("correlation:image-url-request"),
            )
            .await
            .unwrap();
        assert_eq!(
            service.process_next("image-worker:url-test").await.unwrap(),
            SceneImageWorkerOutcome::Failed
        );
        let status = service.status(&campaign.campaign_session_id).await.unwrap();
        assert_eq!(
            status
                .latest_job
                .as_ref()
                .and_then(|job| job.last_failure_code),
            Some(GenerationFailureCode::InvalidArtifact)
        );
        assert!(status.artifact.is_none());
        let quarantine_count = store
            .document_collection(CollectionName::QuarantinedAssets)
            .count_documents(doc! { "reason_code": "provider_url_rejected" })
            .await
            .unwrap();
        assert_eq!(quarantine_count, 1);
        drop_test_database(&store, &database).await;
    }

    #[derive(Debug)]
    struct BlockingImageGenerator {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl ImageGenerator for BlockingImageGenerator {
        async fn generate_image(
            &self,
            request: ImageGenerationRequest,
        ) -> Result<crate::generation::ImageGenerationResponse, GenerationError> {
            self.started.notify_one();
            self.release.notified().await;
            crate::generation::FakeImageGenerator
                .generate_image(request)
                .await
        }
    }

    #[tokio::test]
    async fn cancellation_wins_a_running_provider_race_without_an_artifact() {
        let Some((repository, store, database)) = isolated_mongo_repository().await else {
            return;
        };
        let campaign = committed_scene(repository.clone()).await;
        let storage = tempfile::tempdir().unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let service = SceneImageService::new(
            repository,
            Arc::new(BlockingImageGenerator {
                started: started.clone(),
                release: release.clone(),
            }),
            &fake_profile(),
            &image_governance(),
            storage.path(),
        )
        .unwrap();
        let enqueued = service
            .request(
                &campaign.campaign_session_id,
                campaign.revision,
                "scene-image-cancel-request",
                false,
                Some("correlation:image-cancel-request"),
            )
            .await
            .unwrap();
        let worker = {
            let service = service.clone();
            tokio::spawn(async move { service.process_next("image-worker:cancel-test").await })
        };
        started.notified().await;
        let cancelled = service
            .cancel(&campaign.campaign_session_id, &enqueued.job.id)
            .await
            .unwrap();
        assert_eq!(cancelled.state, GenerationJobState::Cancelled);
        release.notify_one();
        assert_eq!(
            worker.await.unwrap().unwrap(),
            SceneImageWorkerOutcome::LostLease
        );
        let status = service.status(&campaign.campaign_session_id).await.unwrap();
        assert_eq!(
            status.latest_job.as_ref().map(|job| job.state),
            Some(GenerationJobState::Cancelled)
        );
        assert!(status.artifact.is_none());
        drop_test_database(&store, &database).await;
    }
}
