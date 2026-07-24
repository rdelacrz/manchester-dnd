//! Durable, body-free consent and eligibility boundary for private inspiration.
//!
//! This module intentionally has no raw Markdown, filesystem, name, contact,
//! consent-prose, or generated-body type. Source decoding remains in `events`;
//! this boundary persists only opaque IDs, exact digests, closed policy values,
//! minimized neutral facts, revisions, and trusted epoch timestamps.

use std::{
    collections::BTreeSet,
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use manchester_dnd_core::{Sha256Digest, is_valid_opaque_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::PrivateInspirationError,
    events::{EventPrompt, EventSelectionAudit, RuntimeEventPrompt, privacy_source_id},
    repository::MongoRepository,
    seed::SeedVault,
};

pub const PRIVATE_INSPIRATION_SCHEMA_VERSION: u16 = 1;
pub const PRIVATE_INSPIRATION_EXPORT_SCHEMA_VERSION: u16 = 1;
pub const Q11_CONSERVATIVE_POLICY_ID: &str = "q11_conservative_v1";
pub const PARTICIPANT_DELETION_TOMBSTONE_SECONDS: u64 = 35 * 24 * 60 * 60;

const MAX_CAMPAIGN_SAFETY_CODES: usize = 64;
const MAX_SOURCE_THEME_IDS: usize = 16;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpaqueInspirationId(String);

impl OpaqueInspirationId {
    pub fn new(value: impl Into<String>) -> Result<Self, PrivateInspirationError> {
        let value = value.into();
        if !is_valid_opaque_id(&value) {
            return Err(invalid("invalid_opaque_id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpaqueInspirationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OpaqueInspirationId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for OpaqueInspirationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for OpaqueInspirationId {
    type Error = PrivateInspirationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<OpaqueInspirationId> for String {
    fn from(value: OpaqueInspirationId) -> Self {
        value.0
    }
}

/// Opaque verifier/reviewer/operator identity. This is deliberately distinct
/// from a represented participant ID: an operator cannot stand in for the
/// participant's own out-of-band confirmation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpaqueOperatorId(String);

impl OpaqueOperatorId {
    pub fn new(value: impl Into<String>) -> Result<Self, PrivateInspirationError> {
        let value = value.into();
        if !is_valid_opaque_id(&value) || !valid_namespaced_hex_id(&value, "operator:") {
            return Err(invalid("invalid_operator_id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpaqueOperatorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OpaqueOperatorId")
            .field(&self.0)
            .finish()
    }
}

impl TryFrom<String> for OpaqueOperatorId {
    type Error = PrivateInspirationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<OpaqueOperatorId> for String {
    fn from(value: OpaqueOperatorId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspirationAudience {
    PrivateCampaign,
}

impl InspirationAudience {
    pub(crate) const fn as_str(self) -> &'static str {
        "private_campaign"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspirationMedia {
    Text,
    Image,
    Recap,
}

impl InspirationMedia {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Recap => "recap",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "text" => Ok(Self::Text),
            "image" => Ok(Self::Image),
            "recap" => Ok(Self::Recap),
            _ => Err(invalid("stored_media")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspirationTransformation {
    HighFictionDistanceV1,
}

impl InspirationTransformation {
    pub(crate) const fn as_str(self) -> &'static str {
        "high_fiction_distance_v1"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceReviewState {
    Pending,
    Approved,
    Rejected,
    Quarantined,
}

impl SourceReviewState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Quarantined => "quarantined",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(invalid("stored_review_state")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantVerificationMethod {
    ParticipantSignedConfirmation,
    TimestampedTwoChannelAcknowledgement,
}

impl ParticipantVerificationMethod {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ParticipantSignedConfirmation => "participant_signed_confirmation",
            Self::TimestampedTwoChannelAcknowledgement => "timestamped_two_channel_acknowledgement",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "participant_signed_confirmation" => Ok(Self::ParticipantSignedConfirmation),
            "timestamped_two_channel_acknowledgement" => {
                Ok(Self::TimestampedTwoChannelAcknowledgement)
            }
            _ => Err(invalid("stored_verification_method")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedArtifactPolicy {
    DeleteDerived,
    RedactDerived,
    RetainMinimalAudit,
}

impl DerivedArtifactPolicy {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DeleteDerived => "delete_derived",
            Self::RedactDerived => "redact_derived",
            Self::RetainMinimalAudit => "retain_minimal_audit",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "delete_derived" => Ok(Self::DeleteDerived),
            "redact_derived" => Ok(Self::RedactDerived),
            "retain_minimal_audit" => Ok(Self::RetainMinimalAudit),
            _ => Err(invalid("stored_artifact_policy")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentRevocationCode {
    ParticipantRevoked,
    ReviewerRevoked,
    SourceChanged,
    CampaignDisabled,
    PrivacyRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentGrantState {
    Active,
    Expired,
    Revoked,
}

impl ConsentGrantState {
    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "active" => Ok(Self::Active),
            "expired" => Ok(Self::Expired),
            "revoked" => Ok(Self::Revoked),
            _ => Err(invalid("stored_consent_state")),
        }
    }
}

impl ConsentRevocationCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ParticipantRevoked => "participant_revoked",
            Self::ReviewerRevoked => "reviewer_revoked",
            Self::SourceChanged => "source_changed",
            Self::CampaignDisabled => "campaign_disabled",
            Self::PrivacyRequest => "privacy_request",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspirationVetoCode {
    ParticipantVeto,
    SafetyVeto,
    PrivacyVeto,
}

impl InspirationVetoCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ParticipantVeto => "participant_veto",
            Self::SafetyVeto => "safety_veto",
            Self::PrivacyVeto => "privacy_veto",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InspirationVetoScope {
    Campaign,
    Category {
        category_id: OpaqueInspirationId,
    },
    SourceVersion {
        source_id: OpaqueInspirationId,
        source_version: u64,
        source_digest: Sha256Digest,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedWorkKind {
    Text,
    Image,
    Recap,
}

impl DerivedWorkKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Recap => "recap",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableNoSelectionReason {
    DeploymentDisabled,
    GlobalKillSwitch,
    CampaignDisabled,
    CampaignPaused,
    SafetyIncomplete,
    NoEligibleSources,
}

impl DurableNoSelectionReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DeploymentDisabled => "deployment_disabled",
            Self::GlobalKillSwitch => "global_kill_switch",
            Self::CampaignDisabled => "campaign_disabled",
            Self::CampaignPaused => "campaign_paused",
            Self::SafetyIncomplete => "safety_incomplete",
            Self::NoEligibleSources => "no_eligible_sources",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "deployment_disabled" => Ok(Self::DeploymentDisabled),
            "global_kill_switch" => Ok(Self::GlobalKillSwitch),
            "campaign_disabled" => Ok(Self::CampaignDisabled),
            "campaign_paused" => Ok(Self::CampaignPaused),
            "safety_incomplete" => Ok(Self::SafetyIncomplete),
            "no_eligible_sources" => Ok(Self::NoEligibleSources),
            _ => Err(invalid("stored_no_selection_reason")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafetySetupEvidence {
    pub evidence_digest: Sha256Digest,
    pub reviewer_id: OpaqueOperatorId,
    pub tone: CampaignInspirationTone,
    pub allowed_sensitivity_codes: BTreeSet<OpaqueInspirationId>,
    pub line_codes: BTreeSet<OpaqueInspirationId>,
    pub veil_codes: BTreeSet<OpaqueInspirationId>,
    pub excluded_topic_codes: BTreeSet<OpaqueInspirationId>,
    pub excluded_participant_ids: BTreeSet<OpaqueInspirationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignInspirationTone {
    GothicAdventure,
    HopefulAdventure,
    LightheartedAdventure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictedDiagnosticAccessKind {
    SourcePlaintext,
    SourceBackup,
    ImageQuarantine,
    GenerationDiagnostic,
}

impl RestrictedDiagnosticAccessKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SourcePlaintext => "source_plaintext",
            Self::SourceBackup => "source_backup",
            Self::ImageQuarantine => "image_quarantine",
            Self::GenerationDiagnostic => "generation_diagnostic",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "source_plaintext" => Ok(Self::SourcePlaintext),
            "source_backup" => Ok(Self::SourceBackup),
            "image_quarantine" => Ok(Self::ImageQuarantine),
            "generation_diagnostic" => Ok(Self::GenerationDiagnostic),
            _ => Err(invalid("stored_restricted_access_kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictedDiagnosticPurpose {
    SourceReview,
    DataRightsRequest,
    IncidentResponse,
    RestoreDrill,
    SecurityValidation,
}

impl RestrictedDiagnosticPurpose {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SourceReview => "source_review",
            Self::DataRightsRequest => "data_rights_request",
            Self::IncidentResponse => "incident_response",
            Self::RestoreDrill => "restore_drill",
            Self::SecurityValidation => "security_validation",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "source_review" => Ok(Self::SourceReview),
            "data_rights_request" => Ok(Self::DataRightsRequest),
            "incident_response" => Ok(Self::IncidentResponse),
            "restore_drill" => Ok(Self::RestoreDrill),
            "security_validation" => Ok(Self::SecurityValidation),
            _ => Err(invalid("stored_restricted_access_purpose")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictedDiagnosticDecision {
    Allowed,
    Denied,
}

impl RestrictedDiagnosticDecision {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "allowed" => Ok(Self::Allowed),
            "denied" => Ok(Self::Denied),
            _ => Err(invalid("stored_restricted_access_decision")),
        }
    }
}

impl CampaignInspirationTone {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GothicAdventure => "gothic_adventure",
            Self::HopefulAdventure => "hopeful_adventure",
            Self::LightheartedAdventure => "lighthearted_adventure",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Result<Self, PrivateInspirationError> {
        match value {
            "gothic_adventure" => Ok(Self::GothicAdventure),
            "hopeful_adventure" => Ok(Self::HopefulAdventure),
            "lighthearted_adventure" => Ok(Self::LightheartedAdventure),
            _ => Err(invalid("stored_campaign_tone")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureCampaignInspirationCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub expected_revision: u64,
    pub enabled: bool,
    pub safety_setup: Option<SafetySetupEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyParticipantCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub participant_id: OpaqueInspirationId,
    pub method: ParticipantVerificationMethod,
    pub evidence_digest: Sha256Digest,
    pub verifier_id: OpaqueOperatorId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterSourceVersionCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub source_id: OpaqueInspirationId,
    pub source_version: u64,
    pub source_digest: Sha256Digest,
    pub category_id: OpaqueInspirationId,
    pub owner_participant_id: OpaqueInspirationId,
    pub participant_ids: BTreeSet<OpaqueInspirationId>,
    pub sensitivity_codes: BTreeSet<OpaqueInspirationId>,
    pub eligible_media: BTreeSet<InspirationMedia>,
    pub eligible_theme_pack_ids: BTreeSet<OpaqueInspirationId>,
    pub provenance_digest: Sha256Digest,
    pub expires_at_epoch: Option<u64>,
    pub runtime_prompt: RuntimeEventPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSourceVersionCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub source_id: OpaqueInspirationId,
    pub source_version: u64,
    pub source_digest: Sha256Digest,
    pub decision: SourceReviewState,
    pub q11_screened: bool,
    pub reviewer_id: OpaqueOperatorId,
    pub review_evidence_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantConsentCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub source_id: OpaqueInspirationId,
    pub source_version: u64,
    pub source_digest: Sha256Digest,
    pub participant_id: OpaqueInspirationId,
    pub audience: InspirationAudience,
    pub media: InspirationMedia,
    pub transformation: InspirationTransformation,
    pub sensitivity_codes: BTreeSet<OpaqueInspirationId>,
    pub expires_at_epoch: u64,
    pub reviewer_id: OpaqueOperatorId,
    pub participant_confirmation_digest: Sha256Digest,
    pub review_evidence_digest: Sha256Digest,
    pub artifact_policy: DerivedArtifactPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeConsentCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub grant_id: OpaqueInspirationId,
    pub requester_participant_id: OpaqueInspirationId,
    pub reason: ConsentRevocationCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRestrictedDiagnosticAccessCommand {
    pub schema_version: u16,
    pub idempotency_key: OpaqueInspirationId,
    pub campaign_session_id: Option<OpaqueInspirationId>,
    pub operator_id: OpaqueOperatorId,
    pub access_kind: RestrictedDiagnosticAccessKind,
    pub purpose: RestrictedDiagnosticPurpose,
    pub subject_id: OpaqueInspirationId,
    pub evidence_digest: Sha256Digest,
    pub decision: RestrictedDiagnosticDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestrictedDiagnosticAccessProjection {
    pub schema_version: u16,
    pub audit_id: OpaqueInspirationId,
    pub campaign_session_id: Option<OpaqueInspirationId>,
    pub operator_id: OpaqueOperatorId,
    pub access_kind: RestrictedDiagnosticAccessKind,
    pub purpose: RestrictedDiagnosticPurpose,
    pub subject_id: OpaqueInspirationId,
    pub evidence_digest: Sha256Digest,
    pub decision: RestrictedDiagnosticDecision,
    pub occurred_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteParticipantPrivateDataCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub participant_id: OpaqueInspirationId,
    pub operator_id: OpaqueOperatorId,
    pub deletion_evidence_digest: Sha256Digest,
    /// The application boundary must prove that no configured protected source
    /// containing this participant remains loaded before setting this flag.
    pub protected_sources_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyInspirationVetoCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub participant_id: OpaqueInspirationId,
    pub scope: InspirationVetoScope,
    pub code: InspirationVetoCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterDerivedWorkCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub work_id: OpaqueInspirationId,
    pub selection_id: OpaqueInspirationId,
    pub kind: DerivedWorkKind,
    pub artifact_policy: DerivedArtifactPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestInspirationSelectionCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub expected_campaign_revision: u64,
    pub expected_settings_revision: u64,
    pub audience: InspirationAudience,
    pub media: InspirationMedia,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetCampaignInspirationPauseCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub expected_revision: u64,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisableCampaignInspirationCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGlobalInspirationControlCommand {
    pub schema_version: u16,
    pub idempotency_key: OpaqueInspirationId,
    pub expected_revision: u64,
    pub generation_disabled: bool,
    pub operator_id: OpaqueOperatorId,
    pub evidence_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeExpiredParticipantDeletionTombstonesCommand {
    pub schema_version: u16,
    pub idempotency_key: OpaqueInspirationId,
    pub delete_after_epoch_inclusive: u64,
    pub operator_id: OpaqueOperatorId,
    pub evidence_digest: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationPrivacyAction {
    Veil,
    VetoSource,
    VetoCategory,
    Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPresentationPrivacyCommand {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub idempotency_key: OpaqueInspirationId,
    pub presentation_id: OpaqueInspirationId,
    pub action: PresentationPrivacyAction,
}

pub(crate) struct ResolvedInspirationSelectionAuthority {
    pub(crate) seed_reference: OpaqueInspirationId,
    pub(crate) seed: [u8; 32],
}

impl fmt::Debug for ResolvedInspirationSelectionAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedInspirationSelectionAuthority")
            .field("seed_reference", &self.seed_reference)
            .field("seed", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInspirationSettingsProjection {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub revision: u64,
    pub enabled: bool,
    pub generation_paused: bool,
    pub safety_setup_complete: bool,
    pub adults_only: bool,
    pub fictional_distance_locked_high: bool,
    pub tone: CampaignInspirationTone,
    pub line_count: u32,
    pub veil_count: u32,
    pub excluded_topic_count: u32,
    pub excluded_participant_count: u32,
    pub audience: InspirationAudience,
    pub media: InspirationMedia,
    pub q11_policy_id: String,
    pub updated_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInspirationStatus {
    pub schema_version: u16,
    pub deployment_enabled: bool,
    pub global_generation_disabled: bool,
    pub global_control_revision: u64,
    pub settings: Option<CampaignInspirationSettingsProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalInspirationControlProjection {
    pub schema_version: u16,
    pub revision: u64,
    pub generation_disabled: bool,
    pub updated_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionTombstonePurgeOutcome {
    pub schema_version: u16,
    pub delete_after_epoch_inclusive: u64,
    pub purged_count: u32,
    pub applied_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceVersionProjection {
    pub schema_version: u16,
    pub source_id: OpaqueInspirationId,
    pub source_version: u64,
    pub source_digest: Sha256Digest,
    pub category_id: OpaqueInspirationId,
    pub review_state: SourceReviewState,
    pub q11_screened: bool,
    pub participant_count: u32,
    pub sensitivity_count: u32,
    pub eligible_media: BTreeSet<InspirationMedia>,
    pub eligible_theme_pack_ids: BTreeSet<OpaqueInspirationId>,
    pub expires_at_epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantVerificationProjection {
    pub schema_version: u16,
    pub participant_id: OpaqueInspirationId,
    pub method: ParticipantVerificationMethod,
    pub verified_at_epoch: u64,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentGrantProjection {
    pub schema_version: u16,
    pub grant_id: OpaqueInspirationId,
    pub source_id: OpaqueInspirationId,
    pub source_version: u64,
    pub source_digest: Sha256Digest,
    pub participant_id: OpaqueInspirationId,
    pub audience: InspirationAudience,
    pub media: InspirationMedia,
    pub transformation: InspirationTransformation,
    pub artifact_policy: DerivedArtifactPolicy,
    pub expires_at_epoch: u64,
    pub state: ConsentGrantState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VetoProjection {
    pub schema_version: u16,
    pub veto_id: OpaqueInspirationId,
    pub campaign_session_id: OpaqueInspirationId,
    pub participant_id: OpaqueInspirationId,
    pub scope: InspirationVetoScope,
    pub code: InspirationVetoCode,
    pub created_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedWorkProjection {
    pub schema_version: u16,
    pub work_id: OpaqueInspirationId,
    pub selection_id: OpaqueInspirationId,
    pub source_id: OpaqueInspirationId,
    pub source_version: u64,
    pub source_digest: Sha256Digest,
    pub kind: DerivedWorkKind,
    pub artifact_policy: DerivedArtifactPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyTransitionOutcome {
    pub schema_version: u16,
    pub subject_id: OpaqueInspirationId,
    pub pending_work_cancellation_ids: Vec<OpaqueInspirationId>,
    pub effective_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantDeletionOutcome {
    pub schema_version: u16,
    pub participant_id: OpaqueInspirationId,
    pub revoked_grant_count: u32,
    pub quarantined_source_count: u32,
    pub pending_work_cancellation_ids: Vec<OpaqueInspirationId>,
    pub affected_completed_artifact_count: u32,
    pub effective_at_epoch: u64,
    pub tombstone_delete_after_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationPrivacyOutcome {
    pub schema_version: u16,
    pub presentation_id: OpaqueInspirationId,
    pub action: PresentationPrivacyAction,
    pub presentation_hidden: bool,
    pub settings_revision: Option<u64>,
    pub effective_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateInspirationSelection {
    pub schema_version: u16,
    pub selection_id: OpaqueInspirationId,
    pub campaign_session_id: OpaqueInspirationId,
    pub source_version: Option<u64>,
    pub durable_no_selection_reason: Option<DurableNoSelectionReason>,
    pub audit: EventSelectionAudit,
    pub created_at_epoch: u64,
}

pub struct ReservedInspirationSelection {
    pub prompt: Option<EventPrompt>,
    pub outcome: PrivateInspirationSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInspirationRedactedExportV1 {
    pub schema_version: u16,
    pub campaign_session_id: OpaqueInspirationId,
    pub requesting_participant_id: OpaqueInspirationId,
    pub settings: CampaignInspirationSettingsProjection,
    pub sources: Vec<SourceVersionProjection>,
    /// Contains only the requesting participant's grants.
    pub requester_grants: Vec<ConsentGrantProjection>,
}

impl CampaignInspirationRedactedExportV1 {
    pub fn canonical_json(&self) -> Result<String, PrivateInspirationError> {
        serde_json::to_string(self).map_err(PrivateInspirationError::Serialization)
    }
}

pub trait TrustedEpochTime: Send + Sync {
    fn now_epoch_seconds(&self) -> u64;
}

impl<F> TrustedEpochTime for F
where
    F: Fn() -> u64 + Send + Sync,
{
    fn now_epoch_seconds(&self) -> u64 {
        self()
    }
}

#[derive(Debug, Clone, Copy)]
struct SystemTrustedEpochTime;

impl TrustedEpochTime for SystemTrustedEpochTime {
    fn now_epoch_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[derive(Clone)]
pub struct PrivateInspirationApplicationService {
    repository: MongoRepository,
    deployment_enabled: bool,
    seed_vault: Arc<SeedVault>,
    clock: Arc<dyn TrustedEpochTime>,
}

impl PrivateInspirationApplicationService {
    pub fn new(
        repository: MongoRepository,
        deployment_enabled: bool,
        seed_vault: Arc<SeedVault>,
    ) -> Self {
        Self {
            repository,
            deployment_enabled,
            seed_vault,
            clock: Arc::new(SystemTrustedEpochTime),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_clock(
        repository: MongoRepository,
        deployment_enabled: bool,
        seed_vault: Arc<SeedVault>,
        clock: impl TrustedEpochTime + 'static,
    ) -> Self {
        Self {
            repository,
            deployment_enabled,
            seed_vault,
            clock: Arc::new(clock),
        }
    }

    pub async fn configure_campaign(
        &self,
        command: ConfigureCampaignInspirationCommand,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.expected_revision == 0 && command.enabled {
            return Err(invalid("new_settings_must_start_disabled"));
        }
        if command.enabled && !self.deployment_enabled {
            return Err(PrivateInspirationError::DeploymentDisabled);
        }
        if command.enabled && command.safety_setup.is_none() {
            return Err(invalid("enabled_campaign_requires_safety_setup"));
        }
        if let Some(setup) = &command.safety_setup {
            for participant_id in &setup.excluded_participant_ids {
                validate_participant_id(participant_id)?;
            }
            let set_lengths = [
                setup.allowed_sensitivity_codes.len(),
                setup.line_codes.len(),
                setup.veil_codes.len(),
                setup.excluded_topic_codes.len(),
                setup.excluded_participant_ids.len(),
            ];
            if set_lengths
                .into_iter()
                .any(|length| length > MAX_CAMPAIGN_SAFETY_CODES)
                || !setup.line_codes.is_disjoint(&setup.veil_codes)
                || !setup.line_codes.is_disjoint(&setup.excluded_topic_codes)
                || !setup.veil_codes.is_disjoint(&setup.excluded_topic_codes)
            {
                return Err(invalid("invalid_campaign_safety_scope"));
            }
        }
        let fingerprint = fingerprint(&command)?;
        self.repository
            .configure_private_inspiration_campaign(
                &command,
                &fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn campaign_status(
        &self,
        campaign_session_id: &OpaqueInspirationId,
    ) -> Result<CampaignInspirationStatus, PrivateInspirationError> {
        let settings = self
            .repository
            .load_private_inspiration_campaign_settings(campaign_session_id)
            .await?;
        let global = self.repository.load_global_inspiration_control().await?;
        Ok(CampaignInspirationStatus {
            schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
            deployment_enabled: self.deployment_enabled,
            global_generation_disabled: global.generation_disabled,
            global_control_revision: global.revision,
            settings,
        })
    }

    pub async fn record_restricted_diagnostic_access(
        &self,
        command: RecordRestrictedDiagnosticAccessCommand,
    ) -> Result<RestrictedDiagnosticAccessProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        let request_fingerprint = fingerprint(&command)?;
        self.repository
            .record_private_inspiration_restricted_access(
                &command,
                &request_fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn set_global_control(
        &self,
        command: SetGlobalInspirationControlCommand,
    ) -> Result<GlobalInspirationControlProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.expected_revision == 0
            || (!command.generation_disabled && !self.deployment_enabled)
        {
            return Err(if self.deployment_enabled {
                invalid("invalid_global_control_revision")
            } else {
                PrivateInspirationError::DeploymentDisabled
            });
        }
        let request_fingerprint = fingerprint(&command)?;
        self.repository
            .set_global_inspiration_control(
                &command,
                &request_fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn purge_expired_deletion_tombstones(
        &self,
        command: PurgeExpiredParticipantDeletionTombstonesCommand,
    ) -> Result<DeletionTombstonePurgeOutcome, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        let now = self.clock.now_epoch_seconds();
        if command.delete_after_epoch_inclusive > now {
            return Err(invalid("deletion_tombstone_cutoff_is_future"));
        }
        let request_fingerprint = fingerprint(&command)?;
        self.repository
            .purge_expired_private_inspiration_deletion_tombstones(
                &command,
                &request_fingerprint,
                now,
            )
            .await
    }

    pub async fn set_campaign_pause(
        &self,
        command: SetCampaignInspirationPauseCommand,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.expected_revision == 0 {
            return Err(invalid("invalid_settings_revision"));
        }
        if !command.paused && !self.deployment_enabled {
            return Err(PrivateInspirationError::DeploymentDisabled);
        }
        let request_fingerprint = fingerprint(&command)?;
        self.repository
            .set_private_inspiration_campaign_pause(
                &command,
                &request_fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn disable_campaign(
        &self,
        command: DisableCampaignInspirationCommand,
    ) -> Result<CampaignInspirationSettingsProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.expected_revision == 0 {
            return Err(invalid("invalid_settings_revision"));
        }
        let request_fingerprint = fingerprint(&command)?;
        self.repository
            .disable_private_inspiration_campaign(
                &command,
                &request_fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn apply_presentation_privacy_control(
        &self,
        command: ApplyPresentationPrivacyCommand,
    ) -> Result<PresentationPrivacyOutcome, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        let request_fingerprint = fingerprint(&command)?;
        self.repository
            .apply_private_inspiration_presentation_control(
                &command,
                &request_fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn verify_participant(
        &self,
        command: VerifyParticipantCommand,
    ) -> Result<ParticipantVerificationProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        validate_participant_id(&command.participant_id)?;
        if command.participant_id.as_str() == command.verifier_id.as_str() {
            return Err(invalid("participant_and_verifier_must_be_distinct"));
        }
        let fingerprint = fingerprint(&command)?;
        self.repository
            .verify_private_inspiration_participant(
                &command,
                &fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn register_source_version(
        &self,
        command: RegisterSourceVersionCommand,
    ) -> Result<SourceVersionProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.source_version == 0
            || command.source_id.as_str() != privacy_source_id(&command.source_digest)
            || command.participant_ids.is_empty()
            || !command
                .participant_ids
                .contains(&command.owner_participant_id)
            || command.sensitivity_codes.is_empty()
            || command.eligible_media.is_empty()
            || command.eligible_theme_pack_ids.is_empty()
            || command.eligible_theme_pack_ids.len() > MAX_SOURCE_THEME_IDS
            || command.runtime_prompt.validate().is_err()
        {
            return Err(invalid("invalid_source_registration_scope"));
        }
        for participant_id in &command.participant_ids {
            validate_participant_id(participant_id)?;
        }
        let now = self.clock.now_epoch_seconds();
        if command.expires_at_epoch.is_some_and(|expiry| expiry <= now) {
            return Err(invalid("source_expiry_not_future"));
        }
        let fingerprint = fingerprint(&command)?;
        self.repository
            .register_private_inspiration_source(&command, &fingerprint, now)
            .await
    }

    pub async fn review_source_version(
        &self,
        command: ReviewSourceVersionCommand,
    ) -> Result<SourceVersionProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.source_id.as_str() != privacy_source_id(&command.source_digest)
            || command.decision == SourceReviewState::Pending
            || (command.decision == SourceReviewState::Approved && !command.q11_screened)
        {
            return Err(invalid("invalid_source_review_decision"));
        }
        let fingerprint = fingerprint(&command)?;
        self.repository
            .review_private_inspiration_source(
                &command,
                &fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn grant_consent(
        &self,
        command: GrantConsentCommand,
    ) -> Result<ConsentGrantProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        let now = self.clock.now_epoch_seconds();
        if command.source_version == 0
            || command.source_id.as_str() != privacy_source_id(&command.source_digest)
            || command.sensitivity_codes.is_empty()
            || command.expires_at_epoch <= now
        {
            return Err(invalid("invalid_consent_scope"));
        }
        validate_participant_id(&command.participant_id)?;
        let fingerprint = fingerprint(&command)?;
        self.repository
            .grant_private_inspiration_consent(&command, &fingerprint, now)
            .await
    }

    pub async fn revoke_consent(
        &self,
        command: RevokeConsentCommand,
    ) -> Result<PrivacyTransitionOutcome, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        validate_participant_id(&command.requester_participant_id)?;
        let fingerprint = fingerprint(&command)?;
        self.repository
            .revoke_private_inspiration_consent(
                &command,
                &fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn delete_participant_private_data(
        &self,
        command: DeleteParticipantPrivateDataCommand,
    ) -> Result<ParticipantDeletionOutcome, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        validate_participant_id(&command.participant_id)?;
        if !command.protected_sources_removed {
            return Err(invalid("protected_sources_must_be_removed_first"));
        }
        let fingerprint = fingerprint(&command)?;
        self.repository
            .delete_private_inspiration_participant_data(
                &command,
                &fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn apply_veto(
        &self,
        command: ApplyInspirationVetoCommand,
    ) -> Result<(VetoProjection, PrivacyTransitionOutcome), PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        validate_participant_id(&command.participant_id)?;
        if let InspirationVetoScope::SourceVersion {
            source_id,
            source_version,
            source_digest,
        } = &command.scope
            && (*source_version == 0 || source_id.as_str() != privacy_source_id(source_digest))
        {
            return Err(invalid("invalid_veto_source_scope"));
        }
        let fingerprint = fingerprint(&command)?;
        self.repository
            .apply_private_inspiration_veto(&command, &fingerprint, self.clock.now_epoch_seconds())
            .await
    }

    pub async fn register_derived_work(
        &self,
        command: RegisterDerivedWorkCommand,
    ) -> Result<DerivedWorkProjection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        let fingerprint = fingerprint(&command)?;
        self.repository
            .register_private_inspiration_derived_work(
                &command,
                &fingerprint,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    /// Closes a body-free reservation that could not produce a durable
    /// presentation. Cancellation is idempotent and cannot remove a work item
    /// that already completed.
    pub async fn abandon_pending_derived_work(
        &self,
        campaign_session_id: &OpaqueInspirationId,
        work_id: &OpaqueInspirationId,
    ) -> Result<(), PrivateInspirationError> {
        self.repository
            .abandon_private_inspiration_derived_work(
                campaign_session_id,
                work_id,
                self.clock.now_epoch_seconds(),
            )
            .await
    }

    pub async fn request_selection(
        &self,
        command: RequestInspirationSelectionCommand,
    ) -> Result<ReservedInspirationSelection, PrivateInspirationError> {
        validate_schema(command.schema_version)?;
        if command.expected_settings_revision == 0 || command.expected_campaign_revision == 0 {
            return Err(invalid("invalid_selection_scope"));
        }
        let campaign_seed = self
            .seed_vault
            .derive_campaign_seed(command.campaign_session_id.as_str())
            .map_err(|_| invalid("campaign_seed_unavailable"))?;
        let mut seed_hasher = Sha256::new();
        seed_hasher.update(b"manchester-arcana/private-inspiration-rng/v1");
        seed_hasher.update(campaign_seed.expose_to_engine());
        let authority = ResolvedInspirationSelectionAuthority {
            seed_reference: OpaqueInspirationId::new(format!(
                "{}:private-inspiration-v1",
                campaign_seed.reference()
            ))?,
            seed: seed_hasher.finalize().into(),
        };
        let prompts = self
            .repository
            .load_private_inspiration_runtime_prompts()
            .await?;
        let outcome = self
            .repository
            .reserve_private_inspiration_selection(
                self.deployment_enabled,
                &command,
                &authority,
                &prompts,
                self.clock.now_epoch_seconds(),
            )
            .await?;
        let prompt = match (
            outcome.audit.selected_source_id.as_deref(),
            outcome.audit.selected_source_digest.as_ref(),
        ) {
            (Some(source_id), Some(source_digest)) => Some(
                prompts
                    .into_iter()
                    .find(|prompt| {
                        prompt.privacy_source_id() == source_id
                            && prompt.source_digest() == source_digest
                    })
                    .ok_or_else(|| invalid("selected_prompt_not_in_request"))?,
            ),
            (None, None) => None,
            _ => return Err(invalid("stored_selection_shape")),
        };
        Ok(ReservedInspirationSelection { prompt, outcome })
    }

    pub async fn redacted_export(
        &self,
        campaign_session_id: &OpaqueInspirationId,
        requesting_participant_id: &OpaqueInspirationId,
    ) -> Result<CampaignInspirationRedactedExportV1, PrivateInspirationError> {
        self.repository
            .load_private_inspiration_redacted_export(
                campaign_session_id,
                requesting_participant_id,
            )
            .await
    }
}

fn validate_schema(schema_version: u16) -> Result<(), PrivateInspirationError> {
    if schema_version != PRIVATE_INSPIRATION_SCHEMA_VERSION {
        return Err(invalid("unsupported_schema_version"));
    }
    Ok(())
}

fn validate_participant_id(
    participant_id: &OpaqueInspirationId,
) -> Result<(), PrivateInspirationError> {
    if !valid_namespaced_hex_id(participant_id.as_str(), "participant:") {
        return Err(invalid("participant_namespace_required"));
    }
    Ok(())
}

fn valid_namespaced_hex_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(crate) fn fingerprint(value: &impl Serialize) -> Result<Sha256Digest, PrivateInspirationError> {
    let bytes = serde_json::to_vec(value).map_err(PrivateInspirationError::Serialization)?;
    Ok(Sha256Digest::from_bytes(Sha256::digest(bytes).into()))
}

pub(crate) fn internal_id(prefix: &str) -> Result<OpaqueInspirationId, PrivateInspirationError> {
    OpaqueInspirationId::new(format!("{prefix}:{}", Uuid::new_v4().simple()))
}

#[allow(dead_code)]
pub(crate) fn to_i64(value: u64) -> Result<i64, PrivateInspirationError> {
    i64::try_from(value).map_err(|_| invalid("numeric_range"))
}

#[allow(dead_code)]
pub(crate) fn to_u64(value: i64) -> Result<u64, PrivateInspirationError> {
    u64::try_from(value).map_err(|_| invalid("stored_numeric_range"))
}

pub(crate) const fn invalid(code: &'static str) -> PrivateInspirationError {
    PrivateInspirationError::InvalidCommand { code }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_ids_and_versioned_commands_reject_forged_textual_scope() {
        assert!(OpaqueInspirationId::new("source:opaque-1").is_ok());
        for invalid_id in ["../private", "person@example.invalid", "contains space", ""] {
            assert!(OpaqueInspirationId::new(invalid_id).is_err());
        }
        assert!(serde_json::from_str::<OpaqueInspirationId>("\"../private\"").is_err());
    }
}
