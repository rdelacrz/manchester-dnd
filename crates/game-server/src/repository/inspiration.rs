//! Transactional MongoDB persistence for consented private inspiration.
//!
//! Only body-free, minimized projections cross this boundary. Campaign safety
//! policy is embedded in the campaign aggregate; independently mutable source,
//! consent, veto, selection, and derived-work state uses the corresponding
//! private-inspiration collections. Receipts, audits, and deletion tombstones
//! use their consolidated MongoDB collections.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use manchester_dnd_core::{
    Character, RollAlgorithm, SessionDto, SessionEventDto, SessionEventPayload, SessionStatus,
    Sha256Digest, encounter::EncounterStatus, hero::HeroCharacter,
};
use mongodb::{
    ClientSession, Collection,
    bson::{self, Bson, DateTime, Document, doc},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    error::{PersistenceError, PrivateInspirationError},
    events::{
        DeterministicEventRandom, EventEligibility, EventPrompt, EventPromptLoader,
        EventSelectionAudit, RuntimeEventPrompt,
    },
    inspiration::{
        ApplyInspirationVetoCommand, ApplyPresentationPrivacyCommand,
        CampaignInspirationRedactedExportV1, CampaignInspirationSettingsProjection,
        CampaignInspirationTone, ConfigureCampaignInspirationCommand, ConsentGrantProjection,
        ConsentGrantState, DeleteParticipantPrivateDataCommand, DeletionTombstonePurgeOutcome,
        DerivedArtifactPolicy, DerivedWorkProjection, DisableCampaignInspirationCommand,
        DurableNoSelectionReason, GlobalInspirationControlProjection, GrantConsentCommand,
        InspirationAudience, InspirationMedia, InspirationTransformation, InspirationVetoScope,
        OpaqueInspirationId, PARTICIPANT_DELETION_TOMBSTONE_SECONDS,
        PRIVATE_INSPIRATION_EXPORT_SCHEMA_VERSION, PRIVATE_INSPIRATION_SCHEMA_VERSION,
        ParticipantDeletionOutcome, ParticipantVerificationProjection, PresentationPrivacyAction,
        PresentationPrivacyOutcome, PrivacyTransitionOutcome, PrivateInspirationSelection,
        PurgeExpiredParticipantDeletionTombstonesCommand, Q11_CONSERVATIVE_POLICY_ID,
        RecordRestrictedDiagnosticAccessCommand, RegisterDerivedWorkCommand,
        RegisterSourceVersionCommand, RequestInspirationSelectionCommand,
        ResolvedInspirationSelectionAuthority, RestrictedDiagnosticAccessProjection,
        RestrictedDiagnosticDecision, ReviewSourceVersionCommand, RevokeConsentCommand,
        SetCampaignInspirationPauseCommand, SetGlobalInspirationControlCommand, SourceReviewState,
        SourceVersionProjection, VerifyParticipantCommand, VetoProjection, fingerprint,
        internal_id, invalid,
    },
    persistence::CollectionName,
};

use super::{MongoRepository, map_persistence, presentations::PRIVATE_INSPIRATION_REDACTION_BODY};

const SYSTEM_ACTOR_ID: &str = "account:system-private-inspiration";
const GLOBAL_SCOPE_ID: &str = "system:private-inspiration";
const GLOBAL_STATE_ID: &str = "command-receipt:private-inspiration-global-state";
const GLOBAL_STATE_SCOPE: &str = "private_inspiration_global_state";
const GLOBAL_COMMAND_SCOPE: &str = "private_inspiration_global";
const CAMPAIGN_COMMAND_SCOPE: &str = "private_inspiration";
const RESTRICTED_COMMAND_SCOPE: &str = "private_inspiration_restricted";
const PARTICIPANT_TOMBSTONE_KIND: &str = "private_inspiration_participant";
const MAX_CAMPAIGN_SAFETY_CODES: usize = 64;

#[derive(Debug, Clone, Deserialize)]
struct InspirationCampaignDocument {
    #[serde(rename = "_id")]
    id: String,
    revision: i64,
    #[serde(default)]
    theme_id: String,
    session: SessionDto,
    #[serde(default)]
    safety: Option<CampaignSafetyDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CampaignSafetyDocument {
    schema_version: i64,
    revision: i64,
    enabled: bool,
    generation_paused: bool,
    safety_setup_complete: bool,
    adults_only: bool,
    fictional_distance: String,
    tone: String,
    audience: String,
    media: String,
    q11_policy_id: String,
    rng_cursor: i64,
    allowed_sensitivities: Vec<String>,
    lines: Vec<String>,
    veils: Vec<String>,
    excluded_topics: Vec<String>,
    excluded_participant_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    safety_setup_evidence_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    safety_reviewer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    safety_reviewed_at_epoch: Option<i64>,
    updated_at_epoch: i64,
}

impl CampaignSafetyDocument {
    fn from_command(
        command: &ConfigureCampaignInspirationCommand,
        revision: u64,
        now: u64,
    ) -> Result<Self, PrivateInspirationError> {
        let setup = command.safety_setup.as_ref();
        Ok(Self {
            schema_version: i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
            revision: inspiration_i64(revision, "settings_revision_range")?,
            enabled: command.enabled,
            generation_paused: false,
            safety_setup_complete: setup.is_some(),
            adults_only: true,
            fictional_distance: "high_locked".to_owned(),
            tone: setup
                .map_or(CampaignInspirationTone::GothicAdventure, |value| value.tone)
                .as_str()
                .to_owned(),
            audience: InspirationAudience::PrivateCampaign.as_str().to_owned(),
            media: InspirationMedia::Text.as_str().to_owned(),
            q11_policy_id: Q11_CONSERVATIVE_POLICY_ID.to_owned(),
            rng_cursor: 0,
            allowed_sensitivities: string_set(setup.map(|value| &value.allowed_sensitivity_codes)),
            lines: string_set(setup.map(|value| &value.line_codes)),
            veils: string_set(setup.map(|value| &value.veil_codes)),
            excluded_topics: string_set(setup.map(|value| &value.excluded_topic_codes)),
            excluded_participant_ids: string_set(
                setup.map(|value| &value.excluded_participant_ids),
            ),
            safety_setup_evidence_digest: setup
                .map(|value| value.evidence_digest.as_str().to_owned()),
            safety_reviewer_id: setup.map(|value| value.reviewer_id.as_str().to_owned()),
            safety_reviewed_at_epoch: setup
                .map(|_| inspiration_i64(now, "safety_reviewed_at_range"))
                .transpose()?,
            updated_at_epoch: inspiration_i64(now, "settings_updated_at_range")?,
        })
    }

    fn projection(
        &self,
        campaign_id: &str,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let allowed = validated_stored_ids(&self.allowed_sensitivities)?;
        let lines = validated_stored_ids(&self.lines)?;
        let veils = validated_stored_ids(&self.veils)?;
        let excluded_topics = validated_stored_ids(&self.excluded_topics)?;
        let excluded_participants = validated_stored_ids(&self.excluded_participant_ids)?;
        if self.schema_version != i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
            || self.fictional_distance != "high_locked"
            || self.audience != InspirationAudience::PrivateCampaign.as_str()
            || self.media != InspirationMedia::Text.as_str()
            || self.q11_policy_id != Q11_CONSERVATIVE_POLICY_ID
            || !self.adults_only
            || [
                allowed.len(),
                lines.len(),
                veils.len(),
                excluded_topics.len(),
                excluded_participants.len(),
            ]
            .into_iter()
            .any(|length| length > MAX_CAMPAIGN_SAFETY_CODES)
            || !lines.is_disjoint(&veils)
            || !lines.is_disjoint(&excluded_topics)
            || !veils.is_disjoint(&excluded_topics)
            || (!self.safety_setup_complete
                && (!allowed.is_empty()
                    || !lines.is_empty()
                    || !veils.is_empty()
                    || !excluded_topics.is_empty()
                    || !excluded_participants.is_empty()
                    || self.safety_setup_evidence_digest.is_some()
                    || self.safety_reviewer_id.is_some()
                    || self.safety_reviewed_at_epoch.is_some()))
        {
            return Err(invalid("stored_settings_policy"));
        }
        Ok(CampaignInspirationSettingsProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            campaign_session_id: OpaqueInspirationId::new(campaign_id.to_owned())?,
            revision: inspiration_u64(self.revision, "stored_settings_revision")?,
            enabled: self.enabled,
            generation_paused: self.generation_paused,
            safety_setup_complete: self.safety_setup_complete,
            adults_only: true,
            fictional_distance_locked_high: true,
            tone: CampaignInspirationTone::parse(&self.tone)?,
            line_count: bounded_count(self.lines.len(), "stored_line_count")?,
            veil_count: bounded_count(self.veils.len(), "stored_veil_count")?,
            excluded_topic_count: bounded_count(
                self.excluded_topics.len(),
                "stored_excluded_topic_count",
            )?,
            excluded_participant_count: bounded_count(
                self.excluded_participant_ids.len(),
                "stored_excluded_participant_count",
            )?,
            audience: InspirationAudience::PrivateCampaign,
            media: InspirationMedia::Text,
            q11_policy_id: self.q11_policy_id.clone(),
            updated_at_epoch: inspiration_u64(self.updated_at_epoch, "stored_settings_updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    logical_id: String,
    revision: i64,
    source_digest: String,
    category_id: String,
    owner_participant_id: String,
    review_state: String,
    q11_screened: bool,
    audience: String,
    transformation: String,
    provenance_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<DateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_epoch: Option<i64>,
    participants: Vec<String>,
    sensitivities: Vec<String>,
    media: Vec<String>,
    themes: Vec<String>,
    runtime_facts: RuntimeFactsDocument,
    runtime_projection: RuntimeEventPrompt,
    projection_digest: String,
    projection: SourceVersionProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review_evidence_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reviewer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reviewed_at_epoch: Option<i64>,
    registered_at_epoch: i64,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeFactsDocument {
    neutral_facts: Vec<String>,
}

impl SourceDocument {
    fn stored(&self) -> Result<StoredSource, PrivateInspirationError> {
        let participants = validated_stored_ids(&self.participants)?;
        let sensitivities = validated_stored_ids(&self.sensitivities)?;
        let eligible_media = self
            .media
            .iter()
            .map(|value| InspirationMedia::parse(value))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let eligible_theme_pack_ids = self
            .themes
            .iter()
            .map(|value| OpaqueInspirationId::new(value.clone()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if self.schema_version != i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
            || self.audience != InspirationAudience::PrivateCampaign.as_str()
            || self.transformation != InspirationTransformation::HighFictionDistanceV1.as_str()
            || self.runtime_facts.neutral_facts != self.runtime_projection.neutral_facts
            || eligible_media.len() != self.media.len()
            || eligible_theme_pack_ids.len() != self.themes.len()
        {
            return Err(invalid("stored_source_policy"));
        }
        let projection = SourceVersionProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            source_id: OpaqueInspirationId::new(self.logical_id.clone())?,
            source_version: inspiration_u64(self.revision, "stored_source_revision")?,
            source_digest: Sha256Digest::new(self.source_digest.clone())
                .map_err(|_| invalid("stored_source_digest"))?,
            category_id: OpaqueInspirationId::new(self.category_id.clone())?,
            review_state: SourceReviewState::parse(&self.review_state)?,
            q11_screened: self.q11_screened,
            participant_count: bounded_count(participants.len(), "stored_participant_count")?,
            sensitivity_count: bounded_count(sensitivities.len(), "stored_sensitivity_count")?,
            eligible_media,
            eligible_theme_pack_ids,
            expires_at_epoch: self
                .expires_at_epoch
                .map(|value| inspiration_u64(value, "stored_source_expiry"))
                .transpose()?,
        };
        Ok(StoredSource {
            projection,
            participants,
            sensitivities,
            theme_pack_ids: self.themes.iter().cloned().collect(),
        })
    }
}

#[derive(Debug, Clone)]
struct StoredSource {
    projection: SourceVersionProjection,
    participants: BTreeSet<String>,
    sensitivities: BTreeSet<String>,
    theme_pack_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsentDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    campaign_id: String,
    source_id: String,
    source_revision: i64,
    source_digest: String,
    participant_id: String,
    version: i64,
    audience: String,
    media: String,
    transformation: String,
    artifact_policy: String,
    sensitivities: Vec<String>,
    reviewer_id: String,
    participant_confirmation_digest: String,
    review_evidence_digest: String,
    state: String,
    projection: ConsentGrantProjection,
    granted_at_epoch: i64,
    expires_at_epoch: i64,
    expires_at: DateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at_epoch: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revocation_code: Option<String>,
    created_at: DateTime,
    updated_at: DateTime,
}

impl ConsentDocument {
    fn checked_projection(&self) -> Result<ConsentGrantProjection, PrivateInspirationError> {
        validated_stored_ids(&self.sensitivities)?;
        if self.schema_version != i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
            || self.version <= 0
            || self.id != self.projection.grant_id.as_str()
            || !manchester_dnd_core::is_valid_opaque_id(&self.campaign_id)
            || self.source_id != self.projection.source_id.as_str()
            || inspiration_u64(self.source_revision, "stored_consent_source_revision")?
                != self.projection.source_version
            || self.source_digest != self.projection.source_digest.as_str()
            || self.participant_id != self.projection.participant_id.as_str()
            || self.audience != self.projection.audience.as_str()
            || self.media != self.projection.media.as_str()
            || self.transformation != self.projection.transformation.as_str()
            || self.artifact_policy != self.projection.artifact_policy.as_str()
            || !manchester_dnd_core::is_valid_opaque_id(&self.reviewer_id)
            || Sha256Digest::new(self.participant_confirmation_digest.clone()).is_err()
            || Sha256Digest::new(self.review_evidence_digest.clone()).is_err()
            || self.state != consent_state_name(self.projection.state)
            || inspiration_u64(self.expires_at_epoch, "stored_consent_expiry")?
                != self.projection.expires_at_epoch
        {
            return Err(invalid("stored_consent_policy"));
        }
        Ok(self.projection.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkDocument {
    #[serde(rename = "_id")]
    id: String,
    schema_version: i64,
    campaign_id: String,
    /// Unique storage binding used by the collection index.
    selection_id: String,
    /// The durable selection exposed by the application projection.
    source_selection_id: String,
    source_id: String,
    source_revision: i64,
    source_digest: String,
    work_kind: String,
    state: String,
    artifact_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancellation_requested_at_epoch: Option<i64>,
    created_at_epoch: i64,
    created_at: DateTime,
    updated_at: DateTime,
}

impl WorkDocument {
    fn validate(&self) -> Result<(), PrivateInspirationError> {
        if self.schema_version != i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION)
            || !manchester_dnd_core::is_valid_opaque_id(&self.id)
            || !manchester_dnd_core::is_valid_opaque_id(&self.campaign_id)
            || !manchester_dnd_core::is_valid_opaque_id(&self.selection_id)
            || !manchester_dnd_core::is_valid_opaque_id(&self.source_selection_id)
            || !manchester_dnd_core::is_valid_opaque_id(&self.source_id)
            || self.source_revision <= 0
            || Sha256Digest::new(self.source_digest.clone()).is_err()
            || !matches!(self.work_kind.as_str(), "text" | "image" | "recap")
            || !matches!(
                self.state.as_str(),
                "pending" | "cancellation_requested" | "completed" | "redacted" | "deleted"
            )
            || DerivedArtifactPolicy::parse(&self.artifact_policy).is_err()
            || matches!(self.state.as_str(), "completed" | "redacted")
                != self.completed_artifact_id.is_some()
        {
            return Err(invalid("stored_derived_work"));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VetoReceipt {
    veto: VetoProjection,
    transition: PrivacyTransitionOutcome,
}

enum ReceiptReplay<T> {
    Missing,
    Replay(T),
    Conflict,
}

impl MongoRepository {
    /// Loads only minimized, integrity-bound runtime prompt projections.
    pub(crate) async fn load_private_inspiration_runtime_prompts(
        &self,
    ) -> Result<Vec<EventPrompt>, PrivateInspirationError> {
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let mut cursor = sources
            .find(doc! {})
            .sort(doc! { "logical_id": 1_i64, "revision": 1_i64 })
            .await
            .map_err(|error| private_mongo("load private inspiration runtime prompts", error))?;
        let mut prompts = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| private_mongo("read private inspiration runtime prompts", error))?
        {
            let source = cursor.deserialize_current().map_err(|_| {
                private_persistence(private_schema(
                    CollectionName::PrivateInspirationSources,
                    "runtime prompt document could not be decoded",
                ))
            })?;
            let stored = source.stored()?;
            if fingerprint(&source.runtime_projection)?.as_str() != source.projection_digest {
                return Err(invalid("runtime_prompt_projection_digest"));
            }
            prompts.push(
                EventPrompt::from_runtime_projection(
                    stored.projection.source_id.as_str(),
                    stored.projection.source_digest.clone(),
                    source.participants,
                    source.sensitivities,
                    source.runtime_projection,
                )
                .map_err(|_| invalid("stored_runtime_prompt"))?,
            );
        }
        Ok(prompts)
    }

    pub(crate) async fn load_global_inspiration_control(
        &self,
    ) -> Result<GlobalInspirationControlProjection, PrivateInspirationError> {
        let collection = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        Ok(load_global_control(&collection, None)
            .await
            .map_err(private_persistence)?
            .unwrap_or_else(default_global_control))
    }

    pub(crate) async fn record_private_inspiration_restricted_access(
        &self,
        command: &RecordRestrictedDiagnosticAccessCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<RestrictedDiagnosticAccessProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "restricted_access_time_range")?;
        let audit_id = internal_id("restricted-access")?;
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        let scope_id = command
            .campaign_session_id
            .as_ref()
            .map_or(GLOBAL_SCOPE_ID.to_owned(), ToString::to_string);
        self.with_transaction(move |session| {
            let receipts = receipts.clone();
            let audits = audits.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            let audit_id = audit_id.clone();
            let scope_id = scope_id.clone();
            Box::pin(async move {
                match load_receipt::<RestrictedDiagnosticAccessProjection>(
                    &receipts,
                    session,
                    RESTRICTED_COMMAND_SCOPE,
                    &scope_id,
                    command.idempotency_key.as_str(),
                    "restricted_diagnostic_access",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::ScopeDenied));
                    }
                    ReceiptReplay::Missing => {}
                }
                let projection = RestrictedDiagnosticAccessProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    audit_id: audit_id.clone(),
                    campaign_session_id: command.campaign_session_id.clone(),
                    operator_id: command.operator_id.clone(),
                    access_kind: command.access_kind,
                    purpose: command.purpose,
                    subject_id: command.subject_id.clone(),
                    evidence_digest: command.evidence_digest.clone(),
                    decision: command.decision,
                    occurred_at_epoch: now,
                };
                insert_privacy_audit(
                    &audits,
                    session,
                    command
                        .campaign_session_id
                        .as_ref()
                        .map(OpaqueInspirationId::as_str),
                    "restricted_diagnostic_access",
                    "restricted_diagnostic",
                    command.subject_id.as_str(),
                    Some(command.access_kind.as_str()),
                    if command.decision == RestrictedDiagnosticDecision::Allowed {
                        "applied"
                    } else {
                        "denied"
                    },
                    now_date,
                    Some(doc! {
                        "operator_id": command.operator_id.as_str(),
                        "purpose": command.purpose.as_str(),
                        "evidence_digest": command.evidence_digest.as_str(),
                        "decision": command.decision.as_str(),
                    }),
                )
                .await?;
                insert_receipt(
                    &receipts,
                    session,
                    RESTRICTED_COMMAND_SCOPE,
                    &scope_id,
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "restricted_diagnostic_access",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn set_global_inspiration_control(
        &self,
        command: &SetGlobalInspirationControlCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<GlobalInspirationControlProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "global_control_time_range")?;
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let receipts = receipts.clone();
            let audits = audits.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                match load_receipt::<GlobalInspirationControlProjection>(
                    &receipts,
                    session,
                    GLOBAL_COMMAND_SCOPE,
                    GLOBAL_SCOPE_ID,
                    command.idempotency_key.as_str(),
                    "global_control_set",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let current = load_global_control(&receipts, Some(session))
                    .await?
                    .unwrap_or_else(default_global_control);
                if current.revision != command.expected_revision {
                    return Ok(Err(PrivateInspirationError::RevisionConflict {
                        expected: command.expected_revision,
                        current: current.revision,
                    }));
                }
                let revision = current
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| {
                        private_schema(
                            CollectionName::CommandReceipts,
                            "global control revision overflow",
                        )
                    })?;
                let projection = GlobalInspirationControlProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    revision,
                    generation_disabled: command.generation_disabled,
                    updated_at_epoch: now,
                };
                write_global_control(
                    &receipts,
                    session,
                    &current,
                    &projection,
                    &command,
                    &request_fingerprint,
                    now_date,
                )
                .await?;
                if command.generation_disabled {
                    quarantine_all_private_inspiration_work(
                        &work,
                        &presentations,
                        &audits,
                        session,
                        now,
                        now_date,
                    )
                    .await?;
                }
                insert_privacy_audit(
                    &audits,
                    session,
                    None,
                    "global_kill_switch",
                    "campaign",
                    "global:private-inspiration",
                    None,
                    "applied",
                    now_date,
                    Some(doc! {
                        "operator_id": command.operator_id.as_str(),
                        "evidence_digest": command.evidence_digest.as_str(),
                        "generation_disabled": command.generation_disabled,
                        "revision": inspiration_i64_persistence(revision, CollectionName::CommandReceipts)?,
                    }),
                )
                .await?;
                insert_receipt(
                    &receipts,
                    session,
                    GLOBAL_COMMAND_SCOPE,
                    GLOBAL_SCOPE_ID,
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "global_control_set",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn purge_expired_private_inspiration_deletion_tombstones(
        &self,
        command: &PurgeExpiredParticipantDeletionTombstonesCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<DeletionTombstonePurgeOutcome, PrivateInspirationError> {
        let now_date = epoch_date(now, "tombstone_purge_time_range")?;
        let cutoff = epoch_date(
            command.delete_after_epoch_inclusive,
            "tombstone_purge_cutoff_range",
        )?;
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let tombstones = self
            .store()
            .document_collection(CollectionName::DeletionTombstones);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let receipts = receipts.clone();
            let audits = audits.clone();
            let tombstones = tombstones.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                match load_receipt::<DeletionTombstonePurgeOutcome>(
                    &receipts,
                    session,
                    GLOBAL_COMMAND_SCOPE,
                    GLOBAL_SCOPE_ID,
                    command.idempotency_key.as_str(),
                    "tombstone_purge",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(outcome) => return Ok(Ok(outcome)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                serialize_global_operation(&receipts, session, now_date).await?;
                let mut cursor = tombstones
                    .find(doc! {
                        "entity_kind": PARTICIPANT_TOMBSTONE_KIND,
                        "purge_at": { "$lte": cutoff },
                    })
                    .sort(doc! { "entity_id": 1_i64, "_id": 1_i64 })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load expired inspiration tombstones", error)
                    })?;
                let mut ids = Vec::new();
                let mut participant_ids = Vec::new();
                while cursor.advance(&mut *session).await.map_err(|error| {
                    PersistenceError::mongo("read expired inspiration tombstones", error)
                })? {
                    let document = cursor.deserialize_current().map_err(|_| {
                        private_schema(
                            CollectionName::DeletionTombstones,
                            "tombstone document could not be decoded",
                        )
                    })?;
                    ids.push(required_string(
                        &document,
                        "_id",
                        CollectionName::DeletionTombstones,
                    )?);
                    participant_ids.push(required_string(
                        &document,
                        "entity_id",
                        CollectionName::DeletionTombstones,
                    )?);
                }
                if !ids.is_empty() {
                    tombstones
                        .delete_many(doc! { "_id": { "$in": &ids } })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("purge inspiration tombstones", error)
                        })?;
                }
                for participant_id in &participant_ids {
                    insert_privacy_audit(
                        &audits,
                        session,
                        None,
                        "deletion_tombstone_expired",
                        "participant",
                        participant_id,
                        Some(command.operator_id.as_str()),
                        "applied",
                        now_date,
                        None,
                    )
                    .await?;
                }
                let outcome = DeletionTombstonePurgeOutcome {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    delete_after_epoch_inclusive: command.delete_after_epoch_inclusive,
                    purged_count: u32::try_from(ids.len()).map_err(|_| {
                        private_schema(
                            CollectionName::DeletionTombstones,
                            "purged tombstone count exceeds u32",
                        )
                    })?,
                    applied_at_epoch: now,
                };
                insert_receipt(
                    &receipts,
                    session,
                    GLOBAL_COMMAND_SCOPE,
                    GLOBAL_SCOPE_ID,
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "tombstone_purge",
                    &request_fingerprint,
                    &outcome,
                    now_date,
                )
                .await?;
                Ok(Ok(outcome))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn load_private_inspiration_campaign_settings(
        &self,
        campaign_session_id: &OpaqueInspirationId,
    ) -> Result<Option<CampaignInspirationSettingsProjection>, PrivateInspirationError> {
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let campaign = campaigns
            .find_one(doc! { "_id": campaign_session_id.as_str() })
            .await
            .map_err(|error| private_mongo("load private inspiration campaign settings", error))?
            .ok_or(PrivateInspirationError::NotFound)?;
        validate_campaign(&campaign)?;
        campaign
            .safety
            .as_ref()
            .map(|safety| safety.projection(&campaign.id))
            .transpose()
    }
}

fn string_set(values: Option<&BTreeSet<OpaqueInspirationId>>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(BTreeSet::iter)
        .map(ToString::to_string)
        .collect()
}

fn validated_stored_ids(values: &[String]) -> Result<BTreeSet<String>, PrivateInspirationError> {
    let unique = values.iter().cloned().collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err(invalid("stored_duplicate_identifier"));
    }
    for value in &unique {
        OpaqueInspirationId::new(value.clone())?;
    }
    Ok(unique)
}

fn bounded_count(value: usize, code: &'static str) -> Result<u32, PrivateInspirationError> {
    u32::try_from(value).map_err(|_| invalid(code))
}

fn inspiration_i64(value: u64, code: &'static str) -> Result<i64, PrivateInspirationError> {
    i64::try_from(value).map_err(|_| invalid(code))
}

fn inspiration_u64(value: i64, code: &'static str) -> Result<u64, PrivateInspirationError> {
    u64::try_from(value).map_err(|_| invalid(code))
}

fn inspiration_i64_persistence(
    value: u64,
    collection: CollectionName,
) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| {
        private_schema(
            collection,
            "unsigned value exceeds MongoDB signed integer range",
        )
    })
}

fn inspiration_u64_persistence(
    value: i64,
    collection: CollectionName,
) -> Result<u64, PersistenceError> {
    u64::try_from(value)
        .map_err(|_| private_schema(collection, "stored integer is negative or outside u64"))
}

fn epoch_date(value: u64, code: &'static str) -> Result<DateTime, PrivateInspirationError> {
    let milliseconds = value
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| invalid(code))?;
    Ok(DateTime::from_millis(milliseconds))
}

fn private_schema(collection: CollectionName, detail: &'static str) -> PersistenceError {
    PersistenceError::SchemaDrift {
        collection: collection.as_str().to_owned(),
        detail: detail.to_owned(),
    }
}

fn domain_as_schema(_: PrivateInspirationError) -> PersistenceError {
    private_schema(
        CollectionName::PrivateInspirationSources,
        "stored private inspiration domain projection is invalid",
    )
}

fn private_persistence(error: PersistenceError) -> PrivateInspirationError {
    match error {
        PersistenceError::NotFound {
            entity: "private inspiration campaign",
            ..
        } => PrivateInspirationError::NotFound,
        PersistenceError::RevisionConflict {
            entity: "private inspiration settings",
            expected,
            actual,
            ..
        }
        | PersistenceError::RevisionConflict {
            entity: "private inspiration global control",
            expected,
            actual,
            ..
        } => PrivateInspirationError::RevisionConflict {
            expected,
            current: actual,
        },
        other => PrivateInspirationError::Repository(map_persistence(other)),
    }
}

fn private_mongo(operation: &'static str, error: mongodb::error::Error) -> PrivateInspirationError {
    private_persistence(PersistenceError::mongo(operation, error))
}

fn bson_value<T: Serialize>(
    value: &T,
    collection: CollectionName,
) -> Result<Bson, PersistenceError> {
    bson::to_bson(value)
        .map_err(|_| private_schema(collection, "private inspiration BSON encoding failed"))
}

fn bson_document<T: Serialize>(
    value: &T,
    collection: CollectionName,
) -> Result<Document, PersistenceError> {
    bson::to_document(value)
        .map_err(|_| private_schema(collection, "private inspiration BSON encoding failed"))
}

fn decode_field<T: DeserializeOwned>(
    document: &Document,
    field: &str,
    collection: CollectionName,
) -> Result<T, PersistenceError> {
    let value = document
        .get(field)
        .cloned()
        .ok_or_else(|| private_schema(collection, "required BSON field is missing"))?;
    bson::from_bson(value)
        .map_err(|_| private_schema(collection, "private inspiration BSON decoding failed"))
}

fn required_string(
    document: &Document,
    field: &str,
    collection: CollectionName,
) -> Result<String, PersistenceError> {
    document
        .get_str(field)
        .map(str::to_owned)
        .map_err(|_| private_schema(collection, "required string field is missing"))
}

fn optional_string_bson(value: Option<&str>) -> Bson {
    value.map_or(Bson::Null, |value| Bson::String(value.to_owned()))
}

fn optional_u64_bson(
    value: Option<u64>,
    collection: CollectionName,
) -> Result<Bson, PersistenceError> {
    value
        .map(|value| inspiration_i64_persistence(value, collection).map(Bson::Int64))
        .transpose()
        .map(|value| value.unwrap_or(Bson::Null))
}

const fn consent_state_name(state: ConsentGrantState) -> &'static str {
    match state {
        ConsentGrantState::Active => "active",
        ConsentGrantState::Expired => "expired",
        ConsentGrantState::Revoked => "revoked",
    }
}

const fn artifact_policy_rank(policy: DerivedArtifactPolicy) -> u8 {
    match policy {
        DerivedArtifactPolicy::DeleteDerived => 3,
        DerivedArtifactPolicy::RedactDerived => 2,
        DerivedArtifactPolicy::RetainMinimalAudit => 1,
    }
}

fn default_global_control() -> GlobalInspirationControlProjection {
    GlobalInspirationControlProjection {
        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
        revision: 1,
        generation_disabled: false,
        updated_at_epoch: 0,
    }
}

fn validate_campaign(
    campaign: &InspirationCampaignDocument,
) -> Result<(), PrivateInspirationError> {
    campaign
        .session
        .validate()
        .map_err(|_| invalid("stored_campaign_validation"))?;
    let revision = inspiration_u64(campaign.revision, "stored_campaign_revision")?;
    if campaign.session.id != campaign.id
        || campaign.session.last_event_sequence.checked_add(1) != Some(revision)
    {
        return Err(invalid("stored_campaign_identity_or_revision"));
    }
    Ok(())
}

async fn load_campaign_in_session(
    campaigns: &Collection<InspirationCampaignDocument>,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<InspirationCampaignDocument, PersistenceError> {
    let campaign = campaigns
        .find_one(doc! { "_id": campaign_id })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load private inspiration campaign", error))?
        .ok_or_else(|| PersistenceError::NotFound {
            entity: "private inspiration campaign",
            id: campaign_id.to_owned(),
        })?;
    validate_campaign(&campaign).map_err(domain_as_schema)?;
    Ok(campaign)
}

async fn replace_campaign_safety(
    campaigns: &Collection<InspirationCampaignDocument>,
    session: &mut ClientSession,
    campaign_id: &str,
    current_revision: Option<u64>,
    safety: &CampaignSafetyDocument,
    now: DateTime,
) -> Result<(), PersistenceError> {
    let mut filter = doc! { "_id": campaign_id };
    match current_revision {
        Some(revision) => {
            filter.insert(
                "safety.revision",
                inspiration_i64_persistence(revision, CollectionName::Campaigns)?,
            );
        }
        None => {
            filter.insert("safety", Bson::Null);
        }
    }
    let updated = campaigns
        .update_one(
            filter,
            doc! {
                "$set": {
                    "safety": bson_document(safety, CollectionName::Campaigns)?,
                    "updated_at": now,
                }
            },
        )
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("replace campaign inspiration safety", error))?;
    if updated.matched_count != 1 {
        return Err(PersistenceError::RevisionConflict {
            entity: "private inspiration settings",
            id: campaign_id.to_owned(),
            expected: current_revision.unwrap_or(0),
            actual: current_revision.unwrap_or(0),
        });
    }
    Ok(())
}

async fn load_receipt<T: DeserializeOwned>(
    receipts: &Collection<Document>,
    session: &mut ClientSession,
    scope_kind: &str,
    scope_id: &str,
    idempotency_key: &str,
    command_kind: &str,
    request_fingerprint: &Sha256Digest,
) -> Result<ReceiptReplay<T>, PersistenceError> {
    let Some(receipt) = receipts
        .find_one(doc! {
            "scope_kind": scope_kind,
            "scope_id": scope_id,
            "idempotency_key": idempotency_key,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration command receipt", error))?
    else {
        return Ok(ReceiptReplay::Missing);
    };
    if required_string(&receipt, "command_kind", CollectionName::CommandReceipts)? != command_kind
        || required_string(
            &receipt,
            "request_fingerprint",
            CollectionName::CommandReceipts,
        )? != request_fingerprint.as_str()
    {
        return Ok(ReceiptReplay::Conflict);
    }
    decode_field(&receipt, "response", CollectionName::CommandReceipts).map(ReceiptReplay::Replay)
}

#[allow(clippy::too_many_arguments)]
async fn insert_receipt<T: Serialize>(
    receipts: &Collection<Document>,
    session: &mut ClientSession,
    scope_kind: &str,
    scope_id: &str,
    actor_account_id: &str,
    idempotency_key: &str,
    command_kind: &str,
    request_fingerprint: &Sha256Digest,
    response: &T,
    now: DateTime,
) -> Result<(), PersistenceError> {
    receipts
        .insert_one(doc! {
            "_id": format!("command-receipt:{}", Uuid::new_v4()),
            "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
            "scope_kind": scope_kind,
            "scope_id": scope_id,
            "campaign_id": if scope_kind == CAMPAIGN_COMMAND_SCOPE {
                Bson::String(scope_id.to_owned())
            } else {
                Bson::Null
            },
            "actor_account_id": actor_account_id,
            "command_kind": command_kind,
            "idempotency_key": idempotency_key,
            "request_fingerprint": request_fingerprint.as_str(),
            "state": "committed",
            "response": bson_value(response, CollectionName::CommandReceipts)?,
            "created_at": now,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("insert inspiration command receipt", error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_privacy_audit(
    audits: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: Option<&str>,
    action: &str,
    subject_kind: &str,
    subject_id: &str,
    secondary_id: Option<&str>,
    outcome: &str,
    now: DateTime,
    extra_metadata: Option<Document>,
) -> Result<(), PersistenceError> {
    let mut metadata = extra_metadata.unwrap_or_default();
    metadata.insert("secondary_id", optional_string_bson(secondary_id));
    metadata.insert("campaign_id", optional_string_bson(campaign_id));
    audits
        .insert_one(doc! {
            "_id": format!("privacy-audit:{}", Uuid::new_v4()),
            "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
            "category": "private_inspiration",
            "action": action,
            "outcome": outcome,
            "actor_account_id": SYSTEM_ACTOR_ID,
            "scope_kind": subject_kind,
            "scope_id": subject_id,
            "campaign_id": optional_string_bson(campaign_id),
            "metadata": metadata,
            "created_at": now,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("insert inspiration privacy audit", error))?;
    Ok(())
}

async fn load_global_control(
    collection: &Collection<Document>,
    session: Option<&mut ClientSession>,
) -> Result<Option<GlobalInspirationControlProjection>, PersistenceError> {
    let filter = doc! { "_id": GLOBAL_STATE_ID };
    let stored = match session {
        Some(session) => collection
            .find_one(filter)
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("load private inspiration global control", error)
            })?,
        None => collection.find_one(filter).await.map_err(|error| {
            PersistenceError::mongo("load private inspiration global control", error)
        })?,
    };
    let Some(stored) = stored else {
        return Ok(None);
    };
    let projection: GlobalInspirationControlProjection =
        decode_field(&stored, "response", CollectionName::CommandReceipts)?;
    if projection.schema_version != PRIVATE_INSPIRATION_SCHEMA_VERSION
        || stored.get_i64("revision").ok() != i64::try_from(projection.revision).ok()
    {
        return Err(private_schema(
            CollectionName::CommandReceipts,
            "global inspiration control projection is invalid",
        ));
    }
    Ok(Some(projection))
}

#[allow(clippy::too_many_arguments)]
async fn write_global_control(
    collection: &Collection<Document>,
    session: &mut ClientSession,
    current: &GlobalInspirationControlProjection,
    next: &GlobalInspirationControlProjection,
    command: &SetGlobalInspirationControlCommand,
    request_fingerprint: &Sha256Digest,
    now: DateTime,
) -> Result<(), PersistenceError> {
    let existing = collection
        .find_one(doc! { "_id": GLOBAL_STATE_ID })
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("lock private inspiration global control", error)
        })?;
    if existing.is_none() {
        collection
            .insert_one(doc! {
                "_id": GLOBAL_STATE_ID,
                "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                "scope_kind": GLOBAL_STATE_SCOPE,
                "scope_id": GLOBAL_SCOPE_ID,
                "actor_account_id": SYSTEM_ACTOR_ID,
                "command_kind": "private_inspiration_global_state",
                "idempotency_key": "private-inspiration-global-state",
                "request_fingerprint": request_fingerprint.as_str(),
                "state": "committed",
                "revision": inspiration_i64_persistence(
                    next.revision,
                    CollectionName::CommandReceipts,
                )?,
                "operator_id": command.operator_id.as_str(),
                "evidence_digest": command.evidence_digest.as_str(),
                "response": bson_value(next, CollectionName::CommandReceipts)?,
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("insert private inspiration global control", error)
            })?;
        return Ok(());
    }
    let updated = collection
        .update_one(
            doc! {
                "_id": GLOBAL_STATE_ID,
                "revision": inspiration_i64_persistence(
                    current.revision,
                    CollectionName::CommandReceipts,
                )?,
            },
            doc! {
                "$set": {
                    "request_fingerprint": request_fingerprint.as_str(),
                    "revision": inspiration_i64_persistence(
                        next.revision,
                        CollectionName::CommandReceipts,
                    )?,
                    "operator_id": command.operator_id.as_str(),
                    "evidence_digest": command.evidence_digest.as_str(),
                    "response": bson_value(next, CollectionName::CommandReceipts)?,
                    "updated_at": now,
                }
            },
        )
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("update private inspiration global control", error)
        })?;
    if updated.matched_count != 1 {
        return Err(PersistenceError::RevisionConflict {
            entity: "private inspiration global control",
            id: GLOBAL_SCOPE_ID.to_owned(),
            expected: current.revision,
            actual: current.revision,
        });
    }
    Ok(())
}

async fn serialize_global_operation(
    collection: &Collection<Document>,
    session: &mut ClientSession,
    now: DateTime,
) -> Result<(), PersistenceError> {
    let existing = collection
        .find_one(doc! { "_id": GLOBAL_STATE_ID })
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("lock private inspiration global operation", error)
        })?;
    if let Some(existing) = existing {
        let revision = existing.get_i64("revision").map_err(|_| {
            private_schema(
                CollectionName::CommandReceipts,
                "global inspiration state revision is missing",
            )
        })?;
        let updated = collection
            .update_one(
                doc! { "_id": GLOBAL_STATE_ID, "revision": revision },
                doc! { "$set": { "updated_at": now } },
            )
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("serialize private inspiration global operation", error)
            })?;
        if updated.matched_count != 1 {
            return Err(PersistenceError::RevisionConflict {
                entity: "private inspiration global control",
                id: GLOBAL_SCOPE_ID.to_owned(),
                expected: inspiration_u64_persistence(revision, CollectionName::CommandReceipts)?,
                actual: inspiration_u64_persistence(revision, CollectionName::CommandReceipts)?,
            });
        }
    } else {
        let default = GlobalInspirationControlProjection {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            revision: 1,
            generation_disabled: false,
            updated_at_epoch: 0,
        };
        collection
            .insert_one(doc! {
                "_id": GLOBAL_STATE_ID,
                "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                "scope_kind": GLOBAL_STATE_SCOPE,
                "scope_id": GLOBAL_SCOPE_ID,
                "actor_account_id": SYSTEM_ACTOR_ID,
                "command_kind": "private_inspiration_global_state",
                "idempotency_key": "private-inspiration-global-state",
                "request_fingerprint":
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "state": "committed",
                "revision": 1_i64,
                "response": bson_value(&default, CollectionName::CommandReceipts)?,
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("initialize private inspiration global control", error)
            })?;
    }
    Ok(())
}

async fn load_source(
    sources: &Collection<SourceDocument>,
    session: &mut ClientSession,
    source_id: &str,
    source_revision: u64,
) -> Result<Option<SourceDocument>, PersistenceError> {
    sources
        .find_one(doc! {
            "logical_id": source_id,
            "revision": inspiration_i64_persistence(
                source_revision,
                CollectionName::PrivateInspirationSources,
            )?,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration source version", error))
}

async fn participant_is_verified(
    participants: &Collection<Document>,
    session: &mut ClientSession,
    participant_id: &str,
) -> Result<bool, PersistenceError> {
    let Some(participant) = participants
        .find_one(doc! { "_id": participant_id, "participant_id": participant_id })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration participant", error))?
    else {
        return Ok(false);
    };
    let state = required_string(
        &participant,
        "state",
        CollectionName::PrivateInspirationParticipants,
    )?;
    let method = required_string(
        &participant,
        "verification_method",
        CollectionName::PrivateInspirationParticipants,
    )?;
    let projection: ParticipantVerificationProjection = decode_field(
        &participant,
        "projection",
        CollectionName::PrivateInspirationParticipants,
    )?;
    if !matches!(
        method.as_str(),
        "participant_signed_confirmation" | "timestamped_two_channel_acknowledgement"
    ) || projection.schema_version != PRIVATE_INSPIRATION_SCHEMA_VERSION
        || projection.participant_id.as_str() != participant_id
        || projection.method.as_str() != method
        || projection.revoked != (state == "revoked")
    {
        return Err(private_schema(
            CollectionName::PrivateInspirationParticipants,
            "participant verification projection is invalid",
        ));
    }
    Ok(state == "verified")
}

async fn all_source_participants_verified(
    participants: &Collection<Document>,
    session: &mut ClientSession,
    source: &StoredSource,
) -> Result<bool, PersistenceError> {
    if source.participants.is_empty() {
        return Ok(false);
    }
    for participant_id in &source.participants {
        if !participant_is_verified(participants, session, participant_id).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn source_is_vetoed(
    vetoes: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: &str,
    source: &StoredSource,
) -> Result<bool, PersistenceError> {
    vetoes
        .find_one(doc! {
            "campaign_id": campaign_id,
            "state": "active",
            "$or": [
                { "scope_kind": "campaign" },
                {
                    "scope_kind": "category",
                    "category_id": source.projection.category_id.as_str(),
                },
                {
                    "scope_kind": "source_version",
                    "source_id": source.projection.source_id.as_str(),
                    "source_revision": inspiration_i64_persistence(
                        source.projection.source_version,
                        CollectionName::PrivateInspirationVetoes,
                    )?,
                    "source_digest": source.projection.source_digest.as_str(),
                },
            ],
        })
        .session(&mut *session)
        .await
        .map(|value| value.is_some())
        .map_err(|error| PersistenceError::mongo("check inspiration vetoes", error))
}

async fn source_has_complete_consent(
    consents: &Collection<ConsentDocument>,
    session: &mut ClientSession,
    command: &RequestInspirationSelectionCommand,
    source: &StoredSource,
    now: u64,
) -> Result<bool, PersistenceError> {
    let mut cursor = consents
        .find(doc! {
            "campaign_id": command.campaign_session_id.as_str(),
            "source_id": source.projection.source_id.as_str(),
            "source_revision": inspiration_i64_persistence(
                source.projection.source_version,
                CollectionName::PrivateInspirationConsents,
            )?,
            "source_digest": source.projection.source_digest.as_str(),
            "audience": command.audience.as_str(),
            "media": command.media.as_str(),
            "transformation": InspirationTransformation::HighFictionDistanceV1.as_str(),
            "state": "active",
            "expires_at_epoch": {
                "$gt": inspiration_i64_persistence(
                    now,
                    CollectionName::PrivateInspirationConsents,
                )?
            },
        })
        .sort(doc! { "participant_id": 1_i64 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load complete inspiration consent", error))?;
    let mut granted_participants = BTreeSet::new();
    let mut count = 0_usize;
    while cursor
        .advance(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("read complete inspiration consent", error))?
    {
        let consent: ConsentDocument = cursor.deserialize_current().map_err(|_| {
            private_schema(
                CollectionName::PrivateInspirationConsents,
                "selection consent document could not be decoded",
            )
        })?;
        consent.checked_projection().map_err(domain_as_schema)?;
        if consent
            .sensitivities
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != source.sensitivities
        {
            return Ok(false);
        }
        granted_participants.insert(consent.participant_id);
        count += 1;
    }
    Ok(count == source.participants.len() && granted_participants == source.participants)
}

fn normalized_set(values: &[String]) -> BTreeSet<String> {
    values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect()
}

fn normalized_stored_set(values: &BTreeSet<String>) -> BTreeSet<String> {
    values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect()
}

fn empty_selection_audit(
    seed: [u8; 32],
    cursor: u64,
) -> Result<EventSelectionAudit, PrivateInspirationError> {
    let allowed = BTreeSet::new();
    let participants = BTreeSet::new();
    let history = HashMap::new();
    let context = EventEligibility {
        inspiration_enabled: false,
        party_level: 1,
        current_turn: 0,
        allowed_sensitivity_tags: &allowed,
        consenting_participant_aliases: &participants,
        last_triggered_turn: &history,
    };
    let mut random = DeterministicEventRandom::new(seed, cursor);
    EventPromptLoader
        .select_with_audit(&[], &context, &mut random)
        .map(|selected| selected.audit)
        .map_err(PrivateInspirationError::Selection)
}

async fn load_selection_replay(
    selections: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: &str,
    idempotency_key: &str,
    request_fingerprint: &Sha256Digest,
) -> Result<ReceiptReplay<PrivateInspirationSelection>, PersistenceError> {
    let Some(document) = selections
        .find_one(doc! {
            "campaign_id": campaign_id,
            "idempotency_key": idempotency_key,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration selection replay", error))?
    else {
        return Ok(ReceiptReplay::Missing);
    };
    if required_string(
        &document,
        "request_fingerprint",
        CollectionName::PrivateInspirationSelections,
    )? != request_fingerprint.as_str()
    {
        return Ok(ReceiptReplay::Conflict);
    }
    let selection: PrivateInspirationSelection = decode_field(
        &document,
        "projection",
        CollectionName::PrivateInspirationSelections,
    )?;
    if selection.schema_version != PRIVATE_INSPIRATION_SCHEMA_VERSION
        || selection.campaign_session_id.as_str() != campaign_id
        || selection.audit.algorithm != RollAlgorithm::ChaCha20V1
    {
        return Err(private_schema(
            CollectionName::PrivateInspirationSelections,
            "stored selection replay is invalid",
        ));
    }
    Ok(ReceiptReplay::Replay(selection))
}

async fn load_last_triggered_turns(
    selections: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<HashMap<String, u64>, PersistenceError> {
    let mut cursor = selections
        .find(doc! {
            "campaign_id": campaign_id,
            "source_id": { "$type": "string" },
        })
        .sort(doc! { "turn_number": 1_i64, "_id": 1_i64 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration cooldown history", error))?;
    let mut turns = HashMap::new();
    while cursor
        .advance(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("read inspiration cooldown history", error))?
    {
        let document = cursor.deserialize_current().map_err(|_| {
            private_schema(
                CollectionName::PrivateInspirationSelections,
                "cooldown selection document could not be decoded",
            )
        })?;
        let source_id = required_string(
            &document,
            "source_id",
            CollectionName::PrivateInspirationSelections,
        )?;
        let turn = inspiration_u64_persistence(
            document.get_i64("turn_number").map_err(|_| {
                private_schema(
                    CollectionName::PrivateInspirationSelections,
                    "selection turn number is missing",
                )
            })?,
            CollectionName::PrivateInspirationSelections,
        )?;
        turns.insert(source_id, turn);
    }
    Ok(turns)
}

async fn trusted_trigger_window(
    turn_events: &Collection<Document>,
    session: &mut ClientSession,
    campaign: &InspirationCampaignDocument,
) -> Result<Option<(u64, OpaqueInspirationId)>, PersistenceError> {
    if campaign.session.status != SessionStatus::Active || campaign.session.last_event_sequence == 0
    {
        return Ok(None);
    }
    let turn_number = campaign.session.last_event_sequence;
    let event_document = turn_events
        .find_one(doc! {
            "campaign_id": &campaign.id,
            "sequence": inspiration_i64_persistence(
                turn_number,
                CollectionName::TurnEvents,
            )?,
        })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration trigger event", error))?
        .ok_or_else(|| {
            private_schema(
                CollectionName::TurnEvents,
                "inspiration trigger event is missing",
            )
        })?;
    let event: SessionEventDto =
        decode_field(&event_document, "event", CollectionName::TurnEvents)?;
    event.validate().map_err(|_| {
        private_schema(
            CollectionName::TurnEvents,
            "inspiration trigger event is invalid",
        )
    })?;
    if event.session_id != campaign.id || event.sequence != turn_number {
        return Err(private_schema(
            CollectionName::TurnEvents,
            "inspiration trigger event identity is invalid",
        ));
    }
    let safe = match event.payload {
        SessionEventPayload::AbilityCheckResolved { .. }
        | SessionEventPayload::ExplorationSocialResolved { .. }
        | SessionEventPayload::GmNarration { .. }
        | SessionEventPayload::ExperienceAwarded { .. }
        | SessionEventPayload::AiProposalAccepted { .. }
        | SessionEventPayload::AiProposalRejected { .. } => true,
        SessionEventPayload::EncounterResolved { outcome, .. } => {
            matches!(outcome.resolution.state.status, EncounterStatus::Victory)
                || (matches!(outcome.resolution.state.status, EncounterStatus::Defeat)
                    && outcome
                        .resolution
                        .state
                        .transition
                        .as_ref()
                        .is_some_and(|transition| transition.story_recovery_applied))
        }
        SessionEventPayload::SessionStarted
        | SessionEventPayload::PlayerIntent { .. }
        | SessionEventPayload::DiceResolved { .. }
        | SessionEventPayload::SessionEnded => false,
    };
    if !safe {
        return Ok(None);
    }
    Ok(Some((
        turn_number,
        OpaqueInspirationId::new(format!("trigger-window:{turn_number}"))
            .map_err(domain_as_schema)?,
    )))
}

async fn trusted_party_level(
    character_instances: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: &str,
) -> Result<u8, PersistenceError> {
    let mut cursor = character_instances
        .find(doc! { "campaign_id": campaign_id, "state": "active" })
        .sort(doc! { "_id": 1_i64 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load inspiration party", error))?;
    let mut party_level = None;
    while cursor
        .advance(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("read inspiration party", error))?
    {
        let document = cursor.deserialize_current().map_err(|_| {
            private_schema(
                CollectionName::CampaignCharacterInstances,
                "party character document could not be decoded",
            )
        })?;
        let runtime = document.get_document("runtime").map_err(|_| {
            private_schema(
                CollectionName::CampaignCharacterInstances,
                "party character runtime is missing",
            )
        })?;
        let level = if let Some(value) = runtime.get("hero_character") {
            let hero: HeroCharacter = bson::from_bson(value.clone()).map_err(|_| {
                private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "hero runtime could not be decoded",
                )
            })?;
            hero.validate().map_err(|_| {
                private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "hero runtime failed validation",
                )
            })?;
            if hero.campaign_id != campaign_id {
                return Err(private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "hero runtime belongs to another campaign",
                ));
            }
            hero.level.value()
        } else if let Some(value) = runtime.get("hero") {
            let hero: HeroCharacter = bson::from_bson(value.clone()).map_err(|_| {
                private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "campaign hero runtime could not be decoded",
                )
            })?;
            hero.validate().map_err(|_| {
                private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "campaign hero runtime failed validation",
                )
            })?;
            if hero.campaign_id != campaign_id {
                return Err(private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "campaign hero runtime belongs to another campaign",
                ));
            }
            hero.level.value()
        } else if let Some(value) = runtime.get("character_snapshot") {
            let character: Character = bson::from_bson(value.clone()).map_err(|_| {
                private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "core character runtime could not be decoded",
                )
            })?;
            character.validate().map_err(|_| {
                private_schema(
                    CollectionName::CampaignCharacterInstances,
                    "core character runtime failed validation",
                )
            })?;
            character.level().value()
        } else {
            return Err(private_schema(
                CollectionName::CampaignCharacterInstances,
                "party character runtime kind is unsupported",
            ));
        };
        party_level = Some(party_level.map_or(level, |current: u8| current.max(level)));
    }
    party_level.ok_or_else(|| {
        private_schema(
            CollectionName::CampaignCharacterInstances,
            "campaign party is missing",
        )
    })
}

async fn load_work(
    work: &Collection<WorkDocument>,
    session: &mut ClientSession,
    filter: Document,
) -> Result<Vec<WorkDocument>, PersistenceError> {
    let mut cursor = work
        .find(filter)
        .sort(doc! { "campaign_id": 1_i64, "_id": 1_i64 })
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("load private inspiration work", error))?;
    let mut output = Vec::new();
    while cursor
        .advance(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("read private inspiration work", error))?
    {
        let item: WorkDocument = cursor.deserialize_current().map_err(|_| {
            private_schema(
                CollectionName::PrivateInspirationWork,
                "private inspiration work document could not be decoded",
            )
        })?;
        item.validate().map_err(domain_as_schema)?;
        output.push(item);
    }
    Ok(output)
}

async fn cancel_pending_work(
    work: &Collection<WorkDocument>,
    audits: &Collection<Document>,
    session: &mut ClientSession,
    mut filter: Document,
    now_epoch: u64,
    now: DateTime,
) -> Result<Vec<OpaqueInspirationId>, PersistenceError> {
    filter.insert("state", "pending");
    let pending = load_work(work, session, filter).await?;
    let mut ids = Vec::with_capacity(pending.len());
    for item in pending {
        let cancellation_requested_at_epoch = item.created_at_epoch.max(
            inspiration_i64_persistence(now_epoch, CollectionName::PrivateInspirationWork)?,
        );
        let updated = work
            .update_one(
                doc! {
                    "_id": &item.id,
                    "campaign_id": &item.campaign_id,
                    "state": "pending",
                },
                doc! {
                    "$set": {
                        "state": "cancellation_requested",
                        "cancellation_requested_at_epoch": cancellation_requested_at_epoch,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *session)
            .await
            .map_err(|error| PersistenceError::mongo("cancel private inspiration work", error))?;
        if updated.matched_count != 1 {
            return Err(private_schema(
                CollectionName::PrivateInspirationWork,
                "pending work changed during cancellation",
            ));
        }
        insert_privacy_audit(
            audits,
            session,
            Some(&item.campaign_id),
            "derived_work_cancel_requested",
            "derived_work",
            &item.id,
            None,
            "cancel_requested",
            now,
            None,
        )
        .await?;
        ids.push(OpaqueInspirationId::new(item.id).map_err(domain_as_schema)?);
    }
    ids.sort();
    Ok(ids)
}

async fn apply_completed_work_policy(
    work: &Collection<WorkDocument>,
    presentations: &Collection<Document>,
    audits: &Collection<Document>,
    session: &mut ClientSession,
    completed: Vec<WorkDocument>,
    now: DateTime,
) -> Result<(), PersistenceError> {
    for item in completed {
        if !matches!(item.state.as_str(), "completed" | "redacted") {
            return Err(private_schema(
                CollectionName::PrivateInspirationWork,
                "completed work policy received non-completed work",
            ));
        }
        let policy =
            DerivedArtifactPolicy::parse(&item.artifact_policy).map_err(domain_as_schema)?;
        let artifact_id = item.completed_artifact_id.as_deref().ok_or_else(|| {
            private_schema(
                CollectionName::PrivateInspirationWork,
                "completed work artifact is missing",
            )
        })?;
        if !manchester_dnd_core::is_valid_opaque_id(artifact_id) {
            return Err(private_schema(
                CollectionName::PrivateInspirationWork,
                "completed work artifact id is invalid",
            ));
        }
        let (state, retained_artifact, action) = match policy {
            DerivedArtifactPolicy::RedactDerived => {
                let redacted = presentations
                    .update_one(
                        doc! {
                            "_id": artifact_id,
                            "campaign_id": &item.campaign_id,
                            "private_inspiration_work_id": &item.id,
                        },
                        doc! {
                            "$set": {
                                "body": PRIVATE_INSPIRATION_REDACTION_BODY,
                                "privacy_state": "redacted",
                                "updated_at": now,
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("redact completed inspiration presentation", error)
                    })?;
                if redacted.matched_count == 1 {
                    (
                        "redacted",
                        Some(artifact_id.to_owned()),
                        "derived_work_redacted",
                    )
                } else {
                    ("deleted", None, "derived_work_deleted")
                }
            }
            DerivedArtifactPolicy::DeleteDerived | DerivedArtifactPolicy::RetainMinimalAudit => {
                presentations
                    .delete_one(doc! {
                        "_id": artifact_id,
                        "campaign_id": &item.campaign_id,
                        "private_inspiration_work_id": &item.id,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("delete completed inspiration presentation", error)
                    })?;
                ("deleted", None, "derived_work_deleted")
            }
        };
        let updated = work
            .update_one(
                doc! {
                    "_id": &item.id,
                    "state": { "$in": ["completed", "redacted"] },
                },
                doc! {
                    "$set": {
                        "state": state,
                        "completed_artifact_id": retained_artifact
                            .map_or(Bson::Null, Bson::String),
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("apply completed inspiration work policy", error)
            })?;
        if updated.matched_count != 1 {
            return Err(private_schema(
                CollectionName::PrivateInspirationWork,
                "completed work changed during privacy transition",
            ));
        }
        insert_privacy_audit(
            audits,
            session,
            Some(&item.campaign_id),
            action,
            "derived_work",
            &item.id,
            Some(artifact_id),
            "applied",
            now,
            None,
        )
        .await?;
    }
    Ok(())
}

async fn quarantine_all_private_inspiration_work(
    work: &Collection<WorkDocument>,
    presentations: &Collection<Document>,
    audits: &Collection<Document>,
    session: &mut ClientSession,
    now_epoch: u64,
    now: DateTime,
) -> Result<(), PersistenceError> {
    cancel_pending_work(work, audits, session, Document::new(), now_epoch, now).await?;
    let completed = load_work(
        work,
        session,
        doc! { "state": { "$in": ["completed", "redacted"] } },
    )
    .await?;
    for item in completed {
        let artifact_id = item.completed_artifact_id.as_deref().ok_or_else(|| {
            private_schema(
                CollectionName::PrivateInspirationWork,
                "completed work artifact is missing",
            )
        })?;
        let redacted = presentations
            .update_one(
                doc! {
                    "_id": artifact_id,
                    "campaign_id": &item.campaign_id,
                    "private_inspiration_work_id": &item.id,
                },
                doc! {
                    "$set": {
                        "body": PRIVATE_INSPIRATION_REDACTION_BODY,
                        "privacy_state": "redacted",
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *session)
            .await
            .map_err(|error| {
                PersistenceError::mongo("quarantine inspiration presentation", error)
            })?;
        if redacted.matched_count == 0 {
            continue;
        }
        let updated = work
            .update_one(
                doc! {
                    "_id": &item.id,
                    "state": { "$in": ["completed", "redacted"] },
                },
                doc! {
                    "$set": {
                        "state": "redacted",
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *session)
            .await
            .map_err(|error| PersistenceError::mongo("quarantine inspiration work", error))?;
        if updated.matched_count != 1 {
            return Err(private_schema(
                CollectionName::PrivateInspirationWork,
                "completed work changed during global quarantine",
            ));
        }
        insert_privacy_audit(
            audits,
            session,
            Some(&item.campaign_id),
            "derived_work_redacted",
            "derived_work",
            &item.id,
            Some(artifact_id),
            "applied",
            now,
            None,
        )
        .await?;
    }
    Ok(())
}

async fn revoke_campaign_consents(
    consents: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: &str,
    now_epoch: u64,
    now: DateTime,
) -> Result<(), PersistenceError> {
    consents
        .update_many(
            doc! { "campaign_id": campaign_id, "state": "active" },
            doc! {
                "$set": {
                    "state": "revoked",
                    "projection.state": "revoked",
                    "revoked_at_epoch": inspiration_i64_persistence(
                        now_epoch,
                        CollectionName::PrivateInspirationConsents,
                    )?,
                    "revocation_code": "campaign_disabled",
                    "updated_at": now,
                }
            },
        )
        .session(&mut *session)
        .await
        .map_err(|error| PersistenceError::mongo("revoke campaign inspiration consents", error))?;
    Ok(())
}

fn source_key_filter(sources: &[SourceDocument]) -> Vec<Bson> {
    sources
        .iter()
        .map(|source| {
            Bson::Document(doc! {
                "source_id": &source.logical_id,
                "source_revision": source.revision,
            })
        })
        .collect()
}

struct VetoScopeFields {
    scope_kind: &'static str,
    category_id: Bson,
    source_id: Bson,
    source_revision: Bson,
    source_digest: Bson,
}

fn veto_scope_fields(
    scope: &InspirationVetoScope,
) -> Result<VetoScopeFields, PrivateInspirationError> {
    Ok(match scope {
        InspirationVetoScope::Campaign => VetoScopeFields {
            scope_kind: "campaign",
            category_id: Bson::Null,
            source_id: Bson::Null,
            source_revision: Bson::Null,
            source_digest: Bson::Null,
        },
        InspirationVetoScope::Category { category_id } => VetoScopeFields {
            scope_kind: "category",
            category_id: Bson::String(category_id.to_string()),
            source_id: Bson::Null,
            source_revision: Bson::Null,
            source_digest: Bson::Null,
        },
        InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            source_digest,
        } => VetoScopeFields {
            scope_kind: "source_version",
            category_id: Bson::Null,
            source_id: Bson::String(source_id.to_string()),
            source_revision: Bson::Int64(inspiration_i64(
                *source_version,
                "veto_source_revision_range",
            )?),
            source_digest: Bson::String(source_digest.as_str().to_owned()),
        },
    })
}

const fn veto_scope_name(scope: &InspirationVetoScope) -> &'static str {
    match scope {
        InspirationVetoScope::Campaign => "campaign",
        InspirationVetoScope::Category { .. } => "category",
        InspirationVetoScope::SourceVersion { .. } => "source_version",
    }
}

async fn work_filter_for_scope(
    sources: &Collection<SourceDocument>,
    session: &mut ClientSession,
    campaign_id: &str,
    scope: &InspirationVetoScope,
) -> Result<Document, PersistenceError> {
    match scope {
        InspirationVetoScope::Campaign => Ok(doc! { "campaign_id": campaign_id }),
        InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            ..
        } => Ok(doc! {
            "campaign_id": campaign_id,
            "source_id": source_id.as_str(),
            "source_revision": inspiration_i64_persistence(
                *source_version,
                CollectionName::PrivateInspirationWork,
            )?,
        }),
        InspirationVetoScope::Category { category_id } => {
            let mut cursor = sources
                .find(doc! { "category_id": category_id.as_str() })
                .sort(doc! { "logical_id": 1_i64, "revision": 1_i64 })
                .session(&mut *session)
                .await
                .map_err(|error| {
                    PersistenceError::mongo("load category inspiration sources", error)
                })?;
            let mut keys = Vec::new();
            while cursor.advance(&mut *session).await.map_err(|error| {
                PersistenceError::mongo("read category inspiration sources", error)
            })? {
                let source: SourceDocument = cursor.deserialize_current().map_err(|_| {
                    private_schema(
                        CollectionName::PrivateInspirationSources,
                        "category source document could not be decoded",
                    )
                })?;
                keys.push(Bson::Document(doc! {
                    "source_id": source.logical_id,
                    "source_revision": source.revision,
                }));
            }
            if keys.is_empty() {
                Ok(doc! {
                    "campaign_id": campaign_id,
                    "_id": { "$exists": false },
                })
            } else {
                Ok(doc! { "campaign_id": campaign_id, "$or": keys })
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_owner_veto(
    vetoes: &Collection<Document>,
    audits: &Collection<Document>,
    session: &mut ClientSession,
    campaign_id: &str,
    scope: &InspirationVetoScope,
    presentation_id: &str,
    now_epoch: u64,
    now: DateTime,
) -> Result<(), PersistenceError> {
    let veto_id = internal_id("inspiration-veto").map_err(domain_as_schema)?;
    let fields = veto_scope_fields(scope).map_err(domain_as_schema)?;
    vetoes
        .insert_one(doc! {
            "_id": veto_id.as_str(),
            "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
            "campaign_id": campaign_id,
            "actor_participant_id": "participant:campaign-owner",
            "actor_kind": "campaign_owner",
            "scope_kind": fields.scope_kind,
            "category_id": fields.category_id,
            "source_id": fields.source_id,
            "source_revision": fields.source_revision,
            "source_digest": fields.source_digest,
            "state": "active",
            "veto_code": "safety_veto",
            "scope": bson_value(scope, CollectionName::PrivateInspirationVetoes)?,
            "created_at_epoch": inspiration_i64_persistence(
                now_epoch,
                CollectionName::PrivateInspirationVetoes,
            )?,
            "created_at": now,
            "updated_at": now,
        })
        .session(&mut *session)
        .await
        .map_err(|error| {
            PersistenceError::mongo("insert campaign-owner inspiration veto", error)
        })?;
    insert_privacy_audit(
        audits,
        session,
        Some(campaign_id),
        "owner_veto_applied",
        "veto",
        veto_id.as_str(),
        Some(presentation_id),
        "applied",
        now,
        Some(doc! { "scope_kind": veto_scope_name(scope) }),
    )
    .await
}

impl MongoRepository {
    pub(crate) async fn load_private_inspiration_redacted_export(
        &self,
        campaign_session_id: &OpaqueInspirationId,
        requesting_participant_id: &OpaqueInspirationId,
    ) -> Result<CampaignInspirationRedactedExportV1, PrivateInspirationError> {
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let consents = self
            .store()
            .collection::<ConsentDocument>(CollectionName::PrivateInspirationConsents);
        let campaign_id = campaign_session_id.to_string();
        let participant_id = requesting_participant_id.to_string();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let participants = participants.clone();
            let sources = sources.clone();
            let consents = consents.clone();
            let campaign_id = campaign_id.clone();
            let participant_id = participant_id.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(&campaigns, session, &campaign_id).await?;
                if !participant_is_verified(&participants, session, &participant_id).await? {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let Some(safety) = campaign.safety.as_ref() else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let settings = safety.projection(&campaign_id).map_err(domain_as_schema)?;
                let mut grant_cursor = consents
                    .find(doc! {
                        "campaign_id": &campaign_id,
                        "participant_id": &participant_id,
                    })
                    .sort(doc! {
                        "source_id": 1_i64,
                        "source_revision": 1_i64,
                        "_id": 1_i64,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load inspiration export consents", error)
                    })?;
                let mut requester_grants = Vec::new();
                let mut source_keys = BTreeSet::new();
                while grant_cursor.advance(&mut *session).await.map_err(|error| {
                    PersistenceError::mongo("read inspiration export consents", error)
                })? {
                    let consent: ConsentDocument =
                        grant_cursor.deserialize_current().map_err(|_| {
                            private_schema(
                                CollectionName::PrivateInspirationConsents,
                                "export consent document could not be decoded",
                            )
                        })?;
                    let projection = consent.checked_projection().map_err(domain_as_schema)?;
                    source_keys
                        .insert((projection.source_id.to_string(), projection.source_version));
                    requester_grants.push(projection);
                }
                if requester_grants.is_empty() {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let mut source_projections = Vec::with_capacity(source_keys.len());
                for (source_id, source_revision) in source_keys {
                    let Some(source_document) =
                        load_source(&sources, session, &source_id, source_revision).await?
                    else {
                        return Err(private_schema(
                            CollectionName::PrivateInspirationSources,
                            "export source is missing",
                        ));
                    };
                    let source = source_document.stored().map_err(domain_as_schema)?;
                    if !source.participants.contains(&participant_id) {
                        return Err(private_schema(
                            CollectionName::PrivateInspirationSources,
                            "export source participant scope is invalid",
                        ));
                    }
                    source_projections.push(source.projection);
                }
                Ok(Ok(CampaignInspirationRedactedExportV1 {
                    schema_version: PRIVATE_INSPIRATION_EXPORT_SCHEMA_VERSION,
                    campaign_session_id: OpaqueInspirationId::new(campaign_id)
                        .map_err(domain_as_schema)?,
                    requesting_participant_id: OpaqueInspirationId::new(participant_id)
                        .map_err(domain_as_schema)?,
                    settings,
                    sources: source_projections,
                    requester_grants,
                }))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn reserve_private_inspiration_selection(
        &self,
        deployment_enabled: bool,
        command: &RequestInspirationSelectionCommand,
        authority: &ResolvedInspirationSelectionAuthority,
        prompts: &[EventPrompt],
        now: u64,
    ) -> Result<PrivateInspirationSelection, PrivateInspirationError> {
        let now_date = epoch_date(now, "selection_time_range")?;
        let request_fingerprint = fingerprint(command)?;
        let selection_id = internal_id("inspiration-selection")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let global_state = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let consents = self
            .store()
            .collection::<ConsentDocument>(CollectionName::PrivateInspirationConsents);
        let vetoes = self
            .store()
            .document_collection(CollectionName::PrivateInspirationVetoes);
        let selections = self
            .store()
            .document_collection(CollectionName::PrivateInspirationSelections);
        let character_instances = self
            .store()
            .document_collection(CollectionName::CampaignCharacterInstances);
        let turn_events = self.store().document_collection(CollectionName::TurnEvents);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let command = command.clone();
        let authority_seed_reference = authority.seed_reference.clone();
        let authority_seed = authority.seed;
        let prompts = prompts.to_vec();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let global_state = global_state.clone();
            let sources = sources.clone();
            let participants = participants.clone();
            let consents = consents.clone();
            let vetoes = vetoes.clone();
            let selections = selections.clone();
            let character_instances = character_instances.clone();
            let turn_events = turn_events.clone();
            let audits = audits.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            let selection_id = selection_id.clone();
            let authority_seed_reference = authority_seed_reference.clone();
            let prompts = prompts.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_selection_replay(
                    &selections,
                    session,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(selection) => return Ok(Ok(selection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let campaign_revision =
                    inspiration_u64_persistence(campaign.revision, CollectionName::Campaigns)?;
                if campaign_revision != command.expected_campaign_revision {
                    return Ok(Err(PrivateInspirationError::RevisionConflict {
                        expected: command.expected_campaign_revision,
                        current: campaign_revision,
                    }));
                }
                let Some(safety) = campaign.safety.as_ref() else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let settings_revision =
                    inspiration_u64_persistence(safety.revision, CollectionName::Campaigns)?;
                if settings_revision != command.expected_settings_revision {
                    return Ok(Err(PrivateInspirationError::RevisionConflict {
                        expected: command.expected_settings_revision,
                        current: settings_revision,
                    }));
                }
                let global = load_global_control(&global_state, Some(session))
                    .await?
                    .unwrap_or_else(default_global_control);
                let Some((turn_number, trigger_window_id)) =
                    trusted_trigger_window(&turn_events, session, &campaign).await?
                else {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                };
                let cursor =
                    inspiration_u64_persistence(safety.rng_cursor, CollectionName::Campaigns)?;
                let early_reason = if !deployment_enabled {
                    Some(DurableNoSelectionReason::DeploymentDisabled)
                } else if global.generation_disabled {
                    Some(DurableNoSelectionReason::GlobalKillSwitch)
                } else if !safety.enabled {
                    Some(DurableNoSelectionReason::CampaignDisabled)
                } else if safety.generation_paused {
                    Some(DurableNoSelectionReason::CampaignPaused)
                } else if !safety.safety_setup_complete {
                    Some(DurableNoSelectionReason::SafetyIncomplete)
                } else {
                    None
                };

                let mut selected_source_version = None;
                let (audit, durable_no_selection_reason, selected_cooldown) = if let Some(reason) =
                    early_reason
                {
                    (
                        empty_selection_audit(authority_seed, cursor).map_err(domain_as_schema)?,
                        Some(reason),
                        None,
                    )
                } else {
                    let party_level = trusted_party_level(
                        &character_instances,
                        session,
                        command.campaign_session_id.as_str(),
                    )
                    .await?;
                    if campaign.theme_id.is_empty() {
                        return Err(private_schema(
                            CollectionName::Campaigns,
                            "campaign theme is missing",
                        ));
                    }
                    let allowed_sensitivities = safety
                        .allowed_sensitivities
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    let excluded_safety_codes = safety
                        .lines
                        .iter()
                        .chain(&safety.veils)
                        .chain(&safety.excluded_topics)
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    let excluded_participants = safety
                        .excluded_participant_ids
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    let mut source_cursor = sources
                        .find(doc! {
                            "review_state": SourceReviewState::Approved.as_str(),
                            "q11_screened": true,
                            "media": command.media.as_str(),
                            "$or": [
                                { "expires_at_epoch": { "$exists": false } },
                                { "expires_at_epoch": Bson::Null },
                                {
                                    "expires_at_epoch": {
                                        "$gt": inspiration_i64_persistence(
                                            now,
                                            CollectionName::PrivateInspirationSources,
                                        )?
                                    }
                                },
                            ],
                        })
                        .sort(doc! { "logical_id": 1_i64, "revision": 1_i64 })
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("load eligible inspiration sources", error)
                        })?;
                    let mut authenticated_prompts = Vec::new();
                    let mut source_versions = BTreeMap::<(String, String), u64>::new();
                    let mut consenting_participants = BTreeSet::new();
                    while source_cursor
                        .advance(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("read eligible inspiration sources", error)
                        })?
                    {
                        let source_document =
                            source_cursor.deserialize_current().map_err(|_| {
                                private_schema(
                                    CollectionName::PrivateInspirationSources,
                                    "eligible source document could not be decoded",
                                )
                            })?;
                        let source = source_document.stored().map_err(domain_as_schema)?;
                        if !source.sensitivities.is_subset(&allowed_sensitivities)
                            || !source.sensitivities.is_disjoint(&excluded_safety_codes)
                            || !source.participants.is_disjoint(&excluded_participants)
                            || !source.theme_pack_ids.contains(&campaign.theme_id)
                            || source_is_vetoed(
                                &vetoes,
                                session,
                                command.campaign_session_id.as_str(),
                                &source,
                            )
                            .await?
                            || !all_source_participants_verified(&participants, session, &source)
                                .await?
                            || !source_has_complete_consent(
                                &consents, session, &command, &source, now,
                            )
                            .await?
                        {
                            continue;
                        }
                        let Some(prompt) = prompts.iter().find(|prompt| {
                            prompt.privacy_source_id() == source.projection.source_id.as_str()
                                && prompt.source_digest() == &source.projection.source_digest
                                && prompt.metadata.enabled
                                && normalized_set(&prompt.metadata.sensitivity_tags)
                                    == normalized_stored_set(&source.sensitivities)
                                && normalized_set(&prompt.metadata.participant_aliases)
                                    == normalized_stored_set(&source.participants)
                        }) else {
                            continue;
                        };
                        source_versions.insert(
                            (
                                source.projection.source_id.to_string(),
                                source.projection.source_digest.as_str().to_owned(),
                            ),
                            source.projection.source_version,
                        );
                        consenting_participants.extend(normalized_stored_set(&source.participants));
                        authenticated_prompts.push(prompt.clone());
                    }
                    let last_triggered_turn = load_last_triggered_turns(
                        &selections,
                        session,
                        command.campaign_session_id.as_str(),
                    )
                    .await?;
                    let normalized_allowed = normalized_stored_set(&allowed_sensitivities);
                    let context = EventEligibility {
                        inspiration_enabled: true,
                        party_level,
                        current_turn: turn_number,
                        allowed_sensitivity_tags: &normalized_allowed,
                        consenting_participant_aliases: &consenting_participants,
                        last_triggered_turn: &last_triggered_turn,
                    };
                    let mut random = DeterministicEventRandom::new(authority_seed, cursor);
                    let selected = EventPromptLoader
                        .select_with_audit(&authenticated_prompts, &context, &mut random)
                        .map_err(|_| {
                            private_schema(
                                CollectionName::PrivateInspirationSelections,
                                "deterministic inspiration selection failed",
                            )
                        })?;
                    let selected_cooldown =
                        selected.prompt.map(|prompt| prompt.metadata.cooldown_turns);
                    if let (Some(source_id), Some(source_digest)) = (
                        selected.audit.selected_source_id.as_ref(),
                        selected.audit.selected_source_digest.as_ref(),
                    ) {
                        selected_source_version = source_versions
                            .get(&(source_id.clone(), source_digest.as_str().to_owned()))
                            .copied();
                        if selected_source_version.is_none() {
                            return Err(private_schema(
                                CollectionName::PrivateInspirationSelections,
                                "selected source version is missing",
                            ));
                        }
                    }
                    let reason = selected
                        .prompt
                        .is_none()
                        .then_some(DurableNoSelectionReason::NoEligibleSources);
                    (selected.audit, reason, selected_cooldown)
                };

                let next_eligible_turn = match (
                    audit.selected_source_id.as_deref(),
                    selected_source_version,
                    audit.selected_source_digest.as_ref(),
                    selected_cooldown,
                ) {
                    (Some(_), Some(_), Some(_), Some(cooldown)) => {
                        let next = turn_number.checked_add(cooldown).ok_or_else(|| {
                            private_schema(
                                CollectionName::PrivateInspirationSelections,
                                "source cooldown turn overflow",
                            )
                        })?;
                        if next <= turn_number {
                            return Err(private_schema(
                                CollectionName::PrivateInspirationSelections,
                                "selected source cooldown is invalid",
                            ));
                        }
                        Some(next)
                    }
                    (None, None, None, None) => None,
                    _ => {
                        return Err(private_schema(
                            CollectionName::PrivateInspirationSelections,
                            "selected source binding is incomplete",
                        ));
                    }
                };
                let selection = PrivateInspirationSelection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    selection_id: selection_id.clone(),
                    campaign_session_id: command.campaign_session_id.clone(),
                    source_version: selected_source_version,
                    durable_no_selection_reason,
                    audit: audit.clone(),
                    created_at_epoch: now,
                };
                selections
                    .insert_one(doc! {
                        "_id": selection_id.as_str(),
                        "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                        "campaign_id": command.campaign_session_id.as_str(),
                        "idempotency_key": command.idempotency_key.as_str(),
                        "request_fingerprint": request_fingerprint.as_str(),
                        "trigger_window_id": trigger_window_id.as_str(),
                        "campaign_revision": inspiration_i64_persistence(
                            command.expected_campaign_revision,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "settings_revision": inspiration_i64_persistence(
                            command.expected_settings_revision,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "turn_number": inspiration_i64_persistence(
                            turn_number,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "audience": command.audience.as_str(),
                        "media": command.media.as_str(),
                        "seed_reference": authority_seed_reference.as_str(),
                        "eligible_set_digest": audit.eligible_set_digest.as_str(),
                        "eligible_source_count": i64::from(audit.eligible_source_count),
                        "source_id": optional_string_bson(
                            audit.selected_source_id.as_deref(),
                        ),
                        "source_revision": optional_u64_bson(
                            selected_source_version,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "source_digest": optional_string_bson(
                            audit
                                .selected_source_digest
                                .as_ref()
                                .map(Sha256Digest::as_str),
                        ),
                        "next_eligible_turn": optional_u64_bson(
                            next_eligible_turn,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "no_selection_reason": optional_string_bson(
                            durable_no_selection_reason
                                .map(DurableNoSelectionReason::as_str),
                        ),
                        "sample_numerator": optional_u64_bson(
                            audit.sample_numerator,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "sample_denominator": optional_u64_bson(
                            audit.sample_denominator,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "algorithm": audit.algorithm.as_str(),
                        "cursor_before": inspiration_i64_persistence(
                            audit.cursor_before,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "cursor_after": inspiration_i64_persistence(
                            audit.cursor_after,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "projection": bson_value(
                            &selection,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "created_at_epoch": inspiration_i64_persistence(
                            now,
                            CollectionName::PrivateInspirationSelections,
                        )?,
                        "created_at": now_date,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("reserve inspiration selection", error)
                    })?;
                let updated = campaigns
                    .update_one(
                        doc! {
                            "_id": command.campaign_session_id.as_str(),
                            "safety.revision": safety.revision,
                            "safety.rng_cursor": safety.rng_cursor,
                        },
                        doc! {
                            "$set": {
                                "safety.rng_cursor": inspiration_i64_persistence(
                                    audit.cursor_after,
                                    CollectionName::Campaigns,
                                )?,
                                "updated_at": now_date,
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("advance inspiration RNG cursor", error)
                    })?;
                if updated.matched_count != 1 {
                    return Err(PersistenceError::RevisionConflict {
                        entity: "private inspiration settings",
                        id: command.campaign_session_id.to_string(),
                        expected: command.expected_settings_revision,
                        actual: settings_revision,
                    });
                }
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "selection_reserved",
                    "selection",
                    selection_id.as_str(),
                    audit.selected_source_id.as_deref(),
                    "applied",
                    now_date,
                    Some(doc! {
                        "eligible_set_digest": audit.eligible_set_digest.as_str(),
                        "eligible_source_count": i64::from(audit.eligible_source_count),
                        "turn_number": inspiration_i64_persistence(
                            turn_number,
                            CollectionName::AuditEvents,
                        )?,
                    }),
                )
                .await?;
                Ok(Ok(selection))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    pub(crate) async fn apply_private_inspiration_presentation_control(
        &self,
        command: &ApplyPresentationPrivacyCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<PresentationPrivacyOutcome, PrivateInspirationError> {
        let now_date = epoch_date(now, "presentation_privacy_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let vetoes = self
            .store()
            .document_collection(CollectionName::PrivateInspirationVetoes);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let sources = sources.clone();
            let vetoes = vetoes.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_receipt::<PresentationPrivacyOutcome>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "presentation_control",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(outcome) => return Ok(Ok(outcome)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(presentation) = presentations
                    .find_one(doc! {
                        "_id": command.presentation_id.as_str(),
                        "campaign_id": command.campaign_session_id.as_str(),
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load private inspiration presentation", error)
                    })?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let Ok(work_id) = presentation.get_str("private_inspiration_work_id") else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let work_id = work_id.to_owned();
                let Some(stored_work) = work
                    .find_one(doc! {
                        "_id": &work_id,
                        "campaign_id": command.campaign_session_id.as_str(),
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load presentation inspiration work", error)
                    })?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                stored_work.validate().map_err(domain_as_schema)?;
                if !matches!(stored_work.state.as_str(), "completed" | "redacted") {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let source_revision = inspiration_u64_persistence(
                    stored_work.source_revision,
                    CollectionName::PrivateInspirationWork,
                )?;
                let Some(source_document) =
                    load_source(&sources, session, &stored_work.source_id, source_revision).await?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let source = source_document.stored().map_err(domain_as_schema)?;
                if source.projection.source_digest.as_str() != stored_work.source_digest {
                    return Err(private_schema(
                        CollectionName::PrivateInspirationWork,
                        "presentation work source digest does not match registry",
                    ));
                }

                let mut settings_revision = None;
                match command.action {
                    PresentationPrivacyAction::Veil | PresentationPrivacyAction::Report => {
                        presentations
                            .update_one(
                                doc! {
                                    "_id": command.presentation_id.as_str(),
                                    "campaign_id": command.campaign_session_id.as_str(),
                                    "private_inspiration_work_id": &work_id,
                                },
                                doc! {
                                    "$set": {
                                        "body": PRIVATE_INSPIRATION_REDACTION_BODY,
                                        "privacy_state": "redacted",
                                        "updated_at": now_date,
                                    }
                                },
                            )
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo(
                                    "redact private inspiration presentation",
                                    error,
                                )
                            })?;
                        work.update_one(
                            doc! {
                                "_id": &work_id,
                                "state": { "$in": ["completed", "redacted"] },
                            },
                            doc! {
                                "$set": { "state": "redacted", "updated_at": now_date }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("redact private inspiration work", error)
                        })?;
                        let operation = if command.action == PresentationPrivacyAction::Report {
                            let Some(mut safety) = campaign.safety.clone() else {
                                return Ok(Err(PrivateInspirationError::NotFound));
                            };
                            let current_revision = inspiration_u64_persistence(
                                safety.revision,
                                CollectionName::Campaigns,
                            )?;
                            safety.revision = safety.revision.checked_add(1).ok_or_else(|| {
                                private_schema(
                                    CollectionName::Campaigns,
                                    "settings revision overflow",
                                )
                            })?;
                            safety.generation_paused = true;
                            safety.updated_at_epoch =
                                inspiration_i64_persistence(now, CollectionName::Campaigns)?;
                            replace_campaign_safety(
                                &campaigns,
                                session,
                                &campaign.id,
                                Some(current_revision),
                                &safety,
                                now_date,
                            )
                            .await?;
                            settings_revision = Some(inspiration_u64_persistence(
                                safety.revision,
                                CollectionName::Campaigns,
                            )?);
                            "privacy_reported"
                        } else {
                            "presentation_veiled"
                        };
                        insert_privacy_audit(
                            &audits,
                            session,
                            Some(command.campaign_session_id.as_str()),
                            operation,
                            "derived_work",
                            &work_id,
                            Some(command.presentation_id.as_str()),
                            "applied",
                            now_date,
                            None,
                        )
                        .await?;
                    }
                    PresentationPrivacyAction::VetoSource
                    | PresentationPrivacyAction::VetoCategory => {
                        let scope = if command.action == PresentationPrivacyAction::VetoSource {
                            InspirationVetoScope::SourceVersion {
                                source_id: source.projection.source_id.clone(),
                                source_version: source.projection.source_version,
                                source_digest: source.projection.source_digest.clone(),
                            }
                        } else {
                            InspirationVetoScope::Category {
                                category_id: source.projection.category_id.clone(),
                            }
                        };
                        insert_owner_veto(
                            &vetoes,
                            &audits,
                            session,
                            command.campaign_session_id.as_str(),
                            &scope,
                            command.presentation_id.as_str(),
                            now,
                            now_date,
                        )
                        .await?;
                        let work_filter = work_filter_for_scope(
                            &sources,
                            session,
                            command.campaign_session_id.as_str(),
                            &scope,
                        )
                        .await?;
                        cancel_pending_work(
                            &work,
                            &audits,
                            session,
                            work_filter.clone(),
                            now,
                            now_date,
                        )
                        .await?;
                        let mut completed_filter = work_filter;
                        completed_filter.insert("state", doc! { "$in": ["completed", "redacted"] });
                        let completed = load_work(&work, session, completed_filter).await?;
                        apply_completed_work_policy(
                            &work,
                            &presentations,
                            &audits,
                            session,
                            completed,
                            now_date,
                        )
                        .await?;
                    }
                }
                let outcome = PresentationPrivacyOutcome {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    presentation_id: command.presentation_id.clone(),
                    action: command.action,
                    presentation_hidden: true,
                    settings_revision,
                    effective_at_epoch: now,
                };
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "presentation_control",
                    &request_fingerprint,
                    &outcome,
                    now_date,
                )
                .await?;
                Ok(Ok(outcome))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    pub(crate) async fn apply_private_inspiration_veto(
        &self,
        command: &ApplyInspirationVetoCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<(VetoProjection, PrivacyTransitionOutcome), PrivateInspirationError> {
        let now_date = epoch_date(now, "veto_time_range")?;
        let veto_id = internal_id("inspiration-veto")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let vetoes = self
            .store()
            .document_collection(CollectionName::PrivateInspirationVetoes);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let participants = participants.clone();
            let sources = sources.clone();
            let vetoes = vetoes.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            let veto_id = veto_id.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, command.campaign_session_id.as_str())
                    .await?;
                match load_receipt::<VetoReceipt>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "veto_apply",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(receipt) => {
                        return Ok(Ok((receipt.veto, receipt.transition)));
                    }
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                if !participant_is_verified(&participants, session, command.participant_id.as_str())
                    .await?
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                if let InspirationVetoScope::SourceVersion {
                    source_id,
                    source_version,
                    source_digest,
                } = &command.scope
                {
                    let Some(source) =
                        load_source(&sources, session, source_id.as_str(), *source_version).await?
                    else {
                        return Ok(Err(PrivateInspirationError::NotFound));
                    };
                    let source = source.stored().map_err(domain_as_schema)?;
                    if source.projection.source_digest != *source_digest
                        || !source
                            .participants
                            .contains(command.participant_id.as_str())
                    {
                        return Ok(Err(PrivateInspirationError::ScopeDenied));
                    }
                }
                let veto = VetoProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    veto_id: veto_id.clone(),
                    campaign_session_id: command.campaign_session_id.clone(),
                    participant_id: command.participant_id.clone(),
                    scope: command.scope.clone(),
                    code: command.code,
                    created_at_epoch: now,
                };
                let fields = veto_scope_fields(&command.scope).map_err(domain_as_schema)?;
                vetoes
                    .insert_one(doc! {
                        "_id": veto_id.as_str(),
                        "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                        "campaign_id": command.campaign_session_id.as_str(),
                        "actor_participant_id": command.participant_id.as_str(),
                        "actor_kind": "participant",
                        "scope_kind": fields.scope_kind,
                        "category_id": fields.category_id,
                        "source_id": fields.source_id,
                        "source_revision": fields.source_revision,
                        "source_digest": fields.source_digest,
                        "state": "active",
                        "veto_code": command.code.as_str(),
                        "scope": bson_value(
                            &command.scope,
                            CollectionName::PrivateInspirationVetoes,
                        )?,
                        "projection": bson_value(
                            &veto,
                            CollectionName::PrivateInspirationVetoes,
                        )?,
                        "created_at_epoch": inspiration_i64_persistence(
                            now,
                            CollectionName::PrivateInspirationVetoes,
                        )?,
                        "created_at": now_date,
                        "updated_at": now_date,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("insert inspiration veto", error))?;
                let work_filter = work_filter_for_scope(
                    &sources,
                    session,
                    command.campaign_session_id.as_str(),
                    &command.scope,
                )
                .await?;
                let pending_work_cancellation_ids = cancel_pending_work(
                    &work,
                    &audits,
                    session,
                    work_filter.clone(),
                    now,
                    now_date,
                )
                .await?;
                let mut completed_filter = work_filter;
                completed_filter.insert("state", doc! { "$in": ["completed", "redacted"] });
                let completed = load_work(&work, session, completed_filter).await?;
                apply_completed_work_policy(
                    &work,
                    &presentations,
                    &audits,
                    session,
                    completed,
                    now_date,
                )
                .await?;
                let transition = PrivacyTransitionOutcome {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    subject_id: veto_id.clone(),
                    pending_work_cancellation_ids,
                    effective_at_epoch: now,
                };
                let receipt = VetoReceipt {
                    veto: veto.clone(),
                    transition: transition.clone(),
                };
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "veto_apply",
                    &request_fingerprint,
                    &receipt,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "veto_applied",
                    "veto",
                    veto.veto_id.as_str(),
                    Some(command.participant_id.as_str()),
                    "applied",
                    now_date,
                    Some(doc! { "scope_kind": veto_scope_name(&command.scope) }),
                )
                .await?;
                Ok(Ok((veto, transition)))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn register_private_inspiration_derived_work(
        &self,
        command: &RegisterDerivedWorkCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<DerivedWorkProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "derived_work_registration_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let selections = self
            .store()
            .document_collection(CollectionName::PrivateInspirationSelections);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let vetoes = self
            .store()
            .document_collection(CollectionName::PrivateInspirationVetoes);
        let consents = self
            .store()
            .collection::<ConsentDocument>(CollectionName::PrivateInspirationConsents);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let selections = selections.clone();
            let sources = sources.clone();
            let vetoes = vetoes.clone();
            let consents = consents.clone();
            let work = work.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, command.campaign_session_id.as_str())
                    .await?;
                match load_receipt::<DerivedWorkProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "derived_work_register",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(selection_document) = selections
                    .find_one(doc! {
                        "_id": command.selection_id.as_str(),
                        "campaign_id": command.campaign_session_id.as_str(),
                        "source_id": { "$ne": Bson::Null },
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load inspiration selection for work", error)
                    })?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let selection: PrivateInspirationSelection = decode_field(
                    &selection_document,
                    "projection",
                    CollectionName::PrivateInspirationSelections,
                )?;
                let media = required_string(
                    &selection_document,
                    "media",
                    CollectionName::PrivateInspirationSelections,
                )?;
                let (Some(source_id), Some(source_revision), Some(source_digest)) = (
                    selection.audit.selected_source_id.as_deref(),
                    selection.source_version,
                    selection.audit.selected_source_digest.as_ref(),
                ) else {
                    return Err(private_schema(
                        CollectionName::PrivateInspirationSelections,
                        "selected source binding is incomplete",
                    ));
                };
                if media != command.kind.as_str() {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let Some(source_document) =
                    load_source(&sources, session, source_id, source_revision).await?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let source = source_document.stored().map_err(domain_as_schema)?;
                if source.projection.source_digest != *source_digest
                    || source_is_vetoed(
                        &vetoes,
                        session,
                        command.campaign_session_id.as_str(),
                        &source,
                    )
                    .await?
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let mut consent_cursor = consents
                    .find(doc! {
                        "campaign_id": command.campaign_session_id.as_str(),
                        "source_id": source_id,
                        "source_revision": inspiration_i64_persistence(
                            source_revision,
                            CollectionName::PrivateInspirationConsents,
                        )?,
                        "source_digest": source_digest.as_str(),
                        "media": &media,
                        "state": "active",
                        "expires_at_epoch": {
                            "$gt": inspiration_i64_persistence(
                                now,
                                CollectionName::PrivateInspirationConsents,
                            )?
                        },
                    })
                    .sort(doc! { "participant_id": 1_i64 })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load derived work consent policies", error)
                    })?;
                let mut policies = Vec::new();
                while consent_cursor
                    .advance(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("read derived work consent policies", error)
                    })?
                {
                    let consent: ConsentDocument =
                        consent_cursor.deserialize_current().map_err(|_| {
                            private_schema(
                                CollectionName::PrivateInspirationConsents,
                                "derived work consent could not be decoded",
                            )
                        })?;
                    policies.push(
                        consent
                            .checked_projection()
                            .map_err(domain_as_schema)?
                            .artifact_policy,
                    );
                }
                if policies.len() != source.participants.len()
                    || policies.iter().any(|stored| {
                        artifact_policy_rank(command.artifact_policy)
                            < artifact_policy_rank(*stored)
                    })
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let storage_selection_id = if work
                    .find_one(doc! { "selection_id": command.selection_id.as_str() })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check inspiration selection work binding", error)
                    })?
                    .is_some()
                {
                    command.work_id.to_string()
                } else {
                    command.selection_id.to_string()
                };
                let projection = DerivedWorkProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    work_id: command.work_id.clone(),
                    selection_id: command.selection_id.clone(),
                    source_id: OpaqueInspirationId::new(source_id.to_owned())
                        .map_err(domain_as_schema)?,
                    source_version: source_revision,
                    source_digest: source_digest.clone(),
                    kind: command.kind,
                    artifact_policy: command.artifact_policy,
                };
                work.insert_one(WorkDocument {
                    id: command.work_id.to_string(),
                    schema_version: i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                    campaign_id: command.campaign_session_id.to_string(),
                    selection_id: storage_selection_id,
                    source_selection_id: command.selection_id.to_string(),
                    source_id: source_id.to_owned(),
                    source_revision: inspiration_i64_persistence(
                        source_revision,
                        CollectionName::PrivateInspirationWork,
                    )?,
                    source_digest: source_digest.as_str().to_owned(),
                    work_kind: command.kind.as_str().to_owned(),
                    state: "pending".to_owned(),
                    artifact_policy: command.artifact_policy.as_str().to_owned(),
                    completed_artifact_id: None,
                    cancellation_requested_at_epoch: None,
                    created_at_epoch: inspiration_i64_persistence(
                        now,
                        CollectionName::PrivateInspirationWork,
                    )?,
                    created_at: now_date,
                    updated_at: now_date,
                })
                .session(&mut *session)
                .await
                .map_err(|error| {
                    PersistenceError::mongo("register private inspiration work", error)
                })?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "derived_work_register",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "derived_work_registered",
                    "derived_work",
                    command.work_id.as_str(),
                    Some(command.selection_id.as_str()),
                    "applied",
                    now_date,
                    Some(doc! {
                        "kind": command.kind.as_str(),
                        "artifact_policy": command.artifact_policy.as_str(),
                    }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    pub(crate) async fn grant_private_inspiration_consent(
        &self,
        command: &GrantConsentCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<ConsentGrantProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "consent_grant_time_range")?;
        let expires_at = epoch_date(command.expires_at_epoch, "consent_expiry_range")?;
        let grant_id = internal_id("consent-grant")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let consents = self
            .store()
            .collection::<ConsentDocument>(CollectionName::PrivateInspirationConsents);
        let vetoes = self
            .store()
            .document_collection(CollectionName::PrivateInspirationVetoes);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let participants = participants.clone();
            let sources = sources.clone();
            let consents = consents.clone();
            let vetoes = vetoes.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            let grant_id = grant_id.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_receipt::<ConsentGrantProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "consent_grant",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(safety) = campaign.safety.as_ref() else {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                };
                safety.projection(&campaign.id).map_err(domain_as_schema)?;
                if !safety.enabled || !safety.safety_setup_complete {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                if !participant_is_verified(&participants, session, command.participant_id.as_str())
                    .await?
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let Some(source_document) = load_source(
                    &sources,
                    session,
                    command.source_id.as_str(),
                    command.source_version,
                )
                .await?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let source = source_document.stored().map_err(domain_as_schema)?;
                let command_sensitivities = command
                    .sensitivity_codes
                    .iter()
                    .map(ToString::to_string)
                    .collect::<BTreeSet<_>>();
                let allowed_sensitivities = safety.allowed_sensitivities.iter().cloned().collect();
                if source.projection.source_digest != command.source_digest
                    || source.projection.review_state != SourceReviewState::Approved
                    || !source.projection.q11_screened
                    || source
                        .projection
                        .expires_at_epoch
                        .is_some_and(|expiry| expiry <= now)
                    || !source
                        .participants
                        .contains(command.participant_id.as_str())
                    || !source.projection.eligible_media.contains(&command.media)
                    || source.sensitivities != command_sensitivities
                    || !source.sensitivities.is_subset(&allowed_sensitivities)
                    || source_is_vetoed(
                        &vetoes,
                        session,
                        command.campaign_session_id.as_str(),
                        &source,
                    )
                    .await?
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                consents
                    .update_many(
                        doc! {
                            "campaign_id": command.campaign_session_id.as_str(),
                            "state": "active",
                            "expires_at_epoch": {
                                "$lte": inspiration_i64_persistence(
                                    now,
                                    CollectionName::PrivateInspirationConsents,
                                )?
                            },
                        },
                        doc! {
                            "$set": {
                                "state": "expired",
                                "projection.state": "expired",
                                "updated_at": now_date,
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("expire inspiration consents", error)
                    })?;
                let exact_scope = doc! {
                    "campaign_id": command.campaign_session_id.as_str(),
                    "source_id": command.source_id.as_str(),
                    "source_revision": inspiration_i64_persistence(
                        command.source_version,
                        CollectionName::PrivateInspirationConsents,
                    )?,
                    "participant_id": command.participant_id.as_str(),
                    "audience": command.audience.as_str(),
                    "media": command.media.as_str(),
                    "transformation": command.transformation.as_str(),
                };
                let mut active_filter = exact_scope.clone();
                active_filter.insert("state", "active");
                if consents
                    .find_one(active_filter)
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check active inspiration consent", error)
                    })?
                    .is_some()
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let next_version = consents
                    .find_one(doc! {
                        "campaign_id": command.campaign_session_id.as_str(),
                        "source_id": command.source_id.as_str(),
                        "participant_id": command.participant_id.as_str(),
                    })
                    .sort(doc! { "version": -1_i64 })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load latest inspiration consent version", error)
                    })?
                    .map_or(Ok(1_i64), |stored| {
                        stored.version.checked_add(1).ok_or_else(|| {
                            private_schema(
                                CollectionName::PrivateInspirationConsents,
                                "consent version overflow",
                            )
                        })
                    })?;
                let projection = ConsentGrantProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    grant_id: grant_id.clone(),
                    source_id: command.source_id.clone(),
                    source_version: command.source_version,
                    source_digest: command.source_digest.clone(),
                    participant_id: command.participant_id.clone(),
                    audience: command.audience,
                    media: command.media,
                    transformation: command.transformation,
                    artifact_policy: command.artifact_policy,
                    expires_at_epoch: command.expires_at_epoch,
                    state: ConsentGrantState::Active,
                };
                consents
                    .insert_one(ConsentDocument {
                        id: grant_id.to_string(),
                        schema_version: i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                        campaign_id: command.campaign_session_id.to_string(),
                        source_id: command.source_id.to_string(),
                        source_revision: inspiration_i64_persistence(
                            command.source_version,
                            CollectionName::PrivateInspirationConsents,
                        )?,
                        source_digest: command.source_digest.as_str().to_owned(),
                        participant_id: command.participant_id.to_string(),
                        version: next_version,
                        audience: command.audience.as_str().to_owned(),
                        media: command.media.as_str().to_owned(),
                        transformation: command.transformation.as_str().to_owned(),
                        artifact_policy: command.artifact_policy.as_str().to_owned(),
                        sensitivities: command
                            .sensitivity_codes
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                        reviewer_id: command.reviewer_id.as_str().to_owned(),
                        participant_confirmation_digest: command
                            .participant_confirmation_digest
                            .as_str()
                            .to_owned(),
                        review_evidence_digest: command.review_evidence_digest.as_str().to_owned(),
                        state: "active".to_owned(),
                        projection: projection.clone(),
                        granted_at_epoch: inspiration_i64_persistence(
                            now,
                            CollectionName::PrivateInspirationConsents,
                        )?,
                        expires_at_epoch: inspiration_i64_persistence(
                            command.expires_at_epoch,
                            CollectionName::PrivateInspirationConsents,
                        )?,
                        expires_at,
                        revoked_at_epoch: None,
                        revocation_code: None,
                        created_at: now_date,
                        updated_at: now_date,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("grant inspiration consent", error))?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "consent_grant",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "consent_granted",
                    "consent_grant",
                    projection.grant_id.as_str(),
                    Some(command.source_id.as_str()),
                    "applied",
                    now_date,
                    Some(doc! {
                        "participant_id": command.participant_id.as_str(),
                        "media": command.media.as_str(),
                    }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn revoke_private_inspiration_consent(
        &self,
        command: &RevokeConsentCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<PrivacyTransitionOutcome, PrivateInspirationError> {
        let now_date = epoch_date(now, "consent_revocation_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let consents = self
            .store()
            .collection::<ConsentDocument>(CollectionName::PrivateInspirationConsents);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let consents = consents.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, command.campaign_session_id.as_str())
                    .await?;
                match load_receipt::<PrivacyTransitionOutcome>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "consent_revoke",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(outcome) => return Ok(Ok(outcome)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(consent) = consents
                    .find_one(doc! { "_id": command.grant_id.as_str() })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load inspiration consent for revocation", error)
                    })?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                consent.checked_projection().map_err(domain_as_schema)?;
                if consent.campaign_id != command.campaign_session_id.as_str()
                    || consent.participant_id != command.requester_participant_id.as_str()
                    || consent.state == "revoked"
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let updated = consents
                    .update_one(
                        doc! {
                            "_id": command.grant_id.as_str(),
                            "campaign_id": command.campaign_session_id.as_str(),
                            "participant_id": command.requester_participant_id.as_str(),
                            "state": { "$ne": "revoked" },
                        },
                        doc! {
                            "$set": {
                                "state": "revoked",
                                "projection.state": "revoked",
                                "revoked_at_epoch": inspiration_i64_persistence(
                                    now,
                                    CollectionName::PrivateInspirationConsents,
                                )?,
                                "revocation_code": command.reason.as_str(),
                                "updated_at": now_date,
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("revoke inspiration consent", error)
                    })?;
                if updated.matched_count != 1 {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let source_filter = doc! {
                    "campaign_id": command.campaign_session_id.as_str(),
                    "source_id": &consent.source_id,
                    "source_revision": consent.source_revision,
                };
                let pending_work_cancellation_ids = cancel_pending_work(
                    &work,
                    &audits,
                    session,
                    source_filter.clone(),
                    now,
                    now_date,
                )
                .await?;
                let mut completed_filter = source_filter;
                completed_filter.insert("state", doc! { "$in": ["completed", "redacted"] });
                let completed = load_work(&work, session, completed_filter).await?;
                apply_completed_work_policy(
                    &work,
                    &presentations,
                    &audits,
                    session,
                    completed,
                    now_date,
                )
                .await?;
                let outcome = PrivacyTransitionOutcome {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    subject_id: command.grant_id.clone(),
                    pending_work_cancellation_ids,
                    effective_at_epoch: now,
                };
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "consent_revoke",
                    &request_fingerprint,
                    &outcome,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "consent_revoked",
                    "consent_grant",
                    command.grant_id.as_str(),
                    Some(command.requester_participant_id.as_str()),
                    "applied",
                    now_date,
                    Some(doc! { "reason": command.reason.as_str() }),
                )
                .await?;
                Ok(Ok(outcome))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    pub(crate) async fn delete_private_inspiration_participant_data(
        &self,
        command: &DeleteParticipantPrivateDataCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<ParticipantDeletionOutcome, PrivateInspirationError> {
        let now_date = epoch_date(now, "participant_deletion_time_range")?;
        let tombstone_delete_after_epoch = now
            .checked_add(PARTICIPANT_DELETION_TOMBSTONE_SECONDS)
            .ok_or_else(|| invalid("deletion_tombstone_expiry_overflow"))?;
        let purge_at = epoch_date(
            tombstone_delete_after_epoch,
            "participant_tombstone_expiry_range",
        )?;
        let deletion_id = internal_id("participant-deletion")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let consents = self
            .store()
            .collection::<ConsentDocument>(CollectionName::PrivateInspirationConsents);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let tombstones = self
            .store()
            .document_collection(CollectionName::DeletionTombstones);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let participants = participants.clone();
            let sources = sources.clone();
            let consents = consents.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let tombstones = tombstones.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            let deletion_id = deletion_id.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, command.campaign_session_id.as_str())
                    .await?;
                match load_receipt::<ParticipantDeletionOutcome>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "participant_delete",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(outcome) => return Ok(Ok(outcome)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(participant) = participants
                    .find_one(doc! { "_id": command.participant_id.as_str() })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load inspiration participant for deletion", error)
                    })?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let state = required_string(
                    &participant,
                    "state",
                    CollectionName::PrivateInspirationParticipants,
                )?;
                if !matches!(state.as_str(), "verified" | "revoked") {
                    return Err(private_schema(
                        CollectionName::PrivateInspirationParticipants,
                        "participant verification state is invalid",
                    ));
                }
                let verified_at_epoch = participant.get_i64("verified_at_epoch").map_err(|_| {
                    private_schema(
                        CollectionName::PrivateInspirationParticipants,
                        "participant verification time is missing",
                    )
                })?;
                if tombstones
                    .find_one(doc! {
                        "entity_kind": PARTICIPANT_TOMBSTONE_KIND,
                        "entity_id": command.participant_id.as_str(),
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check participant deletion tombstone", error)
                    })?
                    .is_some()
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }

                let mut source_cursor = sources
                    .find(doc! { "participants": command.participant_id.as_str() })
                    .sort(doc! { "logical_id": 1_i64, "revision": 1_i64 })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load participant inspiration sources", error)
                    })?;
                let mut affected_sources = Vec::new();
                while source_cursor
                    .advance(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("read participant inspiration sources", error)
                    })?
                {
                    affected_sources.push(source_cursor.deserialize_current().map_err(|_| {
                        private_schema(
                            CollectionName::PrivateInspirationSources,
                            "participant source document could not be decoded",
                        )
                    })?);
                }

                let participant_update = participants
                    .update_one(
                        doc! { "_id": command.participant_id.as_str() },
                        doc! {
                            "$set": {
                                "state": "revoked",
                                "verification_evidence_digest":
                                    command.deletion_evidence_digest.as_str(),
                                "verifier_id": command.operator_id.as_str(),
                                "revoked_at_epoch": verified_at_epoch.max(
                                    inspiration_i64_persistence(
                                        now,
                                        CollectionName::PrivateInspirationParticipants,
                                    )?,
                                ),
                                "projection.revoked": true,
                                "updated_at": now_date,
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("revoke inspiration participant", error)
                    })?;
                if participant_update.matched_count != 1 {
                    return Ok(Err(PrivateInspirationError::NotFound));
                }

                let mut consent_cursor = consents
                    .find(doc! {
                        "participant_id": command.participant_id.as_str(),
                        "state": { "$in": ["active", "expired"] },
                    })
                    .sort(doc! { "_id": 1_i64 })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo(
                            "load participant inspiration consents for revocation",
                            error,
                        )
                    })?;
                let mut revoked_grants = 0_u64;
                while consent_cursor
                    .advance(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo(
                            "read participant inspiration consents for revocation",
                            error,
                        )
                    })?
                {
                    let consent: ConsentDocument =
                        consent_cursor.deserialize_current().map_err(|_| {
                            private_schema(
                                CollectionName::PrivateInspirationConsents,
                                "participant consent document could not be decoded",
                            )
                        })?;
                    consent.checked_projection().map_err(domain_as_schema)?;
                    let revoked_at_epoch =
                        consent.granted_at_epoch.max(inspiration_i64_persistence(
                            now,
                            CollectionName::PrivateInspirationConsents,
                        )?);
                    let updated = consents
                        .update_one(
                            doc! {
                                "_id": &consent.id,
                                "state": { "$in": ["active", "expired"] },
                            },
                            doc! {
                                "$set": {
                                    "state": "revoked",
                                    "projection.state": "revoked",
                                    "revoked_at_epoch": revoked_at_epoch,
                                    "revocation_code": "privacy_request",
                                    "updated_at": now_date,
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo("revoke participant inspiration consent", error)
                        })?;
                    if updated.matched_count != 1 {
                        return Err(private_schema(
                            CollectionName::PrivateInspirationConsents,
                            "participant consent changed during revocation",
                        ));
                    }
                    revoked_grants = revoked_grants
                        .checked_add(updated.modified_count)
                        .ok_or_else(|| {
                            private_schema(
                                CollectionName::PrivateInspirationConsents,
                                "revoked participant grant count overflow",
                            )
                        })?;
                }

                for source in &affected_sources {
                    let mut projection = source.stored().map_err(domain_as_schema)?.projection;
                    projection.review_state = SourceReviewState::Quarantined;
                    projection.q11_screened = false;
                    let updated = sources
                        .update_one(
                            doc! { "_id": &source.id },
                            doc! {
                                "$set": {
                                    "review_state": SourceReviewState::Quarantined.as_str(),
                                    "q11_screened": false,
                                    "review_evidence_digest":
                                        command.deletion_evidence_digest.as_str(),
                                    "reviewer_id": command.operator_id.as_str(),
                                    "reviewed_at_epoch": inspiration_i64_persistence(
                                        now,
                                        CollectionName::PrivateInspirationSources,
                                    )?,
                                    "projection": bson_value(
                                        &projection,
                                        CollectionName::PrivateInspirationSources,
                                    )?,
                                    "updated_at": now_date,
                                }
                            },
                        )
                        .session(&mut *session)
                        .await
                        .map_err(|error| {
                            PersistenceError::mongo(
                                "quarantine participant inspiration source",
                                error,
                            )
                        })?;
                    if updated.matched_count != 1 {
                        return Err(private_schema(
                            CollectionName::PrivateInspirationSources,
                            "participant source disappeared during deletion",
                        ));
                    }
                    insert_privacy_audit(
                        &audits,
                        session,
                        Some(command.campaign_session_id.as_str()),
                        "source_quarantined",
                        "source_version",
                        &source.logical_id,
                        Some(&source.revision.to_string()),
                        "applied",
                        now_date,
                        None,
                    )
                    .await?;
                }

                let source_filter = source_key_filter(&affected_sources);
                let pending_work_cancellation_ids = if source_filter.is_empty() {
                    Vec::new()
                } else {
                    cancel_pending_work(
                        &work,
                        &audits,
                        session,
                        doc! { "$or": source_filter.clone() },
                        now,
                        now_date,
                    )
                    .await?
                };
                let completed = if source_filter.is_empty() {
                    Vec::new()
                } else {
                    load_work(
                        &work,
                        session,
                        doc! {
                            "$and": [
                                { "$or": source_filter },
                                { "state": { "$in": ["completed", "redacted"] } },
                            ],
                        },
                    )
                    .await?
                };
                let affected_completed_artifact_count =
                    u32::try_from(completed.len()).map_err(|_| {
                        private_schema(
                            CollectionName::PrivateInspirationWork,
                            "completed participant work count exceeds u32",
                        )
                    })?;
                apply_completed_work_policy(
                    &work,
                    &presentations,
                    &audits,
                    session,
                    completed,
                    now_date,
                )
                .await?;

                tombstones
                    .insert_one(doc! {
                        "_id": deletion_id.as_str(),
                        "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                        "entity_kind": PARTICIPANT_TOMBSTONE_KIND,
                        "entity_id": command.participant_id.as_str(),
                        "deletion_id": deletion_id.as_str(),
                        "digest": command.deletion_evidence_digest.as_str(),
                        "requested_by_operator_id": command.operator_id.as_str(),
                        "requested_at_epoch": inspiration_i64_persistence(
                            now,
                            CollectionName::DeletionTombstones,
                        )?,
                        "delete_after_epoch": inspiration_i64_persistence(
                            tombstone_delete_after_epoch,
                            CollectionName::DeletionTombstones,
                        )?,
                        "purge_at": purge_at,
                        "created_at": now_date,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("insert participant deletion tombstone", error)
                    })?;

                let outcome = ParticipantDeletionOutcome {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    participant_id: command.participant_id.clone(),
                    revoked_grant_count: u32::try_from(revoked_grants).map_err(|_| {
                        private_schema(
                            CollectionName::PrivateInspirationConsents,
                            "revoked participant grant count exceeds u32",
                        )
                    })?,
                    quarantined_source_count: u32::try_from(affected_sources.len()).map_err(
                        |_| {
                            private_schema(
                                CollectionName::PrivateInspirationSources,
                                "quarantined participant source count exceeds u32",
                            )
                        },
                    )?,
                    pending_work_cancellation_ids,
                    affected_completed_artifact_count,
                    effective_at_epoch: now,
                    tombstone_delete_after_epoch,
                };
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "participant_delete",
                    &request_fingerprint,
                    &outcome,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "participant_deletion_requested",
                    "participant",
                    command.participant_id.as_str(),
                    None,
                    "applied",
                    now_date,
                    Some(doc! {
                        "operator_id": command.operator_id.as_str(),
                        "tombstone_delete_after_epoch":
                            inspiration_i64_persistence(
                                tombstone_delete_after_epoch,
                                CollectionName::DeletionTombstones,
                            )?,
                    }),
                )
                .await?;
                Ok(Ok(outcome))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    pub(crate) async fn register_private_inspiration_source(
        &self,
        command: &RegisterSourceVersionCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<SourceVersionProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "source_registration_time_range")?;
        let expires_at = command
            .expires_at_epoch
            .map(|value| epoch_date(value, "source_expiry_range"))
            .transpose()?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let participants = participants.clone();
            let sources = sources.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, command.campaign_session_id.as_str())
                    .await?;
                match load_receipt::<SourceVersionProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "source_register",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                for participant in &command.participant_ids {
                    if !participant_is_verified(&participants, session, participant.as_str())
                        .await?
                    {
                        return Ok(Err(PrivateInspirationError::ScopeDenied));
                    }
                }
                if sources
                    .find_one(doc! {
                        "logical_id": command.source_id.as_str(),
                        "revision": inspiration_i64_persistence(
                            command.source_version,
                            CollectionName::PrivateInspirationSources,
                        )?,
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check inspiration source version", error)
                    })?
                    .is_some()
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let projection = SourceVersionProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    source_id: command.source_id.clone(),
                    source_version: command.source_version,
                    source_digest: command.source_digest.clone(),
                    category_id: command.category_id.clone(),
                    review_state: SourceReviewState::Pending,
                    q11_screened: false,
                    participant_count: u32::try_from(command.participant_ids.len()).map_err(
                        |_| {
                            private_schema(
                                CollectionName::PrivateInspirationSources,
                                "source participant count exceeds u32",
                            )
                        },
                    )?,
                    sensitivity_count: u32::try_from(command.sensitivity_codes.len()).map_err(
                        |_| {
                            private_schema(
                                CollectionName::PrivateInspirationSources,
                                "source sensitivity count exceeds u32",
                            )
                        },
                    )?,
                    eligible_media: command.eligible_media.clone(),
                    eligible_theme_pack_ids: command.eligible_theme_pack_ids.clone(),
                    expires_at_epoch: command.expires_at_epoch,
                };
                let runtime_projection_digest =
                    fingerprint(&command.runtime_prompt).map_err(domain_as_schema)?;
                let source = SourceDocument {
                    id: format!(
                        "private-source:{}:{}",
                        command.source_id.as_str(),
                        command.source_version
                    ),
                    schema_version: i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                    logical_id: command.source_id.as_str().to_owned(),
                    revision: inspiration_i64_persistence(
                        command.source_version,
                        CollectionName::PrivateInspirationSources,
                    )?,
                    source_digest: command.source_digest.as_str().to_owned(),
                    category_id: command.category_id.as_str().to_owned(),
                    owner_participant_id: command.owner_participant_id.as_str().to_owned(),
                    review_state: SourceReviewState::Pending.as_str().to_owned(),
                    q11_screened: false,
                    audience: InspirationAudience::PrivateCampaign.as_str().to_owned(),
                    transformation: InspirationTransformation::HighFictionDistanceV1
                        .as_str()
                        .to_owned(),
                    provenance_digest: command.provenance_digest.as_str().to_owned(),
                    expires_at,
                    expires_at_epoch: command
                        .expires_at_epoch
                        .map(|value| {
                            inspiration_i64_persistence(
                                value,
                                CollectionName::PrivateInspirationSources,
                            )
                        })
                        .transpose()?,
                    participants: command
                        .participant_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    sensitivities: command
                        .sensitivity_codes
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    media: command
                        .eligible_media
                        .iter()
                        .map(|media| media.as_str().to_owned())
                        .collect(),
                    themes: command
                        .eligible_theme_pack_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    runtime_facts: RuntimeFactsDocument {
                        neutral_facts: command.runtime_prompt.neutral_facts.clone(),
                    },
                    runtime_projection: command.runtime_prompt.clone(),
                    projection_digest: runtime_projection_digest.as_str().to_owned(),
                    projection: projection.clone(),
                    review_evidence_digest: None,
                    reviewer_id: None,
                    reviewed_at_epoch: None,
                    registered_at_epoch: inspiration_i64_persistence(
                        now,
                        CollectionName::PrivateInspirationSources,
                    )?,
                    created_at: now_date,
                    updated_at: now_date,
                };
                sources
                    .insert_one(source)
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("register inspiration source", error)
                    })?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "source_register",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "source_registered",
                    "source_version",
                    command.source_id.as_str(),
                    Some(&command.source_version.to_string()),
                    "applied",
                    now_date,
                    None,
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn review_private_inspiration_source(
        &self,
        command: &ReviewSourceVersionCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<SourceVersionProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "source_review_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let sources = self
            .store()
            .collection::<SourceDocument>(CollectionName::PrivateInspirationSources);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let sources = sources.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, command.campaign_session_id.as_str())
                    .await?;
                match load_receipt::<SourceVersionProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "source_review",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(source) = load_source(
                    &sources,
                    session,
                    command.source_id.as_str(),
                    command.source_version,
                )
                .await?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let stored = source.stored().map_err(domain_as_schema)?;
                if stored.projection.source_digest != command.source_digest
                    || stored.projection.review_state != SourceReviewState::Pending
                {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let mut projection = stored.projection;
                projection.review_state = command.decision;
                projection.q11_screened = command.q11_screened;
                let updated = sources
                    .update_one(
                        doc! {
                            "_id": &source.id,
                            "review_state": SourceReviewState::Pending.as_str(),
                            "source_digest": command.source_digest.as_str(),
                        },
                        doc! {
                            "$set": {
                                "review_state": command.decision.as_str(),
                                "q11_screened": command.q11_screened,
                                "review_evidence_digest": command.review_evidence_digest.as_str(),
                                "reviewer_id": command.reviewer_id.as_str(),
                                "reviewed_at_epoch": inspiration_i64_persistence(
                                    now,
                                    CollectionName::PrivateInspirationSources,
                                )?,
                                "projection": bson_value(
                                    &projection,
                                    CollectionName::PrivateInspirationSources,
                                )?,
                                "updated_at": now_date,
                            }
                        },
                    )
                    .session(&mut *session)
                    .await
                    .map_err(|error| PersistenceError::mongo("review inspiration source", error))?;
                if updated.matched_count != 1 {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "source_review",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "source_reviewed",
                    "source_version",
                    command.source_id.as_str(),
                    Some(&command.source_version.to_string()),
                    "applied",
                    now_date,
                    Some(doc! {
                        "decision": command.decision.as_str(),
                        "q11_screened": command.q11_screened,
                        "reviewer_id": command.reviewer_id.as_str(),
                    }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn abandon_private_inspiration_derived_work(
        &self,
        campaign_session_id: &OpaqueInspirationId,
        work_id: &OpaqueInspirationId,
        now: u64,
    ) -> Result<(), PrivateInspirationError> {
        let now_date = epoch_date(now, "derived_work_abandon_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let campaign_id = campaign_session_id.to_string();
        let work_id = work_id.to_string();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let work = work.clone();
            let audits = audits.clone();
            let campaign_id = campaign_id.clone();
            let work_id = work_id.clone();
            Box::pin(async move {
                load_campaign_in_session(&campaigns, session, &campaign_id).await?;
                let Some(stored) = work
                    .find_one(doc! { "_id": &work_id, "campaign_id": &campaign_id })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load private inspiration work", error)
                    })?
                else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                stored.validate().map_err(domain_as_schema)?;
                match stored.state.as_str() {
                    "pending" => {
                        let updated = work
                            .update_one(
                                doc! {
                                    "_id": &work_id,
                                    "campaign_id": &campaign_id,
                                    "state": "pending",
                                },
                                doc! {
                                    "$set": {
                                        "state": "cancellation_requested",
                                        "cancellation_requested_at_epoch":
                                            inspiration_i64_persistence(
                                                now,
                                                CollectionName::PrivateInspirationWork,
                                            )?,
                                        "updated_at": now_date,
                                    }
                                },
                            )
                            .session(&mut *session)
                            .await
                            .map_err(|error| {
                                PersistenceError::mongo("abandon private inspiration work", error)
                            })?;
                        if updated.matched_count != 1 {
                            return Ok(Err(PrivateInspirationError::ScopeDenied));
                        }
                        insert_privacy_audit(
                            &audits,
                            session,
                            Some(&campaign_id),
                            "derived_work_cancel_requested",
                            "derived_work",
                            &work_id,
                            None,
                            "cancel_requested",
                            now_date,
                            None,
                        )
                        .await?;
                    }
                    "cancellation_requested" | "redacted" | "deleted" => {}
                    "completed" => return Ok(Err(PrivateInspirationError::ScopeDenied)),
                    _ => {
                        return Err(private_schema(
                            CollectionName::PrivateInspirationWork,
                            "stored derived work state is invalid",
                        ));
                    }
                }
                Ok(Ok(()))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}

impl MongoRepository {
    pub(crate) async fn set_private_inspiration_campaign_pause(
        &self,
        command: &SetCampaignInspirationPauseCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "settings_pause_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_receipt::<CampaignInspirationSettingsProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "settings_pause",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(mut safety) = campaign.safety else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let current_revision =
                    inspiration_u64_persistence(safety.revision, CollectionName::Campaigns)?;
                if current_revision != command.expected_revision {
                    return Ok(Err(PrivateInspirationError::RevisionConflict {
                        expected: command.expected_revision,
                        current: current_revision,
                    }));
                }
                safety.revision = safety.revision.checked_add(1).ok_or_else(|| {
                    private_schema(CollectionName::Campaigns, "settings revision overflow")
                })?;
                safety.generation_paused = command.paused;
                safety.updated_at_epoch =
                    inspiration_i64_persistence(now, CollectionName::Campaigns)?;
                replace_campaign_safety(
                    &campaigns,
                    session,
                    &campaign.id,
                    Some(current_revision),
                    &safety,
                    now_date,
                )
                .await?;
                let projection = safety.projection(&campaign.id).map_err(domain_as_schema)?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    &campaign.id,
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "settings_pause",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(&campaign.id),
                    "settings_changed",
                    "campaign",
                    &campaign.id,
                    None,
                    "applied",
                    now_date,
                    Some(doc! { "paused": command.paused, "revision": safety.revision }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn disable_private_inspiration_campaign(
        &self,
        command: &DisableCampaignInspirationCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "settings_disable_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let consents = self
            .store()
            .document_collection(CollectionName::PrivateInspirationConsents);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let consents = consents.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_receipt::<CampaignInspirationSettingsProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "settings_disable",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let Some(mut safety) = campaign.safety else {
                    return Ok(Err(PrivateInspirationError::NotFound));
                };
                let current_revision =
                    inspiration_u64_persistence(safety.revision, CollectionName::Campaigns)?;
                if current_revision != command.expected_revision {
                    return Ok(Err(PrivateInspirationError::RevisionConflict {
                        expected: command.expected_revision,
                        current: current_revision,
                    }));
                }
                safety.revision = safety.revision.checked_add(1).ok_or_else(|| {
                    private_schema(CollectionName::Campaigns, "settings revision overflow")
                })?;
                safety.enabled = false;
                safety.generation_paused = false;
                safety.updated_at_epoch =
                    inspiration_i64_persistence(now, CollectionName::Campaigns)?;
                replace_campaign_safety(
                    &campaigns,
                    session,
                    &campaign.id,
                    Some(current_revision),
                    &safety,
                    now_date,
                )
                .await?;
                revoke_campaign_consents(&consents, session, &campaign.id, now, now_date).await?;
                cancel_pending_work(
                    &work,
                    &audits,
                    session,
                    doc! { "campaign_id": &campaign.id },
                    now,
                    now_date,
                )
                .await?;
                let completed = load_work(
                    &work,
                    session,
                    doc! {
                        "campaign_id": &campaign.id,
                        "state": { "$in": ["completed", "redacted"] },
                    },
                )
                .await?;
                apply_completed_work_policy(
                    &work,
                    &presentations,
                    &audits,
                    session,
                    completed,
                    now_date,
                )
                .await?;
                let projection = safety.projection(&campaign.id).map_err(domain_as_schema)?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    &campaign.id,
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "settings_disable",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(&campaign.id),
                    "settings_changed",
                    "campaign",
                    &campaign.id,
                    None,
                    "applied",
                    now_date,
                    Some(doc! { "enabled": false, "revision": safety.revision }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn configure_private_inspiration_campaign(
        &self,
        command: &ConfigureCampaignInspirationCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "settings_change_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let consents = self
            .store()
            .document_collection(CollectionName::PrivateInspirationConsents);
        let work = self
            .store()
            .collection::<WorkDocument>(CollectionName::PrivateInspirationWork);
        let presentations = self
            .store()
            .document_collection(CollectionName::GeneratedPresentations);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let consents = consents.clone();
            let work = work.clone();
            let presentations = presentations.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                let campaign = load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_receipt::<CampaignInspirationSettingsProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "settings_change",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                let current_revision = campaign
                    .safety
                    .as_ref()
                    .map(|safety| {
                        inspiration_u64_persistence(safety.revision, CollectionName::Campaigns)
                    })
                    .transpose()?
                    .unwrap_or(0);
                if current_revision != command.expected_revision {
                    return Ok(Err(PrivateInspirationError::RevisionConflict {
                        expected: command.expected_revision,
                        current: current_revision,
                    }));
                }
                let revision = current_revision.checked_add(1).ok_or_else(|| {
                    private_schema(CollectionName::Campaigns, "settings revision overflow")
                })?;
                let mut safety = CampaignSafetyDocument::from_command(&command, revision, now)
                    .map_err(domain_as_schema)?;
                if let Some(current) = &campaign.safety {
                    safety.rng_cursor = current.rng_cursor;
                }
                replace_campaign_safety(
                    &campaigns,
                    session,
                    &campaign.id,
                    campaign.safety.as_ref().map(|_| current_revision),
                    &safety,
                    now_date,
                )
                .await?;
                if !command.enabled {
                    revoke_campaign_consents(&consents, session, &campaign.id, now, now_date)
                        .await?;
                    cancel_pending_work(
                        &work,
                        &audits,
                        session,
                        doc! { "campaign_id": &campaign.id },
                        now,
                        now_date,
                    )
                    .await?;
                    let completed = load_work(
                        &work,
                        session,
                        doc! {
                            "campaign_id": &campaign.id,
                            "state": { "$in": ["completed", "redacted"] },
                        },
                    )
                    .await?;
                    apply_completed_work_policy(
                        &work,
                        &presentations,
                        &audits,
                        session,
                        completed,
                        now_date,
                    )
                    .await?;
                }
                let projection = safety.projection(&campaign.id).map_err(domain_as_schema)?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    &campaign.id,
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "settings_change",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(&campaign.id),
                    "settings_changed",
                    "campaign",
                    &campaign.id,
                    None,
                    "applied",
                    now_date,
                    Some(doc! {
                        "enabled": command.enabled,
                        "safety_setup_complete": safety.safety_setup_complete,
                        "revision": safety.revision,
                    }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }

    pub(crate) async fn verify_private_inspiration_participant(
        &self,
        command: &VerifyParticipantCommand,
        request_fingerprint: &Sha256Digest,
        now: u64,
    ) -> Result<ParticipantVerificationProjection, PrivateInspirationError> {
        let now_date = epoch_date(now, "participant_verification_time_range")?;
        let campaigns = self
            .campaigns()
            .clone_with_type::<InspirationCampaignDocument>();
        let receipts = self
            .store()
            .document_collection(CollectionName::CommandReceipts);
        let audits = self
            .store()
            .document_collection(CollectionName::AuditEvents);
        let participants = self
            .store()
            .document_collection(CollectionName::PrivateInspirationParticipants);
        let tombstones = self
            .store()
            .document_collection(CollectionName::DeletionTombstones);
        let command = command.clone();
        let request_fingerprint = request_fingerprint.clone();
        self.with_transaction(move |session| {
            let campaigns = campaigns.clone();
            let receipts = receipts.clone();
            let audits = audits.clone();
            let participants = participants.clone();
            let tombstones = tombstones.clone();
            let command = command.clone();
            let request_fingerprint = request_fingerprint.clone();
            Box::pin(async move {
                load_campaign_in_session(
                    &campaigns,
                    session,
                    command.campaign_session_id.as_str(),
                )
                .await?;
                match load_receipt::<ParticipantVerificationProjection>(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    command.idempotency_key.as_str(),
                    "participant_verify",
                    &request_fingerprint,
                )
                .await?
                {
                    ReceiptReplay::Replay(projection) => return Ok(Ok(projection)),
                    ReceiptReplay::Conflict => {
                        return Ok(Err(PrivateInspirationError::IdempotencyConflict));
                    }
                    ReceiptReplay::Missing => {}
                }
                tombstones
                    .delete_many(doc! {
                        "entity_kind": PARTICIPANT_TOMBSTONE_KIND,
                        "entity_id": command.participant_id.as_str(),
                        "purge_at": { "$lte": now_date },
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("clear expired participant tombstone", error)
                    })?;
                let deletion_pending = tombstones
                    .find_one(doc! {
                        "entity_kind": PARTICIPANT_TOMBSTONE_KIND,
                        "entity_id": command.participant_id.as_str(),
                    })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("check participant tombstone", error)
                    })?
                    .is_some();
                if deletion_pending {
                    return Ok(Err(PrivateInspirationError::ScopeDenied));
                }
                let existing = participants
                    .find_one(doc! { "_id": command.participant_id.as_str() })
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("load inspiration participant", error)
                    })?;
                let created_at = existing
                    .as_ref()
                    .and_then(|document| document.get_datetime("created_at").ok())
                    .copied()
                    .unwrap_or(now_date);
                let projection = ParticipantVerificationProjection {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    participant_id: command.participant_id.clone(),
                    method: command.method,
                    verified_at_epoch: now,
                    revoked: false,
                };
                participants
                    .replace_one(
                        doc! { "_id": command.participant_id.as_str() },
                        doc! {
                            "_id": command.participant_id.as_str(),
                            "schema_version": i64::from(PRIVATE_INSPIRATION_SCHEMA_VERSION),
                            "participant_id": command.participant_id.as_str(),
                            "state": "verified",
                            "verification_method": command.method.as_str(),
                            "verification_evidence_digest": command.evidence_digest.as_str(),
                            "verifier_id": command.verifier_id.as_str(),
                            "verified_at_epoch": inspiration_i64_persistence(now, CollectionName::PrivateInspirationParticipants)?,
                            "revoked_at_epoch": Bson::Null,
                            "projection": bson_value(&projection, CollectionName::PrivateInspirationParticipants)?,
                            "created_at": created_at,
                            "updated_at": now_date,
                        },
                    )
                    .upsert(true)
                    .session(&mut *session)
                    .await
                    .map_err(|error| {
                        PersistenceError::mongo("verify inspiration participant", error)
                    })?;
                insert_receipt(
                    &receipts,
                    session,
                    CAMPAIGN_COMMAND_SCOPE,
                    command.campaign_session_id.as_str(),
                    SYSTEM_ACTOR_ID,
                    command.idempotency_key.as_str(),
                    "participant_verify",
                    &request_fingerprint,
                    &projection,
                    now_date,
                )
                .await?;
                insert_privacy_audit(
                    &audits,
                    session,
                    Some(command.campaign_session_id.as_str()),
                    "participant_verified",
                    "participant",
                    command.participant_id.as_str(),
                    None,
                    "applied",
                    now_date,
                    Some(doc! {
                        "verification_method": command.method.as_str(),
                        "verifier_id": command.verifier_id.as_str(),
                    }),
                )
                .await?;
                Ok(Ok(projection))
            })
        })
        .await
        .map_err(private_persistence)?
    }
}
